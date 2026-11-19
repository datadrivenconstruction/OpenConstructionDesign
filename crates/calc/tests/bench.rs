//! Analytical benchmarks B1-B13 (docs/design/STRUCTURAL_ANALYSIS.md 7.1), end to end: a model
//! is built with the editor API, keyed overrides set supports, releases, offsets and sections,
//! loads are stored rows, and the numbers come from `ddc_calc::run`. Euler-Bernoulli members
//! (no shear deformation), E = 200 GPa, G = 80 GPa. Cubic shape functions are exact for these
//! cases, so the tolerance is 1e-6 relative unless a comment says otherwise.

use ddc_calc::{run, RunOptions, RunOutcome};
use ddc_geom::member::Shape;
use ddc_model::{AnOverride, BeamJoin, BeamSpec, ColumnSpec, FootingSpec, FootingStep, Just};
use ddc_model::{Load, LoadCase, LoadCombo, MatMech, Model};
use serde_json::{json, Value as J};

const E: f64 = 200e9;
const GM: f64 = 80e9;
const TOL: f64 = 1e-6;

pub struct B {
    pub m: Model,
    pub l0: i64,
}

pub fn bench() -> B {
    let mut m = Model::open(":memory:").unwrap();
    let l0 = m.add_level("L0", 0.0).unwrap();
    B { m, l0 }
}

impl B {
    pub fn mtype(&mut self, name: &str, s: Shape) -> i64 {
        let t = self.m.add_member_type(name, &s, "Steel").unwrap();
        let mech = MatMech { e: Some(E), g: Some(GM), ..MatMech::default() };
        self.m.set_material_mech("Steel", &mech).unwrap();
        t
    }

    pub fn beam(&mut self, t: i64, a: (f64, f64, f64), b: (f64, f64, f64)) -> i64 {
        self.m
            .add_beam(&BeamSpec {
                level: self.l0,
                member_type: t,
                a,
                b,
                bulge: 0.0,
                yjust: Just::Center,
                zjust: Just::Center,
                rotation: 0.0,
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: BeamJoin::None,
                join_end: BeamJoin::None,
            })
            .unwrap()
    }

    pub fn ov(&mut self, target: &str, field: &str, value: J) {
        let o = AnOverride {
            id: 0,
            target: target.into(),
            field: field.into(),
            value,
            anchor: None,
            locked: false,
            src: "manual".into(),
            why: None,
        };
        self.m.set_an_override(&o).unwrap();
    }

    /// User support `s:<node key>` with a DOF mask (F fixed, P free) and optional springs.
    pub fn sup(&mut self, node: &str, dofs: &str) {
        self.ov(&format!("s:{node}"), "dofs", json!(dofs));
    }

    pub fn case(&mut self, name: &str, self_weight: bool) -> i64 {
        let c = LoadCase {
            id: 0,
            name: name.into(),
            category: "permanent".into(),
            self_weight,
            note: String::new(),
            locked: false,
            src: "manual".into(),
        };
        self.m.add_load_case(&c).unwrap()
    }

    pub fn load(&mut self, lcase: i64, kind: &str, target: &str, dir: &str, value: J) -> i64 {
        let l = Load {
            id: 0,
            lcase,
            kind: kind.into(),
            target: target.into(),
            frame: "global".into(),
            dir: dir.into(),
            value,
            grp: None,
            locked: false,
            src: "manual".into(),
            why: None,
        };
        self.m.add_load(&l).unwrap()
    }

    pub fn combo(&mut self, name: &str, terms: Vec<(i64, f64)>) -> i64 {
        let c = LoadCombo {
            id: 0,
            name: name.into(),
            state: "uls".into(),
            generated: false,
            rule: None,
            locked: false,
            src: "manual".into(),
            terms,
        };
        self.m.add_load_combo(&c).unwrap()
    }

