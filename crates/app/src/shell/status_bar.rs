// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Status bar: one strip in three divided zones. Left: recent commands, drawing aids (grid,
//! snap, ortho) and the cursor in project units. Next to it, left-aligned: the step hint as keycap
//! chips. Right: running task, view state (level while the option bar does not show it, style,
//! performance, pick filter), then attention (warnings, messages, save). `fit` decides what a
//! narrow window drops: the hint, then the cursor, then right-zone labels (icons and tooltips
//! stay).

use super::{faults, hint_key, tasks_ui, EditorApp};
use crate::gfx_ui::NoteAct;
use crate::quick::overlay::Recent;
use crate::units::{self, Units};
use ddc_render::graphics::{PerfLevel, Tier};
use ddc_render::VisualStyle;
use ddc_ui::theme::{control, icon_size, space, stroke, Role};
use ddc_ui::{widgets, Catalog, Lang, Theme};
use egui::{self, Color32, Galley, Rect, Sense, Stroke, Vec2};
use std::sync::Arc;

/// Height of the strip: one tab row.
/// One line, 28 px (design spec 2.27); items are 24 px quiet buttons.
pub const BAR_H: f32 = 28.0;
const BTN: f32 = space::XL;
const ICON: f32 = icon_size::SM;
/// Icon stroke at 16 px keeps the 1.5-on-20 grid weight of the shared set.
const ICON_STROKE: f32 = 1.5 * ICON / icon_size::MD;
const CHEV: f32 = space::SM;
const PAD: f32 = space::SM;
const ITEM_GAP: f32 = space::SM;
const ZONE_GAP: f32 = space::LG;
const TOGGLE_GAP: f32 = space::XXS;
const HINT_MIN: f32 = control::DROPDOWN_W;
const LABEL_MAX: f32 = control::DROPDOWN_W;
const TASK_W: f32 = control::DROPDOWN_W * 2.0;
const MSGS_H: f32 = control::DROPDOWN_W * 2.0;
const MSG_TIME_W: f32 = space::XXXL;
const MENU_W: f32 = control::DROPDOWN_W + space::XXXL;

/// Drawing aids, recent commands, frame rate and the save clock of the bar.
pub struct State {
    pub grid: bool,
    pub snap: bool,
    pub ortho: bool,
    pub recent: Recent,
    fps: Fps,
    /// (project, saved head) seen last frame: a moved head on the same project is a save.
    mark: Option<(u64, u64)>,
    saved_at: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            grid: true,
            snap: true,
            ortho: false,
            recent: Recent::default(),
            fps: Fps::default(),
            mark: None,
            saved_at: None,
        }
    }
}

// ---------------------------------------------------------------- layout (pure)

/// Width of a right-zone control with its label and without it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    pub full: f32,
    pub compact: f32,
}

/// What the zones ask for, in points.
#[derive(Clone, Debug)]
pub struct Need {
    /// Recent button, toggles and typed input: never dropped.
    pub left_fixed: f32,
    /// Cursor readout; 0 — none.
    pub coords: f32,
    /// Whole hint on one line; 0 — no hint.
    pub hint: f32,
    pub right: Vec<Slot>,
}

/// What fits: the hint width (0 - hidden), whether the cursor stays, how many right-zone
/// controls (counted from the right) show an icon only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub coords: bool,
    /// Every right-zone label is shown.
    pub labels: bool,
    /// The last `compacted` right-zone controls lost their label.
    pub compacted: usize,
    /// Number of right-zone controls.
    pub items: usize,
    /// The first `hidden` right-zone controls live in the overflow menu (its button is part of
    /// `right`); the last control never does.
    pub hidden: usize,
    pub hint: f32,
    pub left: f32,
    pub right: f32,
}

impl Fit {
    /// Everything shown, gaps included.
    pub fn used(&self) -> f32 {
        let gaps = if self.hint > 0.0 { 2.0 } else { 1.0 } * ZONE_GAP;
        2.0 * PAD + self.left + self.right + self.hint + gaps
    }

    /// Whether the right-zone control at `i` keeps its label.
    pub fn label_at(&self, i: usize) -> bool {
        i + self.compacted < self.items
    }
}

/// Left edge of the hint: right after the left zone, never centred across a gap.
pub fn hint_x(left_end: f32) -> f32 {
    left_end + ZONE_GAP
}

fn row_w(ws: impl Iterator<Item = f32>) -> f32 {
    let (sum, n) = ws.fold((0.0, 0), |(s, n), w| (s + w, n + 1));
    sum + ITEM_GAP * (n.max(1) - 1) as f32
}

/// Zones for a bar `width` wide. Drop order: the hint shrinks, then it goes, then the cursor,
/// and only then right-zone labels one by one from the right (every control keeps its
/// tooltip). Nothing overlaps and a wider bar never shows less.
pub fn fit(width: f32, n: &Need) -> Fit {
    let inner = width - 2.0 * PAD;
    let m = n.right.len();
    let left = |coords: bool| {
        n.left_fixed
            + if coords && n.coords > 0.0 {
                ZONE_GAP + n.coords
            } else {
                0.0
            }
    };
    let right = |k: usize| {
        row_w(
            n.right
                .iter()
                .enumerate()
                .map(|(i, s)| if i + k < m { s.full } else { s.compact }),
        )
    };
    let at = |coords, k: usize, hint| Fit {
        coords,
        labels: k == 0,
        compacted: k,
        items: m,
        hidden: 0,
        hint,
        left: left(coords),
        right: right(k),
    };
    let gaps = if n.hint > 0.0 { 2.0 } else { 1.0 } * ZONE_GAP;
    let free = inner - left(true) - right(0) - gaps;
    if free >= n.hint {
        return at(true, 0, n.hint.max(0.0));
    }
    if n.hint > 0.0 && free >= HINT_MIN.min(n.hint) {
        return at(true, 0, free.min(n.hint));
    }
    if left(true) + right(0) + ZONE_GAP <= inner {
        return at(true, 0, 0.0);
    }
    for k in 0..=m {
        if left(false) + right(k) + ZONE_GAP <= inner {
            return at(false, k, 0.0);
        }
    }
    // still too wide with icons only: leading controls move into the overflow menu
    for h in 1..m.saturating_sub(1) {
        let r = BTN + ITEM_GAP + row_w(n.right[h..].iter().map(|s| s.compact));
        if left(false) + r + ZONE_GAP <= inner {
            return Fit { hidden: h, right: r, ..at(false, m, 0.0) };
        }
    }
    if m < 2 {
        return at(false, m, 0.0);
    }
    let r = BTN + ITEM_GAP + n.right[m - 1].compact;
    Fit { hidden: m - 1, right: r, ..at(false, m, 0.0) }
}

// ---------------------------------------------------------------- formatting (pure)

/// One coordinate in project units with group separators: «12,346» / «12 346» (mm), «1,250'-6"».
pub fn fmt_coord(m: f64, u: Units, lang: Lang, den: usize) -> String {
    let sep = if lang == Lang::Ru { '\u{a0}' } else { ',' };
    match u {
        Units::Metric if lang == Lang::Ru => units::fmt_num_ru(m * 1000.0, 0),
        Units::Metric => units::fmt_num_en(m * 1000.0, 0),
        Units::Imperial => {
            let s = units::format_length_den(m, u, den);
            let Some((ft, rest)) = s.split_once('\'') else {
                return s;
            };
            let (sign, digits) = ft.strip_prefix('-').map_or(("", ft), |d| ("-", d));
            format!("{sign}{}'{rest}", group(digits, sep))
        }
    }
}

fn group(digits: &str, sep: char) -> String {
    let mut g = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            g.push(sep);
        }
        g.push(c);
    }
    g
}

/// X, Y, Z of the cursor.
pub fn fmt_xyz(p: (f64, f64, f64), u: Units, lang: Lang, den: usize) -> [String; 3] {
    [p.0, p.1, p.2].map(|v| fmt_coord(v, u, lang, den))
}

#[derive(Clone, Debug, PartialEq)]
pub enum SaveState {
    /// A project save is running.
    Saving,
    Unsaved,
    /// Saved in this session (time) or opened from its file as is.
    Saved(Option<String>),
    /// Nothing to save yet: a new project without edits.
    Fresh,
}

pub fn save_state(saving: bool, dirty: bool, has_file: bool, at: Option<&str>) -> SaveState {
    match (saving, dirty, at) {
        (true, _, _) => SaveState::Saving,
        (_, true, _) => SaveState::Unsaved,
        (_, _, Some(t)) => SaveState::Saved(Some(t.to_string())),
        _ if has_file => SaveState::Saved(None),
        _ => SaveState::Fresh,
    }
}

