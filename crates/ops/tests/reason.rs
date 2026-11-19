//! Rejection contract: stable codes, field paths, expected/got with units, locale coverage.

use ddc_ops::reason::num;
use ddc_ops::sample;
use ddc_ops::*;
use serde_json::{json, Value as Json};

const EN: &str = include_str!("../../ui/data/i18n/en/ops_reason.json");
const RU: &str = include_str!("../../ui/data/i18n/ru/ops_reason.json");

#[test]
fn every_code_has_en_and_ru_text() {
    for (lang, src) in [("en", EN), ("ru", RU)] {
        let cat: Json = serde_json::from_str(src).unwrap();
        let obj = cat.as_object().unwrap();
        for c in Code::ALL {
            let t = obj.get(&c.hint_key()).and_then(Json::as_str);
            assert!(t.is_some_and(|t| !t.trim().is_empty()), "{lang}: no text for {}", c.key());
        }
        // no stale keys
        for k in obj.keys() {
            let code = k.strip_prefix("ops.reason.").and_then(Code::from_key);
            assert!(code.is_some(), "{lang}: unknown key {k}");
        }
    }
}

#[test]
fn codes_are_stable_and_match_serde() {
    let mut seen = std::collections::BTreeSet::new();
    for c in Code::ALL {
        assert_eq!(serde_json::to_value(c).unwrap(), json!(c.key()));
        assert_eq!(Code::from_key(c.key()), Some(c));
        assert!(seen.insert(c.key()), "duplicate {}", c.key());
    }
    assert_eq!(Code::OutOfRange.key(), "out_of_range");
    assert_eq!(Code::OutOfRange.hint_key(), "ops.reason.out_of_range");
}

#[test]
fn rejection_serde_round_trip() {
    let full = Rejection::out_of_range("/el/height", 0.1, 30.0, "m", 45.0)
        .in_op(&sample::envelope(), 0, 3)
        .with_ref(Ref::id(Cat::Wall, 55))
        .with_ref(Ref::part(Ref::slot(2), "face:left"));
    let bare = Rejection::new(Code::NoOps).field("/groups");
    for r in [full, bare] {
        let s = serde_json::to_string(&r).unwrap();
        let back: Rejection = serde_json::from_str(&s).unwrap();
        assert_eq!(back, r);
    }
    // unknown fields are refused, like the rest of the wire
    let bad = json!({"path": "", "code": "parse", "hint_key": "ops.reason.parse", "extra": 1});
    assert!(serde_json::from_value::<Rejection>(bad).is_err());
    // unknown codes are refused: the set is closed
    let bad = json!({"path": "", "code": "nope", "hint_key": "ops.reason.nope"});
    assert!(serde_json::from_value::<Rejection>(bad).is_err());
}

#[test]
fn rejection_path_points_to_field() {
    let env = sample::envelope();
    let gid = env.groups[0].id;
    let r = Rejection::out_of_range("/height", 0.1, 30.0, "m", 45.0).under("/el").in_op(&env, 0, 3);
    assert_eq!(r.path, "/groups/0/ops/3/el/height");
    assert_eq!((r.group, r.op), (Some(gid), Some(3)));

    let mut env = sample::envelope();
    env.groups[1].ops.insert(0, Op::Delete { target: Targets::one(Ref::slot(99)) });
    let rs = env.rejections();
    let r = rs.iter().find(|r| r.code == Code::UnknownSlot).expect("unknown slot reported");
    assert_eq!(r.path, "/groups/1/ops/0");
    assert_eq!((r.group, r.op), (Some(env.groups[1].id), Some(0)));
    assert_eq!(r.hint_key, "ops.reason.unknown_slot");

    let empty = Envelope::new(Actor::User, "empty").rejections();
    assert_eq!(empty.len(), 1);
    assert_eq!((empty[0].code, empty[0].path.as_str(), empty[0].group), (Code::NoOps, "/groups", None));
    assert!(sample::envelope().rejections().is_empty());
}

#[test]
fn out_of_range_states_expected_and_unit() {
    let r = Rejection::out_of_range("/el/height", 0.1, 30.0, "m", 45.0);
    assert_eq!(r.code, Code::OutOfRange);
    assert_eq!(r.expected.as_deref(), Some("0.1..30 m"));
    assert_eq!(r.got.as_deref(), Some("45 m"));
    assert_eq!(r.to_string(), "out_of_range at /el/height: expected 0.1..30 m, got 45 m");
    let r = Rejection::at_least("/sill", 0.0, "m", -0.25);
    assert_eq!((r.expected.as_deref(), r.got.as_deref()), (Some(">= 0 m"), Some("-0.25 m")));
    assert_eq!((num(2.10004), num(-0.0001), num(1200.0)), ("2.1".into(), "0".into(), "1200".into()));
}

#[test]
fn wire_errors_map_to_codes() {
    let e = Envelope::from_json(r#"{"v": 7}"#).unwrap_err();
    let r = Rejection::from_wire(&e);
    assert_eq!((r.code, r.path.as_str()), (Code::Version, "/v"));
    assert_eq!((r.expected.as_deref(), r.got.as_deref()), (Some(OP_VERSION.to_string().as_str()), Some("7")));
    let e = Envelope::from_json("{").unwrap_err();
    assert_eq!(Rejection::from_wire(&e).code, Code::Parse);
}
