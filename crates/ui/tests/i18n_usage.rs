// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Catalog keys against code. Three checks over the embedded locale files and every `.rs` file:
//! en and ru hold the same keys; every literal key passed to a lookup call exists; every key is
//! used somewhere. "Used" means its quoted literal appears in a source file or a command
//! fragment, or it matches a dynamic form found there (`format!("a.b.{x}")`, a literal ending in
//! `.` or `_`, a `cmd.<id>` label). Keys the scan cannot prove used are listed in `UNUSED_OK` with a reason;
//! an entry that matches nothing fails, so the list only shrinks.
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

/// Lookup calls whose first argument is the key.
const FIRST_ARG: &[&str] = &["tr", "tr_str", "msg", "t", "tf", "plural"];
/// Lookup calls whose second argument is the key (the first is a language).
const SECOND_ARG: &[&str] = &["tr_lang", "tl", "t_in"];
/// Dotted literals passed to a lookup on purpose although no catalog holds them.
const NOT_KEYS: &[(&str, &str)] = &[("no.such.key", "i18n.rs unit test of the missing-key fallback")];

const DOC: &str = "the owning module documents the key as built from a code at runtime";
const SLUG: &str = "suffix comes from a column, category or style slug table";
const OPEN: &str = "no literal use found; dynamic use not ruled out, confirm before removing";

/// Key prefixes (whole key or followed by a dot) allowed to look unused, with the reason.
const UNUSED_OK: &[(&str, &str)] = &[
    ("ai_opt_merge.left", DOC),
    ("ai_opt_merge.merged", DOC),
    ("ai_opt_merge.note", DOC),
    ("ai_opt_merge.parts", DOC),
    ("ai_opt_merge.rebased", DOC),
    ("ai_opt_merge.refused", DOC),
    ("ai_opt_merge.revive", DOC),
    ("ai_opt_merge.title", DOC),
    ("ai_opt_merge.whole", DOC),
    ("autosave.tip", OPEN),
    ("clash.err", OPEN),
    ("clash.gap", OPEN),
    ("clash.selection", OPEN),
    ("clash.tolerance", OPEN),
    ("demo.density", OPEN),
    ("demo.lang", OPEN),
    ("demo.theme", OPEN),
    ("dlg.set", OPEN),
    ("drafting_view.name", OPEN),
    ("edst.move_cancelled", OPEN),
    ("edst.not_deleted", OPEN),
    ("edst.nothing_redo", OPEN),
    ("edst.nothing_undo", OPEN),
    ("edst.opening_not_placed", OPEN),
    ("edst.opening_placed", OPEN),
    ("edst.undo_failed", OPEN),
    ("edst.undone", OPEN),
    ("edst.wall_changed", OPEN),
    ("annot_text.detail", SLUG),
    ("annot_text.legend", OPEN),
    ("annot_text.lstyle", SLUG),
    ("annot_text.tstyle", SLUG),
    ("fe.close", OPEN),
    ("fe.new", OPEN),
    ("gen.why", "rule reasons moved out of crates/gen/data/rules.json, kept for translation"),
    ("gfx.note", SLUG),
    ("hist.curtain_delete", OPEN),
    ("hist.slab_delete", OPEN),
    ("job.loaded", OPEN),
    ("job.no_module", OPEN),
    ("mcp.bar", OPEN),
    ("mcp.set", OPEN),
    ("mep3d.deleted", OPEN),
    ("mep3d.not_deleted", OPEN),
    ("opt.arc", OPEN),
    ("plan.room", OPEN),
    ("rebar.add", OPEN),
    ("rebar.schedule", OPEN),
    ("recover.archive", OPEN),
    ("recover.col", SLUG),
    ("recover.original", OPEN),
    ("report.opt", OPEN),
    ("update_check.current", OPEN),
    ("update_check.newer", OPEN),
    ("update_check.unknown", OPEN),
    ("sch.cat", SLUG),
    ("sch.col", SLUG),
    ("sch.ui", SLUG),
    ("settings.quality", OPEN),
    ("shv.fit", OPEN),
    ("solid.void_kind", SLUG),
    ("src.status", OPEN),
    ("edit_status.ed", OPEN),
    ("edit_status.mt", OPEN),
    ("edit_status.seed", OPEN),
    ("level_ask.level", OPEN),
    ("level_ask.sec", OPEN),
    ("status.filter", OPEN),
    ("status.grid", OPEN),
    ("status.keys", OPEN),
    ("status.off", OPEN),
    ("status.on", OPEN),
    ("host_shape.issue", OPEN),
    ("topo.info", OPEN),
    ("topo.unit", OPEN),
    ("edit_panels.imp", OPEN),
    ("edit_panels.we", OPEN),
    ("unit_fmt.q", OPEN),
    ("view_range.bottom", OPEN),
    ("view_range.cut", OPEN),
    ("view_range.depth", OPEN),
    ("warn.elem", OPEN),
    ("norm_dim", "translations of not-applicable dim texts in norm packs, kept for the dims panel"),
];

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let p = e.expect("dir entry").path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs")
            && p.file_name().is_some_and(|n| n != "i18n_usage.rs")
        {
            out.push(p);
        }
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// `(path, text)` of every `.rs` file in the workspace crates.
fn rs_sources() -> Vec<(String, String)> {
    let mut files = Vec::new();
    walk(&crate_dir().join(".."), &mut files);
    files.sort();
    files.iter().map(|p| (p.display().to_string(), read(p))).collect()
}

