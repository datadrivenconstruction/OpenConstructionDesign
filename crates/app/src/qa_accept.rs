// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Headless acceptance run of ONE file: `ddc-app --accept FILE [--accept-out DIR]`.
//!
//! open -> count (native / mesh / skipped) -> probe that every mesh loads -> one real edit
//! (10 mm move of one element) -> save -> drop -> reopen the saved project -> compare.
//! The driver `tools/qa_accept.py` runs this once per file in its own process, with a timeout
//! and a memory limit, and writes the ledger. Progress is flushed to `DIR/result.json` after
//! every step, so a killed run still tells how far it got. The source file is only read.
//!
//! Set `DDC_NO_IMPORT_CACHE=1` (cold import) and `DDC_WORK_DIR` (working files) in the caller.

use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use web_time::{Duration, Instant};

use ddc_model::{Model, Target};
use rusqlite::types::Value;
use rusqlite::Connection;
use serde_json::{json, Map, Value as Json};

use crate::editor::Editor;
use crate::open::build::{self, Built, Done};
use crate::open::summary;

/// Tables whose rows are the model's geometry: compared before save and after reopen.
const GEOM: [&str; 23] = [
    "wall",
    "opening",
    "slab",
    "slab_vertex",
    "curtain_wall",
    "room",
    "col",
    "beam",
    "roof",
    "roof_vertex",
    "ceiling",
    "ceiling_vertex",
    "comp",
    "comp_qty",
    "mep_run",
    "grid",
    "mesh",
    "d_line",
    "d_polyline",
    "d_vertex",
    "d_arc",
    "d_text",
    "d_insert",
];

/// Move applied by the edit step, metres.
const MOVE_M: f64 = 0.010;

struct Rec {
    m: Map<String, Json>,
    out: PathBuf,
}

impl Rec {
    fn set(&mut self, k: &str, v: impl Into<Json>) {
        self.m.insert(k.to_string(), v.into());
        self.flush();
    }

    fn flush(&self) {
        let _ = std::fs::write(&self.out, Json::Object(self.m.clone()).to_string());
    }

    fn verdict(&mut self, v: &str, msg: &str) {
        self.m.insert("verdict".into(), v.into());
        self.m.insert("error".into(), msg.into());
        self.flush();
    }
}

/// `--accept FILE` present: runs it and returns the exit code (0: a result was written).
pub fn handle(argv: &[String]) -> Option<i32> {
    let i = argv.iter().position(|s| s == "--accept")?;
    let file = PathBuf::from(argv.get(i + 1)?);
    let out = argv
        .iter()
        .position(|s| s == "--accept-out")
        .and_then(|j| argv.get(j + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("ddc-accept"));
    Some(run(&file, &out))
}

fn run(file: &Path, out: &Path) -> i32 {
    let _ = std::fs::create_dir_all(out);
    let mut rec = Rec {
        m: Map::new(),
        out: out.join("result.json"),
    };
    rec.set("file", file.to_string_lossy().to_string());
    rec.set("phase", "start");
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        inner(file, out, &mut rec);
    }));
    if r.is_err() {
        let phase = rec
            .m
            .get("phase")
            .and_then(|p| p.as_str())
            .unwrap_or("?")
            .to_string();
        rec.verdict("CRASH", &format!("panic in phase {phase}"));
    }
    rec.set("done", true);
    println!("{}", Json::Object(rec.m.clone()));
    0
}

// ---- fingerprints --------------------------------------------------------------------------

fn has_table(db: &Connection, t: &str) -> bool {
    db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [t],
        |r| r.get::<_, i64>(0),
    )
    .map_or(false, |n| n > 0)
}

fn count(db: &Connection, t: &str) -> i64 {
    db.query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
        .unwrap_or(0)
}

fn hash_value(v: &Value, h: &mut DefaultHasher) {
    match v {
        Value::Null => 0u8.hash(h),
        Value::Integer(i) => {
            1u8.hash(h);
            i.hash(h);
        }
        Value::Real(f) => {
            2u8.hash(h);
            f.to_bits().hash(h);
        }
        Value::Text(s) => {
            3u8.hash(h);
            s.hash(h);
        }
        Value::Blob(b) => {
            4u8.hash(h);
            b.hash(h);
        }
    }
}

