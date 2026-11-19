// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Steel connections in the model: the gazebo gate (every node placed in one request), parts
//! as solids that follow a section change, one undo step, issues for broken connections.

use ddc_geom::member::Shape;
use ddc_geom::steel::ConnKind;
use ddc_model::{BeamJoin, BeamSpec, ColumnSpec, Model, SteelConnSpec};

struct Gazebo {
    m: Model,
    l0: i64,
    cols: Vec<i64>,
    ring_type: i64,
}

/// 3 x 3 m gazebo: four pipe columns, IPE ring beams between them, four pipe rafters from
/// the columns to a ball at the apex.
fn gazebo() -> Gazebo {
    let mut m = Model::open(":memory:").unwrap();
    let l0 = m.add_level("L0", 0.0).unwrap();
    let col_t = m
        .add_member_type(
            "CHS 168x8",
            &Shape::Pipe {
                d: 0.1683,
                t: 0.008,
            },
            "S355",
        )
        .unwrap();
    let ring_t = m
        .add_member_type(
            "IPE 200",
            &Shape::I {
                h: 0.2,
                b: 0.1,
                tw: 0.0056,
                tf: 0.0085,
            },
            "S355",
        )
        .unwrap();
    let raf_t = m
        .add_member_type(
            "CHS 76x4",
            &Shape::Pipe {
                d: 0.0761,
                t: 0.004,
            },
            "S355",
        )
        .unwrap();
    let corners = [(0.0, 0.0), (3.0, 0.0), (3.0, 3.0), (0.0, 3.0)];
    let cs: Vec<ColumnSpec> = corners
        .iter()
        .map(|&at| ColumnSpec {
            level: l0,
            top_level: None,
            member_type: col_t,
            structural: true,
            at,
            base_offset: 0.0,
            top_offset: 3.2,
            rotation: 0.0,
            top_at: None,
        })
        .collect();
    let cols = m.add_columns(&cs).unwrap();
    let beam = |t: i64, a: (f64, f64, f64), b: (f64, f64, f64)| BeamSpec {
        level: l0,
        member_type: t,
        a,
        b,
        bulge: 0.0,
        yjust: Default::default(),
        zjust: Default::default(),
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::None,
        join_end: BeamJoin::None,
    };
    let mut bs = vec![];
    for i in 0..4 {
        let (a, b) = (corners[i], corners[(i + 1) % 4]);
        bs.push(beam(ring_t, (a.0, a.1, 2.4), (b.0, b.1, 2.4)));
    }
    for c in corners {
        bs.push(beam(raf_t, (c.0, c.1, 2.8), (1.5, 1.5, 4.0)));
    }
    m.add_beams(&bs).unwrap();
    Gazebo {
        m,
        l0,
        cols,
        ring_type: ring_t,
    }
}

fn steel_issues(m: &Model) -> Vec<String> {
    m.check_integrity()
        .unwrap()
        .into_iter()
        .filter(|i| i.table == "steel_conn")
        .map(|i| format!("{} {}", i.id, i.problem))
        .collect()
}

