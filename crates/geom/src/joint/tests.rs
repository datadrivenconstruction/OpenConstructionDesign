use super::*;

const Z: Vec3 = [0.0, 0.0, 1.0];
const X: Vec3 = [1.0, 0.0, 0.0];

fn fr(o: Vec3, z: Vec3) -> JFrame {
    JFrame::new(o, z, X).unwrap()
}

fn joint(kind: JointKind, a: usize, b: Option<usize>, at: Vec3, axis: Vec3) -> Joint {
    Joint {
        kind,
        a,
        b,
        fa: fr(at, axis),
        fb: fr(at, axis),
        rot: None,
        slide: None,
    }
}

/// Parts at the identity pose, so world points are also part points.
fn asm(parts: usize, joints: Vec<Joint>) -> Assembly {
    Assembly {
        poses: vec![Pose::IDENTITY; parts],
        grounded: vec![false; parts],
        joints,
    }
}

fn near(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() < tol, "{a} != {b}");
}

#[test]
fn one_joint_to_the_ground_leaves_its_own_freedom() {
    for kind in JointKind::ALL {
        let a = asm(1, vec![joint(kind, 0, None, [0.3, 0.2, 0.1], Z)]);
        let d = a.dof().unwrap();
        assert_eq!(d.dof, kind.freedom(), "{kind:?}");
        assert!(d.redundant.is_empty(), "{kind:?}");
        assert_eq!(d.moving.is_empty(), kind == JointKind::Rigid);
    }
    assert_eq!(asm(1, vec![]).dof().unwrap().dof, 6);
    let mut g = asm(2, vec![]);
    g.grounded[0] = true;
    assert_eq!(g.dof().unwrap().dof, 6);
}

#[test]
fn chains_add_freedom() {
    // double pendulum
    let a = asm(2, vec![
        joint(JointKind::Revolute, 0, None, [0.0; 3], Z),
        joint(JointKind::Revolute, 1, Some(0), [1.0, 0.0, 0.0], Z),
    ]);
    assert_eq!(a.dof().unwrap().dof, 2);
    // ball then slider
    let a = asm(2, vec![
        joint(JointKind::Ball, 0, None, [0.0; 3], Z),
        joint(JointKind::Slider, 1, Some(0), [1.0, 0.0, 0.0], X),
    ]);
    assert_eq!(a.dof().unwrap().dof, 4);
}

#[test]
fn four_bar_linkage_has_one_degree_of_freedom() {
    // Gruebler in space gives 6*3 - 5*4 = -2; the rank shows the true mobility
    let (o1, p, q, o2) = ([0.0; 3], [1.0, 2.0, 0.0], [4.0, 3.0, 0.0], [4.0, 0.0, 0.0]);
    let a = asm(3, vec![
        joint(JointKind::Revolute, 0, None, o1, Z),
        joint(JointKind::Revolute, 1, Some(0), p, Z),
        joint(JointKind::Revolute, 2, Some(1), q, Z),
        joint(JointKind::Revolute, 2, None, o2, Z),
    ]);
    let d = a.dof().unwrap();
    assert_eq!(d.dof, 1);
    assert_eq!(d.equations, 20);
    assert_eq!(d.rank, 17);
    assert!(!d.redundant.is_empty());
    assert_eq!(d.moving, vec![0, 1, 2]);
}

#[test]
fn slider_crank_has_one_degree_of_freedom() {
    let (o, p, q) = ([0.0; 3], [0.0, 1.0, 0.0], [2.0, 0.0, 0.0]);
    let a = asm(3, vec![
        joint(JointKind::Revolute, 0, None, o, Z),
        joint(JointKind::Revolute, 1, Some(0), p, Z),
        joint(JointKind::Revolute, 2, Some(1), q, Z),
        joint(JointKind::Slider, 2, None, q, X),
    ]);
    assert_eq!(a.dof().unwrap().dof, 1);
}

#[test]
fn redundant_and_locking_joints() {
    // two coaxial hinges on one door: still one freedom, the second hinge is redundant
    let a = asm(1, vec![
        joint(JointKind::Revolute, 0, None, [0.0; 3], Z),
        joint(JointKind::Revolute, 0, None, [0.0, 0.0, 2.0], Z),
    ]);
    let d = a.dof().unwrap();
    assert_eq!(d.dof, 1);
    assert_eq!(d.redundant, vec![1]);
    // hinges on crossing axes lock the part
    let a = asm(1, vec![
        joint(JointKind::Revolute, 0, None, [0.0; 3], Z),
        joint(JointKind::Revolute, 0, None, [1.0, 0.0, 0.0], X),
    ]);
    assert_eq!(a.dof().unwrap().dof, 0);
    assert!(matches!(
        asm(1, vec![joint(JointKind::Ball, 0, Some(0), [0.0; 3], Z)]).dof(),
        Err(JointError::SamePart(0))
    ));
}

