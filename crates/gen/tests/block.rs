// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A block of buildings on plots: shared site rules, parallel build, the block check.

use ddc_gen::project::block::{self, BlockInput, Edge};
use ddc_gen::project::check_project;
use serde_json::{json, Value as J};
use std::time::Instant;

/// `cols` x 2 plots of `w` x `d` m between four streets 12 m wide: the south row on the south
/// street, the north row on the north one, the end plots on a corner.
fn grid(cols: usize, w: f64, d: f64) -> (Vec<J>, Vec<J>) {
    let mut plots = vec![];
    for row in 0..2 {
        for c in 0..cols {
            let (x, y) = (c as f64 * w, row as f64 * d);
            plots.push(json!({"id": format!("{}{}", ["S", "N"][row], c + 1),
                "ring": [[x, y], [x + w, y], [x + w, y + d], [x, y + d]]}));
        }
    }
    let (x1, y1) = (cols as f64 * w, 2.0 * d);
    let s = 12.0;
    let rect = |x0: f64, y0: f64, x1: f64, y1: f64| json!([[x0, y0], [x1, y0], [x1, y1], [x0, y1]]);
    let streets = vec![
        rect(-s, -s, x1 + s, 0.0),
        rect(-s, y1, x1 + s, y1 + s),
        rect(-s, 0.0, 0.0, y1),
        rect(x1, 0.0, x1 + s, y1),
    ];
    (plots, streets)
}

fn input(args: J) -> BlockInput {
    block::read(&args).unwrap_or_else(|e| panic!("{e}"))
}

fn assert_block(b: &block::Block, what: &str) {
    assert!(b.report.failures.is_empty(), "{what}: {:?}", b.report.failures);
    for (p, built) in b.plan.plots.iter().zip(&b.built) {
        // the same gates as a single generated project
        let g = check_project(&built.spec, &built.project);
        assert!(g.passed(), "{what} plot {}: {:?}", p.id, g.failures);
        assert_eq!(built.project.storeys.len() as u32, p.floors, "{what} plot {}", p.id);
    }
    let rings: Vec<&Vec<(f64, f64)>> = b.built.iter().map(|x| &x.project.storeys[0].layout.slab).collect();
    for i in 0..rings.len() {
        for j in i + 1..rings.len() {
            let gap = block::ring_gap(rings[i], rings[j]);
            let tall = b.report.facts[i].height.max(b.report.facts[j].height);
            let need = b.plan.spacing.max(b.plan.sun_ratio * tall);
            assert!(gap > 0.0, "{what}: plots {} and {} overlap", b.plan.plots[i].id, b.plan.plots[j].id);
            assert!(gap + 1e-3 >= need, "{what}: plots {} and {} {gap:.2} m < {need:.2}", b.plan.plots[i].id, b.plan.plots[j].id);
        }
    }
}

#[test]
fn a_twelve_plot_block_builds_without_overlap_and_keeps_its_spacing_rules() {
    let (plots, streets) = grid(6, 30.0, 36.0);
    let mut args = json!({"plots": plots, "streets": streets, "rules": {"block.storeys_max": 4}});
    // one tower: its neighbours step back by the sun rule, not by the spacing
    args["plots"][8]["overrides"] = json!({"floors": 10});
    let t = Instant::now();
    let b = block::generate(&input(args), block::workers()).unwrap_or_else(|e| panic!("{e}"));
    let total = t.elapsed().as_secs_f64();
    eprintln!(
        "block 12 plots: {total:.2} s (plan {:.0} ms, build {:.0} ms on {} workers, check {:.0} ms); min gap {:.2} m",
        b.ms[0], b.ms[1], b.workers, b.ms[2], b.report.min_gap
    );
    assert_eq!(b.built.len(), 12);
    assert_block(&b, "12 plots");
    // corners take the corner program, the rest the street program: from the rules
    for p in &b.plan.plots {
        let corner = ["S1", "S6", "N1", "N6"].contains(&p.id.as_str());
        let want = if corner { "office" } else { "apartment_section" };
        assert_eq!(p.program, want, "{}: {:?}", p.id, p.why);
        assert!(p.why.iter().any(|w| w.contains("block.")), "{}: {:?}", p.id, p.why);
    }
    // the tower's height sets the set-back of its neighbours' shared edges
    let tower = &b.plan.plots[8];
    assert_eq!(tower.floors, 10);
    let half_sun = b.plan.sun_ratio * tower.height * 0.5;
    assert!(half_sun > b.plan.spacing * 0.5);
    for (k, p) in b.plan.plots.iter().enumerate().filter(|(k, _)| *k != 8) {
        for (e, kind) in p.edges.iter().enumerate() {
            if matches!(kind, Edge::Shared(js) if js.contains(&8)) {
                assert!((p.setbacks[e] - half_sun).abs() < 1e-6, "plot {k} edge {e}: {} vs {half_sun}", p.setbacks[e]);
            }
        }
    }
    let heights: Vec<u32> = b.plan.plots.iter().map(|p| p.floors).collect();
    assert!(heights.iter().any(|f| *f != heights[0]), "heights from rules differ: {heights:?}");
}

