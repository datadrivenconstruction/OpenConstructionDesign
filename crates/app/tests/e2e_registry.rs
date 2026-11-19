// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Command registry net: every fragment command (`ui/data/commands.d`) has a handler in the
//! shell (`REGISTER` handlers or `WIRED`), ru+en label keys and an embedded icon; every
//! handler belongs to a registry command; literal i18n keys used by the app exist in both
//! catalogs. Headless: `EditorApp::new(.., None, ..)` needs no GPU.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ddc_app::editor::Editor;
use ddc_app::shell::{EditorApp, WIRED};
use ddc_ui::commands::{Command, Registry};
use ddc_ui::fragments::COMMAND_FRAGMENTS;
use ddc_ui::i18n::Lang;

fn app() -> EditorApp {
    EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
}

fn handled(a: &EditorApp, id: &str) -> bool {
    let r = a.sh.reg.resolve(id);
    a.handlers.contains_key(r) || WIRED.contains(&r)
}

/// Commands of the fragment files: (fragment file, command).
fn fragment_commands() -> Vec<(String, Command)> {
    let mut v = vec![];
    for (name, src) in COMMAND_FRAGMENTS {
        let r = Registry::from_json(src).unwrap_or_else(|e| panic!("{name}: {e}"));
        v.extend(r.all().iter().map(|c| (name.to_string(), c.clone())));
    }
    v
}

#[test]
fn e2e_registry_fragment_commands_have_handler_i18n_icon() {
    let a = app();
    let mut bad = vec![];
    for (file, c) in fragment_commands() {
        if !handled(&a, &c.id) {
            bad.push(format!("{file}: {} has no handler", c.id));
        }
        for lang in [Lang::Ru, Lang::En] {
            if !a.sh.cat.table(lang).contains_key(&c.label_key()) {
                bad.push(format!(
                    "{file}: {} no {lang:?} key {}",
                    c.id,
                    c.label_key()
                ));
            }
        }
        // an empty icon means "not drawn yet" (docs/design/ICONS_V4.md), a wrong name is a typo
        if !c.icon.is_empty() && ddc_ui::icons::source(&c.icon).is_none() {
            bad.push(format!("{file}: {} icon '{}' not embedded", c.id, c.icon));
        }
    }
    assert!(
        bad.is_empty(),
        "{} problems:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn e2e_registry_all_commands_have_ru_en_labels_unique_ids() {
    let a = app();
    let mut seen = BTreeMap::new();
    let mut bad = vec![];
    for c in a.sh.reg.all() {
        *seen.entry(c.id.clone()).or_insert(0) += 1;
        for lang in [Lang::Ru, Lang::En] {
            if !a.sh.cat.table(lang).contains_key(&c.label_key()) {
                bad.push(format!("{} no {lang:?} label", c.id));
            }
        }
        if let Some(al) = &c.alias {
            if a.sh.reg.get(al).is_none() {
                bad.push(format!("{} alias to unknown {al}", c.id));
            }
        }
    }
    bad.extend(
        seen.iter()
            .filter(|(_, n)| **n > 1)
            .map(|(id, n)| format!("{id} defined {n} times")),
    );
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    let open: Vec<&str> =
        a.sh.reg
            .all()
            .iter()
            .filter(|c| !handled(&a, &c.id))
            .map(|c| c.id.as_str())
            .collect();
    eprintln!(
        "registry: {} commands, {} without handler (drawn disabled)",
        a.sh.reg.all().len(),
        open.len()
    );
}

#[test]
fn e2e_registry_handlers_are_registry_commands_with_icons() {
    let a = app();
    let mut bad = vec![];
    let ids = a
        .handlers
        .keys()
        .map(|s| s.as_str())
        .chain(WIRED.iter().copied());
    for id in ids {
        match a.sh.reg.get(id) {
            None => bad.push(format!("handler {id} is not in the registry")),
            // empty = not drawn yet (docs/design/ICONS_V4.md); a wrong name still fails
            Some(c) if !c.icon.is_empty() && ddc_ui::icons::source(&c.icon).is_none() => {
                bad.push(format!("{id}: icon '{}' not embedded", c.icon))
            }
            Some(_) => {}
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// `tr("k")`, `.t("k")`, `.tf("k"`, `.plural("k"` with a literal dotted key.
fn literal_keys(src: &str) -> Vec<String> {
    let mut out = vec![];
    for pre in ["tr(\"", ".t(\"", ".tf(\"", ".plural(\""] {
        let mut s = src;
        while let Some(i) = s.find(pre) {
            // `tr(` must not be the tail of a longer identifier (`attr(`)
            let ok = pre != "tr(\""
                || i == 0
                || !s[..i]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_');
            s = &s[i + pre.len()..];
            let Some(end) = s.find('"') else { break };
            let k = &s[..end];
            let key_like = k.contains('.')
                && k.starts_with(|c: char| c.is_ascii_lowercase())
                && k.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
            if ok && key_like {
                out.push(k.to_string());
            }
        }
    }
    out
}

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn e2e_registry_literal_i18n_keys_exist_ru_en() {
    let a = app();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![];
    rust_files(&src, &mut files);
    assert!(!files.is_empty());
    let mut bad = BTreeSet::new();
    let mut n = 0;
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        for k in literal_keys(&text) {
            n += 1;
            for lang in [Lang::Ru, Lang::En] {
                let t = a.sh.cat.table(lang);
                // plural keys live as `<key>.one/.few/.many/.other`
                let plural = t
                    .range(format!("{k}.")..)
                    .next()
                    .is_some_and(|(x, _)| x.starts_with(&format!("{k}.")));
                if !t.contains_key(&k) && !plural {
                    let file = f.file_name().unwrap().to_string_lossy().to_string();
                    bad.insert(format!("{file}: {k} missing in {lang:?}"));
                }
            }
        }
    }
    assert!(n > 50, "key scan found only {n} literal keys");
    assert!(
        bad.is_empty(),
        "{}",
        bad.into_iter().collect::<Vec<_>>().join("\n")
    );
}

/// Every id the registry can show (base catalog and fragments, aliases through their
/// target) dispatches: `--press <id>` and the palette never meet an unknown command.
#[test]
fn e2e_registry_every_command_dispatches() {
    let a = app();
    let bad: Vec<String> = Registry::builtin()
        .all()
        .iter()
        .filter(|c| !handled(&a, &c.id))
        .map(|c| c.id.clone())
        .collect();
    assert!(bad.is_empty(), "{} ids without a handler: {bad:?}", bad.len());
}
