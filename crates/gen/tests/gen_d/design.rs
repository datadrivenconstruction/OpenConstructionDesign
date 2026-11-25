// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Spec v2 design intent: every massing builds valid, non-overlapping geometry that passes the
//! gates, a v1 brief still reads and builds as before, the example briefs build, and the
//! self-critique loop runs offline.

use ddc_gen::project::critique::{refine, LoopCfg, Renderer, RuleCritic, Snapshot};
use ddc_gen::project::{build_project, check_project, parse_spec, Design, GProject, Massing, ProjectSpec};
use serde_json::{json, Value as J};

fn built(v: &J) -> (ProjectSpec, GProject) {
    let s = parse_spec(v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{v}: {:?}", g.failures);
    (s, p)
}

fn inside(r: &[(f64, f64)], p: (f64, f64)) -> bool {
    let n = r.len();
    let mut odd = false;
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            odd = !odd;
        }
    }
    odd
}

fn area(r: &[(f64, f64)]) -> f64 {
    let n = r.len();
    ((0..n).map(|i| r[i].0 * r[(i + 1) % n].1 - r[(i + 1) % n].0 * r[i].1).sum::<f64>() / 2.0).abs()
}

/// Rooms of a storey never overlap: no room's point lies in another, their areas add up to
/// no more than the slab, and site fixtures stay off the built area.
fn assert_disjoint(p: &GProject, what: &str) {
    for (k, s) in p.storeys.iter().enumerate() {
        let l = &s.layout;
        for (i, a) in l.rooms.iter().enumerate() {
            for (j, b) in l.rooms.iter().enumerate() {
                if i != j {
                    assert!(!inside(&b.polygon, a.point), "{what} floor {k}: room {i} lies in room {j}");
                }
            }
        }
        let rooms: f64 = l.rooms.iter().map(|r| area(&r.polygon)).sum();
        let court = p.court.as_deref().map_or(0.0, area);
        assert!(
            rooms <= area(&l.slab) - court + 1e-3,
            "{what} floor {k}: rooms {rooms:.1} m2 exceed the slab {:.1} m2",
            area(&l.slab) - court
        );
        for f in l.fixtures.iter().filter(|f| matches!(f.family, "Tree" | "Paving Slab")) {
            assert!(
                !l.rooms.iter().any(|r| inside(&r.polygon, f.at)),
                "{what}: a {} stands in a room at {:?}",
                f.family,
                f.at
            );
        }
    }
}

fn office(design: J) -> J {
    json!({"typology": "office", "floors": 4, "design": design})
}

#[test]
fn a_v1_brief_reads_with_the_plain_design_and_builds_as_before() {
    for v in [
        json!({"typology": "house"}),
        json!({"typology": "apartment_section", "floors": 9, "lang": "ru"}),
        json!({"typology": "office", "floors": 3, "origin": [10.0, 5.0]}),
        json!({"typology": "school"}),
    ] {
        let (s, p) = built(&v);
        assert_eq!(s.design, Design::default(), "{v}");
        assert_eq!((p.copies, p.extra_roofs.len()), (1, 0));
        let fixtures = |f: &str| p.storeys.iter().flat_map(|s| &s.layout.fixtures).filter(|x| x.family == f).count();
        assert_eq!(fixtures("Tree") + fixtures("Facade Fin") + fixtures("Entrance Canopy"), 0, "{v}");
    }
}

#[test]
fn origin_moves_columns_and_fixtures_with_the_walls() {
    let (_, a) = built(&json!({"typology": "office", "floors": 2}));
    let (_, b) = built(&json!({"typology": "office", "floors": 2, "origin": [100.0, 50.0]}));
    let (fa, fb) = (&a.storeys[0].layout.fixtures, &b.storeys[0].layout.fixtures);
    assert_eq!(fa.len(), fb.len());
    for (x, y) in fa.iter().zip(fb) {
        assert!((y.at.0 - x.at.0 - 100.0).abs() < 1e-9 && (y.at.1 - x.at.1 - 50.0).abs() < 1e-9);
    }
}

