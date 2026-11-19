// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Copies that also rise: a typical floor copied up by whole storeys is one operation and one
//! undo step. Solids, masses and terrains lift as a whole; level-hosted elements move to the
//! level at the new elevation when there is one, else keep their level and gain the offset.

use rusqlite::{params, Connection, OptionalExtension};

use super::{apply, ordered, table_of, validate, Target};
use crate::{Error, Model};

type P3 = (f64, f64, f64);

/// Two elevations closer than this (1 mm) are the same level.
const SAME: f64 = 1e-3;

impl Model {
    /// Copies with offsets `[dx, dy, dz]` metres, one copy per offset. A `dz` of zero is a
    /// plan copy; a rise needs every target to be a kind that can rise (not a view, a
    /// dimension or a room separator).
    pub fn copy_elements_z(&mut self, ts: &[Target], vs: &[P3]) -> Result<Vec<Target>, Error> {
        if vs.iter().any(|v| !(v.0.is_finite() && v.1.is_finite() && v.2.is_finite())) {
            return Err(Error::Invalid("copy: offset is not finite".into()));
        }
        self.modify("wall_copy", |db, s| {
            validate(db, ts)?;
            if vs.is_empty() {
                return Err(Error::Invalid("copy: no offsets".into()));
            }
            if vs.iter().any(|v| v.2.abs() > SAME) {
                alone_hosted(db, ts)?;
            }
            let mut out = vec![];
            for v in vs {
                let made = apply(db, s, ts, super::Xf::translate((v.0, v.1)), true)?;
                if v.2.abs() > SAME {
                    rise(db, s, &made, v.2)?;
                }
                out.extend(made);
            }
            Ok(out)
        })
    }
}

/// Raises freshly made copies by `dz`.
fn rise(db: &Connection, s: &mut crate::ops::Snapshots, made: &[Target], dz: f64) -> Result<(), Error> {
    let mut shifted = vec![];
    for &t in &ordered(made) {
        match t {
            Target::Solid(id) => crate::solids::lift(db, id, dz)?,
            Target::Mass(id) => {
                s.push(crate::mass::shift_in(db, id, (0.0, 0.0, dz))?);
                shifted.push(id);
            }
            Target::Topo(id) => crate::topo::lift_in(db, id, dz)?,
            // hosted pieces went up with their wall (see `raise_row`)
            Target::Opening(_) => {}
            Target::Component(c) => {
                let host: Option<i64> =
                    db.query_row("SELECT host_wall FROM comp WHERE id=?1", [c], |r| r.get(0))?;
                if host.is_none() {
                    raise_row(db, "comp", c, dz)?;
                }
            }
            Target::MepRun(id) => {
                raise_row(db, "mep_run", id, dz)?;
                let text: String = db.query_row("SELECT points FROM mep_run WHERE id=?1", [id], |r| r.get(0))?;
                let pts = crate::mep::parse_points(&text)
                    .ok_or_else(|| Error::Invalid(format!("run {id}: bad points")))?;
                let up: Vec<P3> = pts.into_iter().map(|p| (p.0, p.1, p.2 + dz)).collect();
                db.execute("UPDATE mep_run SET points=?2 WHERE id=?1", params![id, crate::mep::points_text(&up)])?;
            }
            Target::View(_) | Target::Dim(_) | Target::RoomSep(_) => {
                return Err(Error::Invalid("copy up: a view, a dimension or a room separator does not rise".into()))
            }
            other => {
                let (table, id) = table_of(other);
                raise_row(db, table, id, dz)?;
            }
        }
    }
    if !shifted.is_empty() {
        s.push(crate::mass::follow_shifted(db, &shifted)?);
    }
    Ok(())
}

