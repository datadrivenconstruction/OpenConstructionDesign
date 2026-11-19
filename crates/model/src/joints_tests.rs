// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::f64::consts::FRAC_PI_2;

use super::*;
use crate::{CompSpec, Placement};

const UP: [f64; 3] = [0.0, 0.0, 1.0];
const EAST: [f64; 3] = [1.0, 0.0, 0.0];

struct Fx {
    m: Model,
    lv: i64,
    ctype: i64,
}

fn fx(elev: f64) -> Fx {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", elev).unwrap();
    let family = m.load_library_family("Sink").unwrap();
    let ctype = m.comp_types(family).unwrap()[0].0;
    Fx { m, lv, ctype }
}

impl Fx {
    fn comp(&mut self, x: f64, y: f64) -> i64 {
        self.m.add_component(&CompSpec::at(self.lv, self.ctype, x, y)).unwrap()
    }

    fn joint(&mut self, kind: JointKind, a: i64, b: Option<i64>, o: [f64; 3], z: [f64; 3]) -> i64 {
        self.m
            .joint_add(&JointSpec {
                name: String::new(),
                kind,
                a,
                b,
                frame: JFrame::new(o, z, EAST).unwrap(),
                rot: None,
                slide: None,
            })
            .unwrap()
    }

    fn at(&self, c: i64) -> (f64, f64, f64, f64) {
        let s = self.m.component(c).unwrap();
        (s.at.0, s.at.1, s.at.2, s.rotation)
    }
}

fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-7, "{a} != {b}");
}

