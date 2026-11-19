//! DXF ASCII: reader and an own in-memory document (independent of the model crate).
//! The text writer is test-fixture support only (`dxf-write`): the product never writes DXF.

pub(crate) mod annot;
mod bridge;
pub(crate) mod codepage;
#[cfg(any(test, feature = "dxf-write"))]
mod comp_blocks;
mod entities;
mod expand;
mod ltypes;
pub mod lower;
mod reader;
pub mod text;
#[cfg(any(test, feature = "dxf-write"))]
mod writer;

#[cfg(any(test, feature = "dxf-write"))]
pub use bridge::export_from;
pub use bridge::{dropped_kind, import_into, ImportReport};
#[cfg(any(test, feature = "dxf-write"))]
pub use comp_blocks::{append_components, attr_tag, category_layer, CompBlocksReport};
pub use entities::*;
pub use expand::{
    block_kind, expand_inserts, BlockKind, ExpandOptions, ExpandReport, Expanded, InsertHint,
    InsertInfo,
};
pub use lower::{Fill, LowerReport, PatLine};
pub use reader::{
    read_bytes, read_bytes_with, read_file, read_file_ctl, read_file_with, read_str,
    read_str_with,
};
pub use text::{decode_unicode, mtext_lines, mtext_plain, text_plain};
#[cfg(any(test, feature = "dxf-write"))]
pub use writer::{check_text_round_trip, write_file, write_string};

/// True when this build includes the native .dwg reader (`src/dwg`, detected by build.rs).
pub const DWG_SUPPORTED: bool = cfg!(ddc_dwg);

/// `.dwg` signature (`AC1015`, `AC1032`, old `AC2.10`…): binary drawing, not DXF text.
pub fn looks_like_dwg(bytes: &[u8]) -> bool {
    bytes.len() >= 6
        && bytes.starts_with(b"AC")
        && bytes[2..6].iter().all(|b| b.is_ascii_digit() || *b == b'.')
}

use std::fmt;

/// One group-code / value pair.
#[derive(Debug, Clone, PartialEq)]
pub struct Pair {
    pub code: i32,
    pub value: String,
}

impl Pair {
    pub fn new(code: i32, value: impl Into<String>) -> Self {
        Pair {
            code,
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DxfError(pub String);

impl fmt::Display for DxfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DXF: {}", self.0)
    }
}
impl std::error::Error for DxfError {}

/// Header variable (`$NAME`) with its value pairs as-is.
#[derive(Debug, Clone, PartialEq)]
pub struct HeaderVar {
    pub name: String,
    pub values: Vec<Pair>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub name: String,
    pub flags: i32,
    pub color: i32,
    pub linetype: String,
    pub extra: Vec<Pair>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LineType {
    pub name: String,
    pub description: String,
    pub flags: i32,
    pub pattern_length: f64,
    pub pattern: Vec<f64>,
    pub extra: Vec<Pair>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    pub name: String,
    pub flags: i32,
    pub height: f64,
    pub width_factor: f64,
    pub oblique: f64,
    pub font: String,
    pub big_font: String,
    pub extra: Vec<Pair>,
}

/// Table we do not model: header pairs after the name, and entries verbatim (with their code 0 pair).
#[derive(Debug, Clone, PartialEq)]
pub struct RawTable {
    pub name: String,
    pub header: Vec<Pair>,
    pub entries: Vec<Vec<Pair>>,
}

/// Section we do not model (e.g. OBJECTS, CLASSES): body pairs verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct RawSection {
    pub name: String,
    pub pairs: Vec<Pair>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub name: String,
    pub layer: String,
    pub flags: i32,
    pub base: Pt,
    pub attdefs: Vec<AttDef>,
    pub entities: Vec<Entity>,
    pub extra: Vec<Pair>,
}

impl Block {
    pub fn new(name: impl Into<String>) -> Self {
        Block {
            name: name.into(),
            layer: "0".into(),
            flags: 0,
            base: [0.0; 3],
            attdefs: vec![],
            entities: vec![],
            extra: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DxfDocument {
    pub header: Vec<HeaderVar>,
    pub ltypes: Vec<LineType>,
    pub layers: Vec<Layer>,
    pub styles: Vec<TextStyle>,
    pub other_tables: Vec<RawTable>,
    pub blocks: Vec<Block>,
    pub entities: Vec<Entity>,
    pub other_sections: Vec<RawSection>,
}

impl DxfDocument {
    fn header_pair(&self, name: &str, code: i32) -> Option<&Pair> {
        self.header
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))?
            .values
            .iter()
            .find(|p| p.code == code)
    }

    /// `$INSUNITS` (0 — unitless).
    pub fn insunits(&self) -> i32 {
        self.header_pair("$INSUNITS", 70)
            .and_then(|p| p.value.trim().parse().ok())
            .unwrap_or(0)
    }

    /// Metres per drawing unit from `$INSUNITS`; `None` when unitless or unknown.
    pub fn unit_meters(&self) -> Option<f64> {
        units_meters(self.insunits())
    }

    /// `$LTSCALE` (1 when absent).
    pub fn ltscale(&self) -> f64 {
        self.header_pair("$LTSCALE", 40)
            .and_then(|p| p.value.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(1.0)
    }

    /// `$MEASUREMENT` (0 — imperial, 1 — metric), when present.
    pub fn measurement(&self) -> Option<i32> {
        self.header_pair("$MEASUREMENT", 70)
            .and_then(|p| p.value.trim().parse().ok())
    }

    /// Layout names from the OBJECTS section (model layout first when present).
    /// The name is code 1 of the `AcDbLayout` subclass; the earlier code 1 is the page setup.
    pub fn layouts(&self) -> Vec<String> {
        let mut out = vec![];
        for s in self.other_sections.iter().filter(|s| s.name == "OBJECTS") {
            // 0 — outside LAYOUT, 1 — in LAYOUT before AcDbLayout, 2 — inside AcDbLayout.
            let mut st = 0u8;
            for p in &s.pairs {
                match p.code {
                    0 => st = u8::from(p.value == "LAYOUT"),
                    100 if st == 1 && p.value == "AcDbLayout" => st = 2,
                    1 if st == 2 => {
                        if !p.value.is_empty() && !out.contains(&p.value) {
                            out.push(p.value.clone());
                        }
                        st = 0;
                    }
                    _ => {}
                }
            }
        }
        out.sort_by_key(|n| !n.eq_ignore_ascii_case("Model"));
        out
    }

    /// Entities that live in paper space (code 67 = 1) in the ENTITIES section.
    pub fn paper_count(&self) -> usize {
        self.entities.iter().filter(|e| lower::in_paper(e)).count()
    }
}

/// Metres per unit for an `$INSUNITS` code.
pub fn units_meters(code: i32) -> Option<f64> {
    Some(match code {
        1 => 0.0254,
        2 => 0.3048,
        3 => 1609.344,
        4 => 0.001,
        5 => 0.01,
        6 => 1.0,
        7 => 1000.0,
        8 => 0.0254e-6,
        9 => 0.0254e-3,
        10 => 0.9144,
        11 => 1e-10,
        12 => 1e-9,
        13 => 1e-6,
        14 => 0.1,
        15 => 10.0,
        16 => 100.0,
        17 => 1e9,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
