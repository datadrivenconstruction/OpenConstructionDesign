// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Wording lint over every en/ru catalog string (UX_AUDIT 2.11, docs/design/GLOSSARY.md).
//! Reads the JSON data directly and never touches the UI language. Vocabulary (forbidden
//! variants, capitals allowed inside a label) is parsed from the glossary section marked by
//! `wording-lint` comments; whether a command ends in the ellipsis comes from `"dialog": true`
//! in the command data. Each check is a pure function with a self-test on a planted bad string.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

const LANGS: [&str; 2] = ["en", "ru"];
const ELLIPSIS: char = '\u{2026}';
const EM_DASH: char = '\u{2014}';

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    v.sort();
    v
}

/// One catalog entry: every text of the key (a plural object contributes each form).
struct Entry {
    key: String,
    texts: Vec<String>,
}

/// All entries of a language, later files overriding earlier ones like the app's loader.
fn catalog(lang: &str) -> Vec<Entry> {
    let dir = crate_dir().join("data").join("i18n");
    let mut files =
        vec![dir.join(format!("ui.{lang}.json")), dir.join(format!("commands.{lang}.json"))];
    files.extend(json_files(&dir.join(lang)));
    let mut map: BTreeMap<String, Value> = BTreeMap::new();
    for f in files {
        let m: BTreeMap<String, Value> =
            serde_json::from_str(&read(&f)).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        map.extend(m);
    }
    map.into_iter()
        .map(|(key, v)| {
            let texts = match v {
                Value::String(s) => vec![s],
                Value::Object(o) => {
                    o.values().filter_map(|x| x.as_str().map(String::from)).collect()
                }
                _ => Vec::new(),
            };
            Entry { key, texts }
        })
        .collect()
}

/// `(id, opens a dialog that still needs input)` for every command.
fn commands() -> Vec<(String, bool)> {
    let data = crate_dir().join("data");
    let mut files = vec![data.join("commands.json")];
    files.extend(json_files(&data.join("commands.d")));
    let mut out = Vec::new();
    for f in files {
        let list: Vec<Value> =
            serde_json::from_str(&read(&f)).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        for c in list {
            let id = c["id"].as_str().unwrap_or_else(|| panic!("{}: no id", f.display()));
            out.push((id.to_string(), c["dialog"].as_bool().unwrap_or(false)));
        }
    }
    out
}

// ---- glossary vocabulary ----

/// One forbidden spelling from the glossary table.
#[derive(Clone, Debug)]
struct Pat {
    text: String,
    at_start: bool,
    labels_only: bool,
    case_sensitive: bool,
}

#[derive(Clone, Debug)]
struct Rule {
    lang: String,
    pats: Vec<Pat>,
    /// Phrases of the "write" column; those holding a forbidden word are cut out before a scan.
    ok: Vec<String>,
}

struct Vocab {
    rules: Vec<Rule>,
    capitals: BTreeSet<String>,
}

fn backticked(cell: &str) -> String {
    let c = cell.trim();
    c.strip_prefix('`').and_then(|x| x.strip_suffix('`')).unwrap_or(c).to_string()
}

fn pat(raw: &str) -> Pat {
    let (at_start, rest) = match raw.strip_prefix('^') {
        Some(r) => (true, r),
        None => (false, raw),
    };
    let (labels_only, rest) = match rest.strip_prefix('@') {
        Some(r) => (true, r),
        None => (false, rest),
    };
    Pat {
        text: rest.to_string(),
        at_start,
        labels_only,
        case_sensitive: !rest.chars().any(char::is_lowercase),
    }
}

fn parse_vocab(md: &str) -> Vocab {
    let start = md.find("<!-- wording-lint:forbidden -->").expect("glossary section of the test");
    let mut rules = Vec::new();
    let mut capitals = BTreeSet::new();
    let mut in_table = false;
    let mut in_caps = false;
    for line in md[start..].lines() {
        if line.starts_with("<!-- wording-lint:") {
            in_table = line.contains(":forbidden");
            in_caps = line.contains(":capitals");
            continue;
        }
        if in_table && line.starts_with('|') {
            let cells: Vec<&str> = line.trim_matches('|').split('|').collect();
            if cells.len() != 3 || cells[0].trim() == "lang" || cells[0].trim().starts_with('-') {
                continue;
            }
            rules.push(Rule {
                lang: cells[0].trim().to_string(),
                pats: cells[1].split(';').map(|c| pat(&backticked(c))).collect(),
                ok: cells[2].split(';').map(backticked).collect(),
            });
        }
        if in_caps && line.starts_with("- ") {
            capitals.extend(line[2..].split(',').map(|w| w.trim().to_string()));
        }
    }
    Vocab { rules, capitals }
}

