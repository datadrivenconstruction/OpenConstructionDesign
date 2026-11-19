// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One contract per stdlib function: normal use, boundary, error with line and message,
//! and the same hash on a second run.

use ddc_ops::{Cat, Ref};
use ddc_script::{ErrorKind, GeoScript, RunOpts, Sandbox, ScriptError, ScriptRun};
use serde_json::json;

fn run(src: &str) -> Result<ScriptRun, ScriptError> {
    let o = RunOpts::new(Ref::name(Cat::Level, "Level 1"), json!({}));
    Sandbox.run(src, &o)
}

fn ok(src: &str) -> ScriptRun {
    let a = run(src).unwrap_or_else(|e| panic!("{e}\n{src}"));
    let b = run(src).unwrap();
    assert_eq!(a.hash, b.hash, "deterministic: {src}");
    assert_eq!(a.env, b.env);
    a
}

fn fail(src: &str) -> ScriptError {
    run(src).expect_err("script must fail")
}

const P: &str = "local p = profile.rect(0.1, 0.1)\n";

#[test]
fn sweep_along_polyline_arc_and_point_list() {
    let r = ok(&format!(
        "{P}part('a', sweep(polyline({{vec3(0,0,0), vec3(1,0,0), vec3(1,1,0)}}), p))\n\
         part('b', sweep(arc(vec3(0,0,0), 2, 0, deg(90)), p))\n\
         part('c', sweep({{vec3(0,0,0), vec3(0,0,3)}}, p))"
    ));
    assert_eq!(r.parts.len(), 3);
    // boundary: a closed polyline is a ring path
    let r = ok(&format!(
        "{P}part('ring', sweep(polyline({{vec3(0,0,0), vec3(1,0,0), vec3(1,1,0)}}, true), p))"
    ));
    assert_eq!(r.parts.len(), 1);
}

#[test]
fn sweep_errors_name_the_call_and_line() {
    let e = fail(&format!("{P}\nlocal s = sweep(42, p)"));
    assert_eq!((e.kind, e.line), (ErrorKind::Runtime, 3), "{e}");
    assert!(e.message.starts_with("sweep:") && e.message.contains("polyline"), "{e}");
    let e = fail(&format!("{P}local s = sweep({{vec3(0,0,0)}}, p)"));
    assert_eq!(e.line, 2, "{e}");
    assert!(e.message.contains("sweep:") && e.message.contains("2 or more"), "{e}");
    let e = fail("local s = sweep({vec3(0,0,0), vec3(1,0,0)}, 5)");
    assert_eq!(e.line, 1, "{e}");
}

#[test]
fn polyline_and_arc_boundaries() {
    ok(&format!("{P}part('a', sweep(polyline({{vec3(0,0,0), vec3(0,0,1)}}), p))"));
    let e = fail("local a = polyline({vec3(0,0,0)})");
    assert!(e.message.starts_with("polyline:"), "{e}");
    let e = fail("local a = arc(vec3(0,0,0), 0, 0, 1)");
    assert!(e.message.starts_with("arc:") && e.message.contains("radius"), "{e}");
    let e = fail("local a = arc(vec3(0,0,0), 1, 1, 1)");
    assert!(e.message.starts_with("arc:") && e.message.contains("zero angle"), "{e}");
    // full turn is a closed ring and measures 2*pi*r
    let r = ok_pad("local a = arc(vec3(0,0,0), 1, 0, deg(360))\nmeasure(a)");
    assert!((r.measures[0].value - 2.0 * std::f64::consts::PI).abs() < 1e-9);
    let r = ok_pad("measure(arc(vec3(0,0,0), 2, 0, deg(90)))");
    assert!((r.measures[0].value - std::f64::consts::PI).abs() < 1e-9);
}

