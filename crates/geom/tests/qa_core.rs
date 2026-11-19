// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! QA: previously untested public items of ddc-geom.
use ddc_geom::arc::{axis_length, edge_mid, ArcAxis};
use ddc_geom::csg::{ftag, tag_op, Body, BoolOp};
use ddc_geom::csg_check as _;
use ddc_geom::csv::is_number;
use ddc_geom::math::{v2, v3, Tol, V2, V3};
use ddc_geom::nurbs::Surface;
use ddc_geom::poly::{clip_half, crossings};
use ddc_geom::predicates::{on_segment, segments_meet};
use ddc_geom::profile_cat::Unit;
use ddc_geom::rooms::{interior_point, Region};
use ddc_geom::slope::Plane as Slope;
use ddc_geom::sweep::SweepKind;
use ddc_geom::{Profile, Solid};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

fn rect(w: f64, h: f64) -> Vec<V2> {
    vec![v2(0., 0.), v2(w, 0.), v2(w, h), v2(0., h)]
}

fn cube(s: f64, tag: u32) -> Body {
    let p = Profile::new(rect(s, s), vec![], Tol::default()).unwrap();
    let m = Solid::extrude(&p, s, Tol::default()).triangulate().unwrap();
    Body::from_mesh(&m, tag)
}

// ---- math ----
#[test]
fn v2_v3_len() {
    assert_eq!(V2::len(v2(3.0, 4.0)), 5.0);
    assert_eq!(v2(0.0, 0.0).len(), 0.0);
    assert!(v2(f64::NAN, 1.0).len().is_nan());
    assert_eq!(V3::len(v3(2.0, 3.0, 6.0)), 7.0);
    assert!(v3(1e200, 1e200, 0.0).len().is_infinite() || v3(1e200, 1e200, 0.0).len() > 1e200);
    let n = v3(0.0, 0.0, 5.0).normalized();
    assert_eq!(n, v3(0.0, 0.0, 1.0));
    // zero vector normalizes to NaN (documented hazard, callers must guard)
    let z = v3(0.0, 0.0, 0.0).normalized();
    assert!(z.x.is_nan());
}

// ---- slope ----
#[test]
fn slope_plane_lowest_flat_and_area_factor() {
    let ring = rect(6.0, 4.0);
    let pl = Slope::lowest_on(&ring, 0.1, 0.0).unwrap();
    assert_eq!(pl.p0, v2(0.0, 0.0));
    assert!(ring.iter().all(|&p| pl.rise(p) >= -1e-15));
    // independent of the starting vertex
    let rot: Vec<V2> = ring.iter().cycle().skip(2).take(4).copied().collect();
    let pl2 = Slope::lowest_on(&rot, 0.1, 0.0).unwrap();
    for &p in &ring {
        assert_eq!(pl2.rise(p), pl.rise(p), "same plane from any starting vertex");
    }
    assert!(Slope::lowest_on(&[], 0.1, 0.0).is_none());
    assert!(pl.is_flat() == false);
    let flat = Slope::lowest_on(&ring, 0.0, 0.0).unwrap();
    assert!(flat.is_flat());
    assert_eq!(flat.area_factor(), 1.0);
    let steep = Slope::lowest_on(&ring, 3.0, 4.0).unwrap();
    assert!((steep.area_factor() - 26f64.sqrt()).abs() < 1e-15);
}

// ---- predicates ----
#[test]
fn on_segment_and_segments_meet() {
    let (a, b) = (v2(0., 0.), v2(4., 0.));
    assert!(on_segment(a, b, v2(2., 0.)));
    assert!(on_segment(a, b, a) && on_segment(a, b, b), "closed segment");
    assert!(!on_segment(a, b, v2(5., 0.)), "collinear but beyond");
    assert!(!on_segment(a, b, v2(2., 1e-300)));
    assert!(on_segment(a, a, a), "degenerate segment");
    assert!(segments_meet(a, b, v2(2., -1.), v2(2., 1.)), "cross");
    assert!(segments_meet(a, b, v2(4., 0.), v2(6., 3.)), "touch at end");
    assert!(segments_meet(a, b, v2(2., 0.), v2(8., 0.)), "overlap");
    assert!(!segments_meet(a, b, v2(5., 0.), v2(8., 0.)), "collinear disjoint");
    assert!(!segments_meet(a, b, v2(0., 1.), v2(4., 1.)), "parallel");
    // symmetry over random segments, fixed seed
    let mut r = Lcg(5);
    for _ in 0..500 {
        let mut p = || v2((r.next() * 8.0).floor(), (r.next() * 8.0).floor());
        let (p0, p1, p2, p3) = (p(), p(), p(), p());
        assert_eq!(segments_meet(p0, p1, p2, p3), segments_meet(p2, p3, p0, p1));
        assert_eq!(segments_meet(p0, p1, p2, p3), segments_meet(p1, p0, p3, p2));
    }
}

// ---- poly ----
#[test]
fn clip_half_cases() {
    let sq = rect(4.0, 4.0);
    // keep x <= 2
    let half = clip_half(&sq, v2(1.0, 0.0), 2.0);
    let area = |r: &[V2]| {
        (0..r.len())
            .map(|i| r[i].cross(r[(i + 1) % r.len()]))
            .sum::<f64>()
            / 2.0
    };
    assert!((area(&half) - 8.0).abs() < 1e-12);
    assert_eq!(clip_half(&sq, v2(1.0, 0.0), 10.0).len(), 4, "all kept");
    assert!(clip_half(&sq, v2(1.0, 0.0), -1.0).is_empty(), "all cut");
    assert!(clip_half(&[], v2(1.0, 0.0), 0.0).is_empty());
    // complementary halves add up
    let other = clip_half(&sq, v2(-1.0, 0.0), -2.0);
    assert!((area(&half) + area(&other) - 16.0).abs() < 1e-12);
}

#[test]
fn crossings_cases() {
    let ring = rect(4.0, 4.0);
    let ts = crossings(v2(-2., 2.), v2(6., 2.), &[ring.clone()]);
    assert_eq!(ts.len(), 4);
    assert!((ts[1] - 0.25).abs() < 1e-12 && (ts[2] - 0.75).abs() < 1e-12);
    assert!(ts.windows(2).all(|w| w[0] < w[1]), "ascending, unique");
    assert_eq!(crossings(v2(0., 0.), v2(1., 0.), &[]), vec![0.0, 1.0]);
    assert_eq!(crossings(v2(0., 0.), v2(1., 0.), &[vec![]]), vec![0.0, 1.0]);
    let par = crossings(v2(0., -1.), v2(4., -1.), &[ring]);
    assert_eq!(par, vec![0.0, 1.0], "no crossing");
}

// ---- csv ----
#[test]
fn csv_is_number() {
    for s in ["-5", "+2.5", "-1,5", "-1 234,5", "-1\u{a0}234,5", "+0"] {
        assert!(is_number(s), "{s}");
    }
    for s in ["", "5", "-", "+", "abc", "-5a", "--5", "1,5", "+-1", "-1.2.3x"] {
        assert!(!is_number(s), "{s}");
    }
}

// ---- rooms ----
#[test]
fn region_contains_and_interior_point() {
    let outer = rect(10.0, 10.0);
    let hole: Vec<V2> = vec![v2(4., 4.), v2(4., 6.), v2(6., 6.), v2(6., 4.)];
    let reg = Region {
        outer: outer.clone(),
        holes: vec![hole.clone()],
        area: 96.0,
        perimeter: 48.0,
    };
    assert!(Region::contains(&reg, v2(1., 1.)));
    assert!(!reg.contains(v2(5., 5.)), "inside hole");
    assert!(!reg.contains(v2(0., 5.)), "on boundary is not strictly inside");
    assert!(!reg.contains(v2(20., 5.)));
    let p = interior_point(&[&outer, &hole]).unwrap();
    assert!(reg.contains(p), "{p:?}");
    let p0 = interior_point(&[&outer]).unwrap();
    assert!(p0.x > 0.0 && p0.x < 10.0 && p0.y > 0.0 && p0.y < 10.0);
    assert!(interior_point(&[]).is_none());
    let line = [v2(0., 0.), v2(5., 0.)];
    assert!(interior_point(&[&line]).is_none(), "zero-height ring");
}

