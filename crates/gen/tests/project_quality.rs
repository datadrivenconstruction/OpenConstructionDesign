//! Programme quality of generated projects: the rooms asked are the rooms built, spare band
//! length does not turn into storerooms, a single storey has no stair, habitable rooms are lit.

use ddc_gen::project::{build_project, check_project, parse_spec, GProject, RoomKind};
use serde_json::{json, Value as J};

fn built(v: J) -> GProject {
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{:?}", g.failures);
    let dim: Vec<&String> = g.warnings.iter().filter(|w| w.contains("glazing")).collect();
    assert!(dim.is_empty(), "habitable rooms under 1/8 glazing: {dim:?}");
    p
}

fn area(poly: &[(f64, f64)]) -> f64 {
    let n = poly.len();
    (0..n)
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        .abs()
        / 2.0
}

fn kinds(p: &GProject, floor: usize, k: RoomKind) -> usize {
    p.storeys[floor].rooms.iter().filter(|r| r.kind == k).count()
}

/// Storage area over the net room area of each floor.
fn storage_share(p: &GProject) -> Vec<f64> {
    p.storeys
        .iter()
        .map(|s| {
            let (mut st, mut all) = (0.0, 0.0);
            for (r, meta) in s.layout.rooms.iter().zip(&s.rooms) {
                let a = area(&r.polygon);
                all += a;
                if meta.kind == RoomKind::Storage {
                    st += a;
                }
            }
            st / all
        })
        .collect()
}

fn names(p: &GProject) -> Vec<String> {
    p.storeys
        .iter()
        .flat_map(|s| s.layout.rooms.iter().map(|r| r.name.clone()))
        .collect()
}

fn assert_programme_quality(p: &GProject, what: &str) {
    for (k, share) in storage_share(p).into_iter().enumerate() {
        assert!(share < 0.10, "{what} floor {}: storage {share:.3}", k + 1);
    }
    if p.storeys.len() == 1 {
        assert!(p.storeys[0].layout.stairs.is_empty(), "{what}");
        assert_eq!(kinds(p, 0, RoomKind::Stair), 0, "{what}: stair room on one storey");
    }
    for (k, s) in p.storeys.iter().enumerate() {
        let habitable = s.rooms.iter().filter(|r| r.kind.habitable()).count();
        let windows = s.layout.openings.iter().filter(|o| !o.door).count();
        assert!(windows >= habitable, "{what} floor {}: {windows} windows", k + 1);
    }
}

#[test]
fn a_single_storey_house_brief_gets_bedrooms_bath_and_no_stair() {
    let brief = json!({"typology": "house", "floors": 1, "lang": "ru", "floor_height": 3.0,
        "rooms": [{"kind": "living", "area": 28}, {"kind": "kitchen", "area": 16},
            {"kind": "bedroom", "area": 14, "count": 2}, {"kind": "bedroom", "area": 10},
            {"kind": "bath", "area": 6}, {"kind": "wc", "area": 2}],
        "roof": {"kind": "pitched", "slope": 25}});
    let p = built(brief.clone());
    assert_eq!(p.storeys.len(), 1);
    for (k, n) in [
        (RoomKind::Bedroom, 3),
        (RoomKind::Living, 1),
        (RoomKind::Kitchen, 1),
        (RoomKind::Bath, 1),
        (RoomKind::Wc, 1),
    ] {
        assert_eq!(kinds(&p, 0, k), n, "{k:?}");
    }
    assert_programme_quality(&p, "house brief");
    // the 12 x 10 footprint the brief was drawn for fits once there is no stair core
    let mut fixed = brief;
    fixed["footprint"] = json!({"width": 12, "depth": 10});
    assert_programme_quality(&built(fixed), "house 12x10");
}

#[test]
fn a_house_takes_a_hall_and_its_defaults_are_a_full_home() {
    let p = built(json!({"typology": "house", "floors": 1, "rooms": [
        {"kind": "hall", "area": 6}, {"kind": "living", "area": 24},
        {"kind": "kitchen", "area": 12}, {"kind": "bedroom", "area": 12, "count": 2},
        {"kind": "bath", "area": 5}]}));
    assert!(kinds(&p, 0, RoomKind::Hall) >= 1);
    for floors in [1, 2] {
        let p = built(json!({"typology": "house", "floors": floors}));
        let all = |k| (0..p.storeys.len()).map(|f| kinds(&p, f, k)).sum::<usize>();
        assert!(all(RoomKind::Bedroom) >= 2, "{floors}");
        assert!(all(RoomKind::Bath) >= 1 && all(RoomKind::Kitchen) == 1, "{floors}");
        assert!(all(RoomKind::Living) == 1, "{floors}");
        assert_programme_quality(&p, &format!("house {floors}"));
    }
}

#[test]
fn an_office_has_no_giant_storerooms_or_school_rooms() {
    let brief = json!({"typology": "office", "floors": 4, "lang": "ru", "floor_height": 3.6,
        "grid": 7.2, "corridor": 2.0, "band_depth": 6.0, "lift": true,
        "ground_rooms": [{"kind": "lobby", "area": 80}, {"kind": "office", "area": 36, "count": 4},
            {"kind": "meeting", "area": 30}, {"kind": "wc", "area": 6, "count": 2},
            {"kind": "technical", "area": 20}, {"kind": "storage", "area": 12}],
        "rooms": [{"kind": "office", "area": 36, "count": 6}, {"kind": "meeting", "area": 30},
            {"kind": "staff", "area": 18}, {"kind": "wc", "area": 6, "count": 2},
            {"kind": "storage", "area": 10}]});
    let teacher = "\u{423}\u{447}\u{438}\u{442}\u{435}\u{43b}\u{44c}\u{441}\u{43a}\u{430}\u{44f}";
    for v in [brief, json!({"typology": "office", "lang": "ru"}), json!({"typology": "office"})] {
        let p = built(v.clone());
        assert_programme_quality(&p, &v.to_string());
        let n = names(&p);
        assert!(!n.iter().any(|x| x.contains(teacher)), "{n:?}");
        for f in 0..p.storeys.len() {
            assert!(kinds(&p, f, RoomKind::Wc) >= 2, "floor {f}");
            assert!(kinds(&p, f, RoomKind::Meeting) >= 1, "floor {f}");
            assert_eq!(kinds(&p, f, RoomKind::Classroom), 0);
        }
    }
}

#[test]
fn single_storey_defaults_of_every_room_typology_pass_the_programme_bar() {
    for t in ["house", "office", "school"] {
        for lang in ["en", "ru"] {
            let p = built(json!({"typology": t, "floors": 1, "lang": lang}));
            assert_programme_quality(&p, t);
        }
    }
}
