// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Circuits" window: the panel circuit list and property editing of the selected circuit: device
//! load, cosφ and load type, poles and voltage, installation method, temperature, number of
//! circuits in the group, breaker and cable cross-section overrides. Cable-schedule rules are
//! recomputed from the draft; "Apply" is one undo step (`update_circuit`).

use std::sync::Arc;

use ddc_model::{
    auto_size, preview_rules, CableRules, Circuit, CircuitKind, CircuitReport, CircuitSpec,
    Conductor, Model, RuleFinding, Standard,
};
use ddc_ui::theme::{radius, space, stroke, Role};
use ddc_ui::widgets::{self, Button, NumberField, RowState, TextField};
use ddc_ui::Theme;
use egui::{self, RichText};

use crate::editor::OpFn;
use crate::shell::drawing_kit::number_format;
use crate::shell::EditorApp;

/// Window state: the circuits of the last revision, the selection and the unsaved draft.
#[derive(Default)]
pub struct CircuitEdit {
    pub open: bool,
    sel: Option<i64>,
    cache: Option<(u64, Arc<Vec<Row>>, Standard)>,
    draft: Option<Draft>,
    msg: Option<String>,
}

/// A saved circuit with what the editor needs to preview an edit.
struct Row {
    circuit: Circuit,
    report: CircuitReport,
    /// Per device: the load in use and the family's own value, W.
    loads: Vec<(f64, f64)>,
}

/// The circuit as edited: not in the model until Apply.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub id: i64,
    pub spec: CircuitSpec,
    /// Effective load per device, W.
    pub loads: Vec<f64>,
    fam: Vec<f64>,
    orig: CircuitSpec,
}

impl Draft {
    fn new(r: &Row) -> Self {
        Draft {
            id: r.circuit.id,
            spec: r.circuit.spec.clone(),
            loads: r.loads.iter().map(|l| l.0).collect(),
            fam: r.loads.iter().map(|l| l.1).collect(),
            orig: r.circuit.spec.clone(),
        }
    }

    pub fn total(&self) -> f64 {
        self.loads.iter().sum()
    }

    /// The spec to save: a device whose load equals the family's goes back to "family value".
    pub fn to_spec(&self) -> CircuitSpec {
        let mut s = self.spec.clone();
        for (k, d) in s.devices.iter_mut().enumerate() {
            let (v, f) = (self.loads[k], self.fam[k]);
            d.load = ((v - f).abs() > 1e-9).then_some(v);
        }
        // a designation names the old section or core count
        if s.cable != self.orig.cable || s.poles != self.orig.poles {
            s.cable_type.clear();
        }
        s
    }

    /// Picks a load type and takes its power factor from the table.
    pub fn set_load_type(&mut self, id: &str) {
        if let Some(c) = CableRules::shared().cos_phi_of(id) {
            self.spec.load_type = id.to_string();
            self.spec.cos_phi = c;
        }
    }

    /// Three poles are the 400 V line voltage, one pole 230 V: the voltage follows the poles.
    pub fn set_poles(&mut self, poles: u32) {
        if self.spec.poles == poles {
            return;
        }
        self.spec.poles = poles;
        self.spec.voltage = if poles == 3 { 400.0 } else { 230.0 };
    }

    /// Reads a device back to the family's value.
    pub fn reset_load(&mut self, k: usize) {
        self.loads[k] = self.fam[k];
    }

    pub fn dirty(&self) -> bool {
        self.to_spec() != self.orig
    }

    /// Keys of what stops Apply; the model checks the same numbers.
    pub fn problems(&self) -> Vec<&'static str> {
        let s = &self.spec;
        let pos = |v: f64| v.is_finite() && v > 0.0;
        let mut p = vec![];
        if s.name.trim().is_empty() {
            p.push("ce.err.name");
        }
        if !(pos(s.voltage) && pos(s.breaker) && pos(s.cable) && pos(s.max_drop)) {
            p.push("ce.err.positive");
        }
        if s.poles == 3 && !(380.0..=420.0).contains(&s.voltage) {
            p.push("ce.err.three_poles");
        }
        if !(pos(s.cos_phi) && s.cos_phi <= 1.0) {
            p.push("ce.err.cos");
        }
        if !(s.ambient_c.is_finite() && (-40.0..=100.0).contains(&s.ambient_c)) || s.grouped == 0 {
            p.push("ce.err.ambient");
        }
        if self.loads.iter().any(|l| !(l.is_finite() && *l >= 0.0)) {
            p.push("ce.err.load");
        }
        p
    }

    fn findings(&self, std: Standard, r: &Row) -> Vec<RuleFinding> {
        preview_rules(std, &r.circuit.spec, &r.report, &self.to_spec(), self.total())
    }
}

