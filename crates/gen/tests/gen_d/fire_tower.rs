// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A 40-storey office tower (the bar plan takes depths up to massing.depth, 28 m) with a fire core: two protected stairs on every storey, rated
//! stair enclosures with rated doors, the firefighting lift, sprinklers and the curtain wall.

use ddc_gen::project::{build_project, check_project, fire, parse_spec, GProject, ProjectSpec, RoomKind};
use ddc_gen::{FireSep, WallRole};
use serde_json::{json, Value as J};

fn tower() -> (ProjectSpec, GProject) {
    tower_on(json!({"width": 60, "depth": 28}))
}

fn tower_on(footprint: J) -> (ProjectSpec, GProject) {
    tower_of(footprint, 40)
}

fn tower_of(footprint: J, floors: u32) -> (ProjectSpec, GProject) {
    let v: J = json!({
        "typology": "office",
        "floors": floors,
        "footprint": footprint,
        "lifts": 8,
        "features": ["curtain wall", "sprinklers"]
    });
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{:?}", g.failures);
    (s, p)
}

#[test]
fn every_storey_of_the_tower_has_two_protected_stairs() {
    let (s, p) = tower();
    assert_eq!(p.storeys.len(), 40);
    let tall = s.rules.get("fire.stair_rating_tall") as u32;
    for (k, st) in p.storeys.iter().enumerate() {
        let rooms = st.rooms.iter().filter(|r| r.kind == RoomKind::Stair).count();
        assert!(rooms >= 2, "storey {k}: {rooms} stair room(s)");
        assert!(st.layout.stairs.len() >= 2, "storey {k}: {} flight(s)", st.layout.stairs.len());
        let rated = st
            .layout
            .walls
            .iter()
            .filter(|w| w.fire.is_some_and(|f| f.sep == FireSep::Stair && f.minutes >= tall))
            .count();
        assert!(rated >= 6, "storey {k}: {rated} wall(s) rated for a tall building");
    }
}

#[test]
fn doors_in_the_rated_stair_walls_are_found_by_the_openings() {
    let (_, p) = tower();
    for (k, st) in p.storeys.iter().enumerate() {
        let l = &st.layout;
        let doors = l
            .openings
            .iter()
            .filter(|o| o.door && l.walls[o.wall].fire.is_some_and(|f| f.sep == FireSep::Stair))
            .count();
        assert!(doors >= 2, "storey {k}: {doors} door(s) in rated stair walls");
        assert!(l.walls.iter().any(|w| w.role != WallRole::Exterior && w.fire.is_some()));
    }
}

#[test]
fn the_tower_takes_a_firefighting_lift_and_a_curtain_wall() {
    let (s, p) = tower();
    assert_eq!(s.design.facade, ddc_gen::project::FacadeSystem::Curtain);
    assert!(p.lifts.fighting >= 1, "{:?}", p.lifts);
    assert_eq!(p.lifts.cars, 8, "the brief names 8 lifts, the bank holds 8");
    for (k, st) in p.storeys.iter().enumerate() {
        let shafts = st.layout.shafts.iter().filter(|x| x.kind == ddc_gen::ShaftKind::Lift).count();
        assert_eq!(shafts, 8, "storey {k}");
    }
    assert!(!p.fire.runs.is_empty(), "no sprinkler runs");
}

/// A brief that says "glass" or "glazed" asks for the curtain wall too, on a bar tower.
#[test]
fn a_glass_tower_brief_gets_the_curtain_wall() {
    for word in ["glass", "Glazed facade", "glass tower", "curtain"] {
        let v: J = json!({"typology": "office", "floors": 12, "footprint": {"width": 40, "depth": 20}, "features": [word]});
        let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.design.facade, ddc_gen::project::FacadeSystem::Curtain, "{word}");
        let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
        let (storeys, walls) = ddc_gen::project::brief::curtain_counts(&p);
        assert!(storeys > 0 && walls > 0, "{word}: {storeys} storeys, {walls} curtain walls");
    }
    // a named facade system wins over the keyword
    let v: J = json!({"typology": "office", "floors": 4, "footprint": {"width": 40, "depth": 20},
        "features": ["glass"], "design": {"facade": "punched"}});
    assert_ne!(parse_spec(&v).unwrap_or_else(|e| panic!("{e}")).design.facade, ddc_gen::project::FacadeSystem::Curtain);
}

