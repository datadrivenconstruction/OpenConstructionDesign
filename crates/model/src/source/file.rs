//! Source file memory (schema 39): content hash, stat, relative path, "ignore this version",
//! our own last write, and the `src_target` table of saved exports (written on save).
//!
//! The hash identifies a file: a renamed or moved copy is the same source. A check starts from
//! `stat` (size, mtime) and hashes only when that differs. Nothing here touches the undo journal:
//! these are bookkeeping columns, not model edits.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use web_time::UNIX_EPOCH;

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::{Error, Model};

const NEW_COLUMNS: [&str; 7] = [
    "content_hash TEXT NOT NULL DEFAULT ''",
    "size INTEGER NOT NULL DEFAULT 0",
    "mtime INTEGER NOT NULL DEFAULT 0",
    "rel_path TEXT NOT NULL DEFAULT ''",
    "ignored_hash TEXT NOT NULL DEFAULT ''",
    "written_hash TEXT NOT NULL DEFAULT ''",
    "report TEXT NOT NULL DEFAULT ''",
];

/// Rows elements only point at, with no `src_base` of their own: (table, key column). Rows added
/// after the import have a higher key and do not count: nothing in the source can be stale for them.
const SHARED_ROWS: [(&str, &str); 8] = [
    ("level", "id"),
    ("wall_type", "id"),
    ("wall_type_layer", "wtype"),
    ("slab_type", "id"),
    ("slab_type_layer", "stype"),
    ("member_type", "id"),
    ("comp_type", "id"),
    ("material", "id"),
];

fn shared_exists(db: &Connection, table: &str) -> Result<bool, Error> {
    let n: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Feeds rows with a key up to `limit` into `h`.
fn hash_shared(
    db: &Connection,
    table: &str,
    key: &str,
    limit: i64,
    h: &mut Sha256,
) -> Result<(), Error> {
    // columns added to a table in place (material textures) are not part of what the source said
    let cols: Vec<String> = db
        .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|c| !(table == "material" && c.starts_with("tex_")))
        .map(|c| format!("\"{c}\""))
        .collect();
    let mut st = db.prepare(&format!(
        "SELECT {} FROM {table} WHERE {key}<=?1 ORDER BY {key}, rowid",
        cols.join(",")
    ))?;
    let n = st.column_count();
    let mut rows = st.query([limit])?;
    while let Some(r) = rows.next()? {
        for i in 0..n {
            match r.get_ref(i)? {
                rusqlite::types::ValueRef::Null => h.update([0u8]),
                rusqlite::types::ValueRef::Integer(v) => {
                    h.update([1u8]);
                    h.update(v.to_le_bytes());
                }
                rusqlite::types::ValueRef::Real(v) => {
                    h.update([2u8]);
                    h.update(v.to_le_bytes());
                }
                rusqlite::types::ValueRef::Text(b) | rusqlite::types::ValueRef::Blob(b) => {
                    h.update([3u8]);
                    h.update((b.len() as u64).to_le_bytes());
                    h.update(b);
                }
            }
        }
    }
    Ok(())
}

fn digest_i64(h: Sha256) -> i64 {
    let d = h.finalize();
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[..8]);
    i64::from_le_bytes(b)
}

/// Remembers the shared rows as this import left them.
pub fn note_shared_base(db: &Connection, source: &str) -> Result<(), Error> {
    let mut h = Sha256::new();
    for (table, key) in SHARED_ROWS {
        let mut limit = 0i64;
        if shared_exists(db, table)? {
            limit = db.query_row(
                &format!("SELECT coalesce(max({key}),0) FROM {table}"),
                [],
                |r| r.get(0),
            )?;
            hash_shared(db, table, key, limit, &mut h)?;
        }
        db.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES (?1,?2)",
            params![format!("src_shared_max:{source}:{table}"), limit],
        )?;
    }
    db.execute(
        "INSERT OR REPLACE INTO meta(key,value) VALUES (?1,?2)",
        params![format!("src_shared_hash:{source}"), digest_i64(h)],
    )?;
    Ok(())
}

