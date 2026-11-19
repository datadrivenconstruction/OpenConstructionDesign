// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Bundled content pack (`data/content`): families converted from public-domain models and
//! material swatches, each listed with its licence in `LICENCES.json`.

use std::path::{Path, PathBuf};

use crate::Error;

/// One pack item: family file or material swatch.
#[derive(Clone, Debug, PartialEq)]
pub struct PackItem {
    pub name: String,
    /// Path relative to the pack folder.
    pub file: String,
    /// Family category (`None` for materials).
    pub category: Option<String>,
    pub licence: String,
    /// Swatch colour `#RRGGBB` (materials).
    pub color: Option<String>,
}

/// Pack folder in the source tree.
pub fn content_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("data").join("content")
}

/// Items listed in `<dir>/LICENCES.json`.
pub fn content_items(dir: &Path) -> Result<Vec<PackItem>, Error> {
    let t = std::fs::read_to_string(dir.join("LICENCES.json"))
        .map_err(|e| Error::Invalid(format!("content pack: {e}")))?;
    let v: serde_json::Value =
        serde_json::from_str(&t).map_err(|e| Error::Invalid(format!("content pack: {e}")))?;
    let s = |e: &serde_json::Value, k: &str| e[k].as_str().map(str::to_string);
    v.as_array()
        .ok_or_else(|| Error::Invalid("content pack: list expected".into()))?
        .iter()
        .map(|e| {
            Ok(PackItem {
                name: s(e, "name").ok_or_else(|| Error::Invalid("content pack: name".into()))?,
                file: s(e, "file").ok_or_else(|| Error::Invalid("content pack: file".into()))?,
                category: s(e, "category"),
                licence: s(e, "licence").unwrap_or_default(),
                color: s(e, "color"),
            })
        })
        .collect()
}

/// Definition text of a pack family.
pub fn content_family(dir: &Path, item: &PackItem) -> Result<String, Error> {
    if item.file.contains("..") || !item.file.ends_with(".ddcf") {
        return Err(Error::Invalid(format!("content pack: {} is not a family", item.file)));
    }
    std::fs::read_to_string(dir.join(&item.file))
        .map_err(|e| Error::Invalid(format!("content pack: {}: {e}", item.file)))
}

