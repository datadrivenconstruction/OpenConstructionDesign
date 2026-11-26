// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Form oracles on hand-built kernels: a twisted and a straight tower, a hyperboloid and a
//! cylinder, a dome, a sphere and a drum, a stepped pyramid and a tapered tower.

use super::case::Case;
use super::facts::{Facts, P3};
use super::form_oracles::{heading, turn};
use super::oracle::{evaluate, EvalCx, OracleResult};
use super::tests::{editor, sql};
use crate::editor::Editor;
use std::f64::consts::PI;

fn check_in(ed: &Editor, f: &Facts, kind: &str, params: &str) -> OracleResult {
    let c = Case::parse(&format!("id=\"t\"\n[[turn]]\nsay=\"x\"\n[[oracle.geom]]\nkind=\"{kind}\"\n{params}\n")).unwrap();
    let cx = EvalCx { ed, facts: f, before: None, answer: "", ifc: None, errors_before: 0, receipts: &[] };
    evaluate(&c, &cx).remove(0)
}

fn check(f: &Facts, kind: &str, params: &str) -> OracleResult {
    check_in(&editor(), f, kind, params)
}

/// A square tower of `floors` storeys 4 m apart, 36 m across, each turned `deg` from the one below.
fn tower(floors: usize, deg: f64) -> Editor {
    let ed = editor();
    sql(&ed, "INSERT INTO slab_type(id, name) VALUES(901, 'Floor')");
    for k in 0..floors {
        let (lv, id) = (2000 + k, 3000 + k);
        sql(&ed, &format!("INSERT INTO level(id, name, elevation) VALUES({lv}, 'T{k}', {})", 4.0 * k as f64));
        sql(&ed, &format!("INSERT INTO slab(id, level, stype, offset) VALUES({id}, {lv}, 901, 0)"));
        let t = (deg * k as f64).to_radians();
        // the start corner moves round each storey, as a rebuilt outline would
        for i in 0..4 {
            let a = t + PI / 4.0 + PI / 2.0 * ((i + k) % 4) as f64;
            let (x, y) = (18.0 * 2f64.sqrt() * a.cos(), 18.0 * 2f64.sqrt() * a.sin());
            sql(&ed, &format!("INSERT INTO slab_vertex(slab, ring, idx, x, y) VALUES({id}, 0, {i}, {x}, {y})"));
        }
    }
    ed
}

#[test]
fn headings_ignore_the_start_corner_and_wrap_a_quarter_turn() {
    let sq = |d: f64| -> Vec<[f64; 2]> {
        (0..4).map(|i| {
            let a = (d + 45.0 + 90.0 * i as f64).to_radians();
            [a.cos(), a.sin()]
        })
        .collect()
    };
    assert!(heading(&sq(10.0)).is_some_and(|h| (h - 10.0).abs() < 1e-9));
    let mut r = sq(10.0);
    r.rotate_left(1);
    r.reverse();
    assert!(heading(&r).is_some_and(|h| (h - 10.0).abs() < 1e-9));
    assert!((turn(44.0, -44.0) - 2.0).abs() < 1e-9);
    assert!((turn(-44.0, 44.0) + 2.0).abs() < 1e-9);
    let oct: Vec<[f64; 2]> = (0..8).map(|i| [(PI / 4.0 * i as f64).cos(), (PI / 4.0 * i as f64).sin()]).collect();
    assert!(heading(&oct).is_none(), "a regular octagon has no heading");
}

#[test]
fn a_twisted_tower_turns_each_storey_by_its_angle() {
    let f = Facts::default();
    let r = check_in(&tower(40, 1.5), &f, "twist_per_floor", "deg = 1.5");
    assert!(r.pass && r.measured.starts_with("39 of 39 steps") && r.measured.ends_with("total 58.5 deg"), "{r:?}");
    let r = check_in(&tower(40, -1.5), &f, "twist_per_floor", "deg = 1.5");
    assert!(!r.pass, "turned the other way: {r:?}");
    let r = check_in(&tower(40, 0.0), &f, "twist_per_floor", "deg = 1.5");
    assert!(!r.pass && r.measured.starts_with("0 of 39"), "a straight tower: {r:?}");
    assert!(!check_in(&tower(40, 3.0), &f, "twist_per_floor", "deg = 1.5").pass, "twice the angle");
    assert!(!check_in(&tower(2, 1.5), &f, "twist_per_floor", "deg = 1.5").pass, "one step is not a tower");
    assert!(check(&f, "twist_per_floor", "").measured.contains("missing parameter deg"));
}