/// Hash of every cell the query returns, in order; 0 when the query fails.
fn sig(db: &Connection, sql: &str) -> u64 {
    let mut h = DefaultHasher::new();
    let Ok(mut st) = db.prepare(sql) else {
        return 0;
    };
    let n = st.column_count();
    let Ok(mut rows) = st.query([]) else {
        return 0;
    };
    while let Ok(Some(r)) = rows.next() {
        for i in 0..n {
            if let Ok(v) = r.get::<_, Value>(i) {
                hash_value(&v, &mut h);
            }
        }
        9u8.hash(&mut h);
    }
    h.finish()
}

fn row_sig(db: &Connection, table: &str, id: i64) -> u64 {
    sig(db, &format!("SELECT * FROM \"{table}\" WHERE id={id}"))
}

/// Per geometry table: (row count, hash of all rows).
fn fingerprint(m: &Model) -> BTreeMap<String, (i64, u64)> {
    let db = m.sql();
    let mut out = BTreeMap::new();
    for t in GEOM {
        if !has_table(db, t) {
            continue;
        }
        let n = count(db, t);
        if n > 0 {
            out.insert(
                t.to_string(),
                (n, sig(db, &format!("SELECT * FROM \"{t}\" ORDER BY rowid"))),
            );
        }
    }
    out
}

fn diff(a: &BTreeMap<String, (i64, u64)>, b: &BTreeMap<String, (i64, u64)>) -> Vec<String> {
    let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .map(|k| k.to_string())
        .collect()
}

// ---- open ----------------------------------------------------------------------------------

