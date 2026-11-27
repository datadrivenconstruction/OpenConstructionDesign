// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Cutouts of openings and slabs in wall layer bodies.
//!
//! A layer piece is a convex plan × [bottom, top], a cutter is a plan polygon × [z0, z1]. The plan is cut
//! by the lines of the cutter edges into a consistent subdivision, in z by the cutter levels; the mesh is built
//! from the faces between filled and empty cells, so it is closed by construction.

use crate::grid::{bbox_of, Bbox, BoxGrid};
use crate::layered::{Opening, Strip};
use crate::math::{v3, Tol, V2};
use crate::mesh::Mesh;
use crate::predicates::point_in_ring;
use crate::profile::{signed_area, Profile};

/// Prism cutter: everything inside `ring` (except `holes`) in plan and within [z0, z1] in height is removed.
#[derive(Clone, Debug)]
pub struct Cutter {
    pub ring: Vec<V2>,
    /// Loop holes (a slab hole does not cut a wall).
    pub holes: Vec<Vec<V2>>,
    pub z0: f64,
    pub z1: f64,
}

#[derive(Debug, PartialEq)]
pub enum CutError {
    /// A plan coordinate or piece height is non-finite, or the height is not above the tolerance.
    BadPiece,
    /// The piece footprint is not convex.
    NotConvex,
    /// Cutter: fewer than 3 vertices, non-finite coordinates or z0 >= z1.
    BadCutter { cutter: usize },
    /// An edge with 4 faces that vertex splitting cannot separate: the body is non-manifold.
    NonManifold,
    /// Opening: non-finite or non-positive sizes.
    BadOpening { strip: usize, opening: usize },
}

impl std::fmt::Display for CutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadPiece => f.write_str("a wall piece has a non-finite coordinate or no height"),
            Self::NotConvex => f.write_str("a wall piece footprint is not convex"),
            Self::BadCutter { cutter } => write!(f, "cutter {cutter} is degenerate"),
            Self::NonManifold => f.write_str("the body is non-manifold"),
            Self::BadOpening { strip, opening } => write!(f, "opening {opening} of wall {strip} has an invalid size"),
        }
    }
}

/// Body after cuts: mesh and exact volume (Σ cell area × band height).
#[derive(Debug)]
pub struct CutBody {
    pub mesh: Mesh,
    pub volume: f64,
}

/// Opening cutter: rectangle [along, along+width] along the axis through the full thickness of the host layers.
pub fn opening_cutter(s: &Strip, o: &Opening) -> Cutter {
    let (d, n) = (s.dir(), s.normal());
    let total: f64 = s.layers.iter().map(|l| l.thickness).sum();
    let (v0, v1) = (s.offset0, s.offset0 + total);
    let (u0, u1) = (o.along, o.along + o.width);
    let at = |u: f64, v: f64| s.a + d * u + n * v;
    Cutter {
        ring: vec![at(u0, v0), at(u1, v0), at(u1, v1), at(u0, v1)],
        holes: vec![],
        z0: s.base + o.sill,
        z1: s.base + o.sill + o.height,
    }
}

fn finite2(p: V2) -> bool {
    p.x.is_finite() && p.y.is_finite()
}

/// The test `cut_walls` applies to cutter `k` of its `slabs` before cutting with it.
pub fn check_cutter(k: usize, c: &Cutter) -> Result<(), CutError> {
    let ok = c.ring.len() >= 3
        && c.ring.iter().all(|&p| finite2(p))
        && c.z0.is_finite()
        && c.z1.is_finite()
        && c.z0 < c.z1
        && signed_area(&c.ring) != 0.0
        && c.holes
            .iter()
            .all(|h| h.len() >= 3 && h.iter().all(|&p| finite2(p)));
    ok.then_some(()).ok_or(CutError::BadCutter { cutter: k })
}

struct Plan {
    pts: Vec<V2>,
    cells: Vec<Vec<u32>>,
}

