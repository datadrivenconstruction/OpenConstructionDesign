use super::*;

fn rules() -> Rules {
    let mut r = Rules::new(0.05, 0.4, vec![2.6], (v2(-1.0, -1.0), v2(11.0, 9.0)));
    r.step = 0.4;
    r
}

fn port(x: f64, y: f64, z: f64, d: V3) -> Terminal {
    Terminal {
        at: v3(x, y, z),
        dir: Some(d),
    }
}

/// Every straight between two turns holds `min_leg`; the first and last hold the tangent.
fn legs_ok(p: &[V3], r: &Rules) -> bool {
    let p = clean(p);
    let n = p.len();
    (0..n - 1).all(|i| {
        let l = (p[i + 1] - p[i]).len();
        let inner = i > 0 && i + 2 < n;
        l + 1e-6 >= if inner { r.min_leg } else { r.tangent }
    })
}

fn free(ix: &Index, p: &[V3], c: f64, pass: Pass) -> bool {
    p.windows(2).all(|w| {
        ix.obstacles.iter().all(|o| {
            o.pass != pass
                || w[0].z.max(w[1].z) <= o.z0 - c
                || w[0].z.min(w[1].z) >= o.z1 + c
                || seg_ring_dist(&o.ring, v2(w[0].x, w[0].y), v2(w[1].x, w[1].y)) >= c - 1e-6
        })
    })
}

#[test]
fn two_ports_join_in_the_band_with_room_for_elbows() {
    let r = rules();
    let ix = Index::new(vec![]);
    let src = Source::Terminal(port(0.0, 0.0, 0.5, v3(0.0, 0.0, 1.0)));
    let t = [port(8.0, 6.0, 0.6, v3(0.0, 0.0, 1.0))];
    let tree = route_tree(&ix, &r, &src, &t, &mut RoutedIndex::default());
    assert!(tree.failed.is_empty());
    let b = &tree.branches[0];
    assert_eq!(b.points[0], t[0].at);
    assert_eq!(*b.points.last().unwrap(), v3(0.0, 0.0, 0.5));
    assert!(legs_ok(&b.points, &r), "{:?}", b.points);
    // up, across in two legs at the band, down
    assert!(b.points.iter().filter(|p| (p.z - 2.6).abs() < 1e-6).count() >= 2);
    assert!((poly_len(&b.points) - (2.0 + 2.1 + 14.0)).abs() < 1e-6);
    assert_eq!(tree.fittings(), (3, 0));
}

#[test]
fn a_column_is_never_crossed_and_a_wall_only_once() {
    let mut r = rules();
    r.layers = vec![1.0];
    r.zone = (1.0, 1.0);
    let col = Obstacle::aabb(v3(3.8, -1.0, 0.0), v3(4.2, 3.0, 3.0), Pass::Block, 1);
    let wall = Obstacle::aabb(v3(6.0, -1.0, 0.0), v3(6.2, 9.0, 3.0), Pass::Wall, 7);
    let ix = Index::new(vec![col, wall]);
    let src = Source::Terminal(port(0.0, 1.0, 1.0, v3(1.0, 0.0, 0.0)));
    let t = [port(9.0, 1.0, 1.0, v3(-1.0, 0.0, 0.0))];
    let tree = route_tree(&ix, &r, &src, &t, &mut RoutedIndex::default());
    let p = &tree.branches[0].points;
    assert!(free(&ix, p, r.clearance, Pass::Block), "{p:?}");
    assert!(legs_ok(p, &r));
    // around the column (y beyond 3.05) and straight through the wall
    assert!(p.iter().any(|q| q.y > 3.0));
    let through: usize = p
        .windows(2)
        .filter(|w| (w[0].x - 6.1) * (w[1].x - 6.1) < 0.0)
        .count();
    assert_eq!(through, 1);
}

#[test]
fn slabs_are_crossed_only_vertically() {
    let mut r = rules();
    r.layers = vec![-0.5];
    r.zone = (-0.5, -0.5);
    let slab = Obstacle::aabb(v3(-1.0, -1.0, -0.25), v3(11.0, 9.0, 0.0), Pass::Slab, 3);
    let ix = Index::new(vec![slab]);
    let src = Source::Lines(vec![(v3(9.0, 7.0, -3.0), v3(9.0, 7.0, 0.0))]);
    let t = [port(1.0, 1.0, 0.0, v3(0.0, 0.0, -1.0))];
    let tree = route_tree(&ix, &r, &src, &t, &mut RoutedIndex::default());
    let b = &tree.branches[0];
    assert!(b.tee && b.parent.is_none());
    for w in b.points.windows(2) {
        let inside = w[0].z.min(w[1].z) < 0.0 && w[0].z.max(w[1].z) > -0.25;
        assert!(!inside || (w[0].x == w[1].x && w[0].y == w[1].y), "{w:?}");
    }
    assert_eq!(b.join, v3(9.0, 7.0, -0.5));
}

