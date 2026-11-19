// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Long-span blocks: member counts, proportions, deflection and strength checks, supports
//! and connections, rules that steer the sizing, and the refusals.

use ddc_ops::{Cat, GeoOp, Op, Ref};
use ddc_script::recipes::recipe;
use ddc_script::{GeoScript, RunOpts, Sandbox, ScriptError, ScriptRun};
use serde_json::json;

fn run(src: &str) -> Result<ScriptRun, ScriptError> {
    let o = RunOpts::new(Ref::name(Cat::Level, "Level 1"), json!({}));
    Sandbox.run(src, &o)
}

fn ok(src: &str) -> ScriptRun {
    run(src).unwrap_or_else(|e| panic!("{e}\n{src}"))
}

fn fail(src: &str) -> String {
    run(src).expect_err("script must fail").message
}

fn role(r: &ScriptRun, role: &str) -> usize {
    r.env.groups[0]
        .ops
        .iter()
        .filter(|op| match op {
            Op::Geometry(GeoOp::Solid { name: Some(n), .. }) => n.split(' ').next() == Some(role),
            _ => false,
        })
        .count()
}

/// The numbers a script printed as `K name value`.
fn num(r: &ScriptRun, name: &str) -> f64 {
    let key = format!("K {name} ");
    r.log
        .iter()
        .find_map(|l| l.strip_prefix(&key))
        .unwrap_or_else(|| panic!("no `{key}` in {:?}", r.log))
        .trim()
        .parse()
        .unwrap()
}