pub fn save_text(c: &Catalog, s: &SaveState) -> String {
    match s {
        SaveState::Saving => c.t("sb.save.saving"),
        SaveState::Unsaved => c.t("sb.save.unsaved"),
        SaveState::Saved(Some(t)) => c.tf("sb.save.saved_at", &[("time", t)]),
        SaveState::Saved(None) => c.t("sb.save.saved"),
        SaveState::Fresh => c.t("sb.save.fresh"),
    }
}

/// A save mark that moved on the same project: the project was just saved.
pub fn saved_now(prev: Option<(u64, u64)>, now: (u64, u64)) -> bool {
    prev.is_some_and(|p| p.0 == now.0 && p.1 != now.1)
}

/// Piece of a step hint: plain text or keys with what they do.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Text(String),
    Key { keys: String, desc: String },
}

const KEY_NAMES: &[&str] = &[
    "Ctrl",
    "Shift",
    "Alt",
    "Cmd",
    "Esc",
    "Tab",
    "Enter",
    "Space",
    "Del",
    "Delete",
    "Backspace",
    "Home",
    "End",
    "PgUp",
    "PgDn",
    "PageUp",
    "PageDown",
    "Ins",
    "\u{2191}",
    "\u{2193}",
    "\u{2190}",
    "\u{2192}",
];

fn is_key(t: &str) -> bool {
    let fkey = t.len() > 1
        && t.starts_with('F')
        && t[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n));
    let single = t.chars().count() == 1
        && t.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    KEY_NAMES.contains(&t) || fkey || single
}

fn is_keys(t: &str) -> bool {
    !t.is_empty() && t.split('+').all(is_key)
}

/// «Wall: pick a point · Tab snap · Esc — cancel» → text, then keycap chips with their action.
pub fn parse_hint(s: &str) -> Vec<Part> {
    let mut out = Vec::new();
    for (i, seg) in s.split(" \u{b7} ").map(str::trim).enumerate() {
        if seg.is_empty() {
            continue;
        }
        let key = (i > 0)
            .then(|| seg.split_once(" \u{2014} ").or_else(|| seg.split_once(' ')))
            .flatten()
            .filter(|(k, _)| is_keys(k));
        out.push(match key {
            Some((k, d)) => Part::Key {
                keys: k.to_string(),
                desc: d.trim().to_string(),
            },
            None => Part::Text(seg.to_string()),
        });
    }
    out
}

/// Command behind each visual style (`view.style_*`).
pub fn style_cmd(s: VisualStyle) -> &'static str {
    match s {
        VisualStyle::Wireframe => "view.style_wireframe",
        VisualStyle::HiddenLine => "view.style_hidden",
        VisualStyle::Shaded => "view.style_shaded_plain",
        VisualStyle::ShadedEdges => "view.style_shaded",
        VisualStyle::Realistic => "view.style_realistic",
        VisualStyle::Consistent => "view.style_consistent",
        VisualStyle::XRay => "view.style_xray",
    }
}

pub fn perf_cmd(l: PerfLevel) -> &'static str {
    match l {
        PerfLevel::Auto => "view.gfx_auto",
        PerfLevel::Speed => "view.gfx_speed",
        PerfLevel::Balanced => "view.gfx_balanced",
        PerfLevel::Quality => "view.gfx_quality",
    }
}

fn perf_icon(l: PerfLevel) -> &'static str {
    match l {
        PerfLevel::Auto => "gfx-auto",
        PerfLevel::Speed => "gfx-speed",
        PerfLevel::Balanced => "gfx-balanced",
        PerfLevel::Quality => "gfx-quality",
    }
}

fn tier_key(t: Tier) -> &'static str {
    match t {
        Tier::Speed => "speed",
        Tier::Balanced => "balanced",
        Tier::Quality => "quality",
    }
}

/// How long the frame rate stays after the last drawn frame, seconds.
const HOLD_S: f64 = 1.5;

/// Frame rate of the 3D view while it draws; silent at rest.
#[derive(Default)]
struct Fps {
    ema: f32,
    last: Option<f64>,
}

impl Fps {
    fn push(&mut self, live: bool, dt: f32, now: f64) {
        // a gap of a quarter second is a pause, not a frame
        if !live || !(dt > 0.0 && dt < 0.25) {
            return;
        }
        let f = 1.0 / dt;
        self.ema = if self.ema > 0.0 {
            self.ema * 0.9 + f * 0.1
        } else {
            f
        };
        self.last = Some(now);
    }

    fn show(&self, now: f64) -> Option<u32> {
        self.last
            .filter(|t| now - t < HOLD_S)
            .map(|_| self.ema.round() as u32)
    }

    /// Seconds until the readout goes silent: the one frame that clears it, no polling.
    fn left(&self, now: f64) -> Option<f64> {
        self.last.map(|t| HOLD_S - (now - t)).filter(|s| *s > 0.0)
    }
}

// ---------------------------------------------------------------- commands

fn h_grid(a: &mut EditorApp) {
    a.sbar.grid = !a.sbar.grid;
    sync(a);
}

pub(crate) fn h_snap(a: &mut EditorApp) {
    // the one switch: the editor's snap state; both views read it through `sync`
    a.ed.tool.snap_state.off = snap_on(a);
    sync(a);
}

fn h_ortho(a: &mut EditorApp) {
    a.sbar.ortho = !a.sbar.ortho;
    sync(a);
}

/// Full plan axes on or off; the choice is a preference, kept for the next session.
pub(crate) fn h_axes(a: &mut EditorApp) {
    a.prefs.full_axes = !a.prefs.full_axes;
    a.ed.axes_pref = a.prefs.full_axes;
    a.prefs.save();
    let key = if a.prefs.full_axes { "status.axes_full.on" } else { "status.axes_full.off" };
    a.ed.status = Some(crate::tr(key, &[]));
}

fn snap_on(a: &EditorApp) -> bool {
    !a.ed.tool.snap_state.off
}

/// Drawing aids into the plan tool (a project switch replaces the editor).
pub fn sync(a: &mut EditorApp) {
    let t = &mut a.ed.tool;
    t.grid_on = a.sbar.grid;
    t.snap_on = !t.snap_state.off;
    t.ortho = a.sbar.ortho;
    a.sbar.snap = !a.ed.tool.snap_state.off;
    a.view.e3.snaps_off = a.ed.tool.snap_state.off;
    // 3D filters by the same family profile as the plan
    a.view.e3.profile = if a.view3d { a.view.e3.tool_profile() } else { a.ed.tool.profile };
    // the elevation of the level (plan) or of the work plane (3D) lifts every answer
    a.ed.tool.snap_state.z = if a.view3d { f64::from(a.view.e3.plane.origin.z) } else { a.ed.elevation };
}

/// Commands of this module (`commands.d/sb2.json`).
pub fn register(a: &mut EditorApp) {
    let list: &[(&str, super::Handler)] = &[
        ("view.grid_toggle", h_grid),
        ("view.snap_toggle", h_snap),
        ("view.ortho_toggle", h_ortho),
        (crate::plan_hud::FULL_AXES_CMD, h_axes),
    ];
    for (id, h) in list {
        a.register_command(id, *h);
    }
}

// ---------------------------------------------------------------- frame

