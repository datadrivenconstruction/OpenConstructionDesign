// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Meta test: every top-level window or dialog of the app is either probed by the layout contract
//! (`src/layout_contract.rs`) or listed in `EXEMPT` with a reason.
//!
//! Windows are found in the sources by their constructor (`Dialog::new("key"`, `Window::new(`,
//! `modal_dialog("key"`). A window counts as probed when the contract file names its key (or an
//! id that starts with `key.`) as a string literal, so modules that add probes need no
//! change here. When you add a probe for an exempt window, delete its line from `EXEMPT` and lower
//! `EXEMPT_MAX`: the list may only shrink.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Windows without a headless layout probe yet: (key, one-line reason).
const EXEMPT: &[(&str, &str)] = &[
    ("ai.sent", "AI panel: needs an agent session"),
    ("ai.settings", "AI panel: needs an agent session"),
    ("clip.dialog", "shell dialog, not yet probed headless"),
    ("collab.diff", "needs a second project file"),
    ("collab.diff.failed", "needs a second project file"),
    ("collab.diff.work", "needs a second project file"),
    ("collab.merge", "needs a second project file"),
    ("collab.merge.refused", "needs a second project file"),
    ("collab.merge.work", "needs a second project file"),
    ("collab.options", "needs a second project file"),
    ("collab.options.confirm", "needs a second project file"),
    ("curtain_edit", "needs a selected element or tool state"),
    ("drawing_import", "needs a selected element or tool state"),
    ("drawing_layers", "needs a selected element or tool state"),
    ("drawing_recognize", "needs a selected element or tool state"),
    ("fe.clash", "shell dialog, not yet probed headless"),
    ("import", "shell dialog, not yet probed headless"),
    ("import_dev_key", "shell dialog, not yet probed headless"),
    ("mcp.feed", "AI panel: needs an agent session"),
    ("mep.browser", "needs a selected element or tool state"),
    ("mep.delete", "needs a selected element or tool state"),
    ("mesh_tools", "needs a selected element or tool state"),
    ("type_dialog.panels.properties", "shell dialog, not yet probed headless"),
    ("rebar.analytical", "needs a selected element or tool state"),
    ("rebar.schedule", "needs a selected element or tool state"),
    ("ri12.update", "shell dialog, not yet probed headless"),
    ("profile_sketch.ask", "shell dialog, not yet probed headless"),
    ("roof_edge_form", "needs a selected element or tool state"),
    ("roof_edges", "needs a selected element or tool state"),
    ("sch.fields", "needs a sheet or schedule with content"),
    ("sched_import.import", "needs a sheet or schedule with content"),
    ("sheet_delete", "needs a sheet or schedule with content"),
    ("sheet_issue", "needs a sheet or schedule with content"),
    ("sheet_place_view", "needs a sheet or schedule with content"),
    ("sheet_print", "needs a sheet or schedule with content"),
    ("sheet_properties", "needs a sheet or schedule with content"),
    ("sheet_sets", "needs a sheet or schedule with content"),
    ("sheet_stamp", "needs a sheet or schedule with content"),
    ("source_conflicts", "shell dialog, not yet probed headless"),
    ("sweep_profiles", "needs a selected element or tool state"),
    ("tag_all", "needs a selected element or tool state"),
    ("topo_import", "needs a topography source"),
    ("topo_site", "needs a topography source"),
    ("topo_tool", "needs a topography source"),
    ("wall_edit", "needs a selected element or tool state"),
    ("xs45.drop", "shell dialog, not yet probed headless"),
];

/// Ceiling of the exempt list (baseline): lower it with every probe added, never raise it.
const EXEMPT_MAX: usize = 46;

/// Windows probed through a helper that does not repeat their key in the contract file:
/// (key, text that must occur in the contract file).
const PROBED_VIA: &[(&str, &str)] = &[
    ("mcp.settings", "mcp_bridge::layout_probe("),
    ("ai.audit", "audit_ui::{layout_probe"),
    ("ai.panel", "tests_states::layout_probe("),
];