#[test]
fn drive_a_hinge_within_limits() {
    let mut a = asm(1, vec![joint(JointKind::Revolute, 0, None, [1.0, 0.0, 0.0], Z)]);
    let r = a.drive(0, Coord::Angle, 30f64.to_radians(), &[]).unwrap();
    let p = r.poses[0];
    near(p.yaw(1e-9).unwrap(), 30f64.to_radians(), 1e-9);
    // the hinge point stays
    let h = p.apply([1.0, 0.0, 0.0]);
    near(h[0], 1.0, 1e-9);
    near(h[1], 0.0, 1e-9);
    a.joints[0].rot = Some((0.0, 20f64.to_radians()));
    let r = a.drive(0, Coord::Angle, 30f64.to_radians(), &[]).unwrap();
    near(r.value, 20f64.to_radians(), 1e-12);
    a.poses = r.poses;
    near(a.coordinate(0, Coord::Angle).unwrap(), 20f64.to_radians(), 1e-9);
    assert_eq!(a.drive(0, Coord::Slide, 1.0, &[]), Err(JointError::NoCoordinate));
}

#[test]
fn drive_a_slider_and_a_cylinder() {
    let mut a = asm(1, vec![joint(JointKind::Slider, 0, None, [0.0; 3], Z)]);
    a.joints[0].slide = Some((-1.0, 1.0));
    let r = a.drive(0, Coord::Slide, 0.5, &[]).unwrap();
    near(r.poses[0].t[2], 0.5, 1e-9);
    near(r.poses[0].t[0], 0.0, 1e-9);
    let r = a.drive(0, Coord::Slide, 3.0, &[]).unwrap();
    near(r.poses[0].t[2], 1.0, 1e-9);
    let a = asm(1, vec![joint(JointKind::Cylindrical, 0, None, [0.0; 3], Z)]);
    let r = a.drive(0, Coord::Angle, 1.0, &[]).unwrap();
    near(r.poses[0].yaw(1e-9).unwrap(), 1.0, 1e-9);
    near(r.poses[0].t[2], 0.0, 1e-9);
}

#[test]
fn driving_the_crank_moves_the_whole_loop() {
    let (o1, p, q, o2) = ([0.0; 3], [1.0, 2.0, 0.0], [4.0, 3.0, 0.0], [4.0, 0.0, 0.0]);
    let mut a = asm(3, vec![
        joint(JointKind::Revolute, 0, None, o1, Z),
        joint(JointKind::Revolute, 1, Some(0), p, Z),
        joint(JointKind::Revolute, 2, Some(1), q, Z),
        joint(JointKind::Revolute, 2, None, o2, Z),
    ]);
    let r = a.drive(0, Coord::Angle, 10f64.to_radians(), &[]).unwrap();
    a.poses = r.poses.clone();
    assert!(a.residual() < 1e-9);
    near(a.coordinate(0, Coord::Angle).unwrap(), 10f64.to_radians(), 1e-9);
    // the rocker turned too, and every part stays in the plane
    assert!(a.coordinate(3, Coord::Angle).unwrap().abs() > 1e-3);
    for pose in &r.poses {
        assert!(pose.yaw(1e-9).is_some());
        near(pose.t[2], 0.0, 1e-9);
    }
    // a limit on the rocker stops the drive
    a.poses = vec![Pose::IDENTITY; 3];
    a.joints[3].rot = Some((-0.01, 0.01));
    assert!(matches!(
        a.drive(0, Coord::Angle, 10f64.to_radians(), &[]),
        Err(JointError::Limit { joint: 3, .. })
    ));
}

#[test]
fn frames_move_between_parts() {
    let pa = Pose::plan([1.0, 2.0, 0.5], 0.7);
    let pb = Pose::plan([-3.0, 0.0, 1.0], -1.2);
    let f = fr([0.2, 0.1, 0.0], [0.0, 1.0, 0.0]);
    let g = f.moved(&pa, &pb);
    let (wa, wb) = (pa.apply(f.o), pb.apply(g.o));
    for k in 0..3 {
        near(wa[k], wb[k], 1e-12);
        near(pa.dir(f.z)[k], pb.dir(g.z)[k], 1e-12);
    }
    // a hinge built that way is satisfied as placed
    let a = Assembly {
        poses: vec![pa, pb],
        grounded: vec![false, true],
        joints: vec![Joint {
            kind: JointKind::Revolute,
            a: 0,
            b: Some(1),
            fa: f,
            fb: g,
            rot: None,
            slide: None,
        }],
    };
    assert!(a.residual() < 1e-12);
    assert_eq!(a.dof().unwrap().dof, 1);
}
