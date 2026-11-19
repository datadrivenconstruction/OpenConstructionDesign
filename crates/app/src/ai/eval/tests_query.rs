// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Query, takeoff and network-size oracles, in both outcomes.

use super::case::Case;
use super::facts::Facts;
use super::oracle::{evaluate, EvalCx, OracleResult};
use super::query_oracles::{sizing_key, total, within};
use super::tests::editor;

fn check(toml: &str) -> Vec<OracleResult> {
    let ed = editor();
    let f = Facts::read(&ed.model).unwrap();
    let c = Case::parse(&format!("id=\"t\"\n[[turn]]\nsay=\"x\"\n{toml}")).unwrap();
    let cx = EvalCx { ed: &ed, facts: &f, before: None, answer: "", ifc: None, errors_before: 0, receipts: &[] };
    evaluate(&c, &cx)
}

#[test]
fn query_counts_and_names_a_bad_query() {
    let r = check("[[oracle.query]]\nq=\"wall\"\nmax=0.0\n[[oracle.query]]\nq=\"wall\"\nmin=1.0\n[[oracle.query]]\nq=\"((\"\nmin=1.0\n");
    assert!(r[0].pass && r[0].measured == "0 element(s)", "{:?}", r[0]);
    assert!(!r[1].pass, "{:?}", r[1]);
    assert!(!r[2].pass && r[2].measured.contains("does not parse"), "{:?}", r[2]);
}

#[test]
fn qty_tolerance_is_a_share_of_the_target() {
    assert!(within(8400.0, 8000.0, 0.1));
    assert!(within(7200.0, 8000.0, 0.1));
    assert!(!within(8801.0, 8000.0, 0.1));
    assert!(!within(7199.0, 8000.0, 0.1));
    assert!(within(0.0, 0.0, 0.0) && !within(1.0, 0.0, 0.0));
    let mut t = ddc_model::Totals::default();
    t.slab_area = 7900.0;
    t.walls = 3;
    assert_eq!(total(&t, "slab_area"), Some(7900.0));
    assert_eq!(total(&t, "walls"), Some(3.0));
    assert_eq!(total(&t, "nonsense"), None);
}

#[test]
fn qty_reads_the_model_and_fails_on_unknown_or_per_level() {
    let r = check(
        "[[oracle.qty]]\nkey=\"slab_area\"\nvalue=0.0\ntol=0.0\n[[oracle.qty]]\nkey=\"slab_area\"\nvalue=8000.0\n\
         [[oracle.qty]]\nkey=\"bogus\"\nvalue=1.0\n[[oracle.qty]]\nkey=\"rooms\"\nlevel=2\nvalue=1.0\n",
    );
    assert!(r[0].pass && r[0].measured == "0.00", "{:?}", r[0]);
    assert!(!r[1].pass && r[1].measured == "0.00", "{:?}", r[1]);
    assert!(!r[2].pass && r[2].measured.contains("unknown takeoff key"), "{:?}", r[2]);
    assert!(!r[3].pass && r[3].measured.contains("per-level"), "{:?}", r[3]);
}

#[test]
fn sizing_notes_are_told_from_errors() {
    for k in ["mep.issue.undersized", "mep.issue.undersized_no_size", "mep.issue.overflow", "mep.issue.overload", "mep.issue.port_size", "mep.issue.slope_low"] {
        assert!(sizing_key(k), "{k}");
    }
    for k in ["mep.issue.network", "mep.issue.open_end", "mep.issue.slope_reverse", "mep.issue.no_flow"] {
        assert!(!sizing_key(k), "{k}");
    }
}

#[test]
fn sizes_typical_without_runs_fails_instead_of_passing_quietly() {
    let r = check("[[oracle.geom]]\nkind=\"sizes_typical\"\n");
    assert!(!r[0].pass && r[0].measured.starts_with("no runs"), "{:?}", r[0]);
}