/// Command fragment texts: they name labels by id.
fn command_fragments() -> Vec<String> {
    let dir = crate_dir().join("data").join("commands.d");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files.iter().map(|p| read(p)).collect()
}

/// Every key of one locale: base files plus fragments. A file that is not valid
/// UTF-8 or not a JSON object panics here.
fn catalog_keys(lang: &str) -> BTreeSet<String> {
    let dir = crate_dir().join("data").join("i18n");
    // English keeps its base files at the root; other locales hold them in their folder.
    let mut files: Vec<PathBuf> = [format!("ui.{lang}.json"), format!("commands.{lang}.json")]
        .iter()
        .map(|n| dir.join(n))
        .filter(|p| p.exists())
        .collect();
    let mut frag: Vec<PathBuf> = std::fs::read_dir(dir.join(lang))
        .unwrap_or_else(|e| panic!("i18n/{lang}: {e}"))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    frag.sort();
    files.extend(frag);
    let mut keys = BTreeSet::new();
    for f in files {
        let m: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&read(&f)).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        keys.extend(m.into_keys());
    }
    keys
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `a.b_c`: ASCII words joined by single dots, at least one dot.
fn key_shape(s: &str) -> bool {
    s.contains('.')
        && !s.starts_with('.')
        && !s.ends_with('.')
        && !s.contains("..")
        && s.bytes().all(|b| is_ident(b) || b == b'.')
}

/// The key literal at `i` (after blanks and an optional `&`), if it has the key shape.
fn literal_at(src: &str, mut i: usize) -> Option<&str> {
    let b = src.as_bytes();
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if i < b.len() && b[i] == b'&' {
        i += 1;
    }
    if i >= b.len() || b[i] != b'"' {
        return None;
    }
    let start = i + 1;
    let end = start + src[start..].find('"')?;
    let lit = &src[start..end];
    key_shape(lit).then_some(lit)
}

