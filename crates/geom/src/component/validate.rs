//! Single validation pipeline for family definition text (including AI-written text): parsing,
//! formulas, sketch via the solver, every catalog type, check mode (flex), kernel bodies, budgets.
//! No code is executed: the text is data only. The report holds stable codes (`CODES`) and 1-based
//! source line numbers; a failing stage stops the pipeline and later stages do not run.
//!
//! Stage order: `limits` (text size) → `parse` → `budget` (body count, types, nesting;
//! cheap, hence before the heavy stages) → `formulas` → `sketch` → `types` → `flex` → `kernel`.
//! Time is checked between steps (`eval_timeout`), not by a killer thread.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use super::doc::{stmts, Stmt};
use super::ext::Cons;
use super::flex::{self, Failure};
use super::{Built, Family, FamilyError, Kind, Shape, Value, OUT_OF_RANGE};
use crate::sketch_solve::Verdict;

/// All report codes; the list is stable (pinned by a test).
pub const CODES: [&str; 12] = [
    "parse_error",
    "unknown_ref",
    "formula_cycle",
    "sketch_conflict",
    "sketch_underconstrained",
    "type_fails",
    "flex_fails",
    "nonmanifold",
    "zero_volume",
    "too_large",
    "budget",
    "eval_timeout",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warn,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub code: &'static str,
    pub severity: Severity,
    /// Source text lines (1-based), ascending, without duplicates.
    pub lines: Vec<usize>,
    /// Detail from the check (English, for machines and AI).
    pub msg: String,
    /// What to fix (English).
    pub hint: String,
}

impl Finding {
    /// i18n key of the title (ru/en); the hint is the same key with the `.hint` suffix.
    pub fn key(&self) -> String {
        format!("fam.val.{}", self.code)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub lines: usize,
    pub bodies: usize,
    pub types: usize,
    pub params: usize,
    /// Last stage started.
    pub stage: &'static str,
    pub ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub ok: bool,
    pub findings: Vec<Finding>,
    pub stats: Stats,
}

impl Report {
    pub fn has(&self, code: &str) -> bool {
        self.findings.iter().any(|f| f.code == code)
    }
    pub fn find(&self, code: &str) -> Option<&Finding> {
        self.findings.iter().find(|f| f.code == code)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    pub lines: usize,
    pub bodies: usize,
    pub nest_depth: usize,
    pub types: usize,
    /// Largest side of the body's bounding box, m.
    pub extent_m: f64,
    pub eval_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            lines: 2000,
            bodies: 200,
            nest_depth: 3,
            types: 200,
            extent_m: 50.0,
            eval_ms: 500,
        }
    }
}

struct Run<'a> {
    lim: &'a Limits,
    start: Instant,
    findings: Vec<Finding>,
    stats: Stats,
    st: Vec<Stmt>,
    cons_all: Vec<Cons>,
}

fn finding(
    code: &'static str,
    severity: Severity,
    lines: Vec<usize>,
    msg: impl Into<String>,
    hint: &str,
) -> Finding {
    let lines: BTreeSet<usize> = lines.into_iter().collect();
    Finding {
        code,
        severity,
        lines: lines.into_iter().collect(),
        msg: msg.into(),
        hint: hint.into(),
    }
}

const SOLID_HEADS: [&str; 6] = ["extrude", "revolve", "sweep", "blend", "mesh", "feature"];

fn is_unknown(msg: &str) -> bool {
    ["unknown name", "unknown or solved name", "unknown plane", "unknown parameter"]
        .iter()
        .any(|k| msg.contains(k))
}

fn nest_depth(f: &Family) -> usize {
    f.ext
        .children
        .iter()
        .map(|c| 1 + nest_depth(c))
        .max()
        .unwrap_or(0)
}

