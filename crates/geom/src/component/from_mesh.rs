//! Family from imported meshes (the import wizard's result): one `mesh` body per material, an
//! optional simplified medium level, a box at coarse, a plan outline, Width/Depth/Height that
//! stretch the meshes, and for doors and windows a wall opening with a sill.

use std::fmt::Write;

use super::meshbody::MeshBody;
use super::{Category, Family, Hosting};
use crate::math::{v2, v3, V2, V3};
use crate::mesh::Mesh;

/// Mesh of one material in model axes (metres, z up), before the insertion point is applied.
#[derive(Clone, Debug)]
pub struct MeshPart {
    pub material: String,
    pub body: MeshBody,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Insertion {
    /// Centre of the box in plan, bottom of the box.
    BaseCentre,
    /// The source's own origin.
    Origin,
    /// A picked point in source coordinates.
    Point(V3),
}

#[derive(Clone, Debug)]
pub struct ImportSpec {
    pub name: String,
    pub type_name: String,
    pub category: Category,
    pub hosting: Hosting,
    pub insertion: Insertion,
    /// IFC class and predefined type; by category when `None`.
    pub ifc: Option<(String, String)>,
    /// Text type parameters (property sets, classification, source and licence).
    pub props: Vec<(String, String)>,
    /// Door or window: default sill above the wall bottom.
    pub sill: Option<f64>,
    /// Build a simplified medium level when the mesh is large.
    pub lods: bool,
}

impl ImportSpec {
    pub fn new(name: &str, category: Category) -> ImportSpec {
        let opening = matches!(category, Category::Door | Category::Window);
        ImportSpec {
            name: name.into(),
            type_name: "Default".into(),
            category,
            hosting: if opening {
                Hosting::Wall
            } else {
                Hosting::Floor
            },
            insertion: Insertion::BaseCentre,
            ifc: None,
            props: vec![],
            sill: match category {
                Category::Window => Some(0.9),
                Category::Door => Some(0.0),
                _ => None,
            },
            lods: true,
        }
    }
    fn opening(&self) -> bool {
        self.hosting == Hosting::Wall && matches!(self.category, Category::Door | Category::Window)
    }
}

/// Parameter name: letters, digits and `_`, starting with a letter.
pub fn ident(s: &str) -> String {
    let mut o: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    while o.contains("__") {
        o = o.replace("__", "_");
    }
    let o = o.trim_matches('_').to_string();
    match o.chars().next() {
        Some(c) if c.is_alphabetic() => o,
        _ => format!("P_{o}"),
    }
}

fn quoted(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| match c {
            '"' => '\'',
            '\n' | '\r' | '\t' | '#' => ' ',
            c => c,
        })
        .collect();
    format!("\"{}\"", t.trim())
}

fn bbox(parts: &[MeshPart]) -> Option<[V3; 2]> {
    let mut b: Option<[V3; 2]> = None;
    for p in parts {
        let q = p.body.bbox();
        b = Some(match b {
            None => q,
            Some(o) => [
                v3(o[0].x.min(q[0].x), o[0].y.min(q[0].y), o[0].z.min(q[0].z)),
                v3(o[1].x.max(q[1].x), o[1].y.max(q[1].y), o[1].z.max(q[1].z)),
            ],
        });
    }
    b
}

fn hull(pts: &[V2]) -> Vec<V2> {
    let mut p: Vec<V2> = pts.to_vec();
    p.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    p.dedup_by(|a, b| (*a - *b).len() < 1e-6);
    if p.len() < 3 {
        return p;
    }
    let cross = |o: V2, a: V2, b: V2| (a - o).cross(b - o);
    let half = |it: &mut dyn Iterator<Item = V2>| {
        let mut h: Vec<V2> = vec![];
        for q in it {
            while h.len() >= 2 && cross(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
                h.pop();
            }
            h.push(q);
        }
        h.pop();
        h
    };
    let mut lo = half(&mut p.iter().copied());
    lo.extend(half(&mut p.iter().rev().copied()));
    lo
}

fn len(v: f64) -> String {
    super::doc::mm(v)
}

