//! `plan_station` in one call: the levels below ground, the station's own types and one item
//! with the box, slabs, wells, stairs, columns and rooms; Apply gives exactly the preview and
//! the model holds what the station rules planned. A rule that cannot be met is refused by name.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;
#[path = "app2_fx/mod.rs"]
mod app2_fx;

use app2_fx::fx;

use ddc_app::editor::Editor;
use ddc_gen::project::rules::{Layer, RuleSet};
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::json;

fn scalar(ed: &Editor, q: &str) -> f64 {
    ed.model.sql().query_row(q, [], |r| r.get(0)).unwrap()
}

#[test]
fn a_deep_station_is_planned_on_new_levels_and_applied() {
    let mut ed = Editor::new_in_memory().unwrap();
    let levels_before = count(&ed.model, "level");
    let rules = json!({"station.peak_flow": 18000, "station.rail_depth": 18});
    let args = json!({"title": "Station", "why": "the line needs a station here", "rules": rules, "lang": "ru"});
    let (mut s, provider) = run(&mut ed, vec![tool("plan_station", args)]);
    let res = tool_results(&provider);
    assert_eq!(res.len(), 1, "{res:?}");
    let (text, err) = &res[0];
    assert!(!err, "{text}");
    for k in ["escalator_axes", "station.escalator_capacity", "excavation_depth", "gen.why.e.station_dwall"] {
        assert!(text.contains(k), "{k}: {text}");
    }
    apply_and_check(&mut s, &mut ed);

    let mut rs = RuleSet::new();
    rs.set(Layer::Brief, "station.peak_flow", 18000.0).unwrap();
    let g = ddc_gen::station::plan(&rs).unwrap();
    assert_eq!(count(&ed.model, "level"), levels_before + 3);
    assert!(scalar(&ed, "SELECT min(elevation) FROM level") < -12.0);
    assert_eq!(count(&ed.model, "wall") as usize, g.walls.len());
    assert_eq!(count(&ed.model, "slab") as usize, g.slabs.len());
    assert_eq!(count(&ed.model, "shaft") as usize, g.shafts.len());
    assert_eq!(count(&ed.model, "stair") as usize, g.stairs.len());
    assert_eq!(count(&ed.model, "col") as usize, g.columns.len());
    assert_eq!(count(&ed.model, "room") as usize, g.rooms.len());
    // escalators from the library family and a car in every lift shaft, at least two of them
    let lifts = g.shafts.iter().filter(|x| x.kind == ddc_gen::station::ShaftKind::Lift).count();
    assert!(lifts >= 2, "{lifts}");
    assert_eq!(count(&ed.model, "comp") as usize, g.escalators.len() + lifts);
    let cars = "SELECT count(*) * 1.0 FROM comp c JOIN comp_type t ON t.id = c.ctype                 JOIN comp_family f ON f.id = t.family WHERE f.name IN ('Lift Car', 'Platform Lift')";
    assert_eq!(scalar(&ed, cars) as usize, lifts);
    let esc = "SELECT count(*) * 1.0 FROM comp c JOIN comp_type t ON t.id = c.ctype \
               JOIN comp_family f ON f.id = t.family WHERE f.name = 'Escalator'";
    assert_eq!(scalar(&ed, esc) as usize, g.escalators.len());
    let dia ="SELECT count(*) * 1.0 FROM wall w JOIN wall_type t ON t.id = w.wtype WHERE t.name LIKE 'Diaphragm wall %'";
    assert_eq!(scalar(&ed, dia), 12.0, "every band of the box wall takes the diaphragm type");
    let named = format!("SELECT count(*) * 1.0 FROM level WHERE name = '{}'", fx("station_level"));
    assert_eq!(scalar(&ed, &named), 1.0, "new levels are named in the asked language");
    // the box is 190 m long: its walls reach from end to end
    let len = "SELECT max(x) - min(x) FROM (SELECT ax AS x FROM wall UNION ALL SELECT bx FROM wall)";
    assert!(scalar(&ed, len) >= 190.0, "{}", scalar(&ed, len));
}

#[test]
fn a_station_that_cannot_be_built_is_refused_by_its_rules() {
    let mut ed = Editor::new_in_memory().unwrap();
    let cases = [
        (json!({"rules": {"station.rail_depth": 9}}), "station.platform_clear"),
        (json!({"rules": {"massing.floors": 3}}), "only station.* rules"),
        (json!({"rules": {"station.peak_flow": 999999}}), "station.peak_flow"),
    ];
    let (_s, provider) = run(&mut ed, cases.iter().map(|c| tool("plan_station", c.0.clone())).collect());
    let res = tool_results(&provider);
    assert_eq!(res.len(), cases.len(), "{res:?}");
    for ((_, want), (text, err)) in cases.iter().zip(&res) {
        assert!(*err && text.contains(want), "want {want}: {text}");
    }
}
