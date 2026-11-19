// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The "First steps" tutorial: 7 lessons; a step counts by editor state (wall count,
//! active tool, view), the step's button is highlighted on the ribbon. Progress is in the settings file,
//! and the tutorial can be closed and resumed from the same step.
use crate::ui_kit as kit;
use ddc_model::Model;
use ddc_ui::{Catalog, Registry};
use ddc_ui::theme::{control, layout, space, stroke, Role};
use ddc_ui::widgets::{self, Button};
use ddc_ui::Theme;
use std::sync::atomic::{AtomicU32, Ordering};

/// Model counters that steps are checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Walls,
    Doors,
    Windows,
    Rooms,
    Slabs,
    Levels,
    Roofs,
    Dims,
    Views,
    Tags,
}

const FIELDS: usize = 10;

const COUNT_SQL: [&str; FIELDS - 1] = [
    "SELECT COUNT(*) FROM wall",
    "SELECT COUNT(*) FROM opening WHERE kind='door'",
    "SELECT COUNT(*) FROM opening WHERE kind='window'",
    "SELECT COUNT(*) FROM room",
    "SELECT COUNT(*) FROM slab",
    "SELECT COUNT(*) FROM level",
    "SELECT COUNT(*) FROM roof",
    "SELECT COUNT(*) FROM dim",
    "SELECT COUNT(*) FROM view_def",
];

/// State snapshot: model and UI counters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snap {
    pub n: [i64; FIELDS],
    pub view3d: bool,
    /// Command of the active tool.
    pub active: Option<String>,
    /// Selection category (`wall`, `opening`...).
    pub sel: Option<String>,
    pub palette: bool,
    /// Counters of the XLSX exchange steps: export, workbook edit, apply.
    pub xlsx: [u32; 3],
}

static XLSX: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];

/// Export to a workbook finished.
pub fn note_xlsx_exported() {
    XLSX[0].fetch_add(1, Ordering::Relaxed);
}

/// The workbook preview found edits.
pub fn note_xlsx_edited() {
    XLSX[1].fetch_add(1, Ordering::Relaxed);
}

/// Apply from the workbook done.
pub fn note_xlsx_applied() {
    XLSX[2].fetch_add(1, Ordering::Relaxed);
}

pub fn xlsx_counts() -> [u32; 3] {
    [0, 1, 2].map(|i| XLSX[i].load(Ordering::Relaxed))
}

impl Snap {
    pub fn get(&self, f: Field) -> i64 {
        self.n[f as usize]
    }
    pub fn set(&mut self, f: Field, v: i64) {
        self.n[f as usize] = v;
    }
    /// Model counters; marks are of level `level`. A missing table gives 0.
    pub fn counts(m: &Model, level: i64) -> [i64; FIELDS] {
        let mut n = [0; FIELDS];
        for (i, sql) in COUNT_SQL.iter().enumerate() {
            n[i] = m.sql().query_row(sql, [], |r| r.get(0)).unwrap_or(0);
        }
        n[Field::Tags as usize] = m.tags(level).map_or(0, |t| t.len() as i64);
        n
    }
}