impl Model {
    /// Whether rows of [`SHARED_ROWS`] differ from the last import of `source`. `None`: that import
    /// left no record, so nobody can tell.
    pub fn shared_rows_changed(&self, source: &str) -> Result<Option<bool>, Error> {
        let meta = |k: String| -> Result<Option<i64>, Error> {
            Ok(self
                .db
                .query_row("SELECT value FROM meta WHERE key=?1", [k], |r| r.get(0))
                .optional()?)
        };
        let Some(base) = meta(format!("src_shared_hash:{source}"))? else {
            return Ok(None);
        };
        let mut h = Sha256::new();
        for (table, key) in SHARED_ROWS {
            let Some(limit) = meta(format!("src_shared_max:{source}:{table}"))? else {
                return Ok(None);
            };
            if shared_exists(&self.db, table)? {
                hash_shared(&self.db, table, key, limit, &mut h)?;
            }
        }
        Ok(Some(digest_i64(h) != base))
    }
}

const TARGET_DDL: &str = "
CREATE TABLE IF NOT EXISTS src_target(id INTEGER PRIMARY KEY, format TEXT NOT NULL UNIQUE,
    source TEXT, path TEXT NOT NULL, rel_path TEXT NOT NULL DEFAULT '',
    over_source INTEGER NOT NULL DEFAULT 0, settings TEXT NOT NULL DEFAULT '{}',
    last_hash TEXT NOT NULL DEFAULT '', last_at INTEGER NOT NULL DEFAULT 0,
    last_journal INTEGER NOT NULL DEFAULT 0, last_report TEXT NOT NULL DEFAULT '{}');
";

use crate::schema::has_col as has_column;

/// Schema 39, additive and idempotent: every column and table is checked by name, so the order
/// against the other steps does not matter. `src_file` must exist (`source::migrate` creates it).
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    for def in NEW_COLUMNS {
        let name = def.split(' ').next().unwrap_or_default();
        if !has_column(db, "src_file", name)? {
            db.execute_batch(&format!("ALTER TABLE src_file ADD COLUMN {def}"))?;
        }
    }
    db.execute_batch(TARGET_DDL)?;
    db.execute_batch("CREATE INDEX IF NOT EXISTS src_file_hash ON src_file(content_hash)")?;
    Ok(())
}

// ---------- stat and hash ----------

/// What the file system says about a file without reading it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stat {
    pub size: i64,
    /// Modification time, unix milliseconds.
    pub mtime: i64,
}

pub fn stat_of(path: &Path) -> std::io::Result<Stat> {
    let md = std::fs::metadata(path)?;
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64);
    Ok(Stat {
        size: md.len() as i64,
        mtime,
    })
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

