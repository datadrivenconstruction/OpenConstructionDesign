//! Shapes and boolean operations: exact volumes, closedness, push-pull, fillets, fuzzing.

use std::f64::consts::{PI, TAU};

use ddc_geom::csg::{ftag, Body, BodyError};
use ddc_geom::mass::*;
use ddc_geom::{v2, v3, Tol};

const T: Tol = Tol {
    length: 1e-7,
    angle: 1e-9,
};

fn ngon_area(r: f64, n: usize) -> f64 {
    0.5 * n as f64 * r * r * (TAU / n as f64).sin()
}

fn closed(b: &Body) {
    b.validate(T)
        .unwrap_or_else(|e| panic!("not closed: {e:?}"));
}

fn unit() -> Body {
    cuboid(v3(0., 0., 0.), v3(1., 1., 1.))
}

#[test]
fn cube_minus_cylinder_is_exact() {
    let cyl = cylinder(v2(0.5, 0.5), 0.3, -0.5, 2.0, 32);
    let r = unit().subtract(&cyl, T);
    closed(&r);
    assert!(
        (r.volume() - (1.0 - ngon_area(0.3, 32))).abs() < 1e-9,
        "{}",
        r.volume()
    );
    let i = unit().intersect(&cyl, T);
    closed(&i);
    assert!((i.volume() - ngon_area(0.3, 32)).abs() < 1e-9);
}

#[test]
fn coplanar_union_merges_shared_face() {
    let a = unit();
    let b = cuboid(v3(1., 0., 0.), v3(2., 1., 1.));
    let u = a.union(&b, T);
    closed(&u);
    assert!((u.volume() - 2.0).abs() < 1e-12);
    // the shared face vanishes: surface of a 2x1x1 box
    assert!((u.area() - 10.0).abs() < 1e-12, "{}", u.area());
    // offset coplanar tops and partial overlap
    let c = cuboid(v3(0.5, 0.5, 0.), v3(1.5, 1.5, 1.));
    let u2 = a.union(&c, T);
    closed(&u2);
    assert!((u2.volume() - 1.75).abs() < 1e-12);
    let s = a.subtract(&c, T);
    closed(&s);
    assert!((s.volume() - 0.75).abs() < 1e-12);
    let i = a.intersect(&c, T);
    closed(&i);
    assert!((i.volume() - 0.25).abs() < 1e-12);
}

#[test]
fn touching_faces_edges_and_vertices() {
    let a = unit();
    let face = cuboid(v3(1., 0.25, 0.25), v3(2., 0.75, 0.75));
    let u = a.union(&face, T);
    closed(&u);
    assert!((u.volume() - 1.25).abs() < 1e-12);
    assert!(a.intersect(&face, T).volume().abs() < 1e-12);
    assert!((a.subtract(&face, T).volume() - 1.0).abs() < 1e-12);
    let edge = cuboid(v3(1., 1., 0.), v3(2., 2., 1.));
    let ue = a.union(&edge, T);
    assert!((ue.volume() - 2.0).abs() < 1e-12);
    let vert = cuboid(v3(1., 1., 1.), v3(2., 2., 2.));
    assert!((a.union(&vert, T).volume() - 2.0).abs() < 1e-12);
    // within tolerance counts as touching
    let near = cuboid(v3(1.0 + 1e-9, 0., 0.), v3(2., 1., 1.));
    let un = a.union(&near, T);
    closed(&un);
    assert!((un.volume() - 2.0).abs() < 1e-8);
}

#[test]
fn identical_and_nested_operands() {
    let a = unit();
    assert!((a.union(&a, T).volume() - 1.0).abs() < 1e-12);
    assert!(a.subtract(&a, T).volume().abs() < 1e-12);
    let inner = cuboid(v3(0.25, 0.25, 0.25), v3(0.75, 0.75, 0.75));
    let hollow = a.subtract(&inner, T);
    closed(&hollow);
    assert!((hollow.volume() - 0.875).abs() < 1e-12);
}

