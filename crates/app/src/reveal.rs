// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "As built" reveal from a real diff: the AI plan (`ModelDiff`), version comparison
//! (`VersionDiff`) and merge preview (`MergePlan`) become steps, steps become batches in
//! build order (`ddc_model::reveal_build_order`), and batches become the render timeline
//! (`ddc_render::reveal::Timeline`). No meshes are built here: scene element numbers are supplied by
//! the caller (`resolve`), and the scene is assembled by a single builder, `view3d::scene`.

use std::collections::BTreeMap;

use egui;

use ddc_model::merge::{MergeCase, MergePlan};
use ddc_model::{
    reveal_build_order, shift_of, Date, DiffKind, ElKey, Model, PhaseIndex, WalkStep, ModelDiff, RevealChange as Change, RevealStep as Step,
    RowKind, VersionDiff, PLAN_IGNORED,
};
use ddc_render::glam::Vec3;
use ddc_render::reveal::{Ease, Effect, ElemAnim, ElemTint, Timeline, Track};

/// Frames per batch and the start offset of the next one (batches overlap slightly).
pub const BATCH_FRAMES: u32 = 30;
pub const BATCH_STEP: u32 = 18;
pub const FPS: u32 = 30;
/// Rise from below the spot and drop, m.
const RISE: f32 = 1.5;
/// Recolor colors: added, removed, changed (sRGB).
const TINT_ADDED: [u8; 3] = [64, 190, 96];
const TINT_REMOVED: [u8; 3] = [214, 72, 64];
const TINT_MODIFIED: [u8; 3] = [236, 160, 48];

fn put(map: &mut BTreeMap<ElKey, Step>, key: ElKey, change: Change, from: Option<[f64; 3]>) {
    map.insert(key.clone(), Step { key, change, from });
}

/// Steps of the AI plan: cache-table rows and edit side effects (`secondary`) are skipped; an element's
/// own row defines an add, remove or change, and a part's row means a change of its host.
pub fn from_plan(d: &ModelDiff) -> Vec<Step> {
    let rows = d
        .rows
        .iter()
        .filter(|r| !r.secondary() && !PLAN_IGNORED.contains(&r.table.as_str()));
    let mut out: BTreeMap<ElKey, Step> = BTreeMap::new();
    let mut parts = vec![];
    for r in rows {
        let Some(owner) = r.owner.clone() else { continue };
        if owner.0 != r.table || owner.1 != r.rowid {
            parts.push(owner);
            continue;
        }
        match r.kind {
            RowKind::Created => put(&mut out, owner, Change::Added, None),
            RowKind::Deleted => put(&mut out, owner, Change::Removed, None),
            RowKind::Changed => {
                let cells: Vec<_> = r
                    .cells
                    .iter()
                    .map(|c| (c.col.as_str(), c.before.as_deref(), c.after.as_deref()))
                    .collect();
                put(&mut out, owner, Change::Modified, shift_of(&cells));
            }
        }
    }
    for owner in parts {
        match out.get_mut(&owner) {
            // a wholly added or removed element is already shown; a move with another edit is not a move
            Some(s) if s.change == Change::Modified => s.from = None,
            Some(_) => {}
            None => put(&mut out, owner, Change::Modified, None),
        }
    }
    out.into_values().collect()
}

/// Steps of an AI plan card from its grouped diff: new things appear, demolished ones disappear,
/// changed ones are highlighted (a move, if only the position changed).
pub fn from_elem_diffs(d: &[crate::ai::plan::ElemDiff]) -> Vec<Step> {
    d.iter()
        .map(|e| match e.kind {
            RowKind::Created => Step::new(e.key.clone(), Change::Added),
            RowKind::Deleted => Step::new(e.key.clone(), Change::Removed),
            RowKind::Changed => {
                let cells: Vec<_> = e
                    .cells
                    .iter()
                    .map(|c| (c.col.as_str(), c.before.as_deref(), c.after.as_deref()))
                    .collect();
                Step { key: e.key.clone(), change: Change::Modified, from: shift_of(&cells) }
            }
        })
        .collect()
}

/// Request to show steps: the plan card and merge window buttons do not hold `EditorApp`, so they
/// put the steps here and `controls` picks them up on the frame. `preview` is what was added but is not yet in the model;
/// it is drawn as one plan-preview mesh and appears all at once.
pub fn request(steps: Vec<Step>, preview: bool) {
    REQUEST.with(|r| *r.borrow_mut() = Some((steps, preview)));
}

thread_local! {
    static REQUEST: std::cell::RefCell<Option<(Vec<Step>, bool)>> = const { std::cell::RefCell::new(None) };
}

fn start_requested(a: &mut crate::shell::EditorApp, steps: Vec<Step>, preview: bool) -> bool {
    let refs = scene_refs(a);
    let prev = crate::scene_ids::AI_PREVIEW;
    let in_scene = |k: &ElKey| refs.get(k).and_then(|s| a.ed.element_range(*s));
    let b = batches(&steps, &a.ed.model);
    let map = pack_map(&a.scene);
    let mut tl = timeline(&b, &|k| {
        let s = steps.iter().find(|s| &s.key == k);
        match s {
            Some(s) if preview && s.change == Change::Added => Some((prev, prev + 1)),
            _ => in_scene(k).and_then(|r| packed_range(&map, r)),
        }
    });
    tl = apply_motion(tl, a, a.reveal_motion);
    let p = Player::new(tl);
    if p.is_empty() {
        return false;
    }
    set_pack(a, Some(map));
    a.reveal = Some(p);
    enter_3d(a);
    true
}

/// Switches the numbering of the shown scene (packed while a show plays) and rebuilds it.
fn set_pack(a: &mut crate::shell::EditorApp, map: Option<BTreeMap<u32, u32>>) {
    a.reveal_pack = map;
    a.scene_rev = a.ed.revision.wrapping_add(1);
}

/// A show plays in the full 3D view: no plan, section or sheet beside it.
fn enter_3d(a: &mut crate::shell::EditorApp) {
    crate::sheet_view::deactivate();
    crate::drafting_view::deactivate();
    a.view3d = true;
    a.section = None;
}

/// Version comparison steps: only here means added, only there means removed, a difference means
/// a change (a move, if only the position changed).
pub fn from_version(d: &VersionDiff) -> Vec<Step> {
    d.elements
        .iter()
        .map(|e| {
            let key = (e.table.clone(), e.id);
            match e.kind {
                DiffKind::HereOnly => Step::new(key, Change::Added),
                DiffKind::ThereOnly => Step::new(key, Change::Removed),
                DiffKind::Differs => {
                    let cells: Vec<_> = e
                        .changes
                        .iter()
                        .map(|c| (c.field.as_str(), c.old.as_deref(), c.new.as_deref()))
                        .collect();
                    Step { key, change: Change::Modified, from: shift_of(&cells) }
                }
            }
        })
        .collect()
}

/// Merge preview steps from the conflict list: edited on both sides is a change, deleted
/// by the colleague is a removal, deleted here but touched there is a restore, added by both is an addition.
/// The preview keeps clean (conflict-free) rows only as counters, not item by item.
pub fn from_merge(p: &MergePlan) -> Vec<Step> {
    p.conflicts
        .iter()
        .map(|c| {
            let change = match c.case {
                MergeCase::EditEdit => Change::Modified,
                MergeCase::DeletedThere => Change::Removed,
                MergeCase::DeletedHere | MergeCase::AddAdd => Change::Added,
            };
            Step::new((c.table.clone(), c.id), change)
        })
        .collect()
}

/// Build batches for the steps.
pub fn batches(steps: &[Step], m: &Model) -> Vec<Vec<Step>> {
    reveal_build_order(steps, m)
}

fn tracks_for(change: Change, from: Option<[f64; 3]>, run: (u32, u32), t0: u32) -> Vec<Track> {
    let t1 = t0 + BATCH_FRAMES;
    let tr = |effect, ease| Track { elements: run, t0, t1, effect, ease };
    match change {
        Change::Added => vec![
            tr(Effect::Appear { rise: RISE }, Ease::OutCubic),
            tr(Effect::Tint { rgb: TINT_ADDED }, Ease::Linear),
        ],
        Change::Removed => vec![
            tr(Effect::Vanish { sink: RISE }, Ease::Linear),
            tr(Effect::Tint { rgb: TINT_REMOVED }, Ease::Linear),
        ],
        Change::Modified => {
            let mut v = vec![tr(Effect::Tint { rgb: TINT_MODIFIED }, Ease::Linear)];
            if let Some(f) = from {
                let from = Vec3::new(f[0] as f32, f[1] as f32, f[2] as f32);
                v.push(tr(Effect::Move { from }, Ease::OutCubic));
            }
            v
        }
    }
}

