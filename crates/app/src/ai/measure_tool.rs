// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `measure{a, b, kind}`: distance, clearance, height above the floor, length, volume, on the
//! scratch copy (the draft included). Every answer says where the number comes from:
//! `exact` for kernel solids (mesh, path, quantities) and points, `bbox` for an estimate from
//! bounding boxes. Lengths in metres, volumes in cubic metres.

use super::fmt::{self, Out};
use super::plan::{ElRef, IdOrNew};
use super::qspatial::{self, Space};
use super::session::{Core, Done};
use ddc_ai::CallCx;
use ddc_geom::gsolid::Form;
use ddc_geom::mesh::Mesh;
use ddc_geom::mesh_dist;
use ddc_geom::{v3, V3};
use serde_json::{json, Value as J};

pub const EXACT: &str = "exact";
pub const BBOX: &str = "bbox";

#[derive(Clone, Debug, PartialEq)]
pub enum Tgt {
    Solid { id: i64, level: i64 },
    Boxed { lo: [f64; 3], hi: [f64; 3], level: Option<i64> },
    Point([f64; 3]),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Meas {
    pub value: f64,
    pub unit: &'static str,
    pub source: &'static str,
    /// A point the number belongs to: the closest point of `a` for a clearance.
    pub at: Option<[f64; 3]>,
}

fn arr3(v: &J) -> Option<[f64; 3]> {
    let a = v.as_array().filter(|a| a.len() == 3)?;
    let mut o = [0.0; 3];
    for (k, x) in a.iter().enumerate() {
        o[k] = x.as_f64().filter(|x| x.is_finite())?;
    }
    Some(o)
}

fn resolve(c: &Core, v: &J, space: &mut Option<Space>) -> Result<Tgt, String> {
    if let Some(p) = arr3(v) {
        return Ok(Tgt::Point(p));
    }
    let text = v.as_str().ok_or("a and b are references such as solid:5 or [x, y, z] in metres")?;
    let r = ElRef::parse(text).map_err(|e| e.0)?;
    let id = match r.id {
        IdOrNew::Id(i) => i,
        IdOrNew::New(n) => *c.plan.slots.get(&n).ok_or_else(|| format!("no new element n{n}"))?,
    };
    if r.cat == "solid" {
        let s = c.ed.model.solid(id).map_err(|_| format!("no element solid:{id}"))?;
        return Ok(Tgt::Solid { id, level: s.level });
    }
    if r.cat == "mass" {
        return Err("mass elements cannot be measured yet".to_string());
    }
    let cat = super::sets::norm_cat(&r.cat)
        .ok_or_else(|| format!("unknown category: {}", fmt::text(&r.cat)))?;
    if space.is_none() {
        *space = Some(qspatial::space_of(&c.ed)?);
    }
    let el = space
        .as_ref()
        .and_then(|s| s.els.iter().find(|e| e.cat == cat && e.id == id))
        .ok_or_else(|| format!("no measurable body for {}:{id}", fmt::text(&r.cat)))?;
    Ok(Tgt::Boxed { lo: el.lo, hi: el.hi, level: Some(el.level) })
}

fn solid_box(c: &Core, id: i64) -> Result<([f64; 3], [f64; 3]), String> {
    let q = c
        .ed
        .model
        .solid_quantities(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("solid:{id} has no valid geometry"))?;
    Ok(([q.bbox[0], q.bbox[1], q.bbox[2]], [q.bbox[3], q.bbox[4], q.bbox[5]]))
}

fn bounds(c: &Core, t: &Tgt) -> Result<([f64; 3], [f64; 3]), String> {
    match t {
        Tgt::Solid { id, .. } => solid_box(c, *id),
        Tgt::Boxed { lo, hi, .. } => Ok((*lo, *hi)),
        Tgt::Point(p) => Ok((*p, *p)),
    }
}

/// Centre of mass of a closed mesh; the mean of its vertices when it has no volume.
pub fn centroid(m: &Mesh) -> V3 {
    let (mut vol, mut acc) = (0.0, v3(0.0, 0.0, 0.0));
    for t in &m.triangles {
        let p = |i: u32| m.positions.get(i as usize).copied();
        let (Some(a), Some(b), Some(c)) = (p(t[0]), p(t[1]), p(t[2])) else { continue };
        let v = a.dot(b.cross(c)) / 6.0;
        vol += v;
        acc = acc + (a + b + c) * (v / 4.0);
    }
    if vol.abs() > 1e-12 {
        return acc * (1.0 / vol);
    }
    let n = m.positions.len().max(1) as f64;
    m.positions.iter().fold(v3(0.0, 0.0, 0.0), |s, p| s + *p) * (1.0 / n)
}

fn point_of(c: &Core, t: &Tgt) -> Result<([f64; 3], &'static str), String> {
    match t {
        Tgt::Point(p) => Ok((*p, EXACT)),
        Tgt::Solid { id, .. } => {
            let m = c.ed.model.solid_mesh(*id).map_err(|e| e.to_string())?;
            let p = centroid(&m);
            Ok(([p.x, p.y, p.z], EXACT))
        }
        Tgt::Boxed { lo, hi, .. } => {
            let m = |k: usize| (lo[k] + hi[k]) / 2.0;
            Ok(([m(0), m(1), m(2)], BBOX))
        }
    }
}

fn box_gap(a: &([f64; 3], [f64; 3]), b: &([f64; 3], [f64; 3])) -> f64 {
    let mut s = 0.0;
    for k in 0..3 {
        let d = (b.0[k] - a.1[k]).max(a.0[k] - b.1[k]).max(0.0);
        s += d * d;
    }
    s.sqrt()
}

fn dist3(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn point_to_mesh(p: [f64; 3], m: &Mesh) -> Option<(f64, [f64; 3])> {
    let q = v3(p[0], p[1], p[2]);
    let mut best: Option<(f64, V3)> = None;
    for t in &m.triangles {
        let g = |i: u32| m.positions.get(i as usize).copied();
        let (Some(a), Some(b), Some(c)) = (g(t[0]), g(t[1]), g(t[2])) else { continue };
        let r = mesh_dist::closest_on_triangle(q, a, b, c);
        let d = (r - q).len();
        if best.is_none_or(|x| d < x.0) {
            best = Some((d, r));
        }
    }
    best.map(|(d, r)| (d, [r.x, r.y, r.z]))
}

fn clearance(c: &Core, a: &Tgt, b: &Tgt) -> Result<Meas, String> {
    let mesh = |t: &Tgt| match t {
        Tgt::Solid { id, .. } => c.ed.model.solid_mesh(*id).ok(),
        _ => None,
    };
    let (ma, mb) = (mesh(a), mesh(b));
    let exact = match (a, b, &ma, &mb) {
        (Tgt::Solid { .. }, Tgt::Solid { .. }, Some(x), Some(y)) => {
            mesh_dist::mesh_distance(x, y).map(|r| (r.dist, [r.a.x, r.a.y, r.a.z]))
        }
        (Tgt::Solid { .. }, Tgt::Point(p), Some(x), _)
        | (Tgt::Point(p), Tgt::Solid { .. }, _, Some(x)) => {
            point_to_mesh(*p, x).map(|(d, q)| (d, q))
        }
        (Tgt::Point(p), Tgt::Point(q), _, _) => Some((dist3(*p, *q), *p)),
        _ => None,
    };
    if let Some((d, at)) = exact {
        return Ok(Meas { value: d, unit: "m", source: EXACT, at: Some(at) });
    }
    let (ba, bb) = (bounds(c, a)?, bounds(c, b)?);
    Ok(Meas { value: box_gap(&ba, &bb), unit: "m", source: BBOX, at: None })
}

fn floor_of(c: &Core, t: &Tgt) -> f64 {
    let levels = c.ed.levels();
    let level = match t {
        Tgt::Solid { level, .. } => Some(*level),
        Tgt::Boxed { level, .. } => *level,
        Tgt::Point(_) => None,
    };
    level
        .and_then(|l| levels.iter().find(|x| x.0 == l).map(|x| x.2))
        .or_else(|| levels.iter().map(|x| x.2).reduce(f64::min))
        .unwrap_or(0.0)
}

/// Length of the member axis of a form: the path, or the extrusion depth.
fn form_length(f: &Form) -> Option<f64> {
    match f {
        Form::Extrusion { start, end, .. } => Some((end - start).abs()),
        Form::Sweep { path, .. } => Some(
            path.pts
                .windows(2)
                .enumerate()
                .map(|(i, w)| {
                    let b = path.bulges.get(i).copied().unwrap_or(0.0);
                    let chord = ((w[1].x - w[0].x).powi(2) + (w[1].y - w[0].y).powi(2)).sqrt();
                    let plan = if b.abs() < 1e-12 || chord < 1e-12 {
                        chord
                    } else {
                        let r = chord * (1.0 + b * b) / (4.0 * b.abs());
                        r * (4.0 * b.atan()).abs()
                    };
                    plan.hypot(w[1].z - w[0].z)
                })
                .sum(),
        ),
        _ => None,
    }
}

pub fn measure_of(c: &Core, kind: &str, a: &Tgt, b: Option<&Tgt>) -> Result<Meas, String> {
    let need_b = || b.ok_or_else(|| format!("{kind} needs both a and b"));
    match kind {
        "distance" => {
            let (pa, sa) = point_of(c, a)?;
            let (pb, sb) = point_of(c, need_b()?)?;
            let source = if sa == EXACT && sb == EXACT { EXACT } else { BBOX };
            Ok(Meas { value: dist3(pa, pb), unit: "m", source, at: Some(pa) })
        }
        "clearance" => clearance(c, a, need_b()?),
        "height" => {
            let (lo, _) = bounds(c, a)?;
            let source = if matches!(a, Tgt::Solid { .. } | Tgt::Point(_)) { EXACT } else { BBOX };
            Ok(Meas { value: lo[2] - floor_of(c, a), unit: "m", source, at: Some(lo) })
        }
        "length" => {
            if let Tgt::Solid { id, .. } = a {
                let spec = c.ed.model.solid(*id).map_err(|e| e.to_string())?;
                if let Some(l) = form_length(&spec.form) {
                    return Ok(Meas { value: l, unit: "m", source: EXACT, at: None });
                }
            }
            let (lo, hi) = bounds(c, a)?;
            let v = (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max);
            Ok(Meas { value: v, unit: "m", source: BBOX, at: None })
        }
        "volume" => match a {
            Tgt::Solid { id, .. } => {
                let q = c
                    .ed
                    .model
                    .solid_quantities(*id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("solid:{id} has no valid geometry"))?;
                Ok(Meas { value: q.net_volume, unit: "m3", source: EXACT, at: None })
            }
            Tgt::Point(_) => Err("a point has no volume".to_string()),
            Tgt::Boxed { lo, hi, .. } => {
                let v: f64 = (0..3).map(|k| hi[k] - lo[k]).product();
                Ok(Meas { value: v, unit: "m3", source: BBOX, at: None })
            }
        },
        other => Err(format!("unknown kind: {}", fmt::text(other))),
    }
}

fn round4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}

pub fn measure(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let kind = a["kind"].as_str().ok_or("kind must be text")?;
    let mut space = None;
    let ta = resolve(c, &a["a"], &mut space)?;
    let tb = match a.get("b").filter(|v| !v.is_null()) {
        Some(v) => Some(resolve(c, v, &mut space)?),
        None => None,
    };
    let m = measure_of(c, kind, &ta, tb.as_ref())?;
    let mut o = Out::new(c.max_chars);
    o.put("kind", &json!(kind));
    o.put("value", &json!(round4(m.value)));
    o.put("unit", &json!(m.unit));
    o.put("source", &json!(m.source));
    if m.source == BBOX {
        o.put("note", &json!("estimate from bounding boxes, not a kernel measurement"));
    }
    if let Some(p) = m.at {
        o.put("at", &json!(p.map(round4)));
    }
    c.audit("measure", None, Some(format!("{kind} {} {}", m.source, round4(m.value))), "ok");
    c.note_measured(m.value, m.source);
    Ok(o.finish().into())
}

fn r4(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

fn row_json(i: &ddc_model::MeasureInfo) -> J {
    let (v, state, reason) = i.value.parts();
    let mut o = json!({
        "id": i.id,
        "view": i.spec.view_code(),
        "kind": i.spec.kind.code(),
        "unit": i.spec.kind.unit(),
        "state": state,
    });
    if let Some(v) = v {
        o["value"] = json!(r4(v));
    } else {
        // no number: the reason says why, and the last number is kept apart, never as the value
        o["reason"] = json!(reason);
        if let Some(l) = i.last {
            o["last_value"] = json!(r4(l));
        }
    }
    if !i.spec.label.is_empty() {
        o["label"] = json!(i.spec.label);
    }
    o
}

/// Rows of `measure_list`: every saved measurement of the model.
pub fn list_rows(m: &ddc_model::Model) -> Result<Vec<J>, String> {
    Ok(m.all_measures().map_err(|e| e.to_string())?.iter().map(row_json).collect())
}

/// The answer of `measure_eval`: a saved measurement by id, or a kind over points. Both go
/// through the model's evaluator, so the number is the one the person sees in 3D.
pub fn eval_json(m: &ddc_model::Model, a: &J) -> Result<J, String> {
    use ddc_model::{MeasureAnchor, MeasureKind, MeasureSpec, ViewKey};
    if let Some(id) = a.get("measure_id").and_then(J::as_i64) {
        let i = m
            .measure_info(id)
            .map_err(|_| format!("no measurement {id}; measure_list shows them"))?;
        return Ok(row_json(&i));
    }
    let kind = a
        .get("kind")
        .and_then(J::as_str)
        .and_then(MeasureKind::parse)
        .ok_or("give measure_id, or kind with anchors")?;
    let pts: Vec<[f64; 3]> = a
        .get("anchors")
        .and_then(J::as_array)
        .ok_or("anchors is required with kind")?
        .iter()
        .map(|p| arr3(p).ok_or("each point is [x, y, z] in metres"))
        .collect::<Result<_, _>>()?;
    let anchors: Vec<MeasureAnchor> = pts.iter().map(|p| MeasureAnchor::Point(*p)).collect();
    let spec = MeasureSpec {
        kind,
        view: ViewKey::ThreeD,
        pins: m.measure_pins(&anchors).map_err(|e| e.to_string())?,
        label: String::new(),
    };
    let (v, state, reason) = m.eval_measure_spec(&spec).parts();
    let mut o = json!({"kind": kind.code(), "unit": kind.unit(), "state": state});
    match v {
        Some(v) => o["value"] = json!(r4(v)),
        None => o["reason"] = json!(reason),
    }
    Ok(o)
}

pub fn measure_list(c: &mut Core, _a: &J, _cx: &ddc_ai::CallCx<'_>) -> Result<Done, String> {
    let items = list_rows(&c.ed.model)?;
    let mut o = Out::new(c.max_chars);
    o.put("total", &json!(items.len()));
    o.list("measures", &items);
    Ok(o.finish().into())
}

pub fn measure_eval(c: &mut Core, a: &J, _cx: &ddc_ai::CallCx<'_>) -> Result<Done, String> {
    let r = eval_json(&c.ed.model, a)?;
    let mut o = Out::new(c.max_chars);
    if let Some(m) = r.as_object() {
        for (k, v) in m {
            o.put(k, v);
        }
    }
    c.audit("measure_eval", None, Some(r.to_string()), "ok");
    Ok(o.finish().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::mesh::Mesh;

    #[test]
    fn ai_measure_eval_equals_model() {
        use ddc_model::{MeasureAnchor as A, MeasureKind, MeasureSpec, Model, ViewKey};
        let mut m = Model::open(":memory:").unwrap();
        let pts = [[0.0, 0.0, 0.0], [3.0, 4.0, 0.0]];
        let anchors: Vec<A> = pts.iter().map(|p| A::Point(*p)).collect();
        let spec = MeasureSpec {
            kind: MeasureKind::Distance,
            view: ViewKey::ThreeD,
            pins: m.measure_pins(&anchors).unwrap(),
            label: String::new(),
        };
        let id = m.add_measure(&spec).unwrap();
        let want = m.measure_value(id).unwrap().parts().0.unwrap();
        assert!((want - 5.0).abs() < 1e-9);
        let by_id = eval_json(&m, &json!({"measure_id": id})).unwrap();
        let by_pts = eval_json(&m, &json!({"kind": "distance", "anchors": pts})).unwrap();
        for r in [&by_id, &by_pts] {
            assert_eq!(r["value"].as_f64().unwrap(), r4(want));
            assert_eq!((r["unit"].as_str(), r["state"].as_str()), (Some("m"), Some("live")));
        }
        assert_eq!(list_rows(&m).unwrap().len(), 1);
        // two coincident points are 0 m apart: a number
        let zero = eval_json(&m, &json!({"kind": "distance", "anchors": [[1.0, 1.0, 1.0], [1.0, 1.0, 1.0]]}))
            .unwrap();
        assert_eq!(zero["value"].as_f64(), Some(0.0));
        // no number (an angle at a point that coincides with its arm): a reason, no value field
        let bad = eval_json(&m, &json!({"kind": "angle", "anchors": [[1.0, 1.0, 1.0], [1.0, 1.0, 1.0], [2.0, 1.0, 1.0]]}))
            .unwrap();
        assert!(bad.get("value").is_none() && bad["reason"].as_str().is_some_and(|r| !r.is_empty()), "{bad}");
        assert!(eval_json(&m, &json!({"measure_id": 999})).is_err());
    }

    fn cube(x0: f64, x1: f64) -> Mesh {
        let p = |x: f64, y: f64, z: f64| v3(x, y, z);
        let positions = vec![
            p(x0, 0.0, 0.0),
            p(x1, 0.0, 0.0),
            p(x1, 1.0, 0.0),
            p(x0, 1.0, 0.0),
            p(x0, 0.0, 1.0),
            p(x1, 0.0, 1.0),
            p(x1, 1.0, 1.0),
            p(x0, 1.0, 1.0),
        ];
        let t = [
            [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7], [0, 1, 5], [0, 5, 4],
            [1, 2, 6], [1, 6, 5], [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7],
        ];
        Mesh { positions, triangles: t.to_vec(), face_of: vec![0; 12] }
    }

    #[test]
    fn centroid_of_a_closed_box_is_its_centre() {
        let c = centroid(&cube(1.0, 3.0));
        for (got, want) in [(c.x, 2.0), (c.y, 0.5), (c.z, 0.5)] {
            assert!((got - want).abs() < 1e-12);
        }
    }

    #[test]
    fn a_point_is_measured_to_the_mesh_exactly() {
        let (d, at) = point_to_mesh([3.5, 0.5, 0.5], &cube(0.0, 3.0)).unwrap();
        assert!((d - 0.5).abs() < 1e-12 && (at[0] - 3.0).abs() < 1e-12);
    }

    #[test]
    fn box_gap_is_zero_when_boxes_touch() {
        let a = ([0.0; 3], [1.0; 3]);
        assert_eq!(box_gap(&a, &([1.0, 0.0, 0.0], [2.0, 1.0, 1.0])), 0.0);
        assert!((box_gap(&a, &([2.0, 0.0, 0.0], [3.0, 1.0, 1.0])) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn sweep_length_follows_the_arc() {
        use ddc_geom::gsolid::{Path3, Section};
        let f = |b: f64| Form::Sweep {
            profile: Section::polygon(vec![]),
            path: Path3 { pts: vec![v3(0.0, 0.0, 0.0), v3(2.0, 0.0, 0.0)], bulges: vec![b], smooth: None },
        };
        assert!((form_length(&f(0.0)).unwrap() - 2.0).abs() < 1e-12);
        // bulge 1: a half circle of diameter 2
        assert!((form_length(&f(1.0)).unwrap() - std::f64::consts::PI).abs() < 1e-12);
    }
}
