use std::collections::HashMap;

use ddc_geom::arc::BulgeRing;
use ddc_geom::{v2, v3, V2, V3};
use ddc_model::{Form, MassSpec, Model, SolidForm, SolidFrame, SolidSection, SolidSpec};

use super::merge_planar;
use crate::ifc::{export_ifc_with, plan_bytes, validate_ifc, IfcOptions, IfcSchema, PMesh};

const STAMP: &str = "2026-01-01T00:00:00";

fn opts(schema: IfcSchema) -> IfcOptions {
    IfcOptions {
        schema,
        timestamp: Some(STAMP.into()),
        ..Default::default()
    }
}

fn count(t: &str, e: &str) -> usize {
    t.matches(&format!("={e}(")).count()
}

fn poly(pts: &[(f64, f64)]) -> BulgeRing {
    BulgeRing {
        pts: pts.iter().map(|p| v2(p.0, p.1)).collect(),
        bulges: vec![0.0; pts.len()],
    }
}

fn circle(c: V2, r: f64) -> BulgeRing {
    BulgeRing {
        pts: vec![c + v2(r, 0.0), c + v2(-r, 0.0)],
        bulges: vec![1.0, 1.0],
    }
}

fn solid(m: &mut Model, l: i64, form: SolidForm) -> i64 {
    m.add_solid(&SolidSpec {
        level: l,
        name: String::new(),
        void: false,
        material: "concrete".into(),
        frame: SolidFrame::default(),
        form,
        ifc: None,
    })
    .unwrap()
}

fn volume(p: &PMesh) -> f64 {
    let v = |i: u32| {
        let q = p.positions[i as usize];
        v3(q[0], q[1], q[2])
    };
    p.indices
        .chunks(3)
        .map(|t| v(t[0]).dot(v(t[1]).cross(v(t[2]))))
        .sum::<f64>()
        .abs()
        / 6.0
}

fn bbox(pts: impl Iterator<Item = V3>) -> (V3, V3) {
    let big = f64::INFINITY;
    pts.fold((v3(big, big, big), v3(-big, -big, -big)), |(lo, hi), p| {
        (
            v3(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)),
            v3(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)),
        )
    })
}

fn pbox(p: &PMesh) -> (V3, V3) {
    bbox(p.positions.iter().map(|q| v3(q[0], q[1], q[2])))
}

/// Each written body read back by the own importer: volume within 1 %, bounding box within
/// `tol`; matched by volume.
fn read_back(m: &Model, ids: &[i64], t: &str, tol: f64) {
    let plan = plan_bytes(t.as_bytes()).unwrap();
    assert_eq!(plan.meshes.len(), ids.len(), "{:?}", plan.report.skipped);
    for id in ids {
        let want = m.solid_quantities(*id).unwrap().unwrap().volume;
        let mesh = m.solid_mesh(*id).unwrap();
        let (lo, hi) = bbox(mesh.positions.iter().copied());
        let got = plan
            .meshes
            .iter()
            .min_by(|a, b| (volume(a) - want).abs().total_cmp(&(volume(b) - want).abs()))
            .unwrap();
        let v = volume(got);
        assert!((v - want).abs() < 0.01 * want, "solid {id}: {v} vs {want}");
        let (glo, ghi) = pbox(got);
        assert!((glo - lo).len() < tol && (ghi - hi).len() < tol, "solid {id}: bbox");
    }
}

