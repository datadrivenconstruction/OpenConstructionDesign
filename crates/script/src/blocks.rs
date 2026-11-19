// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Parametric building blocks (docs/design/ICONIC_STRUCTURES.md): one options table in, real
//! parts out. Every member, panel and slab is its own part with a role name and a material;
//! members run straight between shared nodes, so axes meet exactly. Options are lengths in
//! metres or text with a unit, angles in radians or text; a refusal names block and option.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::f64::consts::PI;

use ddc_geom::member::catalog;
use ddc_ops::{IfcClass, Len, Profile, P2};
use mlua::{FromLua, Function, Lua, Table, Value};

use crate::stdlib::{builder, from_catalog, norm};
use crate::units::{parse_angle, parse_len};
use crate::values::{err, num, FormV, GroupH, LResult, Prof, Shape, V3, UP};

/// Points closer than this are one node.
const MERGE: f64 = 1e-4;
/// A quad bending out of its plane by more than this is split into two triangles.
const PLANAR: f64 = 0.002;

pub(crate) fn v(x: f64, y: f64, z: f64) -> V3 {
    V3::new(x, y, z)
}

pub(crate) fn lerp(a: V3, b: V3, t: f64) -> V3 {
    a.add(b.sub(a).scale(t))
}

/// Rotation of `p` about the vertical through `c`.
pub(crate) fn turn(p: V3, c: V3, a: f64) -> V3 {
    let (s, k) = a.sin_cos();
    let (x, y) = (p.x - c.x, p.y - c.y);
    v(c.x + x * k - y * s, c.y + x * s + y * k, p.z)
}

// ---- options ----

/// The options table of one block call; `done` refuses keys nobody read.
pub(crate) struct Opts {
    pub f: &'static str,
    t: Table,
    used: RefCell<Vec<&'static str>>,
}

impl Opts {
    pub fn new(f: &'static str, t: Option<Table>, lua: &Lua) -> LResult<Opts> {
        Ok(Opts {
            f,
            t: match t {
                Some(t) => t,
                None => lua.create_table()?,
            },
            used: RefCell::new(vec![]),
        })
    }

    fn raw(&self, k: &'static str) -> LResult<Value> {
        self.used.borrow_mut().push(k);
        self.t.get(k)
    }

    fn bad<T>(&self, k: &str, msg: impl std::fmt::Display) -> LResult<T> {
        err(format!("{}: {k}: {msg}", self.f))
    }

    pub fn len(&self, k: &'static str, d: f64) -> LResult<f64> {
        let x = match self.raw(k)? {
            Value::Nil => d,
            Value::String(s) => match parse_len(&s.to_str()?) {
                Ok(x) => x,
                Err(e) => return self.bad(k, e),
            },
            v => match num(&v) {
                Some(x) => x,
                None => return self.bad(k, format!("needs a length, got {}", v.type_name())),
            },
        };
        if !x.is_finite() {
            return self.bad(k, "must be finite");
        }
        Ok(x)
    }

    /// A length within `lo..=hi` metres.
    pub fn len_in(&self, k: &'static str, d: f64, lo: f64, hi: f64) -> LResult<f64> {
        let x = self.len(k, d)?;
        if x < lo - 1e-12 || x > hi + 1e-12 {
            return self.bad(k, format!("{x} m is outside {lo}..{hi} m"));
        }
        Ok(x)
    }

    pub fn ang(&self, k: &'static str, d: f64) -> LResult<f64> {
        let x = match self.raw(k)? {
            Value::Nil => d,
            Value::String(s) => match parse_angle(&s.to_str()?) {
                Ok(x) => x,
                Err(e) => return self.bad(k, e),
            },
            v => match num(&v) {
                Some(x) => x,
                None => return self.bad(k, format!("needs an angle, got {}", v.type_name())),
            },
        };
        if !x.is_finite() {
            return self.bad(k, "must be finite");
        }
        Ok(x)
    }

    pub fn ang_in(&self, k: &'static str, d: f64, lo_deg: f64, hi_deg: f64) -> LResult<f64> {
        let x = self.ang(k, d)?;
        let deg = x * 180.0 / PI;
        if deg < lo_deg - 1e-9 || deg > hi_deg + 1e-9 {
            return self.bad(k, format!("{deg:.1} deg is outside {lo_deg}..{hi_deg} deg"));
        }
        Ok(x)
    }

