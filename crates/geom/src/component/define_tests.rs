//! Typed family definitions: lowering to text, range limits, flex rejections, drafts.

use super::define::*;
use super::{Axis, Category, Family, Hosting, Kind, Lod, Value};

fn len(name: &str, default: f64, min: Option<f64>, max: Option<f64>) -> ParamDef {
    ParamDef {
        name: name.into(),
        kind: Kind::Length,
        instance: false,
        unit: Some("mm".into()),
        default: Some(Lit::Num(default)),
        formula: None,
        min,
        max,
    }
}

fn formula(name: &str, kind: Kind, f: &str) -> ParamDef {
    ParamDef {
        name: name.into(),
        kind,
        instance: false,
        unit: None,
        default: None,
        formula: Some(f.into()),
        min: None,
        max: None,
    }
}

fn s(v: &str) -> String {
    v.into()
}

/// Table: four free planes driven by Width/Depth, top board, a formula for the leg pitch.
fn table() -> FamilyDef {
    let count = ParamDef {
        name: "Legs".into(),
        kind: Kind::Int,
        instance: false,
        unit: None,
        default: Some(Lit::Num(4.0)),
        formula: None,
        min: Some(1.0),
        max: Some(8.0),
    };
    FamilyDef {
        name: "AiTable".into(),
        category: Category::Furniture,
        hosting: Hosting::None,
        params: vec![
            len("Width", 1600.0, Some(400.0), Some(3000.0)),
            len("Depth", 800.0, Some(300.0), Some(1500.0)),
            len("Height", 740.0, None, None),
            len("Top", 40.0, None, None),
            count,
            formula("Pitch", Kind::Length, "Width / (Legs - 2)"),
            ParamDef {
                name: "Finish".into(),
                kind: Kind::Material,
                instance: false,
                unit: None,
                default: Some(Lit::Text("Oak".into())),
                formula: None,
                min: None,
                max: None,
            },
        ],
        refs: vec![
            RefDef { name: s("Mid"), axis: Axis::X, at: RefAt::Formula(s("0")) },
            RefDef { name: s("MidY"), axis: Axis::Y, at: RefAt::Formula(s("0")) },
            RefDef { name: s("Left"), axis: Axis::X, at: RefAt::Free(s("-500mm")) },
            RefDef { name: s("Right"), axis: Axis::X, at: RefAt::Free(s("500mm")) },
            RefDef { name: s("Front"), axis: Axis::Y, at: RefAt::Free(s("-250mm")) },
            RefDef { name: s("Back"), axis: Axis::Y, at: RefAt::Free(s("250mm")) },
        ],
        constraints: vec![
            ConsDef::Eq(vec![s("Left"), s("Mid"), s("Right")]),
            ConsDef::Eq(vec![s("Front"), s("MidY"), s("Back")]),
            ConsDef::Dim { value: s("Width"), a: s("Left"), b: s("Right") },
            ConsDef::Dim { value: s("Depth"), a: s("Front"), b: s("Back") },
        ],
        forms: vec![FormDef {
            name: "Board".into(),
            shape: FormShape::Extrude {
                profile: ProfileDef::Rect([s("Left"), s("Front"), s("Right"), s("Back")]),
                dir: Axis::Z,
                from: s("Height - Top"),
                to: s("Height"),
            },
            void: false,
            material: Some(s("Finish")),
        }],
        types: vec![TypeDef {
            name: "1600x800".into(),
            values: vec![(s("Width"), Lit::Num(1600.0)), (s("Depth"), Lit::Num(800.0))],
        }],
        tests: vec![
            vec![(s("Width"), Lit::Num(900.0))],
            vec![(s("Depth"), Lit::Num(1200.0)), (s("Legs"), Lit::Num(6.0))],
        ],
    }
}

#[test]
fn define_family_roundtrip_text() {
    let d = table();
    let a = define(&d).expect("table is accepted");
    let text = d.to_text().unwrap();
    assert_eq!(a.text, text);
    // lowered text is plain family text: parses on its own to the same family
    let f = Family::parse(&text).unwrap();
    assert_eq!(f, a.family);
    assert_eq!(f.name, "AiTable");
    let names: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Width", "Depth", "Height", "Top", "Legs", "Pitch", "Finish"]);
    assert!(f.param("Pitch").unwrap().formula.is_some());
    assert_eq!(f.limits.len(), 3);
    let w = f.limits.iter().find(|l| l.param == "Width").unwrap();
    assert_eq!((w.min, w.max), (Some(0.4), Some(3.0)));
    let labels: Vec<String> = f.ext.cons.iter().map(|c| c.label()).collect();
    assert_eq!(labels[2], "dim Width Left Right");
    assert_eq!(f.solids.len(), 1);
    assert_eq!(f.types[0].values, vec![(s("Width"), Value::Num(1.6)), (s("Depth"), Value::Num(0.8))]);
    // the geometry follows the parameters: board 1.6 x 0.8 x 0.04
    let r = f.resolve(&f.types[0].values, &[]).unwrap();
    let b = f.build(&r, Lod::Medium).unwrap();
    assert!((b.volume - 1.6 * 0.8 * 0.04).abs() < 1e-9, "{}", b.volume);
    // lowering is stable
    assert_eq!(table().to_text().unwrap(), text);
}