/// Words that only contain a glass word (or a glazing word inside a compound) do not ask for the curtain wall.
#[test]
fn glass_inside_other_words_does_not_get_the_curtain_wall() {
    for word in ["fibreglass", "glass_partition", "double_glazing"] {
        let v: J = json!({"typology": "office", "floors": 12, "footprint": {"width": 40, "depth": 20}, "features": [word]});
        let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
        assert_ne!(s.design.facade, ddc_gen::project::FacadeSystem::Curtain, "{word}");
    }
}

/// Sprinklers come from the height rule (`fire.sprinkler_height`): every storey above it carries
/// a head grid with branches to a riser, and each room of it, bar the stairs and lift cars,
/// holds a head. The project's own fire check measures the coverage and finds no fault.
#[test]
fn every_room_on_a_tall_storey_is_under_a_sprinkler_head() {
    let (s, p) = tower();
    let h = s.rules.get("fire.sprinkler_height");
    assert!(p.fire.heads > 0 && !p.fire.runs.is_empty());
    let mut tall = 0;
    for (k, st) in p.storeys.iter().enumerate().filter(|(_, st)| st.elevation >= h) {
        tall += 1;
        let heads: Vec<(f64, f64)> =
            st.layout.fixtures.iter().filter(|f| f.family == "Sprinkler Head").map(|f| f.at).collect();
        assert!(!heads.is_empty(), "storey {k}: no heads");
        for (i, (pr, gr)) in st.rooms.iter().zip(&st.layout.rooms).enumerate() {
            if matches!(pr.kind, RoomKind::Stair | RoomKind::Lift) || gr.polygon.len() < 3 {
                continue;
            }
            let ring = vec![gr.polygon.iter().map(|q| ddc_geom::v2(q.0, q.1)).collect::<Vec<_>>()];
            assert!(
                heads.iter().any(|q| ddc_geom::poly::contains(&ring, ddc_geom::v2(q.0, q.1))),
                "storey {k} room {i} ({:?}): no head over it",
                pr.kind
            );
        }
    }
    assert!(tall >= 30, "{tall} storeys above the sprinkler height");
    assert!(ddc_gen::project::fire_check(&p).is_empty(), "{:?}", ddc_gen::project::fire_check(&p));
}

