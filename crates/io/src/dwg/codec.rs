//! Page decompression: the 2004-family back-reference codec and the 2007 codec with its
//! permuted literal runs; 2007 Reed-Solomon de-interleave (spec: docs/specs/DWG.md §3).
//! All output is bounded by `limit`; malformed input returns `Err`.

use super::{err, R};

struct Src<'a> {
    d: &'a [u8],
    p: usize,
}

impl Src<'_> {
    fn byte(&mut self) -> R<u8> {
        let b = *self
            .d
            .get(self.p)
            .ok_or_else(|| err("compressed data truncated"))?;
        self.p += 1;
        Ok(b)
    }
}

fn copy_back(out: &mut Vec<u8>, dist: usize, n: usize, limit: usize) -> R<()> {
    if dist == 0 || dist > out.len() || n > limit.saturating_sub(out.len()) {
        return Err(err("bad back reference"));
    }
    let start = out.len() - dist;
    if dist >= n {
        out.extend_from_within(start..start + n);
    } else {
        for i in 0..n {
            let b = out[start + i];
            out.push(b);
        }
    }
    Ok(())
}

/// Initial output reservation: proportional to the input, never the full (untrusted) limit.
fn prealloc(input: usize, limit: usize) -> usize {
    limit.min(input.saturating_mul(4)).min(1 << 20)
}

/// 2004-family codec (sections of AC1018 and later, except AC1021).
pub(crate) fn expand2004(data: &[u8], limit: usize) -> R<Vec<u8>> {
    let mut s = Src { d: data, p: 0 };
    let mut out = Vec::with_capacity(prealloc(data.len(), limit));
    let literal = |s: &mut Src, out: &mut Vec<u8>, n: usize| -> R<()> {
        let end = s.p.checked_add(n).filter(|&e| e <= s.d.len());
        match end {
            Some(e) if n <= limit.saturating_sub(out.len()) => {
                out.extend_from_slice(&s.d[s.p..e]);
                s.p = e;
                Ok(())
            }
            _ => Err(err("literal exceeds input/output")),
        }
    };
    let length = |s: &mut Src, base: usize| -> R<usize> {
        let mut extra = 0usize;
        loop {
            let b = s.byte()? as usize;
            if b != 0 {
                return Ok(base + extra + b);
            }
            extra += 255;
            if extra > limit {
                return Err(err("length exceeds budget"));
            }
        }
    };
    let mut op = s.byte()?;
    if op < 16 {
        let n = if op != 0 {
            op as usize + 3
        } else {
            length(&mut s, 15)? + 3
        };
        literal(&mut s, &mut out, n)?;
        op = s.byte()?;
    }
    loop {
        let (count, dist, trailing);
        if op == 0x11 {
            return Ok(out);
        } else if op >= 0x40 {
            count = (op as usize >> 4) - 1;
            dist = ((op as usize >> 2) & 3) + ((s.byte()? as usize) << 2) + 1;
            trailing = op & 3;
        } else if op >= 0x20 {
            let mut c = (op & 31) as usize;
            if c == 0 {
                c = length(&mut s, 31)?;
            }
            count = c + 2;
            let lo = s.byte()? as usize;
            let hi = s.byte()? as usize;
            dist = (lo >> 2) + (hi << 6) + 1;
            trailing = (lo & 3) as u8;
        } else if op >= 0x10 {
            let mut c = (op & 7) as usize;
            if c == 0 {
                c = length(&mut s, 7)?;
            }
            count = c + 2;
            let lo = s.byte()? as usize;
            let hi = s.byte()? as usize;
            dist = (((op & 8) as usize) << 11) + (lo >> 2) + (hi << 6) + 0x4000;
            trailing = (lo & 3) as u8;
        } else {
            return Err(err("unknown compression opcode"));
        }
        copy_back(&mut out, dist, count, limit)?;
        if trailing != 0 {
            literal(&mut s, &mut out, trailing as usize)?;
        } else {
            op = s.byte()?;
            if op < 16 {
                let n = if op != 0 {
                    op as usize + 3
                } else {
                    length(&mut s, 15)? + 3
                };
                literal(&mut s, &mut out, n)?;
            } else {
                continue;
            }
        }
        op = s.byte()?;
    }
}

