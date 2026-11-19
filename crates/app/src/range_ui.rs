//! Drawing quality window: the plan view range with a side elevation (four draggable planes and
//! a probe of what each band holds), the pen table in mm per scale, cut and projection styles per
//! category, and the line pattern and hatch libraries. Every Apply is one model operation, so one
//! undo step.

use std::cell::RefCell;

use ddc_model::drawq::{Band, HatchLib, PatternLib, PenTable, Plane, ViewRange, MAX_MM, MIN_MM};
use egui::{self, Align2, Color32, Pos2, Rect, Sense, Stroke, Vec2};

use crate::editor::Editor;
use crate::shell::EditorApp;
use crate::units::{active_units, DecimalMark, Units};
use crate::ui_kit as kit;
use ddc_ui::widgets::{Button, Cell, Column, NumberField, NumberFormat, RowState, Table};
use crate::view_gfx_ui::run;

/// Catalog text in the window's own language.
fn t(ru: bool, key: &str) -> String {
    crate::tr_lang(ru, key, &[])
}

const WIN_SIZE: [f32; 2] = [640.0, 480.0];
const DIAGRAM_W: f32 = 230.0;
const DIAGRAM_H: f32 = 340.0;
const PAD: f32 = 12.0;
const LABEL_W: f32 = 76.0;
/// Pointer distance, px, within which a plane line is grabbed.
const REACH: f32 = 7.0;
/// Planes snap to this step, m.
const SNAP: f64 = 0.05;
/// Margin around the content of the diagram, m.
const MARGIN: f64 = 0.4;
const MAX_BARS: usize = 14;
/// Width of a weight column in the pen table: one value field.
const PEN_COL_W: f32 = ddc_ui::theme::layout::PROP_VALUE;
const BAR_W: f32 = 6.0;

/// Language of the number fields: text and the decimal mark the numbers are typed in.
#[derive(Clone, Copy)]
pub(crate) struct Loc {
    pub ru: bool,
    pub mark: DecimalMark,
}

impl Loc {
    pub(crate) fn of(ru: bool) -> Self {
        Self { ru, mark: DecimalMark::for_ru(ru) }
    }

    pub(crate) fn format(self) -> NumberFormat {
        if self.ru {
            NumberFormat::RU
        } else {
            NumberFormat::EN
        }
    }
}

/// Plain-number field on the kit (locale mark, muted unit, Enter/Tab/blur commit, Esc reverts,
/// refused text keeps the value). True once per commit. `salt` names the edited value.
#[allow(clippy::too_many_arguments)]
pub(crate) fn number_field(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash,
    v: &mut f64,
    decimals: usize,
    step: f64,
    range: (f64, f64),
    unit: &str,
    loc: Loc,
    label: (egui::Id, &str),
) -> bool {
    let th = ddc_ui::theme::of(ui.ctx());
    let bad = crate::tr_lang(loc.ru, "number_input.bad_number", &[]);
    NumberField::new(salt, v, loc.format())
        .labelled_by(label.0, label.1)
        .error_text(&bad)
        .decimals(decimals)
        .step(step)
        .range(range.0, range.1)
        .unit(unit)
        .width(ddc_ui::theme::layout::PROP_VALUE)
        .show(ui, &th)
        .committed
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Range,
    Pens,
    Styles,
    Lines,
}

const TABS: [(Tab, &str); 4] = [
    (Tab::Range, "draw_quality.tab.range"),
    (Tab::Pens, "draw_quality.tab.pens"),
    (Tab::Styles, "draw_quality.tab.styles"),
    (Tab::Lines, "draw_quality.tab.lines"),
];

fn tab_names(label: &dyn Fn(&str) -> String) -> Vec<String> {
    TABS.iter().map(|(_, k)| label(k)).collect()
}

/// Kit segmented control over `TABS`; true when the tab changed.
fn tab_bar(ui: &mut egui::Ui, tab: &mut Tab, names: &[String]) -> bool {
    let th = ddc_ui::theme::of(ui.ctx());
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let sel = TABS.iter().position(|(x, _)| x == tab).unwrap_or(0);
    match ddc_ui::widgets::segmented(ui, &th, &refs, sel) {
        Some(i) if i < TABS.len() => {
            *tab = TABS[i].0;
            true
        }
        _ => false,
    }
}

#[derive(Default)]
struct Win {
    open: bool,
    tab: Tab,
    level: Option<i64>,
    saved: ViewRange,
    draft: ViewRange,
    /// Where an open top (no upper limit) is shown; it stays open while left there.
    open_top: Option<f64>,
    grab: Option<Plane>,
    /// Vertical extents (base, top) above the level of the elements on it.
    bars: Vec<(f64, f64)>,
    /// Other levels: name and height above the current one.
    levels: Vec<(String, f64)>,
    pens: PenTable,
    pens_saved: PenTable,
    /// Row of the pen table whose weights the editor strip shows.
    pen_sel: usize,
    patterns: PatternLib,
    hatches: HatchLib,
    styles: crate::range_styles::Styles,
    /// The user asked to close while the line libraries or styles hold unsaved edits.
    close_req: bool,
    close_asked: bool,
}

/// What closing the window does: it asks while the libraries or the styles hold edits.
fn close_step(
    libs_dirty: bool,
    styles_dirty: bool,
    choice: Option<crate::unsaved::Choice>,
) -> crate::unsaved::Switch {
    crate::unsaved::decide_switch(libs_dirty || styles_dirty, choice)
}

/// Runs the close request of this frame; true once the window may close.
fn closing(ctx: &egui::Context, ed: &mut Editor, st: &mut Win, level: i64, ru: bool) -> bool {
    use crate::unsaved::{Choice, Switch};
    let (libs, styles) = (crate::range_libs::is_dirty(), st.styles.changed());
    let choice: Option<Choice> = if libs || styles {
        // (key of the question text, name of the edited entry, key of the Apply label)
        let (body_key, entry, apply) = if libs && !styles {
            ("draw_quality.unsaved.lib_body", crate::range_libs::dirty_name(ru), "draw_quality.lib.save")
        } else if styles && !libs {
            ("draw_quality.unsaved.styles_body", crate::tr_lang(ru, "draw_quality.styles.name", &[]), "draw_quality.apply")
        } else {
            ("draw_quality.unsaved.both_body", crate::range_libs::dirty_name(ru), "draw_quality.apply")
        };
        crate::unsaved::ask(
            ctx,
            ru,
            "ux49d.close",
            &entry,
            &crate::tr_lang(ru, body_key, &[]),
            &crate::tr_lang(ru, apply, &[]),
            st.close_asked,
        )
    } else {
        None
    };
    st.close_asked = false;
    match close_step(libs, styles, choice) {
        Switch::Now => true,
        Switch::DiscardThenGo => {
            crate::range_libs::discard();
            st.styles.revert();
            true
        }
        Switch::ApplyThenGo => commit(ed, st, libs, styles, level),
        Switch::Ask => {
            st.close_asked = true;
            false
        }
        Switch::Stay => {
            st.close_req = false;
            false
        }
    }
}

