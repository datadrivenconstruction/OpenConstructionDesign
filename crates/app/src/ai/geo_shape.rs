// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Tolerant reading of `create_geometry` operations. Models send lengths as plain numbers or
//! "300 mm", points as `[x, y, z]` or "0 m, 0 m, 10 m", a form as a name with its fields beside
//! it, a missing frame. All of these have one meaning, so they are turned into the strict wire
//! shape of `ddc_ops::GeoOp` before it is parsed. Metres are the unit of a bare number.

use serde_json::{json, Map, Value as J};

pub use ddc_ai::catalog::{GEO_OP_EXAMPLE as EXAMPLE, GEO_OP_SHORT as SHORT};


/// Names a number or text stands for as a length in these operations.
const LEN_KEYS: [&str; 22] = [
    "w", "h", "b", "t", "tw", "tf", "r", "r_out", "r_in", "start", "end", "thickness", "span",
    "rise", "radius", "oculus", "lx", "ly", "width", "depth", "node", "glass",
];
const POINT_KEYS: [&str; 7] = ["origin", "axis_a", "axis_b", "center", "corner", "near", "to"];
/// Fields that belong to a form when it was named beside them.
const FORM_FIELDS: [&str; 12] = [
    "profile", "frame", "start", "end", "height", "length", "origin", "path", "sections", "axis_a",
    "axis_b", "angle",
];

fn unit_factor(u: &str) -> Option<f64> {
    match u.trim().to_lowercase().trim_end_matches('.') {
        "" | "m" | "meter" | "meters" | "metre" | "metres" => Some(1.0),
        "mm" => Some(0.001),
        "cm" => Some(0.01),
        "ft" | "feet" | "foot" => Some(0.3048),
        "in" | "inch" | "inches" => Some(0.0254),
        _ => None,
    }
}

/// A length in metres from a number, "12", "12 m" or "300 mm".
fn metres_of_text(s: &str) -> Option<f64> {
    let s = s.trim();
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+')))
        .map_or(s.len(), |(i, _)| i);
    let n: f64 = s[..end].parse().ok()?;
    Some(n * unit_factor(&s[end..])?)
}

/// Metres of a length given as a number, text or `{v, unit}`.
fn metres(x: &J) -> Option<f64> {
    match x {
        J::Number(n) => n.as_f64(),
        J::String(s) => metres_of_text(s),
        J::Object(o) => {
            let v = o.get("v")?.as_f64()?;
            let k = match o.get("unit") {
                None | Some(J::Null) => 1.0,
                Some(u) => unit_factor(u.as_str()?)?,
            };
            Some(v * k)
        }
        _ => None,
    }
}

fn len(m: f64) -> J {
    json!({"v": m, "unit": "m"})
}

/// Three or two coordinates (metres) of a point: `[x, y, z]`, `{x, y, z}` with plain, text or
/// `{v, unit}` components, or "x, y, z".
fn coords(x: &J) -> Option<Vec<f64>> {
    match x {
        J::Array(a) if a.len() == 2 || a.len() == 3 => a.iter().map(metres).collect(),
        J::String(s) => {
            let parts: Vec<&str> = if s.contains(',') {
                s.split(',').collect()
            } else {
                s.split_whitespace().collect()
            };
            if parts.len() == 2 || parts.len() == 3 {
                parts.iter().map(|p| metres_of_text(p)).collect()
            } else {
                None
            }
        }
        J::Object(o) if o.contains_key("x") && o.contains_key("y") => {
            let unit = o.get("unit").and_then(J::as_str).and_then(unit_factor).unwrap_or(1.0);
            let part = |k: &str| match o.get(k) {
                None => Some(0.0),
                // a bare component is in the point's own unit, a text or {v, unit} one in its own
                Some(J::Number(n)) => n.as_f64().map(|v| v * unit),
                Some(c) => metres(c),
            };
            let mut v = vec![part("x")?, part("y")?];
            if o.contains_key("z") {
                v.push(part("z")?);
            }
            Some(v)
        }
        _ => None,
    }
}

fn p3(x: &J) -> Option<J> {
    let c = coords(x)?;
    let z = c.get(2).copied().unwrap_or(0.0);
    Some(json!({"x": c[0], "y": c[1], "z": z, "unit": "m"}))
}

fn p2_or_p3(x: &J) -> Option<J> {
    let c = coords(x)?;
    Some(match c.get(2) {
        Some(z) => json!({"x": c[0], "y": c[1], "z": z, "unit": "m"}),
        None => json!({"x": c[0], "y": c[1], "unit": "m"}),
    })
}

