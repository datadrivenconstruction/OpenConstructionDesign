// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The documents of the editor-ERP contract `ddc-erp-link/1` and a checker for them.
//!
//! The schemas are the JSON Schema files of the contract, copied into `contract/schemas`
//! (`contract/SOURCE` names the commit they come from). The checker covers the keywords those
//! files use and nothing else; an unknown keyword is an error, so a schema that grows cannot be
//! half-checked without notice. Writers call it on their own output before it leaves the machine.

use regex::Regex;
use serde_json::Value;

pub const REVISION: &str = include_str!("../contract/schemas/revision.schema.json");
pub const DIFF: &str = include_str!("../contract/schemas/diff.schema.json");
pub const BRIEF: &str = include_str!("../contract/schemas/brief.schema.json");
pub const LINK: &str = include_str!("../contract/schemas/link.schema.json");
pub const CONTEXT: &str = include_str!("../contract/schemas/context.schema.json");
pub const ELEMENT_ROW: &str = include_str!("../contract/schemas/element-row.schema.json");

/// Parquet codecs of `elements.parquet` the ERP intake reads (its reader is not limited to one
/// codec; the intake only limits the zip member method). Desktop writes ZSTD, the browser SNAPPY.
pub const ELEMENTS_CODECS: &[&str] = &["ZSTD", "SNAPPY"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Doc {
    Revision,
    Diff,
    Brief,
    Link,
    Context,
    ElementRow,
}

impl Doc {
    fn text(self) -> &'static str {
        match self {
            Doc::Revision => REVISION,
            Doc::Diff => DIFF,
            Doc::Brief => BRIEF,
            Doc::Link => LINK,
            Doc::Context => CONTEXT,
            Doc::ElementRow => ELEMENT_ROW,
        }
    }
}

/// Checks `value` against the schema of `doc`. Errors are `pointer: what`, in document order.
pub fn validate(doc: Doc, value: &Value) -> Result<(), Vec<String>> {
    let schema: Value = serde_json::from_str(doc.text()).map_err(|e| vec![format!("schema: {e}")])?;
    validate_with(&schema, value)
}

/// Same for a schema given as a value.
pub fn validate_with(schema: &Value, value: &Value) -> Result<(), Vec<String>> {
    let mut errs = vec![];
    check(schema, schema, value, "", &mut errs);
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

const KNOWN: &[&str] = &[
    "$schema", "$id", "$defs", "$ref", "$comment", "title", "description", "default", "examples", "type",
    "properties", "additionalProperties", "required", "pattern", "const", "minimum", "maximum",
    "exclusiveMinimum", "exclusiveMaximum", "items", "enum", "maxLength", "minLength", "minItems", "maxItems",
    "then", "if", "else", "anyOf", "allOf", "oneOf", "contains", "minContains", "maxContains", "uniqueItems",
    "propertyNames", "format", "contentMediaType", "patternProperties", "not",
];

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => v.as_i64().is_some() || v.as_u64().is_some() || v.as_f64().is_some_and(|x| x.fract() == 0.0 && x.is_finite()),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "object" => v.is_object(),
        "array" => v.is_array(),
        _ => false,
    }
}

fn resolve<'a>(root: &'a Value, r: &str) -> Option<&'a Value> {
    let p = r.strip_prefix('#')?;
    if p.is_empty() {
        return Some(root);
    }
    let mut cur = root;
    for part in p.strip_prefix('/')?.split('/') {
        cur = cur.get(part.replace("~1", "/").replace("~0", "~"))?;
    }
    Some(cur)
}

fn rx(p: &str) -> Result<Regex, String> {
    // the regex crate has no look-ahead; the one pattern that uses it (`(?!.*\.\.)`, "no `..`")
    // is checked by the caller as a plain substring test
    Regex::new(&p.replace("(?!.*\\.\\.)", "")).map_err(|e| format!("bad pattern {p}: {e}"))
}

fn is_date_time(s: &str) -> bool {
    let re = Regex::new(r"^\d{4}-\d{2}-\d{2}[Tt]\d{2}:\d{2}:\d{2}(\.\d+)?([Zz]|[+-]\d{2}:\d{2})$").unwrap();
    re.is_match(s)
}