pub fn hash_bytes(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

/// SHA-256 of a file, streamed. `stop` is polled once per block: true gives `Interrupted`.
pub fn hash_file(path: &Path, stop: &dyn Fn() -> bool) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if stop() {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// Hash and stat of a file as it was when read. The stat is taken first, so a file that changes
/// while it is hashed looks changed at the next check instead of silently matching.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileStamp {
    pub hash: String,
    pub stat: Stat,
}

impl FileStamp {
    pub fn of(path: &Path) -> std::io::Result<Self> {
        Self::of_with(path, &|| false)
    }

    pub fn of_with(path: &Path, stop: &dyn Fn() -> bool) -> std::io::Result<Self> {
        let stat = stat_of(path)?;
        Ok(Self {
            hash: hash_file(path, stop)?,
            stat,
        })
    }
}

// ---------- paths ----------

fn norm(c: Component) -> String {
    let s = c.as_os_str().to_string_lossy().to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// `path` relative to the folder `base` with `/` separators; `None` across drives or when either
/// side is not absolute.
pub fn rel_path(base: &Path, path: &Path) -> Option<String> {
    if !base.is_absolute() || !path.is_absolute() {
        return None;
    }
    let b: Vec<Component> = base.components().collect();
    let p: Vec<Component> = path.components().collect();
    let common = b
        .iter()
        .zip(&p)
        .take_while(|(x, y)| norm(**x) == norm(**y))
        .count();
    if common == 0 {
        return None;
    }
    let mut out: Vec<String> = vec![];
    for _ in common..b.len() {
        out.push("..".into());
    }
    for c in &p[common..] {
        out.push(c.as_os_str().to_string_lossy().to_string());
    }
    Some(out.join("/"))
}

// ---------- rows ----------

/// `src_file` columns that concern the file on disk.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceFile {
    pub source: String,
    pub file_key: String,
    pub path: String,
    pub rel_path: String,
    pub format: String,
    pub imported_at: i64,
    pub content_hash: String,
    pub size: i64,
    pub mtime: i64,
    pub ignored_hash: String,
    pub written_hash: String,
}

impl SourceFile {
    /// Nothing on disk to look at (an import from memory).
    pub fn has_file(&self) -> bool {
        !self.path.is_empty() || !self.rel_path.is_empty()
    }
}

const COLS: &str = "source,file_key,path,rel_path,format,imported_at,content_hash,size,mtime,\
                    ignored_hash,written_hash";

fn row(r: &rusqlite::Row) -> rusqlite::Result<SourceFile> {
    Ok(SourceFile {
        source: r.get(0)?,
        file_key: r.get(1)?,
        path: r.get(2)?,
        rel_path: r.get(3)?,
        format: r.get(4)?,
        imported_at: r.get(5)?,
        content_hash: r.get(6)?,
        size: r.get(7)?,
        mtime: r.get(8)?,
        ignored_hash: r.get(9)?,
        written_hash: r.get(10)?,
    })
}

/// Source namespace of an earlier import of this file: by key, then by content (renamed or moved).
pub fn find_source(db: &Connection, file_key: &str, hash: &str) -> Result<Option<String>, Error> {
    let by_key: Option<String> = db
        .query_row(
            "SELECT source FROM src_file WHERE file_key=?1 ORDER BY imported_at DESC, id DESC LIMIT 1",
            [file_key],
            |r| r.get(0),
        )
        .optional()?;
    if by_key.is_some() || hash.is_empty() {
        return Ok(by_key);
    }
    Ok(db
        .query_row(
            "SELECT source FROM src_file WHERE content_hash=?1 OR written_hash=?1 \
             ORDER BY imported_at DESC, id DESC LIMIT 1",
            [hash],
            |r| r.get(0),
        )
        .optional()?)
}

/// Records the file an import read. Used inside an import batch after `Merge::finish` (the row
/// is already snapshotted there) and by [`Model::stamp_source`].
pub fn stamp_db(
    db: &Connection,
    source: &str,
    s: &FileStamp,
    rel: Option<&str>,
) -> Result<(), Error> {
    db.execute(
        "UPDATE src_file SET content_hash=?2, size=?3, mtime=?4, rel_path=COALESCE(?5, rel_path) \
         WHERE source=?1",
        params![source, s.hash, s.stat.size, s.stat.mtime, rel],
    )?;
    Ok(())
}

/// Where to look for a source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    Stored(PathBuf),
    Relative(PathBuf),
    Beside(PathBuf),
}

impl Found {
    pub fn path(&self) -> &Path {
        match self {
            Found::Stored(p) | Found::Relative(p) | Found::Beside(p) => p,
        }
    }
}

