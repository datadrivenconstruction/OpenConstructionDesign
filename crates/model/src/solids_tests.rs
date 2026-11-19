// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::{LayerSpec, PlanViewSpec, SectionSpec, SlabSpec, ViewKind, WallSpec};
use std::f64::consts::PI;

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<V2> {
    vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)]
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

fn layer(t: f64) -> Vec<LayerSpec> {
    vec![LayerSpec {
        material: "concrete".into(),
        thickness: t,
    }]
}

fn close(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() <= eps
}

#[test]
fn schema_version_includes_solids() {
    let m = Model::open(":memory:").unwrap();
    let v: i64 = m
        .sql()
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v, crate::schema::VERSION);
    let n: i64 = m
        .sql()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('solid','solid_host','solid_qty','solid_cut','solid_base')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 5);
}

#[test]
fn ifc_class_allow_list_canonical_names_and_refusals() {
    let c = SolidIfc::new("ifcmember", Some("chord")).unwrap();
    assert_eq!((c.class.as_str(), c.predefined.as_str()), ("IfcMember", "CHORD"));
    assert_eq!(c.entity(), "IFCMEMBER");
    let c = SolidIfc::new("Plate", None).unwrap();
    assert_eq!((c.class.as_str(), c.predefined.as_str()), ("IfcPlate", "NOTDEFINED"));
    for (class, types) in SOLID_IFC_CLASSES {
        for t in *types {
            assert_eq!(SolidIfc::new(class, Some(t)).unwrap().predefined, *t);
        }
    }
    let e = SolidIfc::new("IfcWall", None).unwrap_err().to_string();
    assert!(e.contains("IfcWall") && e.contains("IfcMember"), "{e}");
    let e = SolidIfc::new("IfcMember", Some("LINTEL")).unwrap_err().to_string();
    assert!(e.contains("LINTEL") && e.contains("CHORD"), "{e}");
    assert!(SolidIfc::new("IfcBeam", Some("USERDEFINED")).is_err());
    assert!(SolidIfc::new("", None).is_err());
}

#[test]
fn ifc_class_is_stored_edited_undone_and_refused_on_a_void() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let mut s = boxy(l, rect(0.0, 0.0, 0.3, 0.3), 0.0, 3.0, false);
    s.ifc = Some(SolidIfc::new("IfcColumn", Some("COLUMN")).unwrap());
    let id = m.add_solid(&s).unwrap();
    assert_eq!(m.solid(id).unwrap(), s);
    let mut b = s.clone();
    b.ifc = Some(SolidIfc::new("IfcBeam", Some("BEAM")).unwrap());
    m.update_solid(id, &b).unwrap();
    assert_eq!(m.solid(id).unwrap().ifc.unwrap().class, "IfcBeam");
    m.undo().unwrap();
    assert_eq!(m.solid(id).unwrap().ifc, s.ifc);
    // a void cuts, it is never an element of its own
    let mut v = boxy(l, rect(0.0, 0.0, 0.1, 0.1), 0.0, 1.0, true);
    v.ifc = Some(SolidIfc::new("IfcPlate", None).unwrap());
    assert!(m.add_solid(&v).is_err());
    // a hand-made class that skipped the allow-list is refused at the write
    let mut w = s.clone();
    w.ifc = Some(SolidIfc {
        class: "IfcWall".into(),
        predefined: "NOTDEFINED".into(),
    });
    assert!(m.add_solid(&w).is_err());
    // no class: NULL columns, the generic proxy
    let p = m.add_solid(&boxy(l, rect(1.0, 0.0, 2.0, 1.0), 0.0, 1.0, false)).unwrap();
    assert_eq!(m.solid(p).unwrap().ifc, None);
}

#[test]
fn old_solid_table_gets_the_class_columns() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE solid (id INTEGER PRIMARY KEY, level INTEGER NOT NULL, \
         name TEXT NOT NULL DEFAULT '', kind TEXT NOT NULL, void INTEGER NOT NULL DEFAULT 0, \
         material TEXT NOT NULL DEFAULT '', plane TEXT NOT NULL, def TEXT NOT NULL);",
    )
    .unwrap();
    migrate(&db).unwrap();
    migrate(&db).unwrap();
    assert!(crate::schema::has_col(&db, "solid", "ifc_class").unwrap());
    assert!(crate::schema::has_col(&db, "solid", "ifc_type").unwrap());
}

