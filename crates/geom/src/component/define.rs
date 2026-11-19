// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Typed family definition for programmatic authoring (an assistant or a script): parameters
//! with units and ranges, reference planes, constraints, forms from profiles. `define` lowers it
//! to the family text, parses it, builds every type and runs the flex check with the test
//! values; any failure is a `Rejection` whose path points into the definition and whose message
//! names the parameter, formula or constraint that broke. Lowered text always carries
//! `status draft`; a person promotes it with `promoted`.

use std::fmt;

use super::flex::{self, Failure};
use super::{Axis, Category, Family, FamilyError, Hosting, Kind, Value, OUT_OF_RANGE};

/// Literal: a number in the parameter's unit (SI when the unit is empty) or a text.
#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    Num(f64),
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamDef {
    pub name: String,
    pub kind: Kind,
    pub instance: bool,
    /// Unit of `default`, `min`, `max` and test values: "mm", "m", "deg", ...
    pub unit: Option<String>,
    pub default: Option<Lit>,
    /// Expression over other parameters and planes; excludes `default`.
    pub formula: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RefAt {
    /// Plane fixed by an expression.
    Formula(String),
    /// Plane placed by the constraint sketch, starting near `guess` (a length literal).
    Free(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RefDef {
    pub name: String,
    pub axis: Axis,
    pub at: RefAt,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConsDef {
    /// b - a = value (a parameter name or a literal).
    Dim { value: String, a: String, b: String },
    Angle { value: String, a: String, b: String },
    Eq(Vec<String>),
    Align(String, String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProfileDef {
    Rect([String; 4]),
    Circle([String; 3]),
    Poly(Vec<[String; 2]>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum FormShape {
    Extrude { profile: ProfileDef, dir: Axis, from: String, to: String },
    Revolve { profile: ProfileDef, axis: [String; 2], angle: String },
    Sweep { profile: ProfileDef, path: Vec<[String; 3]> },
    Blend { bottom: ProfileDef, top: ProfileDef, from: String, to: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct FormDef {
    pub name: String,
    pub shape: FormShape,
    pub void: bool,
    /// Material parameter name.
    pub material: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeDef {
    pub name: String,
    pub values: Vec<(String, Lit)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FamilyDef {
    pub name: String,
    pub category: Category,
    pub hosting: Hosting,
    pub params: Vec<ParamDef>,
    pub refs: Vec<RefDef>,
    pub constraints: Vec<ConsDef>,
    pub forms: Vec<FormDef>,
    pub types: Vec<TypeDef>,
    /// Test values for the flex check, on top of the first type.
    pub tests: Vec<Vec<(String, Lit)>>,
}

/// Refusal a program can act on: JSON-pointer-like path into the definition, stable code,
/// message naming what broke; `test` is the index of the test values that failed.
#[derive(Clone, Debug, PartialEq)]
pub struct Rejection {
    pub path: String,
    pub code: &'static str,
    pub msg: String,
    pub test: Option<usize>,
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.test {
            Some(t) => write!(f, "{} [{}] test {t}: {}", self.path, self.code, self.msg),
            None => write!(f, "{} [{}]: {}", self.path, self.code, self.msg),
        }
    }
}

impl std::error::Error for Rejection {}

fn rej(path: impl Into<String>, code: &'static str, msg: impl Into<String>) -> Rejection {
    Rejection {
        path: path.into(),
        code,
        msg: msg.into(),
        test: None,
    }
}

/// Where a parameter's sweep first breaks (`None`: the whole range builds).
#[derive(Clone, Debug, PartialEq)]
pub struct SweepRow {
    pub param: String,
    pub from: f64,
    pub to: f64,
    pub breaks_at: Option<(f64, String)>,
}

#[derive(Clone, Debug)]
pub struct Accepted {
    pub text: String,
    pub family: Family,
    pub sweeps: Vec<SweepRow>,
}

/// Steps of the range sweep per limited parameter.
pub const SWEEP_STEPS: usize = 16;

fn ident(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && c.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// One-line source: no line breaks or comments that would change the lowered text.
fn line_safe(s: &str) -> bool {
    !s.trim().is_empty() && !s.contains(['\n', '\r', '#'])
}

fn num(v: f64) -> String {
    format!("{v}")
}

fn num_lit(v: f64, unit: &Option<String>) -> String {
    format!("{}{}", num(v), unit.as_deref().unwrap_or(""))
}

impl FamilyDef {
    fn param(&self, name: &str) -> Option<&ParamDef> {
        self.params.iter().find(|p| p.name == name)
    }

    /// Literal of a value for parameter `p` in family text.
    fn lit(&self, p: &ParamDef, v: &Lit) -> Result<String, String> {
        match (v, p.kind.is_text()) {
            (Lit::Num(x), false) if x.is_finite() => Ok(num_lit(*x, &p.unit)),
            (Lit::Text(t), true) if !t.contains(['"', '\n', '\r']) => Ok(format!("\"{t}\"")),
            _ => Err(format!(
                "parameter {}: value does not match kind {}",
                p.name,
                p.kind.name()
            )),
        }
    }

    fn assignments(&self, path: &str, vals: &[(String, Lit)]) -> Result<String, Rejection> {
        let mut out = vec![];
        for (k, (n, v)) in vals.iter().enumerate() {
            let p = self
                .param(n)
                .ok_or_else(|| rej(format!("{path}/{k}"), "unknown", format!("unknown parameter {n}")))?;
            let s = self
                .lit(p, v)
                .map_err(|m| rej(format!("{path}/{k}"), "invalid", m))?;
            out.push(format!("{n}={s}"));
        }
        Ok(out.join(", "))
    }

    /// Structural checks that the text parser cannot attribute to a definition path.
    fn check(&self) -> Result<(), Rejection> {
        if !line_safe(&self.name) || self.name.contains('"') {
            return Err(rej("/name", "invalid", "family name must be one line without quotes"));
        }
        let mut seen: Vec<&str> = vec![];
        let names = self
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| (format!("/params/{i}"), p.name.as_str()))
            .chain(self.refs.iter().enumerate().map(|(i, r)| (format!("/refs/{i}"), r.name.as_str())))
            .chain(self.forms.iter().enumerate().map(|(i, f)| (format!("/forms/{i}"), f.name.as_str())));
        for (path, n) in names {
            if !ident(n) {
                return Err(rej(path, "invalid", format!("name '{n}' is not an identifier")));
            }
            if seen.contains(&n) {
                return Err(rej(path, "duplicate", format!("name {n} is used twice")));
            }
            seen.push(n);
        }
        for (i, p) in self.params.iter().enumerate() {
            let path = format!("/params/{i}");
            if p.formula.is_some() && p.default.is_some() {
                return Err(rej(path, "invalid", format!("parameter {}: formula and default are exclusive", p.name)));
            }
            if p.formula.as_deref().is_some_and(|f| !line_safe(f)) {
                return Err(rej(path, "invalid", format!("parameter {}: formula must be one line", p.name)));
            }
            if p.unit.as_deref().is_some_and(|u| !u.chars().all(|c| c.is_ascii_alphanumeric())) {
                return Err(rej(path, "unit", format!("parameter {}: bad unit", p.name)));
            }
            if p.kind.is_text() && (p.min.is_some() || p.max.is_some()) {
                return Err(rej(path, "invalid", format!("parameter {}: text has no range", p.name)));
            }
            if [p.min, p.max].iter().flatten().any(|v| !v.is_finite()) {
                return Err(rej(path, "invalid", format!("parameter {}: range must be finite", p.name)));
            }
        }
        let exprs = self
            .refs
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let s = match &r.at {
                    RefAt::Formula(s) | RefAt::Free(s) => s,
                };
                (format!("/refs/{i}"), vec![s.as_str()])
            })
            .chain(self.constraints.iter().enumerate().map(|(i, c)| {
                let v: Vec<&str> = match c {
                    ConsDef::Dim { value, a, b } | ConsDef::Angle { value, a, b } => {
                        vec![value.as_str(), a.as_str(), b.as_str()]
                    }
                    ConsDef::Eq(n) => n.iter().map(|s| s.as_str()).collect(),
                    ConsDef::Align(a, b) => vec![a.as_str(), b.as_str()],
                };
                (format!("/constraints/{i}"), v)
            }))
            .chain(self.forms.iter().enumerate().map(|(i, f)| (format!("/forms/{i}"), f.exprs())));
        for (path, v) in exprs {
            if let Some(bad) = v.iter().find(|s| !line_safe(s) || s.contains([',', '(', ')']) && !balanced(s)) {
                return Err(rej(path, "invalid", format!("'{bad}' must be one balanced line")));
            }
        }
        for (i, c) in self.constraints.iter().enumerate() {
            let planes: Vec<&String> = match c {
                ConsDef::Dim { a, b, .. } | ConsDef::Angle { a, b, .. } | ConsDef::Align(a, b) => vec![a, b],
                ConsDef::Eq(n) => n.iter().collect(),
            };
            if let Some(bad) = planes.iter().find(|s| !ident(s)) {
                return Err(rej(format!("/constraints/{i}"), "invalid", format!("plane '{bad}' is not an identifier")));
            }
        }
        for (i, f) in self.forms.iter().enumerate() {
            if let Some(m) = &f.material {
                if !ident(m) {
                    return Err(rej(format!("/forms/{i}/material"), "invalid", format!("material '{m}' is not a parameter name")));
                }
            }
        }
        for (i, t) in self.types.iter().enumerate() {
            if !line_safe(&t.name) || t.name.contains('"') {
                return Err(rej(format!("/types/{i}"), "invalid", "type name must be one line without quotes"));
            }
        }
        Ok(())
    }

    /// Family text, one line per item, and the definition path of each line.
    fn lines(&self) -> Result<Vec<(String, String)>, Rejection> {
        self.check()?;
        let mut out: Vec<(String, String)> = vec![
            (format!("family {}", self.name), "/name".into()),
            (format!("category {}", self.category.name()), "/category".into()),
            (format!("host {}", self.hosting.name()), "/hosting".into()),
            ("status draft".into(), "/name".into()),
        ];
        for (i, p) in self.params.iter().enumerate() {
            let path = format!("/params/{i}");
            let scope = if p.instance { "instance" } else { "type" };
            let tail = match (&p.formula, &p.default) {
                (Some(f), _) => format!(" = {}", f.trim()),
                (None, Some(v)) => format!(" {}", self.lit(p, v).map_err(|m| rej(&path, "invalid", m))?),
                (None, None) => String::new(),
            };
            out.push((format!("param {} {} {scope}{tail}", p.name, p.kind.name()), path.clone()));
            if p.min.is_some() || p.max.is_some() {
                let b = |x: Option<f64>| x.map_or("_".to_string(), |x| num_lit(x, &p.unit));
                out.push((format!("limit {} {} {}", p.name, b(p.min), b(p.max)), path));
            }
        }
        for (i, r) in self.refs.iter().enumerate() {
            let ax = match r.axis {
                Axis::X => "x",
                Axis::Y => "y",
                Axis::Z => "z",
            };
            let at = match &r.at {
                RefAt::Formula(e) => format!("= {}", e.trim()),
                RefAt::Free(g) => format!("~ {}", g.trim()),
            };
            out.push((format!("ref {} {ax} {at}", r.name), format!("/refs/{i}")));
        }
        for (i, c) in self.constraints.iter().enumerate() {
            let l = match c {
                ConsDef::Dim { value, a, b } => format!("dim {} {a} {b}", value.trim()),
                ConsDef::Angle { value, a, b } => format!("angle {} {a} {b}", value.trim()),
                ConsDef::Eq(n) => format!("eq {}", n.join(" ")),
                ConsDef::Align(a, b) => format!("align {a} {b}"),
            };
            out.push((l, format!("/constraints/{i}")));
        }
        for (i, f) in self.forms.iter().enumerate() {
            out.push((f.line(), format!("/forms/{i}")));
        }
        for (i, t) in self.types.iter().enumerate() {
            let path = format!("/types/{i}");
            let body = self.assignments(&format!("{path}/values"), &t.values)?;
            out.push((format!("type \"{}\" {body}", t.name), path));
        }
        Ok(out)
    }

    /// Lowered family text (always a draft).
    pub fn to_text(&self) -> Result<String, Rejection> {
        let mut s = String::new();
        for (l, _) in self.lines()? {
            s.push_str(&l);
            s.push('\n');
        }
        Ok(s)
    }
}

fn balanced(s: &str) -> bool {
    let mut d = 0i32;
    for c in s.chars() {
        match c {
            '(' => d += 1,
            ')' => d -= 1,
            _ => {}
        }
        if d < 0 {
            return false;
        }
    }
    d == 0 && (!s.contains(',') || s.contains('('))
}

fn join<const N: usize>(v: &[[String; N]]) -> String {
    v.iter()
        .map(|p| p.iter().map(|s| s.trim()).collect::<Vec<_>>().join(", "))
        .collect::<Vec<_>>()
        .join(", ")
}

impl ProfileDef {
    fn text(&self) -> String {
        match self {
            ProfileDef::Rect(a) => format!("rect({})", join(std::slice::from_ref(a))),
            ProfileDef::Circle(a) => format!("circle({})", join(std::slice::from_ref(a))),
            ProfileDef::Poly(p) => format!("poly({})", join(p)),
        }
    }
    fn exprs(&self) -> Vec<&str> {
        match self {
            ProfileDef::Rect(a) => a.iter().map(|s| s.as_str()).collect(),
            ProfileDef::Circle(a) => a.iter().map(|s| s.as_str()).collect(),
            ProfileDef::Poly(p) => p.iter().flatten().map(|s| s.as_str()).collect(),
        }
    }
}

impl FormDef {
    fn line(&self) -> String {
        let mut s = match &self.shape {
            FormShape::Extrude { profile, dir, from, to } => {
                let d = match dir {
                    Axis::X => " dir(x)",
                    Axis::Y => " dir(y)",
                    Axis::Z => "",
                };
                format!("extrude {} {} z({}, {}){d}", self.name, profile.text(), from.trim(), to.trim())
            }
            FormShape::Revolve { profile, axis, angle } => format!(
                "revolve {} {} axis({}) angle({})",
                self.name,
                profile.text(),
                join(std::slice::from_ref(axis)),
                angle.trim()
            ),
            FormShape::Sweep { profile, path } => {
                format!("sweep {} {} path({})", self.name, profile.text(), join(path))
            }
            FormShape::Blend { bottom, top, from, to } => format!(
                "blend {} {} {} z({}, {})",
                self.name,
                bottom.text(),
                top.text(),
                from.trim(),
                to.trim()
            ),
        };
        if self.void {
            s.push_str(" void");
        }
        if let Some(m) = &self.material {
            s.push_str(&format!(" mat({m})"));
        }
        s
    }
    fn exprs(&self) -> Vec<&str> {
        match &self.shape {
            FormShape::Extrude { profile, from, to, .. } => {
                let mut v = profile.exprs();
                v.extend([from.as_str(), to.as_str()]);
                v
            }
            FormShape::Revolve { profile, axis, angle } => {
                let mut v = profile.exprs();
                v.extend(axis.iter().map(|s| s.as_str()));
                v.push(angle.as_str());
                v
            }
            FormShape::Sweep { profile, path } => {
                let mut v = profile.exprs();
                v.extend(path.iter().flatten().map(|s| s.as_str()));
                v
            }
            FormShape::Blend { bottom, top, from, to } => {
                let mut v = bottom.exprs();
                v.extend(top.exprs());
                v.extend([from.as_str(), to.as_str()]);
                v
            }
        }
    }
}

/// Text of a promoted family: the `status draft` line removed, everything else kept.
pub fn promoted(text: &str) -> String {
    let mut s: String = text
        .lines()
        .filter(|l| l.trim() != "status draft")
        .collect::<Vec<_>>()
        .join("\n");
    s.push('\n');
    s
}

/// Maps a family error to the definition path and names what broke.
fn reject(d: &FamilyDef, paths: &[String], e: &FamilyError) -> Rejection {
    let param_path = |n: &str| {
        d.params
            .iter()
            .position(|p| p.name == n)
            .map(|i| format!("/params/{i}"))
            .or_else(|| d.refs.iter().position(|r| r.name == n).map(|i| format!("/refs/{i}")))
            .or_else(|| d.forms.iter().position(|x| x.name == n).map(|i| format!("/forms/{i}")))
    };
    match e {
        FamilyError::Parse { line, msg } => rej(
            paths.get(line.saturating_sub(1)).cloned().unwrap_or_default(),
            "parse",
            msg.clone(),
        ),
        FamilyError::Cycle(p) => rej(
            p.first().and_then(|n| param_path(n)).unwrap_or_default(),
            "formula",
            format!("formula cycle: {}", p.join(" -> ")),
        ),
        FamilyError::Param { name, msg } => {
            let path = param_path(name).unwrap_or_default();
            if msg.starts_with(OUT_OF_RANGE) {
                return rej(path, "out_of_range", format!("parameter {name} {msg}"));
            }
            let formula = d
                .param(name)
                .and_then(|p| p.formula.clone())
                .or_else(|| {
                    d.refs.iter().find(|r| r.name == *name).and_then(|r| match &r.at {
                        RefAt::Formula(s) => Some(s.clone()),
                        RefAt::Free(_) => None,
                    })
                });
            match formula {
                Some(src) => rej(path, "formula", format!("formula {name} = {}: {msg}", src.trim())),
                None => rej(path, "invalid", format!("parameter {name}: {msg}")),
            }
        }
        FamilyError::Geometry { solid, msg } => rej(
            param_path(solid).unwrap_or_default(),
            "geometry",
            format!("form {solid}: {msg}"),
        ),
    }
}

fn reject_failure(d: &FamilyDef, f: &Family, paths: &[String], e: &Failure) -> Rejection {
    match e {
        Failure::Resolve(e) | Failure::Geometry(e) => reject(d, paths, e),
        Failure::Flipped { dim, value } => {
            let i = f.ext.cons.iter().position(|c| c.label() == *dim);
            rej(
                i.map(|i| format!("/constraints/{i}")).unwrap_or_default(),
                "constraint",
                format!("constraint {dim}: {value} changes sign against the type value"),
            )
        }
    }
}

/// Sweeps every parameter that has both bounds over its range (`SWEEP_STEPS` steps, whole
/// numbers for integers, ends included) and reports the first value where the family fails.
/// Pure and deterministic.
pub fn sweep_limits(f: &Family, base: &[(String, Value)]) -> Vec<SweepRow> {
    f.limits
        .iter()
        .filter(|l| f.param(&l.param).is_some_and(|p| p.formula.is_none()))
        .filter_map(|l| {
            let (from, to) = (l.min?, l.max?);
            // integers step by whole numbers so every count in the range is tried
            let steps = match f.param(&l.param).map(|p| p.kind) {
                Some(Kind::Int) => ((to - from).round() as usize).clamp(1, SWEEP_STEPS),
                _ => SWEEP_STEPS,
            };
            let s = flex::sweep(f, base, &l.param, from, to, steps);
            let breaks_at = flex::first_failure(&s).map(|(v, e)| (v, e.to_string()));
            Some(SweepRow {
                param: l.param.clone(),
                from,
                to,
                breaks_at,
            })
        })
        .collect()
}

/// Validates and lowers a definition: text parses, every type builds, every test builds.
pub fn define(d: &FamilyDef) -> Result<Accepted, Rejection> {
    let lines = d.lines()?;
    let paths: Vec<String> = lines.iter().map(|l| l.1.clone()).collect();
    let text: String = lines.iter().map(|l| format!("{}\n", l.0)).collect();
    let f = Family::parse(&text).map_err(|e| reject(d, &paths, &e))?;
    for (i, t) in f.types.iter().enumerate() {
        if let Some(e) = flex::try_values(&f, &t.values, &[]).failure {
            let mut r = reject_failure(d, &f, &paths, &e);
            r.msg = format!("type {}: {}", t.name, r.msg);
            if r.path.is_empty() {
                r.path = format!("/types/{i}");
            }
            return Err(r);
        }
    }
    let base: Vec<(String, Value)> = f.types.first().map(|t| t.values.clone()).unwrap_or_default();
    if f.types.is_empty() {
        if let Some(e) = flex::try_values(&f, &[], &[]).failure {
            return Err(reject_failure(d, &f, &paths, &e));
        }
    }
    let mut trials = vec![];
    for (k, t) in d.tests.iter().enumerate() {
        let path = format!("/tests/{k}");
        let body = d.assignments(&path, t)?;
        let v = f.assignments(&body).map_err(|m| rej(&path, "invalid", m))?;
        trials.push(v);
    }
    for (k, o) in flex::flex(&f, &base, &trials).iter().enumerate() {
        if let Some(e) = &o.failure {
            let mut r = reject_failure(d, &f, &paths, e);
            r.test = Some(k);
            if r.path.is_empty() {
                r.path = format!("/tests/{k}");
            }
            return Err(r);
        }
    }
    let sweeps = sweep_limits(&f, &base);
    Ok(Accepted {
        text,
        family: f,
        sweeps,
    })
}
