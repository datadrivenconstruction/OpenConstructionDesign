// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Pass and fail fixtures for every rule of the realism critic, then the same rules over a
//! model with real solids.

use super::realism::*;
use super::{CheckScope, Severity};
use crate::solids::SolidSpec;
use crate::Model;
use ddc_geom::gsolid::{Form, Frame, Path3, Section};
use ddc_geom::mesh::Mesh;
use ddc_geom::{v2, v3};

fn key(t: &str, i: i64) -> (String, i64) {
    (t.to_string(), i)
}

fn member(id: i64, pts: &[[f64; 3]]) -> Member {
    Member {
        key: key("solid", id),
        shape: Shape::Open,
        depth: 0.2,
        axis: pts.to_vec(),
        closed: false,
        radius: None,
        bend: None,
        ecc: false,
    }
}

fn count(inp: &Input, k: &str) -> usize {
    analyze(inp).iter().filter(|i| i.msg_key == k).count()
}

fn one(f: impl FnOnce(&mut Input)) -> Input {
    let mut i = Input::default();
    f(&mut i);
    i
}

fn part(id: i64, bbox: [f64; 6], min_dim: f64) -> Part {
    Part { key: key("solid", id), bbox, min_dim, mesh: None }
}

fn cube(x0: f64, x1: f64) -> Mesh {
    let p = |x: f64, y: f64, z: f64| v3(x, y, z);
    let positions = vec![
        p(x0, 0.0, 0.0),
        p(x1, 0.0, 0.0),
        p(x1, 1.0, 0.0),
        p(x0, 1.0, 0.0),
        p(x0, 0.0, 1.0),
        p(x1, 0.0, 1.0),
        p(x1, 1.0, 1.0),
        p(x0, 1.0, 1.0),
    ];
    let t = [
        [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7], [0, 1, 5], [0, 5, 4],
        [1, 2, 6], [1, 6, 5], [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7],
    ];
    Mesh { positions, triangles: t.to_vec(), face_of: vec![0; 12] }
}

#[test]
fn straight_members_are_not_flagged_bent() {
    let i = one(|i| i.members.push(member(1, &[[0.0; 3], [5.0, 0.0, 0.0]])));
    assert_eq!(count(&i, K_BENT) + count(&i, K_RADIUS), 0);
}

#[test]
fn a_curved_member_without_a_bend_type_is_flagged() {
    let i = one(|i| {
        let mut m = member(1, &[[0.0; 3], [5.0, 0.0, 0.0]]);
        m.radius = Some(8.0);
        i.members.push(m);
    });
    assert_eq!(count(&i, K_BENT), 1);
    let mut kink = member(2, &[[0.0; 3], [5.0, 0.0, 0.0]]);
    kink.radius = Some(0.0);
    assert_eq!(count(&one(|i| i.members.push(kink)), K_BENT), 1);
}

#[test]
fn typed_bend_needs_the_table_radius() {
    let mk = |r: f64, method| {
        one(|i| {
            let mut m = member(1, &[[0.0; 3], [5.0, 0.0, 0.0]]);
            m.radius = Some(r);
            m.bend = Some(method);
            i.members.push(m);
        })
    };
    // open section 0.2 m deep: cold needs 5 m, hot 2 m
    assert_eq!(count(&mk(5.0, BendMethod::Cold), K_RADIUS), 0);
    assert_eq!(count(&mk(4.9, BendMethod::Cold), K_RADIUS), 1);
    assert_eq!(count(&mk(2.0, BendMethod::Hot), K_RADIUS), 0);
    assert_eq!(count(&mk(1.9, BendMethod::Hot), K_RADIUS), 1);
    assert_eq!(count(&mk(4.9, BendMethod::Cold), K_BENT), 0);
}

#[test]
fn bend_table_orders_methods_and_classes() {
    for s in [Shape::Open, Shape::Hollow, Shape::Bar] {
        let (hot, cold) = (BendMethod::Hot, BendMethod::Cold);
        assert!(min_bend_radius(s, 0.2, hot) < min_bend_radius(s, 0.2, cold));
    }
    assert!(
        min_bend_radius(Shape::Open, 0.2, BendMethod::Cold)
            > min_bend_radius(Shape::Hollow, 0.2, BendMethod::Cold)
    );
}

