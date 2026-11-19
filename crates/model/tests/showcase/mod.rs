//! Showcase projects: reproducible models built in code through the public model API, shared by
//! the scenario tests of ddc-model, ddc-io and ddc-app (`#[path]` include, see README.md here).
//! Also the model check every project must pass: `validate` = integrity + per-kind validity.
#![allow(dead_code)]

mod edge;
mod office;
mod residential;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use ddc_model::{material_defaults, LayerSpec, Model, OpeningSpec, WallSpec};
use rusqlite::types::Value;

pub use edge::huge_level_n;

/// Builds a project into a fresh model; panics (file:line) on any refused call.
pub type Build = fn(&mut Model);

pub struct Project {
    pub name: &'static str,
    pub about: &'static str,
    pub build: Build,
}

/// Every showcase project. Names are stable: tests and docs refer to them.
pub const PROJECTS: &[Project] = &[
    Project {
        name: "office",
        about: "3 levels, grid, layered walls, doors, windows, rooms, stairs, MEP, sheets",
        build: office::build,
    },
    Project {
        name: "residential",
        about: "2 levels, curtain wall, families, phases, Cyrillic and long names",
        build: residential::build,
    },
    Project {
        name: "edge_empty",
        about: "nothing but a fresh model",
        build: edge::empty,
    },
    Project {
        name: "edge_huge_level",
        about: "one level, 12 x 12 cells of 20 m, one slab over all",
        build: edge::huge_level,
    },
    Project {
        name: "edge_walls",
        about: "zero-length wall refused; overlapping, crossing, short and arc walls",
        build: edge::walls,
    },
    Project {
        name: "edge_deleted_host",
        about: "wall and slab deleted under their openings and hosted components",
        build: edge::deleted_host,
    },
    Project {
        name: "edge_extreme",
        about: "survey coordinates, deep and high levels, 100 m wall, 1 mm layer",
        build: edge::extreme,
    },
    Project {
        name: "edge_units",
        about: "metric and imperial sizes, parameters of every unit kind",
        build: edge::units,
    },
];

pub fn project(name: &str) -> &'static Project {
    PROJECTS
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("no showcase project '{name}'"))
}

/// A fresh in-memory model.
pub fn fresh() -> Model {
    Model::open(":memory:").unwrap()
}

/// The project built into a fresh in-memory model, every material text with its row.
pub fn build(name: &str) -> Model {
    let mut m = build_raw(name);
    ensure_used_materials(&mut m);
    m
}

/// The project as its builder leaves it (material texts may lack rows).
pub fn build_raw(name: &str) -> Model {
    let mut m = fresh();
    (project(name).build)(&mut m);
    m
}

/// Runs `f` for every project (one model each) and fails once, listing every failed project.
pub fn each(f: impl Fn(&Project, Model) -> Result<(), String>) {
    let mut bad = vec![];
    for p in PROJECTS {
        let r = catch_unwind(AssertUnwindSafe(|| f(p, build(p.name))));
        match r {
            Ok(Ok(())) => {}
            Ok(Err(e)) => bad.push(format!("{}: {e}", p.name)),
            Err(e) => {
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                bad.push(format!("{}: panicked: {msg}", p.name));
            }
        }
    }
    assert!(bad.is_empty(), "showcase failures:\n{}", bad.join("\n"));
}

// ---------------------------------------------------------------- building helpers

pub fn layer(material: &str, thickness: f64) -> LayerSpec {
    LayerSpec {
        material: material.into(),
        thickness,
    }
}

/// Adds a material row, as an undo step, for a layer text that has none (the Materials sheet
/// and the scenarios that edit properties want every text as a row).
pub fn ensure_material(m: &mut Model, name: &str) {
    let n: i64 = m
        .sql()
        .query_row("SELECT count(*) FROM material WHERE name=?1", [name], |r| {
            r.get(0)
        })
        .unwrap();
    if n == 0 {
        m.add_material(&material_defaults(name)).unwrap();
    }
}

