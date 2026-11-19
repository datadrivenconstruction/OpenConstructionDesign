// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Script values: vectors, rigid transforms, profiles, paths, forms and handles of created
//! parts. Lengths are metres, angles radians.

use ddc_ops::Profile;
use mlua::{FromLua, Lua, MetaMethod, UserData, UserDataFields, UserDataMethods, Value};

pub(crate) type LResult<T> = mlua::Result<T>;

pub(crate) fn err<T>(msg: impl Into<String>) -> LResult<T> {
    Err(mlua::Error::RuntimeError(msg.into()))
}

macro_rules! from_ud {
    ($t:ty, $name:expr) => {
        impl FromLua for $t {
            fn from_lua(v: Value, _: &Lua) -> LResult<Self> {
                match &v {
                    Value::UserData(ud) if ud.is::<$t>() => Ok(ud.borrow::<$t>()?.clone()),
                    _ => Err(mlua::Error::FromLuaConversionError {
                        from: v.type_name(),
                        to: $name.into(),
                        message: Some(format!("expected {}, got {}", $name, v.type_name())),
                    }),
                }
            }
        }
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct V3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

pub(crate) const UP: V3 = V3 {
    x: 0.0,
    y: 0.0,
    z: 1.0,
};

impl V3 {
    pub fn new(x: f64, y: f64, z: f64) -> V3 {
        V3 { x, y, z }
    }
    pub fn add(self, b: V3) -> V3 {
        V3::new(self.x + b.x, self.y + b.y, self.z + b.z)
    }
    pub fn sub(self, b: V3) -> V3 {
        V3::new(self.x - b.x, self.y - b.y, self.z - b.z)
    }
    pub fn scale(self, k: f64) -> V3 {
        V3::new(self.x * k, self.y * k, self.z * k)
    }
    pub fn dot(self, b: V3) -> f64 {
        self.x * b.x + self.y * b.y + self.z * b.z
    }
    pub fn cross(self, b: V3) -> V3 {
        V3::new(
            self.y * b.z - self.z * b.y,
            self.z * b.x - self.x * b.z,
            self.x * b.y - self.y * b.x,
        )
    }
    pub fn len(self) -> f64 {
        self.dot(self).sqrt()
    }
    pub fn unit(self) -> Option<V3> {
        let n = self.len();
        (n.is_finite() && n > 1e-12).then(|| self.scale(1.0 / n))
    }
    pub fn finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

impl FromLua for V3 {
    /// A point as `vec3(1, 2, 3)`, `{1, 2, 3}`, `{1, 2}` (z = 0) or `{x = 1, y = 2, z = 3}`.
    fn from_lua(v: Value, _: &Lua) -> LResult<Self> {
        let fail = |from: &'static str, why: &str| mlua::Error::FromLuaConversionError {
            from,
            to: "vec3".into(),
            message: Some(format!(
                "expected a point such as vec3(1, 2, 3), {{1, 2, 3}} or {{x = 1, y = 2, z = 3}}; {why}"
            )),
        };
        match &v {
            Value::UserData(ud) if ud.is::<V3>() => Ok(*ud.borrow::<V3>()?),
            Value::Table(t) => {
                let get = |k: &str, i: i64| -> LResult<Option<f64>> {
                    let a: Value = t.get(k)?;
                    if let Some(n) = num(&a) {
                        return Ok(Some(n));
                    }
                    let b: Value = t.get(i)?;
                    Ok(num(&b))
                };
                match (get("x", 1)?, get("y", 2)?, get("z", 3)?) {
                    (Some(x), Some(y), z) => {
                        let p = V3::new(x, y, z.unwrap_or(0.0));
                        if p.finite() {
                            Ok(p)
                        } else {
                            Err(fail("table", "coordinates must be finite numbers"))
                        }
                    }
                    _ => Err(fail("table", "the table needs at least x and y as numbers")),
                }
            }
            other => Err(fail(other.type_name(), &format!("got {}", other.type_name()))),
        }
    }
}

impl UserData for V3 {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_field_method_get("x", |_, v| Ok(v.x));
        f.add_field_method_get("y", |_, v| Ok(v.y));
        f.add_field_method_get("z", |_, v| Ok(v.z));
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("dot", |_, a, b: V3| Ok(a.dot(b)));
        m.add_method("cross", |_, a, b: V3| Ok(a.cross(b)));
        m.add_method("length", |_, a, ()| Ok(a.len()));
        m.add_method("dist", |_, a, b: V3| Ok(a.sub(b).len()));
        m.add_method("unit", |_, a, ()| match a.unit() {
            Some(u) => Ok(u),
            None => err("unit: zero-length vector"),
        });
        m.add_meta_function(MetaMethod::Add, |_, (a, b): (V3, V3)| Ok(a.add(b)));
        m.add_meta_function(MetaMethod::Sub, |_, (a, b): (V3, V3)| Ok(a.sub(b)));
        m.add_meta_function(MetaMethod::Unm, |_, a: V3| Ok(a.scale(-1.0)));
        m.add_meta_function(MetaMethod::Mul, |lua, (a, b): (Value, Value)| {
            match (num(&a), num(&b)) {
                (Some(k), None) => Ok(V3::from_lua(b, lua)?.scale(k)),
                (None, Some(k)) => Ok(V3::from_lua(a, lua)?.scale(k)),
                _ => err("vec3 * vec3: use a:dot(b) or a:cross(b)"),
            }
        });
        m.add_meta_function(MetaMethod::Div, |_, (a, k): (V3, f64)| {
            if k == 0.0 {
                return err("vec3 / 0");
            }
            Ok(a.scale(1.0 / k))
        });
        m.add_meta_function(MetaMethod::Eq, |_, (a, b): (V3, V3)| Ok(a == b));
        m.add_meta_method(MetaMethod::ToString, |_, a, ()| {
            Ok(format!("vec3({}, {}, {})", g(a.x), g(a.y), g(a.z)))
        });
    }
}

pub(crate) fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Number(n) => Some(*n),
        _ => None,
    }
}

