use super::*;
use crate::math::{v2, v3};
use crate::member::Shape;
use crate::mesh_ops::tests::{boxm, explode, prism_mesh, rect};
use crate::mesh_ops::{transform, Affine};

/// A Russian category name from the test data file.
fn ru(key: &str) -> String {
    let m: serde_json::Value = serde_json::from_str(include_str!("../tests/data/ru_categories.json")).unwrap();
    m[key].as_str().unwrap_or_else(|| panic!("no ru category {key}")).to_string()
}

fn near(a: f64, b: f64, e: f64) -> bool {
    (a - b).abs() <= e
}

/// Local (x, y, z) → world (x, z, y): profile in elevation, extruded across y.
fn elevation() -> Affine {
    Affine {
        m: [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]],
        t: v3(0.0, 0.0, 0.0),
    }
}

fn top(m: &Mesh, hint: Option<Kind>) -> Candidate {
    let c = recognize(m, hint, &Options::default());
    assert!(!c.is_empty(), "no candidates");
    c.into_iter().next().unwrap()
}

#[test]
fn box_becomes_wall() {
    let m = explode(&boxm(6.0, 0.2, 3.0));
    let c = top(&m, None);
    let Fit::Wall {
        a,
        b,
        z0,
        height,
        thickness,
        openings,
        top,
    } = &c.fit
    else {
        panic!("not a wall: {c:?}");
    };
    assert!(top.is_none(), "flat top");
    assert!(near((*b - *a).len(), 6.0, 1e-9));
    assert!(
        near(a.y, 0.1, 1e-9) && near(b.y, 0.1, 1e-9),
        "axis mid-thickness"
    );
    assert!(near(*z0, 0.0, 1e-9) && near(*height, 3.0, 1e-9) && near(*thickness, 0.2, 1e-9));
    assert!(openings.is_empty());
    assert!(c.confidence > 0.8, "{}", c.confidence);
    assert!(near(c.fit_volume, c.mesh_volume, 1e-9));
}

#[test]
fn wall_holes_and_notches_become_openings() {
    // 8 × 3 elevation: window 1.2×1.5 at x=1, sill 0.9; door notch 1.0×2.1 at x=5
    let outer = vec![
        v2(0.0, 0.0),
        v2(5.0, 0.0),
        v2(5.0, 2.1),
        v2(6.0, 2.1),
        v2(6.0, 0.0),
        v2(8.0, 0.0),
        v2(8.0, 3.0),
        v2(0.0, 3.0),
    ];
    let m = prism_mesh(outer, vec![rect(1.0, 0.9, 2.2, 2.4)], 0.25, &elevation());
    let c = top(&m, Some(Kind::Wall));
    let Fit::Wall { a, openings, .. } = &c.fit else {
        panic!("not a wall: {c:?}");
    };
    assert_eq!(openings.len(), 2, "{openings:?}");
    // along is measured from a; a may be either end
    let from_x0 = a.x < 4.0;
    let along = |x0: f64, w: f64| if from_x0 { x0 } else { 8.0 - x0 - w };
    let win = openings.iter().find(|o| o.sill > 0.1).expect("window");
    assert!(near(win.along, along(1.0, 1.2), 1e-9) && near(win.width, 1.2, 1e-9));
    assert!(near(win.sill, 0.9, 1e-9) && near(win.height, 1.5, 1e-9));
    let door = openings.iter().find(|o| o.sill == 0.0).expect("door");
    assert!(near(door.along, along(5.0, 1.0), 1e-9) && near(door.height, 2.1, 1e-9));
    assert!(near(c.fit_volume, c.mesh_volume, 1e-9));
    assert!(c.confidence > 0.8, "{}", c.confidence);
}

