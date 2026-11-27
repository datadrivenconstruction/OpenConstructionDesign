// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Names of catalog keys and fragment files. Rules live in `data/i18n_names.json`:
//! no work id (`id_pattern` on the first key segment and on file-name parts, `deep_id_pattern`
//! on later key parts) and no product name (SHA-256 of lowercase words, from that file and from
//! `tools/name_lint/terms.txt`). Every locale holds only keys and files English has (a missing
//! key falls back to the base language and English). Allowlist entries must still match a name,
//! so the lists only shrink. Keys used by code exist in English: see `i18n_usage.rs`.
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

fn data() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data")
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn json_keys(p: &Path) -> BTreeSet<String> {
    let m: BTreeMap<String, serde_json::Value> =
        serde_json::from_str(&read(p)).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    m.into_keys().collect()
}

fn fragment_stems(lang: &str) -> BTreeSet<String> {
    let dir = data().join("i18n").join(lang);
    std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect()
}

fn locales() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(data().join("i18n"))
        .expect("i18n dir")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.is_dir())
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Keys of one locale: its root base files (en only) plus its fragments.
fn keys(lang: &str) -> BTreeSet<String> {
    let i18n = data().join("i18n");
    let mut out = BTreeSet::new();
    for base in [format!("ui.{lang}.json"), format!("commands.{lang}.json")] {
        let p = i18n.join(base);
        if p.exists() {
            out.extend(json_keys(&p));
        }
    }
    for s in fragment_stems(lang) {
        out.extend(json_keys(&i18n.join(lang).join(format!("{s}.json"))));
    }
    out
}

struct Rules {
    id: Regex,
    deep: Regex,
    id_ok: HashSet<String>,
    hashes: HashSet<String>,
    reasons: BTreeMap<String, String>,
    allow_prefixes: Vec<(String, String)>,
    allow_keys: Vec<(String, String)>,
    allow_files: Vec<(String, String)>,
}

fn pairs(v: &serde_json::Value, field: &str) -> Vec<(String, String)> {
    v[field]
        .as_array()
        .unwrap_or_else(|| panic!("i18n_names.json: {field} is not a list"))
        .iter()
        .map(|e| (e[0].as_str().unwrap().to_string(), e[1].as_str().unwrap().to_string()))
        .collect()
}

fn rules() -> Rules {
    let v: serde_json::Value = serde_json::from_str(&read(&data().join("i18n_names.json"))).expect("i18n_names.json");
    let strs = |f: &str| -> Vec<String> {
        v[f].as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect()
    };
    let terms = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/name_lint/terms.txt");
    let mut hashes: HashSet<String> = strs("product_sha256").into_iter().collect();
    hashes.extend(
        read(&terms)
            .lines()
            .filter_map(|l| l.trim().strip_prefix("w:"))
            .map(str::to_string),
    );
    Rules {
        id: Regex::new(v["id_pattern"].as_str().unwrap()).expect("id_pattern"),
        deep: Regex::new(v["deep_id_pattern"].as_str().unwrap()).expect("deep_id_pattern"),
        id_ok: strs("id_ok").into_iter().collect(),
        hashes,
        reasons: v["reasons"]
            .as_object()
            .map(|m| m.iter().map(|(k, s)| (k.clone(), s.as_str().unwrap().to_string())).collect())
            .unwrap_or_default(),
        allow_prefixes: pairs(&v, "allow_key_prefixes"),
        allow_keys: pairs(&v, "allow_keys"),
        allow_files: pairs(&v, "allow_files"),
    }
}

