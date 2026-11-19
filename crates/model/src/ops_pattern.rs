// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Lowering of `Op::Pattern` and `Op::Family` (AI_NATIVE.md sections 4.4, 4.5) onto the
//! model's array and family calls. Child of `ops_apply`, so it shares `Run`, `Fail` and refs.

use ddc_geom::component::define::{
    ConsDef, FamilyDef, FormDef, FormShape, Lit, ParamDef, ProfileDef, RefAt, RefDef,
};
use ddc_geom::component::{flex, Axis as GAxis, Category, Family, Hosting, Kind};
use ddc_ops::{
    Axis, CurveAlign, Div, FamilyForm, FamilyOp, FamilyParam, FamilyRule, Form, ParamKind, Path,
    Pattern, PatternOut, Profile, RefPlaneDef, RuleKind, Spacing as OSpacing, SurfAlign, Unit,
    Value,
};

use super::*;
use crate::modify::MAX_PATTERN_COPIES;
use crate::{GridDiv, ModifyError, PathCount, PatternFace, Spacing, SurfaceUp};

fn at(sub: impl Into<String>, code: Code, s: impl Into<String>) -> Fail {
    Fail::At(sub.into(), code, s.into())
}

fn out_of_range(n: &str) -> Fail {
    Fail::Code(
        Code::TooMany,
        format!("out_of_range: the pattern makes {n} instances, at most {MAX_PATTERN_COPIES} are allowed"),
    )
}

fn cap(n: u64) -> Result<usize, Fail> {
    if n > MAX_PATTERN_COPIES as u64 {
        return Err(out_of_range(&n.to_string()));
    }
    Ok(n as usize)
}

/// The model's own cap (spacing-driven counts) reports as the same out-of-range refusal.
fn model_err(e: Error) -> Fail {
    match e {
        Error::Modify(ModifyError::TooMany(_)) => out_of_range("more"),
        e => Fail::Model(e),
    }
}

// ---- patterns ----

pub(super) fn pattern(
    m: &mut Model,
    run: &mut Run,
    source: &Targets,
    p: &Pattern,
    out: PatternOut,
) -> Result<(), Fail> {
    if out == PatternOut::Element {
        return Err(unsupported("a re-evaluating pattern element is not stored yet; use copies"));
    }
    let refs = targets(m, run, source)?;
    let mut ts = Vec::with_capacity(refs.len());
    for &(cat, id) in &refs {
        ts.push(target_of(cat, id).ok_or_else(|| {
            unsupported(format!("{} cannot be patterned", Ref::id(cat, id).label()))
        })?);
    }
    let copies = match p {
        Pattern::Linear { count, dir, spacing } => {
            let n = cap(*count as u64)?;
            if dir.z.abs() > EPS {
                return Err(unsupported("a linear pattern out of plan is not lowered yet"));
            }
            let l = dir.x.hypot(dir.y);
            if l <= EPS {
                return Err(bad_value("linear pattern direction is zero"));
            }
            let u = (dir.x / l, dir.y / l);
            let sp = match spacing {
                OSpacing::Step { d } => {
                    let d = d.to_m();
                    Spacing::Step((u.0 * d, u.1 * d))
                }
                OSpacing::Fit { total } => {
                    let d = total.to_m();
                    Spacing::Fit((u.0 * d, u.1 * d))
                }
            };
            m.array_linear(&ts, n, sp).map_err(model_err)?
        }
        Pattern::Radial { center, axis, count, angle } => {
            let n = cap(*count as u64)?;
            if axis.x.abs() > EPS || axis.y.abs() > EPS || axis.z.abs() <= EPS {
                return Err(unsupported("a radial pattern about a non-vertical axis is not lowered yet"));
            }
            let c = center.to_m();
            let total = angle.to_rad() * axis.z.signum();
            m.array_radial(&ts, n, (c[0], c[1]), total).map_err(model_err)?
        }
        Pattern::AlongCurve { path, count, spacing, align, start_offset } => {
            if start_offset.to_m().abs() > EPS {
                return Err(unsupported("a start offset along the path is not lowered yet"));
            }
            let mode = match (count, spacing) {
                (Some(n), None) => PathCount::Count(cap(*n as u64)?),
                (None, Some(d)) => PathCount::Step(d.to_m()),
                _ => return Err(bad_value("along a curve needs exactly one of count and spacing")),
            };
            let (pts, closed) = plan_path(m, run, path)?;
            let anchor = pts[0].0;
            let tangent = *align == CurveAlign::Tangent;
            m.array_path(&ts, anchor, &pts, closed, mode, tangent).map_err(model_err)?
        }
        Pattern::OnSurface { face, u, v, align } => {
            let nu = grid_div(u);
            let nv = grid_div(v);
            if let (Div::Count { n: a }, Div::Count { n: b }) = (u, v) {
                cap(*a as u64 * *b as u64)?;
            }
            let f = face_of(m, run, face)?;
            let orient = match align {
                SurfAlign::Up => SurfaceUp::Up,
                SurfAlign::Normal => SurfaceUp::Normal,
            };
            m.array_surface(&ts, &f, nu, nv, orient).map_err(model_err)?
        }
        Pattern::Distribute { a, b, count, ends } => {
            if *ends {
                return Err(unsupported("distribute with instances on the references is not lowered yet"));
            }
            if *count as usize != ts.len() {
                return Err(bad_value(format!(
                    "distribute moves its {} source elements: count must equal that number, got {count}",
                    ts.len()
                )));
            }
            let wa = resolve_as(m, run, a, Cat::Wall)?;
            let wb = resolve_as(m, run, b, Cat::Wall)?;
            m.distribute_between(wa, wb, &ts).map_err(model_err)?;
            run.receipt.changed.extend(refs.into_iter().map(|(c, id)| Ref::id(c, id)));
            return Ok(());
        }
        Pattern::Grid2d { .. } => {
            return Err(unsupported("a 2D grid pattern is not lowered yet"));
        }
    };
    run.receipt.copied.extend(copies.into_iter().map(ref_of));
    Ok(())
}