/// A 45 x 45 m plate is deeper than the bar plan takes (`massing.depth`): the core stands in the
/// middle with no corridor ends, so it holds a separated pair of stairs, each in its own
/// enclosed room with its own rated door.
#[test]
fn a_square_45_m_plate_gets_two_stairs_far_apart() {
    let (s, p) = tower_on(json!({"polygon": [[0, 0], [45, 0], [45, 45], [0, 45]]}));
    assert!(s.plate.is_some());
    let tall = s.rules.get("fire.stair_rating_tall") as u32;
    for (k, st) in p.storeys.iter().enumerate() {
        let stairs: Vec<usize> = (0..st.rooms.len()).filter(|&r| st.rooms[r].kind == RoomKind::Stair).collect();
        assert!(stairs.len() >= 2, "storey {k}: {} stair room(s)", stairs.len());
        assert!(st.layout.stairs.len() >= 2, "storey {k}: {} flight(s)", st.layout.stairs.len());
        let l = &st.layout;
        for &r in &stairs {
            // a door of its own on this room's outline, rated like the walls
            let doors = l
                .openings
                .iter()
                .filter(|o| o.door && l.walls[o.wall].fire.is_some_and(|f| f.sep == FireSep::Stair && f.minutes >= tall))
                .filter(|o| fire::centre(l, o).is_some_and(|c| fire::on_outline(&l.rooms[r].polygon, c)))
                .count();
            assert!(doors >= 1, "storey {k}, stair room {r}: {doors} rated door(s)");
        }
    }
    // every stair reaches the outside on the ground floor by a door of its own, not through the lobby
    let g = &p.storeys[0];
    for r in (0..g.rooms.len()).filter(|&r| g.rooms[r].kind == RoomKind::Stair) {
        let out = g.layout.openings.iter().any(|o| {
            o.door
                && g.layout.walls[o.wall].role == WallRole::Exterior
                && fire::centre(&g.layout, o).is_some_and(|c| fire::on_outline(&g.layout.rooms[r].polygon, c))
        });
        assert!(out, "ground floor: stair room {r} has no door to the outside");
    }
    // the stairs stand far enough apart: no storey carries an exit separation finding
    let sep = s.rules.get("egress.exit_separation");
    let m = fire::measure(&s, &p);
    for (k, f) in m.iter().enumerate().skip(1) {
        let x = f.separation.unwrap_or_else(|| panic!("storey {k}: two exits, no separation measured"));
        assert!(x >= sep, "storey {k}: exits {x:.2} of the diagonal apart, rule {sep}");
    }
    let found: Vec<_> = ddc_gen::project::nav::check(&s, &p).into_iter().filter(|f| f.rule == "egress.exit_separation").collect();
    assert!(found.is_empty(), "{} exit separation finding(s)", found.len());
    // a low plate keeps its single core stair even when wide
    let v = json!({"typology": "office", "floors": 2, "footprint": {"polygon": [[0, 0], [45, 0], [45, 45], [0, 45]]}});
    let p2 = build_project(&parse_spec(&v).unwrap_or_else(|e| panic!("{e}"))).unwrap_or_else(|e| panic!("{e}"));
    assert!(p2.storeys[0].rooms.iter().filter(|r| r.kind == RoomKind::Stair).count() == 1);
    // switched off, the plate keeps its single core stair
    let v = json!({"typology": "office", "floors": 6, "footprint": {"polygon": [[0, 0], [45, 0], [45, 45], [0, 45]]},
        "rules": {"egress.core_pair": "off"}});
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    assert!(p.storeys[0].rooms.iter().filter(|r| r.kind == RoomKind::Stair).count() == 1);
}

/// A tower grown past 40 storeys (the chat edit "add a floor") keeps its fire protection on every
/// storey: two enclosed stairs that run on up, heads, the standpipe and the curtain wall.
#[test]
fn a_tower_grown_past_40_storeys_keeps_its_fire_protection() {
    for n in [41u32, 64] {
        let (s, p) = tower_of(json!({"width": 60, "depth": 28}), n);
        assert_eq!(p.storeys.len(), n as usize, "{n} floors asked");
        let h = s.rules.get("fire.sprinkler_height");
        for (k, st) in p.storeys.iter().enumerate() {
            let stairs = st.rooms.iter().filter(|r| r.kind == RoomKind::Stair).count();
            assert!(stairs >= 2, "{n} floors, storey {k}: {stairs} stair room(s)");
            assert!(st.layout.stairs.len() >= 2, "{n} floors, storey {k}: {} flight(s)", st.layout.stairs.len());
            if st.elevation >= h {
                assert!(st.layout.fixtures.iter().any(|f| f.family == "Sprinkler Head"), "{n} floors, storey {k}: no heads");
            }
        }
        let top = p.storeys.last().unwrap();
        assert!(!top.curtains.is_empty(), "{n} floors: no curtain wall on the top storey");
        assert!(!p.fire.standpipes.is_empty(), "{n} floors: no standpipe");
        assert!(ddc_gen::project::fire_check(&p).is_empty(), "{n} floors: {:?}", ddc_gen::project::fire_check(&p));
    }
}

