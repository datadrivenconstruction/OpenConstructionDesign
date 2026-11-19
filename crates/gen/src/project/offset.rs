// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Offsets and facade bands of plan regions with holes. The mitered inset is the region minus
//! a two-sided strip along every edge and a wedge at every reflex corner, so splits, holes and
//! vanishing parts come out of one boolean step. The band behind each facade edge is that
//! edge's straight-skeleton face (outer ring) or its mitered quad (convex courtyard), cut at
//! the band depth; areas along the facade are exact for any shape.

use super::footprint::{clean, signed_area, Region, P};
use ddc_geom::roof::Footprint;
use ddc_geom::{v2, V2};

fn to_v(r: &[P]) -> Vec<V2> {
    r.iter().map(|p| v2(p.0, p.1)).collect()
}

fn to_p(r: &[V2]) -> Vec<P> {
    r.iter().map(|p| (p.x, p.y)).collect()
}

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}

fn dot(a: P, b: P) -> f64 {
    a.0 * b.0 + a.1 * b.1
}

fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

fn rings(r: &Region) -> impl Iterator<Item = &Vec<P>> {
    std::iter::once(&r.outer).chain(r.holes.iter())
}

/// Strips along every edge (both sides, `d` each) and the wedges at reflex corners.
fn sausages(r: &Region, d: f64) -> Vec<Vec<V2>> {
    let mut out = vec![];
    for ring in rings(r) {
        let n = ring.len();
        let norm = |i: usize| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            let e = sub(b, a);
            let l = e.0.hypot(e.1);
            (-e.1 / l, e.0 / l)
        };
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            let m = norm(i);
            let q = |p: P, s: f64| v2(p.0 + m.0 * d * s, p.1 + m.1 * d * s);
            out.push(vec![q(a, -1.0), q(b, -1.0), q(b, 1.0), q(a, 1.0)]);
            // the corner at b: reflex when the ring turns right (region on the left)
            let (e0, e1) = (sub(b, a), sub(ring[(i + 2) % n], b));
            if cross(e0, e1) < -1e-12 * e0.0.hypot(e0.1) * e1.0.hypot(e1.1) {
                let mn = norm((i + 1) % n);
                let k = d / (1.0 + dot(m, mn));
                let mi = v2(b.0 + (m.0 + mn.0) * k, b.1 + (m.1 + mn.1) * k);
                out.push(vec![v2(b.0, b.1), q(b, 1.0), mi, v2(b.0 + mn.0 * d, b.1 + mn.1 * d)]);
            }
        }
    }
    out
}

fn host(r: &Region) -> Vec<Vec<V2>> {
    rings(r).map(|x| to_v(x)).collect()
}

fn pieces(v: Option<Vec<(Vec<V2>, Vec<Vec<V2>>)>>) -> Vec<Region> {
    v.unwrap_or_default()
        .into_iter()
        .filter_map(|(o, hs)| {
            let outer = clean(&to_p(&o), true)?;
            let holes = hs.iter().filter_map(|h| clean(&to_p(h), false)).collect();
            (signed_area(&outer) > 1e-4).then_some(Region { outer, holes })
        })
        .collect()
}

/// The region shrunk by `d` with mitered corners: no piece when it vanishes, several when a
/// waist closes.
pub fn inset(r: &Region, d: f64) -> Vec<Region> {
    if d <= 0.0 {
        return vec![r.clone()];
    }
    pieces(ddc_geom::poly::subtract(&host(r), &sausages(r, d)))
}

/// The region grown by `d` with mitered corners; courtyards shrink and may close.
pub fn outset(r: &Region, d: f64) -> Region {
    let ((x0, y0), (x1, y1)) = super::footprint::bbox(&r.outer);
    let m = 4.0 * d + 10.0 + (x1 - x0).max(y1 - y0);
    let mut hole = r.outer.clone();
    hole.reverse();
    let around = Region {
        outer: vec![(x0 - m, y0 - m), (x1 + m, y0 - m), (x1 + m, y1 + m), (x0 - m, y1 + m)],
        holes: vec![hole],
    };
    let grown = inset(&around, d)
        .into_iter()
        .flat_map(|p| p.holes)
        .max_by(|a, b| signed_area(a).abs().total_cmp(&signed_area(b).abs()));
    let outer = grown.and_then(|h| clean(&h, true)).unwrap_or_else(|| r.outer.clone());
    let holes = r
        .holes
        .iter()
        .flat_map(|h| {
            let mut c = h.clone();
            c.reverse();
            inset(&Region { outer: c, holes: vec![] }, d)
        })
        .filter_map(|p| clean(&p.outer, false))
        .collect();
    Region { outer, holes }
}

