// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! QA: Model methods on a small building (undo round trips, error paths).

use crate::{LayerSpec, Model, RoomSpec, ShaftSpec, SlabSpec, WallSpec, ZoneSpec};

pub struct Fx {
    pub m: Model,
    pub l1: i64,
    pub l2: i64,
    pub wt: i64,
    pub st: i64,
    pub walls: Vec<i64>,
}

fn lay(t: f64) -> LayerSpec {
    LayerSpec { material: "Concrete".into(), thickness: t }
}

pub fn fx() -> Fx {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let wt = m.add_wall_type("W", &[lay(0.2)]).unwrap();
    let st = m.add_slab_type("S", &[lay(0.2)]).unwrap();
    let pts = [(0.0, 0.0), (6.0, 0.0), (6.0, 5.0), (0.0, 5.0)];
    let mut walls = vec![];
    for i in 0..4 {
        let w = WallSpec {
            a: pts[i],
            b: pts[(i + 1) % 4],
            level: l1,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: Some(0.0),
        };
        walls.push(m.add_wall(&w).unwrap());
    }
    Fx { m, l1, l2, wt, st, walls }
}

pub fn rect(x: f64, y: f64, w: f64, h: f64) -> Vec<(f64, f64)> {
    vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
}

pub fn slab(f: &mut Fx) -> i64 {
    let s = SlabSpec {
        level: f.l1,
        slab_type: f.st,
        offset: 0.0,
        outer: rect(0.0, 0.0, 6.0, 5.0),
        holes: vec![],
    };
    f.m.add_slab(&s).unwrap()
}

#[test]
fn options_rename_primary_remove() {
    let mut f = fx();
    let (set, a) = f.m.add_option_set("Set", "A").unwrap();
    let b = f.m.add_option(set, "B").unwrap();
    f.m.rename_option(b, "B2").unwrap();
    f.m.rename_option_set(set, "Set2").unwrap();
    let sets = f.m.option_sets().unwrap();
    assert_eq!(sets[0].name, "Set2");
    assert!(sets[0].options.iter().any(|o| o.name == "B2"));
    f.m.set_primary_option(b).unwrap();
    let prim = |m: &Model| {
        m.option_sets().unwrap()[0].options.iter().find(|o| o.is_primary).map(|o| o.id)
    };
    assert_eq!(prim(&f.m), Some(b));
    f.m.undo().unwrap();
    assert_eq!(prim(&f.m), Some(a), "undo restores the previous primary");
    f.m.undo().unwrap();
    f.m.undo().unwrap();
    assert_eq!(f.m.option_sets().unwrap()[0].name, "Set");
    let w = f.walls[0];
    let n = f.m.add_to_option(b, &[("wall".to_string(), w)]).unwrap();
    assert!(n >= 1);
    let k = f.m.remove_from_option(&[("wall".to_string(), w)]).unwrap();
    assert_eq!(k, n);
    assert_eq!(f.m.remove_from_option(&[]).unwrap(), 0);
}

#[test]
fn rooms_shape_outline_zone() {
    let mut f = fx();
    let r = RoomSpec {
        level: f.l1,
        name: "R".into(),
        number: "1".into(),
        point: (3.0, 2.5),
        height: 2.7,
    };
    let id = f.m.add_room(&r).unwrap();
    assert!(f.m.room_outline(id).unwrap().is_none(), "bounded room has no sketch");
    f.m.set_room_shape(id, (2.0, 2.0), None).unwrap();
    f.m.undo().unwrap();
    assert!(f.m.set_room_shape(id, (f64::NAN, 0.0), None).is_err());
    assert!(f.m.set_room_shape(id, (1.0, 1.0), Some(&[(0.0, 0.0), (1.0, 0.0)])).is_err());
    assert!(f.m.set_room_shape(9999, (1.0, 1.0), None).is_err());
    assert!(f.m.wall_regions(f.l1).is_ok());
    let z = f.m.add_zone(&ZoneSpec { name: "Z".into(), rooms: vec![id] }).unwrap();
    assert!(f.m.zone_area(z).unwrap() >= 0.0);
    assert_eq!(f.m.zone_area(424242).unwrap(), 0.0);
}

#[test]
fn ref_plane_move_and_undo() {
    let mut f = fx();
    let id = f.m.add_ref_plane("RP", (0.0, 0.0), (5.0, 0.0)).unwrap();
    f.m.set_ref_plane(id, (0.0, 1.0), (5.0, 1.0)).unwrap();
    assert!(f.m.set_ref_plane(id, (1.0, 1.0), (1.0, 1.0)).is_err(), "degenerate");
    assert!(f.m.set_ref_plane(id, (f64::NAN, 0.0), (1.0, 1.0)).is_err());
    assert!(f.m.set_ref_plane(9999, (0.0, 0.0), (1.0, 0.0)).is_err());
    f.m.undo().unwrap();
    let ay: f64 = f
        .m
        .sql()
        .query_row("SELECT ay FROM ref_plane WHERE id=?1", [id], |r| r.get(0))
        .unwrap();
    assert_eq!(ay, 0.0);
}