fn blend_model() -> (Model, Vec<i64>) {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let sq = [(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)];
    // top: half the bottom, shifted, listed from another vertex
    let top = [(1.5, 0.5), (1.5, 1.5), (0.5, 1.5), (0.5, 0.5)];
    let a = solid(
        &mut m,
        l,
        SolidForm::Blend {
            bottom: poly(&sq),
            top: poly(&top),
            height: 3.0,
        },
    );
    let b = solid(
        &mut m,
        l,
        SolidForm::Blend {
            bottom: circle(v2(6.0, 0.0), 1.0),
            top: circle(v2(6.3, 0.1), 0.6),
            height: 2.0,
        },
    );
    // square to circle: no taper, triangles
    let c = solid(
        &mut m,
        l,
        SolidForm::Blend {
            bottom: poly(&[(10.0, 0.0), (12.0, 0.0), (12.0, 2.0), (10.0, 2.0)]),
            top: circle(v2(11.0, 1.0), 0.8),
            height: 2.0,
        },
    );
    let d = solid(
        &mut m,
        l,
        SolidForm::Revolve {
            profile: SolidSection::polygon(vec![
                v2(1.0, 0.0),
                v2(2.0, 0.0),
                v2(2.0, 1.0),
                v2(1.0, 1.0),
            ]),
            axis_o: v2(0.0, 0.0),
            axis_d: v2(0.0, 1.0),
            start: 0.5,
            end: 2.0,
        },
    );
    (m, vec![a, b, c, d])
}

#[test]
fn similar_blends_are_tapered_extrusions_and_read_back() {
    let (m, ids) = blend_model();
    let (t, w) = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap();
    assert_eq!(count(&t, "IFCEXTRUDEDAREASOLIDTAPERED"), 2, "square and circle pairs");
    assert_eq!(count(&t, "IFCDERIVEDPROFILEDEF"), 2);
    assert_eq!(count(&t, "IFCREVOLVEDAREASOLID"), 1, "the revolve with a start angle");
    let tess = |t: &str| count(t, "IFCTRIANGULATEDFACESET") + count(t, "IFCPOLYGONALFACESET");
    assert_eq!(tess(&t), 1, "square to circle");
    assert!(t.contains("'AdvancedSweptSolid'"));
    assert!(w.iter().all(|x| !x.contains("triangles")), "{w:?}");
    let rep = validate_ifc(t.as_bytes());
    assert!(rep.is_clean(), "{:?}", rep.errors);
    read_back(&m, &ids, &t, 0.02);
}

#[test]
fn tessellated_option_and_ifc2x3_flatten_with_a_note() {
    let (m, ids) = blend_model();
    let flat = IfcOptions {
        tessellate: true,
        ..opts(IfcSchema::Ifc4)
    };
    let (t, w) = export_ifc_with(&m, &flat).unwrap();
    assert_eq!(count(&t, "IFCEXTRUDEDAREASOLIDTAPERED"), 0);
    let tess = count(&t, "IFCTRIANGULATEDFACESET") + count(&t, "IFCPOLYGONALFACESET");
    assert_eq!(tess, 3);
    assert!(
        w.iter().any(|x| x.starts_with("2 typed solids written as triangles: exact")),
        "{w:?}"
    );
    assert!(validate_ifc(t.as_bytes()).is_clean());
    read_back(&m, &ids, &t, 0.02);
    let (t2, w2) = export_ifc_with(&m, &opts(IfcSchema::Ifc2x3)).unwrap();
    assert!(!t2.contains("TAPERED") && !t2.contains("IFCTRIANGULATEDFACESET"));
    assert!(t2.contains("IFCFACETEDBREP"));
    assert!(w2.iter().any(|x| x.contains("IFC2X3 has no exact form")), "{w2:?}");
    let rep = validate_ifc(t2.as_bytes());
    assert!(rep.is_clean(), "{:?}", rep.errors);
    // the option does not leak into the next export
    let (t3, _) = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap();
    assert_eq!(count(&t3, "IFCEXTRUDEDAREASOLIDTAPERED"), 2);
}

