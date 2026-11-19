//! Structure, schema 26: footings, braces, trusses, framing systems.
//! Common: DDL, the derived `struct_qty` (an element part is a whole footing, a truss
//! or system member), material densities and a schedule with mass.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use ddc_geom::mesh::Mesh;
use ddc_geom::section::Prism;
use ddc_geom::{V2, V3};
use rusqlite::{params, Connection};

use crate::ops::Inv;
use crate::structure::Built;
use crate::{Error, Model};

/// Schema 26 tables (additive, idempotent). Elevations z are from the level.
/// Pad footing: col is the host column (centre = column bottom + (ex, ey), rotated with the column;
/// top = column bottom); without a host it is (x, y), top = level + top_offset; steps are `w l h;...`
/// bottom to top. Strip footing: wall is the host wall (wall axis and bottom, mid-thickness + ecc),
/// otherwise its own axis. Hosts are soft references: regeneration keeps fallback geometry
/// (x, y, frot, ftop; ax...bulge = wall mid-thickness, ftop) equal to the current one and
/// detaches the element when the host is deleted (host ids are reused). Raft: outline
/// raft_vertex, edge thickening edge_*. Brace: ends (a, b) or a point on a column/beam axis
/// host_* (column|beam) with parameter t_*. Truss: web pratt|howe|warren, chord flat|pitched.
/// System: rule fixed_start|fixed_center|max_equal|fixed_count (count), angle is the beam
/// direction, rad, start_edge is the reference outline edge; ring vertices 0 is the outline, 1... are openings.
pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS iso_footing(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, col INTEGER,
    x REAL NOT NULL, y REAL NOT NULL, ex REAL NOT NULL, ey REAL NOT NULL, rotation REAL NOT NULL,
    round INTEGER NOT NULL, steps TEXT NOT NULL, top_offset REAL NOT NULL, material TEXT NOT NULL,
    frot REAL NOT NULL DEFAULT 0, ftop REAL NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS iso_footing_col ON iso_footing(col);
CREATE TABLE IF NOT EXISTS strip_footing(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, wall INTEGER,
    ax REAL NOT NULL, ay REAL NOT NULL, bx REAL NOT NULL, by REAL NOT NULL, bulge REAL NOT NULL,
    width REAL NOT NULL, depth REAL NOT NULL, ecc REAL NOT NULL, top_offset REAL NOT NULL,
    material TEXT NOT NULL, ftop REAL NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS strip_footing_wall ON strip_footing(wall);
CREATE TABLE IF NOT EXISTS raft(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, thickness REAL NOT NULL,
    edge_width REAL NOT NULL, edge_depth REAL NOT NULL, top_offset REAL NOT NULL, material TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS raft_vertex(raft INTEGER NOT NULL, idx INTEGER NOT NULL, x REAL NOT NULL,
    y REAL NOT NULL, PRIMARY KEY(raft, idx));
CREATE TABLE IF NOT EXISTS brace(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, mtype INTEGER NOT NULL,
    ax REAL NOT NULL, ay REAL NOT NULL, az REAL NOT NULL, bx REAL NOT NULL, by REAL NOT NULL,
    bz REAL NOT NULL, host_a TEXT, ref_a INTEGER, t_a REAL, host_b TEXT, ref_b INTEGER, t_b REAL,
    rotation REAL NOT NULL);
CREATE TABLE IF NOT EXISTS truss(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, web TEXT NOT NULL,
    chord TEXT NOT NULL, ax REAL NOT NULL, ay REAL NOT NULL, az REAL NOT NULL, bx REAL NOT NULL,
    by REAL NOT NULL, bz REAL NOT NULL, height REAL NOT NULL, heel REAL NOT NULL,
    panels INTEGER NOT NULL, chord_type INTEGER NOT NULL, web_type INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS beam_system(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, mtype INTEGER NOT NULL,
    z REAL NOT NULL, angle REAL NOT NULL, spacing REAL NOT NULL, rule TEXT NOT NULL,
    end_gap REAL NOT NULL, zjust INTEGER NOT NULL, start_edge INTEGER, count INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS beam_system_vertex(sys INTEGER NOT NULL, idx INTEGER NOT NULL, x REAL NOT NULL,
    y REAL NOT NULL, ring INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(sys, idx));
CREATE TABLE IF NOT EXISTS struct_qty(kind TEXT NOT NULL, ref INTEGER NOT NULL, part INTEGER NOT NULL,
    role TEXT NOT NULL, material TEXT NOT NULL, length REAL NOT NULL, volume REAL NOT NULL,
    area REAL NOT NULL, PRIMARY KEY(kind, ref, part));
CREATE TABLE IF NOT EXISTS struct_issue(kind TEXT NOT NULL, ref INTEGER NOT NULL, problem TEXT NOT NULL);
";

/// Columns added after the first schema 26 files (table, column, declaration).
const ADDED: [(&str, &str, &str); 6] = [
    ("iso_footing", "frot", "REAL NOT NULL DEFAULT 0"),
    ("iso_footing", "ftop", "REAL NOT NULL DEFAULT 0"),
    ("strip_footing", "ftop", "REAL NOT NULL DEFAULT 0"),
    ("beam_system", "start_edge", "INTEGER"),
    ("beam_system", "count", "INTEGER NOT NULL DEFAULT 0"),
    ("beam_system_vertex", "ring", "INTEGER NOT NULL DEFAULT 0"),
];

/// An element part with a body (world coordinates): a whole footing, a truss member, etc.
/// role: footing | strip_footing | raft | brace | top | bottom | vertical | diagonal | joist.
#[derive(Clone, Debug)]
pub struct StructBody {
    pub kind: &'static str,
    pub id: i64,
    pub level: i64,
    pub part: i64,
    pub role: &'static str,
    pub material: String,
    pub mesh: Mesh,
    pub length: f64,
    pub volume: f64,
    pub area: f64,
    /// Extents in its own system: length, width, height (pad: base w, l and depth;
    /// strip: axis, width, depth; raft: plan frame and full thickness; member:
    /// length and section).
    pub size: [f64; 3],
    /// Mass, kg; None if the material density is unknown.
    pub weight: Option<f64>,
}

/// Called from `schema::upgrade` on every open.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    crate::steel_conn::migrate(db)?;
    for (t, c, decl) in ADDED {
        let n: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info(?1) WHERE name=?2",
            [t, c],
            |r| r.get(0),
        )?;
        if n == 0 {
            db.execute_batch(&format!("ALTER TABLE {t} ADD COLUMN {c} {decl}"))?;
        }
    }
    Ok(())
}

/// Schema 26 element kinds (the `kind` field in `struct_qty` and views).
pub const STRUCT_KINDS: [&str; 6] = [
    "footing",
    "strip_footing",
    "raft",
    "brace",
    "truss",
    "beam_system",
];

/// Element table by kind.
pub(crate) fn table_of(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "footing" => "iso_footing",
        "strip_footing" => "strip_footing",
        "raft" => "raft",
        "brace" => "brace",
        "truss" => "truss",
        "beam_system" => "beam_system",
        "steel_conn" => "steel_conn",
        "gridshell" => "gridshell",
        _ => return None,
    })
}

/// An element part after construction.
pub(crate) struct Part {
    pub kind: &'static str,
    pub id: i64,
    pub level: i64,
    pub part: i64,
    pub role: &'static str,
    pub material: String,
    pub meshes: Vec<Mesh>,
    pub length: f64,
    pub volume: f64,
    pub area: f64,
    /// See [`StructBody::size`].
    pub size: [f64; 3],
    /// Plan footprint (selection, highlight).
    pub plan: Vec<Vec<V2>>,
    /// Prisms for sections.
    pub prisms: Vec<Prism>,
    /// View lines instead of mesh edges (strip: no seams between junction pieces).
    pub lines: Option<Vec<(V3, V3)>>,
    /// Integrity warnings (overlapping strips, etc.).
    pub warn: Vec<String>,
}

impl Part {
    pub fn new(
        kind: &'static str,
        id: i64,
        level: i64,
        role: &'static str,
        material: &str,
    ) -> Part {
        Part {
            kind,
            id,
            level,
            part: 0,
            role,
            material: material.to_string(),
            meshes: vec![],
            length: 0.0,
            volume: 0.0,
            area: 0.0,
            size: [0.0; 3],
            plan: vec![],
            prisms: vec![],
            lines: None,
            warn: vec![],
        }
    }
}

/// Convex hull of plan points (counter-clockwise, no collinear points).
pub(crate) fn hull(pts: &[V2]) -> Vec<V2> {
    let mut p: Vec<V2> = pts.to_vec();
    p.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    p.dedup_by(|a, b| (a.x - b.x).abs() < 1e-12 && (a.y - b.y).abs() < 1e-12);
    if p.len() < 3 {
        return p;
    }
    let cross = |o: V2, a: V2, b: V2| (a - o).cross(b - o);
    let mut h: Vec<V2> = vec![];
    for pass in 0..2 {
        let start = h.len();
        let order: Vec<V2> = if pass == 0 {
            p.clone()
        } else {
            p.iter().rev().copied().collect()
        };
        for q in order {
            while h.len() >= start + 2 && cross(h[h.len() - 2], h[h.len() - 1], q) <= 1e-15 {
                h.pop();
            }
            h.push(q);
        }
        h.pop();
    }
    h
}

/// Columns/beams and schema 26 parts of one model state.
pub(crate) struct Built3 {
    pub members: Vec<Built>,
    pub parts: Vec<Part>,
}

/// The last assembly, keyed by the connection's `total_changes()` (any write invalidates it).
#[derive(Default)]
pub(crate) struct Cache(Mutex<Option<(i64, Arc<Built3>)>>);

thread_local! {
    /// A regeneration assembly before the transaction commits: (connection address, assembly).
    static STASH: RefCell<Option<(usize, Arc<Built3>)>> = const { RefCell::new(None) };
}

fn conn_key(db: &Connection) -> usize {
    db as *const Connection as usize
}

fn changes(db: &Connection) -> Result<i64, Error> {
    Ok(db.query_row("SELECT total_changes()", [], |r| r.get(0))?)
}

/// Drop the assembly of an uncommitted regeneration (start of an operation).
pub(crate) fn clear_stash() {
    STASH.with(|c| *c.borrow_mut() = None);
}

pub(crate) fn any_rows(db: &Connection) -> Result<bool, Error> {
    let n: i64 = db.query_row(
        "SELECT (SELECT count(*) FROM iso_footing) + (SELECT count(*) FROM strip_footing) + \
         (SELECT count(*) FROM raft) + (SELECT count(*) FROM brace) + (SELECT count(*) FROM truss) + \
         (SELECT count(*) FROM beam_system) + (SELECT count(*) FROM steel_conn) + \
         (SELECT count(*) FROM gridshell)",
        [],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// All parts given built columns and beams: footings, then framing (by kind and id).
pub(crate) fn build_with(db: &Connection, members: &[Built]) -> Result<Vec<Part>, Error> {
    if !any_rows(db)? {
        return Ok(vec![]);
    }
    let mut out = crate::footings::build(db, members)?;
    out.extend(crate::framing::build(db, members)?);
    out.extend(crate::gridshell::build(db)?);
    Ok(out)
}

/// Fallback geometry from hosts, then `struct_qty`/`struct_issue` from one assembly
/// (columns and beams come from `structure::regenerate`). Returns the inverse commands of the
/// fallback geometry edits: `Model::op` journals them before the operation's inverse commands.
pub(crate) fn regenerate(db: &Connection, members: Vec<Built>) -> Result<Inv, Error> {
    let inv =
        crate::footings::sync_hosts(db, &members)?.then(crate::framing::sync_hosts(db, &members)?);
    db.execute("DELETE FROM struct_qty", [])?;
    db.execute("DELETE FROM struct_issue", [])?;
    let parts = build_with(db, &members)?;
    let mut st = db.prepare_cached(
        "INSERT OR REPLACE INTO struct_qty(kind,ref,part,role,material,length,volume,area) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
    )?;
    let mut wi =
        db.prepare_cached("INSERT INTO struct_issue(kind,ref,problem) VALUES (?1,?2,?3)")?;
    for p in &parts {
        st.execute(params![
            p.kind, p.id, p.part, p.role, p.material, p.length, p.volume, p.area
        ])?;
        for w in &p.warn {
            wi.execute(params![p.kind, p.id, w])?;
        }
    }
    // steel connections follow their members (after the issue table is cleared)
    let inv = inv.then(crate::steel_conn::sync(db, &members)?);
    let b = Arc::new(Built3 { members, parts });
    STASH.with(|c| *c.borrow_mut() = Some((conn_key(db), b)));
    Ok(inv)
}

/// Material density, kg/m3, by name (Russian/English); None if unknown.
/// Lightweight and admixture concretes ("steel-fibre concrete") are checked before steel.
pub fn density(material: &str) -> Option<f64> {
    let m = material.to_lowercase();
    let has = |keys: &[&str]| keys.iter().any(|k| m.contains(k));
    Some(
        if has(&[
            "aerated",
            "autoclaved",
            "\u{433}\u{430}\u{437}\u{43e}\u{431}\u{435}\u{442}\u{43e}\u{43d}",
            "\u{43f}\u{435}\u{43d}\u{43e}\u{431}\u{435}\u{442}\u{43e}\u{43d}",
            "\u{433}\u{430}\u{437}\u{43e}\u{441}\u{438}\u{43b}\u{438}\u{43a}\u{430}\u{442}",
        ]) {
            600.0
        } else if has(&[
            "lightweight",
            "light-weight",
            "\u{43b}\u{435}\u{433}\u{43a}\u{438}\u{439} \u{431}\u{435}\u{442}\u{43e}\u{43d}",
            "\u{43b}\u{451}\u{433}\u{43a}\u{438}\u{439} \u{431}\u{435}\u{442}\u{43e}\u{43d}",
            "\u{43a}\u{435}\u{440}\u{430}\u{43c}\u{437}\u{438}\u{442}",
        ]) {
            1800.0
        } else if has(&["reinforced", "\u{436}\u{435}\u{43b}\u{435}\u{437}\u{43e}\u{431}\u{435}\u{442}"]) {
            2500.0
        } else if has(&["concrete", "\u{431}\u{435}\u{442}\u{43e}\u{43d}"]) {
            2400.0
        } else if has(&["steel", "\u{441}\u{442}\u{430}\u{43b}\u{44c}", "s235", "s355"]) {
            7850.0
        } else if has(&["alum", "\u{430}\u{43b}\u{44e}\u{43c}"]) {
            2700.0
        } else if has(&["timber", "wood", "glulam", "\u{434}\u{435}\u{440}\u{435}\u{432}", "\u{431}\u{440}\u{443}\u{441}", "\u{43a}\u{43b}\u{435}\u{435}\u{43d}"]) {
            500.0
        } else if has(&["brick", "masonry", "\u{43a}\u{438}\u{440}\u{43f}\u{438}\u{447}", "\u{43a}\u{43b}\u{430}\u{434}\u{43a}"]) {
            1800.0
        } else {
            return None;
        },
    )
}

/// Schema 26 element quantities: sums over parts.
#[derive(Clone, Debug, PartialEq)]
pub struct StructQty {
    pub parts: usize,
    pub length: f64,
    pub volume: f64,
    /// Footings: base area; members: surface area.
    pub area: f64,
    pub material: String,
    /// Mass, kg (volume x density); None if the material density is unknown.
    pub weight: Option<f64>,
}

/// A structure schedule row.
#[derive(Clone, Debug, PartialEq)]
pub struct TakeoffRow {
    pub kind: String,
    pub id: i64,
    pub material: String,
    pub length: f64,
    pub volume: f64,
    pub weight: Option<f64>,
}

impl Model {
    /// Quantities of a footing, brace, truss or system (`kind` is from `STRUCT_KINDS`).
    pub fn struct_quantities(&self, kind: &str, id: i64) -> Result<StructQty, Error> {
        let (parts, length, volume, area, material): (i64, f64, f64, f64, Option<String>) =
            self.db.query_row(
                "SELECT count(*), total(length), total(volume), total(area), min(material) \
                 FROM struct_qty WHERE kind=?1 AND ref=?2",
                params![kind, id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
        if parts == 0 {
            return Err(Error::Invalid(format!("no quantities for {kind} {id}")));
        }
        let weight = self.part_weight(kind, id)?;
        Ok(StructQty {
            parts: parts as usize,
            length,
            volume,
            area,
            material: material.unwrap_or_default(),
            weight,
        })
    }

    /// Mass by parts (part materials may differ: truss chords and web).
    fn part_weight(&self, kind: &str, id: i64) -> Result<Option<f64>, Error> {
        let mut st = self.db.prepare(
            "SELECT material, total(volume) FROM struct_qty WHERE kind=?1 AND ref=?2 GROUP BY material",
        )?;
        let rows: Vec<(String, f64)> = st
            .query_map(params![kind, id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut w = 0.0;
        for (m, v) in rows {
            match density(&m) {
                Some(d) => w += d * v,
                None => return Ok(None),
            }
        }
        Ok(Some(w))
    }

    /// Element parts: (role, length, volume) in build order.
    pub fn struct_parts(&self, kind: &str, id: i64) -> Result<Vec<(String, f64, f64)>, Error> {
        let mut st = self.db.prepare(
            "SELECT role, length, volume FROM struct_qty WHERE kind=?1 AND ref=?2 ORDER BY part",
        )?;
        let rows = st.query_map(params![kind, id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Mass of a column or beam (`kind` column|beam), kg; None if the density is unknown.
    pub fn member_weight(&self, kind: &str, id: i64) -> Result<Option<f64>, Error> {
        let q = if kind == "column" {
            self.column_quantities(id)?
        } else {
            self.beam_quantities(id)?
        };
        Ok(density(&q.material).map(|d| d * q.volume))
    }

    /// Structure schedule: columns, beams, then schema 26 elements (by material).
    pub fn struct_takeoff(&self) -> Result<Vec<TakeoffRow>, Error> {
        let mut out = vec![];
        for (kind, table) in [("column", "col"), ("beam", "beam")] {
            let mut st = self.db.prepare(&format!(
                "SELECT q.ref, t.material, q.length, q.volume FROM member_qty q \
                 JOIN {table} e ON e.id = q.ref JOIN member_type t ON t.id = e.mtype \
                 WHERE q.kind=?1 ORDER BY q.ref"
            ))?;
            let rows: Vec<(i64, String, f64, f64)> = st
                .query_map([kind], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<Result<_, _>>()?;
            for (id, material, length, volume) in rows {
                out.push(TakeoffRow {
                    kind: kind.into(),
                    id,
                    weight: density(&material).map(|d| d * volume),
                    material,
                    length,
                    volume,
                });
            }
        }
        let mut st = self.db.prepare(
            "SELECT kind, ref, material, total(length), total(volume) FROM struct_qty \
             GROUP BY kind, ref, material ORDER BY kind, ref, material",
        )?;
        let rows: Vec<(String, i64, String, f64, f64)> = st
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<_, _>>()?;
        for (kind, id, material, length, volume) in rows {
            out.push(TakeoffRow {
                kind,
                id,
                weight: density(&material).map(|d| d * volume),
                material,
                length,
                volume,
            });
        }
        Ok(out)
    }

    /// Assembly of the current state: from the cache (after an operation, the assembly of its regeneration).
    pub(crate) fn struct_build(&self) -> Result<Arc<Built3>, Error> {
        let key = changes(&self.db)?;
        if let Ok(g) = self.struct_cache.0.lock() {
            if let Some((k, b)) = &*g {
                if *k == key {
                    return Ok(b.clone());
                }
            }
        }
        let members = crate::structure::build_all(&self.db)?;
        let parts = build_with(&self.db, &members)?;
        let b = Arc::new(Built3 { members, parts });
        if let Ok(mut g) = self.struct_cache.0.lock() {
            *g = Some((key, b.clone()));
        }
        Ok(b)
    }

    /// After the transaction commits: the regeneration assembly becomes the view cache.
    pub(crate) fn adopt_struct(&self) {
        let Some((k, b)) = STASH.with(|c| c.borrow_mut().take()) else {
            return;
        };
        if k != conn_key(&self.db) {
            return;
        }
        if let (Ok(key), Ok(mut g)) = (changes(&self.db), self.struct_cache.0.lock()) {
            *g = Some((key, b));
        }
    }

    /// Schema 26 element parts of a level (or all) with bodies, for exchange (IFC).
    pub fn struct_part_bodies(&self, level: Option<i64>) -> Result<Vec<StructBody>, Error> {
        Ok(self
            .struct_build()?
            .parts
            .iter()
            .filter(|p| level.is_none_or(|l| l == p.level))
            .map(|p| StructBody {
                kind: p.kind,
                id: p.id,
                level: p.level,
                part: p.part,
                role: p.role,
                material: p.material.clone(),
                mesh: ddc_geom::mep::merge(&p.meshes),
                length: p.length,
                volume: p.volume,
                area: p.area,
                size: p.size,
                weight: density(&p.material).map(|d| d * p.volume),
            })
            .collect())
    }

    /// Schema 26 element bodies of a level (or all): one mesh per element (kind, id, mesh),
    /// world coordinates.
    pub fn struct_meshes(
        &self,
        level: Option<i64>,
    ) -> Result<Vec<(&'static str, i64, Mesh)>, Error> {
        let b = self.struct_build()?;
        let mut out: Vec<(&'static str, i64, Mesh)> = vec![];
        let mut cur: Vec<Mesh> = vec![];
        let mut key: Option<(&'static str, i64)> = None;
        for p in b
            .parts
            .iter()
            .filter(|p| level.is_none_or(|l| l == p.level))
        {
            if key != Some((p.kind, p.id)) {
                if let Some((k, i)) = key {
                    out.push((k, i, ddc_geom::mep::merge(&cur)));
                }
                cur.clear();
                key = Some((p.kind, p.id));
            }
            cur.extend(p.meshes.iter().cloned());
        }
        if let Some((k, i)) = key {
            out.push((k, i, ddc_geom::mep::merge(&cur)));
        }
        Ok(out)
    }

    /// Plan footprint of schema 26 elements of a level: (kind, id, outlines), world metres.
    #[allow(clippy::type_complexity)]
    pub fn struct_footprints(
        &self,
        level: i64,
    ) -> Result<Vec<(&'static str, i64, Vec<Vec<(f64, f64)>>)>, Error> {
        let b = self.struct_build()?;
        let mut out: Vec<(&'static str, i64, Vec<Vec<(f64, f64)>>)> = vec![];
        for p in b.parts.iter().filter(|p| p.level == level) {
            let rings: Vec<Vec<(f64, f64)>> = p
                .plan
                .iter()
                .map(|r| r.iter().map(|q| (q.x, q.y)).collect())
                .collect();
            match out.last_mut() {
                Some(last) if last.0 == p.kind && last.1 == p.id => last.2.extend(rings),
                _ => out.push((p.kind, p.id, rings)),
            }
        }
        Ok(out)
    }

    /// Schema 26 element prisms for sections: (table, id, material, prism).
    #[allow(clippy::type_complexity)]
    pub(crate) fn struct_section_prisms(
        &self,
    ) -> Result<Vec<(&'static str, i64, String, Prism)>, Error> {
        let b = self.struct_build()?;
        let mut out = vec![];
        for p in &b.parts {
            let table = table_of(p.kind).unwrap_or("iso_footing");
            for q in &p.prisms {
                out.push((table, p.id, p.material.clone(), q.clone()));
            }
        }
        Ok(out)
    }

    /// Delete a schema 26 element (one operation, undoable).
    pub fn delete_struct(&mut self, kind: &str, id: i64) -> Result<(), Error> {
        let table = table_of(kind).ok_or_else(|| Error::Invalid(format!("unknown kind {kind}")))?;
        let owned: &[(&str, &str)] = match table {
            "raft" => &[("raft_vertex", "raft")],
            "beam_system" => &[("beam_system_vertex", "sys")],
            "gridshell" => &[("gridshell_node", "shell"), ("gridshell_bar", "shell")],
            _ => &[],
        };
        let op = if matches!(kind, "footing" | "strip_footing" | "raft") {
            "footing_delete"
        } else {
            "member_struct_delete"
        };
        self.op(op, |db| {
            let mut inv = crate::ops::Inv::default().restore(db, table, "id", id)?;
            if db.execute(&format!("DELETE FROM {table} WHERE id=?1"), [id])? == 0 {
                return Err(Error::Invalid(format!("no such {kind} {id}")));
            }
            for &(t, c) in owned {
                inv = inv.restore(db, t, c, id)?;
                db.execute(&format!("DELETE FROM {t} WHERE {c}=?1"), [id])?;
            }
            // rebar and covers of a footing or raft go with it, one undo brings them back
            if matches!(table, "iso_footing" | "raft") {
                let hk = if table == "raft" { "raft" } else { "footing" };
                for set in ["rebar_set", "rebar_cover"] {
                    if crate::rebar::has_table(db, set)? {
                        inv = inv.restore_where(
                            db,
                            set,
                            "host_kind=?1 AND host=?2",
                            &[
                                rusqlite::types::Value::Text(hk.to_string()),
                                rusqlite::types::Value::Integer(id),
                            ],
                        )?;
                        db.execute(
                            &format!("DELETE FROM {set} WHERE host_kind=?1 AND host=?2"),
                            rusqlite::params![hk, id],
                        )?;
                    }
                }
            }
            Ok(((), id, inv))
        })
    }

    /// Number of elements of a kind on a level (or all).
    pub fn struct_ids(&self, kind: &str, level: Option<i64>) -> Result<Vec<i64>, Error> {
        let table = table_of(kind).ok_or_else(|| Error::Invalid(format!("unknown kind {kind}")))?;
        let mut st = self.db.prepare(&format!(
            "SELECT id FROM {table} WHERE ?1 IS NULL OR level=?1 ORDER BY id"
        ))?;
        let rows = st.query_map([level], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// Violations: parts without quantities (not built) and assembly warnings (`struct_issue`).
pub(crate) fn issues(db: &Connection) -> Result<Vec<crate::Issue>, Error> {
    let mut out = vec![];
    let mut st =
        db.prepare("SELECT kind, ref, problem FROM struct_issue ORDER BY kind, ref, rowid")?;
    let rows: Vec<(String, i64, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    for (kind, id, problem) in rows {
        out.push(crate::Issue {
            table: table_of(&kind).unwrap_or("iso_footing").into(),
            id,
            problem,
        });
    }
    for kind in STRUCT_KINDS {
        let table = table_of(kind).unwrap_or("iso_footing");
        let mut st = db.prepare(&format!(
            "SELECT id FROM {table} WHERE id NOT IN (SELECT ref FROM struct_qty WHERE kind='{kind}') ORDER BY id"
        ))?;
        for id in st.query_map([], |r| r.get::<_, i64>(0))? {
            out.push(crate::Issue {
                table: table.into(),
                id: id?,
                problem: format!("{kind} without quantities"),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{density, hull};
    use crate::footings::FootingSpec;
    use ddc_geom::footing::Step;
    use crate::Model;
    use ddc_geom::v2;

    fn rows(m: &Model, sql: &str) -> String {
        m.sql()
            .query_row(sql, [], |r| r.get::<_, String>(0))
            .unwrap()
    }

    #[test]
    fn footing_delete_undo_keeps_beam_rebar_same_host_id() {
        let mut m = Model::open(":memory:").unwrap();
        let l0 = m.add_level("L0", 0.0).unwrap();
        let t = m
            .add_member_type("RC 300x500", &ddc_geom::member::Shape::Rect { b: 0.3, h: 0.5 }, "Concrete")
            .unwrap();
        let f = m
            .add_footing(&FootingSpec {
                level: l0,
                column: None,
                at: (0.0, 0.0),
                rotation: 0.0,
                round: false,
                steps: vec![Step { w: 1.8, l: 1.8, h: 0.3 }],
                top_offset: 0.0,
                material: "Concrete".into(),
            })
            .unwrap();
        let b = m
            .add_beam(&crate::BeamSpec {
                level: l0,
                member_type: t,
                a: (0.0, 0.0, 3.0),
                b: (6.0, 0.0, 3.0),
                bulge: 0.0,
                yjust: crate::Just::Center,
                zjust: crate::Just::Center,
                rotation: 0.0,
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: crate::BeamJoin::Auto,
                join_end: crate::BeamJoin::Auto,
            })
            .unwrap();
        assert_eq!(f, b, "the test needs the same host id in both tables");
        for (kind, host) in [("footing", f), ("beam", b)] {
            m.sql()
                .execute(
                    "INSERT INTO rebar_set(host_kind, host, layout, rtype) VALUES (?1, ?2, 'long', 1)",
                    rusqlite::params![kind, host],
                )
                .unwrap();
            m.sql()
                .execute(
                    "INSERT INTO rebar_cover(host_kind, host, face, cover) VALUES (?1, ?2, 'top', 0.05)",
                    rusqlite::params![kind, host],
                )
                .unwrap();
        }
        let dump = |m: &Model| {
            rows(
                m,
                "SELECT (SELECT group_concat(id||host_kind||host, ',') FROM rebar_set) || '|' ||
                        (SELECT group_concat(id||host_kind||host, ',') FROM rebar_cover)",
            )
        };
        let before = dump(&m);
        m.delete_struct("footing", f).unwrap();
        let after = rows(
            &m,
            "SELECT (SELECT count(*) FROM rebar_set WHERE host_kind='footing') ||
                    (SELECT count(*) FROM rebar_set WHERE host_kind='beam') ||
                    (SELECT count(*) FROM rebar_cover WHERE host_kind='footing')",
        );
        assert_eq!(after, "010");
        m.undo().unwrap();
        assert_eq!(dump(&m), before);
        assert!(m.footing(f).is_ok());
    }

    #[test]
    fn densities_by_name() {
        assert_eq!(density("Steel"), Some(7850.0));
        assert_eq!(density(crate::test_ru::ru("mat_concrete_b25")), Some(2400.0));
        assert_eq!(density(crate::test_ru::ru("mat_reinforced")), Some(2500.0));
        assert_eq!(density("Glulam GL24h"), Some(500.0));
        assert_eq!(density("Unobtainium"), None);
        assert_eq!(density(crate::test_ru::ru("mat_aerated_d500")), Some(600.0));
        assert_eq!(density("Aerated concrete"), Some(600.0));
        assert_eq!(density("Steel fibre concrete"), Some(2400.0));
        assert_eq!(density("Lightweight concrete"), Some(1800.0));
    }

    #[test]
    fn hull_of_points() {
        let pts = [
            v2(0.0, 0.0),
            v2(2.0, 0.0),
            v2(1.0, 0.5),
            v2(2.0, 1.0),
            v2(0.0, 1.0),
            v2(2.0, 0.0),
        ];
        let h = hull(&pts);
        assert_eq!(h.len(), 4, "{h:?}");
        assert!((ddc_geom::poly::area(&h) - 2.0).abs() < 1e-12);
    }
}
