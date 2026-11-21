// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Component body construction: mesh (visible bodies at the level of detail), exact volume
//! (extrusion A·h, revolve via Pappus's theorem, transition via the prismatoid formula, sweep via the mesh
//! corrected by the exact profile area), bounding box, plan lines.

use super::expr::{Dim, ANGLE, LENGTH, NUMBER};
use super::plant;
use super::{
    Axis, Expr, Family, FamilyError, Hosting, Profile, Resolved, Shape, SymShape, Vis,
    COARSE, FINE, MEDIUM, MODEL, PLAN,
};
use crate::math::{v2, v3, V2, V3};
use crate::mesh::Mesh;
use crate::poly;
use std::f64::consts::TAU;

/// Level of detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lod {
    Coarse,
    Medium,
    Fine,
}

impl Lod {
    pub fn bit(self) -> Vis {
        match self {
            Lod::Coarse => COARSE,
            Lod::Medium => MEDIUM,
            Lod::Fine => FINE,
        }
    }
    /// Segments per full turn.
    pub fn segments(self) -> usize {
        match self {
            Lod::Coarse => 12,
            Lod::Medium => 24,
            Lod::Fine => 48,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub name: String,
    pub material: String,
    /// Exact body volume (for a cut, the subtracted volume).
    pub volume: f64,
    pub void: bool,
    /// Visible in 3D at this level of detail.
    pub visible: bool,
}

#[derive(Clone, Debug)]
pub struct Built {
    /// Bodies visible in 3D (without cuts); `face_of` is the body index in `parts`.
    pub mesh: Mesh,
    /// Bodies minus cuts, m³; independent of visibility.
    pub volume: f64,
    /// Surface area of the bodies (from the mesh), m².
    pub surface: f64,
    /// Bounding box of the bodies in component coordinates.
    pub bbox: [V3; 2],
    pub parts: Vec<Part>,
    /// Plan lines: symbols and outlines of bodies with `plan` visibility.
    pub plan: Vec<(V2, V2)>,
}

fn gerr(solid: &str, msg: impl Into<String>) -> FamilyError {
    FamilyError::Geometry {
        solid: solid.into(),
        msg: msg.into(),
    }
}

fn num(e: &Expr, r: &Resolved, d: Dim, what: &str) -> Result<f64, FamilyError> {
    let v = e
        .eval(&|s| r.nums.get(s).copied())
        .map_err(|m| gerr(what, m))?;
    if !super::expr::zero_fits(v, d) {
        return Err(gerr(what, format!("expected {d}, got {}", v.d)));
    }
    Ok(v.v)
}

struct Prof {
    pts: Vec<V2>,
    area: f64,
    centroid: V2,
    /// (center, radius) for a circle.
    circle: Option<(V2, f64)>,
}

fn cross(a: V2, b: V2, c: V2) -> f64 {
    (b - a).cross(c - a)
}

fn simple(p: &[V2]) -> bool {
    let n = p.len();
    for i in 0..n {
        let (a, b) = (p[i], p[(i + 1) % n]);
        for j in i + 2..n {
            if i == 0 && j == n - 1 {
                continue;
            }
            let (c, d) = (p[j], p[(j + 1) % n]);
            let (d1, d2) = (cross(a, b, c), cross(a, b, d));
            let (d3, d4) = (cross(c, d, a), cross(c, d, b));
            if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
                return false;
            }
        }
    }
    true
}

fn prof(p: &Profile, r: &Resolved, lod: Lod, what: &str) -> Result<Prof, FamilyError> {
    let l = |e: &Expr| num(e, r, LENGTH, what);
    match p {
        Profile::Rect(a) => {
            let (x0, y0, x1, y1) = (l(&a[0])?, l(&a[1])?, l(&a[2])?, l(&a[3])?);
            let (xa, xb, ya, yb) = (x0.min(x1), x0.max(x1), y0.min(y1), y0.max(y1));
            if xb - xa <= 1e-9 || yb - ya <= 1e-9 {
                return Err(gerr(what, "rectangle has no area"));
            }
            Ok(Prof {
                pts: vec![v2(xa, ya), v2(xb, ya), v2(xb, yb), v2(xa, yb)],
                area: (xb - xa) * (yb - ya),
                centroid: v2((xa + xb) / 2.0, (ya + yb) / 2.0),
                circle: None,
            })
        }
        Profile::Circle(a) => {
            let (c, rad) = (v2(l(&a[0])?, l(&a[1])?), l(&a[2])?);
            if rad <= 1e-9 {
                return Err(gerr(what, "circle radius must be positive"));
            }
            let n = lod.segments();
            let pts = (0..n)
                .map(|k| {
                    let t = TAU * k as f64 / n as f64;
                    v2(c.x + rad * t.cos(), c.y + rad * t.sin())
                })
                .collect();
            Ok(Prof {
                pts,
                area: std::f64::consts::PI * rad * rad,
                centroid: c,
                circle: Some((c, rad)),
            })
        }
        Profile::Poly(v) => {
            let mut pts = vec![];
            for q in v {
                pts.push(v2(l(&q[0])?, l(&q[1])?));
            }
            pts.dedup_by(|a, b| (*a - *b).len() < 1e-12);
            if pts.len() > 2 && (pts[0] - pts[pts.len() - 1]).len() < 1e-12 {
                pts.pop();
            }
            // Crossing first: a bow tie has zero signed area but is a crossing, not a flat outline.
            if pts.len() >= 3 && !simple(&pts) {
                return Err(gerr(what, "profile crosses itself"));
            }
            let a = poly::area(&pts);
            if pts.len() < 3 || a.abs() < 1e-12 {
                return Err(gerr(what, "profile has no area"));
            }
            if a < 0.0 {
                pts.reverse();
            }
            let centroid = poly::centroid(&pts);
            Ok(Prof {
                pts,
                area: a.abs(),
                centroid,
                circle: None,
            })
        }
    }
}

/// Mesh accumulator for a single body.
#[derive(Default)]
struct Mb {
    pos: Vec<V3>,
    tris: Vec<[u32; 3]>,
}

impl Mb {
    fn v(&mut self, p: V3) -> u32 {
        self.pos.push(p);
        self.pos.len() as u32 - 1
    }
    fn t(&mut self, a: u32, b: u32, c: u32) {
        let (pa, pb, pc) = (
            self.pos[a as usize],
            self.pos[b as usize],
            self.pos[c as usize],
        );
        if (pb - pa).cross(pc - pa).len() > 1e-18 {
            self.tris.push([a, b, c]);
        }
    }
    fn quad(&mut self, a: u32, b: u32, c: u32, d: u32) {
        self.t(a, b, c);
        self.t(a, c, d);
    }
    /// Cap over the profile: `ring` holds vertex indices in profile order (counter-clockwise).
    fn cap(&mut self, pts: &[V2], ring: &[u32], flip: bool) {
        for t in poly::triangulate(pts) {
            let [a, b, c] = t.map(|i| ring[i]);
            if flip {
                self.t(a, c, b);
            } else {
                self.t(a, b, c);
            }
        }
    }
    fn flip(&mut self) {
        for t in &mut self.tris {
            t.swap(1, 2);
        }
    }
    fn volume(&self) -> f64 {
        self.tris
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| self.pos[i as usize]);
                a.dot(b.cross(c))
            })
            .sum::<f64>()
            / 6.0
    }
    fn area(&self) -> f64 {
        self.tris
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| self.pos[i as usize]);
                (b - a).cross(c - a).len() * 0.5
            })
            .sum()
    }
}