/// Reveal timeline: batch `i` starts at frame `i * BATCH_STEP`; adjacent element ranges
/// with the same action form one track. Elements with no place in the scene (`resolve` returns `None`)
/// are skipped; `resolve` gives a [start, end) range of scene numbers.
pub fn timeline(batches: &[Vec<Step>], resolve: &dyn Fn(&ElKey) -> Option<(u32, u32)>) -> Timeline {
    let mut tracks = vec![];
    for (i, b) in batches.iter().enumerate() {
        let t0 = i as u32 * BATCH_STEP;
        let mut runs: Vec<((u32, u32), Change, Option<[f64; 3]>)> = b
            .iter()
            .filter_map(|s| Some((resolve(&s.key).filter(|r| r.0 < r.1)?, s.change, s.from)))
            .collect();
        runs.sort_by_key(|x| x.0);
        runs.dedup_by_key(|x| x.0);
        let mut k = 0;
        while k < runs.len() {
            let ((a, mut end), change, from) = runs[k];
            k += 1;
            while k < runs.len() && runs[k].0 .0 == end && runs[k].1 == change && runs[k].2 == from {
                end = runs[k].0 .1;
                k += 1;
            }
            tracks.extend(tracks_for(change, from, (a, end), t0));
        }
    }
    Timeline::new(FPS, tracks)
}

/// Reveal frame for the renderer: the state and recolor of all timeline elements.
#[derive(Debug, Default, PartialEq)]
pub struct Frame {
    pub anim: Vec<ElemAnim>,
    pub tint: Vec<ElemTint>,
}

/// Player: timeline frame, direction, pause. Time follows the caller's clock.
pub struct Player {
    timeline: std::sync::Arc<Timeline>,
    pos: f32,
    /// 1 forward, -1 backward, 0 paused.
    dir: i8,
    last: Option<web_time::Instant>,
    rev: u64,
    shown: Option<u32>,
    cache: Option<(u32, std::sync::Arc<Frame>)>,
    /// Label of each batch (date or journal operation): for the slider and export.
    labels: Vec<String>,
}

impl Player {
    /// Starts at the first frame and immediately plays forward.
    pub fn new(timeline: Timeline) -> Self {
        Self { timeline: std::sync::Arc::new(timeline), pos: 0.0, dir: 1, last: None, rev: 1, shown: None, cache: None, labels: vec![] }
    }

    /// Timeline and frame for the camera following the batch (`ddc_render::reveal_export::follow`);
    /// when paused, the camera is free.
    pub fn follow_data(&self) -> Option<(std::sync::Arc<Timeline>, u32)> {
        self.playing().then(|| (self.timeline.clone(), self.frame()))
    }

    pub fn with_labels(mut self, labels: Vec<String>) -> Self {
        self.labels = labels;
        self
    }

    /// Label of the batch playing on the current frame.
    pub fn caption(&self) -> Option<&str> {
        let i = (self.frame() / BATCH_STEP) as usize;
        self.labels.get(i.min(self.labels.len().checked_sub(1)?)).map(String::as_str)
    }

    pub fn len(&self) -> u32 {
        self.timeline.len
    }

    pub fn is_empty(&self) -> bool {
        self.timeline.tracks.is_empty()
    }

    pub fn frame(&self) -> u32 {
        self.pos.round() as u32
    }

    pub fn playing(&self) -> bool {
        self.dir != 0
    }

    pub fn backwards(&self) -> bool {
        self.dir < 0
    }

    pub fn pause(&mut self) {
        self.dir = 0;
        self.last = None;
    }

    /// Play forward (from the end, restart from the beginning).
    pub fn play(&mut self) {
        if self.pos >= self.timeline.len as f32 {
            self.pos = 0.0;
        }
        self.dir = 1;
        self.last = None;
    }

    /// Play backward (from the start, begin at the end).
    pub fn play_back(&mut self) {
        if self.pos <= 0.0 {
            self.pos = self.timeline.len as f32;
        }
        self.dir = -1;
        self.last = None;
    }

    /// Slider: the frame is set by hand and playback stops.
    pub fn seek(&mut self, frame: u32) {
        self.pos = frame.min(self.timeline.len) as f32;
        self.pause();
    }

    /// `dt` seconds passed; at the timeline edge it pauses. `true` means the frame changed.
    pub fn advance(&mut self, dt: f32) -> bool {
        if self.dir == 0 || self.timeline.len == 0 {
            return false;
        }
        let before = self.frame();
        let step = dt.max(0.0) * self.timeline.fps as f32 * f32::from(self.dir);
        self.pos = (self.pos + step).clamp(0.0, self.timeline.len as f32);
        if self.pos <= 0.0 || self.pos >= self.timeline.len as f32 {
            self.dir = 0;
        }
        self.frame() != before
    }

    /// `advance` by clock: the first call after start only records the time.
    pub fn tick(&mut self, now: web_time::Instant) -> bool {
        if self.dir == 0 {
            return false;
        }
        let dt = self.last.map_or(0.0, |t| now.duration_since(t).as_secs_f32());
        self.last = Some(now);
        self.advance(dt.min(0.25))
    }

    /// Frame change counter and the frame itself; without a change, the same `Arc` and nothing is recomputed.
    pub fn current(&mut self) -> (u64, std::sync::Arc<Frame>) {
        let f = self.frame();
        if self.shown != Some(f) {
            self.shown = Some(f);
            self.rev += 1;
        }
        if self.cache.as_ref().is_none_or(|c| c.0 != f) {
            let fr = Frame { anim: self.timeline.sample(f), tint: self.timeline.sample_tint(f) };
            self.cache = Some((f, std::sync::Arc::new(fr)));
        }
        (self.rev, self.cache.as_ref().expect("cache just filled").1.clone())
    }
}

/// Version comparison reveal: open-model elements present in the scene appear, disappear
/// and move in build order; elements deleted in the other version are not drawn in the scene.
pub fn start_version(a: &mut crate::shell::EditorApp, diff: &VersionDiff) -> bool {
    let steps = from_version(diff);
    let b = batches(&steps, &a.ed.model);
    start(a, &b, vec![])
}

/// Scene elements: model key -> selection by which the editor knows the mesh range.
fn scene_refs(a: &crate::shell::EditorApp) -> std::collections::HashMap<ElKey, crate::editor::Sel> {
    crate::selection::all(&a.ed)
        .into_iter()
        .filter_map(|s| crate::phase_ui::sel_ref(s).map(|(t, i)| ((t.to_string(), i), s)))
        .collect()
}

fn start(a: &mut crate::shell::EditorApp, b: &[Vec<Step>], labels: Vec<String>) -> bool {
    let refs = scene_refs(a);
    let map = pack_map(&a.scene);
    let mut tl = timeline(b, &|k| refs.get(k).and_then(|s| a.ed.element_range(*s)).and_then(|r| packed_range(&map, r)));
    tl = apply_motion(tl, a, a.reveal_motion);
    let p = Player::new(tl).with_labels(labels);
    if p.is_empty() {
        return false;
    }
    set_pack(a, Some(map));
    a.reveal = Some(p);
    enter_3d(a);
    true
}

/// "Show by calendar": stages with dates (or equal intervals) as a construction sequence.
pub fn start_calendar(a: &mut crate::shell::EditorApp) -> bool {
    let Ok(idx) = a.ed.model.phase_index() else { return false };
    let m = &a.ed.model;
    let v = from_phases(&idx, &|p| m.phase_dates(p).ok().flatten());
    let (b, labels) = phase_batches(&v);
    start(a, &b, labels)
}

