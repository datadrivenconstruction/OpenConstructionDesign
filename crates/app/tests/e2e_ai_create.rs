// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Create from parameters: a stub generator stands in for the real ones, and direct creation
//! shows that an element of the plan can be named before it exists.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::fake::ScriptedProvider;
use ddc_ai::Mode;
use ddc_app::editor::Editor;
use ddc_gen::{
    GLayout, GOpening, GRoom, GWall, GenError, GenOut, Generator, ParamDef, ParamKind, WallRole,
};
use e2e_ai_common::*;
use e2e_common::{count, ids};
use serde_json::{json, Value as J};

struct RoomBox;

/// The seed moves the box, so two variants are told apart by their coordinates.
impl Generator for RoomBox {
    fn id(&self) -> &'static str {
        "test.room"
    }
    fn params(&self) -> &'static [ParamDef] {
        &[]
    }
    fn build(&self, params: &J, seed: u64) -> Result<GenOut, GenError> {
        let w = params["width"].as_f64().unwrap_or(6.0);
        let x = seed as f64 * 100.0;
        let corners = [(x, 0.0), (x + w, 0.0), (x + w, 4.0), (x, 4.0)];
        let walls = (0..4)
            .map(|i| GWall::line(corners[i], corners[(i + 1) % 4], WallRole::Exterior))
            .collect();
        Ok(GenOut::Layout(GLayout {
            walls,
            openings: vec![GOpening {
                wall: 0,
                along: 1.0,
                door: true,
                width: 0.9,
                height: 2.1,
                sill: 0.0,
                ty: None,
            }],
            rooms: vec![GRoom {
                name: "Box".to_string(),
                apartment: None,
                point: (x + w / 2.0, 2.0),
                polygon: vec![],
            }],
            slab: corners.to_vec(),
            metrics: vec![("area", w * 4.0)],
            ..GLayout::default()
        }))
    }
}

static ROOM_BOX: RoomBox = RoomBox;

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.model.add_slab_type("S200", &concrete(0.2)).unwrap();
    ed.reload().unwrap();
    ed
}

fn generate(ed: &mut Editor) -> ddc_app::ai::UiSession {
    let provider = ScriptedProvider::new(script(vec![tool(
        "run_generator",
        json!({"id": "test.room", "params": {"width": 6.0}, "variants": 2}),
    )]));
    let mut s = start(ed, with_generators(cfg(provider), vec![&ROOM_BOX]));
    turn(&mut s, ed, "a room 6 m wide", Mode::Propose);
    s
}

#[test]
fn a_generated_layout_is_previewed_and_applied_exactly() {
    let mut ed = editor();
    let mut s = generate(&mut ed);
    let plan = plan_of(&s);
    assert_eq!(plan.variants.len(), 2);
    assert_eq!(
        count(&ed.model, "wall"),
        0,
        "nothing reaches the live model before Apply"
    );
    apply_and_check(&mut s, &mut ed);
    assert_eq!(count(&ed.model, "wall"), 4);
    assert_eq!(count(&ed.model, "opening"), 1);
    assert_eq!(count(&ed.model, "room"), 1);
    assert_eq!(count(&ed.model, "slab"), 1);
}

#[test]
fn another_variant_replaces_the_first_one_on_the_scratch_copy() {
    let mut ed = editor();
    let mut s = generate(&mut ed);
    s.set_variant(1);
    assert!(s.wait_idle(&mut ed, WAIT));
    assert_eq!(plan_of(&s).variant, 1);
    apply_and_check(&mut s, &mut ed);
    assert_eq!(
        count(&ed.model, "wall"),
        4,
        "the first variant must be gone"
    );
    let x: f64 = ed
        .model
        .sql()
        .query_row("SELECT min(ax) FROM wall", [], |r| r.get(0))
        .unwrap();
    assert!((x - 200.0).abs() < 1e-6, "{x}");
}

