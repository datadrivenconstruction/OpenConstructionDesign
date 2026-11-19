//! The steel frame generator's long spans: an 80 m truss hall, a 90 m arch bridge and a stadium
//! cantilever ring, each standing on its own columns; a ring without a back-span is refused.

use ddc_gen::steel::build_frame;
use ddc_geom::steel::lattice::MemRole;
use serde_json::json;

fn column_xs(params: serde_json::Value) -> (Vec<f64>, usize) {
    let (f, m) = build_frame(&params, 0).unwrap_or_else(|e| panic!("{params}: {e:?}"));
    let n = m.iter().find(|x| x.0 == "members").unwrap().1 as usize;
    let mut xs: Vec<f64> = f
        .members
        .iter()
        .filter(|m| m.role == MemRole::Column)
        .map(|m| f.nodes[m.a as usize].x)
        .collect();
    xs.sort_by(f64::total_cmp);
    xs.dedup_by(|a, b| (*a - *b).abs() < 0.5);
    (xs, n)
}

#[test]
fn long_spans_stand_clear_between_their_columns() {
    let (xs, n) = column_xs(json!({"kind": "truss_hall", "span": 80, "height": 20, "bays": 10, "spacing": 10}));
    assert_eq!(xs, vec![0.0, 80.0]);
    assert!(n > 300, "{n}");
    let (xs, _) = column_xs(json!({"kind": "arch_bridge", "span": 90, "rise": 15, "width": 4.5, "deck_z": 6}));
    assert_eq!(xs, vec![0.0, 90.0]);
    let (_, n) = column_xs(json!({"kind": "cantilever_ring", "span": 120, "width": 84, "bays": 36, "reach": 28, "backspan": 9, "height": 30}));
    assert!(n > 1000, "{n}");
    let e = build_frame(&json!({"kind": "cantilever_ring", "span": 120, "width": 84, "reach": 28, "backspan": 4, "height": 30}), 0).unwrap_err();
    assert!(format!("{e:?}").contains("back-span"), "{e:?}");
}

/// Member ends no other member shares, above the lowest point of the frame.
fn free_ends(f: &ddc_geom::steel::lattice::Frame) -> usize {
    let mut deg = vec![0usize; f.nodes.len()];
    for m in &f.members {
        deg[m.a as usize] += 1;
        deg[m.b as usize] += 1;
    }
    let low = f.nodes.iter().map(|p| p.z).fold(f64::MAX, f64::min);
    (0..f.nodes.len()).filter(|&i| deg[i] == 1 && f.nodes[i].z > low + 1e-6).count()
}

#[test]
fn long_spans_leave_no_free_ends_and_the_arch_lands_on_the_pier_caps() {
    let mut cases: Vec<serde_json::Value> = ["warren", "pratt", "k", "x"]
        .iter()
        .map(|p| json!({"kind": "truss_hall", "span": 80, "height": 20, "bays": 6, "spacing": 10, "pattern": p}))
        .collect();
    cases.push(json!({"kind": "arch_bridge", "span": 90, "rise": 15, "width": 4.5, "deck_z": 6}));
    cases.push(json!({"kind": "cantilever_ring", "span": 120, "width": 84, "bays": 24, "reach": 28, "backspan": 9, "height": 30}));
    for c in &cases {
        let (f, _) = build_frame(c, 0).unwrap_or_else(|e| panic!("{c}: {e:?}"));
        assert_eq!(free_ends(&f), 0, "{c}");
    }
    let (f, _) = build_frame(&cases[4], 0).unwrap();
    let tops: Vec<_> = f
        .members
        .iter()
        .filter(|m| m.role == MemRole::Column)
        .map(|m| {
            let (p, q) = (f.nodes[m.a as usize], f.nodes[m.b as usize]);
            if p.z > q.z { p } else { q }
        })
        .collect();
    let arch: Vec<_> = f.members.iter().filter(|m| m.role == MemRole::Arch).flat_map(|m| [m.a, m.b]).collect();
    let mut deg = std::collections::HashMap::new();
    for i in &arch {
        *deg.entry(*i).or_insert(0) += 1;
    }
    let ends: Vec<_> = deg.iter().filter(|(_, &d)| d == 1).map(|(&i, _)| f.nodes[i as usize]).collect();
    assert_eq!(ends.len(), 4, "two arches, two ends each");
    for e in ends {
        assert!(tops.iter().any(|t| (*t - e).len() < 1e-3), "arch end {e:?} off the pier caps");
    }
}

