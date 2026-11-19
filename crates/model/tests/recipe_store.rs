//! Recipes and the executor ops a rerun needs: reshape of an extruded solid in place, delete of
//! solids, recipe rows written and undone with the elements in one step.

use ddc_model::{Model, Recipe, RecipePart};
use ddc_ops::{
    Actor, Cat, Dir3, Envelope, Form, Frame, GeoOp, Group, Len, Op, Profile, Ref, Targets, P3,
};

fn bar(x: f64, len: f64) -> Form {
    Form::Extrude {
        profile: Profile::Rect {
            w: Len::mm(100.0),
            h: Len::mm(200.0),
        },
        frame: Frame {
            origin: P3::m(x, 0.0, 0.0),
            x: Dir3::Y,
            z: Dir3::X,
        },
        start: Len::m(0.0),
        end: Len::m(len),
    }
}

fn env(ops: Vec<Op>) -> Envelope {
    let mut e = Envelope::new(
        Actor::Script {
            name: "test".into(),
        },
        "recipe test",
    );
    e.groups.push(Group {
        id: 1,
        title: "g".into(),
        ops,
        depends_on: vec![],
        why: String::new(),
    });
    e
}

fn model() -> (Model, Ref) {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    (m, Ref::id(Cat::Level, l))
}

fn solid(slot: u32, level: &Ref, x: f64) -> Op {
    Op::Geometry(GeoOp::Solid {
        slot,
        level: Some(level.clone()),
        form: bar(x, 2.0),
        material: None,
        name: None,
        ifc: None,
        role: None,
    })
}