fn node(beam_end: [f64; 3], ecc: bool) -> Input {
    one(|i| {
        i.members.push(member(1, &[[0.0, 0.0, 0.0], [0.0, 0.0, 3.0]]));
        let mut b = member(2, &[beam_end, [4.0, 0.0, 3.0]]);
        b.ecc = ecc;
        i.members.push(b);
    })
}

#[test]
fn axes_meeting_within_a_millimetre_pass() {
    assert_eq!(count(&node([0.0, 0.0, 3.0], false), K_AXIS), 0);
    assert_eq!(count(&node([0.0005, 0.0, 3.0], false), K_AXIS), 0);
}

#[test]
fn an_axis_that_misses_the_node_is_flagged() {
    let r = analyze(&node([0.05, 0.0, 3.0], false));
    let f: Vec<_> = r.iter().filter(|i| i.msg_key == K_AXIS).collect();
    assert_eq!(f.len(), 1);
    assert!((f[0].measure.unwrap().value - 50.0).abs() < 1e-6);
    assert_eq!(count(&node([0.0015, 0.0, 3.0], false), K_AXIS), 1);
}

#[test]
fn declared_eccentricity_and_far_members_are_not_a_miss() {
    assert_eq!(count(&node([0.05, 0.0, 3.0], true), K_AXIS), 0);
    assert_eq!(count(&node([2.0, 0.0, 3.0], false), K_AXIS), 0);
}

#[test]
fn tiny_parts_are_flagged() {
    let mk = |d: f64| one(|i| i.parts.push(part(1, [0.0, 0.0, 0.0, 1.0, 1.0, 1.0], d)));
    assert_eq!(count(&mk(0.005), K_TINY), 0);
    assert_eq!(count(&mk(0.004), K_TINY), 1);
}

#[test]
fn slivers_between_boxes() {
    let mk = |gap: f64| {
        one(|i| {
            i.parts.push(part(1, [0.0, 0.0, 0.0, 1.0, 1.0, 1.0], 1.0));
            i.parts.push(part(2, [1.0 + gap, 0.0, 0.0, 2.0, 1.0, 1.0], 1.0));
        })
    };
    assert_eq!(count(&mk(0.0), K_SLIVER), 0);
    assert_eq!(count(&mk(0.0005), K_SLIVER), 1);
    assert_eq!(count(&mk(0.002), K_SLIVER), 0);
}

#[test]
fn slivers_are_measured_on_the_meshes_when_they_exist() {
    let mk = |gap: f64| {
        one(|i| {
            let mut a = part(1, [0.0, 0.0, 0.0, 1.0, 1.0, 1.0], 1.0);
            a.mesh = Some(std::sync::Arc::new(cube(0.0, 1.0)));
            let mut b = part(2, [1.0 + gap, 0.0, 0.0, 2.0, 1.0, 1.0], 1.0);
            b.mesh = Some(std::sync::Arc::new(cube(1.0 + gap, 2.0)));
            i.parts.extend([a, b]);
        })
    };
    assert_eq!(count(&mk(0.0), K_SLIVER), 0);
    assert_eq!(count(&mk(0.0004), K_SLIVER), 1);
}

#[test]
fn plate_thickness_against_the_thinnest_joined_element() {
    let mk = |thick: f64, joined: f64| {
        one(|i| {
            i.plates.push(Plate {
                key: key("solid", 9),
                thick,
                joined: vec![(key("solid", 1), 0.05), (key("solid", 2), joined)],
            })
        })
    };
    assert_eq!(count(&mk(0.04, 0.01), K_PLATE), 0);
    assert_eq!(count(&mk(0.02, 0.005), K_PLATE), 0);
    assert_eq!(count(&mk(0.02, 0.004), K_PLATE), 1);
    assert_eq!(count(&mk(0.021, 0.005), K_PLATE), 1);
    assert_eq!(count(&mk(0.03, 0.005), K_PLATE), 1);
}

fn bolt(d: f64, hole: Option<f64>, edge: Option<f64>, pitch: Option<f64>) -> Input {
    one(|i| i.bolts.push(Bolt { key: key("solid", 5), d, hole, edge, pitch }))
}

#[test]
fn bolt_diameter_comes_from_the_series() {
    assert_eq!(count(&bolt(0.020, None, None, None), K_BOLT_D), 0);
    assert_eq!(count(&bolt(0.021, None, None, None), K_BOLT_D), 1);
}

