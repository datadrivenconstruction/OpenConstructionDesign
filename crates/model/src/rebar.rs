//! Reinforcement (schema 30): bar types, sets attached to a host (column, beam, wall, slab,
//! isolated footing), cover layers per host face. Bars are not stored: they are built from the
//! host's current geometry in its coordinate system, so they move, rebuild and are trimmed
//! with it; deleting the host removes its sets in the same journal entry.
//!
//! Layouts: `long` is longitudinal bars (bottom, top, sides, corners, perimeter) inside stirrups,
//! `stirrup` is closed stirrups along a path, `area` is slab and wall meshes (two directions per
//! face), `free` is a segment in the host's axes, trimmed to the shell with cover.
//!
//! Meshes go around shafts, sketch openings and wall openings (with cover along their edges); walls
//! follow the top and bottom profiles. Shape dimensions in the schedule are over outer faces, length is along the axis.
//! Marks are stable: a mark number (shape, diameter, class, dimensions, hooks) is recorded in `rebar_mark`
//! by reinforcement operations; editing the host gives a new mark only to bars that changed, other
//! numbers do not shift; an automatic number never coincides with a user's own mark.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::f64::consts::PI;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use ddc_geom::member::{Member, Shape};
use ddc_geom::mesh::Mesh;
use ddc_geom::rebar::{self as rb, Bar, Hook, PathLayout, Prim2, ShapeCode};
use ddc_geom::{v2, v3, V2, V3};
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::ops::Inv;
use crate::{Error, Model};

const DDL: &str = "
CREATE TABLE IF NOT EXISTS rebar_set(id INTEGER PRIMARY KEY, host_kind TEXT NOT NULL,
    host INTEGER NOT NULL, layout TEXT NOT NULL, rtype INTEGER NOT NULL,
    shape TEXT NOT NULL DEFAULT '00', hook_a INTEGER NOT NULL DEFAULT 0,
    hook_b INTEGER NOT NULL DEFAULT 0, pos TEXT NOT NULL DEFAULT '',
    count INTEGER NOT NULL DEFAULT 0,
    rule TEXT NOT NULL DEFAULT 'max', spacing REAL NOT NULL DEFAULT 0.2,
    end_zone REAL NOT NULL DEFAULT 0, end_spacing REAL NOT NULL DEFAULT 0.1,
    leg REAL NOT NULL DEFAULT 0, angle REAL NOT NULL DEFAULT 0, rtype2 INTEGER,
    spacing2 REAL NOT NULL DEFAULT 0, ax REAL, ay REAL, az REAL, bx REAL, by REAL, bz REAL,
    mark TEXT);
CREATE INDEX IF NOT EXISTS rebar_set_host ON rebar_set(host_kind, host);
CREATE TABLE IF NOT EXISTS rebar_cover(id INTEGER PRIMARY KEY, host_kind TEXT NOT NULL,
    host INTEGER NOT NULL, face TEXT NOT NULL, cover REAL NOT NULL, UNIQUE(host_kind, host, face));
CREATE TABLE IF NOT EXISTS analytical_adj(id INTEGER PRIMARY KEY, kind TEXT NOT NULL,
    ref INTEGER NOT NULL, dax REAL NOT NULL DEFAULT 0, day REAL NOT NULL DEFAULT 0,
    daz REAL NOT NULL DEFAULT 0, dbx REAL NOT NULL DEFAULT 0, dby REAL NOT NULL DEFAULT 0,
    dbz REAL NOT NULL DEFAULT 0, rel_a TEXT, rel_b TEXT, UNIQUE(kind, ref));
CREATE TABLE IF NOT EXISTS rebar_mark(id INTEGER PRIMARY KEY, key TEXT NOT NULL UNIQUE,
    mark TEXT NOT NULL);
";

const TYPE_DDL: &str = "CREATE TABLE rebar_type(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    d REAL NOT NULL, grade TEXT NOT NULL, mandrel REAL NOT NULL DEFAULT 0,
    unit_mass REAL NOT NULL DEFAULT 0);";

/// Default bar range, mm.
const SEED_MM: [i64; 14] = [6, 8, 10, 12, 14, 16, 18, 20, 22, 25, 28, 32, 36, 40];
pub const DEFAULT_GRADE: &str = "B500";

/// Hosts: kind -> table.
pub const REBAR_HOSTS: [(&str, &str); 6] = [
    ("column", "col"),
    ("beam", "beam"),
    ("wall", "wall"),
    ("slab", "slab"),
    ("footing", "iso_footing"),
    ("raft", "raft"),
];

pub(crate) fn has_table(db: &Connection, name: &str) -> Result<bool, Error> {
    Ok(db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

/// Step 30: reinforcement and analytical-adjustment tables; the bar range only when the types table is created.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    if !has_table(db, "rebar_type")? {
        db.execute_batch(TYPE_DDL)?;
        for mm in SEED_MM {
            db.execute(
                "INSERT INTO rebar_type(name, d, grade) VALUES (?1, ?2, ?3)",
                params![
                    format!("Ø{mm} {DEFAULT_GRADE}"),
                    mm as f64 / 1000.0,
                    DEFAULT_GRADE
                ],
            )?;
        }
    }
    db.execute_batch(DDL)?;
    Ok(())
}

fn host_table(kind: &str) -> Option<(&'static str, &'static str)> {
    REBAR_HOSTS.into_iter().find(|h| h.0 == kind)
}

/// Linear host (bars along an axis: column, beam); the other known kinds are plates.
pub fn linear_host(kind: &str) -> bool {
    matches!(kind, "column" | "beam")
}

/// Whether a set of `layout` is accepted in a host of `kind` (the rule `add_rebar_sets` checks).
pub fn layout_fits(layout: &RebarLayout, kind: &str) -> bool {
    if host_table(kind).is_none() {
        return false;
    }
    match layout {
        RebarLayout::Long { .. } | RebarLayout::Stirrup { .. } => linear_host(kind),
        RebarLayout::Area { .. } => !linear_host(kind),
        _ => true,
    }
}

/// Set references to bar types: integrity and a ban on deleting a type in use.
pub(crate) const REFS: [(&str, &str, &str); 2] = [
    ("rebar_set", "rtype", "rebar_type"),
    ("rebar_set", "rtype2", "rebar_type"),
];

/// Sets and cover layers with an unknown host kind or no host (a reference by kind and id).
pub(crate) fn issues(db: &Connection) -> Result<Vec<crate::Issue>, Error> {
    let mut out = vec![];
    if !has_table(db, "rebar_set")? {
        return Ok(out);
    }
    for tab in ["rebar_set", "rebar_cover"] {
        let mut st = db.prepare(&format!(
            "SELECT id, host_kind, host FROM {tab} ORDER BY id"
        ))?;
        let rows: Vec<(i64, String, i64)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        for (id, k, h) in rows {
            let problem = match host_table(&k) {
                None => format!("unknown host kind '{k}'"),
                Some((_, t)) => {
                    if count(db, &format!("SELECT count(*) FROM {t} WHERE id=?1"), [h])? > 0 {
                        continue;
                    }
                    format!("host -> missing {t} {h}")
                }
            };
            out.push(crate::Issue {
                table: tab.into(),
                id,
                problem,
            });
        }
    }
    Ok(out)
}

/// File revision for assembly caches: grows with every connection write.
pub(crate) fn revision(db: &Connection) -> Result<i64, Error> {
    Ok(db.query_row("SELECT total_changes()", [], |r| r.get(0))?)
}

/// Default cover layers (m) by host kind.
pub fn default_covers(kind: &str) -> &'static [(&'static str, f64)] {
    match kind {
        "column" => &[("side", 0.04), ("bottom", 0.05), ("top", 0.04)],
        "beam" => &[
            ("top", 0.03),
            ("bottom", 0.03),
            ("side", 0.03),
            ("end", 0.03),
        ],
        "wall" => &[("face", 0.025), ("edge", 0.03)],
        "slab" => &[("top", 0.02), ("bottom", 0.025), ("edge", 0.025)],
        "footing" | "raft" => &[("top", 0.05), ("bottom", 0.05), ("edge", 0.05)],
        _ => &[],
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RebarType {
    pub id: i64,
    pub name: String,
    pub d: f64,
    pub grade: String,
    /// Mandrel diameter in d; 0 means by the diameter rule.
    pub mandrel: f64,
    /// Linear mass, kg/m; 0 means from the steel density.
    pub unit_mass: f64,
}

impl RebarType {
    pub fn bend_radius(&self) -> f64 {
        rb::bend_radius(self.d, self.mandrel)
    }

    pub fn mass_per_m(&self) -> f64 {
        if self.unit_mass > 0.0 {
            self.unit_mass
        } else {
            rb::unit_mass(self.d)
        }
    }
}

/// A row of longitudinal bars.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LongPos {
    Bottom,
    Top,
    /// On the side faces, without corner bars.
    Sides,
    Corners,
    /// Around the perimeter: `count` per face including corner bars (round section: in total).
    Perimeter,
}

impl LongPos {
    pub const ALL: [LongPos; 5] = [
        LongPos::Bottom,
        LongPos::Top,
        LongPos::Sides,
        LongPos::Corners,
        LongPos::Perimeter,
    ];

    pub fn name(self) -> &'static str {
        match self {
            LongPos::Bottom => "bottom",
            LongPos::Top => "top",
            LongPos::Sides => "sides",
            LongPos::Corners => "corners",
            LongPos::Perimeter => "perimeter",
        }
    }

    pub fn parse(s: &str) -> Option<LongPos> {
        LongPos::ALL.into_iter().find(|p| p.name() == s)
    }
}

/// Slab/wall faces with meshes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Faces {
    Both,
    Top,
    Bottom,
}

impl Faces {
    pub const ALL: [Faces; 3] = [Faces::Both, Faces::Top, Faces::Bottom];

