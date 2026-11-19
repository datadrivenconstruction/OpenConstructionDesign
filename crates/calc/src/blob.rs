//! Run blob format (design 1.4): `DDCB`, u32 version, u32 header length, a JSON header (keys
//! table and layout), then little-endian f64 values. A run stays readable after the model
//! changes because the header carries the keys.

use serde_json::Value as J;

const MAGIC: &[u8; 4] = b"DDCB";
const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct Blob {
    pub header: J,
    pub data: Vec<f64>,
}

pub fn write_blob(header: &J, data: &[f64]) -> Vec<u8> {
    let h = header.to_string();
    let mut out = Vec::with_capacity(12 + h.len() + 8 * data.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(h.len() as u32).to_le_bytes());
    out.extend_from_slice(h.as_bytes());
    for x in data {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

pub fn read_blob(b: &[u8]) -> Option<Blob> {
    if b.len() < 12 || &b[..4] != MAGIC {
        return None;
    }
    let ver = u32::from_le_bytes(b[4..8].try_into().ok()?);
    let hl = u32::from_le_bytes(b[8..12].try_into().ok()?) as usize;
    if ver != VERSION || b.len() < 12 + hl || (b.len() - 12 - hl) % 8 != 0 {
        return None;
    }
    let header = serde_json::from_slice(&b[12..12 + hl]).ok()?;
    let data = b[12 + hl..]
        .chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap_or([0; 8])))
        .collect();
    Some(Blob { header, data })
}