/// The escape stairs of a tall building run on to the roof (`fire.stair_to_roof`): the 45 m
/// square tower has its pair of flights on all 40 storeys, the top pair reaching the roof
/// through a rated head with an exit door; off, the top storey has none.
#[test]
fn the_tower_stairs_run_on_to_the_roof_through_heads() {
    let flights = |rules: J| {
        let v = json!({
            "typology": "office",
            "floors": 40,
            "footprint": {"polygon": [[0, 0], [45, 0], [45, 45], [0, 45]]},
            "lifts": 8,
            "sprinklers": true,
            "features": ["curtain wall", "sprinklers"],
            "rules": rules
        });
        let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
        let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
        let n: usize = p.storeys.iter().map(|st| st.layout.stairs.len()).sum();
        (s, p, n)
    };
    let (s, p, on) = flights(json!({}));
    assert!(fire::stairs_to_roof(&s));
    assert_eq!(on, 80, "two flights on each of the 40 storeys");
    let top = p.storeys.last().unwrap();
    assert_eq!(p.fire.stair_heads.len(), top.layout.stairs.len(), "a head over every top flight");
    let tall = s.rules.get("fire.stair_rating_tall") as u32;
    for h in &p.fire.stair_heads {
        assert_eq!(h.ring.len(), 4);
        assert_eq!(h.fire, tall);
        assert!(h.height >= 2.4, "head {} m", h.height);
        let (edge, along, width, height) = h.door;
        let (a, b) = (h.ring[edge], h.ring[(edge + 1) % 4]);
        let len = (b.0 - a.0).hypot(b.1 - a.1);
        assert!(width >= s.rules.get("egress.exit_door_min") - 1e-9, "door {width} m");
        assert!(along >= 0.0 && along + width <= len + 1e-9, "door {along}+{width} on {len}");
        assert!(height <= h.height);
        // the head stands round the flight it closes
        let o = top.layout.stairs[h.stair].origin;
        let (lo, hi) = h.ring.iter().fold(((f64::MAX, f64::MAX), (f64::MIN, f64::MIN)), |(l, u), q| {
            ((l.0.min(q.0), l.1.min(q.1)), (u.0.max(q.0), u.1.max(q.1)))
        });
        assert!(o.0 >= lo.0 - 1e-9 && o.0 <= hi.0 + 1e-9 && o.1 >= lo.1 - 1e-9 && o.1 <= hi.1 + 1e-9, "flight at {o:?} outside its head");
    }
    let (s, q, off) = flights(json!({"fire.stair_to_roof": false}));
    assert!(!fire::stairs_to_roof(&s));
    assert_eq!(off, 78, "the top storey ends the stairs");
    assert!(q.fire.stair_heads.is_empty());
    assert!(q.storeys.last().unwrap().layout.stairs.is_empty());
    assert!(q.extra_roofs.is_empty(), "no head, the roof stays whole");
    // the roof opens over every head: no roof piece covers a head, and the pieces hold the
    // whole roof but the heads
    let area = |r: &[(f64, f64)]| {
        (0..r.len()).map(|i| r[i].0 * r[(i + 1) % r.len()].1 - r[(i + 1) % r.len()].0 * r[i].1).sum::<f64>().abs() / 2.0
    };
    let inside = |r: &[(f64, f64)], c: (f64, f64)| {
        let mut odd = false;
        for i in 0..r.len() {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            if (a.1 > c.1) != (b.1 > c.1) && c.0 < a.0 + (c.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
                odd = !odd;
            }
        }
        odd
    };
    let roofs: Vec<&[(f64, f64)]> = std::iter::once(&p.roof).chain(&p.extra_roofs).map(|r| r.ring.as_slice()).collect();
    assert!(roofs.len() > 1, "the roof is parted round the heads");
    for h in &p.fire.stair_heads {
        let c = h.ring.iter().fold((0.0, 0.0), |a, x| (a.0 + x.0 / 4.0, a.1 + x.1 / 4.0));
        assert!(roofs.iter().all(|r| !inside(r, c)), "a roof piece covers the head at {c:?}");
    }
    let open: f64 = p.fire.stair_heads.iter().map(|h| area(&h.ring)).sum();
    let held: f64 = roofs.iter().map(|r| area(r)).sum();
    let whole = area(&q.roof.ring);
    assert!(held < whole - 1.0 && held >= whole - open - 0.01, "pieces {held}, heads {open}, roof {whole}");
}

