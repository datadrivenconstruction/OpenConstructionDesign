//! Freeform export: exact advanced B-rep of a shell (closed, every edge used once each way,
//! loops connected), a bare rational surface, triangles for a trimmed patch, IFC2X3 down
//! conversion, and a doubly curved roof shell made through an ops envelope, re-imported.

use std::collections::HashMap;
use std::f64::consts::PI;

use super::super::{export_ifc, export_ifc_with, IfcOptions, IfcSchema};
use crate::ifc::{import_ifc, validate_ifc, StepFile, Value};
use ddc_geom::nurbs::{self, Curve, Trim};
use ddc_geom::v3;
use ddc_model::{FreeformSpec, Model, ShellSide};
use ddc_ops::{Actor, Cat, Curve3, CurveFit, Envelope, GeoOp, Group, Len, Op, Ref, ShellSide as OpSide, SurfaceBy, P3};

fn count(t: &str, e: &str) -> usize {
    t.matches(&format!("={e}(")).count()
}

fn clean(t: &str) {
    let rep = validate_ifc(t.as_bytes());
    assert!(rep.is_clean(), "{}", rep.brief(12));
}

fn half_cylinder() -> nurbs::Surface {
    let line = Curve::polyline(&[v3(2.0, 0.0, 0.0), v3(2.0, 0.0, 3.0)]).unwrap();
    nurbs::revolve(&line, v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 1.0), PI).unwrap()
}

fn spec(level: i64, surface: nurbs::Surface, thickness: f64) -> FreeformSpec {
    FreeformSpec {
        level,
        name: "Canopy".into(),
        material: "Glulam".into(),
        surface,
        thickness,
        side: ShellSide::Center,
        trim: Trim::default(),
        origin: "revolve".into(),
    }
}

/// Every edge of a closed shell is used once forward and once backward; each loop is a chain.
fn check_topology(t: &str) {
    let f = StepFile::parse(t.as_bytes()).unwrap();
    let ends = |e: u32| -> (u32, u32) {
        let a = f.args(e);
        (a[0].as_ref().unwrap(), a[1].as_ref().unwrap())
    };
    let mut uses: HashMap<u32, (u32, u32)> = HashMap::new();
    for (id, class) in f.iter() {
        if class != "IFCEDGELOOP" {
            continue;
        }
        let oes = f.args(id)[0].refs();
        let walk: Vec<(u32, u32)> = oes
            .iter()
            .map(|oe| {
                let a = f.args(*oe);
                let e = a[2].as_ref().unwrap();
                let fwd = matches!(&a[3], Value::Enum(x) if x == "T");
                let u = uses.entry(e).or_default();
                if fwd {
                    u.0 += 1
                } else {
                    u.1 += 1
                }
                let (s, t) = ends(e);
                if fwd {
                    (s, t)
                } else {
                    (t, s)
                }
            })
            .collect();
        for k in 0..walk.len() {
            assert_eq!(walk[k].1, walk[(k + 1) % walk.len()].0, "loop #{id} is broken");
        }
    }
    assert_eq!(uses.len(), 12);
    assert!(uses.values().all(|u| *u == (1, 1)), "{uses:?}");
}

#[test]
fn shell_is_an_exact_closed_advanced_brep() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 3.0).unwrap();
    m.add_freeform(&spec(l, half_cylinder(), 0.2)).unwrap();
    let t = export_ifc(&m).unwrap();
    clean(&t);
    assert_eq!(count(&t, "IFCADVANCEDBREP"), 1);
    assert_eq!(count(&t, "IFCADVANCEDFACE"), 6);
    assert_eq!(count(&t, "IFCEDGECURVE"), 12);
    assert_eq!(count(&t, "IFCORIENTEDEDGE"), 24);
    // the arc is rational: both offset faces and the two arc sides keep the weights
    assert!(count(&t, "IFCRATIONALBSPLINESURFACEWITHKNOTS") >= 2, "{t}");
    assert!(t.contains("'Body-FallBack','Tessellation'") && t.contains("'Body','AdvancedBrep'"));
    assert!(t.contains("'DDC_Freeform'") && t.contains("'GrossVolume'"));
    assert_eq!(count(&t, "IFCRELASSOCIATESMATERIAL"), 1);
    check_topology(&t);
}

