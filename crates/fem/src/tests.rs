//! Closed-form checks of the frame solver.

use crate::*;
use Restraint::{Fixed as X, Free as O};

const E: f64 = 210e9;
const G: f64 = 81e9;
const IY: f64 = 8e-5;
const IZ: f64 = 2e-5;
const J: f64 = 1e-6;
const A: f64 = 5e-3;

fn frame(nodes: Vec<[f64; 3]>) -> Frame {
    Frame {
        nodes,
        materials: vec![Material { e: E, g: G, density: 7850.0 }],
        sections: vec![Section::new(A, IY, IZ, J)],
        ..Frame::default()
    }
}

fn case(name: &str, loads: Vec<Load>) -> LoadCase {
    LoadCase { name: name.into(), loads }
}

fn run(fr: &Frame) -> Solution {
    solve(fr, &Options::default()).unwrap()
}

#[track_caller]
fn close(got: f64, want: f64, tol: f64) {
    let err = (got - want).abs();
    assert!(err <= tol * want.abs().max(1e-300), "got {got}, want {want}, rel {}", err / want);
}

#[track_caller]
fn tiny(got: f64, scale: f64) {
    assert!(got.abs() <= 1e-9 * scale, "got {got}, expected ~0 at scale {scale}");
}

fn down(w: f64) -> [f64; 3] {
    [0.0, 0.0, -w]
}

fn two_udl(q: f64) -> Vec<Load> {
    vec![Load::udl(0, down(q), Axes::Global), Load::udl(1, down(q), Axes::Global)]
}

fn cantilever(l: f64) -> Frame {
    let mut fr = frame(vec![[0.0; 3], [l, 0.0, 0.0]]);
    fr.members.push(Member::new(0, 1, 0, 0));
    fr.supports.push(Support::fixed(0));
    fr
}

#[test]
fn simply_supported_udl() {
    let (l, q) = (6.0f64, 10e3);
    let mut fr = frame(vec![[0.0; 3], [3.0, 0.0, 0.0], [6.0, 0.0, 0.0]]);
    fr.members = vec![Member::new(0, 1, 0, 0), Member::new(1, 2, 0, 0)];
    fr.supports = vec![
        Support::new(0, [X, X, X, X, O, O]),
        Support::new(2, [O, X, X, O, O, O]),
    ];
    fr.cases.push(case("q", two_udl(q)));
    let s = run(&fr);
    let r = &s.cases[0];
    close(r.disp[1][2], -5.0 * q * l.powi(4) / (384.0 * E * IY), 1e-10);
    close(r.disp[0][4], q * l.powi(3) / (24.0 * E * IY), 1e-10);
    for (mi, dg) in r.diagrams.iter().enumerate() {
        for (x, f) in dg.x.iter().zip(&dg.f) {
            let gx = 3.0 * mi as f64 + x;
            let m = q * gx * (l - gx) / 2.0;
            assert!((f[4] + m).abs() <= 1e-9 * q * l * l, "My at {gx}: {} vs {}", f[4], -m);
            assert!((f[2] - (-q * (l / 2.0 - gx))).abs() <= 1e-9 * q * l);
        }
    }
    close(r.diagrams[0].f.last().unwrap()[4], -q * l * l / 8.0, 1e-10);
    close(r.summary.max_moment, q * l * l / 8.0, 1e-10);
    close(r.reactions[0].1[2], q * l / 2.0, 1e-10);
    assert!(r.equilibrium.relative < 1e-12);
}