fn g(v: f64) -> String {
    format!("{}", (v * 1e9).round() / 1e9 + 0.0)
}

/// Rigid transform: rotation rows `r` and translation `t` (a point maps to `r * p + t`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Mat {
    pub r: [[f64; 3]; 3],
    pub t: V3,
}

impl Mat {
    pub const ID: Mat = Mat {
        r: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        t: V3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
    };

    pub fn translate(v: V3) -> Mat {
        Mat { t: v, ..Mat::ID }
    }

    /// Frame with the given axes as columns and origin `t`; the axes must be orthonormal.
    pub fn from_basis(x: V3, y: V3, z: V3, t: V3) -> Mat {
        Mat {
            r: [[x.x, y.x, z.x], [x.y, y.y, z.y], [x.z, y.z, z.z]],
            t,
        }
    }

    /// Rotation by `a` about the unit axis `k` through the origin (Rodrigues).
    pub fn rot(k: V3, a: f64) -> Mat {
        let (s, c) = a.sin_cos();
        let v = 1.0 - c;
        let (x, y, z) = (k.x, k.y, k.z);
        Mat {
            r: [
                [c + x * x * v, x * y * v - z * s, x * z * v + y * s],
                [y * x * v + z * s, c + y * y * v, y * z * v - x * s],
                [z * x * v - y * s, z * y * v + x * s, c + z * z * v],
            ],
            t: V3::new(0.0, 0.0, 0.0),
        }
    }

    pub fn dir(&self, v: V3) -> V3 {
        let r = &self.r;
        V3::new(
            r[0][0] * v.x + r[0][1] * v.y + r[0][2] * v.z,
            r[1][0] * v.x + r[1][1] * v.y + r[1][2] * v.z,
            r[2][0] * v.x + r[2][1] * v.y + r[2][2] * v.z,
        )
    }

