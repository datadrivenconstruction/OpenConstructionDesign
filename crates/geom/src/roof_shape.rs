//! Roof shapes on top of `roof`: footprint presets (gable, hip, shed, mansard,
//! broken-gable with gable ends), extrusion with profile arcs between offsets, attaching an
//! extrusion to another roof (far end follows the target's top), skylight (rectangular
//! cut-out in a slope; edges of adjacent faces are welded, so no T-junctions and the roof body is closed),
//! eave soffit (between the roof's outer loop and the support line).

use crate::arc::{axis_length, BulgeRing};
use crate::math::{v2, v3, Tol, V2, V3};
use crate::mesh::Mesh;
use crate::poly;
use crate::predicates::point_in_ring;
use crate::roof::{classify, newell, plan, plane_z, Extrusion, Roof, RoofError, RoofFace};

/// Chord sagitta of the extrusion profile arcs.
pub const PROFILE_SAG: f64 = 2e-4;

#[derive(Clone, Debug, PartialEq)]
pub enum ShapeError {
    Roof(RoofError),
    /// Point outside the roof slopes.
    NotOnRoof,
    /// The cut-out extends beyond the slope face (across the ridge, an edge, the eave).
    OutsideFace,
    /// A size is non-finite or too small.
    BadSize,
    /// A profile arc reverses u (|bulge| > 1) or is non-finite.
    BadArc,
    /// No overhang: the outer loop does not enclose the support line.
    NoOverhang,
}

impl From<RoofError> for ShapeError {
    fn from(e: RoofError) -> Self {
        ShapeError::Roof(e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    Gable,
    Hip,
    Shed,
    Mansard,
    /// Broken gable: two slopes along the long sides, ends are gables.
    Gambrel,
}

impl Preset {
    pub const ALL: [Preset; 5] = [
        Preset::Gable,
        Preset::Hip,
        Preset::Shed,
        Preset::Mansard,
        Preset::Gambrel,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Preset::Gable => "gable",
            Preset::Hip => "hip",
            Preset::Shed => "shed",
            Preset::Mansard => "mansard",
            Preset::Gambrel => "gambrel",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == s)
    }
}

/// Loop edge slopes and mansard data for a preset. Collinear adjacent edges form one run.
/// Gable/broken: for a convex loop the slopes are the runs along the main axis (the longest,
/// ±10°), the rest are gables; for an L/T plan a gable is the end of a wing (both corners convex, not longer
/// than the neighbours). Shed: the slope is on the longest run. Mansard/broken: lower slope
/// `slope`, break at `break_h` above the support, upper slope `upper` (None means a flat deck).
pub fn preset(
    ring: &[V2],
    p: Preset,
    slope: f64,
    upper: Option<f64>,
    break_h: f64,
) -> (Vec<Option<f64>>, Option<(f64, Option<f64>)>) {
    let n = ring.len();
    let ccw = poly::area(ring) >= 0.0;
    let edge = |i: usize| ring[(i + 1) % n] - ring[i];
    // runs: run[i] is the run index of edge i
    let collinear = |i: usize, j: usize| {
        let (a, b) = (edge(i), edge(j));
        let l = a.len() * b.len();
        l > 0.0 && a.dot(b) > 0.0 && (a.cross(b) / l).abs() < 1e-9
    };
    let start = (0..n)
        .find(|&i| !collinear((i + n - 1) % n, i))
        .unwrap_or(0);
    let mut run = vec![0usize; n];
    let mut runs: Vec<(V2, V2)> = vec![]; // (start, end)
    for k in 0..n {
        let i = (start + k) % n;
        if k == 0 || !collinear((i + n - 1) % n, i) {
            runs.push((ring[i], ring[(i + 1) % n]));
        } else if let Some(r) = runs.last_mut() {
            r.1 = ring[(i + 1) % n];
        }
        run[i] = runs.len() - 1;
    }
    let m = runs.len();
    let rv = |r: usize| runs[r].1 - runs[r].0;
    // runs of equal length: the first in traversal order (max_by would return the last)
    let longest = (0..m).fold(0, |best, r| {
        if rv(r).len() > rv(best).len() + 1e-9 {
            r
        } else {
            best
        }
    });
    let ax = rv(longest);
    let along = |r: usize| {
        let e = rv(r);
        let l = e.len() * ax.len();
        l > 0.0 && (e.cross(ax) / l).abs() < 10f64.to_radians().sin()
    };
    // convex corner at the end of run r (between r and r+1)
    let convex_at = |r: usize| {
        let k = rv(r).cross(rv((r + 1) % m));
        if ccw {
            k > 0.0
        } else {
            k < 0.0
        }
    };
    let concave = (0..m).any(|r| !convex_at(r) && rv(r).cross(rv((r + 1) % m)).abs() > 1e-12);
    let gable_end = |r: usize| {
        let (pr, nx) = ((r + m - 1) % m, (r + 1) % m);
        let l = rv(r).len();
        convex_at(pr) && convex_at(r) && l <= rv(pr).len() + 1e-9 && l <= rv(nx).len() + 1e-9
    };
    let gable = |r: usize| if concave { gable_end(r) } else { !along(r) };
    let s = Some(slope);
    let slopes: Vec<Option<f64>> = (0..n)
        .map(|i| match p {
            Preset::Hip | Preset::Mansard => s,
            Preset::Gable | Preset::Gambrel => (!gable(run[i])).then_some(slope),
            Preset::Shed => (run[i] == longest).then_some(slope),
        })
        .collect();
    let mansard = match p {
        Preset::Mansard | Preset::Gambrel => Some((break_h, upper)),
        _ => None,
    };
    (slopes, mansard)
}

/// Extrusion with arcs: profile (u, z) over a→b, `bulges[i]` is the arc of run i→i+1 in the (u, z)
/// plane (bulge < 0 arches upward as u grows); swept to the left of offset `w0` by `depth`.
#[derive(Clone, Debug, PartialEq)]
pub struct ArcExtrusion {
    pub a: V2,
    pub b: V2,
    pub profile: Vec<(f64, f64)>,
    pub bulges: Vec<f64>,
    pub w0: f64,
    pub depth: f64,
}

impl ArcExtrusion {
    fn bulge(&self, i: usize) -> f64 {
        self.bulges.get(i).copied().unwrap_or(0.0)
    }

