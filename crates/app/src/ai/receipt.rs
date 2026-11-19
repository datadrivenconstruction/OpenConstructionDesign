// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! What a write did, in numbers the agent can check: the receipt of a changeset, derived from
//! the card's diff, the quantity deltas and the new issues, never from what the agent asked for.

use super::plan::{ElemDiff, QtyDelta};
use super::validate::{IssueLevel, PlanIssue};
use ddc_ai::refusal::{self, RefusalCode};
use ddc_model::RowKind;
use serde_json::{json, Value as J};
use std::collections::BTreeMap;

/// The answer stays under this many bytes: counts stay exact, lists give way.
pub const MAX_BYTES: usize = 8 * 1024;
const MAX_QTY: usize = 24;
const MAX_FINDINGS: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct FindingBrief {
    pub key: String,
    pub block: bool,
    pub el: Option<String>,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WriteReceipt {
    /// `Model::change_head` after the write, or the head the draft stands on while it waits.
    pub token: u64,
    /// The write is in the model (one undo step) or still waits for the person's Apply.
    pub applied: bool,
    pub undo_label: String,
    pub n_created: usize,
    pub n_changed: usize,
    pub n_deleted: usize,
    pub created: Vec<String>,
    pub changed: Vec<String>,
    pub deleted: Vec<String>,
    pub qty: Vec<QtyDelta>,
    pub new_findings: Vec<FindingBrief>,
    /// IFC class and how many created or changed elements fall in it.
    pub ifc: Vec<(String, usize)>,
    /// List entries left out of `created`, `changed`, `deleted`, `qty` and `new_findings`.
    pub truncated: usize,
    /// The plan waits on the card for the person: a mass deletion of `n` of `of` elements.
    pub blocked: Option<(usize, usize)>,
}

/// The IFC class an element table exports as.
pub fn ifc_class(table: &str) -> &'static str {
    match table {
        "wall" => "IfcWall",
        "curtain_wall" => "IfcCurtainWall",
        "opening" => "IfcOpeningElement",
        "slab" => "IfcSlab",
        "room" => "IfcSpace",
        "col" => "IfcColumn",
        "beam" => "IfcBeam",
        "roof" => "IfcRoof",
        "ceiling" => "IfcCovering",
        "stair" => "IfcStair",
        "railing" => "IfcRailing",
        "mep_run" => "IfcDistributionElement",
        _ => "IfcBuildingElementProxy",
    }
}

/// `(n, of)` of the mass-deletion block among `issues`, the wipe included.
pub fn mass_blocked(issues: &[PlanIssue]) -> Option<(usize, usize)> {
    let i = issues
        .iter()
        .find(|i| matches!(i.text_key, "ai.issue.mass_delete" | "ai.issue.wipe"))?;
    Some((i.args.first()?.parse().ok()?, i.args.get(1)?.parse().ok()?))
}

fn el_ref(e: &ElemDiff) -> String {
    format!("{}:{}", e.key.0, e.key.1)
}

impl WriteReceipt {
    pub fn build(
        diff: &[ElemDiff],
        qty: &[QtyDelta],
        issues: &[PlanIssue],
        token: u64,
        applied: bool,
        undo_label: &str,
    ) -> WriteReceipt {
        let by = |k: RowKind| -> Vec<String> {
            diff.iter().filter(|e| e.kind == k).map(el_ref).collect()
        };
        let (created, changed, deleted) = (by(RowKind::Created), by(RowKind::Changed), by(RowKind::Deleted));
        let mut classes: BTreeMap<&str, usize> = BTreeMap::new();
        for e in diff.iter().filter(|e| e.kind != RowKind::Deleted) {
            *classes.entry(ifc_class(&e.key.0)).or_default() += 1;
        }
        let new_findings = issues
            .iter()
            .map(|i| FindingBrief {
                key: i.text_key.to_string(),
                block: i.level == IssueLevel::Block,
                el: i.el.as_ref().map(|k| format!("{}:{}", k.0, k.1)),
                args: i.args.clone(),
            })
            .collect();
        WriteReceipt {
            token,
            applied,
            undo_label: undo_label.to_string(),
            n_created: created.len(),
            n_changed: changed.len(),
            n_deleted: deleted.len(),
            created,
            changed,
            deleted,
            qty: qty.to_vec(),
            new_findings,
            ifc: classes.into_iter().map(|(k, n)| (k.to_string(), n)).collect(),
            truncated: 0,
            blocked: mass_blocked(issues),
        }
        .capped(MAX_BYTES)
    }

    fn entries(&self) -> usize {
        self.created.len() + self.changed.len() + self.deleted.len() + self.qty.len() + self.new_findings.len()
    }

