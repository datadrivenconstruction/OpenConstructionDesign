// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! OBJ + MTL: writer (one `g <element key>` per part, `usemtl` per material, MTL colours) and
//! reader (groups, objects, polygons as fans, negative indices, MTL `Kd`/`d`/`Tr`).
//! OBJ has no up axis; the common reading is Y up, so both sides turn Z up <-> Y up by default.

use std::fmt::Write as _;
use std::path::Path;

use crate::job::{Progress, CANCELLED};
use crate::meshx::{MeshSet, Part, Units, GENERIC};

/// Material of parts that have none (written only when needed).
pub const NO_MATERIAL: &str = "ddc_default";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObjOptions {
    pub units: Units,
    /// Y up in the file (the model is Z up).
    pub y_up: bool,
}

impl Default for ObjOptions {
    fn default() -> Self {
        ObjOptions {
            units: Units::Metre,
            y_up: true,
        }
    }
}

/// A name usable as one OBJ/MTL token: whitespace becomes `_`, empty becomes `_`.
fn token(s: &str) -> String {
    let t: String = s
        .trim()
        .chars()
        .map(|c| if c.is_whitespace() || c.is_control() { '_' } else { c })
        .collect();
    if t.is_empty() {
        "_".into()
    } else {
        t
    }
}

/// `(obj, mtl)` texts; the OBJ refers to `mtl_name` with `mtllib`.
pub fn write(set: &MeshSet, mtl_name: &str, o: ObjOptions) -> Result<(String, String), String> {
    if set.triangle_count() == 0 {
        return Err("nothing to export: the selection has no triangles".into());
    }
    let k = o.units.per_metre();
    let mut obj = String::with_capacity(set.triangle_count() * 60);
    let mut mtl = String::new();
    let unit = if o.units == Units::Metre { "m" } else { "mm" };
    let up = if o.y_up { "Y" } else { "Z" };
    let _ = writeln!(obj, "# units {unit}, {up} up, {} parts", set.parts.len());
    let _ = writeln!(obj, "mtllib {}", token(mtl_name));
    let names: Vec<String> = set.materials.iter().map(|m| token(&m.name)).collect();
    let mut mats: Vec<(&str, [f32; 4])> = names
        .iter()
        .map(String::as_str)
        .zip(set.materials.iter().map(|m| m.rgba))
        .collect();
    // `usemtl` stays in force across groups, so a part without a material needs one of its own
    if set.parts.iter().any(|p| p.material.is_none()) {
        mats.push((NO_MATERIAL, [0.8, 0.8, 0.8, 1.0]));
    }
    for (n, [r, g, b, a]) in mats {
        let _ = writeln!(mtl, "newmtl {n}\nKa 0 0 0\nKd {r:.6} {g:.6} {b:.6}\nKs 0 0 0\nd {a:.6}\nillum 1\n");
    }
    let mut base = 1usize;
    for p in &set.parts {
        let _ = writeln!(obj, "o {}", token(&p.name));
        let _ = writeln!(obj, "g {}", token(&p.key));
        let n = p.material.and_then(|m| names.get(m)).map_or(NO_MATERIAL, String::as_str);
        let _ = writeln!(obj, "usemtl {n}");
        for v in &p.positions {
            let [x, y, z] = v.map(|c| c * k);
            if o.y_up {
                let _ = writeln!(obj, "v {x} {z} {}", -y);
            } else {
                let _ = writeln!(obj, "v {x} {y} {z}");
            }
        }
        for t in &p.triangles {
            let [a, b, c] = t.map(|i| i as usize + base);
            let _ = writeln!(obj, "f {a} {b} {c}");
        }
        base += p.positions.len();
    }
    Ok((obj, mtl))
}

