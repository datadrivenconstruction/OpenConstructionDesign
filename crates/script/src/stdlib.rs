//! The standard library of geometry scripts: vectors and transforms, units, params, catalog
//! profiles, forms, patterns, parts, groups, components, booleans, constraints, measures.
//! Every refusal names the function first (`extrude: ...`) so the error points at the call.

use std::cell::Cell;
use std::f64::consts::PI;

use ddc_geom::member::{catalog, Shape as GShape};
use ddc_ops::{BoolKind, Len, Profile, P2};
use mlua::{AppDataRefMut, FromLua, Function, Lua, Table, Value, Variadic};
use serde_json::Value as J;

use crate::emit::Builder;
use crate::units::{dim_of, param_si, parse_angle, parse_len};
use crate::values::{
    err, num, Comp, FormV, GroupH, LResult, Mat, PartH, PathV, Prof, Shape, UP, V3,
};
use crate::Measure;

/// Tolerance of constraints: 1 mm, the node rule of AI_GEOMETRY_PLAN.md 3.4.
const TOL: f64 = 0.001;
const ANG_TOL: f64 = 1e-4;

pub(crate) fn builder(lua: &Lua) -> LResult<AppDataRefMut<'_, Builder>> {
    lua.app_data_mut::<Builder>()
        .ok_or_else(|| mlua::Error::RuntimeError("internal: no builder".into()))
}

fn ok<T>(r: Result<T, String>) -> LResult<T> {
    r.map_err(mlua::Error::RuntimeError)
}

fn named(f: &str, r: Result<f64, String>) -> LResult<f64> {
    r.map_err(|e| mlua::Error::RuntimeError(format!("{f}: {e}")))
}

fn text_q(f: &str, v: &Value, parse: fn(&str) -> Result<f64, String>) -> LResult<f64> {
    match v {
        Value::String(s) => named(f, parse(&s.to_str()?)),
        Value::Integer(_) | Value::Number(_) => err(format!(
            "{f}: needs text with a unit, like {f}(\"1200 mm\")"
        )),
        other => err(format!(
            "{f}: needs text with a unit, got {}",
            other.type_name()
        )),
    }
}

fn pos(f: &str, what: &str, v: f64) -> LResult<Len> {
    if v.is_finite() && v > 0.0 {
        Ok(Len::m(v))
    } else {
        err(format!(
            "{f}: {what} must be a positive length in metres, got {v}"
        ))
    }
}