#[test]
fn another_variant_unticks_what_was_built_on_the_first_one() {
    let mut ed = editor();
    let provider = ScriptedProvider::new(script(vec![
        tool(
            "run_generator",
            json!({"id": "test.room", "params": {"width": 6.0}, "variants": 2}),
        ),
        tool(
            "create_elements",
            json!({"title": "Door", "cat": "door", "items": [{"wall": "wall:n2", "along": 1.0}]}),
        ),
    ]));
    let mut s = start(
        &mut ed,
        with_generators(cfg(provider.clone()), vec![&ROOM_BOX]),
    );
    turn(&mut s, &mut ed, "a room and a door", Mode::Propose);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    assert_eq!(plan_of(&s).items[1].depends_on, vec![1]);

    s.set_variant(1);
    assert!(s.wait_idle(&mut ed, WAIT));
    let plan = plan_of(&s);
    assert!(
        !plan.items[1].checked,
        "the door was placed on the old layout"
    );
    assert!(
        plan.items[1].conflict.is_some(),
        "the reason must stay on the card"
    );
    apply_and_check(&mut s, &mut ed);
    assert_eq!(
        count(&ed.model, "opening"),
        1,
        "only the generator's own door"
    );
}

#[test]
fn choosing_the_same_variant_changes_nothing() {
    let mut ed = editor();
    let mut s = generate(&mut ed);
    s.set_variant(0);
    assert!(s.wait_idle(&mut ed, WAIT));
    assert!(plan_of(&s).items[0].checked);
    assert_eq!(plan_of(&s).variant, 0);
}

#[test]
fn a_door_can_name_a_wall_of_the_same_plan() {
    let mut ed = editor();
    let (mut s, provider) = run(
        &mut ed,
        vec![
            tool(
                "create_elements",
                json!({"title": "Wall", "cat": "wall", "items": [{"a": [0, 0], "b": [5, 0]}]}),
            ),
            tool(
                "create_elements",
                json!({"title": "Door", "cat": "door", "items": [{"wall": "wall:n1", "along": 1.0}]}),
            ),
        ],
    );
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    assert_eq!(parsed(&results[0].0)["created"], json!(["wall:n1"]));
    assert_eq!(plan_of(&s).items[1].depends_on, vec![1]);
    apply_and_check(&mut s, &mut ed);
    let wall = ids(&ed.model, "wall")[0];
    let host: i64 = ed
        .model
        .sql()
        .query_row("SELECT wall FROM opening", [], |r| r.get(0))
        .unwrap();
    assert_eq!(host, wall);
}