    /// Lists shortened, longest first, until the answer fits `max` bytes; counts are not touched.
    pub fn capped(mut self, max: usize) -> WriteReceipt {
        let full = self.entries() + self.truncated;
        self.qty.truncate(MAX_QTY);
        self.new_findings.truncate(MAX_FINDINGS);
        while self.to_json().to_string().len() > max {
            let lists = [
                self.created.len(),
                self.changed.len(),
                self.deleted.len(),
                self.qty.len(),
                self.new_findings.len(),
            ];
            let (at, longest) = lists.iter().enumerate().max_by_key(|x| *x.1).map(|(i, n)| (i, *n)).unwrap();
            if longest == 0 {
                break;
            }
            let keep = longest / 2;
            match at {
                0 => self.created.truncate(keep),
                1 => self.changed.truncate(keep),
                2 => self.deleted.truncate(keep),
                3 => self.qty.truncate(keep),
                _ => self.new_findings.truncate(keep),
            }
        }
        self.truncated = full - self.entries();
        self
    }

    pub fn to_json(&self) -> J {
        json!({
            "token": format!("c:{}", self.token),
            "applied": self.applied,
            "undo": self.undo_label,
            "counts": {"created": self.n_created, "changed": self.n_changed, "deleted": self.n_deleted},
            "created": self.created,
            "changed": self.changed,
            "deleted": self.deleted,
            "qty": self.qty.iter().map(|q| json!({
                "key": q.key, "level": q.level, "before": q.before, "after": q.after,
            })).collect::<Vec<_>>(),
            "new_findings": self.new_findings.iter().map(|f| json!({
                "key": f.key, "block": f.block, "el": f.el, "args": f.args,
            })).collect::<Vec<_>>(),
            "ifc": self.ifc.iter().map(|(c, n)| json!({"class": c, "count": n})).collect::<Vec<_>>(),
            "truncated": self.truncated,
            "blocked": self.blocked.map(|(n, of)| {
                let r = refusal::of(RefusalCode::MassDeleteBlocked)
                    .arg("n", n.to_string())
                    .arg("of", of.to_string());
                refusal::to_wire(&r, "en")
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(table: &str, id: i64, kind: RowKind) -> ElemDiff {
        ElemDiff {
            key: (table.to_string(), id),
            kind,
            item: None,
            level: None,
            label: String::new(),
            cells: vec![],
        }
    }

    #[test]
    fn counts_are_exact_and_the_answer_fits() {
        let diff: Vec<ElemDiff> = (0..10_000)
            .map(|i| el("wall", i, if i % 3 == 0 { RowKind::Created } else { RowKind::Changed }))
            .collect();
        let r = WriteReceipt::build(&diff, &[], &[], 7, true, "AI");
        assert_eq!(r.n_created + r.n_changed, 10_000);
        assert_eq!(r.n_created, 3334);
        assert!(r.to_json().to_string().len() <= MAX_BYTES);
        assert_eq!(r.created.len() + r.changed.len() + r.truncated, 10_000);
        assert_eq!(r.ifc, vec![("IfcWall".to_string(), 10_000)]);
    }

    /// A thousand changes with their quantity rows answer in well under the budget.
    #[test]
    fn a_receipt_of_a_thousand_changes_is_fast() {
        let diff: Vec<ElemDiff> = (0..1000).map(|i| el("wall", i, RowKind::Changed)).collect();
        let qty: Vec<QtyDelta> = (0..40)
            .map(|i| QtyDelta { key: format!("volume:m{i}"), level: Some(1), before: 1.0, after: 2.0 })
            .collect();
        let t = std::time::Instant::now();
        let r = WriteReceipt::build(&diff, &qty, &[], 1, true, "AI");
        let ms = t.elapsed().as_secs_f64() * 1e3;
        eprintln!("receipt of 1000 changes: {ms:.2} ms");
        assert_eq!(r.n_changed, 1000);
        let budget = if cfg!(debug_assertions) { 300.0 } else { 30.0 };
        assert!(ms <= budget, "{ms} ms");
    }

    #[test]
    fn a_small_write_is_listed_whole() {
        let diff = vec![el("wall", 1, RowKind::Changed), el("col", 2, RowKind::Created), el("wall", 3, RowKind::Deleted)];
        let r = WriteReceipt::build(&diff, &[], &[], 3, false, "AI");
        assert_eq!((r.truncated, r.created.clone(), r.deleted.clone()), (0, vec!["col:2".to_string()], vec!["wall:3".to_string()]));
        // a deleted element has no class any more
        assert_eq!(r.ifc, vec![("IfcColumn".to_string(), 1), ("IfcWall".to_string(), 1)]);
        assert_eq!(r.to_json()["token"], "c:3");
    }
}