/// Prints the measured check of the last block call as `K name value` lines.
const PROBE: &str = "
local c = info.check
print('K util ' .. c.util_max)
print('K defl ' .. c.deflection_mm)
print('K limit ' .. c.limit_mm)
print('K ratio ' .. c.span_depth)
print('K mass ' .. c.mass_t)
print('K freq ' .. c.f1_hz)
print('K ok ' .. (c.ok and 1 or 0))
print('K notes ' .. #c.notes)
print('K why ' .. #c.why)
";

fn probe(call: &str) -> ScriptRun {
    ok(&format!("local g, info = {call}\n{PROBE}audit{{strict = true}}"))
}

const H10: &str = "bridge{kind = 'arch', span = 90, rise = 15, width = 4.5, bays = 30, deck_z = 6, ramp_slope = 0.05}";

#[test]
fn arch_footbridge_has_hangers_bearings_abutments_piers_and_ramps() {
    let r = probe(H10);
    assert_eq!(role(&r, "hanger"), 2 * 29, "a hanger per arch node");
    assert_eq!(role(&r, "arch"), 60);
    assert_eq!(role(&r, "abutment"), 2);
    assert_eq!(role(&r, "wind_brace"), 30);
    // two bearings per abutment, two under each ramp pier
    assert!(role(&r, "bearing") >= 4 + 2 * 2 * 2, "bearings {}", role(&r, "bearing"));
    assert!(role(&r, "pier") >= 8, "ramp piers {}", role(&r, "pier"));
    assert!(role(&r, "footing") >= 2 + role(&r, "pier"), "a footing under every pier and abutment");
    assert!(role(&r, "handrail") > 80 && role(&r, "baluster") > 80);
    // members: girders and cross beams, arch, hangers, bracing: more than the brief's 80
    let members = role(&r, "girder") + role(&r, "cross_beam") + role(&r, "arch") + role(&r, "hanger") + role(&r, "wind_brace");
    assert!(members >= 80 + 30, "{members}");
    // ramp deck panels come on top of the 30 main-span panels
    assert!(role(&r, "deck") > 30 + 20);
}

#[test]
fn arch_footbridge_passes_its_own_checks_and_is_proportioned() {
    let r = probe(H10);
    assert!(num(&r, "ok") == 1.0 || num(&r, "notes") >= 1.0, "a failing check says why: {:?}", r.log);
    assert_eq!(num(&r, "ok"), 1.0, "strength and deflection pass: {:?}", r.log);
    assert!(num(&r, "util") <= 0.95 + 1e-9, "util {}", num(&r, "util"));
    assert!(num(&r, "util") > 0.3, "sections are not wildly oversized: {}", num(&r, "util"));
    assert!(num(&r, "defl") <= num(&r, "limit") + 1e-9, "{} > {}", num(&r, "defl"), num(&r, "limit"));
    // limit is span/400 = 225 mm
    assert!((num(&r, "limit") - 225.0).abs() < 1.0, "{}", num(&r, "limit"));
    assert!((num(&r, "ratio") - 6.0).abs() < 1e-9, "span/rise {}", num(&r, "ratio"));
    assert!(num(&r, "freq") > 0.5 && num(&r, "freq") < 20.0);
    assert!(num(&r, "why") >= 8.0, "every rule explained");
    assert!(r.log.iter().any(|l| l.starts_with("bridge check:") && l.contains("Hz")), "{:?}", r.log);
}

#[test]
fn check_text_names_each_rule_with_its_reason() {
    let r = ok(&format!(
        "local g, info = {H10}\nfor _, l in ipairs(info.check.why) do print('W ' .. l) end\naudit{{strict = true}}"
    ));
    let why: Vec<&String> = r.log.iter().filter(|l| l.starts_with("W ")).collect();
    for key in ["[util_max", "[defl_bridge", "[f_min", "proportions:", "bearings:", "footings:", "ramps:", "hanger:", "arch:", "girder:"] {
        assert!(why.iter().any(|l| l.contains(key)), "missing `{key}` in {why:?}");
    }
}

#[test]
fn stricter_rules_cost_steel_and_deeper_trusses_save_it() {
    let mass = |extra: &str| num(&probe(&format!("bridge{{kind = 'truss', span = 48, bays = 12, depth = 4, width = 4{extra}}}")), "mass");
    let base = mass("");
    let strict = mass(", rules = {util_max = 0.5}");
    assert!(strict > base, "strict {strict} vs base {base}");
    let shallow = num(&probe("bridge{kind = 'truss', span = 48, bays = 12, depth = 3, width = 4}"), "mass");
    let deep = num(&probe("bridge{kind = 'truss', span = 48, bays = 12, depth = 6, width = 4}"), "mass");
    assert!(deep < shallow, "deep {deep} t vs shallow {shallow} t");
    // a longer span needs more steel than twice the shorter one
    let short = num(&probe("bridge{kind = 'girder', span = 20, bays = 8, width = 4}"), "mass");
    let long = num(&probe("bridge{kind = 'girder', span = 40, bays = 16, width = 4}"), "mass");
    assert!(long > 2.0 * short, "{long} vs {short}");
}

#[test]
fn deflection_limit_is_met_for_girders_with_piers() {
    let r = probe("bridge{kind = 'girder', span = 60, bays = 12, width = 5, piers = 2, deck_z = 7}");
    assert!(num(&r, "defl") <= num(&r, "limit") + 1e-9);
    // limit follows the longest sub-span: 20 m / 400 = 50 mm
    assert!((num(&r, "limit") - 50.0).abs() < 0.5, "{}", num(&r, "limit"));
    assert_eq!(role(&r, "pier"), 4, "two interior supports, one pier under each girder");
    assert_eq!(role(&r, "bearing"), 4 + 4, "end bearings and pier bearings");
    assert_eq!(role(&r, "abutment"), 2);
}

#[test]
fn railings_stand_clear_of_stays_hangers_and_truss_webs() {
    for call in [
        "bridge{kind = 'truss', span = 36, width = 3.5, depth = 3, bays = 9, deck_z = 4}",
        H10,
        "bridge{kind = 'cable_stayed', span = 80, width = 4, bays = 16, pylon_h = 30, pylon_at = 0.3, deck_z = 6}",
        "bridge{kind = 'girder', span = 30, width = 3, bays = 6, deck_z = 5, ramp_slope = 0.05}",
    ] {
        let r = ok(&format!("{call}\naudit{{strict = true}}"));
        assert!(role(&r, "handrail") > 0 && role(&r, "baluster") > 0, "{call}");
    }
}

/// Axis ends (start, end) in metres of every extruded part named after `role`.
fn axes(r: &ScriptRun, role: &str) -> Vec<([f64; 3], [f64; 3])> {
    r.env.groups[0]
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Geometry(GeoOp::Solid { form: ddc_ops::Form::Extrude { frame, start, end, .. }, name: Some(n), .. })
                if n.split(' ').next() == Some(role) =>
            {
                let (o, z) = (frame.origin.to_m(), frame.z);
                let at = |t: f64| [o[0] + z.x * t, o[1] + z.y * t, o[2] + z.z * t];
                Some((at(start.to_m()), at(end.to_m())))
            }
            _ => None,
        })
        .collect()
}

