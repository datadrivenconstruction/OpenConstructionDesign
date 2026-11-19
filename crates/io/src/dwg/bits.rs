// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Bounded MSB-first bit reader with the compact field encodings of the format
//! (spec: docs/specs/DWG.md §2). Every read is bounds-checked and returns `Err` past the end.

use super::{err, R};

#[derive(Clone)]
pub(crate) struct Bits<'a> {
    d: &'a [u8],
    pub pos: usize,
    pub end: usize,
}

impl<'a> Bits<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        Bits {
            d,
            pos: 0,
            end: d.len().saturating_mul(8),
        }
    }

    /// View on bits `start..end` (clamped to the data).
    pub fn view(d: &'a [u8], start: usize, end: usize) -> Self {
        let max = d.len().saturating_mul(8);
        let end = end.min(max);
        Bits {
            d,
            pos: start.min(end),
            end,
        }
    }

    pub fn left(&self) -> usize {
        self.end.saturating_sub(self.pos)
    }

    pub fn skip(&mut self, n: usize) -> R<()> {
        if n > self.left() {
            return Err(err("bit stream truncated"));
        }
        self.pos += n;
        Ok(())
    }

    pub fn b(&mut self) -> R<bool> {
        if self.pos >= self.end {
            return Err(err("bit stream truncated"));
        }
        let v = (self.d[self.pos >> 3] >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Ok(v != 0)
    }

    pub fn bits(&mut self, n: u32) -> R<u64> {
        if n > 64 || n as usize > self.left() {
            return Err(err("bit stream truncated"));
        }
        let mut v = 0u64;
        let mut n = n;
        while n >= 8 {
            v = (v << 8) | self.rc()? as u64;
            n -= 8;
        }
        for _ in 0..n {
            v = (v << 1) | self.b()? as u64;
        }
        Ok(v)
    }

    pub fn bb(&mut self) -> R<u8> {
        Ok(((self.b()? as u8) << 1) | self.b()? as u8)
    }

    pub fn rc(&mut self) -> R<u8> {
        if self.left() < 8 {
            return Err(err("bit stream truncated"));
        }
        let i = self.pos >> 3;
        let s = self.pos & 7;
        let v = if s == 0 {
            self.d[i]
        } else {
            let hi = (self.d[i] as u16) << 8;
            let lo = *self.d.get(i + 1).unwrap_or(&0) as u16;
            (((hi | lo) >> (8 - s)) & 0xFF) as u8
        };
        self.pos += 8;
        Ok(v)
    }

    pub fn bytes(&mut self, n: usize) -> R<Vec<u8>> {
        if n > self.left() / 8 {
            return Err(err("bit stream truncated"));
        }
        (0..n).map(|_| self.rc()).collect()
    }

    pub fn rs(&mut self) -> R<u16> {
        Ok(self.rc()? as u16 | (self.rc()? as u16) << 8)
    }

    pub fn rl(&mut self) -> R<u32> {
        Ok(self.rs()? as u32 | (self.rs()? as u32) << 16)
    }

    pub fn rd(&mut self) -> R<f64> {
        finite(self.rd_raw()?)
    }

    /// RD without the finiteness check (callers that have a defined fallback).
    pub fn rd_raw(&mut self) -> R<f64> {
        let mut b = [0u8; 8];
        for x in &mut b {
            *x = self.rc()?;
        }
        Ok(f64::from_le_bytes(b))
    }

    pub fn bs(&mut self) -> R<u16> {
        Ok(match self.bb()? {
            0 => self.rs()?,
            1 => self.rc()? as u16,
            2 => 0,
            _ => 256,
        })
    }

    pub fn bl(&mut self) -> R<u32> {
        Ok(match self.bb()? {
            0 => self.rl()?,
            1 => self.rc()? as u32,
            _ => 0,
        })
    }

    /// 3-bit byte count, then that many little-endian bytes.
    pub fn bll(&mut self) -> R<u64> {
        let n = self.bits(3)?;
        let mut v = 0u64;
        for i in 0..n {
            v |= (self.rc()? as u64) << (8 * i);
        }
        Ok(v)
    }

    pub fn bd(&mut self) -> R<f64> {
        Ok(match self.bb()? {
            0 => self.rd()?,
            1 => 1.0,
            _ => 0.0,
        })
    }

    /// Double with a default: 0 = default, 1 = low 4 bytes patched, 2 = 6 bytes patched, 3 = full.
    pub fn dd(&mut self, def: f64) -> R<f64> {
        let mut b = def.to_le_bytes();
        match self.bb()? {
            0 => return Ok(def),
            1 => {
                for x in b.iter_mut().take(4) {
                    *x = self.rc()?;
                }
            }
            2 => {
                b[4] = self.rc()?;
                b[5] = self.rc()?;
                for x in b.iter_mut().take(4) {
                    *x = self.rc()?;
                }
            }
            _ => {
                for x in &mut b {
                    *x = self.rc()?;
                }
            }
        }
        finite(f64::from_le_bytes(b))
    }

    /// Thickness: bit set = 0.0.
    pub fn bt(&mut self, from_r2000: bool) -> R<f64> {
        if from_r2000 && self.b()? {
            return Ok(0.0);
        }
        self.bd()
    }

    /// Extrusion: bit set = (0,0,1).
    pub fn be(&mut self, from_r2000: bool) -> R<[f64; 3]> {
        if from_r2000 && self.b()? {
            return Ok([0.0, 0.0, 1.0]);
        }
        self.p3()
    }

    pub fn p3(&mut self) -> R<[f64; 3]> {
        Ok([self.bd()?, self.bd()?, self.bd()?])
    }

    pub fn p2r(&mut self) -> R<[f64; 2]> {
        Ok([self.rd()?, self.rd()?])
    }

    pub fn p2d(&mut self, def: [f64; 2]) -> R<[f64; 2]> {
        Ok([self.dd(def[0])?, self.dd(def[1])?])
    }

    /// Handle reference: (code, value).
    pub fn h(&mut self) -> R<(u8, u64)> {
        let code = self.bits(4)? as u8;
        let n = self.bits(4)? as u32;
        Ok((code, self.bits(8 * n)?))
    }

    /// Handle resolved against the referencing object's own handle.
    pub fn href(&mut self, own: u64) -> R<u64> {
        let (code, v) = self.h()?;
        Ok(match code {
            6 => own.wrapping_add(1),
            8 => own.wrapping_sub(1),
            0xA => own.wrapping_add(v),
            0xC => own.wrapping_sub(v),
            _ => v,
        })
    }

    /// Byte-wise text: BS length, then that many bytes (single-byte code page).
    pub fn tv_raw(&mut self) -> R<Vec<u8>> {
        let n = self.bs()? as usize;
        let mut v = self.bytes(n)?;
        while v.last() == Some(&0) {
            v.pop();
        }
        Ok(v)
    }

    /// UTF-16LE text: BS unit count, then 2 bytes per unit.
    pub fn tu(&mut self) -> R<String> {
        let n = self.bs()? as usize;
        if n > self.left() / 16 {
            return Err(err("text exceeds stream"));
        }
        let mut u = Vec::with_capacity(n);
        for _ in 0..n {
            u.push(self.rs()?);
        }
        while u.last() == Some(&0) {
            u.pop();
        }
        Ok(String::from_utf16_lossy(&u))
    }
}