fn dir(x: &J) -> Option<J> {
    let c = match x {
        J::Array(a) if a.len() == 3 => a.iter().map(J::as_f64).collect::<Option<Vec<_>>>()?,
        J::Object(o) => ["x", "y", "z"]
            .iter()
            .map(|k| o.get(*k).and_then(J::as_f64))
            .collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    Some(json!({"x": c[0], "y": c[1], "z": c[2]}))
}

/// The level of an operation: an id, "level:3" or a name.
fn level(x: &J) -> Option<J> {
    let id = match x {
        J::Number(n) => n.as_i64(),
        J::String(s) => s.trim().trim_start_matches("level:").parse().ok(),
        _ => None,
    };
    match (id, x) {
        (Some(id), _) => Some(json!({"by": "id", "cat": "level", "id": id})),
        (None, J::String(s)) => Some(json!({"by": "name", "cat": "level", "name": s})),
        _ => None,
    }
}

fn frame(o: &mut Map<String, J>) {
    if let Some(p) = o.get("origin").and_then(p3) {
        o.insert("origin".into(), p);
    }
    for (k, d) in [("x", json!({"x": 1, "y": 0, "z": 0})), ("z", json!({"x": 0, "y": 0, "z": 1}))] {
        let v = o.get(k).and_then(dir).unwrap_or(d);
        o.insert(k.into(), v);
    }
    o.entry("origin").or_insert_with(|| json!({"x": 0, "y": 0, "z": 0, "unit": "m"}));
}

/// A section name with its sizes in the strict shape: `circle` with a diameter, a round or
/// rectangular hollow section by outside size and wall.
fn profile(o: &mut Map<String, J>) {
    let shape = o.get("shape").and_then(J::as_str).unwrap_or_default().to_lowercase();
    let get = |o: &Map<String, J>, k: &str| o.get(k).and_then(metres);
    match shape.as_str() {
        "rectangle" | "square" => {
            o.insert("shape".into(), json!("rect"));
            let side = get(o, "side").or_else(|| get(o, "w")).or_else(|| get(o, "b"));
            if shape == "square" {
                if let Some(s) = side {
                    o.insert("w".into(), len(s));
                    o.insert("h".into(), len(s));
                }
            }
            o.remove("side");
        }
        "circle" | "round" => {
            o.insert("shape".into(), json!("circle"));
            if let Some(d) = get(o, "d").or_else(|| get(o, "diameter")) {
                o.insert("r".into(), len(d / 2.0));
            }
            o.remove("d");
            o.remove("diameter");
        }
        "chs" | "pipe" | "ring" if o.contains_key("d") || o.contains_key("diameter") => {
            let d = get(o, "d").or_else(|| get(o, "diameter")).unwrap_or(0.0);
            let t = get(o, "t").or_else(|| get(o, "wall")).unwrap_or(0.0);
            *o = Map::from_iter([
                ("shape".to_string(), json!("ring")),
                ("r_out".to_string(), len(d / 2.0)),
                ("r_in".to_string(), len((d / 2.0 - t).max(0.0))),
            ]);
        }
        "rhs" | "shs" | "tube" => {
            let w = get(o, "w").or_else(|| get(o, "b"));
            let h = get(o, "h").or_else(|| w);
            if let (Some(w), Some(h)) = (w, h) {
                o.insert("w".into(), len(w));
                o.insert("h".into(), len(h));
                o.remove("b");
            }
            o.insert("shape".into(), json!("tube"));
        }
        _ => {}
    }
}

/// A form object: its kind from what it holds, the height as an end, a frame at the origin.
fn form(o: &mut Map<String, J>) {
    if !o.contains_key("kind") {
        let kind = if o.contains_key("sections") {
            "loft"
        } else if o.contains_key("axis_a") {
            "revolve"
        } else if o.contains_key("path") && o.contains_key("profile") {
            "sweep"
        } else {
            "extrude"
        };
        o.insert("kind".into(), json!(kind));
    }
    if o.get("kind").and_then(J::as_str) != Some("extrude") {
        return;
    }
    // origin beside the form is the frame's origin
    if let Some(p) = o.remove("origin") {
        let f = o.entry("frame").or_insert_with(|| json!({}));
        if let Some(fo) = f.as_object_mut() {
            fo.entry("origin").or_insert(p);
        }
    }
    let h = ["height", "length"].iter().find_map(|k| o.remove(*k));
    if let Some(h) = h.as_ref().and_then(metres) {
        let start = o.get("start").and_then(metres).unwrap_or(0.0);
        o.insert("start".into(), len(start));
        o.insert("end".into(), len(start + h));
    }
    o.entry("start").or_insert_with(|| len(0.0));
    let f = o.entry("frame").or_insert_with(|| json!({}));
    if !f.is_object() {
        *f = json!({});
    }
}

fn walk(v: &mut J) {
    match v {
        J::Array(a) => a.iter_mut().for_each(walk),
        J::Object(o) => {
            for (k, x) in o.iter_mut() {
                match k.as_str() {
                    "level" => {
                        if let Some(l) = level(x) {
                            *x = l;
                        }
                    }
                    "profile" | "section" => {
                        if let Some(po) = x.as_object_mut() {
                            if po.contains_key("shape") {
                                profile(po);
                            }
                        }
                    }
                    "frame" => {
                        if let Some(fo) = x.as_object_mut() {
                            frame(fo);
                        }
                    }
                    "form" => {
                        if let Some(fo) = x.as_object_mut() {
                            form(fo);
                        }
                    }
                    "pts" => {
                        if let Some(a) = x.as_array_mut() {
                            for p in a {
                                if let Some(n) = p2_or_p3(p) {
                                    *p = n;
                                }
                            }
                        }
                    }
                    k if POINT_KEYS.contains(&k) => {
                        if let Some(n) = p3(x) {
                            *x = n;
                        }
                    }
                    k if LEN_KEYS.contains(&k) => {
                        // `{v, unit}` is already strict; a bare `{v}` is metres
                        let strict = x.get("unit").is_some_and(J::is_string);
                        if !strict && (!x.is_object() || x.get("v").is_some()) {
                            if let Some(m) = metres(x) {
                                *x = len(m);
                            }
                        }
                    }
                    _ => {}
                }
                walk(x);
            }
        }
        _ => {}
    }
}

/// An operation in the strict wire shape, whichever way it was written.
pub fn normalize(op: &mut J) {
    let Some(o) = op.as_object_mut() else {
        return;
    };
    // a pattern keeps its own strict shape (`geo_args` leaves it alone too)
    if o.get("op").and_then(J::as_str) == Some("pattern") {
        return;
    }
    // `form` named as text with its fields beside it
    if let Some(kind) = o.get("form").and_then(J::as_str).map(str::to_string) {
        let mut f = Map::new();
        f.insert("kind".into(), json!(kind));
        for k in FORM_FIELDS {
            if let Some(x) = o.remove(k) {
                f.insert(k.into(), x);
            }
        }
        o.insert("form".into(), J::Object(f));
    } else if !o.contains_key("form") && matches!(o.get("action").and_then(J::as_str), Some("solid" | "mass")) {
        // the form's fields straight on the operation
        let mut f = Map::new();
        for k in FORM_FIELDS {
            if let Some(x) = o.remove(k) {
                f.insert(k.into(), x);
            }
        }
        if f.contains_key("profile") || f.contains_key("sections") {
            o.insert("form".into(), J::Object(f));
        } else {
            o.extend(f);
        }
    }
    if o.get("action").and_then(J::as_str) == Some("boolean") {
        o.entry("keep_tool").or_insert(json!(false));
    }
    walk(op);
}

/// A refusal that names the expected form and shows one whole operation.
pub fn refusal(i: usize, e: &str) -> String {
    format!(
        "ops[{i}]: {e}. Expected one object per operation, e.g. {EXAMPLE}. Short forms are \
         read too: {SHORT}. Lengths are metres as numbers or text with a unit (\"300 mm\"), \
         points are [x, y, z] or {{x, y, z}}, form.kind is extrude, revolve, sweep or loft"
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_pattern_is_left_as_written() {
        let mut p = json!({"op": "pattern", "count": 3, "origin": [1, 2, 3], "w": "300 mm"});
        let before = p.clone();
        normalize(&mut p);
        assert_eq!(p, before);
        // any other operation is still read tolerantly
        let mut s = json!({"op": "solid", "origin": [0, 0, 0]});
        normalize(&mut s);
        assert_ne!(s, json!({"op": "solid", "origin": [0, 0, 0]}));
    }

    use super::*;
    use crate::ai::geo_tools::geo_ops;

    fn same(natural: J) {
        let want = geo_ops(1, &[serde_json::from_str(EXAMPLE).unwrap()]).unwrap();
        let got = geo_ops(1, &[natural.clone()]).unwrap_or_else(|e| panic!("{natural}: {e}"));
        assert_eq!(format!("{got:?}"), format!("{want:?}"), "{natural}");
    }

    fn col() -> J {
        json!({"action": "solid", "form": {"kind": "extrude",
            "profile": {"shape": "rect", "w": 0.3, "h": 0.3}, "start": 0, "end": 10},
            "name": "column"})
    }

    #[test]
    fn the_tool_description_shows_the_shapes_the_parser_reads() {
        let d = ddc_ai::catalog::find("create_geometry").unwrap().input.to_owned();
        let text = (d)().to_json().to_string();
        // the schema names the forms and points to the long text; the point syntax is in help
        assert!(text.contains("extrude") && text.contains("help field_notes"), "{text}");
        let mut loaded = std::collections::BTreeSet::new();
        let notes = ddc_ai::toolset::meta_call(
            ddc_ai::toolset::HELP,
            Some(&json!({"topic": "field_notes"})),
            ddc_ai::Mode::Propose,
            true,
            &mut loaded,
        )
        .text;
        assert!(notes.contains("[x, y, z]") && notes.contains("extrude"), "{notes}");
    }

    #[test]
    fn the_strict_example_is_itself() {
        let ex: J = serde_json::from_str(EXAMPLE).unwrap();
        let s = format!("{:?}", geo_ops(1, &[ex]).unwrap());
        assert!(s.contains("Extrude") && s.contains("10.0"), "{s}");
        assert!((metres_of_text("300 mm").unwrap() - 0.3).abs() < 1e-12);
        assert_eq!(metres_of_text("2.5"), Some(2.5));
        assert_eq!(metres_of_text("two"), None);
        assert!(geo_ops(1, &[serde_json::from_str(SHORT).unwrap()]).is_ok());
    }

    #[test]
    fn plain_numbers_text_and_arrays_mean_the_same_as_the_strict_shape() {
        same(col());
        same(json!({"action": "solid", "name": "column", "form": {
            "profile": {"shape": "rect", "w": "0.3 m", "h": "0.3"}, "start": "0 m", "end": "10 m"}}));
        // the form named as text, its fields beside it, a height instead of an end
        same(json!({"action": "solid", "form": "extrude", "name": "column",
            "profile": {"shape": "rect", "w": 0.3, "h": 0.3}, "height": 10}));
        // an origin as text or as an array, directions as arrays
        same(json!({"action": "solid", "name": "column", "form": {"kind": "extrude",
            "profile": {"shape": "rect", "w": 0.3, "h": 0.3},
            "frame": {"origin": "0 m, 0 m, 0 m", "x": [1, 0, 0], "z": [0, 0, 1]},
            "start": 0.0, "end": 10.0}}));
        same(json!({"action": "solid", "name": "column", "form": {"kind": "extrude",
            "profile": {"shape": "rect", "w": 0.3, "h": 0.3},
            "frame": {"origin": {"x": {"v": 0, "unit": "m"}, "y": 0, "z": 0}},
            "start": 0, "end": 10}}));
    }

    #[test]
    fn the_form_fields_may_sit_on_the_operation() {
        same(json!({"action": "solid", "name": "column",
            "profile": {"shape": "rect", "w": 0.3, "h": 0.3}, "origin": [0, 0, 0], "height": 10}));
    }

    #[test]
    fn a_hollow_section_by_diameter_and_wall_is_a_ring() {
        let op = json!({"action": "solid", "name": "diag", "form": {
            "profile": {"shape": "chs", "d": 0.508, "t": 0.02}, "height": 6}});
        let ops = geo_ops(1, &[op]).unwrap();
        let s = format!("{ops:?}");
        assert!(s.contains("Ring"), "{s}");
    }

    #[test]
    fn a_level_by_id_text_or_name() {
        for (l, want) in [(json!(2), "2"), (json!("level:2"), "2"), (json!("2"), "2")] {
            let mut op = col();
            op["level"] = l;
            let s = format!("{:?}", geo_ops(1, &[op]).unwrap());
            assert!(s.contains(want), "{s}");
        }
        let mut op = col();
        op["level"] = json!("Ground");
        assert!(format!("{:?}", geo_ops(1, &[op]).unwrap()).contains("Ground"));
    }

    #[test]
    fn a_refusal_names_the_expected_shape_with_an_example() {
        let e = geo_ops(1, &[json!({"action": "solid", "form": {"kind": "extrude", "wat": 1}})])
            .unwrap_err();
        assert!(e.contains("ops[0]") && e.contains("\"kind\":\"extrude\"") && e.contains("[x, y, z]"), "{e}");
    }
}
