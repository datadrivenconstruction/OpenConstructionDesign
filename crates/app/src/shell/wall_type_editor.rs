// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Wall type editor: the layer table (function, material, thickness, priority) with add,
//! remove, duplicate and reorder (drag or arrows), a live section with the total thickness,
//! the face that placed walls keep, rename and "Duplicate type". OK writes one undo step.
use super::type_dialog::{material_label, MATERIALS};
use crate::editor::{Editor, OpFn};
use crate::ui_kit as kit;
use crate::units::{edit_length, parse_edit_length, unit_label_in, Units};
use ddc_model::wall_types::{self as wt, Align, LayerFn, TypeLayer};
use ddc_model::Model;
use ddc_ui::theme::{control, layout, space, stroke, Role};
use ddc_ui::{widgets, Catalog, Lang, Theme};
use egui::{self, RichText};
use std::cell::RefCell;
use std::sync::Arc;

/// Most layers a type may hold (the model refuses more).
pub const MAX_LAYERS: usize = 24;
const PRIORITIES: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// One row of the table as typed.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub material: String,
    pub thickness: String,
    pub function: LayerFn,
    pub priority: u8,
}

/// The type being edited; nothing reaches the model before OK.
#[derive(Clone, Debug)]
pub struct WallTypeDraft {
    pub id: i64,
    pub name: String,
    pub rows: Vec<Row>,
    pub align: Align,
    pub units: Units,
    /// Placed walls of the type.
    pub walls: usize,
    /// Why the last OK failed.
    pub error: Option<String>,
    stored_name: String,
    stored: Vec<TypeLayer>,
    conn: usize,
}

fn conn_of(m: &Model) -> usize {
    m.sql() as *const rusqlite::Connection as usize
}

impl WallTypeDraft {
    pub fn load(m: &Model, id: i64, u: Units) -> Result<Self, String> {
        let e = |e: ddc_model::Error| e.to_string();
        let name = m
            .wall_type_usage()
            .map_err(e)?
            .into_iter()
            .find(|t| t.id == id)
            .map(|t| t.name)
            .ok_or_else(|| format!("no wall type {id}"))?;
        let stored = m.wall_type_layers_full(id).map_err(e)?;
        let rows = stored
            .iter()
            .map(|l| Row {
                material: l.material.clone(),
                thickness: edit_length(l.thickness, u),
                function: l.function,
                priority: l.priority,
            })
            .collect();
        Ok(WallTypeDraft {
            id,
            name: name.clone(),
            rows,
            align: Align::Core,
            units: u,
            walls: m.walls_of_type(id).map_err(e)?.len(),
            error: None,
            stored_name: name,
            stored,
            conn: conn_of(m),
        })
    }

    pub fn stored_name(&self) -> &str {
        &self.stored_name
    }

    /// Thickness of row `i` in metres, when it reads as a positive length.
    pub fn thickness(&self, i: usize) -> Option<f64> {
        let r = self.rows.get(i)?;
        parse_edit_length(&r.thickness, self.units).filter(|t| *t > 0.0 && t.is_finite())
    }

