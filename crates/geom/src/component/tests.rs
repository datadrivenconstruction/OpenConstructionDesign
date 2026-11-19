use super::expr::{parse, q, LENGTH, NUMBER};
use super::*;
use std::f64::consts::PI;

fn ev(s: &str) -> Result<Q, String> {
    parse(s).map_err(|e| e.to_string())?.eval(&|n| match n {
        "W" => Some(q(1.6, LENGTH)),
        "D" => Some(q(0.8, LENGTH)),
        _ => None,
    })
}

fn near(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "{a} vs {b}");
}

#[test]
fn formulas_with_units() {
    near(ev("1600mm").unwrap().v, 1.6, 1e-15);
    near(ev("2'6\"").unwrap().v, 0.762, 1e-15);
    near(ev("3ft + 6in").unwrap().v, 1.0668, 1e-15);
    let a = ev("W * D").unwrap();
    assert_eq!(a.d, expr::AREA);
    near(a.v, 1.28, 1e-15);
    near(ev("(W - 2 * 50mm) / 2").unwrap().v, 0.75, 1e-15);
    near(ev("max(W, D, 1m)").unwrap().v, 1.6, 0.0);
    near(ev("min(W, D)").unwrap().v, 0.8, 0.0);
    near(ev("if(W > 1200mm, 6, 4)").unwrap().v, 6.0, 0.0);
    near(ev("if(W > 2m or D < 0.5m, 1m, 2m)").unwrap().v, 2.0, 0.0);
    near(ev("round(W / 3, 10mm)").unwrap().v, 0.53, 1e-15);
    near(ev("floor(W / 0.5m)").unwrap().v, 3.0, 0.0);
    near(ev("sin(30deg)").unwrap().v, 0.5, 1e-15);
    assert_eq!(ev("atan2(D, D)").unwrap().d, expr::ANGLE);
    assert_eq!(ev("W == 1.6m").unwrap(), q(1.0, NUMBER));
    assert_eq!(ev("0.1m + 0.2m == 0.3m").unwrap(), q(1.0, NUMBER));
    // dimensionality
    assert!(ev("W + 1").unwrap_err().contains("length and number"));
    assert!(ev("if(W > 1m, 1m, 2)").unwrap_err().contains("branches"));
    assert!(ev("sin(W)").unwrap_err().contains("sin"));
    assert!(ev("W / (D - D)").unwrap_err().contains("zero"));
    assert!(ev("Q + W").unwrap_err().contains("unknown name 'Q'"));
    assert!(parse("1 +").is_err() && parse("min(1,").is_err() && parse("2 m").is_err());
}

#[test]
fn literal_round_trips() {
    for v in [0.1, 1.0 / 3.0, -2.5e-7, 1e21, 0.0] {
        for d in [LENGTH, NUMBER, expr::ANGLE, expr::AREA] {
            let s = expr::literal(q(v, d));
            assert_eq!(ev(&s).unwrap(), q(v, d), "{s}");
        }
    }
}

const BOX: &str = "family Box
category generic
param A length type 1m
param B length type = A * 2
param C length instance = B + A
param N int instance 3
ref Top z = C
extrude Body rect(0, 0, A, B) z(0, Top)
";

#[test]
fn dependency_order_and_resolution() {
    let f = Family::parse(BOX).unwrap();
    let r = f.resolve(&[], &[]).unwrap();
    near(r.num("C").unwrap(), 3.0, 0.0);
    near(r.num("Top").unwrap(), 3.0, 0.0);
    let r = f
        .resolve(
            &[("A".into(), Value::Num(0.5))],
            &[("N".into(), Value::Num(7.0))],
        )
        .unwrap();
    near(r.num("C").unwrap(), 1.5, 0.0);
    near(r.num("N").unwrap(), 7.0, 0.0);
    // a formula cannot be set; the type is not on the instance
    assert!(matches!(
        f.resolve(&[("B".into(), Value::Num(1.0))], &[]),
        Err(FamilyError::Param { .. })
    ));
    assert!(f.resolve(&[], &[("A".into(), Value::Num(1.0))]).is_err());
    assert_eq!(f.locks(), vec![("Body".to_string(), "Top".to_string())]);
    let b = f.build(&r, Lod::Medium).unwrap();
    near(b.volume, 0.5 * 1.0 * 1.5, 1e-15);
}