fn grid_div(d: &Div) -> GridDiv {
    match d {
        Div::Count { n } => GridDiv::Count(*n as usize),
        Div::Step { d } => GridDiv::Step(d.to_m()),
    }
}

/// Plan path as `(point, bulge of the segment from it)` and closedness.
fn plan_path(m: &Model, run: &Run, p: &Path) -> Result<(Vec<((f64, f64), f64)>, bool), Fail> {
    match p {
        Path::Polyline { pts, bulges, closed } => {
            if pts.len() < 2 {
                return Err(bad_value("a path needs at least two points"));
            }
            if !bulges.is_empty() && bulges.len() != pts.len() && bulges.len() + 1 != pts.len() {
                return Err(bad_value("path bulges must be empty or one per segment"));
            }
            let z0 = pts[0].to_m()[2];
            let mut out = Vec::with_capacity(pts.len());
            for (i, q) in pts.iter().enumerate() {
                let [x, y, z] = q.to_m();
                if (z - z0).abs() > EPS {
                    return Err(unsupported("a path that rises is not lowered yet"));
                }
                out.push(((x, y), bulges.get(i).copied().unwrap_or(0.0)));
            }
            Ok((out, *closed))
        }
        Path::Ref { of } => {
            let base = match of {
                Ref::Part { of, part } if part == "axis" => &**of,
                Ref::Part { .. } => {
                    return Err(unsupported(format!("path {} is not lowered yet", of.label())))
                }
                r => r,
            };
            let id = resolve_as(m, run, base, Cat::Wall)?;
            let (ax, ay, bx, by, bulge): (f64, f64, f64, f64, f64) = m.db.query_row(
                "SELECT ax, ay, bx, by, bulge FROM wall WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            Ok((vec![((ax, ay), bulge), ((bx, by), 0.0)], false))
        }
    }
}

/// Planar face from a face ref. Slab tops with a parallelogram outline are lowered.
fn face_of(m: &Model, run: &Run, r: &Ref) -> Result<PatternFace, Fail> {
    let Ref::Part { of, part } = r else {
        return Err(bad_value(format!("{} is not a face ref", r.label())));
    };
    let (cat, id) = resolve(m, run, of)?;
    if cat != Cat::Slab || part != "face:top" {
        return Err(unsupported(format!("face {} is not lowered yet (slab face:top is)", r.label())));
    }
    let (level, offset): (i64, f64) = m.db.query_row(
        "SELECT level, offset FROM slab WHERE id=?1",
        [id],
        |q| Ok((q.get(0)?, q.get(1)?)),
    )?;
    let z = level_row(m, level)?.1 + offset;
    let rings = crate::slabs::rings(&m.db, id)?;
    let outer = rings.first().map(|v| v.as_slice()).unwrap_or(&[]);
    let [p0, p1, p2, p3] = outer else {
        return Err(unsupported(format!("{} is not a four-corner outline", r.label())));
    };
    let para = (p0.0 + p2.0 - p1.0 - p3.0).abs() + (p0.1 + p2.1 - p1.1 - p3.1).abs();
    if para > 1e-6 {
        return Err(unsupported(format!("{} is not a parallelogram", r.label())));
    }
    Ok(PatternFace {
        origin: [p0.0, p0.1, z],
        u: [p1.0 - p0.0, p1.1 - p0.1, 0.0],
        v: [p3.0 - p0.0, p3.1 - p0.1, 0.0],
    })
}

// ---- families ----

pub(super) fn family(m: &mut Model, run: &mut Run, f: &FamilyOp) -> Result<(), Fail> {
    match f {
        FamilyOp::Define { slot, name, cat, params, planes, forms, constraints, tests } => {
            let d = FamilyDef {
                name: name.clone(),
                category: Category::parse(cat.key()).unwrap_or(Category::Generic),
                hosting: Hosting::None,
                params: params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| param_def(p).map_err(|s| at(format!("/params/{i}"), Code::Invalid, s)))
                    .collect::<Result<_, _>>()?,
                refs: planes.iter().map(plane_def).collect(),
                constraints: constraints
                    .iter()
                    .enumerate()
                    .map(|(i, c)| cons_def(c).map_err(|s| at(format!("/constraints/{i}"), Code::Unsupported, s)))
                    .collect::<Result<_, _>>()?,
                forms: forms
                    .iter()
                    .enumerate()
                    .map(|(i, x)| form_def(x).map_err(|s| at(format!("/forms/{i}"), Code::Unsupported, s)))
                    .collect::<Result<_, _>>()?,
                types: vec![],
                tests: tests
                    .iter()
                    .enumerate()
                    .map(|(k, t)| {
                        t.iter()
                            .map(|fd| {
                                let unit = params.iter().find(|p| p.name == fd.name).and_then(|p| p.unit);
                                Ok((fd.name.clone(), lit(&fd.value, unit)?))
                            })
                            .collect::<Result<Vec<_>, String>>()
                            .map_err(|s| at(format!("/tests/{k}"), Code::Invalid, s))
                    })
                    .collect::<Result<_, _>>()?,
            };
            match m.define_family(&d)? {
                Ok((id, _sweeps)) => {
                    run.made(*slot, Cat::Family, id);
                    Ok(())
                }
                Err(r) => {
                    // the typed definition names planes `refs`; the op names them `planes`
                    let path = match r.path.strip_prefix("/refs") {
                        Some(rest) => format!("/planes{rest}"),
                        None => r.path.clone(),
                    };
                    let test = r.test.map(|t| format!(" (test {t})")).unwrap_or_default();
                    Err(at(path, Code::Invalid, format!("{}: {}{test}", r.code, r.msg)))
                }
            }
        }
        FamilyOp::SetFormula { target, param, formula } => {
            let id = family_ref(m, run, target)?;
            m.set_family_formula(id, param, formula.as_deref())?
                .map_err(|(sub, s)| at(sub, Code::Invalid, s))?;
            run.receipt.changed.push(Ref::id(Cat::Family, id));
            Ok(())
        }
    }
}