    /// What is wrong with row `i`, in the given language.
    pub fn row_problem(&self, i: usize, ru: bool) -> Option<&'static str> {
        let r = self.rows.get(i)?;
        if r.material.trim().is_empty() {
            return Some(crate::tr_str(ru, "wall_type.wt.no_material"));
        }
        self.thickness(i)
            .is_none()
            .then(|| crate::tr_str(ru, "wall_type.wt.bad_thickness"))
    }

    pub fn total(&self) -> Option<f64> {
        (0..self.rows.len()).map(|i| self.thickness(i)).sum()
    }

    pub fn layers(&self) -> Option<Vec<TypeLayer>> {
        let mut out = Vec::with_capacity(self.rows.len());
        for (i, r) in self.rows.iter().enumerate() {
            out.push(TypeLayer {
                material: r.material.trim().to_string(),
                thickness: self.thickness(i)?,
                function: r.function,
                priority: r.priority,
            });
        }
        Some(out)
    }

    /// Why OK is off; None: it may write.
    pub fn blocked(&self, m: &Model, ru: bool) -> Option<String> {
        if self.name.trim().is_empty() {
            return Some(crate::tr_str(ru, "wall_type.wt.no_name").into());
        }
        if self.rows.is_empty() {
            return Some(crate::tr_str(ru, "wall_type.wt.no_layers").into());
        }
        if let Some(p) = (0..self.rows.len()).find_map(|i| self.row_problem(i, ru)) {
            return Some(p.into());
        }
        if self.name.trim() != self.stored_name && self.name_taken(m) {
            return Some(crate::tr_str(ru, "wall_type.wt.name_taken").into());
        }
        if !self.dirty() {
            return Some(crate::tr_str(ru, "wall_type.wt.no_changes").into());
        }
        None
    }

    fn name_taken(&self, m: &Model) -> bool {
        let n = self.name.trim();
        m.wall_type_usage()
            .map(|v| v.iter().any(|t| t.id != self.id && t.name == n))
            .unwrap_or(false)
    }

    pub fn dirty(&self) -> bool {
        self.name.trim() != self.stored_name || self.layers().as_ref() != Some(&self.stored)
    }

    /// Placed walls that change thickness on OK (0 when the total stays).
    pub fn resized_walls(&self) -> usize {
        let old = wt::total(&self.stored);
        match self.total() {
            Some(t) if (t - old).abs() > 1e-9 => self.walls,
            _ => 0,
        }
    }

    /// A new layer after `at` (or at the end), a copy of its neighbour's material.
    pub fn add(&mut self, at: Option<usize>) -> bool {
        if self.rows.len() >= MAX_LAYERS {
            return false;
        }
        let k = at.map_or(self.rows.len(), |i| (i + 1).min(self.rows.len()));
        let f = LayerFn::Finish1;
        let row = Row {
            material: "plaster".into(),
            thickness: edit_length(0.02, self.units),
            function: f,
            priority: f.priority(),
        };
        self.rows.insert(k, row);
        true
    }

    pub fn duplicate(&mut self, i: usize) -> bool {
        if i >= self.rows.len() || self.rows.len() >= MAX_LAYERS {
            return false;
        }
        let r = self.rows[i].clone();
        self.rows.insert(i + 1, r);
        true
    }

    /// The last layer stays: a type needs one.
    pub fn remove(&mut self, i: usize) -> bool {
        if i >= self.rows.len() || self.rows.len() < 2 {
            return false;
        }
        self.rows.remove(i);
        true
    }

    /// Moves row `from` to index `to` (drag and drop).
    pub fn move_to(&mut self, from: usize, to: usize) -> bool {
        let n = self.rows.len();
        if from >= n || to >= n || from == to {
            return false;
        }
        let r = self.rows.remove(from);
        self.rows.insert(to, r);
        true
    }

    pub fn shift(&mut self, i: usize, up: bool) -> bool {
        match (up, i) {
            (true, 0) => false,
            (true, _) => self.move_to(i, i - 1),
            (false, _) => self.move_to(i, i + 1),
        }
    }

    /// The model step of OK: name, layers and the alignment rule in one operation.
    pub fn op(&self, ru: bool) -> Result<OpFn, String> {
        let layers = self
            .layers()
            .ok_or_else(|| crate::tr_str(ru, "wall_type.wt.bad_thickness").to_string())?;
        let (id, name, align) = (self.id, self.name.trim().to_string(), self.align);
        wt::check_type(&name, &layers).map_err(|e| e.to_string())?;
        Ok(OpFn(Arc::new(move |m: &mut Model| {
            m.edit_wall_type(id, &name, &layers, align).map(|_| ())
        })))
    }
}

thread_local! {
    static OPEN: RefCell<Option<WallTypeDraft>> = const { RefCell::new(None) };
}

pub fn with<R>(f: impl FnOnce(&mut Option<WallTypeDraft>) -> R) -> R {
    OPEN.with(|o| f(&mut o.borrow_mut()))
}

pub fn is_open() -> bool {
    with(|o| o.is_some())
}

/// Opens the editor on wall type `id`; unsaved edits are kept (false: another type has them).
pub fn open(m: &Model, id: i64, u: Units) -> bool {
    with(|o| {
        if let Some(d) = o.as_ref().filter(|d| d.conn == conn_of(m)) {
            if d.id == id {
                return true;
            }
            if d.dirty() {
                return false;
            }
        }
        match WallTypeDraft::load(m, id, u) {
            Ok(d) => {
                *o = Some(d);
                true
            }
            Err(_) => false,
        }
    })
}

/// Writes the draft as one undo step and reports it; false keeps the window with the reason.
pub fn apply(ed: &mut Editor, d: &mut WallTypeDraft, ru: bool) -> bool {
    let op = match d.op(ru) {
        Ok(op) => op,
        Err(e) => {
            d.error = Some(e);
            return false;
        }
    };
    let resized = d.resized_walls();
    if let Err(e) = ed.run_op(op) {
        d.error = Some(e.to_string());
        return false;
    }
    let n = resized.to_string();
    ed.status = Some(crate::tr_lang(
        ru,
        "wall_type.wt.saved",
        &[("name", d.name.trim()), ("n", n.as_str())],
    ));
    true
}

