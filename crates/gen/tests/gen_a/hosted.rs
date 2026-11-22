// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Wall-hosted fixtures and openings stand on their own walls in the final project: the briefs of
//! the live runs that failed to apply (a villa on a slope, a U-shaped mixed-use block) and their
//! neighbours build without a hosted piece past its wall's end.

use ddc_gen::project::{build_project, check_project, parse_spec};
use serde_json::{json, Value as J};

/// Gate failures about hosted pieces, or the build error, of one brief.
fn hosted_problems(v: &J) -> Result<Vec<String>, String> {
    let s = parse_spec(v).map_err(|e| e.to_string())?;
    let p = build_project(&s).map_err(|e| e.to_string())?;
    Ok(check_project(&s, &p)
        .failures
        .into_iter()
        .filter(|f| f.contains("fixture") || f.contains("opening"))
        .collect())
}

fn sweep(briefs: &[J]) -> (usize, Vec<String>) {
    let (mut built, mut bad) = (0, vec![]);
    for v in briefs {
        match hosted_problems(v) {
            Ok(f) if f.is_empty() => built += 1,
            Ok(f) => bad.push(format!("{v}: {f:?}")),
            Err(e) if e.contains("fixture") || e.contains("opening") => bad.push(format!("{v}: {e}")),
            Err(e) => eprintln!("not built: {v}: {e}"),
        }
    }
    (built, bad)
}

fn villa(extra: J) -> J {
    let mut v = json!({"typology": "house", "floors": 2, "floor_height": 3.0,
        "footprint": {"width": 10, "depth": 11},
        "roof": {"kind": "hip", "slope": 35, "overhang": 0.6},
        "stair": {"max_riser": 0.17},
        "origin": [14, 10],
        "site": {"plot": [[0, 0], [40, 0], [40, 30], [0, 30]],
                 "terrain": {"slope": 0.1, "downhill": [0, 1]}}});
    ddc_gen::project::merge_patch(&mut v, &extra);
    v
}

fn villa_rooms() -> J {
    json!([{"kind": "living", "area": 30}, {"kind": "kitchen", "area": 12},
        {"kind": "bedroom", "area": 16}, {"kind": "bedroom", "area": 12}, {"kind": "bedroom", "area": 12},
        {"kind": "bath", "area": 6}, {"kind": "wc", "area": 3}])
}

/// The live villa brief: two storeys on a 3 m fall, hip roof, low risers, the terrain modelled.
#[test]
fn the_sloped_villa_hosts_every_piece_on_its_wall() {
    let mut briefs = vec![villa(json!({})), villa(json!({"rooms": villa_rooms()}))];
    for (w, d) in [(10, 11.0), (11, 10.0), (12, 9.5), (14, 8.0), (9, 12.0)] {
        for origin in [json!([14, 10]), json!([0, 0]), json!(null)] {
            for terrain in [json!({"slope": 0.1, "downhill": [0, 1]}), json!({"slope": 0.2, "downhill": [1, 0]}), json!(null)] {
                briefs.push(villa(json!({"footprint": {"width": w, "depth": d}, "origin": origin,
                    "site": {"terrain": terrain}})));
            }
        }
    }
    let (built, bad) = sweep(&briefs);
    assert!(bad.is_empty(), "{} of {} briefs host a piece off its wall:\n{}", bad.len(), briefs.len(), bad.join("\n"));
    assert!(built * 2 >= briefs.len(), "only {built} of {} built", briefs.len());
}

/// The origin moves every column and fixture with the walls, once.
#[test]
fn the_origin_moves_columns_and_fixtures_once() {
    let at = |origin: J| {
        let s = parse_spec(&villa(json!({"origin": origin, "site": null}))).unwrap();
        build_project(&s).unwrap()
    };
    let (p0, p1) = (at(json!([0, 0])), at(json!([14, 10])));
    let near = |a: (f64, f64), b: (f64, f64)| (a.0 + 14.0 - b.0).abs() < 1e-6 && (a.1 + 10.0 - b.1).abs() < 1e-6;
    for (s0, s1) in p0.storeys.iter().zip(&p1.storeys) {
        let (l0, l1) = (&s0.layout, &s1.layout);
        assert!(!l0.fixtures.is_empty());
        assert_eq!(l0.fixtures.len(), l1.fixtures.len());
        for (f0, f1) in l0.fixtures.iter().zip(&l1.fixtures) {
            assert!(near(f0.at, f1.at), "{} {}: {:?} -> {:?}", f0.family, f0.ty, f0.at, f1.at);
        }
        for (c0, c1) in l0.columns.iter().zip(&l1.columns) {
            assert!(near(c0.at, c1.at), "column {:?} -> {:?}", c0.at, c1.at);
        }
        for (w0, w1) in l0.walls.iter().zip(&l1.walls) {
            assert!(near(w0.a, w1.a) && near(w0.b, w1.b));
        }
    }
}

fn mixed(extra: J) -> J {
    let mut v = json!({"typology": "mixed_use", "floors": 7,
        "design": {"massing": "u", "facade": "curtain"}});
    ddc_gen::project::merge_patch(&mut v, &extra);
    v
}

/// The live mixed-use brief: seven storeys, shops below offices, U around a court, curtain wall.
#[test]
fn the_u_shaped_mixed_use_hosts_every_piece_on_its_wall() {
    let mut briefs = vec![mixed(json!({}))];
    for w in [28, 30, 32, 35, 36, 40, 44] {
        for depth in [json!(16), json!(14), json!(null)] {
            briefs.push(mixed(json!({"footprint": {"width": w, "depth": depth}})));
        }
        briefs.push(mixed(json!({"footprint": {"width": w, "depth": 16}, "band_depth": 7.1, "corridor": 1.8})));
        briefs.push(mixed(json!({"footprint": {"width": w}, "design": {"massing": "bar"}})));
        briefs.push(mixed(json!({"footprint": {"width": w}, "design": {"massing": "l"}})));
    }
    let (built, bad) = sweep(&briefs);
    assert!(bad.is_empty(), "{} of {} briefs host a piece off its wall:\n{}", bad.len(), briefs.len(), bad.join("\n"));
    assert!(built * 2 >= briefs.len(), "only {built} of {} built", briefs.len());
}

/// Three long wings carry more columns than one storey's own slot block holds: the block
/// still builds and passes its gates.
#[test]
fn a_long_u_shaped_block_finds_a_layout() {
    let mut most = 0;
    for w in [36, 44] {
        let v = mixed(json!({"footprint": {"width": w, "depth": 16}}));
        let s = parse_spec(&v).unwrap();
        let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
        let g = check_project(&s, &p);
        assert!(g.passed(), "{v}: {:?}", g.failures);
        most = most.max(p.storeys.iter().map(|s| s.layout.columns.len()).max().unwrap_or(0));
    }
    let own = (ddc_gen::project::slots::PER_STOREY - ddc_gen::project::slots::COLUMNS) as usize;
    assert!(most > own, "{most} columns: the case past one block is not exercised");
}
