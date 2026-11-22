// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Any geometry (R7): non-orthogonal grids, curved facades, stepped, terraced, twisted,
//! winged and ring plans, roofs on any outline, and cores carried through every storey.
//! Each free-form brief of the bench is valid, free of clashes, inside its area rule, fully
//! reachable through doors and built identically twice.

#[path = "../../bench/suite.rs"]
mod suite;

use ddc_gen::project::rules::{self, RuleSet};
use ddc_gen::project::{build_project, check_project, curve_walls, parse_spec, usable_rect, GProject, ProjectSpec};
use serde_json::{json, Value as J};

fn built(v: &J) -> (ProjectSpec, GProject) {
    let s = parse_spec(v).unwrap_or_else(|e| panic!("{v}: {e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{v}: {:?}", g.failures);
    (s, p)
}

/// A plate exactly two bands and a corridor deep keeps them; a thinner one narrows its bands
/// towards the rule's least depth; one thinner still is refused with both tries named.
#[test]
fn a_thin_plate_narrows_its_bands_before_it_is_refused() {
    let poly = |d: f64| json!({"typology": "office", "floors": 1, "footprint": {"polygon": [[0.0, 0.0], [30.0, 0.0], [30.0, d], [0.0, d]]}});
    built(&poly(12.0));
    built(&poly(9.0));
    let s = parse_spec(&poly(7.0)).unwrap();
    let e = build_project(&s).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(e.contains("narrowed"), "{e}");
}

/// A room round a corner of a square plate keeps a corridor run its door fits: every room of
/// every storey has a door.
#[test]
fn corner_rooms_of_a_square_plate_keep_a_door_to_the_corridor() {
    for side in [30.0, 45.0] {
        let v = json!({"typology": "office", "floors": 3, "footprint": {"polygon": [[0.0, 0.0], [side, 0.0], [side, side], [0.0, side]]}});
        let (_, p) = built(&v);
        for (k, s) in p.storeys.iter().enumerate() {
            let l = &s.layout;
            for (i, r) in l.rooms.iter().enumerate() {
                let n = r.polygon.len();
                let touches = |w: &ddc_gen::GWall| {
                    let on = |q: (f64, f64)| (0..n).any(|j| {
                        let (a, b) = (r.polygon[j], r.polygon[(j + 1) % n]);
                        let (ux, uy, vx, vy) = (b.0 - a.0, b.1 - a.1, q.0 - a.0, q.1 - a.1);
                        let len2 = ux * ux + uy * uy;
                        let t = (ux * vx + uy * vy) / len2.max(1e-12);
                        (ux * vy - uy * vx).abs() / len2.sqrt().max(1e-12) < 1e-6 && (-1e-6..=1.0 + 1e-6).contains(&t)
                    });
                    on(w.a) && on(w.b)
                };
                let doors = l.openings.iter().filter(|o| o.door && touches(&l.walls[o.wall])).count();
                assert!(doors > 0, "side {side}, storey {k}: room {} has no door", s.rooms.get(i).map_or("corridor".to_string(), |x| x.number.clone()));
            }
        }
    }
}

fn metric(p: &GProject, id: &str) -> f64 {
    p.metrics.iter().find(|m| m.0 == id).unwrap_or_else(|| panic!("no metric {id}")).1
}

fn bench(name: &str) -> suite::Brief {
    suite::load_briefs().into_iter().find(|b| b.name == name).unwrap_or_else(|| panic!("{name}"))
}

fn builtin(id: &str) -> f64 {
    RuleSet::new().value(id).unwrap_or_else(|| panic!("{id}"))
}

const FORMS: [&str; 8] = [
    "form_round_tower",
    "form_ellipse_twist",
    "form_bow_facade",
    "form_ziggurat",
    "form_terrace",
    "form_y_wings",
    "form_ring_court",
    "form_skew_office",
];

/// Share of the asked rooms, %, that must hold the usable rectangle on every free form.
const USABLE_ROOMS: f64 = 75.0;

#[test]
fn every_free_form_brief_is_sound() {
    let briefs = suite::load_briefs();
    let free: Vec<&suite::Brief> = briefs.iter().filter(|b| b.group == "free_form").collect();
    assert_eq!(free.len(), FORMS.len());
    let area_rule = 100.0 * builtin("room.area_error");
    for name in FORMS {
        let b = free.iter().find(|b| b.name == name).unwrap_or_else(|| panic!("{name} not in the bench"));
        let r = suite::measure(b);
        assert!(r.refused.is_none(), "{name}: {:?}", r.refused);
        let m = |id: &str| r.get(id).unwrap_or_else(|| panic!("{name}: {id} not measured"));
        assert_eq!(m("hard_failures"), 0.0, "{name}: {}", r.detail);
        assert_eq!(m("clashes"), 0.0, "{name}: {}", r.detail["clashes"]);
        assert_eq!(m("egress_reachable"), 100.0, "{name}");
        assert_eq!(m("deterministic"), 100.0, "{name}");
        assert!(m("programme_error") <= area_rule, "{name}: programme error {} over {area_rule}", m("programme_error"));
        if let Some(c) = r.get("column_continuity") {
            assert_eq!(c, 100.0, "{name}: columns carried");
        }
    }
}

/// Every roof closes by the straight skeleton, rooms hold their usable rectangle, every
/// column stands on one below, and the core stands in the same place on every storey.
#[test]
fn forms_keep_roof_rooms_columns_and_core() {
    for name in FORMS {
        let (_, p) = built(&bench(name).spec);
        assert_eq!(metric(&p, "roof_skeleton") as usize, 1 + p.extra_roofs.len(), "{name}: a roof does not close");
        assert!(metric(&p, "usable_rooms") >= USABLE_ROOMS, "{name}: usable rooms {}", metric(&p, "usable_rooms"));
        let first = &p.storeys[0].layout;
        assert!(!first.grids.is_empty(), "{name}: no grid");
        for (k, s) in p.storeys.iter().enumerate().skip(1) {
            let below = &p.storeys[k - 1].layout.columns;
            assert!(s.layout.columns.iter().all(|c| below.contains(c)), "{name}: a column of storey {k} stands on nothing");
            let shafts: Vec<_> = s.layout.shafts.iter().map(|x| x.ring.clone()).collect();
            let base: Vec<_> = first.shafts.iter().map(|x| x.ring.clone()).collect();
            assert_eq!(shafts, base, "{name}: lift shaft of storey {k}");
            if let (Some(a), Some(b)) = (s.layout.stairs.first(), first.stairs.first()) {
                assert_eq!((a.origin, a.angle), (b.origin, b.angle), "{name}: stair of storey {k}");
            }
        }
    }
}

#[test]
fn the_grid_follows_the_form() {
    // auto: 1 ortho, 2 skew, 3 radial, 4 edge
    for (name, form) in [
        ("form_round_tower", 3.0),
        ("form_ellipse_twist", 3.0),
        ("form_bow_facade", 4.0),
        ("form_ziggurat", 1.0),
        ("form_skew_office", 2.0),
    ] {
        let (_, p) = built(&bench(name).spec);
        assert_eq!(metric(&p, "grid_form"), form, "{name}");
    }
    // the rule overrides the reading of the outline
    let mut v = bench("form_bow_facade").spec;
    v["rules"] = json!({"grid.form": "ortho", "grid.angle": 30});
    let (_, p) = built(&v);
    assert_eq!(metric(&p, "grid_form"), 1.0);
    let g = &p.storeys[0].layout.grids[0];
    let deg = (g.b.1 - g.a.1).atan2(g.b.0 - g.a.0).to_degrees().rem_euclid(180.0);
    assert!((deg - 30.0).abs() < 1e-6, "{deg}");
    // the skewed grid keeps the plate's two facade directions
    let (_, p) = built(&bench("form_skew_office").spec);
    let mut dirs: Vec<f64> = p.storeys[0]
        .layout
        .grids
        .iter()
        .map(|g| ((g.b.1 - g.a.1).atan2(g.b.0 - g.a.0).to_degrees().rem_euclid(180.0) * 10.0).round() / 10.0)
        .collect();
    dirs.sort_by(f64::total_cmp);
    dirs.dedup();
    let side = 34f64.atan2(12.0).to_degrees();
    assert!(dirs.contains(&0.0) && dirs.iter().any(|d| (d - side).abs() < 0.11), "{dirs:?}");
}

#[test]
fn curved_facades_are_curved_walls() {
    let (_, p) = built(&bench("form_round_tower").spec);
    for s in &p.storeys {
        let cs = curve_walls(&s.layout);
        assert!(cs.len() >= 2, "a round storey is curved walls");
        for c in &cs {
            let (_, r) = c.arc.expect("a circle");
            assert!((r - 18.0).abs() < 0.2, "radius {r}");
            assert!(c.walls.len() >= 2 && c.through.len() >= 3);
        }
    }
    let (_, p) = built(&bench("form_ellipse_twist").spec);
    let cs = curve_walls(&p.storeys[5].layout);
    assert!(cs.len() >= 2 && cs.iter().all(|c| c.arc.is_none()), "an ellipse is splines");
    let (_, p) = built(&bench("form_bow_facade").spec);
    let cs = curve_walls(&p.storeys[0].layout);
    assert_eq!(cs.len(), 1, "one bowed facade");
    let (_, r) = cs[0].arc.expect("an arc");
    assert!((r - 102.25).abs() < 1.0, "radius {r}");
    // a box has none
    let (_, p) = built(&bench("form_ziggurat").spec);
    assert!(curve_walls(&p.storeys[0].layout).is_empty());
}

fn extent(p: &GProject, k: usize, dir: (f64, f64)) -> f64 {
    p.storeys[k]
        .layout
        .rooms
        .iter()
        .flat_map(|r| &r.polygon)
        .map(|q| q.0 * dir.0 + q.1 * dir.1)
        .fold(f64::MIN, f64::max)
}

#[test]
fn stepped_terraced_and_twisted_storeys() {
    let (_, p) = built(&bench("form_ziggurat").spec);
    let gross = |k: usize| p.storeys[k].layout.rooms.iter().map(|r| suite::area(&r.polygon)).sum::<f64>();
    assert!(gross(3) < gross(2) - 1.0 && gross(6) < gross(5) - 1.0, "steps every 3 storeys");
    assert!((gross(1) - gross(0)).abs() < 1.0);
    let (_, p) = built(&bench("form_terrace").spec);
    let north = |k: usize| extent(&p, k, (0.0, 1.0));
    assert!((north(0) - north(2) - 3.0).abs() < 0.05, "{} {}", north(0), north(2));
    assert!((north(0) - north(4) - 6.0).abs() < 0.05);
    let south = |k: usize| extent(&p, k, (0.0, -1.0));
    assert!((south(0) - south(5)).abs() < 1e-6, "the south side stays");
    let (_, p) = built(&bench("form_ellipse_twist").spec);
    assert_ne!(p.storeys[0].layout.slab, p.storeys[6].layout.slab, "the plate turns");
    let net = |k: usize| p.storeys[k].layout.rooms.iter().map(|r| suite::area(&r.polygon)).sum::<f64>();
    assert!((net(0) - net(6)).abs() < 0.01 * net(0), "the plate keeps its area: {} {}", net(0), net(6));
}

#[test]
fn branched_plans_are_one_union_of_arms() {
    let arm = |a: [f64; 2], b: [f64; 2]| json!({"from": a, "to": b, "width": 14});
    let plans = [
        ("L", vec![arm([0.0, 0.0], [48.0, 0.0]), arm([0.0, 0.0], [0.0, 40.0])]),
        ("U", vec![arm([0.0, 0.0], [56.0, 0.0]), arm([0.0, 0.0], [0.0, 36.0]), arm([56.0, 0.0], [56.0, 36.0])]),
        ("T", vec![arm([0.0, 0.0], [60.0, 0.0]), arm([30.0, 0.0], [30.0, 36.0])]),
        ("X", vec![arm([-30.0, 0.0], [30.0, 0.0]), arm([0.0, -30.0], [0.0, 30.0])]),
    ];
    for (what, arms) in plans {
        let v = json!({"typology": "office", "floors": 3, "footprint": {"wings": arms}});
        let (_, p) = built(&v);
        assert!(p.storeys.iter().all(|s| !s.rooms.is_empty()), "{what}");
    }
    let (_, p) = built(&bench("form_ring_court").spec);
    assert!(p.court.is_some(), "four arms round a court leave it open");
    let (_, p) = built(&bench("form_y_wings").spec);
    assert!(p.apartments > 0);
    // arms that do not meet are refused with the reason
    let v = json!({"typology": "office", "floors": 2, "footprint": {"wings": [
        arm([0.0, 0.0], [40.0, 0.0]), arm([0.0, 60.0], [40.0, 60.0])]}});
    let e = parse_spec(&v).unwrap_err().to_string();
    assert!(e.contains("footprint.wings") && e.contains("separate"), "{e}");
}

#[test]
fn the_usable_rectangle_is_exact() {
    let trap = [(0.0, 0.0), (6.0, 0.0), (5.0, 4.0), (1.0, 4.0)];
    let u = usable_rect(&trap, (1.0, 0.0), 2.4).unwrap();
    assert!((u.w - 4.0).abs() < 1e-6 && (u.h - 4.0).abs() < 1e-6, "{u:?}");
    assert!((u.share - 0.8).abs() < 1e-6);
    // the same room turned with its axis
    let t = 0.7f64;
    let turn = |p: &(f64, f64)| (p.0 * t.cos() - p.1 * t.sin(), p.0 * t.sin() + p.1 * t.cos());
    let turned: Vec<(f64, f64)> = trap.iter().map(turn).collect();
    let v = usable_rect(&turned, (t.cos(), t.sin()), 2.4).unwrap();
    assert!((v.w * v.h - 16.0).abs() < 1e-6, "{v:?}");
    // a sliver holds nothing that wide
    assert!(usable_rect(&[(0.0, 0.0), (6.0, 0.0), (0.0, 2.0)], (1.0, 0.0), 2.4).is_none());
}

fn inside(ring: &[(f64, f64)], p: (f64, f64)) -> bool {
    let n = ring.len();
    let mut c = false;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) * (b.0 - a.0) / (b.1 - a.1) {
            c = !c;
        }
    }
    c
}