/// Literal piece sizes for a run shorter than 32 bytes; the pieces are stored in reverse order.
fn pieces(n: usize, after_block: bool) -> Vec<usize> {
    const S: &[(usize, &[usize])] = &[
        (1, &[1]),
        (2, &[1, 1]),
        (3, &[1, 1, 1]),
        (4, &[4]),
        (5, &[4, 1]),
        (6, &[1, 4, 1]),
        (7, &[1, 4, 1, 1]),
        (8, &[8]),
        (9, &[8, 1]),
        (10, &[1, 8, 1]),
        (11, &[1, 8, 1, 1]),
        (17, &[8, 1, 8]),
        (18, &[1, 8, 8, 1]),
        (19, &[8, 8, 1, 1, 1]),
        (20, &[8, 8, 4]),
        (21, &[8, 8, 4, 1]),
        (22, &[8, 8, 4, 1, 1]),
        (23, &[8, 8, 4, 1, 1, 1]),
        (26, &[8, 8, 1, 8, 1]),
        (27, &[8, 8, 1, 8, 1, 1]),
        (28, &[8, 8, 8, 4]),
        (29, &[8, 8, 8, 4, 1]),
        (30, &[8, 8, 8, 4, 1, 1]),
        (31, &[1, 1, 8, 8, 8, 4, 1]),
    ];
    let _ = after_block; // both tables agree on every length they define
    if let Some((_, p)) = S.iter().find(|(k, _)| *k == n) {
        return p.to_vec();
    }
    fallback(n)
}

fn fallback(n: usize) -> Vec<usize> {
    if n >= 16 {
        let mut v = vec![8];
        v.extend(fallback(n - 16));
        v.push(8);
        return v;
    }
    let mut v = vec![];
    if n & 2 != 0 {
        v.push(1);
    }
    if n & 8 != 0 {
        v.push(8);
    }
    if n & 4 != 0 {
        v.push(4);
    }
    if n & 2 != 0 {
        v.push(1);
    }
    if n & 1 != 0 {
        v.push(1);
    }
    v
}

fn unpermute(b: &[u8], out: &mut Vec<u8>) {
    let mut q = 0;
    while b.len() - q >= 32 {
        let k = &b[q..q + 32];
        out.extend_from_slice(&k[24..32]);
        out.extend_from_slice(&k[16..24]);
        out.extend_from_slice(&k[8..16]);
        out.extend_from_slice(&k[0..8]);
        q += 32;
    }
    let r = b.len() - q;
    if r == 0 {
        return;
    }
    let sizes = pieces(r, q > 0);
    let mut parts = Vec::with_capacity(sizes.len());
    let mut p = q;
    for s in sizes {
        let e = (p + s).min(b.len());
        parts.push(&b[p..e]);
        p = e;
    }
    for part in parts.iter().rev() {
        out.extend_from_slice(part);
    }
}

/// 2007 codec (AC1021).
pub(crate) fn expand2007(s: &[u8], limit: usize) -> R<Vec<u8>> {
    if s.is_empty() {
        return Err(err("empty compressed stream"));
    }
    let mut src = Src { d: s, p: 0 };
    let end = s.len();
    let mut out: Vec<u8> = Vec::with_capacity(prealloc(s.len(), limit));
    let litlen = |src: &mut Src, op: u8| -> R<usize> {
        let mut n = op as usize + 8;
        if n == 0x17 {
            let x = src.byte()? as usize;
            n += x;
            if x == 0xFF {
                loop {
                    let x = src.byte()? as usize | (src.byte()? as usize) << 8;
                    n += x;
                    if n > limit {
                        return Err(err("literal length exceeds budget"));
                    }
                    if x != 0xFFFF {
                        break;
                    }
                }
            }
        }
        Ok(n)
    };
    let instr = |src: &mut Src, op: u8| -> R<(u8, usize, usize)> {
        let h = op >> 4;
        let mut op = op;
        let (off, n);
        match h {
            0 => {
                let mut nn = (op & 15) as usize + 0x13;
                let mut o = src.byte()? as usize;
                op = src.byte()?;
                nn += ((op >> 3) & 0x10) as usize;
                o += (((op & 0x78) as usize) << 5) + 1;
                n = nn;
                off = o;
            }
            1 => {
                n = (op & 15) as usize + 3;
                let mut o = src.byte()? as usize;
                op = src.byte()?;
                o += (((op & 0xF8) as usize) << 5) + 1;
                off = o;
            }
            2 => {
                let mut o = src.byte()? as usize;
                o |= (src.byte()? as usize) << 8;
                let mut nn = (op & 7) as usize;
                if op & 8 == 0 {
                    op = src.byte()?;
                    nn += (op & 0xF8) as usize;
                } else {
                    o += 1;
                    nn += (src.byte()? as usize) << 3;
                    op = src.byte()?;
                    nn += (((op & 0xF8) as usize) << 8) + 0x100;
                }
                n = nn;
                off = o;
            }
            _ => {
                n = h as usize;
                let mut o = (op & 15) as usize;
                op = src.byte()?;
                o += (((op & 0xF8) as usize) << 1) + 1;
                off = o;
            }
        }
        Ok((op, off, n))
    };
    let mut op = src.byte()?;
    let mut run = 0usize;
    if op & 0xF0 == 0x20 {
        src.p += 2;
        run = (src.byte()? & 7) as usize;
    }
    while src.p < end {
        if run == 0 {
            run = litlen(&mut src, op)?;
        }
        if src.p + run > end || run > limit.saturating_sub(out.len()) {
            return Err(err("literal exceeds input/output"));
        }
        unpermute(&s[src.p..src.p + run], &mut out);
        src.p += run;
        if src.p >= end {
            break;
        }
        let first = src.byte()?;
        let (mut o2, mut off, mut n) = instr(&mut src, first)?;
        loop {
            copy_back(&mut out, off, n, limit)?;
            run = (o2 & 7) as usize;
            if run != 0 || src.p >= end {
                break;
            }
            let mut next = src.byte()?;
            if next >> 4 == 0 {
                o2 = next;
                break;
            }
            if next >> 4 == 0x0F {
                next &= 0x0F;
            }
            (o2, off, n) = instr(&mut src, next)?;
        }
        op = o2;
    }
    Ok(out)
}

