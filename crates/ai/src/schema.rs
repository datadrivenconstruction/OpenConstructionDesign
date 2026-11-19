// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Small schema builder: one definition yields both the JSON schema shown to the model
//! and the validator run on its arguments.

use serde_json::{json, Map, Value as J};
use std::borrow::Cow;

/// Defaults for values the model fills in: nothing honest comes near them, and bulk
/// edits go through frozen sets rather than long lists.
const MAX_STR_CHARS: usize = 256 * 1024;
const MAX_ITEMS: usize = 10_000;
const MAX_MAP_KEYS: usize = 1000;
const MAX_FIELD_NAME_SHOWN: usize = 64;

fn check_str(v: &J, path: &str, max: usize) -> Result<(), String> {
    let Some(s) = v.as_str() else {
        return Err(format!("{path}: expected string"));
    };
    if s.len() > max && s.chars().count() > max {
        return Err(format!("{path}: at most {max} characters"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub enum Schema {
    Any,
    Str,
    StrMax(usize),
    Num,
    Int {
        min: Option<i64>,
        max: Option<i64>,
    },
    Bool,
    Enum(Vec<&'static str>),
    Arr {
        item: Box<Schema>,
        max: Option<usize>,
    },
    Obj(Vec<(&'static str, Schema)>),
    /// A free-form object: field names are only known at run time.
    Map,
    OneOf(Vec<Schema>),
    Opt(Box<Schema>),
    Desc(Cow<'static, str>, Box<Schema>),
    /// Shown to the model in full but checked only as an object: the tool's own reader
    /// lists every problem at once, with suggestions, where this check stops at the first.
    Shown(Box<Schema>),
}

pub fn shown(inner: Schema) -> Schema {
    Schema::Shown(Box::new(inner))
}

pub fn any() -> Schema {
    Schema::Any
}

pub fn str_() -> Schema {
    Schema::Str
}

pub fn str_max(max_chars: usize) -> Schema {
    Schema::StrMax(max_chars)
}

pub fn num() -> Schema {
    Schema::Num
}

pub fn int() -> Schema {
    Schema::Int {
        min: None,
        max: None,
    }
}

pub fn int_range(min: i64, max: i64) -> Schema {
    Schema::Int {
        min: Some(min),
        max: Some(max),
    }
}

pub fn bool_() -> Schema {
    Schema::Bool
}

pub fn enum_(values: &[&'static str]) -> Schema {
    Schema::Enum(values.to_vec())
}

pub fn arr(item: Schema) -> Schema {
    Schema::Arr {
        item: Box::new(item),
        max: None,
    }
}

pub fn arr_max(item: Schema, max: usize) -> Schema {
    Schema::Arr {
        item: Box::new(item),
        max: Some(max),
    }
}

/// Closed: an unknown field is an error, which is how a model learns it misspelled one.
pub fn obj(fields: Vec<(&'static str, Schema)>) -> Schema {
    Schema::Obj(fields)
}

pub fn map() -> Schema {
    Schema::Map
}

pub fn one_of(options: Vec<Schema>) -> Schema {
    Schema::OneOf(options)
}

pub fn opt(inner: Schema) -> Schema {
    Schema::Opt(Box::new(inner))
}

/// Text may be built at run time, e.g. from the ranges the reader checks.
pub fn desc(text: impl Into<Cow<'static, str>>, inner: Schema) -> Schema {
    Schema::Desc(text.into(), Box::new(inner))
}

impl Schema {
    fn is_optional(&self) -> bool {
        match self {
            Schema::Opt(_) => true,
            Schema::Desc(_, inner) => inner.is_optional(),
            _ => false,
        }
    }

    pub fn to_json(&self) -> J {
        match self {
            Schema::Any => json!({}),
            Schema::Str => json!({ "type": "string" }),
            Schema::StrMax(max) => json!({ "type": "string", "maxLength": max }),
            Schema::Num => json!({ "type": "number" }),
            Schema::Int { min, max } => {
                let mut v = json!({ "type": "integer" });
                if let Some(m) = min {
                    v["minimum"] = json!(m);
                }
                if let Some(m) = max {
                    v["maximum"] = json!(m);
                }
                v
            }
            Schema::Bool => json!({ "type": "boolean" }),
            Schema::Enum(values) => json!({ "type": "string", "enum": values }),
            Schema::Arr { item, max } => {
                let mut v = json!({ "type": "array", "items": item.to_json() });
                if let Some(m) = max {
                    v["maxItems"] = json!(m);
                }
                v
            }
            Schema::Obj(fields) => {
                let mut props = Map::new();
                let mut required = Vec::new();
                for (name, s) in fields {
                    props.insert((*name).to_string(), s.to_json());
                    if !s.is_optional() {
                        required.push(*name);
                    }
                }
                json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
            }
            Schema::Map => json!({ "type": "object" }),
            Schema::OneOf(options) => {
                json!({ "oneOf": options.iter().map(Schema::to_json).collect::<Vec<J>>() })
            }
            Schema::Opt(inner) | Schema::Shown(inner) => inner.to_json(),
            Schema::Desc(text, inner) => {
                let mut v = inner.to_json();
                v["description"] = json!(text);
                v
            }
        }
    }

    /// The error text goes back to the model, so it names the exact field.
    pub fn validate(&self, v: &J) -> Result<(), String> {
        self.check(v, "args")
    }

    fn check(&self, v: &J, path: &str) -> Result<(), String> {
        let mismatch = |want: &str| Err(format!("{path}: expected {want}"));
        match self {
            Schema::Any => Ok(()),
            Schema::Str => check_str(v, path, MAX_STR_CHARS),
            Schema::StrMax(max) => check_str(v, path, *max),
            Schema::Num => match v.as_f64() {
                Some(n) if n.is_finite() => Ok(()),
                _ => mismatch("number"),
            },
            Schema::Int { min, max } => {
                let Some(n) = v.as_i64() else {
                    return mismatch("integer");
                };
                if min.map_or(false, |m| n < m) || max.map_or(false, |m| n > m) {
                    return Err(format!("{path}: {n} is out of range"));
                }
                Ok(())
            }
            Schema::Bool => {
                if v.is_boolean() {
                    Ok(())
                } else {
                    mismatch("boolean")
                }
            }
            Schema::Enum(values) => match v.as_str() {
                Some(s) if values.iter().any(|x| *x == s) => Ok(()),
                _ => Err(format!("{path}: expected one of {}", values.join(", "))),
            },
            Schema::Arr { item, max } => {
                let Some(items) = v.as_array() else {
                    return mismatch("array");
                };
                let m = max.unwrap_or(MAX_ITEMS);
                if items.len() > m {
                    return Err(format!("{path}: at most {m} items, got {}", items.len()));
                }
                for (i, x) in items.iter().enumerate() {
                    item.check(x, &format!("{path}[{i}]"))?;
                }
                Ok(())
            }
            Schema::Obj(fields) => {
                let Some(o) = v.as_object() else {
                    return mismatch("object");
                };
                for k in o.keys() {
                    if !fields.iter().any(|(name, _)| name == k) {
                        // the name is the caller's text: bounded, no control characters
                        let shown: String = k
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(MAX_FIELD_NAME_SHOWN)
                            .collect();
                        return Err(format!("{path}: unknown field {shown}"));
                    }
                }
                for (name, s) in fields {
                    let sub = format!("{path}.{name}");
                    match o.get(*name) {
                        // Models often spell an absent optional field as null.
                        None | Some(J::Null) if s.is_optional() => {}
                        None => return Err(format!("{sub}: required")),
                        Some(x) => s.check(x, &sub)?,
                    }
                }
                Ok(())
            }
            Schema::Shown(_) if v.is_object() => Ok(()),
            Schema::Shown(_) => mismatch("object"),
            Schema::Map => match v.as_object() {
                Some(o) if o.len() > MAX_MAP_KEYS => Err(format!(
                    "{path}: at most {MAX_MAP_KEYS} fields, got {}",
                    o.len()
                )),
                Some(_) => Ok(()),
                None => mismatch("object"),
            },
            Schema::OneOf(options) => {
                let results: Vec<Result<(), String>> =
                    options.iter().map(|o| o.check(v, path)).collect();
                match results.iter().filter(|r| r.is_ok()).count() {
                    1 => Ok(()),
                    0 => {
                        let why: Vec<String> =
                            results.into_iter().filter_map(Result::err).collect();
                        Err(format!(
                            "{path}: matches no allowed form ({})",
                            why.join("; ")
                        ))
                    }
                    _ => Err(format!("{path}: matches several forms, use exactly one")),
                }
            }
            Schema::Opt(inner) | Schema::Desc(_, inner) => inner.check(v, path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Schema {
        obj(vec![
            ("name", str_()),
            ("limit", opt(int_range(1, 200))),
            ("mode", enum_(&["a", "b"])),
            ("items", opt(arr_max(num(), 2))),
            ("set", opt(map())),
        ])
    }

    #[test]
    fn accepts_valid_and_null_optional() {
        let s = sample();
        assert_eq!(s.validate(&json!({ "name": "x", "mode": "a" })), Ok(()));
        assert_eq!(s.validate(&json!({ "name": "x", "mode": "b", "limit": 200, "items": [1, 2.5], "set": { "any": 1 } })), Ok(()));
        assert_eq!(
            s.validate(&json!({ "name": "x", "mode": "a", "limit": null })),
            Ok(())
        );
    }

    #[test]
    fn an_unknown_field_name_is_echoed_short_and_clean() {
        let mut m = Map::new();
        m.insert(format!("bad\n{}", "k".repeat(100_000)), json!(1));
        let err = sample().validate(&J::Object(m)).unwrap_err();
        assert!(err.len() < 200, "{} bytes", err.len());
        assert!(!err.contains('\n'));
    }

    #[test]
    fn errors_name_the_field() {
        let s = sample();
        let err = |v: J| s.validate(&v).unwrap_err();
        assert_eq!(err(json!({ "mode": "a" })), "args.name: required");
        assert_eq!(
            err(json!({ "name": null, "mode": "a" })),
            "args.name: expected string"
        );
        assert_eq!(
            err(json!({ "name": 1, "mode": "a" })),
            "args.name: expected string"
        );
        assert_eq!(
            err(json!({ "name": "x", "mode": "z" })),
            "args.mode: expected one of a, b"
        );
        assert_eq!(
            err(json!({ "name": "x", "mode": "a", "limit": 0 })),
            "args.limit: 0 is out of range"
        );
        assert_eq!(
            err(json!({ "name": "x", "mode": "a", "limit": 1.5 })),
            "args.limit: expected integer"
        );
        assert_eq!(
            err(json!({ "name": "x", "mode": "a", "items": [1, 2, 3] })),
            "args.items: at most 2 items, got 3"
        );
        assert_eq!(
            err(json!({ "name": "x", "mode": "a", "items": ["q"] })),
            "args.items[0]: expected number"
        );
        assert_eq!(
            err(json!({ "name": "x", "mode": "a", "extra": 1 })),
            "args: unknown field extra"
        );
        assert_eq!(err(json!("nope")), "args: expected object");
    }

    #[test]
    fn one_of_requires_exactly_one() {
        let s = one_of(vec![
            obj(vec![("a", str_())]),
            obj(vec![("b", str_())]),
            int(),
        ]);
        assert_eq!(s.validate(&json!({ "a": "x" })), Ok(()));
        assert_eq!(s.validate(&json!(5)), Ok(()));
        assert!(s
            .validate(&json!({ "a": "x", "b": "y" }))
            .unwrap_err()
            .contains("no allowed form"));
        let both = one_of(vec![int(), num()]);
        assert!(both.validate(&json!(5)).unwrap_err().contains("several"));
    }

    #[test]
    fn sizes_are_bounded_even_without_an_explicit_limit() {
        assert_eq!(str_max(3).validate(&json!("\u{044f}\u{0433}\u{0434}")), Ok(()));
        assert_eq!(
            str_max(3).validate(&json!("\u{044f}\u{0433}\u{043e}\u{0434}")).unwrap_err(),
            "args: at most 3 characters"
        );
        assert_eq!(str_max(3).to_json()["maxLength"], 3);
        assert!(str_()
            .validate(&json!("x".repeat(MAX_STR_CHARS + 1)))
            .is_err());
        assert!(arr(int())
            .validate(&J::Array(vec![json!(1); MAX_ITEMS + 1]))
            .is_err());
        assert_eq!(
            arr(int()).validate(&J::Array(vec![json!(1); MAX_ITEMS])),
            Ok(())
        );
        let big: Map<String, J> = (0..=MAX_MAP_KEYS)
            .map(|i| (i.to_string(), json!(1)))
            .collect();
        assert!(map().validate(&J::Object(big)).is_err());
    }

    #[test]
    fn json_schema_shape() {
        let v = sample().to_json();
        assert_eq!(v["type"], "object");
        assert_eq!(v["additionalProperties"], false);
        assert_eq!(v["required"], json!(["name", "mode"]));
        assert_eq!(v["properties"]["limit"]["maximum"], 200);
        assert_eq!(v["properties"]["items"]["maxItems"], 2);
        assert_eq!(desc("d", opt(str_())).to_json()["description"], "d");
        assert!(
            obj(vec![("x", desc("d", opt(str_())))]).to_json()["required"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
