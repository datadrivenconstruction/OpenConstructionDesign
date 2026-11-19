//! Vector export of sheets to PDF: line weights in mm, RGB, layers (OCG), searchable text.
//! Our own PDF writer; font — ttf-parser/subsetter, compression — miniz_oxide.

mod book;
mod font;
pub mod import;
mod writer;

pub use book::PdfBook;
pub use writer::{
    cjk_fallback_fonts, export_pdf, export_pdf_with_font, export_pdf_with_fonts, EMBEDDED_FONT,
};

/// Export error (e.g. non-finite coordinates).
#[derive(Debug, Clone, PartialEq)]
pub struct PdfError(pub String);

impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PDF: {}", self.0)
    }
}
impl std::error::Error for PdfError {}

/// A sheet layer; becomes an optional content group (OCG).
#[derive(Clone, Debug)]
pub struct SheetLayer {
    pub name: String,
    /// Default state when opened in a viewer.
    pub visible: bool,
}

/// A sheet item. Coordinates in mm, origin bottom-left, Y up. `layer` — index into `Sheet::layers`.
#[derive(Clone, Debug)]
pub enum Item {
    Polyline {
        pts: Vec<[f64; 2]>,
        closed: bool,
        weight_mm: f64,
        rgb: [u8; 3],
        layer: usize,
    },
    Text {
        pos: [f64; 2],
        height_mm: f64,
        /// Degrees counter-clockwise.
        rotation: f64,
        string: String,
        rgb: [u8; 3],
        layer: usize,
    },
    /// Region fill by the even-odd rule: the first ring is the outline, the rest are holes.
    Fill {
        rings: Vec<Vec<[f64; 2]>>,
        rgb: [u8; 3],
        layer: usize,
    },
    /// A raster image (logo) in rectangle [x, y, w, h], mm; transparency is preserved.
    Image {
        rect: [f64; 4],
        image: std::sync::Arc<PdfImage>,
        layer: usize,
    },
}

/// An 8-bit RGBA image, rows top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Sheet {
    pub width_mm: f64,
    pub height_mm: f64,
    pub layers: Vec<SheetLayer>,
    pub items: Vec<Item>,
}

#[cfg(test)]
mod tests;