enum Act {
    Run(&'static str),
    Level(i64),
    Note(NoteAct),
    Task(tasks_ui::PillAct),
    Fault(faults::Tone),
    PickOff(std::collections::BTreeSet<&'static str>),
    Pinned(bool),
    MsgsSeen,
    MsgsClear,
}

/// Notification center as the bar shows it: counts and the newest lines.
#[derive(Default)]
struct Msgs {
    total: usize,
    unseen: ddc_ui::notify::Unseen,
    /// Newest first: level, local time, text.
    rows: Vec<(ddc_ui::notify::Level, String, String)>,
}

/// Lines the messages popover lists.
const MSGS_SHOWN: usize = 50;

/// Opens the messages popover (the toast stack's "+N"); same id as the bar item's popup.
pub(crate) fn open_messages(ctx: &egui::Context) {
    // Popup::default_response_id(resp) is resp.id.with("popup")
    let id = egui::Id::new(("ddc.sb", "msgs")).with("popup");
    egui::Popup::open_id(ctx, id);
}

fn msgs_view(t: &ddc_ui::notify::Toasts) -> Msgs {
    Msgs {
        total: t.history.len(),
        unseen: t.unseen(),
        rows: t
            .recent(MSGS_SHOWN)
            .into_iter()
            .map(|(l, at, text)| {
                let time = at.map(super::recover_win::fmt_hhmm).unwrap_or_default();
                (l, time, text.to_string())
            })
            .collect(),
    }
}

/// Everything the bar shows this frame, read before painting.
struct View {
    view3d: bool,
    grid: bool,
    snap: bool,
    ortho: bool,
    xyz: Option<[String; 3]>,
    template: [String; 3],
    unit: String,
    echo: String,
    hint: Option<String>,
    level_id: i64,
    level: String,
    /// The option bar shows the level combo now, so the bar does not repeat it.
    opt_level: bool,
    levels: Vec<(i64, String, String)>,
    style: VisualStyle,
    perf: PerfLevel,
    tier: Tier,
    fps: Option<u32>,
    notes: Vec<String>,
    note: Option<(String, String, NoteAct)>,
    /// The software-drawing chip: label, tooltip, click still acts (`gfx_ui::soft_chip`).
    soft: Option<(String, String, bool)>,
    gpu: String,
    /// Model faults (errors and warnings), same list as the fault window.
    faults: faults::Counts,
    pick: crate::pick_rules::PickFilter,
    /// "Select pinned" (`sel_flags[1]`); off makes picks skip pinned items.
    pinned: bool,
    msgs: Msgs,
    save: SaveState,
    autosave: (String, bool),
    autosave_why: Option<String>,
    ifc: Option<String>,
    task: Option<crate::jobs::JobView>,
    /// The task card is open on the canvas: the bar shows only a small indicator.
    task_card: bool,
    repeat: Option<String>,
    keys: std::collections::HashMap<&'static str, String>,
}

const KEYED: &[&str] = &[
    "app.repeat",
    "view.grid_toggle",
    "view.snap_toggle",
    "view.ortho_toggle",
    "view.level_up",
    "view.level_down",
    "view.warnings",
    "file.save",
    "file.save_ifc",
    "view.style_wireframe",
    "view.style_hidden",
    "view.style_shaded_plain",
    "view.style_shaded",
    "view.style_realistic",
    "view.style_consistent",
    "view.style_xray",
];

fn gather(a: &mut EditorApp, now: f64) -> View {
    let c = &a.sh.cat;
    let ed = &a.ed;
    let u = ed.tool.units;
    let lang = c.lang;
    let den = units::frac_denominator();
    let cur = if a.view3d {
        a.view
            .cursor
            .map(|p| (f64::from(p.x), f64::from(p.y), f64::from(p.z)))
    } else {
        a.plan.cursor.map(|p| (p.x, p.y, ed.elevation))
    };
    let hint = a
        .hint
        .take()
        .or_else(|| a.plan.b.hint(&a.ed))
        .or_else(|| a.tool_hint().map(|(t, s)| (t.to_string(), s.to_string())))
        .map(|(t, s)| hint_key(&t, &s))
        .map(|k| (c.t(&k), k))
        .filter(|(txt, k)| txt != k)
        .map(|(txt, _)| txt);
    let echo = if !ed.wall_draft().input.is_empty() {
        c.tf("status.input", &[("v", &ed.wall_draft().input)])
    } else {
        a.sh.echo.clone()
    };
    let (units, ru) = (ed.tool.units, crate::ui_is_ru());
    let levels = ed
        .levels()
        .into_iter()
        .rev()
        .map(|(id, n, z)| (id, n, crate::editor::levels::elevation_mark(z, units, ru)))
        .collect();
    let notes = if a.view3d {
        crate::view3d::perf_overlay::status_lines(&*a, c)
    } else {
        vec![]
    };
    let saving = a.project_ui.saving();
    let mark = (a.ed.project_id(), a.ed.saved_head());
    if a.sbar.mark.is_some_and(|m| m.0 != mark.0) {
        a.sbar.saved_at = None;
    }
    if saved_now(a.sbar.mark, mark) {
        a.sbar.saved_at = Some(super::recover_win::fmt_hhmm(web_time::SystemTime::now()));
    }
    a.sbar.mark = Some(mark);
    let has_file = a.ed.project.path().is_some();
    let save = save_state(
        saving,
        a.ed.is_dirty(),
        has_file,
        a.sbar.saved_at.as_deref(),
    );
    let repeat = ddc_ui::frecency::with(|u| u.last_repeatable(&a.sh.reg).map(String::from))
        .map(|id| crate::quick::label(a, &id));
    let keys = KEYED
        .iter()
        .map(|id| (*id, crate::quick::keys_text(a, id)))
        .collect();
    let select_on = !a.ed.tool_armed() && !a.ed.opening_mode;
    let tool_active = !(select_on && a.active_command().is_none());
    // a palette-only layout draws no option bar: the level stays here then
    let opt_bar = a.workspace().surface != ddc_ui::workspace::Surface::PaletteOnly;
    let opt_level = opt_bar
        && matches!(
            super::panels::opt_ctx::bar(tool_active, a.ed.selection.len()),
            super::panels::opt_ctx::Bar::Idle
        );
    let c = &a.sh.cat;
    let ed = &a.ed;
    View {
        opt_level,
        view3d: a.view3d,
        grid: a.sbar.grid,
        snap: snap_on(a),
        ortho: a.sbar.ortho,
        xyz: cur.map(|p| fmt_xyz(p, u, lang, den)),
        template: fmt_xyz((-888.888, -888.888, -88.888), u, lang, den),
        unit: if u == Units::Metric {
            c.t("sb.unit.mm")
        } else {
            String::new()
        },
        echo,
        hint,
        level_id: ed.level,
        level: ed.level_name(),
        levels,
        style: a.view.nav.style,
        perf: a.view.gfx.level,
        tier: a.view.gfx.tier,
        fps: a.sbar.fps.show(now),
        notes,
        note: crate::gfx_ui::status_note(&*a, c),
        soft: crate::gfx_ui::soft_chip(&a.view.gfx, c),
        gpu: a.backend.clone(),
        faults: faults::counts(&a.faults.list),
        pick: a.ed.pick_filter.clone(),
        pinned: a.sel_flags[1],
        msgs: msgs_view(&a.sh.toasts),
        save,
        autosave: a.autosave_label(),
        autosave_why: a.autosave_reason(),
        ifc: crate::save::pill::text(a),
        task: calm_task(a, now),
        task_card: a.chrome.tasks.window_h > 0.0,
        repeat,
        keys,
    }
}

/// The task pill of this frame, steadied by the calm rule.
fn calm_task(a: &mut EditorApp, now: f64) -> Option<crate::jobs::JobView> {
    let live = tasks_ui::current(a);
    let expanded = a.chrome.tasks.expanded;
    let end = crate::jobs::take_end();
    a.chrome.tasks.calm.step(now, live, end, expanded)
}

/// The bar: gather, paint, then act (commands run after painting, never mid-frame).
pub fn show(a: &mut EditorApp, ui: &mut egui::Ui) {
    let (now, dt) = ui.input(|i| (i.time, i.unstable_dt));
    a.sbar.fps.push(a.view3d, dt, now);
    sync(a);
    let v = gather(a, now);
    if let Some(left) = a.sbar.fps.left(now).filter(|_| v.fps.is_some()) {
        // live frames refresh the number themselves; at rest one frame clears it
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(left + 0.02));
    }
    if v.task.is_some() {
        // the done state is held for a moment after the job ends
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
    let sh = &mut a.sh;
    let mut acts = Vec::new();
    let mut hold = false;
    Painter {
        th: &sh.theme,
        c: &sh.cat,
        icons: &mut sh.icons,
        acts: &mut acts,
    }
    .bar(ui, &v, &mut hold);
    a.sbar.recent.hold = hold;
    for act in acts {
        match act {
            Act::Run(id) => {
                a.run(id);
            }
            Act::Level(id) => a.ed.set_level(id),
            Act::Note(n) => crate::gfx_ui::note_clicked(a, n),
            Act::Task(tasks_ui::PillAct::Open) => {
                a.chrome.tasks.expanded = true;
                a.chrome.tasks.hidden = false;
            }
            Act::Task(tasks_ui::PillAct::Cancel) => tasks_ui::cancel(a),
            Act::Fault(t) => a.fault_click(t),
            Act::PickOff(off) => a.ed.pick_filter.off = off,
            Act::Pinned(on) => a.sel_flags[1] = on,
            Act::MsgsSeen => a.sh.toasts.mark_seen(),
            Act::MsgsClear => a.sh.toasts.clear_history(),
            Act::Task(tasks_ui::PillAct::Retry(cmd)) => {
                a.chrome.tasks.calm = Default::default();
                a.run(cmd);
            }
        }
    }
    // "select pinned" off: click and hover skip pinned items
    a.ed.pick_filter.skip_pinned = !a.sel_flags[1];
}

// ---------------------------------------------------------------- painting

struct Painter<'a> {
    th: &'a Theme,
    c: &'a Catalog,
    icons: &'a mut ddc_ui::icons::IconCache,
    acts: &'a mut Vec<Act>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Item {
    /// Drawing on the processor: always shown first, with its label, until the window closes.
    Gpu,
    Task,
    Level,
    Style,
    Perf,
    Pick,
    Warn,
    Messages,
    Save,
}

/// What a right-zone control draws.
struct Chip {
    icon: Option<&'static str>,
    tint: Option<Color32>,
    dot: Option<Color32>,
    label: Option<Arc<Galley>>,
    /// Muted readout after the label in a slot of fixed width (the frame rate).
    extra: Option<(Arc<Galley>, f32)>,
    chevron: bool,
}

impl Chip {
    fn width(&self, labels: bool) -> f32 {
        let mut w = space::XS;
        if self.icon.is_some() {
            w += ICON + space::XS;
        }
        if self.dot.is_some() {
            w += space::SM + space::XS;
        }
        let text = labels || self.icon.is_none() && self.dot.is_none();
        if text {
            if let Some(g) = &self.label {
                w += g.size().x + space::XS;
            }
            if let Some((g, slot)) = &self.extra {
                w += g.size().x.max(*slot) + space::XS;
            }
        }
        if self.chevron {
            w += CHEV + space::XS;
        }
        if text || self.chevron {
            w += space::XS;
        }
        w.max(BTN)
    }
}

/// Up-pointing chevron: menus of a bottom bar open upwards.
fn chevron(p: &egui::Painter, c: egui::Pos2, col: Color32) {
    let (w, h) = (CHEV / 2.0 - space::XXS / 2.0, space::XXS);
    p.add(egui::Shape::line(
        vec![
            c + Vec2::new(-w, h),
            c + Vec2::new(0.0, -h),
            c + Vec2::new(w, h),
        ],
        Stroke::new(ICON_STROKE, col),
    ));
}

/// Hover, pressed, on and disabled fills; returns the foreground.
fn paint_state(ui: &egui::Ui, th: &Theme, r: Rect, resp: &egui::Response, on: bool) -> Color32 {
    let fg = widgets::paint_state(ui, th, r, resp, on);
    if on && ui.is_enabled() && resp.is_pointer_button_down_on() {
        ui.painter()
            .rect_filled(r, th.radius("md"), th.c("bg.pressed"));
    }
    fg
}

fn child(ui: &mut egui::Ui, r: Rect, enabled: bool) -> egui::Ui {
    let mut cu = ui.new_child(egui::UiBuilder::new().max_rect(r));
    if !enabled {
        cu.disable();
    }
    cu
}

impl Painter<'_> {
    fn text(&self, ui: &egui::Ui, s: &str, role: Role, col: &str) -> Arc<Galley> {
        let g = ui
            .painter()
            .layout_no_wrap(s.to_string(), self.th.font(role), self.th.c(col));
        if g.size().x > LABEL_MAX {
            widgets::elide(ui, s, self.th.font(role), self.th.c(col), LABEL_MAX)
        } else {
            g
        }
    }

