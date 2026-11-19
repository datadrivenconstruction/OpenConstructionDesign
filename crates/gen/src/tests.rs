// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use serde_json::json;

fn by_id(id: &str) -> &'static dyn Generator {
    *registry().iter().find(|g| g.id() == id).unwrap()
}

/// Bounds of every declared number, one at a time, on top of workable defaults.
fn extremes(g: &dyn Generator, base: &J) -> Vec<J> {
    let mut out = vec![base.clone()];
    for d in g.params() {
        if matches!(
            d.kind,
            ParamKind::Length | ParamKind::Area | ParamKind::Count | ParamKind::Ratio
        ) {
            for x in [d.min, d.max, (d.min + d.max) / 2.0] {
                let mut p = base.clone();
                p[d.name] = if d.kind == ParamKind::Count {
                    json!(x.round() as i64)
                } else {
                    json!(x)
                };
                out.push(p);
            }
        }
        if let ParamKind::Choice(list) = d.kind {
            for c in list {
                let mut p = base.clone();
                p[d.name] = json!(c);
                out.push(p);
            }
        }
    }
    out
}

fn program_rooms() -> J {
    json!([
        {"name": "Hall", "area": 40.0},
        {"name": "Office 1", "area": 18.5},
        {"name": "Office 2", "area": 18.5},
        {"name": "Store", "area": 6.0, "window": false},
        {"name": "Meeting", "area": 24.0},
        {"name": "WC", "area": 3.0, "window": false},
    ])
}

fn bases() -> Vec<(&'static str, J)> {
    vec![
        ("typical_floor", json!({"depth": 18.0})),
        ("area_program", json!({"rooms": program_rooms()})),
        ("column_grid", json!({})),
        ("stair", json!({})),
        ("family", json!({"height": 2.1})),
        ("steel_frame", json!({})),
    ]
}

#[test]
fn every_generator_is_covered() {
    let ids: Vec<&str> = registry().iter().map(|g| g.id()).collect();
    for (id, _) in bases() {
        assert!(ids.contains(&id), "{id}");
    }
    assert_eq!(ids.len(), bases().len());
    for g in registry() {
        assert!(!g.summary().is_empty(), "{}", g.id());
        assert_eq!(g.variants().len(), 3, "{}", g.id());
    }
}

#[test]
fn same_parameters_and_seed_give_the_same_result() {
    for (id, base) in bases() {
        let g = by_id(id);
        for p in extremes(g, &base) {
            for seed in 1..=3 {
                assert_eq!(g.build(&p, seed), g.build(&p, seed), "{id} {p}");
            }
        }
    }
}

#[test]
fn parameter_sets_give_valid_layouts_or_a_stated_refusal() {
    for (id, base) in bases() {
        let g = by_id(id);
        let sets = extremes(g, &base);
        let mut built = 0;
        for p in &sets {
            for seed in 1..=3 {
                match g.build(p, seed) {
                    Ok(GenOut::Layout(l)) => {
                        validate_layout(&l).unwrap_or_else(|e| panic!("{id} {p} seed {seed}: {e}"));
                        built += 1;
                    }
                    Ok(GenOut::Family { .. }) => built += 1,
                    Ok(GenOut::Steel { frame, .. }) => {
                        assert!(frame.validate().is_empty(), "{id} {p} seed {seed}");
                        built += 1;
                    }
                    Err(GenError::Infeasible(why)) => assert!(!why.is_empty()),
                    Err(e) => panic!("{id} {p} seed {seed}: in-range parameters refused: {e}"),
                }
            }
        }
        // refusals are for the corners of the range, not for the range itself
        assert!(
            built * 2 >= sets.len() * 3,
            "{id}: only {built} of {} built",
            sets.len() * 3
        );
    }
}

#[test]
fn apartment_counts_across_the_range_all_build() {
    let g = by_id("typical_floor");
    for n in 2..=40 {
        for core in ["end", "center"] {
            for seed in 1..=3 {
                let p = json!({"apartments": n, "core": core, "depth": 16.0});
                let GenOut::Layout(l) = g
                    .build(&p, seed)
                    .unwrap_or_else(|e| panic!("{n} {core} {seed}: {e}"))
                else {
                    panic!("not a layout");
                };
                let got = l.metrics.iter().find(|m| m.0 == "apartments").unwrap().1;
                assert_eq!(got, n as f64);
                assert_eq!(l.stairs.len(), 1);
                assert_eq!(l.shafts.len(), 2);
            }
        }
    }
}

#[test]
fn floor_rooms_belong_to_their_apartments() {
    let GenOut::Layout(l) = by_id("typical_floor")
        .build(&json!({"apartments": 6}), 1)
        .unwrap()
    else {
        panic!()
    };
    for k in 1..=6 {
        assert!(
            l.rooms.iter().filter(|r| r.apartment == Some(k)).count() >= 3,
            "{k}"
        );
    }
    assert!(l.rooms.iter().any(|r| r.apartment.is_none()));
    assert!(!l.slab_holes.is_empty());
}

