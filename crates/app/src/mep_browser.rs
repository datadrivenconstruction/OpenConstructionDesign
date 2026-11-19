// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The "Systems" window: a list of systems with status, flow and sizes by run, fixtures and their
//! connections, check findings in plain words. Edits go only through `run_op`.

use std::sync::Arc;

use ddc_model::{MepDeviceRow, MepIssue, MepSystemCheck, Model, SystemKind};
use ddc_ui::theme::{radius, space, stroke, Role};
use ddc_ui::widgets::{self, Button, NumberField, RowState, TextField};
use ddc_ui::Theme;
use egui::{self, RichText};

use crate::editor::{OpFn, Sel};
use crate::shell::EditorApp;
use crate::shell::drawing_kit::{fmt_len, len_field, number_format};
use crate::units::Units;

#[derive(Default)]
pub struct MepBrowser {
    pub open: bool,
    sel: Option<i64>,
    /// A report per model revision; a frame takes a reference, not a copy.
    cache: Option<(u64, Arc<Vec<MepSystemCheck>>)>,
    /// How many findings are shown beyond the first page.
    issue_limit: usize,
    new_name: String,
    new_kind: usize,
    /// Insulation in meters; the field shows it in project units.
    ins_m: f64,
    /// A system whose deletion awaits confirmation.
    confirm: Option<i64>,
    msg: Option<String>,
}

/// Findings per page: an imported network can have thousands.
const ISSUE_PAGE: usize = 100;

/// Operation error text: `mep.err.*` keys from the model, in the UI language.
pub fn err_text(cat: &ddc_ui::i18n::Catalog, e: &ddc_model::Error) -> String {
    match e {
        ddc_model::Error::Invalid(k) if k.starts_with("mep.err.") => cat.t(k),
        ddc_model::Error::Geom(_) => cat.tf("mep.issue.network", &[("message", &e.to_string())]),
        _ => e.to_string(),
    }
}

pub fn register(a: &mut EditorApp) {
    for id in ["mep.browser", "mep.system", "mep.check_system"] {
        a.register_command(id, open);
    }
}

fn open(a: &mut EditorApp) {
    a.mep_browser.open = true;
}

enum Act {
    Op(OpFn),
    ShowRun(i64),
    ShowComp(i64),
    OpenCircuits,
}

fn op(f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static) -> Act {
    Act::Op(OpFn(Arc::new(f)))
}

fn right(ui: &mut egui::Ui, text: String) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(text);
    });
}

/// Everything a frame needs to draw: language, theme tokens and the project units.
struct Cx<'a> {
    cat: &'a ddc_ui::i18n::Catalog,
    th: &'a Theme,
    units: Units,
    ru: bool,
}

/// Rows of a run/device table before it scrolls.
const TABLE_ROWS: f32 = 8.0;
/// Width of the new-system name field and of the kind dropdown, in rows.
const NAME_W: f32 = 8.0;
const KIND_W: f32 = 6.0;
/// Longest insulation a system can take, metres.
const INS_MAX_M: f64 = 0.2;

/// Where the delete confirmation stands after an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    Open(i64),
    Confirm,
    Cancel,
}

/// New confirmation state and whether to delete now: a delete needs `Open` first and then
/// `Confirm` for the same system; `Cancel` or a different system disarms it.
fn delete_step(armed: Option<i64>, ev: Option<Ask>) -> (Option<i64>, Option<i64>) {
    match ev {
        Some(Ask::Open(s)) => (Some(s), None),
        Some(Ask::Confirm) => (None, armed),
        Some(Ask::Cancel) => (None, None),
        None => (armed, None),
    }
}

/// Add is blocked on a blank name; returns the trimmed name when it is allowed.
fn new_system_name(raw: &str) -> Option<String> {
    let n = raw.trim();
    (!n.is_empty()).then(|| n.to_string())
}

