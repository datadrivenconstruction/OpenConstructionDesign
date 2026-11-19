//! Free-form envelope of a plate: curved patches, panels and ribs measured on built projects.

use ddc_gen::project::{brief, build_project, check_project, parse_spec, GProject};
use serde_json::{json, Value as J};

fn built(v: &J) -> GProject {
    let s = parse_spec(v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{v}: {:?}", g.failures);
    p
}

fn metric(p: &GProject, k: &str) -> f64 {
    p.metrics.iter().find(|m| m.0 == k).unwrap_or_else(|| panic!("no metric {k}")).1
}

#[test]
fn a_bar_with_a_shell_gets_double_curved_patches_panels_and_ribs() {
    let plain = built(&json!({"typology": "office", "floors": 3}));
    assert!(plain.shell.is_none());
    let p = built(&json!({"typology": "office", "floors": 3, "design": {"shell": true}}));
    let sh = p.shell.as_ref().expect("shell");
    assert!(metric(&p, "shell_patches") >= 12.0);
    assert_eq!(metric(&p, "shell_patches"), (sh.skin.len() + sh.roof.len()) as f64);
    assert!(metric(&p, "shell_double_curved") >= metric(&p, "shell_patches") / 2.0);
    // the skin alone is measured, not assumed from the roof
    assert!(sh.report.double_curved >= sh.skin.len() / 2, "{} of {}", sh.report.double_curved, sh.skin.len());
    assert!(metric(&p, "shell_seam_mm") < 1e-3, "seam {} mm", metric(&p, "shell_seam_mm"));
    assert!(metric(&p, "shell_panels") > 50.0);
    assert!(metric(&p, "shell_gap_mm") <= 20.0, "gap {} mm", metric(&p, "shell_gap_mm"));
    assert!(metric(&p, "shell_off_surface_mm") < 10.0);
    assert!(metric(&p, "shell_clearance_m") > 0.0);
    assert_eq!(metric(&p, "shell_ribs"), (sh.skin_ribs.len() + sh.roof_ribs.len()) as f64);
}

#[test]
fn the_brief_asks_for_a_shell_and_is_told_how() {
    let asked = json!({"typology": "office", "floors": 2, "features": ["freeform"]});
    let s = parse_spec(&asked).unwrap();
    let p = build_project(&s).unwrap();
    let repair = brief::check(&s, &p).repair.expect("a repair");
    assert_eq!(repair["design"]["shell"], json!(true), "{repair}");
    let mut with = asked.clone();
    with["design"] = json!({"shell": true});
    let s = parse_spec(&with).unwrap();
    let p = build_project(&s).unwrap();
    let c = brief::check(&s, &p);
    assert!(c.report["met"].to_string().contains("freeform"), "{}", c.report);
}

#[test]
fn panel_size_follows_the_rule() {
    let a = built(&json!({"typology": "office", "floors": 3, "design": {"shell": true}}));
    let b = built(&json!({"typology": "office", "floors": 3, "design": {"shell": true},
        "rules": {"massing.shell_panel_w": 3.0, "massing.shell_panel_h": 4.0}}));
    assert!(metric(&b, "shell_panels") < metric(&a, "shell_panels") / 2.0);
}

/// A window never reaches into the mitre of a corner of a faceted outline.
#[test]
fn openings_keep_off_the_corners_of_a_faceted_outline() {
    for shape in ["ellipse", "crescent", "triangle", "trapezoid"] {
        let p = built(&json!({"typology": "office", "floors": 2,
            "footprint": {"shape": shape, "width": 30, "depth": 20}, "design": {"shell": true}}));
        for st in &p.storeys {
            let l = &st.layout;
            for (i, o) in l.openings.iter().enumerate() {
                for q in l.openings.iter().skip(i + 1).filter(|q| q.wall == o.wall) {
                    assert!(o.along + o.width <= q.along + 1e-6 || q.along + q.width <= o.along + 1e-6, "{shape}: {o:?} overlaps {q:?}");
                }
            }
            for (i, o) in l.openings.iter().enumerate().filter(|(_, o)| !o.door) {
                let (c0, c1) = ddc_gen::miter_clear(&l.walls, o.wall);
                let len = l.walls[o.wall].length();
                if len - c0 - c1 < o.width {
                    continue;
                }
                assert!(
                    o.along >= c0 - 1e-6 && o.along + o.width <= len - c1 + 1e-6,
                    "{shape} opening {i} {o:?}: clear {c0:.2}/{c1:.2} of {len:.2}"
                );
            }
        }
    }
}

/// The words of a free-form brief all lead to the shell, and `curved` is met by it.
#[test]
fn free_form_words_ask_for_the_shell() {
    for w in ["organic", "sculptural", "wave", "double_curvature", "curved_roof"] {
        // the shell is on by the word itself; switched off, the check asks for it
        let s = parse_spec(&json!({"typology": "office", "floors": 2, "features": [w], "design": {"shell": false}})).unwrap();
        let p = build_project(&s).unwrap();
        let repair = brief::check(&s, &p).repair.unwrap_or_else(|| panic!("{w}: no repair"));
        assert_eq!(repair["design"]["shell"], json!(true), "{w}: {repair}");
    }
    let s = parse_spec(&json!({"typology": "office", "floors": 2, "features": ["curved"], "design": {"shell": true}})).unwrap();
    let p = build_project(&s).unwrap();
    assert!(brief::check(&s, &p).report["met"].to_string().contains("curved"));
}

/// The roof of the shell never dips below the tops of the walls it covers: walls stay under it,
/// and the model's wall trim closes the rest of the gap up to the underside.
#[test]
fn no_wall_pokes_through_the_shell_roof() {
    for shape in ["ellipse", "circle", "crescent"] {
        let p = built(&json!({"typology": "office", "floors": 3,
            "footprint": {"shape": shape, "width": 36, "depth": 24}, "design": {"shell": true}}));
        let sh = p.shell.as_ref().expect("shell");
        let top = p.storeys.last().map(|s| s.elevation + s.height).unwrap();
        let roof: Vec<[f64; 3]> = sh.roof.iter().flat_map(|q| q.sections.iter().flatten().copied()).collect();
        for w in p.storeys.last().unwrap().layout.walls.iter() {
            for k in 0..=8 {
                let (x, y) = w.at(w.length() * k as f64 / 8.0);
                let near = roof.iter().filter(|q| (q[0] - x).hypot(q[1] - y) < 0.75);
                for q in near {
                    assert!(q[2] >= top - 0.05, "{shape}: roof {:.3} under the wall top {top:.3} at ({x:.1}, {y:.1})", q[2]);
                }
            }
        }
    }
}

/// A free-form feature word on a plain bar turns the shell on by itself; the brief's own choice
/// (an explicit design.shell, a bow, wings) is kept.
#[test]
fn a_free_form_feature_word_turns_the_shell_on_for_a_plain_bar() {
    let on = |v: J| parse_spec(&v).unwrap_or_else(|e| panic!("{e}")).design.shell;
    for w in ["organic", "sculptural", "wave", "double_curvature", "curved_roof", "freeform"] {
        assert!(on(json!({"typology": "office", "floors": 3, "features": [w]})), "{w}");
    }
    assert!(!on(json!({"typology": "office", "floors": 3})));
    assert!(!on(json!({"typology": "office", "floors": 3, "features": ["organic"], "design": {"shell": false}})));
    // a bow beside a free-form word gives way to the shell; a polygon footprint takes it too
    let bowed = parse_spec(&json!({"typology": "office", "floors": 3, "features": ["organic"], "design": {"curve": 3.0}})).unwrap();
    assert!(bowed.design.shell && bowed.design.curve == 0.0);
    for w in ["curvy", "undulating", "fluid", "curved_walls"] {
        assert!(on(json!({"typology": "office", "floors": 3, "features": [w]})), "{w}");
    }
    let poly = json!({"typology": "office", "floors": 3, "features": ["sculptural"],
        "footprint": {"polygon": [[0, 0], [60, 0], [60, 30], [0, 30]]}});
    assert!(on(poly));
    assert!(!on(json!({"typology": "office", "floors": 3, "features": ["organic"], "design": {"massing": "l"}})));
}

/// Every opening of a faceted outline lies inside its wall with the end margin the check asks.
#[test]
fn openings_of_a_faceted_outline_lie_inside_their_walls() {
    for shape in ["ellipse", "crescent", "triangle", "trapezoid"] {
        let p = built(&json!({"typology": "office", "floors": 2,
            "footprint": {"shape": shape, "width": 24, "depth": 16}, "design": {"shell": true}}));
        for st in &p.storeys {
            let l = &st.layout;
            for o in &l.openings {
                let len = l.walls[o.wall].length();
                assert!(o.along >= 0.15 - 1e-9 && o.along + o.width <= len - 0.15 + 1e-9, "{shape}: {o:?} on a wall of {len:.2}");
            }
        }
    }
}

/// The person's own words reach the shell rule when the model sends no feature words.
/// A Russian sample from the test data file.
fn ru_sample(key: &str) -> String {
    let m: J = serde_json::from_str(include_str!("data/ru_samples.json")).unwrap();
    m[key].as_str().unwrap_or_else(|| panic!("no ru sample {key}")).to_string()
}

#[test]
fn the_request_text_alone_turns_the_shell_on() {
    use ddc_gen::project::brief::{request_text, set_request_text};
    let spec = json!({"typology": "office", "floors": 3});
    set_request_text("An art museum with a flowing sculptural shell");
    assert!(request_text().contains("sculptural"));
    assert!(parse_spec(&spec).unwrap().design.shell);
    for t in [ru_sample("freeform"), "Ein organisch geformtes Museum".into(), "Un museo de forma libre".into(), "流线型曲面博物馆".into()] {
        set_request_text(&t);
        assert!(parse_spec(&spec).unwrap().design.shell, "{t}");
    }
    set_request_text("An office block with a lift");
    assert!(!parse_spec(&spec).unwrap().design.shell);
    set_request_text("");
}