#[test]
fn wall_type_queries_and_edit() {
    let mut f = fx();
    assert_eq!(f.m.wall_layer_materials(f.wt).unwrap(), vec!["Concrete".to_string()]);
    let layers = [lay(0.1), LayerSpec { material: "Brick".into(), thickness: 0.2 }];
    f.m.set_wall_type_layers(f.wt, "W2", &layers).unwrap();
    assert_eq!(
        f.m.wall_layer_materials(f.wt).unwrap(),
        vec!["Concrete".to_string(), "Brick".to_string()]
    );
    f.m.undo().unwrap();
    assert_eq!(f.m.wall_layer_materials(f.wt).unwrap().len(), 1);
    assert!(f.m.set_wall_type_layers(f.wt, "W", &[]).is_err());
    assert!(f.m.set_wall_type_layers(f.wt, "W", &[lay(-1.0)]).is_err());
    assert!(f.m.set_wall_type_layers(9999, "W", &[lay(0.1)]).is_err());
    assert!(f.m.wall_layer_materials(9999).unwrap().is_empty());
    assert!(f.m.stack_types().unwrap().iter().all(|t| !t.1.is_empty()));
    let jp = f.m.join_prefs(&f.walls).unwrap();
    assert_eq!(jp.len(), f.walls.len());
    assert!(f.m.join_prefs(&[]).unwrap().is_empty());
}

#[test]
fn element_type_assignment() {
    let mut f = fx();
    let s = slab(&mut f);
    let st2 = f.m.add_slab_type("S2", &[lay(0.3)]).unwrap();
    f.m.set_elements_type("slab", &[s], st2).unwrap();
    let ty = |m: &Model| -> i64 {
        m.sql().query_row("SELECT stype FROM slab WHERE id=?1", [s], |r| r.get(0)).unwrap()
    };
    assert_eq!(ty(&f.m), st2);
    f.m.undo().unwrap();
    assert_eq!(ty(&f.m), f.st);
    assert!(f.m.set_elements_type("slab", &[s], 9999).is_err());
    assert!(f.m.set_elements_type("wall", &[s], st2).is_err());
    f.m.set_elements_type("slab", &[], st2).unwrap();
}

#[test]
fn slab_opening_removal() {
    let mut f = fx();
    let s = SlabSpec {
        level: f.l1,
        slab_type: f.st,
        offset: 0.0,
        outer: rect(0.0, 0.0, 6.0, 5.0),
        holes: vec![rect(1.0, 1.0, 1.0, 1.0), rect(3.0, 1.0, 1.0, 1.0)],
    };
    let id = f.m.add_slab(&s).unwrap();
    f.m.remove_slab_opening(id, 0).unwrap();
    assert_eq!(crate::slabs::rings(f.m.sql(), id).unwrap().len(), 2);
    assert!(f.m.remove_slab_opening(id, 5).is_err());
    assert!(f.m.remove_slab_opening(9999, 0).is_err());
    f.m.undo().unwrap();
    assert_eq!(crate::slabs::rings(f.m.sql(), id).unwrap().len(), 3);
}

#[test]
fn shaft_adds() {
    let mut f = fx();
    let sh = ShaftSpec {
        level: f.l1,
        top_level: f.l2,
        base_offset: 0.0,
        top_offset: 0.0,
        ring: rect(1.0, 1.0, 1.0, 1.0),
    };
    assert!(f.m.add_shaft(&sh).is_ok());
}

// ---- terrain, pads, stairs, railings, ramps ----
use crate::{PadSpec, RailingHost, RailingSpec, RampSpec, StairSpec, TopoSpec};

fn topo_spec() -> TopoSpec {
    TopoSpec {
        name: "T".into(),
        thickness: 1.0,
        material: "Soil".into(),
        points: vec![
            (0.0, 0.0, 0.0),
            (10.0, 0.0, 0.5),
            (10.0, 10.0, 1.0),
            (0.0, 10.0, 0.5),
            (5.0, 5.0, 0.3),
        ],
        breaklines: vec![],
        boundary: vec![],
    }
}

