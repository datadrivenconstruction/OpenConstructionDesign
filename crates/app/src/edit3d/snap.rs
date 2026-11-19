//! 3D snaps: vertices, edge midpoints, edge intersections, axis intersections, edges,
//! axis lines on the work plane, the face under the cursor, the work-plane point (grid step).
//! Candidates within the cursor radius are ordered by kind priority and on-screen distance;
//! Tab cycles through them. Points behind an element's visible face (not a room volume) and outside
//! the section box are discarded.
//! Exact NURBS shapes add: nearest on curve, curve midpoint and thirds, tangent and
//! perpendicular from the start point, surface point and iso-curves.

use ddc_render::glam::Vec3;
use ddc_render::pick::ray_aabb;
use ddc_render::{Aabb, Scene};
use egui::Pos2;

use ddc_geom::nurbs::{Curve, Surface};

use super::exact::{d, g, Shape3};
use super::math::{ray_segment, seg_seg, Cam};
use super::pick::{Bodies, Cat, Hit3, Query};
use crate::snap_core::{Feat, SnapRef};
use super::plane::WorkPlane;

/// Snap kind; the order is the priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SnapKind {
    Vertex,
    Mid,
    /// Exact curve: midpoint and thirds by arc length.
    Param,
    Intersection,
    /// Exact curve: where a line from the start point touches it.
    Tangent,
    /// Exact curve: foot of the perpendicular from the start point.
    Perp,
    GridX,
    /// Exact curve: nearest point to the cursor.
    Curve,
    Edge,
    /// Exact surface: nearest point on an iso-curve through a knot or quarter parameter.
    Iso,
    GridLine,
    /// Exact surface: the point under the cursor.
    Surface,
    Face,
    Plane,
}

impl SnapKind {
    /// The plan kind this 3D kind stands for; `None` for kinds that exist only in 3D.
    pub fn plan_kind(self) -> Option<crate::snap::SnapKind> {
        use crate::snap::SnapKind as P;
        Some(match self {
            SnapKind::Vertex => P::Endpoint,
            SnapKind::Mid => P::Midpoint,
            SnapKind::Param => P::Param,
            SnapKind::Intersection | SnapKind::GridX => P::Intersection,
            SnapKind::Tangent => P::Tangent,
            SnapKind::Perp => P::Perpendicular,
            SnapKind::Curve => P::Nearest,
            SnapKind::Edge => P::WallAxis,
            SnapKind::GridLine => P::GridAxis,
            SnapKind::Iso | SnapKind::Surface | SnapKind::Face | SnapKind::Plane => return None,
        })
    }
}

pub const ALL: [SnapKind; 14] = [
    SnapKind::Vertex,
    SnapKind::Mid,
    SnapKind::Param,
    SnapKind::Intersection,
    SnapKind::Tangent,
    SnapKind::Perp,
    SnapKind::GridX,
    SnapKind::Curve,
    SnapKind::Edge,
    SnapKind::Iso,
    SnapKind::GridLine,
    SnapKind::Surface,
    SnapKind::Face,
    SnapKind::Plane,
];

impl SnapKind {
    pub fn key(self) -> &'static str {
        match self {
            SnapKind::Vertex => "vertex",
            SnapKind::Mid => "mid",
            SnapKind::Param => "param",
            SnapKind::Intersection => "intersection",
            SnapKind::Tangent => "tangent",
            SnapKind::Perp => "perp",
            SnapKind::GridX => "gridx",
            SnapKind::Curve => "curve",
            SnapKind::Edge => "edge",
            SnapKind::Iso => "iso",
            SnapKind::GridLine => "gridline",
            SnapKind::Surface => "surface",
            SnapKind::Face => "face",
            SnapKind::Plane => "plane",
        }
    }

    pub fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

/// Enabled snap kinds (bit mask).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mask(pub u16);

impl Default for Mask {
    fn default() -> Self {
        Mask((1 << ALL.len()) - 1)
    }
}

impl Mask {
    pub fn has(self, k: SnapKind) -> bool {
        self.0 & k.bit() != 0
    }

