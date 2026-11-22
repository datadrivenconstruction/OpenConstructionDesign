// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The site: the zoning envelope and its named rule conflicts, basements on plotted sites,
//! the ground level on the terrain, the stepped plinth, approaches at grade, cut and fill.

#[path = "../../bench/suite.rs"]
mod suite;

use ddc_gen::project::site::{self, Terrain};
use ddc_gen::project::why::explain_project;
use ddc_gen::project::{build_project, check_project, merge_patch, parse_spec, GProject, ProjectSpec, RoomKind};
use serde_json::{json, Value as J};

type P = (f64, f64);

fn spec(v: &J) -> ProjectSpec {
    parse_spec(v).unwrap_or_else(|e| panic!("{e}"))
}

fn built(v: &J) -> (ProjectSpec, GProject) {
    let s = spec(v);
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    (s, p)
}

fn bbox(r: &[P]) -> (P, P) {
    r.iter().fold(((f64::MAX, f64::MAX), (f64::MIN, f64::MIN)), |(lo, hi), q| {
        ((lo.0.min(q.0), lo.1.min(q.1)), (hi.0.max(q.0), hi.1.max(q.1)))
    })
}

fn area(r: &[P]) -> f64 {
    let n = r.len();
    ((0..n).map(|i| r[i].0 * r[(i + 1) % n].1 - r[(i + 1) % n].0 * r[i].1).sum::<f64>() / 2.0).abs()
}

fn no_failures(s: &ProjectSpec, p: &GProject, what: &str) {
    let g = check_project(s, p);
    assert!(g.failures.is_empty(), "{what}: {:?}", g.failures);
}

#[test]
fn the_steep_bench_briefs_build_on_their_slope_with_basements_and_no_hard_failure() {
    let steep: Vec<suite::Brief> = suite::load_briefs().into_iter().filter(|b| b.group == "steep_site").collect();
    assert!(steep.len() >= 2);
    for b in &steep {
        let (s, p) = built(&suite::spec_of(b));
        let n = &b.name;
        no_failures(&s, &p, n);
        assert!(s.basements > 0 && p.basements.len() == s.basements as usize, "{n}: basements on the plot");
        let ground = &p.storeys[0].layout.slab;
        for st in &p.basements {
            for q in &st.layout.slab {
                assert!(suite::inside(*q, ground) || suite::dist_poly(*q, ground) < 0.01, "{n}: basement corner {q:?} off the building");
            }
        }
        let g = &p.site;
        assert!(g.terrain && g.stepped, "{n}: slope {} not stepped", g.slope);
        assert!(!g.walls.is_empty() && g.cut > 0.0 && g.fill > 0.0, "{n}: cut {} fill {}", g.cut, g.fill);
        assert!(g.walls.iter().all(|w| w.top > w.base), "{n}");
        assert!(g.excavation > 0.0, "{n}: basements dig");
        assert!(!g.approaches.is_empty(), "{n}: no exterior door");
        assert!(g.approaches.iter().all(|a| a.kind.is_some()), "{n}: {:?}", g.approaches);
    }
}

fn office_on_plot(extra: J) -> J {
    let mut v = json!({"typology": "office", "floors": 4,
        "footprint": {"plot": [[0, 0], [60, 0], [60, 40], [0, 40]]},
        "site": {"setbacks": [6, 3, 8, 3], "build_to": [0]}});
    merge_patch(&mut v, &extra);
    v
}

/// Length of the ring's edges on the line y = `y`.
fn on_y(r: &[P], y: f64) -> f64 {
    let n = r.len();
    (0..n)
        .filter(|&i| (r[i].1 - y).abs() < 0.01 && (r[(i + 1) % n].1 - y).abs() < 0.01)
        .map(|i| (r[(i + 1) % n].0 - r[i].0).abs())
        .sum()
}

