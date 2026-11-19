// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Section fills by a clipping box (CPU, per mesh): a body crossing a box face
//! is cut by its plane. The section segment of each triangle is directed along its normal
//! (body on the left), so contours of closed shells run counter-clockwise and holes
//! clockwise. Open tails are discarded; if a noticeable amount is discarded or the segment
//! graph is unbalanced, the mesh plane is not filled (better no fill than garbage).
//! The fill uses the nonzero winding rule (overlapping shells merge), split into
//! trapezoids by strips between segment endpoints (with intersection points), clipped by the
//! face rectangle. Time is limited by `Budget` (a deadline, a cancel flag for background work).

use crate::scene::{Aabb, Scene, SceneMesh};
use glam::Vec3;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use web_time::Instant;

pub type P = [f64; 2];
/// Directed section segment: body on the left.
pub type Seg = (P, P);

/// Fraction of discarded open segment length beyond which the plane is not filled.
const OPEN_TOLERANCE: f64 = 0.01;
/// Splits of one strip at intersection points; beyond that — as is.
const MAX_SPLITS: usize = 256;

/// Computation limit: a deadline and/or a cancel flag.
#[derive(Clone, Copy, Debug, Default)]
pub struct Budget<'a> {
    pub deadline: Option<Instant>,
    pub cancel: Option<&'a AtomicBool>,
}

impl Budget<'_> {
    pub fn over(&self) -> bool {
        self.cancel.is_some_and(|c| c.load(Ordering::Relaxed))
            || self.deadline.is_some_and(|d| Instant::now() >= d)
    }
}

/// Computation interrupted (deadline passed or cancelled).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Over;

fn dist2(a: P, b: P) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

/// Section segments of a mesh by the plane `p[axis] = c` in coordinates (`(axis+1)%3`, `(axis+2)%3`)
/// — a right-handed triple, viewed from +axis. Direction comes from the triangle normal (vertex winding).
pub fn section_segments(m: &SceneMesh, axis: usize, c: f32) -> Vec<Seg> {
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let c = c as f64;
    let mut segs = Vec::new();
    for t in &m.triangles {
        let p = t.map(|i| m.positions[i as usize].map(|x| x as f64));
        let d = p.map(|q| q[axis] - c);
        let mut pts = [[0.0; 2]; 2];
        let mut n = 0;
        for k in 0..3 {
            let (a, b) = (k, (k + 1) % 3);
            // a vertex on the plane counts as above: no double points
            if (d[a] < 0.0) != (d[b] < 0.0) {
                // a shared edge of neighbours is computed with the same endpoint order — points match bitwise
                let (a, b) = if p[a] <= p[b] { (a, b) } else { (b, a) };
                let s = d[a] / (d[a] - d[b]);
                if n < 2 {
                    pts[n] = [
                        p[a][u] + (p[b][u] - p[a][u]) * s,
                        p[a][v] + (p[b][v] - p[a][v]) * s,
                    ];
                }
                n += 1;
            }
        }
        if n != 2 || dist2(pts[0], pts[1]) <= 1e-18 {
            continue;
        }
        let e1 = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
        let e2 = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
        let nrm = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        // the body is against the normal; left of the direction (−n_v, n_u) in the (u, v) plane
        let dir = [-nrm[v], nrm[u]];
        let s = [pts[1][0] - pts[0][0], pts[1][1] - pts[0][1]];
        if s[0] * dir[0] + s[1] * dir[1] < 0.0 {
            pts.swap(0, 1);
        }
        segs.push((pts[0], pts[1]));
    }
    segs
}