/// Stored path, then the path relative to the project folder, then the same name beside the project.
pub fn locate(f: &SourceFile, project_dir: Option<&Path>) -> Option<Found> {
    if !f.path.is_empty() && Path::new(&f.path).is_file() {
        return Some(Found::Stored(PathBuf::from(&f.path)));
    }
    let dir = project_dir?;
    if !f.rel_path.is_empty() {
        let mut p = dir.to_path_buf();
        for part in f.rel_path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    p.pop();
                }
                x => p.push(x),
            }
        }
        if p.is_file() {
            return Some(Found::Relative(p));
        }
    }
    let name = Path::new(&f.path)
        .file_name()
        .or_else(|| f.rel_path.rsplit('/').next().map(std::ffi::OsStr::new))?;
    let p = dir.join(name);
    p.is_file().then_some(Found::Beside(p))
}

/// The cheap check: does the stat differ from the one recorded (or is there no hash yet)?
pub fn needs_hash(f: &SourceFile, now: &Stat) -> bool {
    f.content_hash.is_empty() || f.size != now.size || f.mtime != now.mtime
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Same content as at the last import or our last write.
    Current,
    /// No hash was recorded (project from before schema 39) and the file is not newer than the
    /// import: take it as the baseline.
    Adopt,
    /// A version the user chose to ignore.
    Ignored,
    Changed,
}

pub fn judge(f: &SourceFile, hash: &str, mtime_ms: i64) -> Verdict {
    if f.content_hash.is_empty() {
        return if mtime_ms / 1000 > f.imported_at + 2 {
            Verdict::Changed
        } else {
            Verdict::Adopt
        };
    }
    if hash == f.content_hash || (!f.written_hash.is_empty() && hash == f.written_hash) {
        Verdict::Current
    } else if !f.ignored_hash.is_empty() && hash == f.ignored_hash {
        Verdict::Ignored
    } else {
        Verdict::Changed
    }
}