#[test]
fn bolt_hole_is_the_diameter_plus_a_clearance() {
    assert_eq!(count(&bolt(0.020, Some(0.022), None, None), K_BOLT_HOLE), 0);
    assert_eq!(count(&bolt(0.020, Some(0.0201), None, None), K_BOLT_HOLE), 1);
    assert_eq!(count(&bolt(0.020, Some(0.026), None, None), K_BOLT_HOLE), 1);
}

#[test]
fn bolt_edge_and_pitch_have_minimums() {
    // hole 22 mm: edge >= 26.4 mm, pitch >= 48.4 mm
    let ok = bolt(0.020, Some(0.022), Some(0.030), Some(0.050));
    assert_eq!(count(&ok, K_BOLT_EDGE) + count(&ok, K_BOLT_PITCH), 0);
    let bad = bolt(0.020, Some(0.022), Some(0.020), Some(0.040));
    assert_eq!(count(&bad, K_BOLT_EDGE), 1);
    assert_eq!(count(&bad, K_BOLT_PITCH), 1);
}

#[test]
fn rebar_cover_is_not_below_the_bar_or_the_floor() {
    let mk = |d: f64, cover: f64| {
        let k = key("rebar_set", 1);
        one(|i| i.rebar.push(Rebar { key: k.clone(), d, cover: Some(cover), spacing: None }))
    };
    assert_eq!(count(&mk(0.016, 0.025), K_COVER), 0);
    assert_eq!(count(&mk(0.016, 0.010), K_COVER), 1);
    assert_eq!(count(&mk(0.032, 0.020), K_COVER), 1);
}

#[test]
fn stirrup_spacing_stays_within_limits() {
    let mk = |s: (f64, f64)| {
        let k = key("rebar_set", 1);
        one(|i| i.rebar.push(Rebar { key: k.clone(), d: 0.008, cover: None, spacing: Some(s) }))
    };
    assert_eq!(count(&mk((0.15, 0.15)), K_STIRRUP), 0);
    assert_eq!(count(&mk((0.10, 0.30)), K_STIRRUP), 0);
    assert_eq!(count(&mk((0.05, 0.40)), K_STIRRUP), 1);
    assert_eq!(count(&mk((0.04, 0.10)), K_STIRRUP), 1);
}

#[test]
fn stair_riser_and_tread_limits() {
    let mk = |riser: f64, tread: f64| {
        one(|i| i.stairs.push(Stair { key: key("stair", 1), riser, tread }))
    };
    assert_eq!(analyze(&mk(0.17, 0.28)).len(), 0);
    assert_eq!(count(&mk(0.25, 0.28), K_RISER), 1);
    assert_eq!(count(&mk(0.17, 0.15), K_TREAD), 1);
}

fn floating(parts: &[(i64, f64, f64)], supports: Vec<[f64; 6]>) -> Input {
    one(|i| {
        i.ground_z = Some(0.0);
        i.supports = supports;
        for (id, z0, z1) in parts {
            i.parts.push(part(*id, [0.0, 0.0, *z0, 1.0, 1.0, *z1], 1.0));
        }
    })
}

#[test]
fn parts_on_the_ground_or_chained_to_it_do_not_float() {
    assert_eq!(count(&floating(&[(1, 0.0, 1.0)], vec![]), K_FLOAT), 0);
    let chain = floating(&[(1, 0.0, 1.0), (2, 1.0, 2.0), (3, 2.0, 3.0)], vec![]);
    assert_eq!(count(&chain, K_FLOAT), 0);
    let on_slab = floating(&[(1, 2.0, 3.0)], vec![[-1.0, -1.0, 1.0, 2.0, 2.0, 2.0]]);
    assert_eq!(count(&on_slab, K_FLOAT), 0);
}

#[test]
fn a_part_in_the_air_floats_once_per_group() {
    let i = floating(&[(1, 0.0, 1.0), (2, 2.0, 3.0), (3, 3.0, 4.0)], vec![]);
    let r = analyze(&i);
    let f: Vec<_> = r.iter().filter(|x| x.msg_key == K_FLOAT).collect();
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].keys[0], key("solid", 2));
    assert_eq!(f[0].args[1].1, "2");
}

#[test]
fn no_level_means_no_floating_verdict() {
    let mut i = floating(&[(1, 5.0, 6.0)], vec![]);
    i.ground_z = None;
    assert_eq!(count(&i, K_FLOAT), 0);
}

