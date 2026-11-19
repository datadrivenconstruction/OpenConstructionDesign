//! Clashes between sets A × B: selection by category/level/list, hard and clearance clashes,
//! statuses kept between runs (the `clash_item` table is created on first use and the schema
//! version does not change; statuses are check data and are not written to the edit journal),
//! and quick route fixes. Statuses are stored per check (`ClashParams::test_key`) and pair;
//! deleting an element deletes its pairs (triggers), so a reissued id does not inherit another
//! element's status, and undoing the deletion restores them (`clash_gone`).

use std::collections::{BTreeMap, HashMap, HashSet};

use ddc_geom::math::v2;
use ddc_geom::mep::clash::{Aabb, Body, Bvh, Convex};
use ddc_geom::mep::Domain;
use rusqlite::{params, types::Value, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::walls::OpeningSpec;
use crate::{Error, Model};

use super::RunSpec;

/// Selection categories: MEP runs and structures.
pub const CLASH_CATS: [&str; 8] = [
    "duct", "pipe", "tray", "conduit", "wall", "slab", "column", "beam",
];

/// Element: (category, id in its own table).
pub type ElKey = (String, i64);

/// A network run (not a structure).
pub fn is_mep(kind: &str) -> bool {
    matches!(kind, "duct" | "pipe" | "tray" | "conduit")
}

fn cat_of(d: Domain) -> &'static str {
    match d {
        Domain::Duct => "duct",
        Domain::Pipe => "pipe",
        Domain::CableTray => "tray",
        Domain::Conduit => "conduit",
    }
}

fn invalid<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

/// One side of a check. An empty category list selects nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClashSide {
    pub cats: Vec<String>,
    pub level: Option<i64>,
    /// Only these elements (selection in the model).
    pub only: Option<Vec<ElKey>>,
}