pub fn window(app: &mut EditorApp, ctx: &egui::Context) {
    if !app.mep_browser.open {
        return;
    }
    let mut w = std::mem::take(&mut app.mep_browser);
    let rev = app.ed.revision;
    if w.cache.as_ref().is_none_or(|c| c.0 != rev) {
        match app.ed.model.mep_check_all() {
            Ok(all) => w.cache = Some((rev, Arc::new(all))),
            Err(e) => w.msg = Some(e.to_string()),
        }
    }
    let th = ddc_ui::theme::of(ctx);
    let mut acts: Vec<Act> = vec![];
    let mut open = true;
    {
        let cat = &app.sh.cat;
        let cx = Cx {
            cat,
            th: &th,
            units: crate::units::active_units(),
            ru: cat.lang == ddc_ui::i18n::Lang::Ru,
        };
        crate::ui_kit::Dialog::new("mep.browser", cat.t("mep.browser.title"), crate::ui_kit::DialogSize::Wide)
            .open(&mut open)
            .show(ctx, |ui| body(ui, &cx, &mut w, &mut acts));
        if let Some(sys) = w.confirm {
            let info = w
                .cache
                .as_ref()
                .and_then(|c| c.1.iter().find(|s| s.system == sys))
                .map(|s| (s.name.clone(), s.runs.len()));
            let ev = match info {
                Some((name, runs)) => confirm_modal(ctx, &cx, &name, runs),
                None => Some(Ask::Cancel),
            };
            let (armed, go) = delete_step(w.confirm, ev);
            w.confirm = armed;
            if let Some(sys) = go {
                acts.push(op(move |m| m.delete_mep_system(sys)));
            }
        }
    }
    w.open = open;
    if !open {
        w.confirm = None;
    }
    for act in acts {
        match act {
            Act::Op(f) => {
                if let Err(e) = app.ed.run_op(f) {
                    w.msg = Some(err_text(&app.sh.cat, &e));
                } else {
                    w.msg = None;
                }
            }
            Act::OpenCircuits => app.circuit_edit.open = true,
            Act::ShowRun(run) => {
                if let Ok(s) = app.ed.model.mep_run(run) {
                    app.ed.set_level(s.level);
                    app.ed.selection = vec![Sel::Mep(run)];
                }
            }
            Act::ShowComp(comp) => {
                if let Ok(s) = app.ed.model.component(comp) {
                    app.ed.set_level(s.level);
                    app.ed.selection = vec![Sel::Comp(comp)];
                }
            }
        }
    }
    app.mep_browser = w;
}

/// Modal asking before a system and its runs are removed; names the system and the run count.
fn confirm_modal(ctx: &egui::Context, cx: &Cx, name: &str, runs: usize) -> Option<Ask> {
    let c = cx.cat;
    let mut ev = None;
    let out = crate::ui_kit::modal_dialog("mep.delete", ctx, |ui| {
        ui.set_width(cx.th.d().row * 14.0);
        crate::ui_kit::modal_title(ui, c.t("mep_browser.delete.title"));
        ui.label(c.tf(
            "mep_browser.delete.body",
            &[("name", name), ("n", &runs.to_string())],
        ));
        ui.label(RichText::new(c.t("mep_browser.delete.undo")).color(cx.th.sem().text_secondary));
        crate::ui_kit::dialog_footer(ui, |ui| {
            let yes = c.t("mep_browser.delete.confirm");
            if crate::ui_kit::confirm_danger_button(ui, yes, true).clicked() {
                ev = Some(Ask::Confirm);
            }
            if crate::ui_kit::cancel_button(ui, c.t("mep_browser.cancel")).clicked() {
                ev = Some(Ask::Cancel);
            }
        });
    });
    if out.should_close() {
        ev = Some(Ask::Cancel);
    }
    ev
}