    fn keys<'k>(&self, v: &'k View, id: &str) -> &'k str {
        v.keys.get(id).map_or("", String::as_str)
    }

    fn bar(&mut self, ui: &mut egui::Ui, v: &View, hold: &mut bool) {
        let th = self.th;
        let c = self.c;
        let full = ui.max_rect();
        ui.painter().hline(
            full.x_range(),
            full.top() + stroke::HAIRLINE / 2.0,
            Stroke::new(stroke::HAIRLINE, th.c("border.subtle")),
        );
        let cy = full.center().y;
        // --- measure
        let echo = (!v.echo.is_empty()).then(|| {
            ui.painter()
                .layout_no_wrap(v.echo.clone(), th.font(Role::Numeric), th.c("accent"))
        });
        let left_fixed = BTN
            + ITEM_GAP
            + 3.0 * BTN
            + 2.0 * TOGGLE_GAP
            + echo.as_ref().map_or(0.0, |g| ITEM_GAP + g.size().x);
        let coords = Coords::measure(ui, th, v);
        let parts = v.hint.as_deref().map(parse_hint).unwrap_or_default();
        let hint_w = HintRow::measure(ui, th, &parts);
        let items = self.right_items(ui, v);
        let need = Need {
            left_fixed,
            coords: coords.width,
            hint: hint_w.total,
            right: items
                .iter()
                .map(|(_, ch)| Slot {
                    full: ch.as_ref().map_or(TASK_W, |c| c.width(true)),
                    compact: ch.as_ref().map_or(TASK_W, |c| c.width(false)),
                })
                .collect(),
        };
        let f = fit(full.width(), &need);
        // --- left
        let mut x = full.left() + PAD;
        let rect =
            |x: f32, w: f32| Rect::from_center_size(egui::pos2(x + w / 2.0, cy), Vec2::new(w, BTN));
        let r = rect(x, BTN);
        let tip_repeat = v.repeat.as_ref().map_or_else(
            || c.t("sb.recent.none"),
            |n| c.tf("sb.recent.repeat", &[("cmd", n)]),
        );
        let resp = self.icon_button(ui, r, "recent", "recent-bar", false, true);
        *hold = resp.hovered() || resp.has_focus();
        let resp = widgets::rich_tooltip(
            resp,
            th,
            &c.t("sb.recent"),
            &tip_repeat,
            self.keys(v, "app.repeat"),
        );
        if resp.clicked() {
            self.acts.push(Act::Run("app.repeat"));
        }
        x += BTN + ITEM_GAP;
        let plan = !v.view3d;
        for (id, icon, key, on, enabled) in [
            ("view.grid_toggle", "plan-grid", "grid", v.grid, plan),
            ("view.snap_toggle", "snaps", "snap", v.snap, true),
            ("view.ortho_toggle", "ortho", "ortho", v.ortho, plan),
        ] {
            let r = rect(x, BTN);
            let resp = self.icon_button(ui, r, key, icon, on, enabled);
            let state = c.t(if on { "sb.on" } else { "sb.off" });
            let desc = if enabled {
                format!("{state} \u{b7} {}", c.t(&format!("sb.{key}.desc")))
            } else {
                c.t("sb.plan_only")
            };
            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            let name = c.t(&format!("sb.{key}"));
            let resp = widgets::rich_tooltip(resp, th, &name, &desc, self.keys(v, id));
            resp.widget_info(|| {
                egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, on, &name)
            });
            if resp.clicked() {
                self.acts.push(Act::Run(id));
            }
            x += BTN + TOGGLE_GAP;
        }
        x += ITEM_GAP - TOGGLE_GAP;
        if f.coords && coords.width > 0.0 {
            x += ZONE_GAP - ITEM_GAP;
            let r = Rect::from_min_size(
                egui::pos2(x, full.top()),
                Vec2::new(coords.width, full.height()),
            );
            coords.paint(ui, th, c, r, v);
            x = r.right() + ITEM_GAP;
        }
        if let Some(g) = echo {
            let pos = egui::pos2(x, cy - g.size().y / 2.0);
            ui.painter().galley(pos, g, th.c("accent"));
        }
        let left_end = full.left() + PAD + f.left;
        // --- right
        let mut x = full.right() - PAD;
        for (i, (item, ch)) in items.iter().enumerate().rev() {
            if i < f.hidden {
                break;
            }
            let labels = f.label_at(i);
            let w = ch.as_ref().map_or(TASK_W, |c| c.width(labels));
            let r = Rect::from_min_max(egui::pos2(x - w, full.top()), egui::pos2(x, full.bottom()));
            if *item == Item::Warn {
                // view state on the left of the divider, attention on the right
                self.divider(ui, full, x - w - ITEM_GAP / 2.0);
            }
            match ch {
                None => self.task(ui, r, v),
                Some(chip) => self.chip(ui, r, *item, chip, labels, v),
            }
            x -= w + ITEM_GAP;
        }
        if f.hidden > 0 {
            let r = Rect::from_center_size(egui::pos2(x - BTN / 2.0, cy), Vec2::splat(BTN));
            self.overflow(ui, r, &items[..f.hidden], v);
        }
        let right_start = full.right() - PAD - f.right;
        // --- centre
        if f.hint > 0.0 {
            let lo = hint_x(left_end);
            let hi = right_start - ZONE_GAP;
            let w = f.hint.min(hi - lo);
            self.divider(ui, full, left_end + ZONE_GAP / 2.0);
            let x0 = lo;
            let r = Rect::from_min_size(egui::pos2(x0, full.top()), Vec2::new(w, full.height()));
            let cut = hint_w.paint(ui, th, r, &parts);
            if cut {
                let resp = ui.interact(r, egui::Id::new("ddc.sb.hint"), Sense::hover());
                if let Some(h) = &v.hint {
                    resp.on_hover_text(h.as_str());
                }
            }
        }
    }

    /// Hairline between two zones, inside the gap that already separates them.
    fn divider(&self, ui: &egui::Ui, full: Rect, x: f32) {
        let ppp = ui.ctx().pixels_per_point();
        ui.painter().vline(
            ddc_ui::px::snap(x, ppp),
            egui::Rangef::new(full.top() + space::XS, full.bottom() - space::XS),
            ddc_ui::px::stroke(ppp, self.th.c("border.subtle")),
        );
    }

    fn icon_button(
        &mut self,
        ui: &mut egui::Ui,
        r: Rect,
        salt: &str,
        icon: &str,
        on: bool,
        enabled: bool,
    ) -> egui::Response {
        let cu = child(ui, r, enabled);
        let resp = cu.interact(r, egui::Id::new(("ddc.sb", salt)), Sense::click());
        let fg = paint_state(&cu, self.th, r, &resp, on);
        let ir = Rect::from_center_size(r.center(), Vec2::splat(ICON));
        self.icons
            .paint(&cu, &widgets::icon_name(icon, on), ICON as u32, ir, fg);
        resp
    }

    fn right_items(&self, ui: &egui::Ui, v: &View) -> Vec<(Item, Option<Chip>)> {
        let th = self.th;
        let c = self.c;
        let mut out = Vec::new();
        if let Some((label, _, live)) = &v.soft {
            out.push((
                Item::Gpu,
                Some(Chip {
                    icon: Some("warning"),
                    tint: Some(th.c("warning")),
                    dot: None,
                    // a warning with its way out reads whole: the cap is for names, and the
                    // bar's fit drops the hint and the cursor before this chip
                    label: Some(ui.painter().layout_no_wrap(
                        label.clone(),
                        th.font(Role::Body),
                        th.c(if *live { "text.primary" } else { "text.secondary" }),
                    )),
                    extra: None,
                    chevron: false,
                }),
            ));
        }
        if v.task.is_some() {
            out.push((Item::Task, None));
        }
        if !v.opt_level {
            out.push((
                Item::Level,
                Some(Chip {
                    icon: Some("level"),
                    tint: None,
                    dot: None,
                    label: Some(self.text(ui, &v.level, Role::Body, "text.secondary")),
                    extra: None,
                    chevron: true,
                }),
            ));
        }
        out.push((
            Item::Style,
            Some(Chip {
                icon: Some(crate::gfx_ui::style_icon(v.style)),
                tint: None,
                dot: None,
                label: Some(self.text(
                    ui,
                    &c.t(&format!("v3d.style.{}", v.style.key())),
                    Role::Body,
                    "text.secondary",
                )),
                extra: None,
                chevron: true,
            }),
        ));
        let warn = v.note.is_some() || !v.notes.is_empty();
        let fps = v.view3d.then(|| {
            let n = v.fps.map_or(String::new(), |n| n.to_string());
            // the slot keeps its width at rest: the readout appears without moving the bar
            let t = c.tf("sb.perf.fps", &[("n", "000")]);
            let g = self.text(ui, &t, Role::Status, "text.secondary");
            (
                g.size().x,
                c.tf("sb.perf.fps", &[("n", &n)]),
                v.fps.is_some(),
            )
        });
        out.push((
            Item::Perf,
            Some(Chip {
                icon: Some(perf_icon(v.perf)),
                tint: warn.then(|| th.c("warning")),
                dot: None,
                label: Some(self.text(
                    ui,
                    &c.t(&format!("sb.perf.{}", v.perf.key())),
                    Role::Body,
                    "text.secondary",
                )),
                extra: fps.map(|(w, t, live)| {
                    let shown = if live { t } else { String::new() };
                    let g = ui.painter().layout_no_wrap(
                        shown,
                        th.font(Role::Status),
                        th.c("text.secondary"),
                    );
                    (g, w)
                }),
                chevron: true,
            }),
        ));
        let (pick_txt, pick_on) = crate::pick_rules::filter_label(c, &v.pick);
        out.push((
            Item::Pick,
            Some(Chip {
                icon: Some("filter"),
                tint: (pick_on || !v.pinned).then(|| th.c("accent")),
                dot: None,
                label: Some(self.text(ui, &pick_txt, Role::Body, "text.secondary")),
                extra: None,
                chevron: true,
            }),
        ));
        out.push((
            Item::Warn,
            Some(Chip {
                icon: Some("warning"),
                tint: Some(if v.faults.errors > 0 {
                    th.c("error")
                } else if v.faults.warnings > 0 {
                    th.c("warning")
                } else {
                    th.c("text.disabled")
                }),
                dot: None,
                label: Some(self.text(
                    ui,
                    &(v.faults.errors + v.faults.warnings).to_string(),
                    Role::Numeric,
                    if v.faults.errors + v.faults.warnings > 0 {
                        "text.primary"
                    } else {
                        "text.disabled"
                    },
                )),
                extra: None,
                chevron: false,
            }),
        ));
        if v.msgs.total > 0 {
            let u = v.msgs.unseen;
            let n = u.errors + u.warnings;
            out.push((
                Item::Messages,
                Some(Chip {
                    icon: Some("issues"),
                    tint: if u.errors > 0 {
                        Some(th.c("error"))
                    } else {
                        (u.warnings > 0).then(|| th.c("warning"))
                    },
                    dot: None,
                    label: (n > 0).then(|| {
                        self.text(ui, &n.to_string(), Role::Numeric, "text.primary")
                    }),
                    extra: None,
                    chevron: false,
                }),
            ));
        }
        let dot = match (&v.save, v.autosave.1) {
            (_, true) => Some(th.c("warning")),
            (SaveState::Unsaved, _) => Some(th.c("accent")),
            (SaveState::Saving, _) => Some(th.c("text.secondary")),
            _ => None,
        };
        let col = if matches!(v.save, SaveState::Unsaved) {
            "text.primary"
        } else {
            "text.secondary"
        };
        out.push((
            Item::Save,
            Some(Chip {
                icon: None,
                tint: None,
                dot,
                label: Some(self.text(ui, &save_text(c, &v.save), Role::Body, col)),
                extra: None,
                chevron: true,
            }),
        ));
        out
    }

    fn task(&mut self, ui: &mut egui::Ui, r: Rect, v: &View) {
        let Some(view) = &v.task else { return };
        let mut cu = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(r)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        cu.set_clip_rect(r.intersect(ui.clip_rect()));
        cu.spacing_mut().item_spacing.x = space::SM;
        let act = if v.task_card && view.end.is_none() {
            tasks_ui::mini(&mut cu, self.th, self.c, view)
        } else {
            tasks_ui::pill(&mut cu, self.th, self.c, view)
        };
        if let Some(a) = act {
            self.acts.push(Act::Task(a));
        }
    }

    /// The "more" button for controls that do not fit: one row per control. A row runs the
    /// action of the control or opens its own menu where this button is.
    fn overflow(
        &mut self,
        ui: &mut egui::Ui,
        r: Rect,
        hidden: &[(Item, Option<Chip>)],
        v: &View,
    ) {
        let (th, c) = (self.th, self.c);
        let cu = child(ui, r, true);
        let resp = cu.interact(r, sb_id("more"), Sense::click());
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
        let fg = paint_state(&cu, th, r, &resp, open);
        let step = space::XS + ICON_STROKE;
        for dx in [-1.0, 0.0, 1.0] {
            let at = egui::pos2(r.center().x + dx * step, r.center().y);
            cu.painter().circle_filled(at, ICON_STROKE, fg);
        }
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        let title = c.t("sb.more.title");
        let resp = widgets::rich_tooltip(resp, th, &title, &c.t("sb.more.desc"), "");
        let mut chosen = None;
        let mut task = None;
        let icons = &mut *self.icons;
        up_menu(&resp, |ui| {
            for (item, ch) in hidden {
                let Some(ch) = ch else {
                    if let Some(job) = &v.task {
                        task = tasks_ui::pill(ui, th, c, job).or(task.take());
                    }
                    continue;
                };
                let (label, trail, enabled) = match item {
                    Item::Level => (c.t("sb.level"), v.level.clone(), !v.levels.is_empty()),
                    Item::Style => (
                        c.t("sb.style"),
                        c.t(&format!("v3d.style.{}", v.style.key())),
                        v.view3d,
                    ),
                    Item::Perf => (
                        c.t("sb.perf"),
                        c.t(&format!("sb.perf.{}", v.perf.key())),
                        true,
                    ),
                    Item::Pick => {
                        let (t, _) = crate::pick_rules::filter_label(c, &v.pick);
                        (t, String::new(), true)
                    }
                    Item::Warn => (warn_title(c, v), String::new(), true),
                    Item::Messages => (c.t("messages.title"), String::new(), true),
                    Item::Save => (save_text(c, &v.save), String::new(), true),
                    // the software-renderer warning: its row retries the graphics card
                    Item::Gpu => match &v.soft {
                        Some((label, _, live)) => (label.clone(), String::new(), *live),
                        None => continue,
                    },
                    Item::Task => continue,
                };
                let id = sb_id(salt_of(*item)).with("row");
                let trail = Trail::Text(&trail);
                let hit = ui
                    .add_enabled_ui(enabled, |ui| {
                        menu_row_at(ui, th, icons, ch.icon, &label, trail, false, Some(id))
                    })
                    .inner;
                if hit.clicked() {
                    chosen = Some(*item);
                }
            }
        });
        if let Some(a) = task {
            self.acts.push(Act::Task(a));
        }
        match chosen {
            Some(Item::Warn) => self.acts.push(warn_act(v)),
            Some(Item::Gpu) => self.acts.push(Act::Note(NoteAct::TryCard)),
            Some(item) => {
                egui::Popup::open_id(ui.ctx(), sb_id(salt_of(item)).with("popup"));
            }
            None => {}
        }
        // an opened control keeps its menu on this button for as long as it is open
        for (item, ch) in hidden {
            let id = sb_id(salt_of(*item));
            if ch.is_some()
                && *item != Item::Warn
                && egui::Popup::is_id_open(ui.ctx(), id.with("popup"))
            {
                let anchor = ui.interact(r, id, Sense::hover());
                self.attach(anchor, *item, v);
            }
        }
    }

    fn chip(
        &mut self,
        ui: &mut egui::Ui,
        slot: Rect,
        item: Item,
        ch: &Chip,
        labels: bool,
        v: &View,
    ) {
        let th = self.th;
        let c = self.c;
        let w = ch.width(labels);
        let r = Rect::from_center_size(slot.center(), Vec2::new(w, BTN));
        let enabled = match item {
            Item::Level => !v.levels.is_empty(),
            Item::Style => v.view3d,
            _ => true,
        };
        let cu = child(ui, r, enabled);
        let resp = cu.interact(r, sb_id(salt_of(item)), Sense::click());
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
        let fg = paint_state(&cu, th, r, &resp, open);
        let p = cu.painter();
        let mut x = r.left() + space::XS;
        let cy = r.center().y;
        if let Some(icon) = ch.icon {
            let ir = Rect::from_min_size(egui::pos2(x, cy - ICON / 2.0), Vec2::splat(ICON));
            let tint = if cu.is_enabled() {
                ch.tint.unwrap_or(fg)
            } else {
                fg
            };
            self.icons.paint(&cu, icon, ICON as u32, ir, tint);
            x += ICON + space::XS;
        }
        if let Some(d) = ch.dot {
            p.circle_filled(
                egui::pos2(x + space::XS, cy),
                space::XS - stroke::HAIRLINE / 2.0,
                d,
            );
            x += space::SM + space::XS;
        }
        let text_shown = labels || ch.icon.is_none() && ch.dot.is_none();
        if text_shown {
            if let Some(g) = &ch.label {
                p.galley(egui::pos2(x, cy - g.size().y / 2.0), g.clone(), fg);
                x += g.size().x + space::XS;
            }
            if let Some((g, slot)) = &ch.extra {
                p.galley(egui::pos2(x, cy - g.size().y / 2.0), g.clone(), fg);
                x += g.size().x.max(*slot) + space::XS;
            }
        }
        if ch.chevron {
            chevron(p, egui::pos2(x + CHEV / 2.0, cy), fg);
        }
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        self.attach(resp, item, v);
    }

    /// Tooltip and menu of a bar item, on its own button or (overflow) on the "more" button.
    fn attach(&mut self, resp: egui::Response, item: Item, v: &View) {
        let (th, c) = (self.th, self.c);
        match item {
            Item::Level => {
                let keys = [
                    self.keys(v, "view.level_up"),
                    self.keys(v, "view.level_down"),
                ]
                .into_iter()
                .filter(|k| !k.is_empty())
                .collect::<Vec<_>>()
                .join(" / ");
                // two chords: named in the text, keycaps would merge them into one
                let mut desc = c.t("sb.level.desc");
                if !keys.is_empty() {
                    desc = format!("{desc}\n{keys}");
                }
                let title = format!("{} \u{b7} {}", c.t("sb.level"), v.level);
                let resp = widgets::rich_tooltip(resp, th, &title, &desc, "");
                self.level_menu(&resp, v);
            }
            Item::Style => {
                let name = c.t(&format!("v3d.style.{}", v.style.key()));
                let desc = if v.view3d {
                    c.t(&format!("v3d.style.tip.{}", v.style.key()))
                } else {
                    c.t("sb.style.plan")
                };
                let title = format!("{} \u{b7} {name}", c.t("sb.style"));
                let resp = widgets::rich_tooltip(resp, th, &title, &desc, "");
                self.style_menu(&resp, v);
            }
            Item::Perf => {
                let mut desc = vec![c.tf(
                    "sb.perf.now",
                    &[("tier", &c.t(&format!("sb.perf.{}", tier_key(v.tier))))],
                )];
                if let Some((t, _, _)) = &v.note {
                    desc.push(t.clone());
                }
                desc.extend(v.notes.iter().cloned());
                if !v.gpu.is_empty() {
                    desc.push(c.tf("sb.perf.gpu", &[("name", &v.gpu)]));
                }
                let title = format!(
                    "{} \u{b7} {}",
                    c.t("sb.perf"),
                    c.t(&format!("sb.perf.{}", v.perf.key()))
                );
                let resp = widgets::rich_tooltip(resp, th, &title, &desc.join("\n"), "");
                self.perf_menu(&resp, v);
            }
            Item::Pick => {
                let (title, _) = crate::pick_rules::filter_label(c, &v.pick);
                let resp = widgets::rich_tooltip(resp, th, &title, &c.t("pick_rules.filter.tip"), "");
                self.pick_menu(&resp, v);
            }
            Item::Warn => {
                let title = warn_title(c, v);
                let resp = widgets::rich_tooltip(
                    resp,
                    th,
                    &title,
                    &c.t("sb.warn.desc"),
                    self.keys(v, "view.warnings"),
                );
                if resp.clicked() {
                    self.acts.push(warn_act(v));
                }
            }
            Item::Messages => {
                let u = v.msgs.unseen;
                let n = u.errors + u.warnings;
                let desc = if n > 0 {
                    c.tf("messages.desc_new", &[("n", &n.to_string())])
                } else {
                    c.t("messages.desc")
                };
                let resp = widgets::rich_tooltip(resp, th, &c.t("messages.title"), &desc, "");
                self.msgs_menu(&resp, v);
            }
            Item::Save => {
                let mut desc = vec![v.autosave.0.clone()];
                if let Some(w) = &v.autosave_why {
                    desc.push(c.tf("autosave_status.autosave_reason", &[("why", w)]));
                }
                desc.extend(v.ifc.iter().cloned());
                let resp = widgets::rich_tooltip(
                    resp,
                    th,
                    &save_text(c, &v.save),
                    &desc.join("\n"),
                    self.keys(v, "file.save"),
                );
                self.save_menu(&resp, v);
            }
            Item::Gpu => {
                if let Some((label, tip, live)) = &v.soft {
                    let resp = widgets::rich_tooltip(resp, th, label, tip, "");
                    if *live && resp.clicked() {
                        self.acts.push(Act::Note(NoteAct::TryCard));
                    }
                }
            }
            Item::Task => {}
        }
    }

    /// Popover with the notification history; having it open counts as having read it.
    fn msgs_menu(&mut self, resp: &egui::Response, v: &View) {
        use ddc_ui::notify::Level;
        let (th, c) = (self.th, self.c);
        let (mut seen, mut clear) = (false, false);
        let icons = &mut *self.icons;
        up_menu(resp, |ui| {
            seen = true;
            section(ui, th, &c.t("messages.title"));
            egui::ScrollArea::vertical()
                .max_height(MSGS_H)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (level, time, text) in &v.msgs.rows {
                        let w = ui.available_width().max(MENU_W);
                        let (r, resp) =
                            ui.allocate_exact_size(Vec2::new(w, th.d().row), Sense::hover());
                        let col = match level {
                            Level::Error => th.c("error"),
                            Level::Warning => th.c("warning"),
                            Level::Success => th.c("success"),
                            Level::Info => th.c("text.secondary"),
                        };
                        let cy = r.center().y;
                        let p = ui.painter();
                        let mut x = r.left() + space::SM;
                        p.circle_filled(
                            egui::pos2(x + space::XS, cy),
                            space::XS - stroke::HAIRLINE / 2.0,
                            col,
                        );
                        x += space::SM + space::XS;
                        let g = p.layout_no_wrap(
                            time.clone(),
                            th.font(Role::Numeric),
                            th.c("text.secondary"),
                        );
                        let tw = g.size().x;
                        p.galley(egui::pos2(x, cy - g.size().y / 2.0), g, th.c("text.secondary"));
                        x += tw.max(MSG_TIME_W) + space::SM;
                        let g = widgets::elide(
                            ui,
                            text,
                            th.font(Role::Body),
                            th.c("text.primary"),
                            (r.right() - space::SM - x).max(0.0),
                        );
                        ui.painter()
                            .galley(egui::pos2(x, cy - g.size().y / 2.0), g, th.c("text.primary"));
                        resp.on_hover_text(text.as_str());
                    }
                });
            ui.separator();
            let t = c.t("messages.copy");
            if menu_row(ui, th, icons, Some("copy"), &t, Trail::None, false).clicked() {
                let all: Vec<String> = v
                    .msgs
                    .rows
                    .iter()
                    .map(|(_, time, text)| format!("{time} {text}").trim().to_string())
                    .collect();
                ui.ctx().copy_text(all.join("\n"));
            }
            let t = c.t("messages.clear");
            if menu_row(ui, th, icons, Some("delete"), &t, Trail::None, false).clicked() {
                clear = true;
                ui.close();
            }
        });
        if seen && (v.msgs.unseen.errors + v.msgs.unseen.warnings) > 0 {
            self.acts.push(Act::MsgsSeen);
        }
        if clear {
            self.acts.push(Act::MsgsClear);
        }
    }

    fn pick_menu(&mut self, resp: &egui::Response, v: &View) {
        let mut off = None;
        let mut pinned = v.pinned;
        let (th, c) = (self.th, self.c);
        up_menu(resp, |ui| {
            off = crate::pick_rules::filter_menu(ui, th, c, &v.pick);
            ui.checkbox(&mut pinned, c.t("pick_rules.pinned"));
        });
        if let Some(off) = off {
            self.acts.push(Act::PickOff(off));
        }
        if pinned != v.pinned {
            self.acts.push(Act::Pinned(pinned));
        }
    }

    fn level_menu(&mut self, resp: &egui::Response, v: &View) {
        let mut pick = None;
        let th = self.th;
        let icons = &mut *self.icons;
        up_menu(resp, |ui| {
            for (id, name, mark) in &v.levels {
                let on = *id == v.level_id;
                if menu_row(ui, th, icons, Some("level"), name, Trail::Text(mark), on).clicked() {
                    pick = Some(*id);
                }
            }
        });
        if let Some(id) = pick.filter(|id| *id != v.level_id) {
            self.acts.push(Act::Level(id));
        }
    }

    fn style_menu(&mut self, resp: &egui::Response, v: &View) {
        let mut pick = None;
        let (th, c) = (self.th, self.c);
        let keys: Vec<&str> = VisualStyle::ALL
            .iter()
            .map(|s| self.keys(v, style_cmd(*s)))
            .collect();
        let icons = &mut *self.icons;
        up_menu(resp, |ui| {
            section(ui, th, &c.t("sb.style"));
            for (s, k) in VisualStyle::ALL.into_iter().zip(keys.iter().copied()) {
                let name = c.t(&format!("v3d.style.{}", s.key()));
                let icon = crate::gfx_ui::style_icon(s);
                let r = menu_row(
                    ui,
                    th,
                    icons,
                    Some(icon),
                    &name,
                    Trail::Keys(k),
                    s == v.style,
                );
                let r = r.on_hover_text(c.t(&format!("v3d.style.tip.{}", s.key())));
                if r.clicked() {
                    pick = Some(s);
                }
            }
        });
        if let Some(s) = pick {
            self.acts.push(Act::Run(style_cmd(s)));
        }
    }

    fn perf_menu(&mut self, resp: &egui::Response, v: &View) {
        let mut pick = None;
        let mut act = None;
        let (th, c) = (self.th, self.c);
        let icons = &mut *self.icons;
        up_menu(resp, |ui| {
            if let Some((text, tip, a)) = &v.note {
                note(ui, th, text);
                if ui.link(c.t("sb.perf.fix")).on_hover_text(tip).clicked() {
                    act = Some(*a);
                }
                ui.separator();
            }
            for n in &v.notes {
                note(ui, th, n);
            }
            section(ui, th, &c.t("sb.perf"));
            for l in PerfLevel::ALL {
                let name = c.t(&format!("settings.gfx.{}", l.key()));
                let r = menu_row(
                    ui,
                    th,
                    icons,
                    Some(perf_icon(l)),
                    &name,
                    Trail::None,
                    l == v.perf,
                );
                let r = r.on_hover_text(c.t(&format!("cmd.{}.desc", perf_cmd(l))));
                if r.clicked() {
                    pick = Some(l);
                }
            }
            ui.separator();
            let t = c.t("sb.perf.settings");
            if menu_row(ui, th, icons, Some("settings"), &t, Trail::None, false).clicked() {
                act = Some(NoteAct::OpenSettings);
            }
            if !v.gpu.is_empty() {
                let g = c.tf("sb.perf.gpu", &[("name", &v.gpu)]);
                ui.add(
                    egui::Label::new(th.rich(Role::Status, &g)).truncate(),
                );
            }
        });
        if let Some(l) = pick {
            self.acts.push(Act::Run(perf_cmd(l)));
        }
        if let Some(a) = act {
            self.acts.push(Act::Note(a));
        }
    }

    fn save_menu(&mut self, resp: &egui::Response, v: &View) {
        let mut run = None;
        let (th, c) = (self.th, self.c);
        let save_keys = self.keys(v, "file.save");
        let ifc_keys = self.keys(v, "file.save_ifc");
        let icons = &mut *self.icons;
        up_menu(resp, |ui| {
            let busy = matches!(v.save, SaveState::Saving);
            let t = c.t("sb.save.save");
            let r = ui
                .add_enabled_ui(!busy, |ui| {
                    menu_row(
                        ui,
                        th,
                        icons,
                        Some("save"),
                        &t,
                        Trail::Keys(save_keys),
                        false,
                    )
                })
                .inner;
            if r.clicked() {
                run = Some("file.save");
            }
            if let Some(pill) = &v.ifc {
                let t = c.t("sb.save.ifc");
                let r = menu_row(
                    ui,
                    th,
                    icons,
                    Some("ifc-out"),
                    &t,
                    Trail::Keys(ifc_keys),
                    false,
                );
                if r.on_hover_text(pill).clicked() {
                    run = Some("file.save_ifc");
                }
            }
            ui.separator();
            let t = c.t("sb.save.autosave");
            let r = menu_row(
                ui,
                th,
                icons,
                Some("autosave"),
                &t,
                Trail::Text(&v.autosave.0),
                false,
            );
            if r.clicked() {
                run = Some("mgr.autosave");
            }
        });
        if let Some(id) = run {
            self.acts.push(Act::Run(id));
        }
    }
}