#[test]
fn fifty_small_buildings_generate_in_parallel_within_a_time_budget() {
    let (plots, streets) = grid(25, 26.0, 30.0);
    let args = json!({"plots": plots, "streets": streets,
        "rules": {"block.storeys_min": 1, "block.storeys_max": 2, "block.far_target": 0.8}});
    let inp = input(args);
    let t = Instant::now();
    let b = block::generate(&inp, block::workers()).unwrap_or_else(|e| panic!("{e}"));
    let secs = t.elapsed().as_secs_f64();
    let elements: usize = b
        .built
        .iter()
        .flat_map(|x| x.project.storeys.iter())
        .map(|s| s.layout.walls.len() + s.layout.openings.len() + s.layout.rooms.len())
        .sum();
    eprintln!(
        "block 50 small buildings: {secs:.2} s on {} workers (plan {:.0} ms, build {:.0} ms, check {:.0} ms), {elements} walls+openings+rooms",
        b.workers, b.ms[0], b.ms[1], b.ms[2]
    );
    assert_eq!(b.built.len(), 50);
    assert_block(&b, "50 plots");
    // generous: an unoptimised test build on one slow core
    assert!(secs < 600.0, "{secs:.1} s");
}

#[test]
fn the_block_is_the_same_on_one_worker_and_on_many() {
    let (plots, streets) = grid(2, 30.0, 36.0);
    let inp = input(json!({"plots": plots, "streets": streets, "rules": {"block.storeys_max": 3}}));
    let one = block::generate(&inp, 1).unwrap();
    let many = block::generate(&inp, 4).unwrap();
    assert_eq!(one.plan, many.plan);
    for (a, b) in one.built.iter().zip(&many.built) {
        assert_eq!(a.project, b.project);
    }
    assert_eq!(one.report, many.report);
}

#[test]
fn a_plot_out_of_reach_of_every_street_is_named_by_the_fire_rule() {
    let plots = json!([
        {"id": "A", "ring": [[0, 0], [30, 0], [30, 30], [0, 30]]},
        {"id": "B", "ring": [[0, 30], [30, 30], [30, 70], [0, 70]]},
    ]);
    let streets = json!([[[-10, -12], [40, -12], [40, 0], [-10, 0]]]);
    let b = block::generate(&input(json!({"plots": plots, "streets": streets})), 2).unwrap();
    assert!(b.report.failures.iter().any(|f| f.contains("plot B") && f.contains("block.fire_access")), "{:?}", b.report.failures);
    assert!(!b.report.failures.iter().any(|f| f.contains("plot A:")), "{:?}", b.report.failures);
    assert!(b.plan.plots[1].why.iter().any(|w| w.contains("block.fire_access")));
}

#[test]
fn bad_requests_name_the_field() {
    for (args, want) in [
        (json!({}), "plots"),
        (json!({"plots": [{"ring": [[0, 0], [1, 1]]}]}), "plots[0].ring"),
        (json!({"plots": [{"id": "A", "ring": [[0, 0], [9, 0], [9, 9]]}, {"id": "A", "ring": [[20, 0], [29, 0], [29, 9]]}]}), "twice"),
        (json!({"plots": [{"ring": [[0, 0], [9, 0], [9, 9]]}], "streets": 3}), "streets"),
    ] {
        let e = block::read(&args).unwrap_err();
        assert!(e.contains(want), "{args}: {e}");
    }
    let (plots, streets) = grid(1, 30.0, 36.0);
    let e = block::plan(&input(json!({"plots": plots, "streets": streets, "rules": {"block.spacing": 500}}))).unwrap_err();
    assert!(e.contains("block.spacing"), "{e}");
}