    /// Profile with tessellated arcs; u strictly increases.
    pub fn points(&self) -> Result<Vec<(f64, f64)>, ShapeError> {
        if self.profile.len() < 2 {
            return Err(RoofError::BadProfile.into());
        }
        let mut out: Vec<(f64, f64)> = vec![self.profile[0]];
        for i in 0..self.profile.len() - 1 {
            let (p, q) = (self.profile[i], self.profile[i + 1]);
            let b = self.bulge(i);
            if !b.is_finite() || b.abs() > 1.0 + 1e-12 {
                return Err(ShapeError::BadArc);
            }
            let pts = BulgeRing {
                pts: vec![v2(p.0, p.1), v2(q.0, q.1)],
                bulges: vec![b, 0.0],
            }
            .tessellate(PROFILE_SAG);
            out.extend(pts.iter().skip(1).map(|v| (v.x, v.y)));
            out.push(q);
        }
        out.dedup_by(|x, y| (x.0 - y.0).abs() < 1e-12 && (x.1 - y.1).abs() < 1e-12);
        if out.windows(2).any(|w| w[1].0 <= w[0].0) {
            return Err(ShapeError::BadArc);
        }
        Ok(out)
    }

    /// Plain extrusion with the tessellated profile and a shifted start.
    pub fn base(&self) -> Result<Extrusion, ShapeError> {
        let l = (self.b - self.a).len();
        if !(l > 1e-9) || !self.w0.is_finite() {
            return Err(RoofError::BadProfile.into());
        }
        let d = (self.b - self.a) * (1.0 / l);
        let nrm = v2(-d.y, d.x) * self.w0;
        Ok(Extrusion {
            a: self.a + nrm,
            b: self.b + nrm,
            profile: self.points()?,
            depth: self.depth,
        })
    }

    pub fn build(&self, base_z: f64) -> Result<Roof, ShapeError> {
        Ok(self.base()?.build(base_z)?)
    }

