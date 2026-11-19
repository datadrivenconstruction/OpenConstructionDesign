use super::*;
use ddc_io::requirements::RowError;
use ddc_model::checks::{ReqField, ReqHit, ReqLine, RuleScope, ValueRule};
use ddc_ui::Lang;

fn report() -> ReqReport {
    let hit = |id: i64, missing: bool, fixable: bool| ReqHit {
        el: ("opening".to_string(), id),
        value: (!missing).then(|| "3".to_string()),
        missing,
        fixable,
    };
    let line = |param: &str, rule: ValueRule, hits: Vec<ReqHit>| ReqLine {
        rule_id: "req.1".to_string(),
        origin: Some("Requirements!2".to_string()),
        param: param.to_string(),
        required: true,
        rule,
        scope: RuleScope {
            cats: vec!["window".to_string()],
            levels: None,
            type_name: None,
            when: None,
        },
        phase: None,
        ran: true,
        checked: 12,
        hits,
    };
    let range = ValueRule::Range {
        min: Some(0.6),
        max: None,
    };
    ReqReport {
        cats: vec![
            CatReport {
                cat: "window".to_string(),
                total: 12,
                compliant: 9,
                lines: vec![
                    line(
                        "FireRating",
                        ValueRule::NonEmpty,
                        vec![hit(1, true, false), hit(2, true, false)],
                    ),
                    line("Width", range, vec![hit(3, false, true)]),
                ],
            },
            CatReport {
                cat: "curtain_wall".to_string(),
                total: 3,
                compliant: 3,
                lines: vec![line("Mark", ValueRule::NonEmpty, vec![])],
            },
        ],
    }
}

fn states() -> Vec<(Reqs, usize)> {
    let refused = Refusal::Rows(
        (0..25)
            .map(|i| RowError {
                sheet: "Requirements".to_string(),
                row: i + 2,
                col: "D".to_string(),
                field: ReqField::Value,
                code: "req.err.range",
                arg: "abc".to_string(),
            })
            .collect(),
    );
    let with = |report: Option<Result<ReqReport, String>>, manual_open: bool| Reqs {
        report,
        manual_open,
        ..Reqs::default()
    };
    vec![
        (Reqs::default(), 0),
        (Reqs::default(), 4),
        (
            Reqs {
                loaded: Some(Loaded::Refused(refused)),
                ..Reqs::default()
            },
            0,
        ),
        (
            Reqs {
                loaded: Some(Loaded::Refused(Refusal::NoSheet)),
                ..Reqs::default()
            },
            0,
        ),
        (
            Reqs {
                loaded: Some(Loaded::Saved {
                    n: 6,
                    sheet: "Requirements".to_string(),
                }),
                ..Reqs::default()
            },
            6,
        ),
        (with(Some(Err("broken".to_string())), false), 3),
        (with(Some(Ok(ReqReport::default())), false), 3),
        (with(Some(Ok(report())), false), 3),
        (with(Some(Ok(report())), true), 3),
    ]
}

/// Renders the section twice (layout settles on the second pass): what it asked for.
fn render(rq: &mut Reqs, stored: usize, lang: Lang) -> (bool, Vec<Action>) {
    let mut th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
    let cat = Catalog::new(lang);
    let ctx = ddc_ui::theme::headless_ctx();
    let (mut check, mut acts) = (false, vec![]);
    for _ in 0..2 {
        acts.clear();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            th.apply(ctx);
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut hot = None;
                check = view(ui, rq, stored, &th, &cat, &mut acts, &mut hot);
            });
        });
    }
    (check, acts)
}

#[test]
fn every_state_renders_in_both_languages() {
    for lang in [Lang::En, Lang::Ru] {
        for (mut rq, stored) in states() {
            let (check, acts) = render(&mut rq, stored, lang);
            assert!(!check && acts.is_empty(), "{lang:?}: nothing clicked");
        }
    }
}

#[test]
fn the_fix_prompt_names_the_fixable_elements_only() {
    let en = Catalog::new(Lang::En);
    let p = fix_prompt(&en, &report());
    assert!(p.starts_with("1 values") && p.contains("opening:3"), "{p}");
    assert!(!p.contains("opening:1"), "{p}");
    assert!(p.contains("check_requirements") && p.contains("apply_fix"), "{p}");
}