#[test]
fn gazebo_gate_places_every_node_in_one_request() {
    let mut g = gazebo();
    let solids0 = g.m.solids(None).unwrap().len();
    let all = g.m.steel_conn_candidates(None, Some(g.l0)).unwrap();
    let n = |k: ConnKind| all.iter().filter(|s| s.kind == k).count();
    assert_eq!(n(ConnKind::BasePlate), 4, "{all:#?}");
    assert_eq!(n(ConnKind::Pinned), 8);
    assert_eq!(n(ConnKind::BracingGusset), 4);
    assert_eq!(n(ConnKind::SphereNode), 1);
    let ids = g.m.add_steel_conns(&all).unwrap();
    assert_eq!(ids.len(), 17);
    assert!(steel_issues(&g.m).is_empty(), "{:#?}", steel_issues(&g.m));
    let parts: usize = ids
        .iter()
        .map(|id| g.m.steel_conn_parts(*id).unwrap().len())
        .sum();
    assert!(parts > 17 * 3);
    assert_eq!(g.m.solids(None).unwrap().len(), solids0 + parts);
    for id in &ids {
        let r = g.m.steel_conn_report(*id).unwrap();
        assert!(r.error.is_none(), "{id}: {:?}", r.error);
        assert_eq!(r.status.key(), "not_checked");
        let clashes = r
            .findings
            .iter()
            .filter(|f| f.rule.key() == "clash")
            .count();
        assert_eq!(clashes, 0, "{id}: {:?}", r.findings);
    }
    let s = g.m.steel_schedule(None).unwrap();
    assert!(s.bolt_count() >= 4 * 4 + 8 * 2 + 4);
    assert!(s.weld_length() > 0.0 && s.plate_mass() > 0.0);
    // a second request finds nothing left to place
    assert!(g
        .m
        .steel_conn_candidates(None, Some(g.l0))
        .unwrap()
        .is_empty());
    // the whole placement is one undo step
    g.m.undo().unwrap();
    assert!(g.m.steel_conns().unwrap().is_empty());
    assert_eq!(g.m.solids(None).unwrap().len(), solids0);
    let parts: i64 =
        g.m.sql()
            .query_row("SELECT count(*) FROM steel_conn_part", [], |r| r.get(0))
            .unwrap();
    assert_eq!(parts, 0, "undo leaves no part rows behind");
}

#[test]
fn connection_parts_take_plate_and_fastener_classes() {
    let mut g = gazebo();
    let all = g.m.steel_conn_candidates(None, Some(g.l0)).unwrap();
    let ids = g.m.add_steel_conns(&all).unwrap();
    let mut seen = std::collections::BTreeMap::<String, usize>::new();
    for id in ids {
        for sid in g.m.steel_conn_parts(id).unwrap() {
            let s = g.m.solid(sid).unwrap();
            // names are "kind conn role mark"
            let role = s.name.split(' ').nth(2).unwrap().to_string();
            let got = s.ifc.map(|k| format!("{} {}", k.class, k.predefined));
            let want = match role.as_str() {
                "plate" | "stiffener" | "gusset" | "cleat" | "packer" => {
                    Some("IfcPlate NOTDEFINED".to_string())
                }
                "bolt" => Some("IfcMechanicalFastener BOLT".into()),
                "anchor" => Some("IfcMechanicalFastener ANCHORBOLT".into()),
                "head" | "nut" | "washer" => Some("IfcMechanicalFastener NOTDEFINED".into()),
                _ => None,
            };
            assert_eq!(got, want, "{}", s.name);
            *seen.entry(role).or_default() += 1;
        }
    }
    for r in ["plate", "bolt"] {
        assert!(seen.contains_key(r), "no {r} part: {seen:?}");
    }
    // a connection solid written before the classes is rewritten with them
    let (cid, spec) = g.m.steel_conns().unwrap()[0].clone();
    let parts = g.m.steel_conn_parts(cid).unwrap();
    let k = parts
        .iter()
        .position(|s| g.m.solid(*s).unwrap().ifc.is_some())
        .expect("a classed part");
    let sid = parts[k];
    g.m.sql()
        .execute("UPDATE solid SET ifc_class=NULL, ifc_type=NULL WHERE id=?1", [sid])
        .unwrap();
    g.m.update_steel_conn(cid, &spec).unwrap();
    assert_eq!(g.m.steel_conn_parts(cid).unwrap()[k], sid, "rows are kept in place");
    assert!(g.m.solid(sid).unwrap().ifc.is_some());
}

#[test]
fn parts_follow_a_section_change_and_undo() {
    let mut g = gazebo();
    let bases =
        g.m.steel_conn_candidates(Some(ConnKind::Pinned), None)
            .unwrap();
    let id = g.m.add_steel_conn(&bases[0]).unwrap();
    let before = g.m.steel_conn_parts(id).unwrap().len();
    let plate_of = |m: &Model| {
        let r = m.steel_conn_report(id).unwrap();
        r.conn.unwrap().bolts.len()
    };
    let bolts0 = plate_of(&g.m);
    let deep = Shape::I {
        h: 0.5,
        b: 0.2,
        tw: 0.0102,
        tf: 0.016,
    };
    g.m.set_member_type(g.ring_type, "IPE 500", &deep, "S355")
        .unwrap();
    let after = g.m.steel_conn_parts(id).unwrap().len();
    assert!(plate_of(&g.m) > bolts0);
    assert!(after > before, "{before} -> {after}");
    assert_eq!(g.m.solids(None).unwrap().len(), after);
    assert!(steel_issues(&g.m).is_empty(), "{:#?}", steel_issues(&g.m));
    g.m.undo().unwrap();
    assert_eq!(g.m.steel_conn_parts(id).unwrap().len(), before);
    assert_eq!(g.m.solids(None).unwrap().len(), before);
}

