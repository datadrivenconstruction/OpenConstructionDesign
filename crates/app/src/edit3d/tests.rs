// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 3D grip to model operation; one action is one editor undo step; selection synchronization.

use super::grips::{grips_of, opening_row, type_thickness, wall_spec, Grip3, Role3};
use super::pick::{Cat, Target};
use super::xform::Xform;
use super::*;
use ddc_model::{
    BeamJoin, BeamSpec, ColumnSpec, Just, LayerSpec, Model, OpeningSpec, SlabSpec, WallSpec,
};

struct T {
    ed: Editor,
    wt: i64,
    st: i64,
    mt: i64,
}

fn base() -> T {
    let mut ed = Editor::new_in_memory().unwrap();
    let layer = [LayerSpec {
        material: "c".into(),
        thickness: 0.2,
    }];
    let wt = ed.model.add_wall_type("W e3d", &layer).unwrap();
    let st = ed.model.add_slab_type("S e3d", &layer).unwrap();
    let mt = ed.model.add_catalog_member_type("R 400x400").unwrap();
    ed.reload().unwrap();
    T { ed, wt, st, mt }
}

fn wall(t: &mut T, a: (f64, f64), b: (f64, f64)) -> i64 {
    let level = t.ed.level;
    let id =
        t.ed.model
            .add_wall(&WallSpec {
                a,
                b,
                level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t.wt,
                offset0: None,
            })
            .unwrap();
    t.ed.reload().unwrap();
    id
}

fn column(t: &mut T, at: (f64, f64)) -> i64 {
    let level = t.ed.level;
    let id =
        t.ed.model
            .add_column(&ColumnSpec {
                level,
                top_level: None,
                member_type: t.mt,
                structural: true,
                at,
                base_offset: 0.0,
                top_offset: 3.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap();
    t.ed.reload().unwrap();
    id
}

fn beam(t: &mut T) -> i64 {
    let level = t.ed.level;
    let id =
        t.ed.model
            .add_beam(&BeamSpec {
                level,
                member_type: t.mt,
                a: (0.0, 0.0, 3.0),
                b: (4.0, 0.0, 3.0),
                bulge: 0.0,
                yjust: Just::Center,
                zjust: Just::Center,
                rotation: 0.0,
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: BeamJoin::default(),
                join_end: BeamJoin::default(),
            })
            .unwrap();
    t.ed.reload().unwrap();
    id
}

fn slab(t: &mut T) -> i64 {
    let level = t.ed.level;
    let id =
        t.ed.model
            .add_slab(&SlabSpec {
                level,
                slab_type: t.st,
                offset: 0.0,
                outer: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)],
                holes: vec![],
            })
            .unwrap();
    t.ed.reload().unwrap();
    id
}

fn grip(m: &Model, t: Target, f: impl Fn(&Role3) -> bool) -> Grip3 {
    grips_of(m, 0.0, t)
        .into_iter()
        .find(|g| f(&g.role))
        .unwrap_or_else(|| panic!("no grip for {t:?}"))
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-5
}

#[test]
fn wall_height_grip_is_one_undo_step() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
    let tw = Target::new(Cat::Wall, w);
    let g = grip(&t.ed.model, tw, |r| *r == Role3::Top);
    assert!(near(g.value(g.pos) as f64, 3.0));
    let mut e = Edit3d::default();
    e.commit_grip(&mut t.ed, g.clone(), g.typed(4.25, g.pos));
    assert!(near(wall_spec(&t.ed.model, w).unwrap().height, 4.25));
    t.ed.undo();
    assert!(near(wall_spec(&t.ed.model, w).unwrap().height, 3.0));
    t.ed.redo();
    assert!(near(wall_spec(&t.ed.model, w).unwrap().height, 4.25));
    // bottom: the top stays in place
    let g = grip(&t.ed.model, tw, |r| *r == Role3::Base);
    e.commit_grip(&mut t.ed, g.clone(), g.pos.with_z(0.5));
    let s = wall_spec(&t.ed.model, w).unwrap();
    assert!(near(s.base_offset, 0.5) && near(s.height, 3.75));
}

