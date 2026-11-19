//! The process plant as a model: families a search finds by their words, the load-bearing
//! steel as columns and beams, a bund round liquid storage, distillation with its reboiler and
//! condenser, outdoor racks on portal frames.

use std::collections::BTreeSet;

use ddc_gen::plant::{self, Plant};
use ddc_gen::project::rules;
use serde_json::{json, Value as J};

fn design(brief: J) -> Plant {
    plant::design(&brief).unwrap_or_else(|e| panic!("{brief}: {e}"))
}

/// A Russian sample from the test data file.
fn ru_sample(key: &str) -> String {
    let m: J = serde_json::from_str(include_str!("data/ru_samples.json")).unwrap();
    m[key].as_str().unwrap_or_else(|| panic!("no ru sample {key}")).to_string()
}

fn every_kind() -> Vec<J> {
    vec![
        json!({"text": "Boiler house 2 MW with 3 boilers"}),
        json!({"text": ru_sample("pump")}),
        json!({"text": "water treatment 20 l/s"}),
        json!({"kind": "process_line", "flow_m3h": 8}),
    ]
}

fn named(p: &Plant, word: &str) -> usize {
    p.kit.comps.iter().filter(|c| c.family.to_lowercase().contains(word)).count()
}

#[test]
fn families_carry_the_words_of_what_they_are() {
    for b in every_kind() {
        let p = design(b.clone());
        for e in &p.equipment {
            let slug = e.kind.name().replace([' ', '-'], "_");
            assert!(e.family_name().starts_with(&slug), "{b}: {}", e.family_name());
            assert!(p.kit.comps.iter().any(|c| c.family == e.family_name()), "{b}: {} not placed", e.tag);
        }
        assert!(named(&p, "pump") >= 2, "{b}");
        assert!(named(&p, "valve") >= 1, "{b}");
        assert!(named(&p, "flanged") >= 1, "{b}");
        assert!(named(&p, "support") >= 1, "{b}");
        for c in p.kit.comps.iter().filter(|c| c.key.starts_with("steel:")) {
            let ok = ["platform_deck", "platform_handrail", "platform_knee_rail", "platform_toe_board", "ladder", "stair", "hanger_rod", "handrail"]
                .iter()
                .any(|w| c.family.starts_with(w));
            assert!(ok, "{b}: {}", c.family);
        }
    }
}

#[test]
fn posts_and_beams_are_members_of_the_model() {
    for b in every_kind() {
        let p = design(b.clone());
        let k = &p.kit;
        assert!(!k.cols.is_empty() && !k.beams.is_empty(), "{b}");
        let keys: BTreeSet<&str> = k.cols.iter().chain(&k.beams).map(|m| m.key.as_str()).collect();
        assert_eq!(keys.len(), k.cols.len() + k.beams.len(), "{b}: keys repeat");
        for c in &k.cols {
            assert!((c.a.x - c.b.x).abs() < 1e-9 && (c.a.y - c.b.y).abs() < 1e-9 && c.b.z > c.a.z, "{b}: {c:?}");
        }
        for m in &k.beams {
            assert!((m.b - m.a).len() > 1e-3, "{b}: {m:?}");
        }
        // every platform has its deck beams on its own steel
        for pf in &p.platforms {
            let tag = pf.tag.replace('-', "_");
            assert!(k.beams.iter().filter(|m| m.key.starts_with(&format!("beam:{tag}:"))).count() >= 4, "{b}: {}", pf.tag);
        }
    }
}

const H03: &str = "Small chemical process plant outdoors: a jacketed reactor, two feed tanks, a distillation column with reboiler and condenser, pumps as duty/standby pairs, a couple of heat exchangers. A steel pipe rack about 4 m high carrying the process lines between the equipment, nozzles on every vessel, pipes sloped where they need to drain, supports every few metres, access platforms and caged ladders round the column and reactor, and a bund wall round the tank farm.";

fn h03() -> Plant {
    design(json!({"text": H03, "flow_m3h": 10}))
}

fn of_kind(p: &Plant, k: plant::EqKind) -> Vec<&plant::Equip> {
    p.equipment.iter().filter(|e| e.kind == k).collect()
}

fn duty(e: &plant::Equip, name: &str) -> f64 {
    e.duty.iter().find(|d| d.0 == name).map_or(0.0, |d| d.1)
}

#[test]
fn a_reboiler_in_the_words_is_not_a_boiler_house() {
    let p = h03();
    assert_eq!(p.brief.kind, plant::Kind::ProcessLine);
    assert!(p.brief.distill && p.brief.outdoor && p.brief.feed_tanks == 2, "{:?}", p.brief);
    assert!(of_kind(&p, plant::EqKind::Boiler).is_empty());
}