/// Profile prism between two rings (side faces from vertex pairs).
fn sides(m: &mut Mb, lo: &[u32], hi: &[u32]) {
    let n = lo.len();
    for k in 0..n {
        let j = (k + 1) % n;
        m.quad(lo[k], lo[j], hi[j], hi[k]);
    }
}

fn ring_edges(pts: &[V2], out: &mut Vec<(V2, V2)>) {
    for k in 0..pts.len() {
        out.push((pts[k], pts[(k + 1) % pts.len()]));
    }
}

fn bbox_rect(m: &Mb, out: &mut Vec<(V2, V2)>) {
    let (mut lo, mut hi) = (v2(f64::MAX, f64::MAX), v2(f64::MIN, f64::MIN));
    for p in &m.pos {
        lo = v2(lo.x.min(p.x), lo.y.min(p.y));
        hi = v2(hi.x.max(p.x), hi.y.max(p.y));
    }
    if lo.x <= hi.x {
        ring_edges(&[lo, v2(hi.x, lo.y), hi, v2(lo.x, hi.y)], out);
    }
}

/// Rotation taking unit `a` to unit `b` (Rodrigues), applied to `v`.
fn rotate_to(a: V3, b: V3, v: V3) -> V3 {
    let k = a.cross(b);
    let s = k.len();
    let c = a.dot(b);
    if s < 1e-12 {
        return v;
    }
    let k = k * (1.0 / s);
    v * c + k.cross(v) * s + k * (k.dot(v) * (1.0 - c))
}

