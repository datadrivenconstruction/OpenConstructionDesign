// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use ddc_geom::component::{Category, Family, Lod};
use std::fmt::Write as _;

fn family(parts: &[MeshPart], cat: Category) -> Family {
    let t = family_text(parts, &ImportSpec::new("T", cat)).unwrap();
    Family::parse(&t).unwrap()
}

fn volume(f: &Family) -> f64 {
    let r = f.resolve(&f.types[0].values, &[]).unwrap();
    f.build(&r, Lod::Fine).unwrap().volume
}

const OBJ: &str = "# two boxes, y up, centimetres
o Crate
v 0 0 0
v 100 0 0
v 100 0 -50
v 0 0 -50
v 0 40 0
v 100 40 0
v 100 40 -50
v 0 40 -50
usemtl Wood
f 1 4 3 2
f 5 6 7 8
f 1 2 6 5
usemtl Metal
f 2/1/1 3/1/1 7/1/1 6/1/1
f 3 4 8 7
f -5 -8 -4 -1
";

#[test]
fn obj_reads_materials_units_and_axes() {
    let s = obj::read(OBJ, Axes { unit: 0.01, y_up: true }).unwrap();
    assert_eq!(s.name, "Crate");
    let names: Vec<&str> = s.parts.iter().map(|p| p.material.as_str()).collect();
    assert_eq!(names, ["Wood", "Metal"]);
    let f = family(&s.parts, Category::Furniture);
    // 1 m × 0.5 m deep × 0.4 m high once y-up becomes z-up
    assert!((volume(&f) - 0.2).abs() < 1e-6, "{}", volume(&f));
    let r = f.resolve(&f.types[0].values, &[]).unwrap();
    let b = f.build(&r, Lod::Fine).unwrap();
    assert!((b.bbox[1].z - 0.4).abs() < 1e-6 && (b.bbox[1].y - b.bbox[0].y - 0.5).abs() < 1e-6);
    assert!(obj::read("f 1 2 3\n", Axes::GLTF).is_err());
}

