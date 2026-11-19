use std::collections::{HashMap, HashSet};
use std::io::Read as _;

use ddc_geom::gsolid::{self, Frame};

use super::*;
use crate::exact::tests::slab;
use crate::meshx::{MeshSet, Part};

/// A mesh of the package: name, positions (model units), triangles, base material index.
struct Obj {
    name: String,
    pts: Vec<[f64; 3]>,
    tri: Vec<[usize; 3]>,
    pindex: Option<usize>,
}

struct Pkg {
    unit: String,
    colours: Vec<String>,
    objects: Vec<Obj>,
    items: usize,
}

fn elems<'a, 'i>(n: roxmltree::Node<'a, 'i>, name: &str) -> Vec<roxmltree::Node<'a, 'i>> {
    n.children().filter(|c| c.is_element() && c.tag_name().name() == name).collect()
}

/// Reads a package and checks it against the rules of the 3MF core specification.
fn validate(bytes: &[u8]) -> Result<Pkg, String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut read = |name: &str| -> Result<String, String> {
        let mut f = z.by_name(name).map_err(|e| format!("{name}: {e}"))?;
        let mut s = String::new();
        f.read_to_string(&mut s).map_err(|e| e.to_string())?;
        Ok(s)
    };
    let types = read("[Content_Types].xml")?;
    let rels = read("_rels/.rels")?;
    let model = read(MODEL_PATH)?;

    let t = roxmltree::Document::parse(&types).map_err(|e| format!("content types: {e}"))?;
    let defaults: HashMap<String, String> = elems(t.root_element(), "Default")
        .iter()
        .filter_map(|d| Some((d.attribute("Extension")?.to_ascii_lowercase(), d.attribute("ContentType")?.to_string())))
        .collect();
    if defaults.get("model").map(String::as_str) != Some("application/vnd.ms-package.3dmanufacturing-3dmodel+xml")
        || defaults.get("rels").map(String::as_str) != Some("application/vnd.openxmlformats-package.relationships+xml")
    {
        return Err(format!("content types: {defaults:?}"));
    }

    let r = roxmltree::Document::parse(&rels).map_err(|e| format!("rels: {e}"))?;
    let start = elems(r.root_element(), "Relationship")
        .into_iter()
        .find(|x| x.attribute("Type") == Some(REL_TYPE))
        .ok_or("no 3D model relationship")?;
    if start.attribute("Target") != Some(&format!("/{MODEL_PATH}")) || start.attribute("Id").is_none() {
        return Err("bad start part relationship".into());
    }

    let m = roxmltree::Document::parse(&model).map_err(|e| format!("model: {e}"))?;
    let root = m.root_element();
    if root.tag_name().name() != "model" || root.tag_name().namespace() != Some(CORE_NS) {
        return Err("root is not a core model".into());
    }
    let unit = root.attribute("unit").unwrap_or("millimeter").to_string();
    if !Unit::ALL.iter().any(|u| u.name() == unit) {
        return Err(format!("unit '{unit}'"));
    }
    let res = elems(root, "resources");
    let [res] = res.as_slice() else { return Err("one resources element".into()) };
    let mut ids = HashSet::new();
    let mut colours = vec![];
    let mut mat_id = None;
    let mut objects = vec![];
    let mut object_ids = HashMap::new();
    for n in res.children().filter(|c| c.is_element()) {
        let id: u32 = n.attribute("id").and_then(|s| s.parse().ok()).ok_or("resource without a numeric id")?;
        if id == 0 || !ids.insert(id) {
            return Err(format!("resource id {id} repeated or zero"));
        }
        match n.tag_name().name() {
            "basematerials" => {
                mat_id = Some(id);
                for b in elems(n, "base") {
                    let c = b.attribute("displaycolor").ok_or("base without colour")?;
                    let hex = c.strip_prefix('#').ok_or("colour without #")?;
                    if !(hex.len() == 6 || hex.len() == 8) || !hex.chars().all(|x| x.is_ascii_hexdigit()) || b.attribute("name").is_none() {
                        return Err(format!("bad base material {c}"));
                    }
                    colours.push(c.to_string());
                }
            }
            "object" => {
                let ty = n.attribute("type").unwrap_or("model");
                if !["model", "solidsupport", "support", "surface", "other"].contains(&ty) {
                    return Err(format!("object type {ty}"));
                }
                let pindex = match n.attribute("pid") {
                    Some(pid) => {
                        if pid.parse::<u32>().ok() != mat_id {
                            return Err(format!("object {id}: pid {pid} is no earlier material group"));
                        }
                        let k: usize = n.attribute("pindex").and_then(|s| s.parse().ok()).ok_or("pid without pindex")?;
                        if k >= colours.len() {
                            return Err(format!("object {id}: pindex {k} out of range"));
                        }
                        Some(k)
                    }
                    None => None,
                };
                let mesh = elems(n, "mesh");
                let [mesh] = mesh.as_slice() else { return Err(format!("object {id}: one mesh")) };
                let mut pts = vec![];
                for v in elems(elems(*mesh, "vertices").first().copied().ok_or("no vertices")?, "vertex") {
                    let c = ["x", "y", "z"].map(|a| v.attribute(a).and_then(|s| s.parse::<f64>().ok()));
                    let [Some(x), Some(y), Some(z)] = c else { return Err("vertex without coordinates".into()) };
                    if ![x, y, z].iter().all(|f| f.is_finite()) {
                        return Err("non-finite vertex".into());
                    }
                    pts.push([x, y, z]);
                }
                let mut tri = vec![];
                for t in elems(elems(*mesh, "triangles").first().copied().ok_or("no triangles")?, "triangle") {
                    let v = ["v1", "v2", "v3"].map(|a| t.attribute(a).and_then(|s| s.parse::<usize>().ok()));
                    let [Some(a), Some(b), Some(c)] = v else { return Err("triangle without indices".into()) };
                    if a == b || b == c || a == c || [a, b, c].iter().any(|&i| i >= pts.len()) {
                        return Err(format!("bad triangle {a} {b} {c}"));
                    }
                    tri.push([a, b, c]);
                }
                if pts.len() < 3 || tri.is_empty() {
                    return Err(format!("object {id}: empty mesh"));
                }
                object_ids.insert(id, ty.to_string());
                objects.push(Obj {
                    name: n.attribute("name").unwrap_or("").to_string(),
                    pts,
                    tri,
                    pindex,
                });
            }
            other => return Err(format!("unknown resource {other}")),
        }
    }
    let build = elems(root, "build");
    let [build] = build.as_slice() else { return Err("one build element".into()) };
    let mut items = 0;
    for it in elems(*build, "item") {
        let id: u32 = it.attribute("objectid").and_then(|s| s.parse().ok()).ok_or("item without object")?;
        match object_ids.get(&id) {
            Some(t) if t != "other" => items += 1,
            _ => return Err(format!("item refers to {id}")),
        }
    }
    Ok(Pkg {
        unit,
        colours,
        objects,
        items,
    })
}