/// Background journal walk: cancelled via a flag.
pub struct JournalJob {
    rx: std::sync::mpsc::Receiver<Result<Vec<WalkStep>, String>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// At most this many journal timeline steps: a long journal is sampled with a stride.
const MAX_STEPS: usize = 400;

/// "Journal -> Play": a copy of the project file is walked in a thread; `controls` picks up the result.
pub fn start_journal(a: &mut crate::shell::EditorApp) -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    let Some(src) = a.ed.project.work_path().map(std::path::Path::to_path_buf) else { return false };
    let stride = (a.ed.model.journal_len().unwrap_or(0) / MAX_STEPS).max(1);
    let scratch = std::env::temp_dir().join(format!("ddc_journal_walk_{}.ddc", std::process::id()));
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let tx_panic = tx.clone();
    let _ = crate::jobs::spawn_guarded(
        "ddc-journal-walk",
        move || {
            let r = Model::journal_walk(&src, &scratch, stride, &mut |_, _| !flag.load(Ordering::Relaxed))
                .and_then(|w| w.with_cancel(flag.clone()).collect::<Result<Vec<_>, _>>())
                .map_err(|e| e.to_string());
            let _ = tx.send(r);
        },
        move |msg| {
            let _ = tx_panic.send(Err(msg));
        },
    );
    a.reveal_job = Some(JournalJob { rx, cancel });
    true
}

/// Picks up the finished journal walk and starts the reveal.
fn poll_journal(a: &mut crate::shell::EditorApp) {
    let Some(job) = a.reveal_job.as_ref() else { return };
    let Ok(r) = job.rx.try_recv() else { return };
    a.reveal_job = None;
    if let Ok(walk) = r {
        let (b, labels) = from_journal(&walk);
        start(a, &b, labels);
    }
}

/// How much the show moves: the interface setting gives `Reduced`, `--reveal-motion off` gives
/// `Off`: changes appear at once, the slider stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Full,
    Reduced,
    Off,
}

/// The explicit choice (`--reveal-motion`) beats the interface setting.
pub fn motion_of(explicit: Option<Motion>, reduced_setting: bool) -> Motion {
    explicit.unwrap_or(if reduced_setting { Motion::Reduced } else { Motion::Full })
}

/// `--reveal-motion` value: none given is `Ok(None)`; anything but the three names is an error.
/// `--reveal-motion V` in the arguments: absent is `Ok(None)`, a missing or unknown value is an error.
pub fn motion_arg(argv: &[String]) -> Result<Option<Motion>, String> {
    match argv.iter().position(|s| s == "--reveal-motion") {
        None => Ok(None),
        Some(i) => parse_motion(argv.get(i + 1).map_or("<missing>", String::as_str)),
    }
}

pub fn parse_motion(v: &str) -> Result<Option<Motion>, String> {
    match v {
        "" => Ok(None),
        "full" => Ok(Some(Motion::Full)),
        "reduced" => Ok(Some(Motion::Reduced)),
        "off" => Ok(Some(Motion::Off)),
        other => Err(format!("--reveal-motion {other}: expected full, reduced or off")),
    }
}

pub fn with_motion(tl: Timeline, m: Motion) -> Timeline {
    match m {
        Motion::Full => tl,
        Motion::Reduced => tl.reduced(),
        Motion::Off => tl.instant(),
    }
}

fn apply_motion(tl: Timeline, a: &crate::shell::EditorApp, explicit: Option<Motion>) -> Timeline {
    with_motion(tl, motion_of(explicit, a.sh.theme.reduced_motion))
}

/// Timeline at a different frame rate: the same events at the same seconds.
pub fn retime(tl: &Timeline, fps: u32) -> Timeline {
    let k = fps as f32 / tl.fps.max(1) as f32;
    let tracks = tl
        .tracks
        .iter()
        .map(|t| Track { t0: (t.t0 as f32 * k).round() as u32, t1: (t.t1 as f32 * k).round() as u32, ..t.clone() })
        .collect();
    Timeline::new(fps, tracks)
}

/// Per-frame cameras: from `cam`, following the active batch (a pure fold of `follow`).
pub fn camera_path(
    scene: &ddc_render::Scene,
    tl: &Timeline,
    cam: &ddc_render::OrbitCamera,
    aspect: f32,
    section: Option<&ddc_render::Aabb>,
) -> (Vec<ddc_render::OrbitCamera>, usize) {
    let mut c = cam.clone();
    let mut stuck = 0;
    let path = (0..tl.len.max(1))
        .map(|f| {
            let n = ddc_render::reveal_export::follow_clear(&tl.active_box(scene, f), &c, aspect, section);
            stuck += usize::from(!n.clear);
            c = n.cam;
            c.clone()
        })
        .collect();
    (path, stuck)
}

/// How the exported reveal moves the camera: one pose framing the whole model (`fixed`, the
/// default), a full turn around it over the show (`orbit`), or framing each batch (`follow`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CamMode {
    Fixed,
    Orbit,
    Follow,
}

/// `--reveal-camera fixed|orbit|follow`: absent is `fixed`; a missing or unknown value is an error.
pub fn camera_arg(argv: &[String]) -> Result<CamMode, String> {
    let Some(i) = argv.iter().position(|s| s == "--reveal-camera") else { return Ok(CamMode::Fixed) };
    match argv.get(i + 1).map_or("<missing>", String::as_str) {
        "fixed" => Ok(CamMode::Fixed),
        "orbit" => Ok(CamMode::Orbit),
        "follow" => Ok(CamMode::Follow),
        other => Err(format!("--reveal-camera {other}: expected fixed, orbit or follow")),
    }
}

/// Per-frame cameras of `mode`; `cam` already frames the whole model. Fixed and orbit keep its
/// target and distance, so nothing jumps between batches.
pub fn camera_path_of(
    mode: CamMode,
    scene: &ddc_render::Scene,
    tl: &Timeline,
    cam: &ddc_render::OrbitCamera,
    aspect: f32,
    section: Option<&ddc_render::Aabb>,
) -> (Vec<ddc_render::OrbitCamera>, usize) {
    let n = tl.len.max(1);
    match mode {
        CamMode::Follow => camera_path(scene, tl, cam, aspect, section),
        CamMode::Fixed => (vec![cam.clone(); n as usize], 0),
        CamMode::Orbit => {
            let turn = std::f32::consts::TAU / n as f32;
            let path = (0..n)
                .map(|f| {
                    let mut c = cam.clone();
                    c.yaw += turn * f as f32;
                    c
                })
                .collect();
            (path, 0)
        }
    }
}

/// `--reveal-out` in the arguments: the run is windowless. A missing value is an error.
pub fn wants_headless(argv: &[String]) -> Result<bool, String> {
    let Some(i) = argv.iter().position(|s| s == "--reveal-out") else { return Ok(false) };
    match argv.get(i + 1) {
        Some(v) if !v.starts_with("--") => Ok(true),
        _ => Err("--reveal-out needs a folder for the frames or a video file such as show.mp4".into()),
    }
}

/// Video file extensions `--reveal-out` encodes to; any other path is a folder of PNG frames.
pub const VIDEO_EXTS: [&str; 4] = ["mp4", "webm", "mov", "gif"];