pub(crate) fn install(lua: &Lua) -> LResult<()> {
    let g = lua.globals();
    let f = |name: &str, func: Function| g.set(name, func);

    f(
        "vec3",
        lua.create_function(|_, (x, y, z): (Option<f64>, Option<f64>, Option<f64>)| {
            let v = V3::new(x.unwrap_or(0.0), y.unwrap_or(0.0), z.unwrap_or(0.0));
            if !v.finite() {
                return err("vec3: components must be finite numbers");
            }
            Ok(v)
        })?,
    )?;
    f("identity", lua.create_function(|_, ()| Ok(Mat::ID))?)?;
    f(
        "translate",
        lua.create_function(|_, v: V3| Ok(Mat::translate(v)))?,
    )?;
    f(
        "rot_x",
        lua.create_function(|_, a: f64| Ok(Mat::rot(V3::new(1.0, 0.0, 0.0), a)))?,
    )?;
    f(
        "rot_y",
        lua.create_function(|_, a: f64| Ok(Mat::rot(V3::new(0.0, 1.0, 0.0), a)))?,
    )?;
    f(
        "rot_z",
        lua.create_function(|_, a: f64| Ok(Mat::rot(UP, a)))?,
    )?;
    f(
        "rot",
        lua.create_function(|_, (k, a): (V3, f64)| match k.unit() {
            Some(u) => Ok(Mat::rot(u, a)),
            None => err("rot: axis is a zero vector"),
        })?,
    )?;

    f(
        "len",
        lua.create_function(|_, v: Value| text_q("len", &v, parse_len))?,
    )?;
    f(
        "angle",
        lua.create_function(|_, v: Value| text_q("angle", &v, parse_angle))?,
    )?;
    f("mm", lua.create_function(|_, v: f64| Ok(v / 1000.0))?)?;
    f("cm", lua.create_function(|_, v: f64| Ok(v / 100.0))?)?;
    f("m", lua.create_function(|_, v: f64| Ok(v))?)?;
    f("deg", lua.create_function(|_, v: f64| Ok(v * PI / 180.0))?)?;

    f("param", lua.create_function(param)?)?;
    g.set("profile", profiles(lua)?)?;

    f("extrude", lua.create_function(extrude)?)?;
    f(
        "polyline",
        lua.create_function(|_, (pts, closed): (Vec<V3>, Option<bool>)| {
            path_of("polyline", pts, None, closed.unwrap_or(false))
        })?,
    )?;
    f(
        "arc",
        lua.create_function(|_, (c, r, a0, a1): (V3, f64, f64, f64)| arc("arc", c, r, a0, a1))?,
    )?;
    f(
        "sweep",
        lua.create_function(|lua, (p, prof, t): (Value, Prof, Option<Table>)| {
            let path = match &p {
                Value::UserData(u) if u.is::<PathV>() => PathV::from_lua(p, lua)?,
                Value::Table(_) => path_of("sweep", Vec::<V3>::from_lua(p, lua)?, None, false)?,
                _ => return err("sweep: path must be polyline(...), arc(...), curve.*{...} or a list of vec3"),
            };
            let o = crate::blocks::Opts::new("sweep", t, lua)?;
            let twist = o.ang("twist", 0.0)?;
            let scale = o.num("scale", 1.0, 0.01, 100.0)?;
            o.done()?;
            if (twist != 0.0 || scale != 1.0) && path.closed {
                return err("sweep: twist and scale need an open path (a closed loop has no end)");
            }
            Ok(FormV(Shape::Sweep { prof: prof.0, path, twist, scale }))
        })?,
    )?;
    f(
        "revolve",
        lua.create_function(|_, (prof, c, r, a): (Prof, V3, f64, f64)| {
            let path = arc("revolve", c, r, 0.0, a)?;
            Ok(FormV(Shape::Sweep { prof: prof.0, path, twist: 0.0, scale: 1.0 }))
        })?,
    )?;
    f("loft", lua.create_function(loft)?)?;
    f(
        "transform",
        lua.create_function(|_, (s, m): (FormV, Mat)| ok(s.transform(&m)))?,
    )?;
    f(
        "pattern",
        lua.create_function(|lua, (s, n, m): (FormV, i64, Mat)| repeat(lua, &s, n, &m))?,
    )?;
    f(
        "pattern_linear",
        lua.create_function(|lua, (s, n, d): (FormV, i64, V3)| {
            repeat(lua, &s, n, &Mat::translate(d))
        })?,
    )?;
    f(
        "pattern_radial",
        lua.create_function(|lua, (s, n, total, c): (FormV, i64, f64, Option<V3>)| {
            let c = c.unwrap_or(V3::new(0.0, 0.0, 0.0));
            let full = total.abs() >= 2.0 * PI - 1e-9;
            let step = if full || n < 2 {
                total / n.max(1) as f64
            } else {
                total / (n - 1) as f64
            };
            let m = Mat::translate(c)
                .then(&Mat::rot(UP, step))
                .then(&Mat::translate(c.scale(-1.0)));
            repeat(lua, &s, n, &m)
        })?,
    )?;

    f("part", lua.create_function(part)?)?;
    f("member", lua.create_function(member)?)?;
    f(
        "parts",
        lua.create_function(
            |lua, (name, forms, mat): (String, Vec<FormV>, Option<String>)| {
                forms
                    .iter()
                    .map(|s| make_part(lua, &name, s, mat.clone()))
                    .collect::<LResult<Vec<_>>>()
            },
        )?,
    )?;
    f("group", lua.create_function(group)?)?;
    f("define_component", lua.create_function(define_component)?)?;
    f("place", lua.create_function(place)?)?;
    f(
        "boolean",
        lua.create_function(|lua, (kind, a, t): (String, PartH, FormV)| {
            let k = match kind.as_str() {
                "cut" => BoolKind::Cut,
                "union" => BoolKind::Union,
                "intersect" => BoolKind::Intersect,
                other => {
                    return err(format!(
                        "boolean: kind `{other}` is not cut, union or intersect"
                    ))
                }
            };
            ok(builder(lua)?
                .boolean(k, a.idx, &t)
                .map_err(|e| format!("boolean: {e}")))
        })?,
    )?;
    f("constrain", lua.create_function(constrain)?)?;
    f("measure", lua.create_function(measure)?)?;
    f(
        "assert",
        lua.create_function(|_, (c, msg): (Value, Option<String>)| {
            if matches!(c, Value::Nil | Value::Boolean(false)) {
                return err(format!(
                    "assert: {}",
                    msg.unwrap_or_else(|| "assertion failed".into())
                ));
            }
            Ok(c)
        })?,
    )?;
    f("audit", lua.create_function(crate::audit::audit)?)?;
    crate::blocks::install(lua)
}

