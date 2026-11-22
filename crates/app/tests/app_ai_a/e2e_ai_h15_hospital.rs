// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! H15 on H02 through the chat: the hospital wing is built and applied, then a partition splits a
//! third-floor ward and a new operating theatre is placed in the far part, with terminals and its
//! own supply and extract air routed from a shaft; the edited model meets every oracle of the
//! H15 case against the H02 model it started from.

use crate::e2e_ai_common;

use ddc_app::ai::eval::case::{evals_dir, Case};
use ddc_app::ai::eval::facts::Facts;
use ddc_app::ai::eval::oracle::{blocking_findings, evaluate, in_range, EvalCx};
use ddc_app::ai::receipt::WriteReceipt;
use ddc_app::ai::UiSession;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::json;

const THEATRE: &str = "Operating theatre 3";
const ANSWER: &str = "One more operating theatre on the third floor, with its own supply and extract air; \
every other system is as it was.";

/// Applies the session's plan and keeps what the person was told about it.
fn apply(s: &mut UiSession, ed: &mut Editor, id: &str, receipts: &mut Vec<WriteReceipt>) {
    let plan = plan_of(s);
    let issues: Vec<_> = plan.issues.iter().chain(&plan.main_issues).cloned().collect();
    let seq = apply_and_check(s, ed);
    receipts.push(WriteReceipt::build(&plan.diff, &plan.qty, &issues, seq as u64, true, id));
}

fn all_ok(res: &[(String, bool)], n: usize) {
    assert!(res.len() == n && res.iter().all(|r| !r.1), "{res:?}");
}

/// H02 as its own test builds it: the hospital wing, then supply, return and exhaust air.
fn h02(ed: &mut Editor) {
    let spec = json!({"typology": "hospital_wing", "floors": 4});
    let mut calls = vec![tool("generate_project", json!({"spec": spec})), tool("load_tools", json!({"groups": ["mep"]}))];
    for kind in ["supply_air", "return_air", "exhaust_air"] {
        let args = json!({"title": kind, "why": "ventilation of the wing", "kind": kind, "from": {"riser": [1.0, 1.0]}, "friction_pa_m": 1.0});
        calls.push(tool("route_mep", args));
    }
    let (mut s, provider) = run(ed, calls);
    all_ok(&tool_results(&provider), 5);
    apply(&mut s, ed, "H02_hospital_hvac", &mut vec![]);
}