/// Writes the library entry, then the styles, into the model; a refusal shows its tab, false.
fn commit(ed: &mut Editor, st: &mut Win, libs: bool, styles: bool, level: i64) -> bool {
    let mut ok = true;
    if libs {
        let (mut pats, mut hats) = (st.patterns.clone(), st.hatches.clone());
        ok = crate::range_libs::save_current(ed, &mut pats, &mut hats);
        if ok {
            (st.patterns, st.hatches) = (pats, hats);
        } else {
            st.tab = Tab::Lines;
        }
    }
    if ok && styles {
        ok = st.styles.apply(ed, st.styles.level_or(level));
        if !ok {
            st.tab = Tab::Styles;
        }
    }
    ok
}

/// Drafts a project close or swap would lose: the edited library entry (its name) and whether
/// the line styles hold edits.
pub fn drafts(ru: bool) -> (Option<String>, bool) {
    let line = crate::range_libs::is_dirty().then(|| crate::range_libs::dirty_name(ru));
    let styles = WIN.with(|w| w.try_borrow().is_ok_and(|w| w.styles.changed()));
    (line, styles)
}

/// Discard on the project question: the same drop as Discard on this window's question.
pub fn discard_drafts() {
    crate::range_libs::discard();
    WIN.with(|w| {
        let mut w = w.borrow_mut();
        w.styles.revert();
        (w.close_req, w.close_asked) = (false, false);
    });
}

/// Save on the project question: commits the drafts; a refusal opens the window on that tab.
pub fn save_drafts(ed: &mut Editor) -> bool {
    let mut st = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    let (libs, styles) = (crate::range_libs::is_dirty(), st.styles.changed());
    let level = st.level.unwrap_or(ed.level);
    // the entry is added to the stored libraries, never to a list the window has not read yet
    if let (true, Ok(p), Ok(h)) = (libs, ed.model.drawq_patterns(), ed.model.drawq_hatches()) {
        (st.patterns, st.hatches) = (p, h);
    }
    let ok = !(libs || styles) || commit(ed, &mut st, libs, styles, level);
    if !ok {
        st.open = true;
    }
    WIN.with(|w| *w.borrow_mut() = st);
    ok
}

thread_local! {
    static WIN: RefCell<Win> = RefCell::new(Win::default());
}

/// Gives this thread's window an unsaved halftone override on `level` (project question tests).
#[cfg(test)]
pub(crate) fn seed_styles_draft_for_test(ed: &Editor, level: i64) {
    WIN.with(|w| {
        let mut w = w.borrow_mut();
        reload(&mut w, ed, level);
        let half = ddc_model::drawq::StyleOverride {
            halftone: true,
            ..Default::default()
        };
        w.styles.draft.insert("wall".into(), half);
        w.open = true;
    });
}

pub fn open() {
    WIN.with(|w| {
        let mut w = w.borrow_mut();
        w.open = true;
        w.level = None;
    });
}

/// Opens the window on tab `tab` (index into `TABS`) for the layout contract.
#[cfg(test)]
pub(crate) fn open_tab_for_layout(tab: usize) {
    WIN.with(|w| {
        let mut w = w.borrow_mut();
        w.open = true;
        w.level = None;
        w.tab = TABS[tab].0;
    });
}

pub fn register(app: &mut EditorApp) {
    app.register_command("view.range", |_| open());
}

// ---- diagram geometry (pure) ----

/// Vertical scale of the side elevation: metres up, pixels down.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Scale {
    z_min: f64,
    z_max: f64,
    y_top: f32,
    y_bottom: f32,
}

impl Scale {
    fn y(&self, z: f64) -> f32 {
        let t = ((z - self.z_min) / (self.z_max - self.z_min)) as f32;
        self.y_bottom + (self.y_top - self.y_bottom) * t
    }

    fn z(&self, y: f32) -> f64 {
        let t = (y - self.y_bottom) / (self.y_top - self.y_bottom);
        self.z_min + f64::from(t) * (self.z_max - self.z_min)
    }
}

/// Visible span of the diagram: the range and the wall extents plus a margin, at least 1 m.
fn fit(r: &ViewRange, bars: &[(f64, f64)]) -> (f64, f64) {
    let mut lo = r.depth;
    let mut hi = r.top;
    for (b, t) in bars {
        lo = lo.min(*b);
        hi = hi.max(*t);
    }
    let (lo, hi) = (lo - MARGIN, hi + MARGIN);
    if hi - lo < 1.0 {
        (lo, lo + 1.0)
    } else {
        (lo, hi)
    }
}

fn snap(z: f64) -> f64 {
    (z / SNAP).round() * SNAP
}

/// The plane whose line is within `reach` px of `y`; on a tie the upper one wins (cut before
/// bottom when they coincide).
fn nearest_plane(r: &ViewRange, sc: &Scale, y: f32, reach: f32) -> Option<Plane> {
    Plane::ALL
        .into_iter()
        .map(|p| (p, (sc.y(p.get(r)) - y).abs()))
        .filter(|(_, d)| *d <= reach)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(p, _)| p)
}

/// Counts of walls per band, for the probe line.
fn band_counts(r: &ViewRange, bars: &[(f64, f64)]) -> [usize; 5] {
    let mut n = [0; 5];
    for (b, t) in bars {
        let i = match r.classify(*b, *t) {
            Band::Cut => 0,
            Band::Overhead => 1,
            Band::Seen => 2,
            Band::Beyond => 3,
            Band::Hidden => 4,
        };
        n[i] += 1;
    }
    n
}

// ---- colours from tokens ----

