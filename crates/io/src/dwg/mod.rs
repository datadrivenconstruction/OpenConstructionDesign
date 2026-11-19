// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Native .dwg reader (storage AC1012..AC1032, i.e. R13..R2018+). Produces the same
//! [`DxfDocument`] the DXF reader builds, so underlay and drawing-to-model work unchanged.
//! Spec: docs/specs/DWG.md. Unknown objects are skipped and counted; never panics.

mod annot;
mod bits;
mod build;
mod codec;
mod container;
mod header;
mod objects;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_roundtrip;

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use crate::dxf::DxfDocument;

/// What a [`DwgError`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DwgErrorKind {
    /// Damaged or inconsistent content.
    #[default]
    Corrupt,
    /// Not a .dwg file.
    NotDwg,
    /// AC1009 or older (R12 and before): a fixed-record layout this reader does not convert.
    Legacy,
    /// A signature newer than the known generations.
    Unsupported,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DwgError(pub String, pub DwgErrorKind);

impl DwgError {
    pub fn kind(&self) -> DwgErrorKind {
        self.1
    }
}

impl fmt::Display for DwgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DWG: {}", self.0)
    }
}
impl std::error::Error for DwgError {}

pub(crate) type R<T> = Result<T, DwgError>;

pub(crate) fn err(s: impl Into<String>) -> DwgError {
    DwgError(s.into(), DwgErrorKind::Corrupt)
}

/// Storage generation, ordered by age.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Version {
    R13,
    R14,
    R2000,
    R2004,
    R2007,
    R2010,
    R2013,
    R2018,
}

impl Version {
    pub fn magic(self) -> &'static str {
        match self {
            Version::R13 => "AC1012",
            Version::R14 => "AC1014",
            Version::R2000 => "AC1015",
            Version::R2004 => "AC1018",
            Version::R2007 => "AC1021",
            Version::R2010 => "AC1024",
            Version::R2013 => "AC1027",
            Version::R2018 => "AC1032",
        }
    }
}

/// True when the bytes start like a .dwg file (any generation, supported or not):
/// "AC1" + three digits, or the pre-R13 "AC1.x" / "AC2.x" forms.
pub fn is_dwg(bytes: &[u8]) -> bool {
    let Some(s) = bytes.get(..6) else {
        return false;
    };
    (s.starts_with(b"AC1") && s[3..].iter().all(u8::is_ascii_digit))
        || ((s.starts_with(b"AC1.") || s.starts_with(b"AC2.")) && s[4].is_ascii_digit())
}

/// Generation from the 6-byte signature; `Err` names unsupported ones.
pub fn version_of(bytes: &[u8]) -> Result<Version, DwgError> {
    let sig = bytes.get(..6).ok_or_else(|| err("file too short"))?;
    Ok(match sig {
        b"AC1012" => Version::R13,
        b"AC1014" => Version::R14,
        b"AC1015" => Version::R2000,
        b"AC1018" => Version::R2004,
        b"AC1021" => Version::R2007,
        b"AC1024" => Version::R2010,
        b"AC1027" => Version::R2013,
        b"AC1032" => Version::R2018,
        _ if is_dwg(bytes) => {
            let s = String::from_utf8_lossy(sig);
            if &*s < "AC1012" || s.starts_with("AC2.") {
                let n = r12_entity_records(bytes)
                    .map(|n| format!(", {n} entity records"))
                    .unwrap_or_default();
                return Err(DwgError(
                    format!("format {s} (R12 or older{n}) is not supported; save as R13 or newer"),
                    DwgErrorKind::Legacy,
                ));
            }
            return Err(DwgError(
                format!("format {s} is not supported (known: AC1012..AC1032)"),
                DwgErrorKind::Unsupported,
            ));
        }
        _ => return Err(DwgError("not a .dwg file".into(), DwgErrorKind::NotDwg)),
    })
}