/// Licences allowed in the pack.
pub const PACK_LICENCES: [&str; 2] = ["CC0-1.0", "CC-BY-4.0"];

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::component::from_mesh::{family_text, ImportSpec, MeshPart};
    use ddc_geom::component::meshbody::MeshBody;
    use ddc_geom::component::{Category, Family, Lod};
    use ddc_geom::v3;
    use serde_json::json;

    /// Every rule a pack must keep: permissive licence, swatch colour, family category, real
    /// size at every detail level, size budget, no unlisted family file.
    fn check_pack(dir: &Path, min_families: usize) -> Result<(), String> {
        let items = content_items(dir).map_err(|e| e.to_string())?;
        let fams = items.iter().filter(|i| i.category.is_some()).count();
        if fams < min_families {
            return Err(format!("{fams} families"));
        }
        let mut bytes = 0u64;
        for it in &items {
            if !PACK_LICENCES.contains(&it.licence.as_str()) {
                return Err(format!("{}: licence {:?}", it.file, it.licence));
            }
            bytes += std::fs::metadata(dir.join(&it.file)).map_err(|e| format!("{}: {e}", it.file))?.len();
            let Some(cat) = &it.category else {
                if !it.color.as_deref().is_some_and(|c| c.len() == 7) {
                    return Err(format!("{}: swatch colour", it.file));
                }
                continue;
            };
            let f = Family::parse(&content_family(dir, it).map_err(|e| e.to_string())?)
                .map_err(|e| format!("{}: {e}", it.file))?;
            if f.category.name() != cat {
                return Err(format!("{}: category {} listed as {cat}", it.file, f.category.name()));
            }
            let r = f.resolve(&f.types[0].values, &[]).map_err(|e| format!("{}: {e}", it.file))?;
            for lod in [Lod::Coarse, Lod::Medium, Lod::Fine] {
                let b = f.build(&r, lod).map_err(|e| format!("{}: {e}", it.file))?;
                let (lo, hi) = (b.bbox[0], b.bbox[1]);
                let big = (hi.x - lo.x).max(hi.y - lo.y).max(hi.z - lo.z);
                // real-world scale: between a vase and a tree
                if !(big > 0.05 && big < 30.0) {
                    return Err(format!("{}: {big} m across", it.file));
                }
            }
        }
        if bytes >= 40 << 20 {
            return Err(format!("{bytes} bytes"));
        }
        let listed: std::collections::HashSet<_> = items.iter().map(|i| i.file.as_str()).collect();
        for e in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let n = e.map_err(|e| e.to_string())?.file_name().into_string().unwrap_or_default();
            if n.ends_with(".ddcf") && !listed.contains(n.as_str()) {
                return Err(format!("{n} has no licence entry"));
            }
        }
        Ok(())
    }

    fn box_body(w: f64, d: f64, h: f64) -> MeshBody {
        let p = |x: f64, y: f64, z: f64| v3(x * w, y * d, z * h);
        let pos = vec![
            p(0., 0., 0.),
            p(1., 0., 0.),
            p(1., 1., 0.),
            p(0., 1., 0.),
            p(0., 0., 1.),
            p(1., 0., 1.),
            p(1., 1., 1.),
            p(0., 1., 1.),
        ];
        let tris = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        MeshBody::new(pos, tris).unwrap()
    }

    /// A pack written the way `examples/content_pack` writes one (family_text per mesh, one
    /// LICENCES.json entry per item): three families and one swatch.
    fn fixture_pack(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ddc-pack-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("materials")).unwrap();
        let mut lic = vec![];
        for (name, cat, (w, d, h)) in [
            ("Crate", Category::Furniture, (0.6, 0.4, 0.45)),
            ("Planter", Category::Generic, (1.2, 0.5, 0.8)),
            ("Bench", Category::Furniture, (1.8, 0.45, 0.45)),
        ] {
            let parts = vec![MeshPart { material: "wood".into(), body: box_body(w, d, h) }];
            let text = family_text(&parts, &ImportSpec::new(name, cat)).unwrap();
            let file = format!("{}.ddcf", name.to_lowercase());
            std::fs::write(dir.join(&file), text).unwrap();
            lic.push(json!({"name": name, "file": file, "category": cat.name(), "licence": "CC0-1.0"}));
        }
        std::fs::write(dir.join("materials/oak.jpg"), [0xFFu8, 0xD8, 0xFF, 0xD9]).unwrap();
        lic.push(json!({"name": "Oak", "file": "materials/oak.jpg", "licence": "CC-BY-4.0", "color": "#A07040"}));
        std::fs::write(dir.join("LICENCES.json"), serde_json::to_string(&lic).unwrap()).unwrap();
        dir
    }

    fn rewrite_licences(dir: &Path, f: impl FnOnce(&mut Vec<serde_json::Value>)) {
        let p = dir.join("LICENCES.json");
        let mut v: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        f(&mut v);
        std::fs::write(p, serde_json::to_string(&v).unwrap()).unwrap();
    }

    #[test]
    fn a_built_pack_is_licensed_builds_at_real_size_and_fits_the_budget() {
        let dir = fixture_pack("ok");
        assert_eq!(check_pack(&dir, 3), Ok(()));
        assert!(check_pack(&dir, 4).unwrap_err().contains("3 families"));
        let _ = std::fs::remove_dir_all(&dir);
        // each rule refuses its own breach
        let broken = |tag: &str, f: &dyn Fn(&Path)| {
            let d = fixture_pack(tag);
            f(&d);
            let e = check_pack(&d, 3).unwrap_err();
            let _ = std::fs::remove_dir_all(&d);
            e
        };
        let e = broken("lic", &|d| rewrite_licences(d, |v| v[1]["licence"] = json!("CC-BY-NC-4.0")));
        assert!(e.contains("planter.ddcf: licence"), "{e}");
        let e = broken("col", &|d| rewrite_licences(d, |v| v[3]["color"] = json!("brown")));
        assert!(e.contains("swatch colour"), "{e}");
        let e = broken("cat", &|d| rewrite_licences(d, |v| v[0]["category"] = json!("door")));
        assert!(e.contains("crate.ddcf: category"), "{e}");
        let e = broken("unl", &|d| {
            std::fs::copy(d.join("crate.ddcf"), d.join("stray.ddcf")).unwrap();
        });
        assert!(e.contains("stray.ddcf has no licence entry"), "{e}");
        let e = broken("big", &|d| {
            let parts = vec![MeshPart { material: "wood".into(), body: box_body(40.0, 1.0, 1.0) }];
            let t = family_text(&parts, &ImportSpec::new("Bench", Category::Furniture)).unwrap();
            std::fs::write(d.join("bench.ddcf"), t).unwrap();
        });
        assert!(e.contains("bench.ddcf: 40"), "{e}");
    }

    /// The bundled pack itself, built locally by `examples/content_pack` from inputs that are not
    /// in the repository: `cargo test -p ddc-model --lib -- --ignored bundled_pack`.
    #[test]
    #[ignore = "needs data/content, built by examples/content_pack"]
    fn bundled_pack_keeps_every_rule() {
        assert_eq!(check_pack(&content_dir(), 60), Ok(()));
    }
}