/// One facade edge and the part of the plan it lights: local `s` along the edge from `a`, `h`
/// inward from it.
#[derive(Clone, Debug)]
pub struct EdgeBand {
    pub a: P,
    pub b: P,
    pub u: P,
    pub n: P,
    pub len: f64,
    /// The edge's skeleton face (outer ring) or mitered quad (courtyard), in plan.
    pub face: Vec<P>,
}

impl EdgeBand {
    pub fn at(&self, s: f64, h: f64) -> P {
        (self.a.0 + self.u.0 * s + self.n.0 * h, self.a.1 + self.u.1 * s + self.n.1 * h)
    }

    pub fn local(&self, p: P) -> (f64, f64) {
        let d = sub(p, self.a);
        (dot(d, self.u), dot(d, self.n))
    }

    /// The face between depths `h0` and `h1`, between `s0` and `s1` along the edge.
    pub fn piece(&self, h0: f64, h1: f64, s0: f64, s1: f64) -> Vec<P> {
        let (u, n) = (v2(self.u.0, self.u.1), v2(self.n.0, self.n.1));
        let a = v2(self.a.0, self.a.1);
        let mut f = to_v(&self.face);
        f = ddc_geom::poly::clip_half(&f, n, n.dot(a) + h1);
        f = ddc_geom::poly::clip_half(&f, -n, -(n.dot(a) + h0));
        f = ddc_geom::poly::clip_half(&f, u, u.dot(a) + s1);
        f = ddc_geom::poly::clip_half(&f, -u, -(u.dot(a) + s0));
        to_p(&f)
    }

    pub fn area(&self, h0: f64, h1: f64, s0: f64, s1: f64) -> f64 {
        let p = self.piece(h0, h1, s0, s1);
        if p.len() < 3 {
            0.0
        } else {
            signed_area(&p).abs()
        }
    }

    /// Spans of `s` where the face reaches from depth `h0` to depth `h1` without a break, so a
    /// cut there runs across the whole band.
    pub fn full(&self, h0: f64, h1: f64) -> Vec<(f64, f64)> {
        // the face is monotone along its edge: the cut line at depth h1 inside the face
        let lo = self.at(-1e3, h1 - 1e-7);
        let hi = self.at(self.len + 1e3, h1 - 1e-7);
        let ring = vec![to_v(&self.face)];
        let span = 2e3 + self.len;
        let deep: Vec<(f64, f64)> = ddc_geom::poly::inside_intervals(v2(lo.0, lo.1), v2(hi.0, hi.1), &ring)
            .into_iter()
            .map(|(t0, t1)| (t0 * span - 1e3, t1 * span - 1e3))
            .collect();
        // and at the facade side too, for a band that starts below the facade
        let lo = self.at(-1e3, h0 + 1e-7);
        let hi = self.at(self.len + 1e3, h0 + 1e-7);
        let near: Vec<(f64, f64)> = ddc_geom::poly::inside_intervals(v2(lo.0, lo.1), v2(hi.0, hi.1), &ring)
            .into_iter()
            .map(|(t0, t1)| (t0 * span - 1e3, t1 * span - 1e3))
            .collect();
        let mut out = vec![];
        for d in &deep {
            for f in &near {
                let (x0, x1) = (d.0.max(f.0).max(0.0), d.1.min(f.1).min(self.len));
                if x1 > x0 + 1e-9 {
                    out.push((x0, x1));
                }
            }
        }
        out
    }
}

/// The band of one ring: its edges in order and where each starts along the loop.
#[derive(Clone, Debug)]
pub struct RingBand {
    pub edges: Vec<EdgeBand>,
    pub start: Vec<f64>,
    pub perimeter: f64,
    pub hole: bool,
}

impl RingBand {
    /// Edge and local `s` of loop position `t`.
    pub fn locate(&self, t: f64) -> (usize, f64) {
        let t = t.rem_euclid(self.perimeter);
        let i = self.start.partition_point(|s| *s <= t).saturating_sub(1);
        (i, (t - self.start[i]).clamp(0.0, self.edges[i].len))
    }

    /// Band area between depths `h0..h1` from loop position `t0` to `t1` (`t1` may pass the
    /// loop end once).
    pub fn area(&self, h0: f64, h1: f64, t0: f64, t1: f64) -> f64 {
        let mut a = 0.0;
        let mut t = t0;
        while t < t1 - 1e-12 {
            let (i, s) = self.locate(t);
            let e = &self.edges[i];
            let room = e.len - s;
            let take = room.min(t1 - t);
            // the first and last piece of each edge keeps its skeleton corner
            let s0 = if s <= 1e-12 { -1e4 } else { s };
            let s1 = if s + take >= e.len - 1e-12 { e.len + 1e4 } else { s + take };
            a += e.area(h0, h1, s0, s1);
            t += take.max(1e-9);
        }
        a
    }
}