#[test]
fn forms_have_exact_volumes() {
    // revolve a rectangle (r 1..2, z 0..1) a full turn: annulus prism
    let prof = [v2(1., 0.), v2(2., 0.), v2(2., 1.), v2(1., 1.)];
    let n = 48;
    let rv = revolve(&prof, TAU, n);
    closed(&rv);
    assert!((rv.volume() - (ngon_area(2., n) - ngon_area(1., n))).abs() < 1e-9);
    let half = revolve(&prof, PI, 24);
    closed(&half);
    // sweep a 0.2x0.2 square along an L path: miter keeps the exact centreline volume
    let sq = [v2(-0.1, -0.1), v2(0.1, -0.1), v2(0.1, 0.1), v2(-0.1, 0.1)];
    let sw = sweep_plan(&sq, &[v2(0., 0.), v2(3., 0.), v2(3., 2.)], 0.0).unwrap();
    closed(&sw);
    assert!((sw.volume() - 0.04 * 5.0).abs() < 1e-9, "{}", sw.volume());
    let sw3 = sweep(&sq, &[v3(0., 0., 0.), v3(1., 0., 1.), v3(1., 1., 2.)]).unwrap();
    closed(&sw3);
    // blend square 2x2 at z=0 to 1x1 at z=3: frustum
    let s2 = [v2(0., 0.), v2(2., 0.), v2(2., 2.), v2(0., 2.)];
    let s1 = [v2(0.5, 0.5), v2(1.5, 0.5), v2(1.5, 1.5), v2(0.5, 1.5)];
    let bl = blend(&[(s2.to_vec(), 0.0), (s1.to_vec(), 3.0)]).unwrap();
    closed(&bl);
    assert!((bl.volume() - 3.0 / 3.0 * (4.0 + 1.0 + 2.0)).abs() < 1e-9);
    let sb = swept_blend(&s2, &s1, &[v3(5., 0., 0.), v3(5., 0., 3.)]).unwrap();
    closed(&sb);
    assert!((sb.volume() - 7.0).abs() < 1e-9, "{}", sb.volume());
    // void form cuts a host: slab minus mass
    let slab = cuboid(v3(-1., -1., 0.), v3(4., 4., 0.2));
    let cut = slab.subtract(&bl, T);
    closed(&cut);
    let z = 0.2 / 3.0;
    let w = 2.0 - z; // section width at the slab top: linear 2 -> 1 over 3
    let lost = 0.2 / 3.0 * (4.0 + w * w + 2.0 * w);
    assert!((cut.volume() - (25.0 * 0.2 - lost)).abs() < 1e-9);
}

#[test]
fn push_pull_moves_a_face_and_keeps_its_name() {
    let b = unit();
    let up = push_pull(&b, 1, 0.5, 9, T).unwrap();
    closed(&up);
    assert!((up.volume() - 1.5).abs() < 1e-12);
    assert!((up.tag_area(1) - 1.0).abs() < 1e-12);
    let down = push_pull(&up, 1, -1.0, 9, T).unwrap();
    closed(&down);
    assert!((down.volume() - 0.5).abs() < 1e-12);
    let (_, p) = face_of(&down, 1).unwrap();
    assert!(p.iter().flatten().all(|q| (q.z - 0.5).abs() < 1e-12));
    // side face of an L-shaped extrusion
    let l = extrude(
        &[
            v2(0., 0.),
            v2(2., 0.),
            v2(2., 1.),
            v2(1., 1.),
            v2(1., 2.),
            v2(0., 2.),
        ],
        0.0,
        1.0,
    );
    let pushed = push_pull(&l, 2, 0.25, 9, T).unwrap();
    closed(&pushed);
    assert!((pushed.volume() - (3.0 + 0.5)).abs() < 1e-12);
}

#[test]
fn chamfer_and_fillet_remove_exact_amounts() {
    let b = unit();
    let e = edge_between(&b, 1, 2, 1e-9).expect("top/front edge");
    assert!(!e.concave());
    let c = chamfer(&b, &e, 0.2, 9, T).unwrap();
    closed(&c);
    assert!((c.volume() - (1.0 - 0.02)).abs() < 1e-12, "{}", c.volume());
    let (r, seg) = (0.25, 16);
    let f = fillet(&b, &e, r, seg, 9, T).unwrap();
    closed(&f);
    let sec = seg as f64 * 0.5 * r * r * (PI / 2.0 / seg as f64).sin();
    assert!((f.volume() - (1.0 - (r * r - sec))).abs() < 1e-12);
    let chord = 2.0 * r * (PI / 4.0 / seg as f64).sin();
    let area = 6.0 - 2.0 * r + seg as f64 * chord - 2.0 * (r * r - sec);
    assert!((f.area() - area).abs() < 1e-12, "{} vs {area}", f.area());
}