#[test]
fn simply_supported_triangular_and_point_load() {
    let (l, q, p, a) = (6.0, 12e3, 5e3, 2.0);
    let mut fr = frame(vec![[0.0; 3], [l, 0.0, 0.0]]);
    fr.members.push(Member::new(0, 1, 0, 0));
    fr.supports = vec![Support::new(0, [X, X, X, X, O, O]), Support::new(1, [O, X, X, O, O, O])];
    let tri = Load::Dist { member: 0, a: 0.0, b: l, w1: [0.0; 3], w2: down(q), axes: Axes::Local };
    let pt = Load::Point { member: 0, a, f: down(p), m: [0.0; 3], axes: Axes::Global };
    fr.cases = vec![case("tri", vec![tri]), case("pt", vec![pt])];
    let s = run(&fr);
    let r = &s.cases[0];
    close(r.reactions[0].1[2], q * l / 6.0, 1e-10);
    close(r.reactions[1].1[2], q * l / 3.0, 1e-10);
    for (x, f) in r.diagrams[0].x.iter().zip(&r.diagrams[0].f) {
        let m = q * x * (l * l - x * x) / (6.0 * l);
        assert!((f[4] + m).abs() <= 1e-9 * q * l * l, "triangular My at {x}");
    }
    let r = &s.cases[1];
    let dg = &r.diagrams[0];
    let k = dg.x.iter().position(|&x| x == a).expect("station at the point load");
    close(dg.f[k][4], -p * a * (l - a) / l, 1e-10);
    // Station at the load reports the right-hand value; the shear jumps by P there.
    close(dg.f[k][2], p * a / l, 1e-10);
    close(dg.f[k - 1][2], -p * (l - a) / l, 1e-10);
}

#[test]
fn cantilever_tip_load_moment_and_lateral() {
    let (l, p, m) = (4.0, 2e3, 3e3);
    let mut fr = cantilever(l);
    fr.cases = vec![
        case("p", vec![Load::Node { node: 1, f: [0.0, 0.0, -p, 0.0, 0.0, 0.0] }]),
        case("m", vec![Load::Node { node: 1, f: [0.0, 0.0, 0.0, 0.0, m, 0.0] }]),
        case("py", vec![Load::Node { node: 1, f: [0.0, p, 0.0, 0.0, 0.0, 0.0] }]),
    ];
    let s = run(&fr);
    let (c0, c1, c2) = (&s.cases[0], &s.cases[1], &s.cases[2]);
    close(c0.disp[1][2], -p * l.powi(3) / (3.0 * E * IY), 1e-10);
    close(c0.disp[1][4], p * l * l / (2.0 * E * IY), 1e-10);
    close(c0.reactions[0].1[2], p, 1e-12);
    close(c0.reactions[0].1[4], -p * l, 1e-12);
    close(c1.disp[1][4], m * l / (E * IY), 1e-10);
    close(c1.disp[1][2], -m * l * l / (2.0 * E * IY), 1e-10);
    for f in &c1.diagrams[0].f {
        close(f[4], m, 1e-10);
    }
    close(c2.disp[1][1], p * l.powi(3) / (3.0 * E * IZ), 1e-10);
    close(c2.disp[1][5], p * l * l / (2.0 * E * IZ), 1e-10);
}

#[test]
fn cantilever_timoshenko_adds_shear_deflection() {
    let (l, p, av) = (2.0, 1e4, 3e-3);
    let mut fr = cantilever(l);
    fr.sections[0].avz = Some(av);
    fr.cases.push(case("p", vec![Load::Node { node: 1, f: [0.0, 0.0, -p, 0.0, 0.0, 0.0] }]));
    let opt = Options { shear: true, ..Options::default() };
    let s = solve(&fr, &opt).unwrap();
    let want = -(p * l.powi(3) / (3.0 * E * IY) + p * l / (G * av));
    close(s.cases[0].disp[1][2], want, 1e-10);
    // Off by default: Euler-Bernoulli.
    close(run(&fr).cases[0].disp[1][2], -p * l.powi(3) / (3.0 * E * IY), 1e-10);
}