fn volume(o: &Obj) -> f64 {
    o.tri
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(|i| o.pts[i]);
            a[0] * (b[1] * c[2] - b[2] * c[1]) + a[1] * (b[2] * c[0] - b[0] * c[2]) + a[2] * (b[0] * c[1] - b[1] * c[0])
        })
        .sum::<f64>()
        / 6.0
}

/// A closed curved slab (red) and an open square sheet (no material).
fn set() -> MeshSet {
    let m = gsolid::mesh(&slab(), &Frame::default(), 1e-3).unwrap();
    let mut ms = MeshSet::default();
    let red = ms.material("red", [1.0, 0.0, 0.0, 1.0]);
    // triangles keep their own corners: welding is the writer's job
    let mut positions = vec![];
    let mut triangles = vec![];
    for t in &m.triangles {
        let base = positions.len() as u32;
        for &i in t {
            let p = m.positions[i as usize];
            positions.push([p.x, p.y, p.z]);
        }
        triangles.push([base, base + 1, base + 2]);
    }
    assert!(ms.push(Part {
        key: "a".into(),
        name: "slab <A&B>".into(),
        material: Some(red),
        positions,
        triangles,
        ..Default::default()
    }));
    assert!(ms.push(Part {
        key: "b".into(),
        name: "sheet".into(),
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]],
        triangles: vec![[0, 1, 2], [0, 2, 3]],
        ..Default::default()
    }));
    ms
}