#[test]
fn terrain_edit_methods() {
    let mut f = fx();
    let t = f.m.add_toposolid(&topo_spec()).unwrap();
    f.m.set_topo_props(t, "T2", 2.0, "Clay").unwrap();
    assert!(f.m.set_topo_props(t, "T", 0.0, "x").is_err());
    assert!(f.m.set_topo_props(t, "T", f64::NAN, "x").is_err());
    assert!(f.m.set_topo_props(9999, "T", 1.0, "x").is_err());
    f.m.undo().unwrap();
    let th: f64 =
        f.m.sql().query_row("SELECT thickness FROM toposolid WHERE id=?1", [t], |r| r.get(0)).unwrap();
    assert_eq!(th, 1.0);
    let mut s2 = topo_spec();
    s2.name = "T3".into();
    f.m.update_toposolid(t, &s2).unwrap();
    s2.thickness = -1.0;
    assert!(f.m.update_toposolid(t, &s2).is_err());
    let pts = f.m.topo_points(t).unwrap();
    assert_eq!(pts.len(), 5);
    f.m.move_topo_points(t, &[(pts[0].0, (0.0, 0.0, 2.0))]).unwrap();
    f.m.move_topo_points(t, &[]).unwrap();
    assert!(f.m.move_topo_points(t, &[(pts[0].0, (f64::NAN, 0.0, 0.0))]).is_err());
    assert_eq!(f.m.topo_points(t).unwrap()[0].1 .2, 2.0);
    f.m.undo().unwrap();
    assert_eq!(f.m.topo_points(t).unwrap()[0].1 .2, 0.0);
    f.m.set_topo_boundary(t, &rect(0.0, 0.0, 10.0, 10.0)).unwrap();
    f.m.set_topo_boundary(t, &[]).unwrap();
    assert!(f.m.set_topo_boundary(t, &[(0.0, 0.0), (1.0, 1.0)]).is_err());
    let pieces = f.m.topo_pieces_shared(t).unwrap();
    assert_eq!(pieces.len(), f.m.topo_pieces(t).unwrap().len());
    assert!(f.m.delete_topo_region(9999).is_err());
}

#[test]
fn pad_update_delete() {
    let mut f = fx();
    let t = f.m.add_toposolid(&topo_spec()).unwrap();
    let mut p = PadSpec {
        topo: t,
        name: "P".into(),
        elevation: 0.5,
        slope: 0.0,
        thickness: 0.3,
        material: "Concrete".into(),
        outline: rect(2.0, 2.0, 3.0, 3.0),
    };
    let id = f.m.add_pad(&p).unwrap();
    p.elevation = 1.5;
    f.m.update_pad(id, &p).unwrap();
    p.slope = -1.0;
    assert!(f.m.update_pad(id, &p).is_err());
    p.slope = 0.0;
    assert!(f.m.update_pad(9999, &p).is_err());
    f.m.delete_pad(id).unwrap();
    assert!(f.m.delete_pad(id).is_err());
    f.m.undo().unwrap();
    f.m.delete_pad(id).unwrap();
}

fn stair_spec(f: &Fx) -> StairSpec {
    StairSpec {
        level: f.l1,
        top_level: f.l2,
        base_offset: 0.0,
        top_offset: 0.0,
        shape: ddc_geom::stair::Shape::Straight,
        origin: (1.0, 1.0),
        angle: 0.0,
        width: 1.0,
        max_riser: 0.18,
        tread: None,
        landing: 1.0,
        thickness: 0.15,
        stringer: None,
        material: "Concrete".into(),
    }
}

#[test]
fn stair_and_railing_queries() {
    let mut f = fx();
    let sid = f.m.add_stair(&stair_spec(&f)).unwrap();
    let lay = f.m.stair_layout(sid).unwrap();
    assert!(lay.risers >= 15, "3 m at 0.18 needs 17 risers, got {}", lay.risers);
    assert!(f.m.stair_layout(9999).is_err());
    let spec = RailingSpec {
        level: f.l1,
        host: RailingHost::Stair { stair: sid, side: 0 },
        height: 1.0,
        post_spacing: 1.2,
        baluster_spacing: 0.12,
        post: 0.05,
        baluster: 0.02,
        rail: (0.05, 0.04),
        material: "Steel".into(),
    };
    let rid = f.m.add_railing(&spec).unwrap();
    assert!(!f.m.railing_layout(rid).unwrap().posts.is_empty());
    assert!(!f.m.railing_meshes(rid).unwrap().is_empty());
    let free = RailingSpec {
        host: RailingHost::Path(vec![(0.0, 0.0, 0.0), (3.0, 0.0, 0.0)]),
        ..spec.clone()
    };
    let fid = f.m.add_railing(&free).unwrap();
    assert_eq!(f.m.railing_path(fid).unwrap().len(), 2);
    f.m.set_railing_path(fid, &[(0.0, 0.0, 0.0), (3.0, 0.0, 0.0), (3.0, 2.0, 0.0)]).unwrap();
    assert_eq!(f.m.railing_path(fid).unwrap().len(), 3);
    assert!(f.m.set_railing_path(fid, &[(0.0, 0.0, 0.0)]).is_err());
    assert!(f.m.set_railing_path(fid, &[(f64::NAN, 0.0, 0.0), (1.0, 0.0, 0.0)]).is_err());
    f.m.undo().unwrap();
    assert_eq!(f.m.railing_path(fid).unwrap().len(), 2);
    let mut up = free.clone();
    up.height = 1.1;
    f.m.update_railing(fid, &up).unwrap();
    up.height = -1.0;
    assert!(f.m.update_railing(fid, &up).is_err());
    assert!(f.m.railing_layout(9999).is_err());
    assert!(f.m.railing_path(9999).unwrap().is_empty());
}

