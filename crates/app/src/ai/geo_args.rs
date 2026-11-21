// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Natural-argument normaliser of `create_geometry`: a model writes `"2.1 m"`, `[0, 0, 3]`,
//! `{"x": "1 m", ...}` or leaves out an unambiguous `kind`; the strict wire types of
//! `ops/src/geo.rs` take `{v, unit}` and `{x, y, z, unit}`. The pass is driven by the JSON
//! schema of `GeoOp` itself, so every field of every action is covered without a table of
//! names, and a value it cannot read unambiguously is left untouched for the strict error.
//! Units stay explicit: a bare number is taken only with the op-level `unit` the caller wrote.

use ddc_ops::GeoOp;
use serde_json::{json, Map, Value as J};
use std::sync::OnceLock;

fn schema() -> &'static J {
    static S: OnceLock<J> = OnceLock::new();
    S.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(GeoOp)).unwrap_or(J::Null)
    })
}

fn resolve<'a>(root: &'a J, s: &'a J) -> (&'a J, Option<&'a str>) {
    match s.get("$ref").and_then(|r| r.as_str()) {
        Some(r) => {
            let name = r.rsplit('/').next().unwrap_or_default();
            let def = root
                .get("definitions")
                .or_else(|| root.get("$defs"))
                .and_then(|d| d.get(name))
                .unwrap_or(s);
            (def, Some(name))
        }
        // A documented field wraps its `$ref` as `{"description", "allOf": [{"$ref"}]}`.
        None => match s.get("allOf").and_then(|a| a.as_array()).map(Vec::as_slice) {
            Some([one]) => resolve(root, one),
            _ => (s, None),
        },
    }
}

fn props(s: &J) -> Option<&Map<String, J>> {
    s.get("properties").and_then(|p| p.as_object())
}

fn enum_values(root: &J, s: &J) -> Vec<String> {
    let (s, _) = resolve(root, s);
    let mut out: Vec<String> = s
        .get("enum")
        .and_then(|e| e.as_array())
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect();
    for k in ["oneOf", "anyOf"] {
        for b in s.get(k).and_then(|b| b.as_array()).into_iter().flatten() {
            out.extend(enum_values(root, b));
        }
    }
    out
}

/// Single value of a tag property (`"kind": {"enum": ["extrude"]}`).
fn tag_of<'a>(variant: &'a J) -> Option<(&'a str, &'a str)> {
    props(variant)?.iter().find_map(|(k, p)| {
        let e = p.get("enum")?.as_array()?;
        if e.len() == 1 { Some((k.as_str(), e[0].as_str()?)) } else { None }
    })
}

fn variants<'a>(root: &'a J, s: &'a J) -> Vec<&'a J> {
    s.get("oneOf")
        .or_else(|| s.get("anyOf"))
        .and_then(|b| b.as_array())
        .map(|b| b.iter().map(|x| resolve(root, x).0).collect())
        .unwrap_or_default()
}

fn factor(unit: &str) -> Option<f64> {
    Some(match unit {
        "m" => 1.0,
        "cm" => 0.01,
        "mm" => 0.001,
        "ft" => 0.3048,
        "in" => 0.0254,
        _ => return None,
    })
}

fn unit_alias(u: &str) -> String {
    match u.trim().to_lowercase().as_str() {
        "meter" | "meters" | "metre" | "metres" => "m",
        "millimeter" | "millimeters" | "millimetre" | "millimetres" => "mm",
        "centimeter" | "centimeters" | "centimetre" | "centimetres" => "cm",
        "feet" | "foot" | "'" => "ft",
        "inch" | "inches" | "\"" => "in",
        "degree" | "degrees" | "°" => "deg",
        "radian" | "radians" => "rad",
        other => return other.to_string(),
    }
    .to_string()
}

/// `"2.1 m"`, `"500mm"`, `"-3,5 m"` -> (value, unit).
fn quantity(s: &str) -> Option<(f64, String)> {
    let t = s.trim().trim_matches(|c| c == '(' || c == ')');
    let cut = t.find(|c: char| c.is_alphabetic() || c == '°' || c == '\'' || c == '"')?;
    let (n, u) = t.split_at(cut);
    // num-lint: a model's argument text; either decimal mark is read
    let v = n.trim().replace(',', ".").parse::<f64>().ok()?;
    Some((v, unit_alias(u)))
}

fn number(v: &J) -> Option<f64> {
    // num-lint: a model's argument text; either decimal mark is read
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().replace(',', ".").parse().ok()))
}