#[test]
fn fixed_fixed_udl() {
    let (l, q) = (5.0, 8e3);
    // One element between fixed nodes: zero equations, forces from fixed-end actions only.
    let mut fr = frame(vec![[0.0; 3], [l, 0.0, 0.0]]);
    fr.members.push(Member::new(0, 1, 0, 0));
    fr.supports = vec![Support::fixed(0), Support::fixed(1)];
    fr.cases.push(case("q", vec![Load::udl(0, down(q), Axes::Global)]));
    for shear in [false, true] {
        let mut f2 = fr.clone();
        f2.sections[0].avz = Some(2e-3);
        let s = solve(&f2, &Options { shear, ..Options::default() }).unwrap();
        assert_eq!(s.equations, 0);
        let r = &s.cases[0];
        close(r.end_forces[0][0][4], -q * l * l / 12.0, 1e-10);
        close(r.end_forces[0][1][4], q * l * l / 12.0, 1e-10);
        close(r.diagrams[0].f[5][4], -q * l * l / 24.0, 1e-10);
        close(r.reactions[0].1[2], q * l / 2.0, 1e-12);
    }
    // Two elements: midspan deflection qL^4/384EI.
    let mut fr = frame(vec![[0.0; 3], [l / 2.0, 0.0, 0.0], [l, 0.0, 0.0]]);
    fr.members = vec![Member::new(0, 1, 0, 0), Member::new(1, 2, 0, 0)];
    fr.supports = vec![Support::fixed(0), Support::fixed(2)];
    fr.cases.push(case("q", two_udl(q)));
    let r = &run(&fr).cases[0];
    close(r.disp[1][2], -q * l.powi(4) / (384.0 * E * IY), 1e-10);
    close(r.end_forces[0][0][4], -q * l * l / 12.0, 1e-10);
}

#[test]
fn axial_bar_and_axial_line_load() {
    let (l, p, q) = (3.0, 1e5, 2e4);
    let mut fr = cantilever(l);
    fr.cases = vec![
        case("p", vec![Load::Node { node: 1, f: [p, 0.0, 0.0, 0.0, 0.0, 0.0] }]),
        case("q", vec![Load::udl(0, [q, 0.0, 0.0], Axes::Local)]),
    ];
    let s = run(&fr);
    close(s.cases[0].disp[1][0], p * l / (E * A), 1e-12);
    for f in &s.cases[0].diagrams[0].f {
        close(f[0], p, 1e-12);
    }
    close(s.cases[1].disp[1][0], q * l * l / (2.0 * E * A), 1e-12);
    close(s.cases[1].diagrams[0].f[0][0], q * l, 1e-12);
    tiny(s.cases[1].diagrams[0].f.last().unwrap()[0], q * l);
}

#[test]
fn torsion() {
    let (l, t) = (2.5, 700.0);
    let mut fr = cantilever(l);
    fr.cases.push(case("t", vec![Load::Node { node: 1, f: [0.0, 0.0, 0.0, t, 0.0, 0.0] }]));
    let r = &run(&fr).cases[0];
    close(r.disp[1][3], t * l / (G * J), 1e-12);
    for f in &r.diagrams[0].f {
        close(f[3], t, 1e-12);
    }
    close(r.reactions[0].1[3], -t, 1e-12);
}

/// Fixed-base portal, columns and beam equal (k = Ib h / (Ic L) = 1, h = L), load H at B.
/// Slope-deflection, axial strain neglected, antisymmetric: joint rotation t, chord psi = D/h,
/// a = EI/h. Joint B: 2a(2t - 3psi) + 6a t = 0 -> t = 0.6 psi. Storey shear:
/// 2 (2a/h)(6psi - 3t) = H -> psi = H h / (16.8 a) -> D = H h^3 / (16.8 EI).
/// Base M = 2a(t - 3psi) = -(2/7) H h, top M = 2a(2t - 3psi) = -(3/14) H h, as in the
/// classic table formulas M_base = H h (3k + 1) / (2 (6k + 1)), M_top = 3k H h / (2 (6k + 1)).
/// Vertical reactions: V L = H h - 2 (2/7) H h -> V = 3H/7.
#[test]
fn portal_frame_textbook() {
    let (h, hl) = (4.0, 1e4);
    let mut fr = frame(vec![[0.0; 3], [0.0, 0.0, h], [h, 0.0, h], [h, 0.0, 0.0]]);
    fr.sections[0] = Section::new(100.0, 1e-4, 1e-4, 1e-4);
    fr.members = vec![Member::new(0, 1, 0, 0), Member::new(1, 2, 0, 0), Member::new(3, 2, 0, 0)];
    fr.supports = vec![Support::fixed(0), Support::fixed(3)];
    fr.cases.push(case("H", vec![Load::Node { node: 1, f: [hl, 0.0, 0.0, 0.0, 0.0, 0.0] }]));
    let r = &run(&fr).cases[0];
    let ei = E * 1e-4;
    close(r.disp[1][0], hl * h.powi(3) / (16.8 * ei), 1e-6);
    close(r.disp[2][0], hl * h.powi(3) / (16.8 * ei), 1e-6);
    for (_, rr) in &r.reactions {
        close(rr[0], -hl / 2.0, 1e-6);
        close(rr[4].abs(), 2.0 / 7.0 * hl * h, 1e-6);
        close(rr[2].abs(), 3.0 / 7.0 * hl, 1e-6);
    }
    close(r.end_forces[1][0][4].abs(), 3.0 / 14.0 * hl * h, 1e-6);
    close(r.end_forces[1][1][4].abs(), 3.0 / 14.0 * hl * h, 1e-6);
    assert!(r.equilibrium.relative < 1e-10);
}

