// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Feature code takes colours and type sizes from tokens (`ddc_ui::theme`). Literals are counted
//! per file against design_lint_baseline.txt: a file may not exceed its entry, an unlisted file
//! has none. `DESIGN_LINT_BLESS=1` rewrites the baseline, once after a merge train; from then on
//! entries only go down. A real data or debug colour carries `// design-lint: data` on its line.
use ddc_ui::theme::Tokens;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const COLOR_LITERALS: &[&str] = &[
    "Color32::from_rgb(",
    "Color32::from_rgba_unmultiplied(",
    "Color32::from_rgba_premultiplied(",
    "Color32::from_gray(",
];
const FONT_LITERALS: &[&str] = &[
    "FontId::proportional(",
    "FontId::monospace(",
    "FontId::new(",
    ".size(",
];
/// Spacing takes `ddc_ui::sem::space::*` (4 px grid), not a number.
const SPACE_LITERALS: &[&str] = &[
    "add_space(",
    "Margin::same(",
    "Margin::symmetric(",
    "item_spacing.x = ",
    "item_spacing.y = ",
    "item_spacing = vec2(",
    "item_spacing = egui::vec2(",
    "item_spacing = Vec2::new(",
    "item_spacing = egui::Vec2::new(",
];
const DATA_MARK: &str = "design-lint: data";
const BASELINE: &str = "tests/design_lint_baseline.txt";

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

/// Code lines up to the inline test module, with 1-based numbers and trailing comments cut.
/// Lines marked as data are dropped.
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
        if !t.starts_with("//") && !l.contains(DATA_MARK) {
            out.push((i + 1, l.split("//").next().unwrap_or("")));
        }
    }
    out
}

/// Occurrences of a constructor whose first argument is a number; rustfmt may put it on `next`.
fn literal_calls(code: &str, next: &str, pat: &str) -> usize {
    code.match_indices(pat)
        .filter(|(i, _)| {
            let rest = code[i + pat.len()..].trim_start();
            let first = if rest.is_empty() {
                next.trim_start()
            } else {
                rest
            };
            first.chars().next().is_some_and(|c| c.is_ascii_digit())
        })
        .count()
}

/// `Color32::WHITE` and friends. `TRANSPARENT` draws nothing, `PLACEHOLDER` is egui's own marker.
fn named_colors(code: &str) -> usize {
    let pat = "Color32::";
    code.match_indices(pat)
        .filter(|(i, _)| {
            let name: String = code[i + pat.len()..]
                .chars()
                .take_while(|c| c.is_ascii_uppercase() || *c == '_')
                .collect();
            !name.is_empty() && name != "TRANSPARENT" && name != "PLACEHOLDER"
        })
        .count()
}

#[derive(Default)]
struct Found {
    color: Vec<usize>,
    font: Vec<usize>,
    space: Vec<usize>,
}

fn scan(src: &str) -> Found {
    let lines = production(src);
    let mut found = Found::default();
    for (k, (no, code)) in lines.iter().enumerate() {
        let next = lines.get(k + 1).map_or("", |n| n.1);
        let colors = COLOR_LITERALS
            .iter()
            .map(|p| literal_calls(code, next, p))
            .sum::<usize>()
            + named_colors(code);
        let fonts: usize = FONT_LITERALS
            .iter()
            .map(|p| literal_calls(code, next, p))
            .sum();
        found.color.extend(std::iter::repeat(*no).take(colors));
        found.font.extend(std::iter::repeat(*no).take(fonts));
        let spaces: usize = SPACE_LITERALS
            .iter()
            .map(|p| literal_calls(code, next, p))
            .sum();
        found.space.extend(std::iter::repeat(*no).take(spaces));
    }
    found
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn root() -> PathBuf {
    crate_dir().join("../..")
}

fn is_test_file(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "tests.rs" || n.ends_with("_tests.rs"))
}

