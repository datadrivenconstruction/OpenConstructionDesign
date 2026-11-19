//! Each grip: a drag gives an exact model state; one undo; dimensions and snapping.

use super::*;
use ddc_model::{
    BeamJoin, ConstraintKind, ConstraintSpec, Just, LayerSpec, PipeMaterial, RailingHost,
    RailingSpec, ShaftSpec, SlabSpec, StairSpec, SystemKind, WallSpec,
};

struct M {
    m: Model,
    lv: i64,
    top: i64,
    wt: i64,
    st: i64,
}

fn base() -> M {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let top = m.add_level("L2", 3.0).unwrap();
    let layer = [LayerSpec {
        material: "c".into(),
        thickness: 0.2,
    }];
    let wt = m.add_wall_type("W", &layer).unwrap();
    let st = m.add_slab_type("S", &layer).unwrap();
    M { m, lv, top, wt, st }
}

fn wall(t: &mut M, a: (f64, f64), b: (f64, f64)) -> i64 {
    t.m.add_wall(&WallSpec {
        a,
        b,
        level: t.lv,
        base_offset: 0.0,
        height: 3.0,
        wall_type: t.wt,
        offset0: None,
    })
    .unwrap()
}

fn find(m: &Model, k: Kind, id: i64, role: Role) -> Grip {
    grips(m, k, id)
        .unwrap()
        .into_iter()
        .find(|g| g.role == role)
        .unwrap_or_else(|| panic!("{k:?} {role:?}"))
}

fn drag(m: &mut Model, k: Kind, id: i64, role: Role, to: V2) {
    let g = find(m, k, id, role);
    apply(m, &g, to).unwrap();
}

const SQ: [(f64, f64); 4] = [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)];

#[test]
fn wall_ends_flip_and_typed_length() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
    drag(&mut t.m, Kind::Wall, w, Role::End(1), v2(5.0, 0.0));
    assert_eq!(wall_axis(&t.m, w).unwrap(), (v2(0.0, 0.0), v2(5.0, 0.0)));
    drag(&mut t.m, Kind::Wall, w, Role::Flip, v2(0.0, 0.0));
    assert_eq!(wall_axis(&t.m, w).unwrap(), (v2(5.0, 0.0), v2(0.0, 0.0)));
    t.m.undo().unwrap();
    // a click on the temporary length dimension, then entering 3.25 m
    let g = find(&t.m, Kind::Wall, w, Role::End(1));
    let d = dims(&t.m, &g, g.pos).unwrap();
    assert!((d[0].value - 5.0).abs() < 1e-12, "primary dim is length");
    apply_value(&mut t.m, &g, &d[0], 3.25).unwrap();
    let (a, b) = wall_axis(&t.m, w).unwrap();
    assert_eq!(a, v2(0.0, 0.0));
    assert!((b - v2(3.25, 0.0)).len() < 1e-12);
}

#[test]
fn slab_vertex_and_edge() {
    let mut t = base();
    let s =
        t.m.add_slab(&SlabSpec {
            level: t.lv,
            slab_type: t.st,
            offset: 0.0,
            outer: SQ.to_vec(),
            holes: vec![],
        })
        .unwrap();
    drag(&mut t.m, Kind::Slab, s, Role::Vertex(2), v2(5.0, 4.0));
    assert_eq!(t.m.slab_rings(s).unwrap()[0][2], (5.0, 4.0));
    // edge 0 (the bottom one) moves only along its normal: drag sideways and down
    let g = find(&t.m, Kind::Slab, s, Role::Edge(0));
    apply(&mut t.m, &g, g.pos + v2(0.7, -1.0)).unwrap();
    let r = &t.m.slab_rings(s).unwrap()[0];
    assert!((r[0].1 + 1.0).abs() < 1e-12 && (r[1].1 + 1.0).abs() < 1e-12);
    assert!((r[0].0).abs() < 1e-12 && (r[1].0 - 4.0).abs() < 1e-12);
}

#[test]
fn ceiling_and_roof_boundaries() {
    let mut t = base();
    let c =
        t.m.add_ceiling(&CeilingSpec {
            level: t.lv,
            slab_type: t.st,
            height: 2.7,
            outer: SQ.to_vec(),
            holes: vec![],
        })
        .unwrap();
    drag(&mut t.m, Kind::Ceiling, c, Role::Vertex(1), v2(6.0, 0.0));
    assert_eq!(t.m.ceiling_rings(c).unwrap()[0][1], (6.0, 0.0));
    let r =
        t.m.add_roof(&RoofSpec {
            level: t.top,
            slab_type: t.st,
            offset: 0.0,
            overhang: 0.0,
            fascia: 0.2,
            kind: RoofKind::Footprint {
                ring: SQ.to_vec(),
                slopes: vec![Some(30.0), None, Some(30.0), None],
                mansard: None,
            },
        })
        .unwrap();
    let g = find(&t.m, Kind::Roof, r, Role::Edge(1));
    apply(&mut t.m, &g, g.pos + v2(1.5, 0.0)).unwrap();
    let RoofKind::Footprint { ring, .. } = t.m.roof(r).unwrap().kind else {
        panic!()
    };
    assert_eq!((ring[1], ring[2]), ((5.5, 0.0), (5.5, 3.0)));
}

