//! Kernel property tests. Case count: `DDC_PROP_CASES` (small by default).

use ddc_geom::layered::{join, layer_footprints, set_threads, Joined, Layer, Strip};
use ddc_geom::predicates::orient2d;
use ddc_geom::profile::signed_area;
use ddc_geom::recognize::{recognize_walls, Options, Seg};
use ddc_geom::{v2, Profile, Solid, Tol, V2};
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

fn cases(default: u32) -> ProptestConfig {
    let n = std::env::var("DDC_PROP_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default);
    ProptestConfig {
        cases: n,
        max_global_rejects: 1_000_000,
        failure_persistence: Some(Box::new(FileFailurePersistence::Off)),
        ..ProptestConfig::default()
    }
}

fn sgn(x: f64) -> i32 {
    if x > 0.0 {
        1
    } else if x < 0.0 {
        -1
    } else {
        0
    }
}

// ---------- orient2d against exact integer evaluation ----------

fn exact_sign(a: (i64, i64), b: (i64, i64), c: (i64, i64)) -> i32 {
    let (ax, ay, bx, by, cx, cy) = (
        a.0 as i128,
        a.1 as i128,
        b.0 as i128,
        b.1 as i128,
        c.0 as i128,
        c.1 as i128,
    );
    let det = (ax - cx) * (by - cy) - (ay - cy) * (bx - cx);
    det.signum() as i32
}

fn pt(p: (i64, i64), shift: i32) -> V2 {
    let s = 2f64.powi(-shift);
    v2(p.0 as f64 * s, p.1 as f64 * s)
}

const R: i64 = 1 << 24;

proptest! {
    #![proptest_config(cases(2000))]

    #[test]
    fn orient2d_matches_exact(
        a in (-R..R, -R..R), b in (-R..R, -R..R), c in (-R..R, -R..R), shift in 0i32..20,
    ) {
        let got = sgn(orient2d(pt(a, shift), pt(b, shift), pt(c, shift)));
        prop_assert_eq!(got, exact_sign(a, b, c));
    }

    /// c = a + k(b-a) + small offset: nearly collinear triples where the fast path is uncertain.
    #[test]
    fn orient2d_near_collinear(
        a in (-1000i64..1000, -1000i64..1000), d in (-1000i64..1000, -1000i64..1000),
        k in -5i64..6, e in (-1i64..2, -1i64..2), shift in 0i32..30,
    ) {
        let b = (a.0 + d.0, a.1 + d.1);
        let c = (a.0 + k * d.0 + e.0, a.1 + k * d.1 + e.1);
        let got = sgn(orient2d(pt(a, shift), pt(b, shift), pt(c, shift)));
        prop_assert_eq!(got, exact_sign(a, b, c));
    }

    #[test]
    fn orient2d_antisymmetric_cyclic(
        a in (-R..R, -R..R), b in (-R..R, -R..R), c in (-R..R, -R..R),
    ) {
        let (pa, pb, pc) = (pt(a, 8), pt(b, 8), pt(c, 8));
        let s = sgn(orient2d(pa, pb, pc));
        prop_assert_eq!(sgn(orient2d(pa, pc, pb)), -s);
        prop_assert_eq!(sgn(orient2d(pb, pc, pa)), s);
        prop_assert_eq!(sgn(orient2d(pc, pa, pb)), s);
    }
}

// ---------- star-shaped polygons ----------

fn star(n: usize, jitter: &[f64], radii: &[f64]) -> Vec<V2> {
    (0..n)
        .map(|i| {
            let ang = (i as f64 + jitter[i] * 0.8) * std::f64::consts::TAU / n as f64;
            v2(radii[i] * ang.cos(), radii[i] * ang.sin())
        })
        .collect()
}

fn star_strategy() -> impl Strategy<Value = Vec<V2>> {
    (3usize..40).prop_flat_map(|n| {
        (
            Just(n),
            prop::collection::vec(0.0f64..1.0, n),
            prop::collection::vec(1.0f64..10.0, n),
        )
            .prop_map(|(n, j, r)| star(n, &j, &r))
    })
}

proptest! {
    #![proptest_config(cases(200))]

    #[test]
    fn star_extrude_mesh_consistent(pts in star_strategy(), h in 0.1f64..20.0) {
        let tol = Tol::default();
        let p = Profile::new(pts.clone(), vec![], tol)
            .map_err(|e| TestCaseError::fail(format!("profile: {e:?}")))?;
        let area = signed_area(&pts).abs();
        prop_assert!((p.area() - area).abs() <= 1e-9 * area);
        let s = Solid::extrude(&p, h, tol);
        prop_assert_eq!(s.validate(0), Ok(()));
        let m = s.triangulate().map_err(|e| TestCaseError::fail(format!("mesh: {e:?}")))?;
        prop_assert!(m.is_watertight());
        let (vs, vm) = (s.volume(), m.volume());
        prop_assert!((vs - area * h).abs() <= 1e-9 * area * h, "solid {} vs {}", vs, area * h);
        prop_assert!((vm - area * h).abs() <= 1e-9 * area * h, "mesh {} vs {}", vm, area * h);
    }
}

// ---------- wall networks ----------

#[derive(Clone, Debug)]
struct Net {
    nodes: Vec<V2>,
    edges: Vec<(usize, usize)>,
    layers: Vec<(f64, u8)>,
}

impl Net {
    fn total(&self) -> f64 {
        self.layers.iter().map(|l| l.0).sum()
    }
    fn degree(&self) -> Vec<usize> {
        let mut d = vec![0; self.nodes.len()];
        for &(a, b) in &self.edges {
            d[a] += 1;
            d[b] += 1;
        }
        d
    }
    /// Has a node where two walls almost continue each other (bend < 20°); regression F1.
    fn strips(&self) -> Vec<Strip> {
        let t = self.total();
        self.edges
            .iter()
            .map(|&(a, b)| Strip {
                a: self.nodes[a],
                b: self.nodes[b],
                layers: self
                    .layers
                    .iter()
                    .map(|&(w, p)| Layer::with_priority(w, p))
                    .collect(),
                offset0: -t / 2.0,
                base: 0.0,
                height: 3.0,
            })
            .collect()
    }
}

/// An nx×ny grid with spacing 4, a random non-empty edge subset, nodes displaced by `jitter`.
fn net_strategy(jitter: f64, same_priority: bool) -> impl Strategy<Value = Net> {
    (2usize..5, 2usize..5).prop_flat_map(move |(nx, ny)| {
        let nn = nx * ny;
        let ne = (nx - 1) * ny + nx * (ny - 1);
        (
            prop::collection::vec((-jitter..=jitter, -jitter..=jitter), nn),
            prop::collection::vec(any::<bool>(), ne),
            prop::collection::vec((0.08f64..0.2, 1u8..4), 1..4),
            prop::collection::vec(any::<bool>(), ne),
        )
            .prop_map(move |(jit, keep, mut layers, flip)| {
                if same_priority {
                    for l in layers.iter_mut() {
                        l.1 = 1;
                    }
                }
                let nodes = (0..nn)
                    .map(|k| {
                        let (i, j) = (k % nx, k / nx);
                        v2(i as f64 * 4.0 + jit[k].0, j as f64 * 4.0 + jit[k].1)
                    })
                    .collect();
                let mut all = vec![];
                for j in 0..ny {
                    for i in 0..nx {
                        if i + 1 < nx {
                            all.push((j * nx + i, j * nx + i + 1));
                        }
                        if j + 1 < ny {
                            all.push((j * nx + i, (j + 1) * nx + i));
                        }
                    }
                }
                let mut edges: Vec<_> = all
                    .iter()
                    .zip(&keep)
                    .zip(&flip)
                    .filter(|((_, k), _)| **k)
                    .map(|((e, _), f)| if *f { (e.1, e.0) } else { *e })
                    .collect();
                if edges.is_empty() {
                    edges.push(all[0]);
                }
                Net {
                    nodes,
                    edges,
                    layers,
                }
            })
    })
}

/// Plan area: layers of all walls plus node fillers.
fn plan_area(strips: &[Strip], j: &Joined, tol: Tol) -> Result<(f64, Vec<Vec<V2>>), String> {
    let mut area = 0.0;
    let mut rings = vec![];
    for (i, s) in strips.iter().enumerate() {
        let f = layer_footprints(s, &j.pieces[i], i, tol).map_err(|e| format!("{e:?}"))?;
        for p in f.iter().flatten() {
            area += p.area();
            rings.push(p.outer.clone());
        }
    }
    for f in &j.fillers {
        area += signed_area(f).abs();
        rings.push(f.clone());
    }
    Ok((area, rings))
}

fn rect_ring(a: V2, b: V2, half: f64, ext_a: f64, ext_b: f64) -> Vec<V2> {
    let d = (b - a) * (1.0 / (b - a).len());
    let n = v2(-d.y, d.x);
    let (a, b) = (a - d * ext_a, b + d * ext_b);
    vec![a + n * half, b + n * half, b - n * half, a - n * half]
}

/// Exact area of the union of axis-aligned rectangles (coordinate compression).
fn union_area_axis(rects: &[(f64, f64, f64, f64)]) -> f64 {
    let mut xs: Vec<f64> = rects.iter().flat_map(|r| [r.0, r.1]).collect();
    let mut ys: Vec<f64> = rects.iter().flat_map(|r| [r.2, r.3]).collect();
    xs.sort_by(f64::total_cmp);
    ys.sort_by(f64::total_cmp);
    let mut total = 0.0;
    for xw in xs.windows(2) {
        for yw in ys.windows(2) {
            let (cx, cy) = ((xw[0] + xw[1]) / 2.0, (yw[0] + yw[1]) / 2.0);
            if rects
                .iter()
                .any(|r| r.0 < cx && cx < r.1 && r.2 < cy && cy < r.3)
            {
                total += (xw[1] - xw[0]) * (yw[1] - yw[0]);
            }
        }
    }
    total
}

fn inside(poly: &[V2], p: V2) -> bool {
    let mut c = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
            c = !c;
        }
    }
    c
}