#[test]
fn every_massing_builds_valid_disjoint_geometry() {
    for m in Massing::NAMES {
        let v = office(json!({"massing": m, "facade": "fins"}));
        let (s, p) = built(&v);
        assert_eq!(s.design.massing.name(), m);
        let wings = Massing::ALL.iter().find(|x| x.name() == m).unwrap().wings();
        assert_eq!(p.copies as usize, wings + 1, "{m}");
        // a roof per bar and per corner hall
        let corners = [0, 1, 2, 4][wings];
        assert_eq!(1 + p.extra_roofs.len(), if wings == 0 { 1 } else { wings + 1 + corners }, "{m}");
        assert_disjoint(&p, m);
        let fins: usize = p.storeys.iter().map(|s| s.layout.fixtures.iter().filter(|f| f.family == "Facade Fin").count()).sum();
        assert!(fins > 0, "{m}: no fins");
        // grid heads stand outside the building, never in a corner hall
        let s0 = &p.storeys[0].layout;
        for g in &s0.grids {
            for e in [g.a, g.b] {
                let built_in = inside(&s0.slab, e) && !p.court.as_deref().is_some_and(|c| inside(c, e));
                assert!(!built_in, "{m}: grid {} ends inside at {e:?}", g.name);
            }
        }
        eprintln!("massing {m}: footprint {:?}, rooms {}, fixtures {}", p.footprint,
            p.storeys.iter().map(|s| s.rooms.len()).sum::<usize>(),
            p.storeys.iter().map(|s| s.layout.fixtures.len()).sum::<usize>());
    }
}

#[test]
fn stepped_blocks_keep_one_stair_axis() {
    for d in [
        json!({"setback": 3.0, "upper_from": 2}),
        json!({"cantilever": 2.0, "upper_from": 1}),
        json!({"massing": "tower_podium", "terrace": true}),
    ] {
        let (_, p) = built(&office(d.clone()));
        let o: Vec<(f64, f64)> = p.storeys.iter().filter_map(|s| s.layout.stairs.first().map(|x| x.origin)).collect();
        assert!(o.windows(2).all(|w| (w[0].0 - w[1].0).abs() < 1e-9 && (w[0].1 - w[1].1).abs() < 1e-9), "{d}: {o:?}");
        let widths: Vec<f64> = p.storeys.iter().map(|s| {
            let xs = s.layout.walls.iter().flat_map(|w| [w.a.0, w.b.0]);
            let (lo, hi) = xs.fold((f64::MAX, f64::MIN), |(l, h), x| (l.min(x), h.max(x)));
            hi - lo
        }).collect();
        assert!(widths.first() != widths.last(), "{d}: the floors do not step {widths:?}");
        assert_disjoint(&p, &d.to_string());
    }
}

#[test]
fn every_facade_system_and_balcony_kind_builds() {
    for f in ["punched", "ribbon", "curtain", "panels", "fins"] {
        for b in ["none", "balcony", "loggia"] {
            let v = json!({"typology": "apartment_section", "floors": 4, "design": {"facade": f, "balconies": b}});
            let (_, p) = built(&v);
            let bal = p.storeys.iter().flat_map(|s| &s.layout.fixtures).filter(|x| x.family == "Balcony" || x.family == "Loggia").count();
            assert_eq!(bal > 0, b != "none", "{f}/{b}");
            assert!(p.storeys[0].layout.fixtures.iter().any(|x| x.family == "Entrance Canopy"), "{f}/{b}: no canopy");
        }
    }
}

#[test]
fn bad_design_fields_are_named() {
    let e = parse_spec(&json!({"typology": "office", "floors": 4,
        "design": {"massing": "zigzag", "setback": 30, "facade": 3, "colour": "red"}}))
    .unwrap_err()
    .to_string();
    for p in ["design.massing", "design.setback", "design.facade", "design.colour"] {
        assert!(e.contains(p), "{p} in {e}");
    }
    let e = parse_spec(&json!({"typology": "office", "floors": 4, "design": {"massing": "l", "setback": 2}}))
        .unwrap_err()
        .to_string();
    assert!(e.contains("not wings"), "{e}");
}

#[test]
fn example_briefs_build_and_pass_the_gates() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut n = 0;
    for name in ["house", "school", "office", "hotel", "apartment_block", "mixed_use"] {
        let text = std::fs::read_to_string(dir.join(format!("{name}.json"))).unwrap();
        let v: J = serde_json::from_str(&text).unwrap();
        let (_, p) = built(&v);
        assert_disjoint(&p, name);
        let fx: usize = p.storeys.iter().map(|s| s.layout.fixtures.len()).sum();
        eprintln!("example {name}: storeys {} footprint {:.1}x{:.1} roofs {} fixtures {fx}",
            p.storeys.len(), p.footprint.0, p.footprint.1, 1 + p.extra_roofs.len());
        n += 1;
    }
    assert_eq!(n, 6);
}

