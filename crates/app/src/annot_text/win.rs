// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Legends window: row contents, scale, sheet placement. Each action is one `run_op`.

use super::{name_of, op, with};
use crate::editor::Editor;
use ddc_model::{AnnoView, LegendRow, LegendSwatch, RegionFill, SCALES};
use ddc_ui::i18n::Catalog;
use ddc_ui::theme::{control, space, Role};
use ddc_ui::widgets::{self, Button, NumberField, NumberFormat, TextField};
use ddc_ui::Theme;

/// Width of the swatch-kind dropdown in a row.
const KIND_W: f32 = control::DROPDOWN_W * 0.7;
/// Width of the value cell (dropdown, or the gray / hatch fields) and of the label field.
const VALUE_W: f32 = control::DROPDOWN_W;
/// Width of one number field in a row.
const NUM_W: f32 = control::FIELD_MIN_W * 1.5;

/// Stored gray 0..=255 shown as a percentage of white.
pub(super) fn gray_pct(g: u8) -> f64 {
    (f64::from(g) / 255.0 * 100.0).round()
}

pub(super) fn pct_gray(p: f64) -> u8 {
    (p.clamp(0.0, 100.0) / 100.0 * 255.0).round() as u8
}

/// A legend is made by the Create button or Enter in the name field; leaving the field (a press
/// on Create itself, or elsewhere) commits the text too and must not make a second legend.
pub(super) fn creates(button: bool, committed: bool, enter: bool, name: &str) -> bool {
    button || (committed && enter && !name.trim().is_empty())
}

/// What the body below the name row shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Pane {
    /// No legends at all: the empty note.
    Empty,
    /// Legends exist but none is chosen: a hint to pick one.
    Pick,
    /// A legend is chosen: scale, rows, placement.
    Detail,
}

pub(super) fn pane(legends: usize, chosen: bool) -> Pane {
    if chosen {
        Pane::Detail
    } else if legends == 0 {
        Pane::Empty
    } else {
        Pane::Pick
    }
}

/// The name Create would use (typed, or the default for an empty field) when a legend already
/// has it; legends are told apart by name, ignoring case and outer spaces.
pub(super) fn name_taken(existing: &[&str], typed: &str, default: &str) -> Option<String> {
    let typed = typed.trim();
    let name = if typed.is_empty() { default.trim() } else { typed };
    existing
        .iter()
        .any(|e| e.trim().to_lowercase() == name.to_lowercase())
        .then(|| name.to_string())
}

/// "From view" replaces the rows, so existing rows need an answer first.
pub(super) fn from_view_needs_confirm(rows: &[LegendRow]) -> bool {
    !rows.is_empty()
}

/// Dropdown over `(value, caption)` pairs; true when the pick moved.
fn pick<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    th: &Theme,
    salt: impl std::hash::Hash,
    cur: &mut T,
    items: &[(T, String)],
    width: f32,
) -> bool {
    let caps: Vec<&str> = items.iter().map(|x| x.1.as_str()).collect();
    let mut at = items.iter().position(|x| x.0 == *cur).unwrap_or(0);
    let r = widgets::dropdown(ui, th, salt, &mut at, &caps, Some(width));
    if r.changed() {
        if let Some(it) = items.get(at) {
            *cur = it.0;
            return true;
        }
    }
    false
}

/// Quiet destructive delete icon.
fn icon_del(ui: &mut egui::Ui, th: &Theme, tip: &str) -> bool {
    let ctx = ui.ctx().clone();
    widgets::with_icons(&ctx, |icons| {
        widgets::icon_button_danger(ui, icons, th, "delete", tip)
    })
    .clicked()
}

/// Muted column / field caption; returns its id for the field it names.
fn caption(ui: &mut egui::Ui, th: &Theme, text: &str) -> egui::Id {
    let t = egui::RichText::new(text)
        .font(th.font(Role::TableHeader))
        .color(th.sem().text_secondary);
    ui.label(t).id
}