/// Primitive body: (mesh, exact volume, plan lines).
/// The plant spec of a `plant` solid's expressions at the resolved values.
fn plant_spec(form: &Expr, dims: &[Expr; 4], leaf: &Expr, seed: &Expr, r: &Resolved, name: &str) -> Result<plant::Spec, FamilyError> {
    use plant::Form;
    let n = |e: &Expr| num(e, r, NUMBER, name);
    let l = |e: &Expr| num(e, r, LENGTH, name);
    let code = n(form)?;
    let form = Form::from_code(code).ok_or_else(|| gerr(name, format!("unknown plant form {code}")))?;
    Ok(plant::Spec {
        form,
        height: l(&dims[0])?,
        crown: l(&dims[1])?,
        trunk: l(&dims[2])?,
        clear: l(&dims[3])?,
        leaf: n(leaf)?,
        seed: n(seed)?.round() as i64 as u64,
    })
}

/// The plant a family grows at the resolved values: its first `plant` solid's spec, for the
/// planting properties of an export. None for a family without one.
pub fn plant_of(f: &Family, r: &Resolved) -> Option<plant::Spec> {
    f.solids.iter().find_map(|s| match &s.shape {
        Shape::Plant { form, dims, leaf, seed, .. } => plant_spec(form, dims, leaf, seed, r, &s.name).ok(),
        _ => None,
    })
}

/// Index in `Built::parts` of the first solid of a plant family that grows `organ`.
pub fn plant_part(f: &Family, r: &Resolved, organ: plant::Organ) -> Option<usize> {
    f.solids
        .iter()
        .filter(|s| !r.hidden.contains(&s.name))
        .position(|s| matches!(s.shape, Shape::Plant { organ: o, .. } if o == organ))
}

