//! Validation pipeline tests: every code, stopping after a stage, messages, budgets.

use super::validate::{validate, Limits, Severity, CODES};

const DESK: &str = r#"family Desk
category furniture
param Width length type 1200mm
param Depth length type 600mm
param Height length type 740mm
param Top length type 40mm
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
extrude Board rect(Left, Front, Right, Back) z(Height - Top, Height)
type "Small" Width=800mm
type "Wide" Width=1800mm
"#;

fn lim() -> Limits {
    Limits::default()
}

fn lines_of(r: &super::validate::Report, code: &str) -> Vec<usize> {
    r.find(code).unwrap_or_else(|| panic!("{code}: {r:?}")).lines.clone()
}

#[test]
fn validate_ok_table_family() {
    let r = validate(DESK, &lim());
    assert!(r.ok, "{r:?}");
    assert!(r.findings.iter().all(|f| f.severity == Severity::Warn), "{r:?}");
    assert_eq!((r.stats.bodies, r.stats.types), (1, 2));
    assert_eq!(r.stats.stage, "kernel");
}

#[test]
fn validate_parse_error() {
    let r = validate("family X\nfoo bar\n", &lim());
    assert!(!r.ok);
    assert_eq!(lines_of(&r, "parse_error"), vec![2]);
}

#[test]
fn validate_unknown_ref() {
    let t = DESK.replace("rect(Left, Front, Right, Back)", "rect(Left, Front, Right, Nowhere)");
    let r = validate(&t, &lim());
    assert_eq!(lines_of(&r, "unknown_ref"), vec![17], "{r:?}");
}

#[test]
fn validate_formula_cycle() {
    let r = validate("family C\nparam A length type = B\nparam B length type = A\n", &lim());
    assert_eq!(lines_of(&r, "formula_cycle"), vec![2, 3], "{r:?}");
}

#[test]
fn validate_sketch_conflict() {
    // half-width 500 mm with a width of 1200 mm
    let t = DESK.replace("dim Depth", "dim 500mm Left CenterLR\ndim Depth");
    let r = validate(&t, &lim());
    assert!(!r.ok);
    let l = lines_of(&r, "sketch_conflict");
    assert!(l.contains(&16), "the added dim is in the minimal set: {l:?}");
    assert!(!l.contains(&17), "unrelated Front/Back dim is not: {l:?}");
    assert_eq!(r.stats.stage, "sketch", "stops after the sketch stage");
}

#[test]
fn validate_sketch_underconstrained() {
    let t = DESK.replace("eq Left CenterLR Right\n", "");
    let r = validate(&t, &lim());
    assert!(r.ok, "{r:?}");
    let f = r.find("sketch_underconstrained").expect("warn");
    assert_eq!(f.severity, Severity::Warn);
    assert!(f.lines.contains(&9) && f.lines.contains(&10), "{f:?}");
}

#[test]
fn validate_type_fails() {
    let t = format!("{DESK}param N int type 2\nparam Half length type = Width / N\ntype \"Zero\" N=0\n");
    let r = validate(&t, &lim());
    assert!(!r.ok);
    let f = r.find("type_fails").expect("type_fails");
    assert!(f.lines.contains(&22), "{f:?}");
    assert!(f.msg.contains("Zero"), "{f:?}");
}

