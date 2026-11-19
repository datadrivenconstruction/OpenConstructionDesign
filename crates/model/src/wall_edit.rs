// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Editing walls and curtain walls (additive, no format version change): tables are created on
//! first access if absent (`ensure`); old files read as "no edits".
//! `wall_join` is the joint type/ban at a wall end · `stack_type`/`stack_part` are stacked walls
//! by height · `wall_sweep` are profiles along the wall · `wall_region`/`wall_wrap` are the vertical
//! structure of a type · `curtain_edit` are curtain wall grid edits (segments, pins, panels).

use ddc_geom::curtain::Section;
use ddc_geom::curtain_edit::{CurtainEdit, PanelType};
use ddc_geom::layered::JoinPrefs;
use ddc_geom::v2;
use ddc_geom::wall_edit::{
    stack_tiers, EndPrefs, FacadeHole, JoinKind, Region, StackPart, Structure, Sweep, SweepOn,
    Tier, WallOpening, Wrap,
};
use rusqlite::{params, types::Value, Connection, OptionalExtension};

use crate::curtain::{parse_section, section_text};
use crate::integrity::Issue;
use crate::ops::Inv;
use crate::{Error, Model, OpeningSpec, Swing};

const TABLES: &[(&str, &str)] = &[
    ("wall_join", "wall INTEGER NOT NULL, end_ INTEGER NOT NULL, kind TEXT NOT NULL, allowed INTEGER NOT NULL, PRIMARY KEY(wall, end_)"),
    ("stack_type", "id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE"),
    ("stack_part", "stack INTEGER NOT NULL, pos INTEGER NOT NULL, wtype INTEGER NOT NULL, height REAL, offset REAL NOT NULL, PRIMARY KEY(stack, pos)"),
    ("wall_sweep", "id INTEGER PRIMARY KEY, wall INTEGER NOT NULL, reveal INTEGER NOT NULL, left_ INTEGER NOT NULL, s0 REAL NOT NULL, s1 REAL NOT NULL, profile TEXT NOT NULL"),
    ("wall_region", "wtype INTEGER NOT NULL, idx INTEGER NOT NULL, layer INTEGER NOT NULL, z0 REAL NOT NULL, z1 REAL NOT NULL, material INTEGER NOT NULL, PRIMARY KEY(wtype, idx)"),
    ("wall_wrap", "wtype INTEGER PRIMARY KEY, at_inserts TEXT NOT NULL, at_ends TEXT NOT NULL, wraps TEXT NOT NULL, core0 INTEGER NOT NULL, core1 INTEGER NOT NULL"),
    ("curtain_edit", "cw INTEGER PRIMARY KEY, state TEXT NOT NULL"),
    ("wall_stack", "wall INTEGER PRIMARY KEY, stack INTEGER NOT NULL"),
    (
        "wall_layer_flag",
        "wtype INTEGER NOT NULL, pos INTEGER NOT NULL, core INTEGER NOT NULL DEFAULT 0,          priority INTEGER, PRIMARY KEY(wtype, pos)",
    ),
];

/// Extra columns of the edit tables (added if present): stable references from structure
/// regions to layer rows (rowid). Core model tables are unchanged.
const COLUMNS: &[(&str, &str, &str)] = &[
    ("wall_region", "lrid", "INTEGER"),
    ("wall_region", "mrid", "INTEGER"),
    // layer function key (`wall_types::LayerFn`); NULL: guessed from the material
    ("wall_layer_flag", "function", "TEXT"),
];

use crate::schema::has_col;

thread_local! {
    static TABLE_MEMO: std::cell::RefCell<Option<std::collections::HashMap<String, bool>>> =
        const { std::cell::RefCell::new(None) };
}

/// The schema does not change during a level computation: `has_table` queries the catalog once,
/// not several times per wall (the catalog is read by a full scan).
pub(crate) struct TableMemo {
    owner: bool,
}

impl TableMemo {
    pub(crate) fn start() -> Self {
        let owner = TABLE_MEMO.with_borrow_mut(|t| {
            let free = t.is_none();
            if free {
                *t = Some(Default::default());
            }
            free
        });
        TableMemo { owner }
    }
}

impl Drop for TableMemo {
    fn drop(&mut self) {
        if self.owner {
            TABLE_MEMO.with_borrow_mut(|t| *t = None);
        }
    }
}

fn has_table(db: &Connection, name: &str) -> Result<bool, Error> {
    if let Some(known) = TABLE_MEMO.with_borrow(|t| t.as_ref().and_then(|m| m.get(name).copied())) {
        return Ok(known);
    }
    let found = db
        .prepare_cached("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1")?
        .query_row([name], |_| Ok(()))
        .optional()?
        .is_some();
    TABLE_MEMO.with_borrow_mut(|t| {
        if let Some(m) = t {
            m.insert(name.to_string(), found);
        }
    });
    Ok(found)
}

/// Migration by table presence: creates only the missing ones.
pub(crate) fn ensure(db: &Connection) -> Result<(), Error> {
    for (t, cols) in TABLES {
        if !has_table(db, t)? {
            db.execute_batch(&format!("CREATE TABLE {t}({cols})"))?;
        }
    }
    for (t, c, ty) in COLUMNS {
        if !has_col(db, t, c)? {
            db.execute_batch(&format!("ALTER TABLE {t} ADD COLUMN {c} {ty}"))?;
        }
    }
    Ok(())
}

/// Joint preferences of walls `ids` by end (for `layered::join_with`).
pub(crate) fn join_prefs(db: &Connection, ids: &[i64]) -> Result<Vec<JoinPrefs>, Error> {
    let mut out = vec![[None, None]; ids.len()];
    if !has_table(db, "wall_join")? {
        return Ok(out);
    }
    let mut st = db.prepare_cached("SELECT end_, kind, allowed FROM wall_join WHERE wall=?1")?;
    for (k, id) in ids.iter().enumerate() {
        let rows: Vec<(i64, String, bool)> = st
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        for (end, kind, allowed) in rows {
            if let (Some(slot), Some(kind)) = (out[k].get_mut(end as usize), JoinKind::parse(&kind))
            {
                *slot = Some(EndPrefs { kind, allowed });
            }
        }
    }
    Ok(out)
}