#[test]
fn shell_and_offset_of_convex_forms() {
    let b = cuboid(v3(0., 0., 0.), v3(2., 2., 2.));
    let s = shell(&b, 0.1, &[1], 9, T).unwrap();
    closed(&s);
    assert!((s.volume() - (8.0 - 1.8 * 1.8 * 1.9)).abs() < 1e-9);
    let o = offset_faces(&b, &[], 0.5, 9, T).unwrap();
    closed(&o);
    assert!((o.volume() - 27.0).abs() < 1e-9);
    let one = offset_faces(&b, &[1], 1.0, 9, T).unwrap();
    assert!((one.volume() - 12.0).abs() < 1e-9);
}

#[test]
fn split_face_and_floor_areas() {
    let b = unit();
    let s = split_face(&b, 1, v3(0.3, 0., 0.), v3(1., 0., 0.), 9, T).unwrap();
    let t = ftag(9, 1);
    closed(&s);
    assert!((s.tag_area(t) - 0.7).abs() < 1e-12);
    assert!((s.tag_area(1) - 0.3).abs() < 1e-12);
    let tower = extrude(
        &[v2(0., 0.), v2(10., 0.), v2(10., 6.), v2(0., 6.)],
        0.0,
        12.0,
    );
    let fl = floors(&tower, &[0.0, 3.0, 6.0, 9.0, 12.0]);
    assert_eq!(fl.len(), 4);
    assert!(fl
        .iter()
        .all(|f| (f.1 - 60.0).abs() < 1e-9 && (f.2 - 32.0).abs() < 1e-9));
    let bl = blend(&[
        (vec![v2(0., 0.), v2(2., 0.), v2(2., 2.), v2(0., 2.)], 0.0),
        (
            vec![v2(0.5, 0.5), v2(1.5, 0.5), v2(1.5, 1.5), v2(0.5, 1.5)],
            3.0,
        ),
    ])
    .unwrap();
    let (a, _) = section(&bl, 1.5);
    assert!((a - 1.5 * 1.5).abs() < 1e-9);
    assert_eq!(section_rings(&tower, 3.0).len(), 1);
}