    pub fn int(&self, k: &'static str, d: i64, lo: i64, hi: i64) -> LResult<i64> {
        let x = match self.raw(k)? {
            Value::Nil => d,
            Value::Integer(i) => i,
            Value::Number(n) if n.fract() == 0.0 && n.abs() < 1e15 => n as i64,
            v => return self.bad(k, format!("needs a whole number, got {}", v.type_name())),
        };
        if !(lo..=hi).contains(&x) {
            return self.bad(k, format!("{x} is outside {lo}..{hi}"));
        }
        Ok(x)
    }

    pub fn num(&self, k: &'static str, d: f64, lo: f64, hi: f64) -> LResult<f64> {
        let x = match self.raw(k)? {
            Value::Nil => d,
            v => match num(&v) {
                Some(x) => x,
                None => return self.bad(k, format!("needs a number, got {}", v.type_name())),
            },
        };
        if !(x.is_finite() && x >= lo && x <= hi) {
            return self.bad(k, format!("{x} is outside {lo}..{hi}"));
        }
        Ok(x)
    }

    pub fn flag(&self, k: &'static str, d: bool) -> LResult<bool> {
        match self.raw(k)? {
            Value::Nil => Ok(d),
            Value::Boolean(b) => Ok(b),
            v => self.bad(k, format!("needs true or false, got {}", v.type_name())),
        }
    }

    pub fn text(&self, k: &'static str, d: &str) -> LResult<String> {
        match self.raw(k)? {
            Value::Nil => Ok(d.to_string()),
            Value::String(s) => {
                let s = s.to_str()?.trim().to_string();
                if s.is_empty() || s.len() > 60 {
                    return self.bad(k, "needs 1-60 characters");
                }
                Ok(s)
            }
            v => self.bad(k, format!("needs text, got {}", v.type_name())),
        }
    }

    pub fn choice(&self, k: &'static str, d: &str, of: &[&str]) -> LResult<String> {
        let s = self.text(k, d)?;
        if !of.contains(&s.as_str()) {
            return self.bad(k, format!("`{s}` is not one of {}", of.join(", ")));
        }
        Ok(s)
    }

    pub fn point(&self, k: &'static str, d: V3) -> LResult<V3> {
        match self.raw(k)? {
            Value::Nil => Ok(d),
            v => point(self.f, k, v),
        }
    }

    pub fn opt_point(&self, k: &'static str) -> LResult<Option<V3>> {
        match self.raw(k)? {
            Value::Nil => Ok(None),
            v => point(self.f, k, v).map(Some),
        }
    }

    pub fn points(&self, k: &'static str) -> LResult<Option<Vec<V3>>> {
        match self.raw(k)? {
            Value::Nil => Ok(None),
            Value::Table(t) => {
                let mut out = vec![];
                for x in t.sequence_values::<Value>() {
                    out.push(point(self.f, k, x?)?);
                }
                if out.len() > 100_000 {
                    return self.bad(k, "at most 100000 points");
                }
                Ok(Some(out))
            }
            v => self.bad(k, format!("needs a list of vec3, got {}", v.type_name())),
        }
    }

    pub fn table(&self, k: &'static str) -> LResult<Option<Table>> {
        match self.raw(k)? {
            Value::Nil => Ok(None),
            Value::Table(t) => Ok(Some(t)),
            v => self.bad(k, format!("needs a table, got {}", v.type_name())),
        }
    }

    pub fn value(&self, k: &'static str) -> LResult<Value> {
        self.raw(k)
    }

    /// A profile: profile.*(...) or text such as "CHS 508x20", "RHS 300x200x10", "IPE 300".
    pub fn prof(&self, k: &'static str, d: &str) -> LResult<Profile> {
        match self.raw(k)? {
            Value::Nil => prof_text(d).or_else(|e| self.bad(k, e)),
            Value::String(s) => prof_text(&s.to_str()?).or_else(|e| self.bad(k, e)),
            Value::UserData(u) if u.is::<Prof>() => Ok(u.borrow::<Prof>()?.0.clone()),
            v => self.bad(k, format!("needs a profile, got {}", v.type_name())),
        }
    }

    pub fn mat(&self, k: &'static str, d: &str) -> LResult<String> {
        self.text(k, d)
    }