#[test]
fn step_53_adds_the_joint_table() {
    assert!(crate::schema::STEPS.contains(&53));
    let d = std::env::temp_dir().join(format!("ddc_pj1_mig_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("old.ddc");
    let ps = p.to_str().unwrap();
    {
        let m = Model::open(ps).unwrap();
        m.db.execute_batch("DROP TABLE asm_joint; ALTER TABLE comp DROP COLUMN tilt").unwrap();
        m.db.execute("UPDATE meta SET value=51 WHERE key='schema_version'", []).unwrap();
    }
    let m = Model::open(ps).unwrap();
    assert!(m.joints().unwrap().is_empty());
    // older instances stay upright: the orientation column arrives empty
    let tilt: i64 = m
        .db
        .query_row("SELECT count(*) FROM pragma_table_info('comp') WHERE name='tilt'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tilt, 1);
    let v: i64 = m
        .db
        .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, crate::schema::VERSION);
}

#[test]
fn hinge_on_the_ground_turns_the_instance_and_undo_puts_it_back() {
    let mut f = fx(3.0);
    let a = f.comp(0.0, 0.0);
    let j = f.joint(JointKind::Revolute, a, None, [1.0, 0.0, 3.0], UP);
    let d = f.m.assembly_dof(a).unwrap();
    assert_eq!((d.dof, d.equations, d.rank), (1, 5, 5));
    assert_eq!(d.moving, vec![a]);
    near(f.m.joint_value(j, Coord::Angle).unwrap().unwrap(), 0.0);
    assert_eq!(f.m.joint_value(j, Coord::Slide).unwrap(), None);
    let n = f.m.journal_len().unwrap();
    near(f.m.joint_drive(j, Coord::Angle, FRAC_PI_2).unwrap(), FRAC_PI_2);
    assert_eq!(f.m.journal_len().unwrap(), n + 1);
    let (x, y, z, r) = f.at(a);
    near(x, 1.0);
    near(y, -1.0);
    near(z, 0.0);
    near(r, FRAC_PI_2);
    near(f.m.joint_value(j, Coord::Angle).unwrap().unwrap(), FRAC_PI_2);
    assert!(f.m.assembly_dof(a).unwrap().residual < 1e-9);
    f.m.undo().unwrap();
    assert_eq!(f.at(a), (0.0, 0.0, 0.0, 0.0));
}

#[test]
fn limits_clamp_a_drive_and_are_checked() {
    let mut f = fx(0.0);
    let a = f.comp(0.0, 0.0);
    let s = f.joint(JointKind::Slider, a, None, [0.0, 0.0, 0.0], EAST);
    assert!(f.m.joint_edit(s, "Rail", Some((0.0, 1.0)), None).is_err());
    assert!(f.m.joint_edit(s, "Rail", None, Some((1.0, -1.0))).is_err());
    f.m.joint_edit(s, "Rail", None, Some((-0.5, 0.5))).unwrap();
    assert_eq!(f.m.joint(s).unwrap().name, "Rail");
    near(f.m.joint_drive(s, Coord::Slide, 2.0).unwrap(), 0.5);
    let (x, y, _, r) = f.at(a);
    near(x, 0.5);
    near(y, 0.0);
    near(r, 0.0);
    assert!(f.m.joint_drive(s, Coord::Angle, 1.0).is_err());
}

fn world(m: &Model, c: i64, p: (f64, f64, f64)) -> (f64, f64, f64) {
    let s = m.component(c).unwrap();
    let el = m.db.query_row("SELECT elevation FROM level WHERE id=?1", [s.level], |r| r.get::<_, f64>(0)).unwrap();
    Placement {
        origin: (s.at.0, s.at.1, el + s.at.2),
        rotation: s.rotation,
        mirror: false,
        tilt: s.tilt,
    }
    .apply(p)
}

#[test]
fn door_on_a_horizontal_hinge_tilts_and_undo_stands_it_up() {
    let mut f = fx(3.0);
    let a = f.comp(2.0, 1.0);
    // hinge along east at the instance's top edge, one metre above its base
    let j = f.joint(JointKind::Revolute, a, None, [2.0, 1.0, 4.0], EAST);
    let n = f.m.journal_len().unwrap();
    near(f.m.joint_drive(j, Coord::Angle, 0.3).unwrap(), 0.3);
    assert_eq!(f.m.journal_len().unwrap(), n + 1);
    let s = f.m.component(a).unwrap();
    assert!(s.tilt.is_some());
    near(s.rotation, 0.0);
    // the free edge swings about the hinge line; the hinge line stays put
    let (x, y, z) = world(&f.m, a, (0.5, 0.0, 0.0));
    near(x, 2.5);
    near(y, 1.0 + 0.3f64.sin());
    near(z, 4.0 - 0.3f64.cos());
    let h = world(&f.m, a, (0.7, 0.0, 1.0));
    near(h.0, 2.7);
    near(h.1, 1.0);
    near(h.2, 4.0);
    near(f.m.joint_value(j, Coord::Angle).unwrap().unwrap(), 0.3);
    // a tilted instance reads back its full orientation: the next drive starts from it
    near(f.m.joint_drive(j, Coord::Angle, -0.5).unwrap(), -0.5);
    let (_, y, z) = world(&f.m, a, (0.0, 0.0, 0.0));
    near(y, 1.0 - 0.5f64.sin());
    near(z, 4.0 - 0.5f64.cos());
    // derived placement and bounds follow the tilt
    let q = f.m.component_quantities(a).unwrap();
    assert_eq!(q.placement.tilt, f.m.component(a).unwrap().tilt);
    f.m.undo().unwrap();
    f.m.undo().unwrap();
    assert_eq!(f.m.component(a).unwrap().tilt, None);
    assert_eq!(f.at(a), (2.0, 1.0, 0.0, 0.0));
}

#[test]
fn crank_slider_has_one_freedom_and_the_slider_follows_the_crank() {
    let mut f = fx(0.0);
    // crank 1 m, rod 2 m, slider on the east line through the crank pivot
    let crank = f.comp(0.5, 0.0);
    let rod = f.comp(2.0, 0.0);
    let slider = f.comp(3.0, 0.0);
    let j0 = f.joint(JointKind::Revolute, crank, None, [0.0, 0.0, 0.0], UP);
    f.joint(JointKind::Revolute, rod, Some(crank), [1.0, 0.0, 0.0], UP);
    f.joint(JointKind::Revolute, slider, Some(rod), [3.0, 0.0, 0.0], UP);
    let js = f.joint(JointKind::Slider, slider, None, [3.0, 0.0, 0.0], EAST);
    let d = f.m.assembly_dof(rod).unwrap();
    assert_eq!((d.comps.len(), d.joints.len()), (3, 4));
    assert_eq!(d.dof, 1, "{d:?}");
    for th in [0.4, 1.2, 2.0] {
        near(f.m.joint_drive(j0, Coord::Angle, th).unwrap(), th);
        let xs = th.cos() + (4.0 - th.sin().powi(2)).sqrt();
        let (x, y, z, rot) = f.at(slider);
        near(x, xs);
        near(y, 0.0);
        near(z, 0.0);
        near(rot, 0.0);
        near(f.m.joint_value(js, Coord::Slide).unwrap().unwrap(), xs - 3.0);
        assert!(f.m.assembly_dof(crank).unwrap().residual < 1e-7);
    }
}

#[test]
fn plan_four_bar_has_one_freedom_and_drives_all_links() {
    let mut f = fx(0.0);
    let crank = f.comp(0.0, 0.5);
    let coupler = f.comp(1.5, 1.2);
    let rocker = f.comp(3.0, 0.7);
    let j0 = f.joint(JointKind::Revolute, crank, None, [0.0, 0.0, 0.0], UP);
    f.joint(JointKind::Revolute, coupler, Some(crank), [0.0, 1.0, 0.0], UP);
    f.joint(JointKind::Revolute, rocker, Some(coupler), [3.0, 1.5, 0.0], UP);
    f.joint(JointKind::Revolute, rocker, None, [3.0, 0.0, 0.0], UP);
    let d = f.m.assembly_dof(coupler).unwrap();
    assert_eq!(d.comps.len(), 3);
    assert_eq!(d.joints.len(), 4);
    // parallel axes repeat three equations: 18 - 17 = 1
    assert_eq!((d.dof, d.equations, d.rank), (1, 20, 17));
    assert!(!d.redundant.is_empty());
    let before: Vec<_> = [crank, coupler, rocker].iter().map(|c| f.at(*c)).collect();
    near(f.m.joint_drive(j0, Coord::Angle, -0.2).unwrap(), -0.2);
    assert!(f.m.assembly_dof(crank).unwrap().residual < 1e-7);
    near(f.m.joint_value(j0, Coord::Angle).unwrap().unwrap(), -0.2);
    for (c, b) in [crank, coupler, rocker].iter().zip(&before) {
        assert_ne!(f.at(*c), *b);
    }
    f.m.undo().unwrap();
    let after: Vec<_> = [crank, coupler, rocker].iter().map(|c| f.at(*c)).collect();
    assert_eq!(after, before);
}

#[test]
fn rigid_and_free_instances_count_right() {
    let mut f = fx(0.0);
    let a = f.comp(0.0, 0.0);
    let b = f.comp(2.0, 0.0);
    let lone = f.comp(9.0, 0.0);
    assert_eq!(f.m.assembly_dof(lone).unwrap().dof, 6);
    f.joint(JointKind::Ball, b, Some(a), [1.0, 0.0, 0.0], UP);
    assert_eq!(f.m.assembly_dof(a).unwrap().dof, 9);
    f.joint(JointKind::Rigid, a, None, [0.0, 0.0, 0.0], UP);
    let d = f.m.assembly_dof(b).unwrap();
    assert_eq!(d.dof, 3);
    assert_eq!(d.moving, vec![b]);
    assert_eq!(f.m.joints_of(lone).unwrap(), vec![]);
}

#[test]
fn bad_joints_are_refused_and_deleting_an_instance_drops_its_joints() {
    let mut f = fx(0.0);
    let a = f.comp(0.0, 0.0);
    let b = f.comp(2.0, 0.0);
    let spec = |kind, b| JointSpec {
        name: "J".into(),
        kind,
        a,
        b,
        frame: JFrame::new([1.0, 0.0, 0.0], UP, EAST).unwrap(),
        rot: None,
        slide: None,
    };
    assert!(f.m.joint_add(&spec(JointKind::Revolute, Some(a))).is_err());
    assert!(f.m.joint_add(&spec(JointKind::Revolute, Some(999))).is_err());
    let mut s = spec(JointKind::Ball, Some(b));
    s.slide = Some((0.0, 1.0));
    assert!(f.m.joint_add(&s).is_err());
    let mut mirrored = CompSpec::at(f.lv, f.ctype, 5.0, 0.0);
    mirrored.mirror = true;
    let mc = f.m.add_component(&mirrored).unwrap();
    assert!(f.m.joint_add(&spec(JointKind::Revolute, Some(mc))).is_err());
    assert!(f.m.joints().unwrap().is_empty());

    let j = f.m.joint_add(&spec(JointKind::Cylindrical, Some(b))).unwrap();
    f.m.delete_component(b).unwrap();
    assert!(f.m.joints().unwrap().is_empty());
    f.m.undo().unwrap();
    assert_eq!(f.m.joints().unwrap()[0].id, j);
    f.m.joint_delete(j).unwrap();
    assert!(f.m.joints().unwrap().is_empty());
    f.m.undo().unwrap();
    assert_eq!(f.m.assembly_dof(a).unwrap().dof, 8);
}

#[test]
fn a_role_comp_dropped_with_its_solid_leaves_no_joint_and_undo_restores_it() {
    let mut f = fx(3.0);
    let a = f.comp(0.0, 0.0);
    f.joint(JointKind::Revolute, a, None, [1.0, 0.0, 3.0], UP);
    f.m.db.execute("INSERT INTO solid_comp(solid, comp) VALUES (9, ?1)", [a]).unwrap();
    f.m.op("test_drop", |db| Ok(((), 9, crate::recipe::drop_role_comp(db, 9, crate::ops::Inv::default())?)))
        .unwrap();
    assert!(f.m.joints().unwrap().is_empty());
    assert!(f.m.component(a).is_err());
    f.m.undo().unwrap();
    assert_eq!(f.m.joints().unwrap().len(), 1);
    assert!(f.m.component(a).is_ok());
}

#[test]
fn steel_connection_removal_goes_through_the_role_comp_drop() {
    let src = include_str!("steel_conn.rs");
    assert_eq!(src.matches("drop_role_comp(db, ").count(), 2);
    assert_eq!(src.matches("DELETE FROM solid WHERE id=?1").count(), 2);
}