// ---- params ----

fn to_json(v: &Value) -> LResult<J> {
    Ok(match v {
        Value::Nil => J::Null,
        Value::Boolean(b) => J::Bool(*b),
        Value::Integer(i) => J::from(*i),
        Value::Number(n) => serde_json::Number::from_f64(*n).map_or(J::Null, J::Number),
        Value::String(s) => J::String(s.to_str()?.to_string()),
        other => {
            return err(format!(
                "param: default must be a number, text or flag, got {}",
                other.type_name()
            ))
        }
    })
}

/// `param(name, default[, {min=, max=}])`: the caller's value or the default, in SI.
fn param(lua: &Lua, (name, default, range): (String, Value, Option<Table>)) -> LResult<Value> {
    if name.trim().is_empty() || name.len() > 60 {
        return err("param: name must be 1-60 characters");
    }
    let dj = to_json(&default)?;
    let unit = match &dj {
        J::String(s) => dim_of(s),
        _ => None,
    };
    let given = builder(lua)?.params_in.get(&name).cloned();
    let value: J = match (unit, &dj, given) {
        (Some(u), J::String(s), g) => {
            let def = if u == "m" {
                parse_len(s)
            } else {
                parse_angle(s)
            };
            let v = match g {
                Some(x) => param_si(&x, u).map_err(|e| format!("param `{name}`: {e}")),
                None => def,
            };
            J::from(ok(v)?)
        }
        (None, d, Some(g)) => {
            let same = matches!(
                (d, &g),
                (J::Number(_), J::Number(_))
                    | (J::Bool(_), J::Bool(_))
                    | (J::String(_), J::String(_))
            );
            if !same {
                return err(format!(
                    "param `{name}`: expected a value like {d}, got {g}"
                ));
            }
            if d.is_i64() && !g.is_i64() {
                return err(format!("param `{name}`: expected a whole number, got {g}"));
            }
            g
        }
        // a unit comes only from a text default
        (None, d, None) | (Some(_), d, _) => d.clone(),
    };
    if let (Some(r), Some(x)) = (range, value.as_f64()) {
        let bound = |k: &str| -> LResult<Option<f64>> {
            match r.get::<Value>(k)? {
                Value::Nil => Ok(None),
                Value::String(s) => {
                    let s = s.to_str()?.to_string();
                    let u = unit.unwrap_or("m");
                    ok(if u == "m" {
                        parse_len(&s)
                    } else {
                        parse_angle(&s)
                    })
                    .map(Some)
                }
                v => {
                    if unit.is_some() && num(&v).is_some() {
                        return err(format!(
                            "param `{name}`: the value has a unit, so `{k}` needs one too, \
                             like \"500 mm\" or \"30 deg\""
                        ));
                    }
                    Ok(num(&v))
                }
            }
        };
        if let Some(lo) = bound("min")? {
            if x < lo - 1e-12 {
                return err(format!("param `{name}`: {x} is below the minimum {lo}"));
            }
        }
        if let Some(hi) = bound("max")? {
            if x > hi + 1e-12 {
                return err(format!("param `{name}`: {x} is above the maximum {hi}"));
            }
        }
    }
    let mut b = builder(lua)?;
    if !b.params.iter().any(|p| p.name == name) {
        b.params.push(crate::ParamOut {
            name,
            default: dj,
            unit,
            value: value.clone(),
        });
    }
    drop(b);
    Ok(match value {
        J::Bool(x) => Value::Boolean(x),
        J::String(s) => Value::String(lua.create_string(&s)?),
        J::Number(n) if n.is_i64() => Value::Integer(n.as_i64().unwrap_or(0)),
        J::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        _ => Value::Nil,
    })
}

// ---- profiles ----

pub(crate) fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_uppercase()
}