pub fn is_video(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Arguments of the video encoder for the frames in `frames` (`frame_0000.png`...).
pub fn video_args(frames: &std::path::Path, fps: u32, out: &std::path::Path) -> Vec<String> {
    let input = frames.join("frame_%04d.png").display().to_string();
    let mut a: Vec<String> = ["-y", "-loglevel", "error", "-framerate"].map(String::from).to_vec();
    a.push(fps.to_string());
    a.extend(["-i".to_string(), input]);
    if !out.extension().is_some_and(|e| e.eq_ignore_ascii_case("gif")) {
        // the widest player support: 8-bit 4:2:0, which needs even frame sides (`even`)
        a.extend(["-pix_fmt", "yuv420p"].map(String::from));
    }
    a.push(out.display().to_string());
    a
}

/// Frame size a video takes: 4:2:0 needs even sides.
pub fn even(size: (u32, u32)) -> (u32, u32) {
    ((size.0 & !1).max(2), (size.1 & !1).max(2))
}

/// The encoder command line as text, for the error when no encoder is installed.
pub fn video_recipe(frames: &std::path::Path, fps: u32, out: &std::path::Path) -> String {
    let q = |s: &String| if s.contains(' ') { format!("\"{s}\"") } else { s.clone() };
    let args: Vec<String> = video_args(frames, fps, out).iter().map(q).collect();
    format!("ffmpeg {}", args.join(" "))
}

/// Encodes the frames into `out` with an installed ffmpeg; without one the frames stay and the
/// error gives the command to run.
fn encode(frames: &std::path::Path, fps: u32, out: &std::path::Path) -> Result<(), String> {
    match std::process::Command::new("ffmpeg").args(video_args(frames, fps, out)).status() {
        Ok(st) if st.success() => {
            let _ = std::fs::remove_dir_all(frames);
            Ok(())
        }
        Ok(st) => Err(format!("the video encoder failed ({st}); the frames are in {}", frames.display())),
        Err(_) => Err(format!(
            "no video encoder found; the frames are in {}. Install ffmpeg and run: {}",
            frames.display(),
            video_recipe(frames, fps, out)
        )),
    }
}

/// Scene elements in order of their number: model key (a `scene` key for a mesh the editor
/// cannot name), element range and lowest point.
pub fn scene_elements(
    scene: &ddc_render::Scene,
    key_of: &dyn Fn(u32) -> Option<ElKey>,
) -> Vec<(ElKey, (u32, u32), f32)> {
    let mut by: BTreeMap<ElKey, ((u32, u32), f32)> = BTreeMap::new();
    for (m, b) in scene.meshes.iter().zip(scene.boxes()) {
        if m.element >= crate::scene_ids::SERVICE_BASE || m.positions.is_empty() {
            continue;
        }
        let k = key_of(m.element).unwrap_or_else(|| ("scene".to_string(), i64::from(m.element)));
        let e = by.entry(k).or_insert(((m.element, m.element + 1), f32::INFINITY));
        e.0 = (e.0 .0.min(m.element), e.0 .1.max(m.element + 1));
        e.1 = e.1.min(b.min.z);
    }
    let mut v: Vec<_> = by.into_iter().map(|(k, (r, z))| (k, r, z)).collect();
    v.sort_by_key(|x| x.1);
    v
}

/// The scene with its element numbers packed from 1 in their order, and the map from the old
/// numbers. The renderer animates an element by its number, up to `MAX_ELEMS`; solids and other
/// high ranges would fall outside and never move.
pub fn packed(scene: &ddc_render::Scene) -> (ddc_render::Scene, BTreeMap<u32, u32>) {
    let map = pack_map(scene);
    let mut meshes: Vec<ddc_render::SceneMesh> = scene.meshes.iter().map(|m| (**m).clone()).collect();
    for m in &mut meshes {
        if let Some(n) = map.get(&m.element) {
            m.element = *n;
        }
    }
    let mut s = ddc_render::Scene::new(meshes);
    s.cats = scene.cats.clone();
    s.focus = scene.focus;
    (s, map)
}

/// The map from element numbers of the scene to numbers packed from 1 in their order; numbers
/// from `SERVICE_BASE` up (site, preview, other levels) keep theirs.
pub fn pack_map(scene: &ddc_render::Scene) -> BTreeMap<u32, u32> {
    let mut ids: Vec<u32> = scene
        .meshes
        .iter()
        .map(|m| m.element)
        .filter(|e| *e < crate::scene_ids::SERVICE_BASE)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids.iter().enumerate().map(|(i, e)| (*e, i as u32 + 1)).collect()
}

/// Meshes with their element numbers through `map` (numbers it does not know keep theirs).
pub fn repack(meshes: &mut [std::sync::Arc<ddc_render::SceneMesh>], map: &BTreeMap<u32, u32>) {
    for m in meshes {
        if let Some(n) = map.get(&m.element) {
            let mut own = (**m).clone();
            own.element = *n;
            *m = std::sync::Arc::new(own);
        }
    }
}

/// An old [start, end) element range in packed numbers; `None` when no element of it is drawn.
pub fn packed_range(map: &BTreeMap<u32, u32>, (a, b): (u32, u32)) -> Option<(u32, u32)> {
    let first = *map.range(a..b).next()?.1;
    let last = *map.range(a..b).next_back()?.1;
    Some((first, last + 1))
}

/// Model key of an element of the editor's own scene (`Editor::scene`): solids by their
/// number range, the rest through the selection they stand for.
fn editor_key(ed: &crate::editor::Editor, e: u32) -> Option<ElKey> {
    use crate::solid_tools::{SOLID_BASE, SOLID_SPAN};
    if (SOLID_BASE..SOLID_BASE + SOLID_SPAN).contains(&e) {
        return Some(("solid".to_string(), i64::from(e - SOLID_BASE)));
    }
    let s = ed.sel_of_element(e)?;
    crate::phase_ui::sel_ref(s).map(|(t, i)| (t.to_string(), i))
}

/// A batch spread over less height than this appears at once, metres.
const BAND_MIN: f32 = 1.0;
/// Bands one batch is split into at most.
const MAX_BANDS: usize = 24;

/// Height bands of a reveal: a batch whose elements' lowest points spread over more than
/// `BAND_MIN` is split, bottom to top, into at most `MAX_BANDS` bands of equal height.
pub fn bands(batches: Vec<Vec<Step>>, low: &dyn Fn(&ElKey) -> Option<f32>) -> Vec<Vec<Step>> {
    let mut out = vec![];
    for b in batches {
        let z: Vec<f32> = b.iter().map(|s| low(&s.key).unwrap_or(0.0)).collect();
        let (lo, hi) = z.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(a, c), v| (a.min(*v), c.max(*v)));
        if !(hi - lo > BAND_MIN) {
            out.push(b);
            continue;
        }
        let n = MAX_BANDS.min(((hi - lo) / BAND_MIN).ceil() as usize).max(2);
        let mut split: Vec<Vec<Step>> = vec![vec![]; n];
        for (s, z) in b.into_iter().zip(z) {
            let k = ((z - lo) / (hi - lo) * n as f32).floor() as usize;
            split[k.min(n - 1)].push(s);
        }
        out.extend(split.into_iter().filter(|v| !v.is_empty()));
    }
    out
}

/// Which frames of the show an export writes (`--reveal-from N --reveal-to M --reveal-step K`): the
/// frame-step mode of video tools. Frames are numbered from 0 in the output whatever the step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub from: u32,
    pub to: Option<u32>,
    pub step: u32,
}

impl Default for Span {
    fn default() -> Self {
        Self { from: 0, to: None, step: 1 }
    }
}

impl Span {
    /// Frames of a timeline `len` frames long (the last frame included), in render order.
    pub fn frames(&self, len: u32) -> Vec<u32> {
        let last = len.saturating_sub(1);
        let to = self.to.map_or(last, |t| t.min(last));
        if self.from > to {
            return vec![];
        }
        (self.from..=to).step_by(self.step.max(1) as usize).collect()
    }

    pub fn is_default(&self) -> bool {
        *self == Span::default()
    }

    /// The span named in the arguments; a bad number or an empty range is an error.
    pub fn from_argv(argv: &[String]) -> Result<Span, String> {
        let num = |flag: &str| -> Result<Option<u32>, String> {
            match argv.iter().position(|s| s == flag) {
                None => Ok(None),
                Some(i) => argv
                    .get(i + 1)
                    .and_then(|v| v.parse::<u32>().ok())
                    .map(Some)
                    .ok_or_else(|| format!("{flag}: expected a whole number of frames")),
            }
        };
        let (from, to, step) = (num("--reveal-from")?, num("--reveal-to")?, num("--reveal-step")?);
        if step == Some(0) {
            return Err("--reveal-step: expected 1 or more".into());
        }
        if let (Some(f), Some(t)) = (from, to) {
            if f > t {
                return Err(format!("--reveal-from {f} is after --reveal-to {t}"));
            }
        }
        Ok(Span { from: from.unwrap_or(0), to, step: step.unwrap_or(1) })
    }
}

/// Export frames `frame_0000.png...` without a window; returns the frame count.
#[cfg(feature = "gui")]
#[allow(clippy::too_many_arguments)]
pub fn export_frames(
    scene: &ddc_render::Scene,
    tl: &Timeline,
    cam: &ddc_render::OrbitCamera,
    size: (u32, u32),
    dir: &std::path::Path,
    only: Option<u32>,
    span: Span,
    section: Option<&ddc_render::Aabb>,
    mode: CamMode,
) -> Result<Exported, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let (path, stuck) = camera_path_of(mode, scene, tl, cam, size.0 as f32 / size.1.max(1) as f32, section);
    let cams = |f: u32| path[(f as usize).min(path.len() - 1)].clone();
    let mut r = ddc_render::reveal_export::render_timeline(scene, tl, &cams, size)?;
    let save = |i: u32, img: ddc_render::reveal_export::Rgba8Image| -> Result<(), String> {
        let png = crate::ai::session::encode_png(&img.px, img.w, img.h)?;
        std::fs::write(dir.join(frame_file(i)), png).map_err(|e| e.to_string())
    };
    if let Some(f) = only {
        // one frame (shots): the last frame of the timeline is the finished scene
        let f = f.min(tl.len.saturating_sub(1));
        save(f, r.render_frame(f)?)?;
        return Ok(Exported { frames: 1, stuck_frames: usize::from(stuck > 0 && path.get(f as usize).is_some()) });
    }
    if !span.is_default() {
        let picked = span.frames(tl.len);
        if picked.is_empty() {
            return Err(format!("no frame to write: the show has {} frames", tl.len));
        }
        let mut n = 0;
        for (i, f) in picked.iter().enumerate() {
            save(i as u32, r.render_frame(*f)?)?;
            n += 1;
        }
        let stuck_frames = picked.iter().filter(|f| path.get(**f as usize).is_some()).count().min(stuck);
        return Ok(Exported { frames: n, stuck_frames });
    }
    let mut n = 0;
    for (i, img) in r.enumerate() {
        save(i as u32, img?)?;
        n += 1;
    }
    Ok(Exported { frames: n, stuck_frames: stuck })
}