    pub fn point(&self, p: V3) -> V3 {
        self.dir(p).add(self.t)
    }

    pub fn then(&self, b: &Mat) -> Mat {
        // self * b: b first, then self
        let mut r = [[0.0; 3]; 3];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, x) in row.iter_mut().enumerate() {
                *x = (0..3).map(|k| self.r[i][k] * b.r[k][j]).sum();
            }
        }
        Mat {
            r,
            t: self.point(b.t),
        }
    }

    pub fn inverse(&self) -> Mat {
        let mut r = [[0.0; 3]; 3];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, x) in row.iter_mut().enumerate() {
                *x = self.r[j][i];
            }
        }
        let m = Mat {
            r,
            t: V3::new(0.0, 0.0, 0.0),
        };
        Mat {
            t: m.dir(self.t).scale(-1.0),
            ..m
        }
    }

    /// Keeps lengths and handedness (no scale, no mirror).
    pub fn rigid(&self) -> bool {
        let c = |j: usize| V3::new(self.r[0][j], self.r[1][j], self.r[2][j]);
        let (a, b, z) = (c(0), c(1), c(2));
        let ok = |x: f64, w: f64| (x - w).abs() < 1e-9;
        ok(a.len(), 1.0)
            && ok(b.len(), 1.0)
            && ok(z.len(), 1.0)
            && ok(a.dot(b), 0.0)
            && ok(a.dot(z), 0.0)
            && ok(b.dot(z), 0.0)
            && ok(a.cross(b).dot(z), 1.0)
            && self.t.finite()
    }

    /// Rotation about the vertical axis only (plan arcs stay arcs).
    pub fn keeps_up(&self) -> bool {
        self.dir(UP).sub(UP).len() < 1e-9
    }
}

from_ud!(Mat, "transform");

impl UserData for Mat {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_field_method_get("origin", |_, m| Ok(m.t));
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("dir", |_, a, v: V3| Ok(a.dir(v)));
        m.add_method("inverse", |_, a, ()| Ok(a.inverse()));
        m.add_meta_function(MetaMethod::Mul, |lua, (a, b): (Mat, Value)| match &b {
            Value::UserData(u) if u.is::<Mat>() => Ok(Value::UserData(
                lua.create_userdata(a.then(&Mat::from_lua(b, lua)?))?,
            )),
            Value::UserData(u) if u.is::<V3>() => Ok(Value::UserData(
                lua.create_userdata(a.point(V3::from_lua(b, lua)?))?,
            )),
            Value::UserData(u) if u.is::<FormV>() => {
                let f = FormV::from_lua(b, lua)?.transform(&a).or_else(err)?;
                Ok(Value::UserData(lua.create_userdata(f)?))
            }
            _ => err("transform * x: x must be a transform, a vec3 or a form"),
        });
        m.add_meta_method(MetaMethod::ToString, |_, a, ()| {
            Ok(format!(
                "transform(origin {}, {}, {})",
                g(a.t.x),
                g(a.t.y),
                g(a.t.z)
            ))
        });
    }
}

/// A section profile (wire form, metres).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Prof(pub Profile);

