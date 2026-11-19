//! Upgrades of older wire versions, applied to the JSON tree before the strict parse, so an
//! old envelope reads as the current version with the meaning it had when it was written.

use serde_json::Value as J;

use crate::OP_VERSION;

/// Oldest wire version this build still reads.
pub const OLDEST: u32 = 1;

/// Brings `env` from version `from` up to `OP_VERSION` in place. False when `from` is
/// outside `OLDEST..=OP_VERSION` (the tree is then left as it was).
pub fn upgrade(env: &mut J, from: u64) -> bool {
    if from < u64::from(OLDEST) || from > u64::from(OP_VERSION) {
        return false;
    }
    if from == 1 {
        v1_to_v2(env);
    }
    true
}

/// v1 had one `type` cat for every type table, told apart only by position, so each ref is
/// retagged by where it sits: wall `type`, type ops -> `wtype`; `set_type.to` by the target kind (wall or none `wtype`, slab `stype`, column/beam `mtype`, other or mixed kinds stay `type`); slab `type`
/// -> `stype`; column/beam `type` and `Profile::Stored.of` -> `mtype`. Any other `type` ref
/// (door, window, ...) stays bare and is refused as before. Filter targets keep their `cat`.
pub fn v1_to_v2(env: &mut J) {
    retag_node(env);
    if let Some(o) = env.as_object_mut() {
        o.insert("v".into(), J::from(2u32));
    }
}

fn retag_ref(r: Option<&mut J>, to: &str) {
    if let Some(J::Object(o)) = r {
        let is_ref = matches!(o.get("by").and_then(J::as_str), Some("id" | "name"));
        if is_ref && o.get("cat").and_then(J::as_str) == Some("type") {
            o.insert("cat".into(), J::from(to));
        }
    }
}

/// Type table of a v1 `set_type.to`, from the kind of element it applies to. None keeps `type`
/// (doors, windows, other cats, mixed refs), which is refused as before. A ref without `cat`
/// (new slot, set, part) cannot be classified and gets `wtype`, as before the split.
fn set_type_tag(target: Option<&J>) -> Option<&'static str> {
    let cat_tag = |c: Option<&str>| match c {
        None | Some("wall") => Some("wtype"),
        Some("slab") => Some("stype"),
        Some("column" | "beam") => Some("mtype"),
        _ => None,
    };
    let t = target?.as_object()?;
    match t.get("by").and_then(J::as_str) {
        Some("refs") => {
            let mut tag = None;
            for (i, r) in t.get("refs").and_then(J::as_array)?.iter().enumerate() {
                let one = cat_tag(r.get("cat").and_then(J::as_str))?;
                if i > 0 && tag != Some(one) {
                    return None;
                }
                tag = Some(one);
            }
            Some(tag.unwrap_or("wtype"))
        }
        _ => cat_tag(t.get("cat").and_then(J::as_str)),
    }
}

fn retag_node(node: &mut J) {
    match node {
        J::Object(o) => {
            match o.get("op").and_then(J::as_str) {
                Some("type") => {
                    for k in ["from", "target", "replace_with"] {
                        retag_ref(o.get_mut(k), "wtype");
                    }
                }
                Some("set_type") => {
                    let to = set_type_tag(o.get("target"));
                    if let Some(to) = to {
                        retag_ref(o.get_mut("to"), to);
                    }
                }
                Some("create") => {
                    if let Some(J::Object(el)) = o.get_mut("el") {
                        let to = match el.get("kind").and_then(J::as_str) {
                            Some("wall") => Some("wtype"),
                            Some("slab") => Some("stype"),
                            Some("column" | "beam") => Some("mtype"),
                            _ => None,
                        };
                        if let Some(to) = to {
                            retag_ref(el.get_mut("type"), to);
                        }
                    }
                }
                _ => {}
            }
            if o.get("shape").and_then(J::as_str) == Some("stored") {
                retag_ref(o.get_mut("of"), "mtype");
            }
            for v in o.values_mut() {
                retag_node(v);
            }
        }
        J::Array(a) => a.iter_mut().for_each(retag_node),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cat, Envelope, Op, Ref, Targets};
    use serde_json::json;

    #[test]
    fn v1_type_ref_upgrades_to_wall_type() {
        let v1 = json!({
            "v": 1,
            "actor": { "kind": "user" },
            "title": "t",
            "why": null,
            "groups": [{ "id": 1, "title": "g", "depends_on": [], "ops": [
                { "op": "set_type",
                  "target": { "by": "filter", "cat": "wall", "query": "" },
                  "to": { "by": "id", "cat": "type", "id": 7 } },
                { "op": "set_type",
                  "target": { "by": "refs", "refs": [{ "by": "id", "cat": "wall", "id": 1 }] },
                  "to": { "by": "name", "cat": "type", "name": "Wall 200" } }
            ]}]
        });
        let env = Envelope::from_value(v1.clone()).unwrap();
        assert_eq!(env.v, OP_VERSION);
        assert!(env.check().is_empty(), "{:?}", env.check());
        let ops: Vec<&Op> = env.ops().collect();
        match ops[0] {
            Op::SetType { target: Targets::Filter { cat, .. }, to } => {
                assert_eq!(*cat, Some(Cat::Wall));
                assert_eq!(*to, Ref::id(Cat::WallType, 7));
                assert_eq!(to.label(), "wtype:7");
            }
            other => panic!("{other:?}"),
        }
        match ops[1] {
            Op::SetType { to, .. } => assert_eq!(*to, Ref::name(Cat::WallType, "Wall 200")),
            other => panic!("{other:?}"),
        }
        // The text path upgrades the same way; v2 keeps `type:` as written.
        assert_eq!(Envelope::from_json(&v1.to_string()).unwrap(), env);
        let mut v2 = v1;
        v2["v"] = json!(2);
        let kept = Envelope::from_value(v2).unwrap();
        assert!(kept.ops().any(|op| matches!(op, Op::SetType { to, .. } if *to == Ref::id(Cat::Type, 7))));
    }

    fn v1_set_type_to_tag(target_ref: serde_json::Value) -> Ref {
        let v1 = json!({
            "v": 1,
            "actor": { "kind": "user" },
            "title": "t",
            "why": null,
            "groups": [{ "id": 1, "title": "g", "depends_on": [], "ops": [
                { "op": "set_type",
                  "target": { "by": "refs", "refs": [target_ref] },
                  "to": { "by": "id", "cat": "type", "id": 7 } }
            ]}]
        });
        let env = Envelope::from_value(v1).unwrap();
        let to = match env.ops().next().unwrap() {
            Op::SetType { to, .. } => to.clone(),
            other => panic!("{other:?}"),
        };
        to
    }

    #[test]
    fn v1_set_type_slot_ref_without_cat_upgrades_to_wtype() {
        assert_eq!(v1_set_type_to_tag(json!({ "by": "new", "slot": 1 })), Ref::id(Cat::WallType, 7));
    }

    #[test]
    fn v1_set_type_set_ref_without_cat_upgrades_to_wtype() {
        assert_eq!(v1_set_type_to_tag(json!({ "by": "set", "name": "north" })), Ref::id(Cat::WallType, 7));
    }

    #[test]
    fn v1_set_type_part_ref_without_cat_upgrades_to_wtype() {
        let part = json!({ "by": "part", "of": { "by": "new", "slot": 1 }, "part": "face:left" });
        assert_eq!(v1_set_type_to_tag(part), Ref::id(Cat::WallType, 7));
    }
}
