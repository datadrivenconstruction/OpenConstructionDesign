//! Files and folders to a flat list: what the open flow works on.
use super::decide::{family, Family, LARGE_BYTES, LARGE_FILES};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq)]
pub struct Expanded {
    pub files: Vec<PathBuf>,
    /// Named files of an unsupported type (never listed from inside a folder).
    pub unsupported: Vec<PathBuf>,
    pub missing: Vec<PathBuf>,
    pub folders: usize,
    pub bytes: u64,
}

impl Expanded {
    pub fn is_large(&self) -> bool {
        self.files.len() > LARGE_FILES || self.bytes > LARGE_BYTES
    }
}

const MAX_DEPTH: usize = 8;
const MAX_FILES: usize = 5000;

fn skip_name(name: &str) -> bool {
    name.starts_with('.')
        || name.starts_with("~$")
        || name == "_ddc_backup"
        || name == "_archive"
        || name.to_ascii_lowercase().contains(".unverified.")
}

fn sorted_entries(dir: &Path) -> Vec<(PathBuf, bool)> {
    let mut v: Vec<(PathBuf, bool)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| !skip_name(&e.file_name().to_string_lossy()))
                .map(|e| {
                    let p = e.path();
                    let d = p.is_dir();
                    (p, d)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|(p, _)| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    v
}

fn walk(dir: &Path, sub: bool, depth: usize, out: &mut Expanded) {
    for (p, is_dir) in sorted_entries(dir) {
        if out.files.len() >= MAX_FILES {
            return;
        }
        if is_dir {
            if sub && depth < MAX_DEPTH {
                walk(&p, sub, depth + 1, out);
            }
        } else if family(&p) != Family::Unsupported {
            out.bytes += std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            out.files.push(p);
        }
    }
}

/// Folders contribute their supported files (top level, or all levels with `sub`).
pub fn expand(inputs: &[PathBuf], sub: bool) -> Expanded {
    let mut out = Expanded::default();
    for p in inputs {
        if p.is_dir() {
            out.folders += 1;
            walk(p, sub, 0, &mut out);
        } else if !p.exists() {
            out.missing.push(p.clone());
        } else if family(p) == Family::Unsupported {
            out.unsupported.push(p.clone());
        } else if !out.files.contains(p) {
            out.bytes += std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            out.files.push(p.clone());
        }
    }
    out
}

/// The level a drawing belongs to by its file name: the longest level name the stem contains.
pub fn level_for_file(levels: &[(i64, String, f64)], file: &Path) -> Option<i64> {
    let stem = file.file_stem()?.to_string_lossy().to_lowercase();
    levels
        .iter()
        .filter(|(_, n, _)| !n.trim().is_empty() && stem.contains(&n.trim().to_lowercase()))
        .max_by_key(|(_, n, _)| n.trim().chars().count())
        .map(|(id, _, _)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc_open_batch_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }

    fn names(e: &Expanded) -> Vec<String> {
        e.files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn folder_top_level_and_subfolders() {
        let d = tmp("folder");
        touch(&d.join("b.ifc"));
        touch(&d.join("a.dxf"));
        touch(&d.join("notes.txt"));
        touch(&d.join("sub").join("c.rvt"));
        touch(&d.join("_ddc_backup").join("old.ifc"));
        let top = expand(&[d.clone()], false);
        assert_eq!(names(&top), vec!["a.dxf", "b.ifc"]);
        assert!(
            top.unsupported.is_empty(),
            "files inside folders are not named"
        );
        assert_eq!(top.folders, 1);
        let all = expand(&[d.clone()], true);
        assert_eq!(names(&all), vec!["a.dxf", "b.ifc", "c.rvt"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn named_files_missing_and_unsupported() {
        let d = tmp("named");
        touch(&d.join("a.ifc"));
        touch(&d.join("b.txt"));
        let e = expand(
            &[
                d.join("a.ifc"),
                d.join("a.ifc"),
                d.join("b.txt"),
                d.join("gone.ddc"),
            ],
            false,
        );
        assert_eq!(e.files.len(), 1, "duplicates collapse");
        assert_eq!(e.unsupported.len(), 1);
        assert_eq!(e.missing.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn large_by_count_and_size() {
        let many = Expanded {
            files: (0..=LARGE_FILES)
                .map(|i| PathBuf::from(format!("{i}.dxf")))
                .collect(),
            ..Default::default()
        };
        assert!(many.is_large());
        let big = Expanded {
            files: vec![PathBuf::from("a.ifc")],
            bytes: LARGE_BYTES + 1,
            ..Default::default()
        };
        assert!(big.is_large());
        assert!(!Expanded::default().is_large());
    }

    #[test]
    fn drawing_level_by_name() {
        let lv = vec![
            (1, "Level 1".to_string(), 0.0),
            (2, "Level 12".to_string(), 3.0),
            (3, String::new(), 6.0),
        ];
        assert_eq!(level_for_file(&lv, Path::new("plan level 12.dxf")), Some(2));
        assert_eq!(level_for_file(&lv, Path::new("plan level 1.dxf")), Some(1));
        assert_eq!(level_for_file(&lv, Path::new("site.dxf")), None);
    }
}