struct Lcg(u64);
impl Lcg {
    fn f(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    // grid-snapped coordinates make coplanar and touching cases common
    fn g(&mut self) -> f64 {
        (self.f() * 8.0).floor() * 0.25
    }
}

fn random_body(r: &mut Lcg) -> Body {
    let (x, y, z) = (r.g(), r.g(), r.g());
    let (w, d, h) = (0.25 + r.g(), 0.25 + r.g(), 0.25 + r.g());
    if r.f() < 0.3 {
        cylinder(
            v2(x, y),
            0.25 + r.g() * 0.5,
            z,
            h,
            6 + (r.f() * 20.0) as usize,
        )
    } else {
        cuboid(v3(x, y, z), v3(x + w, y + d, z + h))
    }
}

#[test]
fn fuzz_1000_booleans_stay_closed_and_consistent() {
    let mut r = Lcg(0x5eed);
    let mut touching = 0;
    for k in 0..1000 {
        let (a, b) = (random_body(&mut r), random_body(&mut r));
        let i = a.intersect(&b, T);
        let u = a.union(&b, T);
        let s = a.subtract(&b, T);
        for (name, x) in [("i", &i), ("u", &u), ("s", &s)] {
            if !x.is_empty() {
                x.check_closed(T)
                    .unwrap_or_else(|e| panic!("case {k} {name}: {e:?}"));
                match x.validate(T) {
                    Ok(()) => {}
                    Err(BodyError::NonManifold(..)) => touching += 1,
                    Err(e) => panic!("case {k} {name}: {e:?}"),
                }
            }
        }
        let (va, vb, vi) = (a.volume(), b.volume(), i.volume());
        assert!((u.volume() - (va + vb - vi)).abs() < 1e-9, "case {k} union");
        assert!((s.volume() - (va - vi)).abs() < 1e-9, "case {k} subtract");
    }
    // edge-touching results are closed but not manifold; they stay a minority
    assert!(touching < 300, "{touching}");
}

#[test]
fn thousand_op_scene_is_fast() {
    let mut r = Lcg(42);
    let mut scene: Vec<Body> = (0..1000).map(|_| random_body(&mut r)).collect();
    let cutter = cylinder(v2(1., 1.), 0.6, -1.0, 4.0, 24);
    let t = std::time::Instant::now();
    for (k, b) in scene.iter_mut().enumerate() {
        *b = match k % 3 {
            0 => b.subtract(&cutter, T),
            1 => b.union(&cutter, T),
            _ => b.intersect(&cutter, T),
        };
    }
    let ms = t.elapsed().as_millis();
    let limit = if cfg!(debug_assertions) {
        20_000
    } else {
        1_000
    };
    assert!(ms < limit, "1k booleans took {ms} ms");
}

fn rot_z(b: &Body, a: f64) -> Body {
    let (s, c) = a.sin_cos();
    let mut out = b.clone();
    for p in &mut out.polys {
        for q in &mut p.pts {
            *q = v3(q.x * c - q.y * s, q.x * s + q.y * c, q.z);
        }
        p.plane = ddc_geom::csg::Plane::of(&p.pts).unwrap();
    }
    out
}

#[test]
fn fuzz_inclined_planes_and_nested_booleans() {
    let mut r = Lcg(0xfeed);
    for k in 0..300 {
        let a = rot_z(&random_body(&mut r), r.f() * 0.7);
        let b = rot_z(&random_body(&mut r), r.f() * 1.3);
        let c = random_body(&mut r);
        let ab = a.union(&b, T);
        let nested = ab.subtract(&c, T).intersect(&a.union(&c, T), T);
        for (name, x) in [("ab", &ab), ("nested", &nested)] {
            if !x.is_empty() {
                x.check_closed(T)
                    .unwrap_or_else(|e| panic!("case {k} {name}: {e:?}"));
            }
        }
        let (va, vb) = (a.volume(), b.volume());
        let vi = a.intersect(&b, T).volume();
        assert!((ab.volume() - (va + vb - vi)).abs() < 1e-7, "case {k}");
        assert!(nested.volume() <= ab.volume() + 1e-7, "case {k}");
    }
}

#[test]
fn validate_rejects_open_nan_and_non_manifold() {
    let mut open = unit();
    open.polys.pop();
    assert!(matches!(open.validate(T), Err(BodyError::OpenEdge(..))));
    let mut nan = unit();
    nan.polys[0].pts[0].x = f64::NAN;
    assert_eq!(nan.validate(T), Err(BodyError::NonFinite));
    // two cubes sharing one edge
    let two = unit().union(&cuboid(v3(1., 1., 0.), v3(2., 2., 1.)), T);
    two.check_closed(T).unwrap();
    assert!(matches!(two.validate(T), Err(BodyError::NonManifold(..))));
}

fn l_shape() -> Body {
    extrude(
        &[
            v2(0., 0.),
            v2(2., 0.),
            v2(2., 1.),
            v2(1., 1.),
            v2(1., 2.),
            v2(0., 2.),
        ],
        0.0,
        1.0,
    )
}

#[test]
fn shell_and_offset_of_non_convex_bodies() {
    let l = l_shape();
    // closed shell: cavity is the L eroded by 0.1 on every side
    let s = shell(&l, 0.1, &[], 9, T).unwrap();
    closed(&s);
    let inner = (1.8 * 0.8 + 0.8 * 1.0) * 0.8;
    assert!((s.volume() - (3.0 - inner)).abs() < 1e-9, "{}", s.volume());
    // open top: the cavity breaks through face 1
    let open = shell(&l, 0.1, &[1], 9, T).unwrap();
    closed(&open);
    let inner = (1.8 * 0.8 + 0.8 * 1.0) * 0.9;
    assert!(
        (open.volume() - (3.0 - inner)).abs() < 1e-9,
        "{}",
        open.volume()
    );
    assert!(!open.polys.iter().any(|p| p.tag == 1 && p.plane.n.z < 0.0));
    // all faces outward: Minkowski sum with a cube, sharp inner corner
    let all = offset_faces(&l, &[], 0.1, 9, T).unwrap();
    closed(&all);
    let area = 2.2 * 1.2 + 1.2 * 1.0;
    assert!((all.volume() - area * 1.2).abs() < 1e-9, "{}", all.volume());
    // all faces inward is the cavity of the closed shell
    let ero = offset_faces(&l, &[], -0.1, 9, T).unwrap();
    closed(&ero);
    assert!((ero.volume() - (1.8 * 0.8 + 0.8 * 1.0) * 0.8).abs() < 1e-9);
    // faces 3 (x=2) and 4 (y=1, inner) meet at a convex edge: mitred corner
    let two = offset_faces(&l, &[3, 4], 0.1, 9, T).unwrap();
    closed(&two);
    assert!((two.volume() - 3.21).abs() < 1e-9, "{}", two.volume());
    assert!((two.tag_area(3) - 1.1).abs() < 1e-9, "{}", two.tag_area(3));
    assert!((two.tag_area(4) - 1.1).abs() < 1e-9, "{}", two.tag_area(4));
    let b = unit();
    assert_eq!(shell(&b, 0.6, &[], 9, T), Err(MassError::EmptyCavity));
    assert_eq!(
        shell(&b, -0.1, &[], 9, T),
        Err(MassError::BadParam("shell thickness"))
    );
    // one face of a non-convex body moves by its own plane
    let o = offset_faces(&l, &[3], 0.5, 9, T).unwrap();
    closed(&o);
    assert!((o.volume() - 3.5).abs() < 1e-9, "{}", o.volume());
}

#[test]
fn fillet_fills_concave_edges_and_does_not_gouge_neighbours() {
    let l = l_shape();
    // inner corner edge between faces 4 (y=1) and 5 (x=1): concave
    let e = edge_between(&l, 4, 5, 1e-9).unwrap();
    assert!(e.concave());
    let r = 0.2;
    let f = fillet(&l, &e, r, 16, 9, T).unwrap();
    closed(&f);
    let sec = 16.0 * 0.5 * r * r * (PI / 2.0 / 16.0).sin();
    assert!(
        (f.volume() - (3.0 + (r * r - sec))).abs() < 1e-9,
        "{}",
        f.volume()
    );
    let top = edge_between(&l, 1, 3, 1e-9).unwrap();
    let c = chamfer(&l, &top, 0.2, 9, T).unwrap();
    closed(&c);
    assert!((c.volume() - (3.0 - 0.02)).abs() < 1e-9, "{}", c.volume());
    // this top edge ends at the concave corner: the cutter must not run into the other wing
    let inner = edge_between(&l, 1, 4, 1e-9).unwrap();
    let c2 = chamfer(&l, &inner, 0.2, 9, T).unwrap();
    closed(&c2);
    assert!((c2.volume() - (3.0 - 0.02)).abs() < 1e-9, "{}", c2.volume());
}

#[test]
fn edge_ops_validate_parameters() {
    let b = unit();
    let e = edge_between(&b, 1, 2, 1e-9).unwrap();
    assert_eq!(
        fillet(&b, &e, 0.0, 8, 9, T),
        Err(MassError::BadParam("fillet radius"))
    );
    assert_eq!(
        chamfer(&b, &e, f64::NAN, 9, T),
        Err(MassError::BadParam("chamfer distance"))
    );
    assert!(edge_between(&b, 1, 0, 1e-9).is_err());
    let s = split_face(&b, 1, v3(0.3, 0., 0.), v3(1., 0., 0.), 9, T).unwrap();
    assert_eq!(
        edge_between(&s, 1, ftag(9, 1), 1e-9).unwrap_err(),
        MassError::Coplanar
    );
    assert_eq!(
        split_face(&b, 1, v3(5., 0., 0.), v3(1., 0., 0.), 9, T).unwrap_err(),
        MassError::NoSplit
    );
}

#[test]
fn curved_faces_are_one_surface_and_reject_push_pull() {
    let prof = [v2(0., 0.), v2(1., 0.), v2(1., 2.), v2(0., 2.)];
    let c = revolve(&prof, TAU, 24);
    closed(&c);
    // bottom, side, top: three surfaces whatever the facet count
    let tags = c.tags();
    assert_eq!(tags.len(), 3, "{tags:?}");
    assert_eq!(push_pull(&c, 3, 0.5, 9, T), Err(MassError::NonPlanar(3)));
    let top = push_pull(&c, 4, 0.5, 9, T).unwrap();
    assert!(top.volume() > c.volume());
}

#[test]
fn blend_with_different_vertex_counts_keeps_corners() {
    let sq = vec![v2(0., 0.), v2(2., 0.), v2(2., 2.), v2(0., 2.)];
    let tri = vec![v2(0., 0.), v2(2., 0.), v2(1., 2.)];
    let r = refine(&tri, 4);
    assert_eq!(r.len(), 4);
    assert!(tri.iter().all(|c| r.contains(c)), "{r:?}");
    let b = blend(&[(sq, 0.0), (tri, 2.0)]).unwrap();
    closed(&b);
    assert!(b.volume() > 0.0);
    assert!(blend(&[(vec![v2(0., 0.)], 0.0)]).is_err());
}

#[test]
fn edge_ids_survive_vertex_insertion() {
    let sq = [v2(0., 0.), v2(2., 0.), v2(2., 2.), v2(0., 2.)];
    let a = extrude_ids(&sq, &[0, 1, 2, 3], 0.0, 1.0).unwrap();
    // a vertex inserted on edge 1 gets a fresh id; the other sides keep theirs
    let five = [v2(0., 0.), v2(2., 0.), v2(2., 1.), v2(2., 2.), v2(0., 2.)];
    let b = extrude_ids(&five, &[0, 1, 4, 2, 3], 0.0, 1.0).unwrap();
    closed(&b);
    for id in [0u32, 2, 3] {
        let (fa, fb) = (a.face_plane(2 + id, 1e-9), b.face_plane(2 + id, 1e-9));
        let (fa, fb) = (fa.unwrap(), fb.unwrap());
        assert!(
            (fa.w - fb.w).abs() < 1e-9 && fa.n.dot(fb.n) > 1.0 - 1e-9,
            "side {id}"
        );
        assert!((a.tag_area(2 + id) - b.tag_area(2 + id)).abs() < 1e-12);
    }
    assert!(extrude_ids(&sq, &[0, 0, 1, 2], 0.0, 1.0).is_err());
    // derived push/pull sides are keyed by the neighbouring face, not by ring position
    let mut ka = FaceKeys::new();
    let pa = push_pull_keyed(&a, 1, 0.5, 9, &mut ka, T).unwrap();
    let mut kb = ka.clone();
    let pb = push_pull_keyed(&b, 1, 0.5, 9, &mut kb, T).unwrap();
    closed(&pb);
    let key = |keys: &FaceKeys, nb: u32| keys.iter().find(|k| k[1] == nb).map(|k| k[3]);
    for nb in [2u32, 4, 5] {
        assert_eq!(key(&ka, nb), key(&kb, nb), "neighbour {nb}");
        // a side flush with its neighbour continues that face (push_pull adopts the tag)
        let k = ftag(9, key(&ka, nb).unwrap());
        assert!(pa.tags().iter().chain(&pb.tags()).all(|t| *t != k));
        let (x, y) = (pa.face_plane(nb, 1e-9), pb.face_plane(nb, 1e-9));
        let (x, y) = (x.unwrap(), y.unwrap());
        assert!(
            (x.w - y.w).abs() < 1e-9 && x.n.dot(y.n) > 1.0 - 1e-9,
            "neighbour {nb}"
        );
    }
    assert!(key(&ka, 6).is_none() && key(&kb, 6).is_some());
    // a blend keeps section edge ids when a vertex is added
    let b1 = blend_ids(&[(sq.to_vec(), 0.0), (sq.to_vec(), 1.0)], &[]).unwrap();
    let b2 = blend_ids(
        &[(five.to_vec(), 0.0), (sq.to_vec(), 1.0)],
        &[vec![0, 1, 8, 2, 3], vec![4, 5, 6, 7]],
    )
    .unwrap();
    closed(&b2);
    let (p1, p2) = (
        b1.face_plane(2 + 8 * 3, 1e-9).unwrap(),
        b2.face_plane(2 + 8 * 3, 1e-9).unwrap(),
    );
    assert!(p1.n.dot(p2.n) > 1.0 - 1e-9 && (p1.w - p2.w).abs() < 1e-9);
}
