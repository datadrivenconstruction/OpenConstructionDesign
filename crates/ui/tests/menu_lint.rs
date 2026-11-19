// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Menus are built from kit rows (`menu_item`, `menu_item_icon`, `menu_sub`, `menu_divider`), not
//! from raw egui. Per file this counts raw entry points (`.menu_button(`, `egui::menu::`) and, inside
//! the closure of a `.menu_button(` or `.context_menu(`, raw rows (`.button(`, `.separator()`,
//! `.selectable_label(`, `egui::Button`). The count may not exceed the entry in
//! menu_lint_baseline.txt (an unlisted file has none); entries only go down, a lower count passes
//! and the entry is then lowered by hand. A deliberate raw site carries
//! `// menu-lint: allow <reason>` on its line (a marker without a reason is ignored).
//! Comments and inline test modules are not scanned.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const BASELINE: &str = "tests/menu_lint_baseline.txt";
const MARK: &str = "menu-lint: allow";
/// Raw entry points that count one each.
const ENTRIES: &[&str] = &[".menu_button(", "egui::menu::"];
/// Closures whose bodies are searched for raw rows.
const BODIES: &[&str] = &[".menu_button(", ".context_menu("];
const ROWS: &[&str] = &[".button(", ".separator()", ".selectable_label(", "egui::Button"];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let p = e.expect("dir entry").path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The line without its `//` comment; a `//` inside a string literal stays.
fn strip_comment(l: &str) -> &str {
    let b = l.as_bytes();
    let (mut i, mut in_str) = (0usize, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                i += 1;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c == b'\'' && b.get(i + 2) == Some(&b'\'') {
            i += 2;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            return &l[..i];
        }
        i += 1;
    }
    l
}