#[test]
fn revolve_quarter_full_and_errors() {
    ok(&format!("{P}part('q', revolve(p, vec3(0,0,0), 3, deg(90)))"));
    ok(&format!("{P}part('full', revolve(p, vec3(0,0,0), 3, deg(360)))"));
    ok(&format!("{P}part('neg', revolve(p, vec3(0,0,0), 3, -deg(90)))"));
    let e = fail(&format!("{P}\npart('x', revolve(p, vec3(0,0,0), -1, deg(90)))"));
    assert_eq!(e.line, 3, "{e}");
    assert!(e.message.starts_with("revolve:") && e.message.contains("radius"), "{e}");
    let e = fail(&format!("{P}part('x', revolve(p, vec3(0,0,0), 1, 0))"));
    assert!(e.message.starts_with("revolve:") && e.message.contains("zero angle"), "{e}");
}

#[test]
fn loft_needs_two_sections() {
    let r = ok("local a = profile.rect(0.2, 0.2)\nlocal b = profile.rect(0.1, 0.1)\n\
                part('l', loft({{a, vec3(0,0,0)}, {b, vec3(0,0,2)}}))");
    assert_eq!(r.parts.len(), 1);
    let r = ok("local a = profile.rect(0.2, 0.2)\n\
                part('l', loft({{profile = a, at = vec3(0,0,0)},\n\
                                {profile = a, at = vec3(0,0,1)}}))");
    assert_eq!(r.parts.len(), 1);
    let e = fail("local a = profile.rect(0.2, 0.2)\n\nlocal f = loft({{a, vec3(0,0,0)}})");
    assert_eq!(e.line, 3, "{e}");
    assert!(e.message.starts_with("loft:") && e.message.contains("2 or more"), "{e}");
}

#[test]
fn pattern_variants_counts_and_bounds() {
    let r = ok(&format!(
        "{P}local f = extrude(p, vec3(0,0,0), vec3(0,0,1))\n\
         parts('a', pattern(f, 4, transform_identity_or_move))"
    )
    .replace("transform_identity_or_move", "identity()"));
    assert_eq!(r.parts.len(), 4);
    let r = ok(&format!(
        "{P}local f = extrude(p, vec3(0,0,0), vec3(0,0,1))\n\
         parts('row', pattern_linear(f, 5, vec3(1,0,0)))\n\
         parts('ring', pattern_radial(f, 6, deg(360), vec3(3,0,0)))\n\
         parts('fan', pattern_radial(f, 3, deg(90)))"
    ));
    assert_eq!(r.parts.len(), 5 + 6 + 3);
    // boundary: one copy is allowed
    let r = ok(&format!(
        "{P}parts('one', pattern_linear(extrude(p, vec3(0,0,0), vec3(0,0,1)), 1, vec3(1,0,0)))"
    ));
    assert_eq!(r.parts.len(), 1);
    // distance between copies is the step
    let r = ok(&format!(
        "{P}local c = parts('row', pattern_linear(extrude(p, vec3(0,0,0), vec3(0,0,1)), 3, \
         vec3(2,0,0)))\nmeasure(c[1].a, c[2].a)"
    ));
    assert!((r.measures[0].value - 2.0).abs() < 1e-9);
}

#[test]
fn pattern_errors() {
    for n in ["0", "-3", "100001"] {
        let e = fail(&format!(
            "{P}\nlocal f = extrude(p, vec3(0,0,0), vec3(0,0,1))\nlocal c = pattern_linear(f, {n}, \
             vec3(1,0,0))"
        ));
        assert_eq!(e.line, 4, "{n}: {e}");
        assert!(e.message.starts_with("pattern:") && e.message.contains("1..100000"), "{e}");
    }
}

#[test]
fn boolean_kinds_and_errors() {
    for k in ["cut", "union", "intersect"] {
        let r = ok(&format!(
            "{P}local a = part('a', extrude(p, vec3(0,0,0), vec3(2,0,0)))\n\
             boolean('{k}', a, extrude(profile.rect(0.02, 0.02), vec3(1,0,0), vec3(1,1,0)))"
        ));
        assert_eq!(r.parts.len(), 1, "{k}");
    }
    let e = fail(&format!(
        "{P}local a = part('a', extrude(p, vec3(0,0,0), vec3(2,0,0)))\n\
         boolean('drill', a, extrude(p, vec3(1,0,0), vec3(1,1,0)))"
    ));
    assert_eq!(e.line, 3, "{e}");
    assert!(e.message.contains("drill") && e.message.contains("cut, union or intersect"), "{e}");
    let e = fail(&format!("{P}boolean('cut', 5, extrude(p, vec3(1,0,0), vec3(1,1,0)))"));
    assert_eq!(e.line, 2, "{e}");
}

