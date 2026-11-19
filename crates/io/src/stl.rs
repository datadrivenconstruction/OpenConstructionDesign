//! STL: binary writer (metres or millimetres), binary and text reader. The file has no units
//! and no materials; the reader welds equal corners so a part comes back as an indexed mesh.

use std::collections::HashMap;
use std::path::Path;

use crate::job::{Progress, CANCELLED};
use crate::meshx::{MeshSet, Part, Units, GENERIC};

fn normal(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> [f32; 3] {
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if l > 0.0 && l.is_finite() {
        n.map(|x| (x / l) as f32)
    } else {
        [0.0; 3]
    }
}

/// Binary STL of every part. Errors when there is nothing to write or more triangles than
/// the format counts.
pub fn write_binary(set: &MeshSet, units: Units) -> Result<Vec<u8>, String> {
    let n = set.triangle_count();
    if n == 0 {
        return Err("nothing to export: the selection has no triangles".into());
    }
    let count = u32::try_from(n).map_err(|_| format!("{n} triangles do not fit in one STL file"))?;
    let k = units.per_metre();
    let mut out = Vec::with_capacity(84 + 50 * n);
    let unit = match units {
        Units::Metre => "m",
        Units::Millimetre => "mm",
    };
    let mut head = format!("binary STL, units {unit}, Z up, {} parts", set.parts.len()).into_bytes();
    head.resize(80, b' ');
    out.extend_from_slice(&head);
    out.extend_from_slice(&count.to_le_bytes());
    for p in &set.parts {
        for t in &p.triangles {
            let [a, b, c] = t.map(|i| p.positions[i as usize].map(|x| x * k));
            for x in normal(a, b, c) {
                out.extend_from_slice(&x.to_le_bytes());
            }
            for v in [a, b, c] {
                for x in v {
                    out.extend_from_slice(&(x as f32).to_le_bytes());
                }
            }
            out.extend_from_slice(&0u16.to_le_bytes());
        }
    }
    Ok(out)
}

pub fn write_file(set: &MeshSet, units: Units, path: &Path) -> Result<(), String> {
    let b = write_binary(set, units)?;
    std::fs::write(path, b).map_err(|e| format!("{}: {e}", path.display()))
}

/// Welds corners with bit-equal coordinates.
#[derive(Default)]
struct Welder {
    at: HashMap<[u64; 3], u32>,
    part: Part,
}

impl Welder {
    fn corner(&mut self, v: [f64; 3]) -> u32 {
        let key = v.map(|x| (x + 0.0).to_bits());
        let next = self.part.positions.len() as u32;
        *self.at.entry(key).or_insert_with(|| {
            self.part.positions.push(v);
            next
        })
    }
    fn tri(&mut self, t: [[f64; 3]; 3]) {
        let i = t.map(|v| self.corner(v));
        if i[0] != i[1] && i[1] != i[2] && i[0] != i[2] {
            self.part.triangles.push(i);
        }
    }
}

fn is_binary(b: &[u8]) -> bool {
    if b.len() < 84 {
        return false;
    }
    let n = u32::from_le_bytes([b[80], b[81], b[82], b[83]]) as u64;
    84 + 50 * n == b.len() as u64
}

/// Reads binary or text STL. `units`: what one file unit is. Degenerate triangles (two equal
/// corners) are dropped and counted in a warning.
pub fn read_bytes(
    bytes: &[u8],
    name: &str,
    units: Units,
    progress: Option<&Progress>,
) -> Result<(MeshSet, Vec<String>), String> {
    let s = 1.0 / units.per_metre();
    let cancelled = || progress.is_some_and(|p| p.is_cancelled());
    if let Some(p) = progress {
        p.phase("job.stage.parse", 0.0, 0.8);
    }
    let mut set = MeshSet::default();
    let mut warns = vec![];
    let mut dropped = 0usize;
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    let finish = |w: Welder, raw: usize, set: &mut MeshSet, dropped: &mut usize| {
        *dropped += raw - w.part.triangles.len();
        set.push(w.part);
    };
    if is_binary(bytes) {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        if let Some(p) = progress {
            p.set_total(n as u64);
        }
        let mut w = Welder::default();
        w.part.key = "stl:0".into();
        w.part.name = stem.clone();
        w.part.category = GENERIC.into();
        for i in 0..n {
            if i % 65_536 == 0 {
                if cancelled() {
                    return Err(CANCELLED.into());
                }
                if let Some(p) = progress {
                    p.set(i as u64);
                }
            }
            let r = &bytes[84 + 50 * i..84 + 50 * i + 50];
            let f = |k: usize| f64::from(f32::from_le_bytes([r[k], r[k + 1], r[k + 2], r[k + 3]])) * s;
            let t = [0, 1, 2].map(|c| [f(12 + 12 * c), f(16 + 12 * c), f(20 + 12 * c)]);
            if t.iter().flatten().any(|x| !x.is_finite()) {
                return Err(format!("{name}: triangle {} has a non-finite coordinate", i + 1));
            }
            w.tri(t);
        }
        finish(w, n, &mut set, &mut dropped);
    } else {
        let text = std::str::from_utf8(bytes).map_err(|_| {
            format!(
                "{name}: not an STL file (binary size does not match its triangle count, and it is not text)"
            )
        })?;
        if !text.trim_start().starts_with("solid") {
            return Err(format!("{name}: not an STL file (no `solid` at the start)"));
        }
        let mut cur: Option<(Welder, usize)> = None;
        let mut corners: Vec<[f64; 3]> = vec![];
        for (ln, line) in text.lines().enumerate() {
            let mut it = line.split_whitespace();
            match it.next() {
                Some("solid") => {
                    if let Some((w, raw)) = cur.take() {
                        finish(w, raw, &mut set, &mut dropped);
                    }
                    let mut w = Welder::default();
                    let rest: Vec<&str> = it.collect();
                    w.part.key = format!("stl:{}", set.parts.len());
                    w.part.name = if rest.is_empty() { stem.clone() } else { rest.join(" ") };
                    w.part.category = GENERIC.into();
                    cur = Some((w, 0));
                }
                Some("vertex") => {
                    let v: Vec<f64> = it.map(|x| x.parse::<f64>()).collect::<Result<_, _>>().map_err(|_| {
                        format!("{name}: line {}: bad vertex `{}`", ln + 1, line.trim())
                    })?;
                    if v.len() != 3 || v.iter().any(|x| !x.is_finite()) {
                        return Err(format!("{name}: line {}: a vertex needs three numbers", ln + 1));
                    }
                    corners.push([v[0] * s, v[1] * s, v[2] * s]);
                }
                Some("endloop") => {
                    let Some((w, raw)) = cur.as_mut() else {
                        return Err(format!("{name}: line {}: facet outside a solid", ln + 1));
                    };
                    if corners.len() < 3 {
                        return Err(format!("{name}: line {}: a facet needs three vertices", ln + 1));
                    }
                    // a polygon facet becomes a fan
                    for k in 1..corners.len() - 1 {
                        w.tri([corners[0], corners[k], corners[k + 1]]);
                        *raw += 1;
                    }
                    corners.clear();
                }
                Some("endsolid") => {
                    if let Some((w, raw)) = cur.take() {
                        finish(w, raw, &mut set, &mut dropped);
                    }
                }
                _ => {}
            }
            if ln % 100_000 == 0 && cancelled() {
                return Err(CANCELLED.into());
            }
        }
        if let Some((w, raw)) = cur.take() {
            warns.push(format!("{name}: the file ends inside `solid {}`", w.part.name));
            finish(w, raw, &mut set, &mut dropped);
        }
    }
    if dropped > 0 {
        warns.push(format!("{dropped} degenerate triangle(s) dropped"));
    }
    if set.parts.is_empty() {
        return Err(format!("{name}: the file has no triangles"));
    }
    Ok((set, warns))
}

pub fn read_file(path: &Path, units: Units, progress: Option<&Progress>) -> Result<(MeshSet, Vec<String>), String> {
    if let Some(p) = progress {
        p.phase("job.stage.read", 0.0, 0.1);
    }
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    read_bytes(&b, &path.display().to_string(), units, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meshx::tests::two_cubes;

    #[test]
    fn binary_round_trip_keeps_counts_and_box_in_both_units() {
        let s = two_cubes();
        for u in [Units::Metre, Units::Millimetre] {
            let b = write_binary(&s, u).unwrap();
            assert_eq!(b.len(), 84 + 50 * 24);
            let (back, w) = read_bytes(&b, "a.stl", u, None).unwrap();
            assert!(w.is_empty(), "{w:?}");
            assert_eq!(back.triangle_count(), 24);
            let (lo, hi) = back.bbox().unwrap();
            let (elo, ehi) = s.bbox().unwrap();
            for k in 0..3 {
                assert!((lo[k] - elo[k]).abs() < 1e-6 && (hi[k] - ehi[k]).abs() < 1e-6);
            }
            assert_eq!(back.parts[0].positions.len(), 16, "welded corners of two cubes in one part");
        }
    }

    #[test]
    fn millimetres_scale_the_numbers() {
        let b = write_binary(&two_cubes(), Units::Millimetre).unwrap();
        let (m, _) = read_bytes(&b, "a.stl", Units::Metre, None).unwrap();
        assert!((m.bbox().unwrap().1[0] - 4000.0).abs() < 1e-3);
    }

    #[test]
    fn text_stl_with_two_solids() {
        let t = "solid a\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid a\n\
                 solid b\nfacet normal 0 0 1\nouter loop\nvertex 0 0 1\nvertex 1 0 1\nvertex 1 1 1\nvertex 0 1 1\nendloop\nendfacet\nendsolid b\n";
        let (s, _) = read_bytes(t.as_bytes(), "t.stl", Units::Metre, None).unwrap();
        assert_eq!(s.parts.len(), 2);
        assert_eq!(s.parts[1].name, "b");
        assert_eq!(s.triangle_count(), 3);
    }

    #[test]
    fn errors_are_plain() {
        let e = read_bytes(b"\x00\x01garbage", "x.stl", Units::Metre, None).unwrap_err();
        assert!(e.contains("not an STL file"), "{e}");
        let e = read_bytes(b"solid a\nfacet\nouter loop\nvertex 0 zero 0\n", "x.stl", Units::Metre, None).unwrap_err();
        assert!(e.contains("line 4"), "{e}");
        let e = write_binary(&MeshSet::default(), Units::Metre).unwrap_err();
        assert!(e.contains("nothing to export"), "{e}");
        let mut b = write_binary(&two_cubes(), Units::Metre).unwrap();
        b.truncate(b.len() - 10);
        assert!(read_bytes(&b, "cut.stl", Units::Metre, None).is_err());
    }

    #[test]
    fn cancel_stops_the_reader() {
        let b = write_binary(&two_cubes(), Units::Metre).unwrap();
        let p = Progress::new();
        p.cancel();
        assert_eq!(read_bytes(&b, "a.stl", Units::Metre, Some(&p)).unwrap_err(), CANCELLED);
    }
}