/// Only segments of closed contours: tails (a node without an incoming or outgoing segment) are peeled
/// off along the chain. `None` — more than `OPEN_TOLERANCE` of the length was removed or the remainder is unbalanced
/// (in ≠ out at a node): the section is corrupted and must not be filled.
pub fn closed(segs: Vec<Seg>, eps: f64) -> Option<Vec<Seg>> {
    let key = |p: P| ((p[0] / eps).round() as i64, (p[1] / eps).round() as i64);
    let mut ids: HashMap<(i64, i64), usize> = HashMap::with_capacity(segs.len());
    let mut node = |p: P| {
        let n = ids.len();
        *ids.entry(key(p)).or_insert(n)
    };
    let ends: Vec<(usize, usize)> = segs.iter().map(|s| (node(s.0), node(s.1))).collect();
    let nn = ids.len();
    let (mut indeg, mut outdeg) = (vec![0u32; nn], vec![0u32; nn]);
    let (mut out_of, mut in_of) = (vec![vec![]; nn], vec![vec![]; nn]);
    for (i, &(a, b)) in ends.iter().enumerate() {
        outdeg[a] += 1;
        indeg[b] += 1;
        out_of[a].push(i);
        in_of[b].push(i);
    }
    let mut alive = vec![true; segs.len()];
    let mut stack: Vec<usize> = (0..nn)
        .filter(|&k| indeg[k] == 0 || outdeg[k] == 0)
        .collect();
    while let Some(k) = stack.pop() {
        if indeg[k] == 0 {
            for &i in &out_of[k] {
                if alive[i] {
                    alive[i] = false;
                    outdeg[k] -= 1;
                    let b = ends[i].1;
                    indeg[b] -= 1;
                    if indeg[b] == 0 {
                        stack.push(b);
                    }
                }
            }
        }
        if outdeg[k] == 0 {
            for &i in &in_of[k] {
                if alive[i] {
                    alive[i] = false;
                    indeg[k] -= 1;
                    let a = ends[i].0;
                    outdeg[a] -= 1;
                    if outdeg[a] == 0 {
                        stack.push(a);
                    }
                }
            }
        }
    }
    let len = |s: &Seg| dist2(s.0, s.1).sqrt();
    let total: f64 = segs.iter().map(len).sum();
    let dropped: f64 = segs
        .iter()
        .zip(&alive)
        .filter(|(_, a)| !**a)
        .map(|(s, _)| len(s))
        .sum();
    if dropped > total * OPEN_TOLERANCE || indeg != outdeg {
        return None;
    }
    Some(
        segs.into_iter()
            .zip(alive)
            .filter(|(_, a)| *a)
            .map(|(s, _)| s)
            .collect(),
    )
}

/// Strip edge: y0 < y1, `w` — winding step when crossing left to right.
struct Edge {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    w: i32,
}

impl Edge {
    fn at(&self, y: f64) -> f64 {
        self.x0 + (self.x1 - self.x0) * (y - self.y0) / (self.y1 - self.y0)
    }
}