#[test]
fn a_check_on_a_model_without_requirements_reports_nothing() {
    let m = Model::open(":memory:").unwrap();
    let mut rq = Reqs::default();
    assert_eq!(rq.stored(&m), 0);
    rq.check(&m);
    assert!(matches!(&rq.report, Some(Ok(r)) if r.cats.is_empty()));
}

#[test]
fn a_loaded_book_replaces_the_report_on_show() {
    let m = Model::open(":memory:").unwrap();
    let cat = Catalog::new(Lang::En);
    let p = std::env::temp_dir().join(format!("ddc-ag5-{}-load.xlsx", std::process::id()));
    let mut t = crate::ai::reqbook::template_text(&cat);
    // the example without its level row, as the requirements sheet
    t.example.remove(2);
    t.sheet_req = "Requirements".to_string();
    t.sheet_example = "Ex".to_string();
    let rows = std::mem::take(&mut t.example);
    ddc_io::requirements::write_template(&p, &t).unwrap();
    let mut rq = Reqs {
        report: Some(Ok(report())),
        ..Reqs::default()
    };
    load(&mut rq, &m, &cat, &p);
    assert_eq!(rq.loaded, Some(Loaded::Refused(Refusal::Empty)));
    assert!(rq.report.is_none() && rq.stored.is_none());
    t.example = rows;
    t.sheet_req = "Old".to_string();
    t.sheet_example = "Requirements".to_string();
    ddc_io::requirements::write_template(&p, &t).unwrap();
    load(&mut rq, &m, &cat, &p);
    let saved = Loaded::Saved {
        n: 5,
        sheet: "Requirements".to_string(),
    };
    assert_eq!(rq.loaded, Some(saved));
    assert_eq!(rq.stored(&m), 5);
    rq.check(&m);
    let rep = rq.report.clone().unwrap().unwrap();
    assert_eq!(rep.requirements(), 5);
    let _ = std::fs::remove_file(&p);
}

fn offers() -> Vec<ReqOffer> {
    let m = Model::open(":memory:").unwrap();
    let req = ddc_model::checks::Requirement {
        cat: "window",
        level: None,
        type_name: None,
        phase: None,
        param: "FireRating".to_string(),
        required: true,
        rule: ValueRule::OneOf(vec!["EI30".to_string(), "EI60".to_string()]),
    };
    let rules = (1..=8)
        .map(|i| m.requirement_rule(i, "rows!1", &req).unwrap())
        .collect();
    vec![
        ReqOffer::Draft {
            rules,
            source: "requirements.xlsx".to_string(),
        },
        ReqOffer::Export,
    ]
}

fn card_frame(lang: Lang, done: bool) -> impl FnMut(&egui::Context) {
    let ed = crate::editor::Editor::new_in_memory().unwrap();
    let cat = Catalog::new(lang);
    let th = Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    let offers = offers();
    let mut hot = None;
    move |ctx: &egui::Context| {
        let cx = Cx {
            th: &th,
            cat: &cat,
            ed: &ed,
            view3d: false,
            palette_open: false,
        };
        let mut acts: Vec<Action> = vec![];
        let size = egui::vec2(super::super::PANEL_W.0, 640.0);
        crate::ai::ui::tests_states::host(ctx, "ai.req", size, |ui| {
            for (i, o) in offers.iter().enumerate() {
                card(ui, &cx, o, done, i, &mut acts);
            }
            let mut rq = Reqs {
                report: Some(Ok(report())),
                manual_open: true,
                ..Reqs::default()
            };
            view(ui, &mut rq, 3, &th, &cat, &mut acts, &mut hot);
        });
    }
}

/// The offer cards and a report with its manual list, at the panel's narrowest width.
pub(crate) fn layout_probe(lang: Lang) -> crate::layout_contract::Probe {
    crate::layout_contract::probe("ai.req", card_frame(lang, false))
}

#[test]
fn offer_cards_render_in_both_languages_done_or_not() {
    for lang in [Lang::En, Lang::Ru] {
        for done in [false, true] {
            let ctx = ddc_ui::theme::headless_ctx();
            let mut f = card_frame(lang, done);
            for _ in 0..2 {
                let _ = ctx.run(egui::RawInput::default(), |ctx| f(ctx));
            }
        }
    }
}
