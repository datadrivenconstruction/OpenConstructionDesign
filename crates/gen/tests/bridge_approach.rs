//! The arch footbridge with what H10 asks of it: guard rails on both deck edges at the rule
//! spacing and height, built into the model as railing elements; counts of the H10 oracle.

use ddc_gen::steel::build_frame;
use ddc_model::Model;
use serde_json::json;

fn metric(m: &[(&'static str, f64)], k: &str) -> f64 {
    m.iter().find(|x| x.0 == k).unwrap_or_else(|| panic!("no metric {k}")).1
}

#[test]
fn the_footbridge_has_a_guard_on_each_deck_edge() {
    let (f, m) = build_frame(&json!({"kind": "arch_bridge", "span": 90.0, "rise": 15.0, "deck_z": 6.0, "width": 4.0}), 0).unwrap();
    assert_eq!(f.guards.len(), 2 + 4, "two deck edges, two edges of each of the two ramps");
    let ys: Vec<f64> = f.guards.iter().map(|g| g.path[0].1).collect();
    assert!((ys[0] - 0.3).abs() < 1e-9 && (ys[1] - 3.7).abs() < 1e-9, "{ys:?}");
    for g in &f.guards {
        assert_eq!((g.height, g.post_spacing), (1.1, 1.2));
    }
    assert_eq!((f.guards[0].path[0].0, f.guards[0].path[1].0), (0.0, 90.0));
    let none = build_frame(&json!({"kind": "arch_bridge", "span": 90.0, "deck_z": 6.0, "width": 4.0, "approach": "none"}), 0).unwrap();
    assert_eq!(none.0.guards.len(), 2);
    assert_eq!(metric(&none.1, "guard_posts"), 2.0 * ((90.0f64 / 1.2).ceil() + 1.0));
    assert!(metric(&m, "guard_posts") > metric(&none.1, "guard_posts"));
    // a rule override moves them: 1.4 m for cyclists, posts every 1.0 m
    let (f, _) = build_frame(&json!({"kind": "arch_bridge", "span": 90.0, "width": 4.0, "rules": {"steel.bridge.rail_height": 1.4, "steel.bridge.rail_post_spacing": 1.0}}), 0).unwrap();
    assert_eq!((f.guards[0].height, f.guards[0].post_spacing), (1.4, 1.0));
    assert!(f.guards.iter().all(|g| g.height == 1.4));
    // other frames carry none
    assert!(build_frame(&json!({"kind": "truss_hall", "span": 80.0, "height": 20.0}), 0).unwrap().0.guards.is_empty());
}

#[test]
fn h10_counts_stand_in_the_model() {
    let (f, _) = build_frame(&json!({"kind": "arch_bridge", "span": 90.0, "rise": 15.0, "deck_z": 6.0, "width": 4.0}), 0).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L0", 0.0).unwrap();
    let ids = m.add_steel_frame(lv, &f).unwrap();
    assert!(ids.beams.len() + ids.braces.len() >= 80, "members");
    assert!(ids.braces.len() >= 30, "hangers");
    assert!(ids.columns.len() + ids.footings.len() >= 4, "supports");
    assert_eq!(m.railings(lv).unwrap().len(), ids.railings.len());
    assert!(ids.railings.len() >= 2, "railings");
    assert_eq!(m.ramps(lv).unwrap().len(), 2);
    assert_eq!(ids.railings.len(), 6);
    assert!(m.stairs(lv).unwrap().is_empty());
}

/// The ramps climb at most the rule slope, with a landing after every rule rise, and meet the
/// deck at both ends; their guards follow the flights and stay flat on the landings.
#[test]
fn the_approach_ramps_keep_the_rule_slope_and_landings() {
    let (f, m) = build_frame(&json!({"kind": "arch_bridge", "span": 90.0, "rise": 15.0, "deck_z": 6.0, "width": 4.0}), 0).unwrap();
    assert_eq!(f.ramps.len(), 2);
    assert_eq!(metric(&m, "ramps"), 2.0);
    for (r, top_x) in f.ramps.iter().zip([0.0, 90.0]) {
        let lay = r.layout().unwrap();
        assert!(r.slope <= 1.0 / 12.0 + 1e-3, "{}", r.slope);
        assert!(lay.flights.iter().all(|fl| fl.z1 - fl.z0 <= 0.75 + 1e-6), "rise between landings");
        assert_eq!(lay.landings.len(), 7);
        let last = lay.flights.last().unwrap();
        assert!((last.z1 - 6.0).abs() < 1e-6, "{}", last.z1);
        assert!((last.start.x + last.dir.x * last.len - top_x).abs() < 1e-6, "meets the deck");
        assert!((r.width - 3.4).abs() < 1e-9, "as wide as the deck between its guards");
    }
    // no side step: each ramp rail ends on the end of a deck rail
    for (g, top_x) in f.guards.iter().skip(2).zip([0.0, 0.0, 90.0, 90.0]) {
        let end = g.path.last().unwrap();
        assert!((end.0 - top_x).abs() < 1e-6 && (end.2 - 6.0).abs() < 1e-6 && [0.3, 3.7].iter().any(|y| (end.1 - y).abs() < 1e-9), "{end:?}");
    }
    // a gentler slope rule lengthens the ramp
    let long = build_frame(&json!({"kind": "arch_bridge", "span": 90.0, "deck_z": 6.0, "width": 4.0, "rules": {"steel.bridge.ramp_slope": 0.05}}), 0).unwrap().1;
    assert!(metric(&long, "ramp_length_m") > metric(&m, "ramp_length_m"));
    // landings every 0.5 m of rise: twice as many
    let lands = build_frame(&json!({"kind": "arch_bridge", "span": 90.0, "deck_z": 6.0, "width": 4.0, "rules": {"steel.bridge.ramp_rise_max": 0.5}}), 0).unwrap().1;
    assert_eq!(metric(&lands, "ramp_landings"), 2.0 * 11.0);
    // every rail of a ramp is flat on its landings
    for g in f.guards.iter().skip(2) {
        let flat = g.path.chunks(2).collect::<Vec<_>>().windows(2).all(|w| (w[0][1].2 - w[1][0].2).abs() < 1e-9);
        assert!(flat);
    }
}

/// A deck too high for a ramp of the rule length gets stairs in flights with landings between
/// them, guards on both sides of each flight and landing; the approach can also be forced
/// either way or left out.
#[test]
fn a_high_deck_gets_stairs_where_the_ramp_would_be_too_long() {
    let bridge = |extra: serde_json::Value| {
        let mut p = json!({"kind": "arch_bridge", "span": 90.0, "rise": 15.0, "deck_z": 12.0, "width": 4.0});
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        build_frame(&p, 0).unwrap()
    };
    let (f, m) = bridge(json!({}));
    assert!(f.ramps.is_empty(), "auto: 12 m up at 1:12 is over the rule length");
    // 12 m in flights of at most 2.7 m: 5 flights and 4 landings at each end
    assert_eq!(f.stairs.len(), 2 * 5);
    assert_eq!((metric(&m, "stairs"), metric(&m, "stair_landings")), (10.0, 8.0));
    for w in &f.stairs {
        let d = &w.def;
        let lay = d.layout().unwrap();
        assert!(d.height <= 2.7 + 1e-9, "rise of a flight {}", d.height);
        assert!(d.height / lay.risers as f64 <= 0.17 + 1e-9);
        assert!((d.width - 3.4).abs() < 1e-9, "as wide as the deck between its guards");
    }
    // the top flight of each end meets the deck edge; every other flight ends on a landing of
    // 1.5 m below the foot of the one above it
    let tops: Vec<f64> = f
        .stairs
        .iter()
        .filter(|w| (w.def.base_z + w.def.height - 12.0).abs() < 1e-9)
        .map(|w| w.def.origin.x + w.def.angle.cos() * w.def.layout().unwrap().tread * (w.def.layout().unwrap().risers - 1) as f64)
        .collect();
    assert_eq!(tops.len(), 2);
    assert!(tops.iter().all(|t| t.abs() < 1e-6 || (t - 90.0).abs() < 1e-6), "{tops:?}");
    let left: Vec<_> = f.stairs.iter().take(5).collect();
    for p in left.windows(2) {
        let (up, down) = (&p[0].def, &p[1].def);
        assert!((up.base_z - (down.base_z + down.height)).abs() < 1e-9, "levels meet on the landing");
        let down_top = down.origin.x + down.layout().unwrap().tread * (down.layout().unwrap().risers - 1) as f64;
        assert!((down_top - (up.origin.x - 1.5)).abs() < 1e-9 || (up.origin.x - down_top - 1.5).abs() < 1e-9, "landing of 1.5 m");
    }
    assert_eq!(bridge(json!({"approach": "ramps"})).0.ramps.len(), 2);
    assert!(bridge(json!({"approach": "none"})).0.stairs.is_empty());
    assert_eq!(build_frame(&json!({"kind": "arch_bridge", "span": 90.0, "deck_z": 6.0, "width": 4.0, "approach": "stairs"}), 0).unwrap().0.stairs.len(), 2 * 3);
    // a longer rule keeps the ramp; a higher flight rise gives fewer flights
    let r = bridge(json!({"rules": {"steel.bridge.ramp_max_len": 300.0}})).0;
    assert!(r.stairs.is_empty() && r.ramps.len() == 2);
    assert_eq!(bridge(json!({"rules": {"steel.bridge.stair_flight_rise": 4.0}})).0.stairs.len(), 2 * 3);
    // in the model: every flight has a guard on each side, every landing and the deck its own
    let mut model = Model::open(":memory:").unwrap();
    let lv = model.add_level("L0", 0.0).unwrap();
    let ids = model.add_steel_frame(lv, &f).unwrap();
    assert_eq!(model.stairs(lv).unwrap().len(), 10);
    assert_eq!(f.guards.len(), 2 + 8 * 2);
    assert_eq!(ids.railings.len(), f.guards.len() + 2 * 10);
    assert!(model.railings(lv).unwrap().len() >= 4, "H10: railing >= 4");
}

/// The guard metrics follow the length of the rail paths, which on a ramp is longer than its plan
/// length, and the posts at the rule spacing along them.
#[test]
fn guard_metrics_follow_the_rail_path_length() {
    use ddc_gen::steel_bridge::{path_length, post_count};
    for approach in ["ramps", "stairs", "none"] {
        let (f, m) = build_frame(&json!({"kind": "arch_bridge", "span": 90.0, "deck_z": 6.0, "width": 4.0, "approach": approach}), 0).unwrap();
        let mut len: f64 = f.guards.iter().map(|g| path_length(&g.path)).sum();
        let mut posts: usize = f.guards.iter().map(|g| post_count(&g.path, g.post_spacing)).sum();
        // the flights' own guards, on both edges of every stair
        for w in &f.stairs {
            for edge in w.def.layout().unwrap().edge_paths() {
                let path: Vec<_> = edge.iter().map(|p| (p.x, p.y, p.z)).collect();
                len += path_length(&path);
                posts += post_count(&path, w.post_spacing);
            }
        }
        assert_eq!(metric(&m, "guard_length_m"), len.round(), "{approach}");
        assert_eq!(metric(&m, "guard_posts"), posts as f64, "{approach}");
    }
    // flight guards count: stairs add more than their landings' guards alone
    let on = |a: &str| build_frame(&json!({"kind": "arch_bridge", "span": 90.0, "deck_z": 6.0, "width": 4.0, "approach": a}), 0).unwrap();
    let (st, none) = (on("stairs"), on("none"));
    let landing_posts: usize = st.0.guards.iter().map(|g| post_count(&g.path, g.post_spacing)).sum();
    assert!(metric(&st.1, "guard_posts") > landing_posts as f64);
    assert!(metric(&st.1, "guard_length_m") > metric(&none.1, "guard_length_m") + 2.0 * 6.0 * 2.0);
    let (f, _) = build_frame(&json!({"kind": "arch_bridge", "span": 90.0, "deck_z": 6.0, "width": 4.0}), 0).unwrap();
    let ramp_rail = path_length(&f.guards[2].path);
    let plan = f.guards[2].path.last().unwrap().0 - f.guards[2].path[0].0;
    assert!(ramp_rail > plan.abs() + 0.01, "{ramp_rail} over a plan length of {plan}");
}