/// Saves the draft as a new type (the edited one stays as stored); its id.
pub fn duplicate_type(ed: &mut Editor, d: &WallTypeDraft, ru: bool) -> Result<i64, String> {
    let layers = d
        .layers()
        .ok_or_else(|| crate::tr_str(ru, "wall_type.wt.bad_thickness").to_string())?;
    let base = if d.name.trim().is_empty() { d.stored_name.clone() } else { d.name.clone() };
    let name = ed.model.free_wall_type_name(base.trim()).map_err(|e| e.to_string())?;
    let (id, n2) = (d.id, name.clone());
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        let nid = m.duplicate_wall_type(id, &n2)?;
        m.edit_wall_type(nid, &n2, &layers, Align::Core).map(|_| ())
    })))
    .map_err(|e| e.to_string())?;
    let nid = ed
        .model
        .wall_type_usage()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|t| t.name == name)
        .map(|t| t.id)
        .ok_or_else(|| name.clone())?;
    ed.status = Some(crate::tr_lang(ru, "wall_type.wt.duplicated", &[("name", name.as_str())]));
    Ok(nid)
}

pub fn fn_label(ru: bool, f: LayerFn) -> &'static str {
    crate::tr_str(
        ru,
        match f {
            LayerFn::Structure => "wall_type.fn.structure",
            LayerFn::Substrate => "wall_type.fn.substrate",
            LayerFn::Insulation => "wall_type.fn.insulation",
            LayerFn::Air => "wall_type.fn.air",
            LayerFn::Membrane => "wall_type.fn.membrane",
            LayerFn::Finish1 => "wall_type.fn.finish1",
            LayerFn::Finish2 => "wall_type.fn.finish2",
        },
    )
}

fn mat_color(th: &Theme, m: &str) -> egui::Color32 {
    let k = crate::section_view::mat_class(m);
    th.c(&format!("canvas.mat.{}", crate::ui_kit::MAT_CLASSES[k]))
}

fn caption(ui: &mut egui::Ui, th: &Theme, s: &str, w: f32) {
    cell(ui, w, |ui| {
        ui.label(
            RichText::new(s)
                .font(th.font(Role::Label))
                .color(th.c("text.secondary")),
        );
    });
}

/// A cell of fixed width in a row, so headers and rows share their columns.
fn cell<R>(ui: &mut egui::Ui, w: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let h = control::HIT_MIN;
    ui.allocate_ui_with_layout(
        egui::vec2(w, h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(w);
            add(ui)
        },
    )
    .inner
}

#[derive(Clone, Copy)]
struct LayerDrag(usize);

/// Column widths: grip, number, function, material, thickness, priority, actions.
fn columns(avail: f32) -> [f32; 7] {
    let fixed = [
        space::LG,
        space::XL,
        control::DROPDOWN_W,
        0.0,
        layout::PROP_VALUE,
        control::HIT_MIN * 2.5,
        control::HIT_MIN * 4.0 + space::XS * 3.0,
    ];
    let gaps = space::SM * 6.0;
    let used: f32 = fixed.iter().sum::<f32>() + gaps;
    let mut w = fixed;
    w[3] = (avail - used).max(control::DROPDOWN_W);
    w
}

#[derive(Default)]
struct Acts {
    shift: Option<(usize, bool)>,
    add: Option<Option<usize>>,
    dup: Option<usize>,
    del: Option<usize>,
    drop: Option<(usize, usize)>,
}