struct Blank;

impl Renderer for Blank {
    fn render(&mut self, p: &GProject) -> Result<Vec<Snapshot>, String> {
        Ok(vec![Snapshot {
            view: format!("axo:{}", p.storeys.len()),
            png: vec![],
        }])
    }
}

#[test]
fn the_critique_loop_patches_a_plain_brief_and_stops() {
    let r = refine(json!({"typology": "office", "floors": 3}), &mut Blank, &mut RuleCritic, LoopCfg::default()).unwrap();
    assert!(r.rounds.len() >= 2, "{:?}", r.rounds);
    assert!(r.score >= 8.0, "{}", r.score);
    assert_eq!(r.best["design"]["facade"], "fins");
    assert!(r.rounds.windows(2).all(|w| w[0].score <= w[1].score));
}

/// The loop hands back the build of its best round, so the draft does not build that brief a
/// second time: it is the very build of `best`.
#[test]
fn the_critique_loop_keeps_the_build_of_its_best_round() {
    let r = refine(json!({"typology": "office", "floors": 3}), &mut Blank, &mut RuleCritic, LoopCfg::default()).unwrap();
    let kept = r.built.as_ref().expect("a scored round keeps its build");
    let (s, p) = built(&r.best);
    assert_eq!(kept.spec, s);
    assert_eq!(kept.project, p);
    assert!(kept.gates.passed());
}

/// Without images and with a critic of the spec alone the rounds are not built one by one, yet
/// the loop ends where the drawing loop ends, with the same rounds, best brief and build.
#[test]
fn the_unbuilt_loop_ends_where_the_built_loop_ends() {
    use ddc_gen::project::critique::NoShots;
    for t in ["office", "hotel", "residential"] {
        let brief = json!({"typology": t, "floors": 3});
        let drawn = refine(brief.clone(), &mut Blank, &mut RuleCritic, LoopCfg::default()).unwrap();
        let plain = refine(brief, &mut NoShots, &mut RuleCritic, LoopCfg::default()).unwrap();
        assert_eq!(plain.best, drawn.best, "{t}");
        assert_eq!(plain.score, drawn.score, "{t}");
        assert_eq!(plain.rounds, drawn.rounds, "{t}");
        assert_eq!(plain.built, drawn.built, "{t}");
    }
}

/// The app's setting runs the loop without images: every template brief comes out building,
/// passing the gates, and keeping what the brief asked for.
#[test]
fn the_imageless_loop_keeps_the_brief_and_its_gates() {
    use ddc_gen::project::critique::NoShots;
    for t in ["hotel", "clinic", "warehouse", "mixed_use", "school_campus", "residential"] {
        let brief = json!({"typology": t, "floors": 3, "features": ["atrium"]});
        let r = refine(brief, &mut NoShots, &mut RuleCritic, LoopCfg::default()).unwrap();
        assert!(r.rounds.iter().all(|x| x.error.is_none()), "{t}: {:?}", r.rounds);
        assert_eq!(r.best["typology"], t);
        assert_eq!(r.best["floors"], 3);
        assert_eq!(r.best["features"], json!(["atrium"]));
        let (_, p) = built(&r.best);
        assert_eq!(p.storeys.len(), 3, "{t}");
    }
}

/// Joined and stepped blocks are ventilated like a plain bar: the gallery's L hotel and office
/// tower once came out with no ducts at all.
#[test]
fn every_massing_is_ventilated() {
    let briefs = [
        json!({"typology": "office", "name": "City Hotel", "floors": 6,
            "rooms": [{"kind": "bedroom", "area": 24, "count": 14}],
            "ground_rooms": [{"kind": "lobby", "area": 120}, {"kind": "dining", "area": 150},
                {"kind": "office", "area": 20, "count": 2}],
            "design": {"massing": "l", "facade": "ribbon", "balconies": "loggia", "trees": 10}}),
        json!({"typology": "office", "name": "Office Tower", "floors": 8,
            "design": {"massing": "tower_podium", "upper_from": 2, "setback": 4,
                "facade": "curtain", "terrace": true}}),
        json!({"typology": "school", "name": "Primary School", "floors": 3,
            "design": {"massing": "u", "facade": "panels", "rhythm": 2.4}}),
    ];
    for v in briefs {
        let (_, p) = built(&v);
        let why: Vec<_> = p.metrics.iter().filter(|m| m.0.starts_with("hvac")).collect();
        assert!(!p.hvac.ducts.is_empty() && !p.hvac.plant.is_empty(), "{}: {why:?}", v["name"]);
        assert!(why.iter().all(|m| m.0 != "hvac_unserved" || m.1 == 0.0), "{}: {why:?}", v["name"]);
    }
}

