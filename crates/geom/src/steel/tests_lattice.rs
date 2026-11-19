// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::lattice::{self, Frame, JointKind, MemRole, Pattern, Secs};
use super::rivet::{self, RivetGroup, RivetRule};
use super::sections;
use super::tower::{self, TowerParams};
use super::tower_size::Loads;

/// A heavier wind and floor than the rule defaults, so the sizing is tested off its defaults too.
const LOADS: Loads = Loads { wind_kpa: 2.5, face_factor: 0.4, self_kn_m: 150.0, deck_kpa: 6.0, f_allow_mpa: 120.0, rankine_a: 9000.0, strut_share: 0.025, e_gpa: 190.0 };

fn tp() -> TowerParams {
    TowerParams { loads: Some(LOADS), lifts: 2, ..TowerParams::default() }
}
use crate::math::{v3, V3};
use crate::member::Shape;

#[test]
fn section_table_parses_and_finds() {
    let t = sections::table();
    assert!(t.len() >= 90, "{}", t.len());
    assert!(matches!(sections::find("HEB 300"), Some(Shape::I { .. })));
    assert!(matches!(sections::find("l 100x100x10"), Some(Shape::Angle { .. })));
    assert!(matches!(sections::find("FL 100x10"), Some(Shape::Rect { .. })));
    assert!(sections::find("nope").is_none());
    let pg = sections::find("PG 1200/400/30/500/40/12").unwrap();
    let a = sections::area(&pg);
    let want = 0.4 * 0.03 + 0.5 * 0.04 + (1.2 - 0.07) * 0.012;
    assert!((a - want).abs() < 1e-9, "{a} {want}");
    assert!(sections::plate_girder(1.2, 0.4, 0.03, 0.4, 0.03, 0.002).is_err(), "web too slender");
    let ipe = sections::area(&sections::find("IPE 300").unwrap());
    assert!((ipe - 0.00500).abs() < 0.0006, "IPE 300 ~ 53.8 cm2 incl. roots: {ipe}");
}

#[test]
fn rivet_layout_is_valid_and_rules_fire() {
    let g = RivetGroup::layout(6, 2, 0.010, 0.020);
    assert!(g.check().is_empty(), "{:?}", g.check());
    assert_eq!(g.count(), 6);
    let mut bad = g.clone();
    bad.pitch = 0.03;
    assert!(bad.check().iter().any(|f| f.rule == RivetRule::PitchMin));
    let mut bad = g.clone();
    bad.end = 0.01;
    assert!(bad.check().iter().any(|f| f.rule == RivetRule::EndMin));
    let mut bad = g.clone();
    bad.edge = 0.01;
    assert!(bad.check().iter().any(|f| f.rule == RivetRule::EdgeMin));
    let mut bad = g.clone();
    bad.pitch = 0.25;
    assert!(bad.check().iter().any(|f| f.rule == RivetRule::PitchMax));
    let mut bad = g.clone();
    bad.gauge = 0.02;
    assert!(bad.check().iter().any(|f| f.rule == RivetRule::GaugeMin));
    let mut bad = g.clone();
    bad.grip = 0.2;
    assert!(bad.check().iter().any(|f| f.rule == RivetRule::Grip));
    let mut bad = g;
    bad.d = 0.0195;
    assert_eq!(bad.check()[0].rule, RivetRule::Series);
}

