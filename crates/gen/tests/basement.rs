//! Basements: parking storeys under the building, the stairs and lifts running down.

use ddc_gen::project::{brief, build_project, check_project, parse_spec, GProject, ProjectSpec, RoomKind};
use serde_json::{json, Value as J};

fn built(v: &J) -> (ProjectSpec, GProject) {
    let s = parse_spec(v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{v}: {:?}", g.failures);
    (s, p)
}

fn metric(p: &GProject, k: &str) -> f64 {
    p.metrics.iter().find(|m| m.0 == k).map_or(f64::NAN, |m| m.1)
}

#[test]
fn basements_hold_parking_under_the_stairs_and_lifts() {
    for t in ["office", "apartment_section", "hotel", "clinic"] {
        let (s, p) = built(&json!({"typology": t, "floors": 5, "basements": 2}));
        assert_eq!(p.basements.len(), 2, "{t}");
        let up = p.storeys[0].layout.stairs.len();
        for (b, st) in p.basements.iter().enumerate() {
            assert!((st.elevation + (b + 1) as f64 * s.floor_height).abs() < 1e-9, "{t}");
            assert!(st.rooms.iter().any(|r| r.kind == RoomKind::Parking), "{t} b{b}");
            assert!(st.stair_room.is_some(), "{t} b{b}");
            assert_eq!(st.layout.stairs.len(), up, "{t}: every stair goes down");
            assert_eq!(st.layout.shafts.len(), p.storeys[0].layout.shafts.len(), "{t}");
            assert!(st.layout.openings.iter().all(|o| o.door), "{t}: no windows below ground");
            // the lower basement's stair comes up through the slab of the upper one
            let want = if b + 1 < p.basements.len() { up } else { 0 };
            assert_eq!(st.layout.slab_holes.len(), want, "{t} b{b}");
        }
        assert!(metric(&p, "parking_bays") > 0.0, "{t}");
        assert_eq!(metric(&p, "basements"), 2.0);
    }
}

#[test]
fn the_ground_slab_opens_for_the_stair_from_below() {
    let (_, plain) = built(&json!({"typology": "office", "floors": 3}));
    let (_, p) = built(&json!({"typology": "office", "floors": 3, "basements": 1}));
    let holes = |p: &GProject| p.storeys[0].layout.slab_holes.len();
    assert_eq!(holes(&p), holes(&plain) + p.storeys[0].layout.stairs.len());
    // the storeys above are the same building
    assert_eq!(p.storeys[1], plain.storeys[1]);
    assert_eq!(p.basements[0].layout.slab_holes.len(), 0);
}

#[test]
fn the_origin_moves_the_basements_too() {
    let (_, a) = built(&json!({"typology": "office", "floors": 3, "basements": 1}));
    let (_, b) = built(&json!({"typology": "office", "floors": 3, "basements": 1, "origin": [10.0, 5.0]}));
    let (wa, wb) = (&a.basements[0].layout.walls[0], &b.basements[0].layout.walls[0]);
    assert!((wb.a.0 - wa.a.0 - 10.0).abs() < 1e-9 && (wb.a.1 - wa.a.1 - 5.0).abs() < 1e-9);
}

#[test]
fn a_basement_without_a_stair_or_past_the_cap_is_refused_with_the_fix() {
    let s = parse_spec(&json!({"typology": "office", "floors": 1, "basements": 1})).unwrap();
    let e = build_project(&s).unwrap_err().to_string();
    assert!(e.contains("floors to 2"), "{e}");
    let e = parse_spec(&json!({"typology": "office", "basements": 4})).unwrap_err().to_string();
    assert!(e.contains("basements"), "{e}");
}

#[test]
fn the_brief_check_asks_for_basements_and_names_the_ramp() {
    let (s, p) = built(&json!({"typology": "office", "floors": 4, "features": ["underground_parking"]}));
    let c = brief::check(&s, &p);
    assert_eq!(c.repair, Some(json!({"basements": 1})), "{}", c.report);
    let (s, p) = built(&json!({"typology": "office", "floors": 4, "basements": 1,
                               "features": ["underground_parking", "basement"]}));
    let c = brief::check(&s, &p);
    assert!(c.repair.is_none(), "{}", c.report);
    assert!(c.report.to_string().contains("ramp"), "{}", c.report);
}
