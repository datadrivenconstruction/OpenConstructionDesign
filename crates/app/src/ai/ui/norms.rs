// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Norm checks on the Checks tab: rules are data with a cited clause, the model is judged by
//! code, and the assistant only explains a finding or drafts a fix when asked. Each row names the
//! rule, its document and clause, the measured and required values; a click selects and frames
//! the element. The findings export to xlsx and to BCF-like JSON.

use super::checks::{element_list, finding_card, fmt_value, tone_of, Target};
use super::kit::{self, space, Button, Tone};
use super::{Action, Cx, PanelUi};
use crate::shell::EditorApp;
use crate::units::{self, Units};
use ddc_model::checks::norm_kinds::PackCtx;
use ddc_model::checks::norms::{
    findings_json_for, pack_norm_rules, run_norms, run_pack_norms, text_key, STARTER_PACK_ID, NormFinding, NormRule,
};
use ddc_model::checks::{CheckScope, Severity};
use ddc_model::checks::norm_pack::{country_for_locale, nearest_pack, NormPack, PackSet};
use ddc_model::checks::{NormLocale, NormProject, FIRE_CLASSES};
use ddc_model::{ElKey, Model};
use ddc_ui::notify::Level;
use ddc_ui::theme::Role;
use ddc_ui::widgets::field::TextField;
use ddc_ui::{Catalog, Lang, Theme};
use egui::{self, RichText, Ui};
use std::path::Path;

/// Rows drawn; the exports carry every finding.
const ROWS_MAX: usize = ddc_ai::limits::NORM_ROWS.us();
/// Elements named in the fix prompt.
const PROMPT_ELS: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Do {
    Json,
    Xlsx,
    /// Add the project parameters the rules read (purpose, occupants, exit flags).
    AddFields,
    /// Fire class by position in `FIRE_CLASSES`; `None` clears it.
    Class(Option<usize>),
    /// Building height typed by the engineer, m; `None` takes it from the levels.
    Height(Option<f64>),
    /// Country pack by position in the shipped pack ids; `None` follows the UI language.
    Pack(Option<usize>),
}

pub struct Norms {
    ran: bool,
    failed: bool,
    rules: Vec<NormRule>,
    /// Why the shipped pack did not load; shown instead of an empty, clean-looking list.
    load_error: Option<String>,
    /// Norm parameters the project lacks; refreshed every frame.
    pub(crate) missing: Vec<&'static str>,
    findings: Vec<NormFinding>,
    /// Severities switched off in the list, in `Severity` order.
    hidden: [bool; 3],
    /// Building inputs as stored; refreshed every frame.
    pub(crate) building: NormProject,
    pub(crate) height_levels: Option<f64>,
    pub(crate) people_upper: Option<i64>,
    height_text: String,
    height_focus: bool,
    /// Pack ids shipped with the editor.
    pub(crate) packs: Vec<String>,
    /// The stored pack choice; refreshed every frame.
    pub(crate) locale: NormLocale,
    /// The pack the UI language points to, shown on the Auto entry.
    pub(crate) auto_pack: String,
    /// The pack in use, resolved once per choice: labels a partial pack, goes into the export.
    chosen: Option<NormPack>,
}

impl Default for Norms {
    fn default() -> Self {
        let mut n = Norms {
            ran: false,
            failed: false,
            rules: vec![],
            load_error: None,
            missing: vec![],
            findings: vec![],
            hidden: [false; 3],
            building: NormProject::default(),
            height_levels: None,
            people_upper: None,
            height_text: String::new(),
            height_focus: false,
            packs: PackSet::embedded().ids().iter().map(|s| s.to_string()).collect(),
            locale: NormLocale::default(),
            auto_pack: String::new(),
            chosen: None,
        };
        // pack rows are judged by the pack checker, so the starter pack is the chosen one
        n.refresh_chosen(STARTER_PACK_ID);
        n
    }
}

impl Norms {
    /// Resolves the project's pack (`Model::effective_norm_pack`) when it changed, not every frame.
    pub(crate) fn refresh_chosen(&mut self, id: &str) {
        if self.chosen.as_ref().is_some_and(|p| p.id == id) {
            return;
        }
        // the rules, the label and the export all come from this one pack; old rows go
        match shipped_packs().resolve(id) {
            Ok(p) => {
                self.rules = pack_norm_rules(&p);
                self.load_error = None;
                self.chosen = Some(p);
            }
            Err(e) => {
                self.rules.clear();
                self.load_error = Some(e.to_string());
                self.chosen = None;
            }
        }
        self.findings.clear();
        self.ran = false;
        self.failed = false;
    }

    #[cfg(test)]
    pub(crate) fn ran_on(m: &Model) -> Self {
        let mut n = Norms::default();
        n.run(m);
        n
    }