#[test]
fn pinned_end_has_zero_moment() {
    let (l, p) = (3.0, 4e3);
    let mut fr = frame(vec![[0.0; 3], [l, 0.0, 0.0], [2.0 * l, 0.0, 0.0]]);
    fr.members = vec![Member::new(0, 1, 0, 0).pin_end(1), Member::new(1, 2, 0, 0)];
    fr.supports = vec![Support::fixed(0), Support::fixed(2)];
    fr.cases.push(case("p", vec![Load::Node { node: 1, f: [0.0, 0.0, -p, 0.0, 0.0, 0.0] }]));
    let r = &run(&fr).cases[0];
    // Two propped members in parallel, each 3EI/L^3.
    close(r.disp[1][2], -p * l.powi(3) / (6.0 * E * IY), 1e-10);
    assert_eq!(r.end_forces[0][1][4], 0.0);
    assert_eq!(r.end_forces[0][1][5], 0.0);
    tiny(r.diagrams[0].f.last().unwrap()[4], p * l);
    close(r.end_forces[0][0][4].abs(), p * l / 2.0, 1e-10);
    // Node 1 rotates freely, so member 1 is a cantilever from node 2 carrying P/2.
    tiny(r.end_forces[1][0][4], p * l);
    close(r.end_forces[1][1][4].abs(), p * l / 2.0, 1e-10);
}

#[test]
fn rigid_offsets() {
    let (a, l, c, p) = (0.5, 3.0, 0.4, 1e3);
    let mut fr = cantilever(a + l + c);
    fr.members[0].offset = [[a, 0.0, 0.0], [-c, 0.0, 0.0]];
    fr.cases.push(case("p", vec![Load::Node { node: 1, f: [0.0, 0.0, -p, 0.0, 0.0, 0.0] }]));
    let r = &run(&fr).cases[0];
    let ei = E * IY;
    close(r.disp[1][2], -(p * l.powi(3) / 3.0 + p * c * l * l + p * c * c * l) / ei, 1e-10);
    close(r.disp[1][4], (p * l * l / 2.0 + p * c * l) / ei, 1e-10);
    close(r.reactions[0].1[4], -p * (a + l + c), 1e-10);
    // Flexible length L, end moment P c at the flexible end j.
    close(r.diagrams[0].x.last().copied().unwrap(), l, 1e-12);
    close(r.end_forces[0][1][4], p * c, 1e-10);
}

#[test]
fn spring_support() {
    let (l, p) = (3.0f64, 6e3);
    let k = 3.0 * E * IY / l.powi(3);
    let mut fr = cantilever(l);
    fr.supports.push(Support::new(1, [O, O, Restraint::Spring(k), O, O, O]));
    fr.cases.push(case("p", vec![Load::Node { node: 1, f: [0.0, 0.0, -p, 0.0, 0.0, 0.0] }]));
    let r = &run(&fr).cases[0];
    close(r.disp[1][2], -p / (2.0 * k), 1e-10);
    close(r.reactions[1].1[2], p / 2.0, 1e-10);
    close(r.reactions[0].1[2], p / 2.0, 1e-10);
    assert!(r.equilibrium.relative < 1e-12);
}