#[test]
fn validator_rejects_a_taper_without_derived_end_and_a_short_face() {
    let (m, _) = blend_model();
    let (t, _) = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap();
    let line = t
        .lines()
        .find(|l| l.contains("=IFCEXTRUDEDAREASOLIDTAPERED("))
        .unwrap();
    let args = line.split_once('(').unwrap().1.trim_end_matches(");");
    let first = args.split(',').next().unwrap();
    let last = args.rsplit(',').next().unwrap();
    let broken = line.replace(&format!(",{last});"), &format!(",{first});"));
    let bad = t.replacen(line, &broken, 1);
    let rep = validate_ifc(bad.as_bytes());
    assert!(rep.errors.iter().any(|e| e.rule == "taper-profiles"), "{:?}", rep.errors);

    let mut mm = Model::open(":memory:").unwrap();
    let l = mm.add_level("L1", 0.0).unwrap();
    mass(&mut mm, l);
    let o = IfcOptions {
        mass_forms: true,
        ..opts(IfcSchema::Ifc4)
    };
    let (t, _) = export_ifc_with(&mm, &o).unwrap();
    let face = t
        .lines()
        .find(|l| l.contains("=IFCINDEXEDPOLYGONALFACE(("))
        .unwrap();
    let (head, _) = face.split_once("((").unwrap();
    let bad = t.replacen(face, &format!("{head}((1,2));"), 1);
    let rep = validate_ifc(bad.as_bytes());
    assert!(rep.errors.iter().any(|e| e.rule == "face-index"), "{:?}", rep.errors);
}

fn mass(m: &mut Model, l: i64) -> i64 {
    m.add_mass(&MassSpec {
        level: l,
        name: "Tower".into(),
        void: false,
        inplace: false,
        form: Form::Cuboid {
            lo: (0.0, 0.0, 0.0),
            hi: (2.0, 3.0, 4.0),
        },
    })
    .unwrap()
}

#[test]
fn mass_forms_are_opt_in_proxies_with_polygonal_bodies() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    mass(&mut m, l);
    let (t, _) = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap();
    assert_eq!(count(&t, "IFCPOLYGONALFACESET"), 0, "off by default");
    let on = IfcOptions {
        mass_forms: true,
        ..opts(IfcSchema::Ifc4)
    };
    let (t, _) = export_ifc_with(&m, &on).unwrap();
    assert_eq!(count(&t, "IFCPOLYGONALFACESET"), 1);
    assert_eq!(count(&t, "IFCINDEXEDPOLYGONALFACE"), 6, "one face per box side");
    assert!(t.contains("'Tower',$,'cuboid'"), "ObjectType is the form kind");
    assert!(t.contains("'DDC_MassForm'"));
    let rep = validate_ifc(t.as_bytes());
    assert!(rep.is_clean(), "{:?}", rep.errors);
    let plan = plan_bytes(t.as_bytes()).unwrap();
    assert_eq!(plan.meshes.len(), 1, "{:?}", plan.report.skipped);
    assert!((volume(&plan.meshes[0]) - 24.0).abs() < 1e-6);
    let (lo, hi) = pbox(&plan.meshes[0]);
    assert!(lo.len() < 1e-9 && (hi - v3(2.0, 3.0, 4.0)).len() < 1e-9);
    // IFC2X3: faceted brep of the same faces
    let old = IfcOptions {
        mass_forms: true,
        ..opts(IfcSchema::Ifc2x3)
    };
    let (t2, _) = export_ifc_with(&m, &old).unwrap();
    assert!(!t2.contains("POLYGONALFACE"));
    assert_eq!(count(&t2, "IFCFACETEDBREP"), 1);
    let rep = validate_ifc(t2.as_bytes());
    assert!(rep.is_clean(), "{:?}", rep.errors);
}

