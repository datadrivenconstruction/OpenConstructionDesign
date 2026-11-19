use std::collections::BTreeMap;

use ddc_geom::analytical::Release;
use ddc_geom::member::Shape;
use serde_json::{json, Value as J};

use crate::{AnOverride, AnalyticalAdj, BeamJoin, BeamSpec, ColumnSpec, FootingSpec, FootingStep};
use crate::{Just, Model};

struct Portal {
    m: Model,
    c0: i64,
    c1: i64,
    b: i64,
    f: i64,
}

fn portal() -> Portal {
    let mut m = Model::open(":memory:").unwrap();
    let l0 = m.add_level("L0", 0.0).unwrap();
    let l1 = m.add_level("L1", 3.0).unwrap();
    let ct = m.add_member_type("RC 400", &Shape::Rect { b: 0.4, h: 0.4 }, "Concrete").unwrap();
    let bt =
        m.add_member_type("RC 300x500", &Shape::Rect { b: 0.3, h: 0.5 }, "Concrete").unwrap();
    let col = |x: f64| ColumnSpec {
        level: l0,
        top_level: Some(l1),
        member_type: ct,
        structural: true,
        at: (x, 0.0),
        base_offset: 0.0,
        top_offset: 0.0,
        rotation: 0.0,
        top_at: None,
    };
    let c0 = m.add_column(&col(0.0)).unwrap();
    let c1 = m.add_column(&col(6.0)).unwrap();
    let b = m.add_beam(&beam(l0, bt, (0.0, 0.0, 3.0), (6.0, 0.0, 3.0))).unwrap();
    let f = m
        .add_footing(&FootingSpec {
            level: l0,
            column: Some(c0),
            at: (0.0, 0.0),
            rotation: 0.0,
            round: false,
            steps: vec![FootingStep { w: 1.5, l: 1.5, h: 0.5 }],
            top_offset: 0.0,
            material: "Concrete".into(),
        })
        .unwrap();
    Portal { m, c0, c1, b, f }
}

fn beam(level: i64, t: i64, a: (f64, f64, f64), b: (f64, f64, f64)) -> BeamSpec {
    BeamSpec {
        level,
        member_type: t,
        a,
        b,
        bulge: 0.0,
        yjust: Just::Center,
        zjust: Just::Center,
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::Auto,
        join_end: BeamJoin::Auto,
    }
}

fn ov(target: &str, field: &str, value: J) -> AnOverride {
    AnOverride {
        id: 0,
        target: target.into(),
        field: field.into(),
        value,
        anchor: None,
        locked: false,
        src: "manual".into(),
        why: None,
    }
}

/// key -> rounded position, for every node.
fn key_map(m: &Model) -> BTreeMap<String, [i64; 3]> {
    let a = m.analytical().unwrap();
    let r = |x: f64| (x * 1e6).round() as i64;
    a.node_keys
        .iter()
        .zip(&a.nodes)
        .map(|(k, p)| (k.clone(), [r(p.x), r(p.y), r(p.z)]))
        .collect()
}

#[test]
fn keys_come_from_physical_ids_and_survive_unrelated_edits() {
    let Portal { mut m, c0, c1, b, f } = portal();
    let a = m.analytical().unwrap();
    let col0 = a.members.iter().find(|x| x.kind == "column" && x.id == c0).unwrap();
    assert_eq!(col0.key, format!("m:column:{c0}"));
    // the column owns its top node: prio 0 beats the beam's prio 1
    assert_eq!(a.node_keys[col0.nodes[1]], format!("n:m:column:{c0}:end"));
    assert_eq!(a.node_keys[col0.nodes[0]], format!("n:m:column:{c0}:start"));
    assert_eq!(a.supports[0].key, format!("s:iso_footing:{f}"));
    assert!(a.members.iter().any(|x| x.key == format!("m:beam:{b}")));
    assert!(a.members.iter().any(|x| x.key == format!("m:column:{c1}")));
    let mut uniq = a.node_keys.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), a.node_keys.len());
    let before = key_map(&m);
    // an unrelated beam on another level far away
    let l9 = m.add_level("L9", 30.0).unwrap();
    let t = m.add_member_type("S", &Shape::Rect { b: 0.2, h: 0.2 }, "Steel").unwrap();
    m.add_beam(&beam(l9, t, (50.0, 50.0, 30.0), (56.0, 50.0, 30.0))).unwrap();
    let after = key_map(&m);
    for (k, p) in &before {
        assert_eq!(after.get(k), Some(p), "{k}");
    }
    assert_eq!(after.len(), before.len() + 2);
    m.undo().unwrap();
    assert_eq!(key_map(&m), before);
}

