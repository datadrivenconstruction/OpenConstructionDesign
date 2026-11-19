//! Roofs and slabs on freeform hosts: exact layer volumes of a dome against the spherical
//! shell, following a host edit, the orphan rule, openings, refusals.

use super::*;
use crate::{FreeformSpec, ShellSide};
use ddc_geom::nurbs::{self, Trim};
use ddc_ops::{
    Actor, Cat, Envelope, GeoOp, Group, Len, Op, Ref, ShellKind as OpKind, ShellLayer as OpLayer, SurfaceBy,
    Targets, P3,
};
use std::f64::consts::PI;

fn model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 3.0).unwrap();
    (m, l)
}

fn host(level: i64, surface: Surface) -> FreeformSpec {
    FreeformSpec {
        level,
        name: "Dome".into(),
        material: String::new(),
        surface,
        thickness: 0.0,
        side: ShellSide::Normal,
        trim: Trim::default(),
        origin: "points".into(),
    }
}

fn layer(name: &str, t: f64) -> ShellLayer {
    ShellLayer { name: name.into(), material: name.into(), thickness: t }
}

fn stack() -> Vec<ShellLayer> {
    vec![layer("Concrete", 0.12), layer("Insulation", 0.08)]
}

fn rel(a: f64, b: f64) -> f64 {
    (a / b - 1.0).abs()
}

/// Volume of a hemispherical shell between radii r0 and r1.
fn half_shell(r0: f64, r1: f64) -> f64 {
    2.0 / 3.0 * PI * (r1.powi(3) - r0.powi(3))
}

#[test]
fn dome_roof_layers_match_the_spherical_shell() {
    let (mut m, l) = model();
    let h = m.add_freeform(&host(l, nurbs::dome(v3(0.0, 0.0, 0.0), 5.0, 5.0).unwrap())).unwrap();
    let s = m.shell_on_host(h, ShellKind::Roof, stack(), ShellSide::Normal, vec![], "Dome roof").unwrap();
    let id = m.add_shell_elem(&s).unwrap();
    assert_eq!(m.shell_elem(id).unwrap(), s);
    assert_eq!(m.shells_on(h).unwrap(), vec![id]);
    assert_eq!(m.shell_elems(Some(l)).unwrap().len(), 1);

    let q = m.shell_elem_quantities(id).unwrap();
    assert!(q.exact);
    assert!(rel(q.layers[0].volume, half_shell(5.0, 5.12)) < 1e-9, "{}", q.layers[0].volume);
    assert!(rel(q.layers[1].volume, half_shell(5.12, 5.2)) < 1e-9, "{}", q.layers[1].volume);
    assert!(rel(q.volume, half_shell(5.0, 5.2)) < 1e-9);
    assert!(rel(q.area, 2.0 * PI * 25.0) < 1e-9);
    assert!((q.thickness - 0.2).abs() < 1e-15);
    // level elevation 3 m: the outer apex is at 3 + 5.2
    assert!((q.bbox[5] - 8.2).abs() < 5e-3 && (q.bbox[2] - 3.0).abs() < 1e-9, "{:?}", q.bbox);

    // the faceted layers are closed and within the chord tolerance of the exact volume
    let sm = m.shell_elem_mesh(id).unwrap();
    assert_eq!(sm.layers.len(), 2);
    assert!(sm.layers.iter().all(|x| x.is_watertight()));
    assert!(rel(sm.layers[0].volume().abs(), half_shell(5.0, 5.12)) < 5e-3);

    // the same stack hung below the surface and centred on it
    let mut r = s.clone();
    r.side = ShellSide::Reverse;
    r.kind = ShellKind::Slab;
    let rid = m.add_shell_elem(&r).unwrap();
    assert!(rel(m.shell_elem_quantities(rid).unwrap().volume, half_shell(4.8, 5.0)) < 1e-9);
    r.side = ShellSide::Center;
    m.update_shell_elem(rid, &r).unwrap();
    assert!(rel(m.shell_elem_quantities(rid).unwrap().volume, half_shell(4.9, 5.1)) < 1e-9);
    m.undo().unwrap();
    assert_eq!(m.shell_elem(rid).unwrap().side, ShellSide::Reverse);
}

