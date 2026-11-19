// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Empty over full": a model with no content never replaces a file that holds some. A stale
//! path or an emptied model must not wipe a real project; "Save as" to a new name stays allowed.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use ddc_model::Model;

/// Catalog key of the worded reason; `failed` in `save/mod.rs` shows it.
pub const EMPTY_OVER_FULL: &str = "save_guard.save.empty_over_full";

/// Tables whose rows count as content next to the elements. Built elements (walls, slabs...)
/// live in their own tables without an `element` row, so they are listed here too.
const TABLES: &[&str] = &[
    "d_line", "d_polyline", "d_text", "d_insert", "d_arc", "mesh", "solid", "topo_point", "sheet",
    "mass", "group_inst", "mep_run", "room", "ref_plane", "wall", "slab", "roof", "ceiling",
    "beam", "brace", "col", "comp", "curtain_wall", "opening", "railing", "ramp", "stair",
    "skylight", "shaft", "soffit", "iso_footing", "strip_footing", "raft", "truss", "toposolid",
    "topo_pad", "rebar_set", "beam_system",
];

/// IFC entity names that mean "a thing in the building".
const PRODUCTS: &[&str] = &[
    "IFCWALL", "IFCWALLSTANDARDCASE", "IFCSLAB", "IFCBEAM", "IFCCOLUMN", "IFCDOOR", "IFCWINDOW",
    "IFCROOF", "IFCSTAIR", "IFCSTAIRFLIGHT", "IFCRAMP", "IFCRAILING", "IFCMEMBER", "IFCPLATE",
    "IFCFOOTING", "IFCCOVERING", "IFCCURTAINWALL", "IFCFURNISHINGELEMENT",
    "IFCBUILDINGELEMENTPROXY", "IFCSPACE", "IFCFLOWSEGMENT", "IFCFLOWFITTING",
    "IFCFLOWTERMINAL", "IFCGEOGRAPHICELEMENT", "IFCANNOTATION", "IFCOPENINGELEMENT",
];

fn count(m: &Model, sql: &str) -> i64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap_or(0)
}

/// Element count plus rows of the content tables; missing tables count as zero.
pub fn weight(m: &Model) -> i64 {
    let mut w = count(m, "SELECT count(*) FROM element WHERE alive=1");
    for t in TABLES {
        w += count(m, &format!("SELECT count(*) FROM \"{t}\""));
    }
    w
}

fn step_has_product(f: File) -> bool {
    let mut rd = BufReader::new(f);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match rd.read_until(b';', &mut buf) {
            Ok(0) | Err(_) => return false,
            Ok(_) => {}
        }
        let Some(eq) = buf.iter().position(|b| *b == b'=') else {
            continue;
        };
        let rest = &buf[eq + 1..];
        let name: Vec<u8> = rest
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric())
            .map(u8::to_ascii_uppercase)
            .collect();
        if PRODUCTS.iter().any(|p| p.as_bytes() == name.as_slice()) {
            return true;
        }
    }
}

/// Elements plus content rows of one of our databases; `None` when it has no readable element
/// table, which means it is not ours.
fn db_weight(path: &Path) -> Option<i64> {
    let db = rusqlite::Connection::open_with_flags(
        path.to_str()?,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    let mut w = db
        .query_row::<i64, _, _>("SELECT count(*) FROM element WHERE alive=1", [], |r| r.get(0))
        .ok()?;
    for t in TABLES {
        w += db
            .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
            .unwrap_or(0);
    }
    Some(w)
}

/// What a target file holds, as far as an offer to replace it cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holds {
    /// Our own project database with this many elements and content rows.
    Ours(i64),
    /// An IFC file with products.
    Ifc,
    /// Anything else: a database without our element table, or unknown bytes.
    Foreign,
}

impl Holds {
    /// Only files we understand may be moved aside and replaced; the rest can only be kept.
    pub fn can_replace(self) -> bool {
        !matches!(self, Holds::Foreign)
    }
}