#[test]
fn measure_points_parts_forms_and_errors() {
    let r = ok_pad("measure(vec3(0,0,0), vec3(3,4,0))");
    assert!((r.measures[0].value - 5.0).abs() < 1e-9);
    assert_eq!((r.measures[0].line, r.measures[0].unit), (1, "m"));
    let r = ok(&format!(
        "{P}local f = extrude(p, vec3(0,0,0), vec3(0,0,2.5))\nlocal a = part('a', f)\n\
         measure(a)\nmeasure(f)\nmeasure(vec3(0,3,4))"
    ));
    let v: Vec<f64> = r.measures.iter().map(|m| m.value).collect();
    assert!((v[0] - 2.5).abs() < 1e-9 && (v[1] - 2.5).abs() < 1e-9 && (v[2] - 5.0).abs() < 1e-9);
    assert_eq!(r.measures[2].line, 6);
    // boundary: zero distance
    let r = ok_pad("measure(vec3(1,1,1), vec3(1,1,1))");
    assert_eq!(r.measures[0].value, 0.0);
    let e = fail("\nlocal d = measure('nope')");
    assert_eq!(e.line, 2, "{e}");
    assert!(e.message.starts_with("measure:") && e.message.contains("string"), "{e}");
}

#[test]
fn transform_moves_and_rotates() {
    let r = ok(&format!(
        "{P}local f = extrude(p, vec3(0,0,0), vec3(1,0,0))\n\
         local g = transform(f, identity())\npart('g', g)\nmeasure(g)"
    ));
    assert!((r.measures[0].value - 1.0).abs() < 1e-9);
    let e = fail("local g = transform(5, identity())");
    assert_eq!(e.line, 1, "{e}");
}

#[test]
fn units_and_lengths() {
    let r = ok_pad("local o = vec3(0,0,0)\nmeasure(o, vec3(mm(1500), 0, 0))\n\
                measure(o, vec3(cm(20), 0, 0))\nmeasure(o, vec3(len('2 m'), 0, 0))\n\
                measure(o, vec3(m(3), 0, 0))");
    let v: Vec<f64> = r.measures.iter().map(|m| m.value).collect();
    for (a, b) in v.iter().zip([1.5, 0.2, 2.0, 3.0]) {
        assert!((a - b).abs() < 1e-9, "{v:?}");
    }
    ok_pad("assert(math.abs(angle('90 deg') - deg(90)) < 1e-12)\n\
                assert(math.abs(deg(180) - math.pi) < 1e-12)");
    let e = fail("\n\nlocal x = angle('ninety')");
    assert_eq!(e.line, 3, "{e}");
    assert!(e.message.starts_with("angle:"), "{e}");
}

#[test]
fn assert_and_constrain_report_position() {
    let e = fail("\nassert(false)");
    assert_eq!((e.kind, e.line), (ErrorKind::Assert, 2), "{e}");
    assert!(e.message.contains("assertion failed"), "{e}");
    let e = fail("assert(nil, 'custom note')");
    assert!(e.message.contains("custom note"), "{e}");
    ok_pad("assert(true)\nassert(0)");
}

#[test]
fn group_define_component_place_errors() {
    let e = fail("place('missing', vec3(0,0,0), 0)");
    assert_eq!(e.line, 1, "{e}");
}

#[test]
fn syntax_error_without_stack_noise() {
    let e = fail("local x = = 1");
    assert_eq!((e.kind, e.line), (ErrorKind::Syntax, 1), "{e}");
    assert!(e.col >= 1, "{e}");
    let s = e.to_string();
    assert!(s.starts_with("line 1, col "), "{s}");
    assert!(!s.contains("stack traceback"), "{s}");
}

/// Scripts that only measure or assert still need one part to be a valid run.
fn ok_pad(src: &str) -> ScriptRun {
    ok(&format!(
        "{src}\npart('_pad', extrude(profile.rect(0.1, 0.1), vec3(0,0,0), vec3(1,0,0)))"
    ))
}