struct Cols {
    bg: Color32,
    line: Color32,
    text: Color32,
    dim: Color32,
    cut: Color32,
    overhead: Color32,
    seen: Color32,
    beyond: Color32,
}

impl Cols {
    fn of(th: &ddc_ui::Theme) -> Self {
        Self {
            bg: th.c("bg.sunken"),
            line: th.c("border.subtle"),
            text: th.c("text.primary"),
            dim: th.c("text.secondary"),
            cut: th.c("accent"),
            overhead: th.c("warning"),
            seen: th.c("success"),
            beyond: th.c("text.secondary"),
        }
    }

    fn of_band(&self, b: Band) -> Color32 {
        match b {
            Band::Cut => self.cut,
            Band::Overhead => self.overhead,
            Band::Seen => self.seen,
            Band::Beyond => self.beyond,
            Band::Hidden => self.line,
        }
    }
}

// ---- window ----

/// Planes to show: an open top is drawn at the default span above the cut.
fn shown(r: ViewRange) -> (ViewRange, Option<f64>) {
    if r.top.is_finite() {
        return (r, None);
    }
    let d = ViewRange::default();
    let top = r.cut + (d.top - d.cut);
    (ViewRange { top, ..r }, Some(top))
}

/// The range changed elsewhere (view graphics window, template, undo): show it unless the user
/// is mid-edit. Reads the per-revision cache, not the database, every frame.
fn follow_stored(st: &mut Win, ed: &Editor) {
    let (now, open_top) = shown(ViewRange::from_plan(crate::plan_range::range(ed)));
    if st.draft == st.saved && now != st.saved {
        (st.saved, st.open_top) = (now, open_top);
        st.draft = now;
    }
}

/// Planes to write: a shown open top the user did not move stays open.
fn stored(d: ViewRange, open_top: Option<f64>) -> ViewRange {
    if open_top == Some(d.top) {
        ViewRange {
            top: f64::INFINITY,
            ..d
        }
    } else {
        d
    }
}

/// The planes when the plan's stored range can hold them (cut strictly above the bottom).
fn storable(d: &ViewRange) -> bool {
    d.check().is_ok() && d.to_plan().issue().is_none()
}

fn reload(st: &mut Win, ed: &Editor, level: i64) {
    st.level = Some(level);
    (st.saved, st.open_top) = shown(ed.model.plan_range(level).unwrap_or_default());
    st.draft = st.saved;
    st.grab = None;
    st.bars = elem_bars(ed, level);
    let levels = ed.model.level_list().unwrap_or_default();
    let here = levels
        .iter()
        .find(|l| l.0 == level)
        .map_or(0.0, |l| l.2);
    st.levels = levels
        .into_iter()
        .filter(|l| l.0 != level)
        .map(|l| (l.1, l.2 - here))
        .collect();
    st.pens = ed.model.drawq_pens().unwrap_or_default();
    st.pens_saved = st.pens.clone();
    st.patterns = ed.model.drawq_patterns().unwrap_or_default();
    st.hatches = ed.model.drawq_hatches().unwrap_or_default();
    st.styles.follow_level(ed, level);
}

/// Distinct vertical extents (base, top) of every element kind on the level, rounded to mm.
fn elem_bars(ed: &Editor, level: i64) -> Vec<(f64, f64)> {
    let r = |v: f64| (v * 1000.0).round() / 1000.0;
    let mut v: Vec<(f64, f64)> = ed
        .model
        .plan_extents(level)
        .unwrap_or_default()
        .into_iter()
        .map(|e| (r(e.z0), r(e.z1)))
        .collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    v.dedup();
    v.truncate(64);
    v
}

/// Called every frame from the shell.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let ru = a.sh.cat.lang == ddc_ui::Lang::Ru;
    let mut st = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if !st.open {
        WIN.with(|w| *w.borrow_mut() = st);
        return;
    }
    let level = a.ed.level;
    if st.level != Some(level) {
        reload(&mut st, &a.ed, level);
    }
    // an unsaved style draft keeps its level; once applied or reverted it follows this one
    st.styles.follow_level(&a.ed, level);
    // a question is open: its Esc answers it and must not also close the window
    if crate::range_libs::asking() || st.close_asked {
        crate::unsaved::hold_esc(ctx);
    }
    follow_stored(&mut st, &a.ed);
    let cols = Cols::of(&a.sh.theme);
    let units = active_units();
    let mut open = true;
    let ed = &mut a.ed;
    kit::Dialog::new("r3.drawq", t(ru, "draw_quality.title"), kit::DialogSize::Tool)
        .size(WIN_SIZE)
        .open(&mut open)
        .scroll_body()
        .show(ctx, |ui| {
            tab_bar(ui, &mut st.tab, &tab_names(&|k| t(ru, k)));
            ui.separator();
            match st.tab {
                Tab::Range => range_tab(ui, ed, &mut st, &cols, units, level, ru),
                Tab::Pens => pens_tab(ui, ed, &mut st, ru),
                Tab::Styles => {
                    let (pens, pats) = (st.pens_saved.clone(), st.patterns.clone());
                    let own = st.styles.level_or(level);
                    if own != level {
                        let name = ed
                            .model
                            .level_list()
                            .unwrap_or_default()
                            .into_iter()
                            .find(|l| l.0 == own)
                            .map_or_else(|| own.to_string(), |l| l.1);
                        kit::warn_note(
                            ui,
                            crate::tr_lang(ru, "draw_quality.styles.other_level", &[("level", name.as_str())]),
                        );
                    }
                    crate::range_styles::show(ui, ed, &mut st.styles, &pens, &pats, own, ru);
                }
                Tab::Lines => {
                    crate::range_libs::show(ui, ed, &mut st.patterns, &mut st.hatches, ru)
                }
            }
        });
    // X or Esc: close only once unsaved library or style edits are answered
    if !open {
        st.close_req = true;
    }
    if st.close_req && closing(ctx, ed, &mut st, level, ru) {
        st.open = false;
        st.close_req = false;
    }
    let _ = crate::unsaved::take_held_esc(ctx);
    WIN.with(|w| *w.borrow_mut() = st);
}