impl FromLua for Prof {
    /// A profile object, a catalogue text (`"CHS 168x8"`, `"rect 300x600"`), or a table:
    /// `{w = 0.3, h = 0.6}`, `{r = 0.1}`, `{d = 0.2}` or a list of points.
    fn from_lua(v: Value, lua: &Lua) -> LResult<Self> {
        let fail = |from: &'static str, why: String| mlua::Error::FromLuaConversionError {
            from,
            to: "profile".into(),
            message: Some(format!(
                "expected profile.rect(0.3, 0.6), \"CHS 168x8\", {{w = 0.3, h = 0.6}}, {{r = 0.1}}                  or a list of points {{{{0, 0}}, {{1, 0}}, {{0, 1}}}}; {why}"
            )),
        };
        match &v {
            Value::UserData(ud) if ud.is::<Prof>() => Ok(ud.borrow::<Prof>()?.clone()),
            Value::String(s) => crate::blocks::prof_text(&s.to_str()?)
                .map(Prof)
                .map_err(|e| fail("string", e)),
            Value::Table(t) => {
                let dim = |k: &str| -> LResult<Option<f64>> {
                    let a: Value = t.get(k)?;
                    match &a {
                        Value::Nil => Ok(None),
                        Value::String(s) => crate::units::parse_len(&s.to_str()?)
                            .map(Some)
                            .map_err(|e| fail("table", format!("{k}: {e}"))),
                        other => num(other).map(Some).ok_or_else(|| {
                            fail("table", format!("{k} must be a number in metres or text like \"300 mm\""))
                        }),
                    }
                };
                let pos = |x: Option<f64>, k: &str| match x {
                    Some(v) if v.is_finite() && v > 0.0 => Ok(v),
                    _ => Err(fail("table", format!("{k} must be a positive length"))),
                };
                let (w, h) = (dim("w")?, dim("h")?);
                let (r, d) = (dim("r")?, dim("d")?);
                if w.is_some() || h.is_some() {
                    return Ok(Prof(Profile::Rect {
                        w: ddc_ops::Len::m(pos(w, "w")?),
                        h: ddc_ops::Len::m(pos(h, "h")?),
                    }));
                }
                if r.is_some() || d.is_some() {
                    let r = r.or(d.map(|d| d / 2.0));
                    return Ok(Prof(Profile::Circle { r: ddc_ops::Len::m(pos(r, "r")?) }));
                }
                let mut pts = vec![];
                for p in t.clone().sequence_values::<Value>() {
                    let p = V3::from_lua(p?, lua).map_err(|e| fail("table", e.to_string()))?;
                    pts.push(ddc_ops::P2::m(p.x, p.y));
                }
                if pts.len() < 3 {
                    return Err(fail("table", "a point list needs 3 or more points".into()));
                }
                Ok(Prof(Profile::Polygon { pts }))
            }
            other => Err(fail(other.type_name(), format!("got {}", other.type_name()))),
        }
    }
}

impl UserData for Prof {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_field_method_get("kind", |_, p| Ok(prof_kind(&p.0)));
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(MetaMethod::ToString, |_, p, ()| {
            Ok(format!("profile {}", prof_kind(&p.0)))
        });
    }
}

pub(crate) fn prof_kind(p: &Profile) -> &'static str {
    match p {
        Profile::Rect { .. } => "rect",
        Profile::Circle { .. } => "circle",
        Profile::Ring { .. } => "ring",
        Profile::I { .. } => "i",
        Profile::H { .. } => "h",
        Profile::C { .. } => "c",
        Profile::T { .. } => "t",
        Profile::L { .. } => "l",
        Profile::Z { .. } => "z",
        Profile::Tube { .. } => "tube",
        Profile::Polygon { .. } => "polygon",
        Profile::Polyline { .. } => "polyline",
        Profile::Rings { .. } => "rings",
        Profile::Stored { .. } => "stored",
    }
}

/// A path: points and one bulge per segment (arc in plan, height linear along it).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PathV {
    pub pts: Vec<V3>,
    pub bulges: Vec<f64>,
    pub closed: bool,
}

from_ud!(PathV, "path");

impl PathV {
    fn seg(&self, i: usize) -> (V3, V3, f64) {
        let n = self.pts.len();
        (
            self.pts[i],
            self.pts[(i + 1) % n],
            self.bulges.get(i).copied().unwrap_or(0.0),
        )
    }

    pub fn segs(&self) -> usize {
        if self.closed {
            self.pts.len()
        } else {
            self.pts.len().saturating_sub(1)
        }
    }

    pub fn length(&self) -> f64 {
        (0..self.segs())
            .map(|i| {
                let (a, b, bu) = self.seg(i);
                let d = b.sub(a);
                let c = (d.x * d.x + d.y * d.y).sqrt();
                let th = 4.0 * bu.atan();
                let plan = if th.abs() < 1e-12 {
                    c
                } else {
                    c * th / (2.0 * (th / 2.0).sin())
                };
                (plan * plan + d.z * d.z).sqrt()
            })
            .sum()
    }
}

