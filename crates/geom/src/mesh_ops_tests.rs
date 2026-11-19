use super::*;
use crate::math::{v2, v3, Tol, V2};
use crate::mesh::Mesh;
use crate::profile::Profile;
use crate::Solid;

/// Prism: profile (x, y) extruded along z by `depth`, then mapped by `f` (local → world).
pub(crate) fn prism_mesh(outer: Vec<V2>, holes: Vec<Vec<V2>>, depth: f64, f: &Affine) -> Mesh {
    let p = Profile::new(outer, holes, Tol::default()).unwrap();
    let m = Solid::extrude(&p, depth, Tol::default())
        .triangulate()
        .unwrap();
    transform(&m, f)
}

pub(crate) fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<V2> {
    vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)]
}

pub(crate) fn boxm(x: f64, y: f64, z: f64) -> Mesh {
    prism_mesh(rect(0.0, 0.0, x, y), vec![], z, &Affine::IDENTITY)
}

/// Each triangle has its own vertices (like import tessellation).
pub(crate) fn explode(m: &Mesh) -> Mesh {
    let mut out = Mesh::default();
    for (i, t) in m.triangles.iter().enumerate() {
        let b = out.positions.len() as u32;
        out.positions
            .extend(t.iter().map(|&k| m.positions[k as usize]));
        out.triangles.push([b, b + 1, b + 2]);
        out.face_of.push(m.face_of.get(i).copied().unwrap_or(0));
    }
    out
}

const TOL: f64 = 1e-7;

fn near(a: f64, b: f64, e: f64) -> bool {
    (a - b).abs() <= e
}

#[test]
fn box_section_mid_is_one_ccw_region() {
    let m = boxm(4.0, 3.0, 2.0);
    let s = section(&m, &Plane::horizontal(1.0), TOL);
    assert_eq!(s.regions.len(), 1);
    assert!(s.open.is_empty());
    let r = &s.regions[0];
    assert_eq!(r.outer.len(), 4, "collinear points removed: {:?}", r.outer);
    assert!(signed_area(&r.outer) > 0.0);
    assert!(near(r.area(), 12.0, 1e-9));
}

#[test]
fn section_is_half_open_at_faces() {
    let m = boxm(4.0, 3.0, 2.0);
    let low = section(&m, &Plane::horizontal(0.0), TOL);
    assert!(
        near(low.area(), 12.0, 1e-9),
        "bottom face plane cuts the body"
    );
    let top = section(&m, &Plane::horizontal(2.0), TOL);
    assert!(top.regions.is_empty() && top.open.is_empty());
}

#[test]
fn section_of_exploded_mesh_with_hole_nests_the_hole() {
    let m = prism_mesh(
        rect(0.0, 0.0, 6.0, 4.0),
        vec![rect(2.0, 1.0, 3.0, 2.5)],
        3.0,
        &Affine::IDENTITY,
    );
    let s = section(&explode(&m), &Plane::horizontal(1.5), TOL);
    assert_eq!(s.regions.len(), 1);
    assert_eq!(s.regions[0].holes.len(), 1);
    assert!(signed_area(&s.regions[0].holes[0]) < 0.0);
    assert!(near(s.area(), 24.0 - 1.5, 1e-9));
}

#[test]
fn vertical_section_frame_is_along_line_and_up() {
    let m = boxm(4.0, 2.0, 3.0);
    let pl = Plane::vertical(v2(-1.0, 1.0), v2(5.0, 1.0)).unwrap();
    let s = section(&m, &pl, TOL);
    assert_eq!(s.regions.len(), 1);
    let (x0, x1, y0, y1) = s.regions[0].outer.iter().fold(
        (f64::MAX, f64::MIN, f64::MAX, f64::MIN),
        |(a, b, c, d), p| (a.min(p.x), b.max(p.x), c.min(p.y), d.max(p.y)),
    );
    // u along a→b from the origin projection, v = z
    assert!(near(x1 - x0, 4.0, 1e-9) && near(y0, 0.0, 1e-9) && near(y1, 3.0, 1e-9));
}