#[test]
fn ramp_queries() {
    let mut f = fx();
    let s = RampSpec {
        level: f.l1,
        top_level: None,
        base_offset: 0.0,
        top_offset: 0.6,
        width: 1.2,
        slope: 0.08,
        thickness: 0.15,
        landing: 1.2,
        max_run: 9.0,
        path: vec![(0.0, 0.0), (10.0, 0.0)],
        material: "Concrete".into(),
    };
    let prev = f.m.ramp_preview(&s).unwrap();
    let id = f.m.add_ramp(&s).unwrap();
    assert!(!f.m.ramp_meshes(id).unwrap().is_empty());
    let _ = f.m.ramp_plan(id, 1.2).unwrap();
    assert!(f.m.ramp_plan(9999, 1.2).is_err());
    assert!(f.m.ramp_meshes(9999).is_err());
    let mut bad = s.clone();
    bad.width = 0.0;
    assert!(f.m.ramp_preview(&bad).is_err());
    let _ = prev;
}

// ---- structure, braces, roof edge ----
use crate::{BeamJoin, BeamSpec, ColumnSpec, RoofKind, RoofSpec, SkylightSpec, TaperSpec};
use crate::BraceSpec;

fn beam_at(f: &Fx, mt: i64) -> BeamSpec {
    BeamSpec {
        level: f.l2,
        member_type: mt,
        a: (0.0, 0.0, 0.0),
        b: (6.0, 0.0, 0.0),
        bulge: 0.0,
        yjust: Default::default(),
        zjust: Default::default(),
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::Auto,
        join_end: BeamJoin::Auto,
    }
}

#[test]
fn structure_queries() {
    let mut f = fx();
    let bt = f.m.add_catalog_member_type("IPE 300").unwrap();
    let ct = f.m.add_catalog_member_type("R 400x400").unwrap();
    let col = ColumnSpec {
        level: f.l1,
        top_level: Some(f.l2),
        member_type: ct,
        structural: true,
        at: (1.0, 1.0),
        base_offset: 0.0,
        top_offset: 0.0,
        rotation: 0.0,
        top_at: None,
    };
    let c = f.m.add_column(&col).unwrap();
    let spec = beam_at(&f, bt);
    let b = f.m.add_beam(&spec).unwrap();
    let on = f.m.beams_on(f.l2).unwrap();
    assert_eq!(on.len(), 1);
    assert_eq!(on[0].0, b);
    assert!(f.m.beams_on(f.l1).unwrap().is_empty());
    assert!(f.m.beams_on(9999).unwrap().is_empty());
    let meshes = f.m.member_meshes().unwrap();
    assert!(meshes.iter().any(|m| m.0 == "beam" && m.1 == b));
    assert!(meshes.iter().any(|m| m.0 == "column" && m.1 == c));
    let wb = f.m.member_weight("beam", b).unwrap().unwrap();
    assert!(wb > 0.0, "steel beam weighs something, got {wb}");
    assert!(f.m.member_weight("column", c).is_ok());
    assert!(f.m.member_weight("beam", 9999).is_err());
    // parts are footings and framing only: columns and beams are members, not parts
    let parts = f.m.struct_part_bodies(None).unwrap();
    assert!(parts.iter().all(|p| p.volume >= 0.0 && p.length >= 0.0));
    let on1 = f.m.struct_part_bodies(Some(f.l1)).unwrap().len();
    let on2 = f.m.struct_part_bodies(Some(f.l2)).unwrap().len();
    assert!(on1 + on2 <= parts.len());
    assert!(f.m.struct_part_bodies(Some(9999)).unwrap().is_empty());
    assert!(f.m.struct_footprints(f.l1).is_ok());
    assert!(f.m.struct_footprints(9999).unwrap().is_empty());
    // braces
    let br = BraceSpec {
        level: f.l1,
        member_type: bt,
        a: (0.0, 0.0, 0.0),
        b: (3.0, 0.0, 3.0),
        start: None,
        end: None,
        rotation: 0.0,
    };
    let id = f.m.add_brace(&br).unwrap();
    let mut up = br.clone();
    up.b = (4.0, 0.0, 3.0);
    f.m.update_brace(id, &up).unwrap();
    assert_eq!(f.m.brace(id).unwrap().b, (4.0, 0.0, 3.0));
    f.m.undo().unwrap();
    assert_eq!(f.m.brace(id).unwrap().b, (3.0, 0.0, 3.0));
    assert!(f.m.update_brace(9999, &br).is_err());
    up.a = up.b;
    assert!(f.m.update_brace(id, &up).is_err(), "zero-length brace");
}