// ---- brep ----
#[test]
fn solid_translated_and_loop_verts() {
    let p = Profile::new(rect(2.0, 3.0), vec![], Tol::default()).unwrap();
    let s = Solid::extrude(&p, 1.0, Tol::default());
    let v0 = s.volume();
    let zs: Vec<f64> = s.verts.iter().map(|v| v.z).collect();
    let t = s.clone().translated_z(5.0);
    assert!((t.volume() - v0).abs() < 1e-12, "translation keeps volume");
    for (a, b) in zs.iter().zip(t.verts.iter()) {
        assert_eq!(a + 5.0, b.z);
    }
    assert_eq!(s.clone().translated_z(0.0).verts, s.verts);
    // every face loop closes and visits its own vertices
    for f in &s.faces {
        for &l in &f.loops {
            let vs = s.loop_verts(l);
            assert!(vs.len() >= 3);
            assert!(vs.iter().all(|&v| (v as usize) < s.verts.len()));
            assert_eq!(vs.len(), s.loop_points(l).len());
        }
    }
}

// ---- arc ----
#[test]
fn arc_axis_accessors_consistent() {
    let t = Tol::default();
    let ax = ArcAxis::new(v2(0., 0.), v2(4., 0.), 0.5, t).unwrap();
    assert_eq!(ax.sigma(), ax.sweep.signum());
    let len = ax.length();
    // endpoints
    assert!((ax.point_at(0.0) - ax.a).len() < 1e-9);
    assert!((ax.point_at(len) - ax.b).len() < 1e-9);
    // angle_at / tangent_at / param_of
    assert!((ax.angle_at(0.0) - ax.start).abs() < 1e-12);
    let mut r = Lcg(11);
    for _ in 0..100 {
        let s = r.next() * len;
        let p = ax.point_at(s);
        assert!((p - ax.center).len() - ax.radius < 1e-9);
        assert!((ax.param_of(p) - s).abs() < 1e-9, "param_of inverts point_at");
        let tg = ax.tangent_at(s);
        assert!((tg.len() - 1.0).abs() < 1e-12);
        // tangent is perpendicular to the radius
        assert!(tg.dot(p - ax.center).abs() < 1e-9);
        if s > 1e-4 && s < len - 1e-4 {
            let c = ArcAxis::contains(&ax, p, t).unwrap();
            assert!((c - s).abs() < 1e-6);
        }
    }
    assert!(ArcAxis::contains(&ax, ax.a, t).is_none(), "end points are not strictly inside");
    assert!(ArcAxis::contains(&ax, v2(2., 10.), t).is_none(), "off the circle");
    // offset radius: left offset shrinks a counter-clockwise arc
    let o = 0.25;
    assert!((ax.radius_at(o) - (ax.radius - ax.sigma() * o)).abs() < 1e-15);
    assert_eq!(ax.radius_at(0.0), ax.radius);
    // negative bulge mirrors
    let neg = ArcAxis::new(v2(0., 0.), v2(4., 0.), -0.5, t).unwrap();
    assert_eq!(neg.sigma(), -ax.sigma());
    assert!((neg.param_of(neg.point_at(1.3)) - 1.3).abs() < 1e-9);
}

#[test]
fn arc_helpers_straight_and_bad() {
    assert!((axis_length(v2(0., 0.), v2(3., 4.), 0.0) - 5.0).abs() < 1e-12, "chord");
    let semi = axis_length(v2(0., 0.), v2(2., 0.), 1.0);
    assert!((semi - std::f64::consts::PI).abs() < 1e-9, "half circle r=1");
    assert_eq!(axis_length(v2(0., 0.), v2(0., 0.), 0.5), 0.0);
    assert!(axis_length(v2(0., 0.), v2(1., 0.), f64::NAN).is_finite());
    assert_eq!(edge_mid(v2(0., 0.), v2(4., 0.), 0.0), v2(2., 0.));
    let m = edge_mid(v2(0., 0.), v2(4., 0.), 1.0);
    assert!(((m - v2(2., 0.)).len() - 2.0).abs() < 1e-9, "apex of a half circle");
    assert_eq!(edge_mid(v2(1., 1.), v2(1., 1.), 0.7), v2(1., 1.));
}

// ---- sweep / names ----
#[test]
fn sweep_kind_names_roundtrip() {
    for k in SweepKind::ALL {
        assert_eq!(SweepKind::parse(SweepKind::name(k)), Some(k));
        assert_eq!(k.subtracts(), k == SweepKind::Reveal);
    }
    assert_eq!(SweepKind::parse(""), None);
    assert_eq!(SweepKind::parse("Fascia"), None, "case-sensitive");
}

#[test]
fn bool_op_names_roundtrip() {
    for op in [BoolOp::Union, BoolOp::Subtract, BoolOp::Intersect] {
        assert_eq!(BoolOp::parse(BoolOp::name(op)), Some(op));
    }
    assert_eq!(BoolOp::parse("cut"), Some(BoolOp::Subtract));
    assert_eq!(BoolOp::parse("xor"), None);
}

#[test]
fn unit_conversion() {
    for (s, f) in [("m", 1.0), ("cm", 0.01), ("mm", 0.001), ("ft", 0.3048), ("in", 0.0254)] {
        let u = Unit::parse(s).unwrap();
        assert!((u.to_metres(1.0) - f).abs() < 1e-15);
        assert_eq!(u.to_metres(0.0), 0.0);
        assert!((u.to_metres(-2.0) + 2.0 * f).abs() < 1e-15);
    }
    assert_eq!(Unit::parse(" mm "), Some(Unit::Mm), "trimmed");
    assert_eq!(Unit::parse("MM"), None);
    assert_eq!(Unit::parse(""), None);
    assert!(Unit::M.to_metres(f64::NAN).is_nan());
}

// ---- csg ----
#[test]
fn csg_tags() {
    assert_eq!(tag_op(ftag(7, 5)), 7);
    assert_eq!(ftag(1, 4096 + 3) & 0xfff, 3, "local part is masked");
    for op in 0..50u32 {
        for l in [0u32, 1, 100, 4095] {
            assert_eq!(tag_op(ftag(op, l)), op);
        }
    }
}