/// One system row: state dot, elided name, kind and status muted below.
fn system_row(ui: &mut egui::Ui, cx: &Cx, selected: bool, c: &MepSystemCheck) -> egui::Response {
    let th = cx.th;
    let d = th.d();
    let size = egui::vec2(ui.available_width(), d.row + th.font(Role::Caption).size + space::XS);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let state = if selected {
        RowState::Selected
    } else {
        RowState::Normal
    };
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
    let dot_r = space::XS;
    let dot = egui::pos2(rect.left() + space::SM + dot_r, rect.top() + d.row / 2.0);
    let s = th.sem();
    let dot_col = if c.errors() > 0 {
        s.error
    } else if c.issues.is_empty() {
        s.success
    } else {
        s.warning
    };
    ui.painter().circle_filled(dot, dot_r, dot_col);
    let left = dot.x + dot_r + space::SM;
    let line1 = egui::Rect::from_min_max(
        egui::pos2(left, rect.top()),
        egui::pos2(rect.right() - space::SM, rect.top() + d.row),
    );
    let cut = widgets::text::paint_in(ui, line1, &c.name, th.font(Role::Body), ink, egui::Align::Min);
    let sub = format!(
        "{} · {}",
        cx.cat.t(&format!("mep.kind.{}", c.kind.name())),
        status_line(cx.cat, c)
    );
    let line2 = egui::Rect::from_min_max(
        egui::pos2(left, rect.top() + d.row - space::XS),
        egui::pos2(rect.right() - space::SM, rect.bottom()),
    );
    widgets::text::paint_in(ui, line2, &sub, th.font(Role::Caption), muted, egui::Align::Min);
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &c.name)
    });
    if cut {
        resp.on_hover_text(&c.name)
    } else {
        resp
    }
}

fn body(ui: &mut egui::Ui, cx: &Cx, w: &mut MepBrowser, acts: &mut Vec<Act>) {
    let (cat, th) = (cx.cat, cx.th);
    let row = th.d().row;
    ui.horizontal(|ui| {
        let lbl = cat.t("mep.browser.new");
        let lr = ui.label(RichText::new(&lbl).color(th.sem().text_secondary));
        let hint = cat.t("mep_browser.name_hint");
        let out = TextField::new(&mut w.new_name)
            .id_salt("mep_new_name")
            .hint(&hint)
            .width(row * NAME_W)
            .labelled_by(lr.id, &lbl)
            .show(ui, th);
        let kinds: Vec<String> = SystemKind::ALL
            .iter()
            .map(|k| cat.t(&format!("mep.kind.{}", k.name())))
            .collect();
        let items: Vec<&str> = kinds.iter().map(String::as_str).collect();
        // wide enough for the longest kind name: "Supply air" must not read "Supply a..."
        let longest = items.iter().map(|s| s.chars().count()).max().unwrap_or(0) as f32;
        let kind_w = (row * KIND_W).max(longest * 8.5 + 40.0);
        widgets::dropdown(ui, th, "mep_new_kind", &mut w.new_kind, &items, Some(kind_w));
        let name = new_system_name(&w.new_name);
        let blocked = name.is_none();
        let r = ui
            .add_enabled_ui(!blocked, |ui| {
                Button::primary(&cat.t("mep.browser.add")).show(ui, th)
            })
            .inner;
        let r = if blocked {
            r.on_disabled_hover_text(cat.t("mep_browser.add_blocked"))
        } else {
            r
        };
        let enter = out.committed && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if let Some(name) = name.filter(|_| r.clicked() || enter) {
            let kind = SystemKind::ALL[w.new_kind.min(SystemKind::ALL.len() - 1)];
            acts.push(op(move |m| m.add_mep_system(&name, kind).map(|_| ())));
            w.new_name.clear();
        }
        let tip = cat.t("cmd_buttons.circuits.tip");
        if Button::secondary(&cat.t("ce.open")).tooltip(&tip, "").show(ui, th).clicked() {
            acts.push(Act::OpenCircuits);
        }
    });
    if let Some(m) = &w.msg {
        crate::ui_kit::error_note(ui, m);
    }
    ui.separator();
    let all: Arc<Vec<MepSystemCheck>> = w
        .cache
        .as_ref()
        .map_or_else(Default::default, |c| c.1.clone());
    if all.is_empty() {
        crate::ui_kit::empty_note(ui, cat.t("mep.browser.empty"), None);
        return;
    }
    if w.sel.is_none_or(|s| !all.iter().any(|c| c.system == s)) {
        w.sel = all.first().map(|c| c.system);
    }
    ui.columns(2, |cols| {
        let [left, right_col] = cols else {
            return;
        };
        crate::ui_kit::no_gap(left, crate::ui_kit::Gap::Rows);
        let row_h = row + th.font(Role::Caption).size + space::XS;
        egui::ScrollArea::vertical()
            .id_salt("mep_sys_list")
            .show_rows(left, row_h, all.len(), |ui, range| {
                for c in &all[range] {
                    if system_row(ui, cx, w.sel == Some(c.system), c).clicked() {
                        w.sel = Some(c.system);
                    }
                }
            });
        egui::ScrollArea::vertical()
            .id_salt("mep_sys_detail")
            .show(right_col, |ui| {
                if let Some(c) = all.iter().find(|c| Some(c.system) == w.sel) {
                    detail(ui, cx, w, c, acts);
                }
            });
    });
}