#[test]
fn roof_edge_elements() {
    let mut f = fx();
    let r = RoofSpec {
        level: f.l2,
        slab_type: f.st,
        offset: 0.0,
        overhang: 0.3,
        fascia: 0.0,
        kind: RoofKind::Footprint {
            ring: rect(0.0, 0.0, 6.0, 5.0),
            slopes: vec![Some(20.0), None, Some(20.0), None],
            mansard: None,
        },
    };
    let roof = f.m.add_roof(&r).unwrap();
    // soffit
    let so = f.m.add_soffit(roof, 0.2, 0.05, None).unwrap();
    assert!(f.m.add_soffit(roof, 0.2, 0.0, None).is_err());
    assert!(f.m.add_soffit(roof, -1.0, 0.05, None).is_err());
    assert_eq!(f.m.level_soffits(f.l2).unwrap(), vec![so]);
    f.m.update_soffit(so, 0.3, 0.06, Some("Wood")).unwrap();
    assert!(f.m.update_soffit(so, 0.3, -1.0, None).is_err());
    f.m.undo().unwrap();
    // skylight
    let k = SkylightSpec { roof, center: (3.0, 2.5), width: 1.0, length: 1.2, curb: 0.15 };
    let kid = f.m.add_skylight(&k).unwrap();
    assert_eq!(f.m.level_skylights(f.l2).unwrap(), vec![kid]);
    assert!(f.m.add_skylight(&SkylightSpec { curb: -0.1, ..k.clone() }).is_err());
    assert!(f.m.add_skylight(&SkylightSpec { curb: f64::NAN, ..k.clone() }).is_err());
    f.m.delete_skylight(kid).unwrap();
    assert!(f.m.level_skylights(f.l2).unwrap().is_empty());
    f.m.undo().unwrap();
    assert_eq!(f.m.level_skylights(f.l2).unwrap().len(), 1);
    // generic delete
    f.m.delete_roof_edge("soffit", so).unwrap();
    assert!(f.m.delete_roof_edge("soffit", so).is_err());
    assert!(f.m.delete_roof_edge("roof", 1).is_err());
    f.m.delete_roof_edge("skylight", kid).unwrap();
    f.m.delete_soffit(9999).expect_err("no such soffit");
}

#[test]
fn taper_and_drain() {
    let mut f = fx();
    let t = TaperSpec {
        level: f.l2,
        slab: None,
        ring: rect(0.0, 0.0, 6.0, 5.0),
        offset: 0.0,
        slope: 0.02,
        t_min: 0.05,
        axis: (1.0, 0.0),
        material: None,
    };
    let id = f.m.add_taper(&t).unwrap();
    assert_eq!(f.m.tapers(f.l2).unwrap(), vec![id]);
    assert!(f.m.tapers(f.l1).unwrap().is_empty());
    let mut up = t.clone();
    up.slope = 0.03;
    f.m.update_taper(id, &up).unwrap();
    let d = f.m.add_drain(id, (3.0, 2.5)).unwrap();
    f.m.move_drain(d, (4.0, 2.0)).unwrap();
    assert!(f.m.move_drain(9999, (4.0, 2.0)).is_err());
    f.m.delete_roof_edge("drain", d).unwrap();
    f.m.delete_roof_edge("taper", id).unwrap();
    assert!(f.m.tapers(f.l2).unwrap().is_empty());
}

#[test]
fn groups_instances_and_expand() {
    let mut f = fx();
    let items: Vec<(&str, i64)> = f.walls.iter().map(|w| ("wall", *w)).collect();
    let (g, inst) = f.m.group_create("G", &items).unwrap();
    assert!(f.m.group_create("  ", &items).is_err());
    let insts = f.m.group_instances(g).unwrap();
    assert_eq!(insts.len(), 1);
    assert_eq!(insts[0].id, inst);
    assert_eq!(f.m.group_of("wall", f.walls[0]).unwrap(), Some(inst));
    assert_eq!(f.m.group_of("wall", 9999).unwrap(), None);
    let one = f.m.group_expand(&[("wall", f.walls[0])]).unwrap();
    assert_eq!(one.len(), 4, "a group member selects the whole instance");
    assert!(f.m.group_expand(&[]).unwrap().is_empty());
    let second = f.m.group_place(g, f.l1, 20.0, 0.0).unwrap();
    assert!(f.m.group_place(g, f.l1, f64::NAN, 0.0).is_err());
    assert_eq!(f.m.group_instances(g).unwrap().len(), 2);
    f.m.group_ungroup_many(&[inst, second]).unwrap();
    assert!(f.m.group_instances(g).unwrap().is_empty());
    f.m.undo().unwrap();
    assert_eq!(f.m.group_instances(g).unwrap().len(), 2);
    assert!(f.m.group_instances(9999).unwrap().is_empty());
    let (w, locked) = f.m.split_locked_sets(&[]).unwrap();
    assert!(w.is_empty() && locked.is_empty());
}