#[test]
fn bare_surface_trimmed_patch_and_ifc2x3() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    m.add_freeform(&spec(l, half_cylinder(), 0.0)).unwrap();
    let t = export_ifc(&m).unwrap();
    clean(&t);
    assert_eq!(count(&t, "IFCSHELLBASEDSURFACEMODEL"), 1);
    assert_eq!(count(&t, "IFCADVANCEDFACE"), 1);
    assert_eq!(count(&t, "IFCRATIONALBSPLINESURFACEWITHKNOTS"), 1);
    assert!(t.contains("'Body','AdvancedSurface'"));

    let mut s = spec(l, half_cylinder(), 0.1);
    s.trim = Trim { outer: None, holes: vec![vec![[0.3, 0.3], [0.7, 0.3], [0.7, 0.7], [0.3, 0.7]]] };
    m.add_freeform(&s).unwrap();
    let t = export_ifc(&m).unwrap();
    clean(&t);
    // the trimmed shell is triangles only, as its Body
    assert_eq!(count(&t, "IFCADVANCEDFACE"), 1);
    assert_eq!(count(&t, "IFCTRIANGULATEDFACESET"), 2);
    assert_eq!(t.matches("'Body','Tessellation'").count(), 1);

    let opts = IfcOptions { schema: IfcSchema::Ifc2x3, timestamp: Some("2026-01-01T00:00:00".into()), ..Default::default() };
    let (t2, _) = export_ifc_with(&m, &opts).unwrap();
    clean(&t2);
    for e in ["IFCADVANCEDFACE", "IFCVERTEXPOINT", "IFCRATIONALBSPLINESURFACEWITHKNOTS", "IFCEDGECURVE"] {
        assert_eq!(count(&t2, e), 0, "{e}");
    }
    assert!(!t2.contains("Body-FallBack") && !t2.contains("Advanced"));
    assert_eq!(count(&t2, "IFCBUILDINGELEMENTPROXY"), 2);
}

/// A doubly curved roof shell made by an agent envelope (loft of three arches), exported
/// exactly and read back.
#[test]
fn doubly_curved_roof_shell_end_to_end() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("Roof", 6.0).unwrap();
    let curves = (0..3)
        .map(|k| {
            let y = 6.0 * k as f64;
            let sag = [0.0, -1.0, 0.0][k];
            Curve3 {
                pts: vec![
                    P3::m(0.0, y, 6.0 + sag),
                    P3::m(6.0, y, 8.5 + sag),
                    P3::m(12.0, y, 6.0 + sag),
                ],
                fit: Some(CurveFit::Through),
                degree: Some(2),
            }
        })
        .collect();
    let op = Op::Geometry(GeoOp::Freeform {
        slot: 0,
        level: Some(Ref::id(Cat::Level, l)),
        surface: SurfaceBy::Loft { curves, degree: Some(2), params: None },
        thickness: Some(Len::mm(150.0)),
        side: Some(OpSide::Reverse),
        trim: None,
        material: Some("Concrete".into()),
        name: Some("Roof shell".into()),
    });
    let mut e = Envelope::new(Actor::Agent { name: "test".into() }, "roof");
    e.groups.push(Group { id: 1, title: "roof".into(), ops: vec![op], depends_on: vec![], why: String::new() });
    m.apply_envelope(&e).unwrap();
    let t = export_ifc(&m).unwrap();
    clean(&t);
    assert_eq!(count(&t, "IFCADVANCEDBREP"), 1);
    assert_eq!(count(&t, "IFCBSPLINESURFACEWITHKNOTS"), 6, "polynomial loft: no weights");
    check_topology(&t);
    let q = m.freeform_quantities(m.freeforms(None).unwrap()[0].0).unwrap();
    assert!(q.area > 12.0 * 12.0 && q.volume > 0.15 * 144.0 * 0.95, "{q:?}");

    let mut back = Model::open(":memory:").unwrap();
    import_ifc(&mut back, t.as_bytes()).unwrap();
    assert!(!back.meshes(None).unwrap().is_empty());
}