impl ClashSide {
    pub fn cats(c: &[&str]) -> Self {
        ClashSide {
            cats: c.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }
    fn takes(&self, kind: &str, id: i64, level: Option<i64>) -> bool {
        self.cats.iter().any(|c| c == kind)
            && self.level.is_none_or(|l| level == Some(l))
            && self
                .only
                .as_ref()
                .is_none_or(|o| o.iter().any(|(k, i)| k == kind && *i == id))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClashParams {
    pub a: ClashSide,
    pub b: ClashSide,
    /// Meters: with `clearance`, the required gap around runs; otherwise an intersection smaller
    /// than a cube of side `tolerance` is ignored.
    pub tolerance: f64,
    pub clearance: bool,
}

impl ClashParams {
    /// Structures against each other (column in a wall, wall overlap at a joint): one check for
    /// the `clashes` rule and the clash tool with the same sets, so the counts match.
    pub fn structure() -> Self {
        ClashParams {
            a: ClashSide::cats(&["wall", "column"]),
            b: ClashSide::cats(&["wall", "column"]),
            ..ClashParams::default()
        }
    }

    /// Check key (sets, mode, tolerance): "Resolved" is set only on pairs of the same check.
    pub fn test_key(&self) -> String {
        let side = |s: &ClashSide| {
            let mut c = s.cats.clone();
            c.sort();
            let only = s.only.as_ref().map_or(String::new(), |o| {
                let mut o = o.clone();
                o.sort();
                let t: Vec<String> = o.iter().map(|(k, i)| format!("{k}{i}")).collect();
                format!("#{:016x}", crate::fnv1a(t.join(",").as_bytes()))
            });
            let l = s.level.map_or("*".to_string(), |l| l.to_string());
            format!("{}@{l}{only}", c.join(","))
        };
        format!(
            "{}|{}|{}{:.1}",
            side(&self.a),
            side(&self.b),
            if self.clearance { "c" } else { "h" },
            self.tolerance * 1000.0
        )
    }
}

/// Bodies of both sides, ready for computation without model access (can run on a thread).
pub struct ClashInput {
    a: Vec<(ElKey, Body)>,
    b: Vec<(ElKey, Body)>,
    min_volume: f64,
    levels: HashMap<ElKey, Option<i64>>,
    test: String,
    /// Wall axes (ax, ay, bx, by) and thickness: an overlap at a junction is not a clash.
    walls: HashMap<i64, ([f64; 4], f64)>,
}

/// L/T junction: an end of one wall lies on the axis of another and the intersection stays
/// within the junction square (thickness by thickness). An overlap beyond the junction or a
/// crossing without abutment is a clash.
fn wall_joint(a: ([f64; 4], f64), b: ([f64; 4], f64), lo: [f64; 3], hi: [f64; 3]) -> bool {
    const EPS: f64 = 1e-6;
    let t = a.1.max(b.1);
    if hi[0] - lo[0] > t + EPS || hi[1] - lo[1] > t + EPS {
        return false;
    }
    let c = ((lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0);
    let on_axis = |p: (f64, f64), w: [f64; 4]| {
        let (dx, dy) = (w[2] - w[0], w[3] - w[1]);
        let l2 = dx * dx + dy * dy;
        if l2 < EPS {
            return false;
        }
        let s = (((p.0 - w[0]) * dx + (p.1 - w[1]) * dy) / l2).clamp(0.0, 1.0);
        let (qx, qy) = (w[0] + s * dx - p.0, w[1] + s * dy - p.1);
        (qx * qx + qy * qy).sqrt() <= 1e-3
    };
    let ends = |w: [f64; 4]| [(w[0], w[1]), (w[2], w[3])];
    let near = |p: (f64, f64)| ((p.0 - c.0).powi(2) + (p.1 - c.1).powi(2)).sqrt() <= t + EPS;
    ends(a.0).into_iter().any(|p| near(p) && on_axis(p, b.0))
        || ends(b.0).into_iter().any(|p| near(p) && on_axis(p, a.0))
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClashHit {
    pub a: ElKey,
    pub b: ElKey,
    pub volume: f64,
    /// Extents of the intersection itself.
    pub lo: [f64; 3],
    pub hi: [f64; 3],
    pub level: Option<i64>,
}

impl ClashHit {
    pub fn at(&self) -> [f64; 3] {
        std::array::from_fn(|i| (self.lo[i] + self.hi[i]) / 2.0)
    }
}

impl ClashInput {
    pub fn len(&self) -> usize {
        self.a.len()
    }
    pub fn is_empty(&self) -> bool {
        self.a.is_empty()
    }
    /// Check key for `clash_merge`.
    pub fn test(&self) -> &str {
        &self.test
    }

    /// Computation; `progress(done, total)` returning `false` aborts (the result is partial).
    pub fn run(&self, progress: &dyn Fn(usize, usize) -> bool) -> Vec<ClashHit> {
        let tree = Bvh::build(&self.b.iter().map(|x| x.1.aabb).collect::<Vec<_>>());
        let (mut out, mut cand) = (vec![], vec![]);
        let mut seen: HashSet<(ElKey, ElKey)> = HashSet::new();
        for (i, (ka, ba)) in self.a.iter().enumerate() {
            if !progress(i, self.a.len()) {
                break;
            }
            cand.clear();
            tree.query(&ba.aabb, &mut cand);
            cand.sort_unstable();
            for &j in &cand {
                let (kb, bb) = &self.b[j];
                if ka == kb {
                    continue;
                }
                // a pair from A∩B occurs twice: run the narrow phase once
                let (x, y) = if ka <= kb { (ka, kb) } else { (kb, ka) };
                if !seen.insert((x.clone(), y.clone())) {
                    continue;
                }
                let (v, bounds) = ba.intersection_bounds(bb);
                let Some(bx) = bounds.filter(|_| v > self.min_volume) else {
                    continue;
                };
                let (lo, hi) = ([bx.min.x, bx.min.y, bx.min.z], [bx.max.x, bx.max.y, bx.max.z]);
                if x.0 == "wall" && y.0 == "wall" {
                    if let (Some(&wa), Some(&wb)) = (self.walls.get(&x.1), self.walls.get(&y.1)) {
                        if wall_joint(wa, wb, lo, hi) {
                            continue;
                        }
                    }
                }
                let level = self
                    .levels
                    .get(x)
                    .copied()
                    .flatten()
                    .or_else(|| self.levels.get(y).copied().flatten());
                out.push(ClashHit {
                    a: x.clone(),
                    b: y.clone(),
                    volume: v,
                    lo,
                    hi,
                    level,
                });
            }
        }
        progress(self.a.len(), self.a.len());
        out.sort_by(|p, q| (&p.a, &p.b).cmp(&(&q.a, &q.b)));
        out
    }
}

/// State of a pair between runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClashStatus {
    New,
    Active,
    Reviewed,
    Resolved,
    Approved,
}

impl ClashStatus {
    pub const ALL: [ClashStatus; 5] = [
        ClashStatus::New,
        ClashStatus::Active,
        ClashStatus::Reviewed,
        ClashStatus::Resolved,
        ClashStatus::Approved,
    ];
    pub fn name(self) -> &'static str {
        match self {
            ClashStatus::New => "new",
            ClashStatus::Active => "active",
            ClashStatus::Reviewed => "reviewed",
            ClashStatus::Resolved => "resolved",
            ClashStatus::Approved => "approved",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.name() == s)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClashItem {
    /// Check key (`ClashParams::test_key`).
    pub test: String,
    pub a: ElKey,
    pub b: ElKey,
    pub status: ClashStatus,
    pub comment: String,
    pub volume: f64,
    pub lo: [f64; 3],
    pub hi: [f64; 3],
    pub level: Option<i64>,
    /// Found in the last run.
    pub present: bool,
    pub first_run: i64,
    pub last_run: i64,
}

impl ClashItem {
    pub fn at(&self) -> [f64; 3] {
        std::array::from_fn(|i| (self.lo[i] + self.hi[i]) / 2.0)
    }
}

/// Result of merging a run with the stored statuses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClashMerge {
    pub run: i64,
    pub new: usize,
    pub still: usize,
    pub resolved: usize,
}

/// Clash fix: applied as one model operation (one undo).
#[derive(Clone, Debug, PartialEq)]
pub enum ClashFix {
    Reroute {
        run: i64,
        spec: RunSpec,
    },
    Opening {
        wall: i64,
        spec: OpeningSpec,
    },
    SlabHole {
        slab: i64,
        outer: Vec<(f64, f64)>,
        holes: Vec<Vec<(f64, f64)>>,
    },
}

const DDL: &str = "CREATE TABLE IF NOT EXISTS clash_item(test TEXT NOT NULL, \
    ka TEXT NOT NULL, ia INTEGER NOT NULL, kb TEXT NOT NULL, ib INTEGER NOT NULL, \
    status TEXT NOT NULL, comment TEXT NOT NULL DEFAULT '', volume REAL NOT NULL DEFAULT 0, \
    lx REAL NOT NULL, ly REAL NOT NULL, lz REAL NOT NULL, hx REAL NOT NULL, hy REAL NOT NULL, \
    hz REAL NOT NULL, level INTEGER, present INTEGER NOT NULL DEFAULT 1, \
    first_run INTEGER NOT NULL, last_run INTEGER NOT NULL, PRIMARY KEY(test, ka, ia, kb, ib))";

/// Pair columns in the deletion snapshot.
const ITEM_COLS: &str = "test,ka,ia,kb,ib,status,comment,volume,lx,ly,lz,hx,hy,hz,level,present,\
    first_run,last_run";

fn gone_trigger() -> String {
    let old = ITEM_COLS
        .split(',')
        .map(|c| format!("OLD.{c}"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "CREATE TEMP TRIGGER IF NOT EXISTS ddc_clash_gone_d AFTER DELETE ON main.clash_item \
         BEGIN INSERT INTO ddc_clash_gone({ITEM_COLS}) VALUES ({old}); END;"
    )
}

/// Temporary per-connection table of snapshots of deleted pairs. `clash_item` itself appears on
/// first use, at which point the trigger installs `clash_table`.
pub(crate) fn clash_track(db: &Connection) -> Result<(), Error> {
    db.execute_batch(&format!(
        "CREATE TEMP TABLE IF NOT EXISTS ddc_clash_gone({ITEM_COLS});"
    ))?;
    let has: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_master WHERE type='table' AND name='clash_item')",
        [],
        |r| r.get(0),
    )?;
    if has {
        db.execute_batch(&gone_trigger())?;
    }
    Ok(())
}

/// Before an operation: no snapshots (check runs and undo also write here; it does not matter).
pub(crate) fn clash_begin(db: &Connection) -> Result<(), Error> {
    db.prepare_cached("DELETE FROM ddc_clash_gone")?
        .execute([])?;
    Ok(())
}

/// Pairs removed during the operation: undo reinserts them.
pub(crate) fn clash_gone(db: &Connection, mut inv: Inv) -> Result<Inv, Error> {
    let n = ITEM_COLS.split(',').count();
    let rows: Vec<Vec<Value>> = db
        .prepare_cached(&format!(
            "SELECT {ITEM_COLS} FROM ddc_clash_gone ORDER BY rowid"
        ))?
        .query_map([], |r| {
            (0..n)
                .map(|i| r.get::<_, Value>(i))
                .collect::<rusqlite::Result<Vec<Value>>>()
        })?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok(inv);
    }
    let ph = (1..=n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("INSERT OR REPLACE INTO clash_item({ITEM_COLS}) VALUES ({ph})");
    for v in rows {
        inv = inv.sql(&sql, v);
    }
    Ok(inv)
}

/// Element table and the pair categories it holds.
const HOSTS: [(&str, &str); 5] = [
    ("wall", "'wall'"),
    ("slab", "'slab'"),
    ("col", "'column'"),
    ("beam", "'beam'"),
    ("mep_run", "'duct','pipe','tray','conduit'"),
];

/// Deleting an element (and undoing its creation) deletes its pairs: ids are reused.
fn cascade_ddl() -> String {
    HOSTS
        .iter()
        .map(|(t, ks)| {
            format!(
                "CREATE TRIGGER IF NOT EXISTS clash_item_{t}_del AFTER DELETE ON {t} BEGIN \
                 DELETE FROM clash_item WHERE (ka IN ({ks}) AND ia=OLD.id) \
                 OR (kb IN ({ks}) AND ib=OLD.id); END;"
            )
        })
        .collect()
}

/// SQL condition "element (k, i) exists".
fn exists_sql(k: &str, i: &str) -> String {
    let arms: String = HOSTS
        .iter()
        .map(|(t, ks)| format!(" WHEN {k} IN ({ks}) THEN EXISTS(SELECT 1 FROM {t} WHERE id={i})"))
        .collect();
    format!("(CASE{arms} ELSE 0 END)")
}

fn key_order(a: &ElKey, b: &ElKey) -> (ElKey, ElKey) {
    if a <= b {
        (a.clone(), b.clone())
    } else {
        (b.clone(), a.clone())
    }
}

impl Model {
    /// Bodies of the selected sides. The clearance inflates run sections (side A or, if it has
    /// no runs, side B).
    pub fn clash_input(&self, p: &ClashParams) -> Result<ClashInput, Error> {
        if !(p.tolerance.is_finite() && p.tolerance >= 0.0) {
            return invalid("clash: tolerance must be >= 0");
        }
        let has_mep = |s: &ClashSide| s.cats.iter().any(|c| is_mep(c));
        if p.clearance && p.tolerance > 0.0 && !has_mep(&p.a) && !has_mep(&p.b) {
            return invalid("clash: clearance needs ducts, pipes, trays or conduits in a set");
        }
        let (da, db) = match (p.clearance, has_mep(&p.a)) {
            (false, _) => (0.0, 0.0),
            (true, true) => (p.tolerance, 0.0),
            (true, false) => (0.0, p.tolerance),
        };
        let mut levels: HashMap<ElKey, Option<i64>> = HashMap::new();
        let (mut a, mut b) = (vec![], vec![]);
        let want_mep = [&p.a, &p.b]
            .iter()
            .any(|s| s.cats.iter().any(|c| is_mep(c)));
        let want_str = [&p.a, &p.b]
            .iter()
            .any(|s| s.cats.iter().any(|c| !is_mep(c)));
        if want_mep {
            let meta: HashMap<i64, (&'static str, i64)> = {
                let mut st = self.db.prepare("SELECT id, domain, level FROM mep_run")?;
                let rows = st.query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?;
                let mut m = HashMap::new();
                for x in rows {
                    let (id, d, l) = x?;
                    if let Some(d) = Domain::parse(&d) {
                        m.insert(id, (cat_of(d), l));
                    }
                }
                m
            };
            for (da_, side_is_a) in [(da, true), (db, false)] {
                let side = if side_is_a { &p.a } else { &p.b };
                if !side.cats.iter().any(|c| is_mep(c)) {
                    continue;
                }
                let runs = self.run_bodies(da_, |id| {
                    meta.get(&id)
                        .is_some_and(|(k, l)| side.takes(k, id, Some(*l)))
                })?;
                for (id, body) in runs {
                    let (k, l) = meta[&id];
                    let key = (k.to_string(), id);
                    levels.insert(key.clone(), Some(l));
                    let dst = if side_is_a { &mut a } else { &mut b };
                    dst.push((key, body));
                }
            }
        }
        if want_str {
            let lv = self.structure_levels()?;
            let (ids, bodies) = self.structure_bodies()?;
            for ((kind, id), body) in ids.into_iter().zip(bodies) {
                let key = (kind.to_string(), id);
                let l = lv.get(&key).copied();
                levels.insert(key.clone(), l);
                if p.a.takes(kind, id, l) {
                    a.push((key.clone(), body.clone()));
                }
                if p.b.takes(kind, id, l) {
                    b.push((key, body));
                }
            }
        }
        Ok(ClashInput {
            a,
            b,
            // openings are pieces with sign −1: passing through a sleeve yields ±1e-17 noise, not a clash
            min_volume: if p.clearance {
                1e-9
            } else {
                p.tolerance.powi(3).max(1e-9)
            },
            levels,
            test: p.test_key(),
            walls: if want_str { self.wall_axes()? } else { HashMap::new() },
        })
    }

    fn wall_axes(&self) -> Result<HashMap<i64, ([f64; 4], f64)>, Error> {
        let mut st = self.db.prepare(
            "SELECT w.id,w.ax,w.ay,w.bx,w.by,\
             (SELECT SUM(thickness) FROM wall_type_layer WHERE wtype=w.wtype) FROM wall w",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                [r.get::<_, f64>(1)?, r.get(2)?, r.get(3)?, r.get(4)?],
                r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
            ))
        })?;
        let mut m = HashMap::new();
        for x in rows {
            let (id, ax, t) = x?;
            m.insert(id, (ax, t));
        }
        Ok(m)
    }

