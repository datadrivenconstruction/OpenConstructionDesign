//! A model file that is already the source of a project opens that project instead of building
//! a second one: the lookup goes over the recent projects, reads only their `src_file` rows and
//! asks the file system for size and mtime (never the file's bytes).
use ddc_model::source::file::{stat_of, Stat};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// What a project remembers about one source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub file_key: String,
    pub path: String,
    pub size: i64,
    pub mtime: i64,
}

/// Does the stored row describe `file`? Same identity key, or the same name with unchanged
/// size and mtime (a moved or renamed copy of the same bytes keeps those).
pub fn matches(row: &Row, key: &str, file: &Path, stat: Stat) -> bool {
    if !row.file_key.is_empty() && row.file_key == key {
        return true;
    }
    let name = |p: &str| {
        Path::new(p)
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    let mine = file
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    row.size > 0 && row.size == stat.size && row.mtime == stat.mtime && name(&row.path) == mine
}

/// How a project's stored source relates to the file on disk now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fit {
    /// Same bytes as far as size and mtime tell.
    Same,
    /// The file was updated in place after the project was made.
    Changed,
}

/// `None`: the row is not this file. A stored size and mtime that differ from the file's mean
/// the file changed since (an identity key alone cannot tell).
pub fn classify(row: &Row, key: &str, file: &Path, stat: Stat) -> Option<Fit> {
    if !matches(row, key, file, stat) {
        return None;
    }
    let known = row.size > 0;
    if known && (row.size != stat.size || row.mtime != stat.mtime) {
        Some(Fit::Changed)
    } else {
        Some(Fit::Same)
    }
}

/// Best of the per-project verdicts, in the order given (newest first): an unchanged match
/// wins over a changed one, the first of each kind wins.
pub fn best(found: &[(usize, Fit)]) -> Option<(usize, Fit)> {
    found
        .iter()
        .find(|(_, f)| *f == Fit::Same)
        .or_else(|| found.first())
        .copied()
}

fn rows(project: &Path) -> Vec<Row> {
    let Ok(db) = Connection::open_with_flags(project, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return vec![];
    };
    let full = read(&db, "SELECT file_key, path, size, mtime FROM src_file");
    // projects older than schema 39 have no size and mtime
    full.or_else(|_| read(&db, "SELECT file_key, path, 0, 0 FROM src_file"))
        .unwrap_or_default()
}

fn read(db: &Connection, sql: &str) -> rusqlite::Result<Vec<Row>> {
    let mut st = db.prepare(sql)?;
    let it = st.query_map([], |r| {
        Ok(Row {
            file_key: r.get(0)?,
            path: r.get(1)?,
            size: r.get(2)?,
            mtime: r.get(3)?,
        })
    })?;
    it.collect()
}

/// The project (of `projects`, newest first) that has `file` as a source, and whether the file
/// is unchanged since.
pub fn find(file: &Path, projects: &[PathBuf]) -> Option<(PathBuf, Fit)> {
    let stat = stat_of(file).ok()?;
    let key = ddc_io::bridge::file_identity(file);
    let found: Vec<(usize, Fit)> = projects
        .iter()
        .enumerate()
        .filter(|(_, p)| p.is_file())
        .filter_map(|(i, p)| {
            rows(p)
                .iter()
                .filter_map(|r| classify(r, &key, file, stat))
                .min()
                .map(|f| (i, f))
        })
        .collect();
    best(&found).map(|(i, f)| (projects[i].clone(), f))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, path: &str, size: i64, mtime: i64) -> Row {
        Row {
            file_key: key.into(),
            path: path.into(),
            size,
            mtime,
        }
    }

    #[test]
    fn identity_key_matches_wherever_the_file_is() {
        let r = row("c:/m/house.ifc", "C:/m/House.ifc", 0, 0);
        let s = Stat { size: 5, mtime: 6 };
        assert!(matches(
            &r,
            "c:/m/house.ifc",
            Path::new("C:/m/House.ifc"),
            s
        ));
        assert!(!matches(
            &r,
            "d:/x/other.ifc",
            Path::new("D:/x/Other.ifc"),
            s
        ));
    }

    #[test]
    fn moved_copy_matches_by_name_size_and_time() {
        let r = row("c:/old/house.ifc", "C:/old/House.ifc", 100, 200);
        let here = Path::new("D:/new/house.ifc");
        assert!(matches(
            &r,
            "d:/new/house.ifc",
            here,
            Stat {
                size: 100,
                mtime: 200
            }
        ));
        assert!(!matches(
            &r,
            "d:/new/house.ifc",
            here,
            Stat {
                size: 101,
                mtime: 200
            }
        ));
        assert!(!matches(
            &r,
            "d:/new/a.ifc",
            Path::new("D:/new/a.ifc"),
            Stat {
                size: 100,
                mtime: 200
            }
        ));
        // no stat stored: nothing to compare
        let old = row("", "C:/old/House.ifc", 0, 0);
        assert!(!matches(&old, "k", here, Stat { size: 0, mtime: 0 }));
    }

    #[test]
    fn missing_projects_are_skipped() {
        let f = std::env::temp_dir().join(format!("ddc-o3-known-{}.ifc", std::process::id()));
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(find(&f, &[PathBuf::from("no/such/project.ddc")]), None);
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn known_ignores_changed_bytes() {
        let r = row("c:/m/house.rvt", "C:/m/House.rvt", 100, 200);
        let f = Path::new("C:/m/House.rvt");
        let at = |size, mtime| classify(&r, "c:/m/house.rvt", f, Stat { size, mtime });
        assert_eq!(at(100, 200), Some(Fit::Same));
        assert_eq!(at(101, 200), Some(Fit::Changed));
        assert_eq!(at(100, 201), Some(Fit::Changed));
        // an old project stored no size: the key alone decides, nothing to compare
        let old = row("c:/m/house.rvt", "C:/m/House.rvt", 0, 0);
        assert_eq!(
            classify(&old, "c:/m/house.rvt", f, Stat { size: 5, mtime: 6 }),
            Some(Fit::Same)
        );
        assert_eq!(classify(&r, "x", Path::new("D:/o.rvt"), Stat { size: 1, mtime: 1 }), None);
    }

    #[test]
    fn unchanged_match_beats_a_changed_one() {
        assert_eq!(best(&[]), None);
        assert_eq!(best(&[(0, Fit::Changed)]), Some((0, Fit::Changed)));
        assert_eq!(
            best(&[(0, Fit::Changed), (1, Fit::Same), (2, Fit::Same)]),
            Some((1, Fit::Same))
        );
    }
}
