// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::hier::{self, HierRules};
use super::hier_check::{self, CheckRules};
use super::hier_render;
use super::lattice::{Frame, MemRole, Pattern};
use super::plate::{self, PlateKind};
use super::profile::Profile;
use super::tower::TowerParams;
use crate::math::v3;
use ddc_fem as fem;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// lattice_tower_300 at full detail, built once for the tests, with its build time.
fn full() -> &'static (hier::Hier, Duration) {
    static H: OnceLock<(hier::Hier, Duration)> = OnceLock::new();
    H.get_or_init(|| {
        let t = Instant::now();
        let h = hier::tower(&TowerParams::default(), &HierRules::default()).expect("lattice_tower_300");
        (h, t.elapsed())
    })
}

#[test]
fn lattice_tower_300_has_three_lattice_levels() {
    let (h, _) = full();
    let t = h.totals();
    eprintln!("totals {t:?}, protos {}, roots {}, skeleton {}", h.protos.len(), h.roots.len(), h.skeleton.members.len());
    assert!(t.max_level() >= 3, "levels {:?}", t.by_level);
    assert!(t.by_level[1] > 0 && t.by_level[2] > 0 && t.by_level[3] > 0, "{:?}", t.by_level);
    assert!(h.protos.iter().any(|p| p.level == 3 && p.planar));
    assert!(h.protos.iter().any(|p| p.level == 2 && !p.kids.is_empty()), "a level-2 lattice with laced chords");
    let one = hier::tower(&TowerParams::default(), &HierRules { levels: 2, ..Default::default() }).unwrap();
    assert_eq!(one.totals().max_level(), 2);
}

#[test]
fn member_and_connection_counts_in_range() {
    let (h, _) = full();
    let t = h.totals();
    assert!((300_000..=3_000_000).contains(&t.pieces), "pieces {}", t.pieces);
    assert!((50_000..=3_000_000).contains(&t.plates), "plates {}", t.plates);
    assert!(t.rivets > 2 * t.plates, "rivets {} plates {}", t.rivets, t.plates);
    for k in PlateKind::ALL {
        assert!(t.plates_by_kind[k as usize] > 0, "no {} plates", k.key());
    }
    assert!((1_000..=100_000).contains(&t.placements), "placements {}", t.placements);
    // the instances carry the detail: far fewer prototypes than placed lattices
    assert!(h.protos.len() * 4 < t.placements, "protos {} placements {}", h.protos.len(), t.placements);
    let mass_t = t.mass / 1e3;
    assert!((3_000.0..=30_000.0).contains(&mass_t), "mass {mass_t} t");
}

#[test]
fn generation_within_time_budget() {
    let (_, built) = full();
    let t = Instant::now();
    let h = hier::tower(&TowerParams::default(), &HierRules::default()).unwrap();
    let again = t.elapsed();
    let budget = if cfg!(debug_assertions) { Duration::from_secs(60) } else { Duration::from_secs(3) };
    eprintln!("generation {:?} (first {:?}), {} pieces", again, built, h.totals().pieces);
    assert!(again < budget, "{again:?} over {budget:?}");
}

#[test]
fn generation_is_deterministic() {
    let (h, _) = full();
    let b = hier::tower(&TowerParams::default(), &HierRules::default()).unwrap();
    assert_eq!(h.digest(), b.digest());
    assert_eq!(h.totals(), b.totals());
    let c = hier::tower(&TowerParams::default(), &HierRules { setback: 0.7, ..Default::default() }).unwrap();
    assert_ne!(h.digest(), c.digest(), "the digest sees a changed rule");
}

#[test]
fn no_zero_length_members() {
    let (h, _) = full();
    assert_eq!(h.refused_zero, 0);
    assert_eq!(h.skeleton.refused_zero, 0);
    let short = |l: &hier::Leaf| l.len() < 1e-3;
    assert!(!h.leaves.iter().any(short));
    for p in &h.protos {
        assert!(!p.leaves.iter().any(short), "proto level {} len {}", p.level, p.len);
        assert!(p.len > 0.0 && p.width > 0.0);
    }
    for r in &h.roots {
        assert!(r.xf.z.len() > 0.999 && r.xf.x.dot(r.xf.y).abs() < 1e-9, "orthonormal placement");
    }
}

