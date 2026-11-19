//! A boiler house from a brief end to end: the assistant drafts it, Apply writes units with
//! their nozzle connectors, the items on the nozzles, pipes with fittings, supports and steel
//! in one undo step, every connector joined; the same plant designed again changes nothing.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::editor::Editor;
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::json;

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}

const BRIEF: &str = "Boiler house 2 MW with 3 boilers";

#[test]
fn a_boiler_house_is_applied_in_one_step_with_every_port_joined() {
    let mut ed = editor();
    let (comps0, runs0) = (count(&ed.model, "comp"), count(&ed.model, "mep_run"));
    let (mut s, provider) = run(&mut ed, vec![tool("design_plant", json!({"why": "plant", "brief": BRIEF}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let reply = parsed(&results[0].0);
    assert_eq!(reply["plant_id"], "K1", "{reply}");
    assert_eq!(count(&ed.model, "comp"), comps0, "nothing reaches the model before Apply");
    assert_eq!(count(&ed.model, "mep_run"), runs0);

    // one journal row, the preview is what was applied, one undo restores the rows exactly
    apply_and_check(&mut s, &mut ed);

    let el = &reply["elements"];
    let placed = count(&ed.model, "comp") - comps0;
    assert_eq!(placed, el["placed"].as_i64().unwrap(), "{el}");
    assert!(el["units"].as_i64().unwrap() >= 5, "boilers, pumps and the rest: {el}");
    assert_eq!(count(&ed.model, "mep_run") - runs0, el["pipe_runs"].as_i64().unwrap());
    assert!(el["pipe_runs"].as_i64().unwrap() > 10);

    let net = ed.model.mep_network(None).unwrap();
    assert!(net.fittings.len() > 10, "elbows and tees come with the pipes: {}", net.fittings.len());
    assert!(net.open_ends.is_empty(), "pipe ends meet nothing: {:?}", &net.open_ends[..net.open_ends.len().min(5)]);
    let ports = ed.model.component_ports(None).unwrap();
    assert!(ports.len() > 20);
    let on_pipe = |p: &ddc_model::WorldPort| net.nodes.iter().any(|n| n.port.as_ref().is_some_and(|q| q.0 == p.comp && q.1 == p.name));
    let near = |a: (f64, f64, f64), b: (f64, f64, f64)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt() < 5e-3;
    for p in &ports {
        let to_item = ports.iter().any(|o| o.comp != p.comp && near(o.at, p.at));
        assert!(on_pipe(p) || to_item, "connector {} of {} meets nothing at {:?}", p.name, p.comp, p.at);
    }
    // every connector is linked to its line's system
    assert_eq!(count(&ed.model, "mep_port"), ports.len() as i64);
    // one undo takes the whole plant away, redo brings it back
    ed.undo();
    assert_eq!((count(&ed.model, "comp"), count(&ed.model, "mep_run"), count(&ed.model, "mep_port")), (comps0, runs0, 0));
    ed.redo();
    assert_eq!(count(&ed.model, "comp") - comps0, placed);

    drop(s);
    // the same plant designed again: everything stays, ids and all
    let (_s2, provider) = run(&mut ed, vec![tool("design_plant", json!({"why": "again", "brief": BRIEF, "plant_id": "K1"}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let again = parsed(&results[0].0);
    assert_eq!(again["plant_id"], "K1");
    assert_eq!(again["unchanged"], true, "{again}");
    assert_eq!(again["changes"], json!({"created": 0, "changed": 0, "deleted": 0}), "{again}");
}

const H03: &str = "Small chemical process plant outdoors: a jacketed reactor, two feed tanks, a distillation column with reboiler and condenser, pumps as duty/standby pairs, heat exchangers, a steel pipe rack about 4 m high, sloped drains, supports, platforms with caged ladders and a bund wall round the tanks.";

#[test]
fn a_chemical_plant_gets_frame_walls_slopes_and_a_reply_with_sizes() {
    let mut ed = editor();
    let (mut s, provider) = run(&mut ed, vec![tool("design_plant", json!({"why": "plant", "brief": H03}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let text = &results[0].0;
    let size = regex_lite_size(text);
    assert!(size, "lines carry a DN: {text}");
    let reply = parsed(text);
    assert!(reply["lines"].as_array().is_some_and(|l| l.len() >= 10), "{reply}");
    let (c0, b0, w0) = (count(&ed.model, "col"), count(&ed.model, "beam"), count(&ed.model, "wall"));
    apply_and_check(&mut s, &mut ed);
    let (cols, beams) = (count(&ed.model, "col") - c0, count(&ed.model, "beam") - b0);
    assert!(cols + beams >= 20, "{cols} columns, {beams} beams");
    assert!(count(&ed.model, "wall") - w0 >= 4, "the bund");
    let sloped = ed.model.sql().query_row("SELECT COUNT(*) FROM mep_run WHERE slope <> 0", [], |r| r.get::<_, i64>(0)).unwrap();
    assert!(sloped >= 5, "{sloped} sloped runs");
    let net = ed.model.mep_network(None).unwrap();
    assert!(net.open_ends.is_empty(), "{:?}", &net.open_ends[..net.open_ends.len().min(5)]);
    ed.undo();
    assert_eq!((count(&ed.model, "col"), count(&ed.model, "beam"), count(&ed.model, "wall")), (c0, b0, w0));
}

fn regex_lite_size(t: &str) -> bool {
    t.match_indices("DN ").any(|(i, _)| t[i + 3..].chars().next().is_some_and(|c| c.is_ascii_digit()))
}
