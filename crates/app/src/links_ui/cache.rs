// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Persistent link cache next to the host project: folder `<project>.links/`, one native project
//! file per source content and importer (`<content hash>.<importer>.ddc`): the imported model
//! plus the derived bodies and wall axes, so a reopen reads bodies instead of importing.
//! Only files of this folder are ever written; the linked source is opened read-only. Every
//! file here is regenerable: a damaged one is a miss, and eviction simply deletes.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use web_time::{Duration, SystemTime, UNIX_EPOCH};

use ddc_model::links::Body;
use ddc_model::Model;
use rusqlite::{params, Connection, OpenFlags};
use sha2::{Digest, Sha256};

use super::load::{Elem, LinkWall};

/// Version of the import + derive pipeline written into the cache; bump when its output changes.
pub const IMPORTER: u32 = 1;
/// Default size limit of one cache folder, MB (`DDC_LINK_CACHE_MB` overrides).
pub const LIMIT_MB: u64 = 20 * 1024;
/// Unfinished files older than this are left-overs of a crash.
const TMP_AGE: Duration = Duration::from_secs(3600);

/// Cache folder of a host project file: `house.ddc` → `house.links`.
pub fn dir_for(project: &Path) -> PathBuf {
    project.with_extension("links")
}

/// Size limit of a cache folder, bytes.
pub fn limit() -> u64 {
    std::env::var("DDC_LINK_CACHE_MB")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(LIMIT_MB)
        .saturating_mul(1 << 20)
}

/// Cache key: source content hash and importer identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    /// Hex SHA-256 of the source bytes.
    pub hash: String,
    /// Importer identity: pipeline version, format, application version, .rvt module.
    pub importer: u64,
}

impl Key {
    pub fn file_name(&self) -> String {
        format!("{}.{:016x}.ddc", &self.hash[..32], self.importer)
    }
}

/// Importer identity of a format (and the .rvt module folder, if any).
pub fn importer(format: &str, module: Option<&Path>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (IMPORTER, format, env!("CARGO_PKG_VERSION")).hash(&mut h);
    if let Some(m) = module {
        m.to_string_lossy().hash(&mut h);
    }
    h.finish()
}

fn mtime_ns(m: &std::fs::Metadata) -> u128 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// SHA-256 of a file, read-only.
pub fn hash_file(p: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn stamp_path(dir: &Path, src: &Path) -> PathBuf {
    let mut h = Sha256::new();
    h.update(src.to_string_lossy().as_bytes());
    dir.join(format!("src-{}.stamp", &hex(&h.finalize())[..16]))
}

/// Content hash of `src`: the remembered one while size and modification time match (unless
/// `rehash`), else read anew and remembered in the folder.
pub fn content_hash(dir: &Path, src: &Path, rehash: bool) -> std::io::Result<String> {
    let meta = std::fs::metadata(src)?;
    let (size, mt) = (meta.len(), mtime_ns(&meta));
    let sp = stamp_path(dir, src);
    if !rehash {
        if let Ok(s) = std::fs::read_to_string(&sp) {
            let f: Vec<&str> = s.trim().split('\t').collect();
            if f.len() == 3
                && f[0] == size.to_string()
                && f[1] == mt.to_string()
                && f[2].len() == 64
            {
                return Ok(f[2].to_string());
            }
        }
    }
    let hash = hash_file(src)?;
    if std::fs::create_dir_all(dir).is_ok() {
        let _ = write_atomic(dir, &sp, format!("{size}\t{mt}\t{hash}\n").as_bytes());
    }
    Ok(hash)
}

fn tmp_in(dir: &Path, name: &str) -> PathBuf {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let t = format!("{:?}", std::thread::current().id());
    let t: String = t.chars().filter(char::is_ascii_digit).collect();
    dir.join(format!("{name}.{}-{t}-{n}.tmp", std::process::id()))
}

fn write_atomic(dir: &Path, to: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = to
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into());
    let tmp = tmp_in(dir, &name);
    std::fs::write(&tmp, bytes)?;
    let r = std::fs::rename(&tmp, to);
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

/// A cache file may only be written inside its folder and never over the linked source.
fn writable(dir: &Path, path: &Path, src: &Path) -> bool {
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    path.parent().is_some_and(|p| same(p, dir)) && !same(path, src)
}

/// Kinds of link elements are a small fixed set; cached names map back to static strings.
fn kind_static(s: &str) -> &'static str {
    static KINDS: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let set = KINDS.get_or_init(Default::default);
    let mut set = set.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(k) = set.get(s) {
        return *k;
    }
    let k: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(k);
    k
}

