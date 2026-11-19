//! JSON schema of the wire, derived from the Rust types (never written by hand).
//!
//! `document()` is pinned as `docs/schema/ops.v1.json`; a test fails when the types and the
//! pinned file drift apart. Tool input schemas for agents are cut from the same definitions
//! with `tool_input`, so a field added to a type shows up for agents without further work.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use crate::refs::Cat;
use crate::{sample, Envelope, Op};

const DEFS: &str = "#/definitions/";

/// Schema of `Envelope` with all reachable definitions, as schemars derives it.
pub fn root() -> Value {
    serde_json::to_value(schemars::schema_for!(Envelope)).expect("schema is plain JSON")
}

/// Reference to a definition by type name, e.g. `def_ref("NewEl")`.
pub fn def_ref(name: &str) -> Value {
    json!({ "$ref": format!("{DEFS}{name}") })
}

/// One valid example per op kind and per new-element family, taken from `sample`.
pub fn examples() -> Value {
    let mut ops = Map::new();
    let mut elements = Map::new();
    for op in sample::all_ops() {
        if let Op::Create { el, .. } = &op {
            let v = serde_json::to_value(el).expect("sample serialises");
            let kind = v["kind"].as_str().unwrap_or_default().to_owned();
            elements.entry(kind).or_insert(v);
        }
        let v = serde_json::to_value(&op).expect("sample serialises");
        ops.entry(op.kind().to_owned()).or_insert(v);
    }
    json!({ "ops": ops, "elements": elements })
}

/// The pinned document: derived schema plus the examples under `x-examples`.
pub fn document() -> Value {
    let mut doc = root();
    doc["title"] = json!("ddc-ops wire v2");
    doc["x-examples"] = examples();
    doc["x-categories"] = json!(Cat::ALL.iter().map(|c| c.key()).collect::<Vec<_>>());
    doc
}

/// Pretty text of `document()`, newline-terminated, as stored in the repository.
pub fn pretty() -> String {
    let mut s = serde_json::to_string_pretty(&document()).expect("schema is plain JSON");
    s.push('\n');
    s
}

fn collect_refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                match (k.as_str(), x.as_str()) {
                    ("$ref", Some(s)) => {
                        if let Some(n) = s.strip_prefix(DEFS) {
                            out.push(n.to_owned());
                        }
                    }
                    _ => collect_refs(x, out),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_refs(x, out)),
        _ => {}
    }
}

/// Object schema for a tool input: `props` as given (they may `$ref` wire types), the
/// definitions they reach copied alongside, unknown fields rejected.
pub fn tool_input(props: Vec<(&str, Value)>, required: &[&str]) -> Value {
    let defs = root()["definitions"].clone();
    let mut need = Vec::new();
    for (_, p) in &props {
        collect_refs(p, &mut need);
    }
    let mut seen = BTreeSet::new();
    let mut keep = Map::new();
    while let Some(name) = need.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(d) = defs.get(&name) {
            collect_refs(d, &mut need);
            keep.insert(name, d.clone());
        }
    }
    let properties: Map<String, Value> = props.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
        "definitions": keep,
    })
}