#[test]
fn topics_basic_queries() {
    use crate::topics::{Topic, Viewpoint};
    let f = fx();
    let t = Topic {
        title: "Clash".into(),
        status: "open".into(),
        priority: "normal".into(),
        author: "qa".into(),
        ..Default::default()
    };
    let id = f.m.topic_add(&t).unwrap();
    let all = f.m.topic_all().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, id);
    assert!(!all[0].guid.is_empty(), "a guid is generated");
    assert_eq!(f.m.topic_by_guid(&all[0].guid).unwrap(), Some(id));
    assert_eq!(f.m.topic_by_guid("").unwrap(), None);
    assert_eq!(f.m.topic_by_guid("nope").unwrap(), None);
    let vp = Viewpoint {
        eye: [0.0, -10.0, 5.0],
        dir: [0.0, 1.0, 0.0],
        up: [0.0, 0.0, 1.0],
        ortho: false,
        fov: 60.0,
        scale: 1.0,
        ..Viewpoint::default()
    };
    f.m.topic_set_view(id, Some(&vp), Some(&[1, 2, 3])).unwrap();
    let back = &f.m.topic_all().unwrap()[0];
    assert_eq!(back.viewpoint, Some(vp.clone()));
    assert_eq!(back.snapshot, Some(vec![1, 2, 3]));
    f.m.topic_set_view(id, None, None).unwrap();
    assert!(f.m.topic_all().unwrap()[0].viewpoint.is_none());
    assert!(f.m.topic_set_view(9999, None, None).is_err());
    let bad = Viewpoint { dir: [0.0, 0.0, 0.0], ..vp.clone() };
    assert!(f.m.topic_set_view(id, Some(&bad), None).is_err());
    let w = f.walls[0];
    assert!(f.m.topic_elem_alive("wall", w));
    assert!(!f.m.topic_elem_alive("wall", 9999));
    assert!(!f.m.topic_elem_alive("not a table\"; drop", 1), "unknown table is refused");
    let tag = f.m.topic_project_tag();
    assert_eq!(tag, f.m.topic_project_tag(), "stable");
}

#[test]
fn load_case_edit_and_delete_guard() {
    use crate::calc_store::LoadCase;
    let mut f = fx();
    let c = LoadCase {
        id: 0,
        name: "Dead".into(),
        category: "permanent".into(),
        self_weight: true,
        note: String::new(),
        locked: false,
        src: "manual".into(),
    };
    let id = f.m.add_load_case(&c).unwrap();
    let mut e = c.clone();
    e.note = "n".into();
    f.m.set_load_case(id, &e).unwrap();
    assert_eq!(f.m.load_cases().unwrap().iter().find(|x| x.id == id).unwrap().note, "n");
    f.m.undo().unwrap();
    assert_eq!(f.m.load_cases().unwrap().iter().find(|x| x.id == id).unwrap().note, "");
    e.name = " bad ".into();
    assert!(f.m.set_load_case(id, &e).is_err(), "padded name");
    e.name = "Dead".into();
    e.category = "nonsense".into();
    assert!(f.m.set_load_case(id, &e).is_err());
    assert!(f.m.set_load_case(9999, &c).is_err());
    assert!(f.m.delete_load(9999, "manual").is_err(), "no such load");
}

#[test]
fn annotation_batches_and_legends() {
    use crate::{DimKind, DimRef, DimSpec};
    let mut f = fx();
    let w = f.walls[0];
    let end = |e| DimRef::WallEnd { wall: w, face: 0, end: e };
    let d = |off: f64| DimSpec {
        level: f.l1,
        kind: DimKind::Linear,
        refs: vec![end(0), end(1)],
        dir: (1.0, 0.0),
        side: 1.0,
        offset: off,
        tier: 0,
        auto: false,
    };
    let ids = f.m.add_dims(&[d(0.5), d(1.0)]).unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(f.m.dims(f.l1).unwrap().len(), 2);
    f.m.undo().unwrap();
    assert!(f.m.dims(f.l1).unwrap().is_empty(), "a batch is one undo step");
    // tags that do not exist
    assert!(f.m.set_tag_leader(9999, true).is_err());
    assert!(f.m.delete_tag(9999).is_err());
    // legends
    let id = f.m.add_legend("Legend", 0.01).unwrap();
    f.m.update_legend(id, "Legend 2", 0.02).unwrap();
    assert!(f.m.update_legend(id, " ", 0.02).is_err());
    assert!(f.m.update_legend(id, "x", 0.0).is_err());
    assert!(f.m.update_legend(id, "x", f64::NAN).is_err());
    assert!(f.m.update_legend(9999, "x", 0.01).is_err());
    assert!(f.m.remove_legend_place(9999).is_err());
    f.m.undo().unwrap();
    assert_eq!(f.m.legends().unwrap()[0].name, "Legend");
}

fn put_mesh(f: &mut Fx, cat: &str) -> i64 {
    let ps: [[f64; 3]; 4] = [[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.]];
    let pb: Vec<u8> = ps.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
    let ib: Vec<u8> = [0u32, 1, 2, 0, 2, 3].iter().flat_map(|i| i.to_le_bytes()).collect();
    f.m.sql()
        .execute(
            "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,?2,'M',?3,?4)",
            rusqlite::params![f.l1, cat, pb, ib],
        )
        .unwrap();
    f.m.sql().last_insert_rowid()
}