/// A plate whose interior holds no core is refused with a reason about the core, never built
/// without stairs.
#[test]
fn a_plate_too_small_for_the_core_is_reported_not_built_without_stairs() {
    let v = json!({"typology": "office", "floors": 12, "footprint": {"polygon": [[0, 0], [13, 0], [13, 13], [0, 13]]}});
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    match build_project(&s) {
        Ok(p) => {
            for (k, st) in p.storeys.iter().enumerate() {
                assert!(st.rooms.iter().any(|r| r.kind == RoomKind::Stair), "storey {k}: built with no stair");
            }
        }
        Err(e) => assert!(e.to_string().to_lowercase().contains("core"), "{e}"),
    }
}

/// A stair on the facade of a plate opens outside on the ground floor, so the ground storey
/// carries an outer door for the lobby and another for that stair.
#[test]
fn a_facade_stair_on_the_square_plate_opens_outside_on_the_ground_floor() {
    let (_, p) = tower_on(json!({"polygon": [[0, 0], [45, 0], [45, 45], [0, 45]]}));
    let l = &p.storeys[0].layout;
    let outer = l.openings.iter().filter(|o| o.door && l.walls[o.wall].role == WallRole::Exterior).count();
    assert!(outer >= 2, "ground storey: {outer} outer door(s)");
}

/// A stair head whose side cannot take a door of `egress.exit_door_min` is refused with a why:
/// no head, the reason among the project's warnings.
#[test]
fn a_head_too_narrow_for_the_exit_door_is_refused_with_a_why() {
    let v = json!({
        "typology": "office",
        "floors": 40,
        "footprint": {"polygon": [[0, 0], [45, 0], [45, 45], [0, 45]]},
        "lifts": 8,
        "rules": {"fire.stair_head.door_margin": 0.5, "fire.stair_head.gap": 0.0, "egress.exit_door_min": 1.5, "egress.exit_door": 2.4}
    });
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    let top = p.storeys.last().unwrap();
    assert_eq!(p.fire.stair_heads.len() + p.fire.head_issues.len(), top.layout.stairs.len(), "{:?}", p.fire.head_issues);
    for h in &p.fire.stair_heads {
        assert!(h.door.2 >= 1.5 - 1e-9, "door {} m kept", h.door.2);
    }
    if !p.fire.head_issues.is_empty() {
        assert!(p.fire.head_issues.iter().all(|i| i.contains("egress.exit_door_min")), "{:?}", p.fire.head_issues);
        let g = check_project(&s, &p);
        assert!(g.warnings.iter().any(|w| w.contains("egress.exit_door_min")), "{:?}", g.warnings);
    }
}

/// A building counts as tall for its stairs by height too (`fire.tall_height_m`): ten storeys of
/// 3.6 m put the top floor at 32.4 m, so the stairs run on to the roof though the storey count
/// is below `fire.tall_storeys`; a higher threshold ends them on the top storey.
#[test]
fn stairs_run_on_to_the_roof_by_height_as_well_as_storeys() {
    let built = |height: f64| {
        let v = json!({"typology": "office", "floors": 10, "floor_height": 3.6,
            "rules": {"fire.tall_storeys": 100, "fire.tall_height_m": height}});
        let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
        let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
        (s, p)
    };
    let (s, p) = built(28.0);
    assert!(fire::stairs_to_roof(&s));
    let top = p.storeys.last().unwrap();
    assert!(!top.layout.stairs.is_empty(), "the top storey keeps its flights");
    assert_eq!(p.fire.stair_heads.len() + p.fire.head_issues.len(), top.layout.stairs.len());
    let (s, p) = built(40.0);
    assert!(!fire::stairs_to_roof(&s));
    assert!(p.storeys.last().unwrap().layout.stairs.is_empty());
    assert!(p.fire.stair_heads.is_empty());
}