#[test]
fn every_bolt_inside_its_plate() {
    let (h, _) = full();
    let v = h.validate();
    assert!(v.is_empty(), "{} findings, first: {:?}", v.len(), v.iter().take(5).collect::<Vec<_>>());
    let all = h.plates.iter().chain(h.protos.iter().flat_map(|p| p.plates.iter()));
    let mut n = 0;
    for p in all {
        for &q in &p.rivets {
            assert!(plate::margin(&p.outline, q) >= p.d0 / 2.0 - 1e-9, "{:?} fastener hole outside its plate", p.kind);
            n += 1;
        }
    }
    assert!(n > 1000);
    // the check itself catches a fastener moved out
    let mut bad = h.protos.iter().flat_map(|p| p.plates.iter()).find(|p| p.kind == PlateKind::Gusset).unwrap().clone();
    let far = bad.outline.iter().map(|p| p.0).fold(f64::MIN, f64::max);
    bad.rivets[0] = (far, bad.rivets[0].1);
    assert!(!plate::check(&bad).is_empty());
}

#[test]
fn profile_spline_drives_the_legs() {
    let p = Profile::from_points(&[(0.0, 60.0), (60.0, 30.0), (120.0, 12.0), (300.0, 3.0)]).unwrap();
    assert_eq!(p.w(0.0), 60.0);
    assert_eq!(p.w(300.0), 3.0);
    assert_eq!(p.w(-5.0), 60.0);
    let mut last = f64::INFINITY;
    for i in 0..=300 {
        let w = p.w(i as f64);
        assert!(w <= last + 1e-12 && (3.0..=60.0).contains(&w), "monotone, no overshoot at {i}: {w}");
        last = w;
    }
    assert!(Profile::from_points(&[(0.0, 5.0), (0.0, 4.0)]).is_err());
    assert!(Profile::from_points(&[(0.0, 5.0)]).is_err());
    assert!(Profile::from_points(&[(0.0, 5.0), (10.0, -1.0)]).is_err());
    let tp = TowerParams { profile: Some(vec![(0.0, 50.0), (57.6, 28.0), (115.7, 12.0), (276.1, 4.0), (300.0, 3.0)]), ..Default::default() };
    assert!((tp.w(57.6) - 28.0).abs() < 1e-9);
    let h = hier::tower(&tp, &HierRules::default()).unwrap();
    let wide = h.skeleton.nodes.iter().filter(|n| n.z < 1e-6).map(|n| n.x.abs().max(n.y.abs())).fold(0.0, f64::max);
    assert!((wide - 50.0).abs() < 1.0, "the footprint follows the spline: {wide}");
    let v = h.validate();
    assert!(v.is_empty(), "{} findings, first: {:?}", v.len(), v.iter().take(5).collect::<Vec<_>>());
    let bad = TowerParams { profile: Some(vec![(0.0, 5.0), (0.0, 4.0)]), ..Default::default() };
    assert!(hier::tower(&bad, &HierRules::default()).is_err());
}

#[test]
fn bracing_patterns_by_rule() {
    let r = HierRules::default();
    assert_eq!(hier::pick_pattern(&r, MemRole::Girder, 1.0, 1.0).0, r.girder_pattern);
    assert_eq!(hier::pick_pattern(&r, MemRole::Arch, 1.0, 1.0).0, r.arch_pattern);
    assert_eq!(hier::pick_pattern(&r, MemRole::Chord, 1.0, 0.3).0, Pattern::K);
    assert_eq!(hier::pick_pattern(&r, MemRole::Chord, 1.0, 1.0).0, Pattern::X);
    assert_eq!(hier::pick_pattern(&r, MemRole::Chord, 0.5, 0.5), (Pattern::Warren, "steel.lat.x_min_width"));
    let (h, _) = full();
    let used: std::collections::BTreeSet<_> = h.protos.iter().map(|p| format!("{:?}", p.pattern)).collect();
    assert!(used.len() >= 3, "patterns in use {used:?}");
    assert!(h.decisions.iter().all(|d| d.rule.starts_with("steel.") && d.count > 0));
    assert!(h.decisions.iter().any(|d| d.key == "steel_pattern"));
    assert!(HierRules { levels: 4, ..Default::default() }.check().is_err());
}

