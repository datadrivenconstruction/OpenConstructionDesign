// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The truss hall built as a hangar (scenario H16): the generator itself gives the roof
//! cladding, the sliding door in the gable on its track beam, the crane runway on corbels and a
//! two-storey annex of rooms; a plain hall stays the frame alone. Every member end meets another
//! member.

use ddc_gen::hangar::HallOutfit;
use ddc_gen::{registry, GenOut, Generator};
use ddc_geom::steel::lattice::{Frame, MemRole};
use serde_json::{json, Value as J};

fn gen() -> &'static dyn Generator {
    *registry().iter().find(|g| g.id() == "steel_frame").unwrap()
}

/// H16: 80 m clear, 100 m long, 20 m clear height.
fn h16() -> J {
    json!({"kind": "truss_hall", "span": 80, "height": 20, "bays": 10, "spacing": 10, "outfit": "hangar"})
}

fn build(p: &J) -> (Frame, Vec<(&'static str, f64)>, Option<Box<HallOutfit>>) {
    match gen().build(p, 1).unwrap_or_else(|e| panic!("{p}: {e:?}")) {
        GenOut::Steel { frame, metrics, hall, .. } => (*frame, metrics, hall),
        _ => panic!("not steel"),
    }
}

fn metric(m: &[(&'static str, f64)], k: &str) -> f64 {
    m.iter().find(|x| x.0 == k).unwrap_or_else(|| panic!("no metric {k}")).1
}

#[test]
fn the_hangar_roof_covers_the_top_chords() {
    let (f, m, hall) = build(&h16());
    let h = hall.expect("a hangar outfit");
    let top = f.nodes.iter().map(|p| p.z).fold(f64::MIN, f64::max);
    assert!((h.roof_z - top).abs() < 1e-9 && top > 20.0, "roof at {} on chords at {top}", h.roof_z);
    assert_eq!(h.roof, vec![(0.0, 0.0), (80.0, 0.0), (80.0, 100.0), (0.0, 100.0)]);
    assert!((h.overhang - 0.3).abs() < 1e-9, "the rule's default");
    assert!(metric(&m, "roof_area_m2") > 8000.0);
    let (_, _, plain) = build(&json!({"kind": "truss_hall", "span": 80, "height": 20, "bays": 10, "spacing": 10}));
    assert!(plain.is_none(), "a plain hall is the frame alone");
    let mut p = h16();
    p["rules"] = json!({"steel.hangar.roof_overhang": 1.0});
    assert!((build(&p).2.unwrap().overhang - 1.0).abs() < 1e-9, "the overhang is a rule");
}

/// Member ends no other member shares, above the lowest point of the frame.
fn free_ends(f: &Frame) -> usize {
    let mut deg = vec![0usize; f.nodes.len()];
    for m in &f.members {
        deg[m.a as usize] += 1;
        deg[m.b as usize] += 1;
    }
    let low = f.nodes.iter().map(|p| p.z).fold(f64::MAX, f64::min);
    (0..f.nodes.len()).filter(|&i| deg[i] == 1 && f.nodes[i].z > low + 1e-6).count()
}

#[test]
fn the_gable_door_hangs_from_its_track_beam() {
    let (f, m, hall) = build(&h16());
    let g = hall.unwrap().gable;
    assert_eq!(g.walls.len(), 1);
    assert_eq!((g.walls[0].a, g.walls[0].b), ((0.0, 0.0), (80.0, 0.0)));
    assert_eq!(g.openings.len(), 1);
    let d = &g.openings[0];
    assert!(d.door && (d.width - 76.0).abs() < 1e-9 && (d.height - 18.5).abs() < 1e-9, "{d:?}");
    assert!(d.along + d.width <= 80.0 && d.height <= g.height);
    assert!(metric(&m, "door_track_beams") >= 4.0);
    let track: Vec<_> = f
        .members
        .iter()
        .filter(|x| x.role == MemRole::Beam)
        .filter(|x| [x.a, x.b].iter().all(|&i| f.nodes[i as usize].y.abs() < 1e-9 && (f.nodes[i as usize].z - 18.5).abs() < 1e-9))
        .collect();
    assert_eq!(track.len() as f64, metric(&m, "door_track_beams"));
    assert_eq!(free_ends(&f), 0);
    let cols: Vec<_> = f.members.iter().filter(|x| x.role == MemRole::Column).collect();
    let mut xs: Vec<f64> = cols.iter().map(|c| f.nodes[c.a as usize].x).collect();
    xs.sort_by(f64::total_cmp);
    xs.dedup();
    assert_eq!(xs, vec![0.0, 80.0], "the span stays clear");
    let mut p = h16();
    p["rules"] = json!({"steel.hangar.door_jamb": 4.0, "steel.hangar.track_drop": 3.0});
    let d = build(&p).2.unwrap().gable.openings[0].clone();
    assert!((d.width - 72.0).abs() < 1e-9 && (d.height - 17.0).abs() < 1e-9, "the door is sized by rules");
    p["rules"] = json!({"steel.hangar.door_jamb": 10.0});
    p["span"] = json!(20);
    assert!(gen().build(&p, 1).is_err(), "no room for a door");
}

#[test]
fn the_crane_runway_stands_on_corbels_along_both_column_lines() {
    let (f, m, _) = build(&h16());
    assert!((metric(&m, "crane_rail_z") - 16.0).abs() < 1e-9);
    assert_eq!(metric(&m, "crane_corbels"), 22.0, "one per column, 11 frames a side");
    assert_eq!(metric(&m, "crane_runway_beams"), 20.0, "one per bay a side");
    let at = |x: f64| {
        f.members
            .iter()
            .filter(|b| b.role == MemRole::Beam)
            .filter(|b| [b.a, b.b].iter().all(|&i| (f.nodes[i as usize].x - x).abs() < 1e-9 && (f.nodes[i as usize].z - 16.0).abs() < 1e-9))
            .count()
    };
    assert_eq!((at(0.75), at(79.25)), (10, 10), "runway beams at the corbel reach");
    assert_eq!(free_ends(&f), 0, "every corbel meets a column node and a runway joint");
    // the H16 oracle: trusses, bars, beams and braces >= 300
    assert!(f.members.len() >= 300, "{}", f.members.len());
    let mut p = h16();
    p["rules"] = json!({"steel.hangar.crane_headroom": 6.0, "steel.hangar.corbel_reach": 1.0});
    let (_, m, _) = build(&p);
    assert!((metric(&m, "crane_rail_z") - 14.0).abs() < 1e-9, "the runway height is a rule");
    p["rules"] = json!({"steel.hangar.corbel_reach": 2.5});
    assert!(gen().build(&p, 1).is_err(), "a runway in the door is refused");
}

#[test]
fn the_annex_holds_workshops_below_offices_above_and_a_stair() {
    let (_, m, hall) = build(&h16());
    let a = hall.unwrap().annex;
    assert_eq!(a.len(), 2, "two storeys");
    assert_eq!(metric(&m, "annex_storeys"), 2.0);
    let (ground, upper) = (&a[0], &a[1]);
    assert_eq!(ground.rooms.len(), 13, "80 m cut into rooms about 6 m wide");
    assert!(ground.rooms.iter().skip(1).all(|r| r.name == "Workshop"));
    assert!(upper.rooms.iter().skip(1).all(|r| r.name == "Office"));
    assert_eq!(metric(&m, "annex_rooms"), 26.0);
    for st in &a {
        assert!((st.height - 3.6).abs() < 1e-9);
        assert!(st.walls.iter().all(|w| w.a.1 >= 100.0 - 1e-9 && w.b.1 <= 108.0 + 1e-9), "behind the far gable");
        for r in &st.rooms {
            assert!(r.point.1 > 100.0 && r.point.1 < 108.0);
        }
        for o in &st.openings {
            let w = &st.walls[o.wall];
            let len = ((w.b.0 - w.a.0).powi(2) + (w.b.1 - w.a.1).powi(2)).sqrt();
            assert!(o.door && o.along >= 0.0 && o.along + o.width <= len + 1e-9 && o.height <= st.height);
        }
    }
    assert_eq!(ground.openings.iter().filter(|o| o.wall == 0).count(), 13, "every workshop opens to the hall");
    assert_eq!(upper.openings.len(), 12, "the offices open one into the next");
    assert_eq!(ground.stairs.len(), 1);
    assert!(upper.stairs.is_empty() && upper.slab.len() == 4 && upper.slab_holes.len() == 1, "the upper floor has the stair well");
    let mut p = h16();
    p["rules"] = json!({"steel.hangar.annex_storeys": 3, "steel.hangar.annex_room": 10.0});
    let a = build(&p).2.unwrap().annex;
    assert_eq!((a.len(), a[0].rooms.len()), (3, 8), "storeys and room width are rules");
    assert_eq!(a.iter().filter(|s| !s.stairs.is_empty()).count(), 2);
}

#[test]
fn overhead_cranes_run_on_the_runway_by_rule() {
    let (_, m, hall) = build(&h16());
    let c = hall.unwrap().cranes;
    assert_eq!(c.len(), 1, "one crane by default");
    assert!((c[0].span - 78.5).abs() < 1e-9 && (c[0].capacity - 10.0).abs() < 1e-9, "{c:?}");
    assert!((c[0].at.0 - 40.0).abs() < 1e-9 && (c[0].at.1 - 50.0).abs() < 1e-9);
    assert!((c[0].at.2 - 16.25).abs() < 1e-9, "on the rail top: runway axis 16 m + half an HEB 500");
    assert_eq!((metric(&m, "cranes"), metric(&m, "crane_capacity_t")), (1.0, 10.0));
    let mut p = h16();
    p["rules"] = json!({"steel.hangar.crane.count": 2, "steel.hangar.crane.capacity": 20});
    let c = build(&p).2.unwrap().cranes;
    assert_eq!(c.iter().map(|c| c.at.1).collect::<Vec<_>>(), vec![25.0, 75.0]);
    assert!(c.iter().all(|c| c.capacity == 20.0));
    p["rules"] = json!({"steel.hangar.crane.count": 0});
    assert!(build(&p).2.unwrap().cranes.is_empty(), "a hangar without a crane");
    let lib = ddc_model::library().unwrap();
    let fam = lib.iter().find(|f| f.name == ddc_gen::hangar::CRANE_FAMILY).expect("the crane family ships");
    let vals = vec![("Span".to_string(), ddc_model::ParamValue::Num(78.5)), ("Capacity".to_string(), ddc_model::ParamValue::Num(10.0))];
    let r = fam.resolve(&vals, &[]).unwrap();
    let b = fam.build(&r, ddc_model::Lod::Fine).unwrap();
    assert!(b.volume > 0.0, "the bridge of a 78.5 m crane builds");
}