/// Material texts in use (layers, stairs, railings, pipes...) that have no material row: the
/// rows opening a file would add. Same scan as the model's own (`material` TEXT columns).
pub fn unmaterialized(m: &Model) -> Vec<String> {
    let tables: Vec<String> = m
        .sql()
        .prepare(
            "SELECT m.name FROM sqlite_master m, pragma_table_info(m.name) p \
             WHERE m.type='table' AND p.name='material' AND upper(p.type)='TEXT' \
             AND m.name<>'material' AND m.name NOT LIKE 'sqlite_%' AND m.name NOT LIKE 'src_%' \
             ORDER BY m.name",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|x| x.unwrap())
        .filter(|t| !t.ends_with("_qty"))
        .collect();
    let mut names = std::collections::BTreeSet::new();
    for t in tables {
        let sql = format!(
            "SELECT DISTINCT material FROM \"{t}\" WHERE typeof(material)='text' \
             AND material NOT IN (SELECT name FROM material)"
        );
        let mut st = m.sql().prepare(&sql).unwrap();
        for n in st.query_map([], |r| r.get::<_, String>(0)).unwrap() {
            let n = n.unwrap();
            if !n.trim().is_empty() {
                names.insert(n);
            }
        }
    }
    names.into_iter().collect()
}

/// Adds (journaled) the material rows `unmaterialized` lists.
pub fn ensure_used_materials(m: &mut Model) {
    for n in unmaterialized(m) {
        ensure_material(m, &n);
    }
}

/// A wall type whose layer materials all have a material row.
pub fn wall_type(m: &mut Model, name: &str, layers: &[(&str, f64)]) -> i64 {
    for (mat, _) in layers {
        ensure_material(m, mat);
    }
    let ls: Vec<LayerSpec> = layers.iter().map(|(n, t)| layer(n, *t)).collect();
    m.add_wall_type(name, &ls).unwrap()
}

/// A slab type whose layer materials all have a material row.
pub fn slab_type(m: &mut Model, name: &str, layers: &[(&str, f64)]) -> i64 {
    for (mat, _) in layers {
        ensure_material(m, mat);
    }
    let ls: Vec<LayerSpec> = layers.iter().map(|(n, t)| layer(n, *t)).collect();
    m.add_slab_type(name, &ls).unwrap()
}

pub fn wall(level: i64, wall_type: i64, a: (f64, f64), b: (f64, f64), height: f64) -> WallSpec {
    WallSpec {
        a,
        b,
        level,
        base_offset: 0.0,
        height,
        wall_type,
        offset0: None,
    }
}

/// Closed ring of walls through `pts` (counter-clockwise for an outer ring).
pub fn ring(level: i64, wall_type: i64, pts: &[(f64, f64)], height: f64) -> Vec<WallSpec> {
    (0..pts.len())
        .map(|k| wall(level, wall_type, pts[k], pts[(k + 1) % pts.len()], height))
        .collect()
}

pub fn door(along: f64, width: f64) -> OpeningSpec {
    OpeningSpec {
        along,
        width,
        sill: 0.0,
        height: 2.1,
        kind: "door".into(),
    }
}

pub fn window(along: f64, width: f64, sill: f64, height: f64) -> OpeningSpec {
    OpeningSpec {
        along,
        width,
        sill,
        height,
        kind: "window".into(),
    }
}

/// Counter-clockwise rectangle.
pub fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

/// Clockwise rectangle (a hole in a slab outline).
pub fn hole(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
    vec![(x0, y0), (x0, y1), (x1, y1), (x1, y0)]
}

/// `text` repeated and cut to exactly `n` characters (not bytes), never ending in a space
/// (names are stored trimmed).
pub fn long_text(text: &str, n: usize) -> String {
    let mut v: Vec<char> = text.chars().cycle().take(n).collect();
    if let Some(last) = v.last_mut().filter(|c| c.is_whitespace()) {
        *last = '.';
    }
    v.into_iter().collect()
}

/// Component type id of a library family.
pub fn lib_type(m: &mut Model, family: &str, ty: &str) -> i64 {
    let f = m.load_library_family(family).unwrap();
    m.comp_type_id(f, ty)
        .unwrap()
        .unwrap_or_else(|| panic!("library {family} has no type {ty}"))
}

/// First id of `sql` (a query of one integer column), if any row.
pub fn first_id(m: &Model, sql: &str) -> Option<i64> {
    m.sql()
        .prepare(sql)
        .ok()?
        .query_row([], |r| r.get::<_, i64>(0))
        .ok()
}

pub fn count(m: &Model, table: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| {
            r.get(0)
        })
        .unwrap_or(0)
}

pub fn has_table(m: &Model, table: &str) -> bool {
    m.sql()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
        > 0
}