/// UI code under the ratchet, as (path from the repo root with `/`, source).
fn ui_sources() -> Vec<(String, String)> {
    let root = root();
    let mut files = vec![root.join("crates/ui/src/shell.rs")];
    rs_files(&root.join("crates/ui/src/widgets"), &mut files);
    rs_files(&root.join("crates/app/src"), &mut files);
    files
        .into_iter()
        .filter(|p| !is_test_file(p))
        .map(|p| {
            let name = p
                .strip_prefix(&root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            let src = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
            (name, src)
        })
        .collect()
}

/// `path color font [space]` per line (space defaults to 0); `#` starts a comment.
fn parse_baseline(text: &str) -> BTreeMap<String, (usize, usize, usize)> {
    let mut out = BTreeMap::new();
    for l in text.lines() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.split_whitespace().collect();
        let num = |s: &str| {
            s.parse::<usize>()
                .unwrap_or_else(|_| panic!("bad baseline line: {l}"))
        };
        assert!(f.len() == 3 || f.len() == 4, "bad baseline line: {l}");
        let space = f.get(3).map_or(0, |s| num(s));
        out.insert(f[0].to_string(), (num(f[1]), num(f[2]), space));
    }
    out
}

fn bless(found: &[(String, Found)]) {
    let mut text = String::from(
        "# Literal colours, fonts and spacing per file: path color font space. Only goes down; see design_lint.rs.\n",
    );
    let mut rows: Vec<_> = found
        .iter()
        .filter(|(_, f)| !f.color.is_empty() || !f.font.is_empty() || !f.space.is_empty())
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, f) in rows {
        text.push_str(&format!(
            "{name} {} {} {}\n",
            f.color.len(),
            f.font.len(),
            f.space.len()
        ));
    }
    std::fs::write(crate_dir().join(BASELINE), text).expect("write the baseline");
}

#[test]
fn literals_stay_within_the_per_file_baseline() {
    let found: Vec<(String, Found)> = ui_sources()
        .into_iter()
        .map(|(name, src)| {
            let f = scan(&src);
            (name, f)
        })
        .collect();
    if std::env::var_os("DESIGN_LINT_BLESS").is_some() {
        bless(&found);
        return;
    }
    let path = crate_dir().join(BASELINE);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let base = parse_baseline(&text);
    let mut over = Vec::new();
    for (name, f) in &found {
        let (color, font, space) = base.get(name).copied().unwrap_or((0, 0, 0));
        if f.space.len() > space {
            over.push(format!(
                "{name}: {} spacing literals, allowed {space}; use ddc_ui::sem::space::*; lines {:?}",
                f.space.len(),
                f.space
            ));
        }
        if f.color.len() > color {
            over.push(format!(
                "{name}: {} colours, allowed {color}; lines {:?}",
                f.color.len(),
                f.color
            ));
        }
        if f.font.len() > font {
            over.push(format!(
                "{name}: {} font sizes, allowed {font}; lines {:?}",
                f.font.len(),
                f.font
            ));
        }
    }
    assert!(
        over.is_empty(),
        "use ddc_ui::theme tokens, new colours go to tokens.json, data colours carry \
         `// {DATA_MARK}`:\n{}",
        over.join("\n")
    );
}

/// Shell files: the ui crate (outside the token tables) and the app chrome, panels and dialogs.
fn shell_sources() -> Vec<(String, String)> {
    let root = root();
    let mut files = Vec::new();
    rs_files(&root.join("crates/ui/src"), &mut files);
    rs_files(&root.join("crates/app/src/shell"), &mut files);
    rs_files(&root.join("crates/app/src/ui_kit"), &mut files);
    for f in ["shell.rs", "ui_kit.rs", "status.rs"] {
        files.push(root.join("crates/app/src").join(f));
    }
    const TABLES: &[&str] = &["theme.rs", "typo.rs", "sem.rs"];
    files
        .into_iter()
        .filter(|p| p.exists() && !is_test_file(p))
        .filter(|p| !p.components().any(|c| c.as_os_str() == "bin"))
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            !(p.starts_with(root.join("crates/ui/src")) && TABLES.contains(&n))
        })
        .map(|p| {
            let name = p
                .strip_prefix(&root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            let src = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
            (name, src)
        })
        .collect()
}