/// What an export wrote. `stuck_frames` > 0: the camera started inside the section box and
/// could not leave it in that many frames (the pose was kept); the caller reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exported {
    pub frames: usize,
    pub stuck_frames: usize,
}

/// File name of an exported frame.
pub fn frame_file(i: u32) -> String {
    format!("frame_{i:04}.png")
}

/// `--reveal all|calendar --reveal-out DIR|FILE.mp4`: windowless reveal of the open model. `all`
/// builds every element of the scene, scripted parts included, bottom to top in build order;
/// `calendar` goes by stages. A video path encodes the frames (`encode`). The AI plan needs a
/// session and is unavailable without a window.
#[cfg(feature = "gui")]
#[allow(clippy::too_many_arguments)]
pub fn export_cli(
    a: &mut crate::shell::EditorApp,
    mode: &str,
    out: &std::path::Path,
    fps: u32,
    size: (u32, u32),
    only: Option<u32>,
    span: Span,
    cam_mode: CamMode,
) -> Result<Exported, String> {
    let (scene, map) = packed(&a.ed.scene());
    let old: Vec<u32> = map.keys().copied().collect();
    let els = {
        let ed = &a.ed;
        scene_elements(&scene, &|e| old.get((e as usize).checked_sub(1)?).and_then(|o| editor_key(ed, *o)))
    };
    let range: std::collections::HashMap<ElKey, (u32, u32)> = els.iter().map(|(k, r, _)| (k.clone(), *r)).collect();
    let low: std::collections::HashMap<ElKey, f32> = els.iter().map(|(k, _, z)| (k.clone(), *z)).collect();
    let refs = scene_refs(a);
    let b: Vec<Vec<Step>> = match mode {
        "calendar" => {
            let idx = a.ed.model.phase_index().map_err(|e| e.to_string())?;
            let m = &a.ed.model;
            phase_batches(&from_phases(&idx, &|p| m.phase_dates(p).ok().flatten())).0
        }
        "all" | "" => {
            let steps: Vec<Step> = els.iter().map(|(k, ..)| Step::new(k.clone(), Change::Added)).collect();
            bands(batches(&steps, &a.ed.model), &|k| low.get(k).copied())
        }
        other => {
            return Err(format!(
                "--reveal {other}: expected all or calendar (an AI plan needs a live session, so the plan card has its own button)"
            ))
        }
    };
    let resolve = |k: &ElKey| {
        range.get(k).copied().or_else(|| refs.get(k).and_then(|s| a.ed.element_range(*s)).and_then(|r| packed_range(&map, r)))
    };
    let mut tl = timeline(&b, &resolve);
    tl = apply_motion(tl, a, a.reveal_motion);
    let tl = retime(&tl, fps);
    if tl.tracks.is_empty() {
        return Err("nothing to show: no element of the scene is in the timeline".into());
    }
    let video = is_video(out);
    let size = if video { even(size) } else { size };
    let mut cam = ddc_render::OrbitCamera::default();
    if let Some(bd) = scene.bounds {
        cam.fit(&bd, size.0 as f32 / size.1.max(1) as f32);
    }
    let section = a.view.section_box;
    if !video {
        return export_frames(&scene, &tl, &cam, size, out, only, span, section.as_ref(), cam_mode);
    }
    let stem = out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let frames = out.with_file_name(format!("{stem}.frames"));
    let _ = std::fs::remove_dir_all(&frames);
    let n = export_frames(&scene, &tl, &cam, size, &frames, only, span, section.as_ref(), cam_mode)?;
    encode(&frames, fps, out)?;
    Ok(n)
}

