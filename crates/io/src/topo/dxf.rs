//! Terrain from a drawing: contours (LWPOLYLINE at elevation 38, 2D POLYLINE at its elevation),
//! 3D polylines and lines with z as breaklines; 3DFACE corners, POINT, polyface/polygon mesh
//! vertices and block insertion points (survey point symbols) as points. Blocks are not expanded.
//! Entities lying at z = 0 everywhere (frames, title blocks, 2D annotation) are left out unless
//! asked for; [`dxf_layers`] lists the layers for the import dialog.

use super::{SurveyUnit, TopoSource};
use crate::dxf::{DxfDocument, Entity, Pair};

type P3 = (f64, f64, f64);

#[derive(Clone, Debug, PartialEq)]
pub struct DxfTopoOptions {
    /// Layers to read (case-insensitive); empty — all.
    pub layers: Vec<String>,
    /// Contours and 3D polylines become breaklines (else their vertices are plain points).
    pub contours_as_breaklines: bool,
    /// Also entities at z = 0 everywhere (off: they are usually frames and annotation).
    pub keep_flat: bool,
    /// Block insertion points become survey points.
    pub inserts: bool,
    /// Unit of the drawing's coordinates; None - `$INSUNITS`, else metres.
    pub unit: Option<SurveyUnit>,
}

impl Default for DxfTopoOptions {
    fn default() -> Self {
        DxfTopoOptions {
            layers: vec![],
            contours_as_breaklines: true,
            keep_flat: false,
            inserts: true,
            unit: None,
        }
    }
}

/// Unit the drawing names in `$INSUNITS`; None when unitless or not offered.
pub fn dxf_unit(doc: &DxfDocument) -> Option<SurveyUnit> {
    SurveyUnit::from_insunits(doc.insunits())
}

/// Layer of the drawing with its terrain-capable entities.
#[derive(Clone, Debug, PartialEq)]
pub struct DxfLayer {
    pub name: String,
    /// Lines, polylines, faces, points and inserts on the layer.
    pub entities: usize,
    /// Of them, with some z ≠ 0.
    pub elevated: usize,
}