#[test]
fn wall_lean_grip_tilts_the_wall_in_one_undo_step() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
    let tw = Target::new(Cat::Wall, w);
    let g = grip(&t.ed.model, tw, |r| *r == Role3::Lean);
    // at 3/4 of the axis on the vertical top, moving along the left normal (+y)
    assert!(near(g.pos.x as f64, 3.0) && near(g.pos.y as f64, 0.0) && near(g.pos.z as f64, 3.0));
    assert!(near(g.value(g.pos) as f64, 0.0));
    let shift = 3.0 * 10f64.to_radians().tan();
    let mut e = Edit3d::default();
    e.commit_grip(&mut t.ed, g.clone(), g.typed(shift as f32, g.pos));
    let l = t.ed.model.wall_lean(w).unwrap();
    assert!((l.angle - 10f64.to_radians()).abs() < 1e-6, "{}", l.angle.to_degrees());
    // the grip follows the lean; the preview shows the tilted axis surface
    let g2 = grip(&t.ed.model, tw, |r| *r == Role3::Lean);
    assert!(near(g2.pos.y as f64, shift));
    let pv = super::grips::preview(&t.ed.model, 0.0, &g2, g2.pos).unwrap();
    assert_eq!(pv[0].len(), 5);
    assert!(near(pv[0][2].y as f64, shift) && near(pv[0][2].z as f64, 3.0));
    t.ed.undo();
    assert!(t.ed.model.wall_lean(w).unwrap().is_plain());
    // beyond the largest lean the model refuses and nothing changes
    e.commit_grip(&mut t.ed, g.clone(), g.typed(10.0, g.pos));
    assert!(t.ed.model.wall_lean(w).unwrap().is_plain());
    // a top profile keeps the wall vertical: no lean grip
    t.ed.model.set_wall_top_profile(w, Some(&[(0.0, 3.0), (4.0, 3.5)])).unwrap();
    t.ed.reload().unwrap();
    assert!(grips_of(&t.ed.model, 0.0, tw).iter().all(|g| g.role != Role3::Lean));
}

/// An arc wall leans radially: the grip sits on the arc at 3/4 of its length and moves along
/// the local left normal (toward the centre of a left-turning arc); one undo step.
#[test]
fn arc_wall_lean_grip_moves_radially_in_one_undo_step() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
    t.ed.model.set_wall_bulge(w, 0.3).unwrap();
    t.ed.reload().unwrap();
    let tw = Target::new(Cat::Wall, w);
    let tol = ddc_geom::Tol::default();
    let ax = ddc_geom::arc::ArcAxis::new(ddc_geom::v2(0.0, 0.0), ddc_geom::v2(4.0, 0.0), 0.3, tol).unwrap();
    let p = ax.point_at(0.75 * ax.length());
    let g = grip(&t.ed.model, tw, |r| *r == Role3::Lean);
    assert!(near(g.pos.x as f64, p.x) && near(g.pos.y as f64, p.y) && near(g.pos.z as f64, 3.0));
    assert!(near(g.value(g.pos) as f64, 0.0));
    let shift = 3.0 * 8f64.to_radians().tan();
    let mut e = Edit3d::default();
    e.commit_grip(&mut t.ed, g.clone(), g.typed(shift as f32, g.pos));
    let l = t.ed.model.wall_lean(w).unwrap();
    assert!((l.angle - 8f64.to_radians()).abs() < 1e-6, "{}", l.angle.to_degrees());
    // the grip follows: on the concentric circle of radius r - shift
    let g2 = grip(&t.ed.model, tw, |r| *r == Role3::Lean);
    let q = ddc_geom::v2(g2.pos.x as f64, g2.pos.y as f64);
    assert!(((q - ax.center).len() - (ax.radius - shift)).abs() < 1e-4);
    // the preview: base arc and top arc, closed
    let pv = super::grips::preview(&t.ed.model, 0.0, &g2, g2.pos).unwrap();
    let ring = &pv[0];
    assert!(ring.len() > 5 && ring.first() == ring.last());
    let top = ring[ring.len() / 2];
    let r_top = (ddc_geom::v2(top.x as f64, top.y as f64) - ax.center).len();
    assert!((r_top - (ax.radius - shift)).abs() < 1e-4 && near(top.z as f64, 3.0));
    t.ed.undo();
    assert!(t.ed.model.wall_lean(w).unwrap().is_plain());
    t.ed.redo();
    assert!((t.ed.model.wall_lean(w).unwrap().angle - l.angle).abs() < 1e-12);
}

#[test]
fn wall_end_plan_grip_lifted_to_3d() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
    let g = grip(
        &t.ed.model,
        Target::new(Cat::Wall, w),
        |r| matches!(r, Role3::Plan(p) if p.role == crate::grips::Role::End(1)),
    );
    assert!(near(g.pos.z as f64, 0.0) && near(g.pos.x as f64, 4.0));
    super::grips::apply(&mut t.ed.model, 0.0, &g, Vec3::new(5.0, 1.0, 0.0)).unwrap();
    assert_eq!(wall_spec(&t.ed.model, w).unwrap().b, (5.0, 1.0));
    let pv = super::grips::preview(&t.ed.model, 0.0, &g, Vec3::new(6.0, 1.0, 0.0)).unwrap();
    assert!(!pv.is_empty());
}