/// Closed box of edge `n` cells of size `h`, each side an n x n grid of triangle pairs.
fn grid_box(n: u32, h: f64) -> (Vec<V3>, Vec<[u32; 3]>) {
    let mut ids: HashMap<[u32; 3], u32> = HashMap::new();
    let mut pos = vec![];
    let mut at = |k: [u32; 3], pos: &mut Vec<V3>| {
        *ids.entry(k).or_insert_with(|| {
            pos.push(v3(k[0] as f64 * h, k[1] as f64 * h, k[2] as f64 * h));
            pos.len() as u32 - 1
        })
    };
    let mut tris = vec![];
    for a in 0..3 {
        let (u, w) = ((a + 1) % 3, (a + 2) % 3);
        for side in [0, n] {
            for i in 0..n {
                for j in 0..n {
                    let key = |di: u32, dj: u32| {
                        let mut k = [0u32; 3];
                        k[a] = side;
                        k[u] = i + di;
                        k[w] = j + dj;
                        k
                    };
                    let q = [key(0, 0), key(1, 0), key(1, 1), key(0, 1)].map(|k| at(k, &mut pos));
                    if side == n {
                        tris.push([q[0], q[1], q[2]]);
                        tris.push([q[0], q[2], q[3]]);
                    } else {
                        tris.push([q[0], q[2], q[1]]);
                        tris.push([q[0], q[3], q[2]]);
                    }
                }
            }
        }
    }
    (pos, tris)
}

#[test]
fn planar_merge_finds_sides_and_holes() {
    let (pos, tris) = grid_box(12, 0.25);
    let faces = merge_planar(&pos, &tris);
    assert_eq!(faces.len(), 6);
    assert!(faces.iter().all(|(o, h)| o.len() == 48 && h.is_empty()));
    // a flat 3 x 3 grid without its middle cell: one face with one hole
    let p: Vec<V3> = (0..16).map(|k| v3((k % 4) as f64, (k / 4) as f64, 0.0)).collect();
    let mut t = vec![];
    for i in 0..3u32 {
        for j in 0..3u32 {
            if (i, j) == (1, 1) {
                continue;
            }
            let c = |di: u32, dj: u32| (j + dj) * 4 + i + di;
            t.push([c(0, 0), c(1, 0), c(1, 1)]);
            t.push([c(0, 0), c(1, 1), c(0, 1)]);
        }
    }
    let f = merge_planar(&p, &t);
    assert_eq!(f.len(), 1);
    assert_eq!((f[0].0.len(), f[0].1.len(), f[0].1[0].len()), (12, 1, 4));
    // a bent strip stays two faces
    let q = vec![v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(1.0, 1.0, 0.1), v3(0.0, 1.0, 0.0)];
    assert_eq!(merge_planar(&q, &[[0, 1, 2], [0, 2, 3]]).len(), 2);
}

#[test]
fn large_imported_mesh_is_a_polygonal_face_set() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let (pos, tris) = grid_box(12, 0.25);
    assert!(tris.len() >= super::LARGE);
    let pb: Vec<u8> = pos
        .iter()
        .flat_map(|p| [p.x, p.y, p.z])
        .flat_map(f64::to_le_bytes)
        .collect();
    let ib: Vec<u8> = tris.iter().flatten().flat_map(|i| i.to_le_bytes()).collect();
    m.sql()
        .execute(
            "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,'Generic','grid',?2,?3)",
            rusqlite::params![l, pb, ib],
        )
        .unwrap();
    for schema in [IfcSchema::Ifc4, IfcSchema::Ifc4x3] {
        let (t, _) = export_ifc_with(&m, &opts(schema)).unwrap();
        assert_eq!(count(&t, "IFCPOLYGONALFACESET"), 1);
        assert_eq!(count(&t, "IFCINDEXEDPOLYGONALFACE"), 6);
        let rep = validate_ifc(t.as_bytes());
        assert!(rep.is_clean(), "{:?}", rep.errors);
        let plan = plan_bytes(t.as_bytes()).unwrap();
        assert_eq!(plan.meshes.len(), 1);
        assert!((volume(&plan.meshes[0]) - 27.0).abs() < 1e-6);
    }
    let (t2, _) = export_ifc_with(&m, &opts(IfcSchema::Ifc2x3)).unwrap();
    assert!(!t2.contains("POLYGONALFACE") && t2.contains("IFCFACETEDBREP"));
    assert!(validate_ifc(t2.as_bytes()).is_clean());
}