    pub fn toggle(&mut self, k: SnapKind) {
        self.0 ^= k.bit();
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snap {
    pub kind: SnapKind,
    pub p: Vec3,
    /// Distance from the cursor on screen, px.
    pub px: f32,
    /// Element and feature the point lies on (walls: the same refs as a plan hit).
    pub refs: [Option<SnapRef>; 2],
}

impl Snap {
    /// The shared plan hit of this point: plan kind, plan position, refs. `None` for kinds that
    /// exist only in 3D (face, plane, iso, surface).
    pub fn plan_hit(&self) -> Option<crate::snap_core::SnapHit> {
        Some(crate::snap_core::SnapHit {
            kind: self.kind.plan_kind()?,
            p: ddc_geom::v3(f64::from(self.p.x), f64::from(self.p.y), f64::from(self.p.z)),
            refs: self.refs,
        })
    }
}

pub struct Input<'a> {
    pub cam: &'a Cam,
    pub pos: Pos2,
    pub plane: Option<&'a WorkPlane>,
    /// Work-plane grid step, m (0 means no rounding).
    pub step: f32,
    /// Capture radius, px.
    pub radius: f32,
    pub mask: Mask,
    /// Model axes: plan endpoints.
    pub grids: &'a [((f64, f64), (f64, f64))],
    /// Bodies we do not snap to (the dragged element).
    pub exclude: &'a [usize],
    /// Cursor ray hits already found this frame (None means find them here).
    pub hits: Option<&'a [Hit3]>,
    /// Scene index and section box.
    pub query: Query<'a>,
    /// Exact curves and surfaces behind bodies.
    pub shapes: &'a [Shape3],
    /// Start point of the current drag or tool, for tangent and perpendicular snaps.
    pub from: Option<Vec3>,
}

pub const RADIUS: f32 = 12.0;
const NEAR_BODIES: usize = 12;
const NEAR_EDGES: usize = 64;

/// All candidates under the cursor by priority.
pub fn candidates(bodies: &mut Bodies, scene: &Scene, inp: &Input) -> Vec<Snap> {
    let cam = inp.cam;
    let ray = cam.ray(inp.pos);
    let mut out: Vec<Snap> = vec![];
    let found;
    let all = match inp.hits {
        Some(h) => h,
        None => {
            found = bodies.hits_in(scene, &ray, &inp.query);
            &found[..]
        }
    };
    // room volumes (rank 1) neither occlude nor provide points
    let hits: Vec<Hit3> = all
        .iter()
        .filter(|h| {
            !inp.exclude.contains(&h.body)
                && bodies.list[h.body].rank == 0
                && !bodies.list[h.body].target.is_some_and(super::pick::is_grid)
        })
        .copied()
        .collect();
    // depth of the visible face: points beyond it are not taken (except axes and the plane)
    let front = hits.iter().map(|h| h.t).fold(f32::INFINITY, f32::min);
    let q = inp.query;
    let visible = |p: Vec3| {
        let d = (p - ray.origin).dot(ray.dir);
        !(d > front * 1.02 + 0.05) && q.keeps(p)
    };
    let push = |out: &mut Vec<Snap>, kind: SnapKind, p: Vec3| {
        if !inp.mask.has(kind) {
            return;
        }
        let px = cam.px_dist(p, inp.pos);
        if px <= inp.radius {
            out.push(Snap { kind, p, px, refs: [None, None] });
        }
    };

    // bodies near the ray: those hit and those whose bounds, inflated by the radius, the ray crosses
    let mut near: Vec<(f32, usize)> = hits.iter().map(|h| (h.t, h.body)).collect();
    let indexed = q.index.is_some_and(|ix| ix.matches(scene));
    let cands: Vec<usize> = if indexed {
        // via the index: bodies at the visible face and at the work-plane point
        let mut at: Vec<Vec3> = hits.first().map(|h| h.point).into_iter().collect();
        at.extend(inp.plane.and_then(|wp| wp.hit(&ray)));
        let mut v = vec![];
        for p in at {
            v.extend(bodies.near(scene, p, cam.m_per_px(p) * inp.radius, &q));
        }
        v.sort_unstable();
        v.dedup();
        v
    } else {
        (0..bodies.list.len()).collect()
    };
    for i in cands {
        let b = &bodies.list[i];
        if inp.exclude.contains(&i)
            || b.target.is_none()
            || b.rank > 0
            || near.iter().any(|n| n.1 == i)
        {
            continue;
        }
        let tol = cam.m_per_px(b.center()) * inp.radius;
        let fat = Aabb {
            min: b.aabb.min - Vec3::splat(tol),
            max: b.aabb.max + Vec3::splat(tol),
        };
        if let Some(t) = ray_aabb(&ray, &fat) {
            near.push((t, i));
        }
    }
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    near.truncate(NEAR_BODIES);

    let mut edges: Vec<[Vec3; 2]> = vec![];
    for &(_, i) in &near {
        let wall = bodies.list[i].target.filter(|t| t.cat == Cat::Wall).map(|t| t.id);
        let tag = |out: &mut Vec<Snap>, from: usize, feat: Feat| {
            if let Some(id) = wall {
                for s in &mut out[from..] {
                    s.refs[0] = Some(SnapRef { target: ddc_model::Target::Wall(id), feat });
                }
            }
        };
        let f = bodies.feature(i, scene);
        for &v in &f.verts {
            if visible(v) {
                let n = out.len();
                push(&mut out, SnapKind::Vertex, v);
                tag(&mut out, n, Feat::End(0));
            }
        }
        for &[a, b] in &f.edges {
            let mid = (a + b) * 0.5;
            if visible(mid) {
                let n = out.len();
                push(&mut out, SnapKind::Mid, mid);
                tag(&mut out, n, Feat::Mid);
            }
            let q = ray_segment(&ray, a, b);
            if visible(q) && cam.px_dist(q, inp.pos) <= inp.radius {
                let n = out.len();
                push(&mut out, SnapKind::Edge, q);
                tag(&mut out, n, Feat::Line);
                if edges.len() < NEAR_EDGES {
                    edges.push([a, b]);
                }
            }
        }
    }
    for i in 0..edges.len() {
        for j in i + 1..edges.len() {
            if let Some(p) = crossing(edges[i], edges[j]) {
                if visible(p) {
                    push(&mut out, SnapKind::Intersection, p);
                }
            }
        }
    }

    if let Some(wp) = inp.plane {
        let traces: Vec<[Vec3; 2]> = inp
            .grids
            .iter()
            .filter_map(|&(a, b)| wp.trace(a, b, 1000.0))
            .collect();
        for &[a, b] in &traces {
            push(&mut out, SnapKind::GridLine, ray_segment(&ray, a, b));
        }
        for i in 0..traces.len() {
            for j in i + 1..traces.len() {
                if let Some(p) = crossing(traces[i], traces[j]) {
                    push(&mut out, SnapKind::GridX, p);
                }
            }
        }
    }

    // exact curves and surfaces
    for sh in inp.shapes {
        let body = bodies.of(sh.target);
        if body.is_some_and(|b| inp.exclude.contains(&b)) {
            continue;
        }
        let c = (sh.aabb.min + sh.aabb.max) * 0.5;
        if !sh.near_ray(&ray, cam.m_per_px(c) * inp.radius) {
            continue;
        }
        for cv in &sh.curves {
            let (_, p, px) = curve_near(cam, inp.pos, cv);
            if px > inp.radius * 4.0 {
                continue;
            }
            if visible(p) {
                push(&mut out, SnapKind::Curve, p);
            }
            if inp.mask.has(SnapKind::Param) {
                for f in [1.0 / 3.0, 0.5, 2.0 / 3.0] {
                    let p = g(cv.point(cv.at_fraction(f)));
                    if visible(p) {
                        push(&mut out, SnapKind::Param, p);
                    }
                }
            }
            if let Some(from) = inp.from {
                if inp.mask.has(SnapKind::Perp) {
                    for t in cv.perpendicular_feet(d(from)) {
                        let p = g(cv.point(t));
                        if visible(p) {
                            push(&mut out, SnapKind::Perp, p);
                        }
                    }
                }
                if inp.mask.has(SnapKind::Tangent) {
                    for t in cv.tangent_points(d(from), 1e-6) {
                        let p = g(cv.point(t));
                        if visible(p) {
                            push(&mut out, SnapKind::Tangent, p);
                        }
                    }
                }
            }
        }
        let (Some(s), Some(b)) = (&sh.surface, body) else {
            continue;
        };
        let Some(h) = hits.iter().find(|h| h.body == b) else {
            continue;
        };
        let foot = s.closest(d(h.point));
        let p = g(foot.point);
        if q.keeps(p) {
            push(&mut out, SnapKind::Surface, p);
        }
        if inp.mask.has(SnapKind::Iso) {
            let (lines_u, lines_v) = iso_params(s);
            let isos = lines_u.iter().filter_map(|&u| s.iso_u(u).ok());
            for cv in isos.chain(lines_v.iter().filter_map(|&v| s.iso_v(v).ok())) {
                let (_, p, _) = curve_near(cam, inp.pos, &cv);
                if visible(p) {
                    push(&mut out, SnapKind::Iso, p);
                }
            }
        }
    }

    if let Some(h) = hits.first() {
        if inp.mask.has(SnapKind::Face) && q.keeps(h.point) {
            out.push(Snap {
                kind: SnapKind::Face,
                p: h.point,
                px: 0.0,
                refs: [None, None],
            });
        }
    }
    if let Some(wp) = inp.plane {
        if let Some(p) = wp.hit(&ray) {
            let p = if inp.step > 0.0 {
                wp.grid_node(p, inp.step)
            } else {
                p
            };
            out.push(Snap {
                kind: SnapKind::Plane,
                p,
                px: cam.px_dist(p, inp.pos),
                refs: [None, None],
            });
        }
    }
    order(&mut out);
    out
}

/// Curve point nearest the cursor on screen: best of a uniform sample, then a golden-section
/// search between its neighbours. Returns (parameter, point, distance in px).
pub fn curve_near(cam: &Cam, pos: Pos2, c: &Curve) -> (f64, Vec3, f32) {
    const N: usize = 96;
    let (a, b) = c.domain();
    let at = |t: f64| {
        let p = g(c.point(t));
        (p, cam.px_dist(p, pos))
    };
    let ts: Vec<f64> = (0..=N).map(|k| a + (b - a) * k as f64 / N as f64).collect();
    let k = (0..=N).min_by(|&i, &j| at(ts[i]).1.total_cmp(&at(ts[j]).1)).unwrap_or(0);
    let (mut lo, mut hi) = (ts[k.saturating_sub(1)], ts[(k + 1).min(N)]);
    let r = 0.5 * (5f64.sqrt() - 1.0);
    for _ in 0..48 {
        let (m1, m2) = (hi - r * (hi - lo), lo + r * (hi - lo));
        if at(m1).1 <= at(m2).1 {
            hi = m2;
        } else {
            lo = m1;
        }
    }
    let t = 0.5 * (lo + hi);
    let (p, px) = at(t);
    (t, p, px)
}

/// Iso-curve parameters of a surface: quarters of each domain plus the interior knots.
fn iso_params(s: &Surface) -> (Vec<f64>, Vec<f64>) {
    let lines = |(a, b): (f64, f64), knots: &[f64]| {
        let mut t: Vec<f64> = (0..=4).map(|k| a + (b - a) * k as f64 / 4.0).collect();
        t.extend(knots.iter().copied().filter(|k| *k > a && *k < b));
        t.sort_by(f64::total_cmp);
        t.dedup_by(|x, y| (*x - *y).abs() < 1e-12);
        t
    };
    (lines(s.domain_u(), s.knots_u()), lines(s.domain_v(), s.knots_v()))
}

/// Intersection of two 3D segments (no more than 1 mm apart), otherwise None.
pub fn crossing(a: [Vec3; 2], b: [Vec3; 2]) -> Option<Vec3> {
    let da = a[1] - a[0];
    let db = b[1] - b[0];
    if da.cross(db).length_squared() < 1e-10 * da.length_squared() * db.length_squared() {
        return None;
    }
    let (p, q) = seg_seg(a[0], a[1], b[0], b[1]);
    ((p - q).length() < 1e-3).then_some((p + q) * 0.5)
}

/// Sort by priority and distance; coincident points collapse into one of the highest kind.
pub fn order(v: &mut Vec<Snap>) {
    v.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.px.total_cmp(&b.px)));
    let mut out: Vec<Snap> = Vec::with_capacity(v.len());
    for s in v.drain(..) {
        if !out.iter().any(|o| (o.p - s.p).length_squared() < 1e-8) {
            out.push(s);
        }
    }
    *v = out;
}