    /// Exact: (profile length along the arcs, area under the profile down to z = 0).
    pub fn exact(&self) -> (f64, f64) {
        let (mut len, mut under) = (0.0, 0.0);
        for i in 0..self.profile.len().saturating_sub(1) {
            let (p, q) = (self.profile[i], self.profile[i + 1]);
            let b = self.bulge(i);
            len += axis_length(v2(p.0, p.1), v2(q.0, q.1), b);
            under += (q.0 - p.0) * (p.1 + q.1) / 2.0;
            if b.abs() > 1e-12 {
                let c = (q.0 - p.0).hypot(q.1 - p.1);
                let r = c * (1.0 + b * b) / (4.0 * b.abs());
                let th = 4.0 * b.abs().atan();
                // bulge > 0: the arc lies to the right of p→q (lower as u grows)
                under -= b.signum() * r * r / 2.0 * (th - th.sin());
            }
        }
        (len, under)
    }
}

/// Sloped roof faces: (plan, upward normal, point).
fn sloped_planes(r: &Roof) -> Vec<(Vec<V2>, V3, V3)> {
    r.faces
        .iter()
        .filter(|f| f.sloped)
        .filter_map(|f| {
            let mut n = newell(&f.pts);
            if n.z.abs() < 1e-12 {
                return None;
            }
            if n.z < 0.0 {
                n = -n;
            }
            Some((plan(f), n, f.pts[0]))
        })
        .collect()
}

/// First intersection of the horizontal ray o + nrm·w (w ≥ 0) at height z with the roof top.
fn hit(o: V2, nrm: V2, z: f64, planes: &[(Vec<V2>, V3, V3)]) -> Option<(f64, usize)> {
    let mut best: Option<(f64, usize)> = None;
    for (k, (ring, n, p0)) in planes.iter().enumerate() {
        let z_o = p0.z - (n.x * (o.x - p0.x) + n.y * (o.y - p0.y)) / n.z;
        let kk = (n.x * nrm.x + n.y * nrm.y) / n.z;
        if kk.abs() < 1e-12 {
            continue;
        }
        // z_o − kk·w = z
        let w = (z_o - z) / kk;
        if w < -1e-9 || !w.is_finite() {
            continue;
        }
        let w = w.max(0.0);
        if point_in_ring(o + nrm * w, ring) < 0 {
            continue;
        }
        if best.is_none_or(|b| w < b.0) {
            best = Some((w, k));
        }
    }
    best
}

/// Extrusion attached to roof `target`: the far end of each profile run is the
/// line where its plane meets the target roof top (extension or trimming); where the ray
/// misses the target, the end sits at `depth`. A run crossing a target edge is split.
pub fn join(ex: &ArcExtrusion, base_z: f64, target: &Roof) -> Result<Roof, ShapeError> {
    let e = ex.base()?;
    // profile and depth validation: same as for a plain extrusion
    e.build(base_z)?;
    let l = (e.b - e.a).len();
    let d = (e.b - e.a) * (1.0 / l);
    let nrm = v2(-d.y, d.x);
    let planes = sloped_planes(target);
    let prof = &e.profile;
    let far = |u: f64, z: f64| -> (f64, usize) {
        hit(e.a + d * u, nrm, base_z + z, &planes).unwrap_or((e.depth, usize::MAX))
    };
    let at = |u: f64, w: f64, z: f64| {
        let p = e.a + d * u + nrm * w;
        v3(p.x, p.y, base_z + z)
    };
    let mut faces = vec![];
    // far edges along the target top: a roof-to-roof joint (valley), not a gable overhang
    let mut seam: Vec<(V3, V3)> = vec![];
    for (k, s) in prof.windows(2).enumerate() {
        let (p, q) = (s[0], s[1]);
        let lerp = |t: f64| (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t);
        // far edge: points (t, w) along the run, bisected where the target face changes
        let mut ts: Vec<(f64, f64)> = vec![];
        let h0 = far(p.0, p.1);
        let h1 = far(q.0, q.1);
        ts.push((0.0, h0.0));
        refine(&far, &lerp, (0.0, h0), (1.0, h1), 14, &mut ts);
        ts.push((1.0, h1.0));
        let mut pts = vec![at(p.0, 0.0, p.1), at(q.0, 0.0, q.1)];
        let mut prev: Option<(V3, bool)> = None;
        for &(t, w) in ts.iter().rev() {
            let (u, z) = lerp(t);
            let v = at(u, w, z);
            let on = far(u, z).1 != usize::MAX;
            if let Some((pv, pon)) = prev {
                if on && pon && (v - pv).len() > 1e-9 {
                    seam.push((pv, v));
                }
            }
            prev = Some((v, on));
            pts.push(v);
        }
        pts.dedup_by(|a, b| (*a - *b).len() < 1e-9);
        while pts.len() > 1 && (pts[0] - pts[pts.len() - 1]).len() < 1e-9 {
            pts.pop();
        }
        let pl: Vec<V2> = pts.iter().map(|v| v2(v.x, v.y)).collect();
        if pts.len() < 3 || poly::area(&pl) < 1e-12 {
            continue;
        }
        faces.push(RoofFace {
            pts,
            sloped: true,
            edge: k,
            phase: 0,
        });
    }
    if faces.is_empty() {
        return Err(RoofError::BadProfile.into());
    }
    let zmin = prof.iter().map(|p| p.1).fold(f64::MAX, f64::min);
    let mut lines = classify(&faces, base_z + zmin, 1e-7);
    let on_seam = |a: V3, b: V3| {
        seam.iter().any(|&(x, y)| {
            let d = y - x;
            let l2 = d.dot(d);
            let near = |p: V3| {
                let t = ((p - x).dot(d) / l2).clamp(0.0, 1.0);
                (p - (x + d * t)).len() < 1e-6
            };
            l2 > 0.0 && near(a) && near(b)
        })
    };
    for l in &mut lines {
        if matches!(
            l.kind,
            crate::roof::LineKind::Rake | crate::roof::LineKind::Eave
        ) && on_seam(l.a, l.b)
        {
            l.kind = crate::roof::LineKind::Valley;
        }
    }
    Ok(Roof {
        faces,
        lines,
        eave_z: base_z + zmin,
    })
}

fn refine(
    far: &dyn Fn(f64, f64) -> (f64, usize),
    lerp: &dyn Fn(f64) -> (f64, f64),
    a: (f64, (f64, usize)),
    b: (f64, (f64, usize)),
    depth: u32,
    out: &mut Vec<(f64, f64)>,
) {
    if a.1 .1 == b.1 .1 || depth == 0 {
        return;
    }
    let t = (a.0 + b.0) / 2.0;
    let (u, z) = lerp(t);
    let h = far(u, z);
    refine(far, lerp, a, (t, h), depth - 1, out);
    out.push((t, h.0));
    refine(far, lerp, (t, h), b, depth - 1, out);
}

/// Rectangular cut-out in a slope: plan centre, width along the slope's horizontal, length along
/// the slope (true length).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Opening {
    pub center: V2,
    pub width: f64,
    pub length: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cut {
    pub roof: Roof,
    /// Cut-out corners on the slope top (counter-clockwise in plan).
    pub corners: [V3; 4],
    /// Unit slope normal pointing up.
    pub normal: V3,
}

/// Skylight cut-out: the rectangle is oriented by the face under the centre (width along the horizontal,
/// length along the slope) and cuts every face it touches (across a valley, a break, vault faces).
/// The cut-out must lie entirely on the roof and not touch earlier cut-outs.
pub fn cut_opening(roof: &Roof, o: &Opening) -> Result<Cut, ShapeError> {
    if !(o.width >= 0.05 && o.length >= 0.05) || !o.center.x.is_finite() || !o.center.y.is_finite()
    {
        return Err(ShapeError::BadSize);
    }
    let fi = roof
        .faces
        .iter()
        .position(|f| f.sloped && point_in_ring(o.center, &plan(f)) > 0)
        .or_else(|| {
            roof.faces
                .iter()
                .position(|f| f.sloped && point_in_ring(o.center, &plan(f)) >= 0)
        })
        .ok_or(ShapeError::NotOnRoof)?;
    let f = &roof.faces[fi];
    let mut n = newell(&f.pts);
    if n.z < 0.0 {
        n = -n;
    }
    let nl = n.len();
    let h = v2(n.x, n.y);
    // up the slope in plan: −(n.x, n.y); a flat face uses x
    let g = if h.len() > 1e-9 * nl {
        h * (-1.0 / h.len())
    } else {
        v2(1.0, 0.0)
    };
    let e = v2(-g.y, g.x);
    let (hl, hw) = (o.length * n.z / nl / 2.0, o.width / 2.0);
    let c = o.center;
    let mut rect = vec![
        c - g * hl - e * hw,
        c + g * hl - e * hw,
        c + g * hl + e * hw,
        c - g * hl + e * hw,
    ];
    if poly::area(&rect) < 0.0 {
        rect.reverse();
    }
    let before = roof.quantities().plan_area;
    let cut = crate::shape_edit::cut_roof(roof, &rect, false);
    // the cut-out is fully on the roof and clear of earlier ones: removed area = rectangle area
    let removed = before - cut.quantities().plan_area;
    let ra = poly::area(&rect);
    if (removed - ra).abs() > 1e-7 * ra.max(1.0) || rect.iter().any(|&p| roof.top_at(p).is_none()) {
        return Err(ShapeError::OutsideFace);
    }
    let corners = [0, 1, 2, 3].map(|i| v3(rect[i].x, rect[i].y, plane_z(f, rect[i])));
    Ok(Cut {
        roof: cut,
        corners,
        normal: n * (1.0 / nl),
    })
}

/// Inserts into face edges the vertices of other faces that lie on them (removes T-junctions).
pub fn weld(faces: &mut [RoofFace], eps: f64) {
    let all: Vec<V3> = faces.iter().flat_map(|f| f.pts.iter().copied()).collect();
    for f in faces.iter_mut() {
        let n = f.pts.len();
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let (a, b) = (f.pts[i], f.pts[(i + 1) % n]);
            out.push(a);
            let d = b - a;
            let l2 = d.dot(d);
            if l2 < eps * eps {
                continue;
            }
            let mut mids: Vec<(f64, V3)> = all
                .iter()
                .filter_map(|&p| {
                    let t = (p - a).dot(d) / l2;
                    if t * l2.sqrt() <= eps || (1.0 - t) * l2.sqrt() <= eps {
                        return None;
                    }
                    ((p - (a + d * t)).len() <= eps).then_some((t, p))
                })
                .collect();
            mids.sort_by(|x, y| x.0.total_cmp(&y.0));
            mids.dedup_by(|x, y| (x.1 - y.1).len() <= eps);
            out.extend(mids.into_iter().map(|m| m.1));
        }
        f.pts = out;
    }
}