#[test]
fn i_beam_becomes_beam() {
    let sh = Shape::I {
        h: 0.3,
        b: 0.15,
        tw: 0.0071,
        tf: 0.0107,
    };
    let (ring, _) = sh.rings();
    // local (y, z, len) → world (len, y, z)
    let f = Affine {
        m: [[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        t: v3(2.0, 1.0, 3.5),
    };
    let m = explode(&prism_mesh(ring, vec![], 6.0, &f));
    let c = top(&m, None);
    let Fit::Beam {
        a,
        b,
        shape,
        rotation,
    } = &c.fit
    else {
        panic!("not a beam: {c:?}");
    };
    let Shape::I { h, b: w, tw, tf } = shape else {
        panic!("not an I: {shape:?}");
    };
    assert!(near(*h, 0.3, 1e-9) && near(*w, 0.15, 1e-9));
    assert!(near(*tw, 0.0071, 1e-9) && near(*tf, 0.0107, 1e-9));
    assert!(near(rotation.abs(), 0.0, 1e-9), "{rotation}");
    assert!(near(a.x, 2.0, 1e-9) && near(b.x, 8.0, 1e-9));
    assert!(near(a.y, 1.0, 1e-9) && near(a.z, 3.5, 1e-9));
    assert!(c.confidence > 0.8, "{}", c.confidence);
}

#[test]
fn sloped_plate_becomes_roof() {
    let th = 20f64.to_radians();
    // plate 6 (x) × 4 (y) × 0.2, tilted about x: rises along +y
    let f = Affine {
        m: [
            [1.0, 0.0, 0.0],
            [0.0, th.cos(), -th.sin()],
            [0.0, th.sin(), th.cos()],
        ],
        t: v3(0.0, 0.0, 3.0),
    };
    let m = explode(&prism_mesh(rect(0.0, 0.0, 6.0, 4.0), vec![], 0.2, &f));
    let c = top(&m, None);
    let Fit::Roof {
        ring,
        eave,
        slope_deg,
        base_z,
        thickness,
    } = &c.fit
    else {
        panic!("not a roof: {c:?}");
    };
    assert!(near(*slope_deg, 20.0, 1e-6) && near(*thickness, 0.2, 1e-9));
    let n = ring.len();
    let (p, q) = (ring[*eave], ring[(*eave + 1) % n]);
    assert!(
        near(p.y, q.y, 1e-9) && near((q - p).len(), 6.0, 1e-9),
        "eave along x"
    );
    assert!(
        ring.iter().all(|r| r.y >= p.y - 1e-9),
        "eave is the low edge"
    );
    // top surface at the eave: z = 3 + 0.2·cos(th)
    assert!(near(*base_z, 3.0 + 0.2 * th.cos(), 1e-9));
    assert!(c.confidence > 0.8, "{}", c.confidence);
    let all = recognize(&m, Some(Kind::Slab), &Options::default());
    assert_eq!(all[0].kind(), Kind::Slab, "hint picks the sloped slab");
    assert!(matches!(all[0].fit, Fit::SlopedSlab { .. }));
}

#[test]
fn plate_with_hole_becomes_slab() {
    let m = prism_mesh(
        rect(0.0, 0.0, 8.0, 5.0),
        vec![rect(2.0, 2.0, 3.0, 3.0)],
        0.25,
        &Affine::translate(v3(0.0, 0.0, 2.75)),
    );
    let c = top(&m, None);
    let Fit::Slab {
        outer,
        holes,
        top,
        thickness,
    } = &c.fit
    else {
        panic!("not a slab: {c:?}");
    };
    assert_eq!((outer.len(), holes.len()), (4, 1));
    assert!(near(*top, 3.0, 1e-9) && near(*thickness, 0.25, 1e-9));
    assert!(near(c.fit_volume, (40.0 - 1.0) * 0.25, 1e-9));
}

#[test]
fn rect_and_round_columns() {
    let m = prism_mesh(rect(1.8, 2.8, 2.2, 3.2), vec![], 3.0, &Affine::IDENTITY);
    let c = top(&m, None);
    let Fit::Column {
        at, z0, z1, shape, ..
    } = &c.fit
    else {
        panic!("not a column: {c:?}");
    };
    assert!(near(at.x, 2.0, 1e-9) && near(at.y, 3.0, 1e-9));
    assert!(near(*z0, 0.0, 1e-9) && near(*z1, 3.0, 1e-9));
    assert!(matches!(shape, Shape::Rect { b, h } if near(*b, 0.4, 1e-9) && near(*h, 0.4, 1e-9)));
    let (ring, _) = Shape::Circle { d: 0.5 }.rings();
    let m = prism_mesh(ring, vec![], 3.0, &Affine::translate(v3(5.0, 5.0, 0.0)));
    let c = top(&m, None);
    let Fit::Column { at, shape, .. } = &c.fit else {
        panic!("not a column: {c:?}");
    };
    assert!(near(at.x, 5.0, 1e-9) && near(at.y, 5.0, 1e-9));
    assert!(matches!(shape, Shape::Circle { d } if near(*d, 0.5, 1e-9)));
    assert!(c.confidence > 0.8, "{}", c.confidence);
}

#[test]
fn rotated_column_keeps_rotation() {
    let a = 30f64.to_radians();
    let f = Affine::rotate_z(v3(0.0, 0.0, 0.0), a);
    let m = transform(
        &prism_mesh(rect(-0.3, -0.15, 0.3, 0.15), vec![], 3.0, &Affine::IDENTITY),
        &f,
    );
    let c = top(&m, Some(Kind::Column));
    let Fit::Column {
        shape, rotation, ..
    } = &c.fit
    else {
        panic!("not a column: {c:?}");
    };
    let Shape::Rect { b, h } = shape else {
        panic!("{shape:?}")
    };
    // b along the rotation angle
    assert!(near(*b, 0.6, 1e-9) && near(*h, 0.3, 1e-9));
    assert!(near(*rotation, a, 1e-9), "{rotation}");
}

#[test]
fn odd_prism_is_a_generic_extrusion() {
    let tri = vec![v2(0.0, 0.0), v2(3.0, 0.0), v2(1.0, 2.0)];
    let m = prism_mesh(tri, vec![], 2.5, &Affine::IDENTITY);
    let c = recognize(&m, Some(Kind::Extrusion), &Options::default());
    let e = c
        .iter()
        .find(|c| c.kind() == Kind::Extrusion)
        .expect("extrusion");
    let Fit::Extrusion(x) = &e.fit else {
        unreachable!()
    };
    assert!(near(x.depth, 2.5, 1e-9) && x.outer.len() == 3);
    let j = x.to_json();
    assert!(
        j.starts_with("{\"plane\":{\"o\":") && j.contains("\"outer\":[["),
        "{j}"
    );
    assert!(x.def_json().ends_with("\"start\":0,\"end\":2.5}"), "{j}");
}

#[test]
fn bulges_from_polygon_arcs() {
    let (ring, _) = Shape::Circle { d: 2.0 }.rings();
    let a = with_arcs(&ring, 1e-9);
    assert_eq!(a.len(), 2);
    assert!(near(a[0].1, 1.0, 1e-9) && near(a[1].1, 1.0, 1e-9), "{a:?}");
    let sq = rect(0.0, 0.0, 1.0, 1.0);
    assert!(with_arcs(&sq, 1e-9).iter().all(|p| p.1 == 0.0));
}

#[test]
fn categories_hint_kinds() {
    assert_eq!(hint_from_category("IfcWallStandardCase"), Some(Kind::Wall));
    assert_eq!(hint_from_category(&ru("slab")), Some(Kind::Slab));
    assert_eq!(
        hint_from_category("Structural Framing Beam"),
        Some(Kind::Beam)
    );
    assert_eq!(hint_from_category(&ru("column")), Some(Kind::Column));
    assert_eq!(hint_from_category("Roofs"), Some(Kind::Roof));
    assert_eq!(hint_from_category("Curtain Wall"), None);
    assert_eq!(hint_from_category("Furniture"), None);
}

#[test]
fn foreign_categories_never_become_walls_or_columns() {
    let o = Options::default();
    // wardrobe 0.6 × 0.6 × 2.0: a column by shape alone
    let wardrobe = prism_mesh(rect(0.0, 0.0, 0.6, 0.6), vec![], 2.0, &Affine::IDENTITY);
    let free = recognize(&wardrobe, None, &o);
    assert!(free.iter().any(|c| c.kind() == Kind::Column), "{free:?}");
    assert!(
        free.iter().all(|c| !c.consistent),
        "no category, nothing to agree with"
    );
    for cat in ["Furniture".to_string(), ru("furniture"), "Casework".to_string()] {
        let cat = cat.as_str();
        let c = recognize_category(&wardrobe, cat, None, &o);
        assert!(
            c.iter()
                .all(|c| !matches!(c.kind(), Kind::Column | Kind::Wall)),
            "{cat}: {c:?}"
        );
        assert!(c
            .iter()
            .all(|c| c.kind() == Kind::Extrusion || c.confidence <= FOREIGN_CAP));
        assert!(c.iter().all(|c| !c.consistent));
    }
    // door leaf 0.9 × 0.04 × 2.1 and a glass panel 1.5 × 0.02 × 3.0
    let door = prism_mesh(rect(0.0, 0.0, 0.9, 0.04), vec![], 2.1, &Affine::IDENTITY);
    let glass = prism_mesh(rect(0.0, 0.0, 1.5, 0.02), vec![], 3.0, &Affine::IDENTITY);
    let windows = ru("windows");
    for (m, cat) in [
        (&door, "Doors"),
        (&glass, "Curtain Panels"),
        (&glass, windows.as_str()),
    ] {
        let c = recognize_category(m, cat, None, &o);
        assert!(c.iter().all(|c| c.kind() != Kind::Wall), "{cat}: {c:?}");
    }
    // the same thin short plate without a category: a wall at half confidence at most
    let w = recognize(&door, None, &o);
    assert!(
        w.iter()
            .filter(|c| c.kind() == Kind::Wall)
            .all(|c| c.confidence < 0.6),
        "{w:?}"
    );
    // other foreign categories keep native kinds, never above the cap
    let slab = prism_mesh(rect(0.0, 0.0, 4.0, 3.0), vec![], 0.2, &Affine::IDENTITY);
    let c = recognize_category(&slab, "Generic Models", None, &o);
    let s = c
        .iter()
        .find(|c| c.kind() == Kind::Slab)
        .expect("slab kept");
    assert!(s.confidence <= FOREIGN_CAP && !s.consistent);
    assert_eq!(gate_from_category("Plumbing Fixtures"), Gate::Foreign);
    assert_eq!(gate_from_category("Walls"), Gate::Open);
}

#[test]
fn category_hit_is_consistent() {
    let m = prism_mesh(rect(0.0, 0.0, 6.0, 0.2), vec![], 3.0, &Affine::IDENTITY);
    let c = recognize_category(&m, "Walls", None, &Options::default());
    assert_eq!(c[0].kind(), Kind::Wall);
    assert!(c[0].consistent && c[0].confidence > 0.9);
    assert!(c.iter().skip(1).all(|c| !c.consistent));
}

#[test]
fn mitred_wall_stays_confident() {
    // 5 m wall, 0.3 thick, both ends cut at 45° (corner joins)
    let plan = vec![v2(0.0, 0.0), v2(5.0, 0.0), v2(4.7, 0.3), v2(0.3, 0.3)];
    let m = explode(&prism_mesh(plan, vec![], 3.0, &Affine::IDENTITY));
    let c = top(&m, None);
    let Fit::Wall {
        a, b, thickness, ..
    } = &c.fit
    else {
        panic!("not a wall: {c:?}");
    };
    assert!(near(*thickness, 0.3, 1e-9));
    assert!(near((*b - *a).len(), 4.7, 1e-6), "mid-thickness length");
    assert!(c.confidence >= 0.8, "{}", c.confidence);
    // a short mitred wall (2 m) still reads as a wall
    let plan = vec![v2(0.0, 0.0), v2(2.0, 0.0), v2(1.7, 0.3), v2(0.3, 0.3)];
    let all = recognize(
        &prism_mesh(plan, vec![], 3.0, &Affine::IDENTITY),
        None,
        &Options::default(),
    );
    let w = all.iter().find(|c| c.kind() == Kind::Wall).expect("wall");
    assert!(w.confidence >= 0.6, "{}", w.confidence);
}

#[test]
fn gable_wall_keeps_top_profile() {
    let gable = vec![
        v2(0.0, 0.0),
        v2(6.0, 0.0),
        v2(6.0, 3.0),
        v2(3.0, 4.5),
        v2(0.0, 3.0),
    ];
    let m = prism_mesh(gable, vec![], 0.25, &elevation());
    let c = top(&m, Some(Kind::Wall));
    let Fit::Wall { height, top, .. } = &c.fit else {
        panic!("not a wall: {c:?}");
    };
    assert!(near(*height, 4.5, 1e-9));
    let t = top.as_ref().expect("top profile");
    assert_eq!(t.len(), 3, "{t:?}");
    assert!(t.windows(2).all(|w| w[1].0 > w[0].0));
    assert!(near(t[0].0, 0.0, 1e-9) && near(t[1].0, 3.0, 1e-9) && near(t[2].0, 6.0, 1e-9));
    assert!(near(t[0].1, 3.0, 1e-9) && near(t[1].1, 4.5, 1e-9) && near(t[2].1, 3.0, 1e-9));
    assert!(
        near(c.fit_volume, c.mesh_volume, 1e-9),
        "{} vs {}",
        c.fit_volume,
        c.mesh_volume
    );
    assert!(!c.notes.contains(&"top_not_flat"), "{:?}", c.notes);
    assert!(c.confidence > 0.8, "{}", c.confidence);
}

fn ngon(n: usize, r: f64) -> Vec<V2> {
    use std::f64::consts::{PI, TAU};
    (0..n)
        .map(|i| {
            let a = TAU * i as f64 / n as f64 + PI / n as f64;
            v2(r * a.cos(), r * a.sin())
        })
        .collect()
}

#[test]
fn octagon_is_a_polygon_not_a_circle() {
    let m = prism_mesh(ngon(8, 0.3), vec![], 3.0, &Affine::IDENTITY);
    let c = top(&m, Some(Kind::Column));
    let Fit::Column { shape, .. } = &c.fit else {
        panic!("not a column: {c:?}");
    };
    assert!(matches!(shape, Shape::Poly(p) if p.len() == 8), "{shape:?}");
    assert!(near(c.fit_volume, c.mesh_volume, 1e-9));
    assert!(with_arcs(&ngon(8, 0.3), 1e-9).iter().all(|p| p.1 == 0.0));
    // 16 sides read as a circle, confidence reduced by the lost area
    let m = prism_mesh(ngon(16, 0.3), vec![], 3.0, &Affine::IDENTITY);
    let c = top(&m, Some(Kind::Column));
    assert!(
        matches!(
            c.fit,
            Fit::Column {
                shape: Shape::Circle { .. },
                ..
            }
        ),
        "{c:?}"
    );
    // the hint bonus saturates confidence: the reduction shows without a hint
    let c = top(&m, None);
    assert!(
        matches!(c.fit, Fit::Column { .. }) && c.confidence < 1.0,
        "{c:?}"
    );
}

/// Rolled I outline with root fillets of radius `r` (`k` segments each), counter-clockwise.
fn filleted_i(h: f64, b: f64, tw: f64, tf: f64, r: f64, k: usize) -> Vec<V2> {
    let (hb, hh, hw) = (b / 2.0, h / 2.0, tw / 2.0);
    let arc = |out: &mut Vec<V2>, c: V2, a0: f64, a1: f64| {
        for i in 0..=k {
            let a = (a0 + (a1 - a0) * i as f64 / k as f64).to_radians();
            out.push(v2(c.x + r * a.cos(), c.y + r * a.sin()));
        }
    };
    let mut p = vec![v2(-hb, -hh), v2(hb, -hh), v2(hb, -hh + tf)];
    arc(&mut p, v2(hw + r, -hh + tf + r), -90.0, -180.0);
    arc(&mut p, v2(hw + r, hh - tf - r), 180.0, 90.0);
    p.extend([v2(hb, hh - tf), v2(hb, hh), v2(-hb, hh), v2(-hb, hh - tf)]);
    arc(&mut p, v2(-hw - r, hh - tf - r), 90.0, 0.0);
    arc(&mut p, v2(-hw - r, -hh + tf + r), 0.0, -90.0);
    p.push(v2(-hb, -hh + tf));
    p
}

#[test]
fn rolled_i_with_fillets_is_an_i() {
    let ring = filleted_i(0.3, 0.15, 0.0071, 0.0107, 0.015, 4);
    let f = Affine {
        m: [[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        t: v3(0.0, 0.0, 3.0),
    };
    let m = prism_mesh(ring, vec![], 6.0, &f);
    let c = top(&m, Some(Kind::Beam));
    let Fit::Beam { shape, .. } = &c.fit else {
        panic!("not a beam: {c:?}");
    };
    let Shape::I { h, b, tw, tf } = shape else {
        panic!("not an I: {shape:?}");
    };
    assert!(near(*h, 0.3, 1e-9) && near(*b, 0.15, 1e-9));
    assert!(
        near(*tw, 0.0071, 1e-9) && near(*tf, 0.0107, 1e-6),
        "{tw} {tf}"
    );
    assert!(c.confidence > 0.8, "{}", c.confidence);
}

#[test]
fn t_junction_mesh_is_recognized() {
    // 6 × 0.2 × 3 box; the top face carries an extra vertex on its long front edge
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
    let st = crate::mesh_ops::stats(&m, 1e-7);
    assert!(
        st.closed && st.oriented,
        "T-junction healed by weld: {st:?}"
    );
    let c = top(&m, None);
    assert_eq!(c.kind(), Kind::Wall);
    assert!(c.confidence > 0.8, "{}", c.confidence);
}