/// Two-storey-high 3D frame with every load type, releases, offsets, orientations.
fn space_frame() -> Frame {
    let h = 3.5;
    let mut fr = frame(vec![
        [0.0, 0.0, 0.0],
        [6.0, 0.0, 0.0],
        [6.0, 4.0, 0.0],
        [0.0, 4.0, 0.0],
        [0.0, 0.0, h],
        [6.0, 0.0, h],
        [6.0, 4.0, h],
        [0.0, 4.0, h],
        [3.0, 2.0, h + 1.5],
    ]);
    let col = Section::new(8e-3, 2e-4, 6e-5, 3e-6);
    fr.sections.push(Section { avy: Some(2e-3), avz: Some(3e-3), ..col });
    fr.materials.push(Material { e: 30e9, g: 12.5e9, density: 2500.0 });
    for c in 0..4 {
        let mut m = Member::new(c, c + 4, 1, 1);
        m.orient = Orient::Roll(0.3 * c as f64);
        fr.members.push(m);
    }
    for (i, j) in [(4, 5), (5, 6), (6, 7), (7, 4)] {
        let mut m = Member::new(i, j, 0, 0);
        m.offset = [[0.0, 0.0, -0.2], [0.0, 0.0, -0.2]];
        fr.members.push(m);
    }
    for c in 4..8 {
        let mut m = Member::new(c, 8, 0, 0).pin_end(1);
        m.orient = Orient::Vector([0.3, 1.0, 0.2]);
        fr.members.push(m);
    }
    fr.members.push(Member::new(0, 6, 0, 0).pin_end(0).pin_end(1));
    fr.supports = vec![Support::fixed(0), Support::fixed(1), Support::pinned(2), Support::fixed(3)];
    fr.supports.push(Support::new(2, [O, O, O, Restraint::Spring(5e6), O, Restraint::Spring(1e6)]));
    let (lo, gl) = (Axes::Local, Axes::Global);
    let w6 = [50.0, -400.0, 900.0];
    let mixed = vec![
        Load::Node { node: 8, f: [1e3, -2e3, -5e3, 300.0, -200.0, 100.0] },
        Load::Node { node: 5, f: [4e3, 0.0, 0.0, 0.0, 0.0, 0.0] },
        Load::Point { member: 4, a: 2.0, f: [0.0, 500.0, -3e3], m: [0.0, 100.0, 50.0], axes: lo },
        Load::Point { member: 9, a: 1.0, f: [200.0, 0.0, -800.0], m: [0.0; 3], axes: gl },
        Load::Dist {
            member: 5,
            a: 0.5,
            b: 3.0,
            w1: [0.0, 0.0, -2e3],
            w2: [100.0, 300.0, -6e3],
            axes: gl,
        },
        Load::Dist { member: 6, a: 1.0, b: f64::INFINITY, w1: w6, w2: w6, axes: lo },
    ];
    let wind = [0.0, 1.5e3, 0.0];
    fr.cases = vec![
        case("mixed", mixed),
        case("sw", vec![Load::SelfWeight { dir: [0.0, 0.0, -1.0], g: 9.81 }]),
        case("wind", vec![Load::udl(1, wind, gl), Load::udl(0, wind, gl)]),
    ];
    fr.combos = vec![
        Combo { name: "ULS".into(), factors: vec![(0, 1.35), (1, 1.35), (2, 1.5)] },
        Combo { name: "SLS".into(), factors: vec![(0, 1.0), (1, 1.0), (2, 0.6)] },
    ];
    fr.envelopes = vec![Envelope { name: "env".into(), cases: vec![0, 1, 2], combos: vec![0, 1] }];
    fr
}

#[test]
fn space_frame_equilibrium() {
    let fr = space_frame();
    let s = run(&fr);
    for r in s.cases.iter().chain(&s.combos) {
        assert!(r.equilibrium.relative < 1e-9, "{}: {:?}", r.name, r.equilibrium);
        let scale = r.equilibrium.load[..3].iter().map(|v| v.abs()).sum::<f64>() * 10.0;
        for (dg, ef) in r.diagrams.iter().zip(&r.end_forces) {
            for k in 0..6 {
                tiny(dg.f[0][k] + ef[0][k], scale);
                tiny(dg.f.last().unwrap()[k] - ef[1][k], scale);
            }
        }
        assert!(r.summary.max_disp > 0.0 && r.summary.max_moment > 0.0);
    }
    // Self-weight resultant equals total member weight.
    let sw = &s.cases[1].equilibrium;
    let w: f64 = (0..fr.members.len()).map(|i| member_weight(&fr, i)).sum();
    close(sw.load[2], -w, 1e-12);
    // Pinned brace (member 12): zero bending at both ends.
    for e in 0..2 {
        tiny(s.cases[0].end_forces[12][e][4], 1e3);
        tiny(s.cases[0].end_forces[12][e][5], 1e3);
    }
}

