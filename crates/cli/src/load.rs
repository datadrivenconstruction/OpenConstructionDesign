// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Open any input the io crate reads into a private in-memory or temporary model.

use std::path::{Path, PathBuf};

use ddc_model::Model;

pub struct Loaded {
    pub model: Model,
    pub kind: &'static str,
    /// Import warnings, in the reader's words.
    pub warnings: Vec<String>,
    // dropped after `model`, which holds the file open
    _work: Option<Work>,
}

impl Loaded {
    /// A model made in memory (`ddc run --new`).
    pub fn of(model: Model) -> Loaded {
        Loaded { model, kind: "ddc", warnings: vec![], _work: None }
    }
}

/// Private working copy of a project: the user's file is only read.
struct Work(PathBuf);

impl Drop for Work {
    fn drop(&mut self) {
        ddc_model::remove_db_files(&self.0);
    }
}

pub fn ext_of(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

pub fn load(path: &Path) -> Result<Loaded, String> {
    if !path.is_file() {
        return Err(format!("file not found: {}", path.display()));
    }
    match ext_of(path).as_str() {
        "ddc" => load_project(path),
        "ifc" | "ifczip" => load_ifc(path),
        "rvt" => load_rvt(path),
        e if ddc_io::meshx::READ_EXTS.contains(&e) => load_mesh(path),
        e => Err(format!("unsupported input format: .{e}")),
    }
}

fn mem() -> Result<Model, String> {
    Model::open(":memory:").map_err(|e| e.to_string())
}

fn load_project(path: &Path) -> Result<Loaded, String> {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let work = std::env::temp_dir().join(format!("ddc-cli-{}-{n}.ddc", std::process::id()));
    let guard = Work(work.clone());
    let opened = ddc_model::open_project_copy(path, &work, false, &mut |_, _| Ok(()))
        .map_err(|e| e.to_string())?;
    let mut warnings = vec![];
    if let Some(v) = opened.upgraded_from {
        warnings.push(format!("project format v{v} upgraded in memory only"));
    }
    Ok(Loaded {
        model: opened.model,
        kind: "ddc",
        warnings,
        _work: Some(guard),
    })
}

fn load_ifc(path: &Path) -> Result<Loaded, String> {
    let mut model = mem()?;
    let rep = ddc_io::ifc::import_ifc_path(&mut model, path).map_err(|e| e.to_string())?;
    Ok(Loaded {
        model,
        kind: "ifc",
        warnings: rep.warnings,
        _work: None,
    })
}

fn load_mesh(path: &Path) -> Result<Loaded, String> {
    let mut model = mem()?;
    let (set, warnings) = ddc_io::meshx::read_file(path, None)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    ddc_io::meshx::import_into(&mut model, &set, &name, None)?;
    Ok(Loaded {
        model,
        kind: "mesh",
        warnings,
        _work: None,
    })
}

fn load_rvt(path: &Path) -> Result<Loaded, String> {
    use ddc_io::bridge as b;
    let (found, trust) = b::installed();
    let pkg = b::for_ext(&found, "rvt").ok_or_else(|| b::missing_module_message("rvt"))?;
    let o = b::RunOptions::default();
    let frames =
        b::collect_trusted(&pkg.dir, &trust, path, &o, &mut |_| {}).map_err(|e| e.to_string())?;
    let mut model = mem()?;
    b::import_frames(&mut model, path, frames).map_err(|e| e.to_string())?;
    Ok(Loaded {
        model,
        kind: "rvt",
        warnings: vec![],
        _work: None,
    })
}