/// A step condition relative to the snapshot at the step start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Check {
    PaletteOpen,
    View3d(bool),
    /// The command's tool is active.
    Active(&'static str),
    /// No active tool (Esc).
    Idle,
    /// An element of the category is selected.
    Selected(&'static str),
    /// The counter grew by at least n.
    More(Field, i64),
    /// The counter dropped by at least n.
    Fewer(Field, i64),
    /// The workbook was exported (after the step began).
    XlsxExported,
    /// Edits were found in the workbook.
    XlsxEdited,
    /// Edits from the workbook were applied.
    XlsxApplied,
}

pub fn done(c: Check, base: &Snap, now: &Snap) -> bool {
    match c {
        Check::PaletteOpen => now.palette,
        Check::View3d(on) => now.view3d == on,
        Check::Active(id) => now.active.as_deref() == Some(id),
        Check::Idle => now.active.is_none(),
        Check::Selected(cat) => now.sel.as_deref() == Some(cat),
        Check::More(f, k) => now.get(f) - base.get(f) >= k,
        Check::Fewer(f, k) => base.get(f) - now.get(f) >= k,
        Check::XlsxExported => now.xlsx[0] > base.xlsx[0],
        Check::XlsxEdited => now.xlsx[1] > base.xlsx[1],
        Check::XlsxApplied => now.xlsx[2] > base.xlsx[2],
    }
}

pub struct Step {
    pub check: Check,
    /// The ribbon button the step highlights.
    pub cmd: Option<&'static str>,
    /// Canvas action: show a hint over the view.
    pub canvas: bool,
}

pub struct Lesson {
    /// String keys: `tut.<id>.title`, `tut.<id>.<step number from 1>`.
    pub id: &'static str,
    pub steps: &'static [Step],
}

const fn st(check: Check, cmd: Option<&'static str>, canvas: bool) -> Step {
    Step { check, cmd, canvas }
}

use Check::*;
use Field::*;

pub static LESSONS: [Lesson; 7] = [
    Lesson {
        id: "ui",
        steps: &[
            st(PaletteOpen, Some("app.palette"), false),
            st(View3d(true), Some("view.3d"), false),
            st(View3d(false), Some("view.plan"), false),
        ],
    },
    Lesson {
        id: "walls",
        steps: &[
            st(Active("arch.wall"), Some("arch.wall"), false),
            st(More(Walls, 4), None, true),
            st(Idle, None, false),
            st(Selected("wall"), None, true),
        ],
    },
    Lesson {
        id: "openings",
        steps: &[
            st(Active("arch.door"), Some("arch.door"), false),
            st(More(Doors, 1), None, true),
            st(Active("arch.window"), Some("arch.window"), false),
            st(More(Windows, 2), None, true),
        ],
    },
    Lesson {
        id: "rooms",
        steps: &[
            st(Active("arch.wall_room"), Some("arch.wall_room"), false),
            st(More(Walls, 4), None, true),
            st(Fewer(Walls, 1), Some("edit.undo"), false),
            st(More(Walls, 1), Some("edit.redo"), false),
        ],
    },
    Lesson {
        id: "levels",
        steps: &[
            st(More(Levels, 1), Some("mgr.level"), false),
            st(More(Roofs, 1), Some("arch.roof"), true),
            st(View3d(true), Some("view.3d"), false),
        ],
    },
    Lesson {
        id: "docs",
        steps: &[
            st(More(Dims, 1), Some("ann.dim"), true),
            st(More(Views, 1), Some("arch.section"), true),
            st(More(Tags, 1), Some("ann.tag"), true),
        ],
    },
    Lesson {
        id: "spreadsheet", // i18n key part (tut.spreadsheet.*)
        steps: &[
            st(XlsxExported, Some("data.export_xlsx"), false),
            st(XlsxEdited, None, false),
            st(XlsxApplied, None, false),
        ],
    },
];

/// Tutorial state; `step == steps.len()` means the lesson is done and waits for "Next".
#[derive(Default)]
pub struct Tour {
    pub open: bool,
    pub lesson: usize,
    pub step: usize,
    /// Completed lessons, one bit per lesson.
    pub done: u8,
    base: Option<Snap>,
    /// A new step: the host switches the ribbon tab to the step's button.
    pub entered: bool,
    loaded: bool,
    /// Counters by model revision and level (not recomputed every frame).
    cache: Option<(u64, i64, [i64; FIELDS])>,
}

impl Tour {
    pub fn lesson(&self) -> &'static Lesson {
        &LESSONS[self.lesson.min(LESSONS.len() - 1)]
    }
    pub fn current(&self) -> Option<&'static Step> {
        self.lesson().steps.get(self.step)
    }
    pub fn finished(&self) -> bool {
        self.current().is_none()
    }
    pub fn all_done(&self) -> bool {
        self.done.count_ones() as usize >= LESSONS.len()
    }
    /// Open at the saved step.
    pub fn open(&mut self) {
        if !self.loaded {
            self.loaded = true;
            if let Some((l, s, d)) = load().as_deref().and_then(decode) {
                (self.lesson, self.step, self.done) = (l, s, d);
            }
        }
        self.open = true;
        self.restart_step();
    }
    pub fn go(&mut self, lesson: usize, step: usize) {
        self.lesson = lesson.min(LESSONS.len() - 1);
        self.step = step.min(self.lesson().steps.len());
        self.restart_step();
        save(self);
    }
    fn restart_step(&mut self) {
        self.base = None;
        self.entered = true;
    }
    /// The next lesson (after a completed one, the first uncompleted).
    pub fn next_lesson(&mut self) {
        let n = LESSONS.len();
        let next = (1..=n)
            .map(|k| (self.lesson + k) % n)
            .find(|&i| self.done & (1 << i) == 0)
            .unwrap_or((self.lesson + 1) % n);
        self.go(next, 0);
    }
    pub fn skip(&mut self) {
        self.advance();
    }
    pub fn back(&mut self) {
        if self.step > 0 {
            let s = self.step - 1;
            self.go(self.lesson, s);
        }
    }
    fn advance(&mut self) {
        let len = self.lesson().steps.len();
        self.step = (self.step + 1).min(len);
        if self.step == len {
            self.done |= 1 << self.lesson;
        }
        self.restart_step();
        save(self);
    }
    /// A snapshot with the model counters cached by revision.
    pub fn snap(&mut self, m: &Model, revision: u64, level: i64) -> [i64; FIELDS] {
        match self.cache {
            Some((r, l, n)) if r == revision && l == level => n,
            _ => {
                let n = Snap::counts(m, level);
                self.cache = Some((revision, level, n));
                n
            }
        }
    }
    /// Check the current step; true means it counted.
    pub fn tick(&mut self, now: &Snap) -> bool {
        let Some(step) = self.current() else {
            return false;
        };
        let base = self.base.get_or_insert_with(|| now.clone());
        // a "select"/"3D" step does not count at once if the condition was already met before the step
        let fresh = !matches!(step.check, Selected(_) | View3d(_) | Idle | Active(_))
            || !done(step.check, base, base);
        if fresh && done(step.check, base, now) {
            self.advance();
            return true;
        }
        if !fresh && !done(step.check, base, now) {
            // the condition was reset: wait for it again
            self.base = Some(now.clone());
        }
        false
    }
}

