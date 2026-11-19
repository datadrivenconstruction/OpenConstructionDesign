// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! H01 (the 300 m lattice tower) through the tools the recipe names, with a scripted model in
//! place of the chat: every model oracle of the case must pass on what the tools build.

use super::case::{self, Case};
use super::run::{replay_profile, run_case, Made, Opts};
use ddc_ai::fake::{Script, ScriptedProvider};
use serde_json::json;
use std::time::Duration;

fn tower_params() -> serde_json::Value {
    json!({"kind": "lattice_tower_300", "pattern": "x", "tower_height": 300, "base_width": 125,
           "platform_1": 57, "platform_2": 115, "platform_3": 276})
}

/// Russian text of the scripted runs (`tests/data/eval_ru.json`); `{sway}` becomes [`sway_mm`].
pub(super) fn ru(key: &str) -> String {
    let all: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/eval_ru.json"
    )))
    .expect("tests/data/eval_ru.json");
    let t = all[key].as_str().unwrap_or_else(|| panic!("eval_ru.json: no {key}"));
    if t.contains("{sway}") {
        t.replace("{sway}", &format!("{:.0}", sway_mm()))
    } else {
        t.to_string()
    }
}

/// The sway the generator reports for these params (`sway_top_mm`), the number the model reads
/// in the run_generator result; the reply states it instead of a fixed one.
fn sway_mm() -> f64 {
    let (_, m) = ddc_gen::steel::build_frame(&tower_params(), 0).unwrap();
    m.iter().find(|x| x.0 == "sway_top_mm").expect("sway_top_mm").1
}

#[test]
fn h01_tower_by_the_recipe_passes_every_oracle() {
    let c = Case::load(&case::evals_dir().join("cases").join("H01_lattice_tower_300.toml")).unwrap();
    let tool = |name: &str, args: serde_json::Value| vec![Script::Tool { name: name.into(), args }];
    let p = ScriptedProvider::new(vec![
        tool("load_tools", json!({"groups": ["scripts"]})),
        tool(
            "run_generator",
            json!({"id": "steel_frame", "variants": 1, "params": tower_params()}),
        ),
        tool("load_tools", json!({"groups": ["steel"]})),
        tool("connect_members", json!({"title": "Plates and bolts at the nodes"})),
        vec![Script::Text(format!("Built the tower; the wind sway at the top is about {:.0} mm (preliminary), with gusset plates and bolts at every node.", sway_mm()))],
    ]);
    let made = Made { provider: p, replay: None, rec: None };
    let o = Opts { out: None, profile: replay_profile(), price_in: 0.0, price_out: 0.0, turn_timeout: Duration::from_secs(1800) };
    let m = run_case(&c, &made, &o, None);
    let failed: Vec<_> = m.oracles.iter().filter(|r| !r.pass).collect();
    assert!(failed.is_empty() && !m.oracles.is_empty(), "{failed:#?}\n{}", m.note);
    assert!(m.refusals.is_empty(), "{:?}", m.refusals);
}

/// H11 is the same tower asked for in Russian: the recipe is named by its words, the answer is
/// Russian with the sway number, and every oracle of the case passes on what the tools build.
#[test]
fn h11_russian_tower_by_the_recipe_passes_every_oracle() {
    let c = Case::load(&case::evals_dir().join("cases").join("H11_ru_lattice_tower.toml")).unwrap();
    let tool = |name: &str, args: serde_json::Value| vec![Script::Tool { name: name.into(), args }];
    let p = ScriptedProvider::new(vec![
        tool("load_tools", json!({"groups": ["scripts"]})),
        tool(
            "run_generator",
            json!({"id": "steel_frame", "variants": 1, "params": tower_params()}),
        ),
        tool("load_tools", json!({"groups": ["steel"]})),
        tool("connect_members", json!({"title": ru("plates_title")})),
        vec![Script::Text(ru("tower_reply"))],
    ]);
    let made = Made { provider: p, replay: None, rec: None };
    let o = Opts { out: None, profile: replay_profile(), price_in: 0.0, price_out: 0.0, turn_timeout: Duration::from_secs(1800) };
    let m = run_case(&c, &made, &o, None);
    let failed: Vec<_> = m.oracles.iter().filter(|r| !r.pass).collect();
    assert!(failed.is_empty() && !m.oracles.is_empty(), "{failed:#?}
{}", m.note);
    assert!(m.refusals.is_empty(), "{:?}", m.refusals);
}

/// T01, a one-line request in Russian at "excellent detail": the same tower at
/// detail 3 in one run_generator call, which also puts plates and bolts at every node; every
/// oracle of the case on what the tools build.
#[test]
fn t01_owner_tower_at_full_detail_passes_every_oracle() {
    let c = Case::load(&case::evals_dir().join("cases").join("T01_tower_owner.toml")).unwrap();
    let tool = |name: &str, args: serde_json::Value| vec![Script::Tool { name: name.into(), args }];
    let mut params = tower_params();
    params["detail"] = json!(3);
    let p = ScriptedProvider::new(vec![
        tool("help", json!({"topic": "lattice_tower"})),
        tool("load_tools", json!({"groups": ["scripts"]})),
        tool("run_generator", json!({"id": "steel_frame", "variants": 1, "params": params})),
        vec![Script::Text(ru("tower_300_reply"))],
    ]);
    let made = Made { provider: p, replay: None, rec: None };
    let o = Opts { out: None, profile: replay_profile(), price_in: 0.0, price_out: 0.0, turn_timeout: Duration::from_secs(3600) };
    let m = run_case(&c, &made, &o, None);
    let failed: Vec<_> = m.oracles.iter().filter(|r| !r.pass).collect();
    assert!(failed.is_empty() && !m.oracles.is_empty(), "{failed:#?}\n{}", m.note);
    assert!(m.refusals.is_empty(), "{:?}", m.refusals);
}
