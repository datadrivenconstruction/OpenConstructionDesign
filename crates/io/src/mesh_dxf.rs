//! DXF of a [`MeshSet`]: plan cuts and vertical sections as LINEs, the bodies as `3DFACE`s,
//! one layer per category (`<category>` for cut lines, `3D-<category>` for faces). Views sit
//! side by side to the right of the model so they never overlap the 3D faces.

use std::collections::BTreeMap;
use std::path::Path;

use crate::dxf::{DxfDocument, Entity, HeaderVar, Layer, Line, Pair, Text};
use crate::meshx::{linear_to_srgb8, MeshSet, Units};

/// Horizontal cut: everything crossing `z` (m), drawn in plan.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanCut {
    pub name: String,
    pub z: f64,
}

/// Vertical cut through `origin` along `dir` (plan, unit or not): drawn as (along, z).
#[derive(Clone, Debug, PartialEq)]
pub struct SectionCut {
    pub name: String,
    pub origin: [f64; 2],
    pub dir: [f64; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub struct DxfOptions {
    pub plans: Vec<PlanCut>,
    pub sections: Vec<SectionCut>,
    pub faces: bool,
    pub units: Units,
}

/// Height of a plan cut above its level (m).
pub const PLAN_CUT: f64 = 1.2;

impl DxfOptions {
    /// One plan per level (`(name, elevation)`) at [`PLAN_CUT`], a section along X and one
    /// along Y through the middle of the set, and the 3D faces. Without levels: one plan at
    /// [`PLAN_CUT`] above the lowest point.
    pub fn standard(set: &MeshSet, levels: &[(String, f64)], units: Units) -> Self {
        let (lo, hi) = set.bbox().unwrap_or(([0.0; 3], [0.0; 3]));
        let mut plans: Vec<PlanCut> = levels
            .iter()
            .map(|(n, e)| PlanCut {
                name: n.clone(),
                z: e + PLAN_CUT,
            })
            .collect();
        if plans.is_empty() {
            plans.push(PlanCut {
                name: "Plan".into(),
                z: lo[2] + PLAN_CUT,
            });
        }
        let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
        DxfOptions {
            plans,
            sections: vec![
                SectionCut {
                    name: "Section X".into(),
                    origin: c,
                    dir: [1.0, 0.0],
                },
                SectionCut {
                    name: "Section Y".into(),
                    origin: c,
                    dir: [0.0, 1.0],
                },
            ],
            faces: true,
            units,
        }
    }
}

/// What went into the drawing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DxfStats {
    pub lines: usize,
    pub faces: usize,
    pub layers: usize,
    pub views: usize,
}

/// Layer names may not hold `<>/\":;?*|=,` or a backquote.
pub fn layer_name(s: &str) -> String {
    let t: String = s
        .trim()
        .chars()
        .map(|c| if "<>/\\\":;?*|=,`".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    if t.is_empty() {
        "Generic".into()
    } else {
        t
    }
}

/// Segment where the triangle crosses the plane `d(p) = 0`. A corner exactly on the plane
/// counts as above: an edge lying in the plane comes from one triangle only, a touching
/// corner gives two equal points and no segment.
fn cut_tri(d: [f64; 3], p: [[f64; 3]; 3]) -> Option<([f64; 3], [f64; 3])> {
    let up = d.map(|x| x >= 0.0);
    if up[0] == up[1] && up[1] == up[2] {
        return None;
    }
    let mut pts = vec![];
    for (a, b) in [(0, 1), (1, 2), (2, 0)] {
        if up[a] != up[b] {
            let t = d[a] / (d[a] - d[b]);
            pts.push([0, 1, 2].map(|k| p[a][k] + t * (p[b][k] - p[a][k])));
        }
    }
    match pts.as_slice() {
        [a, b] if a != b => Some((*a, *b)),
        _ => None,
    }
}

fn header(name: &str, code: i32, v: impl Into<String>) -> HeaderVar {
    HeaderVar {
        name: name.into(),
        values: vec![Pair::new(code, v)],
    }
}

fn line(layer: &str, a: [f64; 3], b: [f64; 3]) -> Entity {
    Entity::Line(Line {
        common: crate::dxf::Common::on_layer(layer),
        start: a,
        end: b,
    })
}

/// The drawing; errors when the set is empty.
pub fn build(set: &MeshSet, o: &DxfOptions) -> Result<(DxfDocument, DxfStats), String> {
    let Some((lo, hi)) = set.bbox() else {
        return Err("nothing to export: the selection has no triangles".into());
    };
    let k = o.units.per_metre();
    let mut doc = DxfDocument::default();
    let insunits = match o.units {
        Units::Metre => 6,
        Units::Millimetre => 4,
    };
    doc.header.push(header("$INSUNITS", 70, insunits.to_string()));
    doc.header.push(header("$MEASUREMENT", 70, "1"));
    let mut stats = DxfStats::default();
    let mut layers: BTreeMap<String, Option<i32>> = BTreeMap::new();
    let colour = |p: &crate::meshx::Part| -> Option<i32> {
        let m = set.materials.get(p.material?)?;
        let c = m.rgba.map(linear_to_srgb8);
        Some((i32::from(c[0]) << 16) | (i32::from(c[1]) << 8) | i32::from(c[2]))
    };
    let s = |v: [f64; 3]| v.map(|x| x * k);

    let gap = ((hi[0] - lo[0]).max(hi[1] - lo[1]) * 0.25).max(2.0);
    let mut x0 = hi[0] + gap;
    let title_h = (gap * 0.1).max(0.25);
    let mut view = |doc: &mut DxfDocument,
                    layers: &mut BTreeMap<String, Option<i32>>,
                    stats: &mut DxfStats,
                    title: &str,
                    segs: Vec<(String, Option<i32>, [f64; 2], [f64; 2])>| {
        if segs.is_empty() {
            return;
        }
        let (mut a, mut b) = ([f64::MAX; 2], [f64::MIN; 2]);
        for (_, _, p, q) in &segs {
            for v in [p, q] {
                for i in 0..2 {
                    a[i] = a[i].min(v[i]);
                    b[i] = b[i].max(v[i]);
                }
            }
        }
        let shift = [x0 - a[0], lo[1] - a[1]];
        for (layer, rgb, p, q) in segs {
            layers.entry(layer.clone()).or_insert(rgb);
            doc.entities.push(line(
                &layer,
                s([p[0] + shift[0], p[1] + shift[1], 0.0]),
                s([q[0] + shift[0], q[1] + shift[1], 0.0]),
            ));
            stats.lines += 1;
        }
        layers.entry("VIEW-TITLES".into()).or_insert(None);
        doc.entities.push(Entity::Text(Text {
            common: crate::dxf::Common::on_layer("VIEW-TITLES"),
            pos: s([x0, lo[1] - 2.0 * title_h, 0.0]),
            height: title_h * k,
            text: title.to_string(),
            rotation: 0.0,
            style: None,
        }));
        stats.views += 1;
        x0 += (b[0] - a[0]) + gap;
    };

    for pc in &o.plans {
        let mut segs = vec![];
        for p in &set.parts {
            let layer = layer_name(&p.category);
            for t in &p.triangles {
                let c = t.map(|i| p.positions[i as usize]);
                if let Some((a, b)) = cut_tri(c.map(|v| v[2] - pc.z), c) {
                    segs.push((layer.clone(), colour(p), [a[0], a[1]], [b[0], b[1]]));
                }
            }
        }
        view(&mut doc, &mut layers, &mut stats, &format!("Plan {}", pc.name), segs);
    }
    for sc in &o.sections {
        let l = (sc.dir[0] * sc.dir[0] + sc.dir[1] * sc.dir[1]).sqrt();
        if !(l > 0.0 && l.is_finite()) {
            return Err(format!("section `{}`: the direction has no length", sc.name));
        }
        let u = [sc.dir[0] / l, sc.dir[1] / l];
        // the cut plane holds `u` and Z; its normal is `u` turned a quarter left
        let n = [-u[1], u[0]];
        let mut segs = vec![];
        for p in &set.parts {
            let layer = layer_name(&p.category);
            for t in &p.triangles {
                let c = t.map(|i| p.positions[i as usize]);
                let d = c.map(|v| (v[0] - sc.origin[0]) * n[0] + (v[1] - sc.origin[1]) * n[1]);
                if let Some((a, b)) = cut_tri(d, c) {
                    let along = |v: [f64; 3]| (v[0] - sc.origin[0]) * u[0] + (v[1] - sc.origin[1]) * u[1];
                    segs.push((layer.clone(), colour(p), [along(a), a[2]], [along(b), b[2]]));
                }
            }
        }
        view(&mut doc, &mut layers, &mut stats, &sc.name, segs);
    }
    if o.faces {
        for p in &set.parts {
            let layer = format!("3D-{}", layer_name(&p.category));
            layers.entry(layer.clone()).or_insert(colour(p));
            for t in &p.triangles {
                let c = t.map(|i| s(p.positions[i as usize]));
                let mut pairs = vec![
                    Pair::new(0, "3DFACE"),
                    Pair::new(100, "AcDbEntity"),
                    Pair::new(8, layer.clone()),
                    Pair::new(100, "AcDbFace"),
                ];
                for (j, v) in [c[0], c[1], c[2], c[2]].iter().enumerate() {
                    for (a, x) in v.iter().enumerate() {
                        pairs.push(Pair::new(10 + 10 * a as i32 + j as i32, format!("{x}")));
                    }
                }
                doc.entities.push(Entity::Unknown(pairs));
                stats.faces += 1;
            }
        }
    }
    for (i, (name, rgb)) in layers.into_iter().enumerate() {
        let mut extra = vec![];
        if let Some(c) = rgb {
            extra.push(Pair::new(420, c.to_string()));
        }
        doc.layers.push(Layer {
            name,
            flags: 0,
            color: 1 + (i as i32 % 9),
            linetype: "Continuous".into(),
            extra,
        });
    }
    stats.layers = doc.layers.len();
    Ok((doc, stats))
}

pub fn write_string(set: &MeshSet, o: &DxfOptions) -> Result<(String, DxfStats), String> {
    let (doc, st) = build(set, o)?;
    Ok((crate::dxf::write_string(&doc), st))
}

pub fn write_file(set: &MeshSet, o: &DxfOptions, path: &Path) -> Result<DxfStats, String> {
    let (text, st) = write_string(set, o)?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(st)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meshx::tests::two_cubes;

    fn layer_lines(doc: &DxfDocument, layer: &str) -> usize {
        doc.entities
            .iter()
            .filter(|e| matches!(e, Entity::Line(l) if l.common.layer == layer))
            .count()
    }

    #[test]
    fn opens_in_our_reader_with_layers_cuts_and_faces() {
        let mut s = two_cubes();
        s.parts[1].category = "wall".into();
        let o = DxfOptions {
            plans: vec![PlanCut { name: "L1".into(), z: 1.5 }],
            sections: vec![SectionCut { name: "S".into(), origin: [0.0, 0.5], dir: [1.0, 0.0] }],
            faces: true,
            units: Units::Millimetre,
        };
        let (text, st) = write_string(&s, &o).unwrap();
        let back = crate::dxf::read_str(&text).unwrap();
        assert_eq!(back.insunits(), 4);
        // plan at 1.5 cuts the upper cube (4 sides x 2 triangles); section y=0.5 cuts both
        assert_eq!(layer_lines(&back, "box"), 8);
        assert_eq!(layer_lines(&back, "wall"), 16);
        let faces = back
            .entities
            .iter()
            .filter(|e| matches!(e, Entity::Unknown(p) if p[0].value == "3DFACE"))
            .count();
        assert_eq!(faces, 24);
        assert_eq!(st.faces, 24);
        assert_eq!(st.views, 2);
        let names: Vec<&str> = back.layers.iter().map(|l| l.name.as_str()).collect();
        for want in ["box", "wall", "3D-box", "3D-wall", "VIEW-TITLES"] {
            assert!(names.contains(&want), "{want} missing in {names:?}");
        }
        let lines = back.entities.iter().filter(|e| matches!(e, Entity::Line(_))).count();
        assert_eq!(lines, st.lines);
        let len = |layer: &str| -> f64 {
            back.entities
                .iter()
                .filter_map(|e| match e {
                    Entity::Line(l) if l.common.layer == layer => {
                        Some(((l.end[0] - l.start[0]).powi(2) + (l.end[1] - l.start[1]).powi(2)).sqrt())
                    }
                    _ => None,
                })
                .sum()
        };
        // perimeters in mm: box only in the section, wall in plan and section
        assert!((len("box") - 4000.0).abs() < 1e-6, "{}", len("box"));
        assert!((len("wall") - 8000.0).abs() < 1e-6, "{}", len("wall"));
    }

    #[test]
    fn plan_outline_of_a_cube_is_its_perimeter() {
        let s = two_cubes();
        let o = DxfOptions {
            plans: vec![PlanCut { name: "low".into(), z: 0.5 }],
            sections: vec![],
            faces: false,
            units: Units::Metre,
        };
        let (doc, st) = build(&s, &o).unwrap();
        assert_eq!(st.views, 1);
        let len: f64 = doc
            .entities
            .iter()
            .filter_map(|e| match e {
                Entity::Line(l) => Some(((l.end[0] - l.start[0]).powi(2) + (l.end[1] - l.start[1]).powi(2)).sqrt()),
                _ => None,
            })
            .sum();
        assert!((len - 4.0).abs() < 1e-9, "{len}");
    }

    #[test]
    fn corner_on_the_plane_and_empty_set() {
        assert_eq!(cut_tri([0.0, 1.0, 1.0], [[0.0; 3]; 3]), None);
        let p = [[0.0, 0.0, 0.0], [1.0, 0.0, 1.0], [1.0, 0.0, -1.0]];
        let (a, b) = cut_tri([0.0, 1.0, -1.0], p).unwrap();
        assert_eq!((a, b), ([1.0, 0.0, 0.0], [0.0, 0.0, 0.0]));
        let e = build(&MeshSet::default(), &DxfOptions::standard(&MeshSet::default(), &[], Units::Metre)).unwrap_err();
        assert!(e.contains("nothing to export"), "{e}");
        assert_eq!(layer_name("a/b:c"), "a_b_c");
    }
}
