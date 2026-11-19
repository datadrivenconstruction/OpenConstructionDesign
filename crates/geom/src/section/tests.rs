use super::*;

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<V2> {
    vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)]
}

fn boxp(x0: f64, y0: f64, x1: f64, y1: f64, z0: f64, z1: f64) -> Prism {
    Prism {
        rings: vec![rect(x0, y0, x1, y1)],
        bottom: Zfn::flat(z0),
        top: Zfn::flat(z1),
        voids: vec![],
    }
}

fn frame(o: (f64, f64), u: (f64, f64), cut: bool) -> Frame {
    Frame {
        o: v2(o.0, o.1),
        u: v2(u.0, u.1),
        far: None,
        cut,
    }
}

fn len(ls: &[Line2]) -> f64 {
    ls.iter().map(|l| (l.b - l.a).len()).sum()
}

fn cut_area(c: &CutRegion) -> f64 {
    c.rings.iter().map(|r| signed_area(r)).sum()
}

#[test]
fn section_box_behind_box_exact_visibility() {
    let ps = [
        boxp(0.0, 0.0, 2.0, 1.0, 0.0, 3.0),
        boxp(1.0, 2.0, 3.0, 3.0, 0.5, 2.5),
    ];
    // South elevation: A is the full 2×3 (10), B is right of A: bottom 1 + side 2 + top 1.
    let e = section(&ps, &frame((-1.0, -5.0), (1.0, 0.0), false));
    assert!(e.cuts.is_empty());
    let a: Vec<Line2> = e.lines.iter().filter(|l| l.item == 0).cloned().collect();
    let b: Vec<Line2> = e.lines.iter().filter(|l| l.item == 1).cloned().collect();
    assert!((len(&a) - 10.0).abs() < 1e-9, "{a:?}");
    assert!((len(&b) - 4.0).abs() < 1e-9, "{b:?}");
    // Section through A: 2×3 cut, B visible to the right of the cut.
    let s = section(&ps, &frame((-1.0, 0.5), (1.0, 0.0), true));
    assert_eq!(s.cuts.len(), 1);
    assert!((cut_area(&s.cuts[0]) - 6.0).abs() < 1e-12);
    assert_eq!(s.cuts[0].rings[0].len(), 4);
    assert!(s.lines.iter().all(|l| l.item == 1));
    assert!((len(&s.lines) - 4.0).abs() < 1e-9, "{:?}", s.lines);
    // Far boundary in front of B: no lines.
    let mut f = frame((-1.0, 0.5), (1.0, 0.0), true);
    f.far = Some(1.0);
    assert!(section(&ps, &f).lines.is_empty());
}

#[test]
fn section_through_door_void() {
    let mut w = boxp(0.0, 0.0, 4.0, 0.2, 0.0, 3.0);
    w.voids.push(Void {
        o: v2(0.0, 0.0),
        dir: v2(1.0, 0.0),
        u0: 1.0,
        u1: 1.9,
        z0: 0.0,
        z1: 2.1,
    });
    // Across the wall through the door, looking west (−x): s = y + 1.
    let s = section(&[w.clone()], &frame((1.45, -1.0), (0.0, 1.0), true));
    assert_eq!(s.cuts.len(), 1);
    assert!((cut_area(&s.cuts[0]) - 0.18).abs() < 1e-12);
    let r = &s.cuts[0].rings[0];
    let zs: Vec<f64> = r.iter().map(|p| p.y).collect();
    assert!(zs
        .iter()
        .all(|&z| (z - 2.1).abs() < 1e-12 || (z - 3.0).abs() < 1e-12));
    // The reveal at x = 1 is visible: bottom 0.2 and two jambs 2.1 (the top is hidden by the cut).
    assert!((len(&s.lines) - 4.4).abs() < 1e-9, "{:?}", s.lines);
    // Elevation: face outline with the door cut-out 4+3+4+3 − 0.9 + 2·2.1 + 0.9.
    let e = section(&[w], &frame((-1.0, -5.0), (1.0, 0.0), false));
    assert!((len(&e.lines) - 18.2).abs() < 1e-9, "{:?}", e.lines);
}

#[test]
fn section_along_gable_top_profile() {
    let mut w = boxp(0.0, 0.0, 4.0, 0.2, 0.0, 0.0);
    w.top = Zfn {
        o: v2(0.0, 0.0),
        z: 0.0,
        grad: v2(0.0, 0.0),
        dir: v2(1.0, 0.0),
        prof: vec![(0.0, 3.0), (2.0, 4.0), (4.0, 3.0)],
    };
    let s = section(&[w.clone()], &frame((-1.0, 0.1), (1.0, 0.0), true));
    assert_eq!(s.cuts.len(), 1);
    assert!((cut_area(&s.cuts[0]) - 14.0).abs() < 1e-12);
    assert_eq!(s.cuts[0].rings[0].len(), 5);
    let e = section(&[w], &frame((-1.0, -5.0), (1.0, 0.0), false));
    let want = 10.0 + 2.0 * 5f64.sqrt();
    assert!((len(&e.lines) - want).abs() < 1e-9, "{:?}", e.lines);
}