fn columns(db: &Connection, table: &str) -> Result<Vec<String>, Error> {
    let mut st = db.prepare(&format!("PRAGMA table_info({table})"))?;
    let v = st.query_map([], |r| r.get::<_, String>(1))?.collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

fn level_elev(db: &Connection, id: i64) -> Result<f64, Error> {
    db.query_row("SELECT elevation FROM level WHERE id=?1", [id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no level {id}")))
}

fn level_at(db: &Connection, z: f64) -> Result<Option<i64>, Error> {
    Ok(db
        .query_row("SELECT id FROM level WHERE ABS(elevation-?1)<?2 ORDER BY id LIMIT 1", params![z, SAME], |r| r.get(0))
        .optional()?)
}

/// Columns that hold a height above the level, per table: what grows when no level is there.
fn offsets(table: &str) -> &'static [&'static str] {
    match table {
        "slab" | "roof" | "ceiling" => &["offset"],
        "comp" => &["z"],
        "beam" => &["az", "bz"],
        "mep_run" => &[],
        _ => &["base_offset", "top_offset"],
    }
}

/// A selected opening or component whose host wall is not selected would be copied onto the
/// same wall by `apply`; a rising copy refuses it.
fn alone_hosted(db: &Connection, ts: &[Target]) -> Result<(), Error> {
    for &t in ts {
        let host: Option<i64> = match t {
            Target::Opening(o) => Some(db.query_row("SELECT wall FROM opening WHERE id=?1", [o], |r| r.get(0))?),
            Target::Component(c) => db.query_row("SELECT host_wall FROM comp WHERE id=?1", [c], |r| r.get(0))?,
            _ => None,
        };
        if host.is_some_and(|h| !ts.contains(&Target::Wall(h))) {
            return Err(Error::Invalid("copy up: a door, window or fitting rises with its wall; select the wall too".into()));
        }
    }
    Ok(())
}

/// One row up by `dz`: onto the level there when it exists, else its own offsets grow. Pieces
/// hosted on a wall follow it.
fn raise_row(db: &Connection, table: &str, id: i64, dz: f64) -> Result<(), Error> {
    let cols = columns(db, table)?;
    let has = |c: &str| cols.iter().any(|x| x == c);
    if !has("level") {
        return Err(Error::Invalid(format!("copy up: {table} {id} has no level to rise on")));
    }
    let level: i64 = db.query_row(&format!("SELECT level FROM {table} WHERE id=?1"), [id], |r| r.get(0))?;
    let top: Option<i64> = if has("top_level") {
        db.query_row(&format!("SELECT top_level FROM {table} WHERE id=?1"), [id], |r| r.get(0))?
    } else {
        None
    };
    let base_to = level_at(db, level_elev(db, level)? + dz)?;
    let top_to = match top {
        Some(t) => level_at(db, level_elev(db, t)? + dz)?,
        None => None,
    };
    let mut onto = None;
    if let (Some(b), true) = (base_to, top.is_none() || top_to.is_some()) {
        db.execute(&format!("UPDATE {table} SET level=?2 WHERE id=?1"), params![id, b])?;
        if let Some(t) = top_to {
            db.execute(&format!("UPDATE {table} SET top_level=?2 WHERE id=?1"), params![id, t])?;
        }
        onto = Some(b);
    } else if table != "mep_run" {
        let cols: Vec<&str> = offsets(table).iter().copied().filter(|c| has(c)).collect();
        if cols.is_empty() {
            return Err(Error::Invalid(format!(
                "copy up: {table} {id} has no level {dz} m above its own and no offset to rise by; add that level first"
            )));
        }
        for c in cols {
            db.execute(&format!("UPDATE {table} SET {c}={c}+?2 WHERE id=?1"), params![id, dz])?;
        }
    }
    if table == "wall" {
        match onto {
            Some(l) => db.execute("UPDATE comp SET level=?2 WHERE host_wall=?1", params![id, l])?,
            None => db.execute("UPDATE comp SET z=z+?2 WHERE host_wall=?1", params![id, dz])?,
        };
    }
    Ok(())
}