/// Writes `path` and the MTL next to it (same stem, `.mtl`).
pub fn write_file(set: &MeshSet, path: &Path, o: ObjOptions) -> Result<(), String> {
    let mtl_path = path.with_extension("mtl");
    let mtl_name = mtl_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "materials.mtl".into());
    let (obj, mtl) = write(set, &mtl_name, o)?;
    std::fs::write(path, obj).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::write(&mtl_path, mtl).map_err(|e| format!("{}: {e}", mtl_path.display()))
}

/// MTL text into `(name, linear rgba)`.
pub fn parse_mtl(text: &str) -> Vec<(String, [f32; 4])> {
    let mut out: Vec<(String, [f32; 4])> = vec![];
    for line in text.lines() {
        let mut it = line.split_whitespace();
        // num-lint: OBJ file numbers, invariant form by the file format
        let num = |s: Option<&str>| s.and_then(|x| x.parse::<f32>().ok()).filter(|x| x.is_finite());
        match it.next() {
            Some("newmtl") => out.push((it.collect::<Vec<_>>().join(" "), [0.8, 0.8, 0.8, 1.0])),
            Some("Kd") => {
                if let (Some(m), Some(r)) = (out.last_mut(), num(it.next())) {
                    let g = num(it.next()).unwrap_or(r);
                    let b = num(it.next()).unwrap_or(r);
                    m.1[0] = r.clamp(0.0, 1.0);
                    m.1[1] = g.clamp(0.0, 1.0);
                    m.1[2] = b.clamp(0.0, 1.0);
                }
            }
            Some("d") => {
                if let (Some(m), Some(a)) = (out.last_mut(), num(it.next())) {
                    m.1[3] = a.clamp(0.0, 1.0);
                }
            }
            Some("Tr") => {
                if let (Some(m), Some(t)) = (out.last_mut(), num(it.next())) {
                    m.1[3] = (1.0 - t).clamp(0.0, 1.0);
                }
            }
            _ => {}
        }
    }
    out
}

struct Reader<'a> {
    name: &'a str,
    set: MeshSet,
    /// all `v` lines, file units
    verts: Vec<[f64; 3]>,
    /// global vertex -> local index of the current part
    local: Vec<u32>,
    part: Option<Part>,
    group: String,
    object: String,
    material: Option<usize>,
    groups_seen: std::collections::HashMap<String, usize>,
}

impl Reader<'_> {
    fn flush(&mut self) {
        if let Some(p) = self.part.take() {
            if !p.triangles.is_empty() {
                self.set.parts.push(p);
            }
        }
        self.local.clear();
    }

    fn start(&mut self) {
        self.flush();
        let n = self.groups_seen.entry(self.group.clone()).or_insert(0);
        *n += 1;
        let key = if *n == 1 {
            self.group.clone()
        } else {
            format!("{}#{}", self.group, n)
        };
        self.part = Some(Part {
            key,
            name: if self.object.is_empty() { self.group.clone() } else { self.object.clone() },
            category: GENERIC.into(),
            material: self.material,
            ..Default::default()
        });
    }

    fn index(&self, s: &str, ln: usize) -> Result<usize, String> {
        let v = s.split('/').next().unwrap_or("");
        let i: i64 = v
            .parse()
            .map_err(|_| format!("{}: line {ln}: bad face index `{s}`", self.name))?;
        let n = self.verts.len() as i64;
        let k = if i > 0 { i - 1 } else { n + i };
        if i == 0 || k < 0 || k >= n {
            return Err(format!(
                "{}: line {ln}: face index {i} is outside the {n} vertices read so far",
                self.name
            ));
        }
        Ok(k as usize)
    }

    fn corner(&mut self, g: usize, scale: f64, y_up: bool) -> u32 {
        if self.local.len() < self.verts.len() {
            self.local.resize(self.verts.len(), u32::MAX);
        }
        let p = self.part.as_mut().expect("part started before faces");
        if self.local[g] == u32::MAX {
            let [x, y, z] = self.verts[g].map(|c| c * scale);
            p.positions.push(if y_up { [x, -z, y] } else { [x, y, z] });
            self.local[g] = (p.positions.len() - 1) as u32;
        }
        self.local[g]
    }
}