/// Shell text takes a type role (`th.font(Role::..)`, `th.rich`, `typo::font`): no font
/// constructors, raw scale constants or weight families, literal or not.
fn font_bypasses(code: &str) -> usize {
    const RAW: &[&str] = &[
        "FontId::new(",
        "FontId::proportional(",
        "FontId::monospace(",
        "type_size::",
        "family(W_",
    ];
    RAW.iter().map(|p| code.matches(p).count()).sum::<usize>()
        + literal_calls(code, "", ".size(")
}

#[test]
fn shell_text_uses_type_roles_only() {
    let sources = shell_sources();
    assert!(sources.len() > 40, "shell file list is too short: {}", sources.len());
    let mut hits = Vec::new();
    for (name, src) in &sources {
        for (no, code) in production(src) {
            if font_bypasses(code) > 0 {
                hits.push(format!("{name}:{no}: {}", code.trim()));
            }
        }
    }
    let msg = hits.join("\n");
    assert!(hits.is_empty(), "use a type role (docs/design/TYPOGRAPHY.md):\n{msg}");
    assert_eq!(font_bypasses("FontId::new(type_size::CAPTION, family(W_MEDIUM))"), 3);
    assert_eq!(font_bypasses("th.font(Role::RibbonLabel)"), 0);
    assert_eq!(font_bypasses("text.size(12.0)"), 1);
}