#[test]
fn rivet_proto_and_instances() {
    let s = &rivet::catalog().sizes[3];
    let m0 = rivet::proto_mesh(s, 0);
    let m1 = rivet::proto_mesh(s, 1);
    assert!(m1.triangles.len() * 2 < m0.triangles.len());
    assert!(m0.triangles.iter().flatten().all(|&i| (i as usize) < m0.positions.len()));
    assert_eq!(std::mem::size_of::<rivet::FastInst>(), 20);
    let f = rivet::FastInst::new(v3(100.0, 50.0, 280.0), v3(0.0, 0.0, 0.0), v3(0.0, 1.0, 1.0), 0.024);
    let back = f.pos(v3(0.0, 0.0, 0.0));
    assert!((back - v3(100.0, 50.0, 280.0)).len() < 1e-4);
    assert!((f.axis().z - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-3);
    assert_eq!(rivet::lod(5.0, 0.02), Some(0));
    assert_eq!(rivet::lod(30.0, 0.02), Some(1));
    assert_eq!(rivet::lod(100.0, 0.02), None);
}

const S: Secs = Secs { chord: "L 100x100x10", diag: "L 60x60x6", strut: "L 50x50x5" };

#[test]
fn truss_patterns_are_connected_and_counted() {
    for (pat, members) in [(Pattern::Warren, 2 * 6 + 1 + 6 + 6), (Pattern::Pratt, 12 + 1 + 6 + 6), (Pattern::X, 12 + 1 + 6 + 24), (Pattern::K, 12 + 6 + 1 + 6 + 12)] {
        let mut f = Frame::new();
        // Curved top chord, straight bottom chord.
        let top: Vec<_> = (0..=6).map(|i| v3(i as f64 * 3.0, 0.0, 3.0 + (i as f64 * 0.5).sin())).collect();
        let bot: Vec<_> = (0..=6).map(|i| v3(i as f64 * 3.0, 0.0, 0.0)).collect();
        f.truss(&top, &bot, pat, S, MemRole::Chord);
        assert!(f.validate().is_empty(), "{pat:?}: {:?}", f.validate());
        assert_eq!(f.members.len(), members, "{pat:?}");
        assert_eq!(f.refused_zero, 0);
    }
}

#[test]
fn zero_length_and_duplicates_refused() {
    let mut f = Frame::new();
    assert!(f.add(v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 0.0005), "L 50x50x5", MemRole::Strut, v3(1.0, 0.0, 0.0)).is_none());
    let a = f.add(v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), "L 50x50x5", MemRole::Strut, v3(0.0, 0.0, 1.0));
    let b = f.add(v3(1.0, 0.0, 0.0), v3(0.0, 0.0, 0.0), "L 50x50x5", MemRole::Strut, v3(0.0, 0.0, 1.0));
    assert_eq!(a, b);
    assert_eq!(f.members.len(), 1);
    assert_eq!(f.refused_zero, 1);
    f.add(v3(5.0, 0.0, 0.0), v3(6.0, 0.0, 0.0), "L 50x50x5", MemRole::Strut, v3(0.0, 0.0, 1.0));
    assert!(f.validate().iter().any(|e| e.contains("2 disconnected")));
}

#[test]
fn tapered_box_lattice_joints_check_clean() {
    let lv: Vec<_> = (0..=10)
        .map(|k| {
            let z = k as f64 * 2.0;
            let w = 3.0 - 0.1 * k as f64;
            [v3(w, w, z), v3(-w, w, z), v3(-w, -w, z), v3(w, -w, z)]
        })
        .collect();
    let mut f = Frame::new();
    f.box_lattice(&lv, Pattern::X, S, 2, "L 50x50x5");
    assert!(f.validate().is_empty(), "{:?}", f.validate());
    let (js, rep) = lattice::joints(&f, 0.0);
    assert!(rep.findings.is_empty(), "{:?}", &rep.findings[..rep.findings.len().min(3)]);
    assert_eq!(js.iter().filter(|j| j.kind == JointKind::Base).count(), 4);
    assert_eq!(js.iter().filter(|j| j.kind == JointKind::Crossing).count(), 40);
    assert!(js.iter().filter(|j| j.kind == JointKind::Gusset).all(|j| j.plate.0 > 0.1 && j.plate.2 >= 0.010));
    let sets = lattice::fasteners(&f, &js, true);
    let n: usize = sets.iter().map(|s| s.items.len()).sum();
    assert!(n > 500, "{n}");
}

#[test]
fn portal_and_braced_frames_valid() {
    let p = tower::portal_frame(24.0, 6.0, 10.0, 6, 6.0, 1.5).unwrap();
    assert!(p.validate().is_empty(), "{:?}", p.validate());
    let b = tower::braced_frame(3, 2, 6.0, 7.5, 5, 3.6).unwrap();
    assert!(b.validate().is_empty(), "{:?}", b.validate());
    assert_eq!(b.members.iter().filter(|m| m.role == MemRole::Column).count(), 4 * 3 * 5);
    assert!(tower::portal_frame(0.5, 6.0, 10.0, 6, 6.0, 1.5).is_err());
}

#[test]
fn tower_params_refused() {
    let mut p = tp();
    p.platforms = [120.0, 115.0, 276.0];
    assert!(tower::lattice_tower_300(&p).is_err());
    let mut p = tp();
    p.arch_rise = 55.0;
    assert!(tower::lattice_tower_300(&p).is_err());
}

