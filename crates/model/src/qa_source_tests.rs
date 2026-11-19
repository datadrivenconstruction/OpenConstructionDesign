//! QA: source file bookkeeping (stamps, ignored versions, shared base rows).

use crate::source::file::{
    find_source, note_shared_base, stamp_db, stat_of, FileStamp, SourceFile, Stat,
};
use crate::Model;
use std::path::PathBuf;

fn put(m: &Model, source: &str, key: &str, path: &str, at: i64) {
    m.sql()
        .execute(
            "INSERT INTO src_file(source,file_key,path,format,reader,reader_version,first_import,\
             imported_at) VALUES (?1,?2,?3,'ifc','r','1',?4,?4)",
            rusqlite::params![source, key, path, at],
        )
        .unwrap();
}

fn tmp(name: &str, body: &[u8]) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc-qa-core-{}-{name}", std::process::id()));
    std::fs::write(&d, body).unwrap();
    d
}

#[test]
fn source_rows_stamps_and_versions() {
    let m = Model::open(":memory:").unwrap();
    assert!(m.source_files().unwrap().is_empty());
    put(&m, "ifc:A", "key-a", "C:/a.ifc", 100);
    put(&m, "ifc:B", "key-b", "", 200);
    let all = m.source_files().unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].source, "ifc:B", "newest import first");
    assert!(SourceFile::has_file(&all[1]));
    assert!(!SourceFile::has_file(&all[0]), "no path and no relative path");
    // find by key, then by content
    assert_eq!(find_source(m.sql(), "key-a", "").unwrap().as_deref(), Some("ifc:A"));
    assert_eq!(find_source(m.sql(), "other", "").unwrap(), None);
    let st = FileStamp { hash: "h1".into(), stat: Stat { size: 10, mtime: 5 } };
    stamp_db(m.sql(), "ifc:A", &st, Some("../a.ifc")).unwrap();
    let a = m.source_file("ifc:A").unwrap().unwrap();
    assert_eq!((a.content_hash.as_str(), a.size, a.mtime, a.rel_path.as_str()), ("h1", 10, 5, "../a.ifc"));
    assert_eq!(find_source(m.sql(), "moved", "h1").unwrap().as_deref(), Some("ifc:A"), "by content");
    assert_eq!(find_source(m.sql(), "moved", "nope").unwrap(), None);
    stamp_db(m.sql(), "ifc:A", &st, None).unwrap();
    assert_eq!(m.source_file("ifc:A").unwrap().unwrap().rel_path, "../a.ifc", "None keeps it");
    // stat only
    m.refresh_source_stat("ifc:A", &Stat { size: 11, mtime: 6 }).unwrap();
    let a = m.source_file("ifc:A").unwrap().unwrap();
    assert_eq!((a.size, a.mtime, a.content_hash.as_str()), (11, 6, "h1"));
    // ignore and clear
    m.ignore_source_version("ifc:A", "bad").unwrap();
    assert_eq!(m.source_file("ifc:A").unwrap().unwrap().ignored_hash, "bad");
    m.ignore_source_version("ifc:A", "").unwrap();
    assert_eq!(m.source_file("ifc:A").unwrap().unwrap().ignored_hash, "");
    // our own write
    let w = FileStamp { hash: "w1".into(), stat: Stat { size: 12, mtime: 7 } };
    m.mark_source_written("ifc:A", &w).unwrap();
    let a = m.source_file("ifc:A").unwrap().unwrap();
    assert_eq!((a.written_hash.as_str(), a.size, a.mtime), ("w1", 12, 7));
    assert!(m.note_source_written("ifc:A", "w2").unwrap());
    assert!(!m.note_source_written("ifc:none", "w2").unwrap());
    // an unknown source changes nothing and is not an error
    m.ignore_source_version("ifc:none", "x").unwrap();
    m.refresh_source_stat("ifc:none", &Stat::default()).unwrap();
    assert!(m.source_file("ifc:none").unwrap().is_none());
}

#[test]
fn file_stamp_of_real_files() {
    let p = tmp("stamp", b"hello");
    let st = FileStamp::of(&p).unwrap();
    assert_eq!(st.stat.size, 5);
    assert_eq!(st.hash.len(), 64);
    assert_eq!(stat_of(&p).unwrap().size, 5);
    let again = FileStamp::of_with(&p, &|| false).unwrap();
    assert_eq!(again.hash, st.hash, "deterministic");
    let stopped = FileStamp::of_with(&p, &|| true).unwrap_err();
    assert_eq!(stopped.kind(), std::io::ErrorKind::Interrupted);
    let _ = std::fs::remove_file(&p);
    assert!(FileStamp::of(&p).is_err(), "missing file");
    assert!(stat_of(&p).is_err());
}

#[test]
fn shared_rows_baseline() {
    let m = Model::open(":memory:").unwrap();
    put(&m, "ifc:A", "k", "C:/a.ifc", 1);
    assert_eq!(m.shared_rows_changed("ifc:A").unwrap(), None, "no baseline recorded");
    note_shared_base(m.sql(), "ifc:A").unwrap();
    assert_eq!(m.shared_rows_changed("ifc:A").unwrap(), Some(false));
    assert_eq!(m.shared_rows_changed("ifc:other").unwrap(), None);
    // noting twice is harmless
    note_shared_base(m.sql(), "ifc:A").unwrap();
    assert_eq!(m.shared_rows_changed("ifc:A").unwrap(), Some(false));
}