impl Run<'_> {
    fn err(&mut self, f: Finding) {
        self.findings.push(f);
    }

    fn has_error(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    fn timed_out(&mut self) -> bool {
        if self.start.elapsed() > Duration::from_millis(self.lim.eval_ms) {
            self.err(finding(
                "eval_timeout",
                Severity::Error,
                vec![],
                format!("checking took longer than {} ms", self.lim.eval_ms),
                "Simplify the definition: fewer bodies, types or nested families.",
            ));
            return true;
        }
        false
    }

    /// Operator lines (1-based) by head and name; a name equal to the head means an unnamed operator.
    fn lines_named(&self, heads: &[&str], name: &str) -> Vec<usize> {
        self.st
            .iter()
            .filter(|s| (heads.is_empty() || heads.contains(&s.head.as_str())) && s.name == name)
            .map(|s| s.first + 1)
            .collect()
    }

    fn lines_by_name(&self, name: &str) -> Vec<usize> {
        self.st
            .iter()
            .filter(|s| s.name == name || s.head == name)
            .map(|s| s.first + 1)
            .collect()
    }

    /// Line of the k-th dimension/equality/alignment/angle operator (index in `Ext::cons`).
    fn cons_line(&self, k: usize) -> Option<usize> {
        self.st
            .iter()
            .filter(|s| matches!(s.head.as_str(), "dim" | "eq" | "align" | "angle"))
            .nth(k)
            .map(|s| s.first + 1)
    }

    /// Family error → finding; `fallback` is the code used when the error is not a special one.
    fn from_error(&self, e: &FamilyError, fallback: &'static str, extra: &[usize]) -> Finding {
        let (code, mut lines, hint): (&'static str, Vec<usize>, &str) = match e {
            FamilyError::Parse { line, msg } => {
                let (c, h) = if is_unknown(msg) {
                    ("unknown_ref", "Declare the name with param or ref, or fix the spelling.")
                } else {
                    ("parse_error", "Fix the statement; one statement per line, see the definition syntax.")
                };
                (c, vec![*line], h)
            }
            FamilyError::Cycle(names) => {
                let mut l = vec![];
                for n in names {
                    l.extend(self.lines_named(&["param", "ref"], n));
                }
                (
                    "formula_cycle",
                    l,
                    "A formula depends on itself through other names; break the loop.",
                )
            }
            FamilyError::Param { name, msg } => {
                if let Some(rest) = msg.strip_prefix("constraints conflict: ") {
                    let mut l = vec![];
                    for label in rest.split("; ") {
                        // labels match `Cons::label`; duplicate labels make both candidates
                        for (k, c) in self.cons_all.iter().enumerate() {
                            if c.label() == label {
                                l.extend(self.cons_line(k));
                            }
                        }
                    }
                    (
                        "sketch_conflict",
                        l,
                        "These dimensions cannot all hold at once; remove or relax one of them.",
                    )
                } else if is_unknown(msg) {
                    (
                        "unknown_ref",
                        self.lines_by_name(name),
                        "Declare the name with param or ref, or fix the spelling.",
                    )
                } else {
                    (
                        fallback,
                        self.lines_by_name(name),
                        "Fix the value or the formula of this parameter.",
                    )
                }
            }
            FamilyError::Geometry { solid, msg } => {
                let zero = ["no area", "no depth", "no volume", "must be positive", "zero-length", "above bottom"];
                let bad = ["crosses itself", "folds back", "reaches outside", "exceed"];
                let (c, h) = if zero.iter().any(|k| msg.contains(k)) {
                    ("zero_volume", "Give the body a positive size in every direction.")
                } else if bad.iter().any(|k| msg.contains(k)) {
                    ("nonmanifold", "Fix the outline or the cut so the body is a valid closed solid.")
                } else {
                    (fallback, "Fix the body definition.")
                };
                (c, self.lines_by_name(solid), h)
            }
        };
        lines.extend_from_slice(extra);
        finding(code, Severity::Error, lines, e.to_string(), hint)
    }
}

/// Validates the whole definition text.
pub fn validate(text: &str, lim: &Limits) -> Report {
    let mut run = Run {
        lim,
        start: Instant::now(),
        findings: vec![],
        stats: Stats {
            lines: text.lines().count(),
            ..Stats::default()
        },
        st: vec![],
        cons_all: vec![],
    };
    pipeline(&mut run, text);
    run.stats.ms = run.start.elapsed().as_millis() as u64;
    Report {
        ok: !run.has_error(),
        findings: run.findings,
        stats: run.stats,
    }
}

