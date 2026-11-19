//! Family editor: reference planes solved by a sketch, parameter-driven dimensions,
//! equalities, alignments, angle parameters, text conditions, per-parameter solid visibility,
//! insertion point and nested families. Extends the definition text:
//!
//! ```text
//! ref Center x = 0                 # pinned plane (as before)
//! ref Left x ~ -400mm              # free: the sketch solves its position, ~ is the initial value
//! refline Diag at(0, 0) angle(~30deg)   # reference line; the angle is a formula or ~free
//! dim Width Left Right             # Right − Left = Width (dimension tied to a parameter)
//! eq Left Center Right             # equal spacing
//! align Shelf Top                  # planes coincide (alignment lock)
//! angle Tilt Front Diag            # counter-clockwise angle from Front to Diag = Tilt
//! param Finish material type = if(Width > 1200mm, "Steel", "Oak")   # text condition
//! param Legs familytype type "Leg:Round"   # nested family type
//! visible Shelf HasShelf and Height > 600mm  # solid/nest is visible only when true
//! origin Left Front 0              # insertion point (expressions and planes allowed)
//! nest LegA Leg type(Round) at(Left, Front, 0) rot(0deg) set(Height = LegH, Mat = Finish)
//! nest LegB of(Legs) at(Right, Front, 0)     # family and type come from the familytype parameter
//! begin                            # nested definition up to `end` (after the family line)
//! family Leg
//! ...
//! end
//! ```
//!
//! Parameter formulas cannot reference free planes and lines (the sketch solves them
//! after the parameters); solids, nests and the insertion point can.

use std::sync::Arc;

use super::expr::{self, Expr, ANGLE, LENGTH};
use super::rig;
use super::{Axis, Built, Family, FamilyError, Kind, Lod, Resolved, Value};
use crate::math::{v2, v3, V3};

/// Maximum nesting depth of `begin … end`.
pub const MAX_NEST: usize = 4;