fn finite(v: f64) -> R<f64> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(err("non-finite double"))
    }
}

/// Modular char (byte-wise, 7 bits per byte, high bit = more); the last byte's 0x40 is the sign.
pub(crate) fn mc(d: &[u8], pos: &mut usize) -> R<i64> {
    let mut v: u64 = 0;
    let mut shift = 0u32;
    loop {
        let b = *d.get(*pos).ok_or_else(|| err("modular char truncated"))?;
        *pos += 1;
        if b & 0x80 == 0 {
            let neg = b & 0x40 != 0;
            v |= ((b & 0x3F) as u64).checked_shl(shift).unwrap_or(0);
            let v = v as i64;
            return Ok(if neg { v.wrapping_neg() } else { v });
        }
        v |= ((b & 0x7F) as u64).checked_shl(shift).unwrap_or(0);
        shift += 7;
        if shift > 63 {
            return Err(err("modular char too long"));
        }
    }
}

/// Unsigned modular char.
pub(crate) fn umc(d: &[u8], pos: &mut usize) -> R<u64> {
    let mut v: u64 = 0;
    let mut shift = 0u32;
    loop {
        let b = *d.get(*pos).ok_or_else(|| err("modular char truncated"))?;
        *pos += 1;
        v |= ((b & 0x7F) as u64).checked_shl(shift).unwrap_or(0);
        if b & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
        if shift > 63 {
            return Err(err("modular char too long"));
        }
    }
}

/// Modular short: little-endian 16-bit words, 15 bits each, high bit = more.
pub(crate) fn ms(d: &[u8], pos: &mut usize) -> R<u64> {
    let mut v: u64 = 0;
    for i in 0..4 {
        let lo = *d.get(*pos).ok_or_else(|| err("modular short truncated"))? as u64;
        let hi = *d
            .get(*pos + 1)
            .ok_or_else(|| err("modular short truncated"))? as u64;
        *pos += 2;
        let w = lo | hi << 8;
        v |= (w & 0x7FFF) << (15 * i);
        if w & 0x8000 == 0 {
            return Ok(v);
        }
    }
    Err(err("modular short too long"))
}