    /// Runs `pack` on `m`, for fixtures that need a country pack's rules.
    #[cfg(test)]
    pub(crate) fn ran_with(m: &Model, pack: &str) -> Self {
        let mut n = Norms::default();
        n.refresh_chosen(pack);
        n.run(m);
        n
    }

    pub fn run(&mut self, m: &Model) {
        self.ran = true;
        let found = match &self.chosen {
            Some(p) => {
                // the same context the AI check builds from the stored project
                let ctx = PackCtx {
                    typology: (!self.locale.typology.is_empty()).then(|| self.locale.typology.clone()),
                    region: (!self.locale.region.is_empty()).then(|| self.locale.region.clone()),
                    ..PackCtx::default()
                };
                run_pack_norms(m, p, &self.rules, &CheckScope::All, &ctx)
            }
            None => run_norms(m, &self.rules, &CheckScope::All),
        };
        match found {
            Ok(f) => {
                self.findings = f;
                self.failed = false;
            }
            Err(_) => {
                self.findings.clear();
                self.failed = true;
            }
        }
    }

    fn shown(&self) -> impl Iterator<Item = &NormFinding> {
        self.findings
            .iter()
            .filter(|f| !self.hidden[sev_index(f.severity)])
    }
}

const fn sev_index(s: Severity) -> usize {
    match s {
        Severity::Error => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
    }
}

const fn filter_key(s: Severity) -> &'static str {
    match s {
        Severity::Error => "norms.filter.error",
        Severity::Warning => "norms.filter.warning",
        Severity::Info => "norms.filter.info",
    }
}

const SEVERITIES: [Severity; 3] = [Severity::Error, Severity::Warning, Severity::Info];

/// `measured 0.7 m, limit 0.8 m` in project units; empty when the finding has no numbers.
fn values_text(cat: &Catalog, f: &NormFinding, u: Units) -> String {
    let (Some(v), Some(l)) = (f.measured, f.required) else {
        return String::new();
    };
    cat.tf(
        "ai_checks.check.measure",
        &[
            ("value", &fmt_value(cat, v, f.unit, u)),
            ("limit", &fmt_value(cat, l, f.unit, u)),
        ],
    )
}

/// The section of the Checks tab.
pub fn section(
    ui: &mut Ui,
    nm: &mut Norms,
    cx: &Cx<'_>,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    nm.missing = cx.ed.model.norm_params_missing().unwrap_or_default();
    nm.building = cx.ed.model.norm_project().unwrap_or_default();
    nm.height_levels = cx.ed.model.building_height().ok().flatten();
    nm.people_upper = cx.ed.model.people_upper_floor().ok().flatten();
    nm.locale = cx.ed.model.norm_locale().unwrap_or_default();
    nm.auto_pack = auto_pack(cx.cat.lang.code());
    let in_effect = cx.ed.model.effective_norm_pack(shipped_packs(), cx.cat.lang.code(), None);
    nm.refresh_chosen(&in_effect.unwrap_or_else(|_| nm.auto_pack.clone()));
    if view(ui, nm, cx.th, cx.cat, units::active_units(), acts, hot) {
        nm.run(&cx.ed.model);
    }
}

/// The section without the editor: true when the person asked for a run.
pub(crate) fn view(
    ui: &mut Ui,
    nm: &mut Norms,
    th: &Theme,
    cat: &Catalog,
    u: Units,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) -> bool {
    kit::section_title(ui, th, &cat.t("norms.title"));
    if let Some(why) = &nm.load_error {
        kit::error_note(ui, th, &cat.tf("norms.pack_error", &[("why", why)]));
        return false;
    }
    note(ui, th, &cat.t("norms.example_note"));
    if !nm.missing.is_empty() {
        let text = cat.tf("norms.fields_missing", &[("names", &nm.missing.join(", "))]);
        note(ui, th, &text);
        if Button::secondary(&cat.t("norms.fields_add"))
            .small()
            .tooltip(&cat.t("norms.fields_add_tip"), "")
            .show(ui, th)
            .clicked()
        {
            acts.push(Action::Norms(Do::AddFields));
        }
    }
    building(ui, nm, th, cat, acts);
    let mut run = false;
    ui.horizontal_wrapped(|ui| {
        let label = if nm.ran {
            cat.t("norms.run_again")
        } else {
            cat.t("norms.run")
        };
        run = Button::secondary(&label).small().show(ui, th).clicked();
        if nm.ran && !nm.findings.is_empty() {
            if Button::quiet(&cat.t("norms.export_xlsx"))
                .small()
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::Norms(Do::Xlsx));
            }
            if Button::quiet(&cat.t("norms.export_json"))
                .small()
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::Norms(Do::Json));
            }
        }
    });
    if !nm.ran {
        note(ui, th, &cat.t("norms.never"));
        return run;
    }
    if nm.failed {
        kit::error_note(ui, th, &cat.t("ai.check.failed"));
        return run;
    }
    if nm.findings.is_empty() {
        note(
            ui,
            th,
            &cat.tf("norms.clean", &[("n", &nm.rules.len().to_string())]),
        );
        return run;
    }
    ui.horizontal_wrapped(|ui| {
        for s in SEVERITIES {
            let n = nm.findings.iter().filter(|f| f.severity == s).count();
            if n == 0 {
                continue;
            }
            let key = filter_key(s);
            let label = format!("{} ({n})", cat.t(key));
            let on = !nm.hidden[sev_index(s)];
            let b = if on {
                Button::secondary(&label)
            } else {
                Button::quiet(&label)
            };
            if b.small().show(ui, th).clicked() {
                nm.hidden[sev_index(s)] = on;
            }
        }
    });
    let total = nm.shown().count();
    let shown = total.min(ROWS_MAX);
    let count = cat.tf("ai.check.count", &[("n", &total.to_string())]);
    kit::caption(ui, th, &count);
    if total > shown {
        let cap = cat.tf(
            "ai_checks.check.cap",
            &[("shown", &shown.to_string()), ("total", &total.to_string())],
        );
        kit::warn_note(ui, th, &cap);
    }
    let rows: Vec<(NormFinding, NormRule)> = nm
        .shown()
        .take(ROWS_MAX)
        .filter_map(|f| nm.rules.get(f.rule_index).map(|r| (f.clone(), r.clone())))
        .collect();
    for (f, r) in &rows {
        row(ui, th, cat, u, f, r, acts, hot);
        ui.add_space(space::XS);
    }
    run
}