fn val(p: &Pair) -> Option<f64> {
    p.value.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn ival(p: &Pair) -> i32 {
    p.value.trim().parse::<i32>().unwrap_or(0)
}

/// Point from codes base, base+10, base+20.
fn point(rec: &[Pair], base: i32) -> Option<P3> {
    let g = |c: i32| rec.iter().find(|p| p.code == c).and_then(val);
    Some((g(base)?, g(base + 10)?, g(base + 20).unwrap_or(0.0)))
}

fn layer_of(rec: &[Pair]) -> &str {
    rec.iter()
        .find(|p| p.code == 8)
        .map_or("0", |p| p.value.trim())
}

fn kind(rec: &[Pair]) -> &str {
    rec.first().map_or("", |p| p.value.trim())
}

/// Terrain geometry of one entity.
enum Geo {
    /// Polyline vertices (contour, 3D polyline, line).
    Line(Vec<P3>),
    /// Mesh vertices, POINT.
    Points(Vec<P3>),
    /// 3DFACE distinct corners (fewer than 3 — degenerate, still taken as points).
    Face(Vec<P3>),
    /// Block insertion point.
    Insert(P3),
    /// Unreadable vertex or point.
    Bad,
}

impl Geo {
    fn flat(&self) -> bool {
        let f = |v: &[P3]| v.iter().all(|p| p.2 == 0.0);
        match self {
            Geo::Line(v) | Geo::Points(v) | Geo::Face(v) => f(v),
            Geo::Insert(p) => p.2 == 0.0,
            Geo::Bad => false,
        }
    }
}

/// Model-space entities as terrain geometry, with their layer.
fn walk(doc: &DxfDocument, mut f: impl FnMut(&str, Geo)) {
    let ents = &doc.entities;
    let mut i = 0;
    while i < ents.len() {
        let e = &ents[i];
        i += 1;
        match e {
            Entity::Line(l) => {
                let (a, b) = (l.start, l.end);
                f(
                    &l.common.layer,
                    Geo::Line(vec![(a[0], a[1], a[2]), (b[0], b[1], b[2])]),
                );
            }
            Entity::LwPolyline(p) => {
                let z = p
                    .common
                    .extra
                    .iter()
                    .find(|q| q.code == 38)
                    .and_then(val)
                    .unwrap_or(0.0);
                let mut pts: Vec<P3> = p.vertices.iter().map(|v| (v.x, v.y, z)).collect();
                if p.closed() && pts.len() > 2 {
                    pts.push(pts[0]);
                }
                f(&p.common.layer, Geo::Line(pts));
            }
            Entity::Insert(ins) => {
                let q = ins.pos;
                f(&ins.common.layer, Geo::Insert((q[0], q[1], q[2])));
            }
            Entity::Unknown(rec) => match kind(rec) {
                "POINT" => f(
                    layer_of(rec),
                    point(rec, 10).map_or(Geo::Bad, |p| Geo::Points(vec![p])),
                ),
                "3DFACE" => {
                    let mut c: Vec<P3> = vec![];
                    for base in [10, 11, 12, 13] {
                        if let Some(p) = point(rec, base) {
                            if !c.contains(&p) {
                                c.push(p);
                            }
                        }
                    }
                    f(layer_of(rec), Geo::Face(c));
                }
                "POLYLINE" => {
                    let layer = layer_of(rec);
                    let flags = rec.iter().find(|p| p.code == 70).map_or(0, ival);
                    let elev = point(rec, 10).map_or(0.0, |p| p.2);
                    let mesh = flags & (16 | 64) != 0;
                    let is3d = flags & 8 != 0;
                    let mut pts = vec![];
                    while i < ents.len() {
                        let Entity::Unknown(v) = &ents[i] else { break };
                        match kind(v) {
                            "VERTEX" => {
                                i += 1;
                                let vf = v.iter().find(|p| p.code == 70).map_or(0, ival);
                                // polyface face record: indices only, no position
                                if vf & 128 != 0 && vf & 64 == 0 {
                                    continue;
                                }
                                match point(v, 10) {
                                    Some(p) if mesh || is3d => pts.push(p),
                                    Some(p) => pts.push((p.0, p.1, elev)),
                                    None => f(layer, Geo::Bad),
                                }
                            }
                            "SEQEND" => {
                                i += 1;
                                break;
                            }
                            _ => break,
                        }
                    }
                    if mesh {
                        f(layer, Geo::Points(pts));
                    } else {
                        if flags & 1 != 0 && pts.len() > 2 {
                            pts.push(pts[0]);
                        }
                        f(layer, Geo::Line(pts));
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// Layers carrying terrain-capable entities, in order of first use.
pub fn dxf_layers(doc: &DxfDocument) -> Vec<DxfLayer> {
    let mut out: Vec<DxfLayer> = vec![];
    walk(doc, |layer, g| {
        if matches!(g, Geo::Bad) {
            return;
        }
        let up = !g.flat();
        let name = layer.trim();
        let k = match out.iter().position(|l| l.name.eq_ignore_ascii_case(name)) {
            Some(k) => k,
            None => {
                out.push(DxfLayer {
                    name: name.to_string(),
                    entities: 0,
                    elevated: 0,
                });
                out.len() - 1
            }
        };
        out[k].entities += 1;
        out[k].elevated += usize::from(up);
    });
    out
}

/// Reads terrain geometry from the model-space entities.
pub fn from_dxf(doc: &DxfDocument, o: &DxfTopoOptions) -> TopoSource {
    let wanted = |layer: &str| {
        o.layers.is_empty()
            || o.layers
                .iter()
                .any(|l| l.eq_ignore_ascii_case(layer.trim()))
    };
    let mut out = TopoSource::default();
    walk(doc, |layer, g| {
        if !wanted(layer) || (matches!(g, Geo::Insert(_)) && !o.inserts) {
            return;
        }
        if !o.keep_flat && g.flat() {
            out.filtered += 1;
            return;
        }
        match g {
            Geo::Bad => out.skipped += 1,
            Geo::Insert(p) => out.points.push(p),
            Geo::Points(v) => out.points.extend(v),
            Geo::Face(c) => {
                if c.len() < 3 {
                    out.skipped += 1;
                }
                out.points.extend(c);
            }
            Geo::Line(v) => {
                if v.len() >= 2 && o.contours_as_breaklines {
                    out.breaklines.push(v);
                } else {
                    out.points.extend(v);
                }
            }
        }
    });
    out.scale(o.unit.or_else(|| dxf_unit(doc)).unwrap_or_default().meters());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(body: &str) -> DxfDocument {
        let text = format!("0\nSECTION\n2\nENTITIES\n{body}0\nENDSEC\n0\nEOF\n");
        crate::dxf::read_str(&text).unwrap()
    }

    #[test]
    fn contour_at_elevation() {
        let d = doc("0\nLWPOLYLINE\n8\nCONTOURS\n90\n3\n70\n0\n38\n125.5\n10\n0\n20\n0\n10\n10\n20\n0\n10\n10\n20\n5\n");
        let s = from_dxf(&d, &DxfTopoOptions::default());
        assert_eq!(
            s.breaklines,
            vec![vec![
                (0.0, 0.0, 125.5),
                (10.0, 0.0, 125.5),
                (10.0, 5.0, 125.5)
            ]]
        );
        let only = from_dxf(
            &d,
            &DxfTopoOptions {
                layers: vec!["other".into()],
                ..Default::default()
            },
        );
        assert_eq!(only.vertex_count(), 0);
        let pts = from_dxf(
            &d,
            &DxfTopoOptions {
                layers: vec!["contours".into()],
                contours_as_breaklines: false,
                ..Default::default()
            },
        );
        assert_eq!(pts.points.len(), 3);
    }

    #[test]
    fn polyline_3d_and_polyface() {
        let d = doc(concat!(
            "0\nPOLYLINE\n8\n0\n66\n1\n70\n8\n10\n0\n20\n0\n30\n0\n",
            "0\nVERTEX\n8\n0\n10\n1\n20\n2\n30\n3\n70\n32\n",
            "0\nVERTEX\n8\n0\n10\n4\n20\n5\n30\n6\n70\n32\n",
            "0\nSEQEND\n8\n0\n",
            "0\nPOLYLINE\n8\n0\n66\n1\n70\n64\n10\n0\n20\n0\n30\n0\n",
            "0\nVERTEX\n8\n0\n10\n0\n20\n0\n30\n1\n70\n192\n",
            "0\nVERTEX\n8\n0\n10\n1\n20\n0\n30\n2\n70\n192\n",
            "0\nVERTEX\n8\n0\n10\n0\n20\n1\n30\n3\n70\n192\n",
            "0\nVERTEX\n8\n0\n10\n0\n20\n0\n30\n0\n70\n128\n71\n1\n72\n2\n73\n3\n",
            "0\nSEQEND\n8\n0\n",
        ));
        let s = from_dxf(&d, &DxfTopoOptions::default());
        assert_eq!(s.breaklines, vec![vec![(1.0, 2.0, 3.0), (4.0, 5.0, 6.0)]]);
        assert_eq!(
            s.points,
            vec![(0.0, 0.0, 1.0), (1.0, 0.0, 2.0), (0.0, 1.0, 3.0)]
        );
    }

    #[test]
    fn faces_points_and_lines() {
        let d = doc(concat!(
            "0\n3DFACE\n8\n0\n10\n0\n20\n0\n30\n1\n11\n1\n21\n0\n31\n2\n12\n1\n22\n1\n32\n3\n13\n1\n23\n1\n33\n3\n",
            "0\nPOINT\n8\n0\n10\n5\n20\n6\n30\n7\n",
            "0\nLINE\n8\n0\n10\n0\n20\n0\n30\n1\n11\n2\n21\n0\n31\n2\n",
        ));
        let s = from_dxf(&d, &DxfTopoOptions::default());
        assert_eq!(
            s.points,
            vec![
                (0.0, 0.0, 1.0),
                (1.0, 0.0, 2.0),
                (1.0, 1.0, 3.0),
                (5.0, 6.0, 7.0)
            ]
        );
        assert_eq!(s.breaklines, vec![vec![(0.0, 0.0, 1.0), (2.0, 0.0, 2.0)]]);
    }

    #[test]
    fn insunits_mm_imports_at_one_thousandth() {
        let mut d = doc("0\nPOINT\n8\n0\n10\n5000\n20\n6000\n30\n101500\n");
        assert_eq!(dxf_unit(&d), None);
        let raw = from_dxf(&d, &DxfTopoOptions::default());
        assert_eq!(raw.points, vec![(5000.0, 6000.0, 101500.0)], "unitless: metres");
        d.header.push(crate::dxf::HeaderVar {
            name: "$INSUNITS".into(),
            values: vec![Pair::new(70, "4")],
        });
        assert_eq!(dxf_unit(&d), Some(SurveyUnit::Mm));
        let p = from_dxf(&d, &DxfTopoOptions::default()).points[0];
        assert!((p.0 - 5.0).abs() < 1e-12 && (p.1 - 6.0).abs() < 1e-12);
        assert!((p.2 - 101.5).abs() < 1e-12, "z scales too");
        // the dialog's explicit choice overrides the header
        let ft = DxfTopoOptions {
            unit: Some(SurveyUnit::Ft),
            ..Default::default()
        };
        let q = from_dxf(&d, &ft).points[0];
        assert!((q.0 - 5000.0 * 0.3048).abs() < 1e-9);
    }
}