fn member_weight(fr: &Frame, i: usize) -> f64 {
    let m = &fr.members[i];
    let p = |n: usize, e: usize| {
        let c = fr.nodes[n];
        [c[0] + m.offset[e][0], c[1] + m.offset[e][1], c[2] + m.offset[e][2]]
    };
    let (a, b) = (p(m.i, 0), p(m.j, 1));
    let l = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    fr.materials[m.material].density * fr.sections[m.section].a * 9.81 * l
}

#[test]
fn mechanism_is_reported() {
    // Torsional spin: no support or member restrains rotation about the beam axis (X).
    let mut fr = frame(vec![[0.0; 3], [4.0, 0.0, 0.0]]);
    fr.members.push(Member::new(0, 1, 0, 0));
    fr.supports = vec![Support::pinned(0), Support::new(1, [O, X, X, O, O, O])];
    fr.cases.push(case("p", vec![Load::udl(0, down(1e3), Axes::Global)]));
    let err = solve(&fr, &Options::default()).unwrap_err();
    assert!(matches!(err, FemError::Mechanism { dof: 3, .. }), "{err:?}");
    assert!(err.to_string().ends_with("is unrestrained in rotation about X"), "{err}");
    // A node with no member and no support.
    let mut fr = cantilever(2.0);
    fr.nodes.push([9.0, 9.0, 9.0]);
    let err = solve(&fr, &Options::default()).unwrap_err();
    assert_eq!(err, FemError::Mechanism { node: 2, dof: 0 });
    assert_eq!(err.to_string(), "node 2 is unrestrained in translation along X");
    // Bad input is an error, not NaN.
    let mut fr = cantilever(2.0);
    fr.sections[0].iy = 0.0;
    assert!(matches!(solve(&fr, &Options::default()), Err(FemError::Input(_))));
}

#[test]
fn combination_is_linear_and_envelope_bounds() {
    let mut fr = space_frame();
    let mut scaled = Vec::new();
    for (ci, f) in [(0usize, 1.35), (1, 1.35), (2, 1.5)] {
        for ld in &fr.cases[ci].loads {
            scaled.push(match ld.clone() {
                Load::Node { node, f: v } => Load::Node { node, f: v.map(|x| x * f) },
                Load::Point { member, a, f: v, m, axes } => {
                    Load::Point { member, a, f: v.map(|x| x * f), m: m.map(|x| x * f), axes }
                }
                Load::Dist { member, a, b, w1, w2, axes } => Load::Dist {
                    member,
                    a,
                    b,
                    w1: w1.map(|x| x * f),
                    w2: w2.map(|x| x * f),
                    axes,
                },
                Load::SelfWeight { dir, g } => Load::SelfWeight { dir, g: g * f },
            });
        }
    }
    fr.cases.push(case("direct", scaled));
    let s = run(&fr);
    let (c, d) = (&s.combos[0], &s.cases[3]);
    let dmax = d.summary.max_disp;
    for (x, y) in c.disp.iter().zip(&d.disp) {
        for k in 0..3 {
            tiny(x[k] - y[k], dmax);
        }
    }
    let fmax = d.summary.max_moment;
    for (x, y) in c.diagrams.iter().zip(&d.diagrams) {
        assert_eq!(x.x, y.x);
        for (p, q) in x.f.iter().zip(&y.f) {
            for k in 0..6 {
                tiny(p[k] - q[k], fmax);
            }
        }
    }
    for (x, y) in c.reactions.iter().zip(&d.reactions) {
        for k in 0..6 {
            tiny(x.1[k] - y.1[k], fmax);
        }
    }
    let env = &s.envelopes[0];
    for item in s.cases[..3].iter().chain(&s.combos) {
        for ((hi, lo), v) in env.max.disp.iter().zip(&env.min.disp).zip(&item.disp) {
            for k in 0..6 {
                assert!(hi[k] >= v[k] && lo[k] <= v[k]);
            }
        }
        for ((hi, lo), v) in env.max.diagrams.iter().zip(&env.min.diagrams).zip(&item.diagrams) {
            for ((h, l), x) in hi.f.iter().zip(&lo.f).zip(&v.f) {
                for k in 0..6 {
                    assert!(h[k] >= x[k] && l[k] <= x[k]);
                }
            }
        }
    }
}

