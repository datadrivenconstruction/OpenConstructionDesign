// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The optional `ValueSources` sheet: absent by default (the book is the book it always was),
//! present on request with the Source and Verified columns, and never read back by import.

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{
    Edit, LayerSpec, Model, PVal, ParamSpec, ParamType, SrcKind, ValueSrc, WallSpec,
};

use super::tests::tmp;
use super::value_sources::{utc, SHEET};
use super::*;

const T0: i64 = 1_700_000_000;

/// Two walls with "Fire" set; the first one by the assistant, unverified.
fn model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m
        .add_wall_type("ext", &[LayerSpec { material: "brick".into(), thickness: 0.2 }])
        .unwrap();
    let mut ids = vec![];
    for y in [0.0, 4.0] {
        let w = WallSpec {
            a: (0.0, y),
            b: (5.0, y),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        ids.push(m.add_wall(&w).unwrap());
    }
    m.add_param(&ParamSpec {
        name: "Fire".into(),
        ptype: ParamType::Text,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    })
    .unwrap();
    let set = |id: i64| Edit::ParamValue {
        param: "Fire".into(),
        kind: "wall".into(),
        id,
        value: Some(PVal::Text("EI60".into())),
    };
    m.apply_edits(&[set(ids[0]), set(ids[1])]).unwrap();
    let src = ValueSrc::ai(SrcKind::Ai, T0, "s", "s#2", "=fill the fire ratings");
    m.record_value_src(&[("wall", ids[0], "Fire")], &src).unwrap();
    (m, ids[0])
}

fn export_with(m: &Model, from: &std::path::Path, to: &std::path::Path, on: bool) {
    let style = Style::default().with_sources(on);
    export_xlsx_styled(m, from, to, Units::Metric, &[], &style).unwrap();
}

fn ops(m: &Model) -> i64 {
    m.sql().query_row("SELECT count(*) FROM op", [], |r| r.get(0)).unwrap()
}

#[test]
fn the_sheet_is_absent_by_default() {
    let (m, _) = model();
    let p = tmp("vs_default");
    export_with(&m, &p, &p, false);
    assert!(!sheet_names(&p).unwrap().iter().any(|n| n == SHEET));
}

#[test]
fn on_request_it_lists_source_and_verified_and_import_ignores_it() {
    let (mut m, w) = model();
    let p = tmp("vs_on");
    export_with(&m, &p, &p, true);
    assert!(sheet_names(&p).unwrap().iter().any(|n| n == SHEET));
    let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
    let g = wb.worksheet_range(SHEET).unwrap();
    let text = |r: usize, c: usize| g.get((r, c)).map(|d| d.to_string()).unwrap_or_default();
    assert_eq!(text(0, 3), "Source");
    assert_eq!(text(0, 7), "Verified");
    assert_eq!(text(1, 0), "wall");
    assert_eq!(g.get((1, 1)), Some(&Data::Float(w as f64)));
    assert_eq!((text(1, 2), text(1, 3)), ("Fire".to_string(), "ai".to_string()));
    assert_eq!(text(1, 4), utc(T0));
    assert_eq!(text(1, 6), "=fill the fire ratings", "a prompt stays text");
    assert_eq!(text(1, 7), "", "not verified");
    assert!(g.get((2, 0)).is_none_or(|d| *d == Data::Empty), "one recorded value");
    drop(wb);
    let (before, n) = (m.value_srcs().unwrap(), ops(&m));
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(rep.applied && rep.changes.is_empty() && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(ops(&m), n, "nothing written");
    assert_eq!(m.value_srcs().unwrap(), before, "the records stay as they were");
}

#[test]
fn a_reexport_without_sources_does_not_keep_an_old_copy() {
    let (m, _) = model();
    let (a, b) = (tmp("vs_old"), tmp("vs_again"));
    export_with(&m, &a, &a, true);
    export_with(&m, &a, &b, false);
    assert!(!sheet_names(&b).unwrap().iter().any(|n| n == SHEET), "not a user sheet");
}

#[test]
fn time_text_is_the_same_everywhere() {
    assert_eq!(utc(T0), "2023-11-14 22:13 UTC");
    assert_eq!(utc(0), "1970-01-01 00:00 UTC");
}
