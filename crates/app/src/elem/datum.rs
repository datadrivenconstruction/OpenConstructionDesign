//! Stair, railing, grid and reference plane in the element contract (D2, D4): plan outlines for
//! selection, the ends of a reference plane (its grips live in `grips.rs`), and the 3D bodies of
//! the two datums, drawn as thin ribbons at the level so that they can be picked and moved.
//!
//! The four families carry the `Sel::Ext` codes of their places in `structx::CORE_KINDS`.
//! Stair and railing keep their 3D categories (`Cat::Stair`,
//! `Cat::Railing`); grid and reference plane use `Cat::Ext(code)`.

use crate::edit3d::pick::{Cat, Target};
use crate::edit3d::scene3d::Part;
use crate::editor::structx::code_of;
use crate::editor::Editor;
use crate::geom_props::{Effect, Row, Unit};
use ddc_geom::arc::ArcAxis;
use ddc_geom::{v2, Tol, V2};
use ddc_model::{Error, Model};
use ddc_render::glam::Vec3;

type P = (f64, f64);

/// Edge-only families: a click inside the outline must not select them.
pub fn edge_only(code: u16) -> bool {
    ["railing", "grid", "ref_plane"]
        .iter()
        .any(|k| code_of(k) == Some(code))
}

/// 3D category of a stair or railing selection code; the others use `Cat::Ext(code)`.
pub fn cat_of_code(code: u16) -> Option<Cat> {
    if code_of("stair") == Some(code) {
        Some(Cat::Stair)
    } else if code_of("railing") == Some(code) {
        Some(Cat::Railing)
    } else {
        None
    }
}

/// Editor selection of a 3D stair or railing.
pub fn sel_of_cat(c: Cat, id: i64) -> Option<crate::editor::Sel> {
    let kind = match c {
        Cat::Stair => "stair",
        Cat::Railing => "railing",
        _ => return None,
    };
    crate::editor::structx::sel(kind, id)
}

/// The two ends of a reference plane.
pub fn ref_plane_ends(m: &Model, id: i64) -> Result<(V2, V2), Error> {
    m.ref_planes()?
        .into_iter()
        .find(|r| r.0 == id)
        .map(|r| (v2(r.1 .0, r.1 .1), v2(r.2 .0, r.2 .1)))
        .ok_or_else(|| Error::Invalid(format!("no such reference plane {id}")))
}

fn ids(m: &Model, sql: &str, level: i64) -> Vec<i64> {
    m.sql()
        .prepare(sql)
        .and_then(|mut st| st.query_map([level], |r| r.get(0))?.collect())
        .unwrap_or_default()
}

fn pairs(r: &[V2]) -> Vec<P> {
    r.iter().map(|p| (p.x, p.y)).collect()
}

/// A path as a ring that goes there and back: the ring keeps the open line selectable by its
/// edges and has no inside. A straight line stays a two-point ring.
fn there_and_back(path: &[P]) -> Vec<P> {
    let mut ring = path.to_vec();
    if path.len() > 2 {
        ring.extend(path[1..path.len() - 1].iter().rev().copied());
    }
    ring
}

/// The axis of a grid in the plan: the chord, or the arc sampled.
pub(crate) fn grid_line(a: P, b: P, bulge: f64) -> Vec<P> {
    let (va, vb) = (v2(a.0, a.1), v2(b.0, b.1));
    match ArcAxis::new(va, vb, bulge, Tol::default()) {
        Ok(ax) if bulge != 0.0 => {
            let n = 24;
            let l = ax.length();
            (0..=n)
                .map(|i| {
                    let p = ax.point_at(l * i as f64 / n as f64);
                    (p.x, p.y)
                })
                .collect()
        }
        _ => vec![a, b],
    }
}

