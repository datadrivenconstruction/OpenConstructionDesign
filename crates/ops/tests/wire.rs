//! Wire contract of `ddc-ops`: round trip, strictness, version and upgrade, explicit units,
//! golden files (v1 kept as written, read through the upgrade; v2 current).

use std::collections::BTreeSet;

use ddc_ops::sample;
use ddc_ops::*;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value as Json};

const GOLDEN_V1: &str = include_str!("golden/envelope_v1.json");
const GOLDEN: &str = include_str!("golden/envelope_v2.json");

fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(x: &T) {
    let s = serde_json::to_string(x).unwrap();
    let back: T = serde_json::from_str(&s).unwrap_or_else(|e| panic!("{e}: {s}"));
    assert_eq!(&back, x, "{s}");
    // Serialisation is stable: the same value gives the same bytes again.
    assert_eq!(serde_json::to_string(&back).unwrap(), s);
}

#[test]
fn envelope_json_roundtrip_all_variants() {
    let env = sample::envelope();
    assert!(env.check().is_empty(), "{:?}", env.check());

    let s = env.to_json().unwrap();
    assert_eq!(Envelope::from_json(&s).unwrap(), env);
    let pretty = env.to_json_pretty().unwrap();
    assert_eq!(Envelope::from_json(&pretty).unwrap(), env);
    assert_eq!(Envelope::from_value(serde_json::from_str(&s).unwrap()).unwrap(), env);

    // Every op variant on its own, and coverage of every kind.
    let mut kinds = BTreeSet::new();
    for op in sample::all_ops() {
        roundtrip(&op);
        kinds.insert(op.kind());
    }
    let all: BTreeSet<&str> = Op::KINDS.iter().copied().collect();
    assert_eq!(kinds, all, "samples must cover every op kind");

    // Every NewEl family appears.
    let fams: BTreeSet<String> = sample::all_ops()
        .iter()
        .filter_map(|op| match op {
            Op::Create { el, .. } => Some(serde_json::to_value(el).unwrap()["kind"].as_str().unwrap().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(fams.len(), 15, "{fams:?}");

    for x in sample::all_refs() {
        roundtrip(&x);
    }
    for x in sample::all_targets() {
        roundtrip(&x);
    }
    for x in sample::all_actors() {
        roundtrip(&x);
    }
    for x in sample::all_places() {
        roundtrip(&x);
    }
    for x in sample::all_profiles() {
        roundtrip(&x);
    }
    for x in sample::all_forms() {
        roundtrip(&x);
    }
    for x in sample::all_patterns() {
        roundtrip(&x);
    }
    for x in sample::all_xforms() {
        roundtrip(&x);
    }
    for u in Unit::ALL {
        roundtrip(&Value::num(1.0, u));
    }
}

#[test]
fn wire_names_are_stable() {
    let op = Op::Type(TypeOp::Rename { target: Ref::id(Cat::CurtainWall, 7), name: "CW".into() });
    assert_eq!(
        serde_json::to_value(&op).unwrap(),
        json!({"op": "type", "action": "rename",
               "target": {"by": "id", "cat": "curtain_wall", "id": 7}, "name": "CW"})
    );
    for c in Cat::ALL {
        assert_eq!(serde_json::to_value(c).unwrap(), json!(c.key()));
        assert_eq!(Cat::from_key(c.key()), Some(c));
    }
    assert_eq!(Ref::part(Ref::id(Cat::Wall, 55), "face:left").label(), "wall:55/face:left");
    assert_eq!(serde_json::to_value(Unit::KgPerM3).unwrap(), json!("kg/m3"));
}

#[test]
fn unknown_field_rejected() {
    let base: Json = serde_json::from_str(&sample::envelope().to_json().unwrap()).unwrap();
    assert!(Envelope::from_value(base.clone()).is_ok());

    // Top level.
    let mut top = base.clone();
    top["colour"] = json!("red");
    assert!(matches!(Envelope::from_value(top), Err(WireError::Json(_))));

    // Inside a group.
    let mut grp = base.clone();
    grp["groups"][0]["priority"] = json!(1);
    assert!(Envelope::from_value(grp).is_err());

    // Inside an op, inside a nested element, inside a ref, inside a quantity.
    let cases: [&[&str]; 4] = [
        &["groups", "1", "ops", "0"],
        &["groups", "1", "ops", "0", "el"],
        &["groups", "1", "ops", "0", "el", "level"],
        &["groups", "1", "ops", "0", "el", "height"],
    ];
    for path in cases {
        let mut j = base.clone();
        let mut node = &mut j;
        for k in path {
            node = match k.parse::<usize>() {
                Ok(i) => &mut node[i],
                Err(_) => &mut node[*k],
            };
        }
        assert!(node.is_object(), "{path:?}");
        node["extra"] = json!(0);
        let s = j.to_string();
        let err = Envelope::from_json(&s).unwrap_err().to_string();
        assert!(err.contains("extra"), "{path:?}: {err}");
    }

    // Misspelt op tag and unknown unit.
    assert!(serde_json::from_value::<Op>(json!({"op": "creat", "slot": 1})).is_err());
    assert!(serde_json::from_value::<Len>(json!({"v": 1.0, "unit": "yd"})).is_err());
}

#[test]
fn version_mismatch_rejected() {
    let mut j: Json = serde_json::from_str(&sample::envelope().to_json().unwrap()).unwrap();
    let next = u64::from(OP_VERSION) + 1;
    j["v"] = json!(next);
    match Envelope::from_json(&j.to_string()) {
        Err(WireError::Version { got, want }) => assert_eq!((got, want), (next, OP_VERSION)),
        other => panic!("{other:?}"),
    }
    assert!(matches!(Envelope::from_value(j.clone()), Err(WireError::Version { got, .. }) if got == next));

    // A future version with new fields reports the version, not the unknown field.
    j["new_thing"] = json!(true);
    assert!(matches!(Envelope::from_json(&j.to_string()), Err(WireError::Version { .. })));

    j["v"] = json!(0);
    assert!(matches!(Envelope::from_json(&j.to_string()), Err(WireError::Version { got: 0, .. })));

    let obj = j.as_object_mut().unwrap();
    obj.remove("v");
    assert!(matches!(Envelope::from_json(&j.to_string()), Err(WireError::NoVersion)));

    // A struct built by hand with a wrong version fails the structural check.
    let mut env = sample::envelope();
    env.v = OP_VERSION + 1;
    assert!(env.check().iter().any(|i| i.code == "version"));
}

/// Walk JSON; every quantity (`{"v": scalar}`) and every point (numeric `x`, `y`) must carry
/// a `unit` key. Unitless directions sit under the keys `dir`, `axis`, `x`, `z`.
fn assert_units(j: &Json, at: &str, key: &str, seen: &mut usize) {
    match j {
        Json::Object(m) => {
            let scalar_v = m.get("v").map_or(false, |v| !v.is_object() && !v.is_array());
            if scalar_v && !at.is_empty() {
                assert!(m.contains_key("unit"), "value without unit at {at}");
                *seen += 1;
            }
            let point = m.get("x").map_or(false, Json::is_number) && m.get("y").map_or(false, Json::is_number);
            if point && !matches!(key, "dir" | "axis" | "x" | "z") {
                assert!(m.contains_key("unit"), "point without unit at {at}");
                *seen += 1;
            }
            for (k, v) in m {
                assert_units(v, &format!("{at}/{k}"), k, seen);
            }
        }
        Json::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                assert_units(v, &format!("{at}/{i}"), key, seen);
            }
        }
        _ => {}
    }
}

#[test]
fn units_explicit_in_wire() {
    let j = serde_json::to_value(sample::envelope()).unwrap();
    let mut seen = 0;
    assert_units(&j, "", "", &mut seen);
    assert!(seen > 50, "only {seen} quantities");

    // A dimensionless value still writes `"unit": null`; omitting the key is an error.
    assert_eq!(serde_json::to_value(Value::text("EI30")).unwrap(), json!({"v": "EI30", "unit": null}));
    assert!(serde_json::from_value::<Value>(json!({"v": 3.0})).is_err());
    assert_eq!(serde_json::from_value::<Value>(json!({"v": 3, "unit": null})).unwrap(), Value::plain(json!(3)));

    // Lengths, angles and points without a unit are rejected.
    assert!(serde_json::from_value::<Len>(json!({"v": 3.0})).is_err());
    assert!(serde_json::from_value::<Angle>(json!({"v": 90.0})).is_err());
    assert!(serde_json::from_value::<P3>(json!({"x": 1.0, "y": 2.0, "z": 0.0})).is_err());
    assert!(serde_json::from_value::<P2>(json!({"x": 1.0, "y": 2.0})).is_err());
    assert!(serde_json::from_value::<V3>(json!({"x": 1.0, "y": 0.0, "z": 0.0})).is_err());
    // A bare number where a length is expected is rejected too.
    assert!(serde_json::from_value::<Len>(json!(3.0)).is_err());
    // Param definitions must state their unit (null for text).
    assert!(serde_json::from_value::<ParamOp>(json!({
        "action": "define", "name": "N", "kind": "text", "cats": [], "instance": true, "group": null
    }))
    .is_err());

    // Conversions to SI.
    assert!((Len::mm(2500.0).to_m() - 2.5).abs() < 1e-12);
    assert!((Len { v: 10.0, unit: LenUnit::Ft }.to_m() - 3.048).abs() < 1e-12);
    assert!((Angle::deg(180.0).to_rad() - std::f64::consts::PI).abs() < 1e-12);
    assert_eq!(Value::num(250.0, Unit::Mm).si(), Some(0.25));
    assert_eq!(Value::plain(json!(4)).si(), Some(4.0));
    for u in Unit::ALL {
        assert!(u.si_factor() > 0.0);
    }
    assert_eq!(Unit::Ft.dim(), Dim::Length);
}

#[test]
fn golden_v1_reads_as_v2() {
    let env = Envelope::from_json(GOLDEN_V1).unwrap();
    assert_eq!(env.v, OP_VERSION);
    assert!(env.check().is_empty(), "{:?}", env.check());
    let golden: Json = serde_json::from_str(GOLDEN).unwrap();
    assert_eq!(serde_json::to_value(&env).unwrap(), golden);
}

#[test]
fn golden_v2_is_stable() {
    let env = Envelope::from_json(GOLDEN).unwrap();
    assert!(env.check().is_empty(), "{:?}", env.check());
    // The file states every field: re-serialising gives the same JSON tree.
    let golden: Json = serde_json::from_str(GOLDEN).unwrap();
    assert_eq!(serde_json::to_value(&env).unwrap(), golden);
    let pretty = env.to_json_pretty().unwrap();
    assert_eq!(Envelope::from_json(&pretty).unwrap().to_json_pretty().unwrap(), pretty);
}

#[test]
fn check_reports_structure() {
    let mut env = sample::envelope();
    // Group 3 uses slot 23 of group 2; dropping the dependency must be reported.
    env.groups[2].depends_on.clear();
    assert!(env.check().iter().any(|i| i.code == "undeclared_dependency" && i.path.starts_with("/groups/2/")));

    let mut env = sample::envelope();
    env.groups[1].depends_on.push(9);
    assert!(env.check().iter().any(|i| i.code == "bad_dependency"));

    let mut env = sample::envelope();
    let dup = env.groups[0].ops[0].clone();
    env.groups[0].ops.push(dup);
    assert!(env.check().iter().any(|i| i.code == "duplicate_slot"));

    let mut env = sample::envelope();
    env.groups[0].ops.insert(0, Op::Delete { target: Targets::one(Ref::slot(99)) });
    assert!(env.check().iter().any(|i| i.code == "unknown_slot" && i.path == "/groups/0/ops/0"));

    let env = Envelope::new(Actor::User, "empty");
    assert_eq!(env.check(), vec![Issue { path: "/groups".into(), code: "no_ops" }]);
}

fn v1_env(ops: Json) -> Json {
    json!({
        "v": 1, "actor": { "kind": "user" }, "title": "t", "why": null,
        "groups": [{ "id": 1, "title": "g", "depends_on": [], "ops": ops }]
    })
}

fn first_create_type(env: &Envelope) -> Ref {
    match env.ops().next().unwrap() {
        Op::Create { el, .. } => match el {
            NewEl::Slab { ty, .. } | NewEl::Column { ty, .. } | NewEl::Door { ty, .. } => ty.clone().unwrap(),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

#[test]
fn v1_member_ty_upgrades_to_mtype() {
    let t = json!({ "by": "name", "cat": "type", "name": "C 400x400" });
    let v1 = v1_env(json!([{ "op": "create", "slot": 1, "el": {
        "kind": "column",
        "level": { "by": "id", "cat": "level", "id": 1 },
        "at": { "kind": "at", "p": { "x": 0.0, "y": 0.0, "z": 0.0, "unit": "m" } },
        "top": null, "height": { "v": 3.0, "unit": "m" },
        "rotation": { "v": 0.0, "unit": "deg" }, "type": t, "fields": [] } }]));
    let env = Envelope::from_value(v1).unwrap();
    assert_eq!(first_create_type(&env), Ref::name(Cat::MemberType, "C 400x400"));
}

#[test]
fn v1_slab_ty_to_stype() {
    let v1 = v1_env(json!([{ "op": "create", "slot": 1, "el": {
        "kind": "slab",
        "level": { "by": "id", "cat": "level", "id": 1 },
        "boundary": [], "offset": { "v": 0.0, "unit": "m" },
        "type": { "by": "id", "cat": "type", "id": 5 }, "fields": [] } }]));
    let env = Envelope::from_value(v1).unwrap();
    assert_eq!(first_create_type(&env), Ref::id(Cat::SlabType, 5));
}

#[test]
fn v1_door_type_left_bare() {
    let v1 = v1_env(json!([{ "op": "create", "slot": 1, "el": {
        "kind": "door",
        "host": { "by": "id", "cat": "wall", "id": 1 },
        "at": { "kind": "at", "p": { "x": 0.0, "y": 0.0, "z": 0.0, "unit": "m" } },
        "type": { "by": "name", "cat": "type", "name": "Door 900x2100" }, "fields": [] } }]));
    let env = Envelope::from_value(v1).unwrap();
    assert_eq!(first_create_type(&env), Ref::name(Cat::Type, "Door 900x2100"));
}

#[test]
fn v1_stored_profile_of_upgrades_to_mtype() {
    let mut v1 = json!({ "shape": "stored", "of": { "by": "name", "cat": "type", "name": "HEA 200" } });
    let mut v1 = v1_env(json!([{ "op": "x", "profile": v1.take() }]));
    ddc_ops::upgrade::v1_to_v2(&mut v1);
    assert_eq!(v1["groups"][0]["ops"][0]["profile"]["of"]["cat"], "mtype");
}

fn first_set_type_to(env: &Envelope) -> Ref {
    match env.ops().next().unwrap() {
        Op::SetType { to, .. } => to.clone(),
        other => panic!("{other:?}"),
    }
}

fn v1_set_type(target: serde_json::Value) -> Envelope {
    let v1 = v1_env(json!([{ "op": "set_type", "target": target,
        "to": { "by": "name", "cat": "type", "name": "T 1" } }]));
    Envelope::from_value(v1).unwrap()
}

#[test]
fn v1_set_type_door_filter_keeps_type() {
    let env = v1_set_type(json!({ "by": "filter", "cat": "door", "query": "" }));
    assert_eq!(first_set_type_to(&env), Ref::name(Cat::Type, "T 1"));
}

#[test]
fn v1_set_type_slab_filter_to_stype() {
    let env = v1_set_type(json!({ "by": "filter", "cat": "slab", "query": "" }));
    assert_eq!(first_set_type_to(&env), Ref::name(Cat::SlabType, "T 1"));
}

#[test]
fn v1_set_type_refs_column_to_mtype() {
    let col = json!({ "by": "id", "cat": "column", "id": 1 });
    let env = v1_set_type(json!({ "by": "refs", "refs": [col.clone()] }));
    assert_eq!(first_set_type_to(&env), Ref::name(Cat::MemberType, "T 1"));
    let door = json!({ "by": "id", "cat": "door", "id": 2 });
    let env = v1_set_type(json!({ "by": "refs", "refs": [col, door] }));
    assert_eq!(first_set_type_to(&env), Ref::name(Cat::Type, "T 1"));
}