#[test]
fn csg_body_queries() {
    let b = cube(2.0, 3);
    assert!(!b.is_empty());
    assert!((b.volume() - 8.0).abs() < 1e-9);
    assert!(b.is_convex(1e-9));
    assert!(Body::contains(&b, v3(1., 1., 1.)));
    assert!(!b.contains(v3(5., 1., 1.)));
    assert_eq!(b.max_tag(), 3);
    assert_eq!(b.tags(), vec![3]);
    let rt = b.clone().retagged(2);
    assert_eq!(rt.tags(), vec![5]);
    let tg = Body::from_mesh(
        &Solid::extrude(&Profile::new(rect(1., 1.), vec![], Tol::default()).unwrap(), 1., Tol::default())
            .triangulate()
            .unwrap(),
        2,
    )
    .tagged(9);
    assert!(tg.tags().iter().all(|&t| tag_op(t) == 9));
    // empty body
    let e = Body::default();
    assert!(Body::is_empty(&e) && e.max_tag() == 0 && e.volume() == 0.0);
    assert!(!e.contains(v3(0., 0., 0.)));
    assert!(e.to_mesh().triangles.is_empty());
    assert!(e.feature_edges(1e-9, 0.99).is_empty());
    assert!(e.face_outline(0, 1e-9).is_empty());
    // boolean
    let o = cube(2.0, 4).translated(v3(1., 0., 0.));
    let u = b.boolean(&o, BoolOp::Union, Tol::default());
    assert!((u.volume() - 12.0).abs() < 1e-6, "{}", u.volume());
    let i = b.boolean(&o, BoolOp::Intersect, Tol::default());
    assert!((i.volume() - 4.0).abs() < 1e-6);
    let d = b.boolean(&o, BoolOp::Subtract, Tol::default());
    assert!((d.volume() - 4.0).abs() < 1e-6);
    // inclusion-exclusion and determinism
    assert!((u.volume() + i.volume() - 16.0).abs() < 1e-6);
    let u2 = b.boolean(&o, BoolOp::Union, Tol::default());
    assert_eq!(u.polys.len(), u2.polys.len());
    // welded keeps volume
    assert!((u.welded(1e-9).volume() - u.volume()).abs() < 1e-9);
    // to_mesh closed and volume equal
    let m = b.to_mesh();
    assert!(m.is_watertight());
    assert!((m.volume() - 8.0).abs() < 1e-9);
    // box has 12 feature edges (angle > tol), each outline has 4 corners
    let fe = b.feature_edges(1e-9, 0.99);
    assert!(fe.len() >= 12, "{}", fe.len());
    assert!(b.face_outline(3, 1e-9).is_empty(), "a closed surface has no boundary");
    assert!(b.face_outline(77, 1e-9).is_empty(), "unknown tag");
    let quad = ddc_geom::csg::Poly::new(
        vec![v3(0., 0., 0.), v3(2., 0., 0.), v3(2., 2., 0.), v3(0., 2., 0.)],
        7,
    )
    .unwrap();
    assert!((quad.area() - 4.0).abs() < 1e-12);
    assert!(ddc_geom::csg::Poly::new(vec![v3(0., 0., 0.), v3(1., 0., 0.), v3(2., 0., 0.)], 1).is_none());
    let sheet = Body { polys: vec![quad] };
    let ring = sheet.face_outline(7, 1e-9);
    assert_eq!(ring.len(), 1);
    assert_eq!(ring[0].len(), 4);
}

// ---- nurbs surface ----
#[test]
fn nurbs_surface_plane_patch() {
    let ctrl = vec![v3(0., 0., 0.), v3(0., 2., 0.), v3(3., 0., 0.), v3(3., 2., 0.)];
    let k = vec![0.0, 0.0, 1.0, 1.0];
    let s = Surface::new(1, 1, k.clone(), k.clone(), 2, 2, ctrl, vec![1.0; 4]).unwrap();
    assert_eq!(s.knots_u(), &k[..]);
    assert_eq!(s.knots_v(), &k[..]);
    assert_eq!(s.domain_u(), (0.0, 1.0));
    assert_eq!(s.domain_v(), (0.0, 1.0));
    assert_eq!((s.deg_u(), s.deg_v(), s.nu(), s.nv()), (1, 1, 2, 2));
    assert_eq!(s.ctrl().len(), 4);
    assert_eq!(s.weights(), &[1.0; 4]);
    assert!(!s.closed_u() && !s.closed_v());
    assert!(!s.pole_u(false) && !s.pole_u(true) && !s.pole_v(false) && !s.pole_v(true));
    assert!((s.point(0.5, 0.5) - v3(1.5, 1.0, 0.0)).len() < 1e-12);
    assert!((s.point(2.0, -1.0) - s.point(1.0, 0.0)).len() < 1e-12, "parameters clamp");
    let n = s.normal(0.3, 0.4).unwrap();
    assert!((n.z.abs() - 1.0).abs() < 1e-12);
}

#[test]
fn nurbs_surface_pole_and_errors() {
    // collapsed edge u = u0 -> pole
    let ctrl = vec![v3(0., 0., 1.), v3(0., 0., 1.), v3(1., 0., 0.), v3(0., 1., 0.)];
    let k = vec![0.0, 0.0, 1.0, 1.0];
    let s = Surface::new(1, 1, k.clone(), k.clone(), 2, 2, ctrl, vec![1.0; 4]).unwrap();
    assert!(s.pole_u(false));
    assert!(!s.pole_u(true));
    // wrong sizes are refused, not panicking
    assert!(Surface::new(1, 1, k.clone(), k.clone(), 2, 2, vec![v3(0., 0., 0.)], vec![1.0]).is_err());
    let ctrl = vec![v3(0., 0., 0.); 4];
    assert!(Surface::new(1, 1, k.clone(), k.clone(), 2, 2, ctrl.clone(), vec![1.0; 3]).is_err());
    assert!(Surface::new(1, 1, k.clone(), k.clone(), 2, 2, ctrl.clone(), vec![0.0; 4]).is_err());
    let nan = vec![v3(f64::NAN, 0., 0.); 4];
    assert!(Surface::new(1, 1, k.clone(), k.clone(), 2, 2, nan, vec![1.0; 4]).is_err());
    assert!(Surface::new(1, 1, vec![1.0, 0.0, 1.0, 1.0], k.clone(), 2, 2, ctrl, vec![1.0; 4]).is_err());
}

// ---- mesh_ops::Plane ----
#[test]
fn mesh_ops_plane_roundtrip() {
    use ddc_geom::mesh_ops::Plane;
    assert!(Plane::new(v3(0., 0., 0.), v3(1., 1., 1.)).is_none());
    assert!(Plane::new(v3(f64::NAN, 0., 1.), v3(0., 0., 0.)).is_none());
    let mut r = Lcg(99);
    for _ in 0..200 {
        let n = v3(r.next() - 0.5, r.next() - 0.5, r.next() - 0.5);
        if n.len() < 1e-3 {
            continue;
        }
        let o = v3(r.next() * 10., r.next() * 10., r.next() * 10.);
        let pl = Plane::new(n, o).unwrap();
        assert!((pl.n.len() - 1.0).abs() < 1e-12, "stored normal is unit");
        let q = v2(r.next() * 5. - 2.5, r.next() * 5. - 2.5);
        let p = pl.to_3d(q);
        assert!((pl.n.dot(p) - pl.d).abs() < 1e-9, "3d point lies on the plane");
        let back = pl.to_2d(p);
        assert!((back - q).len() < 1e-9, "to_2d inverts to_3d");
    }
    // vertical wall plane keeps the horizontal normal
    let v = Plane::vertical(v2(0., 0.), v2(4., 0.)).unwrap();
    assert!(v.n.z.abs() < 1e-15);
    assert!(Plane::vertical(v2(1., 1.), v2(1., 1.)).is_none());
}

