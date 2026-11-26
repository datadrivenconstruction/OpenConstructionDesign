//! Default room and level names come from the UI catalogs: every locale folder of
//! `../ui/data/i18n` is a locale a spec may ask for, and its `gen_names.json`, when there is
//! one, is embedded, and so is every word list of `data/lang/<locale>/`. Paths stay relative to the crate (see the ui build script): checkouts
//! of one source tree share one OUT_DIR, so the generated code resolves them against the crate being compiled.
use std::{env, fs, path::Path};

fn main() {
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
    let mut out = String::from("const LOCALES: &[&str] = &[");
    for l in &locales {
        out.push_str(&format!("{l:?}, "));
    }
    out.push_str("];\nconst NAME_FILES: &[(&str, &str)] = &[\n");
    for l in &locales {
        let f = format!("{rel}/{l}/gen_names.json");
        println!("cargo:rerun-if-changed={f}");
        if root.join(l).join("gen_names.json").is_file() {
            out.push_str(&format!(
                "    ({l:?}, include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/\", {f:?}))),\n"
            ));
        }
    }
    out.push_str("];\n");
    // word lists of every locale folder of data/lang: (locale, file stem, text)
    let words = "data/lang";
    println!("cargo:rerun-if-changed={words}");
    let mut files: Vec<(String, String)> = vec![];
    let dir = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join(words);
    for d in fs::read_dir(dir).into_iter().flatten().flatten().filter(|e| e.path().is_dir()) {
        let loc = d.file_name().to_string_lossy().into_owned();
        for f in fs::read_dir(d.path()).into_iter().flatten().flatten() {
            if let Some(stem) = f.file_name().to_string_lossy().strip_suffix(".json") {
                files.push((loc.clone(), stem.to_string()));
            }
        }
    }
    files.sort();
    out.push_str("const WORD_FILES: &[(&str, &str, &str)] = &[\n");
    for (loc, stem) in &files {
        let f = format!("{words}/{loc}/{stem}.json");
        println!("cargo:rerun-if-changed={f}");
        out.push_str(&format!(
            "    ({loc:?}, {stem:?}, include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/\", {f:?}))),\n"
        ));
    }
    out.push_str("];\n");
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("gen_names.rs"), out).unwrap();
}