impl UserData for PathV {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("length", |_, p, ()| Ok(p.length()));
        m.add_meta_method(MetaMethod::ToString, |_, p, ()| {
            Ok(format!("path({} points)", p.pts.len()))
        });
        crate::geo_curve::path_methods(m);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Shape {
    /// Profile in the x/y plane of the frame at `o`, extruded `len` along `z`.
    Extrude {
        prof: Profile,
        o: V3,
        x: V3,
        z: V3,
        len: f64,
    },
    /// `twist`: total turn of the profile over the path (radians); `scale`: profile scale at
    /// the end of the path.
    Sweep {
        prof: Profile,
        path: PathV,
        twist: f64,
        scale: f64,
    },
    /// Horizontal sections at their points, x along +x.
    Loft {
        secs: Vec<(Profile, V3)>,
    },
}

/// A form not placed in the model yet.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FormV(pub Shape);

from_ud!(FormV, "form");

/// Points a profile keeps; a catalog or primitive profile counts as a few.
fn profile_points(p: &ddc_ops::Profile) -> usize {
    match p {
        ddc_ops::Profile::Polygon { pts } => pts.len(),
        _ => 4,
    }
}

impl FormV {
    /// Rough memory weight in points: what a copy of the form costs outside the interpreter.
    pub fn weight(&self) -> usize {
        1 + match &self.0 {
            Shape::Extrude { prof, .. } => profile_points(prof),
            Shape::Sweep { prof, path, .. } => profile_points(prof) + path.pts.len(),
            Shape::Loft { secs } => secs.iter().map(|(p, _)| 1 + profile_points(p)).sum(),
        }
    }

    pub fn transform(&self, m: &Mat) -> Result<FormV, String> {
        if !m.rigid() {
            return Err("transform: only rotations and moves (no scale or mirror)".into());
        }
        Ok(FormV(match &self.0 {
            Shape::Extrude { prof, o, x, z, len } => Shape::Extrude {
                prof: prof.clone(),
                o: m.point(*o),
                x: m.dir(*x),
                z: m.dir(*z),
                len: *len,
            },
            Shape::Sweep { prof, path, twist, scale } => {
                if !m.keeps_up() && path.bulges.iter().any(|b| *b != 0.0) {
                    return Err("transform: a path with arcs turns about the vertical only".into());
                }
                Shape::Sweep {
                    twist: *twist,
                    scale: *scale,
                    prof: prof.clone(),
                    path: PathV {
                        pts: path.pts.iter().map(|p| m.point(*p)).collect(),
                        ..path.clone()
                    },
                }
            }
            Shape::Loft { secs } => {
                if m.dir(V3::new(1.0, 0.0, 0.0))
                    .sub(V3::new(1.0, 0.0, 0.0))
                    .len()
                    > 1e-9
                {
                    return Err("transform: a loft can only be moved, not rotated".into());
                }
                Shape::Loft {
                    secs: secs
                        .iter()
                        .map(|(p, at)| (p.clone(), m.point(*at)))
                        .collect(),
                }
            }
        }))
    }

    /// Start and end of the form's axis.
    pub fn ends(&self) -> (V3, V3) {
        match &self.0 {
            Shape::Extrude { o, z, len, .. } => (*o, o.add(z.scale(*len))),
            Shape::Sweep { path, .. } => {
                let last = if path.closed {
                    path.pts[0]
                } else {
                    *path.pts.last().unwrap_or(&path.pts[0])
                };
                (path.pts[0], last)
            }
            Shape::Loft { secs } => (secs[0].1, secs[secs.len() - 1].1),
        }
    }