pub(crate) fn from_catalog(s: &GShape) -> Option<Profile> {
    Some(match *s {
        GShape::Rect { b, h } => Profile::Rect {
            w: Len::m(b),
            h: Len::m(h),
        },
        GShape::Circle { d } => Profile::Circle { r: Len::m(d / 2.0) },
        GShape::I { h, b, tw, tf } => Profile::I {
            h: Len::m(h),
            b: Len::m(b),
            tw: Len::m(tw),
            tf: Len::m(tf),
        },
        GShape::Channel { h, b, tw, tf } => Profile::C {
            h: Len::m(h),
            b: Len::m(b),
            tw: Len::m(tw),
            tf: Len::m(tf),
        },
        GShape::Tee { h, b, tw, tf } => Profile::T {
            h: Len::m(h),
            b: Len::m(b),
            tw: Len::m(tw),
            tf: Len::m(tf),
        },
        GShape::Angle { h, b, t } => Profile::L {
            h: Len::m(h),
            b: Len::m(b),
            t: Len::m(t),
        },
        GShape::Pipe { d, t } => Profile::Ring {
            r_out: Len::m(d / 2.0),
            r_in: Len::m(d / 2.0 - t),
        },
        GShape::Tube { h, b, t } => Profile::Tube {
            w: Len::m(b),
            h: Len::m(h),
            t: Len::m(t),
        },
        _ => return None,
    })
}

fn profiles(lua: &Lua) -> LResult<Table> {
    let t = lua.create_table()?;
    t.set(
        "rect",
        lua.create_function(|_, (w, h): (f64, f64)| {
            Ok(Prof(Profile::Rect {
                w: pos("profile.rect", "w", w)?,
                h: pos("profile.rect", "h", h)?,
            }))
        })?,
    )?;
    t.set(
        "circle",
        lua.create_function(|_, r: f64| {
            Ok(Prof(Profile::Circle {
                r: pos("profile.circle", "r", r)?,
            }))
        })?,
    )?;
    t.set(
        "ring",
        lua.create_function(|_, (ro, ri): (f64, f64)| {
            if !(ri > 0.0 && ri < ro) {
                return err("profile.ring: need 0 < r_in < r_out");
            }
            Ok(Prof(Profile::Ring {
                r_out: pos("profile.ring", "r_out", ro)?,
                r_in: Len::m(ri),
            }))
        })?,
    )?;
    t.set(
        "chs",
        lua.create_function(|_, (d, th): (f64, f64)| {
            if !(th > 0.0 && 2.0 * th < d) {
                return err("profile.chs: need 0 < t < d/2");
            }
            Ok(Prof(Profile::Ring {
                r_out: pos("profile.chs", "d", d / 2.0)?,
                r_in: Len::m(d / 2.0 - th),
            }))
        })?,
    )?;
    let tube = |f: &'static str| {
        move |_: &Lua, (h, b, th): (f64, f64, f64)| {
            if !(th > 0.0 && 2.0 * th < h.min(b)) {
                return err(format!("{f}: need 0 < t < half the smaller side"));
            }
            Ok(Prof(Profile::Tube {
                w: pos(f, "b", b)?,
                h: pos(f, "h", h)?,
                t: Len::m(th),
            }))
        }
    };
    t.set("rhs", lua.create_function(tube("profile.rhs"))?)?;
    let shs = tube("profile.shs");
    t.set(
        "shs",
        lua.create_function(move |l, (b, th): (f64, f64)| shs(l, (b, b, th)))?,
    )?;
    for (k, mk) in [("i", 0u8), ("h", 1), ("c", 2), ("t", 3)] {
        let f = format!("profile.{k}");
        t.set(
            k,
            lua.create_function(move |_, (h, b, tw, tf): (f64, f64, f64, f64)| {
                let (h, b, tw, tf) = (
                    pos(&f, "h", h)?,
                    pos(&f, "b", b)?,
                    pos(&f, "tw", tw)?,
                    pos(&f, "tf", tf)?,
                );
                if tw.v >= b.v || 2.0 * tf.v >= h.v {
                    return err(format!(
                        "{f}: web must be thinner than b and flanges thinner than h/2"
                    ));
                }
                Ok(Prof(match mk {
                    0 => Profile::I { h, b, tw, tf },
                    1 => Profile::H { h, b, tw, tf },
                    2 => Profile::C { h, b, tw, tf },
                    _ => Profile::T { h, b, tw, tf },
                }))
            })?,
        )?;
    }
    t.set(
        "l",
        lua.create_function(|_, (h, b, th): (f64, f64, f64)| {
            let f = "profile.l";
            Ok(Prof(Profile::L {
                h: pos(f, "h", h)?,
                b: pos(f, "b", b)?,
                t: pos(f, "t", th)?,
            }))
        })?,
    )?;
    t.set(
        "polygon",
        lua.create_function(|_, pts: Vec<Vec<f64>>| {
            if pts.len() < 3
                || pts
                    .iter()
                    .any(|p| p.len() != 2 || !p.iter().all(|x| x.is_finite()))
            {
                return err("profile.polygon: need 3 or more points {x, y}");
            }
            if pts.len() > MAX_INPUT_POINTS {
                return err(format!(
                    "profile.polygon: {} points; at most {MAX_INPUT_POINTS}",
                    pts.len()
                ));
            }
            Ok(Prof(Profile::Polygon {
                pts: pts.iter().map(|p| P2::m(p[0], p[1])).collect(),
            }))
        })?,
    )?;
    t.set(
        "named",
        lua.create_function(|_, name: String| {
            let all = catalog();
            match all
                .iter()
                .find(|(n, _)| norm(n) == norm(&name))
                .and_then(|(_, s)| from_catalog(s))
            {
                Some(p) => Ok(Prof(p)),
                None => {
                    let names: Vec<&str> = all.iter().map(|(n, _)| *n).collect();
                    err(format!(
                        "profile.named: `{name}` is not in the catalog ({})",
                        names.join(", ")
                    ))
                }
            }
        })?,
    )?;
    t.set(
        "list",
        lua.create_function(|_, ()| {
            Ok(catalog()
                .into_iter()
                .map(|(n, _)| n.to_string())
                .collect::<Vec<_>>())
        })?,
    )?;
    Ok(t)
}