    /// Refuses options this block does not read (a misspelt option would silently default).
    pub fn done(&self) -> LResult<()> {
        let used = self.used.borrow();
        for kv in self.t.clone().pairs::<Value, Value>() {
            let (k, _) = kv?;
            let name = match &k {
                Value::String(s) => s.to_str()?.to_string(),
                other => return err(format!("{}: option keys are names, got {}", self.f, other.type_name())),
            };
            if !used.contains(&name.as_str()) {
                let mut all: Vec<&str> = used.clone();
                all.sort_unstable();
                all.dedup();
                return err(format!(
                    "{}: unknown option `{name}` (options: {})",
                    self.f,
                    all.join(", ")
                ));
            }
        }
        Ok(())
    }
}

fn point(f: &str, k: &str, v: Value) -> LResult<V3> {
    let p = match &v {
        Value::UserData(u) if u.is::<V3>() => *u.borrow::<V3>()?,
        Value::Table(t) => {
            let c = |i: usize, name: &str| -> LResult<f64> {
                let n: Value = t.get(name)?;
                if let Some(x) = num(&n) {
                    return Ok(x);
                }
                let x: Value = t.get(i)?;
                Ok(num(&x).unwrap_or(f64::NAN))
            };
            v3_or(c(1, "x")?, c(2, "y")?, c(3, "z").unwrap_or(0.0))
        }
        other => {
            return err(format!(
                "{f}: {k}: needs a point such as vec3(1, 2, 3), {{1, 2, 3}} or {{x = 1, y = 2, z = 3}}, got {}",
                other.type_name()
            ))
        }
    };
    if !p.finite() {
        return err(format!("{f}: {k}: needs finite coordinates"));
    }
    Ok(p)
}

fn v3_or(x: f64, y: f64, z: f64) -> V3 {
    V3::new(x, y, if z.is_nan() { 0.0 } else { z })
}

fn mm(x: f64) -> Len {
    Len::m(x / 1000.0)
}

/// "CHS 508x20", "RHS 300x200x10", "SHS 200x10", "rect 300x600", "round 40", "I 600x300",
/// or a catalogue name ("IPE 300", "HEA 200"); sizes in millimetres.
pub(crate) fn prof_text(s: &str) -> Result<Profile, String> {
    let t = s.trim();
    let (kind, dims) = t.split_once(' ').unwrap_or((t, ""));
    let nums: Vec<f64> = dims
        .split(['x', 'X', '*'])
        .map(|x| x.trim().parse::<f64>())
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_default();
    let ok = |n: usize| nums.len() == n && nums.iter().all(|x| x.is_finite() && *x > 0.0);
    let p = match kind.to_ascii_lowercase().as_str() {
        "chs" if ok(2) && 2.0 * nums[1] < nums[0] => Profile::Ring {
            r_out: mm(nums[0] / 2.0),
            r_in: mm(nums[0] / 2.0 - nums[1]),
        },
        "rhs" if ok(3) && 2.0 * nums[2] < nums[0].min(nums[1]) => Profile::Tube {
            w: mm(nums[1]),
            h: mm(nums[0]),
            t: mm(nums[2]),
        },
        "shs" if ok(2) && 2.0 * nums[1] < nums[0] => Profile::Tube {
            w: mm(nums[0]),
            h: mm(nums[0]),
            t: mm(nums[1]),
        },
        "rect" if ok(2) => Profile::Rect { w: mm(nums[0]), h: mm(nums[1]) },
        "round" if ok(1) => Profile::Circle { r: mm(nums[0] / 2.0) },
        "i" if ok(2) => Profile::I {
            h: mm(nums[0]),
            b: mm(nums[1]),
            tw: mm((nums[0] / 40.0).max(6.0)),
            tf: mm((nums[0] / 25.0).max(8.0)),
        },
        _ => {
            let all = catalog();
            return all
                .iter()
                .find(|(n, _)| norm(n) == norm(t))
                .and_then(|(_, s)| from_catalog(s))
                .ok_or_else(|| {
                    format!(
                        "`{t}` is not a profile: write CHS 508x20, RHS 300x200x10, SHS 200x10, \
                         rect 300x600, round 40, I 600x300 (mm) or a catalogue name like IPE 300"
                    )
                });
        }
    };
    Ok(p)
}

// ---- arches ----

/// Height of a curve of `kind` over span `l` with `rise` at `x` (0..l).
#[derive(Clone)]
pub(crate) struct Curve {
    kind: u8,
    l: f64,
    rise: f64,
    c: f64,
}