#[derive(Clone, Debug, PartialEq)]
pub struct FreeRef {
    pub name: String,
    pub axis: Axis,
    pub guess: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AngleSrc {
    Expr(Expr),
    /// Free angle with an initial value, rad.
    Free(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RefLine {
    pub name: String,
    pub at: [Expr; 2],
    pub angle: AngleSrc,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cons {
    /// b − a = value along the planes' axis.
    Dim {
        value: Expr,
        a: String,
        b: String,
    },
    Eq(Vec<String>),
    Align(String, String),
    /// Counter-clockwise angle from a to b.
    Angle {
        value: Expr,
        a: String,
        b: String,
    },
}

impl Cons {
    pub fn label(&self) -> String {
        let v = |e: &Expr| match e {
            Expr::Var(n) => n.clone(),
            Expr::Num(q) => expr::literal(*q),
            _ => "(…)".into(),
        };
        match self {
            Cons::Dim { value, a, b } => format!("dim {} {a} {b}", v(value)),
            Cons::Eq(n) => format!("eq {}", n.join(" ")),
            Cons::Align(a, b) => format!("align {a} {b}"),
            Cons::Angle { value, a, b } => format!("angle {} {a} {b}", v(value)),
        }
    }
    /// Parameter the dimension is tied to (the value is the parameter name).
    pub fn param(&self) -> Option<&str> {
        match self {
            Cons::Dim {
                value: Expr::Var(n),
                ..
            }
            | Cons::Angle {
                value: Expr::Var(n),
                ..
            } => Some(n),
            _ => None,
        }
    }
    pub fn names(&self) -> Vec<&str> {
        match self {
            Cons::Dim { a, b, .. } | Cons::Angle { a, b, .. } | Cons::Align(a, b) => {
                vec![a.as_str(), b.as_str()]
            }
            Cons::Eq(n) => n.iter().map(|s| s.as_str()).collect(),
        }
    }
}

/// Text formula: a string, a text parameter, or `if(condition, a, b)`.
#[derive(Clone, Debug, PartialEq)]
pub enum TextF {
    Lit(String),
    Param(String),
    If(Expr, Box<TextF>, Box<TextF>),
}

impl TextF {
    pub fn parse(s: &str) -> Result<TextF, String> {
        if s.chars().count() > expr::MAX_LEN {
            return Err(format!("formula longer than {} characters", expr::MAX_LEN));
        }
        Self::parse_at(s, 0)
    }

    fn parse_at(s: &str, depth: usize) -> Result<TextF, String> {
        // same limits as numeric formulas (a single condition parser, `expr`)
        if depth > expr::MAX_DEPTH {
            return Err(format!("nesting deeper than {}", expr::MAX_DEPTH));
        }
        let s = s.trim();
        if let Some(inner) = s.strip_prefix("if").map(str::trim_start) {
            if let Some(inner) = inner.strip_prefix('(').and_then(|i| i.strip_suffix(')')) {
                let p = super::split_top(inner);
                if p.len() != 3 {
                    return Err("if() takes 3 argument(s)".into());
                }
                return Ok(TextF::If(
                    super::ex(&p[0])?,
                    Box::new(Self::parse_at(&p[1], depth + 1)?),
                    Box::new(Self::parse_at(&p[2], depth + 1)?),
                ));
            }
        }
        if let Some(t) = super::unquote(s) {
            return Ok(TextF::Lit(t));
        }
        if !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Ok(TextF::Param(s.to_string()));
        }
        Err(format!(
            "expected \"text\", a text parameter or if(...), got '{s}'"
        ))
    }

    pub fn eval(&self, r: &Resolved) -> Result<String, String> {
        match self {
            TextF::Lit(t) => Ok(t.clone()),
            TextF::Param(p) => r
                .texts
                .get(p)
                .cloned()
                .ok_or_else(|| format!("unknown text parameter '{p}'")),
            TextF::If(c, a, b) => {
                let v = c.eval(&|s| r.nums.get(s).copied())?;
                if v.v != 0.0 {
                    a.eval(r)
                } else {
                    b.eval(r)
                }
            }
        }
    }

    fn vars(&self, nums: &mut Vec<String>, texts: &mut Vec<String>) {
        match self {
            TextF::Lit(_) => {}
            TextF::Param(p) => texts.push(p.clone()),
            TextF::If(c, a, b) => {
                c.vars(nums);
                a.vars(nums, texts);
                b.vars(nums, texts);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum NestOf {
    /// Nested family by name and (optionally) type.
    Family { family: String, ty: Option<String> },
    /// The familytype parameter: value is "Family:Type".
    Param(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum NestVal {
    Num(Expr),
    Text(TextF),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Nest {
    pub name: String,
    pub of: NestOf,
    pub at: [Expr; 3],
    pub rot: Option<Expr>,
    /// Bindings: nested parameter ← parent expression.
    pub set: Vec<(String, NestVal)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ext {
    pub free_refs: Vec<FreeRef>,
    pub lines: Vec<RefLine>,
    pub cons: Vec<Cons>,
    pub text_formulas: Vec<(String, TextF)>,
    pub visible: Vec<(String, Expr)>,
    pub origin: Option<[Expr; 3]>,
    pub nests: Vec<Nest>,
    /// Nested definitions (`begin … end`).
    pub children: Vec<Arc<Family>>,
}

/// Operators parsed by this module.
pub const STMTS: [&str; 8] = [
    "refline", "dim", "eq", "align", "angle", "visible", "origin", "nest",
];

fn words(rest: &str) -> Vec<String> {
    rest.split_whitespace().map(str::to_string).collect()
}

fn name_ok(n: &str) -> bool {
    n.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
        && n.chars().all(|c| c.is_alphanumeric() || c == '_')
}

impl Ext {
    pub fn has_text_formula(&self, p: &str) -> bool {
        self.text_formulas.iter().any(|t| t.0 == p)
    }

    /// The name is defined by the sketch (a free plane or line).
    pub fn defines(&self, n: &str) -> bool {
        self.free_refs.iter().any(|r| r.name == n) || self.lines.iter().any(|l| l.name == n)
    }

    pub fn child(&self, name: &str) -> Option<&Arc<Family>> {
        self.children.iter().find(|c| c.name == name)
    }

    pub fn is_empty_rig(&self) -> bool {
        self.free_refs.is_empty() && self.lines.is_empty() && self.cons.is_empty()
    }

    /// `ref <name> <axis> ~ <guess>`.
    pub(super) fn free_ref(&mut self, name: &str, ax: Axis, guess: &str) -> Result<(), String> {
        let g = super::ex(guess.trim())?.eval(&|_| None)?;
        if !expr::zero_fits(g, LENGTH) {
            return Err(format!("{name}: expected a length after ~"));
        }
        self.free_refs.push(FreeRef {
            name: name.to_string(),
            axis: ax,
            guess: g.v,
        });
        Ok(())
    }

    pub(super) fn stmt(&mut self, head: &str, rest: &str) -> Result<(), String> {
        match head {
            "refline" => {
                let it = super::items(rest)?;
                let usage = "refline <name> at(x, y) angle(expr | ~guess)";
                let Some((name, None)) = it.first().cloned() else {
                    return Err(usage.into());
                };
                let (mut at, mut angle) = (None, None);
                for (w, a) in &it[1..] {
                    match (w.as_str(), a) {
                        ("at", Some(a)) => at = Some(super::exprs::<2>("at", a)?),
                        ("angle", Some(a)) => {
                            let a = a.trim();
                            angle = Some(match a.strip_prefix('~') {
                                Some(g) => {
                                    let q = super::ex(g.trim())?.eval(&|_| None)?;
                                    if !expr::zero_fits(q, ANGLE) {
                                        return Err(format!("{name}: angle(~…) needs an angle"));
                                    }
                                    AngleSrc::Free(q.v)
                                }
                                None => AngleSrc::Expr(super::ex(a)?),
                            })
                        }
                        (o, _) => return Err(format!("unexpected '{o}' ({usage})")),
                    }
                }
                self.lines.push(RefLine {
                    name,
                    at: at.ok_or(usage)?,
                    angle: angle.ok_or(usage)?,
                });
            }
            "dim" | "angle" => {
                let w = words(rest);
                if w.len() != 3 {
                    return Err(format!("{head} <param | value> <a> <b>"));
                }
                let value = super::ex(&w[0])?;
                let (a, b) = (w[1].clone(), w[2].clone());
                self.cons.push(if head == "dim" {
                    Cons::Dim { value, a, b }
                } else {
                    Cons::Angle { value, a, b }
                });
            }
            "eq" => {
                let w = words(rest);
                if w.len() < 3 {
                    return Err("eq <a> <b> <c> ... (at least 3 planes)".into());
                }
                self.cons.push(Cons::Eq(w));
            }
            "align" => {
                let w = words(rest);
                if w.len() != 2 {
                    return Err("align <a> <b>".into());
                }
                self.cons.push(Cons::Align(w[0].clone(), w[1].clone()));
            }
            "visible" => {
                let (name, e) = rest
                    .split_once(char::is_whitespace)
                    .ok_or("visible <solid | nest> <condition>")?;
                if self.visible.iter().any(|v| v.0 == name) {
                    return Err(format!("duplicate visibility of {name}"));
                }
                self.visible.push((name.to_string(), super::ex(e.trim())?));
            }
            "origin" => {
                // "origin x, y, z" or "origin Left Front" (no spaces inside expressions)
                let p = if rest.contains(',') {
                    super::split_top(rest)
                } else {
                    rest.split_whitespace().map(str::to_string).collect()
                };
                if p.len() < 2 || p.len() > 3 {
                    return Err("origin <x>, <y>[, z]".into());
                }
                let e = |i: usize| -> Result<Expr, String> {
                    p.get(i)
                        .map_or(Ok(Expr::Num(expr::q(0.0, LENGTH))), |s| super::ex(s))
                };
                if self.origin.is_some() {
                    return Err("origin is given twice".into());
                }
                self.origin = Some([e(0)?, e(1)?, e(2)?]);
            }
            "nest" => self.nests.push(nest(rest)?),
            o => return Err(format!("unknown statement '{o}'")),
        }
        Ok(())
    }

    /// Checks run after the whole text is parsed (`check_names` has already passed).
    pub(super) fn validate(&self, f: &Family) -> Result<(), FamilyError> {
        let err = perr;
        let fixed = |s: &str| f.param(s).is_some() || f.refs.iter().any(|r| r.name == s);
        for n in self
            .free_refs
            .iter()
            .map(|r| &r.name)
            .chain(self.lines.iter().map(|l| &l.name))
        {
            if !name_ok(n) {
                return Err(err(n, "bad name".into()));
            }
            let twice = self.free_refs.iter().filter(|r| &r.name == n).count()
                + self.lines.iter().filter(|l| &l.name == n).count();
            if fixed(n.as_str()) || twice > 1 {
                return Err(err(n, "name used twice".into()));
            }
        }
        // formulas and pinned planes are evaluated before the sketch
        let mut v = vec![];
        for p in &f.params {
            if let Some(e) = &p.formula {
                v.clear();
                e.vars(&mut v);
                if let Some(s) = v.iter().find(|s| self.defines(s)) {
                    return Err(err(
                        &p.name,
                        format!("formula uses '{s}', which the sketch solves later"),
                    ));
                }
            }
        }
        for r in &f.refs {
            v.clear();
            r.at.vars(&mut v);
            if let Some(s) = v.iter().find(|s| self.defines(s)) {
                return Err(err(&r.name, format!("uses '{s}', which the sketch solves")));
            }
        }
        for l in &self.lines {
            v.clear();
            l.at.iter().for_each(|e| e.vars(&mut v));
            if let AngleSrc::Expr(e) = &l.angle {
                e.vars(&mut v);
            }
            if let Some(s) = v.iter().find(|s| !fixed(s.as_str())) {
                return Err(err(&l.name, format!("unknown or solved name '{s}'")));
            }
        }
        let axis_of = |n: &str| -> Option<Option<Axis>> {
            if let Some(r) = f.refs.iter().find(|r| r.name == n) {
                return Some(Some(r.axis));
            }
            if let Some(r) = self.free_refs.iter().find(|r| r.name == n) {
                return Some(Some(r.axis));
            }
            self.lines.iter().any(|l| l.name == n).then_some(None)
        };
        for c in &self.cons {
            let label = c.label();
            let mut axes = vec![];
            for n in c.names() {
                axes.push(axis_of(n).ok_or_else(|| err(&label, format!("unknown plane '{n}'")))?);
            }
            match c {
                Cons::Angle { .. } => {
                    if axes.iter().any(|a| *a == Some(Axis::Z)) {
                        return Err(err(
                            &label,
                            "angles are measured in plan (x, y, lines)".into(),
                        ));
                    }
                }
                _ => {
                    if axes.iter().any(|a| a.is_none()) {
                        return Err(err(&label, "reference lines take only angle".into()));
                    }
                    if axes.windows(2).any(|w| w[0] != w[1]) {
                        return Err(err(&label, "planes must share one axis".into()));
                    }
                }
            }
            if let Cons::Dim { value, .. } | Cons::Angle { value, .. } = c {
                v.clear();
                value.vars(&mut v);
                if let Some(s) = v.iter().find(|s| !fixed(s.as_str())) {
                    return Err(err(&label, format!("unknown or solved name '{s}'")));
                }
                if let Some(p) = c.param().and_then(|p| f.param(p)) {
                    let want = if matches!(c, Cons::Dim { .. }) {
                        Kind::Length
                    } else {
                        Kind::Angle
                    };
                    if p.kind != want {
                        return Err(err(&label, format!("{} must be {}", p.name, want.name())));
                    }
                }
            }
        }
        let text_param = |s: &str| f.param(s).is_some_and(|p| p.kind.is_text());
        let check_text = |owner: &str, t: &TextF| -> Result<(), FamilyError> {
            let (mut nums, mut texts) = (vec![], vec![]);
            t.vars(&mut nums, &mut texts);
            if let Some(s) = nums.iter().find(|s| !fixed(s.as_str())) {
                return Err(err(owner, format!("unknown name '{s}'")));
            }
            if let Some(s) = texts.iter().find(|s| !text_param(s.as_str())) {
                return Err(err(owner, format!("'{s}' is not a text parameter")));
            }
            Ok(())
        };
        for (k, (p, t)) in self.text_formulas.iter().enumerate() {
            check_text(p.as_str(), t)?;
            // text formulas are evaluated in order: a forward reference is an error
            let (mut nums, mut texts) = (vec![], vec![]);
            t.vars(&mut nums, &mut texts);
            if let Some(s) = texts
                .iter()
                .find(|s| self.text_formulas[k..].iter().any(|(q, _)| q == *s))
            {
                return Err(err(
                    p,
                    format!("text formula uses '{s}' defined later or itself"),
                ));
            }
        }
        let known_geo = |s: &str| fixed(s) || self.defines(s);
        for (n, e) in &self.visible {
            if !f.solids.iter().any(|s| &s.name == n) && !self.nests.iter().any(|x| &x.name == n) {
                return Err(err(n, "visible: no such solid or nest".into()));
            }
            v.clear();
            e.vars(&mut v);
            if let Some(s) = v.iter().find(|s| !fixed(s.as_str())) {
                return Err(err(n, format!("unknown name '{s}'")));
            }
        }
        if let Some(o) = &self.origin {
            v.clear();
            o.iter().for_each(|e| e.vars(&mut v));
            if let Some(s) = v.iter().find(|s| !known_geo(s.as_str())) {
                return Err(err("origin", format!("unknown name '{s}'")));
            }
        }
        for n in &self.nests {
            if self.nests.iter().filter(|x| x.name == n.name).count() > 1
                || f.solids.iter().any(|s| s.name == n.name)
            {
                return Err(err(&n.name, "name used twice".into()));
            }
            let child = match &n.of {
                NestOf::Family { family, ty } => {
                    let c = self
                        .child(family)
                        .ok_or_else(|| err(&n.name, format!("no nested family '{family}'")))?;
                    if let Some(t) = ty {
                        if c.family_type(t).is_none() {
                            return Err(err(&n.name, format!("{family} has no type '{t}'")));
                        }
                    }
                    Some(c.clone())
                }
                NestOf::Param(p) => {
                    if !f.param(p).is_some_and(|x| x.kind == Kind::FamilyType) {
                        return Err(err(&n.name, format!("{p} is not a familytype parameter")));
                    }
                    None
                }
            };
            v.clear();
            n.at.iter().for_each(|e| e.vars(&mut v));
            if let Some(r) = &n.rot {
                r.vars(&mut v);
            }
            for (k, val) in &n.set {
                if let NestVal::Num(e) = val {
                    e.vars(&mut v);
                }
                if let Some(c) = &child {
                    let p = c
                        .param(k)
                        .ok_or_else(|| err(&n.name, format!("{} has no parameter {k}", c.name)))?;
                    if p.formula.is_some() || c.ext.has_text_formula(k) {
                        return Err(err(&n.name, format!("{k} is a formula and cannot be set")));
                    }
                    match (p.kind.is_text(), text_of(val)) {
                        (true, Some(t)) => check_text(n.name.as_str(), &t)?,
                        (false, _) if matches!(val, NestVal::Num(_)) => {}
                        _ => return Err(err(&n.name, format!("{k}: value does not match kind"))),
                    }
                }
            }
            if let Some(s) = v.iter().find(|s| !known_geo(s.as_str())) {
                return Err(err(&n.name, format!("unknown name '{s}'")));
            }
        }
        Ok(())
    }

    /// Tail of `resolve`: text formulas, sketch, visibility.
    pub(super) fn finish(&self, f: &Family, r: &mut Resolved) -> Result<(), FamilyError> {
        for (p, t) in &self.text_formulas {
            let v = t.eval(r).map_err(|msg| FamilyError::Param {
                name: p.clone(),
                msg,
            })?;
            r.texts.insert(p.clone(), v);
        }
        if !self.is_empty_rig() {
            let rep = rig::solve(f, r)?;
            if let Some(first) = rep.conflicting.first() {
                let all: Vec<String> = rep.conflicting.iter().map(|c| c.0.clone()).collect();
                return Err(FamilyError::Param {
                    name: first.0.clone(),
                    msg: format!("constraints conflict: {}", all.join("; ")),
                });
            }
            for (n, q) in rep.values {
                r.nums.insert(n, q);
            }
        }
        r.hidden.clear();
        for (n, e) in &self.visible {
            let v = e
                .eval(&|s| r.nums.get(s).copied())
                .map_err(|msg| FamilyError::Param {
                    name: n.clone(),
                    msg,
                })?;
            if v.v == 0.0 {
                r.hidden.push(n.clone());
            }
        }
        Ok(())
    }

    /// Insertion point in definition coordinates.
    pub fn origin_at(&self, r: &Resolved) -> Result<V3, FamilyError> {
        let Some(o) = &self.origin else {
            return Ok(v3(0.0, 0.0, 0.0));
        };
        let mut c = [0.0; 3];
        for (k, e) in o.iter().enumerate() {
            c[k] = len(e, r, "origin")?;
        }
        Ok(v3(c[0], c[1], c[2]))
    }

    /// Family and type of the nest for the computed values.
    pub fn nest_target(
        &self,
        n: &Nest,
        r: &Resolved,
    ) -> Result<(Arc<Family>, Option<String>), FamilyError> {
        let gerr = |msg: String| FamilyError::Geometry {
            solid: n.name.clone(),
            msg,
        };
        let (fam, ty) = match &n.of {
            NestOf::Family { family, ty } => (family.clone(), ty.clone()),
            NestOf::Param(p) => {
                let v = r.texts.get(p).cloned().unwrap_or_default();
                match v.split_once(':') {
                    Some((a, b)) => (a.trim().to_string(), Some(b.trim().to_string())),
                    None => (v.trim().to_string(), None),
                }
            }
        };
        let c = self
            .child(&fam)
            .cloned()
            .ok_or_else(|| gerr(format!("no nested family '{fam}'")))?;
        if let Some(t) = &ty {
            if c.family_type(t).is_none() {
                return Err(gerr(format!("{fam} has no type '{t}'")));
            }
        }
        Ok((c, ty))
    }

    /// Tail of `build`: nested families, then a shift to the insertion point.
    pub(super) fn finish_build(
        &self,
        r: &Resolved,
        lod: Lod,
        b: &mut Built,
    ) -> Result<(), FamilyError> {
        let had_solids = b.parts.iter().any(|p| !p.void);
        let mut first = !had_solids;
        for n in &self.nests {
            if r.hidden.contains(&n.name) {
                continue;
            }
            let gerr = |msg: String| FamilyError::Geometry {
                solid: n.name.clone(),
                msg,
            };
            let (child, ty) = self.nest_target(n, r)?;
            let mut tv: Vec<(String, Value)> = match &ty {
                Some(t) => child.type_values(t).unwrap_or(&[]).to_vec(),
                None => vec![],
            };
            for (k, val) in &n.set {
                let p = child
                    .param(k)
                    .ok_or_else(|| gerr(format!("{} has no parameter {k}", child.name)))?;
                let v = match (p.kind.is_text(), val) {
                    (true, val) => {
                        let t = text_of(val)
                            .ok_or_else(|| gerr(format!("{k}: value does not match kind")))?;
                        Value::Text(t.eval(r).map_err(&gerr)?)
                    }
                    (false, NestVal::Num(e)) => {
                        let q = e.eval(&|s| r.nums.get(s).copied()).map_err(&gerr)?;
                        let x =
                            super::check_kind(p.kind, q).map_err(|m| gerr(format!("{k}: {m}")))?;
                        Value::Num(x)
                    }
                    (false, NestVal::Text(_)) => {
                        return Err(gerr(format!("{k}: value does not match kind")))
                    }
                };
                tv.retain(|x| x.0 != *k);
                tv.push((k.clone(), v));
            }
            let cr = child
                .resolve(&tv, &[])
                .map_err(|e| gerr(format!("{}: {e}", child.name)))?;
            let cb = child
                .build(&cr, lod)
                .map_err(|e| gerr(format!("{}: {e}", child.name)))?;
            let at = [
                len(&n.at[0], r, &n.name)?,
                len(&n.at[1], r, &n.name)?,
                len(&n.at[2], r, &n.name)?,
            ];
            let rot = match &n.rot {
                Some(e) => {
                    let q = e.eval(&|s| r.nums.get(s).copied()).map_err(&gerr)?;
                    if !expr::zero_fits(q, ANGLE) {
                        return Err(gerr("rot() needs an angle".into()));
                    }
                    q.v
                }
                None => 0.0,
            };
            append(b, &cb, &n.name, at, rot, &mut first);
        }
        let o = self.origin_at(r)?;
        if o.x != 0.0 || o.y != 0.0 || o.z != 0.0 {
            for p in &mut b.mesh.positions {
                *p = *p - o;
            }
            b.bbox = [b.bbox[0] - o, b.bbox[1] - o];
            let o2 = v2(o.x, o.y);
            for l in &mut b.plan {
                *l = (l.0 - o2, l.1 - o2);
            }
        }
        Ok(())
    }
}

fn perr(name: &str, msg: String) -> FamilyError {
    FamilyError::Param {
        name: name.into(),
        msg,
    }
}

/// Text value of a binding: a string/condition or the name of a parent text parameter.
fn text_of(v: &NestVal) -> Option<TextF> {
    match v {
        NestVal::Text(t) => Some(t.clone()),
        NestVal::Num(Expr::Var(n)) => Some(TextF::Param(n.clone())),
        NestVal::Num(_) => None,
    }
}

fn len(e: &Expr, r: &Resolved, what: &str) -> Result<f64, FamilyError> {
    let q = e
        .eval(&|s| r.nums.get(s).copied())
        .map_err(|msg| FamilyError::Geometry {
            solid: what.into(),
            msg,
        })?;
    if !expr::zero_fits(q, LENGTH) {
        return Err(FamilyError::Geometry {
            solid: what.into(),
            msg: format!("expected length, got {}", q.d),
        });
    }
    Ok(q.v)
}

/// Adds a built nest: rotation about z, then translation.
fn append(b: &mut Built, c: &Built, name: &str, at: [f64; 3], rot: f64, first: &mut bool) {
    let (s, co) = rot.sin_cos();
    let tr = |p: V3| {
        v3(
            p.x * co - p.y * s + at[0],
            p.x * s + p.y * co + at[1],
            p.z + at[2],
        )
    };
    let base = b.mesh.positions.len() as u32;
    let fi = b.parts.len() as u32;
    b.mesh
        .positions
        .extend(c.mesh.positions.iter().map(|p| tr(*p)));
    b.mesh
        .triangles
        .extend(c.mesh.triangles.iter().map(|t| t.map(|i| i + base)));
    b.mesh.face_of.extend(c.mesh.face_of.iter().map(|f| f + fi));
    for p in &c.parts {
        let mut p = p.clone();
        p.name = format!("{name}/{}", p.name);
        b.parts.push(p);
    }
    b.volume += c.volume;
    b.surface += c.surface;
    let [lo, hi] = c.bbox;
    for x in [lo.x, hi.x] {
        for y in [lo.y, hi.y] {
            for z in [lo.z, hi.z] {
                let q = tr(v3(x, y, z));
                if *first {
                    b.bbox = [q, q];
                    *first = false;
                }
                b.bbox[0] = v3(
                    b.bbox[0].x.min(q.x),
                    b.bbox[0].y.min(q.y),
                    b.bbox[0].z.min(q.z),
                );
                b.bbox[1] = v3(
                    b.bbox[1].x.max(q.x),
                    b.bbox[1].y.max(q.y),
                    b.bbox[1].z.max(q.z),
                );
            }
        }
    }
    let t2 = |p: crate::math::V2| {
        let q = tr(v3(p.x, p.y, 0.0));
        v2(q.x, q.y)
    };
    b.plan.extend(c.plan.iter().map(|(a, e)| (t2(*a), t2(*e))));
}

fn nest(rest: &str) -> Result<Nest, String> {
    let usage =
        "nest <name> <Family> [type(T)] | of(Param), at(x, y, z) [rot(a)] [set(K = expr, ...)]";
    let it = super::items(rest)?;
    let Some((name, None)) = it.first().cloned() else {
        return Err(usage.into());
    };
    if !name_ok(&name) {
        return Err(format!("bad nest name '{name}'"));
    }
    let (mut family, mut ty, mut param, mut at, mut rot, mut set) =
        (None, None, None, None, None, vec![]);
    for (w, a) in &it[1..] {
        match (w.as_str(), a) {
            (w, None) => {
                if family.is_some() {
                    return Err(format!("unexpected '{w}' ({usage})"));
                }
                family = Some(super::unquote(w).unwrap_or_else(|| w.to_string()));
            }
            ("type", Some(a)) => {
                ty = Some(super::unquote(a).unwrap_or_else(|| a.trim().to_string()))
            }
            ("of", Some(a)) => param = Some(a.trim().to_string()),
            ("at", Some(a)) => at = Some(super::exprs::<3>("at", a)?),
            ("rot", Some(a)) => rot = Some(super::ex(a)?),
            ("set", Some(a)) => {
                for s in super::split_top(a) {
                    if s.is_empty() {
                        continue;
                    }
                    let (k, v) = s
                        .split_once('=')
                        .ok_or_else(|| format!("set(): expected Key = value, got '{s}'"))?;
                    let v = v.trim();
                    // a string, if(..., "…"), or a text parameter name is resolved at validation time
                    let val = if v.starts_with('"') || (v.starts_with("if") && v.contains('"')) {
                        NestVal::Text(TextF::parse(v)?)
                    } else {
                        NestVal::Num(super::ex(v)?)
                    };
                    set.push((k.trim().to_string(), val));
                }
            }
            (o, Some(_)) => return Err(format!("unknown clause {o}() ({usage})")),
        }
    }
    let of = match (family, param) {
        (Some(family), None) => NestOf::Family { family, ty },
        (None, Some(p)) if ty.is_none() => NestOf::Param(p),
        _ => return Err(usage.into()),
    };
    Ok(Nest {
        name,
        of,
        at: at.ok_or(usage)?,
        rot,
        set,
    })
}

/// Cuts out `begin … end` blocks (nested definitions): returns the parent text with blank lines in
/// their place (line numbers are preserved) and the block texts with the line number of `begin`.
pub(super) fn split_nested(text: &str) -> Result<(String, Vec<(usize, String)>), FamilyError> {
    let (mut body, mut blocks) = (Vec::new(), Vec::new());
    let (mut depth, mut cur, mut start, mut seen_family) = (0usize, String::new(), 0usize, false);
    let perr = |line: usize, msg: &str| FamilyError::Parse {
        line,
        msg: msg.into(),
    };
    for (n, line) in text.lines().enumerate() {
        let code = line.split('#').next().unwrap_or("").trim();
        let head = code.split_whitespace().next().unwrap_or("");
        match head {
            "begin" => {
                if depth == 0 {
                    if !seen_family {
                        return Err(perr(n + 1, "nested 'begin' must follow the 'family' line"));
                    }
                    start = n + 1;
                    cur.clear();
                } else {
                    cur.push_str(line);
                    cur.push('\n');
                }
                depth += 1;
                if depth > MAX_NEST {
                    return Err(perr(n + 1, "nested families deeper than 4"));
                }
                body.push(String::new());
            }
            "end" => {
                if depth == 0 {
                    return Err(perr(n + 1, "'end' without 'begin'"));
                }
                depth -= 1;
                if depth == 0 {
                    blocks.push((start, std::mem::take(&mut cur)));
                } else {
                    cur.push_str(line);
                    cur.push('\n');
                }
                body.push(String::new());
            }
            _ if depth > 0 => {
                cur.push_str(line);
                cur.push('\n');
                body.push(String::new());
            }
            _ => {
                if head == "family" {
                    seen_family = true;
                }
                body.push(line.to_string());
            }
        }
    }
    if depth > 0 {
        return Err(perr(start, "'begin' without 'end'"));
    }
    Ok((body.join("\n"), blocks))
}
