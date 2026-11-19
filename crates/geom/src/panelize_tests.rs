use super::*;
use crate::math::v3;

const K1: [f64; 4] = [0.0, 0.0, 1.0, 1.0];
const K2: [f64; 6] = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];

/// Flat 10 x 6 m rectangle.
fn plane() -> Surface {
    let ctrl = vec![v3(0.0, 0.0, 0.0), v3(0.0, 6.0, 0.0), v3(10.0, 0.0, 0.0), v3(10.0, 6.0, 0.0)];
    Surface::new(1, 1, K1.to_vec(), K1.to_vec(), 2, 2, ctrl, vec![1.0; 4]).unwrap()
}

/// Barrel vault: straight along x (0..10), parabola z = -y^2 across y in -1..1.
fn vault() -> Surface {
    let mut ctrl = vec![];
    for x in [0.0, 10.0] {
        for (y, z) in [(-1.0, -1.0), (0.0, 1.0), (1.0, -1.0)] {
            ctrl.push(v3(x, y, z));
        }
    }
    Surface::new(1, 2, K1.to_vec(), K2.to_vec(), 2, 3, ctrl, vec![1.0; 6]).unwrap()
}

/// Facade band rising along v: z = 8v - 2v^2 (0..6 m), bowed in y.
fn facade() -> Surface {
    let mut ctrl = vec![];
    for x in [0.0, 10.0] {
        for (y, z) in [(0.0, 0.0), (1.0, 4.0), (0.0, 6.0)] {
            ctrl.push(v3(x, y, z));
        }
    }
    Surface::new(1, 2, K1.to_vec(), K2.to_vec(), 2, 3, ctrl, vec![1.0; 6]).unwrap()
}

/// Hypar z = x*y / 10 on [0, 10]^2 (bilinear), quads off the rulings are warped.
fn hypar() -> Surface {
    let mut ctrl = vec![];
    for i in 0..3 {
        for j in 0..3 {
            let (x, y) = (5.0 * i as f64, 5.0 * j as f64);
            let z = 0.1 * x * y + 0.02 * (x - 5.0) * (x - 5.0) - 0.03 * (y - 5.0) * (y - 5.0);
            ctrl.push(v3(x, y, z));
        }
    }
    Surface::new(2, 2, K2.to_vec(), K2.to_vec(), 3, 3, ctrl, vec![1.0; 9]).unwrap()
}

fn total_area(p: &Panels) -> f64 {
    p.panels.iter().map(|x| x.area).sum()
}

#[test]
fn plane_quads_are_flat_and_one_type() {
    let s = plane();
    let p = panelize(&s, Spacing::Params(5), &Rows::Spacing(Spacing::Params(3)), Pattern::Quad, PanelLimits::default())
        .unwrap();
    assert_eq!((p.cols, p.rows, p.nodes.len(), p.panels.len()), (5, 3, 24, 15));
    assert!(p.panels.iter().all(|x| x.kind == PanelKind::Flat && x.planarity < 1e-12 && x.warp < 1e-12));
    assert_eq!(p.clusters, vec![Cluster { count: 15, first: 0 }]);
    assert!((total_area(&p) - 60.0).abs() < 1e-9);
    assert!(p.off_surface(&s) < 1e-12);

    let w = panelize(&s, Spacing::Width(3.0), &Rows::Spacing(Spacing::Width(2.0)), Pattern::Quad, PanelLimits::default())
        .unwrap();
    assert_eq!((w.cols, w.rows), (4, 3));
}