/// «3 errors · 1 warning».
fn warn_title(c: &Catalog, v: &View) -> String {
    let n = v.faults;
    let mut title = Vec::new();
    if n.errors > 0 {
        title.push(c.plural("faults.errors", n.errors));
    }
    if n.warnings > 0 || n.errors == 0 {
        title.push(c.plural("faults.warnings", n.warnings));
    }
    title.join(" \u{b7} ")
}

/// What a click on the warnings control does.
fn warn_act(v: &View) -> Act {
    match faults::visible_tones(v.faults).first() {
        Some(&t) => Act::Fault(t),
        None => Act::Run("view.warnings"),
    }
}

fn sb_id(salt: &str) -> egui::Id {
    egui::Id::new(("ddc.sb", salt))
}

fn salt_of(item: Item) -> &'static str {
    match item {
        Item::Level => "level",
        Item::Style => "style",
        Item::Perf => "perf",
        Item::Pick => "pick",
        Item::Warn => "warn",
        Item::Messages => "msgs",
        Item::Save => "save",
        Item::Task => "task",
        Item::Gpu => "gpu",
    }
}

fn up_menu(resp: &egui::Response, add: impl FnOnce(&mut egui::Ui)) {
    egui::Popup::menu(resp)
        .align(egui::RectAlign::TOP_END)
        .gap(space::XS)
        .show(|ui| {
            ui.set_min_width(MENU_W);
            ui.spacing_mut().item_spacing.y = space::XXS;
            add(ui);
        });
}

