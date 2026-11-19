// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Revision packages (`.ddcrev`, format `ddc-revision/1`) as written by the project link: a zip
//! with `manifest.json` and the full model `model.ddc` beside exports. Comparing against a
//! revision needs only the model; it is unpacked with size limits into a work folder.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Manifest format this reader understands.
pub const FORMAT: &str = "ddc-revision/1";
/// File extension of a revision package.
pub const EXT: &str = "ddcrev";
const MODEL_ENTRY: &str = "model.ddc";
const MANIFEST_ENTRY: &str = "manifest.json";
const MAX_MANIFEST: u64 = 1 << 20;
/// A model larger than this is refused rather than filling the disk.
pub const MAX_MODEL: u64 = 8 << 30;

#[derive(Debug)]
pub enum RevisionError {
    /// Not a zip, no manifest or no model inside.
    NotRevision(String),
    /// A manifest of another format (the value found).
    Format(String),
    TooLarge,
    Io(std::io::Error),
}

impl std::fmt::Display for RevisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRevision(s) => write!(f, "not a revision package: {s}"),
            Self::Format(s) => write!(f, "revision format '{s}' is not supported"),
            Self::TooLarge => write!(f, "revision model is larger than {MAX_MODEL} bytes"),
            Self::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for RevisionError {}

impl From<std::io::Error> for RevisionError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// What the manifest says about the revision (all optional but the format).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RevisionInfo {
    pub format: String,
    pub label: Option<String>,
    pub created: Option<String>,
}

/// True when the path names a revision package by extension.
pub fn is_revision(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXT))
}

fn text(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| v.get(*k)?.as_str().map(str::to_string))
}

/// Reads the manifest and unpacks the model into `work/model.ddc`.
pub fn unpack_model(src: &Path, work: &Path) -> Result<(PathBuf, RevisionInfo), RevisionError> {
    let mut z = zip::ZipArchive::new(File::open(src)?)
        .map_err(|e| RevisionError::NotRevision(e.to_string()))?;
    let info = {
        let mut m = z
            .by_name(MANIFEST_ENTRY)
            .map_err(|_| RevisionError::NotRevision(MANIFEST_ENTRY.into()))?;
        if m.size() > MAX_MANIFEST {
            return Err(RevisionError::NotRevision(MANIFEST_ENTRY.into()));
        }
        let mut buf = Vec::new();
        m.by_ref().take(MAX_MANIFEST + 1).read_to_end(&mut buf)?;
        let v: serde_json::Value = serde_json::from_slice(&buf)
            .map_err(|e| RevisionError::NotRevision(e.to_string()))?;
        let format = text(&v, &["format"]).unwrap_or_default();
        if format != FORMAT {
            return Err(RevisionError::Format(format));
        }
        RevisionInfo {
            format,
            label: text(&v, &["label", "name", "message"]),
            created: text(&v, &["created", "created_at", "time"]),
        }
    };
    let mut f = z
        .by_name(MODEL_ENTRY)
        .map_err(|_| RevisionError::NotRevision(MODEL_ENTRY.into()))?;
    if f.size() > MAX_MODEL {
        return Err(RevisionError::TooLarge);
    }
    std::fs::create_dir_all(work)?;
    let out = work.join(MODEL_ENTRY);
    let mut w = File::create(&out)?;
    let n = std::io::copy(&mut f.by_ref().take(MAX_MODEL + 1), &mut w)?;
    w.flush()?;
    drop(w);
    if n > MAX_MODEL {
        let _ = std::fs::remove_file(&out);
        return Err(RevisionError::TooLarge);
    }
    Ok((out, info))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    fn dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ddc-rev-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn package(p: &Path, manifest: &str, model: Option<&[u8]>) {
        let mut z = zip::ZipWriter::new(File::create(p).unwrap());
        let o = SimpleFileOptions::default();
        z.start_file(MANIFEST_ENTRY, o).unwrap();
        z.write_all(manifest.as_bytes()).unwrap();
        if let Some(m) = model {
            z.start_file(MODEL_ENTRY, o).unwrap();
            z.write_all(m).unwrap();
        }
        z.start_file("diff.json", o).unwrap();
        z.write_all(b"{}").unwrap();
        z.finish().unwrap();
    }

    #[test]
    fn the_model_comes_out_byte_for_byte() {
        let d = dir("ok");
        let src = d.join("r.ddcrev");
        let body: Vec<u8> = (0..70_000u32).map(|i| (i * 7 % 251) as u8).collect();
        package(
            &src,
            r#"{"format":"ddc-revision/1","label":"R3","created":"2026-11-01T10:00:00Z"}"#,
            Some(&body),
        );
        assert!(is_revision(&src));
        let (out, info) = unpack_model(&src, &d.join("work")).unwrap();
        assert_eq!(std::fs::read(out).unwrap(), body);
        assert_eq!(info.label.as_deref(), Some("R3"));
        assert_eq!(info.created.as_deref(), Some("2026-11-01T10:00:00Z"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn foreign_or_incomplete_packages_are_refused() {
        let d = dir("bad");
        let src = d.join("r.ddcrev");
        package(&src, r#"{"format":"ddc-revision/9"}"#, Some(b"x"));
        assert!(matches!(unpack_model(&src, &d), Err(RevisionError::Format(f)) if f == "ddc-revision/9"));
        package(&src, r#"{"format":"ddc-revision/1"}"#, None);
        assert!(matches!(unpack_model(&src, &d), Err(RevisionError::NotRevision(_))));
        std::fs::write(&src, b"plain text").unwrap();
        assert!(matches!(unpack_model(&src, &d), Err(RevisionError::NotRevision(_))));
        assert!(!is_revision(&d.join("r.ddc")));
        let _ = std::fs::remove_dir_all(d);
    }
}
