// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Corpus list for the big-file benchmark: rows of the corpus list file
//! (`| # | KB | why | `path` |`), the biggest files per format first.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub kb: u64,
    pub ext: String,
    pub path: PathBuf,
}

/// Table rows with a size and a back-quoted path.
pub fn parse(md: &str) -> Vec<Entry> {
    md.lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .filter_map(|l| {
            let cells: Vec<&str> = l
                .trim()
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect();
            let kb = cells
                .get(1)?
                .replace([' ', '\u{a0}'], "")
                .parse::<u64>()
                .ok()?;
            let raw = cells
                .iter()
                .rev()
                .find(|c| c.starts_with('`') && c.ends_with('`') && c.len() > 2)?;
            let path = PathBuf::from(raw.trim_matches('`'));
            let ext = path.extension()?.to_str()?.to_ascii_lowercase();
            Some(Entry { kb, ext, path })
        })
        .collect()
}

/// `DDC_CORPUS`, else a local `corpus.md` next to this crate; `None` makes the benchmark skip.
pub fn corpus_file() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("DDC_CORPUS") {
        return Some(PathBuf::from(p));
    }
    Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus.md")).filter(|p| p.is_file())
}

/// The `n` biggest existing files of `ext` (`DDC_BIG_FILE` overrides with one file).
pub fn biggest(all: &[Entry], ext: &str, n: usize) -> Vec<Entry> {
    if let Ok(p) = std::env::var("DDC_BIG_FILE") {
        let path = PathBuf::from(p);
        let e = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if e != ext || !path.is_file() {
            return vec![];
        }
        let kb = std::fs::metadata(&path)
            .map(|m| m.len() / 1024)
            .unwrap_or(0);
        return vec![Entry { kb, ext: e, path }];
    }
    let mut v: Vec<Entry> = all
        .iter()
        .filter(|e| e.ext == ext && e.path.is_file())
        .cloned()
        .collect();
    v.sort_by(|a, b| b.kb.cmp(&a.kb));
    v.truncate(n);
    v
}

/// One JSON line for the ledger (no escaping beyond quotes and backslashes in the name).
pub fn ledger_line(name: &str, fields: &[(&str, f64)]) -> String {
    let n = name.replace('\\', "/").replace('"', "'");
    let f: Vec<String> = fields.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
    format!("{{\"file\":\"{n}\",{}}}", f.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_parse_and_sort_by_size() {
        let md = "# x\n| # | \u{041a}\u{0411} | \u{041f}\u{043e}\u{0447}\u{0435}\u{043c}\u{0443} | \u{041f}\u{0443}\u{0442}\u{044c} |\n|---|---:|---|---|\n\
                  | 1 | 532 | a; b | `C:/x/a.rvt` |\n| 2 | 12 345 | big | `C:/y/b.IFC` |\n| bad | row |\n";
        let v = parse(md);
        assert_eq!(v.len(), 2);
        assert_eq!(
            v[0],
            Entry {
                kb: 532,
                ext: "rvt".into(),
                path: "C:/x/a.rvt".into()
            }
        );
        assert_eq!(v[1].kb, 12345);
        assert_eq!(v[1].ext, "ifc");
        assert!(ledger_line(r"a\b.ifc", &[("s", 1.5)]).contains("\"file\":\"a/b.ifc\",\"s\":1.5"));
    }
}