/// Type layers: (rowid, core, priority). Without the columns the core is unset and priority is the default.
pub(crate) fn layer_meta(db: &Connection, wtype: i64) -> Result<Vec<(i64, bool, u8)>, Error> {
    let sql = if has_table(db, "wall_layer_flag")? {
        "SELECT l.rowid, COALESCE(f.core, 0), f.priority FROM wall_type_layer l          LEFT JOIN wall_layer_flag f ON f.wtype=l.wtype AND f.pos=l.pos          WHERE l.wtype=?1 ORDER BY l.pos"
    } else {
        "SELECT rowid, 0, NULL FROM wall_type_layer WHERE wtype=?1 ORDER BY pos"
    };
    let mut st = db.prepare_cached(sql)?;
    let v = st
        .query_map([wtype], |r| {
            let p: Option<i64> = r.get(2)?;
            Ok((
                r.get(0)?,
                r.get::<_, i64>(1)? != 0,
                p.map_or(ddc_geom::layered::Layer::DEFAULT_PRIORITY, |p| {
                    p.clamp(0, 255) as u8
                }),
            ))
        })?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

/// The saved edit state of a curtain wall.
pub(crate) fn curtain_edit_db(db: &Connection, cw: i64) -> Result<Option<CurtainEdit>, Error> {
    if !has_table(db, "curtain_edit")? {
        return Ok(None);
    }
    let s: Option<String> = db
        .query_row("SELECT state FROM curtain_edit WHERE cw=?1", [cw], |r| {
            r.get(0)
        })
        .optional()?;
    s.map(|s| parse_curtain(&s)).transpose()
}

/// Wall deletion: its joints, profiles and stacked type go with it (undo restores them).
pub(crate) fn drop_wall(db: &Connection, mut inv: Inv, wall: i64) -> Result<Inv, Error> {
    for t in ["wall_join", "wall_sweep", "wall_stack"] {
        if has_table(db, t)? {
            inv = inv.then(inv_row(db, t, "wall=?1", &[Value::Integer(wall)])?);
            db.execute(&format!("DELETE FROM {t} WHERE wall=?1"), [wall])?;
        }
    }
    Ok(inv)
}

/// Dangling references of the edit tables (existing tables only).
pub(crate) fn issues(db: &Connection) -> Result<Vec<Issue>, Error> {
    let refs = [
        ("wall_join", "wall", "wall"),
        ("wall_sweep", "wall", "wall"),
        ("wall_stack", "wall", "wall"),
        ("wall_stack", "stack", "stack_type"),
        ("stack_part", "stack", "stack_type"),
        ("stack_part", "wtype", "wall_type"),
        ("wall_region", "wtype", "wall_type"),
        ("wall_wrap", "wtype", "wall_type"),
        ("wall_layer_flag", "wtype", "wall_type"),
        ("curtain_edit", "cw", "curtain_wall"),
    ];
    let mut out = vec![];
    for (child, col, parent) in refs {
        if !has_table(db, child)? || !has_table(db, parent)? {
            continue;
        }
        let mut st = db.prepare(&format!(
            "SELECT rowid, {col} FROM {child} WHERE {col} NOT IN (SELECT id FROM {parent}) ORDER BY rowid"
        ))?;
        let rows: Vec<(i64, i64)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        out.extend(rows.into_iter().map(|(id, to)| Issue {
            table: child.into(),
            id,
            problem: format!("{col} -> missing {parent} {to}"),
        }));
    }
    if has_table(db, "wall_region")? && has_col(db, "wall_region", "lrid")? {
        let mut st = db.prepare(
            "SELECT rowid FROM wall_region WHERE lrid IS NOT NULL AND lrid NOT IN (SELECT rowid FROM wall_type_layer)",
        )?;
        let rows: Vec<i64> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        out.extend(rows.into_iter().map(|id| Issue {
            table: "wall_region".into(),
            id,
            problem: "region of a deleted layer".into(),
        }));
    }
    Ok(out)
}

/// A wall for edit-driven quantity corrections: axis length, height, layers, openings on the elevation.
pub(crate) struct WallQ<'a> {
    pub id: i64,
    pub wtype: i64,
    pub len: f64,
    pub height: f64,
    pub mats: &'a [String],
    pub thick: &'a [f64],
    pub holes: Vec<FacadeHole>,
    /// Radius by offset from the axis (arc), for the Pappus correction.
    pub radius: Option<Box<dyn Fn(f64) -> f64 + 'a>>,
    pub offset0: f64,
    /// Free (unjoined) ends, the return at the end faces.
    pub free_ends: usize,
}

pub(crate) type QtyRow = (i64, i64, String, f64, f64);

/// Edit-driven corrections of a wall's `wall_qty` rows: profiles (a protrusion is a "sweep" row, a groove is cut from
/// the layers of that face), structure regions (volume moves to the region's material), layer returns at
/// openings and free end faces, a stacked type (tier rows instead of layers).
pub(crate) fn adjust_rows(db: &Connection, w: &WallQ, rows: &mut Vec<QtyRow>) -> Result<(), Error> {
    let n = w.thick.len();
    let mine: Vec<usize> = (0..rows.len()).filter(|&k| rows[k].0 == w.id).collect();
    if mine.len() != n || n == 0 {
        return Ok(());
    }
    if let Some(tiers) = wall_tiers(db, w.id, w.height)? {
        rows.retain(|r| r.0 != w.id);
        for (k, (t, name)) in tiers.iter().enumerate() {
            let band = t.z1 - t.z0;
            let cut: f64 = w
                .holes
                .iter()
                .map(|h| (h.1 - h.0) * (h.3.min(t.z1) - h.2.max(t.z0)).max(0.0))
                .sum();
            rows.push((
                w.id,
                3000 + k as i64,
                format!("stack:{name}"),
                t.thickness * w.len,
                (t.thickness * (w.len * band - cut)).max(0.0),
            ));
        }
        return Ok(());
    }
    let total: f64 = w.thick.iter().sum();
    let s = structure_of(db, w.wtype, w.height)?;
    // returns at openings and free end faces (within the type's layers, with no double counting at joints)
    let mut delta = vec![0.0; n];
    for h in &w.holes {
        let o = WallOpening {
            s0: h.0,
            s1: h.1,
            z0: h.2,
            z1: h.3,
        };
        for (d, x) in delta
            .iter_mut()
            .zip(s.wrap_delta(s.at_inserts, o.jamb_perimeter()))
        {
            *d += x;
        }
    }
    for _ in 0..w.free_ends {
        for (d, x) in delta.iter_mut().zip(s.wrap_delta(s.at_ends, w.height)) {
            *d += x;
        }
    }
    for (l, &k) in mine.iter().enumerate() {
        rows[k].4 = (rows[k].4 + delta[l]).max(0.0);
    }
    // profiles: face "left" is at the last layer (offset0 + total), "right" at the first
    let sweeps = sweeps_db(db, w.id)?;
    let mut extra = vec![];
    for (k, (_, sw)) in sweeps.iter().enumerate() {
        let on = SweepOn {
            sweep: sw,
            len: w.len,
            h: w.height,
            holes: &w.holes,
        };
        let face = if sw.left {
            w.offset0 + total
        } else {
            w.offset0
        };
        let inward = if sw.left { -1.0 } else { 1.0 };
        let k_arc = |u: f64| match &w.radius {
            Some(r) => {
                let r0 = r(0.0);
                if r0.abs() > 1e-12 {
                    r(face + inward * u) / r0
                } else {
                    1.0
                }
            }
            None => 1.0,
        };
        if sw.reveal {
            let order: Vec<usize> = if sw.left {
                (0..n).rev().collect()
            } else {
                (0..n).collect()
            };
            let from_face: Vec<f64> = order.iter().map(|&i| w.thick[i]).collect();
            for (c, &i) in on.cut_per_layer(&from_face, &k_arc).iter().zip(&order) {
                let r = &mut rows[mine[i]];
                r.4 = (r.4 - c).max(0.0);
            }
        } else {
            let v = on.added_volume(&k_arc);
            if v > 0.0 {
                extra.push((w.id, 1000 + k as i64, "sweep".to_string(), 0.0, v));
            }
        }
    }
    // structure regions: a share of the layer volume moves to the region's material
    let default = s.regions.len() == n && s.regions.iter().all(|r| r.material == r.layer);
    if !default {
        let shares = s.shares();
        for (l, sh) in shares.iter().enumerate() {
            let base = rows[mine[l]].4;
            let mut moved = 0.0;
            for (j, &(m, f)) in sh.iter().enumerate() {
                if m != l && f > 0.0 {
                    let v = base * f;
                    moved += v;
                    extra.push((w.id, 2000 + (l * 100 + j) as i64, w.mats[m].clone(), 0.0, v));
                }
            }
            rows[mine[l]].4 = (base - moved).max(0.0);
        }
    }
    rows.extend(extra);
    Ok(())
}