/// The candidate taking Tab cycling into account.
pub fn pick(c: &[Snap], cycle: usize) -> Option<Snap> {
    (!c.is_empty()).then(|| c[cycle % c.len()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit3d::pick::{bar, Body, Cat, Target};
    use egui::{pos2, vec2, Rect};

    fn setup() -> (Bodies, Cam) {
        let (pos, tris) = bar(Vec3::ZERO, Vec3::Z, 1.0, 1.0, Vec3::X);
        let b = Bodies::from_list(vec![Body::own(Target::new(Cat::Column, 1), pos, tris, 0)]);
        let mut c = ddc_render::OrbitCamera::default();
        c.target = Vec3::new(0.0, 0.0, 0.5);
        let cam = Cam::new(&c, Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)));
        (b, cam)
    }

    fn input<'a>(cam: &'a Cam, pos: Pos2, wp: Option<&'a WorkPlane>) -> Input<'a> {
        Input {
            cam,
            pos,
            plane: wp,
            step: 0.1,
            radius: RADIUS,
            mask: Mask::default(),
            grids: &[],
            exclude: &[],
            hits: None,
            query: Query::default(),
            shapes: &[],
            from: None,
        }
    }

    #[test]
    fn hit_carries_refs() {
        let (pos, tris) = bar(Vec3::ZERO, Vec3::Z, 1.0, 1.0, Vec3::X);
        let mut b = Bodies::from_list(vec![Body::own(Target::new(Cat::Wall, 12), pos, tris, 0)]);
        let mut c = ddc_render::OrbitCamera::default();
        c.target = Vec3::new(0.0, 0.0, 0.5);
        let cam = Cam::new(&c, Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)));
        let s = Scene::default();
        let eye = cam.eye;
        let f = b.feature(0, &s).clone();
        let wall = Some(SnapRef { target: ddc_model::Target::Wall(12), feat: Feat::End(0) });
        // the vertex nearest the eye
        let v = f.verts.iter().copied().min_by(|a, c| (*a - eye).length().total_cmp(&(*c - eye).length())).unwrap();
        let got = candidates(&mut b, &s, &input(&cam, cam.to_screen(v).unwrap() + vec2(2.0, 1.0), None));
        assert_eq!(got[0].kind, SnapKind::Vertex);
        assert_eq!(got[0].refs[0], wall);
        // the middle of the nearest edge says "mid" of wall 12
        let m = f.edges.iter().map(|[a, b]| (*a + *b) * 0.5).min_by(|a, c| (*a - eye).length().total_cmp(&(*c - eye).length())).unwrap();
        let got = candidates(&mut b, &s, &input(&cam, cam.to_screen(m).unwrap(), None));
        let mid = got.iter().find(|s| s.kind == SnapKind::Mid && (s.p - m).length() < 1e-4).expect("mid");
        assert_eq!(mid.refs[0].map(|r| (r.target, r.feat)), Some((ddc_model::Target::Wall(12), Feat::Mid)));
        // the shared plan hit keeps the refs and the plan kind; 3D-only kinds have none
        let h = mid.plan_hit().expect("plan hit");
        assert_eq!((h.kind, h.refs[0]), (crate::snap::SnapKind::Midpoint, mid.refs[0]));
        assert!(Snap { kind: SnapKind::Plane, p: Vec3::ZERO, px: 0.0, refs: [None, None] }.plan_hit().is_none());
        // a column carries no wall refs
        let (pos, tris) = bar(Vec3::ZERO, Vec3::Z, 1.0, 1.0, Vec3::X);
        let mut col = Bodies::from_list(vec![Body::own(Target::new(Cat::Column, 1), pos, tris, 0)]);
        let got = candidates(&mut col, &s, &input(&cam, cam.to_screen(v).unwrap() + vec2(2.0, 1.0), None));
        assert!(got.iter().all(|s| s.refs == [None, None]));
    }

    /// A query over many bodies stays cheap: 400 bars, the cursor on one of them.
    #[test]
    fn query_budget_many_bodies() {
        let mut list = vec![];
        for i in 0..400 {
            let (x, y) = ((i % 20) as f32 * 3.0, (i / 20) as f32 * 3.0);
            let (pos, tris) = bar(Vec3::new(x, y, 0.0), Vec3::new(x, y, 3.0), 1.0, 1.0, Vec3::X);
            list.push(Body::own(Target::new(Cat::Wall, i + 1), pos, tris, 0));
        }
        let mut b = Bodies::from_list(list);
        let mut c = ddc_render::OrbitCamera::default();
        c.target = Vec3::new(30.0, 30.0, 1.5);
        let cam = Cam::new(&c, Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)));
        let s = Scene::default();
        let on = pos2(400.0, 300.0);
        let t = std::time::Instant::now();
        for _ in 0..50 {
            candidates(&mut b, &s, &input(&cam, on, None));
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / 50.0;
        let budget = if cfg!(debug_assertions) { 80.0 } else { 4.0 };
        assert!(ms <= budget, "{ms} ms per query, budget {budget}");
    }

    #[test]
    fn plan_kinds_and_profile_mask() {
        use crate::snap::{SnapKind as P, SnapProfile};
        assert_eq!(SnapKind::Vertex.plan_kind(), Some(P::Endpoint));
        assert_eq!(SnapKind::Mid.plan_kind(), Some(P::Midpoint));
        assert_eq!(SnapKind::GridLine.plan_kind(), Some(P::GridAxis));
        assert_eq!(SnapKind::Plane.plan_kind(), None);
        // a column profile has no wall axes: the 3D edge snap goes, vertices and the plane stay
        let m = SnapProfile::COLUMN.mask3(Mask(u16::MAX));
        assert!(m.has(SnapKind::Vertex) && m.has(SnapKind::Plane) && m.has(SnapKind::Face));
        assert!(!m.has(SnapKind::Edge) && !m.has(SnapKind::Perp));
        let all = SnapProfile::ALL.mask3(Mask(u16::MAX));
        assert!(ALL.iter().all(|k| all.has(*k)));
    }

    #[test]
    fn grid_body_not_snap_target() {
        let code = crate::editor::structx::code_of("grid").unwrap();
        let (pos, tris) = bar(Vec3::new(-2.0, 0.0, 0.0), Vec3::new(2.0, 0.0, 0.0), 0.5, 0.5, Vec3::Z);
        let b = Bodies::from_list(vec![Body::own(Target::new(Cat::Ext(code), 3), pos, tris, 0)]);
        let (_, cam) = setup();
        let mut b = b;
        let on = cam.to_screen(Vec3::ZERO).unwrap();
        let s = Scene::default();
        let r = cam.ray(on);
        assert!(!b.hits_in(&s, &r, &Query::default()).is_empty(), "the ribbon is hit");
        let out = candidates(&mut b, &s, &input(&cam, on, None));
        assert!(
            out.iter().all(|c| c.kind != SnapKind::Face),
            "no face snap on a grid body"
        );
    }

    #[test]
    fn room_volume_does_not_occlude_and_clip_drops_points() {
        let (pos, tris) = bar(Vec3::ZERO, Vec3::Z, 1.0, 1.0, Vec3::X);
        // a room volume around and above the column
        let (rp, rt) = bar(
            Vec3::new(0.0, 0.0, -0.1),
            Vec3::new(0.0, 0.0, 3.0),
            6.0,
            6.0,
            Vec3::X,
        );
        let mut b = Bodies::from_list(vec![
            Body::own(Target::new(Cat::Column, 1), pos, tris, 0),
            Body::own(Target::new(Cat::Room, 2), rp, rt, 1),
        ]);
        let mut c = ddc_render::OrbitCamera::default();
        c.target = Vec3::new(0.0, 0.0, 0.5);
        let cam = Cam::new(&c, Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)));
        let s = Scene::default();
        let eye = cam.eye;
        let top = b
            .feature(0, &s)
            .verts
            .clone()
            .into_iter()
            .filter(|p| p.z > 0.5)
            .min_by(|a, c| (*a - eye).length().total_cmp(&(*c - eye).length()))
            .unwrap();
        let at = cam.to_screen(top).unwrap() + vec2(2.0, 1.0);
        let got = candidates(&mut b, &s, &input(&cam, at, None));
        assert_eq!(
            got[0].kind,
            SnapKind::Vertex,
            "the room in front does not hide the column"
        );
        assert!((got[0].p - top).length() < 1e-4);
        assert!(
            got.iter().all(|s| s.p.z < 2.9),
            "no points on the room volume"
        );
        // the section is below the column top: its upper points are discarded
        let mut inp = input(&cam, at, None);
        inp.query.clip = Some(Aabb {
            min: Vec3::splat(-5.0),
            max: Vec3::new(5.0, 5.0, 0.5),
        });
        let got = candidates(&mut b, &s, &inp);
        assert!(got.iter().all(|s| s.p.z <= 0.5 + 1e-3), "{got:?}");
    }

    fn circle(c: Vec3, r: f64) -> Curve {
        let x = ddc_geom::v3(1.0, 0.0, 0.0);
        let y = ddc_geom::v3(0.0, 1.0, 0.0);
        Curve::circle(d(c), x, y, r).unwrap()
    }

    #[test]
    fn tangent_from_the_start_point_then_nearest_on_curve_by_tab() {
        let (_, cam) = setup();
        let mut b = Bodies::from_list(vec![]);
        let s = Scene::default();
        let (r, dist) = (0.8f64, 2.0f64);
        let sh = [Shape3::new(Target::new(Cat::Column, 7), vec![circle(Vec3::new(0.0, 0.0, 1.5), r)], None)];
        let (tx, ty) = (r * r / dist, r * (1.0 - r * r / (dist * dist)).sqrt());
        let want = Vec3::new(tx as f32, ty as f32, 1.5);
        let at = cam.to_screen(want).unwrap() + vec2(1.0, 1.0);
        let mut inp = input(&cam, at, None);
        inp.shapes = &sh;
        inp.from = Some(Vec3::new(dist as f32, 0.0, 1.5));
        let c = candidates(&mut b, &s, &inp);
        assert_eq!(c[0].kind, SnapKind::Tangent, "{c:?}");
        assert!((c[0].p - want).length() < 1e-5, "{:?}", c[0].p);
        // Tab: the nearest point on the curve next, then back to the tangent
        assert_eq!(pick(&c, 1).map(|s| s.kind), Some(SnapKind::Curve), "{c:?}");
        let on = pick(&c, 1).unwrap().p;
        assert!(((on.x as f64).hypot(on.y as f64) - r).abs() < 1e-5 && (on.z - 1.5).abs() < 1e-6);
        assert_eq!(pick(&c, c.len()), Some(c[0]));
        // without a start point there is no tangent
        inp.from = None;
        let c = candidates(&mut b, &s, &inp);
        assert!(c.iter().all(|s| s.kind != SnapKind::Tangent) && c[0].kind == SnapKind::Curve);
        // a third of the circle by arc length
        let third = Vec3::new((r * (2.0 * std::f64::consts::PI / 3.0).cos()) as f32, (r * (2.0 * std::f64::consts::PI / 3.0).sin()) as f32, 1.5);
        let mut inp = input(&cam, cam.to_screen(third).unwrap() + vec2(2.0, 0.0), None);
        inp.shapes = &sh;
        let c = candidates(&mut b, &s, &inp);
        assert_eq!(c[0].kind, SnapKind::Param, "{c:?}");
        assert!((c[0].p - third).length() < 1e-5);
    }

    #[test]
    fn surface_point_and_iso_curve_on_an_exact_patch() {
        let (mut b, cam) = setup();
        let s = Scene::default();
        // the exact patch on the top face of the column (u along x, v along y)
        let p = |x: f64, y: f64| ddc_geom::v3(x, y, 1.0);
        let k = vec![0.0, 0.0, 1.0, 1.0];
        let sf = Surface::new(1, 1, k.clone(), k, 2, 2, vec![p(-0.5, -0.5), p(-0.5, 0.5), p(0.5, -0.5), p(0.5, 0.5)], vec![1.0; 4]).unwrap();
        let sh = [Shape3::new(Target::new(Cat::Column, 1), vec![], Some(sf))];
        let base = Vec3::new(0.0, 0.1, 1.0);
        let off = cam.m_per_px(base) * 3.0;
        let target = base + Vec3::new(off, 0.0, 0.0);
        let at = cam.to_screen(target).unwrap();
        let mut inp = input(&cam, at, None);
        inp.shapes = &sh;
        let c = candidates(&mut b, &s, &inp);
        let surf = c.iter().position(|s| s.kind == SnapKind::Surface).expect("surface snap");
        assert!((c[surf].p.z - 1.0).abs() < 1e-6 && (c[surf].p - target).length() < off, "{:?}", c[surf]);
        let iso = c.iter().find(|s| s.kind == SnapKind::Iso).expect("iso snap");
        assert!(iso.p.x.abs() < 1e-5 && (iso.p.z - 1.0).abs() < 1e-6, "{iso:?}");
        if let Some(f) = c.iter().position(|s| s.kind == SnapKind::Face) {
            assert!(surf < f);
        }
        // Tab walks every candidate and wraps
        let kinds: Vec<SnapKind> = (0..c.len()).map(|k| pick(&c, k).unwrap().kind).collect();
        assert!(kinds.contains(&SnapKind::Iso) && kinds.contains(&SnapKind::Surface));
        assert_eq!(pick(&c, c.len()), Some(c[0]));
        // the tangent work plane of the patch
        let (q, n) = sh[0].tangent_plane(target, Vec3::Z).unwrap();
        assert!((q.z - 1.0).abs() < 1e-6 && (n - Vec3::Z).length() < 1e-6);
        // the dragged element gives no exact snaps
        let ex = [0usize];
        inp.exclude = &ex;
        let c = candidates(&mut b, &s, &inp);
        assert!(c.iter().all(|s| s.kind != SnapKind::Surface && s.kind != SnapKind::Iso));
    }

    #[test]
    fn vertex_wins_over_edge_and_face() {
        let (mut b, cam) = setup();
        let s = Scene::default();
        // the top vertex facing the camera
        let v = b.feature(0, &s).verts.clone();
        let eye = cam.eye;
        let top = v
            .into_iter()
            .filter(|p| p.z > 0.5)
            .min_by(|a, c| (*a - eye).length().total_cmp(&(*c - eye).length()))
            .unwrap();
        let at = cam.to_screen(top).unwrap() + vec2(2.0, 1.0);
        let c = candidates(&mut b, &s, &input(&cam, at, None));
        assert_eq!(c[0].kind, SnapKind::Vertex);
        assert!((c[0].p - top).length() < 1e-4);
        assert!(c.iter().any(|s| s.kind == SnapKind::Edge));
        // Tab advances through the list and wraps around
        assert_eq!(pick(&c, 0), Some(c[0]));
        assert_eq!(pick(&c, c.len()), Some(c[0]));
    }

    #[test]
    fn midpoint_and_plane_fallback() {
        let (mut b, cam) = setup();
        let s = Scene::default();
        let f = b.feature(0, &s).clone();
        let eye = cam.eye;
        let e = f
            .edges
            .iter()
            .filter(|e| e[0].z > 0.5 && e[1].z > 0.5)
            .min_by(|a, c| {
                let m = |e: &[Vec3; 2]| ((e[0] + e[1]) * 0.5 - eye).length();
                m(a).total_cmp(&m(c))
            })
            .copied()
            .unwrap();
        let mid = (e[0] + e[1]) * 0.5;
        let c = candidates(&mut b, &s, &input(&cam, cam.to_screen(mid).unwrap(), None));
        assert_eq!(c[0].kind, SnapKind::Mid);
        // far from a body: only the plane point, rounded to the step
        let wp = WorkPlane::level(0, 0.0);
        let far = cam.to_screen(Vec3::new(2.03, -1.98, 0.0)).unwrap();
        let c = candidates(&mut b, &s, &input(&cam, far, Some(&wp)));
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].kind, SnapKind::Plane);
        assert!((c[0].p - Vec3::new(2.0, -2.0, 0.0)).length() < 1e-4);
    }

    #[test]
    fn grid_crossing_on_level_plane() {
        let (mut b, cam) = setup();
        let s = Scene::default();
        let wp = WorkPlane::level(0, 0.0);
        let grids = [((2.0, -5.0), (2.0, 5.0)), ((-5.0, -2.0), (5.0, -2.0))];
        let at = cam.to_screen(Vec3::new(2.0, -2.0, 0.0)).unwrap() + vec2(3.0, -2.0);
        let mut inp = input(&cam, at, Some(&wp));
        inp.grids = &grids;
        let c = candidates(&mut b, &s, &inp);
        assert_eq!(c[0].kind, SnapKind::GridX);
        assert!((c[0].p - Vec3::new(2.0, -2.0, 0.0)).length() < 1e-4);
        assert!(c.iter().any(|s| s.kind == SnapKind::GridLine));
        // a disabled kind is not offered
        inp.mask.toggle(SnapKind::GridX);
        let c = candidates(&mut b, &s, &inp);
        assert!(c.iter().all(|s| s.kind != SnapKind::GridX));
    }

    #[test]
    fn crossing_math_and_dedupe() {
        let a = [Vec3::ZERO, Vec3::new(2.0, 0.0, 0.0)];
        let b = [Vec3::new(1.0, -1.0, 0.0), Vec3::new(1.0, 1.0, 0.0)];
        assert!((crossing(a, b).unwrap() - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-5);
        let skew = [Vec3::new(1.0, -1.0, 1.0), Vec3::new(1.0, 1.0, 1.0)];
        assert!(crossing(a, skew).is_none());
        assert!(
            crossing(a, [Vec3::Y, Vec3::new(2.0, 1.0, 0.0)]).is_none(),
            "parallel"
        );
        let p = Vec3::ONE;
        let mut v = vec![
            Snap {
                kind: SnapKind::Edge,
                p,
                px: 1.0,
                refs: [None, None],
            },
            Snap {
                kind: SnapKind::Vertex,
                p,
                px: 3.0,
                refs: [None, None],
            },
            Snap {
                kind: SnapKind::Plane,
                p: Vec3::ZERO,
                px: 0.0,
                refs: [None, None],
            },
        ];
        order(&mut v);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].kind, SnapKind::Vertex);
    }
}