fn head_of(path: &Path) -> Option<([u8; 16], usize)> {
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; 16];
    let n = f.read(&mut head).ok()?;
    Some((head, n))
}

/// Classifies the file at `path` for the "Replace anyway" offer.
pub fn holds(path: &Path) -> Holds {
    let Some((head, n)) = head_of(path) else {
        return Holds::Foreign;
    };
    if head[..n].starts_with(b"SQLite format 3") {
        return db_weight(path).map_or(Holds::Foreign, Holds::Ours);
    }
    if head[..n].starts_with(b"ISO-10303-21")
        && File::open(path).is_ok_and(step_has_product)
    {
        return Holds::Ifc;
    }
    Holds::Foreign
}

/// True when the file at `path` holds elements or content rows. Unknown formats with bytes in
/// them count as full: refusing is the safe side. A missing or empty file holds nothing.
pub fn target_holds_content(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() == 0 {
        return false;
    }
    let Some((head, n)) = head_of(path) else {
        return true;
    };
    if head[..n].starts_with(b"SQLite format 3") {
        // no readable element table: not our schema, refusing is the safe side
        return db_weight(path).map_or(true, |w| w > 0);
    }
    if head[..n].starts_with(b"ISO-10303-21") {
        return match File::open(path) {
            Ok(f) => step_has_product(f),
            Err(_) => true,
        };
    }
    true
}