fn shape(
    sh: &Shape,
    r: &Resolved,
    lod: Lod,
    name: &str,
) -> Result<(Mb, f64, Vec<(V2, V2)>), FamilyError> {
    let l = |e: &Expr| num(e, r, LENGTH, name);
    let mut m = Mb::default();
    let mut plan = vec![];
    let vol = match sh {
        Shape::Extrude {
            profile,
            dir,
            range,
        } => {
            let p = prof(profile, r, lod, name)?;
            let (a, b) = (l(&range[0])?, l(&range[1])?);
            let (a, b) = (a.min(b), a.max(b));
            if b - a <= 1e-9 {
                return Err(gerr(name, "extrusion has no depth"));
            }
            let map = |q: V2, w: f64| match dir {
                Axis::Z => v3(q.x, q.y, w),
                Axis::X => v3(w, q.x, q.y),
                Axis::Y => v3(q.x, w, q.y),
            };
            let lo: Vec<u32> = p.pts.iter().map(|q| m.v(map(*q, a))).collect();
            let hi: Vec<u32> = p.pts.iter().map(|q| m.v(map(*q, b))).collect();
            m.cap(&p.pts, &lo, true);
            m.cap(&p.pts, &hi, false);
            sides(&mut m, &lo, &hi);
            if *dir == Axis::Y {
                m.flip(); // (x, w, y) is a reflection
            }
            if *dir == Axis::Z {
                ring_edges(&p.pts, &mut plan);
            } else {
                bbox_rect(&m, &mut plan);
            }
            p.area * (b - a)
        }
        Shape::Revolve {
            profile,
            axis,
            angle,
        } => {
            let p = prof(profile, r, lod, name)?;
            let (ax, ay) = (l(&axis[0])?, l(&axis[1])?);
            let th = num(angle, r, ANGLE, name)?;
            if th <= 1e-9 || th > TAU + 1e-9 {
                return Err(gerr(name, "revolve angle must be in (0, 360deg]"));
            }
            if p.pts.iter().any(|q| q.x < -1e-9) {
                return Err(gerr(name, "revolve profile crosses the axis (r < 0)"));
            }
            let full = th > TAU - 1e-9;
            let n = ((lod.segments() as f64 * th / TAU).ceil() as usize).max(3);
            let rings = if full { n } else { n + 1 };
            let at = |q: V2, k: usize| {
                let f = th * k as f64 / n as f64;
                v3(ax + q.x * f.cos(), ay + q.x * f.sin(), q.y)
            };
            // a point on the axis is one vertex for every ring, so the mesh closes there
            let mut ids: Vec<Vec<u32>> = Vec::with_capacity(rings);
            for k in 0..rings {
                let ring = p
                    .pts
                    .iter()
                    .enumerate()
                    .map(|(i, q)| if k > 0 && q.x.abs() <= 1e-9 { ids[0][i] } else { m.v(at(*q, k)) })
                    .collect();
                ids.push(ring);
            }
            let np = p.pts.len();
            for k in 0..n {
                let (r0, r1) = (&ids[k], &ids[(k + 1) % rings]);
                for i in 0..np {
                    let j = (i + 1) % np;
                    // A=(i,φk) D=(i,φk+1) C=(j,φk+1) B=(j,φk): outward for a counter-clockwise profile
                    m.t(r0[i], r1[i], r1[j]);
                    m.t(r0[i], r1[j], r0[j]);
                }
            }
            if !full {
                let (first, last) = (ids[0].clone(), ids[rings - 1].clone());
                m.cap(&p.pts, &first, false);
                m.cap(&p.pts, &last, true);
                bbox_rect(&m, &mut plan);
            } else {
                let rmax = p.pts.iter().map(|q| q.x).fold(0.0, f64::max);
                let rmax = p.circle.map_or(rmax, |(c, rad)| c.x + rad);
                let n = lod.segments();
                let c: Vec<V2> = (0..n)
                    .map(|k| {
                        let t = TAU * k as f64 / n as f64;
                        v2(ax + rmax * t.cos(), ay + rmax * t.sin())
                    })
                    .collect();
                ring_edges(&c, &mut plan);
            }
            th * p.area * p.centroid.x
        }
        Shape::Sweep { profile, path } => {
            let p = prof(profile, r, lod, name)?;
            let mut pts = vec![];
            for q in path {
                pts.push(v3(l(&q[0])?, l(&q[1])?, l(&q[2])?));
            }
            let mut dirs = vec![];
            for w in pts.windows(2) {
                let d = w[1] - w[0];
                if d.len() < 1e-9 {
                    return Err(gerr(name, "path has a zero-length segment"));
                }
                dirs.push(d.normalized());
            }
            let d0 = dirs[0];
            let up = v3(0.0, 0.0, 1.0);
            let mut s = up.cross(d0);
            if s.len() < 1e-9 {
                s = v3(0.0, -1.0, 0.0).cross(d0) * -1.0;
            }
            let mut s = s.normalized();
            let mut w = d0.cross(s);
            let mut rings: Vec<Vec<u32>> = vec![];
            for (i, &v) in pts.iter().enumerate() {
                let din = if i == 0 { dirs[0] } else { dirs[i - 1] };
                let dout = dirs.get(i).copied().unwrap_or(din);
                let t = din + dout;
                if t.len() < 1e-9 {
                    return Err(gerr(name, "path folds back on itself"));
                }
                let t = t.normalized();
                let ring: Vec<u32> = p
                    .pts
                    .iter()
                    .map(|q| {
                        let pp = v + s * q.x + w * q.y;
                        // projection along the incoming direction onto the joint plane
                        let k = (pp - v).dot(t) / din.dot(t);
                        m.v(pp - din * k)
                    })
                    .collect();
                rings.push(ring);
                // ring i lies in the frame of incoming segment i−1: carry it over to segment i
                if i >= 1 && i < dirs.len() {
                    s = rotate_to(dirs[i - 1], dirs[i], s);
                    w = rotate_to(dirs[i - 1], dirs[i], w);
                }
            }
            let (first, last) = (rings[0].clone(), rings[rings.len() - 1].clone());
            m.cap(&p.pts, &first, true);
            m.cap(&p.pts, &last, false);
            for k in 0..rings.len() - 1 {
                let (a, b) = (rings[k].clone(), rings[k + 1].clone());
                sides(&mut m, &a, &b);
            }
            bbox_rect(&m, &mut plan);
            let ap = poly::area(&p.pts);
            m.volume() * p.area / ap
        }
        Shape::Blend {
            bottom,
            top,
            range,
            dir,
        } => {
            let (p0, p1) = (prof(bottom, r, lod, name)?, prof(top, r, lod, name)?);
            if p0.pts.len() != p1.pts.len() {
                return Err(gerr(
                    name,
                    format!(
                        "blend profiles differ in vertex count ({} and {})",
                        p0.pts.len(),
                        p1.pts.len()
                    ),
                ));
            }
            let (a, b) = (l(&range[0])?, l(&range[1])?);
            if b - a <= 1e-9 {
                return Err(gerr(name, "blend top must be above bottom"));
            }
            let map = |q: V2, w: f64| match dir {
                Axis::Z => v3(q.x, q.y, w),
                Axis::X => v3(w, q.x, q.y),
                Axis::Y => v3(q.x, w, q.y),
            };
            let lo: Vec<u32> = p0.pts.iter().map(|q| m.v(map(*q, a))).collect();
            let hi: Vec<u32> = p1.pts.iter().map(|q| m.v(map(*q, b))).collect();
            m.cap(&p0.pts, &lo, true);
            m.cap(&p1.pts, &hi, false);
            sides(&mut m, &lo, &hi);
            if *dir == Axis::Y {
                m.flip();
            }
            if *dir == Axis::Z {
                ring_edges(&p0.pts, &mut plan);
                ring_edges(&p1.pts, &mut plan);
            } else {
                bbox_rect(&m, &mut plan);
            }
            let am = match (p0.circle, p1.circle) {
                (Some((_, r0)), Some((_, r1))) => std::f64::consts::PI * ((r0 + r1) / 2.0).powi(2),
                _ => {
                    let mid: Vec<V2> = p0
                        .pts
                        .iter()
                        .zip(&p1.pts)
                        .map(|(x, y)| (*x + *y) * 0.5)
                        .collect();
                    poly::area(&mid).abs()
                }
            };
            let (a0, a1) = match (p0.circle, p1.circle) {
                (Some(_), Some(_)) => (p0.area, p1.area),
                _ => (poly::area(&p0.pts), poly::area(&p1.pts)),
            };
            (b - a) / 6.0 * (a0 + 4.0 * am + a1)
        }
        Shape::Mesh { body, scale } => {
            let s = match scale {
                None => v3(1.0, 1.0, 1.0),
                Some(f) => {
                    let mut k = [1.0; 3];
                    for i in 0..3 {
                        k[i] = num(&f[i], r, NUMBER, name)?;
                        if !(k[i].is_finite() && k[i] > 1e-6) {
                            return Err(gerr(name, "mesh scale must be positive"));
                        }
                    }
                    v3(k[0], k[1], k[2])
                }
            };
            for p in &body.pos {
                m.pos.push(v3(p.x * s.x, p.y * s.y, p.z * s.z));
            }
            for t in &body.tris {
                m.t(t[0], t[1], t[2]);
            }
            let pts: Vec<V2> = m.pos.iter().map(|p| v2(p.x, p.y)).collect();
            let hull = hull(&pts);
            if hull.len() >= 3 {
                ring_edges(&hull, &mut plan);
            }
            if body.welded().closed() {
                m.volume().abs()
            } else {
                0.0
            }
        }
        Shape::Plant { organ, form, dims, leaf, seed } => {
            use super::plant;
            let spec = plant_spec(form, dims, leaf, seed, r, name)?;
            spec.check().map_err(|e| gerr(name, e))?;
            let g = plant::grow(&spec, *organ, lod);
            m.pos = g.pos;
            m.tris = g.tris;
            plan = g.plan;
            g.volume
        }
        Shape::Feature { tl } => {
            let b = feature_body(tl, r).map_err(|e| gerr(name, e))?;
            let mesh = b.to_mesh();
            m.pos = mesh.positions;
            for t in &mesh.triangles {
                m.t(t[0], t[1], t[2]);
            }
            let pts: Vec<V2> = m.pos.iter().map(|p| v2(p.x, p.y)).collect();
            let hull = hull(&pts);
            if hull.len() >= 3 {
                ring_edges(&hull, &mut plan);
            }
            b.volume()
        }
    };
    Ok((m, vol, plan))
}

