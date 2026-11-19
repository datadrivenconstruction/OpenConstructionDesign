// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A parameter whose name collides with a sheet column or another parameter reaches the book
//! under its own header and reads back to itself; a column that fits several is refused.

use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{Edit, LayerSpec, Model, PVal, ParamSpec, ParamType, WallSpec};
use rust_xlsxwriter::Workbook;

use super::msg::{fill_template, match_template};
use super::param_head::{self, Bound};
use super::*;

type Grid = Vec<Vec<Data>>;

const FIRE_RU: &str = "\u{41e}\u{433}\u{43d}\u{435}\u{441}\u{442}\u{43e}\
    \u{439}\u{43a}\u{43e}\u{441}\u{442}\u{44c}";
const FIRE_RU_UP: &str = "\u{41e}\u{413}\u{41d}\u{415}\u{421}\u{422}\u{41e}\
    \u{419}\u{41a}\u{41e}\u{421}\u{422}\u{42c}";

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("ddc_xlsx_tests");
    std::fs::create_dir_all(&d).unwrap();
    d.join(format!("{}_phd_{name}.xlsx", std::process::id()))
}

fn walls() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = LayerSpec { material: "brick".into(), thickness: 0.25 };
    let t = m.add_wall_type("ext", &[layer]).unwrap();
    let w = WallSpec {
        a: (0.0, 0.0),
        b: (4.0, 0.0),
        level: lv,
        base_offset: 0.0,
        height: 3.0,
        wall_type: t,
        offset0: None,
    };
    let id = m.add_wall(&w).unwrap();
    (m, id)
}

fn spec(name: &str, t: ParamType) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        ptype: t,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    }
}

fn set_value(m: &mut Model, name: &str, id: i64, v: PVal) {
    let e = Edit::ParamValue { param: name.into(), kind: "wall".into(), id, value: Some(v) };
    m.apply_edits(&[e]).unwrap();
}

fn value(m: &Model, name: &str, id: i64) -> Option<PVal> {
    let d = m.param_by_name(name).unwrap().unwrap();
    m.param_value(d.id, "wall", id).unwrap()
}

fn wall_height(m: &Model, id: i64) -> f64 {
    m.sql().query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0)).unwrap()
}

fn export(m: &Model, name: &str) -> PathBuf {
    let p = tmp(name);
    export_xlsx(m, &p).unwrap();
    p
}

fn grid(p: &Path, sheet: &str) -> Grid {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    wb.worksheet_range(sheet).unwrap().rows().map(|r| r.to_vec()).collect()
}

fn heads(p: &Path) -> Vec<String> {
    grid(p, WALLS)[0].iter().map(|d| d.to_string()).collect()
}

/// Copies a book cell by cell, `f` edits the grids.
fn edit(src: &Path, name: &str, f: impl Fn(&str, &mut Grid)) -> PathBuf {
    let dst = tmp(name);
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for sheet in wb.sheet_names() {
        let mut g: Grid = wb.worksheet_range(&sheet).unwrap().rows().map(|r| r.to_vec()).collect();
        f(&sheet, &mut g);
        let ws = out.add_worksheet();
        ws.set_name(&sheet).unwrap();
        for (i, row) in g.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                    Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
    }
    out.save(&dst).unwrap();
    dst
}

fn col(g: &Grid, header: &str) -> usize {
    g[0].iter()
        .position(|d| d.to_string() == header)
        .unwrap_or_else(|| panic!("no column {header}: {:?}", g[0]))
}

fn put(g: &mut Grid, r: usize, c: usize, v: Data) {
    while g[r].len() <= c {
        g[r].push(Data::Empty);
    }
    g[r][c] = v;
}

fn opts(apply: bool) -> ImportOptions {
    ImportOptions { apply, delete_missing: false, new_params: false }
}

fn no_op(m: &mut Model, p: &Path) {
    let rep = import_xlsx_with(m, p, opts(false)).unwrap();
    assert!(
        rep.changes.is_empty()
            && rep.rejected.is_empty()
            && rep.proposals.is_empty()
            && rep.ignored.is_empty()
            && rep.conflicts.is_empty(),
        "{rep:?}"
    );
}