/// Player window: back, pause/forward, frame slider, close (the scene is left as it was).
pub fn controls(a: &mut crate::shell::EditorApp, ctx: &egui::Context) {
    if crate::phase_ui::take_calendar_request() {
        start_calendar(a);
    }
    if let Some((steps, preview)) = REQUEST.with(|r| r.borrow_mut().take()) {
        start_requested(a, steps, preview);
    }
    poll_journal(a);
    if a.reveal_job.is_some() {
        ctx.request_repaint();
        let (th, cat) = (&a.sh.theme, &a.sh.cat);
        let stop = crate::ui_kit::Dialog::hud(
            "reveal_job",
            cat.t("collab_diff.reveal.journal_busy"),
            egui::Align2::CENTER_BOTTOM,
            [0.0, -24.0],
        )
        .id(egui::Id::new("ddc_reveal_job"))
        .show(ctx, |ui| ddc_ui::widgets::Button::quiet(&cat.t("collab_diff.reveal.close")).show(ui, th).clicked())
        .and_then(|r| r.inner)
        .unwrap_or(false);
        if stop {
            if let Some(j) = a.reveal_job.take() {
                j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    let Some(p) = a.reveal.as_mut() else { return };
    if p.tick(web_time::Instant::now()) || p.playing() {
        ctx.request_repaint();
    }
    let (th, cat) = (&a.sh.theme, &a.sh.cat);
    let mut close = false;
    // the player row (buttons, frame slider, date) needs a tool width, not a HUD's
    crate::ui_kit::Dialog::hud("reveal", cat.t("collab_diff.reveal.title"), egui::Align2::CENTER_BOTTOM, [0.0, -24.0])
        .id(egui::Id::new("ddc_reveal"))
        .size([ddc_ui::theme::layout::DIALOG_TOOL, 0.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                use ddc_ui::widgets::Button;
                if Button::quiet(&cat.t("collab_diff.reveal.back")).show(ui, th).clicked() {
                    p.play_back();
                }
                let playing = p.playing();
                let label = cat.t(if playing { "collab_diff.reveal.pause" } else { "collab_diff.reveal.play" });
                if Button::quiet(&label).show(ui, th).clicked() {
                    if playing {
                        p.pause();
                    } else if p.backwards() {
                        p.play_back();
                    } else {
                        p.play();
                    }
                }
                let mut f = p.frame();
                if ui.add(egui::Slider::new(&mut f, 0..=p.len()).show_value(false)).changed() {
                    p.seek(f);
                }
                if let Some(c) = p.caption() {
                    ui.label(c);
                }
                if Button::quiet(&cat.t("collab_diff.reveal.close")).show(ui, th).clicked() {
                    close = true;
                }
            });
        });
    if close {
        a.reveal = None;
        set_pack(a, None);
    }
}

/// Steps by stage: what is built in a stage appears on its date, what is demolished disappears on the date of the
/// demolition stage. What is created in the first stage stands from the start and is not part of the reveal. If not all
/// stages have dates, all stages get equal intervals (`EVEN_DAYS`) from the earliest known date.
/// The result is in ascending date order; the `Vec` of steps is ordered by element.
pub fn from_phases(idx: &PhaseIndex, dates: &dyn Fn(i64) -> Option<(Date, Date)>) -> Vec<(Date, Vec<Step>)> {
    let known: Vec<Option<Date>> = idx.phases.iter().map(|p| dates(p.id).map(|d| d.0)).collect();
    let at: Vec<Date> = if known.iter().all(Option::is_some) {
        known.into_iter().flatten().collect()
    } else {
        let base = known.iter().flatten().min().map_or(0, |d| d.days());
        (0..idx.phases.len()).map(|i| Date::from_days(base + i as i64 * EVEN_DAYS)).collect()
    };
    let date_of = |phase: i64| idx.pos(phase).map(|i| at[i]);
    let mut by: BTreeMap<Date, Vec<Step>> = BTreeMap::new();
    for (kind, id, e) in idx.elements() {
        let key: ElKey = (kind.to_string(), id);
        if idx.pos(e.created).is_some_and(|i| i > 0) {
            if let Some(d) = date_of(e.created) {
                by.entry(d).or_default().push(Step::new(key.clone(), Change::Added));
            }
        }
        if let Some(d) = e.demolished.and_then(date_of) {
            by.entry(d).or_default().push(Step::new(key, Change::Removed));
        }
    }
    by.into_iter().collect()
}

/// Days between stages without dates.
pub const EVEN_DAYS: i64 = 30;

/// Calendar reveal timeline and batch labels (dates) for the slider and export.
pub fn phase_batches(v: &[(Date, Vec<Step>)]) -> (Vec<Vec<Step>>, Vec<String>) {
    v.iter().map(|(d, s)| (s.clone(), d.to_string())).unzip()
}

/// Journal batches from the first entry to the last: walk steps go from newest to oldest. Operations without
/// reveal elements are skipped; the label is the operation number and kind.
pub fn from_journal(walk: &[WalkStep]) -> (Vec<Vec<Step>>, Vec<String>) {
    let mut batches = vec![];
    let mut labels = vec![];
    for w in walk.iter().rev().filter(|w| !w.steps.is_empty()) {
        let label = format!("#{} {}", w.seq, w.kind);
        for b in split_big(w.steps.clone()) {
            batches.push(b);
            labels.push(label.clone());
        }
    }
    (batches, labels)
}

/// One journal entry with hundreds of elements (an applied plan) is shown in creation order, in
/// at most `MAX_CHUNKS` batches, not all at once.
fn split_big(mut steps: Vec<Step>) -> Vec<Vec<Step>> {
    const SMALL: usize = 24;
    const MAX_CHUNKS: usize = 60;
    if steps.len() <= SMALL {
        return vec![steps];
    }
    steps.sort_by(|a, b| a.key.1.cmp(&b.key.1).then_with(|| a.key.0.cmp(&b.key.0)));
    let per = steps.len().div_ceil(MAX_CHUNKS).max(SMALL / 4);
    steps.chunks(per).map(<[Step]>::to_vec).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::merge::MergeConflict;
    use ddc_model::{CellDiff, ElementDiff, FieldChange, FieldGroup};

    fn key(t: &str, id: i64) -> ElKey {
        (t.to_string(), id)
    }

    fn row(table: &str, rowid: i64, kind: RowKind, owner: Option<ElKey>, cells: &[(&str, &str, &str)]) -> ddc_model::RowDiff {
        ddc_model::RowDiff {
            table: table.to_string(),
            rowid,
            kind,
            owner,
            cells: cells
                .iter()
                .map(|(c, b, a)| CellDiff {
                    col: c.to_string(),
                    before: Some(b.to_string()),
                    after: Some(a.to_string()),
                })
                .collect(),
        }
    }

    fn own(table: &str, id: i64, kind: RowKind, cells: &[(&str, &str, &str)]) -> ddc_model::RowDiff {
        row(table, id, kind, Some(key(table, id)), cells)
    }

    #[test]
    fn plan_diff_to_steps_skips_cache_tables() {
        let d = ModelDiff {
            rows: vec![
                own("wall", 1, RowKind::Created, &[]),
                // a part of an added wall and cache tables produce no steps
                row("wall_qty", 9, RowKind::Created, Some(key("wall", 1)), &[]),
                row("mesh_proto", 3, RowKind::Created, Some(key("mesh_proto", 3)), &[]),
                row("mesh_geo", 3, RowKind::Changed, Some(key("mesh_geo", 3)), &[]),
                row("comp_thumb", 2, RowKind::Deleted, Some(key("comp_thumb", 2)), &[]),
                // edit side effect
                own("clash_item", 4, RowKind::Deleted, &[]),
                own("col", 5, RowKind::Deleted, &[]),
                own("slab", 6, RowKind::Changed, &[("stype", "1", "2")]),
                // an edit of only a part is a change of the host
                row("slab_vertex", 1, RowKind::Changed, Some(key("slab", 7)), &[("x", "0", "1")]),
                row("meta", 1, RowKind::Changed, None, &[]),
            ],
            truncated: false,
        };
        let s = from_plan(&d);
        let got: Vec<(String, i64, Change)> = s.iter().map(|s| (s.key.0.clone(), s.key.1, s.change)).collect();
        assert_eq!(
            got,
            vec![
                ("col".to_string(), 5, Change::Removed),
                ("slab".to_string(), 6, Change::Modified),
                ("slab".to_string(), 7, Change::Modified),
                ("wall".to_string(), 1, Change::Added),
            ]
        );
        assert!(s.iter().all(|s| s.from.is_none()));
    }

    #[test]
    fn modified_moved_wall_becomes_move() {
        let moved = ModelDiff {
            rows: vec![own("wall", 3, RowKind::Changed, &[("ax", "0", "2"), ("bx", "5", "7")])],
            truncated: false,
        };
        let s = from_plan(&moved);
        assert_eq!(s[0].from, Some([-2.0, 0.0, 0.0]));
        let tl = timeline(&[s], &|k| Some((k.1 as u32 + 10, k.1 as u32 + 11)));
        let mv: Vec<_> = tl
            .tracks
            .iter()
            .filter(|t| matches!(t.effect, Effect::Move { .. }))
            .collect();
        assert_eq!(mv.len(), 1);
        assert_eq!(mv[0].elements, (13, 14));
        assert_eq!(mv[0].effect, Effect::Move { from: Vec3::new(-2.0, 0.0, 0.0) });
        // a stretched wall and a wall with another edit are not a move
        let stretched = ModelDiff {
            rows: vec![own("wall", 3, RowKind::Changed, &[("bx", "5", "7")])],
            truncated: false,
        };
        assert_eq!(from_plan(&stretched)[0].from, None);
        let also_part = ModelDiff {
            rows: vec![
                own("wall", 3, RowKind::Changed, &[("ax", "0", "2"), ("bx", "5", "7")]),
                row("wall_qty", 1, RowKind::Changed, Some(key("wall", 3)), &[("area", "1", "2")]),
            ],
            truncated: false,
        };
        assert_eq!(from_plan(&also_part)[0].from, None);
        // version comparison: the same rule by fields
        let v = VersionDiff {
            elements: vec![ElementDiff {
                table: "wall".into(),
                id: 3,
                kind: DiffKind::Differs,
                key: None,
                other: Some(3),
                fields: vec!["ay".into(), "by".into()],
                changes: ["ay", "by"]
                    .iter()
                    .map(|f| FieldChange {
                        field: f.to_string(),
                        group: FieldGroup::Column,
                        old: Some("1".into()),
                        new: Some("0".into()),
                    })
                    .collect(),
                spot: None,
            }],
            total: 1,
            tables: vec![],
        };
        assert_eq!(from_version(&v)[0].from, Some([0.0, 1.0, 0.0]));
    }

    #[test]
    fn version_diff_directions() {
        let e = |t: &str, id, kind| ElementDiff {
            table: t.into(),
            id,
            kind,
            key: None,
            other: None,
            fields: vec![],
            changes: vec![],
            spot: None,
        };
        let v = VersionDiff {
            elements: vec![
                e("wall", 1, DiffKind::HereOnly),
                e("col", 2, DiffKind::ThereOnly),
                e("slab", 3, DiffKind::Differs),
            ],
            total: 3,
            tables: vec![],
        };
        let c: Vec<Change> = from_version(&v).iter().map(|s| s.change).collect();
        assert_eq!(c, [Change::Added, Change::Removed, Change::Modified]);
    }

    #[test]
    fn merge_preview_steps_match_conflict_list() {
        let c = |table: &str, id, case| MergeConflict { table: table.into(), id, case, fields: vec![] };
        let p = MergePlan {
            conflicts: vec![
                c("wall", 1, MergeCase::EditEdit),
                c("col", 2, MergeCase::DeletedThere),
                c("slab", 3, MergeCase::DeletedHere),
                c("beam", 4, MergeCase::AddAdd),
            ],
            ..Default::default()
        };
        let s = from_merge(&p);
        assert_eq!(s.len(), p.conflicts.len());
        let want = [Change::Modified, Change::Removed, Change::Added, Change::Added];
        for ((st, cf), w) in s.iter().zip(&p.conflicts).zip(want) {
            assert_eq!((st.key.clone(), st.change), ((cf.table.clone(), cf.id), w));
        }
        assert!(from_merge(&MergePlan::default()).is_empty());
    }

    #[test]
    fn timeline_runs_merge_and_follow_batch_order() {
        let steps: Vec<Step> = (1..=4).map(|i| Step::new(key("wall", i), Change::Added)).collect();
        let more = vec![Step::new(key("roof", 1), Change::Added), Step::new(key("col", 1), Change::Removed)];
        let m = Model::open(":memory:").unwrap();
        let mut all = steps;
        all.extend(more);
        let b = batches(&all, &m);
        // removed items, then walls, then roofing
        assert_eq!(b.len(), 3);
        let tl = timeline(&b, &|k| {
            let e = k.1 as u32 + if k.0 == "wall" { 100 } else { 200 };
            Some((e, e + 1))
        });
        // walls 101..=104 form one appearance track per batch
        let walls: Vec<_> = tl
            .tracks
            .iter()
            .filter(|t| matches!(t.effect, Effect::Appear { .. }) && t.elements.0 == 101)
            .collect();
        assert_eq!(walls.len(), 1);
        assert_eq!(walls[0].elements, (101, 105));
        assert_eq!(walls[0].t0, BATCH_STEP);
        let roof = tl.tracks.iter().find(|t| t.elements == (201, 202) && matches!(t.effect, Effect::Appear { .. })).unwrap();
        assert_eq!(roof.t0, 2 * BATCH_STEP);
        assert_eq!(tl.len, 2 * BATCH_STEP + BATCH_FRAMES);
        // an element with no scene number is skipped
        assert!(timeline(&b, &|_| None).tracks.is_empty());
    }

    fn small_timeline() -> Timeline {
        timeline(&[vec![Step::new(key("wall", 1), Change::Added)]], &|_| Some((1, 2)))
    }

    #[test]
    fn player_runs_pauses_at_the_end_and_goes_back() {
        let mut p = Player::new(small_timeline());
        assert_eq!((p.frame(), p.playing()), (0, true));
        assert!(p.advance(0.5));
        assert!(p.frame() > 0 && p.frame() < p.len());
        // does not go past the end and pauses
        assert!(p.advance(10.0));
        assert_eq!((p.frame(), p.playing()), (p.len(), false));
        assert!(!p.advance(1.0));
        // reverse play from the end to the start
        p.play_back();
        assert!(p.backwards() && p.playing());
        p.advance(10.0);
        assert_eq!((p.frame(), p.playing()), (0, false));
        // the slider sets the frame and pauses; it cannot go out of range
        p.seek(9999);
        assert_eq!((p.frame(), p.playing()), (p.len(), false));
        p.seek(7);
        assert_eq!(p.frame(), 7);
        // starting from the end restarts from the beginning
        p.seek(p.len());
        p.play();
        assert_eq!(p.frame(), 0);
    }

    #[test]
    fn player_frames_repeat_until_the_frame_changes() {
        let mut p = Player::new(small_timeline());
        p.seek(5);
        let (r1, f1) = p.current();
        let (r2, f2) = p.current();
        assert_eq!(r1, r2);
        assert!(std::sync::Arc::ptr_eq(&f1, &f2));
        p.seek(6);
        let (r3, f3) = p.current();
        assert!(r3 > r2 && !std::sync::Arc::ptr_eq(&f1, &f3));
        // the last frame is a static scene: everything in place, nothing recolored
        p.seek(p.len());
        let (_, end) = p.current();
        assert!(end.anim.iter().all(|e| *e == ElemAnim::REST));
        assert!(end.tint.iter().all(|t| *t == ElemTint::NONE));
        assert_eq!(end.anim.len(), 2);
    }

    #[test]
    fn plan_of_2000_rows_makes_a_timeline_within_the_budget() {
        let kinds = ["wall", "col", "slab", "beam", "comp"];
        let rows: Vec<_> = (0..2000i64)
            .map(|i| own(kinds[i as usize % 5], i / 5 + 1, if i % 7 == 0 { RowKind::Deleted } else { RowKind::Created }, &[]))
            .collect();
        let d = ModelDiff { rows, truncated: false };
        let m = Model::open(":memory:").unwrap();
        let t = web_time::Instant::now();
        let steps = from_plan(&d);
        let b = batches(&steps, &m);
        let tl = timeline(&b, &|k| {
            let e = k.1 as u32 * 8 + kinds.iter().position(|x| *x == k.0).unwrap_or(0) as u32;
            Some((e, e + 1))
        });
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(steps.len(), 2000);
        assert!(!tl.tracks.is_empty());
        assert!(ms <= 150.0, "plan of 2000 rows: {ms:.1} ms");
    }

    fn d(y: i32, m: u8, dd: u8) -> Date {
        Date::new(y, m, dd).unwrap()
    }

    fn phased_model() -> (Model, [i64; 3], [i64; 2]) {
        use ddc_model::{LayerSpec, WallSpec};
        let mut m = Model::open(":memory:").unwrap();
        let ph: Vec<i64> = m.phases().unwrap().iter().map(|p| p.id).collect();
        let lv = m.add_level("L0", 0.0).unwrap();
        let wt = m.add_wall_type("w", &[LayerSpec { material: "m".into(), thickness: 0.2 }]).unwrap();
        let mk = |m: &mut Model, x: f64| {
            m.add_wall(&WallSpec { a: (x, 0.0), b: (x, 4.0), level: lv, base_offset: 0.0, height: 3.0, wall_type: wt, offset0: None })
                .unwrap()
        };
        let (old, new) = (mk(&mut m, 0.0), mk(&mut m, 5.0));
        m.set_element_phases(&[("wall", old)], Some(ph[0]), Some(Some(ph[1]))).unwrap();
        m.set_element_phases(&[("wall", new)], Some(ph[2]), None).unwrap();
        (m, [ph[0], ph[1], ph[2]], [old, new])
    }

    #[test]
    fn demolished_vanishes_on_demolition_date() {
        let (mut m, ph, [old, new]) = phased_model();
        m.set_phase_dates(ph[0], Some((d(2024, 1, 1), d(2024, 12, 31)))).unwrap();
        m.set_phase_dates(ph[1], Some((d(2025, 3, 1), d(2025, 3, 20)))).unwrap();
        m.set_phase_dates(ph[2], Some((d(2025, 4, 1), d(2025, 9, 1)))).unwrap();
        let idx = m.phase_index().unwrap();
        let got = from_phases(&idx, &|p| m.phase_dates(p).unwrap());
        assert_eq!(
            got,
            vec![
                (d(2025, 3, 1), vec![Step::new(key("wall", old), Change::Removed)]),
                (d(2025, 4, 1), vec![Step::new(key("wall", new), Change::Added)]),
            ]
        );
        let (b, labels) = phase_batches(&got);
        assert_eq!((b.len(), labels), (2, vec!["2025-03-01".to_string(), "2025-04-01".to_string()]));
    }

    #[test]
    fn phases_without_dates_get_even_spacing() {
        let (m, _, [old, new]) = phased_model();
        let idx = m.phase_index().unwrap();
        let got = from_phases(&idx, &|_| None);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].1, vec![Step::new(key("wall", old), Change::Removed)]);
        assert_eq!(got[1].1, vec![Step::new(key("wall", new), Change::Added)]);
        assert_eq!(got[1].0.days() - got[0].0.days(), EVEN_DAYS, "neighbouring phases are an equal step apart");
        // partially given dates are not mixed with equal ones: the same order and step
        let part = from_phases(&idx, &|p| (p == idx.phases[0].id).then(|| (d(2025, 1, 1), d(2025, 1, 2))));
        assert_eq!(part[0].0, Date::from_days(d(2025, 1, 1).days() + EVEN_DAYS));
    }

    #[test]
    fn plan_card_diffs_become_steps() {
        use crate::ai::plan::ElemDiff;
        let e = |id: i64, kind: RowKind| ElemDiff { key: key("wall", id), kind, item: None, level: None, label: String::new(), cells: vec![] };
        let got = from_elem_diffs(&[e(1, RowKind::Created), e(2, RowKind::Deleted), e(3, RowKind::Changed)]);
        let ch: Vec<_> = got.iter().map(|s| (s.key.1, s.change)).collect();
        assert_eq!(ch, vec![(1, Change::Added), (2, Change::Removed), (3, Change::Modified)]);
        request(got, true);
        assert!(REQUEST.with(|r| r.borrow_mut().take()).is_some_and(|(s, p)| s.len() == 3 && p));
    }

    #[test]
    fn frame_files_sort_in_order() {
        assert_eq!(frame_file(0), "frame_0000.png");
        assert_eq!(frame_file(40), "frame_0040.png");
        let mut v: Vec<String> = [180, 9, 40].map(frame_file).to_vec();
        v.sort();
        assert_eq!(v, ["frame_0009.png", "frame_0040.png", "frame_0180.png"]);
    }

    #[test]
    fn motion_flag_is_validated_in_the_arguments() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(motion_arg(&v(&["ddc"])), Ok(None));
        assert_eq!(motion_arg(&v(&["ddc", "--reveal-motion", "off"])), Ok(Some(Motion::Off)));
        assert!(motion_arg(&v(&["ddc", "--reveal-motion", "x"])).unwrap_err().contains("full, reduced or off"));
        assert!(motion_arg(&v(&["ddc", "--reveal-motion"])).is_err(), "a missing value is an error");
    }

    #[test]
    fn camera_reveal_out_and_video_arguments() {
        use std::path::Path;
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(camera_arg(&v(&["ddc"])), Ok(CamMode::Fixed));
        assert_eq!(camera_arg(&v(&["ddc", "--reveal-camera", "orbit"])), Ok(CamMode::Orbit));
        assert!(camera_arg(&v(&["ddc", "--reveal-camera"])).is_err());
        assert_eq!(wants_headless(&v(&["ddc", "--open-project", "a.ddc"])), Ok(false));
        assert_eq!(wants_headless(&v(&["ddc", "--reveal-out", "show.mp4"])), Ok(true));
        assert!(wants_headless(&v(&["ddc", "--reveal-out", "--reveal", "all"])).is_err());
        assert!(is_video(Path::new("a/show.MP4")) && !is_video(Path::new("a/frames")));
        let a = video_args(Path::new("f"), 24, Path::new("o.mp4"));
        assert!(a.windows(2).any(|w| w == ["-framerate", "24"]) && a.contains(&"yuv420p".to_string()));
        assert!(!video_args(Path::new("f"), 24, Path::new("o.gif")).contains(&"yuv420p".to_string()));
        assert_eq!(even((1281, 721)), (1280, 720));
        assert!(video_recipe(Path::new("my frames"), 30, Path::new("o.mp4")).starts_with("ffmpeg "));
    }

    fn two_levels_scene() -> ddc_render::Scene {
        let m = |e: u32, z: f32| ddc_render::SceneMesh {
            element: e,
            tag: 0,
            positions: vec![[0.0, 0.0, z], [1.0, 0.0, z], [0.0, 1.0, z + 3.0]],
            triangles: vec![[0, 1, 2]],
        };
        ddc_render::Scene::new(vec![m(7, 40.0), m(5, 0.0), m(6, 20.0), m(crate::scene_ids::SERVICE_BASE, 0.0)])
    }

    /// Script-made solids (no model selection) still reveal, bottom to top in bands.
    #[test]
    fn every_scene_element_reveals_bottom_up() {
        let sc = two_levels_scene();
        let els = scene_elements(&sc, &|e| (e == 6).then(|| ("solid".to_string(), 1)));
        assert_eq!(els.len(), 3, "service meshes are not part of the show");
        assert_eq!(els[1].0, ("solid".to_string(), 1));
        assert_eq!(els[0].0, ("scene".to_string(), 5));
        let low: std::collections::HashMap<_, _> = els.iter().map(|(k, _, z)| (k.clone(), *z)).collect();
        // one batch as given, top first: the bands put it in height order
        let batch = vec![els[2].0.clone(), els[1].0.clone(), els[0].0.clone()]
            .into_iter()
            .map(|k| Step::new(k, Change::Added))
            .collect::<Vec<_>>();
        let b = bands(vec![batch], &|k| low.get(k).copied());
        assert_eq!(b.len(), 3, "{b:?}");
        let z: Vec<f32> = b.iter().map(|x| low[&x[0].key]).collect();
        assert!(z.windows(2).all(|w| w[0] < w[1]), "{z:?}");
        // a flat batch stays one
        let flat = vec![Step::new(els[0].0.clone(), Change::Added)];
        assert_eq!(bands(vec![flat], &|_| Some(0.0)).len(), 1);
    }

    /// Solid numbers start at 0x2000_0000, past the renderer's animation table: packed, they move.
    #[test]
    fn packed_numbers_fit_the_animation_table() {
        let m = |e: u32| ddc_render::SceneMesh {
            element: e,
            tag: 0,
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            triangles: vec![[0, 1, 2]],
        };
        let base = crate::solid_tools::SOLID_BASE;
        let sc = ddc_render::Scene::new(vec![m(base + 7), m(5), m(base + 3), m(crate::scene_ids::SERVICE_BASE)]);
        let (p, map) = packed(&sc);
        let mut got: Vec<u32> = p.meshes.iter().map(|m| m.element).collect();
        got.truncate(3);
        assert_eq!(got, vec![3, 1, 2]);
        assert!(got.iter().all(|e| (*e as usize) < ddc_render::reveal::MAX_ELEMS));
        assert_eq!(p.meshes[3].element, crate::scene_ids::SERVICE_BASE, "service meshes keep theirs");
        assert_eq!(packed_range(&map, (base, base + 8)), Some((2, 4)));
        assert_eq!(packed_range(&map, (100, 200)), None);
    }

    #[test]
    fn fixed_and_orbit_cameras_keep_framing_the_whole_model() {
        let sc = two_levels_scene();
        let mut cam = ddc_render::OrbitCamera::default();
        cam.fit(&sc.bounds.unwrap(), 1.6);
        let tl = Timeline { fps: 30, tracks: vec![], len: 40 };
        let (fixed, _) = camera_path_of(CamMode::Fixed, &sc, &tl, &cam, 1.6, None);
        assert_eq!(fixed.len(), 40);
        assert!(fixed.iter().all(|c| c.target == cam.target && c.distance == cam.distance));
        let (orbit, _) = camera_path_of(CamMode::Orbit, &sc, &tl, &cam, 1.6, None);
        assert!(orbit.iter().all(|c| c.target == cam.target && c.distance == cam.distance));
        let turn = orbit[39].yaw - orbit[0].yaw;
        assert!((turn - std::f32::consts::TAU * 39.0 / 40.0).abs() < 1e-4, "{turn}");
    }

    #[test]
    fn motion_choice_and_parsing() {
        assert_eq!(motion_of(None, false), Motion::Full);
        assert_eq!(motion_of(None, true), Motion::Reduced);
        assert_eq!(motion_of(Some(Motion::Off), false), Motion::Off);
        assert_eq!(motion_of(Some(Motion::Full), true), Motion::Full);
        assert_eq!(parse_motion(""), Ok(None));
        assert_eq!(parse_motion("off"), Ok(Some(Motion::Off)));
        assert_eq!(parse_motion("reduced"), Ok(Some(Motion::Reduced)));
        assert_eq!(parse_motion("full"), Ok(Some(Motion::Full)));
        let e = parse_motion("fast").unwrap_err();
        assert!(e.contains("fast") && e.contains("full") && e.contains("reduced") && e.contains("off"), "{e}");
    }

    #[test]
    fn frame_span_steps_and_rejects_bad_ranges() {
        let a = |v: &[&str]| Span::from_argv(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(a(&[]).unwrap().is_default());
        let sp = a(&["--reveal-from", "10", "--reveal-to", "25", "--reveal-step", "5"]).unwrap();
        assert_eq!(sp.frames(100), vec![10, 15, 20, 25]);
        assert_eq!(sp.frames(18), vec![10, 15], "clipped to the last frame");
        assert!(sp.frames(5).is_empty());
        assert!(a(&["--reveal-step", "0"]).is_err());
        assert!(a(&["--reveal-from", "9", "--reveal-to", "3"]).is_err());
        assert!(a(&["--reveal-from", "x"]).is_err());
    }

    #[test]
    fn a_big_journal_entry_plays_in_creation_order_in_many_batches() {
        let steps: Vec<Step> = (1..=500).rev().map(|i| Step::new(key("solid", i), Change::Added)).collect();
        let w = WalkStep {
            seq: 6,
            kind: "wall_ai_plan".into(),
            changes: Default::default(),
            steps,
        };
        let (b, labels) = from_journal(&[w]);
        assert!(b.len() > 10 && b.len() <= 60 && labels.len() == b.len());
        let ids: Vec<i64> = b.iter().flatten().map(|s| s.key.1).collect();
        assert_eq!(ids, (1..=500).collect::<Vec<_>>());
    }
}