#[test]
fn mesh_queries_and_edits() {
    let mut f = fx();
    let a = put_mesh(&mut f, "furniture");
    let b = put_mesh(&mut f, "furniture");
    let c = put_mesh(&mut f, "generic");
    assert_eq!(f.m.mesh_ids(None, None).unwrap(), vec![a, b, c]);
    assert_eq!(f.m.mesh_ids(Some(f.l1), Some("generic")).unwrap(), vec![c]);
    assert!(f.m.mesh_ids(Some(9999), None).unwrap().is_empty());
    assert!(f.m.mesh_ids(None, Some("none")).unwrap().is_empty());
    assert_eq!(
        f.m.mesh_categories().unwrap(),
        vec![("furniture".to_string(), 2), ("generic".to_string(), 1)]
    );
    f.m.delete_mesh_faces(a, &[0]).unwrap();
    let n: i64 = f
        .m
        .sql()
        .query_row("SELECT length(indices) FROM mesh WHERE id=?1", [a], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 12, "one of two triangles left");
    f.m.undo().unwrap();
    let n: i64 = f
        .m
        .sql()
        .query_row("SELECT length(indices) FROM mesh WHERE id=?1", [a], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 24);
    assert!(f.m.clean_meshes(&[], 1e-6).is_err());
    assert!(f.m.clean_meshes(&[a, b], 1e-6).is_ok());
    f.m.delete_meshes(&[a, b]).unwrap();
    assert_eq!(f.m.mesh_ids(None, None).unwrap(), vec![c]);
    f.m.undo().unwrap();
    assert_eq!(f.m.mesh_ids(None, None).unwrap().len(), 3);
    f.m.delete_meshes(&[]).unwrap();
}

#[test]
fn rebar_types_and_names() {
    use crate::rebar::{Faces, LongPos};
    for p in LongPos::ALL {
        assert_eq!(LongPos::parse(LongPos::name(p)), Some(p));
    }
    for p in Faces::ALL {
        assert_eq!(Faces::parse(Faces::name(p)), Some(p));
    }
    assert_eq!(LongPos::parse("middle"), None);
    assert_eq!(Faces::parse(""), None);
    let mut f = fx();
    let id = f.m.add_rebar_type("D16", 0.016, "B500", 0.0).unwrap();
    assert!(f.m.add_rebar_type(" ", 0.016, "B500", 0.0).is_err());
    assert!(f.m.add_rebar_type("x", 0.0, "B500", 0.0).is_err());
    assert!(f.m.add_rebar_type("x", 0.5, "B500", 0.0).is_err());
    assert!(f.m.add_rebar_type("x", f64::NAN, "B500", 0.0).is_err());
    assert!(f.m.add_rebar_type("x", 0.016, "B500", -1.0).is_err());
    let t = f.m.rebar_types().unwrap().into_iter().find(|t| t.id == id).unwrap();
    assert!(t.bend_radius() > 0.0);
    let m1 = t.mass_per_m();
    assert!((m1 - 1.578).abs() < 0.05, "D16 weighs about 1.58 kg/m, got {m1}");
    let mut own = t.clone();
    own.unit_mass = 2.0;
    assert_eq!(own.mass_per_m(), 2.0, "a stated mass wins over the density");
    // an empty host list deletes nothing
    f.m.delete_rebar(&[]).unwrap();
    assert_eq!(f.m.delete_host_rebar(&[]).unwrap(), 0);
    assert!(f.m.rebar_built().is_ok());
    assert!(f.m.rebar_meshes(None, None).unwrap().is_empty());
}

#[test]
fn plan_views_and_cut_profiles() {
    use crate::curtain::GridLines;
    use ddc_geom::curtain::Section;
    use crate::CurtainSpec;
    let mut f = fx();
    // empty levels give empty plans
    assert!(f.m.stairs_plan(f.l1, 1.2).unwrap().is_empty());
    assert!(f.m.curtain_plan(f.l1).unwrap().is_empty());
    assert!(f.m.curtain_plan(9999).unwrap().is_empty());
    let sec = Section::Rect { w: 0.05, d: 0.15 };
    let cw = CurtainSpec {
        level: f.l1,
        name: "CW".into(),
        a: (10.0, 0.0),
        b: (16.0, 0.0),
        bulge: 0.0,
        base_offset: 0.0,
        height: 3.0,
        u: GridLines::Spacing(1.5),
        v: GridLines::Spacing(1.0),
        interior: sec,
        border: sec,
        lines: Default::default(),
        top: vec![],
    };
    let two = f.m.add_curtain_walls(&[cw.clone(), CurtainSpec { a: (10.0, 5.0), b: (16.0, 5.0), ..cw }]).unwrap();
    assert_eq!(two.len(), 2);
    assert!(!f.m.curtain_plan(f.l1).unwrap().is_empty());
    f.m.undo().unwrap();
    assert!(f.m.curtain_plan(f.l1).unwrap().is_empty(), "both walls are one undo step");
    assert!(f.m.add_curtain_walls(&[]).is_err());
    // stair plan marks
    f.m.add_stair(&stair_spec(&f)).unwrap();
    assert!(!f.m.stairs_plan(f.l1, 1.2).unwrap().is_empty());
    assert!(f.m.stairs_plan(f.l2, 1.2).unwrap().is_empty());
    // cut profiles
    let s = slab(&mut f);
    assert!(f.m.slab_cut_rings(s).unwrap().is_empty(), "no cut applied");
    assert!(f.m.slab_cut_rings(9999).unwrap().is_empty());
    let base = f.m.wall_base_profile(f.walls[0]).unwrap();
    let many = f.m.wall_base_profiles(&f.walls).unwrap();
    assert_eq!(many.len(), 4);
    assert_eq!(many[0], base);
    assert!(f.m.wall_base_profiles(&[]).unwrap().is_empty());
}