/// Body of a component timeline: parameters named like family parameters take their resolved
/// values. One shared node cache, so instances differing in one value rebuild only what
/// depends on it.
fn feature_body(tl: &crate::feature::Timeline, r: &Resolved) -> Result<crate::csg::Body, String> {
    use crate::feature::{rebuild, standard, Cache};
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let mut t = tl.clone();
    for p in &mut t.params {
        if let Some(q) = r.nums.get(&p.name) {
            p.expr = super::expr::literal(*q);
        }
    }
    let mut c = CACHE
        .get_or_init(|| Mutex::new(Cache::new(256)))
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let out = rebuild(&t, standard(), &mut c, 0).map_err(|e| e.to_string())?;
    if out.body.is_some() {
        return Ok(out.body.unwrap());
    }
    Err(match out.first_error() {
        Some((id, m)) => {
            let n = t.node(id).map_or(String::new(), |n| n.name.clone());
            format!("feature {n}: {m}")
        }
        None => "the timeline builds nothing".into(),
    })
}

/// Convex hull, counter-clockwise (monotone chain).
fn hull(p: &[V2]) -> Vec<V2> {
    let mut pts: Vec<V2> = p.to_vec();
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup_by(|a, b| (*a - *b).len() < 1e-9);
    if pts.len() < 3 {
        return pts;
    }
    let half = |it: &mut dyn Iterator<Item = V2>| {
        let mut h: Vec<V2> = vec![];
        for q in it {
            while h.len() >= 2 && cross(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
                h.pop();
            }
            h.push(q);
        }
        h.pop();
        h
    };
    let mut lo = half(&mut pts.iter().copied());
    lo.extend(half(&mut pts.iter().rev().copied()));
    lo
}