/// A number field named by `label`; true once the edit is committed.
#[allow(clippy::too_many_arguments)]
fn num(
    ui: &mut egui::Ui,
    th: &Theme,
    nf: NumberFormat,
    salt: impl std::hash::Hash,
    v: &mut f64,
    (lo, hi): (f64, f64),
    unit: &str,
    label: (egui::Id, &str),
) -> bool {
    NumberField::new(salt, v, nf)
        .decimals(if unit == "°" || unit == "%" { 0 } else { 1 })
        .range(lo, hi)
        .unit(unit)
        .width(NUM_W)
        .labelled_by(label.0, label.1)
        .show(ui, th)
        .committed
}

const HATCH_DEG: f64 = 45.0;
const HATCH_STEP_MM: f64 = 2.0;
const SOLID_GRAY: u8 = 128;

#[derive(Default)]
pub(super) struct LegendUi {
    pub open: bool,
    sel: Option<i64>,
    rows: Vec<LegendRow>,
    /// Legend and model revision the row buffer was taken from.
    rows_of: Option<(i64, (u64, u64))>,
    new_name: String,
    sheet: Option<i64>,
    /// Sheet for which `x`/`y` were defaulted: a new sheet gets a new frame corner.
    pos_of: Option<i64>,
    x: f64,
    y: f64,
    /// "From view" waits for the answer to the replace question.
    confirm_view: bool,
}

enum Act {
    Create(String),
    Delete(i64),
    Scale(i64, String, f64),
    Rows(i64, Vec<LegendRow>),
    Place(i64, i64, (f64, f64)),
    Move(i64, (f64, f64)),
    Remove(i64),
}

fn default_pos(ed: &Editor, sheet: i64) -> Option<(f64, f64)> {
    let s = ed.model.sheet(sheet).ok()?;
    ddc_io::sheets::legend_default_pos(&s.paper, s.landscape, &s.standard).ok()
}

#[derive(Clone, Copy, PartialEq)]
enum SwatchKind {
    None,
    Line,
    Solid,
    Mask,
    Hatch,
    Detail,
    Wall,
}

fn kind_of(s: LegendSwatch) -> SwatchKind {
    match s {
        LegendSwatch::None => SwatchKind::None,
        LegendSwatch::Line(_) => SwatchKind::Line,
        LegendSwatch::Region(RegionFill::Solid { .. }) => SwatchKind::Solid,
        LegendSwatch::Region(RegionFill::Mask) => SwatchKind::Mask,
        LegendSwatch::Region(RegionFill::Hatch { .. }) => SwatchKind::Hatch,
        LegendSwatch::Detail(_) => SwatchKind::Detail,
        LegendSwatch::WallType(_) => SwatchKind::Wall,
    }
}

struct Lists {
    kinds: Vec<(SwatchKind, String)>,
    lines: Vec<(i64, String)>,
    defs: Vec<(i64, String)>,
    walls: Vec<(i64, String)>,
}

impl Lists {
    fn new(c: &Catalog, ed: &Editor) -> Self {
        Self {
            kinds: vec![
                (SwatchKind::None, c.t("annot_text.swatch.none")),
                (SwatchKind::Line, c.t("annot_text.swatch.line")),
                (SwatchKind::Solid, c.t("annot_text.swatch.solid")),
                (SwatchKind::Mask, c.t("annot_text.swatch.mask")),
                (SwatchKind::Hatch, c.t("annot_text.swatch.hatch")),
                (SwatchKind::Detail, c.t("annot_text.swatch.detail")),
                (SwatchKind::Wall, c.t("annot_text.swatch.wall")),
            ],
            lines: ed
                .model
                .line_styles()
                .unwrap_or_default()
                .into_iter()
                .map(|s| (s.id, name_of(c, "annot_text.lstyle", &s.name)))
                .collect(),
            defs: ed
                .model
                .detail_defs()
                .unwrap_or_default()
                .into_iter()
                .map(|d| (d.id, name_of(c, "annot_text.detail", &d.name)))
                .collect(),
            walls: ddc_io::drawing_model::wall_types(&ed.model)
                .unwrap_or_default()
                .into_iter()
                .map(|w| (w.id, w.name))
                .collect(),
        }
    }