const CELL: f64 = 0.05;
const GRID: usize = 560;
const ORG: f64 = 0.0;

/// Number of raster cells whose centers are covered by the union of polygons.
fn raster_count(polys: &[Vec<V2>]) -> usize {
    let mut g = vec![false; GRID * GRID];
    for poly in polys {
        let lo = |f: fn(&V2) -> f64| poly.iter().map(f).fold(f64::MAX, f64::min);
        let hi = |f: fn(&V2) -> f64| poly.iter().map(f).fold(f64::MIN, f64::max);
        let idx = |v: f64, up: bool| {
            let k = ((v - ORG) / CELL).floor() as i64 + i64::from(up);
            k.clamp(0, GRID as i64 - 1) as usize
        };
        let (i0, i1) = (idx(lo(|p| p.x), false), idx(hi(|p| p.x), true));
        let (j0, j1) = (idx(lo(|p| p.y), false), idx(hi(|p| p.y), true));
        for j in j0..=j1 {
            for i in i0..=i1 {
                let c = v2(ORG + (i as f64 + 0.5) * CELL, ORG + (j as f64 + 0.5) * CELL);
                if !g[j * GRID + i] && inside(poly, c) {
                    g[j * GRID + i] = true;
                }
            }
        }
    }
    g.iter().filter(|&&b| b).count()
}