/// Fire class, building height and the people on the busiest upper floor: what the stair
/// width rule reads from the building as a whole.
fn shipped_packs() -> &'static PackSet {
    static SET: std::sync::OnceLock<PackSet> = std::sync::OnceLock::new();
    SET.get_or_init(PackSet::embedded)
}

/// The shipped pack nearest to a UI language.
pub(crate) fn auto_pack(lang: &str) -> String {
    nearest_pack(shipped_packs(), &country_for_locale(lang, None))
}

fn pack_choice(ui: &mut Ui, nm: &Norms, th: &Theme, cat: &Catalog, acts: &mut Vec<Action>) {
    let auto = cat.tf("norms.pack_auto", &[("pack", &nm.auto_pack)]);
    let mut items: Vec<&str> = vec![auto.as_str()];
    items.extend(nm.packs.iter().map(String::as_str));
    let mut i = nm
        .packs
        .iter()
        .position(|p| *p == nm.locale.pack)
        .map_or(0, |p| p + 1);
    kit::caption(ui, th, &cat.t("norms.pack"));
    if kit::dropdown(ui, th, "norms.pack", &mut i, &items, 160.0) {
        acts.push(Action::Norms(Do::Pack(i.checked_sub(1))));
    }
    note(ui, th, &cat.t("norms.pack_tip"));
    if let Some(t) = nm.chosen.as_ref().and_then(|p| partial_note(cat, p)) {
        note(ui, th, &t);
    }
}

/// The label of a pack that does not state every key of the reference set; None when complete.
pub(crate) fn partial_note(cat: &Catalog, p: &NormPack) -> Option<String> {
    (!p.complete()).then(|| cat.tf("norms.pack_partial", &[("pack", &p.id)]))
}

fn building(ui: &mut Ui, nm: &mut Norms, th: &Theme, cat: &Catalog, acts: &mut Vec<Action>) {
    pack_choice(ui, nm, th, cat, acts);
    kit::caption(ui, th, &cat.t("norms.building"));
    let none = cat.t("norms.class_none");
    let mut items: Vec<&str> = vec![none.as_str()];
    items.extend(FIRE_CLASSES);
    let mut i = FIRE_CLASSES
        .iter()
        .position(|c| *c == nm.building.fire_class)
        .map_or(0, |p| p + 1);
    if kit::dropdown(ui, th, "norms.class", &mut i, &items, 120.0) {
        acts.push(Action::Norms(Do::Class(i.checked_sub(1))));
    }
    if !nm.height_focus {
        nm.height_text = nm
            .building
            .height_m
            .map(|h| h.to_string())
            .unwrap_or_default();
    }
    let label = cat.t("norms.height");
    let hint = nm
        .height_levels
        .map_or(String::new(), |h| format!("{h:.2}"));
    let caption = RichText::new(&label)
        .font(th.font(Role::Caption))
        .color(th.c("text.secondary"));
    let l = ui.label(caption);
    let o = TextField::new(&mut nm.height_text)
        .id_salt("norms.height")
        .labelled_by(l.id, &label)
        .hint(&hint)
        .width(120.0)
        .show(ui, th);
    nm.height_focus = o.response.has_focus();
    if o.committed {
        // a height in metres: either decimal mark whatever the UI language, digits of any script
        let t = nm.height_text.trim();
        match (t.is_empty(), ddc_ui::numparse::parse(t, ddc_ui::numparse::DecimalMark::Comma)) {
            (true, _) => acts.push(Action::Norms(Do::Height(None))),
            (false, Ok(h)) if h.is_finite() && h >= 0.0 => {
                acts.push(Action::Norms(Do::Height(Some(h))));
            }
            _ => {}
        }
    }
    let people = nm.people_upper.map_or(String::from("-"), |n| n.to_string());
    note(
        ui,
        th,
        &cat.tf("norms.building_info", &[("people", &people)]),
    );
}

