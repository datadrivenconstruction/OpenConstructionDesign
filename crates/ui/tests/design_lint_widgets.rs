//! Feature code uses kit widgets, not raw egui ones. Raw widget calls are counted per file against
//! design_lint_widgets_baseline.txt: a file may not exceed its entry, an unlisted file has none.
//! Entries only go down; `DESIGN_LINT_BLESS=1` rewrites the file after a merge train. A raw call
//! that is justified carries `// design-lint: widget` on its line. Kit internals, comments and
//! test code are exempt.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Raw egui widget calls and their kit replacement.
const RAW_WIDGETS: &[(&str, &str)] = &[
    ("ComboBox::", "ddc_ui::widgets::dropdown / dropdown_h"),
    ("DragValue::", "ddc_ui::widgets::NumberField"),
    (".button(", "ddc_ui::widgets::Button::{primary,secondary,quiet}(..).show"),
    (".small_button(", "ddc_ui::widgets::Button::quiet(..).small().show"),
    (".selectable_label(", "ddc_ui::widgets::segmented or a kit menu_item"),
    (".selectable_value(", "ddc_ui::widgets::segmented or dropdown"),
    (".checkbox(", "ddc_ui::widgets::checkbox"),
    (".radio(", "ddc_ui::widgets::segmented"),
    (".radio_value(", "ddc_ui::widgets::segmented"),
];
const MARK: &str = "design-lint: widget";
const BASELINE: &str = "tests/design_lint_widgets_baseline.txt";

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

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

/// Code lines up to the inline test module, trailing comments cut; marked lines are dropped.
fn production(src: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.starts_with("#[cfg(test)]") {
            let next = lines.get(i + 1).map_or("", |n| n.trim());
            if next.starts_with("mod ") && next.ends_with('{') {
                break;
            }
        }
        if !t.starts_with("//") && !l.contains(MARK) {
            out.push((i + 1, l.split("//").next().unwrap_or("")));
        }
    }
    out
}

/// Raw widget hits as (line, pattern index into `RAW_WIDGETS`).
fn scan(src: &str) -> Vec<(usize, usize)> {
    let mut hits = Vec::new();
    for (no, code) in production(src) {
        for (k, (pat, _)) in RAW_WIDGETS.iter().enumerate() {
            for _ in code.matches(pat) {
                hits.push((no, k));
            }
        }
    }
    hits
}

/// UI code under the ratchet, as (path from the repo root with `/`, source). The kit itself
/// (`ui/src/widgets`, `app/src/ui_kit*`) and test files are not listed.
fn sources() -> Vec<(String, String)> {
    let root = crate_dir().join("../..");
    let mut files = vec![root.join("crates/ui/src/shell.rs")];
    rs_files(&root.join("crates/app/src"), &mut files);
    files
        .into_iter()
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n != "tests.rs" && !n.ends_with("_tests.rs")
        })
        .map(|p| {
            let name = p
                .strip_prefix(&root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            (name, p)
        })
        .filter(|(n, _)| n != "crates/app/src/ui_kit.rs" && !n.starts_with("crates/app/src/ui_kit/"))
        .map(|(name, p)| {
            let src = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
            (name, src)
        })
        .collect()
}

/// `path count` per line; `#` starts a comment.
fn parse_baseline(text: &str) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for l in text.lines() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.split_whitespace().collect();
        assert!(f.len() == 2, "bad baseline line: {l}");
        let n = f[1]
            .parse::<usize>()
            .unwrap_or_else(|_| panic!("bad baseline line: {l}"));
        out.insert(f[0].to_string(), n);
    }
    out
}

/// One message per file above its allowance, naming the file, the lines and the kit replacement.
fn violations(
    found: &[(String, Vec<(usize, usize)>)],
    base: &BTreeMap<String, usize>,
) -> Vec<String> {
    let mut over = Vec::new();
    for (name, hits) in found {
        let allowed = base.get(name).copied().unwrap_or(0);
        if hits.len() > allowed {
            let mut uses: Vec<String> = hits
                .iter()
                .map(|(no, k)| {
                    let (pat, kit) = RAW_WIDGETS[*k];
                    format!("line {no}: `{pat}` -> {kit}")
                })
                .collect();
            uses.dedup();
            over.push(format!(
                "{name}: {} raw widgets, allowed {allowed}\n    {}",
                hits.len(),
                uses.join("\n    ")
            ));
        }
    }
    over
}