// ---------------------------------------------------------------- model check

/// (table, problem, query of the offending ids). Rules that the integrity check does not cover:
/// every element must be valid for its kind, not only reference existing rows.
const KIND_RULES: &[(&str, &str, &str)] = &[
    ("level", "level without a name", "SELECT id FROM level WHERE trim(name)=''"),
    (
        "level",
        "elevation not finite",
        "SELECT id FROM level WHERE elevation IS NULL OR abs(elevation) > 1e300",
    ),
    (
        "wall",
        "wall shorter than 1 mm",
        "SELECT id FROM wall WHERE (bx-ax)*(bx-ax)+(by-ay)*(by-ay) < 1e-6",
    ),
    ("wall", "wall height not positive", "SELECT id FROM wall WHERE NOT height > 0"),
    (
        "wall_type",
        "wall type with a layer of no thickness",
        "SELECT DISTINCT wtype FROM wall_type_layer WHERE NOT thickness > 0",
    ),
    (
        "slab_type",
        "slab type with a layer of no thickness",
        "SELECT DISTINCT stype FROM slab_type_layer WHERE NOT thickness > 0",
    ),
    (
        "wall_type",
        "layer without a material",
        "SELECT DISTINCT wtype FROM wall_type_layer WHERE trim(material)=''",
    ),
    (
        "opening",
        "opening of no size",
        "SELECT id FROM opening WHERE NOT (width > 0 AND height > 0)",
    ),
    (
        "opening",
        "opening outside its straight wall",
        "SELECT o.id FROM opening o JOIN wall w ON w.id=o.wall WHERE coalesce(w.bulge,0)=0 \
         AND (o.along < -1e-9 OR (o.along+o.width)*(o.along+o.width) > \
         (w.bx-w.ax)*(w.bx-w.ax)+(w.by-w.ay)*(w.by-w.ay) + 1e-9)",
    ),
    (
        "opening",
        "opening of an unknown kind",
        "SELECT id FROM opening WHERE kind NOT IN ('door','window','void')",
    ),
    ("room", "room height not positive", "SELECT id FROM room WHERE NOT height > 0"),
    (
        "slab",
        "slab outline under 3 points",
        "SELECT id FROM slab WHERE (SELECT count(*) FROM slab_vertex v \
         WHERE v.slab=slab.id AND v.ring=0) < 3",
    ),
    (
        "curtain_wall",
        "curtain wall height not positive",
        "SELECT id FROM curtain_wall WHERE NOT height > 0",
    ),
    (
        "mep_run",
        "run with fewer than 2 points",
        "SELECT id FROM mep_run WHERE instr(points, ';') = 0",
    ),
    (
        "comp",
        "component on a wall that is gone",
        "SELECT id FROM comp WHERE host_wall IS NOT NULL \
         AND host_wall NOT IN (SELECT id FROM wall)",
    ),
    (
        "comp",
        "component on a slab that is gone",
        "SELECT id FROM comp WHERE host_slab IS NOT NULL \
         AND host_slab NOT IN (SELECT id FROM slab)",
    ),
    ("sheet", "sheet without a number", "SELECT id FROM sheet WHERE trim(number)=''"),
    ("viewport", "viewport scale not positive", "SELECT id FROM viewport WHERE NOT scale > 0"),
];

/// Every problem of the model: integrity issues (references resolve, no orphans), per-kind
/// rules and non-finite numbers. Empty = the model is valid. Reads only.
pub fn validate(m: &Model) -> Vec<String> {
    let mut out: Vec<String> = m
        .check_integrity()
        .unwrap()
        .into_iter()
        .map(|i| format!("{} {}: {}", i.table, i.id, i.problem))
        .collect();
    for (table, problem, sql) in KIND_RULES {
        if !has_table(m, table) {
            continue;
        }
        let mut st = m
            .sql()
            .prepare(sql)
            .unwrap_or_else(|e| panic!("rule '{problem}': {e}"));
        let ids: Vec<i64> = st
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|x| x.unwrap())
            .collect();
        out.extend(ids.into_iter().map(|id| format!("{table} {id}: {problem}")));
    }
    out.extend(non_finite(m));
    out
}