/// A length component as (metres-or-unit value, unit) from a number, a quantity string or
/// a `{v, unit}` object.
fn component(v: &J) -> Option<(f64, Option<String>)> {
    match v {
        J::Number(n) => Some((n.as_f64()?, None)),
        J::String(s) => match quantity(s) {
            Some((x, u)) => Some((x, Some(u))),
            // num-lint: a model's argument text; either decimal mark is read
            None => Some((s.trim().replace(',', ".").parse().ok()?, None)),
        },
        J::Object(o) => {
            let x = number(o.get("v").or_else(|| o.get("value"))?)?;
            Some((x, o.get("unit").and_then(|u| u.as_str()).map(unit_alias)))
        }
        _ => None,
    }
}

/// Components with one shared unit: the unit all of them name, or metres when they mix;
/// a bare number takes the caller's default unit, none without it.
fn unify(parts: &[(f64, Option<String>)], default: Option<&str>) -> Option<(Vec<f64>, String)> {
    let units: Vec<&str> = parts
        .iter()
        .map(|(_, u)| u.as_deref().or(default))
        .collect::<Option<_>>()?;
    let first = units.first()?;
    if units.iter().all(|u| u == first) {
        return Some((parts.iter().map(|p| p.0).collect(), (*first).to_string()));
    }
    let vals = parts
        .iter()
        .zip(&units)
        .map(|((x, _), u)| Some(x * factor(u)?))
        .collect::<Option<_>>()?;
    Some((vals, "m".to_string()))
}

/// `"0,0,3"`, `"0 0 3 m"`, `"(0; 0; 3) mm"` -> numbers and an optional unit.
fn list_of(s: &str) -> Option<(Vec<f64>, Option<String>)> {
    let t = s.trim().trim_matches(|c| c == '(' || c == ')' || c == '[' || c == ']');
    let (nums, unit) = match t.find(|c: char| c.is_alphabetic()) {
        Some(i) => (t[..i].trim().trim_end_matches(|c| c == ')' || c == ']'), Some(unit_alias(&t[i..]))),
        None => (t, None),
    };
    let sep = if nums.contains(';') {
        ';'
    } else if nums.contains(',') && nums.contains(' ') && !nums.contains(", ") {
        ' '
    } else if nums.contains(',') {
        ','
    } else {
        ' '
    };
    let v = nums
        .split(sep)
        .map(str::trim)
        .filter(|x| !x.is_empty())
        // num-lint: a model's argument text after the unit split
        .map(|x| x.parse::<f64>().ok())
        .collect::<Option<Vec<_>>>()?;
    Some((v, unit))
}

/// Point with `names` components (`["x","y","z"]`) from an array, a string or an object.
fn point(v: &J, names: &[&str], default: Option<&str>) -> Option<J> {
    let parts: Vec<(f64, Option<String>)> = match v {
        J::Array(a) if a.len() == names.len() + 1 && a[names.len()].is_string() => {
            let u = unit_alias(a[names.len()].as_str()?);
            a[..names.len()]
                .iter()
                .map(|x| component(x).map(|(n, own)| (n, own.or_else(|| Some(u.clone())))))
                .collect::<Option<_>>()?
        }
        J::Array(a) if a.len() == names.len() => a.iter().map(component).collect::<Option<_>>()?,
        J::String(s) => {
            let (n, u) = list_of(s)?;
            (n.len() == names.len()).then_some(())?;
            n.into_iter().map(|x| (x, u.clone())).collect()
        }
        J::Object(o) => {
            let mut p: Vec<(f64, Option<String>)> =
                names.iter().map(|k| component(o.get(*k)?)).collect::<Option<_>>()?;
            if let Some(u) = o.get("unit").and_then(|u| u.as_str()) {
                let u = unit_alias(u);
                for c in &mut p {
                    c.1.get_or_insert(u.clone());
                }
            }
            p
        }
        _ => return None,
    };
    let (vals, unit) = unify(&parts, default)?;
    let mut o = Map::new();
    for (k, x) in names.iter().zip(vals) {
        o.insert((*k).to_string(), json!(x));
    }
    o.insert("unit".into(), json!(unit));
    Some(J::Object(o))
}

fn dir(v: &J) -> Option<J> {
    let n: Vec<f64> = match v {
        J::Array(a) if a.len() == 3 => a.iter().map(number).collect::<Option<_>>()?,
        J::String(s) => match s.trim().to_lowercase().as_str() {
            "x" | "+x" => vec![1.0, 0.0, 0.0],
            "-x" => vec![-1.0, 0.0, 0.0],
            "y" | "+y" => vec![0.0, 1.0, 0.0],
            "-y" => vec![0.0, -1.0, 0.0],
            "z" | "+z" => vec![0.0, 0.0, 1.0],
            "-z" => vec![0.0, 0.0, -1.0],
            _ => list_of(s)?.0,
        },
        J::Object(o) => ["x", "y", "z"].iter().map(|k| number(o.get(*k)?)).collect::<Option<_>>()?,
        _ => return None,
    };
    (n.len() == 3).then(|| json!({"x": n[0], "y": n[1], "z": n[2]}))
}

