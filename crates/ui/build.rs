// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Embeds command/i18n fragments: `data/commands.d/*.json`, `data/i18n/<lang>/*.json`.
//! Help topics: `data/help/<lang>/*.md`. A feature adds a file; no shared list to edit.
//! Paths stay relative to the crate: checkouts of one source tree share one OUT_DIR and one build-script binary,
//! so an absolute path (or a compile-time `env!` here) would embed another checkout's files.
use std::{env, fs, path::Path};

/// The crate root of the checkout being built, read at run time (not baked into this binary).
fn manifest_dir() -> String {
    env::var("CARGO_MANIFEST_DIR").unwrap()
}

fn list(rel: &str) -> Vec<String> {
    list_ext(rel, "json")
}

/// Files `rel/*.ext` as crate-relative paths with `/` separators, sorted.
fn list_ext(rel: &str, ext: &str) -> Vec<String> {
    println!("cargo:rerun-if-changed={rel}");
    let dir = Path::new(&manifest_dir()).join(rel);
    let mut v: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == ext))
        .map(|p| {
            let n = p.file_name().unwrap().to_string_lossy().into_owned();
            let r = format!("{rel}/{n}");
            println!("cargo:rerun-if-changed={r}");
            r
        })
        .collect();
    v.sort();
    v
}

/// `include_str!` of a crate-relative path, resolved against the crate being compiled.
fn include(rel: &str) -> String {
    format!("include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/\", {rel:?}))")
}

fn arr(name: &str, files: &[String]) -> String {
    let items: String = files
        .iter()
        .map(|f| {
            let n = f.rsplit('/').next().unwrap_or(f);
            format!("    ({n:?}, {}),\n", include(f))
        })
        .collect();
    format!("pub const {name}: &[(&str, &str)] = &[\n{items}];\n")
}

/// The generated icon set `icons_v4/<direction>/<master>/<name>.svg` keyed
/// `direction/master/name` (tools/icons/gen.py writes it; ICONS_V4.md).
fn icons_v4(root: &str) -> String {
    let mut items: Vec<(String, String)> = Vec::new();
    for dir in ["outline", "duotone", "filled"] {
        for px in ["16", "20", "24"] {
            let rel = format!("{root}/{dir}/{px}");
            for f in list_ext(&rel, "svg") {
                let stem = Path::new(&f).file_stem().unwrap().to_string_lossy().into_owned();
                items.push((format!("{dir}/{px}/{stem}"), f));
            }
        }
    }
    let body: String = items
        .iter()
        .map(|(k, f)| format!("    ({k:?}, {}),\n", include(f)))
        .collect();
    format!("pub const ICONS_V4: &[(&str, &str)] = &[\n{body}];\n")
}

/// The ribbon set `assets/icons3/<master>/<name>.svg` keyed `master/name`
/// (tools/icons/ribbon.py writes it; ICONS_V4.md).
fn icons_ribbon(root: &str) -> String {
    let mut body = String::new();
    for px in ["16", "20", "24"] {
        for f in list_ext(&format!("{root}/{px}"), "svg") {
            let stem = Path::new(&f).file_stem().unwrap().to_string_lossy().into_owned();
            body += &format!("    ({:?}, {}),\n", format!("{px}/{stem}"), include(&f));
        }
    }
    format!("pub const ICONS_RIBBON: &[(&str, &str)] = &[\n{body}];\n")
}

/// The style pack `assets/icons_pick/<style>/<master>/<name>.svg` keyed `style/master/name`
/// (tools/icons/styles.py writes it).
fn icons_pick(root: &str) -> String {
    let mut body = String::new();
    for style in ["hierarchical", "color", "monoline", "isometric"] {
        for px in ["16", "20", "24"] {
            for f in list_ext(&format!("{root}/{style}/{px}"), "svg") {
                let stem = Path::new(&f).file_stem().unwrap().to_string_lossy().into_owned();
                body += &format!("    ({:?}, {}),\n", format!("{style}/{px}/{stem}"), include(&f));
            }
        }
    }
    format!("pub const ICONS_PICK: &[(&str, &str)] = &[\n{body}];\n")
}

/// `I18N_ALL`:`(locale code, fragments)` for every folder `data/i18n/<code>/`, sorted by code.
fn i18n_all(root: &str) -> String {
    println!("cargo:rerun-if-changed={root}");
    let dir = Path::new(&manifest_dir()).join(root);
    let mut codes: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    codes.sort();
    let mut s = String::from("pub const I18N_ALL: &[(&str, &[(&str, &str)])] = &[\n");
    for c in codes {
        let files = list(&format!("{root}/{c}"));
        let items: String = files
            .iter()
            .map(|f| {
                let n = f.rsplit('/').next().unwrap_or(f);
                format!("({n:?}, {}), ", include(f))
            })
            .collect();
        s += &format!("    ({c:?}, &[{items}]),\n");
    }
    s + "];\n"
}

fn main() {
    let mut s = arr("COMMAND_FRAGMENTS", &list("data/commands.d"));
    s += &arr("I18N_EN_FRAGMENTS", &list("data/i18n/en"));
    s += &arr("I18N_RU_FRAGMENTS", &list("data/i18n/ru"));
    s += &i18n_all("data/i18n");
    s += &arr("HELP_EN_FRAGMENTS",&list_ext("data/help/en", "md"));
    s += &arr("HELP_RU_FRAGMENTS", &list_ext("data/help/ru", "md"));
    s += &icons_v4("../../docs/design/icons_v4");
    s += &icons_ribbon("../../assets/icons3");
    s += &icons_pick("../../assets/icons_pick");
    println!("cargo:rerun-if-changed=data/icon_style.json");
    let out = Path::new(&env::var("OUT_DIR").unwrap()).join("fragments.rs");
    fs::write(out, s).unwrap();
}
