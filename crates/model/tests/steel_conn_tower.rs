// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The 300 m lattice tower takes gusset plates where its braces end: on the leg columns
//! (bracing gussets), on the platform truss chords (bracing gussets on beams, where no leg
//! passes) and where only braces meet, at the X crossings (truss gussets).

use ddc_geom::steel::lattice::MemRole;
use ddc_geom::steel::{tower, ConnKind};
use ddc_geom::V3;
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
    let (gussets, crossings) = (of(ConnKind::BracingGusset), of(ConnKind::TrussGusset));
    assert!(!crossings.is_empty(), "X crossings get a plate");
    // a bracing gusset sits on a column or a beam (the platform truss chords), then braces
    let host_is = |s: &SteelConnSpec, k: &str| s.members[0].0 == k && s.members[1..].iter().all(|x| x.0 == "brace");
    assert!(gussets.iter().all(|s| host_is(s, "column") || host_is(s, "beam")));
    let on_cols: Vec<SteelConnSpec> = gussets.iter().filter(|s| host_is(*s, "column")).cloned().collect();
    // from the frame: the leg column axes and each brace's ends, in the order the model adds them
    let column = |r: MemRole, a: V3, b: V3| {
        (matches!(r, MemRole::Column) || (matches!(r, MemRole::Chord) && f.chords_are_columns)) && (a.z - b.z).abs() > 1e-3
    };
    let (mut legs, mut brace_ends) = (vec![], vec![]);
    for m in &f.members {
        let (a, b) = (f.nodes[m.a as usize], f.nodes[m.b as usize]);
        if column(m.role, a, b) {
            legs.push((a, b));
        } else if !matches!(m.role, MemRole::Beam | MemRole::Girder) {
            brace_ends.push([a, b]);
        }
    }
    assert_eq!(brace_ends.len(), ids.braces.len());
    let on_leg = |p: V3| {
        legs.iter().any(|&(a, b)| {
            let d = b - a;
            let t = ((p - a).dot(d) / d.dot(d)).clamp(0.0, 1.0);
            (a + d * t - p).len() < 1e-6
        })
    };
    // every brace end on a leg column axis is joined by a gusset on the column, never on a beam
    // a brace in line with a leg is spliced to it: that is its column connection
    let splices = of(ConnKind::ColumnSplice);
    let joined = |id: i64, host: &str| {
        gussets
            .iter()
            .chain(if host == "column" { &splices[..] } else { &splices[..0] })
            .filter(|s| host_is(*s, host) && s.members[1..].iter().any(|x| x.1 == id))
            .count()
    };
    // no end goes without a connection silently: the audit names any the search left out
    let audit = m.steel_conn_audit().unwrap();
    // every class of refusal by host kind and reason (numbers stripped), with a count each
    let mut classes = std::collections::BTreeMap::<String, usize>::new();
    for s in &audit {
        let reason: String = s.reason.chars().map(|c| if c.is_ascii_digit() || c == '.' { '#' } else { c }).collect();
        let host = s.host.as_ref().map_or("none", |h| h.0.as_str());
        *classes.entry(format!("{host}: {reason}")).or_default() += 1;
    }
    assert!(audit.is_empty(), "{} brace ends without a connection: {classes:#?}; first {:?}", audit.len(), &audit[..audit.len().min(3)]);
    let mut leg_ends = 0;
    for (id, e) in ids.braces.iter().zip(&brace_ends) {
        let n = e.iter().filter(|p| on_leg(**p)).count();
        leg_ends += n;
        assert!(
            joined(*id, "column") >= n,
            "brace {id} {:?}: {n} ends on a leg, {} column gussets, {} beam gussets, {} truss gussets",
            e,
            joined(*id, "column"),
            joined(*id, "beam"),
            crossings.iter().filter(|s| s.members.iter().any(|x| x.1 == *id)).count()
        );
        if n == 2 {
            assert_eq!(joined(*id, "beam"), 0, "brace {id} runs leg to leg but sits on a beam");
        }
    }
    let col_refs: usize = on_cols.iter().chain(splices.iter().filter(|s| host_is(s, "column"))).map(|s| s.members.len() - 1).sum();
    assert!(col_refs >= leg_ends && leg_ends > 80, "{col_refs} brace ends on column gussets, {leg_ends} on leg axes");
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
    assert_eq!(left.iter().filter(|s| s.members.iter().any(|x| x.0 == "brace")).count(), gussets.len() - 80);
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
