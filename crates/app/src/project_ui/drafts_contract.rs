//! Contract: every place that keeps an editable draft outside the model is registered in
//! `drafts.rs`, so Close, New and Open ask about it. Source-level: the scan finds files that
//! declare a `thread_local!` and name a draft or dirty flag, then checks each against the lists
//! below. A new holder fails the scan until it is registered or listed with a reason. The
//! EXEMPT list may only shrink (its size is capped).

use std::fs;
use std::path::{Path, PathBuf};

/// Holders reached from `drafts.rs`: (file under `src`, text `drafts.rs` must contain).
const REGISTERED: &[(&str, &str)] = &[
    ("family_editor/mod.rs", "crate::family_editor::draft_names"),
    ("range_ui.rs", "crate::range_ui::drafts"),
    // Line and hatch library entries are read through the range window's `drafts`.
    ("range_libs.rs", "crate::range_ui::drafts"),
    (
        "view_gfx_ui/vv_win.rs",
        "crate::view_gfx_ui::vv_win::drafts",
    ),
    (
        "view_gfx_ui/materials_win.rs",
        "crate::view_gfx_ui::materials_win::draft_name",
    ),
    // Type dialog and parameter editor drafts live in the browser's state.
    ("shell/browser.rs", "crate::shell::panels::browser::drafts"),
    ("data_ui.rs", "crate::data_ui::info_draft"),
    // Staged cells and the definition form of the schedules window.
    ("schedule/multi_ui.rs", "crate::schedule::multi_ui::draft_count"),
];

/// Modules whose drafts Discard and Save all reach (`crate::<module>::{discard,save}_drafts`).
const ACTIONS: &[&str] = &[
    "family_editor",
    "range_ui",
    "view_gfx_ui::vv_win",
    "view_gfx_ui::materials_win",
    "shell::panels::browser",
    "data_ui",
    "schedule::multi_ui",
];

/// Not a project draft; the reason says what ends the state instead.
const EXEMPT: &[(&str, &str)] = &[
    (
        "solid_tools.rs",
        "in-progress sketch of the active tool; reset on tool change, never a stored edit",
    ),
    (
        "quick/tipcard.rs",
        "`dirty` is a repaint flag of the quick card, not edited data",
    ),
    (
        "ai/session.rs",
        "AI proposal and rule drafts live in the chat panel and reach the model only on Apply",
    ),
    (
        "issues_ui.rs",
        "new-issue form of a modal dialog; an issue exists only after Create, Cancel drops the text",
    ),
    (
        "shell/wall_type_editor.rs",
        "wall type editor dialog; the type changes only on its Apply, Cancel drops the draft",
    ),
];

const EXEMPT_CAP: usize = 5;

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Code before the first test module; test fixtures are not holders.
fn production(text: &str) -> String {
    let mut v = String::new();
    for l in text.lines() {
        if l.trim() == "#[cfg(test)]" {
            break;
        }
        v.push_str(l);
        v.push('\n');
    }
    v
}

/// Identifiers outside string literals and `//` comments.
fn idents(line: &str) -> Vec<String> {
    let (mut code, mut quoted, mut esc) = (String::new(), false, false);
    let b: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if quoted {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                quoted = false;
            }
        } else if c == '"' {
            quoted = true;
            code.push(' ');
        } else if c == '/' && b.get(i + 1) == Some(&'/') {
            break;
        } else {
            code.push(c);
        }
        i += 1;
    }
    code.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn is_draft_word(w: &str) -> bool {
    w == "draft" || w == "dirty" || w == "Draft" || (w.len() > 5 && w.ends_with("Draft"))
}

/// Files (relative to `src`, slash separated) holding a thread-local plus a draft word.
fn holders() -> Vec<String> {
    let root = src_root();
    let mut files = Vec::new();
    walk(&root, &mut files);
    let mut out = Vec::new();
    for p in files {
        let Ok(text) = fs::read_to_string(&p) else { continue };
        let code = production(&text);
        if !code.contains("thread_local!") {
            continue;
        }
        // `tool.draft` is the active tool's input, held by the editor, never by a thread-local
        if code.lines().any(|l| idents(&l.replace("tool.draft", "tool.input")).iter().any(|w| is_draft_word(w))) {
            let rel = p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
            out.push(rel);
        }
    }
    out.sort();
    out
}

fn drafts_src() -> String {
    let p = src_root().join("project_ui").join("drafts.rs");
    production(&fs::read_to_string(p).expect("drafts.rs"))
}

fn listed(list: &[(&str, &str)], f: &str) -> bool {
    list.iter().any(|(n, _)| *n == f)
}

#[test]
fn every_draft_holder_is_registered_or_listed() {
    let found = holders();
    assert!(found.len() >= 6, "scan finds the known holders: {found:?}");
    let unknown: Vec<&String> = found
        .iter()
        .filter(|f| !listed(REGISTERED, f) && !listed(EXEMPT, f))
        .collect();
    assert!(
        unknown.is_empty(),
        "new draft holder: register it in project_ui/drafts.rs (and REGISTERED here) or list it \
         in EXEMPT with a reason: {unknown:?}"
    );
}

#[test]
fn lists_hold_no_stale_entries_and_only_shrink() {
    let found = holders();
    for (name, list) in [("REGISTERED", REGISTERED), ("EXEMPT", EXEMPT)] {
        for (f, why) in list {
            assert!(found.iter().any(|x| x == f), "{name}: {f} is no draft holder any more");
            assert!(!why.is_empty(), "{name}: {f} needs a reason");
        }
    }
    let all: Vec<&str> = REGISTERED.iter().chain(EXEMPT).map(|(f, _)| *f).collect();
    for (i, f) in all.iter().enumerate() {
        assert!(!all[i + 1..].contains(f), "{f} is in two lists");
    }
    assert!(EXEMPT.len() <= EXEMPT_CAP, "EXEMPT may only shrink");
}

#[test]
fn registered_holders_are_wired_into_drafts() {
    let src = drafts_src();
    for (f, needle) in REGISTERED {
        assert!(src.contains(needle), "{f}: drafts.rs lacks `{needle}`");
    }
    let range_ui = fs::read_to_string(src_root().join("range_ui.rs")).unwrap();
    assert!(
        range_ui.contains("range_libs::"),
        "range_ui must read the library drafts for the range_libs entry to hold"
    );
}

#[test]
fn registered_holders_are_covered_by_all_three_actions() {
    let src = drafts_src();
    for m in ACTIONS {
        for f in ["discard_drafts", "save_drafts"] {
            let call = format!("crate::{m}::{f}");
            assert!(src.contains(&call), "{call} missing: Discard or Save skips {m}");
        }
    }
}

fn assert_registered(module: &str) {
    assert!(
        drafts_src().contains(module),
        "{module} keeps a draft outside the model but drafts.rs does not list it"
    );
}

#[test]
fn view_graphics_draft_is_registered() {
    assert_registered("vv_win");
}

#[test]
fn material_draft_is_registered() {
    assert_registered("materials_win");
}

#[test]
fn browser_drafts_are_registered() {
    assert_registered("browser");
}

#[test]
fn project_info_draft_is_registered() {
    assert_registered("data_ui");
    assert!(holders().iter().any(|f| f == "data_ui.rs"), "the scan finds the info window");
}
