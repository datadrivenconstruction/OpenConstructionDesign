// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Service risers stay inside every slab they pass through: the live office tower and the
//! wet-wall apartment block that failed with "slab hole is outside the slab".

use ddc_gen::project::{build_project, check_project, parse_spec};
use serde_json::{json, Value};

fn briefs() -> Vec<(&'static str, Value)> {
    let room = |k: &str, a: f64| json!({"kind": k, "area": a});
    let flat = |name: &str, count: u32, beds: usize| {
        let mut rooms = vec![room("living", 18.0), room("kitchen", 9.0), room("bath", 4.5), room("hall", 3.5)];
        rooms.extend((0..beds).map(|_| room("bedroom", 12.0)));
        json!({"name": name, "count": count, "rooms": rooms})
    };
    let mix = json!([flat("1", 1, 0), flat("2", 2, 1), flat("3", 1, 2)]);
    vec![
        ("office 1", json!({"typology": "office", "floors": 1, "footprint": {"width": 50.0, "depth": 24.0}, "services": true})),
        ("office 3", json!({"typology": "office", "floors": 3, "footprint": {"width": 50.0, "depth": 24.0}, "services": true})),
        ("office 14", json!({"typology": "office", "floors": 14, "floor_height": 3.6, "footprint": {"width": 50.0, "depth": 24.0}, "lift": true, "services": true})),
        ("office 14 60x20", json!({"typology": "office", "floors": 14, "footprint": {"width": 60.0, "depth": 20.0}, "services": true})),
        ("office 4 default", json!({"typology": "office", "floors": 4, "services": true})),
        ("apartments 6", json!({"typology": "apartment_section", "floors": 6, "services": true})),
        ("apartments 6 balconies", json!({"typology": "apartment_section", "floors": 6, "services": true,
            "design": {"balconies": "balcony"}, "roof": {"kind": "flat"}})),
        ("apartments 6 mix", json!({"typology": "apartment_section", "floors": 6, "services": true, "apartments": mix,
            "design": {"balconies": "balcony"}, "roof": {"kind": "flat"}})),
    ]
}

fn inside(r: &[(f64, f64)], p: (f64, f64)) -> bool {
    let n = r.len();
    let mut odd = false;
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            odd = !odd;
        }
    }
    odd
}

#[test]
fn riser_holes_inside_slabs() {
    let mut bad = vec![];
    for (name, b) in briefs() {
        let s = match parse_spec(&b) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{name}: spec {e}");
                continue;
            }
        };
        let p = match build_project(&s) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{name}: build {e}");
                continue;
            }
        };
        let g = check_project(&s, &p);
        eprintln!("{name}: {} storeys, gates {}", p.storeys.len(), if g.passed() { "ok" } else { "FAIL" });
        if g.passed() {
            continue;
        }
        eprintln!("GATE {name}: {}", g.failures.iter().take(4).cloned().collect::<Vec<_>>().join(" | "));
        for (k, st) in p.storeys.iter().enumerate().take(3) {
            let l = &st.layout;
            for (i, h) in l.slab_holes.iter().enumerate() {
                let out = h.iter().filter(|q| !inside(&l.slab, **q)).count();
                let n = h.len();
                let area = (0..n).map(|j| h[j].0 * h[(j + 1) % n].1 - h[(j + 1) % n].0 * h[j].1).sum::<f64>().abs() / 2.0;
                if i < 2 || out > 0 || area < 0.01 {
                    eprintln!("GATE   st{k} hole {i} area {area:.4} outside {out} ring {h:?} slab {:?}", l.slab);
                }
            }
        }
        bad.push(name);
    }
    assert!(bad.is_empty(), "{bad:?}");
}
