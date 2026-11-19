// `ddc_dwg` is set when the native .dwg reader module is present, so the app can hide
// .dwg from file dialogs in builds without it.
fn main() {
    println!("cargo:rustc-check-cfg=cfg(ddc_dwg)");
    println!("cargo:rerun-if-changed=src/lib.rs");
    // A missing path is not watched (cargo would rerun forever); adding it also edits lib.rs.
    if std::path::Path::new("src/dwg").is_dir() {
        println!("cargo:rerun-if-changed=src/dwg");
    }
    if std::path::Path::new("src/dwg/mod.rs").exists() {
        println!("cargo:rustc-cfg=ddc_dwg");
    }
}
