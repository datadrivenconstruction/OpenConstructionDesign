// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;

#[test]
fn queued_note_names_the_draft_and_the_wait() {
    let n = queued_note(3);
    assert!(n.contains("#3"));
    assert!(n.contains("pending user Apply"));
}

#[test]
fn an_op_counts_every_kind_of_element() {
    let o = AgentOp {
        item: 1,
        title: "t".into(),
        checked: true,
        conflict: None,
        created: 2,
        changed: 3,
        deleted: 4,
    };
    assert_eq!(o.elements(), 9);
}

#[test]
fn no_items_make_no_rows() {
    assert!(ops_of(&[], &[]).is_empty());
}

#[test]
fn ended_drafts_are_remembered_up_to_the_cap() {
    let mut d = AgentDrafts::default();
    for i in 0..(ENDED_KEPT as u32 + 10) {
        d.end(&format!("mcp:c{i}"), i + 1, "applied");
    }
    assert_eq!(d.ended.len(), ENDED_KEPT);
    assert!(!d.ended.contains_key("mcp:c0"));
}

#[test]
fn a_draft_without_a_number_is_not_remembered() {
    let mut d = AgentDrafts::default();
    d.end("mcp:x", 0, "applied");
    assert!(d.ended.is_empty());
}
