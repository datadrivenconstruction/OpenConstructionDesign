// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Number-parsing lint: no new blind comma-to-point swap and no new bare float parse on user
//! or stored text. Stored text goes through `ddc_model::parse_invariant` / `parse_stored`,
//! user input through `ddc_ui::numparse` with an explicit UI mark.
//!
//! Existing sites are frozen per file in `num_lint_baseline.txt` (`<path> <count>`). A count
//! may fall (fix a site, shrink the file entry) but never rise; a file missing from the
//! baseline has an implicit count of 0. A deliberate site opts out with `// num-lint: <reason>`
//! on the same line or the line above (a reason is required).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const HELPERS: &str = "use ddc_ui::numparse (user input) or ddc_model::parse_stored / \
parse_invariant (stored text), or mark the line `// num-lint: <reason>`";

/// Files that implement the approved parsers themselves.
const EXEMPT_FILES: &[&str] = &[
    "crates/ui/src/numparse.rs",
    "crates/model/src/num_text.rs",
    "crates/app/src/units.rs",
    "crates/io/src/xlsx/units.rs",
];

#[derive(Debug, PartialEq)]
struct Hit {
    line: usize,
    what: &'static str,
    /// Opt-out marker present but without a reason.
    empty_reason: bool,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Code part of a line: text before a `//` comment that is outside a string literal.
fn split_comment(line: &str) -> (&str, &str) {
    let b = line.as_bytes();
    let (mut in_str, mut i) = (false, 0);
    while i < b.len() {
        match b[i] {
            b'\\' if in_str => i += 1,
            b'"' => in_str = !in_str,
            b'/' if !in_str && b.get(i + 1) == Some(&b'/') => return (&line[..i], &line[i..]),
            _ => {}
        }
        i += 1;
    }
    (line, "")
}

/// Reason of a `num-lint:` marker in a comment: None = no marker, Some(false) = empty reason.
fn marker(comment: &str) -> Option<bool> {
    let at = comment.find("num-lint:")?;
    Some(!comment[at + "num-lint:".len()..].trim().is_empty())
}

fn what_matches(code: &str) -> Option<&'static str> {
    let swap = ["replace(',', \".\")", "replace(',', '.')", "replace(\",\", \".\")"];
    if swap.iter().any(|p| code.contains(p)) {
        return Some("comma-to-point swap");
    }
    if code.contains("parse::<f64>()") || code.contains("parse::<f32>()") {
        return Some("bare float parse");
    }
    if code.contains("f64::from_str(") || code.contains("f32::from_str(") {
        return Some("bare float from_str");
    }
    None
}

/// A `#[cfg(test)]` attribute starts the test region only when an inline `mod x {` follows.
fn starts_test_mod(lines: &[&str], i: usize) -> bool {
    if lines[i].trim() != "#[cfg(test)]" {
        return false;
    }
    lines[i + 1..]
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("#["))
        .is_some_and(|l| l.contains("mod ") && l.ends_with('{'))
}

fn scan_text(text: &str) -> Vec<Hit> {
    let lines: Vec<&str> = text.lines().collect();
    let mut hits = Vec::new();
    let mut prev_marker: Option<bool> = None;
    for (i, raw) in lines.iter().enumerate() {
        if starts_test_mod(&lines, i) {
            break;
        }
        let t = raw.trim_start();
        let (code, comment) = split_comment(raw);
        let own = marker(comment);
        if t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') {
            // A comment-only line can carry the opt-out for the next line.
            prev_marker = marker(t);
            continue;
        }
        if let Some(what) = what_matches(code) {
            let m = own.or(prev_marker);
            if m != Some(true) {
                hits.push(Hit { line: i + 1, what, empty_reason: m == Some(false) });
            }
        }
        prev_marker = None;
    }
    hits
}

fn is_test_path(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    rel.contains("/tests/")
        || name == "tests.rs"
        || name.starts_with("tests_")
        || name.ends_with("_tests.rs")
        || name.ends_with("_test.rs")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn scan_repo() -> BTreeMap<String, Vec<Hit>> {
    let root = repo_root();
    let mut files = Vec::new();
    for c in fs::read_dir(root.join("crates")).expect("crates dir").flatten() {
        collect(&c.path().join("src"), &mut files);
    }
    let mut out = BTreeMap::new();
    for f in files {
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        let rel = rel.trim_start_matches("./").to_string();
        if EXEMPT_FILES.contains(&rel.as_str()) || is_test_path(&rel) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&f) else { continue };
        let hits = scan_text(&text);
        if !hits.is_empty() {
            out.insert(rel, hits);
        }
    }
    out
}

fn parse_baseline(text: &str) -> Result<BTreeMap<String, usize>, String> {
    let mut m = BTreeMap::new();
    for (n, l) in text.lines().enumerate() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let (path, cnt) = l.rsplit_once(' ').ok_or(format!("baseline line {}: no count", n + 1))?;
        let cnt: usize = cnt.parse().map_err(|_| format!("baseline line {}: bad count", n + 1))?;
        if m.insert(path.trim().to_string(), cnt).is_some() {
            return Err(format!("baseline line {}: duplicate path", n + 1));
        }
    }
    Ok(m)
}

/// Errors: a marker without a reason, or a file above its baseline (names file:line).
fn check(found: &BTreeMap<String, Vec<Hit>>, base: &BTreeMap<String, usize>) -> Vec<String> {
    let mut errs = Vec::new();
    for (file, hits) in found {
        for h in hits.iter().filter(|h| h.empty_reason) {
            errs.push(format!("{file}:{}: `num-lint:` marker needs a reason", h.line));
        }
        let allowed = base.get(file).copied().unwrap_or(0);
        if hits.len() > allowed {
            let lines: Vec<String> = hits.iter().map(|h| format!("{file}:{} ({})", h.line, h.what)).collect();
            errs.push(format!(
                "{file}: {} sites, baseline {allowed}; new or moved: {}. {HELPERS}",
                hits.len(),
                lines.join(", ")
            ));
        }
    }
    errs
}