#[test]
fn section_sloped_slab_top_visible() {
    let top = Zfn {
        o: v2(0.0, 0.0),
        z: 1.0,
        grad: v2(0.0, 0.25),
        dir: v2(0.0, 0.0),
        prof: vec![],
    };
    let mut bottom = top.clone();
    bottom.z -= 0.2;
    let p = Prism {
        rings: vec![rect(0.0, 0.0, 4.0, 4.0)],
        bottom,
        top,
        voids: vec![],
    };
    // From the south the top rises away from the viewer and is visible: end face 8.4 + far edge 4 + sides 2·1.
    let e = section(&[p.clone()], &frame((-1.0, -5.0), (1.0, 0.0), false));
    assert!((len(&e.lines) - 14.4).abs() < 1e-9, "{:?}", e.lines);
    // From the north the top is not visible but the bottom is (it falls away from the viewer): end face + edge + sides.
    let n = section(&[p.clone()], &frame((5.0, 9.0), (-1.0, 0.0), false));
    assert!((len(&n.lines) - 14.4).abs() < 1e-9, "{:?}", n.lines);
    // Section at x = 2: parallelogram 4 × 0.2.
    let s = section(&[p], &frame((2.0, -1.0), (0.0, 1.0), true));
    assert!((cut_area(&s.cuts[0]) - 0.8).abs() < 1e-12);
}

#[test]
fn line_intervals_with_hole() {
    let mut hole = rect(1.0, 1.0, 2.0, 2.0);
    hole.reverse();
    let iv = line_intervals(
        &[rect(0.0, 0.0, 3.0, 3.0), hole],
        v2(-1.0, 1.5),
        v2(1.0, 0.0),
    );
    assert_eq!(iv, vec![(1.0, 2.0), (3.0, 4.0)]);
}

/// Grid of 51×50 + 50×51 walls, 4 m each with a door in each, section through the middle; < 1 s in release.
#[test]
#[ignore]
fn section_budget_5000_walls() {
    let mut ps = vec![];
    for i in 0..=50 {
        for j in 0..50 {
            let (a, b) = (i as f64 * 4.0, j as f64 * 4.0);
            let door = |o: V2, dir: V2| Void {
                o,
                dir,
                u0: 1.0,
                u1: 1.9,
                z0: 0.0,
                z1: 2.1,
            };
            let mut h = boxp(b, a - 0.1, b + 4.0, a + 0.1, 0.0, 3.0);
            h.voids.push(door(v2(b, a), v2(1.0, 0.0)));
            let mut v = boxp(a - 0.1, b, a + 0.1, b + 4.0, 0.0, 3.0);
            v.voids.push(door(v2(a, b), v2(0.0, 1.0)));
            ps.push(h);
            ps.push(v);
        }
    }
    assert_eq!(ps.len(), 5100);
    let t = std::time::Instant::now();
    let s = section(&ps, &frame((-1.0, 98.0), (1.0, 0.0), true));
    let dt = t.elapsed().as_secs_f64();
    println!(
        "section 5100 walls: {dt:.3} s, cuts {}, lines {}",
        s.cuts.len(),
        s.lines.len()
    );
    assert_eq!(s.cuts.len(), 51);
    assert!(!s.lines.is_empty());
    assert!(dt < 1.0, "{dt}");
}

/// 20k walls: a 4 m grid with doors plus 40 long walls (400 m each) across it, testing candidate
/// selection for long horizontal edges; < 3 s in release.
#[test]
#[ignore]
fn section_budget_20k_walls_long() {
    let mut ps = vec![];
    for i in 0..=70 {
        for j in 0..70 {
            let (a, b) = (i as f64 * 4.0, j as f64 * 4.0);
            ps.push(boxp(b, a - 0.1, b + 4.0, a + 0.1, 0.0, 3.0));
            ps.push(boxp(a - 0.1, b, a + 0.1, b + 4.0, 0.0, 3.0));
            ps.push(boxp(b + 0.5, a - 0.05, b + 3.5, a + 0.05, 3.0, 3.4));
            ps.push(boxp(a - 0.05, b + 0.5, a + 0.05, b + 3.5, 3.0, 3.4));
        }
    }
    for k in 0..40 {
        let y = k as f64 * 7.0 + 1.5;
        ps.push(boxp(-50.0, y, 350.0, y + 0.2, 0.0, 2.0 + k as f64 * 0.05));
    }
    assert!(ps.len() >= 19_600);
    let t = std::time::Instant::now();
    let s = section(&ps, &frame((-60.0, -30.0), (1.0, 0.0), false));
    let dt = t.elapsed().as_secs_f64();
    println!(
        "elevation {} prisms: {dt:.3} s, lines {}",
        ps.len(),
        s.lines.len()
    );
    assert!(!s.lines.is_empty());
    assert!(dt < 3.0, "{dt}");
}