// ---- forms ----

/// Member from `a` to `b`; profile y points up unless the member is vertical, then +x is x.
fn extrude(_: &Lua, (prof, a, b, roll): (Prof, V3, V3, Option<f64>)) -> LResult<FormV> {
    let d = b.sub(a);
    let Some(z) = d.unit() else {
        return err("extrude: start and end are the same point");
    };
    let side = UP.cross(z);
    let mut x = side.unit().unwrap_or(V3::new(1.0, 0.0, 0.0));
    if let Some(r) = roll {
        let (s, c) = r.sin_cos();
        x = x.scale(c).add(z.cross(x).scale(s));
    }
    Ok(FormV(Shape::Extrude {
        prof: prof.0,
        o: a,
        x,
        z,
        len: d.len(),
    }))
}

fn path_of(f: &str, pts: Vec<V3>, bulges: Option<Vec<f64>>, closed: bool) -> LResult<PathV> {
    if pts.len() < 2 || pts.iter().any(|p| !p.finite()) {
        return err(format!("{f}: need 2 or more finite points"));
    }
    if pts.len() > MAX_INPUT_POINTS {
        return err(format!("{f}: {} points; at most {MAX_INPUT_POINTS}", pts.len()));
    }
    let segs = if closed { pts.len() } else { pts.len() - 1 };
    let bulges = bulges.unwrap_or_else(|| vec![0.0; segs]);
    if bulges.len() != segs {
        return err(format!("{f}: {} bulges for {segs} segments", bulges.len()));
    }
    Ok(PathV {
        pts,
        bulges,
        closed,
    })
}

/// Arc in the horizontal plane through `c`, from angle `a0` to `a1`; a full turn is closed.
fn arc(f: &str, c: V3, r: f64, a0: f64, a1: f64) -> LResult<PathV> {
    if !(r.is_finite() && r > 0.0) {
        return err(format!("{f}: radius must be positive"));
    }
    let at = |a: f64| V3::new(c.x + r * a.cos(), c.y + r * a.sin(), c.z);
    let sweep = a1 - a0;
    if sweep.abs() < 1e-9 {
        return err(format!("{f}: zero angle"));
    }
    if sweep.abs() >= 2.0 * PI - 1e-9 {
        let s = sweep.signum();
        return Ok(PathV {
            pts: vec![at(a0), at(a0 + s * PI)],
            bulges: vec![s, s],
            closed: true,
        });
    }
    Ok(PathV {
        pts: vec![at(a0), at(a1)],
        bulges: vec![(sweep / 4.0).tan()],
        closed: false,
    })
}

fn loft(lua: &Lua, secs: Vec<Table>) -> LResult<FormV> {
    if secs.len() < 2 {
        return err("loft: need 2 or more sections {profile, at}");
    }
    if secs.len() > MAX_INPUT_POINTS {
        return err(format!("loft: {} sections; at most {MAX_INPUT_POINTS}", secs.len()));
    }
    let mut out = Vec::with_capacity(secs.len());
    for s in secs {
        let p: Value = s.get("profile").or_else(|_| s.get(1))?;
        let p = if p.is_nil() { s.get(1)? } else { p };
        let at: Value = s.get("at")?;
        let at = if at.is_nil() { s.get(2)? } else { at };
        out.push((Prof::from_lua(p, lua)?.0, V3::from_lua(at, lua)?));
    }
    Ok(FormV(Shape::Loft { secs: out }))
}