#[test]
fn overrides_win_over_analytical_adj_and_are_one_undo_step() {
    let Portal { mut m, b, .. } = portal();
    let adj = AnalyticalAdj {
        release_start: Some(Release::Pinned),
        ..Default::default()
    };
    m.set_analytical_adj("beam", &[b], adj).unwrap();
    let key = format!("m:beam:{b}");
    let a = m.analytical().unwrap();
    let bm = &a.members[a.member_of(&key).unwrap()];
    assert_eq!(bm.releases[0], [false, false, false, false, true, true]);
    assert_eq!(bm.release.0, Release::Pinned);
    m.set_an_override(&ov(&key, "release_start", json!({"mask": "FFFFFF"}))).unwrap();
    let a = m.analytical().unwrap();
    let bm = &a.members[a.member_of(&key).unwrap()];
    assert_eq!(bm.releases[0], [false; 6]);
    assert_eq!(bm.release.0, Release::Fixed);
    assert_eq!(a.overrides.len(), 1);
    assert_eq!(a.overrides[0].status, "applied");
    // a partial release: torsion free at the end
    m.set_an_override(&ov(&key, "release_end", json!({"mask": "FFFPFF"}))).unwrap();
    let a = m.analytical().unwrap();
    let bm = &a.members[a.member_of(&key).unwrap()];
    assert_eq!(bm.releases[1], [false, false, false, true, false, false]);
    m.undo().unwrap();
    m.undo().unwrap();
    let a = m.analytical().unwrap();
    assert!(a.overrides.is_empty());
    assert_eq!(a.members[a.member_of(&key).unwrap()].release.0, Release::Pinned);
}

#[test]
fn missing_targets_are_reported_and_kept() {
    let Portal { mut m, b, .. } = portal();
    m.set_an_override(&ov("m:beam:999", "role", json!("excluded"))).unwrap();
    let mut locked = ov("p:slab:77", "role", json!("excluded"));
    locked.locked = true;
    m.set_an_override(&locked).unwrap();
    let key = format!("m:beam:{b}");
    m.set_an_override(&ov(&key, "role", json!("excluded"))).unwrap();
    let a = m.analytical().unwrap();
    let st: Vec<&str> = a.overrides.iter().map(|o| o.status).collect();
    assert_eq!(st, ["orphaned", "conflicting", "applied"]);
    assert!(a.issues.iter().any(|q| q.code == "orphan_override" && q.kind == "override"));
    assert!(a.issues.iter().any(|q| q.code == "locked_conflict"));
    assert_eq!(a.members[a.member_of(&key).unwrap()].role, "excluded");
    // the beam is deleted: its override becomes an orphan, the row stays
    m.delete_beam(b).unwrap();
    let a = m.analytical().unwrap();
    assert_eq!(a.overrides.iter().filter(|o| o.status == "orphaned").count(), 2);
    assert_eq!(m.an_overrides().unwrap().len(), 3);
}

#[test]
fn user_supports_dofs_springs_and_disabled_footings() {
    let Portal { mut m, c0, c1, f, .. } = portal();
    let base1 = format!("n:m:column:{c1}:start");
    let sk = format!("s:{base1}");
    m.set_an_override(&ov(&sk, "dofs", json!("FFFPPP"))).unwrap();
    m.set_an_override(&ov(&sk, "springs", json!([0, 0, 0, 0, 0, 5e6]))).unwrap();
    m.set_an_override(&ov(&format!("s:iso_footing:{f}"), "enabled", json!(false))).unwrap();
    let a = m.analytical().unwrap();
    assert_eq!(a.supports.len(), 1, "{:?}", a.supports);
    let s = &a.supports[0];
    assert_eq!(s.key, sk);
    assert_eq!(a.node_keys[s.node], base1);
    assert_eq!(s.fixed, [true, true, true, false, false, false]);
    assert_eq!(s.kind, Release::Pinned);
    assert_eq!(s.springs[5], 5e6);
    assert!(!a.supports.iter().any(|s| s.from.0 == "iso_footing"));
    let _ = c0;
}