#[test]
fn deterministic_bitwise() {
    let fr = space_frame();
    let a = run(&fr);
    let b = run(&fr);
    let bits = |s: &Solution| {
        let mut v: Vec<u64> = Vec::new();
        for r in s.cases.iter().chain(&s.combos) {
            v.extend(r.disp.iter().flatten().map(|x| x.to_bits()));
            v.extend(r.end_forces.iter().flatten().flatten().map(|x| x.to_bits()));
            for d in &r.diagrams {
                v.extend(d.f.iter().flatten().map(|x| x.to_bits()));
            }
            v.extend(r.reactions.iter().flat_map(|x| x.1).map(|x| x.to_bits()));
        }
        v
    };
    assert_eq!(bits(&a), bits(&b));
    assert_eq!(a, b);
}

#[test]
fn rcm_orders_a_scrambled_chain() {
    // Chain 0-5-1-6-2-7-3-8-4: RCM must give consecutive neighbours.
    let chain = [0usize, 5, 1, 6, 2, 7, 3, 8, 4];
    let mut adj = vec![Vec::new(); 9];
    for w in chain.windows(2) {
        adj[w[0]].push(w[1]);
        adj[w[1]].push(w[0]);
    }
    let order = crate::skyline::rcm(&adj);
    assert_eq!(order.len(), 9);
    for w in order.windows(2) {
        assert!(adj[w[0]].contains(&w[1]), "{order:?}");
    }
}

#[test]
fn skyline_solves_spd_system() {
    // [4 1 0; 1 3 1; 0 1 2] x = [1 2 3]
    let mut s = crate::skyline::Skyline::new(vec![0, 0, 1]);
    for (i, j, v) in [(0, 0, 4.0), (1, 0, 1.0), (1, 1, 3.0), (2, 1, 1.0), (2, 2, 2.0)] {
        s.add(i, j, v);
    }
    s.factor(1e-12).unwrap();
    let mut b = vec![1.0, 2.0, 3.0];
    s.solve(&mut b);
    let want = [2.0 / 9.0, 1.0 / 9.0, 13.0 / 9.0];
    for k in 0..3 {
        close(b[k], want[k], 1e-12);
    }
}

#[test]
#[ignore = "performance: 10k-member grid, run with --release --ignored"]
fn perf_10k_member_grid() {
    let n = 71;
    let mut nodes = Vec::new();
    for i in 0..n {
        for j in 0..n {
            nodes.push([i as f64, j as f64, 0.0]);
        }
    }
    let mut fr = frame(nodes);
    for i in 0..n {
        for j in 0..n {
            let a = i * n + j;
            if j + 1 < n {
                fr.members.push(Member::new(a, a + 1, 0, 0));
            }
            if i + 1 < n {
                fr.members.push(Member::new(a, a + n, 0, 0));
            }
        }
    }
    assert!(fr.members.len() >= 9900);
    for c in [0, n - 1, n * (n - 1), n * n - 1] {
        fr.supports.push(Support::fixed(c));
    }
    fr.cases.push(case("sw", vec![Load::SelfWeight { dir: [0.0, 0.0, -1.0], g: 9.81 }]));
    let mid = Load::Node { node: n * n / 2, f: [0.0, 0.0, -1e4, 0.0, 0.0, 0.0] };
    fr.cases.push(case("p", vec![mid]));
    let t = std::time::Instant::now();
    let s = run(&fr);
    let dt = t.elapsed().as_secs_f64();
    eprintln!("grid: {} eq, profile {}, {dt:.2} s", s.equations, s.profile);
    assert!(s.cases[0].equilibrium.relative < 1e-8);
    assert!(dt < 60.0, "took {dt} s");
}