#[test]
fn the_plate_keeps_its_set_backs_build_to_line_and_limits() {
    // per-edge set-backs: south 6 (build-to), east 3, north 8, west 3
    let (s, p) = built(&office_on_plot(json!({})));
    no_failures(&s, &p, "set-backs");
    for st in &p.storeys {
        let ((x0, y0), (x1, y1)) = bbox(&st.layout.slab);
        assert!(x0 > 2.99 && y0 > 5.99 && x1 < 57.01 && y1 < 32.01, "{:?}", st.layout.slab);
    }
    assert!(on_y(&p.storeys[0].layout.slab, 6.0) >= 0.6 * 54.0);

    // coverage 0.4 of 2400 m2: the facades off the build-to line move in, the south one stays
    let (s, p) = built(&office_on_plot(json!({"site": {"coverage_max": 0.4}})));
    no_failures(&s, &p, "coverage");
    let ground = &p.storeys[0].layout.slab;
    assert!(area(ground) <= 0.4 * 2400.0 * 1.002, "{}", area(ground));
    assert!(p.site.coverage <= 0.4 * 1.002 && p.site.coverage > 0.35, "{}", p.site.coverage);
    assert!(on_y(ground, 6.0) >= 0.6 * 54.0, "build-to kept");
    assert!(s.notes.iter().any(|n| n.contains("site.coverage_max")), "{:?}", s.notes);

    // floor-area ratio 1.2 over 4 floors binds harder than the coverage
    let (s, p) = built(&office_on_plot(json!({"rules": {"site.far_max": 1.2}})));
    no_failures(&s, &p, "far");
    assert!(p.site.far <= 1.2 * 1.002, "{}", p.site.far);
}

#[test]
fn a_broken_limit_is_a_conflict_that_names_its_rule() {
    for (extra, rule) in [
        (json!({"site": {"max_storeys": 3}}), "site.max_storeys"),
        (json!({"rules": {"site.max_storeys": 3}}), "site.max_storeys"),
        (json!({"site": {"max_height": 10}}), "site.max_height"),
    ] {
        let e = build_project(&spec(&office_on_plot(extra.clone()))).unwrap_err().to_string();
        assert!(e.contains("site rule conflict") && e.contains(rule), "{extra}: {e}");
    }
    // a bar that does not fit the plot's set-backs
    let bar = json!({"typology": "office", "floors": 2, "site": {"plot": [[0, 0], [20, 0], [20, 12], [0, 12]], "setbacks": [2, 2, 2, 2]}});
    let e = build_project(&spec(&bar)).unwrap_err().to_string();
    assert!(e.contains("site.plot_setback"), "{e}");
    // every edge a build-to line leaves nothing to move in for the coverage
    let all = office_on_plot(json!({"site": {"build_to": [0, 1, 2, 3], "coverage_max": 0.3}}));
    let e = parse_spec(&all).unwrap_err().to_string();
    assert!(e.contains("site.coverage_max"), "{e}");
}

#[test]
fn cut_and_fill_match_a_plane_slope() {
    let t = Terrain::Plane { z: 0.0, slope: 0.1, downhill: (1.0, 0.0) };
    let ring = [(0.0, 0.0), (20.0, 0.0), (20.0, 10.0), (0.0, 10.0)];
    let (cut, fill) = site::cut_fill(&ring, -1.0, &t).unwrap();
    // a level at the mean of a plane: s * L * A / 8 each side
    assert!((cut - 50.0).abs() < 0.5 && (fill - 50.0).abs() < 0.5, "{cut} {fill}");
    let (cut, fill) = site::cut_fill(&ring, 0.0, &t).unwrap();
    assert!(cut.abs() < 1e-9 && (fill - 200.0).abs() < 1.0, "{cut} {fill}");

    // a whole building on its pad (no margin): balanced at the mean level
    let (_, p) = built(&json!({"typology": "office", "floors": 2,
        "site": {"terrain": {"slope": 0.1, "downhill": [1, 0], "z": 5}, "pad_margin": 0}}));
    let ((x0, y0), (x1, y1)) = bbox(&p.storeys[0].layout.slab);
    let (l, a) = (x1 - x0, (x1 - x0) * (y1 - y0));
    let each = 0.1 * l * a / 8.0;
    let g = &p.site;
    assert!((g.cut + g.fill - 2.0 * each).abs() < 0.03 * 2.0 * each, "cut {} fill {} vs {each} each", g.cut, g.fill);
    assert!((g.cut - g.fill).abs() <= a * 0.03, "level snapped to 5 cm: {} {}", g.cut, g.fill);
    assert!((g.ground_z - (5.0 - 0.1 * (x0 + x1) / 2.0)).abs() <= 0.03, "{}", g.ground_z);
    assert!(!g.stepped && (g.slope - 0.1).abs() < 1e-6, "{}", g.slope);

    // the same plane as survey points gives the same ground and volumes
    let pts: Vec<J> = (-2..=8)
        .flat_map(|i| (-2..=5).map(move |j| (f64::from(i) * 10.0, f64::from(j) * 10.0)))
        .map(|(x, y)| json!([x, y, 5.0 - 0.1 * x]))
        .collect();
    let (_, q) = built(&json!({"typology": "office", "floors": 2,
        "site": {"terrain": {"points": pts}, "pad_margin": 0}}));
    assert!((q.site.ground_z - g.ground_z).abs() < 1e-6, "{} {}", q.site.ground_z, g.ground_z);
    assert!((q.site.cut - g.cut).abs() < 0.01 * g.cut && (q.site.fill - g.fill).abs() < 0.01 * g.fill);
}