#[test]
fn cycle_is_an_error_with_path() {
    let src = "family Loop
param A length type = C + 1mm
param B length type = A * 2
param C length type = B - 1mm
";
    match Family::parse(src) {
        Err(FamilyError::Cycle(p)) => {
            assert_eq!(p.first(), p.last());
            assert_eq!(p.len(), 4);
            for n in ["A", "B", "C"] {
                assert!(p.iter().any(|x| x == n));
            }
        }
        o => panic!("{o:?}"),
    }
    let self_ref = "family S\nparam A length type = A\n";
    assert!(matches!(Family::parse(self_ref), Err(FamilyError::Cycle(p)) if p == ["A", "A"]));
    let bad_unit = "family U\nparam A length type 3\n";
    assert!(matches!(
        Family::parse(bad_unit),
        Err(FamilyError::Parse { line: 2, .. })
    ));
    let unknown = "family U\nparam A length type = Z\n";
    assert!(Family::parse(unknown).is_err());
}

const TABLE: &str = r#"family Table
category furniture
param Width length type 1600mm
param Depth length type 800mm
param Height length type 750mm
param Top length type 40mm
param Leg length type 60mm
param LegH length type = Height - Top
param Wood material type "Oak"
ref Left x = -Width / 2
ref Right x = Width / 2
ref Front y = -Depth / 2
ref Back y = Depth / 2
type "1600x800" Width=1600mm, Depth=800mm
type "60x30in" Width=60in, Depth=30in, Height=29in
extrude Top rect(Left, Front, Right, Back) z(LegH, Height) mat(Wood) show(medium, fine, 3d)
extrude Leg1 rect(Left, Front, Left + Leg, Front + Leg) z(0, LegH) mat(Wood) show(medium, fine, 3d)
extrude Leg2 rect(Right - Leg, Front, Right, Front + Leg) z(0, LegH) mat(Wood) show(medium, fine, 3d)
extrude Leg3 rect(Right - Leg, Back - Leg, Right, Back) z(0, LegH) mat(Wood) show(medium, fine, 3d)
extrude Leg4 rect(Left, Back - Leg, Left + Leg, Back) z(0, LegH) mat(Wood) show(medium, fine, 3d)
extrude Box rect(Left, Front, Right, Back) z(0, Height) show(coarse, 3d)
symbol rect(Left, Front, Right, Back)
"#;

#[test]
fn table_1600x800_volume() {
    let f = Family::parse(TABLE).unwrap();
    let r = f.resolve(f.type_values("1600x800").unwrap(), &[]).unwrap();
    let b = f.build(&r, Lod::Medium).unwrap();
    // top 1.6x0.8x0.04 + 4 legs 0.06x0.06x0.71; the "box" coarse body is also a solid, but here
    // the volume counts all bodies: check without it via the parts
    let parts: f64 = b
        .parts
        .iter()
        .filter(|p| p.name != "Box")
        .map(|p| p.volume)
        .sum();
    near(parts, 1.6 * 0.8 * 0.04 + 4.0 * 0.06 * 0.06 * 0.71, 1e-15);
    near(parts, 0.061424, 1e-15);
    assert_eq!(b.parts[0].material, "Oak");
    // mesh: 5 visible prisms of 12 triangles each, closed, volume matches
    assert_eq!(b.mesh.triangles.len(), 60);
    assert!(b.mesh.is_watertight());
    near(b.mesh.volume(), 0.061424, 1e-12);
    assert_eq!(b.bbox[1].z, 0.75);
    // coarse: the box only
    let c = f.build(&r, Lod::Coarse).unwrap();
    near(c.mesh.volume(), 1.6 * 0.8 * 0.75, 1e-12);
    // plan: the symbol is 4 lines (bodies without plan)
    assert_eq!(b.plan.len(), 4);
    let imp = f.resolve(f.type_values("60x30in").unwrap(), &[]).unwrap();
    near(imp.num("LegH").unwrap(), 29.0 * 0.0254 - 0.04, 1e-15);
}

/// Bowl: profile (r, z) with a bottom of thickness t and a wall of thickness t up to height H.
const BASIN: &str = "family Basin
category plumbing
host wall
param R length type 250mm
param H length type 180mm
param T length type 15mm
revolve Bowl poly(0,0, R,0, R,H, R-T,H, R-T,T, 0,T) axis(0, 0)
";