#[test]
fn slab_offset_and_thickness_grips() {
    let mut t = base();
    let s = slab(&mut t);
    let ts = Target::new(Cat::Slab, s);
    let g = grip(&t.ed.model, ts, |r| *r == Role3::Offset);
    super::grips::apply(&mut t.ed.model, 0.0, &g, g.typed(0.3, g.pos)).unwrap();
    let off: f64 =
        t.ed.model
            .sql()
            .query_row("SELECT offset FROM slab WHERE id=?1", [s], |r| r.get(0))
            .unwrap();
    assert!(near(off, 0.3));
    // the type's only user: layers are scaled, the type is unchanged
    let g = grip(&t.ed.model, ts, |r| *r == Role3::Thickness);
    assert!(near(g.value(g.pos) as f64, 0.2));
    super::grips::apply(&mut t.ed.model, 0.0, &g, g.typed(0.25, g.pos)).unwrap();
    assert!(near(type_thickness(&t.ed.model, t.st), 0.25));
    // a second user of the type: the element gets a copy of the type
    let s2 = slab(&mut t);
    let g = grip(&t.ed.model, Target::new(Cat::Slab, s2), |r| {
        *r == Role3::Thickness
    });
    super::grips::apply(&mut t.ed.model, 0.0, &g, g.typed(0.1, g.pos)).unwrap();
    let st2: i64 =
        t.ed.model
            .sql()
            .query_row("SELECT stype FROM slab WHERE id=?1", [s2], |r| r.get(0))
            .unwrap();
    assert_ne!(st2, t.st);
    assert!(near(type_thickness(&t.ed.model, st2), 0.1));
    assert!(near(type_thickness(&t.ed.model, t.st), 0.25));
    t.ed.model.undo().unwrap();
    let back: i64 =
        t.ed.model
            .sql()
            .query_row("SELECT stype FROM slab WHERE id=?1", [s2], |r| r.get(0))
            .unwrap();
    assert_eq!(back, t.st);
    // too thin: an error, nothing is written
    let g = grip(&t.ed.model, ts, |r| *r == Role3::Thickness);
    assert!(super::grips::apply(&mut t.ed.model, 0.0, &g, g.typed(0.001, g.pos)).is_err());
}

#[test]
fn thickness_grip_copy_keeps_type_param_values() {
    let mut t = base();
    let _s1 = slab(&mut t);
    let s2 = slab(&mut t);
    let db = t.ed.model.sql();
    db.execute(
        "INSERT INTO pparam_val(param, kind, ref, value) VALUES (7,'slab_type',?1,'F-90')",
        [t.st],
    )
    .unwrap();
    db.execute(
        "INSERT INTO prop(kind, ref, key, value, unit) VALUES ('slab_type',?1,'Mark','S1','')",
        [t.st],
    )
    .unwrap();
    let count = |m: &Model| -> (i64, i64) {
        let c = |sql: &str| -> i64 { m.sql().query_row(sql, [], |r| r.get(0)).unwrap() };
        (
            c("SELECT count(*) FROM pparam_val"),
            c("SELECT count(*) FROM prop"),
        )
    };
    let before = count(&t.ed.model);
    let g = grip(&t.ed.model, Target::new(Cat::Slab, s2), |r| {
        *r == Role3::Thickness
    });
    let top = t.ed.model.op_top().unwrap();
    super::grips::apply(&mut t.ed.model, 0.0, &g, g.typed(0.1, g.pos)).unwrap();
    assert_eq!(t.ed.model.op_top().unwrap(), top + 1, "one undo step");
    let st2: i64 =
        t.ed.model
            .sql()
            .query_row("SELECT stype FROM slab WHERE id=?1", [s2], |r| r.get(0))
            .unwrap();
    assert_ne!(st2, t.st);
    let val = |m: &Model| -> Option<String> {
        m.sql()
            .query_row(
                "SELECT value FROM pparam_val WHERE kind='slab_type' AND ref=?1",
                [st2],
                |r| r.get(0),
            )
            .ok()
    };
    assert_eq!(val(&t.ed.model).as_deref(), Some("F-90"));
    let mark: String = t
        .ed
        .model
        .sql()
        .query_row(
            "SELECT value FROM prop WHERE kind='slab_type' AND ref=?1 AND key='Mark'",
            [st2],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mark, "S1");
    // undo removes the copied rows, the source type keeps its own
    t.ed.model.undo().unwrap();
    assert_eq!(count(&t.ed.model), before);
    // redo (the same action again) restores them
    super::grips::apply(&mut t.ed.model, 0.0, &g, g.typed(0.1, g.pos)).unwrap();
    assert_eq!(count(&t.ed.model), (before.0 + 1, before.1 + 1));
}

