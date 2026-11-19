// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::lattice::Frame;
use super::outfit::{outfit, OutfitRules};
use super::tower::{lattice_tower_300, TowerParams};

fn built() -> (Frame, Frame, super::outfit::Outfit, TowerParams) {
    let p = TowerParams::default();
    let base = lattice_tower_300(&p).unwrap();
    let mut f = base.clone();
    let o = outfit(&mut f, &p, &OutfitRules::default()).unwrap();
    (base, f, o, p)
}

#[test]
fn outfit_keeps_one_connected_valid_frame() {
    let (base, f, _, _) = built();
    assert_eq!(base.components(), 1);
    assert_eq!(f.components(), 1, "railings, stairs or cabin hang loose");
    assert!(f.validate().is_empty(), "{:?}", f.validate());
}

#[test]
fn outfit_adds_members_by_tier() {
    let (base, f, o, p) = built();
    assert!(o.rail_posts >= 3 * 4 * 2, "posts {}", o.rail_posts);
    assert!(o.stair_treads >= 4 * 20, "treads {}", o.stair_treads);
    assert!(o.lift_rail_members >= 4 * 20, "lift {}", o.lift_rail_members);
    assert!(o.cabin_members >= 20 && o.mast_members >= 3);
    assert_eq!(o.piers.len(), 4);
    assert_eq!(f.piers, o.piers);
    assert!(o.piers.iter().all(|p| p.half_foot > p.half_top && p.z_foot < p.z_top));
    let added = f.members.len() - base.members.len();
    assert!(added >= o.rail_posts + o.stair_treads + o.lift_rail_members + o.cabin_members + o.mast_members);
    assert!(f.nodes.len() > base.nodes.len() && f.members.len() > base.members.len());
    assert!(o.stair_treads > (p.platforms[1] / OutfitRules::default().stair_riser) as usize, "a riser every step, not one per panel");
}

#[test]
fn mast_top_is_above_the_shaft_by_the_rules() {
    let (_, f, _, p) = built();
    let r = OutfitRules::default();
    let top = f.nodes.iter().map(|n| n.z).fold(0.0, f64::max);
    let want = p.height + r.cabin_height + r.roof_rise + r.mast_height;
    assert!((top - want).abs() < 1e-6, "{top} vs {want}");
}

#[test]
fn railings_stand_on_each_platform() {
    let (_, f, _, p) = built();
    let r = OutfitRules::default();
    for z in p.platforms {
        let n = f.nodes.iter().filter(|q| (q.z - (z + r.rail_height)).abs() < 1e-6).count();
        assert!(n >= 8, "platform {z}: {n} rail nodes");
    }
}

#[test]
fn legs_taper_with_decreasing_slope() {
    let p = TowerParams::default();
    let w: Vec<f64> = (0..=10).map(|i| p.w(p.platforms[1] * i as f64 / 10.0)).collect();
    for k in 1..w.len() {
        assert!(w[k] < w[k - 1]);
    }
    let d: Vec<f64> = w.windows(2).map(|x| x[0] - x[1]).collect();
    assert!(d[0] > d[d.len() - 1], "leg curve must flatten upwards");
}

#[test]
fn every_added_node_has_two_members_except_free_tips() {
    let (_, f, _, p) = built();
    let mut deg = vec![0usize; f.nodes.len()];
    for m in &f.members {
        deg[m.a as usize] += 1;
        deg[m.b as usize] += 1;
    }
    let r = OutfitRules::default();
    let tip = p.height + r.cabin_height + r.roof_rise + r.mast_height;
    let free = f.nodes.iter().enumerate().filter(|(i, n)| deg[*i] < 2 && (n.z - tip).abs() > 1e-6).count();
    assert_eq!(free, 0);
}

#[test]
fn stairs_have_a_landing_after_every_flight() {
    let (_, _, o, _) = built();
    let r = OutfitRules::default();
    assert!(o.stair_landings > 0);
    // 4 legs, one landing per `stair_flight` risers (a few risers end a panel without one)
    let flights = o.stair_treads as f64 / r.stair_flight;
    assert!((o.stair_landings as f64 - flights).abs() <= flights * 0.3 + 4.0, "{} landings for {} treads", o.stair_landings, o.stair_treads);
    let fewer = {
        let p = TowerParams::default();
        let mut f = lattice_tower_300(&p).unwrap();
        outfit(&mut f, &p, &OutfitRules { stair_flight: 8.0, ..r }).unwrap()
    };
    assert!(fewer.stair_landings > o.stair_landings);
}

#[test]
fn legs_with_stair_flights_take_no_stair_members() {
    let p = TowerParams::default();
    let mut f = lattice_tower_300(&p).unwrap();
    assert_eq!(f.flights.iter().map(|fl| ((fl.plan[0] + fl.plan[2]).signum() as i32, (fl.plan[1] + fl.plan[3]).signum() as i32)).collect::<std::collections::BTreeSet<_>>().len(), 2);
    let mut bare = f.clone();
    bare.flights.clear();
    let with = outfit(&mut f, &p, &OutfitRules::default()).unwrap();
    let all = outfit(&mut bare, &p, &OutfitRules::default()).unwrap();
    // two of four legs keep member stairs, every leg keeps its lift guide
    assert_eq!(with.stair_treads * 2, all.stair_treads);
    assert_eq!(with.lift_rail_members, all.lift_rail_members);
}
