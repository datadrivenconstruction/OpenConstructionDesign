// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `elements.parquet` of a revision package (contract `ddc-elements/1`, card E2).
//!
//! `rows` reads one row per element out of the model (quantities from the same `*_qty` tables
//! the schedules and the xlsx export read), `writer` puts rows into a Parquet file and reads
//! them back. Geometry columns (bounding box, geometry hash) come from the caller, which owns
//! the meshes; elements without a mesh get a box derived from the model where it has one.

mod rows;
mod writer;

#[cfg(test)]
mod tests;

pub use rows::{collect, glb_box, short_hash, slug, Collected, Geo, GeoMap, GEOMETRY_NONE};
pub use writer::{read_parquet, write_parquet, COLUMNS, ROW_GROUP_MAX};
#[cfg(test)]
use writer::write_parquet_with;

/// One row of `elements.parquet`. `key` is written to `id`, `stable_id` and `element_key`.
#[derive(Clone, Debug, PartialEq)]
pub struct ElementRow {
    pub key: String,
    pub element_type: String,
    pub ifc_class: String,
    pub type_key: String,
    pub name: String,
    pub storey: Option<String>,
    pub space: Option<String>,
    pub discipline: String,
    pub material: Option<String>,
    pub phase: Option<String>,
    /// IfcSpace only (DIN 277 style use); `None` where the model does not record the use.
    pub space_usage: Option<String>,
    pub geometry_hash: String,
    pub length_m: Option<f64>,
    pub area_m2: Option<f64>,
    pub area_gross_m2: Option<f64>,
    pub volume_m3: Option<f64>,
    pub volume_gross_m3: Option<f64>,
    pub weight_kg: Option<f64>,
    pub count: Option<i64>,
    /// min x, y, z, max x, y, z in GLB space (Y-up, metres).
    pub bbox: [f64; 6],
    /// JSON object text of the remaining parameters.
    pub props: Option<String>,
}

/// What a writer or collector noticed and repaired or refused to guess; never silent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub key: String,
    pub field: &'static str,
    pub what: String,
}

/// The key grammar of the contract (`^[A-Za-z0-9_$][A-Za-z0-9._#/:$-]*$`, at most 255 characters).
pub fn key_ok(k: &str) -> bool {
    let mut c = k.chars();
    c.next().is_some_and(|f| f.is_ascii_alphanumeric() || f == '_' || f == '$')
        && c.all(|x| x.is_ascii_alphanumeric() || "._#/:-$".contains(x))
        && k.len() <= 255
}
