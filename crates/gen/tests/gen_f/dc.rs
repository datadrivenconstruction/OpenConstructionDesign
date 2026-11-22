// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The data centre fit-out against the live hard scenario: about 600 racks in three halls,
//! cooling units along the halls, chillers N+1, UPS and batteries, generators outside, trays
//! over the racks and a dual-path chilled water network, every run end joined.

use ddc_gen::project::dc::{self, GDc, Kind};
use ddc_gen::project::rules::{self, defs};
use ddc_gen::project::{build_project, parse_spec, GProject, ProjectSpec};
use serde_json::{json, Value as J};

fn brief(rules: J, hall: &str) -> J {
    json!({
        "typology": "office", "floors": 1, "floor_height": 5.0, "band_depth": 24.0,
        "footprint": {"width": 72.0},
        "rooms": [
            {"name": hall, "kind": "technical", "area": 720.0, "count": 3},
            {"name": "UPS room", "kind": "technical", "area": 150.0, "count": 2},
            {"name": "Battery room", "kind": "technical", "area": 100.0, "count": 2},
            {"name": "Chiller plant", "kind": "technical", "area": 120.0}
        ],
        "rules": rules
    })
}

fn build(v: &J) -> (ProjectSpec, GProject, GDc) {
    let s = parse_spec(v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    let d = dc::design(&s, &p);
    (s, p, d)
}

fn h07() -> (ProjectSpec, GProject, GDc) {
    build(&brief(json!({"dc.racks": 600}), "Data hall"))
}

#[test]
fn the_h07_counts_are_met() {
    let (s, _, d) = h07();
    let n = |k| d.count(k);
    assert!(n(Kind::Rack) >= 400, "racks {}", n(Kind::Rack));
    assert!(n(Kind::Crah) >= 8, "cooling units {}", n(Kind::Crah));
    assert!(n(Kind::Chiller) >= 3, "chillers {}", n(Kind::Chiller));
    assert!(n(Kind::Ups) >= 4, "ups {}", n(Kind::Ups));
    assert!(n(Kind::Genset) >= 3, "generators {}", n(Kind::Genset));
    assert!(d.trays() >= 40, "trays {}", d.trays());
    assert!(n(Kind::Door) >= 6, "containment doors {}", n(Kind::Door));
    assert!(n(Kind::Pdu) >= 6, "pdus {}", n(Kind::Pdu));
    assert!(d.pipes() >= 80, "pipes {}", d.pipes());
    assert!(d.sizes().len() >= 3, "sizes {:?}", d.sizes());
    assert_eq!(d.halls.len(), 3);
    let text = d.redundancy();
    assert!(text.contains("2N") && text.contains("N+1") && text.contains("Not redundant: nothing"), "{text}");
    let codes = d.redundancy_codes();
    assert!(codes.contains(&("power", "2N".to_string())) && codes.contains(&("chw", "dual".to_string())), "{codes:?}");
    assert!(!d.why.is_empty() && d.why.iter().all(|w| w.rule.starts_with("dc.")));
    let feats = dc::features(&s, &d);
    assert!(feats.iter().all(|f| f.1), "{feats:?}");
}

fn seg_dist(p: (f64, f64, f64), a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    let d = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
    let dd = d.0 * d.0 + d.1 * d.1 + d.2 * d.2;
    let t = if dd > 0.0 { (((p.0 - a.0) * d.0 + (p.1 - a.1) * d.1 + (p.2 - a.2) * d.2) / dd).clamp(0.0, 1.0) } else { 0.0 };
    let q = (a.0 + d.0 * t - p.0, a.1 + d.1 * t - p.1, a.2 + d.2 * t - p.2);
    (q.0 * q.0 + q.1 * q.1 + q.2 * q.2).sqrt()
}

#[test]
fn every_run_end_meets_a_run_a_connector_or_a_cap() {
    let (_, p, d) = h07();
    let ports: Vec<(f64, f64, f64)> = d.links.iter().map(|k| dc::link_at(&p, &d, k).expect("connector")).collect();
    assert!(ports.len() >= 2 * d.count(Kind::Crah), "{} links", ports.len());
    let mut free = vec![];
    for (i, r) in d.runs.iter().enumerate() {
        assert!(r.points.len() >= 2);
        for (end, cap) in [(r.points[0], r.caps.0), (*r.points.last().unwrap(), r.caps.1)] {
            if cap {
                continue;
            }
            let on_run = d.runs.iter().enumerate().any(|(j, o)| {
                j != i && o.net == r.net && o.points.windows(2).any(|w| seg_dist(end, w[0], w[1]) < 1e-3)
            });
            let on_port = ports.iter().any(|q| seg_dist(end, *q, *q) < 1e-3);
            if !(on_run || on_port) {
                free.push((i, end));
            }
        }
    }
    assert!(free.is_empty(), "{} free ends of {} runs, first {:?}", free.len(), d.runs.len(), &free[..free.len().min(5)]);
}

#[test]
fn units_do_not_overlap_and_clear_the_columns() {
    let (_, p, d) = h07();
    let boxes: Vec<(usize, usize, [f64; 4])> =
        d.units.iter().enumerate().map(|(i, u)| (i, u.storey, dc::plan_box(u).expect("library type"))).collect();
    let hit = |a: &[f64; 4], b: &[f64; 4]| a[0] < b[2] - 1e-6 && b[0] < a[2] - 1e-6 && a[1] < b[3] - 1e-6 && b[1] < a[3] - 1e-6;
    for (i, k, a) in &boxes {
        for (j, l, b) in &boxes {
            assert!(i >= j || k != l || !hit(a, b), "units {i} and {j} overlap");
        }
        for c in &p.storeys[*k].layout.columns {
            let cb = [c.at.0 - c.section.0 / 2.0, c.at.1 - c.section.1 / 2.0, c.at.0 + c.section.0 / 2.0, c.at.1 + c.section.1 / 2.0];
            assert!(!hit(a, &cb), "unit {i} on a column");
        }
    }
    // runs keep off the columns of their own storey by their outer size
    for r in &d.runs {
        let half = r.half();
        let st = &p.storeys[r.storey];
        for c in &st.layout.columns {
            for w in r.points.windows(2) {
                let top = (c.at.0, c.at.1, st.elevation + st.height);
                let bot = (c.at.0, c.at.1, st.elevation);
                let near = (0..=20).any(|t| {
                    let f = t as f64 / 20.0;
                    let q = (w[0].0 + (w[1].0 - w[0].0) * f, w[0].1 + (w[1].1 - w[0].1) * f, w[0].2 + (w[1].2 - w[0].2) * f);
                    seg_dist(q, bot, top) < half + 0.12
                });
                assert!(!near, "{:?} run passes a column at {:?}", r.net, c.at);
            }
        }
    }
}

#[test]
fn a_rule_changes_the_design() {
    let (_, _, hot) = build(&brief(json!({"dc.racks": 600, "dc.rack_kw": 12.0}), "Data hall"));
    let (_, _, cool) = build(&brief(json!({"dc.racks": 600, "dc.rack_kw": 4.0}), "Data hall"));
    assert!(hot.count(Kind::Crah) > cool.count(Kind::Crah), "{} vs {}", hot.count(Kind::Crah), cool.count(Kind::Crah));
    let (_, _, few) = build(&brief(json!({"dc.racks": 120}), "Data hall"));
    assert_eq!(few.count(Kind::Rack), 120);
    let (s, _, n) = build(&brief(json!({"dc.racks": 600, "dc.gen_spare": 0, "dc.ups_paths": 1}), "Data hall"));
    assert!(n.redundancy().contains("Not redundant: generators"), "{}", n.redundancy());
    // one UPS path: path B trays are capped, never fed from path A
    let unfed = n.why.iter().filter(|w| w.key == "dc_no_feed").count();
    assert_eq!(unfed, n.halls.len(), "every hall's path B feeder is capped with its reason");
    assert!(n.redundancy_codes().iter().all(|(k, c)| *k != "power" || c != "2N"), "{:?}", n.redundancy_codes());
    let _ = s;
}

/// A Russian sample from the test data file.
fn ru_sample(key: &str) -> String {
    let m: serde_json::Value = serde_json::from_str(include_str!("../data/ru_samples.json")).unwrap();
    m[key].as_str().unwrap_or_else(|| panic!("no ru sample {key}")).to_string()
}

#[test]
fn a_hall_is_found_by_its_tag_or_its_name_in_another_language() {
    let (_, _, ru) = build(&brief(json!({}), &ru_sample("machine_hall")));
    assert!(ru.count(Kind::Rack) > 0);
    let mut v = brief(json!({}), "Hall 1");
    v["rooms"][0]["tags"] = json!(["data_hall"]);
    let (_, _, tagged) = build(&v);
    assert!(tagged.count(Kind::Rack) > 0);
    let (_, _, none) = build(&json!({"typology": "office", "floors": 2}));
    assert!(none.is_empty());
}

#[test]
fn every_dc_rule_has_a_default_and_both_languages() {
    let ui = |lang: &str| -> J {
        let path = format!("{}/../ui/data/i18n/{lang}/gen_datacenter.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let (en, ru) = (ui("en"), ui("ru"));
    let ds: Vec<_> = defs().iter().filter(|d| d.group == "dc").collect();
    assert!(ds.len() >= 25, "{}", ds.len());
    for d in ds {
        assert!(d.default.is_some(), "{}", d.id);
        for k in [format!("gen.rule.{}.title", d.id), format!("gen.rule.{}.desc", d.id)] {
            assert!(en[&k].is_string() && ru[&k].is_string(), "{k}");
        }
    }
    for (k, _) in dc::why::EN {
        let k = format!("gen.why.e.{k}");
        assert!(en[&k].is_string() && ru[&k].is_string(), "{k}");
    }
    assert!(en["gen.group.dc"].is_string() && ru["gen.group.dc"].is_string());
    let _ = rules::groups();
}

/// Numeric literals outside comments, strings and tests: only structural ones.
#[test]
fn the_fit_out_holds_no_number_outside_the_rules() {
    const STRUCTURAL: [&str; 11] = ["0", "1", "2", "3", "4", "0.0", "1.0", "2.0", "0.5", "1e-9", "1000.0"];
    const EXCEPTIONS: [(&str, &str); 1] = [("plant.rs", "60.0")];
    let sources = [
        ("mod.rs", include_str!("../../src/project/dc/mod.rs")),
        ("hall.rs", include_str!("../../src/project/dc/hall.rs")),
        ("plant.rs", include_str!("../../src/project/dc/plant.rs")),
        ("why.rs", include_str!("../../src/project/dc/why.rs")),
    ];
    let mut stray = vec![];
    for (file, src) in sources {
        let code = src.split("#[cfg(test)]").next().unwrap_or("");
        for (n, line) in code.lines().enumerate() {
            let mut s = String::new();
            let mut quoted = false;
            for c in line.chars() {
                if c == '"' {
                    quoted = !quoted;
                } else if !quoted {
                    s.push(c);
                }
            }
            let s = s.split("//").next().unwrap_or("").to_string();
            let b: Vec<char> = s.chars().collect();
            let mut i = 0;
            while i < b.len() {
                let prev = if i == 0 { ' ' } else { b[i - 1] };
                if b[i].is_ascii_digit() && !(prev.is_alphanumeric() || prev == '_' || prev == '.') {
                    let mut t = String::new();
                    while i < b.len() && (b[i].is_ascii_digit() || b[i] == '.' || b[i] == 'e' || b[i] == '-' || b[i] == '_') {
                        if b[i] == '.' && !(i + 1 < b.len() && b[i + 1].is_ascii_digit()) {
                            break;
                        }
                        if b[i] == '-' && !t.ends_with('e') {
                            break;
                        }
                        t.push(b[i]);
                        i += 1;
                    }
                    while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_') {
                        i += 1;
                    }
                    let t = t.replace('_', "");
                    if !STRUCTURAL.contains(&t.as_str()) && !EXCEPTIONS.contains(&(file, t.as_str())) {
                        stray.push(format!("{file}:{} {t}", n + 1));
                    }
                } else {
                    i += 1;
                }
            }
        }
    }
    assert!(stray.is_empty(), "numbers without a rule: {stray:?}");
    for (_, src) in sources {
        for part in src.split("rule(\"").skip(1) {
            let id = part.split('"').next().unwrap_or("");
            assert!(rules::def(id).is_some_and(|d| d.default.is_some()), "rule {id}");
        }
    }
}

fn seg_seg(a: (f64, f64, f64), b: (f64, f64, f64), c: (f64, f64, f64), d: (f64, f64, f64)) -> f64 {
    use ddc_geom::math::v3;
    ddc_geom::mep::route3d::seg_seg_dist(v3(a.0, a.1, a.2), v3(b.0, b.1, b.2), v3(c.0, c.1, c.2), v3(d.0, d.1, d.2))
}

/// Joined runs: an end of one lies on the other.
fn joined(a: &dc::DcRun, b: &dc::DcRun) -> bool {
    let ends = |x: &dc::DcRun| [x.points[0], *x.points.last().unwrap()];
    let on = |e: (f64, f64, f64), x: &dc::DcRun| x.points.windows(2).any(|w| seg_dist(e, w[0], w[1]) < 1e-3);
    ends(a).iter().any(|e| on(*e, b)) || ends(b).iter().any(|e| on(*e, a))
}

#[test]
fn runs_keep_clear_of_each_other_by_their_outer_size() {
    let (_, _, d) = h07();
    let mut hits = vec![];
    for (i, a) in d.runs.iter().enumerate() {
        for (j, b) in d.runs.iter().enumerate().skip(i + 1) {
            if a.storey != b.storey || joined(a, b) {
                continue;
            }
            let need = a.half() + b.half() - 1e-6;
            let close = a.points.windows(2).any(|u| b.points.windows(2).any(|v| seg_seg(u[0], u[1], v[0], v[1]) < need));
            if close {
                hits.push((i, j, a.net, b.net));
            }
        }
    }
    assert!(hits.is_empty(), "{} clashing pairs, first {:?}", hits.len(), &hits[..hits.len().min(5)]);
}

#[test]
fn runs_keep_out_of_the_units_they_do_not_join() {
    let (_, p, d) = h07();
    let mut hits = vec![];
    for (ui, u) in d.units.iter().enumerate() {
        let b = dc::plan_box(u).expect("library type");
        let z0 = p.storeys[u.storey].elevation + u.z;
        let z1 = z0 + dc::unit_height(u).expect("library type");
        let inside = |q: (f64, f64, f64), m: f64| {
            q.0 > b[0] - m && q.0 < b[2] + m && q.1 > b[1] - m && q.1 < b[3] + m && q.2 > z0 - m && q.2 < z1 + m
        };
        for (ri, r) in d.runs.iter().enumerate() {
            if r.storey != u.storey {
                continue;
            }
            // a run ending on the unit's face joins its connector
            if [r.points[0], *r.points.last().unwrap()].iter().any(|e| inside(*e, 1e-3)) {
                continue;
            }
            let m = r.half() - 1e-6;
            let close = r.points.windows(2).any(|w| {
                let steps = ((seg_dist(w[0], w[1], w[1]) / 0.05).ceil() as usize).max(1);
                (0..=steps).any(|t| {
                    let f = t as f64 / steps as f64;
                    inside((w[0].0 + (w[1].0 - w[0].0) * f, w[0].1 + (w[1].1 - w[0].1) * f, w[0].2 + (w[1].2 - w[0].2) * f), m)
                })
            });
            if close {
                hits.push((ri, r.net, ui, u.kind));
            }
        }
    }
    assert!(hits.is_empty(), "{} run-unit clashes, first {:?}", hits.len(), &hits[..hits.len().min(5)]);
}