#[test]
fn vault_slab_follows_the_host_and_turns_orphan() {
    let (mut m, l) = model();
    let v = nurbs::vault(v3(0.0, 0.0, 0.0), v3(6.0, 0.0, 0.0), 4.0, 2.0).unwrap();
    let h = m.add_freeform(&host(l, v.clone())).unwrap();
    let s = m.shell_on_host(h, ShellKind::Slab, vec![layer("Brick", 0.25)], ShellSide::Reverse, vec![], "").unwrap();
    let id = m.add_shell_elem(&s).unwrap();
    // half annulus of radii 1.75 and 2 over 6 m
    let want = PI / 2.0 * (4.0 - 1.75f64.powi(2)) * 6.0;
    assert!(rel(m.shell_elem_quantities(id).unwrap().volume, want) < 1e-9);

    // a host edit carries over in the same undo step
    let up = v.translated(v3(0.0, 0.0, 1.0));
    m.update_freeform(h, &host(l, up.clone())).unwrap();
    let e = m.shell_elem(id).unwrap();
    assert_eq!(e.surface, up);
    assert!(!e.orphan);
    m.undo().unwrap();
    assert_eq!(m.shell_elem(id).unwrap().surface, v);

    // a host too tight for the stack: the slab keeps its last surface, flagged orphan
    let tight = nurbs::vault(v3(0.0, 0.0, 0.0), v3(6.0, 0.0, 0.0), 0.4, 0.2).unwrap();
    m.update_freeform(h, &host(l, tight)).unwrap();
    let e = m.shell_elem(id).unwrap();
    assert!(e.orphan && e.surface == v);
    m.undo().unwrap();
    assert!(!m.shell_elem(id).unwrap().orphan);

    // deleting the host keeps the slab as an orphan; undo brings both back
    m.delete_freeform(h).unwrap();
    let e = m.shell_elem(id).unwrap();
    assert!(e.orphan && e.surface == v && e.host == Some(h));
    assert!(m.shell_elem_quantities(id).is_ok());
    m.undo().unwrap();
    assert!(!m.shell_elem(id).unwrap().orphan);

    let (_, loss) = m.freeform_to_mesh(h).unwrap();
    assert!(loss.iter().any(|x| x.contains("roofs and slabs: 1")), "{loss:?}");
    assert!(m.shell_elem(id).unwrap().orphan);
    m.undo().unwrap();
    assert!(!m.shell_elem(id).unwrap().orphan);

    m.delete_shell_elem(id).unwrap();
    assert!(m.shell_elem(id).is_err());
    m.undo().unwrap();
    assert!(m.shell_elem(id).is_ok());
}

#[test]
fn openings_cut_by_plan_projection() {
    let (mut m, l) = model();
    let h = m.add_freeform(&host(l, nurbs::dome(v3(0.0, 0.0, 0.0), 5.0, 5.0).unwrap())).unwrap();
    let plain = m.shell_on_host(h, ShellKind::Roof, stack(), ShellSide::Normal, vec![], "").unwrap();
    let full = m.add_shell_elem(&plain).unwrap();
    let hole = vec![[1.0, 1.0], [2.0, 1.0], [2.0, 2.0], [1.0, 2.0]];
    let s = m.shell_on_host(h, ShellKind::Roof, stack(), ShellSide::Normal, vec![hole], "").unwrap();
    let id = m.add_shell_elem(&s).unwrap();
    let q1 = m.shell_elem_quantities(id).unwrap();
    assert!(!q1.exact && m.shell_elem_quantities(full).unwrap().exact);
    // with an opening the figures come from facets: compare with the facets of the full dome
    let sf = m.freeform(h).unwrap().surface;
    let full_area = nurbs::tessellate_trimmed(&sf, &Trim::default(), &crate::freeform::TESS).unwrap().mesh.area();
    let full_vol: f64 = m.shell_elem_mesh(full).unwrap().layers.iter().map(|x| x.volume().abs()).sum();
    // 1 m2 in plan; on the sphere there (z about 4.5) the slope adds about 10 %
    let cut = full_area - q1.area;
    assert!(cut > 1.0 && cut < 1.3, "{cut}");
    // the stack grows outward: (1 + t / R) widens it by about 4 %
    let lost = full_vol - q1.volume;
    assert!(rel(lost, cut * 0.208) < 0.1, "{lost} vs {}", cut * 0.208);
    assert!(m.shell_elem_mesh(id).unwrap().loss.iter().any(|x| x.contains("openings: 1")));

    // off the surface, across the seam, too few corners: refused
    for o in [
        vec![[4.5, 0.5], [6.0, 0.5], [6.0, 1.0]],
        vec![[2.0, -0.5], [3.0, -0.5], [3.0, 0.5], [2.0, 0.5]],
        vec![[1.0, 1.0], [2.0, 1.0]],
    ] {
        let s = m.shell_on_host(h, ShellKind::Roof, stack(), ShellSide::Normal, vec![o], "").unwrap();
        assert!(m.add_shell_elem(&s).is_err());
    }
}

fn envelope(ops: Vec<Op>) -> Envelope {
    let mut e = Envelope::new(Actor::Agent { name: "test".into() }, "shells");
    e.groups.push(Group { id: 1, title: "g".into(), ops, depends_on: vec![], why: String::new() });
    e
}