/// A 40-storey tower under the roof `kind` ("gable", "barrel"): the stairs run on to the roof.
fn tower_roofed(kind: &str) -> (ProjectSpec, GProject) {
    let v = json!({
        "typology": "office",
        "floors": 40,
        "footprint": {"polygon": [[0, 0], [45, 0], [45, 45], [0, 45]]},
        "lifts": 8,
        "roof": {"kind": kind, "slope": 20}
    });
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    (s, p)
}

/// Under a gable or hip roof the stair head goes through it: every head is built, none lower
/// than the rule's height, the roof is parted into planar pieces (one sloped edge each, the cuts
/// vertical, no piece off the old surface) and no head is refused.
#[test]
fn a_stair_head_goes_through_gable_and_hip_roofs() {
    for kind in ["gable", "hip"] {
        let (s, p) = tower_roofed(kind);
        let top = p.storeys.last().unwrap();
        assert!(p.fire.head_issues.is_empty(), "{kind}: {:?}", p.fire.head_issues);
        assert_eq!(p.fire.stair_heads.len(), top.layout.stairs.len(), "{kind}");
        let least = s.rules.get("fire.stair_head.height");
        assert!(p.fire.stair_heads.iter().all(|h| h.height >= least - 1e-9), "{kind}");
        assert!(!p.extra_roofs.is_empty(), "{kind}: the roof is parted round the heads");
        for r in std::iter::once(&p.roof).chain(&p.extra_roofs) {
            assert_eq!(r.edges.len(), r.ring.len(), "{kind}: a slope or a vertical cut on every edge");
            assert!(r.edges.iter().any(|e| e.is_some()), "{kind}: every piece hangs on a sloped edge");
            assert!(r.edges.iter().any(|e| e.is_none()) || r.ring.len() == 3, "{kind}: a cut is a vertical edge");
            assert!(r.overhang == 0.0, "{kind}: the overhang is in the outline of the piece");
        }
        let tall = p.fire.stair_heads.iter().map(|h| h.height).fold(0.0, f64::max);
        assert!(tall > least, "{kind}: a head under the roof is taller than the flat-roof head: {tall}");
    }
}

/// The cap of a head stands above the highest point of the roof over it: under a barrel vault,
/// whose pieces keep the vault, no head is refused and the heads are no lower than the rule.
#[test]
fn a_stair_head_goes_through_a_barrel_roof() {
    let (s, p) = tower_roofed("barrel");
    let top = p.storeys.last().unwrap();
    assert!(p.fire.head_issues.is_empty(), "{:?}", p.fire.head_issues);
    assert_eq!(p.fire.stair_heads.len(), top.layout.stairs.len());
    assert!(p.roof.vault.is_some(), "the roof is a vault");
    assert!(!p.extra_roofs.is_empty(), "the vault is parted round the heads");
    assert!(p.extra_roofs.iter().all(|r| r.vault.is_some()), "every piece keeps the vault");
    let least = s.rules.get("fire.stair_head.height");
    assert!(p.fire.stair_heads.iter().all(|h| h.height >= least - 1e-9));
    let tall = p.fire.stair_heads.iter().map(|h| h.height).fold(0.0, f64::max);
    assert!(tall > least, "a head under the crown of the vault is taller than the flat-roof head: {tall}");
}

