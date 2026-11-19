//! The client against the in-process ERP (`ddc_link::mock`), which checks every request and
//! answer against the contract schemas. The live run against a real ERP is card I1.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ddc_link::mock::{self, Holder, MockErp};
use ddc_link::{connect, resume, Action, ClientInfo, Keeper, LinkError, LockState, MemoryTokens, TokenStore};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc-link-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn info() -> ClientInfo {
    ClientInfo::editor("0.0.1-test")
}

fn linked(erp: &MockErp, dir: &std::path::Path) -> ddc_link::Linked {
    let f = dir.join("p.ddclink");
    std::fs::write(&f, erp.link_file("new", None)).unwrap();
    connect(&f, &MemoryTokens::new(), &info()).unwrap().0
}

fn no_contract_errors(erp: &MockErp) {
    let e = erp.state().contract_errors.clone();
    assert!(e.is_empty(), "{e:#?}");
}

#[test]
fn open_link_exchanges_code_keeps_token_in_store_and_deletes_file() {
    let erp = MockErp::start();
    let dir = tmp("open");
    let f = dir.join("Wohnhaus.ddclink");
    let text = erp.link_file("new", None);
    std::fs::write(&f, &text).unwrap();
    let store = MemoryTokens::new();
    let (l, link) = connect(&f, &store, &info()).unwrap();
    assert_eq!(link.action, Action::New);
    assert!(!f.exists(), "the single-use link file is deleted after use");
    let id = format!("ddc-link/{}/{}", erp.url.trim_start_matches("http://"), erp.project_id);
    assert_eq!(store.ids(), vec![id.clone()]);
    assert_eq!(l.store_id(), id);
    let token = store.get(&id).unwrap().unwrap();
    assert!(token.expose().starts_with("ddcl_"));
    assert!(!format!("{l:?}").contains(token.expose()), "Debug of the session never shows the token");
    {
        let s = erp.state();
        let r = &s.requests[0];
        assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/api/v1/design/link/exchange"));
        assert_eq!(r.header("X-DDC-Link"), Some("1"));
        assert!(r.header("Authorization").is_none(), "the exchange carries the code, not a token");
    }
    // the token works, and comes back from the store after a restart
    let ctx = l.client.context(&l.project_id).unwrap();
    assert_eq!(ctx.project.name, "Wohnhaus Beispielstraße");
    let again = resume(&l.erp, &l.project_id, &store).unwrap().expect("token in store");
    again.client.context(&l.project_id).unwrap();
    assert_eq!(erp.state().requests.last().unwrap().header("Authorization").map(|a| a.starts_with("Bearer ddcl_")), Some(true));
    // the same code twice: 410, and the file goes
    std::fs::write(&f, &text).unwrap();
    assert_eq!(connect(&f, &store, &info()).unwrap_err(), LinkError::CodeGone);
    assert!(!f.exists());
    no_contract_errors(&erp);
}

#[test]
fn ai_key_of_the_context_stays_in_memory() {
    const KEY: &str = "sk-MARKER-ai-key-7f3a9c1e5b";
    let erp = MockErp::start();
    erp.set_ai_key(KEY);
    let l = linked(&erp, &tmp("ai"));
    let ctx = l.client.context(&l.project_id).unwrap();
    let ai = ctx.ai.clone().expect("ai block");
    assert_eq!(ai.api_key.as_ref().map(|k| k.expose()), Some(KEY));
    assert_eq!(ai.protocol, "chat-completions");
    assert!(!format!("{ctx:?}").contains("MARKER"), "Debug of the context masks the key");
    assert!(ctx.brief.is_some());
    // nothing the editor sent carries it
    for r in erp.state().requests.iter() {
        assert!(!String::from_utf8_lossy(&r.body).contains("MARKER"));
    }
    no_contract_errors(&erp);
}

