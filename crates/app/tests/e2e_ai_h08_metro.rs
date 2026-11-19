// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! H08, the 18 m deep metro station, through the chat: `load_tools` station, then `plan_station` with the case's depth and
//! flow, Apply once, then every oracle of the case on the applied model.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::json;

#[test]
fn h08_metro_station_through_the_tools_meets_every_oracle() {
    let mut ed = Editor::new_in_memory().unwrap();
    let args = json!({"title": "Metro station", "why": "island platform 18 m below ground with a concourse above",
                      "rules": {"station.rail_depth": 18, "station.peak_flow": 18000}});
    // plan_station alone: box, vaults over the tracks and platform, ground, escalators, lifts, shafts
    let calls = vec![tool("load_tools", json!({"groups": ["station"]})), tool("plan_station", args)];
    let n = calls.len();
    let (mut s, provider) = run(&mut ed, calls);
    let res = tool_results(&provider);
    assert!(res.len() == n && res.iter().all(|r| !r.1), "{res:?}");
    meets_every_oracle("H08_metro_station.toml", &mut s, &mut ed, "Station planned: platform at 18 m, concourse above, six escalators, two lifts, two shafts.");
}

/// The vault roofs read back from the model after Apply: all on the platform level, springing
/// at the platform plus the planned offset (never under station.vault_spring), the crown of
/// each under the concourse slab, one row per bay over the two trackways and the platform.
#[test]
fn station_vault_roofs_sit_on_the_platform_under_the_concourse_slab() {
    use ddc_gen::project::rules::def;
    use ddc_model::RoofKind;
    let mut ed = Editor::new_in_memory().unwrap();
    let args = json!({"title": "Metro station", "why": "vault check", "rules": {"station.rail_depth": 18}});
    let (mut s, provider) = run(&mut ed, vec![tool("load_tools", json!({"groups": ["station"]})), tool("plan_station", args)]);
    let res = tool_results(&provider);
    assert!(res.len() == 2 && res.iter().all(|r| !r.1), "{res:?}");
    apply_and_check(&mut s, &mut ed);
    let levels = ed.model.level_list().unwrap();
    let z_of = |name: &str| levels.iter().find(|l| l.1 == name).unwrap_or_else(|| panic!("{name} in {levels:?}"));
    let (platform, concourse) = (z_of("Platform level"), z_of("Concourse level"));
    let d = |id: &str| def(id).and_then(|x| x.default).unwrap();
    let under_slab = concourse.2 - d("station.floor_slab");
    let roofs = e2e_common::ids(&ed.model, "roof");
    let (len_box, bay) = (d("station.platform_length") + 2.0 * d("station.end_zone"), d("station.vault_bay"));
    let per_row = (len_box / bay - 1e-9).ceil() as usize;
    assert_eq!(roofs.len(), 3 * per_row, "a vault per bay over each trackway and the platform");
    for id in roofs {
        let r = ed.model.roof(id).unwrap();
        assert_eq!(r.level, platform.0, "vault {id} stands on the platform level");
        let RoofKind::Extrusion { profile, .. } = &r.kind else { panic!("vault {id} is an extrusion: {:?}", r.kind) };
        let rise = profile.iter().map(|p| p.1).fold(f64::MIN, f64::max);
        let spring = platform.2 + r.offset;
        assert!(r.offset >= d("station.vault_spring") - 1e-9, "vault {id} springs {} m over the platform", r.offset);
        assert!((profile[0].1).abs() < 1e-9 && (profile[profile.len() - 1].1).abs() < 1e-9, "springs at both ends");
        let crown = spring + rise;
        assert!(crown <= under_slab + 1e-6, "vault {id} crown {crown} over the slab underside {under_slab}");
        assert!(crown >= under_slab - 0.5, "vault {id} crown {crown} is far under the slab {under_slab}");
    }
}
