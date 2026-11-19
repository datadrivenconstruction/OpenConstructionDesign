// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 3MF writer (core specification 2015/02, docs/IO4.md): one mesh object per part, base
//! materials for colours, model unit from the options, every part checked for being
//! watertight and outward oriented before it is written.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;

use crate::meshx::{linear_to_srgb8, MeshSet, Part};

pub const CORE_NS: &str = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02";
pub const MODEL_PATH: &str = "3D/3dmodel.model";
pub const REL_TYPE: &str = "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel";

/// Model units of the core specification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Unit {
    Micron,
    #[default]
    Millimeter,
    Centimeter,
    Inch,
    Foot,
    Meter,
}

impl Unit {
    pub const ALL: [Unit; 6] = [Unit::Micron, Unit::Millimeter, Unit::Centimeter, Unit::Inch, Unit::Foot, Unit::Meter];

    pub fn name(self) -> &'static str {
        match self {
            Unit::Micron => "micron",
            Unit::Millimeter => "millimeter",
            Unit::Centimeter => "centimeter",
            Unit::Inch => "inch",
            Unit::Foot => "foot",
            Unit::Meter => "meter",
        }
    }
    pub fn per_metre(self) -> f64 {
        match self {
            Unit::Micron => 1e6,
            Unit::Millimeter => 1e3,
            Unit::Centimeter => 1e2,
            Unit::Inch => 1.0 / 0.0254,
            Unit::Foot => 1.0 / 0.3048,
            Unit::Meter => 1.0,
        }
    }
    pub fn parse(s: &str) -> Option<Unit> {
        let s = s.to_ascii_lowercase();
        Unit::ALL.into_iter().find(|u| {
            u.name() == s
                || matches!(
                    (u, s.as_str()),
                    (Unit::Micron, "um") | (Unit::Millimeter, "mm") | (Unit::Centimeter, "cm") | (Unit::Inch, "in") | (Unit::Foot, "ft") | (Unit::Meter, "m")
                )
        })
    }
}

/// What to do with a part that is not a closed, consistently oriented surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Policy {
    /// Write it and list it in the report.
    #[default]
    Report,
    /// Leave it out and list it.
    Skip,
    /// Refuse the whole export.
    Require,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub unit: Unit,
    pub policy: Policy,
    /// Model title (metadata).
    pub title: String,
}

/// Check of one part after welding equal positions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartCheck {
    pub name: String,
    pub triangles: usize,
    /// Degenerate triangles dropped (repeated vertex or zero area).
    pub degenerate: usize,
    /// Edges with one triangle only.
    pub open_edges: usize,
    /// Edges with more than two triangles, or two running the same way.
    pub bad_edges: usize,
    /// Triangles turned so that the normals point out (negative volume before).
    pub reoriented: bool,
    pub written: bool,
}

impl PartCheck {
    pub fn watertight(&self) -> bool {
        self.open_edges == 0 && self.bad_edges == 0 && self.triangles > 0
    }
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub parts: Vec<PartCheck>,
}

impl Report {
    pub fn leaky(&self) -> impl Iterator<Item = &PartCheck> {
        self.parts.iter().filter(|p| !p.watertight())
    }
}

/// Welded mesh of one part, in metres.
struct Welded {
    pos: Vec<[f64; 3]>,
    tri: Vec<[u32; 3]>,
}

fn weld(p: &Part, chk: &mut PartCheck) -> Welded {
    let mut ids: HashMap<[u64; 3], u32> = HashMap::new();
    let mut pos = vec![];
    let map: Vec<u32> = p
        .positions
        .iter()
        .map(|q| {
            let k = q.map(|c| (c + 0.0).to_bits());
            *ids.entry(k).or_insert_with(|| {
                pos.push(*q);
                (pos.len() - 1) as u32
            })
        })
        .collect();
    let mut tri = vec![];
    for t in &p.triangles {
        let Some(v) = t.iter().map(|&i| map.get(i as usize).copied()).collect::<Option<Vec<u32>>>() else {
            chk.degenerate += 1;
            continue;
        };
        let [a, b, c] = [v[0], v[1], v[2]];
        let (pa, pb, pc) = (pos[a as usize], pos[b as usize], pos[c as usize]);
        let u = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
        let w = [pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2]];
        let n = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
        if a == b || b == c || a == c || n.iter().all(|x| *x == 0.0) {
            chk.degenerate += 1;
            continue;
        }
        tri.push([a, b, c]);
    }
    Welded { pos, tri }
}

fn check(w: &mut Welded, chk: &mut PartCheck) {
    chk.triangles = w.tri.len();
    let mut dir: HashMap<(u32, u32), u32> = HashMap::new();
    for t in &w.tri {
        for k in 0..3 {
            *dir.entry((t[k], t[(k + 1) % 3])).or_default() += 1;
        }
    }
    let mut seen: HashMap<(u32, u32), ()> = HashMap::new();
    for (&(a, b), &n) in &dir {
        let key = (a.min(b), a.max(b));
        if seen.insert(key, ()).is_some() {
            continue;
        }
        let back = dir.get(&(b, a)).copied().unwrap_or(0);
        match (n, back) {
            (1, 1) => {}
            (1, 0) | (0, 1) => chk.open_edges += 1,
            _ => chk.bad_edges += 1,
        }
    }
    if chk.watertight() {
        let vol: f64 = w
            .tri
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| w.pos[i as usize]);
                a[0] * (b[1] * c[2] - b[2] * c[1]) + a[1] * (b[2] * c[0] - b[0] * c[2]) + a[2] * (b[0] * c[1] - b[1] * c[0])
            })
            .sum();
        if vol < 0.0 {
            for t in &mut w.tri {
                t.swap(1, 2);
            }
            chk.reoriented = true;
        }
    }
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => o.push(' '),
            c => o.push(c),
        }
    }
    o
}

