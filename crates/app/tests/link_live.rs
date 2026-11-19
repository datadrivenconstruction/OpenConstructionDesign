// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The editor's linked session against a LIVE ERP (no mock): driven by `tools/e2e/test_70_editor_live.py`
//! in the link repo, which boots the ERP, makes revision 1 and writes the `.ddclink`.
//!
//! `DDC_LINK_LIVE` = the `.ddclink` (action `open` on the chain tip), `DDC_LINK_LIVE_WORK` = an empty work
//! folder. The test writes `result.json` there for the harness to check against the ERP API.
//! `DDC_LINK_LIVE_426` = a `.ddclink` whose ERP answers another link version.

use std::path::PathBuf;

use ddc_app::editor::Editor;
use ddc_app::link::erp::{self, Fail, LockView};
use ddc_link::{store_id, Action, LinkError, LinkFile, MemoryTokens};
use serde_json::json;

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set (run by tools/e2e/test_70_editor_live.py)")))
}

#[test]
#[ignore = "needs a live ERP: tools/e2e/test_70_editor_live.py"]
fn live_open_edit_publish_close() {
    let f = env_path("DDC_LINK_LIVE");
    let base = env_path("DDC_LINK_LIVE_WORK");
    let link = LinkFile::parse(&std::fs::read_to_string(&f).unwrap()).unwrap();
    let store = MemoryTokens::new();

    let o = erp::open_link(&f, &store, &base).expect("open the link");
    assert!(!f.exists(), "the .ddclink is deleted after use");
    assert_eq!(o.action, Action::Open);
    let mut s = o.session;
    assert_eq!(store.ids(), vec![store_id(&link.erp, &link.project_id)], "token in the store");
    assert_eq!(s.lock(), LockView::Mine, "lock taken");
    let ctx_lock = s.linked.client.context(&s.linked.project_id).unwrap().lock.expect("the ERP shows the lock");
    assert!(ctx_lock.mine);

    let tip = s.linked.client.chain_tip(&s.linked.project_id).unwrap().expect("revision 1 exists");
    assert_eq!(link.revision_id.as_deref(), Some(tip.revision_id.as_str()));
    let fetched = o.model.expect("model.ddc of the tip");
    assert!(fetched.is_file() && fetched.starts_with(&s.work));
    assert_eq!(s.revision.as_ref().map(|r| r.revision_id.as_str()), Some(tip.revision_id.as_str()));

    // a real edit through the editor API: one more level on the fetched model
    let edited = s.work.join("model.ddc");
    let mut ed = Editor::new_in_memory().unwrap();
    ed.open_project(&fetched).unwrap();
    ed.try_add_level("Dachterrasse", 99.0).expect("add a level");
    ed.save_project_as(&edited).unwrap();
    drop(ed);

    let mut steps = Vec::new();
    let r = s.publish_job(&edited, Some("Dachterrasse".into())).unwrap().run(&mut |x| steps.push(x.to_string()));
    s.published(&r);
    let r2 = r.expect("publish revision 2");
    assert_eq!(r2.status, "shared", "pending acceptance");
    assert_eq!(r2.parent_revision_id.as_deref(), Some(tip.revision_id.as_str()), "parent = previous tip");
    assert_eq!(r2.number, tip.number + 1);
    assert_eq!(r2.message, "Dachterrasse");
    assert_eq!(steps.first().map(String::as_str), Some("tip"));
    assert_eq!(steps.last().map(String::as_str), Some("upload"));
    assert!(s.work.join("revs").join(format!("{}.ddcrev", r2.revision_id)).is_file());

    let client = s.linked.client.clone();
    let pid = s.linked.project_id.clone();
    s.close().expect("release the lock");
    assert_eq!(s.lock(), LockView::Released);
    assert!(client.context(&pid).unwrap().lock.is_none(), "the ERP has no lock after close");

    let out = json!({"project_id": pid, "parent": tip.revision_id, "parent_number": tip.number,
                     "revision_id": r2.revision_id, "number": r2.number, "status": r2.status});
    std::fs::write(base.join("result.json"), serde_json::to_vec_pretty(&out).unwrap()).unwrap();
}

#[test]
#[ignore = "needs a live ERP: tools/e2e/test_70_editor_live.py"]
fn live_other_link_version_stops_the_editor() {
    let f = env_path("DDC_LINK_LIVE_426");
    let base = env_path("DDC_LINK_LIVE_WORK");
    let store = MemoryTokens::new();
    match erp::open_link(&f, &store, &base) {
        Err(Fail::Link(e @ LinkError::Unsupported { .. })) => {
            let LinkError::Unsupported { supported } = &e else { unreachable!() };
            assert_eq!(supported, &vec![2]);
            assert!(e.to_string().contains("update the editor"), "{e}");
        }
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("opened against an ERP on another link version"),
    }
    assert!(store.ids().is_empty(), "no token kept");
    assert!(f.exists(), "the link file stays: the user updates and opens it again");
}