fn family_ref(m: &Model, run: &Run, r: &Ref) -> Result<i64, Fail> {
    match r {
        Ref::Id { cat: Cat::Family, id } => {
            if !row_exists(&m.db, "comp_family", *id)? {
                return Err(bad_ref(format!("{} does not exist", r.label())));
            }
            Ok(*id)
        }
        Ref::Name { cat: Cat::Family, name } => {
            m.family_id(name)?.ok_or_else(|| bad_ref(format!("no family named \"{name}\"")))
        }
        Ref::New { slot } => match run.slots.get(slot) {
            Some((Cat::Family, id)) => Ok(*id),
            _ => Err(bad_ref(format!("slot {slot} is not a family created by an earlier op"))),
        },
        _ => Err(bad_ref(format!("{} is not a family", r.label()))),
    }
}

fn unit_key(u: Unit) -> String {
    serde_json::to_value(u)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Literal in the parameter's unit `unit` (the value's own unit when the parameter has none).
fn lit(v: &Value, unit: Option<Unit>) -> Result<Lit, String> {
    match &v.v {
        serde_json::Value::String(s) => Ok(Lit::Text(s.clone())),
        serde_json::Value::Bool(b) => Ok(Lit::Num(if *b { 1.0 } else { 0.0 })),
        serde_json::Value::Number(n) => {
            let x = n.as_f64().ok_or("number out of range")?;
            match (v.unit, unit) {
                (Some(vu), Some(pu)) if vu != pu => {
                    if vu.dim() != pu.dim() {
                        return Err(format!("unit {} does not match {}", unit_key(vu), unit_key(pu)));
                    }
                    Ok(Lit::Num(vu.to_si(x) / pu.si_factor()))
                }
                _ => Ok(Lit::Num(x)),
            }
        }
        _ => Err("expected a number, boolean or text".into()),
    }
}