fn symbol_lines(
    s: &SymShape,
    r: &Resolved,
    lod: Lod,
    out: &mut Vec<(V2, V2)>,
) -> Result<(), FamilyError> {
    let l = |e: &Expr| num(e, r, LENGTH, "symbol");
    let arc = |c: V2, rad: f64, a0: f64, a1: f64, out: &mut Vec<(V2, V2)>| {
        let n = ((lod.segments() as f64 * (a1 - a0).abs() / TAU).ceil() as usize).max(2);
        let at = |k: usize| {
            let t = a0 + (a1 - a0) * k as f64 / n as f64;
            v2(c.x + rad * t.cos(), c.y + rad * t.sin())
        };
        for k in 0..n {
            out.push((at(k), at(k + 1)));
        }
    };
    match s {
        SymShape::Line(a) => out.push((v2(l(&a[0])?, l(&a[1])?), v2(l(&a[2])?, l(&a[3])?))),
        SymShape::Rect(a) => {
            let (x0, y0, x1, y1) = (l(&a[0])?, l(&a[1])?, l(&a[2])?, l(&a[3])?);
            ring_edges(&[v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)], out);
        }
        SymShape::Circle(a) => arc(v2(l(&a[0])?, l(&a[1])?), l(&a[2])?, 0.0, TAU, out),
        SymShape::Arc(a) => {
            let (a0, a1) = (
                num(&a[3], r, ANGLE, "symbol")?,
                num(&a[4], r, ANGLE, "symbol")?,
            );
            arc(v2(l(&a[0])?, l(&a[1])?), l(&a[2])?, a0, a1, out)
        }
        SymShape::Poly(p, closed) => {
            let mut pts = vec![];
            for q in p {
                pts.push(v2(l(&q[0])?, l(&q[1])?));
            }
            for w in pts.windows(2) {
                out.push((w[0], w[1]));
            }
            if *closed && pts.len() > 2 {
                out.push((pts[pts.len() - 1], pts[0]));
            }
        }
    }
    Ok(())
}

