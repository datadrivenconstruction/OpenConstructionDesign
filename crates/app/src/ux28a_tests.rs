// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Locale rule: no Cyrillic in production `.rs` code (comments and test/fixture files excepted);
//! the strings moved to `i18n/{en,ru}/edit_panels.json` exist in both languages.

use std::collections::BTreeMap;
use std::path::Path;

const EN: &str = include_str!("../../ui/data/i18n/en/edit_panels.json");
const RU: &str = include_str!("../../ui/data/i18n/ru/edit_panels.json");

fn cyr(s: &str) -> bool {
    s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c))
}

fn table(s: &str) -> BTreeMap<String, String> {
    serde_json::from_str(s).expect("catalog json")
}

fn holes(s: &str) -> Vec<String> {
    let mut v: Vec<String> = s
        .split('{')
        .skip(1)
        .filter_map(|r| r.split('}').next())
        .map(str::to_string)
        .collect();
    v.sort();
    v
}

#[test]
fn keys_in_both_languages_en_without_cyrillic() {
    let (en, ru) = (table(EN), table(RU));
    assert!(en.len() >= 60);
    assert_eq!(en.keys().collect::<Vec<_>>(), ru.keys().collect::<Vec<_>>());
    for (k, v) in &en {
        assert!(!cyr(v), "{k}: Cyrillic in the English text");
        assert_eq!(holes(v), holes(&ru[k]), "{k}: placeholders differ");
        // explicit language, the global UI language is never touched
        assert_eq!(&crate::tr_lang(false, k, &[]), v, "{k}");
        assert_eq!(&crate::tr_lang(true, k, &[]), &ru[k], "{k}");
    }
}

#[test]
fn en_panel_texts_have_no_cyrillic() {
    let a = &[("name", "X"), ("e", "e"), ("n", "1")];
    for k in table(EN).keys() {
        assert!(!cyr(&crate::tr_lang(false, k, a)), "{k}");
    }
}

/// `src` with every inline `#[cfg(test)] [#[..]] [pub(..)] mod x { .. }` blanked, line numbers kept.
/// `#[cfg(test)] mod x;` declarations cut nothing: the rest of the file is still production code.
fn production_part(src: &str) -> String {
    let lines: Vec<&str> = src.split('\n').collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        match test_mod_end(&lines, i) {
            Some(e) => {
                out.extend(std::iter::repeat("").take(e - i + 1));
                i = e + 1;
            }
            None => {
                out.push(lines[i]);
                i += 1;
            }
        }
    }
    out.join("\n")
}

fn strip_attrs(mut rest: &str) -> Option<&str> {
    while rest.starts_with("#[") {
        let mut depth = 0;
        let end = rest.char_indices().find_map(|(k, c)| {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(k);
                    }
                }
                _ => {}
            }
            None
        })?;
        rest = rest[end + 1..].trim();
    }
    Some(rest)
}

fn is_inline_mod(rest: &str) -> bool {
    let rest = match rest.strip_prefix("pub(") {
        Some(r) => match r.split_once(')') {
            Some((_, r)) => r.trim_start(),
            None => return false,
        },
        None => rest.strip_prefix("pub ").map_or(rest, str::trim_start),
    };
    let Some(rest) = rest.strip_prefix("mod ") else { return false };
    let rest = rest.trim_start();
    let name = rest.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(rest.len());
    name > 0 && rest[name..].trim_start().starts_with('{')
}

/// Last line of the inline test module whose `#[cfg(test)]` is on line `i`.
fn test_mod_end(lines: &[&str], i: usize) -> Option<usize> {
    let mut rest = lines[i].trim().strip_prefix("#[cfg(test)]")?.trim();
    let mut j = i;
    loop {
        rest = strip_attrs(rest)?;
        if !rest.is_empty() && !rest.starts_with("//") {
            break;
        }
        j += 1;
        rest = lines.get(j)?.trim();
    }
    // a `#[cfg(test)]` function at file level is test code as much as a test module
    let test_fn = rest.strip_prefix("pub(crate) ").unwrap_or(rest).starts_with("fn ");
    if is_inline_mod(rest) || test_fn {
        block_end(lines, j)
    } else {
        None
    }
}

fn raw_prefix(ch: &[char], k: usize) -> bool {
    let id = |c: char| c.is_alphanumeric() || c == '_';
    match k {
        0 => true,
        1 => !id(ch[0]) || ch[0] == 'b',
        _ => !id(ch[k - 1]) || (ch[k - 1] == 'b' && !id(ch[k - 2])),
    }
}

