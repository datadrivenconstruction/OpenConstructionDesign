// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Free-form mesh body of a family (`mesh <name> data(...)`): imported or converted geometry.
//!
//! Stored inline in the family text so a family stays one self-contained record. Encoding:
//! positions quantised to 0.1 mm and delta-coded, indices delta-coded, both as zigzag varints,
//! then zlib and URL-safe base64 without padding. A 2 000-triangle chair is ~12 kB of text.

use crate::math::{v3, V3};
use base64::Engine;

/// Quantum of stored coordinates, m.
pub const QUANTUM: f64 = 1e-4;
const VERSION: u8 = 1;
/// Upper bound on decoded size: refuses corrupt or hostile data before allocating.
const MAX_VERTS: usize = 4_000_000;

#[derive(Clone, Debug, PartialEq)]
pub struct MeshBody {
    /// Coordinates from the insertion point, m (x right, y depth, z up).
    pub pos: Vec<V3>,
    pub tris: Vec<[u32; 3]>,
}

fn put(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn zig(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzig(v: u64) -> i64 {
    (v >> 1) as i64 ^ -((v & 1) as i64)
}

struct Rd<'a> {
    b: &'a [u8],
    at: usize,
}

impl Rd<'_> {
    fn get(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.b.get(self.at).ok_or("mesh data ends early")?;
            self.at += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("mesh data: varint too long".into())
    }
}

impl MeshBody {
    /// Drops degenerate and out-of-range triangles; positions must be finite.
    pub fn new(pos: Vec<V3>, tris: Vec<[u32; 3]>) -> Result<MeshBody, String> {
        if pos.iter().any(|p| !(p.x.is_finite() && p.y.is_finite() && p.z.is_finite())) {
            return Err("mesh has a non-finite coordinate".into());
        }
        let n = pos.len() as u32;
        let tris: Vec<[u32; 3]> = tris
            .into_iter()
            .filter(|t| t.iter().all(|&i| i < n) && t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
            .collect();
        if tris.is_empty() {
            return Err("mesh has no triangles".into());
        }
        Ok(MeshBody { pos, tris })
    }

    pub fn bbox(&self) -> [V3; 2] {
        let mut b = [v3(f64::MAX, f64::MAX, f64::MAX), v3(f64::MIN, f64::MIN, f64::MIN)];
        for p in &self.pos {
            b[0] = v3(b[0].x.min(p.x), b[0].y.min(p.y), b[0].z.min(p.z));
            b[1] = v3(b[1].x.max(p.x), b[1].y.max(p.y), b[1].z.max(p.z));
        }
        b
    }

    /// Every edge shared by exactly two triangles in opposite directions: the mesh bounds a volume.
    pub fn closed(&self) -> bool {
        let mut e: std::collections::HashMap<(u32, u32), i32> = std::collections::HashMap::new();
        for t in &self.tris {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                let key = (a.min(b), a.max(b));
                *e.entry(key).or_default() += if a < b { 1 } else { -1 };
            }
        }
        e.values().all(|&c| c == 0) && !e.is_empty()
    }