fn param_def(p: &FamilyParam) -> Result<ParamDef, String> {
    let kind = match p.kind {
        ParamKind::Text => Kind::Text,
        ParamKind::Integer => Kind::Int,
        ParamKind::Number => Kind::Number,
        ParamKind::Length => Kind::Length,
        ParamKind::Area => Kind::Area,
        ParamKind::Volume => Kind::Volume,
        ParamKind::Angle => Kind::Angle,
        ParamKind::Bool => Kind::Bool,
        ParamKind::Ref => return Err(format!("parameter {}: ref parameters are not lowered yet", p.name)),
    };
    let unit = p.unit.or(p.default.unit);
    let default = match (&p.formula, &p.default.v) {
        (Some(_), _) | (None, serde_json::Value::Null) => None,
        (None, _) => Some(lit(&p.default, unit).map_err(|e| format!("parameter {}: {e}", p.name))?),
    };
    Ok(ParamDef {
        name: p.name.clone(),
        kind,
        instance: p.instance,
        unit: unit.map(unit_key),
        default,
        formula: p.formula.clone(),
        min: None,
        max: None,
    })
}

fn mm(v: f64) -> String {
    format!("{}mm", v * 1000.0)
}

fn axis(a: Axis) -> GAxis {
    match a {
        Axis::X => GAxis::X,
        Axis::Y => GAxis::Y,
        Axis::Z => GAxis::Z,
    }
}

/// A plane with a formula is fixed by it; otherwise the constraint sketch places it from `offset`.
fn plane_def(p: &RefPlaneDef) -> RefDef {
    RefDef {
        name: p.name.clone(),
        axis: axis(p.axis),
        at: match &p.formula {
            Some(f) => RefAt::Formula(f.clone()),
            None => RefAt::Free(mm(p.offset.to_m())),
        },
    }
}

fn cons_def(c: &FamilyRule) -> Result<ConsDef, String> {
    match (c.kind, c.planes.as_slice(), &c.param) {
        (RuleKind::Distance, [a, b], Some(v)) => Ok(ConsDef::Dim { value: v.clone(), a: a.clone(), b: b.clone() }),
        (RuleKind::Equal, ps, None) if ps.len() >= 2 => Ok(ConsDef::Eq(ps.to_vec())),
        (RuleKind::Align, [a, b], None) => Ok(ConsDef::Align(a.clone(), b.clone())),
        (k, _, _) => Err(format!("family constraint {k:?} with these planes is not lowered yet")),
    }
}

fn profile_def(p: &Profile, o: [f64; 3]) -> Result<ProfileDef, String> {
    Ok(match p {
        Profile::Rect { w, h } => {
            let (w, h) = (w.to_m() / 2.0, h.to_m() / 2.0);
            ProfileDef::Rect([mm(o[0] - w), mm(o[1] - h), mm(o[0] + w), mm(o[1] + h)])
        }
        Profile::Circle { r } => ProfileDef::Circle([mm(o[0]), mm(o[1]), mm(r.to_m())]),
        Profile::Polygon { pts } => ProfileDef::Poly(
            pts.iter()
                .map(|q| {
                    let [x, y] = q.to_m();
                    [mm(o[0] + x), mm(o[1] + y)]
                })
                .collect(),
        ),
        _ => return Err("only rect, circle and polygon profiles are lowered into families yet".into()),
    })
}