fn measure(v: &J, units: &[String], default: Option<&str>) -> Option<J> {
    let (x, u) = component(v)?;
    let unit = match u {
        Some(u) => u,
        None => default.filter(|d| units.iter().any(|x| x == d))?.to_string(),
    };
    units.iter().any(|x| *x == unit).then(|| json!({"v": x, "unit": unit}))
}

fn has(s: &J, keys: &[&str]) -> bool {
    props(s).is_some_and(|p| p.len() == keys.len() && keys.iter().all(|k| p.contains_key(*k)))
}

fn walk(v: &mut J, s: &J, root: &J, default: Option<&str>) {
    let (s, name) = resolve(root, s);
    if let Some(any) = s.get("anyOf").and_then(|a| a.as_array()) {
        // Option<T>: the non-null branch.
        let mut live = any.iter().filter(|b| b.get("type").and_then(|t| t.as_str()) != Some("null"));
        if let (Some(b), None) = (live.next(), live.next()) {
            if !v.is_null() {
                walk(v, b, root, default);
            }
            return;
        }
    }
    if s.get("oneOf").is_some() {
        pick_variant(v, s, name, root, default);
        return;
    }
    if has(s, &["v", "unit"]) {
        let units = props(s).map(|p| enum_values(root, &p["unit"])).unwrap_or_default();
        if let Some(m) = measure(v, &units, default) {
            *v = m;
        }
    } else if has(s, &["x", "y", "z", "unit"]) {
        if let Some(p) = point(v, &["x", "y", "z"], default) {
            *v = p;
        }
    } else if has(s, &["x", "y", "unit"]) {
        if let Some(p) = point(v, &["x", "y"], default) {
            *v = p;
        }
    } else if has(s, &["x", "y", "z"]) {
        if let Some(d) = dir(v) {
            *v = d;
        }
    } else if let (Some(o), Some(p)) = (v.as_object_mut(), props(s)) {
        for (k, x) in o.iter_mut() {
            if let Some(ps) = p.get(k) {
                walk(x, ps, root, default);
            }
        }
    } else if let (Some(a), Some(items)) = (v.as_array_mut(), s.get("items")) {
        // A point list written as one flat string or nested arrays of numbers stays as is;
        // each item is read against the item schema.
        for x in a.iter_mut() {
            walk(x, items, root, default);
        }
    } else if let Some(x) = v.as_str().map(str::to_string) {
        let vals = enum_values(root, s);
        if let Some(m) = vals.iter().find(|e| e.eq_ignore_ascii_case(x.trim()) && **e != x) {
            *v = json!(m);
        }
    }
}

fn pick_variant(v: &mut J, s: &J, name: Option<&str>, root: &J, default: Option<&str>) {
    let vs = variants(root, s);
    // `wall:5` is a reference written as a string.
    if name == Some("Ref") {
        if let Some(r) = v.as_str().and_then(ref_of) {
            *v = r;
        }
    }
    let Some(o) = v.as_object() else { return };
    let tags: Vec<(&J, (&str, &str))> = vs.iter().filter_map(|x| Some((*x, tag_of(x)?))).collect();
    let chosen = match tags.first().map(|t| t.1 .0) {
        Some(key) => match o.get(key).and_then(|t| t.as_str()) {
            Some(t) => tags.iter().find(|(_, (_, val))| val.eq_ignore_ascii_case(t)).map(|t| (t.0, key, t.1 .1)),
            None => {
                // No tag: the one variant that holds every key given and every required key.
                let fits: Vec<_> = tags
                    .iter()
                    .filter(|(x, (tk, _))| {
                        let p = props(x);
                        let req = x.get("required").and_then(|r| r.as_array());
                        o.keys().all(|k| p.is_some_and(|p| p.contains_key(k)))
                            && req.into_iter().flatten().filter_map(|r| r.as_str()).all(|r| r == *tk || o.contains_key(r))
                    })
                    .collect();
                (fits.len() == 1).then(|| (fits[0].0, key, fits[0].1 .1))
            }
        },
        None => None,
    };
    let Some((variant, key, val)) = chosen else { return };
    if let Some(o) = v.as_object_mut() {
        o.insert(key.to_string(), json!(val));
        if let Some(p) = props(variant) {
            for (k, x) in o.iter_mut() {
                if k != key {
                    if let Some(ps) = p.get(k) {
                        walk(x, ps, root, default);
                    }
                }
            }
        }
    }
}

