// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::SolidSpec;
use ddc_geom::gsolid::Frame;
use ddc_geom::mesh::Mesh;
use ddc_geom::mesh_ops::Affine;
use ddc_geom::{v2, Profile, Solid, Tol, V2};

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<V2> {
    vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)]
}

fn ngon(n: usize, r: f64) -> Vec<V2> {
    use std::f64::consts::{PI, TAU};
    (0..n)
        .map(|i| {
            let a = TAU * i as f64 / n as f64 + PI / n as f64;
            v2(r * a.cos(), r * a.sin())
        })
        .collect()
}

/// Profile (x, y) extruded along z by `depth`.
fn prism(outer: Vec<V2>, depth: f64) -> Mesh {
    let p = Profile::new(outer, vec![], Tol::default()).unwrap();
    let m = Solid::extrude(&p, depth, Tol::default())
        .triangulate()
        .unwrap();
    mesh_ops::transform(&m, &Affine::IDENTITY)
}

fn model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    (m, l)
}

fn add_mesh(m: &Model, level: i64, category: &str, mesh: &Mesh) -> i64 {
    let (p, i) = crate::mesh_blobs(mesh);
    m.sql()
        .execute(
            "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,?2,'n',?3,?4)",
            params![level, category, p, i],
        )
        .unwrap();
    m.sql().last_insert_rowid()
}

fn boxy(level: i64, r: Vec<V2>, z0: f64, z1: f64, void: bool) -> SolidSpec {
    SolidSpec {
        level,
        name: "box".into(),
        void,
        material: "concrete".into(),
        ifc: None,
        frame: Frame::default(),
        form: Form::Extrusion {
            profile: Section::polygon(r),
            start: z0,
            end: z1,
        },
    }
}

fn count(m: &Model, t: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
        .unwrap()
}

/// Row counts of the touched tables, mesh blobs and the journal length.
fn state(m: &Model) -> (Vec<i64>, Vec<(i64, Vec<u8>, Vec<u8>)>, usize) {
    let counts = ["mesh", "solid", "solid_host", "prop", "wall", "slab", "col", "beam"]
        .iter()
        .map(|t| count(m, t))
        .collect();
    let mut st = m
        .sql()
        .prepare("SELECT id, positions, indices FROM mesh ORDER BY id")
        .unwrap();
    let blobs = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    (counts, blobs, m.journal_len().unwrap())
}

fn has(r: &LossReport, what: &str) -> bool {
    r.loss.iter().any(|l| l.what == what)
}

#[test]
fn solid_to_mesh_reports_tree_loss() {
    let (mut m, l) = model();
    let s = m
        .add_solid(&boxy(l, rect(0.0, 0.0, 2.0, 1.0), 0.0, 3.0, false))
        .unwrap();
    let v = m
        .add_solid(&boxy(l, rect(0.5, 0.25, 1.0, 0.75), -1.0, 4.0, true))
        .unwrap();
    assert!(
        m.solid_cuts(v).unwrap().iter().any(|c| (c.kind.as_str(), c.id) == ("solid", s)),
        "precondition: the void cuts the solid"
    );
    let req = ConvertRequest::new("solid", s, ConvertTo::Mesh);
    let dry = m.convert_kind_dry(&req).unwrap();
    assert!(has(&dry, "boolean tree"), "{dry:?}");
    assert!(has(&dry, "parameters"), "{dry:?}");
    assert!(!has(&dry, "curved faces"), "box has no curved faces: {dry:?}");
    let tree = dry.loss.iter().find(|l| l.what == "boolean tree").unwrap();
    assert!(tree.measure.starts_with("1 void cut(s)"), "{}", tree.measure);

    let before = state(&m);
    let (r, mesh) = m.convert_kind(&req).unwrap();
    assert_eq!(r, dry);
    assert_eq!(r.result_table, "mesh");
    assert!(m.solid(s).is_err(), "source solid replaced");
    let info = m.mesh_info(mesh).unwrap();
    assert!((info.mesh.volume().abs() - 6.0).abs() < 1e-9, "uncut body kept");
    let mat: String = m
        .sql()
        .query_row(
            "SELECT value FROM prop WHERE kind='mesh' AND ref=?1 AND key=?2",
            params![mesh, SOLID_MATERIAL],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mat, "concrete");
    // one undo restores the solid and removes the mesh
    m.undo().unwrap();
    assert_eq!(state(&m), before);
    assert!(m.solid(s).is_ok());
    assert!(m.mesh_info(mesh).is_err());
    m.assert_integrity();
}

#[test]
fn mesh_to_solid_refuses_open_edges() {
    let (mut m, l) = model();
    let open = mesh_ops::delete_faces(&prism(rect(0.0, 0.0, 2.0, 1.0), 3.0), &[0]);
    let id = add_mesh(&m, l, "", &open);
    let before = state(&m);
    let req = ConvertRequest::new("mesh", id, ConvertTo::Solid);
    let e = m.convert_kind_dry(&req).unwrap_err().to_string();
    assert!(e.contains("open edge"), "{e}");
    let e = m.convert_kind(&req).unwrap_err().to_string();
    assert!(e.contains("open edge"), "{e}");
    assert_eq!(state(&m), before, "refusal writes nothing");
}

