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
    use ddc_geom::component::{Family, Lod};

    #[test]
    fn every_pack_item_is_licensed_builds_at_real_size_and_fits_the_budget() {
        let dir = content_dir();
        let items = content_items(&dir).unwrap();
        let fams: Vec<_> = items.iter().filter(|i| i.category.is_some()).collect();
        assert!(fams.len() >= 60, "{} families", fams.len());
        let mut bytes = 0u64;
        for it in &items {
            assert!(PACK_LICENCES.contains(&it.licence.as_str()), "{}: {}", it.file, it.licence);
            bytes += std::fs::metadata(dir.join(&it.file)).unwrap().len();
            let Some(cat) = &it.category else {
                assert!(it.color.as_deref().is_some_and(|c| c.len() == 7), "{}", it.file);
                continue;
            };
            let f = Family::parse(&content_family(&dir, it).unwrap()).unwrap();
            assert_eq!(f.category.name(), cat, "{}", it.file);
            let r = f.resolve(&f.types[0].values, &[]).unwrap();
            for lod in [Lod::Coarse, Lod::Medium, Lod::Fine] {
                let b = f.build(&r, lod).unwrap();
                let (lo, hi) = (b.bbox[0], b.bbox[1]);
                let big = (hi.x - lo.x).max(hi.y - lo.y).max(hi.z - lo.z);
                // real-world scale: between a vase and a tree
                assert!(big > 0.05 && big < 30.0, "{} {big}", it.file);
            }
        }
        assert!(bytes < 40 << 20, "{bytes} bytes");
        // every file in the folder is listed
        let listed: std::collections::HashSet<_> = items.iter().map(|i| i.file.as_str()).collect();
        for e in std::fs::read_dir(&dir).unwrap() {
            let n = e.unwrap().file_name().into_string().unwrap();
            if n.ends_with(".ddcf") {
                assert!(listed.contains(n.as_str()), "{n} has no licence entry");
            }
        }
    }
}
