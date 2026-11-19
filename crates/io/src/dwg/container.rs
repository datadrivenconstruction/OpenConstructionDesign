//! Container layer: locates the header-variable, class, handle-map and object sections
//! for the three storage families (spec: docs/specs/DWG.md §3).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use super::bits::{le16, le32, le64};
use super::codec::{expand2004, expand2007, rs_data};
use super::{err, DwgLimits, DwgReport, Version, R};

#[derive(Debug)]
pub(crate) struct Sections<'a> {
    pub header: Vec<u8>,
    pub classes: Vec<u8>,
    pub handles: Vec<u8>,
    /// Handle-map offsets index into this (whole file for R13..R2000).
    pub objects: Cow<'a, [u8]>,
    pub codepage: u16,
}

pub(crate) fn read<'a>(
    raw: &'a [u8],
    ver: Version,
    lim: &DwgLimits,
    rep: &mut DwgReport,
) -> R<Sections<'a>> {
    match ver {
        Version::R13 | Version::R14 | Version::R2000 => r2000(raw),
        Version::R2007 => r2007(raw, lim, rep),
        _ => r2004(raw, lim, rep),
    }
}

/// Decoding budget shared by all sections of one file.
struct Budget {
    page: usize,
    total: usize,
    used_bytes: usize,
    used_pages: HashSet<i64>,
}

impl Budget {
    fn new(raw: &[u8], lim: &DwgLimits) -> Self {
        Budget {
            page: lim.max_page.max(1),
            total: lim.max_total.min(cap(raw)),
            used_bytes: 0,
            used_pages: HashSet::new(),
        }
    }

    fn section(&mut self, name: &str, size: usize) -> R<()> {
        self.used_bytes = self
            .used_bytes
            .checked_add(size)
            .filter(|&t| t <= self.total)
            .ok_or_else(|| err(format!("section {name}: decoded size budget exceeded")))?;
        Ok(())
    }

    /// Each physical page may feed at most one logical page (no reuse, no cycles).
    fn claim(&mut self, name: &str, pid: i64) -> R<()> {
        if !self.used_pages.insert(pid) {
            return Err(err(format!("{name}: page {pid} referenced twice")));
        }
        Ok(())
    }
}

fn slice(raw: &[u8], at: usize, n: usize) -> R<&[u8]> {
    at.checked_add(n)
        .and_then(|e| raw.get(at..e))
        .ok_or_else(|| err("section outside the file"))
}

fn r2000(raw: &[u8]) -> R<Sections<'_>> {
    let codepage = le16(raw, 0x13)?;
    let n = le32(raw, 0x15)? as usize;
    if n > 32 {
        return Err(err("bad section locator count"));
    }
    let (mut header, mut classes, mut handles) = (None, None, None);
    for i in 0..n {
        let at = 0x19 + 9 * i;
        let num = *raw.get(at).ok_or_else(|| err("file header truncated"))?;
        let off = le32(raw, at + 1)? as usize;
        let size = le32(raw, at + 5)? as usize;
        let slot = match num {
            0 => &mut header,
            1 => &mut classes,
            2 => &mut handles,
            _ => continue,
        };
        *slot = Some(slice(raw, off, size)?.to_vec());
    }
    Ok(Sections {
        header: header.ok_or_else(|| err("header section missing"))?,
        classes: classes.unwrap_or_default(),
        handles: handles.ok_or_else(|| err("object map missing"))?,
        objects: Cow::Borrowed(raw),
        codepage,
    })
}

const SECTION_NAMES: [&str; 4] = [
    "AcDb:Header",
    "AcDb:Classes",
    "AcDb:Handles",
    "AcDb:AcDbObjects",
];

/// System page (page map or section map): 20-byte header, then compressed data.
fn system_page(raw: &[u8], at: usize, magic: u32, max_page: usize) -> R<Vec<u8>> {
    if le32(raw, at)? != magic {
        return Err(err("system page signature mismatch"));
    }
    let usize_ = le32(raw, at + 4)? as usize;
    let csize = le32(raw, at + 8)? as usize;
    let ctype = le32(raw, at + 12)?;
    if usize_ > max_page {
        return Err(err("system page too large"));
    }
    let data = slice(raw, at + 20, csize)?;
    if ctype == 2 {
        expand2004(data, usize_)
    } else {
        Ok(data[..data.len().min(usize_)].to_vec())
    }
}