impl Curve {
    pub fn new(f: &str, kind: &str, l: f64, rise: f64) -> LResult<Curve> {
        if !(l > 0.1 && rise > 0.05) {
            return err(format!("{f}: span and rise must be positive (span {l} m, rise {rise} m)"));
        }
        let k = match kind {
            "parabolic" => 0,
            "catenary" => 1,
            "circular" | "barrel" => 2,
            "pointed" => 3,
            other => {
                return err(format!(
                    "{f}: kind `{other}` is not catenary, parabolic, circular, barrel or pointed"
                ))
            }
        };
        if k == 2 && rise > l / 2.0 + 1e-9 {
            return err(format!("{f}: a circular arch rises at most half its span ({} m)", l / 2.0));
        }
        let c = match k {
            // catenary y = c (cosh(l/2c) - cosh((x - l/2)/c)) with y(l/2) = rise
            1 => {
                let g = |c: f64| c * ((l / (2.0 * c)).cosh() - 1.0) - rise;
                let (mut lo, mut hi) = (l * 1e-3, l * 1e4);
                if g(lo) < 0.0 {
                    return err(format!("{f}: rise {rise} m is too high for a catenary over {l} m"));
                }
                for _ in 0..200 {
                    let mid = 0.5 * (lo + hi);
                    if g(mid) > 0.0 {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                0.5 * (lo + hi)
            }
            2 => (l * l / 4.0 + rise * rise) / (2.0 * rise),
            3 => (rise * rise + l * l / 4.0) / l,
            _ => 0.0,
        };
        Ok(Curve { kind: k, l, rise, c })
    }

    /// Rise of a circular arch; None for the other kinds.
    pub fn circular_rise(&self) -> Option<f64> {
        (self.kind == 2).then_some(self.rise)
    }

    pub fn y(&self, x: f64) -> f64 {
        let (l, h, c) = (self.l, self.rise, self.c);
        let x = x.clamp(0.0, l);
        match self.kind {
            0 => 4.0 * h * x / l * (1.0 - x / l),
            1 => c * ((l / (2.0 * c)).cosh() - ((x - l / 2.0) / c).cosh()),
            2 => ((c * c - (x - l / 2.0).powi(2)).max(0.0)).sqrt() - (c - h),
            _ => {
                let xx = if x <= l / 2.0 { x } else { l - x };
                (c * c - (xx - c).powi(2)).max(0.0).sqrt()
            }
        }
        .max(0.0)
    }

    /// `n + 1` points from `a` to `b` (heights between linear), rising along the vertical.
    pub fn points(&self, a: V3, b: V3, n: usize) -> Vec<V3> {
        (0..=n)
            .map(|i| {
                let t = i as f64 / n as f64;
                lerp(a, b, t).add(UP.scale(self.y(t * self.l)))
            })
            .collect()
    }
}

// ---- emitting parts ----

type Q = (i64, i64, i64);

fn q(p: V3) -> Q {
    let k = 1.0 / MERGE;
    ((p.x * k).round() as i64, (p.y * k).round() as i64, (p.z * k).round() as i64)
}

/// Member form from `a` to `b`: profile y up unless vertical (then x is +x), turned by `roll`.
pub(crate) fn member_form(prof: &Profile, a: V3, b: V3, roll: f64) -> Option<FormV> {
    let d = b.sub(a);
    let z = d.unit()?;
    let mut x = UP.cross(z).unit().unwrap_or(v(1.0, 0.0, 0.0));
    if roll != 0.0 {
        let (s, c) = roll.sin_cos();
        x = x.scale(c).add(z.cross(x).scale(s));
    }
    Some(FormV(Shape::Extrude { prof: prof.clone(), o: a, x, z, len: d.len() }))
}

/// Newell normal (not unit) of a polygon.
fn newell(p: &[V3]) -> V3 {
    let mut n = v(0.0, 0.0, 0.0);
    for i in 0..p.len() {
        let (a, b) = (p[i], p[(i + 1) % p.len()]);
        n = n.add(v(
            (a.y - b.y) * (a.z + b.z),
            (a.z - b.z) * (a.x + b.x),
            (a.x - b.x) * (a.y + b.y),
        ));
    }
    n
}

/// How a block made a part; a role missing from [`ROLE_IFC`] takes the class of its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    Member,
    Panel,
    Slab,
    Solid,
}

/// Role -> (IFC class, predefined type). Truss webs follow the native truss export.
pub const ROLE_IFC: &[(&str, &str, &str)] = &[
    ("column", "IfcColumn", "COLUMN"),
    ("core", "IfcColumn", "COLUMN"),
    ("mast", "IfcColumn", "COLUMN"),
    ("pier", "IfcColumn", "COLUMN"),
    ("pylon", "IfcColumn", "COLUMN"),
    ("beam", "IfcBeam", "BEAM"),
    ("cross_beam", "IfcBeam", "BEAM"),
    ("edge_beam", "IfcBeam", "BEAM"),
    ("girder", "IfcBeam", "BEAM"),
    ("pylon_beam", "IfcBeam", "BEAM"),
    ("ring_beam", "IfcBeam", "BEAM"),
    ("chord", "IfcMember", "CHORD"),
    ("top_chord", "IfcMember", "CHORD"),
    ("bottom_chord", "IfcMember", "CHORD"),
    ("diagonal", "IfcMember", "STRUT"),
    ("vertical", "IfcMember", "POST"),
    ("post", "IfcMember", "POST"),
    ("baluster", "IfcMember", "POST"),
    ("brace", "IfcMember", "BRACE"),
    ("hanger", "IfcMember", "BRACE"),
    ("stay", "IfcMember", "BRACE"),
    ("wind_brace", "IfcMember", "BRACE"),
    ("bearing", "IfcMember", "MEMBER"),
    ("abutment", "IfcFooting", "STRIP_FOOTING"),
    ("crane_beam", "IfcBeam", "BEAM"),
    ("crane_girder", "IfcBeam", "BEAM"),
    ("gate_header", "IfcBeam", "LINTEL"),
    ("gate", "IfcPlate", "SHEET"),
    ("arch_brace", "IfcMember", "BRACE"),
    ("top_brace", "IfcMember", "BRACE"),
    ("purlin", "IfcMember", "PURLIN"),
    ("rafter", "IfcMember", "RAFTER"),
    ("stringer", "IfcMember", "STRINGER"),
    ("mullion", "IfcMember", "MULLION"),
    ("handrail", "IfcRailing", "HANDRAIL"),
    ("panel", "IfcPlate", "CURTAIN_PANEL"),
    ("plate", "IfcPlate", "SHEET"),
    ("bolt", "IfcMechanicalFastener", "BOLT"),
    ("tread", "IfcPlate", "SHEET"),
    ("membrane", "IfcCovering", "MEMBRANE"),
    ("cover", "IfcCovering", "ROOFING"),
    ("slab", "IfcSlab", "FLOOR"),
    ("deck", "IfcSlab", "FLOOR"),
    ("shell", "IfcSlab", "ROOF"),
    ("footing", "IfcFooting", "PAD_FOOTING"),
    ("louvre", "IfcShadingDevice", "JALOUSIE"),
    ("lamella", "IfcShadingDevice", "JALOUSIE"),
    ("slat", "IfcShadingDevice", "JALOUSIE"),
    ("fin", "IfcShadingDevice", "NOTDEFINED"),
];

/// IFC class of a block part; a free-form solid of an unknown role stays a generic proxy.
pub fn ifc_of(kind: PartKind, role: &str) -> Option<IfcClass> {
    let (class, pre) = match ROLE_IFC.iter().find(|r| r.0 == role) {
        Some(&(_, c, p)) => (c, Some(p)),
        None => match kind {
            PartKind::Member => ("IfcMember", Some("MEMBER")),
            PartKind::Panel => ("IfcPlate", None),
            PartKind::Slab => ("IfcSlab", Some("FLOOR")),
            PartKind::Solid => return None,
        },
    };
    Some(IfcClass {
        class: class.to_string(),
        predefined: pre.map(str::to_string),
    })
}

/// Collects one block's parts, its nodes and counts by role.
pub(crate) struct Em<'l> {
    lua: &'l Lua,
    pub f: &'static str,
    parts: Vec<usize>,
    edges: HashSet<(Q, Q)>,
    node_seen: HashSet<Q>,
    pub nodes: Vec<V3>,
    roles: BTreeMap<String, usize>,
}