/// Number with at most nine decimals, no trailing zeros.
fn num(x: f64) -> String {
    let r = (x * 1e9).round() / 1e9;
    let r = if r == 0.0 { 0.0 } else { r };
    format!("{r}")
}

/// The model part (3D/3dmodel.model) and the check of every part.
pub fn model_xml(ms: &MeshSet, opt: &Options) -> Result<(String, Report), String> {
    let k = opt.unit.per_metre();
    let mut report = Report::default();
    let mut objects = String::new();
    let mut items = String::new();
    // base materials: resource 1, one entry per material of the set
    let has_mat = !ms.materials.is_empty();
    let mut next_id = if has_mat { 2 } else { 1 };
    for p in &ms.parts {
        let mut chk = PartCheck {
            name: p.name.clone(),
            ..Default::default()
        };
        let mut w = weld(p, &mut chk);
        check(&mut w, &mut chk);
        let ok = chk.watertight();
        if !ok && opt.policy == Policy::Require {
            return Err(format!(
                "part '{}' is not watertight: {} open and {} bad edges",
                p.name, chk.open_edges, chk.bad_edges
            ));
        }
        if (ok || opt.policy == Policy::Report) && !w.tri.is_empty() {
            let id = next_id;
            next_id += 1;
            let mat = match p.material.filter(|&m| m < ms.materials.len()) {
                Some(m) => format!(" pid=\"1\" pindex=\"{m}\""),
                None => String::new(),
            };
            let _ = write!(objects, "<object id=\"{id}\" type=\"model\" name=\"{}\"{mat}><mesh><vertices>", esc(&p.name));
            for q in &w.pos {
                let _ = write!(objects, "<vertex x=\"{}\" y=\"{}\" z=\"{}\"/>", num(q[0] * k), num(q[1] * k), num(q[2] * k));
            }
            objects.push_str("</vertices><triangles>");
            for t in &w.tri {
                let _ = write!(objects, "<triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>", t[0], t[1], t[2]);
            }
            objects.push_str("</triangles></mesh></object>\n");
            let _ = writeln!(items, "<item objectid=\"{id}\"/>");
            chk.written = true;
        }
        report.parts.push(chk);
    }
    if items.is_empty() {
        return Err("no part to write: every part is empty or left out as not watertight".into());
    }
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(x, "<model unit=\"{}\" xml:lang=\"en-US\" xmlns=\"{CORE_NS}\">", opt.unit.name());
    if !opt.title.is_empty() {
        let _ = writeln!(x, "<metadata name=\"Title\">{}</metadata>", esc(&opt.title));
    }
    x.push_str("<resources>\n");
    if has_mat {
        x.push_str("<basematerials id=\"1\">");
        for m in &ms.materials {
            let [r, g, b] = [0, 1, 2].map(|i| linear_to_srgb8(m.rgba[i]));
            let a = (m.rgba[3].clamp(0.0, 1.0) * 255.0).round() as u8;
            let alpha = if a < 255 { format!("{a:02X}") } else { String::new() };
            let _ = write!(x, "<base name=\"{}\" displaycolor=\"#{r:02X}{g:02X}{b:02X}{alpha}\"/>", esc(&m.name));
        }
        x.push_str("</basematerials>\n");
    }
    x.push_str(&objects);
    x.push_str("</resources>\n<build>\n");
    x.push_str(&items);
    x.push_str("</build>\n</model>\n");
    Ok((x, report))
}

/// The 3MF package bytes.
pub fn write(ms: &MeshSet, opt: &Options) -> Result<(Vec<u8>, Report), String> {
    let (model, report) = model_xml(ms, opt)?;
    let types = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
        <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
        <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
        <Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/>\
        </Types>\n";
    let rels = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Target=\"/{MODEL_PATH}\" Id=\"rel0\" Type=\"{REL_TYPE}\"/></Relationships>\n"
    );
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in [("[Content_Types].xml", types), ("_rels/.rels", rels.as_str()), (MODEL_PATH, model.as_str())] {
        z.start_file(name, o).map_err(|e| e.to_string())?;
        z.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    }
    let cur = z.finish().map_err(|e| e.to_string())?;
    Ok((cur.into_inner(), report))
}

/// Writes the package next to `path` and renames it into place.
pub fn write_file(ms: &MeshSet, opt: &Options, path: &std::path::Path) -> Result<Report, String> {
    let (bytes, report) = write(ms, opt)?;
    crate::fsutil::replace_durably_bytes(path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(report)
}

#[cfg(test)]
#[path = "threemf_tests.rs"]
mod tests;