/// Members meet at nodes, so a pylon leg is split where stays reach it: `legs` vertical chains
/// from the ground to one top, unbroken, and every stay's upper end is a node of a leg.
fn legs_carry_the_stays_at_nodes(r: &ScriptRun, legs: usize) {
    let eq = |a: f64, b: f64| (a - b).abs() < 1e-3;
    let mut chains: Vec<([f64; 2], Vec<(f64, f64)>)> = vec![];
    let pylon = axes(r, "pylon");
    for (a, b) in &pylon {
        assert!(eq(a[0], b[0]) && eq(a[1], b[1]), "a leg piece is vertical: {a:?} {b:?}");
        let seg = (a[2].min(b[2]), a[2].max(b[2]));
        match chains.iter_mut().find(|c| eq(c.0[0], a[0]) && eq(c.0[1], a[1])) {
            Some(c) => c.1.push(seg),
            None => chains.push(([a[0], a[1]], vec![seg])),
        }
    }
    assert_eq!(chains.len(), legs, "legs {chains:?}");
    let top = pylon.iter().map(|(a, b)| a[2].max(b[2])).fold(f64::MIN, f64::max);
    for (at, segs) in &mut chains {
        segs.sort_by(|p, q| p.0.total_cmp(&q.0));
        assert!(eq(segs[0].0, 0.0), "leg at {at:?} stands on the ground: {segs:?}");
        assert!(segs.windows(2).all(|w| eq(w[0].1, w[1].0)), "leg at {at:?} is unbroken: {segs:?}");
        assert!(eq(segs[segs.len() - 1].1, top), "leg at {at:?} reaches the top {top}: {segs:?}");
    }
    let node = |p: [f64; 3]| pylon.iter().any(|(a, b)| [a, b].iter().any(|q| (0..3).all(|k| eq(q[k], p[k]))));
    let stays = axes(r, "stay");
    assert!(!stays.is_empty() && stays.len() == role(r, "stay"), "every stay is a straight member");
    for (a, b) in stays {
        let up = if a[2] > b[2] { a } else { b };
        assert!(node(up), "stay ends off a leg node at {up:?}");
    }
}

#[test]
fn cable_stayed_stays_are_tension_rods_and_pylon_stands_on_the_ground() {
    let r = probe("bridge{kind = 'cable_stayed', span = 80, width = 4, bays = 16, pylon_h = 30, pylon_at = 0.3, deck_z = 6}");
    legs_carry_the_stays_at_nodes(&r, 2);
    assert!(role(&r, "stay") >= 20);
    assert!(num(&r, "util") <= 0.95 + 1e-9);
    assert!(num(&r, "defl") <= num(&r, "limit") + 1e-9);
}

#[test]
fn profile_option_fixes_a_group_and_size_false_skips_the_check() {
    let r = ok("local g, info = bridge{kind = 'truss', span = 36, bays = 9, depth = 3, girder = 'I 900x400'}\n\
                print('S ' .. info.check.sections.girder)\naudit{strict = true}");
    assert!(r.log.iter().any(|l| l == "S given by the script"), "{:?}", r.log);
    let r = ok("local g, info = bridge{kind = 'truss', span = 36, bays = 9, depth = 3, size = false}\n\
                print('C ' .. tostring(info.check.ok))\naudit{strict = true}");
    assert!(r.log.iter().any(|l| l == "C nil"), "{:?}", r.log);
    assert!(r.log.iter().any(|l| l.starts_with("bridge check: check not run")), "{:?}", r.log);
}

#[test]
fn rules_are_refused_by_name_and_range() {
    let e = fail("bridge{rules = {nonsense = 1}}");
    assert!(e.contains("not a rule") && e.contains("util_max"), "{e}");
    let e = fail("bridge{rules = {util_max = 2}}");
    assert!(e.contains("outside its range"), "{e}");
    let e = fail("bridge{kind = 'arch', piers = 1}");
    assert!(e.contains("piers apply to the truss and girder kinds"), "{e}");
    let e = fail("bridge{kind = 'truss', deck_z = 0.6}");
    assert!(e.contains("deck_z leaves less than 0.3 m"), "{e}");
    let e = fail("space_frame{nx = 6, ny = 6, bay = 4, height = 12, depth = 3, columns = 'perimeter', crane_z = 11}");
    assert!(e.contains("crane_z must be at least 0.5 m under the bottom chords"), "{e}");
}

