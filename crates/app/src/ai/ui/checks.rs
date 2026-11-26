// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The Checks tab: run the model's rules, go to a finding, ask the assistant to fix it.

use super::kit::{self, space, Button, Tone};
use super::{Action, Cx, PanelUi};
use crate::units::{self, DecimalMark, Units};
use ddc_model::checks::{run_all, CheckScope, Finding, Severity};
use ddc_model::{ElKey, Model};
use ddc_ui::theme::Role;
use ddc_ui::{Catalog, Lang};
use egui::{self, RichText, Ui};

const FINDINGS_MAX: usize = ddc_ai::limits::UI_FINDINGS.us();
/// Height share of the tab the requirements section takes at most.
const REQ_SHARE: f32 = 0.45;

#[derive(Default)]
pub struct Checks {
    ran: bool,
    findings: Vec<Finding>,
    /// Findings the rules produced, before the list was cut to `FINDINGS_MAX`.
    total: usize,
    failed: bool,
    pub reqs: super::requirements::Reqs,
    pub norms: super::norms::Norms,
}

impl Checks {
    /// Errors, warnings and notes among the listed findings of the last run; None before a run.
    pub fn counts(&self) -> Option<[usize; 3]> {
        if !self.ran || self.failed {
            return None;
        }
        let n = |s: Severity| self.findings.iter().filter(|f| f.severity == s).count();
        Some([n(Severity::Error), n(Severity::Warning), n(Severity::Info)])
    }

    pub fn run(&mut self, m: &Model) {
        self.ran = true;
        // Errors sort first, so cutting the sorted list keeps the most important ones.
        match run_all(m, None, &CheckScope::All, usize::MAX) {
            Ok(mut f) => {
                self.total = f.len();
                f.truncate(FINDINGS_MAX);
                self.findings = f;
                self.failed = false;
            }
            Err(_) => {
                self.findings.clear();
                self.total = 0;
                self.failed = true;
            }
        }
    }
}

/// One number in project units: lengths in mm or ft-in, areas and volumes in m or ft, in the
/// decimal mark of the catalog language.
pub(super) fn fmt_value(cat: &Catalog, v: f64, unit: &str, u: Units) -> String {
    let ru = cat.lang == Lang::Ru;
    let mark = DecimalMark::for_lang(cat.lang);
    let num = |x: f64, d: usize| match mark {
        DecimalMark::Comma => units::fmt_num_comma(x, d),
        DecimalMark::Point => units::fmt_num_en(x, d),
    };
    let (area_d, vol_d) = units::precision();
    match (unit, u) {
        ("m", Units::Metric) => format!(
            "{}\u{a0}{}",
            units::edit_length_den_mark(v, u, units::frac_denominator(), mark),
            crate::tr_str(ru, "unit_fmt.unit.mm")
        ),
        ("m", Units::Imperial) => units::format_length_den(v, u, units::frac_denominator()),
        ("m2", Units::Metric) => {
            format!(
                "{}\u{a0}{}",
                num(v, area_d),
                crate::tr_str(ru, "unit_fmt.unit.m2")
            )
        }
        ("m2", Units::Imperial) => {
            format!("{}\u{a0}ft\u{b2}", num(units::area_in_units(v, u), area_d))
        }
        ("m3", Units::Metric) => {
            format!(
                "{}\u{a0}{}",
                num(v, vol_d),
                crate::tr_str(ru, "unit_fmt.unit.m3")
            )
        }
        ("m3", Units::Imperial) => {
            format!("{}\u{a0}ft\u{b3}", num(units::volume_in_units(v, u), vol_d))
        }
        ("mm", _) => fmt_value(cat, v / 1000.0, "m", u),
        ("count", _) => format!("{}\u{a0}{}", num(v, 0), cat.t("ai.unit.count")),
        _ => num(v, 3),
    }
}

fn message(cat: &Catalog, f: &Finding, u: Units) -> String {
    let args: Vec<(&str, &str)> = f.args.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let text = cat.tf(f.msg_key, &args);
    let Some(ms) = f.measure else { return text };
    let (value, limit) = (
        fmt_value(cat, ms.value, ms.unit, u),
        fmt_value(cat, ms.limit, ms.unit, u),
    );
    let shown = cat.tf(
        "ai_checks.check.measure",
        &[("value", &value), ("limit", &limit)],
    );
    format!("{text} ({shown})")
}