#[test]
fn opening_grips_along_sill_head() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (5.0, 0.0));
    let o =
        t.ed.model
            .add_opening(
                w,
                &OpeningSpec {
                    along: 1.0,
                    width: 1.0,
                    sill: 0.9,
                    height: 1.2,
                    kind: "window".into(),
                },
            )
            .unwrap();
    let to = Target::new(Cat::Opening, o);
    let m = &mut t.ed.model;
    let g = grip(m, to, |r| *r == Role3::Sill);
    assert!(near(g.value(g.pos) as f64, 0.9));
    super::grips::apply(m, 0.0, &g, g.typed(0.6, g.pos)).unwrap();
    let s = opening_row(m, o).unwrap().1;
    assert!(
        near(s.sill, 0.6) && near(s.height, 1.5),
        "head stays: {s:?}"
    );
    let g = grip(m, to, |r| *r == Role3::Along);
    super::grips::apply(m, 0.0, &g, g.typed(2.0, g.pos)).unwrap();
    assert!(near(opening_row(m, o).unwrap().1.along, 2.0));
    let g = grip(m, to, |r| *r == Role3::Head);
    super::grips::apply(m, 0.0, &g, g.typed(1.0, g.pos)).unwrap();
    assert!(near(opening_row(m, o).unwrap().1.height, 1.0));
    // a sill below the wall bottom is clamped to it; the top stays in place
    let g = grip(m, to, |r| *r == Role3::Sill);
    super::grips::apply(m, 0.0, &g, g.typed(-0.5, g.pos)).unwrap();
    let s = opening_row(m, o).unwrap().1;
    assert!(near(s.sill, 0.0) && near(s.height, 1.6), "{s:?}");
}

#[test]
fn opening_drag_stays_in_its_wall_and_wall_keeps_openings() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (5.0, 0.0));
    let spec = OpeningSpec {
        along: 1.0,
        width: 1.0,
        sill: 0.9,
        height: 1.2,
        kind: "window".into(),
    };
    let o = t.ed.model.add_opening(w, &spec).unwrap();
    let to = Target::new(Cat::Opening, o);
    let m = &mut t.ed.model;
    // along: no further than the wall end (5 m minus width)
    let g = grip(m, to, |r| *r == Role3::Along);
    super::grips::apply(m, 0.0, &g, g.typed(10.0, g.pos)).unwrap();
    assert!(near(opening_row(m, o).unwrap().1.along, 4.0));
    let g = grip(m, to, |r| *r == Role3::Width);
    super::grips::apply(m, 0.0, &g, g.typed(10.0, g.pos)).unwrap();
    assert!(
        near(opening_row(m, o).unwrap().1.width, 1.0),
        "width up to the wall end"
    );
    // top: not above the wall top (3 m)
    let g = grip(m, to, |r| *r == Role3::Head);
    super::grips::apply(m, 0.0, &g, g.typed(5.0, g.pos)).unwrap();
    let s = opening_row(m, o).unwrap().1;
    assert!(near(s.sill + s.height, 3.0), "{s:?}");
    // the model also rejects an opening beyond the wall
    assert!(m
        .update_opening(
            o,
            &OpeningSpec {
                along: 4.5,
                ..spec.clone()
            }
        )
        .is_err());
    // a wall cannot go below its openings, neither with its top nor its bottom
    let tw = Target::new(Cat::Wall, w);
    let g = grip(m, tw, |r| *r == Role3::Top);
    assert!(super::grips::apply(m, 0.0, &g, g.typed(1.5, g.pos)).is_err());
    let g = grip(m, tw, |r| *r == Role3::Base);
    assert!(super::grips::apply(m, 0.0, &g, g.pos + Vec3::Z * 0.5).is_err());
    assert!(near(wall_spec(m, w).unwrap().height, 3.0));
    // profile-defined top: there is no height grip
    m.set_wall_top_profile(w, Some(&[(0.0, 3.0), (2.5, 4.0), (5.0, 3.0)]))
        .unwrap();
    assert!(grips_of(m, 0.0, tw).iter().all(|g| g.role != Role3::Top));
}

#[test]
fn far_from_origin_grips_write_exact_f64() {
    let mut t = base();
    let c = column(&mut t, (500_000.123, 7.0));
    let m = &mut t.ed.model;
    let tc = Target::new(Cat::Column, c);
    let g = grip(
        m,
        tc,
        |r| matches!(r, Role3::Plan(p) if p.role == crate::grips::Role::Move),
    );
    super::grips::apply(m, 0.0, &g, g.pos + Vec3::X).unwrap();
    let at = m.column(c).unwrap().at;
    assert!(
        (at.0 - 500_001.123).abs() < 1e-6 && (at.1 - 7.0).abs() < 1e-9,
        "{at:?}"
    );
    let g = grip(m, tc, |r| *r == Role3::Top);
    super::grips::apply(m, 0.0, &g, g.pos + Vec3::Z * 0.25).unwrap();
    assert!(near(m.column(c).unwrap().top_offset, 3.25));
}

