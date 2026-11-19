// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Per-operator draft diff and edit rejection.

use super::doc::{op_diff, FamDoc, OpChange};

const OLD: &str = "family Desk\nparam Width length type 1200mm\nparam Depth length type 600mm\nextrude Top rect(0, 0, Width, Depth) z(0, 40mm)\n";

#[test]
fn op_diff_add_change_remove() {
    let new = "family Desk\nparam Width length type 1400mm\nextrude Top rect(0, 0, Width, 600mm) z(0, 40mm)\nparam Leg length type 50mm\n";
    let d = op_diff(&FamDoc::new(OLD), new);
    assert_eq!(d.len(), 4, "{d:?}");
    assert!(matches!(&d[0], OpChange::Changed { old, new } if old.name == "Width" && new.text.contains("1400mm")));
    assert!(matches!(&d[1], OpChange::Changed { new, .. } if new.name == "Top"));
    assert!(matches!(&d[2], OpChange::Added(s) if s.name == "Leg"));
    assert!(matches!(&d[3], OpChange::Removed(s) if s.name == "Depth"));
    assert!(op_diff(&FamDoc::new(OLD), OLD).is_empty());
    // whitespace and comments outside string literals do not change the operator
    assert!(op_diff(&FamDoc::new(OLD), &OLD.replace("param Width length", "param   Width  length")).is_empty());
}

#[test]
fn op_diff_nested_block_whole() {
    let old = format!("{OLD}begin\nfamily Leg\nparam R length type 20mm\nend\n");
    let new = old.replace("20mm", "30mm");
    let d = op_diff(&FamDoc::new(&old), &new);
    assert_eq!(d.len(), 1, "{d:?}");
    let OpChange::Changed { old: o, new: n } = &d[0] else { panic!("{d:?}") };
    assert_eq!((o.head.as_str(), o.name.as_str()), ("begin", "Leg"));
    assert!(o.text.contains("20mm") && n.text.contains("30mm") && n.text.contains("end"));
}

#[test]
fn reject_op_reverts_text() {
    let new = "family Desk\nparam Width length type 1400mm\nparam Depth length type 600mm\nextrude Top rect(0, 0, Width, Depth) z(0, 40mm)\nparam Leg length type 50mm\n";
    let old = FamDoc::new(OLD);
    let mut doc = FamDoc::new(new);
    for c in op_diff(&old, doc.text()) {
        doc.reject_op(&c).unwrap();
    }
    assert!(op_diff(&old, doc.text()).is_empty(), "{}", doc.text());
    // each undo is its own history step
    assert!(doc.undo());
    assert_eq!(op_diff(&old, doc.text()).len(), 1);
    // a deleted operator comes back
    let mut d2 = FamDoc::new(OLD.replace("param Depth length type 600mm\n", "").replace("Depth", "500mm"));
    let ch = op_diff(&old, d2.text());
    assert!(ch.iter().any(|c| matches!(c, OpChange::Removed(s) if s.name == "Depth")));
    // the removed one first: the changed one refers to it
    for c in ch.into_iter().rev() {
        d2.reject_op(&c).unwrap();
    }
    assert!(op_diff(&old, d2.text()).is_empty(), "{}", d2.text());
}

/// The diff and the report of a 300-line draft together stay inside the card's budget
/// (200 ms in release; a debug build is an order slower).
#[test]
fn op_diff_and_report_budget_300_lines() {
    use super::validate::{validate, Limits};
    let make = |k: i32| {
        let mut t = String::from("family Big
param W length type 1000mm
");
        for i in 0..40 {
            t += &format!("extrude B{i} rect(0, 0, W, 100mm) z({}mm, {}mm)
", i * 10, i * 10 + 8 + k);
        }
        for i in 0..20 {
            t += &format!("type \"T{i}\" W={}mm
", 500 + i * 50);
        }
        while t.lines().count() < 300 {
            t += "# padding
";
        }
        t
    };
    let (old, new) = (make(0), make(1));
    let t0 = std::time::Instant::now();
    let d = op_diff(&FamDoc::new(old), &new);
    let r = validate(&new, &Limits::default());
    let ms = t0.elapsed().as_millis();
    assert_eq!(d.len(), 40, "every body changed once");
    assert!(r.ok, "{r:?}");
    let cap = if cfg!(debug_assertions) { 3000 } else { 200 };
    assert!(ms <= cap, "{ms} ms");
}
