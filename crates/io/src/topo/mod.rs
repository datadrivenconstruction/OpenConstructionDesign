// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Terrain sources for toposolids: survey point files (CSV/TSV/XYZ), DXF contours, 3D polylines,
//! faces and points, IFC site/terrain bodies. Output is plain points, breaklines and a boundary;
//! the model builds the surface.

mod dxf;
mod ifc;
mod units;
mod xyz;

pub use dxf::{dxf_layers, dxf_unit, from_dxf, DxfLayer, DxfTopoOptions};
pub use ifc::from_ifc;
pub use units::{
    comment_unit, guess, guess_by_extent, implausible, span, split_unit, Span, SurveyUnit,
    UnitGuess, UnitSource,
};
pub use xyz::{
    parse_xyz, parse_xyz_with, preview_xyz, xyz_unit, ColumnOrder, Sep, XyzOptions, XyzPreview,
};

use std::collections::HashMap;
use std::path::Path;

/// Points, breaklines (contours, 3D polylines) and boundary ring (ccw; empty = convex hull).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TopoSource {
    pub points: Vec<(f64, f64, f64)>,
    pub breaklines: Vec<Vec<(f64, f64, f64)>>,
    pub boundary: Vec<(f64, f64)>,
    /// Rows or entities that could not be read.
    pub skipped: usize,
    /// Of `skipped`: rows with a number that reads two ways (`1,250`) in a file with no mark.
    pub ambiguous: usize,
    /// Points read through digit-group separators (`1 250,5`, `1,234.5`).
    pub grouped: usize,
    /// Entities left out by the drawing filter (flat at z = 0).
    pub filtered: usize,
}

impl TopoSource {
    /// Total vertex count (points and breakline vertices).
    pub fn vertex_count(&self) -> usize {
        self.points.len() + self.breaklines.iter().map(Vec::len).sum::<usize>()
    }
}

/// Source format by file extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Xyz,
    Dxf,
    Ifc,
}

/// Extensions the import dialog offers.
pub const EXTENSIONS: [&str; 7] = ["csv", "txt", "xyz", "pts", "tsv", "dxf", "ifc"];

pub fn kind_of(path: &Path) -> Option<SourceKind> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "csv" | "txt" | "xyz" | "pts" | "tsv" => Some(SourceKind::Xyz),
        "dxf" => Some(SourceKind::Dxf),
        "ifc" => Some(SourceKind::Ifc),
        _ => None,
    }
}

/// Reading choices of the import dialog.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadOptions {
    pub xyz: XyzOptions,
    pub dxf: DxfTopoOptions,
}

/// Reads a terrain source by extension (csv/txt/xyz/pts/tsv, dxf, ifc) with default options.
pub fn read_file(path: &Path) -> Result<TopoSource, String> {
    read_file_with(path, &ReadOptions::default())
}

/// Reads a terrain source by extension with the dialog's choices.
pub fn read_file_with(path: &Path, o: &ReadOptions) -> Result<TopoSource, String> {
    let Some(kind) = kind_of(path) else {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        return Err(format!("unsupported terrain file: .{ext}"));
    };
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    match kind {
        SourceKind::Xyz => parse_xyz_with(&String::from_utf8_lossy(&bytes), &o.xyz),
        SourceKind::Dxf => {
            let doc = crate::dxf::read_bytes(&bytes).map_err(|e| e.to_string())?;
            dxf_source(&doc, &o.dxf)
        }
        SourceKind::Ifc => from_ifc(&bytes),
    }
}

/// Terrain of a drawing; an error when the chosen layers give nothing.
pub fn dxf_source(doc: &crate::dxf::DxfDocument, o: &DxfTopoOptions) -> Result<TopoSource, String> {
    let src = from_dxf(doc, o);
    if src.vertex_count() == 0 {
        return Err(if src.filtered > 0 {
            format!(
                "no elevated terrain in the drawing ({} entities at z = 0 left out)",
                src.filtered
            )
        } else {
            "no terrain points in the drawing".into()
        });
    }
    Ok(src)
}

/// Merges points closer than `tol` (first wins); returns unique points and the index map.
pub(crate) fn dedupe(pts: &[(f64, f64, f64)], tol: f64) -> (Vec<(f64, f64, f64)>, Vec<usize>) {
    let key = |v: f64| (v / tol).round() as i64;
    let mut seen: HashMap<(i64, i64, i64), usize> = HashMap::new();
    let mut out = vec![];
    let mut map = Vec::with_capacity(pts.len());
    for &p in pts {
        let k = (key(p.0), key(p.1), key(p.2));
        let i = *seen.entry(k).or_insert_with(|| {
            out.push(p);
            out.len() - 1
        });
        map.push(i);
    }
    (out, map)
}
