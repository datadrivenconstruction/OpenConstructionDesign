//! Family editor tests: plane sketch, formulas and conditions, visibility, insertion point,
//! nested parts, validation mode.

use super::flex::{self, Failure};
use super::{Family, Lod, Value};
use crate::sketch_solve::Verdict;

const DESK: &str = r#"family Desk
category furniture
param Width length type 1200mm
param Depth length type 600mm
param Height length type 740mm
param Top length type 40mm
param HasShelf bool type 1
param Finish material type = if(Width > 1500mm, "Steel", "Oak")
ref CenterLR x = 0
ref CenterFB y = 0
ref Left x ~ -500mm
ref Right x ~ 500mm
ref Front y ~ -250mm
ref Back y ~ 250mm
eq Left CenterLR Right
eq Front CenterFB Back
dim Width Left Right
dim Depth Front Back
extrude Board rect(Left, Front, Right, Back) z(Height - Top, Height) mat(Finish)
extrude Shelf rect(Left, Front, Right, Back) z(200mm, 220mm)
visible Shelf HasShelf
origin Left, Front, 0
type "Small" Width=800mm
"#;

fn num(v: f64) -> Value {
    Value::Num(v)
}

#[test]
fn equal_and_dims_drive_free_planes() {
    let f = Family::parse(DESK).unwrap();
    let r = f.resolve(&[], &[]).unwrap();
    assert!((r.num("Left").unwrap() + 0.6).abs() < 1e-9);
    assert!((r.num("Right").unwrap() - 0.6).abs() < 1e-9);
    assert!((r.num("Back").unwrap() - 0.3).abs() < 1e-9);
    let rep = f.rig_report(&r).unwrap();
    assert_eq!(rep.verdict, Verdict::Well, "{rep:?}");
    assert_eq!(rep.dof, 0);
    // the type changes the width; planes follow
    let r = f.resolve(f.type_values("Small").unwrap(), &[]).unwrap();
    assert!((r.num("Right").unwrap() - 0.4).abs() < 1e-9);
}

#[test]
fn under_and_over_constrained_reports() {
    // without left-right equality: one degree of freedom
    let t = DESK.replace("eq Left CenterLR Right\n", "");
    let f = Family::parse(&t).unwrap();
    let r = f.resolve(&[], &[]).unwrap();
    let rep = f.rig_report(&r).unwrap();
    assert_eq!(rep.verdict, Verdict::Under);
    assert_eq!(rep.dof, 1);
    assert!(rep.free.contains(&"Left".to_string()) && rep.free.contains(&"Right".to_string()));
    // consistent redundancy: the half-width is also given by a dimension
    let t = DESK.replace("dim Depth", "dim 600mm Left CenterLR\ndim Depth");
    let f = Family::parse(&t).unwrap();
    let r = f.resolve(&[], &[]).unwrap();
    let rep = f.rig_report(&r).unwrap();
    assert_eq!(rep.verdict, Verdict::Over);
    assert_eq!(rep.redundant.len(), 1, "{:?}", rep.redundant);
    assert!(rep.redundant[0].ends_with("Left CenterLR"));
    // indices in Ext::cons: eq, eq, dim Width, dim 600mm (3), dim Depth
    assert_eq!(rep.redundant_ids, vec![3]);
    // another width makes the redundant dimension inconsistent: the error names only the minimal
    // set (front/back planes do not take part)
    let e = f
        .resolve(&[("Width".into(), num(1.0))], &[])
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("constraints conflict") && e.contains("Left CenterLR"),
        "{e}"
    );
    assert!(!e.contains("Front"), "{e}");
}

#[test]
fn dragging_a_determined_plane_names_the_constraints() {
    use super::rig::drag_check;
    let f = Family::parse(DESK).unwrap();
    let r = f.resolve(&[], &[]).unwrap();
    // Left is held by equality and width: the shift does not snap back silently
    let by = drag_check(&f, &r, "Left", -0.7)
        .unwrap()
        .expect("determined");
    assert_eq!(by.len(), 2, "{by:?}");
    assert!(by.iter().any(|l| l.starts_with("dim Width")), "{by:?}");
    assert!(by.iter().any(|l| l.starts_with("eq Left")), "{by:?}");
    // without equality Left is free: dragging is allowed
    let t = DESK.replace("eq Left CenterLR Right\n", "");
    let f = Family::parse(&t).unwrap();
    let r = f.resolve(&[], &[]).unwrap();
    assert_eq!(drag_check(&f, &r, "Left", -0.7).unwrap(), None);
}