#[test]
fn small_truss_matches_hand_calculation() {
    // span 2a, rise h, load P at the apex: rafter N = -P / (2 sin θ), tie N = P / (2 tan θ)
    let (a, h, p) = (3.0, 2.0, 100e3);
    let mut f = Frame::new();
    let (l, r, top) = (v3(0.0, 0.0, 0.0), v3(2.0 * a, 0.0, 0.0), v3(a, 0.0, h));
    let n = v3(0.0, 1.0, 0.0);
    let m0 = f.add(l, top, "L 100x100x10", MemRole::Chord, n).unwrap();
    let m1 = f.add(top, r, "L 100x100x10", MemRole::Chord, n).unwrap();
    let m2 = f.add(l, r, "L 100x100x10", MemRole::Chord, n).unwrap();
    let (il, ir, it) = (f.node(l), f.node(r), f.node(top));
    use fem::Restraint::{Fixed, Free};
    let sup = [fem::Support::new(il as usize, [Fixed, Fixed, Fixed, Fixed, Free, Free]), fem::Support::new(ir as usize, [Free, Fixed, Fixed, Fixed, Free, Free])];
    let ax = hier_check::axial(&f, &[(it, [0.0, 0.0, -p])], &sup).unwrap();
    let th = (h / a).atan();
    let (raf, tie) = (-p / (2.0 * th.sin()), p / (2.0 * th.tan()));
    for (m, want) in [(m0, raf), (m1, raf), (m2, tie)] {
        let got = ax[m as usize];
        assert!((got - want).abs() < 0.01 * want.abs(), "member {m}: {got} vs hand {want}");
    }
    // the allowable force is the plain stress at zero length and falls with slenderness
    let c = CheckRules::default();
    let s = super::sections::find("L 100x100x10").unwrap();
    let q = hier_check::props(&s);
    assert!((q.a - super::sections::area(&s)).abs() < 1e-9);
    assert!((hier_check::rankine(q.a, 1.0, 0.0, &c) - q.a * c.f_allow).abs() < 1e-6);
    let r = hier_check::props(&crate::member::Shape::Rect { b: 0.1, h: 0.2 });
    assert!((r.iy - 0.1 * 0.008 / 12.0).abs() < 1e-12 && (r.iz - 0.2 * 0.001 / 12.0).abs() < 1e-12, "{r:?}");
    assert!((r.w().0 - 0.1 * 0.04 / 6.0).abs() < 1e-12);
    let rr = (q.imin / q.a).sqrt();
    assert!(rr > 0.019 && rr < 0.020, "angle minor radius {rr}");
    assert!(hier_check::rankine(q.a, rr, 3.0, &c) < 0.5 * q.a * c.f_allow);
}

#[test]
fn preliminary_check_of_lattice_tower_300() {
    let (h, _) = full();
    let rep = hier_check::check(h, &CheckRules::default()).unwrap();
    eprintln!("check: max {:.3} at {}, over {}, sway {:.3} m, residual {:.1e}, eq {}", rep.max, rep.max_member, rep.over, rep.sway, rep.residual, rep.equations);
    let mut worst = rep.uses.clone();
    worst.sort_by(|a, b| b.ratio.total_cmp(&a.ratio));
    let lat: std::collections::HashSet<u32> = h.roots.iter().map(|r| r.member).collect();
    for u in worst.iter().take(8) {
        let m = &h.skeleton.members[u.member as usize];
        let (a, b) = (h.skeleton.nodes[m.a as usize], h.skeleton.nodes[m.b as usize]);
        eprintln!("  {:?} {} len {:.1} z {:.0} latticed {} demand {:.3e} cap {:.3e} ratio {:.2} slender {:.0}", m.role, h.skeleton.sections[m.sec as usize], (b - a).len(), (a.z + b.z) / 2.0, lat.contains(&u.member), u.demand, u.capacity, u.ratio, u.slender);
    }
    let by_role = |lt: bool| worst.iter().filter(|u| lat.contains(&u.member) == lt).map(|u| u.ratio).fold(0.0, f64::max);
    eprintln!("  worst latticed {:.2}, worst rolled {:.2}", by_role(true), by_role(false));
    assert!(rep.preliminary);
    assert_eq!(rep.uses.len(), h.skeleton.members.len());
    assert!(rep.residual < 1e-6, "equilibrium {}", rep.residual);
    assert!(rep.max.is_finite() && rep.max > 0.0);
    // a stiffness sanity bound (height / 50), not a serviceability check
    assert!(rep.sway > 0.0 && rep.sway < h.params.height / 50.0, "sway {}", rep.sway);
}