fn vocab() -> Vocab {
    let p = crate_dir().join("..").join("..").join("docs").join("design").join("GLOSSARY.md");
    parse_vocab(&read(&p))
}

// ---- which keys are labels ----

fn is_label_key(key: &str, ids: &BTreeSet<String>) -> bool {
    if key.starts_with("rgroup.") || key.starts_with("tab.") || key.starts_with("ribbon.tab.") {
        return true;
    }
    if let Some(rest) = key.strip_prefix("cmd.") {
        let id = rest.strip_suffix(".short").unwrap_or(rest);
        return ids.contains(id);
    }
    let parts: Vec<&str> = key.split('.').collect();
    let last = parts.last().copied().unwrap_or("");
    let button = parts.contains(&"btn") || last.ends_with("_btn") || last == "button";
    let tab_name = parts.len() <= 3 && parts[..parts.len() - 1].contains(&"tab");
    button || tab_name
}

// ---- checks (pure) ----

fn has_ascii_ellipsis(s: &str) -> bool {
    s.contains("...")
}

fn spaced_hyphen(s: &str) -> bool {
    if s.contains("--") {
        return true;
    }
    let c: Vec<char> = s.chars().collect();
    (1..c.len().saturating_sub(2)).any(|i| {
        c[i] == ' '
            && c[i + 1] == '-'
            && c[i + 2] == ' '
            && !c[i - 1].is_whitespace()
            && i + 3 < c.len()
            && !c[i + 3].is_whitespace()
            && !(c[i - 1].is_ascii_digit() && c[i + 3].is_ascii_digit())
    })
}

fn trailing_period(s: &str) -> bool {
    s.trim_end().ends_with('.') && !s.trim_end().ends_with("..")
}

/// First word that starts with a capital although it is neither first in its part (parts are
/// split by « · ») nor a listed proper noun or acronym. Quoted names are references.
fn case_problem(s: &str, capitals: &BTreeSet<String>) -> Option<String> {
    for part in s.split(" \u{b7} ") {
        for (i, word) in part.split_whitespace().enumerate() {
            let w = word.trim_start_matches(['(', '[', '<']);
            if i == 0 || w.starts_with(['\u{ab}', '"', '\u{201c}', '\'']) {
                continue;
            }
            let base: String = w
                .split(['-', '/', ',', ':', ';', ')', '\u{bb}', '"', '\u{201d}', '\'', ELLIPSIS])
                .next()
                .unwrap_or("")
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect();
            let capital = base.chars().next().is_some_and(char::is_uppercase);
            if capital && !base.chars().any(|c| c.is_ascii_digit()) && !capitals.contains(&base) {
                return Some(word.to_string());
            }
        }
    }
    None
}

fn colon_in_name(s: &str) -> bool {
    s.contains(": ")
}

