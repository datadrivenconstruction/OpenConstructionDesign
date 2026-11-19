//! Information requirements: parsing per column, every value rule, the pack, report counts.

use super::*;
use crate::{Edit, LayerSpec, OpeningSpec, PVal, ParamSpec, ParamType, WallSpec};

struct B {
    m: Model,
    level: i64,
    wall: i64,
}

fn base() -> B {
    let mut m = Model::open(":memory:").unwrap();
    let level = m.add_level("L1", 0.0).unwrap();
    let layer = [LayerSpec {
        material: "Brick".into(),
        thickness: 0.2,
    }];
    let wt = m.add_wall_type("W200", &layer).unwrap();
    let spec = WallSpec {
        a: (0.0, 0.0),
        b: (40.0, 0.0),
        level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: wt,
        offset0: None,
    };
    let wall = m.add_wall(&spec).unwrap();
    B { m, level, wall }
}

impl B {
    fn opening(&mut self, kind: &str, along: f64, width: f64) -> i64 {
        let spec = OpeningSpec {
            along,
            width,
            sill: if kind == "window" { 0.9 } else { 0.0 },
            height: if kind == "window" { 1.2 } else { 2.1 },
            kind: kind.into(),
        };
        self.m.add_opening(self.wall, &spec).unwrap()
    }
    fn param(&mut self, name: &str, ptype: ParamType, unit: &str) {
        let s = ParamSpec {
            name: name.into(),
            ptype,
            per_type: false,
            unit: unit.into(),
            group: String::new(),
            guid: None,
            categories: vec!["window".into(), "door".into()],
        };
        self.m.add_param(&s).unwrap();
    }
    fn set(&mut self, name: &str, id: i64, v: PVal) {
        let e = Edit::ParamValue {
            param: name.into(),
            kind: "opening".into(),
            id,
            value: Some(v),
        };
        self.m.apply_edits(&[e]).unwrap();
    }
    fn prop(&self, id: i64, k: &str, v: &str) {
        self.m
            .sql()
            .execute(
                "INSERT INTO prop(kind, ref, key, value, unit) VALUES ('opening', ?1, ?2, ?3, '')",
                rusqlite::params![id, k, v],
            )
            .unwrap();
    }
    /// Stores the rows as the requirements pack; panics on a row error.
    fn load(&self, rows: &[ReqText]) {
        let rules: Vec<RuleDef> = rows
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let r = parse_requirement(t).unwrap();
                self.m.requirement_rule(i + 2, &format!("Requirements!{}", i + 2), &r).unwrap()
            })
            .collect();
        self.m.save_requirements(&rules).unwrap();
    }
    fn report(&self) -> ReqReport {
        requirements_report(&self.m, &CheckScope::All).unwrap()
    }
}

fn row(cat: &str, param: &str, kind: &str, value: &str, unit: &str) -> ReqText {
    ReqText {
        cat: cat.into(),
        param: param.into(),
        required: "yes".into(),
        kind: kind.into(),
        value: value.into(),
        unit: unit.into(),
        ..ReqText::default()
    }
}

fn field_of(t: ReqText) -> (ReqField, &'static str) {
    let e = parse_requirement(&t).unwrap_err();
    (e.field, e.code)
}

#[test]
fn each_bad_cell_names_its_column() {
    let ok = row("Windows", "FireRating", "one_of", "EI30; EI60", "");
    assert!(parse_requirement(&ok).is_ok());
    let cases = [
        (ReqText { cat: "Spaceships".into(), ..ok.clone() }, ReqField::Category),
        (ReqText { param: " ".into(), ..ok.clone() }, ReqField::Parameter),
        (ReqText { required: "maybe".into(), ..ok.clone() }, ReqField::Required),
        (ReqText { kind: "fuzzy".into(), ..ok.clone() }, ReqField::Rule),
        (ReqText { value: "".into(), ..ok.clone() }, ReqField::Value),
        (row("window", "w", "range", "a..b", ""), ReqField::Value),
        (row("window", "w", "range", "900..2400", "furlong"), ReqField::Unit),
        (row("window", "w", "regex", "EI(", ""), ReqField::Value),
        (row("window", "w", "unit", "", ""), ReqField::Unit),
        (ReqText { type_name: "T".into(), ..row("room", "Name", "", "", "") }, ReqField::Type),
    ];
    for (t, want) in cases {
        let (got, code) = field_of(t.clone());
        assert_eq!(got, want, "{t:?}");
        assert!(code.starts_with("req.err."), "{code}");
    }
    assert_eq!(field_of(row("window", "w", "range", "5..1", "")).1, "req.err.range_order");
}