fn section(ui: &mut egui::Ui, th: &Theme, title: &str) {
    ui.add_space(space::XXS);
    ui.label(
        egui::RichText::new(title.to_uppercase())
            .font(th.font(Role::Section))
            .color(th.c("text.secondary")),
    );
}

fn note(ui: &mut egui::Ui, th: &Theme, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.set_max_width(MENU_W);
        ui.label(
            egui::RichText::new(text)
                .font(th.font(Role::Body))
                .color(th.c("text.primary")),
        );
    });
}

enum Trail<'a> {
    None,
    Keys(&'a str),
    Text(&'a str),
}

/// One menu line: icon, label, keys or a muted value on the right; `on` marks the current one.
fn menu_row(
    ui: &mut egui::Ui,
    th: &Theme,
    icons: &mut ddc_ui::icons::IconCache,
    icon: Option<&str>,
    label: &str,
    trail: Trail,
    on: bool,
) -> egui::Response {
    menu_row_at(ui, th, icons, icon, label, trail, on, None)
}

/// As [`menu_row`], with a widget id fixed by the caller (probes and keyboard focus find it).
#[allow(clippy::too_many_arguments)]
fn menu_row_at(
    ui: &mut egui::Ui,
    th: &Theme,
    icons: &mut ddc_ui::icons::IconCache,
    icon: Option<&str>,
    label: &str,
    trail: Trail,
    on: bool,
    id: Option<egui::Id>,
) -> egui::Response {
    let w = ui.available_width().max(MENU_W);
    let size = Vec2::new(w, th.d().row);
    let (r, resp) = match id {
        Some(id) => {
            let (_, r) = ui.allocate_space(size);
            (r, ui.interact(r, id, Sense::click()))
        }
        None => ui.allocate_exact_size(size, Sense::click()),
    };
    let fg = widgets::paint_state(ui, th, r, &resp, on);
    let cy = r.center().y;
    let mut x = r.left() + space::SM;
    if let Some(i) = icon {
        let ir = Rect::from_min_size(egui::pos2(x, cy - ICON / 2.0), Vec2::splat(ICON));
        icons.paint(ui, &widgets::icon_name(i, on), ICON as u32, ir, fg);
        x += ICON + space::SM;
    }
    let p = ui.painter();
    let right = match trail {
        Trail::Keys(k) if !k.is_empty() => {
            widgets::paint_keycaps(p, th, egui::pos2(r.right() - space::SM, cy), k)
        }
        Trail::Text(t) if !t.is_empty() => {
            let g = p.layout_no_wrap(
                t.to_string(),
                th.font(Role::Numeric),
                th.c("text.secondary"),
            );
            let gx = r.right() - space::SM - g.size().x;
            p.galley(
                egui::pos2(gx, cy - g.size().y / 2.0),
                g,
                th.c("text.secondary"),
            );
            gx
        }
        _ => r.right() - space::SM,
    };
    let col = if on { fg } else { th.c("text.primary") };
    let g = widgets::elide(
        ui,
        label,
        th.font(Role::Body),
        col,
        (right - space::SM - x).max(0.0),
    );
    p.galley(egui::pos2(x, cy - g.size().y / 2.0), g, col);
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, label)
    });
    resp
}