pub fn register(a: &mut EditorApp) {
    a.register_command("mep.circuits", |a| a.circuit_edit.open = true);
}

enum Act {
    Op(OpFn),
}

fn op(f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static) -> Act {
    Act::Op(OpFn(Arc::new(f)))
}

struct Cx<'a> {
    cat: &'a ddc_ui::i18n::Catalog,
    th: &'a Theme,
    ru: bool,
}

/// Width of a number field and of a dropdown, in rows.
const NUM_W: f32 = 5.0;
const PICK_W: f32 = 9.0;
const NAME_W: f32 = 12.0;
/// Rows of the circuit list.
const LIST_W: f32 = 14.0;

fn load_rows(m: &Model) -> Result<(Vec<Row>, Standard), ddc_model::Error> {
    let mut rows = vec![];
    for c in m.circuits(None)? {
        let report = m.circuit_report(&c)?;
        let loads = m.circuit_device_loads(&c)?;
        rows.push(Row { circuit: c, report, loads });
    }
    Ok((rows, m.elec_standard()?))
}

pub fn window(app: &mut EditorApp, ctx: &egui::Context) {
    if !app.circuit_edit.open {
        return;
    }
    let mut w = std::mem::take(&mut app.circuit_edit);
    refresh(&mut w, &app.ed.model, app.ed.revision);
    let th = ddc_ui::theme::of(ctx);
    let mut acts: Vec<Act> = vec![];
    let mut open = true;
    {
        let cat = &app.sh.cat;
        let cx = Cx { cat, th: &th, ru: cat.lang == ddc_ui::i18n::Lang::Ru };
        crate::ui_kit::Dialog::new("mep.circuits", cat.t("ce.title"), crate::ui_kit::DialogSize::Wide)
            .open(&mut open)
            .show(ctx, |ui| body(ui, &cx, &mut w, &mut acts));
    }
    w.open = open;
    for Act::Op(f) in acts {
        match app.ed.run_op(f) {
            Ok(()) => w.msg = None,
            Err(e) => w.msg = Some(crate::mep_browser::err_text(&app.sh.cat, &e)),
        }
    }
    app.circuit_edit = w;
}

/// Reloads the circuits when the model changed and keeps the draft while its circuit is the
/// same as the one it was taken from.
fn refresh(w: &mut CircuitEdit, m: &Model, rev: u64) {
    if w.cache.as_ref().is_some_and(|c| c.0 == rev) {
        return;
    }
    match load_rows(m) {
        Ok((rows, std)) => w.cache = Some((rev, Arc::new(rows), std)),
        Err(e) => {
            w.msg = Some(e.to_string());
            return;
        }
    }
    let rows = &w.cache.as_ref().expect("just set").1;
    if w.sel.is_none_or(|s| !rows.iter().any(|r| r.circuit.id == s)) {
        w.sel = rows.first().map(|r| r.circuit.id);
        w.draft = None;
    }
    let keep = w.draft.as_ref().is_some_and(|d| {
        rows.iter().any(|r| r.circuit.id == d.id && r.circuit.spec == d.orig)
    });
    if !keep {
        w.draft = None;
    }
}

