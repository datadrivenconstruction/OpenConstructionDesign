// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
// `ddc_dwg` is set when the native .dwg reader module is present, so the app can hide
// .dwg from file dialogs in builds without it. Words io writes into a model or a book come
// from `../ui/data/i18n/<locale>/io_words.json` (paths stay relative to the crate: checkouts
// of one source tree share one OUT_DIR).
use std::{env, fs, path::Path};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(ddc_dwg)");
    println!("cargo:rerun-if-changed=src/lib.rs");
    // A missing path is not watched (cargo would rerun forever); adding it also edits lib.rs.
    if Path::new("src/dwg").is_dir() {
        println!("cargo:rerun-if-changed=src/dwg");
    }
    if Path::new("src/dwg/mod.rs").exists() {
        println!("cargo:rustc-cfg=ddc_dwg");
    }
    let rel = "../ui/data/i18n";
    println!("cargo:rerun-if-changed={rel}");
    let root = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join(rel);
    let mut locales: Vec<String> = fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    locales.sort();
    let mut out = String::from("const WORD_FILES: &[(&str, &str)] = &[\n");
    for l in &locales {
        let f = format!("{rel}/{l}/io_words.json");
        println!("cargo:rerun-if-changed={f}");
        if root.join(l).join("io_words.json").is_file() {
            out.push_str(&format!(
                "    ({l:?}, include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/\", {f:?}))),\n"
            ));
        }
    }
    out.push_str("];\n");
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("io_words.rs"), out).unwrap();
}
