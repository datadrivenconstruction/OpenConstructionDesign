//! The 300 m lattice tower takes gusset plates where its braces end: on the leg columns
//! (bracing gussets) and where only braces meet, at the X crossings (truss gussets).

use ddc_geom::steel::{tower, ConnKind};
use ddc_model::{Model, SteelConnSpec};

#[test]
fn the_300_m_tower_takes_gussets_at_its_brace_nodes() {
    let f = tower::lattice_tower_300(&tower::TowerParams::default()).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L0", 0.0).unwrap();
    let ids = m.add_steel_frame(lv, &f).unwrap();
    assert!(!ids.braces.is_empty() && !ids.columns.is_empty());
    let on_braces: Vec<SteelConnSpec> = m
        .steel_conn_candidates(None, None)
        .unwrap()
        .into_iter()
        .filter(|s| s.members.iter().any(|x| x.0 == "brace"))
        .collect();
    let of = |k: ConnKind| on_braces.iter().filter(|s| s.kind == k).cloned().collect::<Vec<_>>();
    let (on_cols, crossings) = (of(ConnKind::BracingGusset), of(ConnKind::TrussGusset));
    assert!(on_cols.len() >= 100, "{} bracing gussets", on_cols.len());
    assert!(!crossings.is_empty(), "X crossings get a plate");
    assert!(on_cols.iter().all(|s| s.members[0].0 == "column" && s.members[1..].iter().all(|x| x.0 == "brace")));
    // each brace end is joined once
    let mut ends: Vec<(i64, usize)> = vec![];
    for s in &on_braces {
        for (k, id) in s.members.iter().skip(1) {
            assert_eq!(k, "brace");
            ends.push((*id, 0));
        }
    }
    let count = |id: i64| ends.iter().filter(|e| e.0 == id).count();
    assert!(ends.iter().all(|e| count(e.0) <= 2), "a brace has two ends");
    let pick: Vec<SteelConnSpec> = on_cols.iter().take(80).chain(crossings.iter().take(40)).cloned().collect();
    let cids = m.add_steel_conns(&pick).unwrap();
    assert_eq!(cids.len(), pick.len());
    let parts: usize = cids.iter().map(|id| m.steel_conn_parts(*id).unwrap().len()).sum();
    assert!(parts >= 200, "{parts} parts");
    for id in &cids {
        let r = m.steel_conn_report(*id).unwrap();
        assert!(r.error.is_none(), "{id}: {:?}", r.error);
    }
    // the placed nodes are not offered again
    let left = m.steel_conn_candidates(Some(ConnKind::BracingGusset), None).unwrap();
    assert_eq!(left.iter().filter(|s| s.members.iter().any(|x| x.0 == "brace")).count(), on_cols.len() - 80);
    // regeneration rebuilds only what changed: one more connection is one build, not 121
    let extra = left.iter().find(|s| s.members.iter().any(|x| x.0 == "brace")).unwrap().clone();
    let before = ddc_model::steel_conn_rebuilds();
    let id = m.add_steel_conn(&extra).unwrap();
    assert_eq!(ddc_model::steel_conn_rebuilds() - before, 1);
    assert!(!m.steel_conn_parts(id).unwrap().is_empty());
    // undo takes the new one away and keeps the others' parts
    m.undo().unwrap();
    let after: usize = cids.iter().map(|id| m.steel_conn_parts(*id).unwrap().len()).sum();
    assert_eq!(after, parts);
}

/// The tower at full detail needs at least 500 connections over 10 000 members, 2 000
/// of them braces; the default 300 m tower offers that many joints by construction.
#[test]
fn the_300_m_tower_meets_the_t01_member_and_joint_counts() {
    let f = tower::lattice_tower_300(&tower::TowerParams::default()).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L0", 0.0).unwrap();
    let ids = m.add_steel_frame(lv, &f).unwrap();
    assert!(ids.columns.len() + ids.beams.len() + ids.braces.len() >= 10_000, "{} members", f.members.len());
    assert!(ids.braces.len() >= 2_000, "{} braces", ids.braces.len());
    let offered = m.steel_conn_candidates(None, None).unwrap();
    assert!(offered.len() >= 500, "{} connections offered", offered.len());
}