/// A room with a corner that is not square: a trapezoid or a wedge.
fn oblique(ring: &[(f64, f64)]) -> bool {
    let n = ring.len();
    (0..n).any(|i| {
        let (a, b, c) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
        let (u, v) = ((b.0 - a.0, b.1 - a.1), (c.0 - b.0, c.1 - b.1));
        let cos = (u.0 * v.0 + u.1 * v.1) / (u.0.hypot(u.1) * v.0.hypot(v.1)).max(1e-12);
        cos.abs() > 0.02 && cos.abs() < 0.98
    })
}

/// Trapezoids of the skewed grid are furnished in the rectangle the usable rule guarantees:
/// every room that holds it gets a fit-out, and every piece stands inside a room.
#[test]
fn trapezoid_rooms_are_furnished_in_their_usable_rectangle() {
    let (_, p) = built(&bench("form_skew_office").spec);
    let min = builtin("grid.min_usable");
    let mut trapezoids = 0;
    for (k, s) in p.storeys.iter().enumerate() {
        let l = &s.layout;
        assert!(!l.fixtures.is_empty(), "storey {k} is not furnished");
        for f in &l.fixtures {
            assert!(l.rooms.iter().any(|r| inside(&r.polygon, f.at)), "storey {k}: {} at {:?} stands in no room", f.ty, f.at);
        }
        for (i, (gr, pr)) in l.rooms.iter().zip(&s.rooms).enumerate() {
            let used = pr.asked.is_some() && !pr.kind.circulation();
            if !used || !oblique(&gr.polygon) {
                continue;
            }
            let axis = (1.0, 0.0);
            if usable_rect(&gr.polygon, axis, min + 0.3).is_none() {
                continue;
            }
            trapezoids += 1;
            assert!(s.fit.iter().any(|f| f.room == i), "storey {k}: trapezoid room {} holds its rectangle but has no fit-out", gr.name);
            assert!(
                l.fixtures.iter().any(|f| inside(&gr.polygon, f.at)),
                "storey {k}: trapezoid room {} has no furniture", gr.name
            );
        }
    }
    assert!(trapezoids > 0, "the skewed office has trapezoid rooms");
    assert!(metric(&p, "fitted_rooms") > 0.0);
}