/// Points all pattern copies of a whole run may hold together (Rust memory, outside the Lua
/// limit).
pub(crate) const MAX_COPY_POINTS: usize = 4_000_000;
/// Points one polygon, path or loft may be given.
const MAX_INPUT_POINTS: usize = 100_000;

fn repeat(lua: &Lua, s: &FormV, n: i64, m: &Mat) -> LResult<Vec<FormV>> {
    if !(1..=100_000).contains(&n) {
        return err("pattern: count must be 1..100000");
    }
    let cost = s.weight().saturating_mul(n as usize);
    let mut b = builder(lua)?;
    let total = b.copy_points.saturating_add(cost);
    if total > MAX_COPY_POINTS {
        return err(format!(
            "pattern: {n} copies of a form with {} points need {cost} points, {} already \
             copied in this run; at most {MAX_COPY_POINTS} per run",
            s.weight(),
            b.copy_points
        ));
    }
    b.copy_points = total;
    drop(b);
    let mut out = Vec::with_capacity(n as usize);
    let mut cur = s.clone();
    for _ in 0..n {
        let next = ok(cur.transform(m))?;
        out.push(std::mem::replace(&mut cur, next));
    }
    Ok(out)
}

// ---- parts, groups, components ----

fn make_part(lua: &Lua, name: &str, s: &FormV, mat: Option<String>) -> LResult<PartH> {
    let idx = ok(builder(lua)?
        .part(name, s, mat, None)
        .map_err(|e| format!("part: {e}")))?;
    let (a, b) = s.ends();
    Ok(PartH {
        idx,
        a,
        b,
        len: s.length(),
    })
}

/// `part(name, form[, material])` or `part(form[, material])`.
fn part(lua: &Lua, args: Variadic<Value>) -> LResult<PartH> {
    let mut it = args.into_iter();
    let first = it.next().unwrap_or(Value::Nil);
    let (name, form) = match first {
        Value::String(s) => (s.to_str()?.to_string(), it.next().unwrap_or(Value::Nil)),
        other => ("part".to_string(), other),
    };
    let form = FormV::from_lua(form, lua)
        .map_err(|_| mlua::Error::RuntimeError("part: second argument must be a form".into()))?;
    let po = crate::geo_build::part_opts(lua, it.next().unwrap_or(Value::Nil))?;
    let idx = ok(builder(lua)?
        .part_in(&name, &form, po.material, po.ifc, po.level)
        .map_err(|e| format!("part: {e}")))?;
    let (a, b) = form.ends();
    Ok(PartH { idx, a, b, len: form.length() })
}

/// `member(name, a, b, section[, role])`: a straight beam or brace row of a catalogue section
/// ("CHS 168.3x8"); role is "beam" or "brace" (default).
fn member(
    lua: &Lua,
    (name, a, b, section, role): (String, V3, V3, String, Option<String>),
) -> LResult<PartH> {
    let role = match role.as_deref() {
        None | Some("brace") => ddc_ops::MemberRole::Brace,
        Some("beam") => ddc_ops::MemberRole::Beam,
        Some(r) => return err(format!("member: role `{r}` is beam or brace")),
    };
    let idx = ok(builder(lua)?
        .member(&name, a, b, &section, role, 0.1)
        .map_err(|e| format!("member: {e}")))?;
    Ok(PartH { idx, a, b, len: b.sub(a).len() })
}

/// Deepest list of lists `group` follows; a list that holds itself would never end.
const MAX_NEST: usize = 32;

fn collect(lua: &Lua, v: Value, out: &mut Vec<usize>, depth: usize) -> LResult<()> {
    if depth > MAX_NEST {
        return err("group: lists are nested too deeply; does a list contain itself?");
    }
    match &v {
        Value::UserData(u) if u.is::<PartH>() => out.push(u.borrow::<PartH>()?.idx),
        Value::UserData(u) if u.is::<GroupH>() => out.extend(u.borrow::<GroupH>()?.parts.iter()),
        Value::Table(t) => {
            for x in t.sequence_values::<Value>() {
                collect(lua, x?, out, depth + 1)?;
            }
        }
        other => {
            return err(format!(
                "group: children must be parts or groups, got {}",
                other.type_name()
            ))
        }
    }
    Ok(())
}

