// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Link-derived data in host coordinates: plan lines (a section at the cut height), snaps.
//! Pure functions: the input is link elements (link world) and its placement. 3D is `source`.

use ddc_geom::{v2, V2};
use ddc_model::links::{section, Body, Transform};
use ddc_render::SceneMesh;

use super::load::{Elem, LinkWall};
use crate::snap::SnapWall;

/// Plan cut height above the current host level elevation, m (as for plan views).
pub const PLAN_CUT: f64 = 1.2;
/// Scene element for link meshes: it matches no host element, so it is not pickable.
pub const LINK_ELEMENT: u32 = crate::scene_ids::LINK;
/// Palette tag: neutral gray (see the shader `palette`, `t % 4 == 3`).
pub const LINK_TAG: u32 = 7;
/// Plan lines as snaps only if there are no more than this (otherwise just the link wall axes).
pub const MAX_LINE_SNAPS: usize = 4000;

/// Adds a scene mesh (z from elevation `z`) to a body in world coordinates.
pub fn append(b: &mut Body, m: &SceneMesh, z: f64) {
    let base = b.pos.len() as u32;
    b.pos.extend(
        m.positions
            .iter()
            .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64 + z]),
    );
    b.tri.extend(
        m.triangles
            .iter()
            .map(|t| [t[0] + base, t[1] + base, t[2] + base]),
    );
}

/// A link body in host coordinates.
pub fn body_in_host(b: &Body, xf: &Transform) -> Body {
    Body {
        pos: b.pos.iter().map(|&p| xf.apply(p)).collect(),
        tri: b.tri.clone(),
    }
}

/// Plan lines: the section of all link bodies by plane z = `zc` (host world), in the host plan.
pub fn plan_lines(elems: &[Elem], xf: &Transform, zc: f64) -> Vec<(V2, V2)> {
    let z = zc - xf.dz;
    let to = |p: [f64; 2]| {
        let (x, y) = xf.apply2(p[0], p[1]);
        v2(x, y)
    };
    elems
        .iter()
        .flat_map(|e| section(&e.body.pos, &e.body.tri, z))
        .map(|[a, b]| (to(a), to(b)))
        .collect()
}

/// Snaps: axes of link walls crossing the cut plane (with faces by thickness),
/// and plan lines if there are few.
pub fn snap_walls(
    walls: &[LinkWall],
    lines: &[(V2, V2)],
    xf: &Transform,
    zc: f64,
) -> Vec<SnapWall> {
    let z = zc - xf.dz;
    let to = |p: (f64, f64)| {
        let (x, y) = xf.apply2(p.0, p.1);
        v2(x, y)
    };
    let mut out: Vec<SnapWall> = walls
        .iter()
        .filter(|w| w.z0 <= z && z <= w.z1)
        .map(|w| SnapWall {
            a: to(w.a),
            b: to(w.b),
            half: w.half,
        })
        .collect();
    if lines.len() <= MAX_LINE_SNAPS {
        out.extend(lines.iter().map(|&(a, b)| SnapWall { a, b, half: 0.0 }));
    }
    out
}