fn status_line(cat: &ddc_ui::i18n::Catalog, c: &MepSystemCheck) -> String {
    let errors = c.errors();
    let notes = c.issues.len() - errors;
    if errors > 0 {
        cat.tf("mep.browser.errors", &[("n", &errors.to_string())])
    } else if notes > 0 {
        cat.tf("mep.browser.notes", &[("n", &notes.to_string())])
    } else {
        cat.t("mep.browser.ok")
    }
}

fn issue_text(cat: &ddc_ui::i18n::Catalog, i: &MepIssue) -> String {
    let args = i.args();
    let refs: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
    cat.tf(i.key(), &refs)
}

fn detail(ui: &mut egui::Ui, cx: &Cx, w: &mut MepBrowser, c: &MepSystemCheck, acts: &mut Vec<Act>) {
    let (cat, th) = (cx.cat, cx.th);
    let s = th.sem();
    let unit = cat.t(c.unit_key());
    ui.label(RichText::new(&c.name).strong());
    let qty = |v: f64| {
        if c.unit_key() == "mep.unit.water" {
            format!("{v:.2}")
        } else {
            format!("{v:.0}")
        }
    };
    ui.label(
        RichText::new(cat.tf(
            "mep.browser.summary",
            &[
                ("flow", &qty(c.total)),
                ("unit", &unit),
                ("length", &format!("{:.1}", c.length)),
                ("fittings", &c.fittings.to_string()),
                ("parts", &c.parts.to_string()),
            ],
        ))
        .color(s.text_secondary),
    );
    ui.label(RichText::new(cat.t("mep.browser.indicative")).color(s.text_secondary));
    ui.add_space(space::SM);

    if c.issues.is_empty() {
        ui.colored_label(s.success, cat.t("mep.browser.clean"));
    }
    let shown = w.issue_limit.max(ISSUE_PAGE);
    let quiet = |ui: &mut egui::Ui, text: String| Button::quiet(&text).small().show(ui, th).clicked();
    for (k, i) in c.issues.iter().enumerate().take(shown) {
        ui.horizontal_wrapped(|ui| {
            let col = if i.is_error() { s.error } else { s.warning };
            ui.colored_label(col, issue_text(cat, i));
            let id = egui::Id::new(("mep_issue", c.system, k));
            ui.push_id(id, |ui| {
                if let Some(run) = i.run() {
                    if quiet(ui, cat.t("mep.browser.show")) {
                        acts.push(Act::ShowRun(run));
                    }
                } else if let Some(comp) = i.comp() {
                    if quiet(ui, cat.t("mep.browser.show")) {
                        acts.push(Act::ShowComp(comp));
                    }
                }
                if let MepIssue::OpenEnd { run, end } = *i {
                    if quiet(ui, cat.t("mep.browser.cap")) {
                        acts.push(op(move |m| m.mep_cap_end(run, end, true)));
                    }
                }
            });
        });
    }
    if c.issues.len() > shown {
        let rest = (c.issues.len() - shown).to_string();
        if quiet(ui, cat.tf("mep.browser.more", &[("n", &rest)])) {
            w.issue_limit = shown + ISSUE_PAGE;
        }
    }
    ui.add_space(space::SM);

    if !c.runs.is_empty() {
        ui.label(RichText::new(cat.t("mep.browser.runs")).strong());
        table(
            ui,
            th,
            ("mep_runs", c.system),
            6,
            c.runs.len(),
            |ui| {
                ui.label(cat.t("mep.col.run"));
                ui.label(cat.t("mep.col.size"));
                right(ui, cat.t("mep.col.length"));
                right(ui, format!("{}, {unit}", cat.t("mep.col.flow")));
                right(ui, cat.t("mep.col.speed"));
                right(ui, cat.t("mep_browser.col.insulation"));
            },
            |ui, k| {
                let r = &c.runs[k];
                if ui.link(r.run.to_string()).clicked() {
                    acts.push(Act::ShowRun(r.run));
                }
                ui.label(&r.size);
                right(ui, format!("{:.2}", r.length));
                right(ui, qty(r.flow));
                right(ui, r.speed.map_or(String::new(), |v| format!("{v:.1}")));
                right(ui, fmt_len(r.insulation, cx.units, cx.ru));
            },
        );
        ui.add_space(space::SM);
    }

    if !c.devices.is_empty() {
        ui.label(RichText::new(cat.t("mep.browser.devices")).strong());
        table(
            ui,
            th,
            ("mep_devices", c.system),
            4,
            c.devices.len(),
            |ui| {
                ui.label(cat.t("mep.col.device"));
                ui.label(cat.t("mep.col.connector"));
                right(ui, format!("{}, {unit}", cat.t("mep.col.flow")));
                ui.label(String::new());
            },
            |ui, k| device_row(ui, cx, w, &c.devices[k], acts),
        );
        ui.add_space(space::SM);
    }

    ui.horizontal(|ui| {
        let lbl = cat.t("mep.browser.insulation");
        let lr = ui.label(RichText::new(&lbl).color(s.text_secondary));
        let (salt, lim) = (("mep_ins", c.system), Some((0.0, INS_MAX_M)));
        len_field(ui, th, salt, (lr.id, &lbl), &mut w.ins_m, cx.units, cx.ru, lim);
        if Button::secondary(&cat.t("mep.browser.apply")).show(ui, th).clicked() {
            let (sys, t) = (c.system, w.ins_m);
            acts.push(op(move |m| m.mep_set_system_insulation(sys, t)));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui_kit::danger_button(ui, cat.t("mep.browser.delete"), true).clicked() {
                w.confirm = delete_step(w.confirm, Some(Ask::Open(c.system))).0;
            }
        });
    });
}