#[test]
fn gltf_round_trips_through_the_own_writer() {
    let mut sc = crate::gltf::Scene::new("t");
    let m = sc.material("Oak", [0.6, 0.4, 0.2, 1.0], crate::gltf::Value::Null);
    let p: Vec<[f32; 3]> = vec![
        [0., 0., 0.],
        [2., 0., 0.],
        [2., 1., 0.],
        [0., 1., 0.],
        [0., 0., 0.5],
        [2., 0., 0.5],
        [2., 1., 0.5],
        [0., 1., 0.5],
    ];
    let t = [
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
    sc.add_part("box", m, &p, &t, crate::gltf::Value::Null).unwrap();
    let glb = crate::gltf::write_glb(&sc).unwrap();
    let s = gltf::read(&glb, None).unwrap();
    assert_eq!(s.parts.len(), 1);
    assert_eq!(s.parts[0].material, "Oak");
    let f = family(&s.parts, Category::Generic);
    assert!((volume(&f) - 1.0).abs() < 1e-6, "{}", volume(&f));
    let r = f.resolve(&f.types[0].values, &[]).unwrap();
    let b = f.build(&r, Lod::Fine).unwrap();
    assert!((b.bbox[1].z - 0.5).abs() < 1e-6, "z up kept: {:?}", b.bbox);
    assert!(gltf::read(b"glTF\x02\0\0\0", None).is_err());
}

/// Box B-rep of planes and lines in millimetres, as CAD systems write it.
fn step_box(w: f64, d: f64, h: f64) -> String {
    let mut s = String::new();
    let mut n = 10u32;
    let mut add = |s: &mut String, e: String| {
        n += 1;
        let _ = writeln!(s, "#{n}={e};");
        n
    };
    let c = [
        [0., 0., 0.],
        [w, 0., 0.],
        [w, d, 0.],
        [0., d, 0.],
        [0., 0., h],
        [w, 0., h],
        [w, d, h],
        [0., d, h],
    ];
    let pts: Vec<u32> = c
        .iter()
        .map(|p| add(&mut s, format!("CARTESIAN_POINT('',({:?},{:?},{:?}))", p[0], p[1], p[2])))
        .collect();
    let vx: Vec<u32> = pts.iter().map(|p| add(&mut s, format!("VERTEX_POINT('',#{p})"))).collect();
    let quads = [
        [0, 3, 2, 1],
        [4, 5, 6, 7],
        [0, 1, 5, 4],
        [1, 2, 6, 5],
        [2, 3, 7, 6],
        [3, 0, 4, 7],
    ];
    let mut edges: Vec<((usize, usize), u32)> = vec![];
    let mut faces = vec![];
    for q in quads {
        let mut oe = vec![];
        for k in 0..4 {
            let (a, b) = (q[k], q[(k + 1) % 4]);
            let (key, fwd) = if a < b { ((a, b), true) } else { ((b, a), false) };
            let e = match edges.iter().find(|x| x.0 == key) {
                Some(x) => x.1,
                None => {
                    let (p, r) = (c[key.0], c[key.1]);
                    let dv = [r[0] - p[0], r[1] - p[1], r[2] - p[2]];
                    let l = (dv[0] * dv[0] + dv[1] * dv[1] + dv[2] * dv[2]).sqrt();
                    let dir = add(&mut s, format!("DIRECTION('',({:?},{:?},{:?}))", dv[0] / l, dv[1] / l, dv[2] / l));
                    let vec = add(&mut s, format!("VECTOR('',#{dir},{l:?})"));
                    let line = add(&mut s, format!("LINE('',#{},#{vec})", pts[key.0]));
                    let e = add(&mut s, format!("EDGE_CURVE('',#{},#{},#{line},.T.)", vx[key.0], vx[key.1]));
                    edges.push((key, e));
                    e
                }
            };
            oe.push(add(&mut s, format!("ORIENTED_EDGE('',*,*,#{e},{})", if fwd { ".T." } else { ".F." })));
        }
        let l: Vec<String> = oe.iter().map(|e| format!("#{e}")).collect();
        let lp = add(&mut s, format!("EDGE_LOOP('',({}))", l.join(",")));
        let fb = add(&mut s, format!("FACE_OUTER_BOUND('',#{lp},.T.)"));
        let (a, b, cc) = (c[q[0]], c[q[1]], c[q[2]]);
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [cc[0] - a[0], cc[1] - a[1], cc[2] - a[2]];
        let nn = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let l = (nn[0] * nn[0] + nn[1] * nn[1] + nn[2] * nn[2]).sqrt();
        let nd = add(&mut s, format!("DIRECTION('',({:?},{:?},{:?}))", nn[0] / l, nn[1] / l, nn[2] / l));
        let ax = add(&mut s, format!("AXIS2_PLACEMENT_3D('',#{},#{nd},$)", pts[q[0]]));
        let pl = add(&mut s, format!("PLANE('',#{ax})"));
        faces.push(add(&mut s, format!("ADVANCED_FACE('',(#{fb}),#{pl},.T.)")));
    }
    let l: Vec<String> = faces.iter().map(|e| format!("#{e}")).collect();
    let sh = add(&mut s, format!("CLOSED_SHELL('',({}))", l.join(",")));
    let so = add(&mut s, format!("MANIFOLD_SOLID_BREP('Block',#{sh})"));
    let rgb = add(&mut s, "COLOUR_RGB('',1.,0.,0.)".into());
    let st = add(&mut s, format!("FILL_AREA_STYLE_COLOUR('',#{rgb})"));
    add(&mut s, format!("STYLED_ITEM('',(#{st}),#{so})"));
    format!(
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nFILE_NAME('b.step','',(''),(''),'','','');\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\nDATA;\n\
         #1=(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.));\n#2=PRODUCT('Block','Block','',());\n{s}ENDSEC;\nEND-ISO-10303-21;\n"
    )
}

#[test]
fn step_brep_box_tessellates_in_metres_with_its_colour() {
    let s = step::read(step_box(100.0, 200.0, 300.0).as_bytes()).unwrap();
    assert_eq!(s.name, "Block");
    assert_eq!(s.parts.len(), 1, "{:?}", s.warnings);
    assert_eq!(s.parts[0].material, "Colour #FF0000");
    let f = family(&s.parts, Category::Equipment);
    assert!((volume(&f) - 0.006).abs() < 1e-7, "{}", volume(&f));
    assert!(step::read(b"ISO-10303-21;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n").is_err());
}

#[test]
fn read_path_picks_the_reader_by_extension() {
    let dir = std::env::temp_dir().join(format!("ddc_compimport_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("garden_crate.obj");
    std::fs::write(&p, OBJ).unwrap();
    let s = read_path(&p, Axes { unit: 0.01, y_up: true }).unwrap();
    assert_eq!(s.name, "Crate");
    let q = dir.join("x.txt");
    std::fs::write(&q, "x").unwrap();
    assert!(read_path(&q, Axes::GLTF).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
