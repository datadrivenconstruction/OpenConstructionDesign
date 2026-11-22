// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The outdoor process plant of the site example at its full flow: designed with the default
//! rules, every nozzle piped, the rack at road clearance, no building round it, and the pipes
//! built into a model network without a leg too short for its elbows.

use ddc_gen::plant::{self, Plant};
use serde_json::{json, Value as J};

fn brief() -> J {
    serde_json::from_str(include_str!("data/plant_run_brief.json")).unwrap()
}

fn design(b: &J) -> Plant {
    plant::design(b).unwrap_or_else(|e| panic!("{b}: {e}"))
}

fn briefs() -> Vec<(String, J)> {
    let mut site = brief();
    let mut out = vec![("site default".to_string(), {
        let mut b = site.clone();
        b["rules"] = json!({});
        b
    })];
    out.push(("site step 1".into(), std::mem::take(&mut site)));
    let h03 = "Small chemical process plant outdoors: a jacketed reactor, two feed tanks, a distillation column with reboiler and condenser, pumps as duty/standby pairs, a couple of heat exchangers. A steel pipe rack about 4 m high carrying the process lines between the equipment, nozzles on every vessel, pipes sloped where they need to drain, supports every few metres, access platforms and caged ladders round the column and reactor, and a bund wall round the tank farm.";
    out.push(("h03 q10".into(), json!({"text": h03, "flow_m3h": 10})));
    out.push(("h03 q10 indoor".into(), json!({"text": h03, "flow_m3h": 10, "outdoor": false})));
    out.push(("boiler".into(), json!({"text": "Boiler house 2 MW with 3 boilers"})));
    out.push(("water".into(), json!({"text": "water treatment 20 l/s"})));
    out.push(("line8".into(), json!({"kind": "process_line", "flow_m3h": 8})));
    out
}

#[test]
#[ignore]
fn probe() {
    let only = std::env::var("PROBE").unwrap_or_default();
    for (name, b) in briefs() {
        if !only.is_empty() && !name.starts_with(&only) {
            continue;
        }
        let p = design(&b);
        let r = &p.report;
        println!("== {name}: nozzles {} connected {} issues {} tiers {:?} plot x {:.1} y {:.1}..{:.1} walk {:?}", r.nozzles, r.connected, r.issues.len(), p.piping.tiers, p.hall.x1, p.hall.y0, p.hall.y1, p.hall.walk);
        let mut by: std::collections::BTreeMap<&str, usize> = Default::default();
        for i in &r.issues {
            *by.entry(i.check).or_default() += 1;
        }
        println!("  by check {by:?}");
        for i in r.issues.iter().take(30) {
            println!("  {}: {}", i.check, i.what);
        }
        if std::env::var("PROBE_OPS").is_ok() {
            for i in p.piping.items.iter().filter(|i| i.item.operated() && i.op.z > 1.8) {
                let e = &p.equipment[i.end.eq];
                println!("  op {} {:?} on {}.{} op ({:.2},{:.2},{:.2}) dir ({:.0},{:.0},{:.0}) unit at ({:.1},{:.1}) decks {:?}", i.tag, i.item, e.tag, e.nozzles[i.end.nz].name, i.op.x, i.op.y, i.op.z, i.dir.x, i.dir.y, i.dir.z, e.at.x, e.at.y,
                    p.platforms.iter().filter(|q| q.serves == e.tag).map(|q| q.deck).collect::<Vec<_>>());
            }
        }
        if std::env::var("PROBE_EQ").is_ok() {
            for q in &p.platforms {
                println!("  plat {} deck {:.2} lo ({:.1},{:.1}) hi ({:.1},{:.1}) access {:?} {:?}", q.serves, q.deck, q.lo.x, q.lo.y, q.hi.x, q.hi.y, (q.access.lo, q.access.hi), q.extra.iter().map(|a| (a.lo, a.hi)).collect::<Vec<_>>());
            }
            for e in p.equipment.iter().filter(|e| e.tag == "P-104" || e.tag == "P-106") {
                for n in &e.nozzles {
                    let (a, d) = e.nozzle_world(e.nozzles.iter().position(|m| m.name == n.name).unwrap());
                    println!("  nz {}.{} at ({:.2},{:.2},{:.2}) dir ({:.0},{:.0},{:.0})", e.tag, n.name, a.x, a.y, a.z, d.x, d.y, d.z);
                }
            }
            for e in &p.equipment {
                let (lo, hi) = e.extent();
                println!("  {} {:?} {} at ({:.1},{:.1},{:.1}) rot {} size ({:.1},{:.1},{:.1}) row {:?}", e.tag, e.kind, e.role, e.at.x, e.at.y, e.at.z, e.rot, hi.x - lo.x, hi.y - lo.y, hi.z - lo.z, e.row);
            }
        }
        match build_network(&p) {
            Ok(n) => println!("  network ok, {} fittings, {} open ends", n.fittings.len(), n.open_ends.len()),
            Err(e) => println!("  network: {}", &e[..e.len().min(600)]),
        }
    }
}

