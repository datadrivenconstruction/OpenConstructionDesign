use super::*;
use ddc_io::requirements::{write_report, write_template};
use ddc_model::checks::{ReqHit, RuleScope};

fn tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ddc-ag5-{}-{tag}.xlsx", std::process::id()))
}

fn hit(id: i64, missing: bool, fixable: bool) -> ReqHit {
    ReqHit {
        el: ("opening".to_string(), id),
        value: (!missing).then(|| "EI15".to_string()),
        missing,
        fixable,
    }
}

fn line(id: &str, param: &str, rule: ValueRule, checked: usize, hits: Vec<ReqHit>) -> ReqLine {
    ReqLine {
        rule_id: id.to_string(),
        origin: Some("Requirements!3".to_string()),
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
        checked,
        hits,
    }
}

/// 202 windows, 4 without a rating (one of them also of a wrong width), 1 wrong width more.
fn windows() -> ReqReport {
    let unrated = (1..=4).map(|i| hit(i, true, false)).collect();
    let rating = line("req.1", "FireRating", ValueRule::NonEmpty, 202, unrated);
    let width = ValueRule::Range {
        min: Some(0.6),
        max: Some(2.4),
    };
    let width = line("req.2", "Width", width, 202, vec![hit(4, false, true), hit(9, false, true)]);
    ReqReport {
        cats: vec![CatReport {
            cat: "window".to_string(),
            total: 202,
            compliant: 197,
            lines: vec![rating, width],
        }],
    }
}

#[test]
fn a_book_in_either_language_reads_through_the_same_words() {
    let en = Catalog::new(Lang::En);
    let ru = Catalog::new(Lang::Ru);
    let v = vocab(&en);
    for (cat, key, to) in [
        (&ru, "req.cat.window", "window"),
        (&ru, "req.cat.mep_run", "mep_run"),
        (&en, "req.cat.curtain_wall", "curtain_wall"),
        (&ru, "req.kind.regex", "regex"),
        (&ru, "req.yn.no", "no"),
        (&en, "req.yn.yes", "yes"),
    ] {
        let w = cat.t(key).to_lowercase();
        assert!(v.words.iter().any(|(a, b)| *a == w && b == to), "{key} {w}");
    }
    let head = ru.t("req.col.parameter");
    assert!(v.headers.iter().any(|(f, c)| *f == ReqField::Parameter && *c == head));
    assert!(v.sheets.contains(&ru.t("req.sheet.req")) && v.skip.contains(&en.t("req.sheet.help")));
}

#[test]
fn the_template_round_trips_in_both_languages() {
    for lang in [Lang::En, Lang::Ru] {
        let cat = Catalog::new(lang);
        let t = template_text(&cat);
        assert_eq!(t.headers.len(), ReqField::ALL.len());
        assert!(t.example.iter().all(|r| r.len() == ReqField::ALL.len()));
        let p = tmp(&format!("tpl-{lang:?}"));
        write_template(&p, &t).unwrap();
        let mut m = Model::open(":memory:").unwrap();
        // the empty requirements sheet is read first and loads nothing
        let empty = load_book(&m, &cat, &p, None);
        assert!(matches!(empty, Err(Refusal::Empty)), "{lang:?}");
        // the example names a level this model lacks: refused at its cell, nothing loads
        let ex = Some(t.sheet_example.as_str());
        let Err(Refusal::Rows(errs)) = load_book(&m, &cat, &p, ex) else {
            panic!("{lang:?}: the missing level refuses the book");
        };
        let e = &errs[0];
        assert_eq!((errs.len(), e.row, e.col.as_str(), e.code), (1, 4, "B", "req.err.level"));
        assert!(!error_text(&cat, e).contains("req."), "{}", error_text(&cat, e));
        m.add_level(&t.example[2][1], 0.0).unwrap();
        let (rules, sheet) = load_book(&m, &cat, &p, ex).unwrap();
        assert_eq!((rules.len(), sheet), (6, t.sheet_example.clone()), "{lang:?}");
        let kinds: Vec<&str> = rules
            .iter()
            .map(|r| match &r.check {
                ddc_model::checks::RuleCheck::Value { rule, .. } => rule.kind(),
                _ => "",
            })
            .collect();
        assert_eq!(kinds, ["one_of", "range", "regex", "non_empty", "non_empty", "unit"]);
        m.save_requirements(&rules).unwrap();
        assert_eq!(m.requirements().unwrap().len(), 6);
        let _ = std::fs::remove_file(&p);
    }
}

#[test]
fn a_refused_cell_reads_with_its_sheet_row_and_column() {
    let e = RowError {
        sheet: "Requirements".to_string(),
        row: 6,
        col: "D".to_string(),
        field: ReqField::Value,
        code: "req.err.range_order",
        arg: "5..1".to_string(),
    };
    let en = error_text(&Catalog::new(Lang::En), &e);
    assert!(en.starts_with("Requirements, row 6, column D: ") && en.contains("5..1"), "{en}");
    let ru = error_text(&Catalog::new(Lang::Ru), &e);
    assert!(ru.contains('6') && ru.contains('D') && !ru.contains("req."), "{ru}");
    let no_col = RowError { col: String::new(), ..e };
    assert!(!error_text(&Catalog::new(Lang::En), &no_col).contains("column"));
}

#[test]
fn the_category_line_counts_compliant_elements_and_names_the_gaps() {
    let en = Catalog::new(Lang::En);
    let rep = windows();
    let s = cat_line(&en, &rep.cats[0]);
    assert!(s.starts_with("Windows: 197/202 compliant, 4 missing FireRating, 2 with Width"), "{s}");
    let ok = line("req.3", "Mark", ValueRule::NonEmpty, 202, vec![]);
    assert_eq!(line_text(&en, &ok), "Mark: all 202 pass");
    let ru = cat_line(&Catalog::new(Lang::Ru), &rep.cats[0]);
    assert!(ru.contains("197/202") && !ru.contains("req."), "{ru}");
    let gone = ReqLine { ran: false, ..ok };
    assert!(line_text(&en, &gone).contains("not checked"));
    let m = manual(&rep);
    assert_eq!(m.len(), 4, "the fixable width hits are not manual work");
    assert_eq!(rep.fixable_split(), (2, 4));
}

#[test]
fn the_report_workbook_has_a_summary_and_a_sheet_per_category() {
    for lang in [Lang::En, Lang::Ru] {
        let cat = Catalog::new(lang);
        let sheets = report_sheets(&cat, &windows());
        assert_eq!(sheets.len(), 2);
        assert_eq!(sheets[0].note.as_deref(), Some(cat.t("req.basis").as_str()));
        assert_eq!(sheets[0].rows.len(), 2, "one category and the total");
        let counts = [RCell::Num(202.0), RCell::Num(197.0), RCell::Num(5.0)];
        assert_eq!(sheets[0].rows[0][1..4], counts);
        assert_eq!(sheets[1].rows.len(), 6, "every failing element and requirement");
        assert!(sheets[1].rows.iter().all(|r| r.len() == sheets[1].headers.len()));
        let p = tmp(&format!("rep-{lang:?}"));
        write_report(&p, &sheets).unwrap();
        let names = ddc_io::xlsx::sheet_names(&p).unwrap();
        assert_eq!(names, [cat.t("req.x.summary"), cat.t("req.cat.window")]);
        let _ = std::fs::remove_file(&p);
    }
    assert!(Catalog::new(Lang::En).t("req.basis").contains("not proof of code compliance"));
}