impl Model {
    pub fn source_files(&self) -> Result<Vec<SourceFile>, Error> {
        let mut st = self.db.prepare(&format!(
            "SELECT {COLS} FROM src_file ORDER BY imported_at DESC, id DESC"
        ))?;
        let rows = st.query_map([], row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn source_file(&self, source: &str) -> Result<Option<SourceFile>, Error> {
        Ok(self
            .db
            .query_row(
                &format!("SELECT {COLS} FROM src_file WHERE source=?1"),
                [source],
                row,
            )
            .optional()?)
    }

    /// Records hash, stat and (when given) the path relative to the project folder.
    pub fn stamp_source(
        &self,
        source: &str,
        s: &FileStamp,
        rel: Option<&str>,
    ) -> Result<(), Error> {
        stamp_db(&self.db, source, s, rel)
    }

    /// Same content, new stat: no hashing next time.
    pub fn refresh_source_stat(&self, source: &str, s: &Stat) -> Result<(), Error> {
        self.db.execute(
            "UPDATE src_file SET size=?2, mtime=?3 WHERE source=?1",
            params![source, s.size, s.mtime],
        )?;
        Ok(())
    }

    /// Result report of the last import of `source` (JSON of `ddc_io::import_summary`).
    pub fn set_source_report(&self, source: &str, json: &str) -> Result<(), Error> {
        self.db.execute(
            "UPDATE src_file SET report=?2 WHERE source=?1",
            params![source, json],
        )?;
        Ok(())
    }

    /// (source, report) of every source file with a report, newest import first.
    pub fn source_reports(&self) -> Result<Vec<(String, String)>, Error> {
        let mut st = self.db.prepare(
            "SELECT source, report FROM src_file WHERE report NOT IN ('', '{}') ORDER BY imported_at DESC, id DESC",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// "Ignore this version" (empty hash clears it).
    pub fn ignore_source_version(&self, source: &str, hash: &str) -> Result<(), Error> {
        self.db.execute(
            "UPDATE src_file SET ignored_hash=?2 WHERE source=?1",
            params![source, hash],
        )?;
        Ok(())
    }

    /// Our own write over the source: the file with this hash is not somebody else's change.
    pub fn mark_source_written(&self, source: &str, s: &FileStamp) -> Result<(), Error> {
        self.db.execute(
            "UPDATE src_file SET written_hash=?2, size=?3, mtime=?4 WHERE source=?1",
            params![source, s.hash, s.stat.size, s.stat.mtime],
        )?;
        Ok(())
    }

    /// The source file lives somewhere else now: path, key and relative path follow, the
    /// namespace (and so every link and base row) stays.
    pub fn rebind_source(
        &self,
        source: &str,
        path: &str,
        file_key: &str,
        rel: Option<&str>,
    ) -> Result<(), Error> {
        self.db.execute(
            "UPDATE src_file SET path=?2, file_key=?3, rel_path=COALESCE(?4, rel_path) WHERE source=?1",
            params![source, path, file_key, rel],
        )?;
        Ok(())
    }

    pub fn find_source_by_content(
        &self,
        file_key: &str,
        hash: &str,
    ) -> Result<Option<String>, Error> {
        find_source(&self.db, file_key, hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old_db() -> Connection {
        // a project of schema 38: src_file without the new columns
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(super::super::DDL).unwrap();
        db
    }

    fn put(db: &Connection, source: &str, key: &str, path: &str) {
        db.execute(
            "INSERT INTO src_file(source,file_key,path,format,reader,reader_version,first_import,imported_at) \
             VALUES (?1,?2,?3,'ifc','r','1',100,100)",
            params![source, key, path],
        )
        .unwrap();
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-o2-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn migration_from_38_adds_columns_once() {
        let db = old_db();
        put(&db, "ifc:A", "k", "C:/a.ifc");
        migrate(&db).unwrap();
        migrate(&db).unwrap();
        for def in NEW_COLUMNS {
            let c = def.split(' ').next().unwrap();
            assert!(has_column(&db, "src_file", c).unwrap(), "{c}");
        }
        let (h, n): (String, i64) = db
            .query_row("SELECT content_hash, size FROM src_file", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((h.as_str(), n), ("", 0), "old rows get empty defaults");
        let t: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='src_target'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(t, 1);
    }

    #[test]
    fn fresh_project_has_the_step() {
        assert!(crate::schema::STEPS.contains(&39));
        let m = Model::open(":memory:").unwrap();
        assert!(has_column(m.sql(), "src_file", "content_hash").unwrap());
        let v = crate::schema::detect(m.sql()).unwrap();
        assert_eq!(v, crate::schema::VERSION);
        assert!(crate::schema::VERSION >= 39);
    }

    #[test]
    fn report_round_trips() {
        let m = Model::open(":memory:").unwrap();
        put(m.sql(), "ifc:A", "c:/a.ifc", "C:/a.ifc");
        assert!(m.source_reports().unwrap().is_empty());
        m.set_source_report("ifc:A", "{\"n\":1}").unwrap();
        assert_eq!(
            m.source_reports().unwrap(),
            vec![("ifc:A".to_string(), "{\"n\":1}".to_string())]
        );
    }

    #[test]
    fn identity_survives_rename() {
        let m = Model::open(":memory:").unwrap();
        put(m.sql(), "ifc:A", "c:/old/house.ifc", "C:/old/House.ifc");
        let s = FileStamp {
            hash: hash_bytes(b"model"),
            stat: Stat { size: 5, mtime: 7 },
        };
        m.stamp_source("ifc:A", &s, Some("../old/House.ifc"))
            .unwrap();
        // same key: found by key
        assert_eq!(
            m.find_source_by_content("c:/old/house.ifc", "")
                .unwrap()
                .as_deref(),
            Some("ifc:A")
        );
        // renamed and moved: found by content
        let moved = m
            .find_source_by_content("d:/new/renamed.ifc", &s.hash)
            .unwrap();
        assert_eq!(moved.as_deref(), Some("ifc:A"));
        // other content: a new source
        assert_eq!(
            m.find_source_by_content("d:/new/other.ifc", &hash_bytes(b"x"))
                .unwrap(),
            None
        );
        m.rebind_source("ifc:A", "D:/new/renamed.ifc", "d:/new/renamed.ifc", None)
            .unwrap();
        let f = m.source_file("ifc:A").unwrap().unwrap();
        assert_eq!(
            (f.path.as_str(), f.rel_path.as_str()),
            ("D:/new/renamed.ifc", "../old/House.ifc")
        );
        assert_eq!(m.sources().unwrap().len(), 1, "no duplicate source");
    }

    #[test]
    fn equal_stat_is_not_hashed() {
        let mut f = SourceFile {
            content_hash: "h".into(),
            size: 10,
            mtime: 20,
            ..Default::default()
        };
        assert!(!needs_hash(
            &f,
            &Stat {
                size: 10,
                mtime: 20
            }
        ));
        assert!(needs_hash(
            &f,
            &Stat {
                size: 11,
                mtime: 20
            }
        ));
        assert!(needs_hash(
            &f,
            &Stat {
                size: 10,
                mtime: 21
            }
        ));
        f.content_hash.clear();
        assert!(
            needs_hash(
                &f,
                &Stat {
                    size: 10,
                    mtime: 20
                }
            ),
            "no hash yet: one hash"
        );
    }

    #[test]
    fn verdicts() {
        let f = SourceFile {
            content_hash: "a".into(),
            written_hash: "w".into(),
            ignored_hash: "i".into(),
            imported_at: 1000,
            ..Default::default()
        };
        assert_eq!(judge(&f, "a", 0), Verdict::Current);
        assert_eq!(judge(&f, "w", 0), Verdict::Current, "our own write");
        assert_eq!(judge(&f, "i", 0), Verdict::Ignored);
        assert_eq!(judge(&f, "z", 0), Verdict::Changed);
        let old = SourceFile {
            imported_at: 1000,
            ..Default::default()
        };
        assert_eq!(judge(&old, "z", 1_000_000), Verdict::Adopt);
        assert_eq!(judge(&old, "z", 9_000_000), Verdict::Changed);
    }

    #[test]
    fn hash_streams_and_stops() {
        let d = tmp("hash");
        let p = d.join("a.bin");
        std::fs::write(&p, vec![7u8; 3 << 20]).unwrap();
        let a = hash_file(&p, &|| false).unwrap();
        assert_eq!(a, hash_bytes(&vec![7u8; 3 << 20]));
        assert_eq!(
            hash_file(&p, &|| true).unwrap_err().kind(),
            std::io::ErrorKind::Interrupted
        );
        let s = FileStamp::of(&p).unwrap();
        assert_eq!((s.hash, s.stat.size), (a, 3 << 20));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn moved_source_is_located() {
        let d = tmp("loc");
        let proj = d.join("proj");
        let other = d.join("other");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("House.ifc"), b"x").unwrap();
        let rel = rel_path(&proj, &other.join("House.ifc")).unwrap();
        assert_eq!(rel, "../other/House.ifc");
        let mut f = SourceFile {
            path: d
                .join("gone")
                .join("House.ifc")
                .to_string_lossy()
                .to_string(),
            rel_path: rel,
            ..Default::default()
        };
        assert!(matches!(locate(&f, Some(&proj)), Some(Found::Relative(_))));
        f.rel_path.clear();
        assert_eq!(locate(&f, Some(&proj)), None);
        std::fs::write(proj.join("House.ifc"), b"x").unwrap();
        assert!(matches!(locate(&f, Some(&proj)), Some(Found::Beside(_))));
        f.path = other.join("House.ifc").to_string_lossy().to_string();
        assert!(matches!(locate(&f, None), Some(Found::Stored(_))));
        let _ = std::fs::remove_dir_all(&d);
    }
}