fn table(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    ru: bool,
    d: &mut WallTypeDraft,
    extra: &[String],
) {
    let lang = if ru { Lang::Ru } else { Lang::En };
    let n = d.rows.len();
    let lu = unit_label_in(d.units, crate::units::lang_of(ru));
    let problems: Vec<Option<&str>> = (0..n).map(|i| d.row_problem(i, ru)).collect();
    let mut ids: Vec<String> = MATERIALS.iter().map(|m| (*m).to_owned()).collect();
    for m in extra.iter().chain(d.rows.iter().map(|r| &r.material)) {
        if !m.trim().is_empty() && !ids.contains(m) {
            ids.push(m.clone());
        }
    }
    let labels: Vec<String> = ids.iter().map(|m| material_label(cat, lang, m)).collect();
    let other = crate::tr_str(ru, "wall_type.wt.other_material");
    let mut mat_items: Vec<&str> = labels.iter().map(String::as_str).collect();
    mat_items.push(other);
    let fn_items: Vec<&str> = LayerFn::ALL.iter().map(|f| fn_label(ru, *f)).collect();
    let mut acts = Acts::default();
    // slack for the scroll bar's strip and the frame's stroke: rows must not widen the window
    let avail = ui.available_width() - space::LG;
    egui::Frame::new()
        .fill(th.c("bg.sunken"))
        .corner_radius(th.radius("md"))
        .inner_margin(egui::Margin::same(space::SM as i8))
        .show(ui, |ui| {
            ui.set_min_width(avail - 2.0 * space::SM);
            let w = columns((avail - 2.0 * space::SM).min(ui.available_width()));
            ui.spacing_mut().item_spacing.x = space::SM;
            ui.horizontal(|ui| {
                let heads = [
                    "",
                    crate::tr_str(ru, "wall_type.wt.h_num"),
                    crate::tr_str(ru, "wall_type.wt.h_function"),
                    crate::tr_str(ru, "wall_type.wt.h_material"),
                    crate::tr_str(ru, "wall_type.wt.h_thickness"),
                    crate::tr_str(ru, "wall_type.wt.h_priority"),
                    "",
                ];
                for (h, w) in heads.iter().zip(w) {
                    caption(ui, th, h, w);
                }
            });
            if n == 0 {
                widgets::empty_state_compact(
                    ui,
                    th,
                    crate::tr_str(ru, "wall_type.wt.no_layers"),
                    crate::tr_str(ru, "wall_type.wt.no_layers_hint"),
                );
            }
            let (lo, hi) = d.layers().map_or((usize::MAX, 0), |l| wt::core_range(&l));
            let lists = Lists {
                ids: &ids,
                mats: &mat_items,
                fns: &fn_items,
                unit: lu,
                core: (lo, hi),
            };
            for i in 0..n {
                let row = ui.horizontal(|ui| row_ui(ui, th, ru, d, i, &w, &lists, &mut acts));
                if let Some(msg) = problems[i] {
                    ui.horizontal(|ui| {
                        ui.add_space(w[0] + w[1] + space::SM * 2.0);
                        let t = RichText::new(msg).font(th.font(Role::Caption));
                        ui.label(t.color(th.c("danger")));
                    });
                }
                let zid = egui::Id::new(("ag3.wt.drop", i));
                let zone = ui.interact(row.response.rect, zid, egui::Sense::hover());
                if zone.dnd_hover_payload::<LayerDrag>().is_some_and(|p| p.0 != i) {
                    let r = row.response.rect;
                    let y = if i == 0 { r.top() } else { r.bottom() };
                    ui.painter().line_segment(
                        [egui::pos2(r.left(), y), egui::pos2(r.right(), y)],
                        egui::Stroke::new(stroke::HAIRLINE * 2.0, th.c("accent")),
                    );
                }
                if let Some(p) = zone.dnd_release_payload::<LayerDrag>() {
                    acts.drop = Some((p.0, i));
                }
            }
            ui.add_space(space::XS);
            let full = n >= MAX_LAYERS;
            let add = ui.add_enabled_ui(!full, |ui| {
                widgets::Button::secondary(crate::tr_str(ru, "wall_type.wt.add_layer")).show(ui, th)
            });
            if add.inner.clicked() && !full {
                acts.add = Some(None);
            }
        });
    if let Some((i, up)) = acts.shift {
        d.shift(i, up);
    }
    if let Some(at) = acts.add {
        d.add(at);
    }
    if let Some(i) = acts.dup {
        d.duplicate(i);
    }
    if let Some(i) = acts.del {
        d.remove(i);
    }
    if let Some((a, b)) = acts.drop {
        d.move_to(a, b);
    }
}

/// What every row shares: material ids and labels, function labels, the unit, the core.
struct Lists<'a> {
    ids: &'a [String],
    mats: &'a [&'a str],
    fns: &'a [&'a str],
    unit: &'a str,
    core: (usize, usize),
}