#[test]
fn a_tree_reaches_every_target_with_tees_clear_of_each_other() {
    let r = rules();
    let ix = Index::new(vec![]);
    let src = Source::Lines(vec![(v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 3.0))]);
    let t: Vec<Terminal> = (0..6)
        .map(|k| port(2.0 + k as f64 * 1.5, 1.0 + (k % 3) as f64 * 2.5, 0.9, v3(0.0, 0.0, 1.0)))
        .collect();
    let mut others = RoutedIndex::default();
    let tree = route_tree(&ix, &r, &src, &t, &mut others);
    assert!(tree.failed.is_empty(), "{:?}", tree.failed);
    assert_eq!(tree.branches.len(), 6);
    for b in &tree.branches {
        assert!(legs_ok(&b.points, &r), "{:?}", b.points);
        // the last leg meets the tree perpendicular
        let n = b.points.len();
        let last = (b.points[n - 1] - b.points[n - 2]).normalized();
        let host: Vec<(V3, V3)> = match b.parent {
            None => vec![(v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 3.0))],
            Some(p) => tree.branches[p].points.windows(2).map(|w| (w[0], w[1])).collect(),
        };
        let seg = host
            .iter()
            .find(|s| seg_seg_dist(b.join, b.join, s.0, s.1) < 1e-6)
            .expect("join lies on its parent");
        assert!(last.dot((seg.1 - seg.0).normalized()).abs() < 1e-9);
    }
    // branches keep the clearance from each other away from their joins
    for (i, a) in tree.branches.iter().enumerate() {
        for b in tree.branches.iter().skip(i + 1) {
            for wa in a.points.windows(2) {
                for wb in b.points.windows(2) {
                    let d = seg_seg_dist(wa[0], wa[1], wb[0], wb[1]);
                    let touch = [a.join, b.join].iter().any(|j| {
                        seg_seg_dist(*j, *j, wa[0], wa[1]) < 1e-6
                            && seg_seg_dist(*j, *j, wb[0], wb[1]) < 1e-6
                    });
                    assert!(touch || d >= 2.0 * r.clearance - 1e-6, "{wa:?} {wb:?}");
                }
            }
        }
    }
}

#[test]
fn fall_rises_away_from_the_source_at_the_exact_slope() {
    let r = rules();
    let ix = Index::new(vec![]);
    let src = Source::Lines(vec![(v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 3.0))]);
    let t = [
        port(6.0, 0.8, 3.2, v3(0.0, 0.0, -1.0)),
        port(4.0, 4.0, 3.2, v3(0.0, 0.0, -1.0)),
    ];
    let mut tree = route_tree(&ix, &r, &src, &t, &mut RoutedIndex::default());
    apply_fall(&mut tree, &r.layers, 2.0);
    for b in &tree.branches {
        assert_eq!(b.points[0], t[b.target].at, "ports keep their place");
        for w in b.points.windows(2) {
            let run = v2(w[1].x - w[0].x, w[1].y - w[0].y).len();
            if run > 1e-6 {
                // towards the source (end of the branch) the run falls by 2 %
                assert!(((w[0].z - w[1].z) / run - 0.02).abs() < 1e-9, "{w:?}");
            }
        }
    }
}

#[test]
fn a_queue_entry_is_stale_only_when_a_cheaper_way_was_found() {
    // pushed at f = g + h = 4 + 3; the state now costs 2: 2 + 3 = 5 < 7, stale
    assert!(stale(7.0, 2.0, 3.0));
    // the entry that set the cost is current
    assert!(!stale(7.0, 4.0, 3.0));
    // rounding noise is not stale
    assert!(!stale(7.0 + 1e-12, 4.0, 3.0));
}

#[test]
fn the_search_skips_stale_entries_and_still_finds_the_best_way() {
    // a wall with a gap forces a detour; the best route is the same with the skip on
    let mut r = rules();
    r.layers = vec![1.0];
    r.zone = (1.0, 1.0);
    let wall = Obstacle::aabb(v3(5.0, -1.0, 0.0), v3(5.2, 6.0, 3.0), Pass::Block, 1);
    let ix = Index::new(vec![wall]);
    let src = Source::Terminal(port(0.0, 1.0, 1.0, v3(1.0, 0.0, 0.0)));
    let t = [port(9.0, 1.0, 1.0, v3(-1.0, 0.0, 0.0))];
    let tree = route_tree(&ix, &r, &src, &t, &mut RoutedIndex::default());
    assert!(tree.failed.is_empty());
    let p = &tree.branches[0].points;
    assert!(free(&ix, p, r.clearance, Pass::Block), "{p:?}");
    // around the end of the wall (y beyond 6.05) there and back: at least 2 * 5.05 + 9
    assert!(poly_len(p) >= 9.0 + 2.0 * 5.0, "{}", poly_len(p));
    let again = route_tree(&ix, &r, &src, &t, &mut RoutedIndex::default());
    assert_eq!(again.branches[0].points, *p, "deterministic");
}