    pub fn length(&self) -> f64 {
        match &self.0 {
            Shape::Extrude { len, .. } => *len,
            Shape::Sweep { path, .. } => path.length(),
            Shape::Loft { .. } => {
                let (a, b) = self.ends();
                a.sub(b).len()
            }
        }
    }

    pub fn kind(&self) -> &'static str {
        match &self.0 {
            Shape::Extrude { .. } => "extrude",
            Shape::Sweep { .. } => "sweep",
            Shape::Loft { .. } => "loft",
        }
    }
}

impl UserData for FormV {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_field_method_get("a", |_, s| Ok(s.ends().0));
        f.add_field_method_get("b", |_, s| Ok(s.ends().1));
        f.add_field_method_get("kind", |_, s| Ok(s.kind()));
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("length", |_, s, ()| Ok(s.length()));
        m.add_method("moved", |_, s, v: V3| {
            s.transform(&Mat::translate(v)).or_else(err)
        });
        m.add_method("rotated", |_, s, (a, c): (f64, Option<V3>)| {
            let c = c.unwrap_or(V3::new(0.0, 0.0, 0.0));
            let m = Mat::translate(c)
                .then(&Mat::rot(UP, a))
                .then(&Mat::translate(c.scale(-1.0)));
            s.transform(&m).or_else(err)
        });
        m.add_method("transformed", |_, s, t: Mat| s.transform(&t).or_else(err));
        m.add_meta_method(MetaMethod::ToString, |_, s, ()| {
            Ok(format!("form {} ({} m)", s.kind(), g(s.length())))
        });
    }
}

/// A created part: index into the builder, its axis ends and length.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PartH {
    pub idx: usize,
    pub a: V3,
    pub b: V3,
    pub len: f64,
}

from_ud!(PartH, "part");

impl UserData for PartH {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_field_method_get("a", |_, p| Ok(p.a));
        f.add_field_method_get("b", |_, p| Ok(p.b));
        f.add_field_method_get("length", |_, p| Ok(p.len));
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(MetaMethod::ToString, |_, p, ()| {
            Ok(format!("part #{}", p.idx + 1))
        });
    }
}

/// Parts gathered under a name.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupH {
    pub name: String,
    pub parts: Vec<usize>,
}

from_ud!(GroupH, "group");

impl UserData for GroupH {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_field_method_get("count", |_, g| Ok(g.parts.len()));
        f.add_field_method_get("name", |_, g| Ok(g.name.clone()));
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(MetaMethod::ToString, |_, g, ()| {
            Ok(format!("group {} ({} parts)", g.name, g.parts.len()))
        });
    }
}

/// A reusable set of named forms around its own origin; `place` creates its parts.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Comp {
    pub name: String,
    pub forms: Vec<(String, FormV, Option<String>)>,
}

from_ud!(Comp, "component");

impl UserData for Comp {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_field_method_get("name", |_, c| Ok(c.name.clone()));
        f.add_field_method_get("count", |_, c| Ok(c.forms.len()));
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(MetaMethod::ToString, |_, c, ()| {
            Ok(format!("component {}", c.name))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_and_inverse() {
        let m = Mat::translate(V3::new(1.0, 2.0, 3.0)).then(&Mat::rot(UP, 0.7));
        assert!(m.rigid() && m.keeps_up());
        let p = V3::new(0.3, -4.0, 2.0);
        assert!(m.inverse().point(m.point(p)).sub(p).len() < 1e-12);
        let q = Mat::rot(V3::new(1.0, 0.0, 0.0), std::f64::consts::FRAC_PI_2).point(UP);
        assert!(q.sub(V3::new(0.0, -1.0, 0.0)).len() < 1e-12);
    }

    #[test]
    fn arc_path_length() {
        // half circle of radius 1: bulge 1
        let p = PathV {
            pts: vec![V3::new(1.0, 0.0, 0.0), V3::new(-1.0, 0.0, 0.0)],
            bulges: vec![1.0],
            closed: false,
        };
        assert!((p.length() - std::f64::consts::PI).abs() < 1e-12);
    }
}