fn sloped(slope: f64, site: J) -> J {
    let mut v = json!({"typology": "office", "floors": 3, "basements": 1,
        "footprint": {"plot": [[0, 0], [50, 0], [50, 36], [0, 36]], "plot_setback": 4},
        "site": {"terrain": {"slope": slope, "downhill": [1, 0]}}});
    merge_patch(&mut v, &json!({ "site": site }));
    v
}

#[test]
fn rules_and_locks_steer_the_level_and_the_plinth() {
    let (s, p) = built(&sloped(0.3, json!({})));
    no_failures(&s, &p, "steep");
    assert!(p.site.stepped && p.site.ground_by == "balance");
    assert!(p.site.walls.iter().any(|w| w.terrace > 0), "terraces of the stepped plinth");
    assert!(p.site.pads.len() > 1, "terrace pads");
    assert!(p.basements.len() == 1);

    // the rule threshold, not the slope alone, decides the plinth
    let mut v = sloped(0.3, json!({}));
    merge_patch(&mut v, &json!({"rules": {"site.step_slope": 0.5}}));
    let (_, p) = built(&v);
    assert!(!p.site.stepped);
    let (_, p) = built(&sloped(0.03, json!({})));
    assert!(!p.site.stepped && p.site.walls.iter().all(|w| w.terrace == 0));

    // entrance at grade: the floor meets the terrain at the main door
    let (s, p) = built(&sloped(0.1, json!({"ground": "entrance"})));
    no_failures(&s, &p, "entrance");
    let main = p.site.approaches.iter().find(|a| a.main).expect("a main door");
    let at = 0.0 - 0.1 * main.door.0;
    assert!(p.site.ground_by == "entrance" && (p.site.ground_z - at).abs() <= 0.06, "{} vs {at}", p.site.ground_z);

    // locks keep a level and a plinth form from an earlier run
    let (_, p) = built(&sloped(0.3, json!({"locks": {"ground_z": -2.0, "plinth": "flat"}})));
    assert!(p.site.ground_z == -2.0 && p.site.ground_by == "locked" && !p.site.stepped);
    assert!(p.site.walls.iter().all(|w| w.terrace == 0));

    // every site decision says which rule made it
    let (s, p) = built(&sloped(0.3, json!({})));
    let whys = explain_project(&s, &p);
    for rule in ["site.ground_rule", "site.step_slope", "site.retain_min", "site.entrance_step", "site.plot_setback"] {
        assert!(whys.iter().any(|w| w.target == "site" && w.rule == rule), "no reason by {rule}");
    }
    assert!(p.metrics.iter().any(|m| m.0 == "cut_m3") && p.metrics.iter().any(|m| m.0 == "ground_z"));
}

#[test]
fn basements_build_under_plotted_plates() {
    for plot in [
        json!([[0, 0], [62, 0], [50, 34], [8, 30]]),
        json!([[0, 0], [52, 0], [52, 24], [6, 30], [0, 24]]),
    ] {
        let v = json!({"typology": "office", "floors": 4, "basements": 1,
            "footprint": {"plot": plot, "plot_setback": 3}});
        let (s, p) = built(&v);
        no_failures(&s, &p, "plot basement");
        assert_eq!(p.basements.len(), 1);
        let ground = &p.storeys[0].layout.slab;
        let b = &p.basements[0].layout.slab;
        assert!(b.iter().all(|q| suite::inside(*q, ground) || suite::dist_poly(*q, ground) < 0.01), "{b:?}");
        assert!(area(b) > 0.4 * area(ground), "{} of {}", area(b), area(ground));
    }
}