#[test]
fn flex_failure_names_formula() {
    let mut d = table();
    d.tests.push(vec![(s("Legs"), Lit::Num(2.0))]);
    let r = define(&d).unwrap_err();
    assert_eq!(r.code, "formula", "{r}");
    assert_eq!(r.path, "/params/5");
    assert_eq!(r.test, Some(2));
    assert!(r.msg.contains("Pitch = Width / (Legs - 2)"), "{r}");
}

#[test]
fn flex_failure_names_constraint() {
    let mut d = table();
    d.params[0].min = None;
    d.tests = vec![vec![(s("Width"), Lit::Num(-100.0))]];
    let r = define(&d).unwrap_err();
    assert_eq!(r.code, "constraint", "{r}");
    assert_eq!(r.path, "/constraints/2");
    assert!(r.msg.contains("dim Width Left Right"), "{r}");
}

#[test]
fn param_out_of_range_refused() {
    let mut d = table();
    d.tests = vec![vec![(s("Width"), Lit::Num(5000.0))]];
    let r = define(&d).unwrap_err();
    assert_eq!(r.code, "out_of_range", "{r}");
    assert_eq!(r.path, "/params/0");
    assert!(r.msg.contains("Width"), "{r}");
    // a type value outside the range is refused the same way
    let mut d = table();
    d.types[0].values[1].1 = Lit::Num(100.0);
    let r = define(&d).unwrap_err();
    assert_eq!(r.code, "out_of_range", "{r}");
    assert!(r.msg.contains("Depth"), "{r}");
    // and so is a direct resolve of the parsed family
    let f = Family::parse(&table().to_text().unwrap()).unwrap();
    let e = f.resolve(&[(s("Width"), Value::Num(9.0))], &[]).unwrap_err();
    assert!(e.to_string().contains("Width"), "{e}");
}

#[test]
fn bad_unit_and_parse_errors_point_to_the_item() {
    let mut d = table();
    d.params[2].default = Some(Lit::Num(740.0));
    d.params[2].unit = Some("deg".into());
    let r = define(&d).unwrap_err();
    assert_eq!((r.code, r.path.as_str()), ("parse", "/params/2"), "{r}");
    let mut d = table();
    d.forms[0].material = Some(s("Nope"));
    let r = define(&d).unwrap_err();
    assert_eq!(r.path, "/forms/0", "{r}");
    let mut d = table();
    d.params[5].formula = Some(s("Width\nparam X length type 1m"));
    assert_eq!(define(&d).unwrap_err().code, "invalid");
}

#[test]
fn ai_family_is_draft() {
    let text = table().to_text().unwrap();
    let f = Family::parse(&text).unwrap();
    assert!(f.draft);
    let p = promoted(&text);
    assert!(!p.contains("status draft"));
    let g = Family::parse(&p).unwrap();
    assert!(!g.draft);
    assert_eq!(g.params, f.params);
    assert_eq!(g.solids, f.solids);
    // explicit status and a bad status
    assert!(!Family::parse("family A\nstatus released\n").unwrap().draft);
    assert!(Family::parse("family A\nstatus maybe\n").is_err());
}

#[test]
fn flex_sweep_is_deterministic() {
    let d = table();
    let a = define(&d).unwrap();
    let again = define(&d).unwrap();
    assert_eq!(a.sweeps, again.sweeps);
    let params: Vec<&str> = a.sweeps.iter().map(|r| r.param.as_str()).collect();
    assert_eq!(params, ["Width", "Depth", "Legs"]);
    // Width and Depth build over their whole range; Legs breaks at 2 (Pitch divides by zero)
    assert!(a.sweeps[0].breaks_at.is_none(), "{:?}", a.sweeps[0]);
    assert!(a.sweeps[1].breaks_at.is_none(), "{:?}", a.sweeps[1]);
    let (at, why) = a.sweeps[2].breaks_at.clone().expect("Legs breaks");
    assert!((at - 2.0).abs() < 0.5, "{at}");
    assert!(why.contains("Pitch"), "{why}");
    let base = a.family.types[0].values.clone();
    assert_eq!(sweep_limits(&a.family, &base), a.sweeps);
}

#[test]
fn existing_text_families_still_parse() {
    let lib = include_str!("../../../model/data/library.ddcf");
    let mut n = 0;
    for src in lib.split("\n---") {
        if !src.lines().any(|l| l.trim_start().starts_with("family ")) {
            continue;
        }
        let f = Family::parse(src.trim_start_matches('\n')).unwrap_or_else(|e| panic!("{e}"));
        assert!(!f.draft && f.limits.is_empty(), "{}", f.name);
        n += 1;
    }
    assert!(n >= 10, "{n}");
    // limits in hand-written text
    let f = Family::parse("family A\nparam W length type 1m\nlimit W 500mm _\n").unwrap();
    assert_eq!(f.limits[0].max, None);
    assert!(f.resolve(&[(s("W"), Value::Num(0.4))], &[]).is_err());
    assert!(Family::parse("family A\nparam W length type 1m\nlimit W 2m 1m\n").is_err());
    assert!(Family::parse("family A\nparam M material type \"x\"\nlimit M 1 2\n").is_err());
}
