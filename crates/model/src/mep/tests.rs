// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::{CompSpec, LayerSpec, SlabSpec, WallSpec};
use std::f64::consts::PI;

fn close(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() <= eps
}

fn model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    (m, l)
}

#[test]
fn schema_16_tables_and_undo() {
    let (mut m, l) = model();
    assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
    let s = m.add_mep_system("CW-1", SystemKind::ColdWater).unwrap();
    let r = m
        .add_mep_run(
            &RunSpec::pipe(
                s,
                l,
                PipeMaterial::Copper,
                15,
                vec![(0.0, 0.0, 1.0), (2.0, 0.0, 1.0)],
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(m.mep_run(r).unwrap().dn, Some(15));
    assert_eq!(
        m.mep_systems().unwrap()[0].color,
        SystemKind::ColdWater.color()
    );
    // a level with a run cannot be deleted
    assert!(matches!(m.delete_level(l), Err(Error::Referenced { .. })));
    m.undo().unwrap();
    assert!(m.mep_runs(None).unwrap().is_empty());
    m.assert_integrity();
    // a duct in a water-supply system is an error
    let duct = RunSpec::new(
        s,
        l,
        Section::new(Domain::Duct, Shape::Rect { w: 0.4, h: 0.2 }),
        vec![(0.0, 0.0, 3.0), (1.0, 0.0, 3.0)],
    );
    assert!(m.add_mep_run(&duct).is_err());
}

#[test]
fn elbow_90_dn100_length() {
    let (mut m, l) = model();
    let s = m.add_mep_system("HW", SystemKind::Heating).unwrap();
    m.add_mep_run(
        &RunSpec::pipe(
            s,
            l,
            PipeMaterial::Steel,
            100,
            vec![(0.0, 0.0, 1.0), (3.0, 0.0, 1.0), (3.0, 4.0, 1.0)],
        )
        .unwrap(),
    )
    .unwrap();
    let net = m.mep_network(Some(s)).unwrap();
    assert_eq!(net.count(FittingKind::Elbow), 1);
    let e = &net.fittings[0];
    assert_eq!(e.size, "DN100");
    assert!(close(e.qty.length, 0.1524 * PI / 2.0, 1e-12));
    assert_eq!(net.open_ends.len(), 2);
    let q = m.mep_quantities(Some(s)).unwrap();
    assert!(close(q.sizes[0].length, 7.0 - 2.0 * 0.1524, 1e-9));
    assert_eq!(q.fittings, vec![("elbow", "DN100".to_string(), 1)]);
}

#[test]
fn duct_400x200_area_per_metre_and_insulation() {
    let (mut m, l) = model();
    let s = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let mut sec = Section::new(Domain::Duct, Shape::Rect { w: 0.4, h: 0.2 });
    sec.insulation = 0.05;
    m.add_mep_run(&RunSpec::new(
        s,
        l,
        sec,
        vec![(0.0, 0.0, 3.0), (10.0, 0.0, 3.0)],
    ))
    .unwrap();
    let q = m.mep_quantities(Some(s)).unwrap();
    let r = &q.sizes[0];
    assert_eq!(r.size, "400x200");
    assert!(close(r.surface / r.length, 1.2, 1e-12));
    assert!(close(r.insulation, 10.0 * (0.5 * 0.3 - 0.08), 1e-9));
}

#[test]
fn sanitary_slope_two_percent_over_five_metres() {
    let (mut m, l) = model();
    let s = m.add_mep_system("SAN", SystemKind::Sanitary).unwrap();
    let mut r = RunSpec::pipe(
        s,
        l,
        PipeMaterial::Pvc,
        100,
        vec![(0.0, 0.0, -0.3), (5.0, 0.0, 0.0)],
    )
    .unwrap();
    r.slope = 2.0;
    let id = m.add_mep_run(&r).unwrap();
    let p = m.mep_run(id).unwrap().points;
    assert!(close(p[0].2 - p[1].2, 0.1, 1e-12));
    assert!(close(p[1].2, -0.4, 1e-12));
}

#[test]
fn tee_connectivity_caps_and_open_ends() {
    let (mut m, l) = model();
    let s = m.add_mep_system("CW", SystemKind::ColdWater).unwrap();
    let mut main = RunSpec::pipe(
        s,
        l,
        PipeMaterial::Copper,
        25,
        vec![(0.0, 0.0, 1.0), (10.0, 0.0, 1.0)],
    )
    .unwrap();
    main.caps = (true, false);
    let a = m.add_mep_run(&main).unwrap();
    let b = m
        .add_mep_run(
            &RunSpec::pipe(
                s,
                l,
                PipeMaterial::Copper,
                15,
                vec![(5.0, 0.0, 1.0), (5.0, 3.0, 1.0)],
            )
            .unwrap(),
        )
        .unwrap();
    let net = m.mep_network(Some(s)).unwrap();
    assert_eq!(net.count(FittingKind::Tee), 1);
    assert_eq!(net.count(FittingKind::Cap), 1);
    assert_eq!(net.edges.len(), 3);
    assert_eq!(net.parts(), 1);
    let tee = net
        .nodes
        .iter()
        .find(|n| n.fitting == Some(FittingKind::Tee))
        .unwrap();
    assert_eq!(tee.at, (5.0, 0.0, 1.0));
    let mut arms = tee.arms.clone();
    arms.sort();
    assert_eq!(arms, vec![(a, 2), (a, 2), (b, 0)]);
    let mut open: Vec<i64> = net.open_ends.iter().map(|o| o.0).collect();
    open.sort();
    assert_eq!(open, vec![a, b]);
    // cross: a second branch into the same point
    m.add_mep_run(
        &RunSpec::pipe(
            s,
            l,
            PipeMaterial::Copper,
            15,
            vec![(5.0, 0.0, 1.0), (5.0, -3.0, 1.0)],
        )
        .unwrap(),
    )
    .unwrap();
    let net = m.mep_network(Some(s)).unwrap();
    assert_eq!(
        (net.count(FittingKind::Tee), net.count(FittingKind::Cross)),
        (0, 1)
    );
    // five legs: a riser up through the crossing
    m.add_mep_run(
        &RunSpec::pipe(
            s,
            l,
            PipeMaterial::Copper,
            15,
            vec![(5.0, 0.0, 1.0), (5.0, 0.0, 4.0)],
        )
        .unwrap(),
    )
    .unwrap();
    let net = m.mep_network(Some(s)).unwrap();
    assert_eq!(net.count(FittingKind::Cross), 1);
}

#[test]
fn transition_and_node_elbow_between_runs() {
    let (mut m, l) = model();
    let s = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let rect = |w, h| Section::new(Domain::Duct, Shape::Rect { w, h });
    m.add_mep_run(&RunSpec::new(
        s,
        l,
        rect(0.6, 0.3),
        vec![(0.0, 0.0, 3.0), (5.0, 0.0, 3.0)],
    ))
    .unwrap();
    m.add_mep_run(&RunSpec::new(
        s,
        l,
        rect(0.4, 0.3),
        vec![(5.0, 0.0, 3.0), (9.0, 0.0, 3.0)],
    ))
    .unwrap();
    m.add_mep_run(&RunSpec::new(
        s,
        l,
        rect(0.4, 0.3),
        vec![(9.0, 0.0, 3.0), (9.0, 5.0, 3.0)],
    ))
    .unwrap();
    let net = m.mep_network(Some(s)).unwrap();
    assert_eq!(net.count(FittingKind::Transition), 1);
    assert_eq!(net.count(FittingKind::Elbow), 1);
    let el = net
        .fittings
        .iter()
        .find(|f| f.kind == FittingKind::Elbow)
        .unwrap();
    // rectangular: radius = width in the plane of the turn
    assert!(close(el.qty.length, 0.4 * PI / 2.0, 1e-12));
    assert_eq!(net.open_ends.len(), 2);
    let meshes = m.mep_meshes(Some(s)).unwrap();
    assert!(meshes.iter().all(|x| x.2.is_watertight()));
}

#[test]
fn clash_count_on_known_scene() {
    let (mut m, l) = model();
    let wt = m
        .add_wall_type(
            "W200",
            &[LayerSpec {
                material: "Concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    for x in [2.0, 6.0] {
        m.add_wall(&WallSpec {
            a: (x, -5.0),
            b: (x, 5.0),
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    }
    let st = m
        .add_slab_type(
            "S200",
            &[LayerSpec {
                material: "Concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    m.add_slab(&SlabSpec {
        level: l,
        slab_type: st,
        offset: 3.2,
        outer: vec![(0.0, -5.0), (10.0, -5.0), (10.0, 5.0), (0.0, 5.0)],
        holes: vec![vec![(7.0, -1.0), (8.0, -1.0), (8.0, 1.0), (7.0, 1.0)]],
    })
    .unwrap();
    let s = m.add_mep_system("CW", SystemKind::ColdWater).unwrap();
    let pipe = |pts| RunSpec::pipe(s, l, PipeMaterial::Steel, 50, pts).unwrap();
    let a = m
        .add_mep_run(&pipe(vec![(0.0, 0.0, 1.0), (9.0, 0.0, 1.0)]))
        .unwrap();
    // a riser through a slab opening has no intersection; through the slab, one
    m.add_mep_run(&pipe(vec![(7.5, 0.0, 2.0), (7.5, 0.0, 5.0)]))
        .unwrap();
    let c = m
        .add_mep_run(&pipe(vec![(4.0, 0.0, 2.0), (4.0, 0.0, 5.0)]))
        .unwrap();
    let hits = m.mep_clashes(Some(s), 1e-9).unwrap();
    assert_eq!(hits.len(), 3);
    let area = PI * 0.0603 * 0.0603 / 4.0;
    assert_eq!((hits[0].run, hits[0].other), (a, "wall"));
    assert!(close(hits[0].volume, area * 0.2, 1e-9));
    assert_eq!((hits[2].run, hits[2].other), (c, "slab"));
    assert!(close(hits[2].volume, area * 0.2, 1e-9));
}

#[test]
fn riser_plan_symbols_and_section() {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let s = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let sec = Section::new(Domain::Duct, Shape::Round { d: 0.3 });
    let r = m
        .add_mep_riser(
            RunSpec::new(s, l1, sec, vec![]),
            1.0,
            1.0,
            (l1, 0.5),
            (l2, 2.5),
        )
        .unwrap();
    assert_eq!(
        m.mep_run(r).unwrap().points,
        vec![(1.0, 1.0, 0.5), (1.0, 1.0, 5.5)]
    );
    let plan = m.mep_plan(l1, Detail::Double).unwrap();
    assert!(matches!(plan[0].prim, PlanPrim::Rise { .. }));
    assert_eq!(plan[0].color, SystemKind::SupplyAir.color());
    let cut = m.mep_section((0.0, 0.0, 1.0), 2.0).unwrap();
    assert_eq!(cut.len(), 1);
    assert!(cut[0].1.iter().all(|p| close(p.2, 2.0, 1e-12)));
}

#[test]
fn component_ports_and_auto_connect() {
    let (mut m, l) = model();
    let fam = m.load_library_family("Sink").unwrap();
    let ty = m.comp_types(fam).unwrap()[0].0;
    let c = m.add_component(&CompSpec::at(l, ty, 2.0, 1.0)).unwrap();
    let ports = m.component_ports(Some(c)).unwrap();
    assert_eq!(ports.len(), 3);
    assert_eq!(ports[0].system, Some(SystemKind::Sanitary));
    assert!(close(ports[0].at.2, 0.9 - 0.18, 1e-12));
    let s = m.add_mep_system("SAN", SystemKind::Sanitary).unwrap();
    m.add_mep_run(
        &RunSpec::pipe(
            s,
            l,
            PipeMaterial::Pvc,
            100,
            vec![(0.0, 0.0, 0.1), (6.0, 0.0, 0.1)],
        )
        .unwrap(),
    )
    .unwrap();
    let br = m.mep_connect(c, "Waste", s).unwrap();
    assert_eq!(m.mep_run(br).unwrap().dn, Some(40));
    let net = m.mep_network(Some(s)).unwrap();
    assert!(net
        .nodes
        .iter()
        .any(|n| n.port == Some((c, "Waste".into()))));
    assert_eq!(net.count(FittingKind::Tee), 1);
    assert!(!net.open_ends.iter().any(|o| o.0 == br));
}

fn clash_scene() -> (Model, i64, i64, i64) {
    let (mut m, l) = model();
    let wt = m
        .add_wall_type(
            "W200",
            &[LayerSpec {
                material: "Concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let w = m
        .add_wall(&WallSpec {
            a: (3.0, -5.0),
            b: (3.0, 5.0),
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    let s = m.add_mep_system("CW", SystemKind::ColdWater).unwrap();
    let r = m
        .add_mep_run(
            &RunSpec::pipe(
                s,
                l,
                PipeMaterial::Steel,
                50,
                vec![(0.0, 0.0, 1.0), (8.0, 0.0, 1.0)],
            )
            .unwrap(),
        )
        .unwrap();
    (m, w, r, l)
}

fn pipe_vs_wall() -> ClashParams {
    ClashParams {
        a: ClashSide::cats(&["pipe"]),
        b: ClashSide::cats(&["wall"]),
        ..Default::default()
    }
}

#[test]
fn clash_sets_filter_and_clearance() {
    let (m, w, r, l) = clash_scene();
    let hits = m.clash_find(&pipe_vs_wall()).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        (hits[0].a.clone(), hits[0].b.clone()),
        (("pipe".into(), r), ("wall".into(), w))
    );
    assert!(
        hits[0].lo[0] >= 2.9 && hits[0].hi[0] <= 3.1,
        "{:?}",
        hits[0]
    );
    assert_eq!(hits[0].level, Some(l));
    // another level, another category, and selection by id
    let mut p = pipe_vs_wall();
    p.a.level = Some(l + 100);
    assert!(m.clash_find(&p).unwrap().is_empty());
    p.a.level = None;
    p.b.only = Some(vec![("wall".into(), w + 1)]);
    assert!(m.clash_find(&p).unwrap().is_empty());
    p.b = ClashSide::cats(&["slab"]);
    assert!(m.clash_find(&p).unwrap().is_empty());
    // clearance: a pipe along a wall 0.37 m from the face does not intersect but violates the 0.5 m clearance
    let (mut m2, _, r2, _) = clash_scene();
    let mut spec = m2.mep_run(r2).unwrap();
    spec.points = vec![(2.5, -3.0, 1.0), (2.5, 3.0, 1.0)];
    m2.update_mep_run(r2, &spec).unwrap();
    assert!(m2.clash_find(&pipe_vs_wall()).unwrap().is_empty());
    let mut c = pipe_vs_wall();
    c.clearance = true;
    c.tolerance = 0.5;
    assert_eq!(m2.clash_find(&c).unwrap().len(), 1);
    c.tolerance = 0.1;
    assert!(m2.clash_find(&c).unwrap().is_empty());
}

#[test]
fn clash_status_persists_between_runs() {
    let (mut m, _w, _r, _) = clash_scene();
    let k = pipe_vs_wall().test_key();
    let hits = m.clash_find(&pipe_vs_wall()).unwrap();
    let r1 = m.clash_merge(&k, &hits).unwrap();
    assert_eq!((r1.new, r1.still, r1.resolved), (1, 0, 0));
    let it = m.clash_items().unwrap().remove(0);
    assert_eq!(it.status, ClashStatus::New);
    // status and comment survive a re-run (the key is the element pair)
    m.clash_set_status(&k, &it.b, &it.a, ClashStatus::Reviewed, "agreed with structures")
        .unwrap();
    let r2 = m.clash_merge(&k, &hits).unwrap();
    assert_eq!((r2.new, r2.still, r2.resolved), (0, 1, 0));
    let it2 = m.clash_items().unwrap().remove(0);
    assert_eq!(it2.status, ClashStatus::Reviewed);
    assert_eq!(it2.comment, "agreed with structures");
    assert_eq!(it2.first_run, 1);
    assert_eq!(it2.last_run, 2);
    // vanished: Resolved; came back: Active; the comment is kept
    let r3 = m.clash_merge(&k, &[]).unwrap();
    assert_eq!(r3.resolved, 1);
    let it3 = m.clash_items().unwrap().remove(0);
    assert_eq!((it3.status, it3.present), (ClashStatus::Resolved, false));
    m.clash_merge(&k, &hits).unwrap();
    let it4 = m.clash_items().unwrap().remove(0);
    assert_eq!((it4.status, it4.present), (ClashStatus::Active, true));
    assert_eq!(it4.comment, "agreed with structures");
    // Accepted is not reset by vanishing
    m.clash_set_status(&k, &it4.a, &it4.b, ClashStatus::Approved, "")
        .unwrap();
    m.clash_merge(&k, &[]).unwrap();
    assert_eq!(m.clash_items().unwrap()[0].status, ClashStatus::Approved);
}

#[test]
fn clash_quick_fixes_apply_and_undo() {
    let (mut m, w, r, _) = clash_scene();
    let k = pipe_vs_wall().test_key();
    m.clash_merge(&k, &m.clash_find(&pipe_vs_wall()).unwrap())
        .unwrap();
    let it = m.clash_items().unwrap().remove(0);
    let before = m.mep_run(r).unwrap();

    // bypass over the top: the clash disappears, undo restores the route
    let fix = m.clash_fix_reroute(&it, true, 0.05).unwrap();
    m.apply_clash_fix(&fix).unwrap();
    assert!(m.mep_run(r).unwrap().points.len() > 2);
    assert!(m.clash_find(&pipe_vs_wall()).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.mep_run(r).unwrap(), before);
    assert_eq!(m.clash_find(&pipe_vs_wall()).unwrap().len(), 1);

    // sleeve in a wall: the void opening encloses the pipe, undo removes it
    let fix = m.clash_fix_sleeve(&it, 0.03).unwrap();
    let ClashFix::Opening { wall, spec } = &fix else {
        panic!("{fix:?}")
    };
    assert_eq!((*wall, spec.kind.as_str()), (w, "void"));
    assert!(spec.along < 5.0 && spec.along + spec.width > 5.0);
    let n = |m: &Model| -> i64 {
        m.sql()
            .query_row("SELECT count(*) FROM opening", [], |r| r.get(0))
            .unwrap()
    };
    m.apply_clash_fix(&fix).unwrap();
    assert_eq!(n(&m), 1);
    // the opening is subtracted from the wall body: the clash is really gone
    assert!(m.clash_find(&pipe_vs_wall()).unwrap().is_empty());
    // a second opening at the same place is not placed
    assert!(m.clash_fix_sleeve(&it, 0.03).is_err());
    m.undo().unwrap();
    assert_eq!(n(&m), 0);
    assert_eq!(m.clash_find(&pipe_vs_wall()).unwrap().len(), 1);
}

#[test]
fn clash_resolve_scoped_to_test_and_delete_forgets_pair() {
    let (mut m, w, _r, _) = clash_scene();
    let p = pipe_vs_wall();
    let k = p.test_key();
    m.clash_merge(&k, &m.clash_find(&p).unwrap()).unwrap();
    // another check (set B is slabs) found nothing: the "pipe × wall" pair is not resolved
    let mut q = pipe_vs_wall();
    q.b = ClashSide::cats(&["slab"]);
    assert_ne!(q.test_key(), k);
    let mut sel = pipe_vs_wall();
    sel.b.only = Some(vec![("wall".into(), w + 1)]);
    assert_ne!(sel.test_key(), k);
    assert_eq!(m.clash_merge(&q.test_key(), &[]).unwrap().resolved, 0);
    assert_eq!(m.clash_merge(&sel.test_key(), &[]).unwrap().resolved, 0);
    let it = m.clash_items().unwrap().remove(0);
    assert_eq!((it.status, it.present), (ClashStatus::New, true));
    assert_eq!(it.test, k);
    assert!(m
        .clash_set_status(&q.test_key(), &it.a, &it.b, ClashStatus::Approved, "")
        .is_err());
    // the wall was deleted: the pair is forgotten and a new element with the same id will not
    // find it; the same wall after undo returns with its status and comment
    m.clash_set_status(&k, &it.a, &it.b, ClashStatus::Approved, "agreed")
        .unwrap();
    m.delete_wall(w).unwrap();
    assert!(m.clash_items().unwrap().is_empty());
    m.undo().unwrap();
    let back = m.clash_items().unwrap().remove(0);
    assert_eq!(
        (back.status, back.comment.as_str()),
        (ClashStatus::Approved, "agreed")
    );
    m.clash_merge(&k, &m.clash_find(&p).unwrap()).unwrap();
    let it = m.clash_items().unwrap().remove(0);
    assert_eq!(
        (it.status, it.comment.as_str()),
        (ClashStatus::Approved, "agreed")
    );
}

#[test]
fn clash_clearance_offsets_flat_section() {
    // a flat 600×100 duct 50 mm above a slab: a 60 mm clearance is violated, 40 mm is not
    let (mut m, l) = model();
    let st = m
        .add_slab_type(
            "S200",
            &[LayerSpec {
                material: "Concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    m.add_slab(&SlabSpec {
        level: l,
        slab_type: st,
        offset: 0.9,
        outer: vec![(-1.0, -2.0), (9.0, -2.0), (9.0, 2.0), (-1.0, 2.0)],
        holes: vec![],
    })
    .unwrap();
    let s = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    m.add_mep_run(&RunSpec::new(
        s,
        l,
        Section::new(Domain::Duct, Shape::Rect { w: 0.6, h: 0.1 }),
        vec![(0.0, 0.0, 1.0), (8.0, 0.0, 1.0)],
    ))
    .unwrap();
    let mut p = ClashParams {
        a: ClashSide::cats(&["duct"]),
        b: ClashSide::cats(&["slab"]),
        tolerance: 0.06,
        clearance: true,
    };
    assert_eq!(m.clash_find(&p).unwrap().len(), 1);
    p.tolerance = 0.04;
    assert!(m.clash_find(&p).unwrap().is_empty());
    // clearance without MEP categories is not defined for anyone
    p.a = ClashSide::cats(&["wall"]);
    assert!(m.clash_find(&p).is_err());
}

#[test]
fn clash_slab_sleeve_adds_hole_and_undoes() {
    let (mut m, l) = model();
    let st = m
        .add_slab_type(
            "S200",
            &[LayerSpec {
                material: "Concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let slab = m
        .add_slab(&SlabSpec {
            level: l,
            slab_type: st,
            offset: 3.2,
            outer: vec![(0.0, -5.0), (10.0, -5.0), (10.0, 5.0), (0.0, 5.0)],
            holes: vec![],
        })
        .unwrap();
    let s = m.add_mep_system("CW", SystemKind::ColdWater).unwrap();
    m.add_mep_run(
        &RunSpec::pipe(
            s,
            l,
            PipeMaterial::Steel,
            50,
            vec![(4.0, 0.0, 2.0), (4.0, 0.0, 5.0)],
        )
        .unwrap(),
    )
    .unwrap();
    let p = ClashParams {
        a: ClashSide::cats(&["pipe"]),
        b: ClashSide::cats(&["slab"]),
        ..Default::default()
    };
    let hits = m.clash_find(&p).unwrap();
    assert_eq!(hits.len(), 1);
    m.clash_merge(&p.test_key(), &hits).unwrap();
    let it = m.clash_items().unwrap().remove(0);
    let fix = m.clash_fix_sleeve(&it, 0.05).unwrap();
    m.apply_clash_fix(&fix).unwrap();
    assert_eq!(m.slab_rings(slab).unwrap().len(), 2);
    assert!(m.clash_find(&p).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.slab_rings(slab).unwrap().len(), 1);
}

fn all_finite(net: &MepNetwork) -> bool {
    let pt = |p: V3| p.x.is_finite() && p.y.is_finite() && p.z.is_finite();
    net.fitting_meshes
        .iter()
        .all(|(_, m)| m.positions.iter().all(|p| pt(*p)))
        && net.pieces.iter().all(|(_, p, _)| {
            let (a, b) = p.ends();
            pt(a) && pt(b) && p.length().is_finite()
        })
        && net.fittings.iter().all(|f| f.qty.volume.is_finite())
}

#[test]
fn zero_length_tray_segment_keeps_fittings_finite() {
    let (mut m, l) = model();
    let s = m.add_mep_system("PW", SystemKind::Power).unwrap();
    let sec = Section::new(Domain::CableTray, Shape::Rect { w: 0.3, h: 0.1 });
    // a repeated point at a corner and in the middle of a straight
    let pts = vec![
        (0.0, 0.0, 3.0),
        (2.0, 0.0, 3.0),
        (2.0, 0.0, 3.0),
        (2.0, 2.0, 3.0),
        (2.0, 2.0, 3.0),
        (2.0, 4.0, 3.0),
    ];
    m.add_mep_run(&RunSpec::new(s, l, sec, pts)).unwrap();
    let net = m.mep_network(None).unwrap();
    assert!(!net.pieces.is_empty());
    assert!(all_finite(&net));
}

#[test]
fn vertical_riser_with_zero_horizontal_run_is_finite() {
    let (mut m, l) = model();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let s = m.add_mep_system("PW", SystemKind::Power).unwrap();
    let sec = Section::new(Domain::CableTray, Shape::Rect { w: 0.3, h: 0.1 });
    m.add_mep_riser(RunSpec::new(s, l, sec, vec![]), 1.0, 1.0, (l, 0.0), (l2, 0.0))
        .unwrap();
    // a horizontal branch leaves the top of the riser
    m.add_mep_run(&RunSpec::new(s, l, sec, vec![(1.0, 1.0, 3.0), (1.0, 1.0, 3.0), (4.0, 1.0, 3.0)]))
        .unwrap();
    let net = m.mep_network(None).unwrap();
    assert!(!net.pieces.is_empty());
    assert!(all_finite(&net));
}