fn range_tab(
    ui: &mut egui::Ui,
    ed: &mut Editor,
    st: &mut Win,
    cols: &Cols,
    units: Units,
    level: i64,
    ru: bool,
) {
    ui.horizontal_top(|ui| {
        elevation(ui, st, cols, ru);
        ui.vertical(|ui| {
            plane_fields(ui, st, units, Loc::of(ru));
            ui.add_space(ui.spacing().item_spacing.y);
            probe(ui, st, cols, ru);
            ui.add_space(ui.spacing().item_spacing.y);
            range_buttons(ui, ed, st, level, ru);
        });
    });
}

/// Lowest and highest plane height a typed value may take, m.
const PLANE_LO: f64 = -100.0;
const PLANE_HI: f64 = 1000.0;

/// The four plane heights as typed lengths in project units. A value changes only on a commit
/// (Enter, Tab, blur); Esc restores, refused text keeps the draft and shows the field error.
fn plane_fields(ui: &mut egui::Ui, st: &mut Win, units: Units, loc: Loc) {
    let th = ddc_ui::theme::of(ui.ctx());
    for pl in Plane::ALL {
        let mut v = pl.get(&st.draft);
        let name = crate::tr_lang(loc.ru, &format!("draw_quality.plane.{}", pl.key()), &[]);
        let done = kit::labeled_row_id(ui, &name, |ui, lid| {
            let salt = egui::Id::new(("ux56e.plane", pl.key()));
            let done = kit::fixed_cell(ui, ddc_ui::theme::layout::PROP_VALUE, |ui| {
                kit::len_field(ui, &th, salt, &mut v, units, PLANE_LO, PLANE_HI, loc.ru, lid, &name)
            });
            // feet and inches carry their marks in the text; metric shows mm after it
            let unit = match units {
                Units::Metric => crate::tr_str(loc.ru, "unit_fmt.unit.mm"),
                Units::Imperial => "",
            };
            if !unit.is_empty() {
                ui.weak(unit);
            }
            done
        });
        if done {
            st.draft = st.draft.with_plane(pl, v);
        }
        ui.add_space(ddc_ui::theme::space::XS);
    }
}

fn probe(ui: &mut egui::Ui, st: &Win, cols: &Cols, ru: bool) {
    // an open top is only drawn at a finite height: count with the planes the plan uses
    let n = band_counts(&stored(st.draft, st.open_top), &st.bars);
    for (i, band) in [
        Band::Cut,
        Band::Overhead,
        Band::Seen,
        Band::Beyond,
        Band::Hidden,
    ]
    .into_iter()
    .enumerate()
    {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(
                Vec2::splat(ui.text_style_height(&egui::TextStyle::Body)),
                Sense::hover(),
            );
            ui.painter().rect_filled(r, 2.0, cols.of_band(band));
            ui.label(crate::tr_lang(
                ru,
                &format!("draw_quality.band.{}", band_key(band)),
                &[("n", &n[i].to_string())],
            ));
        });
    }
}

fn band_key(b: Band) -> &'static str {
    match b {
        Band::Cut => "cut",
        Band::Seen => "seen",
        Band::Overhead => "overhead",
        Band::Beyond => "beyond",
        Band::Hidden => "hidden",
    }
}

/// Stores the draft planes in one model operation (one undo step).
fn apply_range(ed: &mut Editor, st: &mut Win, level: i64) -> bool {
    let d = stored(st.draft, st.open_top);
    let ok = run(ed, move |m| m.set_plan_range(level, d));
    if ok {
        st.saved = st.draft;
    }
    ok
}

fn range_buttons(ui: &mut egui::Ui, ed: &mut Editor, st: &mut Win, level: i64, ru: bool) {
    let same = st.draft == st.saved;
    let why = t(ru, "draw_quality.no_change");
    let bad = (!storable(&st.draft)).then(|| t(ru, "draw_quality.bad_range"));
    let blocked = bad.as_deref().or(same.then_some(why.as_str()));
    ui.horizontal(|ui| {
        if kit::primary_action(ui, t(ru, "draw_quality.apply"), blocked) {
            apply_range(ed, st, level);
        }
        let revert = ui
            .add_enabled_ui(!same, |ui| kit::secondary_button(ui, t(ru, "draw_quality.revert")))
            .inner;
        if revert.clicked() {
            st.draft = st.saved;
        }
        let th = ddc_ui::theme::of(ui.ctx());
        let label = t(ru, "draw_quality.default");
        if Button::quiet(&label).show(ui, &th).clicked()
            && run(ed, move |m| m.reset_plan_range(level))
        {
            // the same planes the plan and the view graphics window now read
            (st.saved, st.open_top) = shown(ed.model.plan_range(level).unwrap_or_default());
            st.draft = st.saved;
        }
    });
}