impl Plan {
    /// Splits all cells by the line (o, d). The edge intersection point is computed from the vertex with the smaller
    /// id, so the side shared by two cells is split identically from both sides.
    fn split(&mut self, o: V2, d: V2, eps: f64) {
        let inv = 1.0 / d.len();
        let side: Vec<f64> = self
            .pts
            .iter()
            .map(|&p| {
                let s = d.cross(p - o) * inv;
                if s.abs() <= eps {
                    0.0
                } else {
                    s
                }
            })
            .collect();
        let mut made: Vec<(u32, u32, u32)> = vec![];
        let mut out = Vec::with_capacity(self.cells.len() * 2);
        for cell in std::mem::take(&mut self.cells) {
            let pos = cell.iter().any(|&v| side[v as usize] > 0.0);
            let neg = cell.iter().any(|&v| side[v as usize] < 0.0);
            if !(pos && neg) {
                out.push(cell);
                continue;
            }
            let (mut a, mut b) = (vec![], vec![]);
            let n = cell.len();
            for i in 0..n {
                let (v, w) = (cell[i], cell[(i + 1) % n]);
                let (sv, sw) = (side[v as usize], side[w as usize]);
                if sv >= 0.0 {
                    a.push(v);
                }
                if sv <= 0.0 {
                    b.push(v);
                }
                if sv * sw < 0.0 {
                    let (p, q) = (v.min(w), v.max(w));
                    let x = match made.iter().find(|m| m.0 == p && m.1 == q) {
                        Some(m) => m.2,
                        None => {
                            let (sp, sq) = (side[p as usize], side[q as usize]);
                            let (pp, pq) = (self.pts[p as usize], self.pts[q as usize]);
                            self.pts.push(pp + (pq - pp) * (sp / (sp - sq)));
                            let id = (self.pts.len() - 1) as u32;
                            made.push((p, q, id));
                            id
                        }
                    };
                    a.push(x);
                    b.push(x);
                }
            }
            out.push(a);
            out.push(b);
        }
        self.cells = out;
    }
}

fn cell_area(pts: &[V2], cell: &[u32]) -> f64 {
    let o = pts[cell[0] as usize];
    let mut s = 0.0;
    for i in 1..cell.len() - 1 {
        let (a, b) = (pts[cell[i] as usize] - o, pts[cell[i + 1] as usize] - o);
        s += a.cross(b);
    }
    s * 0.5
}