#[test]
fn mesh_moves_in_one_step_and_noop_leaves_no_step() {
    let mut t = base();
    let mesh = ddc_geom::mesh::Mesh {
        positions: vec![
            ddc_geom::v3(0.0, 0.0, 0.0),
            ddc_geom::v3(1.0, 0.0, 0.0),
            ddc_geom::v3(0.0, 1.0, 0.0),
        ],
        triangles: vec![[0, 1, 2]],
        face_of: vec![0],
    };
    let (pb, ib) = ddc_model::mesh_blobs(&mesh);
    let lv = t.ed.level;
    let id = t
        .ed
        .model
        .batch_walls("wall_e3d_test_mesh", |db| {
            db.execute(
                "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,'Furniture','Box',?2,?3)",
                (lv, pb, ib),
            )?;
            Ok(db.last_insert_rowid())
        })
        .unwrap();
    t.ed.reload().unwrap();
    let mut e = Edit3d::default();
    e.set_sel(&mut t.ed, vec![Target::new(Cat::Mesh, id)]);
    let r = ddc_render::Ray {
        origin: Vec3::new(0.0, 0.0, 10.0),
        dir: Vec3::NEG_Z,
    };
    let d = gizmo::Drag::begin(gizmo::Handle::Plane(2), Vec3::ZERO, &r).unwrap();
    let x0 = |ed: &Editor| ed.model.imported_mesh(id).unwrap().mesh.positions[0].x;
    e.drag = Some(Drag3::Gizmo {
        d,
        x: Some(Xform::Move(Vec3::new(2.0, 0.0, 0.0))),
    });
    e.finish_gizmo(&mut t.ed, false);
    assert!(near(x0(&t.ed), 2.0), "mesh moved by the gizmo");
    assert!(t.ed.can_undo());
    t.ed.undo();
    assert!(near(x0(&t.ed), 0.0));
    let before = t.ed.can_undo();
    // an external category and a move with no change: an error, no undo step
    e.set_sel(&mut t.ed, vec![Target::new(Cat::Ext(3), 1)]);
    e.drag = Some(Drag3::Gizmo {
        d,
        x: Some(Xform::Move(Vec3::X)),
    });
    e.finish_gizmo(&mut t.ed, false);
    assert_eq!(
        t.ed.can_undo(),
        before,
        "no undo step for an unsupported move"
    );
    assert!(t.ed.status.is_some());
    let room = [Target::new(Cat::Room, 99)];
    let r = xform::apply(&mut t.ed.model, 0.0, &room, &Xform::Move(Vec3::Z), false);
    assert!(matches!(r, Err(ddc_model::Error::Invalid(k)) if k == "e3d.err.noop"));
    assert!(err_text(true, &ddc_model::Error::Invalid("e3d.err.noop".into())) != "e3d.err.noop");
}

#[test]
fn escape_clears_drag_typing_and_dim_at_once() {
    let mut e = Edit3d::default();
    e.drag = Some(Drag3::Box {
        from: egui::pos2(1.0, 1.0),
    });
    e.typing = Some("1.5".into());
    e.edit_dim = Some(Role3::Top);
    e.escape();
    assert!(e.drag.is_none() && e.typing.is_none() && e.edit_dim.is_none());
    assert!(!e.busy() && !e.accepts_text());
}

struct Dummy;
impl super::grips::Provider for Dummy {
    fn owns(&self, _t: Target) -> bool {
        false
    }
    fn grips(&self, _m: &Model, _z0: f64, _t: Target) -> Result<Vec<Grip3>, ddc_model::Error> {
        Ok(vec![])
    }
    fn apply(
        &self,
        _m: &mut Model,
        _z0: f64,
        _g: &Grip3,
        _to: Vec3,
    ) -> Result<(), ddc_model::Error> {
        Ok(())
    }
}

#[test]
fn grip_provider_registration_is_idempotent() {
    for _ in 0..3 {
        super::grips::register(std::sync::Arc::new(Dummy));
    }
    let n = super::grips::registered()
        .iter()
        .filter(|k| k.ends_with("::Dummy"))
        .count();
    assert_eq!(n, 1);
}

#[test]
fn column_and_beam_grips() {
    let mut t = base();
    let c = column(&mut t, (1.0, 1.0));
    let b = beam(&mut t);
    let m = &mut t.ed.model;
    let g = grip(m, Target::new(Cat::Column, c), |r| *r == Role3::Top);
    super::grips::apply(m, 0.0, &g, g.typed(3.5, g.pos)).unwrap();
    assert!(near(m.column(c).unwrap().top_offset, 3.5));
    let g = grip(m, Target::new(Cat::Column, c), |r| *r == Role3::Base);
    super::grips::apply(m, 0.0, &g, g.pos.with_z(0.25)).unwrap();
    assert!(near(m.column(c).unwrap().base_offset, 0.25));
    assert!(
        super::grips::apply(m, 0.0, &g, g.pos.with_z(9.0)).is_err(),
        "base above top"
    );
    // a beam end raised (Ctrl) makes a sloped beam
    let tb = Target::new(Cat::Beam, b);
    let g = grip(m, tb, |r| *r == Role3::End3(1));
    assert!(g.lifts());
    let ray = ddc_render::Ray {
        origin: Vec3::new(4.0, -10.0, 4.0),
        dir: Vec3::Y,
    };
    let to = grip_to(&g, &ray, None, true, 0.0, false).unwrap();
    assert!((to - Vec3::new(4.0, 0.0, 4.0)).length() < 1e-4, "{to:?}");
    super::grips::apply(m, 0.0, &g, to).unwrap();
    assert!(near(m.beam(b).unwrap().b.2, 4.0));
    let g = grip(m, tb, |r| *r == Role3::Roll);
    super::grips::apply(m, 0.0, &g, g.typed(30.0, g.pos)).unwrap();
    assert!(near(m.beam(b).unwrap().rotation.to_degrees(), 30.0));
}