/// Literal keys handed to the lookup calls in one source text.
fn called_keys(src: &str) -> Vec<&str> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    for (names, second) in [(FIRST_ARG, false), (SECOND_ARG, true)] {
        for name in names {
            let pat = format!("{name}(");
            for (at, _) in src.match_indices(&pat) {
                if at > 0 && is_ident(b[at - 1]) {
                    continue;
                }
                let mut i = at + pat.len();
                if second {
                    let s = i;
                    while i < b.len() && !matches!(b[i], b',' | b'(' | b')' | b'"') {
                        i += 1;
                    }
                    if i == s || i >= b.len() || b[i] != b',' {
                        continue;
                    }
                    i += 1;
                }
                if let Some(k) = literal_at(src, i) {
                    out.push(k);
                }
            }
        }
    }
    out
}

fn lit_byte(b: u8) -> bool {
    is_ident(b) || matches!(b, b'.' | b'{' | b'}' | b':' | b',' | b'!' | b'?' | b'-')
}

/// Quoted tokens shaped like a key or a key template (`word.` first, then key characters).
fn quoted_keys(text: &str) -> HashSet<&str> {
    let b = text.as_bytes();
    let mut out = HashSet::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < b.len() && lit_byte(b[j]) {
                j += 1;
            }
            if j < b.len() && b[j] == b'"' && j > start {
                let s = &text[start..j];
                let word = s.bytes().take_while(|c| is_ident(*c)).count();
                if word > 0 && word < s.len() && s.as_bytes()[word] == b'.' {
                    out.insert(s);
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Template split at `{...}` placeholders: `a.{x}.b` gives `["a.", ".b"]`.
fn template_parts(lit: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = lit;
    while let Some(o) = rest.find('{') {
        let Some(c) = rest[o..].find('}') else { break };
        parts.push(&rest[..o]);
        rest = &rest[o + c + 1..];
    }
    parts.push(rest);
    parts
}

/// Does `key` fit the template, every placeholder standing for one or more characters?
fn template_matches(parts: &[&str], key: &str) -> bool {
    if parts.len() < 2 || !key.starts_with(parts[0]) {
        return false;
    }
    let mut pos = parts[0].len();
    for mid in &parts[1..parts.len() - 1] {
        let from = pos + 1;
        match key.get(from..).and_then(|r| r.find(mid)) {
            Some(off) => pos = from + off + mid.len(),
            None => return false,
        }
    }
    let last = parts[parts.len() - 1];
    key.len() >= pos + 1 + last.len() && key.ends_with(last)
}

/// Command ids from the fragments: `"id": "<value>"`.
fn command_ids(fragments: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for f in fragments {
        let b = f.as_bytes();
        for (at, _) in f.match_indices("\"id\"") {
            let mut i = at + 4;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= b.len() || b[i] != b':' {
                continue;
            }
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= b.len() || b[i] != b'"' {
                continue;
            }
            if let Some(n) = f[i + 1..].find('"') {
                if n > 0 {
                    out.push(f[i + 1..i + 1 + n].to_string());
                }
            }
        }
    }
    out
}

/// Keys with no use in `text` (all sources joined) or in the command `ids`.
fn unused_keys(text: &str, ids: &[String], keys: &BTreeSet<String>) -> Vec<String> {
    let lits = quoted_keys(text);
    let mut prefixes: Vec<&str> = Vec::new();
    let mut templates: Vec<Vec<&str>> = Vec::new();
    for &l in &lits {
        let head = l.split('{').next().unwrap_or(l);
        if l.contains('{') {
            prefixes.push(head);
            let lead = head.bytes().take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_').count();
            if lead > 0 && head.as_bytes().get(lead) == Some(&b'.') {
                templates.push(template_parts(l));
            }
        }
        if l.ends_with('.') || l.ends_with('_') {
            prefixes.push(l);
        }
    }
    prefixes.retain(|p| p.len() >= 3 && p.contains('.'));
    keys.iter()
        .filter(|k| {
            let k = k.as_str();
            !(lits.contains(k)
                || prefixes.iter().any(|p| k.starts_with(p))
                || (k.starts_with("cmd.") && ids.iter().any(|i| k.starts_with(&format!("cmd.{i}"))))
                || templates.iter().any(|t| template_matches(t, k)))
        })
        .cloned()
        .collect()
}

fn allowed(key: &str) -> bool {
    UNUSED_OK
        .iter()
        .any(|(p, _)| key == *p || (key.starts_with(p) && key.as_bytes()[p.len()] == b'.'))
}

#[test]
fn english_and_russian_hold_the_same_keys() {
    let en = catalog_keys("en");
    let ru = catalog_keys("ru");
    let only_en: Vec<_> = en.difference(&ru).take(20).collect();
    let only_ru: Vec<_> = ru.difference(&en).take(20).collect();
    assert!(only_en.is_empty(), "keys missing in ru: {only_en:?}");
    assert!(only_ru.is_empty(), "keys missing in en: {only_ru:?}");
}

#[test]
fn every_literal_key_in_code_exists_in_both_languages() {
    let en = catalog_keys("en");
    let ru = catalog_keys("ru");
    let mut missing = Vec::new();
    for (path, src) in rs_sources() {
        for k in called_keys(&src) {
            if NOT_KEYS.iter().any(|(n, _)| *n == k) {
                continue;
            }
            if !en.contains(k) || !ru.contains(k) {
                missing.push(format!("{k} ({path})"));
            }
        }
    }
    assert!(missing.is_empty(), "keys used in code but absent from a locale: {missing:#?}");
}

#[test]
fn unused_keys_are_only_the_listed_ones() {
    let keys = catalog_keys("en");
    let mut texts: Vec<String> = rs_sources().into_iter().map(|(_, s)| s).collect();
    let fragments = command_fragments();
    let ids = command_ids(&fragments);
    texts.extend(fragments);
    let unused = unused_keys(&texts.join("\n"), &ids, &keys);
    let stray: Vec<_> = unused.iter().filter(|k| !allowed(k)).collect();
    assert!(
        stray.is_empty(),
        "{} keys no code uses (remove from en and ru, or list in UNUSED_OK with a reason): {:#?}",
        stray.len(),
        stray.iter().take(40).collect::<Vec<_>>()
    );
    let stale: Vec<_> = UNUSED_OK
        .iter()
        .filter(|(p, _)| !unused.iter().any(|k| k == *p || k.starts_with(&format!("{p}."))))
        .map(|(p, _)| *p)
        .collect();
    assert!(stale.is_empty(), "UNUSED_OK entries that no longer match an unused key: {stale:?}");
    assert!(UNUSED_OK.iter().all(|(_, why)| !why.is_empty()));
}

#[test]
fn scan_sees_a_planted_missing_key() {
    let src = "let a = crate::tr(\"zz.planted_missing\", &[]); let b = tl(ru, \"zz.second\"); let c = get(\"zz.skip\");";
    let en: BTreeSet<String> = BTreeSet::from(["zz.second".to_string()]);
    let missing: Vec<_> = called_keys(src).into_iter().filter(|k| !en.contains(*k)).collect();
    assert_eq!(missing, vec!["zz.planted_missing"]);
}

#[test]
fn scan_sees_a_planted_unused_key_and_honours_dynamic_forms() {
    let keys: BTreeSet<String> = ["zz.dead", "zz.live", "zz.dyn.a", "zz.mid.x.end", "cmd.zzcmd.label"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let text = "let a = c.t(\"zz.live\"); let b = format!(\"zz.dyn.{}\", n); let c = format!(\"zz.mid.{}.end\", n);";
    let unused = unused_keys(text, &["zzcmd".to_string()], &keys);
    assert_eq!(unused, vec!["zz.dead".to_string()]);
}

#[test]
fn command_ids_are_read_from_fragments() {
    let ids = command_ids(&["[{ \"id\": \"a.b\", \"x\": 1 }, {\"id\":\"c\"}]".to_string()]);
    assert_eq!(ids, vec!["a.b".to_string(), "c".to_string()]);
}