    pub fn solve(&self) -> RunOutcome {
        let o = match run(&self.m, &RunOptions::default()) {
            Ok(o) => o,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(o.status, "ok", "{:?}", o.checks);
        for c in &o.checks {
            assert!(c.fem_relative <= 1e-9, "{c:?}");
            assert!(c.applied_relative <= 1e-9, "{c:?}");
            assert!(c.conservation <= 1e-9, "{c:?}");
        }
        assert!(o.run_id.is_some());
        o
    }
}

pub fn start(id: i64) -> String {
    format!("n:m:beam:{id}:start")
}
pub fn end(id: i64) -> String {
    format!("n:m:beam:{id}:end")
}

pub fn disp(o: &RunOutcome, case: usize, node: &str) -> [f64; 6] {
    let n = o.input.node_of(node).unwrap_or_else(|| panic!("{node}: {:?}", o.input.node_keys));
    o.solution.cases[case].disp[n]
}

pub fn reaction(o: &RunOutcome, case: usize, node: &str) -> [f64; 6] {
    let n = o.input.node_of(node).unwrap();
    o.solution.cases[case].reactions.iter().find(|r| r.0 == n).unwrap().1
}

/// End forces (local) of the solver member with key `seg`, end 0 or 1.
pub fn end_force(o: &RunOutcome, case: usize, seg: &str, e: usize) -> [f64; 6] {
    let s = o.input.seg_keys.iter().position(|k| k == seg).unwrap();
    o.solution.cases[case].end_forces[s][e]
}

#[track_caller]
pub fn close(what: &str, got: f64, want: f64, tol: f64) {
    let err = (got - want).abs() / want.abs().max(1e-300);
    assert!(err <= tol, "{what}: got {got:e}, want {want:e}, rel {err:e}");
    println!("BENCH {what}: want {want:.9e} got {got:.9e} rel {err:.1e}");
}

fn rect(b: f64, h: f64) -> Shape {
    Shape::Rect { b, h }
}

/// I about local y of Rect{b, h}: b h^3 / 12 (h along the section z axis).
fn iy(b: f64, h: f64) -> f64 {
    b * h.powi(3) / 12.0
}

#[test]
fn b1_axial_bar() {
    // delta = P L / (E A)
    let (l, p, b, h) = (4.0, 1e5, 0.1, 0.2);
    let mut t = bench();
    let s = t.mtype("S", rect(b, h));
    let id = t.beam(s, (0.0, 0.0, 0.0), (l, 0.0, 0.0));
    t.sup(&start(id), "FFFFFF");
    let c = t.case("P", false);
    t.load(c, "point", &end(id), "x", json!({"f": p}));
    let o = t.solve();
    close("B1 ux", disp(&o, 0, &end(id))[0], p * l / (E * b * h), TOL);
    close("B1 Rx", reaction(&o, 0, &start(id))[0], -p, TOL);
}

#[test]
fn b2_cantilever_tip_load() {
    // delta = P L^3 / 3EI, theta = P L^2 / 2EI, M0 = P L
    let (l, p, b, h) = (3.0, 1e4, 0.1, 0.2);
    let ei = E * iy(b, h);
    let mut t = bench();
    let s = t.mtype("S", rect(b, h));
    let id = t.beam(s, (0.0, 0.0, 0.0), (l, 0.0, 0.0));
    t.sup(&start(id), "FFFFFF");
    let c = t.case("P", false);
    t.load(c, "point", &end(id), "-z", json!({"f": p}));
    let o = t.solve();
    let u = disp(&o, 0, &end(id));
    close("B2 uz", u[2], -p * l.powi(3) / (3.0 * ei), TOL);
    // slope duz/dx < 0 is a positive rotation about +Y
    close("B2 ry", u[4], p * l * l / (2.0 * ei), TOL);
    close("B2 M0", reaction(&o, 0, &start(id))[4], -p * l, TOL);
    close("B2 Rz", reaction(&o, 0, &start(id))[2], p, TOL);
}

/// Beam of span `l` in two halves (a node at midspan), UDL `w` downward on both.
fn two_halves(t: &mut B, l: f64, b: f64, h: f64, w: f64) -> (i64, i64) {
    let s = t.mtype("S", rect(b, h));
    let b1 = t.beam(s, (0.0, 0.0, 0.0), (l / 2.0, 0.0, 0.0));
    let b2 = t.beam(s, (l / 2.0, 0.0, 0.0), (l, 0.0, 0.0));
    let c = t.case("w", false);
    let target = json!([format!("m:beam:{b1}"), format!("m:beam:{b2}")]).to_string();
    t.load(c, "line", &target, "-z", json!({"q1": w}));
    (b1, b2)
}

#[test]
fn b3_simply_supported_udl() {
    // delta = 5 w L^4 / 384 EI, M = w L^2 / 8, R = w L / 2
    let (l, w, b, h) = (6.0, 5e3, 0.1, 0.3);
    let ei = E * iy(b, h);
    let mut t = bench();
    let (b1, b2) = two_halves(&mut t, l, b, h, w);
    t.sup(&start(b1), "FFFFPP");
    t.sup(&end(b2), "PFFPPP");
    let o = t.solve();
    close("B3 delta", disp(&o, 0, &end(b1))[2], -5.0 * w * l.powi(4) / (384.0 * ei), TOL);
    let mm = end_force(&o, 0, &format!("m:beam:{b1}#0"), 1)[4];
    close("B3 Mmid", mm.abs(), w * l * l / 8.0, TOL);
    close("B3 R", reaction(&o, 0, &start(b1))[2], w * l / 2.0, TOL);
}

#[test]
fn b4_fixed_fixed_udl() {
    // M_end = w L^2 / 12, M_mid = w L^2 / 24, delta = w L^4 / 384 EI
    let (l, w, b, h) = (6.0, 5e3, 0.1, 0.3);
    let ei = E * iy(b, h);
    let mut t = bench();
    let (b1, b2) = two_halves(&mut t, l, b, h, w);
    t.sup(&start(b1), "FFFFFF");
    t.sup(&end(b2), "FFFFFF");
    let o = t.solve();
    close("B4 delta", disp(&o, 0, &end(b1))[2], -w * l.powi(4) / (384.0 * ei), TOL);
    close("B4 Mend", reaction(&o, 0, &start(b1))[4].abs(), w * l * l / 12.0, TOL);
    let mm = end_force(&o, 0, &format!("m:beam:{b1}#0"), 1)[4];
    close("B4 Mmid", mm.abs(), w * l * l / 24.0, TOL);
}

#[test]
fn b5_propped_cantilever_udl() {
    // R_prop = 3 w L / 8, M_fixed = w L^2 / 8
    let (l, w, b, h) = (5.0, 4e3, 0.1, 0.3);
    let mut t = bench();
    let s = t.mtype("S", rect(b, h));
    let id = t.beam(s, (0.0, 0.0, 0.0), (l, 0.0, 0.0));
    t.sup(&start(id), "FFFFFF");
    t.sup(&end(id), "PFFPPP");
    let c = t.case("w", false);
    t.load(c, "line", &format!("m:beam:{id}"), "-z", json!({"q1": w}));
    let o = t.solve();
    close("B5 R", reaction(&o, 0, &end(id))[2], 3.0 * w * l / 8.0, TOL);
    close("B5 Mfix", reaction(&o, 0, &start(id))[4].abs(), w * l * l / 8.0, TOL);
}

#[test]
fn b6_torsion_bar() {
    // theta = T L / (G J), J = pi d^4 / 32 (solid circle); T = P e from a lever at the tip
    // lever longer than the 0.6 m snap tolerance, or its far end snaps onto the bar tip
    let (l, d, e, p) = (2.0, 0.1f64, 1.5, 2e3);
    let j = std::f64::consts::PI * d.powi(4) / 32.0;
    let mut t = bench();
    let s = t.mtype("C", Shape::Circle { d });
    let lever = t.mtype("L", rect(0.2, 0.2));
    let id = t.beam(s, (0.0, 0.0, 0.0), (l, 0.0, 0.0));
    let lv = t.beam(lever, (l, 0.0, 0.0), (l, e, 0.0));
    t.sup(&start(id), "FFFFFF");
    let c = t.case("T", false);
    t.load(c, "point", &end(lv), "-z", json!({"f": p}));
    let o = t.solve();
    let a = t.m.analytical().unwrap();
    let mi = a.member_of(&format!("m:beam:{id}")).unwrap();
    let sec = &a.sections[a.members[mi].section.unwrap()];
    close("B6 J", sec.props.j, j, 1e-12);
    // arm (0, e, 0) x force (0, 0, -P) = (-P e, 0, 0)
    close("B6 rx", disp(&o, 0, &end(id))[3], -p * e * l / (GM * j), TOL);
}

#[test]
fn b7_pinned_beam_in_fixed_frame_equals_b3() {
    // beam released (ry, rz) at both column tops: M_mid = w L^2 / 8, end moments 0
    let (l, hc, w) = (6.0, 3.0, 5e3);
    let mut t = bench();
    let l1 = t.m.add_level("L1", hc).unwrap();
    let ct = t.mtype("C", rect(0.3, 0.3));
    let bt = t.mtype("B", rect(0.2, 0.4));
    let col = |x: f64| ColumnSpec {
        level: t.l0,
        top_level: Some(l1),
        member_type: ct,
        structural: true,
        at: (x, 0.0),
        base_offset: 0.0,
        top_offset: 0.0,
        rotation: 0.0,
        top_at: None,
    };
    let (s0, s1) = (col(0.0), col(l));
    let c0 = t.m.add_column(&s0).unwrap();
    let c1 = t.m.add_column(&s1).unwrap();
    for c in [c0, c1] {
        footing(&mut t.m, t.l0, c);
    }
    let b1 = t.beam(bt, (0.0, 0.0, hc), (l / 2.0, 0.0, hc));
    let b2 = t.beam(bt, (l / 2.0, 0.0, hc), (l, 0.0, hc));
    t.ov(&format!("m:beam:{b1}"), "release_start", json!({"mask": "FFFFPP"}));
    t.ov(&format!("m:beam:{b2}"), "release_end", json!({"mask": "FFFFPP"}));
    let c = t.case("w", false);
    let target = json!([format!("m:beam:{b1}"), format!("m:beam:{b2}")]).to_string();
    t.load(c, "line", &target, "-z", json!({"q1": w}));
    let o = t.solve();
    let mm = end_force(&o, 0, &format!("m:beam:{b1}#0"), 1)[4];
    close("B7 Mmid", mm.abs(), w * l * l / 8.0, TOL);
    let m0 = end_force(&o, 0, &format!("m:beam:{b1}#0"), 0)[4];
    assert!(m0.abs() <= 1e-9 * w * l * l, "{m0}");
    close("B7 V", end_force(&o, 0, &format!("m:beam:{b1}#0"), 0)[2].abs(), w * l / 2.0, TOL);
}

/// Pad footing under a column (centred: `at` is the offset from the column axis).
pub fn footing(m: &mut Model, level: i64, col: i64) -> i64 {
    m.add_footing(&FootingSpec {
        level,
        column: Some(col),
        at: (0.0, 0.0),
        rotation: 0.0,
        round: false,
        steps: vec![FootingStep { w: 1.0, l: 1.0, h: 0.4 }],
        top_offset: 0.0,
        material: "Concrete".into(),
    })
    .unwrap()
}

#[test]
fn b8_rigid_end_offset() {
    // flexible cantilever L with a rigid arm e at the tip: shear P and moment P e at the
    // flexible end, delta = P/EI (L^3/3 + e L^2 + e^2 L), M0 = P (L + e)
    let (l, e, p, b, h) = (3.0, 0.5, 1e4, 0.1, 0.2);
    let ei = E * iy(b, h);
    let mut t = bench();
    let s = t.mtype("S", rect(b, h));
    let id = t.beam(s, (0.0, 0.0, 0.0), (l + e, 0.0, 0.0));
    t.ov(&format!("m:beam:{id}"), "ecc_end", json!({"frame": "global", "d": [-e, 0.0, 0.0]}));
    t.sup(&start(id), "FFFFFF");
    let c = t.case("P", false);
    t.load(c, "point", &end(id), "-z", json!({"f": p}));
    let o = t.solve();
    let want = p / ei * (l.powi(3) / 3.0 + e * l * l + e * e * l);
    close("B8 uz", disp(&o, 0, &end(id))[2], -want, TOL);
    close("B8 M0", reaction(&o, 0, &start(id))[4].abs(), p * (l + e), TOL);
}

#[test]
fn b9_spring_supports() {
    // each end spring carries P/2: delta = (P/2) / k
    let (l, p, k) = (4.0, 1e4, 2e6);
    let mut t = bench();
    let s = t.mtype("S", rect(0.1, 0.2));
    let id = t.beam(s, (0.0, 0.0, 0.0), (l, 0.0, 0.0));
    t.sup(&start(id), "FFPFPP");
    t.sup(&end(id), "PFPPPP");
    let springs = json!([0.0, 0.0, k, 0.0, 0.0, 0.0]);
    t.ov(&format!("s:{}", start(id)), "springs", springs.clone());
    t.ov(&format!("s:{}", end(id)), "springs", springs);
    let c = t.case("P", false);
    t.load(c, "point", &format!("m:beam:{id}"), "-z", json!({"f": p, "x": 2.0}));
    let o = t.solve();
    close("B9 uz start", disp(&o, 0, &start(id))[2], -p / 2.0 / k, TOL);
    close("B9 uz end", disp(&o, 0, &end(id))[2], -p / 2.0 / k, TOL);
    close("B9 Rz", reaction(&o, 0, &end(id))[2], p / 2.0, TOL);
}

#[test]
fn b10_portal_frame_lateral_load() {
    // slope-deflection, fixed bases, antisymmetric sway: kc = EIc/h, kb = EIb/L,
    // theta = 6 kc psi / (4 kc + 6 kb), 12 kc (2 psi - theta) / h = H, delta = psi h,
    // M_base = 2 kc (theta - 3 psi). Axial strain is suppressed with A = 1e3 m2: beam
    // shortening (H/2) L / (E A) is 2e-7 of the sway (a larger A costs equilibrium digits).
    let (l, hc, h, ic, ib) = (6.0, 3.0, 1e4, 2e-4, 4e-4);
    let mut t = bench();
    let l1 = t.m.add_level("L1", hc).unwrap();
    let ct = t.mtype("C", rect(0.3, 0.3));
    let col = |x: f64| ColumnSpec {
        level: t.l0,
        top_level: Some(l1),
        member_type: ct,
        structural: true,
        at: (x, 0.0),
        base_offset: 0.0,
        top_offset: 0.0,
        rotation: 0.0,
        top_at: None,
    };
    let (s0, s1) = (col(0.0), col(l));
    let c0 = t.m.add_column(&s0).unwrap();
    let c1 = t.m.add_column(&s1).unwrap();
    for c in [c0, c1] {
        footing(&mut t.m, t.l0, c);
    }
    let bm = t.beam(ct, (0.0, 0.0, hc), (l, 0.0, hc));
    let sec = |i: f64| json!({"A": 1e3, "Iy": i, "Iz": i, "J": 1e-2});
    t.ov(&format!("m:column:{c0}"), "section", sec(ic));
    t.ov(&format!("m:column:{c1}"), "section", sec(ic));
    t.ov(&format!("m:beam:{bm}"), "section", sec(ib));
    let c = t.case("H", false);
    t.load(c, "point", &format!("n:m:column:{c0}:end"), "x", json!({"f": h}));
    let o = t.solve();
    let (kc, kb) = (E * ic / hc, E * ib / l);
    let r = 6.0 * kc / (4.0 * kc + 6.0 * kb);
    let psi = h * hc / (12.0 * kc * (2.0 - r));
    let theta = r * psi;
    let tol = 1e-6;
    close("B10 sway", disp(&o, 0, &format!("n:m:column:{c0}:end"))[0], psi * hc, tol);
    close("B10 sway c1", disp(&o, 0, &format!("n:m:column:{c1}:end"))[0], psi * hc, tol);
    let mb = 2.0 * kc * (theta - 3.0 * psi);
    close("B10 Mbase", reaction(&o, 0, &format!("n:m:column:{c0}:start"))[4].abs(), mb.abs(), tol);
    close("B10 Hbase", reaction(&o, 0, &format!("n:m:column:{c0}:start"))[0], -h / 2.0, tol);
}

#[test]
fn b11_space_cantilever_rolled_section() {
    // local y = (0, c, s), z = (0, -s, c) for roll 30 deg; tip deflections per local axis:
    // dy = Py L^3 / (3 E Iz), dz = Pz L^3 / (3 E Iy), Iy = b h^3/12, Iz = h b^3/12
    let (l, b, h, fy, fz) = (2.0, 0.1, 0.3, 1e3, -2e3);
    let mut t = bench();
    let s = t.mtype("S", rect(b, h));
    let id = t.beam(s, (0.0, 0.0, 0.0), (l, 0.0, 0.0));
    t.ov(&format!("m:beam:{id}"), "roll", json!({"deg": 30.0}));
    t.sup(&start(id), "FFFFFF");
    let c = t.case("P", false);
    t.load(c, "point", &end(id), "y", json!({"f": fy}));
    t.load(c, "point", &end(id), "z", json!({"f": fz}));
    let o = t.solve();
    let (cs, sn) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
    let (y, z) = ([0.0, cs, sn], [0.0, -sn, cs]);
    let (py, pz) = (fy * y[1] + fz * y[2], fy * z[1] + fz * z[2]);
    let dy = py * l.powi(3) / (3.0 * E * h * b.powi(3) / 12.0);
    let dz = pz * l.powi(3) / (3.0 * E * iy(b, h));
    let u = disp(&o, 0, &end(id));
    close("B11 uy", u[1], dy * y[1] + dz * z[1], TOL);
    close("B11 uz", u[2], dy * y[2] + dz * z[2], TOL);
}

#[test]
fn b12_trapezoid_point_and_combination() {
    // trapezoid q1 at x1 to q2 at x2: W = (q1 + q2)/2 (x2 - x1),
    // xc = x1 + (x2 - x1)(q1 + 2 q2) / (3 (q1 + q2)); R_end = W xc / L; point: R_end = P a / L
    let (l, q1, q2, x1, x2, p, xa) = (5.0, 2e3, 6e3, 1.0, 4.0, 3e3, 3.5);
    let mut t = bench();
    let s = t.mtype("S", rect(0.1, 0.3));
    let id = t.beam(s, (0.0, 0.0, 0.0), (l, 0.0, 0.0));
    t.sup(&start(id), "FFFFPP");
    t.sup(&end(id), "PFFPPP");
    let a = t.case("trap", false);
    let b = t.case("point", false);
    let key = format!("m:beam:{id}");
    t.load(a, "line", &key, "-z", json!({"q1": q1, "q2": q2, "x1": x1, "x2": x2}));
    t.load(b, "point", &key, "-z", json!({"f": p, "x": xa}));
    t.combo("ULS", vec![(a, 1.35), (b, 1.5)]);
    let o = t.solve();
    let w = 0.5 * (q1 + q2) * (x2 - x1);
    let xc = x1 + (x2 - x1) * (q1 + 2.0 * q2) / (3.0 * (q1 + q2));
    let (ra, rb) = (w * xc / l, p * xa / l);
    close("B12 trap R_end", reaction(&o, 0, &end(id))[2], ra, TOL);
    close("B12 trap R_start", reaction(&o, 0, &start(id))[2], w - ra, TOL);
    close("B12 point R_end", reaction(&o, 1, &end(id))[2], rb, TOL);
    let n = o.input.node_of(&end(id)).unwrap();
    let combo = o.solution.combos[0].reactions.iter().find(|r| r.0 == n).unwrap().1[2];
    close("B12 combo R_end", combo, 1.35 * ra + 1.5 * rb, TOL);
}

#[test]
fn b13_two_way_slab_on_four_beams() {
    // 45 degree tributary of a 6 x 4 slab: long beams q (Lx Ly/2 - Ly^2/4) = 8 q,
    // short beams q Ly^2 / 4 = 4 q; sum = q A; the long beam ordinate is q x up to x = Ly/2
    let (lx, ly, q) = (6.0, 4.0, 5e3);
    let mut t = bench();
    let s = t.mtype("S", rect(0.2, 0.4));
    let c = [(0.0, 0.0, 0.0), (lx, 0.0, 0.0), (lx, ly, 0.0), (0.0, ly, 0.0)];
    let ids: Vec<i64> = (0..4).map(|k| t.beam(s, c[k], c[(k + 1) % 4])).collect();
    let layer = ddc_model::LayerSpec { material: "Concrete".into(), thickness: 0.2 };
    let st = t.m.add_slab_type("RC 200", &[layer]).unwrap();
    let slab = t
        .m
        .add_slab(&ddc_model::SlabSpec {
            level: t.l0,
            slab_type: st,
            offset: 0.1,
            outer: vec![(0.0, 0.0), (lx, 0.0), (lx, ly), (0.0, ly)],
            holes: vec![],
        })
        .unwrap();
    // corner nodes belong to the lowest beam id meeting there (owner rule)
    for n in [start(ids[0]), end(ids[0]), end(ids[1]), end(ids[2])] {
        t.sup(&n, "FFFPPP");
    }
    let lc = t.case("q", false);
    t.load(lc, "area", &format!("p:slab:{slab}"), "-z", json!({"p": q}));
    let o = t.solve();
    let a = t.m.analytical().unwrap();
    let mut per = vec![0.0; 4];
    for l in &o.input.frame.cases[0].loads {
        if let ddc_fem::Load::Dist { member, a: x0, b: x1, w1, w2, .. } = l {
            let mi = o.input.segs[*member].member;
            let k = ids.iter().position(|i| *i == a.members[mi].id).unwrap();
            per[k] += -0.5 * (w1[2] + w2[2]) * (x1 - x0);
        }
    }
    let long = q * (lx * ly / 2.0 - ly * ly / 4.0);
    let short = q * ly * ly / 4.0;
    close("B13 long 1", per[0], long, 1e-12);
    close("B13 short 1", per[1], short, 1e-12);
    close("B13 long 2", per[2], long, 1e-12);
    close("B13 short 2", per[3], short, 1e-12);
    let rz: f64 = o.solution.cases[0].reactions.iter().map(|r| r.1[2]).sum();
    close("B13 sum R", rz, q * lx * ly, 1e-9);
}

#[test]
fn self_weight_of_a_cantilever() {
    // R = rho A g L, rho = 7850 kg/m3 (steel class value)
    let (l, b, h) = (3.0, 0.1, 0.2);
    let mut t = bench();
    let s = t.mtype("S", rect(b, h));
    let id = t.beam(s, (0.0, 0.0, 0.0), (l, 0.0, 0.0));
    t.sup(&start(id), "FFFFFF");
    t.case("G", true);
    let o = t.solve();
    let w = 7850.0 * b * h * ddc_calc::G;
    close("SW Rz", reaction(&o, 0, &start(id))[2], w * l, TOL);
    close("SW M0", reaction(&o, 0, &start(id))[4].abs(), w * l * l / 2.0, TOL);
}

#[test]
fn unsupported_beam_is_blocked_with_a_node_key() {
    let mut t = bench();
    let s = t.mtype("S", rect(0.1, 0.2));
    let id = t.beam(s, (0.0, 0.0, 0.0), (3.0, 0.0, 0.0));
    let c = t.case("P", false);
    t.load(c, "point", &end(id), "-z", json!({"f": 1e3}));
    match run(&t.m, &RunOptions::default()) {
        Err(ddc_calc::CalcError::Blocked(v)) => {
            let u = v.iter().find(|i| i.code == "unstable").expect("unstable issue");
            assert!(u.blocking && u.key.starts_with("n:m:beam:"), "{u:?}");
        }
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("solved a mechanism"),
    }
    assert!(t.m.calc_runs().unwrap().is_empty(), "a blocked run is not stored");
}

/// B12 model: two cases and a combination.
fn b12_model() -> B {
    let mut t = bench();
    let s = t.mtype("S", rect(0.1, 0.3));
    let id = t.beam(s, (0.0, 0.0, 0.0), (5.0, 0.0, 0.0));
    t.sup(&start(id), "FFFFPP");
    t.sup(&end(id), "PFFPPP");
    let a = t.case("trap", true);
    let b = t.case("point", false);
    let key = format!("m:beam:{id}");
    t.load(a, "line", &key, "-z", json!({"q1": 2e3, "q2": 6e3, "x1": 1.0, "x2": 4.0}));
    t.load(b, "point", &key, "-z", json!({"f": 3e3, "x": 3.5}));
    t.combo("ULS", vec![(a, 1.35), (b, 1.5)]);
    t
}

#[test]
fn runs_are_deterministic_and_stored_with_their_input() {
    let t = b12_model();
    let o1 = t.solve();
    let o2 = run(&t.m, &RunOptions { store: false, ..RunOptions::default() }).unwrap();
    assert_eq!(o1.input_hash, o2.input_hash);
    assert_eq!(ddc_calc::run::result_blobs(&o1), ddc_calc::run::result_blobs(&o2));
    // the stored run is found by its hash and carries the same bytes
    let id = t.m.calc_run_for(&o1.input_hash).unwrap().unwrap();
    assert_eq!(Some(id), o1.run_id);
    for (part, bytes) in ddc_calc::run::result_blobs(&o1) {
        assert_eq!(t.m.calc_blob(id, part).unwrap().as_deref(), Some(&bytes[..]), "{part}");
    }
    // the input blob alone reproduces the solution bit for bit
    let input = t.m.calc_blob(id, "input").unwrap().unwrap();
    assert_eq!(ddc_calc::run::input_hash(&input), o1.input_hash);
    let j: J = serde_json::from_slice(&input).unwrap();
    let fr = ddc_calc::run::frame_from_json(&j).unwrap();
    let s = ddc_fem::solve(&fr, &RunOptions::default().fem).unwrap();
    assert_eq!(s, o1.solution);
}

// ---- sa5: properties on generated frames -------------------------------------------------

/// xorshift64*: a fixed seed gives the same frame on every platform.
struct Rng(u64);

impl Rng {
    fn f(&mut self, lo: f64, hi: f64) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let u = (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64;
        lo + (hi - lo) * u
    }
    fn pick(&mut self, n: usize) -> usize {
        (self.f(0.0, n as f64) as usize).min(n - 1)
    }
}

/// Space frame: (nx + 1) x (ny + 1) columns on fixed pads, `storeys` storeys, beams both ways
/// at every floor. `mirror` reflects it about x = 0. Returns the column ids per storey.
fn space_frame(t: &mut B, seed: u64, storeys: usize, mirror: bool) -> Vec<Vec<i64>> {
    let mut r = Rng(seed);
    let (nx, ny) = (2, 2);
    let mut xs = vec![0.0];
    let mut ys = vec![0.0];
    for _ in 0..nx {
        let last = *xs.last().unwrap();
        xs.push(last + r.f(3.0, 7.0));
    }
    for _ in 0..ny {
        let last = *ys.last().unwrap();
        ys.push(last + r.f(3.0, 7.0));
    }
    if mirror {
        xs.iter_mut().for_each(|x| *x = -*x);
    }
    let mut lv = vec![t.l0];
    let mut z = 0.0;
    let mut zs = vec![0.0];
    for k in 0..storeys {
        z += r.f(2.8, 4.0);
        zs.push(z);
        lv.push(t.m.add_level(&format!("L{}", k + 1), z).unwrap());
    }
    let a = r.f(0.3, 0.5);
    let ct = t.mtype("C", rect(a, a));
    let (bb, bh) = (r.f(0.2, 0.3), r.f(0.4, 0.6));
    let bt = t.mtype("B", rect(bb, bh));
    let mut cols = vec![];
    for k in 0..storeys {
        let mut row = vec![];
        for &x in &xs {
            for &y in &ys {
                let s = ColumnSpec {
                    level: lv[k],
                    top_level: Some(lv[k + 1]),
                    member_type: ct,
                    structural: true,
                    at: (x, y),
                    base_offset: 0.0,
                    top_offset: 0.0,
                    rotation: 0.0,
                    top_at: None,
                };
                let c = t.m.add_column(&s).unwrap();
                if k == 0 {
                    footing(&mut t.m, t.l0, c);
                }
                row.push(c);
            }
        }
        cols.push(row);
    }
    for &z in &zs[1..] {
        for (i, &x) in xs.iter().enumerate() {
            for (j, &y) in ys.iter().enumerate() {
                if i + 1 < xs.len() {
                    t.beam(bt, (x, y, z), (xs[i + 1], y, z));
                }
                if j + 1 < ys.len() {
                    t.beam(bt, (x, y, z), (x, ys[j + 1], z));
                }
            }
        }
    }
    cols
}

#[test]
fn maxwell_betti_reciprocity_on_generated_frames() {
    // u_A(j, b) = u_B(i, a) for unit forces P_A = e_a at node i and P_B = e_b at node j
    for seed in [1u64, 7, 42, 2026, 0x5eed] {
        let mut t = bench();
        let cols = space_frame(&mut t, seed, 2, false);
        let mut r = Rng(seed ^ 0xabcd);
        let tops: Vec<i64> = cols.iter().flatten().copied().collect();
        let (ci, cj) = (tops[r.pick(tops.len())], tops[r.pick(tops.len())]);
        let (da, db) = (["x", "y", "z"][r.pick(3)], ["x", "y", "z"][r.pick(3)]);
        let (ni, nj) = (format!("n:m:column:{ci}:end"), format!("n:m:column:{cj}:end"));
        let a = t.case("A", false);
        let b = t.case("B", false);
        t.load(a, "point", &ni, da, json!({"f": 1.0}));
        t.load(b, "point", &nj, db, json!({"f": 1.0}));
        t.combo("A+B", vec![(a, 1.0), (b, 1.0)]);
        let o = t.solve();
        let ax = |d: &str| ["x", "y", "z"].iter().position(|k| *k == d).unwrap();
        let uab = disp(&o, 0, &nj)[ax(db)];
        let uba = disp(&o, 1, &ni)[ax(da)];
        let scale = o.solution.cases.iter().flat_map(|c| c.disp.iter().flatten());
        let scale = scale.fold(0.0f64, |m, x| m.max(x.abs()));
        assert!((uab - uba).abs() <= 1e-9 * scale, "seed {seed}: {uab:e} vs {uba:e}");
        // superposition: the combination is the sum of the cases
        for n in 0..o.input.frame.nodes.len() {
            for k in 0..6 {
                let s = o.solution.cases[0].disp[n][k] + o.solution.cases[1].disp[n][k];
                let c = o.solution.combos[0].disp[n][k];
                assert!((s - c).abs() <= 1e-12 * scale.max(1e-300), "seed {seed}");
            }
        }
    }
}

#[test]
fn mirrored_frame_gives_mirrored_results() {
    // reflection x -> -x: u' = (-ux, uy, uz), rotations (axial vectors) r' = (rx, -ry, -rz)
    let solve = |mirror: bool| {
        let mut t = bench();
        let cols = space_frame(&mut t, 99, 2, mirror);
        let c = t.case("W", true);
        let sx = if mirror { "-x" } else { "x" };
        let top = |k: usize| format!("n:m:column:{}:end", cols[1][k]);
        t.load(c, "point", &top(0), sx, json!({"f": 2e4}));
        t.load(c, "point", &top(4), "-z", json!({"f": 5e4}));
        t.load(c, "point", &top(7), "y", json!({"f": 1e4}));
        t.solve()
    };
    let (o, m) = (solve(false), solve(true));
    let fo = &o.input.frame.nodes;
    let fm = &m.input.frame.nodes;
    assert_eq!(fo.len(), fm.len());
    let sign = [-1.0, 1.0, 1.0, 1.0, -1.0, -1.0];
    let scale = o.solution.cases[0].disp.iter().flatten().fold(0.0f64, |a, x| a.max(x.abs()));
    for (i, p) in fo.iter().enumerate() {
        let d = |q: &[f64; 3]| (q[0] + p[0]).abs() + (q[1] - p[1]).abs() + (q[2] - p[2]).abs();
        let j = (0..fm.len()).min_by(|&a, &b| d(&fm[a]).total_cmp(&d(&fm[b]))).unwrap();
        assert!(d(&fm[j]) < 1e-9, "no mirror node for {p:?}");
        for k in 0..6 {
            let (u, v) = (o.solution.cases[0].disp[i][k], m.solution.cases[0].disp[j][k]);
            assert!((u - sign[k] * v).abs() <= 1e-9 * scale, "node {i} dof {k}: {u:e} {v:e}");
        }
    }
}

#[test]
fn same_run_after_save_and_reopen() {
    let path = std::env::temp_dir().join(format!("ddc_calc_reopen_{}.db", std::process::id()));
    let p = path.to_str().unwrap().to_string();
    let (hash, blobs) = {
        let mut t = B { m: Model::open(&p).unwrap(), l0: 0 };
        t.l0 = t.m.add_level("L0", 0.0).unwrap();
        let cols = space_frame(&mut t, 5, 1, false);
        let c = t.case("G", true);
        let top = format!("n:m:column:{}:end", cols[0][0]);
        t.load(c, "point", &top, "x", json!({"f": 1e4}));
        let o = t.solve();
        (o.input_hash.clone(), ddc_calc::run::result_blobs(&o))
    };
    let m = Model::open(&p).unwrap();
    let o = run(&m, &RunOptions { store: false, ..RunOptions::default() }).unwrap();
    assert_eq!(o.input_hash, hash);
    assert_eq!(ddc_calc::run::result_blobs(&o), blobs);
    assert!(m.calc_run_for(&hash).unwrap().is_some());
    drop(m);
    let _ = std::fs::remove_file(&path);
}