/// CRC-16 (reflected 0x8005) as used for sections, records and object frames.
pub(crate) fn crc16(seed: u16, d: &[u8]) -> u16 {
    static T: std::sync::OnceLock<[u16; 256]> = std::sync::OnceLock::new();
    let t = T.get_or_init(|| {
        let mut t = [0u16; 256];
        for (i, x) in t.iter_mut().enumerate() {
            let mut c = i as u16;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    (c >> 1) ^ 0xA001
                } else {
                    c >> 1
                };
            }
            *x = c;
        }
        t
    });
    let mut c = seed;
    for &b in d {
        c = (c >> 8) ^ t[((c ^ b as u16) & 0xFF) as usize];
    }
    c
}

fn le<const N: usize>(d: &[u8], at: usize) -> R<[u8; N]> {
    let s = at
        .checked_add(N)
        .and_then(|e| d.get(at..e))
        .ok_or_else(|| err("read past end"))?;
    let mut b = [0u8; N];
    b.copy_from_slice(s);
    Ok(b)
}

pub(crate) fn le16(d: &[u8], at: usize) -> R<u16> {
    le::<2>(d, at).map(u16::from_le_bytes)
}

pub(crate) fn le32(d: &[u8], at: usize) -> R<u32> {
    le::<4>(d, at).map(u32::from_le_bytes)
}

pub(crate) fn le64(d: &[u8], at: usize) -> R<u64> {
    le::<8>(d, at).map(u64::from_le_bytes)
}

/// MSB-first bit writer mirroring [`Bits`], for hand-built records in unit tests.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct BitW {
    pub d: Vec<u8>,
    /// Bits written.
    pub n: usize,
}

#[cfg(test)]
impl BitW {
    pub fn b(&mut self, v: bool) -> &mut Self {
        if self.n.is_multiple_of(8) {
            self.d.push(0);
        }
        if v {
            self.d[self.n / 8] |= 0x80 >> (self.n % 8);
        }
        self.n += 1;
        self
    }
    pub fn bits(&mut self, v: u64, n: u32) -> &mut Self {
        for i in (0..n).rev() {
            self.b((v >> i) & 1 != 0);
        }
        self
    }
    pub fn bb(&mut self, v: u8) -> &mut Self {
        self.bits(v as u64, 2)
    }
    pub fn rc(&mut self, v: u8) -> &mut Self {
        self.bits(v as u64, 8)
    }
    pub fn bs(&mut self, v: u16) -> &mut Self {
        self.bb(0).rc(v as u8).rc((v >> 8) as u8)
    }
    pub fn bl(&mut self, v: u32) -> &mut Self {
        self.bb(0);
        for b in v.to_le_bytes() {
            self.rc(b);
        }
        self
    }
    pub fn bd(&mut self, v: f64) -> &mut Self {
        self.bb(0);
        for b in v.to_le_bytes() {
            self.rc(b);
        }
        self
    }
    /// Byte text before R2007: BS length, then the bytes.
    pub fn tv(&mut self, s: &str) -> &mut Self {
        self.bs(s.len() as u16);
        for b in s.bytes() {
            self.rc(b);
        }
        self
    }
    pub fn h(&mut self, code: u8, v: u64) -> &mut Self {
        let be = v.to_be_bytes();
        let n = be.iter().position(|&b| b != 0).map_or(0, |p| 8 - p);
        self.bits(code as u64, 4).bits(n as u64, 4);
        for &b in &be[8 - n..] {
            self.rc(b);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_fields() {
        // BS code 1 + byte 0x2A; BD code 1 (1.0); BB 2 => BL 0.
        let d = [0b0100_1010, 0b1001_1000];
        let mut r = Bits::new(&d);
        assert_eq!(r.bs().unwrap(), 0x2A);
        assert_eq!(r.bd().unwrap(), 1.0);
        assert_eq!(r.bl().unwrap(), 0);
        assert!(Bits::new(&[]).b().is_err());
    }

    #[test]
    fn modular() {
        let mut p = 0;
        assert_eq!(mc(&[0x82, 0x41], &mut p).unwrap(), -130);
        p = 0;
        assert_eq!(umc(&[0x82, 0x01], &mut p).unwrap(), 130);
        p = 0;
        assert_eq!(ms(&[0x01, 0x80, 0x02, 0x00], &mut p).unwrap(), 1 | 2 << 15);
        assert!(mc(&[0xFF; 20], &mut 0).is_err());
    }

    #[test]
    fn crc_known() {
        // CRC-16/ARC check value with zero seed.
        assert_eq!(crc16(0, b"123456789"), 0xBB3D);
    }

    #[test]
    fn le_bounds_no_overflow() {
        let d = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(le16(&d, 6).unwrap(), 0x0807);
        assert_eq!(le64(&d, 0).unwrap(), 0x0807_0605_0403_0201);
        assert!(le16(&d, 7).is_err());
        assert!(le32(&d, usize::MAX).is_err());
        assert!(le64(&d, usize::MAX - 3).is_err());
    }
}