#[test]
fn bad_requests_are_refused_and_lost_members_raise_an_issue() {
    let mut g = gazebo();
    let wrong = SteelConnSpec {
        level: g.l0,
        kind: ConnKind::Pinned,
        members: vec![("column".into(), g.cols[0])],
        params: Default::default(),
    };
    assert!(g.m.add_steel_conn(&wrong).is_err());
    let base = SteelConnSpec {
        level: g.l0,
        kind: ConnKind::BasePlate,
        members: vec![("column".into(), g.cols[0])],
        params: Default::default(),
    };
    let id = g.m.add_steel_conn(&base).unwrap();
    assert!(!g.m.steel_conn_parts(id).unwrap().is_empty());
    g.m.delete_column(g.cols[0]).unwrap();
    assert!(g.m.steel_conn_parts(id).unwrap().is_empty());
    assert_eq!(steel_issues(&g.m).len(), 1);
    g.m.delete_steel_conn(id).unwrap();
    assert!(steel_issues(&g.m).is_empty());
}

#[test]
fn a_pair_of_members_takes_one_connection_of_any_type() {
    let mut g = gazebo();
    let pinned = g.m.steel_conn_candidates(Some(ConnKind::Pinned), None).unwrap();
    let pin = pinned[0].clone();
    let id = g.m.add_steel_conn(&pin).unwrap();
    // the same pair as another type is refused with the existing connection named
    let rigid = SteelConnSpec {
        kind: ConnKind::RigidEndPlate,
        ..pin.clone()
    };
    let e = g.m.add_steel_conn(&rigid).unwrap_err().to_string();
    assert!(e.contains("already have connection") && e.contains("pinned"), "{e}");
    let mut swapped = rigid.clone();
    swapped.members.reverse();
    assert!(g.m.add_steel_conn(&swapped).is_err());
    // an auto request never offers that pair again, as any type
    let left = g.m.steel_conn_candidates(None, None).unwrap();
    assert!(left
        .iter()
        .all(|s| { let mut a = s.members.clone(); a.sort(); let mut b = pin.members.clone(); b.sort(); a != b }));
    // two specs of one batch for one pair are refused too, nothing written
    let n = g.m.steel_conns().unwrap().len();
    let other = left.iter().find(|s| s.kind == ConnKind::Pinned).unwrap().clone();
    let twin = SteelConnSpec { kind: ConnKind::RigidEndPlate, ..other.clone() };
    assert!(g.m.add_steel_conns(&[other, twin]).is_err());
    assert_eq!(g.m.steel_conns().unwrap().len(), n);
    // editing a connection to its own pair is fine
    g.m.update_steel_conn(id, &pin).unwrap();
}

#[test]
fn an_unreadable_row_does_not_stop_the_others_from_being_rebuilt() {
    let mut g = gazebo();
    let bases = g.m.steel_conn_candidates(Some(ConnKind::BasePlate), None).unwrap();
    let ids = g.m.add_steel_conns(&bases).unwrap();
    g.m.sql()
        .execute("UPDATE steel_conn SET kind='nonsense' WHERE id=?1", [ids[0]])
        .unwrap();
    // a section change regenerates every connection: the broken row is reported, the rest build
    g.m.set_member_type(
        g.ring_type,
        "IPE 220",
        &Shape::I { h: 0.22, b: 0.11, tw: 0.0059, tf: 0.0092 },
        "S355",
    )
    .unwrap();
    for id in &ids[1..] {
        assert!(!g.m.steel_conn_parts(*id).unwrap().is_empty(), "{id}");
    }
    let issues = steel_issues(&g.m);
    assert!(issues.iter().any(|i| i.starts_with(&ids[0].to_string())), "{issues:?}");
}
