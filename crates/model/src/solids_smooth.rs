//! JSON of a smooth sweep path (`"smooth"` next to `"path"` in a sweep form): segments, frame,
//! roll and the twist/scale laws. The polyline `"path"` stays as its plan trace for old readers.

use ddc_geom::frame3::{FrameMode, Law, Role, Seg, Smooth};
use ddc_geom::nurbs::Curve;
use ddc_geom::{v3, V3};
use serde_json::{json, Value};

use super::{arr, invalid, num};
use crate::Error;

fn p_json(p: V3) -> Value {
    json!([p.x, p.y, p.z])
}

fn p_of(v: &Value) -> Result<V3, Error> {
    let a = arr(v)?;
    if a.len() != 3 {
        return invalid("smooth: point needs [x, y, z]");
    }
    Ok(v3(num(&a[0])?, num(&a[1])?, num(&a[2])?))
}

fn law_json(l: Law) -> Value {
    match l {
        Law::Linear(a, b) => json!({"kind": "linear", "a": a, "b": b}),
        Law::Ease(a, b) => json!({"kind": "ease", "a": a, "b": b}),
    }
}

fn law_of(v: Option<&Value>, dflt: f64) -> Result<Law, Error> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(Law::constant(dflt));
    };
    if v.is_number() {
        return Ok(Law::constant(num(v)?));
    }
    let a = num(v.get("a").unwrap_or(&Value::Null))?;
    let b = num(v.get("b").unwrap_or(&Value::Null))?;
    match v.get("kind").and_then(Value::as_str).unwrap_or("linear") {
        "linear" => Ok(Law::Linear(a, b)),
        "ease" => Ok(Law::Ease(a, b)),
        k => invalid(format!("smooth: unknown law {k:?}")),
    }
}

fn seg_json(s: &Seg) -> Value {
    match s {
        Seg::Line { to } => json!({"kind": "line", "to": p_json(*to)}),
        Seg::Arc { via, to } => json!({"kind": "arc", "via": p_json(*via), "to": p_json(*to)}),
        Seg::Helix {
            center,
            axis,
            angle,
            rise,
        } => json!({
            "kind": "helix",
            "center": p_json(*center),
            "axis": p_json(*axis),
            "angle": angle,
            "rise": rise,
        }),
        Seg::Spline { via, to } => json!({
            "kind": "spline",
            "via": via.iter().map(|p| p_json(*p)).collect::<Vec<_>>(),
            "to": p_json(*to),
        }),
        Seg::Nurbs(c) => json!({
            "kind": "nurbs",
            "degree": c.degree(),
            "knots": c.knots(),
            "ctrl": c.ctrl().iter().map(|p| p_json(*p)).collect::<Vec<_>>(),
            "weights": c.weights(),
        }),
    }
}

fn seg_of(v: &Value) -> Result<Seg, Error> {
    let g = |k: &str| v.get(k).unwrap_or(&Value::Null);
    Ok(match g("kind").as_str().unwrap_or("") {
        "line" => Seg::Line { to: p_of(g("to"))? },
        "arc" => Seg::Arc {
            via: p_of(g("via"))?,
            to: p_of(g("to"))?,
        },
        "helix" => Seg::Helix {
            center: p_of(g("center"))?,
            axis: p_of(g("axis"))?,
            angle: num(g("angle"))?,
            rise: num(g("rise"))?,
        },
        "spline" => Seg::Spline {
            via: arr(g("via"))?.iter().map(p_of).collect::<Result<_, _>>()?,
            to: p_of(g("to"))?,
        },
        "nurbs" => {
            let degree = g("degree")
                .as_u64()
                .filter(|d| (1..=9).contains(d))
                .ok_or_else(|| Error::Invalid("smooth: nurbs degree 1..9".into()))?;
            let knots = arr(g("knots"))?.iter().map(num).collect::<Result<_, _>>()?;
            let ctrl = arr(g("ctrl"))?.iter().map(p_of).collect::<Result<_, _>>()?;
            let weights = arr(g("weights"))?.iter().map(num).collect::<Result<_, _>>()?;
            let c = Curve::new(degree as usize, knots, ctrl, weights)
                .map_err(|e| Error::Invalid(format!("smooth: nurbs {e:?}")))?;
            Seg::Nurbs(c)
        }
        k => return invalid(format!("smooth: unknown segment {k:?}")),
    })
}

pub(crate) fn smooth_json(s: &Smooth) -> Value {
    let frame = match s.frame {
        FrameMode::Rmf => json!("rmf"),
        FrameMode::Fixed(up) => json!({"fixed": p_json(up)}),
    };
    json!({
        "start": p_json(s.start),
        "segs": s.segs.iter().map(seg_json).collect::<Vec<_>>(),
        "frame": frame,
        "roll": s.roll,
        "twist": law_json(s.twist),
        "scale": law_json(s.scale),
        "role": s.role.name(),
    })
}

pub(crate) fn smooth_of(v: &Value) -> Result<Smooth, Error> {
    let g = |k: &str| v.get(k).unwrap_or(&Value::Null);
    let segs = arr(g("segs"))?.iter().map(seg_of).collect::<Result<Vec<_>, _>>()?;
    let mut s = Smooth::new(p_of(g("start"))?, segs);
    s.frame = match g("frame") {
        Value::Null => FrameMode::Rmf,
        Value::String(m) if m == "rmf" => FrameMode::Rmf,
        f => match f.get("fixed") {
            Some(up) => FrameMode::Fixed(p_of(up)?),
            None => return invalid(format!("smooth: frame {f} is not \"rmf\" or {{fixed}}")),
        },
    };
    s.roll = match g("roll") {
        Value::Null => 0.0,
        r => num(r)?,
    };
    s.twist = law_of(v.get("twist"), 0.0)?;
    s.scale = law_of(v.get("scale"), 1.0)?;
    s.role = match g("role").as_str() {
        None => Role::Generic,
        Some(r) => Role::parse(r).ok_or_else(|| Error::Invalid(format!("smooth: role {r:?}")))?,
    };
    Ok(s)
}
