use super::from_mesh::{family_text, ImportSpec, Insertion, MeshPart};
use super::gen::*;
use super::meshbody::MeshBody;
use super::*;
use crate::math::{v3, V3};

fn near(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "{a} vs {b}");
}

fn built(f: &Family, ty: usize, inst: &[(String, Value)], lod: Lod) -> Built {
    let r = f.resolve(&f.types[ty].values, inst).unwrap();
    f.build(&r, lod).unwrap()
}

#[test]
fn every_generated_family_builds_every_type_at_every_level() {
    let cat = catalog();
    assert_eq!(cat.len(), 22);
    for text in cat {
        let f = Family::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert!(!f.types.is_empty(), "{}", f.name);
        for k in 0..f.types.len() {
            for lod in [Lod::Coarse, Lod::Medium, Lod::Fine] {
                let b = built(&f, k, &[], lod);
                assert!(b.volume > 0.0, "{} {}", f.name, f.types[k].name);
                assert!(!b.plan.is_empty(), "{} has no plan", f.name);
                assert!(!b.mesh.triangles.is_empty(), "{}", f.name);
            }
        }
    }
}

#[test]
fn doors_and_windows_are_wall_hosted_with_sizes_and_an_opening() {
    for text in [door(DoorKind::Single), door(DoorKind::Double), window(WindowKind::Casement)] {
        let f = Family::parse(&text).unwrap();
        assert_eq!(f.hosting, Hosting::Wall);
        assert!(matches!(f.category, Category::Door | Category::Window));
        for p in ["Width", "Height", "Sill"] {
            assert!(f.param(p).is_some(), "{} lacks {p}", f.name);
        }
        assert!(f.solids.iter().any(|s| s.void && s.name == "Opening"));
    }
    let f = Family::parse(&door(DoorKind::Single)).unwrap();
    assert_eq!(f.ifc, ("IfcDoor".to_string(), "DOOR".to_string()));
    let b = built(&f, 1, &[], Lod::Fine);
    near(b.bbox[1].x - b.bbox[0].x, 0.9, 1e-9);
    near(b.bbox[1].z, 2.1, 1e-9);
    // frame + leaf, the void cuts the wall and not the door
    let (w, h, fw, fd, lt) = (0.9, 2.1, 0.05, 0.1, 0.04);
    let frame = 2.0 * fw * fd * h + (w - 2.0 * fw) * fd * fw;
    let leaf = (w - 2.0 * fw) * lt * (h - fw);
    near(b.volume, frame + leaf, 1e-12);
}

#[test]
fn fixed_window_volume_is_exact_and_panes_follow_the_grid() {
    let f = Family::parse(&window(WindowKind::Fixed)).unwrap();
    let b = built(&f, 1, &[], Lod::Fine);
    let (w, h, fw, fd, gt) = (1.2, 1.4, 0.06, 0.08, 0.024);
    let v = 2.0 * fw * fd * h + 2.0 * (w - 2.0 * fw) * fd * fw + (w - 2.0 * fw) * (h - 2.0 * fw) * gt;
    near(b.volume, v, 1e-12);
    let f = Family::parse(&window(WindowKind::MultiPane(3, 2))).unwrap();
    assert_eq!(f.solids.iter().filter(|s| s.name.starts_with("Pane")).count(), 6);
    assert_eq!(f.solids.iter().filter(|s| s.name.starts_with("Mullion")).count(), 2);
    let b = built(&f, 2, &[], Lod::Fine);
    near(b.bbox[1].x - b.bbox[0].x, 1.8, 1e-9);
}

#[test]
fn stairs_reach_their_height() {
    for (k, n) in [(StairKind::Straight, 17), (StairKind::L, 18), (StairKind::U, 18)] {
        let f = Family::parse(&stair(k, n)).unwrap();
        let b = built(&f, 0, &[], Lod::Fine);
        near(b.bbox[1].z, 0.175 * n as f64, 1e-9);
        let steps = f.solids.iter().filter(|s| s.name.starts_with("Step")).count();
        assert_eq!(steps, if k == StairKind::Straight { n } else { n - 1 } as usize, "{k:?}");
    }
}