/// Reads OBJ text. `mtl` resolves an `mtllib` name to its text (None: not found, warned).
pub fn read_str(
    text: &str,
    name: &str,
    o: ObjOptions,
    mtl: &mut dyn FnMut(&str) -> Option<String>,
    progress: Option<&Progress>,
) -> Result<(MeshSet, Vec<String>), String> {
    let scale = 1.0 / o.units.per_metre();
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    let mut r = Reader {
        name,
        set: MeshSet::default(),
        verts: vec![],
        local: vec![],
        part: None,
        group: stem,
        object: String::new(),
        material: None,
        groups_seen: Default::default(),
    };
    let mut warns = vec![];
    let mut lib: Vec<(String, [f32; 4])> = vec![];
    let mut skipped_lines = 0usize;
    if let Some(p) = progress {
        p.phase("job.stage.parse", 0.1, 0.8);
        p.set_bytes(true);
        p.set_total(text.len() as u64);
    }
    let mut at = 0usize;
    for (i, raw) in text.lines().enumerate() {
        let ln = i + 1;
        at += raw.len() + 1;
        if ln % 50_000 == 0 {
            if progress.is_some_and(|p| p.is_cancelled()) {
                return Err(CANCELLED.into());
            }
            if let Some(p) = progress {
                p.set(at as u64);
            }
        }
        let line = raw.split('#').next().unwrap_or("").trim();
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                let v: Vec<f64> = it.take(3).map(str::parse).collect::<Result<_, _>>().map_err(|_| {
                    format!("{name}: line {ln}: bad vertex `{line}`")
                })?;
                if v.len() != 3 || v.iter().any(|x| !x.is_finite()) {
                    return Err(format!("{name}: line {ln}: a vertex needs three finite numbers"));
                }
                r.verts.push([v[0], v[1], v[2]]);
            }
            Some("f") => {
                let ix: Vec<usize> = it.map(|s| r.index(s, ln)).collect::<Result<_, _>>()?;
                if ix.len() < 3 {
                    return Err(format!("{name}: line {ln}: a face needs three corners"));
                }
                if r.part.is_none() {
                    r.start();
                }
                let c: Vec<u32> = ix.iter().map(|&g| r.corner(g, scale, o.y_up)).collect();
                let p = r.part.as_mut().expect("started");
                for k in 1..c.len() - 1 {
                    let t = [c[0], c[k], c[k + 1]];
                    if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] {
                        p.triangles.push(t);
                    }
                }
            }
            Some("g") => {
                let g = it.collect::<Vec<_>>().join(" ");
                r.group = if g.is_empty() { "default".into() } else { g };
                r.start();
            }
            Some("o") => {
                r.object = it.collect::<Vec<_>>().join(" ");
                r.start();
            }
            Some("usemtl") => {
                let m = it.collect::<Vec<_>>().join(" ");
                let rgba = lib.iter().find(|x| x.0 == m).map(|x| x.1);
                if rgba.is_none() {
                    warns.push(format!("material `{m}` is not in the material library"));
                }
                let idx = r.set.material(&m, rgba.unwrap_or([0.8, 0.8, 0.8, 1.0]));
                r.material = Some(idx);
                let has_faces = r.part.as_ref().is_some_and(|p| !p.triangles.is_empty());
                if has_faces {
                    r.start();
                } else if let Some(p) = r.part.as_mut() {
                    p.material = Some(idx);
                }
            }
            Some("mtllib") => {
                for f in it {
                    match mtl(f) {
                        Some(t) => lib.extend(parse_mtl(&t)),
                        None => warns.push(format!("material library `{f}` not found")),
                    }
                }
            }
            Some("vt" | "vn" | "vp" | "s" | "l" | "p") | None => {}
            Some(_) => skipped_lines += 1,
        }
    }
    r.flush();
    if skipped_lines > 0 {
        warns.push(format!("{skipped_lines} line(s) with unsupported statements skipped"));
    }
    if r.set.parts.is_empty() {
        return Err(format!("{name}: the file has no faces"));
    }
    Ok((r.set, warns))
}