fn frame(a: P, b: P) -> (P, P, f64) {
    let e = sub(b, a);
    let l = e.0.hypot(e.1);
    let u = (e.0 / l, e.1 / l);
    (u, (-u.1, u.0), l)
}

/// Skeleton faces of a counter-clockwise ring, one per edge, from the roof skeleton at 45
/// degrees (height = distance to the edge line).
fn skeleton_faces(ring: &[P]) -> Result<Vec<Vec<P>>, String> {
    let fp = Footprint {
        ring: to_v(ring),
        slopes: vec![Some(45.0); ring.len()],
        base_z: 0.0,
        overhang: 0.0,
        mansard: None,
    };
    let roof = fp.build().map_err(|e| format!("the plate's skeleton failed: {e:?}"))?;
    let n = ring.len();
    let mut faces: Vec<Vec<P>> = vec![vec![]; n];
    for f in &roof.faces {
        if !f.sloped {
            continue;
        }
        let pts: Vec<P> = f.pts.iter().map(|p| (p.x, p.y)).collect();
        // the face's own edge: the ring edge both of whose ends are on it at height zero
        let on = |q: P| f.pts.iter().any(|p| p.z.abs() < 1e-6 && (p.x - q.0).hypot(p.y - q.1) < 1e-6);
        let i = (0..n).find(|&i| on(ring[i]) && on(ring[(i + 1) % n])).unwrap_or(f.edge);
        if i < n && faces[i].is_empty() {
            faces[i] = pts;
        } else if i < n {
            return Err(format!("edge {i} has two skeleton faces"));
        }
    }
    if let Some(i) = faces.iter().position(Vec::is_empty) {
        return Err(format!("edge {i} has no skeleton face"));
    }
    Ok(faces)
}

/// Mitered quads of a convex courtyard ring (clockwise: the region on its left), `depth` deep.
fn hole_faces(ring: &[P], depth: f64) -> Vec<Vec<P>> {
    let n = ring.len();
    let nrm = |i: usize| frame(ring[i], ring[(i + 1) % n]).1;
    let miter = |i: usize| {
        let (m0, m1) = (nrm((i + n - 1) % n), nrm(i));
        let k = depth / (1.0 + dot(m0, m1));
        (ring[i].0 + (m0.0 + m1.0) * k, ring[i].1 + (m0.1 + m1.1) * k)
    };
    (0..n).map(|i| vec![ring[i], ring[(i + 1) % n], miter((i + 1) % n), miter(i)]).collect()
}

/// The facade bands of a region: the outer ring and every courtyard.
pub fn bands(r: &Region, depth: f64) -> Result<Vec<RingBand>, String> {
    let mut out = vec![];
    for (k, ring) in rings(r).enumerate() {
        let faces = if k == 0 { skeleton_faces(ring)? } else { hole_faces(ring, depth) };
        let n = ring.len();
        let mut edges = vec![];
        let mut start = vec![];
        let mut t = 0.0;
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            let (u, nn, len) = frame(a, b);
            start.push(t);
            t += len;
            edges.push(EdgeBand {
                a,
                b,
                u,
                n: nn,
                len,
                face: faces[i].clone(),
            });
        }
        out.push(RingBand {
            edges,
            start,
            perimeter: t,
            hole: k > 0,
        });
    }
    Ok(out)
}

/// Distance from `p` to the nearest edge of the region.
pub fn depth_of(r: &Region, p: P) -> f64 {
    rings(r)
        .flat_map(|ring| {
            let n = ring.len();
            (0..n).map(move |i| crate::check::dist_to_seg(p, ring[i], ring[(i + 1) % n]))
        })
        .fold(f64::MAX, f64::min)
}