#[test]
fn ops_dome_preset_layered_roof_and_orphan_on_delete() {
    let (mut m, l) = model();
    let dome = Op::Geometry(GeoOp::Freeform {
        slot: 0,
        level: Some(Ref::id(Cat::Level, l)),
        // absolute z: the rim on the level at 3 m
        surface: SurfaceBy::Dome { center: P3::m(0.0, 0.0, 3.0), radius: Len::m(5.0), rise: Len::m(5.0) },
        thickness: None,
        side: None,
        trim: None,
        material: None,
        name: Some("Dome".into()),
    });
    let roof = Op::Geometry(GeoOp::SurfaceShell {
        slot: 1,
        host: Ref::slot(0),
        kind: OpKind::Roof,
        layers: vec![
            OpLayer { thickness: Len::mm(120.0), material: Some("Concrete".into()), name: None },
            OpLayer { thickness: Len::mm(80.0), material: None, name: Some("Insulation".into()) },
        ],
        side: None,
        openings: None,
        name: Some("Dome roof".into()),
    });
    let r = m.apply_envelope(&envelope(vec![dome, roof])).unwrap();
    let (h, id) = match (&r.created[0].1, &r.created[1].1) {
        (Ref::Id { cat: Cat::Freeform, id: h }, Ref::Id { cat: Cat::SurfaceShell, id }) => (*h, *id),
        o => panic!("{o:?}"),
    };
    let s = m.shell_elem(id).unwrap();
    assert_eq!((s.host, s.kind, s.side), (Some(h), ShellKind::Roof, ShellSide::Normal));
    assert_eq!((s.layers[0].name.as_str(), s.layers[0].material.as_str()), ("Layer 1", "Concrete"));
    assert!((s.layers[0].thickness - 0.12).abs() < 1e-15);
    assert_eq!((s.layers[1].name.as_str(), s.layers[1].material.as_str()), ("Insulation", ""));
    assert!(rel(m.shell_elem_quantities(id).unwrap().volume, half_shell(5.0, 5.2)) < 1e-9);
    // the preset is stored in level coordinates: the rim sits at z 0
    let ff = m.freeform(h).unwrap();
    assert_eq!(ff.origin, "dome");
    assert!(ff.surface.point(0.0, 0.0).z.abs() < 1e-12);

    let bad = Op::Geometry(GeoOp::Freeform {
        slot: 0,
        level: Some(Ref::id(Cat::Level, l)),
        surface: SurfaceBy::Dome { center: P3::m(0.0, 0.0, 3.0), radius: Len::m(2.0), rise: Len::m(3.0) },
        thickness: None,
        side: None,
        trim: None,
        material: None,
        name: None,
    });
    assert!(m.apply_envelope(&envelope(vec![bad])).is_err());

    let del = Op::Delete { target: Targets::one(Ref::id(Cat::Freeform, h)) };
    let r = m.apply_envelope(&envelope(vec![del])).unwrap();
    assert!(r.loss.iter().any(|x| x.contains("1 roofs and slabs kept as orphans")), "{:?}", r.loss);
    assert!(m.shell_elem(id).unwrap().orphan);
    m.undo().unwrap();
    assert!(!m.shell_elem(id).unwrap().orphan);

    let del = Op::Delete { target: Targets::one(Ref::id(Cat::SurfaceShell, id)) };
    m.apply_envelope(&envelope(vec![del])).unwrap();
    assert!(m.shell_elem(id).is_err());
}

#[test]
fn refusals() {
    let (mut m, l) = model();
    let h = m.add_freeform(&host(l, nurbs::dome(v3(0.0, 0.0, 0.0), 2.0, 2.0).unwrap())).unwrap();
    let ok = m.shell_on_host(h, ShellKind::Roof, stack(), ShellSide::Normal, vec![], "").unwrap();
    for bad in [
        ShellElemSpec { layers: vec![], ..ok.clone() },
        ShellElemSpec { layers: vec![layer("x", 0.0)], ..ok.clone() },
        ShellElemSpec { layers: vec![layer("x", 2.5)], ..ok.clone() },
        // 2.1 m below a 2 m sphere folds through the centre
        ShellElemSpec { layers: vec![layer("x", 1.5), layer("y", 0.6)], side: ShellSide::Reverse, ..ok.clone() },
        ShellElemSpec { level: 999, ..ok.clone() },
        ShellElemSpec { host: Some(999), ..ok.clone() },
    ] {
        assert!(m.add_shell_elem(&bad).is_err(), "{:?}", bad.layers);
    }
    assert!(m.shell_elems(None).unwrap().is_empty());
    assert!(m.shell_on_host(999, ShellKind::Roof, stack(), ShellSide::Normal, vec![], "").is_err());
}