#[allow(clippy::too_many_arguments)]
fn row_ui(
    ui: &mut egui::Ui,
    th: &Theme,
    ru: bool,
    d: &mut WallTypeDraft,
    i: usize,
    w: &[f32; 7],
    ls: &Lists<'_>,
    acts: &mut Acts,
) {
    let (ids, core) = (ls.ids, ls.core);
    let n = d.rows.len();
    let bad = d.row_problem(i, ru).is_some();
    let r = &mut d.rows[i];
    cell(ui, w[0], |ui| {
        let size = egui::vec2(w[0], control::HIT_MIN);
        let (g, resp) = ui.allocate_exact_size(size, egui::Sense::drag());
        let resp = resp.on_hover_cursor(egui::CursorIcon::Grab);
        resp.dnd_set_drag_payload(LayerDrag(i));
        let c = th.c(if resp.hovered() { "text.primary" } else { "text.secondary" });
        for k in 0..6 {
            let (dx, dy) = ((k % 2) as f32 * space::XS, (k / 2) as f32 * space::XS);
            let at = g.center() + egui::vec2(dx - space::XXS, dy - space::XS);
            ui.painter().circle_filled(at, stroke::HAIRLINE, c);
        }
    });
    cell(ui, w[1], |ui| {
        let t = format!("{}", i + 1);
        let txt = RichText::new(t).font(th.font(Role::Numeric));
        let in_core = i >= core.0 && i <= core.1;
        ui.label(if in_core { txt.color(th.c("accent")) } else { txt });
    });
    cell(ui, w[2], |ui| {
        let mut sel = LayerFn::ALL.iter().position(|f| *f == r.function).unwrap_or(0);
        if widgets::dropdown(ui, th, ("ag3.wt.fn", i), &mut sel, ls.fns, Some(w[2])).changed() {
            let f = LayerFn::ALL[sel];
            // the priority follows the function until it is set by hand
            if r.priority == r.function.priority() {
                r.priority = f.priority();
            }
            r.function = f;
        }
    });
    cell(ui, w[3], |ui| {
        let flag = egui::Id::new(("ag3.wt.custom", i));
        let custom = ui.data(|m| m.get_temp::<bool>(flag)).unwrap_or(false);
        let other_ix = ids.len();
        let at = ids.iter().position(|m| *m == r.material);
        let mut sel = if custom { other_ix } else { at.unwrap_or(other_ix) };
        let sq = egui::vec2(space::MD, space::MD);
        let (sw, _) = ui.allocate_exact_size(sq, egui::Sense::hover());
        ui.painter().rect_filled(sw, ddc_ui::theme::radius::SM, mat_color(th, &r.material));
        let mw = w[3] - space::MD - space::SM;
        let salt = ("ag3.wt.mat", i);
        if custom || at.is_none() {
            let lbl = egui::Id::new(("ag3.wt.mat.lbl", i));
            widgets::TextField::new(&mut r.material)
                .id_salt(("ag3.wt.mat.text", i))
                .width(mw)
                .labelled_by(lbl, crate::tr_str(ru, "wall_type.wt.h_material"))
                .show(ui, th);
        } else if widgets::dropdown(ui, th, salt, &mut sel, ls.mats, Some(mw)).changed() {
            ui.data_mut(|m| m.insert_temp(flag, sel == other_ix));
            if let Some(id) = ids.get(sel) {
                r.material = id.clone();
            }
        }
    });
    cell(ui, w[4], |ui| {
        widgets::TextField::new(&mut r.thickness)
            .id_salt(("ag3.wt.th", i))
            .width(w[4])
            .unit(ls.unit)
            .right_aligned()
            .error(bad)
            .labelled_by(
                egui::Id::new(("ag3.wt.th.lbl", i)),
                crate::tr_str(ru, "wall_type.wt.h_thickness"),
            )
            .show(ui, th);
    });
    cell(ui, w[5], |ui| {
        let mut sel = usize::from(r.priority.clamp(1, 9) - 1);
        let salt = ("ag3.wt.pr", i);
        if widgets::dropdown(ui, th, salt, &mut sel, &PRIORITIES, Some(w[5])).changed() {
            r.priority = sel as u8 + 1;
        }
    });
    cell(ui, w[6], |ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        let tr = |k: &'static str| crate::tr_str(ru, k);
        if widgets::glyph_button(ui, "move-up", i > 0, tr("wall_type.wt.move_up")).clicked() {
            acts.shift = Some((i, true));
        }
        if widgets::glyph_button(ui, "move-down", i + 1 < n, tr("wall_type.wt.move_down")).clicked() {
            acts.shift = Some((i, false));
        }
        if widgets::glyph_button(ui, "copy", n < MAX_LAYERS, tr("wall_type.wt.dup_layer")).clicked() {
            acts.dup = Some(i);
        }
        let del_tip = if n > 1 { tr("wall_type.wt.del_layer") } else { tr("wall_type.wt.del_last") };
        if widgets::glyph_button(ui, "delete", n > 1, del_tip).clicked() {
            acts.del = Some(i);
        }
    });
}

