//! Refusal texts of every UI locale: each `../ui/data/i18n/<locale>/ai_refusal.json` is
//! embedded. Paths stay relative to the crate (see the gen build script): checkouts of one
//! source tree share one OUT_DIR, so the generated code resolves them against the crate being compiled.
use std::{env, fs, path::Path};

fn main() {
    let rel = "../ui/data/i18n";
    println!("cargo:rerun-if-changed={rel}");
    let root = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join(rel);
    let mut locales: Vec<String> = fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("ai_refusal.json").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    // English first: the fallback of every lookup
    locales.sort_by_key(|l| (l != "en", l.clone()));
    let mut out = String::from("const BUNDLES: &[(&str, &str)] = &[\n");
    for l in &locales {
        let f = format!("{rel}/{l}/ai_refusal.json");
        println!("cargo:rerun-if-changed={f}");
        out.push_str(&format!(
            "    ({l:?}, include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/\", {f:?}))),\n"
        ));
    }
    out.push_str("];\n");
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("refusal_bundles.rs"), out).unwrap();
}