pub fn read_file(path: &Path, o: ObjOptions, progress: Option<&Progress>) -> Result<(MeshSet, Vec<String>), String> {
    if let Some(p) = progress {
        p.phase("job.stage.read", 0.0, 0.1);
    }
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&b);
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut mtl = |f: &str| -> Option<String> {
        let p = Path::new(f);
        if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return None;
        }
        std::fs::read(dir.join(p)).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
    };
    read_str(&text, &path.display().to_string(), o, &mut mtl, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meshx::tests::two_cubes;

    fn round(o: ObjOptions) {
        let s = two_cubes();
        let (obj, mtl) = write(&s, "two.mtl", o).unwrap();
        assert!(obj.contains("g wall:1") && obj.contains("usemtl red"));
        let (back, w) = read_str(&obj, "two.obj", o, &mut |f| (f == "two.mtl").then(|| mtl.clone()), None).unwrap();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(back.triangle_count(), s.triangle_count());
        assert_eq!(back.parts.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(), ["wall:1", "mesh:2"]);
        let (a, b) = (back.bbox().unwrap(), s.bbox().unwrap());
        for k in 0..3 {
            assert!((a.0[k] - b.0[k]).abs() < 1e-9 && (a.1[k] - b.1[k]).abs() < 1e-9);
        }
        let red = back.parts[0].material.unwrap();
        assert_eq!(back.materials[red].rgba, [1.0, 0.0, 0.0, 1.0]);
        let other = back.parts[1].material.unwrap();
        assert_eq!(back.materials[other].name, NO_MATERIAL);
    }

    #[test]
    fn round_trip_keeps_groups_materials_and_box() {
        round(ObjOptions::default());
        round(ObjOptions { units: Units::Millimetre, y_up: false });
    }

    #[test]
    fn y_up_file_turns_to_z_up() {
        let t = "v 0 0 0\nv 1 0 0\nv 0 5 0\nf 1 2 3\n";
        let (s, _) = read_str(t, "a.obj", ObjOptions::default(), &mut |_| None, None).unwrap();
        assert_eq!(s.bbox().unwrap().1, [1.0, 0.0, 5.0]);
    }

    #[test]
    fn quads_negative_indices_and_slashes() {
        let t = "o box\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nf -4/1 -3/1 -2/1 -1/1\nf 1//1 2//1 3//1\n";
        let (s, _) = read_str(t, "q.obj", ObjOptions { units: Units::Metre, y_up: false }, &mut |_| None, None).unwrap();
        assert_eq!(s.triangle_count(), 3);
        assert_eq!(s.parts[0].name, "box");
        assert_eq!(s.parts[0].positions.len(), 4);
    }

    #[test]
    fn errors_name_the_line() {
        let e = read_str("v 0 0 0\nf 1 2 3\n", "e.obj", ObjOptions::default(), &mut |_| None, None).unwrap_err();
        assert!(e.contains("line 2") && e.contains("outside"), "{e}");
        let e = read_str("v 0 x 0\n", "e.obj", ObjOptions::default(), &mut |_| None, None).unwrap_err();
        assert!(e.contains("line 1"), "{e}");
        let e = read_str("# empty\n", "e.obj", ObjOptions::default(), &mut |_| None, None).unwrap_err();
        assert!(e.contains("no faces"), "{e}");
        let (_, w) = read_str("mtllib gone.mtl\nusemtl x\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n", "w.obj", ObjOptions::default(), &mut |_| None, None).unwrap();
        assert_eq!(w.len(), 2, "{w:?}");
    }
}