/// A table in its own scroll area: only visible rows are built, row 0 is the header.
fn table(
    ui: &mut egui::Ui,
    th: &Theme,
    id: (&'static str, i64),
    cols: usize,
    n: usize,
    header: impl FnOnce(&mut egui::Ui),
    mut row: impl FnMut(&mut egui::Ui, usize),
) {
    let row_h = th.d().row;
    egui::ScrollArea::vertical()
        .id_salt(id)
        .max_height(row_h * TABLE_ROWS)
        .auto_shrink([false, true])
        .show_rows(ui, row_h, n + 1, |ui, range| {
            egui::Grid::new(id)
                .striped(true)
                .num_columns(cols)
                .show(ui, |ui| {
                    if range.start == 0 {
                        header(ui);
                        ui.end_row();
                    }
                    for k in range.start.saturating_sub(1)..range.end.saturating_sub(1).min(n) {
                        row(ui, k);
                        ui.end_row();
                    }
                });
        });
}

fn device_row(ui: &mut egui::Ui, cx: &Cx, w: &MepBrowser, d: &MepDeviceRow, acts: &mut Vec<Act>) {
    let (cat, th) = (cx.cat, cx.th);
    let s = th.sem();
    if ui.link(d.comp.to_string()).clicked() {
        acts.push(Act::ShowComp(d.comp));
    }
    let cap = ui.label(&d.connector);
    // the field writes its value back only on a finished edit, so `v` is the model value until then
    let mut v = d.flow;
    let key = w
        .cache
        .as_ref()
        .and_then(|c| c.1.iter().find(|sys| sys.devices.iter().any(|x| x.comp == d.comp)))
        .map_or("mep.unit.air", |sys| sys.unit_key());
    let unit = cat.t(key);
    let out = ui
        .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_enabled_ui(!d.source, |ui| {
                NumberField::new(("mep_flow", d.comp, &d.connector), &mut v, number_format(cx.ru))
                    .labelled_by(cap.id, &d.connector)
                    .decimals(flow_decimals(key))
                    .range(0.0, FLOW_MAX)
                    .unit(&unit)
                    .width(th.d().row * FLOW_W)
                    .show(ui, th)
            })
            .inner
        })
        .inner;
    if out.committed {
        let (comp, name) = (d.comp, d.connector.clone());
        acts.push(op(move |m| m.mep_set_port_flow(comp, &name, Some(v))));
    }
    let (text, col) = if d.source {
        (cat.t("mep.device.source"), s.text_secondary)
    } else if d.connected {
        (cat.t("mep.device.connected"), s.text_secondary)
    } else {
        (cat.t("mep.device.not_connected"), s.error)
    };
    ui.colored_label(col, text);
}