/// A face nearly parallel to the view direction (|e·u| ~ 1e-7) is treated as edge-on: the view equals that of an
/// exactly perpendicular beam.
#[test]
fn near_edge_on_face_is_ignored() {
    let tilted = |dx: f64| Prism {
        rings: vec![vec![
            v2(0.0, 0.0),
            v2(1.0, 0.0),
            v2(1.0 + dx, 10.0),
            v2(dx, 10.0),
        ]],
        bottom: Zfn::flat(0.0),
        top: Zfn::flat(3.0),
        voids: vec![],
    };
    let wall = boxp(-1.0, 12.0, 3.0, 12.5, 0.0, 5.0);
    let f = frame((-5.0, -5.0), (1.0, 0.0), false);
    let exact = section(&[tilted(0.0), wall.clone()], &f);
    for dx in [1e-7, -1e-7, 5e-8] {
        let t = section(&[tilted(dx), wall.clone()], &f);
        assert!(
            (len(&t.lines) - len(&exact.lines)).abs() < 1e-5,
            "{dx}: {} vs {}",
            len(&t.lines),
            len(&exact.lines)
        );
        assert_eq!(t.lines.len(), exact.lines.len(), "{dx}");
    }
}

/// Annular sector r ∈ [r0, r1], angle [a0, a1], approximated by chords at angular step `step` (degrees).
fn arc_wall(r0: f64, r1: f64, a0: f64, a1: f64, step: f64) -> Prism {
    let n = ((a1 - a0) / step).round() as usize;
    let at = |r: f64, k: usize| {
        let a = (a0 + (a1 - a0) * k as f64 / n as f64).to_radians();
        v2(r * a.cos(), r * a.sin())
    };
    let mut ring: Vec<V2> = (0..=n).map(|k| at(r1, k)).collect();
    ring.extend((0..=n).rev().map(|k| at(r0, k)));
    Prism {
        rings: vec![ring],
        bottom: Zfn::flat(0.0),
        top: Zfn::flat(3.0),
        voids: vec![],
    }
}

fn ray(deg: f64) -> V2 {
    let a = deg.to_radians();
    v2(a.cos(), a.sin())
}

#[test]
fn arc_wall_opening_is_cut_in_section() {
    let w = arc_wall(5.0, 5.2, 0.0, 90.0, 1.0);
    let door = vec![vec![ArcVoid {
        c: v2(0.0, 0.0),
        r0: ray(30.0),
        r1: ray(40.0),
        z0: 0.0,
        z1: 2.1,
    }]];
    // Radial section through the opening: 0.2 × 0.9 above the door.
    let through = section_with_arcs(
        &[w.clone()],
        &door,
        &frame((0.0, 0.0), (ray(35.0).x, ray(35.0).y), true),
    );
    assert_eq!(through.cuts.len(), 1);
    assert!((cut_area(&through.cuts[0]) - 0.18).abs() < 1e-9);
    // Missing the opening: full 0.2 × 3 cut; the opening ray does not cut the wall on the other side.
    for deg in [25.0, 45.0, 80.0] {
        let s = section_with_arcs(
            &[w.clone()],
            &door,
            &frame((0.0, 0.0), (ray(deg).x, ray(deg).y), true),
        );
        assert!(
            (s.cuts.iter().map(cut_area).sum::<f64>() - 0.6).abs() < 1e-9,
            "{deg}"
        );
    }
    // Without voids the behaviour is unchanged.
    let plain = section(
        &[w.clone()],
        &frame((0.0, 0.0), (ray(35.0).x, ray(35.0).y), true),
    );
    assert!((cut_area(&plain.cuts[0]) - 0.6).abs() < 1e-9);
    // Elevation from inside the arc: the opening lintel appears at level 2.1.
    let f = frame((-1.0, 1.0), (ray(-45.0).x, ray(-45.0).y), false);
    let head = |o: &SectionOut| {
        o.lines
            .iter()
            .filter(|l| (l.a.y - 2.1).abs() < 1e-9 && (l.b.y - 2.1).abs() < 1e-9)
            .count()
    };
    let (a, b) = (
        section(&[w.clone()], &f),
        section_with_arcs(&[w], &door, &f),
    );
    assert_eq!(head(&a), 0);
    assert!(head(&b) > 0);
}

#[test]
fn collinear_walls_have_no_seam_on_elevation() {
    let ps = [
        boxp(0.0, 0.0, 2.0, 0.2, 0.0, 3.0),
        boxp(2.0, 0.0, 4.0, 0.2, 0.0, 3.0),
        boxp(4.0, 0.0, 6.0, 0.2, 0.0, 2.0),
    ];
    let e = section(&ps, &frame((-1.0, -5.0), (1.0, 0.0), false));
    // Outline 6 + 3 + 4 (top 0..4) + 1 (step) + 2 + 2: the seam at x=2 is gone, at x=4 only the step remains.
    assert!((len(&e.lines) - 18.0).abs() < 1e-9, "{:?}", e.lines);
    assert!(!e
        .lines
        .iter()
        .any(|l| (l.a.x - 2.0).abs() < 1e-9 && (l.b.x - 2.0).abs() < 1e-9));
}