fn ref_of(s: &str) -> Option<J> {
    serde_json::to_value(ddc_ops::Ref::parse(s.trim()).ok()?).ok()
}

/// Rewrite the natural spellings of one geometry op into the strict wire form. `op` is the
/// action object (without the `op` tag). An op-level `unit` is the unit of bare numbers.
pub fn normalize(op: &mut J) {
    let (default, tag) = match op.as_object_mut() {
        Some(o) => (
            o.remove("unit").and_then(|u| u.as_str().map(unit_alias)),
            o.remove("op"),
        ),
        None => (None, None),
    };
    let root = schema();
    if !root.is_null() {
        walk(op, root, root, default.as_deref());
    }
    if let (Some(o), Some(t)) = (op.as_object_mut(), tag) {
        o.insert("op".into(), t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_spellings_become_the_wire_form() {
        let mut o = json!({"action": "solid", "slot": 0, "form": {
            "kind": "extrude",
            "profile": {"shape": "rect", "w": "300 mm", "h": {"v": 0.5, "unit": "m"}},
            "frame": {"origin": [0, 0, 3, "m"], "x": "x", "z": [0, 0, 1]},
            "start": "0 m", "end": "2,5 m"}});
        normalize(&mut o);
        let g: GeoOp = serde_json::from_value(o).expect("strict parse");
        assert!(matches!(g, GeoOp::Solid { .. }));
    }

    #[test]
    fn tag_is_inferred_only_when_one_variant_fits() {
        let mut o = json!({"action": "solid", "slot": 0, "form": {
            "profile": {"shape": "circle", "r": "100 mm"},
            "frame": {"origin": {"x": "1 m", "y": {"v": 200, "unit": "cm"}, "z": "0 m"}, "x": [1,0,0], "z": "z"},
            "start": "0 m", "end": "3 m"}});
        normalize(&mut o);
        assert_eq!(o["form"]["kind"], "extrude");
        assert_eq!(o["form"]["frame"]["origin"]["unit"], "m");
        assert_eq!(o["form"]["frame"]["origin"]["y"], 2.0);
        let mut o = json!({"action": "solid", "slot": 0, "form": {
            "profile": {"shape": "circle", "r": "1 m"}, "frame": {"origin": [0,0,0,"m"], "x": "x", "z": "z"}}});
        normalize(&mut o);
        assert!(o["form"].get("kind").is_none());
    }

    #[test]
    fn bare_numbers_need_an_explicit_unit() {
        let mut o = json!({"action": "thicken", "target": "wall:5", "thickness": 300});
        normalize(&mut o);
        assert_eq!(o["thickness"], 300);
        assert_eq!(o["target"]["by"], "id");
        let mut o = json!({"action": "thicken", "target": "wall:5", "thickness": 300, "unit": "mm"});
        normalize(&mut o);
        assert_eq!(o["thickness"], json!({"v": 300.0, "unit": "mm"}));
        assert!(o.get("unit").is_none());
    }

    #[test]
    fn geo_ops_takes_the_natural_call_of_a_member() {
        let op = json!({"action": "solid", "unit": "m", "name": "brace", "form": {
            "profile": {"shape": "circle", "r": "50 mm"},
            "frame": {"origin": "0,0,3", "x": [1, 0, 0], "z": [0, 0, 1]},
            "start": 0, "end": "4 m"}});
        let ops = super::super::geo_tools::geo_ops(0, &[op]).expect("natural args parse");
        assert_eq!(ops.len(), 1);
        // geo_args leaves a bare number alone; create_geometry then reads it as metres (geo_shape)
        let form = |r: J| json!({"action": "solid", "name": "brace", "form": {
            "profile": {"shape": "circle", "r": r}, "start": 0, "end": 4}});
        let bare = super::super::geo_tools::geo_ops(0, &[form(json!(0.05))]).expect("bare = metres");
        let unit = super::super::geo_tools::geo_ops(0, &[form(json!("0.05 m"))]).unwrap();
        assert_eq!(format!("{bare:?}"), format!("{unit:?}"));
        let bad = json!({"action": "solid", "form": {"profile": {"shape": "circle", "r": 50}}});
        assert!(super::super::geo_tools::geo_ops(0, &[bad]).is_err(), "no end: refused");
    }
}