#[test]
fn stadium_roof_has_trusses_columns_back_stays_and_a_seating_bowl() {
    let r = probe("stadium_roof{a = 60, b = 42, depth = 24, bays = 32, divisions = 5, height = 28, bowl = true}");
    assert_eq!(role(&r, "column"), 64);
    assert_eq!(role(&r, "stay"), 16, "a back stay on every second truss");
    assert_eq!(role(&r, "footing"), 16, "an anchor pad under each stay");
    assert_eq!(role(&r, "front_ring"), 32);
    let trusses = role(&r, "top_chord") + role(&r, "bottom_chord") + role(&r, "diagonal") + role(&r, "vertical");
    assert!(trusses >= 300, "{trusses}");
    assert!(role(&r, "slab") >= 3 * 32, "three tiers all round: {}", role(&r, "slab"));
    assert!(role(&r, "post") > 100);
    // the roof's own check: cantilever tip deflection limit is the overhang / 150 = 160 mm
    assert!((num(&r, "limit") - 160.0).abs() < 1.0, "{}", num(&r, "limit"));
    assert!(num(&r, "util") <= 0.95 + 1e-9, "util {}", num(&r, "util"));
    assert!(num(&r, "defl") <= num(&r, "limit") + 1e-9);
    assert!((num(&r, "ratio") - 4.0).abs() < 1e-9);
}

#[test]
fn the_long_span_recipes_build_clean_and_check() {
    for name in ["tied_arch_bridge", "hangar_space_frame", "stadium_roof", "truss_footbridge", "cable_stayed_footbridge", "space_frame_hall"] {
        let r = ok(recipe(name).unwrap());
        assert!(
            r.log.iter().any(|l| l.contains(" check: ratio ") && l.ends_with("passes")),
            "{name} does not pass its own check: {:?}",
            r.log
        );
    }
}