/// Set when the list was cut: how many are shown of how many exist.
fn cap_note(cat: &Catalog, shown: usize, total: usize) -> Option<String> {
    (total > shown).then(|| {
        cat.tf(
            "ai_checks.check.cap",
            &[("shown", &shown.to_string()), ("total", &total.to_string())],
        )
    })
}

pub(super) const fn tone_of(s: Severity) -> (Tone, &'static str) {
    match s {
        Severity::Error => (Tone::Danger, "ai.check.error"),
        Severity::Warning => (Tone::Warning, "ai.check.warning"),
        Severity::Info => (Tone::Neutral, "ai.check.info"),
    }
}

/// `wall:12, door:55`, the form the tools take back.
pub(super) fn element_list(keys: &[ElKey]) -> String {
    keys.iter()
        .map(|(t, id)| format!("{t}:{id}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn tab(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    // the requirements report scrolls on its own, so the findings below stay reachable
    egui::ScrollArea::vertical()
        .id_salt("ai.checks.req")
        .max_height(ui.available_height() * REQ_SHARE)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            super::requirements::section(ui, &mut st.checks.reqs, cx, acts, hot);
            ui.separator();
            super::norms::section(ui, &mut st.checks.norms, cx, acts, hot);
        });
    ui.separator();
    if view(
        ui,
        &st.checks,
        cx.th,
        cx.cat,
        units::active_units(),
        acts,
        hot,
    ) {
        st.checks.run(&cx.ed.model);
    }
}

/// The tab without the editor: true when the user asked for a run.
fn view(
    ui: &mut Ui,
    ch: &Checks,
    th: &ddc_ui::Theme,
    cat: &Catalog,
    u: Units,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) -> bool {
    if !ch.ran {
        return kit::empty_state(
            ui,
            th,
            &cat.t("ai_checks.check.never_title"),
            &cat.t("ai_checks.check.never_hint"),
            Some(&cat.t("ai.check.run")),
        );
    }
    let run = Button::quiet(&cat.t("ai_checks.check.run_again"))
        .tooltip(&cat.t("cmd.data.check.desc"), "")
        .show(ui, th)
        .clicked();
    ui.add_space(space::XS);
    if ch.failed {
        kit::error_note(ui, th, &cat.t("ai.check.failed"));
        return run;
    }
    if ch.findings.is_empty() {
        kit::empty_state(
            ui,
            th,
            &cat.t("ai.check.clean"),
            &cat.t("ai.check.clean_hint"),
            None,
        );
        return run;
    }
    match cap_note(cat, ch.findings.len(), ch.total) {
        Some(note) => kit::warn_note(ui, th, &note),
        None => {
            kit::caption(
                ui,
                th,
                &cat.tf("ai.check.count", &[("n", &ch.findings.len().to_string())]),
            );
        }
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for sev in [Severity::Error, Severity::Warning, Severity::Info] {
                let group: Vec<&Finding> =
                    ch.findings.iter().filter(|f| f.severity == sev).collect();
                if group.is_empty() {
                    continue;
                }
                let title = format!("{} ({})", cat.t(section_key(sev)), group.len());
                kit::section_title(ui, th, &title);
                for f in group {
                    row(ui, th, cat, u, f, acts, hot);
                    ui.add_space(space::XS);
                }
            }
        });
    run
}

const fn section_key(s: Severity) -> &'static str {
    match s {
        Severity::Error => "ai_checks.check.sec.error",
        Severity::Warning => "ai_checks.check.sec.warning",
        Severity::Info => "ai_checks.check.sec.info",
    }
}

/// One finding makes one issue: the key is the rule, the message key and the elements.
fn issue_draft(f: &Finding, text: &str) -> crate::issues_ui::Draft {
    let ids: Vec<String> = f.elements.iter().map(|(t, i)| format!("{t}:{i}")).collect();
    crate::issues_ui::Draft {
        title: text.to_string(),
        description: element_list(&f.elements),
        elements: f
            .elements
            .iter()
            .map(|(t, i)| ddc_model::TopicElem::local(t, *i))
            .collect(),
        source: format!("check:{}", f.rule),
        source_key: format!("{}|{}", f.key(), ids.join(",")),
        priority: Some(match f.severity {
            Severity::Error => "high",
            Severity::Warning => "normal",
            Severity::Info => "low",
        }),
    }
}

