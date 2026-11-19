// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Seeded fuzz of the creation tools: random click sequences of the loop and two-point tools
//! with Esc, undo, redo and save + reopen. Failures are product findings; the failing seed and
//! op are in the panic message. `DDC_FUZZ_SEEDS=n` runs seeds 1..=n (default 3);
//! the ignored `creation_fuzz_long` runs 2000 ops per seed.

use super::{arm, Kind};
use crate::editor::Editor;
use crate::wall_tool::{Event, Key, Mode, MIN_LEN};
use ddc_geom::{v2, V2};
use ddc_model::{Model, WallSpec};
use ddc_ui::i18n::Lang;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Tables the tools write to; NaN, geometry and Esc checks look here.
const KEY: [&str; 15] = [
    "wall",
    "slab",
    "slab_vertex",
    "slab_type",
    "roof",
    "roof_vertex",
    "ceiling",
    "ceiling_vertex",
    "shaft",
    "shaft_vertex",
    "beam",
    "grid",
    "room",
    "level",
    "opening",
];

const LOOPS: [Kind; 5] = [
    Kind::Floor,
    Kind::StructSlab,
    Kind::Roof,
    Kind::Ceiling,
    Kind::Shaft,
];

#[derive(Clone, Copy)]
struct Checks {
    undo_all: bool,
    reopen: bool,
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn db<T>(r: Result<T, rusqlite::Error>) -> Result<T, String> {
    r.map_err(|e| e.to_string())
}

fn tables(e: &Editor) -> Result<Vec<String>, String> {
    let c = e.model.sql();
    let mut st = db(c.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' \
         AND name NOT LIKE '%cache' AND name NOT IN ('meta','ddc_dirty') ORDER BY name",
    ))?;
    let rows = db(st.query_map([], |r| r.get::<_, String>(0)))?;
    db(rows.collect())
}

fn count(c: &rusqlite::Connection, t: &str) -> Result<i64, String> {
    db(c.query_row(&format!("SELECT COUNT(*) FROM \"{t}\""), [], |r| r.get(0)))
}

fn counts(e: &Editor) -> Result<Vec<(String, i64)>, String> {
    tables(e)?
        .into_iter()
        .map(|t| count(e.model.sql(), &t).map(|n| (t, n)))
        .collect()
}

fn key_total(e: &Editor) -> Result<i64, String> {
    let mut n = 0;
    for t in KEY {
        n += count(e.model.sql(), t)?;
    }
    Ok(n)
}

/// Every row of a table as text, sorted.
fn dump(c: &rusqlite::Connection, t: &str) -> Result<Vec<String>, String> {
    let mut st = db(c.prepare(&format!("SELECT * FROM \"{t}\"")))?;
    let n = st.column_count();
    let mut rows = db(st.query([]))?;
    let mut out = vec![];
    while let Some(r) = db(rows.next())? {
        let mut s = String::new();
        for i in 0..n {
            s += &format!("{:?}|", db(r.get_ref(i))?);
        }
        out.push(s);
    }
    out.sort();
    Ok(out)
}

fn fixture(e: &mut Editor) {
    let wall_type = e.current_type().unwrap().id;
    let level = e.level;
    // 5 x 5 lattice, 4 m step: 20 + 20 walls
    for i in 0..5 {
        for j in 0..5 {
            let (x, y) = (i as f64 * 4.0, j as f64 * 4.0);
            for (a, b, on) in [((x, y), (x + 4.0, y), i < 4), ((x, y), (x, y + 4.0), j < 4)] {
                if on {
                    let spec = WallSpec {
                        a,
                        b,
                        level,
                        base_offset: 0.0,
                        height: 3.0,
                        wall_type,
                        offset0: None,
                    };
                    e.model.add_wall(&spec).unwrap();
                }
            }
        }
    }
    e.reload().unwrap();
    e.tool.underlay_segs = drawing();
}

/// A closed drawing loop beside the walls.
fn drawing() -> Arc<Vec<(V2, V2)>> {
    let c = [v2(20.0, 0.0), v2(26.0, 0.0), v2(26.0, 6.0), v2(20.0, 6.0)];
    Arc::new((0..4).map(|i| (c[i], c[(i + 1) % 4])).collect())
}

