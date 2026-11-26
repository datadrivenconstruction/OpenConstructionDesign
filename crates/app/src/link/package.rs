// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The `.ddcrev` zip: fixed entry order and timestamps, so equal contents give equal bytes.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::glb::hex;

pub fn sha256(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

/// `manifest.json` first, then `files` in the order given. Parquet and GLB are stored when the
/// caller says so (`stored`), everything else is deflated.
pub fn zip_bytes(entries: &[(String, Vec<u8>)], stored: &dyn Fn(&str) -> bool) -> Result<Vec<u8>, String> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in entries {
        let method = if stored(name) { zip::CompressionMethod::Stored } else { zip::CompressionMethod::Deflated };
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(method)
            .last_modified_time(zip::DateTime::default())
            .unix_permissions(0o644)
            .large_file(data.len() as u64 > u32::MAX as u64 / 2);
        w.start_file(name.as_str(), o).map_err(|e| format!("{name}: {e}"))?;
        w.write_all(data).map_err(|e| format!("{name}: {e}"))?;
    }
    Ok(w.finish().map_err(|e| e.to_string())?.into_inner())
}

/// Inflated megabytes one package may hold in memory unless `DDC_PACKAGE_MAX_MB` says otherwise.
const MAX_TOTAL_MB: u64 = 512;
const MAX_ENTRIES: usize = 10_000;
/// Inflated bytes per packed byte an entry may reach once it is past [`RATIO_FROM`]: deflate of
/// real model data stays far below; a zip bomb is far above.
const MAX_RATIO: u64 = 200;
const RATIO_FROM: u64 = 1 << 20;
/// First buffer of an entry: the declared size is the writer's claim, so it only caps this.
const START_CAP: u64 = 64 << 10;

/// Inflated bytes one package may hold in memory.
pub fn max_total() -> u64 {
    std::env::var("DDC_PACKAGE_MAX_MB")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(MAX_TOTAL_MB)
        .saturating_mul(1 << 20)
}

/// Every entry of the package at `path`, by name.
pub fn read_all(path: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    read_capped(path, max_total())
}

/// [`read_all`] that refuses more than `max` inflated bytes in all: the sizes in a zip are the
/// writer's claim, so a forged one must not size an allocation or let a bomb fill memory.
fn read_capped(path: &Path, max: u64) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file_len = f.metadata().map_or(u64::MAX, |m| m.len());
    read_zip(std::io::BufReader::new(f), &path.display().to_string(), file_len, max)
}

/// Every entry of a package held in memory (the browser has no files), by name, with the same
/// limits as [`read_all`].
pub fn read_all_bytes(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    read_zip(std::io::Cursor::new(bytes), "package", bytes.len() as u64, max_total())
}

fn read_zip<R: Read + std::io::Seek>(r: R, label: &str, file_len: u64, max: u64) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut z = zip::ZipArchive::new(r).map_err(|e| format!("{label}: not a zip: {e}"))?;
    if z.len() > MAX_ENTRIES {
        return Err(format!("{label}: {} entries, more than the {MAX_ENTRIES} a package may hold", z.len()));
    }
    let limit = |name: &str| format!("{label}: entry '{name}' goes past the package limit of {} MiB", max >> 20);
    let mut out = BTreeMap::new();
    let mut left = max;
    let mut chunk = vec![0u8; 64 << 10];
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(|e| e.to_string())?;
        if e.is_dir() {
            continue;
        }
        let name = e.name().map_err(|err| format!("{label}: bad entry name: {err}"))?.into_owned();
        if name.contains("..") || name.starts_with('/') || name.contains('\\') {
            return Err(format!("{label}: unsafe entry name '{name}'"));
        }
        if e.size() > left {
            return Err(limit(&name));
        }
        // the packed size is a claim too: the file length bounds what can really be packed
        let packed = e.compressed_size().min(file_len).max(1);
        let mut buf = Vec::with_capacity(e.size().min(START_CAP) as usize);
        loop {
            let n = e.read(&mut chunk).map_err(|e| format!("{name}: {e}"))?;
            if n == 0 {
                break;
            }
            if (buf.len() + n) as u64 > left {
                return Err(limit(&name));
            }
            buf.extend_from_slice(&chunk[..n]);
            let got = buf.len() as u64;
            if got > RATIO_FROM && got > packed.saturating_mul(MAX_RATIO) {
                return Err(format!(
                    "{label}: entry '{name}' inflates more than {MAX_RATIO} times its packed size (a zip bomb?)"
                ));
            }
        }
        // many entries each holding a first buffer would add up: keep only what came in
        buf.shrink_to_fit();
        left -= buf.len() as u64;
        if out.insert(name.clone(), buf).is_some() {
            return Err(format!("{label}: entry '{name}' appears twice"));
        }
    }
    Ok(out)
}