/// End of the argument list opened just before `from`, skipping string literals.
fn close_paren(text: &str, from: usize) -> Option<usize> {
    let (mut depth, mut in_str, mut esc) = (1usize, false, false);
    for (i, c) in text[from..].char_indices() {
        if in_str {
            match c {
                _ if esc => esc = false,
                '\\' => esc = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(from + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Names bound to an on-colour, as in `let w = t.sem().snap_text_on;`. A call on the right is not
/// a colour of that name: `let x = f(Color32::WHITE);`.
fn on_colour_names(lines: &[(usize, &str)]) -> Vec<String> {
    let mut names = Vec::new();
    for (_, code) in lines {
        let Some(rest) = code.trim_start().strip_prefix("let ") else {
            continue;
        };
        let rest = rest.strip_prefix("mut ").unwrap_or(rest);
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let value = rest.split_once('=').map_or("", |(_, v)| v);
        if !name.is_empty() && is_on_colour(value) && !value.replace("()", "").contains('(') {
            names.push(name);
        }
    }
    names
}

/// Colours made for a fill of their own: on-colour tokens, the sheet colour, fixed white and black.
fn is_on_colour(code: &str) -> bool {
    code.contains("_text_on")
        || code.contains(".text_on\"")
        || code.contains(".paper")
        || code.contains("Color32::WHITE")
        || code.contains("Color32::BLACK")
}

/// Calls where a `*.text_on` colour lands on a button painted with the theme fill. On-colours are
/// made for their accent fill: on `bg.panel` they drop to about 1:1, and no pair list shows it.
fn on_colour_misuse(src: &str) -> Vec<String> {
    const THEMED: &[&str] = &[
        ".button(",
        ".small_button(",
        ".selectable_label(",
        "Button::new(",
    ];
    let lines = production(src);
    let names = on_colour_names(&lines);
    let text: String = lines.iter().map(|l| l.1).collect::<Vec<_>>().join(" ");
    let mut hits = Vec::new();
    for &pat in THEMED {
        for (i, _) in text.match_indices(pat) {
            let from = i + pat.len();
            let Some(end) = close_paren(&text, from) else {
                continue;
            };
            // `Button::new(..).fill(..)` brings its own fill
            let after: String = text[end..].chars().take(120).collect();
            let chain = after.split(';').next().unwrap_or("");
            if pat == "Button::new(" && chain.contains(".fill(") {
                continue;
            }
            let arg = &text[from..end];
            for (j, _) in arg.match_indices(".color(") {
                let inner = &arg[j + ".color(".len()..];
                let colour = inner.split(')').next().unwrap_or("").trim();
                if is_on_colour(colour) || names.iter().any(|n| n == colour) {
                    hits.push(format!(
                        "{pat}{}",
                        arg.split_whitespace().collect::<Vec<_>>().join(" ")
                    ));
                }
            }
        }
    }
    hits
}

#[test]
fn on_colours_stay_on_their_fill() {
    let mut hits = Vec::new();
    for (name, src) in ui_sources() {
        for h in on_colour_misuse(&src) {
            hits.push(format!("{name}: {h}"));
        }
    }
    assert!(
        hits.is_empty(),
        "an on-colour on a themed button is unreadable in one theme, leave the button text default:\n{}",
        hits.join("\n")
    );
}

/// `th.c("typo")` is magenta at run time only.
#[test]
fn token_names_in_code_exist() {
    let tokens = Tokens::embedded().expect("embedded tokens parse");
    let pat = ".c(\"";
    let mut missing = Vec::new();
    let theme = std::fs::read_to_string(crate_dir().join("src/theme.rs")).expect("theme.rs");
    let mut sources = ui_sources();
    sources.push(("crates/ui/src/theme.rs".to_string(), theme));
    for (name, src) in &sources {
        for (no, code) in production(src) {
            for (i, _) in code.match_indices(pat) {
                let token = code[i + pat.len()..].split('"').next().unwrap_or("");
                if !tokens.color.light.contains_key(token) {
                    missing.push(format!("{name}:{no}: {token}"));
                }
            }
        }
    }
    assert!(missing.is_empty(), "no such token:\n{}", missing.join("\n"));
}

#[test]
fn lint_sees_what_it_should() {
    let pat = "Color32::from_rgb(";
    assert_eq!(literal_calls("Color32::from_rgb(1, 2, 3)", "", pat), 1);
    assert_eq!(literal_calls("Color32::from_rgb(0xFF, 0, 0)", "", pat), 1);
    assert_eq!(literal_calls("Color32::from_rgb(r, g, b)", "", pat), 0);
    assert_eq!(literal_calls("Color32::from_rgb(", "    1,", pat), 1);
    assert_eq!(literal_calls("Color32::from_rgb(", "    r,", pat), 0);
    assert_eq!(literal_calls("text.size(12.0)", "", ".size("), 1);
    assert_eq!(literal_calls("text.size(type_size::BODY)", "", ".size("), 0);
    assert_eq!(literal_calls("text.desired_size(12.0)", "", ".size("), 0);
    assert_eq!(
        named_colors("p.text(Color32::WHITE); q(egui::Color32::LIGHT_GRAY)"),
        2
    );
    assert_eq!(
        named_colors("x(Color32::TRANSPARENT); Color32::from_rgb(r, g, b)"),
        0
    );
    let src = "let a = 1;\n// skipped\n#[cfg(test)]\nmod tests {\nlet b = 2;\n}\n";
    assert_eq!(production(src), vec![(1, "let a = 1;")]);
    let decl = "#[cfg(test)]\nmod kit_tests;\nlet a = 1;\n";
    assert_eq!(production(decl).len(), 3);
    let split = "let c = Color32::from_rgb(\n    1,\n    2,\n    3,\n);\nf(Color32::WHITE);\n";
    let f = scan(split);
    assert_eq!((f.color, f.font), (vec![1, 6], vec![]));
    let sp = "ui.add_space(8.0);\nui.add_space(space::SM);\nMargin::same(6)\nMargin::symmetric(\n    4, 2)\n\
              s.item_spacing.x = 0.0;\ns.item_spacing = egui::vec2(4.0, 2.0);\ns.item_spacing.y = gap;\n\
              ui.add_space(8.0); // design-lint: data\n";
    assert_eq!(scan(sp).space, vec![1, 3, 4, 6, 7]);
    let marked = "let c = Color32::from_rgb(1, 2, 3); // design-lint: data\n";
    assert!(scan(marked).color.is_empty());
    let tail = "let c = token; // was Color32::from_rgb(1, 2, 3)\n";
    assert!(scan(tail).color.is_empty());
}

#[test]
fn baseline_format() {
    let b = parse_baseline("# note\n\ncrates/app/src/a.rs 2 3\n");
    assert_eq!(b.get("crates/app/src/a.rs"), Some(&(2, 3, 0)));
    assert_eq!(b.len(), 1);
    let b = parse_baseline("crates/app/src/a.rs 2 3 9\n");
    assert_eq!(b.get("crates/app/src/a.rs"), Some(&(2, 3, 9)));
}

/// Every spacing token from XS up sits on the 4 px grid and the scale rises strictly.
#[test]
fn space_tokens_are_on_the_four_px_grid() {
    use ddc_ui::sem::space::*;
    let scale = [XS, SM, MD, LG, XL, XXL, XXXL];
    for v in scale {
        assert_eq!(v % 4.0, 0.0, "{v} is off the 4 px grid");
    }
    assert!(scale.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(XXS, 2.0);
}

#[test]
fn on_colour_lint_sees_the_isolate_banner() {
    let bad = r#"
        let w = t.sem().snap_text_on;
        ui.label(egui::RichText::new(label).color(w).strong());
        if ui.button(egui::RichText::new("×").color(w)).clicked() {}
        if ui
            .button(
                egui::RichText::new(self.labels.t("iso.apply")).color(w),
            )
            .clicked()
        {}
    "#;
    assert_eq!(
        on_colour_misuse(bad).len(),
        2,
        "{:?}",
        on_colour_misuse(bad)
    );
    let direct = r#"ui.small_button(RichText::new("x(").color(s.accent_text_on));"#;
    assert_eq!(on_colour_misuse(direct).len(), 1);
    let token = r#"ui.button(RichText::new("x").color(th.c("danger.text_on")));"#;
    assert_eq!(on_colour_misuse(token).len(), 1);
    let own_fill =
        r#"ui.add(egui::Button::new(RichText::new("x").color(s.accent_text_on)).fill(s.accent));"#;
    assert!(on_colour_misuse(own_fill).is_empty());
    let white = "let w = Color32::WHITE;\nui.button(RichText::new(\"x\").color(w));";
    assert_eq!(on_colour_misuse(white).len(), 1);
    let sheet = "let w = ov.paper;\nui.button(RichText::new(\"x\").color(w));";
    assert_eq!(on_colour_misuse(sheet).len(), 1);
    let label_only = r#"ui.label(RichText::new("x").color(s.snap_text_on)); ui.button("y");"#;
    assert!(on_colour_misuse(label_only).is_empty());
}

const CYRILLIC_BASELINE: &str = "tests/cyrillic_baseline.txt";

fn is_cyrillic(c: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&c)
}

/// String literals on one code line that contain Cyrillic. Strings that span lines are judged per
/// line, which only matters for how the baseline was counted.
fn cyrillic_literals(code: &str) -> usize {
    let chars: Vec<char> = code.chars().collect();
    let (mut n, mut i) = (0, 0);
    let (mut in_str, mut esc, mut hit) = (false, false, false);
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
                n += usize::from(hit);
            } else if is_cyrillic(c) {
                hit = true;
            }
        } else if c == '\'' && chars.get(i + 1) == Some(&'"') && chars.get(i + 2) == Some(&'\'') {
            i += 2;
        } else if c == '"' {
            in_str = true;
            hit = false;
        }
        i += 1;
    }
    n + usize::from(in_str && hit)
}

fn cyrillic_counts() -> BTreeMap<String, usize> {
    ui_sources()
        .into_iter()
        .filter_map(|(name, src)| {
            let n: usize = production(&src)
                .iter()
                .map(|(_, code)| cyrillic_literals(code))
                .sum();
            (n > 0).then_some((name, n))
        })
        .collect()
}

/// Cyrillic belongs in the locale files. Existing literals are counted per file; the count may
/// only fall. `DESIGN_LINT_BLESS=1` rewrites the baseline.
#[test]
fn cyrillic_literals_stay_within_the_per_file_baseline() {
    let found = cyrillic_counts();
    let path = crate_dir().join(CYRILLIC_BASELINE);
    if std::env::var_os("DESIGN_LINT_BLESS").is_some() {
        let mut text = String::from(
            "# String literals with Cyrillic per file: path count. Only goes down; see design_lint.rs.\n",
        );
        for (name, n) in &found {
            text.push_str(&format!("{name} {n}\n"));
        }
        std::fs::write(&path, text).expect("write the cyrillic baseline");
        return;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let base = parse_counts(&text);
    let over: Vec<String> = found
        .iter()
        .filter(|(name, n)| **n > base.get(*name).copied().unwrap_or(0))
        .map(|(name, n)| {
            let allowed = base.get(name).copied().unwrap_or(0);
            format!("{name}: {n} literals with Cyrillic, allowed {allowed}")
        })
        .collect();
    assert!(
        over.is_empty(),
        "UI text goes to crates/ui/data/i18n/{{en,ru}}/*.json, not into code:\n{}",
        over.join("\n")
    );
}

/// `path count` per line; `#` starts a comment.
fn parse_counts(text: &str) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for l in text.lines().map(str::trim) {
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.split_whitespace().collect();
        assert_eq!(f.len(), 2, "bad baseline line: {l}");
        let n = f[1]
            .parse::<usize>()
            .unwrap_or_else(|_| panic!("bad baseline line: {l}"));
        out.insert(f[0].to_string(), n);
    }
    out
}

/// Sorted file names and, per file, its sorted keys.
fn locale_keys(dir: &Path) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let p = e.expect("dir entry").path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let name = p.file_name().expect("file name").to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let value: serde_json::Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let obj = value
            .as_object()
            .unwrap_or_else(|| panic!("{}: not a JSON object", p.display()));
        let mut keys: Vec<String> = obj.keys().cloned().collect();
        keys.sort();
        out.insert(name, keys);
    }
    out
}

