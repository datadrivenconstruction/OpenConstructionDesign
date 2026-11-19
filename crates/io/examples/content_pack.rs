// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Builds the bundled content pack from downloaded CC0 sources.
//! `cargo run -p ddc-io --example content_pack -- <raw dir> <out dir>`
//! raw dir: `manifest_raw.json` (id, name, category, hosting?, licence...) and `dl/<id>/<id>.gltf`;
//! materials: `acg_manifest.json` and `acg/<id>.jpg`. Writes `<out>/<file>.ddcf`,
//! `<out>/materials/*` and `<out>/LICENCES.json` (one entry per item, with its source).

use std::path::Path;

use ddc_geom::component::from_mesh::{family_text, ident, ImportSpec};
use ddc_geom::component::{Category, Hosting};
use ddc_io::compimport::{read_path, Axes};
use serde_json::{json, Value};

fn main() -> Result<(), String> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let [raw, out] = [&a[0], &a[1]].map(Path::new);
    let rd = |p: &str| -> Result<Value, String> {
        let t = std::fs::read_to_string(raw.join(p)).map_err(|e| format!("{p}: {e}"))?;
        serde_json::from_str(&t).map_err(|e| format!("{p}: {e}"))
    };
    std::fs::create_dir_all(out.join("materials")).map_err(|e| e.to_string())?;
    let mut lic = vec![];
    for it in rd("manifest_raw.json")?.as_array().ok_or("manifest")? {
        let id = it["id"].as_str().ok_or("id")?;
        let name = it["name"].as_str().ok_or("name")?;
        let cat = Category::parse(it["category"].as_str().ok_or("category")?).ok_or("category")?;
        let src = read_path(&raw.join("dl").join(id).join(format!("{id}.gltf")), Axes::GLTF)
            .map_err(|e| format!("{id}: {e}"))?;
        let mut spec = ImportSpec::new(name, cat);
        spec.hosting = match it["hosting"].as_str() {
            Some("ceiling") => Hosting::Ceiling,
            Some("wall") => Hosting::Wall,
            _ => spec.hosting,
        };
        spec.props = vec![
            ("Licence".into(), it["licence"].as_str().unwrap_or("").into()),
            ("ContentId".into(), id.into()),
        ];
        let text = family_text(&src.parts, &spec).map_err(|e| format!("{id}: {e}"))?;
        let file = format!("{}.ddcf", ident(name).to_lowercase());
        std::fs::write(out.join(&file), &text).map_err(|e| e.to_string())?;
        let mut e = it.clone();
        e["file"] = json!(file);
        e["warnings"] = json!(src.warnings);
        lic.push(e);
        eprintln!("{file}: {} bytes", text.len());
    }
    for it in rd("acg_manifest.json")?.as_array().ok_or("materials")? {
        let tex = it["texture"].as_str().ok_or("texture")?;
        std::fs::copy(raw.join("acg").join(tex), out.join("materials").join(tex))
            .map_err(|e| e.to_string())?;
        let mut e = it.clone();
        e["file"] = json!(format!("materials/{tex}"));
        lic.push(e);
    }
    let t = serde_json::to_string_pretty(&lic).map_err(|e| e.to_string())?;
    std::fs::write(out.join("LICENCES.json"), t + "\n").map_err(|e| e.to_string())
}