#[test]
fn every_issue_has_a_catalog_key_and_a_measure() {
    let mut i = Input::default();
    let mut m = member(1, &[[0.0; 3], [5.0, 0.0, 0.0]]);
    m.radius = Some(2.0);
    i.members.push(m);
    i.parts.push(part(1, [0.0; 6], 0.001));
    for x in analyze(&i) {
        assert!(KEYS.contains(&x.msg_key));
        assert!(x.measure.is_some());
        assert!(x.severity <= Severity::Warning);
    }
}

#[test]
fn the_same_input_gives_the_same_findings_in_the_same_order() {
    let mut i = node([1.0, 0.0, 0.0], false);
    i.parts.push(part(1, [0.0; 6], 0.001));
    i.parts.push(part(2, [5.0, 5.0, 5.0, 6.0, 6.0, 6.0], 0.001));
    let a = format!("{:?}", analyze(&i));
    let b = format!("{:?}", analyze(&i));
    assert!(!analyze(&i).is_empty());
    assert_eq!(a, b);
}

// ---- over a model with real solids ----

fn i_section() -> Section {
    // 200 x 100 I with 8 mm flanges and a 5 mm web
    let (h, b, tf, tw) = (0.2, 0.1, 0.008, 0.005);
    let pts = [
        (-b / 2.0, -h / 2.0), (b / 2.0, -h / 2.0), (b / 2.0, -h / 2.0 + tf),
        (tw / 2.0, -h / 2.0 + tf), (tw / 2.0, h / 2.0 - tf), (b / 2.0, h / 2.0 - tf),
        (b / 2.0, h / 2.0), (-b / 2.0, h / 2.0), (-b / 2.0, h / 2.0 - tf),
        (-tw / 2.0, h / 2.0 - tf), (-tw / 2.0, -h / 2.0 + tf), (-b / 2.0, -h / 2.0 + tf),
    ];
    Section::polygon(pts.iter().map(|p| v2(p.0, p.1)).collect())
}

fn solid(level: i64, material: &str, form: Form) -> SolidSpec {
    SolidSpec {
        level,
        name: "s".into(),
        void: false,
        material: material.into(),
        ifc: None,
        frame: Frame::default(),
        form,
    }
}

fn arc_sweep() -> Form {
    Form::Sweep {
        profile: i_section(),
        path: Path3 {
            pts: vec![v3(0.0, 0.0, 0.0), v3(6.0, 0.0, 0.0)],
            bulges: vec![0.4],
            smooth: None,
        },
    }
}

fn bent_count(m: &Model, k: &str) -> usize {
    critique(m, &CheckScope::All).unwrap().iter().filter(|f| f.msg_key == k).count()
}

#[test]
fn a_swept_i_section_on_an_arc_is_a_bent_straight_profile_until_typed() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let id = m.add_solid(&solid(l, "S355", arc_sweep())).unwrap();
    assert_eq!(bent_count(&m, K_BENT), 1);
    // chord 6 m, bulge 0.4: radius 6 * 1.16 / 1.6 = 4.35 m, under the 5 m cold minimum
    m.update_solid(id, &solid(l, "S355;bent=cold", arc_sweep())).unwrap();
    assert_eq!(bent_count(&m, K_BENT), 0);
    assert_eq!(bent_count(&m, K_RADIUS), 1);
    m.update_solid(id, &solid(l, "S355;bent=hot", arc_sweep())).unwrap();
    assert_eq!(bent_count(&m, K_RADIUS), 0);
}

#[test]
fn straight_extruded_members_with_met_axes_are_clean() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let ext = |z1: f64| Form::Extrusion { profile: i_section(), start: 0.0, end: z1 };
    m.add_solid(&solid(l, "S355", ext(3.0))).unwrap();
    let r = critique(&m, &CheckScope::All).unwrap();
    assert!(r.iter().all(|f| !KEYS[..3].contains(&f.msg_key)), "{r:?}");
}

#[test]
fn a_revolved_ring_is_a_bent_member_with_its_radius() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let ring = |mat: &str| {
        solid(
            l,
            mat,
            Form::Revolve {
                profile: i_section(),
                axis_o: v2(-3.0, 0.0),
                axis_d: v2(0.0, 1.0),
                start: 0.0,
                end: 2.0 * std::f64::consts::PI,
            },
        )
    };
    let id = m.add_solid(&ring("S355")).unwrap();
    assert_eq!(bent_count(&m, K_BENT), 1);
    m.update_solid(id, &ring("S355;bent=cold")).unwrap();
    assert_eq!(bent_count(&m, K_BENT), 0);
    assert_eq!(bent_count(&m, K_RADIUS), 1, "a 200 mm I cold-bent needs 5 m");
    m.update_solid(id, &ring("S355;bent=hot")).unwrap();
    assert_eq!(bent_count(&m, K_BENT) + bent_count(&m, K_RADIUS), 0);
}