fn load_baseline() -> BTreeMap<String, usize> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("num_lint_baseline.txt");
    parse_baseline(&fs::read_to_string(p).expect("num_lint_baseline.txt")).expect("baseline parses")
}

#[test]
fn baseline_loads_and_points_at_files() {
    let base = load_baseline();
    assert!(!base.is_empty());
    for (path, n) in &base {
        assert!(*n > 0, "{path}: remove zero entries");
        assert!(repo_root().join(path).is_file(), "{path}: baseline file is gone, drop the entry");
    }
}

#[test]
fn no_new_blind_number_parsing() {
    let found = scan_repo();
    let base = load_baseline();
    let errs = check(&found, &base);
    assert!(errs.is_empty(), "number-parsing lint:\n{}", errs.join("\n"));
    let below: Vec<&String> = base
        .iter()
        .filter(|(f, n)| found.get(*f).map_or(0, |h| h.len()) < **n)
        .map(|(f, _)| f)
        .collect();
    if !below.is_empty() {
        eprintln!("num_lint: baseline can shrink for {below:?}");
    }
}

/// Prints the current baseline: `cargo test -p ddc-app --test num_lint print_baseline -- --ignored --nocapture`.
#[test]
#[ignore]
fn print_baseline() {
    for (f, h) in scan_repo() {
        println!("{f} {}", h.len());
    }
}

#[test]
fn fixture_violations_are_caught() {
    let swap = format!("let n = t.replace(',', {q}.{q});", q = '"');
    let hits = scan_text(&swap);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].line, 1);
    assert_eq!(hits[0].what, "comma-to-point swap");
    let hits = scan_text("fn f(t: &str) {\n    let v = t.trim().parse::<f64>();\n}");
    assert_eq!((hits.len(), hits[0].line, hits[0].what), (1, 2, "bare float parse"));
    assert_eq!(scan_text("let a = x.replace(\",\", \".\");").len(), 1);
    assert_eq!(scan_text("let a = f64::from_str(t);").len(), 1);
}

#[test]
fn fixture_message_names_file_line_and_helper() {
    let found = BTreeMap::from([("crates/x/src/a.rs".to_string(), scan_text("\n\nlet v = t.parse::<f64>();"))]);
    let errs = check(&found, &BTreeMap::new());
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("crates/x/src/a.rs:3"), "{}", errs[0]);
    assert!(errs[0].contains("numparse") && errs[0].contains("parse_stored"), "{}", errs[0]);
}

#[test]
fn opt_out_with_reason_is_honored() {
    assert!(scan_text("let v = t.parse::<f64>(); // num-lint: protocol field, always point").is_empty());
    assert!(scan_text("// num-lint: sensor feed is invariant\nlet v = t.parse::<f64>();").is_empty());
    // The marker covers only the next line.
    assert_eq!(scan_text("// num-lint: reason\nlet a = 1;\nlet v = t.parse::<f64>();").len(), 1);
    // An empty reason does not opt out and is reported.
    let hits = scan_text("let v = t.parse::<f64>(); // num-lint:");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].empty_reason);
    let found = BTreeMap::from([("crates/x/src/a.rs".to_string(), hits)]);
    assert!(check(&found, &BTreeMap::from([("crates/x/src/a.rs".to_string(), 1)]))
        .iter()
        .any(|e| e.contains("needs a reason")));
}

#[test]
fn decrease_passes_and_increase_fails() {
    let two = scan_text("let a = x.parse::<f64>();\nlet b = y.parse::<f64>();");
    let one = scan_text("let a = x.parse::<f64>();");
    let f = "crates/x/src/a.rs".to_string();
    let base = BTreeMap::from([(f.clone(), 2)]);
    assert!(check(&BTreeMap::from([(f.clone(), two)]), &base).is_empty());
    assert!(check(&BTreeMap::from([(f.clone(), one)]), &base).is_empty());
    assert!(check(&BTreeMap::new(), &base).is_empty());
    let three = scan_text("a.parse::<f64>();\nb.parse::<f64>();\nc.parse::<f32>();");
    assert_eq!(check(&BTreeMap::from([(f, three)]), &base).len(), 1);
}

#[test]
fn comments_strings_and_tests_are_excluded() {
    assert!(scan_text("// t.parse::<f64>() is wrong here\n/// x.replace(',', \".\")").is_empty());
    assert!(scan_text("let s = \"call parse::<f64>() later\";").len() == 1, "string content is not stripped");
    assert!(scan_text("let a = 1; // t.parse::<f64>()").is_empty());
    assert!(scan_text("let u = \"http://x\"; // note").is_empty());
    let src = "fn real() {}\n#[cfg(test)]\nmod tests {\n    fn t() { \"1\".parse::<f64>(); }\n}";
    assert!(scan_text(src).is_empty());
    // `#[cfg(test)] use ...;` does not end the scan.
    assert_eq!(scan_text("#[cfg(test)]\nuse x::y;\nlet v = t.parse::<f64>();").len(), 1);
    assert!(is_test_path("crates/app/src/foo_tests.rs"));
    assert!(is_test_path("crates/io/src/xlsx/tests_loss.rs"));
    assert!(is_test_path("crates/app/tests/e2e.rs"));
    assert!(!is_test_path("crates/app/src/query.rs"));
}