fn bless(found: &[(String, Vec<(usize, usize)>)]) {
    let mut text = String::from(
        "# Raw egui widgets per file: path count. Only goes down; see design_lint_widgets.rs.\n",
    );
    let mut rows: Vec<_> = found.iter().filter(|(_, h)| !h.is_empty()).collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, h) in rows {
        text.push_str(&format!("{name} {}\n", h.len()));
    }
    std::fs::write(crate_dir().join(BASELINE), text).expect("write the baseline");
}

#[test]
fn raw_widgets_not_above_baseline() {
    let found: Vec<(String, Vec<(usize, usize)>)> = sources()
        .into_iter()
        .map(|(name, src)| {
            let h = scan(&src);
            (name, h)
        })
        .collect();
    assert!(found.len() > 100, "source list is too short: {}", found.len());
    if std::env::var_os("DESIGN_LINT_BLESS").is_some() {
        bless(&found);
        return;
    }
    let path = crate_dir().join(BASELINE);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let over = violations(&found, &parse_baseline(&text));
    assert!(
        over.is_empty(),
        "raw egui widgets outside the kit; use the kit widget named on each line (or mark a \
         justified call `// {MARK}`):\n{}",
        over.join("\n")
    );
}

#[test]
fn baseline_loads_and_names_real_files() {
    let path = crate_dir().join(BASELINE);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let base = parse_baseline(&text);
    assert!(!base.is_empty());
    let root = crate_dir().join("../..");
    for (name, n) in &base {
        assert!(*n > 0, "{name}: zero entries are removed");
        assert!(root.join(name).exists(), "baseline names a missing file: {name}");
    }
    let b = parse_baseline("# note\n\ncrates/app/src/a.rs 2\n");
    assert_eq!(b.get("crates/app/src/a.rs"), Some(&2));
    assert_eq!(b.len(), 1);
}

fn run(src: &str, base_n: usize) -> Vec<String> {
    let found = vec![("crates/app/src/x.rs".to_string(), scan(src))];
    let mut base = BTreeMap::new();
    base.insert("crates/app/src/x.rs".to_string(), base_n);
    violations(&found, &base)
}

#[test]
fn new_violation_is_detected_with_file_and_kit_name() {
    let src = "fn f(ui: &mut Ui) {\n    egui::ComboBox::from_id_salt(\"a\");\n    ui.button(\"ok\");\n}\n";
    assert_eq!(scan(src), vec![(2, 0), (3, 2)]);
    let over = run(src, 1);
    assert_eq!(over.len(), 1);
    assert!(over[0].contains("crates/app/src/x.rs"), "{}", over[0]);
    assert!(over[0].contains("2 raw widgets, allowed 1"), "{}", over[0]);
    assert!(over[0].contains("dropdown"), "{}", over[0]);
    assert!(over[0].contains("Button"), "{}", over[0]);
    // an unlisted file has no allowance
    let found = vec![("crates/app/src/new.rs".to_string(), scan(src))];
    assert_eq!(violations(&found, &BTreeMap::new()).len(), 1);
}

#[test]
fn chained_calls_and_other_widgets_are_seen() {
    let src = "ui\n    .button(\"a\");\nui.checkbox(&mut b, \"c\");\negui::DragValue::new(&mut v);\n\
               ui.radio_value(&mut m, 1, \"x\");\nui.small_button(\"s\");\n";
    assert_eq!(scan(src).len(), 5);
    // kit calls are not raw: free fn checkbox, Button::show, menu_button
    let kit = "checkbox(ui, th, &mut b, \"c\");\nButton::primary(\"ok\").show(ui, th);\nui.menu_button(\"m\", f);\n";
    assert!(scan(kit).is_empty());
}

#[test]
fn decreased_count_passes() {
    let src = "ui.button(\"a\");\n";
    assert!(run(src, 3).is_empty());
    assert!(run(src, 1).is_empty());
    assert!(run("let a = 1;\n", 2).is_empty());
}

#[test]
fn comments_tests_and_marked_lines_are_excluded() {
    let src = "// ui.button(\"a\");\nlet a = 1; // ui.button(\"b\")\n\
               ui.button(\"ok\"); // design-lint: widget\n\
               #[cfg(test)]\nmod tests {\n    ui.button(\"t\");\n}\n";
    assert!(scan(src).is_empty());
    let decl = "#[cfg(test)]\nmod kit_tests;\nui.button(\"a\");\n";
    assert_eq!(scan(decl).len(), 1);
}
