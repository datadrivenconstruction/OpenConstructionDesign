// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Levels below ground from a typology: the stairs, lifts and the through corridor of the
//! ground floor run down every level as in any basement; the rest of each level, the rectangles
//! the box leaves free, is handed to the spaces the model puts on that level, largest space to
//! largest rectangle. What no space asks for becomes the level's fill. The outside walls of the
//! levels hold back the soil.

use super::super::program::Light;
use super::super::names;
use super::super::tile::{m, Cell, Mm};
use super::super::{PRole, ProjectSpec, RoomKind};
use super::{Below, Level, Model, TSpace};
use crate::{GenError, GLayout, WallRole};

type R = (Mm, Mm, Mm, Mm);

fn infeasible(s: impl Into<String>) -> GenError {
    GenError::Infeasible(s.into())
}

fn core(k: RoomKind) -> bool {
    matches!(k, RoomKind::Corridor | RoomKind::Stair | RoomKind::Lift)
}

fn rect(c: &Cell) -> R {
    (c.x0, c.y0, c.x1, c.y1)
}

fn size(r: &R) -> Mm {
    (r.2 - r.0) * (r.3 - r.1)
}

fn cell(r: R, s: &TSpace, lang: &str) -> Cell {
    Cell {
        x0: r.0,
        y0: r.1,
        x1: r.2,
        y1: r.3,
        kind: s.kind,
        name: s.display(lang).to_string(),
        owner: PRole::Public,
        asked: None,
        grown: false,
        lit: !matches!(s.light, Light::None),
    }
}

/// The two halves of a rectangle cut across its longer side.
fn halves(r: R) -> (R, R) {
    if r.2 - r.0 >= r.3 - r.1 {
        let x = r.0 + (r.2 - r.0) / 2;
        ((r.0, r.1, x, r.3), (x, r.1, r.2, r.3))
    } else {
        let y = r.1 + (r.3 - r.1) / 2;
        ((r.0, r.1, r.2, y), (r.0, y, r.2, r.3))
    }
}

/// The cells of level `b` (0 right under the ground floor) over the base plan the basement
/// builder made: its stairs, lifts and corridors kept, its free rectangles given to the spaces
/// of the level.
pub(crate) fn level_cells(spec: &ProjectSpec, model: &Model, below: &Below, base: &[Cell], b: usize) -> Result<Vec<Cell>, GenError> {
    let lang = spec.lang;
    let lvl = &below.levels[b.min(below.levels.len() - 1)];
    let mut out: Vec<Cell> = base.iter().filter(|c| core(c.kind)).cloned().collect();
    if let Some(n) = &lvl.corridor {
        for c in out.iter_mut().filter(|c| c.kind == RoomKind::Corridor) {
            c.name = n.get(lang);
        }
    }
    let mut free: Vec<R> = base.iter().filter(|c| !core(c.kind)).map(rect).collect();
    free.sort_by(|a, b| size(b).cmp(&size(a)).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
    let fill = model
        .spaces
        .iter()
        .find(|s| s.class == lvl.fill)
        .ok_or_else(|| infeasible(format!("typology: no class '{}' to fill the level", lvl.fill)))?;
    // a model with more storeys below than levels repeats the last level's fill only
    let own = b < below.levels.len();
    let mut want: Vec<&TSpace> = model
        .spaces
        .iter()
        .filter(|s| own && s.level == Level::Below(b as u32 + 1) && s.class != lvl.fill && s.count > 0)
        .collect();
    want.sort_by(|a, c| (c.target_area() * f64::from(c.count)).total_cmp(&(a.target_area() * f64::from(a.count))));
    for s in want {
        for _ in 0..s.count {
            if free.is_empty() {
                halve_for(&mut out, s, lang, b)?;
                continue;
            }
            let r = free.remove(0);
            out.push(cell(r, s, lang));
        }
    }
    // the fill the level itself asks for gets a room even when the others took every rectangle
    if free.is_empty() && own && fill.level == Level::Below(b as u32 + 1) && fill.count > 0 {
        halve_for(&mut out, fill, lang, b)?;
    }
    for r in free {
        out.push(cell(r, fill, lang));
    }
    Ok(out)
}

/// No free rectangle left on level `b`: the largest room of `s`'s class is cut in two, else the
/// largest room the level holds gives up half of itself to `s`.
fn halve_for(out: &mut Vec<Cell>, s: &TSpace, lang: &str, b: usize) -> Result<(), GenError> {
    let largest = |same: bool| {
        out.iter()
            .enumerate()
            .filter(|(_, c)| !core(c.kind) && (!same || (c.kind == s.kind && c.name == s.display(lang))))
            .max_by_key(|(_, c)| size(&rect(c)))
            .map(|(i, _)| i)
    };
    let at = largest(true).or_else(|| largest(false)).ok_or_else(|| {
        infeasible(format!("level below {}: no room left for '{}'; widen the footprint or lower its count", b + 1, s.name.en))
    })?;
    let (a, z) = halves(rect(&out[at]));
    (out[at].x0, out[at].y0, out[at].x1, out[at].y1) = a;
    out.push(cell(z, s, lang));
    Ok(())
}

/// The outside walls of a level below ground hold back the soil.
pub(crate) fn retain(l: &mut GLayout) {
    for w in l.walls.iter_mut().filter(|w| w.role == WallRole::Exterior) {
        w.role = WallRole::Retaining;
    }
}

/// Name of the level `b` in the spec's language.
pub(crate) fn level_name(spec: &ProjectSpec, below: &Below, b: usize) -> String {
    let n = &below.levels[b.min(below.levels.len() - 1)].name;
    let base = n.get(spec.lang);
    if b < below.levels.len() {
        base
    } else {
        format!("{} {}", names::basement_name(b, spec.lang), base)
    }
}

/// Plan area of a rectangle cell in m2.
pub(crate) fn area_m2(c: &Cell) -> f64 {
    m(c.x1 - c.x0) * m(c.y1 - c.y0)
}