/// Outlines for plan selection: (kind, id, rings). Grids and reference planes belong to no level.
pub fn footprints(m: &Model, level: i64) -> Vec<(&'static str, i64, Vec<Vec<P>>)> {
    let mut out = vec![];
    for id in ids(m, "SELECT id FROM stair WHERE level=?1 ORDER BY id", level) {
        let Ok(l) = m.stair_layout(id) else { continue };
        let mut rings: Vec<Vec<P>> = l.flights.iter().map(|f| pairs(&f.ring(l.tread))).collect();
        rings.extend(l.landings.iter().map(|g| pairs(&g.ring)));
        out.push(("stair", id, rings));
    }
    for id in ids(m, "SELECT id FROM railing WHERE level=?1 ORDER BY id", level) {
        let Ok(path) = m.railing_path(id) else { continue };
        let pts: Vec<P> = path.iter().map(|q| (q.0, q.1)).collect();
        if pts.len() >= 2 {
            out.push(("railing", id, vec![there_and_back(&pts)]));
        }
    }
    for g in m.grids().unwrap_or_default() {
        let s = &g.spec;
        out.push((
            "grid",
            g.id,
            vec![there_and_back(&grid_line(s.a, s.b, s.bulge))],
        ));
    }
    for (id, a, b) in m.ref_planes().unwrap_or_default() {
        out.push(("ref_plane", id, vec![vec![a, b]]));
    }
    out
}

/// Colours of the two datums (a pure colour tag).
const TAG_GRID: u32 = ddc_render::TAG_RGB | 0x3a_7b_d5;
const TAG_REF_PLANE: u32 = ddc_render::TAG_RGB | 0x8a_8f_95;
/// Ribbon: width and the lift above the level plane, m.
const WIDTH: f64 = 0.06;
const LIFT: (f64, f64) = (0.01, 0.05);

/// A thin box along every segment of the line.
fn ribbon(target: Target, tag: u32, line: &[P]) -> Part {
    let mut p = Part::new(target, tag);
    for w in line.windows(2) {
        let (a, b) = (v2(w[0].0, w[0].1), v2(w[1].0, w[1].1));
        let d = b - a;
        let l = d.len();
        if l < 1e-9 {
            continue;
        }
        let n = v2(-d.y, d.x) * (WIDTH / 2.0 / l);
        let base = p.pos.len() as u32;
        for z in [LIFT.0, LIFT.1] {
            for q in [a - n, b - n, b + n, a + n] {
                p.pos.push(Vec3::new(q.x as f32, q.y as f32, z as f32));
            }
        }
        // bottom, top and four sides
        let quads = [[0, 3, 2, 1], [4, 5, 6, 7], [0, 1, 5, 4], [1, 2, 6, 5], [2, 3, 7, 6], [3, 0, 4, 7]];
        for q in quads {
            p.tris.push([base + q[0], base + q[1], base + q[2]]);
            p.tris.push([base + q[0], base + q[2], base + q[3]]);
        }
    }
    p
}

/// Bodies of the grids; they are drawn on the active level only, once.
pub fn grids3d(ed: &Editor, level: i64) -> Vec<Part> {
    let Some(code) = code_of("grid") else {
        return vec![];
    };
    if level != ed.level {
        return vec![];
    }
    ed.model
        .grids()
        .unwrap_or_default()
        .iter()
        .map(|g| {
            let line = grid_line(g.spec.a, g.spec.b, g.spec.bulge);
            ribbon(Target::new(Cat::Ext(code), g.id), TAG_GRID, &line)
        })
        .collect()
}

/// Bodies of the reference planes, as [`grids3d`].
pub fn ref_planes3d(ed: &Editor, level: i64) -> Vec<Part> {
    let Some(code) = code_of("ref_plane") else {
        return vec![];
    };
    if level != ed.level {
        return vec![];
    }
    ed.model
        .ref_planes()
        .unwrap_or_default()
        .into_iter()
        .map(|(id, a, b)| ribbon(Target::new(Cat::Ext(code), id), TAG_REF_PLANE, &[a, b]))
        .collect()
}

fn with_len(a: P, b: P, len: f64) -> P {
    let ang = (b.1 - a.1).atan2(b.0 - a.0);
    (a.0 + len * ang.cos(), a.1 + len * ang.sin())
}