    /// A new sample of the selected kind; kinds with no available data get none.
    fn swatch(&self, k: SwatchKind) -> LegendSwatch {
        let first = |v: &[(i64, String)]| v.first().map(|x| x.0);
        match k {
            SwatchKind::None => LegendSwatch::None,
            SwatchKind::Line => first(&self.lines).map_or(LegendSwatch::None, LegendSwatch::Line),
            SwatchKind::Solid => LegendSwatch::Region(RegionFill::Solid { gray: SOLID_GRAY }),
            SwatchKind::Mask => LegendSwatch::Region(RegionFill::Mask),
            SwatchKind::Hatch => LegendSwatch::Region(RegionFill::Hatch {
                deg: HATCH_DEG,
                step_mm: HATCH_STEP_MM,
                cross: false,
            }),
            SwatchKind::Detail => {
                first(&self.defs).map_or(LegendSwatch::None, LegendSwatch::Detail)
            }
            SwatchKind::Wall => {
                first(&self.walls).map_or(LegendSwatch::None, LegendSwatch::WallType)
            }
        }
    }
}

pub(super) fn show(ctx: &egui::Context, ed: &mut Editor) {
    let Some(mut st) = with(|s| s.legend.open.then(|| std::mem::take(&mut s.legend))) else {
        return;
    };
    let c = super::cat();
    let mut open = true;
    let mut acts = vec![];
    crate::ui_kit::Dialog::new("annot.legend", c.t("annot_text.legend.title"), crate::ui_kit::DialogSize::Tool)
        .open(&mut open)
        .show(ctx, |ui| body(ui, &c, ed, &mut st, &mut acts));
    st.open = open;
    let mut created = false;
    for a in acts {
        created |= matches!(a, Act::Create(_));
        let gone = match &a {
            Act::Delete(id) => ed
                .model
                .legends()
                .unwrap_or_default()
                .into_iter()
                .find(|l| l.id == *id)
                .map(|l| l.name),
            _ => None,
        };
        let del = match &a {
            Act::Delete(id) => Some(*id),
            _ => None,
        };
        apply(ed, a);
        // a refused delete keeps its error line and must not offer Undo of an older step
        let done = del.is_some_and(|id| {
            ed.model.legends().is_ok_and(|ls| !ls.iter().any(|l| l.id == id))
        });
        if let Some(name) = gone.filter(|_| done) {
            ed.say(
                crate::status::Severity::Info,
                c.tf("legend.legend.deleted", &[("name", &name)]),
            );
            ed.offer_undo();
        }
    }
    if created {
        st.sel = ed
            .model
            .legends()
            .unwrap_or_default()
            .iter()
            .map(|l| l.id)
            .max();
        st.new_name.clear();
    }
    with(|s| s.legend = st);
}

fn apply(ed: &mut Editor, a: Act) {
    match a {
        Act::Create(name) => op(ed, move |m| m.add_legend(&name, 100.0).map(|_| ())),
        Act::Delete(id) => op(ed, move |m| m.delete_legend(id)),
        Act::Scale(id, name, sc) => op(ed, move |m| m.update_legend(id, &name, sc)),
        Act::Rows(id, rows) => op(ed, move |m| m.set_legend_rows(id, &rows)),
        Act::Place(sheet, id, pos) => op(ed, move |m| m.place_legend(sheet, id, pos).map(|_| ())),
        Act::Move(id, pos) => op(ed, move |m| m.move_legend_place(id, pos)),
        Act::Remove(id) => op(ed, move |m| m.remove_legend_place(id)),
    }
}

