// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The services clean-up pass: deterministic metrics, every joint kept, no clash added, no
//! typology worse and some shorter, the same result wherever the origin stands.
use ddc_gen::project::{build_project, duct_faults, parse_spec, route_metrics, services_errors, void_check, GProject};
use serde_json::{json, Value};

fn typologies() -> Vec<(&'static str, Value)> {
    vec![
        ("house", json!({"typology": "house", "floors": 2, "services": true})),
        ("apartment_section", json!({"typology": "apartment_section", "floors": 5, "services": true})),
        ("office", json!({"typology": "office", "floors": 8, "services": true, "sprinklers": true})),
        ("school", json!({"typology": "school", "floors": 3, "services": true})),
        // the ward building the void clash tests build
        ("hospital", json!({"typology": "office", "floors": 10, "services": true, "sprinklers": true,
            "rooms": [{"kind": "ward", "area": 22, "count": 4, "name": "Ward", "tags": ["clinical"]},
                      {"kind": "technical", "area": 36, "name": "Theatre", "tags": ["operating"]},
                      {"kind": "ward", "area": 20, "name": "Isolation", "tags": ["isolation"]},
                      {"kind": "office", "area": 18, "count": 2}, {"kind": "wc", "area": 4, "count": 2}]})),
    ]
}

fn build(mut v: Value, coord: bool) -> (GProject, (f64, f64)) {
    let rules = v.as_object_mut().unwrap().entry("rules").or_insert(json!({}));
    rules["mep.coord"] = json!(coord);
    let spec = parse_spec(&v).unwrap();
    (build_project(&spec).unwrap(), spec.origin)
}

fn metrics(v: &Value, coord: bool) -> Value {
    let (p, o) = build(v.clone(), coord);
    route_metrics(&p, o)
}

/// Writes one line per typology and state to `ROUTE_LEDGER` when it is set.
#[test]
fn ledger() {
    let Ok(path) = std::env::var("ROUTE_LEDGER") else { return };
    let tag = std::env::var("ROUTE_TAG").unwrap_or_default();
    let mut out = String::new();
    for (name, v) in typologies() {
        for coord in [false, true] {
            let m = metrics(&v, coord);
            out += &json!({"typology": name, "coord": coord, "tag": tag, "metrics": m}).to_string();
            out += "\n";
        }
    }
    use std::io::Write;
    std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap().write_all(out.as_bytes()).unwrap();
}

fn ends_on(p: &[(f64, f64, f64)], q: (f64, f64, f64)) -> bool {
    p.windows(2).any(|w| {
        let (a, b) = (w[0], w[1]);
        let d = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
        let l2 = d.0 * d.0 + d.1 * d.1 + d.2 * d.2;
        let t = if l2 > 0.0 { (((q.0 - a.0) * d.0 + (q.1 - a.1) * d.1 + (q.2 - a.2) * d.2) / l2).clamp(0.0, 1.0) } else { 0.0 };
        let c = (a.0 + d.0 * t - q.0, a.1 + d.1 * t - q.1, a.2 + d.2 * t - q.2);
        (c.0 * c.0 + c.1 * c.1 + c.2 * c.2).sqrt() < 1e-3
    })
}

/// Every run by system with its points: water and power runs, sprinkler runs, ducts.
fn runs(p: &GProject) -> Vec<(String, Vec<(f64, f64, f64)>)> {
    let mut out: Vec<(String, Vec<(f64, f64, f64)>)> = vec![];
    for r in p.services.water.iter().chain(&p.services.elec) {
        out.push((format!("{:?}", r.sys), r.points.clone()));
    }
    for f in &p.fire.runs {
        out.push(("fire".into(), f.run.points.clone()));
    }
    for d in &p.hvac.ducts {
        out.push((format!("duct {:?} {}", d.sys, d.net), d.points.clone()));
    }
    out
}

/// For each run end, the runs of its system it lies on: the joints of the networks.
fn joints(p: &GProject) -> Vec<(usize, usize, Vec<usize>)> {
    let rs = runs(p);
    let bb: Vec<[f64; 6]> = rs
        .iter()
        .map(|r| {
            r.1.iter().fold([f64::MAX, f64::MAX, f64::MAX, f64::MIN, f64::MIN, f64::MIN], |b, q| {
                [b[0].min(q.0), b[1].min(q.1), b[2].min(q.2), b[3].max(q.0), b[4].max(q.1), b[5].max(q.2)]
            })
        })
        .collect();
    let inside = |b: &[f64; 6], q: (f64, f64, f64)| {
        q.0 > b[0] - 1e-3 && q.0 < b[3] + 1e-3 && q.1 > b[1] - 1e-3 && q.1 < b[4] + 1e-3 && q.2 > b[2] - 1e-3 && q.2 < b[5] + 1e-3
    };
    let mut out = vec![];
    for (i, (sys, pts)) in rs.iter().enumerate() {
        for (e, q) in [pts[0], *pts.last().unwrap()].into_iter().enumerate() {
            let on: Vec<usize> = rs.iter().enumerate().filter(|(j, r)| *j != i && &r.0 == sys && inside(&bb[*j], q) && ends_on(&r.1, q)).map(|(j, _)| j).collect();
            out.push((i, e, on));
        }
    }
    out
}