#[test]
fn crud_round_trip_quantities_and_undo() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 3.0).unwrap();
    let s = boxy(l, rect(0.0, 0.0, 2.0, 1.0), 0.0, 3.0, false);
    let id = m.add_solid(&s).unwrap();
    assert_eq!(m.solid(id).unwrap(), s);
    let q = m.solid_quantities(id).unwrap().unwrap();
    assert!(close(q.volume, 6.0, 1e-12) && close(q.net_volume, 6.0, 1e-12));
    assert!(close(q.area, 2.0 * 2.0 + 6.0 * 3.0, 1e-12));
    // world extent: raised to the level elevation
    assert!(close(q.bbox[2], 3.0, 1e-12) && close(q.bbox[5], 6.0, 1e-12));
    // edit: revolve instead of extrude
    let mut r = s.clone();
    r.form = Form::Revolve {
        profile: Section::polygon(rect(1.0, 0.0, 2.0, 1.0)),
        axis_o: v2(0.0, 0.0),
        axis_d: v2(0.0, 1.0),
        start: 0.0,
        end: 2.0 * PI,
    };
    m.update_solid(id, &r).unwrap();
    let q = m.solid_quantities(id).unwrap().unwrap();
    assert!(close(q.volume, PI * 3.0, 1e-9), "{}", q.volume);
    m.undo().unwrap();
    assert_eq!(m.solid(id).unwrap(), s);
    assert!(close(
        m.solid_quantities(id).unwrap().unwrap().volume,
        6.0,
        1e-12
    ));
    m.delete_solid(id).unwrap();
    assert!(m.solid(id).is_err());
    assert!(m.solid_quantities(id).unwrap().is_none());
    m.undo().unwrap();
    assert_eq!(m.solid(id).unwrap(), s);
    m.assert_integrity();
}

#[test]
fn json_round_trip_of_every_form() {
    let sec = Section {
        outer: BulgeRing {
            pts: vec![v2(0.0, 0.0), v2(1.0, 0.0), v2(1.0, 1.0)],
            bulges: vec![0.0, 0.3, 0.0],
        },
        holes: vec![],
    };
    let path = Path3 {
        pts: vec![v3(0.0, 0.0, 0.0), v3(3.0, 0.0, 0.0), v3(3.0, 3.0, 1.0)],
        bulges: vec![0.0, 0.2],
        smooth: None,
    };
    let forms = vec![
        Form::Extrusion {
            profile: sec.clone(),
            start: -1.0,
            end: 2.5,
        },
        Form::Blend {
            bottom: sec.outer.clone(),
            top: BulgeRing::from_polygon(rect(0.0, 0.0, 0.5, 0.5)),
            height: 2.0,
        },
        Form::Revolve {
            profile: sec.clone(),
            axis_o: v2(-1.0, 0.0),
            axis_d: v2(0.0, 1.0),
            start: 0.1,
            end: 1.7,
        },
        Form::Sweep {
            profile: sec.clone(),
            path: path.clone(),
        },
        Form::SweptBlend {
            bottom: sec.outer.clone(),
            top: sec.outer,
            path,
        },
    ];
    for f in forms {
        assert_eq!(form_of(&form_json(&f)).unwrap(), f);
    }
    assert!(form_of(&json!({"kind": "nope"})).is_err());
}

#[test]
fn invalid_specs_are_rejected_without_writing() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let mut s = boxy(l, rect(0.0, 0.0, 1.0, 1.0), 1.0, 1.0, false);
    assert!(matches!(m.add_solid(&s), Err(Error::Degenerate(_))));
    s.form = Form::Extrusion {
        profile: Section::polygon(rect(0.0, 0.0, 1.0, 1.0)),
        start: 0.0,
        end: 1.0,
    };
    s.frame.x = v3(2.0, 0.0, 0.0);
    assert!(matches!(m.add_solid(&s), Err(Error::Invalid(_))));
    s.frame = Frame::default();
    s.level = 999;
    assert!(m.add_solid(&s).is_err());
    assert!(m.solids(None).unwrap().is_empty());
}