/// The section across the wall: layers to scale, exterior on the left, the core between
/// dashed marks, and the total.
fn section(ui: &mut egui::Ui, th: &Theme, ru: bool, d: &WallTypeDraft) {
    let h = control::HIT_MIN * 3.0;
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, th.radius("sm"), th.c("canvas.paper"));
    let cap = th.font(Role::Caption);
    let muted = th.c("text.secondary");
    let inner = r.shrink2(egui::vec2(space::MD, space::LG));
    p.text(
        egui::pos2(inner.left(), r.top() + space::XXS),
        egui::Align2::LEFT_TOP,
        crate::tr_str(ru, "wall_type.wt.exterior"),
        cap.clone(),
        muted,
    );
    p.text(
        egui::pos2(inner.right(), r.top() + space::XXS),
        egui::Align2::RIGHT_TOP,
        crate::tr_str(ru, "wall_type.wt.interior"),
        cap.clone(),
        muted,
    );
    let ts: Vec<f64> = (0..d.rows.len()).map(|i| d.thickness(i).unwrap_or(0.0)).collect();
    let tot: f64 = ts.iter().sum();
    if tot <= 0.0 {
        return;
    }
    let (lo, hi) = d.layers().map_or((usize::MAX, usize::MAX), |l| wt::core_range(&l));
    let mut x = inner.left();
    let mut marks = vec![];
    for (i, (row, t)) in d.rows.iter().zip(&ts).enumerate() {
        let w = (*t / tot) as f32 * inner.width();
        let at = egui::pos2(x, inner.top());
        let band = egui::Rect::from_min_size(at, egui::vec2(w, inner.height()));
        p.rect_filled(band, 0.0, mat_color(th, &row.material));
        p.rect_stroke(
            band,
            0.0,
            egui::Stroke::new(stroke::HAIRLINE, th.c("canvas.cut")),
            egui::StrokeKind::Inside,
        );
        if i == lo {
            marks.push(x);
        }
        if i == hi {
            marks.push(x + w);
        }
        x += w;
    }
    let dash = egui::Stroke::new(stroke::HAIRLINE * 2.0, th.c("accent"));
    for m in marks {
        let a = egui::pos2(m, r.top() + space::SM);
        let b = egui::pos2(m, r.bottom() - space::SM);
        p.extend(egui::Shape::dashed_line(&[a, b], dash, space::XS, space::XS));
    }
    let total = format!(
        "{} {} {}",
        crate::tr_str(ru, "wall_type.wt.total"),
        edit_length(tot, d.units),
        unit_label_in(d.units, crate::units::lang_of(ru))
    );
    p.text(
        egui::pos2(inner.center().x, r.bottom() - space::XXS),
        egui::Align2::CENTER_BOTTOM,
        total,
        th.font(Role::Numeric),
        th.c("text.primary"),
    );
}

/// Draws the editor; called every frame. The model connection changing closes it.
pub fn show(ctx: &egui::Context, th: &Theme, cat: &Catalog, ru: bool, ed: &mut Editor) {
    let Some(mut d) = with(|o| o.take()) else {
        return;
    };
    if d.conn != conn_of(&ed.model) {
        return;
    }
    let keep = window(ctx, th, cat, ru, ed, &mut d);
    if keep {
        with(|o| *o = Some(d));
    }
}