impl<'l> Em<'l> {
    pub fn new(lua: &'l Lua, f: &'static str) -> Em<'l> {
        Em {
            lua,
            f,
            parts: vec![],
            edges: HashSet::new(),
            node_seen: HashSet::new(),
            nodes: vec![],
            roles: BTreeMap::new(),
        }
    }

    fn node(&mut self, p: V3) {
        if self.node_seen.insert(q(p)) {
            self.nodes.push(p);
        }
    }

    fn put(&mut self, kind: PartKind, role: &str, f: &FormV, mat: &str) -> LResult<()> {
        let idx = builder(self.lua)?
            .part(role, f, Some(mat.to_string()), ifc_of(kind, role))
            .map_err(|e| mlua::Error::RuntimeError(format!("{}: {e}", self.f)))?;
        self.parts.push(idx);
        *self.roles.entry(role.to_string()).or_insert(0) += 1;
        Ok(())
    }

    /// A grid structure the model builds on its host; `nodes` are its lattice points.
    pub fn lattice(
        &mut self,
        role: &str,
        make: impl FnOnce(u32, Option<ddc_ops::Ref>) -> ddc_ops::GeoOp,
        nodes: &[V3],
        r: f64,
    ) -> LResult<()> {
        let idx = builder(self.lua)?
            .lattice(role, make, nodes, r)
            .map_err(|e| mlua::Error::RuntimeError(format!("{}: {e}", self.f)))?;
        nodes.iter().for_each(|p| self.node(*p));
        self.parts.push(idx);
        *self.roles.entry(role.to_string()).or_insert(0) += 1;
        Ok(())
    }

    /// A straight member between two nodes; the same edge twice is made once.
    pub fn member(&mut self, role: &str, prof: &Profile, a: V3, b: V3, mat: &str) -> LResult<bool> {
        self.member_roll(role, prof, a, b, mat, 0.0)
    }

    pub fn member_roll(
        &mut self,
        role: &str,
        prof: &Profile,
        a: V3,
        b: V3,
        mat: &str,
        roll: f64,
    ) -> LResult<bool> {
        if !(a.finite() && b.finite()) {
            return err(format!("{}: a node is not finite", self.f));
        }
        if b.sub(a).len() < 0.01 {
            return Ok(false);
        }
        let (qa, qb) = (q(a), q(b));
        let key = if qa <= qb { (qa, qb) } else { (qb, qa) };
        if !self.edges.insert(key) {
            return Ok(false);
        }
        let Some(f) = member_form(prof, a, b, roll) else { return Ok(false) };
        self.node(a);
        self.node(b);
        self.put(PartKind::Member, role, &f, mat)?;
        Ok(true)
    }

    /// A flat panel of thickness `t` centred on the polygon `pts`; a warped quad becomes two
    /// triangles. Returns the panels made.
    pub fn panel(&mut self, role: &str, pts: &[V3], t: f64, mat: &str) -> LResult<usize> {
        let mut p: Vec<V3> = vec![];
        for x in pts {
            if p.last().is_none_or(|l: &V3| l.sub(*x).len() > 0.001) {
                p.push(*x);
            }
        }
        while p.len() > 1 && p[0].sub(p[p.len() - 1]).len() <= 0.001 {
            p.pop();
        }
        if p.len() < 3 {
            return Ok(0);
        }
        let Some(n) = newell(&p).unit() else { return Ok(0) };
        let c = p.iter().fold(v(0.0, 0.0, 0.0), |s, x| s.add(*x)).scale(1.0 / p.len() as f64);
        let warp = p.iter().map(|x| x.sub(c).dot(n).abs()).fold(0.0, f64::max);
        if warp > PLANAR {
            if p.len() == 4 {
                return Ok(self.panel(role, &[p[0], p[1], p[2]], t, mat)?
                    + self.panel(role, &[p[0], p[2], p[3]], t, mat)?);
            }
            // fan from the centre
            let mut made = 0;
            for i in 0..p.len() {
                made += self.panel(role, &[c, p[i], p[(i + 1) % p.len()]], t, mat)?;
            }
            return Ok(made);
        }
        let Some(x) = p[1].sub(p[0]).unit() else { return Ok(0) };
        let x = match x.sub(n.scale(x.dot(n))).unit() {
            Some(x) => x,
            None => return Ok(0),
        };
        let y = n.cross(x);
        let area2 = newell(&p).len();
        if area2 < 2e-4 {
            return Ok(0);
        }
        let local: Vec<P2> = p
            .iter()
            .map(|q| {
                let d = q.sub(p[0]);
                P2::m(d.dot(x), d.dot(y))
            })
            .collect();
        let f = FormV(Shape::Extrude {
            prof: Profile::Polygon { pts: local },
            o: p[0].sub(n.scale(t / 2.0)),
            x,
            z: n,
            len: t,
        });
        self.put(PartKind::Panel, role, &f, mat)?;
        Ok(1)
    }

    /// A horizontal slab: outline (and holes) in plan, top at `top`, `t` thick.
    pub fn slab(
        &mut self,
        role: &str,
        outer: &[V3],
        holes: &[Vec<V3>],
        top: f64,
        t: f64,
        mat: &str,
    ) -> LResult<()> {
        let pl = |r: &[V3]| -> Vec<P2> {
            let mut out: Vec<P2> = vec![];
            for p in r {
                let q = P2::m(p.x, p.y);
                if out.last().is_none_or(|l| {
                    let [a, b] = l.to_m();
                    (a - p.x).hypot(b - p.y) > 0.001
                }) {
                    out.push(q);
                }
            }
            out
        };
        let o = pl(outer);
        if o.len() < 3 {
            return err(format!("{}: a slab outline needs 3 or more points", self.f));
        }
        let prof = if holes.is_empty() {
            Profile::Polygon { pts: o }
        } else {
            Profile::Rings { outer: o, holes: holes.iter().map(|h| pl(h)).collect() }
        };
        let f = FormV(Shape::Extrude {
            prof,
            o: v(0.0, 0.0, top - t),
            x: v(1.0, 0.0, 0.0),
            z: UP,
            len: t,
        });
        self.put(PartKind::Slab, role, &f, mat)
    }

    /// Any form as one part (lofts, plates with holes).
    pub fn solid(&mut self, role: &str, f: &FormV, mat: &str) -> LResult<()> {
        self.put(PartKind::Solid, role, f, mat)
    }

    /// Groups the parts under `name`; returns the group and `{nodes, counts, parts}`.
    pub fn finish(self, name: &str) -> LResult<(GroupH, Table)> {
        if self.parts.is_empty() {
            return err(format!("{}: the options build nothing", self.f));
        }
        builder(self.lua)?
            .group(name, &self.parts)
            .map_err(|e| mlua::Error::RuntimeError(format!("{}: {e}", self.f)))?;
        let info = self.lua.create_table()?;
        info.set("nodes", self.nodes.clone())?;
        let counts = self.lua.create_table()?;
        for (k, n) in &self.roles {
            counts.set(k.as_str(), *n)?;
        }
        info.set("counts", counts)?;
        info.set("parts", self.parts.len())?;
        Ok((GroupH { name: name.to_string(), parts: self.parts }, info))
    }
}