/// `group(name, children)`: a list of parts and groups, or a function whose parts it takes.
fn group(lua: &Lua, (name, kids): (String, Value)) -> LResult<GroupH> {
    let mut parts = vec![];
    if let Value::Function(f) = &kids {
        let from = builder(lua)?.parts.len();
        f.call::<()>(())?;
        parts.extend(from..builder(lua)?.parts.len());
    } else {
        collect(lua, kids, &mut parts, 0)?;
    }
    parts.sort_unstable();
    parts.dedup();
    ok(builder(lua)?
        .group(&name, &parts)
        .map_err(|e| format!("group: {e}")))?;
    Ok(GroupH { name, parts })
}

fn define_component(lua: &Lua, (name, def): (String, Value)) -> LResult<Comp> {
    let def = match def {
        Value::Function(f) => f.call::<Value>(())?,
        v => v,
    };
    let Value::Table(t) = def else {
        return err("define_component: give a list of forms or {name, form} pairs");
    };
    let mut forms = vec![];
    for (i, x) in t.sequence_values::<Value>().enumerate() {
        let x = x?;
        let item = match &x {
            Value::UserData(u) if u.is::<FormV>() => {
                (format!("f{}", i + 1), FormV::from_lua(x, lua)?, None)
            }
            Value::Table(p) => {
                let n: Option<String> = p.get("name")?;
                let fv: Value = p.get("form")?;
                let mat: Option<String> = p.get("material")?;
                (
                    n.unwrap_or_else(|| format!("f{}", i + 1)),
                    FormV::from_lua(fv, lua)?,
                    mat,
                )
            }
            other => {
                return err(format!(
                    "define_component: item {} is a {}",
                    i + 1,
                    other.type_name()
                ))
            }
        };
        forms.push(item);
    }
    if forms.is_empty() {
        return err("define_component: no forms");
    }
    Ok(Comp { name, forms })
}

/// `place(component, at[, rot][, role])`: rot is an angle about the vertical or a transform; a
/// string in its place (or after it) is the role: the placed group also becomes one component
/// row of that name (`place(blade, v(0, 0, 80), "blade")`), so counts read it as a blade.
fn place(lua: &Lua, (c, at, rot, role): (Comp, V3, Option<Value>, Option<String>)) -> LResult<GroupH> {
    let (rot, role) = match rot {
        Some(Value::String(s)) => (None, Some(s.to_str()?.to_string())),
        other => (other, role),
    };
    let r = match rot {
        None | Some(Value::Nil) => Mat::ID,
        Some(v) => match num(&v) {
            Some(a) => Mat::rot(UP, a),
            None => Mat::from_lua(v, lua)?,
        },
    };
    let m = Mat::translate(at).then(&r);
    let mut parts = vec![];
    for (n, s, mat) in &c.forms {
        let s = ok(s.transform(&m).map_err(|e| format!("place: {e}")))?;
        parts.push(make_part(lua, n, &s, mat.clone())?.idx);
    }
    if let Some(role) = role {
        ok(builder(lua)?.tag(&parts, &role, at).map_err(|e| format!("place: {e}")))?;
    }
    ok(builder(lua)?
        .group(&c.name, &parts)
        .map_err(|e| format!("place: {e}")))?;
    Ok(GroupH {
        name: c.name,
        parts,
    })
}

// ---- checks ----

fn ends_of(lua: &Lua, v: Value) -> LResult<Vec<V3>> {
    Ok(match &v {
        Value::UserData(u) if u.is::<V3>() => vec![V3::from_lua(v, lua)?],
        Value::UserData(u) if u.is::<PartH>() => {
            let p = u.borrow::<PartH>()?;
            vec![p.a, p.b]
        }
        Value::UserData(u) if u.is::<FormV>() => {
            let (a, b) = u.borrow::<FormV>()?.ends();
            vec![a, b]
        }
        other => {
            return err(format!(
                "expected a point, part or form, got {}",
                other.type_name()
            ))
        }
    })
}

fn axis_of(lua: &Lua, v: Value) -> LResult<V3> {
    let e = ends_of(lua, v)?;
    let d = if e.len() == 1 { e[0] } else { e[1].sub(e[0]) };
    d.unit()
        .ok_or_else(|| mlua::Error::RuntimeError("constrain: zero-length axis".into()))
}

fn min_gap(a: &[V3], b: &[V3]) -> f64 {
    a.iter()
        .flat_map(|p| b.iter().map(move |q| p.sub(*q).len()))
        .fold(f64::INFINITY, f64::min)
}

