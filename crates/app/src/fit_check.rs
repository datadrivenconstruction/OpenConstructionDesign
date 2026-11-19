// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Whether every opening fits its wall once a batch of edits stands, over the whole model
//! (all levels). Opening edits pass `opening_tool::validate`; a wall edit is refused when an
//! opening it hosts fitted before and would not fit after. Used by the multi-select edits,
//! query and AI assign (`multi_edit::screen_openings`) and by the workbook read.
use std::collections::{BTreeMap, HashMap, HashSet};

use ddc_geom::{v2, V2};
use ddc_io::xlsx::{Rejection, Report};
use ddc_model::{Edit, Model};

use crate::opening_tool::{validate, Flip, HostWall, Kind, OpeningType, PlaceError, Placed};

/// Walls and openings as the model holds them.
#[derive(Clone, Debug, Default)]
pub struct Fit {
    pub walls: HashMap<i64, HostWall>,
    pub placed: Vec<Placed>,
}

/// One refused edit: its index in the batch, the element (a new opening: minus its book row
/// or minus its index + 1) and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Refused {
    pub index: usize,
    pub id: i64,
    pub why: PlaceError,
}

fn host(id: i64, a: V2, b: V2, height: f64, bulge: f64) -> HostWall {
    let run = ddc_geom::arc::axis_length(a, b, bulge);
    HostWall { id, a, b, height, half: 0.05, run }
}

impl Fit {
    /// The walls and openings the editor holds (its level), when the model cannot be read.
    pub fn of_editor(ed: &crate::editor::Editor) -> Fit {
        let walls = ed.walls.iter().filter_map(|w| ed.host_of(w.id)).map(|h| (h.id, h)).collect();
        Fit { walls, placed: ed.openings.clone() }
    }

    /// Every wall and opening of the model.
    pub fn load(m: &Model) -> Result<Fit, String> {
        let db = m.sql();
        let e = |e: rusqlite::Error| e.to_string();
        let mut st = db
            .prepare("SELECT id, ax, ay, bx, by, height, bulge FROM wall")
            .map_err(e)?;
        let rows = st
            .query_map([], |r| {
                let (a, b) = (v2(r.get(1)?, r.get(2)?), v2(r.get(3)?, r.get(4)?));
                Ok(host(r.get(0)?, a, b, r.get(5)?, r.get(6)?))
            })
            .map_err(e)?;
        let mut walls = HashMap::new();
        for h in rows {
            let h = h.map_err(e)?;
            walls.insert(h.id, h);
        }
        drop(st);
        let mut st = db
            .prepare("SELECT id, wall, along, width, sill, height, kind FROM opening ORDER BY id")
            .map_err(e)?;
        let placed = st
            .query_map([], |r| {
                let kind: String = r.get(6)?;
                Ok(Placed {
                    id: r.get(0)?,
                    wall: r.get(1)?,
                    along: r.get(2)?,
                    width: r.get(3)?,
                    sill: r.get(4)?,
                    height: r.get(5)?,
                    kind: Kind::from_model(&kind),
                    flip: Flip::default(),
                })
            })
            .map_err(e)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(e)?;
        Ok(Fit { walls, placed })
    }
}

/// Id that stands for the new opening of edit `i`: minus the book row its `Mark` names, else
/// minus `i + 1`. Never a real id.
fn new_id(edits: &[Edit], i: usize) -> i64 {
    match edits.get(i + 1) {
        Some(Edit::Mark { kind, row }) if kind == "opening" => -i64::from(*row),
        _ => -(i as i64) - 1,
    }
}

/// The model as the batch leaves it: walls with their accepted edits, openings checked in
/// batch order (each against the accepted ones at their new size and the rest as they are).
struct Stand {
    walls: HashMap<i64, HostWall>,
    placed: Vec<Placed>,
    done: HashSet<i64>,
    refused: Vec<Refused>,
}

