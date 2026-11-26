// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Accepting proposals: walls, openings, rooms, axes and columns in one journal operation (one undo).

use std::collections::BTreeSet;

use ddc_model::{Error, Model};
use rusqlite::{params, Connection};

use super::{Memory, Proposals};

#[derive(Clone, Debug)]
pub struct AcceptOptions {
    pub level: i64,
    pub room_height: f64,
    /// Remember proposals from `Proposals` that were not accepted as rejected.
    pub remember_rest: bool,
    /// Column top: a level (offset 0) or a height from the bottom.
    pub top_level: Option<i64>,
    pub column_height: f64,
    /// Locale (`de`, `pt-BR`) of the names written for new types; see [`crate::words`].
    pub lang: String,
}

impl AcceptOptions {
    pub fn new(level: i64) -> Self {
        Self {
            level,
            room_height: 2.7,
            remember_rest: true,
            top_level: None,
            column_height: 3.0,
            lang: "en".into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AcceptReport {
    pub walls: usize,
    pub openings: usize,
    pub rooms: usize,
    pub wall_types_created: usize,
    /// Walls added because an opening on them was selected.
    pub implied_walls: usize,
    pub grids: usize,
    pub columns: usize,
    /// Room separation lines from drawing outlines (where there is no wall).
    pub room_seps: usize,
    /// Selected ids that are not in the proposals.
    pub unknown_ids: Vec<u64>,
    pub wall_ids: Vec<i64>,
    /// Skipped proposals (non-finite geometry, zero-height column): the reason.
    pub warnings: Vec<String>,
}

/// Creates the selection in one operation. An opening pulls in its wall. Wall types with no match
/// are created as "Drawing N mm" (single-layer). Accepted ids go into `mem.accepted`.
pub fn accept_proposals(
    model: &mut Model,
    props: &Proposals,
    ids: &[u64],
    opt: &AcceptOptions,
    mem: &mut Memory,
) -> Result<AcceptReport, Error> {
    let want: BTreeSet<u64> = ids.iter().copied().collect();
    let known: BTreeSet<u64> = props.all_ids().into_iter().collect();
    let mut rep = AcceptReport {
        unknown_ids: want.difference(&known).copied().collect(),
        ..Default::default()
    };
    let level_ok: i64 =
        model
            .sql()
            .query_row("SELECT count(*) FROM level WHERE id=?1", [opt.level], |r| {
                r.get(0)
            })?;
    if level_ok == 0 {
        return Err(Error::Invalid(format!("no such level {}", opt.level)));
    }
    fn fin(v: &[f64]) -> bool {
        v.iter().all(|x| x.is_finite())
    }
    let mut warnings = vec![];
    // invalid walls are skipped with a warning, the rest is accepted
    let bad_walls: BTreeSet<u64> = props
        .walls
        .iter()
        .filter(|w| {
            !fin(&[w.a.0, w.a.1, w.b.0, w.b.1, w.thickness, w.height])
                || w.thickness <= 0.0
                || w.height <= 0.0
        })
        .map(|w| w.id)
        .collect();
    let openings: Vec<_> = props
        .openings
        .iter()
        .filter(|o| want.contains(&o.id))
        .filter(|o| {
            let ok = !bad_walls.contains(&o.wall)
                && fin(&[o.along, o.width, o.sill, o.height])
                && o.width > 0.0;
            if !ok {
                warnings.push(format!("opening {}: bad size or its wall is invalid", o.id));
            }
            ok
        })
        .collect();
    let mut wall_set: BTreeSet<u64> = props
        .walls
        .iter()
        .filter(|w| want.contains(&w.id))
        .map(|w| w.id)
        .collect();
    let direct = wall_set.len();
    wall_set.extend(openings.iter().map(|o| o.wall));
    rep.implied_walls = wall_set.len() - direct;
    for w in props
        .walls
        .iter()
        .filter(|w| wall_set.contains(&w.id) && bad_walls.contains(&w.id))
    {
        warnings.push(format!("wall {}: non-finite or non-positive size", w.id));
    }
    let walls: Vec<_> = props
        .walls
        .iter()
        .filter(|w| wall_set.contains(&w.id) && !bad_walls.contains(&w.id))
        .collect();
    // a room with a bad point is skipped; a bad outline is dropped (boundaries come from walls)
    let rooms: Vec<(&super::RoomCand, &[(f64, f64)])> = props
        .rooms
        .iter()
        .filter(|r| want.contains(&r.id))
        .filter_map(|r| {
            if !fin(&[r.point.0, r.point.1]) {
                warnings.push(format!("room {}: non-finite point", r.id));
                return None;
            }
            let ring_ok = r.boundary.iter().all(|p| fin(&[p.0, p.1]));
            if !ring_ok {
                warnings.push(format!("room {}: non-finite outline dropped", r.id));
            }
            Some((
                r,
                if ring_ok {
                    r.boundary.as_slice()
                } else {
                    &[][..]
                },
            ))
        })
        .collect();
    let grids: Vec<_> = props
        .grids
        .iter()
        .filter(|g| want.contains(&g.id))
        .filter(|g| {
            let ok =
                fin(&[g.a.0, g.a.1, g.b.0, g.b.1]) && (g.b.0 - g.a.0).hypot(g.b.1 - g.a.1) > 1e-6;
            if !ok {
                warnings.push(format!("grid {}: degenerate", g.id));
            }
            ok
        })
        .collect();
    let columns: Vec<_> = props
        .columns
        .iter()
        .filter(|c| want.contains(&c.id))
        .filter(|c| {
            let ok = fin(&[c.at.0, c.at.1, c.rotation, c.b, c.h]) && c.b > 0.0 && c.h > 0.0;
            if !ok {
                warnings.push(format!("column {}: bad size", c.id));
            }
            ok
        })
        .collect();

    let mut skipped_cols = 0;
    if !(walls.is_empty() && rooms.is_empty() && grids.is_empty() && columns.is_empty()) {
        let created =
            model.batch_walls_edit("wall_from_drawing", |db, snaps| {
                let mut created = 0;
                let mut wall_row = std::collections::HashMap::new();
                let mut row_ids = vec![];
                let mut seps = 0;
                for w in &walls {
                    let wtype = match w
                        .wall_type
                        .filter(|_| w.type_delta.is_some_and(|d| d <= 0.02))
                    {
                        Some(t) => t,
                        None => drawing_type(db, w.thickness, &mut created)?,
                    };
                    db.execute(
                        "INSERT INTO wall(level,wtype,ax,ay,bx,by,base_offset,height,offset0) \
                     VALUES (?1,?2,?3,?4,?5,?6,0,?7,NULL)",
                        params![opt.level, wtype, w.a.0, w.a.1, w.b.0, w.b.1, w.height],
                    )?;
                    let id = db.last_insert_rowid();
                    wall_row.insert(w.id, id);
                    row_ids.push(id);
                }
                for o in &openings {
                    let Some(&wall) = wall_row.get(&o.wall) else {
                        return Err(Error::Invalid(format!(
                            "opening {} refers to unknown wall proposal {}",
                            o.id, o.wall
                        )));
                    };
                    db.execute(
                    "INSERT INTO opening(wall,along,width,sill,height,kind,swing_side,swing_hinge) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        wall,
                        o.along,
                        o.width,
                        o.sill,
                        o.height,
                        o.kind,
                        o.swing_side,
                        o.swing_hinge
                    ],
                )?;
                    if !o.family.is_empty() {
                        let oid = db.last_insert_rowid();
                        // a row of a deleted opening with the same id: keep for undo, then replace
                        let stale: i64 = db.query_row(
                            "SELECT count(*) FROM prop WHERE kind='opening' AND ref=?1",
                            [oid],
                            |r| r.get(0),
                        )?;
                        if stale > 0 {
                            snaps.save(db, "prop", "ref", oid)?;
                            db.execute(
                                "DELETE FROM prop WHERE kind='opening' AND ref=?1 AND key='family'",
                                [oid],
                            )?;
                        }
                        db.execute(
                            "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('opening',?1,'family',?2,'')",
                            params![oid, o.family],
                        )?;
                    }
                }
                // level walls — both those already in the model and the just-inserted ones
                let mut lines = if rooms.iter().any(|(_, b)| !b.is_empty()) {
                    level_lines(db, opt.level)?
                } else {
                    vec![]
                };
                for (r, boundary) in &rooms {
                    db.execute(
                    "INSERT INTO room(level,name,number,px,py,height) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![opt.level, r.name, r.number, r.point.0, r.point.1, opt.room_height],
                )?;
                    seps += room_seps(db, opt.level, boundary, &mut lines)?;
                }
                for g in &grids {
                    let name = free_name(db, "grid", &g.name)?;
                    db.execute(
                        "INSERT INTO grid(name,ax,ay,bx,by,bulge,base_level,top_level,bubbles)                          VALUES (?1,?2,?3,?4,?5,0,NULL,NULL,3)",
                        params![name, g.a.0, g.a.1, g.b.0, g.b.1],
                    )?;
                }
                let (top_level, top_offset) = match opt.top_level {
                    Some(t) => (Some(t), 0.0),
                    None => (None, opt.column_height),
                };
                let mut columns = columns.as_slice();
                if !columns.is_empty() {
                    let elev = |lv: i64| -> Result<Option<f64>, Error> {
                        Ok(db
                            .query_row("SELECT elevation FROM level WHERE id=?1", [lv], |r| {
                                r.get::<_, f64>(0)
                            })
                            .ok())
                    };
                    let base = elev(opt.level)?;
                    let top = elev(top_level.unwrap_or(opt.level))?.map(|e| e + top_offset);
                    // as with manual column insertion: the top is strictly above the bottom
                    let ok = matches!((base, top), (Some(b), Some(t)) if t.is_finite() && t - b > 1e-6);
                    if !ok {
                        skipped_cols = columns.len();
                        columns = &[];
                    }
                }
                for c in columns {
                    let mtype = column_type(db, c, &opt.lang)?;
                    db.execute(
                        "INSERT INTO col(level,top_level,mtype,structural,x,y,base_offset,top_offset,rotation,tx,ty)                          VALUES (?1,?2,?3,0,?4,?5,0,?6,?7,NULL,NULL)",
                        params![opt.level, top_level, mtype, c.at.0, c.at.1, top_offset, c.rotation],
                    )?;
                }
                Ok((created, row_ids, seps))
            })?;
        rep.wall_types_created = created.0;
        rep.wall_ids = created.1;
        rep.room_seps = created.2;
    }
    let columns: Vec<_> = if skipped_cols > 0 {
        warnings.push(format!(
            "{skipped_cols} columns skipped: column top must be above its base (check top level / height)"
        ));
        vec![]
    } else {
        columns
    };
    rep.warnings = warnings;
    rep.grids = grids.len();
    rep.columns = columns.len();
    rep.walls = walls.len();
    rep.openings = openings.len();
    rep.rooms = rooms.len();

    let taken: BTreeSet<u64> = walls
        .iter()
        .map(|w| w.id)
        .chain(openings.iter().map(|o| o.id))
        .chain(rooms.iter().map(|(r, _)| r.id))
        .chain(grids.iter().map(|g| g.id))
        .chain(columns.iter().map(|c| c.id))
        .collect();
    if opt.remember_rest {
        // selected but skipped (invalid) ones are not counted as rejected
        let rest: Vec<u64> = known
            .difference(&taken)
            .filter(|id| !want.contains(id))
            .copied()
            .collect();
        mem.reject(&rest);
    }
    mem.accepted.extend(taken);
    Ok(rep)
}

/// A name free in `table`: "A", "A (2)", …
fn free_name(db: &Connection, table: &str, base: &str) -> Result<String, Error> {
    let sql = format!("SELECT 1 FROM {table} WHERE name=?1");
    ddc_model::free_name(base, |n| format!("{base} ({n})"), |name| {
        Ok::<_, Error>(db.query_row(&sql, [name], |_| Ok(())).is_ok())
    })
}

/// Column type by section: an existing one with the same shape or a new one named in `lang`.
fn column_type(db: &Connection, c: &super::ColumnCand, lang: &str) -> Result<i64, Error> {
    use ddc_geom::member::Shape;
    let mm = |v: f64| (v * 1000.0).round();
    let (shape, name) = match c.diameter {
        Some(d) => (
            Shape::Circle { d: mm(d) / 1000.0 },
            crate::words::text_with(lang, "io.w.column_round", &[("d", &mm(d).to_string())]),
        ),
        None => (
            Shape::Rect {
                b: mm(c.b) / 1000.0,
                h: mm(c.h) / 1000.0,
            },
            crate::words::text_with(
                lang,
                "io.w.column_rect",
                &[("b", &mm(c.b).to_string()), ("h", &mm(c.h).to_string())],
            ),
        ),
    };
    let text = shape.text();
    if let Ok(id) = db.query_row(
        "SELECT id FROM member_type WHERE shape=?1 ORDER BY id LIMIT 1",
        [&text],
        |r| r.get::<_, i64>(0),
    ) {
        return Ok(id);
    }
    let name = free_name(db, "member_type", &name)?;
    db.execute(
        "INSERT INTO member_type(name,shape,material) VALUES (?1,?2,'Concrete')",
        params![name, text],
    )?;
    Ok(db.last_insert_rowid())
}

/// A wall axis or level separation line: endpoints and half-thickness.
type LevelLine = ((f64, f64), (f64, f64), f64);

/// Walls (full thickness from the type's layers) and level separation lines from the model.
fn level_lines(db: &Connection, level: i64) -> Result<Vec<LevelLine>, Error> {
    let mut out = vec![];
    let mut st = db.prepare(
        "SELECT w.ax,w.ay,w.bx,w.by, \
         COALESCE((SELECT sum(l.thickness) FROM wall_type_layer l WHERE l.wtype=w.wtype),0.2) \
         FROM wall w WHERE w.level=?1",
    )?;
    let rows = st.query_map([level], |r| {
        Ok((
            (r.get::<_, f64>(0)?, r.get::<_, f64>(1)?),
            (r.get::<_, f64>(2)?, r.get::<_, f64>(3)?),
            r.get::<_, f64>(4)? / 2.0,
        ))
    })?;
    for r in rows {
        out.push(r?);
    }
    let mut st = db.prepare("SELECT ax,ay,bx,by FROM room_sep WHERE level=?1")?;
    let rows = st.query_map([level], |r| {
        Ok((
            (r.get::<_, f64>(0)?, r.get::<_, f64>(1)?),
            (r.get::<_, f64>(2)?, r.get::<_, f64>(3)?),
            0.0,
        ))
    })?;
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Room outline edges along which there is no level wall (including one already in the model) or
/// separation line become new separation lines; the added ones go into `lines`.
fn room_seps(
    db: &Connection,
    level: i64,
    ring: &[(f64, f64)],
    lines: &mut Vec<LevelLine>,
) -> Result<usize, Error> {
    let n = ring.len();
    let mut count = 0;
    for k in 0..n {
        let (a, b) = (ring[k], ring[(k + 1) % n]);
        let len = (b.0 - a.0).hypot(b.1 - a.1);
        if !(len >= 0.05) {
            continue;
        }
        let m = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        let along_wall = lines.iter().any(|&(wa, wb, hw)| {
            let d = (wb.0 - wa.0, wb.1 - wa.1);
            let wl = d.0.hypot(d.1);
            if !(wl >= 1e-9) {
                return false;
            }
            let u = (d.0 / wl, d.1 / wl);
            let parallel = ((b.0 - a.0) * u.1 - (b.1 - a.1) * u.0).abs() <= 0.05 * len;
            let rel = (m.0 - wa.0, m.1 - wa.1);
            let t = rel.0 * u.0 + rel.1 * u.1;
            let lat = (rel.0 * -u.1 + rel.1 * u.0).abs();
            parallel && lat <= hw + 0.1 && t >= -0.1 && t <= wl + 0.1
        });
        if !along_wall {
            db.execute(
                "INSERT INTO room_sep(level,ax,ay,bx,by) VALUES (?1,?2,?3,?4,?5)",
                params![level, a.0, a.1, b.0, b.1],
            )?;
            lines.push((a, b, 0.0));
            count += 1;
        }
    }
    Ok(count)
}

/// Single-layer type "Drawing N mm": an existing one with a total layer thickness within 0.5 mm
/// or a new one. A same-named type of another thickness is not reused (the name gets the suffix " (N)").
fn drawing_type(db: &Connection, thickness: f64, created: &mut usize) -> Result<i64, Error> {
    const TOL: f64 = 0.0005;
    let found: Option<i64> = db
        .query_row(
            "SELECT t.id FROM wall_type t JOIN wall_type_layer l ON l.wtype=t.id              GROUP BY t.id HAVING abs(sum(l.thickness)-?1) <= ?2 ORDER BY t.id LIMIT 1",
            params![thickness, TOL],
            |r| r.get(0),
        )
        .ok();
    if let Some(id) = found {
        return Ok(id);
    }
    let base = format!("Drawing {:.0} mm", thickness * 1000.0);
    let mut name = base.clone();
    let mut n = 1;
    while db
        .query_row("SELECT 1 FROM wall_type WHERE name=?1", [&name], |_| Ok(()))
        .is_ok()
    {
        n += 1;
        name = format!("{base} ({n})");
    }
    db.execute("INSERT INTO wall_type(name) VALUES (?1)", [&name])?;
    let id = db.last_insert_rowid();
    db.execute(
        "INSERT INTO wall_type_layer VALUES (?1,0,'Generic',?2)",
        params![id, (thickness * 1e6).round() / 1e6],
    )?;
    *created += 1;
    Ok(id)
}
