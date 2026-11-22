// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! H02, the four-floor hospital wing with full ventilation, through the chat: `generate_project`
//! with the hospital typology and `route_mep` for supply, return and exhaust air, Apply once, then every oracle of the case on the applied model.

use crate::e2e_ai_common;

use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::json;

const ANSWER: &str = "Hospital wing of four floors with air handling units in the rooftop plant room. Supply, return \
and exhaust ducts are sized for the flow (EN 16798, ASHRAE 170, VDI 6022); fire and smoke dampers sit where ducts \
cross compartment walls. Operating theatres are at positive pressure and isolation rooms at negative pressure to the \
corridors (pressure cascade); every room has a terminal.";

#[test]
fn h02_hospital_through_the_tools_meets_every_oracle() {
    let mut ed = Editor::new_in_memory().unwrap();
    let spec = json!({"typology": "hospital_wing", "floors": 4});
    let mut calls = vec![tool("generate_project", json!({"spec": spec})), tool("load_tools", json!({"groups": ["mep"]}))];
    for kind in ["supply_air", "return_air", "exhaust_air"] {
        let args = json!({"title": kind, "why": "ventilation of the wing", "kind": kind, "from": {"riser": [1.0, 1.0]}, "friction_pa_m": 1.0});
        calls.push(tool("route_mep", args));
    }
    let (mut s, provider) = run(&mut ed, calls);
    let res = tool_results(&provider);
    assert!(res.len() == 5 && res.iter().all(|r| !r.1), "{res:?}");
    meets_every_oracle("H02_hospital_hvac.toml", &mut s, &mut ed, ANSWER);
}