#[test]
fn text_conditionals_visibility_and_origin() {
    let f = Family::parse(DESK).unwrap();
    let r = f.resolve(&[], &[]).unwrap();
    assert_eq!(r.texts["Finish"], "Oak");
    let r = f.resolve(&[("Width".into(), num(1.6))], &[]).unwrap();
    assert_eq!(r.texts["Finish"], "Steel");
    // formula text cannot be assigned
    assert!(f
        .resolve(&[("Finish".into(), Value::Text("Ash".into()))], &[])
        .is_err());
    assert!(f.assignments("Finish=\"Ash\"").is_err());
    let with = f.build(&f.resolve(&[], &[]).unwrap(), Lod::Medium).unwrap();
    let r = f.resolve(&[("HasShelf".into(), num(0.0))], &[]).unwrap();
    assert_eq!(r.hidden, vec!["Shelf".to_string()]);
    let without = f.build(&r, Lod::Medium).unwrap();
    assert!(without.volume < with.volume);
    assert!(without.parts.iter().all(|p| p.name != "Shelf"));
    // insertion point is the left front corner
    assert!(with.bbox[0].x.abs() < 1e-9 && with.bbox[0].y.abs() < 1e-9);
    assert!((with.bbox[1].x - 1.2).abs() < 1e-9);
}

#[test]
fn angular_parameter_drives_reference_line() {
    let f = Family::parse(
        "family Wedge\nparam Tilt angle type 30deg\nref Base y = 0\n\
         refline Slope at(0, 0) angle(~10deg)\nangle Tilt Base Slope\n\
         extrude B poly(0, 0, 1m, 0, cos(Slope) * 1m, sin(Slope) * 1m) z(0, 100mm)\n",
    )
    .unwrap();
    let r = f.resolve(&[], &[]).unwrap();
    assert!((r.num("Slope").unwrap() - 30f64.to_radians()).abs() < 1e-8);
    let b = f.build(&r, Lod::Medium).unwrap();
    // triangle 1 × sin30 / 2 × 0.1
    assert!((b.volume - 0.5 * 30f64.to_radians().sin() * 0.1).abs() < 1e-9);
}

#[test]
fn nested_families_with_type_parameter() {
    let t = r#"family Table
param W length type 1000mm
param LegH length type 700mm
param LegKind familytype type "Leg:Square"
nest L1 Leg type(Round) at(0, 0, 0) set(H = LegH)
nest L2 of(LegKind) at(W, 0, 0) rot(90deg) set(H = LegH)
begin
family Leg
param H length type 500mm
param S length type 50mm
extrude Post rect(0, 0, S, S) z(0, H)
type "Round" S=40mm
type "Square" S=60mm
end
"#;
    let f = Family::parse(t).unwrap();
    assert_eq!(f.ext.children.len(), 1);
    let r = f.resolve(&[], &[]).unwrap();
    let b = f.build(&r, Lod::Medium).unwrap();
    let want = 0.04 * 0.04 * 0.7 + 0.06 * 0.06 * 0.7;
    assert!((b.volume - want).abs() < 1e-12, "{}", b.volume);
    assert!(b.parts.iter().any(|p| p.name == "L1/Post"));
    // rotation by 90°: the second leg goes to −x from point W (without rotation the edge would be 1.06)
    assert!((b.bbox[1].x - 1.0).abs() < 1e-9, "{:?}", b.bbox);
    let r = f
        .resolve(&[("LegKind".into(), Value::Text("Leg:Round".into()))], &[])
        .unwrap();
    let b2 = f.build(&r, Lod::Medium).unwrap();
    assert!((b2.volume - 2.0 * 0.04 * 0.04 * 0.7).abs() < 1e-12);
    let r = f
        .resolve(&[("LegKind".into(), Value::Text("Leg:Hex".into()))], &[])
        .unwrap();
    assert!(f.build(&r, Lod::Medium).is_err());
    // nested-part parse errors
    assert!(Family::parse("begin\nfamily X\nend\nfamily Y\n").is_err());
    assert!(Family::parse("family Y\nnest A Nope at(0, 0, 0)\n").is_err());
    assert!(Family::parse("family Y\nbegin\nfamily X\n").is_err());
}