// a corridor wide enough for a platform lift beside each half-level flight
fn split_brief(extra: J) -> J {
    let mut v = json!({"typology": "office", "floors": 3, "roof": {"kind": "flat"}, "corridor": 2.6,
        "site": {"terrain": {"slope": 0.3, "downhill": [1, 0]}}});
    merge_patch(&mut v, &extra);
    v
}

fn wet(k: RoomKind) -> bool {
    matches!(k, RoomKind::Wc | RoomKind::Bath | RoomKind::Kitchen)
}

#[test]
fn a_steep_slope_splits_the_levels_and_digs_less_than_one_pad() {
    let (s, p) = built(&split_brief(json!({})));
    no_failures(&s, &p, "split");
    let g = &p.site;
    let sp = g.split.as_ref().unwrap_or_else(|| panic!("one level: {}", g.split_why));
    assert!((sp.drop - p.storeys[0].height / 2.0).abs() < 1e-9, "half a storey: {}", sp.drop);
    assert!(sp.share >= 0.25 && sp.share <= 0.75, "{}", sp.share);
    assert!(sp.cuts.iter().all(|c| c.low.0 > 0.9), "the downhill block is the lower one: {:?}", sp.cuts);
    let mut wet_low: Vec<(P, bool)> = vec![];
    for (k, st) in p.blocks() {
        let l = &st.layout;
        // a flight of exactly the drop joins the half-levels of every storey
        let mine: Vec<_> = sp.flights.iter().filter(|f| f.0 == k).collect();
        assert!(!mine.is_empty(), "{}: no flight", st.name);
        for (_, f) in mine {
            let lay = f.def().layout().unwrap();
            assert!((lay.risers as f64 * lay.riser - sp.drop).abs() < 1e-6, "{}", st.name);
            assert!(lay.riser <= s.max_riser + 1e-9);
        }
        for w in &l.walls {
            assert!(!sp.crosses(&[w.a, w.b]), "{}: wall {w:?} on both half-levels", st.name);
        }
        for (gr, pr) in l.rooms.iter().zip(&st.rooms) {
            if !matches!(pr.kind, RoomKind::Corridor | RoomKind::Lobby) {
                assert!(!sp.crosses(&gr.polygon), "{} room {} on both half-levels", st.name, pr.number);
            }
            // a wet room over a wet room keeps its half-level: the riser runs straight
            if wet(pr.kind) {
                let low = sp.is_low(gr.point);
                for (q, was) in &wet_low {
                    if (q.0 - gr.point.0).abs() < 1e-6 && (q.1 - gr.point.1).abs() < 1e-6 {
                        assert_eq!(*was, low, "{} {}", st.name, pr.number);
                    }
                }
                wet_low.push((gr.point, low));
            }
        }
    }
    // the two pads half a storey apart and the step held between them
    assert!(g.pads.iter().any(|x| (x.z - g.ground_z).abs() < 1e-9));
    assert!(g.pads.iter().any(|x| (x.z - (g.ground_z - sp.drop)).abs() < 1e-9));
    assert!(g.walls.iter().any(|w| (w.top - g.ground_z).abs() < 1e-9 && (w.base - (g.ground_z - sp.drop)).abs() < 1e-9));
    assert!(g.approaches.iter().all(|a| a.kind.is_some()), "{:?}", g.approaches);

    // the plinth it replaces: the same brief with the plinth locked stepped, one pad
    let (s2, q) = built(&split_brief(json!({"site": {"locks": {"plinth": "stepped"}}})));
    no_failures(&s2, &q, "plinth");
    assert!(q.site.split.is_none() && q.site.stepped);
    let one = q.site.cut + q.site.fill;
    assert!((sp.plinth_cut + sp.plinth_fill - one).abs() < 1e-6, "{} vs {one}", sp.plinth_cut + sp.plinth_fill);
    assert!(g.cut + g.fill < one, "split {} m3 against one pad {one} m3", g.cut + g.fill);
    // a plane under a level pad: cut and fill s * L * A / 8 each side, as the analytic slope
    let (_, nopad) = built(&split_brief(json!({"site": {"locks": {"plinth": "flat"}, "pad_margin": 0}})));
    let ((x0, y0), (x1, y1)) = bbox(&nopad.storeys[0].layout.slab);
    let each = 0.3 * (x1 - x0) * (x1 - x0) * (y1 - y0) / 8.0;
    assert!((nopad.site.cut + nopad.site.fill - 2.0 * each).abs() < 0.03 * 2.0 * each);

    // the why names the rule and both volumes
    let whys = explain_project(&s, &p);
    assert!(whys.iter().any(|w| w.rule == "site.split_slope" && w.target == "site"));
    assert!(p.metrics.iter().any(|m| m.0 == "split_drop" && (m.1 - sp.drop).abs() < 1e-9));
}