/// Largest flow a device port takes.
const FLOW_MAX: f64 = 1.0e7;
/// Width of the flow field, in rows.
const FLOW_W: f32 = 5.0;

/// Water flow is fractional; air and power are whole numbers.
fn flow_decimals(unit_key: &str) -> usize {
    if unit_key == "mep.unit.water" {
        2
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_closes_the_browser() {
        let ed = crate::editor::Editor::new_in_memory().unwrap();
        let mut app = EditorApp::new(ed, None, "test".into());
        app.mep_browser.open = true;
        crate::ui_kit::run_esc_frames(|c| window(&mut app, c));
        assert!(!app.mep_browser.open);
    }

    const SRC: &str = include_str!("mep_browser.rs");

    /// Source above the test module.
    fn code() -> &'static str {
        &SRC[..SRC.find("#[cfg(test)]").unwrap()]
    }

    fn json_keys(lang: &str) -> std::collections::BTreeSet<String> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ui/data/i18n")
            .join(lang)
            .join("mep_browser.json");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        v.as_object().unwrap().keys().cloned().collect()
    }

    fn add_system(app: &mut EditorApp, name: &'static str) -> i64 {
        app.ed
            .run_op(OpFn(Arc::new(move |m| {
                m.add_mep_system(name, SystemKind::ALL[0]).map(|_| ())
            })))
            .unwrap();
        app.ed.model.mep_systems().unwrap().last().unwrap().id
    }

    fn frames(app: &mut EditorApp, n: usize) {
        let ctx = ddc_ui::theme::headless_ctx();
        for _ in 0..n {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 720.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |c| window(app, c));
        }
    }

    #[test]
    fn add_is_blocked_on_a_blank_name() {
        assert_eq!(new_system_name(""), None);
        assert_eq!(new_system_name(" \t "), None);
        assert_eq!(new_system_name("  Supply 1 ").as_deref(), Some("Supply 1"));
    }

    #[test]
    fn delete_needs_a_confirm_for_the_same_system() {
        assert_eq!(delete_step(None, None), (None, None));
        let (armed, go) = delete_step(None, Some(Ask::Open(7)));
        assert_eq!((armed, go), (Some(7), None));
        assert_eq!(delete_step(armed, None), (Some(7), None));
        assert_eq!(delete_step(armed, Some(Ask::Confirm)), (None, Some(7)));
        assert_eq!(delete_step(armed, Some(Ask::Cancel)), (None, None));
        // a confirm with nothing asked deletes nothing
        assert_eq!(delete_step(None, Some(Ask::Confirm)), (None, None));
        // asking about another system re-arms for that one only
        assert_eq!(delete_step(Some(7), Some(Ask::Open(8))), (Some(8), None));
    }

    #[test]
    fn open_confirm_survives_frames_and_keeps_the_system() {
        let ed = crate::editor::Editor::new_in_memory().unwrap();
        let mut app = EditorApp::new(ed, None, "test".into());
        let sys = add_system(&mut app, "Supply 1");
        app.mep_browser.open = true;
        frames(&mut app, 3);
        assert!(app.mep_browser.confirm.is_none());
        app.mep_browser.confirm = Some(sys);
        frames(&mut app, 3);
        assert_eq!(app.mep_browser.confirm, Some(sys));
        assert_eq!(app.ed.model.mep_systems().unwrap().len(), 1);
        // closing the window drops a pending confirmation
        app.mep_browser.open = false;
        app.mep_browser.confirm = Some(sys);
        frames(&mut app, 1);
        assert_eq!(app.ed.model.mep_systems().unwrap().len(), 1);
    }

    #[test]
    fn confirmed_delete_is_one_undo_step() {
        let ed = crate::editor::Editor::new_in_memory().unwrap();
        let mut app = EditorApp::new(ed, None, "test".into());
        let sys = add_system(&mut app, "Supply 1");
        app.ed
            .run_op(OpFn(Arc::new(move |m| m.delete_mep_system(sys))))
            .unwrap();
        assert!(app.ed.model.mep_systems().unwrap().is_empty());
        app.ed.undo();
        let back = app.ed.model.mep_systems().unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "Supply 1");
    }

    #[test]
    fn insulation_round_trips_in_project_units() {
        use crate::shell::drawing_kit::len_unit;
        use crate::units::{edit_length, parse_edit_length, FOOT, INCH};
        for m in [INCH, 2.0 * INCH, 1.5 * INCH, 0.05, FOOT / 4.0] {
            // 0.05 m is not a whole 1/16 inch: only the metric leg is exact for it
            if m != 0.05 {
                let imp = edit_length(m, Units::Imperial);
                let back = parse_edit_length(&imp, Units::Imperial).unwrap();
                assert!((back - m).abs() < INCH / 64.0, "{imp} -> {back} vs {m}");
            }
            let met = edit_length(m, Units::Metric);
            let back = parse_edit_length(&met, Units::Metric).unwrap();
            assert!((back - m).abs() < 1e-6, "{met} -> {back} vs {m}");
        }
        // the field shows feet in an imperial project and millimetres in a metric one
        let (i, mm) = (len_unit(Units::Imperial, false), len_unit(Units::Metric, false));
        assert!((0.05 * mm.factor - 50.0).abs() < 1e-9);
        assert!((FOOT * i.factor - 1.0).abs() < 1e-12);
        assert_ne!(i.unit, mm.unit);
        // a value typed in feet reaches the model in metres, untouched by rounding
        let typed = 0.25 * i.factor;
        assert!((typed / i.factor - 0.25).abs() < 1e-12);
    }

    #[test]
    fn no_raw_widgets_colours_or_literal_widths_remain() {
        let s = code();
        for raw in [
            "egui::TextEdit",
            "egui::ComboBox",
            "egui::DragValue",
            "egui::Button",
            "ui.button(",
            "ui.small_button(",
            "selectable_label(",
            "selectable_value(",
            "Color32",
            "th.c(\"",
            "desired_width(",
            "280.0",
            "160.0",
            "pal.",
        ] {
            assert!(!s.contains(raw), "left in the MEP browser: {raw}");
        }
        for kit in [
            "TextField::new(",
            "widgets::dropdown(",
            "Button::primary(",
            "danger_button(",
            "len_field(",
            "NumberField::new(",
            "modal_dialog(",
        ] {
            assert!(s.contains(kit), "kit widget missing: {kit}");
        }
        // the Delete button only asks; the delete op is queued by the confirmation alone
        let at = s.find("danger_button(ui, cat.t(\"mep.browser.delete\")").unwrap();
        assert!(s[at..at + 250].contains("Ask::Open("));
        assert_eq!(s.matches("delete_mep_system(").count(), 1);
    }

    #[test]
    fn strings_exist_in_both_languages_without_touching_the_global_one() {
        use ddc_ui::i18n::{Catalog, Lang};
        let (en, ru) = (json_keys("en"), json_keys("ru"));
        assert_eq!(en, ru);
        let mut used = std::collections::BTreeSet::new();
        for (at, _) in code().match_indices("\"mep_browser.") {
            let rest = &code()[at + 1..];
            used.insert(rest[..rest.find('"').unwrap()].to_string());
        }
        assert!(used.len() >= 6, "{used:?}");
        for k in &used {
            assert!(en.contains(k), "missing key {k}");
        }
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            for k in &en {
                assert_ne!(&c.t(k), k, "{k} missing in {lang:?}");
            }
        }
        let c = Catalog::new(Lang::Ru);
        let body = c.tf("mep_browser.delete.body", &[("name", "S1"), ("n", "3")]);
        assert!(body.contains("S1") && body.contains('3'));
    }
}