fn inside(a: &[V3; 2], b: &[V3; 2]) -> bool {
    let e = 1e-9;
    a[0].x >= b[0].x - e
        && a[0].y >= b[0].y - e
        && a[0].z >= b[0].z - e
        && a[1].x <= b[1].x + e
        && a[1].y <= b[1].y + e
        && a[1].z <= b[1].z + e
}

fn bounds(m: &Mb) -> [V3; 2] {
    let mut b = [
        v3(f64::MAX, f64::MAX, f64::MAX),
        v3(f64::MIN, f64::MIN, f64::MIN),
    ];
    for p in &m.pos {
        b[0] = v3(b[0].x.min(p.x), b[0].y.min(p.y), b[0].z.min(p.z));
        b[1] = v3(b[1].x.max(p.x), b[1].y.max(p.y), b[1].z.max(p.z));
    }
    b
}

impl Family {
    /// Body from the resolved values (`resolve`) at level of detail `lod`.
    pub fn build(&self, r: &Resolved, lod: Lod) -> Result<Built, FamilyError> {
        let mut mesh = Mesh::default();
        let (mut volume, mut surface) = (0.0, 0.0);
        let mut bbox: Option<[V3; 2]> = None;
        let mut voids = vec![];
        let (mut parts, mut plan) = (vec![], vec![]);
        // open mesh pieces: (index in `parts`, positions, triangles)
        let mut open: Vec<(usize, Vec<V3>, Vec<[u32; 3]>)> = vec![];
        for s in &self.solids {
            if r.hidden.contains(&s.name) {
                continue;
            }
            let (m, v, outline) = shape(&s.shape, r, lod, &s.name)?;
            // a bare tree has no foliage: the part stays, empty, with its plan outline
            if m.pos.is_empty() && matches!(s.shape, Shape::Plant { .. }) {
                if !s.void && s.vis & PLAN != 0 && s.vis & lod.bit() != 0 {
                    plan.extend(outline);
                }
                // the material belongs to the definition, so an empty organ keeps it
                parts.push(Part {
                    name: s.name.clone(),
                    material: s.material.as_ref().map(|m| r.material_name(m)).unwrap_or_default(),
                    volume: 0.0,
                    void: s.void,
                    visible: false,
                });
                continue;
            }
            // an open (surface) mesh has no volume on its own; it still displays
            let surface_only = matches!(s.shape, Shape::Mesh { .. }) && !s.void && v == 0.0;
            if surface_only && !s.alt {
                open.push((parts.len(), m.pos.clone(), m.tris.clone()));
            }
            // shade, light and streak organs are tone skins over the crown and bark: no volume of their own
            let tone = !s.void && v == 0.0 && matches!(s.shape, Shape::Plant { organ, .. } if organ.is_tone());
            if !v.is_finite() || (v <= 0.0 && !surface_only && !tone) {
                return Err(gerr(&s.name, "solid has no volume"));
            }
            let visible = !s.void && s.vis & MODEL != 0 && s.vis & lod.bit() != 0;
            let b = bounds(&m);
            if s.void {
                // a void of a wall-hosted family cuts the wall, not the family itself
                if self.hosting != Hosting::Wall {
                    volume -= v;
                }
                voids.push((s.name.clone(), b));
            } else {
                if !s.alt {
                    volume += v;
                    surface += m.area();
                }
                bbox = Some(match bbox {
                    None => b,
                    Some(o) => [
                        v3(o[0].x.min(b[0].x), o[0].y.min(b[0].y), o[0].z.min(b[0].z)),
                        v3(o[1].x.max(b[1].x), o[1].y.max(b[1].y), o[1].z.max(b[1].z)),
                    ],
                });
                if s.vis & PLAN != 0 && s.vis & lod.bit() != 0 {
                    plan.extend(outline);
                }
            }
            if visible {
                let base = mesh.positions.len() as u32;
                let fi = parts.len() as u32;
                mesh.positions.extend(m.pos.iter().copied());
                for t in &m.tris {
                    mesh.triangles.push(t.map(|i| i + base));
                    mesh.face_of.push(fi);
                }
            }
            let material = s.material.as_ref().map(|m| r.material_name(m)).unwrap_or_default();
            parts.push(Part {
                name: s.name.clone(),
                material,
                volume: v,
                void: s.void,
                visible,
            });
        }
        // A model cut into open pieces (one per material) bounds a volume together when the
        // pieces close along their shared edges: the volume is then the sum of signed volumes.
        if open.len() > 1 {
            let (mut pos, mut tris) = (vec![], vec![]);
            for (_, p, t) in &open {
                let base = pos.len() as u32;
                pos.extend_from_slice(p);
                tris.extend(t.iter().map(|t| t.map(|i| i + base)));
            }
            let closed = super::meshbody::MeshBody::new(pos, tris)
                .map(|b| b.welded().closed())
                .unwrap_or(false);
            if closed {
                let signed: Vec<f64> = open
                    .iter()
                    .map(|(_, p, t)| Mb { pos: p.clone(), tris: t.clone() }.volume())
                    .collect();
                let total: f64 = signed.iter().sum();
                let sign = if total < 0.0 { -1.0 } else { 1.0 };
                volume += total.abs();
                for ((k, _, _), v) in open.iter().zip(&signed) {
                    parts[*k].volume = v * sign;
                }
            }
        }
        let bbox = bbox.unwrap_or([v3(0.0, 0.0, 0.0); 2]);
        for (name, b) in voids {
            if self.hosting != Hosting::Wall && !inside(&b, &bbox) {
                return Err(gerr(&name, "void reaches outside the solids"));
            }
        }
        if volume < -1e-12 {
            return Err(gerr(&self.name, "voids exceed the solids"));
        }
        for sym in &self.symbols {
            if sym.vis & PLAN != 0 && sym.vis & lod.bit() != 0 {
                symbol_lines(&sym.shape, r, lod, &mut plan)?;
            }
        }
        let mut b = Built {
            mesh,
            volume,
            surface,
            bbox,
            parts,
            plan,
        };
        self.ext.finish_build(r, lod, &mut b)?;
        Ok(b)
    }

    /// Void bodies (cuts) in component coordinates: what a wall-hosted family removes from its
    /// host. Hidden voids are left out.
    pub fn void_meshes(&self, r: &Resolved, lod: Lod) -> Result<Vec<Mesh>, FamilyError> {
        let mut out = vec![];
        for s in self.solids.iter().filter(|s| s.void && !r.hidden.contains(&s.name)) {
            let (m, _, _) = shape(&s.shape, r, lod, &s.name)?;
            out.push(Mesh {
                face_of: vec![0; m.tris.len()],
                positions: m.pos,
                triangles: m.tris,
            });
        }
        Ok(out)
    }
}
