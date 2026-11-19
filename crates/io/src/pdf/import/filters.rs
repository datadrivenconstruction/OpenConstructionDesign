// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Stream filters: Flate (+ PNG/TIFF predictors), ASCIIHex, ASCII85, LZW.
//! Each filter's output is capped at `limit` bytes (protection against "bombs").

use super::lexer::{Dict, Obj};
use super::ImportError;

fn err(m: &str) -> ImportError {
    ImportError::Filter(m.to_string())
}

/// Applies the /Filter chain with /DecodeParms. An unknown filter is an error naming it.
pub fn decode(
    raw: &[u8],
    filters: &[Vec<u8>],
    parms: &[Option<Dict>],
    limit: usize,
) -> Result<Vec<u8>, ImportError> {
    let mut data = raw.to_vec();
    for (i, f) in filters.iter().enumerate() {
        let p = parms.get(i).and_then(|p| p.as_ref());
        data = match f.as_slice() {
            b"FlateDecode" | b"Fl" => predict(flate(&data, limit)?, p, limit)?,
            b"LZWDecode" | b"LZW" => {
                let early = p
                    .and_then(|p| p.get(b"EarlyChange"))
                    .and_then(Obj::int)
                    .unwrap_or(1)
                    != 0;
                predict(lzw(&data, early, limit)?, p, limit)?
            }
            b"ASCIIHexDecode" | b"AHx" => ascii_hex(&data),
            b"ASCII85Decode" | b"A85" => ascii85(&data, limit)?,
            other => {
                return Err(ImportError::Unsupported(format!(
                    "filter {}",
                    String::from_utf8_lossy(other)
                )))
            }
        };
        if data.len() > limit {
            return Err(err("decoded stream exceeds size limit"));
        }
    }
    Ok(data)
}

pub fn flate(data: &[u8], limit: usize) -> Result<Vec<u8>, ImportError> {
    use miniz_oxide::inflate::{decompress_to_vec_with_limit, decompress_to_vec_zlib_with_limit};
    match decompress_to_vec_zlib_with_limit(data, limit) {
        Ok(v) => Ok(v),
        Err(e) => {
            // Truncated stream: take what was decompressed; otherwise try raw deflate.
            if !e.output.is_empty() {
                if e.output.len() >= limit {
                    return Err(err("decoded stream exceeds size limit"));
                }
                return Ok(e.output);
            }
            match decompress_to_vec_with_limit(data, limit) {
                Ok(v) => Ok(v),
                Err(e) if !e.output.is_empty() && e.output.len() < limit => Ok(e.output),
                Err(_) => Err(err("corrupt Flate stream")),
            }
        }
    }
}

fn predict(data: Vec<u8>, p: Option<&Dict>, limit: usize) -> Result<Vec<u8>, ImportError> {
    let Some(p) = p else { return Ok(data) };
    let g = |k: &[u8], d: i64| p.get(k).and_then(Obj::int).unwrap_or(d);
    let pred = g(b"Predictor", 1);
    if pred <= 1 {
        return Ok(data);
    }
    let colors = g(b"Colors", 1).clamp(1, 32) as usize;
    let bpc = g(b"BitsPerComponent", 8).clamp(1, 16) as usize;
    let cols = g(b"Columns", 1).clamp(1, 1 << 20) as usize;
    let bpp = (colors * bpc).div_ceil(8).max(1);
    let row = (colors * bpc * cols).div_ceil(8);
    if pred == 2 {
        if bpc != 8 {
            return Ok(data);
        }
        let mut out = data;
        for r in out.chunks_mut(row) {
            for i in bpp..r.len() {
                r[i] = r[i].wrapping_add(r[i - bpp]);
            }
        }
        return Ok(out);
    }
    // PNG: each row is preceded by a filter-type byte.
    let mut out = Vec::with_capacity(data.len().min(limit));
    let mut prev = vec![0u8; row];
    for chunk in data.chunks(row + 1) {
        let Some((&ty, src)) = chunk.split_first() else {
            break;
        };
        let mut cur = vec![0u8; row];
        cur[..src.len()].copy_from_slice(src);
        for i in 0..row {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            cur[i] = match ty {
                1 => cur[i].wrapping_add(a),
                2 => cur[i].wrapping_add(b),
                3 => cur[i].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => {
                    let pa = (b as i16 - c as i16).abs();
                    let pb = (a as i16 - c as i16).abs();
                    let pc = (a as i16 + b as i16 - 2 * c as i16).abs();
                    let pr = if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    };
                    cur[i].wrapping_add(pr)
                }
                _ => cur[i],
            };
        }
        out.extend_from_slice(&cur[..src.len()]);
        prev = cur;
        if out.len() > limit {
            return Err(err("decoded stream exceeds size limit"));
        }
    }
    Ok(out)
}