fn body(ui: &mut egui::Ui, cx: &Cx, w: &mut CircuitEdit, acts: &mut Vec<Act>) {
    let (cat, th) = (cx.cat, cx.th);
    let Some((_, rows, std)) = w.cache.clone() else {
        if let Some(m) = &w.msg {
            crate::ui_kit::error_note(ui, m);
        }
        return;
    };
    standard_row(ui, cx, std, acts);
    if let Some(m) = &w.msg {
        crate::ui_kit::error_note(ui, m);
    }
    ui.separator();
    if rows.is_empty() {
        crate::ui_kit::empty_note(ui, cat.t("ce.empty"), Some(&cat.t("ce.empty_hint")));
        return;
    }
    ui.columns(2, |cols| {
        let [left, right] = cols else { return };
        egui::ScrollArea::vertical().id_salt("ce_list").show(left, |ui| {
            for r in rows.iter() {
                let selected = w.sel == Some(r.circuit.id);
                if list_row(ui, cx, selected, r).clicked() && !selected {
                    w.sel = Some(r.circuit.id);
                    w.draft = None;
                }
            }
        });
        egui::ScrollArea::vertical().id_salt("ce_form").show(right, |ui| {
            let Some(row) = rows.iter().find(|r| Some(r.circuit.id) == w.sel) else {
                return;
            };
            let d = w.draft.get_or_insert_with(|| Draft::new(row));
            form(ui, cx, d, row, std, &mut w.msg, acts);
        });
    });
    let _ = th;
}

fn standard_row(ui: &mut egui::Ui, cx: &Cx, std: Standard, acts: &mut Vec<Act>) {
    let (cat, th) = (cx.cat, cx.th);
    ui.horizontal(|ui| {
        let lbl = cat.t("ce.standard");
        ui.label(RichText::new(&lbl).color(th.sem().text_secondary));
        let names = [cat.t("ce.standard.pue"), cat.t("ce.standard.iec")];
        let items: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut i = (std == Standard::Iec) as usize;
        let before = i;
        widgets::dropdown(ui, th, "ce_standard", &mut i, &items, Some(th.d().row * PICK_W));
        if i != before {
            let s = if i == 1 { Standard::Iec } else { Standard::Pue };
            acts.push(op(move |m| m.set_elec_standard(s)));
        }
    });
}

fn list_row(ui: &mut egui::Ui, cx: &Cx, selected: bool, r: &Row) -> egui::Response {
    let th = cx.th;
    let d = th.d();
    let width = (d.row * LIST_W).min(ui.available_width());
    let size = egui::vec2(width, d.row + th.font(Role::Caption).size + space::XS);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let state = if selected { RowState::Selected } else { RowState::Normal };
    let (fill, ink, muted) = widgets::table::row_colors(th.sem(), state, resp.hovered());
    ui.painter().rect_filled(rect, radius::SM, fill);
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect,
            radius::SM,
            egui::Stroke::new(stroke::FOCUS, th.sem().focus_ring),
            egui::StrokeKind::Inside,
        );
    }
    let bad = r.report.rules.iter().any(|f| !f.ok);
    let s = th.sem();
    let dot_r = space::XS;
    let dot = egui::pos2(rect.left() + space::SM + dot_r, rect.top() + d.row / 2.0);
    ui.painter().circle_filled(dot, dot_r, if bad { s.error } else { s.success });
    let left = dot.x + dot_r + space::SM;
    let line1 = egui::Rect::from_min_max(
        egui::pos2(left, rect.top()),
        egui::pos2(rect.right() - space::SM, rect.top() + d.row),
    );
    widgets::text::paint_in(ui, line1, &r.circuit.spec.name, th.font(Role::Body), ink, egui::Align::Min);
    let sub = format!(
        "{} A · {} mm² · {:.0} W",
        r.circuit.spec.breaker, r.circuit.spec.cable, r.report.load
    );
    let line2 = egui::Rect::from_min_max(
        egui::pos2(left, rect.top() + d.row - space::XS),
        egui::pos2(rect.right() - space::SM, rect.bottom()),
    );
    widgets::text::paint_in(ui, line2, &sub, th.font(Role::Caption), muted, egui::Align::Min);
    let name = r.circuit.spec.name.clone();
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &name)
    });
    resp
}

/// One labelled number field of the form; true when an edit was finished.
#[allow(clippy::too_many_arguments)]
fn num(
    ui: &mut egui::Ui,
    cx: &Cx,
    salt: &str,
    label: &str,
    v: &mut f64,
    decimals: usize,
    range: (f64, f64),
    unit: &str,
) {
    let th = cx.th;
    let cap = ui.label(label);
    NumberField::new(("ce", salt), v, number_format(cx.ru))
        .labelled_by(cap.id, label)
        .decimals(decimals)
        .range(range.0, range.1)
        .unit(unit)
        .width(th.d().row * NUM_W)
        .show(ui, th);
    ui.end_row();
}