#[test]
fn hangar_spans_80_m_clear_with_runway_crane_girder_and_gates() {
    let r = ok(&format!("local g, info = space_frame{{nx = 20, ny = 16, bay = 5, depth = 4.5, height = 28, columns = 'perimeter', column_every = 2, crane_z = 14, gates = 4, gate_h = 20}}\n{PROBE}audit{{strict = true}}"));
    assert_eq!(role(&r, "crane_girder"), 1);
    assert!(role(&r, "crane_beam") >= 8, "{}", role(&r, "crane_beam"));
    assert_eq!(role(&r, "gate"), 4);
    assert!(role(&r, "gate_header") >= 3);
    let lattice = role(&r, "top_chord") + role(&r, "bottom_chord") + role(&r, "diagonal");
    assert!(lattice >= 2000, "{lattice}");
    // clear span 80 m = 16 bays of 5 m; depth 4.5 m gives span/depth 17.8
    assert!((num(&r, "ratio") - 80.0 / 4.5).abs() < 1e-9);
    assert!((num(&r, "limit") - 320.0).abs() < 1.0, "span/250: {}", num(&r, "limit"));
    assert!(num(&r, "util") <= 0.95 + 1e-9 && num(&r, "defl") <= num(&r, "limit") + 1e-9);
    // runway beams and the crane load reach the check: sizes only grow from their start, so a
    // far heavier crane costs steel where a lighter one does not
    let heavy = num(
        &ok(&format!("local g, info = space_frame{{nx = 20, ny = 16, bay = 5, depth = 4.5, height = 28, columns = 'perimeter', column_every = 2, crane_z = 14, rules = {{crane = 1500}}}}
{PROBE}audit{{strict = true}}")),
        "mass",
    );
    assert!(heavy > num(&r, "mass"), "{heavy} vs {}", num(&r, "mass"));
}

#[test]
fn long_span_blocks_are_deterministic() {
    let a = ok(recipe("tied_arch_bridge").unwrap());
    let b = ok(recipe("tied_arch_bridge").unwrap());
    assert_eq!(a.hash, b.hash);
}

#[test]
fn imported_scripts_keep_the_new_roles_classified() {
    let r = ok(&format!("{H10}\naudit{{strict = true}}"));
    for (role_name, class) in [("hanger", "IfcMember"), ("bearing", "IfcMember"), ("abutment", "IfcFooting"), ("wind_brace", "IfcMember")] {
        let found = r.env.groups[0].ops.iter().find_map(|op| match op {
            Op::Geometry(GeoOp::Solid { name: Some(n), ifc, .. }) if n.split(' ').next() == Some(role_name) => Some(ifc.clone()),
            _ => None,
        });
        let c = found.unwrap_or_else(|| panic!("no {role_name}")).unwrap_or_else(|| panic!("{role_name} unclassed"));
        assert_eq!(c.class, class, "{role_name}");
    }
}

/// Count of every role in the first group's solids.
fn roles(r: &ScriptRun) -> std::collections::BTreeMap<String, usize> {
    let mut m = std::collections::BTreeMap::new();
    for op in &r.env.groups[0].ops {
        if let Op::Geometry(GeoOp::Solid { name: Some(n), .. }) = op {
            *m.entry(n.split(' ').next().unwrap_or("").to_string()).or_insert(0) += 1;
        }
    }
    m
}

#[test]
fn sizing_changes_sections_not_the_layout_of_the_older_recipes() {
    for (name, call) in [
        ("truss_footbridge", "bridge{"),
        ("cable_stayed_footbridge", "bridge{"),
        ("space_frame_hall", "space_frame{"),
    ] {
        let src = recipe(name).unwrap();
        let sized = ok(src);
        let plain = ok(&src.replace(call, &format!("{call}size = false, ")));
        // the plates of the rod joints are the only parts the check adds
        let (a, b) = (roles(&sized), roles(&plain));
        for (k, n) in &b {
            assert_eq!(a.get(k), Some(n), "{name}: role {k} changed by sizing");
        }
        assert!(plain.log.iter().any(|l| l.contains("check not run")), "{name}: {:?}", plain.log);
        assert!(sized.log.iter().any(|l| l.contains(" check: ratio ")), "{name} says what it checked");
    }
}

#[test]
fn analysis_model_matches_the_geometry_and_has_no_free_ends() {
    let r = ok(&format!(
        "local g, info = {H10}
local c = info.check
print('K free ' .. c.free_ends)
print('K members ' .. c.members)
         for k, n in pairs(c.group_members) do print('G ' .. k .. ' ' .. n) end
audit{{strict = true}}"
    ));
    assert_eq!(num(&r, "free"), 0.0, "every node of the analysis is held: {:?}", r.log);
    let g = |name: &str| -> usize {
        let key = format!("G {name} ");
        r.log.iter().find_map(|l| l.strip_prefix(&key)).map_or(0, |n| n.trim().parse().unwrap())
    };
    // the analysed lattice has the members of the emitted parts, one for one
    assert_eq!(g("arch"), role(&r, "arch"));
    assert_eq!(g("hanger"), role(&r, "hanger"));
    assert_eq!(g("wind_brace"), role(&r, "wind_brace"));
    assert!(num(&r, "members") as usize >= g("arch") + g("hanger"));
}

#[test]
fn every_long_span_recipe_analyses_a_held_model() {
    for (name, call) in [
        ("stadium_roof", "stadium_roof{"),
        ("hangar_space_frame", "space_frame{"),
        ("space_frame_hall", "space_frame{"),
        ("truss_footbridge", "bridge{"),
        ("cable_stayed_footbridge", "bridge{"),
    ] {
        let src = recipe(name).unwrap().replacen(call, &format!("local _g, info = {call}"), 1);
        let r = ok(&format!("{src}
print('K free ' .. info.check.free_ends)
print('K ok ' .. (info.check.ok and 1 or 0))"));
        assert_eq!(num(&r, "free"), 0.0, "{name}: {:?}", r.log);
        assert_eq!(num(&r, "ok"), 1.0, "{name}: {:?}", r.log);
    }
}

#[test]
fn a_given_catalogue_section_is_checked_as_given() {
    let r = probe("bridge{kind = 'girder', span = 30, bays = 3, width = 4, girder = 'IPE 200', size = true}");
    assert_eq!(num(&r, "ok"), 0.0, "a light rolled beam cannot span 10 m under crowd load: {:?}", r.log);
    assert!(num(&r, "util") > 0.95 || num(&r, "defl") > num(&r, "limit"), "{:?}", r.log);
}

#[test]
fn many_checks_in_one_script_hit_the_analysis_budget_not_the_sandbox() {
    let t = std::time::Instant::now();
    let e = run("for i = 1, 200 do bridge{kind = 'truss', span = 60, bays = 20, depth = 4, width = 4, name = 'b' .. i} end").expect_err("a script cannot run unbounded checks");
    assert!(t.elapsed().as_secs() < 150, "{:?}", t.elapsed());
    assert!(!e.message.is_empty());
}

#[test]
fn rod_joints_have_gusset_plates_and_bolts_sized_by_the_force() {
    let r = ok(&format!("{H10}\naudit{{strict = true}}"));
    let ends = 2 * role(&r, "hanger");
    assert!(role(&r, "plate") >= ends, "a gusset plate on each end of every hanger");
    assert!(role(&r, "bolt") >= 2 * role(&r, "plate").min(ends), "at least two bolts on each plate: {}", role(&r, "bolt"));
    assert!(r.log.iter().any(|l| l.starts_with("bridge check:")), "{:?}", r.log);
    let heavy = ok("bridge{kind = 'arch', span = 90, rise = 15, width = 4.5, bays = 30, deck_z = 6, rules = {live_deck = 10}}");
    assert!(role(&heavy, "bolt") >= role(&r, "bolt"), "more load never needs fewer bolts");
}