/// The city block of the live scenario: 120 x 90 m between streets 12 m wide, a 9-storey
/// apartment block on the north edge, a 6-storey office on the east, a 3-storey school in the
/// south-west corner and a 2-storey shop pavilion in the middle, at least 12 m apart.
fn city_block() -> J {
    let rect = |x0: f64, y0: f64, x1: f64, y1: f64| json!([[x0, y0], [x1, y0], [x1, y1], [x0, y1]]);
    let s = 12.0;
    json!({
        "plots": [
            {"id": "N", "ring": rect(0.0, 62.0, 96.0, 90.0),
             "overrides": {"typology": "apartment_section", "floors": 9, "name": "Apartments"}},
            {"id": "E", "ring": rect(84.0, 0.0, 120.0, 48.0),
             "overrides": {"typology": "office", "floors": 6, "name": "Office"}},
            {"id": "SW", "ring": rect(0.0, 0.0, 44.0, 40.0),
             "overrides": {"typology": "school", "floors": 3, "name": "School"}},
            {"id": "M", "ring": rect(56.0, 12.0, 76.0, 46.0),
             "overrides": {"typology": "office", "floors": 2, "name": "Shop pavilion",
                           "ground_rooms": [{"kind": "retail", "area": 80, "count": 2}]}},
        ],
        "streets": [rect(-s, -s, 120.0 + s, 0.0), rect(-s, 90.0, 120.0 + s, 90.0 + s),
                    rect(-s, 0.0, 0.0, 90.0), rect(120.0, 0.0, 120.0 + s, 90.0)],
        "rules": {"block.spacing": 12},
    })
}

#[test]
fn the_city_block_brief_keeps_its_storeys_and_every_gap() {
    let t = Instant::now();
    let b = block::generate(&input(city_block()), block::workers()).unwrap_or_else(|e| panic!("{e}"));
    eprintln!("city block 4 buildings: {:.2} s, min gap {:.2} m", t.elapsed().as_secs_f64(), b.report.min_gap);
    assert_block(&b, "city block");
    let floors: Vec<(&str, u32)> = b.plan.plots.iter().map(|p| (p.id.as_str(), p.floors)).collect();
    assert_eq!(floors, [("N", 9), ("E", 6), ("SW", 3), ("M", 2)]);
    for (p, built) in b.plan.plots.iter().zip(&b.built) {
        assert_eq!(built.project.storeys.len() as u32, p.floors, "plot {}: storeys asked are built", p.id);
    }
    assert!(b.report.min_gap + 1e-3 >= 12.0, "{}", b.report.min_gap);
    for f in &b.report.facts {
        assert!(f.nearest + 1e-3 >= 12.0, "{f:?}");
    }
}

#[test]
fn a_plot_that_does_not_touch_its_neighbour_still_keeps_the_spacing() {
    // 4 m of open land between the plots: the side set-backs alone would leave 8 m
    let plots = json!([
        {"id": "A", "ring": [[0, 0], [30, 0], [30, 36], [0, 36]]},
        {"id": "B", "ring": [[34, 0], [64, 0], [64, 36], [34, 36]]},
    ]);
    let streets = json!([[[-12, -12], [76, -12], [76, 0], [-12, 0]]]);
    let args = json!({"plots": plots, "streets": streets,
        "rules": {"block.spacing": 12, "block.storeys_min": 2, "block.storeys_max": 2}});
    let b = block::generate(&input(args), 2).unwrap_or_else(|e| panic!("{e}"));
    assert_block(&b, "two plots 4 m apart");
    assert!(b.report.min_gap + 1e-3 >= 12.0, "{}", b.report.min_gap);
    // the facing edges carry the rest of the spacing, named
    let a = &b.plan.plots[0];
    assert!((a.setbacks[1] - 4.0).abs() < 1e-6, "{:?}", a.setbacks);
    assert!(a.why.iter().any(|w| w.contains("edge 1") && w.contains("block.spacing") && w.contains("plot B")), "{:?}", a.why);
    // the other edges keep their own rules
    assert!((a.setbacks[2] - 2.0).abs() < 1e-6, "{:?}", a.setbacks);
}

#[test]
fn overlapping_plots_are_refused_by_name() {
    let plots = json!([
        {"id": "A", "ring": [[0, 0], [30, 0], [30, 30], [0, 30]]},
        {"id": "B", "ring": [[20, 10], [50, 10], [50, 40], [20, 40]]},
    ]);
    let e = block::plan(&input(json!({"plots": plots}))).unwrap_err();
    assert!(e.contains("plots A and B overlap"), "{e}");
    let same = json!([
        {"id": "A", "ring": [[0, 0], [30, 0], [30, 30], [0, 30]]},
        {"id": "B", "ring": [[0, 0], [30, 0], [30, 30], [0, 30]]},
    ]);
    assert!(block::plan(&input(json!({"plots": same}))).unwrap_err().contains("overlap"));
}