/// One labelled dropdown; `items` are shown, the result is the chosen index.
fn pick(ui: &mut egui::Ui, cx: &Cx, salt: &str, label: &str, cur: usize, items: &[String]) -> usize {
    let th = cx.th;
    ui.label(label);
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    let mut i = cur.min(items.len().saturating_sub(1));
    widgets::dropdown(ui, th, ("ce", salt), &mut i, &refs, Some(th.d().row * PICK_W));
    ui.end_row();
    i
}

fn form(
    ui: &mut egui::Ui,
    cx: &Cx,
    d: &mut Draft,
    row: &Row,
    std: Standard,
    msg: &mut Option<String>,
    acts: &mut Vec<Act>,
) {
    let (cat, th) = (cx.cat, cx.th);
    let rules = CableRules::shared();
    let problems = d.problems();
    ui.horizontal(|ui| {
        let dirty = d.dirty();
        let apply = ui
            .add_enabled_ui(dirty && problems.is_empty(), |ui| {
                Button::primary(&cat.t("ce.apply")).show(ui, th)
            })
            .inner;
        if apply.clicked() {
            let (id, spec) = (d.id, d.to_spec());
            acts.push(op(move |m| m.update_circuit(id, &spec)));
        }
        let rev = ui.add_enabled_ui(dirty, |ui| Button::secondary(&cat.t("ce.revert")).show(ui, th));
        if rev.inner.clicked() {
            *d = Draft::new(row);
            *msg = None;
        }
        if Button::secondary(&cat.t("ce.autosize")).show(ui, th).clicked() {
            match auto_size(std, &d.to_spec(), d.total(), row.report.length) {
                Some(s) => {
                    d.spec.breaker = s.breaker;
                    d.spec.cable = s.section;
                    *msg = None;
                }
                None => *msg = Some(cat.t("ce.autosize_none")),
            }
        }
    });
    ui.add_space(space::SM);
    egui::Grid::new(("ce_grid", d.id)).num_columns(2).show(ui, |ui| {
        let lbl = cat.t("ce.f.name");
        let lr = ui.label(&lbl);
        TextField::new(&mut d.spec.name)
            .id_salt(("ce_name", d.id))
            .width(th.d().row * NAME_W)
            .labelled_by(lr.id, &lbl)
            .show(ui, th);
        ui.end_row();

        let kinds = [CircuitKind::Power, CircuitKind::Lighting];
        let names: Vec<String> = kinds.iter().map(|k| cat.t(&format!("ce.kind.{}", k.name()))).collect();
        let cur = kinds.iter().position(|k| *k == d.spec.kind).unwrap_or(0);
        d.spec.kind = kinds[pick(ui, cx, "kind", &cat.t("ce.f.kind"), cur, &names)];

        let poles = ["1", "3"].map(String::from);
        let cur = (d.spec.poles == 3) as usize;
        let p = pick(ui, cx, "poles", &cat.t("ce.f.poles"), cur, &poles);
        d.set_poles(if p == 1 { 3 } else { 1 });
        num(ui, cx, "voltage", &cat.t("ce.f.voltage"), &mut d.spec.voltage, 0, (12.0, 1000.0), "V");

        let mats = [Conductor::Cu, Conductor::Al];
        let names: Vec<String> = mats.iter().map(|c| cat.t(&format!("ce.conductor.{}", c.name()))).collect();
        let cur = mats.iter().position(|c| *c == d.spec.conductor).unwrap_or(0);
        d.spec.conductor = mats[pick(ui, cx, "conductor", &cat.t("ce.f.conductor"), cur, &names)];

        let names: Vec<String> = rules
            .load_types
            .iter()
            .map(|t| format!("{} ({})", cat.t(&format!("ce.lt.{}", t.id)), t.cos_phi))
            .collect();
        let cur = rules.load_types.iter().position(|t| t.id == d.spec.load_type).unwrap_or(0);
        let i = pick(ui, cx, "load_type", &cat.t("ce.f.load_type"), cur, &names);
        if rules.load_types[i].id != d.spec.load_type {
            d.set_load_type(&rules.load_types[i].id);
        }
        num(ui, cx, "cos", &cat.t("ce.f.cos"), &mut d.spec.cos_phi, 2, (0.05, 1.0), "");

        let names: Vec<String> = rules.laying.iter().map(|l| cat.t(&format!("ce.laying.{}", l.id))).collect();
        let cur = rules.laying.iter().position(|l| l.id == d.spec.laying).unwrap_or(0);
        let i = pick(ui, cx, "laying", &cat.t("ce.f.laying"), cur, &names);
        d.spec.laying = rules.laying[i].id.clone();

        num(ui, cx, "ambient", &cat.t("ce.f.ambient"), &mut d.spec.ambient_c, 0, (-40.0, 100.0), "°C");
        let mut g = d.spec.grouped as f64;
        num(ui, cx, "grouped", &cat.t("ce.f.grouped"), &mut g, 0, (1.0, 200.0), "");
        d.spec.grouped = g.round().max(1.0) as u32;
        num(ui, cx, "breaker", &cat.t("ce.f.breaker"), &mut d.spec.breaker, 0, (1.0, 6300.0), "A");
        num(ui, cx, "cable", &cat.t("ce.f.cable"), &mut d.spec.cable, 1, (0.5, 1000.0), "mm²");
        num(ui, cx, "drop", &cat.t("ce.f.max_drop"), &mut d.spec.max_drop, 1, (0.1, 20.0), "%");
    });

    ui.add_space(space::SM);
    ui.label(RichText::new(cat.t("ce.devices")).color(th.sem().text_secondary));
    egui::Grid::new(("ce_dev", d.id)).num_columns(3).show(ui, |ui| {
        for k in 0..d.loads.len() {
            let dev = &d.spec.devices[k];
            let cap = ui.label(cat.tf(
                "ce.device",
                &[("comp", &dev.comp.to_string()), ("connector", &dev.connector)],
            ));
            NumberField::new(("ce_load", d.id, k), &mut d.loads[k], number_format(cx.ru))
                .labelled_by(cap.id, &dev.connector)
                .decimals(0)
                .range(0.0, 1.0e7)
                .unit("W")
                .width(th.d().row * NUM_W)
                .show(ui, th);
            let own = (d.loads[k] - d.fam[k]).abs() > 1e-9;
            ui.add_enabled_ui(own, |ui| {
                if Button::quiet(&cat.t("ce.reset")).show(ui, th).clicked() {
                    d.reset_load(k);
                }
            });
            ui.end_row();
        }
    });
    ui.label(cat.tf(
        "ce.totals",
        &[
            ("load", &format!("{:.0}", d.total())),
            ("len", &format!("{:.1}", row.report.length)),
        ],
    ));

    ui.add_space(space::SM);
    ui.label(RichText::new(cat.t("ce.rules")).color(th.sem().text_secondary));
    for f in d.findings(std, row) {
        let col = if f.ok { th.sem().success } else { th.sem().error };
        let text = cat.tf(
            &format!("ce.rule.{}", f.rule),
            &[("value", &format!("{:.2}", f.value)), ("limit", &format!("{:.2}", f.limit))],
        );
        ui.add(egui::Label::new(RichText::new(text).color(col)).wrap());
        if let Some(n) = &f.note {
            ui.add(egui::Label::new(RichText::new(n).color(th.sem().text_secondary)).wrap());
        }
    }
    if rules.x0(d.spec.conductor, d.spec.cable).1 {
        ui.label(RichText::new(cat.t("mep.sched.x0_estimated")).color(th.sem().text_secondary));
    }
    for k in &problems {
        crate::ui_kit::error_note(ui, &cat.t(k));
    }
}