/// Re-reads a package and checks it against its own manifest: every listed file present with the
/// listed hash and size, nothing unlisted. Returns the manifest.
pub fn verify(path: &Path) -> Result<Value, String> {
    verify_entries(&read_all(path)?)
}

/// [`verify`] of a package held in memory.
pub fn verify_bytes(bytes: &[u8]) -> Result<Value, String> {
    verify_entries(&read_all_bytes(bytes)?)
}

fn verify_entries(all: &BTreeMap<String, Vec<u8>>) -> Result<Value, String> {
    let m: Value = serde_json::from_slice(all.get("manifest.json").ok_or("no manifest.json")?).map_err(|e| format!("manifest.json: {e}"))?;
    let mut listed = std::collections::BTreeSet::from(["manifest.json".to_string()]);
    for f in m["files"].as_array().ok_or("manifest has no files list")? {
        let p = f["path"].as_str().ok_or("a file entry has no path")?;
        let data = all.get(p).ok_or_else(|| format!("{p} is listed but missing"))?;
        if sha256(data) != f["sha256"].as_str().unwrap_or_default() {
            return Err(format!("{p}: sha256 differs from the manifest"));
        }
        if data.len() as u64 != f["bytes"].as_u64().unwrap_or(u64::MAX) {
            return Err(format!("{p}: size differs from the manifest"));
        }
        listed.insert(p.to_string());
    }
    if let Some(x) = all.keys().find(|k| !listed.contains(*k)) {
        return Err(format!("{x} is in the package but not in the manifest"));
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pack(extra: &[(&str, &[u8])], bad_hash: bool) -> Vec<u8> {
        let mut files = vec![];
        let mut entries: Vec<(String, Vec<u8>)> = vec![];
        for (n, d) in extra {
            let h = if bad_hash { "0".repeat(64) } else { sha256(d) };
            files.push(json!({"path": n, "role": "source", "sha256": h, "bytes": d.len()}));
            entries.push((n.to_string(), d.to_vec()));
        }
        entries.insert(0, ("manifest.json".into(), json!({"files": files}).to_string().into_bytes()));
        zip_bytes(&entries, &|n| n.ends_with(".parquet")).unwrap()
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-pkg-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&d);
        d
    }

    #[test]
    fn equal_input_gives_equal_bytes_and_verifies() {
        let a = pack(&[("model.ddc", b"abc"), ("elements.parquet", b"xyz")], false);
        assert_eq!(a, pack(&[("model.ddc", b"abc"), ("elements.parquet", b"xyz")], false));
        let p = tmp("ok.zip");
        std::fs::write(&p, &a).unwrap();
        verify(&p).unwrap();
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_wrong_hash_a_missing_file_and_an_extra_file_are_refused() {
        let p = tmp("bad.zip");
        std::fs::write(&p, pack(&[("model.ddc", b"abc")], true)).unwrap();
        assert!(verify(&p).unwrap_err().contains("sha256"));
        // extra entry
        let mut e: Vec<(String, Vec<u8>)> = vec![("manifest.json".into(), json!({"files": []}).to_string().into_bytes()), ("x.bin".into(), vec![1])];
        std::fs::write(&p, zip_bytes(&e, &|_| false).unwrap()).unwrap();
        assert!(verify(&p).unwrap_err().contains("not in the manifest"));
        // listed but missing
        e[0].1 = json!({"files": [{"path": "gone.bin", "role": "source", "sha256": "0", "bytes": 1}]}).to_string().into_bytes();
        e.pop();
        std::fs::write(&p, zip_bytes(&e, &|_| false).unwrap()).unwrap();
        assert!(verify(&p).unwrap_err().contains("missing"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_package_in_memory_verifies_like_the_file() {
        let good = pack(&[("model.ddc", b"abc")], false);
        assert_eq!(read_all_bytes(&good).unwrap()["model.ddc"], b"abc");
        verify_bytes(&good).unwrap();
        assert!(verify_bytes(&pack(&[("model.ddc", b"abc")], true)).unwrap_err().contains("sha256"));
        assert!(read_all_bytes(b"not a zip").is_err());
    }

    #[test]
    fn not_a_zip_is_an_error_not_a_panic() {
        let p = tmp("junk.zip");
        std::fs::write(&p, b"not a zip").unwrap();
        assert!(read_all(&p).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn sec_package_over_the_byte_limit_is_refused() {
        let p = tmp("big.zip");
        let e = vec![("a.bin".to_string(), vec![7u8; 1000]), ("b.bin".to_string(), vec![8u8; 1000])];
        std::fs::write(&p, zip_bytes(&e, &|_| false).unwrap()).unwrap();
        assert_eq!(read_capped(&p, 2000).unwrap().len(), 2);
        assert!(read_capped(&p, 1999).unwrap_err().contains("limit"));
        assert!(read_capped(&p, 500).unwrap_err().contains("limit"));
        let _ = std::fs::remove_file(&p);
    }

    /// A forged uncompressed size (4 GiB) must not size the buffer: an error or the real bytes.
    #[test]
    fn sec_forged_entry_size_does_not_allocate_it() {
        let p = tmp("forged.zip");
        let mut b = zip_bytes(&[("a.bin".to_string(), vec![1u8; 10])], &|_| true).unwrap();
        let find = |b: &[u8], sig: [u8; 4]| b.windows(4).position(|w| w == sig).unwrap();
        let local = find(&b, [0x50, 0x4b, 0x03, 0x04]);
        b[local + 22..local + 26].copy_from_slice(&u32::MAX.to_le_bytes());
        let central = find(&b, [0x50, 0x4b, 0x01, 0x02]);
        b[central + 24..central + 28].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&p, &b).unwrap();
        if let Ok(all) = read_capped(&p, 1 << 20) {
            assert!(all.values().all(|v| v.len() <= 10));
        }
        let _ = std::fs::remove_file(&p);
    }

    /// Sets the uncompressed size of every entry (local and central headers) to `size`.
    fn forge_sizes(b: &mut [u8], size: u32) {
        let mut i = 0;
        while i + 4 <= b.len() {
            match b[i..i + 4] {
                [0x50, 0x4b, 0x03, 0x04] => b[i + 22..i + 26].copy_from_slice(&size.to_le_bytes()),
                [0x50, 0x4b, 0x01, 0x02] => b[i + 24..i + 28].copy_from_slice(&size.to_le_bytes()),
                _ => {}
            }
            i += 1;
        }
    }

    fn many(n: usize) -> Vec<(String, Vec<u8>)> {
        (0..n).map(|i| (format!("e{i}.bin"), vec![(i % 251) as u8])).collect()
    }

    #[test]
    fn sec_more_than_max_entries_is_refused() {
        let p = tmp("entries.zip");
        std::fs::write(&p, zip_bytes(&many(MAX_ENTRIES + 1), &|_| true).unwrap()).unwrap();
        assert!(read_capped(&p, u64::MAX).unwrap_err().contains("more than"));
        std::fs::write(&p, zip_bytes(&many(MAX_ENTRIES), &|_| true).unwrap()).unwrap();
        assert_eq!(read_capped(&p, u64::MAX).unwrap().len(), MAX_ENTRIES);
        let _ = std::fs::remove_file(&p);
    }

    /// Ten thousand one-byte entries that each claim almost 4 GiB: nothing is reserved for the
    /// claim, so the buffers hold what came in, not entries times a capacity.
    #[test]
    fn sec_many_entries_with_a_fake_size_reserve_nothing() {
        let p = tmp("fake-many.zip");
        let mut b = zip_bytes(&many(MAX_ENTRIES), &|_| true).unwrap();
        forge_sizes(&mut b, 0xFFFF_FFF0);
        std::fs::write(&p, &b).unwrap();
        match read_capped(&p, u64::MAX) {
            Ok(all) => {
                assert_eq!(all.len(), MAX_ENTRIES);
                assert!(all.values().all(|v| v.len() == 1 && v.capacity() < 1024));
            }
            Err(e) => assert!(!e.is_empty()),
        }
        // under the default cap the claim alone is refused
        assert!(read_capped(&p, max_total()).unwrap_err().contains("limit"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn sec_a_ratio_bomb_is_refused_and_plain_data_is_not() {
        let p = tmp("bomb.zip");
        std::fs::write(&p, zip_bytes(&[("z.bin".to_string(), vec![0u8; 8 << 20])], &|_| false).unwrap()).unwrap();
        let e = read_capped(&p, u64::MAX).unwrap_err();
        assert!(e.contains("inflates more than"), "{e}");
        // 2 MiB that do not pack well read through
        let mut x = 0x2545_f491_u32;
        let noise: Vec<u8> = (0..2 << 20)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        std::fs::write(&p, zip_bytes(&[("n.bin".to_string(), noise.clone())], &|_| false).unwrap()).unwrap();
        assert_eq!(read_capped(&p, u64::MAX).unwrap()["n.bin"], noise);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn the_default_total_cap_is_a_real_limit() {
        if std::env::var_os("DDC_PACKAGE_MAX_MB").is_none() {
            assert_eq!(max_total(), 512 << 20);
        }
    }
}
