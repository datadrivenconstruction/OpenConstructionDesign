// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The lift family: a car at the ground stop, a landing door per storey and shaft, a pit under
//! the ground stop, headroom and machine over the top stop; the count follows the waiting
//! interval; the shaft always holds the car.

use ddc_gen::project::lift::{self, band_for, class_for, envelope};
use ddc_gen::project::{build_project, parse_spec, GProject, ProjectSpec};
use serde_json::{json, Value as J};

fn built(v: J) -> (ProjectSpec, GProject) {
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    (s, p)
}

fn office(floors: u32, extra: J) -> J {
    let mut v = json!({"typology": "office", "floors": floors});
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

fn count(p: &GProject, k: usize, family: &str) -> usize {
    p.storeys[k].layout.fixtures.iter().filter(|f| f.family == family).count()
}

#[test]
fn every_shaft_gets_car_pit_doors_and_head() {
    let (_, p) = built(office(14, json!({"lifts": 3})));
    let n = p.lifts.cars as usize;
    assert_eq!(n, 3);
    let top = p.storeys.len() - 1;
    assert_eq!(count(&p, 0, "Lift Car"), n);
    assert_eq!(count(&p, 0, "Lift Pit"), n);
    assert_eq!(count(&p, 1, "Lift Car"), 0);
    for k in 0..=top {
        assert_eq!(count(&p, k, "Lift Landing Door"), n, "storey {k}");
    }
    assert_eq!(count(&p, top, "Lift Overrun"), n);
    assert_eq!(count(&p, top, "Lift Machine"), n);
    // the pit lies below the ground stop, the machine hangs in the headroom
    let pit = p.storeys[0].layout.fixtures.iter().find(|f| f.family == "Lift Pit").unwrap();
    assert!(pit.z <= -1.0, "{}", pit.z);
    let b = band_for(ddc_gen::project::rules::rule("core.lift_speed"));
    assert!((pit.z + b.pit).abs() < 1e-9);
    let m = p.storeys[top].layout.fixtures.iter().find(|f| f.family == "Lift Machine").unwrap();
    assert!(m.z > 0.0 && m.z < b.head, "{}", m.z);
}

#[test]
fn the_shaft_holds_the_car_with_its_clearances() {
    let (sp, p) = built(office(14, json!({"lifts": 2})));
    lift::check(&sp, &p).unwrap();
    let s = lift::summary(lift::serve_height(&sp));
    let c = class_for(ddc_gen::project::rules::rule("core.lift_capacity") as u32);
    assert!(s.persons >= ddc_gen::project::rules::rule("core.lift_capacity") as u32);
    let (w, d) = envelope(c);
    assert!(w > c.car.0 && d > c.car.1);
    // each ring is no smaller than the envelope in both extents
    for sh in p.storeys[0].layout.shafts.iter().take(2) {
        let xs: Vec<f64> = sh.ring.iter().map(|q| q.0).collect();
        let ys: Vec<f64> = sh.ring.iter().map(|q| q.1).collect();
        let ex = xs.iter().cloned().fold(f64::MIN, f64::max) - xs.iter().cloned().fold(f64::MAX, f64::min);
        let ey = ys.iter().cloned().fold(f64::MIN, f64::max) - ys.iter().cloned().fold(f64::MAX, f64::min);
        assert!(ex.min(ey) >= w.min(d) - 1e-6 && ex.max(ey) >= w.max(d) - 1e-6, "{ex} x {ey} vs {w} x {d}");
    }
}

#[test]
fn a_bigger_car_makes_a_bigger_shaft() {
    let ring = |cap: u32| {
        let (sp, p) = built(office(6, json!({"lifts": 1, "rules": {"core.lift_capacity": cap}})));
        lift::check(&sp, &p).unwrap();
        let r = &p.storeys[0].layout.shafts[0].ring;
        let ys: Vec<f64> = r.iter().map(|q| q.1).collect();
        ys.iter().cloned().fold(f64::MIN, f64::max) - ys.iter().cloned().fold(f64::MAX, f64::min)
    };
    assert!(ring(21) > ring(8) + 0.3, "{} {}", ring(21), ring(8));
}

#[test]
fn a_shorter_waiting_interval_asks_for_more_cars() {
    let wanted = |interval: f64| {
        let (_, p) = built(office(30, json!({"footprint": {"width": 40.0}, "lift": true, "rules": {"core.lift_interval": interval}})));
        p.lifts.wanted
    };
    assert!(wanted(15.0) > wanted(90.0), "{} {}", wanted(15.0), wanted(90.0));
    let (_, p) = built(office(30, json!({"footprint": {"width": 40.0}, "lift": true})));
    assert!(p.lifts.interval > 0.0 && p.lifts.persons >= 1, "{:?}", p.lifts);
}

#[test]
fn the_lift_family_explains_itself() {
    use ddc_gen::project::why::explain_project;
    let (s, p) = built(office(30, json!({"footprint": {"width": 40.0}, "lift": true})));
    let w = explain_project(&s, &p);
    let e = w.iter().find(|w| w.key == "lift_family").expect("lift_family");
    assert!(e.effect.contains("landing door") && e.effect.contains("pit"), "{}", e.effect);
    let (s, p) = built(office(6, json!({"lifts": 2})));
    assert!(explain_project(&s, &p).iter().any(|w| w.key == "lift_family_n"));
}

#[test]
fn the_catalogue_types_exist_in_the_library() {
    let lib = ddc_model::library().unwrap();
    let ty = |fam: &str, name: &str| {
        let f = lib.iter().find(|f| f.name == fam).unwrap_or_else(|| panic!("family {fam}"));
        assert!(f.types.iter().any(|t| t.name == name), "{fam} has no type {name}");
    };
    for (id, speed) in [("630", 0.8), ("1000", 1.6), ("1600", 4.0)] {
        let b = band_for(speed);
        ty("Lift Car", &format!("{id} kg"));
        ty("Lift Pit", &format!("{id} kg {}", b.id));
        ty("Lift Overrun", &format!("{id} kg {}", b.id));
        ty("Lift Machine", &b.id);
    }
    for w in ["800", "900", "1100", "1200"] {
        ty("Lift Landing Door", w);
    }
}

/// The numbers of the library types equal the catalogue they were written from.
#[test]
fn the_library_types_agree_with_the_catalogue() {
    let src = ddc_model::library_sources();
    let text = |fam: &str| src.iter().find(|s| s.lines().any(|l| l.trim() == format!("family {fam}"))).copied().unwrap();
    let mm = |text: &str, ty: &str, key: &str| -> f64 {
        let line = text.lines().find(|l| l.starts_with(&format!("type \"{ty}\" "))).unwrap_or_else(|| panic!("type {ty}"));
        let v = line.split(&format!("{key}=")).nth(1).unwrap_or_else(|| panic!("{ty} has no {key}"));
        v.split(|c| c == ',' || c == ' ').next().unwrap().trim_end_matches("mm").parse::<f64>().unwrap() / 1000.0
    };
    let (car, pit, over, mach) = (text("Lift Car"), text("Lift Pit"), text("Lift Overrun"), text("Lift Machine"));
    for id in ["630", "800", "1000", "1275", "1600"] {
        let persons = match id { "630" => 8, "800" => 10, "1000" => 13, "1275" => 17, _ => 21 };
        let c = class_for(persons);
        assert_eq!(c.id, id);
        assert!((mm(&car, &format!("{id} kg"), "Width") - c.car.0).abs() < 1e-9);
        assert!((mm(&car, &format!("{id} kg"), "Depth") - c.car.1).abs() < 1e-9);
        let (ew, ed) = envelope(c);
        for (speed, b) in [(0.8, band_for(0.8)), (1.6, band_for(1.6)), (4.0, band_for(4.0))] {
            let _ = speed;
            let t = format!("{id} kg {}", b.id);
            assert!((mm(&pit, &t, "Width") - ew).abs() < 1e-9 && (mm(&pit, &t, "Depth") - ed).abs() < 1e-9, "{t}");
            assert!((mm(&pit, &t, "Pit") - b.pit).abs() < 1e-9, "{t}");
            assert!((mm(&over, &t, "Head") - b.head).abs() < 1e-9, "{t}");
            assert!((mm(&over, &t, "Width") - ew).abs() < 1e-9, "{t}");
        }
    }
    for speed in [0.8, 1.6, 4.0] {
        let b = band_for(speed);
        assert!((mm(&mach, &b.id, "Width") - b.machine.0).abs() < 1e-9, "{}", b.id);
        assert!((mm(&mach, &b.id, "Height") - b.machine.2).abs() < 1e-9, "{}", b.id);
    }
}

#[test]
fn lifts_keep_their_issues_apart_from_the_fire_issues() {
    let (sp, p) = built(office(14, json!({"lifts": 3})));
    assert!(p.fire.issues.is_empty(), "{:?}", p.fire.issues);
    assert!(p.lifts.issues.is_empty(), "{:?}", p.lifts.issues);
    let g = ddc_gen::project::check_project(&sp, &p);
    assert!(g.failures.is_empty(), "{:?}", g.failures);
}

#[test]
fn basements_get_doors_and_the_pit_at_the_lowest_stop() {
    let (sp, p) = built(office(8, json!({"lifts": 2, "basements": 2})));
    assert_eq!(p.basements.len(), 2);
    let bank = |k: usize| p.basements[k].layout.shafts.iter().filter(|s| s.kind == ddc_gen::ShaftKind::Lift).count();
    assert_eq!((bank(0), bank(1)), (2, 2));
    for b in &p.basements {
        let n = b.layout.fixtures.iter().filter(|f| f.family == "Lift Landing Door").count();
        assert_eq!(n, 2);
    }
    let pits = |l: &ddc_gen::GLayout| l.fixtures.iter().filter(|f| f.family == "Lift Pit").count();
    assert_eq!((pits(&p.basements[0].layout), pits(&p.basements[1].layout), pits(&p.storeys[0].layout)), (0, 2, 0));
    lift::check(&sp, &p).unwrap();
}

#[test]
fn a_cut_bank_and_an_oversized_car_are_reported() {
    let (_, p) = built(office(40, json!({"footprint": {"width": 60.0}, "rules": {"core.lift_bank_max": 2}})));
    assert_eq!(p.lifts.cars, 2);
    assert!(p.lifts.wanted > 2);
    assert!(p.lifts.issues.iter().any(|i| i.contains("cut to 2")), "{:?}", p.lifts.issues);
    let (_, p) = built(office(6, json!({"lifts": 1, "rules": {"core.lift_capacity": 30}})));
    assert!(p.lifts.issues.iter().any(|i| i.contains("largest car")), "{:?}", p.lifts.issues);
    assert!(lift::exceeds(30) && !lift::exceeds(21));
}

#[test]
fn a_tall_building_takes_a_firefighting_class() {
    let (sp, low) = built(office(6, json!({"lifts": 2, "rules": {"core.lift_capacity": 8}})));
    assert_eq!(low.lifts.fighting, 0);
    assert_eq!(lift::bank_class(lift::serve_height(&sp)).id, "630");
    let (sp, tall) = built(office(14, json!({"lifts": 2, "rules": {"core.lift_capacity": 8}})));
    assert_eq!(tall.lifts.fighting, 1);
    assert!(lift::bank_class(lift::serve_height(&sp)).load >= 1000.0);
    let (_, one) = built(office(14, json!({"lifts": 1})));
    assert!(one.lifts.issues.iter().any(|i| i.contains("firefighting")), "{:?}", one.lifts.issues);
}

#[test]
fn service_shafts_are_not_lift_shafts() {
    let (_, p) = built(office(14, json!({"lifts": 2})));
    let lifts = p.storeys[0].layout.shafts.iter().filter(|s| s.kind == ddc_gen::ShaftKind::Lift).count();
    assert_eq!(lifts, 2);
}