/// Triangles of the region with nonzero winding of segments `segs`, clipped by the rectangle
/// [lo, hi]. Strips lie between adjacent endpoint marks; inside a strip the edge order
/// is constant (a strip is split at intersection points). `None` — the budget ran out.
pub fn fill(segs: &[Seg], lo: P, hi: P, budget: &Budget) -> Option<Vec<[P; 3]>> {
    let mut es: Vec<Edge> = segs
        .iter()
        .filter(|(a, b)| a[1] != b[1])
        .map(|&(a, b)| {
            // up — leaving the body (body on the left), down — entering
            let (p, q, w) = if a[1] < b[1] { (a, b, -1) } else { (b, a, 1) };
            Edge {
                x0: p[0],
                y0: p[1],
                x1: q[0],
                y1: q[1],
                w,
            }
        })
        .filter(|e| e.y1 > lo[1] && e.y0 < hi[1])
        .collect();
    es.sort_by(|a, b| a.y0.total_cmp(&b.y0));
    let mut ys: Vec<f64> = es
        .iter()
        .flat_map(|e| [e.y0, e.y1])
        .chain([lo[1], hi[1]])
        .filter(|y| *y >= lo[1] && *y <= hi[1])
        .collect();
    ys.sort_by(f64::total_cmp);
    ys.dedup();
    let scale = (hi[0] - lo[0]).abs().max((hi[1] - lo[1]).abs()).max(1e-9);
    let tiny = scale * 1e-12;
    let mut out = Vec::new();
    let (mut next, mut act) = (0usize, Vec::<usize>::new());
    let mut ord: Vec<(f64, f64, usize)> = Vec::new();
    for win in ys.windows(2) {
        if budget.over() {
            return None;
        }
        let (mut y0, y1) = (win[0], win[1]);
        while next < es.len() && es[next].y0 <= y0 {
            act.push(next);
            next += 1;
        }
        act.retain(|&i| es[i].y1 > y0);
        let mut splits = 0;
        while y0 < y1 && !act.is_empty() {
            ord.clear();
            ord.extend(act.iter().map(|&i| (es[i].at(y0), es[i].at(y1), i)));
            ord.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
            // the first intersection above y0 — among neighbours in order at y0
            let mut yc = y1;
            if splits < MAX_SPLITS {
                for p in ord.windows(2) {
                    let (a, b) = (p[0], p[1]);
                    if a.1 > b.1 + tiny {
                        let (d0, d1) = (a.0 - b.0, a.1 - b.1);
                        let y = y0 + (-d0 / (d1 - d0)) * (y1 - y0);
                        if y > y0 + tiny && y < yc {
                            yc = y;
                        }
                    }
                }
            }
            let (mut wsum, mut start) = (0i32, None::<usize>);
            for &(_, _, i) in &ord {
                let before = wsum;
                wsum += es[i].w;
                if before == 0 && wsum != 0 {
                    start = Some(i);
                } else if before != 0 && wsum == 0 {
                    if let Some(s) = start.take() {
                        let (l, r) = (&es[s], &es[i]);
                        let quad = [
                            [l.at(y0), y0],
                            [r.at(y0), y0],
                            [r.at(yc), yc],
                            [l.at(yc), yc],
                        ];
                        let poly = clip_rect(&quad, lo, hi);
                        for k in 1..poly.len().saturating_sub(1) {
                            let t = [poly[0], poly[k], poly[k + 1]];
                            if cross(t[0], t[1], t[2]).abs() > tiny * tiny {
                                out.push(t);
                            }
                        }
                    }
                }
            }
            y0 = yc;
            splits += 1;
        }
    }
    Some(out)
}