/// Appends `data` at logical `off` of `out`. Pages must come in order without overlap
/// (a gap left by a missing page is zero-filled); `size` is already charged to the budget.
fn place(out: &mut Vec<u8>, off: usize, data: &[u8], size: usize) -> R<()> {
    if off < out.len() || off >= size {
        return Err(err("section pages overlap or exceed the section"));
    }
    let take = data.len().min(size - off);
    out.resize(off, 0);
    out.extend_from_slice(&data[..take]);
    Ok(())
}

/// The 0x6C-byte file header of the 2004 family, stored XOR-ed with an LCG stream.
pub(crate) fn decrypt_header(enc: &[u8]) -> [u8; 0x6C] {
    let mut hdr = [0u8; 0x6C];
    let mut seed: u32 = 1;
    for (i, b) in enc.iter().take(0x6C).enumerate() {
        seed = seed.wrapping_mul(0x343FD).wrapping_add(0x269EC3);
        hdr[i] = b ^ (seed >> 16) as u8;
    }
    hdr
}

fn r2004<'a>(raw: &'a [u8], lim: &DwgLimits, rep: &mut DwgReport) -> R<Sections<'a>> {
    let mut bud = Budget::new(raw, lim);
    let hdr = decrypt_header(slice(raw, 0x80, 0x6C)?);
    if &hdr[..11] != b"AcFssFcAJMB" {
        return Err(err("file header signature mismatch (damaged file)"));
    }
    if le32(&hdr, 0x58)? != 0 {
        return Err(err("page map address out of range"));
    }
    let map_at = (le32(&hdr, 0x54)? as usize).saturating_add(0x100);
    if map_at.saturating_add(20) > raw.len() {
        return Err(err("page map address out of range"));
    }
    let smap_id = le32(&hdr, 0x5C)? as i64;
    let pm = system_page(raw, map_at, 0x4163_0E3B, bud.page)?;
    let mut pages = HashMap::new();
    let (mut pos, mut addr) = (0usize, 0x100usize);
    while pos + 8 <= pm.len() {
        let id = le32(&pm, pos)? as i32 as i64;
        let size = le32(&pm, pos + 4)? as usize;
        pos += 8;
        if id < 0 {
            pos += 16;
        } else {
            if size < 20 {
                return Err(err(format!("page map: page {id} smaller than its header")));
            }
            if pages.insert(id, addr).is_some() {
                return Err(err(format!("page map: duplicate page {id}")));
            }
        }
        addr = addr.saturating_add(size);
    }
    let sm_at = *pages
        .get(&smap_id)
        .ok_or_else(|| err("section map page missing"))?;
    let sm = system_page(raw, sm_at, 0x4163_003B, bud.page)?;
    // The page map and the section map are system pages: data sections may not reuse them.
    bud.used_pages.insert(smap_id);
    let count = le32(&sm, 0)? as usize;
    let codepage = le16(raw, 0x13)?;
    let mut found: [Option<Vec<u8>>; 4] = Default::default();
    let mut names = HashSet::new();
    let mut pos = 20usize;
    for _ in 0..count.min(4096) {
        let size = le64(&sm, pos)? as usize;
        let npages = le32(&sm, pos + 8)? as usize;
        let max = le32(&sm, pos + 12)? as usize;
        let compressed = le32(&sm, pos + 20)?;
        let encrypted = le32(&sm, pos + 28)?;
        let name_raw = slice(&sm, pos + 32, 64)?;
        let name_end = name_raw.iter().position(|&b| b == 0).unwrap_or(64);
        let name = String::from_utf8_lossy(&name_raw[..name_end]).to_string();
        pos += 96;
        // Each entry is 16 bytes of the section map; the count cannot exceed what is left.
        if npages > sm.len().saturating_sub(pos) / 16 {
            return Err(err(format!("section map: {name} page list truncated")));
        }
        let mut entries = Vec::with_capacity(npages);
        for _ in 0..npages {
            entries.push((
                le32(&sm, pos)? as i64,
                le32(&sm, pos + 4)? as usize,
                le64(&sm, pos + 8)? as usize,
            ));
            pos += 16;
        }
        if !names.insert(name.clone()) {
            return Err(err(format!("section map: duplicate section {name}")));
        }
        let Some(slot) = SECTION_NAMES.iter().position(|n| *n == name) else {
            continue;
        };
        if encrypted == 1 {
            return Err(err(
                "encrypted (password-protected) drawing is not supported",
            ));
        }
        if max == 0 || max > bud.page {
            return Err(err(format!(
                "section {name}: page size {max} exceeds the page limit"
            )));
        }
        bud.section(&name, size)?;
        let mut out = Vec::new();
        for (pid, csize, off) in entries {
            bud.claim(&name, pid)?;
            let Some(&at) = pages.get(&pid) else {
                rep.warn(format!("{name}: page {pid} missing"));
                continue;
            };
            let Ok(magic) = le32(raw, at) else {
                rep.warn(format!("{name}: page {pid} past end of file"));
                continue;
            };
            if magic ^ (0x4164_536Bu32 ^ at as u32) != 0x4163_043B {
                rep.warn(format!("{name}: data page header mismatch"));
            }
            let Ok(data) = slice(raw, at.saturating_add(32), csize) else {
                rep.warn(format!("{name}: page {pid} past end of file"));
                continue;
            };
            let page = if compressed == 2 {
                match expand2004(data, max) {
                    Ok(p) => p,
                    Err(e) => {
                        rep.warn(format!("{name}: page {pid}: {}", e.0));
                        continue;
                    }
                }
            } else {
                data[..data.len().min(max)].to_vec()
            };
            place(&mut out, off, &page, size)?;
        }
        found[slot] = Some(out);
    }
    let [header, classes, handles, objects] = found;
    Ok(Sections {
        header: header.ok_or_else(|| err("header section missing"))?,
        classes: classes.unwrap_or_default(),
        handles: handles.ok_or_else(|| err("object map missing"))?,
        objects: Cow::Owned(objects.ok_or_else(|| err("object section missing"))?),
        codepage,
    })
}