/// The arch bridge reports the vibration check of its deck: frequency, sag and a verdict.
#[test]
fn the_arch_bridge_reports_its_deck_vibration() {
    let m = |p: serde_json::Value| build_frame(&p, 0).unwrap().1;
    let get = |m: &[(&str, f64)], k: &str| m.iter().find(|x| x.0 == k).unwrap_or_else(|| panic!("no {k}")).1;
    let long = m(json!({"kind": "arch_bridge", "span": 90.0, "rise": 15.0, "deck_z": 6.0, "width": 4.0}));
    let short = m(json!({"kind": "arch_bridge", "span": 40.0, "rise": 7.0, "deck_z": 6.0, "width": 4.0}));
    let (fl, fs) = (get(&long, "deck_freq_hz"), get(&short, "deck_freq_hz"));
    assert!(fl > 0.0 && fs > 3.0 * fl, "{fl} Hz over 90 m, {fs} Hz over 40 m");
    assert!(get(&long, "deck_sag_mm") > get(&short, "deck_sag_mm"));
    assert!(get(&short, "deck_comfort_ok") >= get(&long, "deck_comfort_ok"));
    assert!(m(json!({"kind": "truss_hall", "span": 80.0, "height": 20.0})).iter().all(|x| x.0 != "deck_freq_hz"));
}

/// A stadium roof ring of 200 x 160 m builds; the span limits are rules: the ring takes up to 250 m
/// by default, a hall only 120 m, and a rule override moves either.
#[test]
fn a_stadium_ring_of_200_by_160_builds_and_the_span_limits_are_rules() {
    let ring = |extra: serde_json::Value| {
        let mut p = json!({"kind": "cantilever_ring", "span": 200, "width": 160, "reach": 25, "backspan": 8, "height": 30, "bays": 40});
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        build_frame(&p, 0)
    };
    let (f, _) = ring(json!({})).unwrap();
    assert!(!f.members.is_empty());
    assert!(ring(json!({"span": 260})).is_err(), "over the ring's 250 m");
    assert!(ring(json!({"span": 260, "rules": {"steel.ring.max_span": 300}})).is_ok(), "the rule moves the limit");
    assert!(ring(json!({"bays": 100})).is_err(), "over the ring's 96 trusses");
    assert!(ring(json!({"span": "260"})).is_err(), "a number as text is refused, not read as the default");
    let hall = |span: f64, rules: serde_json::Value| build_frame(&json!({"kind": "truss_hall", "span": span, "height": 25, "rules": rules}), 0);
    assert!(hall(150.0, json!({})).is_err(), "a hall stays within 120 m");
    assert!(hall(150.0, json!({"steel.long.max_span": 160})).is_ok());
}

/// H04 asks for at least 12 columns and 300 frame members (trusses, braces, beams) of the roof
/// at its own parameters: a radial truss per bay with its two columns, ring girders, purlins at
/// every top chord node and plan bracing give them by construction.
#[test]
fn the_h04_stadium_ring_gives_the_columns_and_members_its_oracles_count() {
    use ddc_geom::steel::lattice::MemRole;
    let p = json!({"kind": "cantilever_ring", "span": 200, "width": 160, "reach": 25, "backspan": 8, "height": 30, "bays": 40});
    let (f, _) = build_frame(&p, 0).unwrap();
    let cols = f.members.iter().filter(|m| m.role == MemRole::Column).count();
    let frame = f.members.iter().filter(|m| m.role != MemRole::Column).count();
    assert_eq!(cols, 80, "two columns under each of the 40 trusses");
    assert!(frame >= 300, "{frame} frame members");
    let braces = f.members.iter().filter(|m| m.role == MemRole::PlanBrace).count();
    assert!(braces >= 10, "plan bracing in every fourth bay: {braces}");
    // fewer bays, fewer members, never below the 8 trusses the ring needs
    let (small, _) = build_frame(&json!({"kind": "cantilever_ring", "span": 200, "width": 160, "reach": 25, "backspan": 8, "height": 30, "bays": 8}), 0).unwrap();
    assert!(small.members.len() < f.members.len());
    assert!(small.members.iter().filter(|m| m.role == MemRole::Column).count() >= 12);
}
