// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The browser's autosaved draft: a `.ddcrev` built in memory from the open editor must pass the
//! same checks as `ddc publish` (manifest, hashes, rows, diff against the chain tip).

use std::collections::BTreeSet;

use ddc_app::editor::{Editor, Sel};
use ddc_app::link::package;
use ddc_app::link::publish::{package_draft, rows_of_model, DraftArgs};
use ddc_io::contract::{validate, Doc};
use ddc_io::elements_parquet::{read_parquet, ElementRow};
use serde_json::Value;

const PROJECT: &str = "746402cb-14a8-430c-b3b9-9c6908fa75d7";
const TIP: &str = "9c1d2e3f-4a5b-4c6d-8e7f-0a1b2c3d4e5f";
const DRAFT: &str = "5b0c7e21-9d3f-4a8e-b6c1-2f4d8a9e0c13";

fn example() -> Editor {
    let brief: Value = serde_json::from_str(include_str!("../../io/contract/examples/brief.de-residential.json")).unwrap();
    ddc_app::link::gen::build(&brief).unwrap().0
}

fn keys(rows: &[ElementRow]) -> BTreeSet<String> {
    rows.iter().map(|r| r.key.clone()).collect()
}

fn draft(ed: &mut Editor, parent: Option<(String, Vec<ElementRow>)>) -> (Value, std::collections::BTreeMap<String, Vec<u8>>) {
    let a = DraftArgs { project_id: PROJECT.into(), name: "Entwurf".into(), revision_id: Some(DRAFT.into()), parent, ..Default::default() };
    let (zip, rows) = package_draft(ed, a).unwrap();
    let m = package::verify_bytes(&zip).unwrap();
    validate(Doc::Revision, &m).unwrap();
    let files = package::read_all_bytes(&zip).unwrap();
    // the rows handed back (the page's ifc_class/type_key per key) are the package's
    let ids = |r: &[ElementRow]| r.iter().map(|x| (x.key.clone(), x.ifc_class.clone(), x.type_key.clone())).collect::<BTreeSet<_>>();
    assert_eq!(ids(&read_parquet(files["elements.parquet"].clone()).unwrap()), ids(&rows));
    (m, files)
}

#[test]
fn a_draft_built_in_memory_passes_the_contract_and_diffs_against_the_tip() {
    let mut ed = example();
    let (m, files) = draft(&mut ed, None);
    assert_eq!(m["project_ref"]["erp_project_id"], PROJECT);
    assert_eq!(m["revision_id"], DRAFT);
    assert!(m["parent_revision_id"].is_null());
    assert!(!files.contains_key("diff.json"));
    let tip_rows = read_parquet(files["elements.parquet"].clone()).unwrap();
    assert!(!tip_rows.is_empty());

    // a tip the page holds as a model gives the rows its package carries
    assert_eq!(keys(&rows_of_model(&files["model.ddc"]).unwrap()), keys(&tip_rows));
    // the model in the package opens again and keeps the keys minted for it
    assert_eq!(keys(&read_parquet(draft(&mut ed, None).1["elements.parquet"].clone()).unwrap()), keys(&tip_rows));

    // an edit, then the draft against the tip: diff.json names the removed wall
    let (wall, key) = ed.model.element_keys().unwrap().into_iter().find(|((t, _), _)| t == "wall").map(|((_, id), k)| (id, k)).unwrap();
    ed.selection = vec![Sel::Wall(wall)];
    ed.delete_selection();
    let (m, files) = draft(&mut ed, Some((TIP.into(), tip_rows)));
    assert_eq!(m["parent_revision_id"], TIP);
    let diff: Value = serde_json::from_slice(&files["diff.json"]).unwrap();
    validate(Doc::Diff, &diff).unwrap();
    assert!(diff["removed"].as_array().unwrap().iter().any(|k| k == key.as_str()), "{key} in {}", diff["removed"]);
}

/// The demo stand's tip revision: the DE example's package, written to `DDC_DEMO_DDCREV`.
/// `cargo test -p ddc-app --features link --test web_draft -- --ignored demo`
#[test]
#[ignore]
fn demo_package() {
    let mut ed = example();
    let a = DraftArgs { project_id: PROJECT.into(), name: "Wohnhaus München".into(), ..Default::default() };
    let (zip, rows) = package_draft(&mut ed, a).unwrap();
    validate(Doc::Revision, &package::verify_bytes(&zip).unwrap()).unwrap();
    assert!(rows.iter().any(|r| r.ifc_class.starts_with("IfcWall")));
    std::fs::write(std::env::var("DDC_DEMO_DDCREV").unwrap(), zip).unwrap();
}
