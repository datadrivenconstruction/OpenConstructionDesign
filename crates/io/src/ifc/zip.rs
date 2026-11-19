//! IFCZIP: a zip archive holding one `.ifc` file (stored or deflated). Only what the format
//! needs: central directory lookup, local header, raw inflate; no ZIP64, no encryption.

/// Largest inflated member accepted (hostile archives cannot exhaust memory).
const MAX_INFLATED: usize = 2 << 30;

fn u16_at(b: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as usize)
}
fn u32_at(b: &[u8], i: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?) as usize)
}

/// `.ifc` entry that is not macOS resource-fork metadata (`__MACOSX/`, `._name.ifc`).
pub(crate) fn ifczip_entry_ok(n: &str) -> bool {
    let low = n.to_ascii_lowercase();
    let base = low.rsplit(['/', '\\']).next().unwrap_or(&low);
    low.ends_with(".ifc")
        && !low.starts_with("__macosx/")
        && !low.contains("/__macosx/")
        && !base.starts_with("._")
}

pub fn is_zip(b: &[u8]) -> bool {
    b.starts_with(b"PK\x03\x04")
}

/// Bytes of the first `.ifc` member of a zip archive.
pub fn ifc_member(b: &[u8]) -> Result<Vec<u8>, String> {
    let bad = |m: &str| Err(format!("IFCZIP: {m}"));
    // end of central directory: last 22..65557 bytes
    let lo = b.len().saturating_sub(22 + 65535);
    let Some(eocd) = (lo..b.len().saturating_sub(21))
        .rev()
        .find(|&i| b.get(i..i + 4) == Some(b"PK\x05\x06"))
    else {
        return bad("no central directory");
    };
    let n = u16_at(b, eocd + 10).unwrap_or(0);
    let mut at = u32_at(b, eocd + 16).unwrap_or(usize::MAX);
    for _ in 0..n {
        if b.get(at..at + 4) != Some(b"PK\x01\x02") {
            return bad("broken central directory");
        }
        let (Some(method), Some(csize), Some(usize_), Some(nl), Some(xl), Some(cl), Some(off)) = (
            u16_at(b, at + 10),
            u32_at(b, at + 20),
            u32_at(b, at + 24),
            u16_at(b, at + 28),
            u16_at(b, at + 30),
            u16_at(b, at + 32),
            u32_at(b, at + 42),
        ) else {
            return bad("truncated entry");
        };
        let name = b.get(at + 46..at + 46 + nl).unwrap_or_default();
        at += 46 + nl + xl + cl;
        if !ifczip_entry_ok(&String::from_utf8_lossy(name)) {
            continue;
        }
        if b.get(off..off + 4) != Some(b"PK\x03\x04") {
            return bad("bad local header");
        }
        let (Some(lnl), Some(lxl)) = (u16_at(b, off + 26), u16_at(b, off + 28)) else {
            return bad("truncated local header");
        };
        let start = off + 30 + lnl + lxl;
        let Some(data) = start.checked_add(csize).and_then(|e| b.get(start..e)) else {
            return bad("truncated member");
        };
        return match method {
            0 => Ok(data.to_vec()),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(
                data,
                usize_.clamp(1, MAX_INFLATED),
            )
            .map_err(|e| format!("IFCZIP: inflate {:?}", e.status)),
            m => bad(&format!("compression method {m}")),
        };
    }
    bad("no .ifc member")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_deflated_and_stored_members() {
        let body = b"ISO-10303-21;".repeat(100);
        for method in [
            zip::CompressionMethod::Stored,
            zip::CompressionMethod::Deflated,
        ] {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            let o = zip::write::SimpleFileOptions::default().compression_method(method);
            w.start_file("readme.txt", o).unwrap();
            w.write_all(b"x").unwrap();
            w.start_file("model.IFC", o).unwrap();
            w.write_all(&body).unwrap();
            let z = w.finish().unwrap().into_inner();
            assert!(is_zip(&z));
            assert_eq!(ifc_member(&z).unwrap(), body);
            for n in 0..z.len() {
                let _ = ifc_member(&z[..n]);
            }
        }
        assert!(ifc_member(b"PK\x03\x04junk").is_err());
    }
}