fn ext_of(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// What opening a drawing read and dropped, and how many recognised proposals were taken.
#[derive(Default)]
struct DrawStats {
    entities: usize,
    dropped: BTreeMap<String, usize>,
    proposals_total: usize,
    proposals_accepted: usize,
}

/// Writes the drawing part of the record: entities, dropped_by_kind, proposals accepted/total.
fn put_drawing_stats(rec: &mut Rec, st: &DrawStats) {
    rec.set("drawing_entities", st.entities);
    rec.set("dropped_by_kind", rows_json(&st.dropped.iter().map(|(k, v)| (k.clone(), *v)).collect::<Vec<_>>()));
    rec.set("dropped_total", st.dropped.values().sum::<usize>());
    rec.set("proposals_total", st.proposals_total);
    rec.set("proposals_accepted", st.proposals_accepted);
}

/// A drawing read into the model: its entities, and the walls, openings, rooms, grids and
/// columns recognised on it, all accepted. Returns the built project and the entity count.
fn build_drawing(path: &Path) -> Result<(Built, DrawStats), String> {
    use ddc_io::drawing_model::{
        accept_proposals, propose, wall_types, AcceptOptions, Memory, ProposeOptions, Underlay,
    };
    let doc = crate::drawing::read_drawing(path)?;
    let shared: Mutex<Option<PathBuf>> = Mutex::new(None);
    let (work, mut model, seeded) = build::fresh(&shared)?;
    let rep = ddc_io::dxf::import_into(&mut model, &doc).map_err(|e| e.to_string())?;
    let entities = rep.lines
        + rep.polylines
        + rep.arcs
        + rep.circles
        + rep.texts
        + rep.inserts
        + rep.hatches;
    let level = build::tidy(&mut model, seeded).ok_or("no level after import")?;
    let (u, _unit) = Underlay::from_dxf_auto(&doc);
    let types = wall_types(&model).unwrap_or_default();
    let props = propose(&u, &types, &ProposeOptions::default(), &Memory::default());
    let ids = props.all_ids();
    let acc = accept_proposals(
        &mut model,
        &props,
        &ids,
        &AcceptOptions::new(level),
        &mut Memory::default(),
    )
    .map_err(|e| e.to_string())?;
    let stats = DrawStats {
        entities,
        dropped: rep.dropped.clone(),
        proposals_total: ids.len(),
        proposals_accepted: acc.walls + acc.openings + acc.rooms + acc.grids + acc.columns,
    };
    Ok((
        Built {
            model,
            work,
            source: path.to_path_buf(),
            level: Some(level),
            done: Done::Ifc,
            cached: false,
        },
        stats,
    ))
}

/// (built project, drawing entity count when the source is a drawing).
fn open_any(path: &Path) -> Result<(Built, Option<DrawStats>), String> {
    let shared: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let c = crate::jobs::Progress::new();
    match ext_of(path).as_str() {
        "ifc" | "ifczip" => Ok((build::build_ifc(path.to_path_buf(), shared, &c)?, None)),
        "rvt" => {
            let br = crate::import::Bridges::load();
            let entries = crate::import::entries(&br.list);
            let Some(dir) = crate::import::entry_for(&entries, path).and_then(|e| e.dir.clone())
            else {
                return Err("no trusted module reads this file".into());
            };
            let b = build::build_rvt(path.to_path_buf(), dir, br.trust, shared, &c)?;
            Ok((b, None))
        }
        "dwg" | "dxf" => {
            let (b, st) = build_drawing(path)?;
            Ok((b, Some(st)))
        }
        e => Err(format!("unsupported extension: {e}")),
    }
}

// ---- display probe -------------------------------------------------------------------------

/// Loads every mesh shape the way the 3D view does: (loaded, total, ms). No frame is drawn.
fn mesh_probe(m: &Model, level: Option<i64>) -> (usize, usize, f64) {
    let t = Instant::now();
    let Ok(mut feed) = crate::bigfile::MeshFeed::of(m, level) else {
        return (0, 0, 0.0);
    };
    let total = feed.total();
    let mut cache = crate::bigfile::MeshCache::new(1 << 30);
    while feed.pending() > 0 {
        match feed.step(m, &mut cache, Duration::from_millis(500), 20_000) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    (feed.loaded.len(), total, t.elapsed().as_secs_f64() * 1000.0)
}

// ---- edit ----------------------------------------------------------------------------------

fn pick_ids(db: &Connection, t: &str) -> Vec<i64> {
    let n = count(db, t);
    if n == 0 {
        return vec![];
    }
    let q = |sql: String| db.query_row(&sql, [], |r| r.get::<_, i64>(0)).ok();
    let mut v: Vec<i64> = vec![];
    // the last row first: a big file must be editable at its far end too
    for id in [
        q(format!("SELECT max(id) FROM \"{t}\"")),
        q(format!(
            "SELECT id FROM \"{t}\" ORDER BY id LIMIT 1 OFFSET {}",
            n / 2
        )),
        q(format!("SELECT min(id) FROM \"{t}\"")),
    ]
    .into_iter()
    .flatten()
    {
        if !v.contains(&id) {
            v.push(id);
        }
    }
    v
}

struct Edited {
    table: &'static str,
    id: i64,
    before: u64,
    after: u64,
    ms: f64,
}

/// Moves one element 10 mm; the first that really changes its stored geometry wins.
fn edit_one(model: &mut Model, errs: &mut Vec<String>) -> Option<Edited> {
    let kinds: [(&'static str, fn(i64) -> Target); 9] = [
        ("wall", Target::Wall),
        ("col", Target::Column),
        ("beam", Target::Beam),
        ("slab", Target::Slab),
        ("curtain_wall", Target::Curtain),
        ("ceiling", Target::Ceiling),
        ("comp", Target::Component),
        ("mep_run", Target::MepRun),
        ("mesh", Target::Mesh),
    ];
    for (table, mk) in kinds {
        if !has_table(model.sql(), table) {
            continue;
        }
        for id in pick_ids(model.sql(), table) {
            let before = row_sig(model.sql(), table, id);
            let t = Instant::now();
            match model.move_elements(&[mk(id)], (MOVE_M, 0.0)) {
                Ok(()) => {
                    let after = row_sig(model.sql(), table, id);
                    if after != before {
                        return Some(Edited {
                            table,
                            id,
                            before,
                            after,
                            ms: t.elapsed().as_secs_f64() * 1000.0,
                        });
                    }
                    errs.push(format!("{table} {id}: move changed no stored geometry"));
                }
                Err(e) => errs.push(format!("{table} {id}: {e}")),
            }
            errs.truncate(8);
        }
    }
    None
}

// ---- the run -------------------------------------------------------------------------------

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn rows_json(rows: &[(String, usize)]) -> Json {
    let mut m = Map::new();
    for (k, n) in rows {
        m.insert(k.clone(), json!(n));
    }
    Json::Object(m)
}

fn inner(file: &Path, out: &Path, rec: &mut Rec) {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    rec.set("size_bytes", std::fs::metadata(file).map_or(0, |m| m.len()));
    rec.set("editable", false);

    // open
    rec.set("phase", "open");
    let t = Instant::now();
    let (built, stats) = match open_any(file) {
        Ok(x) => x,
        Err(e) => {
            let v = if e.starts_with("unsupported") {
                "UNSUPPORTED"
            } else {
                "OPEN_FAIL"
            };
            rec.set("open_ms", ms(t));
            rec.verdict(v, &e);
            return;
        }
    };
    rec.set("open_ms", ms(t));
    let Built {
        model, work, level, ..
    } = built;
    let mut ed = match Editor::start_project() {
        Ok(e) => e,
        Err(e) => {
            rec.verdict("OPEN_FAIL", &format!("editor: {e}"));
            return;
        }
    };
    if let Err(e) = ed.adopt_built(model, work, level) {
        rec.verdict("OPEN_FAIL", &format!("adopt: {e}"));
        return;
    }

    // counts
    rec.set("phase", "count");
    let s = summary::from_model(&ed.model, &name);
    let (native, mesh) = (s.native_total(), s.mesh_total());
    let skipped = summary::stored(&ed.model).map_or(0, |x| x.skipped_total());
    rec.set("native", native);
    rec.set("mesh", mesh);
    rec.set("skipped", skipped);
    rec.set("native_by_kind", rows_json(&s.native));
    rec.set("mesh_by_category", rows_json(&s.mesh));
    // a drawing is shared out by its entities, a model by its elements
    let entities = stats.as_ref().map(|s| s.entities);
    let total = match &stats {
        Some(st) => {
            put_drawing_stats(rec, st);
            st.entities.max(native)
        }
        None => native + mesh,
    };
    rec.set("total", total);
    let share = |n: usize| if total > 0 { n as f64 / total as f64 } else { 0.0 };
    rec.set("native_share", share(native));
    rec.set(
        "share_basis",
        if entities.is_some() {
            "native elements recognised / drawing entities"
        } else {
            "native elements / (native + mesh)"
        },
    );

    // display probe: natives are drawn from their tables, meshes must load their shape
    rec.set("phase", "display");
    let (loaded, feed_total, probe_ms) = mesh_probe(&ed.model, level);
    rec.set("mesh_loaded", loaded);
    rec.set("mesh_feed_total", feed_total);
    rec.set("display_ms", probe_ms);
    let displayed = native + loaded.min(mesh);
    rec.set("displayed", displayed);
    rec.set("displayed_share", share(displayed));
    rec.set(
        "displayed_basis",
        "native counted from tables + meshes whose shape loaded; no frame was rendered",
    );

    // edit
    rec.set("phase", "edit");
    let pre = fingerprint(&ed.model);
    let mut errs = vec![];
    let Some(e) = edit_one(&mut ed.model, &mut errs) else {
        rec.set("edit_errors", errs);
        rec.verdict("EDIT_FAIL", "no element accepted a geometry-changing move");
        return;
    };
    rec.set("edit_ms", e.ms);
    rec.set(
        "edit",
        json!({"table": e.table, "id": e.id, "move_mm": MOVE_M * 1000.0}),
    );
    rec.set(
        "edit_kind",
        if e.table == "mesh" { "mesh" } else { "native" },
    );
    let post = fingerprint(&ed.model);
    let touched = diff(&pre, &post);
    if touched.is_empty() {
        rec.verdict("EDIT_FAIL", "table fingerprints did not change after the edit");
        return;
    }
    rec.set("edit_tables_changed", touched);

    // save
    rec.set("phase", "save");
    let target = crate::project::with_project_ext(out.join("saved"));
    let t = Instant::now();
    if let Err(err) = ed.save_project_as(&target) {
        rec.verdict("SAVE_FAIL", &format!("{err:?}"));
        return;
    }
    rec.set("save_ms", ms(t));
    rec.set(
        "saved_bytes",
        std::fs::metadata(&target).map_or(0, |m| m.len()),
    );
    drop(ed);

    // reopen
    rec.set("phase", "reopen");
    let t = Instant::now();
    let mut ed2 = match Editor::start_project() {
        Ok(e) => e,
        Err(e) => {
            rec.verdict("REOPEN_FAIL", &format!("editor: {e}"));
            return;
        }
    };
    if let Err(err) = ed2.open_project(&target) {
        rec.verdict("REOPEN_FAIL", &format!("{err:?}"));
        return;
    }
    rec.set("reopen_ms", ms(t));

    // compare
    rec.set("phase", "compare");
    let back = fingerprint(&ed2.model);
    let differs = diff(&post, &back);
    let row_back = row_sig(ed2.model.sql(), e.table, e.id);
    let survived = row_back == e.after && row_back != e.before;
    rec.set("reopen_tables_differ", differs.clone());
    rec.set("edit_survived", survived);
    if !differs.is_empty() {
        rec.verdict("REOPEN_DIFF", &format!("tables differ: {}", differs.join(",")));
        return;
    }
    if !survived {
        rec.verdict("REOPEN_DIFF", "edited element did not keep its edited geometry");
        return;
    }
    rec.set("editable", true);
    rec.verdict("PASS", "");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_names_the_tables_that_differ() {
        let mut a = BTreeMap::new();
        a.insert("wall".to_string(), (2, 7u64));
        a.insert("slab".to_string(), (1, 3u64));
        let mut b = a.clone();
        assert!(diff(&a, &b).is_empty());
        b.insert("wall".to_string(), (2, 8u64));
        b.insert("col".to_string(), (1, 1u64));
        assert_eq!(diff(&a, &b), vec!["col".to_string(), "wall".to_string()]);
    }

    #[test]
    fn qa_accept_record_has_dropped_by_kind() {
        let dir = std::env::temp_dir().join(format!("ddc-qa-dropped-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut rec = Rec {
            m: Map::new(),
            out: dir.join("result.json"),
        };
        let mut st = DrawStats {
            entities: 10,
            proposals_total: 5,
            proposals_accepted: 4,
            ..Default::default()
        };
        st.dropped.insert("spline".into(), 3);
        st.dropped.insert("proxy".into(), 1);
        put_drawing_stats(&mut rec, &st);
        let back: Json =
            serde_json::from_str(&std::fs::read_to_string(dir.join("result.json")).unwrap())
                .unwrap();
        assert_eq!(back["dropped_by_kind"]["spline"], 3);
        assert_eq!(back["dropped_by_kind"]["proxy"], 1);
        assert_eq!(back["dropped_total"], 4);
        assert_eq!((back["proposals_accepted"].as_u64(), back["proposals_total"].as_u64()), (Some(4), Some(5)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn row_sig_sees_a_changed_cell() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE wall(id INTEGER PRIMARY KEY, ax REAL); INSERT INTO wall VALUES(1, 0.5);")
            .unwrap();
        let a = row_sig(&db, "wall", 1);
        db.execute_batch("UPDATE wall SET ax = 0.51").unwrap();
        assert_ne!(a, row_sig(&db, "wall", 1));
        assert_eq!(count(&db, "wall"), 1);
    }

    #[test]
    fn unsupported_extension_is_a_recorded_verdict_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("ddc-qa-unsup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.abc");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(run(&f, &dir.join("o")), 0);
        let txt = std::fs::read_to_string(dir.join("o").join("result.json")).unwrap();
        let v: Json = serde_json::from_str(&txt).unwrap();
        assert_eq!(v["verdict"], "UNSUPPORTED");
        assert_eq!(v["editable"], false);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