/// Cuts the cutters out of the prism `footprint` × [zb, zt]. The footprint is convex, any orientation.
pub fn cut_prism(
    footprint: &[V2],
    zb: f64,
    zt: f64,
    cutters: &[&Cutter],
    tol: Tol,
) -> Result<CutBody, CutError> {
    if footprint.len() < 3
        || !footprint.iter().all(|&p| finite2(p))
        || !zb.is_finite()
        || !zt.is_finite()
        || zt - zb <= tol.length
    {
        return Err(CutError::BadPiece);
    }
    for (k, c) in cutters.iter().enumerate() {
        check_cutter(k, c)?;
    }
    let mut fp = footprint.to_vec();
    let a = signed_area(&fp);
    if a == 0.0 || !a.is_finite() {
        return Err(CutError::BadPiece);
    }
    if a < 0.0 {
        fp.reverse();
    }
    let n = fp.len();
    for i in 0..n {
        let (p, q, r) = (fp[i], fp[(i + 1) % n], fp[(i + 2) % n]);
        if (q - p).cross(r - q) < -tol.length * (q - p).len().max((r - q).len()) {
            return Err(CutError::NotConvex);
        }
    }

    let fb = bbox_of(&fp, tol.length);
    let live: Vec<&Cutter> = cutters
        .iter()
        .copied()
        .filter(|c| {
            let cb = bbox_of(&c.ring, 0.0);
            c.z1 > zb + tol.length
                && c.z0 < zt - tol.length
                && cb[0] <= fb[2]
                && cb[2] >= fb[0]
                && cb[1] <= fb[3]
                && cb[3] >= fb[1]
        })
        .collect();

    let mut plan = Plan {
        pts: fp.clone(),
        cells: vec![(0..n as u32).collect()],
    };
    for c in &live {
        for r in std::iter::once(&c.ring).chain(&c.holes) {
            let m = r.len();
            for i in 0..m {
                let (p, q) = (r[i], r[(i + 1) % m]);
                if (q - p).len() > tol.length {
                    plan.split(p, q - p, tol.length);
                }
            }
        }
    }

    let mut levels = vec![zb, zt];
    for c in &live {
        for z in [c.z0, c.z1] {
            if z > zb + tol.length && z < zt - tol.length {
                levels.push(z);
            }
        }
    }
    levels.sort_by(f64::total_cmp);
    levels.dedup_by(|a, b| *a - *b <= tol.length);
    let nb = levels.len() - 1;

    let pts = &plan.pts;
    let cells = &plan.cells;
    // occupancy of cell × band
    let mut solid = vec![true; cells.len() * nb];
    for (ci, cell) in cells.iter().enumerate() {
        let mut cen = V2 { x: 0.0, y: 0.0 };
        for &v in cell {
            cen = cen + pts[v as usize];
        }
        cen = cen * (1.0 / cell.len() as f64);
        for c in &live {
            if point_in_ring(cen, &c.ring) != 1
                || c.holes.iter().any(|h| point_in_ring(cen, h) == 1)
            {
                continue;
            }
            for j in 0..nb {
                let mid = 0.5 * (levels[j] + levels[j + 1]);
                if mid > c.z0 && mid < c.z1 {
                    solid[ci * nb + j] = false;
                }
            }
        }
    }

    let mut volume = 0.0;
    for (ci, cell) in cells.iter().enumerate() {
        let ar = cell_area(pts, cell);
        for j in 0..nb {
            if solid[ci * nb + j] {
                volume += ar * (levels[j + 1] - levels[j]);
            }
        }
    }

    // directed plan edge → cell
    let mut edges: Vec<(u32, u32, u32)> = vec![];
    for (ci, cell) in cells.iter().enumerate() {
        for i in 0..cell.len() {
            edges.push((cell[i], cell[(i + 1) % cell.len()], ci as u32));
        }
    }
    edges.sort_unstable();
    let nbr = |p: u32, q: u32| -> Option<usize> {
        edges
            .binary_search_by(|e| (e.0, e.1).cmp(&(p, q)))
            .ok()
            .map(|k| edges[k].2 as usize)
    };

    // A vertex (plan point, level) is split by classes of filled cells around it
    // connected through a shared face: a diagonal touch yields two vertices, an edge yields 2 faces.
    let mut cells_of: Vec<Vec<u32>> = vec![vec![]; pts.len()];
    for (ci, cell) in cells.iter().enumerate() {
        for &v in cell {
            cells_of[v as usize].push(ci as u32);
        }
    }
    // (cell×band node, class representative) for all filled nodes around (p, l)
    let classes = |p: u32, l: usize| -> Vec<(usize, usize)> {
        let mut nodes: Vec<usize> = vec![];
        for &ck in &cells_of[p as usize] {
            for bb in [l.wrapping_sub(1), l] {
                if bb < nb && solid[ck as usize * nb + bb] {
                    nodes.push(ck as usize * nb + bb);
                }
            }
        }
        if nodes.len() <= 1 {
            return nodes.into_iter().map(|n| (n, 0)).collect();
        }
        let mut par: Vec<usize> = (0..nodes.len()).collect();
        fn find(par: &mut [usize], mut i: usize) -> usize {
            while par[i] != i {
                par[i] = par[par[i]];
                i = par[i];
            }
            i
        }
        let at = |n: usize| nodes.iter().position(|&x| x == n);
        for (i, &n) in nodes.iter().enumerate() {
            let (ck, bb) = (n / nb, n % nb);
            let link = |m: usize, par: &mut Vec<usize>| {
                if let Some(k) = at(m) {
                    let (x, y) = (find(par, i), find(par, k));
                    par[x.max(y)] = x.min(y);
                }
            };
            if bb + 1 == l {
                link(ck * nb + l, &mut par);
            }
            let cell = &cells[ck];
            let m = cell.len();
            let k = cell.iter().position(|&v| v == p).unwrap_or(0);
            let (prev, next) = (cell[(k + m - 1) % m], cell[(k + 1) % m]);
            for o in [nbr(next, p), nbr(p, prev)].into_iter().flatten() {
                link(o * nb + bb, &mut par);
            }
        }
        (0..nodes.len())
            .map(|i| (nodes[i], find(&mut par, i)))
            .collect()
    };
    let nl = levels.len();
    let mut memo: Vec<Option<Vec<(usize, usize, u32)>>> = vec![None; pts.len() * nl];
    let mut mesh = Mesh::default();
    let mut vid = |p: u32, l: usize, ci: usize, b: usize, mesh: &mut Mesh| -> u32 {
        let m = memo[p as usize * nl + l].get_or_insert_with(|| {
            classes(p, l)
                .into_iter()
                .map(|(n, r)| (n, r, u32::MAX))
                .collect()
        });
        let r = m.iter().find(|e| e.0 == ci * nb + b).map_or(0, |e| e.1);
        if let Some(e) = m.iter().find(|e| e.1 == r && e.2 != u32::MAX) {
            return e.2;
        }
        let q = pts[p as usize];
        mesh.positions.push(v3(q.x, q.y, levels[l]));
        let id = (mesh.positions.len() - 1) as u32;
        for e in m.iter_mut().filter(|e| e.1 == r) {
            e.2 = id;
        }
        id
    };
    for (ci, cell) in cells.iter().enumerate() {
        let s = |j: usize| solid[ci * nb + j];
        for j in 0..nb {
            if !s(j) {
                continue;
            }
            if j == 0 || !s(j - 1) {
                let v: Vec<u32> = cell.iter().map(|&p| vid(p, j, ci, j, &mut mesh)).collect();
                for i in 1..v.len() - 1 {
                    mesh.triangles.push([v[0], v[i + 1], v[i]]);
                    mesh.face_of.push(0);
                }
            }
            if j == nb - 1 || !s(j + 1) {
                let v: Vec<u32> = cell
                    .iter()
                    .map(|&p| vid(p, j + 1, ci, j, &mut mesh))
                    .collect();
                for i in 1..v.len() - 1 {
                    mesh.triangles.push([v[0], v[i], v[i + 1]]);
                    mesh.face_of.push(1);
                }
            }
            for i in 0..cell.len() {
                let (p, q) = (cell[i], cell[(i + 1) % cell.len()]);
                if nbr(q, p).is_some_and(|o| solid[o * nb + j]) {
                    continue;
                }
                let (p0, q0) = (vid(p, j, ci, j, &mut mesh), vid(q, j, ci, j, &mut mesh));
                let (p1, q1) = (
                    vid(p, j + 1, ci, j, &mut mesh),
                    vid(q, j + 1, ci, j, &mut mesh),
                );
                mesh.triangles.push([p0, q0, q1]);
                mesh.triangles.push([p0, q1, p1]);
                mesh.face_of.extend([2, 2]);
            }
        }
    }
    if !manifold(&mesh) {
        return Err(CutError::NonManifold);
    }
    Ok(CutBody { mesh, volume })
}