#[test]
fn steel_sections_have_exact_area_times_length() {
    let len = 2.5;
    let inst = vec![("Length".to_string(), Value::Num(len))];
    let (h, b, tw, tf) = (0.3, 0.15, 0.0071, 0.0107);
    let f = Family::parse(&steel(Steel::I { h, b, tw, tf })).unwrap();
    assert_eq!(f.types[0].name, "300x150x7.1x10.7");
    near(built(&f, 0, &inst, Lod::Fine).volume, (2.0 * b * tf + (h - 2.0 * tf) * tw) * len, 1e-12);
    let f = Family::parse(&steel(Steel::Rhs { h: 0.2, b: 0.1, t: 0.008 })).unwrap();
    let a = 0.2 * 0.1 - (0.2 - 0.016) * (0.1 - 0.016);
    near(built(&f, 0, &inst, Lod::Fine).volume, a * len, 1e-12);
    let f = Family::parse(&steel(Steel::Chs { d: 0.1683, t: 0.008 })).unwrap();
    let a = std::f64::consts::PI * (0.1683f64.powi(2) - (0.1683 - 0.016f64).powi(2)) / 4.0;
    near(built(&f, 0, &inst, Lod::Fine).volume, a * len, 1e-12);
    let f = Family::parse(&steel(Steel::L { h: 0.1, b: 0.1, t: 0.01 })).unwrap();
    near(built(&f, 0, &inst, Lod::Fine).volume, (0.1 * 0.01 + 0.09 * 0.01) * len, 1e-12);
}

#[test]
fn fittings_have_connectors_at_their_ends() {
    let f = Family::parse(&fitting(Fitting::PipeElbow)).unwrap();
    let r = f.resolve(&f.types[0].values, &[]).unwrap();
    let p = f.ports(&r).unwrap();
    assert_eq!(p.len(), 2);
    let d = 0.0603;
    near(p[1].at.x, 1.5 * d, 1e-12);
    near(p[1].at.y, 1.5 * d, 1e-12);
    let f = Family::parse(&fitting(Fitting::DuctTransition)).unwrap();
    let b = built(&f, 0, &[], Lod::Fine);
    // prismatoid of two rectangles: exact
    let (a0, a1, am) = (0.6 * 0.4, 0.4 * 0.3, 0.5 * 0.35);
    near(b.volume, 0.4 / 6.0 * (a0 + 4.0 * am + a1), 1e-12);
    near(b.bbox[1].x, 0.4, 1e-12);
}

fn cube(o: V3, s: f64) -> MeshBody {
    let p = |x: f64, y: f64, z: f64| v3(o.x + x * s, o.y + y * s, o.z + z * s);
    MeshBody::new(
        vec![
            p(0., 0., 0.),
            p(1., 0., 0.),
            p(1., 1., 0.),
            p(0., 1., 0.),
            p(0., 0., 1.),
            p(1., 0., 1.),
            p(1., 1., 1.),
            p(0., 1., 1.),
        ],
        vec![
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
        ],
    )
    .unwrap()
}

#[test]
fn mesh_family_stretches_with_its_sizes_and_keeps_materials() {
    let parts = vec![
        MeshPart {
            material: "Oak".into(),
            body: cube(v3(10.0, 20.0, 5.0), 0.5),
        },
        MeshPart {
            material: "Steel".into(),
            body: cube(v3(10.5, 20.0, 5.0), 0.5),
        },
    ];
    let mut spec = ImportSpec::new("Crate", Category::Furniture);
    spec.props = vec![("Pset_Common.Reference".into(), "A-1 \"x\"".into())];
    let text = family_text(&parts, &spec).unwrap();
    let f = Family::parse(&text).unwrap();
    assert_eq!(f.hosting, Hosting::Floor);
    assert!(f.param("Pset_Common_Reference").is_some(), "{text}");
    let b = built(&f, 0, &[], Lod::Fine);
    near(b.volume, 2.0 * 0.125, 1e-6);
    // insertion at the base centre
    near(b.bbox[0].x, -0.5, 1e-4);
    near(b.bbox[0].z, 0.0, 1e-9);
    assert_eq!(b.parts.iter().filter(|p| p.visible).map(|p| p.material.as_str()).collect::<Vec<_>>(), ["Oak", "Steel"]);
    // twice the width: twice the volume, box follows
    let r = f
        .resolve(&f.assignments("Width=2m").unwrap(), &[])
        .unwrap();
    let b = f.build(&r, Lod::Fine).unwrap();
    near(b.bbox[1].x - b.bbox[0].x, 2.0, 1e-4);
    near(b.volume, 0.5, 1e-5);
    let c = f.build(&r, Lod::Coarse).unwrap();
    assert_eq!(c.parts.iter().filter(|p| p.visible).count(), 1, "coarse is the box");
    near(c.bbox[1].x - c.bbox[0].x, 2.0, 1e-4);
    assert!(!c.plan.is_empty());
}