fn row(
    ui: &mut Ui,
    th: &ddc_ui::Theme,
    cat: &Catalog,
    u: Units,
    f: &Finding,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (tone, key) = tone_of(f.severity);
    let text = message(cat, f, u);
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
            ui.horizontal(|ui| {
                kit::badge(ui, th, &cat.t(key), tone);
                ui.add(
                    egui::Label::new(
                        RichText::new(&text)
                            .font(th.font(Role::Label))
                            .color(th.c("text.primary")),
                    )
                    .wrap()
                    .sense(egui::Sense::click()),
                )
            })
            .inner
        },
        |ui, acts| {
            if Button::quiet(&cat.t("ai_modes.check.explain"))
                .small()
                .show(ui, th)
                .clicked()
            {
                let prompt = cat.tf(
                    "ai_modes.check.explain_prompt",
                    &[("what", &text), ("els", &element_list(&f.elements))],
                );
                acts.push(Action::AskExplain(prompt));
            }
            if Button::quiet(&cat.t("ai.check.ask_fix"))
                .small()
                .show(ui, th)
                .clicked()
            {
                let prompt = cat.tf(
                    "ai.check.fix_prompt",
                    &[("what", &text), ("els", &element_list(&f.elements))],
                );
                acts.push(Action::AskFix(prompt));
            }
            if Button::quiet(&cat.t("issues.issue.from_finding"))
                .small()
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::NewIssue(Box::new(issue_draft(f, &text))));
            }
        },
    );
}

/// What a finding points at: the elements to highlight and select, and a point to centre on.
pub(super) struct Target<'a> {
    pub elements: &'a [ElKey],
    pub at: Option<[f64; 3]>,
}