fn ev(e: &mut Editor, ev: Event) -> Result<(), String> {
    let u = e.undo_len();
    e.handle(ev);
    let d = e.undo_len();
    if d > u + 1 {
        return Err(format!("one event pushed {} undo steps", d - u));
    }
    Ok(())
}

fn click(e: &mut Editor, p: V2) -> Result<(), String> {
    ev(e, Event::Move(p))?;
    ev(e, Event::Click(p))
}

/// Esc never writes a row nor an undo step.
fn esc(e: &mut Editor) -> Result<(), String> {
    let (rows, u) = (key_total(e)?, e.undo_len());
    ev(e, Event::Key(Key::Escape))?;
    if key_total(e)? != rows || e.undo_len() != u {
        return Err("Esc changed the model".into());
    }
    Ok(())
}

fn finish(e: &mut Editor) -> Result<(), String> {
    for _ in 0..12 {
        if e.model_tool().is_none() {
            break;
        }
        esc(e)?;
    }
    e.set_mode(Mode::Select);
    Ok(())
}

/// Lattice point, a near-duplicate of the previous click or a continuation of the last edge.
fn pt(r: &mut Rng, prev: &[V2]) -> V2 {
    match (r.below(10), prev) {
        (0, [.., p]) => v2(p.x + 0.001, p.y),
        (1, [.., q, p]) => v2(p.x + (p.x - q.x) * 0.5, p.y + (p.y - q.y) * 0.5),
        _ => v2(r.below(25) as f64, r.below(25) as f64),
    }
}

fn points(r: &mut Rng, n: u64) -> Vec<V2> {
    let mut v: Vec<V2> = vec![];
    for _ in 0..n {
        let p = pt(r, &v);
        v.push(p);
    }
    v
}

fn orient(a: V2, b: V2, c: V2) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

fn crosses(a: V2, b: V2, c: V2, d: V2) -> bool {
    const EPS: f64 = 1e-9;
    let (d1, d2, d3, d4) = (orient(c, d, a), orient(c, d, b), orient(a, b, c), orient(a, b, d));
    ((d1 > EPS && d2 < -EPS) || (d1 < -EPS && d2 > EPS))
        && ((d3 > EPS && d4 < -EPS) || (d3 < -EPS && d4 > EPS))
}

fn check_ring(tag: &str, outer: bool, p: &[V2]) -> Result<(), String> {
    let n = p.len();
    if n < 3 {
        return Err(format!("{tag}: ring of {n} points"));
    }
    if p.iter().any(|q| !q.x.is_finite() || !q.y.is_finite()) {
        return Err(format!("{tag}: non-finite vertex"));
    }
    let mut area = 0.0;
    for i in 0..n {
        let (a, b, c) = (p[i], p[(i + 1) % n], p[(i + 2) % n]);
        let (l1, l2) = ((b.x - a.x).hypot(b.y - a.y), (c.x - b.x).hypot(c.y - b.y));
        if l1 < MIN_LEN {
            return Err(format!("{tag}: edge {i} is {l1} long"));
        }
        let cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
        let dot = (b.x - a.x) * (c.x - b.x) + (b.y - a.y) * (c.y - b.y);
        if cross.abs() < 1e-9 * l1 * l2 && dot < 0.0 {
            return Err(format!("{tag}: spike at vertex {}", (i + 1) % n));
        }
        area += a.x * b.y - b.x * a.y;
    }
    if outer && area <= 0.0 {
        return Err(format!("{tag}: outline is not counter-clockwise"));
    }
    for i in 0..n {
        for j in i + 2..n {
            if i == 0 && j == n - 1 {
                continue;
            }
            if crosses(p[i], p[(i + 1) % n], p[j], p[(j + 1) % n]) {
                return Err(format!("{tag}: edges {i} and {j} cross"));
            }
        }
    }
    Ok(())
}

/// `sql` yields (owner, ring, x, y) ordered by owner, ring, index.
fn check_rings(e: &Editor, name: &str, sql: &str) -> Result<(), String> {
    let mut st = db(e.model.sql().prepare(sql))?;
    let it = db(st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, f64>(3)?,
        ))
    }))?;
    let rows: Vec<(i64, i64, f64, f64)> = db(it.collect())?;
    let mut i = 0;
    while i < rows.len() {
        let (o, k) = (rows[i].0, rows[i].1);
        let mut ring = vec![];
        while i < rows.len() && (rows[i].0, rows[i].1) == (o, k) {
            ring.push(v2(rows[i].2, rows[i].3));
            i += 1;
        }
        check_ring(&format!("{name} {o} ring {k}"), k == 0, &ring)?;
    }
    Ok(())
}