fn stand(fit: &Fit, edits: &[Edit], walls_out: &BTreeMap<usize, Refused>) -> Stand {
    let mut walls = fit.walls.clone();
    let mut gone = HashSet::new();
    for (i, e) in edits.iter().enumerate() {
        match e {
            Edit::Wall { id, spec, bulge } if !walls_out.contains_key(&i) => {
                if let Some(h) = walls.get_mut(id) {
                    let (a, b) = (v2(spec.a.0, spec.a.1), v2(spec.b.0, spec.b.1));
                    *h = host(*id, a, b, spec.height, *bulge);
                }
            }
            Edit::Delete { table: "wall", id } => {
                walls.remove(id);
            }
            Edit::Delete { table: "opening", id } => {
                gone.insert(*id);
            }
            _ => {}
        }
    }
    let mut placed: Vec<Placed> = fit
        .placed
        .iter()
        .filter(|p| walls.contains_key(&p.wall) && !gone.contains(&p.id))
        .copied()
        .collect();
    let (mut done, mut refused) = (HashSet::new(), vec![]);
    for (i, e) in edits.iter().enumerate() {
        let (id, wall, spec) = match e {
            Edit::Opening { id, wall, spec } => (*id, *wall, spec),
            Edit::NewOpening { wall, spec, .. } => (new_id(edits, i), *wall, spec),
            _ => continue,
        };
        let kind = Kind::from_model(&spec.kind);
        let t = OpeningType { kind, width: spec.width, height: spec.height, sill: spec.sill };
        let err = match walls.get(&wall) {
            Some(h) => validate(h, &placed, Some(id), spec.along, &t).err(),
            None => Some(PlaceError::NoWall),
        };
        if let Some(why) = err {
            refused.push(Refused { index: i, id, why });
            continue;
        }
        let (along, width, sill, height) = (spec.along, spec.width, spec.sill, spec.height);
        let p = Placed { id, wall, along, width, sill, height, kind, flip: Flip::default() };
        match placed.iter_mut().find(|q| q.id == id) {
            Some(q) => *q = Placed { flip: q.flip, ..p },
            None => placed.push(p),
        }
        done.insert(id);
    }
    Stand { walls, placed, done, refused }
}

/// First opening on wall `id`, not edited by the batch, that fits the wall as it is and would
/// not fit it as edited. Overlaps are not the wall's business.
fn hosted_misfit(fit: &Fit, s: &Stand, id: i64) -> Option<PlaceError> {
    let (old, new) = (fit.walls.get(&id)?, s.walls.get(&id)?);
    s.placed
        .iter()
        .filter(|p| p.wall == id && !s.done.contains(&p.id))
        .find_map(|p| {
            let t = OpeningType { kind: p.kind, width: p.width, height: p.height, sill: p.sill };
            let now = validate(new, &[], Some(p.id), p.along, &t).err()?;
            let fitted = validate(old, &[], Some(p.id), p.along, &t).is_ok();
            fitted.then(|| PlaceError::Hosts { opening: p.id, err: Box::new(now) })
        })
}

/// The refused edits of `edits`, in batch order. A refused wall keeps its openings checked
/// against the wall as it is, so the opening checks run again until nothing more is refused.
pub fn screen(fit: &Fit, edits: &[Edit]) -> Vec<Refused> {
    let mut walls_out: BTreeMap<usize, Refused> = BTreeMap::new();
    loop {
        let s = stand(fit, edits, &walls_out);
        let mut more = false;
        for (i, e) in edits.iter().enumerate() {
            let Edit::Wall { id, .. } = e else { continue };
            if walls_out.contains_key(&i) {
                continue;
            }
            if let Some(why) = hosted_misfit(fit, &s, *id) {
                walls_out.insert(i, Refused { index: i, id: *id, why });
                more = true;
            }
        }
        if !more {
            let mut v = s.refused;
            v.extend(walls_out.into_values());
            v.sort_by_key(|r| r.index);
            return v;
        }
    }
}

/// Whether the batch has an edit these checks look at.
pub fn touches(edits: &[Edit]) -> bool {
    edits
        .iter()
        .any(|e| matches!(e, Edit::Opening { .. } | Edit::NewOpening { .. } | Edit::Wall { .. }))
}

const OPENINGS: &str = "Openings";
const WALLS: &str = "Walls";

/// Metres as the book's reasons write them: whole millimetres, no trailing zeros.
fn m(v: f64) -> String {
    format!("{}", (v * 1000.0).round() / 1000.0 + 0.0)
}

/// The workbook reason of a refusal: English, matched back by the `xl.msg.xlval_*`
/// templates for the UI language.
pub fn reason(why: &PlaceError) -> String {
    match why {
        PlaceError::NoWall => "opening has no wall to sit in".into(),
        PlaceError::NonPositive => "opening size must be > 0 and sill >= 0".into(),
        PlaceError::Outside { len, from, to } => format!(
            "opening runs from {} m to {} m, the wall is {} m long",
            m(*from),
            m(*to),
            m(*len)
        ),
        PlaceError::TooTall { top, height } => {
            format!("opening top {} m is above the wall height {} m", m(*top), m(*height))
        }
        PlaceError::Overlap { other } if *other < 0 => {
            format!("opening overlaps the new opening in row {}", -other)
        }
        PlaceError::Overlap { other } => format!("opening overlaps opening {other}"),
        PlaceError::Hosts { opening, err } => match err.as_ref() {
            PlaceError::TooTall { top, height } => format!(
                "wall height {} m is below the top {} m of opening {opening}",
                m(*height),
                m(*top)
            ),
            PlaceError::Outside { len, to, .. } => format!(
                "wall length {} m is shorter than the end {} m of opening {opening}",
                m(*len),
                m(*to)
            ),
            _ => format!("opening {opening} would no longer fit the wall"),
        },
    }
}