fn too_long(en: &str, ru: &str) -> bool {
    let (e, r) = (en.chars().count(), ru.chars().count());
    r * 10 > e * 14 && r > e + 12
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn word_start(hay: &[char], i: usize) -> bool {
    i == 0 || !(hay[i - 1].is_alphanumeric() || hay[i - 1] == '_')
}

fn find_from(hay: &[char], needle: &[char], from: usize, p: &Pat) -> Option<usize> {
    let end = hay.len().checked_sub(needle.len())?;
    (from..=end).find(|&i| {
        (if p.at_start { i == 0 } else { word_start(hay, i) })
            && needle.iter().enumerate().all(|(k, n)| {
                if p.case_sensitive {
                    hay[i + k] == *n
                } else {
                    lower(hay[i + k]) == lower(*n)
                }
            })
    })
}

/// Cuts every occurrence of `phrase` (any case, word start) out of `text`.
fn strip_phrase(text: &str, phrase: &str) -> String {
    let mut hay: Vec<char> = text.chars().collect();
    let needle: Vec<char> = phrase.chars().collect();
    let p = Pat { text: phrase.into(), at_start: false, labels_only: false, case_sensitive: false };
    while let Some(i) = find_from(&hay, &needle, 0, &p) {
        hay.drain(i..i + needle.len());
    }
    hay.into_iter().collect()
}

fn forbidden_hit(text: &str, rule: &Rule, label: bool) -> Option<String> {
    let mut t = text.to_string();
    for ok in &rule.ok {
        // only a correct phrase that contains a forbidden word ("AI assistant") is cut out
        let holds = rule.pats.iter().any(|p| ok.to_lowercase().contains(&p.text.to_lowercase()));
        if holds {
            t = strip_phrase(&t, ok);
        }
    }
    let hay: Vec<char> = t.chars().collect();
    for p in &rule.pats {
        if p.labels_only && !label {
            continue;
        }
        let needle: Vec<char> = p.text.chars().collect();
        if find_from(&hay, &needle, 0, p).is_some() {
            return Some(p.text.clone());
        }
    }
    None
}

// ---- driver ----

fn report(what: &str, found: Vec<String>) {
    assert!(
        found.is_empty(),
        "{what}: {} problem(s)\n{}",
        found.len(),
        found.iter().take(80).cloned().collect::<Vec<_>>().join("\n")
    );
}

fn id_set() -> BTreeSet<String> {
    commands().into_iter().map(|(id, _)| id).collect()
}

/// Every label text as `(lang, key, text)`.
fn labels() -> Vec<(&'static str, String, String)> {
    let ids = id_set();
    let mut out = Vec::new();
    for lang in LANGS {
        for e in catalog(lang) {
            if is_label_key(&e.key, &ids) {
                for t in e.texts {
                    out.push((lang, e.key.clone(), t));
                }
            }
        }
    }
    out
}

#[test]
fn ellipsis_is_one_character() {
    let mut bad = Vec::new();
    for lang in LANGS {
        for e in catalog(lang) {
            for t in &e.texts {
                if has_ascii_ellipsis(t) {
                    bad.push(format!("{lang} {}: {t:?}", e.key));
                }
            }
        }
    }
    report("three dots instead of the single ellipsis character", bad);
}

#[test]
fn ellipsis_marks_exactly_the_dialog_commands() {
    let cmds = commands();
    assert!(cmds.iter().any(|(_, d)| *d), "no command is marked as a dialog");
    let mut bad = Vec::new();
    for lang in LANGS {
        let cat: BTreeMap<String, String> = catalog(lang)
            .into_iter()
            .filter_map(|e| e.texts.into_iter().next().map(|t| (e.key, t)))
            .collect();
        for (id, dialog) in &cmds {
            let key = format!("cmd.{id}");
            let Some(text) = cat.get(&key) else {
                bad.push(format!("{lang} {key}: no label"));
                continue;
            };
            if text.ends_with(ELLIPSIS) != *dialog {
                let want = if *dialog { "must end with" } else { "must not end with" };
                bad.push(format!("{lang} {key}: {text:?} {want} the ellipsis"));
            }
        }
    }
    report("ellipsis against the command's dialog flag", bad);
}

#[test]
fn labels_are_sentence_case() {
    let caps = vocab().capitals;
    let bad = labels()
        .into_iter()
        .filter_map(|(l, k, t)| case_problem(&t, &caps).map(|w| format!("{l} {k}: {t:?} ({w})")))
        .collect();
    report("capital letter inside a label", bad);
}

#[test]
fn labels_have_no_trailing_period() {
    let bad = labels()
        .into_iter()
        .filter(|(_, _, t)| trailing_period(t))
        .map(|(l, k, t)| format!("{l} {k}: {t:?}"))
        .collect();
    report("label ends with a period", bad);
}

#[test]
fn dashes_are_em_dashes_with_spaces() {
    let mut bad = Vec::new();
    for lang in LANGS {
        for e in catalog(lang) {
            for t in &e.texts {
                if spaced_hyphen(t) {
                    bad.push(format!("{lang} {}: {t:?} (use \" {EM_DASH} \")", e.key));
                }
            }
        }
    }
    report("spaced hyphen or double hyphen", bad);
}

#[test]
fn glossary_variants_are_absent() {
    let v = vocab();
    let ids = id_set();
    let mut bad = Vec::new();
    for lang in LANGS {
        for e in catalog(lang) {
            let label = is_label_key(&e.key, &ids);
            for t in &e.texts {
                for r in v.rules.iter().filter(|r| r.lang == lang) {
                    if let Some(w) = forbidden_hit(t, r, label) {
                        bad.push(format!("{lang} {}: {t:?} has {w:?}", e.key));
                    }
                }
            }
        }
    }
    report("forbidden glossary variant", bad);
}

#[test]
fn command_names_have_no_colon() {
    let ids = id_set();
    let bad = labels()
        .into_iter()
        .filter(|(_, k, t)| {
            let id = k.strip_prefix("cmd.").map(|r| r.strip_suffix(".short").unwrap_or(r));
            id.is_some_and(|i| ids.contains(i)) && colon_in_name(t)
        })
        .map(|(l, k, t)| format!("{l} {k}: {t:?}"))
        .collect();
    report("command name written as \"X: verb\"", bad);
}

#[test]
fn russian_command_labels_fit_the_english_ones() {
    let en: BTreeMap<String, String> = catalog("en")
        .into_iter()
        .filter_map(|e| e.texts.into_iter().next().map(|t| (e.key, t)))
        .collect();
    let ids = id_set();
    let mut bad = Vec::new();
    for e in catalog("ru") {
        let Some(id) = e.key.strip_prefix("cmd.") else { continue };
        let name = id.strip_suffix(".short").unwrap_or(id);
        if !ids.contains(name) {
            continue;
        }
        if let (Some(ru), Some(eng)) = (e.texts.first(), en.get(&e.key)) {
            if too_long(eng, ru) {
                bad.push(format!("{}: en {} chars {eng:?}, ru {} chars {ru:?}", e.key,
                    eng.chars().count(), ru.chars().count()));
            }
        }
    }
    report("Russian label more than 40 % longer than English", bad);
}

#[test]
fn vocabulary_is_read_from_the_glossary() {
    let v = vocab();
    for lang in LANGS {
        assert!(v.rules.iter().any(|r| r.lang == lang), "no rules for {lang}");
    }
    for w in ["IFC", "PDF", "3D", "AI", "MCP"] {
        assert!(v.capitals.contains(w), "{w} missing from the allowed capitals");
    }
    let ids = id_set();
    for lang in LANGS {
        let n = labels().iter().filter(|(l, k, _)| *l == lang && k.starts_with("cmd.")).count();
        assert!(n >= ids.len(), "{lang}: only {n} command labels found");
    }
}

// Russian samples, escaped to keep the source ASCII.
const RU_NEW_WALL_BAD: &str =
    "\u{41d}\u{43e}\u{432}\u{430}\u{44f} \u{421}\u{442}\u{435}\u{43d}\
    \u{430}";
const RU_NEW_WALL: &str = "\u{41d}\u{43e}\u{432}\u{430}\u{44f} \u{441}\u{442}\u{435}\u{43d}\u{430}";
const RU_TYPE_SIZE: &str = "\u{442}\u{438}\u{43f}\u{43e}\u{440}\u{430}\u{437}\u{43c}\u{435}\u{440}";
const RU_AI_PREFIX: &str =
    "\u{418}\u{418}: \u{441}\u{43f}\u{440}\u{43e}\u{441}\u{438}\u{442}\
    \u{44c}";
const RU_AI_MODEL: &str = "\u{41c}\u{43e}\u{434}\u{435}\u{43b}\u{44c} \u{418}\u{418}: gpt";
const RU_EDIT_WALL: &str =
    "\u{41f}\u{440}\u{430}\u{432}\u{43a}\u{430} \u{441}\u{442}\u{435}\
    \u{43d}\u{44b}";
const RU_OPEN: &str = "\u{41e}\u{442}\u{43a}\u{440}\u{44b}\u{442}\u{44c}";
const RU_LONG: &str =
    "\u{41e}\u{447}\u{435}\u{43d}\u{44c} \u{434}\u{43b}\u{438}\u{43d}\
    \u{43d}\u{430}\u{44f} \u{43f}\u{43e}\u{434}\u{43f}\u{438}\u{441}\
    \u{44c} \u{43a}\u{43e}\u{43c}\u{430}\u{43d}\u{434}\u{44b} \u{434}\
    \u{43b}\u{44f} \u{43f}\u{440}\u{43e}\u{432}\u{435}\u{440}\u{43a}\
    \u{438}";

// ---- self-tests: every check fires on a planted bad string and passes a good one ----

#[test]
fn planted_ellipsis_dots_are_found() {
    assert!(has_ascii_ellipsis("Open..."));
    assert!(!has_ascii_ellipsis("Open\u{2026}"));
}

#[test]
fn planted_spaced_hyphens_are_found() {
    assert!(spaced_hyphen("Undone - Ctrl+Y redoes"));
    assert!(spaced_hyphen("a -- b"));
    assert!(!spaced_hyphen("Undone \u{2014} Ctrl+Y redoes"));
    assert!(!spaced_hyphen("pages 3 - 5"));
    assert!(!spaced_hyphen("x = -5 and Re-pick"));
}

#[test]
fn planted_trailing_period_is_found() {
    assert!(trailing_period("Save."));
    assert!(!trailing_period("Save"));
    assert!(!trailing_period("Save as\u{2026}"));
}

#[test]
fn planted_capitals_are_found() {
    let caps = vocab().capitals;
    assert_eq!(case_problem("Print Sheets", &caps).as_deref(), Some("Sheets"));
    assert_eq!(case_problem("Export to XLSX", &caps), None);
    assert_eq!(case_problem("Export to Foo", &caps).as_deref(), Some("Foo"));
    assert_eq!(case_problem("3D view \u{b7} Level 1", &caps), None);
    assert!(case_problem(RU_NEW_WALL_BAD, &caps).is_some());
    assert!(case_problem(RU_NEW_WALL, &caps).is_none());
}

#[test]
fn planted_colon_names_are_found() {
    assert!(colon_in_name("AI: ask"));
    assert!(!colon_in_name("Ask AI"));
}

#[test]
fn planted_long_russian_labels_are_found() {
    assert!(too_long("Open file", RU_LONG));
    assert!(!too_long("Open", RU_OPEN));
}

#[test]
fn planted_glossary_variants_are_found() {
    let v = vocab();
    let hit = |lang: &str, s: &str, label: bool| {
        v.rules.iter().filter(|r| r.lang == lang).find_map(|r| forbidden_hit(s, r, label))
    };
    assert!(hit("en", "Select the bearing wall", false).is_some());
    assert!(hit("en", "AI chat", false).is_some());
    assert!(hit("en", "Assistant", false).is_some());
    assert!(hit("en", "Ask the AI assistant", false).is_none());
    assert!(hit("en", "AI: ask", false).is_some());
    assert!(hit("en", "Model AI: x", false).is_none());
    assert!(hit("ru", RU_TYPE_SIZE, false).is_some());
    assert!(hit("ru", RU_AI_PREFIX, false).is_some());
    assert!(hit("ru", RU_AI_MODEL, false).is_none());
    let edit = RU_EDIT_WALL;
    assert!(hit("ru", edit, true).is_some());
    assert!(hit("ru", edit, false).is_none());
    assert!(hit("ru", "AI", false).is_some());
}

#[test]
fn planted_label_keys_are_recognised() {
    let ids: BTreeSet<String> = ["arch.wall".to_string()].into();
    assert!(is_label_key("cmd.arch.wall", &ids));
    assert!(is_label_key("cmd.arch.wall.short", &ids));
    assert!(!is_label_key("cmd.arch.wall.desc", &ids));
    assert!(is_label_key("rgroup.build", &ids));
    assert!(is_label_key("project.btn.save", &ids));
    assert!(!is_label_key("project.damaged.prev", &ids));
}