fn note(ui: &mut Ui, th: &Theme, text: &str) {
    ui.add(
        egui::Label::new(
            RichText::new(text)
                .font(th.font(Role::Caption))
                .color(th.c("text.secondary")),
        )
        .wrap(),
    );
}

#[allow(clippy::too_many_arguments)]
fn row(
    ui: &mut Ui,
    th: &Theme,
    cat: &Catalog,
    u: Units,
    f: &NormFinding,
    r: &NormRule,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (tone, key) = tone_of(f.severity);
    let title = rule_title(cat, r);
    let values = values_text(cat, f, u);
    let target = Target {
        elements: &f.elements,
        at: f.at,
    };
    finding_card(
        ui,
        th,
        cat,
        target,
        hot,
        acts,
        |ui| {
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    kit::badge(ui, th, &cat.t(key), tone);
                    if r.example {
                        kit::badge(ui, th, &cat.t("norms.example"), Tone::Neutral);
                    }
                });
                let head = ui.add(
                    egui::Label::new(
                        RichText::new(&title)
                            .font(th.font(Role::Label))
                            .color(th.c("text.primary")),
                    )
                    .wrap()
                    .sense(egui::Sense::click()),
                );
                let cited = format!("{} {}", r.doc, f.clause_of(r));
                note(ui, th, &cat.tf("norms.clause", &[("clause", &cited)]));
                if !values.is_empty() {
                    note(ui, th, &values);
                }
                if !f.elements.is_empty() {
                    let n = f.elements.len().to_string();
                    note(ui, th, &cat.tf("norms.elements", &[("n", &n)]));
                }
                head
            })
            .inner
        },
        |ui, acts| {
            if Button::quiet(&cat.t("norms.ask"))
                .small()
                .tooltip(&cat.t("norms.ask_tip"), "")
                .show(ui, th)
                .clicked()
            {
                let cited = format!("{} {}", r.doc, f.clause_of(r));
                let els = element_list(&f.elements[..f.elements.len().min(PROMPT_ELS)]);
                let prompt = cat.tf(
                    "norms.ask_prompt",
                    &[
                        ("rule", r.id()),
                        ("clause", &cited),
                        ("els", &els),
                        ("values", &values),
                    ],
                );
                acts.push(Action::AskFix(prompt));
            }
        },
    );
}

/// Rule title in the catalog language: the pack's inline text, then the locale bundle
/// (`norm_rule.<id>`), each along the language chain; English from the pack otherwise.
pub(crate) fn rule_title(cat: &Catalog, r: &NormRule) -> String {
    let key = text_key(r.id());
    cat.lang
        .chain()
        .take_while(|l| *l != Lang::En)
        .find_map(|l| {
            r.title_in(l.code())
                .map(str::to_string)
                .or_else(|| cat.table(l).get(&key).and_then(|v| v.as_str()).map(str::to_string))
        })
        .unwrap_or_else(|| r.title().to_string())
}

/// `rules` with the catalog-language title added inline, so an export carries it.
fn localized(cat: &Catalog, rules: &[NormRule]) -> Vec<NormRule> {
    let code = cat.lang.code();
    rules
        .iter()
        .map(|r| {
            let mut r = r.clone();
            let t = rule_title(cat, &r);
            if cat.lang != Lang::En && t != r.title() {
                r.titles.insert(code.to_string(), t);
            }
            r
        })
        .collect()
}

fn severity_label(cat: &Catalog, s: Severity) -> String {
    cat.t(tone_of(s).1)
}