#[test]
fn gizmo_composite_is_one_undo_step_and_copy() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
    let c = column(&mut t, (1.0, 2.0));
    let mut e = Edit3d::default();
    let ts = vec![Target::new(Cat::Wall, w), Target::new(Cat::Column, c)];
    e.set_sel(&mut t.ed, ts.clone());
    use crate::editor::bodies::BodyKind;
    assert_eq!(
        t.ed.selection,
        vec![Sel::Wall(w), Sel::Body(BodyKind::Column, c)]
    );
    assert_eq!(t.ed.grip.focus, vec![(crate::grips::Kind::Column, c)]);
    let r = ddc_render::Ray {
        origin: Vec3::new(0.0, 0.0, 10.0),
        dir: Vec3::NEG_Z,
    };
    let d = gizmo::Drag::begin(gizmo::Handle::Plane(2), Vec3::ZERO, &r).unwrap();
    e.drag = Some(Drag3::Gizmo {
        d,
        x: Some(Xform::Move(Vec3::new(1.0, 1.0, 0.5))),
    });
    e.finish_gizmo(&mut t.ed, false);
    assert!(e.drag.is_none());
    let ws = wall_spec(&t.ed.model, w).unwrap();
    assert!(near(ws.a.0, 1.0) && near(ws.base_offset, 0.5));
    assert!(near(t.ed.model.column(c).unwrap().at.1, 3.0));
    t.ed.undo();
    assert!(near(wall_spec(&t.ed.model, w).unwrap().a.0, 0.0));
    assert!(near(wall_spec(&t.ed.model, w).unwrap().base_offset, 0.0));
    assert!(near(t.ed.model.column(c).unwrap().at.1, 2.0));
    t.ed.redo();
    assert!(near(t.ed.model.column(c).unwrap().at.1, 3.0));
    // copy with Ctrl: two walls, two columns; one undo removes both copies
    e.drag = Some(Drag3::Gizmo {
        d,
        x: Some(Xform::Move(Vec3::X * 5.0)),
    });
    e.finish_gizmo(&mut t.ed, true);
    let n = |m: &Model, tb: &str| -> i64 {
        m.sql()
            .query_row(&format!("SELECT COUNT(*) FROM {tb}"), [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!((n(&t.ed.model, "wall"), n(&t.ed.model, "col")), (2, 2));
    t.ed.undo();
    assert_eq!((n(&t.ed.model, "wall"), n(&t.ed.model, "col")), (1, 1));
}

#[test]
fn escape_unwinds_in_order_and_selection_sync() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
    let mut e = Edit3d::default();
    e.set_tool(Tool::Measure);
    e.measure.click(Vec3::ZERO);
    e.typing = Some("12".into());
    assert!(e.busy());
    e.escape();
    assert!(e.typing.is_none() && !e.measure.pts.is_empty());
    e.escape();
    assert!(e.measure.pts.is_empty() && e.tool == Tool::Measure);
    e.escape();
    assert_eq!(e.tool, Tool::Select);
    assert!(!e.busy());
    e.set_sel(&mut t.ed, vec![Target::new(Cat::Wall, w)]);
    assert_eq!(e.mirror, t.ed.selection);
}

#[test]
fn every_command_has_strings() {
    for id in COMMANDS {
        for ru in [false, true] {
            let k = format!("cmd.{id}");
            assert_ne!(tr(ru, &k), k, "{id}");
        }
    }
    for k in snap::ALL {
        assert_ne!(
            tr(true, &format!("e3d.snap.{}", k.key())),
            format!("e3d.snap.{}", k.key())
        );
    }
    for t in [
        Tool::Select,
        Tool::Box,
        Tool::Measure,
        Tool::Face,
        Tool::Pick3,
        Tool::Mirror,
        Tool::Array,
        Tool::Create(create::Kind::Wall),
        Tool::Create(create::Kind::Floor),
        Tool::Create(create::Kind::Column),
        Tool::Create(create::Kind::Beam),
    ] {
        let k = format!("e3d.hint.{}", t.key());
        assert_ne!(tr(false, &k), k);
    }
    // edit-error keys in the status line: every `e3d.err.*` key from the code exists in both languages
    let src = [
        include_str!("create.rs"),
        include_str!("grips.rs"),
        include_str!("mep_grips.rs"),
        include_str!("xform.rs"),
        include_str!("mirror_array.rs"),
    ];
    for s in src {
        for k in s
            .split('"')
            .filter(|k| k.starts_with("e3d.err.") && !k.contains(' '))
        {
            for ru in [false, true] {
                let e = ddc_model::Error::Invalid(k.into());
                assert_ne!(err_text(ru, &e), k, "{k}");
            }
        }
    }
}

#[test]
fn mirror_array_strings_exist_in_both_languages() {
    // every `x3d.*` key used by the tool
    let src = include_str!("mirror_array.rs");
    let mut keys: Vec<&str> = src
        .split('"')
        .filter(|k| k.starts_with("x3d.") && !k.contains(' ') && !k.ends_with('.'))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    assert!(keys.len() >= 20, "{keys:?}");
    for k in keys {
        for ru in [false, true] {
            assert_ne!(tr(ru, k), k, "{k} ({ru})");
        }
    }
    for ru in [false, true] {
        for k in ["x3d.axis.grid", "x3d.axis.wall", "e3d.hint.mirror", "e3d.hint.array"] {
            assert_ne!(tr(ru, k), k, "{k}");
        }
    }
}

#[test]
fn measure_keeps_selection_3d() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
    let mut e = Edit3d::default();
    e.set_sel(&mut t.ed, vec![Target::new(Cat::Wall, w)]);
    e.set_tool(Tool::Measure);
    e.measure.click(Vec3::ZERO);
    e.measure.click(Vec3::new(4.0, 0.0, 0.0));
    assert_eq!(e.sel, vec![Target::new(Cat::Wall, w)]);
    assert_eq!(t.ed.selection, vec![Sel::Wall(w)]);
    // the readout under the cursor comes from the pick query, not from selecting
    let h = crate::pick_rules::here_of(&t.ed, Sel::Wall(w));
    assert!((h.len.unwrap() - 4.0).abs() < 1e-9);
    assert_eq!(t.ed.selection, vec![Sel::Wall(w)]);
}

