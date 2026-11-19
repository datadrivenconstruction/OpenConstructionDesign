//! Terrain from IFC: bodies of IfcSite and IfcGeographicElement (triangulated/polygonal face
//! sets, faceted breps, shell/face based surface models, mapped items) placed by the local
//! placement chain and scaled to metres. A solid body keeps its upward faces (the top surface).

use std::collections::HashMap;

use super::{dedupe, TopoSource};
use crate::ifc::{StepFile, Value};

type P3 = [f64; 3];
/// Affine map: rows of [R | t].
type M = [[f64; 4]; 3];

const ID: M = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
];
const MAX_DEPTH: usize = 64;

fn mul(a: &M, b: &M) -> M {
    let mut o = [[0.0; 4]; 3];
    for i in 0..3 {
        for j in 0..4 {
            let mut v: f64 = (0..3).map(|k| a[i][k] * b[k][j]).sum();
            if j == 3 {
                v += a[i][3];
            }
            o[i][j] = v;
        }
    }
    o
}

fn apply(m: &M, p: P3) -> P3 {
    let r = |i: usize| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2] + m[i][3];
    [r(0), r(1), r(2)]
}

fn sub(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: P3, b: P3) -> P3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn unit(a: P3) -> Option<P3> {
    let l = dot(a, a).sqrt();
    (l > 1e-12 && l.is_finite()).then(|| [a[0] / l, a[1] / l, a[2] / l])
}

/// Frame from z axis, x hint, origin and scale.
fn frame(z: P3, x: P3, o: P3, s: f64) -> M {
    let z = unit(z).unwrap_or([0.0, 0.0, 1.0]);
    let xp = sub(x, [z[0] * dot(x, z), z[1] * dot(x, z), z[2] * dot(x, z)]);
    let x = unit(xp)
        .or_else(|| unit(cross([0.0, 1.0, 0.0], z)))
        .unwrap_or([1.0, 0.0, 0.0]);
    let y = cross(z, x);
    let mut m = [[0.0; 4]; 3];
    for i in 0..3 {
        m[i] = [x[i] * s, y[i] * s, z[i] * s, o[i]];
    }
    m
}

struct Rd<'a> {
    f: &'a StepFile,
}

