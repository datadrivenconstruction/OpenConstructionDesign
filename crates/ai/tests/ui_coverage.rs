//! Every interactive command is accounted for in `data/ui_coverage.tsv`: the API tool that does
//! the same, why it is UI-only, or, for a model edit with no API form yet, the model call it
//! makes. `docs/api/COVERAGE.md` is generated from that table; `DDC_BLESS_COVERAGE=1` rewrites it.

use ddc_ai::registry;
use serde_json::Value as J;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Model edits still without an API form. It may only go down: a new edit command comes with
/// its tool, and covering a listed one lowers it.
const GAP_CEILING: usize = 131;

/// Why a command is UI-only.
const UI_ONLY: &[(&str, &str)] = &[
    ("view", "changes the view or the display, not the model"),
    ("nav", "moves around the project: views, levels, tabs"),
    ("dialog", "opens a window; its edits are their own commands"),
    ("select", "changes the selection, which the API names by reference instead"),
    ("file", "opens, saves or exchanges files on the person's disk"),
    ("session", "drives the editor session: tool state, undo, chat, panels"),
    ("clipboard", "the system clipboard, which the API replaces with copy_elements"),
];

struct Row {
    kind: String,
    value: String,
    note: String,
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn ui_commands() -> Vec<String> {
    let data = root().join("../ui/data");
    let mut files = vec![data.join("commands.json")];
    let mut more: Vec<PathBuf> = fs::read_dir(data.join("commands.d"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    more.sort();
    files.extend(more);
    let mut ids = vec![];
    for f in files {
        let v: J = serde_json::from_str(&fs::read_to_string(&f).unwrap()).unwrap();
        for c in v.as_array().unwrap_or_else(|| panic!("{f:?}: an array")) {
            ids.push(c["id"].as_str().unwrap_or_else(|| panic!("{f:?}: {c}")).to_string());
        }
    }
    ids
}

fn table() -> BTreeMap<String, Row> {
    let text = fs::read_to_string(root().join("data/ui_coverage.tsv")).unwrap();
    let mut rows = BTreeMap::new();
    for line in text.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let f: Vec<&str> = line.split('\t').collect();
        assert_eq!(f.len(), 4, "four columns: {line}");
        let row = Row { kind: f[1].into(), value: f[2].into(), note: f[3].trim().into() };
        assert!(rows.insert(f[0].to_string(), row).is_none(), "{} twice", f[0]);
    }
    rows
}

#[test]
fn every_ui_command_has_an_api_tool_or_a_reason() {
    let ui = ui_commands();
    let rows = table();
    let mut gaps = 0;
    for id in &ui {
        let r = rows.get(id).unwrap_or_else(|| {
            panic!("{id}: new command; add it to data/ui_coverage.tsv with its API tool")
        });
        assert!(!r.note.is_empty(), "{id}: say what it does");
        match r.kind.as_str() {
            "api" => {
                let tool = r.value.split(':').next().unwrap();
                assert!(registry::find(tool).is_some(), "{id}: no API tool {tool}");
            }
            "gap" => gaps += 1,
            k => assert!(UI_ONLY.iter().any(|u| u.0 == k), "{id}: unknown kind {k}"),
        }
    }
    for id in rows.keys() {
        assert!(ui.contains(id), "{id}: no such command any more; drop its row");
    }
    assert!(
        gaps <= GAP_CEILING,
        "{gaps} model edits without an API form, at most {GAP_CEILING}: give the new one a tool"
    );
    assert_eq!(gaps, GAP_CEILING, "fewer gaps now: lower GAP_CEILING to {gaps}");
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|")
}

fn coverage_md() -> String {
    let rows = table();
    let count = |k: &str| rows.values().filter(|r| r.kind == k).count();
    let ui_only = rows.len() - count("api") - count("gap");
    let mut s = String::from("# Interactive commands and the API\n\n");
    s += "Generated from `crates/ai/data/ui_coverage.tsv` by `crates/ai/tests/ui_coverage.rs`;\n";
    s += "edit the table, then run that test with `DDC_BLESS_COVERAGE=1`.\n\n";
    s += &format!(
        "{} commands: {} with an API tool, {} UI-only, {} model edits without an API form yet.\n\n",
        rows.len(),
        count("api"),
        ui_only,
        count("gap")
    );
    s += "| Kind | Meaning | Commands |\n|---|---|---|\n";
    s += &format!("| api | an API tool does the same | {} |\n", count("api"));
    for (k, why) in UI_ONLY {
        s += &format!("| {k} | UI-only: {why} | {} |\n", count(k));
    }
    s += &format!("| gap | edits the model, no API form yet | {} |\n\n", count("gap"));
    s += "| Command | Kind | API tool or model call | Note |\n|---|---|---|---|\n";
    for (id, r) in &rows {
        let v = if r.value == "-" { String::new() } else { format!("`{}`", r.value) };
        s += &format!("| `{id}` | {} | {v} | {} |\n", r.kind, cell(&r.note));
    }
    s
}

#[test]
fn coverage_doc_matches_the_table() {
    let path = root().join("../../docs/api/COVERAGE.md");
    let want = coverage_md();
    if std::env::var_os("DDC_BLESS_COVERAGE").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &want).unwrap();
    }
    let have = fs::read_to_string(&path).unwrap_or_default().replace("\r\n", "\n");
    assert!(have == want, "docs/api/COVERAGE.md is stale: run with DDC_BLESS_COVERAGE=1");
}