#[test]
fn column_beam_move_rotate_ends() {
    let mut t = base();
    let mt = t.m.add_catalog_member_type("R 400x400").unwrap();
    let c =
        t.m.add_column(&ColumnSpec {
            level: t.lv,
            top_level: Some(t.top),
            member_type: mt,
            structural: true,
            at: (1.0, 1.0),
            base_offset: 0.0,
            top_offset: 0.0,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap();
    drag(&mut t.m, Kind::Column, c, Role::Move, v2(2.0, 3.0));
    drag(&mut t.m, Kind::Column, c, Role::Rotate, v2(2.0, 4.0));
    let s = t.m.column(c).unwrap();
    assert_eq!(s.at, (2.0, 3.0));
    assert!((s.rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
    let b =
        t.m.add_beam(&BeamSpec {
            level: t.top,
            member_type: mt,
            a: (0.0, 0.0, 0.0),
            b: (5.0, 0.0, 0.0),
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
    drag(&mut t.m, Kind::Beam, b, Role::End(1), v2(6.0, 2.0));
    assert_eq!(t.m.beam(b).unwrap().b, (6.0, 2.0, 0.0));
}

#[test]
fn stair_run_and_railing_path() {
    let mut t = base();
    let s =
        t.m.add_stair(&StairSpec::straight(t.lv, t.top, (0.0, 0.0), 0.0))
            .unwrap();
    let n = (t.m.stair_layout(s).unwrap().risers - 1) as f64;
    drag(&mut t.m, Kind::Stair, s, Role::Run, v2(n * 0.28, 0.4));
    assert!((t.m.stair(s).unwrap().tread.unwrap() - 0.28).abs() < 1e-12);
    let g = find(&t.m, Kind::Stair, s, Role::Run);
    assert!(
        (g.pos - v2(n * 0.28, 0.0)).len() < 1e-9,
        "run grip at new end"
    );
    drag(&mut t.m, Kind::Stair, s, Role::Move, v2(1.0, 2.0));
    assert_eq!(t.m.stair(s).unwrap().origin, (1.0, 2.0));
    let r =
        t.m.add_railing(&RailingSpec::new(
            t.lv,
            RailingHost::Path(vec![(0.0, 0.0, 0.0), (3.0, 0.0, 0.0), (3.0, 3.0, 0.5)]),
        ))
        .unwrap();
    drag(&mut t.m, Kind::Railing, r, Role::Vertex(2), v2(4.0, 3.0));
    assert_eq!(t.m.railing_path(r).unwrap()[2], (4.0, 3.0, 0.5));
}

#[test]
fn grid_level_extents() {
    let mut t = base();
    let g =
        t.m.add_grid(&GridSpec::line("A", (0.0, -1.0), (0.0, 9.0)))
            .unwrap();
    drag(&mut t.m, Kind::Grid, g, Role::End(1), v2(0.0, 12.0));
    let s = t.m.grids().unwrap().remove(0);
    assert_eq!((s.spec.b, s.name.as_str()), ((0.0, 12.0), "A"));
    drag(&mut t.m, Kind::Level, t.top, Role::Elevation, v2(0.0, 3.3));
    assert_eq!(level(&t.m, t.top).unwrap(), ("L2".into(), 3.3));
}

#[test]
fn level_value_may_be_negative() {
    let mut t = base();
    let g = find(&t.m, Kind::Level, t.top, Role::Elevation);
    let d = TempDim { a: v2(0.0, 0.0), b: v2(0.0, 3.0), value: 3.0 };
    apply_value(&mut t.m, &g, &d, -1.0).unwrap();
    assert_eq!(level(&t.m, t.top).unwrap().1, -1.0);
    let w = find(&t.m, Kind::Level, t.lv, Role::End(1));
    assert!(apply_value(&mut t.m, &Grip { kind: Kind::Wall, ..w }, &d, -1.0).is_err());
}

#[test]
fn component_origin_rotation_and_shape_param() {
    let mut t = base();
    let fam =
        t.m.load_family(
            "family Box\ncategory generic\nhost none\nparam Width length instance 1000mm\n\
             param Depth length instance 600mm\nparam Height length type 900mm\n\
             extrude Body rect(0, 0, Width, Depth) z(0, Height)\n",
        )
        .unwrap();
    let ct = t.m.add_comp_type(fam, "T", &[]).unwrap();
    let c =
        t.m.add_component(&CompSpec::at(t.lv, ct, 1.0, 1.0))
            .unwrap();
    let w = find(&t.m, Kind::Comp, c, Role::Param("Width".into(), 0));
    assert!((w.pos - v2(2.0, 1.0)).len() < 1e-9, "width handle at +x");
    apply(&mut t.m, &w, v2(2.5, 1.3)).unwrap();
    let s = t.m.component(c).unwrap();
    assert!(s
        .values
        .iter()
        .any(|v| v.0 == "Width" && v.1 == ParamValue::Num(1.5)));
    drag(&mut t.m, Kind::Comp, c, Role::Move, v2(4.0, 0.0));
    drag(&mut t.m, Kind::Comp, c, Role::Rotate, v2(4.0, 1.0));
    let s = t.m.component(c).unwrap();
    assert_eq!((s.at.0, s.at.1), (4.0, 0.0));
    assert!((s.rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
}

#[test]
fn mep_run_vertex() {
    let mut t = base();
    let sys = t.m.add_mep_system("CW", SystemKind::ColdWater).unwrap();
    let r =
        t.m.add_mep_run(
            &RunSpec::pipe(
                sys,
                t.lv,
                PipeMaterial::Copper,
                15,
                vec![(0.0, 0.0, 1.0), (2.0, 0.0, 1.0), (2.0, 2.0, 1.0)],
            )
            .unwrap(),
        )
        .unwrap();
    drag(&mut t.m, Kind::Mep, r, Role::Vertex(1), v2(3.0, 0.0));
    let p = t.m.mep_run(r).unwrap().points;
    assert_eq!(p[1], (3.0, 0.0, 1.0));
}

#[test]
fn eq_chain_kept_by_wall_grip_and_conflict_reported() {
    let mut t = base();
    let ws: Vec<i64> = [0.0, 3.0, 6.0]
        .iter()
        .map(|&x| wall(&mut t, (x, 0.0), (x, 5.0)))
        .collect();
    t.m.add_constraint(&ConstraintSpec {
        kind: ConstraintKind::Equal,
        walls: ws.clone(),
        value: None,
    })
    .unwrap();
    // drag the end wall's end down: it rotates, the chain stays equal and parallel
    drag(&mut t.m, Kind::Wall, ws[2], Role::End(0), v2(9.0, 0.0));
    drag(&mut t.m, Kind::Wall, ws[2], Role::End(1), v2(9.0, 5.0));
    let x: Vec<f64> = ws
        .iter()
        .map(|&w| wall_axis(&t.m, w).unwrap().0.x)
        .collect();
    assert!(((x[1] - x[0]) - (x[2] - x[1])).abs() < 1e-6, "{x:?}");
    // a length lock plus a grip stretch gives a conflict with clear text, the model is unchanged
    t.m.add_constraint(&ConstraintSpec {
        kind: ConstraintKind::LockLength,
        walls: vec![ws[0]],
        value: None,
    })
    .unwrap();
    let before = wall_axis(&t.m, ws[0]).unwrap();
    let g = find(&t.m, Kind::Wall, ws[0], Role::End(1));
    let e = apply(&mut t.m, &g, g.pos + v2(0.0, 2.0)).unwrap_err();
    assert!(e.to_string().contains("lock_len #"), "{e}");
    assert_eq!(wall_axis(&t.m, ws[0]).unwrap(), before);
}

#[test]
fn preview_matches_write() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
    let v = wall(&mut t, (0.0, 2.0), (3.0, 2.0));
    let g = find(&t.m, Kind::Wall, v, Role::End(1));
    let c = compute(&t.m, &g, v2(4.0, 2.0)).unwrap();
    assert_eq!(outline(&c), vec![vec![v2(0.0, 2.0), v2(4.0, 2.0)]]);
    let _ = w;
}

fn rejects(m: &mut Model, k: Kind, id: i64, role: Role, to: V2) -> Error {
    let g = find(m, k, id, role);
    assert!(compute(m, &g, to).is_err(), "preview rejects too");
    apply(m, &g, to).unwrap_err()
}

#[test]
fn degenerate_edits_rejected_without_write() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
    let e = rejects(&mut t.m, Kind::Wall, w, Role::End(1), v2(0.0, 0.0));
    assert!(matches!(e, Error::Degenerate(_)), "{e}");
    assert_eq!(wall_axis(&t.m, w).unwrap(), (v2(0.0, 0.0), v2(4.0, 0.0)));
    let g =
        t.m.add_grid(&GridSpec::line("A", (0.0, -1.0), (0.0, 9.0)))
            .unwrap();
    rejects(&mut t.m, Kind::Grid, g, Role::End(1), v2(0.0, -1.0));
    let s =
        t.m.add_slab(&SlabSpec {
            level: t.lv,
            slab_type: t.st,
            offset: 0.0,
            outer: SQ.to_vec(),
            holes: vec![],
        })
        .unwrap();
    // a bowtie and a zero-length edge
    let e = rejects(&mut t.m, Kind::Slab, s, Role::Vertex(2), v2(-1.0, 1.0));
    assert!(e.to_string().contains("self-intersects"), "{e}");
    let e = rejects(&mut t.m, Kind::Slab, s, Role::Vertex(2), v2(4.0, 0.0));
    assert!(e.to_string().contains("zero"), "{e}");
    assert_eq!(t.m.slab_rings(s).unwrap()[0], SQ.to_vec());
}

#[test]
fn railing_vertex_onto_neighbour_merges() {
    let mut t = base();
    let r =
        t.m.add_railing(&RailingSpec::new(
            t.lv,
            RailingHost::Path(vec![(0.0, 0.0, 0.0), (3.0, 0.0, 0.0), (3.0, 3.0, 0.0)]),
        ))
        .unwrap();
    drag(&mut t.m, Kind::Railing, r, Role::Vertex(2), v2(3.0, 0.0));
    assert_eq!(
        t.m.railing_path(r).unwrap(),
        vec![(0.0, 0.0, 0.0), (3.0, 0.0, 0.0)]
    );
}

#[test]
fn flip_keeps_openings_in_place_and_lock_dist_holds() {
    let mut t = base();
    let a = wall(&mut t, (0.0, 0.0), (10.0, 0.0));
    let c = wall(&mut t, (0.0, 3.0), (10.0, 3.0));
    t.m.add_constraint(&ConstraintSpec {
        kind: ConstraintKind::LockDist,
        walls: vec![a, c],
        value: None,
    })
    .unwrap();
    let o =
        t.m.add_opening(
            a,
            &ddc_model::OpeningSpec {
                along: 1.0,
                width: 0.9,
                sill: 0.0,
                height: 2.1,
                kind: "door".into(),
            },
        )
        .unwrap();
    drag(&mut t.m, Kind::Wall, a, Role::Flip, v2(0.0, 0.0));
    assert_eq!(wall_axis(&t.m, a).unwrap(), (v2(10.0, 0.0), v2(0.0, 0.0)));
    let along: f64 =
        t.m.sql()
            .query_row("SELECT along FROM opening WHERE id=?1", [o], |r| r.get(0))
            .unwrap();
    assert!((along - 8.1).abs() < 1e-12, "{along}");
    assert_eq!(wall_axis(&t.m, c).unwrap(), (v2(0.0, 3.0), v2(10.0, 3.0)));
    t.m.undo().unwrap();
    assert_eq!(wall_axis(&t.m, a).unwrap(), (v2(0.0, 0.0), v2(10.0, 0.0)));
}

fn find_in(m: &Model, k: Kind, id: i64, role: Role, view: &str) -> Grip {
    grips_in(m, k, id, view)
        .unwrap()
        .into_iter()
        .find(|g| g.role == role)
        .unwrap_or_else(|| panic!("{k:?} {role:?} in {view}"))
}

fn drag_in(m: &mut Model, id: i64, role: Role, to: V2, view: &str) {
    let g = find_in(m, Kind::Grid, id, role, view);
    apply(m, &g, to).unwrap();
}

fn near2(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
}

fn beam_spec(t: &M, mt: i64) -> BeamSpec {
    BeamSpec {
        level: t.top,
        member_type: mt,
        a: (0.0, 0.0, 0.0),
        b: (4.0, 0.0, 0.0),
        bulge: 0.0,
        yjust: Just::Center,
        zjust: Just::Center,
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::default(),
        join_end: BeamJoin::default(),
    }
}

#[test]
fn arc_wall_bulge_grip_through_point_and_back_to_straight() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
    assert!(grips(&t.m, Kind::Wall, w)
        .unwrap()
        .iter()
        .all(|g| g.role != Role::Bulge));
    t.m.set_wall_bulge(w, -0.25).unwrap();
    // point (2, -1) right of the chord: a counterclockwise arc, bulge = sagitta / half-chord = 0.5
    drag(&mut t.m, Kind::Wall, w, Role::Bulge, v2(2.0, -1.0));
    assert!((t.m.wall_bulge(w).unwrap() - 0.5).abs() < 1e-12);
    let g = find(&t.m, Kind::Wall, w, Role::Bulge);
    assert!(
        (g.pos - v2(2.0, -1.0)).len() < 1e-9,
        "grip on the arc midpoint"
    );
    assert_eq!(wall_axis(&t.m, w).unwrap(), (v2(0.0, 0.0), v2(4.0, 0.0)));
    drag(&mut t.m, Kind::Wall, w, Role::Bulge, v2(2.0, 0.0));
    assert_eq!(t.m.wall_bulge(w).unwrap(), 0.0);
    t.m.undo().unwrap();
    assert!((t.m.wall_bulge(w).unwrap() - 0.5).abs() < 1e-12);
}

#[test]
fn beam_rotate_about_middle_and_end_elevations() {
    let mut t = base();
    let mt = t.m.add_catalog_member_type("R 400x400").unwrap();
    let b = t.m.add_beam(&beam_spec(&t, mt)).unwrap();
    let r = find(&t.m, Kind::Beam, b, Role::Rotate);
    assert!((r.pos - v2(2.0, ARM)).len() < 1e-12);
    // the rotation grip goes left of the midpoint: +90 degrees
    drag(&mut t.m, Kind::Beam, b, Role::Rotate, v2(1.0, 0.0));
    let s = t.m.beam(b).unwrap();
    assert!(
        (s.a.0 - 2.0).abs() < 1e-12 && (s.a.1 + 2.0).abs() < 1e-12,
        "{s:?}"
    );
    assert!(
        (s.b.0 - 2.0).abs() < 1e-12 && (s.b.1 - 2.0).abs() < 1e-12,
        "{s:?}"
    );
    // the axis now points up: normal (-1, 0); the z grip of end 1 is left of the end by ZOFF
    let z = find(&t.m, Kind::Beam, b, Role::Z(1));
    assert!((z.pos - v2(2.0 - ZOFF, 2.0)).len() < 1e-12);
    apply(&mut t.m, &z, z.pos + v2(-0.3, 0.05)).unwrap();
    let s = t.m.beam(b).unwrap();
    assert!((s.b.2 - 0.3).abs() < 1e-12 && s.a.2 == 0.0, "{s:?}");
    let z = find(&t.m, Kind::Beam, b, Role::Z(1));
    let d = dims(&t.m, &z, z.pos).unwrap();
    assert!((d[0].value - 0.3).abs() < 1e-12, "primary dim is z");
    apply_value(&mut t.m, &z, &d[0], 1.25).unwrap();
    assert!((t.m.beam(b).unwrap().b.2 - 1.25).abs() < 1e-12);
    t.m.undo().unwrap();
    assert!((t.m.beam(b).unwrap().b.2 - 0.3).abs() < 1e-12);
}

#[test]
fn stair_width_edge_and_landing_handles() {
    let mut t = base();
    let s =
        t.m.add_stair(&StairSpec::straight(t.lv, t.top, (0.0, 0.0), 0.0))
            .unwrap();
    // straight: no landing grip; the right edge (-y) moves by 0.3, the left edge stays
    assert!(grips(&t.m, Kind::Stair, s)
        .unwrap()
        .iter()
        .all(|g| g.role != Role::Landing));
    let g = find(&t.m, Kind::Stair, s, Role::Width);
    apply(&mut t.m, &g, g.pos + v2(0.2, -0.3)).unwrap();
    let sp = t.m.stair(s).unwrap();
    assert!(
        (sp.width - 1.5).abs() < 1e-12 && near2(sp.origin, (0.0, -0.15)),
        "{sp:?}"
    );
    let g = find(&t.m, Kind::Stair, s, Role::Width);
    let d = dims(&t.m, &g, g.pos).unwrap();
    assert!((d[0].value - 1.5).abs() < 1e-12, "primary dim is width");
    apply_value(&mut t.m, &g, &d[0], 1.0).unwrap();
    let sp = t.m.stair(s).unwrap();
    assert!((sp.width - 1.0).abs() < 1e-12);
    assert!(
        (sp.origin.1 + sp.width / 2.0 - 0.6).abs() < 1e-12,
        "left edge fixed"
    );
    t.m.undo().unwrap();
    assert!((t.m.stair(s).unwrap().width - 1.5).abs() < 1e-12);
    // L: the landing after 4 risers
    let mut sp = t.m.stair(s).unwrap();
    sp.shape = StairShape::L {
        left: false,
        first: None,
    };
    t.m.update_stair(s, &sp).unwrap();
    let tr = t.m.stair_layout(s).unwrap().tread;
    let o = v2(sp.origin.0, sp.origin.1);
    drag(
        &mut t.m,
        Kind::Stair,
        s,
        Role::Landing,
        o + v2(tr * 3.0 + sp.width / 2.0 + 0.04, 0.3),
    );
    assert_eq!(
        t.m.stair(s).unwrap().shape,
        StairShape::L {
            left: false,
            first: Some(4)
        }
    );
    assert_eq!(t.m.stair_layout(s).unwrap().flights[0].risers, 4);
    // U: landing depth is up to the far edge
    let mut sp = t.m.stair(s).unwrap();
    sp.shape = StairShape::U {
        left: false,
        first: Some(6),
        gap: 0.1,
    };
    t.m.update_stair(s, &sp).unwrap();
    let l1 = tr * 5.0;
    drag(
        &mut t.m,
        Kind::Stair,
        s,
        Role::LandingDepth,
        o + v2(l1 + 1.6, 0.0),
    );
    assert!((t.m.stair(s).unwrap().landing - 1.6).abs() < 1e-12);
    let e = rejects(
        &mut t.m,
        Kind::Stair,
        s,
        Role::LandingDepth,
        o + v2(l1 + 0.1, 0.0),
    );
    assert!(e.to_string().contains("landing"), "{e}");
}

#[test]
fn stair_railing_grip_moves_its_stair_edge() {
    let mut t = base();
    let s =
        t.m.add_stair(&StairSpec::straight(t.lv, t.top, (0.0, 0.0), 0.0))
            .unwrap();
    let rs =
        t.m.add_stair_railings(s, &RailingSpec::new(t.lv, RailingHost::Path(vec![])))
            .unwrap();
    for (k, &r) in rs.iter().enumerate() {
        let g = find(&t.m, Kind::Railing, r, Role::Width);
        let side = if g.pos.y > 0.0 { 1.0 } else { -1.0 };
        let sp0 = t.m.stair(s).unwrap();
        // outward by 0.2: width +0.2, the opposite edge stays
        apply(&mut t.m, &g, g.pos + v2(0.1, side * 0.2)).unwrap();
        let sp = t.m.stair(s).unwrap();
        assert!((sp.width - sp0.width - 0.2).abs() < 1e-12, "railing {k}");
        let far = |x: &StairSpec| x.origin.1 - side * x.width / 2.0;
        assert!((far(&sp) - far(&sp0)).abs() < 1e-12, "railing {k}");
        let moved = find(&t.m, Kind::Railing, r, Role::Width);
        assert!(
            (moved.pos.y - g.pos.y - side * 0.2).abs() < 1e-9,
            "railing follows"
        );
    }
    t.m.undo().unwrap();
    assert!(
        (t.m.stair(s).unwrap().width - 1.4).abs() < 1e-12,
        "one undo per grip"
    );
}

#[test]
fn hosted_component_flips_hand_and_facing() {
    let mut t = base();
    let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
    let f = t.m.load_library_family("Washbasin").unwrap();
    let ct = t.m.comp_type_id(f, "500").unwrap().unwrap();
    let mut cs = CompSpec::at(t.lv, ct, 1.0, 0.1);
    cs.host = CompHost::Wall(w);
    let c = t.m.add_component(&cs).unwrap();
    // a wall family cannot stand free: the unhosted one is a table
    let tf = t.m.load_library_family("Table").unwrap();
    let tt = t.m.comp_type_id(tf, "1600x800").unwrap().unwrap();
    let free =
        t.m.add_component(&CompSpec::at(t.lv, tt, 5.0, 5.0))
            .unwrap();
    assert!(grips(&t.m, Kind::Comp, free)
        .unwrap()
        .iter()
        .all(|g| !matches!(g.role, Role::FlipHand | Role::FlipFacing)));
    let g = find(&t.m, Kind::Comp, c, Role::FlipHand);
    assert!(is_click(&g.role));
    apply(&mut t.m, &g, g.pos).unwrap();
    let s = t.m.component(c).unwrap();
    assert!(s.mirror && s.rotation == 0.0 && s.at == cs.at);
    let g = find(&t.m, Kind::Comp, c, Role::FlipFacing);
    apply(&mut t.m, &g, g.pos).unwrap();
    let s = t.m.component(c).unwrap();
    assert!(!s.mirror && (s.rotation.abs() - std::f64::consts::PI).abs() < 1e-12);
    assert_eq!(s.at, cs.at, "stays at its place on the wall");
    t.m.undo().unwrap();
    t.m.undo().unwrap();
    let s = t.m.component(c).unwrap();
    assert!(!s.mirror && s.rotation == 0.0);
}

#[test]
fn mep_insert_on_segment_and_delete_vertex() {
    let mut t = base();
    let sys = t.m.add_mep_system("CW", SystemKind::ColdWater).unwrap();
    let pts = vec![(0.0, 0.0, 1.0), (2.0, 0.0, 1.4), (2.0, 2.0, 1.4)];
    let r =
        t.m.add_mep_run(&RunSpec::pipe(sys, t.lv, PipeMaterial::Copper, 15, pts.clone()).unwrap())
            .unwrap();
    let pts = t.m.mep_run(r).unwrap().points;
    let g = find(&t.m, Kind::Mep, r, Role::Insert(0));
    assert!((g.pos - v2(1.0, 0.0)).len() < 1e-12);
    apply(&mut t.m, &g, v2(1.0, -0.5)).unwrap();
    let p = t.m.mep_run(r).unwrap().points;
    assert_eq!(p.len(), 4);
    assert!(near2((p[1].0, p[1].1), (1.0, -0.5)), "{p:?}");
    assert!(
        (p[1].2 - (pts[0].2 + pts[1].2) / 2.0).abs() < 1e-12,
        "{p:?}"
    );
    delete_vertex(&mut t.m, Kind::Mep, r, 1).unwrap();
    assert_eq!(t.m.mep_run(r).unwrap().points, pts);
    t.m.undo().unwrap();
    assert_eq!(t.m.mep_run(r).unwrap().points.len(), 4);
    delete_vertex(&mut t.m, Kind::Mep, r, 1).unwrap();
    delete_vertex(&mut t.m, Kind::Mep, r, 2).unwrap();
    let e = delete_vertex(&mut t.m, Kind::Mep, r, 0).unwrap_err();
    assert!(matches!(e, Error::Degenerate(_)), "{e}");
    assert_eq!(t.m.mep_run(r).unwrap().points.len(), 2);
}

#[test]
fn slab_hole_vertex_and_edge_grips() {
    // the refusal text is checked in English
    crate::lang_on_this_thread(false);
    let mut t = base();
    // holes are stored clockwise; this one starts with the bottom edge
    let hole = vec![(2.0, 1.0), (1.0, 1.0), (1.0, 2.0), (2.0, 2.0)];
    let s =
        t.m.add_slab(&SlabSpec {
            level: t.lv,
            slab_type: t.st,
            offset: 0.0,
            outer: SQ.to_vec(),
            holes: vec![hole],
        })
        .unwrap();
    drag(
        &mut t.m,
        Kind::Slab,
        s,
        Role::HoleVertex(0, 3),
        v2(2.5, 2.2),
    );
    let r = t.m.slab_rings(s).unwrap();
    assert_eq!(r[0], SQ.to_vec(), "outer untouched");
    assert_eq!(r[1][3], (2.5, 2.2));
    let g = find(&t.m, Kind::Slab, s, Role::HoleEdge(0, 0));
    apply(&mut t.m, &g, g.pos + v2(0.4, -0.3)).unwrap();
    let r = &t.m.slab_rings(s).unwrap()[1];
    assert!(near2(r[0], (2.0, 0.7)) && near2(r[1], (1.0, 0.7)), "{r:?}");
    // a hole turned inside out is refused with nothing written
    let e = rejects(
        &mut t.m,
        Kind::Slab,
        s,
        Role::HoleVertex(0, 0),
        v2(2.5, 2.5),
    );
    // the hole check runs first and names its reason through the localized ring refusal
    let Error::Degenerate(m) = &e else { panic!("{e}") };
    assert!(is_ring_refusal(m), "{e}");
}

#[test]
fn grid_bubbles_and_per_view_2d_extents() {
    let mut t = base();
    let g =
        t.m.add_grid(&GridSpec::line("A", (0.0, -1.0), (0.0, 9.0)))
            .unwrap();
    // model: the end mark is switched off by a click, one undo
    let b = find(&t.m, Kind::Grid, g, Role::Bubble(1));
    apply(&mut t.m, &b, b.pos).unwrap();
    let s = t.m.grids().unwrap().remove(0).spec;
    assert!(s.bubble_start && !s.bubble_end);
    t.m.undo().unwrap();
    assert!(t.m.grids().unwrap()[0].spec.bubble_end);
    // the end is dragged sideways: it slides along the axis line, the direction is unchanged
    drag(&mut t.m, Kind::Grid, g, Role::End(1), v2(0.4, 7.0));
    let s = t.m.grids().unwrap().remove(0).spec;
    assert!(near2(s.a, (0.0, -1.0)) && near2(s.b, (0.0, 7.0)), "{s:?}");
    t.m.undo().unwrap();
    // view: 2D bounds, the model is unchanged
    let v = format!("plan:{}", t.lv);
    assert!(grips(&t.m, Kind::Grid, g)
        .unwrap()
        .iter()
        .all(|x| !matches!(x.role, Role::Mode(_))));
    let m0 = find_in(&t.m, Kind::Grid, g, Role::Mode(0), &v);
    apply(&mut t.m, &m0, m0.pos).unwrap();
    drag_in(&mut t.m, g, Role::End(1), v2(0.0, 6.0), &v);
    let b0 = find_in(&t.m, Kind::Grid, g, Role::Bubble(0), &v);
    apply(&mut t.m, &b0, b0.pos).unwrap();
    let e = t.m.datum_extent(&v, DatumKind::Grid, g).unwrap().unwrap();
    assert_eq!(
        (e.a, e.b, e.bubbles),
        ((0.0, -1.0), (0.0, 6.0), (false, true))
    );
    let s = t.m.grids().unwrap().remove(0).spec;
    assert_eq!((s.b, s.bubble_start), ((0.0, 9.0), true), "model untouched");
    let other = format!("plan:{}", t.top);
    assert_eq!(
        find_in(&t.m, Kind::Grid, g, Role::End(1), &other).pos,
        v2(0.0, 9.0)
    );
    // back to 3D: the view record is removed; undo restores it
    let m1 = find_in(&t.m, Kind::Grid, g, Role::Mode(1), &v);
    apply(&mut t.m, &m1, m1.pos).unwrap();
    assert_eq!(t.m.datum_extent(&v, DatumKind::Grid, g).unwrap(), None);
    assert_eq!(
        find_in(&t.m, Kind::Grid, g, Role::End(1), &v).pos,
        v2(0.0, 9.0)
    );
    t.m.undo().unwrap();
    assert_eq!(t.m.datum_extent(&v, DatumKind::Grid, g).unwrap(), Some(e));
}

#[test]
fn level_extents_3d_and_2d_and_wall_top_follows_level_grip() {
    let mut t = base();
    let v = "section:1";
    let g = find_in(&t.m, Kind::Level, t.top, Role::End(1), v);
    assert_eq!(g.pos, v2(ddc_model::LEVEL_DEFAULT.1, 3.0));
    apply(&mut t.m, &g, v2(12.0, 3.7)).unwrap();
    let e =
        t.m.datum_extent(MODEL_VIEW, DatumKind::Level, t.top)
            .unwrap()
            .unwrap();
    assert_eq!((e.a.0, e.b.0), (0.0, 12.0), "3D extent");
    assert_eq!(level(&t.m, t.top).unwrap().1, 3.0, "elevation unchanged");
    let m0 = find_in(&t.m, Kind::Level, t.top, Role::Mode(0), v);
    apply(&mut t.m, &m0, m0.pos).unwrap();
    let g = find_in(&t.m, Kind::Level, t.top, Role::End(0), v);
    apply(&mut t.m, &g, v2(-3.0, 3.0)).unwrap();
    let own =
        t.m.datum_extent(v, DatumKind::Level, t.top)
            .unwrap()
            .unwrap();
    assert_eq!((own.a.0, own.b.0), (-3.0, 12.0));
    let model =
        t.m.datum_extent(MODEL_VIEW, DatumKind::Level, t.top)
            .unwrap()
            .unwrap();
    assert_eq!(model.a.0, 0.0, "3D extent untouched by 2D edit");
    // the wall top is attached to a level: the level elevation grip drives the wall height, one undo
    let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
    t.m.add_constraint(&ConstraintSpec {
        kind: ConstraintKind::TopToLevel,
        walls: vec![w, t.top],
        value: None,
    })
    .unwrap();
    let h = |m: &Model| -> f64 {
        m.sql()
            .query_row("SELECT height FROM wall WHERE id=?1", [w], |r| r.get(0))
            .unwrap()
    };
    drag(&mut t.m, Kind::Level, t.top, Role::Elevation, v2(0.0, 3.3));
    assert_eq!((level(&t.m, t.top).unwrap().1, h(&t.m)), (3.3, 3.3));
    t.m.undo().unwrap();
    assert_eq!((level(&t.m, t.top).unwrap().1, h(&t.m)), (3.0, 3.0));
}

#[test]
fn barrel_end_grips_stretch_profile_and_keep_arcs() {
    let mut t = base();
    let r =
        t.m.add_barrel_roof(t.lv, t.st, 3.0, (0.0, 0.0), (6.0, 0.0), 1.5, 4.0)
            .unwrap();
    let arcs = t.m.roof_ext(r).unwrap().unwrap().0;
    drag(&mut t.m, Kind::Roof, r, Role::End(1), v2(9.0, 0.0));
    let RoofKind::Extrusion { b, profile, .. } = t.m.roof(r).unwrap().kind else {
        panic!()
    };
    assert_eq!(b, (9.0, 0.0));
    assert_eq!(profile.len(), 2, "no profile points added");
    assert!(
        (profile[1].0 - 9.0).abs() < 1e-12,
        "profile follows the span: {profile:?}"
    );
    assert_eq!(
        t.m.roof_ext(r).unwrap().unwrap().0,
        arcs,
        "arc stays on its segment"
    );
    drag(&mut t.m, Kind::Roof, r, Role::End(0), v2(3.0, 0.0));
    let RoofKind::Extrusion { profile, .. } = t.m.roof(r).unwrap().kind else {
        panic!()
    };
    assert!((profile[1].0 - 6.0).abs() < 1e-12);
    // a vault over the whole new span: the crown at the middle rises bulge·span/2 above the eaves
    let g = t.m.roof_geometry(r).unwrap();
    let mid = g.top_at(v2(6.0, 1.0)).unwrap();
    let edge = g.top_at(v2(3.05, 1.0)).unwrap();
    assert!(mid - edge > 1.2, "vault spans 3..9: {mid} {edge}");
}

#[test]
fn imported_mesh_moves_by_its_centre_grip_in_one_undo_step() {
    let mut t = base();
    let pts: [[f64; 3]; 4] = [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 4.0, 0.0], [0.0, 4.0, 1.0]];
    let pb: Vec<u8> = pts.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
    let ib: Vec<u8> = [0u32, 1, 2, 0, 2, 3]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let lv = t.lv;
    let id =
        t.m.batch_walls("wall_grips_test_mesh", |db| {
            db.execute(
                "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,'Furniture','Box',?2,?3)",
                (lv, pb, ib),
            )?;
            Ok(db.last_insert_rowid())
        })
        .unwrap();
    let g = find(&t.m, Kind::Mesh, id, Role::Move);
    assert_eq!(g.pos, v2(1.0, 2.0));
    let x0 = |m: &Model| m.imported_mesh(id).unwrap().mesh.positions[0];
    let before = x0(&t.m);
    apply(&mut t.m, &g, v2(4.0, 2.5)).unwrap();
    let after = x0(&t.m);
    assert!((after.x - before.x - 3.0).abs() < 1e-9 && (after.y - before.y - 0.5).abs() < 1e-9);
    assert_eq!(after.z, before.z, "plan drag keeps the height");
    t.m.undo().unwrap();
    assert_eq!(x0(&t.m), before);
    assert!(grips(&t.m, Kind::Mesh, id + 99).is_err());
}

#[test]
fn shaft_vertex_and_edge_grips_one_undo_each() {
    let mut t = base();
    let s = t
        .m
        .add_shaft(&ShaftSpec {
            level: t.lv,
            top_level: t.top,
            base_offset: 0.0,
            top_offset: 0.0,
            ring: SQ.to_vec(),
        })
        .unwrap();
    // four vertices, four edges and the move grip
    assert_eq!(grips(&t.m, Kind::Shaft, s).unwrap().len(), 9);
    drag(&mut t.m, Kind::Shaft, s, Role::Vertex(1), v2(6.0, 0.0));
    assert_eq!(t.m.shaft_ring(s).unwrap()[1], (6.0, 0.0));
    t.m.undo().unwrap();
    assert_eq!(t.m.shaft_ring(s).unwrap(), SQ.to_vec());
    // the right edge slides along its normal
    drag(&mut t.m, Kind::Shaft, s, Role::Edge(1), v2(5.0, 1.5));
    let r = t.m.shaft_ring(s).unwrap();
    assert!((r[1].0 - 5.0).abs() < 1e-9 && (r[2].0 - 5.0).abs() < 1e-9, "{r:?}");
    t.m.undo().unwrap();
    assert_eq!(t.m.shaft_ring(s).unwrap(), SQ.to_vec());
    // a bow-tie is refused and the model is untouched
    let g = find(&t.m, Kind::Shaft, s, Role::Vertex(1));
    assert!(apply(&mut t.m, &g, v2(4.0, 5.0)).is_err());
    assert_eq!(t.m.shaft_ring(s).unwrap(), SQ.to_vec());
}