/// One frame of the editor; false: it closed.
pub fn window(
    ctx: &egui::Context,
    th: &Theme,
    cat: &Catalog,
    ru: bool,
    ed: &mut Editor,
    d: &mut WallTypeDraft,
) -> bool {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let (mut open, mut ok, mut cancel, mut dup) = (true, false, false, false);
    let mut switch: Option<i64> = None;
    let extra = crate::view_gfx_ui::project_materials(ed);
    let types: Vec<(i64, String)> = ed
        .model
        .wall_type_usage()
        .map(|v| v.into_iter().map(|t| (t.id, t.name)).collect())
        .unwrap_or_default();
    let blocked = d.blocked(&ed.model, ru);
    kit::Dialog::new("wall_type_editor", tr("wall_type.wt.title"), kit::DialogSize::Workspace)
        .open(&mut open)
        .show(ctx, |ui| {
            kit::labeled_row(ui, tr("wall_type.wt.type"), |ui| {
                let names: Vec<&str> = types.iter().map(|t| t.1.as_str()).collect();
                let mut sel = types.iter().position(|t| t.0 == d.id).unwrap_or(0);
                let clean = !d.dirty();
                let tw = Some(layout::PROP_VALUE * 3.0);
                let r = ui
                    .add_enabled_ui(clean, |ui| {
                        widgets::dropdown(ui, th, "wall_type.wt.type", &mut sel, &names, tw)
                    })
                    .inner;
                let r = if clean { r } else { r.on_disabled_hover_text(tr("wall_type.wt.switch_dirty")) };
                if r.changed() {
                    switch = types.get(sel).map(|t| t.0);
                }
                let b = widgets::Button::secondary(tr("wall_type.wt.duplicate_type"))
                    .tooltip(tr("wall_type.wt.duplicate_tip"), "")
                    .show(ui, th);
                dup = b.clicked();
            });
            let no_name = d.name.trim().is_empty();
            kit::labeled_row(ui, tr("wall_type.wt.name"), |ui| {
                widgets::TextField::new(&mut d.name)
                    .id_salt("wall_type.wt.name")
                    .width(layout::PROP_VALUE * 3.0)
                    .error(no_name)
                    .labelled_by(egui::Id::new("ag3.wt.name.lbl"), tr("wall_type.wt.name"))
                    .show(ui, th);
            });
            ui.add_space(space::SM);
            egui::ScrollArea::vertical()
                .id_salt("ag3.wt.rows")
                .max_height(control::HIT_MIN * 10.0)
                .show(ui, |ui| table(ui, th, cat, ru, d, &extra));
            ui.add_space(space::SM);
            section(ui, th, ru, d);
            ui.add_space(space::SM);
            kit::labeled_row(ui, tr("wall_type.wt.walls_keep"), |ui| {
                let items = [tr("wall_type.wt.keep_core"), tr("wall_type.wt.keep_exterior")];
                let at = usize::from(d.align == Align::Exterior);
                if let Some(i) = widgets::segmented(ui, th, &items, at) {
                    d.align = if i == 0 { Align::Core } else { Align::Exterior };
                }
            });
            let n = d.walls.to_string();
            let uses = crate::tr_lang(ru, "wall_type.wt.uses", &[("n", n.as_str())]);
            ui.label(RichText::new(uses).font(th.font(Role::Label)).color(th.c("text.secondary")));
            let resized = d.resized_walls();
            if resized > 0 {
                let k = resized.to_string();
                kit::warn_note(ui, crate::tr_lang(ru, "wall_type.wt.resizes", &[("n", k.as_str())]));
            }
            if let Some(e) = &d.error {
                let t = RichText::new(e.as_str()).font(th.font(Role::Label));
                ui.label(t.color(th.c("danger")));
            }
            kit::dialog_footer(ui, |ui| {
                ok = kit::primary_action_table(ui, tr("wall_type.wt.ok"), blocked.as_deref());
                cancel = kit::cancel_button(ui, tr("wall_type.wt.cancel")).clicked();
            });
        });
    if dup {
        match duplicate_type(ed, d, ru) {
            Ok(nid) => {
                if let Ok(nd) = WallTypeDraft::load(&ed.model, nid, d.units) {
                    *d = nd;
                }
            }
            Err(e) => d.error = Some(e),
        }
        return true;
    }
    if let Some(id) = switch.filter(|id| *id != d.id) {
        if let Ok(nd) = WallTypeDraft::load(&ed.model, id, d.units) {
            *d = nd;
        }
        return true;
    }
    if ok && blocked.is_none() {
        return !apply(ed, d, ru);
    }
    open && !cancel
}

/// What the layout probe opens the editor on.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Case {
    /// The default type with its walls.
    Plain,
    /// Many layers, a long name and a thickness change (the warning shows).
    Busy,
}

#[cfg(test)]
pub(crate) fn test_editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let segs = [((0.0, 0.0), (6.0, 0.0)), ((0.0, 3.0), (6.0, 3.0))];
    crate::modify_tools::tests::walls(&mut ed, &segs);
    ed
}