/// The findings as sheet rows under the headings of the catalog language.
pub fn sheet_rows(
    cat: &Catalog,
    rules: &[NormRule],
    findings: &[NormFinding],
) -> (Vec<String>, Vec<Vec<Cell>>) {
    let head = [
        "norms.col.severity",
        "norms.col.rule",
        "norms.col.title",
        "norms.col.document",
        "norms.col.clause",
        "norms.col.example",
        "norms.col.elements",
        "norms.col.measured",
        "norms.col.required",
        "norms.col.unit",
    ]
    .iter()
    .map(|k| cat.t(k))
    .collect();
    let rows = findings
        .iter()
        .filter_map(|f| {
            let r = rules.get(f.rule_index)?;
            Some(vec![
                Cell::Text(severity_label(cat, f.severity)),
                Cell::Text(r.id().to_string()),
                Cell::Text(rule_title(cat, r)),
                Cell::Text(r.doc.clone()),
                Cell::Text(f.clause_of(r).to_string()),
                Cell::Text(cat.t(if r.example { "norms.yes" } else { "norms.no" })),
                Cell::Text(element_list(&f.elements)),
                f.measured.map_or(Cell::Empty, Cell::Num),
                f.required.map_or(Cell::Empty, Cell::Num),
                Cell::Text(f.unit.to_string()),
            ])
        })
        .collect();
    (head, rows)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Text(String),
    Num(f64),
    Empty,
}

pub fn write_xlsx(
    cat: &Catalog,
    rules: &[NormRule],
    findings: &[NormFinding],
    pack: Option<&NormPack>,
    path: &Path,
) -> Result<(), String> {
    let (head, rows) = sheet_rows(cat, rules, findings);
    let Some(p) = pack else {
        return ddc_io::requirements::save_table(path, &cat.t("norms.sheet"), &head, &rows);
    };
    // the findings sheet is unchanged; the pack and its coverage go on a sheet of their own
    let missing = p.missing();
    let info = [
        ("pack".to_string(), p.id.clone()),
        ("version".to_string(), p.meta["version"].as_str().unwrap_or("").to_string()),
        ("complete".to_string(), p.complete().to_string()),
        ("missing".to_string(), missing.len().to_string()),
        ("missing_keys".to_string(), missing.join(", ")),
    ];
    ddc_io::requirements::save_table_with_info(path, &cat.t("norms.sheet"), &head, &rows, &cat.t("norms.sheet_pack"), &info)
}

impl ddc_io::requirements::ToCell for Cell {
    fn cell(&self) -> ddc_io::requirements::TableCell<'_> {
        use ddc_io::requirements::TableCell;
        match self {
            Cell::Text(t) => TableCell::Text(t),
            Cell::Num(x) => TableCell::Num(*x),
            Cell::Empty => TableCell::Empty,
        }
    }
}

/// Findings with the pack in use; a partial pack is labelled `complete: false` in the file.
pub fn write_json(rules: &[NormRule], findings: &[NormFinding], pack: Option<&NormPack>, path: &Path) -> Result<(), String> {
    std::fs::write(path, findings_json_for(rules, findings, pack)).map_err(|e| e.to_string())
}

/// Asks for a file name and writes the findings; false when the person cancelled.
fn export(app: &mut EditorApp, nm: &Norms, d: Do) -> Result<bool, String> {
    let (ext, name) = match d {
        Do::Xlsx => ("xlsx", "XLSX"),
        Do::Json => ("json", "JSON"),
        _ => return Ok(false),
    };
    let file = format!("{}.{ext}", app.sh.cat.t("norms.file"));
    let Some(path) = crate::file_pick::Dlg::new()
        .add_filter(name, &[ext])
        .set_file_name(file)
        .save_file()
    else {
        return Ok(false);
    };
    match d {
        Do::Xlsx => write_xlsx(&app.sh.cat, &nm.rules, &nm.findings, nm.chosen.as_ref(), &path)?,
        Do::Json => write_json(&localized(&app.sh.cat, &nm.rules), &nm.findings, nm.chosen.as_ref(), &path)?,
        _ => {}
    }
    Ok(true)
}

