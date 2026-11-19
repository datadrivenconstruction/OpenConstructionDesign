// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Steel connections (g5) in the model: a journaled spec row per connection (type, joined
//! members, parameters) and its parts as `solid` rows. Regeneration rebuilds every connection
//! from the current members, so a changed section or a moved member updates the plates,
//! bolts and welds; the rewritten solids go back to the journal with the edit for undo.
//! Only connections whose inputs changed are rebuilt (session cache `steel_conn_key`).

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};

use ddc_geom::member::{Member, Path};
use ddc_geom::steel::bar::seg_dist;
use ddc_geom::steel::check::{check, Finding};
use ddc_geom::steel::forces::{verdict, Verdict};
use ddc_geom::steel::schedule::{schedule, Schedule};
use ddc_geom::steel::{build, Conn, ConnKind, Params, Role};
use ddc_geom::V3;
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::solids::{form_json, frame_json, SolidIfc, SolidSpec};
use crate::structure::Built;
use crate::{Error, Model};

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS steel_conn (
    id INTEGER PRIMARY KEY,
    level INTEGER NOT NULL,
    kind TEXT NOT NULL,
    members TEXT NOT NULL,
    params TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS steel_conn_part (
    conn INTEGER NOT NULL,
    pos INTEGER NOT NULL,
    solid INTEGER NOT NULL,
    PRIMARY KEY (conn, pos)
);
";

/// Member kinds a connection can join.
pub const STEEL_MEMBER_KINDS: [&str; 3] = ["column", "beam", "brace"];

/// Columns and beams after their joins, then the braces.
fn conn_members(db: &Connection) -> Result<Vec<Built>, Error> {
    let mut v = crate::structure::build_all(db)?;
    v.extend(crate::framing::brace_members(db)?);
    Ok(v)
}

#[derive(Clone, Debug, PartialEq)]
pub struct SteelConnSpec {
    pub level: i64,
    pub kind: ConnKind,
    /// Joined members in the order of [`ConnKind::members_hint`]: (`column`|`beam`, id).
    pub members: Vec<(String, i64)>,
    pub params: Params,
}

/// A connection as built now: its parts, rule findings and the analysis status.
#[derive(Clone, Debug)]
pub struct SteelConnReport {
    pub spec: SteelConnSpec,
    /// None when the connection cannot be built; `error` says why.
    pub conn: Option<Conn>,
    pub error: Option<String>,
    pub findings: Vec<Finding>,
    pub status: Verdict,
}

pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

pub(crate) fn count(db: &Connection) -> Result<i64, Error> {
    Ok(db.query_row("SELECT count(*) FROM steel_conn", [], |r| r.get(0))?)
}

fn members_text(m: &[(String, i64)]) -> String {
    m.iter()
        .map(|(k, id)| format!("{k}:{id}"))
        .collect::<Vec<_>>()
        .join(";")
}

fn members_of(s: &str) -> Result<Vec<(String, i64)>, Error> {
    s.split(';')
        .filter(|x| !x.trim().is_empty())
        .map(|x| {
            let (k, id) = x
                .split_once(':')
                .ok_or_else(|| Error::Invalid(format!("steel_conn: member {x:?}")))?;
            let id = id
                .trim()
                .parse::<i64>()
                .map_err(|_| Error::Invalid(format!("steel_conn: member {x:?}")))?;
            Ok((k.trim().to_string(), id))
        })
        .collect()
}

type Row = (i64, SteelConnSpec);

fn rows(db: &Connection, only: Option<i64>) -> Result<Vec<Row>, Error> {
    rows_each(db, only)?
        .into_iter()
        .map(|r| r.map_err(|(_, e)| e))
        .collect()
}

/// Every row, a row that cannot be read as its id and the reason.
fn rows_each(db: &Connection, only: Option<i64>) -> Result<Vec<Result<Row, (i64, Error)>>, Error> {
    let mut st = db.prepare_cached(
        "SELECT id, level, kind, members, params FROM steel_conn          WHERE ?1 IS NULL OR id=?1 ORDER BY id",
    )?;
    let raw: Vec<(i64, i64, String, String, String)> = st
        .query_map([only], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<_, _>>()?;
    Ok(raw
        .into_iter()
        .map(|(id, level, kind, members, params)| {
            let parsed = (|| {
                let kind = ConnKind::parse(&kind)
                    .ok_or_else(|| Error::Invalid(format!("steel_conn {id}: type {kind:?}")))?;
                let params = Params::parse(&params).map_err(Error::Invalid)?;
                Ok(SteelConnSpec {
                    level,
                    kind,
                    members: members_of(&members)?,
                    params,
                })
            })();
            parsed.map(|s| (id, s)).map_err(|e| (id, e))
        })
        .collect())
}

/// Members by (kind, id).
fn index(members: &[Built]) -> HashMap<(&'static str, i64), &Member> {
    members.iter().map(|b| ((b.kind, b.id), &b.m)).collect()
}

/// The connection built on the current members.
fn build_on(
    idx: &HashMap<(&'static str, i64), &Member>,
    s: &SteelConnSpec,
) -> Result<(Conn, Vec<Member>), String> {
    let mut ms = vec![];
    for (k, id) in &s.members {
        let key = STEEL_MEMBER_KINDS.iter().find(|x| **x == k.as_str());
        match key.and_then(|k| idx.get(&(*k, *id))) {
            Some(m) => ms.push((*m).clone()),
            None => return Err(format!("{k} {id} is not a column, beam or brace of the model")),
        }
    }
    build(s.kind, &ms, &s.params)
        .map(|c| (c, ms))
        .map_err(|e| e.0)
}

/// Existing connections by their sorted member sets: one read of the table for a whole batch.
type Have = HashMap<Vec<(String, i64)>, (i64, ConnKind)>;

/// The connections of the model (but `except`), the lowest id first for a member set.
fn have(db: &Connection, except: Option<i64>) -> Result<Have, Error> {
    let mut out = Have::new();
    for (id, s) in rows_each(db, None)?.into_iter().flatten() {
        if Some(id) != except {
            out.entry(sorted(&s.members)).or_insert((id, s.kind));
        }
    }
    Ok(out)
}

fn taken(id: i64, kind: ConnKind) -> Error {
    Error::Invalid(format!(
        "steel_conn: these members already have connection {id} ({}); change or delete it instead",
        kind.key()
    ))
}

fn spec_check(db: &Connection, s: &SteelConnSpec, except: Option<i64>) -> Result<(), Error> {
    let built = conn_members(db)?;
    spec_check_on(db, &index(&built), &have(db, except)?, s)
}

fn spec_check_on(
    db: &Connection,
    idx: &HashMap<(&'static str, i64), &Member>,
    have: &Have,
    s: &SteelConnSpec,
) -> Result<(), Error> {
    if let Some(&(id, kind)) = have.get(&sorted(&s.members)) {
        return Err(taken(id, kind));
    }
    let (lo, hi) = s.kind.arity();
    if s.members.len() < lo || s.members.len() > hi {
        return Err(Error::Invalid(format!(
            "steel_conn: {} joins {lo}..{hi} members ({})",
            s.kind.key(),
            s.kind.members_hint()
        )));
    }
    let lv: i64 = db
        .prepare_cached("SELECT count(*) FROM level WHERE id=?1")?
        .query_row([s.level], |r| r.get(0))?;
    if lv == 0 {
        return Err(Error::Invalid(format!("steel_conn: no level {}", s.level)));
    }
    build_on(idx, s)
        .map(drop)
        .map_err(|e| Error::Invalid(format!("steel_conn: {e}")))
}

fn insert(db: &Connection, s: &SteelConnSpec) -> Result<i64, Error> {
    db.execute(
        "INSERT INTO steel_conn(level, kind, members, params) VALUES (?1,?2,?3,?4)",
        params![
            s.level,
            s.kind.key(),
            members_text(&s.members),
            s.params.text()
        ],
    )?;
    Ok(db.last_insert_rowid())
}

/// Part solids of a connection: (pos, solid id).
fn parts_of(db: &Connection, conn: i64) -> Result<Vec<(i64, i64)>, Error> {
    let mut st =
        db.prepare_cached("SELECT pos, solid FROM steel_conn_part WHERE conn=?1 ORDER BY pos")?;
    let r = st.query_map([conn], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(r.collect::<Result<_, _>>()?)
}

thread_local! {
    static REBUILT: Cell<u64> = const { Cell::new(0) };
}

/// Connections regeneration has rebuilt on this thread so far; the others kept their parts
/// because their members, spec and level were as at their last build.
pub fn steel_conn_rebuilds() -> u64 {
    REBUILT.with(Cell::get)
}

/// Session cache (TEMP: never saved, so a newer builder always rebuilds an opened file):
/// conn -> hash of what its parts were built from, and the issues that build reported.
const KEY_DDL: &str = "CREATE TEMP TABLE IF NOT EXISTS steel_conn_key(\
    conn INTEGER PRIMARY KEY, key TEXT NOT NULL, issues TEXT NOT NULL, parts INTEGER NOT NULL, \
    sig TEXT NOT NULL)";
const ISSUE_SEP: char = '\u{1f}';

/// Hash of everything a connection's parts depend on: its id (part names), spec, level
/// elevation and the joined members as built now.
fn build_key(idx: &HashMap<(&'static str, i64), &Member>, id: i64, s: &SteelConnSpec, elev: f64) -> String {
    let mut h = DefaultHasher::new();
    (id, s.level, s.kind.key(), members_text(&s.members), s.params.text()).hash(&mut h);
    elev.to_bits().hash(&mut h);
    for (k, mid) in &s.members {
        let m = STEEL_MEMBER_KINDS
            .iter()
            .find(|x| **x == k.as_str())
            .and_then(|k| idx.get(&(*k, *mid)));
        format!("{m:?}").hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

/// Fingerprint of a connection's part solids in position order (level, name, material,
/// plane, definition, IFC class and type of each); equal for the built specs and for the
/// stored rows exactly when the rows are as the build left them.
fn parts_sig<'a>(keys: impl IntoIterator<Item = &'a SolidKey>) -> String {
    let mut h = DefaultHasher::new();
    let mut n = 0usize;
    for k in keys {
        k.hash(&mut h);
        n += 1;
    }
    n.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Issues of the last build when it was made from `key` and its part rows and solids are
/// still as that build wrote them (an id reused after a delete, or a row edited or classed
/// differently since, does not match); None when it has to be rebuilt.
fn cached(db: &Connection, id: i64, key: &str) -> Result<Option<String>, Error> {
    let hit: Option<(String, i64, String)> = db
        .prepare_cached("SELECT issues, parts, sig FROM steel_conn_key WHERE conn=?1 AND key=?2")?
        .query_row(params![id, key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    let Some((issues, parts, sig)) = hit else {
        return Ok(None);
    };
    let old = parts_of(db, id)?;
    if old.len() as i64 != parts {
        return Ok(None);
    }
    let mut keys = Vec::with_capacity(old.len());
    for (_, sid) in &old {
        match stored(db, *sid)? {
            Some(k) => keys.push(k),
            None => return Ok(None),
        }
    }
    Ok((parts_sig(&keys) == sig).then_some(issues))
}

/// Rebuilds the parts of every connection whose members, spec or level changed since its
/// last build; returns the inverse of the solid and part-row edits. Problems go to
/// `struct_issue` (kind `steel_conn`).
pub(crate) fn sync(db: &Connection, members: &[Built]) -> Result<Inv, Error> {
    sync_in(db, members, None, &mut vec![])
}

/// [`sync`] of the connections `ids` alone (rows an operation wrote; the others' members did
/// not change): their issues are replaced, and the solids written or deleted go to `touched`.
pub(crate) fn sync_ids(
    db: &Connection,
    members: &[Built],
    ids: &BTreeSet<i64>,
    touched: &mut Vec<i64>,
) -> Result<Inv, Error> {
    let mut gone = db.prepare_cached("DELETE FROM struct_issue WHERE kind='steel_conn' AND ref=?1")?;
    for id in ids {
        gone.execute([id])?;
    }
    sync_in(db, members, Some(ids), touched)
}

fn sync_in(
    db: &Connection,
    members: &[Built],
    only: Option<&BTreeSet<i64>>,
    touched: &mut Vec<i64>,
) -> Result<Inv, Error> {
    let _s = crate::perf::span("steel_conn.sync");
    let mut inv = Inv::default();
    if count(db)? == 0 {
        return Ok(inv);
    }
    db.execute_batch(KEY_DDL)?;
    db.execute(
        "DELETE FROM steel_conn_key WHERE conn NOT IN (SELECT id FROM steel_conn)",
        [],
    )?;
    let lv: HashMap<i64, f64> = crate::walls::levels(db)?.into_iter().collect();
    let braces = crate::framing::brace_members(db)?;
    let mut idx = index(members);
    idx.extend(index(&braces));
    let mut issue = db
        .prepare_cached("INSERT INTO struct_issue(kind,ref,problem) VALUES ('steel_conn',?1,?2)")?;
    let all = match only {
        None => rows_each(db, None)?,
        Some(ids) => {
            let mut v = vec![];
            for &id in ids {
                v.extend(rows_each(db, Some(id))?);
            }
            v
        }
    };
    for row in all {
        let (id, s) = match row {
            Ok(r) => r,
            Err((id, e)) => {
                issue.execute(params![id, e.to_string()])?;
                continue;
            }
        };
        let elev = lv.get(&s.level).copied().unwrap_or(0.0);
        let key = build_key(&idx, id, &s, elev);
        if let Some(text) = cached(db, id, &key)? {
            for t in text.split(ISSUE_SEP).filter(|t| !t.is_empty()) {
                issue.execute(params![id, t])?;
            }
            continue;
        }
        REBUILT.with(|c| c.set(c.get() + 1));
        let mut said: Vec<String> = vec![];
        let specs: Vec<SolidSpec> = match build_on(&idx, &s) {
            Ok((c, ms)) => {
                for f in check(&c, &ms).iter().filter(|f| f.rule.is_error()) {
                    said.push(f.text.clone());
                }
                solids_of(&c, id, s.level, elev)
            }
            Err(e) => {
                said.push(e);
                vec![]
            }
        };
        for t in &said {
            issue.execute(params![id, t])?;
        }
        let sig = parts_sig(&specs.iter().map(key_of).collect::<Vec<_>>());
        db.prepare_cached(
            "INSERT OR REPLACE INTO steel_conn_key(conn,key,issues,parts,sig) VALUES (?1,?2,?3,?4,?5)",
        )?
        .execute(params![id, key, said.join(&ISSUE_SEP.to_string()), specs.len() as i64, sig])?;
        let old = parts_of(db, id)?;
        let mut rows_inv = None;
        let mut fresh = vec![];
        for (pos, spec) in specs.iter().enumerate() {
            let have = old.iter().find(|(p, _)| *p == pos as i64).map(|x| x.1);
            let current = match have {
                Some(sid) => stored(db, sid)?,
                None => None,
            };
            let want = key_of(spec);
            match (have, current) {
                (Some(sid), Some(cur)) if cur == want => fresh.push((pos as i64, sid)),
                (Some(sid), Some(_)) => {
                    inv = inv.restore(db, "solid", "id", sid)?;
                    crate::solids::write(db, spec, Some(sid))?;
                    fresh.push((pos as i64, sid));
                    touched.push(sid);
                }
                _ => {
                    let sid = crate::solids::write(db, spec, None)?;
                    inv = inv.delete("solid", "id", sid);
                    fresh.push((pos as i64, sid));
                    touched.push(sid);
                    rows_inv.get_or_insert(());
                }
            }
        }
        for (pos, sid) in &old {
            if *pos as usize >= specs.len() || !fresh.iter().any(|f| f.1 == *sid) {
                inv = inv.restore(db, "solid", "id", *sid)?;
                inv = crate::recipe::drop_role_comp(db, *sid, inv)?;
                db.prepare_cached("DELETE FROM solid WHERE id=?1")?.execute([sid])?;
                touched.push(*sid);
                rows_inv.get_or_insert(());
            }
        }
        if rows_inv.is_some() || old.len() != fresh.len() {
            // undo drops the rows written below, then puts the old ones back
            inv = inv
                .delete("steel_conn_part", "conn", id)
                .restore(db, "steel_conn_part", "conn", id)?;
            db.execute("DELETE FROM steel_conn_part WHERE conn=?1", [id])?;
            let mut ins =
                db.prepare_cached("INSERT INTO steel_conn_part(conn,pos,solid) VALUES (?1,?2,?3)")?;
            for (pos, sid) in &fresh {
                ins.execute(params![id, pos, sid])?;
            }
        }
    }
    Ok(inv)
}

/// What a solid row is compared by: level, name, material, plane, definition text and IFC
/// class and type (a row from before the classes is rewritten with them).
type SolidKey = (i64, String, String, String, String, Option<String>, Option<String>);

fn key_of(s: &SolidSpec) -> SolidKey {
    (
        s.level,
        s.name.clone(),
        s.material.clone(),
        frame_json(&s.frame).to_string(),
        form_json(&s.form).to_string(),
        s.ifc.as_ref().map(|k| k.class.clone()),
        s.ifc.as_ref().map(|k| k.predefined.clone()),
    )
}

fn stored(db: &Connection, sid: i64) -> Result<Option<SolidKey>, Error> {
    Ok(db
        .query_row(
            "SELECT level, name, material, plane, def, ifc_class, ifc_type FROM solid WHERE id=?1",
            [sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .optional()?)
}

/// IFC class of a connection part: plates as IfcPlate, bolts and anchors (with their heads,
/// nuts and washers) as IfcMechanicalFastener; welds and space-frame nodes stay proxies.
pub(crate) fn part_ifc(role: Role) -> Option<SolidIfc> {
    let (class, pre) = match role {
        r if r.is_plate() => ("IfcPlate", None),
        Role::Bolt => ("IfcMechanicalFastener", Some("BOLT")),
        Role::Anchor => ("IfcMechanicalFastener", Some("ANCHORBOLT")),
        r if r.is_fastener_piece() => ("IfcMechanicalFastener", None),
        _ => return None,
    };
    SolidIfc::new(class, pre).ok()
}

/// Solid rows of a built connection; the frame origin is lowered to the level.
fn solids_of(c: &Conn, id: i64, level: i64, elev: f64) -> Vec<SolidSpec> {
    c.parts
        .iter()
        .map(|p| {
            let mut frame = p.frame;
            frame.o.z -= elev;
            SolidSpec {
                level,
                name: format!("{} {} {} {}", c.kind.key(), id, p.role.key(), p.mark),
                void: false,
                material: p.material.clone(),
                ifc: part_ifc(p.role),
                frame,
                form: p.form.clone(),
            }
        })
        .collect()
}

/// Deletes a connection with its part solids (inside an op).
fn remove(db: &Connection, id: i64) -> Result<Inv, Error> {
    let mut inv = Inv::default().restore(db, "steel_conn", "id", id)?;
    inv = inv.restore(db, "steel_conn_part", "conn", id)?;
    for (_, sid) in parts_of(db, id)? {
        inv = inv.restore(db, "solid", "id", sid)?;
        inv = crate::recipe::drop_role_comp(db, sid, inv)?;
        db.execute("DELETE FROM solid WHERE id=?1", [sid])?;
    }
    db.execute("DELETE FROM steel_conn_part WHERE conn=?1", [id])?;
    db.execute("DELETE FROM steel_conn WHERE id=?1", [id])?;
    Ok(inv)
}

/// Endpoints of a straight member's axis.
fn ends(m: &Member) -> Option<(V3, V3)> {
    match m.path {
        Path::Line { a, b } => Some((a, b)),
        Path::Arc { .. } => None,
    }
}

fn near(a: V3, b: V3, tol: f64) -> bool {
    (a - b).len() <= tol
}

/// Larger side of a member's section.
fn span(m: &Member) -> f64 {
    let (w, h) = m.shape.size();
    w.max(h)
}

/// A point on a millimetre grid (to visit each node once).
fn grid(p: V3) -> (i64, i64, i64) {
    (
        (p.x * 1e3).round() as i64,
        (p.y * 1e3).round() as i64,
        (p.z * 1e3).round() as i64,
    )
}

/// Side of the cells members are filed in for neighbour lookups, m.
const CELL: f64 = 2.0;

type Key3 = (i64, i64, i64);

/// Members filed by the cells of points at most a metre apart along them, so a lookup reads
/// the members of the 27 cells around a point, not all of them. A member within
/// `size - 0.5` of a point is found (each point of its axis is within 0.5 of a sample).
struct Cells {
    size: f64,
    at: HashMap<Key3, Vec<usize>>,
}

impl Cells {
    fn new(ms: &[&Built], size: f64) -> Cells {
        let mut c = Cells { size, at: HashMap::new() };
        for (i, m) in ms.iter().enumerate() {
            let Some((a, b)) = ends(&m.m) else { continue };
            for k in c.along(a, b) {
                c.at.entry(k).or_default().push(i);
            }
        }
        c
    }

    fn cell(&self, p: V3) -> Key3 {
        let f = |v: f64| (v / self.size).floor() as i64;
        (f(p.x), f(p.y), f(p.z))
    }

    /// The cells of points a metre apart along a segment, each once.
    fn along(&self, a: V3, b: V3) -> Vec<Key3> {
        let n = ((b - a).len().ceil() as usize).max(1);
        let mut v: Vec<Key3> = (0..=n).map(|i| self.cell(a + (b - a) * (i as f64 / n as f64))).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Members filed in or next to the cells `ks`, in their order in the list.
    fn around(&self, ks: &[Key3]) -> Vec<usize> {
        let mut out = vec![];
        for k in ks {
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(v) = self.at.get(&(k.0 + dx, k.1 + dy, k.2 + dz)) {
                            out.extend_from_slice(v);
                        }
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Candidate connections the model's members suggest (bulk placement).
pub(crate) fn candidates(
    members: &[Built],
    kind: Option<ConnKind>,
    level: Option<i64>,
) -> Vec<SteelConnSpec> {
    let want = |k: ConnKind| kind.is_none_or(|x| x == k);
    let at_level = |l: i64| level.is_none_or(|x| x == l);
    let cols: Vec<&Built> = members.iter().filter(|b| b.kind == "column").collect();
    let beams: Vec<&Built> = members.iter().filter(|b| b.kind == "beam").collect();
    let line = |b: &Built| ends(&b.m);
    let reff = |b: &Built| (b.kind.to_string(), b.id);
    let mut out = vec![];
    let tol = 0.002;
    // the widest reach below is one section: cells wide enough that the 27 around a point hold it
    let size = members.iter().map(|b| span(&b.m) + 0.6).fold(CELL, f64::max);
    let col_cells = Cells::new(&cols, size);
    let beam_cells = Cells::new(&beams, size);
    for c in &cols {
        let Some((a, top)) = line(c) else { continue };
        let below = col_cells
            .around(&[col_cells.cell(a)])
            .into_iter()
            .map(|i| cols[i])
            .filter(|o| o.id != c.id)
            .find(|o| line(o).is_some_and(|(_, t)| near(t, a, tol)));
        match below {
            None if want(ConnKind::BasePlate) && at_level(c.level) => out.push(SteelConnSpec {
                level: c.level,
                kind: ConnKind::BasePlate,
                members: vec![reff(c)],
                params: Params::default(),
            }),
            Some(lo) if want(ConnKind::ColumnSplice) && at_level(c.level) => {
                out.push(SteelConnSpec {
                    level: c.level,
                    kind: ConnKind::ColumnSplice,
                    members: vec![reff(lo), reff(c)],
                    params: Params::default(),
                })
            }
            _ => {}
        }
        // beam ends on this column, between its ends
        for b in beam_cells.around(&col_cells.along(a, top)).into_iter().map(|i| beams[i]) {
            let Some((p, q)) = line(b) else { continue };
            // beams may stop at the column face (joins): within the section plus a margin
            let reach = span(&c.m) / 2.0 + 0.01;
            for e in [p, q] {
                if seg_dist(a, top, e, e) > reach || e.z < a.z + 0.05 || e.z > top.z - 0.05 {
                    continue;
                }
                let steep = (q - p).normalized().z.abs() > 0.2;
                let k = if steep {
                    ConnKind::BracingGusset
                } else {
                    ConnKind::Pinned
                };
                if !(want(k) && at_level(b.level)) {
                    continue;
                }
                out.push(SteelConnSpec {
                    level: b.level,
                    kind: k,
                    members: vec![reff(c), reff(b)],
                    params: Params::default(),
                });
            }
        }
    }
    // beam ends meeting away from columns: splices of two in line, ball nodes of three or more
    let mut seen: HashSet<(i64, i64, i64)> = HashSet::new();
    for b in &beams {
        let Some((p, q)) = line(b) else { continue };
        for e in [p, q] {
            if seen.contains(&grid(e)) {
                continue;
            }
            let here = [beam_cells.cell(e)];
            let on_col = col_cells.around(&here).into_iter().map(|i| cols[i]).any(|c| {
                line(c).is_some_and(|(a, t)| seg_dist(a, t, e, e) <= span(&c.m) / 2.0 + 0.01)
            });
            if on_col {
                continue;
            }
            // ends cut back by joins still meet within the deeper section
            let meet: Vec<&Built> = beam_cells
                .around(&here)
                .into_iter()
                .map(|i| beams[i])
                .filter(|o| {
                    let r = span(&b.m).max(span(&o.m)).max(tol);
                    line(o).is_some_and(|(s, t)| near(s, e, r) || near(t, e, r))
                })
                .collect();
            for m in &meet {
                if let Some((s, t)) = line(m) {
                    let r = span(&b.m).max(span(&m.m)).max(tol);
                    for x in [s, t].into_iter().filter(|x| near(*x, e, r)) {
                        seen.insert(grid(x));
                    }
                }
            }
            let lvl = meet[0].level;
            if !at_level(lvl) {
                continue;
            }
            if meet.len() == 2 {
                let d = |x: &Built| line(x).map_or(V3::default(), |(s, t)| (t - s).normalized());
                if d(meet[0]).dot(d(meet[1])).abs() > 0.98 && want(ConnKind::BeamSplice) {
                    out.push(SteelConnSpec {
                        level: lvl,
                        kind: ConnKind::BeamSplice,
                        members: vec![reff(meet[0]), reff(meet[1])],
                        params: Params::default(),
                    });
                }
            } else if meet.len() >= 3 && want(ConnKind::SphereNode) {
                out.push(SteelConnSpec {
                    level: lvl,
                    kind: ConnKind::SphereNode,
                    members: meet.iter().map(|m| reff(m)).collect(),
                    params: Params::default(),
                });
            }
        }
    }
    out.extend(brace_nodes(members, kind, level, &mut vec![]));
    out
}

/// A brace end the candidate search offers no connection for, and why: the connection audit
/// lists these, so no end goes without one silently.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnSkip {
    pub member: (String, i64),
    /// The member the connection would sit on, when there is one.
    pub host: Option<(String, i64)>,
    pub reason: String,
}

/// Gusset plates where braces end: on a column or beam through the node (bracing gusset), or,
/// where only braces meet (K and X nodes), on one of them as the chord (truss gusset). One
/// plate per plane and side of the host. A brace in line with the host is spliced to it
/// (column or beam splice). An end that gets nothing goes to `skips` with the reason.
fn brace_nodes(
    members: &[Built],
    kind: Option<ConnKind>,
    level: Option<i64>,
    skips: &mut Vec<ConnSkip>,
) -> Vec<SteelConnSpec> {
    let braces: Vec<&Built> = members.iter().filter(|b| b.kind == "brace").collect();
    if braces.is_empty() {
        return vec![];
    }
    let offer = |k: ConnKind| kind.is_none_or(|x| x == k);
    // hosts by the 2 m cells of points along them
    let cell = |p: V3| ((p.x / 2.0).floor() as i64, (p.y / 2.0).floor() as i64, (p.z / 2.0).floor() as i64);
    let mut cells: HashMap<(i64, i64, i64), Vec<&Built>> = HashMap::new();
    for h in members.iter().filter(|b| b.kind == "column" || b.kind == "beam") {
        let Some((a, b)) = ends(&h.m) else { continue };
        let n = ((b - a).len().ceil() as usize).max(1);
        let mut seen = HashSet::new();
        for i in 0..=n {
            let c = cell(a + (b - a) * (i as f64 / n as f64));
            if seen.insert(c) {
                cells.entry(c).or_default().push(h);
            }
        }
    }
    let mut at: BTreeMap<(i64, i64, i64), Vec<(&Built, V3, V3)>> = BTreeMap::new();
    for b in &braces {
        let Some((p, q)) = ends(&b.m) else { continue };
        at.entry(grid(p)).or_default().push((*b, p, (q - p).normalized()));
        at.entry(grid(q)).or_default().push((*b, q, (p - q).normalized()));
    }
    let reff = |b: &Built| (b.kind.to_string(), b.id);
    let mut out = vec![];
    for here in at.values() {
        let node = here[0].1;
        // a node on the edge of a cell may lie within the half-width of a host sampled in the next
        let c0 = cell(node);
        let near = (-1..=1).flat_map(|dx| (-1..=1).flat_map(move |dy| (-1..=1).map(move |dz| (c0.0 + dx, c0.1 + dy, c0.2 + dz))));
        let host = near
            .filter_map(|c| cells.get(&c))
            .flatten()
            .filter(|h| ends(&h.m).is_some_and(|(a, t)| seg_dist(a, t, node, node) <= span(&h.m) / 2.0 + 0.01))
            .min_by_key(|h| (h.kind != "column", h.id))
            .copied();
        let (host, k, max_webs) = match host {
            Some(h) => (h, ConnKind::BracingGusset, 4),
            None => (here.iter().map(|e| e.0).min_by_key(|b| b.id).unwrap(), ConnKind::TrussGusset, 7),
        };
        if level.is_some_and(|l| l != host.level) {
            continue;
        }
        let skip = |b: &Built, why: String| ConnSkip { member: reff(b), host: Some(reff(host)), reason: why };
        let Some((a, t)) = ends(&host.m) else {
            skips.extend(here.iter().filter(|e| e.0.id != host.id || e.0.kind != host.kind).map(|e| skip(e.0, "the host has no axis".into())));
            continue;
        };
        let u = (t - a).normalized();
        // webs by the side of the host they lie on: the plane normal with its sign
        let mut sides: BTreeMap<(i64, i64, i64), Vec<(&Built, f64)>> = BTreeMap::new();
        let mut in_line: Vec<&Built> = vec![];
        for (b, _, away) in here {
            if b.kind == host.kind && b.id == host.id {
                continue;
            }
            if away.dot(u).abs() > 0.98 {
                in_line.push(*b);
                continue;
            }
            let n = u.cross(*away).normalized();
            let q = |x: f64| (x * 20.0).round() as i64;
            sides.entry((q(n.x), q(n.y), q(n.z))).or_default().push((*b, away.dot(u).abs()));
        }
        // a brace along the host continues it: no gusset plane, a splice joins them
        let sk = if host.kind == "column" { ConnKind::ColumnSplice } else { ConnKind::BeamSplice };
        for b in in_line {
            if !offer(sk) {
                continue;
            }
            match build(sk, &[host.m.clone(), b.m.clone()], &Params::default()) {
                Ok(_) => out.push(SteelConnSpec {
                    level: host.level,
                    kind: sk,
                    members: vec![reff(host), reff(b)],
                    params: Params::default(),
                }),
                Err(e) => skips.push(skip(b, format!("in line with the {}, no {}: {e}", host.kind, sk.key()))),
            }
        }
        if !offer(k) {
            continue;
        }
        for webs in sides.values_mut() {
            // the gusset reads a second member square to the host as a corner host: steep
            // members go first, and members all square to the host get a plate each
            webs.sort_by(|x, y| y.1.total_cmp(&x.1));
            let per = if webs[0].1 < 0.1 { 1 } else { max_webs };
            let webs: Vec<&Built> = webs.iter().map(|w| w.0).collect();
            let builds = |chunk: &[&Built]| {
                let mut ms = vec![host.m.clone()];
                ms.extend(chunk.iter().map(|b| b.m.clone()));
                build(k, &ms, &Params::default()).map(|_| ()).map_err(|e| e.to_string())
            };
            for chunk in webs.chunks(per) {
                // one web the shared plate cannot take must not cost the others theirs: a
                // refused group falls back to a plate per web, and a refused web is reported
                let mut groups: Vec<Vec<&Built>> = vec![];
                if builds(chunk).is_ok() {
                    groups.push(chunk.to_vec());
                } else {
                    for b in chunk.iter().copied() {
                        match builds(std::slice::from_ref(&b)) {
                            Ok(()) => groups.push(vec![b]),
                            Err(e) => skips.push(skip(b, format!("no {} on the {}: {e}", k.key(), host.kind))),
                        }
                    }
                }
                for g in groups {
                    let mut refs = vec![reff(host)];
                    refs.extend(g.iter().map(|b| reff(b)));
                    out.push(SteelConnSpec { level: host.level, kind: k, members: refs, params: Params::default() });
                }
            }
        }
    }
    out
}

impl Model {
    /// Adds a connection after building it once on the current members (a refusal says why).
    pub fn add_steel_conn(&mut self, s: &SteelConnSpec) -> Result<i64, Error> {
        Ok(self.add_steel_conns(std::slice::from_ref(s))?[0])
    }

    /// Adds several connections as one edit (one undo step).
    pub fn add_steel_conns(&mut self, ss: &[SteelConnSpec]) -> Result<Vec<i64>, Error> {
        if ss.is_empty() {
            return Ok(vec![]);
        }
        let built = conn_members(&self.db)?;
        let idx = index(&built);
        let have = have(&self.db, None)?;
        // the members of each spec by their sorted refs: one hash lookup, not a scan of the
        // specs before it (thousands of gussets on a tower)
        let mut seen: HashMap<Vec<(String, i64)>, ConnKind> = HashMap::new();
        for s in ss {
            spec_check_on(&self.db, &idx, &have, s)?;
            if let Some(o) = seen.insert(sorted(&s.members), s.kind) {
                return Err(Error::Invalid(format!(
                    "steel_conn: {} and {} join the same members",
                    o.key(),
                    s.kind.key()
                )));
            }
        }
        self.insert_conns(ss)
    }

    /// A connection at every free node whose members are all `ours` (a frame just placed): the
    /// candidates of the members around the frame only, so a second frame in the model costs
    /// what the first did. Specs that would be refused (no build on these members, a member set
    /// joined already or twice) are left out, not fatal; one undo step.
    pub(crate) fn connect_members_of(&mut self, ours: &HashSet<(&str, i64)>) -> Result<Vec<i64>, Error> {
        let pick = crate::perf::span("conn.pick");
        let built = conn_members(&self.db)?;
        let mut lo = [f64::MAX; 3];
        let mut hi = [f64::MIN; 3];
        for b in built.iter().filter(|b| ours.contains(&(b.kind, b.id))) {
            for p in ends(&b.m).into_iter().flat_map(|(a, t)| [a, t]) {
                for (k, v) in [p.x, p.y, p.z].into_iter().enumerate() {
                    lo[k] = lo[k].min(v);
                    hi[k] = hi[k].max(v);
                }
            }
        }
        // members reaching into the frame's box stay in: a node shared with them is not free
        let pad = CELL + built.iter().map(|b| span(&b.m)).fold(0.0, f64::max);
        let near: Vec<Built> = built
            .into_iter()
            .filter(|b| {
                ends(&b.m).is_some_and(|(a, t)| {
                    [(a.x, t.x), (a.y, t.y), (a.z, t.z)]
                        .iter()
                        .enumerate()
                        .all(|(k, &(u, v))| u.min(v) <= hi[k] + pad && u.max(v) >= lo[k] - pad)
                })
            })
            .collect();
        let idx = index(&near);
        let have = have(&self.db, None)?;
        let mut seen = HashSet::new();
        let specs: Vec<SteelConnSpec> = candidates(&near, None, None)
            .into_iter()
            .filter(|s| s.members.iter().all(|(c, i)| ours.contains(&(c.as_str(), *i))))
            .filter(|s| spec_check_on(&self.db, &idx, &have, s).is_ok())
            .filter(|s| seen.insert(sorted(&s.members)))
            .collect();
        drop(pick);
        self.insert_conns(&specs)
    }

    /// Writes checked specs as one edit (one undo step).
    fn insert_conns(&mut self, ss: &[SteelConnSpec]) -> Result<Vec<i64>, Error> {
        if ss.is_empty() {
            return Ok(vec![]);
        }
        self.op("steel_conn_add", |db| {
            let mut ids = vec![];
            let mut inv = Inv::default();
            for s in ss {
                let id = insert(db, s)?;
                inv = inv.delete("steel_conn", "id", id);
                ids.push(id);
            }
            let first = ids[0];
            Ok((ids, first, inv))
        })
    }

    pub fn update_steel_conn(&mut self, id: i64, s: &SteelConnSpec) -> Result<(), Error> {
        spec_check(&self.db, s, Some(id))?;
        self.op("steel_conn_edit", |db| {
            if rows(db, Some(id))?.is_empty() {
                return Err(Error::Invalid(format!("steel_conn {id}: not found")));
            }
            let inv = Inv::default().restore(db, "steel_conn", "id", id)?;
            db.execute(
                "UPDATE steel_conn SET level=?2, kind=?3, members=?4, params=?5 WHERE id=?1",
                params![
                    id,
                    s.level,
                    s.kind.key(),
                    members_text(&s.members),
                    s.params.text()
                ],
            )?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_steel_conn(&mut self, id: i64) -> Result<(), Error> {
        self.op("steel_conn_delete", |db| {
            if rows(db, Some(id))?.is_empty() {
                return Err(Error::Invalid(format!("steel_conn {id}: not found")));
            }
            Ok(((), id, remove(db, id)?))
        })
    }

    pub fn steel_conns(&self) -> Result<Vec<(i64, SteelConnSpec)>, Error> {
        rows(&self.db, None)
    }

    /// Solid ids of a connection's parts, in part order.
    pub fn steel_conn_parts(&self, id: i64) -> Result<Vec<i64>, Error> {
        Ok(parts_of(&self.db, id)?.into_iter().map(|x| x.1).collect())
    }

    /// The connection built now, its rule findings and its analysis status (no results yet:
    /// "not checked by analysis").
    pub fn steel_conn_report(&self, id: i64) -> Result<SteelConnReport, Error> {
        let (_, spec) = rows(&self.db, Some(id))?
            .pop()
            .ok_or_else(|| Error::Invalid(format!("steel_conn {id}: not found")))?;
        self.steel_conn_preview(spec)
    }

    /// A connection of `spec` built on the current members without storing it: what
    /// [`Self::steel_conn_report`] would say once it is added.
    pub fn steel_conn_preview(&self, spec: SteelConnSpec) -> Result<SteelConnReport, Error> {
        let built = conn_members(&self.db)?;
        Ok(match build_on(&index(&built), &spec) {
            Ok((c, ms)) => SteelConnReport {
                findings: check(&c, &ms),
                status: verdict(&c),
                conn: Some(c),
                error: None,
                spec,
            },
            Err(e) => SteelConnReport {
                spec,
                conn: None,
                error: Some(e),
                findings: vec![],
                status: Verdict::NotChecked,
            },
        })
    }

    /// Bolt, weld and plate schedule of all connections (or of `ids`).
    pub fn steel_schedule(&self, ids: Option<&[i64]>) -> Result<Schedule, Error> {
        let built = conn_members(&self.db)?;
        let idx = index(&built);
        let conns: Vec<Conn> = rows(&self.db, None)?
            .into_iter()
            .filter(|(id, _)| ids.is_none_or(|x| x.contains(id)))
            .filter_map(|(_, s)| build_on(&idx, &s).ok().map(|x| x.0))
            .collect();
        Ok(schedule(conns.iter()))
    }

    /// Connections the members suggest that the model does not have yet, of `kind` (all
    /// types when None) and at `level` (all when None).
    pub fn steel_conn_candidates(
        &self,
        kind: Option<ConnKind>,
        level: Option<i64>,
    ) -> Result<Vec<SteelConnSpec>, Error> {
        let built = conn_members(&self.db)?;
        // a pair of members takes one connection, whatever its type
        let have: HashSet<Vec<(String, i64)>> = rows_each(&self.db, None)?
            .into_iter()
            .flatten()
            .map(|(_, s)| sorted(&s.members))
            .collect();
        Ok(candidates(&built, kind, level)
            .into_iter()
            .filter(|s| !have.contains(&sorted(&s.members)))
            .collect())
    }

    /// The connection audit: every brace end at a node that no candidate connection takes,
    /// with the member, its host and the reason.
    pub fn steel_conn_audit(&self) -> Result<Vec<ConnSkip>, Error> {
        let built = conn_members(&self.db)?;
        let mut skips = vec![];
        brace_nodes(&built, None, None, &mut skips);
        Ok(skips)
    }
}

fn sorted(m: &[(String, i64)]) -> Vec<(String, i64)> {
    let mut v = m.to_vec();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SteelConnSpec {
        SteelConnSpec {
            level: 1,
            kind: ConnKind::Pinned,
            members: vec![("column".into(), 1), ("beam".into(), 2)],
            params: Params::default(),
        }
    }

    #[test]
    fn build_key_differs_in_every_spec_input() {
        let idx = HashMap::new();
        let base = build_key(&idx, 7, &spec(), 0.0);
        assert_eq!(base, build_key(&idx, 7, &spec(), 0.0));
        assert_ne!(base, build_key(&idx, 8, &spec(), 0.0), "id");
        assert_ne!(base, build_key(&idx, 7, &spec(), 3.0), "elevation");
        let mut s = spec();
        s.level = 2;
        assert_ne!(base, build_key(&idx, 7, &s, 0.0), "level");
        let mut s = spec();
        s.kind = ConnKind::BasePlate;
        assert_ne!(base, build_key(&idx, 7, &s, 0.0), "kind");
        let mut s = spec();
        s.members[1].1 = 3;
        assert_ne!(base, build_key(&idx, 7, &s, 0.0), "members");
        let mut s = spec();
        s.params.bolt = Some(0.02);
        assert_ne!(base, build_key(&idx, 7, &s, 0.0), "params");
    }

    #[test]
    fn parts_sig_differs_when_only_the_ifc_class_differs() {
        let k = |class: Option<&str>| -> SolidKey {
            (1, "n".into(), "steel".into(), "{}".into(), "{}".into(), class.map(Into::into), None)
        };
        let a = parts_sig(&[k(Some("IfcPlate"))]);
        assert_eq!(a, parts_sig(&[k(Some("IfcPlate"))]));
        assert_ne!(a, parts_sig(&[k(None)]), "class removed");
        assert_ne!(a, parts_sig(&[k(Some("IfcMechanicalFastener"))]), "class changed");
        assert_ne!(a, parts_sig(&[k(Some("IfcPlate")), k(Some("IfcPlate"))]), "part count");
    }
}