/// Cursor readout: axis letters muted, numbers right-aligned in fixed slots, unit once.
struct Coords {
    width: f32,
    slot: f32,
    axis: f32,
    unit: f32,
}

impl Coords {
    const AXES: [&'static str; 3] = ["X", "Y", "Z"];

    fn measure(ui: &egui::Ui, th: &Theme, v: &View) -> Coords {
        let p = ui.painter();
        let w = |s: &str, role| {
            p.layout_no_wrap(s.to_string(), th.font(role), th.c("text.primary"))
                .size()
                .x
        };
        let slot = v
            .template
            .iter()
            .chain(v.xyz.iter().flatten())
            .map(|s| w(s.as_str(), Role::Numeric))
            .fold(0.0, f32::max);
        let axis = Self::AXES
            .iter()
            .map(|a| w(*a, Role::Caption))
            .fold(0.0, f32::max);
        let unit = if v.unit.is_empty() {
            0.0
        } else {
            space::XS + w(v.unit.as_str(), Role::Caption)
        };
        let width = 3.0 * (axis + space::XS + slot) + 2.0 * space::MD + unit;
        Coords {
            width,
            slot,
            axis,
            unit,
        }
    }

    fn paint(&self, ui: &mut egui::Ui, th: &Theme, c: &Catalog, r: Rect, v: &View) {
        let resp = ui.interact(r, egui::Id::new("ddc.sb.coords"), Sense::hover());
        let Some(xyz) = &v.xyz else { return };
        let p = ui.painter();
        let cy = r.center().y;
        let mut x = r.left();
        for (a, val) in Self::AXES.iter().zip(xyz) {
            let ga = p.layout_no_wrap(
                a.to_string(),
                th.font(Role::Caption),
                th.c("text.secondary"),
            );
            p.galley(
                egui::pos2(x, cy - ga.size().y / 2.0),
                ga,
                th.c("text.secondary"),
            );
            x += self.axis + space::XS;
            let gv = p.layout_no_wrap(val.clone(), th.font(Role::Numeric), th.c("text.primary"));
            let gx = x + self.slot - gv.size().x;
            p.galley(
                egui::pos2(gx, cy - gv.size().y / 2.0),
                gv,
                th.c("text.primary"),
            );
            x += self.slot + space::MD;
        }
        if self.unit > 0.0 {
            let gu = p.layout_no_wrap(
                v.unit.clone(),
                th.font(Role::Caption),
                th.c("text.secondary"),
            );
            let ux = x - space::MD + space::XS;
            p.galley(
                egui::pos2(ux, cy - gu.size().y / 2.0),
                gu,
                th.c("text.secondary"),
            );
        }
        resp.on_hover_text(c.t("sb.coords.tip"));
    }
}

/// Measured hint: each part's width, and the whole line.
struct HintRow {
    widths: Vec<f32>,
    total: f32,
}

const DOT: &str = "\u{b7}";

impl HintRow {
    fn keycaps_w(p: &egui::Painter, th: &Theme, keys: &str) -> f32 {
        let caps: Vec<f32> = keys
            .split(['+', ' '])
            .filter(|k| !k.is_empty())
            .map(|k| {
                let g = p.layout_no_wrap(
                    k.to_string(),
                    th.font(Role::Caption),
                    th.c("text.secondary"),
                );
                (g.size().x + space::MD).max(control::KEYCAP_H)
            })
            .collect();
        caps.iter().sum::<f32>() + space::XS * (caps.len().max(1) - 1) as f32
    }