    pub fn name(self) -> &'static str {
        match self {
            Faces::Both => "both",
            Faces::Top => "top",
            Faces::Bottom => "bottom",
        }
    }

    pub fn parse(s: &str) -> Option<Faces> {
        Faces::ALL.into_iter().find(|p| p.name() == s)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RebarLayout {
    /// Longitudinal bars of a column/beam; `leg` is the bend of shapes 11/21 (0 means the full core height).
    Long {
        pos: LongPos,
        count: usize,
        shape: ShapeCode,
        hooks: (Hook, Hook),
        leg: f64,
    },
    /// Closed stirrups (shape 51) with hooks `hook` at one corner.
    Stirrup { rule: PathLayout, hook: Hook },
    /// Mesh: main direction `angle` (outer layer), the second at +90 degrees (inner layer).
    Area {
        faces: Faces,
        angle: f64,
        spacing: f64,
        minor_type: i64,
        minor_spacing: f64,
    },
    /// A segment in the host's axes (column/beam: x along the axis from the start; slab: plan x, y from
    /// the host origin, z from the bottom).
    Free {
        a: (f64, f64, f64),
        b: (f64, f64, f64),
        hooks: (Hook, Hook),
    },
}

impl RebarLayout {
    pub fn name(&self) -> &'static str {
        match self {
            RebarLayout::Long { .. } => "long",
            RebarLayout::Stirrup { .. } => "stirrup",
            RebarLayout::Area { .. } => "area",
            RebarLayout::Free { .. } => "free",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RebarSetSpec {
    pub host_kind: String,
    pub host: i64,
    pub rtype: i64,
    pub layout: RebarLayout,
    /// A custom mark instead of the automatic one.
    pub mark: Option<String>,
}

/// A set's bar (world coordinates).
#[derive(Clone, Debug)]
pub struct RebarBar {
    pub set: i64,
    pub host_kind: &'static str,
    pub host: i64,
    pub level: i64,
    pub mark: String,
    pub rtype: i64,
    pub shape: ShapeCode,
    pub hooks: (Hook, Hook),
    /// Shape segment dimensions over outer faces (A, B, C...), as in the bending schedule.
    pub dims: Vec<f64>,
    pub d: f64,
    pub grade: String,
    /// Length along the axis including bends and hooks.
    pub length: f64,
    pub unit_mass: f64,
    /// A stirrup (transverse bar); otherwise a main bar.
    pub ligature: bool,
    pub bar: Bar,
}

impl RebarBar {
    pub fn weight(&self) -> f64 {
        self.length * self.unit_mass
    }
}

/// All bars of the model and set issues (set, issue code).
#[derive(Debug, Default)]
pub struct RebarBuilt {
    pub bars: Vec<RebarBar>,
    pub issues: Vec<(i64, &'static str)>,
    /// Set -> (level, fingerprint of its bars' geometry): views rebuild only the
    /// changed sets.
    pub sigs: BTreeMap<i64, (i64, u64)>,
    /// New marks not yet written to `rebar_mark`: (key, mark).
    fresh: Vec<(String, String)>,
}

/// Assembly caches until the next file change: bars and the analytical model.
#[derive(Default)]
pub(crate) struct Cache {
    built: Mutex<Option<(i64, Arc<RebarBuilt>)>>,
    pub(crate) analytical: Mutex<Option<(i64, Arc<crate::Analytical>)>>,
}

/// A bending schedule row (one mark).
#[derive(Clone, Debug, PartialEq)]
pub struct RebarRow {
    pub mark: String,
    pub shape: &'static str,
    pub d: f64,
    pub grade: String,
    /// "A=1000 B=300", mm.
    pub dims: String,
    /// "90/90", hook angles in degrees.
    pub hooks: String,
    pub count: usize,
    pub length: f64,
    pub total_length: f64,
    pub unit_mass: f64,
    pub weight: f64,
}

/// Row of the set schedule: the bars of one set summed.
#[derive(Clone, Debug, PartialEq)]
pub struct RebarSetRow {
    pub set: i64,
    pub host_kind: &'static str,
    pub host: i64,
    /// Distinct marks of the set, in order of first use.
    pub marks: Vec<String>,
    pub d: f64,
    pub grade: String,
    pub count: usize,
    pub total_length: f64,
    pub weight: f64,
}

/// A set's bars in plan: lines below the cut plane, points where it crosses them.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct RebarPlan {
    pub set: i64,
    pub lines: Vec<Vec<V2>>,
    pub dots: Vec<(V2, f64)>,
    /// Bars entirely above the cut plane: drawn as hidden lines, picked like the rest.
    pub hidden: Vec<Vec<V2>>,
}

// ---------------------------------------------------------------- storage

const SET_COLS: &str = "host_kind,host,layout,rtype,shape,hook_a,hook_b,pos,count,rule,spacing,\
    end_zone,end_spacing,leg,angle,rtype2,spacing2,ax,ay,az,bx,by,bz,mark";

fn hook(v: i64) -> Hook {
    Hook::from_deg(v).unwrap_or_default()
}

fn read_set(r: &Row) -> rusqlite::Result<(i64, RebarSetSpec)> {
    let layout: String = r.get(3)?;
    let shape: String = r.get(5)?;
    let (ha, hb) = (hook(r.get(6)?), hook(r.get(7)?));
    let pos: String = r.get(8)?;
    let count = r.get::<_, i64>(9)?.max(0) as usize;
    let rule: String = r.get(10)?;
    let (spacing, end_zone, end_spacing): (f64, f64, f64) = (r.get(11)?, r.get(12)?, r.get(13)?);
    let p3 = |i: usize| -> rusqlite::Result<(f64, f64, f64)> {
        Ok((
            r.get::<_, Option<f64>>(i)?.unwrap_or(0.0),
            r.get::<_, Option<f64>>(i + 1)?.unwrap_or(0.0),
            r.get::<_, Option<f64>>(i + 2)?.unwrap_or(0.0),
        ))
    };
    let l = match layout.as_str() {
        "stirrup" => RebarLayout::Stirrup {
            rule: match rule.as_str() {
                "fixed" => PathLayout::Fixed(count),
                "spacing" => PathLayout::Spacing(spacing),
                "zones" => PathLayout::Zones {
                    end: end_zone,
                    end_s: end_spacing,
                    mid_s: spacing,
                },
                _ => PathLayout::MaxSpacing(spacing),
            },
            hook: ha,
        },
        "area" => RebarLayout::Area {
            faces: Faces::parse(&pos).unwrap_or(Faces::Both),
            angle: r.get(15)?,
            spacing,
            minor_type: r.get::<_, Option<i64>>(16)?.unwrap_or(r.get(4)?),
            minor_spacing: r.get(17)?,
        },
        "free" => RebarLayout::Free {
            a: p3(18)?,
            b: p3(21)?,
            hooks: (ha, hb),
        },
        _ => RebarLayout::Long {
            pos: LongPos::parse(&pos).unwrap_or(LongPos::Bottom),
            count,
            shape: ShapeCode::parse(&shape).unwrap_or_default(),
            hooks: (ha, hb),
            leg: r.get(14)?,
        },
    };
    Ok((
        r.get(0)?,
        RebarSetSpec {
            host_kind: r.get(1)?,
            host: r.get(2)?,
            rtype: r.get(4)?,
            layout: l,
            mark: r.get(24)?,
        },
    ))
}

fn put(db: &Connection, id: Option<i64>, s: &RebarSetSpec) -> Result<i64, Error> {
    let none = None::<f64>;
    // shape, hook_a, hook_b, pos, count, rule, spacing, end_zone, end_spacing, leg, angle,
    // rtype2, spacing2, a, b
    type Cols = (
        &'static str,
        i64,
        i64,
        &'static str,
        i64,
        &'static str,
        f64,
        f64,
        f64,
        f64,
        f64,
        Option<i64>,
        f64,
        [Option<f64>; 6],
    );
    let c: Cols = match &s.layout {
        RebarLayout::Long {
            pos,
            count,
            shape,
            hooks,
            leg,
        } => (
            shape.code(),
            hooks.0.deg(),
            hooks.1.deg(),
            pos.name(),
            *count as i64,
            "max",
            0.2,
            0.0,
            0.1,
            *leg,
            0.0,
            None,
            0.0,
            [none; 6],
        ),
        RebarLayout::Stirrup { rule, hook } => {
            let (n, sp, ez, es) = match *rule {
                PathLayout::Fixed(n) => (n as i64, 0.2, 0.0, 0.1),
                PathLayout::MaxSpacing(s) | PathLayout::Spacing(s) => (0, s, 0.0, 0.1),
                PathLayout::Zones { end, end_s, mid_s } => (0, mid_s, end, end_s),
            };
            (
                ShapeCode::Stirrup.code(),
                hook.deg(),
                hook.deg(),
                "",
                n,
                rule.rule(),
                sp,
                ez,
                es,
                0.0,
                0.0,
                None,
                0.0,
                [none; 6],
            )
        }
        RebarLayout::Area {
            faces,
            angle,
            spacing,
            minor_type,
            minor_spacing,
        } => (
            "00",
            0,
            0,
            faces.name(),
            0,
            "max",
            *spacing,
            0.0,
            0.1,
            0.0,
            *angle,
            Some(*minor_type),
            *minor_spacing,
            [none; 6],
        ),
        RebarLayout::Free { a, b, hooks } => (
            "00",
            hooks.0.deg(),
            hooks.1.deg(),
            "",
            0,
            "max",
            0.2,
            0.0,
            0.1,
            0.0,
            0.0,
            None,
            0.0,
            [
                Some(a.0),
                Some(a.1),
                Some(a.2),
                Some(b.0),
                Some(b.1),
                Some(b.2),
            ],
        ),
    };
    db.execute(
        &format!(
            "INSERT OR REPLACE INTO rebar_set(id,{SET_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,\
             ?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25)"
        ),
        params![
            id,
            s.host_kind,
            s.host,
            s.layout.name(),
            s.rtype,
            c.0,
            c.1,
            c.2,
            c.3,
            c.4,
            c.5,
            c.6,
            c.7,
            c.8,
            c.9,
            c.10,
            c.11,
            c.12,
            c.13[0],
            c.13[1],
            c.13[2],
            c.13[3],
            c.13[4],
            c.13[5],
            s.mark.as_deref().map(str::trim).filter(|m| !m.is_empty()),
        ],
    )?;
    Ok(id.unwrap_or_else(|| db.last_insert_rowid()))
}

fn count(db: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<i64, Error> {
    Ok(db.query_row(sql, p, |r| r.get(0))?)
}

fn types(db: &Connection) -> Result<HashMap<i64, RebarType>, Error> {
    let mut st = db.prepare("SELECT id, name, d, grade, mandrel, unit_mass FROM rebar_type")?;
    let rows = st.query_map([], |r| {
        Ok(RebarType {
            id: r.get(0)?,
            name: r.get(1)?,
            d: r.get(2)?,
            grade: r.get(3)?,
            mandrel: r.get(4)?,
            unit_mass: r.get(5)?,
        })
    })?;
    let mut out = HashMap::new();
    for t in rows {
        let t = t?;
        out.insert(t.id, t);
    }
    Ok(out)
}

fn all_sets(db: &Connection) -> Result<Vec<(i64, RebarSetSpec)>, Error> {
    let mut st = db.prepare(&format!("SELECT id,{SET_COLS} FROM rebar_set ORDER BY id"))?;
    let rows = st.query_map([], read_set)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn fin(v: &[f64]) -> bool {
    v.iter().all(|x| x.is_finite())
}

fn check(db: &Connection, s: &RebarSetSpec) -> Result<(), Error> {
    let bad = |m: String| Err(Error::Invalid(m));
    let Some((kind, table)) = host_table(&s.host_kind) else {
        return bad(format!("rebar host kind '{}'", s.host_kind));
    };
    if count(
        db,
        &format!("SELECT count(*) FROM {table} WHERE id=?1"),
        [s.host],
    )? == 0
    {
        return bad(format!("rebar host {kind} {} not found", s.host));
    }
    let ty = |id: i64| count(db, "SELECT count(*) FROM rebar_type WHERE id=?1", [id]);
    if ty(s.rtype)? == 0 {
        return bad(format!("rebar type {} not found", s.rtype));
    }
    let linear = linear_host(kind);
    match &s.layout {
        RebarLayout::Long {
            count, shape, leg, ..
        } => {
            if !linear {
                return bad(format!(
                    "longitudinal bars need a column or beam, not {kind}"
                ));
            }
            if *count == 0 || *count > 200 || *shape == ShapeCode::Stirrup {
                return bad("longitudinal bars: count 1…200, shape 00/11/21".into());
            }
            if !fin(&[*leg]) || *leg < 0.0 {
                return bad("longitudinal bars: leg must be ≥ 0".into());
            }
        }
        RebarLayout::Stirrup { rule, .. } => {
            if !linear {
                return bad(format!("stirrups need a column or beam, not {kind}"));
            }
            if !rule.valid() || matches!(rule, PathLayout::Fixed(0)) {
                return bad("stirrups: bad spacing rule".into());
            }
        }
        RebarLayout::Area {
            angle,
            spacing,
            minor_type,
            minor_spacing,
            ..
        } => {
            if linear {
                return bad(format!(
                    "area reinforcement needs a wall, slab, footing or raft, not {kind}"
                ));
            }
            if !fin(&[*angle, *spacing, *minor_spacing]) || *spacing < 0.05 || *minor_spacing < 0.05
            {
                return bad("area reinforcement: spacing must be ≥ 50 mm".into());
            }
            if ty(*minor_type)? == 0 {
                return bad(format!("rebar type {minor_type} not found"));
            }
        }
        RebarLayout::Free { a, b, .. } => {
            let (a, b) = (v3(a.0, a.1, a.2), v3(b.0, b.1, b.2));
            if !fin(&[a.x, a.y, a.z, b.x, b.y, b.z]) || (b - a).len() < rb::MIN_BAR {
                return bad("free bar: ends must differ".into());
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- hosts

#[derive(Clone, Copy, Debug)]
struct Frame {
    o: V3,
    ex: V3,
    ey: V3,
    ez: V3,
}

impl Frame {
    fn at(&self, p: V3) -> V3 {
        self.o + self.ex * p.x + self.ey * p.y + self.ez * p.z
    }

    fn dir(&self, v: V3) -> V3 {
        self.ex * v.x + self.ey * v.y + self.ez * v.z
    }
}

#[derive(Clone, Copy, Debug)]
enum Sect {
    Rect { y0: f64, y1: f64, z0: f64, z1: f64 },
    Circle { cy: f64, cz: f64, r: f64 },
}

#[derive(Clone, Debug)]
enum Geo {
    /// Axis along x from 0 to `len`, section in (y, z).
    Linear { len: f64, sect: Sect, column: bool },
    /// Region in (x, y), thickness along z from 0 (bottom/left face) to `t`.
    Plate { rings: Vec<Vec<V2>>, t: f64 },
}

#[derive(Clone, Debug)]
struct Host {
    level: i64,
    f: Frame,
    geo: Geo,
    cov: HashMap<String, f64>,
    kind: &'static str,
}

impl Host {
    fn cover(&self, face: &str) -> f64 {
        self.cov.get(face).copied().unwrap_or_else(|| {
            default_covers(self.kind)
                .iter()
                .find(|c| c.0 == face)
                .map_or(0.03, |c| c.1)
        })
    }

    /// Column/beam: cover layers (start, end, y-, y+, z-, z+).
    fn lin_cov(&self, column: bool) -> [f64; 6] {
        if column {
            let s = self.cover("side");
            [self.cover("bottom"), self.cover("top"), s, s, s, s]
        } else {
            let (s, e) = (self.cover("side"), self.cover("end"));
            [e, e, s, s, self.cover("bottom"), self.cover("top")]
        }
    }

    /// Slab/wall/footing: (bottom, top, edges).
    fn plate_cov(&self) -> (f64, f64, f64) {
        if self.kind == "wall" {
            let f = self.cover("face");
            (f, f, self.cover("edge"))
        } else {
            (self.cover("bottom"), self.cover("top"), self.cover("edge"))
        }
    }
}

/// The bar section system is the same as for the element body.
fn member_frame(u: V3, rotation: f64) -> (V3, V3) {
    let z = v3(0.0, 0.0, 1.0);
    let h = z.cross(u);
    let ey = if h.len() < 1e-9 {
        v3(1.0, 0.0, 0.0)
    } else {
        h.normalized()
    };
    let ez = u.cross(ey);
    let (c, s) = (rotation.cos(), rotation.sin());
    (ey * c + ez * s, ez * c - ey * s)
}

fn linear(m: &Member, column: bool) -> Result<(Frame, Geo), &'static str> {
    let (a, b) = m.axis_ends().ok_or("host_shape")?;
    let len = (b - a).len();
    if len < rb::MIN_BAR {
        return Err("host_shape");
    }
    let ex = (b - a) * (1.0 / len);
    let (ey, ez) = member_frame(ex, m.rotation);
    let (lo, hi) = m.extents();
    let sect = match m.shape {
        Shape::Rect { .. } => Sect::Rect {
            y0: lo.x,
            y1: hi.x,
            z0: lo.y,
            z1: hi.y,
        },
        Shape::Circle { .. } => Sect::Circle {
            cy: (lo.x + hi.x) / 2.0,
            cz: (lo.y + hi.y) / 2.0,
            r: (hi.x - lo.x) / 2.0,
        },
        _ => return Err("host_shape"),
    };
    Ok((
        Frame { o: a, ex, ey, ez },
        Geo::Linear { len, sect, column },
    ))
}

fn covers_of(db: &Connection, kind: &str, id: i64) -> Result<HashMap<String, f64>, Error> {
    let mut st =
        db.prepare_cached("SELECT face, cover FROM rebar_cover WHERE host_kind=?1 AND host=?2")?;
    let rows = st.query_map(params![kind, id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

const Z: V3 = v3(0.0, 0.0, 1.0);

/// Polyline height (u, h) at point u: linear between vertices, beyond the ends the end value.
fn prof_at(p: &[(f64, f64)], u: f64) -> f64 {
    let Some(first) = p.first() else {
        return 0.0;
    };
    if u <= first.0 {
        return first.1;
    }
    for w in p.windows(2) {
        let ((u0, h0), (u1, h1)) = (w[0], w[1]);
        if u <= u1 {
            return if u1 - u0 < 1e-12 {
                h1
            } else {
                h0 + (h1 - h0) * (u - u0) / (u1 - u0)
            };
        }
    }
    p[p.len() - 1].1
}

type Prof<'a> = Option<&'a [(f64, f64)]>;

/// Wall elevation outline (u, z above the bottom): bottom by `base`, top by `top` (None means flat).
fn wall_ring(l: f64, height: f64, top: Prof, base: Prof) -> Vec<V2> {
    let at = |p: Prof, u: f64, flat: f64| p.map_or(flat, |p| prof_at(p, u));
    let inner = |p: Prof| -> Vec<V2> {
        p.unwrap_or_default()
            .iter()
            .filter(|q| q.0 > 1e-9 && q.0 < l - 1e-9)
            .map(|q| v2(q.0, q.1))
            .collect()
    };
    let mut r = vec![v2(0.0, at(base, 0.0, 0.0))];
    r.extend(inner(base));
    r.push(v2(l, at(base, l, 0.0)));
    r.push(v2(l, at(top, l, height)));
    r.extend(inner(top).into_iter().rev());
    r.push(v2(0.0, at(top, 0.0, height)));
    r.dedup_by(|a, b| (*a - *b).len() < 1e-9);
    r
}

fn wall_host(
    db: &Connection,
    sh: &mut crate::cuts::Shapes,
    id: i64,
) -> Result<Result<(i64, Frame, Geo), &'static str>, Error> {
    let row = db
        .query_row(
            "SELECT w.level, w.ax, w.ay, w.bx, w.by, w.bulge, w.base_offset, w.height, w.offset0, \
             (SELECT total(t.thickness) FROM wall_type_layer t WHERE t.wtype = w.wtype), \
             l.elevation, w.top_profile FROM wall w JOIN level l ON l.id = w.level \
             WHERE w.id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    v2(r.get(1)?, r.get(2)?),
                    v2(r.get(3)?, r.get(4)?),
                    r.get::<_, f64>(5)?,
                    r.get::<_, f64>(6)?,
                    r.get::<_, f64>(7)?,
                    r.get::<_, Option<f64>>(8)?,
                    r.get::<_, f64>(9)?,
                    r.get::<_, f64>(10)?,
                    r.get::<_, Option<String>>(11)?,
                ))
            },
        )
        .optional()?;
    let Some((level, a, b, bulge, base, height, o0, t, elev, top)) = row else {
        return Ok(Err("host_missing"));
    };
    if bulge.abs() > 1e-9 || t <= 0.0 || height <= 0.0 {
        return Ok(Err("host_shape"));
    }
    let center = o0.unwrap_or(-t / 2.0) + t / 2.0;
    let (a, b) = crate::footings::shift_left(a, b, 0.0, center);
    let l = (b - a).len();
    if l < rb::MIN_BAR {
        return Ok(Err("host_shape"));
    }
    let ex = v3((b.x - a.x) / l, (b.y - a.y) / l, 0.0);
    let ez = ex.cross(Z);
    let f = Frame {
        o: v3(a.x, a.y, elev + base) - ez * (t / 2.0),
        ex,
        ey: Z,
        ez,
    };
    let top = top.as_deref().and_then(crate::walls::parse_top_profile);
    let low = crate::cuts::base_profile(db, sh, id)?;
    let mut rings = vec![wall_ring(l, height, top.as_deref(), low.as_deref())];
    let mut st =
        db.prepare_cached("SELECT along, width, sill, height FROM opening WHERE wall=?1")?;
    let ops = st.query_map([id], |r| {
        Ok((
            r.get::<_, f64>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, f64>(3)?,
        ))
    })?;
    for o in ops {
        let (al, w, s, h) = o?;
        let (x0, x1) = (al.max(0.0), (al + w).min(l));
        let (y0, y1) = (s.max(0.0), (s + h).min(height));
        if x1 - x0 > 1e-6 && y1 - y0 > 1e-6 {
            rings.push(vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)]);
        }
    }
    Ok(Ok((level, f, Geo::Plate { rings, t })))
}

fn slab_host(db: &Connection, id: i64) -> Result<Result<(i64, Frame, Geo), &'static str>, Error> {
    let row = db
        .query_row(
            "SELECT s.level, s.offset, s.slope, \
             (SELECT total(t.thickness) FROM slab_type_layer t WHERE t.stype = s.stype), \
             l.elevation FROM slab s JOIN level l ON l.id = s.level WHERE s.id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((level, off, slope, t, elev)) = row else {
        return Ok(Err("host_missing"));
    };
    if slope.is_some_and(|s| !s.trim().is_empty()) || t <= 0.0 {
        return Ok(Err("host_shape"));
    }
    // plan after shafts and sketch openings: rings of pieces in a row (even-odd)
    let rings: Vec<Vec<V2>> = crate::cuts::slab_cut(db, id)?
        .map(|c| c.rings())
        .unwrap_or_default();
    let f = Frame {
        o: v3(0.0, 0.0, elev + off - t),
        ex: v3(1.0, 0.0, 0.0),
        ey: v3(0.0, 1.0, 0.0),
        ez: Z,
    };
    Ok(Ok((level, f, Geo::Plate { rings, t })))
}

/// Raft: plan outline in world coordinates, a plate of its thickness under the top offset.
fn raft_host(db: &Connection, id: i64) -> Result<Result<(i64, Frame, Geo), &'static str>, Error> {
    let row = db
        .query_row(
            "SELECT r.level, r.thickness, r.top_offset, l.elevation \
             FROM raft r JOIN level l ON l.id = r.level WHERE r.id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((level, t, top, elev)) = row else {
        return Ok(Err("host_missing"));
    };
    let mut st = db.prepare("SELECT x, y FROM raft_vertex WHERE raft = ?1 ORDER BY idx")?;
    let ring: Vec<V2> = st
        .query_map([id], |r| Ok(v2(r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)))?
        .collect::<Result<_, _>>()?;
    if ring.len() < 3 || t <= 0.0 {
        return Ok(Err("host_shape"));
    }
    let f = Frame {
        o: v3(0.0, 0.0, elev + top - t),
        ex: v3(1.0, 0.0, 0.0),
        ey: v3(0.0, 1.0, 0.0),
        ez: Z,
    };
    Ok(Ok((level, f, Geo::Plate { rings: vec![ring], t })))
}

fn footing_host(
    db: &Connection,
    id: i64,
) -> Result<Result<(i64, Frame, Geo), &'static str>, Error> {
    let row = db
        .query_row(
            "SELECT f.level, f.x, f.y, CASE WHEN f.col IS NULL THEN f.rotation ELSE f.frot END, \
             CASE WHEN f.col IS NULL THEN f.top_offset ELSE f.ftop END, f.round, f.steps, \
             l.elevation FROM iso_footing f JOIN level l ON l.id = f.level WHERE f.id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, bool>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, f64>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((level, x, y, rot, top, round, steps, elev)) = row else {
        return Ok(Err("host_missing"));
    };
    let Some(steps) = crate::footings::parse_steps(&steps).filter(|s| !s.is_empty()) else {
        return Ok(Err("host_shape"));
    };
    let depth: f64 = steps.iter().map(|s| s.h).sum();
    let s0 = &steps[0];
    let (c, s) = (rot.cos(), rot.sin());
    let f = Frame {
        o: v3(x, y, elev + top - depth),
        ex: v3(c, s, 0.0),
        ey: v3(-s, c, 0.0),
        ez: Z,
    };
    let ring = if round {
        let r = s0.w / 2.0;
        (0..32)
            .map(|k| {
                let a = 2.0 * PI * k as f64 / 32.0;
                v2(r * a.cos(), r * a.sin())
            })
            .collect()
    } else {
        let (hx, hy) = (s0.w / 2.0, s0.l / 2.0);
        vec![v2(-hx, -hy), v2(hx, -hy), v2(hx, hy), v2(-hx, hy)]
    };
    Ok(Ok((
        level,
        f,
        Geo::Plate {
            rings: vec![ring],
            t: s0.h,
        },
    )))
}

// ---------------------------------------------------------------- construction

struct Piece {
    bar: Bar,
    rtype: i64,
    shape: ShapeCode,
    hooks: (Hook, Hook),
    dims: Vec<f64>,
    ligature: bool,
}

fn even(lo: f64, hi: f64, n: usize) -> Vec<f64> {
    rb::positions(lo, hi.max(lo), PathLayout::Fixed(n))
}

/// Beyond this the set is not built (an input error, not a design).
const MAX_BARS: usize = 50_000;

#[allow(clippy::too_many_arguments)]
fn long_bars(
    h: &Host,
    len: f64,
    sect: Sect,
    column: bool,
    ds: f64,
    t: &RebarType,
    pos: LongPos,
    n: usize,
    shape: ShapeCode,
    hooks: (Hook, Hook),
    leg: f64,
) -> Result<Vec<Piece>, &'static str> {
    let d = t.d;
    let rc = t.bend_radius();
    let [x0c, x1c, y0c, y1c, z0c, z1c] = h.lin_cov(column);
    let bent_a = hooks.0 != Hook::None || shape == ShapeCode::U;
    let bent_b = hooks.1 != Hook::None || shape != ShapeCode::Straight;
    let x0 = x0c + if bent_a { d / 2.0 } else { 0.0 };
    let x1 = len - x1c - if bent_b { d / 2.0 } else { 0.0 };
    if x1 - x0 < rb::MIN_BAR {
        return Err("too_short");
    }
    // (y, z, inward direction, free space along it)
    let mut at: Vec<(f64, f64, f64, f64, f64)> = vec![];
    match sect {
        Sect::Rect { y0, y1, z0, z1 } => {
            let (yl, yh) = (y0 + y0c + ds + d / 2.0, y1 - y1c - ds - d / 2.0);
            let (zl, zh) = (z0 + z0c + ds + d / 2.0, z1 - z1c - ds - d / 2.0);
            if yh < yl - 1e-9 || zh < zl - 1e-9 {
                return Err("too_small");
            }
            let (ry, rz) = (yh - yl, zh - zl);
            let row = |z: f64, wz: f64, k: usize, at: &mut Vec<_>| {
                for y in even(yl, yh, k) {
                    at.push((y, z, 0.0, wz, rz));
                }
            };
            let sides = |k: usize, at: &mut Vec<_>| {
                let zs = even(zl, zh, k + 2);
                for z in zs.iter().skip(1).take(k) {
                    at.push((yl, *z, 1.0, 0.0, ry));
                    at.push((yh, *z, -1.0, 0.0, ry));
                }
            };
            match pos {
                LongPos::Bottom => row(zl, 1.0, n, &mut at),
                LongPos::Top => row(zh, -1.0, n, &mut at),
                LongPos::Sides => sides(n, &mut at),
                LongPos::Corners => {
                    for (y, z, wz) in [(yl, zl, 1.0), (yh, zl, 1.0), (yl, zh, -1.0), (yh, zh, -1.0)]
                    {
                        at.push((y, z, 0.0, wz, rz));
                    }
                }
                LongPos::Perimeter => {
                    let k = n.max(2);
                    row(zl, 1.0, k, &mut at);
                    row(zh, -1.0, k, &mut at);
                    sides(k - 2, &mut at);
                }
            }
        }
        Sect::Circle { cy, cz, r } => {
            let rr = r - y0c - ds - d / 2.0;
            if rr <= 0.0 {
                return Err("too_small");
            }
            let k = if matches!(pos, LongPos::Corners) {
                4
            } else {
                n
            };
            for j in 0..k {
                let a = 2.0 * PI * j as f64 / k as f64;
                at.push((
                    cy + rr * a.cos(),
                    cz + rr * a.sin(),
                    -a.cos(),
                    -a.sin(),
                    2.0 * rr,
                ));
            }
        }
    }
    let side_a = if shape == ShapeCode::U {
        v2(1.0, 0.0)
    } else {
        v2(0.0, 1.0)
    };
    let side_b = if shape == ShapeCode::Straight {
        v2(0.0, 1.0)
    } else {
        v2(-1.0, 0.0)
    };
    let mut out = Vec::with_capacity(at.len());
    for (y, z, wy, wz, room) in at {
        let l = if leg > 0.0 { leg.min(room) } else { room };
        let verts = match shape {
            ShapeCode::Straight => vec![v2(x0, 0.0), v2(x1, 0.0)],
            ShapeCode::L => vec![v2(x0, 0.0), v2(x1, 0.0), v2(x1, l)],
            ShapeCode::U => vec![v2(x0, l), v2(x0, 0.0), v2(x1, 0.0), v2(x1, l)],
            ShapeCode::Stirrup => return Err("shape"),
        };
        let path = rb::centerline(&verts, rc, d, hooks, side_a, side_b).map_err(|_| "too_short")?;
        out.push(Piece {
            bar: Bar {
                d,
                o: h.f.at(v3(0.0, y, z)),
                u: h.f.ex,
                v: h.f.dir(v3(0.0, wy, wz)),
                path,
            },
            rtype: t.id,
            shape,
            hooks,
            dims: rb::outer_dims(&verts, hooks, d),
            ligature: false,
        });
    }
    Ok(out)
}

fn stirrups(
    h: &Host,
    len: f64,
    sect: Sect,
    column: bool,
    t: &RebarType,
    rule: PathLayout,
    hk: Hook,
) -> Result<Vec<Piece>, &'static str> {
    let Sect::Rect { y0, y1, z0, z1 } = sect else {
        return Err("host_shape");
    };
    let d = t.d;
    let [x0c, x1c, y0c, y1c, z0c, z1c] = h.lin_cov(column);
    let (ya, yb) = (y0 + y0c + d / 2.0, y1 - y1c - d / 2.0);
    let (za, zb) = (z0 + z0c + d / 2.0, z1 - z1c - d / 2.0);
    let (a, b) = (yb - ya, zb - za);
    if a <= 0.0 || b <= 0.0 {
        return Err("too_small");
    }
    let verts = ShapeCode::Stirrup.verts(a, b, 0.0);
    let side = v2(a / 2.0, -b / 2.0);
    let path = rb::centerline(&verts, t.bend_radius(), d, (hk, hk), side, side)
        .map_err(|_| "too_short")?;
    let xs = rb::positions(x0c + d / 2.0, len - x1c - d / 2.0, rule);
    if xs.len() > MAX_BARS {
        return Err("too_many");
    }
    Ok(xs
        .into_iter()
        .map(|x| Piece {
            bar: Bar {
                d,
                o: h.f.at(v3(x, ya, za)),
                u: h.f.ey,
                v: h.f.ez,
                path: path.clone(),
            },
            rtype: t.id,
            shape: ShapeCode::Stirrup,
            hooks: (hk, hk),
            // outer dimensions of a closed stirrup: axis + d
            dims: vec![a + d, b + d],
            ligature: true,
        })
        .collect())
}