#[test]
fn a_compound_3d_move_failing_midway_leaves_the_model_as_it_was() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
    // an earlier edit: the rollback of the failed move must not reach it
    t.ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        m.move_elements(&[ddc_model::Target::Wall(w)], (0.0, 1.0))
    })))
    .unwrap();
    let (h0, n0) = (t.ed.model.content_hash().unwrap(), t.ed.undo_len());
    let mut e = Edit3d::default();
    // the wall moves first (plan operation), then the missing column fails the member edits
    e.set_sel(
        &mut t.ed,
        vec![Target::new(Cat::Wall, w), Target::new(Cat::Column, 999_999)],
    );
    let r = ddc_render::Ray {
        origin: Vec3::new(0.0, 0.0, 10.0),
        dir: Vec3::NEG_Z,
    };
    let d = gizmo::Drag::begin(gizmo::Handle::Plane(2), Vec3::ZERO, &r).unwrap();
    for copy in [false, true] {
        e.drag = Some(Drag3::Gizmo {
            d,
            x: Some(Xform::Move(Vec3::new(2.0, 0.0, 0.0))),
        });
        e.finish_gizmo(&mut t.ed, copy);
        assert_eq!(t.ed.model.content_hash().unwrap(), h0, "copy={copy}: nothing kept");
        assert_eq!(t.ed.undo_len(), n0, "copy={copy}: no undo step");
        assert!(near(wall_spec(&t.ed.model, w).unwrap().a.1, 1.0));
    }
    t.ed.undo();
    assert!(near(wall_spec(&t.ed.model, w).unwrap().a.1, 0.0), "the earlier edit undoes");
}

fn esc_event() -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: Some(egui::Key::Escape),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

/// Each 3D tool lights its ribbon command (resolved id); Esc through `pre_input` leaves it.
#[test]
fn tools_report_active_command_until_esc() {
    let cases = [
        ("e3d.box", "e3d.box"),
        ("mod.measure", "e3d.measure"),
        ("e3d.measure", "e3d.measure"),
        ("e3d.wall", "e3d.wall"),
        ("e3d.floor", "e3d.floor"),
        ("e3d.column", "e3d.column"),
        ("e3d.beam", "e3d.beam"),
        ("e3d.wp_face", "e3d.wp_face"),
        ("e3d.wp_pick", "e3d.wp_pick"),
    ];
    for (run, want) in cases {
        let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
        assert!(a.run(run), "{run}");
        assert!(a.view3d, "{run}");
        assert_eq!(a.active_command(), Some(want), "{run}");
        assert_eq!(a.sh.reg.resolve(run), want, "ribbon compares resolved ids");
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![esc_event()],
            ..Default::default()
        };
        let _ = ctx.run(input, |c| pre_input(&mut a, c));
        assert_eq!(a.view.e3.keys, vec![Key3::Esc], "{run}: 3D takes Esc");
        for k in std::mem::take(&mut a.view.e3.keys) {
            assert_eq!(k, Key3::Esc);
            a.view.e3.escape();
        }
        assert_eq!(a.view.e3.tool, Tool::Select, "{run}");
        assert_eq!(a.active_command(), None, "{run} after Esc");
    }
}

