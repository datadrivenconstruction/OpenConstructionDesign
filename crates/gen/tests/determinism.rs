// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The same brief gives the same building: the free layout search spends a node budget, not
//! wall time, so a cold build twice hashes the same. One test per binary: the zone store is
//! shared by the threads of a test binary.

use ddc_gen::project::{build_project, parse_spec, steer};
use serde_json::json;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[test]
fn the_same_brief_gives_the_same_layout_hash_twice() {
    let brief = json!({"typology": "office", "floors": 3, "footprint": {"width": 30},
                       "rules": {"layout.strategy": "free"},
                       "rooms": [{"kind": "office", "area": 30, "count": 4}, {"kind": "meeting", "area": 40},
                                 {"kind": "wc", "area": 4, "count": 2}]});
    let spec = parse_spec(&brief).unwrap_or_else(|e| panic!("{e}"));
    let hash = || {
        steer::forget();
        let p = build_project(&spec).unwrap_or_else(|e| panic!("{e}"));
        let mut h = DefaultHasher::new();
        for s in &p.storeys {
            format!("{:?}", s.layout).hash(&mut h);
        }
        h.finish()
    };
    let (a, b) = (hash(), hash());
    assert_eq!(a, b, "two cold builds of one brief differ");
    assert_eq!(steer::capped(), 0, "a zone search was stopped by the clock, not its node budget");
}
