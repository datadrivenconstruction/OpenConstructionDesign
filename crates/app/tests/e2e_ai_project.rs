// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Project from a brief end to end: a scripted model fills the spec, the builder drafts the
//! building, Apply writes it in one undo step, and the result exports to IFC.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::fake::{Script, ScriptedProvider};
use ddc_ai::Mode;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use e2e_common::{count, dump_except, hash_of, ids};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}

/// Entities of each IFC class in the export.
fn ifc_classes(ed: &Editor) -> BTreeMap<String, usize> {
    let text = ddc_io::ifc::export_ifc(&ed.model).unwrap();
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for line in text.lines() {
        let Some((_, rest)) = line.split_once('=') else {
            continue;
        };
        let Some((class, _)) = rest.split_once('(') else {
            continue;
        };
        if class.starts_with("IFC") {
            *out.entry(class.trim().to_string()).or_default() += 1;
        }
    }
    out
}

fn has(classes: &BTreeMap<String, usize>, prefix: &str) -> usize {
    classes
        .iter()
        .filter(|(k, _)| k.as_str() == prefix || k.starts_with(&format!("{prefix}STANDARD")))
        .map(|(_, v)| *v)
        .sum()
}

#[test]
fn a_nine_storey_section_is_drafted_applied_in_one_step_and_exported() {
    let mut ed = editor();
    let levels0 = count(&ed.model, "level");
    let spec = json!({"typology": "apartment_section", "floors": 9, "lang": "ru"});
    let t = Instant::now();
    let (mut s, provider) = run(
        &mut ed,
        vec![tool("generate_project", json!({"spec": spec}))],
    );
    let draft_ms = t.elapsed().as_secs_f64() * 1000.0;
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let reply = parsed(&results[0].0);
    assert_eq!(
        count(&ed.model, "wall"),
        0,
        "nothing reaches the model before Apply"
    );
    let t = Instant::now();
    apply_and_check(&mut s, &mut ed);
    let apply_ms = t.elapsed().as_secs_f64() * 1000.0;
    // the ground floor is the current level; eight storeys and the roof are new
    assert_eq!(count(&ed.model, "level"), levels0 + 9);
    let rooms = ids(&ed.model, "room");
    assert_eq!(
        rooms.len() as i64,
        reply["project"]["counts"]["rooms"].as_i64().unwrap()
    );
    let open: Vec<i64> = rooms
        .iter()
        .copied()
        .filter(|r| ed.model.room_quantities(*r).unwrap().is_none())
        .collect();
    assert!(open.is_empty(), "rooms not closed in the model: {open:?}");
    assert_eq!(count(&ed.model, "stair"), stairs_of(&spec));
    assert_eq!(count(&ed.model, "roof"), roofs_of(&spec), "the roof parted round the stair heads");
    // guards: both sides of every stair, and the flat roof deck inside its parapet
    let rails = |w: &str| -> i64 {
        let q = format!("SELECT count(*) FROM railing WHERE {w}");
        ed.model.sql().query_row(&q, [], |r| r.get(0)).unwrap()
    };
    assert_eq!(rails("stair IS NOT NULL"), 2 * stairs_of(&spec));
    assert!(rails("stair IS NULL") >= 1, "no guard on the roof deck");
    let classes = ifc_classes(&ed);
    eprintln!(
        "gen1 timing e2e: 9-storey section draft {draft_ms:.0} ms, apply {apply_ms:.0} ms, \
         build {}; ifc {:?}",
        reply["ms"],
        classes
            .iter()
            .filter(
                |(k, _)| ["WALL", "DOOR", "WINDOW", "SLAB", "SPACE", "STAIR", "STOREY", "ROOF"]
                    .iter()
                    .any(|w| k.contains(w))
            )
            .collect::<Vec<_>>()
    );
    for c in [
        "IFCWALL",
        "IFCDOOR",
        "IFCWINDOW",
        "IFCSLAB",
        "IFCSPACE",
        "IFCBUILDINGSTOREY",
    ] {
        assert!(has(&classes, c) > 0, "{c} missing: {classes:?}");
    }
    assert_eq!(has(&classes, "IFCSPACE"), rooms.len());
    assert!(draft_ms < 60_000.0, "{draft_ms} ms");
}

fn ms_since(t: Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 10_000.0).round() / 10.0
}

