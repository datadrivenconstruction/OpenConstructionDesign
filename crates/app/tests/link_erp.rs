// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The editor's linked session against the in-process ERP (`ddc-link` feature `mock-erp`), which
//! answers like the ERP router and checks every request and response against `contract/`.
//! The live ERP run is card I1.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ddc_app::editor::Editor;
use ddc_app::link::erp::{self, Fail, LockView};
use ddc_link::mock::{example_brief, Holder, MockErp};
use ddc_link::{store_id, Action, ErpUrl, LinkError, MemoryTokens};
use serde_json::json;

/// Planted as the ERP user's AI key; must never leave memory.
const MARK: &str = "sk-ddc-marker-7Q2vZ9xK4pL1";

static LOGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Capture;

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        LOGS.lock().unwrap().push(format!("{} {} {}", r.level(), r.target(), r.args()));
    }
    fn flush(&self) {}
}

static CAPTURE: Capture = Capture;

fn capture_logs() {
    let _ = log::set_logger(&CAPTURE);
    log::set_max_level(log::LevelFilter::Trace);
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc-link-app-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn link(erp: &MockErp, dir: &Path, action: &str) -> PathBuf {
    let f = dir.join(format!("{action}.ddclink"));
    std::fs::write(&f, erp.link_file(action, None)).unwrap();
    f
}

fn no_contract_errors(erp: &MockErp) {
    let e = erp.state().contract_errors.clone();
    assert!(e.is_empty(), "{e:#?}");
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            files(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// Every file under `dir`, packages also entry by entry (their entries are compressed).
fn where_found(dir: &Path, needle: &str) -> Vec<String> {
    let mut all = Vec::new();
    files(dir, &mut all);
    let mut hits = Vec::new();
    for p in all {
        let bytes = std::fs::read(&p).unwrap();
        if contains(&bytes, needle) {
            hits.push(p.display().to_string());
        }
        if let Ok(mut z) = zip::ZipArchive::new(std::io::Cursor::new(&bytes)) {
            for i in 0..z.len() {
                let mut f = z.by_index(i).unwrap();
                let mut b = Vec::new();
                f.read_to_end(&mut b).unwrap();
                if contains(&b, needle) {
                    hits.push(format!("{}!{}", p.display(), f.name().unwrap()));
                }
            }
        }
    }
    hits
}

/// The example brief with every value a person has confirmed, as the ERP hands it over
/// after the questions are answered.
fn confirmed_brief() -> serde_json::Value {
    let mut b = example_brief();
    b["provenance"]["/structure/system"] = json!({"status": "confirmed", "sources": [{"kind": "document"}]});
    b
}

#[test]
fn generate_edit_publish_and_the_ai_key_stays_in_memory() {
    capture_logs();
    let base = tmp("flow");
    let erp = MockErp::start();
    erp.set_ai_key(MARK);
    erp.state().brief = Some(confirmed_brief());
    let store = MemoryTokens::new();

    let f = link(&erp, &base, "generate");
    let o = erp::open_link(&f, &store, &base).expect("open the link");
    assert!(!f.exists(), "the .ddclink is deleted after use");
    assert_eq!(o.action, Action::Generate);
    let mut s = o.session;
    let model = o.model.expect("a generated model");
    assert!(model.is_file() && model.starts_with(&s.work));
    assert_eq!(s.ctx.ai.as_ref().and_then(|a| a.api_key.as_ref()).map(|k| k.expose()), Some(MARK));
    assert!(!format!("{:?}", s.ctx).contains(MARK), "the key never prints");
    assert_eq!(s.lock(), LockView::Mine);
    let erp_url = ErpUrl::parse(&erp.url).unwrap();
    assert_eq!(store.ids(), vec![store_id(&erp_url, &erp.project_id)]);
    assert_eq!(erp::work_of(&model).as_deref(), Some(s.work.as_path()));

    // first revision: from the generated model, with its brief
    let mut steps = Vec::new();
    let r1 = s.publish_job(&model, Some("Entwurf".into())).unwrap().run(&mut |x| steps.push(x.to_string()));
    s.published(&r1);
    let r1 = r1.expect("first publish");
    assert_eq!((r1.number, r1.parent_revision_id.as_deref(), r1.status.as_str()), (1, None, "shared"));
    assert_eq!(r1.brief_version, Some(confirmed_brief()["version"].as_i64().unwrap()));
    assert_eq!(steps.first().map(String::as_str), Some("tip"));
    assert_eq!(steps.last().map(String::as_str), Some("upload"));
    assert!(s.work.join("revs").join(format!("{}.ddcrev", r1.revision_id)).is_file());
    assert!(s.generated_from.is_none());

    // an edit, saved where the window saves an untitled ERP model
    let edited = s.work.join("model.ddc");
    let mut ed = Editor::new_in_memory().unwrap();
    ed.open_project(&model).unwrap();
    ed.try_add_level("Dachterrasse", 19.5).unwrap();
    ed.save_project_as(&edited).unwrap();
    drop(ed);
    let r2 = s.publish_job(&edited, Some("Dachterrasse".into())).unwrap().run(&mut |_| {});
    s.published(&r2);
    let r2 = r2.expect("second publish");
    assert_eq!((r2.number, r2.parent_revision_id.as_deref()), (2, Some(r1.revision_id.as_str())));

    // accepted in the ERP, then published from a work folder without our own package of it:
    // the parent's element table comes from the ERP
    erp.accept(&r2.revision_id);
    std::fs::remove_file(s.work.join("revs").join(format!("{}.ddcrev", r2.revision_id))).unwrap();
    let r3 = s.publish_job(&edited, None).unwrap().run(&mut |_| {}).expect("third publish");
    assert_eq!(r3.parent_revision_id.as_deref(), Some(r2.revision_id.as_str()));
    assert!(s.work.join("revs").join(format!("{}.parent.ddcrev", r2.revision_id)).is_file());

    let list = s.linked.client.revisions(&erp.project_id).unwrap();
    let ids: Vec<&str> = list.iter().map(|r| r.revision_id.as_str()).collect();
    assert_eq!(ids, [r3.revision_id.as_str(), r2.revision_id.as_str(), r1.revision_id.as_str()]);
    s.close().unwrap();
    assert_eq!(erp.state().lock, None, "closing gives the lock back");
    assert_eq!(s.lock(), LockView::Released, "the panel no longer names us as the holder");

    // the key and the token: nowhere on disk, in what was sent, in the settings, in the log
    let tokens = erp.state().tokens.clone();
    assert!(!tokens.is_empty());
    for secret in std::iter::once(MARK.to_string()).chain(tokens) {
        assert_eq!(where_found(&base, &secret), Vec::<String>::new(), "{secret} on disk");
        let st = erp.state();
        assert!(!st.uploads.iter().any(|u| contains(u, &secret)), "{secret} uploaded");
        let sent = st.requests.iter().filter(|r| contains(&r.body, &secret)).count();
        assert_eq!(sent, 0, "{secret} in a request body");
        drop(st);
        let prefs = ddc_app::prefs::Prefs { erp_link: true, ..Default::default() };
        assert!(!prefs.text().contains(&secret));
        assert!(!LOGS.lock().unwrap().iter().any(|l| l.contains(&secret)), "{secret} logged");
    }
    no_contract_errors(&erp);
}

#[test]
fn a_lost_lock_stops_publishing_until_taken_again() {
    let base = tmp("lost");
    let erp = MockErp::start();
    let store = MemoryTokens::new();
    let o = erp::open_link(&link(&erp, &base, "new"), &store, &base).unwrap();
    assert!(o.model.is_none());
    let mut s = o.session;
    let m = s.work.join("model.ddc");
    Editor::new_in_memory().unwrap().save_project_as(&m).unwrap();

    // the ERP let the lock expire and someone else took it: the upload is refused with 409
    erp.state().lock = Some(Holder::Other("Bauleitung".into()));
    let r = s.publish_job(&m, None).unwrap().run(&mut |_| {});
    assert!(matches!(r, Err(Fail::Link(LinkError::LockLost))), "{r:?}");
    s.published(&r);
    assert_eq!(s.lock(), LockView::Lost);
    assert!(matches!(s.publish_job(&m, None), Err(Fail::Link(LinkError::LockLost))));
    assert!(erp.state().revisions.is_empty(), "nothing published");
    erp.state().lock = None;
    let out: Vec<_> = std::fs::read_dir(s.work.join("out")).unwrap().collect();
    assert!(out.is_empty(), "a refused package is not left behind");

    // taking it again is the user's step, never automatic
    s.take_lock().unwrap();
    assert_eq!(s.lock(), LockView::Mine);
    let r = s.publish_job(&m, None).unwrap().run(&mut |_| {}).expect("publish after retake");
    assert_eq!(r.number, 1);
    no_contract_errors(&erp);
}

#[test]
fn someone_else_editing_leaves_the_session_read_only() {
    let base = tmp("held");
    let erp = MockErp::start();
    erp.state().lock = Some(Holder::Other("Statik".into()));
    let s = erp::open_link(&link(&erp, &base, "new"), &MemoryTokens::new(), &base).unwrap().session;
    assert_eq!(s.lock(), LockView::HeldBy("Statik".into()));
    assert!(!s.can_publish());
    assert!(s.publish_job(&base.join("x.ddc"), None).is_err());
}

#[test]
fn an_erp_on_a_newer_link_version_stops_the_editor_and_keeps_the_package() {
    let base = tmp("426");
    let erp = MockErp::start();
    let mut s = erp::open_link(&link(&erp, &base, "new"), &MemoryTokens::new(), &base).unwrap().session;
    let m = s.work.join("model.ddc");
    Editor::new_in_memory().unwrap().save_project_as(&m).unwrap();

    // the ERP is upgraded while the package is being built
    let before = erp.state().requests.len();
    let r = s
        .publish_job(&m, None)
        .unwrap()
        .run(&mut |step| {
            if step == "upload" {
                erp.state().supported = vec![2];
            }
        });
    s.published(&r);
    match &r {
        Err(Fail::Link(e @ LinkError::Unsupported { supported })) => {
            assert_eq!(supported, &vec![2]);
            assert!(e.to_string().contains("update the editor"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    let after: Vec<_> = erp.state().requests[before..].iter().map(|q| (q.method.clone(), q.status)).collect();
    assert_eq!(after.iter().filter(|(_, st)| *st == 426).count(), 1, "no retry: {after:?}");
    assert_eq!(after.last().map(|(m, _)| m.as_str()), Some("POST"));
    let kept: Vec<_> = std::fs::read_dir(s.work.join("out")).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(kept.len(), 1, "the package stays for a manual hand-over");
    ddc_app::link::package::verify(&kept[0]).expect("a whole package");
    assert_eq!(s.lock(), LockView::Stopped(vec![2]));

    // nothing else goes out once stopped
    let n = erp.state().requests.len();
    assert!(s.refresh().is_err());
    assert!(s.publish_job(&m, None).is_err());
    assert_eq!(erp.state().requests.len(), n);
}