fn with_angle(a: P, b: P, ang: f64) -> P {
    let len = (b.0 - a.0).hypot(b.1 - a.1);
    (a.0 + len * ang.cos(), a.1 + len * ang.sin())
}

fn grid_ends(m: &Model, id: i64) -> Result<(P, P), Error> {
    let g = m.grid(id)?;
    Ok((g.spec.a, g.spec.b))
}

fn plane_ends(m: &Model, id: i64) -> Result<(P, P), Error> {
    let (a, b) = ref_plane_ends(m, id)?;
    Ok(((a.x, a.y), (b.x, b.y)))
}

fn set_grid(m: &mut Model, id: i64, f: impl FnOnce(P, P) -> P) -> Result<(), Error> {
    let mut s = m.grid(id)?.spec;
    s.b = f(s.a, s.b);
    m.update_grid(id, &s)
}

fn set_plane(m: &mut Model, id: i64, f: impl FnOnce(P, P) -> P) -> Result<(), Error> {
    let (a, b) = plane_ends(m, id)?;
    m.set_ref_plane(id, a, f(a, b))
}

/// Length and direction of a grid and of a reference plane; the far end moves, the first stays.
pub fn rows() -> Vec<Row> {
    let row = |kind, key, unit, range: (f64, f64), delta, get, set| Row {
        kind,
        key,
        unit,
        lo: range.0,
        hi: range.1,
        delta,
        effect: Effect::Scene,
        fields: &["b"],
        in_panel: true,
        get,
        set,
    };
    let len = (0.001, 1e3);
    let deg = (-360.0, 360.0);
    vec![
        row(
            "grid",
            "geom.line_length",
            Unit::Len,
            len,
            0.5,
            |m, id| {
                let (a, b) = grid_ends(m, id)?;
                Ok((b.0 - a.0).hypot(b.1 - a.1))
            },
            |m, id, v| set_grid(m, id, |a, b| with_len(a, b, v)),
        ),
        row(
            "grid",
            "geom.line_angle",
            Unit::Deg,
            deg,
            0.25,
            |m, id| {
                let (a, b) = grid_ends(m, id)?;
                Ok((b.1 - a.1).atan2(b.0 - a.0))
            },
            |m, id, v| set_grid(m, id, |a, b| with_angle(a, b, v)),
        ),
        row(
            "ref_plane",
            "geom.line_length",
            Unit::Len,
            len,
            0.5,
            |m, id| {
                let (a, b) = plane_ends(m, id)?;
                Ok((b.0 - a.0).hypot(b.1 - a.1))
            },
            |m, id, v| set_plane(m, id, |a, b| with_len(a, b, v)),
        ),
        row(
            "ref_plane",
            "geom.line_angle",
            Unit::Deg,
            deg,
            0.25,
            |m, id| {
                let (a, b) = plane_ends(m, id)?;
                Ok((b.1 - a.1).atan2(b.0 - a.0))
            },
            |m, id, v| set_plane(m, id, |a, b| with_angle(a, b, v)),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_and_back_keeps_a_line_a_line() {
        assert_eq!(there_and_back(&[(0.0, 0.0), (1.0, 0.0)]).len(), 2);
        let r = there_and_back(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)]);
        assert_eq!(r, vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (1.0, 0.0)]);
    }

    #[test]
    fn datum_codes_are_edge_only_except_the_stair() {
        for (k, edge) in [("stair", false), ("railing", true), ("grid", true), ("ref_plane", true)] {
            let c = code_of(k).unwrap();
            assert_eq!(edge_only(c), edge, "{k}");
        }
        use crate::editor::structx as sx;
        let named = [sx::STAIR_CODE, sx::RAILING_CODE, sx::GRID_CODE, sx::REF_PLANE_CODE];
        for (k, c) in sx::DATUM_KINDS.iter().zip(named) {
            assert_eq!(code_of(k), Some(c), "{k}");
        }
    }
}