/// A point of the box `b` (inset 0.4 m) at least `gap` from every component of the level and
/// from `taken`, tried on a 0.3 m grid from the box centre out.
fn clear_spot(f: &Facts, level: i64, b: [f64; 4], taken: &[[f64; 2]], gap: f64) -> [f64; 2] {
    let at = [(b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0];
    let mut offs: Vec<(f64, f64)> = (-8..=8).flat_map(|i| (-8..=8).map(move |j| (i as f64 * 0.3, j as f64 * 0.3))).collect();
    offs.sort_by(|a, c| a.0.hypot(a.1).total_cmp(&c.0.hypot(c.1)));
    offs.iter()
        .map(|(dx, dy)| [at[0] + dx, at[1] + dy])
        .filter(|p| p[0] >= b[0] + 0.4 && p[0] <= b[2] - 0.4 && p[1] >= b[1] + 0.4 && p[1] <= b[3] - 0.4)
        .find(|p| {
            let comps = f.comps.iter().filter(|c| c.level == level).map(|c| [c.at[0], c.at[1]]);
            comps.chain(taken.iter().copied()).all(|q| (p[0] - q[0]).hypot(p[1] - q[1]) >= gap)
        })
        .unwrap_or_else(|| panic!("no clear spot near {at:?} on level {level}"))
}

#[test]
fn h15_theatre_added_on_h02_through_the_tools_meets_every_oracle() {
    let case = Case::load(&evals_dir().join("cases").join("H15_hospital_add_theatre.toml")).unwrap();
    assert_eq!(case.base, "@H02_hospital_hvac");
    let mut ed = Editor::new_in_memory().unwrap();
    h02(&mut ed);
    let before = Facts::read(&ed.model).unwrap();
    let errors_before = blocking_findings(&ed.model).unwrap();
    let theatre = regex::Regex::new("(?i)operating|theatre|theater|surg").unwrap();
    let theatres_before = before.rooms.iter().filter(|r| theatre.is_match(&r.name)).count();

    // the third floor: the third level that has rooms, by elevation
    let levels: Vec<i64> = ed.model.level_list().unwrap().into_iter().map(|l| l.0).collect();
    let floors: Vec<i64> = levels.into_iter().filter(|l| before.rooms.iter().any(|r| r.level == *l)).collect();
    let l3 = floors[2];
    let ward = regex::Regex::new("(?i)ward|patient|bed|office|staff").unwrap();
    let room = before
        .rooms
        .iter()
        .find(|r| r.level == l3 && ward.is_match(&r.name) && !theatre.is_match(&r.name))
        .unwrap_or_else(|| panic!("no ward on level {l3}: {:?}", before.rooms.iter().filter(|r| r.level == l3).map(|r| &r.name).collect::<Vec<_>>()))
        .clone();
    // the partition: across the ward's longer side, 45 % of the way from the side away from the
    // ward's own point, so that point stays in the ward and the far part is the theatre
    let ring = ed.model.room_outline(room.id).unwrap().expect("the ward is closed");
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for (x, y) in &ring {
        b = [b[0].min(*x), b[1].min(*y), b[2].max(*x), b[3].max(*y)];
    }
    let along_x = b[2] - b[0] >= b[3] - b[1];
    let (lo, hi, own) = if along_x { (b[0], b[2], room.at[0]) } else { (b[1], b[3], room.at[1]) };
    let far_lo = own > (lo + hi) / 2.0;
    let cut = if far_lo { lo + 0.45 * (hi - lo) } else { hi - 0.45 * (hi - lo) };
    let part = match (along_x, far_lo) {
        (true, true) => [b[0], b[1], cut, b[3]],
        (true, false) => [cut, b[1], b[2], b[3]],
        (false, true) => [b[0], b[1], b[2], cut],
        (false, false) => [b[0], cut, b[2], b[3]],
    };
    // ends reach into the ward's walls; the top stays under the ceiling void and its ducts
    let (wa, wb) = if along_x { ([cut, b[1] - 0.1], [cut, b[3] + 0.1]) } else { ([b[0] - 0.1, cut], [b[2] + 0.1, cut]) };
    let floor_z = before.level_z.get(&l3).copied().unwrap_or(0.0);
    let duct_low = before.runs.iter().filter(|r| r.level == l3).flat_map(|r| r.pts.iter().map(|p| p[2])).fold(f64::MAX, f64::min);
    let wall_h = (duct_low - floor_z - 0.3).clamp(2.4, 3.0);
    let mid_part = [(part[0] + part[2]) / 2.0, (part[1] + part[3]) / 2.0];
    // the new terminals hang where the level's diffusers hang
    let z = before
        .comps
        .iter()
        .find(|c| c.level == l3 && c.family == "Supply Diffuser")
        .map(|c| c.at[2])
        .expect("the level has diffusers");
    let sa = clear_spot(&before, l3, part, &[], 0.9);
    let ea = clear_spot(&before, l3, part, &[sa], 0.9);
    let mut receipts = vec![];

    // a partition, the theatre room in the far part, one diffuser and one extract grille in it
    let (mut s, provider) = run(
        &mut ed,
        vec![
            tool("create_elements", json!({"title": "Partition for the theatre", "cat": "wall", "items": [{"a": wa, "b": wb, "level": l3, "height": wall_h}]})),
            tool("create_elements", json!({"title": THEATRE, "cat": "room", "items": [{"point": mid_part, "name": THEATRE, "level": l3, "height": wall_h}]})),
            tool("load_tools", json!({"groups": ["mep"]})),
            tool(
                "place_fixtures",
                json!({"title": "Theatre terminals", "level": l3, "items": [
                    {"family": "Supply Diffuser", "type": "600x600", "at": [sa[0], sa[1], z]},
                    {"family": "Exhaust Grille", "type": "400x400", "at": [ea[0], ea[1], z]}
                ]}),
            ),
        ],
    );
    all_ok(&tool_results(&provider), 4);
    apply(&mut s, &mut ed, &case.id, &mut receipts);
    drop(s);
    let mid = Facts::read(&ed.model).unwrap();
    let new_comp = |fam: &str, p: [f64; 2]| {
        mid.comps
            .iter()
            .find(|c| c.level == l3 && c.family == fam && (c.at[0] - p[0]).hypot(c.at[1] - p[1]) < 0.05)
            .unwrap_or_else(|| panic!("no new {fam} at {p:?}"))
            .id
    };
    let (d, g) = (new_comp("Supply Diffuser", sa), new_comp("Exhaust Grille", ea));

    // its own two air systems, each from a riser in a shaft that reaches the third floor
    let shaft = before
        .shafts
        .iter()
        .filter(|sh| sh.level <= l3 && sh.top >= l3)
        .find_map(|sh| sh.bbox)
        .expect("a shaft through the third floor");
    let routes = [
        ("supply_air", "Theatre 3 supply", d, [shaft[0] + 0.15, shaft[1] + 0.15]),
        ("exhaust_air", "Theatre 3 extract", g, [shaft[2] - 0.15, shaft[3] - 0.15]),
    ];
    let mut calls = vec![tool("load_tools", json!({"groups": ["mep"]}))];
    for (kind, system, comp, riser) in routes {
        calls.push(tool(
            "route_mep",
            json!({"title": system, "why": "own air for the new theatre", "kind": kind, "system": system, "level": l3,
                   "from": {"riser": riser}, "to": [format!("comp:{comp}/Air")], "friction_pa_m": 1.0}),
        ));
    }
    let (mut s, provider) = run(&mut ed, calls);
    all_ok(&tool_results(&provider), 3);
    apply(&mut s, &mut ed, &case.id, &mut receipts);

    let facts = Facts::read(&ed.model).unwrap();
    let theatres = facts.rooms.iter().filter(|r| theatre.is_match(&r.name)).count();
    assert_eq!(theatres, theatres_before + 1, "one theatre more");
    assert_eq!(facts.rooms.len(), before.rooms.len() + 1, "one room more, the ward kept");
    assert!(facts.rooms.iter().any(|r| r.id == room.id && r.name == room.name), "the ward keeps its name");
    let mut short = vec![];
    for c in &case.oracle.count {
        match facts.count(&c.spec) {
            Ok(n) if in_range(n as f64, c.min, c.max) => {}
            Ok(n) => short.push(format!("{}: {n} (min {:?}, max {:?})", c.spec, c.min, c.max)),
            Err(e) => short.push(format!("{}: {e}", c.spec)),
        }
    }
    assert!(short.is_empty(), "{}: count oracles: {short:#?}", case.id);
    let cx = EvalCx { ed: &ed, facts: &facts, before: Some(&before), answer: ANSWER, ifc: None, errors_before, receipts: &receipts };
    let all = evaluate(&case, &cx);
    let failed: Vec<_> = all.iter().filter(|r| !r.pass).collect();
    assert!(!all.is_empty() && failed.is_empty(), "{}: {failed:#?}", case.id);
}
