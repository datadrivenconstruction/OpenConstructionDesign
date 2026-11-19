// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `Model::to_bytes` / `Model::from_bytes`: the browser build keeps the model in memory and moves
//! it as file bytes. Every table survives the round trip, the loaded model edits and undoes, a WAL
//! file loads, and bytes that are not a model are refused as corrupt.

use ddc_model::{Error, LayerSpec, Model, OpeningSpec, WallSpec};

fn wall(m: &mut Model, level: i64, wt: i64, y: f64) -> i64 {
    m.add_wall(&WallSpec {
        a: (0.0, y),
        b: (6.0, y),
        level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: wt,
        offset0: Some(0.0),
    })
    .unwrap()
}

fn sample() -> (Model, i64, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let level = m.add_level("L1", 0.0).unwrap();
    let lay = LayerSpec {
        material: "Concrete".into(),
        thickness: 0.2,
    };
    let wt = m.add_wall_type("W", &[lay]).unwrap();
    let w = wall(&mut m, level, wt, 0.0);
    m.add_opening(
        w,
        &OpeningSpec {
            along: 3.0,
            width: 1.0,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        },
    )
    .unwrap();
    (m, level, wt)
}

/// (table, row count) for every table, sorted: the whole content at a glance.
fn census(m: &Model) -> Vec<(String, i64)> {
    let db = m.sql();
    let mut st = db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    let names: Vec<String> = st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    names
        .into_iter()
        .map(|t| {
            let n = db
                .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
                .unwrap();
            (t, n)
        })
        .collect()
}

#[test]
fn round_trip_keeps_every_table() {
    let (m, _, _) = sample();
    let bytes = m.to_bytes().unwrap();
    assert!(bytes.starts_with(b"SQLite format 3\0"));
    let back = Model::from_bytes(&bytes).unwrap();
    assert_eq!(census(&back), census(&m));
    assert_eq!(back.schema_version().unwrap(), m.schema_version().unwrap());
    // stable: a second trip gives the same content
    let again = Model::from_bytes(&back.to_bytes().unwrap()).unwrap();
    assert_eq!(census(&again), census(&m));
}

#[test]
fn loaded_model_edits_and_undoes() {
    let (m, level, wt) = sample();
    let before = census(&m);
    let mut back = Model::from_bytes(&m.to_bytes().unwrap()).unwrap();
    wall(&mut back, level, wt, 5.0);
    assert_ne!(census(&back), before, "the edit wrote rows");
    back.undo().unwrap();
    let walls = |m: &Model| census(m).into_iter().find(|(t, _)| t == "wall").map(|(_, n)| n);
    assert_eq!(walls(&m), Some(1));
    assert_eq!(walls(&back), Some(1), "undo removed the new wall");
    // the original is untouched by edits on the copy
    assert_eq!(census(&m), before);
}

#[test]
fn file_model_round_trips() {
    let path = std::env::temp_dir().join(format!("ddc_bytes_{}.ddc", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let p = path.to_str().unwrap();
    let (src, _, _) = sample();
    // the bytes are a desktop file as well, and a file model serializes back to the same content
    std::fs::write(&path, src.to_bytes().unwrap()).unwrap();
    let f = Model::open(p).unwrap();
    let back = Model::from_bytes(&f.to_bytes().unwrap()).unwrap();
    assert_eq!(census(&back), census(&src));
    drop(f);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn wal_header_loads() {
    let (m, _, _) = sample();
    let mut bytes = m.to_bytes().unwrap();
    bytes[18] = 2;
    bytes[19] = 2;
    let back = Model::from_bytes(&bytes).unwrap();
    assert_eq!(census(&back), census(&m));
}

#[test]
fn not_a_model_is_corrupt() {
    for bad in [&b""[..], b"hello", &[0u8; 4096][..]] {
        assert!(matches!(Model::from_bytes(bad), Err(Error::Corrupt(_))), "{} bytes", bad.len());
    }
    // right magic, damaged pages
    let (m, _, _) = sample();
    let mut bytes = m.to_bytes().unwrap();
    let mid = bytes.len() / 2;
    bytes[mid..].fill(0xA5);
    assert!(Model::from_bytes(&bytes).is_err());
}