#[test]
fn level_delete_blocked_by_solid() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let id = m
        .add_solid(&boxy(l, rect(0.0, 0.0, 1.0, 1.0), 0.0, 1.0, false))
        .unwrap();
    match m.delete_level(l) {
        Err(Error::Referenced { by, .. }) => assert!(by.contains(&("solid", id))),
        r => panic!("expected Referenced, got {r:?}"),
    }
    m.delete_solid(id).unwrap();
    m.delete_level(l).unwrap();
    m.assert_integrity();
}

#[test]
fn void_cuts_flat_slab_exactly_and_undoes() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let t = m.add_slab_type("floor", &layer(0.2)).unwrap();
    let slab = m
        .add_slab(&SlabSpec {
            level: l,
            slab_type: t,
            offset: 0.0,
            outer: vec![(0.0, 0.0), (10.0, 0.0), (10.0, 8.0), (0.0, 8.0)],
            holes: vec![],
        })
        .unwrap();
    let before = m.slab_quantities(slab).unwrap()[0].clone();
    // a 1×2 void through the slab (slab top at elevation 0, bottom −0.2)
    let v = m
        .add_solid(&boxy(l, rect(2.0, 2.0, 3.0, 4.0), -1.0, 1.0, true))
        .unwrap();
    let after = m.slab_quantities(slab).unwrap()[0].clone();
    assert!(close(before.volume - after.volume, 0.4, 1e-5), "{after:?}");
    assert!(close(before.area - after.area, 2.0, 1e-4), "{after:?}");
    let cuts = m.solid_cuts(v).unwrap();
    assert_eq!(cuts.len(), 1);
    assert_eq!((cuts[0].kind.as_str(), cuts[0].id), ("slab", slab));
    // a void has no net volume of its own
    assert_eq!(m.solid_quantities(v).unwrap().unwrap().net_volume, 0.0);
    m.undo().unwrap();
    let back = m.slab_quantities(slab).unwrap()[0].clone();
    assert!(close(back.volume, before.volume, 1e-12));
    m.assert_integrity();
}

#[test]
fn void_cuts_wall_layer_and_solid_with_explicit_hosts() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let wt = m.add_wall_type("w", &layer(0.2)).unwrap();
    let w = m
        .add_walls(&[WallSpec {
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        }])
        .unwrap()[0];
    let wv = |m: &Model| -> f64 {
        m.sql()
            .query_row("SELECT SUM(volume) FROM wall_qty WHERE wall=?1", [w], |r| {
                r.get(0)
            })
            .unwrap()
    };
    let full = wv(&m);
    let host = m
        .add_solid(&boxy(l, rect(1.5, -2.0, 3.0, -1.0), 0.0, 2.0, false))
        .unwrap();
    // the void intersects both the wall (1×0.2×1) and the solid (0.5×0.5×1)
    let v = m
        .add_solid(&boxy(l, rect(1.0, -1.5, 2.0, 1.0), 0.5, 1.5, true))
        .unwrap();
    assert!(close(full - wv(&m), 0.2, 1e-5), "{}", full - wv(&m));
    let hq = m.solid_quantities(host).unwrap().unwrap();
    assert!(
        close(hq.volume - hq.net_volume, 0.5 * 0.5 * 1.0, 1e-5),
        "{hq:?}"
    );
    // solid only: the wall is not cut
    m.set_solid_hosts(v, &[("solid".into(), host)]).unwrap();
    assert!(close(wv(&m), full, 1e-12));
    assert_eq!(m.solid_cuts(v).unwrap().len(), 1);
    m.undo().unwrap();
    assert!(m.solid_hosts(v).unwrap().is_empty());
    assert_eq!(m.solid_cuts(v).unwrap().len(), 2);
    // deleting the host solid: the void remains, the deduction disappears
    m.delete_solid(host).unwrap();
    assert_eq!(m.solid_cuts(v).unwrap().len(), 1);
    m.assert_integrity();
}

#[test]
fn move_rotate_and_copy_transform_work_plane() {
    use crate::Target;
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let id = m
        .add_solid(&boxy(l, rect(0.0, 0.0, 1.0, 1.0), 0.0, 1.0, false))
        .unwrap();
    m.move_elements(&[Target::Solid(id)], (5.0, 0.0)).unwrap();
    let q = m.solid_quantities(id).unwrap().unwrap();
    assert!(close(q.bbox[0], 5.0, 1e-12) && close(q.bbox[3], 6.0, 1e-12));
    let copies = m
        .copy_elements(&[Target::Solid(id)], &[(0.0, 3.0)])
        .unwrap();
    let Target::Solid(c) = copies[0] else {
        panic!("{copies:?}")
    };
    let qc = m.solid_quantities(c).unwrap().unwrap();
    assert!(close(qc.bbox[1], 3.0, 1e-12) && close(qc.volume, 1.0, 1e-12));
    m.undo().unwrap();
    assert!(m.solid(c).is_err());
    m.assert_integrity();
}