#[test]
fn node_move_split_reanchor_and_member_fields() {
    let Portal { mut m, c0, c1, b, .. } = portal();
    let top1 = format!("n:m:column:{c1}:end");
    m.set_an_override(&ov(&top1, "move", json!({"d": [0.0, 0.0, 0.25]}))).unwrap();
    let a = m.analytical().unwrap();
    let n = a.node_of(&top1).unwrap();
    assert!((a.nodes[n].z - 3.25).abs() < 1e-9, "{:?}", a.nodes[n]);
    // split the beam off the first column top: one more node, the beam start is free
    let top0 = format!("n:m:column:{c0}:end");
    let n0 = a.nodes.len();
    m.set_an_override(&ov(&top0, "split", json!(true))).unwrap();
    let a = m.analytical().unwrap();
    assert_eq!(a.nodes.len(), n0 + 1);
    let bm = &a.members[a.member_of(&format!("m:beam:{b}")).unwrap()];
    assert_ne!(bm.nodes[0], a.node_of(&top0).unwrap());
    assert_eq!(a.node_keys[bm.nodes[0]], format!("n:m:beam:{b}:start"));
    // an override on a key that no longer exists is found again by its anchor
    let mut o = ov("n:m:column:9999:end", "move", json!({"d": [0.0, 0.0, 0.0]}));
    o.anchor = Some(json!({"at": [6.0, 0.0, 3.25], "ends": [format!("m:column:{c1}:end")]}));
    m.set_an_override(&o).unwrap();
    let a = m.analytical().unwrap();
    let u = a.overrides.iter().find(|u| u.target == o.target).unwrap();
    assert_eq!(u.status, "re_anchored");
    // eccentricity in local axes, inline section, truss role
    let key = format!("m:beam:{b}");
    let ecc = json!({"frame": "local", "d": [0.0, 0.0, 0.2]});
    m.set_an_override(&ov(&key, "ecc_start", ecc)).unwrap();
    let sec = json!({"A": 0.01, "Iy": 2e-5, "Iz": 1e-5, "J": 3e-6});
    m.set_an_override(&ov(&key, "section", sec)).unwrap();
    m.set_an_override(&ov(&key, "role", json!("truss"))).unwrap();
    let a = m.analytical().unwrap();
    let bm = &a.members[a.member_of(&key).unwrap()];
    // beam along +X: local z is global +Z
    assert!((bm.ecc[0].z - 0.2).abs() < 1e-12 && bm.ecc[0].x.abs() < 1e-12);
    let s = &a.sections[bm.section.unwrap()];
    assert_eq!((s.name.as_str(), s.area, s.props.j), ("engineer-defined", 0.01, 3e-6));
    assert_eq!(bm.releases[0], [false, false, false, false, true, true]);
    assert_eq!(bm.releases[1], [false, false, false, true, true, true]);
    let j = a.to_json();
    assert_eq!(j["version"], 2);
    assert_eq!(j["members"][0]["key"].as_str().unwrap().split(':').count(), 3);
    assert!(j["nodes"][0]["key"].as_str().unwrap().starts_with("n:"));
}

#[test]
fn unknown_material_is_a_blocking_issue() {
    let Portal { mut m, .. } = portal();
    let l = m.add_level("L5", 15.0).unwrap();
    let t = m.add_member_type("X", &Shape::Rect { b: 0.2, h: 0.2 }, "Mystery stuff").unwrap();
    let id = m.add_beam(&beam(l, t, (0.0, 20.0, 15.0), (4.0, 20.0, 15.0))).unwrap();
    let a = m.analytical().unwrap();
    assert!(a.issues.iter().any(|q| q.code == "no_material" && q.id == id));
    assert!(!a.issues.iter().any(|q| q.code == "no_material" && q.id != id));
}