/// Forms with fixed sizes, extruded up from a frame that is not turned.
fn form_def(f: &FamilyForm) -> Result<FormDef, String> {
    if f.visible_if.is_some() {
        return Err("visibility conditions are not lowered yet".into());
    }
    let Form::Extrude { profile, frame, start, end } = &f.form else {
        return Err("only extrusions are lowered into families yet".into());
    };
    let (x, z) = (frame.x, frame.z);
    let upright = (z.x.abs() + z.y.abs()) <= EPS && z.z > 0.0 && (x.y.abs() + x.z.abs()) <= EPS && x.x > 0.0;
    if !upright {
        return Err("only frames aligned with the family axes are lowered yet".into());
    }
    let o = frame.origin.to_m();
    Ok(FormDef {
        name: f.name.clone(),
        shape: FormShape::Extrude {
            profile: profile_def(profile, o)?,
            dir: GAxis::Z,
            from: mm(o[2] + start.to_m()),
            to: mm(o[2] + end.to_m()),
        },
        void: false,
        material: f.material.clone(),
    })
}

/// Family text with the `param` line of `name` set to `= formula` (or to no value).
fn with_formula(def: &str, name: &str, formula: Option<&str>) -> Result<String, String> {
    if let Some(f) = formula {
        if f.trim().is_empty() || f.contains(['\n', '\r', '#']) {
            return Err("formula must be one non-empty line".into());
        }
    }
    let mut hit = false;
    let mut out = String::with_capacity(def.len() + 16);
    for line in def.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        if w.len() >= 4 && w[0] == "param" && w[1] == name {
            hit = true;
            out.push_str(&format!("param {} {} {}", w[1], w[2], w[3]));
            if let Some(f) = formula {
                out.push_str(&format!(" = {}", f.trim()));
            }
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !hit {
        return Err(format!("no parameter {name}"));
    }
    Ok(out)
}

impl Model {
    /// Sets (or clears) the formula of a family parameter after a flex check of every type.
    /// A refusal is `Ok(Err((path, reason)))` with `/param` or `/formula`; `Err` is storage.
    pub fn set_family_formula(
        &mut self,
        id: i64,
        param: &str,
        formula: Option<&str>,
    ) -> Result<Result<(), (String, String)>, Error> {
        let def: String = self
            .db
            .query_row("SELECT def FROM comp_family WHERE id=?1", [id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no component family {id}")))?;
        let new = match with_formula(&def, param, formula) {
            Ok(t) => t,
            Err(s) => return Ok(Err(("/param".into(), s))),
        };
        let src = formula.unwrap_or("(none)");
        let f = match Family::parse(&new) {
            Ok(f) => f,
            Err(e) => return Ok(Err(("/formula".into(), format!("formula {param} = {src}: {e}")))),
        };
        let bases: Vec<Vec<_>> = if f.types.is_empty() {
            vec![vec![]]
        } else {
            f.types.iter().map(|t| t.values.clone()).collect()
        };
        for b in &bases {
            if let Some(e) = flex::try_values(&f, b, &[]).failure {
                return Ok(Err(("/formula".into(), format!("formula {param} = {src}: {e}"))));
            }
        }
        self.reload_family(id, &new)?;
        Ok(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ops::{Actor, Dir3, Frame, Group, Len, P3};

    use crate::{LayerSpec, WallSpec};

    fn model() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let wt = m
            .add_wall_type("W200", &[LayerSpec { material: "C".into(), thickness: 0.2 }])
            .unwrap();
        (m, lv, wt)
    }

    fn add_wall(m: &mut Model, lv: i64, wt: i64, a: (f64, f64), b: (f64, f64)) -> i64 {
        m.add_wall(&WallSpec { a, b, level: lv, base_offset: 0.0, height: 3.0, wall_type: wt, offset0: None })
            .unwrap()
    }

    fn one(op: Op) -> Envelope {
        let mut e = Envelope::new(Actor::Agent { name: "test".into() }, "pattern test");
        e.groups = vec![Group { id: 1, title: "g".into(), ops: vec![op], depends_on: vec![], why: String::new() }];
        e
    }

    fn pattern_of(src: i64, pattern: Pattern) -> Op {
        Op::Pattern {
            source: Targets::Refs { refs: vec![Ref::id(Cat::Wall, src)] },
            pattern,
            out: PatternOut::Copies,
        }
    }

    fn wall_a(m: &Model, id: i64) -> (f64, f64) {
        m.sql()
            .query_row("SELECT ax, ay FROM wall WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
    }

    fn walls(m: &Model) -> i64 {
        m.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn envelope_array_along_arc_count() {
        let (mut m, lv, wt) = model();
        let src = add_wall(&mut m, lv, wt, (0.0, 0.0), (0.0, 0.5));
        let arc = Path::Polyline {
            pts: vec![P3::m(0.0, 0.0, 0.0), P3::m(10.0, 0.0, 0.0)],
            bulges: vec![1.0],
            closed: false,
        };
        let p = Pattern::AlongCurve {
            path: arc,
            count: Some(5),
            spacing: None,
            align: CurveAlign::Tangent,
            start_offset: Len::m(0.0),
        };
        let r = m.apply_envelope(&one(pattern_of(src, p))).unwrap();
        assert_eq!(r.copied.len(), 4, "{r:?}");
        assert_eq!(walls(&m), 5);
        // a half circle about (5, 0): every copy starts on it, the last at the path end
        let mut last = (0.0, 0.0);
        for c in &r.copied {
            let Ref::Id { cat: Cat::Wall, id } = c else { panic!("{c:?}") };
            let a = wall_a(&m, *id);
            assert!(((a.0 - 5.0).hypot(a.1) - 5.0).abs() < 1e-6, "{a:?}");
            last = a;
        }
        assert!((last.0 - 10.0).abs() < 1e-6 && last.1.abs() < 1e-6, "{last:?}");
        m.undo().unwrap();
        assert_eq!(walls(&m), 1);
    }

    #[test]
    fn envelope_distribute_between_walls() {
        let (mut m, lv, wt) = model();
        let a = add_wall(&mut m, lv, wt, (0.0, 0.0), (10.0, 0.0));
        let b = add_wall(&mut m, lv, wt, (0.0, 10.0), (10.0, 10.0));
        let i1 = add_wall(&mut m, lv, wt, (0.0, 1.0), (10.0, 1.0));
        let i2 = add_wall(&mut m, lv, wt, (0.0, 2.0), (10.0, 2.0));
        let op = Op::Pattern {
            source: Targets::Refs { refs: vec![Ref::id(Cat::Wall, i1), Ref::id(Cat::Wall, i2)] },
            pattern: Pattern::Distribute {
                a: Ref::id(Cat::Wall, a),
                b: Ref::id(Cat::Wall, b),
                count: 2,
                ends: false,
            },
            out: PatternOut::Copies,
        };
        let r = m.apply_envelope(&one(op)).unwrap();
        assert!(r.copied.is_empty());
        assert_eq!(r.changed.len(), 2, "{r:?}");
        let (y1, y2) = (wall_a(&m, i1).1, wall_a(&m, i2).1);
        // equal walls: equal gaps mean equal axis spacing
        assert!((y1 - 10.0 / 3.0).abs() < 1e-6, "{y1}");
        assert!((y2 - 20.0 / 3.0).abs() < 1e-6, "{y2}");
        assert_eq!(walls(&m), 4);
        // a count that differs from the sources is refused, nothing moves
        let op = Op::Pattern {
            source: Targets::Refs { refs: vec![Ref::id(Cat::Wall, i1)] },
            pattern: Pattern::Distribute {
                a: Ref::id(Cat::Wall, a),
                b: Ref::id(Cat::Wall, b),
                count: 3,
                ends: false,
            },
            out: PatternOut::Copies,
        };
        let e = m.apply_envelope(&one(op)).unwrap_err();
        assert_eq!(e.code, Code::Invalid);
        assert!((wall_a(&m, i1).1 - y1).abs() < 1e-12);
    }

    fn width(default_mm: Option<f64>, formula: Option<&str>) -> FamilyParam {
        FamilyParam {
            name: "Width".into(),
            kind: ParamKind::Length,
            unit: Some(Unit::Mm),
            default: match default_mm {
                Some(v) => Value::num(v, Unit::Mm),
                None => Value::plain(serde_json::Value::Null),
            },
            formula: formula.map(str::to_string),
            instance: false,
        }
    }

    fn define(name: &str, params: Vec<FamilyParam>) -> Op {
        let board = FamilyForm {
            name: "Board".into(),
            form: Form::Extrude {
                profile: Profile::Rect { w: Len::mm(1000.0), h: Len::mm(500.0) },
                frame: Frame { origin: P3::m(0.0, 0.0, 0.0), x: Dir3::X, z: Dir3::Z },
                start: Len::mm(0.0),
                end: Len::mm(700.0),
            },
            visible_if: None,
            material: None,
        };
        Op::Family(FamilyOp::Define {
            slot: 1,
            name: name.into(),
            cat: Cat::Component,
            params,
            planes: vec![],
            forms: vec![board],
            constraints: vec![],
            tests: vec![vec![ddc_ops::Field { name: "Width".into(), value: Value::num(900.0, Unit::Mm) }]],
        })
    }

    #[test]
    fn envelope_family_define_is_draft() {
        let (mut m, _, _) = model();
        let r = m.apply_envelope(&one(define("AiBox", vec![width(Some(1200.0), None)]))).unwrap();
        let [(1, Ref::Id { cat: Cat::Family, id })] = r.created.as_slice() else {
            panic!("{r:?}")
        };
        assert!(m.family_is_draft(*id).unwrap());
        // a formula cycle is refused with the path of the parameter that holds it
        let mut cyc = width(None, Some("Depth"));
        cyc.name = "Depth".into();
        cyc.formula = Some("Width".into());
        let bad = define("AiLoop", vec![width(None, Some("Depth")), cyc]);
        let e = m.apply_envelope(&one(bad)).unwrap_err();
        assert!(e.path.starts_with("/groups/0/ops/0/params/"), "{e:?}");
        assert!(e.to_string().contains("formula"), "{e:?}");
        assert!(m.family_id("AiLoop").unwrap().is_none());
        // set_formula on the draft: a broken formula is refused with its path
        let set = Op::Family(FamilyOp::SetFormula {
            target: Ref::name(Cat::Family, "AiBox"),
            param: "Width".into(),
            formula: Some("Width + 1mm".into()),
        });
        let e = m.apply_envelope(&one(set)).unwrap_err();
        assert_eq!(e.path, "/groups/0/ops/0/formula", "{e:?}");
    }

    #[test]
    fn pattern_count_cap_rejected() {
        let (mut m, lv, wt) = model();
        let src = add_wall(&mut m, lv, wt, (0.0, 0.0), (1.0, 0.0));
        let p = Pattern::Linear {
            count: MAX_PATTERN_COPIES as u32 + 1,
            dir: Dir3::Y,
            spacing: OSpacing::Step { d: Len::m(1.0) },
        };
        let e = m.apply_envelope(&one(pattern_of(src, p))).unwrap_err();
        assert_eq!(e.code, Code::TooMany);
        assert!(e.to_string().contains("out_of_range"), "{e:?}");
        assert_eq!(e.path, "/groups/0/ops/0");
        assert_eq!(walls(&m), 1);
        // a spacing that would make too many copies along a path is the same refusal
        let p = Pattern::AlongCurve {
            path: Path::Polyline {
                pts: vec![P3::m(0.0, 0.0, 0.0), P3::m(100.0, 0.0, 0.0)],
                bulges: vec![],
                closed: false,
            },
            count: None,
            spacing: Some(Len::mm(1.0)),
            align: CurveAlign::Fixed,
            start_offset: Len::m(0.0),
        };
        let e = m.apply_envelope(&one(pattern_of(src, p))).unwrap_err();
        assert_eq!(e.code, Code::TooMany);
        assert!(e.to_string().contains("out_of_range"), "{e:?}");
        assert_eq!(walls(&m), 1);
    }
}