/// Decoded sections cannot plausibly exceed the input by more than this ratio.
fn cap(raw: &[u8]) -> usize {
    raw.len().saturating_mul(256).max(1 << 20)
}

fn ceil_div(a: usize, b: usize) -> usize {
    a.div_ceil(b)
}

fn r2007<'a>(raw: &'a [u8], lim: &DwgLimits, rep: &mut DwgReport) -> R<Sections<'a>> {
    let mut bud = Budget::new(raw, lim);
    let max_page = bud.page;
    let data = rs_data(raw, 0x80, 3, 239)?;
    let length = le32(&data, 24)? as i32;
    if length <= 0 || length as usize > data.len() - 32 {
        return Err(err("file header frame length"));
    }
    let h = expand2007(&data[32..32 + length as usize], 0x110)?;
    if h.len() < 0x110 {
        return Err(err("file header truncated"));
    }
    let f = |o: usize| le64(&h, o).map(|v| v as usize);
    let sys = |at: usize, csize: usize, usize_: usize, repeat: usize| -> R<Vec<u8>> {
        if csize == 0 || csize > 1 << 24 || usize_ > max_page || repeat == 0 || repeat > 64 {
            return Err(err("system page sizes"));
        }
        let k = ceil_div(ceil_div(csize, 8) * 8 * repeat, 239);
        let d = rs_data(raw, at, k, 239)?;
        expand2007(&d[..csize.min(d.len())], usize_)
    };
    let pmap = sys(
        0x480usize.saturating_add(f(0x38)?),
        f(0x50)?,
        f(0x58)?,
        f(0x18)?,
    )?;
    let mut pages = HashMap::new();
    let mut pos = 0x480usize;
    for c in pmap.chunks_exact(16) {
        let size = le64(c, 0)? as i64;
        let id = le64(c, 8)? as i64;
        if size <= 0 {
            break;
        }
        if id > 0 && pages.insert(id, pos).is_some() {
            return Err(err(format!("page map: duplicate page {id}")));
        }
        pos = pos.saturating_add(size as usize);
    }
    let smap_id = f(0xC0)? as i64;
    let sm_at = *pages
        .get(&smap_id)
        .ok_or_else(|| err("section map page missing"))?;
    let smap = sys(sm_at, f(0xB0)?, f(0xC8)?, f(0xD8)?)?;
    bud.used_pages.insert(smap_id);
    let mut found: [Option<Vec<u8>>; 4] = Default::default();
    let mut names = HashSet::new();
    let mut i = 0usize;
    while i + 64 <= smap.len() {
        let fl: Vec<usize> = (0..8)
            .map(|k| le64(&smap, i + 8 * k).map(|v| v as usize))
            .collect::<R<_>>()?;
        i += 64;
        let (size, nlen, npages) = (fl[0], fl[4], fl[7]);
        let name_b = slice(&smap, i, nlen)?;
        let units: Vec<u16> = name_b
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        let name = String::from_utf16_lossy(&units);
        i += nlen;
        if npages > (smap.len() - i.min(smap.len())) / 56 {
            return Err(err("section map truncated"));
        }
        let plist: Vec<[usize; 7]> = (0..npages)
            .map(|j| {
                let mut p = [0usize; 7];
                for (k, v) in p.iter_mut().enumerate() {
                    *v = le64(&smap, i + 56 * j + 8 * k)? as usize;
                }
                Ok(p)
            })
            .collect::<R<_>>()?;
        i += 56 * npages;
        if !name.is_empty() && !names.insert(name.clone()) {
            return Err(err(format!("section map: duplicate section {name}")));
        }
        let Some(slot) = SECTION_NAMES.iter().position(|n| *n == name) else {
            continue;
        };
        bud.section(&name, size)?;
        let mut buf = Vec::new();
        for [off, _, pid, us, cs, _, _] in plist {
            bud.claim(&name, pid as i64)?;
            let Some(&pa) = pages.get(&(pid as i64)) else {
                rep.warn(format!("{name}: page {pid} missing"));
                continue;
            };
            if us > max_page || cs > max_page {
                return Err(err(format!("{name}: page {pid} exceeds the page limit")));
            }
            let page = if us == cs {
                match slice(raw, pa, cs) {
                    Ok(d) => d.to_vec(),
                    Err(_) => {
                        rep.warn(format!("{name}: page {pid} past end of file"));
                        continue;
                    }
                }
            } else {
                let k = ceil_div(ceil_div(cs, 8) * 8, 251);
                let Ok(d) = rs_data(raw, pa, k, 251) else {
                    rep.warn(format!("{name}: page {pid} past end of file"));
                    continue;
                };
                match expand2007(&d[..cs.min(d.len())], us) {
                    Ok(p) => p,
                    Err(e) => {
                        rep.warn(format!("{name}: page {pid}: {}", e.0));
                        continue;
                    }
                }
            };
            place(&mut buf, off, &page, size)?;
        }
        found[slot] = Some(buf);
    }
    let [header, classes, handles, objects] = found;
    Ok(Sections {
        header: header.ok_or_else(|| err("header section missing"))?,
        classes: classes.unwrap_or_default(),
        handles: handles.ok_or_else(|| err("object map missing"))?,
        objects: Cow::Owned(objects.ok_or_else(|| err("object section missing"))?),
        codepage: le16(raw, 0x13).unwrap_or(30),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encrypts a 0x6C-byte header the way the 2004 family stores it (XOR is symmetric).
    fn enc_header(plain: &[u8; 0x6C]) -> [u8; 0x6C] {
        decrypt_header(plain)
    }

    fn sys_page(magic: u32, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![];
        v.extend_from_slice(&magic.to_le_bytes());
        v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(&1u32.to_le_bytes()); // uncompressed
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    /// Minimal AC1018 file: page map at 0x100 (page 1), section map (page 2), then data pages.
    /// `entries` lists (page id, logical offset) of the AcDb:Header section.
    fn build_r2004(data_pages: &[&[u8]], entries: &[(u32, u64)], max: u32, size: u64) -> Vec<u8> {
        let mut smap = vec![];
        smap.extend_from_slice(&1u32.to_le_bytes()); // count
        smap.extend_from_slice(&[0u8; 16]);
        smap.extend_from_slice(&size.to_le_bytes());
        smap.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        smap.extend_from_slice(&max.to_le_bytes());
        smap.extend_from_slice(&[0u8; 4]);
        smap.extend_from_slice(&1u32.to_le_bytes()); // compressed = 1 (raw)
        smap.extend_from_slice(&[0u8; 4]);
        smap.extend_from_slice(&0u32.to_le_bytes()); // not encrypted
        let mut name = [0u8; 64];
        name[..11].copy_from_slice(b"AcDb:Header");
        smap.extend_from_slice(&name);
        for &(pid, off) in entries {
            smap.extend_from_slice(&pid.to_le_bytes());
            smap.extend_from_slice(&(data_pages[0].len() as u32).to_le_bytes());
            smap.extend_from_slice(&off.to_le_bytes());
        }
        let sm_page = sys_page(0x4163_003B, &smap);
        let mut dpages = vec![];
        for d in data_pages {
            let mut p = vec![0u8; 32];
            p.extend_from_slice(d);
            dpages.push(p);
        }
        // Page map: (1, pm), (2, sm), (3.., data); sizes are physical lengths.
        let pm_len = 20 + 8 * (2 + dpages.len());
        let mut pm = vec![];
        pm.extend_from_slice(&1u32.to_le_bytes());
        pm.extend_from_slice(&(pm_len as u32).to_le_bytes());
        pm.extend_from_slice(&2u32.to_le_bytes());
        pm.extend_from_slice(&(sm_page.len() as u32).to_le_bytes());
        for (k, p) in dpages.iter().enumerate() {
            pm.extend_from_slice(&(3 + k as u32).to_le_bytes());
            pm.extend_from_slice(&(p.len() as u32).to_le_bytes());
        }
        let pm_page = sys_page(0x4163_0E3B, &pm);
        assert_eq!(pm_page.len(), pm_len);
        let mut raw = vec![0u8; 0x100];
        raw[..6].copy_from_slice(b"AC1018");
        let mut h = [0u8; 0x6C];
        h[..11].copy_from_slice(b"AcFssFcAJMB");
        h[0x5C..0x60].copy_from_slice(&2u32.to_le_bytes()); // section map id
        raw[0x80..0x80 + 0x6C].copy_from_slice(&enc_header(&h));
        raw.extend_from_slice(&pm_page);
        raw.extend_from_slice(&sm_page);
        for p in dpages {
            raw.extend_from_slice(&p);
        }
        raw
    }

    fn lim() -> DwgLimits {
        DwgLimits::default()
    }

    #[test]
    fn r2004_page_reuse_rejected() {
        let raw = build_r2004(&[b"abcd"], &[(3, 0), (3, 4)], 4, 8);
        let e = r2004(&raw, &lim(), &mut DwgReport::default()).unwrap_err();
        assert!(e.0.contains("referenced twice"), "{}", e.0);
        // System pages cannot be pulled into a data section either.
        let raw = build_r2004(&[b"abcd"], &[(2, 0)], 4, 4);
        assert!(r2004(&raw, &lim(), &mut DwgReport::default()).is_err());
    }

    #[test]
    fn r2004_pages_must_be_ordered() {
        let raw = build_r2004(&[b"abcd", b"efgh"], &[(4, 4), (3, 0)], 4, 8);
        let e = r2004(&raw, &lim(), &mut DwgReport::default()).unwrap_err();
        assert!(e.0.contains("overlap"), "{}", e.0);
    }

    #[test]
    fn r2004_page_and_total_limits() {
        let raw = build_r2004(&[b"abcd"], &[(3, 0)], 4, 4);
        let small = DwgLimits {
            max_page: 2,
            ..lim()
        };
        assert!(r2004(&raw, &small, &mut DwgReport::default()).is_err());
        let tiny = DwgLimits {
            max_total: 3,
            ..lim()
        };
        let e = r2004(&raw, &tiny, &mut DwgReport::default()).unwrap_err();
        assert!(e.0.contains("budget"), "{}", e.0);
        // Declared size far beyond the file ratio is refused before any allocation.
        let raw = build_r2004(&[b"abcd"], &[(3, 0)], 4, 1 << 40);
        assert!(r2004(&raw, &lim(), &mut DwgReport::default()).is_err());
    }

    #[test]
    fn r2004_header_guards() {
        let mut raw = build_r2004(&[b"abcd"], &[(3, 0)], 4, 4);
        let mut h = decrypt_header(&raw[0x80..0x80 + 0x6C]);
        h[0x58] = 1; // high word of the page map address
        raw[0x80..0x80 + 0x6C].copy_from_slice(&enc_header(&h));
        assert!(r2004(&raw, &lim(), &mut DwgReport::default()).is_err());
        h[0x58] = 0;
        h[0x54..0x58].copy_from_slice(&u32::MAX.to_le_bytes());
        raw[0x80..0x80 + 0x6C].copy_from_slice(&enc_header(&h));
        assert!(r2004(&raw, &lim(), &mut DwgReport::default()).is_err());
        h[0] = b'X';
        raw[0x80..0x80 + 0x6C].copy_from_slice(&enc_header(&h));
        assert!(r2004(&raw, &lim(), &mut DwgReport::default()).is_err());
    }

    #[test]
    fn r2004_missing_data_page_warns_and_continues() {
        // Second entry names a page beyond the map; the section keeps the first page.
        let raw = build_r2004(&[b"abcd"], &[(3, 0), (9, 4)], 4, 8);
        let mut rep = DwgReport::default();
        let e = r2004(&raw, &lim(), &mut rep).unwrap_err();
        // Only the header section exists, so the read fails later on the object map,
        // after warning about the missing page rather than aborting on it.
        assert!(e.0.contains("object map"), "{}", e.0);
        assert!(rep.warnings.iter().any(|w| w.contains("page 9 missing")));
    }

    #[test]
    fn place_appends_in_order() {
        let mut out = vec![];
        place(&mut out, 0, b"ab", 6).unwrap();
        place(&mut out, 4, b"efgh", 6).unwrap();
        assert_eq!(out, b"ab\0\0ef");
        assert!(place(&mut out, 2, b"x", 6).is_err());
        assert!(place(&mut vec![], 6, b"x", 6).is_err());
    }
}