fn fail<E: std::fmt::Debug>(e: E) -> TestCaseError {
    TestCaseError::fail(format!("{e:?}"))
}

proptest! {
    #![proptest_config(cases(60))]

    /// Axis-aligned grid, equal priority: the plan area equals the exact union.
    #[test]
    fn grid_plan_area_is_exact_union(net in net_strategy(0.0, true)) {
        let tol = Tol::default();
        let strips = net.strips();
        let j = join(&strips, tol).map_err(fail)?;
        let (area, _) = plan_area(&strips, &j, tol).map_err(TestCaseError::fail)?;
        let t = net.total();
        let deg = net.degree();
        let rects: Vec<_> = net.edges.iter().map(|&(a, b)| {
            let (pa, pb) = (net.nodes[a], net.nodes[b]);
            let ext = |n: usize| if deg[n] >= 2 { t / 2.0 } else { 0.0 };
            let (x0, x1) = (pa.x.min(pb.x), pa.x.max(pb.x));
            let (y0, y1) = (pa.y.min(pb.y), pa.y.max(pb.y));
            let (lo, hi) = if (pa.x, pa.y) <= (pb.x, pb.y) { (a, b) } else { (b, a) };
            if (pa.y - pb.y).abs() < 1e-12 {
                (x0 - ext(lo), x1 + ext(hi), y0 - t / 2.0, y0 + t / 2.0)
            } else {
                (x0 - t / 2.0, x0 + t / 2.0, y0 - ext(lo), y1 + ext(hi))
            }
        }).collect();
        let want = union_area_axis(&rects);
        prop_assert!((area - want).abs() <= 1e-9 * want, "plan {} vs union {}", area, want);
    }

    /// Different layer priorities: the total plan is the same (layers share one strip).
    #[test]
    fn grid_priorities_keep_total_area(net in net_strategy(0.0, false)) {
        let tol = Tol::default();
        let mut same = net.clone();
        for l in same.layers.iter_mut() { l.1 = 1; }
        let (s1, s2) = (net.strips(), same.strips());
        let j1 = join(&s1, tol).map_err(fail)?;
        let j2 = join(&s2, tol).map_err(fail)?;
        let a1 = plan_area(&s1, &j1, tol).map_err(TestCaseError::fail)?.0;
        let a2 = plan_area(&s2, &j2, tol).map_err(TestCaseError::fail)?.0;
        prop_assert!((a1 - a2).abs() <= 1e-9 * a2, "priorities {} vs equal {}", a1, a2);
    }

    /// Displaced nodes: join succeeds, layers do not overlap, the plan lies between the lower and upper bounds.
    #[test]
    fn jittered_network_bounds(net in net_strategy(0.5, true)) {
        let tol = Tol::default();
        let strips = net.strips();
        let j = join(&strips, tol).map_err(fail)?;
        let (area, rings) = plan_area(&strips, &j, tol).map_err(TestCaseError::fail)?;
        let (t, deg) = (net.total(), net.degree());
        let shift = |ps: Vec<V2>| ps.into_iter().map(|p| p + v2(2.0, 2.0)).collect::<Vec<_>>();
        let rings: Vec<_> = rings.into_iter().map(shift).collect();
        let rect = |ext: f64| -> Vec<Vec<V2>> {
            net.edges.iter().map(|&(a, b)| {
                let e = |n: usize| if deg[n] >= 2 { ext } else { 0.0 };
                shift(rect_ring(net.nodes[a], net.nodes[b], t / 2.0, e(a), e(b)))
            }).collect()
        };
        let (out, lo, hi) = (raster_count(&rings), raster_count(&rect(0.0)), raster_count(&rect(1.2 * t)));
        let cells = area / (CELL * CELL);
        let slack = 0.03 * cells + 100.0;
        prop_assert!(out as f64 + slack >= lo as f64, "covers less than bare strips: {} < {}", out, lo);
        prop_assert!(out as f64 <= hi as f64 + slack, "exceeds extended strips: {} > {}", out, hi);
        prop_assert!((cells - out as f64).abs() <= slack, "overlap/gap: sum {} vs union {} (lo {} hi {} t {} rings {})", cells, out, lo, hi, t, rings.len());
    }
}