#[test]
fn distillation_brings_a_column_with_its_reboiler_and_condenser() {
    let p = h03();
    assert!(p.report.ok, "{:?}", p.report.issues.iter().take(10).map(|i| format!("{}: {}", i.check, i.what)).collect::<Vec<_>>());
    assert_eq!(p.report.connected, p.report.nozzles);
    assert_eq!(of_kind(&p, plant::EqKind::Column).len(), 1);
    assert_eq!(of_kind(&p, plant::EqKind::Reactor).len(), 1);
    assert_eq!(of_kind(&p, plant::EqKind::Tank).len(), 3, "two feed tanks and the product tank");
    let hx = of_kind(&p, plant::EqKind::Exchanger);
    let roles: BTreeSet<&str> = hx.iter().map(|e| e.role).collect();
    assert_eq!(roles, BTreeSet::from(["condenser", "cooler", "reboiler"]));
    // what the overhead gives up in the condenser the reboiler took in
    let by = |r: &str| hx.iter().find(|e| e.role == r).map(|e| duty(e, "heat_kw")).unwrap();
    assert!((by("condenser") - by("reboiler")).abs() < 1e-6 && by("reboiler") > 0.0);
    // every pump set is duty and standby
    let mut sets: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
    for e in of_kind(&p, plant::EqKind::Pump) {
        let c = sets.entry(e.role).or_default();
        if e.standby {
            c.1 += 1
        } else {
            c.0 += 1
        }
    }
    for r in ["transfer_pump", "product_pump", "dispatch_pump", "reflux_pump", "bottoms_pump"] {
        let (d, s) = sets[r];
        assert!(d >= 1 && s == 1, "{r}: {d}+{s}");
    }
    // the column is tall: a platform with a caged ladder serves it
    let col = &of_kind(&p, plant::EqKind::Column)[0].tag;
    let pf = p.platforms.iter().find(|x| &x.serves == col).expect("a platform at the column");
    assert!(pf.access.caged || pf.access.kind == "stair");
}

#[test]
fn the_reflux_rule_moves_the_boil_up_and_says_so() {
    let kw = |r: f64| {
        let p = design(json!({"text": H03, "flow_m3h": 10, "rules": {"plant.column.reflux": r}}));
        let rb = of_kind(&p, plant::EqKind::Exchanger).into_iter().find(|e| e.role == "reboiler").map(|e| duty(e, "heat_kw")).unwrap();
        (rb, p.why.iter().any(|y| y.rule == "plant.column.reflux" && y.key == "plant_reflux"))
    };
    let (lo, said) = kw(1.0);
    let (hi, _) = kw(3.0);
    assert!(said && hi > lo * 1.5, "{lo} {hi}");
}

#[test]
fn lines_have_a_service_and_a_size() {
    let p = h03();
    assert!(p.schedules.lines.len() >= 10);
    for l in &p.schedules.lines {
        assert!(l.dn >= 15 && !l.service.is_empty() && l.length > 0.0, "{}", l.number);
    }
    let sizes: BTreeSet<u32> = p.schedules.lines.iter().map(|l| l.dn).collect();
    assert!(sizes.len() >= 4, "{sizes:?}");
}

fn tank_boxes(p: &Plant) -> Vec<(f64, f64, f64, f64)> {
    of_kind(p, plant::EqKind::Tank)
        .iter()
        .flat_map(|e| e.body())
        .map(|(lo, hi)| (lo.x, lo.y, hi.x, hi.y))
        .collect()
}

#[test]
fn the_tanks_stand_in_a_bund_that_holds_the_largest() {
    let p = h03();
    let b = p.bund.as_ref().expect("a process line has a bund");
    assert_eq!(b.tanks.len(), 3);
    assert!(b.volume >= 1.1 * b.big - 1e-9, "{} vs {}", b.volume, b.big);
    for (x0, y0, x1, y1) in tank_boxes(&p) {
        assert!(b.lo.x < x0 && b.lo.y < y0 && b.hi.x > x1 && b.hi.y > y1, "a tank sticks out of the bund");
    }
    assert!(p.report.issues.iter().all(|i| i.check != "bund"), "{:?}", p.report.issues.iter().map(|i| &i.what).collect::<Vec<_>>());
    // four straight walls, closed, as high as the bund
    let w = &p.kit.walls;
    assert_eq!(w.len(), 4);
    for k in 0..4 {
        assert!((w[k].b - w[(k + 1) % 4].a).len() < 1e-9 && (w[k].height - b.h).abs() < 1e-9);
    }
    // a stricter volume rule widens it, and the answer says why
    let q = design(json!({"text": H03, "flow_m3h": 10, "rules": {"plant.bund.volume": 1.5}}));
    let qb = q.bund.as_ref().unwrap();
    assert!(qb.volume >= 1.5 * qb.big - 1e-9);
    assert!(p.why.iter().any(|y| y.key == "plant_bund" && y.rule == "plant.bund.volume"));
    // other kinds of plant have none
    assert!(design(json!({"text": "water treatment 20 l/s"})).bund.is_none());
}