#[allow(clippy::too_many_arguments)]
fn area_bars(
    h: &Host,
    rings: &[Vec<V2>],
    t: f64,
    faces: Faces,
    angle: f64,
    major: &RebarType,
    spacing: f64,
    minor: &RebarType,
    spacing2: f64,
) -> Result<Vec<Piece>, &'static str> {
    let (cb, ct, ce) = h.plate_cov();
    let (d1, d2) = (major.d, minor.d);
    let need = match faces {
        Faces::Both => cb + ct + 2.0 * (d1 + d2),
        Faces::Top => ct + d1 + d2,
        Faces::Bottom => cb + d1 + d2,
    };
    if need > t + 1e-9 {
        return Err("too_small");
    }
    let mut layers = vec![];
    if faces != Faces::Top {
        layers.push((cb + d1 / 2.0, cb + d1 + d2 / 2.0));
    }
    if faces != Faces::Bottom {
        layers.push((t - ct - d1 / 2.0, t - ct - d1 - d2 / 2.0));
    }
    let l1 = rb::area_lines(rings, angle, spacing, ce, d1);
    let l2 = rb::area_lines(rings, angle + PI / 2.0, spacing2, ce, d2);
    if (l1.len() + l2.len()) * layers.len() > MAX_BARS {
        return Err("too_many");
    }
    let mut out = vec![];
    for (z1, z2) in layers {
        for (lines, z, ty) in [(&l1, z1, major), (&l2, z2, minor)] {
            for (p, q) in lines.iter() {
                let a = h.f.at(v3(p.x, p.y, z));
                let b = h.f.at(v3(q.x, q.y, z));
                out.push(Piece {
                    bar: Bar::straight(a, b, ty.d),
                    rtype: ty.id,
                    shape: ShapeCode::Straight,
                    hooks: (Hook::None, Hook::None),
                    dims: vec![(*q - *p).len()],
                    ligature: false,
                });
            }
        }
    }
    Ok(out)
}