#[test]
fn rules_and_locks_choose_split_levels() {
    // a higher threshold keeps one level and says why
    let (_, p) = built(&split_brief(json!({"rules": {"site.split_slope": 0.5}})));
    assert!(p.site.split.is_none() && p.site.split_why.contains("site.split_slope"), "{}", p.site.split_why);
    // the lock splits a gentle slope too
    let (s, p) = built(&split_brief(json!({"site": {"terrain": {"slope": 0.1}, "locks": {"plinth": "split"}}})));
    no_failures(&s, &p, "locked split");
    assert!(p.site.split.is_some(), "{}", p.site.split_why);
    // a pitched roof parts too: two roofs, each with its own ridge, the cut edge in the form
    // of the roof's edge along the cut
    let (s, p) = built(&split_brief(json!({"roof": {"kind": "pitched", "slope": 30}})));
    no_failures(&s, &p, "pitched");
    let sp = p.site.split.as_ref().unwrap_or_else(|| panic!("one level: {}", p.site.split_why));
    let (hi, lo) = sp.roofs(&p.roof);
    assert!(!hi.is_empty() && !lo.is_empty(), "{hi:?} {lo:?}");
    let whole = area(&p.roof.ring);
    assert!((hi.iter().chain(&lo).map(|r| area(&r.ring)).sum::<f64>() - whole).abs() < 1e-6);
    for r in hi.iter().chain(&lo) {
        assert!(r.ring.len() >= 3 && r.ring.len() < p.roof.ring.len() + 3, "{:?}", r.ring);
        if !p.roof.edges.is_empty() {
            assert_eq!(r.edges.len(), r.ring.len());
            // a sloped edge on each side keeps a ridge in every piece
            assert!(r.edges.iter().filter(|e| e.is_some()).count() >= 2, "{:?}", r.edges);
        }
    }
    // deterministic, and the cuts move with the origin
    let (_, a) = built(&split_brief(json!({})));
    let (_, b) = built(&split_brief(json!({})));
    assert_eq!(a, b);
    let (_, c) = built(&split_brief(json!({"origin": [100.0, -40.0]})));
    let (x, y) = (a.site.split.as_ref().unwrap(), c.site.split.as_ref().unwrap());
    let (cx, cy) = (&x.cuts[0], &y.cuts[0]);
    assert!((cy.at - cx.at - (100.0 * cx.low.0 - 40.0 * cx.low.1)).abs() < 1e-9);
    assert_eq!(x.flights.len(), y.flights.len());
    assert_eq!(x.access.len(), y.access.len());
}

