// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `data/known_gaps.md` is checked here: its stub list equals the registry commands with no
//! handler and no branch in `EditorApp::run_cmd` (expected empty), and every gap row is sized.
use crate::editor::Editor;
use crate::shell::EditorApp;
use std::collections::BTreeSet;

const DOC: &str = include_str!("../data/known_gaps.md");
const SHELL: &str = include_str!("shell.rs");

/// The body of `run_cmd`: the command match lives there.
fn run_cmd_src() -> &'static str {
    let s = SHELL.find("fn run_cmd(").expect("run_cmd in shell.rs");
    let e = SHELL[s..].find("\n    }\n").map_or(SHELL.len(), |e| s + e);
    &SHELL[s..e]
}

/// Prefixes the match routes by `id.starts_with("…")`.
fn routed_prefixes(src: &str) -> Vec<&str> {
    src.match_indices("starts_with(\"")
        .filter_map(|(i, m)| {
            let rest = &src[i + m.len()..];
            rest.find('"').map(|e| &rest[..e])
        })
        .collect()
}

/// Registry commands that would only toast "unavailable": no handler, no branch.
pub(crate) fn stubs(a: &EditorApp) -> BTreeSet<String> {
    let src = run_cmd_src();
    let pre = routed_prefixes(src);
    a.sh.reg
        .all()
        .iter()
        .map(|c| c.id.as_str())
        .filter(|id| a.sh.reg.resolve(id) == *id)
        .filter(|id| !a.handlers.contains_key(*id))
        .filter(|id| !src.contains(&format!("\"{id}\"")))
        .filter(|id| !pre.iter().any(|p| id.starts_with(p)))
        .map(str::to_string)
        .collect()
}

/// Lines of the `## <title>` section, up to the next `## `.
fn section(title: &str) -> Vec<&'static str> {
    let head = format!("## {title}");
    let mut on = false;
    let mut out = vec![];
    for l in DOC.lines() {
        if l.starts_with("## ") {
            on = l.trim() == head;
            continue;
        }
        if on {
            out.push(l);
        }
    }
    out
}

/// `| a | b | … |` cells of the table rows of a section (header and rule skipped).
fn rows(title: &str) -> Vec<Vec<String>> {
    section(title)
        .into_iter()
        .filter(|l| l.trim_start().starts_with('|'))
        .skip(2)
        .map(|l| l.trim().trim_matches('|').split('|').map(|c| c.trim().to_string()).collect())
        .collect()
}

#[test]
fn the_stub_list_matches_the_registry_and_is_empty() {
    let a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    let found = stubs(&a);
    let listed: BTreeSet<String> = rows("Command stubs").into_iter().map(|r| r[0].trim_matches('`').to_string()).collect();
    assert_eq!(listed, found, "data/known_gaps.md «Command stubs» is out of date");
    assert!(found.is_empty(), "commands without an implementation: {found:?}");
}

#[test]
fn every_gap_is_sized_and_named_once() {
    let rs = rows("Gaps");
    assert!(!rs.is_empty(), "the gap table is empty or missing");
    let mut seen = BTreeSet::new();
    for r in &rs {
        assert_eq!(r.len(), 4, "row needs gap, size, area, next step: {r:?}");
        assert!(["S", "M", "L"].contains(&r[1].as_str()), "size S/M/L: {r:?}");
        assert!(!r[3].is_empty(), "next step: {r:?}");
        assert!(seen.insert(r[0].clone()), "listed twice: {}", r[0]);
    }
}

#[test]
fn the_scanner_sees_branches_and_prefixes() {
    let src = run_cmd_src();
    assert!(src.contains("\"edit.undo\""));
    assert!(routed_prefixes(src).contains(&"ui.layout.set."));
}