/// Progress as a `lesson step mask` string.
pub fn encode(t: &Tour) -> String {
    format!("{} {} {}", t.lesson, t.step, t.done)
}

pub fn decode(s: &str) -> Option<(usize, usize, u8)> {
    let mut it = s.split_whitespace().map(|x| x.parse::<u32>().ok());
    let (l, st, d) = (it.next()??, it.next()??, it.next()??);
    let l = (l as usize).min(LESSONS.len() - 1);
    let st = (st as usize).min(LESSONS[l].steps.len());
    Some((l, st, u8::try_from(d).ok()? & ((1u8 << LESSONS.len()) - 1)))
}

fn path() -> Option<std::path::PathBuf> {
    crate::gfx::data_dir().map(|d| d.join("tutorial.txt"))
}

fn load() -> Option<String> {
    std::fs::read_to_string(path()?).ok()
}

fn save(t: &Tour) {
    if cfg!(test) {
        return;
    }
    if let Some(p) = path() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = crate::safe_file::write_atomic(&p, encode(t).as_bytes());
    }
}

/// What the host should do after the card frame.
#[derive(Debug, PartialEq)]
pub enum Out {
    None,
    Run(String),
}

/// The lesson card in the bottom-right corner, ribbon button highlight, a hint over the canvas.
pub fn show(ctx: &egui::Context, t: &mut Tour, cat: &Catalog, reg: &Registry) -> Out {
    let mut out = Out::None;
    if !t.open {
        return out;
    }
    let th = ddc_ui::theme::of(ctx);
    let mut open = true;
    kit::Dialog::hud(
        "tutorial",
        cat.t("tut.title"),
        egui::Align2::RIGHT_BOTTOM,
        [-space::LG, -(space::XXL + space::MD)],
    )
    .open(&mut open)
    .collapsible(true)
    .size([layout::DIALOG_HUD, 0.0])
    .show(ctx, |ui| {
        ui.set_max_width(layout::DIALOG_HUD);
        out = card(ui, &th, t, cat, reg);
    });
    if !open {
        t.open = false;
        save(t);
        return out;
    }
    if let Some(step) = t.current() {
        let calm = th.reduced_motion || ctx.style().animation_time <= 0.0;
        let now = ctx.input(|i| i.time);
        let k = pulse_k(step_age(ctx, (t.lesson, t.step), now), calm);
        let accent = th.sem().accent;
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("ddc.help.tour.hl"),
        ));
        if let Some(r) = step
            .cmd
            .and_then(|id| ddc_ui::widgets::ribbon::button_rect(ctx, id))
        {
            let r = r.expand(space::XS);
            let corner = th.radius("md");
            painter.rect_stroke(
                r,
                corner,
                egui::Stroke::new(stroke::FOCUS, accent),
                egui::StrokeKind::Outside,
            );
            if let Some(k) = k {
                painter.rect_stroke(
                    r.expand(k * space::SM),
                    corner,
                    egui::Stroke::new(stroke::FOCUS, accent.gamma_multiply(1.0 - k)),
                    egui::StrokeKind::Outside,
                );
                // one short pulse after a step change; a steady ring needs no frames
                ctx.request_repaint_after(PULSE_FRAME);
            }
        }
        if step.canvas {
            let sr = ctx.screen_rect();
            let at = egui::pos2(sr.center().x, sr.top() + sr.height() * 0.3);
            let on = th.sem().accent_text_on;
            let galley = painter.layout_no_wrap(cat.t("tut.canvas"), th.font(Role::Title), on);
            let pad = egui::vec2(space::LG, space::SM);
            let r = egui::Rect::from_center_size(at, galley.size() + pad * 2.0);
            painter.rect_filled(r, th.radius("lg"), accent);
            painter.galley(r.min + pad, galley, on);
        }
    }
    out
}