/// Book column the refusal points at.
fn column_key(why: &PlaceError) -> &'static str {
    match why {
        PlaceError::NoWall => "wall",
        PlaceError::NonPositive => "width",
        PlaceError::Outside { .. } | PlaceError::Overlap { .. } => "along",
        PlaceError::TooTall { .. } => "height",
        PlaceError::Hosts { err, .. } => match err.as_ref() {
            PlaceError::Outside { .. } => "ax",
            _ => "height",
        },
    }
}

/// Edit `e` belongs to the refused row: the element `id`, or the new row `row`.
fn tied(e: &Edit, kind: &str, id: i64, row: Option<u32>) -> bool {
    match e {
        Edit::OpeningSwing { id: i, .. } => kind == "opening" && row.is_none() && *i == id,
        Edit::ParamValue { kind: k, id: i, .. } | Edit::Phase { kind: k, id: i, .. } => {
            k == kind && row.is_none() && *i == id
        }
        Edit::Mark { kind: k, row: r } | Edit::ParamValueNew { kind: k, row: r, .. } => {
            k == kind && Some(*r) == row
        }
        _ => false,
    }
}

/// Runs the checks over a workbook read: each refused row moves from the batch and the
/// change list to `rejected` with its address and reason, and is never applied; the rest of
/// the book applies as before (one step).
pub fn screen_report(model: &Model, rep: &mut Report) -> Result<(), String> {
    if !touches(&rep.edits) {
        return Ok(());
    }
    let fit = Fit::load(model)?;
    let refused = screen(&fit, &rep.edits);
    if refused.is_empty() {
        return Ok(());
    }
    let mut out = HashSet::new();
    for r in &refused {
        let (sheet, kind) = match rep.edits[r.index] {
            Edit::Wall { .. } => (WALLS, "wall"),
            _ => (OPENINGS, "opening"),
        };
        let new_row = match rep.edits[r.index] {
            Edit::NewOpening { .. } => match rep.edits.get(r.index + 1) {
                Some(Edit::Mark { kind, row }) if kind == "opening" => Some(*row),
                _ => None,
            },
            _ => None,
        };
        let row = new_row.or_else(|| {
            let mine = |c: &&ddc_io::xlsx::Change| c.sheet == sheet && c.id == Some(r.id);
            rep.changes.iter().find(mine).map(|c| c.row)
        });
        let key = column_key(&r.why);
        let cell = row.and_then(|row| {
            let of_row = |c: &&ddc_io::xlsx::Change| c.sheet == sheet && c.row == row;
            let by_key = rep.changes.iter().filter(of_row).find(|c| c.column.starts_with(key));
            by_key.or_else(|| rep.changes.iter().find(of_row)).map(|c| (c.col, c.column.clone()))
        });
        let (col, column) = cell.unwrap_or((0, key.to_string()));
        // no row found: row 0 stops the whole import rather than apply a refused edit
        rep.rejected.push(Rejection {
            sheet: sheet.into(),
            row: row.unwrap_or(0),
            col,
            column,
            reason: reason(&r.why),
        });
        if let Some(row) = row {
            rep.changes.retain(|c| !(c.sheet == sheet && c.row == row));
            rep.conflicts.retain(|c| !(c.sheet == sheet && c.row == row));
            rep.fingerprints.remove(&(sheet.to_string(), row));
        }
        // nothing of the row applies: new-column values, and no "taken" base for the next read
        let id = new_row.is_none().then_some(r.id);
        for p in rep.proposals.iter_mut().filter(|p| p.sheet == sheet) {
            p.refuse(row, kind, id);
        }
        if let Some(id) = id {
            rep.applied_cells.retain(|c| !c.is_of(sheet, id));
        }
        out.insert(r.index);
        for (j, e) in rep.edits.iter().enumerate() {
            if tied(e, kind, r.id, new_row) {
                out.insert(j);
            }
        }
    }
    let mut j = 0;
    rep.edits.retain(|_| {
        j += 1;
        !out.contains(&(j - 1))
    });
    Ok(())
}

#[cfg(test)]
#[path = "fit_check_tests.rs"]
mod tests;