/// The pipes of the plant built into a model as `design_plant` applies them: a system per line,
/// a run per pipe; the network is then laid out with its elbows and tees.
fn build_network(p: &Plant) -> Result<ddc_model::MepNetwork, String> {
    use ddc_model::{MepBuild, Model, RunSpec, SysRef};
    let mut m = Model::open(":memory:").map_err(|e| e.to_string())?;
    let level = m.add_level("L0", 0.0).map_err(|e| e.to_string())?;
    let mut build = MepBuild::default();
    for l in &p.lines {
        build.systems.push((l.number(), plant::system_kind(l.svc)));
    }
    for r in &p.kit.runs {
        let l = &p.lines[r.line];
        let pts = r.points.iter().map(|q| (q.x, q.y, q.z)).collect();
        let mut spec = RunSpec::pipe(0, level, l.spec.material(), r.dn, pts).map_err(|e| e.to_string())?;
        spec.section.insulation = l.insul;
        spec.slope = r.fall;
        build.runs.push((SysRef::New(r.line), spec));
    }
    m.mep_build(&build).map_err(|e| e.to_string())?;
    m.mep_network(None).map_err(|e| e.to_string())
}

#[test]
fn the_site_brief_with_default_rules_builds_a_network() {
    let mut b = brief();
    b["rules"] = json!({});
    let p = design(&b);
    let n = build_network(&p).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(n.open_ends.len(), p.report.nozzles, "a free end at each nozzle, none elsewhere");
}

#[test]
fn every_plant_brief_pipes_every_nozzle_with_no_issue() {
    for (name, b) in briefs() {
        let p = design(&b);
        let r = &p.report;
        let what: Vec<String> = r.issues.iter().take(10).map(|i| format!("{}: {}", i.check, i.what)).collect();
        assert_eq!(r.connected, r.nozzles, "{name}: {what:?}");
        assert!(r.issues.is_empty(), "{name}: {} issues {what:?}", r.issues.len());
        let n = build_network(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(n.open_ends.len(), r.nozzles, "{name}: free ends");
    }
}

#[test]
fn the_outdoor_rack_stands_at_road_height_with_tiers_apart() {
    let p = design(&brief());
    let t = &p.piping.tiers;
    assert!(t[0] >= 4.0 && t[0] <= 6.5, "first tier {:.2}", t[0]);
    for w in t.windows(2) {
        let d = w[1] - w[0];
        assert!((1.4..=2.1).contains(&d), "tier pitch {d:.2}");
    }
}

#[test]
fn an_outdoor_plant_has_no_walls_but_its_bund() {
    let p = design(&brief());
    assert!(!p.kit.walls.is_empty(), "the tank farm keeps its bund");
    for w in &p.kit.walls {
        assert!(w.key.starts_with("wall:bund:"), "{}", w.key);
    }
}