#[test]
fn outdoors_the_rack_stands_on_portals_and_indoors_it_hangs() {
    let out = h03();
    let rack_posts = out.kit.cols.iter().filter(|c| c.key.starts_with("col:rack:")).count();
    let trapezes = out.piping.trapezes.len();
    assert!(trapezes >= 2 && rack_posts == 2 * trapezes, "{rack_posts} posts for {trapezes} trapezes");
    assert!(out.kit.beams.iter().any(|b| b.key.starts_with("beam:rack:") && (b.b.x - b.a.x).abs() > 1.0), "no tie along the rack");
    assert!(out.kit.families.iter().all(|f| !f.name.starts_with("hanger_rod_rack")));
    let inn = design(json!({"text": H03, "flow_m3h": 10, "outdoor": false}));
    assert!(inn.kit.cols.iter().all(|c| !c.key.starts_with("col:rack:")));
    assert!(inn.kit.families.iter().any(|f| f.name.starts_with("hanger_rod_rack")));
    // the frame of H03: columns and beams enough to read as a structure, over 3.5 m
    assert!(out.kit.cols.len() + out.kit.beams.len() >= 20, "{}", out.kit.cols.len() + out.kit.beams.len());
    let top = out.kit.cols.iter().map(|c| c.b.z).fold(0.0, f64::max);
    assert!(top >= 3.5, "{top}");
}

/// Pipes are straight pieces of at most `plant.pipe.max_len`, each its own run; they meet end to
/// end, so each line is one network and only the nozzles are free ends.
#[test]
fn pipes_are_straight_pieces_that_stay_one_network_per_line() {
    use std::collections::{BTreeMap, BTreeSet};
    let key = |q: ddc_geom::math::V3| ((q.x * 1e4).round() as i64, (q.y * 1e4).round() as i64, (q.z * 1e4).round() as i64);
    for (max, rules_json) in [(6.0, json!({})), (3.0, json!({"plant.pipe.max_len": 3.0}))] {
        let p = design(json!({"text": H03, "flow_m3h": 10, "rules": rules_json}));
        assert!(p.kit.runs.len() >= 40, "{} pipes at {max} m", p.kit.runs.len());
        let mut at: BTreeMap<(usize, (i64, i64, i64)), usize> = BTreeMap::new();
        for r in &p.kit.runs {
            assert_eq!(r.points.len(), 2, "line {}: not a straight pipe", r.line);
            let len = (r.points[1] - r.points[0]).len();
            assert!(len > 1e-3 && len <= max + 1e-6, "line {}: a pipe of {len} m, the longest is {max}", r.line);
            for q in &r.points {
                *at.entry((r.line, key(*q))).or_default() += 1;
            }
        }
        // free ends: exactly one per nozzle, every other end meets a neighbour
        let free = at.values().filter(|n| **n == 1).count();
        assert_eq!(free, p.report.nozzles, "{free} free ends for {} nozzles", p.report.nozzles);
        // each line is one piece: pipes joined through shared ends reach every pipe
        for li in 0..p.lines.len() {
            let runs: Vec<_> = p.kit.runs.iter().filter(|r| r.line == li).collect();
            if runs.is_empty() {
                continue;
            }
            let mut seen = BTreeSet::from([0usize]);
            let mut todo = vec![0usize];
            while let Some(i) = todo.pop() {
                for (j, o) in runs.iter().enumerate() {
                    let joined = o.points.iter().any(|q| runs[i].points.iter().any(|w| key(*q) == key(*w)));
                    if joined && seen.insert(j) {
                        todo.push(j);
                    }
                }
            }
            assert_eq!(seen.len(), runs.len(), "line {} falls into pieces", p.lines[li].number());
        }
    }
}