/// A free bar: (piece, whether trimmed).
fn free_bar(
    h: &Host,
    t: &RebarType,
    a: V3,
    b: V3,
    hooks: (Hook, Hook),
) -> Result<(Piece, bool), &'static str> {
    let d = t.d;
    let (p, q, center) = match &h.geo {
        Geo::Linear { len, sect, column } => {
            let [x0c, x1c, y0c, y1c, z0c, z1c] = h.lin_cov(*column);
            let (lo, hi, c) = match *sect {
                Sect::Rect { y0, y1, z0, z1 } => (
                    v3(x0c, y0 + y0c + d / 2.0, z0 + z0c + d / 2.0),
                    v3(len - x1c, y1 - y1c - d / 2.0, z1 - z1c - d / 2.0),
                    v3(len / 2.0, (y0 + y1) / 2.0, (z0 + z1) / 2.0),
                ),
                Sect::Circle { cy, cz, r } => {
                    let k = (r - y0c - d / 2.0) / std::f64::consts::SQRT_2;
                    (
                        v3(x0c, cy - k, cz - k),
                        v3(len - x1c, cy + k, cz + k),
                        v3(len / 2.0, cy, cz),
                    )
                }
            };
            let (p, q) = rb::clip_box(a, b, lo, hi).ok_or("empty")?;
            (p, q, c)
        }
        Geo::Plate { rings, t: th } => {
            let (cb, ct, ce) = h.plate_cov();
            let (zl, zh) = (cb + d / 2.0, th - ct - d / 2.0);
            if zh < zl {
                return Err("too_small");
            }
            let (a2, b2) = (v2(a.x, a.y), v2(b.x, b.y));
            let (t0, t1) = if (b2 - a2).len() < 1e-9 {
                // a vertical bar: the plan point lies inside the region with cover
                if !rb::inside_region(a2, rings, ce) {
                    return Err("empty");
                }
                (0.0, 1.0)
            } else {
                rb::clip_region(a2, b2, rings, ce).ok_or("empty")?
            };
            let cz = |p: V3| v3(p.x, p.y, p.z.clamp(zl, zh));
            let (p, q) = (cz(a + (b - a) * t0), cz(a + (b - a) * t1));
            (p, q, v3((p.x + q.x) / 2.0, (p.y + q.y) / 2.0, th / 2.0))
        }
    };
    let clipped = (p - a).len() > 1e-3 || (q - b).len() > 1e-3;
    let l = (q - p).len();
    if l < rb::MIN_BAR {
        return Err("empty");
    }
    let dir = (q - p) * (1.0 / l);
    let s0 = if hooks.0 != Hook::None { d / 2.0 } else { 0.0 };
    let s1 = if hooks.1 != Hook::None { d / 2.0 } else { 0.0 };
    let (p, l) = (p + dir * s0, l - s0 - s1);
    let mid = p + dir * (l / 2.0);
    let w = center - mid;
    let w = w - dir * w.dot(dir);
    let vloc = if w.len() < 1e-9 {
        rb::any_perp(dir)
    } else {
        w.normalized()
    };
    let verts = [v2(0.0, 0.0), v2(l, 0.0)];
    let up = v2(0.0, 1.0);
    let path =
        rb::centerline(&verts, t.bend_radius(), d, hooks, up, up).map_err(|_| "too_short")?;
    Ok((
        Piece {
            bar: Bar {
                d,
                o: h.f.at(p),
                u: h.f.dir(dir),
                v: h.f.dir(vloc),
                path,
            },
            rtype: t.id,
            shape: ShapeCode::Straight,
            hooks,
            dims: rb::outer_dims(&verts, hooks, d),
            ligature: false,
        },
        clipped,
    ))
}

/// The bar axis leaves the host's concrete (a hook or bend longer than the section): the bar
/// surface is beyond the face by more than the tolerance.
fn sticks_out(h: &Host, bar: &Bar) -> bool {
    const TOL: f64 = 1e-4;
    let r = bar.d / 2.0;
    let loc = |p: V3| {
        let q = p - h.f.o;
        v3(q.dot(h.f.ex), q.dot(h.f.ey), q.dot(h.f.ez))
    };
    bar.points(bar.d)
        .into_iter()
        .map(loc)
        .any(|p| match &h.geo {
            Geo::Linear { len, sect, .. } => {
                let out_x = p.x < -TOL || p.x > len + TOL;
                out_x
                    || match *sect {
                        Sect::Rect { y0, y1, z0, z1 } => {
                            p.y < y0 + r - TOL
                                || p.y > y1 - r + TOL
                                || p.z < z0 + r - TOL
                                || p.z > z1 - r + TOL
                        }
                        Sect::Circle { cy, cz, r: rr } => {
                            ((p.y - cy).powi(2) + (p.z - cz).powi(2)).sqrt() > rr - r + TOL
                        }
                    }
            }
            Geo::Plate { rings, t } => {
                p.z < r - TOL || p.z > t - r + TOL || !rb::inside_region(v2(p.x, p.y), rings, 0.0)
            }
        })
}

use ddc_geom::to_mm as mm;

/// Mark key: diameter, class, shape, dimensions (to mm) and hooks; identical bars share it.
fn mark_key(p: &RebarBar) -> String {
    let dims: Vec<String> = p.dims.iter().map(|x| mm(*x).to_string()).collect();
    format!(
        "{}|{}|{}|{}|{}|{}",
        mm(p.d),
        p.grade,
        p.shape.code(),
        dims.join(","),
        p.hooks.0.deg(),
        p.hooks.1.deg()
    )
}

fn hash_bar(h: &mut DefaultHasher, b: &Bar) {
    let v = [
        b.d, b.o.x, b.o.y, b.o.z, b.u.x, b.u.y, b.u.z, b.v.x, b.v.y, b.v.z,
    ];
    for x in v {
        x.to_bits().hash(h);
    }
    for p in &b.path {
        let w: [f64; 6] = match *p {
            Prim2::Line(a, c) => [0.0, a.x, a.y, c.x, c.y, 0.0],
            Prim2::Arc { c, r, a0, sweep } => [1.0, c.x, c.y, r, a0, sweep],
        };
        for x in w {
            x.to_bits().hash(h);
        }
    }
}