#[test]
fn point_grid_answers_close_points_across_cell_borders() {
    let mut g = PointGrid::new(1.0);
    g.add(v3(0.99, 0.0, 0.0));
    assert!(g.any_within(v3(1.01, 0.0, 0.0), 0.5), "next cell, 0.02 away");
    assert!(!g.any_within(v3(1.6, 0.0, 0.0), 0.5));
    assert!(!g.any_within(v3(0.99, 0.0, 0.9), 0.5));
    g.add(v3(-0.01, -0.01, -0.01));
    assert!(g.any_within(v3(0.01, 0.01, 0.01), 0.5), "across zero");
}

/// Appends one JSON line to the bench ledger when `DDC_BENCH_RECORD=1`
/// (`DDC_BENCH_LEDGER` names the file).
fn record(name: &str, ms: f64, routed: usize, failed: usize) {
    if std::env::var("DDC_BENCH_RECORD").as_deref() != Ok("1") {
        return;
    }
    let path = std::env::var("DDC_BENCH_LEDGER").unwrap_or_else(|_| {
        format!("{}/../../target/bench/ledger.jsonl", env!("CARGO_MANIFEST_DIR"))
    });
    let line = format!(
        "{{\"bench\":\"{name}\",\"ms\":{ms:.2},\"routed\":{routed},\"failed\":{failed},\"build\":\"{}\"}}",
        if cfg!(debug_assertions) { "debug" } else { "release" }
    );
    use std::io::Write;
    if let Some(d) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

fn wide() -> Rules {
    let mut r = Rules::new(0.05, 0.4, vec![2.6], (v2(-2.0, -2.0), v2(30.0, 30.0)));
    r.step = 0.4;
    r.zone = (2.6, 2.6);
    r.max_states = 4_000_000;
    r
}

/// 200 ports on a 1.2 m grid, nothing in the way and every port inside the bounds: all 200 are
/// reachable by construction, so all 200 must be routed and none reported.
#[test]
fn two_hundred_reachable_targets_all_route() {
    let r = wide();
    let ix = Index::new(vec![]);
    let src = Source::Terminal(port(0.0, 0.0, 0.5, v3(0.0, 0.0, 1.0)));
    let t: Vec<Terminal> = (0..200)
        .map(|i| port(1.2 + 1.2 * (i % 14) as f64, 1.2 + 1.2 * (i / 14) as f64, 0.5, v3(0.0, 0.0, 1.0)))
        .collect();
    let t0 = std::time::Instant::now();
    let tree = route_tree(&ix, &r, &src, &t, &mut RoutedIndex::default());
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    eprintln!("200 reachable targets: {ms:.1} ms");
    record("route_tree_200_reachable", ms, tree.branches.len(), tree.failed.len());
    assert!(tree.failed.is_empty(), "failed {:?}", tree.failed);
    assert_eq!(tree.branches.len(), 200);
    for b in &tree.branches {
        assert_eq!(b.points[0], t[b.target].at);
    }
    let again = route_tree(&ix, &r, &src, &t, &mut RoutedIndex::default());
    assert_eq!(again.branches.len(), 200);
    assert!((again.length() - tree.length()).abs() < 1e-9);
}

/// The same 200 ports, but every tenth sits in a closed pocket of blocks that reaches above the
/// band: those 20 cannot leave it, so exactly 20 are reported and 180 routed.
#[test]
fn unreachable_targets_are_counted_exactly() {
    let r = wide();
    let mut obs = vec![];
    let mut sealed = vec![];
    let t: Vec<Terminal> = (0..200)
        .map(|i| {
            let (x, y) = (1.2 + 1.2 * (i % 14) as f64, 1.2 + 1.2 * (i / 14) as f64);
            if i % 10 == 9 {
                sealed.push(i);
                // four blocks around the port, 0.5 m from it, floor to above the band
                let (lo, hi) = (-1.0, 4.0);
                for (a, b) in [
                    (v3(x - 0.55, y - 0.55, lo), v3(x + 0.55, y - 0.45, hi)),
                    (v3(x - 0.55, y + 0.45, lo), v3(x + 0.55, y + 0.55, hi)),
                    (v3(x - 0.55, y - 0.45, lo), v3(x - 0.45, y + 0.45, hi)),
                    (v3(x + 0.45, y - 0.45, lo), v3(x + 0.55, y + 0.45, hi)),
                ] {
                    obs.push(Obstacle::aabb(a, b, Pass::Block, 100 + i as i64));
                }
            }
            port(x, y, 0.5, v3(0.0, 0.0, 1.0))
        })
        .collect();
    let ix = Index::new(obs);
    let src = Source::Terminal(port(0.0, 0.0, 0.5, v3(0.0, 0.0, 1.0)));
    let t0 = std::time::Instant::now();
    let tree = route_tree(&ix, &r, &src, &t, &mut RoutedIndex::default());
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    eprintln!("200 targets, 20 sealed: {ms:.1} ms");
    record("route_tree_200_sealed20", ms, tree.branches.len(), tree.failed.len());
    let mut failed = tree.failed.clone();
    failed.sort_unstable();
    assert_eq!(failed, sealed);
    assert_eq!(tree.branches.len(), 180);
}