/// A finding in a card: `head` draws the clickable part (its response is the hit area, hover
/// highlights the elements, a click selects the first and centres the view), `tail` adds the
/// buttons below. Shared by the model checks and the norm checks.
pub(super) fn finding_card(
    ui: &mut Ui,
    th: &ddc_ui::Theme,
    cat: &Catalog,
    target: Target<'_>,
    hot: &mut Option<Vec<ElKey>>,
    acts: &mut Vec<Action>,
    head: impl FnOnce(&mut Ui) -> egui::Response,
    tail: impl FnOnce(&mut Ui, &mut Vec<Action>),
) {
    kit::card(ui, th, |ui| {
        ui.set_width(ui.available_width());
        // Reserved first so the hover fill lands under the text.
        let bg = ui.painter().add(egui::Shape::Noop);
        let r = head(ui)
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(cat.t("ai_checks.check.goto"));
        if r.hovered() {
            *hot = Some(target.elements.to_vec());
            ui.painter().set(
                bg,
                egui::Shape::rect_filled(r.rect.expand(2.0), th.radius("sm"), th.c("bg.hover")),
            );
        }
        if r.clicked() {
            if let Some(el) = target.elements.first() {
                acts.push(Action::SelectEl(el.clone()));
            }
            if let Some(at) = target.at {
                acts.push(Action::Center(at));
            }
        }
        tail(ui, acts);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_list_reads_as_references() {
        assert_eq!(
            element_list(&[("wall".to_string(), 12), ("door".to_string(), 55)]),
            "wall:12, door:55"
        );
    }

    fn finding(sev: Severity, measure: Option<ddc_model::checks::Measure>) -> Finding {
        Finding {
            rule: "t",
            severity: sev,
            elements: vec![("wall".to_string(), 1)],
            at: None,
            measure,
            msg_key: "ai.check.opening_overlap",
            args: vec![],
            fix: ddc_model::checks::Fix::None,
        }
    }

    fn measured(v: f64, l: f64, unit: &'static str) -> Finding {
        finding(
            Severity::Error,
            Some(ddc_model::checks::Measure::new(v, l, unit)),
        )
    }

    #[test]
    fn length_shows_in_mm_and_in_ft_in() {
        let en = Catalog::new(Lang::En);
        let ru = Catalog::new(Lang::Ru);
        let f = measured(0.25, 0.5, "m");
        let s = message(&en, &f, Units::Metric);
        assert!(s.contains("measured 250\u{a0}mm, limit 500\u{a0}mm"), "{s}");
        let s = message(&ru, &measured(0.2505, 0.5, "m"), Units::Metric);
        assert!(s.contains("250,5\u{a0}\u{43c}\u{43c}"), "{s}");
        assert!(!s.contains("0.2505") && !s.contains("0,251"), "{s}");
        let s = message(
            &en,
            &measured(2.5 * units::FOOT, 3.0 * units::FOOT, "m"),
            Units::Imperial,
        );
        assert!(s.contains("measured 2'-6\", limit 3'-0\""), "{s}");
        assert!(!s.contains("mm"), "{s}");
    }

    #[test]
    fn area_volume_and_count_follow_units() {
        let en = Catalog::new(Lang::En);
        let ru = Catalog::new(Lang::Ru);
        let (ad, vd) = units::precision();
        let s = message(&en, &measured(12.5, 10.0, "m2"), Units::Metric);
        assert!(
            s.contains(&format!("{}\u{a0}m\u{b2}", units::fmt_num_en(12.5, ad))),
            "{s}"
        );
        let s = message(&ru, &measured(12.5, 10.0, "m2"), Units::Metric);
        assert!(
            s.contains(&format!(
                "{}\u{a0}\u{43c}\u{b2}",
                units::fmt_num_comma(12.5, ad)
            )),
            "{s}"
        );
        let s = message(&en, &measured(1.0, 2.0, "m3"), Units::Imperial);
        assert!(s.contains("ft\u{b3}") && !s.contains("m\u{b3}"), "{s}");
        assert!(
            s.contains(&units::fmt_num_en(1.0 / 0.3048_f64.powi(3), vd)),
            "{s}"
        );
        let s = message(&en, &measured(3.0, 0.0, "count"), Units::Metric);
        assert!(s.contains("measured 3\u{a0}pcs, limit 0\u{a0}pcs"), "{s}");
        assert_eq!(
            message(&en, &finding(Severity::Info, None), Units::Metric).contains('('),
            false
        );
    }

    #[test]
    fn cap_note_names_shown_and_total() {
        let en = Catalog::new(Lang::En);
        assert_eq!(cap_note(&en, 3, 3), None);
        let s = cap_note(&en, FINDINGS_MAX, 1234).unwrap();
        assert!(s.contains("500") && s.contains("1234"), "{s}");
        let ru = Catalog::new(Lang::Ru);
        let s = cap_note(&ru, FINDINGS_MAX, 1234).unwrap();
        assert!(
            s.contains("500") && s.contains("1234") && !s.contains("ai_checks."),
            "{s}"
        );
    }

    #[test]
    fn keys_exist_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for k in [
                "ai_checks.check.measure",
                "ai_checks.check.cap",
                "ai_checks.check.run_again",
                "ai_checks.check.goto",
                "ai_checks.check.never_title",
                "ai_checks.check.never_hint",
                "ai_checks.check.sec.error",
                "ai_checks.check.sec.warning",
                "ai_checks.check.sec.info",
            ] {
                assert_ne!(cat.t_in(lang, k), k, "{k} in {lang:?}");
            }
        }
    }

    /// Renders the tab twice (layout settles on the second pass); returns whether Run was asked.
    fn render(ch: &Checks, lang: Lang) -> (bool, Vec<Action>) {
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
                    run = view(ui, ch, &th, &cat, Units::Metric, &mut acts, &mut hot);
                });
            });
        }
        (run, acts)
    }

    #[test]
    fn every_state_renders_in_both_languages() {
        let mut capped = Checks::default();
        capped.ran = true;
        capped.findings = (0..FINDINGS_MAX).map(|_| measured(0.1, 0.2, "m")).collect();
        capped.total = FINDINGS_MAX + 7;
        let mut mixed = Checks::default();
        mixed.ran = true;
        mixed.total = 3;
        mixed.findings = vec![
            finding(Severity::Info, None),
            measured(1.0, 2.0, "m2"),
            finding(Severity::Warning, None),
        ];
        let clean = Checks {
            ran: true,
            ..Checks::default()
        };
        let failed = Checks {
            ran: true,
            failed: true,
            ..Checks::default()
        };
        for lang in [Lang::En, Lang::Ru] {
            for ch in [&Checks::default(), &clean, &failed, &mixed, &capped] {
                let (run, acts) = render(ch, lang);
                assert!(!run && acts.is_empty(), "{lang:?}: nothing clicked");
            }
        }
    }

    #[test]
    fn run_fills_total_and_cap() {
        let m = Model::open(":memory:").unwrap();
        let mut c = Checks::default();
        c.run(&m);
        assert!(c.findings.len() <= FINDINGS_MAX && c.total >= c.findings.len());
    }

    #[test]
    fn checks_on_an_empty_model_run_clean() {
        let m = Model::open(":memory:").unwrap();
        let mut c = Checks::default();
        c.run(&m);
        assert!(c.ran && !c.failed);
    }
}