#[test]
fn mesh_fit_reports_tolerance() {
    let (mut m, l) = model();
    // 16 sides read as a round column: the circle keeps more area than the polygon
    let id = add_mesh(&m, l, "", &prism(ngon(16, 0.3), 3.0));
    let mut req = ConvertRequest::new("mesh", id, ConvertTo::Parametric);
    req.hint = Some(Kind::Column);
    let dry = m.convert_kind_dry(&req).unwrap();
    assert_eq!(dry.result_table, "col");
    let lost = dry
        .loss
        .iter()
        .find(|l| l.what == "geometry beyond fit tolerance")
        .unwrap_or_else(|| panic!("{dry:?}"));
    assert!(lost.measure.contains("tolerance 0.001 m"), "{}", lost.measure);
    // a looser tolerance accepts the same fit
    let mut loose = req.clone();
    loose.tol = 0.05;
    let r = m.convert_kind_dry(&loose).unwrap();
    assert!(!has(&r, "geometry beyond fit tolerance"), "{r:?}");
    let (applied, col) = m.convert_kind(&req).unwrap();
    assert_eq!(applied, dry);
    assert_eq!(count(&m, "col"), 1);
    assert!(col > 0);
    m.undo().unwrap();
    assert_eq!(count(&m, "col"), 0);
    assert!(m.mesh_info(id).is_ok());
}

#[test]
fn lossless_conversion_reports_nothing_lost() {
    let (mut m, l) = model();
    let id = add_mesh(&m, l, "", &prism(rect(0.0, 0.0, 2.0, 1.0), 3.0));
    let req = ConvertRequest::new("mesh", id, ConvertTo::Solid);
    let (r, s) = m.convert_kind(&req).unwrap();
    assert!(r.lossless(), "{r:?}");
    assert_eq!(r.result_table, "solid");
    let q = m.solid_quantities(s).unwrap().unwrap();
    assert!((q.volume - 6.0).abs() < 1e-9, "{q:?}");
    assert!(m.mesh_info(id).is_err(), "source mesh replaced");
    m.undo().unwrap();
    assert!(m.mesh_info(id).is_ok());
    assert_eq!(count(&m, "solid"), 0);
    m.assert_integrity();
}

#[test]
fn unsupported_pair_refused_with_reason_model_unchanged() {
    let (mut m, l) = model();
    let s = m
        .add_solid(&boxy(l, rect(0.0, 0.0, 1.0, 1.0), 0.0, 1.0, false))
        .unwrap();
    let mesh = add_mesh(&m, l, "", &prism(rect(0.0, 0.0, 1.0, 1.0), 1.0));
    let before = state(&m);
    for (t, id, to, why) in [
        ("solid", s, ConvertTo::Parametric, "convert to mesh first"),
        ("solid", s, ConvertTo::Solid, "already a solid"),
        ("mesh", mesh, ConvertTo::Copies, "not a pattern"),
        ("mesh", mesh, ConvertTo::Path, "not a curve"),
        ("wall", 1, ConvertTo::Mesh, "not a convertible kind"),
    ] {
        let req = ConvertRequest::new(t, id, to);
        let e = m.convert_kind(&req).unwrap_err().to_string();
        assert!(e.contains(why), "{t}->{}: {e}", to.key());
        let e = m.convert_kind_dry(&req).unwrap_err().to_string();
        assert!(e.contains(why), "{t}->{}: {e}", to.key());
    }
    // a void is refused too: as a mesh it would add material
    let v = m
        .add_solid(&boxy(l, rect(5.0, 5.0, 6.0, 6.0), 0.0, 1.0, true))
        .unwrap();
    let before_void = state(&m);
    let e = m
        .convert_kind(&ConvertRequest::new("solid", v, ConvertTo::Mesh))
        .unwrap_err()
        .to_string();
    assert!(e.contains("void"), "{e}");
    assert_eq!(state(&m), before_void);
    m.undo().unwrap();
    assert_eq!(state(&m), before);
}

#[test]
fn dry_run_report_equals_applied_report() {
    let (mut m, l) = model();
    let id = add_mesh(&m, l, "", &prism(ngon(16, 0.3), 3.0));
    let s = m
        .add_solid(&boxy(l, rect(4.0, 0.0, 6.0, 1.0), 0.0, 3.0, false))
        .unwrap();
    let mut col = ConvertRequest::new("mesh", id, ConvertTo::Parametric);
    col.hint = Some(Kind::Column);
    for req in [col, ConvertRequest::new("solid", s, ConvertTo::Mesh)] {
        let before = state(&m);
        let dry = m.convert_kind_dry(&req).unwrap();
        assert_eq!(state(&m), before, "dry run leaves the model unchanged");
        let (applied, _) = m.convert_kind(&req).unwrap();
        assert_eq!(dry, applied);
        assert_eq!(m.journal_len().unwrap(), before.2 + 1, "one operation");
    }
}
