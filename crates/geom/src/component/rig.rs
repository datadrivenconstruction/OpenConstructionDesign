//! Sketch of a family's reference planes for the `crate::sketch_solve` solver: plane x = c is the line
//! (c, −1)→(c, 1), planes y and z are (−1, c)→(1, c); a pinned plane has both points fixed,
//! a free one has a single degree of freedom (direction locked). A reference line is a point (given
//! by a formula) and a unit segment at an angle. Dimensions, equalities and alignments are equations
//! tagged with their index in `Ext::cons`; the report returns indices (no matching by text).

use super::expr::{self, ANGLE, LENGTH, Q};
use super::ext::{AngleSrc, Cons};
use super::{Axis, Family, FamilyError, Resolved};
use crate::sketch_solve::{self as sketch, Geo, Options, Sketch, Verdict, L};

/// Sketch entity: a name and its line.
#[derive(Clone, Debug, PartialEq)]
pub struct Entity {
    pub name: String,
    /// None for a reference line.
    pub axis: Option<Axis>,
    pub free: bool,
    pub line: L,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RigReport {
    pub verdict: Verdict,
    pub dof: usize,
    pub entities: Vec<Entity>,
    /// Solved values of free planes (length) and lines (angle).
    pub values: Vec<(String, Q)>,
    /// Planes and lines that are not fully determined.
    pub free: Vec<String>,
    /// Redundant constraints (labels).
    pub redundant: Vec<String>,
    /// Conflicting constraints (label, residual): a minimal set, culprit first.
    pub conflicting: Vec<(String, f64)>,
    /// The same redundant constraints as indices into `Ext::cons` (the k-th dim/eq/align/angle operator of the text).
    pub redundant_ids: Vec<usize>,
    /// The same conflicting constraints as indices into `Ext::cons`, with residuals.
    pub conflicting_ids: Vec<(usize, f64)>,
    pub sketch: sketch::Report,
}

fn perr(name: &str, msg: impl Into<String>) -> FamilyError {
    FamilyError::Param {
        name: name.into(),
        msg: msg.into(),
    }
}

fn val(r: &Resolved, e: &expr::Expr, want: expr::Dim, who: &str) -> Result<f64, FamilyError> {
    let q = e
        .eval(&|s| r.nums.get(s).copied())
        .map_err(|m| perr(who, m))?;
    if !expr::zero_fits(q, want) {
        return Err(perr(who, format!("expected {want}, got {}", q.d)));
    }
    Ok(q.v)
}

/// Builds the sketch from the family and its computed parameters. `drag` is the plane being dragged and
/// its new position: it is pinned there (everything else adjusts).
pub fn build(
    f: &Family,
    r: &Resolved,
    drag: Option<(&str, f64)>,
) -> Result<(Sketch, Vec<Entity>), FamilyError> {
    let ext = &f.ext;
    let mut sk = Sketch::default();
    let mut ents: Vec<Entity> = vec![];
    // internal constraints carry tag −2 and come first, so user constraints are the ones reported as redundant
    let mut plane = |sk: &mut Sketch, name: &str, ax: Axis, c: f64, free: bool| {
        let (a, b) = match ax {
            Axis::X => (sk.point(c, -1.0), sk.point(c, 1.0)),
            _ => (sk.point(-1.0, c), sk.point(1.0, c)),
        };
        let dragged = drag.filter(|d| d.0 == name).map(|d| d.1);
        match (ax, free, dragged) {
            (Axis::X, false, _) | (Axis::X, true, Some(_)) => {
                let c = dragged.unwrap_or(c);
                sk.add(-2, Geo::Fix(a, c, -1.0));
                sk.add(-2, Geo::Fix(b, c, 1.0));
            }
            (Axis::X, true, None) => {
                sk.add(-2, Geo::FixY(a, -1.0));
                sk.add(-2, Geo::FixY(b, 1.0));
                sk.add(-2, Geo::Vertical((a, b)));
            }
            (_, false, _) | (_, true, Some(_)) => {
                let c = dragged.unwrap_or(c);
                sk.add(-2, Geo::Fix(a, -1.0, c));
                sk.add(-2, Geo::Fix(b, 1.0, c));
            }
            (_, true, None) => {
                sk.add(-2, Geo::FixX(a, -1.0));
                sk.add(-2, Geo::FixX(b, 1.0));
                sk.add(-2, Geo::Horizontal((a, b)));
            }
        }
        ents.push(Entity {
            name: name.to_string(),
            axis: Some(ax),
            free: free && dragged.is_none(),
            line: (a, b),
        });
    };
    let used: Vec<&str> = ext.cons.iter().flat_map(|c| c.names()).collect();
    for rp in &f.refs {
        // pinned planes without constraints do not affect the sketch
        if used.contains(&rp.name.as_str()) || drag.is_some_and(|d| d.0 == rp.name) {
            let c = r.num(&rp.name).unwrap_or(0.0);
            plane(&mut sk, &rp.name, rp.axis, c, false);
        }
    }
    for fr in &ext.free_refs {
        plane(&mut sk, &fr.name, fr.axis, fr.guess, true);
    }
    for l in &ext.lines {
        let (x, y) = (
            val(r, &l.at[0], LENGTH, &l.name)?,
            val(r, &l.at[1], LENGTH, &l.name)?,
        );
        let (a0, free) = match &l.angle {
            AngleSrc::Expr(e) => (val(r, e, ANGLE, &l.name)?, false),
            AngleSrc::Free(g) => (*g, true),
        };
        let p = sk.point(x, y);
        let q = sk.point(x + a0.cos(), y + a0.sin());
        sk.add(-2, Geo::Fix(p, x, y));
        if free {
            sk.add(-2, Geo::Dist(p, q, 1.0));
        } else {
            sk.add(-2, Geo::Fix(q, x + a0.cos(), y + a0.sin()));
        }
        ents.push(Entity {
            name: l.name.clone(),
            axis: None,
            free,
            line: (p, q),
        });
    }
    let ent = |n: &str| ents.iter().find(|e| e.name == n).cloned();
    for (k, c) in ext.cons.iter().enumerate() {
        let tag = k as i64;
        let label = c.label();
        let get = |n: &str| ent(n).ok_or_else(|| perr(&label, format!("unknown plane '{n}'")));
        match c {
            Cons::Dim { value, a, b } => {
                let d = val(r, value, LENGTH, &label)?;
                let (ea, eb) = (get(a)?, get(b)?);
                // plane x points along +y: the signed distance to the left is −Δx
                let s = if ea.axis == Some(Axis::X) { -d } else { d };
                sk.add(tag, Geo::PointLine(eb.line.0, ea.line, s));
            }
            Cons::Align(a, b) => {
                let (ea, eb) = (get(a)?, get(b)?);
                sk.add(tag, Geo::PointLine(eb.line.0, ea.line, 0.0));
            }
            Cons::Eq(ns) => {
                let ls = ns
                    .iter()
                    .map(|n| get(n).map(|e| e.line))
                    .collect::<Result<Vec<_>, _>>()?;
                sk.add(tag, Geo::EqualGaps(ls));
            }
            Cons::Angle { value, a, b } => {
                let th = val(r, value, ANGLE, &label)?;
                let (ea, eb) = (get(a)?, get(b)?);
                sk.add(tag, Geo::Angle(ea.line, eb.line, th));
            }
        }
    }
    Ok((sk, ents))
}

/// Solves the family's sketch for the computed parameters.
pub fn solve(f: &Family, r: &Resolved) -> Result<RigReport, FamilyError> {
    solve_drag(f, r, None)
}

/// Same, with a plane being dragged (edit mode).
pub fn solve_drag(
    f: &Family,
    r: &Resolved,
    drag: Option<(&str, f64)>,
) -> Result<RigReport, FamilyError> {
    let (sk, ents) = build(f, r, drag)?;
    let rep = sketch::solve(&sk, &Options::default());
    let label = |t: i64| {
        f.ext
            .cons
            .get(t.max(0) as usize)
            .filter(|_| t >= 0)
            .map(|c| c.label())
            .unwrap_or_else(|| "reference planes".into())
    };
    let mut values = vec![];
    let mut free = vec![];
    for e in &ents {
        let (p, q) = (rep.pts[e.line.0], rep.pts[e.line.1]);
        let v = match e.axis {
            Some(Axis::X) => expr::q(p[0], LENGTH),
            Some(_) => expr::q(p[1], LENGTH),
            None => expr::q((q[1] - p[1]).atan2(q[0] - p[0]), ANGLE),
        };
        if e.free || drag.is_some_and(|d| d.0 == e.name) {
            values.push((e.name.clone(), v));
        }
        if rep.free_points.contains(&e.line.0) || rep.free_points.contains(&e.line.1) {
            free.push(e.name.clone());
        }
    }
    // internal plane equations (tag −2) are hidden from the user as long as user equations exist
    let user = |t: i64| (t >= 0 && (t as usize) < f.ext.cons.len()).then_some(t as usize);
    let mut redundant_ids: Vec<usize> = rep.redundant.iter().filter_map(|t| user(*t)).collect();
    redundant_ids.dedup();
    let conflicting_ids: Vec<(usize, f64)> = rep
        .conflicting
        .iter()
        .filter_map(|(t, v)| user(*t).map(|k| (k, *v)))
        .collect();
    let redundant: Vec<String> = redundant_ids.iter().map(|k| label(*k as i64)).collect();
    let mut conflicting: Vec<(String, f64)> = conflicting_ids
        .iter()
        .map(|(k, v)| (label(*k as i64), *v))
        .collect();
    if conflicting.is_empty() {
        if let Some((t, v)) = rep.conflicting.first() {
            conflicting.push((label(*t), *v));
        }
    }
    Ok(RigReport {
        verdict: rep.verdict(),
        dof: rep.dof,
        entities: ents,
        values,
        free,
        redundant,
        conflicting,
        redundant_ids,
        conflicting_ids,
        sketch: rep,
    })
}

/// Dragging plane `name` to `at`: `Ok(None)` means the plane can go there (a free
/// plane gets a new initial position); `Ok(Some(set))` means its position is determined
/// by constraints (labels of a minimal conflicting set, culprit first).
pub fn drag_check(
    f: &Family,
    r: &Resolved,
    name: &str,
    at: f64,
) -> Result<Option<Vec<String>>, FamilyError> {
    let rep = solve_drag(f, r, Some((name, at)))?;
    Ok((rep.verdict == Verdict::Conflict)
        .then(|| rep.conflicting.into_iter().map(|c| c.0).collect()))
}

impl Family {
    /// Sketch report for the computed values (editor, validation mode).
    pub fn rig_report(&self, r: &Resolved) -> Result<RigReport, FamilyError> {
        solve(self, r)
    }
}