/// The benchmark: build, validate, derive joints, expand rivets; prints the numbers.
#[test]
fn lattice_tower_300_bench() {
    let t0 = std::time::Instant::now();
    let f = tower::lattice_tower_300(&tp()).unwrap();
    let t_frame = t0.elapsed();
    let errs = f.validate();
    assert!(errs.is_empty(), "{errs:?}");
    let t1 = std::time::Instant::now();
    let (js, rep) = lattice::joints(&f, 0.0);
    let t_joint = t1.elapsed();
    let t2 = std::time::Instant::now();
    let sets = lattice::fasteners(&f, &js, true);
    let t_fast = t2.elapsed();
    let rivets: usize = sets.iter().map(|s| s.items.len()).sum();
    let fast_bytes: usize = sets.iter().map(|s| s.bytes()).sum();
    let top = f.nodes.iter().map(|p| p.z).fold(0.0, f64::max);
    println!(
        "lattice_tower_300: members {} nodes {} sections {} mass {:.0} t | joints {} {:?} plate protos {} findings {} | rivets {} ({:.1} MB) frame {:.2} MB | build {:?} joints {:?} rivets {:?} | height {top:.1}",
        f.members.len(),
        f.nodes.len(),
        f.sections.len(),
        f.mass() / 1000.0,
        rep.joints,
        rep.by_kind,
        rep.plate_protos,
        rep.findings.len(),
        rivets,
        fast_bytes as f64 / 1e6,
        f.bytes() as f64 / 1e6,
        t_frame,
        t_joint,
        t_fast,
    );
    assert!((top - 300.0).abs() < 1e-6);
    assert!((15_000..30_000).contains(&f.members.len()), "{}", f.members.len());
    assert!(rivets > 1_000_000, "{rivets}");
    assert!(rep.findings.is_empty(), "{:?}", &rep.findings[..rep.findings.len().min(5)]);
    assert_eq!(f.refused_zero, 0);
    // Near-camera pass: rivets within 15 m of an eye at the first platform corner.
    let w = tp().w(57.6);
    let near: usize = sets.iter().map(|s| s.near(v3(w, w, 57.6), 15.0).count()).sum();
    println!("near-camera rivets within 15 m: {near}");
    assert!(near > 0 && near * 20 < rivets);
}