/// Rings of `n` nodes every 10 m up to 120 m at the radius `r(z)`.
fn rings(r: impl Fn(f64) -> f64) -> Facts {
    let mut f = Facts::default();
    for i in 0..24 {
        let a = 2.0 * PI * i as f64 / 24.0;
        for k in 0..=12 {
            let z = 10.0 * k as f64;
            f.points.push([r(z) * a.cos(), r(z) * a.sin(), z]);
        }
    }
    f
}

#[test]
fn a_hyperboloid_has_a_waist_a_cylinder_and_a_cone_do_not() {
    // base 90 m, throat 55 m at 90 m, top about 60 m
    let k = (45f64.powi(2) - 27.5f64.powi(2)) / 8100.0;
    let hyp = rings(|z| (27.5f64.powi(2) + k * (z - 90.0).powi(2)).sqrt());
    let r = check(&hyp, "waist", "");
    assert!(r.pass && r.measured.contains("waist 27.5 m (0.61) at 0.79"), "{r:?}");
    assert!(!check(&rings(|_| 30.0), "waist", "").pass, "a cylinder");
    assert!(!check(&rings(|z| 45.0 - z / 6.0), "waist", "").pass, "a cone narrows to its top");
    assert!(!check(&hyp, "waist", "ratio_max = 0.5").pass);
    assert!(!check(&Facts::default(), "waist", "").pass);
}

fn sphere_pts(r: f64, zc: f64, elev: impl Iterator<Item = i32>) -> Vec<P3> {
    let e: Vec<i32> = elev.collect();
    let mut v = vec![];
    for i in 0..20 {
        let a = 2.0 * PI * i as f64 / 20.0;
        for d in &e {
            let t = (*d as f64).to_radians();
            v.push([r * t.cos() * a.cos(), r * t.cos() * a.sin(), zc + r * t.sin()]);
        }
    }
    v
}

#[test]
fn a_dome_and_a_sphere_keep_their_nodes_on_one_sphere() {
    let dome = Facts { points: sphere_pts(12.0, 0.0, (0..90).step_by(18)), ..Facts::default() };
    let r = check(&dome, "sphere_fit", "radius = 12.0");
    assert!(r.pass && r.measured.starts_with("radius 12.00 m, 1.00 of 100 nodes"), "{r:?}");
    assert!(!check(&dome, "sphere_fit", "radius = 20.0").pass, "the wrong radius");
    assert!(!check(&dome, "sphere_fit", "extent_min = 0.8").pass, "a dome is not a whole sphere");
    let ball = Facts { points: sphere_pts(15.0, 15.0, (-75..90).step_by(15)), ..Facts::default() };
    assert!(check(&ball, "sphere_fit", "radius = 15.0\nextent_min = 0.8").pass);
    // a drum of the same size: walls straight up
    let mut drum = Facts::default();
    for i in 0..16 {
        let a = 2.0 * PI * i as f64 / 16.0;
        for z in [0.0, 3.0, 6.0, 9.0, 12.0] {
            drum.points.push([12.0 * a.cos(), 12.0 * a.sin(), z]);
        }
    }
    assert!(!check(&drum, "sphere_fit", "").pass, "a drum");
    assert!(!check(&Facts::default(), "sphere_fit", "").pass);
}

