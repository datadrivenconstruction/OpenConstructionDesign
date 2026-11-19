// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Import/export. Closed formats go through an external process via `bridge`.

pub mod attach;
pub mod bcf;
pub mod bridge;
pub mod compimport;
pub mod drawing_model;
pub mod dwg;
pub mod dxf;
#[cfg(feature = "link")]
pub mod contract;
#[cfg(feature = "link")]
pub mod elements_parquet;
pub mod exact;
pub mod fsutil;
pub mod gltf;
pub mod ifc;
pub mod import_summary;
pub mod job;
pub mod logo;
#[cfg(any(test, feature = "dxf-write"))]
pub mod mesh_dxf;
pub mod meshx;
pub mod num_file;
pub mod obj;
pub mod pdf;
pub mod plan_image;
pub mod requirements;
#[doc(hidden)]
pub mod ru_fixtures;
pub mod revision;
pub mod sheets;
pub mod step;
pub mod stl;
pub mod svg;
pub mod table_csv;
pub mod threemf;
pub mod topo;
pub mod xlsx;

use std::path::Path;

/// What an importer hands to the core: only open model entities.
pub trait Sink {
    fn element(&mut self, kind: &str, params: &[(&str, f64)]) -> u64;
}

pub trait Importer: Send + Sync {
    /// Extensions without the dot, lower case.
    fn extensions(&self) -> &[&str];
    fn import(&self, path: &Path, sink: &mut dyn Sink) -> Result<(), String>;
}

#[derive(Default)]
pub struct Registry {
    importers: Vec<Box<dyn Importer>>,
}

impl Registry {
    pub fn register(&mut self, imp: Box<dyn Importer>) {
        self.importers.push(imp);
    }
    pub fn for_path(&self, path: &Path) -> Option<&dyn Importer> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        self.importers
            .iter()
            .find(|i| i.extensions().contains(&ext.as_str()))
            .map(|b| b.as_ref())
    }
}
