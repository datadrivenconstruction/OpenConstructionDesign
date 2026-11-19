// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::ai::plan::{replay, FindingRef};
use crate::ai::sets::Sets;
use crate::editor::Editor;
use ddc_model::checks::Fix;

fn item(id: u32, op: PlanOp, depends_on: Vec<u32>) -> PlanItem {
    PlanItem {
        id,
        title: format!("item {id}"),
        why: String::new(),
        op,
        checked: true,
        depends_on,
        actor: "chat".into(),
        conflict: None,
        sets: Default::default(),
    }
}

fn r(cat: &str, id: i64) -> ElRef {
    ElRef {
        cat: cat.into(),
        id: IdOrNew::Id(id),
    }
}

fn ai() -> Actor {
    Actor::Ai {
        session: "s1".into(),
        model: "m".into(),
    }
}

/// Every `NewElem` variant, written with the words `lift` gives back.
fn every_elem() -> Vec<NewElem> {
    vec![
        NewElem::Wall {
            level: 1,
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            bulge: 0.25,
            base_offset: 0.1,
            height: 3.0,
            wtype: Some(r("wall_type", 1)),
        },
        NewElem::Wall {
            level: 1,
            a: (0.0, 2.0),
            b: (6.0, 2.0),
            base_offset: 0.0,
            height: 3.0,
            wtype: Some(r("wall_type", 1)),
            bulge: 0.25,
        },
        NewElem::Opening {
            wall: r("wall", 7),
            along: 2.0,
            width: 0.9,
            height: 2.1,
            sill: 0.0,
            kind: "door".into(),
        },
        NewElem::Opening {
            wall: r("wall", 7),
            along: 4.0,
            width: 1.2,
            height: 1.4,
            sill: 0.9,
            kind: "window".into(),
        },
        NewElem::Opening {
            wall: r("wall", 7),
            along: 5.0,
            width: 0.5,
            height: 0.5,
            sill: 1.0,
            kind: "void".into(),
        },
        NewElem::Slab {
            level: 1,
            stype: Some(r("slab_type", 2)),
            offset: -0.2,
            outer: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0)],
            holes: vec![vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.0)]],
        },
        NewElem::Room {
            level: 1,
            name: "Hall".into(),
            number: "101".into(),
            point: (1.0, 1.0),
            height: 2.7,
        },
        NewElem::Column {
            level: 1,
            top_level: Some(2),
            mtype: Some(r("member_type", 3)),
            at: (3.0, 3.0),
            base_offset: 0.0,
            top_offset: -0.3,
            rotation: 0.25,
            structural: true,
            top_at: Some((4.0, 3.5)),
        },
        NewElem::Beam {
            level: 1,
            mtype: None,
            a: (0.0, 0.0, 3.0),
            b: (6.0, 0.0, 3.0),
        },
        NewElem::Comp {
            level: 1,
            ctype: r("comp_type", 4),
            at: (1.0, 2.0, 0.0),
            rotation: 1.5,
            host: Some(r("wall", 7)),
            scale: 1.15,
            min_clear: 1.8,
        },
        NewElem::Ceiling {
            level: 1,
            stype: None,
            height: 2.6,
            outer: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0)],
            holes: vec![],
        },
        NewElem::Roof {
            level: 2,
            stype: Some(r("slab_type", 2)),
            offset: 0.0,
            overhang: 0.5,
            ring: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
            slope: Some(30.0),
            edges: vec![Some(30.0), None, Some(30.0), None],
            vault: None,
        },
        NewElem::Roof {
            level: 2,
            stype: Some(r("slab_type", 2)),
            offset: 0.0,
            overhang: 0.0,
            ring: vec![(0.0, 0.0), (8.0, 0.0), (8.0, 10.0), (0.0, 10.0)],
            slope: Some(30.0),
            edges: vec![],
            vault: Some(crate::ai::plan::RoofVault {
                a: (0.0, 0.0),
                b: (8.0, 0.0),
                profile: vec![(0.0, 0.0), (1.0, 2.0), (4.0, 3.0), (7.0, 2.0), (8.0, 0.0)],
                depth: 10.0,
            }),
        },
        NewElem::Stair {
            level: 1,
            top_level: 2,
            origin: (2.0, 2.0),
            angle: 0.5,
        },
        NewElem::Pipe {
            system: r("mep_system", 2),
            level: 1,
            material: "steel".into(),
            dn: 50,
            points: vec![(0.0, 0.0, 2.5), (3.0, 0.0, 2.5)],
            section: None,
        },
        NewElem::Curtain {
            level: 1,
            name: "CW".into(),
            a: (0.0, 5.0),
            b: (6.0, 5.0),
            base_offset: 0.0,
            height: 3.0,
            spacing: (1.5, 1.0),
            mullion: (0.05, 0.1),
            spandrel: 0.0,
        },
        NewElem::GenStair {
            level: 1,
            top_level: 2,
            top_offset: 0.0,
            origin: (8.0, 0.0),
            angle: 0.0,
            shape: StairShape::U {
                left: true,
                first: Some(8),
                gap: 0.1,
            },
            width: 1.2,
            max_riser: 0.18,
            landing: 1.3,
        },
        NewElem::Shaft {
            level: 1,
            top_level: 2,
            top_offset: 0.0,
            ring: vec![(9.0, 9.0), (10.0, 9.0), (10.0, 10.0)],
        },
        NewElem::Grid {
            name: "A".into(),
            a: (0.0, -1.0),
            b: (0.0, 10.0),
        },
    ]
}