#[test]
fn variants_differ_by_their_criterion() {
    for (id, base) in bases() {
        let g = by_id(id);
        let v: Vec<GenOut> = (1..=3).map(|s| g.build(&base, s).unwrap()).collect();
        assert_ne!(v[0], v[1], "{id}");
        assert_ne!(v[0], v[2], "{id}");
        assert_ne!(v[1], v[2], "{id}");
        assert_eq!(
            g.build(&base, 4).unwrap(),
            v[0],
            "{id}: a larger seed wraps around"
        );
    }
}

#[test]
fn out_of_range_parameters_are_refused() {
    let floor = by_id("typical_floor");
    for p in [
        json!({"apartments": 1}),
        json!({"apartments": 41}),
        json!({"apartments": 2.5}),
        json!({"depth": 1e9}),
        json!({"length": 5.0}),
        json!({"area": 50.0}),
        json!({"core": "nowhere"}),
        json!({"nonsense": 1}),
        json!([1, 2]),
    ] {
        assert!(
            matches!(floor.build(&p, 1), Err(GenError::Param { .. })),
            "{p}"
        );
    }
    let prog = by_id("area_program");
    for rooms in [
        json!([]),
        json!([{"name": "A", "area": 10.0}]),
        json!([{"name": "A", "area": 10.0}, {"name": "", "area": 10.0}]),
        json!([{"name": "A", "area": 10.0}, {"name": "B\nC", "area": 10.0}]),
        json!([{"name": "A", "area": 10.0}, {"name": "B", "area": 1.0}]),
        json!([{"name": "A", "area": 10.0}, {"name": "B"}]),
    ] {
        assert!(
            matches!(
                prog.build(&json!({"rooms": rooms}), 1),
                Err(GenError::Param { .. })
            ),
            "{rooms}"
        );
    }
}

#[test]
fn too_many_apartments_for_a_given_length_are_infeasible() {
    let p = json!({"apartments": 40, "length": 30.0});
    assert!(matches!(
        by_id("typical_floor").build(&p, 1),
        Err(GenError::Infeasible(_))
    ));
}

#[test]
fn a_program_gives_each_room_its_share() {
    let GenOut::Layout(l) = by_id("area_program")
        .build(&json!({"rooms": program_rooms()}), 1)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(l.rooms.len(), 7);
    for name in ["Hall", "Store", "WC"] {
        assert!(l.rooms.iter().any(|r| r.name == name), "{name}");
    }
    let dev = l
        .metrics
        .iter()
        .find(|m| m.0 == "area_deviation")
        .unwrap()
        .1;
    assert!(dev.is_finite() && dev >= 0.0);
}

#[test]
fn every_family_kind_parses_and_builds_with_a_unique_name() {
    let g = by_id("family");
    let mut names = std::collections::BTreeSet::new();
    for kind in ["table", "cabinet", "shelf", "box", "door", "window"] {
        for (w, d, h) in [(0.3, 0.2, 1.8), (6.0, 0.6, 4.0), (1.2, 0.5, 2.1)] {
            for seed in 1..=3 {
                let p = json!({"kind": kind, "width": w, "depth": d, "height": h});
                let GenOut::Family { def } = g
                    .build(&p, seed)
                    .unwrap_or_else(|e| panic!("{kind} {w}x{d}x{h}: {e}"))
                else {
                    panic!("not a family");
                };
                assert!(
                    names.insert(def.lines().next().unwrap().to_string()),
                    "{def}"
                );
                assert!(family_metrics(&def)[0].1 >= 1.0);
            }
        }
    }
}

#[test]
fn the_validator_catches_what_it_promises() {
    let wall = |a, b| GWall::line(a, b, WallRole::Exterior);
    let square = || {
        vec![
            wall((0.0, 0.0), (4.0, 0.0)),
            wall((4.0, 0.0), (4.0, 4.0)),
            wall((4.0, 4.0), (0.0, 4.0)),
            wall((0.0, 4.0), (0.0, 0.0)),
        ]
    };
    let room = |poly: Vec<(f64, f64)>| GRoom {
        name: "R".into(),
        apartment: None,
        point: (2.0, 2.0),
        polygon: poly,
    };
    let ring = vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];

    let ok = GLayout {
        walls: square(),
        rooms: vec![room(ring.clone())],
        ..GLayout::default()
    };
    validate_layout(&ok).unwrap();

    let mut crossing = ok.clone();
    crossing.walls.push(wall((-1.0, 2.0), (5.0, 2.0)));
    assert!(validate_layout(&crossing).is_err());

    let mut free = ok.clone();
    free.walls.push(wall((1.0, 0.0), (1.0, 2.0)));
    assert!(validate_layout(&free).unwrap_err().contains("free end"));

    let mut open = ok.clone();
    open.walls.remove(0);
    assert!(validate_layout(&open).is_err());

    let mut outside = ok.clone();
    outside.openings.push(GOpening {
        wall: 0,
        along: 3.5,
        door: true,
        width: 0.9,
        height: 2.1,
        sill: 0.0,
        ty: None,
    });
    assert!(validate_layout(&outside).is_err());

    let mut twin = ok.clone();
    twin.rooms.push(room(ring));
    assert!(validate_layout(&twin).unwrap_err().contains("overlap"));
}