#[test]
fn instanced_groups_cover_every_placement() {
    let (h, _) = full();
    let (groups, rolled) = hier_render::groups(h);
    let placed: usize = groups.iter().map(|g| g.xfs.len()).sum();
    let own = h.placements().iter().filter(|(p, _)| !h.protos[*p as usize].leaves.is_empty()).count();
    assert_eq!(placed, own);
    assert!(!rolled.triangles.is_empty());
    let tri: usize = groups.iter().map(|g| g.lods[0].0.triangles.len() * g.xfs.len()).sum();
    eprintln!("groups {}, instances {placed}, instanced triangles {tri}, mesh bytes {}", groups.len(), groups.iter().map(|g| g.lods.iter().map(|l| l.0.bytes()).sum::<usize>()).sum::<usize>());
    for g in &groups {
        let m = &g.lods[0].0;
        assert!(m.triangles.iter().flatten().all(|&i| (i as usize) < m.positions.len()));
        if g.lods.len() > 1 {
            assert!(g.lods[1].0.triangles.len() < m.triangles.len() && g.lods[1].1 > 0.0);
        }
    }
}

fn reach_of(h: &hier::Hier, l: &hier::Leaf) -> f64 {
    let s = super::sections::find(&h.sections[l.sec as usize]).expect("section");
    let r = s.size().0.max(s.size().1);
    if l.heel { r } else { r / 2.0 }
}

#[test]
fn branches_lie_inside_the_box_and_bracing_ends_on_their_axes() {
    let (h, _) = full();
    let mut boxes = 0;
    for p in h.protos.iter().filter(|p| !p.planar) {
        let half = p.width / 2.0 + 1e-9;
        // laced branches: every piece of every branch, in the member's axes
        for (k, xf) in &p.kids {
            for l in &h.protos[*k as usize].leaves {
                let r = reach_of(h, l);
                for q in [l.a, l.b] {
                    let q = xf.p(q);
                    assert!(q.x.abs() + r <= half && q.y.abs() + r <= half, "level {} branch piece sticks out: {:?} + {r}, half width {half}", p.level, q);
                }
            }
        }
        // chords that are rolled angles: heel on the corner, both legs inward
        for l in p.leaves.iter().filter(|l| l.role == MemRole::Chord && l.heel) {
            let s = super::sections::find(&h.sections[l.sec as usize]).unwrap();
            let r = s.size().0.max(s.size().1);
            let u = (l.b - l.a).normalized();
            let y = (l.up - u * l.up.dot(u)).normalized();
            let z = u.cross(y).normalized();
            for tip in [l.a + y * r, l.a + z * r] {
                assert!(tip.x.abs() <= half && tip.y.abs() <= half, "angle leg outside the box at {:?}", tip);
            }
        }
        // bracing meets the branch axes at the member's ends
        if !p.kids.is_empty() {
            let c = p.axis_w / 2.0;
            let on_axis = |q: crate::math::V3| (q.x.abs() - c).abs() < 1e-6 && (q.y.abs() - c).abs() < 1e-6;
            let ends = p.leaves.iter().filter(|l| l.role == MemRole::Strut && l.a.z.abs() < 1e-9 && l.b.z.abs() < 1e-9);
            let mut n = 0;
            for l in ends {
                assert!(on_axis(l.a) && on_axis(l.b), "end strut off the branch axes: {:?} {:?}", l.a, l.b);
                n += 1;
            }
            assert!(n >= 4, "no end struts");
            boxes += 1;
        }
    }
    assert!(boxes > 0, "no latticed box with laced branches");
}