#[test]
fn tower_sections_come_from_the_sizing_rule_and_carry_their_force() {
    use super::tower_size::{self, Ladders};
    let r = &LOADS;
    let lad = Ladders::builtin();
    for pat in [Pattern::X, Pattern::K] {
        let p = TowerParams { pattern: pat, ..tp() };
        for z in (0..300).map(|i| i as f64 + 0.5) {
            let s = tower_size::size_at(&p, r, z);
            assert_eq!(s.over, 0, "{pat:?} at {z}: {s:?}");
            assert!(lad.chord.iter().any(|c| c == s.chord) && lad.brace.iter().any(|c| c == s.diag) && lad.strut.iter().any(|c| c == s.strut));
            let panel = if z < p.platforms[1] { p.panel_leg } else { p.panel_shaft };
            assert!(tower_size::allowable(s.chord, panel, true, r) >= s.chord_n);
        }
        let area = |z: f64| tower_size::props(tower_size::size_at(&p, r, z).chord).unwrap().0;
        assert!(area(1.0) > 4.0 * area(299.0), "base chords far heavier than the top");
        let (sway, over) = tower_size::sway(&p, r);
        assert_eq!(over, 0);
        assert!(sway > 0.05 && sway < p.height / 200.0, "sway {sway}");
    }
    // the frame uses exactly the sized sections in its lattice
    let f = tower::lattice_tower_300(&tp()).unwrap();
    let base = f.members.iter().find(|m| m.role == MemRole::Chord && f.nodes[m.a as usize].z.min(f.nodes[m.b as usize].z) < 1e-9).unwrap();
    assert_eq!(f.sections[base.sec as usize], tower_size::size_at(&tp(), r, 0.8).chord);
    // a wider ladder top or a twice-given rule is refused
    let txt = include_str!("../../data/steel/tower_sizing.csv");
    assert!(Ladders::parse(&txt.replace("ladder,chord,L 150x150x15\n", "ladder,chord,L 150x150x15\nladder,chord,L 60x60x6\n")).is_err());
    assert!(Ladders::parse(&format!("{txt}rule,wind_kpa,1\n")).is_err());
    assert!(Ladders::parse(&txt.replace("ladder,strut,L 60x60x6", "ladder,strut,L 61x61x6")).is_err());
    // the shipped ladders parse; an equal-area step (angle then box) is refused and names its line
    Ladders::parse(txt).unwrap();
    let dup = txt.replace("ladder,brace,SHS 200x200x10
", "ladder,brace,L 200x200x20
ladder,brace,SHS 200x200x10
");
    let line = 1 + dup.lines().position(|l| l == "ladder,brace,SHS 200x200x10").unwrap();
    let e = Ladders::parse(&dup).unwrap_err();
    assert!(e.starts_with(&format!("line {line}:")) && e.contains("SHS 200x200x10"), "{e}");
    let e = Ladders::parse(&txt.replace("ladder,strut,L 60x60x6", "ladder,strut,L 61x61x6")).unwrap_err();
    assert!(e.starts_with("line "), "{e}");
    // numbers are rules, not section data; the tower is refused without its loads or with one out of range
    assert!(tower::lattice_tower_300(&TowerParams { loads: None, ..tp() }).is_err());
    assert!(tower::lattice_tower_300(&TowerParams::default()).is_ok());
    assert!(tower::lattice_tower_300(&TowerParams { loads: Some(Loads { f_allow_mpa: 0.0, ..LOADS }), ..tp() }).is_err());
}

#[test]
fn angle_and_box_section_properties() {
    use super::tower_size::props;
    let (a, r) = props("L 100x100x10").unwrap();
    assert!((a - 0.0019).abs() < 1e-6, "{a}");
    assert!((r - 0.0196).abs() < 0.0006, "least radius of an equal angle {r}");
    let (a, r) = props("SHS 200x200x10").unwrap();
    assert!((a - 0.0076).abs() < 1e-9);
    let i = (0.2f64.powi(4) - 0.18f64.powi(4)) / 12.0;
    assert!((r - (i / a).sqrt()).abs() < 1e-9);
}

#[test]
fn tower_carries_three_decks_and_stairs_inside_two_legs() {
    let p = tp();
    let f = tower::lattice_tower_300(&p).unwrap();
    assert!(f.chords_are_columns);
    let z: Vec<f64> = f.decks.iter().map(|d| d.z).collect();
    assert_eq!(z, p.platforms.to_vec());
    for d in &f.decks {
        assert!((d.half - p.w(d.z)).abs() < 1e-9 && d.open < d.half);
        for h in &d.holes {
            // an opening lies on the floor, clear of the open centre
            assert!(h[0] < h[2] && h[1] < h[3]);
            assert!(h.iter().all(|v| v.abs() < d.half), "{h:?} off a deck of {}", d.half);
            assert!(h[0].abs().min(h[2].abs()).max(h[1].abs().min(h[3].abs())) > d.open, "{h:?} in the open centre {}", d.open);
        }
    }
    assert_eq!(f.decks.iter().map(|d| d.holes.len()).collect::<Vec<_>>(), vec![2, 3, 1]);
    // two lift runs up the core: ground to the second platform, then to the top, each clear of
    // the plan brace (x = y), through the open centre of the first platform and inside the top one
    let l = &f.lifts;
    assert_eq!(l.len(), 2);
    assert_eq!((l[0].z0, l[0].z1, l[1].z0, l[1].z1), (0.0, p.platforms[1], p.platforms[1], p.platforms[2]));
    for r in l {
        let c = [(r.plan[0], r.plan[1]), (r.plan[2], r.plan[1]), (r.plan[2], r.plan[3]), (r.plan[0], r.plan[3])];
        assert!(c.iter().all(|q| (q.0 - q.1).abs() / 2f64.sqrt() > 0.3), "{r:?} on the plan brace");
        assert!(c.iter().all(|q| q.0.abs().max(q.1.abs()) < f.decks[0].open && q.0.abs().max(q.1.abs()) < p.w(p.platforms[2]) - 0.3));
        assert!(f.decks.iter().filter(|d| d.z > r.z0 + 1e-9 && d.z <= r.z1 + 1e-9).all(|d| d.open > 0.0 || d.holes.contains(&r.plan)));
    }
    assert!(tower::lattice_tower_300(&TowerParams { lifts: 0, ..p.clone() }).unwrap().lifts.is_empty());
    assert!(tower::lattice_tower_300(&TowerParams { lift_offset: 1.0, ..p.clone() }).is_err(), "a lift on the plan brace is refused");
    let slim = TowerParams { profile: Some(vec![(0.0, 40.0), (57.6, 22.0), (115.7, 10.0), (300.0, 3.0)]), ..p.clone() };
    assert_eq!(tower::lattice_tower_300(&slim).unwrap().lifts.len(), 1, "no lift to a top platform too small for its box");
    // flights climb without gaps from the ground to the merge, two legs
    assert_eq!(f.flights.first().unwrap().z0, 0.0);
    let tops = f.flights.iter().filter(|s| (s.z1 - p.platforms[1]).abs() < 1e-9).count();
    assert_eq!(tops, 2);
    for w in f.flights.windows(2).filter(|w| w[1].z0 > 0.0) {
        assert!((w[1].z0 - w[0].z1).abs() < 1e-9 && w[0].z1 - w[0].z0 <= 3.6 + 1e-9);
    }
    // each flight stays inside its leg box over its whole rise, and off the leg plan brace
    let s_at = |z: f64| p.leg0 + (p.w(p.platforms[1]) - p.leg0) * z / p.platforms[1];
    for fl in &f.flights {
        for z in [fl.z0, fl.z1] {
            let (w, s) = (p.w(z), s_at(z));
            let (x0, x1) = (fl.plan[0].abs().min(fl.plan[2].abs()), fl.plan[0].abs().max(fl.plan[2].abs()));
            let (y0, y1) = (fl.plan[1].abs().min(fl.plan[3].abs()), fl.plan[1].abs().max(fl.plan[3].abs()));
            assert!(x0 > w - s + 0.3 && x1 < w - 0.3 && y0 > w - s + 0.3 && y1 < w - 0.3, "{fl:?} at {z}: leg {}..{w}", w - s);
        }
        let corners = [(fl.plan[0], fl.plan[1]), (fl.plan[2], fl.plan[1]), (fl.plan[2], fl.plan[3]), (fl.plan[0], fl.plan[3])];
        let side: Vec<bool> = corners.iter().map(|c| c.0.abs() > c.1.abs()).collect();
        assert!(side.iter().all(|s| *s == side[0]), "{fl:?} crosses the plan brace");
    }
}

fn a_guyed_mast_is_a_triangular_lattice_held_by_three_guys_per_level() {
    let f = tower::guyed_mast(80.0, 1.2, 1.6, 3, 0.625, Pattern::Warren).unwrap();
    assert!(f.validate().is_empty(), "{:?}", f.validate());
    let cables: Vec<_> = f.members.iter().filter(|m| m.role == MemRole::Cable).collect();
    assert_eq!(cables.len(), 9);
    let (mut tops, mut feet) = (vec![], vec![]);
    for m in &cables {
        let (a, b) = (f.nodes[m.a as usize], f.nodes[m.b as usize]);
        let (lo, hi) = if a.z < b.z { (a, b) } else { (b, a) };
        assert!(lo.z.abs() < 1e-9 && ((lo.x * lo.x + lo.y * lo.y).sqrt() - 50.0).abs() < 1e-6, "anchor {lo:?}");
        tops.push((hi.z * 10.0).round() / 10.0);
        feet.push(((lo.x * 1e3).round() as i64, (lo.y * 1e3).round() as i64));
    }
    tops.sort_by(f64::total_cmp);
    tops.dedup();
    assert_eq!(tops, vec![25.6, 51.2, 76.8], "guy levels near a third, two thirds and the head");
    feet.sort();
    feet.dedup();
    assert_eq!(feet.len(), 3, "one anchor per leg direction");
    // the shaft is a triangle 1.2 m a side at every level
    let legs: Vec<V3> = f.nodes.iter().copied().filter(|p| p.z.abs() < 1e-9 && (p.x * p.x + p.y * p.y).sqrt() < 1.0).collect();
    assert_eq!(legs.len(), 3);
    for i in 0..3 {
        assert!(((legs[i] - legs[(i + 1) % 3]).len() - 1.2).abs() < 1e-9);
    }
    assert!(f.members.iter().filter(|m| m.role == MemRole::Diagonal).count() >= 3 * 50);
    assert!(tower::guyed_mast(80.0, 1.2, 1.6, 0, 0.625, Pattern::Warren).is_err());
}