pub fn contains(r: &Region, p: P) -> bool {
    crate::check::inside(&r.outer, p) && r.holes.iter().all(|h| !crate::check::inside(h, p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::footprint::region_area;

    fn rect(w: f64, d: f64) -> Region {
        Region {
            outer: vec![(0.0, 0.0), (w, 0.0), (w, d), (0.0, d)],
            holes: vec![],
        }
    }

    #[test]
    fn inset_of_a_rectangle_is_exact() {
        let r = inset(&rect(20.0, 10.0), 2.0);
        assert_eq!(r.len(), 1);
        assert!((region_area(&r[0]) - 16.0 * 6.0).abs() < 1e-9);
        assert!(inset(&rect(20.0, 10.0), 5.0).is_empty(), "the bar closes at half its depth");
    }

    #[test]
    fn inset_of_a_concave_l_keeps_a_square_inner_corner() {
        let l = Region {
            outer: vec![(0.0, 0.0), (20.0, 0.0), (20.0, 8.0), (8.0, 8.0), (8.0, 20.0), (0.0, 20.0)],
            holes: vec![],
        };
        let r = inset(&l, 2.0);
        assert_eq!(r.len(), 1);
        // two 4 x 16 arms sharing a 4 x 4 corner
        assert!((region_area(&r[0]) - (16.0 * 4.0 * 2.0 - 16.0)).abs() < 1e-9, "{}", region_area(&r[0]));
        assert!(r[0].outer.iter().any(|p| (p.0 - 6.0).abs() < 1e-9 && (p.1 - 6.0).abs() < 1e-9));
    }

    #[test]
    fn a_waist_splits_and_a_courtyard_grows() {
        // two 10 x 10 squares joined by a 2 m neck
        let db = Region {
            outer: vec![
                (0.0, 0.0),
                (10.0, 0.0),
                (10.0, 4.0),
                (14.0, 4.0),
                (14.0, 0.0),
                (24.0, 0.0),
                (24.0, 10.0),
                (14.0, 10.0),
                (14.0, 6.0),
                (10.0, 6.0),
                (10.0, 10.0),
                (0.0, 10.0),
            ],
            holes: vec![],
        };
        assert_eq!(inset(&db, 1.5).len(), 2);
        let c = Region {
            outer: vec![(0.0, 0.0), (30.0, 0.0), (30.0, 30.0), (0.0, 30.0)],
            holes: vec![vec![(10.0, 10.0), (10.0, 20.0), (20.0, 20.0), (20.0, 10.0)]],
        };
        let r = inset(&c, 2.0);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].holes.len(), 1);
        assert!((region_area(&r[0]) - (26.0 * 26.0 - 14.0 * 14.0)).abs() < 1e-9);
        let g = outset(&rect(10.0, 10.0), 1.0);
        assert!((region_area(&g) - 144.0).abs() < 1e-9);
    }

    #[test]
    fn degenerate_insets_are_empty_not_wrong() {
        let tri = Region {
            outer: vec![(0.0, 0.0), (10.0, 0.0), (5.0, 0.2)],
            holes: vec![],
        };
        assert!(inset(&tri, 0.5).is_empty());
        assert_eq!(inset(&tri, 0.0).len(), 1);
    }

    #[test]
    fn band_areas_add_up_for_convex_and_concave_rings() {
        let shapes = [
            rect(30.0, 14.0),
            Region {
                outer: vec![(0.0, 0.0), (30.0, 0.0), (30.0, 14.0), (14.0, 14.0), (14.0, 30.0), (0.0, 30.0)],
                holes: vec![],
            },
            Region {
                outer: vec![(0.0, 0.0), (40.0, 0.0), (20.0, 30.0)],
                holes: vec![],
            },
        ];
        for (k, r) in shapes.iter().enumerate() {
            let d = 5.0;
            let b = bands(r, d).unwrap();
            let rb = &b[0];
            let total = rb.area(0.0, d, 0.0, rb.perimeter);
            let core: f64 = inset(r, d).iter().map(region_area).sum();
            assert!((total + core - region_area(r)).abs() < 1e-6, "shape {k}: {total} + {core}");
            // splitting the loop anywhere keeps the sum
            let t = rb.perimeter * 0.37;
            let parts = rb.area(0.0, d, 0.0, t) + rb.area(0.0, d, t, rb.perimeter);
            assert!((parts - total).abs() < 1e-6, "shape {k}");
            // the long edge of the bar is clean in its middle
            if k == 0 {
                let f = rb.edges[0].full(0.0, d);
                assert!(f.len() == 1 && (f[0].0 - 5.0).abs() < 1e-6 && (f[0].1 - 25.0).abs() < 1e-6, "{f:?}");
                // the 14 m end is a hip: its face peaks at 7 m, so it is 5 m deep only on 5..9
                let g = rb.edges[1].full(0.0, d);
                assert!(g.len() == 1 && (g[0].0 - 5.0).abs() < 1e-6 && (g[0].1 - 9.0).abs() < 1e-6, "{g:?}");
            }
        }
    }

    #[test]
    fn courtyard_bands_close_the_ring() {
        let c = Region {
            outer: vec![(0.0, 0.0), (40.0, 0.0), (40.0, 40.0), (0.0, 40.0)],
            holes: vec![vec![(14.0, 14.0), (14.0, 26.0), (26.0, 26.0), (26.0, 14.0)]],
        };
        let b = bands(&c, 5.0).unwrap();
        let hole = &b[1];
        let a = hole.area(0.0, 5.0, 0.0, hole.perimeter);
        assert!((a - (22.0 * 22.0 - 144.0)).abs() < 1e-6, "{a}");
    }
}