/// Square storeys 4 m apart, the first `base` across, each set back `step` on every side.
fn terraces(n: usize, base: f64, step: f64) -> Editor {
    let ed = editor();
    sql(&ed, "INSERT INTO slab_type(id, name) VALUES(902, 'Terrace')");
    for k in 0..n {
        let (lv, id, h) = (4000 + k, 5000 + k, base / 2.0 - step * k as f64);
        sql(&ed, &format!("INSERT INTO level(id, name, elevation) VALUES({lv}, 'P{k}', {})", 4.0 * k as f64));
        sql(&ed, &format!("INSERT INTO slab(id, level, stype, offset) VALUES({id}, {lv}, 902, 0)"));
        for (i, (x, y)) in [(-h, -h), (h, -h), (h, h), (-h, h)].iter().enumerate() {
            sql(&ed, &format!("INSERT INTO slab_vertex(slab, ring, idx, x, y) VALUES({id}, 0, {i}, {x}, {y})"));
        }
    }
    ed
}

#[test]
fn a_stepped_pyramid_sets_each_terrace_back_by_its_step() {
    let f = Facts::default();
    let r = check_in(&terraces(7, 60.0, 4.0), &f, "setback_per_floor", "inset = 4.0");
    assert!(r.pass && r.measured == "6 of 6 steps on the set-back (1.00), 4.00..4.00 m", "{r:?}");
    assert!(!check_in(&terraces(7, 60.0, 0.0), &f, "setback_per_floor", "inset = 4.0").pass, "a block");
    assert!(!check_in(&terraces(7, 60.0, 2.0), &f, "setback_per_floor", "inset = 4.0").pass, "half the step");
    assert!(!check_in(&terraces(2, 60.0, 4.0), &f, "setback_per_floor", "inset = 4.0").pass, "one step");
    assert!(check(&f, "setback_per_floor", "").measured.contains("missing parameter inset"));
}

#[test]
fn a_tapered_tower_narrows_to_its_top_a_drum_does_not() {
    // 8 m across at the foot, 5 m at the top of 120 m
    let cone = rings(|z| 4.0 - 1.5 * z / 120.0);
    let r = check(&cone, "top_ratio", "min = 0.5\nmax = 0.75");
    assert!(r.pass && r.measured == "base radius 4.0 m, top 2.6 m (0.66)", "{r:?}");
    assert!(!check(&rings(|_| 4.0), "top_ratio", "min = 0.5\nmax = 0.75").pass, "a drum");
    assert!(!check(&Facts::default(), "top_ratio", "max = 0.75").pass);
}

/// A tower of wall rings only (no members): 10 storeys of 24 walls, 8 m across at the foot,
/// 5 m at the top of 40 m; with `house`, a keeper's house 6 x 6 m beside the foot.
fn wall_tower(house: bool) -> (Editor, Facts) {
    let ed = editor();
    sql(&ed, "INSERT INTO wall_type(id, name) VALUES(903, 'Masonry')");
    let wall = |lv: i64, [ax, ay, bx, by]: [f64; 4], h: f64| {
        sql(&ed, &format!(
            "INSERT INTO wall(level, wtype, ax, ay, bx, by, base_offset, height) VALUES({lv}, 903, {ax}, {ay}, {bx}, {by}, 0, {h})"
        ));
    };
    for k in 0..10 {
        let (z, lv) = (4.0 * k as f64, 6000 + k);
        sql(&ed, &format!("INSERT INTO level(id, name, elevation) VALUES({lv}, 'T{k}', {z})"));
        let r = 4.0 - 1.5 * z / 40.0;
        for i in 0..24 {
            let (a, b) = (2.0 * PI * i as f64 / 24.0, 2.0 * PI * (i + 1) as f64 / 24.0);
            wall(lv, [r * a.cos(), r * a.sin(), r * b.cos(), r * b.sin()], 4.0);
        }
    }
    if house {
        for w in [[5.0, -3.0, 11.0, -3.0], [11.0, -3.0, 11.0, 3.0], [11.0, 3.0, 5.0, 3.0], [5.0, 3.0, 5.0, -3.0]] {
            wall(6000, w, 3.0);
        }
    }
    let f = Facts::read(&ed.model).unwrap();
    (ed, f)
}

