//! Structural analysis job (docs/design/STRUCTURAL_ANALYSIS.md 1.1, 1.6, 1.7): the analytical
//! model with its keys and overrides plus the stored loads become the solver input
//! ([`adapter`]), loads are expanded to member and nodal loads ([`loads`]), the frame is solved
//! by `ddc-fem` and the run is stored with its input hash and an equilibrium check ([`run`]).
//! `crates/fem` owns the numbers; this crate owns translation and meaning. SI units.

pub mod adapter;
mod blob;
pub mod loads;
pub mod run;
pub mod tributary;

pub use adapter::{build_input, CalcInput, CaseLoads, Seg};
pub use blob::{read_blob, write_blob, Blob};
pub use run::{run, solve_input, CaseCheck, RunOptions, RunOutcome};

/// Solver identity stored in `calc_run.fem_version`.
pub const FEM_VERSION: &str = "ddc-fem linear-frame 1";
/// Relative equilibrium and load-conservation tolerance (design 7.2).
pub const EQ_TOL: f64 = 1e-9;
/// Standard gravity, m/s2.
pub const G: f64 = 9.80665;

/// Resolves a `filter:<query>` load target to stable keys (the query DSL lives in the app).
pub type Resolver<'a> = &'a dyn Fn(&str) -> Result<Vec<String>, String>;

/// A finding of the job; `blocking` ones stop the run (no_section, no_material, unstable).
#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub code: &'static str,
    /// Stable key of the item (member, node, load `l:<id>`, case `lc:<id>`).
    pub key: String,
    pub blocking: bool,
    pub note: String,
}

impl Issue {
    pub fn warn(code: &'static str, key: impl Into<String>, note: impl Into<String>) -> Self {
        Issue { code, key: key.into(), blocking: false, note: note.into() }
    }
    pub fn block(code: &'static str, key: impl Into<String>, note: impl Into<String>) -> Self {
        Issue { code, key: key.into(), blocking: true, note: note.into() }
    }
}

#[derive(Debug)]
pub enum CalcError {
    Model(ddc_model::Error),
    /// Blocking issues: nothing was solved or stored.
    Blocked(Vec<Issue>),
}

impl From<ddc_model::Error> for CalcError {
    fn from(e: ddc_model::Error) -> Self {
        CalcError::Model(e)
    }
}

impl std::fmt::Display for CalcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CalcError::Model(e) => write!(f, "{e}"),
            CalcError::Blocked(v) => {
                let codes: Vec<String> = v
                    .iter()
                    .filter(|i| i.blocking)
                    .map(|i| format!("{} {} ({})", i.code, i.key, i.note))
                    .collect();
                write!(f, "analysis blocked: {}", codes.join("; "))
            }
        }
    }
}

impl std::error::Error for CalcError {}

pub(crate) type P3 = [f64; 3];

pub(crate) fn add3(a: P3, b: P3) -> P3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub(crate) fn sub3(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub(crate) fn mul3(a: P3, k: f64) -> P3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
pub(crate) fn dot3(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub(crate) fn cross3(a: P3, b: P3) -> P3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
pub(crate) fn norm3(a: P3) -> f64 {
    dot3(a, a).sqrt()
}
pub(crate) fn unit3(a: P3) -> P3 {
    let l = norm3(a);
    if l > 0.0 {
        mul3(a, 1.0 / l)
    } else {
        [0.0; 3]
    }
}

/// Adds force `f` at point `p` (and couple `m`) to a resultant about the origin.
pub(crate) fn push_resultant(acc: &mut [f64; 6], p: P3, f: P3, m: P3) {
    let r = cross3(p, f);
    for k in 0..3 {
        acc[k] += f[k];
        acc[3 + k] += r[k] + m[k];
    }
}

#[cfg(test)]
mod tests;
