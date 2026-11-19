// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Shared helpers for the end-to-end tests (`tests/e2e_*.rs` in ddc-app and ddc-io):
//! row dumps and content hashes of every table, integrity assertion, seeded RNG,
//! corpus file lookup for the ignored heavy tests, temp paths.
#![allow(dead_code)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use ddc_model::Model;
use rusqlite::types::Value;

/// Rows of one table as text, in rowid order.
pub type TableRows = (String, Vec<String>);

/// User tables (no `sqlite_*`), sorted by name.
pub fn tables(m: &Model) -> Vec<String> {
    let mut st = m
        .sql()
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .map(|x| x.unwrap())
        .collect()
}

/// Every row of every table except `skip`, sorted: undo restores rows, not rowids.
/// Empty tables are left out: additive tables appear lazily, absent equals empty. `join_cache`
/// is derived and survives undo.
pub fn dump_except(m: &Model, skip: &[&str]) -> Vec<TableRows> {
    let db = m.sql();
    let mut out = vec![];
    for t in tables(m) {
        if skip.contains(&t.as_str()) || t == "join_cache" {
            continue;
        }
        // the stamp of a journal row and the project identity are nonces: replays differ there
        let cols: Vec<String> = db
            .prepare(&format!("SELECT name FROM pragma_table_info('{t}')"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|c| c.unwrap())
            .filter(|c| !(t == "op" && c == "stamp"))
            .map(|c| format!("\"{c}\""))
            .collect();
        let filter = if t == "meta" {
            " WHERE key<>'project_uid'"
        } else {
            ""
        };
        let sql = format!("SELECT {} FROM \"{t}\"{filter}", cols.join(","));
        let mut q = db.prepare(&sql).unwrap();
        let n = q.column_count();
        let mut rows = q.query([]).unwrap();
        let mut v = vec![];
        while let Some(r) = rows.next().unwrap() {
            let cells: Vec<String> = (0..n)
                .map(|i| format!("{:?}", r.get::<_, Value>(i).unwrap()))
                .collect();
            v.push(cells.join("|"));
        }
        if !v.is_empty() {
            v.sort();
            out.push((t, v));
        }
    }
    out
}

/// All tables, the undo journal included.
pub fn dump(m: &Model) -> Vec<TableRows> {
    dump_except(m, &[])
}

/// Data only: the undo journal (`op`) is left out (replays and reopened files differ there).
pub fn dump_data(m: &Model) -> Vec<TableRows> {
    dump_except(m, &["op"])
}

pub fn hash_of(d: &[TableRows]) -> u64 {
    let mut h = DefaultHasher::new();
    d.hash(&mut h);
    h.finish()
}

/// Hash of every row of every table (journal included).
pub fn hash(m: &Model) -> u64 {
    hash_of(&dump(m))
}

/// Hash of every data row (journal excluded).
pub fn data_hash(m: &Model) -> u64 {
    hash_of(&dump_data(m))
}

/// Human-readable difference between two dumps (first rows that differ per table).
pub fn diff(now: &[TableRows], want: &[TableRows]) -> String {
    let mut s = String::new();
    let names: std::collections::BTreeSet<&String> = now.iter().chain(want).map(|t| &t.0).collect();
    for n in names {
        let a = now.iter().find(|t| &t.0 == n).map(|t| &t.1);
        let b = want.iter().find(|t| &t.0 == n).map(|t| &t.1);
        if a != b {
            let (a, b) = (
                a.cloned().unwrap_or_default(),
                b.cloned().unwrap_or_default(),
            );
            let extra: Vec<&String> = a.iter().filter(|r| !b.contains(r)).take(3).collect();
            let missing: Vec<&String> = b.iter().filter(|r| !a.contains(r)).take(3).collect();
            s += &format!(
                "\n[{n}] rows now {} want {}\n  extra: {extra:?}\n  missing: {missing:?}",
                a.len(),
                b.len()
            );
        }
    }
    s
}

/// Panics with a table diff when the two dumps differ.
pub fn assert_same(now: &[TableRows], want: &[TableRows], ctx: &str) {
    if hash_of(now) != hash_of(want) {
        panic!("{ctx}: model differs{}", diff(now, want));
    }
}

/// `check_integrity` must report nothing.
pub fn assert_clean(m: &Model, ctx: &str) {
    let iss = m.check_integrity().unwrap();
    assert!(iss.is_empty(), "{ctx}: integrity issues {iss:?}");
}

/// `SELECT count(*) FROM table` (0 when the table does not exist).
pub fn count(m: &Model, table: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| {
            r.get(0)
        })
        .unwrap_or(0)
}

/// Undo journal length.
pub fn journal_len(m: &Model) -> i64 {
    count(m, "op")
}

/// Corpus sample for a heavy test: env `var` overrides `default`; `None` (with a note)
/// when the file is not on this machine, so the ignored test passes as skipped.
pub fn corpus(var: &str, default: &str) -> Option<PathBuf> {
    let p = std::env::var(var)
        .map(PathBuf::from)
        .unwrap_or_else(|_| default.into());
    if p.is_file() {
        Some(p)
    } else {
        eprintln!(
            "e2e: corpus file {} not found ({var}), skipped",
            p.display()
        );
        None
    }
}

/// Fresh temp file path for this test (removed first if left over).
pub fn temp_file(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("ddc_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    // tests of one process run in parallel: a name alone would be shared between them
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = dir.join(format!("{}_{n}_{name}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

pub fn exists(p: &Path) -> bool {
    p.is_file() && std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false)
}

/// Seeded xorshift; the same seed gives the same sequence on every machine.
pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    pub fn f(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * (self.next() % 10_000) as f64 / 10_000.0
    }
    pub fn pick<T: Copy>(&mut self, v: &[T]) -> Option<T> {
        (!v.is_empty()).then(|| v[self.below(v.len())])
    }
}

/// Number of seeded sequences: `DDC_E2E_SEQS` or `default`.
pub fn seqs(default: u64) -> u64 {
    std::env::var("DDC_E2E_SEQS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Ids of a table, ascending.
pub fn ids(m: &Model, table: &str) -> Vec<i64> {
    let Ok(mut st) = m
        .sql()
        .prepare(&format!("SELECT id FROM \"{table}\" ORDER BY id"))
    else {
        return vec![];
    };
    st.query_map([], |r| r.get(0))
        .unwrap()
        .map(|x| x.unwrap())
        .collect()
}