#[test]
fn a_lift_serves_both_half_levels_and_every_room_reaches_an_entrance() {
    use site::Access;
    // by default a platform lift beside each flight on the storeys the lift serves
    let (s, p) = built(&split_brief(json!({})));
    no_failures(&s, &p, "split");
    let sp = p.site.split.as_ref().unwrap_or_else(|| panic!("one level: {}", p.site.split_why));
    assert!(p.storeys.iter().any(|st| st.rooms.iter().any(|r| r.kind == RoomKind::Lift)));
    let lost = site::barrier_free(&p);
    assert!(lost.is_empty(), "{lost:?}");
    let platforms: Vec<_> = sp.access.iter().filter(|a| a.kind == Access::Platform).collect();
    assert!(!platforms.is_empty(), "{:?}", sp.access);
    for a in &platforms {
        let st = p.blocks().find(|b| b.0 == a.block).unwrap().1;
        assert!(matches!(st.rooms[a.room].kind, RoomKind::Corridor | RoomKind::Lobby));
        assert!(st.layout.fixtures.iter().any(|f| f.family == "Platform Lift" && f.at == a.at));
        // it stands on the lower half-level, clear of the flight
        assert!(sp.is_low(a.at));
    }
    let lifts = p.metrics.iter().find(|m| m.0 == "split_platform_lifts").map(|m| m.1);
    assert_eq!(lifts, Some(platforms.len() as f64));
    // the flights alone would leave the lower half-levels of the upper storeys without a way:
    // the check finds them once the platform lifts are gone
    let mut q = p.clone();
    if let Some(x) = q.site.split.as_mut() {
        x.access.clear();
    }
    let lost = site::barrier_free(&q);
    assert!(!lost.is_empty(), "{lost:?}");

    // a corridor too narrow for a platform beside the flight: split_lift asks for a second
    // lift, else the building keeps one level and says why
    let (s, p) = built(&split_brief(json!({"corridor": 1.8})));
    no_failures(&s, &p, "narrow");
    match &p.site.split {
        Some(sp) => {
            assert!(sp.access.iter().any(|a| a.kind == Access::Lift), "{:?}", sp.access);
            assert!(site::barrier_free(&p).is_empty());
        }
        None => assert!(p.site.split_why.contains("site.split_lift") || p.site.split_why.contains("site.platform"), "{}", p.site.split_why),
    }
    let (s, p) = built(&split_brief(json!({"site": {"split_lift": "lift"}})));
    no_failures(&s, &p, "second lift");
    if let Some(sp) = &p.site.split {
        assert!(site::barrier_free(&p).is_empty());
        assert!(!sp.access.is_empty());
    }
}

#[test]
fn wings_and_courts_part_on_a_cut_each() {
    for massing in ["u", "courtyard", "l"] {
        let (s, p) = built(&split_brief(json!({"design": {"massing": massing}, "floors": 2,
            "site": {"locks": {"plinth": "split"}}})));
        no_failures(&s, &p, massing);
        let Some(sp) = &p.site.split else {
            panic!("{massing}: one level: {}", p.site.split_why);
        };
        assert!(site::barrier_free(&p).is_empty(), "{massing}");
        // every slab and roof stands in pieces on one half-level each
        for (_, st) in p.blocks() {
            assert!(sp.pieces(&st.layout.slab, &st.layout.slab_holes).is_some(), "{massing} {}", st.name);
        }
        for r in std::iter::once(&p.roof).chain(&p.extra_roofs) {
            let (hi, lo) = sp.roofs(r);
            let sum: f64 = hi.iter().chain(&lo).map(|x| area(&x.ring)).sum();
            assert!((sum - area(&r.ring)).abs() < 1e-6, "{massing}");
        }
        if massing == "courtyard" {
            assert!(sp.cuts.len() >= 2, "a cut in each wing across the slope: {:?}", sp.cuts);
        }
    }
}

#[test]
fn the_site_is_deterministic_and_moves_with_the_origin() {
    let v = sloped(0.25, json!({"setbacks": [4, 6, 4, 2], "build_to": [3]}));
    let (_, a) = built(&v);
    let (_, b) = built(&v);
    assert_eq!(a, b);
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
    let mut w = v.clone();
    merge_patch(&mut w, &json!({"origin": [100.0, -40.0]}));
    let (s, c) = built(&w);
    no_failures(&s, &c, "moved");
    assert!((c.site.ground_z - a.site.ground_z).abs() < 1e-9 && (c.site.cut - a.site.cut).abs() < 1e-6);
    let (pa, pc) = (&a.site.walls, &c.site.walls);
    assert_eq!(pa.len(), pc.len());
    for (x, y) in pa.iter().zip(pc) {
        assert!((y.a.0 - x.a.0 - 100.0).abs() < 1e-9 && (y.a.1 - x.a.1 + 40.0).abs() < 1e-9);
    }
}