#[test]
fn washbasin_revolve_volume_is_exact() {
    let f = Family::parse(BASIN).unwrap();
    assert_eq!(f.hosting, Hosting::Wall);
    assert_eq!(f.ifc.0, "IfcSanitaryTerminal");
    let r = f.resolve(&[], &[]).unwrap();
    let (rr, h, t) = (0.25f64, 0.18f64, 0.015f64);
    let exact = PI * (t * rr * rr + (h - t) * (rr * rr - (rr - t) * (rr - t)));
    for lod in [Lod::Coarse, Lod::Fine] {
        let b = f.build(&r, lod).unwrap();
        near(b.volume, exact, 1e-15);
        assert!(b.mesh.volume() > 0.0);
        // inscribed polygon: the mesh is smaller by ~ (1 − sin(2π/n)/(2π/n))
        let n = lod.segments() as f64;
        let k = (TAU_N(n)).sin() / TAU_N(n);
        near(b.mesh.volume(), exact * k, exact * 1e-9);
    }
    // half a turn gives half the volume, caps close it
    let half = "family Half\nrevolve S rect(0.1m, 0, 0.2m, 0.1m) angle(180deg)\n";
    let f = Family::parse(half).unwrap();
    let b = f.build(&f.resolve(&[], &[]).unwrap(), Lod::Fine).unwrap();
    near(b.volume, PI * 0.01 * 0.15, 1e-15);
    assert!(b.mesh.is_watertight());
}

#[test]
fn revolve_with_points_on_the_axis_is_closed() {
    let f = Family::parse(BASIN).unwrap();
    let cone = Family::parse("family Neck\nrevolve N rect(0, 0, 0.1m, 0.2m)\nrevolve H rect(0, 0, 0.1m, 0.2m) angle(90deg)\n").unwrap();
    for f in [f, cone] {
        let r = f.resolve(&[], &[]).unwrap();
        for lod in [Lod::Coarse, Lod::Fine] {
            assert!(f.build(&r, lod).unwrap().mesh.is_watertight(), "{} {lod:?}", f.name);
        }
    }
}

#[allow(non_snake_case)]
fn TAU_N(n: f64) -> f64 {
    2.0 * PI / n
}

#[test]
fn sweep_blend_void_and_errors() {
    let src = "family Mix
sweep Rail rect(-10mm, -10mm, 10mm, 10mm) path(0m,0m,0m, 1m,0m,0m, 1m,1m,0m, 1m,1m,1m)
blend Cone circle(0m, 0m, 200mm) circle(0m, 0m, 100mm) z(0m, 300mm)
extrude Slab rect(2m, 0m, 3m, 1m) z(0m, 1m)
extrude Hole void rect(2.25m, 0.25m, 2.75m, 0.75m) z(0m, 1m)
";
    let f = Family::parse(src).unwrap();
    let b = f.build(&f.resolve(&[], &[]).unwrap(), Lod::Fine).unwrap();
    let vol = |n: &str| b.parts.iter().find(|p| p.name == n).unwrap().volume;
    // sweep with bisector joints: A · axis length
    near(vol("Rail"), 0.0004 * 3.0, 1e-15);
    // truncated cone
    near(vol("Cone"), PI * 0.3 / 3.0 * (0.04 + 0.02 + 0.01), 1e-15);
    near(vol("Hole"), 0.25, 1e-15);
    near(b.volume, vol("Rail") + vol("Cone") + 1.0 - 0.25, 1e-12);
    assert!(!b.parts[3].visible);
    // cut outside the bodies, differing vertex counts, profile crossing the axis
    let out = "family V\nextrude A rect(0m,0m,1m,1m) z(0m,1m)\nextrude B void rect(2m,0m,3m,1m) z(0m,1m)\n";
    let f = Family::parse(out).unwrap();
    assert!(f.build(&f.resolve(&[], &[]).unwrap(), Lod::Medium).is_err());
    let bl = "family V\nblend A rect(0m,0m,1m,1m) circle(0m,0m,1m) z(0m,1m)\n";
    let f = Family::parse(bl).unwrap();
    assert!(f.build(&f.resolve(&[], &[]).unwrap(), Lod::Medium).is_err());
    let rv = "family V\nrevolve A rect(-1m,0m,1m,1m)\n";
    let f = Family::parse(rv).unwrap();
    assert!(f.build(&f.resolve(&[], &[]).unwrap(), Lod::Medium).is_err());
}

#[test]
fn extrude_along_x_and_y_is_outward() {
    for d in ["x", "y"] {
        let src = format!("family E\nextrude A rect(0m,0m,2m,3m) dir({d}) z(0m,5m)\n");
        let f = Family::parse(&src).unwrap();
        let b = f.build(&f.resolve(&[], &[]).unwrap(), Lod::Medium).unwrap();
        near(b.volume, 30.0, 1e-12);
        near(b.mesh.volume(), 30.0, 1e-12);
        assert_eq!(b.plan.len(), 4);
    }
}

