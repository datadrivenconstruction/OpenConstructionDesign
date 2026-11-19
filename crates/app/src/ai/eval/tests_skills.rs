// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The guide triggers, the order cases run in and the nightly gate.

use super::case::{self, Case};
use super::metrics::{gate, last_line, GATE_PASS_DROP};
use super::run::phases;
use super::tests::editor;
use super::triggers::{self, first_guide_read, TriggerSet, MIN_NO, MIN_YES};
use crate::ai::mcp_bridge::RESOURCE_READ;
use ddc_model::AuditEntry;
use serde_json::json;

#[test]
fn skills_trigger_cases_min() {
    let sets = triggers::load_dir(&triggers::triggers_dir()).unwrap();
    let mut covered: Vec<&str> = sets.iter().map(|s| s.skill.as_str()).collect();
    covered.sort();
    let mut guides: Vec<&str> = ddc_ai::skills::SKILLS.iter().map(|s| s.id).collect();
    guides.sort();
    assert_eq!(covered, guides, "one trigger set per guide");
    for s in &sets {
        assert!(s.problems().is_empty(), "{:?}", s.problems());
        assert!(s.yes.len() >= MIN_YES && s.no.len() >= MIN_NO);
    }
    // the card's own pair: a lattice mast reads the structure guide, painting walls reads none
    let st = sets.iter().find(|s| s.skill == "structure").unwrap();
    let (yes, no) = (super::tests_h01::ru("structure_yes"), super::tests_h01::ru("structure_no"));
    assert!(st.yes.iter().any(|p| p.contains(&yes)));
    assert!(st.no.iter().any(|p| p.contains(&no)));
    for s in &sets {
        assert!(s.no.iter().chain(&s.yes).all(|p| !p.trim().is_empty()));
    }
    // a set short of evidence is named, not passed
    let thin = TriggerSet { skill: "mep".into(), yes: vec!["a".into()], no: vec![] };
    assert_eq!(thin.problems().len(), 2);
    let odd = TriggerSet { skill: "nope".into(), yes: vec!["x".into(); 5], no: vec!["x".into(); 3] };
    assert!(odd.problems().iter().any(|p| p.contains("no such guide")));
    assert!(odd.problems().iter().any(|p| p.contains("both yes and no")));
}

#[test]
fn trigger_oracle_reads_the_audit() {
    let ed = editor();
    let read = |uri: &str| AuditEntry {
        session: "s".into(),
        actor: "mcp:agent".into(),
        event: RESOURCE_READ.into(),
        endpoint: Some(uri.into()),
        status: "ok".into(),
        ..AuditEntry::default()
    };
    let start = ed.model.audit_append(&AuditEntry { event: "mcp_connect".into(), status: "ok".into(), ..AuditEntry::default() }).unwrap();
    assert_eq!(first_guide_read(&ed.model, start), None, "nothing read yet");
    ed.model.audit_append(&read("ddc://skills/structure")).unwrap();
    let mid = ed.model.audit_append(&read("ddc://skills/safe_edits")).unwrap();
    assert_eq!(first_guide_read(&ed.model, start), Some("structure"));
    assert_eq!(first_guide_read(&ed.model, mid - 1), Some("safe_edits"), "only reads after the request count");

    // scoring a set against what the audit says each request read first
    let set = TriggerSet {
        skill: "structure".into(),
        yes: vec!["build a mast".into(), "build a truss".into()],
        no: vec!["paint the walls".into()],
    };
    let first = |p: &str| match p {
        "build a mast" => Some("structure".to_string()),
        "build a truss" => Some("mep".to_string()),
        _ => None,
    };
    assert_eq!(set.misses(first), vec!["structure yes: build a truss".to_string()]);
    let painted = |_: &str| Some("structure".to_string());
    assert_eq!(set.misses(painted), vec!["structure no: paint the walls".to_string()]);
}

#[test]
fn smoke_last() {
    let all = case::load_dir(&case::evals_dir().join("cases")).unwrap();
    for c in all.iter().filter(|c| c.id.starts_with('S')) {
        assert_eq!(c.tier, "smoke", "{}: the simple houses are smoke", c.id);
    }
    let order: Vec<&Case> = phases(&all).concat().into_iter().map(|i| &all[i]).collect();
    assert_eq!(order.len(), all.len(), "every case starts once");
    let first_smoke = order.iter().position(|c| c.tier == "smoke").expect("smoke cases exist");
    assert!(order[first_smoke..].iter().all(|c| c.tier == "smoke"), "nothing but smoke after the first smoke case");
    assert!(order[..first_smoke].iter().any(|c| c.tier == "hard"));
    // a case built on an earlier result starts after that result
    for (k, c) in order.iter().enumerate() {
        if let Some(base) = c.base.strip_prefix('@') {
            let at = order.iter().position(|o| o.id == base).unwrap_or_else(|| panic!("{}: no base {base}", c.id));
            assert!(at < k, "{} starts before its base {base}", c.id);
        }
    }
    // a hard run leaves the simple houses out of its share of passes
    assert!(all.iter().filter(|c| c.tier == "hard").all(|c| !c.id.starts_with('S')));
}

#[test]
fn nightly_gate_against_the_last_line() {
    let line = |pass: f64, usd: f64, cases: u64| json!({"provider": "live", "tier": "hard", "cases": cases, "pass_rate": pass, "usd": usd});
    let base = line(0.60, 10.0, 10);
    assert!(gate(&base, &line(0.50, 10.0, 10)).is_empty(), "ten points down is still within");
    let drop = gate(&base, &line(0.45, 10.0, 10));
    assert_eq!(drop.len(), 1);
    assert!(drop[0].starts_with("pass_rate 60 % -> 45 %"), "{drop:?}");
    assert!(gate(&base, &line(0.60, 12.5, 10)).is_empty(), "a quarter more per case is still within");
    assert_eq!(gate(&base, &line(0.60, 13.0, 10)).len(), 1);
    // per case: the same total over twice the cases is cheaper, not dearer
    assert!(gate(&base, &line(0.60, 20.0, 20)).is_empty());
    assert!(GATE_PASS_DROP > 0.0);

    let dir = std::env::temp_dir().join(format!("ddc_gate_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ledger = dir.join("ai_eval.jsonl");
    let text = [line(0.7, 1.0, 1), json!({"provider": "replay", "tier": "hard", "pass_rate": 1.0}), line(0.6, 1.0, 1), json!({"provider": "live", "tier": "smoke", "pass_rate": 0.1})]
        .iter()
        .map(|j| j.to_string() + "\n")
        .collect::<String>();
    std::fs::write(&ledger, text).unwrap();
    assert_eq!(last_line(&ledger, "live", "hard").unwrap()["pass_rate"], json!(0.6), "the newest of the same provider and tier");
    assert!(last_line(&ledger, "live", "edit").is_none(), "the first line of a tier is the base");
    let _ = std::fs::remove_dir_all(&dir);
}