/// Length of the pulse that marks a new step, seconds.
const PULSE_S: f64 = 0.8;
/// Frame gap while the pulse runs (about 60 fps).
const PULSE_FRAME: std::time::Duration = std::time::Duration::from_millis(16);

/// Pulse progress 0..1 while it runs; none when it is over or motion is reduced.
pub fn pulse_k(age: f64, calm: bool) -> Option<f32> {
    (!calm && (0.0..PULSE_S).contains(&age)).then(|| (age / PULSE_S) as f32)
}

/// Seconds since the step was first drawn; a new (lesson, step) restarts the clock.
fn step_age(ctx: &egui::Context, key: (usize, usize), now: f64) -> f64 {
    let id = egui::Id::new("ddc.help.tour.step");
    let prev = ctx.data(|d| d.get_temp::<(usize, usize, f64)>(id));
    match prev {
        Some((l, s, at)) if (l, s) == key => now - at,
        _ => {
            ctx.data_mut(|d| d.insert_temp(id, (key.0, key.1, now)));
            0.0
        }
    }
}

/// Card body: lesson dropdown, progress, step text, command chip, navigation.
fn card(ui: &mut egui::Ui, th: &Theme, t: &mut Tour, cat: &Catalog, reg: &Registry) -> Out {
    let mut out = Out::None;
    let lesson = t.lesson();
    let titles: Vec<String> = (0..LESSONS.len()).map(|i| lesson_title(cat, i)).collect();
    let items: Vec<&str> = titles.iter().map(String::as_str).collect();
    let (done, w) = (t.done, ui.available_width());
    let mut pick = t.lesson;
    let tip = cat.t("tutorial.tut.done_tip");
    let mark = |ui: &mut egui::Ui, i: usize| done_mark(ui, th, done & (1 << i) != 0, &tip);
    widgets::dropdown_marked(
        ui,
        th,
        "ddc.help.tour.lesson",
        &mut pick,
        &items,
        Some(w),
        None,
        Some(&mark),
    );
    if pick != t.lesson {
        t.go(pick, 0);
        return out;
    }
    ui.add_space(space::SM);
    let n = lesson.steps.len();
    let label = match t.current() {
        Some(_) => {
            let (k, of) = ((t.step + 1).to_string(), n.to_string());
            cat.tf("tut.step", &[("n", k.as_str()), ("of", of.as_str())])
        }
        None => cat.t("tut.lesson_done"),
    };
    widgets::progress(ui, th, Some(t.step as f32 / n as f32), &label);
    ui.add_space(space::SM);
    match t.current() {
        Some(step) => {
            let text = cat.t(&format!("tut.{}.{}", lesson.id, t.step + 1));
            ui.label(th.rich(Role::BodyStrong, &text));
            if let Some(id) = step.cmd {
                let label = cat.t(&format!("cmd.{id}"));
                let keys = reg
                    .get(id)
                    .and_then(|c| c.shortcuts.first().cloned().or(c.mnemonic.clone()))
                    .unwrap_or_default();
                ui.add_space(space::SM);
                ui.horizontal_wrapped(|ui| {
                    ui.label(th.rich(Role::Caption, &cat.t("tut.press")));
                    if !keys.is_empty() {
                        ui.scope(|ui| widgets::keycap(ui, th, &keys));
                    }
                    if Button::secondary(&label).small().show(ui, th).clicked() {
                        out = Out::Run(id.to_string());
                    }
                });
            }
            ui.add_space(space::SM);
            ui.horizontal(|ui| {
                let (back, skip) = (cat.t("tut.back"), cat.t("tut.skip"));
                if t.step > 0 && Button::quiet(&back).small().show(ui, th).clicked() {
                    t.back();
                }
                if Button::quiet(&skip).small().show(ui, th).clicked() {
                    t.skip();
                }
            });
        }
        None => {
            if t.all_done() {
                ui.label(th.rich(Role::Body, &cat.t("tut.all_done")));
            }
            ui.add_space(space::SM);
            ui.horizontal(|ui| {
                let (next, again) = (cat.t("tut.next"), cat.t("tut.again"));
                if Button::primary(&next).show(ui, th).clicked() {
                    t.next_lesson();
                }
                if t.all_done() {
                    let sample = cat.t("cmd.help.sample");
                    if Button::secondary(&sample).show(ui, th).clicked() {
                        out = Out::Run("help.sample".into());
                    }
                }
                if Button::quiet(&again).small().show(ui, th).clicked() {
                    t.go(t.lesson, 0);
                }
            });
        }
    }
    out
}