#[test]
fn parse_errors_name_the_line() {
    let e = Family::parse("family X\nextrude A rect(0m,0m,1m) z(0m,1m)\n").unwrap_err();
    assert!(matches!(e, FamilyError::Parse { line: 2, .. }), "{e}");
    let e = Family::parse("family X\nwobble\n").unwrap_err();
    assert!(e.to_string().contains("line 2"));
    let e = Family::parse(
        "family X\nparam M material type \"A\"\nextrude A rect(0m,0m,1m,1m) z(0m,1m) mat(N)\n",
    );
    assert!(e.is_err());
    // multi-line instruction
    let f = Family::parse("family X\nextrude A rect(0m, 0m,\n  1m, 1m) z(0m, 1m)\n").unwrap();
    assert_eq!(f.solids.len(), 1);
}

#[test]
fn connectors_resolve_with_parameters() {
    let f = Family::parse(
        "family Basin\nparam H length type 800mm\nconnector Waste pipe sanitary at(0, 100mm, H - 200mm) dir(0, 0, -2) size(40mm)\nconnector Feed duct at(0, 0, H) dir(0, 0, 1) size(100mm)\n",
    )
    .unwrap();
    let r = f.resolve(&[], &[]).unwrap();
    let p = f.ports(&r).unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(p[0].system, Some(crate::mep::SystemKind::Sanitary));
    assert!((p[0].at.z - 0.6).abs() < 1e-12 && (p[0].dir.z + 1.0).abs() < 1e-12);
    assert!((p[0].size - 0.04).abs() < 1e-12);
    assert_eq!(p[1].domain, crate::mep::Domain::Duct);
    assert!(Family::parse("family X\nconnector A pipe at(0,0,Q) dir(0,0,1) size(1mm)\n").is_err());
    assert!(Family::parse("family X\nconnector A at(0,0,0) dir(0,0,1) size(1mm)\n").is_err());
}

#[test]
fn if_branches_are_lazy() {
    let z = |s: &str| {
        parse(s).map_err(|e| e.to_string())?.eval(&|n| match n {
            "w" => Some(q(0.0, NUMBER)),
            "L" => Some(q(3.0, LENGTH)),
            _ => None,
        })
    };
    near(z("if(w > 0, L / w, 0)").unwrap().v, 0.0, 0.0);
    near(z("if(w == 0, 0, L / w)").unwrap().v, 0.0, 0.0);
    assert!(z("if(w == 0, L / w, 0)").is_err());
}

#[test]
fn rounding_arity_is_an_error_not_a_panic() {
    for s in [
        "round()",
        "floor()",
        "ceil(1, 2, 3)",
        "abs()",
        "if(1)",
        "atan2(1)",
    ] {
        assert!(ev(s).is_err(), "{s}");
    }
}

#[test]
fn dimension_exponent_overflow_is_an_error() {
    let big = vec!["W"; 130].join(" * ");
    assert!(ev(&big).unwrap_err().contains("overflow"));
    let inv = vec!["W"; 140].join(" / ");
    assert!(ev(&inv).is_err());
}

mod shared_evaluator {
    use super::super::expr::{parse_in, Code, Cx, Dialect, Fail, MASS};
    use super::*;

    fn env(n: &str) -> Result<Q, Fail> {
        match n {
            "W" => Ok(q(1.6, LENGTH)),
            "D" => Ok(q(0.8, LENGTH)),
            "m" => Ok(q(3.0, MASS)),
            "Net area" | "b" => Ok(q(5.0, NUMBER)),
            _ => Err(Fail::new(Code::Env(n.into()), n)),
        }
    }

    fn run(s: &str, d: Dialect, lenient: bool, dry: bool) -> Result<Q, Fail> {
        let cx = Cx {
            env: &env,
            lenient,
            dry,
        };
        match parse_in(s, d) {
            Ok(e) => e.eval_with(&cx),
            Err(e) => Err(Fail::new(e.code, e.msg)),
        }
    }

    fn code(s: &str) -> Code {
        run(s, Dialect::Component, false, false)
            .expect_err(s)
            .code
    }

    #[test]
    fn mass_is_a_dimension() {
        let ev = |s: &str| run(s, Dialect::Component, false, false);
        assert_eq!(ev("m * 2").unwrap(), q(6.0, MASS));
        assert_eq!(ev("sqrt(m * m)").unwrap(), q(3.0, MASS));
        assert_eq!(ev("m / m").unwrap().d, NUMBER);
        assert!(ev("m + W").is_err());
        assert_eq!(MASS.to_string(), "mass");
    }