/// Every undirected edge has exactly two faces with opposite directions.
fn manifold(m: &Mesh) -> bool {
    let mut e: Vec<(u32, u32, bool)> = Vec::with_capacity(m.triangles.len() * 3);
    for t in &m.triangles {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            e.push((a.min(b), a.max(b), a < b));
        }
    }
    e.sort_unstable();
    e.chunks(2)
        .all(|c| c.len() == 2 && c[0].0 == c[1].0 && c[0].1 == c[1].1 && c[0].2 != c[1].2)
}

/// Node filler prism: footprint × [zb, zt].
#[derive(Clone, Debug)]
pub struct Filler {
    pub ring: Vec<V2>,
    pub zb: f64,
    pub zt: f64,
}

/// Result of `cut_walls`: a piece error does not affect the others; an invalid cutter is skipped.
#[derive(Debug)]
pub struct Cuts {
    /// `[wall][layer][piece]`.
    pub walls: Vec<Vec<Vec<Result<CutBody, CutError>>>>,
    pub fillers: Vec<Result<CutBody, CutError>>,
    /// Dropped cutters (`BadCutter` is an index into `slabs`, `BadOpening`).
    pub skipped: Vec<CutError>,
}

impl Cuts {
    /// All errors: dropped cutters, then wall pieces and fillers.
    pub fn errors(&self) -> impl Iterator<Item = &CutError> {
        let w = self.walls.iter().flatten().flatten();
        self.skipped
            .iter()
            .chain(w.chain(&self.fillers).filter_map(|r| r.as_ref().err()))
    }
}