#[test]
fn tag_syntax_reads_name_id_and_a_unit_after_it() {
    assert_eq!(param_head::split_tag("Height (#12)"), Some(("Height", 12)));
    assert_eq!(param_head::split_tag(" Height (#12) [mm] "), Some(("Height", 12)));
    assert_eq!(param_head::split_tag("Cost [USD] (#3)"), Some(("Cost [USD]", 3)));
    assert_eq!(param_head::split_tag("Foo (#1) (#2)"), Some(("Foo (#1)", 2)));
    for no in ["Height", "(#12)", "Height (#)", "Height (#x)", "Height (#12) tail"] {
        assert_eq!(param_head::split_tag(no), None, "{no}");
    }
}

#[test]
fn bind_prefers_the_exact_name_and_never_guesses_between_two() {
    let (mut m, _) = walls();
    m.add_param(&spec("Cost [EUR]", ParamType::Number)).unwrap();
    m.add_param(&spec("Cost [USD]", ParamType::Number)).unwrap();
    let own = m.params().unwrap();
    let is = |h: &str, want: &str| match param_head::bind(h, &own) {
        Bound::One(d) => assert_eq!(d.spec.name, want, "{h}"),
        _ => panic!("{h} did not bind"),
    };
    is("Cost [EUR]", "Cost [EUR]");
    is(" Cost [USD] ", "Cost [USD]");
    assert!(matches!(param_head::bind("Cost [GBP]", &own), Bound::Many(n) if n.len() == 2));
    assert!(matches!(param_head::bind("cost", &own), Bound::Many(n) if n.len() == 2));
    assert!(matches!(param_head::bind("Other", &own), Bound::None));
    assert!(matches!(param_head::bind("Cost [EUR] (#99999)", &own), Bound::StaleTag));
}

#[test]
fn two_parameters_differing_by_brackets_round_trip_apart() {
    let (mut m, w) = walls();
    m.add_param(&spec("Cost [EUR]", ParamType::Number)).unwrap();
    let usd = m.add_param(&spec("Cost [USD]", ParamType::Number)).unwrap();
    set_value(&mut m, "Cost [EUR]", w, PVal::Num(10.0));
    set_value(&mut m, "Cost [USD]", w, PVal::Num(20.0));
    let p = export(&m, "cost");
    let tagged = format!("Cost [USD] (#{usd})");
    let h = heads(&p);
    assert!(h.iter().any(|x| x == "Cost [EUR]") && h.contains(&tagged), "{h:?}");
    no_op(&mut m, &p);
    let t = tagged.clone();
    let q = edit(&p, "cost_edit", move |s, g| {
        if s == WALLS {
            let (a, b) = (col(g, "Cost [EUR]"), col(g, &t));
            assert_eq!(g[1][a].to_string(), "10");
            assert_eq!(g[1][b].to_string(), "20");
            put(g, 1, a, Data::Float(11.0));
            put(g, 1, b, Data::Float(22.0));
        }
    });
    let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(value(&m, "Cost [EUR]", w), Some(PVal::Num(11.0)));
    assert_eq!(value(&m, "Cost [USD]", w), Some(PVal::Num(22.0)));
}

#[test]
fn two_parameters_differing_by_case_round_trip_apart() {
    for (a, b) in [("Area", "AREA"), (FIRE_RU, FIRE_RU_UP)] {
        let (mut m, w) = walls();
        m.add_param(&spec(a, ParamType::Text)).unwrap();
        let second = m.add_param(&spec(b, ParamType::Text)).unwrap();
        set_value(&mut m, a, w, PVal::Text("one".into()));
        set_value(&mut m, b, w, PVal::Text("two".into()));
        let p = export(&m, if a == "Area" { "case_en" } else { "case_ru" });
        let tagged = format!("{b} (#{second})");
        assert!(heads(&p).contains(&tagged), "{:?}", heads(&p));
        no_op(&mut m, &p);
        let (x, t) = (a.to_string(), tagged.clone());
        let q = edit(&p, if a == "Area" { "case_en_e" } else { "case_ru_e" }, move |s, g| {
            if s == WALLS {
                let (i, k) = (col(g, &x), col(g, &t));
                put(g, 1, i, Data::String("uno".into()));
                put(g, 1, k, Data::String("dos".into()));
            }
        });
        let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
        assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
        assert_eq!(value(&m, a, w), Some(PVal::Text("uno".into())));
        assert_eq!(value(&m, b, w), Some(PVal::Text("dos".into())));
    }
}

