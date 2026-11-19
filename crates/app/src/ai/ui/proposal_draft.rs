// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! What an agent proposal would do, read from its dry run: element rows with human names, counts
//! by kind and the live elements it touches. Read only: the card and the view outline use it,
//! the model changes only through the card's Apply.

use super::logic::Counts;
use crate::ai::session::{ProposalCard, ProposalState};
use ddc_model::{ElKey, RowKind};
use ddc_ops::{Cat, Ref};
use ddc_ui::i18n::Catalog;
use serde_json::Value as J;
use std::collections::HashSet;

/// One element the dry run created, changed or deleted.
#[derive(Clone, Debug, PartialEq)]
pub struct DraftRow {
    pub kind: RowKind,
    /// Wire category (`wall`, `door`); empty when the label names none.
    pub cat: String,
    pub id: Option<i64>,
    /// Unique name of a named ref (`level:"Level 1"`).
    pub name: Option<String>,
    /// The label as the executor wrote it, for rows that parse to nothing better.
    pub raw: String,
    /// Live element to outline and select. `None` for created rows: they exist only after Apply.
    pub key: Option<ElKey>,
}

/// Categories with their own word in the catalog (`ai.proposal.el.<cat>`); others read "Element".
pub const WORDS: [&str; 20] = [
    "wall",
    "door",
    "window",
    "opening",
    "curtain_wall",
    "slab",
    "roof",
    "ceiling",
    "column",
    "beam",
    "stair",
    "railing",
    "ramp",
    "component",
    "room",
    "shaft",
    "mep_run",
    "level",
    "grid",
    "mesh",
];

/// Model table of a category the view can outline and select (the preview's element tables).
fn table_of(c: Cat) -> Option<&'static str> {
    Some(match c {
        Cat::Wall => "wall",
        Cat::Door | Cat::Window | Cat::Opening => "opening",
        Cat::CurtainWall => "curtain_wall",
        Cat::Slab => "slab",
        Cat::Room => "room",
        Cat::MepRun => "mep_run",
        Cat::Component => "comp",
        Cat::Mesh => "mesh",
        Cat::Column => "col",
        Cat::Beam => "beam",
        Cat::Ceiling => "ceiling",
        Cat::Roof => "roof",
        Cat::Stair => "stair",
        Cat::Railing => "railing",
        _ => return None,
    })
}

/// A face or edge counts as its element.
fn base(r: Ref) -> Ref {
    match r {
        Ref::Part { of, .. } => base(*of),
        r => r,
    }
}

fn row(kind: RowKind, label: &str) -> DraftRow {
    let mut out = DraftRow {
        kind,
        cat: String::new(),
        id: None,
        name: None,
        raw: crate::ai::fmt::text(label),
        key: None,
    };
    match Ref::parse(label).map(base) {
        Ok(Ref::Id { cat, id }) => {
            out.cat = cat.key().to_string();
            out.id = Some(id);
            if kind != RowKind::Created {
                out.key = table_of(cat).map(|t| (t.to_string(), id));
            }
        }
        Ok(Ref::Name { cat, name }) => {
            out.cat = cat.key().to_string();
            out.name = Some(crate::ai::fmt::text(&name));
        }
        _ => {}
    }
    out
}

fn labels(diff: &J, field: &str) -> Vec<String> {
    let Some(a) = diff.get(field).and_then(J::as_array) else {
        return Vec::new();
    };
    a.iter()
        .filter_map(|v| match v {
            J::String(s) => Some(s.clone()),
            // `created`: `{slot, ref}`
            J::Object(o) => o.get("ref").and_then(J::as_str).map(str::to_string),
            _ => None,
        })
        .collect()
}

/// Rows of the dry run's `diff` (`ops_exec::receipt_json`), in `+ ~ -` order. Copies are new
/// elements, so they read as created.
pub fn rows(diff: &J) -> Vec<DraftRow> {
    let mut out = Vec::new();
    for (field, kind) in [
        ("created", RowKind::Created),
        ("copied", RowKind::Created),
        ("changed", RowKind::Changed),
        ("deleted", RowKind::Deleted),
    ] {
        out.extend(labels(diff, field).iter().map(|l| row(kind, l)));
    }
    out
}

pub fn counts(rows: &[DraftRow]) -> Counts {
    let mut c = Counts::default();
    rows.iter().for_each(|r| c.add(r.kind));
    c
}

/// Live elements of one kind, for a chip's hover and click.
pub fn keys_of_kind(rows: &[DraftRow], k: RowKind) -> Vec<ElKey> {
    let mut seen = HashSet::new();
    rows.iter()
        .filter(|r| r.kind == k)
        .filter_map(|r| r.key.clone())
        .filter(|key| seen.insert(key.clone()))
        .collect()
}