// ---- rebar ----
#[test]
fn rebar_scalars_and_hooks() {
    use ddc_geom::rebar::{bend_radius, mandrel_rule, unit_mass, Hook, Prim2};
    assert_eq!(mandrel_rule(0.016), 4.0);
    assert_eq!(mandrel_rule(0.0161), 7.0);
    assert_eq!(mandrel_rule(0.0), 4.0);
    assert_eq!(bend_radius(0.01, 0.0), (4.0 * 0.01 + 0.01) / 2.0, "rule when mandrel <= 0");
    assert_eq!(bend_radius(0.01, -3.0), bend_radius(0.01, 0.0));
    assert_eq!(bend_radius(0.01, 10.0), (10.0 * 0.01 + 0.01) / 2.0);
    assert!((unit_mass(0.012) - 0.8878).abs() < 1e-3, "12 mm bar is 0.888 kg/m");
    assert_eq!(unit_mass(0.0), 0.0);
    for (d, h) in [(0, Hook::None), (90, Hook::H90), (135, Hook::H135), (180, Hook::H180)] {
        assert_eq!(Hook::from_deg(d), Some(h));
        assert_eq!(h.deg(), d);
    }
    for bad in [-90, 1, 45, 270, i64::MAX, i64::MIN] {
        assert_eq!(Hook::from_deg(bad), None);
    }
    assert_eq!(Prim2::len(&Prim2::Line(v2(0., 0.), v2(3., 4.))), 5.0);
    let arc = Prim2::Arc { c: v2(0., 0.), r: 2.0, a0: 0.0, sweep: -std::f64::consts::PI };
    assert!((Prim2::len(&arc) - 2.0 * std::f64::consts::PI).abs() < 1e-12, "length ignores sign");
    assert_eq!(Prim2::len(&Prim2::Line(v2(1., 1.), v2(1., 1.))), 0.0);
}

// ---- member ----
#[test]
fn just_integer_codec() {
    use ddc_geom::member::Just;
    for v in [-100i64, -1, 0, 1, 7, i64::MAX, i64::MIN] {
        let j = Just::from_i(v);
        assert_eq!(j.to_i(), v.signum());
    }
    assert_eq!(Just::from_i(0), Just::Center);
}

// ---- ramp ----
#[test]
fn ramp_required_run() {
    use ddc_geom::ramp::RampDef;
    let mut d = RampDef {
        path: vec![v2(0., 0.), v2(10., 0.)],
        base_z: 0.0,
        rise: 0.6,
        width: 1.2,
        slope: 0.05,
        thickness: 0.15,
        landing: 1.5,
        max_run: 9.0,
    };
    assert!((d.required_run() - 12.0).abs() < 1e-12);
    d.slope = 0.0;
    assert!(d.required_run().is_infinite());
    d.slope = -0.1;
    assert!(d.required_run().is_infinite(), "non-positive slope has no finite run");
    d.slope = 0.1;
    d.rise = 0.0;
    assert_eq!(d.required_run(), 0.0);
}

// ---- sketch curves ----
#[test]
fn sketch_curve_degenerate_and_velocity() {
    use ddc_geom::sketch::SketchCurve as C;
    let eps = 1e-9;
    assert!(C::Line { a: v2(0., 0.), b: v2(0., 0.) }.is_degenerate(eps));
    assert!(C::Line { a: v2(f64::NAN, 0.), b: v2(1., 0.) }.is_degenerate(eps));
    assert!(!C::Line { a: v2(0., 0.), b: v2(1., 0.) }.is_degenerate(eps));
    assert!(C::Arc { c: v2(0., 0.), r: 1.0, a0: 0.0, sweep: 0.0 }.is_degenerate(eps));
    assert!(C::Arc { c: v2(0., 0.), r: f64::INFINITY, a0: 0.0, sweep: 1.0 }.is_degenerate(eps));
    assert!(!C::Arc { c: v2(0., 0.), r: 1.0, a0: 0.0, sweep: 1.0 }.is_degenerate(eps));
    assert!(C::Ellipse { c: v2(0., 0.), rx: 0.0, ry: 1.0, rot: 0.0, t0: 0.0, sweep: 1.0 }.is_degenerate(eps));
    assert!(C::Spline { ctrl: vec![v2(0., 0.)] }.is_degenerate(eps));
    assert!(C::Spline { ctrl: vec![v2(0., 0.), v2(0., 0.)] }.is_degenerate(eps));
    assert!(!C::Spline { ctrl: vec![v2(0., 0.), v2(1., 1.)] }.is_degenerate(eps));
    // velocity agrees with a central difference of point(t)
    let curves = [
        C::Line { a: v2(0., 0.), b: v2(3., 4.) },
        C::Arc { c: v2(1., 1.), r: 2.0, a0: 0.3, sweep: 2.0 },
        C::Ellipse { c: v2(0., 0.), rx: 3.0, ry: 2.0, rot: 0.4, t0: 0.1, sweep: 2.5 },
        C::Spline { ctrl: vec![v2(0., 0.), v2(1., 2.), v2(3., 1.), v2(4., 4.)] },
    ];
    for c in &curves {
        for t in [0.2, 0.5, 0.8] {
            let h = 1e-6;
            let fd = (c.point(t + h) - c.point(t - h)) * (1.0 / (2.0 * h));
            let v = c.velocity(t);
            assert!((fd - v).len() < 1e-3 * (1.0 + v.len()), "{c:?} t={t}: {fd:?} vs {v:?}");
        }
    }
}

#[test]
fn sketch_dims_mm_text() {
    use ddc_geom::sketch_dims::mm_text;
    assert_eq!(mm_text(0.25), "250mm");
    assert_eq!(mm_text(0.0), "0mm");
    assert_eq!(mm_text(-1.5), "-1500mm");
    assert_eq!(mm_text(0.0001234567891), "0.123457mm", "rounded to 1e-6 mm");
    assert!(!mm_text(f64::NAN).is_empty());
}

// ---- component ----
#[test]
fn component_small_items() {
    use ddc_geom::component::catalog::strip_bom;
    use ddc_geom::component::Lod;
    assert_eq!(strip_bom("\u{feff}a,b"), "a,b");
    assert_eq!(strip_bom("a,b"), "a,b");
    assert_eq!(strip_bom(""), "");
    assert_eq!(strip_bom("\u{feff}\u{feff}x"), "\u{feff}x", "only one mark");
    let bits = [Lod::Coarse.bit(), Lod::Medium.bit(), Lod::Fine.bit()];
    assert!(bits[0] != bits[1] && bits[1] != bits[2] && bits[0] != bits[2]);
}

#[test]
fn release_names_roundtrip() {
    use ddc_geom::analytical::Release;
    for r in [Release::Fixed, Release::Pinned] {
        assert_eq!(Release::parse(Release::name(r)), Some(r));
    }
    assert_eq!(Release::parse("Fixed"), None);
    assert_eq!(Release::parse(""), None);
}

// ---- family document ----
mod fam_doc {
    use ddc_geom::component::doc::FamDoc;
    use ddc_geom::component::{Category, Hosting, Kind};

    fn desk() -> FamDoc {
        FamDoc::blank("Desk", Category::Furniture, Hosting::None)
    }

    #[test]
    fn new_normalises_trailing_newline() {
        assert_eq!(FamDoc::new("family A").text(), "family A\n");
        assert_eq!(FamDoc::new("family A\n").text(), "family A\n");
        let d = FamDoc::new("");
        assert!(!d.can_undo() && !d.can_redo() && !d.dirty);
    }

    #[test]
    fn edits_undo_redo_round_trip() {
        let mut d = desk();
        let t0 = d.text().to_string();
        assert!(!d.can_undo() && !d.dirty);
        d.add_param("Width", Kind::Length, false, "600mm").unwrap();
        assert!(d.can_undo() && d.dirty && !d.can_redo());
        let t1 = d.text().to_string();
        assert_ne!(t0, t1);
        assert!(d.family().is_ok());
        d.set_param("Width", Kind::Length, true, "700mm").unwrap();
        let t2 = d.text().to_string();
        assert!(t2.contains("700mm"));
        assert!(d.undo());
        assert_eq!(d.text(), t1);
        assert!(d.can_redo());
        assert!(d.redo());
        assert_eq!(d.text(), t2);
        assert!(!d.redo(), "nothing left to redo");
        assert!(d.undo() && d.undo());
        assert_eq!(d.text(), t0);
        assert!(!d.undo(), "nothing left to undo");
        // a new edit clears the redo stack
        d.add_param("Depth", Kind::Length, false, "400mm").unwrap();
        assert!(!d.can_redo());
    }

