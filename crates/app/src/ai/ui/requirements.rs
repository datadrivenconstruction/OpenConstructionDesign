//! Information requirements on the Checks tab: load a workbook, check, see per category how
//! many elements comply, go to the failing ones, export the report and ask for a fix draft.
//! What fails without an automatic fix stays listed for manual work. Passing only says the
//! listed information is there and well-formed: it is not proof of code compliance.

use super::kit::{self, space, Button, Tone};
use super::{Action, Cx, Entry, PanelUi};
use crate::ai::reqbook::{self, Refusal};
use crate::ai::session::ReqOffer;
use crate::ai::AiError;
use crate::shell::EditorApp;
use ddc_model::checks::{requirements_report, CatReport, CheckScope, ReqReport};
use ddc_model::{ElKey, Model};
use ddc_ui::notify::Level;
use ddc_ui::theme::Role;
use ddc_ui::{Catalog, Theme};
use egui::{self, RichText, Ui};
use std::path::Path;

/// Rows of the manual-work list; the report workbook has them all.
const MANUAL_MAX: usize = 50;
/// Refused cells shown under a refused book.
const ERRORS_MAX: usize = 20;
/// Requirements named on an offer card.
const OFFER_LINES: usize = 6;
/// Elements named in the fix prompt; the assistant runs the check for the rest.
const PROMPT_ELS: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Do {
    Load,
    Template,
    /// Export the report on show.
    Export,
    /// The chat card `i`: save its requirements or export a fresh report.
    Offer(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Loaded {
    Saved { n: usize, sheet: String },
    Refused(Refusal),
}

#[derive(Default)]
pub struct Reqs {
    /// Requirements stored in the model; read again after a load or a check.
    stored: Option<usize>,
    loaded: Option<Loaded>,
    report: Option<Result<ReqReport, String>>,
    manual_open: bool,
}

impl Reqs {
    pub fn check(&mut self, m: &Model) {
        self.stored = None;
        self.report = Some(requirements_report(m, &CheckScope::All).map_err(|e| e.to_string()));
    }

    fn stored(&mut self, m: &Model) -> usize {
        *self
            .stored
            .get_or_insert_with(|| m.requirements().map_or(0, |v| v.len()))
    }

    /// Forget what was shown: the model's requirements changed.
    fn reset(&mut self) {
        self.stored = None;
        self.report = None;
        self.manual_open = false;
    }
}

fn wrapped(ui: &mut Ui, th: &Theme, text: &str, role: Role, color: &str) -> egui::Response {
    ui.add(
        egui::Label::new(RichText::new(text).font(th.font(role)).color(th.c(color))).wrap(),
    )
}

/// A row that shows its elements on hover and selects them on click.
fn hot_row(
    ui: &mut Ui,
    (th, cat): (&Theme, &Catalog),
    text: &str,
    badge: Option<(Tone, String)>,
    keys: Vec<ElKey>,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let bg = ui.painter().add(egui::Shape::Noop);
    let r = ui
        .horizontal(|ui| {
            if let Some((tone, b)) = &badge {
                kit::badge(ui, th, b, *tone);
            }
            let role = if badge.is_some() { Role::Label } else { Role::Caption };
            let color = if badge.is_some() { "text.primary" } else { "text.secondary" };
            let label = RichText::new(text).font(th.font(role)).color(th.c(color));
            let sense = if keys.is_empty() {
                egui::Sense::hover()
            } else {
                egui::Sense::click()
            };
            ui.add(egui::Label::new(label).wrap().sense(sense))
        })
        .inner;
    if keys.is_empty() {
        return;
    }
    let r = r
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(cat.t("req.goto"));
    if r.hovered() {
        ui.painter().set(
            bg,
            egui::Shape::rect_filled(r.rect.expand(2.0), th.radius("sm"), th.c("bg.hover")),
        );
        *hot = Some(keys.clone());
    }
    if r.clicked() {
        acts.push(Action::ShowDraft(keys));
    }
}

fn cat_block(
    ui: &mut Ui,
    th: &Theme,
    cat: &Catalog,
    c: &CatReport,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let ok = c.compliant == c.total;
    let badge = (
        if ok { Tone::Success } else { Tone::Warning },
        format!("{}/{}", c.compliant, c.total),
    );
    let head = cat.tf(
        "req.cat_head",
        &[("cat", &reqbook::cat_name(cat, &c.cat)), ("n", &c.lines.len().to_string())],
    );
    hot_row(ui, (th, cat), &head, Some(badge), c.failing(), acts, hot);
    for l in &c.lines {
        let text = reqbook::line_text(cat, l);
        ui.horizontal(|ui| {
            ui.add_space(space::MD);
            ui.vertical(|ui| hot_row(ui, (th, cat), &text, None, l.elements(), acts, hot));
        });
    }
}

/// The prompt behind "Create fix draft": the assistant reruns the check and drafts the fixes,
/// so nothing changes before the person applies the card.
pub fn fix_prompt(cat: &Catalog, rep: &ReqReport) -> String {
    let els: Vec<String> = rep
        .cats
        .iter()
        .flat_map(|c| c.lines.iter().flat_map(|l| l.hits.iter()))
        .filter(|h| h.fixable)
        .map(|h| reqbook::el_ref(&h.el))
        .take(PROMPT_ELS)
        .collect();
    let n = rep.fixable_split().0.to_string();
    cat.tf("req.fix_prompt", &[("n", &n), ("els", &els.join(", "))])
}

fn refused(ui: &mut Ui, th: &Theme, cat: &Catalog, r: &Refusal) {
    kit::error_note(ui, th, &reqbook::refusal_text(cat, r));
    let Refusal::Rows(rows) = r else { return };
    for e in rows.iter().take(ERRORS_MAX) {
        wrapped(ui, th, &reqbook::error_text(cat, e), Role::Caption, "text.secondary");
    }
    if rows.len() > ERRORS_MAX {
        let more = (rows.len() - ERRORS_MAX).to_string();
        kit::caption(ui, th, &cat.tf("req.more", &[("n", &more)]));
    }
}

/// The section, on top of the Checks tab.
pub fn section(
    ui: &mut Ui,
    rq: &mut Reqs,
    cx: &Cx<'_>,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let stored = rq.stored(&cx.ed.model);
    if view(ui, rq, stored, cx.th, cx.cat, acts, hot) {
        rq.check(&cx.ed.model);
    }
}

/// The section without the editor: true when the person asked for a check.
fn view(
    ui: &mut Ui,
    rq: &mut Reqs,
    stored: usize,
    th: &Theme,
    cat: &Catalog,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) -> bool {
    kit::section_title(ui, th, &cat.t("req.title"));
    let mut check = false;
    let report = rq.report.as_ref().and_then(|r| r.as_ref().ok());
    ui.horizontal(|ui| {
        if Button::secondary(&cat.t("req.load"))
            .small()
            .tooltip(&cat.t("req.load_tip"), "")
            .show(ui, th)
            .clicked()
        {
            acts.push(Action::Req(Do::Load));
        }
        if Button::quiet(&cat.t("req.template"))
            .small()
            .tooltip(&cat.t("req.template_tip"), "")
            .show(ui, th)
            .clicked()
        {
            acts.push(Action::Req(Do::Template));
        }
    });
    // a second row: a button never gets the tail of a line, where its label would be cut
    ui.horizontal(|ui| {
        check = Button::secondary(&cat.t("req.check"))
            .small()
            .enabled(stored > 0)
            .show(ui, th)
            .clicked();
        if Button::quiet(&cat.t("req.export"))
            .small()
            .enabled(report.is_some_and(|r| !r.cats.is_empty()))
            .show(ui, th)
            .clicked()
        {
            acts.push(Action::Req(Do::Export));
        }
    });
    match &rq.loaded {
        Some(Loaded::Saved { n, sheet }) => {
            let text = cat.tf("req.loaded", &[("n", &n.to_string()), ("sheet", sheet)]);
            wrapped(ui, th, &text, Role::Caption, "text.secondary");
        }
        Some(Loaded::Refused(r)) => refused(ui, th, cat, r),
        None if stored == 0 => {
            wrapped(ui, th, &cat.t("req.none"), Role::Caption, "text.secondary");
        }
        None => {
            let text = cat.tf("req.stored", &[("n", &stored.to_string())]);
            wrapped(ui, th, &text, Role::Caption, "text.secondary");
        }
    }
    match &rq.report {
        None => {}
        Some(Err(_)) => kit::error_note(ui, th, &cat.t("req.failed")),
        Some(Ok(rep)) if rep.cats.is_empty() => {
            wrapped(ui, th, &cat.t("req.empty"), Role::Caption, "text.secondary");
        }
        Some(Ok(rep)) => {
            ui.add_space(space::XS);
            let total = cat.tf(
                "req.total",
                &[("ok", &rep.compliant().to_string()), ("total", &rep.total().to_string())],
            );
            wrapped(ui, th, &total, Role::BodyStrong, "text.primary");
            for c in &rep.cats {
                cat_block(ui, th, cat, c, acts, hot);
                ui.add_space(space::XS);
            }
            let (fixable, manual) = rep.fixable_split();
            ui.horizontal(|ui| {
                let label = cat.tf("req.fix", &[("n", &fixable.to_string())]);
                if Button::primary(&label)
                    .small()
                    .enabled(fixable > 0)
                    .tooltip(&cat.t("req.fix_tip"), "")
                    .show(ui, th)
                    .clicked()
                {
                    acts.push(Action::AskFix(fix_prompt(cat, rep)));
                }
            });
            ui.horizontal(|ui| {
                if manual > 0 {
                    let label = cat.tf("req.manual", &[("n", &manual.to_string())]);
                    if Button::quiet(&label).small().show(ui, th).clicked() {
                        rq.manual_open = !rq.manual_open;
                    }
                }
            });
            if rq.manual_open {
                let list = reqbook::manual(rep);
                for (k, params) in list.iter().take(MANUAL_MAX) {
                    let text = format!("{} \u{2014} {params}", reqbook::el_ref(k));
                    hot_row(ui, (th, cat), &text, None, vec![k.clone()], acts, hot);
                }
                if list.len() > MANUAL_MAX {
                    let more = (list.len() - MANUAL_MAX).to_string();
                    kit::caption(ui, th, &cat.tf("req.more", &[("n", &more)]));
                }
            }
        }
    }
    wrapped(ui, th, &cat.t("req.basis"), Role::Caption, "text.secondary");
    check
}

/// The chat card of an offer.
pub fn card(
    ui: &mut Ui,
    cx: &Cx<'_>,
    offer: &ReqOffer,
    done: bool,
    i: usize,
    acts: &mut Vec<Action>,
) {
    let (th, cat) = (cx.th, cx.cat);
    kit::card(ui, th, |ui| {
        ui.set_width(ui.available_width());
        let (title, button, finished) = match offer {
            ReqOffer::Draft { rules, source } => {
                let n = rules.len().to_string();
                let title = cat.tf("req.offer.title", &[("n", &n), ("source", source)]);
                (title, "req.offer.save", "req.offer.saved")
            }
            ReqOffer::Export => (cat.t("req.offer.export"), "req.export", "req.offer.exported"),
        };
        wrapped(ui, th, &title, Role::BodyStrong, "text.primary");
        if let ReqOffer::Draft { rules, .. } = offer {
            for r in rules.iter().take(OFFER_LINES) {
                wrapped(ui, th, &offer_line(cat, r), Role::Caption, "text.secondary");
            }
            if rules.len() > OFFER_LINES {
                let more = (rules.len() - OFFER_LINES).to_string();
                kit::caption(ui, th, &cat.tf("req.more", &[("n", &more)]));
            }
            wrapped(ui, th, &cat.t("req.offer.replace"), Role::Caption, "text.secondary");
        }
        let label = cat.t(if done { finished } else { button });
        if Button::secondary(&label)
            .small()
            .enabled(!done)
            .show(ui, th)
            .clicked()
        {
            acts.push(Action::Req(Do::Offer(i)));
        }
    });
}

fn offer_line(cat: &Catalog, r: &ddc_model::checks::RuleDef) -> String {
    let what = match &r.check {
        ddc_model::checks::RuleCheck::Value { rule, .. } => reqbook::rule_text(cat, rule),
        _ => String::new(),
    };
    let c = r.scope.cats.first().map(|c| reqbook::cat_name(cat, c)).unwrap_or_default();
    let param = r.title.split_once(": ").map_or(r.title.as_str(), |x| x.1);
    format!("{c}: {param} \u{2014} {what}")
}

/// Asks for a file name and writes the report there; false when the person cancelled.
fn export(app: &mut EditorApp, rep: &ReqReport) -> Result<bool, String> {
    let name = format!("{}.xlsx", app.sh.cat.t("req.report_file"));
    let Some(path) = crate::file_pick::Dlg::new()
        .add_filter("XLSX", &["xlsx"])
        .set_file_name(name)
        .save_file()
    else {
        return Ok(false);
    };
    write_report(&app.sh.cat, rep, &path)?;
    Ok(true)
}

pub fn write_report(cat: &Catalog, rep: &ReqReport, path: &Path) -> Result<(), String> {
    ddc_io::requirements::write_report(path, &reqbook::report_sheets(cat, rep))
}

fn toast(app: &mut EditorApp, r: Result<bool, String>, ok_key: &str) {
    match r {
        Ok(true) => {
            let text = app.sh.cat.t(ok_key);
            app.sh.toasts.push(Level::Success, text, None);
        }
        Ok(false) => {}
        Err(e) => {
            let text = app.sh.cat.tf("req.write_failed", &[("why", &e)]);
            app.sh.toasts.push(Level::Warning, text, None);
        }
    }
}

/// Loads the book at `path` into the live model; the outcome stays on show.
pub fn load(rq: &mut Reqs, m: &Model, cat: &Catalog, path: &Path) {
    rq.loaded = Some(match reqbook::load_book(m, cat, path, None) {
        Ok((rules, sheet)) => match m.save_requirements(&rules) {
            Ok(()) => Loaded::Saved {
                n: rules.len(),
                sheet,
            },
            Err(e) => Loaded::Refused(Refusal::Read(e.to_string())),
        },
        Err(r) => Loaded::Refused(r),
    });
    rq.reset();
}

pub(super) fn act(st: &mut PanelUi, app: &mut EditorApp, d: Do) {
    match d {
        Do::Load => {
            let Some(path) = crate::file_pick::Dlg::new()
                .add_filter("XLSX", &["xlsx"])
                .pick_file()
            else {
                return;
            };
            load(&mut st.checks.reqs, &app.ed.model, &app.sh.cat, &path);
        }
        Do::Template => {
            let name = format!("{}.xlsx", app.sh.cat.t("req.template_file"));
            let picked = crate::file_pick::Dlg::new()
                .add_filter("XLSX", &["xlsx"])
                .set_file_name(name)
                .save_file();
            let r = match picked {
                None => Ok(false),
                Some(p) => {
                    let t = reqbook::template_text(&app.sh.cat);
                    ddc_io::requirements::write_template(&p, &t).map(|()| true)
                }
            };
            toast(app, r, "req.template_saved");
        }
        Do::Export => {
            let Some(Ok(rep)) = st.checks.reqs.report.clone() else {
                return;
            };
            let r = export(app, &rep);
            toast(app, r, "req.exported");
        }
        Do::Offer(i) => {
            let Some(Entry::Req { offer, done }) = st.log.get_mut(i) else {
                return;
            };
            match offer {
                ReqOffer::Draft { rules, .. } => match app.ed.model.save_requirements(rules) {
                    Ok(()) => {
                        *done = true;
                        st.checks.reqs.reset();
                        st.checks.reqs.loaded = None;
                    }
                    Err(e) => st.error = Some(AiError::Internal(e.to_string())),
                },
                ReqOffer::Export => {
                    let r = requirements_report(&app.ed.model, &CheckScope::All)
                        .map_err(|e| e.to_string())
                        .and_then(|rep| export(app, &rep));
                    *done = matches!(r, Ok(true));
                    toast(app, r, "req.exported");
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "requirements_tests.rs"]
pub(crate) mod tests;