fn count(m: &Model, t: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn recipe_lands_and_leaves_with_its_elements_in_one_step() {
    let (mut m, l) = model();
    let e = env(vec![solid(0, &l, 0.0), solid(1, &l, 3.0)]);
    let ids = m
        .atomic("script", |m| {
            let r = m
                .apply_envelope_step(&e)
                .map_err(|r| ddc_model::Error::Invalid(r.to_string()))?;
            let parts: Vec<RecipePart> = r
                .created
                .iter()
                .zip(["a", "b"])
                .map(|((_, rf), k)| match rf {
                    Ref::Id { id, .. } => RecipePart {
                        key: k.into(),
                        tbl: "solid".into(),
                        el: *id,
                    },
                    _ => unreachable!(),
                })
                .collect();
            let rec = Recipe {
                id: 0,
                name: "bars".into(),
                script: "-- bars".into(),
                params_json: "{}".into(),
                hash: "h1".into(),
                version: 1,
            };
            let id = m.recipe_add(&rec, &parts)?;
            Ok((id, parts))
        })
        .unwrap();
    let (rid, parts) = ids;
    assert_eq!(m.recipe(rid).unwrap().unwrap().hash, "h1");
    assert_eq!(m.recipe_parts(rid).unwrap(), parts);
    assert_eq!(
        m.recipe_of("solid", parts[1].el).unwrap(),
        Some((rid, "b".to_string()))
    );

    // a rerun: b changes in place, a goes, c is new
    let before = m.solid(parts[1].el).unwrap();
    let e2 = env(vec![
        Op::Delete {
            target: Targets::one(Ref::id(Cat::Solid, parts[0].el)),
        },
        Op::Geometry(GeoOp::Reshape {
            target: Ref::id(Cat::Solid, parts[1].el),
            form: bar(3.0, 5.0),
        }),
        solid(0, &l, 9.0),
    ]);
    m.atomic("script_rerun", |m| {
        let r = m
            .apply_envelope_step(&e2)
            .map_err(|r| ddc_model::Error::Invalid(r.to_string()))?;
        assert_eq!(r.changed, vec![Ref::id(Cat::Solid, parts[1].el)]);
        let Ref::Id { id: c, .. } = r.created[0].1 else {
            unreachable!()
        };
        m.recipe_update(
            rid,
            "{\"len\":5}",
            "h2",
            1,
            &["a".into()],
            &[RecipePart {
                key: "c".into(),
                tbl: "solid".into(),
                el: c,
            }],
        )
    })
    .unwrap();
    assert_eq!(count(&m, "solid"), 2);
    assert_ne!(
        m.solid(parts[1].el).unwrap().form,
        before.form,
        "reshaped in place"
    );
    assert_eq!(m.solid(parts[1].el).unwrap().level, before.level);
    let keys: Vec<String> = m
        .recipe_parts(rid)
        .unwrap()
        .into_iter()
        .map(|p| p.key)
        .collect();
    assert_eq!(keys, ["b", "c"]);
    assert_eq!(m.recipe(rid).unwrap().unwrap().params_json, "{\"len\":5}");

    // one undo returns the first state exactly
    m.undo().unwrap();
    assert_eq!(m.recipe(rid).unwrap().unwrap().hash, "h1");
    assert_eq!(m.recipe_parts(rid).unwrap(), parts);
    assert_eq!(m.solid(parts[1].el).unwrap(), before);
    // and one more removes recipe and elements together
    m.undo().unwrap();
    assert!(m.recipe(rid).unwrap().is_none());
    assert_eq!((count(&m, "solid"), count(&m, "recipe_part")), (0, 0));
}

#[test]
fn reshape_refuses_what_it_cannot_do_in_place() {
    let (mut m, l) = model();
    let r = m.apply_envelope(&env(vec![solid(0, &l, 0.0)])).unwrap();
    let target = r.created[0].1.clone();
    let sweep = Form::Sweep {
        profile: Profile::Circle { r: Len::mm(50.0) },
        path: ddc_ops::Path::Polyline {
            pts: vec![P3::m(0.0, 0.0, 0.0), P3::m(1.0, 0.0, 0.0)],
            bulges: vec![],
            closed: false,
        },
        twist: None,
        scale: None,
    };
    let e = m.apply_envelope(&env(vec![Op::Geometry(GeoOp::Reshape {
        target,
        form: sweep,
    })]));
    assert!(e.unwrap_err().to_string().contains("not lowered"));
}

#[test]
fn recipe_links_only_to_element_tables() {
    let (mut m, _) = model();
    let rec = Recipe {
        id: 0,
        name: String::new(),
        script: String::new(),
        params_json: "{}".into(),
        hash: String::new(),
        version: 1,
    };
    let bad = RecipePart {
        key: "x".into(),
        tbl: "wall; DROP TABLE wall".into(),
        el: 1,
    };
    assert!(m.recipe_add(&rec, &[bad]).is_err());
    assert!(m.recipes().unwrap().is_empty());
}

#[test]
fn a_reused_row_id_is_never_taken_for_the_deleted_part() {
    let (mut m, l) = model();
    let e = env(vec![solid(0, &l, 0.0)]);
    let (rid, el) = m
        .atomic("script", |m| {
            let r = m
                .apply_envelope_step(&e)
                .map_err(|r| ddc_model::Error::Invalid(r.to_string()))?;
            let Ref::Id { id, .. } = r.created[0].1 else {
                unreachable!()
            };
            let rec = Recipe {
                id: 0,
                name: "one".into(),
                script: "-- one".into(),
                params_json: "{}".into(),
                hash: "h".into(),
                version: 1,
            };
            let part = RecipePart {
                key: "bar".into(),
                tbl: "solid".into(),
                el: id,
            };
            Ok((m.recipe_add(&rec, &[part])?, id))
        })
        .unwrap();
    // the person deletes the last row; the next solid gets the same id
    m.delete_solid(el).unwrap();
    assert_eq!(m.recipe_of("solid", el).unwrap(), None, "the link left with the row");
    let other = m.atomic("by hand", |m| {
        let r = m
            .apply_envelope_step(&env(vec![solid(0, &l, 5.0)]))
            .map_err(|r| ddc_model::Error::Invalid(r.to_string()))?;
        let Ref::Id { id, .. } = r.created[0].1 else {
            unreachable!()
        };
        Ok(id)
    });
    let other = other.unwrap();
    assert_eq!(other, el, "SQLite hands the freed id out again");
    assert_eq!(m.recipe_of("solid", other).unwrap(), None, "a stranger is not a recipe part");
    assert!(m.recipe_parts(rid).unwrap().is_empty());
    // undo of the hand-made solid and of the delete brings the original link back
    m.undo().unwrap();
    m.undo().unwrap();
    assert_eq!(m.recipe_of("solid", el).unwrap(), Some((rid, "bar".to_string())));
    assert_eq!(m.recipe_parts(rid).unwrap().len(), 1);
}

fn role_solid(slot: u32, level: &Ref, role: &str, x: f64) -> Op {
    let Op::Geometry(GeoOp::Solid { slot, level, form, material, name, ifc, .. }) =
        solid(slot, level, x)
    else {
        unreachable!()
    };
    Op::Geometry(GeoOp::Solid {
        slot,
        level,
        form,
        material,
        name,
        ifc,
        role: Some(ddc_ops::RoleTag { name: role.into(), at: P3::m(x, 0.0, 5.0) }),
    })
}

#[test]
fn a_role_on_a_solid_writes_a_component_row_in_the_same_step() {
    let (mut m, l) = model();
    let e = env(vec![
        role_solid(0, &l, "blade", 0.0),
        role_solid(1, &l, "blade", 3.0),
        role_solid(2, &l, "gondola", 6.0),
        solid(3, &l, 9.0),
    ]);
    m.apply_envelope_step(&e).unwrap();
    assert_eq!(count(&m, "solid"), 4);
    assert_eq!(count(&m, "comp"), 3);
    // one family per role, shared by its rows
    let fams: Vec<String> = m
        .sql()
        .prepare("SELECT name FROM comp_family ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(fams, ["Blade", "Gondola"]);
    let at: f64 = m
        .sql()
        .query_row("SELECT max(x) FROM comp", [], |r| r.get(0))
        .unwrap();
    assert!((at - 6.0).abs() < 1e-9, "x = {at}");
}

#[test]
fn a_role_must_be_a_plain_word() {
    let (mut m, l) = model();
    let e = env(vec![role_solid(0, &l, "../x", 0.0)]);
    assert!(m.apply_envelope_step(&e).is_err());
    let e = env(vec![role_solid(0, &l, "", 0.0)]);
    assert!(m.apply_envelope_step(&e).is_err());
}

fn solid_ids(m: &Model) -> Vec<Ref> {
    m.sql()
        .prepare("SELECT id FROM solid ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .map(|id| Ref::id(Cat::Solid, id.unwrap()))
        .collect()
}

#[test]
fn deleting_tagged_solids_deletes_their_component_rows_and_undo_brings_both_back() {
    let (mut m, l) = model();
    let blades = |slot0: u32| -> Vec<Op> {
        (0..3).map(|i| role_solid(slot0 + i, &l, "blade", 3.0 * i as f64)).collect()
    };
    m.apply_envelope_step(&env(blades(0))).unwrap();
    assert_eq!((count(&m, "solid"), count(&m, "comp")), (3, 3));

    // a rerun twice: delete what the recipe made, make it again
    for _ in 0..2 {
        let mut ops = vec![Op::Delete { target: Targets::Refs { refs: solid_ids(&m) } }];
        ops.extend(blades(0));
        m.apply_envelope_step(&env(ops)).unwrap();
    }
    assert_eq!((count(&m, "solid"), count(&m, "comp")), (3, 3));

    m.apply_envelope_step(&env(vec![Op::Delete { target: Targets::Refs { refs: solid_ids(&m) } }]))
        .unwrap();
    assert_eq!((count(&m, "solid"), count(&m, "comp")), (0, 0));
    m.undo().unwrap();
    assert_eq!((count(&m, "solid"), count(&m, "comp")), (3, 3));
    assert_eq!(count(&m, "solid_comp"), 3);
}

fn comp_ids(m: &Model) -> Vec<i64> {
    m.sql()
        .prepare("SELECT id FROM comp ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn deleting_the_component_drops_its_link_so_a_reused_id_is_safe() {
    let (mut m, l) = model();
    m.apply_envelope_step(&env(vec![role_solid(0, &l, "blade", 0.0)])).unwrap();
    let comp = comp_ids(&m)[0];
    m.delete_component(comp).unwrap();
    assert_eq!(count(&m, "solid_comp"), 0);
    // the freed id is taken by an unrelated component, then the solid goes
    m.apply_envelope_step(&env(vec![role_solid(1, &l, "gondola", 3.0)])).unwrap();
    let ids = comp_ids(&m);
    assert_eq!(ids.len(), 1);
    let first = solid_ids(&m)[0].clone();
    m.apply_envelope_step(&env(vec![Op::Delete { target: Targets::Refs { refs: vec![first] } }]))
        .unwrap();
    // the first solid had no link left: only its own row would go, and it has none
    assert_eq!(count(&m, "comp"), 1, "someone else's component survived");
    // undo of the component delete brings the link back with the row
    let (mut m, l) = model();
    m.apply_envelope_step(&env(vec![role_solid(0, &l, "blade", 0.0)])).unwrap();
    m.delete_component(comp_ids(&m)[0]).unwrap();
    m.undo().unwrap();
    assert_eq!((count(&m, "comp"), count(&m, "solid_comp")), (1, 1));
}

#[test]
fn converting_a_tagged_solid_to_a_mesh_takes_its_component_row_and_undo_returns_it() {
    let (mut m, l) = model();
    m.apply_envelope_step(&env(vec![role_solid(0, &l, "blade", 0.0)])).unwrap();
    let Ref::Id { id, .. } = solid_ids(&m)[0].clone() else { unreachable!() };
    m.convert_kind(&ddc_model::ConvertRequest::new("solid", id, ddc_model::ConvertTo::Mesh)).unwrap();
    assert_eq!((count(&m, "comp"), count(&m, "solid_comp")), (0, 0));
    m.undo().unwrap();
    assert_eq!((count(&m, "solid"), count(&m, "comp"), count(&m, "solid_comp")), (1, 1, 1));
}