/// Checked now, from the script's own exact geometry; a broken one stops the run here.
fn constrain(lua: &Lua, (kind, a, b, v): (String, Value, Value, Option<f64>)) -> LResult<()> {
    let held = match kind.as_str() {
        "coincident" => {
            let g = min_gap(&ends_of(lua, a)?, &ends_of(lua, b)?);
            (g <= TOL)
                .then_some(())
                .ok_or(format!("ends are {:.1} mm apart (1 mm allowed)", g * 1e3))
        }
        "distance" => {
            let want = v.ok_or_else(|| {
                mlua::Error::RuntimeError("constrain: distance needs a value".into())
            })?;
            let g = min_gap(&ends_of(lua, a)?, &ends_of(lua, b)?);
            ((g - want).abs() <= TOL).then_some(()).ok_or(format!(
                "distance is {:.1} mm, wanted {:.1} mm",
                g * 1e3,
                want * 1e3
            ))
        }
        "parallel" | "perpendicular" => {
            let c = axis_of(lua, a)?.dot(axis_of(lua, b)?).abs();
            let good = if kind == "parallel" {
                (1.0 - c) <= ANG_TOL
            } else {
                c <= ANG_TOL
            };
            good.then_some(())
                .ok_or(format!("axes are not {kind} (cos {c:.6})"))
        }
        other => {
            return err(format!(
                "constrain: kind `{other}` is not coincident, distance, parallel or perpendicular"
            ))
        }
    };
    ok(held.map_err(|e| format!("constrain {kind}: {e}")))?;
    builder(lua)?.constraints += 1;
    Ok(())
}

/// `measure(a, b)`: distance of points; `measure(x)`: length of a part, form or path.
fn measure(lua: &Lua, (a, b): (Value, Option<Value>)) -> LResult<f64> {
    let v = match b {
        Some(b) if !b.is_nil() => min_gap(&ends_of(lua, a)?, &ends_of(lua, b)?),
        _ => match &a {
            Value::UserData(u) if u.is::<PartH>() => u.borrow::<PartH>()?.len,
            Value::UserData(u) if u.is::<FormV>() => u.borrow::<FormV>()?.length(),
            Value::UserData(u) if u.is::<PathV>() => u.borrow::<PathV>()?.length(),
            Value::UserData(u) if u.is::<V3>() => u.borrow::<V3>()?.len(),
            other => return err(format!("measure: cannot measure a {}", other.type_name())),
        },
    };
    let mut bl = builder(lua)?;
    // the caller's own line: the hook's last line may predate a one-line chunk
    let line = lua
        .inspect_stack(1, |d| d.current_line())
        .flatten()
        .map_or(bl.line, |l| l as u32);
    bl.measures.push(Measure {
        line,
        value: v,
        unit: "m",
    });
    Ok(v)
}

/// Deterministic `pairs`: numbers ascending, then text in byte order.
pub(crate) fn sorted_pairs(lua: &Lua, t: Table) -> LResult<(Function, Table, Value)> {
    let mut keys: Vec<Value> = vec![];
    for kv in t.pairs::<Value, Value>() {
        keys.push(kv?.0);
    }
    let rank = |v: &Value| -> LResult<(u8, f64, Vec<u8>)> {
        Ok(match v {
            Value::Integer(i) => (0, *i as f64, vec![]),
            Value::Number(n) => (0, *n, vec![]),
            Value::String(s) => (1, 0.0, s.as_bytes().to_vec()),
            Value::Boolean(b) => (2, f64::from(u8::from(*b)), vec![]),
            other => {
                return err(format!(
                    "pairs: keys of type {} have no stable order",
                    other.type_name()
                ))
            }
        })
    };
    let mut ranked = keys
        .into_iter()
        .map(|k| Ok((rank(&k)?, k)))
        .collect::<LResult<Vec<_>>>()?;
    ranked.sort_by(|a, b| {
        a.0 .0
            .cmp(&b.0 .0)
            .then(a.0 .1.total_cmp(&b.0 .1))
            .then(a.0 .2.cmp(&b.0 .2))
    });
    let keys: Vec<Value> = ranked.into_iter().map(|x| x.1).collect();
    let at = Cell::new(0usize);
    let next = lua.create_function(move |_, (t, _): (Table, Value)| {
        let i = at.get();
        at.set(i + 1);
        match keys.get(i) {
            Some(k) => Ok((k.clone(), t.raw_get::<Value>(k.clone())?)),
            None => Ok((Value::Nil, Value::Nil)),
        }
    })?;
    Ok((next, t, Value::Nil))
}