    /// Immediate computation (no thread): handy for tests and small models.
    pub fn clash_find(&self, p: &ClashParams) -> Result<Vec<ClashHit>, Error> {
        Ok(self.clash_input(p)?.run(&|_, _| true))
    }

    fn structure_levels(&self) -> Result<HashMap<ElKey, i64>, Error> {
        let mut m = HashMap::new();
        for (kind, table) in [
            ("wall", "wall"),
            ("slab", "slab"),
            ("column", "col"),
            ("beam", "beam"),
        ] {
            let mut st = self.db.prepare(&format!("SELECT id, level FROM {table}"))?;
            for x in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
                let (id, l) = x?;
                m.insert((kind.to_string(), id), l);
            }
        }
        Ok(m)
    }

    /// Body of each run (route pieces and fittings), section inflated by `grow`.
    fn run_bodies(&self, grow: f64, pick: impl Fn(i64) -> bool) -> Result<Vec<(i64, Body)>, Error> {
        let net = self.mep_network(None)?;
        let mut by: BTreeMap<i64, Vec<(Convex, f64)>> = BTreeMap::new();
        for (run, piece, s) in net.pieces.iter().chain(&net.fitting_pieces) {
            if !pick(*run) {
                continue;
            }
            // the clearance is an outline offset (rectangle wider and taller by 2·grow), not a scale
            let ring = s.envelope().grow(grow.max(0.0)).outline_equal_area(24);
            let samples = piece.samples(4);
            for w in samples.windows(2) {
                let ((p0, f), (p1, _)) = (w[0], w[1]);
                if (p1 - p0).len() > 1e-9 {
                    by.entry(*run)
                        .or_default()
                        .push((Convex::segment(&ring, p0, p1, f.side, f.up), 1.0));
                }
            }
        }
        Ok(by.into_iter().map(|(r, p)| (r, Body::new(p))).collect())
    }

    // ---- statuses ----

    fn clash_table(&self) -> Result<(), Error> {
        self.db
            .execute_batch(&format!("{DDL};{}{}", cascade_ddl(), gone_trigger()))?;
        Ok(())
    }

    /// Merges a run result with the statuses. A new pair is "New"; found again: "New" →
    /// "Active", "Resolved" → "Active" (it came back), "Reviewed"/"Accepted" are unchanged;
    /// vanished: "Accepted" stays, the rest become "Resolved".
    /// Pairs of other checks are untouched; pairs of deleted elements are forgotten.
    pub fn clash_merge(&mut self, test: &str, hits: &[ClashHit]) -> Result<ClashMerge, Error> {
        self.clash_table()?;
        let run: i64 = self.db.query_row(
            "SELECT COALESCE(MAX(last_run),0)+1 FROM clash_item",
            [],
            |r| r.get(0),
        )?;
        let tx = self.db.unchecked_transaction()?;
        // rows left by deletions without the trigger (the file was edited before it existed)
        tx.execute(
            &format!(
                "DELETE FROM clash_item WHERE NOT {} OR NOT {}",
                exists_sql("clash_item.ka", "clash_item.ia"),
                exists_sql("clash_item.kb", "clash_item.ib")
            ),
            [],
        )?;
        let mut out = ClashMerge {
            run,
            ..Default::default()
        };
        let mut found: HashSet<(ElKey, ElKey)> = HashSet::new();
        for h in hits {
            let (a, b) = key_order(&h.a, &h.b);
            found.insert((a.clone(), b.clone()));
            let old: Option<String> = tx
                .query_row(
                    "SELECT status FROM clash_item \
                     WHERE test=?5 AND ka=?1 AND ia=?2 AND kb=?3 AND ib=?4",
                    params![a.0, a.1, b.0, b.1, test],
                    |r| r.get(0),
                )
                .optional()?;
            let status = match old.as_deref().and_then(ClashStatus::parse) {
                None => {
                    out.new += 1;
                    ClashStatus::New
                }
                Some(s) => {
                    out.still += 1;
                    match s {
                        ClashStatus::New | ClashStatus::Resolved => ClashStatus::Active,
                        s => s,
                    }
                }
            };
            tx.execute(
                "INSERT INTO clash_item(ka,ia,kb,ib,status,volume,lx,ly,lz,hx,hy,hz,level,present,\
                 first_run,last_run,test) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,1,?14,?14,?15) \
                 ON CONFLICT(test,ka,ia,kb,ib) DO UPDATE SET status=?5, volume=?6, lx=?7, ly=?8, \
                 lz=?9, hx=?10, hy=?11, hz=?12, level=?13, present=1, last_run=?14",
                params![
                    a.0,
                    a.1,
                    b.0,
                    b.1,
                    status.name(),
                    h.volume,
                    h.lo[0],
                    h.lo[1],
                    h.lo[2],
                    h.hi[0],
                    h.hi[1],
                    h.hi[2],
                    h.level,
                    run,
                    test
                ],
            )?;
        }
        let gone: Vec<(ElKey, ElKey, String)> = {
            let mut st = tx
                .prepare("SELECT ka,ia,kb,ib,status FROM clash_item WHERE present=1 AND test=?1")?;
            let rows = st.query_map([test], |r| {
                Ok((
                    (r.get::<_, String>(0)?, r.get::<_, i64>(1)?),
                    (r.get::<_, String>(2)?, r.get::<_, i64>(3)?),
                    r.get::<_, String>(4)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for (a, b, st) in gone {
            if found.contains(&(a.clone(), b.clone())) {
                continue;
            }
            let next = if st == ClashStatus::Approved.name() {
                st.as_str()
            } else {
                out.resolved += 1;
                ClashStatus::Resolved.name()
            };
            tx.execute(
                "UPDATE clash_item SET present=0, status=?5, last_run=?6 \
                 WHERE test=?7 AND ka=?1 AND ia=?2 AND kb=?3 AND ib=?4",
                params![a.0, a.1, b.0, b.1, next, run, test],
            )?;
        }
        tx.commit()?;
        Ok(out)
    }

    /// All stored pairs of all checks, ordered by check, category, id.
    pub fn clash_items(&self) -> Result<Vec<ClashItem>, Error> {
        self.clash_table()?;
        let mut st = self.db.prepare(
            "SELECT ka,ia,kb,ib,status,comment,volume,lx,ly,lz,hx,hy,hz,level,present,first_run,\
             last_run,test FROM clash_item ORDER BY test,ka,ia,kb,ib",
        )?;
        let rows = st.query_map([], |r| {
            Ok(ClashItem {
                test: r.get(17)?,
                a: (r.get(0)?, r.get(1)?),
                b: (r.get(2)?, r.get(3)?),
                status: ClashStatus::parse(&r.get::<_, String>(4)?).unwrap_or(ClashStatus::Active),
                comment: r.get(5)?,
                volume: r.get(6)?,
                lo: [r.get(7)?, r.get(8)?, r.get(9)?],
                hi: [r.get(10)?, r.get(11)?, r.get(12)?],
                level: r.get(13)?,
                present: r.get::<_, i64>(14)? != 0,
                first_run: r.get(15)?,
                last_run: r.get(16)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn clash_set_status(
        &self,
        test: &str,
        a: &ElKey,
        b: &ElKey,
        status: ClashStatus,
        comment: &str,
    ) -> Result<(), Error> {
        self.clash_table()?;
        let (a, b) = key_order(a, b);
        let n = self.db.execute(
            "UPDATE clash_item SET status=?5, comment=?6 \
             WHERE test=?7 AND ka=?1 AND ia=?2 AND kb=?3 AND ib=?4",
            params![a.0, a.1, b.0, b.1, status.name(), comment, test],
        )?;
        if n == 0 {
            return invalid("clash: no such pair");
        }
        Ok(())
    }

    /// Forgets all stored pairs.
    pub fn clash_clear(&self) -> Result<(), Error> {
        self.clash_table()?;
        self.db.execute("DELETE FROM clash_item", [])?;
        Ok(())
    }

    // ---- extents and fixes ----

    /// Extents of an element: a run or a structure.
    pub fn clash_element_bounds(&self, keys: &[ElKey]) -> Result<Vec<Option<Aabb>>, Error> {
        let mep: HashSet<i64> = keys.iter().filter(|k| is_mep(&k.0)).map(|k| k.1).collect();
        let runs: HashMap<i64, Aabb> = if mep.is_empty() {
            HashMap::new()
        } else {
            self.run_bodies(0.0, |id| mep.contains(&id))?
                .into_iter()
                .map(|(i, b)| (i, b.aabb))
                .collect()
        };
        let st: HashMap<ElKey, Aabb> = if keys.iter().any(|k| !is_mep(&k.0)) {
            let (ids, bodies) = self.structure_bodies()?;
            ids.into_iter()
                .zip(bodies)
                .map(|((k, i), b)| ((k.to_string(), i), b.aabb))
                .collect()
        } else {
            HashMap::new()
        };
        Ok(keys
            .iter()
            .map(|k| {
                if is_mep(&k.0) {
                    runs.get(&k.1).copied()
                } else {
                    st.get(k).copied()
                }
            })
            .collect())
    }

    /// Obstacle bypass: the run rises (`up`) over it or drops under it at 45° with clearance
    /// `gap`; affected segments get a "hump". Only for horizontal segments and routes
    /// without slope.
    pub fn clash_fix_reroute(
        &self,
        item: &ClashItem,
        up: bool,
        gap: f64,
    ) -> Result<ClashFix, Error> {
        let (run, other) = if is_mep(&item.a.0) {
            (&item.a, &item.b)
        } else if is_mep(&item.b.0) {
            (&item.b, &item.a)
        } else {
            return invalid("reroute: no network run in the pair");
        };
        let spec = self.mep_run(run.1)?;
        if spec.slope != 0.0 {
            return invalid("reroute: run with slope");
        }
        let ob = self
            .clash_element_bounds(std::slice::from_ref(other))?
            .remove(0)
            .ok_or_else(|| Error::Invalid("reroute: obstacle not found".into()))?;
        let (w, h) = spec.section.envelope().wh();
        let (hh, margin) = (h / 2.0, gap + w / 2.0);
        let (z_new, pts) = (
            if up {
                ob.max.z + gap + hh
            } else {
                ob.min.z - gap - hh
            },
            &spec.points,
        );
        let mut out = vec![pts[0]];
        let mut changed = false;
        for k in 0..pts.len() - 1 {
            let (p, q) = (pts[k], pts[k + 1]);
            let (dx, dy, dz) = (q.0 - p.0, q.1 - p.1, q.2 - p.2);
            let len = (dx * dx + dy * dy).sqrt();
            let flat = dz.abs() < 1e-6 && len > 1e-6;
            let vertical_overlap = p.2 + hh + gap >= ob.min.z && p.2 - hh - gap <= ob.max.z;
            let span = if flat && vertical_overlap {
                clip_rect(
                    (p.0, p.1),
                    (dx, dy),
                    (ob.min.x - margin, ob.min.y - margin),
                    (ob.max.x + margin, ob.max.y + margin),
                )
            } else {
                None
            };
            if let Some((t0, t1)) = span {
                let ramp = (z_new - p.2).abs();
                let at = |s: f64, z: f64| (p.0 + dx * s / len, p.1 + dy * s / len, z);
                let (s0, s1) = (t0 * len, t1 * len);
                let (r0, r1) = ((s0 - ramp).max(0.0), (s1 + ramp).min(len));
                for (s, z) in [(r0, p.2), (s0, z_new), (s1, z_new), (r1, q.2)] {
                    out.push(at(s, z));
                }
                changed = true;
            }
            out.push(q);
        }
        if !changed {
            return invalid("reroute: segment does not cross the obstacle");
        }
        let mut dedup: Vec<(f64, f64, f64)> = vec![];
        for p in out {
            if dedup
                .last()
                .is_none_or(|l| (l.0 - p.0).abs() + (l.1 - p.1).abs() + (l.2 - p.2).abs() > 1e-6)
            {
                dedup.push(p);
            }
        }
        let mut spec = spec;
        spec.points = dedup;
        Ok(ClashFix::Reroute { run: run.1, spec })
    }

    /// Sleeve: a "void" opening in a wall or a hole in a slab around the intersection, with clearance.
    pub fn clash_fix_sleeve(&self, item: &ClashItem, gap: f64) -> Result<ClashFix, Error> {
        let host = [&item.a, &item.b]
            .into_iter()
            .find(|k| k.0 == "wall" || k.0 == "slab")
            .ok_or_else(|| Error::Invalid("sleeve: only for walls and slabs".into()))?;
        if !(is_mep(&item.a.0) || is_mep(&item.b.0)) {
            return invalid("sleeve: no network run in the pair");
        }
        let (lo, hi) = (item.lo, item.hi);
        if host.0 == "wall" {
            let (ax, ay, bx, by, bulge, z0, wh): (f64, f64, f64, f64, f64, f64, f64) =
                self.db.query_row(
                    "SELECT w.ax,w.ay,w.bx,w.by,w.bulge,w.base_offset+l.elevation,w.height \
                     FROM wall w JOIN level l ON l.id=w.level WHERE w.id=?1",
                    [host.1],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                        ))
                    },
                )?;
            let (wa, wb) = (v2(ax, ay), v2(bx, by));
            let pts = super::arc_points(wa, wb, bulge);
            let l = super::arc_len(wa, wb, bulge);
            if l < 1e-6 {
                return invalid("sleeve: wall has zero length");
            }
            // `along` is the arc length; arc_points chords are equal, the parameter is by the nearest axis point
            let seg = l / (pts.len() - 1) as f64;
            let u = |x: f64, y: f64| -> f64 {
                let q = v2(x, y);
                let mut best = (f64::INFINITY, 0.0);
                for (k, w) in pts.windows(2).enumerate() {
                    let d = w[1] - w[0];
                    let dd = d.x * d.x + d.y * d.y;
                    let r = q - w[0];
                    let t = if dd > 1e-18 {
                        ((r.x * d.x + r.y * d.y) / dd).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let dist = (w[0] + d * t - q).len();
                    if dist < best.0 {
                        best = (dist, seg * (k as f64 + t));
                    }
                }
                best.1
            };
            let us = [
                u(lo[0], lo[1]),
                u(lo[0], hi[1]),
                u(hi[0], lo[1]),
                u(hi[0], hi[1]),
            ];
            let (u0, u1) = (
                us.iter().cloned().fold(f64::INFINITY, f64::min) - gap,
                us.iter().cloned().fold(f64::NEG_INFINITY, f64::max) + gap,
            );
            let (u0, u1) = (u0.max(0.0), u1.min(l));
            if u1 - u0 < 1e-3 {
                return invalid("sleeve: outside the wall");
            }
            let sill = (lo[2] - gap - z0).max(0.0);
            let top = (hi[2] + gap - z0).min(wh);
            if top - sill < 1e-3 {
                return invalid("sleeve: outside the wall height");
            }
            // overlapping an opening would subtract it from the wall body twice
            let overlap: i64 = self.db.query_row(
                "SELECT count(*) FROM opening WHERE wall=?1 AND along<?3 AND along+width>?2 \
                 AND sill<?5 AND sill+height>?4",
                params![host.1, u0, u1, sill, top],
                |r| r.get(0),
            )?;
            if overlap > 0 {
                return invalid("sleeve: overlaps an existing opening");
            }
            Ok(ClashFix::Opening {
                wall: host.1,
                spec: OpeningSpec {
                    along: u0,
                    width: u1 - u0,
                    sill,
                    height: top - sill,
                    kind: "void".into(),
                },
            })
        } else {
            let rings = self.slab_rings(host.1)?;
            let Some((outer, holes)) = rings.split_first() else {
                return invalid("sleeve: slab has no outline");
            };
            let (x0, y0, x1, y1) = (lo[0] - gap, lo[1] - gap, hi[0] + gap, hi[1] + gap);
            let mut holes = holes.to_vec();
            holes.push(vec![(x0, y0), (x0, y1), (x1, y1), (x1, y0)]);
            Ok(ClashFix::SlabHole {
                slab: host.1,
                outer: outer.clone(),
                holes,
            })
        }
    }

    /// Applies a fix: one journal operation, reverted by `undo`.
    pub fn apply_clash_fix(&mut self, fix: &ClashFix) -> Result<(), Error> {
        match fix {
            ClashFix::Reroute { run, spec } => self.update_mep_run(*run, spec),
            ClashFix::Opening { wall, spec } => self.add_opening(*wall, spec).map(|_| ()),
            ClashFix::SlabHole { slab, outer, holes } => {
                self.set_slab_boundary(*slab, outer, holes)
            }
        }
    }
}

/// Segment p + t·d, t ∈ [0,1], inside a rectangle: the interval (t0, t1) or None.
fn clip_rect(p: (f64, f64), d: (f64, f64), lo: (f64, f64), hi: (f64, f64)) -> Option<(f64, f64)> {
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, d, lo, hi) in [(p.0, d.0, lo.0, hi.0), (p.1, d.1, lo.1, hi.1)] {
        if d.abs() < 1e-12 {
            if p < lo || p > hi {
                return None;
            }
        } else {
            let (a, b) = ((lo - p) / d, (hi - p) / d);
            t0 = t0.max(a.min(b));
            t1 = t1.min(a.max(b));
        }
    }
    (t1 - t0 > 1e-9).then_some((t0, t1))
}