#[test]
fn parameter_named_as_a_fixed_column_is_edited_without_touching_the_column() {
    let (mut m, w) = walls();
    let id = m.add_param(&spec("Height", ParamType::Length)).unwrap();
    set_value(&mut m, "Height", w, PVal::Num(9.0));
    let p = export(&m, "height");
    let tagged = format!("Height (#{id}) [m]");
    assert!(heads(&p).contains(&tagged), "{:?}", heads(&p));
    no_op(&mut m, &p);
    let t = tagged.clone();
    let q = edit(&p, "height_edit", move |s, g| {
        if s == WALLS {
            let c = col(g, &t);
            put(g, 1, c, Data::Float(8.0));
        }
    });
    let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(value(&m, "Height", w), Some(PVal::Num(8.0)));
    assert_eq!(wall_height(&m, w), 3.0);
}

#[test]
fn collision_header_follows_the_units_of_the_book() {
    let (mut m, w) = walls();
    let id = m.add_param(&spec("Height", ParamType::Length)).unwrap();
    set_value(&mut m, "Height", w, PVal::Num(3.048));
    let p = tmp("height_ft");
    export_xlsx_units(&m, &p, Units::Imperial).unwrap();
    let tagged = format!("Height (#{id}) [ft]");
    let g = grid(&p, WALLS);
    let c = col(&g, &tagged);
    assert!((g[1][c].to_string().parse::<f64>().unwrap() - 10.0).abs() < 1e-9);
    no_op(&mut m, &p);
}

#[test]
fn bracket_name_of_a_length_parameter_is_not_read_as_its_unit() {
    let (mut m, w) = walls();
    m.add_param(&spec("Span [main]", ParamType::Length)).unwrap();
    set_value(&mut m, "Span [main]", w, PVal::Num(2.0));
    let p = export(&m, "span");
    no_op(&mut m, &p);
    let q = edit(&p, "span_edit", |s, g| {
        if s == WALLS {
            let c = col(g, "Span [main] [m]");
            put(g, 1, c, Data::Float(2.5));
        }
    });
    let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(value(&m, "Span [main]", w), Some(PVal::Num(2.5)));
}

#[test]
fn second_export_keeps_one_column_per_parameter() {
    let (mut m, w) = walls();
    let id = m.add_param(&spec("Height", ParamType::Text)).unwrap();
    set_value(&mut m, "Height", w, PVal::Text("tall".into()));
    let p = export(&m, "twice");
    export_xlsx(&m, &p).unwrap();
    let tagged = format!("Height (#{id})");
    let n = heads(&p).iter().filter(|h| **h == tagged).count();
    assert_eq!(n, 1, "{:?}", heads(&p));
    no_op(&mut m, &p);
}

#[test]
fn hand_added_column_fitting_two_parameters_writes_into_neither() {
    let (mut m, w) = walls();
    m.add_param(&spec("Cost [EUR]", ParamType::Number)).unwrap();
    m.add_param(&spec("Cost [USD]", ParamType::Number)).unwrap();
    set_value(&mut m, "Cost [EUR]", w, PVal::Num(10.0));
    let p = export(&m, "hand");
    let q = edit(&p, "hand_edit", |s, g| {
        if s == WALLS {
            let c = g[0].len();
            put(g, 0, c, Data::String("Cost [GBP]".into()));
            put(g, 1, c, Data::Float(99.0));
        }
    });
    let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
    let why = fill_template(
        msg::PARAM_AMBIGUOUS.1,
        &["Cost [GBP]", "Cost [EUR], Cost [USD]"],
    );
    assert!(rep.ignored.iter().any(|i| i.reason == why), "{:?}", rep.ignored);
    assert!(rep.proposals.is_empty(), "{:?}", rep.proposals);
    assert_eq!(value(&m, "Cost [EUR]", w), Some(PVal::Num(10.0)));
    assert_eq!(value(&m, "Cost [USD]", w), None);
}