/// Infinite numbers anywhere (NaN is stored as NULL and refused by NOT NULL columns).
fn non_finite(m: &Model) -> Vec<String> {
    let mut out = vec![];
    for t in tables(m) {
        let cols: Vec<String> = m
            .sql()
            .prepare(&format!(
                "SELECT name FROM pragma_table_info('{t}') \
                 WHERE upper(type) IN ('REAL','DOUBLE','FLOAT')"
            ))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|c| c.unwrap())
            .collect();
        for c in cols {
            let sql =
                format!("SELECT count(*) FROM \"{t}\" WHERE \"{c}\" = 9e999 OR \"{c}\" = -9e999");
            let n: i64 = m.sql().query_row(&sql, [], |r| r.get(0)).unwrap_or(0);
            if n > 0 {
                out.push(format!("{t}.{c}: {n} infinite values"));
            }
        }
    }
    out
}

/// Fails with every problem `validate` finds.
pub fn assert_valid(m: &Model, ctx: &str) {
    let v = validate(m);
    assert!(v.is_empty(), "{ctx}: model check failed:\n{}", v.join("\n"));
}

/// `validate` as a Result, for `each`.
pub fn valid(m: &Model, ctx: &str) -> Result<(), String> {
    let v = validate(m);
    if v.is_empty() {
        Ok(())
    } else {
        Err(format!("{ctx}: model check failed: {}", v.join("; ")))
    }
}

// ---------------------------------------------------------------- dumps

/// Rows of one table as text, sorted.
pub type TableRows = (String, Vec<String>);

/// User tables (no `sqlite_*`), sorted by name.
pub fn tables(m: &Model) -> Vec<String> {
    let mut st = m
        .sql()
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' \
             ORDER BY name",
        )
        .unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .map(|x| x.unwrap())
        .collect()
}

/// Recomputed after every step with fresh row ids (quantities, cut bodies, caches, checks):
/// not the user's data, so an undo compares without them.
pub fn derived(t: &str) -> bool {
    t.ends_with("_qty")
        || t.ends_with("_cut")
        || t.ends_with("_cache")
        || ["struct_issue", "solid_base", "geom_join", "dim_geo"].contains(&t)
}

/// What a dump holds besides the user's rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// Every row of every table, the undo journal included.
    All,
    /// No journal (`op`): a fresh model and an undone one differ there by design.
    Data,
    /// No journal and no derived tables (`derived`).
    User,
}

/// Every row of every table per `keep`, sorted per table; empty tables are left out (additive
/// tables appear lazily). Nonces are left out: the stamp of a journal row and the project uid.
/// Clash statuses live outside the journal and are not part of any contract here.
pub fn dump(m: &Model, keep: Keep) -> Vec<TableRows> {
    let db = m.sql();
    let mut out = vec![];
    for t in tables(m) {
        if t == "join_cache" || t == "clash_item" {
            continue;
        }
        if keep != Keep::All && t == "op" {
            continue;
        }
        if keep == Keep::User && derived(&t) {
            continue;
        }
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

/// First rows that differ per table; empty when the dumps are equal.
pub fn diff(now: &[TableRows], want: &[TableRows]) -> String {
    let mut s = String::new();
    let names: std::collections::BTreeSet<&String> =
        now.iter().chain(want).map(|t| &t.0).collect();
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

/// `Err` with a table diff when the dumps differ.
pub fn same(now: &[TableRows], want: &[TableRows], ctx: &str) -> Result<(), String> {
    let d = diff(now, want);
    if d.is_empty() {
        Ok(())
    } else {
        Err(format!("{ctx}: model differs{d}"))
    }
}

/// Undoes until nothing is left; the number of steps undone.
pub fn undo_all(m: &mut Model) -> Result<usize, String> {
    let mut n = 0;
    loop {
        match m.undo() {
            Ok(()) => n += 1,
            Err(ddc_model::Error::NothingToUndo) => return Ok(n),
            Err(e) => return Err(format!("undo step {n}: {e}")),
        }
    }
}

/// Fresh temp path for this process and call (tests of one binary run in parallel).
pub fn temp_file(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("ddc_showcase");
    std::fs::create_dir_all(&dir).unwrap();
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = dir.join(format!("{}_{n}_{name}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

/// Removes a model file with its journal side files.
pub fn remove_db(p: &std::path::Path) {
    for ext in ["", "-wal", "-shm"] {
        let mut s = p.as_os_str().to_owned();
        s.push(ext);
        let _ = std::fs::remove_file(s);
    }
}