/// The atrium roofs the courtyard: the ground slab runs through as its floor, the upper slabs
/// stay open round it, and a roof closes it at the top.
#[test]
fn the_atrium_is_the_court_with_a_floor_and_a_roof() {
    let (s, p) = built(&json!({"typology": "office", "floors": 4, "design": {"atrium": true}}));
    assert_eq!(s.design.massing, Massing::Courtyard);
    assert!(s.notes.iter().any(|n| n.contains("atrium")), "{:?}", s.notes);
    let court = p.court.clone().expect("a courtyard block");
    assert!(!p.storeys[0].layout.slab_holes.contains(&court));
    assert!(p.storeys[1..].iter().all(|st| st.layout.slab_holes.contains(&court)));
    assert_eq!(p.extra_roofs.last().map(|r| &r.ring), Some(&court));
    assert!(p.metrics.iter().any(|m| m.0 == "atrium_area" && m.1 > 20.0), "{:?}", p.metrics);
    let e = parse_spec(&json!({"typology": "office", "design": {"massing": "l", "atrium": true}}))
        .unwrap_err()
        .to_string();
    assert!(e.contains("design.atrium"), "{e}");
}

#[test]
fn the_brief_check_repairs_an_atrium_into_a_roofed_court() {
    use ddc_gen::project::brief::check;
    let (s, p) = built(&json!({"typology": "office", "floors": 4, "features": ["atrium"]}));
    let fix = check(&s, &p).repair.expect("a repair");
    assert_eq!(fix, json!({"design": {"massing": "courtyard", "atrium": true}}));
    let mut v = json!({"typology": "office", "floors": 4, "features": ["atrium"]});
    ddc_gen::project::merge_patch(&mut v, &fix);
    let (s, p) = built(&v);
    let c = check(&s, &p);
    assert!(c.repair.is_none(), "{}", c.report);
    assert_eq!(c.report["features"]["approximated"][0]["feature"], "atrium", "{}", c.report);
}

/// A wall tagged exterior has open ground on one side: where wings join, the walls each wing
/// laid as its outside stand inside the block and take neither entrances nor windows.
#[test]
fn no_exterior_wall_has_building_on_both_sides() {
    let mut briefs: Vec<J> = Massing::NAMES.iter().map(|m| office(json!({"massing": m}))).collect();
    briefs.push(json!({"typology": "school_campus", "floors": 4, "design": {"massing": "courtyard"},
        "rooms": [{"kind": "classroom", "area": 60, "count": 12}, {"kind": "gym", "area": 300}]}));
    for v in briefs {
        let s = parse_spec(&v).unwrap_or_else(|e| panic!("{v}: {e}"));
        let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
        for (k, st) in p.storeys.iter().enumerate() {
            let l = &st.layout;
            // building is a room of the storey: a slab with no room on it is a terrace or a roof
            let built = |q: (f64, f64)| l.rooms.iter().any(|r| inside(&r.polygon, q));
            for (i, w) in l.walls.iter().enumerate().filter(|(_, w)| w.role == ddc_gen::WallRole::Exterior) {
                let (dx, dy) = (w.b.0 - w.a.0, w.b.1 - w.a.1);
                let n = dx.hypot(dy);
                if n < 1e-6 {
                    continue;
                }
                let m = (w.a.0 + dx / 2.0, w.a.1 + dy / 2.0);
                let (nx, ny) = (-dy / n * 0.5, dx / n * 0.5);
                assert!(
                    !(built((m.0 + nx, m.1 + ny)) && built((m.0 - nx, m.1 - ny))),
                    "{v}: storey {k} wall {i} {:?}-{:?} is exterior with building on both sides",
                    w.a,
                    w.b
                );
            }
        }
    }
}