/// A level with one wall; devices of the library are wall-hosted, see `place_on`.
#[cfg(test)]
fn wall_fixtures(m: &mut Model) -> (i64, i64) {
    use ddc_model::{LayerSpec, WallSpec};
    let l = m.add_level("L1", 0.0).unwrap();
    let wt = m
        .add_wall_type("W200", &[LayerSpec { material: "Concrete".into(), thickness: 0.2 }])
        .unwrap();
    let wall = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (20.0, 0.0),
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    (l, wall)
}

/// A library device on the wall, `x` metres along it.
#[cfg(test)]
fn place_on(m: &mut Model, at: (i64, i64), family: &str, ty: &str, x: f64) -> i64 {
    use ddc_model::{CompHost, CompSpec};
    let f = m.load_library_family(family).unwrap();
    let ct = m.comp_type_id(f, ty).unwrap().unwrap();
    let mut s = CompSpec::at(at.0, ct, x, 0.0);
    s.host = CompHost::Wall(at.1);
    s.at.2 = 1.2;
    m.add_component(&s).unwrap()
}

/// Opens the window on an app whose model holds a panel and three circuits, one of them
/// failing its rules, for the layout contract.
#[cfg(test)]
pub(crate) fn open_for_layout(app: &mut EditorApp) {
    use ddc_model::CircuitDevice;
    let m = &mut app.ed.model;
    let at = wall_fixtures(m);
    let panel = place_on(m, at, "Distribution Board", "400x600", 1.0);
    let sockets: Vec<i64> =
        (0..3).map(|k| place_on(m, at, "Socket Outlet", "Single", 2.0 + k as f64)).collect();
    for (k, kind) in [CircuitKind::Power, CircuitKind::Lighting, CircuitKind::Power].into_iter().enumerate() {
        let dev = |c: &i64| CircuitDevice { comp: *c, connector: "Power".into(), load: Some(900.0) };
        let mut s = CircuitSpec::new(panel, kind, sockets.iter().map(dev).collect());
        s.name = format!("Circuit with a rather long name number {k}");
        if k == 2 {
            s.breaker = 2.0;
        }
        app.ed.model.add_circuit(&s).unwrap();
    }
    app.ed.revision += 1;
    app.circuit_edit.open = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use ddc_model::CircuitDevice;

    /// An editor with one panel and a circuit of two sockets of 300 W each.
    fn with_circuit() -> (Editor, i64) {
        let mut ed = Editor::new_in_memory().unwrap();
        let at = wall_fixtures(&mut ed.model);
        let panel = place_on(&mut ed.model, at, "Distribution Board", "400x600", 1.0);
        let devices = (0..2)
            .map(|k| {
                let c = place_on(&mut ed.model, at, "Socket Outlet", "Single", 2.0 + k as f64);
                CircuitDevice { comp: c, connector: "Power".into(), load: Some(300.0) }
            })
            .collect();
        let mut s = CircuitSpec::new(panel, CircuitKind::Power, devices);
        s.name = "C1".into();
        let id = ed.model.add_circuit(&s).unwrap();
        ed.revision += 1;
        (ed, id)
    }

    fn draft_of(ed: &Editor, id: i64) -> (Draft, Row) {
        let (rows, _) = load_rows(&ed.model).unwrap();
        let row = rows.into_iter().find(|r| r.circuit.id == id).unwrap();
        (Draft::new(&row), row)
    }

    #[test]
    fn load_type_sets_cos_and_poles_set_voltage() {
        let (ed, id) = with_circuit();
        let (mut d, _) = draft_of(&ed, id);
        d.set_load_type("workshop");
        assert_eq!((d.spec.load_type.as_str(), d.spec.cos_phi), ("workshop", 0.60));
        d.set_load_type("nothing");
        assert_eq!(d.spec.load_type, "workshop");
        d.set_poles(3);
        assert_eq!((d.spec.poles, d.spec.voltage), (3, 400.0));
        d.set_poles(1);
        assert_eq!(d.spec.voltage, 230.0);
    }

    #[test]
    fn a_load_equal_to_the_family_value_is_not_an_override() {
        let (ed, id) = with_circuit();
        let (mut d, _) = draft_of(&ed, id);
        assert!(!d.dirty());
        d.loads[0] = 450.0;
        assert!(d.dirty());
        assert_eq!(d.to_spec().devices[0].load, Some(450.0));
        d.reset_load(0);
        let fam = d.fam[0];
        assert_eq!(d.loads[0], fam);
        assert_eq!(d.to_spec().devices[0].load, None);
        assert_eq!(d.total(), d.loads[0] + d.loads[1]);
    }

    #[test]
    fn problems_block_apply() {
        let (ed, id) = with_circuit();
        let (mut d, _) = draft_of(&ed, id);
        assert!(d.problems().is_empty());
        d.spec.cos_phi = 1.2;
        d.spec.grouped = 0;
        d.spec.poles = 3;
        d.loads[1] = -5.0;
        d.spec.name = "  ".into();
        let p = d.problems();
        for k in ["ce.err.cos", "ce.err.ambient", "ce.err.three_poles", "ce.err.load", "ce.err.name"] {
            assert!(p.contains(&k), "{k} in {p:?}");
        }
    }

    #[test]
    fn preview_follows_the_draft_and_apply_is_one_undo_step() {
        let (mut ed, id) = with_circuit();
        let (mut d, row) = draft_of(&ed, id);
        // 600 W / (230 * 0.85) = 3.07 A on a 16 A breaker: fine; a 2 A breaker fails EL-BRK-01
        let ok = d.findings(Standard::Pue, &row);
        assert!(ok.iter().find(|f| f.rule == "EL-BRK-01").unwrap().ok);
        d.spec.breaker = 2.0;
        let bad = d.findings(Standard::Pue, &row);
        assert!(!bad.iter().find(|f| f.rule == "EL-BRK-01").unwrap().ok);
        d.spec.breaker = 16.0;
        d.spec.cable = 4.0;
        d.spec.ambient_c = 40.0;
        d.spec.grouped = 3;
        d.set_load_type("shop");
        d.loads[0] = 500.0;
        let spec = d.to_spec();
        let before = ed.model.circuits(None).unwrap();
        ed.run_op(OpFn(Arc::new({
            let spec = spec.clone();
            move |m| m.update_circuit(id, &spec)
        })))
        .unwrap();
        let after = ed.model.circuits(None).unwrap();
        let s = &after[0].spec;
        assert_eq!((s.cable, s.ambient_c, s.grouped, s.cos_phi), (4.0, 40.0, 3, 0.85));
        assert_eq!((s.load_type.as_str(), s.devices[0].load), ("shop", Some(500.0)));
        // one undo restores everything the apply wrote
        ed.undo();
        assert_eq!(ed.model.circuits(None).unwrap(), before);
    }

    #[test]
    fn auto_size_fills_breaker_and_section_from_the_draft() {
        let (ed, id) = with_circuit();
        let (mut d, row) = draft_of(&ed, id);
        d.loads = vec![3000.0, 3000.0];
        let s = auto_size(Standard::Pue, &d.to_spec(), d.total(), row.report.length).unwrap();
        // 6000 / (230 * 0.85) = 30.7 A takes the 32 A breaker
        assert_eq!(s.breaker, 32.0);
        assert!(s.section >= 4.0);
    }

    #[test]
    fn no_raw_widgets_and_both_languages_have_the_keys() {
        use ddc_ui::i18n::{Catalog, Lang};
        let src = include_str!("circuit_edit.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        for raw in ["egui::TextEdit", "egui::ComboBox", "egui::DragValue", "egui::Button", "ui.button(", "Color32", "ui_is_ru", "crate::tr("] {
            assert!(!code.contains(raw), "left in the circuit editor: {raw}");
        }
        let mut keys: Vec<String> = ["title", "empty", "empty_hint", "standard", "standard.pue", "standard.iec",
            "f.name", "f.kind", "f.poles", "f.voltage", "f.conductor", "f.load_type", "f.cos", "f.laying",
            "f.ambient", "f.grouped", "f.breaker", "f.cable", "f.max_drop", "devices", "device", "reset",
            "totals", "rules", "apply", "revert", "autosize", "autosize_none", "kind.power", "kind.lighting",
            "conductor.cu", "conductor.al", "err.name", "err.positive", "err.three_poles", "err.cos",
            "err.ambient", "err.load", "open"]
            .iter().map(|k| format!("ce.{k}")).collect();
        let r = CableRules::shared();
        keys.extend(r.load_types.iter().map(|t| format!("ce.lt.{}", t.id)));
        keys.extend(r.laying.iter().map(|l| format!("ce.laying.{}", l.id)));
        keys.extend(["EL-BRK-01", "EL-BRK-02", "EL-BRK-03", "EL-CAB-01", "EL-DRP-01"].map(|k| format!("ce.rule.{k}")));
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            for k in &keys {
                assert_ne!(&c.t(k), k, "{k} missing in {lang:?}");
            }
        }
    }
}