#[test]
fn a_new_type_can_be_used_by_the_next_item() {
    let mut ed = editor();
    let (mut s, provider) = run(
        &mut ed,
        vec![
            tool(
                "create_type",
                json!({"title": "Type", "cat": "wall", "name": "W300", "fields": {"layers": [{"material": "Concrete", "thickness": 0.3}]}}),
            ),
            tool(
                "create_elements",
                json!({"title": "Wall", "cat": "wall", "items": [{"a": [0, 0], "b": [5, 0], "type": "wall_type:n1"}]}),
            ),
        ],
    );
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    apply_and_check(&mut s, &mut ed);
    let name: String = ed
        .model
        .sql()
        .query_row(
            "SELECT t.name FROM wall w JOIN wall_type t ON t.id=w.wtype",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(name, "W300");
}

#[test]
fn a_negative_size_is_refused_before_it_reaches_the_plan() {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let wall = add_wall(&mut ed, level, (0.0, 0.0), (5.0, 0.0));
    let (s, provider) = run(
        &mut ed,
        vec![tool(
            "create_elements",
            json!({"title": "Door", "cat": "door", "items": [{"wall": format!("wall:{wall}"), "along": 1.0, "width": -1.0}]}),
        )],
    );
    let results = tool_results(&provider);
    assert!(results[0].1, "{results:?}");
    assert!(results[0].0.contains("width"), "{}", results[0].0);
    assert!(s.plan().is_none());
}

#[test]
fn a_failed_creation_leaves_no_item_and_says_why() {
    let mut ed = editor();
    let (s, provider) = run(
        &mut ed,
        vec![tool(
            "create_elements",
            json!({"title": "Door", "cat": "door", "items": [{"wall": "wall:999", "along": 1.0}]}),
        )],
    );
    let results = tool_results(&provider);
    assert!(results[0].1, "{results:?}");
    assert!(s.plan().is_none());
}

/// Bounds of every declared number, one at a time, on top of workable defaults.
fn parameter_sets(g: &dyn Generator, base: &J) -> Vec<J> {
    let mut out = vec![base.clone()];
    for d in g.params() {
        match d.kind {
            ParamKind::Length | ParamKind::Area | ParamKind::Ratio => {
                for x in [d.min, d.max] {
                    let mut p = base.clone();
                    p[d.name] = json!(x);
                    out.push(p);
                }
            }
            ParamKind::Count => {
                for x in [d.min, d.max] {
                    let mut p = base.clone();
                    p[d.name] = json!(x.round() as i64);
                    out.push(p);
                }
            }
            ParamKind::Choice(list) => {
                for c in list {
                    let mut p = base.clone();
                    p[d.name] = json!(c);
                    out.push(p);
                }
            }
            _ => {}
        }
    }
    out
}

fn base_params(id: &str) -> J {
    match id {
        "typical_floor" => json!({"depth": 18.0}),
        "area_program" => json!({"rooms": [
            {"name": "Hall", "area": 40.0},
            {"name": "Office", "area": 18.5},
            {"name": "Store", "area": 6.0, "window": false},
        ]}),
        "family" => json!({"height": 2.1}),
        _ => json!({}),
    }
}

fn assert_sound(m: &ddc_model::Model, what: &str) {
    let issues = m.check_integrity().unwrap();
    assert!(issues.is_empty(), "{what}: {issues:?}");
    let warnings: Vec<String> = m
        .wall_warnings()
        .unwrap()
        .into_iter()
        .map(|w| w.message)
        .collect();
    assert!(warnings.is_empty(), "{what}: {warnings:?}");
    let open: i64 = m
        .sql()
        .query_row(
            "SELECT count(*) FROM room r LEFT JOIN room_qty q ON q.room=r.id WHERE q.room IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(open, 0, "{what}: rooms not enclosed");
}

/// Refusing bounds it cannot satisfy is allowed; a plan it does offer must be sound and exact.
#[test]
fn every_generator_gives_a_sound_model_across_its_parameters() {
    let mut applied = 0;
    for g in ddc_gen::registry() {
        for params in parameter_sets(*g, &base_params(g.id())) {
            let what = format!("{} {params}", g.id());
            let mut ed = editor();
            let provider = ScriptedProvider::new(script(vec![tool(
                "run_generator",
                json!({"id": g.id(), "params": params, "variants": 3}),
            )]));
            let mut s = start(&mut ed, cfg(provider.clone()));
            turn(&mut s, &mut ed, "make it", Mode::Propose);
            if tool_results(&provider)[0].1 {
                assert!(s.plan().is_none(), "{what}");
                continue;
            }
            let variants = plan_of(&s).variants.len();
            assert!((1..=3).contains(&variants), "{what}");
            let work =
                ddc_model::Model::open_copy(&s.work_path().unwrap().to_string_lossy()).unwrap();
            assert_sound(&work, &what);
            drop(work);
            apply_and_check(&mut s, &mut ed);
            assert_sound(&ed.model, &what);
            applied += 1;
        }
    }
    assert!(
        applied >= ddc_gen::registry().len() * 3,
        "only {applied} plans were offered"
    );
}

#[test]
fn the_same_parameters_give_the_same_model() {
    for g in ddc_gen::registry() {
        let hash = || {
            let mut ed = editor();
            let provider = ScriptedProvider::new(script(vec![tool(
                "run_generator",
                json!({"id": g.id(), "params": base_params(g.id())}),
            )]));
            let mut s = start(&mut ed, cfg(provider));
            turn(&mut s, &mut ed, "make it", Mode::Propose);
            s.apply(&mut ed).unwrap();
            live_hash(&ed.model)
        };
        assert_eq!(hash(), hash(), "{}", g.id());
    }
}

#[test]
fn a_spiral_stair_that_climbs_too_little_per_turn_is_refused() {
    let mut ed = editor();
    ed.model.add_level("L1", 0.0).unwrap();
    ed.model.add_level("L2", 3.0).unwrap();
    ed.reload().unwrap();
    let provider = ScriptedProvider::new(script(vec![tool(
        "create_elements",
        json!({
            "title": "Spiral",
            "cat": "stair",
            "items": [{
                "origin": [8.0, 0.0],
                "top_level": "L2",
                "angle": 0.0,
                "shape": "spiral",
                "radius": 1.1,
                "core": 0.3,
                "turn": 9.5,
                "width": 0.8,
                "max_riser": 0.15,
                "landing": 1.2
            }]
        }),
    )]));
    let mut s = start(&mut ed, cfg(provider.clone()));
    turn(&mut s, &mut ed, "a spiral stair", Mode::Propose);
    let results = tool_results(&provider);
    assert!(results.iter().any(|r| r.1), "{results:?}");
    assert_eq!(count(&ed.model, "stair"), 0);
}