// ---------- determinism ----------

fn grid_segs(n: usize, t: f64) -> Vec<Seg> {
    let mut v = vec![];
    for j in 0..=n {
        for i in 0..n {
            let (x, y) = (i as f64 * 4.0, j as f64 * 4.0);
            for s in [-t / 2.0, t / 2.0] {
                v.push(Seg {
                    a: v2(x, y + s),
                    b: v2(x + 4.0, y + s),
                });
                v.push(Seg {
                    a: v2(y + s, x),
                    b: v2(y + s, x + 4.0),
                });
            }
        }
    }
    v
}

proptest! {
    #![proptest_config(cases(20))]

    #[test]
    fn deterministic_debug_output(net in net_strategy(0.5, false), pts in star_strategy()) {
        let tol = Tol::default();
        let run = || -> String {
            let strips = net.strips();
            let j = join(&strips, tol);
            let mut out = format!("{j:?}");
            if let Ok(j) = &j {
                out += &format!("{:?}", plan_area(&strips, j, tol));
            }
            let p = Profile::new(pts.clone(), vec![], tol);
            out += &format!("{p:?}");
            if let Ok(p) = p {
                let s = Solid::extrude(&p, 2.0, tol);
                out += &format!("{:?}{:?}", s, s.triangulate());
            }
            let r = recognize_walls(&grid_segs(3, net.total().max(0.1)), &Options::default());
            out + &format!("{r:?}")
        };
        prop_assert_eq!(run(), run());
    }
}

