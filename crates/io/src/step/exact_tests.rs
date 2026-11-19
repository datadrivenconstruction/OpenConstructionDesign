//! Exact STEP round trips: our solids -> STEP -> exact import keep the volume and the face
//! kinds; the mesh reader of the same file agrees on the volume.

use std::f64::consts::FRAC_PI_2;

use ddc_geom::gsolid::{self, Form, Frame};
use ddc_geom::math::v3;
use ddc_geom::nurbs;

use super::write::{num_for_tests, text_for_tests, write, Schema, WriteOptions};
use super::{read_bytes, read_exact};
use crate::exact::build;
use crate::exact::sweep_tests::{bend, helix, l_sweep, square_to_circle};
use crate::exact::tests::{dome, slab, spindle, tilted, tube};
use crate::exact::{nurbs_face, recognize, volume, Ax3, Doc, Mapped, Node, Part};
use crate::meshx::{MeshSet, Units};

fn opt(schema: Schema, units: Units) -> WriteOptions {
    WriteOptions {
        schema,
        units,
        stamp: String::new(),
    }
}

fn one(name: &str, f: &Form, fr: &Frame, rgb: Option<[u8; 3]>) -> Doc {
    let (body, exact) = build::form(f, fr, 1e-3).unwrap();
    assert!(exact);
    Doc {
        name: name.into(),
        parts: vec![Part {
            name: name.into(),
            body,
            rgb,
        }],
        roots: vec![Node::leaf(name, Ax3::WORLD, 0)],
    }
}

fn mesh_volume(ms: &MeshSet) -> f64 {
    ms.parts
        .iter()
        .flat_map(|p| {
            p.triangles.iter().map(|t| {
                let [a, b, c] = t.map(|i| p.positions[i as usize]);
                let cr = [
                    b[1] * c[2] - b[2] * c[1],
                    b[2] * c[0] - b[0] * c[2],
                    b[0] * c[1] - b[1] * c[0],
                ];
                (a[0] * cr[0] + a[1] * cr[1] + a[2] * cr[2]) / 6.0
            })
        })
        .sum()
}

/// Round trip of one form: face histogram equal, volume within 0.1 %, mesh reader within 3 %.
fn round_trip(f: &Form, fr: &Frame, schema: Schema, units: Units) {
    let doc = one("part", f, fr, Some([200, 40, 30]));
    let text = write(&doc, &opt(schema, units)).unwrap();
    let (back, warns) = read_exact(text.as_bytes()).unwrap();
    assert!(warns.is_empty(), "{warns:?}");
    assert_eq!(back.parts.len(), 1);
    let b = &back.parts[0].body;
    b.check().unwrap();
    assert_eq!(b.histogram(), doc.parts[0].body.histogram());
    assert_eq!(back.parts[0].rgb, Some([200, 40, 30]));
    let q = gsolid::quantities(f).unwrap().volume;
    let m = recognize(b);
    let v = m.volume().unwrap_or_else(|| panic!("not recognised: {m:?}"));
    assert!((v - q).abs() <= 1e-3 * q, "exact {v} vs {q}");
    let (ms, _) = read_bytes(text.as_bytes(), "rt", None).unwrap();
    let mv = mesh_volume(&ms);
    assert!((mv - q).abs() <= 0.03 * q, "mesh {mv} vs {q}");
}

#[test]
fn extrusion_round_trip_ap214_mm() {
    round_trip(&slab(), &tilted(), Schema::Ap214, Units::Millimetre);
}

#[test]
fn revolve_round_trip_ap242_m() {
    round_trip(&spindle(), &Frame::default(), Schema::Ap242, Units::Metre);
}

#[test]
fn partial_revolve_round_trip() {
    round_trip(&dome(), &tilted(), Schema::Ap214, Units::Millimetre);
}

#[test]
fn void_round_trip() {
    round_trip(&tube(), &tilted(), Schema::Ap242, Units::Millimetre);
}

#[test]
fn header_names_the_schema_and_units() {
    let doc = one("p", &slab(), &Frame::default(), None);
    let a = write(&doc, &opt(Schema::Ap214, Units::Millimetre)).unwrap();
    assert!(a.contains("FILE_SCHEMA(('AUTOMOTIVE_DESIGN"));
    assert!(a.contains("SI_UNIT(.MILLI.,.METRE.)"));
    assert!(!a.contains("STYLED_ITEM"));
    let b = write(&doc, &opt(Schema::Ap242, Units::Metre)).unwrap();
    assert!(b.contains("AP242_MANAGED_MODEL_BASED_3D_ENGINEERING_MIM_LF"));
    assert!(b.contains("SI_UNIT($,.METRE.)"));
    assert!(b.starts_with("ISO-10303-21;") && b.trim_end().ends_with("END-ISO-10303-21;"));
}

#[test]
fn reals_and_strings_follow_part_21() {
    assert_eq!(num_for_tests(1.0), "1.");
    assert_eq!(num_for_tests(-0.0), "0.");
    assert_eq!(num_for_tests(0.25), "0.25");
    assert_eq!(text_for_tests("a'b\\c"), "'a''b\\\\c'");
    assert_eq!(text_for_tests("\u{0414}x"), "'\\X2\\0414\\X0\\x'");
}