fn every_op() -> Vec<PlanOp> {
    vec![
        PlanOp::Assign {
            target: Targets::Refs(vec![r("wall", 7)]),
            set: vec![
                Assign {
                    key: "mark".into(),
                    val: Val::Lit("A-1".into()),
                },
                Assign {
                    key: "height".into(),
                    val: Val::Rel('+', "100".into()),
                },
            ],
        },
        PlanOp::ReplaceType {
            target: Targets::Refs(vec![r("wall", 7)]),
            to: r("wall_type", 1),
        },
        PlanOp::Create {
            slot0: 1,
            items: every_elem(),
        },
        PlanOp::Delete {
            target: Targets::Filter("wall".into()),
        },
        PlanOp::NewType(NewType {
            slot: 20,
            cat: "wall".into(),
            name: "W200".into(),
            layers: vec![LayerSpec {
                material: "Concrete".into(),
                thickness: 0.2,
            }],
            shape: None,
            material: String::new(),
            family: None,
            values: vec![],
        }),
        PlanOp::NewParam(ParamSpec {
            name: "Fire rating".into(),
            ptype: ParamType::Text,
            per_type: false,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: vec!["wall".into(), "door".into()],
        }),
        PlanOp::LoadFamily {
            def: "family text".into(),
        },
        PlanOp::Generate {
            id: "g".into(),
            params: json!({}),
            seed: 1,
            level: 1,
            slot0: 30,
        },
        PlanOp::Fix(FindingRef {
            id: "f1".into(),
            rule: "r".into(),
            key: "k".into(),
            elements: vec![],
            fix: Fix::None,
        }),
    ]
}

fn state_of(ops: Vec<PlanOp>) -> PlanState {
    let mut st = PlanState::default();
    for (i, op) in ops.into_iter().enumerate() {
        st.items.push(item(i as u32 + 1, op, vec![]));
    }
    st.units.insert(1, Units::Metric);
    st
}

#[test]
fn every_plan_op_variant_lowers_or_is_refused_with_reason() {
    let mut lowered = vec![];
    let mut refused = vec![];
    for op in every_op() {
        match lower_op(&op, Some(Units::Metric)) {
            Ok(ops) => {
                assert!(!ops.is_empty(), "{} lowered to nothing", op_kind(&op));
                lowered.push(op_kind(&op));
            }
            Err(why) => {
                assert!(!why.trim().is_empty(), "{} refused without a reason", op_kind(&op));
                refused.push(op_kind(&op));
            }
        }
    }
    assert_eq!(
        lowered,
        ["assign", "replace_type", "create", "delete", "new_type", "new_param"]
    );
    assert_eq!(refused, ["load_family", "generate", "fix"]);

    // the plan keeps going past a refused item; one that depends on it is refused too
    let mut st = state_of(every_op());
    st.items.push(item(
        10,
        PlanOp::Delete {
            target: Targets::Filter("door".into()),
        },
        vec![7],
    ));
    let l = lower_plan(&st, ai(), "plan");
    assert_eq!(l.envelope.groups.len(), 6);
    let ids: Vec<(u32, &str)> = l.refused.iter().map(|x| (x.item, x.kind)).collect();
    assert_eq!(
        ids,
        [(7, "load_family"), (8, "generate"), (9, "fix"), (10, "delete")]
    );
    assert!(l.refused.iter().all(|x| !x.reason.is_empty()));
}

