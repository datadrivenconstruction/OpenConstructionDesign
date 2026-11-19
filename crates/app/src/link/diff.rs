// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `diff.json` (`ddc-diff/1`): this revision's rows against the parent's, keyed by `element_key`.

use std::collections::{BTreeMap, BTreeSet};

use ddc_io::elements_parquet::ElementRow;
use serde_json::{json, Map, Value};

fn same_num(a: f64, b: f64) -> bool {
    a == b || (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) if a.is_number() && b.is_number() => same_num(x, y),
        _ => a == b,
    }
}

fn opt(s: &Option<String>) -> Value {
    s.as_ref().map_or(Value::Null, |v| json!(v))
}

fn optf(v: Option<f64>) -> Value {
    v.filter(|x| x.is_finite()).map_or(Value::Null, |x| json!(x))
}

/// Scalars the schema allows; objects and arrays travel as compact JSON text.
fn scalar(v: &Value) -> Value {
    match v {
        Value::Array(_) | Value::Object(_) => json!(v.to_string()),
        o => o.clone(),
    }
}

/// Column values (not the key, not the box, not the hash) and one `props.<name>` per parameter.
fn fields(r: &ElementRow) -> BTreeMap<String, Value> {
    let mut m = BTreeMap::new();
    for (k, v) in [
        ("element_type", json!(r.element_type)),
        ("ifc_class", json!(r.ifc_class)),
        ("type_key", json!(r.type_key)),
        ("name", json!(r.name)),
        ("storey", opt(&r.storey)),
        ("space", opt(&r.space)),
        ("discipline", json!(r.discipline)),
        ("material", opt(&r.material)),
        ("phase", opt(&r.phase)),
        ("space_usage", opt(&r.space_usage)),
        ("length_m", optf(r.length_m)),
        ("area_m2", optf(r.area_m2)),
        ("area_gross_m2", optf(r.area_gross_m2)),
        ("volume_m3", optf(r.volume_m3)),
        ("volume_gross_m3", optf(r.volume_gross_m3)),
        ("weight_kg", optf(r.weight_kg)),
        ("count", r.count.map_or(Value::Null, |c| json!(c))),
    ] {
        m.insert(k.to_string(), v);
    }
    if let Some(Ok(Value::Object(p))) = r.props.as_ref().map(|t| serde_json::from_str::<Value>(t)) {
        for (k, v) in p {
            m.insert(format!("props.{k}"), scalar(&v));
        }
    }
    m
}

fn round9(x: f64) -> f64 {
    let r = (x * 1e9).round() / 1e9;
    if r == 0.0 {
        0.0
    } else {
        r
    }
}