#[test]
fn plan_and_section_show_cut_and_projection() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let id = m
        .add_solid(&boxy(l, rect(0.0, 0.0, 2.0, 1.0), 0.0, 3.0, false))
        .unwrap();
    // a low solid below the cut plane: projection only
    let low = m
        .add_solid(&boxy(l, rect(4.0, 0.0, 5.0, 1.0), 0.0, 0.5, false))
        .unwrap();
    let pv = m
        .plan_view(&PlanViewSpec {
            level: l,
            cut_height: 1.2,
            grids: vec![],
        })
        .unwrap();
    let cut = pv.count(
        |p| matches!(p, ViewPrim::SectionCut { table: "solid", element, .. } if *element == id),
    );
    assert_eq!(cut, 1);
    let low_cut = pv.count(
        |p| matches!(p, ViewPrim::SectionCut { table: "solid", element, .. } if *element == low),
    );
    assert_eq!(low_cut, 0);
    let low_edges = pv
        .count(|p| matches!(p, ViewPrim::Edge { table: "solid", element, .. } if *element == low));
    assert!(low_edges >= 4, "{low_edges}");
    // section at y = 0.5 along x, looking toward +y
    let sv = m
        .section_view(
            &SectionSpec {
                kind: ViewKind::Section,
                name: "A".into(),
                a: (-1.0, 0.5),
                b: (6.0, 0.5),
                depth: 10.0,
                far_clip: false,
            },
            &[],
        )
        .unwrap();
    let area: f64 = sv
        .prims
        .iter()
        .filter_map(|p| match p {
            ViewPrim::SectionCut {
                table: "solid",
                element,
                rings,
                ..
            } if *element == id => {
                let r: Vec<V2> = rings.iter().map(|q| v2(q.0, q.1)).collect();
                Some(ddc_geom::poly::area(&r))
            }
            _ => None,
        })
        .sum();
    assert!(close(area, 6.0, 1e-9), "{area}");
}

#[test]
fn void_is_hidden_in_views() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    m.add_solid(&boxy(l, rect(0.0, 0.0, 1.0, 1.0), 0.0, 3.0, true))
        .unwrap();
    let pv = m
        .plan_view(&PlanViewSpec {
            level: l,
            cut_height: 1.2,
            grids: vec![],
        })
        .unwrap();
    assert_eq!(
        pv.count(|p| matches!(p, ViewPrim::SectionCut { table: "solid", .. })),
        0
    );
}

fn floor(m: &mut Model, l: i64) -> i64 {
    let t = m.add_slab_type("floor", &layer(0.2)).unwrap();
    m.add_slab(&SlabSpec {
        level: l,
        slab_type: t,
        offset: 0.0,
        outer: vec![(0.0, 0.0), (10.0, 0.0), (10.0, 8.0), (0.0, 8.0)],
        holes: vec![],
    })
    .unwrap()
}

fn one_wall(m: &mut Model, l: i64) -> i64 {
    let wt = m.add_wall_type("w", &layer(0.2)).unwrap();
    m.add_walls(&[WallSpec {
        a: (0.0, 0.0),
        b: (6.0, 0.0),
        level: l,
        base_offset: 0.0,
        height: 3.0,
        wall_type: wt,
        offset0: None,
    }])
    .unwrap()[0]
}