#[test]
fn top_ratio_reads_a_tower_built_of_walls_about_its_own_axis() {
    let (ed, f) = wall_tower(false);
    let alone = check_in(&ed, &f, "top_ratio", "min = 0.5
max = 0.8");
    assert!(alone.pass, "{alone:?}");
    let (ed, f) = wall_tower(true);
    let housed = check_in(&ed, &f, "top_ratio", "min = 0.5
max = 0.8");
    assert!(housed.pass && housed.measured == alone.measured, "{housed:?} vs {alone:?}");
}

#[test]
fn a_walled_tower_has_its_height() {
    let (ed, f) = wall_tower(true);
    assert_eq!(f.masses.len(), 244, "a group per wall");
    let h = check_in(&ed, &f, "height", "min = 39.9
max = 40.1");
    assert!(h.pass, "{h:?}");
}

/// A lighthouse shaft as one lofted solid: a 24-gon 8 m across at the foot, 5.2 m at 45 m; a
/// void (a stair well cut) reaching higher must not count.
#[test]
fn a_lofted_solid_tower_has_its_height_and_taper() {
    use ddc_geom::arc::BulgeRing;
    use ddc_geom::math::v2;
    use ddc_model::{SolidForm, SolidFrame, SolidSpec};
    let mut ed = editor();
    let lv = super::tests::level(&ed);
    let ring = |r: f64| BulgeRing {
        pts: (0..24).map(|i| (2.0 * PI * i as f64 / 24.0, r)).map(|(a, r)| v2(r * a.cos(), r * a.sin())).collect(),
        bulges: vec![0.0; 24],
    };
    let solid = |void: bool, top: f64, height: f64| SolidSpec {
        level: lv,
        name: "shaft".into(),
        void,
        material: "Masonry".into(),
        frame: SolidFrame::default(),
        form: SolidForm::Blend { bottom: ring(4.0), top: ring(top), height },
        ifc: None,
    };
    ed.model.add_solid(&solid(false, 2.6, 45.0)).unwrap();
    ed.model.add_solid(&solid(true, 1.0, 60.0)).unwrap();
    let f = Facts::read(&ed.model).unwrap();
    assert_eq!(f.masses.len(), 1, "the void is no mass");
    let z0 = f.z(lv, 0.0);
    let h = check_in(&ed, &f, "height", &format!("min = {}
max = {}", z0 + 44.9, z0 + 45.1));
    assert!(h.pass, "{h:?}");
    let r = check_in(&ed, &f, "top_ratio", "min = 0.6
max = 0.75");
    assert!(r.pass, "{r:?}");
    let drum = {
        let mut ed = editor();
        ed.model.add_solid(&solid(false, 4.0, 45.0)).unwrap();
        let f = Facts::read(&ed.model).unwrap();
        check_in(&ed, &f, "top_ratio", "min = 0.6
max = 0.75").pass
    };
    assert!(!drum, "a straight drum is no taper");
}

/// G33 end to end on the model: the tapered wall tower with a spiral stair up every storey
/// counts its stairs, its height and its taper.
#[test]
fn a_lighthouse_with_a_spiral_stair_meets_its_oracles() {
    use ddc_model::{StairShape, StairSpec};
    let (mut ed, _) = wall_tower(true);
    for k in 0..9 {
        let mut s = StairSpec::straight(6000 + k, 6001 + k, (0.0, 0.0), 0.0);
        s.shape = StairShape::Spiral { radius: 1.2, core: 0.3, turn: 6.0 };
        s.max_riser = 0.2;
        let id = ed.model.add_stair(&s).unwrap();
        assert!(ed.model.stair_quantities(id).unwrap().risers >= 20);
    }
    let f = Facts::read(&ed.model).unwrap();
    assert_eq!(f.count("stair"), Ok(9), "spiral flights are stair rows");
    for (kind, params) in [("height", "min = 36.0\nmax = 50.0"), ("top_ratio", "min = 0.3\nmax = 0.85")] {
        let r = check_in(&ed, &f, kind, params);
        assert!(r.pass, "{r:?}");
    }
}