    #[test]
    fn duplicate_and_missing_parameters_are_errors() {
        let mut d = desk();
        d.add_param("Width", Kind::Length, false, "600mm").unwrap();
        let before = d.text().to_string();
        assert!(d.add_param("Width", Kind::Length, false, "1mm").is_err());
        assert!(d.set_param("Nope", Kind::Length, false, "1mm").is_err());
        assert_eq!(d.text(), before, "failed edits leave the text alone");
    }

    #[test]
    fn set_text_validates() {
        let mut d = desk();
        let before = d.text().to_string();
        assert!(d.set_text("this is not a family".into()).is_err());
        assert_eq!(d.text(), before);
        assert!(!d.can_undo());
        d.set_text(before.trim_end().to_string()).unwrap();
        assert!(!d.can_undo(), "same text is not an edit");
        d.set_text(format!("{before}param W length type 1mm")).unwrap();
        assert!(d.can_undo());
    }

    #[test]
    fn replace_and_remove_statements() {
        let mut d = desk();
        d.add_param("Width", Kind::Length, false, "600mm").unwrap();
        let s = d.find("param", "Width").unwrap();
        d.replace_stmt(&s, "param Width length type 650mm").unwrap();
        assert!(d.text().contains("650mm"));
        let s = d.find("param", "Width").unwrap();
        d.remove_stmt(&s).unwrap();
        assert!(d.find("param", "Width").is_none());
        // a statement taken before the text changed is stale
        let mut stale = s.clone();
        stale.first = 10_000;
        stale.last = 10_001;
        assert!(d.replace_stmt(&stale, "param X length type 1mm").is_err());
        assert!(d.remove_stmt(&stale).is_err());
    }

    #[test]
    fn reference_planes_free_and_fixed() {
        let mut d = desk();
        d.set_ref_free("CenterLR", true, 0.25).unwrap();
        assert!(d.find("ref", "CenterLR").unwrap().text.contains('~'));
        d.set_ref_free("CenterLR", false, 0.25).unwrap();
        assert!(d.find("ref", "CenterLR").unwrap().text.contains('='));
        assert!(d.set_ref_free("NoSuchPlane", true, 0.0).is_err());
    }

    #[test]
    fn origin_category_and_constraints() {
        let mut d = desk();
        d.set_origin("0", "0", "0").unwrap();
        d.set_origin("1mm", "2mm", "3mm").unwrap();
        assert_eq!(d.text().matches("\norigin ").count(), 1, "replaced, not duplicated");
        d.set_category(Category::Casework).unwrap();
        assert_eq!(d.text().matches("\ncategory ").count(), 1);
        assert!(d.family().is_ok());
        let t = d.text().to_string();
        if d.add_angle("90deg", "CenterLR", "CenterFB").is_ok() {
            assert!(d.text().contains("angle 90deg"));
        } else {
            assert_eq!(d.text(), t);
        }
        if d.add_align("CenterLR", "CenterFB").is_ok() {
            assert!(d.text().contains("align CenterLR CenterFB"));
        }
    }

    #[test]
    fn nested_family_is_replaced_by_name() {
        let mut d = desk();
        let child = "family Leg\ncategory furniture\nhost none\nref C x = 0\n";
        let r = d.add_nested(child);
        if r.is_ok() {
            assert_eq!(d.text().matches("\nbegin\n").count(), 1);
            d.add_nested(child).unwrap();
            assert_eq!(d.text().matches("\nbegin\n").count(), 1, "same name replaces");
        }
        assert!(d.add_nested("garbage").is_err());
    }
}

// ---- clash volumes ----
#[test]
fn convex_prism_intersection_and_body_bounds() {
    use ddc_geom::mep::clash::{self, Convex};
    let a = Convex::prism(&rect(2.0, 2.0), 0.0, 2.0);
    let b = Convex::prism(&rect(2.0, 2.0).iter().map(|p| *p + v2(1.0, 0.0)).collect::<Vec<_>>(), 0.0, 2.0);
    assert!((a.volume() - 8.0).abs() < 1e-12);
    let bb = Convex::aabb(&a);
    assert_eq!((bb.min, bb.max), (v3(0., 0., 0.), v3(2., 2., 2.)));
    let i = Convex::intersection(&a, &b).expect("overlap");
    assert!((i.volume() - 4.0).abs() < 1e-9);
    assert!((a.intersection_volume(&b) - b.intersection_volume(&a)).abs() < 1e-12, "symmetric");
    let far = Convex::prism(&rect(1.0, 1.0).iter().map(|p| *p + v2(10.0, 10.0)).collect::<Vec<_>>(), 0.0, 1.0);
    assert!(Convex::intersection(&a, &far).is_none());
    assert_eq!(a.intersection_volume(&far), 0.0);
    // coplanar faces must not be counted twice
    assert!((a.intersection_volume(&a) - 8.0).abs() < 1e-9, "self intersection");
    let inner = Convex::prism(&rect(1.0, 1.0), 0.0, 1.0);
    assert!((a.intersection_volume(&inner) - 1.0).abs() < 1e-9, "contained, three coplanar faces");
    let touching = Convex::prism(&rect(2.0, 2.0).iter().map(|p| *p + v2(2.0, 0.0)).collect::<Vec<_>>(), 0.0, 2.0);
    assert_eq!(a.intersection_volume(&touching), 0.0, "shared face has no volume");
    let mut r = Lcg(31);
    for _ in 0..100 {
        let (x0, y0) = ((r.next() * 4.0).floor(), (r.next() * 4.0).floor());
        let (w, h) = (1.0 + (r.next() * 3.0).floor(), 1.0 + (r.next() * 3.0).floor());
        let q: Vec<V2> = vec![v2(x0, y0), v2(x0 + w, y0), v2(x0 + w, y0 + h), v2(x0, y0 + h)];
        let b2 = Convex::prism(&q, 0.0, 2.0);
        let ox = (x0 + w).min(2.0) - x0.max(0.0);
        let oy = (y0 + h).min(2.0) - y0.max(0.0);
        let want = if ox > 0.0 && oy > 0.0 { ox * oy * 2.0 } else { 0.0 };
        assert!((a.intersection_volume(&b2) - want).abs() < 1e-9, "{x0} {y0} {w} {h}");
    }
    // clockwise input is accepted
    let mut cw = rect(2.0, 2.0);
    cw.reverse();
    assert!((Convex::prism(&cw, 0.0, 2.0).volume() - 8.0).abs() < 1e-12);
    // Body: bounds and signed pieces
    let ba = clash::Body::new(vec![(a.clone(), 1.0)]);
    let bb2 = clash::Body::new(vec![(b, 1.0)]);
    let (v, bounds) = clash::Body::intersection_bounds(&ba, &bb2);
    assert!((v - 4.0).abs() < 1e-9);
    let bounds = bounds.unwrap();
    assert!((bounds.min.x - 1.0).abs() < 1e-9 && (bounds.max.x - 2.0).abs() < 1e-9);
    let (v0, none) = clash::Body::intersection_bounds(&ba, &clash::Body::new(vec![(far, 1.0)]));
    assert_eq!(v0, 0.0);
    assert!(none.is_none());
    let empty = clash::Body::new(vec![]);
    assert_eq!(clash::Body::intersection_bounds(&empty, &ba).0, 0.0);
}