fn sha_hex(word: &str) -> String {
    Sha256::digest(word.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// A lowercase word of the name hashes to a banned product name.
fn product(name: &str, hashes: &HashSet<String>) -> bool {
    static WORD: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let word = WORD.get_or_init(|| Regex::new("[a-z][a-z0-9]*").unwrap());
    let low = name.to_lowercase();
    word.find_iter(&low).any(|w| hashes.contains(&sha_hex(w.as_str())))
}

fn is_id(r: &Rules, part: &str) -> bool {
    !r.id_ok.contains(part) && r.id.is_match(part)
}

fn bad_key(r: &Rules, k: &str) -> bool {
    let mut segs = k.split('.');
    let head = segs.next().unwrap_or("");
    let deep = segs.any(|s| s.split('_').any(|t| r.deep.is_match(t)));
    is_id(r, head) || deep || product(k, &r.hashes)
}

fn bad_file(r: &Rules, stem: &str) -> bool {
    stem.split(['-', '_']).any(|p| is_id(r, p)) || product(stem, &r.hashes)
}

#[test]
fn every_locale_holds_only_english_keys() {
    let en = keys("en");
    let mut extra = Vec::new();
    for loc in locales().iter().filter(|l| *l != "en") {
        let n = keys(loc).difference(&en).count();
        if n > 0 {
            extra.push(format!("{loc}: {n}"));
        }
    }
    assert!(extra.is_empty(), "keys English does not have: {extra:?}");
}

/// Values of one locale, as `keys` reads them.
fn values(lang: &str) -> BTreeMap<String, serde_json::Value> {
    let i18n = data().join("i18n");
    let mut files: Vec<PathBuf> = [format!("ui.{lang}.json"), format!("commands.{lang}.json")]
        .into_iter()
        .map(|b| i18n.join(b))
        .filter(|p| p.exists())
        .collect();
    files.extend(fragment_stems(lang).into_iter().map(|s| i18n.join(lang).join(format!("{s}.json"))));
    let mut out = BTreeMap::new();
    for p in files {
        let m: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&read(&p)).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        out.extend(m);
    }
    out
}

/// One product name in every language: a string that names the product in English names it
/// the same way in each locale (a stale translation keeps an old name for years).
#[test]
fn every_locale_names_the_product_as_english_does() {
    const PRODUCT: &str = "OpenConstructionDesign";
    let en = values("en");
    let named: Vec<&String> = en.iter().filter(|(_, v)| v.to_string().contains(PRODUCT)).map(|(k, _)| k).collect();
    assert!(!named.is_empty(), "no English string names {PRODUCT}");
    let mut bad = Vec::new();
    for loc in locales().iter().filter(|l| *l != "en") {
        let have = values(loc);
        for k in &named {
            if let Some(v) = have.get(*k) {
                if !v.to_string().contains(PRODUCT) {
                    bad.push(format!("{loc}/{k}: {v}"));
                }
            }
        }
    }
    assert!(bad.is_empty(), "{} strings name the product otherwise: {:?}", bad.len(), &bad[..bad.len().min(20)]);
}

#[test]
fn every_locale_file_has_an_english_twin() {
    // A locale's `ui.json` and `commands.json` are its copies of the base files `ui.en.json`, `commands.en.json`.
    let mut en = fragment_stems("en");
    en.extend(["ui".to_string(), "commands".to_string()]);
    let mut extra = Vec::new();
    for loc in locales().iter().filter(|l| *l != "en") {
        extra.extend(fragment_stems(loc).difference(&en).map(|s| format!("{loc}/{s}.json")));
    }
    assert!(extra.is_empty(), "fragments without an English file: {extra:?}");
}

#[test]
fn key_names_carry_no_work_id_or_product_name() {
    let r = rules();
    let pre: HashSet<&str> = r.allow_prefixes.iter().map(|p| p.0.as_str()).collect();
    let one: HashSet<&str> = r.allow_keys.iter().map(|p| p.0.as_str()).collect();
    let bad: Vec<String> = keys("en")
        .into_iter()
        .filter(|k| bad_key(&r, k))
        .filter(|k| !pre.contains(k.split('.').next().unwrap()) && !one.contains(k.as_str()))
        .collect();
    assert!(bad.is_empty(), "{} keys break data/i18n_names.json: {:?}", bad.len(), &bad[..bad.len().min(20)]);
}

#[test]
fn file_names_carry_no_work_id_or_product_name() {
    let r = rules();
    let allow: HashSet<&str> = r.allow_files.iter().map(|p| p.0.as_str()).collect();
    let bad: Vec<String> =
        fragment_stems("en").into_iter().filter(|s| bad_file(&r, s) && !allow.contains(s.as_str())).collect();
    assert!(bad.is_empty(), "fragment names break data/i18n_names.json: {bad:?}");
}

#[test]
fn allowlist_entries_are_still_needed_and_explained() {
    let r = rules();
    let en = keys("en");
    let stems = fragment_stems("en");
    let mut stale = Vec::new();
    for (p, why) in &r.allow_prefixes {
        if !r.reasons.contains_key(why) {
            stale.push(format!("prefix {p}: unknown reason {why}"));
        }
        if !en.iter().any(|k| k.split('.').next() == Some(p.as_str()) && bad_key(&r, k)) {
            stale.push(format!("prefix {p}: no offending key left"));
        }
    }
    for (k, why) in &r.allow_keys {
        if !r.reasons.contains_key(why) || !en.contains(k) || !bad_key(&r, k) {
            stale.push(format!("key {k}"));
        }
    }
    for (s, why) in &r.allow_files {
        if !r.reasons.contains_key(why) || !stems.contains(s) || !bad_file(&r, s) {
            stale.push(format!("file {s}"));
        }
    }
    assert!(stale.is_empty(), "drop these allowlist entries: {stale:?}");
}

#[test]
fn rules_catch_planted_names() {
    let mut r = rules();
    for k in ["ux67i.title", "g8.cube", "a.b", "xl.msg.xs6_extra", "tl2fam.done"] {
        assert!(bad_key(&r, k), "{k} should be caught");
    }
    for k in ["v3d.cube", "mep3d.removed", "ux.m2_unit", "sched.ifc4x3", "view_gfx.title"] {
        assert!(!bad_key(&r, k), "{k} should pass");
    }
    assert!(bad_file(&r, "ux67i") && bad_file(&r, "ai1_tiers") && bad_file(&r, "b"));
    assert!(!bad_file(&r, "view_gfx") && !bad_file(&r, "mep3d") && !bad_file(&r, "ai-mcp"));
    r.hashes.insert(sha_hex("zzplanted"));
    assert!(bad_key(&r, "tut.zzplanted.title") && bad_file(&r, "zzplanted_win"));
    assert!(!bad_key(&r, "tut.spreadsheet.title"));
}