/// Regular polygon of `n` points, radius `r`, around `c`, starting at angle `a0`.
pub(crate) fn ngon(c: V3, r: f64, n: usize, a0: f64) -> Vec<V3> {
    (0..n)
        .map(|i| {
            let a = a0 + 2.0 * PI * i as f64 / n as f64;
            v(c.x + r * a.cos(), c.y + r * a.sin(), c.z)
        })
        .collect()
}

pub(crate) fn install(lua: &Lua) -> LResult<()> {
    let g = lua.globals();
    crate::blocks_build::install(lua, &g)?;
    crate::blocks_surf::install(lua, &g)?;
    crate::geo_curve::install(lua, &g)?;
    crate::geo_build::install(lua, &g)?;
    Ok(())
}

/// A Lua function taking an optional options table.
pub(crate) fn block(
    lua: &Lua,
    f: fn(&Lua, Option<Table>) -> LResult<(GroupH, Table)>,
) -> LResult<Function> {
    lua.create_function(move |lua, t: Value| {
        let t = match t {
            Value::Nil => None,
            Value::Table(t) => Some(t),
            other => {
                return err(format!("blocks take one options table {{...}}, got {}", other.type_name()))
            }
        };
        f(lua, t)
    })
}

/// A vec3 from a Lua value (for surfaces written as Lua functions).
pub(crate) fn v3_of(lua: &Lua, x: Value, f: &str) -> LResult<V3> {
    match V3::from_lua(x, lua) {
        Ok(p) if p.finite() => Ok(p),
        _ => err(format!("{f}: the surface must return a finite vec3")),
    }
}