/// Muted check in front of a finished lesson; an empty cell of the same size keeps rows aligned.
fn done_mark(ui: &mut egui::Ui, th: &Theme, done: bool, tip: &str) {
    let (r, resp) = ui.allocate_exact_size(egui::Vec2::splat(control::CHECK), egui::Sense::hover());
    if done {
        resp.on_hover_text(tip);
        let at = |x: f32, y: f32| r.min + egui::vec2(x * r.width(), y * r.height());
        ui.painter().add(egui::Shape::line(
            vec![at(0.18, 0.55), at(0.42, 0.78), at(0.84, 0.28)],
            egui::Stroke::new(stroke::INDICATOR, th.sem().text_secondary),
        ));
    }
}

fn lesson_title(cat: &Catalog, i: usize) -> String {
    format!("{}. {}", i + 1, cat.t(&format!("tut.{}.title", LESSONS[i].id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{LayerSpec, OpeningSpec, WallSpec};

    /// A synthetic model: a level, a wall type, `walls` walls on a 10 × 10 perimeter, `doors` doors.
    fn model(walls: usize, doors: usize) -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "W",
                &[LayerSpec {
                    material: "masonry".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let c = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let mut ids = vec![];
        for i in 0..walls {
            let (a, b) = (c[i % 4], c[(i + 1) % 4]);
            let off = (i / 4) as f64 * 20.0;
            ids.push(
                m.add_wall(&WallSpec {
                    a: (a.0 + off, a.1),
                    b: (b.0 + off, b.1),
                    level: l,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: t,
                    offset0: None,
                })
                .unwrap(),
            );
        }
        for k in 0..doors {
            let o = OpeningSpec {
                along: 2.0 + 2.0 * k as f64,
                width: 0.9,
                sill: 0.0,
                height: 2.1,
                kind: "door".into(),
            };
            m.add_opening(ids[0], &o).unwrap();
        }
        (m, l)
    }

    fn snap(m: &Model, l: i64) -> Snap {
        Snap {
            n: Snap::counts(m, l),
            ..Default::default()
        }
    }

    #[test]
    fn counts_from_model() {
        let (m, l) = model(4, 2);
        let s = snap(&m, l);
        assert_eq!(s.get(Field::Walls), 4);
        assert_eq!(s.get(Field::Doors), 2);
        assert_eq!(s.get(Field::Windows), 0);
        assert_eq!(s.get(Field::Levels), 1);
        assert_eq!(s.get(Field::Tags), 0);
    }

    #[test]
    fn four_walls_step() {
        let (m0, l0) = model(1, 0);
        let (m1, l1) = model(4, 0);
        let (m2, l2) = model(5, 1);
        let base = snap(&m0, l0);
        // 1 to 4 walls: a gain of 3, the "draw 4 walls" step is not counted
        assert!(!done(Check::More(Field::Walls, 4), &base, &snap(&m1, l1)));
        assert!(done(Check::More(Field::Walls, 4), &base, &snap(&m2, l2)));
        assert!(done(Check::More(Field::Doors, 1), &base, &snap(&m2, l2)));
        assert!(done(
            Check::Fewer(Field::Walls, 1),
            &snap(&m2, l2),
            &snap(&m1, l1)
        ));
        assert!(!done(
            Check::Fewer(Field::Walls, 1),
            &snap(&m1, l1),
            &snap(&m2, l2)
        ));
    }

    #[test]
    fn ui_state_checks() {
        let base = Snap::default();
        let mut now = Snap::default();
        assert!(done(Check::Idle, &base, &now));
        assert!(!done(Check::Active("arch.wall"), &base, &now));
        now.active = Some("arch.wall".into());
        now.sel = Some("wall".into());
        now.view3d = true;
        now.palette = true;
        for c in [
            Check::Active("arch.wall"),
            Check::Selected("wall"),
            Check::View3d(true),
            Check::PaletteOpen,
        ] {
            assert!(done(c, &base, &now), "{c:?}");
        }
        assert!(!done(Check::Idle, &base, &now));
        assert!(!done(Check::Selected("room"), &base, &now));
    }

    #[test]
    fn tour_walks_the_walls_lesson() {
        let mut t = Tour::default();
        t.go(1, 0);
        let mut now = Snap::default();
        assert!(!t.tick(&now));
        now.active = Some("arch.wall".into());
        assert!(t.tick(&now));
        assert_eq!(t.step, 1);
        // the step base is the snapshot at entry; four new walls count the step
        now.set(Field::Walls, 2);
        assert!(!t.tick(&now));
        now.set(Field::Walls, 5);
        assert!(!t.tick(&now));
        now.set(Field::Walls, 6);
        assert!(t.tick(&now));
        // the "Esc" base was taken with an active tool
        assert!(!t.tick(&now));
        now.active = None;
        assert!(t.tick(&now));
        assert!(!t.tick(&now));
        now.sel = Some("wall".into());
        assert!(t.tick(&now));
        assert!(t.finished());
        assert_eq!(t.done, 1 << 1);
        t.next_lesson();
        assert_eq!((t.lesson, t.step), (2, 0));
    }

    #[test]
    fn tour_walks_the_xlsx_lesson() {
        let mut t = Tour::default();
        t.go(6, 0);
        let mut now = Snap::default();
        assert!(!t.tick(&now));
        // an edit before the export does not count the step
        now.xlsx[1] = 1;
        assert!(!t.tick(&now));
        now.xlsx[0] = 1;
        assert!(t.tick(&now));
        now.xlsx[1] = 2;
        assert!(!t.tick(&now));
        now.xlsx[1] = 3;
        assert!(t.tick(&now));
        assert!(!t.tick(&now));
        now.xlsx[2] = 1;
        assert!(t.tick(&now));
        assert!(t.finished());
        assert_eq!(t.done, 1 << 6);
    }

    #[test]
    fn already_true_condition_needs_a_fresh_action() {
        let mut t = Tour::default();
        t.go(0, 1); // "open 3D"
        let mut now = Snap {
            view3d: true,
            ..Default::default()
        };
        assert!(!t.tick(&now), "3D was already open before the step");
        now.view3d = false;
        assert!(!t.tick(&now));
        now.view3d = true;
        assert!(t.tick(&now));
    }

    #[test]
    fn progress_roundtrip_and_resume() {
        let mut t = Tour::default();
        t.go(3, 2);
        t.done = 0b101;
        let s = encode(&t);
        assert_eq!(decode(&s), Some((3, 2, 0b101)));
        assert_eq!(
            decode("99 99 255"),
            Some((6, LESSONS[6].steps.len(), 0b111_1111))
        );
        assert_eq!(decode("x"), None);
        assert_eq!(decode(""), None);
    }

    #[test]
    fn lessons_have_strings_and_commands() {
        let reg = Registry::builtin();
        let cats = [
            Catalog::new(ddc_ui::Lang::Ru),
            Catalog::new(ddc_ui::Lang::En),
        ];
        assert_eq!(LESSONS.len(), 7);
        let xlsx = LESSONS.iter().find(|l| l.id == "spreadsheet").unwrap();
        assert_eq!(xlsx.steps[0].cmd, Some("data.export_xlsx"));
        assert!(reg.get("data.export_xlsx").is_some());
        for c in &cats {
            for k in [
                "tut.title",
                "tut.step",
                "tut.press",
                "tut.back",
                "tut.skip",
                "tut.next",
                "tut.again",
                "tut.lesson_done",
                "tut.all_done",
                "tut.canvas",
            ] {
                assert_ne!(c.t_in(c.lang, k), k, "{k}");
            }
        }
        for l in &LESSONS {
            for c in &cats {
                let k = format!("tut.{}.title", l.id);
                assert_ne!(c.t_in(c.lang, &k), k);
                for i in 1..=l.steps.len() {
                    let k = format!("tut.{}.{i}", l.id);
                    assert_ne!(c.t_in(c.lang, &k), k);
                }
            }
            for s in l.steps {
                if let Some(id) = s.cmd {
                    assert!(reg.get(id).is_some(), "{id}");
                }
                if let Check::Active(id) = s.check {
                    assert!(reg.get(id).is_some(), "{id}");
                }
            }
        }
    }

    /// One frame of `show` at `time`; with `ring` the ribbon button of the step is "drawn".
    /// Returns the repaint delay the frame asked for and the number of painted shapes.
    fn frame(
        ctx: &egui::Context,
        time: f64,
        t: &mut Tour,
        cat: &Catalog,
        ring: bool,
    ) -> (std::time::Duration, usize) {
        let reg = Registry::builtin();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            time: Some(time),
            ..Default::default()
        };
        let out = ctx.run(input, |ctx| {
            if ring {
                if let Some(id) = t.current().and_then(|s| s.cmd) {
                    let r = egui::Rect::from_min_size(
                        egui::pos2(200.0, 60.0),
                        egui::vec2(64.0, 56.0),
                    );
                    let pass = ctx.cumulative_pass_nr();
                    ctx.data_mut(|d| d.insert_temp(egui::Id::new(("ribbon.rect", id)), (r, pass)));
                }
            }
            let _ = show(ctx, t, cat, &reg);
        });
        let delay = out.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        (delay, out.shapes.len())
    }

    fn open_tour() -> Tour {
        Tour {
            open: true,
            ..Default::default()
        }
    }

    #[test]
    fn idle_card_requests_no_repaint() {
        let cat = Catalog::new(ddc_ui::Lang::En);
        let ctx = ddc_ui::theme::headless_ctx();
        let mut t = open_tour();
        assert!(t.current().and_then(|s| s.cmd).is_some());
        // the step stays unchanged: after the pulse nothing asks for another frame
        let mut last = std::time::Duration::ZERO;
        for k in 0..6 {
            last = frame(&ctx, 2.0 * k as f64, &mut t, &cat, true).0;
        }
        assert!(last >= std::time::Duration::from_secs(1), "{last:?}");
    }

    #[test]
    fn pulse_requests_a_bounded_repaint_then_stops() {
        let cat = Catalog::new(ddc_ui::Lang::En);
        let ctx = ddc_ui::theme::headless_ctx();
        let mut t = open_tour();
        let (d0, _) = frame(&ctx, 0.0, &mut t, &cat, true);
        assert!(d0 <= std::time::Duration::from_millis(50), "{d0:?}");
        let (d1, _) = frame(&ctx, 0.4, &mut t, &cat, true);
        assert!(d1 <= std::time::Duration::from_millis(50), "{d1:?}");
        // a step change restarts the pulse
        t.skip();
        let (d2, _) = frame(&ctx, 10.0, &mut t, &cat, true);
        assert!(d2 <= std::time::Duration::from_millis(50), "{d2:?}");
        let (d3, _) = frame(&ctx, 20.0, &mut t, &cat, true);
        assert!(d3 >= std::time::Duration::from_secs(1), "{d3:?}");
    }

    #[test]
    fn pulse_is_short_and_absent_with_reduced_motion() {
        assert_eq!(pulse_k(0.0, false), Some(0.0));
        assert!(pulse_k(PULSE_S * 0.5, false).is_some());
        assert_eq!(pulse_k(PULSE_S, false), None);
        assert_eq!(pulse_k(30.0, false), None);
        assert_eq!(pulse_k(-1.0, false), None);
        assert_eq!(pulse_k(0.1, true), None);
        let ctx = ddc_ui::theme::headless_ctx();
        assert_eq!(step_age(&ctx, (0, 0), 5.0), 0.0);
        assert!((step_age(&ctx, (0, 0), 5.5) - 0.5).abs() < 1e-9);
        assert_eq!(step_age(&ctx, (0, 1), 6.0), 0.0);
    }

    #[test]
    fn step_navigation() {
        let mut t = Tour::default();
        t.back();
        assert_eq!((t.lesson, t.step), (0, 0));
        t.skip();
        assert_eq!(t.step, 1);
        t.back();
        assert_eq!(t.step, 0);
        let n = t.lesson().steps.len();
        for _ in 0..n {
            t.skip();
        }
        assert!(t.finished());
        assert_eq!(t.done, 1);
        t.skip();
        assert_eq!(t.step, n);
        t.next_lesson();
        assert_eq!((t.lesson, t.step), (1, 0));
        t.go(0, 0);
        assert!(!t.finished());
    }

    #[test]
    fn every_step_renders_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = Catalog::new(lang);
            let ctx = ddc_ui::theme::headless_ctx();
            let mut t = open_tour();
            for l in 0..LESSONS.len() {
                for s in 0..=LESSONS[l].steps.len() {
                    t.lesson = l;
                    t.step = s;
                    t.done = if s == LESSONS[l].steps.len() { 0x7f } else { 1 };
                    let (_, shapes) = frame(&ctx, 3.0 * (l * 10 + s) as f64, &mut t, &cat, true);
                    assert!(shapes > 0, "{l}.{s}");
                }
            }
        }
        let cyr = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c));
        let en = Catalog::new(ddc_ui::Lang::En);
        let key = "tutorial.tut.done_tip";
        assert_ne!(en.t_in(ddc_ui::Lang::En, key), key);
        assert!(cyr(&en.t_in(ddc_ui::Lang::Ru, key)));
        assert!(!en.t_in(ddc_ui::Lang::Ru, key).contains('\u{fffd}'));
    }

    #[test]
    fn the_card_is_built_from_kit_parts_only() {
        let src = include_str!("tutorial.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        let from = code.find("pub fn show(").unwrap();
        let to = code.find("fn lesson_title(").unwrap();
        let body = &code[from..to];
        for raw in [
            "egui::ComboBox",
            "ProgressBar",
            "ui.button(",
            "small_button(",
            "RichText",
            "Color32::",
            "visuals.selection",
            "from_millis(50)",
            ".code()",
            ".strong()",
            "selectable_value(",
            "from_rgb",
        ] {
            assert!(!body.contains(raw), "{raw}");
        }
        for kit in ["dropdown_marked(", "widgets::progress(", "Button::primary(", "Button::quiet(", "widgets::keycap("] {
            assert!(body.contains(kit), "{kit}");
        }
    }
}