/// The side elevation: bands, wall extents, other levels and the four planes to drag.
fn elevation(ui: &mut egui::Ui, st: &mut Win, cols: &Cols, ru: bool) {
    let size = Vec2::new(DIAGRAM_W, DIAGRAM_H);
    let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
    let rect = resp.rect;
    let (z_min, z_max) = fit(&st.draft, &st.bars);
    let sc = Scale {
        z_min,
        z_max,
        y_top: rect.top() + PAD,
        y_bottom: rect.bottom() - PAD,
    };
    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            st.grab = nearest_plane(&st.draft, &sc, p.y, REACH);
        }
    }
    if resp.dragged() {
        if let (Some(pl), Some(p)) = (st.grab, resp.interact_pointer_pos()) {
            st.draft = st.draft.with_plane(pl, snap(sc.z(p.y)));
        }
    }
    if resp.drag_stopped() {
        st.grab = None;
    }
    if let Some(p) = resp.hover_pos() {
        if st.grab.is_some() || nearest_plane(&st.draft, &sc, p.y, REACH).is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
    }
    let font = egui::TextStyle::Small.resolve(ui.style());
    let (x0, x1) = (rect.left() + LABEL_W, rect.right() - PAD);
    painter.rect_filled(rect, 4.0, cols.bg);
    let r = st.draft;
    let band = |a: f64, b: f64, c: Color32| {
        let br = Rect::from_min_max(Pos2::new(x0, sc.y(a)), Pos2::new(x1, sc.y(b)));
        painter.rect_filled(br, 0.0, c.gamma_multiply(0.18));
    };
    band(r.top, r.cut, cols.overhead);
    band(r.cut, r.bottom, cols.seen);
    band(r.bottom, r.depth, cols.beyond);
    for (name, z) in &st.levels {
        let y = sc.y(*z);
        if y > sc.y_top - PAD && y < sc.y_bottom + PAD {
            painter.line_segment(
                [Pos2::new(x0, y), Pos2::new(x1, y)],
                Stroke::new(1.0, cols.line),
            );
            painter.text(
                Pos2::new(x1, y),
                Align2::RIGHT_BOTTOM,
                name,
                font.clone(),
                cols.dim,
            );
        }
    }
    // wall extents as bars coloured by their band: the "why is it hidden" probe
    let n = st.bars.len().min(MAX_BARS);
    let step = ((x1 - x0) / (MAX_BARS as f32 + 1.0)).max(BAR_W);
    let eff = stored(r, st.open_top);
    for (i, (b, top)) in st.bars.iter().take(n).enumerate() {
        let x = x0 + step * (i as f32 + 0.5);
        let bar = Rect::from_min_max(Pos2::new(x, sc.y(*top)), Pos2::new(x + BAR_W, sc.y(*b)));
        painter.rect_filled(bar, 1.0, cols.of_band(eff.classify(*b, *top)));
    }
    // ground line of the level
    let y0 = sc.y(0.0);
    painter.line_segment(
        [Pos2::new(rect.left() + PAD, y0), Pos2::new(x1, y0)],
        Stroke::new(1.0, cols.dim),
    );
    for pl in Plane::ALL {
        let z = pl.get(&r);
        let y = sc.y(z);
        let hot = st.grab == Some(pl);
        let (c, w) = if pl == Plane::Cut {
            (cols.cut, 2.0)
        } else {
            (cols.text, 1.0)
        };
        painter.line_segment(
            [Pos2::new(rect.left() + PAD, y), Pos2::new(x1, y)],
            Stroke::new(if hot { w + 1.0 } else { w }, c),
        );
        painter.text(
            Pos2::new(rect.left() + PAD, y),
            Align2::LEFT_BOTTOM,
            t(ru, &format!("draw_quality.plane.{}", pl.key())),
            font.clone(),
            c,
        );
    }
}

// ---- pens ----

fn pens_tab(ui: &mut egui::Ui, ed: &mut Editor, st: &mut Win, ru: bool) {
    let mut denoms: Vec<u32> = st
        .pens
        .pens
        .iter()
        .flat_map(|p| p.columns.iter().map(|c| c.0))
        .collect();
    denoms.sort_unstable();
    denoms.dedup();
    let loc = Loc::of(ru);
    ui.label(t(ru, "draw_quality.pens.hint"));
    ui.add_space(ddc_ui::theme::space::XS);
    pens_table(ui, st, &denoms, loc);
    ui.add_space(ddc_ui::theme::space::SM);
    pen_editor(ui, st, &denoms, loc);
    ui.add_space(ddc_ui::theme::space::SM);
    let same = st.pens == st.pens_saved;
    let why = t(ru, "draw_quality.no_change");
    let bad = (!st.pens.check()).then(|| t(ru, "draw_quality.bad_pens"));
    let blocked = bad.as_deref().or(same.then_some(why.as_str()));
    ui.horizontal(|ui| {
        if kit::primary_action(ui, t(ru, "draw_quality.apply"), blocked) {
            apply_pens(ed, st);
        }
        let th = ddc_ui::theme::of(ui.ctx());
        let label = t(ru, "draw_quality.pens.builtin");
        if Button::quiet(&label).show(ui, &th).clicked() {
            st.pens = PenTable::default();
        }
    });
}

/// Stores the pen table in one model operation (one undo step).
fn apply_pens(ed: &mut Editor, st: &mut Win) -> bool {
    let p = st.pens.clone();
    let ok = run(ed, move |m| m.set_drawq_pens(&p));
    if ok {
        st.pens_saved = st.pens.clone();
    }
    ok
}

/// Pen name as shown: the catalog name of a built-in pen, else its own.
fn pen_title(name: &str, ru: bool) -> String {
    let key = format!("draw_quality.pen.{name}");
    let shown = crate::tr_lang(ru, &key, &[]);
    if shown == key {
        name.to_string()
    } else {
        shown
    }
}

/// Weight of a pen at a scale, mm; None where the pen has no such column.
fn pen_mm(pen: &ddc_model::drawq::Pen, denom: u32) -> Option<f64> {
    pen.columns.iter().find(|c| c.0 == denom).map(|c| f64::from(c.1))
}

/// The pens as a table: names left, weights right-aligned with the mm unit muted. A click
/// selects the row the editor strip below works on.
fn pens_table(ui: &mut egui::Ui, st: &mut Win, denoms: &[u32], loc: Loc) {
    let th = ddc_ui::theme::of(ui.ctx());
    let head: Vec<String> = denoms.iter().map(|d| format!("1:{d}")).collect();
    let pen_l = crate::tr_lang(loc.ru, "draw_quality.pen", &[]);
    let mut cols = vec![Column::text(&pen_l).sortable(false)];
    cols.extend(head.iter().map(|h| Column::number(h, PEN_COL_W).sortable(false)));
    let unit = crate::tr_str(loc.ru, "unit_fmt.unit.mm");
    let sel = st.pen_sel.min(st.pens.pens.len().saturating_sub(1));
    let pens = &st.pens.pens;
    let mut sort = None;
    let out = Table::new("ux56e.pens", &cols, pens.len()).show(
        ui,
        &th,
        &mut sort,
        |r| if r == sel { RowState::Selected } else { RowState::Normal },
        |r, c| {
            let Some(pen) = pens.get(r) else {
                return Cell::muted("");
            };
            if c == 0 {
                return Cell::text(pen_title(&pen.name, loc.ru));
            }
            match pen_mm(pen, denoms[c - 1]) {
                Some(w) => Cell::num(ddc_ui::widgets::field::format_number(w, 2, loc.format()), unit),
                None => Cell::muted("-"),
            }
        },
    );
    if let Some(r) = out.clicked {
        st.pen_sel = r;
    }
}