fn num(m: &Value, k: &str) -> f64 {
    m["total"][k].as_f64().unwrap()
}

#[test]
fn the_pass_keeps_every_joint_adds_no_fault_and_makes_no_typology_worse() {
    let mut fails = vec![];
    let mut gain = false;
    for (name, v) in typologies() {
        let (off, o) = build(v.clone(), false);
        let (on, _) = build(v.clone(), true);
        let (a, b) = (runs(&off), runs(&on));
        assert_eq!(a.len(), b.len(), "{name}: run count");
        let (j0, j1) = (joints(&off), joints(&on));
        assert_eq!(j0, j1, "{name}: a joint was lost or added");
        // an end at a connector or a cap stays; one on another run may follow that run
        for (i, e, on_runs) in &j0 {
            let at = |r: &[(f64, f64, f64)]| if *e == 0 { r[0] } else { *r.last().unwrap() };
            if on_runs.is_empty() {
                assert_eq!(at(&a[*i].1), at(&b[*i].1), "{name}: a free run end moved");
            }
        }
        assert!(services_errors(&on).len() <= services_errors(&off).len(), "{name}: services errors {:?}", services_errors(&on));
        assert!(duct_faults(&on).len() <= duct_faults(&off).len(), "{name}: duct faults {:?}", duct_faults(&on));
        assert!(void_check(&on).len() <= void_check(&off).len(), "{name}: clashes {:?}", void_check(&on));
        let (m0, m1) = (route_metrics(&off, o), route_metrics(&on, o));
        assert!(num(&m1, "hard_clashes") <= num(&m0, "hard_clashes"), "{name}: hard clashes");
        let tiers = |m: &Value| m["tier_violations"].as_object().map_or(0, |o| o.values().map(|v| v.as_u64().unwrap()).sum::<u64>());
        let line = format!(
            "{name}: bends/10m {} -> {}, efficiency {} -> {}, tier {} -> {}, length {} -> {}, soft {} -> {}",
            num(&m0, "bends_per_10m"), num(&m1, "bends_per_10m"), num(&m0, "efficiency"), num(&m1, "efficiency"), tiers(&m0), tiers(&m1),
            num(&m0, "length_m"), num(&m1, "length_m"), num(&m0, "soft_clashes"), num(&m1, "soft_clashes")
        );
        println!("{line}");
        let soft = |m: &Value| num(m, "soft_clashes");
        let bends = |m: &Value| m["systems"].as_object().unwrap().values().map(|v| v["bends"].as_u64().unwrap()).sum::<u64>();
        // fewer metres with the same bends raise bends per 10 m: the count is what must not grow
        let worse = bends(&m1) > bends(&m0)
            || num(&m1, "efficiency") < num(&m0, "efficiency")
            || num(&m1, "length_m") > num(&m0, "length_m")
            || tiers(&m1) > tiers(&m0)
            || soft(&m1) > soft(&m0);
        if worse {
            fails.push(line);
        }
        gain |= bends(&m1) < bends(&m0) || num(&m1, "length_m") < num(&m0, "length_m") || soft(&m1) < soft(&m0) || tiers(&m1) < tiers(&m0);
    }
    assert!(gain, "the pass changed nothing on any typology");
    assert!(fails.is_empty(), "worse: {fails:?}");
}

#[test]
fn the_metrics_are_the_same_on_every_build_and_wherever_the_origin_stands() {
    let v = typologies().into_iter().find(|t| t.0 == "school").unwrap().1;
    let a = metrics(&v, true);
    assert_eq!(a, metrics(&v, true), "two builds differ");
    let mut moved = v.clone();
    moved["origin"] = json!([100.37, -50.12]);
    let b = metrics(&moved, true);
    // a shifted origin rounds the world coordinates differently: counts equal, sizes within
    // 5 mm, and what the generator alone already changes with the origin left aside
    let (off_a, off_b) = (metrics(&v, false), metrics(&moved, false));
    fn same(a: &Value, b: &Value, oa: &Value, ob: &Value, at: &str) {
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                for (k, v) in x.iter().filter(|(k, _)| k.as_str() != "sha256") {
                    same(v, &y[k], &oa[k], &ob[k], &format!("{at}/{k}"));
                }
            }
            _ if oa != ob => {}
            (Value::Number(x), Value::Number(y)) if x.is_f64() || y.is_f64() => {
                assert!((x.as_f64().unwrap() - y.as_f64().unwrap()).abs() <= 5e-3, "the origin moved {at}: {x} {y}")
            }
            _ => assert_eq!(a, b, "the origin moved {at}"),
        }
    }
    same(&a, &b, &off_a, &off_b, "");
}