fn body(ui: &mut egui::Ui, c: &Catalog, ed: &Editor, st: &mut LegendUi, acts: &mut Vec<Act>) {
    let legends = ed.model.legends().unwrap_or_default();
    if !st.sel.is_some_and(|id| legends.iter().any(|l| l.id == id)) {
        st.sel = legends.first().map(|l| l.id);
    }
    let th = ddc_ui::theme::of(ui.ctx());
    let th = &*th;
    ui.horizontal(|ui| {
        if !legends.is_empty() {
            caption(ui, th, &c.t("legend.legend.pick"));
            let list: Vec<(Option<i64>, String)> = legends
                .iter()
                .map(|l| (Some(l.id), l.name.clone()))
                .collect();
            pick(ui, th, "f4_leg_sel", &mut st.sel, &list, VALUE_W);
            if let Some(id) = st.sel {
                let sheets = placed_on(ed, id);
                let tip = if sheets.is_empty() {
                    c.t("legend.legend.delete_tip")
                } else {
                    c.tf("legend.legend.delete_placed", &[("sheets", &sheets.join(", "))])
                };
                if icon_del(ui, th, &tip) {
                    acts.push(Act::Delete(id));
                }
            }
        }
    });
    let default_name = c.t("annot_text.legend.default_name");
    let names: Vec<&str> = legends.iter().map(|l| l.name.as_str()).collect();
    let taken = name_taken(&names, &st.new_name, &default_name);
    let reason = taken
        .as_deref()
        .map(|n| c.tf("legend.legend.taken", &[("name", n)]));
    ui.horizontal(|ui| {
        let hint = c.t("legend.legend.new_hint");
        let name = TextField::new(&mut st.new_name)
            .id_salt("ux51a.leg.new")
            .hint(&hint)
            .width(VALUE_W)
            .labelled_by(egui::Id::new("ux51a.leg.new.lbl"), &hint)
            .show(ui, th);
        let label = c.t("annot_text.legend.create");
        let made = ui
            .add_enabled_ui(reason.is_none(), |ui| Button::primary(&label).show(ui, th))
            .inner;
        let make = match &reason {
            Some(r) => made.on_disabled_hover_text(r).clicked(),
            None => made.clicked(),
        };
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        if reason.is_none() && creates(make, name.committed, enter, &st.new_name) {
            let n = if st.new_name.trim().is_empty() {
                default_name.clone()
            } else {
                st.new_name.trim().to_string()
            };
            acts.push(Act::Create(n));
        }
    });
    if let Some(r) = &reason {
        ui.label(egui::RichText::new(r).color(th.sem().text_secondary));
    }
    let chosen = st.sel.and_then(|id| legends.iter().find(|l| l.id == id));
    let Some(def) = chosen else {
        match pane(legends.len(), false) {
            Pane::Pick => {
                ui.add_space(space::SM);
                ui.label(
                    egui::RichText::new(c.t("legend.legend.pick_hint"))
                        .color(th.sem().text_secondary),
                );
            }
            _ => {
                widgets::icon_empty_state(
                    ui,
                    "legend",
                    &c.t("annot_text.legend.empty"),
                    &c.t("legend.legend.empty_hint"),
                    None,
                );
            }
        }
        return;
    };
    ui.add_space(space::XS);
    ui.horizontal(|ui| {
        caption(ui, th, &c.t("annot_text.opt.scale"));
        let mut sc = def.scale;
        let scales: Vec<(f64, String)> = SCALES.iter().map(|s| (*s, format!("1:{s}"))).collect();
        if pick(ui, th, "f4_leg_scale", &mut sc, &scales, KIND_W) {
            acts.push(Act::Scale(def.id, def.name.clone(), sc));
        }
    });
    ui.separator();
    rows(ui, c, th, ed, st, def.id, acts);
    ui.separator();
    placement(ui, c, th, ed, st, def.id, acts);
}

/// "number name" of every sheet this legend is placed on.
fn placed_on(ed: &Editor, id: i64) -> Vec<String> {
    ed.model
        .sheets()
        .unwrap_or_default()
        .into_iter()
        .filter(|(sid, _)| {
            ed.model
                .legend_places(*sid)
                .unwrap_or_default()
                .iter()
                .any(|p| p.legend == id)
        })
        .map(|(_, s)| format!("{} {}", s.number, s.name))
        .collect()
}