#[test]
fn open_surface_section_is_a_polyline() {
    let m = Mesh {
        positions: vec![
            v3(0.0, 0.0, 0.0),
            v3(5.0, 0.0, 0.0),
            v3(5.0, 0.0, 3.0),
            v3(0.0, 0.0, 3.0),
        ],
        triangles: vec![[0, 1, 2], [0, 2, 3]],
        face_of: vec![0, 0],
    };
    let s = section(&m, &Plane::horizontal(1.0), TOL);
    assert!(s.regions.is_empty());
    assert_eq!(s.open.len(), 1);
    let l = &s.open[0];
    assert!(near((l[l.len() - 1] - l[0]).len(), 5.0, 1e-9));
}

#[test]
fn cut_keeps_closed_capped_half() {
    let m = explode(&boxm(4.0, 3.0, 2.0));
    let below = cut(&m, &Plane::horizontal(0.5), Keep::Below, TOL);
    assert!(below.capped);
    let st = stats(&below.mesh, TOL);
    assert!(st.closed && st.oriented, "{st:?}");
    assert!(near(st.volume, 6.0, 1e-9));
    assert!(near(st.max.z, 0.5, 1e-12));
    let above = cut(&m, &Plane::horizontal(0.5), Keep::Above, TOL);
    let sa = stats(&above.mesh, TOL);
    assert!(sa.oriented && near(sa.volume, 18.0, 1e-9));
}

#[test]
fn split_through_a_hole_sums_volume() {
    let m = prism_mesh(
        rect(0.0, 0.0, 6.0, 4.0),
        vec![rect(2.0, 1.0, 3.0, 2.5)],
        0.3,
        &Affine::IDENTITY,
    );
    let total = stats(&m, TOL).volume;
    let pl = Plane::vertical(v2(2.5, -1.0), v2(2.5, 5.0)).unwrap();
    let (a, b) = split(&m, &pl, TOL);
    assert!(a.capped && b.capped);
    let (sa, sb) = (stats(&a.mesh, TOL), stats(&b.mesh, TOL));
    assert!(sa.oriented && sb.oriented);
    assert!(near(sa.volume + sb.volume, total, 1e-9));
    assert!(near(
        sa.volume.min(sb.volume),
        (2.5 * 4.0 - 0.5 * 1.5) * 0.3,
        1e-9
    ));
}

#[test]
fn split_ring_cap_has_hole() {
    // cut across a tube: each cap is an annulus
    let m = prism_mesh(
        rect(-1.0, -1.0, 1.0, 1.0),
        vec![rect(-0.5, -0.5, 0.5, 0.5)],
        4.0,
        &Affine::IDENTITY,
    );
    let (a, b) = split(&m, &Plane::horizontal(1.0), TOL);
    let (sa, sb) = (stats(&a.mesh, TOL), stats(&b.mesh, TOL));
    assert!(sa.oriented && sb.oriented);
    assert!(near(sa.volume, 3.0, 1e-9) && near(sb.volume, 9.0, 1e-9));
}

#[test]
fn weld_and_clean_report() {
    let m = boxm(1.0, 1.0, 1.0);
    let mut e = explode(&m);
    let t0 = e.triangles[0];
    e.triangles.push(t0); // duplicate
    let b = e.positions.len() as u32;
    e.positions.extend([v3(0.0, 0.0, 0.0); 3]);
    e.triangles.push([b, b + 1, b + 2]); // degenerate
    e.face_of.extend([0, 0]);
    let (w, rep) = clean(&e, TOL);
    assert_eq!(w.positions.len(), 8);
    assert_eq!(w.triangles.len(), m.triangles.len());
    assert_eq!((rep.duplicate, rep.degenerate), (1, 1));
    assert!(stats(&w, TOL).oriented);
}

#[test]
fn mirror_keeps_outward_orientation() {
    let m = boxm(2.0, 1.0, 1.0);
    let f = Affine::mirror(Plane::vertical(v2(5.0, 0.0), v2(5.0, 1.0)).unwrap());
    let r = transform(&m, &f);
    let st = stats(&r, TOL);
    assert!(st.oriented && near(st.volume, 2.0, 1e-12));
    assert!(near(st.min.x, 8.0, 1e-12) && near(st.max.x, 10.0, 1e-12));
}