pub fn ascii_hex(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut hi: Option<u8> = None;
    for &c in data {
        if c == b'>' {
            break;
        }
        let v = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => continue,
        };
        match hi.take() {
            Some(h) => out.push(h << 4 | v),
            None => hi = Some(v),
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    out
}

pub fn ascii85(data: &[u8], limit: usize) -> Result<Vec<u8>, ImportError> {
    let mut out = Vec::with_capacity(data.len() * 4 / 5);
    let mut group = [0u8; 5];
    let mut n = 0;
    let data = data.strip_prefix(b"<~").unwrap_or(data);
    for &c in data {
        match c {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0; 4]),
            b'!'..=b'u' => {
                group[n] = c - b'!';
                n += 1;
                if n == 5 {
                    let v = group.iter().fold(0u64, |a, &d| a * 85 + d as u64);
                    if v > u32::MAX as u64 {
                        return Err(err("ASCII85 group overflow"));
                    }
                    out.extend_from_slice(&(v as u32).to_be_bytes());
                    n = 0;
                }
            }
            _ => {}
        }
        if out.len() > limit {
            return Err(err("decoded stream exceeds size limit"));
        }
    }
    if n > 1 {
        for g in group.iter_mut().skip(n) {
            *g = 84;
        }
        let v = group.iter().fold(0u64, |a, &d| a * 85 + d as u64);
        let v = v.min(u32::MAX as u64) as u32;
        out.extend_from_slice(&v.to_be_bytes()[..n - 1]);
    }
    Ok(out)
}

pub fn lzw(data: &[u8], early: bool, limit: usize) -> Result<Vec<u8>, ImportError> {
    let mut out = Vec::new();
    // Table: (prefix, last byte, length); strings are restored by walking the prefixes.
    let mut table: Vec<(u16, u8, u32)> = (0..256).map(|i| (u16::MAX, i as u8, 1)).collect();
    table.push((0, 0, 0)); // 256 clear
    table.push((0, 0, 0)); // 257 EOD
    let mut width = 9;
    let mut prev: Option<u16> = None;
    let (mut buf, mut bits) = (0u32, 0u32);
    let mut tmp = Vec::new();
    let emit = |code: u16, table: &[(u16, u8, u32)], tmp: &mut Vec<u8>| {
        tmp.clear();
        let mut c = code;
        while c != u16::MAX {
            let (p, b, _) = table[c as usize];
            tmp.push(b);
            c = p;
        }
        tmp.reverse();
    };
    for &byte in data {
        buf = buf << 8 | byte as u32;
        bits += 8;
        while bits >= width {
            let code = ((buf >> (bits - width)) & ((1 << width) - 1)) as u16;
            bits -= width;
            match code {
                256 => {
                    table.truncate(258);
                    width = 9;
                    prev = None;
                    continue;
                }
                257 => return Ok(out),
                _ => {}
            }
            let n = table.len() as u16;
            if code > n || (code == n && prev.is_none()) {
                return Err(err("corrupt LZW stream"));
            }
            if code < n {
                emit(code, &table, &mut tmp);
            } else if let Some(p) = prev {
                emit(p, &table, &mut tmp);
                let f = tmp[0];
                tmp.push(f);
            }
            if let Some(p) = prev {
                if table.len() < 4096 {
                    let len = table[p as usize].2 + 1;
                    table.push((p, tmp[0], len));
                }
            }
            out.extend_from_slice(&tmp);
            if out.len() > limit {
                return Err(err("decoded stream exceeds size limit"));
            }
            prev = Some(code);
            let next = table.len() + early as usize;
            width = if next >= 2048 {
                12
            } else if next >= 1024 {
                11
            } else if next >= 512 {
                10
            } else {
                9
            };
        }
        buf &= (1 << bits) - 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_filters() {
        assert_eq!(ascii_hex(b"48 65 6C6C6F>"), b"Hello");
        assert_eq!(
            ascii85(b"87cURD]i,\"Ebo80~>", 1 << 20).unwrap(),
            b"Hello World!"
        );
        assert_eq!(ascii85(b"z~>", 100).unwrap(), [0; 4]);
    }

    #[test]
    fn lzw_spec_sample() {
        // Example from the filter description: 2D 2D 2D 2D 2D 41 2D 2D 2D 42.
        let enc = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        assert_eq!(
            lzw(&enc, true, 1 << 20).unwrap(),
            [0x2D, 0x2D, 0x2D, 0x2D, 0x2D, 0x41, 0x2D, 0x2D, 0x2D, 0x42]
        );
    }

    #[test]
    fn png_up_predictor() {
        let raw = [2, 1, 2, 2, 1, 1];
        let mut p = Dict::default();
        p.0.push((b"Predictor".to_vec(), Obj::Int(12)));
        p.0.push((b"Columns".to_vec(), Obj::Int(2)));
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        let out = decode(&z, &[b"FlateDecode".to_vec()], &[Some(p)], 1 << 20).unwrap();
        assert_eq!(out, [1, 2, 2, 3]);
    }
}
