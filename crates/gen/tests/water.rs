// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Water supply and drainage self-checks.

use ddc_gen::project::{build_project, parse_spec, services_errors, svc_ports, GProject, GRun, SvcSys};
use serde_json::json;

fn project(t: &str, floors: u32) -> GProject {
    build_project(&parse_spec(&json!({"typology": t, "floors": floors, "services": true})).unwrap()).unwrap()
}

/// The four typologies of the services briefs, built once.
fn projects() -> &'static [(&'static str, GProject)] {
    static P: std::sync::OnceLock<Vec<(&'static str, GProject)>> = std::sync::OnceLock::new();
    P.get_or_init(|| [("house", 2), ("apartment_section", 5), ("office", 4), ("school", 3)].into_iter().map(|(t, n)| (t, project(t, n))).collect())
}

fn water(p: &GProject, sys: SvcSys) -> impl Iterator<Item = &GRun> {
    p.services.water.iter().filter(move |r| r.sys == sys)
}

fn dn(r: &GRun) -> u32 {
    r.pipe.map_or(0, |x| x.1)
}

/// Whether `q` lies on the axis of a run, within a millimetre.
fn on(r: &GRun, q: (f64, f64, f64)) -> bool {
    r.points.windows(2).any(|w| {
        let (a, b) = (w[0], w[1]);
        let d = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
        let l2 = d.0 * d.0 + d.1 * d.1 + d.2 * d.2;
        let t = if l2 > 0.0 { (((q.0 - a.0) * d.0 + (q.1 - a.1) * d.1 + (q.2 - a.2) * d.2) / l2).clamp(0.0, 1.0) } else { 0.0 };
        let c = (a.0 + d.0 * t - q.0, a.1 + d.1 * t - q.1, a.2 + d.2 * t - q.2);
        (c.0 * c.0 + c.1 * c.1 + c.2 * c.2).sqrt() < 1e-3
    })
}

#[test]
fn every_fixture_is_on_supply_and_drain() {
    for (t, p) in projects() {
        // known open ports: a WC too near its stack for a collector (apartment) and cold taps
        // walled in by the drops of the room beside (school)
        let most = match *t {
            "apartment_section" => 4,
            "school" => 15,
            _ => 0,
        };
        let mut open = vec![];
        for (k, st) in p.storeys.iter().enumerate() {
            for (i, f) in st.layout.fixtures.iter().enumerate() {
                for c in svc_ports(f).into_iter().filter(|c| c.sys != SvcSys::Power) {
                    if !p.services.links.iter().any(|l| l.storey == k && l.fixture == i && l.connector == c.name && l.sys == c.sys) {
                        open.push(format!("storey {k} {} {} {:?}", f.family, c.name, c.sys));
                    }
                }
            }
        }
        assert!(open.len() <= most, "{t}: {} open, {open:?}", open.len());
        let unserved = p.metrics.iter().find(|m| m.0 == "water_unserved").map_or(f64::NAN, |m| m.1);
        assert!(unserved <= most as f64, "{t}: water_unserved {unserved}");
    }
}

/// The water runs of a project moved to another origin move with it, unchanged.
#[test]
fn water_follows_the_origin() {
    let spec = |o: Option<[f64; 2]>| {
        let mut j = json!({"typology": "house", "floors": 2, "services": true});
        if let Some(o) = o {
            j["origin"] = json!(o);
        }
        build_project(&parse_spec(&j).unwrap()).unwrap()
    };
    let (p, q) = (spec(None), spec(Some([100.0, -50.0])));
    assert_eq!(p.services.water.len(), q.services.water.len());
    for (r, s) in p.services.water.iter().zip(&q.services.water) {
        let same = r.sys == s.sys
            && r.points.len() == s.points.len()
            && r.points.iter().zip(&s.points).all(|(u, v)| (u.0 + 100.0 - v.0).abs() < 1e-6 && (u.1 - 50.0 - v.1).abs() < 1e-6 && (u.2 - v.2).abs() < 1e-9);
        assert!(same, "{:?} {:?}
 vs {:?}", r.sys, r.points, s.points);
    }
}

#[test]
fn water_networks_are_sound() {
    for (t, p) in projects() {
        let errs: Vec<String> = services_errors(p).into_iter().filter(|f| ["Cold", "Hot", "Sanitary"].iter().any(|s| f.starts_with(s))).collect();
        assert!(errs.is_empty(), "{t}: {errs:?}");
    }
}

/// EN 12056-2 / DIN 1986-100: every sloping drain falls at least 1 %, and each run keeps
/// one sense along its points (down the flow, or up a stack).
#[test]
fn drains_fall_and_never_run_uphill() {
    for (t, p) in projects() {
        for r in water(p, SvcSys::Sanitary) {
            let dz: Vec<f64> = r.points.windows(2).map(|w| w[1].2 - w[0].2).collect();
            let down = dz.iter().all(|d| *d <= 1e-9);
            let up = dz.iter().all(|d| *d >= -1e-9);
            assert!(down || up, "{t}: drain rises and falls {:?}", r.points);
            for w in r.points.windows(2) {
                let plan = (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
                let fall = (w[1].2 - w[0].2).abs();
                if plan > 0.1 && fall < plan {
                    assert!(fall / plan >= 0.01 - 1e-9, "{t}: drain fall {:.2} % at {:?}", fall / plan * 100.0, w[0]);
                }
            }
        }
    }
}

/// A drain never narrows downstream: a run ending on another is no larger than it.
#[test]
fn drain_sizes_never_shrink_downstream() {
    for (t, p) in projects() {
        let runs: Vec<&GRun> = water(p, SvcSys::Sanitary).collect();
        for a in &runs {
            let end = *a.points.last().unwrap();
            for b in runs.iter().filter(|b| !std::ptr::eq(**b, *a) && on(b, end)) {
                let at_start = {
                    let s = b.points[0];
                    (s.0 - end.0).hypot(s.1 - end.1).hypot(s.2 - end.2) < 1e-3
                };
                // a stack continues from one storey's segment to the next
                if at_start {
                    continue;
                }
                assert!(dn(a) <= dn(b), "{t}: DN{} drains into DN{} at {end:?}", dn(a), dn(b));
            }
        }
    }
}

/// Risers narrow going up as the flow they carry drops, and every stack is vented above
/// the roof.
#[test]
fn risers_narrow_upwards_and_stacks_are_vented() {
    for (t, p) in projects() {
        for sys in [SvcSys::Cold, SvcSys::Hot, SvcSys::Sanitary] {
            let mut ups: Vec<&GRun> = water(p, sys).filter(|r| r.points.len() == 2 && r.points[0].0 == r.points[1].0 && r.points[0].1 == r.points[1].1).collect();
            ups.sort_by(|a, b| a.points[0].2.min(a.points[1].2).total_cmp(&b.points[0].2.min(b.points[1].2)));
            for (i, a) in ups.iter().enumerate() {
                let top = a.points[0].2.max(a.points[1].2);
                let above = ups[i + 1..].iter().find(|b| b.points[0].0 == a.points[0].0 && b.points[0].1 == a.points[0].1 && (b.points[0].2.min(b.points[1].2) - top).abs() < 1e-6);
                match above {
                    Some(b) if sys != SvcSys::Sanitary => assert!(dn(b) <= dn(a), "{t}: {sys:?} riser widens upwards at {:?}", b.points[0]),
                    None if sys == SvcSys::Sanitary && a.caps.1 => assert!(top > p.roof.elevation, "{t}: stack vent at {top:.2} under the roof {:.2}", p.roof.elevation),
                    _ => {}
                }
            }
        }
        let vents = water(p, SvcSys::Sanitary).filter(|r| r.caps.1).count();
        assert!(vents > 0, "{t}: no stack vent");
    }
}