/// Line of the `}` closing the first `{` from line `start`; braces in strings, chars and comments
/// do not count.
fn block_end(lines: &[&str], start: usize) -> Option<usize> {
    let (mut depth, mut cmt) = (0i32, 0u32);
    // None: code; Some(None): plain string; Some(Some(h)): raw string with h hashes
    let mut in_str: Option<Option<usize>> = None;
    for (li, line) in lines.iter().enumerate().skip(start) {
        let ch: Vec<char> = line.chars().collect();
        let n = ch.len();
        let at = |k: usize| ch.get(k).copied().unwrap_or('\0');
        let mut k = 0;
        while k < n {
            let (c, nx) = (ch[k], at(k + 1));
            if cmt > 0 {
                if c == '*' && nx == '/' {
                    cmt -= 1;
                    k += 2;
                } else if c == '/' && nx == '*' {
                    cmt += 1;
                    k += 2;
                } else {
                    k += 1;
                }
            } else if let Some(raw) = in_str {
                match raw {
                    Some(h) if c == '"' && (1..=h).all(|d| at(k + d) == '#') => {
                        in_str = None;
                        k += 1 + h;
                    }
                    None if c == '\\' => k += 2,
                    None if c == '"' => {
                        in_str = None;
                        k += 1;
                    }
                    _ => k += 1,
                }
            } else if c == '/' && nx == '/' {
                break;
            } else if c == '/' && nx == '*' {
                cmt = 1;
                k += 2;
            } else if c == '"' {
                in_str = Some(None);
                k += 1;
            } else if c == 'r' && raw_prefix(&ch, k) {
                let h = ch[k + 1..].iter().take_while(|&&x| x == '#').count();
                if at(k + 1 + h) == '"' {
                    in_str = Some(Some(h));
                    k += 2 + h;
                } else {
                    k += 1;
                }
            } else if c == '\'' {
                if nx == '\\' {
                    k = (k + 3..n).find(|&d| ch[d] == '\'').map_or(n, |d| d + 1);
                } else if at(k + 2) == '\'' {
                    k += 3;
                } else {
                    k += 1;
                }
            } else {
                if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    depth -= 1;
                    if depth == 0 {
                        return Some(li);
                    }
                }
                k += 1;
            }
        }
    }
    None
}

#[test]
fn production_part_cuts_only_inline_test_modules() {
    let src = "fn a() {}\n#[cfg(test)]\nmod decl;\nconst A: &str = \"\u{416}1\";\n#[cfg(test)]\n\
               #[allow(unused)]\npub(crate) mod tests {\n    const B: &str = \"}\u{416}2\";\n\
               \x20   const C: char = '{';\n    const D: &str = r#\"}\"#;\n    // }\n    /* } */\n\
               \x20   fn f<'a>(x: &'a str) -> &'a str { x }\n}\nconst E: &str = \"\u{416}3\";\n\
               #[cfg(test)] mod t2 { const F: &str = \"\u{416}4\"; }\n\
               #[cfg(test)]\n#[test]\nfn g() {\n    let _ = \"\u{416}5\";\n}\nconst G: &str = \"\u{416}6\";\n";
    let p = production_part(src);
    assert_eq!(p.lines().count(), src.lines().count(), "line numbers kept");
    let hits: Vec<usize> =
        p.lines().enumerate().filter(|(_, l)| cyr(l)).map(|(n, _)| n + 1).collect();
    // `mod decl;` does not end the scan; the inline modules (pub(crate), one-line) and the
    // file-level test function are skipped
    assert_eq!(hits, vec![4, 15, 22]);
}

fn scan(dir: &Path, bad: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if p.is_dir() {
            if name != "tests" && name != "target" {
                scan(&p, bad);
            }
            continue;
        }
        let skip = !name.ends_with(".rs")
            || name.starts_with("tests_")
            || name.ends_with("_tests.rs")
            || name == "tests.rs"
            || name.starts_with("fixture");
        if skip {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&p) else { continue };
        for (n, l) in production_part(&src).lines().enumerate() {
            if l.trim_start().starts_with("//") || !cyr(l) {
                continue;
            }
            // a trailing `// comment` without a quote after it is not code
            let code = match l.split_once("//") {
                Some((a, b)) if !b.contains('"') => a,
                _ => l,
            };
            if cyr(code) {
                bad.push(format!("{}:{}", p.display(), n + 1));
            }
        }
    }
}

#[test]
fn no_cyrillic_in_production_code() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut bad = Vec::new();
    for c in std::fs::read_dir(&root).unwrap().flatten() {
        scan(&c.path().join("src"), &mut bad);
    }
    assert!(bad.is_empty(), "Cyrillic outside comments: {:?}", &bad[..bad.len().min(20)]);
}