#[test]
fn view_gfx_fields_filters_and_styles() {
    use crate::view_gfx::{DetailLevel, GfxOverride, TemplateField, ViewFilterSpec, ViewGfx};
    let a = ViewGfx { detail: DetailLevel::Fine, thin_lines: true, ..Default::default() };
    let mut b = ViewGfx::default();
    b.take_fields(&a, &[TemplateField::Detail]);
    assert_eq!(b.detail, DetailLevel::Fine);
    assert!(!b.thin_lines, "only the listed fields are copied");
    b.take_fields(&a, &[]);
    assert!(!b.thin_lines);
    b.take_fields(&a, &TemplateField::ALL);
    assert_eq!(b, a);

    let all = ViewFilterSpec::default();
    assert!(all.applies_to("wall") && all.applies_to(""), "no categories means all");
    let some = ViewFilterSpec { categories: vec!["wall".into()], ..Default::default() };
    assert!(some.applies_to("wall"));
    assert!(!some.applies_to("slab"));
    assert!(some.matches("wall", &|_| None), "no rules match every element");
    assert!(!some.matches("slab", &|_| None));

    let mut f = fx();
    assert!(f.m.view_filters().unwrap().is_empty());
    let red = GfxOverride { line: Some(0xff0000), ..Default::default() };
    f.m.set_object_styles(&[("wall".to_string(), red)]).unwrap();
    f.m.undo().unwrap();
    f.m.set_object_styles(&[]).unwrap();
    assert!(f.m.set_object_styles(&[("nonsense".to_string(), red)]).is_err());
    let bad = GfxOverride { line: Some(0x1_000_000), ..Default::default() };
    assert!(f.m.set_object_styles(&[("wall".to_string(), bad)]).is_err());
    let bad = GfxOverride { transparency: Some(101), ..Default::default() };
    assert!(f.m.set_object_styles(&[("wall".to_string(), bad)]).is_err());
    let bad = GfxOverride { line_weight: Some(f32::NAN), ..Default::default() };
    assert!(f.m.set_object_styles(&[("wall".to_string(), bad)]).is_err());
}

#[test]
fn bulk_batch_and_phase_index() {
    use crate::phases::PhaseIndex;
    let mut f = fx();
    let sync = |m: &Model| -> i64 {
        m.sql().query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap()
    };
    let before = sync(&f.m);
    let n0: i64 = f.m.sql().query_row("SELECT COUNT(*) FROM level", [], |r| r.get(0)).unwrap();
    let n: i64 = f
        .m
        .batch_bulk("level_bulk_test", |db| {
            db.execute("INSERT INTO level(name, elevation) VALUES ('B', 9.0)", [])?;
            Ok(db.query_row("SELECT COUNT(*) FROM level", [], |r| r.get(0))?)
        })
        .unwrap();
    assert_eq!(n, n0 + 1);
    assert_eq!(sync(&f.m), before, "pragmas are restored");
    // a failing closure leaves nothing behind and still restores the pragmas
    let r: Result<(), _> = f.m.batch_bulk("level_bulk_fail", |db| {
        db.execute("INSERT INTO level(name, elevation) VALUES ('C', 10.0)", [])?;
        Err(crate::Error::Invalid("stop".into()))
    });
    assert!(r.is_err());
    assert_eq!(sync(&f.m), before);
    let c: i64 = f.m.sql().query_row("SELECT COUNT(*) FROM level", [], |r| r.get(0)).unwrap();
    assert_eq!(c, n0 + 1, "rolled back");
    // phases: names and defaults
    let px = PhaseIndex::load(f.m.sql()).unwrap();
    assert_eq!(px.name(9999), "");
    let first = px.first();
    let e = px.get("wall", 424242);
    assert_eq!(e.created, first.unwrap_or(0));
    assert_eq!(e.demolished, None);
    if let Some(p) = first {
        assert!(!px.name(p).is_empty());
    }
}