    fn part_w(p: &egui::Painter, th: &Theme, i: usize, part: &Part) -> f32 {
        let tw = |s: &str| {
            p.layout_no_wrap(s.to_string(), th.font(Role::Body), th.c("text.secondary"))
                .size()
                .x
        };
        match part {
            Part::Text(t) if i == 0 => tw(t.as_str()),
            Part::Text(t) => tw(DOT) + space::MD + tw(t.as_str()),
            Part::Key { keys, desc } => {
                Self::keycaps_w(p, th, keys) + space::XS + tw(desc.as_str())
            }
        }
    }

    fn measure(ui: &egui::Ui, th: &Theme, parts: &[Part]) -> HintRow {
        let p = ui.painter();
        let widths: Vec<f32> = parts
            .iter()
            .enumerate()
            .map(|(i, part)| Self::part_w(p, th, i, part))
            .collect();
        let total = widths.iter().sum::<f32>() + space::MD * (widths.len().max(1) - 1) as f32;
        HintRow {
            widths,
            total: if parts.is_empty() { 0.0 } else { total },
        }
    }

    /// Paints the parts that fit whole in `r`; later ones are dropped and only a too long
    /// instruction is elided. True when anything was cut.
    fn paint(&self, ui: &egui::Ui, th: &Theme, r: Rect, parts: &[Part]) -> bool {
        let p = ui.painter_at(r);
        let cy = r.center().y;
        let mut x = r.left();
        let sec = th.c("text.secondary");
        let text = |p: &egui::Painter, x: f32, s: &str, col: Color32, max: f32| {
            let g = widgets::elide(ui, s, th.font(Role::Body), col, max);
            let w = g.size().x;
            p.galley(egui::pos2(x, cy - g.size().y / 2.0), g, col);
            w
        };
        for (i, (part, w)) in parts.iter().zip(&self.widths).enumerate() {
            let room = r.right() - x;
            if *w > room {
                // a part that does not fit is dropped whole; the tooltip carries the full hint
                if i == 0 {
                    text(&p, x, part_text(part), th.c("text.primary"), room);
                }
                return true;
            }
            match part {
                Part::Text(t) if i == 0 => {
                    text(&p, x, t.as_str(), th.c("text.primary"), *w + 1.0);
                }
                Part::Text(t) => {
                    let d = text(&p, x, DOT, sec, *w);
                    text(&p, x + d + space::MD, t.as_str(), sec, *w);
                }
                Part::Key { keys, desc } => {
                    let kw = Self::keycaps_w(&p, th, keys);
                    widgets::paint_keycaps(&p, th, egui::pos2(x + kw, cy), keys);
                    text(&p, x + kw + space::XS, desc.as_str(), sec, *w);
                }
            }
            x += w + space::MD;
        }
        false
    }
}

fn part_text(p: &Part) -> &str {
    match p {
        Part::Text(t) => t,
        Part::Key { desc, .. } => desc,
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod shots;