// ---- curtain axis, raft, slab shape ----
#[test]
fn curtain_axis_left_normal() {
    use ddc_geom::curtain::Axis;
    let ax = Axis::new(v2(0., 0.), v2(4., 0.), 0.0).unwrap();
    assert_eq!(Axis::left_at(&ax, 1.0), v2(0.0, 1.0));
    let up = Axis::new(v2(0., 0.), v2(0., 3.), 0.0).unwrap();
    let l = Axis::left_at(&up, 0.5);
    assert!((l.x + 1.0).abs() < 1e-15 && l.y.abs() < 1e-15);
    let arc = Axis::new(v2(0., 0.), v2(4., 0.), 0.5).unwrap();
    for s in [0.0, 1.0, arc.length() / 2.0, arc.length()] {
        let n = Axis::left_at(&arc, s);
        assert!((n.len() - 1.0).abs() < 1e-12);
        let t = arc.arc.unwrap().tangent_at(s);
        assert!(n.dot(t).abs() < 1e-12, "normal is perpendicular to the tangent");
    }
    assert!(Axis::new(v2(1., 1.), v2(1., 1.), 0.0).is_err());
}

#[test]
fn raft_thickened_flag() {
    use ddc_geom::footing::Raft;
    let mut r = Raft { ring: rect(10.0, 8.0), top: 0.0, thickness: 0.4, edge_width: 0.0, edge_depth: 0.0 };
    assert!(!Raft::thickened(&r));
    assert!(r.inner().is_none());
    assert!(r.check().is_ok());
    r.edge_width = 0.5;
    assert!(!Raft::thickened(&r), "needs both width and depth");
    r.edge_depth = 0.3;
    assert!(Raft::thickened(&r));
    assert!(r.inner().is_some());
    r.edge_depth = f64::NAN;
    assert!(!Raft::thickened(&r), "NaN depth is not a thickening");
    assert!(r.check().is_err());
    r.edge_depth = -1.0;
    assert!(!Raft::thickened(&r));
}

#[test]
fn slab_shape_is_empty() {
    use ddc_geom::shape_edit::{ShapeLine, SlabShape};
    let mut s = SlabShape::default();
    assert!(SlabShape::is_empty(&s));
    s.ring_dz = vec![vec![0.0, 0.0], vec![]];
    assert!(SlabShape::is_empty(&s), "all-zero offsets are empty");
    s.ring_dz[0][1] = 0.01;
    assert!(!SlabShape::is_empty(&s));
    s.ring_dz = vec![];
    s.points.push((v2(1., 1.), 0.1));
    assert!(!SlabShape::is_empty(&s));
    s.points.clear();
    s.lines.push(ShapeLine { a: v2(0., 0.), b: v2(1., 0.), dza: 0.0, dzb: 0.0 });
    assert!(!SlabShape::is_empty(&s));
}

// ---- member ----
mod member_geometry {
    use ddc_geom::math::{v3, V3};
    use ddc_geom::member::{Just, Member, Path, Shape};

    fn beam(shape: Shape, a: V3, b: V3) -> Member {
        Member {
            shape,
            path: Path::Line { a, b },
            rotation: 0.0,
            yjust: Just::Center,
            zjust: Just::Center,
            column: false,
            start: None,
            end: None,
        }
    }

    #[test]
    fn web_flange_only_for_rolled_sections() {
        let (h, b, tw, tf) = (0.3, 0.15, 0.007, 0.011);
        assert_eq!(Shape::web_flange(&Shape::I { h, b, tw, tf }), Some((tw, tf)));
        assert_eq!(Shape::web_flange(&Shape::Channel { h, b, tw, tf }), Some((tw, tf)));
        assert_eq!(Shape::web_flange(&Shape::Tee { h, b, tw, tf }), Some((tw, tf)));
        assert_eq!(Shape::web_flange(&Shape::Rect { b, h }), None);
        assert_eq!(Shape::web_flange(&Shape::Circle { d: 0.2 }), None);
    }

    #[test]
    fn extents_follow_justification() {
        let m = beam(Shape::Rect { b: 0.2, h: 0.4 }, v3(0., 0., 3.), v3(6., 0., 3.));
        let (lo, hi) = Member::extents(&m);
        assert!((lo.x + 0.1).abs() < 1e-12 && (hi.x - 0.1).abs() < 1e-12);
        assert!((lo.y + 0.2).abs() < 1e-12 && (hi.y - 0.2).abs() < 1e-12);
        let mut top = m.clone();
        top.zjust = Just::Pos;
        let (lo2, hi2) = Member::extents(&top);
        assert!(((hi2.y - lo2.y) - (hi.y - lo.y)).abs() < 1e-12, "size does not change");
        assert!((lo2.y - lo.y).abs() > 1e-9 || (hi2.y - hi.y).abs() > 1e-9, "offset moves the box");
    }

    #[test]
    fn axis_ends_dir_and_z_range() {
        let m = beam(Shape::Rect { b: 0.2, h: 0.4 }, v3(0., 0., 3.), v3(6., 0., 3.));
        let (a, b) = Member::axis_ends(&m).unwrap();
        assert!((a - v3(0., 0., 3.)).len() < 1e-9 && (b - v3(6., 0., 3.)).len() < 1e-9);
        let d = Member::axis_dir(&m).unwrap();
        assert!((d - v3(1., 0., 0.)).len() < 1e-12);
        let (z0, z1) = Member::z_range(&m);
        assert!(z0 < z1 && (z1 - z0 - 0.4).abs() < 1e-9, "{z0} {z1}");
        // a column is the same measured vertically
        let mut c = beam(Shape::Rect { b: 0.3, h: 0.3 }, v3(1., 1., 0.), v3(1., 1., 3.0));
        c.column = true;
        let (c0, c1) = Member::z_range(&c);
        assert!((c0 - 0.0).abs() < 1e-9 && (c1 - 3.0).abs() < 1e-9);
        let d = Member::axis_dir(&c).unwrap();
        assert!((d - v3(0., 0., 1.)).len() < 1e-12);
        // degenerate: zero-length axis gives no direction
        let z = beam(Shape::Rect { b: 0.2, h: 0.2 }, v3(0., 0., 0.), v3(0., 0., 0.));
        assert!(Member::axis_dir(&z).is_none());
    }
}

// ---- mass / gsolid / rebar / wall_edit helpers ----
mod helpers {
    use super::{rect, Lcg};
    use ddc_geom::csg::Body;
    use ddc_geom::gsolid::correspond;
    use ddc_geom::mass::{convex_from_planes, cuboid, planar_face, resample, vertical_rings, MassError};
    use ddc_geom::math::{v2, v3, Tol, V2};
    use ddc_geom::rebar::{any_perp, region_intervals};
    use ddc_geom::wall_edit::centroid_u;

    fn ring_len(r: &[V2]) -> f64 {
        ddc_geom::poly::perimeter(r)
    }

    #[test]
    fn resample_keeps_perimeter_and_count() {
        let sq = rect(4.0, 4.0);
        assert_eq!(resample(&sq, 4), sq, "same count is identity");
        let r8 = resample(&sq, 8);
        assert_eq!(r8.len(), 8);
        assert_eq!(r8[0], sq[0], "starts at vertex 0");
        assert!((ring_len(&r8) - 16.0).abs() < 1e-9);
        let r3 = resample(&sq, 3);
        assert_eq!(r3.len(), 3);
        for p in &r3 {
            let on = p.x.abs() < 1e-9 || (p.x - 4.0).abs() < 1e-9 || p.y.abs() < 1e-9 || (p.y - 4.0).abs() < 1e-9;
            assert!(on, "resampled points stay on the outline: {p:?}");
        }
        assert_eq!(resample(&sq, 0).len(), 0);
    }