/// Weights of the selected pen, one field per scale: mm muted, commit on Enter, Tab or blur.
fn pen_editor(ui: &mut egui::Ui, st: &mut Win, denoms: &[u32], loc: Loc) {
    let sel = st.pen_sel.min(st.pens.pens.len().saturating_sub(1));
    let Some(pen) = st.pens.pens.get_mut(sel) else {
        return;
    };
    let title = pen_title(&pen.name, loc.ru);
    ui.label(egui::RichText::new(title.clone()).strong());
    let unit = crate::tr_str(loc.ru, "unit_fmt.unit.mm");
    ui.horizontal_wrapped(|ui| {
        for d in denoms {
            let Some(col) = pen.columns.iter_mut().find(|c| c.0 == *d) else {
                continue;
            };
            let name = format!("{title} 1:{d}");
            let mut w = f64::from(col.1);
            let range = (f64::from(MIN_MM), f64::from(MAX_MM));
            // scale and its field wrap as one: the scale never breaks off or over lines
            use ddc_ui::widgets::form;
            let th = ddc_ui::theme::of(ui.ctx());
            let scale = format!("1:{d}");
            let gap = ui.spacing().item_spacing.x;
            let pair_w = form::text_w(ui, &th, &scale) + gap + ddc_ui::theme::layout::PROP_VALUE;
            let size = egui::vec2(pair_w, form::row_h(&th));
            let lay = egui::Layout::left_to_right(egui::Align::Center);
            ui.allocate_ui_with_layout(size, lay, |ui| {
                let lid = form::label(ui, &th, &scale, f32::INFINITY).id;
                let id = ("ux56e.pen", sel, *d);
                if number_field(ui, id, &mut w, 2, 0.01, range, unit, loc, (lid, &name)) {
                    col.1 = w as f32;
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Context with the named font weights (table headers, tabs, labels): a bare one panics.
    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    use crate::unsaved::{Choice, Switch};

    #[test]
    fn closing_with_unsaved_edits_asks_and_clean_closing_does_not() {
        assert_eq!(close_step(false, false, None), Switch::Now);
        assert_eq!(close_step(true, false, None), Switch::Ask);
        assert_eq!(close_step(false, true, None), Switch::Ask);
        assert_eq!(close_step(true, true, Some(Choice::Cancel)), Switch::Stay);
        assert_eq!(close_step(true, false, Some(Choice::Discard)), Switch::DiscardThenGo);
        assert_eq!(close_step(false, true, Some(Choice::Apply)), Switch::ApplyThenGo);
    }

    fn sc() -> Scale {
        Scale {
            z_min: -1.0,
            z_max: 3.0,
            y_top: 0.0,
            y_bottom: 400.0,
        }
    }

    #[test]
    fn scale_round_trips_and_points_up() {
        let s = sc();
        assert_eq!(s.y(-1.0), 400.0);
        assert_eq!(s.y(3.0), 0.0);
        assert!(s.y(2.0) < s.y(1.0), "higher z is higher on screen");
        for z in [-0.5, 0.0, 1.2, 2.9] {
            assert!((s.z(s.y(z)) - z).abs() < 1e-5);
        }
    }

    #[test]
    fn fit_covers_range_and_walls() {
        let r = ViewRange::default();
        let (lo, hi) = fit(&r, &[(-2.0, 1.0), (0.0, 5.0)]);
        assert!(lo < -2.0 && hi > 5.0);
        let (lo, hi) = fit(&r, &[]);
        assert!(lo < r.depth && hi > r.top);
        let flat = ViewRange {
            top: 0.0,
            cut: 0.0,
            bottom: 0.0,
            depth: 0.0,
        };
        let (lo, hi) = fit(&flat, &[]);
        assert!(hi - lo >= 1.0);
    }

    #[test]
    fn snapping_and_grab() {
        assert!((snap(1.23) - 1.25).abs() < 1e-9);
        assert!((snap(-0.02)).abs() < 1e-9);
        let r = ViewRange::default();
        let s = Scale {
            z_min: -1.0,
            z_max: 4.0,
            y_top: 0.0,
            y_bottom: 500.0,
        };
        assert_eq!(nearest_plane(&r, &s, s.y(r.cut) + 2.0, REACH), Some(Plane::Cut));
        assert_eq!(nearest_plane(&r, &s, s.y(r.top) - 3.0, REACH), Some(Plane::Top));
        assert_eq!(nearest_plane(&r, &s, s.y(0.6), 1.0), None);
        let mut d = r;
        d = d.with_plane(Plane::Bottom, snap(s.z(s.y(2.0))));
        assert!(d.check().is_ok());
        assert_eq!(d.cut, 2.0, "dragging the bottom above the cut pushes the cut");
    }

    #[test]
    fn open_top_stays_open_unless_moved() {
        let open = ViewRange::from_plan(ddc_model::ViewRange::default());
        let (s, o) = shown(open);
        assert!(s.top.is_finite() && s.check().is_ok());
        assert_eq!(o, Some(s.top));
        assert_eq!(stored(s, o).to_plan(), ddc_model::ViewRange::default());
        let cut = s.with_plane(Plane::Cut, 0.5);
        assert_eq!(stored(cut, o).to_plan().top, None, "moving the cut keeps the top open");
        let top = s.with_plane(Plane::Top, 2.5);
        assert_eq!(stored(top, o).to_plan().top, Some(2.5));
        // a finite top is shown as it is
        assert_eq!(shown(top), (top, None));
        // equal cut and bottom cannot be stored: Apply is blocked
        assert!(!storable(&s.with_plane(Plane::Bottom, s.cut)));
        assert!(storable(&s));
        // a wall above the shown open top is drawn by the plan: never counted as hidden
        let high = [(s.top + 1.0, s.top + 2.0)];
        assert_eq!(band_counts(&stored(s, o), &high), [0, 1, 0, 0, 0]);
        assert_eq!(band_counts(&stored(top, o), &high), [0, 0, 0, 0, 1]);
    }

    #[test]
    fn window_follows_a_range_set_elsewhere() {
        let mut e = Editor::new_in_memory().unwrap();
        let lv = e.level;
        let mut st = Win::default();
        reload(&mut st, &e, lv);
        let r = ddc_model::ViewRange {
            cut: 0.75,
            ..Default::default()
        };
        let v = ddc_model::ViewKey::Plan(lv);
        assert!(run(&mut e, move |m| m.set_view_range(v, r)));
        follow_stored(&mut st, &e);
        assert_eq!(st.draft.cut, 0.75, "the view graphics range shows here");
        assert_eq!(stored(st.draft, st.open_top).to_plan(), r);
        // mid-edit drafts are kept
        st.draft = st.draft.with_plane(Plane::Cut, 1.0);
        e.undo();
        follow_stored(&mut st, &e);
        assert_eq!(st.draft.cut, 1.0);
    }

    #[test]
    fn probe_counts_every_bar_once() {
        let r = ViewRange::default();
        let bars = [(0.0, 3.0), (2.0, 2.5), (0.0, 1.0), (-0.3, -0.1), (-3.0, -2.0)];
        let n = band_counts(&r, &bars);
        assert_eq!(n, [1, 1, 1, 1, 1]);
        assert_eq!(n.iter().sum::<usize>(), bars.len());
    }

    fn key_ev(key: egui::Key) -> Vec<egui::Event> {
        [true, false]
            .into_iter()
            .map(|pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .collect()
    }

    /// Erases the shown text, then types `s`.
    fn typed(s: &str) -> Vec<egui::Event> {
        let mut v = Vec::new();
        for _ in 0..24 {
            v.extend(key_ev(egui::Key::Backspace));
        }
        v.push(egui::Event::Text(s.to_string()));
        v
    }

    fn frame(ctx: &egui::Context, events: Vec<egui::Event>, add: &mut dyn FnMut(&mut egui::Ui)) -> usize {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0))),
            events,
            ..Default::default()
        };
        let full = ctx.run(input, |c| {
            egui::CentralPanel::default().show(c, |ui| add(ui));
        });
        full.shapes.len()
    }

    fn pen_frame(ctx: &egui::Context, w: &mut f64, loc: Loc, events: Vec<egui::Event>) -> (bool, egui::Id) {
        let mut out = (false, egui::Id::NULL);
        frame(ctx, events, &mut |ui| {
            let lid = ui.label("1:50").id;
            out.1 = ui.id().with(egui::Id::new(("t.ux56e.pen", 1)));
            let range = (f64::from(MIN_MM), f64::from(MAX_MM));
            out.0 = number_field(ui, ("t.ux56e.pen", 1), w, 2, 0.01, range, "mm", loc, (lid, "pen"));
        });
        out
    }

    /// Focuses the pen field and types `text`; returns the field id.
    fn pen_type(ctx: &egui::Context, w: &mut f64, loc: Loc, text: &str) -> egui::Id {
        let (_, id) = pen_frame(ctx, w, loc, vec![]);
        ctx.memory_mut(|m| m.request_focus(id));
        pen_frame(ctx, w, loc, vec![]);
        pen_frame(ctx, w, loc, typed(text));
        id
    }

    #[test]
    fn ru_pen_width_commits_with_a_comma_on_enter() {
        let ctx = font_ctx();
        let loc = Loc::of(true);
        let mut w = 0.18;
        pen_type(&ctx, &mut w, loc, "0,25");
        assert_eq!(w, 0.18, "typing alone commits nothing");
        let (done, _) = pen_frame(&ctx, &mut w, loc, key_ev(egui::Key::Enter));
        assert!(done && (w - 0.25).abs() < 1e-9, "{w}");
        // the same comma in English is a group mark: refused
        let (en, ctx_en) = (Loc::of(false), font_ctx());
        let mut v = 0.18;
        pen_type(&ctx_en, &mut v, en, "0,25");
        pen_frame(&ctx_en, &mut v, en, key_ev(egui::Key::Enter));
        assert_eq!(v, 0.18);
    }

    #[test]
    fn esc_reverts_a_pen_edit() {
        let ctx = font_ctx();
        let loc = Loc::of(false);
        let mut w = 0.18;
        pen_type(&ctx, &mut w, loc, "0.5");
        pen_frame(&ctx, &mut w, loc, key_ev(egui::Key::Escape));
        pen_frame(&ctx, &mut w, loc, vec![]);
        assert_eq!(w, 0.18);
    }

    #[test]
    fn refused_pen_text_keeps_the_value_and_the_field_open() {
        let ctx = font_ctx();
        let loc = Loc::of(false);
        let mut w = 0.18;
        let id = pen_type(&ctx, &mut w, loc, "abc");
        let (done, _) = pen_frame(&ctx, &mut w, loc, key_ev(egui::Key::Enter));
        pen_frame(&ctx, &mut w, loc, vec![]);
        assert!(!done && w == 0.18, "{w}");
        assert!(ctx.memory(|m| m.has_focus(id)), "the error stays in the field");
    }

    fn plane_frame(ctx: &egui::Context, st: &mut Win, u: Units, loc: Loc, events: Vec<egui::Event>) -> usize {
        frame(ctx, events, &mut |ui| plane_fields(ui, st, u, loc))
    }

    #[test]
    fn plane_commits_in_feet_and_inches() {
        let ctx = font_ctx();
        let (u, loc) = (Units::Imperial, Loc::of(false));
        let mut st = Win::default();
        plane_frame(&ctx, &mut st, u, loc, vec![]);
        // Tab reaches the first field (the top plane)
        plane_frame(&ctx, &mut st, u, loc, key_ev(egui::Key::Tab));
        plane_frame(&ctx, &mut st, u, loc, vec![]);
        assert!(ctx.memory(|m| m.focused()).is_some(), "Tab focuses a plane field");
        plane_frame(&ctx, &mut st, u, loc, typed("12'"));
        assert_eq!(st.draft.top, 3.0, "typing alone commits nothing");
        plane_frame(&ctx, &mut st, u, loc, key_ev(egui::Key::Enter));
        plane_frame(&ctx, &mut st, u, loc, vec![]);
        assert!((st.draft.top - 12.0 * crate::units::FOOT).abs() < 1e-9, "{}", st.draft.top);
    }

    #[test]
    fn plane_text_is_read_in_the_ui_language() {
        let u = Units::Metric;
        let ru = kit::len_commit("1200,5", u, PLANE_LO, PLANE_HI, DecimalMark::Comma);
        assert!(ru.is_ok());
        let en = kit::len_commit("1200,5", u, PLANE_LO, PLANE_HI, DecimalMark::Point);
        assert_ne!(ru.ok(), en.ok());
        assert!(kit::len_commit("abc", u, PLANE_LO, PLANE_HI, DecimalMark::Point).is_err());
    }

    #[test]
    fn range_and_pens_apply_as_one_undo_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let level = ed.level;
        let mut st = Win::default();
        reload(&mut st, &ed, level);
        let before = ed.model.plan_range(level).unwrap_or_default();
        st.draft = st.draft.with_plane(Plane::Cut, 0.5);
        assert!(apply_range(&mut ed, &mut st, level));
        assert_ne!(ed.model.plan_range(level).unwrap_or_default(), before);
        ed.undo();
        assert_eq!(ed.model.plan_range(level).unwrap_or_default(), before);

        let pens = ed.model.drawq_pens().unwrap();
        st.pens = pens.clone();
        st.pens_saved = pens.clone();
        st.pens.pens[0].columns[0].1 = 0.35;
        assert!(apply_pens(&mut ed, &mut st));
        assert_eq!(st.pens_saved, st.pens);
        assert_ne!(ed.model.drawq_pens().unwrap(), pens);
        ed.undo();
        assert_eq!(ed.model.drawq_pens().unwrap(), pens);
    }

    #[test]
    fn plane_and_pen_widgets_render_in_both_languages() {
        for (ru, u) in [(false, Units::Metric), (true, Units::Metric), (false, Units::Imperial), (true, Units::Imperial)] {
            let ctx = font_ctx();
            let loc = Loc::of(ru);
            let mut st = Win { pens: PenTable::default(), ..Default::default() };
            let denoms: Vec<u32> = vec![50, 100, 200];
            let mut shapes = 0;
            for _ in 0..3 {
                shapes = frame(&ctx, vec![], &mut |ui| {
                    plane_fields(ui, &mut st, u, loc);
                    pens_table(ui, &mut st, &denoms, loc);
                    pen_editor(ui, &mut st, &denoms, loc);
                });
            }
            assert!(shapes > 0, "{ru}");
        }
    }

    fn tabs_frame(ctx: &egui::Context, tab: &mut Tab, ru: bool, events: Vec<egui::Event>) -> usize {
        let names = tab_names(&|k| crate::tr_lang(ru, k, &[]));
        frame(ctx, events, &mut |ui| {
            tab_bar(ui, tab, &names);
        })
    }

    #[test]
    fn tab_bar_renders_en_ru_and_keeps_state() {
        for ru in [false, true] {
            let ctx = font_ctx();
            ctx.set_style(ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact).style());
            let mut tab = Tab::Pens;
            let mut shapes = 0;
            for _ in 0..3 {
                shapes = tabs_frame(&ctx, &mut tab, ru, vec![]);
            }
            assert!(shapes > 0, "{ru}");
            assert!(tab == Tab::Pens, "idle frames keep the tab");
            let names = tab_names(&|k| crate::tr_lang(ru, k, &[]));
            assert!(names.iter().all(|n| !n.is_empty() && !n.starts_with("draw_quality.")), "{names:?}");
        }
    }

    #[test]
    fn tab_bar_click_switches_tab() {
        let ctx = font_ctx();
        ctx.set_style(ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact).style());
        let mut tab = Tab::Range;
        for _ in 0..3 {
            tabs_frame(&ctx, &mut tab, false, vec![]);
        }
        // sweep the bar row: a click on some pill must move the tab off Range and keep it
        let mut hit = false;
        for x in (10..500).step_by(10) {
            let p = Pos2::new(x as f32, 12.0);
            let btn = |pressed| egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            tabs_frame(&ctx, &mut tab, false, vec![egui::Event::PointerMoved(p)]);
            tabs_frame(&ctx, &mut tab, false, vec![btn(true)]);
            tabs_frame(&ctx, &mut tab, false, vec![btn(false)]);
            if tab != Tab::Range {
                hit = true;
                break;
            }
        }
        assert!(hit, "a click on the bar switches the tab");
        let kept = tab;
        tabs_frame(&ctx, &mut tab, false, vec![]);
        assert!(tab == kept);
    }

    #[test]
    fn tab_labels_exist_in_both_locales() {
        let en: serde_json::Value = serde_json::from_str(include_str!("../../ui/data/i18n/en/draw_quality.json")).unwrap();
        let ru: serde_json::Value = serde_json::from_str(include_str!("../../ui/data/i18n/ru/draw_quality.json")).unwrap();
        for (_, k) in TABS {
            assert!(en.get(k).is_some() && ru.get(k).is_some(), "{k}");
        }
    }

    #[test]
    fn pens_and_planes_use_kit_widgets_only() {
        let src = include_str!("range_ui.rs");
        // test-only helpers sit among the code: the scan ends at the test module
        let body = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        for raw in ["DragValue", "egui::Grid", "ui.button(", ".suffix(", ".speed(", "selectable_label", "selectable_value"] {
            assert!(!body.contains(raw), "raw widget left: {raw}");
        }
        assert!(body.contains("Table::new(") && body.contains("NumberField::new(") && body.contains("len_field("));
    }

    #[test]
    fn locale_json_is_paired_and_utf8() {
        let en = include_str!("../../ui/data/i18n/en/texture_hint.json");
        let ru = include_str!("../../ui/data/i18n/ru/texture_hint.json");
        let (en, ru): (serde_json::Value, serde_json::Value) =
            (serde_json::from_str(en).unwrap(), serde_json::from_str(ru).unwrap());
        let keys = |v: &serde_json::Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys(&en), keys(&ru));
        assert!(!keys(&en).is_empty());
    }

    #[test]
    fn project_drafts_are_listed_dropped_and_committed() {
        let mut ed = Editor::new_in_memory().unwrap();
        let level = ed.level;
        crate::range_libs::discard();
        assert_eq!(drafts(false), (None, false), "nothing edited: no draft");
        seed_styles_draft_for_test(&ed, level);
        crate::range_libs::seed_hatch_draft_for_test("h9");
        assert_eq!(drafts(false), (Some("h9".to_string()), true));
        discard_drafts();
        assert_eq!(drafts(false), (None, false));
        let stored = ed.model.drawq_hatches().unwrap();
        seed_styles_draft_for_test(&ed, level);
        crate::range_libs::seed_hatch_draft_for_test("h9");
        assert!(save_drafts(&mut ed));
        assert_eq!(drafts(false), (None, false));
        let hats = ed.model.drawq_hatches().unwrap();
        assert!(hats.get("h9").is_some());
        for name in stored.names() {
            assert!(hats.get(&name).is_some(), "stored hatch {name} kept");
        }
        let styles = crate::range_styles::Styles::load(&ed, level);
        assert!(styles.saved.contains_key("wall"));
    }
}