fn rows(
    ui: &mut egui::Ui,
    c: &Catalog,
    th: &Theme,
    ed: &Editor,
    st: &mut LegendUi,
    id: i64,
    acts: &mut Vec<Act>,
) {
    let key = ed.cache_key();
    if st.rows_of != Some((id, key)) {
        st.rows = ed.model.legend_rows(id).unwrap_or_default();
        st.rows_of = Some((id, key));
    }
    let l = Lists::new(c, ed);
    let mut changed = false;
    let mut remove = None;
    let nf = NumberFormat::for_lang(c.lang);
    let (h_kind, h_val, h_label) = (
        c.t("legend.col.swatch"),
        c.t("legend.col.value"),
        c.t("legend.col.label"),
    );
    let remove_tip = c.t("annot_text.legend.remove");
    if st.rows.is_empty() {
        ui.label(
            egui::RichText::new(c.t("legend.legend.no_rows")).color(th.sem().text_secondary),
        );
    } else {
        egui::Grid::new("f4_leg_rows")
            .spacing([space::SM, space::XS])
            .show(ui, |ui| {
                caption(ui, th, &h_kind);
                let val_id = caption(ui, th, &h_val);
                let lbl_id = caption(ui, th, &h_label);
                ui.end_row();
                for (i, r) in st.rows.iter_mut().enumerate() {
                    let named = |h: &str| format!("{h} {}", i + 1);
                    changed |= swatch_cell(ui, th, nf, c, &l, i, &mut r.swatch, (val_id, &named(&h_val)));
                    let out = TextField::new(&mut r.label)
                        .id_salt(("ux51a.leg.lbl", i))
                        .width(VALUE_W)
                        .labelled_by(lbl_id, &named(&h_label))
                        .show(ui, th);
                    changed |= out.committed;
                    if icon_del(ui, th, &remove_tip) {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
    }
    if let Some(i) = remove {
        st.rows.remove(i);
        changed = true;
    }
    ui.add_space(space::XS);
    ui.horizontal(|ui| {
        if Button::secondary(&c.t("annot_text.legend.add_row")).show(ui, th).clicked() {
            st.rows.push(LegendRow {
                swatch: l.swatch(SwatchKind::Line),
                label: c.t("annot_text.legend.default_label"),
            });
            changed = true;
        }
        let from = Button::secondary(&c.t("annot_text.legend.from_view")).show(ui, th).clicked();
        if from && from_view_needs_confirm(&st.rows) {
            st.confirm_view = true;
        } else if from {
            changed |= replace_from_view(c, ed, &mut st.rows);
        }
    });
    if st.confirm_view {
        ui.horizontal(|ui| {
            let q = c.tf("legend.legend.replace_q", &[("n", &st.rows.len().to_string())]);
            ui.label(egui::RichText::new(q).color(th.sem().text_secondary));
            if Button::danger(&c.t("legend.legend.replace")).show(ui, th).clicked() {
                st.confirm_view = false;
                changed |= replace_from_view(c, ed, &mut st.rows);
            }
            if Button::quiet(&c.t("legend.legend.keep")).show(ui, th).clicked() {
                st.confirm_view = false;
            }
        });
    }
    if changed && ed.model.legend_rows(id).unwrap_or_default() != st.rows {
        acts.push(Act::Rows(id, st.rows.clone()));
    }
}

/// "From view": rows from the styles of the current plan; false when nothing changed.
fn replace_from_view(c: &Catalog, ed: &Editor, rows: &mut Vec<LegendRow>) -> bool {
    *rows = ed
        .model
        .legend_rows_from_view(AnnoView::Plan(ed.level))
        .unwrap_or_default();
    // the row gets the sheet-facing name, not the internal style key
    for r in rows.iter_mut() {
        match r.swatch {
            LegendSwatch::Line(_) => r.label = name_of(c, "annot_text.lstyle", &r.label),
            LegendSwatch::Detail(_) => r.label = name_of(c, "annot_text.detail", &r.label),
            _ => {}
        }
    }
    true
}

/// Kind and value cells of one row (two grid columns); true when the swatch changed.
#[allow(clippy::too_many_arguments)]
fn swatch_cell(
    ui: &mut egui::Ui,
    th: &Theme,
    nf: NumberFormat,
    c: &Catalog,
    l: &Lists,
    i: usize,
    s: &mut LegendSwatch,
    label: (egui::Id, &str),
) -> bool {
    let mut changed = false;
    let mut k = kind_of(*s);
    if pick(ui, th, ("f4_leg_kind", i), &mut k, &l.kinds, KIND_W) {
        *s = l.swatch(k);
        changed = true;
    }
    let salt = ("f4_leg_val", i);
    ui.horizontal(|ui| match s {
        LegendSwatch::Line(v) => changed |= pick(ui, th, salt, v, &l.lines, VALUE_W),
        LegendSwatch::Detail(v) => changed |= pick(ui, th, salt, v, &l.defs, VALUE_W),
        LegendSwatch::WallType(v) => changed |= pick(ui, th, salt, v, &l.walls, VALUE_W),
        LegendSwatch::Region(RegionFill::Solid { gray }) => {
            let before = gray_pct(*gray);
            let mut pct = before;
            let done = num(ui, th, nf, ("f4_leg_gray", i), &mut pct, (0.0, 100.0), "%", label);
            if done && pct != before {
                *gray = pct_gray(pct);
                changed = true;
            }
            chip(ui, th, *gray);
        }
        LegendSwatch::Region(RegionFill::Hatch { deg, step_mm, .. }) => {
            let a = num(ui, th, nf, ("f4_leg_deg", i), deg, (-180.0, 180.0), "°", label);
            let unit = c.t("annot_text.unit.mm");
            let b = num(ui, th, nf, ("f4_leg_step", i), step_mm, (0.2, 50.0), &unit, label);
            changed |= a || b;
        }
        _ => {}
    });
    changed
}

/// Live gray chip next to the percentage field.
fn chip(ui: &mut egui::Ui, th: &Theme, gray: u8) {
    let side = th.d().icon;
    let (r, _) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::hover());
    ui.painter()
        .rect_filled(r, th.radius("sm"), egui::Color32::from_gray(gray));
    ui.painter().rect_stroke(
        r,
        th.radius("sm"),
        egui::Stroke::new(ddc_ui::theme::stroke::HAIRLINE, th.sem().border_subtle),
        egui::StrokeKind::Inside,
    );
}

fn placement(
    ui: &mut egui::Ui,
    c: &Catalog,
    th: &Theme,
    ed: &Editor,
    st: &mut LegendUi,
    id: i64,
    acts: &mut Vec<Act>,
) {
    let sheets: Vec<(Option<i64>, String)> = ed
        .model
        .sheets()
        .unwrap_or_default()
        .into_iter()
        .map(|(sid, s)| (Some(sid), format!("{} {}", s.number, s.name)))
        .collect();
    if !sheets.iter().any(|x| x.0 == st.sheet) {
        st.sheet = sheets.first().and_then(|x| x.0);
    }
    if st.sheet.is_none() {
        ui.label(c.t("annot_text.legend.no_sheets"));
        return;
    }
    let unit = c.t("annot_text.unit.mm");
    let nf = NumberFormat::for_lang(c.lang);
    ui.horizontal(|ui| {
        caption(ui, th, &c.t("annot_text.legend.sheet"));
        pick(ui, th, "f4_leg_sheet", &mut st.sheet, &sheets, VALUE_W);
        if st.pos_of != st.sheet {
            st.pos_of = st.sheet;
            if let Some(p) = st.sheet.and_then(|id| default_pos(ed, id)) {
                (st.x, st.y) = p;
            }
        }
        let (xl, yl) = (c.t("annot_text.legend.x"), c.t("annot_text.legend.y"));
        let xid = caption(ui, th, &xl);
        num(ui, th, nf, "ux51a.leg.x", &mut st.x, (0.0, 2000.0), &unit, (xid, &xl));
        let yid = caption(ui, th, &yl);
        num(ui, th, nf, "ux51a.leg.y", &mut st.y, (0.0, 2000.0), &unit, (yid, &yl));
        if let (Some(sheet), true) = (
            st.sheet,
            Button::primary(&c.t("annot_text.legend.place")).show(ui, th).clicked(),
        ) {
            acts.push(Act::Place(sheet, id, (st.x, st.y)));
        }
    });
    let remove_tip = c.t("annot_text.legend.remove");
    for (sid, name) in sheets.iter().filter_map(|(s, n)| s.map(|s| (s, n))) {
        for p in ed
            .model
            .legend_places(sid)
            .unwrap_or_default()
            .iter()
            .filter(|p| p.legend == id)
        {
            ui.push_id(p.id, |ui| {
                ui.horizontal(|ui| {
                    let lbl = format!("{name} \u{b7}");
                    let lid = ui
                        .allocate_ui(egui::vec2(VALUE_W, th.d().control), |ui| {
                            widgets::elided_label(
                                ui,
                                &lbl,
                                th.font(Role::Body),
                                th.sem().text_primary,
                            )
                        })
                        .inner
                        .id;
                    let (mut x, mut y) = p.pos;
                    let a = num(ui, th, nf, ("ux51a.pl.x", p.id), &mut x, (0.0, 2000.0), &unit, (lid, &lbl));
                    let b = num(ui, th, nf, ("ux51a.pl.y", p.id), &mut y, (0.0, 2000.0), &unit, (lid, &lbl));
                    if a || b {
                        acts.push(Act::Move(p.id, (x, y)));
                    }
                    if icon_del(ui, th, &remove_tip) {
                        acts.push(Act::Remove(p.id));
                    }
                });
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::Lang;

    #[test]
    fn gray_percent_round_trips_at_the_ends_and_middle() {
        for g in [0u8, 128, 255] {
            assert_eq!(pct_gray(gray_pct(g)), g);
        }
        assert_eq!(gray_pct(255), 100.0);
        assert_eq!(pct_gray(150.0), 255);
        assert_eq!(pct_gray(-5.0), 0);
    }

    #[test]
    fn leaving_the_name_field_does_not_create_a_legend() {
        // press on Create: the field commits on that frame, the click lands one frame later
        assert!(!creates(false, true, false, "Doors"));
        assert!(creates(true, false, false, "Doors"));
        assert!(creates(false, true, true, "Doors"));
        assert!(!creates(false, true, true, "  "));
    }

    #[test]
    fn from_view_asks_only_when_rows_exist() {
        let row = LegendRow {
            swatch: LegendSwatch::None,
            label: String::new(),
        };
        assert!(!from_view_needs_confirm(&[]));
        assert!(from_view_needs_confirm(&[row]));
    }

    #[test]
    fn win_source_uses_kit_widgets_only() {
        let src = include_str!("win.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        for bad in [
            "egui::Com\u{62}oBox",
            "egui::Drag\u{56}alue",
            "ui.but\u{74}on(",
            "super::com\u{62}o(",
            "super::nu\u{6d}(",
        ] {
            assert!(!code.contains(bad), "{bad}");
        }
    }

    #[test]
    fn window_draws_rows_and_placement_headless() {
        with(|s| *s = super::super::State::default());
        let mut ed = Editor::new_in_memory().unwrap();
        let lid = ed.model.add_legend("A", 100.0).unwrap();
        ed.model
            .set_legend_rows(
                lid,
                &[
                    LegendRow {
                        swatch: LegendSwatch::Region(RegionFill::Solid { gray: 128 }),
                        label: "a".into(),
                    },
                    LegendRow {
                        swatch: LegendSwatch::Region(RegionFill::Hatch {
                            deg: HATCH_DEG,
                            step_mm: HATCH_STEP_MM,
                            cross: false,
                        }),
                        label: "b".into(),
                    },
                ],
            )
            .unwrap();
        with(|s| {
            s.legend.open = true;
            s.legend.confirm_view = true;
        });
        let ctx = ddc_ui::theme::headless_ctx();
        for _ in 0..3 {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 720.0),
                    )),
                    ..Default::default()
                },
                |c| show(c, &mut ed),
            );
        }
        assert!(with(|s| s.legend.open));
        assert_eq!(ed.model.legend_rows(lid).unwrap().len(), 2);
    }

    /// Texts and buttons (label, disabled) of the last of several headless frames, in `lang`.
    fn frame(lang: Lang, ed: &mut Editor, typed: &str) -> (Vec<String>, Vec<(String, bool)>) {
        fn walk(s: &egui::Shape, out: &mut Vec<String>) {
            match s {
                egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
                egui::Shape::Vec(v) => v.iter().for_each(|x| walk(x, out)),
                _ => {}
            }
        }
        with(|s| *s = super::super::State::default());
        with(|s| {
            s.legend.open = true;
            s.legend.new_name = typed.to_string();
        });
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx.enable_accesskit();
        let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
        let (mut texts, mut buttons) = (vec![], vec![]);
        for _ in 0..4 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 1000.0),
                )),
                ..Default::default()
            };
            let out = ctx.run(raw, |c| {
                let _ = th.apply(c);
                super::super::window(c, ed, lang);
            });
            texts.clear();
            buttons.clear();
            out.shapes.iter().for_each(|cs| walk(&cs.shape, &mut texts));
            if let Some(u) = out.platform_output.accesskit_update {
                for (_, n) in u.nodes {
                    if n.role() == egui::accesskit::Role::Button {
                        buttons.push((n.label().unwrap_or("").to_string(), n.is_disabled()));
                    }
                }
            }
        }
        (texts, buttons)
    }

    fn has(texts: &[String], s: &str) -> bool {
        texts.iter().any(|t| t.contains(s))
    }

    #[test]
    fn no_legends_shows_the_empty_note_only() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let mut ed = Editor::new_in_memory().unwrap();
            let (texts, _) = frame(lang, &mut ed, "");
            assert!(has(&texts, &c.t("annot_text.legend.empty")), "{lang:?}");
            assert!(has(&texts, &c.t("legend.legend.empty_hint")), "{lang:?}");
            assert!(!has(&texts, &c.t("legend.legend.pick_hint")), "{lang:?}");
        }
    }

    #[test]
    fn existing_legends_never_show_the_empty_note() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let mut ed = Editor::new_in_memory().unwrap();
            ed.model.add_legend("Doors", 100.0).unwrap();
            let (texts, _) = frame(lang, &mut ed, "");
            assert!(!has(&texts, &c.t("annot_text.legend.empty")), "{lang:?}");
            assert!(!has(&texts, &c.t("legend.legend.empty_hint")), "{lang:?}");
        }
    }

    #[test]
    fn pane_follows_legends_and_choice() {
        assert_eq!(pane(0, false), Pane::Empty);
        assert_eq!(pane(3, false), Pane::Pick);
        assert_eq!(pane(3, true), Pane::Detail);
        assert_eq!(pane(1, true), Pane::Detail);
    }

    #[test]
    fn taken_names_ignore_case_and_outer_spaces() {
        let have = ["Doors", " Walls "];
        assert_eq!(name_taken(&have, "doors", "Legend"), Some("doors".into()));
        assert_eq!(name_taken(&have, "  WALLS", "Legend"), Some("WALLS".into()));
        assert_eq!(name_taken(&have, "Floors", "Legend"), None);
        // an empty field creates the default name: taken when that exists
        assert_eq!(name_taken(&["Legend"], "  ", "Legend"), Some("Legend".into()));
        assert_eq!(name_taken(&have, "", "Legend"), None);
        assert_eq!(name_taken(&[], "A", "Legend"), None);
    }

    #[test]
    fn create_is_disabled_with_a_reason_on_a_taken_name_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let create = c.t("annot_text.legend.create");
            let mut ed = Editor::new_in_memory().unwrap();
            ed.model.add_legend("Doors", 100.0).unwrap();
            let reason = c.tf("legend.legend.taken", &[("name", "doors")]);

            let (texts, buttons) = frame(lang, &mut ed, "doors");
            assert!(
                buttons.iter().any(|(l, off)| *l == create && *off),
                "{lang:?}: {buttons:?}"
            );
            assert!(has(&texts, &reason), "{lang:?}");

            let (texts, buttons) = frame(lang, &mut ed, "Walls");
            assert!(
                buttons.iter().any(|(l, off)| *l == create && !*off),
                "{lang:?}: {buttons:?}"
            );
            assert!(!has(&texts, &c.tf("legend.legend.taken", &[("name", "Walls")])));
        }
    }

    #[test]
    fn create_is_enabled_for_a_first_legend_with_an_empty_name() {
        let c = Catalog::new(Lang::En);
        let mut ed = Editor::new_in_memory().unwrap();
        let (_, buttons) = frame(Lang::En, &mut ed, "");
        assert!(buttons.iter().any(|(l, off)| *l == c.t("annot_text.legend.create") && !*off));
    }

    #[test]
    fn create_is_the_primary_action() {
        let code = include_str!("win.rs").split("#[cfg(test)]").next().unwrap();
        assert!(code.contains("Button::primary(&label)"));
        assert!(!code.contains("Button::secondary(&c.t(\"annot_text.legend.create\"))"));
    }

    #[test]
    fn esc_closes_the_legend_window_without_creating_one() {
        with(|s| *s = super::super::State::default());
        let mut ed = Editor::new_in_memory().unwrap();
        let n = ed.model.legends().unwrap().len();
        with(|s| s.legend.open = true);
        crate::ui_kit::run_esc_frames(|c| show(c, &mut ed));
        assert!(!with(|s| s.legend.open));
        assert_eq!(ed.model.legends().unwrap().len(), n);
    }
}