impl<'a> Rd<'a> {
    fn args(&self, id: u32) -> &'a [Value] {
        self.f.get(id).map(|e| e.args()).unwrap_or(&[])
    }
    fn ty(&self, id: u32) -> &'a str {
        self.f.type_of(id).unwrap_or("")
    }
    fn arg(&self, id: u32, i: usize) -> Option<&'a Value> {
        self.args(id).get(i)
    }
    fn refs(&self, v: Option<&'a Value>) -> Vec<u32> {
        v.and_then(Value::as_list)
            .map(|l| l.iter().filter_map(Value::as_ref).collect())
            .unwrap_or_default()
    }

    fn nums(v: &Value) -> Option<P3> {
        let l = v.as_list()?;
        let g = |i: usize| l.get(i).and_then(Value::as_f64);
        Some([g(0)?, g(1)?, g(2).unwrap_or(0.0)])
    }

    /// IfcCartesianPoint or IfcDirection coordinates.
    fn coords(&self, id: u32) -> Option<P3> {
        Self::nums(self.arg(id, 0)?)
    }

    fn opt_dir(&self, v: Option<&Value>, d: P3) -> P3 {
        v.and_then(Value::as_ref)
            .and_then(|r| self.coords(r))
            .unwrap_or(d)
    }

    fn axis(&self, id: u32) -> M {
        match self.ty(id) {
            "IFCAXIS2PLACEMENT3D" => {
                let o = self.opt_dir(self.arg(id, 0), [0.0; 3]);
                let z = self.opt_dir(self.arg(id, 1), [0.0, 0.0, 1.0]);
                let x = self.opt_dir(self.arg(id, 2), [1.0, 0.0, 0.0]);
                frame(z, x, o, 1.0)
            }
            "IFCAXIS2PLACEMENT2D" => {
                let o = self.opt_dir(self.arg(id, 0), [0.0; 3]);
                let x = self.opt_dir(self.arg(id, 1), [1.0, 0.0, 0.0]);
                frame([0.0, 0.0, 1.0], [x[0], x[1], 0.0], [o[0], o[1], 0.0], 1.0)
            }
            _ => ID,
        }
    }

    fn placement(&self, id: u32, depth: usize) -> M {
        if depth > MAX_DEPTH || self.ty(id) != "IFCLOCALPLACEMENT" {
            return ID;
        }
        let parent = self
            .arg(id, 0)
            .and_then(Value::as_ref)
            .map_or(ID, |p| self.placement(p, depth + 1));
        let local = self
            .arg(id, 1)
            .and_then(Value::as_ref)
            .map_or(ID, |a| self.axis(a));
        mul(&parent, &local)
    }

    /// IfcCartesianTransformationOperator3D(Axis1, Axis2, LocalOrigin, Scale, Axis3).
    fn operator(&self, id: u32) -> M {
        if !self
            .ty(id)
            .starts_with("IFCCARTESIANTRANSFORMATIONOPERATOR3D")
        {
            return ID;
        }
        let x = self.opt_dir(self.arg(id, 0), [1.0, 0.0, 0.0]);
        let o = self.opt_dir(self.arg(id, 2), [0.0; 3]);
        let s = self.arg(id, 3).and_then(Value::as_f64).unwrap_or(1.0);
        let z = self.opt_dir(self.arg(id, 4), [0.0, 0.0, 1.0]);
        frame(z, x, o, s)
    }

    fn point_list(&self, id: u32) -> Vec<P3> {
        self.arg(id, 0)
            .and_then(Value::as_list)
            .map(|l| l.iter().filter_map(Self::nums).collect())
            .unwrap_or_default()
    }

    /// Index lists (1-based) of a face set, mapped through PnIndex when given.
    fn indexed(&self, pts: &[P3], v: Option<&Value>, pn: &[usize]) -> Vec<P3> {
        let Some(l) = v.and_then(Value::as_list) else {
            return vec![];
        };
        l.iter()
            .filter_map(|i| i.as_f64())
            .filter_map(|i| {
                let i = i as usize;
                let i = if pn.is_empty() {
                    i
                } else {
                    *pn.get(i.checked_sub(1)?)?
                };
                pts.get(i.checked_sub(1)?).copied()
            })
            .collect()
    }

    fn pn(&self, v: Option<&Value>) -> Vec<usize> {
        v.and_then(Value::as_list)
            .map(|l| {
                l.iter()
                    .filter_map(|x| x.as_f64())
                    .map(|x| x as usize)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn fan(m: &M, poly: &[P3], out: &mut Vec<[P3; 3]>) {
        for k in 1..poly.len().saturating_sub(1) {
            out.push([apply(m, poly[0]), apply(m, poly[k]), apply(m, poly[k + 1])]);
        }
    }

    fn face(&self, id: u32, m: &M, out: &mut Vec<[P3; 3]>) {
        let bounds = self.refs(self.arg(id, 0));
        let Some(&b) = bounds
            .iter()
            .find(|&&b| self.ty(b) == "IFCFACEOUTERBOUND")
            .or(bounds.first())
        else {
            return;
        };
        let Some(lp) = self.arg(b, 0).and_then(Value::as_ref) else {
            return;
        };
        if self.ty(lp) != "IFCPOLYLOOP" {
            return;
        }
        let mut poly: Vec<P3> = self
            .refs(self.arg(lp, 0))
            .into_iter()
            .filter_map(|p| self.coords(p))
            .collect();
        if self.arg(b, 1).and_then(Value::as_enum) == Some("F") {
            poly.reverse();
        }
        Self::fan(m, &poly, out);
    }

    fn item(&self, id: u32, m: &M, out: &mut Vec<[P3; 3]>, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        match self.ty(id) {
            "IFCTRIANGULATEDFACESET" | "IFCTRIANGULATEDIRREGULARNETWORK" => {
                let Some(c) = self.arg(id, 0).and_then(Value::as_ref) else {
                    return;
                };
                let pts = self.point_list(c);
                let pn = self.pn(self.arg(id, 4));
                if let Some(l) = self.arg(id, 3).and_then(Value::as_list) {
                    for t in l {
                        let tri = self.indexed(&pts, Some(t), &pn);
                        Self::fan(m, &tri, out);
                    }
                }
            }
            "IFCPOLYGONALFACESET" => {
                let Some(c) = self.arg(id, 0).and_then(Value::as_ref) else {
                    return;
                };
                let pts = self.point_list(c);
                let pn = self.pn(self.arg(id, 3));
                for fc in self.refs(self.arg(id, 2)) {
                    let poly = self.indexed(&pts, self.arg(fc, 0), &pn);
                    Self::fan(m, &poly, out);
                }
            }
            "IFCFACETEDBREP" | "IFCFACETEDBREPWITHVOIDS" => {
                if let Some(s) = self.arg(id, 0).and_then(Value::as_ref) {
                    self.item(s, m, out, depth + 1);
                }
            }
            "IFCSHELLBASEDSURFACEMODEL" | "IFCFACEBASEDSURFACEMODEL" => {
                for s in self.refs(self.arg(id, 0)) {
                    self.item(s, m, out, depth + 1);
                }
            }
            "IFCCLOSEDSHELL" | "IFCOPENSHELL" | "IFCCONNECTEDFACESET" => {
                for fc in self.refs(self.arg(id, 0)) {
                    if matches!(self.ty(fc), "IFCFACE" | "IFCFACESURFACE") {
                        self.face(fc, m, out);
                    }
                }
            }
            "IFCMAPPEDITEM" => {
                let Some(map) = self.arg(id, 0).and_then(Value::as_ref) else {
                    return;
                };
                let origin = self
                    .arg(map, 0)
                    .and_then(Value::as_ref)
                    .map_or(ID, |a| self.axis(a));
                let target = self
                    .arg(id, 1)
                    .and_then(Value::as_ref)
                    .map_or(ID, |t| self.operator(t));
                let mm = mul(m, &mul(&target, &origin));
                if let Some(rep) = self.arg(map, 1).and_then(Value::as_ref) {
                    for it in self.refs(self.arg(rep, 3)) {
                        self.item(it, &mm, out, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }

    fn unit_scale(&self, id: u32, depth: usize) -> Option<f64> {
        if depth > 8 {
            return None;
        }
        match self.ty(id) {
            "IFCSIUNIT" => {
                if self.arg(id, 1).and_then(Value::as_enum) != Some("LENGTHUNIT") {
                    return None;
                }
                Some(match self.arg(id, 2).and_then(Value::as_enum) {
                    Some("MILLI") => 1e-3,
                    Some("CENTI") => 1e-2,
                    Some("DECI") => 1e-1,
                    Some("MICRO") => 1e-6,
                    Some("KILO") => 1e3,
                    _ => 1.0,
                })
            }
            "IFCCONVERSIONBASEDUNIT" | "IFCCONVERSIONBASEDUNITWITHOFFSET" => {
                if self.arg(id, 1).and_then(Value::as_enum) != Some("LENGTHUNIT") {
                    return None;
                }
                let mw = self.arg(id, 3).and_then(Value::as_ref)?;
                let v = self.arg(mw, 0).and_then(Value::as_f64)?;
                let base = self
                    .arg(mw, 1)
                    .and_then(Value::as_ref)
                    .and_then(|u| self.unit_scale(u, depth + 1))
                    .unwrap_or(1.0);
                Some(v * base)
            }
            _ => None,
        }
    }

    /// Metres per file length unit: the project's unit assignment, else any length unit.
    fn length_scale(&self) -> f64 {
        for (id, t) in self.f.iter() {
            if t == "IFCUNITASSIGNMENT" {
                for u in self.refs(self.arg(id, 0)) {
                    if let Some(s) = self.unit_scale(u, 0) {
                        return s;
                    }
                }
            }
        }
        self.f
            .iter()
            .filter(|(_, t)| *t == "IFCSIUNIT")
            .find_map(|(id, _)| self.unit_scale(id, 0))
            .unwrap_or(1.0)
    }

    /// Geographic element that is terrain: PredefinedType TERRAIN, or an unspecific type whose
    /// name says so (trees, signs and other geographic objects are not terrain).
    fn is_terrain(&self, id: u32) -> bool {
        if self.ty(id) != "IFCGEOGRAPHICELEMENT" {
            return true;
        }
        match self.arg(id, 8).and_then(Value::as_enum) {
            Some(e) if e.eq_ignore_ascii_case("TERRAIN") => true,
            Some(e)
                if !matches!(
                    e.to_ascii_uppercase().as_str(),
                    "USERDEFINED" | "NOTDEFINED"
                ) =>
            {
                false
            }
            _ => [2, 3, 4].iter().any(|&k| {
                self.arg(id, k).and_then(Value::as_str).is_some_and(|s| {
                    let s = s.to_lowercase();
                    [
                        "terrain",
                        "topo",
                        "ground",
                        "surface",
                        "\u{440}\u{435}\u{43b}\u{44c}\u{435}\u{444}",
                        "\u{433}\u{440}\u{443}\u{43d}\u{442}",
                        "\u{43f}\u{43e}\u{432}\u{435}\u{440}\u{445}\u{43d}",
                    ]
                    .iter()
                    .any(|w| s.contains(w))
                })
            }),
        }
    }
}

fn plan2(t: &[P3; 3]) -> f64 {
    (t[1][0] - t[0][0]) * (t[2][1] - t[0][1]) - (t[1][1] - t[0][1]) * (t[2][0] - t[0][0])
}

/// Terrain points and outer boundary from the site/terrain bodies of an IFC file.
pub fn from_ifc(bytes: &[u8]) -> Result<TopoSource, String> {
    let f = StepFile::parse(bytes).map_err(|e| e.to_string())?;
    let rd = Rd { f: &f };
    let scale = rd.length_scale();
    let products: Vec<u32> = f
        .iter()
        .filter(|(_, t)| matches!(*t, "IFCSITE" | "IFCGEOGRAPHICELEMENT"))
        .map(|(i, _)| i)
        .filter(|&i| rd.is_terrain(i))
        .collect();
    let mut tris: Vec<[P3; 3]> = vec![];
    for p in products {
        let Some(shape) = rd.arg(p, 6).and_then(Value::as_ref) else {
            continue;
        };
        let m = rd
            .arg(p, 5)
            .and_then(Value::as_ref)
            .map_or(ID, |pl| rd.placement(pl, 0));
        let mut body = vec![];
        for rep in rd.refs(rd.arg(shape, 2)) {
            let ident = rd.arg(rep, 1).and_then(Value::as_str).unwrap_or("");
            if matches!(
                ident,
                "Axis" | "Box" | "FootPrint" | "Annotation" | "Profile"
            ) {
                continue;
            }
            for it in rd.refs(rd.arg(rep, 3)) {
                rd.item(it, &m, &mut body, 0);
            }
        }
        let body: Vec<[P3; 3]> = body
            .into_iter()
            .map(|t| t.map(|q| [q[0] * scale, q[1] * scale, q[2] * scale]))
            .collect();
        let tiny = |t: &[P3; 3]| {
            let e = |a: P3, b: P3| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
            let l = e(t[0], t[1]).max(e(t[1], t[2])).max(e(t[2], t[0]));
            plan2(t).abs() <= 1e-9 * l * l
        };
        let up: Vec<[P3; 3]> = body
            .iter()
            .filter(|t| !tiny(*t) && plan2(*t) > 0.0)
            .copied()
            .collect();
        if up.is_empty() {
            // a surface written facing down: keep it, flipped to ccw
            tris.extend(
                body.iter()
                    .filter(|t| !tiny(*t))
                    .map(|t| [t[0], t[2], t[1]]),
            );
        } else {
            tris.extend(up);
        }
    }
    if tris.is_empty() {
        return Err("no terrain body (IfcSite / IfcGeographicElement) in the file".into());
    }
    let flat: Vec<(f64, f64, f64)> = tris.iter().flatten().map(|p| (p[0], p[1], p[2])).collect();
    let (points, map) = dedupe(&flat, 1e-6);
    let idx: Vec<[usize; 3]> = map
        .chunks(3)
        .map(|c| [c[0], c[1], c[2]])
        .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
        .collect();
    let boundary = outer_loop(&points, &idx);
    Ok(TopoSource {
        points,
        breaklines: vec![],
        boundary,
        ..Default::default()
    })
}

/// Largest ccw loop of edges used by one triangle only.
fn outer_loop(pts: &[(f64, f64, f64)], tris: &[[usize; 3]]) -> Vec<(f64, f64)> {
    let mut count: HashMap<(usize, usize), u32> = HashMap::new();
    for t in tris {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *count.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let mut next: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut n_edges = 0;
    for t in tris {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if count[&(a.min(b), a.max(b))] == 1 {
                next.entry(a).or_default().push(b);
                n_edges += 1;
            }
        }
    }
    let mut best: (f64, Vec<usize>) = (0.0, vec![]);
    let mut starts: Vec<usize> = next.keys().copied().collect();
    starts.sort_unstable();
    for s in starts {
        while next.get(&s).is_some_and(|v| !v.is_empty()) {
            let mut lp = vec![s];
            let mut cur = s;
            let mut steps = 0;
            loop {
                let Some(n) = next.get_mut(&cur).and_then(|v| v.pop()) else {
                    break;
                };
                steps += 1;
                if n == s || steps > n_edges {
                    break;
                }
                lp.push(n);
                cur = n;
            }
            let a: f64 = (0..lp.len())
                .map(|i| {
                    let (p, q) = (pts[lp[i]], pts[lp[(i + 1) % lp.len()]]);
                    p.0 * q.1 - q.0 * p.1
                })
                .sum::<f64>()
                / 2.0;
            if lp.len() >= 3 && a > best.0 {
                best = (a, lp);
            }
        }
    }
    best.1.iter().map(|&i| (pts[i].0, pts[i].1)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nFILE_NAME('t','',(''),(''),'','','');\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n";
    const TAIL: &str = "ENDSEC;\nEND-ISO-10303-21;\n";

    fn area(r: &[(f64, f64)]) -> f64 {
        (0..r.len())
            .map(|i| {
                let (p, q) = (r[i], r[(i + 1) % r.len()]);
                p.0 * q.1 - q.0 * p.1
            })
            .sum::<f64>()
            / 2.0
    }

    #[test]
    fn site_face_set_in_mm_with_offset() {
        let data = "#1=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);\n\
#2=IFCUNITASSIGNMENT((#1));\n\
#3=IFCCARTESIANPOINT((1000.,2000.,500.));\n\
#4=IFCAXIS2PLACEMENT3D(#3,$,$);\n\
#5=IFCLOCALPLACEMENT($,#4);\n\
#6=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(10000.,0.,1000.),(10000.,10000.,2000.),(0.,10000.,1000.)),$);\n\
#7=IFCTRIANGULATEDFACESET(#6,$,.F.,((1,2,3),(1,3,4)),$);\n\
#8=IFCSHAPEREPRESENTATION($,'Body','Tessellation',(#7));\n\
#9=IFCPRODUCTDEFINITIONSHAPE($,$,(#8));\n\
#10=IFCSITE('0000000000000000000001',$,'Site',$,$,#5,#9,$,.ELEMENT.,$,$,$,$,$);\n";
        let s = from_ifc(format!("{HEAD}{data}{TAIL}").as_bytes()).unwrap();
        let near = |a: (f64, f64, f64), b: (f64, f64, f64)| {
            (a.0 - b.0).abs() + (a.1 - b.1).abs() + (a.2 - b.2).abs() < 1e-9
        };
        assert_eq!(s.points.len(), 4);
        assert!(near(s.points[0], (1.0, 2.0, 0.5)));
        assert!(s.points.iter().any(|&p| near(p, (11.0, 12.0, 2.5))));
        assert_eq!(s.boundary.len(), 4);
        assert!((area(&s.boundary) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn closed_brep_keeps_top_and_rotation() {
        // placement rotated 90° about z (x axis along +Y), metres
        let data = "#1=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);\n\
#2=IFCUNITASSIGNMENT((#1));\n\
#3=IFCCARTESIANPOINT((0.,0.,0.));\n\
#4=IFCDIRECTION((0.,1.,0.));\n\
#5=IFCAXIS2PLACEMENT3D(#3,$,#4);\n\
#6=IFCLOCALPLACEMENT($,#5);\n\
#11=IFCCARTESIANPOINT((0.,0.,1.));\n\
#12=IFCCARTESIANPOINT((2.,0.,1.));\n\
#13=IFCCARTESIANPOINT((2.,1.,1.));\n\
#14=IFCCARTESIANPOINT((0.,1.,1.));\n\
#15=IFCCARTESIANPOINT((0.,0.,0.));\n\
#16=IFCCARTESIANPOINT((2.,0.,0.));\n\
#17=IFCCARTESIANPOINT((2.,1.,0.));\n\
#18=IFCCARTESIANPOINT((0.,1.,0.));\n\
#20=IFCPOLYLOOP((#11,#12,#13,#14));\n\
#21=IFCFACEOUTERBOUND(#20,.T.);\n\
#22=IFCFACE((#21));\n\
#23=IFCPOLYLOOP((#15,#16,#17,#18));\n\
#24=IFCFACEOUTERBOUND(#23,.F.);\n\
#25=IFCFACE((#24));\n\
#26=IFCCLOSEDSHELL((#22,#25));\n\
#27=IFCFACETEDBREP(#26);\n\
#28=IFCSHAPEREPRESENTATION($,'Body','Brep',(#27));\n\
#29=IFCPRODUCTDEFINITIONSHAPE($,$,(#28));\n\
#30=IFCGEOGRAPHICELEMENT('0000000000000000000002',$,'Ground',$,$,#6,#29,$,.TERRAIN.);\n";
        let s = from_ifc(format!("{HEAD}{data}{TAIL}").as_bytes()).unwrap();
        assert_eq!(s.points.len(), 4);
        assert!(s.points.iter().all(|p| (p.2 - 1.0).abs() < 1e-12));
        // local (2, 1) maps to world (-1, 2)
        assert!(s
            .points
            .iter()
            .any(|p| (p.0 + 1.0).abs() < 1e-12 && (p.1 - 2.0).abs() < 1e-12));
        assert!((area(&s.boundary) - 2.0).abs() < 1e-9);
        assert!(from_ifc(format!("{HEAD}{TAIL}").as_bytes()).is_err());
        // a tree is a geographic element too, but not terrain
        let tree = data.replace(
            "'Ground',$,$,#6,#29,$,.TERRAIN.",
            "'Oak',$,'Tree',#6,#29,$,.USERDEFINED.",
        );
        assert!(from_ifc(format!("{HEAD}{tree}{TAIL}").as_bytes()).is_err());
    }
}