#[test]
fn rows_parse_to_every_rule_kind_in_base_units() {
    let p = |k: &str, v: &str, u: &str| parse_requirement(&row("doors", "P", k, v, u)).unwrap();
    assert_eq!(p("", "", "").rule, ValueRule::NonEmpty);
    assert_eq!(p("non-empty", "", "").cat, "door");
    assert_eq!(
        p("One of", "EI30, EI60, EI30", "").rule,
        ValueRule::OneOf(vec!["EI30".into(), "EI60".into()])
    );
    let r = p("range", "900..2400", "mm").rule;
    let ValueRule::Range { min, max } = r.clone() else {
        panic!("{r:?}")
    };
    assert!((min.unwrap() - 0.9).abs() < 1e-12 && (max.unwrap() - 2.4).abs() < 1e-12);
    assert_eq!(p("range", ">= 0,5", "").rule, ValueRule::Range { min: Some(0.5), max: None });
    assert_eq!(p("range", "..3", "").rule, ValueRule::Range { min: None, max: Some(3.0) });
    assert_eq!(p("pattern", "EI\\d+", "").rule, ValueRule::Regex("EI\\d+".into()));
    assert_eq!(p("unit", "", "mm").rule, ValueRule::Unit("mm".into()));
    let opt = parse_requirement(&ReqText { required: "no".into(), ..row("wall", "P", "", "", "") });
    assert!(!opt.unwrap().required);
}

#[test]
fn level_and_phase_must_exist_in_the_model() {
    let b = base();
    let mut t = row("window", "FireRating", "", "", "");
    t.level = "l1".into();
    let r = parse_requirement(&t).unwrap();
    let rule = b.m.requirement_rule(4, "Requirements!4", &r).unwrap();
    assert_eq!(rule.scope.levels, Some(vec![b.level]));
    assert_eq!((rule.id.as_str(), rule.severity), ("req.4", Severity::Error));
    t.level = "L9".into();
    let e = b.m.requirement_rule(4, "", &parse_requirement(&t).unwrap()).unwrap_err();
    assert_eq!((e.field, e.code, e.arg.as_str()), (ReqField::Level, "req.err.level", "L9"));
    t.level.clear();
    t.phase = "No such phase".into();
    let e = b.m.requirement_rule(4, "", &parse_requirement(&t).unwrap()).unwrap_err();
    assert_eq!(e.field, ReqField::Phase);
}

#[test]
fn the_pack_round_trips_and_feeds_the_company_rules() {
    let mut b = base();
    let w = b.opening("window", 1.0, 1.2);
    b.load(&[
        row("window", "FireRating", "regex", "EI\\d+", ""),
        row("window", "width", "range", "1000..1500", "mm"),
    ]);
    let stored = b.m.requirements().unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].source_text.as_deref(), Some("Requirements!2"));
    assert!(b.m.project_rules().unwrap().iter().any(|r| r.id == "req.2"));
    let only = ["company_rules"];
    let found = super::run_all(&b.m, Some(&only[..]), &CheckScope::All, 50).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].msg_key, "ai.check.req.missing");
    assert_eq!(found[0].elements, vec![("opening".to_string(), w)]);
    // only value rules, unique ids; a stored bad pattern is refused whole on read
    let mut dup = stored.clone();
    dup[1].id = dup[0].id.clone();
    assert!(b.m.save_requirements(&dup).is_err());
    let raw = b.m.ai_policy(REQ_PACK).unwrap().unwrap().replace("EI\\\\d+", "EI(");
    assert!(load_pack(&raw).is_err());
    b.m.save_requirements(&[]).unwrap();
    assert!(b.report().cats.is_empty());
}

#[test]
fn every_value_rule_kind_is_checked() {
    let mut b = base();
    b.param("FireRating", ParamType::Text, "");
    b.param("ClearWidth", ParamType::Length, "");
    b.param("Mass", ParamType::Number, "kg");
    let ids: Vec<i64> = (0..4).map(|i| b.opening("window", 1.0 + 3.0 * i as f64, 1.2)).collect();
    b.set("FireRating", ids[0], PVal::Text("EI30".into()));
    b.set("FireRating", ids[1], PVal::Text("EI 30".into()));
    b.set("ClearWidth", ids[0], PVal::Num(1.1));
    b.set("Mass", ids[0], PVal::Num(40.0));
    b.prop(ids[0], "Code", "W-001");
    b.prop(ids[1], "Code", "w1");
    b.prop(ids[0], "SillNote", "900 mm");
    b.prop(ids[1], "SillNote", "0.9 m2");
    let mut optional = row("window", "Code", "regex", "W-\\d{3}", "");
    optional.required = "no".into();
    b.load(&[
        row("window", "FireRating", "", "", ""),
        row("window", "FireRating", "one_of", "EI30; EI60", ""),
        optional,
        row("window", "width", "range", "1000..1300", "mm"),
        row("window", "ClearWidth", "unit", "", "mm"),
        row("window", "Mass", "unit", "", "kg"),
        row("window", "SillNote", "unit", "", "mm"),
        row("window", "width", "unit", "", "m2"),
    ]);
    let rep = b.report();
    let l = &rep.cats[0].lines;
    let counts: Vec<(usize, usize)> = l.iter().map(|x| (x.missing(), x.invalid())).collect();
    assert_eq!(counts[0], (2, 0), "non-empty: two blank");
    assert_eq!(counts[1], (2, 1), "one of: EI 30 is not on the list");
    assert_eq!(counts[2], (0, 1), "optional pattern: blanks pass, w1 fails");
    assert_eq!(counts[3], (0, 0), "range: 1.2 m inside 1000..1300 mm");
    assert_eq!(counts[4], (3, 0), "unit: a length parameter is in mm");
    assert_eq!(counts[5], (3, 0), "unit: the number parameter is labelled kg");
    assert_eq!(counts[6], (2, 1), "unit: text must carry mm");
    assert_eq!(counts[7], (0, 4), "unit: a width is no area");
    assert!(l[2].hits.iter().all(|h| h.el == ("opening".to_string(), ids[1])));
}