    #[test]
    fn round_takes_decimals_or_a_step() {
        near(ev("round(2.567, 2)").unwrap().v, 2.57, 1e-12);
        near(ev("floor(2.59, 1)").unwrap().v, 2.5, 1e-12);
        near(ev("ceil(2.51, 1)").unwrap().v, 2.6, 1e-12);
        near(ev("round(1234, 0 - 2)").unwrap().v, 1200.0, 1e-9);
        assert_eq!(ev("round(W, 1)").unwrap(), q(1.6, LENGTH));
        near(ev("round(W / 3, 10mm)").unwrap().v, 0.53, 1e-15);
        near(ev("round(W, 0.5m)").unwrap().v, 1.5, 1e-12);
        assert!(ev("round(2.5, 0.5)").unwrap_err().contains("whole"));
        assert!(ev("round(W, 0m)").unwrap_err().contains("positive"));
        assert!(ev("round(W)").unwrap_err().contains("needs a step"));
    }

    #[test]
    fn every_error_has_a_code() {
        let long = "1+".repeat(600);
        let deep = format!("{}1{}", "(".repeat(100), ")".repeat(100));
        let overflow = vec!["W"; 130].join(" * ");
        let cases: Vec<(&str, Code)> = vec![
            ("1 +", Code::Truncated),
            ("(1", Code::Paren),
            ("max(1 2)", Code::CallParen("max".into())),
            ("1 2", Code::Trailing('2')),
            (")", Code::Unexpected(')')),
            ("1.2.3", Code::Number("1.2.3".into())),
            (long.as_str(), Code::TooLong),
            (deep.as_str(), Code::Depth),
            ("q + 1", Code::Env("q".into())),
            ("foo(1)", Code::UnknownFunction("foo".into())),
            ("abs()", Code::ArgCount("abs".into())),
            ("W / 0", Code::DivZero),
            ("W * 1e999", Code::NotFinite),
            ("sqrt(0 - 1)", Code::SqrtNeg),
            ("sqrt(W)", Code::Root(LENGTH)),
            (
                "W + D * D",
                Code::Mismatch {
                    a: LENGTH,
                    b: expr::AREA,
                },
            ),
            ("if(W, 1, 2)", Code::Condition(LENGTH)),
            ("sin(W)", Code::Trig(LENGTH)),
            ("round(2.5, 0.5)", Code::RoundDigits),
            ("round(W, 0m)", Code::RoundStep),
            ("round(W)", Code::NeedsStep(LENGTH)),
            (overflow.as_str(), Code::Overflow),
        ];
        for (src, want) in cases {
            assert_eq!(code(src), want, "{src}");
        }
    }

    #[test]
    fn the_sheet_dialect() {
        let ev = |s: &str| run(s, Dialect::Sheet, true, false).map(|q| q.v);
        assert_eq!(ev("[Net area] * 2 + b"), Ok(15.0));
        assert_eq!(ev("ROUND(2.567; 1)"), Ok(2.6));
        assert_eq!(ev("1 = 1"), Ok(1.0));
        assert_eq!(ev("1 == 1"), Ok(1.0));
        assert_eq!(ev("1 <> 1"), Ok(0.0));
        assert_eq!(ev("2 <> 1"), Ok(1.0));
        assert_eq!(ev("2 <= 1"), Ok(0.0));
        // numbers carry no units there, names in brackets only there
        assert!(parse_in("5mm", Dialect::Sheet).is_err() && parse("5mm").is_ok());
        assert_eq!(
            parse_in("[a", Dialect::Sheet).unwrap_err().code,
            Code::Bracket
        );
        assert_eq!(parse("[a]").unwrap_err().code, Code::Unexpected('['));
        assert_eq!(parse("1 = 1").unwrap_err().code, Code::Trailing('='));
    }

    #[test]
    fn schedule_rules_are_switches_of_one_evaluator() {
        let sheet = |s: &str, lenient: bool, dry: bool| run(s, Dialect::Sheet, lenient, dry);
        assert!(sheet("W + 1", false, false).is_err());
        assert_eq!(sheet("W + 1", true, false).unwrap().d, LENGTH);
        assert_eq!(sheet("1 + W", true, false).unwrap().d, LENGTH);
        assert_eq!(sheet("max(W, 5)", true, false).unwrap().d, LENGTH);
        assert!(sheet("W + D * D", true, false).is_err());
        let zero = sheet("W / 0", false, false).unwrap_err();
        assert_eq!(zero.code, Code::DivZero);
        assert_eq!(sheet("W / 0", false, true).unwrap().d, LENGTH);
        assert_eq!(sheet("sqrt(0 - 1)", false, true).unwrap().d, NUMBER);
        assert_eq!(sheet("round(W, D)", false, true).unwrap().d, LENGTH);
    }
}