pub fn area(r: &[P]) -> f64 {
    let n = r.len();
    (0..n)
        .map(|i| {
            let (a, b) = (r[i], r[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        * 0.5
}

fn cross(o: P, a: P, b: P) -> f64 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// Clipping a convex contour by a rectangle (Sutherland–Hodgman over four half-planes).
pub fn clip_rect(r: &[P], lo: P, hi: P) -> Vec<P> {
    let mut cur = r.to_vec();
    for (k, bound, keep_below) in [
        (0, lo[0], false),
        (0, hi[0], true),
        (1, lo[1], false),
        (1, hi[1], true),
    ] {
        if cur.is_empty() {
            break;
        }
        let ins = |p: &P| {
            if keep_below {
                p[k] <= bound
            } else {
                p[k] >= bound
            }
        };
        let mut out = Vec::with_capacity(cur.len() + 4);
        for i in 0..cur.len() {
            let (a, b) = (cur[i], cur[(i + 1) % cur.len()]);
            let (ia, ib) = (ins(&a), ins(&b));
            if ia {
                out.push(a);
            }
            if ia != ib {
                let s = (bound - a[k]) / (b[k] - a[k]);
                let mut q = [a[0] + (b[0] - a[0]) * s, a[1] + (b[1] - a[1]) * s];
                q[k] = bound;
                out.push(q);
            }
        }
        cur = out;
    }
    cur.dedup_by(|a, b| dist2(*a, *b) < 1e-20);
    if cur.len() > 1 && dist2(cur[0], cur[cur.len() - 1]) < 1e-20 {
        cur.pop();
    }
    cur
}

/// Whether a face of box `b` intersects box `mb` (the body is cut by at least one plane).
fn crosses(mb: &Aabb, b: &Aabb) -> bool {
    !mb.is_empty()
        && (0..3).any(|k| {
            (mb.min[k] < b.min[k] && b.min[k] < mb.max[k])
                || (mb.min[k] < b.max[k] && b.max[k] < mb.max[k])
        })
}

/// Fills of the faces of box `b` for one mesh (bounds `mb`); `tag` — the fill tag.
pub fn mesh_caps(
    m: &SceneMesh,
    mb: &Aabb,
    b: &Aabb,
    tag: u32,
    budget: &Budget,
) -> Result<Option<SceneMesh>, Over> {
    let mut out = SceneMesh {
        element: m.element,
        tag,
        positions: vec![],
        triangles: vec![],
    };
    for axis in 0..3 {
        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
        if mb.max[u] < b.min[u]
            || mb.min[u] > b.max[u]
            || mb.max[v] < b.min[v]
            || mb.min[v] > b.max[v]
        {
            return Ok(None);
        }
        for c in [b.min[axis], b.max[axis]] {
            if !(mb.min[axis] < c && c < mb.max[axis]) {
                continue;
            }
            if budget.over() {
                return Err(Over);
            }
            let Some(segs) = closed(section_segments(m, axis, c), 1e-6) else {
                continue;
            };
            let lo = [b.min[u] as f64, b.min[v] as f64];
            let hi = [b.max[u] as f64, b.max[v] as f64];
            for t in fill(&segs, lo, hi, budget).ok_or(Over)? {
                let base = out.positions.len() as u32;
                for p in t {
                    let mut q = [0f32; 3];
                    q[axis] = c;
                    q[u] = p[0] as f32;
                    q[v] = p[1] as f32;
                    out.positions.push(q);
                }
                out.triangles.push([base, base + 1, base + 2]);
            }
        }
    }
    Ok((!out.triangles.is_empty()).then_some(out))
}

/// Section fills of the whole scene by box `b` with a source number: mesh `i` or instance
/// group `meshes.len() + g` (their meshes are transformed to world). `tag_of` gives the fill tag
/// or `None` (do not fill: glass, lines). `Err` — the budget ran out, no partial result is returned.
pub fn scene_caps(
    scene: &Scene,
    b: &Aabb,
    tag_of: impl Fn(&SceneMesh) -> Option<u32>,
    budget: &Budget,
) -> Result<Vec<(usize, SceneMesh)>, Over> {
    let mut out = Vec::new();
    for (i, (m, mb)) in scene.meshes.iter().zip(&scene.boxes).enumerate() {
        if !crosses(mb, b) {
            continue;
        }
        let m: &SceneMesh = m;
        if let Some(tag) = tag_of(m) {
            if let Some(c) = mesh_caps(m, mb, b, tag, budget)? {
                out.push((i, c));
            }
        }
    }
    let n = scene.meshes.len();
    for (g, grp) in scene.instanced.iter().enumerate() {
        for inst in grp.inst.iter() {
            let wb = grp.world_bounds(inst);
            if !crosses(&wb, b) {
                continue;
            }
            // a mirrored placement flips the winding — the nonzero winding rule tolerates it
            let w = SceneMesh {
                element: inst.element,
                tag: grp.proto.tag,
                positions: grp
                    .proto
                    .positions
                    .iter()
                    .map(|p| inst.m.transform_point3(Vec3::from(*p)).into())
                    .collect(),
                triangles: grp.proto.triangles.clone(),
            };
            if let Some(tag) = tag_of(&w) {
                if let Some(c) = mesh_caps(&w, &wb, b, tag, budget)? {
                    out.push((n + g, c));
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closed box [x0,x1]×[y0,y1]×[z0,z1] (12 triangles, normals outward).
    fn cube(min: [f32; 3], max: [f32; 3]) -> SceneMesh {
        let p: Vec<[f32; 3]> = (0..8)
            .map(|i| {
                [
                    if i & 1 == 0 { min[0] } else { max[0] },
                    if i & 2 == 0 { min[1] } else { max[1] },
                    if i & 4 == 0 { min[2] } else { max[2] },
                ]
            })
            .collect();
        let q = |a, b, c, d| [[a, b, c], [a, c, d]];
        let triangles = [
            q(0, 2, 3, 1),
            q(4, 5, 7, 6),
            q(0, 1, 5, 4),
            q(2, 6, 7, 3),
            q(0, 4, 6, 2),
            q(1, 3, 7, 5),
        ]
        .concat();
        SceneMesh {
            element: 7,
            tag: 0,
            positions: p,
            triangles,
        }
    }

    fn merge(a: SceneMesh, b: SceneMesh) -> SceneMesh {
        let mut o = a;
        let base = o.positions.len() as u32;
        o.positions.extend(b.positions);
        o.triangles
            .extend(b.triangles.iter().map(|t| t.map(|k| k + base)));
        o
    }

    /// Tube: walls of the outer box face outward, of the inner one — inward (the body between them).
    fn tube() -> SceneMesh {
        let mut o = cube([0.0, 0.0, 0.0], [4.0, 4.0, 3.0]);
        let mut i = cube([1.0, 1.0, 0.0], [3.0, 3.0, 3.0]);
        o.triangles = o.triangles[4..].to_vec();
        i.triangles = i.triangles[4..]
            .iter()
            .map(|t| [t[0], t[2], t[1]])
            .collect();
        merge(o, i)
    }

    fn tri_area(m: &SceneMesh) -> f64 {
        m.triangles
            .iter()
            .map(|t| {
                let p = t.map(|i| Vec3::from(m.positions[i as usize]));
                ((p[1] - p[0]).cross(p[2] - p[0]).length() * 0.5) as f64
            })
            .sum()
    }

    fn filled(segs: &[Seg], lo: P, hi: P) -> f64 {
        fill(segs, lo, hi, &Budget::default())
            .unwrap()
            .iter()
            .map(|t| cross(t[0], t[1], t[2]).abs() * 0.5)
            .sum()
    }

    const BIG: (P, P) = ([-100.0, -100.0], [100.0, 100.0]);

    #[test]
    fn segments_keep_direction_body_on_the_left() {
        let m = cube([0.0, 0.0, 0.0], [2.0, 1.0, 3.0]);
        let s = closed(section_segments(&m, 2, 1.5), 1e-6).unwrap();
        // one segment per triangle: two per side face
        assert_eq!(s.len(), 8);
        // counter-clockwise contour: the sum of signed areas is positive
        let a: f64 = s
            .iter()
            .map(|(p, q)| p[0] * q[1] - q[0] * p[1])
            .sum::<f64>()
            * 0.5;
        assert!((a - 2.0).abs() < 1e-9, "{a}");
        assert!((filled(&s, BIG.0, BIG.1) - 2.0).abs() < 1e-9);
        // along X: coordinates (y, z)
        let s = closed(section_segments(&m, 0, 1.0), 1e-6).unwrap();
        assert!((filled(&s, BIG.0, BIG.1) - 3.0).abs() < 1e-9);
        assert!(section_segments(&m, 2, 5.0).is_empty());
    }

    #[test]
    fn ring_hole_by_winding() {
        let s = closed(section_segments(&tube(), 2, 1.0), 1e-6).unwrap();
        assert!((filled(&s, BIG.0, BIG.1) - 12.0).abs() < 1e-9);
        // a rectangle cuts the ring through the hole: the opening is not filled
        let a = filled(&s, [-1.0, 1.5], [5.0, 2.5]);
        assert!((a - 2.0).abs() < 1e-9, "{a}");
    }

    #[test]
    fn overlapping_shells_union_not_xor() {
        // two intersecting boxes in one mesh (tread + stringer): union 4×2 + 2×4 − 2×2
        let m = merge(
            cube([0.0, 0.0, 0.0], [4.0, 2.0, 3.0]),
            cube([1.0, 0.0, 0.0], [3.0, 4.0, 3.0]),
        );
        let s = closed(section_segments(&m, 2, 1.0), 1e-6).unwrap();
        let a = filled(&s, BIG.0, BIG.1);
        assert!((a - 12.0).abs() < 1e-9, "{a}");
        // cross
        let m = merge(
            cube([0.0, 1.0, 0.0], [5.0, 2.0, 3.0]),
            cube([2.0, -1.0, 0.0], [3.0, 4.0, 3.0]),
        );
        let s = closed(section_segments(&m, 2, 1.0), 1e-6).unwrap();
        assert!((filled(&s, BIG.0, BIG.1) - 9.0).abs() < 1e-9);
        // square and rhombus: slanted edges cross the square's side inside strips
        let ring =
            |p: &[P]| -> Vec<Seg> { (0..p.len()).map(|i| (p[i], p[(i + 1) % p.len()])).collect() };
        let mut s = ring(&[[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]]);
        s.extend(ring(&[[2.5, 0.0], [3.5, 1.0], [2.5, 2.0], [1.5, 1.0]]));
        let a = filled(&s, BIG.0, BIG.1);
        assert!((a - 5.75).abs() < 1e-9, "{a}");
        // the same contour, clockwise (mirrored) — the same area
        let rev: Vec<Seg> = s.iter().map(|&(a, b)| (b, a)).collect();
        assert!((filled(&rev, BIG.0, BIG.1) - 5.75).abs() < 1e-9);
    }

    #[test]
    fn open_or_unbalanced_section_is_skipped() {
        // a box missing one side face: the section contour is open
        let mut m = cube([0.0, 0.0, 0.0], [4.0, 2.0, 3.0]);
        m.triangles.drain(4..6);
        assert!(closed(section_segments(&m, 2, 1.0), 1e-6).is_none());
        let b = Aabb {
            min: Vec3::splat(-1.0),
            max: Vec3::new(5.0, 5.0, 1.0),
        };
        let mb = crate::scene::mesh_box(&m);
        assert!(matches!(
            mesh_caps(&m, &mb, &b, 1, &Budget::default()),
            Ok(None)
        ));
        // a small tail (below tolerance) is simply discarded
        let mut s = section_segments(&cube([0.0; 3], [4.0, 2.0, 3.0]), 2, 1.0);
        s.push(([9.0, 9.0], [9.0, 9.05]));
        assert_eq!(closed(s, 1e-6).unwrap().len(), 8);
    }

    #[test]
    fn caps_cover_cut_area_inside_box() {
        let m = cube([0.0, 0.0, 0.0], [4.0, 2.0, 3.0]);
        let s = Scene::new(vec![m]);
        // the box cuts the body from above (z = 1) and from the right (x = 3)
        let b = Aabb {
            min: Vec3::new(-1.0, -1.0, -1.0),
            max: Vec3::new(3.0, 5.0, 1.0),
        };
        let caps = scene_caps(
            &s,
            &b,
            |_| Some(crate::style::cap_tag([1, 2, 3])),
            &Budget::default(),
        )
        .unwrap();
        assert_eq!(caps.len(), 1);
        let (src, c) = &caps[0];
        assert_eq!((*src, c.element), (0, 7));
        assert!(crate::style::is_cap(c.tag));
        // top: 3×2 (clipped at x ≤ 3), right face: 2×1 (clipped at z ≤ 1)
        assert!((tri_area(c) - 8.0).abs() < 1e-4, "{}", tri_area(c));
        for p in &c.positions {
            assert!(p[0] <= 3.0 + 1e-5 && p[2] <= 1.0 + 1e-5);
        }
        let far = Aabb {
            min: Vec3::splat(-10.0),
            max: Vec3::splat(10.0),
        };
        assert!(scene_caps(&s, &far, |_| Some(1), &Budget::default())
            .unwrap()
            .is_empty());
        assert!(scene_caps(&s, &b, |_| None, &Budget::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn instanced_bodies_are_capped_in_world() {
        use crate::chunks::{Inst, Instanced};
        use glam::Mat4;
        let g = Instanced::new(
            1,
            std::sync::Arc::new(cube([0.0; 3], [1.0, 1.0, 2.0])),
            vec![
                Inst {
                    m: Mat4::from_translation(Vec3::new(5.0, 0.0, 0.0)),
                    element: 40,
                },
                Inst {
                    // mirror along X
                    m: Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)),
                    element: 41,
                },
            ],
        );
        let s = Scene::new(vec![]).with_instanced(vec![g]);
        let b = Aabb {
            min: Vec3::splat(-10.0),
            max: Vec3::new(10.0, 10.0, 1.0),
        };
        let caps = scene_caps(&s, &b, |_| Some(1), &Budget::default()).unwrap();
        assert_eq!(caps.len(), 2);
        assert!(caps
            .iter()
            .all(|(src, c)| *src == 0 && (tri_area(c) - 1.0).abs() < 1e-5));
        let mut el: Vec<u32> = caps.iter().map(|c| c.1.element).collect();
        el.sort();
        assert_eq!(el, vec![40, 41]);
        let xs = caps.iter().flat_map(|c| c.1.positions.iter().map(|p| p[0]));
        assert!(
            xs.into_iter().all(|x| x <= 1e-5 || x >= 5.0 - 1e-5),
            "instances in world"
        );
    }

    #[test]
    fn budget_cancels() {
        let s = Scene::new(vec![cube([0.0; 3], [4.0, 2.0, 3.0])]);
        let b = Aabb {
            min: Vec3::splat(-1.0),
            max: Vec3::new(5.0, 5.0, 1.0),
        };
        let stop = AtomicBool::new(true);
        let cancel = Budget {
            deadline: None,
            cancel: Some(&stop),
        };
        assert!(matches!(
            scene_caps(&s, &b, |_| Some(1), &cancel),
            Err(Over)
        ));
        let late = Budget {
            deadline: Some(Instant::now()),
            cancel: None,
        };
        assert!(matches!(scene_caps(&s, &b, |_| Some(1), &late), Err(Over)));
    }

    #[test]
    fn many_vertex_section_is_fast_and_exact() {
        // a cylinder with 20,000 facets: the section is a 20,000-gon, strips O(n log n)
        let n = 20_000u32;
        let r = 2.0f64;
        let mut positions = vec![];
        for k in 0..n {
            let a = k as f64 / n as f64 * std::f64::consts::TAU;
            let (x, y) = ((r * a.cos()) as f32, (r * a.sin()) as f32);
            positions.push([x, y, 0.0]);
            positions.push([x, y, 3.0]);
        }
        let mut triangles = vec![];
        for k in 0..n {
            let (a0, a1) = (2 * k, 2 * k + 1);
            let (b0, b1) = (2 * ((k + 1) % n), 2 * ((k + 1) % n) + 1);
            triangles.push([a0, b0, b1]);
            triangles.push([a0, b1, a1]);
        }
        let m = SceneMesh {
            element: 1,
            tag: 0,
            positions,
            triangles,
        };
        let t0 = Instant::now();
        let s = closed(section_segments(&m, 2, 1.0), 1e-9).unwrap();
        assert_eq!(s.len(), 2 * n as usize);
        let a = filled(&s, BIG.0, BIG.1);
        let exact = 0.5 * n as f64 * r * r * (std::f64::consts::TAU / n as f64).sin();
        assert!((a - exact).abs() < 1e-3, "{a} {exact}");
        assert!(t0.elapsed().as_secs_f32() < 5.0);
    }
}