#[test]
fn report_counts_compliant_elements_per_category() {
    let mut b = base();
    b.param("FireRating", ParamType::Text, "");
    let wins: Vec<i64> = (0..5).map(|i| b.opening("window", 1.0 + 3.0 * i as f64, 1.2)).collect();
    let doors: Vec<i64> = (0..2).map(|i| b.opening("door", 20.0 + 3.0 * i as f64, 0.9)).collect();
    for w in &wins[..4] {
        b.set("FireRating", *w, PVal::Text("EI30".into()));
    }
    b.load(&[
        row("Windows", "FireRating", "", "", ""),
        row("Windows", "width", "range", "..1.0", ""),
        row("Doors", "FireRating", "one_of", "EI60", ""),
    ]);
    let rep = b.report();
    assert_eq!(rep.cats.len(), 2);
    let (w, d) = (&rep.cats[0], &rep.cats[1]);
    assert_eq!((w.cat.as_str(), w.total, w.compliant), ("window", 5, 0));
    assert_eq!((w.lines[0].checked, w.lines[0].missing()), (5, 1));
    assert_eq!(w.lines[1].invalid(), 5);
    assert_eq!(w.failing().len(), 5, "each element once");
    assert_eq!((d.cat.as_str(), d.total, d.compliant), ("door", 2, 0));
    assert!(d.lines[0].hits.iter().all(|h| h.missing && h.fixable), "one allowed text value");
    assert_eq!(rep.total(), 7);
    assert_eq!(rep.requirements(), 3);
    let (fixable, manual) = rep.fixable_split();
    assert_eq!((fixable, manual), (2 + 5, 1), "a width clamps into range; a blank waits");
    // the fix of a requirement finding is found again and undoes exactly
    let one = CheckScope::Elements(vec![("opening".into(), doors[0])]);
    let found = requirement_findings(&b.m, &one).unwrap();
    assert_eq!(found.len(), 1);
    let top = b.m.op_top().unwrap();
    b.m.apply_check_fix(&found[0].fix).unwrap();
    assert_eq!(b.m.op_top().unwrap(), top + 1);
    assert_eq!(b.report().cats[1].compliant, 1);
    b.m.undo().unwrap();
    assert_eq!(b.report().cats[1].compliant, 0);
}

#[test]
fn filters_narrow_by_level_and_phase() {
    let mut b = base();
    let (w1, w2) = (b.opening("window", 1.0, 1.2), b.opening("window", 5.0, 1.2));
    let phases = b.m.phases().unwrap();
    assert!(phases.len() >= 2, "the default phases are seeded");
    b.m.sql()
        .execute(
            "INSERT OR REPLACE INTO elem_phase(kind, ref, created) VALUES ('opening', ?1, ?2)",
            [w2, phases[1].id],
        )
        .unwrap();
    let mut t = row("window", "Code", "", "", "");
    t.phase = phases[1].name.clone();
    let mut on_level = row("window", "Code", "", "", "");
    on_level.level = "L1".into();
    b.load(&[t, on_level]);
    let rep = b.report();
    let l = &rep.cats[0].lines;
    assert_eq!((l[0].checked, l[0].elements()), (1, vec![("opening".to_string(), w2)]));
    assert_eq!(l[1].checked, 2);
    assert!(l[1].elements().contains(&("opening".to_string(), w1)));
}

#[test]
fn a_pattern_from_a_book_cannot_reach_the_sql() {
    let mut b = base();
    let w = b.opening("window", 1.0, 1.2);
    b.prop(w, "x' OR 1=1 --", "v");
    b.load(&[row("window", "x' OR 1=1 --", "one_of", "a'); DROP TABLE wall; --", "")]);
    let rep = b.report();
    assert_eq!(rep.cats[0].lines[0].invalid(), 1);
    assert!(b.m.sql().prepare("SELECT 1 FROM wall").is_ok());
}
