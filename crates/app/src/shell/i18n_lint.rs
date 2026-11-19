//! Catalog lint: the chrome shows no text that bypasses the locale catalog.
//! Source scan is textual (no parser): good enough to catch a new hardcoded label in review.
use std::path::{Path, PathBuf};

fn app_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn shell_files() -> Vec<PathBuf> {
    let mut v = vec![app_src().join("shell.rs")];
    rs_files(&app_src().join("shell"), &mut v);
    v.retain(|p| !p.ends_with("i18n_lint.rs"));
    v.sort();
    v
}

/// The part of a file before its test module.
fn non_test(s: &str) -> &str {
    match s.find("#[cfg(test)]\nmod ") {
        Some(i) => &s[..i],
        None => s,
    }
}

/// String literal that starts at `s[i] == '"'`: (content, index after the closing quote).
fn lit_at(s: &str, i: usize) -> Option<(&str, usize)> {
    let b = s.as_bytes();
    let mut k = i + 1;
    while k < b.len() {
        match b[k] {
            b'\\' => k += 2,
            b'"' => return Some((&s[i + 1..k], k + 1)),
            _ => k += 1,
        }
    }
    None
}

fn is_key(k: &str) -> bool {
    k.contains('.')
        && k.split('.').all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

fn is_cyr(c: char) -> bool {
    ('\u{400}'..='\u{4ff}').contains(&c)
}

/// Callee path before the `(` at `open` (letters, digits, `_`, `::`).
fn callee(s: &str, open: usize) -> &str {
    let b = s.as_bytes();
    let mut j = open;
    while j > 0 && (b[j - 1].is_ascii_alphanumeric() || b[j - 1] == b'_' || b[j - 1] == b':') {
        j -= 1;
    }
    &s[j..open]
}

/// Catalog keys passed as the first or second literal argument of a lookup call.
fn used_keys(src: &str) -> Vec<(usize, String)> {
    const NAMES: [&str; 7] = ["tr", "tr_str", "tr_lang", "tr_n", "t", "tf", "t_in"];
    let b = src.as_bytes();
    let mut out = vec![];
    for (open, _) in src.match_indices('(') {
        let name = callee(src, open).rsplit("::").next().unwrap_or("");
        if !NAMES.contains(&name) {
            continue;
        }
        let mut k = open + 1;
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        if k < b.len() && b[k] != b'"' {
            let st = k;
            while k < b.len() && (b[k].is_ascii_alphanumeric() || b"_:.".contains(&b[k])) {
                k += 1;
            }
            if k == st || !(b[st].is_ascii_alphabetic() || b[st] == b'_') {
                continue;
            }
            while k < b.len() && b[k].is_ascii_whitespace() {
                k += 1;
            }
            if k >= b.len() || b[k] != b',' {
                continue;
            }
            k += 1;
            while k < b.len() && b[k].is_ascii_whitespace() {
                k += 1;
            }
        }
        if k < b.len() && b[k] == b'"' {
            if let Some((lit, _)) = lit_at(src, k) {
                if is_key(lit) {
                    out.push((src[..open].matches('\n').count() + 1, lit.to_string()));
                }
            }
        }
    }
    out
}

const API: [&str; 19] = [
    "label",
    "heading",
    "button",
    "small_button",
    "menu_button",
    "selectable_label",
    "checkbox",
    "on_hover_text",
    "on_disabled_hover_text",
    "hint_text",
    "link",
    "caption",
    "section_title",
    "empty_note",
    "kv_num",
    "primary_action",
    "cancel_button",
    "colored_label",
    "selected_text",
];
const API_NEW: [&str; 3] = ["RichText::new", "Button::new", "Window::new"];
/// Brand and key names that are the same in every language.
const ALLOWED_LITERALS: [&str; 3] = ["DDC Editor", "OpenConstructionDesign", "Esc"];

/// Text literals given straight to egui widgets: (line, literal).
fn widget_literals(src: &str) -> Vec<(usize, String)> {
    let b = src.as_bytes();
    let mut out = vec![];
    for (open, _) in src.match_indices('(') {
        let path = callee(src, open);
        let last = path.rsplit("::").next().unwrap_or("");
        if !(API.contains(&last) || API_NEW.iter().any(|n| path.ends_with(n))) {
            continue;
        }
        let mut k = open + 1;
        for _ in 0..4 {
            while k < b.len() && (b[k].is_ascii_whitespace() || b[k] == b'&') {
                k += 1;
            }
            if k >= b.len() {
                break;
            }
            if b[k] == b'"' {
                if let Some((lit, _)) = lit_at(src, k) {
                    let letters = lit.chars().any(char::is_alphabetic);
                    if letters && !ALLOWED_LITERALS.contains(&lit) {
                        out.push((src[..open].matches('\n').count() + 1, lit.to_string()));
                    }
                }
                break;
            }
            while k < b.len() && !b"\",()".contains(&b[k]) {
                k += 1;
            }
            if k >= b.len() || b[k] != b',' {
                break;
            }
            k += 1;
        }
    }
    out
}

/// Name of the function that contains line `line` (1-based).
fn enclosing_fn(src: &str, line: usize) -> String {
    for l in src.lines().take(line).collect::<Vec<_>>().iter().rev() {
        let t = l.trim_start();
        let t = t.strip_prefix("pub(crate) ").or_else(|| t.strip_prefix("pub(super) ")).unwrap_or(t);
        let t = t.strip_prefix("pub ").unwrap_or(t);
        if let Some(r) = t.strip_prefix("fn ") {
            return r.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        }
    }
    String::new()
}

/// Functions whose Russian literals are sample data or word lists, not chrome.
fn cyrillic_allowed(fn_name: &str) -> bool {
    fn_name.starts_with("demo") || fn_name == "status_level" || fn_name == "unique_level_name"
}

#[test]
fn en_and_ru_catalogs_have_identical_keys() {
    use ddc_ui::i18n::{Catalog, Lang};
    let c = Catalog::new(Lang::En);
    let (en, ru) = (c.table(Lang::En), c.table(Lang::Ru));
    let only_en: Vec<_> = en.keys().filter(|k| !ru.contains_key(*k)).take(10).collect();
    let only_ru: Vec<_> = ru.keys().filter(|k| !en.contains_key(*k)).take(10).collect();
    assert!(only_en.is_empty(), "keys missing in ru: {only_en:?}");
    assert!(only_ru.is_empty(), "keys missing in en: {only_ru:?}");
}

#[test]
fn qa12_values_are_filled_and_share_placeholders() {
    use ddc_ui::i18n::{Catalog, Lang};
    fn holes(s: &str) -> Vec<String> {
        let mut v: Vec<String> = s
            .split('{')
            .skip(1)
            .filter_map(|p| p.split('}').next())
            .filter(|n| n.chars().all(|c| c.is_alphanumeric() || c == '_'))
            .map(str::to_string)
            .collect();
        v.sort();
        v
    }
    let c = Catalog::new(Lang::En);
    let mut n = 0;
    for (k, en) in c.table(Lang::En).iter().filter(|(k, _)| k.starts_with("type_dialog.")) {
        let ru = c.table(Lang::Ru).get(k).and_then(|v| v.as_str()).unwrap_or("");
        let en = en.as_str().unwrap_or("");
        assert!(!en.is_empty() && !ru.is_empty(), "{k}: empty text");
        assert_eq!(holes(en), holes(ru), "{k}: placeholders differ");
        n += 1;
    }
    assert!(n > 100, "q12 catalog went missing: {n}");
}

#[test]
fn every_literal_key_used_in_code_exists() {
    use ddc_ui::i18n::{Catalog, Lang};
    let c = Catalog::new(Lang::En);
    let en = c.table(Lang::En);
    let mut files = vec![];
    rs_files(&app_src(), &mut files);
    let mut missing = vec![];
    for f in files {
        let s = std::fs::read_to_string(&f).unwrap();
        for (line, k) in used_keys(&s) {
            if !en.contains_key(&k) {
                missing.push(format!("{}:{line} {k}", f.display()));
            }
        }
    }
    assert!(missing.is_empty(), "keys not in the catalog:\n{}", missing.join("\n"));
}

#[test]
fn shell_has_no_hardcoded_chrome_text() {
    let mut bad = vec![];
    for f in shell_files() {
        let all = std::fs::read_to_string(&f).unwrap();
        let src = non_test(&all);
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        for (line, lit) in widget_literals(src) {
            bad.push(format!("{name}:{line} widget text {lit:?}"));
        }
        for (i, _) in src.match_indices("if ru") {
            let rest = src[i + 5..].trim_start();
            if let Some(r) = rest.strip_prefix('{') {
                if r.trim_start().starts_with('"') {
                    let line = src[..i].matches('\n').count() + 1;
                    bad.push(format!("{name}:{line} bilingual literal pair"));
                }
            }
        }
        for (n, l) in src.lines().enumerate() {
            let code = l.split(" //").next().unwrap_or(l);
            if code.trim_start().starts_with("//") || !code.contains('"') {
                continue;
            }
            if code.chars().any(is_cyr) && !cyrillic_allowed(&enclosing_fn(src, n + 1)) {
                bad.push(format!("{name}:{} Cyrillic literal", n + 1));
            }
        }
    }
    assert!(bad.is_empty(), "text outside the catalog:\n{}", bad.join("\n"));
}

#[test]
fn scanner_finds_what_it_should() {
    let src = "ui.label(\"Open\");\nui.button(crate::tr(\"a.b\", &[]));\nlet x = if ru { \"A\" } else { \"B\" };\nc.t(\"x.y\"); tr_lang(ru, \"p.q\", &[]);";
    assert_eq!(widget_literals(src).len(), 1);
    let keys: Vec<_> = used_keys(src).into_iter().map(|k| k.1).collect();
    assert_eq!(keys, ["a.b", "x.y", "p.q"]);
    assert!(is_key("type_dialog.panels.sill") && !is_key("Open") && !is_key("a.B"));
}

/// Keys whose English value is the template of an io/geometry message (`xl_text::tr`,
/// `param_editor::msg`): they keep the engine's straight quotes.
#[cfg(test)]
const ENGINE_TEMPLATES: &[&str] = &["xl.msg.", "xl.pfx.", "pe.msg_"];

#[test]
fn names_are_quoted_typographically() {
    use ddc_ui::i18n::{Catalog, Lang};
    let c = Catalog::new(Lang::En);
    for (lang, bad) in [(Lang::En, ["'{", "«", "»"]), (Lang::Ru, ["'{", "“", "”"])] {
        for (k, v) in c.table(lang).iter() {
            // English engine messages are matched back to their templates byte for byte
            if lang == Lang::En && ENGINE_TEMPLATES.iter().any(|p| k.starts_with(p)) {
                continue;
            }
            let v = v.as_str().unwrap_or("");
            for b in bad {
                assert!(!v.contains(b), "{lang:?} {k}: quote style {b:?} in {v:?}");
            }
        }
    }
}

#[test]
fn job_stage_labels_are_plain() {
    use ddc_ui::i18n::{Catalog, Lang};
    let c = Catalog::new(Lang::En);
    for k in ["job.stage.plan", "job.stage.commit", "job.stage.meshes", "job.stage.bridge"] {
        let v = c.table(Lang::En).get(k).and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
        for w in ["module", "instance", "detail", "parsing"] {
            assert!(!v.contains(w), "{k}: jargon {w:?} in {v:?}");
        }
    }
}

#[test]
fn xlw_uses_one_term_for_workbook() {
    use ddc_ui::i18n::{Catalog, Lang};
    let c = Catalog::new(Lang::En);
    for (k, v) in c.table(Lang::En).iter().filter(|(k, _)| k.starts_with("xlw.")) {
        let v = v.as_str().unwrap_or("").to_lowercase();
        // `{book}` is a placeholder name, not wording
        let loose = v.replace("workbook", "").replace("notebook", "").replace("{book}", "");
        assert!(!loose.contains("book"), "{k}: use workbook, got {v:?}");
        assert!(!v.contains(" - "), "{k}: use an em dash: {v:?}");
    }
    for (k, v) in c.table(Lang::Ru).iter().filter(|(k, _)| k.starts_with("xlw.")) {
        let v = v.as_str().unwrap_or("");
        assert!(!v.contains(" - "), "{k}: use an em dash: {v:?}");
    }
}

#[test]
fn xlw_status_lines_start_upper() {
    use ddc_ui::i18n::{Catalog, Lang};
    let c = Catalog::new(Lang::En);
    for k in [
        "xlw.saved", "xlw.not_saved", "xlw.workbook_not_read", "xlw.workbook_updated",
        "xlw.the_model_changed_after_the_previe", "xlw.book_changed_reread",
        "xlw.new_rows_got_their_id", "xlw.workbook_import_failed",
    ] {
        for lang in [Lang::En, Lang::Ru] {
            let v = c.table(lang).get(k).and_then(|v| v.as_str()).unwrap_or("");
            assert!(v.chars().next().is_some_and(char::is_uppercase), "{lang:?} {k}: {v:?}");
        }
    }
}