/// Prism over quadrilateral `base` along unit `n` from h0 to h1.
pub fn prism(base: &[V3; 4], n: V3, h0: f64, h1: f64) -> Mesh {
    let mut m = Mesh::default();
    for h in [h0, h1] {
        for p in base {
            m.positions.push(*p + n * h);
        }
    }
    let quads = [
        [0, 3, 2, 1],
        [4, 5, 6, 7],
        [0, 1, 5, 4],
        [1, 2, 6, 5],
        [2, 3, 7, 6],
        [3, 0, 4, 7],
    ];
    for (k, q) in quads.iter().enumerate() {
        m.triangles.push([q[0], q[1], q[2]]);
        m.triangles.push([q[0], q[2], q[3]]);
        m.face_of.push(k as u32);
        m.face_of.push(k as u32);
    }
    if m.volume() < 0.0 {
        for t in &mut m.triangles {
            t.swap(1, 2);
        }
    }
    m
}

/// Skylight body: a curb box over the cut-out from the roof underside (`below` along the normal) to `curb` above the top,
/// and glazing of thickness `glass` on top, inset by the frame `frame`.
pub fn skylight_body(c: &Cut, below: f64, curb: f64, frame: f64, glass: f64) -> (Mesh, Mesh) {
    skylight_frame(&c.corners, c.normal, below, curb, frame, glass)
}