#[test]
fn stale_tag_is_refused_and_never_a_new_parameter() {
    let (mut m, _) = walls();
    let p = export(&m, "stale");
    let q = edit(&p, "stale_edit", |s, g| {
        if s == WALLS {
            let c = g[0].len();
            put(g, 0, c, Data::String("Gone (#4242)".into()));
            put(g, 1, c, Data::Float(1.0));
        }
    });
    let rep = import_xlsx_with(&mut m, &q, opts(false)).unwrap();
    let why = fill_template(msg::PARAM_TAG_STALE.1, &["Gone (#4242)"]);
    assert!(rep.ignored.iter().any(|i| i.reason == why), "{:?}", rep.ignored);
    assert!(rep.proposals.is_empty(), "{:?}", rep.proposals);
}

#[test]
fn old_book_with_a_plain_header_still_imports() {
    let (mut m, w) = walls();
    m.add_param(&spec("Fire", ParamType::Text)).unwrap();
    let p = export(&m, "old");
    let q = edit(&p, "old_edit", |s, g| {
        if s == WALLS {
            let c = col(g, "Fire");
            put(g, 0, c, Data::String("FIRE".into()));
            put(g, 1, c, Data::String("EI60".into()));
        }
    });
    let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(value(&m, "Fire", w), Some(PVal::Text("EI60".into())));
}

#[test]
fn collision_is_one_undo_step_and_preview_changes_nothing() {
    let (mut m, w) = walls();
    let id = m.add_param(&spec("Height", ParamType::Text)).unwrap();
    set_value(&mut m, "Height", w, PVal::Text("a".into()));
    let p = export(&m, "undo");
    let t = format!("Height (#{id})");
    let q = edit(&p, "undo_edit", move |s, g| {
        if s == WALLS {
            let c = col(g, &t);
            put(g, 1, c, Data::String("b".into()));
        }
    });
    let rep = import_xlsx_with(&mut m, &q, opts(false)).unwrap();
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert_eq!(value(&m, "Height", w), Some(PVal::Text("a".into())));
    let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(value(&m, "Height", w), Some(PVal::Text("b".into())));
    m.undo().unwrap();
    assert_eq!(value(&m, "Height", w), Some(PVal::Text("a".into())));
}

#[test]
fn param_head_reasons_have_en_and_ru_texts() {
    let cat = |src: &str| -> std::collections::BTreeMap<String, String> {
        serde_json::from_str(src).unwrap()
    };
    let en = cat(include_str!("../../../ui/data/i18n/en/xl_param_head.json"));
    let ru = cat(include_str!("../../../ui/data/i18n/ru/xl_param_head.json"));
    assert_eq!(en.keys().collect::<Vec<_>>(), ru.keys().collect::<Vec<_>>());
    let cyr = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4FF}').contains(&c));
    for (key, tpl) in msg::PARAM_HEAD_REASONS {
        assert_eq!(en.get(key).map(String::as_str), Some(tpl), "{key}");
        let r = &ru[key];
        assert!(cyr(r) && r != tpl, "{key}: {r}");
        assert_eq!(tpl.matches("{}").count(), r.matches("{}").count(), "{key}");
    }
    // a non-ASCII name passes through the template into the Russian text
    let text = fill_template(msg::PARAM_TAG_STALE.1, &[FIRE_RU]);
    let args = match_template(msg::PARAM_TAG_STALE.1, &text).unwrap();
    let shown = fill_template(&ru[msg::PARAM_TAG_STALE.0], &args);
    assert!(shown.contains(FIRE_RU) && !shown.contains("{}"), "{shown}");
}