#[test]
fn tags_do_not_change_the_material() {
    let (base, t) = super::realism_input::tags_of("S355;bent=cold;ecc;bolt=M20;hole=22");
    assert_eq!(base, "S355");
    assert_eq!(t.bend, Some(BendMethod::Cold));
    assert!(t.ecc);
    assert_eq!(t.bolt, Some(Some(0.020)));
    assert_eq!(t.hole, Some(0.022));
    assert_eq!(super::realism_input::tags_of("concrete").1, Default::default());
}

#[test]
fn an_empty_model_is_clean_and_the_rule_is_registered() {
    let m = Model::open(":memory:").unwrap();
    assert!(critique(&m, &CheckScope::All).unwrap().is_empty());
    assert!(super::rules().iter().any(|r| r.id() == "realism"));
}

// ---- gate: gazebo and sphere fixtures ----

fn column(m: &mut Model, l: i64, x: f64, y: f64) -> i64 {
    let mut s = solid(l, "S355", Form::Extrusion { profile: i_section(), start: 0.0, end: 3.0 });
    s.frame.o = v3(x, y, 0.0);
    m.add_solid(&s).unwrap()
}

/// A beam along x at height 3 m, its axis starting at `x0`.
fn beam(l: i64, x0: f64, y: f64, len: f64, mat: &str) -> SolidSpec {
    let mut s = solid(l, mat, Form::Extrusion { profile: i_section(), start: 0.0, end: len });
    s.frame = Frame {
        o: v3(x0, y, 3.0),
        x: v3(0.0, 1.0, 0.0),
        y: v3(0.0, 0.0, 1.0),
        z: v3(1.0, 0.0, 0.0),
    };
    s
}

fn left_over(m: &Model, keys: &[&str]) -> Vec<String> {
    critique(m, &CheckScope::All)
        .unwrap()
        .into_iter()
        .filter(|f| keys.contains(&f.msg_key))
        .map(|f| format!("{} {:?}", f.msg_key, f.args))
        .collect()
}

#[test]
fn gazebo_has_no_bent_straight_profile_and_no_missed_node_after_correction() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    for (x, y) in [(0.0, 0.0), (4.0, 0.0), (0.0, 4.0), (4.0, 4.0)] {
        column(&mut m, l, x, y);
    }
    // first draft: a rolled rafter drawn bent, and a beam whose axis stops 20 mm short of the node
    let rafter = m.add_solid(&solid(l, "S355", arc_sweep())).unwrap();
    let eave = m.add_solid(&beam(l, 0.02, 0.0, 3.98, "S355")).unwrap();
    let first = left_over(&m, &[K_BENT, K_AXIS]);
    assert!(first.iter().any(|f| f.starts_with(K_BENT)), "{first:?}");
    assert!(first.iter().any(|f| f.starts_with(K_AXIS)), "{first:?}");
    // the corrections the findings ask for
    m.update_solid(rafter, &solid(l, "S355;bent=hot", arc_sweep())).unwrap();
    m.update_solid(eave, &beam(l, 0.0, 0.0, 4.0, "S355")).unwrap();
    assert_eq!(left_over(&m, &[K_BENT, K_AXIS]), Vec::<String>::new());
}

#[test]
fn a_sphere_of_revolution_is_not_a_bent_profile() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let n = 24;
    let r = 1.0;
    let pts = (0..=n)
        .map(|i| {
            let a = std::f64::consts::PI * i as f64 / n as f64 - std::f64::consts::FRAC_PI_2;
            v2(r * a.cos(), r * a.sin())
        })
        .collect();
    m.add_solid(&solid(
        l,
        "glass",
        Form::Revolve {
            profile: Section::polygon(pts),
            axis_o: v2(0.0, 0.0),
            axis_d: v2(0.0, 1.0),
            start: 0.0,
            end: 2.0 * std::f64::consts::PI,
        },
    ))
    .unwrap();
    assert_eq!(left_over(&m, &[K_BENT, K_AXIS]), Vec::<String>::new());
}