/// Prints the long level runs of each corridor of one storey when `ROUTE_PROBE` names a typology.
#[test]
fn probe() {
    let Ok(t) = std::env::var("ROUTE_PROBE") else { return };
    let v = typologies().into_iter().find(|x| x.0 == t).unwrap().1;
    let k: usize = std::env::var("ROUTE_STOREY").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let (p, _) = build(v, std::env::var("ROUTE_COORD").is_ok());
    let st = &p.storeys[k];
    for (ri, r) in st.rooms.iter().enumerate() {
        if format!("{:?}", r.kind) != "Corridor" {
            continue;
        }
        let poly = &st.layout.rooms[ri].polygon;
        let b = poly.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, q| [b[0].min(q.0), b[1].min(q.1), b[2].max(q.0), b[3].max(q.1)]);
        println!("corridor {ri} {:.2?} z0 {:.2} h {:.2}", b, st.elevation, st.height);
        let mut rows = vec![];
        let mut add = |key: String, pts: &[(f64, f64, f64)], size: (f64, f64)| {
            for (s, w) in pts.windows(2).enumerate() {
                let (a, c) = (w[0], w[1]);
                let inside = |q: (f64, f64, f64)| q.0 > b[0] - 0.01 && q.0 < b[2] + 0.01 && q.1 > b[1] - 0.01 && q.1 < b[3] + 0.01;
                let len = ((c.0 - a.0).powi(2) + (c.1 - a.1).powi(2)).sqrt();
                if inside(a) && inside(c) && len > 2.0 && (c.2 - a.2).abs() < 0.1 {
                    let along_x = (c.0 - a.0).abs() > (c.1 - a.1).abs();
                    let across = if along_x { a.1 } else { a.0 };
                    rows.push((key.clone(), s, pts.len(), along_x, across, a.2 - st.elevation, len, size));
                }
            }
        };
        for r in p.services.water.iter().chain(&p.services.elec) {
            add(format!("{:?}/{:?}", r.sys, r.section.domain), &r.points, r.section.envelope().wh());
        }
        for f in &p.fire.runs {
            add(format!("fire/{:?}", f.kind), &f.run.points, f.run.section.envelope().wh());
        }
        for d in &p.hvac.ducts {
            add(format!("duct {:?}", d.sys), &d.points, d.size);
        }
        rows.sort_by(|a, b| a.4.total_cmp(&b.4));
        for r in rows {
            println!("  {:<22} seg {}/{} x{} across {:.3} z {:.3} len {:.2} size {:.3?}", r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7);
        }
    }
}

/// Prints the runs of one system with three bends or more when `ROUTE_RUNS` names the system.
#[test]
fn probe_runs() {
    let (Ok(t), Ok(sys)) = (std::env::var("ROUTE_PROBE"), std::env::var("ROUTE_RUNS")) else { return };
    let v = typologies().into_iter().find(|x| x.0 == t).unwrap().1;
    let (p, _) = build(v, std::env::var("ROUTE_COORD").is_ok());
    let mut n = 0;
    for (k, pts) in runs(&p).into_iter().filter(|r| r.0 == sys).map(|r| r.1).enumerate() {
        let bends = pts
            .windows(3)
            .filter(|w| {
                let u = (w[1].0 - w[0].0, w[1].1 - w[0].1, w[1].2 - w[0].2);
                let v = (w[2].0 - w[1].0, w[2].1 - w[1].1, w[2].2 - w[1].2);
                let c = (u.0 * v.0 + u.1 * v.1 + u.2 * v.2) / ((u.0 * u.0 + u.1 * u.1 + u.2 * u.2).sqrt() * (v.0 * v.0 + v.1 * v.1 + v.2 * v.2).sqrt());
                c < 0.999
            })
            .count();
        if bends >= 3 && n < 12 {
            n += 1;
            let s: Vec<String> = pts.iter().map(|q| format!("({:.3},{:.3},{:.3})", q.0, q.1, q.2)).collect();
            println!("run {k} bends {bends}: {}", s.join(" "));
        }
    }
}