#[test]
fn triangles_and_diamonds_tile_the_surface() {
    let s = plane();
    let rows = Rows::Spacing(Spacing::Params(4));
    let t = panelize(&s, Spacing::Params(5), &rows, Pattern::Triangle, PanelLimits::default()).unwrap();
    assert_eq!(t.panels.len(), 40);
    assert!(t.panels.iter().all(|x| x.nodes.len() == 3));
    assert!((total_area(&t) - 60.0).abs() < 1e-9);
    // two triangle shapes (mirror halves of a cell) are one type: same sorted edges
    assert_eq!(t.clusters.len(), 1);

    let d = panelize(&s, Spacing::Params(5), &rows, Pattern::Diamond, PanelLimits::default()).unwrap();
    let odd = (0..=4).flat_map(|j| (0..=5).map(move |i| (i + j) % 2)).filter(|&k| k == 1).count();
    assert_eq!(d.panels.len(), odd);
    assert!((total_area(&d) - 60.0).abs() < 1e-9, "{}", total_area(&d));
    // an interior diamond holds four half-cells (cells are 2 x 1.5 m)
    let interior = d.panels.iter().filter(|x| x.nodes.len() == 4 && (x.area - 6.0).abs() < 1e-9).count();
    assert!(interior > 0);
}

#[test]
fn equal_spacing_gives_equal_arcs() {
    let s = vault();
    let p = panelize(&s, Spacing::Params(5), &Rows::Spacing(Spacing::Equal(4)), Pattern::Quad, PanelLimits::default())
        .unwrap();
    let w = p.cols + 1;
    let um = 0.5;
    let arcs: Vec<f64> = (0..p.rows)
        .map(|j| {
            let (a, b) = (p.uv[j * w][1], p.uv[(j + 1) * w][1]);
            let f = |v: f64| s.point(um, v);
            let (_, cum) = arc_table(&f, a, b);
            cum[cum.len() - 1]
        })
        .collect();
    let (lo, hi) = arcs.iter().fold((f64::MAX, 0.0f64), |(l, h), &x| (l.min(x), h.max(x)));
    assert!((hi - lo) / hi < 1e-4, "{arcs:?}");
    // parameter steps would not: the crown arcs are shorter than the flanks
    let q = panelize(&s, Spacing::Params(5), &Rows::Spacing(Spacing::Params(4)), Pattern::Quad, PanelLimits::default())
        .unwrap();
    let d = |k: usize| (q.nodes[(k + 1) * w] - q.nodes[k * w]).len();
    assert!(d(0) > 1.3 * d(1), "{} {}", d(0), d(1));
    // an extrusion is planar everywhere; symmetric rows pair up into two panel types
    assert!(q.panels.iter().all(|x| x.kind == PanelKind::Flat));
    let mut counts: Vec<usize> = q.clusters.iter().map(|c| c.count).collect();
    counts.sort();
    assert_eq!(counts, vec![10, 10]);
}

#[test]
fn rows_at_heights() {
    let s = facade();
    let ls = vec![0.0, 1.0, 2.5, 4.2, 6.0];
    let p = panelize(&s, Spacing::Params(4), &Rows::Levels(ls.clone()), Pattern::Quad, PanelLimits::default()).unwrap();
    assert_eq!((p.cols, p.rows), (4, 4));
    for (j, l) in ls.iter().enumerate() {
        for i in 0..=4 {
            let z = p.nodes[j * 5 + i].z;
            assert!((z - l).abs() < 1e-9, "{i} {j}: {z}");
        }
    }
    let e = panelize(&s, Spacing::Params(4), &Rows::Levels(vec![1.0, 7.0]), Pattern::Quad, PanelLimits::default());
    assert_eq!(e.unwrap_err(), PanelError::Level { level: 7.0, col: 0 });
    let e = panelize(&s, Spacing::Params(4), &Rows::Levels(vec![2.0, 1.0]), Pattern::Quad, PanelLimits::default());
    assert!(matches!(e, Err(PanelError::Input(_))));
}

#[test]
fn counts_are_bounded() {
    let s = plane();
    let r = Rows::Spacing(Spacing::Params(2));
    let lim = PanelLimits::default();
    assert_eq!(panelize(&s, Spacing::Params(0), &r, Pattern::Quad, lim).unwrap_err(), PanelError::Count('u'));
    assert_eq!(panelize(&s, Spacing::Params(MAX_DIV + 1), &r, Pattern::Quad, lim).unwrap_err(), PanelError::Count('u'));
    let bad = Rows::Spacing(Spacing::Width(0.0));
    assert!(matches!(panelize(&s, Spacing::Params(2), &bad, Pattern::Quad, lim), Err(PanelError::Input(_))));
    assert!(PanelError::Count('v').to_string().contains("1 to 500"));
}

