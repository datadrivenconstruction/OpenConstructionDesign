use super::*;

use ddc_ops::{Actor, End, Group, Len, Side, P2, P3};

use rusqlite::OptionalExtension;

use crate::LayerSpec;

pub(super) const WT: &str = "ops-test-200";

pub(super) fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    m.add_wall_type(WT, &[LayerSpec { material: "Concrete".into(), thickness: 0.2 }])
        .unwrap();
    m
}

/// Content of every table except the journal and `ai_*`; rows sorted, so rowid order is moot.
pub(super) fn hash(m: &Model) -> u64 {
    use std::hash::{Hash, Hasher};
    let db = m.sql();
    let mut tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    tables.retain(|t| t != "op" && !t.starts_with("ai_") && !t.starts_with("sqlite_"));
    tables.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for t in tables {
        let mut st = db.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
        let n = st.column_count();
        let mut rows: Vec<String> = st
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| format!("{:?}", r.get::<_, rusqlite::types::Value>(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .map(|r| r.unwrap())
            // a random identity of each model, not its content
            .filter(|r: &String| !(t == "meta" && r.starts_with("Text(\"project_uid\")")))
            .collect();
        rows.sort();
        t.hash(&mut h);
        rows.hash(&mut h);
    }
    h.finish()
}

pub(super) fn group(id: u32, deps: &[u32], ops: Vec<Op>) -> Group {
    Group { id, title: format!("g{id}"), ops, depends_on: deps.to_vec(), why: String::new() }
}

pub(super) fn envelope(groups: Vec<Group>) -> Envelope {
    let mut e = Envelope::new(Actor::Agent { name: "test".into() }, "ops apply test");
    e.groups = groups;
    e
}

fn wall(slot: u32, level: Ref, x0: f64, x1: f64, z: f64) -> Op {
    Op::Create {
        slot,
        el: NewEl::Wall {
            level,
            a: Place::At { p: P3::m(x0, 0.0, z) },
            b: Place::At { p: P3::m(x1, 0.0, z) },
            height: Len::m(3.0),
            top: None,
            ty: Some(Ref::name(Cat::Type, WT)),
            fields: vec![],
        },
    }
}

fn opening(slot: u32, host: Ref, centre: f64) -> Op {
    Op::Create {
        slot,
        el: NewEl::Opening {
            host: host.clone(),
            at: Place::OnHost {
                host,
                along: Len::m(centre),
                from_end: End::A,
                side: Side::Center,
                z: Len::m(0.0),
            },
            w: Len::mm(1000.0),
            h: Len::mm(2000.0),
            sill: Len::mm(900.0),
        },
    }
}

/// Level, grid, two walls on the new level, a hole in the first wall: two groups.
fn building() -> Envelope {
    envelope(vec![
        group(
            1,
            &[],
            vec![
                Op::Level(LevelOp::Create {
                    slot: 1,
                    name: "ops-L1".into(),
                    elevation: Len::m(3.0),
                }),
                Op::Grid(GridOp::Create {
                    slot: 2,
                    name: "ops-A".into(),
                    a: P2::m(0.0, -1.0),
                    b: P2::m(0.0, 9.0),
                    bulge: 0.0,
                }),
            ],
        ),
        group(
            2,
            &[1],
            vec![
                wall(3, Ref::slot(1), 0.0, 6.0, 3.0),
                wall(4, Ref::slot(1), 6.0, 12.0, 3.0),
                opening(5, Ref::slot(3), 2.0),
            ],
        ),
    ])
}

fn level_named(m: &Model, name: &str) -> Option<i64> {
    m.sql()
        .query_row("SELECT id FROM level WHERE name=?1", [name], |r| r.get(0))
        .optional()
        .unwrap()
}

#[test]
fn envelope_is_one_journal_entry() {
    let mut m = model();
    let n0 = m.journal_len().unwrap();
    let r = m.apply_envelope(&building()).unwrap();
    assert_eq!(m.journal_len().unwrap(), n0 + 1);
    assert_eq!(r.journal_seq, m.op_top().unwrap());
    let top = &m.journal(1).unwrap()[0];
    assert_eq!(top.seq, r.journal_seq);
    assert_eq!(top.kind, ENVELOPE_KIND);
}

#[test]
fn receipt_maps_slots_to_ids() {
    let mut m = model();
    let r = m.apply_envelope(&building()).unwrap();
    let slots: Vec<u32> = r.created.iter().map(|(s, _)| *s).collect();
    assert_eq!(slots, vec![1, 2, 3, 4, 5]);
    let id = |slot: u32, cat: Cat| -> i64 {
        match &r.created.iter().find(|(s, _)| *s == slot).unwrap().1 {
            Ref::Id { cat: c, id } if *c == cat => *id,
            other => panic!("slot {slot}: {other:?}"),
        }
    };
    let lv = id(1, Cat::Level);
    assert_eq!(level_named(&m, "ops-L1"), Some(lv));
    let g = id(2, Cat::Grid);
    assert_eq!(m.grid(g).unwrap().name, "ops-A");
    let db = m.sql();
    for slot in [3, 4] {
        let (level, base): (i64, f64) = db
            .query_row("SELECT level, base_offset FROM wall WHERE id=?1", [id(slot, Cat::Wall)], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(level, lv);
        // z 3 m on a level at 3 m: the wall sits on the level
        assert!(base.abs() < 1e-9, "base offset {base}");
    }
    let (host, along): (i64, f64) = db
        .query_row("SELECT wall, along FROM opening WHERE id=?1", [id(5, Cat::Opening)], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(host, id(3, Cat::Wall));
    // centre 2 m, width 1 m: the stored start is 1.5 m
    assert!((along - 1.5).abs() < 1e-9, "along {along}");
    assert!(r.changed.is_empty() && r.deleted.is_empty() && r.copied.is_empty());
}

#[test]
fn failed_group_leaves_model_unchanged() {
    let mut m = model();
    let h0 = hash(&m);
    let n0 = m.journal_len().unwrap();
    let level = Op::Level(LevelOp::Create {
        slot: 1,
        name: "ops-L1".into(),
        elevation: Len::m(3.0),
    });

    // a ref to nothing in group 2
    let bad_ref = envelope(vec![
        group(1, &[], vec![level.clone()]),
        group(2, &[1], vec![Op::Delete { target: Targets::one(Ref::id(Cat::Wall, 999_999)) }]),
    ]);
    let e = m.apply_envelope(&bad_ref).unwrap_err();
    assert_eq!((e.group, e.op, e.code), (Some(2), Some(0), Code::NotFound));
    assert_eq!(e.path, "/groups/1/ops/0");

    // the model refuses: the opening does not fit its 6 m wall
    let no_fit = envelope(vec![
        group(1, &[], vec![level.clone()]),
        group(
            2,
            &[1],
            vec![wall(2, Ref::slot(1), 0.0, 6.0, 3.0), opening(3, Ref::slot(2), 5.9)],
        ),
    ]);
    let e = m.apply_envelope(&no_fit).unwrap_err();
    assert_eq!((e.group, e.op), (Some(2), Some(1)));
    assert!(!matches!(e.code, Code::NotFound | Code::Unsupported | Code::Internal), "{e}");

    // an op kind without a model function
    let unsupported = envelope(vec![
        group(1, &[], vec![level]),
        group(
            2,
            &[1],
            vec![Op::Param(ddc_ops::ParamOp::Delete { name: "x".into() })],
        ),
    ]);
    let e = m.apply_envelope(&unsupported).unwrap_err();
    assert_eq!(e.code, Code::Unsupported);
    assert!(e.to_string().contains("param.delete"), "{e}");

    assert_eq!(level_named(&m, "ops-L1"), None);
    assert_eq!(m.journal_len().unwrap(), n0);
    assert_eq!(hash(&m), h0);
    // the model still takes the good part afterwards
    assert!(m.apply_envelope(&building()).is_ok());
}

#[test]
fn structure_errors_reject_before_any_write() {
    let mut m = model();
    let h0 = hash(&m);
    let e = m.apply_envelope(&envelope(vec![])).unwrap_err();
    assert_eq!(e.code, Code::NoOps, "{e}");
    let unknown_slot = envelope(vec![group(1, &[], vec![wall(2, Ref::slot(7), 0.0, 1.0, 0.0)])]);
    let e = m.apply_envelope(&unknown_slot).unwrap_err();
    assert_eq!(e.code, Code::UnknownSlot, "{e}");
    assert_eq!(hash(&m), h0);
}

#[test]
fn dry_run_leaves_model_untouched_and_matches_apply() {
    let mut m = model();
    let h0 = hash(&m);
    let n0 = m.journal_len().unwrap();
    let dry = m.dry_run_envelope(&building()).unwrap();
    assert_eq!(hash(&m), h0);
    assert_eq!(m.journal_len().unwrap(), n0);
    assert_eq!(level_named(&m, "ops-L1"), None);
    let real = m.apply_envelope(&building()).unwrap();
    assert_eq!(dry, real);
    // a dry run that fails reports the same rejection as the apply
    let bad = envelope(vec![group(
        1,
        &[],
        vec![Op::Delete { target: Targets::one(Ref::id(Cat::Wall, 999_999)) }],
    )]);
    let h1 = hash(&m);
    assert_eq!(m.dry_run_envelope(&bad).unwrap_err(), m.apply_envelope(&bad).unwrap_err());
    assert_eq!(hash(&m), h1);
}

#[test]
fn undo_redo_envelope_exact() {
    let mut m = model();
    let h0 = hash(&m);
    let n0 = m.journal_len().unwrap();
    let r1 = m.apply_envelope(&building()).unwrap();
    let h1 = hash(&m);
    assert_ne!(h1, h0);
    m.undo().unwrap();
    assert_eq!(hash(&m), h0);
    assert_eq!(m.journal_len().unwrap(), n0);
    // redo is a replay of the envelope: same ids, same rows, same journal seq
    let r2 = m.apply_envelope(&building()).unwrap();
    assert_eq!(r2, r1);
    assert_eq!(hash(&m), h1);
}

#[test]
fn edits_of_existing_rows_are_reported() {
    let mut m = model();
    let r = m.apply_envelope(&building()).unwrap();
    let wall_ref = r.created[2].1.clone();
    let h1 = hash(&m);
    let edits = envelope(vec![group(
        1,
        &[],
        vec![
            Op::Level(LevelOp::Rename { target: Ref::name(Cat::Level, "ops-L1"), name: "ops-L1b".into() }),
            Op::Grid(GridOp::Rename { target: Ref::name(Cat::Grid, "ops-A"), name: "ops-B".into() }),
            Op::Transform {
                target: Targets::one(wall_ref.clone()),
                xf: Xform::Move { by: ddc_ops::V3::m(0.0, 1.0, 0.0) },
                copy: false,
            },
            Op::Delete { target: Targets::one(r.created[3].1.clone()) },
        ],
    )]);
    let e = m.apply_envelope(&edits).unwrap();
    assert_eq!(e.deleted, vec![r.created[3].1.clone()]);
    assert!(e.changed.contains(&wall_ref));
    assert!(level_named(&m, "ops-L1b").is_some());
    m.undo().unwrap();
    assert_eq!(hash(&m), h1);
}

// ---- geometry ops (ops_geo.rs) ----

use ddc_geom::csg::BoolOp;
use ddc_ops::{BoolKind, ConvertTo as OpConvertTo, Dir3, Form, Frame, GeoOp, Profile, Tool};

fn up_frame(x: f64, y: f64, z: f64) -> Frame {
    Frame { origin: P3::m(x, y, z), x: Dir3::X, z: Dir3::Z }
}

fn extrude(profile: Profile, x: f64) -> Form {
    Form::Extrude { profile, frame: up_frame(x, 0.0, 1.0), start: Len::m(0.0), end: Len::m(2.0) }
}

fn square(w: f64) -> Profile {
    Profile::Rect { w: Len::m(w), h: Len::m(w) }
}

fn geo_solid(slot: u32, level: i64, form: Form) -> Op {
    Op::Geometry(GeoOp::Solid {
        slot,
        level: Some(Ref::id(Cat::Level, level)),
        form,
        material: Some("steel".into()),
        name: None,
        ifc: None,
        role: None,
    })
}

fn geo_mass(slot: u32, level: i64, form: Form) -> Op {
    Op::Geometry(GeoOp::Mass { slot, level: Some(Ref::id(Cat::Level, level)), form, name: None })
}

fn slot_id(r: &Receipt, slot: u32) -> (Cat, i64) {
    match r.created.iter().find(|(s, _)| *s == slot) {
        Some((_, Ref::Id { cat, id })) => (*cat, *id),
        other => panic!("slot {slot}: {other:?}"),
    }
}

#[test]
fn envelope_extrude_i_profile_volume() {
    let mut m = model();
    let lv = m.add_level("geo-L1", 1.0).unwrap();
    let i = Profile::I { h: Len::mm(300.0), b: Len::mm(150.0), tw: Len::mm(10.0), tf: Len::mm(20.0) };
    let env = envelope(vec![group(1, &[], vec![geo_solid(1, lv, extrude(i, 2.0))])]);
    let n0 = m.journal_len().unwrap();
    let r = m.apply_envelope(&env).unwrap();
    let (cat, id) = slot_id(&r, 1);
    assert_eq!(cat, Cat::Solid);
    let area = 2.0 * 0.15 * 0.02 + (0.30 - 2.0 * 0.02) * 0.01;
    let q = m.solid_quantities(id).unwrap().expect("solid quantities");
    assert!((q.volume - 2.0 * area).abs() < 1e-6, "volume {} vs {}", q.volume, 2.0 * area);
    assert!(r.loss.is_empty(), "{:?}", r.loss);
    assert_eq!(m.journal_len().unwrap(), n0 + 1);
}

#[test]
fn envelope_boolean_cut_keeps_link() {
    let mut m = model();
    let lv = m.add_level("geo-L1", 1.0).unwrap();
    let cut = Op::Geometry(GeoOp::Boolean {
        kind: BoolKind::Cut,
        a: Ref::slot(1),
        b: Tool::Ref { of: Ref::slot(2) },
        keep_tool: true,
    });
    let env = envelope(vec![group(
        1,
        &[],
        vec![geo_mass(1, lv, extrude(square(2.0), 0.0)), geo_mass(2, lv, extrude(square(1.0), 1.0)), cut],
    )]);
    let n0 = m.journal_len().unwrap();
    let r = m.apply_envelope(&env).unwrap();
    let ((ca, a), (cb, b)) = (slot_id(&r, 1), slot_id(&r, 2));
    assert_eq!((ca, cb), (Cat::Mass, Cat::Mass));
    let res = match r.changed.last() {
        Some(Ref::Id { cat: Cat::Mass, id }) => *id,
        other => panic!("boolean result missing: {other:?}"),
    };
    // the result is a new row without a slot: the receipt lists it as made
    assert!(r.copied.contains(&Ref::id(Cat::Mass, res)), "{:?}", r.copied);
    let link = m.boolean_link(res).unwrap().expect("linked result");
    assert_eq!((link.op, link.a, link.b), (BoolOp::Subtract, a, b));
    assert_eq!(m.boolean_dependents(b).unwrap(), vec![res]);
    // one envelope, one undo step
    assert_eq!(m.journal_len().unwrap(), n0 + 1);

    // a tool the boolean would consume is refused by name, nothing written
    let h0 = hash(&m);
    let drop_tool = Op::Geometry(GeoOp::Boolean {
        kind: BoolKind::Cut,
        a: Ref::id(Cat::Mass, a),
        b: Tool::Ref { of: Ref::id(Cat::Mass, b) },
        keep_tool: false,
    });
    let e = m.apply_envelope(&envelope(vec![group(1, &[], vec![drop_tool])])).unwrap_err();
    assert_eq!(e.code, Code::Unsupported);
    assert!(e.to_string().contains("keep_tool"), "{e}");
    assert_eq!(hash(&m), h0);
}

#[test]
fn envelope_convert_reports_loss() {
    let mut m = model();
    let lv = m.add_level("geo-L1", 1.0).unwrap();
    let convert = Op::Geometry(GeoOp::Convert { target: Ref::slot(1), to: OpConvertTo::Mesh });
    let env = envelope(vec![group(1, &[], vec![geo_solid(1, lv, extrude(square(1.0), 0.0)), convert])]);
    let h0 = hash(&m);
    let dry = m.dry_run_envelope(&env).unwrap();
    assert_eq!(hash(&m), h0, "dry run leaves the model as it was");
    let r = m.apply_envelope(&env).unwrap();
    assert!(!r.loss.is_empty(), "solid to mesh drops the parameters");
    assert!(r.loss.iter().any(|l| l.starts_with("parameters")), "{:?}", r.loss);
    assert_eq!(dry.loss, r.loss, "dry run reports the same loss");
    assert!(
        r.changed.iter().any(|x| matches!(x, Ref::Id { cat: Cat::Mesh, .. })),
        "{:?}",
        r.changed
    );
}

#[test]
fn bad_profile_param_rejection_path() {
    let mut m = model();
    let lv = m.add_level("geo-L1", 1.0).unwrap();
    let bad = Profile::I { h: Len::mm(300.0), b: Len::mm(150.0), tw: Len::mm(0.0), tf: Len::mm(20.0) };
    let env = envelope(vec![group(
        1,
        &[],
        vec![geo_solid(1, lv, extrude(square(1.0), 0.0)), geo_solid(2, lv, extrude(bad, 3.0))],
    )]);
    let (h0, n0) = (hash(&m), m.journal_len().unwrap());
    let e = m.apply_envelope(&env).unwrap_err();
    assert_eq!((e.group, e.op, e.path.as_str()), (Some(1), Some(1), "/groups/0/ops/1"));
    assert_eq!(e.code, Code::Invalid, "{e}");
    assert!(e.to_string().contains("web"), "{e}");
    // the good first op is rolled back with it
    assert_eq!(hash(&m), h0);
    assert_eq!(m.journal_len().unwrap(), n0);
}

#[test]
fn geometry_solid_carries_its_ifc_class_and_refuses_one_off_the_list() {
    let mut m = model();
    let lv = m.add_level("geo-L1", 1.0).unwrap();
    let classed = |slot: u32, class: &str, pre: Option<&str>| {
        let mut op = geo_solid(slot, lv, extrude(square(1.0), 0.0));
        if let Op::Geometry(GeoOp::Solid { ifc, .. }) = &mut op {
            *ifc = Some(ddc_ops::IfcClass {
                class: class.into(),
                predefined: pre.map(str::to_string),
            });
        }
        op
    };
    let r = m
        .apply_envelope(&envelope(vec![group(1, &[], vec![classed(1, "IfcMember", Some("chord"))])]))
        .unwrap();
    let (_, id) = slot_id(&r, 1);
    let c = m.solid(id).unwrap().ifc.expect("class kept");
    assert_eq!((c.class.as_str(), c.predefined.as_str()), ("IfcMember", "CHORD"));
    let h0 = hash(&m);
    for (class, pre) in [("IfcWall", None), ("IfcSlab", Some("CHORD"))] {
        let env = envelope(vec![group(1, &[], vec![classed(1, class, pre)])]);
        let e = m.apply_envelope(&env).unwrap_err();
        assert_eq!(e.code, Code::Invalid, "{e}");
        assert!(e.to_string().contains("not allowed") || e.to_string().contains("no predefined"), "{e}");
    }
    assert_eq!(hash(&m), h0);
}

fn option_of(m: &Model, env: &Envelope) -> i64 {
    let (base_head, base_stamp) = m.ai_option_head().unwrap();
    m.ai_option_add(&crate::OptionRec {
        id: 0,
        set: "s".into(),
        name: "A".into(),
        base_head,
        base_stamp,
        envelope: env.to_json().unwrap(),
        seed: 0,
        params: "{}".into(),
        generator: Some("gen-x".into()),
        metrics: None,
        note: None,
        why: Some("option why".into()),
        status: OptionStatus::Draft,
    })
    .unwrap()
}

#[test]
fn option_apply_stamps_ai_actor() {
    let mut m = model();
    let id = option_of(&m, &building());
    let n0 = m.journal_len().unwrap();
    let r = m.ai_option_apply_ops(id).unwrap();
    assert_eq!(m.journal_len().unwrap(), n0 + 1, "one journal entry");
    let top = &m.journal(1).unwrap()[0];
    assert_eq!(top.seq, r.journal_seq);
    let ai = Actor::Ai { session: format!("option:{id}"), model: "gen-x".into() };
    assert_eq!(top.actor, Some(ai.clone()));
    assert_eq!(top.why.as_deref(), Some("option why"));
    assert!(top.has_src, "forward envelope stored");
    let src = m.journal_src(top.seq).unwrap().expect("forward envelope");
    assert_eq!(src.actor, ai);
    assert_eq!(src.groups, building().groups);
    assert_eq!(m.ai_option_get(id).unwrap().unwrap().status, OptionStatus::Accepted);
}

/// A session recipe's Apply writes its row in the step; the receipt names the one merged
/// entry `before + 1` (the value a failed read after the commit falls back to), stamped with the
/// new row's id.
#[test]
fn option_apply_new_names_the_merged_entry() {
    let mut m = model();
    let id = option_of(&m, &building());
    let rec = m.ai_option_get(id).unwrap().unwrap();
    m.sql().execute("DELETE FROM ai_option WHERE id=?1", [id]).unwrap();
    let before = m.op_top().unwrap();
    let (r, new_id) = m.ai_option_apply_new_ops(&rec).unwrap();
    assert_eq!((r.journal_seq, m.op_top().unwrap()), (before + 1, before + 1));
    let top = &m.journal(1).unwrap()[0];
    let ai = Actor::Ai { session: format!("option:{new_id}"), model: "gen-x".into() };
    assert_eq!((top.seq, top.actor.clone()), (r.journal_seq, Some(ai)));
    assert_eq!(m.ai_option_get(new_id).unwrap().unwrap().status, OptionStatus::Accepted);
    let again = m.ai_option_apply_new_ops(&rec).unwrap_err();
    assert_eq!(again, crate::OptionApplyError::Stale, "the model moved past the base");
    assert_eq!(m.ai_option_list(None).unwrap().len(), 1, "a refusal adds no row");
}

#[test]
fn option_apply_listed_in_ai_batches() {
    let listed = |m: &Model, seq: i64| m.ai_batches().unwrap().into_iter().find(|b| b.op_seq == seq);
    let mut m = model();
    let h0 = hash(&m);
    let id = option_of(&m, &building());
    let r = m.ai_option_apply_ops(id).unwrap();
    let b = listed(&m, r.journal_seq).expect("option Apply lists as an AI batch");
    assert_eq!(b.audit, -r.journal_seq, "envelope batch, no audit row");
    assert_eq!(b.model.as_deref(), Some("gen-x"));
    assert_eq!(b.why.as_deref(), Some("option why"));
    assert!(!b.reverted);
    // one undo step reverts the whole option and drops it from the list
    m.undo().unwrap();
    assert_eq!(hash(&m), h0);
    assert!(listed(&m, r.journal_seq).is_none());
}

/// One wall a->b on a new level at 0, with the given create-time fields.
fn wall_with(fields: Vec<Field>) -> Envelope {
    let level = Op::Level(LevelOp::Create {
        slot: 1,
        name: "ops-L1".into(),
        elevation: Len::m(0.0),
    });
    let wall = Op::Create {
        slot: 2,
        el: NewEl::Wall {
            level: Ref::slot(1),
            a: Place::At { p: P3::m(0.0, 0.0, 0.0) },
            b: Place::At { p: P3::m(4.0, 0.0, 0.0) },
            height: Len::m(3.0),
            top: None,
            ty: Some(Ref::name(Cat::Type, WT)),
            fields,
        },
    };
    envelope(vec![group(1, &[], vec![level]), group(2, &[1], vec![wall])])
}

fn bulge_field(v: serde_json::Value) -> Field {
    Field::new("bulge", ddc_ops::Value::plain(v))
}

#[test]
fn create_wall_with_bulge_makes_arc_wall() {
    let mut m = model();
    let n0 = m.journal_len().unwrap();
    let r = m.apply_envelope(&wall_with(vec![bulge_field(serde_json::json!(0.5))])).unwrap();
    assert_eq!(m.journal_len().unwrap(), n0 + 1, "one journal entry");
    let Some((_, Ref::Id { cat: Cat::Wall, id })) = r.created.iter().find(|(s, _)| *s == 2) else {
        panic!("no wall: {:?}", r.created);
    };
    assert_eq!(m.wall_bulge(*id).unwrap(), 0.5);
    let kind: i64 = m
        .sql()
        .query_row("SELECT axis_kind FROM wall WHERE id=?1", [*id], |r| r.get(0))
        .unwrap();
    assert_eq!(kind, 1, "arc axis");
    // zero bulge stays a straight wall
    let mut s = model();
    let r = s.apply_envelope(&wall_with(vec![bulge_field(serde_json::json!(0.0))])).unwrap();
    let Some((_, Ref::Id { id, .. })) = r.created.iter().find(|(k, _)| *k == 2) else {
        panic!("no wall");
    };
    assert_eq!(s.wall_bulge(*id).unwrap(), 0.0);
}

#[test]
fn create_wall_bad_bulge_or_other_field_is_rejected_unchanged() {
    let bad = [
        vec![Field::new("bulge", ddc_ops::Value::num(0.5, ddc_ops::Unit::M))],
        vec![bulge_field(serde_json::json!("x"))],
        vec![bulge_field(serde_json::json!(0.5)), bulge_field(serde_json::json!(0.2))],
        // a half-circle limit and beyond is not a valid arc
        vec![bulge_field(serde_json::json!(1e9))],
    ];
    for fields in bad {
        let mut m = model();
        let (h0, n0) = (hash(&m), m.journal_len().unwrap());
        let e = m.apply_envelope(&wall_with(fields.clone())).unwrap_err();
        assert!(!matches!(e.code, Code::Unsupported | Code::Internal), "{fields:?}: {e}");
        assert_eq!((hash(&m), m.journal_len().unwrap()), (h0, n0), "{fields:?}");
    }
    let mut m = model();
    let offset0 = Field::new("offset0", ddc_ops::Value::num(0.1, ddc_ops::Unit::M));
    let e = m.apply_envelope(&wall_with(vec![bulge_field(serde_json::json!(0.5)), offset0])).unwrap_err();
    assert_eq!(e.code, Code::Unsupported);
    assert!(e.to_string().contains("offset0"), "{e}");
}

/// e2e: an inward-sloped tower from one envelope (as the generator or the AI writes it): four
/// walls with a `lean` field. The shell volume matches the analytic one (corners neither gap
/// nor overlap), one journal entry, one undo restores the model; a later `set` tapers them.
#[test]
fn envelope_inward_sloped_tower_volume_is_analytic() {
    let (lw, h, t, deg) = (10.0, 6.0, 0.2, 8.0f64);
    let mut m = model();
    let (h0, n0) = (hash(&m), m.journal_len().unwrap());
    let level = Op::Level(LevelOp::Create { slot: 1, name: "ops-L1".into(), elevation: Len::m(0.0) });
    let p = [(0.0, 0.0), (lw, 0.0), (lw, lw), (0.0, lw)];
    let walls: Vec<Op> = (0..4)
        .map(|i| {
            let (a, b) = (p[i], p[(i + 1) % 4]);
            Op::Create {
                slot: 2 + i as u32,
                el: NewEl::Wall {
                    level: Ref::slot(1),
                    a: Place::At { p: P3::m(a.0, a.1, 0.0) },
                    b: Place::At { p: P3::m(b.0, b.1, 0.0) },
                    height: Len::m(h),
                    top: None,
                    ty: Some(Ref::name(Cat::Type, WT)),
                    fields: vec![Field::new("lean", ddc_ops::Value::num(deg, ddc_ops::Unit::Deg))],
                },
            }
        })
        .collect();
    let r = m.apply_envelope(&envelope(vec![group(1, &[], vec![level]), group(2, &[1], walls)])).unwrap();
    assert_eq!(m.journal_len().unwrap(), n0 + 1);
    let ids: Vec<i64> = r
        .created
        .iter()
        .filter_map(|(_, x)| match x {
            Ref::Id { cat: Cat::Wall, id } => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 4);
    for id in &ids {
        assert!((m.wall_lean(*id).unwrap().angle - deg.to_radians()).abs() < 1e-12);
    }
    let vol = |m: &Model| -> f64 { m.material_volumes().unwrap().iter().map(|v| v.1).sum() };
    let tn = deg.to_radians().tan();
    let want = 2.0 * t * h * (2.0 * lw - 2.0 * tn * h);
    assert!((vol(&m) - want).abs() < 1e-6, "{} vs {want}", vol(&m));
    assert!(m.wall_warnings().unwrap().iter().all(|w| !w.message.contains("lean")));
    // taper all four to half the thickness at the top: the shell section halves at the top
    let set: Vec<Op> = ids
        .iter()
        .map(|id| Op::Set {
            target: Targets::one(Ref::id(Cat::Wall, *id)),
            fields: vec![Field::new("top_scale", ddc_ops::Value::plain(serde_json::json!(0.5)))],
        })
        .collect();
    m.apply_envelope(&envelope(vec![group(1, &[], set)])).unwrap();
    assert!(vol(&m) < want && vol(&m) > 0.5 * want, "{}", vol(&m));
    m.undo().unwrap();
    assert!((vol(&m) - want).abs() < 1e-6);
    m.undo().unwrap();
    assert_eq!(hash(&m), h0);
}

// ---- set_type across categories ----

struct Retype {
    m: Model,
    slabs: Vec<i64>,
    cols: Vec<i64>,
    st_a: i64,
    st_b: i64,
    mt_b: i64,
    wall: i64,
}

fn retype_model() -> Retype {
    let mut m = model();
    let lv = m.add_level("rt-L1", 0.0).unwrap();
    let lay = [LayerSpec { material: "Concrete".into(), thickness: 0.2 }];
    let st_a = m.add_slab_type("rt-slab-a", &lay).unwrap();
    let st_b = m.add_slab_type("rt-slab-b", &lay).unwrap();
    let sq = |x: f64| vec![(x, 0.0), (x + 4.0, 0.0), (x + 4.0, 4.0), (x, 4.0)];
    let slabs = (0..2)
        .map(|i| {
            m.add_slab(&crate::SlabSpec {
                level: lv,
                slab_type: st_a,
                offset: 0.0,
                outer: sq(i as f64 * 10.0),
                holes: vec![],
            })
            .unwrap()
        })
        .collect();
    let mt_a = m
        .add_member_type("rt-col-a", &ddc_geom::member::Shape::parse("rect 0.3 0.3").unwrap(), "Concrete")
        .unwrap();
    let mt_b = m
        .add_member_type("rt-col-b", &ddc_geom::member::Shape::parse("rect 0.4 0.4").unwrap(), "Concrete")
        .unwrap();
    let cols = (0..2)
        .map(|i| {
            m.add_column(&crate::ColumnSpec {
                level: lv,
                top_level: None,
                member_type: mt_a,
                structural: true,
                at: (i as f64 * 3.0, 20.0),
                base_offset: 0.0,
                top_offset: 3.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap()
        })
        .collect();
    let w = envelope(vec![group(1, &[], vec![wall(1, Ref::id(Cat::Level, lv), 0.0, 5.0, 0.0)])]);
    let wall = match m.apply_envelope(&w).unwrap().created[0].1.clone() {
        Ref::Id { id, .. } => id,
        _ => unreachable!(),
    };
    Retype { m, slabs, cols, st_a, st_b, mt_b, wall }
}

fn retype_env(refs: Vec<Ref>, to: Ref) -> Envelope {
    envelope(vec![group(1, &[], vec![Op::SetType { target: Targets::Refs { refs }, to }])])
}

fn stype_of(m: &Model, table: &str, id: i64) -> i64 {
    let c = if table == "slab" { "stype" } else { "mtype" };
    m.sql().query_row(&format!("SELECT {c} FROM {table} WHERE id=?1"), [id], |r| r.get(0)).unwrap()
}

#[test]
fn set_type_slabs_one_undo_step() {
    let mut t = retype_model();
    let (h0, n0) = (hash(&t.m), t.m.journal_len().unwrap());
    let refs = t.slabs.iter().map(|&i| Ref::id(Cat::Slab, i)).collect();
    let r = t.m.apply_envelope(&retype_env(refs, Ref::id(Cat::SlabType, t.st_b))).unwrap();
    assert_eq!(r.changed.len(), 2);
    assert_eq!(t.m.journal_len().unwrap(), n0 + 1);
    for &s in &t.slabs {
        assert_eq!(stype_of(&t.m, "slab", s), t.st_b);
    }
    t.m.undo().unwrap();
    assert_eq!(hash(&t.m), h0);
    assert_eq!(t.m.journal_len().unwrap(), n0);
    assert_eq!(stype_of(&t.m, "slab", t.slabs[0]), t.st_a);
}

#[test]
fn set_type_mixed_cats_refused_nothing_written() {
    let mut t = retype_model();
    let (h0, n0) = (hash(&t.m), t.m.journal_len().unwrap());
    // slab first, then a column under a slab type: nothing may be written
    let refs = vec![Ref::id(Cat::Slab, t.slabs[0]), Ref::id(Cat::Column, t.cols[0])];
    assert!(t.m.apply_envelope(&retype_env(refs, Ref::id(Cat::SlabType, t.st_b))).is_err());
    // a wall and a slab under a wall type
    let refs = vec![Ref::id(Cat::Wall, t.wall), Ref::id(Cat::Slab, t.slabs[0])];
    assert!(t.m.apply_envelope(&retype_env(refs, Ref::name(Cat::Type, WT))).is_err());
    assert_eq!(hash(&t.m), h0);
    assert_eq!(t.m.journal_len().unwrap(), n0);
}

#[test]
fn set_type_undo_redo_restores_all_targets() {
    let mut t = retype_model();
    let h0 = hash(&t.m);
    let mut refs: Vec<Ref> = t.cols.iter().map(|&i| Ref::id(Cat::Column, i)).collect();
    refs.push(Ref::id(Cat::Wall, t.wall));
    // columns and a wall in one envelope: two ops, one entry
    let env = envelope(vec![group(
        1,
        &[],
        vec![
            Op::SetType {
                target: Targets::Refs { refs: refs[..2].to_vec() },
                to: Ref::id(Cat::MemberType, t.mt_b),
            },
            Op::SetType { target: Targets::Refs { refs: vec![refs[2].clone()] }, to: Ref::name(Cat::Type, WT) },
        ],
    )]);
    let r1 = t.m.apply_envelope(&env).unwrap();
    let h1 = hash(&t.m);
    assert_ne!(h1, h0);
    t.m.undo().unwrap();
    assert_eq!(hash(&t.m), h0);
    let r2 = t.m.apply_envelope(&env).unwrap();
    assert_eq!(r2, r1);
    assert_eq!(hash(&t.m), h1);
    assert_eq!(stype_of(&t.m, "col", t.cols[1]), t.mt_b);
}

#[test]
fn set_type_opening_still_refused_unchanged() {
    let mut t = retype_model();
    let h0 = hash(&t.m);
    let refs = vec![Ref::id(Cat::Door, 1)];
    assert!(t.m.apply_envelope(&retype_env(refs, Ref::id(Cat::SlabType, t.st_b))).is_err());
    assert_eq!(hash(&t.m), h0);
}