    #[test]
    fn convex_from_planes_box_and_empty() {
        let t = Tol::default();
        let pl = [
            (1, v3(1., 0., 0.), 2.0),
            (2, v3(-1., 0., 0.), 0.0),
            (3, v3(0., 1., 0.), 3.0),
            (4, v3(0., -1., 0.), 0.0),
            (5, v3(0., 0., 1.), 4.0),
            (6, v3(0., 0., -1.), 0.0),
        ];
        let b = convex_from_planes(&pl, t).unwrap();
        assert!((b.volume() - 24.0).abs() < 1e-6, "{}", b.volume());
        assert!(b.is_convex(1e-9));
        // contradicting half-spaces leave nothing
        let bad = [(1, v3(1., 0., 0.), 0.0), (2, v3(-1., 0., 0.), -1.0)];
        assert!(matches!(convex_from_planes(&bad, t), Err(MassError::Empty)));
        // an open set of planes is refused, not returned as a huge box
        assert!(matches!(convex_from_planes(&[], t), Err(MassError::Unbounded)));
        assert!(matches!(convex_from_planes(&pl[..5], t), Err(MassError::Unbounded)));
    }

    #[test]
    fn planar_face_and_vertical_rings() {
        let b = cuboid(v3(0., 0., 0.), v3(2., 2., 2.));
        let tag = b.polys[0].tag;
        assert!(planar_face(&b, tag, 1e-9).is_ok());
        assert!(matches!(planar_face(&b, 99_999, 1e-9), Err(MassError::NoFace(99_999))));
        assert!(matches!(planar_face(&Body::default(), 0, 1e-9), Err(MassError::NoFace(0))));
        let rings = vertical_rings(&b, v2(1.0, 1.0), v2(1.0, 0.0));
        assert_eq!(rings.len(), 1);
        let area: f64 = ddc_geom::poly::area(&rings[0]).abs();
        assert!((area - 4.0).abs() < 1e-9, "2 x 2 section, got {area}");
        let miss = vertical_rings(&b, v2(1.0, 10.0), v2(1.0, 0.0));
        assert!(miss.is_empty());
    }

    #[test]
    fn any_perp_is_unit_and_perpendicular() {
        let mut r = Lcg(1);
        for _ in 0..500 {
            let u = v3(r.next() - 0.5, r.next() - 0.5, r.next() - 0.5);
            if u.len() < 1e-3 {
                continue;
            }
            let u = u.normalized();
            let p = any_perp(u);
            assert!((p.len() - 1.0).abs() < 1e-12);
            assert!(p.dot(u).abs() < 1e-12);
        }
        for u in [v3(0., 0., 1.), v3(0., 0., -1.), v3(1., 0., 0.), v3(0., 1., 0.)] {
            assert!(any_perp(u).dot(u).abs() < 1e-12);
        }
    }

    #[test]
    fn region_intervals_cut_a_square() {
        let ring = vec![rect(4.0, 4.0)];
        let iv = region_intervals(v2(-1.0, 2.0), v2(1.0, 0.0), &ring);
        assert_eq!(iv.len(), 1);
        assert!((iv[0].0 - 1.0).abs() < 1e-9 && (iv[0].1 - 5.0).abs() < 1e-9, "{iv:?}");
        assert!((iv[0].2 - 1.0).abs() < 1e-9 && (iv[0].3 - 1.0).abs() < 1e-9, "perpendicular hits");
        let none = region_intervals(v2(-1.0, 9.0), v2(1.0, 0.0), &ring);
        assert!(none.is_empty());
        assert!(region_intervals(v2(0.0, 0.0), v2(1.0, 0.0), &[]).is_empty());
        assert!(region_intervals(v2(0.0, 0.0), v2(1.0, 0.0), &[vec![v2(0., 0.), v2(1., 1.)]]).is_empty());
        // a hole splits the interval
        let hole = vec![v2(1., 1.), v2(1., 3.), v2(3., 3.), v2(3., 1.)];
        let iv = region_intervals(v2(-1.0, 2.0), v2(1.0, 0.0), &[rect(4.0, 4.0), hole]);
        assert_eq!(iv.len(), 2);
    }

    #[test]
    fn centroid_u_of_polygons() {
        assert!((centroid_u(&rect(4.0, 2.0)) - 2.0).abs() < 1e-12);
        let shifted: Vec<V2> = rect(4.0, 2.0).iter().map(|p| *p + v2(10.0, 5.0)).collect();
        assert!((centroid_u(&shifted) - 12.0).abs() < 1e-12);
        assert_eq!(centroid_u(&[]), 0.0);
        assert_eq!(centroid_u(&[v2(1., 1.), v2(2., 2.), v2(3., 3.)]), 0.0, "zero area");
    }

    #[test]
    fn correspond_gives_equal_lengths_on_the_same_parameter() {
        let a = rect(4.0, 4.0);
        let b = vec![v2(0., 0.), v2(2., 0.), v2(3., 1.), v2(2., 2.), v2(0., 2.)];
        let (pa, pb) = correspond(&a, &b);
        assert_eq!(pa.len(), pb.len());
        assert!(pa.len() >= a.len().max(b.len()));
        assert_eq!(pa[0], a[0]);
        assert_eq!(pb[0], b[0]);
        // every original vertex is still on the resampled ring
        for v in &a {
            assert!(pa.iter().any(|p| (*p - *v).len() < 1e-9));
        }
        for v in &b {
            assert!(pb.iter().any(|p| (*p - *v).len() < 1e-9));
        }
        let (x, y) = correspond(&a, &a);
        assert_eq!(x.len(), 4);
        assert_eq!(x, y);
    }
}

#[test]
fn curtain_offset_down() {
    use ddc_geom::curtain::offset_down;
    let flat = [(0.0, 3.0), (4.0, 3.0)];
    assert_eq!(offset_down(&flat, 0.0), flat.to_vec());
    assert_eq!(offset_down(&[(0.0, 1.0)], 0.5), vec![(0.0, 1.0)], "single point unchanged");
    assert_eq!(offset_down(&[], 0.5), Vec::<(f64, f64)>::new());
    let d = offset_down(&flat, 0.5);
    assert_eq!(d.len(), 2);
    assert!(d.iter().all(|p| (p.1 - 2.5).abs() < 1e-12));
    // slope 1 gives a vertical drop of w * sqrt 2
    let s = offset_down(&[(0.0, 0.0), (4.0, 4.0)], 1.0);
    assert!((s[0].1 + 2f64.sqrt()).abs() < 1e-9);
    // roof peak: the offset line keeps the shape
    let peak = offset_down(&[(0.0, 0.0), (2.0, 2.0), (4.0, 0.0)], 0.2);
    assert_eq!(peak.len(), 3);
    assert!(peak[1].1 < 2.0);
}