fn f64s(b: &[u8]) -> impl Iterator<Item = f64> + '_ {
    b.chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap_or([0; 8])))
}

fn u32s(b: &[u8]) -> impl Iterator<Item = u32> + '_ {
    b.chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap_or([0; 4])))
}

/// What a hit gives back.
pub struct Cached {
    pub elems: Vec<Elem>,
    pub walls: Vec<LinkWall>,
    pub levels: usize,
}

const DDL: &str = "
CREATE TABLE link_cache_meta(k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE link_cache_elem(seq INTEGER PRIMARY KEY, kind TEXT NOT NULL, id INTEGER NOT NULL,
    guid TEXT NOT NULL, pos BLOB NOT NULL, tri BLOB NOT NULL);
CREATE TABLE link_cache_wall(seq INTEGER PRIMARY KEY, ax REAL, ay REAL, bx REAL, by REAL,
    z0 REAL, z1 REAL, half REAL);
";

fn meta(db: &Connection) -> rusqlite::Result<HashMap<String, String>> {
    let mut st = db.prepare("SELECT k, v FROM link_cache_meta")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

fn read_db(db: &Connection, key: &Key) -> rusqlite::Result<Option<Cached>> {
    let m = meta(db)?;
    let ok = m.get("hash") == Some(&key.hash)
        && m.get("importer") == Some(&format!("{:016x}", key.importer))
        && m.get("complete").map(String::as_str) == Some("1");
    if !ok {
        return Ok(None);
    }
    let levels = m.get("levels").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut st = db.prepare("SELECT kind, id, guid, pos, tri FROM link_cache_elem ORDER BY seq")?;
    let elems = st
        .query_map([], |r| {
            let kind: String = r.get(0)?;
            let pb: Vec<u8> = r.get(3)?;
            let tb: Vec<u8> = r.get(4)?;
            let p: Vec<f64> = f64s(&pb).collect();
            let t: Vec<u32> = u32s(&tb).collect();
            Ok(Elem {
                kind: kind_static(&kind),
                id: r.get(1)?,
                guid: r.get(2)?,
                body: Body {
                    pos: p.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
                    tri: t.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut st =
        db.prepare("SELECT ax, ay, bx, by, z0, z1, half FROM link_cache_wall ORDER BY seq")?;
    let walls = st
        .query_map([], |r| {
            Ok(LinkWall {
                a: (r.get(0)?, r.get(1)?),
                b: (r.get(2)?, r.get(3)?),
                z0: r.get(4)?,
                z1: r.get(5)?,
                half: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let n: i64 = m.get("elems").and_then(|v| v.parse().ok()).unwrap_or(-1);
    if n != elems.len() as i64 {
        return Ok(None);
    }
    Ok(Some(Cached {
        elems,
        walls,
        levels,
    }))
}

/// Reads the cache entry of `key`; a missing, foreign or damaged file is a miss (a damaged
/// one is deleted: it is ours and regenerable). A hit marks the file as recently used.
pub fn read(dir: &Path, key: &Key) -> Option<Cached> {
    let path = dir.join(key.file_name());
    if !path.is_file() {
        return None;
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let got = Connection::open_with_flags(&path, flags)
        .and_then(|db| read_db(&db, key))
        .ok()
        .flatten();
    match &got {
        Some(_) => touch(&path),
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
    got
}

fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::options().append(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

fn blob_f64(b: &Body) -> Vec<u8> {
    b.pos
        .iter()
        .flat_map(|p| p.iter().flat_map(|v| v.to_le_bytes()))
        .collect()
}

fn blob_u32(b: &Body) -> Vec<u8> {
    b.tri
        .iter()
        .flat_map(|t| t.iter().flat_map(|v| v.to_le_bytes()))
        .collect()
}

fn fill(
    db: &mut Connection,
    key: &Key,
    elems: &[Elem],
    walls: &[LinkWall],
    levels: usize,
) -> rusqlite::Result<()> {
    db.execute_batch(DDL)?;
    let tx = db.transaction()?;
    {
        let mut st = tx.prepare(
            "INSERT INTO link_cache_elem(seq, kind, id, guid, pos, tri) VALUES (?1,?2,?3,?4,?5,?6)",
        )?;
        for (i, e) in elems.iter().enumerate() {
            st.execute(params![
                i as i64,
                e.kind,
                e.id,
                e.guid,
                blob_f64(&e.body),
                blob_u32(&e.body)
            ])?;
        }
        let mut st = tx.prepare(
            "INSERT INTO link_cache_wall(seq, ax, ay, bx, by, z0, z1, half) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        )?;
        for (i, w) in walls.iter().enumerate() {
            st.execute(params![
                i as i64, w.a.0, w.a.1, w.b.0, w.b.1, w.z0, w.z1, w.half
            ])?;
        }
        let mut st = tx.prepare("INSERT INTO link_cache_meta(k, v) VALUES (?1, ?2)")?;
        let importer = format!("{:016x}", key.importer);
        let (lv, ne) = (levels.to_string(), elems.len().to_string());
        let rows = [
            ("hash", key.hash.as_str()),
            ("importer", importer.as_str()),
            ("levels", lv.as_str()),
            ("elems", ne.as_str()),
        ];
        for (k, v) in rows {
            st.execute(params![k, v])?;
        }
        // written last: a file without it is unfinished
        st.execute(params!["complete", "1"])?;
    }
    tx.commit()
}

/// Writes the cache entry: the imported model (native project) plus derived bodies, into a
/// temporary file of the folder, then renamed into place. Returns the file name.
pub fn write(
    dir: &Path,
    src: &Path,
    key: &Key,
    model: &Model,
    elems: &[Elem],
    walls: &[LinkWall],
    levels: usize,
) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = key.file_name();
    let path = dir.join(&name);
    if !writable(dir, &path, src) {
        return Err("link cache: refused to write outside its folder".into());
    }
    let tmp = tmp_in(dir, &name);
    let r = (|| -> Result<(), String> {
        let t = tmp.to_string_lossy().into_owned();
        model
            .sql()
            .execute("VACUUM INTO ?1", [t])
            .map_err(|e| e.to_string())?;
        let mut db = Connection::open(&tmp).map_err(|e| e.to_string())?;
        fill(&mut db, key, elems, walls, levels).map_err(|e| e.to_string())?;
        drop(db);
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
        // a parallel load of the same source may have written it meanwhile
        if path.is_file() {
            return Ok(name);
        }
    }
    r.map(|()| name)
}

fn ours(name: &str) -> bool {
    let p: Vec<&str> = name.split('.').collect();
    p.len() == 3
        && p[0].len() == 32
        && p[1].len() == 16
        && p[2] == "ddc"
        && p[..2]
            .iter()
            .all(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Deletes least recently used cache files beyond `limit` bytes (never one named in `keep`),
/// and left-over temporary files. Only regenerable files of the folder are touched.
/// Returns how many cache files were deleted.
pub fn evict(dir: &Path, keep: &[String], limit: u64) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut files = vec![];
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(m) = e.metadata() else { continue };
        if !m.is_file() {
            continue;
        }
        let t = m.modified().unwrap_or(UNIX_EPOCH);
        if name.ends_with(".tmp") {
            if now.duration_since(t).is_ok_and(|d| d > TMP_AGE) {
                let _ = std::fs::remove_file(e.path());
            }
        } else if ours(&name) {
            files.push((t, m.len(), name, e.path()));
        }
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort_by(|a, b| (a.0, &a.2).cmp(&(b.0, &b.2)));
    let mut n = 0;
    for (_, len, name, path) in files {
        if total <= limit {
            break;
        }
        if keep.contains(&name) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
            n += 1;
        }
    }
    n
}