#[test]
fn a_frozen_selection_and_a_shared_parameter_are_refused() {
    let sel = PlanOp::Delete {
        target: Targets::Selection,
    };
    assert!(lower_op(&sel, None).is_err());
    let shared = PlanOp::NewParam(ParamSpec {
        name: "Code".into(),
        ptype: ParamType::Text,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: Some("0f0f".into()),
        categories: vec!["wall".into()],
    });
    assert!(lower_op(&shared, None).is_err());
}

#[test]
fn lowering_is_deterministic() {
    let st = state_of(every_op());
    let a = lower_plan(&st, ai(), "plan");
    let b = lower_plan(&st, ai(), "plan");
    assert_eq!(a, b);
    assert_eq!(
        a.envelope.to_json().unwrap(),
        b.envelope.to_json().unwrap()
    );
}

#[test]
fn a_lowered_plan_round_trips_through_serde() {
    let st = state_of(every_op());
    let env = lower_plan(&st, ai(), "plan").envelope;
    let back = Envelope::from_json(&env.to_json().unwrap()).unwrap();
    assert_eq!(back, env);

    let lifted = lift(&back).unwrap();
    let kept: Vec<&PlanItem> = st
        .items
        .iter()
        .filter(|i| lower_op(&i.op, None).is_ok())
        .collect();
    assert_eq!(lifted.len(), kept.len());
    for (orig, l) in kept.iter().zip(&lifted) {
        assert_eq!(l.item.id, orig.id);
        assert_eq!(format!("{:?}", l.item.op), format!("{:?}", orig.op));
    }
    assert_eq!(lifted[0].units, Some(Units::Metric));

    // and lowering the lifted plan gives the same envelope
    let mut again = PlanState::default();
    for l in lifted {
        if let Some(u) = l.units {
            again.units.insert(l.item.id, u);
        }
        again.items.push(l.item);
    }
    assert_eq!(lower_plan(&again, ai(), "plan").envelope, env);
}

// ---- through the plan path on a model ----