#[test]
fn warped_quads_are_classified_and_planarized() {
    let s = hypar();
    let lim = PanelLimits { flat: 1e-3, cold: 0.05, cluster: 2e-3 };
    let mut p = panelize(&s, Spacing::Params(6), &Rows::Spacing(Spacing::Params(6)), Pattern::Quad, lim).unwrap();
    let before = p.max_planarity();
    assert!(before > lim.flat, "{before}");
    assert!(p.panels.iter().any(|x| x.kind != PanelKind::Flat));
    for x in &p.panels {
        let want = if x.planarity <= lim.flat {
            PanelKind::Flat
        } else if x.planarity <= lim.cold {
            PanelKind::ColdBent
        } else {
            PanelKind::HotBent
        };
        assert_eq!(x.kind, want);
        assert!(x.warp > 0.0 || x.planarity < 1e-12);
    }
    // free nodes: alternating projection drives the quads flat
    let mut free = p.clone();
    let (b, a) = free.planarize(&s, 300, 0.0);
    assert!((b - before).abs() < 1e-15);
    assert!(a < 0.2 * b, "{b} -> {a}");
    // pulled to the surface: still better, and the nodes stay close to it
    let (b2, a2) = p.planarize(&s, 100, 0.2);
    assert!(a2 < b2, "{b2} -> {a2}");
    assert!(p.off_surface(&s) < before, "{}", p.off_surface(&s));
    // boundary nodes keep their boundary parameter
    let w = p.cols + 1;
    for j in 0..=p.rows {
        assert_eq!(p.uv[j * w][0], 0.0);
        assert_eq!(p.uv[j * w + p.cols][0], 1.0);
    }
}

#[test]
fn hypar_panels_are_exactly_ruled() {
    // bilinear hypar: grid edges are its rulings, so every panel edge lies on the surface
    let ctrl = vec![v3(0.0, 0.0, 0.0), v3(0.0, 10.0, 0.0), v3(10.0, 0.0, 0.0), v3(10.0, 10.0, 5.0)];
    let s = Surface::new(1, 1, K1.to_vec(), K1.to_vec(), 2, 2, ctrl, vec![1.0; 4]).unwrap();
    let p = panelize(&s, Spacing::Equal(5), &Rows::Spacing(Spacing::Equal(4)), Pattern::Quad, PanelLimits::default())
        .unwrap();
    for x in &p.panels {
        for k in 0..4 {
            let (a, b) = (x.nodes[k], x.nodes[(k + 1) % 4]);
            let mid = (p.nodes[a] + p.nodes[b]) * 0.5;
            let t = [(p.uv[a][0] + p.uv[b][0]) / 2.0, (p.uv[a][1] + p.uv[b][1]) / 2.0];
            assert!((mid - s.point(t[0], t[1])).len() < 1e-12);
        }
        assert!(x.warp > 1e-3 && x.planarity > 0.0);
    }
}

#[test]
fn cylinder_facade_is_one_panel_type() {
    // exact quarter cylinder, radius 5 m, axis along x
    let w = std::f64::consts::FRAC_1_SQRT_2;
    let mut ctrl = vec![];
    for x in [0.0, 12.0] {
        for (y, z) in [(5.0, 0.0), (5.0, 5.0), (0.0, 5.0)] {
            ctrl.push(v3(x, y, z));
        }
    }
    let s = Surface::new(1, 2, K1.to_vec(), K2.to_vec(), 2, 3, ctrl, vec![1.0, w, 1.0, 1.0, w, 1.0]).unwrap();
    let p = panelize(&s, Spacing::Params(4), &Rows::Spacing(Spacing::Equal(6)), Pattern::Quad, PanelLimits::default())
        .unwrap();
    assert!(p.nodes.iter().all(|n| ((n.y * n.y + n.z * n.z).sqrt() - 5.0).abs() < 1e-12));
    assert!(p.panels.iter().all(|x| x.kind == PanelKind::Flat));
    assert_eq!(p.clusters, vec![Cluster { count: 24, first: 0 }]);
}
