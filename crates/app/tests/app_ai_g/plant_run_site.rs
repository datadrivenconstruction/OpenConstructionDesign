// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rebuilds a plant run folder (`PLANT_RUN_DIR` with `brief.json`): `design_plant` through the
//! chat tools, Apply, the model saved as `plant.ddc` and the run's figures as `log.jsonl`.

use crate::e2e_ai_common;

use ddc_app::editor::Editor;
use ddc_gen::plant;
use e2e_ai_common::*;
use serde_json::{json, Value as J};

#[test]
#[ignore]
fn plant_run_folder_is_rebuilt_from_its_brief() {
    let dir = std::path::PathBuf::from(std::env::var("PLANT_RUN_DIR").expect("PLANT_RUN_DIR"));
    let brief: J = serde_json::from_str(&std::fs::read_to_string(dir.join("brief.json")).unwrap()).unwrap();
    let p = plant::design(&brief).unwrap();

    let mut ed = Editor::new_in_memory().unwrap();
    // the rack frames and platform steel take the project's first member type, as in a project
    let hea = ddc_geom::member::Shape::I { h: 0.2, b: 0.2, tw: 0.0065, tf: 0.01 };
    ed.model.add_member_type("HEA 200", &hea, "S355").unwrap();
    ed.reload().unwrap();
    let calls = vec![
        tool("load_tools", json!({"groups": ["plant"]})),
        tool("design_plant", json!({"why": "process plant of the brief", "brief": brief})),
    ];
    let (mut s, provider) = run(&mut ed, calls);
    let res = tool_results(&provider);
    assert!(res.iter().all(|r| !r.1), "{res:?}");
    apply_and_check(&mut s, &mut ed);
    drop(s);
    let out = dir.join("plant.ddc");
    let _ = std::fs::remove_file(&out);
    ed.model.backup_to(&out).unwrap();

    let r = &p.report;
    let pipe_m: f64 = p.kit.runs.iter().map(|k| k.points.windows(2).map(|w| (w[1] - w[0]).len()).sum::<f64>()).sum();
    let valves = p.piping.items.iter().filter(|i| i.item.operated()).count();
    let mut by: std::collections::BTreeMap<&str, usize> = Default::default();
    for s in &p.piping.supports {
        *by.entry(s.kind.name()).or_default() += 1;
    }
    let lines = [
        json!({"step": "design_plant", "cmd": "cargo test -p ddc-app --test plant_run_site -- --ignored (design_plant with brief.json, Apply, save)", "brief": brief}),
        json!({"project": "process_plant", "units": p.equipment.iter().filter(|e| e.kind.in_row()).count(),
               "nozzles": r.nozzles, "nozzles_connected": r.connected, "lines": p.lines.len(),
               "pipe_runs": p.kit.runs.len(), "pipe_m": (pipe_m * 100.0).round() / 100.0,
               "supports": p.piping.supports.len(), "supports_by_kind": by, "valves": valves,
               "rack_tiers_m": p.piping.tiers, "outdoor": p.brief.outdoor,
               "report_ok": r.issues.is_empty(), "issues": r.issues.len()}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(dir.join("log.jsonl"), text).unwrap();
}