#[cfg(test)]
mod budget {
    use super::*;
    use crate::edit3d::pick::{bar, Body, Target};
    use eframe::egui::{pos2, vec2, Rect};

    /// 5000 bars in a 100 x 50 grid; the cursor sits on one of them. Release builds only.
    #[test]
    fn query_budget_5000_bodies() {
        if cfg!(debug_assertions) {
            return;
        }
        let mut list = vec![];
        for i in 0..5000 {
            let (x, y) = ((i % 100) as f32 * 3.0, (i / 100) as f32 * 3.0);
            let (pos, tris) = bar(Vec3::new(x, y, 0.0), Vec3::new(x, y, 3.0), 1.0, 1.0, Vec3::X);
            list.push(Body::own(Target::new(Cat::Wall, i + 1), pos, tris, 0));
        }
        let mut b = Bodies::from_list(list);
        let mut c = ddc_render::OrbitCamera::default();
        c.target = Vec3::new(150.0, 75.0, 1.5);
        let cam = Cam::new(&c, Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)));
        let s = Scene::default();
        let inp = Input {
            cam: &cam,
            pos: pos2(400.0, 300.0),
            plane: None,
            step: 0.1,
            radius: RADIUS,
            mask: Mask::default(),
            grids: &[],
            exclude: &[],
            hits: None,
            query: Query::default(),
            shapes: &[],
            from: None,
        };
        candidates(&mut b, &s, &inp);
        let mut worst = 0.0f64;
        for _ in 0..20 {
            let t = std::time::Instant::now();
            candidates(&mut b, &s, &inp);
            worst = worst.max(t.elapsed().as_secs_f64() * 1000.0);
        }
        assert!(worst <= 4.0, "{worst} ms per query on 5000 bodies, budget 4 ms");
    }
}
