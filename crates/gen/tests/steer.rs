//! Steering by edits: after an edit the free layout searches again only the zones the edit
//! changed, gives the same building a cold build gives, and with the score is back within
//! 300 ms. One test per binary: the zone store is shared by the threads of a test binary.

use ddc_gen::project::score::score;
use ddc_gen::project::{build_project, parse_spec, steer, GProject, ProjectSpec};
use serde_json::{json, Value as J};
use std::time::Instant;

fn built(v: &J) -> (ProjectSpec, GProject) {
    let s = parse_spec(v).unwrap_or_else(|e| panic!("{e}: {v}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}: {v}"));
    (s, p)
}

/// Searched zones of one build.
fn searched(v: &J) -> (usize, (ProjectSpec, GProject)) {
    let before = steer::zones().1;
    let b = built(v);
    (steer::zones().1 - before, b)
}

fn office(ground_wc: u32) -> J {
    json!({"typology": "office", "floors": 4, "footprint": {"width": 30},
           "rules": {"layout.strategy": "free"},
           "rooms": [{"kind": "office", "area": 30, "count": 4}, {"kind": "meeting", "area": 40},
                     {"kind": "wc", "area": 4, "count": 2}],
           "ground_rooms": [{"kind": "lobby", "area": 60}, {"kind": "office", "area": 30, "count": 3},
                            {"kind": "wc", "area": 4, "count": ground_wc}]})
}

/// The target of an optimised build; an unoptimised test build runs about ten times slower.
fn limit_ms() -> f64 {
    if cfg!(debug_assertions) {
        3000.0
    } else {
        300.0
    }
}

#[test]
fn an_edit_searches_only_the_zones_it_changed() {
    let (a, b) = (office(2), office(1));
    // cold: the edited brief alone, every zone searched (the ground floor and one typical floor,
    // the typical floors being the same problem)
    steer::forget();
    let (cold, (_, want)) = searched(&b);
    assert!(cold >= 2, "{cold}");

    steer::forget();
    let _ = built(&a);
    let t = Instant::now();
    let (edit, (s, p)) = searched(&b);
    let _ = score(&s, &p);
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(format!("{p:?}"), format!("{want:?}"), "the steered build differs from a cold one");
    // only the ground floor changed
    assert_eq!(edit, 1, "searched {edit} zones, a cold build {cold}");
    assert!(ms <= limit_ms(), "{ms:.0} ms");

    // an apartment section rebuilt as it was searches nothing, its apartments included
    let sec = json!({"typology": "apartment_section", "floors": 3, "rules": {"layout.strategy": "free"}});
    let _ = built(&sec);
    let t = Instant::now();
    let (again, _) = searched(&sec);
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(again, 0);
    assert!(ms <= limit_ms(), "{ms:.0} ms");
}
