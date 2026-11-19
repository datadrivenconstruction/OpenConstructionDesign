// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The G suite on scripted answers: every G case that has a scripted answer here runs through
//! the eval executor (plan, Apply, oracles); one run prints the pass rate of the whole suite
//! and the oracles still failing. Add a case by adding a row to `SCRIPTED`.

use super::case::{evals_dir, Case};
use super::run::{replay_profile, run_case, Made, Opts};
use ddc_ai::fake::{Script, ScriptedProvider};
use serde_json::json;
use std::time::Duration;

/// (case file, recipe the scripted model runs); the oracle names that must pass.
const SCRIPTED: &[(&str, &str, &[&str])] = &[
    ("G25_wind_turbine", "wind_turbine", &["comp:blade|rotor"]),
    ("G26_ferris_wheel", "ferris_wheel", &["comp:gondola|cabin"]),
];

fn opts() -> Opts {
    Opts {
        out: None,
        profile: replay_profile(),
        price_in: 0.0,
        price_out: 0.0,
        turn_timeout: Duration::from_secs(120),
    }
}

#[test]
fn g_suite_scripted_cases_run_and_their_targeted_oracles_pass() {
    let (mut passed, mut oracles, mut total) = (0, 0, 0);
    let mut targeted = vec![];
    for (id, recipe, must) in SCRIPTED {
        let case = Case::load(&evals_dir().join("cases").join(format!("{id}.toml"))).unwrap();
        let script = ddc_script::recipes::recipe(recipe).unwrap();
        let provider = ScriptedProvider::new(vec![
            vec![Script::Tool {
                name: "run_script".into(),
                args: json!({"title": recipe.replace('_', " "), "script": script, "params": {}}),
            }],
            vec![Script::Text("done".into())],
        ]);
        let made = Made { provider, replay: None, rec: None };
        let m = run_case(&case, &made, &opts(), None);
        assert!(m.ran(), "{id}: {}", m.note);
        total += 1;
        passed += usize::from(m.pass());
        oracles += m.oracles.iter().filter(|o| o.pass).count();
        let n = m.oracles.len();
        eprintln!("G-suite {id}: {:?}, oracles {}/{n}", m.status, m.oracles.iter().filter(|o| o.pass).count());
        for o in m.oracles.iter().filter(|o| !o.pass) {
            eprintln!("  fail {}: expected {}, measured {}", o.name, o.expected, o.measured);
        }
        for want in *must {
            let o = m.oracles.iter().find(|o| o.name.contains(want));
            targeted.push(format!("{id} {want}: {}", o.map_or("missing".into(), |o| format!("{} ({})", o.pass, o.measured))));
            assert!(o.is_some_and(|o| o.pass), "{id}: oracle `{want}` {o:?}");
        }
    }
    eprintln!("G-suite scripted: {passed}/{total} cases pass, {oracles} oracles pass; {targeted:?}");
}