/// Tiers of a stacked wall (the type is assigned to the wall) or None.
pub(crate) fn wall_tiers(
    db: &Connection,
    wall: i64,
    height: f64,
) -> Result<Option<Vec<(Tier, String)>>, Error> {
    if !has_table(db, "wall_stack")? {
        return Ok(None);
    }
    let stack: Option<i64> = db
        .query_row("SELECT stack FROM wall_stack WHERE wall=?1", [wall], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(stack) = stack else { return Ok(None) };
    let mut st = db.prepare_cached(
        "SELECT p.wtype, p.height, p.offset, t.name FROM stack_part p JOIN wall_type t ON t.id=p.wtype \
         WHERE p.stack=?1 ORDER BY p.pos",
    )?;
    let rows: Vec<(i64, Option<f64>, f64, String)> = st
        .query_map([stack], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    let mut parts = vec![];
    for (t, h, o, _) in &rows {
        parts.push(StackPart {
            name: t.to_string(),
            thickness: Model::type_thickness(db, *t)?.iter().sum(),
            height: *h,
            offset: *o,
        });
    }
    let tiers = stack_tiers(&parts, 0.0, height).map_err(Error::Invalid)?;
    Ok(Some(
        tiers
            .into_iter()
            .zip(rows.into_iter().map(|r| r.3))
            .collect(),
    ))
}

fn sweeps_db(db: &Connection, wall: i64) -> Result<Vec<(i64, Sweep)>, Error> {
    if !has_table(db, "wall_sweep")? {
        return Ok(vec![]);
    }
    let mut st = db.prepare_cached(
        "SELECT id, reveal, left_, s0, s1, profile FROM wall_sweep WHERE wall=?1 ORDER BY id",
    )?;
    let raw: Vec<(i64, bool, bool, f64, f64, String)> = st
        .query_map([wall], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    raw.into_iter()
        .map(|(id, reveal, left, s0, s1, p)| {
            Ok((
                id,
                Sweep {
                    reveal,
                    left,
                    s0,
                    s1,
                    profile: parse_profile(&p)?,
                },
            ))
        })
        .collect()
}

/// The vertical structure of a type on a wall of height `height`.
pub(crate) fn structure_of(db: &Connection, wtype: i64, height: f64) -> Result<Structure, Error> {
    let t = Model::type_thickness(db, wtype)?;
    let n = t.len();
    let meta = layer_meta(db, wtype)?;
    let core: Vec<usize> = (0..n)
        .filter(|&i| meta.get(i).is_some_and(|m| m.1))
        .collect();
    let mut s = Structure::new(t, (0, n), height);
    if let (Some(&a), Some(&b)) = (core.first(), core.last()) {
        s.core = (a, b + 1);
    }
    if !has_table(db, "wall_region")? {
        return Ok(s);
    }
    let pos_of = |rid: Option<i64>| rid.and_then(|r| meta.iter().position(|m| m.0 == r));
    let rid_cols = has_col(db, "wall_region", "lrid")?;
    let sql = if rid_cols {
        "SELECT layer, z0, z1, material, lrid, mrid FROM wall_region WHERE wtype=?1 ORDER BY idx"
    } else {
        "SELECT layer, z0, z1, material, NULL, NULL FROM wall_region WHERE wtype=?1 ORDER BY idx"
    };
    let mut st = db.prepare(sql)?;
    let raw: Vec<(usize, f64, f64, usize, Option<i64>, Option<i64>)> = st
        .query_map([wtype], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    // the layer-row reference outranks the index: if the layer is deleted, the region disappears
    let regions: Vec<Region> = raw
        .into_iter()
        .filter_map(|(layer, z0, z1, material, lr, mr)| {
            let layer = match lr {
                Some(_) => pos_of(lr)?,
                None => layer,
            };
            let material = match mr {
                Some(_) => pos_of(mr)?,
                None => material,
            };
            (layer < n && material < n).then_some(Region {
                layer,
                z0,
                z1,
                material,
            })
        })
        .collect();
    if !regions.is_empty() {
        s.regions = regions;
    }
    let wr: Option<(String, String, String, usize, usize)> = if has_table(db, "wall_wrap")? {
        db.query_row(
            "SELECT at_inserts, at_ends, wraps, core0, core1 FROM wall_wrap WHERE wtype=?1",
            [wtype],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?
    } else {
        None
    };
    if let Some((a, e, wraps, c0, c1)) = wr {
        s.at_inserts = Wrap::parse(&a);
        s.at_ends = Wrap::parse(&e);
        s.wraps = (0..n)
            .map(|i| wraps.as_bytes().get(i) == Some(&b'1'))
            .collect();
        if core.is_empty() {
            s.core = (c0.min(n), c1.min(n).max(c0.min(n)));
        }
    }
    Ok(s.fit(height))
}

/// Changes to one wall from the edit panel: one model operation, one undo.
#[derive(Clone, Debug, Default)]
pub struct WallChange {
    pub wall: i64,
    /// (wall, end, joint): a node's joint may belong to a neighbouring wall (T-junction).
    pub joins: Vec<(i64, u8, EndPrefs)>,
    /// (opening, along, width, sill, height, side/hinges).
    pub openings: Vec<(i64, f64, f64, f64, f64, Option<Swing>)>,
    pub sweeps_add: Vec<Sweep>,
    pub sweeps_set: Vec<(i64, Sweep)>,
    pub sweeps_del: Vec<i64>,
    pub structure: Option<(i64, Structure)>,
    /// Some(None) removes the stacked type.
    pub stack: Option<Option<i64>>,
    pub voids: Vec<WallOpening>,
}

fn check_sweep(s: &Sweep) -> Result<(), Error> {
    let fin = s.profile.iter().all(|p| p.x.is_finite() && p.y.is_finite());
    if s.profile.len() < 3 || !fin || !(s.s1 > s.s0) || !s.s0.is_finite() || !s.s1.is_finite() {
        return Err(Error::Invalid(
            "sweep needs a finite profile of 3+ points and a positive run".into(),
        ));
    }
    if ddc_geom::wall_edit::area(&s.profile) <= 1e-12 {
        return Err(Error::Invalid("sweep profile has no area".into()));
    }
    Ok(())
}

fn write_regions(db: &Connection, wtype: i64, s: &Structure) -> Result<(), Error> {
    let meta = layer_meta(db, wtype)?;
    db.execute("DELETE FROM wall_region WHERE wtype=?1", [wtype])?;
    for (k, r) in s.regions.iter().enumerate() {
        db.execute(
            "INSERT INTO wall_region(wtype,idx,layer,z0,z1,material,lrid,mrid) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                wtype,
                k as i64,
                r.layer as i64,
                r.z0,
                r.z1,
                r.material as i64,
                meta.get(r.layer).map(|m| m.0),
                meta.get(r.material).map(|m| m.0)
            ],
        )?;
    }
    let wraps: String = s.wraps.iter().map(|b| if *b { '1' } else { '0' }).collect();
    db.execute(
        "INSERT OR REPLACE INTO wall_wrap(wtype,at_inserts,at_ends,wraps,core0,core1) VALUES (?1,?2,?3,?4,?5,?6)",
        params![wtype, s.at_inserts.as_str(), s.at_ends.as_str(), wraps, s.core.0 as i64, s.core.1 as i64],
    )?;
    Ok(())
}

fn inv_row(db: &Connection, table: &str, cond: &str, args: &[Value]) -> Result<Inv, Error> {
    // snapshot of the row (or its absence): undo deletes the new one and restores the old
    let mut st = db.prepare(&format!("SELECT * FROM {table} WHERE {cond}"))?;
    let names: Vec<String> = st.column_names().map(|c| c.to_string()).collect();
    let rows: Vec<Vec<Value>> = st
        .query_map(rusqlite::params_from_iter(args.iter()), |r| {
            (0..names.len()).map(|i| r.get::<_, Value>(i)).collect()
        })?
        .collect::<Result<_, _>>()?;
    let mut inv = Inv::default().sql(&format!("DELETE FROM {table} WHERE {cond}"), args.to_vec());
    let ph = (1..=names.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    for r in rows {
        inv = inv.sql(
            &format!("INSERT INTO {table}({}) VALUES ({ph})", names.join(",")),
            r,
        );
    }
    Ok(inv)
}

fn profile_text(p: &[ddc_geom::V2]) -> String {
    p.iter()
        .map(|q| format!("{} {}", q.x, q.y))
        .collect::<Vec<_>>()
        .join(";")
}

/// Profile "u z;u z;...": a malformed point or a non-finite number is an error, not skipped.
fn parse_profile(s: &str) -> Result<Vec<ddc_geom::V2>, Error> {
    let bad = |t: &str| Error::Invalid(format!("bad sweep profile point '{t}'"));
    let pts = s
        .split(';')
        .filter(|t| !t.trim().is_empty())
        .map(|t| {
            let v: Vec<f64> = t
                .split_whitespace()
                .map(|x| x.parse::<f64>().map_err(|_| bad(t)))
                .collect::<Result<_, _>>()?;
            match v[..] {
                [x, y] if x.is_finite() && y.is_finite() => Ok(v2(x, y)),
                _ => Err(bad(t)),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    if pts.len() < 3 {
        return Err(Error::Invalid("sweep profile needs 3+ points".into()));
    }
    Ok(pts)
}

fn ptype_char(t: PanelType) -> &'static str {
    t.as_str()
}

/// Grid edit state text: "key values" lines, one per edit.
fn curtain_text(e: &CurtainEdit) -> String {
    let nums = |v: &[f64]| {
        v.iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let bits = |v: &[bool]| {
        v.iter()
            .map(|b| if *b { "1" } else { "0" })
            .collect::<String>()
    };
    let mut out = vec![
        format!("u {}", nums(&e.u)),
        format!("v {}", nums(&e.v)),
        format!("pu {}", bits(&e.pin_u)),
        format!("pv {}", bits(&e.pin_v)),
        format!("in {}", section_text(&e.interior)),
        format!("bo {}", section_text(&e.border)),
        format!("tilt {}", e.tilt),
    ];
    for (k, c) in e.corner.iter().enumerate() {
        if let Some(c) = c {
            out.push(format!("corner {k} {}", section_text(c)));
        }
    }
    for s in &e.removed {
        out.push(format!("rm {} {} {}", s.0 as u8, s.1, s.2));
    }
    for ((i, j), t) in &e.panel {
        out.push(format!("panel {i} {j} {}", ptype_char(*t)));
    }
    for (s, m) in &e.mullion {
        out.push(format!(
            "mul {} {} {} {}",
            s.0 as u8,
            s.1,
            s.2,
            section_text(m)
        ));
    }
    out.join("\n")
}

fn parse_curtain(s: &str) -> Result<CurtainEdit, Error> {
    let bad = || Error::Invalid("damaged curtain edit state".into());
    let mut e = CurtainEdit::new(vec![], vec![], Section::None, Section::None);
    for line in s.lines() {
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        let w: Vec<&str> = rest.split_whitespace().collect();
        let n = |i: usize| {
            w.get(i)
                .and_then(|x| x.parse::<usize>().ok())
                .ok_or_else(bad)
        };
        match key {
            "u" | "v" => {
                let v = w
                    .iter()
                    .map(|x| x.parse::<f64>().map_err(|_| bad()))
                    .collect::<Result<Vec<_>, _>>()?;
                if key == "u" {
                    e.u = v
                } else {
                    e.v = v
                }
            }
            "pu" => e.pin_u = rest.chars().map(|c| c == '1').collect(),
            "pv" => e.pin_v = rest.chars().map(|c| c == '1').collect(),
            "in" => e.interior = parse_section(rest)?,
            "bo" => e.border = parse_section(rest)?,
            "tilt" => e.tilt = rest.parse().map_err(|_| bad())?,
            "corner" => {
                let k = n(0)?.min(1);
                e.corner[k] = Some(parse_section(&w[1..].join(" "))?);
            }
            "rm" => {
                e.removed.insert((n(0)? == 1, n(1)?, n(2)?));
            }
            "panel" => {
                let t = w.get(2).and_then(|x| PanelType::parse(x)).ok_or_else(bad)?;
                e.panel.insert((n(0)?, n(1)?), t);
            }
            "mul" => {
                e.mullion.insert(
                    (n(0)? == 1, n(1)?, n(2)?),
                    parse_section(&w[3..].join(" "))?,
                );
            }
            _ => return Err(bad()),
        }
    }
    if e.u.len() < 2 || e.v.len() < 2 || e.pin_u.len() != e.u.len() || e.pin_v.len() != e.v.len() {
        return Err(bad());
    }
    Ok(e)
}

impl Model {
    /// Joint type and permission at a wall end (`end`: 0 is start a, 1 is end b).
    pub fn set_wall_join(&mut self, wall: i64, end: u8, p: EndPrefs) -> Result<(), Error> {
        if end > 1 {
            return Err(Error::Invalid("wall end is 0 or 1".into()));
        }
        crate::wall_spline::refuse_inner_end(&self.db, wall, end)?;
        self.op("wall_edit_join", |db| {
            ensure(db)?;
            let args = [Value::Integer(wall), Value::Integer(end as i64)];
            let inv = inv_row(db, "wall_join", "wall=?1 AND end_=?2", &args)?;
            db.execute(
                "INSERT OR REPLACE INTO wall_join(wall,end_,kind,allowed) VALUES (?1,?2,?3,?4)",
                params![wall, end, p.kind.as_str(), p.allowed],
            )?;
            Ok(((), wall, inv))
        })
    }

    pub fn wall_join(&self, wall: i64, end: u8) -> Result<EndPrefs, Error> {
        if !has_table(&self.db, "wall_join")? {
            return Ok(EndPrefs::default());
        }
        let r: Option<(String, bool)> = self
            .db
            .query_row(
                "SELECT kind, allowed FROM wall_join WHERE wall=?1 AND end_=?2",
                params![wall, end],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(r.map_or_else(EndPrefs::default, |(k, a)| EndPrefs {
            kind: JoinKind::parse(&k).unwrap_or(JoinKind::Butt),
            allowed: a,
        }))
    }

    pub(crate) fn type_thickness(db: &Connection, wtype: i64) -> Result<Vec<f64>, Error> {
        let mut st =
            db.prepare_cached("SELECT thickness FROM wall_type_layer WHERE wtype=?1 ORDER BY pos")?;
        let v: Vec<f64> = st
            .query_map([wtype], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        if v.is_empty() {
            return Err(Error::Invalid(format!("no wall type {wtype}")));
        }
        Ok(v)
    }

    /// A stacked wall type: parts bottom to top (wall type, height or None for variable, offset).
    pub fn add_stack_type(
        &mut self,
        name: &str,
        parts: &[(i64, Option<f64>, f64)],
    ) -> Result<i64, Error> {
        // check before writing: exactly one variable part
        let probe: Vec<StackPart> = parts
            .iter()
            .map(|p| StackPart {
                name: String::new(),
                thickness: 1.0,
                height: p.1,
                offset: p.2,
            })
            .collect();
        let fixed: f64 = parts.iter().filter_map(|p| p.1).sum();
        stack_tiers(&probe, 0.0, fixed + 1.0).map_err(Error::Invalid)?;
        self.op("wall_edit_stack", |db| {
            ensure(db)?;
            for p in parts {
                Self::type_thickness(db, p.0)?;
            }
            db.execute("INSERT INTO stack_type(name) VALUES (?1)", [name])?;
            let id = db.last_insert_rowid();
            for (k, p) in parts.iter().enumerate() {
                db.execute(
                    "INSERT INTO stack_part(stack,pos,wtype,height,offset) VALUES (?1,?2,?3,?4,?5)",
                    params![id, k as i64, p.0, p.1, p.2],
                )?;
            }
            let inv =
                Inv::default()
                    .delete("stack_part", "stack", id)
                    .delete("stack_type", "id", id);
            Ok((id, id, inv))
        })
    }

    /// Tiers of a stacked wall with bottom `base` and total height `total`.
    pub fn stack_tiers(&self, stack: i64, base: f64, total: f64) -> Result<Vec<Tier>, Error> {
        if !has_table(&self.db, "stack_part")? {
            return Err(Error::Invalid(format!("no stack type {stack}")));
        }
        let mut st = self
            .db
            .prepare("SELECT wtype, height, offset FROM stack_part WHERE stack=?1 ORDER BY pos")?;
        let rows: Vec<(i64, Option<f64>, f64)> = st
            .query_map([stack], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        let parts = rows
            .iter()
            .map(|&(t, h, o)| {
                Ok(StackPart {
                    name: t.to_string(),
                    thickness: Self::type_thickness(&self.db, t)?.iter().sum(),
                    height: h,
                    offset: o,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        stack_tiers(&parts, base, total).map_err(Error::Invalid)
    }

    pub fn add_wall_sweep(&mut self, wall: i64, s: &Sweep) -> Result<i64, Error> {
        check_sweep(s)?;
        self.op("wall_edit_sweep", |db| {
            ensure(db)?;
            db.execute(
                "INSERT INTO wall_sweep(wall,reveal,left_,s0,s1,profile) VALUES (?1,?2,?3,?4,?5,?6)",
                params![wall, s.reveal, s.left, s.s0, s.s1, profile_text(&s.profile)],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, wall, Inv::default().delete("wall_sweep", "id", id)))
        })
    }

    pub fn wall_sweeps(&self, wall: i64) -> Result<Vec<(i64, Sweep)>, Error> {
        sweeps_db(&self.db, wall)
    }

    /// The vertical structure of a type on a wall of height `height` (regions clipped at the top).
    pub fn wall_structure(&self, wtype: i64, height: f64) -> Result<Structure, Error> {
        structure_of(&self.db, wtype, height)
    }

    /// Save the vertical structure of a type (regions, return) in one operation.
    pub fn set_wall_structure(&mut self, wtype: i64, s: &Structure) -> Result<(), Error> {
        let n = Self::type_thickness(&self.db, wtype)?.len();
        if s.regions
            .iter()
            .any(|r| r.layer >= n || r.material >= n || r.z1 <= r.z0)
        {
            return Err(Error::Invalid("region outside the wall type".into()));
        }
        self.op("wall_edit_structure", |db| {
            ensure(db)?;
            let a = [Value::Integer(wtype)];
            let inv = inv_row(db, "wall_region", "wtype=?1", &a)?.then(inv_row(
                db,
                "wall_wrap",
                "wtype=?1",
                &a,
            )?);
            write_regions(db, wtype, s)?;
            Ok(((), wtype, inv))
        })
    }

    /// A rectangular opening drawn on the wall elevation (a cut-out with no infill).
    pub fn add_wall_opening(&mut self, wall: i64, o: &WallOpening) -> Result<i64, Error> {
        self.add_opening(
            wall,
            &OpeningSpec {
                along: o.s0,
                width: o.s1 - o.s0,
                sill: o.z0,
                height: o.z1 - o.z0,
                kind: "void".into(),
            },
        )
    }

    /// Offset of the wall's right face from the axis (along the left normal); NULL means the wall is on the axis.
    pub fn wall_offset0(&self, wall: i64) -> Result<f64, Error> {
        let (wtype, off): (i64, Option<f64>) = self
            .db
            .query_row("SELECT wtype, offset0 FROM wall WHERE id=?1", [wall], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(|_| Error::Invalid(format!("no such wall {wall}")))?;
        let total: f64 = Self::type_thickness(&self.db, wtype)?.iter().sum();
        Ok(off.unwrap_or(-total / 2.0))
    }

    /// Type layer materials (outside to inside).
    pub fn wall_layer_materials(&self, wtype: i64) -> Result<Vec<String>, Error> {
        let mut st = self
            .db
            .prepare_cached("SELECT material FROM wall_type_layer WHERE wtype=?1 ORDER BY pos")?;
        let v = st
            .query_map([wtype], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Wall joint preferences by end (empty means the default joint).
    pub fn join_prefs(&self, ids: &[i64]) -> Result<Vec<JoinPrefs>, Error> {
        join_prefs(&self.db, ids)
    }

    /// Core and cleanup priority of type layers (outside to inside).
    pub fn wall_layer_flags(&self, wtype: i64) -> Result<Vec<(bool, u8)>, Error> {
        Ok(layer_meta(&self.db, wtype)?
            .into_iter()
            .map(|m| (m.1, m.2))
            .collect())
    }

    /// Core and priority of layer `pos` of a type (columns are added on the first write).
    pub fn set_wall_layer_flags(
        &mut self,
        wtype: i64,
        pos: i64,
        core: bool,
        priority: Option<u8>,
    ) -> Result<(), Error> {
        self.op("wall_edit_layer", |db| {
            ensure(db)?;
            let n: i64 = db.query_row(
                "SELECT count(*) FROM wall_type_layer WHERE wtype=?1 AND pos=?2",
                params![wtype, pos],
                |r| r.get(0),
            )?;
            if n == 0 {
                return Err(Error::Invalid(format!("no layer {pos} in wall type {wtype}")));
            }
            let inv = inv_row(db, "wall_layer_flag", "wtype=?1", &[Value::Integer(wtype)])?;
            db.execute(
                "INSERT OR REPLACE INTO wall_layer_flag(wtype,pos,core,priority) VALUES (?1,?2,?3,?4)",
                params![wtype, pos, core, priority],
            )?;
            Ok(((), wtype, inv))
        })
    }

    pub fn update_wall_sweep(&mut self, id: i64, s: &Sweep) -> Result<(), Error> {
        check_sweep(s)?;
        self.op("wall_edit_sweep", |db| {
            ensure(db)?;
            let inv = inv_row(db, "wall_sweep", "id=?1", &[Value::Integer(id)])?;
            let n = db.execute(
                "UPDATE wall_sweep SET reveal=?2,left_=?3,s0=?4,s1=?5,profile=?6 WHERE id=?1",
                params![id, s.reveal, s.left, s.s0, s.s1, profile_text(&s.profile)],
            )?;
            if n == 0 {
                return Err(Error::Invalid(format!("no sweep {id}")));
            }
            Ok(((), id, inv))
        })
    }

    pub fn delete_wall_sweep(&mut self, id: i64) -> Result<(), Error> {
        self.op("wall_edit_sweep", |db| {
            ensure(db)?;
            let inv = inv_row(db, "wall_sweep", "id=?1", &[Value::Integer(id)])?;
            if db.execute("DELETE FROM wall_sweep WHERE id=?1", [id])? == 0 {
                return Err(Error::Invalid(format!("no sweep {id}")));
            }
            Ok(((), id, inv))
        })
    }

    /// Assign a stacked type to a wall (None removes it).
    pub fn set_wall_stack(&mut self, wall: i64, stack: Option<i64>) -> Result<(), Error> {
        self.apply_wall_change(&WallChange {
            wall,
            stack: Some(stack),
            ..Default::default()
        })
    }

    pub fn wall_stack(&self, wall: i64) -> Result<Option<i64>, Error> {
        if !has_table(&self.db, "wall_stack")? {
            return Ok(None);
        }
        Ok(self
            .db
            .query_row("SELECT stack FROM wall_stack WHERE wall=?1", [wall], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Tiers of a wall with an assigned stacked type: (tier, part type name).
    pub fn wall_stack_tiers(
        &self,
        wall: i64,
        height: f64,
    ) -> Result<Option<Vec<(Tier, String)>>, Error> {
        wall_tiers(&self.db, wall, height)
    }

    /// Stacked types: (id, name).
    pub fn stack_types(&self) -> Result<Vec<(i64, String)>, Error> {
        if !has_table(&self.db, "stack_type")? {
            return Ok(vec![]);
        }
        let mut st = self
            .db
            .prepare("SELECT id, name FROM stack_type ORDER BY id")?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    pub fn opening_kind(&self, id: i64) -> Result<String, Error> {
        self.db
            .query_row("SELECT kind FROM opening WHERE id=?1", [id], |r| r.get(0))
            .map_err(|_| Error::Invalid(format!("no such opening {id}")))
    }

    /// All wall edit panel changes in one operation (one undo step).
    pub fn apply_wall_change(&mut self, c: &WallChange) -> Result<(), Error> {
        for (w, end, _) in &c.joins {
            if *end > 1 {
                return Err(Error::Invalid("wall end is 0 or 1".into()));
            }
            crate::wall_spline::refuse_inner_end(&self.db, *w, *end)?;
        }
        for s in c.sweeps_add.iter().chain(c.sweeps_set.iter().map(|x| &x.1)) {
            check_sweep(s)?;
        }
        if let Some((wtype, s)) = &c.structure {
            let n = Self::type_thickness(&self.db, *wtype)?.len();
            if s.regions
                .iter()
                .any(|r| r.layer >= n || r.material >= n || r.z1 <= r.z0)
            {
                return Err(Error::Invalid("region outside the wall type".into()));
            }
        }
        let wall = c.wall;
        self.op("wall_edit", |db| {
            ensure(db)?;
            let exists: Option<i64> = db
                .query_row("SELECT id FROM wall WHERE id=?1", [wall], |r| r.get(0))
                .optional()?;
            if exists.is_none() {
                return Err(Error::Invalid(format!("no such wall {wall}")));
            }
            let w = [Value::Integer(wall)];
            let mut inv = inv_row(db, "wall_join", "wall=?1", &w)?
                .then(inv_row(db, "wall_sweep", "wall=?1", &w)?)
                .then(inv_row(db, "wall_stack", "wall=?1", &w)?);
            let mut others: Vec<i64> = c.joins.iter().map(|j| j.0).filter(|&o| o != wall).collect();
            others.sort_unstable();
            others.dedup();
            for o in others {
                inv = inv.then(inv_row(db, "wall_join", "wall=?1", &[Value::Integer(o)])?);
            }
            for (jw, end, p) in &c.joins {
                db.execute(
                    "INSERT OR REPLACE INTO wall_join(wall,end_,kind,allowed) VALUES (?1,?2,?3,?4)",
                    params![jw, end, p.kind.as_str(), p.allowed],
                )?;
            }
            let mut holes: Vec<FacadeHole> = vec![];
            for &(id, along, width, sill, height, swing) in &c.openings {
                inv = Inv::default().restore(db, "opening", "id", id)?.then(inv);
                // the opening kind does not change: an empty opening stays empty
                let n = db.execute(
                    "UPDATE opening SET along=?2,width=?3,sill=?4,height=?5 WHERE id=?1 AND wall=?6",
                    params![id, along, width, sill, height, wall],
                )?;
                if n == 0 {
                    return Err(Error::Invalid(format!("no opening {id} on wall {wall}")));
                }
                if let Some(s) = swing {
                    db.execute(
                        "UPDATE opening SET swing_side=?2,swing_hinge=?3 WHERE id=?1",
                        params![id, s.side, s.hinge],
                    )?;
                }
                holes.push((along, along + width, sill, sill + height));
            }
            // changed openings must not intersect any opening of this wall (on the elevation)
            let mut st = db.prepare("SELECT id, along, width, sill, height FROM opening WHERE wall=?1")?;
            let all: Vec<(i64, FacadeHole)> = st
                .query_map([wall], |r| {
                    let (a, w, s, h): (f64, f64, f64, f64) = (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
                    Ok((r.get(0)?, (a, a + w, s, s + h)))
                })?
                .collect::<Result<_, _>>()?;
            drop(st);
            for (k, h) in holes.iter().enumerate() {
                let me = c.openings[k].0;
                if all.iter().any(|(id, o)| *id != me && ddc_geom::wall_edit::overlaps_2d(*h, *o)) {
                    return Err(Error::Invalid("openings overlap on the wall face".into()));
                }
            }
            for s in &c.sweeps_add {
                db.execute(
                    "INSERT INTO wall_sweep(wall,reveal,left_,s0,s1,profile) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![wall, s.reveal, s.left, s.s0, s.s1, profile_text(&s.profile)],
                )?;
            }
            for (id, s) in &c.sweeps_set {
                let n = db.execute(
                    "UPDATE wall_sweep SET reveal=?2,left_=?3,s0=?4,s1=?5,profile=?6 WHERE id=?1 AND wall=?7",
                    params![id, s.reveal, s.left, s.s0, s.s1, profile_text(&s.profile), wall],
                )?;
                if n == 0 {
                    return Err(Error::Invalid(format!("no sweep {id} on wall {wall}")));
                }
            }
            for id in &c.sweeps_del {
                db.execute("DELETE FROM wall_sweep WHERE id=?1 AND wall=?2", params![id, wall])?;
            }
            if let Some((wtype, s)) = &c.structure {
                let a = [Value::Integer(*wtype)];
                inv = inv
                    .then(inv_row(db, "wall_region", "wtype=?1", &a)?)
                    .then(inv_row(db, "wall_wrap", "wtype=?1", &a)?);
                write_regions(db, *wtype, s)?;
            }
            if let Some(stack) = c.stack {
                match stack {
                    Some(st) => {
                        let ok: Option<i64> = db
                            .query_row("SELECT id FROM stack_type WHERE id=?1", [st], |r| r.get(0))
                            .optional()?;
                        if ok.is_none() {
                            return Err(Error::Invalid(format!("no stack type {st}")));
                        }
                        db.execute(
                            "INSERT OR REPLACE INTO wall_stack(wall,stack) VALUES (?1,?2)",
                            params![wall, st],
                        )?;
                    }
                    None => {
                        db.execute("DELETE FROM wall_stack WHERE wall=?1", [wall])?;
                    }
                }
            }
            for o in &c.voids {
                db.execute(
                    "INSERT INTO opening(wall,along,width,sill,height,kind) VALUES (?1,?2,?3,?4,?5,'void')",
                    params![wall, o.s0, o.s1 - o.s0, o.z0, o.z1 - o.z0],
                )?;
                inv = inv.delete("opening", "id", db.last_insert_rowid());
            }
            Ok(((), wall, inv))
        })
    }

    pub fn save_curtain_edit(&mut self, cw: i64, e: &CurtainEdit) -> Result<(), Error> {
        self.op("curtain_panel", |db| {
            ensure(db)?;
            let inv = inv_row(db, "curtain_edit", "cw=?1", &[Value::Integer(cw)])?;
            db.execute(
                "INSERT OR REPLACE INTO curtain_edit(cw,state) VALUES (?1,?2)",
                params![cw, curtain_text(e)],
            )?;
            Ok(((), cw, inv))
        })
    }

    pub fn curtain_edit(&self, cw: i64) -> Result<Option<CurtainEdit>, Error> {
        curtain_edit_db(&self.db, cw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, WallSpec};
    use ddc_geom::wall_edit::rect_profile;

    fn model() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let ly = |t: f64| LayerSpec {
            material: "m".into(),
            thickness: t,
        };
        let t = m
            .add_wall_type("W", &[ly(0.02), ly(0.2), ly(0.01)])
            .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (6.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        (m, t, w)
    }

    #[test]
    fn join_prefs_roundtrip_and_undo() {
        let (mut m, _, w) = model();
        assert_eq!(m.wall_join(w, 1).unwrap(), EndPrefs::default());
        let p = EndPrefs {
            kind: JoinKind::Miter,
            allowed: false,
        };
        m.set_wall_join(w, 1, p).unwrap();
        m.set_wall_join(
            w,
            1,
            EndPrefs {
                kind: JoinKind::SquareOff,
                allowed: true,
            },
        )
        .unwrap();
        m.undo().unwrap();
        assert_eq!(m.wall_join(w, 1).unwrap(), p);
        m.undo().unwrap();
        assert_eq!(m.wall_join(w, 1).unwrap(), EndPrefs::default());
        assert!(m.set_wall_join(w, 2, p).is_err());
    }

    #[test]
    fn stacked_type_heights() {
        let (mut m, t, _) = model();
        let s = m
            .add_stack_type("S", &[(t, Some(1.0), 0.05), (t, None, 0.0)])
            .unwrap();
        let tiers = m.stack_tiers(s, 0.5, 4.0).unwrap();
        assert_eq!(
            tiers.iter().map(|x| (x.z0, x.z1)).collect::<Vec<_>>(),
            vec![(0.5, 1.5), (1.5, 4.5)]
        );
        assert!((tiers[0].thickness - 0.23).abs() < 1e-12);
        assert!(m
            .add_stack_type("X", &[(t, None, 0.0), (t, None, 0.0)])
            .is_err());
    }

    #[test]
    fn sweeps_structure_openings_curtain_persist() {
        let (mut m, t, w) = model();
        let sw = Sweep {
            profile: rect_profile(0.05, 0.03, 1.5, true),
            s0: 1.0,
            s1: 5.0,
            reveal: true,
            left: true,
        };
        m.add_wall_sweep(w, &sw).unwrap();
        let got = m.wall_sweeps(w).unwrap();
        assert_eq!(got[0].1, sw);
        let mut s = m.wall_structure(t, 3.0).unwrap();
        let up = s.split(0, 1.0).unwrap();
        s.assign(up, 2).unwrap();
        s.wraps[0] = true;
        s.at_inserts = Wrap::Both;
        m.set_wall_structure(t, &s).unwrap();
        let back = m.wall_structure(t, 4.0).unwrap();
        assert_eq!(back.regions[1].z1, 4.0);
        assert_eq!(back.at_inserts, Wrap::Both);
        assert!((back.volumes(1.0)[2] - (0.02 * 3.0 + 0.01 * 4.0)).abs() < 1e-12);
        m.undo().unwrap();
        assert_eq!(m.wall_structure(t, 3.0).unwrap().regions.len(), 3);
        let o = WallOpening::from_corners(v2(1.0, 0.9), v2(2.5, 2.1), 6.0, 3.0).unwrap();
        let id = m.add_wall_opening(w, &o).unwrap();
        assert!(id > 0);
        let mut e = CurtainEdit::new(
            vec![0.0, 2.0, 4.0],
            vec![0.0, 3.0],
            Section::Rect { w: 0.05, d: 0.1 },
            Section::Circle { d: 0.08 },
        );
        e.merge((0, 0), (1, 0)).unwrap();
        e.set_panel((0, 0), PanelType::Door).unwrap();
        e.corner[1] = Some(Section::Rect { w: 0.1, d: 0.1 });
        e.pin(true, 1, true).unwrap();
        m.save_curtain_edit(7, &e).unwrap();
        assert_eq!(m.curtain_edit(7).unwrap().unwrap(), e);
        m.undo().unwrap();
        assert_eq!(m.curtain_edit(7).unwrap(), None);
    }
    fn vol(m: &Model, w: i64) -> f64 {
        m.sql()
            .query_row("SELECT SUM(volume) FROM wall_qty WHERE wall=?1", [w], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn mat_vol(m: &Model, w: i64, mat: &str) -> f64 {
        m.sql()
            .query_row(
                "SELECT COALESCE(SUM(volume),0) FROM wall_qty WHERE wall=?1 AND material=?2",
                params![w, mat],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn wall(m: &mut Model, a: (f64, f64), b: (f64, f64), t: i64) -> i64 {
        let lv = m
            .sql()
            .query_row("SELECT id FROM level", [], |r| r.get(0))
            .unwrap();
        m.add_wall(&WallSpec {
            a,
            b,
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap()
    }

    #[test]
    fn joins_change_wall_volumes() {
        let (mut m, t, w) = model();
        let o = wall(&mut m, (6.0, 0.0), (6.0, 4.0), t);
        let (v0, o0) = (vol(&m, w), vol(&m, o));
        let butt = EndPrefs {
            kind: JoinKind::Butt,
            allowed: true,
        };
        m.set_wall_join(w, 1, butt).unwrap();
        let (v1, o1) = (vol(&m, w), vol(&m, o));
        assert!(
            (v1 - v0).abs() > 1e-6 || (o1 - o0).abs() > 1e-6,
            "join kind drives quantities"
        );
        m.set_wall_join(
            w,
            1,
            EndPrefs {
                allowed: false,
                ..butt
            },
        )
        .unwrap();
        assert!(vol(&m, w) + vol(&m, o) > 0.0);
        m.undo().unwrap();
        m.undo().unwrap();
        assert!((vol(&m, w) - v0).abs() < 1e-9);
        m.assert_integrity();
    }

    #[test]
    fn sweep_reveal_region_wrap_stack_drive_quantities() {
        let (mut m, _, w) = model();
        let v0 = vol(&m, w);
        // 0.1x0.1 cornice over the full length (a region wider than the wall is clipped): +0.06 m3
        let cornice = Sweep {
            profile: vec![v2(0.0, 2.8), v2(-0.1, 2.8), v2(-0.1, 2.9), v2(0.0, 2.9)],
            s0: -1.0,
            s1: 9.0,
            reveal: false,
            left: true,
        };
        m.add_wall_sweep(w, &cornice).unwrap();
        assert!((mat_vol(&m, w, "sweep") - 0.06).abs() < 1e-9);
        let groove = Sweep {
            profile: rect_profile(0.01, 0.02, 1.5, true),
            s0: 0.0,
            s1: 6.0,
            reveal: true,
            left: true,
        };
        let id = m.add_wall_sweep(w, &groove).unwrap();
        let v1 = vol(&m, w);
        assert!(v1 < v0 + 0.06 - 1e-9, "reveal cuts layers");
        let right = Sweep {
            left: false,
            ..groove.clone()
        };
        m.update_wall_sweep(id, &right).unwrap();
        assert!((vol(&m, w) - v1).abs() < 1e-9, "same cut on the other face");
        m.delete_wall_sweep(id).unwrap();
        assert!((vol(&m, w) - (v0 + 0.06)).abs() < 1e-9);

        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L1", 0.0).unwrap();
        let ly = |n: &str, t: f64| LayerSpec {
            material: n.into(),
            thickness: t,
        };
        let t = m
            .add_wall_type("W", &[ly("a", 0.02), ly("b", 0.2), ly("c", 0.01)])
            .unwrap();
        let w = wall(&mut m, (0.0, 0.0), (6.0, 0.0), t);
        // the region of layer 0 above 1 m takes the material of layer 2
        let a0 = mat_vol(&m, w, "a");
        let mut s = m.wall_structure(t, 3.0).unwrap();
        let up = s.split(0, 1.0).unwrap();
        s.assign(up, 2).unwrap();
        m.set_wall_structure(t, &s).unwrap();
        let a1 = mat_vol(&m, w, "a");
        assert!((a1 - a0 / 3.0).abs() < 1e-9, "{a0} {a1}");
        assert!(mat_vol(&m, w, "c") > 0.01 * 6.0 * 3.0 * 1.5);
        // outer layer return at an opening: layer "a" grows, "b" loses
        let mut s = m.wall_structure(t, 3.0).unwrap();
        s.core = (2, 3);
        s.wraps[0] = true;
        s.at_inserts = Wrap::Exterior;
        m.set_wall_structure(t, &s).unwrap();
        m.add_opening(
            w,
            &OpeningSpec {
                along: 2.0,
                width: 1.0,
                sill: 0.9,
                height: 1.2,
                kind: "window".into(),
            },
        )
        .unwrap();
        let (ap, bp) = (mat_vol(&m, w, "a"), mat_vol(&m, w, "b"));
        let mut s0 = s.clone();
        s0.wraps[0] = false;
        m.set_wall_structure(t, &s0).unwrap();
        assert!(
            mat_vol(&m, w, "a") < ap && mat_vol(&m, w, "b") > bp,
            "wrap applied"
        );
        // stacked type: tier rows instead of layers
        let st = m
            .add_stack_type("S", &[(t, Some(1.0), 0.0), (t, None, 0.0)])
            .unwrap();
        m.set_wall_stack(w, Some(st)).unwrap();
        assert_eq!(m.wall_stack(w).unwrap(), Some(st));
        let n: i64 = m
            .sql()
            .query_row(
                "SELECT count(*) FROM wall_qty WHERE wall=?1 AND pos>=3000",
                [w],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
        m.undo().unwrap();
        assert_eq!(m.wall_stack(w).unwrap(), None);
        m.assert_integrity();
    }

    #[test]
    fn delete_cascades_and_undo_restores() {
        let (mut m, t, w) = model();
        let p = EndPrefs {
            kind: JoinKind::Miter,
            allowed: false,
        };
        m.set_wall_join(w, 0, p).unwrap();
        let sw = Sweep {
            profile: rect_profile(0.02, 0.02, 1.0, false),
            s0: 0.0,
            s1: 6.0,
            reveal: false,
            left: false,
        };
        m.add_wall_sweep(w, &sw).unwrap();
        m.delete_wall(w).unwrap();
        m.assert_integrity();
        let w2 = wall(&mut m, (0.0, 0.0), (6.0, 0.0), t);
        assert!(
            m.wall_sweeps(w2).unwrap().is_empty(),
            "new wall inherits nothing"
        );
        assert_eq!(m.wall_join(w2, 0).unwrap(), EndPrefs::default());
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(m.wall_sweeps(w).unwrap().len(), 1);
        assert!(!m.wall_join(w, 0).unwrap().allowed);
        m.set_wall_layer_flags(t, 1, true, Some(3)).unwrap();
        assert_eq!(m.wall_layer_flags(t).unwrap()[1], (true, 3));
        assert!(m.set_wall_layer_flags(t, 9, true, None).is_err());
        m.add_stack_type("S", &[(t, None, 0.0)]).unwrap();
        assert!(m.delete_wall_type(t).is_err(), "part of a stacked type");
        assert!(parse_profile("0 0;1 x;1 1").is_err());
        assert!(parse_profile("0 0;1 0").is_err());
        assert_eq!(parse_profile("0 0;1 0;1 1").unwrap().len(), 3);
    }

    #[test]
    fn curtain_edit_drives_layout_and_cascades() {
        let mut m = Model::open(":memory:").unwrap();
        let level = m.add_level("L1", 0.0).unwrap();
        let s = Section::Rect { w: 0.05, d: 0.15 };
        let cw = m
            .add_curtain_wall(&crate::CurtainSpec {
                level,
                name: "C".into(),
                a: (0.0, 0.0),
                b: (6.0, 0.0),
                bulge: 0.0,
                base_offset: 0.0,
                height: 3.0,
                u: crate::GridLines::Spacing(1.5),
                v: crate::GridLines::Spacing(1.0),
                interior: s,
                border: s,
                lines: Default::default(),
                top: vec![],
            })
            .unwrap();
        let l0 = m.curtain_layout(cw).unwrap();
        let v: Vec<f64> = l0.layout.v.iter().map(|z| z - l0.z0).collect();
        let mut e = CurtainEdit::new(l0.layout.u.clone(), v, s, s);
        e.set_panel((0, 0), PanelType::Door).unwrap();
        let mid = (e.u[1] + e.u[2]) / 2.0;
        e.add_line(true, mid).unwrap();
        m.save_curtain_edit(cw, &e).unwrap();
        let l1 = m.curtain_layout(cw).unwrap();
        assert_eq!(l1.layout.u.len(), l0.layout.u.len() + 1);
        assert!(l1.panels.iter().any(|p| p.kind == crate::PanelKind::Door));
        m.delete_curtain_wall(cw).unwrap();
        assert!(m.curtain_edit(cw).unwrap().is_none());
        m.assert_integrity();
        m.undo().unwrap();
        assert!(m.curtain_edit(cw).unwrap().is_some());
    }

    #[test]
    fn apply_wall_change_is_one_step_and_keeps_void() {
        let (mut m, _, w) = model();
        let o = WallOpening::from_corners(v2(1.0, 0.5), v2(2.0, 1.5), 6.0, 3.0).unwrap();
        let id = m.add_wall_opening(w, &o).unwrap();
        let c = WallChange {
            wall: w,
            joins: vec![(
                w,
                1,
                EndPrefs {
                    kind: JoinKind::SquareOff,
                    allowed: true,
                },
            )],
            openings: vec![(id, 1.5, 1.0, 0.6, 1.0, None)],
            ..Default::default()
        };
        m.apply_wall_change(&c).unwrap();
        assert_eq!(m.opening_kind(id).unwrap(), "void");
        assert_eq!(m.wall_join(w, 1).unwrap().kind, JoinKind::SquareOff);
        m.undo().unwrap();
        assert_eq!(m.wall_join(w, 1).unwrap(), EndPrefs::default());
        let along: f64 = m
            .sql()
            .query_row("SELECT along FROM opening WHERE id=?1", [id], |r| r.get(0))
            .unwrap();
        assert!((along - 1.0).abs() < 1e-12);
        let bad = WallChange {
            wall: w,
            sweeps_add: vec![Sweep {
                profile: vec![v2(0.0, 0.0), v2(1.0, 0.0)],
                s0: 0.0,
                s1: 1.0,
                reveal: false,
                left: false,
            }],
            ..Default::default()
        };
        assert!(m.apply_wall_change(&bad).is_err());
    }
}