/// De-interleaves `k` Reed-Solomon codewords of 255 bytes at `off`, keeping `data` bytes of each.
pub(crate) fn rs_data(raw: &[u8], off: usize, k: usize, data: usize) -> R<Vec<u8>> {
    let total = k
        .checked_mul(255)
        .ok_or_else(|| err("codeword count overflow"))?;
    let block = off
        .checked_add(total)
        .and_then(|e| raw.get(off..e))
        .ok_or_else(|| err("interleaved block truncated"))?;
    let mut out = Vec::with_capacity(k * data);
    for i in 0..k {
        for j in 0..data {
            out.push(block[i + j * k]);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand2004_literal_and_match() {
        // literal "abcd" (op 1 => 4 bytes), match op 0x20|5 => count 7 dist 4, then end 0x11.
        let d = [
            0x01, b'a', b'b', b'c', b'd', 0x25, 0x0C, 0x00, 0x11, 0x00, 0x00,
        ];
        let out = expand2004(&d, 64).unwrap();
        assert_eq!(&out, b"abcdabcdabc");
        assert!(expand2004(&[0x01, b'a'], 64).is_err());
        assert!(expand2004(&[0x01, b'a', b'b', b'c', b'd', 0x25, 0xFC, 0x00], 64).is_err());
    }

    #[test]
    fn expand2007_matches_keep_opcode() {
        // 8-byte literal, then a short match (h>=3: n 3, dist 8), end of input.
        let mut d = vec![0x00];
        d.extend_from_slice(b"abcdefgh");
        d.extend_from_slice(&[0x37, 0x00]);
        assert_eq!(expand2007(&d, 64).unwrap(), b"abcdefghabc");
        // h=1 match (n 5, dist 8) whose second byte carries a 1-byte trailing literal.
        let mut d = vec![0x00];
        d.extend_from_slice(b"abcdefgh");
        d.extend_from_slice(&[0x12, 0x07, 0x01, b'z']);
        assert_eq!(expand2007(&d, 64).unwrap(), b"abcdefghabcdez");
        // Output limit is enforced.
        assert!(expand2007(&d, 10).is_err());
    }

    #[test]
    fn prealloc_is_bounded_by_input() {
        assert_eq!(prealloc(3, 1 << 30), 12);
        assert_eq!(prealloc(1 << 28, 1 << 30), 1 << 20);
        let out = expand2004(&[0x01, b'a', b'b', b'c', b'd', 0x11, 0, 0], 1 << 30).unwrap();
        assert!(out.capacity() < 1 << 10);
    }

    #[test]
    fn permute_tables() {
        let mut out = vec![];
        unpermute(&(0u8..32).collect::<Vec<_>>(), &mut out);
        assert_eq!(&out[..8], &[24, 25, 26, 27, 28, 29, 30, 31]);
        out.clear();
        unpermute(&[1, 2, 3, 4, 5], &mut out);
        assert_eq!(out, vec![5, 1, 2, 3, 4]);
        assert_eq!(fallback(24), vec![8, 8, 8]);
    }

    #[test]
    fn rs_interleave() {
        let raw: Vec<u8> = (0..255u32 * 2).map(|i| i as u8).collect();
        let d = rs_data(&raw, 0, 2, 3).unwrap();
        assert_eq!(d, vec![0, 2, 4, 1, 3, 5]);
        assert!(rs_data(&raw, 1, 2, 3).is_err());
    }
}