    pub fn encode(&self) -> String {
        let mut raw = vec![VERSION];
        put(&mut raw, self.pos.len() as u64);
        put(&mut raw, self.tris.len() as u64);
        let mut prev = [0i64; 3];
        for p in &self.pos {
            let q = [p.x, p.y, p.z].map(|c| (c / QUANTUM).round() as i64);
            for k in 0..3 {
                put(&mut raw, zig(q[k] - prev[k]));
            }
            prev = q;
        }
        let mut last = 0i64;
        for t in &self.tris {
            for &i in t {
                put(&mut raw, zig(i as i64 - last));
                last = i as i64;
            }
        }
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 9);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(z)
    }

    pub fn decode(s: &str) -> Result<MeshBody, String> {
        let z = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s.trim())
            .map_err(|e| format!("mesh data: {e}"))?;
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&z, 64 << 20)
            .map_err(|e| format!("mesh data: inflate {:?}", e.status))?;
        if raw.first() != Some(&VERSION) {
            return Err("mesh data: unknown version".into());
        }
        let mut r = Rd { b: &raw, at: 1 };
        let (nv, nt) = (r.get()? as usize, r.get()? as usize);
        // each vertex takes at least 3 bytes and each triangle 3: bounds before allocating
        if nv > MAX_VERTS || nt > 2 * MAX_VERTS || 3 * (nv + nt) > raw.len() {
            return Err("mesh data: sizes do not match the data".into());
        }
        let mut pos = Vec::with_capacity(nv);
        let mut prev = [0i64; 3];
        for _ in 0..nv {
            for c in &mut prev {
                *c += unzig(r.get()?);
            }
            pos.push(v3(
                prev[0] as f64 * QUANTUM,
                prev[1] as f64 * QUANTUM,
                prev[2] as f64 * QUANTUM,
            ));
        }
        let mut tris = Vec::with_capacity(nt);
        let mut last = 0i64;
        for _ in 0..nt {
            let mut t = [0u32; 3];
            for i in &mut t {
                last += unzig(r.get()?);
                if last < 0 || last as usize >= nv {
                    return Err("mesh data: index out of range".into());
                }
                *i = last as u32;
            }
            tris.push(t);
        }
        if r.at != raw.len() {
            return Err("mesh data: trailing bytes".into());
        }
        MeshBody::new(pos, tris)
    }

    /// Merges vertices that coincide at the storage quantum (split normals/UV seams).
    pub fn welded(&self) -> MeshBody {
        let mut ix: std::collections::HashMap<[i64; 3], u32> = std::collections::HashMap::new();
        let mut pos = vec![];
        let map: Vec<u32> = self
            .pos
            .iter()
            .map(|p| {
                let k = [p.x, p.y, p.z].map(|c| (c / QUANTUM).round() as i64);
                *ix.entry(k).or_insert_with(|| {
                    pos.push(*p);
                    pos.len() as u32 - 1
                })
            })
            .collect();
        let tris = self
            .tris
            .iter()
            .map(|t| t.map(|i| map[i as usize]))
            .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
            .collect();
        MeshBody { pos, tris }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> MeshBody {
        let p = |x: f64, y: f64, z: f64| v3(x, y, z);
        let pos = vec![
            p(0., 0., 0.),
            p(1., 0., 0.),
            p(1., 1., 0.),
            p(0., 1., 0.),
            p(0., 0., 1.),
            p(1., 0., 1.),
            p(1., 1., 1.),
            p(0., 1., 1.),
        ];
        let tris = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        MeshBody::new(pos, tris).unwrap()
    }

    #[test]
    fn round_trip_is_exact_at_the_quantum() {
        let mut m = cube();
        m.pos[6] = v3(1.23456, -0.5, 2.0001);
        let s = m.encode();
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "{s}");
        let d = MeshBody::decode(&s).unwrap();
        assert_eq!(d.tris, m.tris);
        for (a, b) in d.pos.iter().zip(&m.pos) {
            assert!((*a - *b).len() <= QUANTUM, "{a:?} {b:?}");
        }
        assert!(cube().closed());
        let mut open = cube();
        open.tris.pop();
        assert!(!open.closed());
    }

    #[test]
    fn corrupt_data_is_an_error_not_a_panic() {
        let s = cube().encode();
        assert!(MeshBody::decode(&s[..s.len() / 2]).is_err());
        assert!(MeshBody::decode("!!!").is_err());
        let raw = miniz_oxide::deflate::compress_to_vec_zlib(&[1, 0xff, 0xff, 0xff, 0x7f, 1], 6);
        let b = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        assert!(MeshBody::decode(&b).is_err());
    }

    #[test]
    fn weld_joins_split_seams() {
        let c = cube();
        let mut pos = vec![];
        let mut tris = vec![];
        for t in &c.tris {
            let b = pos.len() as u32;
            pos.extend(t.iter().map(|&i| c.pos[i as usize]));
            tris.push([b, b + 1, b + 2]);
        }
        let split = MeshBody::new(pos, tris).unwrap();
        assert!(!split.closed());
        let w = split.welded();
        assert_eq!(w.pos.len(), 8);
        assert!(w.closed());
    }
}
