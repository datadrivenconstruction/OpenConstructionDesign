//! Automatic building services follow the regional switch `gen.mep_auto`: off by default for
//! the US norm pack, on for the DE pack; routing by hand works everywhere.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::ai::{mep_gate, project_plan};
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::json;

fn put(ed: &mut Editor, family: &str, ty: &str, x: f64, z: f64) -> i64 {
    let m = &mut ed.model;
    let f = m.load_library_family(family).unwrap();
    let ct = m.comp_type_id(f, ty).unwrap().unwrap();
    let mut s = ddc_model::CompSpec::at(ed.level, ct, x, 1.0);
    s.at.2 = z;
    match m.add_component(&s) {
        Ok(id) => return id,
        Err(e) if e.to_string().contains("wall-hosted") => {}
        Err(e) => panic!("{family}: {e}"),
    }
    let wall: i64 = m
        .sql()
        .query_row("SELECT id FROM wall ORDER BY id", [], |r| r.get(0))
        .unwrap();
    s.host = ddc_model::CompHost::Wall(wall);
    s.at = (x, 0.1, z);
    m.add_component(&s).unwrap()
}

/// Two walls, a WC and a washbasin, a distribution board and three sockets, with the norm pack.
fn rooms(pack: &str) -> (Editor, i64) {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    add_wall(&mut ed, level, (0.0, 0.0), (10.0, 0.0));
    add_wall(&mut ed, level, (10.0, 0.0), (10.0, 6.0));
    put(&mut ed, "WC", "Floor 650", 2.0, 0.0);
    put(&mut ed, "Washbasin", "500", 3.5, 0.8);
    let panel = put(&mut ed, "Distribution Board", "400x600", 6.0, 1.2);
    for x in [7.0, 8.0, 9.0] {
        put(&mut ed, "Socket Outlet", "Single", x, 0.3);
    }
    let mut n = ed.model.norm_locale().unwrap();
    n.pack = pack.into();
    ed.model.set_norm_locale(&n).unwrap();
    (ed, panel)
}

fn ducts(ed: &Editor) -> usize {
    let input = json!({"typology": "office", "floors": 3, "rules": {"mep.hvac.system": "vav"}});
    project_plan::build_on(&ed.model, &input).unwrap().project.hvac.ducts.len()
}

#[test]
fn us_pack_turns_automatic_services_off_by_default_and_de_leaves_them_on() {
    let (us, _) = rooms("us");
    let (de, _) = rooms("de");
    assert!(!mep_gate::is_on(&us.model, "de"));
    assert!(mep_gate::is_on(&de.model, "en-US"));
    assert_eq!(ducts(&us), 0);
    assert!(ducts(&de) > 0);
}

#[test]
fn the_person_can_turn_it_on_for_the_us_pack() {
    let (us, _) = rooms("us");
    us.model.set_ai_policy(mep_gate::POLICY_MEP_AUTO, "1").unwrap();
    assert!(ducts(&us) > 0);
}

#[test]
fn manual_route_mep_works_for_us_and_sizing_by_code_tables_is_refused() {
    let (mut ed, panel) = rooms("us");
    let (_s, provider) = run(
        &mut ed,
        vec![
            tool("route_mep", json!({"title": "Cold", "kind": "cold", "from": {"riser": [8.0, 4.0]}})),
            tool("power_devices", json!({"title": "Power", "panel": format!("comp:{panel}")})),
        ],
    );
    let r = tool_results(&provider);
    assert!(!r[0].1, "{}", r[0].0);
    assert!(r[1].1 && r[1].0.contains("gen.mep_auto"), "{}", r[1].0);
}

#[test]
fn power_devices_runs_for_the_de_pack() {
    let (mut ed, panel) = rooms("de");
    let (_s, provider) = run(
        &mut ed,
        vec![tool("power_devices", json!({"title": "Power", "panel": format!("comp:{panel}")}))],
    );
    let r = tool_results(&provider);
    assert!(!r[0].1, "{}", r[0].0);
}

/// The hand router reads no norm pack: its sources name none of the pack types.
#[test]
fn the_manual_router_does_not_read_norm_packs() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../model/src/mep");
    let banned = ["norm_pack", "PackSet", "NormPack", "norm_locale", "norm_project", "rule_norms", "checks::norm"];
    let mut seen = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        let src = std::fs::read_to_string(&p).unwrap();
        for b in banned {
            assert!(!src.contains(b), "{name} mentions {b}");
        }
        seen += 1;
    }
    assert!(seen >= 5, "{seen} router sources read");
}
