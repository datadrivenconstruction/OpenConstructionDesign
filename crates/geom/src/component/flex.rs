//! Test mode ("flex"): trial parameter values -> evaluation, sketch, body; a failure
//! names its cause (formula, constraint conflict, dimension or angle changed sign, geometry).
//! Trial values override type and instance values (like nested-component bindings).

use super::ext::Cons;
use super::rig::RigReport;
use super::{Built, Family, FamilyError, Lod, Resolved, Value};
use crate::sketch_solve::Verdict;

#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    /// Formula, value or constraint conflict.
    Resolve(FamilyError),
    /// A parametric dimension or angle changed sign relative to the type value (or collapsed
    /// to zero): planes/lines swapped places. A zero type value (a legitimate zero
    /// offset) is not checked.
    Flipped {
        dim: String,
        value: f64,
    },
    Geometry(FamilyError),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Failure::Resolve(e) | Failure::Geometry(e) => write!(f, "{e}"),
            Failure::Flipped { dim, value } => {
                write!(
                    f,
                    "{dim}: {value} changes sign against the type value, planes flip"
                )
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub values: Vec<(String, Value)>,
    pub failure: Option<Failure>,
    pub verdict: Option<Verdict>,
    /// Planes not defined by the sketch (a warning, not a failure).
    pub free: Vec<String>,
    pub resolved: Option<Resolved>,
    pub built: Option<Built>,
}

impl Outcome {
    pub fn ok(&self) -> bool {
        self.failure.is_none()
    }
    pub fn volume(&self) -> Option<f64> {
        self.built.as_ref().map(|b| b.volume)
    }
}

fn merged(base: &[(String, Value)], over: &[(String, Value)]) -> Vec<(String, Value)> {
    let mut v: Vec<(String, Value)> = base
        .iter()
        .filter(|(k, _)| !over.iter().any(|o| o.0 == *k))
        .cloned()
        .collect();
    v.extend(over.iter().cloned());
    v
}

/// Parametric dimensions and angles: (index in `Ext::cons`, label, value; angle in (−π, π]).
fn driven(f: &Family, r: &Resolved) -> Vec<(usize, String, f64)> {
    f.ext
        .cons
        .iter()
        .enumerate()
        .filter_map(|(k, c)| {
            let v = r.num(c.param()?)?;
            Some(match c {
                Cons::Angle { .. } => (k, c.label(), ddc_solver::wrap_angle(v)),
                _ => (k, c.label(), v),
            })
        })
        .collect()
}

/// The sign changed relative to the type value, or the value collapsed to zero.
fn flipped(base: f64, v: f64) -> bool {
    base != 0.0 && (v == 0.0 || v.signum() != base.signum())
}

/// A single trial: type values `base` overridden by `trial`.
pub fn try_values(f: &Family, base: &[(String, Value)], trial: &[(String, Value)]) -> Outcome {
    let reference = f.resolve(base, &[]).ok().map(|r| driven(f, &r));
    try_against(f, base, trial, reference.as_deref())
}

fn try_against(
    f: &Family,
    base: &[(String, Value)],
    trial: &[(String, Value)],
    reference: Option<&[(usize, String, f64)]>,
) -> Outcome {
    let values = merged(base, trial);
    let mut out = Outcome {
        values: trial.to_vec(),
        failure: None,
        verdict: None,
        free: vec![],
        resolved: None,
        built: None,
    };
    let r = match f.resolve(&values, &[]) {
        Ok(r) => r,
        Err(e) => {
            out.failure = Some(Failure::Resolve(e));
            return out;
        }
    };
    if !f.ext.is_empty_rig() {
        if let Ok(RigReport { verdict, free, .. }) = f.rig_report(&r) {
            out.verdict = Some(verdict);
            out.free = free;
        }
    }
    // without type values (the type itself does not build) — positive direction by default
    for (k, dim, v) in driven(f, &r) {
        let b = reference
            .and_then(|rf| rf.iter().find(|x| x.0 == k))
            .map_or(1.0, |x| x.2);
        if flipped(b, v) {
            out.failure = Some(Failure::Flipped { dim, value: v });
            out.resolved = Some(r);
            return out;
        }
    }
    match f.build(&r, Lod::Medium) {
        Ok(b) => out.built = Some(b),
        Err(e) => out.failure = Some(Failure::Geometry(e)),
    }
    out.resolved = Some(r);
    out
}

/// A series of trials.
pub fn flex(f: &Family, base: &[(String, Value)], trials: &[Vec<(String, Value)>]) -> Vec<Outcome> {
    let reference = f.resolve(base, &[]).ok().map(|r| driven(f, &r));
    trials
        .iter()
        .map(|t| try_against(f, base, t, reference.as_deref()))
        .collect()
}

/// Sweep of a numeric parameter from `from` to `to` in `steps` steps (endpoints included).
pub fn sweep(
    f: &Family,
    base: &[(String, Value)],
    param: &str,
    from: f64,
    to: f64,
    steps: usize,
) -> Vec<(f64, Outcome)> {
    let n = steps.clamp(1, 1000);
    let reference = f.resolve(base, &[]).ok().map(|r| driven(f, &r));
    (0..=n)
        .map(|k| {
            let v = from + (to - from) * k as f64 / n as f64;
            let t = [(param.to_string(), Value::Num(v))];
            (v, try_against(f, base, &t, reference.as_deref()))
        })
        .collect()
}

/// The first value in the series at which the family fails.
pub fn first_failure(s: &[(f64, Outcome)]) -> Option<(f64, &Failure)> {
    s.iter()
        .find_map(|(v, o)| o.failure.as_ref().map(|f| (*v, f)))
}