/// Regression F1: a nearly straight join of two walls (bend ~4°) produced a self-intersecting layer footprint.
/// `layer_footprints` used to return Degenerate/SelfIntersection; now the join is cut by a single line.
#[test]
fn finding_f1_shallow_bend() {
    let net = Net {
        nodes: vec![
            v2(0.0, -0.1272028350316853),
            v2(4.0, 0.0),
            v2(8.0, 0.1014559240693382),
        ],
        edges: vec![(1, 0), (1, 2)],
        layers: vec![(0.08, 1), (0.18517125698891054, 1)],
    };
    let (tol, strips) = (Tol::default(), net.strips());
    let j = join(&strips, tol).unwrap();
    plan_area(&strips, &j, tol).unwrap();
}

/// The join/plan/recognize result is bit-for-bit identical with 1 and 8 threads (network above the threading threshold).
#[test]
fn thread_count_determinism() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 11) as f64 / (1u64 << 53) as f64) - 0.5
    };
    let (nx, ny) = (30, 30);
    let nodes: Vec<V2> = (0..nx * ny)
        .map(|k| {
            v2(
                (k % nx) as f64 * 4.0 + rnd() * 0.6,
                (k / nx) as f64 * 4.0 + rnd() * 0.6,
            )
        })
        .collect();
    let mut edges = vec![];
    for k in 0..nx * ny {
        if k % nx + 1 < nx && rnd() > -0.35 {
            edges.push((k, k + 1));
        }
        if k / nx + 1 < ny && rnd() > -0.35 {
            edges.push((k, k + nx));
        }
    }
    let net = Net {
        nodes,
        edges,
        layers: vec![(0.1, 3), (0.2, 1), (0.05, 5)],
    };
    let tol = Tol::default();
    let segs = grid_segs(20, 0.2);
    let run = |n: usize| -> String {
        set_threads(n);
        let strips = net.strips();
        let j = join(&strips, tol);
        let mut out = format!("{j:?}");
        if let Ok(j) = &j {
            out += &format!("{:?}", plan_area(&strips, j, tol));
        }
        out + &format!("{:?}", recognize_walls(&segs, &Options::default()))
    };
    let (one, many) = (run(1), run(8));
    set_threads(0);
    assert!(one.len() > 10_000);
    assert_eq!(one, many);
}