fn check(root: &Value, s: &Value, v: &Value, at: &str, errs: &mut Vec<String>) {
    let here = |m: String| format!("{}: {m}", if at.is_empty() { "/" } else { at });
    let Some(obj) = s.as_object() else {
        if s == &Value::Bool(false) {
            errs.push(here("not allowed".into()));
        }
        return;
    };
    for k in obj.keys() {
        if !KNOWN.contains(&k.as_str()) {
            errs.push(here(format!("schema keyword '{k}' is not supported by the checker")));
        }
    }
    if let Some(r) = obj.get("$ref").and_then(Value::as_str) {
        match resolve(root, r) {
            Some(t) => check(root, t, v, at, errs),
            None => errs.push(here(format!("unresolved $ref {r}"))),
        }
    }
    if let Some(t) = obj.get("type") {
        let ok = match t {
            Value::String(t) => type_ok(t, v),
            Value::Array(a) => a.iter().filter_map(Value::as_str).any(|t| type_ok(t, v)),
            _ => true,
        };
        if !ok {
            errs.push(here(format!("expected {t}, found {}", kind(v))));
            return;
        }
    }
    if let Some(c) = obj.get("const") {
        if c != v {
            errs.push(here(format!("must be {c}")));
        }
    }
    if let Some(Value::Array(e)) = obj.get("enum") {
        if !e.contains(v) {
            errs.push(here(format!("{v} is not one of {}", Value::Array(e.clone()))));
        }
    }
    if let Some(n) = v.as_f64() {
        if let Some(m) = obj.get("minimum").and_then(Value::as_f64) {
            if n < m {
                errs.push(here(format!("{n} is below {m}")));
            }
        }
        if let Some(m) = obj.get("maximum").and_then(Value::as_f64) {
            if n > m {
                errs.push(here(format!("{n} is above {m}")));
            }
        }
        if let Some(m) = obj.get("exclusiveMinimum").and_then(Value::as_f64) {
            if n <= m {
                errs.push(here(format!("{n} must be above {m}")));
            }
        }
        if let Some(m) = obj.get("exclusiveMaximum").and_then(Value::as_f64) {
            if n >= m {
                errs.push(here(format!("{n} must be below {m}")));
            }
        }
    }
    if let Some(t) = v.as_str() {
        let len = t.chars().count();
        if let Some(m) = obj.get("minLength").and_then(Value::as_u64) {
            if (len as u64) < m {
                errs.push(here(format!("shorter than {m}")));
            }
        }
        if let Some(m) = obj.get("maxLength").and_then(Value::as_u64) {
            if len as u64 > m {
                errs.push(here(format!("longer than {m}")));
            }
        }
        if let Some(p) = obj.get("pattern").and_then(Value::as_str) {
            match rx(p) {
                Ok(r) => {
                    if !r.is_match(t) || (p.contains("(?!.*\\.\\.)") && t.contains("..")) {
                        errs.push(here(format!("'{}' does not match {p}", clip(t))));
                    }
                }
                Err(e) => errs.push(here(e)),
            }
        }
        match obj.get("format").and_then(Value::as_str) {
            Some("date-time") if !is_date_time(t) => errs.push(here(format!("'{t}' is not a date-time"))),
            Some("uri") if !t.contains(':') => errs.push(here(format!("'{t}' is not a URI"))),
            Some("date") if !Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap().is_match(t) => {
                errs.push(here(format!("'{t}' is not a date")))
            }
            _ => {}
        }
        if obj.get("contentMediaType").and_then(Value::as_str) == Some("application/json")
            && serde_json::from_str::<Value>(t).is_err()
        {
            errs.push(here("is not JSON text".into()));
        }
    }
    if let Some(a) = v.as_array() {
        if let Some(m) = obj.get("minItems").and_then(Value::as_u64) {
            if (a.len() as u64) < m {
                errs.push(here(format!("fewer than {m} items")));
            }
        }
        if let Some(m) = obj.get("maxItems").and_then(Value::as_u64) {
            if a.len() as u64 > m {
                errs.push(here(format!("more than {m} items")));
            }
        }
        if obj.get("uniqueItems") == Some(&Value::Bool(true)) {
            for (i, x) in a.iter().enumerate() {
                if a[..i].contains(x) {
                    errs.push(here(format!("item {i} repeats an earlier item")));
                }
            }
        }
        if let Some(it) = obj.get("items") {
            for (i, x) in a.iter().enumerate() {
                check(root, it, x, &format!("{at}/{i}"), errs);
            }
        }
        if let Some(c) = obj.get("contains") {
            let n = a.iter().filter(|x| matches(root, c, x)).count() as u64;
            let min = obj.get("minContains").and_then(Value::as_u64).unwrap_or(1);
            if n < min {
                errs.push(here(format!("{n} items match, at least {min} needed")));
            }
            if let Some(max) = obj.get("maxContains").and_then(Value::as_u64) {
                if n > max {
                    errs.push(here(format!("{n} items match, at most {max} allowed")));
                }
            }
        }
    }
    if let Some(m) = v.as_object() {
        if let Some(Value::Array(req)) = obj.get("required") {
            for r in req.iter().filter_map(Value::as_str) {
                if !m.contains_key(r) {
                    errs.push(here(format!("missing '{r}'")));
                }
            }
        }
        let props = obj.get("properties").and_then(Value::as_object);
        for (k, x) in m {
            let sub = format!("{at}/{k}");
            if let Some(ps) = props.and_then(|p| p.get(k)) {
                check(root, ps, x, &sub, errs);
            } else {
                match obj.get("additionalProperties") {
                    Some(Value::Bool(false)) => errs.push(here(format!("unknown field '{k}'"))),
                    Some(s @ Value::Object(_)) => check(root, s, x, &sub, errs),
                    _ => {}
                }
            }
            if let Some(pn) = obj.get("propertyNames") {
                check(root, pn, &Value::String(k.clone()), &sub, errs);
            }
        }
    }
    if let Some(Value::Array(a)) = obj.get("anyOf") {
        if !a.iter().any(|s| matches(root, s, v)) {
            errs.push(here("matches none of the allowed forms".into()));
        }
    }
    if let Some(Value::Array(a)) = obj.get("oneOf") {
        if a.iter().filter(|s| matches(root, s, v)).count() != 1 {
            errs.push(here("must match exactly one of the allowed forms".into()));
        }
    }
    if let Some(Value::Array(a)) = obj.get("allOf") {
        for s in a {
            check(root, s, v, at, errs);
        }
    }
    if let Some(n) = obj.get("not") {
        if matches(root, n, v) {
            errs.push(here("matches a form that is not allowed".into()));
        }
    }
    if let Some(c) = obj.get("if") {
        if matches(root, c, v) {
            if let Some(t) = obj.get("then") {
                check(root, t, v, at, errs);
            }
        } else if let Some(e) = obj.get("else") {
            check(root, e, v, at, errs);
        }
    }
}