fn wall_vol(m: &Model, w: i64) -> f64 {
    m.sql()
        .query_row("SELECT SUM(volume) FROM wall_qty WHERE wall=?1", [w], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn overlapping_voids_cut_slab_once() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let slab = floor(&mut m, l);
    let before = m.slab_quantities(slab).unwrap()[0].clone();
    m.add_solid(&boxy(l, rect(2.0, 2.0, 4.0, 3.0), -1.0, 1.0, true))
        .unwrap();
    m.add_solid(&boxy(l, rect(3.0, 2.0, 5.0, 3.0), -1.0, 1.0, true))
        .unwrap();
    let after = m.slab_quantities(slab).unwrap()[0].clone();
    // a 3 × 1 union, not 2 + 2
    assert!(close(before.volume - after.volume, 0.6, 1e-9), "{after:?}");
    assert!(close(before.area - after.area, 3.0, 1e-9), "{after:?}");
}

#[test]
fn void_over_door_and_shaft_is_not_double_counted() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let w = one_wall(&mut m, l);
    m.add_opening(
        w,
        &crate::OpeningSpec {
            along: 1.0,
            width: 1.0,
            sill: 0.0,
            height: 2.0,
            kind: "door".into(),
        },
    )
    .unwrap();
    let full = wall_vol(&m, w);
    // void x ∈ [0.5, 2.5], z ∈ [0.5, 1.5]: the door has already removed x ∈ [1, 2]
    m.add_solid(&boxy(l, rect(0.5, -1.0, 2.5, 1.0), 0.5, 1.5, true))
        .unwrap();
    let d = full - wall_vol(&m, w);
    assert!(close(d, 1.0 * 0.2 * 1.0, 1e-9), "{d}");
    // a shaft through the slab; a void above the shaft and next to it
    let slab = floor(&mut m, l);
    m.add_shaft(&crate::ShaftSpec {
        level: l,
        top_level: l2,
        base_offset: -1.0,
        top_offset: 0.0,
        ring: vec![(6.0, 6.0), (7.0, 6.0), (7.0, 7.0), (6.0, 7.0)],
    })
    .unwrap();
    let before = m.slab_quantities(slab).unwrap()[0].clone();
    m.add_solid(&boxy(l, rect(6.0, 6.0, 8.0, 7.0), -1.0, 1.0, true))
        .unwrap();
    let after = m.slab_quantities(slab).unwrap()[0].clone();
    assert!(
        close(before.volume - after.volume, 0.2, 1e-9),
        "{before:?} {after:?}"
    );
    assert!(
        close(before.area - after.area, 1.0, 1e-9),
        "{before:?} {after:?}"
    );
}

#[test]
fn partial_recess_keeps_slab_area() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let slab = floor(&mut m, l);
    let before = m.slab_quantities(slab).unwrap()[0].clone();
    m.add_solid(&boxy(l, rect(1.0, 1.0, 3.0, 2.0), -0.01, 1.0, true))
        .unwrap();
    let after = m.slab_quantities(slab).unwrap()[0].clone();
    assert!(close(before.volume - after.volume, 0.02, 1e-9), "{after:?}");
    assert!(close(after.area, before.area, 1e-12), "{after:?}");
}

#[test]
fn repeated_regenerate_does_not_accumulate_cuts() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let slab = floor(&mut m, l);
    m.add_solid(&boxy(l, rect(2.0, 2.0, 3.0, 4.0), -1.0, 1.0, true))
        .unwrap();
    let once = m.slab_quantities(slab).unwrap()[0].clone();
    // the slab is not rebuilt (as invalid): solids regenerated twice in a row
    regenerate(&m.db).unwrap();
    regenerate(&m.db).unwrap();
    let twice = m.slab_quantities(slab).unwrap()[0].clone();
    assert!(
        close(twice.volume, once.volume, 1e-12),
        "{once:?} {twice:?}"
    );
    assert!(close(twice.area, once.area, 1e-12), "{once:?} {twice:?}");
    // a full regeneration gives the same result
    crate::regenerate(&m.db).unwrap();
    let full = m.slab_quantities(slab).unwrap()[0].clone();
    assert!(close(full.volume, once.volume, 1e-12), "{once:?} {full:?}");
}