/// The editor drawn headless in an explicit language, for the layout contract.
#[cfg(test)]
pub(crate) fn layout_probe(ru: bool, case: Case) -> crate::layout_contract::Probe {
    let mut ed = test_editor();
    let id = ed.walls[0].wall_type;
    let mut d = WallTypeDraft::load(&ed.model, id, ed.tool.units).unwrap();
    if case == Case::Busy {
        d.name = "Long wall type name ".repeat(4);
        for i in 0..6 {
            d.add(Some(i));
        }
        d.rows[0].function = LayerFn::Membrane;
    }
    let th = Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    let cat = Catalog::new(if ru { Lang::Ru } else { Lang::En });
    crate::layout_contract::probe("wall_type_editor", |ctx| {
        window(ctx, &th, &cat, ru, &mut ed, &mut d);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(ed: &Editor) -> WallTypeDraft {
        WallTypeDraft::load(&ed.model, ed.walls[0].wall_type, ed.tool.units).unwrap()
    }

    #[test]
    fn layer_ops_keep_at_least_one_layer() {
        let ed = test_editor();
        let mut d = draft(&ed);
        let n = d.rows.len();
        assert!(d.add(None));
        assert_eq!(d.rows.len(), n + 1);
        assert!(d.duplicate(0));
        assert_eq!(d.rows[0], d.rows[1]);
        let last = d.rows.len() - 1;
        d.rows[last].material = "glass".into();
        assert!(d.move_to(last, 0));
        assert_eq!(d.rows[0].material, "glass");
        assert!(d.shift(0, false));
        assert_eq!(d.rows[1].material, "glass");
        assert!(!d.shift(0, true), "the first row has nothing above");
        while d.rows.len() > 1 {
            assert!(d.remove(0));
        }
        assert!(!d.remove(0), "the last layer stays");
        assert!(!d.move_to(0, 0));
    }

    #[test]
    fn validation_blocks_ok_with_a_reason() {
        let ed = test_editor();
        let mut d = draft(&ed);
        let is = |d: &WallTypeDraft, ru: bool, k: &'static str| {
            assert_eq!(d.blocked(&ed.model, ru).as_deref(), Some(crate::tr_str(ru, k)));
        };
        is(&d, false, "wall_type.wt.no_changes");
        d.rows[0].thickness = "0".into();
        is(&d, false, "wall_type.wt.bad_thickness");
        d.rows[0].thickness = "abc".into();
        assert!(d.op(false).is_err());
        d.rows[0].thickness = edit_length(0.3, d.units);
        d.name = "  ".into();
        is(&d, true, "wall_type.wt.no_name");
        let other = ed.types.iter().find(|t| t.id != d.id).unwrap().name.clone();
        d.name = other;
        is(&d, false, "wall_type.wt.name_taken");
        d.name = "Fresh name".into();
        assert!(d.blocked(&ed.model, false).is_none());
    }

    #[test]
    fn resize_warning_counts_the_placed_walls() {
        let ed = test_editor();
        let mut d = draft(&ed);
        assert_eq!(d.walls, 2);
        assert_eq!(d.resized_walls(), 0);
        d.rows[0].function = LayerFn::Membrane;
        assert_eq!(d.resized_walls(), 0, "a function change keeps the thickness");
        d.add(None);
        assert_eq!(d.resized_walls(), 2);
    }

    #[test]
    fn ok_is_one_undo_step_and_keeps_the_exterior_face() {
        let mut ed = test_editor();
        let wall = ed.walls[0].id;
        let mut d = draft(&ed);
        let face0 = ed.model.wall_offset0(wall).unwrap();
        d.align = Align::Exterior;
        d.add(None);
        d.rows[0].priority = 7;
        let before = ed.can_undo();
        assert!(apply(&mut ed, &mut d, false));
        assert!((ed.model.wall_offset0(wall).unwrap() - face0).abs() < 1e-9);
        let got = ed.model.wall_type_layers_full(d.id).unwrap();
        assert_eq!(got.len(), d.rows.len());
        assert_eq!(got[0].priority, 7);
        assert!(ed.status.is_some());
        ed.undo();
        assert_eq!(ed.can_undo(), before);
        assert_eq!(ed.model.wall_type_layers_full(d.id).unwrap().len(), d.rows.len() - 1);
    }

    #[test]
    fn duplicate_type_leaves_the_source_as_stored() {
        let mut ed = test_editor();
        let mut d = draft(&ed);
        let src = ed.model.wall_type_layers_full(d.id).unwrap();
        d.add(None);
        let nid = duplicate_type(&mut ed, &d, false).unwrap();
        assert_ne!(nid, d.id);
        assert_eq!(ed.model.wall_type_layers_full(d.id).unwrap(), src);
        assert_eq!(ed.model.wall_type_layers_full(nid).unwrap().len(), src.len() + 1);
        assert!(ed.model.walls_of_type(nid).unwrap().is_empty());
    }

    #[test]
    fn open_keeps_unsaved_edits_of_another_type() {
        let ed = test_editor();
        let (a, b) = (ed.types[0].id, ed.types[1].id);
        assert!(open(&ed.model, a, ed.tool.units));
        with(|o| o.as_mut().unwrap().name = "Edited".into());
        assert!(!open(&ed.model, b, ed.tool.units));
        assert_eq!(with(|o| o.as_ref().map(|d| d.id)), Some(a));
        with(|o| *o = None);
        assert!(open(&ed.model, b, ed.tool.units));
        with(|o| *o = None);
    }

    #[test]
    fn function_labels_exist_in_both_languages() {
        for f in LayerFn::ALL {
            for ru in [false, true] {
                assert!(!fn_label(ru, f).starts_with("wall_type."), "{f:?} {ru}");
            }
        }
        let cyr = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c));
        assert!(cyr(fn_label(true, LayerFn::Insulation)));
        assert!(!cyr(fn_label(false, LayerFn::Insulation)));
    }
}