fn pipeline(run: &mut Run, text: &str) {
    let lim = run.lim;
    run.stats.stage = "limits";
    if run.stats.lines > lim.lines {
        let n = run.stats.lines;
        run.err(finding(
            "budget",
            Severity::Error,
            vec![lim.lines + 1],
            format!("{n} lines, limit {}", lim.lines),
            "Split the family: move parts into nested begin … end blocks or smaller families.",
        ));
        return;
    }
    run.st = stmts(text);

    run.stats.stage = "parse";
    let f = match Family::parse(text) {
        Ok(f) => f,
        Err(e) => {
            let fd = run.from_error(&e, "parse_error", &[]);
            run.err(fd);
            return;
        }
    };
    run.cons_all = f.ext.cons.clone();
    run.stats.bodies = f.solids.len();
    run.stats.types = f.types.len();
    run.stats.params = f.params.len();
    if run.timed_out() {
        return;
    }

    run.stats.stage = "budget";
    let depth = nest_depth(&f);
    let over: Vec<(bool, String, Vec<&str>)> = vec![
        (f.solids.len() > lim.bodies, format!("{} bodies, limit {}", f.solids.len(), lim.bodies), SOLID_HEADS.to_vec()),
        (f.types.len() > lim.types, format!("{} types, limit {}", f.types.len(), lim.types), vec!["type"]),
        (depth > lim.nest_depth, format!("nesting depth {depth}, limit {}", lim.nest_depth), vec!["begin"]),
    ];
    for (bad, msg, heads) in over {
        if bad {
            let l: Vec<usize> = run.st.iter().filter(|s| heads.contains(&s.head.as_str())).map(|s| s.first + 1).take(5).collect();
            run.err(finding(
                "budget",
                Severity::Error,
                l,
                msg,
                "Reduce the count: use arrays of types, parameters or nested families.",
            ));
        }
    }
    if run.has_error() {
        return;
    }

    run.stats.stage = "formulas";
    // defaults: formulas, names, limits
    let base: Vec<(String, Value)> = f.types.first().map(|t| t.values.clone()).unwrap_or_default();
    let default = f.resolve(&[], &[]);
    run.stats.stage = "sketch";
    if let Err(e) = &default {
        let fd = run.from_error(e, "parse_error", &[]);
        run.err(fd);
        return;
    }
    let r0 = default.expect("checked");
    if !f.ext.is_empty_rig() {
        if let Ok(rep) = f.rig_report(&r0) {
            if rep.verdict == Verdict::Under {
                let mut l = vec![];
                for n in &rep.free {
                    l.extend(run.lines_named(&["ref", "refline"], n));
                }
                run.err(finding(
                    "sketch_underconstrained",
                    Severity::Warn,
                    l,
                    format!("free: {}", rep.free.join(", ")),
                    "Add dim, eq or align so the free planes are determined, or pin them with '='.",
                ));
            }
        }
    }
    if run.timed_out() {
        return;
    }

    run.stats.stage = "types";
    let mut built: Vec<(String, Built)> = vec![];
    let targets: Vec<(String, Vec<(String, Value)>)> = if f.types.is_empty() {
        vec![(String::new(), vec![])]
    } else {
        f.types.iter().map(|t| (t.name.clone(), t.values.clone())).collect()
    };
    for (name, vals) in &targets {
        if run.timed_out() {
            return;
        }
        let tline = if name.is_empty() { vec![] } else { run.lines_named(&["type"], name) };
        let r = match f.resolve(vals, &[]) {
            Ok(r) => r,
            Err(e) => {
                let mut fd = run.from_error(&e, "type_fails", &tline);
                fd.msg = format!("type '{name}': {}", fd.msg);
                run.err(fd);
                continue;
            }
        };
        match f.build(&r, super::Lod::Medium) {
            Ok(b) => built.push((name.clone(), b)),
            Err(e) => {
                let mut fd = run.from_error(&e, "type_fails", &tline);
                fd.msg = format!("type '{name}': {}", fd.msg);
                run.err(fd);
            }
        }
    }
    if run.has_error() {
        return;
    }

    run.stats.stage = "flex";
    let flex_base = if f.types.is_empty() { vec![] } else { base };
    if let Ok(rb) = f.resolve(&flex_base, &[]) {
        for p in &f.params {
            if run.timed_out() {
                return;
            }
            let free_to_set = p.formula.is_none() && !f.ext.has_text_formula(&p.name);
            let Some(v) = rb.num(&p.name).filter(|v| *v != 0.0 && free_to_set) else {
                continue;
            };
            let trials: Vec<f64> = match p.kind {
                Kind::Length => vec![v * 0.5, v * 2.0],
                Kind::Angle => {
                    let d = 15f64.to_radians();
                    vec![v - d, v + d]
                }
                _ => continue,
            };
            for t in trials {
                let o = flex::try_values(&f, &flex_base, &[(p.name.clone(), Value::Num(t))]);
                let Some(fail) = &o.failure else { continue };
                if matches!(fail, Failure::Resolve(FamilyError::Param { msg, .. }) if msg.starts_with(OUT_OF_RANGE)) {
                    continue;
                }
                let l = run.lines_named(&["param"], &p.name);
                run.err(finding(
                    "flex_fails",
                    Severity::Warn,
                    l,
                    format!("{} = {t}: {fail}", p.name),
                    "Make the dependent geometry follow this parameter, or add a limit to forbid the range.",
                ));
                break;
            }
        }
    }

    run.stats.stage = "kernel";
    let flat = f.solids.iter().any(|s| matches!(s.shape, Shape::Mesh { .. } | Shape::Feature { .. }));
    for (name, b) in &built {
        let who = if name.is_empty() { String::new() } else { format!("type '{name}': ") };
        let tline = if name.is_empty() { vec![] } else { run.lines_named(&["type"], name) };
        if !flat && !b.mesh.triangles.is_empty() && !b.mesh.is_watertight() {
            run.err(finding(
                "nonmanifold",
                Severity::Error,
                tline.clone(),
                format!("{who}the body mesh is not closed"),
                "Fix the outline or the cut so the body is a valid closed solid.",
            ));
        }
        for p in b.parts.iter().filter(|p| !p.void && p.volume <= 1e-12) {
            let is_mesh = f.solids.iter().any(|s| s.name == p.name && matches!(s.shape, Shape::Mesh { .. }));
            if !is_mesh {
                let mut l = run.lines_named(&SOLID_HEADS, &p.name);
                l.extend(tline.iter().copied());
                run.err(finding(
                    "zero_volume",
                    Severity::Error,
                    l,
                    format!("{who}body '{}' has zero volume", p.name),
                    "Give the body a positive size in every direction.",
                ));
            }
        }
        if !flat && b.volume <= 0.0 && f.solids.iter().any(|s| !s.void) {
            let mut l: Vec<usize> = run.st.iter().filter(|s| SOLID_HEADS.contains(&s.head.as_str())).map(|s| s.first + 1).collect();
            l.extend(tline.iter().copied());
            run.err(finding(
                "zero_volume",
                Severity::Error,
                l,
                format!("{who}the family has zero volume"),
                "Voids must not remove the whole family; check the cut sizes.",
            ));
        }
        let ext = [b.bbox[1].x - b.bbox[0].x, b.bbox[1].y - b.bbox[0].y, b.bbox[1].z - b.bbox[0].z]
            .into_iter()
            .fold(0.0, f64::max);
        if ext > lim.extent_m {
            run.err(finding(
                "too_large",
                Severity::Error,
                tline,
                format!("{who}size {ext:.1} m, limit {} m", lim.extent_m),
                "Check units: lengths take mm, m, in; a family is one product, not a building.",
            ));
        }
        if run.timed_out() {
            return;
        }
    }
}