const CTORS: [&str; 3] = ["Dialog::new(", "Window::new(", "modal_dialog("];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// Keys of the windows constructed in `src`: the first string literal of the constructor call,
/// reached only through names, paths, commas and opening parentheses.
fn window_keys(src: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for ctor in CTORS {
        for (at, _) in src.match_indices(ctor) {
            let rest = &src[at + ctor.len()..];
            for (i, c) in rest.char_indices() {
                if c == '"' {
                    let tail = &rest[i + 1..];
                    if let Some(end) = tail.find('"') {
                        let key = &tail[..end];
                        if key.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                            keys.push(key.to_string());
                        }
                    }
                    break;
                }
                if !(c.is_alphanumeric() || c.is_whitespace() || "_:(,".contains(c)) {
                    break;
                }
            }
        }
    }
    keys
}

fn all_windows() -> BTreeSet<String> {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src_dir, &mut files);
    let mut keys = BTreeSet::new();
    for f in files {
        let name = f.to_string_lossy().replace('\\', "/");
        // The contract itself and the scaffold (its tests build throw-away windows).
        if name.ends_with("/layout_contract.rs") || name.ends_with("/ui_kit/dialog.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&f).unwrap();
        keys.extend(window_keys(&text));
    }
    // Keys starting with "t." belong to throw-away windows of unit tests.
    keys.retain(|k| !k.starts_with("t."));
    keys
}

fn contract_text() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/layout_contract.rs"))
        .unwrap()
}

fn is_probed(key: &str, contract: &str) -> bool {
    contract.contains(&format!("\"{key}\"")) || contract.contains(&format!("\"{key}."))
        || PROBED_VIA.iter().any(|(k, via)| *k == key && contract.contains(via))
}

#[test]
fn every_window_is_probed_or_exempt() {
    let windows = all_windows();
    assert!(windows.len() > 60, "the window scan found only {} windows", windows.len());
    let contract = contract_text();
    let exempt: BTreeSet<&str> = EXEMPT.iter().map(|(k, _)| *k).collect();
    let loose: Vec<&String> = windows
        .iter()
        .filter(|k| !is_probed(k, &contract) && !exempt.contains(k.as_str()))
        .collect();
    assert!(
        loose.is_empty(),
        "windows neither probed in layout_contract.rs nor listed in EXEMPT (add a probe; do not \
         grow the list): {loose:?}"
    );
}

#[test]
fn the_exempt_list_only_shrinks() {
    assert!(
        EXEMPT.len() <= EXEMPT_MAX,
        "EXEMPT has {} entries, the baseline is {EXEMPT_MAX}",
        EXEMPT.len()
    );
    assert!(
        EXEMPT.len() == EXEMPT_MAX,
        "EXEMPT shrank to {}: lower EXEMPT_MAX to match",
        EXEMPT.len()
    );
    let mut seen = BTreeSet::new();
    for (key, why) in EXEMPT {
        assert!(seen.insert(*key), "duplicate EXEMPT entry {key}");
        assert!(!why.trim().is_empty() && !why.contains('\n'), "{key}: one-line reason needed");
    }
}

#[test]
fn exempt_entries_name_existing_windows() {
    let windows = all_windows();
    let gone: Vec<&str> =
        EXEMPT.iter().map(|(k, _)| *k).filter(|k| !windows.contains(*k)).collect();
    assert!(gone.is_empty(), "EXEMPT names windows that no longer exist (delete them): {gone:?}");
}

#[test]
fn the_scan_reads_keys_through_wrappers() {
    let src = "kit::Dialog::new(\n    \"a.b\",\n    t)\negui::Window::new(l(lang, \"x1\"))\n\
               Self(crate::ui_kit::Dialog::new(key, title, size))\nmodal_dialog(\"c\", ctx)";
    let mut got = window_keys(src);
    got.sort();
    assert_eq!(got, vec!["a.b", "c", "x1"]);
}
