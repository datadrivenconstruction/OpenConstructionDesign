//! The raised electrical level and the exact duct, pipe and tray clash search agree.
use ddc_gen::project::{build_project, parse_spec, void_check};
use serde_json::json;

fn clashes(v: serde_json::Value) -> Vec<String> {
    let p = build_project(&parse_spec(&v).unwrap()).unwrap();
    assert!(!p.hvac.ducts.is_empty() && !p.services.elec.is_empty(), "nothing to check");
    void_check(&p)
}

fn hospital(extra: serde_json::Value) -> serde_json::Value {
    let ward = |n: &str| json!({"kind": "ward", "area": 22, "count": 4, "name": n, "tags": ["clinical"]});
    json!({"typology": "office", "floors": 10, "services": true, "sprinklers": true, "rules": extra,
        "rooms": [ward("Ward"), {"kind": "technical", "area": 36, "name": "Theatre", "tags": ["operating"]},
                  {"kind": "ward", "area": 20, "name": "Isolation", "tags": ["isolation"]},
                  {"kind": "office", "area": 18, "count": 2}, {"kind": "wc", "area": 4, "count": 2}]})
}

#[test]
fn the_hospital_has_no_clash_with_the_raised_electrical_level() {
    let c = clashes(hospital(json!({})));
    assert!(c.is_empty(), "{} clashes, first: {:?}", c.len(), c.first());
}

#[test]
fn an_eight_storey_office_has_no_clash_with_the_raised_electrical_level() {
    let c = clashes(json!({"typology": "office", "floors": 8, "services": true, "sprinklers": true}));
    assert!(c.is_empty(), "{} clashes, first: {:?}", c.len(), c.first());
}

#[test]
fn the_same_two_buildings_hold_with_the_void_layers_on() {
    let on = json!({"mep.layers": true});
    let c = clashes(hospital(on.clone()));
    assert!(c.is_empty(), "hospital: {} clashes, first: {:?}", c.len(), c.first());
    let c = clashes(json!({"typology": "office", "floors": 8, "services": true, "sprinklers": true, "rules": on}));
    assert!(c.is_empty(), "office: {} clashes, first: {:?}", c.len(), c.first());
}