// ---- stair flight profiles ----
#[test]
fn stair_flight_profiles() {
    use ddc_geom::stair::{Flight, Shape, Stair};
    let f = Flight { start: v2(0., 0.), dir: v2(1., 0.), width: 1.0, risers: 11, z0: 0.0 };
    let st = Stair {
        risers: 11,
        riser: 0.17,
        tread: 0.28,
        flights: vec![f],
        landings: vec![],
        winders: vec![],
        thickness: 0.15,
        stringer: None,
        shape: Shape::Straight,
        landing_depth: 1.0,
        z: (0.0, 1.87),
    };
    let m = f.treads();
    assert_eq!(m, 10);
    let prof = Stair::flight_profile(&st, &f);
    // counter-clockwise, non-empty, inside the run box
    let area: f64 = (0..prof.len())
        .map(|i| {
            let (a, b) = (prof[i], prof[(i + 1) % prof.len()]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        / 2.0;
    assert!(area > 0.0, "counter-clockwise, got {area}");
    let (l, top) = (0.28 * m as f64, 0.17 * m as f64);
    assert!(prof.iter().all(|p| p.0 >= -1e-12 && p.0 <= l + 1e-12 && p.1 >= -1e-12 && p.1 <= top + 1e-12));
    assert!(prof.iter().any(|p| (p.0 - l).abs() < 1e-12 && (p.1 - top).abs() < 1e-12), "top nosing present");
    // top line: starts at the first riser, steps up by one riser per tread
    let t = Stair::flight_top(&st, &f);
    assert_eq!(t[0], (0.0, 0.17));
    assert_eq!(*t.last().unwrap(), (l, top));
    assert_eq!(t.len(), 2 + 2 * (m - 1));
    assert!(t.windows(2).all(|w| w[1].0 >= w[0].0 && w[1].1 >= w[0].1), "monotone");
    // bottom line: along the soffit, starts at the origin and never rises above the top
    let b = Stair::flight_bottom(&st, &f);
    assert!(b.len() == 2 || b.len() == 3);
    assert_eq!(b[0], (0.0, 0.0));
    assert!(b.last().unwrap().1 <= top + 1e-12);
    // volume of the slab profile is positive and below the full wedge
    assert!(area < l * top);
}

#[test]
fn roof_preset_names_roundtrip() {
    use ddc_geom::roof_shape::Preset;
    for p in Preset::ALL {
        assert_eq!(Preset::parse(Preset::name(p)), Some(p));
    }
    assert_eq!(Preset::parse("Gable"), None);
    assert_eq!(Preset::parse(""), None);
}

#[test]
fn repair_opts_report_only() {
    use ddc_geom::mesh_repair::RepairOpts;
    let r = RepairOpts::report_only();
    let d = RepairOpts::default();
    assert!(!r.fix_orientation && !r.fill_holes);
    assert!(d.fix_orientation && d.fill_holes);
    assert_eq!((r.tol, r.max_hole_edges, r.max_triangles), (d.tol, d.max_hole_edges, d.max_triangles));
}

#[test]
fn refill_faces_guards() {
    use ddc_geom::face_fill::refill_faces;
    use ddc_geom::mesh::Mesh;
    // face_of out of step with triangles: nothing happens
    let mut m = Mesh {
        positions: vec![v3(0., 0., 0.), v3(1., 0., 0.), v3(0., 1., 0.)],
        triangles: vec![[0, 1, 2]],
        face_of: vec![],
    };
    let before = (m.positions.clone(), m.triangles.clone());
    assert_eq!(refill_faces(&mut m, &[true, true, true], 0), 0);
    assert_eq!((m.positions.clone(), m.triangles.clone()), before);
    // nothing touched: nothing refilled
    m.face_of = vec![0];
    assert_eq!(refill_faces(&mut m, &[false, false, false], 0), 0);
    assert_eq!(m.triangles, before.1);
    // empty mesh
    let mut e = Mesh::default();
    assert_eq!(refill_faces(&mut e, &[], 0), 0);
    // flags shorter than the vertex list are treated as untouched
    assert_eq!(refill_faces(&mut m, &[], 0), 0);
}

#[test]
fn roof_underside_at_follows_slope() {
    use ddc_geom::roof::Footprint;
    let r = Footprint {
        ring: rect(10.0, 6.0),
        slopes: vec![Some(30.0); 4],
        base_z: 3.0,
        overhang: 0.0,
        mansard: None,
    }
    .build()
    .unwrap();
    let p = v2(1.0, 3.0);
    let top = r.top_at(p).unwrap();
    assert_eq!(r.underside_at(p, 0.0), Some(top), "zero thickness");
    // on the 30 degree face the vertical drop is t / cos(30)
    let u = r.underside_at(p, 0.2).unwrap();
    let want = top - 0.2 / 30f64.to_radians().cos();
    assert!((u - want).abs() < 1e-9, "{u} vs {want}");
    assert!(r.underside_at(v2(-5.0, -5.0), 0.2).is_none(), "outside the roof");
    // monotone in thickness
    assert!(r.underside_at(p, 0.4).unwrap() < u);
}

#[test]
fn curtain_layout_lines_matches_layout_and_per_line_sections() {
    use ddc_geom::curtain::{layout, layout_lines, line_sections, spacing_lines, Axis, Section};
    let ax = Axis::new(v2(0., 0.), v2(6., 0.), 0.0).unwrap();
    let u = spacing_lines(6.0, 1.5);
    let v = spacing_lines(3.0, 1.0);
    let rect_s = Section::Rect { w: 0.05, d: 0.15 };
    let a = layout(&ax, 0.0, &u, &v, rect_s, rect_s);
    let su = line_sections(u.len(), rect_s, rect_s);
    let sv = line_sections(v.len(), rect_s, rect_s);
    let b = layout_lines(&ax, 0.0, &u, &v, &su, &sv);
    assert_eq!(a.cells.len(), b.cells.len());
    assert_eq!(a.bars.len(), b.bars.len());
    for (x, y) in a.cells.iter().zip(&b.cells) {
        assert!((x.net_area - y.net_area).abs() < 1e-12);
    }
    // no sections at all: the glass is the whole cell
    let none = vec![Section::None; u.len()];
    let nonev = vec![Section::None; v.len()];
    let c = layout_lines(&ax, 0.0, &u, &v, &none, &nonev);
    for cell in &c.cells {
        assert!((cell.net_area - cell.area).abs() < 1e-12);
    }
    assert!(c.bars.is_empty() || c.bars.iter().all(|bar| bar.length() >= 0.0));
    // one wide mullion takes area from its two neighbours
    let mut wide = su.clone();
    wide[2] = Section::Rect { w: 0.2, d: 0.15 };
    let d = layout_lines(&ax, 0.0, &u, &v, &wide, &sv);
    let net = |l: &ddc_geom::curtain::Layout| l.cells.iter().map(|c| c.net_area).sum::<f64>();
    assert!(net(&d) < net(&b));
}

#[test]
fn sketch_grid_points_segments_pairs() {
    use ddc_geom::sketch::Grid;
    let pts = [v2(0.0, 0.0), v2(0.1, 0.0), v2(5.0, 5.0), v2(5.05, 5.0)];
    let g = Grid::of_points(pts.iter().copied(), 1.0);
    let mut near0: Vec<usize> = Grid::near(&g, v2(0.0, 0.0), 0.5).collect();
    near0.sort();
    assert_eq!(near0, vec![0, 1]);
    let mut far: Vec<usize> = Grid::near(&g, v2(5.0, 5.0), 0.5).collect();
    far.sort();
    assert_eq!(far, vec![2, 3]);
    assert_eq!(Grid::near(&g, v2(100.0, 100.0), 0.5).count(), 0);
    assert_eq!(Grid::of_points(std::iter::empty(), 1.0).pairs().len(), 0);
    let pairs = Grid::pairs(&g);
    assert!(pairs.contains(&(0, 1)) && pairs.contains(&(2, 3)));
    assert!(!pairs.contains(&(0, 2)));
    assert!(pairs.iter().all(|p| p.0 < p.1));
    // segments: crossing segments share a cell, a far one does not
    let segs = [
        (v2(0.0, 0.0), v2(2.0, 2.0)),
        (v2(0.0, 2.0), v2(2.0, 0.0)),
        (v2(50.0, 50.0), v2(51.0, 50.0)),
    ];
    let gs = Grid::of_segments(segs.iter().copied());
    let sp = Grid::pairs(&gs);
    assert!(sp.contains(&(0, 1)), "crossing diagonals must be candidates");
    assert!(!sp.contains(&(0, 2)) && !sp.contains(&(1, 2)));
    assert_eq!(Grid::of_segments(std::iter::empty()).pairs().len(), 0);
    // degenerate zero-length segment does not panic
    let _ = Grid::of_segments([(v2(1.0, 1.0), v2(1.0, 1.0))].into_iter()).pairs();
}