#[test]
fn version_426_stops_the_client_without_retry() {
    let erp = MockErp::start();
    erp.state().supported = vec![2];
    let dir = tmp("426");
    let f = dir.join("p.ddclink");
    std::fs::write(&f, erp.link_file("new", None)).unwrap();
    let e = connect(&f, &MemoryTokens::new(), &info()).unwrap_err();
    assert_eq!(e, LinkError::Unsupported { supported: vec![2] });
    assert!(e.is_stop());
    assert!(e.to_string().contains("update the editor"), "{e}");
    assert!(f.exists(), "the code was not used: the file stays");

    // a session that was fine, then the ERP is updated: one 426, then silence
    erp.state().supported = vec![1];
    let l = linked(&erp, &dir);
    erp.state().supported = vec![2];
    assert!(l.client.context(&l.project_id).unwrap_err().is_stop());
    let n = erp.state().requests.len();
    assert!(matches!(l.client.take_lock(&l.project_id), Err(LinkError::Unsupported { .. })));
    assert!(matches!(l.client.clone().heartbeat(&l.project_id), Err(LinkError::Unsupported { .. })), "clones share the stop");
    assert_eq!(erp.state().requests.len(), n, "no request after a 426");
    assert_eq!(l.client.stopped_with(), Some(vec![2]));
}

#[test]
fn lock_taken_held_lost_released() {
    let erp = MockErp::start();
    let l = linked(&erp, &tmp("lock"));
    let p = &l.project_id;
    erp.state().lock = Some(Holder::Other("Anna".into()));
    match l.client.take_lock(p) {
        Err(LinkError::LockHeld { held_by, .. }) => assert_eq!(held_by, "Anna"),
        other => panic!("{other:?}"),
    }
    erp.state().lock = None;
    let lock = l.client.take_lock(p).unwrap();
    assert!(lock.mine);
    assert!(l.client.heartbeat(p).unwrap().mine);
    erp.state().drop_lock = true;
    assert_eq!(l.client.heartbeat(p).unwrap_err(), LinkError::LockLost);
    assert_eq!(erp.state().lock, None, "a heartbeat never re-takes a lost lock");
    let pkg = tmp("lock").join("r.ddcrev");
    std::fs::write(&pkg, mock::package(p, &mock::uuid(1), None, &[])).unwrap();
    assert_eq!(l.client.upload(p, &pkg).unwrap_err(), LinkError::LockLost, "no upload without the lock");
    l.client.take_lock(p).unwrap();
    l.client.release_lock(p).unwrap();
    assert_eq!(erp.state().lock, None);
    erp.state().can_publish = false;
    assert!(matches!(l.client.take_lock(p), Err(LinkError::Forbidden(_))));
    no_contract_errors(&erp);
}

#[test]
fn keeper_beats_and_reports_a_lost_lock() {
    let erp = MockErp::start();
    let l = linked(&erp, &tmp("keeper"));
    let lock = l.client.take_lock(&l.project_id).unwrap();
    let k = Keeper::start(l.client.clone(), &l.project_id, lock.expires_at, Duration::from_millis(30));
    let wait = |f: &dyn Fn() -> bool| {
        let t = Instant::now();
        while !f() && t.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        f()
    };
    assert!(wait(&|| erp.state().heartbeats >= 2));
    assert!(k.is_mine());
    erp.state().drop_lock = true;
    assert!(wait(&|| k.state().0 == LockState::Lost), "{:?}", k.state());
    let beats = erp.state().heartbeats;
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(erp.state().heartbeats, beats, "the keeper stops after a lost lock");
    k.release().unwrap();

    // released by the user: lock gone at the ERP
    l.client.take_lock(&l.project_id).unwrap();
    let k = Keeper::start(l.client.clone(), &l.project_id, String::new(), Duration::from_secs(3600));
    k.release().unwrap();
    assert_eq!(erp.state().lock, None);
}