#[test]
fn the_new_rules_have_defaults() {
    for id in [
        "grid.form",
        "grid.angle",
        "grid.skew",
        "grid.ortho_tolerance",
        "grid.ray",
        "grid.curve_share",
        "grid.min_usable",
        "grid.usable_share",
        "massing.step_every",
        "massing.step_setback",
        "massing.step_mode",
        "massing.step_dir",
        "massing.wing_width",
        "room.area_error",
        "massing.vault_profile",
        "massing.vault_arc_span",
        "massing.vault_rise",
        "massing.vault_fill",
    ] {
        let d = rules::def(id).unwrap_or_else(|| panic!("{id}"));
        assert!(d.default.is_some(), "{id}");
    }
}

fn barrel(width: f64, depth: f64, rules: J) -> GProject {
    built(&json!({
        "typology": "office", "floors": 2,
        "footprint": {"shape": "rectangle", "width": width, "depth": depth},
        "roof": {"kind": "barrel"}, "rules": rules
    }))
    .1
}

fn top(v: &ddc_gen::project::PVault) -> f64 {
    v.profile.iter().map(|q| q.1).fold(f64::MIN, f64::max)
}

#[test]
fn barrel_roofs_take_the_profile_the_rules_choose() {
    let span = |v: &ddc_gen::project::PVault| (v.b.0 - v.a.0).hypot(v.b.1 - v.a.1);
    // auto, short span: a half circle across the short side, drawn along the long one
    let p = barrel(30.0, 12.0, json!({}));
    assert_eq!(metric(&p, "vaults"), 1.0);
    let v = p.roof.vault.as_ref().expect("a vault");
    assert!((span(v) - 12.0).abs() < 1e-6 && (v.depth - 30.0).abs() < 1e-6, "{v:?}");
    assert!((top(v) - 6.0).abs() < 0.06, "half circle rise {}", top(v));
    assert!(v.profile.windows(2).all(|w| w[1].0 > w[0].0));
    assert_eq!((v.profile[0], *v.profile.last().unwrap()), ((0.0, 0.0), (12.0, 0.0)));
    // auto, wide span: a segment rising massing.vault_rise of the span
    let p = barrel(40.0, 20.0, json!({}));
    let v = p.roof.vault.as_ref().expect("a vault");
    let rise = builtin("massing.vault_rise") * 20.0;
    assert!((top(v) - rise).abs() < 0.06, "segment rise {} for {rise}", top(v));
    // asked: a half ellipse of the same rise
    let p = barrel(30.0, 12.0, json!({"massing.vault_profile": "ellipse", "massing.vault_rise": 0.25}));
    let v = p.roof.vault.as_ref().expect("a vault");
    assert!((top(v) - 3.0).abs() < 0.06, "ellipse rise {}", top(v));
    // an L fills too little of its box: it keeps its gable
    let l = built(&json!({
        "typology": "office", "floors": 2,
        "footprint": {"polygon": [[0, 0], [40, 0], [40, 14], [14, 14], [14, 40], [0, 40]]},
        "roof": {"kind": "barrel"}
    }))
    .1;
    assert!(l.roof.vault.is_none());
    assert_eq!(metric(&l, "vaults"), 0.0);
}

/// A bowed bar keeps a roof that closes, whichever facade bows (the free-form museum brief
/// failed with a self-intersecting roof ring three times).
#[test]
fn bowed_bars_keep_a_closing_roof() {
    for (south, north) in [(5.0, 0.0), (0.0, 5.0), (5.0, 5.0), (3.0, 0.0)] {
        for roof in [json!(null), json!({"kind": "gable"}), json!({"kind": "hip"})] {
            let mut v = json!({
                "typology": "office", "floors": 3, "floor_height": 5.0,
                "footprint": {"width": 90, "depth": 30},
                "design": {"massing": "bar", "curve": south, "curve_north": north, "facade": "curtain", "canopy": true}
            });
            if !roof.is_null() {
                v["roof"] = roof.clone();
            }
            let (_, p) = built(&v);
            assert!(metric(&p, "curved_walls") >= 1.0, "{south}/{north}: no curved facade");
            assert!(p.roof.ring.len() > 4, "{south}/{north} {roof}: roof ring not bent");
            assert_eq!(metric(&p, "roof_skeleton") as usize, 1 + p.extra_roofs.len(), "{south}/{north} {roof}: a roof does not close");
        }
    }
}
