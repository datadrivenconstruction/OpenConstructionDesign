// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The assistant without a worker thread (browser build): `pump_core` steps the worker once a
//! frame, the model sits behind the host page (`ddc-web-bridge/1`) and answers frames later.
//! The same script on the desktop thread gives the same plan.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use std::collections::VecDeque;
use std::sync::Arc;

use ddc_ai::fake::{Script, ScriptedProvider};
use ddc_ai::host_bridge::{HostProvider, Mailbox, BRIDGE};
use ddc_ai::Mode;
use ddc_app::ai::UiSession;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::{json, Value as J};

/// Frames between a request leaving and the host's answer.
const LATE: u32 = 3;

fn posts() -> J {
    json!({"title": "Posts", "cat": "column", "items": [
        {"at": [0.0, 0.0], "top_offset": 3.0},
        {"at": [4.0, 0.0], "top_offset": 3.0},
    ]})
}

fn fresh() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}

#[test]
fn a_stepped_session_with_a_host_three_frames_late_reaches_the_end_of_the_plan() {
    let mut ed = fresh();
    let mb = Arc::new(Mailbox::default());
    let mut c = cfg(ScriptedProvider::new(vec![]));
    c.provider = Arc::new(HostProvider::new(mb.clone()));
    let view = view_of(&ed);
    let mut s = UiSession::start_stepped(&ed, c, view).unwrap();
    assert!(s.wait_idle(&mut ed, WAIT), "the session did not open");
    // the browser build has no files: both copies are held in memory
    for p in [s.base_path().unwrap(), s.work_path().unwrap()] {
        assert!(ddc_model::is_memory_copy(p) && !p.exists(), "{}", p.display());
    }

    let mut replies = VecDeque::from(vec![
        json!({"bridge": BRIDGE, "tool_calls": [{"id": "c1", "name": "create_elements", "args": posts()}]}),
        json!({"bridge": BRIDGE, "text": "done", "finish": "stop"}),
    ]);
    let view = view_of(&ed);
    s.send_turn(&ed, "two posts".to_string(), view, Mode::Propose).unwrap();
    let (mut waiting, mut asked, mut frames) = (Vec::<(u64, u32)>::new(), 0, 0);
    loop {
        frames += 1;
        assert!(frames < 10_000, "the turn never ended");
        s.pump_core(&mut ed);
        for (id, req) in mb.take_outbox() {
            assert_eq!(req["bridge"], BRIDGE);
            asked += 1;
            waiting.push((id, LATE));
        }
        for w in waiting.iter_mut() {
            w.1 -= 1;
        }
        let due: Vec<u64> = waiting.iter().filter(|w| w.1 == 0).map(|w| w.0).collect();
        waiting.retain(|w| w.1 > 0);
        for id in due {
            mb.answer(id, replies.pop_front().expect("one reply per request"));
        }
        if replies.is_empty() && waiting.is_empty() && mb.open() == 0 && !s.is_busy() {
            break;
        }
    }
    assert!(s.wait_idle(&mut ed, WAIT), "the stepped session did not settle");
    assert_eq!(asked, 2, "one request per model step, none repeated while waiting");
    assert!(frames > 2 * LATE as usize, "the answers really came frames later");
    let stepped = plan_of(&s);
    assert!(!stepped.items.is_empty(), "the plan is there");

    // the same script on the desktop worker thread: the same plan
    let mut ed2 = fresh();
    let (t, _) = run(&mut ed2, vec![Script::Tool { name: "create_elements".into(), args: posts() }]);
    let threaded = plan_of(&t);
    let checked = |p: &ddc_app::ai::PlanView| p.items.iter().map(|i| i.checked).collect::<Vec<_>>();
    assert_eq!(checked(&stepped), checked(&threaded));
    assert_eq!(stepped.diff.len(), threaded.diff.len());

    // Apply works the same in the stepped session
    apply_and_check(&mut s, &mut ed);
    drop(s);
}