fn dump(ed: &Editor) -> Vec<String> {
    let db = ed.model.sql();
    let mut q = db
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='table' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap();
    let tables: Vec<String> = q
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut out = vec![];
    for t in tables {
        // the journal and audit rows carry times
        if matches!(t.as_str(), "op" | "meta" | "ai_audit" | "ai_chat") {
            continue;
        }
        let mut s = db.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
        let n = s.column_count();
        let mut rows: Vec<String> = s
            .query_map([], |row| {
                let mut line = t.clone();
                for i in 0..n {
                    let v: rusqlite::types::Value = row.get(i)?;
                    line.push_str(&format!("|{v:?}"));
                }
                Ok(line)
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows.sort();
        out.extend(rows);
    }
    out
}

fn model_plan() -> PlanState {
    let lvl = Editor::new_in_memory().unwrap().level;
    let wall = |a: (f64, f64), b: (f64, f64)| NewElem::Wall {
        level: lvl,
        a,
        b,
        bulge: 0.0,
        base_offset: 0.0,
        height: 3.0,
        wtype: None,
    };
    let lit = |k: &str, v: &str| Assign {
        key: k.into(),
        val: Val::Lit(v.into()),
    };
    let mut st = PlanState::default();
    st.items = vec![
        item(
            1,
            PlanOp::Create {
                slot0: 1,
                items: vec![wall((0.0, 0.0), (6.0, 0.0)), wall((6.0, 0.0), (6.0, 4.0))],
            },
            vec![],
        ),
        item(
            2,
            PlanOp::Create {
                slot0: 3,
                items: vec![
                    NewElem::Opening {
                        wall: ElRef::new("wall", 1),
                        along: 2.0,
                        width: 0.9,
                        height: 2.1,
                        sill: 0.0,
                        kind: "door".into(),
                    },
                    NewElem::Opening {
                        wall: ElRef::new("wall", 2),
                        along: 1.5,
                        width: 1.2,
                        height: 1.4,
                        sill: 0.9,
                        kind: "window".into(),
                    },
                ],
            },
            vec![1],
        ),
        item(
            3,
            PlanOp::NewType(NewType {
                slot: 5,
                cat: "wall".into(),
                name: "Lowered 200".into(),
                layers: vec![LayerSpec {
                    material: "Concrete".into(),
                    thickness: 0.2,
                }],
                shape: None,
                material: String::new(),
                family: None,
                values: vec![],
            }),
            vec![],
        ),
        item(
            4,
            PlanOp::ReplaceType {
                target: Targets::Refs(vec![ElRef::new("wall", 2)]),
                to: ElRef::new("wall_type", 5),
            },
            vec![1, 3],
        ),
        item(
            5,
            PlanOp::Assign {
                target: Targets::Refs(vec![ElRef::new("wall", 1)]),
                // walls take type, height and offset as fields; a mark is not one of them
                set: vec![lit("offset", "100"), lit("height", "3500")],
            },
            vec![1],
        ),
        item(
            6,
            PlanOp::NewParam(ParamSpec {
                name: "Fire rating".into(),
                ptype: ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            }),
            vec![],
        ),
        item(
            7,
            PlanOp::Assign {
                target: Targets::Refs(vec![ElRef::new("wall", 1)]),
                set: vec![lit("Fire rating", "EI60")],
            },
            vec![1, 6],
        ),
        item(
            8,
            PlanOp::Delete {
                target: Targets::Refs(vec![ElRef::new("window", 4)]),
            },
            vec![2],
        ),
    ];
    st.next_item = 8;
    st.next_slot = 5;
    st.units.insert(5, Units::Metric);
    st
}

/// The plan read back from its envelope.
fn relift(st: &PlanState) -> (Envelope, PlanState) {
    let l = lower_plan(st, ai(), "plan");
    assert!(l.refused.is_empty(), "{:?}", l.refused);
    let mut out = PlanState::default();
    for x in lift(&l.envelope).unwrap() {
        if let Some(u) = x.units {
            out.units.insert(x.item.id, u);
        }
        out.items.push(x.item);
    }
    out.next_item = st.next_item;
    out.next_slot = st.next_slot;
    (l.envelope, out)
}

type Run = (Vec<String>, Vec<(u32, i64)>, Vec<(u32, Option<String>)>);

/// Replays a plan on a fresh copy, as a sync does.
fn run(st: &mut PlanState) -> Run {
    let mut ed = Editor::new_in_memory().unwrap();
    replay(st, &mut ed, &Sets::default(), &[], &[]).unwrap();
    let mut slots: Vec<(u32, i64)> = st.slots.iter().map(|(k, v)| (*k, *v)).collect();
    slots.sort_unstable();
    let conflicts = st
        .items
        .iter()
        .map(|i| (i.id, i.conflict.clone()))
        .collect();
    (dump(&ed), slots, conflicts)
}

#[test]
fn plan_diff_identical_after_lowering() {
    let mut plan = model_plan();
    let (_, mut lowered) = relift(&plan);
    let (d1, s1, c1) = run(&mut plan);
    let (d2, s2, c2) = run(&mut lowered);
    for id in [1, 2, 5] {
        assert_eq!(c1[id as usize - 1], (id, None), "item {id} must compile");
    }
    assert_ne!(d1, dump(&Editor::new_in_memory().unwrap()), "the plan changed nothing");
    assert_eq!(c2, c1);
    assert_eq!(s2, s1);
    assert_eq!(d2, d1);
}

#[test]
fn replay_after_sync_keeps_slots() {
    let plan = model_plan();
    let (env, mut lowered) = relift(&plan);

    // slots stay slots: creators carry them, later groups name them as `new:<slot>`
    let made: Vec<u32> = env.ops().filter_map(Op::slot).collect();
    assert_eq!(made, [1, 2, 3, 4, 5]);
    assert_eq!(
        env.groups[3].ops,
        [Op::SetType {
            target: OpTargets::Refs {
                refs: vec![Ref::New { slot: 2 }]
            },
            to: Ref::New { slot: 5 },
        }]
    );
    match &lowered.items[1].op {
        PlanOp::Create { slot0, .. } => assert_eq!(*slot0, 3),
        other => panic!("{other:?}"),
    }

    let (_, first, _) = run(&mut lowered);
    let (_, again, conflicts) = run(&mut lowered);
    assert_eq!(first.iter().map(|s| s.0).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
    assert_eq!(again, first);
    assert!(conflicts.iter().take(2).all(|c| c.1.is_none()), "{conflicts:?}");
    let (_, orig, _) = run(&mut model_plan());
    assert_eq!(first, orig);
}

#[test]
fn lifted_item_has_empty_why() {
    let st = model_plan();
    let (env, back) = relift(&st);
    assert!(!back.items.is_empty());
    assert!(back.items.iter().all(|i| i.why.is_empty()), "no rationale in, none out");
    assert!(!env.to_json().unwrap().contains("\"why\":\"\""), "empty group why is not serialized");
}

#[test]
fn lower_lift_keeps_why() {
    let mut st = model_plan();
    st.items[0].why = "walls first: later items hang off them".into();
    let (env, back) = relift(&st);
    assert_eq!(env.groups[0].why, st.items[0].why);
    assert_eq!(back.items[0].why, st.items[0].why);
    assert!(back.items[1..].iter().all(|i| i.why.is_empty()));
}

#[test]
fn envelope_without_why_parses() {
    let st = model_plan();
    let (env, _) = relift(&st);
    let text = env.to_json().unwrap();
    assert!(!text.contains("\"why\":\"\""));
    let parsed = Envelope::from_json(&text).unwrap();
    assert!(parsed.groups.iter().all(|g| g.why.is_empty()));
    assert_eq!(parsed, env);
}

// ---- through the envelope executor (`Editor::run_ops`) ----

/// An editor on the model executor with one 6 m wall and a member type; returns the wall.
fn exec_editor() -> (Editor, i64) {
    crate::editor::ops_run::set_thread_exec(Some(std::sync::Arc::new(crate::ops_exec::ModelExec)));
    let mut ed = Editor::new_in_memory().unwrap();
    let spec = ddc_model::WallSpec {
        a: (0.0, 0.0),
        b: (6.0, 0.0),
        level: ed.level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: ed.types[0].id,
        offset0: None,
    };
    ed.model.add_walls(&[spec]).unwrap();
    ed.model
        .add_member_type("lower-C300", &ddc_model::MemberShape::Rect { b: 0.3, h: 0.3 }, "Concrete")
        .unwrap();
    ed.reload().unwrap();
    let wall: i64 = ed
        .model
        .sql()
        .query_row("SELECT max(id) FROM wall", [], |r| r.get(0))
        .unwrap();
    (ed, wall)
}

/// Model content, journal length and undo depth.
fn exec_state(ed: &Editor) -> (u64, usize, usize) {
    (
        ed.model.content_hash().unwrap(),
        ed.model.journal_len().unwrap(),
        ed.undo_len(),
    )
}

fn create_plan(items: Vec<NewElem>) -> PlanState {
    let mut st = PlanState::default();
    st.items = vec![item(1, PlanOp::Create { slot0: 1, items }, vec![])];
    st.next_item = 1;
    st
}

fn lowered_env(st: &PlanState) -> Envelope {
    let l = lower_plan(st, ai(), "plan");
    assert!(l.refused.is_empty(), "{:?}", l.refused);
    l.envelope
}

#[test]
fn an_ai_door_lands_where_the_plan_puts_it() {
    let (mut ed, wall) = exec_editor();
    let st = create_plan(vec![NewElem::Opening {
        wall: r("wall", wall),
        along: 2.0,
        width: 0.9,
        height: 2.1,
        sill: 0.0,
        kind: "door".into(),
    }]);
    let env = lowered_env(&st);
    // the envelope names the centre; the plan named the start
    match &env.groups[0].ops[0] {
        Op::Create {
            el: NewEl::Door {
                at: Place::OnHost { along, .. },
                ..
            },
            ..
        } => assert!((along.to_m() - 2.45).abs() < 1e-9, "{along:?}"),
        other => panic!("{other:?}"),
    }
    ed.run_ops(env).unwrap();
    let (along, width): (f64, f64) = ed
        .model
        .sql()
        .query_row("SELECT along,width FROM opening WHERE wall=?1", [wall], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert!((along - 2.0).abs() < 1e-9 && (width - 0.9).abs() < 1e-9, "{along} {width}");
    crate::editor::ops_run::set_thread_exec(None);
}

#[test]
fn an_ai_room_and_column_are_made_in_one_step_with_the_room_height() {
    let (mut ed, _) = exec_editor();
    let lvl = ed.level;
    let st = create_plan(vec![
        NewElem::Room {
            level: lvl,
            name: "Hall".into(),
            number: "101".into(),
            point: (1.0, 1.0),
            height: 2.7,
        },
        NewElem::Column {
            level: lvl,
            top_level: None,
            mtype: None,
            at: (3.0, 2.0),
            base_offset: 0.0,
            top_offset: 2.8,
            rotation: 0.0,
            structural: true,
            top_at: None,
        },
    ]);
    let env = lowered_env(&st);
    assert!(
        env.ops().all(|o| !matches!(o, Op::Create { el: NewEl::Generic { .. }, .. })),
        "room and column go typed, the executor refuses generic"
    );
    let s0 = exec_state(&ed);
    let rc = ed.run_ops(env).unwrap();
    assert_eq!(rc.created.len(), 2);
    let (name, number, h): (String, String, f64) = ed
        .model
        .sql()
        .query_row("SELECT name,number,height FROM room", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!((name.as_str(), number.as_str()), ("Hall", "101"));
    assert!((h - 2.7).abs() < 1e-9, "room height {h}, not the 3.0 default");
    let made: Vec<(Cat, i64)> = rc
        .created
        .iter()
        .map(|(_, x)| match x {
            Ref::Id { cat, id } => (*cat, *id),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(made.iter().map(|m| m.0).collect::<Vec<_>>(), [Cat::Room, Cat::Column]);
    let col = ed.model.column(made[1].1).unwrap();
    assert!((col.top_offset - 2.8).abs() < 1e-9 && col.at == (3.0, 2.0), "{col:?}");
    let s1 = exec_state(&ed);
    assert_eq!((s1.1, s1.2), (s0.1 + 1, s0.2 + 1), "one journal entry, one undo step");
    ed.undo();
    assert_eq!(ed.model.content_hash().unwrap(), s0.0, "undo removes both");
    crate::editor::ops_run::set_thread_exec(None);
}

#[test]
fn an_ai_slab_with_a_bad_outline_changes_nothing() {
    let (mut ed, _) = exec_editor();
    let lvl = ed.level;
    let st = create_plan(vec![
        NewElem::Room {
            level: lvl,
            name: "Hall".into(),
            number: String::new(),
            point: (1.0, 1.0),
            height: 2.7,
        },
        NewElem::Slab {
            level: lvl,
            stype: None,
            offset: 0.0,
            outer: vec![(0.0, 0.0), (6.0, 0.0)],
            holes: vec![],
        },
    ]);
    let env = lowered_env(&st);
    let s0 = exec_state(&ed);
    assert!(ed.run_ops(env).is_err());
    assert_eq!(exec_state(&ed), s0, "all or nothing: the room is not kept either");
    crate::editor::ops_run::set_thread_exec(None);
}

fn add_text_param(ed: &mut Editor, name: &str, per_type: bool) {
    ed.model
        .add_param(&ParamSpec {
            name: name.into(),
            ptype: ParamType::Text,
            per_type,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: vec!["wall".into()],
        })
        .unwrap();
}

fn text_param(ed: &Editor, name: &str, kind: &str, id: i64) -> Option<ddc_model::PVal> {
    let d = ed.model.param_by_name(name).unwrap().unwrap();
    ed.model.param_value_of(&d, kind, id).unwrap()
}

fn assign_plan(target: ElRef, set: &[(&str, &str)]) -> PlanState {
    let mut st = PlanState::default();
    let set = set
        .iter()
        .map(|(k, v)| Assign {
            key: (*k).into(),
            val: Val::Lit((*v).into()),
        })
        .collect();
    st.items = vec![item(1, PlanOp::Assign { target: Targets::Refs(vec![target]), set }, vec![])];
    st.next_item = 1;
    st
}

/// An AI assign with an instance and a type parameter on a wall: the instance value lands on the
/// wall, the type value on its type, one undo step for both.
#[test]
fn an_ai_assign_with_type_and_instance_params_lands_in_one_step() {
    let (mut ed, wall) = exec_editor();
    add_text_param(&mut ed, "Fire rating", false);
    add_text_param(&mut ed, "Acoustic class", true);
    let wt: i64 = ed
        .model
        .sql()
        .query_row("SELECT wtype FROM wall WHERE id=?1", [wall], |r| r.get(0))
        .unwrap();
    let st = assign_plan(r("wall", wall), &[("Fire rating", "EI60"), ("Acoustic class", "B")]);
    let s0 = exec_state(&ed);
    ed.run_ops(lowered_env(&st)).unwrap();
    assert_eq!(text_param(&ed, "Fire rating", "wall", wall), Some(ddc_model::PVal::Text("EI60".into())));
    assert_eq!(text_param(&ed, "Acoustic class", "wall_type", wt), Some(ddc_model::PVal::Text("B".into())));
    let s1 = exec_state(&ed);
    assert_eq!((s1.1, s1.2), (s0.1 + 1, s0.2 + 1), "one journal entry, one undo step");
    ed.undo();
    assert_eq!(ed.model.content_hash().unwrap(), s0.0, "undo takes both values back");
    crate::editor::ops_run::set_thread_exec(None);
}

/// An instance parameter on a type row would mean every wall of the type: refused, nothing kept.
#[test]
fn an_ai_instance_param_on_a_type_row_changes_nothing() {
    let (mut ed, _) = exec_editor();
    add_text_param(&mut ed, "Fire rating", false);
    add_text_param(&mut ed, "Acoustic class", true);
    let wt = ed.types[0].id;
    let st = assign_plan(r("wall_type", wt), &[("Acoustic class", "B"), ("Fire rating", "EI60")]);
    let s0 = exec_state(&ed);
    let e = ed.run_ops(lowered_env(&st)).unwrap_err();
    assert!(e.to_string().contains("instance parameter"), "{e}");
    assert_eq!(exec_state(&ed), s0, "no value and no undo step");
    assert_eq!(text_param(&ed, "Acoustic class", "wall_type", wt), None);
    crate::editor::ops_run::set_thread_exec(None);
}

#[test]
fn spiral_stairs_are_written_with_their_radius_core_and_turn() {
    let shape = StairShape::Spiral { radius: 1.2, core: 0.3, turn: 4.5 };
    let (j, _) = shape_json(&shape);
    assert_eq!(j["kind"], "spiral");
    assert_eq!(j["radius"], 1.2);
    assert_eq!(j["core"], 0.3);
    assert_eq!(j["turn"], 4.5);
}

/// G28: a V-column from `create_elements` keeps its top point through the plan replay (Apply)
/// and through the lowered envelope; before, the plan column had no top point to keep.
#[test]
fn a_slanted_column_keeps_its_top_point() {
    let col = |level: i64, top: (f64, f64)| NewElem::Column {
        level,
        top_level: None,
        mtype: None,
        at: (45.0, 0.0),
        base_offset: 0.0,
        top_offset: 12.0,
        rotation: 0.0,
        structural: true,
        top_at: Some(top),
    };
    let tops = |ed: &Editor| -> Vec<(Option<f64>, Option<f64>)> {
        let db = ed.model.sql();
        let mut s = db.prepare("SELECT tx, ty FROM col ORDER BY id").unwrap();
        let v: Vec<_> = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
        v
    };
    let (mut ed, _) = exec_editor();
    let lvl = ed.level;
    let mut st = create_plan(vec![col(lvl, (42.0, 3.0)), col(lvl, (42.0, -3.0))]);
    replay(&mut st, &mut ed, &Sets::default(), &[], &[]).unwrap();
    assert_eq!(tops(&ed), vec![(Some(42.0), Some(3.0)), (Some(42.0), Some(-3.0))], "the V pair");
    let (mut ed, _) = exec_editor();
    let st = create_plan(vec![col(ed.level, (42.0, 3.0))]);
    ed.run_ops(lowered_env(&st)).unwrap();
    assert_eq!(tops(&ed), vec![(Some(42.0), Some(3.0))], "the envelope executor");
    crate::editor::ops_run::set_thread_exec(None);
}
