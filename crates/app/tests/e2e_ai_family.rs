//! A family drafted from the chat: the handler of `family_draft` checks the text, hands it to the
//! family editor as an event and leaves the project untouched; a refused text comes back as a
//! report with excerpts, and the accepted retry reports the rounds it took.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::ai::session::UiEvent;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::json;

const DESK: &str = "family Desk
category furniture
param Width length type 1200mm
type \"Desk\" Width=1200mm
extrude Board rect(0, 0, Width, 600mm) z(0, 40mm)
";

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}

#[test]
fn a_refused_draft_is_repaired_and_reaches_the_editor_untouched_project() {
    let mut ed = editor();
    let hash = live_hash(&ed.model);
    let ops = ed.model.journal_len().unwrap();
    let bad = DESK.replace("z(0, 40mm)", "z(0, Thick)");
    let (mut s, provider) = run(
        &mut ed,
        vec![
            tool("family_draft", json!({ "text": bad })),
            tool("family_draft", json!({ "text": DESK })),
        ],
    );
    let r = tool_results(&provider);
    assert_eq!(r.len(), 2, "{r:?}");
    let (code, detail) = ddc_ai::fake::refusal_of(&r[0].0).expect("a typed refusal");
    assert!(r[0].1 && code == "draft_invalid", "{}", r[0].0);
    assert_eq!(detail["findings"][0]["code"], "unknown_ref");
    assert!(detail["findings"][0]["excerpt"].as_array().is_some_and(|e| !e.is_empty()), "{detail}");
    assert!(!r[1].1, "{}", r[1].0);
    assert!(r[1].0.contains("\"rounds_used\":1"), "{}", r[1].0);
    let drafts: Vec<_> = s
        .drain_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::FamilyDraft { name, repair, .. } => Some((name, repair)),
            _ => None,
        })
        .collect();
    assert_eq!(drafts.len(), 1, "{drafts:?}");
    assert_eq!(drafts[0].0, "Desk");
    assert_eq!(drafts[0].1 .0, 1, "one refused round before it passed");
    assert_eq!(live_hash(&ed.model), hash, "a draft never touches the model");
    assert_eq!(ed.model.journal_len().unwrap(), ops);
    assert!(ed.model.fam_docs().unwrap().is_empty());
}

/// The draft goes through the handler of the chat: the journal and the model stay as they were.
#[test]
fn draft_writes_no_journal() {
    let mut ed = editor();
    let hash = live_hash(&ed.model);
    let ops = ed.model.journal_len().unwrap();
    let (mut s, provider) = run(&mut ed, vec![tool("family_draft", json!({ "text": DESK }))]);
    let r = tool_results(&provider);
    assert!(!r[0].1 && r[0].0.contains("\"status\":\"draft\""), "{}", r[0].0);
    let queued: Vec<bool> = s
        .drain_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::FamilyDraft { queued, .. } => Some(queued),
            _ => None,
        })
        .collect();
    assert_eq!(queued, [false], "the person's own request opens at once");
    assert_eq!(live_hash(&ed.model), hash);
    assert_eq!(ed.model.journal_len().unwrap(), ops);
}