/// Recorded marks: key -> mark.
fn registry(db: &Connection) -> Result<HashMap<String, String>, Error> {
    if !has_table(db, "rebar_mark")? {
        return Ok(HashMap::new());
    }
    let mut st = db.prepare_cached("SELECT key, mark FROM rebar_mark")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Bar marks: the set's own mark; otherwise the one recorded for the key; otherwise a new number,
/// the smallest free one not taken by recorded or custom marks (order is by sets and bars).
/// One mark on different geometry is a `mark_conflict` issue on its sets.
fn assign_marks(out: &mut RebarBuilt, own: Vec<Option<String>>, reg: &HashMap<String, String>) {
    let keys: Vec<String> = out.bars.iter().map(mark_key).collect();
    let mut taken: HashSet<String> = reg.values().cloned().collect();
    taken.extend(own.iter().flatten().cloned());
    let mut fresh: BTreeMap<String, String> = BTreeMap::new();
    let mut next = 1usize;
    for ((b, o), k) in out.bars.iter_mut().zip(own).zip(&keys) {
        b.mark = match o {
            Some(m) => m,
            None => match reg.get(k) {
                Some(m) => m.clone(),
                None => fresh
                    .entry(k.clone())
                    .or_insert_with(|| {
                        while taken.contains(&next.to_string()) {
                            next += 1;
                        }
                        taken.insert(next.to_string());
                        next.to_string()
                    })
                    .clone(),
            },
        };
    }
    out.fresh = fresh.into_iter().collect();
    let mut first: HashMap<&str, &str> = HashMap::new();
    let mut bad: HashSet<&str> = HashSet::new();
    for (b, k) in out.bars.iter().zip(&keys) {
        match first.get(b.mark.as_str()) {
            Some(k0) if *k0 != k.as_str() => {
                bad.insert(b.mark.as_str());
            }
            Some(_) => {}
            None => {
                first.insert(b.mark.as_str(), k.as_str());
            }
        }
    }
    let mut flagged: BTreeSet<i64> = BTreeSet::new();
    let mut issues = vec![];
    for b in &out.bars {
        if bad.contains(b.mark.as_str()) && flagged.insert(b.set) {
            issues.push((b.set, "mark_conflict"));
        }
    }
    out.issues.extend(issues);
}

pub(crate) fn build(db: &Connection) -> Result<RebarBuilt, Error> {
    let mut out = RebarBuilt::default();
    if !has_table(db, "rebar_set")? {
        return Ok(out);
    }
    let sets = all_sets(db)?;
    if sets.is_empty() {
        return Ok(out);
    }
    let ty = types(db)?;
    let members: HashMap<(&'static str, i64), crate::structure::Built> = if sets
        .iter()
        .any(|s| matches!(s.1.host_kind.as_str(), "column" | "beam"))
    {
        crate::structure::build_all(db)?
            .into_iter()
            .map(|b| ((b.kind, b.id), b))
            .collect()
    } else {
        HashMap::new()
    };
    let mut hosts: HashMap<(&'static str, i64), Result<Host, &'static str>> = HashMap::new();
    let mut shapes = crate::cuts::Shapes::default();
    for (_, s) in &sets {
        let Some((kind, _)) = host_table(&s.host_kind) else {
            continue;
        };
        if hosts.contains_key(&(kind, s.host)) {
            continue;
        }
        let g: Result<(i64, Frame, Geo), &'static str> = match kind {
            "column" | "beam" => match members.get(&(kind, s.host)) {
                Some(b) => linear(&b.m, kind == "column").map(|(f, g)| (b.level, f, g)),
                None => Err("host_missing"),
            },
            "wall" => wall_host(db, &mut shapes, s.host)?,
            "slab" => slab_host(db, s.host)?,
            "raft" => raft_host(db, s.host)?,
            _ => footing_host(db, s.host)?,
        };
        let h = match g {
            Ok((level, f, geo)) => Ok(Host {
                level,
                f,
                geo,
                cov: covers_of(db, kind, s.host)?,
                kind,
            }),
            Err(e) => Err(e),
        };
        hosts.insert((kind, s.host), h);
    }
    // longitudinal bars sit inside the host's thickest stirrups
    let mut ds: HashMap<(&'static str, i64), f64> = HashMap::new();
    for (_, s) in &sets {
        if let (RebarLayout::Stirrup { .. }, Some((kind, _)), Some(t)) =
            (&s.layout, host_table(&s.host_kind), ty.get(&s.rtype))
        {
            let e = ds.entry((kind, s.host)).or_insert(0.0);
            *e = e.max(t.d);
        }
    }
    let mut own: Vec<Option<String>> = vec![];
    for (id, s) in &sets {
        let Some((kind, _)) = host_table(&s.host_kind) else {
            continue;
        };
        let Some(t) = ty.get(&s.rtype) else {
            out.issues.push((*id, "type_missing"));
            continue;
        };
        let h = match hosts.get(&(kind, s.host)) {
            Some(Ok(h)) => h,
            Some(Err(e)) => {
                out.issues.push((*id, *e));
                continue;
            }
            None => continue,
        };
        let pieces: Result<Vec<Piece>, &'static str> = match (&s.layout, &h.geo) {
            (
                RebarLayout::Long {
                    pos,
                    count,
                    shape,
                    hooks,
                    leg,
                },
                Geo::Linear { len, sect, column },
            ) => long_bars(
                h,
                *len,
                *sect,
                *column,
                ds.get(&(kind, s.host)).copied().unwrap_or(0.0),
                t,
                *pos,
                *count,
                *shape,
                *hooks,
                *leg,
            ),
            (RebarLayout::Stirrup { rule, hook }, Geo::Linear { len, sect, column }) => {
                stirrups(h, *len, *sect, *column, t, *rule, *hook)
            }
            (
                RebarLayout::Area {
                    faces,
                    angle,
                    spacing,
                    minor_type,
                    minor_spacing,
                },
                Geo::Plate { rings, t: th },
            ) => match ty.get(minor_type) {
                Some(t2) => area_bars(
                    h,
                    rings,
                    *th,
                    *faces,
                    *angle,
                    t,
                    *spacing,
                    t2,
                    *minor_spacing,
                ),
                None => Err("type_missing"),
            },
            (RebarLayout::Free { a, b, hooks }, _) => {
                match free_bar(h, t, v3(a.0, a.1, a.2), v3(b.0, b.1, b.2), *hooks) {
                    Ok((p, clipped)) => {
                        if clipped {
                            out.issues.push((*id, "clipped"));
                        }
                        Ok(vec![p])
                    }
                    Err(e) => Err(e),
                }
            }
            _ => Err("host_shape"),
        };
        let pieces = match pieces {
            Ok(p) if p.is_empty() => {
                out.issues.push((*id, "empty"));
                continue;
            }
            Ok(p) => p,
            Err(e) => {
                out.issues.push((*id, e));
                continue;
            }
        };
        // meshes are built inside the outline; bent and hooked bars are checked for leaving the concrete
        let bent =
            |p: &Piece| p.shape != ShapeCode::Straight || p.hooks != (Hook::None, Hook::None);
        if pieces.iter().any(|p| bent(p) && sticks_out(h, &p.bar)) {
            out.issues.push((*id, "outside"));
        }
        for p in pieces {
            let pt = ty.get(&p.rtype).unwrap_or(t);
            let length = p.bar.length();
            let b = RebarBar {
                set: *id,
                host_kind: kind,
                host: s.host,
                level: h.level,
                mark: String::new(),
                rtype: p.rtype,
                shape: p.shape,
                hooks: p.hooks,
                dims: p.dims,
                d: pt.d,
                grade: pt.grade.clone(),
                length,
                unit_mass: pt.mass_per_m(),
                ligature: p.ligature,
                bar: p.bar,
            };
            own.push(s.mark.clone());
            out.bars.push(b);
        }
    }
    assign_marks(&mut out, own, &registry(db)?);
    let mut hs: BTreeMap<i64, (i64, DefaultHasher)> = BTreeMap::new();
    for b in &out.bars {
        let e = hs
            .entry(b.set)
            .or_insert_with(|| (b.level, DefaultHasher::new()));
        hash_bar(&mut e.1, &b.bar);
    }
    out.sigs = hs
        .into_iter()
        .map(|(k, (l, h))| (k, (l, h.finish())))
        .collect();
    Ok(out)
}

/// Record the new marks of the current assembly (in a reinforcement operation): they do not shift afterwards.
fn register_marks(db: &Connection) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    for (k, m) in build(db)?.fresh {
        let n = db.execute(
            "INSERT OR IGNORE INTO rebar_mark(key, mark) VALUES (?1, ?2)",
            params![k, m],
        )?;
        if n > 0 {
            inv = inv.delete("rebar_mark", "id", db.last_insert_rowid());
        }
    }
    Ok(inv)
}

/// True when the operation deleted a host row or wrote to the tables that point at hosts.
pub(crate) fn hosts_changed(t: &crate::changes::Touch) -> bool {
    t.touched(&["rebar_set", "rebar_cover", "analytical_adj"])
        || REBAR_HOSTS
            .iter()
            .map(|h| h.1)
            .chain(crate::analytical::ADJ_KINDS.iter().map(|h| h.1))
            .any(|tb| t.deleted(&[tb]))
}

/// Sets and cover layers of deleted hosts (and analytical adjustments of deleted elements):
/// snapshots into the operation entry, then deletion.
pub(crate) fn prune_hosts(db: &Connection) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    if !has_table(db, "rebar_set")? {
        return Ok(inv);
    }
    let mut gone =
        |tab: &str, kcol: &str, rcol: &str, kind: &str, table: &str| -> Result<(), Error> {
            let mut st = db.prepare_cached(&format!(
                "SELECT id FROM {tab} WHERE {kcol}=?1 AND {rcol} NOT IN (SELECT id FROM {table})"
            ))?;
            let ids: Vec<i64> = st
                .query_map([kind], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for id in ids {
                inv = std::mem::take(&mut inv).restore(db, tab, "id", id)?;
                db.execute(&format!("DELETE FROM {tab} WHERE id=?1"), [id])?;
            }
            Ok(())
        };
    for (kind, table) in REBAR_HOSTS {
        if has_table(db, table)? {
            gone("rebar_set", "host_kind", "host", kind, table)?;
            gone("rebar_cover", "host_kind", "host", kind, table)?;
        }
    }
    for (kind, table) in crate::analytical::ADJ_KINDS {
        if has_table(db, table)? {
            gone("analytical_adj", "kind", "ref", kind, table)?;
        }
    }
    Ok(inv)
}

fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<i64>(), b.parse::<i64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        _ => a.cmp(b),
    }
}

impl Model {
    /// Bars of the whole model from the hosts' current geometry (cached until the next file change).
    pub fn rebar_built(&self) -> Result<Arc<RebarBuilt>, Error> {
        let key = revision(&self.db)?;
        if let Ok(g) = self.rebar_cache.built.lock() {
            if let Some((k, b)) = &*g {
                if *k == key {
                    return Ok(b.clone());
                }
            }
        }
        let b = Arc::new(build(&self.db)?);
        if let Ok(mut g) = self.rebar_cache.built.lock() {
            *g = Some((key, b.clone()));
        }
        Ok(b)
    }

    /// Bars of a level (or of all).
    pub fn rebar_bars(&self, level: Option<i64>) -> Result<Vec<RebarBar>, Error> {
        Ok(self
            .rebar_built()?
            .bars
            .iter()
            .filter(|b| level.is_none_or(|l| b.level == l))
            .cloned()
            .collect())
    }

    pub fn rebar_types(&self) -> Result<Vec<RebarType>, Error> {
        let mut v: Vec<RebarType> = types(&self.db)?.into_values().collect();
        v.sort_by(|a, b| a.d.total_cmp(&b.d).then(a.name.cmp(&b.name)));
        Ok(v)
    }