#[test]
fn drain_runs_fall() {
    let p = h03();
    let falls = |pts: &[ddc_geom::math::V3]| {
        let mut f: Vec<f64> = vec![];
        for w in pts.windows(2) {
            let run = (w[1].x - w[0].x).hypot(w[1].y - w[0].y);
            let dz = w[1].z - w[0].z;
            if run > 1e-6 && dz.abs() <= run {
                f.push(-dz / run);
            }
        }
        !f.is_empty() && f.iter().all(|x| *x > 1e-6 && (x - f[0]).abs() < 1e-5)
    };
    let sloped = p.kit.runs.iter().filter(|r| falls(&r.points)).count();
    assert!(sloped >= 5, "{sloped} of {} runs fall evenly", p.kit.runs.len());
}

/// The counts the process-plant brief is judged on: equipment, nozzles, valves, supports,
/// platforms with their rails, ways up, and the steel under the rack and platforms.
#[test]
fn the_h03_plant_meets_the_counts_of_its_brief() {
    let p = h03();
    let eq = |k| of_kind(&p, k).len();
    let vessels = eq(plant::EqKind::Tank) + eq(plant::EqKind::Reactor) + eq(plant::EqKind::Column) + eq(plant::EqKind::Exchanger);
    assert!(vessels >= 7, "{vessels} vessels and exchangers");
    assert!(eq(plant::EqKind::Pump) >= 3, "{} pumps", eq(plant::EqKind::Pump));
    assert!(named(&p, "nozzle") + named(&p, "flange") >= 10, "{} nozzles", named(&p, "nozzle") + named(&p, "flange"));
    // the case counts every run as a pipe (`mep:pipe` >= 40)
    assert!(p.kit.runs.len() >= 40, "{} runs", p.kit.runs.len());
    assert!(p.schedules.totals.supports >= 15, "{} supports", p.schedules.totals.supports);
    assert!(p.schedules.totals.valves >= 8, "{} valves", p.schedules.totals.valves);
    let ways_up = p.platforms.len() + named(&p, "ladder") + named(&p, "stair");
    assert!(p.platforms.len() >= 2 && ways_up >= 3, "{} platforms, {ways_up} with ladders and stairs", p.platforms.len());
    assert!(p.platforms.iter().all(|x| x.serves.len() > 0), "a platform serves nothing");
    assert!(p.kit.cols.len() + p.kit.beams.len() >= 20);
}

/// A platform above the reach height has a stair and a ladder at its two ends; one at or below
/// it has a single way up.
/// The count oracle `railing+comp:platform|grating >= 6` reads families by name: a platform's
/// deck, handrail, knee rail and toe board are separate parts, so a few platforms are enough.
#[test]
fn a_platform_is_named_in_its_deck_rails_and_toe_boards() {
    let p = h03();
    let all = named(&p, "platform");
    assert!(all >= 6, "{all} platform parts of {} platforms", p.platforms.len());
    for pf in &p.platforms {
        let tag = pf.tag.replace(['-', ' '], "_");
        for part in ["platform_deck", "platform_handrail", "platform_knee_rail", "platform_toe_board"] {
            let name = format!("{part}_{tag}");
            assert!(p.kit.comps.iter().any(|c| c.family == name), "{}: no {part}", pf.tag);
        }
    }
}

#[test]
fn every_platform_has_a_stair_and_a_ladder() {
    let p = h03();
    assert!(!p.platforms.is_empty());
    let reach = rules::rule("plant.access.reach");
    let stair_run = |deck: f64| deck / rules::rule("plant.stair.angle").to_radians().tan();
    for pf in &p.platforms {
        if pf.deck <= reach {
            assert!(pf.extra.is_empty(), "{}: a low platform has one way up", pf.tag);
            continue;
        }
        assert_eq!(pf.extra.len(), 1, "{}: {} exits", pf.tag, 1 + pf.extra.len());
        let kinds: BTreeSet<&str> = std::iter::once(pf.access.kind).chain(pf.extra.iter().map(|a| a.kind)).collect();
        // a stair longer than max_run is replaced by a ship ladder
        let want = if stair_run(pf.deck) > rules::rule("plant.stair.max_run") { ["ladder", "ship_ladder"] } else { ["ladder", "stair"] };
        assert_eq!(kinds, BTreeSet::from(want), "{}", pf.tag);
        assert!((pf.access.foot - pf.extra[0].foot).len() > 0.5, "{}: both ways up at one end", pf.tag);
    }
    assert!(named(&p, "ladder") + named(&p, "stair") + named(&p, "ship") >= 3);
}