#[test]
fn validate_flex_fails() {
    // the divisor becomes zero at Height ×0.5
    let t = DESK.replace("param Top length type 40mm
", "param Top length type 40mm
param K length type = Width / if(Height < 500mm, 0, 1)
");
    let r = validate(&t, &lim());
    let f = r.find("flex_fails").unwrap_or_else(|| panic!("{r:?}"));
    assert_eq!(f.severity, Severity::Warn);
    assert!(r.ok, "flex is a warning: {r:?}");
}

#[test]
fn validate_nonmanifold() {
    let t = format!("{DESK}extrude Bow poly(0,0, 1m,1m, 1m,0, 0,1m) z(0, 1m)\n");
    let r = validate(&t, &lim());
    assert!(lines_of(&r, "nonmanifold").contains(&20), "{r:?}");
}

#[test]
fn validate_zero_volume() {
    let t = DESK.replace("z(Height - Top, Height)", "z(Height, Height)");
    let r = validate(&t, &lim());
    assert!(!r.ok);
    let l = lines_of(&r, "zero_volume");
    assert!(l.contains(&17), "{l:?}");
}

#[test]
fn validate_too_large() {
    let t = DESK.replace("Width=1800mm", "Width=80m");
    let r = validate(&t, &lim());
    assert!(r.has("too_large"), "{r:?}");
}

#[test]
fn validate_budget() {
    let l = Limits { lines: 10, ..lim() };
    let r = validate(DESK, &l);
    assert!(r.has("budget") && !r.ok);
    assert_eq!(r.stats.stage, "limits");
    let l = Limits { types: 1, ..lim() };
    assert!(validate(DESK, &l).has("budget"));
    let l = Limits { bodies: 0, ..lim() };
    assert!(validate(DESK, &l).has("budget"));
}

#[test]
fn validate_eval_timeout() {
    let l = Limits { eval_ms: 0, ..lim() };
    let r = validate(DESK, &l);
    assert!(r.has("eval_timeout") && !r.ok, "{r:?}");
}

#[test]
fn stage_stops_after_error() {
    // parse error: neither the sketch nor the types are checked
    let r = validate("family X\nbad line\nextrude A rect(0,0,0,0) z(0,0)\n", &lim());
    assert_eq!(r.stats.stage, "parse");
    assert_eq!(r.findings.len(), 1, "{r:?}");
}

#[test]
fn lines_point_to_source() {
    // comments and blank lines do not shift line numbers
    let t = format!("# header\n\n{DESK}");
    let t = t.replace("z(Height - Top, Height)", "z(Height, Height)");
    let r = validate(&t, &lim());
    let src: Vec<&str> = t.lines().collect();
    for n in lines_of(&r, "zero_volume") {
        assert!(src[n - 1].starts_with("extrude") || src[n - 1].starts_with("type"), "{n}: {}", src[n - 1]);
    }
}

#[test]
fn codes_list_is_stable() {
    assert_eq!(
        CODES,
        [
            "parse_error",
            "unknown_ref",
            "formula_cycle",
            "sketch_conflict",
            "sketch_underconstrained",
            "type_fails",
            "flex_fails",
            "nonmanifold",
            "zero_volume",
            "too_large",
            "budget",
            "eval_timeout"
        ]
    );
    // every code has a title and a hint in both languages
    for lang in ["en", "ru"] {
        let p = format!("{}/../ui/data/i18n/{lang}/fam_validate.json", env!("CARGO_MANIFEST_DIR"));
        let s = std::fs::read_to_string(&p).expect(&p);
        for c in CODES {
            assert!(s.contains(&format!("\"fam.val.{c}\"")), "{lang} {c}");
            assert!(s.contains(&format!("\"fam.val.{c}.hint\"")), "{lang} {c} hint");
        }
    }
}

#[test]
fn validate_budget_300_lines_40_bodies_20_types() {
    let mut t = String::from("family Big\nparam W length type 1000mm\n");
    for i in 0..40 {
        t += &format!("extrude B{i} rect(0, 0, W, 100mm) z({}mm, {}mm)\n", i * 10, i * 10 + 8);
    }
    for i in 0..20 {
        t += &format!("type \"T{i}\" W={}mm\n", 500 + i * 50);
    }
    while t.lines().count() < 300 {
        t += "# padding\n";
    }
    let t0 = std::time::Instant::now();
    let r = validate(&t, &lim());
    let ms = t0.elapsed().as_millis();
    assert!(r.ok, "{r:?}");
    assert_eq!((r.stats.lines, r.stats.bodies, r.stats.types), (300, 40, 20));
    // 150 ms in release; a debug build is an order of magnitude slower
    let cap = if cfg!(debug_assertions) { 3000 } else { 150 };
    assert!(ms <= cap, "{ms} ms");
}