/// Bodies of all layers of all walls and fillers, with cuts. An opening cuts every wall whose footprint
/// it touches (corner of a neighbouring wall, end of an abutting wall at a T-junction, node filler); slabs too.
pub fn cut_walls(
    strips: &[Strip],
    footprints: &[Vec<Vec<Profile>>],
    openings: &[Vec<Opening>],
    slabs: &[Cutter],
    fillers: &[Filler],
    tol: Tol,
) -> Cuts {
    let mut skipped = vec![];
    let mut cutters: Vec<Cutter> = vec![];
    for (k, c) in slabs.iter().enumerate() {
        match check_cutter(k, c) {
            Ok(()) => cutters.push(c.clone()),
            Err(e) => skipped.push(e),
        }
    }
    for (i, (s, os)) in strips.iter().zip(openings).enumerate() {
        for (k, o) in os.iter().enumerate() {
            let ok = [o.along, o.width, o.sill, o.height]
                .iter()
                .all(|v| v.is_finite())
                && o.width > 0.0
                && o.height > 0.0;
            if ok {
                cutters.push(opening_cutter(s, o));
            } else {
                skipped.push(CutError::BadOpening {
                    strip: i,
                    opening: k,
                });
            }
        }
    }
    let boxes: Vec<Bbox> = cutters.iter().map(|c| bbox_of(&c.ring, 0.0)).collect();
    let grid = BoxGrid::new(&boxes);
    let one = |ring: &[V2], zb: f64, zt: f64, cand: &mut Vec<usize>| {
        grid.query(&bbox_of(ring, tol.length), cand);
        let cs: Vec<&Cutter> = cand.iter().map(|&k| &cutters[k]).collect();
        cut_prism(ring, zb, zt, &cs, tol)
    };
    let idx: Vec<usize> = (0..strips.len().min(footprints.len())).collect();
    let walls = crate::layered::par_map(&idx, |&i| {
        let s = &strips[i];
        let mut cand = vec![];
        footprints[i]
            .iter()
            .map(|layer| {
                layer
                    .iter()
                    .map(|p| one(&p.outer, s.base, s.base + s.height, &mut cand))
                    .collect()
            })
            .collect()
    });
    let mut cand = vec![];
    let fillers = fillers
        .iter()
        .map(|f| one(&f.ring, f.zb, f.zt, &mut cand))
        .collect();
    Cuts {
        walls,
        fillers,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layered::{join, layer_footprints, Layer};
    use crate::math::v2;

    fn wall(a: V2, b: V2, t: f64, h: f64) -> Strip {
        Strip {
            a,
            b,
            layers: vec![Layer::new(t)],
            offset0: -t / 2.0,
            base: 0.0,
            height: h,
        }
    }

    fn op(along: f64, width: f64, sill: f64, height: f64) -> Opening {
        Opening {
            along,
            width,
            sill,
            height,
        }
    }

    /// Sum of all body volumes; checks closedness and agreement with the mesh volume.
    fn total(strips: &[Strip], ops: &[Vec<Opening>], slabs: &[Cutter]) -> f64 {
        let tol = Tol::default();
        let j = join(strips, tol).unwrap();
        let fps: Vec<_> = strips
            .iter()
            .enumerate()
            .map(|(i, s)| layer_footprints(s, &j.pieces[i], i, tol).unwrap())
            .collect();
        let cuts = cut_walls(strips, &fps, ops, slabs, &[], tol);
        assert_eq!(cuts.errors().count(), 0);
        let mut v = 0.0;
        for b in cuts.walls.iter().flatten().flatten() {
            let b = b.as_ref().unwrap();
            assert!(b.mesh.is_watertight() && manifold(&b.mesh));
            assert!((b.mesh.volume() - b.volume).abs() < 1e-9);
            v += b.volume;
        }
        v
    }

    fn room(t: f64) -> Vec<Strip> {
        let p = [v2(0., 0.), v2(6., 0.), v2(6., 4.), v2(0., 4.)];
        (0..4).map(|i| wall(p[i], p[(i + 1) % 4], t, 3.0)).collect()
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn room_door_window_exact() {
        let ops = vec![
            vec![op(2.55, 0.9, 0.0, 2.1)],
            vec![op(1.0, 1.2, 0.9, 1.4)],
            vec![],
            vec![],
        ];
        close(
            total(&room(0.2), &[vec![], vec![], vec![], vec![]], &[]),
            12.0,
        );
        close(total(&room(0.2), &ops, &[]), 11.286);
        close(total(&room(0.3), &ops, &[]), 16.929);
    }

    #[test]
    fn door_touching_wall_end_cuts_corner() {
        let ops = vec![vec![op(0.0, 0.9, 0.0, 2.1)], vec![], vec![], vec![]];
        close(total(&room(0.2), &ops, &[]), 12.0 - 0.378);
    }

    #[test]
    fn adjacent_openings() {
        let ops = vec![
            vec![op(1.0, 0.9, 0.0, 2.1), op(1.9, 1.2, 0.9, 1.4)],
            vec![],
            vec![],
            vec![],
        ];
        close(total(&room(0.2), &ops, &[]), 11.286);
    }

    #[test]
    fn opening_through_t_junction() {
        let s = vec![
            wall(v2(0., 0.), v2(6., 0.), 0.2, 3.0),
            wall(v2(3., 0.), v2(3., 3.), 0.2, 3.0),
        ];
        close(total(&s, &[vec![], vec![]], &[]), 5.34);
        close(
            total(&s, &[vec![op(2.5, 1.0, 0.0, 2.0)], vec![]], &[]),
            4.94,
        );
    }

    #[test]
    fn slab_cuts_wall() {
        let s = vec![wall(v2(0., 0.), v2(6., 0.), 0.2, 6.0)];
        let slab = Cutter {
            ring: vec![v2(-1., -1.), v2(3., -1.), v2(3., 1.), v2(-1., 1.)],
            holes: vec![],
            z0: 3.0,
            z1: 3.2,
        };
        close(total(&s, &[vec![]], &[slab]), 7.08);
    }

    fn check(b: &CutBody, want: f64) {
        assert!(b.mesh.is_watertight() && manifold(&b.mesh));
        close(b.volume, want);
        close(b.mesh.volume(), want);
    }

    #[test]
    fn diagonal_touch_is_manifold() {
        let tol = Tol::default();
        let s = wall(v2(0., 0.), v2(6., 0.), 0.2, 3.0);
        let fp = [v2(0., -0.1), v2(6., -0.1), v2(6., 0.1), v2(0., 0.1)];
        // a window at a door corner and a checkerboard of three openings
        let a = opening_cutter(&s, &op(1.0, 1.0, 0.0, 1.0));
        let b = opening_cutter(&s, &op(2.0, 1.0, 1.0, 1.0));
        let c = opening_cutter(&s, &op(3.0, 1.0, 0.0, 1.0));
        check(
            &cut_prism(&fp, 0.0, 3.0, &[&a, &b], tol).unwrap(),
            3.6 - 0.4,
        );
        check(
            &cut_prism(&fp, 0.0, 3.0, &[&a, &b, &c], tol).unwrap(),
            3.6 - 0.6,
        );
        // diagonal touch at the top and bottom of the wall
        let d = opening_cutter(&s, &op(4.0, 1.0, 2.0, 1.0));
        let e = opening_cutter(&s, &op(5.0, 1.0, 0.0, 2.0));
        check(
            &cut_prism(&fp, 0.0, 3.0, &[&d, &e], tol).unwrap(),
            3.6 - 0.6,
        );
    }

    #[test]
    fn opening_flush_with_top_and_bottom() {
        let tol = Tol::default();
        let s = wall(v2(0., 0.), v2(6., 0.), 0.2, 3.0);
        let fp = [v2(0., -0.1), v2(6., -0.1), v2(6., 0.1), v2(0., 0.1)];
        let full = opening_cutter(&s, &op(1.0, 1.0, 0.0, 3.0));
        let top = opening_cutter(&s, &op(3.0, 1.0, 2.0, 1.0));
        let over = opening_cutter(&s, &op(4.5, 0.5, -1.0, 5.0));
        check(&cut_prism(&fp, 0.0, 3.0, &[&top], tol).unwrap(), 3.6 - 0.2);
        // a full-height opening splits the body into two shells
        check(
            &cut_prism(&fp, 0.0, 3.0, &[&full, &top, &over], tol).unwrap(),
            3.6 - 0.6 - 0.2 - 0.3,
        );
    }

    #[test]
    fn slab_flush_with_wall_top_and_holes() {
        let s = vec![wall(v2(0., 0.), v2(6., 0.), 0.2, 3.0)];
        let sq = |x0: f64, x1: f64| vec![v2(x0, -1.), v2(x1, -1.), v2(x1, 1.), v2(x0, 1.)];
        let at_top = Cutter {
            ring: sq(-1., 7.),
            holes: vec![],
            z0: 2.8,
            z1: 3.0,
        };
        close(total(&s, &[vec![]], &[at_top]), 3.6 - 6. * 0.2 * 0.2);
        let above = Cutter {
            ring: sq(-1., 7.),
            holes: vec![],
            z0: 3.0,
            z1: 3.2,
        };
        close(total(&s, &[vec![]], &[above]), 3.6);
        let mut holed = Cutter {
            ring: sq(-1., 7.),
            holes: vec![sq(2., 4.)],
            z0: 1.0,
            z1: 1.2,
        };
        holed.holes[0].reverse();
        close(total(&s, &[vec![]], &[holed]), 3.6 - 4. * 0.2 * 0.2);
    }

    #[test]
    fn rotated_wall_exact() {
        let tol = Tol::default();
        let (c, sn) = (0.6_f64, 0.8_f64);
        let s = wall(v2(1., 2.), v2(1. + 6. * c, 2. + 6. * sn), 0.2, 3.0);
        let n = s.normal();
        let fp: Vec<V2> = [(0., -0.1), (6., -0.1), (6., 0.1), (0., 0.1)]
            .iter()
            .map(|&(u, v)| s.a + s.dir() * u + n * v)
            .collect();
        let a = opening_cutter(&s, &op(1.0, 0.9, 0.0, 2.1));
        let b = opening_cutter(&s, &op(1.9, 1.2, 2.1, 0.9));
        check(
            &cut_prism(&fp, 0.0, 3.0, &[&a, &b], tol).unwrap(),
            3.6 - 0.378 - 0.216,
        );
    }

    #[test]
    fn filler_and_bad_piece_isolated() {
        let tol = Tol::default();
        let s = wall(v2(0., 0.), v2(6., 0.), 0.2, 3.0);
        let fp = |x0: f64, x1: f64| {
            Profile::new(
                vec![v2(x0, -0.1), v2(x1, -0.1), v2(x1, 0.1), v2(x0, 0.1)],
                vec![],
                tol,
            )
            .unwrap()
        };
        let fps = vec![vec![vec![fp(0., 6.)]]];
        // a door at the wall end extends into the filler [-0.2, 0] × thickness
        let f = Filler {
            ring: vec![v2(-0.2, -0.1), v2(0., -0.1), v2(0., 0.1), v2(-0.2, 0.1)],
            zb: 0.0,
            zt: 3.0,
        };
        let bad = Filler {
            ring: vec![v2(0., 0.), v2(1., 0.), v2(f64::NAN, 1.)],
            zb: 0.0,
            zt: 3.0,
        };
        let o = vec![vec![op(-0.2, 1.1, 0.0, 2.1)]];
        let c = cut_walls(&[s], &fps, &o, &[], &[f, bad], tol);
        check(c.walls[0][0][0].as_ref().unwrap(), 3.6 - 0.9 * 2.1 * 0.2);
        check(c.fillers[0].as_ref().unwrap(), 0.2 * 0.2 * (3.0 - 2.1));
        assert_eq!(c.fillers[1].as_ref().err(), Some(&CutError::BadPiece));
        assert_eq!(c.errors().count(), 1);
    }

    #[test]
    fn bad_input_is_error_not_panic() {
        let tol = Tol::default();
        let fp = [v2(0., 0.), v2(1., 0.), v2(1., 1.), v2(0., 1.)];
        let nan = Cutter {
            ring: vec![v2(f64::NAN, 0.), v2(1., 0.), v2(1., 1.)],
            holes: vec![],
            z0: 0.0,
            z1: 1.0,
        };
        let flat = Cutter {
            ring: vec![v2(0., 0.), v2(1., 0.), v2(2., 0.)],
            holes: vec![],
            z0: 0.0,
            z1: 1.0,
        };
        assert_eq!(
            cut_prism(&fp, 0.0, 1.0, &[&nan], tol).err(),
            Some(CutError::BadCutter { cutter: 0 })
        );
        assert!(cut_prism(&fp, 0.0, 1.0, &[&flat], tol).is_err());
        assert!(cut_prism(&fp, 0.0, f64::NAN, &[], tol).is_err());
        assert!(cut_prism(&fp[..2], 0.0, 1.0, &[], tol).is_err());
        let star = [v2(0., 0.), v2(2., 0.), v2(1., 0.2), v2(1., 2.)];
        assert_eq!(
            cut_prism(&star, 0.0, 1.0, &[], tol).err(),
            Some(CutError::NotConvex)
        );
        let s = wall(v2(0., 0.), v2(6., 0.), 0.2, 3.0);
        let fps = vec![vec![vec![Profile::new(
            vec![v2(0., -0.1), v2(6., -0.1), v2(6., 0.1), v2(0., 0.1)],
            vec![],
            tol,
        )
        .unwrap()]]];
        let c = cut_walls(
            &[s],
            &fps,
            &[vec![op(1.0, f64::NAN, 0.0, 1.0)]],
            &[],
            &[],
            tol,
        );
        assert_eq!(
            c.skipped,
            vec![CutError::BadOpening {
                strip: 0,
                opening: 0
            }]
        );
        close(c.walls[0][0][0].as_ref().unwrap().volume, 1.2 * 3.0);
    }

    #[test]
    fn deterministic_bits() {
        let ops = vec![
            vec![op(0.0, 0.9, 0.0, 2.1)],
            vec![op(1.0, 1.2, 0.9, 1.4)],
            vec![],
            vec![],
        ];
        let run = || {
            let tol = Tol::default();
            let s = room(0.2);
            let j = join(&s, tol).unwrap();
            let fps: Vec<_> = (0..4)
                .map(|i| layer_footprints(&s[i], &j.pieces[i], i, tol).unwrap())
                .collect();
            let b = cut_walls(&s, &fps, &ops, &[], &[], tol).walls;
            b.into_iter()
                .flatten()
                .flatten()
                .map(Result::unwrap)
                .flat_map(|b| {
                    b.mesh
                        .positions
                        .into_iter()
                        .map(|p| (p.x.to_bits(), p.z.to_bits()))
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn random_openings_volume_matches_analytic() {
        // simple generator: deterministic LCG
        let mut x: u64 = 0x9e3779b97f4a7c15;
        let mut rnd = || {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 11) as f64 / (1u64 << 53) as f64
        };
        let tol = Tol::default();
        for _ in 0..300 {
            let len = 1.0 + rnd() * 10.0;
            let t = 0.05 + rnd() * 0.5;
            let s = wall(v2(0., 0.), v2(len, 0.), t, 3.0);
            // non-overlapping openings within the axis
            let mut ops = vec![];
            let mut u = 0.0;
            while u < len {
                let w = 0.1 + rnd() * 1.5;
                if u + w > len {
                    break;
                }
                if rnd() < 0.6 {
                    let sill = (rnd() * 2.0 * 10.0).round() / 10.0;
                    let h = (0.1 + rnd() * (3.0 - sill - 0.1)).min(3.0 - sill);
                    ops.push(op(u, w, sill, h));
                }
                u += w + if rnd() < 0.3 { 0.0 } else { rnd() };
            }
            let fp = [
                v2(0., -t / 2.),
                v2(len, -t / 2.),
                v2(len, t / 2.),
                v2(0., t / 2.),
            ];
            let cs: Vec<Cutter> = ops.iter().map(|o| opening_cutter(&s, o)).collect();
            let rs: Vec<&Cutter> = cs.iter().collect();
            let want = len * t * 3.0 - ops.iter().map(|o| o.width * o.height * t).sum::<f64>();
            let b = cut_prism(&fp, 0.0, 3.0, &rs, tol).unwrap();
            assert!(b.mesh.is_watertight() && manifold(&b.mesh));
            assert!((b.volume - want).abs() < 1e-9, "{} {want}", b.volume);
            assert!((b.mesh.volume() - want).abs() < 1e-9);
        }
    }

    #[test]
    #[ignore = "budget: cargo test --release -p ddc-geom budget_10k -- --ignored"]
    fn budget_10k_walls_two_openings() {
        let tol = Tol::default();
        let mut strips = vec![];
        let mut ops = vec![];
        let mut fps = vec![];
        for i in 0..10_000 {
            let (x, y) = ((i % 100) as f64 * 8.0, (i / 100) as f64 * 2.0);
            let s = wall(v2(x, y), v2(x + 6.0, y), 0.2, 3.0);
            fps.push(vec![vec![Profile::new(
                vec![
                    v2(x, y - 0.1),
                    v2(x + 6.0, y - 0.1),
                    v2(x + 6.0, y + 0.1),
                    v2(x, y + 0.1),
                ],
                vec![],
                tol,
            )
            .unwrap()]]);
            strips.push(s);
            ops.push(vec![op(1.0, 0.9, 0.0, 2.1), op(3.5, 1.2, 0.9, 1.4)]);
        }
        let t = web_time::Instant::now();
        let b = cut_walls(&strips, &fps, &ops, &[], &[], tol).walls;
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let v: f64 = b
            .iter()
            .flatten()
            .flatten()
            .map(|b| b.as_ref().unwrap().volume)
            .sum();
        eprintln!("cut 10k walls: {ms:.1} ms, V {v}");
        assert!((v - 10_000.0 * (3.6 - 0.378 - 0.336)).abs() < 1e-6);
        assert!(ms < 1000.0);
    }
}