fn phases_json(p: &[ddc_model::perf::Phase]) -> serde_json::Value {
    p.iter()
        .map(|x| (x.name.to_string(), json!([(x.ms * 10.0).round() / 10.0, x.calls])))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

fn openings_of(ed: &Editor, kind: &str) -> i64 {
    ed.model
        .sql()
        .query_row("SELECT count(*) FROM opening WHERE kind=?1", [kind], |r| r.get(0))
        .unwrap()
}

/// Every level's 3D scene from the shared wall cuts equals the one computed in full from all
/// cutters: the same meshes, numbers, tags, vertices and triangles, and the same issues.
fn assert_wall_cuts_keep_the_scene(ed: &mut Editor) {
    type Mesh = (u32, u32, Vec<[f32; 3]>, Vec<[u32; 3]>);
    let keep = ed.level;
    let levels: Vec<i64> = ed.levels().into_iter().map(|l| l.0).collect();
    let scenes = |ed: &mut Editor, on: bool| -> Vec<(Vec<Mesh>, Vec<String>)> {
        ed.wall_cuts.on = on;
        let mut out = vec![];
        for lv in &levels {
            ed.set_level(*lv);
            let (s, issues) = ed.scene_report();
            let one = |m: &std::sync::Arc<ddc_render::SceneMesh>| -> Mesh {
                (m.element, m.tag, m.positions.clone(), m.triangles.clone())
            };
            out.push((s.meshes.iter().map(one).collect(), issues));
        }
        out
    };
    let cached = scenes(ed, true);
    let full = scenes(ed, false);
    ed.wall_cuts.on = true;
    ed.set_level(keep);
    assert_eq!(cached.len(), levels.len());
    for (k, (a, b)) in cached.iter().zip(&full).enumerate() {
        assert!(!a.0.is_empty() || b.0.is_empty(), "level {k}: no scene");
        assert!(a.0 == b.0, "level {k}: the cached wall cut changed the scene");
        assert_eq!(a.1, b.1, "level {k}: the cached wall cut changed the issues");
    }
}

/// apply1: the 9-storey section is drafted and applied with the counts the builder promises,
/// the scratch copy equal to the live model; a release build keeps the tool within 0.5 s and
/// the Apply within 2 s. `DDC_PERF_LEDGER` (a file) gets one line of phase times per run,
/// `DDC_APPLY_TABLES` (a file) the hash of each table, to compare two builds row for row.
#[test]
fn a_nine_storey_section_drafts_and_applies_within_budget() {
    ddc_model::perf::enable(true);
    ddc_model::perf::take();
    let mut ed = editor();
    let levels0 = count(&ed.model, "level");
    let spec = json!({"typology": "apartment_section", "floors": 9, "lang": "ru"});
    let t = Instant::now();
    let (mut s, provider) = run(
        &mut ed,
        vec![tool("generate_project", json!({"spec": spec}))],
    );
    let turn_ms = ms_since(t);
    let draft = ddc_model::perf::take();
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let reply = parsed(&results[0].0);
    let tool_ms = reply["ms"]["draft"].as_f64().unwrap();
    if !s.can_apply() {
        // over the size cap: applied the way the card offers it, after "Allow large"
        s.allow_large(true);
        assert!(s.wait_idle(&mut ed, WAIT));
    }
    assert!(s.can_apply(), "cannot apply: {:?}", plan_of(&s).issues);
    ddc_model::perf::take();
    let t = Instant::now();
    s.apply(&mut ed).unwrap();
    let apply_ms = ms_since(t);
    let applied = ddc_model::perf::take();
    let t = Instant::now();
    assert!(s.wait_idle(&mut ed, WAIT), "the worker did not take the Apply");
    let worker_ms = ms_since(t);
    let worker = ddc_model::perf::take();
    ddc_model::perf::enable(false);

    let got = json!({
        "levels": count(&ed.model, "level") - levels0,
        "walls": count(&ed.model, "wall"),
        "doors": openings_of(&ed, "door"),
        "windows": openings_of(&ed, "window"),
        "rooms": count(&ed.model, "room"),
        "slabs": count(&ed.model, "slab"),
        "stairs": count(&ed.model, "stair"),
        "roofs": count(&ed.model, "roof"),
    });
    let want = json!({
        "levels": 9, "walls": 234, "doors": 199, "windows": 126, "rooms": 207, "slabs": 9,
        "stairs": 8, "roofs": 1,
    });
    assert_copy_is_live(&s, &ed);
    // the value records carry the session and the clock: compared by count, not by row
    let mut skip: Vec<&str> = NOT_MODEL.to_vec();
    skip.push("value_src");
    let tables = dump_except(&ed.model, &skip);
    let model_hash = format!("{:016x}", hash_of(&tables));
    if let Ok(p) = std::env::var("DDC_APPLY_TABLES") {
        let lines: Vec<String> = tables
            .iter()
            .map(|t| format!("{} {} {:016x}", t.0, t.1.len(), hash_of(std::slice::from_ref(t))))
            .collect();
        std::fs::write(p, lines.join("\n")).unwrap();
    }
    let line = json!({
        "bench": "apply_section9",
        "label": std::env::var("DDC_PERF_LABEL").unwrap_or_default(),
        "release": !cfg!(debug_assertions),
        "tool_ms": tool_ms,
        "turn_ms": turn_ms,
        "apply_ms": apply_ms,
        "worker_ms": worker_ms,
        "counts": got,
        "value_src": count(&ed.model, "value_src"),
        "model_hash": model_hash,
        "draft": phases_json(&draft),
        "apply": phases_json(&applied),
        "worker": phases_json(&worker),
    });
    eprintln!("apply1 bench: {line}");
    if let Ok(p) = std::env::var("DDC_PERF_LEDGER") {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap();
        writeln!(f, "{line}").unwrap();
    }
    assert_eq!(got, want);
    assert_wall_cuts_keep_the_scene(&mut ed);
    if !cfg!(debug_assertions) {
        assert!(tool_ms <= 500.0, "generate_project took {tool_ms} ms");
        assert!(apply_ms <= 2000.0, "Apply took {apply_ms} ms");
    }
}

#[test]
fn a_revision_rebuilds_the_same_item_and_applies_once() {
    let mut ed = editor();
    let (mut s, provider) = run(
        &mut ed,
        vec![
            tool(
                "generate_project",
                json!({"spec": {"typology": "office", "floors": 2}}),
            ),
            tool(
                "revise_project",
                json!({"changes": {"floors": 3, "corridor": 2.0}}),
            ),
        ],
    );
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let rev = parsed(&results[1].0);
    assert_eq!(rev["item"], 1);
    assert_eq!(plan_of(&s).items.len(), 1, "the revision is the same item");
    let before = rev["created_before"]["wall"].as_i64().unwrap();
    let after = rev["created_after"]["wall"].as_i64().unwrap();
    assert!(after > before, "{before} -> {after}");
    apply_and_check(&mut s, &mut ed);
    assert_eq!(count(&ed.model, "wall"), after);
    let width: f64 = ed
        .model
        .sql()
        .query_row("SELECT max(ay) FROM wall", [], |r| r.get(0))
        .unwrap();
    assert!(width > 0.0);
}

#[test]
fn an_office_with_vav_applies_its_mechanical_design_as_connected_elements() {
    use ddc_model::SystemKind;
    let mut ed = editor();
    let spec = json!({"typology": "office", "floors": 3, "rules": {"mep.hvac.system": "vav"}});
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    apply_and_check(&mut s, &mut ed);
    let m = &ed.model;
    let boxes = comps_of(m, &["HVAC VAV Box HW"]);
    let ahu = comps_of(m, &["HVAC Air Handling Unit CHW HW"]);
    let pumps = comps_of(m, &["HVAC Pump CHW", "HVAC Pump HW"]);
    assert!(boxes.len() >= 3, "{} VAV boxes", boxes.len());
    assert_eq!(ahu.len(), 1);
    assert_eq!(pumps.len(), 2);
    // every port of the units is joined to its system
    let linked: std::collections::BTreeSet<(i64, String)> = m
        .mep_port_links(None)
        .unwrap()
        .into_iter()
        .filter(|l| l.system.is_some())
        .map(|l| (l.comp, l.connector))
        .collect();
    let joined = |c: i64, names: &[&str]| {
        for n in names {
            assert!(linked.contains(&(c, n.to_string())), "unit {c}: {n} is not joined");
        }
    };
    for &b in &boxes {
        joined(b, &["Inlet", "Outlet", "hw_flow", "hw_return"]);
    }
    joined(ahu[0], &["Supply", "Return", "chw_flow", "chw_return", "hw_flow", "hw_return"]);
    for &p in &pumps {
        joined(p, &["Suction", "Discharge"]);
    }
    // ducts with their fittings, water pipes, and networks the checks find whole
    let fits = m.mep_fittings(None).unwrap();
    let kind = |k: &str| fits.iter().filter(|f| f.kind.name() == k).count();
    assert!(kind("elbow") > 0 && kind("tee") > 0, "{} elbows, {} tees", kind("elbow"), kind("tee"));
    let mut runs: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for c in m.mep_check_all().unwrap() {
        assert_eq!(c.errors(), 0, "system {}: {:?}", c.name, c.issues);
        let k = match c.kind {
            SystemKind::SupplyAir | SystemKind::ExhaustAir => "air",
            SystemKind::ChilledWater => "chw",
            SystemKind::Heating => "hw",
            _ => "other",
        };
        *runs.entry(k).or_default() += c.runs.len();
    }
    assert!(runs.get("air").copied().unwrap_or(0) > boxes.len(), "{runs:?}");
    assert!(runs.get("chw").copied().unwrap_or(0) > 0 && runs.get("hw").copied().unwrap_or(0) > 0, "{runs:?}");
    eprintln!("hvac2 e2e: {} VAV boxes, {} fittings, runs {runs:?}", boxes.len(), fits.len());
}

#[test]
fn every_typology_applies_and_exports() {
    for (t, floors) in [("house", 2), ("office", 2), ("school", 2)] {
        let mut ed = editor();
        let spec = json!({"typology": t, "floors": floors});
        let (mut s, provider) = run(
            &mut ed,
            vec![tool("generate_project", json!({"spec": spec}))],
        );
        let results = tool_results(&provider);
        assert!(results.iter().all(|r| !r.1), "{t}: {results:?}");
        apply_and_check(&mut s, &mut ed);
        let classes = ifc_classes(&ed);
        for c in ["IFCWALL", "IFCDOOR", "IFCWINDOW", "IFCSLAB", "IFCSPACE"] {
            assert!(has(&classes, c) > 0, "{t}: {c} missing");
        }
        eprintln!(
            "gen1 typology e2e {t}: walls {} doors+windows {} rooms {}",
            count(&ed.model, "wall"),
            count(&ed.model, "opening"),
            count(&ed.model, "room")
        );
    }
}

/// Walls of the model on an arc axis.
fn arc_walls(ed: &Editor) -> Vec<i64> {
    ids(&ed.model, "wall")
        .into_iter()
        .filter(|&id| ed.model.wall_bulge(id).unwrap() != 0.0)
        .collect()
}

#[test]
fn a_curved_facade_applies_as_one_arc_wall_per_storey_exports_and_undoes() {
    let curves = |ed: &Editor| has(&ifc_classes(ed), "IFCTRIMMEDCURVE");
    let mut flat = editor();
    let (mut s, _) = run(
        &mut flat,
        vec![tool("generate_project", json!({"spec": {"typology": "office", "floors": 2}}))],
    );
    apply_and_check(&mut s, &mut flat);
    let mut ed = editor();
    let spec = json!({"typology": "office", "floors": 2, "design": {"curve": 2.5}});
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let walls0 = count(&ed.model, "wall");
    apply_and_check(&mut s, &mut ed);
    // one arc wall bows out the south facade of each storey, the chords are gone
    let arcs = arc_walls(&ed);
    assert_eq!(arcs.len(), 2, "{arcs:?}");
    for &id in &arcs {
        assert!(ed.model.wall_bulge(id).unwrap() > 0.0);
    }
    assert!(count(&ed.model, "wall") < count(&flat.model, "wall") + 2);
    // the arc axes reach the IFC file
    assert!(curves(&ed) >= curves(&flat) + 2, "{} vs {}", curves(&ed), curves(&flat));
    ed.undo();
    assert_eq!(count(&ed.model, "wall"), walls0);
    assert!(arc_walls(&ed).is_empty());
}

#[test]
fn a_bad_spec_is_refused_with_every_problem_and_no_item() {
    let mut ed = editor();
    let spec = json!({"typology": "office", "flors": 3, "rooms": [{"kind": "ofice", "area": 30}]});
    let (s, provider) = run(
        &mut ed,
        vec![tool("generate_project", json!({"spec": spec}))],
    );
    let results = tool_results(&provider);
    assert!(results[0].1, "refused");
    let text = &results[0].0;
    assert!(text.contains("did you mean 'floors'"), "{text}");
    assert!(text.contains("did you mean 'office'"), "{text}");
    assert!(s.plan().map_or(true, |p| p.items.is_empty()));
}

/// Stairs a project of `spec` holds: the flights of every storey, as the fire rules size them
/// (a storey whose occupants need two exits has two stairs); the top storey's run on to the
/// roof in a tall building (`fire.stair_to_roof`).
fn stairs_of(spec: &serde_json::Value) -> i64 {
    let s = ddc_gen::project::parse_spec(spec).unwrap();
    let p = ddc_gen::project::build_project(&s).unwrap();
    p.storeys.iter().map(|st| st.layout.stairs.len() as i64).sum()
}

/// Roof elements of a project of `spec`: the roof and the wings' roofs, a flat roof parted
/// round every stair head that comes out on it.
fn roofs_of(spec: &serde_json::Value) -> i64 {
    let s = ddc_gen::project::parse_spec(spec).unwrap();
    let p = ddc_gen::project::build_project(&s).unwrap();
    1 + p.extra_roofs.len() as i64
}

fn two_turns(first: Script, second: Script) -> Arc<ScriptedProvider> {
    let done = || vec![Script::Text("done".to_string())];
    ScriptedProvider::new(vec![vec![first], done(), vec![second], done()])
}

fn prop_count(ed: &Editor, key: &str, value: i64) -> i64 {
    let sql = "SELECT count(*) FROM prop WHERE kind='wall' AND key=?1 AND value=?2";
    ed.model
        .sql()
        .query_row(sql, rusqlite::params![key, value], |r| r.get(0))
        .unwrap()
}

/// Fire-rated walls of the generator become wall types named for what they separate and their
/// resistance; the walls state it, and so do the doors in them.
#[test]
fn rated_walls_get_their_own_types_and_their_doors_a_rating() {
    let mut ed = editor();
    let provider = ScriptedProvider::new(vec![
        vec![tool("generate_project", json!({"spec": {"typology": "office", "floors": 8}}))],
        vec![Script::Text("done".to_string())],
    ]);
    let mut s = start(&mut ed, cfg(provider));
    turn(&mut s, &mut ed, "make it", Mode::Propose);
    apply_and_check(&mut s, &mut ed);
    let sql = ed.model.sql();
    let rated_types: i64 = sql
        .query_row("SELECT count(*) FROM wall_type WHERE name LIKE 'Gen % Wall EI%'", [], |r| r.get(0))
        .unwrap();
    assert!(rated_types >= 1, "no rated wall type");
    let rated_walls: i64 = sql
        .query_row(
            "SELECT count(*) FROM wall w JOIN wall_type t ON t.id = w.wtype WHERE t.name LIKE 'Gen % Wall EI%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let per_storey: i64 = sql.query_row("SELECT count(*) FROM level", [], |r| r.get(0)).unwrap();
    assert!(rated_walls >= per_storey - 1, "{rated_walls} rated wall(s) on {per_storey} levels");
    let stated: i64 = sql
        .query_row(
            "SELECT count(*) FROM prop p JOIN wall w ON p.kind='wall' AND p.ref = w.id              JOIN wall_type t ON t.id = w.wtype              WHERE p.key='Pset_WallCommon.FireRating' AND p.value LIKE 'EI%' AND t.name LIKE 'Gen % Wall EI%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stated, rated_walls, "every rated wall states its resistance");
    let doors: i64 = sql
        .query_row("SELECT count(*) FROM prop WHERE kind='opening' AND key='Pset_DoorCommon.FireRating' AND value LIKE 'EI%'", [], |r| r.get(0))
        .unwrap();
    assert!(doors >= 1, "no rated door");
    let text = ddc_io::ifc::export_ifc(&ed.model).unwrap();
    assert!(text.contains("EI") && text.contains("'FireRating'"));
}

/// Pipe count of each sprinkler-kind system: the sprinkler network and every standpipe.
fn fire_systems(ed: &Editor) -> BTreeMap<String, i64> {
    let sql = "SELECT s.name, (SELECT count(*) FROM mep_run r WHERE r.system = s.id) \
               FROM mep_system s WHERE s.kind = 'sprinkler'";
    let mut st = ed.model.sql().prepare(sql).unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|x| x.unwrap()).collect()
}

/// The standpipes sync after the sprinklers in their own slot block: a revision that changes
/// the sprinkler network rebuilds both and neither sync deletes the other's systems or pipes.
#[test]
fn standpipes_and_sprinklers_survive_each_others_sync() {
    let mut ed = editor();
    let provider = two_turns(
        tool("generate_project", json!({"spec": {"typology": "office", "floors": 11}})),
        tool("revise_project", json!({"changes": {"floors": 12}})),
    );
    let mut s = start(&mut ed, cfg(provider.clone()));
    turn(&mut s, &mut ed, "make it", Mode::Propose);
    apply_and_check(&mut s, &mut ed);
    let first = fire_systems(&ed);
    let pipes = |m: &BTreeMap<String, i64>| m.iter().filter(|(n, _)| n.starts_with("Standpipe")).map(|x| *x.1).collect::<Vec<i64>>();
    let sp1 = pipes(&first);
    assert!(!sp1.is_empty() && sp1.iter().all(|n| *n > 0), "{first:?}");
    assert!(first.values().all(|n| *n > 0), "a fire system lost its pipes: {first:?}");
    turn(&mut s, &mut ed, "twelve floors", Mode::Propose);
    apply_and_check(&mut s, &mut ed);
    let second = fire_systems(&ed);
    let sp2 = pipes(&second);
    assert_eq!(sp1.len(), sp2.len(), "{first:?} -> {second:?}");
    // one riser and one valve branch more per standpipe for the new storey
    assert!(sp1.iter().zip(&sp2).all(|(a, b)| b > a), "{first:?} -> {second:?}");
    assert!(second.values().all(|n| *n > 0), "a fire system lost its pipes: {second:?}");
    assert_eq!(first.len(), second.len(), "{first:?} -> {second:?}");
}

#[test]
fn an_applied_project_is_revised_by_a_diff_and_undone_in_one_step() {
    let mut ed = editor();
    let spec = json!({"typology": "apartment_section", "floors": 9});
    let provider = two_turns(
        tool("generate_project", json!({"spec": spec})),
        tool("revise_project", json!({"changes": {"floors": 12}})),
    );
    let mut s = start(&mut ed, cfg(provider.clone()));
    turn(&mut s, &mut ed, "make it", Mode::Propose);
    apply_and_check(&mut s, &mut ed);
    let walls9 = ids(&ed.model, "wall");
    let kept9: Vec<(&str, Vec<i64>)> = ["stair", "shaft", "grid", "roof", "room", "opening"]
        .into_iter()
        .map(|t| (t, ids(&ed.model, t)))
        .collect();
    let stairs9 = count(&ed.model, "stair");
    let levels9 = count(&ed.model, "level");
    assert_eq!(stairs9, stairs_of(&spec));
    // load bearing walls and their fire rating are the properties the IFC export reads
    assert!(prop_count(&ed, "Pset_WallCommon.LoadBearing", 1) > 0);
    assert!(prop_count(&ed, "Pset_WallCommon.LoadBearing", 0) > 0);
    let text = ddc_io::ifc::export_ifc(&ed.model).unwrap();
    assert!(text.contains("'LoadBearing'") && text.contains("'FireRating'"));
    // every generated element carries its stable key, and no two share one
    let keys9 = generator_keys(&ed);
    let distinct: std::collections::BTreeSet<&String> = keys9.values().collect();
    assert_eq!(distinct.len(), keys9.len(), "keys are unique");
    for kind in ["wall", "room", "opening", "slab", "roof", "stair"] {
        let have = keys9.keys().filter(|k| k.0 == kind).count() as i64;
        assert_eq!(have, count(&ed.model, kind), "every {kind} has a key");
    }

    let t = Instant::now();
    turn(&mut s, &mut ed, "twelve floors", Mode::Propose);
    let results = tool_results(&provider);
    let (last, err) = results.last().unwrap();
    assert!(!err, "{last}");
    let reply = parsed(last);
    assert_eq!(reply["project_id"], "P1", "{reply}");
    let deleted = reply["changes"]["deleted"].as_i64().unwrap();
    apply_and_check(&mut s, &mut ed);
    eprintln!(
        "gen1 timing e2e: revise 9 -> 12 storeys {:.0} ms, changes {}",
        t.elapsed().as_secs_f64() * 1000.0,
        reply["changes"]
    );
    assert_eq!(count(&ed.model, "stair"), stairs_of(&json!({"typology": "apartment_section", "floors": 12})));
    assert_eq!(count(&ed.model, "roof"), roofs_of(&json!({"typology": "apartment_section", "floors": 12})));
    let walls12 = ids(&ed.model, "wall");
    let kept = walls9.iter().filter(|w| walls12.contains(w)).count();
    assert_eq!(kept, walls9.len(), "every wall of floors 1-9 keeps its id");
    assert_eq!(deleted, 0, "nothing is deleted and created again");
    // the roof moves up and the top floor's stairs and shafts reach new levels: in place
    for (t, before) in &kept9 {
        let now = ids(&ed.model, t);
        let lost: Vec<&i64> = before.iter().filter(|i| !now.contains(i)).collect();
        assert!(lost.is_empty(), "{t} lost ids {lost:?}");
    }
    // an element of floors 1-9 keeps its key when floors are added
    let keys12 = generator_keys(&ed);
    let moved: Vec<_> = keys9
        .iter()
        .filter(|(el, k)| keys12.get(*el) != Some(*k))
        .collect();
    // the old top floor gains its stair and the roof key stays: nothing may change key
    assert!(moved.is_empty(), "keys changed: {:?}", &moved[..moved.len().min(5)]);
    ed.undo();
    assert_eq!(count(&ed.model, "stair"), stairs9);
    assert_eq!(count(&ed.model, "level"), levels9);
    assert_eq!(ids(&ed.model, "wall"), walls9);
}

/// Generator key by (table, element id).
fn generator_keys(ed: &Editor) -> BTreeMap<(String, i64), String> {
    let mut st = ed
        .model
        .sql()
        .prepare("SELECT kind, ref, CAST(value AS TEXT) FROM prop WHERE key=?1")
        .unwrap();
    st.query_map([ddc_model::KEY_PROP], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn a_hand_edit_is_kept_and_named_when_the_revision_would_change_it() {
    let mut ed = editor();
    let provider = two_turns(
        tool(
            "generate_project",
            json!({"spec": {"typology": "office", "floors": 2}}),
        ),
        tool("revise_project", json!({"changes": {"lang": "ru"}})),
    );
    let mut s = start(&mut ed, cfg(provider.clone()));
    turn(&mut s, &mut ed, "make it", Mode::Propose);
    apply_and_check(&mut s, &mut ed);
    let room = ids(&ed.model, "room")[0];
    let spec = ed
        .model
        .sql()
        .query_row(
            "SELECT level, number, px, py, height FROM room WHERE id=?1",
            [room],
            |r| {
                Ok(ddc_model::RoomSpec {
                    level: r.get(0)?,
                    name: "Server room".to_string(),
                    number: r.get(1)?,
                    point: (r.get(2)?, r.get(3)?),
                    height: r.get(4)?,
                })
            },
        )
        .unwrap();
    let edit = ddc_model::Edit::Room { id: room, spec };
    ed.model.apply_edits(&[edit]).unwrap();
    ed.reload().unwrap();
    turn(&mut s, &mut ed, "in Russian", Mode::Propose);
    let results = tool_results(&provider);
    let (last, err) = results.last().unwrap();
    assert!(!err, "{last}");
    let reply = parsed(last);
    let kept = reply["kept_hand_edits"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        kept.iter().any(|k| k["element"] == format!("room {room}")),
        "{reply}"
    );
    apply_and_check(&mut s, &mut ed);
    let name: String = ed
        .model
        .sql()
        .query_row("SELECT name FROM room WHERE id=?1", [room], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "Server room");
}

/// A room renamed by hand is offered for pinning; the pin is a geometry lock in the stored
/// brief, and a revision that changes the programme keeps it instead of refusing.
#[test]
fn a_hand_edited_room_is_pinned_and_a_revision_keeps_it() {
    use ddc_app::ai::project_plan::{self, PinAt};
    use ddc_gen::project::locks;
    let rooms = |beds: u32| {
        json!([{"kind": "living", "area": 25}, {"kind": "kitchen", "area": 12},
               {"kind": "bedroom", "area": 14, "count": beds}, {"kind": "bath", "area": 5}, {"kind": "wc", "area": 2}])
    };
    let mut ed = editor();
    let provider = two_turns(
        tool("generate_project", json!({"spec": {"typology": "house", "floors": 1, "rooms": rooms(3)}})),
        tool("revise_project", json!({"changes": {"rooms": rooms(2)}})),
    );
    let mut s = start(&mut ed, cfg(provider.clone()));
    turn(&mut s, &mut ed, "make it", Mode::Propose);
    apply_and_check(&mut s, &mut ed);
    assert!(project_plan::hand_edited(&ed.model).unwrap().is_empty(), "nothing edited yet");
    let room: i64 = ed
        .model
        .sql()
        .query_row("SELECT id FROM room WHERE name LIKE '%edroom%' ORDER BY id LIMIT 1", [], |r| r.get(0))
        .unwrap();
    ed.model
        .sql()
        .execute("UPDATE room SET name='Study' WHERE id=?1", [room])
        .unwrap();
    let edited = project_plan::hand_edited(&ed.model).unwrap();
    let (pid, at) = edited
        .iter()
        .find(|p| matches!(p.1, PinAt::Room { .. }))
        .cloned()
        .unwrap_or_else(|| panic!("{edited:?}"));
    project_plan::pin(&mut ed.model, &pid, &at).unwrap();
    let input = |ed: &Editor| project_plan::stored(&ed.model).unwrap().last().unwrap().1["input"].clone();
    let pinned = locks::of_brief(&input(&ed)).unwrap();
    assert_eq!(pinned.len(), 1);
    // pinning twice adds nothing
    project_plan::pin(&mut ed.model, &pid, &at).unwrap();
    assert_eq!(locks::of_brief(&input(&ed)).unwrap().len(), 1);

    turn(&mut s, &mut ed, "two bedrooms", Mode::Propose);
    let results = tool_results(&provider);
    let (last, err) = results.last().unwrap();
    assert!(!err, "{last}");
    let reply = parsed(last);
    assert!(reply["refused_by_locks"].is_null(), "{reply}");
    apply_and_check(&mut s, &mut ed);
    let now = input(&ed);
    let b = project_plan::build(&now).unwrap();
    assert!(pinned[0].holds(&now, &b.spec, &b.project), "{now}");
    let beds = b.project.storeys[0].rooms.iter().filter(|r| r.kind == ddc_gen::project::RoomKind::Bedroom).count();
    assert_eq!(beds, 2);
}

#[test]
fn stairs_shafts_grids_and_the_roof_change_in_place() {
    let mut ed = editor();
    let spec = json!({"typology": "house", "floors": 2, "roof": {"kind": "pitched", "slope": 30}});
    let provider = two_turns(
        tool("generate_project", json!({"spec": spec})),
        tool(
            "revise_project",
            json!({"changes": {"floor_height": 3.3, "roof": {"slope": 35}}}),
        ),
    );
    let mut s = start(&mut ed, cfg(provider.clone()));
    turn(&mut s, &mut ed, "make it", Mode::Propose);
    apply_and_check(&mut s, &mut ed);
    let tables = ["stair", "shaft", "grid", "roof"];
    let before: Vec<Vec<i64>> = tables.iter().map(|t| ids(&ed.model, t)).collect();
    let rows = |ed: &Editor, t: &str| -> Vec<String> {
        let sql = format!("SELECT * FROM {t} ORDER BY id");
        let mut st = ed.model.sql().prepare(&sql).unwrap();
        let n = st.column_count();
        st.query_map([], |r| {
            Ok((0..n)
                .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                .collect::<Vec<_>>()
                .join(";"))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    };
    let rows_before: Vec<Vec<String>> = tables.iter().map(|t| rows(&ed, t)).collect();
    turn(&mut s, &mut ed, "taller floors", Mode::Propose);
    let results = tool_results(&provider);
    let (last, err) = results.last().unwrap();
    assert!(!err, "{last}");
    let reply = parsed(last);
    assert_eq!(reply["changes"]["deleted"], 0, "{reply}");
    apply_and_check(&mut s, &mut ed);
    for (k, t) in tables.iter().enumerate() {
        assert_eq!(ids(&ed.model, t), before[k], "{t} ids");
    }
    // the stair climbs a taller floor and the roof is steeper: the rows did change
    assert_ne!(rows(&ed, "stair"), rows_before[0]);
    assert_ne!(rows(&ed, "roof"), rows_before[3]);
}

fn level_names(ed: &Editor) -> Vec<String> {
    let mut st = ed
        .model
        .sql()
        .prepare("SELECT name FROM level ORDER BY elevation")
        .unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn fewer_floors_delete_the_project_levels_and_more_bring_them_back() {
    let mut ed = editor();
    let levels0 = count(&ed.model, "level");
    let spec = json!({"typology": "apartment_section", "floors": 12});
    let done = || vec![Script::Text("done".to_string())];
    let revise = |n: u32| tool("revise_project", json!({"changes": {"floors": n}}));
    let provider = ScriptedProvider::new(vec![
        vec![tool("generate_project", json!({"spec": spec}))],
        done(),
        vec![revise(9)],
        done(),
        vec![revise(12)],
        done(),
    ]);
    let mut s = start(&mut ed, cfg(provider.clone()));
    turn(&mut s, &mut ed, "twelve", Mode::Propose);
    apply_and_check(&mut s, &mut ed);
    let names12 = level_names(&ed);
    let roof12: i64 = ed
        .model
        .sql()
        .query_row("SELECT id FROM roof", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count(&ed.model, "level"), levels0 + 12);

    turn(&mut s, &mut ed, "nine", Mode::Propose);
    let results = tool_results(&provider);
    let (last, err) = results.last().unwrap();
    assert!(!err, "{last}");
    apply_and_check(&mut s, &mut ed);
    let names9 = level_names(&ed);
    assert_eq!(count(&ed.model, "level"), levels0 + 9, "{names9:?}");
    assert_eq!(
        names9.last().map(String::as_str),
        Some("Roof"),
        "{names9:?}"
    );
    assert!(
        !names9.iter().any(|n| n == "Level 10" || n.contains('(')),
        "{names9:?}"
    );
    let (roof9, on): (i64, String) = ed
        .model
        .sql()
        .query_row(
            "SELECT r.id, l.name FROM roof r JOIN level l ON l.id=r.level",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(roof9, roof12, "the roof moves down in place");
    assert_eq!(on, "Roof");
    assert_eq!(count(&ed.model, "stair"), 8);

    turn(&mut s, &mut ed, "twelve again", Mode::Propose);
    let results = tool_results(&provider);
    let (last, err) = results.last().unwrap();
    assert!(!err, "{last}");
    apply_and_check(&mut s, &mut ed);
    assert_eq!(
        level_names(&ed),
        names12,
        "the same levels and names as the first time"
    );
    assert_eq!(count(&ed.model, "stair"), stairs_of(&json!({"typology": "apartment_section", "floors": 12})));
    ed.undo();
    assert_eq!(level_names(&ed), names9);
}

/// Top of the project's roof: the elevation of the level it stands on.
fn roof_z(ed: &Editor) -> f64 {
    ed.model
        .sql()
        .query_row("SELECT l.elevation FROM roof r JOIN level l ON l.id=r.level", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_fortieth_floor_tower_gets_a_forty_first_in_place_and_undoes() {
    let mut ed = editor();
    let spec = json!({"typology": "office", "floors": 40, "floor_height": 3.6});
    let done = || vec![Script::Text("done".to_string())];
    let provider = ScriptedProvider::new(vec![
        vec![tool("generate_project", json!({"spec": spec}))],
        done(),
        vec![tool("revise_project", json!({"changes": {"floors": 41}}))],
        done(),
    ]);
    let mut s = start(&mut ed, cfg(provider.clone()));
    turn(&mut s, &mut ed, "a tower", Mode::Propose);
    apply_and_check(&mut s, &mut ed);
    let (levels40, walls40, roofs40, z40) =
        (count(&ed.model, "level"), ids(&ed.model, "wall"), ids(&ed.model, "roof"), roof_z(&ed));
    let names40 = level_names(&ed);
    let keys40 = generator_keys(&ed);

    turn(&mut s, &mut ed, "one more floor", Mode::Propose);
    let results = tool_results(&provider);
    let (last, err) = results.last().unwrap();
    assert!(!err, "the 41st floor is refused: {last}");
    apply_and_check(&mut s, &mut ed);
    assert_eq!(count(&ed.model, "level"), levels40 + 1);
    assert_eq!(ids(&ed.model, "roof"), roofs40, "the roof keeps its elements");
    let z41 = roof_z(&ed);
    assert!((z41 - z40 - 3.6).abs() < 1e-6, "the roof rises one storey: {z40} -> {z41}");
    let walls41 = ids(&ed.model, "wall");
    let kept = walls40.iter().filter(|w| walls41.contains(w)).count();
    assert_eq!(kept, walls40.len(), "every wall of the 40 floors keeps its id");
    assert!(walls41.len() > walls40.len(), "the new storey brings walls");
    let keys41 = generator_keys(&ed);
    let moved: Vec<_> = keys40
        .iter()
        // the building's own parts; the fire mains are sized by the tower's height
        .filter(|(el, _)| ["wall", "room", "opening", "slab", "roof", "stair"].contains(&el.0.as_str()))
        .filter(|(el, k)| keys41.get(*el) != Some(*k))
        .collect();
    assert!(moved.is_empty(), "keys changed: {:?}", &moved[..moved.len().min(5)]);

    ed.undo();
    assert_eq!(level_names(&ed), names40);
    assert_eq!(ids(&ed.model, "wall"), walls40);
    assert!((roof_z(&ed) - z40).abs() < 1e-9, "undo lowers the roof");
}

/// Gallery harness: every `*.json` brief in `DDC_GEN_SPECS` goes through the model's tool call
/// and Apply, then lands as `<name>.ddc` in `DDC_GEN_OUT` for `--open-project` screenshots.
#[test]
#[ignore = "writes project files; run with DDC_GEN_SPECS and DDC_GEN_OUT"]
fn gallery_projects_are_drafted_applied_and_saved() {
    let (Ok(dir), Ok(out)) = (std::env::var("DDC_GEN_SPECS"), std::env::var("DDC_GEN_OUT")) else {
        return;
    };
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    for f in files {
        let spec: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        let name = f.file_stem().unwrap().to_string_lossy().to_string();
        let mut ed = editor();
        let t = Instant::now();
        // {"generator": id, "params": {...}} runs a registered generator instead of a brief
        let call = match spec["generator"].as_str() {
            Some(id) => tool("run_generator", json!({"id": id, "params": spec["params"], "variants": 1})),
            None => tool("generate_project", json!({"spec": spec})),
        };
        let (mut s, provider) = run(&mut ed, vec![call]);
        let results = tool_results(&provider);
        if results.iter().any(|r| r.1) {
            eprintln!("gallery {name}: REFUSED {}", results[0].0);
            continue;
        }
        apply_and_check(&mut s, &mut ed);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let target = std::path::Path::new(&out).join(format!("{name}.ddc"));
        let _ = std::fs::remove_file(&target);
        ed.save_project_as(&target).unwrap();
        eprintln!(
            "gallery {name}: {ms:.0} ms, walls {}, doors+windows {}, rooms {}, stairs {}",
            count(&ed.model, "wall"),
            count(&ed.model, "opening"),
            count(&ed.model, "room"),
            count(&ed.model, "stair")
        );
    }
}

#[test]
fn a_two_storey_house_with_services_applies_with_clean_networks_and_sized_circuits() {
    let mut ed = editor();
    let spec = json!({"typology": "house", "floors": 2, "services": true});
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    apply_and_check(&mut s, &mut ed);
    let facts = assert_services(&ed.model, true);
    assert!(facts.water_runs > 0 && facts.power_runs > 0, "{facts:?}");
    assert!(facts.wet > 0, "{facts:?}");
    assert!(facts.circuits > 0, "{facts:?}");
    // every circuit carries a breaker and a cable from the table
    for c in ed.model.circuits(None).unwrap() {
        assert!(c.spec.cable >= 1.5 && c.spec.breaker >= 6.0, "{:?}", c.spec);
    }
}

#[test]
fn a_split_level_project_on_the_model_terrain_drafts_and_applies() {
    let mut ed = editor();
    let mut points = vec![];
    for i in -12..=24 {
        for j in -12..=16 {
            let (x, y) = (f64::from(i) * 5.0, f64::from(j) * 5.0);
            points.push((x, y, 50.0 - 0.3 * x));
        }
    }
    let topo = ddc_model::TopoSpec {
        name: "Ground".into(),
        thickness: 2.0,
        material: "earth".into(),
        points,
        ..Default::default()
    };
    ed.model.add_toposolid(&topo).unwrap();
    ed.reload().unwrap();
    let levels0 = count(&ed.model, "level");
    // a corridor wide enough for a platform lift beside each half-level flight
    let spec = json!({"typology": "office", "floors": 3, "roof": {"kind": "flat"}, "corridor": 2.6,
        "site": {"terrain": "model"}});
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let reply = parsed(&results[0].0);
    let metrics = &reply["project"]["metrics"];
    assert!(metrics["split_drop"].as_f64().is_some(), "{metrics}");
    assert!(metrics["split_platform_lifts"].as_f64().is_some_and(|n| n >= 1.0), "{metrics}");
    assert!(metrics["cut_m3"].as_f64().is_some_and(|c| c > 0.0), "{metrics}");
    apply_and_check(&mut s, &mut ed);
    // two storeys and the roof above the current level, and a lower level under each storey
    assert_eq!(count(&ed.model, "level"), levels0 + 2 + 1 + 3);
    let names = level_names(&ed);
    assert_eq!(names.iter().filter(|n| n.ends_with(" split")).count(), 3, "{names:?}");
    assert_eq!(count(&ed.model, "roof"), 2, "the roof parts on the line");
    let open: Vec<i64> = ids(&ed.model, "room")
        .into_iter()
        .filter(|r| ed.model.room_quantities(*r).unwrap().is_none())
        .collect();
    assert!(open.is_empty(), "rooms not closed across the half-levels: {open:?}");
}

#[test]
fn a_curtain_facade_exports_plates_and_mullions_with_the_spandrel_row_solid() {
    let mut ed = editor();
    let spec = json!({"typology": "office", "floors": 4, "features": ["curtain_wall"]});
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    apply_and_check(&mut s, &mut ed);
    let walls = ids(&ed.model, "curtain_wall");
    assert!(!walls.is_empty(), "the brief asked for a curtain wall and none was written");
    // the spandrel row is solid, the rows above are glass
    let mut solid = 0;
    for &id in &walls {
        let l = ed.model.curtain_layout(id).unwrap();
        for (c, p) in l.layout.cells.iter().zip(&l.panels) {
            if c.j == 0 {
                assert_eq!(p.kind, ddc_model::PanelKind::Solid, "wall {id} cell {},{}", c.i, c.j);
                solid += 1;
            }
        }
    }
    assert!(solid > 0);
    let classes = ifc_classes(&ed);
    let (plates, members) = (has(&classes, "IFCPLATE"), has(&classes, "IFCMEMBER"));
    assert!(plates >= solid, "{plates} plates for {solid} solid panels: {classes:?}");
    assert!(members > walls.len(), "{members} members (mullions, transoms) for {} walls", walls.len());
}

#[test]
fn a_tower_grows_from_40_to_41_to_64_storeys_with_two_stairs_per_storey() {
    let mut ed = editor();
    let spec = json!({"typology": "office", "floors": 40, "floor_height": 3.6});
    let done = || vec![Script::Text("done".to_string())];
    let provider = ScriptedProvider::new(vec![
        vec![tool("generate_project", json!({"spec": spec}))],
        done(),
        vec![tool("revise_project", json!({"changes": {"floors": 41}}))],
        done(),
        vec![tool("revise_project", json!({"changes": {"floors": 64}}))],
        done(),
    ]);
    let mut s = start(&mut ed, cfg(provider.clone()));
    turn(&mut s, &mut ed, "a tower", Mode::Propose);
    apply_and_check(&mut s, &mut ed);
    for floors in [41, 64] {
        turn(&mut s, &mut ed, "more floors", Mode::Propose);
        let (last, err) = tool_results(&provider).last().cloned().unwrap();
        assert!(!err, "{floors} floors refused: {last}");
        apply_and_check(&mut s, &mut ed);
        let spec = json!({"typology": "office", "floors": floors, "floor_height": 3.6});
        let stairs = count(&ed.model, "stair");
        assert_eq!(stairs, stairs_of(&spec), "{floors} floors: stairs follow the fire rules");
        assert!(stairs >= 2 * (floors - 1), "two stairs per storey: {stairs} at {floors}");
    }
}

#[test]
fn a_lattice_tower_gets_its_200_m_platform_through_run_generator() {
    let mut ed = editor();
    let (mut s, provider) = run(
        &mut ed,
        vec![
            tool("run_generator", json!({"id": "steel_frame", "params": {"kind": "lattice_tower_300"}, "variants": 1})),
            tool("run_generator", json!({"id": "steel_frame", "params": {"kind": "tower_platform", "at": 200}, "variants": 1})),
        ],
    );
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    apply_and_check(&mut s, &mut ed);
    let top: f64 = ed
        .model
        .sql()
        .query_row("SELECT max(max(az, bz)) FROM beam", [], |r| r.get(0))
        .unwrap();
    assert!(top > 195.0, "members reach the 200 m platform: {top}");
}

/// "A 6-storey CLT and glulam office": the brief words alone picks the timber structure, and the model
/// holds glulam beams, CLT floor panels and timber wall assemblies.
#[test]
fn a_clt_office_brief_builds_a_timber_structure() {
    let mut ed = editor();
    let spec = json!({"typology": "office", "floors": 6, "features": ["clt", "glulam"]});
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    apply_and_check(&mut s, &mut ed);
    let names = |q: &str| -> Vec<String> {
        let mut st = ed.model.sql().prepare(q).unwrap();
        st.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
    };
    let members = names("SELECT DISTINCT material FROM member_type");
    assert!(members.iter().any(|m| m.to_lowercase().contains("glulam")), "{members:?}");
    assert!(count(&ed.model, "beam") > 0, "no glulam beams");
    let floors = names("SELECT DISTINCT m.material FROM slab_type_layer m");
    assert!(floors.iter().any(|m| m.to_lowercase().contains("timber") || m.to_lowercase().contains("clt")), "{floors:?}");
    assert!(count(&ed.model, "slab") > 7, "the floors are cut into panels: {}", count(&ed.model, "slab"));
    // timber reads as timber: distinct colours, volumes in the takeoff, names in the IFC
    let color = |n: &str| ed.model.material(n).unwrap().spec.color;
    let clt_name: String = names("SELECT DISTINCT material FROM slab_type_layer")
        .into_iter()
        .find(|m| m.to_lowercase().contains("laminated"))
        .expect("a CLT floor layer");
    let (g, c) = (color("Glulam Timber"), color(&clt_name));
    assert!(g.is_some() && c.is_some() && g != c, "glulam {g:?} CLT {c:?}");
    let beam_vol: f64 = ids(&ed.model, "beam").iter().map(|b| ed.model.beam_quantities(*b).unwrap().volume).sum();
    assert!(beam_vol > 0.0, "glulam beams carry no volume");
    let clt_vol: f64 = ed
        .model
        .sql()
        .prepare("SELECT COALESCE(SUM(volume),0) FROM slab_qty WHERE LOWER(material) LIKE '%laminated%'")
        .unwrap()
        .query_row([], |r| r.get(0))
        .unwrap();
    assert!(clt_vol > 0.0, "CLT floors carry no volume");
    let ifc = ddc_io::ifc::export_ifc(&ed.model).unwrap();
    for name in ["Glulam Timber", clt_name.as_str()] {
        assert!(ifc.contains(&format!("IFCMATERIAL('{name}'")), "{name} missing from the IFC");
    }
    let walls = names("SELECT DISTINCT material FROM wall_type_layer");
    assert!(walls.iter().any(|m| m.to_lowercase().contains("timber") || m.to_lowercase().contains("clt")), "{walls:?}");
}

/// H09 through the tool path: a 10-storey timber residential building. The oracles of
/// `H09_timber_multistorey`: levels, timber elements, frame, stairs, guards, IFC classes and the
/// timber share of walls, slabs and frame; the superstructure holds no concrete outside the core.
#[test]
fn h09_a_ten_storey_timber_building_is_timber_above_ground() {
    let mut ed = editor();
    let spec = json!({"typology": "apartment_section", "floors": 10, "design": {"balconies": "balcony"},
        "features": ["glulam", "clt", "balconies"]});
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    apply_and_check(&mut s, &mut ed);
    let q = |sql: &str| -> i64 { ed.model.sql().query_row(sql, [], |r| r.get(0)).unwrap() };
    let timber = "LOWER(l.material) LIKE '%timber%' OR LOWER(l.material) LIKE '%clt%' OR LOWER(l.material) LIKE '%wood%'";
    let walls = q("SELECT COUNT(*) FROM wall");
    let walls_t = q(&format!("SELECT COUNT(DISTINCT w.id) FROM wall w JOIN wall_type_layer l ON l.wtype=w.wtype WHERE {timber}"));
    let slabs = q("SELECT COUNT(*) FROM slab");
    let slabs_t = q(&format!("SELECT COUNT(DISTINCT s.id) FROM slab s JOIN slab_type_layer l ON l.stype=s.stype WHERE {timber}"));
    let t = "LOWER(t.material) LIKE '%timber%' OR LOWER(t.material) LIKE '%glulam%' OR LOWER(t.material) LIKE '%wood%'";
    let frame = q("SELECT (SELECT COUNT(*) FROM col)+(SELECT COUNT(*) FROM beam)");
    let frame_t = q(&format!(
        "SELECT (SELECT COUNT(*) FROM col e JOIN member_type t ON t.id=e.mtype WHERE {t}) +                 (SELECT COUNT(*) FROM beam e JOIN member_type t ON t.id=e.mtype WHERE {t})"));
    let levels = count(&ed.model, "level");
    assert!((9..=12).contains(&(levels as i64)), "{levels} levels");
    assert!(walls_t + slabs_t + frame_t >= 200, "{walls_t} walls, {slabs_t} slabs, {frame_t} frame members in timber");
    assert!(frame >= 60, "{frame} columns and beams");
    assert!(count(&ed.model, "stair") >= 8);
    assert!(count(&ed.model, "railing") >= 10);
    assert!(walls_t as f64 >= 0.4 * walls as f64, "timber walls {walls_t}/{walls}");
    assert!(slabs_t as f64 >= 0.8 * slabs as f64, "CLT slabs {slabs_t}/{slabs}");
    assert!(frame_t as f64 >= 0.8 * frame as f64, "glulam frame {frame_t}/{frame}");
    // no concrete in the superstructure but the core spine: columns, beams and floors above the ground
    let concrete = "LOWER(l.material) LIKE '%concrete%'";
    let concrete_floors = q(&format!(
        "SELECT COUNT(DISTINCT s.id) FROM slab s JOIN slab_type_layer l ON l.stype=s.stype JOIN level v ON v.id=s.level          WHERE {concrete} AND v.elevation > (SELECT MIN(elevation) FROM level)"));
    assert_eq!(concrete_floors, 0, "concrete slabs above the ground floor");
    let concrete_frame = q("SELECT COUNT(*) FROM (SELECT mtype FROM col UNION ALL SELECT mtype FROM beam) e JOIN member_type t ON t.id=e.mtype WHERE LOWER(t.material) LIKE '%concrete%'");
    assert_eq!(concrete_frame, 0, "concrete columns or beams");
    let classes = ifc_classes(&ed);
    for c in ["IFCBUILDINGSTOREY", "IFCWALL", "IFCSLAB", "IFCCOLUMN", "IFCBEAM"] {
        assert!(has(&classes, c) >= 1, "no {c}: {classes:?}");
    }
}

/// A generated house under a gable roof in a garden: the walls under the roof are trimmed to
/// it, their doors and windows stay under the trimmed tops, and no tree crown (its type's width
/// at its own scale) reaches over the roof or the walls.
#[test]
fn a_gable_house_in_a_garden_is_trimmed_and_its_trees_keep_clear() {
    let mut ed = editor();
    let spec = json!({"typology": "house", "floors": 2, "roof": {"kind": "gable", "slope": 35, "overhang": 0.5},
                      "design": {"trees": 12, "paving": 1.5}});
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    assert!(tool_results(&provider).iter().all(|r| !r.1), "{:?}", tool_results(&provider));
    apply_and_check(&mut s, &mut ed);
    let m = &ed.model;
    let walls: Vec<(i64, (f64, f64), (f64, f64))> = {
        let mut st = m.sql().prepare("SELECT id, ax, ay, bx, by FROM wall ORDER BY id").unwrap();
        st.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?), (r.get(3)?, r.get(4)?)))).unwrap().map(Result::unwrap).collect()
    };
    let trimmed: Vec<i64> = walls.iter().map(|w| w.0).filter(|&w| m.wall_top_roof(w).unwrap().is_some()).collect();
    assert!(trimmed.len() >= 4, "walls under the gable roof: {trimmed:?}");
    for &w in &trimmed {
        let top = m.wall_top_profile(w).unwrap().unwrap_or_else(|| panic!("wall {w} follows the roof but has no top"));
        let at = |u: f64| {
            top.windows(2)
                .find(|p| u >= p[0].0 - 1e-9 && u <= p[1].0 + 1e-9 && p[1].0 - p[0].0 > 1e-9)
                .map_or(top.last().unwrap().1, |p| p[0].1 + (p[1].1 - p[0].1) * (u - p[0].0) / (p[1].0 - p[0].0))
        };
        let mut st = m.sql().prepare("SELECT along, width, sill, height, kind FROM opening WHERE wall=?1").unwrap();
        let ops: Vec<(f64, f64, f64, f64, String)> =
            st.query_map([w], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap().map(Result::unwrap).collect();
        for (along, width, sill, height, kind) in ops {
            let lowest = (0..=10).map(|i| at(along + width * i as f64 / 10.0)).fold(f64::MAX, f64::min);
            assert!(sill + height < lowest + 1e-6, "wall {w}: the {kind} head {} is above the trimmed top {lowest}", sill + height);
        }
    }
    // the roof in plan: its faces
    let faces: Vec<Vec<(f64, f64)>> = ids(m, "roof")
        .into_iter()
        .flat_map(|r| m.roof_geometry(r).unwrap().faces.into_iter().map(|f| f.pts.iter().map(|p| (p.x, p.y)).collect()))
        .collect();
    assert!(!faces.is_empty());
    let seg = |q: (f64, f64), a: (f64, f64), b: (f64, f64)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let t = (((q.0 - a.0) * dx + (q.1 - a.1) * dy) / (dx * dx + dy * dy).max(1e-12)).clamp(0.0, 1.0);
        ((q.0 - a.0 - t * dx).powi(2) + (q.1 - a.1 - t * dy).powi(2)).sqrt()
    };
    let inside = |q: (f64, f64), r: &[(f64, f64)]| {
        let mut c = false;
        for i in 0..r.len() {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            if (a.1 > q.1) != (b.1 > q.1) && q.0 < a.0 + (q.1 - a.1) * (b.0 - a.0) / (b.1 - a.1) {
                c = !c;
            }
        }
        c
    };
    let trees = comps_of(m, &["Tree"]);
    assert!(trees.len() >= 4, "{trees:?}");
    for t in trees {
        let c = m.component(t).unwrap();
        let num = |v: &[(String, ddc_geom::component::Value)], k: &str| {
            v.iter().find(|p| p.0 == k).and_then(|p| if let ddc_geom::component::Value::Num(x) = p.1 { Some(x) } else { None })
        };
        let crown = num(&m.comp_type_values(c.ctype).unwrap(), "Crown").expect("a tree type has a crown");
        let r = crown * num(&c.values, "Scale").unwrap_or(1.0) / 2.0;
        let p = (c.at.0, c.at.1);
        for f in &faces {
            assert!(!inside(p, f), "tree {t} stands under the roof");
            let d = (0..f.len()).map(|i| seg(p, f[i], f[(i + 1) % f.len()])).fold(f64::MAX, f64::min);
            assert!(d > r, "tree {t}: crown {r:.2} m reaches the roof {d:.2} m away");
        }
        for (w, a, b) in &walls {
            let d = seg(p, *a, *b);
            assert!(d > r + 0.2, "tree {t}: crown {r:.2} m reaches wall {w} {d:.2} m away");
        }
    }
}
