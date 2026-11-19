use super::*;
use ddc_model::{OptionRec, OptionStatus, RecipeStore};

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc-optkeep-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn rec(name: &str) -> OptionRec {
    OptionRec {
        id: 0,
        set: "generated".into(),
        name: name.into(),
        base_head: 1,
        base_stamp: 2,
        envelope: "{}".into(),
        seed: 7,
        params: "{}".into(),
        generator: None,
        metrics: None,
        note: None,
        why: None,
        status: OptionStatus::Draft,
    }
}

fn store(root: &Path) -> OptionStore {
    OptionStore::open(&root.join("p.db")).unwrap()
}

#[test]
fn recipes_survive_reopen() {
    let root = tmp("reopen");
    let s = store(&root);
    let a = s.add_recipe(&rec("a")).unwrap();
    let b = s.add_recipe(&rec("b")).unwrap();
    drop(s);
    let s = store(&root);
    let names: Vec<_> = s.list(None).unwrap().into_iter().map(|r| (r.id, r.name)).collect();
    assert_eq!(names, vec![(a, "a".to_string()), (b, "b".to_string())]);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn delete_moves_copy_to_archive() {
    let root = tmp("archive");
    let s = store(&root);
    let id = s.add_recipe(&rec("a")).unwrap();
    let dir = options_dir(&root.join("p.db"));
    fs::create_dir_all(&dir).unwrap();
    let copy = dir.join(format!("opt-{id}.db"));
    fs::write(&copy, b"data").unwrap();
    fs::write(with_suffix(&copy, "-wal"), b"wal").unwrap();
    let moved = delete_option(&s, &dir, id).unwrap().expect("archived");
    assert!(!copy.exists() && !with_suffix(&copy, "-wal").exists());
    assert_eq!(moved.parent().unwrap(), root.join("_archive"));
    assert_eq!(fs::read(&moved).unwrap(), b"data");
    assert_eq!(fs::read(with_suffix(&moved, "-wal")).unwrap(), b"wal");
    assert!(s.get(id).unwrap().is_none());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn other_copies_stay_and_missing_recipe_is_refused() {
    let root = tmp("keep");
    let s = store(&root);
    let a = s.add_recipe(&rec("a")).unwrap();
    let b = s.add_recipe(&rec("b")).unwrap();
    let dir = options_dir(&root.join("p.db"));
    fs::create_dir_all(&dir).unwrap();
    for id in [a, b] {
        fs::write(dir.join(format!("opt-{id}.db")), b"x").unwrap();
    }
    delete_option(&s, &dir, a).unwrap();
    assert!(dir.join(format!("opt-{b}.db")).is_file(), "orphan rule: only the deleted one moves");
    assert!(delete_option(&s, &dir, 999).is_err());
    assert!(s.get(b).unwrap().is_some());
    // recipe without a copy: the row goes, nothing is archived
    fs::remove_file(dir.join(format!("opt-{b}.db"))).unwrap();
    assert_eq!(delete_option(&s, &dir, b).unwrap(), None);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn archiving_twice_never_overwrites() {
    let root = tmp("twice");
    let copy = root.join("p.options").join("opt-1.db");
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    fs::write(&copy, b"one").unwrap();
    let first = archive_copy(&copy).unwrap().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(3));
    fs::write(&copy, b"two").unwrap();
    let second = archive_copy(&copy).unwrap().unwrap();
    assert_ne!(first, second);
    assert_eq!(fs::read(first).unwrap(), b"one");
    assert_eq!(fs::read(second).unwrap(), b"two");
    let _ = fs::remove_dir_all(&root);
}

/// An option base whose journal head is `(seq, stamp)`.
fn base_at(root: &Path, tag: &str, seq: i64, stamp: i64) -> Model {
    let path = root.join(format!("base-{tag}.db"));
    let m = Model::open(&path.to_string_lossy()).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("INSERT INTO op(seq, element, kind, stamp) VALUES (?1, 0, 'probe', ?2)", [seq, stamp])
        .unwrap();
    assert_eq!(m.ai_option_head().unwrap(), (seq, stamp));
    m
}

#[test]
fn list_options_draft_stays_draft() {
    let root = tmp("list");
    let s = store(&root);
    let m = base_at(&root, "fresh", 1, 2);
    let mut r = rec("wide");
    r.metrics = Some(r#"{"gfa":1.0,"cost":2.0}"#.into());
    r.why = Some("max floor area".into());
    let a = s.add_recipe(&r).unwrap();
    s.add_recipe(&rec("other")).unwrap();
    let fresh = list_rows(&m, &s, None).unwrap();
    assert_eq!(fresh.len(), 2);
    assert_eq!(fresh[0]["id"], a);
    assert!(fresh.iter().all(|r| r["status"] == "draft"), "no twin in the model: still draft");
    assert_eq!(fresh[0]["stale"], false);
    assert_eq!(fresh[0]["metrics"], json!(["cost", "gfa"]));
    assert_eq!(fresh[0]["why"], "max floor area");
    assert!(list_rows(&m, &s, Some("nope")).unwrap().is_empty());
    drop(m);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn list_options_stale_flag_kept() {
    let root = tmp("stale");
    let s = store(&root);
    s.add_recipe(&rec("a")).unwrap();
    s.add_recipe(&rec("b")).unwrap();
    // the model moved on: same recipes, now stale, status untouched
    let m = base_at(&root, "moved", 9, 2);
    let moved = list_rows(&m, &s, None).unwrap();
    assert_eq!(moved.len(), 2);
    assert!(moved.iter().all(|r| r["stale"] == true && r["status"] == "draft"));
    drop(m);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn list_options_applied_from_tab_is_accepted() {
    let root = tmp("applied");
    let s = store(&root);
    let a = s.add_recipe(&rec("a")).unwrap();
    let b = s.add_recipe(&rec("b")).unwrap();
    // the Options tab Apply of an unsaved project: the recipe is copied into the model and its
    // Apply is a live journal record there; the session store still says draft
    let m = base_at(&root, "tab", 1, 2);
    let twin = m.ai_option_add(&rec("a")).unwrap();
    rusqlite::Connection::open(root.join("base-tab.db"))
        .unwrap()
        .execute("UPDATE ai_option SET accepted_seq=1, accepted_stamp=2 WHERE id=?1", [twin])
        .unwrap();
    assert_eq!(m.ai_option_get(twin).unwrap().unwrap().status, OptionStatus::Accepted);
    assert_eq!(s.get(a).unwrap().unwrap().status, OptionStatus::Draft);
    let rows = list_rows(&m, &s, None).unwrap();
    let status = |id: i64| rows.iter().find(|r| r["id"] == id).unwrap()["status"].clone();
    assert_eq!(status(a), "accepted");
    assert_eq!(status(b), "draft", "only the copied recipe changes");
    drop(m);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn discard_option_keeps_row_rejected() {
    let root = tmp("discard");
    let s = store(&root);
    let id = s.add_recipe(&rec("a")).unwrap();
    let dir = options_dir(&root.join("p.db"));
    fs::create_dir_all(&dir).unwrap();
    let copy = dir.join(format!("opt-{id}.db"));
    fs::write(&copy, b"data").unwrap();
    let moved = discard(&s, Some(&dir), id).unwrap().expect("archived");
    assert!(!copy.exists());
    assert_eq!(fs::read(moved).unwrap(), b"data");
    assert_eq!(s.get(id).unwrap().expect("row kept").status, OptionStatus::Rejected);
    assert!(discard(&s, Some(&dir), id).unwrap_err().contains("already discarded"));
    assert!(discard(&s, Some(&dir), 999).unwrap_err().contains("no option"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn store_reject_only_drafts() {
    let root = tmp("reject");
    let s = store(&root);
    let id = s.add_recipe(&rec("a")).unwrap();
    s.reject(id).unwrap();
    assert!(s.reject(id).is_err(), "a rejected option is not rejected again");
    assert!(s.reject(999).is_err());
    assert_eq!(s.rev().unwrap(), (1, 1));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn tool_and_ui_discard_same_state() {
    let root = tmp("same");
    let project = root.join("p.db");
    let m = ddc_model::Model::open(&project.to_string_lossy()).unwrap();
    let (a, b) = (m.ai_option_add(&rec("a")).unwrap(), m.ai_option_add(&rec("b")).unwrap());
    let dir = options_dir(&project);
    fs::create_dir_all(&dir).unwrap();
    for id in [a, b] {
        fs::write(dir.join(format!("opt-{id}.db")), b"data").unwrap();
    }
    // the tool: its own connection to the project file
    let s = OptionStore::open(&project).unwrap();
    discard(&s, Some(&dir), a).unwrap().expect("archived");
    // the Options tab: the open model
    let mut cache = crate::ai::ui::compare::OptCache::default();
    cache.sync_from(&m, None);
    cache.discard(&m, b, Some(&dir)).unwrap();
    for id in [a, b] {
        assert_eq!(m.ai_option_get(id).unwrap().unwrap().status, OptionStatus::Rejected);
        assert_eq!(s.get(id).unwrap().unwrap().status, OptionStatus::Rejected);
        assert!(!dir.join(format!("opt-{id}.db")).exists());
    }
    assert_eq!(fs::read_dir(archive_dir(&dir)).unwrap().count(), 2);
    cache.sync_from(&m, None);
    assert!(cache.rejected.contains(&a) && cache.rejected.contains(&b));
    drop((s, m));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn discard_accepted_refused() {
    let root = tmp("accepted");
    let s = store(&root);
    let id = s.add_recipe(&rec("a")).unwrap();
    // a store without a journal reads a recorded Apply as accepted
    rusqlite::Connection::open(root.join("p.db"))
        .unwrap()
        .execute("UPDATE ai_option SET accepted_seq=1, accepted_stamp=1 WHERE id=?1", [id])
        .unwrap();
    let dir = options_dir(&root.join("p.db"));
    fs::create_dir_all(&dir).unwrap();
    let copy = dir.join(format!("opt-{id}.db"));
    fs::write(&copy, b"data").unwrap();
    assert_eq!(s.get(id).unwrap().unwrap().status, OptionStatus::Accepted);
    let e = discard(&s, Some(&dir), id).unwrap_err();
    assert_eq!(e, format!("option {id} is accepted: it is already in the model and cannot be discarded"));
    assert!(copy.is_file() && s.get(id).unwrap().is_some());
    let _ = fs::remove_dir_all(&root);
}

/// Store stub holding one recipe in a chosen status (the real store only inserts drafts).
struct OneRecipe(std::cell::RefCell<OptionRec>);

impl RecipeStore for OneRecipe {
    fn add_recipe(&self, _: &OptionRec) -> Result<i64, ddc_model::Error> {
        unreachable!()
    }
    fn get_recipe(&self, id: i64) -> Result<Option<OptionRec>, ddc_model::Error> {
        Ok(Some(self.0.borrow().clone()).filter(|r| r.id == id))
    }
    fn reject_recipe(&self, _: i64) -> Result<(), ddc_model::Error> {
        self.0.borrow_mut().status = OptionStatus::Rejected;
        Ok(())
    }
}

#[test]
fn discard_merged_refused() {
    let mut r = rec("m");
    r.id = 5;
    r.status = OptionStatus::Merged;
    let s = OneRecipe(std::cell::RefCell::new(r));
    assert!(discard(&s, None, 5).unwrap_err().contains("is merged"));
    assert_eq!(s.0.borrow().status, OptionStatus::Merged, "the row is untouched");
    assert_eq!(status_name(OptionStatus::Merged), "merged");
}

#[test]
fn worker_store_is_always_in_session_folder() {
    let root = tmp("locate");
    let session = root.join("session").join("options");
    let (dir, db) = locate(&session);
    assert_eq!(dir, session);
    assert_eq!(db, session.join("recipes.db"));
    // a saved project: the UI reads the session store, never the project file itself
    let project = root.join("p.db");
    let m = ddc_model::Model::open(&project.to_string_lossy()).unwrap();
    let (udir, udb) = ui_store(&m, Some(&session)).expect("session store");
    assert_eq!((udir, udb.clone()), (session.clone(), db));
    assert_ne!(udb, project);
    assert!(ui_store(&m, None).is_none(), "no session: the model's own rows");
    drop(m);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn merged_option_reads_merged_and_is_not_discarded() {
    let root = tmp("merged");
    let project = root.join("p.db");
    let mut m = ddc_model::Model::open(&project.to_string_lossy()).unwrap();
    let id = m.ai_option_add(&rec("a")).unwrap();
    // merged = a live merge entry sealed with the Apply (no stored mark)
    m.atomic_merge("merge_card", id, None, |m| m.add_level("L", 0.0)).unwrap();
    assert_eq!(m.ai_option_get(id).unwrap().unwrap().status, OptionStatus::Merged);
    // the tool's own connection (no journal columns) reads it the same way
    let s = OptionStore::open(&project).unwrap();
    let r = s.get(id).unwrap().unwrap();
    assert_eq!(r.status, OptionStatus::Merged);
    assert_eq!(option_row(&r, (1, 2))["status"], "merged");
    assert!(discard(&s, None, id).is_err(), "a merged option is part of the model");
    assert_eq!(s.get(id).unwrap().unwrap().status, OptionStatus::Merged);
    drop((s, m));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn list_options_merged_from_card_is_merged() {
    let root = tmp("listmerged");
    let s = store(&root);
    let a = s.add_recipe(&rec("a")).unwrap();
    let mut m = base_at(&root, "listmerged", 1, 2);
    let twin = m.ai_option_add(&rec("a")).unwrap();
    m.atomic_merge("merge_card", twin, None, |m| m.add_level("L", 0.0)).unwrap();
    let rows = list_rows(&m, &s, None).unwrap();
    assert_eq!(rows.iter().find(|r| r["id"] == a).unwrap()["status"], "merged");
    drop(m);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn compare_treats_merged_as_applied() {
    let root = tmp("cmpmerged");
    let s = store(&root);
    let a = s.add_recipe(&rec("a")).unwrap();
    let mut m = base_at(&root, "cmpmerged", 1, 2);
    let twin = m.ai_option_add(&rec("a")).unwrap();
    m.atomic_merge("merge_card", twin, None, |m| m.add_level("L", 0.0)).unwrap();
    let mut cache = crate::ai::ui::compare::OptCache::default();
    cache.sync_from(&m, Some(&root.join("p.db")));
    assert!(cache.accepted.contains(&a));
    assert!(!cache.rejected.contains(&a));
    drop(m);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn discard_merged_store_row_refused() {
    let root = tmp("discmerged");
    let s = store(&root);
    let id = s.add_recipe(&rec("a")).unwrap();
    rusqlite::Connection::open(root.join("p.db"))
        .unwrap()
        .execute("INSERT INTO ai_option_merge(option_id, seq, stamp) VALUES (?1, 1, 0)", [id])
        .unwrap();
    assert!(discard(&s, None, id).unwrap_err().contains("merged"));
    assert_eq!(s.get(id).unwrap().unwrap().status, OptionStatus::Merged);
    let _ = fs::remove_dir_all(&root);
}