#[test]
fn affine_compose_rotate_scale() {
    let c = v3(1.0, 1.0, 0.0);
    let a =
        Affine::rotate_z(c, std::f64::consts::FRAC_PI_2).then(&Affine::scale(c, v3(2.0, 2.0, 2.0)));
    let p = a.apply(v3(2.0, 1.0, 0.0));
    assert!(near(p.x, 1.0, 1e-12) && near(p.y, 3.0, 1e-12));
    assert!(near(a.det(), 8.0, 1e-12) && a.is_valid());
    let st = stats(&transform(&boxm(1.0, 1.0, 1.0), &a), TOL);
    assert!(near(st.volume, 8.0, 1e-9));
}

#[test]
fn convexity_and_merge_delete() {
    let b = boxm(1.0, 1.0, 1.0);
    assert_eq!(stats(&b, TOL).convex, Some(true));
    let l = prism_mesh(
        vec![
            v2(0.0, 0.0),
            v2(2.0, 0.0),
            v2(2.0, 1.0),
            v2(1.0, 1.0),
            v2(1.0, 2.0),
            v2(0.0, 2.0),
        ],
        vec![],
        1.0,
        &Affine::IDENTITY,
    );
    let sl = stats(&l, TOL);
    assert!(sl.oriented && sl.convex == Some(false) && near(sl.volume, 3.0, 1e-12));
    let two = merge(&[&b, &transform(&b, &Affine::translate(v3(3.0, 0.0, 0.0)))]);
    let st = stats(&two, TOL);
    assert!(st.oriented && near(st.volume, 2.0, 1e-12));
    let holed = delete_faces(&b, &[0]);
    let sh = stats(&holed, TOL);
    assert!(!sh.closed && sh.boundary_edges == 3);
}

#[test]
fn weld_keeps_needles_clean_drops_them() {
    let mut e = explode(&boxm(1.0, 1.0, 1.0));
    let n = e.triangles.len();
    let b = e.positions.len() as u32;
    // zero-area needle with three distinct vertices along the bottom front edge
    e.positions
        .extend([v3(0.0, 0.0, 0.0), v3(0.5, 0.0, 0.0), v3(1.0, 0.0, 0.0)]);
    e.triangles.push([b, b + 1, b + 2]);
    e.face_of.push(0);
    assert_eq!(
        weld(&e, TOL).triangles.len(),
        n + 1,
        "weld keeps the needle"
    );
    let (c, rep) = clean(&e, TOL);
    assert_eq!((c.triangles.len(), rep.degenerate), (n, 1));
}

/// 6 × 0.2 × 3 box whose top face has an extra vertex on its long front edge.
fn t_junction_box() -> Mesh {
    let full = explode(&boxm(6.0, 0.2, 3.0));
    let mut m = Mesh::default();
    let tri = |m: &mut Mesh, ps: [V3; 3]| {
        let b = m.positions.len() as u32;
        m.positions.extend(ps);
        m.triangles.push([b, b + 1, b + 2]);
        m.face_of.push(0);
    };
    for t in &full.triangles {
        let ps = t.map(|k| full.positions[k as usize]);
        if !ps.iter().all(|p| p.z > 2.999) {
            tri(&mut m, ps);
        }
    }
    let q = [
        v3(0.0, 0.0, 3.0),
        v3(3.0, 0.0, 3.0),
        v3(6.0, 0.0, 3.0),
        v3(6.0, 0.2, 3.0),
        v3(0.0, 0.2, 3.0),
    ];
    for [i, j, k] in [[0usize, 1, 4], [1, 2, 3], [1, 3, 4]] {
        tri(&mut m, [q[i], q[j], q[k]]);
    }
    m
}