/// The overhang of a cut gable roof is the uncut one's: the pieces together reach as far out as
/// the uncut outline with its overhang, and an outer eave hangs as far below the roof level as
/// the uncut eave does, on the same plane.
#[test]
fn a_cut_gable_keeps_the_eave_overhang_of_the_uncut_roof() {
    let build = |to_roof: bool| {
        let v = json!({
            "typology": "office",
            "floors": 40,
            "footprint": {"polygon": [[0, 0], [45, 0], [45, 45], [0, 45]]},
            "lifts": 8,
            "roof": {"kind": "gable", "slope": 20},
            "rules": {"fire.stair_to_roof": if to_roof { 1.0 } else { 0.0 }}
        });
        let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
        build_project(&s).unwrap_or_else(|e| panic!("{e}"))
    };
    let (cut, whole) = (build(true), build(false));
    assert!(!cut.extra_roofs.is_empty() && whole.extra_roofs.is_empty());
    let oh = whole.roof.overhang;
    assert!(oh > 0.0, "the brief's roof has an overhang");
    let reach = |roofs: Vec<&ddc_gen::project::PRoof>, grow: f64| {
        let pts: Vec<(f64, f64)> = roofs.iter().flat_map(|r| r.ring.iter().copied()).collect();
        let f = |g: fn(&(f64, f64)) -> f64, up: bool| pts.iter().map(g).fold(if up { f64::NEG_INFINITY } else { f64::INFINITY }, |a, b| if up { a.max(b) } else { a.min(b) });
        let (x0, x1, y0, y1) = (f(|q| q.0, false), f(|q| q.0, true), f(|q| q.1, false), f(|q| q.1, true));
        [x0 - grow, x1 + grow, y0 - grow, y1 + grow]
    };
    fn all(p: &GProject) -> Vec<&ddc_gen::project::PRoof> {
        std::iter::once(&p.roof).chain(&p.extra_roofs).collect()
    }
    let (a, b) = (reach(all(&cut), 0.0), reach(all(&whole), oh));
    for (x, y) in a.iter().zip(&b) {
        assert!((x - y).abs() < 1e-6, "cut {a:?} against uncut with overhang {b:?}");
    }
    let drop = oh * 20f64.to_radians().tan();
    let low = all(&cut).iter().map(|r| r.lift).fold(f64::INFINITY, f64::min);
    assert!((low + drop).abs() < 1e-6, "outer eave hangs {low} against {}", -drop);
}

/// Eight cars stand in two facing rows of four (`core.lift_row_max`): the lift room is one
/// row long, 2.4 m and three shaft pitches, not eight in a row, so the 60 m plate holds it.
#[test]
fn eight_cars_stand_in_two_facing_rows_and_the_core_fits_60_m() {
    let (s, p) = tower();
    let l = &p.storeys[0].layout;
    let lifts: Vec<&Vec<(f64, f64)>> = l.shafts.iter().filter(|x| x.kind == ddc_gen::ShaftKind::Lift).map(|x| &x.ring).collect();
    assert_eq!(lifts.len(), 8);
    let mut rows: Vec<i64> = lifts.iter().map(|r| (r.iter().map(|q| q.1).fold(f64::MAX, f64::min) * 100.0).round() as i64).collect();
    rows.sort_unstable();
    rows.dedup();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let pitch = s.rules.get("core.lift_shaft_w") + s.rules.get("core.lift_wall");
    let row = s.rules.get("core.lift_room") + 3.0 * pitch;
    let k = p.storeys[0].rooms.iter().position(|r| r.kind == RoomKind::Lift).expect("lift room");
    let xs: Vec<f64> = l.rooms[k].polygon.iter().map(|q| q.0).collect();
    let w = xs.iter().cloned().fold(f64::MIN, f64::max) - xs.iter().cloned().fold(f64::MAX, f64::min);
    assert!(w >= row - 1e-6 && w < row + 2.0 * pitch, "lift room {w} m, one row of four {row} m");
}
