// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Host under the pointer: the element the ray meets first, so a hosted family
//! (door, window, opening) is placed by the face it was clicked on in 3D or in a section, and
//! by the wall axis in plan. Straight walls as boxes in f64; arc walls keep the plan route.
use super::pointer::{Feat, HostHit, Pointer, Ray3, ViewKind};
use super::adapter::ViewAdapter;
use super::scope::Target;
use crate::editor::{Editor, Wall};
use crate::grips::Kind;
use ddc_geom::{v2, v3, V2, V3};

/// Local frame of a straight wall: origin `a` at its base, `u` along, `n` left normal.
struct Frame {
    o: V3,
    u: V3,
    n: V3,
    len: f64,
    half: f64,
    h: f64,
}

fn frame(ed: &Editor, w: &Wall) -> Option<Frame> {
    if w.bulge.abs() > 1e-9 {
        return None;
    }
    let d = w.b - w.a;
    let len = d.len();
    let t = ed.type_by_id(w.wall_type).map_or(0.0, |t| t.thickness());
    if len < 1e-9 || t <= 0.0 || w.height <= 0.0 {
        return None;
    }
    let u = v3(d.x / len, d.y / len, 0.0);
    Some(Frame {
        o: v3(w.a.x, w.a.y, ed.elevation + w.base_offset),
        u,
        n: v3(-u.y, u.x, 0.0),
        len,
        half: t / 2.0,
        h: w.height,
    })
}

/// Entry of the ray into the wall box: (t, feature, outward normal).
fn enter(f: &Frame, r: &Ray3) -> Option<(f64, Feat, V3)> {
    let q = r.o - f.o;
    let z = v3(0.0, 0.0, 1.0);
    // slabs: along, across, up
    let axes = [(f.u, 0.0, f.len), (f.n, -f.half, f.half), (z, 0.0, f.h)];
    let (mut t0, mut t1) = (f64::NEG_INFINITY, f64::INFINITY);
    let mut hit = None;
    for (k, (ax, lo, hi)) in axes.iter().enumerate() {
        let (o, d) = (q.dot(*ax), r.d.dot(*ax));
        if d.abs() < 1e-12 {
            if o < *lo || o > *hi {
                return None;
            }
            continue;
        }
        let (a, b) = ((lo - o) / d, (hi - o) / d);
        let (near, far) = if a < b { (a, b) } else { (b, a) };
        if near > t0 {
            t0 = near;
            // the face crossed on entry looks against the ray
            hit = Some((k, if d > 0.0 { -1.0 } else { 1.0 }));
        }
        t1 = t1.min(far);
    }
    if t0 > t1 || t1 < 0.0 || t0 < 0.0 {
        return None;
    }
    let (k, sg) = hit?;
    Some(match k {
        1 => (t0, Feat::Side(sg as i8), f.n * sg),
        0 => (t0, Feat::Face, f.u * sg),
        _ => (t0, Feat::Face, z * sg),
    })
}

/// First wall the ray enters (ray starting outside every wall).
pub fn wall_hit(ed: &Editor, r: &Ray3) -> Option<HostHit> {
    let mut best: Option<(f64, HostHit)> = None;
    for w in &ed.walls {
        let Some(f) = frame(ed, w) else { continue };
        let Some((t, feat, n)) = enter(&f, r) else { continue };
        if best.as_ref().is_none_or(|b| t < b.0) {
            let p = r.at(t);
            let axis = p - f.n * (p - f.o).dot(f.n);
            let h = HostHit { target: Target::Elem(Kind::Wall, w.id), feat, p, n, axis };
            best = Some((t, h));
        }
    }
    best.map(|b| b.1)
}

/// Plan host: the wall whose axis is nearest within `radius`, the side by the cursor.
pub fn wall_near(ed: &Editor, p: V2, radius: f64) -> Option<HostHit> {
    let mut best: Option<(f64, HostHit)> = None;
    for w in &ed.walls {
        let Some(f) = frame(ed, w) else { continue };
        let q = p - v2(f.o.x, f.o.y);
        let (s, o) = (q.x * f.u.x + q.y * f.u.y, q.x * f.n.x + q.y * f.n.y);
        if !(0.0..=f.len).contains(&s) || o.abs() > radius {
            continue;
        }
        if best.as_ref().is_none_or(|b| o.abs() < b.0) {
            let sg = if o < 0.0 { -1.0 } else { 1.0 };
            let at = v3(p.x, p.y, f.o.z);
            let axis = at - f.n * o;
            let h = HostHit { target: Target::Elem(Kind::Wall, w.id), feat: Feat::Side(sg as i8), p: at, n: f.n * sg, axis };
            best = Some((o.abs(), h));
        }
    }
    best.map(|b| b.1)
}

/// `p` with the host of its route: the ray in 3D and section, the wall axis in plan. Only a
/// hosted family looks for a host; other tools keep the work-plane point.
pub fn attach(ed: &Editor, mut p: Pointer) -> Pointer {
    if !ed.opening_mode {
        return p;
    }
    p.host = match p.view {
        ViewKind::Plan { .. } => p.plan().and_then(|q| wall_near(ed, q, ed.otool.radius)),
        _ => wall_hit(ed, &p.ray),
    };
    p
}

/// Along the wall axis and height above the wall base of a host point.
pub fn wall_local(ed: &Editor, wall: i64, p: V3) -> Option<(f64, f64)> {
    let w = ed.walls.iter().find(|w| w.id == wall)?;
    let f = frame(ed, w)?;
    let q = p - f.o;
    Some((q.dot(f.u), q.z))
}