    /// A new bar type; `mandrel` is the mandrel diameter in d (0 means by the rule).
    pub fn add_rebar_type(
        &mut self,
        name: &str,
        d: f64,
        grade: &str,
        mandrel: f64,
    ) -> Result<i64, Error> {
        let name = name.trim();
        if name.is_empty() || !fin(&[d, mandrel]) || !(0.003..=0.08).contains(&d) || mandrel < 0.0 {
            return Err(Error::Invalid(
                "rebar type: name, d 3…80 mm, mandrel ≥ 0".into(),
            ));
        }
        self.op("rebar_type_add", |db| {
            db.execute(
                "INSERT INTO rebar_type(name, d, grade, mandrel) VALUES (?1, ?2, ?3, ?4)",
                params![name, d, grade.trim(), mandrel],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("rebar_type", "id", id)))
        })
    }

    /// Sets in one operation (one undo step).
    pub fn add_rebar_sets(&mut self, ss: &[RebarSetSpec]) -> Result<Vec<i64>, Error> {
        for s in ss {
            check(&self.db, s)?;
        }
        self.op("rebar_add", |db| {
            let mut ids = vec![];
            let mut inv = Inv::default();
            for s in ss {
                let id = put(db, None, s)?;
                inv = inv.delete("rebar_set", "id", id);
                ids.push(id);
            }
            let inv = inv.then(register_marks(db)?);
            let first = ids.first().copied().unwrap_or(0);
            Ok((ids, first, inv))
        })
    }

    pub fn update_rebar_set(&mut self, id: i64, s: &RebarSetSpec) -> Result<(), Error> {
        check(&self.db, s)?;
        self.op("rebar_edit", |db| {
            if count(db, "SELECT count(*) FROM rebar_set WHERE id=?1", [id])? == 0 {
                return Err(Error::Invalid(format!("rebar set {id} not found")));
            }
            let inv = Inv::default().restore(db, "rebar_set", "id", id)?;
            put(db, Some(id), s)?;
            Ok(((), id, inv.then(register_marks(db)?)))
        })
    }

    pub fn rebar_set(&self, id: i64) -> Result<Option<RebarSetSpec>, Error> {
        Ok(self
            .db
            .query_row(
                &format!("SELECT id,{SET_COLS} FROM rebar_set WHERE id=?1"),
                [id],
                read_set,
            )
            .optional()?
            .map(|r| r.1))
    }

    /// Sets of a host (or of all hosts of the kind when `host` is None).
    pub fn rebar_sets_of(
        &self,
        kind: &str,
        host: Option<i64>,
    ) -> Result<Vec<(i64, RebarSetSpec)>, Error> {
        Ok(all_sets(&self.db)?
            .into_iter()
            .filter(|(_, s)| s.host_kind == kind && host.is_none_or(|h| s.host == h))
            .collect())
    }

    pub fn delete_rebar(&mut self, ids: &[i64]) -> Result<(), Error> {
        self.op("rebar_delete", |db| {
            let mut inv = Inv::default();
            for &id in ids {
                inv = inv.restore(db, "rebar_set", "id", id)?;
                db.execute("DELETE FROM rebar_set WHERE id=?1", [id])?;
            }
            Ok(((), ids.first().copied().unwrap_or(0), inv))
        })
    }

    /// All host sets in one operation; the number deleted.
    pub fn delete_host_rebar(&mut self, hosts: &[(String, i64)]) -> Result<usize, Error> {
        let mut ids = vec![];
        for (k, h) in hosts {
            ids.extend(self.rebar_sets_of(k, Some(*h))?.into_iter().map(|s| s.0));
        }
        if !ids.is_empty() {
            self.delete_rebar(&ids)?;
        }
        Ok(ids.len())
    }

    /// Cover layers of host faces (one operation); faces come from [`default_covers`].
    pub fn set_rebar_covers(
        &mut self,
        hosts: &[(String, i64)],
        covers: &[(String, f64)],
    ) -> Result<(), Error> {
        for (k, h) in hosts {
            let Some((kind, table)) = host_table(k) else {
                return Err(Error::Invalid(format!("rebar host kind '{k}'")));
            };
            if count(
                &self.db,
                &format!("SELECT count(*) FROM {table} WHERE id=?1"),
                [*h],
            )? == 0
            {
                return Err(Error::Invalid(format!("rebar host {kind} {h} not found")));
            }
            for (f, v) in covers {
                if !default_covers(kind).iter().any(|c| c.0 == f) {
                    return Err(Error::Invalid(format!("cover face '{f}' of {kind}")));
                }
                if !v.is_finite() || !(0.0..=0.5).contains(v) {
                    return Err(Error::Invalid("cover must be 0…500 mm".into()));
                }
            }
        }
        self.op("rebar_cover", |db| {
            let mut inv = Inv::default();
            for (k, h) in hosts {
                for (f, v) in covers {
                    let rid: Option<i64> = db
                        .query_row(
                            "SELECT id FROM rebar_cover WHERE host_kind=?1 AND host=?2 AND face=?3",
                            params![k, h, f],
                            |r| r.get(0),
                        )
                        .optional()?;
                    match rid {
                        Some(r) => {
                            inv = inv.restore(db, "rebar_cover", "id", r)?;
                            db.execute(
                                "UPDATE rebar_cover SET cover=?2 WHERE id=?1",
                                params![r, v],
                            )?;
                        }
                        None => {
                            db.execute(
                                "INSERT INTO rebar_cover(host_kind, host, face, cover) \
                                 VALUES (?1, ?2, ?3, ?4)",
                                params![k, h, f, v],
                            )?;
                            inv = inv.delete("rebar_cover", "id", db.last_insert_rowid());
                        }
                    }
                }
            }
            let inv = inv.then(register_marks(db)?);
            Ok(((), hosts.first().map_or(0, |h| h.1), inv))
        })
    }

    /// Record marks of bars that appeared after host edits (one operation): the numbers
    /// stop depending on further edits.
    pub fn freeze_rebar_marks(&mut self) -> Result<usize, Error> {
        let n = self.rebar_built()?.fresh.len();
        if n == 0 {
            return Ok(0);
        }
        self.op("rebar_marks", |db| Ok((n, 0, register_marks(db)?)))
    }

    /// A host's cover layers: the specified ones over the defaults.
    pub fn rebar_covers(&self, kind: &str, host: i64) -> Result<Vec<(&'static str, f64)>, Error> {
        let own = covers_of(&self.db, kind, host)?;
        Ok(default_covers(kind)
            .iter()
            .map(|(f, v)| (*f, own.get(*f).copied().unwrap_or(*v)))
            .collect())
    }

    /// Bar solids of a level by sets; `sides` is the tube facet count (None means by bar count).
    pub fn rebar_meshes(
        &self,
        level: Option<i64>,
        sides: Option<usize>,
    ) -> Result<Vec<(i64, Mesh)>, Error> {
        let b = self.rebar_built()?;
        let k = sides.unwrap_or_else(|| rb::lod_sides(b.bars.len()));
        let mut by: BTreeMap<i64, Vec<Mesh>> = BTreeMap::new();
        for bar in b.bars.iter().filter(|x| level.is_none_or(|l| x.level == l)) {
            by.entry(bar.set).or_default().push(bar.bar.mesh(k));
        }
        Ok(by
            .into_iter()
            .map(|(s, ms)| (s, ddc_geom::mep::merge(&ms)))
            .collect())
    }

    /// Bending schedule of a level (or the whole model): a row per mark and geometry (diameter, class, shape,
    /// outer dimensions to mm, hooks); a custom mark on different bars gives several rows.
    pub fn rebar_schedule(&self, level: Option<i64>) -> Result<Vec<RebarRow>, Error> {
        let b = self.rebar_built()?;
        let mut by: BTreeMap<(String, String), RebarRow> = BTreeMap::new();
        for x in b.bars.iter().filter(|x| level.is_none_or(|l| x.level == l)) {
            let names = ["A", "B", "C", "D", "E"];
            let key = (x.mark.clone(), mark_key(x));
            let r = by.entry(key).or_insert_with(|| RebarRow {
                mark: x.mark.clone(),
                shape: x.shape.code(),
                d: x.d,
                grade: x.grade.clone(),
                dims: x
                    .dims
                    .iter()
                    .zip(names)
                    .map(|(v, n)| format!("{n}={}", mm(*v)))
                    .collect::<Vec<_>>()
                    .join(" "),
                hooks: format!("{}/{}", x.hooks.0.deg(), x.hooks.1.deg()),
                count: 0,
                length: x.length,
                total_length: 0.0,
                unit_mass: x.unit_mass,
                weight: 0.0,
            });
            r.count += 1;
            r.total_length += x.length;
            r.weight += x.weight();
        }
        let mut v: Vec<RebarRow> = by.into_values().collect();
        for r in &mut v {
            r.length = r.total_length / r.count.max(1) as f64;
        }
        // stable sort: within a mark, key order
        v.sort_by(|a, b| natural(&a.mark, &b.mark));
        Ok(v)
    }

    /// One row per set of the level (all levels for None), sorted by host kind, host and set,
    /// with the bar count, total length and mass of its bars.
    pub fn rebar_set_schedule(&self, level: Option<i64>) -> Result<Vec<RebarSetRow>, Error> {
        let b = self.rebar_built()?;
        let mut by: BTreeMap<(&'static str, i64, i64), RebarSetRow> = BTreeMap::new();
        for x in b.bars.iter().filter(|x| level.is_none_or(|l| x.level == l)) {
            let r = by
                .entry((x.host_kind, x.host, x.set))
                .or_insert_with(|| RebarSetRow {
                    set: x.set,
                    host_kind: x.host_kind,
                    host: x.host,
                    marks: vec![],
                    d: x.d,
                    grade: x.grade.clone(),
                    count: 0,
                    total_length: 0.0,
                    weight: 0.0,
                });
            if !r.marks.contains(&x.mark) {
                r.marks.push(x.mark.clone());
            }
            r.count += 1;
            r.total_length += x.length;
            r.weight += x.weight();
        }
        Ok(by.into_values().collect())
    }

    /// Bars of a level in plan with cut plane `cut` above the level: those below are lines,
    /// those crossing are points.
    pub fn rebar_plan(&self, level: i64, cut: f64) -> Result<Vec<RebarPlan>, Error> {
        let elev: f64 = self
            .db
            .query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0.0);
        let cut = elev + cut;
        let b = self.rebar_built()?;
        let mut by: BTreeMap<i64, RebarPlan> = BTreeMap::new();
        for x in b.bars.iter().filter(|x| x.level == level) {
            let pts = x.bar.points(x.d);
            let (lo, hi) = pts
                .iter()
                .fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.z), b.max(p.z)));
            let e = by.entry(x.set).or_insert_with(|| RebarPlan {
                set: x.set,
                ..Default::default()
            });
            if lo > cut {
                e.hidden.push(pts.iter().map(|p| v2(p.x, p.y)).collect());
                continue;
            }
            if hi <= cut {
                e.lines.push(pts.iter().map(|p| v2(p.x, p.y)).collect());
                continue;
            }
            for w in pts.windows(2) {
                let (p, q) = (w[0], w[1]);
                if (p.z - cut) * (q.z - cut) <= 0.0 && (q.z - p.z).abs() > 1e-12 {
                    let t = (cut - p.z) / (q.z - p.z);
                    let c = p + (q - p) * t;
                    e.dots.push((v2(c.x, c.y), x.d / 2.0));
                }
            }
        }
        Ok(by.into_values().collect())
    }

    /// Set issues: (set, code): host_shape, host_missing, too_small, too_short, empty,
    /// clipped, too_many, type_missing, outside (a hook or bend leaves the concrete),
    /// mark_conflict (one mark on different bars).
    pub fn rebar_issues(&self) -> Result<Vec<(i64, &'static str)>, Error> {
        Ok(self.rebar_built()?.issues.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BeamJoin, BeamSpec, ColumnSpec, FootingSpec, FootingStep, Just, LayerSpec, ShaftSpec,
        SlabSpec, WallSpec,
    };

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
    }

    fn setup() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l0 = m.add_level("L0", 0.0).unwrap();
        let l1 = m.add_level("L1", 3.0).unwrap();
        (m, l0, l1)
    }

    fn rtype(m: &Model, mm: i64) -> RebarType {
        m.rebar_types()
            .unwrap()
            .into_iter()
            .find(|t| (t.d * 1000.0).round() as i64 == mm)
            .unwrap()
    }

    fn beam(m: &mut Model, l0: i64, b: (f64, f64, f64)) -> (i64, BeamSpec) {
        let t = m
            .add_member_type("RC 300x500", &Shape::Rect { b: 0.3, h: 0.5 }, "Concrete")
            .unwrap();
        let s = BeamSpec {
            level: l0,
            member_type: t,
            a: (0.0, 0.0, 3.0),
            b,
            bulge: 0.0,
            yjust: Just::Center,
            zjust: Just::Center,
            rotation: 0.0,
            start_ext: 0.0,
            end_ext: 0.0,
            join_start: BeamJoin::Auto,
            join_end: BeamJoin::Auto,
        };
        (m.add_beam(&s).unwrap(), s)
    }

    fn set(kind: &str, host: i64, rtype: i64, layout: RebarLayout) -> RebarSetSpec {
        RebarSetSpec {
            host_kind: kind.into(),
            host,
            rtype,
            layout,
            mark: None,
        }
    }

    #[test]
    fn schema_step_creates_seeded_types() {
        let (m, _, _) = setup();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        let t = m.rebar_types().unwrap();
        assert_eq!(t.len(), SEED_MM.len());
        assert!(t.iter().all(|t| t.grade == DEFAULT_GRADE));
    }

    #[test]
    fn beam_stirrups_count_length_and_follow_host_with_undo() {
        let (mut m, l0, _) = setup();
        let (bid, mut bs) = beam(&mut m, l0, (6.0, 0.0, 3.0));
        let t8 = rtype(&m, 8);
        let ids = m
            .add_rebar_sets(&[set(
                "beam",
                bid,
                t8.id,
                RebarLayout::Stirrup {
                    rule: PathLayout::MaxSpacing(0.2),
                    hook: Hook::H135,
                },
            )])
            .unwrap();
        let bars = m.rebar_bars(None).unwrap();
        // span 6 − 2·(0.03 + 0.004) = 5.932 → ⌈29.66⌉ + 1
        assert_eq!(bars.len(), 31);
        let (a, b) = (0.3 - 0.06 - 0.008, 0.5 - 0.06 - 0.008);
        let rc = (4.0 * 0.008 + 0.008) / 2.0;
        let want = 2.0 * a + 2.0 * b - 8.0 * rc + 3.0 * PI * rc + 2.0 * 0.075;
        for x in &bars {
            close(x.length, want);
            assert!(x.ligature);
            assert_eq!(x.mark, "1");
            assert_eq!(x.set, ids[0]);
        }
        close(bars[0].bar.o.x, 0.034);
        // the beam grows: stirrups regenerate along the new span
        bs.b = (8.0, 0.0, 3.0);
        m.update_beam(bid, &bs).unwrap();
        assert_eq!(m.rebar_bars(None).unwrap().len(), 41);
        m.undo().unwrap();
        assert_eq!(m.rebar_bars(None).unwrap().len(), 31);
        // undo of the set itself
        m.undo().unwrap();
        assert!(m.rebar_bars(None).unwrap().is_empty());
        assert!(m.rebar_sets_of("beam", Some(bid)).unwrap().is_empty());
    }

    #[test]
    fn hooked_bottom_bars_sit_inside_stirrups_and_covers_regenerate() {
        let (mut m, l0, _) = setup();
        let (bid, _) = beam(&mut m, l0, (6.0, 0.0, 3.0));
        let (t8, t16) = (rtype(&m, 8), rtype(&m, 16));
        m.add_rebar_sets(&[
            set(
                "beam",
                bid,
                t8.id,
                RebarLayout::Stirrup {
                    rule: PathLayout::MaxSpacing(0.2),
                    hook: Hook::H135,
                },
            ),
            set(
                "beam",
                bid,
                t16.id,
                RebarLayout::Long {
                    pos: LongPos::Bottom,
                    count: 3,
                    shape: ShapeCode::Straight,
                    hooks: (Hook::H90, Hook::H90),
                    leg: 0.0,
                },
            ),
        ])
        .unwrap();
        let long: Vec<RebarBar> = m
            .rebar_bars(None)
            .unwrap()
            .into_iter()
            .filter(|b| !b.ligature)
            .collect();
        assert_eq!(long.len(), 3);
        let rc = (4.0 * 0.016 + 0.016) / 2.0;
        let span = 6.0 - 2.0 * (0.03 + 0.008);
        let want = span - 2.0 * rc + PI * rc + 2.0 * 12.0 * 0.016;
        for b in &long {
            close(b.length, want);
            // outer dimension: both ends hooked → + d
            close(b.dims[0], span + 0.016);
        }
        // outer bars: section width − 2·(cover + stirrup + d/2)
        close(
            (long[2].bar.o - long[0].bar.o).len(),
            0.3 - 2.0 * (0.03 + 0.008 + 0.008),
        );
        let sched = m.rebar_schedule(None).unwrap();
        assert_eq!(sched.len(), 2);
        let lr = sched.iter().find(|r| r.shape == "00").unwrap();
        assert_eq!(lr.count, 3);
        close(lr.weight, 3.0 * want * rb::unit_mass(0.016));
        assert_eq!(lr.hooks, "90/90");
        // a thicker side cover shrinks stirrups and pulls the bars in; undo restores
        m.set_rebar_covers(&[("beam".into(), bid)], &[("side".into(), 0.05)])
            .unwrap();
        let st = m
            .rebar_bars(None)
            .unwrap()
            .into_iter()
            .find(|b| b.ligature)
            .unwrap();
        close(st.dims[0], 0.3 - 0.1);
        assert_eq!(m.rebar_covers("beam", bid).unwrap()[2], ("side", 0.05));
        m.undo().unwrap();
        let st = m
            .rebar_bars(None)
            .unwrap()
            .into_iter()
            .find(|b| b.ligature)
            .unwrap();
        close(st.dims[0], 0.3 - 0.06);
    }

    #[test]
    fn deleting_the_host_drops_its_rebar_in_the_same_undo_step() {
        let (mut m, l0, _) = setup();
        let (bid, _) = beam(&mut m, l0, (6.0, 0.0, 3.0));
        let t8 = rtype(&m, 8);
        m.add_rebar_sets(&[set(
            "beam",
            bid,
            t8.id,
            RebarLayout::Stirrup {
                rule: PathLayout::Fixed(5),
                hook: Hook::H135,
            },
        )])
        .unwrap();
        m.set_rebar_covers(&[("beam".into(), bid)], &[("end".into(), 0.05)])
            .unwrap();
        m.delete_beam(bid).unwrap();
        assert!(m.rebar_sets_of("beam", None).unwrap().is_empty());
        assert!(m.rebar_bars(None).unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.rebar_bars(None).unwrap().len(), 5);
        assert_eq!(m.rebar_covers("beam", bid).unwrap()[3], ("end", 0.05));
    }

    #[test]
    fn slab_area_reinforcement_two_faces_reflows_with_the_outline() {
        let (mut m, l0, _) = setup();
        let st = m
            .add_slab_type(
                "RC200",
                &[LayerSpec {
                    material: "Concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let sid = m
            .add_slab(&SlabSpec {
                level: l0,
                slab_type: st,
                offset: 0.0,
                outer: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)],
                holes: vec![],
            })
            .unwrap();
        let t10 = rtype(&m, 10);
        m.add_rebar_sets(&[set(
            "slab",
            sid,
            t10.id,
            RebarLayout::Area {
                faces: Faces::Both,
                angle: 0.0,
                spacing: 0.2,
                minor_type: t10.id,
                minor_spacing: 0.2,
            },
        )])
        .unwrap();
        let bars = m.rebar_bars(Some(l0)).unwrap();
        // x bars: offsets 0.03…2.97 → 16; y bars: 0.03…3.97 → 21; two faces
        assert_eq!(bars.len(), 2 * (16 + 21));
        let top_x: Vec<&RebarBar> = bars
            .iter()
            .filter(|b| b.bar.o.z > -0.1 && (b.bar.u.x - 1.0).abs() < 1e-9)
            .collect();
        assert_eq!(top_x.len(), 16);
        for b in &top_x {
            close(b.length, 4.0 - 0.05);
            close(b.bar.o.z, -0.02 - 0.005);
        }
        m.set_slab_boundary(sid, &[(0.0, 0.0), (6.0, 0.0), (6.0, 3.0), (0.0, 3.0)], &[])
            .unwrap();
        let bars = m.rebar_bars(Some(l0)).unwrap();
        assert_eq!(bars.len(), 2 * (16 + 31));
        assert!(bars
            .iter()
            .filter(|b| (b.bar.u.x - 1.0).abs() < 1e-9)
            .all(|b| (b.length - 5.95).abs() < 1e-6));
        // plan: all bars lie below a cut 1 m above the level
        let plan = m.rebar_plan(l0, 1.0).unwrap();
        assert_eq!(
            plan.iter().map(|p| p.lines.len()).sum::<usize>(),
            bars.len()
        );
    }

    #[test]
    fn free_bar_is_clipped_to_the_column_envelope_and_dotted_in_plan() {
        let (mut m, l0, l1) = setup();
        let t = m
            .add_member_type("RC 400", &Shape::Rect { b: 0.4, h: 0.4 }, "Concrete")
            .unwrap();
        let cid = m
            .add_column(&ColumnSpec {
                level: l0,
                top_level: Some(l1),
                member_type: t,
                structural: true,
                at: (1.0, 1.0),
                base_offset: 0.0,
                top_offset: 0.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap();
        let t20 = rtype(&m, 20);
        let sid = m
            .add_rebar_sets(&[set(
                "column",
                cid,
                t20.id,
                RebarLayout::Free {
                    a: (-1.0, 0.0, 0.0),
                    b: (5.0, 0.0, 0.0),
                    hooks: (Hook::None, Hook::None),
                },
            )])
            .unwrap()[0];
        let bars = m.rebar_bars(None).unwrap();
        assert_eq!(bars.len(), 1);
        close(bars[0].length, 3.0 - 0.05 - 0.04);
        close(bars[0].bar.o.z, 0.05);
        assert!(m.rebar_issues().unwrap().contains(&(sid, "clipped")));
        let plan = m.rebar_plan(l0, 1.2).unwrap();
        assert_eq!(plan[0].dots.len(), 1);
        let (p, r) = plan[0].dots[0];
        assert!((p - v2(1.0, 1.0)).len() < 1e-6);
        close(r, 0.01);
        // perimeter bars of a square column: 3 per face → 8
        m.add_rebar_sets(&[set(
            "column",
            cid,
            t20.id,
            RebarLayout::Long {
                pos: LongPos::Perimeter,
                count: 3,
                shape: ShapeCode::Straight,
                hooks: (Hook::None, Hook::None),
                leg: 0.0,
            },
        )])
        .unwrap();
        assert_eq!(m.rebar_bars(None).unwrap().len(), 9);
        // a set on the wrong host kind is refused
        assert!(m
            .add_rebar_sets(&[set(
                "column",
                cid,
                t20.id,
                RebarLayout::Area {
                    faces: Faces::Both,
                    angle: 0.0,
                    spacing: 0.2,
                    minor_type: t20.id,
                    minor_spacing: 0.2,
                },
            )])
            .is_err());
    }

    #[test]
    fn footing_bottom_mat() {
        let (mut m, l0, _) = setup();
        let fid = m
            .add_footing(&FootingSpec {
                level: l0,
                column: None,
                at: (0.0, 0.0),
                rotation: 0.0,
                round: false,
                steps: vec![FootingStep {
                    w: 2.0,
                    l: 2.0,
                    h: 0.5,
                }],
                top_offset: -0.5,
                material: "Concrete".into(),
            })
            .unwrap();
        let t12 = rtype(&m, 12);
        m.add_rebar_sets(&[set(
            "footing",
            fid,
            t12.id,
            RebarLayout::Area {
                faces: Faces::Bottom,
                angle: 0.0,
                spacing: 0.2,
                minor_type: t12.id,
                minor_spacing: 0.2,
            },
        )])
        .unwrap();
        let bars = m.rebar_bars(None).unwrap();
        // offsets −0.944…0.944 → ⌈9.44⌉ + 1 per direction
        assert_eq!(bars.len(), 22);
        for b in &bars {
            close(b.length, 2.0 - 0.1);
            assert!(b.bar.o.z < -0.9);
        }
    }

    fn slab(m: &mut Model, l0: i64, holes: Vec<Vec<(f64, f64)>>) -> i64 {
        let st = m
            .add_slab_type(
                "RC200",
                &[LayerSpec {
                    material: "Concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        m.add_slab(&SlabSpec {
            level: l0,
            slab_type: st,
            offset: 0.0,
            outer: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
            holes,
        })
        .unwrap()
    }

    fn mat(t: i64, faces: Faces) -> RebarLayout {
        RebarLayout::Area {
            faces,
            angle: 0.0,
            spacing: 0.2,
            minor_type: t,
            minor_spacing: 0.2,
        }
    }

    fn marks_of(m: &Model, host: i64) -> BTreeSet<String> {
        m.rebar_bars(None)
            .unwrap()
            .into_iter()
            .filter(|b| b.host == host)
            .map(|b| b.mark)
            .collect()
    }

    fn one(s: &str) -> BTreeSet<String> {
        BTreeSet::from([s.to_string()])
    }

    #[test]
    fn slab_mats_skip_a_shaft_with_edge_cover() {
        let (mut m, l0, l1) = setup();
        let sid = slab(&mut m, l0, vec![]);
        let t10 = rtype(&m, 10);
        m.add_rebar_sets(&[set("slab", sid, t10.id, mat(t10.id, Faces::Bottom))])
            .unwrap();
        let full: f64 = m.rebar_bars(None).unwrap().iter().map(|b| b.length).sum();
        m.add_shaft(&ShaftSpec {
            level: l0,
            top_level: l1,
            base_offset: -1.0,
            top_offset: 0.0,
            ring: vec![(2.0, 1.0), (4.0, 1.0), (4.0, 3.0), (2.0, 3.0)],
        })
        .unwrap();
        let bars = m.rebar_bars(None).unwrap();
        let cut: f64 = bars.iter().map(|b| b.length).sum();
        assert!(cut < full - 1.0, "{cut} vs {full}");
        // edge cover 25 mm + d/2: no bar in the shaft or along its faces closer than that
        let (c, r) = (0.025, 0.03);
        for b in &bars {
            let (p, q) = b.bar.ends().unwrap();
            let x_bar = (q.x - p.x).abs() > (q.y - p.y).abs();
            let (w, lo, hi) = if x_bar {
                (p.y, p.x.min(q.x), p.x.max(q.x))
            } else {
                (p.x, p.y.min(q.y), p.y.max(q.y))
            };
            let (b0, b1, s0, s1) = if x_bar {
                (1.0, 3.0, 2.0, 4.0)
            } else {
                (2.0, 4.0, 1.0, 3.0)
            };
            if w > b0 - r + 1e-9 && w < b1 + r - 1e-9 {
                assert!(hi <= s0 - c + 1e-6 || lo >= s1 + c - 1e-6, "{p:?} {q:?}");
            }
        }
    }

    #[test]
    fn wall_mesh_stays_below_a_sloped_top() {
        let (mut m, l0, _) = setup();
        let wt = m
            .add_wall_type(
                "RC200",
                &[LayerSpec {
                    material: "Concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let wid = m
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (4.0, 0.0),
                level: l0,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        m.set_wall_top_profile(wid, Some(&[(0.0, 3.0), (4.0, 2.0)][..]))
            .unwrap();
        let t12 = rtype(&m, 12);
        m.add_rebar_sets(&[set("wall", wid, t12.id, mat(t12.id, Faces::Both))])
            .unwrap();
        let bars = m.rebar_bars(None).unwrap();
        assert!(!bars.is_empty());
        for b in &bars {
            let (p, q) = b.bar.ends().unwrap();
            for e in [p, q] {
                assert!(e.z <= 3.0 - e.x / 4.0 - 0.03 + 1e-6, "{e:?}");
            }
        }
        // the vertical bar near the low end is shorter than the one near the high end
        let mut vert: Vec<(f64, f64)> = bars
            .iter()
            .filter_map(|b| {
                let (p, q) = b.bar.ends()?;
                ((q.z - p.z).abs() > 0.5).then_some((p.x, b.length))
            })
            .collect();
        vert.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (hi, lo) = (vert[0], vert[vert.len() - 1]);
        assert!(lo.1 < hi.1 - 0.5, "{lo:?} {hi:?}");
    }

    #[test]
    fn vertical_free_bar_in_a_slab_opening_is_not_built() {
        let (mut m, l0, _) = setup();
        let hole = vec![(1.5, 1.0), (2.5, 1.0), (2.5, 2.0), (1.5, 2.0)];
        let sid = slab(&mut m, l0, vec![hole]);
        let t12 = rtype(&m, 12);
        let free = |x: f64, y: f64| RebarLayout::Free {
            a: (x, y, 0.0),
            b: (x, y, 0.2),
            hooks: (Hook::None, Hook::None),
        };
        let ids = m
            .add_rebar_sets(&[
                set("slab", sid, t12.id, free(2.0, 1.5)),
                set("slab", sid, t12.id, free(0.5, 0.5)),
            ])
            .unwrap();
        let iss = m.rebar_issues().unwrap();
        assert!(iss.contains(&(ids[0], "empty")));
        let bars = m.rebar_bars(None).unwrap();
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].set, ids[1]);
        // covers: bottom 25, top 20, plus d/2 at each end
        close(bars[0].length, 0.2 - 0.025 - 0.02 - 0.012);
    }

    #[test]
    fn hooks_longer_than_the_section_are_reported() {
        let (mut m, l0, _) = setup();
        let (bid, _) = beam(&mut m, l0, (6.0, 0.0, 3.0));
        let hooked = RebarLayout::Long {
            pos: LongPos::Bottom,
            count: 2,
            shape: ShapeCode::Straight,
            hooks: (Hook::H90, Hook::H90),
            leg: 0.0,
        };
        let (t32, t16) = (rtype(&m, 32), rtype(&m, 16));
        let ids = m
            .add_rebar_sets(&[
                set("beam", bid, t32.id, hooked.clone()),
                set("beam", bid, t16.id, hooked),
            ])
            .unwrap();
        let iss = m.rebar_issues().unwrap();
        // 32 mm: bend radius 128 mm + 12d tail reach past the top face of a 500 mm beam
        assert!(iss.contains(&(ids[0], "outside")));
        assert!(!iss.contains(&(ids[1], "outside")));
    }

    #[test]
    fn marks_stay_put_when_another_host_changes() {
        let (mut m, l0, _) = setup();
        let (b1, mut s1) = beam(&mut m, l0, (6.0, 0.0, 3.0));
        let mut s2 = s1.clone();
        s2.a = (10.0, 0.0, 3.0);
        s2.b = (10.0, 4.0, 3.0);
        let b2 = m.add_beam(&s2).unwrap();
        let (t8, t16) = (rtype(&m, 8), rtype(&m, 16));
        let long = RebarLayout::Long {
            pos: LongPos::Bottom,
            count: 2,
            shape: ShapeCode::Straight,
            hooks: (Hook::None, Hook::None),
            leg: 0.0,
        };
        let stirrup = RebarLayout::Stirrup {
            rule: PathLayout::MaxSpacing(0.2),
            hook: Hook::H135,
        };
        m.add_rebar_sets(&[set("beam", b1, t16.id, long.clone())])
            .unwrap();
        m.add_rebar_sets(&[set("beam", b2, t8.id, stirrup)])
            .unwrap();
        assert_eq!(marks_of(&m, b1), one("1"));
        assert_eq!(marks_of(&m, b2), one("2"));
        // the first beam grows: only its bars get a new number, the stirrups keep theirs
        s1.b = (7.0, 0.0, 3.0);
        m.update_beam(b1, &s1).unwrap();
        assert_eq!(marks_of(&m, b1), one("3"));
        assert_eq!(marks_of(&m, b2), one("2"));
        assert_eq!(m.freeze_rebar_marks().unwrap(), 1);
        assert_eq!(m.freeze_rebar_marks().unwrap(), 0);
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(marks_of(&m, b1), one("1"));
        // a user mark equal to another bar's mark: a conflict, listed in separate rows
        let mut own = set("beam", b1, t16.id, long);
        own.mark = Some("2".into());
        let id = m.add_rebar_sets(&[own]).unwrap()[0];
        assert!(m.rebar_issues().unwrap().contains(&(id, "mark_conflict")));
        let rows: Vec<RebarRow> = m
            .rebar_schedule(None)
            .unwrap()
            .into_iter()
            .filter(|r| r.mark == "2")
            .collect();
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].shape, rows[1].shape);
        // a new auto mark never takes a user mark
        let row = |pos: LongPos| RebarLayout::Long {
            pos,
            count: 2,
            shape: ShapeCode::Straight,
            hooks: (Hook::None, Hook::None),
            leg: 0.0,
        };
        let mut top = set("beam", b2, t16.id, row(LongPos::Top));
        top.mark = Some("3".into());
        m.add_rebar_sets(&[top]).unwrap();
        let id3 = m
            .add_rebar_sets(&[set("beam", b2, t16.id, row(LongPos::Bottom))])
            .unwrap()[0];
        let auto: BTreeSet<String> = m
            .rebar_bars(None)
            .unwrap()
            .into_iter()
            .filter(|b| b.set == id3)
            .map(|b| b.mark)
            .collect();
        assert_eq!(auto, one("4"));
    }

    fn raft_with_mat(m: &mut Model, l0: i64) -> (i64, i64) {
        let rid = m
            .add_raft(&crate::RaftSpec {
                level: l0,
                ring: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
                thickness: 0.4,
                edge_width: 0.0,
                edge_depth: 0.0,
                top_offset: 0.0,
                material: "Concrete".into(),
            })
            .unwrap();
        let t12 = rtype(m, 12);
        let sid = m
            .add_rebar_sets(&[set("raft", rid, t12.id, mat(t12.id, Faces::Bottom))])
            .unwrap()[0];
        (rid, sid)
    }

    fn rebar_rows(m: &Model) -> String {
        m.sql()
            .query_row(
                "SELECT coalesce((SELECT group_concat(id||host_kind||':'||host||layout, ',') FROM \
                 (SELECT * FROM rebar_set ORDER BY id)), '') || '|' || \
                 coalesce((SELECT group_concat(id||host_kind||':'||host||face||cover, ',') FROM \
                 (SELECT * FROM rebar_cover ORDER BY id)), '')",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
    }

    #[test]
    fn raft_delete_prunes_rebar_undo_restores() {
        let (mut m, l0, _) = setup();
        let (rid, _) = raft_with_mat(&mut m, l0);
        m.set_rebar_covers(&[("raft".into(), rid)], &[("bottom".into(), 0.04)])
            .unwrap();
        // a beam with the same id as the raft: its rebar must not follow the raft
        let (bid, _) = beam(&mut m, l0, (6.0, 0.0, 3.0));
        assert_eq!(bid, rid, "the test needs the same host id in both tables");
        let t10 = rtype(&m, 10);
        let long = RebarLayout::Long {
            pos: LongPos::Bottom,
            count: 2,
            shape: ShapeCode::Straight,
            hooks: (Hook::None, Hook::None),
            leg: 0.0,
        };
        m.add_rebar_sets(&[set("beam", bid, t10.id, long)]).unwrap();
        let before = rebar_rows(&m);
        let bars = m.rebar_bars(None).unwrap().len();
        assert!(bars > 0);
        m.delete_struct("raft", rid).unwrap();
        assert!(m.rebar_sets_of("raft", Some(rid)).unwrap().is_empty());
        let n: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM rebar_cover WHERE host_kind='raft'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        let n: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM rebar_set WHERE host_kind='beam'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "a set of another host kind stays");
        m.undo().unwrap();
        assert_eq!(rebar_rows(&m), before, "exact rows back");
        assert_eq!(m.rebar_bars(None).unwrap().len(), bars);
    }

    #[test]
    fn copy_raft_carries_rebar() {
        let (mut m, l0, _) = setup();
        let (rid, _) = raft_with_mat(&mut m, l0);
        m.set_rebar_covers(&[("raft".into(), rid)], &[("bottom".into(), 0.04)])
            .unwrap();
        let clip = m.clip_copy(&[("raft", rid)]).unwrap();
        let rep = m
            .clip_paste(
                &clip,
                &crate::PasteOpts {
                    dx: 10.0,
                    dy: 0.0,
                    level: crate::PlaceLevel::Base(l0),
                },
            )
            .unwrap();
        let new = rep.created.iter().find(|x| x.0 == "raft").unwrap().1;
        assert_ne!(new, rid);
        assert_eq!(m.rebar_sets_of("raft", Some(new)).unwrap().len(), 1);
        assert_eq!(m.rebar_sets_of("raft", Some(rid)).unwrap().len(), 1);
        let covers: i64 = m
            .sql()
            .query_row(
                "SELECT count(*) FROM rebar_cover WHERE host_kind='raft' AND host=?1",
                [new],
                |r| r.get(0),
            )
            .unwrap();
        assert!(covers >= 1, "the copy has its own covers");
    }

    #[test]
    fn raft_default_covers_match_footing() {
        assert_eq!(default_covers("raft"), default_covers("footing"));
    }

    #[test]
    fn integrity_and_phases_know_rebar_sets() {
        let (m, _, _) = setup();
        let t8 = rtype(&m, 8);
        m.sql()
            .execute(
                "INSERT INTO rebar_set(host_kind, host, layout, rtype) \
                 VALUES ('beam', 999, 'stirrup', ?1)",
                [t8.id],
            )
            .unwrap();
        let iss = m.check_integrity().unwrap();
        assert!(iss
            .iter()
            .any(|i| i.table == "rebar_set" && i.problem.contains("missing beam 999")));
        assert!(crate::PHASED.contains(&"rebar_set"));
    }

    #[test]
    fn bars_above_the_cut_are_hidden_lines_and_sets_sum_by_host() {
        let (mut m, l0, _) = setup();
        let (bid, _) = beam(&mut m, l0, (6.0, 0.0, 3.0));
        let t16 = rtype(&m, 16);
        let long = RebarLayout::Long {
            pos: LongPos::Bottom,
            count: 2,
            shape: ShapeCode::Straight,
            hooks: (Hook::None, Hook::None),
            leg: 0.0,
        };
        let ids = m
            .add_rebar_sets(&[set("beam", bid, t16.id, long)])
            .unwrap();
        let plan = m.rebar_plan(l0, 1.2).unwrap();
        assert_eq!(plan.len(), 1);
        assert!(plan[0].lines.is_empty() && plan[0].dots.is_empty());
        assert_eq!(plan[0].hidden.len(), 2);
        let rows = m.rebar_set_schedule(Some(l0)).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].set, rows[0].count), (ids[0], 2));
        assert_eq!((rows[0].host_kind, rows[0].host), ("beam", bid));
        let bars = m.rebar_bars(None).unwrap();
        close(rows[0].weight, bars.iter().map(|b| b.weight()).sum::<f64>());
        close(rows[0].total_length, bars.iter().map(|b| b.length).sum::<f64>());
        assert!(m.rebar_set_schedule(Some(l0 + 1000)).unwrap().is_empty());
    }
}