#[test]
fn upload_lists_downloads_and_guards_the_parent() {
    let erp = MockErp::start();
    let dir = tmp("upload");
    let l = linked(&erp, &dir);
    let (c, p) = (&l.client, l.project_id.as_str());
    c.take_lock(p).unwrap();
    assert!(c.chain_tip(p).unwrap().is_none());
    let r1 = dir.join("r1.ddcrev");
    std::fs::write(&r1, mock::package(p, &mock::uuid(1), None, &[("sheets/Plan_EG.pdf", "sheet", b"%PDF-1.7 plan")])).unwrap();
    let v1 = c.upload(p, &r1).unwrap();
    assert_eq!((v1.number, v1.status.as_str(), v1.parent_revision_id.as_deref()), (1, "shared", None));
    assert_eq!(c.download(&v1.revision_id, "sheets/Plan_EG.pdf").unwrap(), b"%PDF-1.7 plan");
    assert!(matches!(c.download(&v1.revision_id, "../x"), Err(LinkError::Protocol(_))));
    assert!(matches!(c.download(&v1.revision_id, "nope.bin"), Err(LinkError::NotFound(_))));

    // same revision twice: 409
    assert!(matches!(c.upload(p, &r1), Err(LinkError::Conflict(_))));
    // a wrong parent: the ERP names the tip
    let bad = dir.join("bad.ddcrev");
    std::fs::write(&bad, mock::package(p, &mock::uuid(2), None, &[])).unwrap();
    assert_eq!(c.upload(p, &bad).unwrap_err(), LinkError::ParentMoved { latest: Some(v1.revision_id.clone()) });
    // waiting rev1 is the tip; rev2 builds on it
    let tip = c.chain_tip(p).unwrap().unwrap();
    assert_eq!(tip.revision_id, v1.revision_id);
    let r2 = dir.join("r2.ddcrev");
    std::fs::write(&r2, mock::package(p, &mock::uuid(2), Some(&tip.revision_id), &[])).unwrap();
    let v2 = c.upload(p, &r2).unwrap();
    assert_eq!((v2.number, v2.parent_revision_id.as_deref()), (2, Some(v1.revision_id.as_str())));
    let list = c.revisions(p).unwrap();
    assert_eq!(list.iter().map(|r| r.number).collect::<Vec<_>>(), vec![2, 1], "newest first");
    assert_eq!(c.revision(&v2.revision_id).unwrap().manifest["revision_id"], v2.revision_id.as_str());
    // after a rejection the tip is the one before
    erp.reject(&v2.revision_id);
    assert_eq!(c.chain_tip(p).unwrap().unwrap().revision_id, v1.revision_id);
    // a tampered package: 422
    let t = dir.join("t.ddcrev");
    std::fs::write(&t, mock::package_lying(p, &mock::uuid(3), Some(&v1.revision_id), "model.ddc")).unwrap();
    assert!(matches!(c.upload(p, &t), Err(LinkError::Rejected(m)) if m.contains("sha256")));
    // an import record on top is never a parent: the tip stays the design below it
    let imp = erp.wrap_import(p);
    assert_eq!(c.chain_tip(p).unwrap().unwrap().revision_id, v1.revision_id);
    let r4 = dir.join("r4.ddcrev");
    std::fs::write(&r4, mock::package(p, &mock::uuid(4), Some(&imp), &[])).unwrap();
    assert_eq!(c.upload(p, &r4).unwrap_err(), LinkError::ParentMoved { latest: Some(v1.revision_id.clone()) });
    // over a published import there is no parent at all: v1 is archived
    erp.accept(&imp);
    assert!(c.chain_tip(p).unwrap().is_none());
    // a design waiting on a revision that was archived meanwhile is stale and never a parent
    let r5 = dir.join("r5.ddcrev");
    std::fs::write(&r5, mock::package(p, &mock::uuid(5), None, &[])).unwrap();
    let v5 = c.upload(p, &r5).unwrap();
    assert_eq!(c.chain_tip(p).unwrap().unwrap().revision_id, v5.revision_id);
    erp.state().revisions.iter_mut().find(|r| r["revision_id"] == v5.revision_id.as_str()).unwrap()["parent_revision_id"] =
        serde_json::json!(v1.revision_id);
    assert!(c.chain_tip(p).unwrap().is_none());
    no_contract_errors(&erp);
}

#[test]
fn a_dead_token_is_401_and_can_be_forgotten() {
    let erp = MockErp::start();
    let dir = tmp("401");
    let f = dir.join("p.ddclink");
    std::fs::write(&f, erp.link_file("new", None)).unwrap();
    let store = MemoryTokens::new();
    let (l, _) = connect(&f, &store, &info()).unwrap();
    erp.state().tokens.clear();
    assert_eq!(l.client.context(&l.project_id).unwrap_err(), LinkError::Unauthorized);
    l.forget(&store).unwrap();
    assert!(store.ids().is_empty());
    assert!(resume(&l.erp, &l.project_id, &store).unwrap().is_none());
}

#[test]
fn not_an_erp_is_a_protocol_error() {
    // a plain HTTP server that answers without the version header
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            use std::io::{Read, Write};
            let mut b = [0u8; 4096];
            let _ = s.read(&mut b);
            let body = r#"{"token":"x"}"#;
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    let erp = ddc_link::ErpUrl::parse(&format!("http://127.0.0.1:{port}")).unwrap();
    let c = ddc_link::Client::new(&erp).unwrap();
    let code = ddc_link::Secret::new("A".repeat(43));
    assert!(matches!(c.exchange(&code, &info()), Err(LinkError::Protocol(_))));
}
