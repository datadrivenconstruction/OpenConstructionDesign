//! A room line with `count` inside an apartment is built that many times.

use ddc_gen::project::{build_project, check_project, parse_spec, GProject};
use serde_json::{json, Value};

fn spec(three: &Value, width: f64) -> Value {
    let two = json!({"name": "2", "count": 2, "rooms": [{"kind": "living", "area": 20.0}, {"kind": "bedroom", "area": 14.5},
        {"kind": "kitchen", "area": 11.1}, {"kind": "bath", "area": 4.9}, {"kind": "hall", "area": 3.0}]});
    json!({"typology": "apartment_section", "floors": 3, "floor_height": 3.0, "lift": true,
           "footprint": {"width": width, "depth": 12.0}, "apartments": [two, three]})
}

/// First width from 34 m up that the builder accepts (the layout needs whole-room fits).
fn built(three: &Value) -> (f64, GProject) {
    for w in (34..=70).step_by(2) {
        let w = w as f64;
        if let Ok(p) = build_project(&parse_spec(&spec(three, w)).unwrap()) {
            return (w, p);
        }
    }
    panic!("no width from 34 to 70 m builds");
}

#[test]
fn a_counted_bedroom_line_gives_that_many_rooms_and_passes_the_gates() {
    let three = json!({"name": "3", "count": 2, "rooms": [{"kind": "living", "area": 21.9},
        {"kind": "bedroom", "area": 15.8, "count": 2}, {"kind": "kitchen", "area": 12.2},
        {"kind": "bath", "area": 5.4}, {"kind": "hall", "area": 3.0}]});
    let (w, p) = built(&three);
    let s = parse_spec(&spec(&three, w)).unwrap();
    let g = check_project(&s, &p);
    assert!(g.passed(), "{:?}", g.failures);
    // per floor: 2 x 4 rooms of the small flat + 2 x 5 of the large one (the hall is not asked)
    for st in &p.storeys {
        assert_eq!(st.rooms.iter().filter(|r| r.asked.is_some()).count(), 18);
    }
}

#[test]
fn a_counted_line_equals_the_same_line_written_twice() {
    let counted = json!({"name": "3", "count": 2, "rooms": [{"kind": "living", "area": 21.9},
        {"kind": "bedroom", "area": 15.8, "count": 2}, {"kind": "kitchen", "area": 12.2}, {"kind": "bath", "area": 5.4}]});
    let twice = json!({"name": "3", "count": 2, "rooms": [{"kind": "living", "area": 21.9},
        {"kind": "bedroom", "area": 15.8}, {"kind": "bedroom", "area": 15.8}, {"kind": "kitchen", "area": 12.2},
        {"kind": "bath", "area": 5.4}]});
    let (w, a) = built(&counted);
    let b = build_project(&parse_spec(&spec(&twice, w)).unwrap()).unwrap();
    assert_eq!(a.storeys.len(), b.storeys.len());
    for (x, y) in a.storeys.iter().zip(&b.storeys) {
        assert_eq!(x.layout.walls.len(), y.layout.walls.len());
        assert_eq!(x.layout.rooms.len(), y.layout.rooms.len());
    }
}