/// Where a stair would run too far for the room, a ship ladder and a caged ladder take its place,
/// and the report and the reasons list every exit.
#[test]
fn a_long_stair_gives_way_to_a_ship_ladder() {
    let p = design(json!({"text": H03, "flow_m3h": 10, "rules": {"plant.stair.max_run": 2.0}}));
    let tall: Vec<_> = p.platforms.iter().filter(|x| x.deck > 3.0).collect();
    assert!(!tall.is_empty());
    for pf in &tall {
        let kinds: BTreeSet<&str> = std::iter::once(pf.access.kind).chain(pf.extra.iter().map(|a| a.kind)).collect();
        assert!(kinds.contains("ship_ladder") && !kinds.contains("stair"), "{}: {kinds:?}", pf.tag);
        assert!(pf.extra.iter().any(|a| a.kind == "ladder" && a.caged), "{}: no cage ladder", pf.tag);
    }
    let r = serde_json::to_string(&p.schedules.platforms).unwrap();
    assert!(r.contains("ship_ladder") && r.contains("\"exits\""));
    assert!(p.why.iter().any(|w| w.key.contains("plant_ship")));
}

/// A ship ladder pitch not steeper than the stair is raised to 10 degrees above it, with a note.
#[test]
fn a_ship_ladder_is_steeper_than_the_stair_or_is_raised() {
    let p = design(json!({"text": H03, "flow_m3h": 10, "rules": {"plant.stair.angle": 60.0, "plant.stair.ship_angle": 55.0, "plant.stair.max_run": 2.0}}));
    assert!(p.why.iter().any(|w| w.key.contains("plant_ship_angle")), "no note on the raised pitch");
    let ok = design(json!({"text": H03, "flow_m3h": 10}));
    assert!(!ok.why.iter().any(|w| w.key.contains("plant_ship_angle")), "a steeper ship ladder gets no note");
}

const G15: &str = "A vertical storage tank 12 m diameter 14 m tall with a conical roof, manway, four nozzles at \
different heights and orientations, ladder, and a ring foundation.";

/// G15: one tank, four nozzles at four heights and four directions plus a manway as their own
/// elements (the count oracle `comp:nozzle|manway|flange >= 5`), a caged ladder, and a closed
/// ring of strip footings under the shell.
#[test]
fn the_g15_tank_meets_the_counts_of_its_brief() {
    let p = design(json!({"text": G15}));
    assert_eq!(p.brief.kind, plant::Kind::StorageTank);
    assert!(p.report.ok);
    assert_eq!(named(&p, "tank") + named(&p, "vessel"), 1);
    let fittings: Vec<_> = p.kit.comps.iter().filter(|c| ["nozzle", "manway", "flange"].iter().any(|w| c.family.to_lowercase().contains(w))).collect();
    assert_eq!(fittings.len(), 5, "{fittings:?}");
    let nozzles: Vec<_> = fittings.iter().filter(|c| c.key.starts_with("nozzle:")).collect();
    assert_eq!(nozzles.len(), 4);
    let heights: BTreeSet<i64> = nozzles.iter().map(|c| (c.at.z * 1000.0).round() as i64).collect();
    assert_eq!(heights.len(), 4, "nozzles share a height");
    let dirs: BTreeSet<i64> = nozzles.iter().map(|c| (c.at.y.atan2(c.at.x).to_degrees()).round() as i64).collect();
    assert_eq!(dirs.len(), 4, "nozzles share a direction");
    assert!(nozzles.iter().all(|c| c.at.z > 0.0 && c.at.z < 14.0 + 0.1 * 12.0 + 0.01));
    assert_eq!(named(&p, "ladder"), 1);
    assert!(p.why.iter().any(|w| w.key == "plant_ladder_cage"), "a 14 m ladder is caged");
    let ring = &p.kit.footings;
    assert_eq!(ring.len(), 4);
    for (k, f) in ring.iter().enumerate() {
        let next = &ring[(k + 1) % ring.len()];
        assert!((f.b - next.a).len() < 1e-9, "the ring is open at {k}");
        assert!(((f.a.x * f.a.x + f.a.y * f.a.y).sqrt() - 6.0).abs() < 1e-9);
    }
    let fams: BTreeSet<&str> = p.kit.families.iter().map(|f| f.name.as_str()).collect();
    assert!(p.kit.comps.iter().all(|c| fams.contains(c.family.as_str())));
    let keys: BTreeSet<&str> = p.kit.comps.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys.len(), p.kit.comps.len(), "keys repeat");
    assert!(p.kit.cols.is_empty() && p.kit.beams.is_empty() && p.kit.runs.is_empty());
    // a brief without sizes takes them from the rules
    let q = design(json!({"kind": "storage_tank", "rules": {"plant.tank.d": 8.0, "plant.tank.nozzles": 6}}));
    assert_eq!(q.equipment[0].dims.d, 8.0);
    assert_eq!(q.kit.comps.iter().filter(|c| c.key.starts_with("nozzle:")).count(), 6);
}
