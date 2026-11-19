// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Every prompt of the G (any geometry) eval cases, in every language, routes to the group
//! that holds the tools for its subject, so they are offered without a load_tools call.

use ddc_ai::toolset::intent_groups;

/// Groups a case's prompt must reach besides `scripts` (where run_script and the blocks live):
/// structures whose members come from the steel tools.
const ALSO: &[(&str, &str)] = &[("G08", "steel"), ("G14", "steel"), ("G27", "steel")];

#[test]
fn every_g_prompt_routes_to_the_group_of_its_subject() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("evals/cases");
    let (mut seen, mut lost) = (0, vec![]);
    for e in std::fs::read_dir(&dir).expect("evals/cases") {
        let path = e.expect("entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        if !(name.starts_with('G') && name.ends_with(".toml")) {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("case");
        let say = text
            .lines()
            .find_map(|l| l.strip_prefix("say = "))
            .and_then(|q| serde_json::from_str::<String>(q.trim()).ok())
            .unwrap_or_else(|| panic!("{name}: no single-line say"));
        seen += 1;
        let got = intent_groups(&say);
        let mut want = vec!["scripts"];
        want.extend(ALSO.iter().filter(|(c, _)| name.starts_with(c)).map(|(_, g)| *g));
        for g in want {
            if !got.contains(g) {
                lost.push(format!("{name}: no {g} in {got:?}: {say}"));
            }
        }
    }
    assert!(seen >= 90, "only {seen} G cases found");
    assert!(lost.is_empty(), "prompts that miss their group:\n{}", lost.join("\n"));
}