#[test]
fn formula_limits_and_validation() {
    let deep = format!("{}1{}", "(".repeat(70), ")".repeat(70));
    let e = Family::parse(&format!("family A\nparam X number type = {deep}\n")).unwrap_err();
    assert!(e.to_string().contains("nesting"), "{e}");
    let long = vec!["1"; 600].join(" + ");
    assert!(Family::parse(&format!("family A\nparam X number type = {long}\n")).is_err());
    // condition chain within the depth limit
    let mut f = String::from("1");
    for _ in 0..20 {
        f = format!("if(X0 > 0, {f}, 0)");
    }
    let fam = Family::parse(&format!(
        "family A\nparam X0 number type 1\nparam X number type = {f}\n"
    ))
    .unwrap();
    assert_eq!(fam.resolve(&[], &[]).unwrap().num("X"), Some(1.0));
    // a parameter formula cannot depend on the plane being solved
    let e = Family::parse("family A\nref L x ~ 0\nparam W length type = L * 2\n").unwrap_err();
    assert!(e.to_string().contains("sketch"), "{e}");
    // a dimension may be bound only to a length parameter
    let e =
        Family::parse("family A\nparam N int type 2\nref A0 x = 0\nref B0 x ~ 1m\ndim N A0 B0\n")
            .unwrap_err();
    assert!(e.to_string().contains("length"), "{e}");
    let e = Family::parse("family A\nref A0 x = 0\nref B0 y ~ 1m\ndim 1m A0 B0\n").unwrap_err();
    assert!(e.to_string().contains("axis"), "{e}");
    assert!(Family::parse("family A\nparam T text type = if(1, \"a\", Nope)\n").is_err());
}

#[test]
fn flex_reports_failures() {
    let f = Family::parse(DESK).unwrap();
    let base = f.type_values("Small").unwrap().to_vec();
    let out = flex::flex(
        &f,
        &base,
        &[
            vec![("Width".into(), num(0.9))],
            vec![("Width".into(), num(-0.1))],
            vec![("Top".into(), num(0.8))],
            vec![("Depth".into(), num(0.0))],
        ],
    );
    assert!(out[0].ok(), "{:?}", out[0].failure);
    assert_eq!(out[0].verdict, Some(Verdict::Well));
    assert!(matches!(&out[1].failure, Some(Failure::Flipped { dim, .. }) if dim.contains("Width")));
    // board thicker than the table height: the bottom is below zero but the body still builds; a thickness greater than
    // the height gives z(Height - Top, Height) with a negative bottom, but does not break
    assert!(out[2].ok() || matches!(out[2].failure, Some(Failure::Geometry(_))));
    assert!(matches!(out[3].failure, Some(Failure::Flipped { .. })));
    let s = flex::sweep(&f, &base, "Width", 0.4, 2.0, 4);
    assert_eq!(s.len(), 5);
    assert!(flex::first_failure(&s).is_none());
    let s = flex::sweep(&f, &base, "Width", 0.4, -0.4, 4);
    let (v, _) = flex::first_failure(&s).unwrap();
    assert!(v <= 0.0);
}

#[test]
fn flex_checks_angles_and_keeps_zero_offsets() {
    let f = Family::parse(
        "family Wedge\nparam Tilt angle type 30deg\nref Base y = 0\n\
         refline Slope at(0, 0) angle(~10deg)\nangle Tilt Base Slope\n\
         extrude B poly(0, 0, 1m, 0, cos(Slope) * 1m, sin(Slope) * 1m) z(0, 100mm)\n",
    )
    .unwrap();
    let t = |deg: f64| flex::try_values(&f, &[], &[("Tilt".into(), num(deg.to_radians()))]);
    assert!(t(60.0).ok(), "{:?}", t(60.0).failure);
    // the angle switched sides (including across ±180°): the line flipped
    for d in [-20.0, 200.0] {
        assert!(
            matches!(t(d).failure, Some(Failure::Flipped { ref dim, .. }) if dim.contains("Tilt")),
            "{d}"
        );
    }
    // a legitimate zero type offset is not counted as a flip
    let z = Family::parse(
        "family Z\nparam Off length type 0mm\nref A x = 0\nref B x ~ 10mm\ndim Off A B\n\
         extrude S rect(0, 0, 1m, 1m) z(0, 1m)\n",
    )
    .unwrap();
    for v in [0.0, -0.1, 0.1] {
        let o = flex::try_values(&z, &[], &[("Off".into(), num(v))]);
        assert!(o.ok(), "{v}: {:?}", o.failure);
    }
}