/// Every live element the proposal would change or delete, once each.
pub fn affected(rows: &[DraftRow]) -> Vec<ElKey> {
    let mut seen = HashSet::new();
    rows.iter()
        .filter_map(|r| r.key.clone())
        .filter(|key| seen.insert(key.clone()))
        .collect()
}

/// What the views outline while proposal cards are open: their affected elements. A decided card
/// (applied, rejected, stale) lights nothing.
pub fn open_keys(cards: &[ProposalCard]) -> Vec<ElKey> {
    let mut seen = HashSet::new();
    cards
        .iter()
        .filter(|c| matches!(c.state, ProposalState::Waiting | ProposalState::Applying))
        .flat_map(|c| affected(&rows(&c.diff)))
        .filter(|key| seen.insert(key.clone()))
        .collect()
}

/// `Wall 55`, `Level Level 1`; the executor's own label when the category is unknown.
pub fn human(cat: &Catalog, r: &DraftRow) -> String {
    if r.cat.is_empty() {
        return r.raw.clone();
    }
    let word = if WORDS.contains(&r.cat.as_str()) {
        cat.t(&format!("ai.proposal.el.{}", r.cat))
    } else {
        cat.t("ai.proposal.el_other")
    };
    match (&r.name, r.id) {
        (Some(n), _) => format!("{word} {n}"),
        (None, Some(id)) => format!("{word} {id}"),
        (None, None) => word,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card(id: u32, diff: J, state: ProposalState) -> ProposalCard {
        ProposalCard {
            id,
            actor: "mcp:agent".into(),
            title: "t".into(),
            ops: 2,
            diff,
            state,
            base_head: 0,
        }
    }

    fn key(t: &str, id: i64) -> ElKey {
        (t.to_string(), id)
    }

    #[test]
    fn rows_read_every_kind_with_live_keys_only_for_existing_elements() {
        let d = json!({
            "created": [{"slot": 1, "ref": "wall:9"}],
            "copied": ["door:12"],
            "changed": ["wall:3", "wall:55/face:left"],
            "deleted": ["window:7", "level:\"Level 2\""],
            "loss": []
        });
        let r = rows(&d);
        assert_eq!(r.len(), 6);
        assert_eq!(counts(&r), Counts { created: 2, changed: 2, deleted: 2 });
        assert_eq!(r[0].key, None, "a created wall is not in the live model yet");
        assert_eq!(r[1].key, None);
        assert_eq!(r[2].key, Some(key("wall", 3)));
        assert_eq!(r[3].key, Some(key("wall", 55)), "a face is its element");
        assert_eq!(r[4].key, Some(key("opening", 7)));
        assert_eq!(r[5].key, None, "levels are not outlined");
        assert_eq!(r[5].name.as_deref(), Some("Level 2"));
        assert_eq!(
            affected(&r),
            vec![key("wall", 3), key("wall", 55), key("opening", 7)]
        );
        assert_eq!(keys_of_kind(&r, RowKind::Deleted), vec![key("opening", 7)]);
    }

    #[test]
    fn an_old_count_only_diff_gives_no_rows_and_zero_counts() {
        let r = rows(&json!({"created": 3, "count": 4}));
        assert!(r.is_empty());
        assert_eq!(counts(&r).total(), 0);
    }

    #[test]
    fn names_are_human_in_both_languages() {
        let r = rows(&json!({"changed": ["wall:55", "mep_device:2", "set:north"],
                             "deleted": ["level:\"Level 1\""]}));
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = Catalog::new(lang);
            let wall = human(&cat, &r[0]);
            assert_eq!(wall, format!("{} 55", cat.t("ai.proposal.el.wall")));
            assert!(!wall.contains("ai.proposal"), "{wall}");
            assert!(!wall.contains(':'), "no raw ref: {wall}");
            assert_eq!(human(&cat, &r[1]), format!("{} 2", cat.t("ai.proposal.el_other")));
            assert_eq!(human(&cat, &r[2]), "set:north", "an unparsed label stays as written");
            assert!(human(&cat, &r[3]).ends_with(" Level 1"));
            for w in WORDS {
                let k = format!("ai.proposal.el.{w}");
                assert_ne!(cat.t(&k), k, "{k} is in the catalog");
            }
        }
    }

    #[test]
    fn open_keys_follow_only_waiting_and_applying_cards() {
        let d = json!({"changed": ["wall:1"], "deleted": ["wall:2", "wall:1"]});
        let open = card(1, d.clone(), ProposalState::Waiting);
        let keys: HashSet<ElKey> = open_keys(&[open.clone()]).into_iter().collect();
        assert_eq!(keys, [key("wall", 1), key("wall", 2)].into_iter().collect());
        assert_eq!(open_keys(&[open]).len(), 2, "each element once");
        for s in [ProposalState::Approved, ProposalState::Rejected, ProposalState::Stale] {
            assert!(open_keys(&[card(2, d.clone(), s)]).is_empty(), "{s:?} lights nothing");
        }
        assert!(open_keys(&[]).is_empty());
    }
}
