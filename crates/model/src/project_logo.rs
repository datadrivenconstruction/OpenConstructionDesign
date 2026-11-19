// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Company logo of the project: one PNG kept as property `png` of `project_logo` row 0, so it
//! travels inside the project file. The model only guards the container (PNG signature, size);
//! decoding, scaling and the friendly errors live where the file is picked.

use rusqlite::types::Value;

use crate::{Error, Model};

const KIND: &str = "project_logo";
const KEY: &str = "png";
const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Largest stored logo, bytes: the picker downscales long before this.
pub const MAX_LOGO_BYTES: usize = 1 << 20;

/// (width, height) from the IHDR chunk of a PNG; None when the bytes are not a PNG.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || bytes[..8] != PNG_SIG || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let be = |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let (w, h) = (be(16), be(20));
    (w > 0 && h > 0).then_some((w, h))
}

impl Model {
    /// The project's logo as PNG bytes; None: no logo.
    pub fn project_logo(&self) -> Result<Option<Vec<u8>>, Error> {
        for (key, v, _) in self.props(KIND, 0)? {
            if let (KEY, Value::Blob(b)) = (key.as_str(), v) {
                return Ok(Some(b));
            }
        }
        Ok(None)
    }

    /// Sets or removes (`None`) the logo in one undo step. `false`: nothing differs.
    pub fn set_project_logo(&mut self, png: Option<&[u8]>) -> Result<bool, Error> {
        if let Some(b) = png {
            if png_size(b).is_none() {
                return Err(Error::Invalid("project logo: not a PNG".into()));
            }
            if b.len() > MAX_LOGO_BYTES {
                return Err(Error::Invalid("project logo: file too large".into()));
            }
        }
        if self.project_logo()?.as_deref() == png {
            return Ok(false);
        }
        let value = png.map(|b| (Value::Blob(b.to_vec()), ""));
        self.set_prop(KIND, 0, KEY, value)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2x1 PNG header with a valid signature and IHDR; enough for the container checks.
    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = PNG_SIG.to_vec();
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        v
    }

    #[test]
    fn roundtrip_undo_and_no_op() {
        let mut m = Model::open(":memory:").unwrap();
        assert_eq!(m.project_logo().unwrap(), None);
        let p = png(2, 1);
        assert!(m.set_project_logo(Some(&p)).unwrap());
        assert_eq!(m.project_logo().unwrap().as_deref(), Some(p.as_slice()));
        assert!(!m.set_project_logo(Some(&p)).unwrap());
        assert!(m.set_project_logo(None).unwrap());
        assert_eq!(m.project_logo().unwrap(), None);
        m.undo().unwrap();
        assert_eq!(m.project_logo().unwrap().as_deref(), Some(p.as_slice()));
        m.undo().unwrap();
        assert_eq!(m.project_logo().unwrap(), None);
    }

    #[test]
    fn refuses_non_png_and_oversize() {
        let mut m = Model::open(":memory:").unwrap();
        assert!(m.set_project_logo(Some(b"GIF89a....")).is_err());
        let mut big = png(2, 1);
        big.resize(MAX_LOGO_BYTES + 1, 0);
        assert!(m.set_project_logo(Some(&big)).is_err());
        assert_eq!(m.project_logo().unwrap(), None);
    }

    #[test]
    fn png_size_reads_the_header() {
        assert_eq!(png_size(&png(300, 120)), Some((300, 120)));
        assert_eq!(png_size(&png(0, 5)), None);
        assert_eq!(png_size(b"short"), None);
    }

    #[test]
    fn logo_survives_save_as_copy() {
        let dir = std::env::temp_dir().join(format!("ddc_logo_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.ddc");
        let mut m = Model::open(path.to_str().unwrap()).unwrap();
        let p = png(8, 8);
        m.set_project_logo(Some(&p)).unwrap();
        drop(m);
        let m = Model::open(path.to_str().unwrap()).unwrap();
        assert_eq!(m.project_logo().unwrap().as_deref(), Some(p.as_slice()));
        drop(m);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
