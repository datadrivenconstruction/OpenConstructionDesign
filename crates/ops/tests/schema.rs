// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The published schema is derived from the types and pinned on disk; drift fails here.
//! Refresh after an intended change: `DDC_BLESS_SCHEMA=1 cargo test -p ddc-ops --test schema`.

use std::path::PathBuf;

use ddc_ops::{schema, Op};
use serde_json::Value;

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/schema/ops.v1.json")
}

fn has_str(v: &Value, s: &str) -> bool {
    match v {
        Value::String(x) => x == s,
        Value::Array(a) => a.iter().any(|x| has_str(x, s)),
        Value::Object(m) => m.values().any(|x| has_str(x, s)),
        _ => false,
    }
}

#[test]
fn schema_snapshot_matches() {
    let path = golden_path();
    if std::env::var_os("DDC_BLESS_SCHEMA").is_some() {
        std::fs::write(&path, schema::pretty()).unwrap();
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    // Compared as values so key order never matters.
    let pinned: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    assert!(
        pinned == schema::document(),
        "docs/schema/ops.v1.json differs from the types; rerun with DDC_BLESS_SCHEMA=1 and review the diff"
    );
}

#[test]
fn every_op_kind_appears_in_the_schema() {
    let doc = schema::document();
    let defs = &doc["definitions"];
    for kind in Op::KINDS {
        let (head, tail) = match kind.split_once('.') {
            Some((h, t)) => (h, Some(t)),
            None => (kind, None),
        };
        assert!(has_str(&defs["Op"], head), "{kind}: no `{head}` in Op");
        if let Some(tail) = tail {
            let def = match head {
                "geometry" => "GeoOp",
                "type" => "TypeOp",
                "param" => "ParamOp",
                "family" => "FamilyOp",
                "level" => "LevelOp",
                "grid" => "GridOp",
                "option" => "OptionOp",
                other => panic!("unknown family {other}"),
            };
            // schemars 0.8 inlines a newtype variant of an internally tagged enum
            // (`Geometry(GeoOp)`): the family then lives in its Op variant, not in definitions
            let variant = defs["Op"]["oneOf"]
                .as_array()
                .and_then(|vs| vs.iter().find(|v| has_str(&v["properties"]["op"], head)));
            let scope = defs
                .get(def)
                .or(variant)
                .unwrap_or_else(|| panic!("{kind}: no schema for {def}"));
            assert!(has_str(scope, tail), "{kind}: no `{tail}` in {def}");
        }
    }
}

#[test]
fn every_op_variant_has_valid_example() {
    let ex = schema::examples();
    for kind in Op::KINDS {
        let v = ex["ops"][kind].clone();
        assert!(!v.is_null(), "no example for {kind}");
        let op: Op = serde_json::from_value(v).unwrap_or_else(|e| panic!("{kind}: {e}"));
        assert_eq!(op.kind(), kind);
    }
    assert_eq!(ex["elements"].as_object().unwrap().len(), 15);
    for (kind, v) in ex["elements"].as_object().unwrap() {
        let el: ddc_ops::NewEl = serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{kind}: {e}"));
        assert_eq!(serde_json::to_value(&el).unwrap()["kind"], kind.as_str());
    }
}

#[test]
fn units_are_present_in_field_descriptions() {
    let doc = schema::document();
    for name in ["Len", "Angle", "P2", "P3", "V3", "Value"] {
        let def = &doc["definitions"][name];
        let props = def["properties"].as_object().unwrap_or_else(|| panic!("{name}: no properties"));
        for (field, p) in props {
            let d = p["description"].as_str().unwrap_or("");
            assert!(!d.is_empty(), "{name}.{field}: no description");
        }
        let d = props["unit"]["description"].as_str().unwrap_or("");
        assert!(d.to_lowercase().contains("unit"), "{name}.unit: {d}");
        let req = def["required"].as_array().unwrap_or_else(|| panic!("{name}: no required"));
        assert!(req.iter().any(|r| r == "unit"), "{name}: unit not required");
    }
}

#[test]
fn schema_title_names_the_current_wire_version() {
    let doc = schema::document();
    let want = format!("ddc-ops wire v{}", ddc_ops::OP_VERSION);
    assert_eq!(doc["title"].as_str(), Some(want.as_str()));
}