/// Mirror and array in 3D light the plan commands; a 3D tool left behind in the plan does not.
#[test]
fn mirror_array_and_plan_view_active_command() {
    let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    a.view3d = true;
    for (t, want) in [(Tool::Mirror, "mod.mirror"), (Tool::Array, "mod.array")] {
        a.view.e3.set_tool(t);
        assert_eq!(a.active_command(), Some(want));
    }
    a.view.e3.set_tool(Tool::Measure);
    a.view3d = false;
    assert_eq!(a.active_command(), None, "the plan runs no 3D tool");
    a.view3d = true;
    a.view.e3.set_tool(Tool::Select);
    assert_eq!(a.active_command(), None);
}

/// A 4 x 4 bicubic net over 6 x 6 m rising 1.5 m in the middle.
fn freeform(t: &mut T) -> i64 {
    use ddc_geom::{nurbs, v3};
    let mut ctrl = vec![];
    for i in 0..4 {
        for j in 0..4 {
            let (a, b) = (i as f64 / 3.0 - 0.5, j as f64 / 3.0 - 0.5);
            ctrl.push(v3(6.0 * i as f64 / 3.0, 6.0 * j as f64 / 3.0, 1.5 * (1.0 - 2.0 * a * a - 2.0 * b * b)));
        }
    }
    let k = nurbs::clamped_knots(4, 3);
    let surface = nurbs::Surface::new(3, 3, k.clone(), k, 4, 4, ctrl, vec![1.0; 16]).unwrap();
    let level = t.ed.level;
    let id = t
        .ed
        .model
        .add_freeform(&ddc_model::FreeformSpec {
            level,
            name: "Canopy".into(),
            material: "Glulam".into(),
            surface,
            thickness: 0.0,
            side: ddc_model::ShellSide::Normal,
            trim: nurbs::Trim::default(),
            origin: "points".into(),
        })
        .unwrap();
    t.ed.reload().unwrap();
    id
}

#[test]
fn control_point_drag_weight_and_normal_moves_are_one_undo_step_each() {
    crate::freeform_edit3d::install();
    let mut t = base();
    let id = freeform(&mut t);
    let z0 = t.ed.elevation;
    let tf = Target::new(Cat::Ext(crate::freeform_edit3d::CAT), id);
    let gs = grips_of(&t.ed.model, z0, tf);
    assert_eq!(gs.len(), 16);
    let g = gs[5].clone(); // control point (1, 1)
    let before = t.ed.model.freeform(id).unwrap();
    let mut e = Edit3d::default();
    // plain drag in the horizontal plane
    let to = g.pos + Vec3::new(0.5, -0.25, 0.0);
    e.commit_grip(&mut t.ed, g.clone(), to);
    let p = t.ed.model.freeform(id).unwrap().surface.ctrl()[5];
    assert!(near(p.x, before.surface.ctrl()[5].x + 0.5) && near(p.y, before.surface.ctrl()[5].y - 0.25));
    t.ed.undo();
    assert_eq!(t.ed.model.freeform(id).unwrap(), before);
    // Alt: weight along the normal; the typed value is the weight
    let gw = super::grips::variant(&t.ed.model, z0, &g, true, false);
    assert!(matches!(gw.axis, super::grips::Axis3::Line(_)) && gw.role != g.role);
    assert!(near(gw.value(gw.pos) as f64, 1.0));
    assert!(!super::grips::preview(&t.ed.model, z0, &gw, gw.typed(2.5, gw.pos)).unwrap()[0].is_empty());
    e.commit_grip(&mut t.ed, gw.clone(), gw.typed(2.5, gw.pos));
    let s = t.ed.model.freeform(id).unwrap().surface;
    assert!(near(s.weights()[5], 2.5) && s.ctrl() == before.surface.ctrl());
    t.ed.undo();
    assert_eq!(t.ed.model.freeform(id).unwrap(), before);
    // a weight at or below zero is refused and changes nothing
    e.commit_grip(&mut t.ed, gw.clone(), gw.typed(-0.5, gw.pos));
    assert_eq!(t.ed.model.freeform(id).unwrap(), before);
    // Shift: along the surface normal at the point's Greville parameters
    let gn = super::grips::variant(&t.ed.model, z0, &g, false, true);
    let super::grips::Axis3::Line(n) = gn.axis else { panic!("no normal line") };
    let (gu, gv) = (before.surface.greville_u(), before.surface.greville_v());
    let want = before.surface.normal(gu[1], gv[1]).unwrap().normalized();
    assert!((n - Vec3::new(want.x as f32, want.y as f32, want.z as f32)).length() < 1e-5);
    e.commit_grip(&mut t.ed, gn.clone(), gn.typed(0.4, gn.pos));
    let q = t.ed.model.freeform(id).unwrap().surface.ctrl()[5];
    let o = before.surface.ctrl()[5];
    assert!(near(q.x, o.x + 0.4 * want.x) && near(q.y, o.y + 0.4 * want.y) && near(q.z, o.z + 0.4 * want.z));
    t.ed.undo();
    assert_eq!(t.ed.model.freeform(id).unwrap(), before);
    // no modifier: the grip as is
    assert_eq!(super::grips::variant(&t.ed.model, z0, &g, false, false).axis, g.axis);
}