/// Records in the entity section of an AC1009 file (bounds at 0x14, each record led by
/// kind, flags and its total length), when bounds and lengths tile the section exactly.
fn r12_entity_records(b: &[u8]) -> Option<usize> {
    if b.get(..6)? != b"AC1009" {
        return None;
    }
    let at = |o: usize| {
        b.get(o..o + 4)
            .map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]) as usize)
    };
    let (es, ee) = (at(0x14)?, at(0x18)?);
    if es < 0x80 || es > ee || ee > b.len() {
        return None;
    }
    let (mut pos, mut n) = (es, 0usize);
    while pos < ee {
        let len = u16::from_le_bytes([*b.get(pos + 2)?, *b.get(pos + 3)?]) as usize;
        if len < 6 || len > ee - pos {
            return None;
        }
        pos += len;
        n += 1;
    }
    Some(n)
}

/// What the reader did besides the document: generation, counts, skipped kinds, warnings.
#[derive(Debug, Clone, Default)]
pub struct DwgReport {
    pub version: Option<Version>,
    /// Objects in the handle map.
    pub objects: usize,
    /// Entities placed into the document (model space, paper space and blocks).
    pub entities: usize,
    /// Object kinds not converted, with counts (unknown kinds, non-graphic objects excluded).
    pub skipped: BTreeMap<String, usize>,
    /// Objects whose record could not be decoded.
    pub failed: usize,
    /// Object frames with a CRC mismatch (decoded anyway).
    pub crc_mismatches: usize,
    /// Layout (tab) names from the layout dictionary, "Model" first; empty when not found.
    pub layouts: Vec<String>,
    pub warnings: Vec<String>,
}

impl DwgReport {
    pub(crate) fn warn(&mut self, w: impl Into<String>) {
        if self.warnings.len() < 64 {
            self.warnings.push(w.into());
        }
    }
    pub(crate) fn skip(&mut self, kind: &str) {
        *self.skipped.entry(kind.to_string()).or_default() += 1;
    }
}

/// Resource limits for decoding one file; the defaults suit desktop and browser builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DwgLimits {
    /// Largest decoded page (system or data page), bytes.
    pub max_page: usize,
    /// Sum of all decoded sections, bytes (also capped at 256x the file size).
    pub max_total: usize,
}

impl Default for DwgLimits {
    fn default() -> Self {
        DwgLimits {
            max_page: 16 << 20,
            max_total: 512 << 20,
        }
    }
}

pub fn read_bytes(bytes: &[u8]) -> Result<DxfDocument, DwgError> {
    read_bytes_report(bytes).map(|(d, _)| d)
}

pub fn read_bytes_report(bytes: &[u8]) -> Result<(DxfDocument, DwgReport), DwgError> {
    read_bytes_with(bytes, &DwgLimits::default())
}

pub fn read_bytes_with(
    bytes: &[u8],
    lim: &DwgLimits,
) -> Result<(DxfDocument, DwgReport), DwgError> {
    let ver = version_of(bytes)?;
    let mut rep = DwgReport {
        version: Some(ver),
        ..Default::default()
    };
    let secs = container::read(bytes, ver, lim, &mut rep)?;
    let doc = build::document(&secs, ver, &mut rep)?;
    Ok((doc, rep))
}

/// [`read_bytes`] that polls `stop` between the stages (version, container, objects); the
/// error text of a stop is [`crate::job::CANCELLED`].
pub fn read_bytes_stop(bytes: &[u8], stop: &dyn Fn() -> bool) -> Result<DxfDocument, DwgError> {
    let gone = || err(crate::job::CANCELLED.to_string());
    let ver = version_of(bytes)?;
    let mut rep = DwgReport {
        version: Some(ver),
        ..Default::default()
    };
    if stop() {
        return Err(gone());
    }
    let secs = container::read(bytes, ver, &DwgLimits::default(), &mut rep)?;
    if stop() {
        return Err(gone());
    }
    Ok(build::document(&secs, ver, &mut rep)?)
}

pub fn read_file(path: &Path) -> Result<DxfDocument, DwgError> {
    let bytes = std::fs::read(path).map_err(|e| err(e.to_string()))?;
    read_bytes(&bytes)
}