#[test]
fn corrupt_definition_is_skipped_with_error() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let good = m
        .add_solid(&boxy(l, rect(0.0, 0.0, 1.0, 1.0), 0.0, 2.0, false))
        .unwrap();
    let bad = m
        .add_solid(&boxy(l, rect(3.0, 0.0, 4.0, 1.0), 0.0, 2.0, false))
        .unwrap();
    m.db.execute("UPDATE solid SET def='{broken' WHERE id=?1", [bad])
        .unwrap();
    crate::regenerate(&m.db).unwrap();
    assert!(m.solid_quantities(good).unwrap().is_some());
    assert!(m.solid_quantities(bad).unwrap().is_none());
    assert!(m.solid_error(bad).unwrap().is_some());
    assert!(m.solid_error(good).unwrap().is_none());
    assert_eq!(m.solid_errors().unwrap().len(), 1);
    assert_eq!(m.solids(None).unwrap().len(), 1);
    // model edits and views do not break
    let slab = floor(&mut m, l);
    assert!(!m.slab_quantities(slab).unwrap().is_empty());
    let pv = m
        .plan_view(&PlanViewSpec {
            level: l,
            cut_height: 1.2,
            grids: vec![],
        })
        .unwrap();
    assert_eq!(
        pv.count(|p| matches!(p, ViewPrim::SectionCut { table: "solid", .. })),
        1
    );
}

#[test]
fn deleted_host_drops_reference_and_undo_restores_it() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let w = one_wall(&mut m, l);
    let v = m
        .add_solid(&boxy(l, rect(1.0, -1.0, 2.0, 1.0), 0.5, 1.5, true))
        .unwrap();
    m.set_solid_hosts(v, &[("wall".into(), w)]).unwrap();
    m.delete_wall(w).unwrap();
    assert!(m.solid_hosts(v).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.solid_hosts(v).unwrap(), vec![("wall".to_string(), w)]);
    assert_eq!(m.solid_cuts(v).unwrap().len(), 1);
    // the host solid is deleted, so the reference to it goes away too
    let host = m
        .add_solid(&boxy(l, rect(1.0, 2.0, 2.0, 3.0), 0.0, 2.0, false))
        .unwrap();
    m.set_solid_hosts(v, &[("solid".into(), host)]).unwrap();
    m.delete_solid(host).unwrap();
    assert!(m.solid_hosts(v).unwrap().is_empty());
    m.assert_integrity();
}

#[test]
fn copied_wall_with_void_hosts_the_copy() {
    use crate::Target;
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let w = one_wall(&mut m, l);
    let v = m
        .add_solid(&boxy(l, rect(1.0, -1.0, 2.0, 1.0), 0.5, 1.5, true))
        .unwrap();
    m.set_solid_hosts(v, &[("wall".into(), w)]).unwrap();
    let out = m
        .copy_elements(&[Target::Solid(v), Target::Wall(w)], &[(0.0, 5.0)])
        .unwrap();
    let nw = out
        .iter()
        .find_map(|t| match t {
            Target::Wall(i) => Some(*i),
            _ => None,
        })
        .unwrap();
    let nv = out
        .iter()
        .find_map(|t| match t {
            Target::Solid(i) => Some(*i),
            _ => None,
        })
        .unwrap();
    assert_eq!(m.solid_hosts(nv).unwrap(), vec![("wall".to_string(), nw)]);
    assert!(close(wall_vol(&m, nw), wall_vol(&m, w), 1e-9));
}

#[test]
fn void_above_sloped_wall_top_is_not_counted() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let w = one_wall(&mut m, l);
    m.db.execute("UPDATE wall SET top_profile='0 1;6 1' WHERE id=?1", [w])
        .unwrap();
    crate::regenerate(&m.db).unwrap();
    let full = wall_vol(&m, w);
    // void z ∈ [0.5, 2.5], wall top at 1 m: only z ∈ [0.5, 1] is cut
    m.add_solid(&boxy(l, rect(1.0, -1.0, 2.0, 1.0), 0.5, 2.5, true))
        .unwrap();
    let d = full - wall_vol(&m, w);
    assert!(close(d, 1.0 * 0.2 * 0.5, 1e-9), "{d}");
}

#[test]
fn slab_mesh_cut_has_through_holes_and_reports_partial() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let slab = floor(&mut m, l);
    let plain = m.slab_mesh(slab).unwrap();
    let through = m
        .add_solid(&boxy(l, rect(2.0, 2.0, 3.0, 4.0), -1.0, 1.0, true))
        .unwrap();
    let recess = m
        .add_solid(&boxy(l, rect(5.0, 2.0, 6.0, 4.0), -0.05, 1.0, true))
        .unwrap();
    let (mesh, skipped) = m.slab_mesh_cut(slab).unwrap();
    assert!(mesh.triangles.len() > plain.triangles.len());
    assert_eq!(skipped, vec![recess]);
    assert!(!skipped.contains(&through));
}