/// `YYYYMMDD-HHMMSS` in UTC for a Unix time.
pub fn stamp_of(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Journal files that belong to a database file and must travel with it.
const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// Moves `target` and its `-wal`/`-shm`/`-journal` files to `_archive/<name>.<stamp>` (plus the
/// same suffix) beside it, never overwriting; returns the new path of the main file. Nothing is
/// deleted, and on error everything moved so far goes back and the target is where it was.
pub fn archive_target(target: &Path, stamp: &str) -> std::io::Result<PathBuf> {
    let name = target
        .file_name()
        .ok_or_else(|| std::io::Error::other("target has no file name"))?
        .to_string_lossy()
        .into_owned();
    let dir = target.parent().unwrap_or(Path::new(".")).join("_archive");
    std::fs::create_dir_all(&dir)?;
    let free = |d: &Path| !d.exists() && SIDECARS.iter().all(|x| !with_suffix(d, x).exists());
    let mut dest = dir.join(format!("{name}.{stamp}"));
    let mut n = 2;
    while !free(&dest) {
        dest = dir.join(format!("{name}.{stamp}-{n}"));
        n += 1;
    }
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut pairs = vec![(target.to_path_buf(), dest.clone())];
    for x in SIDECARS {
        let from = with_suffix(target, x);
        if from.symlink_metadata().is_ok() {
            pairs.push((from, with_suffix(&dest, x)));
        }
    }
    for (from, to) in pairs {
        if let Err(e) = std::fs::rename(&from, &to) {
            for (f, t) in moved.iter().rev() {
                let _ = std::fs::rename(t, f);
            }
            return Err(e);
        }
        moved.push((from, to));
    }
    Ok(dest)
}

/// `Err(key)` when writing `snap` over `target` would replace content with nothing.
pub fn check(snap: &Model, target: &Path) -> Result<(), &'static str> {
    if weight(snap) == 0 && target_holds_content(target) {
        return Err(EMPTY_OVER_FULL);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn stamp_is_utc_and_sortable() {
        assert_eq!(super::stamp_of(0), "19700101-000000");
        assert_eq!(super::stamp_of(1_700_000_000), "20231114-221320");
    }

    #[test]
    fn archive_never_overwrites() {
        let d = std::env::temp_dir().join(format!("ddc-sv14-arch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let t = d.join("a.ifc");
        std::fs::write(&t, b"one").unwrap();
        let p1 = super::archive_target(&t, "S").unwrap();
        std::fs::write(&t, b"two").unwrap();
        let p2 = super::archive_target(&t, "S").unwrap();
        assert_ne!(p1, p2);
        assert_eq!(std::fs::read(&p1).unwrap(), b"one");
        assert_eq!(std::fs::read(&p2).unwrap(), b"two");
        assert!(!t.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn arch_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-sv15-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn archive_moves_wal_and_shm_with_target() {
        let d = arch_dir("wal");
        let live = d.join("live.ddc");
        let t = d.join("p.ddc");
        {
            let db = rusqlite::Connection::open(&live).unwrap();
            db.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
                 CREATE TABLE k(v TEXT); INSERT INTO k VALUES ('kept');",
            )
            .unwrap();
            // snapshot main file + log while open (an open handle blocks rename on some systems,
            // and closing checkpoints the log away): this is the state a crash leaves behind
            let lwal = super::with_suffix(&live, "-wal");
            assert!(std::fs::metadata(&lwal).unwrap().len() > 0);
            std::fs::write(&t, std::fs::read(&live).unwrap()).unwrap();
            std::fs::write(super::with_suffix(&t, "-wal"), std::fs::read(&lwal).unwrap()).unwrap();
        }
        let wal = super::with_suffix(&t, "-wal");
        let moved = super::archive_target(&t, "S").unwrap();
        assert!(!t.exists() && !wal.exists());
        assert!(super::with_suffix(&moved, "-wal").is_file());
        {
            // the log was moved with its name stem, so the archive reopens with the data
            let re = rusqlite::Connection::open(&moved).unwrap();
            let v: String = re.query_row("SELECT v FROM k", [], |r| r.get(0)).unwrap();
            assert_eq!(v, "kept");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn archive_without_sidecars_unchanged() {
        let d = arch_dir("plain");
        let t = d.join("a.ddc");
        std::fs::write(&t, b"x").unwrap();
        let moved = super::archive_target(&t, "S").unwrap();
        assert_eq!(std::fs::read(&moved).unwrap(), b"x");
        let n = std::fs::read_dir(d.join("_archive")).unwrap().count();
        assert_eq!(n, 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn archive_sidecar_failure_restores_all() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = arch_dir("lock");
        let t = d.join("a.ddc");
        std::fs::write(&t, b"main").unwrap();
        let shm = super::with_suffix(&t, "-shm");
        std::fs::write(super::with_suffix(&t, "-wal"), b"wal").unwrap();
        std::fs::write(&shm, b"shm").unwrap();
        // an open handle without sharing makes the rename of this one file fail
        let _lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&shm)
            .unwrap();
        assert!(super::archive_target(&t, "S").is_err());
        assert_eq!(std::fs::read(&t).unwrap(), b"main");
        assert_eq!(std::fs::read(super::with_suffix(&t, "-wal")).unwrap(), b"wal");
        // the unshared handle also blocks reading, so it goes before the last check
        drop(_lock);
        assert_eq!(std::fs::read(&shm).unwrap(), b"shm");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn archive_locked_main_file_refuses() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = arch_dir("lockmain");
        let t = d.join("a.ddc");
        std::fs::write(&t, b"main").unwrap();
        let _lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&t)
            .unwrap();
        assert!(super::archive_target(&t, "S").is_err());
        drop(_lock);
        assert_eq!(std::fs::read(&t).unwrap(), b"main");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn foreign_database_counts_as_full() {
        let p = std::env::temp_dir().join(format!("ddc-ri2-foreign-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        {
            let db = rusqlite::Connection::open(&p).unwrap();
            db.execute_batch("CREATE TABLE notes(t TEXT); INSERT INTO notes VALUES ('x');")
                .unwrap();
        }
        let empty = ddc_model::Model::open(":memory:").unwrap();
        assert_eq!(super::check(&empty, &p), Err(super::EMPTY_OVER_FULL));
        let _ = std::fs::remove_file(&p);
    }
}