#[test]
fn the_lattice_tower_follows_the_brief_height_footprint_and_platforms() {
    let g = by_id("steel_frame");
    let p = json!({"kind": "lattice_tower_300", "panel": 4.0, "tower_height": 200.0, "base_width": 80.0,
                   "platform_1": 40.0, "platform_2": 80.0, "platform_3": 180.0});
    let Ok(GenOut::Steel { frame, .. }) = g.build(&p, 1) else { panic!("tower refused") };
    let top = frame.nodes.iter().map(|n| n.z).fold(f64::MIN, f64::max);
    assert!((top - 200.0).abs() < 1e-6, "top {top}");
    let foot = frame.nodes.iter().filter(|n| n.z.abs() < 1e-9).map(|n| n.x.abs().max(n.y.abs())).fold(0.0, f64::max);
    assert!((foot - 40.0).abs() < 1e-6, "half footprint {foot}");
    for z in [40.0, 80.0, 180.0] {
        assert!(frame.nodes.iter().any(|n| (n.z - z).abs() < 1e-6), "no platform at {z}");
    }
    let bad = json!({"kind": "lattice_tower_300", "tower_height": 200.0, "platform_3": 250.0});
    assert!(g.build(&bad, 1).is_err(), "a platform above the top is refused");
}

#[test]
fn power_line_counts_only_its_own_strings() {
    use ddc_geom::steel::lattice::{Frame, InsulatorString};
    use ddc_geom::steel::tower::TowerParams;
    let rs = crate::project::rules::RuleSet::new();
    let mut f = Frame::new();
    f.insulators = vec![InsulatorString { at: (0.0, 0.0), z: 10.0, length: 1.0 }; 5];
    let m = steel::power_line(&mut f, &TowerParams::default(), &rs).unwrap();
    let n = m.iter().find(|x| x.0 == "insulator_strings").unwrap().1;
    assert_eq!(n as usize, f.insulators.len() - 5, "{n} of {}", f.insulators.len());
    assert!(n > 0.0);
}

#[test]
fn a_bare_lattice_ignores_platforms_and_names_the_failing_param() {
    let g = by_id("steel_frame");
    let pylon = |extra: serde_json::Value| {
        let mut p = json!({"kind": "lattice_tower_300", "service": "power_line", "tower_height": 45, "base_width": 8});
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        g.build(&p, 1)
    };
    // platforms above the top are no fault of a tower without platforms
    assert!(pylon(json!({"platform_2": 400.0, "platform_3": 390.0})).is_ok());
    let tp = ddc_geom::steel::tower::TowerParams { loads: None, ..Default::default() };
    assert_eq!(tp.shaft_fault(), None, "a bare lattice needs no loads");
    let loads = ddc_geom::steel::tower_size::Loads { f_allow_mpa: 0.0, ..Default::default() };
    let tp = ddc_geom::steel::tower::TowerParams { loads: Some(loads), ..Default::default() };
    assert_eq!(tp.shaft_fault().map(|f| f.0), Some("loads"));
    // a bad load rule is the rules' fault, not the height's
    let e = pylon(json!({"rules": {"steel.check.f_allow": 0.0}})).unwrap_err().to_string();
    assert!(e.contains("parameter rules") && !e.contains("tower_height"), "{e}");
    let tp = ddc_geom::steel::tower::TowerParams { panel_leg: 0.1, ..Default::default() };
    assert_eq!(tp.shaft_fault().map(|f| f.0), Some("panel"));
}

#[test]
fn tower_sizing_defaults_are_the_rule_defaults() {
    use ddc_geom::steel::tower_size::Loads;
    assert_eq!(steel::loads(&crate::project::rules::RuleSet::new()), Loads::default());
    // the brief's rule overrides reach the sizing: a stronger wind gives a larger sway
    let g = by_id("steel_frame");
    let sway = |q: f64| {
        let p = json!({"kind": "lattice_tower_300", "panel": 4.0, "rules": {"steel.check.wind_q": q}});
        let Ok(GenOut::Steel { metrics, .. }) = g.build(&p, 1) else { panic!("tower refused") };
        metrics.iter().find(|m| m.0 == "sway_top_mm").expect("sway metric").1
    };
    assert!(sway(3000.0) > sway(1000.0));
}