#[test]
fn package_follows_the_core_spec() {
    let ms = set();
    let (bytes, report) = write(&ms, &Options::default()).unwrap();
    let pkg = validate(&bytes).unwrap();
    assert_eq!(pkg.unit, "millimeter");
    assert_eq!(pkg.items, 2);
    assert_eq!(pkg.colours, vec!["#FF0000".to_string()]);
    let slab_obj = &pkg.objects[0];
    assert_eq!(slab_obj.name, "slab <A&B>");
    assert_eq!(slab_obj.pindex, Some(0));
    assert_eq!(pkg.objects[1].pindex, None);
    // welded: far fewer vertices than triangle corners, volume in mm3
    assert!(slab_obj.pts.len() * 3 < ms.parts[0].positions.len());
    let q = gsolid::quantities(&slab()).unwrap().volume;
    let v = volume(slab_obj) / 1e9;
    assert!((v - q).abs() < 0.01 * q, "{v} vs {q}");
    let max_x = slab_obj.pts.iter().map(|p| p[0]).fold(f64::MIN, f64::max);
    assert!(max_x > 3000.0 && max_x < 3500.0, "{max_x}");
    assert!(report.parts[0].watertight());
    let leaky: Vec<&PartCheck> = report.leaky().collect();
    assert_eq!(leaky.len(), 1);
    assert_eq!(leaky[0].open_edges, 4);
    assert!(leaky[0].written);
}

#[test]
fn skip_and_require_policies() {
    let ms = set();
    let (bytes, report) = write(
        &ms,
        &Options {
            policy: Policy::Skip,
            ..Default::default()
        },
    )
    .unwrap();
    let pkg = validate(&bytes).unwrap();
    assert_eq!(pkg.items, 1);
    assert!(!report.parts[1].written);
    let e = write(
        &ms,
        &Options {
            policy: Policy::Require,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(e.contains("sheet"), "{e}");
}

#[test]
fn units_and_orientation() {
    let mut ms = set();
    ms.parts.truncate(1);
    for t in &mut ms.parts[0].triangles {
        t.swap(1, 2);
    }
    let (bytes, report) = write(
        &ms,
        &Options {
            unit: Unit::Inch,
            title: "Plate & co".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(report.parts[0].reoriented);
    let pkg = validate(&bytes).unwrap();
    assert_eq!(pkg.unit, "inch");
    let q = gsolid::quantities(&slab()).unwrap().volume / 0.0254f64.powi(3);
    let v = volume(&pkg.objects[0]);
    assert!(v > 0.0 && (v - q).abs() < 0.01 * q, "{v} vs {q}");
    assert_eq!(Unit::parse("mm"), Some(Unit::Millimeter));
    assert_eq!(Unit::parse("Foot"), Some(Unit::Foot));
}

#[test]
fn degenerate_and_non_manifold_counts() {
    let mut ms = MeshSet::default();
    ms.push(Part {
        name: "fan".into(),
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [2.0, 0.0, 0.0]],
        // three triangles share edge 0-1 in the same direction; one is a sliver of zero area
        triangles: vec![[0, 1, 2], [0, 1, 3], [1, 0, 3], [0, 1, 4]],
        ..Default::default()
    });
    let (_, report) = write(&ms, &Options::default()).unwrap();
    let c = &report.parts[0];
    assert_eq!(c.degenerate, 1);
    assert_eq!(c.triangles, 3);
    assert!(c.bad_edges >= 1);
    assert!(!c.watertight());
}