/// Offset of the `)` that closes the call whose `(` ends just before `from`.
fn close_paren(text: &str, from: usize) -> Option<usize> {
    let b = text.as_bytes();
    let (mut depth, mut i) = (1usize, from);
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                if b.get(i + 1) == Some(&b'\\') {
                    i += 3;
                    while i < b.len() && b[i] != b'\'' {
                        i += 1;
                    }
                } else if b.get(i + 2) == Some(&b'\'') {
                    i += 2;
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Marker with a non-empty reason.
fn has_mark(line: &str) -> bool {
    line.find(MARK)
        .is_some_and(|p| line[p + MARK.len()..].trim().chars().count() >= 3)
}

/// Raw menu constructions in the production part of `src`, as (1-based line, what).
fn scan(src: &str) -> Vec<(usize, &'static str)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut code = String::new();
    let mut starts = Vec::new();
    let mut allowed = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.starts_with("#[cfg(test)]") {
            let next = lines.get(i + 1).map_or("", |n| n.trim());
            if next.starts_with("mod ") && next.ends_with('{') {
                break;
            }
        }
        starts.push(code.len());
        allowed.push(has_mark(l));
        if !t.starts_with("//") {
            code.push_str(strip_comment(l));
        }
        code.push('\n');
    }
    let line_of = |off: usize| starts.partition_point(|&s| s <= off) - 1;
    let mut hits: BTreeMap<usize, &'static str> = BTreeMap::new();
    for pat in ENTRIES.iter().copied() {
        for (off, _) in code.match_indices(pat) {
            if !allowed[line_of(off)] {
                hits.insert(off, pat);
            }
        }
    }
    for pat in BODIES.iter().copied() {
        for (off, _) in code.match_indices(pat) {
            if allowed[line_of(off)] {
                continue;
            }
            let open = off + pat.len();
            let close = close_paren(&code, open).unwrap_or(code.len());
            let body = &code[open..close];
            for row in ROWS.iter().copied() {
                for (r, _) in body.match_indices(row) {
                    let at = open + r;
                    if !allowed[line_of(at)] {
                        hits.insert(at, row);
                    }
                }
            }
        }
    }
    hits.into_iter().map(|(o, w)| (line_of(o) + 1, w)).collect()
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn is_test_file(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "tests.rs" || n.ends_with("_tests.rs"))
}

/// Raw-construction count per file under app/src and ui/src (path from the repo root with `/`).
fn found() -> BTreeMap<String, Vec<(usize, &'static str)>> {
    let root = crate_dir().join("../..");
    let mut files = Vec::new();
    rs_files(&root.join("crates/app/src"), &mut files);
    rs_files(&root.join("crates/ui/src"), &mut files);
    let mut out = BTreeMap::new();
    for p in files.into_iter().filter(|p| !is_test_file(p)) {
        let name = p
            .strip_prefix(&root)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace('\\', "/");
        let src = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
        let hits = scan(&src);
        if !hits.is_empty() {
            out.insert(name, hits);
        }
    }
    out
}

/// `path count` per line; `#` starts a comment.
fn parse_baseline(text: &str) -> Result<BTreeMap<String, usize>, String> {
    let mut out = BTreeMap::new();
    for (n, l) in text.lines().enumerate() {
        let l = l.split('#').next().unwrap_or("").trim();
        if l.is_empty() {
            continue;
        }
        let mut it = l.split_whitespace();
        let (Some(path), Some(cnt), None) = (it.next(), it.next(), it.next()) else {
            return Err(format!("baseline line {}: want `path count`", n + 1));
        };
        let cnt = cnt
            .parse::<usize>()
            .map_err(|e| format!("baseline line {}: {e}", n + 1))?;
        if out.insert(path.to_string(), cnt).is_some() {
            return Err(format!("baseline line {}: {path} listed twice", n + 1));
        }
    }
    Ok(out)
}

/// Files above their entry. A lower count (or a missing file) passes.
fn over_baseline(
    found: &BTreeMap<String, Vec<(usize, &'static str)>>,
    base: &BTreeMap<String, usize>,
) -> Vec<String> {
    let mut bad = Vec::new();
    for (name, hits) in found {
        let allowed = base.get(name).copied().unwrap_or(0);
        if hits.len() > allowed {
            let at: Vec<String> = hits.iter().map(|(l, w)| format!("{l}:{w}")).collect();
            bad.push(format!(
                "{name}: {} raw menu constructions, baseline {allowed} ({})",
                hits.len(),
                at.join(" ")
            ));
        }
    }
    bad
}

fn baseline() -> BTreeMap<String, usize> {
    let path = crate_dir().join(BASELINE);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    parse_baseline(&text).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn menus_stay_within_the_per_file_baseline() {
    let bad = over_baseline(&found(), &baseline());
    assert!(
        bad.is_empty(),
        "build menus from kit rows (menu_item_icon / menu_sub / menu_divider), or mark a deliberate \
         raw site with `// menu-lint: allow <reason>`:\n{}",
        bad.join("\n")
    );
}

#[test]
fn baseline_loads() {
    let base = baseline();
    assert!(!base.is_empty());
    for (name, n) in &base {
        assert!(name.starts_with("crates/") && name.ends_with(".rs"), "{name}");
        assert!(*n > 0, "{name}: drop entries that reached 0");
    }
    assert!(parse_baseline("a.rs x").is_err());
    assert!(parse_baseline("a.rs 1\na.rs 2").is_err());
    assert_eq!(parse_baseline("# c\na.rs 2 # tail\n").unwrap()["a.rs"], 2);
}

#[test]
fn fixture_violation_is_detected() {
    let src = r#"
fn header(ui: &mut Ui) {
    ui.menu_button("File", |ui| {
        if ui.button("Open").clicked() {}
        ui.separator();
    });
}
fn ctx(r: Response) {
    r.context_menu(|ui| {
        if ui.add(egui::Button::new("Del")).clicked() {}
        ui.selectable_label(false, "x");
    });
}
"#;
    let hits = scan(src);
    assert_eq!(hits.len(), 1 + 2 + 2, "{hits:?}");
    assert!(hits.contains(&(3, ".menu_button(")));
    assert!(hits.contains(&(4, ".button(")));
    assert!(hits.contains(&(5, ".separator()")));
    assert!(hits.contains(&(10, "egui::Button")));
    assert!(hits.contains(&(11, ".selectable_label(")));
    assert_eq!(scan("let _ = egui::menu::bar(ui, |ui| {});").len(), 1);
}

#[test]
fn kit_rows_are_clean() {
    let src = r#"
r.context_menu(|ui| {
    if menu_item_icon(ui, th, None, ")(", "", MenuTone::Normal, true).clicked() {
        ui.close();
    }
    menu_divider(ui, th);
    let s = menu_sub(ui, th, None, "Sub", true, |ui| {
        menu_item(ui, th, "x", "", MenuTone::Normal, true);
    });
});
"#;
    assert!(scan(src).is_empty(), "{:?}", scan(src));
}

#[test]
fn opt_out_is_honored_with_a_reason() {
    let site = "ui.menu_button(\"A\", |ui| { ui.button(\"b\"); }); // menu-lint: allow native menu bar";
    assert!(scan(site).is_empty());
    let row = "r.context_menu(|ui| {\n    ui.separator(); // menu-lint: allow thin rule\n    ui.button(\"x\");\n});";
    assert_eq!(scan(row), vec![(3, ".button(")]);
    let no_reason = "ui.menu_button(\"A\", |ui| {}); // menu-lint: allow";
    assert_eq!(scan(no_reason).len(), 1);
}

#[test]
fn decrease_passes_and_increase_fails() {
    let mut f = BTreeMap::new();
    f.insert("a.rs".to_string(), vec![(1, ".button("), (2, ".separator()")]);
    let mut b = BTreeMap::new();
    b.insert("a.rs".to_string(), 5);
    assert!(over_baseline(&f, &b).is_empty());
    b.insert("a.rs".to_string(), 2);
    assert!(over_baseline(&f, &b).is_empty());
    b.insert("a.rs".to_string(), 1);
    assert_eq!(over_baseline(&f, &b).len(), 1);
    assert_eq!(over_baseline(&f, &BTreeMap::new()).len(), 1);
    assert!(over_baseline(&BTreeMap::new(), &b).is_empty());
}

#[test]
fn comments_and_tests_are_not_scanned() {
    let src = r#"
// ui.menu_button("x", |ui| { ui.button("y"); });
/// r.context_menu(|ui| { ui.separator(); });
fn f() {
    let url = "http://x"; // ui.menu_button("a", |ui| {});
}
#[cfg(test)]
mod tests {
    fn t(ui: &mut Ui) {
        ui.menu_button("x", |ui| { ui.button("y"); });
    }
}
"#;
    assert!(scan(src).is_empty(), "{:?}", scan(src));
}