/// Skylight body from the cut-out corners and the unit slope normal (see [`skylight_body`]).
pub fn skylight_frame(
    k: &[V3; 4],
    normal: V3,
    below: f64,
    curb: f64,
    frame: f64,
    glass: f64,
) -> (Mesh, Mesh) {
    let box_ = prism(k, normal, -below, curb);
    // equal-width frame: shift the corner along both adjacent edges
    let short = (0..4).any(|i| (k[(i + 1) % 4] - k[i]).len() <= 2.0 * frame);
    let inset: [V3; 4] = std::array::from_fn(|i| {
        let p = k[i];
        if short {
            return p;
        }
        let (a, b) = (k[(i + 1) % 4] - p, k[(i + 3) % 4] - p);
        p + a * (frame / a.len()) + b * (frame / b.len())
    });
    (box_, prism(&inset, normal, curb, curb + glass))
}

/// Outer loops of the slopes in plan (boundary of the union of sloped faces; skylight
/// holes are loops too).
pub fn outline(roof: &Roof) -> Vec<Vec<V2>> {
    let key = |p: V2| ((p.x * 1e7).round() as i64, (p.y * 1e7).round() as i64);
    let mut count: std::collections::HashMap<((i64, i64), (i64, i64)), (V2, V2, usize)> =
        std::collections::HashMap::new();
    for f in roof.faces.iter().filter(|f| f.sloped) {
        let pl = plan(f);
        for i in 0..pl.len() {
            let (a, b) = (pl[i], pl[(i + 1) % pl.len()]);
            let (ka, kb) = (key(a), key(b));
            if ka == kb {
                continue;
            }
            let k = if ka <= kb { (ka, kb) } else { (kb, ka) };
            count.entry(k).or_insert((a, b, 0)).2 += 1;
        }
    }
    let mut segs: Vec<(V2, V2)> = count
        .into_values()
        .filter(|v| v.2 == 1)
        .map(|v| (v.0, v.1))
        .collect();
    segs.sort_by(|x, y| {
        (x.0.x, x.0.y)
            .partial_cmp(&(y.0.x, y.0.y))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    poly::chain(&segs, 1e-7)
}

/// Eave soffit: a horizontal slab of thickness `t` with its top at `z_top` between the roof's outer
/// loop and the support line `support`. Returns (body, area).
pub fn soffit(roof: &Roof, support: &[V2], z_top: f64, t: f64) -> Result<(Mesh, f64), ShapeError> {
    if !(t > 0.0) || !z_top.is_finite() || support.len() < 3 {
        return Err(ShapeError::BadSize);
    }
    let outer = outline(roof)
        .into_iter()
        .max_by(|a, b| poly::area(a).abs().total_cmp(&poly::area(b).abs()))
        .ok_or(ShapeError::NoOverhang)?;
    if support.iter().any(|&p| point_in_ring(p, &outer) <= 0) {
        return Err(ShapeError::NoOverhang);
    }
    let area = poly::area(&outer).abs() - poly::area(support).abs();
    if area < 1e-6 {
        return Err(ShapeError::NoOverhang);
    }
    let tol = Tol::default();
    let p = crate::Profile::new(outer, vec![support.to_vec()], tol)
        .map_err(|_| ShapeError::NoOverhang)?;
    let m = crate::Solid::extrude(&p, t, tol)
        .translated_z(z_top - t)
        .triangulate()
        .map_err(|_| ShapeError::NoOverhang)?;
    Ok((m, area))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roof::{Footprint, LineKind};

    fn near(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() <= tol * b.abs().max(1.0), "{a} vs {b}");
    }

    fn rect(l: f64, w: f64) -> Vec<V2> {
        vec![v2(0.0, 0.0), v2(l, 0.0), v2(l, w), v2(0.0, w)]
    }

    fn build(ring: &[V2], p: Preset, slope: f64, overhang: f64) -> Roof {
        let (slopes, mansard) = preset(ring, p, slope, Some(25.0), 1.5);
        Footprint {
            ring: ring.to_vec(),
            slopes,
            base_z: 3.0,
            overhang,
            mansard,
        }
        .build()
        .unwrap()
    }

    #[test]
    fn presets_match_analytic_areas_and_volumes() {
        let (l, w) = (10.0, 6.0);
        let th = 30f64.to_radians();
        let r = rect(l, w);
        // gable: slopes along the long sides
        let (s, m) = preset(&r, Preset::Gable, 30.0, None, 0.0);
        assert_eq!(s, vec![Some(30.0), None, Some(30.0), None]);
        assert_eq!(m, None);
        let q = build(&r, Preset::Gable, 30.0, 0.0).quantities();
        near(q.plan_area, l * w, 1e-9);
        near(q.area, l * w / th.cos(), 1e-9);
        let h = w / 2.0 * th.tan();
        near(q.volume_under, l * w * h / 2.0, 1e-9);
        near(q.gable_area, w * h, 1e-9);
        near(q.ridge, l, 1e-9);
        // hip: V = h·W·(L − W/3)/2
        let q = build(&r, Preset::Hip, 30.0, 0.0).quantities();
        near(q.area, l * w / th.cos(), 1e-9);
        near(q.volume_under, h * w * (l - w / 3.0) / 2.0, 1e-9);
        near(q.ridge, l - w, 1e-9);
        // shed: the slope is on the longest edge, rising across the full width
        let (s, _) = preset(&r, Preset::Shed, 30.0, None, 0.0);
        assert_eq!(s.iter().filter(|x| x.is_some()).count(), 1);
        let q = build(&r, Preset::Shed, 30.0, 0.0).quantities();
        near(q.area, l * w / th.cos(), 1e-9);
        near(q.volume_under, l * w * w * th.tan() / 2.0, 1e-9);
    }

    #[test]
    fn mansard_and_gambrel_presets_break_slopes() {
        let r = rect(10.0, 8.0);
        let (s, m) = preset(&r, Preset::Mansard, 70.0, Some(25.0), 1.5);
        assert!(s.iter().all(|x| *x == Some(70.0)));
        assert_eq!(m, Some((1.5, Some(25.0))));
        let mansard = build(&r, Preset::Mansard, 70.0, 0.0);
        assert!(mansard.lines.iter().any(|l| l.kind == LineKind::Break));
        let (s, m) = preset(&r, Preset::Gambrel, 60.0, Some(25.0), 1.5);
        assert_eq!(s, vec![Some(60.0), None, Some(60.0), None]);
        assert!(m.is_some());
        let g = build(&r, Preset::Gambrel, 60.0, 0.0);
        let q = g.quantities();
        assert!(q.gable_area > 0.0);
        assert!(g.lines.iter().any(|l| l.kind == LineKind::Break));
        // broken: lower slope 1.5/tan60 in plan on each side, top at 25°
        let run_lo = 1.5 / 60f64.to_radians().tan();
        let rise_hi = (4.0 - run_lo) * 25f64.to_radians().tan();
        let top = g.top_at(v2(5.0, 4.0)).unwrap();
        near(top, 3.0 + 1.5 + rise_hi, 1e-9);
    }

    #[test]
    fn barrel_extrusion_matches_arc_and_offsets() {
        let ex = ArcExtrusion {
            a: v2(0.0, 0.0),
            b: v2(6.0, 0.0),
            profile: vec![(0.0, 0.0), (6.0, 0.0)],
            bulges: vec![-1.0],
            w0: 2.0,
            depth: 10.0,
        };
        let (len, under) = ex.exact();
        let pi = std::f64::consts::PI;
        near(len, 3.0 * pi, 1e-12);
        near(under, pi * 9.0 / 2.0, 1e-12);
        let r = ex.build(4.0).unwrap();
        let q = r.quantities();
        near(q.area, len * 10.0, 1e-4);
        near(q.volume_under, under * 10.0, 2e-4);
        near(q.plan_area, 60.0, 1e-9);
        // between offsets: plan width [2, 12], vault at the centre
        near(r.top_at(v2(3.0, 2.5)).unwrap(), 7.0, 1e-4);
        assert!(r.top_at(v2(3.0, 1.5)).is_none());
        assert!(r.top_at(v2(3.0, 12.5)).is_none());
        // |bulge| > 1 reverses u
        let bad = ArcExtrusion {
            bulges: vec![-1.5],
            ..ex.clone()
        };
        assert_eq!(bad.points().unwrap_err(), ShapeError::BadArc);
    }

    fn plane_roof() -> Roof {
        // a single slope z = 0.5·y over [−10, 10] × [0, 20]
        let f = RoofFace {
            pts: vec![
                v3(-10.0, 0.0, 0.0),
                v3(10.0, 0.0, 0.0),
                v3(10.0, 20.0, 10.0),
                v3(-10.0, 20.0, 10.0),
            ],
            sloped: true,
            edge: 0,
            phase: 0,
        };
        let lines = classify(std::slice::from_ref(&f), 0.0, 1e-7);
        Roof {
            faces: vec![f],
            lines,
            eave_z: 0.0,
        }
    }

    #[test]
    fn join_extends_to_target_plane() {
        let ex = ArcExtrusion {
            a: v2(0.0, 0.0),
            b: v2(4.0, 0.0),
            profile: vec![(0.0, 0.0), (2.0, 2.0), (4.0, 0.0)],
            bulges: vec![],
            w0: 0.0,
            depth: 1.0,
        };
        let r = join(&ex, 0.0, &plane_roof()).unwrap();
        let q = r.quantities();
        // each slope is the triangle (0,0)-(2,0)-(2,4) in plan
        near(q.plan_area, 8.0, 1e-9);
        near(q.area, 8.0 * 2f64.sqrt(), 1e-9);
        near(q.volume_under, 32.0 / 3.0, 1e-9);
        // the ridge is extended to y = 4 (depth 1 is extended)
        let ridge: f64 = r
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Ridge)
            .map(|l| (l.b - l.a).len())
            .sum();
        near(ridge, 4.0, 1e-9);
    }

    #[test]
    fn join_trims_and_misses_keep_depth() {
        let ex = ArcExtrusion {
            a: v2(0.0, 0.0),
            b: v2(4.0, 0.0),
            profile: vec![(0.0, 4.0), (4.0, 4.0)],
            bulges: vec![],
            w0: 0.0,
            depth: 30.0,
        };
        // the flat top z = 4 meets the slope at y = 8: trimmed from 30 to 8
        let q = join(&ex, 0.0, &plane_roof()).unwrap().quantities();
        near(q.plan_area, 32.0, 1e-9);
        // target off to the side: the end sits at depth
        let far = ArcExtrusion {
            a: v2(100.0, 0.0),
            b: v2(104.0, 0.0),
            ..ex.clone()
        };
        let q = join(&far, 0.0, &plane_roof()).unwrap().quantities();
        near(q.plan_area, 120.0, 1e-9);
    }

    #[test]
    fn skylight_cuts_exact_area_and_mesh_stays_closed() {
        let r = build(&rect(10.0, 6.0), Preset::Hip, 30.0, 0.5);
        let before = r.quantities();
        let o = Opening {
            center: v2(5.0, 1.0),
            width: 1.0,
            length: 1.2,
        };
        let c = cut_opening(&r, &o).unwrap();
        let t = 0.25;
        let after = c.roof.quantities();
        near(after.area, before.area - 1.2, 1e-9);
        let cos = 30f64.to_radians().cos();
        near(after.plan_area, before.plan_area - 1.2 * cos, 1e-9);
        // corners lie on the slope plane; the cut-out length is along the slope
        near(
            (c.corners[1] - c.corners[0])
                .len()
                .max((c.corners[2] - c.corners[1]).len()),
            1.2,
            1e-9,
        );
        for p in c.corners {
            near(r.top_at(v2(p.x, p.y)).unwrap(), p.z, 1e-9);
        }
        // roof body: volume = true area × thickness (welded, no T-junctions)
        near(c.roof.mesh(t).volume(), after.area * t, 1e-9);
        // skylight: curb box and glazing above the slope
        let (frame, glass) = skylight_body(&c, t, 0.15, 0.05, 0.02);
        near(frame.volume(), 1.0 * 1.2 * (t + 0.15), 1e-9);
        assert!(glass.volume() > 0.0);
        // across a hip edge it cuts both faces; past the eave is an error; outside the roof is an error
        let wide = Opening {
            center: v2(2.0, 1.0),
            width: 3.0,
            length: 1.0,
        };
        let cw = cut_opening(&r, &wide).unwrap();
        near(
            before.plan_area - cw.roof.quantities().plan_area,
            3.0 * cos,
            1e-9,
        );
        near(
            cw.roof.mesh(t).volume(),
            cw.roof.quantities().area * t,
            1e-9,
        );
        let past_eave = Opening {
            center: v2(5.0, -0.3),
            ..o
        };
        assert_eq!(
            cut_opening(&r, &past_eave).unwrap_err(),
            ShapeError::OutsideFace
        );
        // centre inside an earlier cut-out: not on the roof; centre nearby but touching a cut-out: area error
        assert_eq!(cut_opening(&c.roof, &o).unwrap_err(), ShapeError::NotOnRoof);
        let touching = Opening {
            center: v2(5.7, 1.0),
            ..o
        };
        assert_eq!(
            cut_opening(&c.roof, &touching).unwrap_err(),
            ShapeError::OutsideFace
        );
        let off = Opening {
            center: v2(50.0, 1.0),
            ..o
        };
        assert_eq!(cut_opening(&r, &off).unwrap_err(), ShapeError::NotOnRoof);
        // two skylights in a row
        let c2 = cut_opening(
            &c.roof,
            &Opening {
                center: v2(7.0, 1.0),
                ..o
            },
        )
        .unwrap();
        near(c2.roof.quantities().area, before.area - 2.4, 1e-9);
        near(c2.roof.mesh(t).volume(), (before.area - 2.4) * t, 1e-9);
    }

    #[test]
    fn soffit_fills_overhang() {
        let ring = rect(10.0, 6.0);
        let r = build(&ring, Preset::Hip, 30.0, 0.5);
        let (m, a) = soffit(&r, &ring, 3.0, 0.02).unwrap();
        near(a, 11.0 * 7.0 - 60.0, 1e-9);
        near(m.volume(), a * 0.02, 1e-9);
        let flush = build(&ring, Preset::Hip, 30.0, 0.0);
        assert_eq!(
            soffit(&flush, &ring, 3.0, 0.02).unwrap_err(),
            ShapeError::NoOverhang
        );
    }

    fn l_ring() -> Vec<V2> {
        vec![
            v2(0.0, 0.0),
            v2(10.0, 0.0),
            v2(10.0, 4.0),
            v2(4.0, 4.0),
            v2(4.0, 8.0),
            v2(0.0, 8.0),
        ]
    }

    #[test]
    fn gable_on_l_plan_puts_gables_on_wing_ends() {
        let r = l_ring();
        let (s, _) = preset(&r, Preset::Gable, 30.0, None, 0.0);
        let x = Some(30.0);
        assert_eq!(s, vec![x, None, x, x, None, x]);
        let g = Footprint {
            ring: r,
            slopes: s,
            base_z: 0.0,
            overhang: 0.0,
            mansard: None,
        }
        .build()
        .unwrap();
        assert!(g.quantities().gable_area > 0.0);
        // rectangle: as before, slopes along the long sides
        let (s, _) = preset(&rect(10.0, 6.0), Preset::Gable, 30.0, None, 0.0);
        assert_eq!(s, vec![x, None, x, None]);
    }

    #[test]
    fn shed_with_split_long_edge_slopes_whole_run() {
        let r = vec![
            v2(0.0, 0.0),
            v2(4.0, 0.0),
            v2(10.0, 0.0),
            v2(10.0, 6.0),
            v2(0.0, 6.0),
        ];
        let (s, _) = preset(&r, Preset::Shed, 20.0, None, 0.0);
        assert_eq!(s, vec![Some(20.0), Some(20.0), None, None, None]);
        let q = Footprint {
            ring: r,
            slopes: s,
            base_z: 0.0,
            overhang: 0.0,
            mansard: None,
        }
        .build()
        .unwrap()
        .quantities();
        near(q.plan_area, 60.0, 1e-9);
    }

    #[test]
    fn skylight_across_l_plan_valley() {
        let r = build(&l_ring(), Preset::Hip, 30.0, 0.0);
        let before = r.quantities();
        let o = Opening {
            center: v2(3.6, 3.6),
            width: 0.6,
            length: 0.6,
        };
        let c = cut_opening(&r, &o).unwrap();
        let q = c.roof.quantities();
        let ra = 0.6 * 0.6 * 30f64.to_radians().cos();
        near(before.plan_area - q.plan_area, ra, 1e-9);
        near(c.roof.mesh(0.2).volume(), q.area * 0.2, 1e-9);
    }

    #[test]
    fn skylight_frame_has_equal_widths() {
        let k = [
            v3(0.0, 0.0, 0.0),
            v3(0.78, 0.0, 0.0),
            v3(0.78, 1.18, 0.0),
            v3(0.0, 1.18, 0.0),
        ];
        let c = Cut {
            roof: plane_roof(),
            corners: k,
            normal: v3(0.0, 0.0, 1.0),
        };
        let (_, glass) = skylight_body(&c, 0.2, 0.15, 0.05, 0.02);
        let xs: Vec<f64> = glass.positions.iter().map(|p| p.x).collect();
        let ys: Vec<f64> = glass.positions.iter().map(|p| p.y).collect();
        let span = |v: &[f64]| {
            v.iter().copied().fold(f64::MIN, f64::max) - v.iter().copied().fold(f64::MAX, f64::min)
        };
        near(span(&xs), 0.68, 1e-12);
        near(span(&ys), 1.08, 1e-12);
    }

    #[test]
    fn join_seam_is_valley_not_rake() {
        let ex = ArcExtrusion {
            a: v2(0.0, 0.0),
            b: v2(4.0, 0.0),
            profile: vec![(0.0, 0.0), (2.0, 2.0), (4.0, 0.0)],
            bulges: vec![],
            w0: 0.0,
            depth: 1.0,
        };
        let r = join(&ex, 0.0, &plane_roof()).unwrap();
        let q = r.quantities();
        // gable edges only at the near end
        near(q.rake, 2.0 * 8f64.sqrt(), 1e-9);
        assert!(q.valley > 0.0);
    }
}
