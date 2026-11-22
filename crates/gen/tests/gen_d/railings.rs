// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Guard rails by rule: both sides of every stair, the flat roof deck behind a low parapet, none
//! when the rule is off; a pitched roof gets no deck guard.

use ddc_gen::project::railings::RailOn;
use ddc_gen::project::{build_project, parse_spec, slots, GProject};
use serde_json::json;

fn build(extra: serde_json::Value) -> GProject {
    let mut v = json!({"typology": "apartment_section", "floors": 5});
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    build_project(&parse_spec(&v).unwrap()).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn every_stair_has_two_guards_and_the_flat_roof_one_ring() {
    let p = build(json!({}));
    for s in &p.storeys {
        let on_stair: Vec<(usize, u8)> = s
            .railings
            .iter()
            .filter_map(|r| match r.on {
                RailOn::Stair { stair, side } => Some((stair, side)),
                _ => None,
            })
            .collect();
        let want: Vec<(usize, u8)> = (0..s.layout.stairs.len()).flat_map(|i| [(i, 0), (i, 1)]).collect();
        assert_eq!(on_stair, want, "{}", s.name);
        assert!(s.railings.iter().all(|r| (r.height - 1.1).abs() < 1e-9));
        assert!(s.railings.len() <= slots::RAILINGS_MAX as usize);
    }
    assert!(p.storeys.iter().any(|s| !s.layout.stairs.is_empty()));
    let top = p.storeys.last().unwrap();
    let ring: Vec<_> = top
        .railings
        .iter()
        .filter_map(|r| match &r.on {
            RailOn::Path(q) => Some(q),
            _ => None,
        })
        .collect();
    assert_eq!(ring.len(), 1, "one guard on the roof deck");
    let q = ring[0];
    assert!(q.len() >= 5 && q.first() == q.last(), "a closed ring: {q:?}");
    let z = p.roof.elevation - top.elevation;
    assert!(q.iter().all(|x| (x.2 - z).abs() < 1e-9));
    // inside the roof outline
    let (lo, hi) = p.roof.ring.iter().fold((f64::MAX, f64::MIN), |a, r| (a.0.min(r.0), a.1.max(r.0)));
    assert!(q.iter().all(|x| x.0 > lo && x.0 < hi), "{q:?}");
    // the lower storeys hold no roof guard
    for s in &p.storeys[..p.storeys.len() - 1] {
        assert!(s.railings.iter().all(|r| matches!(r.on, RailOn::Stair { .. })), "{}", s.name);
    }
}

#[test]
fn the_rules_switch_guards_off_and_set_their_height() {
    let off = build(json!({"rules": {"egress.railings": false}}));
    assert!(off.storeys.iter().all(|s| s.railings.is_empty()));
    let no_roof = build(json!({"rules": {"egress.roof_guard": false}}));
    assert!(no_roof.storeys.last().unwrap().railings.iter().all(|r| matches!(r.on, RailOn::Stair { .. })));
    let tall = build(json!({"rules": {"egress.railing_height": 1.2}}));
    assert!(tall.storeys.iter().flat_map(|s| &s.railings).all(|r| (r.height - 1.2).abs() < 1e-9));
    let pitched = build(json!({"roof": {"kind": "pitched"}}));
    assert!(pitched.storeys.last().unwrap().railings.iter().all(|r| matches!(r.on, RailOn::Stair { .. })));
}