#[test]
fn t_junctions_are_split() {
    let m = t_junction_box();
    let (c, rep) = clean(&m, TOL);
    assert!(rep.t_junctions >= 1, "{rep:?}");
    let st = stats(&c, TOL);
    assert!(st.closed && st.oriented, "{st:?}");
    assert!(near(st.volume, 6.0 * 0.2 * 3.0, 1e-9));
    // a vertical section through the junction closes
    let pl = Plane::vertical(v2(2.5, -1.0), v2(2.5, 1.0)).unwrap();
    let s = section(&m, &pl, TOL);
    assert_eq!((s.regions.len(), s.open.len()), (1, 0));
    assert!(near(s.area(), 0.2 * 3.0, 1e-9));
}

#[test]
fn section_stitches_cracks() {
    // box 4 × 3 × 2; the x = 4 face is shifted 1e-6 at its top corner (beyond the weld tolerance)
    let mut e = explode(&boxm(4.0, 3.0, 2.0));
    let tris = e.triangles.clone();
    for t in tris {
        let [a, b, c] = t.map(|k| e.positions[k as usize]);
        let n = (b - a).cross(c - a);
        if n.x > 0.0 && n.x.abs() > n.y.abs() && n.x.abs() > n.z.abs() {
            for k in t {
                let p = &mut e.positions[k as usize];
                if (*p - v3(4.0, 3.0, 2.0)).len() < 1e-9 {
                    *p = v3(4.0 + 1e-6, 3.0, 2.0);
                }
            }
        }
    }
    let s = section(&e, &Plane::horizontal(1.0), TOL);
    assert_eq!((s.regions.len(), s.open.len()), (1, 0), "{s:?}");
    assert!(near(s.area(), 12.0, 1e-5));
}

fn tri_area(pts: &[V2], t: &[[usize; 3]]) -> f64 {
    t.iter()
        .map(|&[a, b, c]| (pts[b] - pts[a]).cross(pts[c] - pts[a]) / 2.0)
        .sum()
}

#[test]
fn triangulate_region_with_hole_and_collinear_rim() {
    // collinear midpoint on the outer bottom edge must stay a vertex
    let outer = vec![
        v2(0.0, 0.0),
        v2(2.0, 0.0),
        v2(4.0, 0.0),
        v2(4.0, 4.0),
        v2(0.0, 4.0),
    ];
    let hole = rect(1.0, 1.0, 3.0, 3.0);
    let t = triangulate_region(&outer, &[hole.clone()]).expect("triangulated");
    let pts: Vec<V2> = outer.iter().chain(&hole).copied().collect();
    assert_eq!(t.len(), pts.len() + 2 - 2, "n + 2h - 2 triangles");
    assert!(near(tri_area(&pts, &t), 12.0, 1e-12));
    assert!(t
        .iter()
        .all(|&[a, b, c]| (pts[b] - pts[a]).cross(pts[c] - pts[a]) >= -1e-12));
    for i in 0..pts.len() {
        assert!(t.iter().any(|x| x.contains(&i)), "vertex {i} used");
    }
}

#[test]
fn triangulate_region_large_ring() {
    // 20 000-vertex star-ish ring: must finish quickly and cover the area
    let n = 20_000;
    let ring: Vec<V2> = (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            let r = if i % 2 == 0 { 10.0 } else { 9.5 };
            v2(r * a.cos(), r * a.sin())
        })
        .collect();
    let t = triangulate_region(&ring, &[]).expect("triangulated");
    assert_eq!(t.len(), n - 2);
    assert!(near(tri_area(&ring, &t), signed_area(&ring), 1e-6));
}

#[test]
fn cut_reports_source_closedness() {
    let closed = cut(
        &boxm(1.0, 1.0, 1.0),
        &Plane::horizontal(0.5),
        Keep::Below,
        TOL,
    );
    assert!(closed.source_closed && closed.capped);
    let sheet = Mesh {
        positions: vec![
            v3(0.0, 0.0, 0.0),
            v3(5.0, 0.0, 0.0),
            v3(5.0, 0.0, 3.0),
            v3(0.0, 0.0, 3.0),
        ],
        triangles: vec![[0, 1, 2], [0, 2, 3]],
        face_of: vec![0, 0],
    };
    let open = cut(&sheet, &Plane::horizontal(1.0), Keep::Below, TOL);
    assert!(!open.source_closed, "an open sheet may stay open");
}