/// The point on the face of the wall under the plan point `p` that looks toward `eye`, at
/// `z` above the wall base: what a user aims at in 3D to place on that wall.
pub fn face_point(ed: &Editor, p: V2, eye: V3, z: f64) -> Option<V3> {
    for w in &ed.walls {
        let Some(f) = frame(ed, w) else { continue };
        let q = p - v2(f.o.x, f.o.y);
        let (s, o) = (q.x * f.u.x + q.y * f.u.y, q.x * f.n.x + q.y * f.n.y);
        if !(0.0..=f.len).contains(&s) || o.abs() > f.half + 1e-9 {
            continue;
        }
        let side = if (eye - f.o).dot(f.n) < 0.0 { -1.0 } else { 1.0 };
        return Some(f.o + f.u * s + f.n * (side * f.half) + v3(0.0, 0.0, z.clamp(0.0, f.h)));
    }
    None
}

/// The screen pointer of `ad` that meets the wall under the plan point `q` at the same place
/// along its axis: the top of the axis (seen from above), else its middle height. What a
/// script click on a wall becomes in a 3D or section view.
pub fn aim(ed: &Editor, ad: &dyn ViewAdapter, q: V2) -> Option<Pointer> {
    let h = wall_near(ed, q, ed.otool.radius)?;
    let w = ed.walls.iter().find(|w| Target::Elem(Kind::Wall, w.id) == h.target)?;
    let f = frame(ed, w)?;
    let want = v2(h.axis.x, h.axis.y);
    [f.h, f.h / 2.0].into_iter().find_map(|z| {
        let p = attach(ed, ad.pointer_at(ad.project_at(h.axis + v3(0.0, 0.0, z))?));
        let hit = p.host?;
        let same = hit.target == h.target && (v2(hit.axis.x, hit.axis.y) - want).len() < 1e-6;
        same.then_some(p)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interact::adapter::{SectionAdapter, View3dAdapter};
    use crate::interact::pointer::WorkPlane;

    fn editor() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.commit_walls(vec![(v2(0.0, 0.0), v2(6.0, 0.0))]);
        ed
    }

    #[test]
    fn ray_meets_the_wall_side_first() {
        let ed = editor();
        let half = ed.current_type().unwrap().thickness() / 2.0;
        // from the south, level with the middle of the wall
        let r = Ray3 { o: v3(2.0, -5.0, 1.0), d: v3(0.0, 1.0, 0.0) };
        let h = wall_hit(&ed, &r).unwrap();
        assert_eq!(h.feat, Feat::Side(-1));
        assert!((h.p - v3(2.0, -half, 1.0)).len() < 1e-9);
        assert!((h.n - v3(0.0, -1.0, 0.0)).len() < 1e-12);
        assert_eq!(wall_local(&ed, ed.walls[0].id, h.p).map(|l| (l.0 * 1e6).round() / 1e6), Some(2.0));
        // past the end: no host
        assert!(wall_hit(&ed, &Ray3 { o: v3(7.0, -5.0, 1.0), d: v3(0.0, 1.0, 0.0) }).is_none());
    }

    #[test]
    fn face_point_is_what_the_3d_ray_meets() {
        let ed = editor();
        let cam = ddc_render::OrbitCamera { distance: 40.0, ..Default::default() };
        let ad = View3dAdapter::new(&cam, [0.0, 0.0], [1600.0, 1200.0], 0.0, WorkPlane::level(ed.level, 0.0));
        let mut ed = ed;
        ed.start_opening(crate::opening_tool::OpeningType::window());
        let w = face_point(&ed, v2(3.0, 0.03), ad.eye, 1.0).unwrap();
        let p = attach(&ed, ad.pointer_at(ad.project_at(w).unwrap()));
        let h = p.host.expect("wall under the pointer");
        assert!((h.p - w).len() < 1e-9, "{:?} vs {w:?}", h.p);
        let (along, up) = wall_local(&ed, ed.walls[0].id, h.p).unwrap();
        assert!((along - 3.0).abs() < 1e-9 && (up - 1.0).abs() < 1e-9);
        // plan tools see the axis point under it
        assert_eq!(p.plan(), Some(v2(3.0, 0.0)));
    }

    #[test]
    fn section_click_on_the_far_wall_hosts_it() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.commit_walls(vec![(v2(0.0, 0.0), v2(6.0, 0.0)), (v2(6.0, 0.0), v2(6.0, 4.0))]);
        ed.start_opening(crate::opening_tool::OpeningType::window());
        let east = ed.walls[1].id;
        // a cut across the building looking at the east wall, one of the two directions
        let hit = [(v2(3.0, -1.0), v2(3.0, 5.0)), (v2(3.0, 5.0), v2(3.0, -1.0))].into_iter().find_map(|(a, b)| {
            let ad = SectionAdapter::new(1, a, b, v2(0.0, 0.0), 100.0, [1600.0, 1200.0])?;
            aim(&ed, &ad, v2(6.0, 2.0))
        });
        let h = hit.expect("east wall under a section pointer").host.unwrap();
        assert_eq!(h.target, Target::Elem(Kind::Wall, east));
        let (along, _) = wall_local(&ed, east, h.p).unwrap();
        assert!((along - 2.0).abs() < 1e-6, "{along}");
    }
}