fn check_segments(e: &Editor, name: &str, sql: &str) -> Result<(), String> {
    let mut st = db(e.model.sql().prepare(sql))?;
    let it = db(st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, f64>(3)?,
            r.get::<_, f64>(4)?,
        ))
    }))?;
    let rows: Vec<(i64, f64, f64, f64, f64)> = db(it.collect())?;
    for (id, ax, ay, bx, by) in rows {
        let l = (bx - ax).hypot(by - ay);
        if !l.is_finite() || l < MIN_LEN {
            return Err(format!("{name} {id}: length {l}"));
        }
    }
    Ok(())
}

fn check_model(e: &Editor) -> Result<(), String> {
    for t in KEY {
        for row in dump(e.model.sql(), t)? {
            if row.contains("Real(NaN") || row.contains("Real(inf") || row.contains("Real(-inf") {
                return Err(format!("{t}: non-finite value in {row}"));
            }
        }
    }
    check_segments(e, "wall", "SELECT id, ax, ay, bx, by FROM wall")?;
    check_segments(e, "beam", "SELECT id, ax, ay, bx, by FROM beam")?;
    check_segments(e, "grid", "SELECT id, ax, ay, bx, by FROM grid")?;
    check_rings(
        e,
        "slab",
        "SELECT slab, ring, x, y FROM slab_vertex ORDER BY slab, ring, idx",
    )?;
    check_rings(
        e,
        "ceiling",
        "SELECT ceiling, ring, x, y FROM ceiling_vertex ORDER BY ceiling, ring, idx",
    )?;
    check_rings(
        e,
        "roof",
        "SELECT v.roof, 0, v.x, v.y FROM roof_vertex v JOIN roof r ON r.id = v.roof \
         WHERE r.kind = 'footprint' ORDER BY v.roof, v.idx",
    )?;
    check_rings(
        e,
        "shaft",
        "SELECT shaft, 0, x, y FROM shaft_vertex ORDER BY shaft, idx",
    )
}

static FILES: AtomicU64 = AtomicU64::new(0);