pub(super) fn act(st: &mut PanelUi, app: &mut EditorApp, d: Do) {
    if matches!(d, Do::AddFields | Do::Class(_) | Do::Height(_) | Do::Pack(_)) {
        let packs = st.checks.norms.packs.clone();
        let add = move |m: &mut Model| match d {
            Do::Pack(i) => {
                let mut p = m.norm_locale()?;
                p.pack = i.and_then(|i| packs.get(i).cloned()).unwrap_or_default();
                m.set_norm_locale(&p).map(|_| ())
            }
            Do::Class(c) => {
                let mut p = m.norm_project()?;
                p.fire_class = c.map_or(String::new(), |i| FIRE_CLASSES[i].to_string());
                m.set_norm_project(&p).map(|_| ())
            }
            Do::Height(h) => {
                let mut p = m.norm_project()?;
                p.height_m = h;
                m.set_norm_project(&p).map(|_| ())
            }
            _ => m.ensure_norm_params().map(|_| ()),
        };
        let r = app.ed.run_op(crate::editor::OpFn(std::sync::Arc::new(add)));
        match r {
            Ok(()) => {
                let text = app.sh.cat.t("norms.fields_added");
                app.sh.toasts.push(Level::Success, text, None);
            }
            Err(e) => {
                let text = app
                    .sh
                    .cat
                    .tf("norms.export_failed", &[("why", &e.to_string())]);
                app.sh.toasts.push(Level::Warning, text, None);
            }
        }
        return;
    }
    match export(app, &st.checks.norms, d) {
        Ok(true) => {
            let text = app.sh.cat.t("norms.exported");
            app.sh.toasts.push(Level::Success, text, None);
        }
        Ok(false) => {}
        Err(e) => {
            let text = app.sh.cat.tf("norms.export_failed", &[("why", &e)]);
            app.sh.toasts.push(Level::Warning, text, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{LayerSpec, OpeningSpec, WallSpec};

    fn model_with_narrow_door() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let level = m.add_level("L1", 0.0).unwrap();
        let layer = [LayerSpec {
            material: "Brick".into(),
            thickness: 0.2,
        }];
        let wt = m.add_wall_type("W200", &layer).unwrap();
        let wall = m
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (10.0, 0.0),
                level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        let kind = "door".into();
        let spec = OpeningSpec {
            along: 2.0,
            width: 0.7,
            sill: 0.0,
            height: 2.1,
            kind,
        };
        let d = m.add_opening(wall, &spec).unwrap();
        m.ensure_norm_params().unwrap();
        let pid = m.param_by_name("EvacuationExit").unwrap().unwrap().id;
        let yes = Some(ddc_model::PVal::Bool(true));
        let set = ddc_model::ParamSet {
            param: pid,
            kind: "opening".into(),
            id: d,
            value: yes,
        };
        m.set_param_values(&[set]).unwrap();
        m
    }

    #[test]
    fn a_damaged_pack_shows_its_error_instead_of_an_empty_list() {
        for lang in [Lang::En, Lang::Ru] {
            let mut nm = Norms {
                load_error: Some("bad json".into()),
                ..Norms::default()
            };
            let (run, acts) = render(&mut nm, lang);
            assert!(!run && acts.is_empty());
        }
        assert!(
            Norms::default().load_error.is_none(),
            "the shipped pack loads"
        );
        assert!(!Norms::default().rules.is_empty());
    }

    #[test]
    fn keys_exist_in_both_languages() {
        let keys = [
            "norms.title",
            "norms.example_note",
            "norms.run",
            "norms.run_again",
            "norms.never",
            "norms.clean",
            "norms.filter.error",
            "norms.filter.warning",
            "norms.filter.info",
            "norms.example",
            "norms.clause",
            "norms.elements",
            "norms.ask",
            "norms.ask_tip",
            "norms.ask_prompt",
            "norms.export_xlsx",
            "norms.export_json",
            "norms.exported",
            "norms.export_failed",
            "norms.file",
            "norms.sheet",
            "norms.yes",
            "norms.no",
            "norms.col.severity",
            "norms.col.rule",
            "norms.col.title",
            "norms.col.document",
            "norms.col.clause",
            "norms.col.example",
            "norms.col.elements",
            "norms.col.measured",
            "norms.col.required",
            "norms.col.unit",
            "norms.pack_error",
            "norms.fields_missing",
            "norms.fields_add",
            "norms.fields_add_tip",
            "norms.fields_added",
            "norms.building",
            "norms.class_none",
            "norms.height",
            "norms.building_info",
            "norms.pack",
            "norms.pack_auto",
            "norms.pack_tip",
            "norms.pack_partial",
            "norms.sheet_pack",
            "ai.check.rule.compare",
            "ai.check.rule.unset",
            "ai.check.rule.unique",
        ];
        let cat = Catalog::new(Lang::En);
        for k in keys {
            let (en, ru) = (cat.t_in(Lang::En, k), cat.t_in(Lang::Ru, k));
            assert_ne!(en, k, "{k} missing in en");
            assert_ne!(ru, en, "{k} missing in ru");
            assert!(ru.chars().any(|c| c as u32 > 0x400), "{k}: ru text {ru}");
        }
    }

    #[test]
    fn rule_titles_come_from_the_locale_bundle_with_english_fallback() {
        let (en, ru, fr) = (Catalog::new(Lang::En), Catalog::new(Lang::Ru), Catalog::new(Lang::Fr));
        for id in shipped_packs().ids() {
            let p = shipped_packs().resolve(id).unwrap();
            for r in pack_norm_rules(&p) {
                let key = text_key(r.id());
                // the English bundle is the translation source: it must match the pack text
                assert_eq!(en.table(Lang::En).get(&key).and_then(|v| v.as_str()), Some(r.title()), "{key}");
                assert_eq!(rule_title(&en, &r), r.title());
                assert_ne!(rule_title(&ru, &r), r.title(), "{key} has a translation");
                let want = fr.table(Lang::Fr).get(&key).and_then(|v| v.as_str()).unwrap_or(r.title());
                assert_eq!(rule_title(&fr, &r), want);
            }
        }
        // an inline title of an older or user pack wins over the bundle
        let mut r = pack_norm_rules(&shipped_packs().resolve("ru").unwrap()).remove(0);
        r.titles.insert("ru".into(), "inline".into());
        assert_eq!(rule_title(&ru, &r), "inline");
    }

    #[test]
    fn rows_follow_the_catalog_language_and_carry_the_clause() {
        let m = model_with_narrow_door();
        let nm = Norms::ran_with(&m, "ru");
        assert!(!nm.findings.is_empty());
        let en = Catalog::new(Lang::En);
        let ru = Catalog::new(Lang::Ru);
        let (head, rows) = sheet_rows(&en, &nm.rules, &nm.findings);
        assert_eq!(head.len(), rows[0].len());
        let width = rows
            .iter()
            .find(|r| r[1] == Cell::Text("ex.door.width".into()))
            .expect("the door width rule found the narrow door");
        assert_eq!(width[0], Cell::Text("Error".into()));
        let Cell::Text(title) = &width[2] else {
            panic!()
        };
        assert!(
            title.starts_with(
                "Evacuation exit (EvacuationExit = yes), opening width at least 0.8 m"
            ),
            "{title}"
        );
        assert_eq!(
            width[3],
            Cell::Text("SP 1.13130.2020 (amend. 1-3, 2025)".into())
        );
        assert_eq!(width[4], Cell::Text("p.4.2.19".into()));
        assert_eq!(
            (width[7].clone(), width[8].clone()),
            (Cell::Num(0.7), Cell::Num(0.8))
        );
        let (head_ru, rows_ru) = sheet_rows(&ru, &nm.rules, &nm.findings);
        assert_ne!(head, head_ru);
        assert_eq!(rows_ru.len(), rows.len());
        let Cell::Text(t) = &rows_ru[0][2] else {
            panic!()
        };
        assert!(t.chars().any(|c| c as u32 > 0x400), "{t}");
    }

    #[test]
    fn exports_write_xlsx_and_json() {
        let m = model_with_narrow_door();
        let nm = Norms::ran_with(&m, "ru");
        let dir = std::env::temp_dir().join(format!("ddc_chk1_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (x, j) = (dir.join("f.xlsx"), dir.join("f.json"));
        write_xlsx(&Catalog::new(Lang::Ru), &nm.rules, &nm.findings, None, &x).unwrap();
        let us = shipped_packs().resolve("us").unwrap();
        write_json(&nm.rules, &nm.findings, Some(&us), &j).unwrap();
        assert_eq!(&std::fs::read(&x).unwrap()[..2], b"PK");
        let text = std::fs::read_to_string(&j).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["topics"].as_array().unwrap().len(), nm.findings.len());
        assert_eq!((&v["pack"]["id"], &v["pack"]["complete"]), (&serde_json::json!("us"), &serde_json::json!(false)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_partial_pack_is_labelled_in_the_panel() {
        let cat = Catalog::new(Lang::En);
        let mut nm = Norms::default();
        // the label follows the pack the project uses: the stored choice, else the UI language
        let mut m = Model::open(":memory:").unwrap();
        assert!(m.set_norm_locale(&NormLocale { pack: "us".into(), ..NormLocale::default() }).unwrap());
        nm.refresh_chosen(&m.effective_norm_pack(shipped_packs(), "ru", None).unwrap());
        let us = nm.chosen.as_ref().unwrap();
        assert_eq!(us.id, "us");
        let t = partial_note(&cat, us).expect("us is partial");
        assert!(t.contains("us") && t != "norms.pack_partial", "{t}");
        let de = shipped_packs().resolve("de").unwrap();
        assert_eq!(partial_note(&cat, &de), None, "the complete pack has no label");
        // a new choice resolves again
        nm.refresh_chosen("de");
        assert_eq!(nm.chosen.as_ref().map(|p| p.id.as_str()), Some("de"));
    }

    #[test]
    fn the_chosen_pack_runs_its_rules_and_both_exports_name_it() {
        use calamine::{open_workbook, Data, Reader, Xlsx};
        let mut m = Model::open(":memory:").unwrap();
        let l0 = m.add_level("L0", 0.0).unwrap();
        let l1 = m.add_level("L1", 3.0).unwrap();
        // risers over 0.197 m and a width under 0.914 m: the us house stair rules
        let mut s = ddc_model::StairSpec::straight(l0, l1, (1.0, 2.0), 0.0);
        s.max_riser = 0.22;
        s.width = 0.9;
        m.add_stair(&s).unwrap();
        let mut nm = Norms::default();
        nm.locale.typology = "house".into();
        nm.refresh_chosen("us");
        nm.run(&m);
        let us = nm.chosen.clone().unwrap();
        assert!(nm.findings.iter().any(|f| f.rule_id.starts_with("us.")), "{:?}", nm.findings);
        for f in &nm.findings {
            let from = f.rule_id.split('.').next().unwrap_or("");
            assert!(us.chain.iter().any(|c| c == from), "{} is a rule of the us chain", f.rule_id);
            assert_eq!(nm.rules[f.rule_index].id(), f.rule_id);
        }
        let dir = std::env::temp_dir().join(format!("ddc_chk_us_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (x, j) = (dir.join("f.xlsx"), dir.join("f.json"));
        write_json(&nm.rules, &nm.findings, nm.chosen.as_ref(), &j).unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&j).unwrap()).unwrap();
        assert_eq!(v["pack"]["id"], "us");
        assert!(v["topics"].as_array().unwrap().iter().any(|t| t["rule"]["id"].as_str().unwrap().starts_with("us.")));
        let cat = Catalog::new(Lang::En);
        write_xlsx(&cat, &nm.rules, &nm.findings, nm.chosen.as_ref(), &x).unwrap();
        let mut wb: Xlsx<_> = open_workbook(&x).unwrap();
        assert_eq!(wb.sheet_names(), [cat.t("norms.sheet"), cat.t("norms.sheet_pack")]);
        let info = wb.worksheet_range(&cat.t("norms.sheet_pack")).unwrap();
        let field = |k: &str| {
            info.rows()
                .find(|r| r[0] == Data::String(k.into()))
                .map(|r| r[1].to_string())
                .unwrap_or_default()
        };
        assert_eq!(field("pack"), "us");
        assert_eq!(field("complete"), "false");
        assert_eq!(field("missing"), us.missing().len().to_string());
        // the findings sheet keeps one row per finding under its header
        let data = wb.worksheet_range(&cat.t("norms.sheet")).unwrap();
        assert_eq!(data.height(), nm.findings.len() + 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_is_repeatable() {
        let m = model_with_narrow_door();
        let (a, b) = (Norms::ran_with(&m, "ru"), Norms::ran_with(&m, "ru"));
        assert_eq!(a.findings, b.findings);
    }

    fn render(nm: &mut Norms, lang: Lang) -> (bool, Vec<Action>) {
        let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let cat = Catalog::new(lang);
        let ctx = ddc_ui::theme::headless_ctx();
        let (mut run, mut acts) = (false, vec![]);
        for _ in 0..2 {
            acts.clear();
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                th.apply(ctx);
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut hot = None;
                    run = view(ui, nm, &th, &cat, Units::Metric, &mut acts, &mut hot);
                });
            });
        }
        (run, acts)
    }

    #[test]
    fn every_state_renders_in_both_languages() {
        let m = model_with_narrow_door();
        for lang in [Lang::En, Lang::Ru] {
            let mut never = Norms::default();
            let mut failed = Norms {
                ran: true,
                failed: true,
                ..Norms::default()
            };
            let mut clean = Norms::ran_on(&Model::open(":memory:").unwrap());
            let mut found = Norms::ran_with(&m, "ru");
            let mut filtered = Norms::ran_with(&m, "ru");
            filtered.hidden = [true, true, false];
            for nm in [
                &mut never,
                &mut failed,
                &mut clean,
                &mut found,
                &mut filtered,
            ] {
                let (run, acts) = render(nm, lang);
                assert!(!run && acts.is_empty(), "{lang:?}: nothing clicked");
            }
        }
    }

    #[test]
    fn the_auto_pack_follows_the_ui_language() {
        assert_eq!(auto_pack("ru"), "ru");
        assert_eq!(auto_pack("en"), "us");
        assert_eq!(auto_pack("en-GB"), "uk");
        let nm = Norms::default();
        assert_eq!(
            nm.packs,
            [
                "ae", "ae-du", "ar", "au", "br", "ca", "cn", "cz", "de", "es", "es-ct", "eu", "fr", "generic", "id", "in", "it",
                "jp", "kr", "kz", "mx", "nl", "pl", "ru", "sa", "se", "tr", "ua", "uk", "us", "vn", "za"
            ]
        );
        let mut m = Model::open(":memory:").unwrap();
        let p = NormLocale { pack: "ru".into(), ..NormLocale::default() };
        assert!(m.set_norm_locale(&p).unwrap());
        assert_eq!(m.effective_norm_pack(shipped_packs(), "en", None).unwrap(), "ru");
    }

    #[test]
    fn severity_filter_hides_rows() {
        let m = model_with_narrow_door();
        let mut nm = Norms::ran_with(&m, "ru");
        let all = nm.shown().count();
        nm.hidden[sev_index(Severity::Error)] = true;
        assert!(nm.shown().count() < all);
        assert!(nm.shown().all(|f| f.severity != Severity::Error));
    }
}