/// Family text; the result parses and builds (checked before it is returned).
pub fn family_text(parts: &[MeshPart], spec: &ImportSpec) -> Result<String, String> {
    let [lo, hi] = bbox(parts).ok_or("nothing to import: no triangles")?;
    let size = hi - lo;
    if size.x.max(size.y).max(size.z) > 1000.0 {
        return Err(format!(
            "the model is {:.0} m across: check the units",
            size.x.max(size.y).max(size.z)
        ));
    }
    let origin = match spec.insertion {
        Insertion::BaseCentre => v3((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0, lo.z),
        Insertion::Origin => v3(0.0, 0.0, 0.0),
        Insertion::Point(p) => p,
    };
    let opening = spec.opening();
    let mut s = String::new();
    let _ = writeln!(s, "family {}", spec.name.replace(['\n', '#'], " ").trim());
    let _ = writeln!(s, "category {}", spec.category.name());
    let _ = writeln!(s, "host {}", spec.hosting.name());
    if let Some((c, p)) = &spec.ifc {
        let _ = writeln!(s, "ifc {} {}", ident(c), ident(p));
    }
    // sizes that stretch the meshes; a flat axis keeps scale 1
    let axes = [("Width", size.x), ("Depth", size.y), ("Height", size.z)];
    let mut scale = vec![];
    for (n, v) in axes {
        if v > 1e-4 {
            let _ = writeln!(s, "param {n} length type {}", len(v));
            scale.push(format!("{n} / {}", len(v)));
        } else {
            scale.push("1".into());
        }
    }
    let scale = format!("scale({})", scale.join(", "));
    if opening {
        if size.x <= 1e-4 || size.z <= 1e-4 {
            return Err("a door or window needs width and height".into());
        }
        let _ = writeln!(s, "param Sill length type {}", len(spec.sill.unwrap_or(0.0)));
    }
    let mut mats: Vec<String> = vec![];
    for p in parts {
        if !mats.contains(&p.material) {
            mats.push(p.material.clone());
        }
    }
    let mat_param = |k: usize| format!("Mat{}", k + 1);
    for (k, m) in mats.iter().enumerate() {
        let _ = writeln!(s, "param {} material type {}", mat_param(k), quoted(m));
    }
    let mut seen = vec![];
    for (k, v) in &spec.props {
        let mut n = ident(k);
        if ["Width", "Depth", "Height", "Sill"].contains(&n.as_str()) || n.starts_with("Mat") {
            n = format!("P_{n}");
        }
        if seen.contains(&n) {
            continue;
        }
        let _ = writeln!(s, "param {n} text type {}", quoted(v));
        seen.push(n);
    }
    let _ = writeln!(s, "type {}", quoted(&spec.type_name));
    let mut all2: Vec<V2> = vec![];
    for (k, p) in parts.iter().enumerate() {
        let mi = mats.iter().position(|m| *m == p.material).unwrap_or(0);
        let moved = MeshBody::new(
            p.body.pos.iter().map(|q| *q - origin).collect(),
            p.body.tris.clone(),
        )?
        .welded();
        all2.extend(moved.pos.iter().map(|q| v2(q.x, q.y)));
        let medium = if spec.lods && moved.tris.len() >= 400 {
            let m = Mesh {
                positions: moved.pos.clone(),
                triangles: moved.tris.clone(),
                face_of: vec![0; moved.tris.len()],
            };
            crate::simplify::lods(&m)
                .levels
                .get(1)
                .and_then(|l| MeshBody::new(l.positions.clone(), l.triangles.clone()).ok())
        } else {
            None
        };
        let mat = format!("mat({})", mat_param(mi));
        match medium {
            Some(md) => {
                let _ = writeln!(s, "mesh Body{k} data({}) {scale} {mat} show(fine, 3d, elev)", moved.encode());
                let _ = writeln!(s, "mesh Body{k}M data({}) {scale} {mat} alt show(medium, 3d, elev)", md.encode());
            }
            None => {
                let _ = writeln!(s, "mesh Body{k} data({}) {scale} {mat} show(medium, fine, 3d, elev)", moved.encode());
            }
        }
    }
    // coarse: the box; plan: the outline of everything at every level
    let (b0, b1) = (lo - origin, hi - origin);
    let sx = |x: f64| if size.x > 1e-4 { format!("{} * Width / {}", len(x), len(size.x)) } else { len(x) };
    let sy = |y: f64| if size.y > 1e-4 { format!("{} * Depth / {}", len(y), len(size.y)) } else { len(y) };
    let sz = |z: f64| if size.z > 1e-4 { format!("{} * Height / {}", len(z), len(size.z)) } else { len(z) };
    if size.x > 1e-4 && size.y > 1e-4 && size.z > 1e-4 {
        let _ = writeln!(
            s,
            "extrude Box rect({}, {}, {}, {}) z({}, {}) mat({}) alt show(coarse, 3d, elev)",
            sx(b0.x),
            sy(b0.y),
            sx(b1.x),
            sy(b1.y),
            sz(b0.z),
            sz(b1.z),
            mat_param(0)
        );
    }
    let h = hull(&all2);
    if h.len() >= 3 {
        let pts: Vec<String> = h.iter().map(|q| format!("{}, {}", sx(q.x), sy(q.y))).collect();
        let _ = writeln!(s, "symbol loop({})", pts.join(", "));
    }
    if opening {
        let _ = writeln!(
            s,
            "extrude Opening rect({}, -1m, {}, 1m) z({}, {}) void",
            sx(b0.x),
            sx(b1.x),
            sz(b0.z),
            sz(b1.z)
        );
    }
    let f = Family::parse(&s).map_err(|e| format!("generated family does not parse: {e}"))?;
    let ty = f.types.first().map(|t| t.values.clone()).unwrap_or_default();
    let r = f.resolve(&ty, &[]).map_err(|e| e.to_string())?;
    f.build(&r, super::Lod::Fine).map_err(|e| e.to_string())?;
    Ok(s)
}

/// Turns Y-up source axes (glTF, most OBJ) into model Z-up axes: (x, y, z) → (x, -z, y).
pub fn y_up_to_z_up(p: V3) -> V3 {
    v3(p.x, -p.z, p.y)
}