/// Save, open the copy and compare every table with the live model.
fn reopen_equal(e: &Editor) -> Result<(), String> {
    let n = FILES.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("ddc-fuzz-{}-{n}.db", std::process::id()));
    let res = (|| -> Result<(), String> {
        e.model.snapshot_to(&path).map_err(|x| x.to_string())?;
        let s = path.to_str().ok_or("non-UTF-8 temp path")?;
        let m = Model::open(s).map_err(|x| x.to_string())?;
        for t in tables(e)? {
            if dump(e.model.sql(), &t)? != dump(m.sql(), &t)? {
                return Err(format!("reopen: table {t} differs"));
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&path);
    res
}

fn step(e: &mut Editor, r: &mut Rng, c: Checks, name: &mut String) -> Result<(), String> {
    let kind = LOOPS[r.below(LOOPS.len() as u64) as usize];
    match r.below(100) {
        0..=29 => {
            *name = format!("loop {kind:?}");
            arm(e, kind, Lang::En);
            e.tool.underlay_segs = drawing();
            let n = 3 + r.below(4);
            let pts = points(r, n);
            for &p in &pts {
                click(e, p)?;
            }
            if r.below(4) == 0 {
                click(e, pts[0])?;
            }
            ev(e, Event::Key(Key::Enter))?;
            finish(e)
        }
        30..=41 => {
            *name = format!("loop {kind:?} then Esc");
            arm(e, kind, Lang::En);
            let n = 1 + r.below(5);
            for p in points(r, n) {
                click(e, p)?;
            }
            finish(e)
        }
        42..=55 => {
            *name = "pick region".into();
            let kind = if r.below(2) == 0 { Kind::Floor } else { Kind::Ceiling };
            arm(e, kind, Lang::En);
            e.tool.underlay_segs = drawing();
            e.model_tool_mut().unwrap().opts.pick = true;
            let p = match r.below(10) {
                0..=4 => v2(r.unit() * 16.0, r.unit() * 16.0),
                5..=7 => v2(20.0 + r.unit() * 6.0, r.unit() * 6.0),
                _ => v2(r.unit() * 40.0 - 5.0, r.unit() * 40.0 - 5.0),
            };
            click(e, p)?;
            finish(e)
        }
        56..=63 => {
            *name = "grid".into();
            arm(e, Kind::Grid, Lang::En);
            for p in points(r, 2) {
                click(e, p)?;
            }
            finish(e)
        }
        64..=73 => {
            *name = "beam chain".into();
            arm(e, Kind::Beam, Lang::En);
            let n = 2 + r.below(3);
            for p in points(r, n) {
                click(e, p)?;
            }
            finish(e)
        }
        74..=81 => {
            *name = "wall chain".into();
            e.set_mode(Mode::Polyline);
            let n = 2 + r.below(4);
            for p in points(r, n) {
                click(e, p)?;
            }
            // the first Esc of a wall chain finishes it (wall_tool, Key::Escape): one step at most
            ev(e, Event::Key(Key::Escape))?;
            finish(e)
        }
        82..=89 => {
            *name = "undo".into();
            e.set_mode(Mode::Select);
            let (u, rd) = (e.undo_len(), e.redo_len());
            e.undo();
            let want = (u.saturating_sub(1), if u > 0 { rd + 1 } else { rd });
            if (e.undo_len(), e.redo_len()) != want {
                return Err(format!(
                    "undo: stacks {u}/{rd} became {}/{}",
                    e.undo_len(),
                    e.redo_len()
                ));
            }
            Ok(())
        }
        90..=94 => {
            *name = "redo".into();
            e.set_mode(Mode::Select);
            let (u, rd) = (e.undo_len(), e.redo_len());
            e.redo();
            if e.undo_len() < u || e.redo_len() != rd.saturating_sub(1) {
                return Err(format!(
                    "redo: stacks {u}/{rd} became {}/{}",
                    e.undo_len(),
                    e.redo_len()
                ));
            }
            Ok(())
        }
        _ => {
            *name = "save and reopen".into();
            e.set_mode(Mode::Select);
            if c.reopen {
                reopen_equal(e)?;
            }
            Ok(())
        }
    }
}

fn run(seed: u64, ops: usize, c: Checks) -> Result<(), String> {
    let mut e = Editor::new_in_memory().map_err(|x| x.to_string())?;
    fixture(&mut e);
    let base = counts(&e)?;
    let mut r = Rng(seed);
    let mut name = String::new();
    for i in 0..ops {
        step(&mut e, &mut r, c, &mut name)
            .and_then(|()| check_model(&e))
            .map_err(|m| format!("seed {seed}, op {i} ({name}): {m}"))?;
    }
    if c.undo_all {
        e.set_mode(Mode::Select);
        for _ in 0..e.undo_len() + 2 {
            e.undo();
        }
        if e.undo_len() != 0 {
            return Err(format!("seed {seed}: {} undo steps left", e.undo_len()));
        }
        let now = counts(&e)?;
        if now != base {
            let diff: Vec<_> = base.iter().zip(&now).filter(|(a, b)| a != b).collect();
            return Err(format!("seed {seed}: undo of everything left {diff:?}"));
        }
    }
    Ok(())
}

fn seeds() -> Vec<u64> {
    let n = std::env::var("DDC_FUZZ_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    (1..=n).collect()
}

fn all(ops: usize, c: Checks) {
    for s in seeds() {
        if let Err(m) = run(s, ops, c) {
            panic!("{m}");
        }
    }
}

const PLAIN: Checks = Checks {
    undo_all: false,
    reopen: false,
};

#[test]
fn creation_fuzz_rings_valid() {
    all(200, PLAIN);
}

#[test]
fn creation_fuzz_undo_all_restores() {
    all(200, Checks { undo_all: true, ..PLAIN });
}

#[test]
fn creation_fuzz_reopen_equal() {
    all(200, Checks { reopen: true, ..PLAIN });
}

#[test]
#[ignore = "long: 2000 ops per seed"]
fn creation_fuzz_long() {
    all(2000, Checks { undo_all: true, reopen: true });
}