#[test]
fn assembly_keeps_names_placements_and_colours() {
    let (bx, _) = build::form(&slab(), &Frame::default(), 1e-3).unwrap();
    let (sp, _) = build::form(&spindle(), &Frame::default(), 1e-3).unwrap();
    let rot = Ax3 {
        o: v3(5.0, 0.0, 1.0),
        z: v3(0.0, 0.0, 1.0),
        x: v3(0.0, 1.0, 0.0),
    };
    let doc = Doc {
        name: "rig".into(),
        parts: vec![
            Part {
                name: "slab".into(),
                body: bx,
                rgb: Some([255, 0, 0]),
            },
            Part {
                name: "spindle".into(),
                body: sp,
                rgb: Some([0, 0, 255]),
            },
        ],
        roots: vec![
            Node::leaf("slab A", Ax3::WORLD, 0),
            Node {
                name: "sub".into(),
                place: Ax3 {
                    o: v3(0.0, 10.0, 0.0),
                    ..Ax3::WORLD
                },
                part: None,
                children: vec![Node::leaf("slab B", rot, 0), Node::leaf("spindle", Ax3::WORLD, 1)],
            },
        ],
    };
    let text = write(&doc, &opt(Schema::Ap214, Units::Millimetre)).unwrap();
    assert_eq!(text.matches("NEXT_ASSEMBLY_USAGE_OCCURRENCE(").count(), 4);
    let (back, warns) = read_exact(text.as_bytes()).unwrap();
    assert!(warns.is_empty(), "{warns:?}");
    // one part per distinct solid, three placements
    assert_eq!(back.parts.len(), 2);
    let want = doc.placements();
    let got = back.placements();
    assert_eq!(got.len(), 3);
    for (path, part, xf) in &want {
        let name = path.last().unwrap();
        let (_, gp, gx) = got
            .iter()
            .find(|(p, _, _)| p.last() == Some(name))
            .unwrap_or_else(|| panic!("no placement '{name}' in {got:?}"));
        let (wb, gb) = (&doc.parts[*part], &back.parts[*gp]);
        assert_eq!(wb.rgb, gb.rgb);
        assert_eq!(wb.body.histogram(), gb.body.histogram());
        for p in [v3(0.0, 0.0, 0.0), v3(1.0, 2.0, 3.0)] {
            let d = (xf.apply(p) - gx.apply(p)).len();
            assert!(d < 1e-9, "{name}: placement differs by {d}");
        }
    }
}

#[test]
fn nurbs_sheet_round_trip() {
    let line = nurbs::Curve::polyline(&[v3(1.0, 0.0, 0.0), v3(1.5, 0.0, 2.0)]).unwrap();
    let s = nurbs::revolve(&line, v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 1.0), FRAC_PI_2 * 3.0).unwrap();
    let body = nurbs_face::sheet(&s).unwrap();
    let doc = Doc {
        name: "sheet".into(),
        parts: vec![Part {
            name: "sheet".into(),
            body,
            rgb: None,
        }],
        roots: vec![Node::leaf("sheet", Ax3::WORLD, 0)],
    };
    let text = write(&doc, &opt(Schema::Ap242, Units::Millimetre)).unwrap();
    assert!(text.contains("RATIONAL_B_SPLINE_SURFACE"));
    assert!(text.contains("SHELL_BASED_SURFACE_MODEL"));
    let (back, _) = read_exact(text.as_bytes()).unwrap();
    let Mapped::Sheets(got) = recognize(&back.parts[0].body) else { panic!("not sheets") };
    assert_eq!(got.len(), 1);
    let g = &got[0];
    assert_eq!((g.nu(), g.nv(), g.deg_u(), g.deg_v()), (s.nu(), s.nv(), s.deg_u(), s.deg_v()));
    for (a, b) in g.ctrl().iter().zip(s.ctrl()) {
        assert!((*a - *b).len() < 1e-9);
    }
    for (a, b) in g.weights().iter().zip(s.weights()) {
        assert!((a - b).abs() < 1e-12);
    }
    for (a, b) in g.knots_u().iter().zip(s.knots_u()) {
        assert!((a - b).abs() < 1e-12);
    }
    let p = (s.point(0.3, 0.6) - g.point(0.3, 0.6)).len();
    assert!(p < 1e-9);
}

/// Round trip of a swept form: B-spline and planar faces come back with the same kinds and the
/// same volume (0.1 %); the mesh reader agrees within 3 %.
fn swept_round_trip(f: &Form, fr: &Frame, schema: Schema, units: Units) {
    let doc = one("swept", f, fr, None);
    let v0 = volume(&doc.parts[0].body).unwrap();
    let text = write(&doc, &opt(schema, units)).unwrap();
    let (back, warns) = read_exact(text.as_bytes()).unwrap();
    assert!(warns.is_empty(), "{warns:?}");
    let b = &back.parts[0].body;
    b.check().unwrap();
    assert_eq!(b.histogram(), doc.parts[0].body.histogram());
    let v1 = volume(b).unwrap();
    assert!((v1 - v0).abs() <= 1e-3 * v0, "{v1} vs {v0}");
    let (ms, _) = read_bytes(text.as_bytes(), "rt", None).unwrap();
    let mv = mesh_volume(&ms);
    assert!((mv - v0).abs() <= 0.03 * v0, "mesh {mv} vs {v0}");
}

#[test]
fn sweep_round_trips() {
    swept_round_trip(&l_sweep(), &tilted(), Schema::Ap214, Units::Millimetre);
    swept_round_trip(&bend(), &Frame::default(), Schema::Ap242, Units::Metre);
    swept_round_trip(&helix(), &tilted(), Schema::Ap242, Units::Millimetre);
}

#[test]
fn blend_round_trip() {
    swept_round_trip(&square_to_circle(), &tilted(), Schema::Ap214, Units::Millimetre);
}

#[test]
fn refuses_a_broken_body() {
    let mut doc = one("p", &slab(), &Frame::default(), None);
    doc.parts[0].body.faces.pop();
    doc.parts[0].body.shells[0].pop();
    let e = write(&doc, &WriteOptions::default()).unwrap_err();
    assert!(e.contains("closed body"), "{e}");
}