fn locale_diff(
    en: &BTreeMap<String, Vec<String>>,
    ru: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let mut diff = Vec::new();
    for name in en.keys().chain(ru.keys()).collect::<std::collections::BTreeSet<_>>() {
        match (en.get(name), ru.get(name)) {
            (Some(_), None) => diff.push(format!("{name}: only in en")),
            (None, Some(_)) => diff.push(format!("{name}: only in ru")),
            (Some(a), Some(b)) => {
                for k in a.iter().filter(|k| !b.contains(k)) {
                    diff.push(format!("{name}: key {k} missing in ru"));
                }
                for k in b.iter().filter(|k| !a.contains(k)) {
                    diff.push(format!("{name}: key {k} missing in en"));
                }
            }
            (None, None) => {}
        }
    }
    diff
}

#[test]
fn every_locale_file_has_the_same_keys_in_en_and_ru() {
    let dir = crate_dir().join("data/i18n");
    let (en, ru) = (locale_keys(&dir.join("en")), locale_keys(&dir.join("ru")));
    assert!(!en.is_empty(), "no English locale files found");
    let diff = locale_diff(&en, &ru);
    assert!(diff.is_empty(), "en and ru differ:\n{}", diff.join("\n"));
}

#[test]
fn locale_and_cyrillic_checks_see_what_they_should() {
    let ru = "\u{0434}";
    assert_eq!(cyrillic_literals("let a = \"abc\";"), 0);
    assert_eq!(cyrillic_literals(&format!("let a = \"{ru}\";")), 1);
    assert_eq!(cyrillic_literals(&format!("f(\"x\", \"{ru}\", \"{ru}{ru}\")")), 2);
    assert_eq!(cyrillic_literals(&format!("f(\"a\\\"{ru}\")")), 1);
    assert_eq!(cyrillic_literals(&format!("let q = '\"'; g(\"{ru}\")")), 1);
    assert_eq!(cyrillic_literals(&format!("let c = '{ru}';")), 0);
    let m = |keys: &[&str]| keys.iter().map(|k| k.to_string()).collect::<Vec<_>>();
    let one = BTreeMap::from([("a.json".to_string(), m(&["k1", "k2"]))]);
    assert!(locale_diff(&one, &one.clone()).is_empty());
    let fewer = BTreeMap::from([("a.json".to_string(), m(&["k1"]))]);
    assert_eq!(locale_diff(&one, &fewer), vec!["a.json: key k2 missing in ru"]);
    assert_eq!(locale_diff(&fewer, &one), vec!["a.json: key k2 missing in en"]);
    let other = BTreeMap::from([("b.json".to_string(), m(&["k1"]))]);
    assert_eq!(locale_diff(&one, &other).len(), 2);
    let b = parse_counts("# n\n\ncrates/app/src/a.rs 4\n");
    assert_eq!(b.get("crates/app/src/a.rs"), Some(&4));
}