#[test]
fn open_surface_mesh_displays_without_volume() {
    let mut body = cube(v3(0.0, 0.0, 0.0), 1.0);
    body.tris.truncate(10);
    let parts = vec![MeshPart {
        material: "Plastic".into(),
        body,
    }];
    let text = family_text(&parts, &ImportSpec::new("Shell", Category::Generic)).unwrap();
    let f = Family::parse(&text).unwrap();
    let b = built(&f, 0, &[], Lod::Fine);
    assert_eq!(b.mesh.triangles.len(), 10);
}

#[test]
fn imported_door_becomes_a_hosted_family_with_an_opening() {
    let parts = vec![MeshPart {
        material: "Wood".into(),
        body: MeshBody::new(cube(v3(0.0, 0.0, 0.0), 1.0).pos.iter().map(|p| v3(p.x * 0.9, p.y * 0.05, p.z * 2.1)).collect(), cube(v3(0.0, 0.0, 0.0), 1.0).tris).unwrap(),
    }];
    let mut spec = ImportSpec::new("Imported Door", Category::Door);
    spec.insertion = Insertion::BaseCentre;
    let f = Family::parse(&family_text(&parts, &spec).unwrap()).unwrap();
    assert_eq!(f.hosting, Hosting::Wall);
    assert_eq!(f.ifc.0, "IfcDoor");
    let r = f.resolve(&f.assignments("Width=1200mm, Sill=0mm").unwrap(), &[]).unwrap();
    let b = f.build(&r, Lod::Fine).unwrap();
    near(b.bbox[1].x - b.bbox[0].x, 1.2, 1e-4);
    near(r.num("Height").unwrap(), 2.1, 1e-9);
    assert!(f.solids.iter().any(|s| s.void));
    let mut spec = ImportSpec::new("Flat", Category::Window);
    spec.insertion = Insertion::Origin;
    let flat = MeshPart {
        material: "Glass".into(),
        body: MeshBody::new(vec![v3(0., 0., 0.), v3(1., 0., 0.), v3(1., 1., 0.)], vec![[0, 1, 2]]).unwrap(),
    };
    assert!(family_text(&[flat], &spec).unwrap_err().contains("width and height"));
}

#[test]
fn large_mesh_gets_a_simplified_medium_level() {
    // a finely tessellated sphere
    let (nu, nv) = (48u32, 24u32);
    let mut pos = vec![];
    for j in 0..=nv {
        for i in 0..nu {
            let (u, v) = (
                std::f64::consts::TAU * i as f64 / nu as f64,
                std::f64::consts::PI * j as f64 / nv as f64,
            );
            pos.push(v3(v.sin() * u.cos(), v.sin() * u.sin(), -v.cos()));
        }
    }
    let mut tris = vec![];
    for j in 0..nv {
        for i in 0..nu {
            let a = j * nu + i;
            let b = j * nu + (i + 1) % nu;
            let (c, d) = (a + nu, b + nu);
            tris.push([a, b, d]);
            tris.push([a, d, c]);
        }
    }
    let parts = vec![MeshPart {
        material: "Stone".into(),
        body: MeshBody::new(pos, tris).unwrap(),
    }];
    let text = family_text(&parts, &ImportSpec::new("Ball", Category::Planting)).unwrap();
    let f = Family::parse(&text).unwrap();
    assert!(f.solids.iter().any(|s| s.name == "Body0M"), "medium level");
    let fine = built(&f, 0, &[], Lod::Fine).mesh.triangles.len();
    let med = built(&f, 0, &[], Lod::Medium).mesh.triangles.len();
    assert!(med * 2 < fine, "{med} vs {fine}");
}