/// `parent` is `None` for a first revision: everything is added.
pub fn diff(rev: &str, parent_id: Option<&str>, parent: &[ElementRow], now: &[ElementRow]) -> Value {
    let old: BTreeMap<&str, &ElementRow> = parent.iter().map(|r| (r.key.as_str(), r)).collect();
    let new: BTreeMap<&str, &ElementRow> = now.iter().map(|r| (r.key.as_str(), r)).collect();
    let added: Vec<&str> = new.keys().filter(|k| !old.contains_key(*k)).copied().collect();
    let removed: Vec<&str> = old.keys().filter(|k| !new.contains_key(*k)).copied().collect();

    let mut changed = vec![];
    for (k, n) in &new {
        let Some(o) = old.get(k) else { continue };
        let (fo, fnw) = (fields(o), fields(n));
        let names: BTreeSet<&String> = fo.keys().chain(fnw.keys()).collect();
        let mut list = vec![];
        for f in names {
            let (a, b) = (fo.get(f).cloned().unwrap_or(Value::Null), fnw.get(f).cloned().unwrap_or(Value::Null));
            if !same(&a, &b) {
                list.push(json!({"field": f, "old": a, "new": b}));
            }
        }
        let geometry = o.geometry_hash != n.geometry_hash;
        if geometry {
            list.push(json!({"field": "geometry_hash", "old": o.geometry_hash, "new": n.geometry_hash}));
        }
        if !list.is_empty() {
            changed.push(json!({"element_key": k, "geometry": geometry, "fields": list}));
        }
    }

    // (type_key, ifc_class) -> count, length, area, volume, weight
    let mut by: BTreeMap<(String, String), [f64; 5]> = BTreeMap::new();
    let mut tally = |rows: &[ElementRow], sign: f64| {
        for r in rows {
            let e = by.entry((r.type_key.clone(), r.ifc_class.clone())).or_default();
            e[0] += sign;
            for (i, v) in [r.length_m, r.area_m2, r.volume_m3, r.weight_kg].into_iter().enumerate() {
                e[i + 1] += sign * v.filter(|x| x.is_finite()).unwrap_or(0.0);
            }
        }
    };
    tally(now, 1.0);
    tally(parent, -1.0);
    let by_type: Vec<Value> = by
        .into_iter()
        .filter_map(|((t, c), d)| {
            let d = d.map(round9);
            if d.iter().all(|x| *x == 0.0) {
                return None;
            }
            let mut o = Map::new();
            o.insert("type_key".into(), json!(t));
            o.insert("ifc_class".into(), json!(c));
            o.insert("count_delta".into(), json!(d[0] as i64));
            for (i, k) in ["length_m_delta", "area_m2_delta", "volume_m3_delta", "weight_kg_delta"].into_iter().enumerate() {
                if d[i + 1] != 0.0 {
                    o.insert(k.into(), json!(d[i + 1]));
                }
            }
            Some(Value::Object(o))
        })
        .collect();

    json!({
        "schema": "ddc-diff/1", "revision_id": rev, "parent_revision_id": parent_id,
        "added": added, "removed": removed, "changed": changed, "by_type": by_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, area: f64, hash: &str) -> ElementRow {
        ElementRow {
            key: key.into(),
            element_type: "wall".into(),
            ifc_class: "IfcWall".into(),
            type_key: "wall/x".into(),
            name: key.into(),
            storey: Some("EG".into()),
            space: None,
            discipline: "architecture".into(),
            material: None,
            phase: None,
            space_usage: None,
            geometry_hash: hash.into(),
            length_m: Some(4.0),
            area_m2: Some(area),
            area_gross_m2: None,
            volume_m3: None,
            volume_gross_m3: None,
            weight_kg: None,
            count: None,
            bbox: [0.0; 6],
            props: Some(r#"{"u":0.24,"layers":[1,2]}"#.into()),
        }
    }

    const R: &str = "95bb145e-f295-43dd-a059-d27876741295";
    const P: &str = "68143a44-27fb-47bb-aa7e-c1229f23a2dc";

    #[test]
    fn added_removed_changed_are_disjoint_and_match_the_schema() {
        let parent = vec![row("a", 10.0, "aa"), row("b", 10.0, "bb"), row("c", 10.0, "cc")];
        let mut c = row("c", 12.0, "cd");
        c.props = Some(r#"{"u":0.21,"layers":[1,2]}"#.into());
        let now = vec![row("a", 10.0, "aa"), c, row("d", 5.0, "dd")];
        let d = diff(R, Some(P), &parent, &now);
        assert_eq!(d["added"], json!(["d"]));
        assert_eq!(d["removed"], json!(["b"]));
        let ch = d["changed"].as_array().unwrap();
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0]["element_key"], "c");
        assert_eq!(ch[0]["geometry"], true);
        let names: Vec<&str> = ch[0]["fields"].as_array().unwrap().iter().map(|f| f["field"].as_str().unwrap()).collect();
        assert_eq!(names, ["area_m2", "props.u", "geometry_hash"]);
        // a: 0, b: -10, c: +2, d: +5 => count 0 (removed one, added one), area -3
        let bt = d["by_type"].as_array().unwrap();
        assert_eq!(bt.len(), 1);
        assert_eq!(bt[0]["area_m2_delta"], -3.0);
        assert!(bt[0].get("count_delta").is_some());
        ddc_io::contract::validate(ddc_io::contract::Doc::Diff, &d).unwrap();
    }

    #[test]
    fn first_revision_adds_everything() {
        let now = vec![row("a", 1.0, "aa"), row("b", 2.0, "bb")];
        let d = diff(R, None, &[], &now);
        assert_eq!(d["added"], json!(["a", "b"]));
        assert!(d["parent_revision_id"].is_null());
        ddc_io::contract::validate(ddc_io::contract::Doc::Diff, &d).unwrap();
    }

    #[test]
    fn equal_models_have_an_empty_diff() {
        let rows = vec![row("a", 1.0, "aa")];
        let d = diff(R, Some(P), &rows, &rows);
        assert!(d["added"].as_array().unwrap().is_empty());
        assert!(d["removed"].as_array().unwrap().is_empty());
        assert!(d["changed"].as_array().unwrap().is_empty());
        assert!(d["by_type"].as_array().unwrap().is_empty());
    }

    #[test]
    fn float_noise_is_not_a_change() {
        let (a, b) = (row("a", 10.0, "aa"), row("a", 10.0 + 1e-12, "aa"));
        assert!(diff(R, Some(P), &[a], &[b])["changed"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_parameter_that_appears_is_a_change_from_null() {
        let a = row("a", 1.0, "aa");
        let mut b = a.clone();
        b.props = Some(r#"{"u":0.24,"layers":[1,2],"new":"x"}"#.into());
        let d = diff(R, Some(P), &[a], &[b]);
        let f = &d["changed"][0]["fields"][0];
        assert_eq!((f["field"].as_str(), f["old"].is_null(), f["new"].as_str()), (Some("props.new"), true, Some("x")));
        assert_eq!(d["changed"][0]["geometry"], false);
        ddc_io::contract::validate(ddc_io::contract::Doc::Diff, &d).unwrap();
    }

    #[test]
    fn a_nested_parameter_value_is_compared_as_text() {
        let a = row("a", 1.0, "aa");
        let mut b = a.clone();
        b.props = Some(r#"{"u":0.24,"layers":[1,3]}"#.into());
        let d = diff(R, Some(P), &[a], &[b]);
        assert_eq!(d["changed"][0]["fields"][0]["new"], "[1,3]");
    }
}