fn matches(root: &Value, s: &Value, v: &Value) -> bool {
    let mut e = vec![];
    check(root, s, v, "", &mut e);
    e.is_empty()
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn clip(s: &str) -> String {
    s.chars().take(60).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ex(name: &str) -> Value {
        let t = match name {
            "manifest" => include_str!("../contract/examples/manifest.json"),
            "diff" => include_str!("../contract/examples/diff.json"),
            "brief" => include_str!("../contract/examples/brief.de-residential.json"),
            "link" => include_str!("../contract/examples/link.ddclink.json"),
            "context" => include_str!("../contract/examples/context.json"),
            _ => unreachable!(),
        };
        serde_json::from_str(t).unwrap()
    }

    #[test]
    fn the_contract_examples_validate() {
        for (doc, name) in [
            (Doc::Revision, "manifest"),
            (Doc::Diff, "diff"),
            (Doc::Brief, "brief"),
            (Doc::Link, "link"),
            (Doc::Context, "context"),
        ] {
            validate(doc, &ex(name)).unwrap_or_else(|e| panic!("{name}: {e:#?}"));
        }
        let rows: Vec<Value> = serde_json::from_str(include_str!("../contract/examples/elements.rows.json")).unwrap();
        assert!(!rows.is_empty());
        for r in &rows {
            validate(Doc::ElementRow, r).unwrap_or_else(|e| panic!("{e:#?}"));
        }
    }

    #[test]
    fn the_checker_rejects_what_the_schema_forbids() {
        let mut m = ex("manifest");
        m["units"]["length"] = json!("ft");
        m["extra"] = json!(1);
        m["files"][0]["sha256"] = json!("xyz");
        m["created_at"] = json!("yesterday");
        let e = validate(Doc::Revision, &m).unwrap_err().join("\n");
        assert!(e.contains("/units/length"), "{e}");
        assert!(e.contains("unknown field 'extra'"), "{e}");
        assert!(e.contains("/files/0/sha256"), "{e}");
        assert!(e.contains("/created_at"), "{e}");
        let mut m = ex("manifest");
        m.as_object_mut().unwrap().remove("counts");
        assert!(validate(Doc::Revision, &m).unwrap_err().join(" ").contains("missing 'counts'"));
    }

    #[test]
    fn contains_bounds_and_conditionals_work() {
        let mut m = ex("manifest");
        let files = m["files"].as_array_mut().unwrap();
        files.retain(|f| f["role"] != "geometry");
        let e = validate(Doc::Revision, &m).unwrap_err().join(" ");
        assert!(e.contains("at least 1"), "{e}");
        let mut m = ex("manifest");
        m["source"] = json!({"kind": "generated"});
        assert!(validate(Doc::Revision, &m).is_err(), "generated needs brief_sha256 and generator");
    }

    #[test]
    fn an_unknown_schema_keyword_is_an_error_not_a_silent_pass() {
        let s = json!({"type": "string", "dependentSchemas": {}});
        let e = validate_with(&s, &json!("x")).unwrap_err().join(" ");
        assert!(e.contains("dependentSchemas"));
    }
}
