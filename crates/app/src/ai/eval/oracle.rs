// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Oracles read the saved model (and the answer text), never the model's own claims about it.
//! A check that cannot be made, because the model has no table for it, FAILS with the reason;
//! it is neither skipped nor a panic.

use super::case::{Case, ChecksO, ClaimO, CountO, GeomO, IfcO, TextO};
use super::facts::{rx, Facts};
use crate::ai::receipt::WriteReceipt;
use crate::editor::Editor;
use serde_json::{Map, Value as J};

#[derive(Clone, Debug, PartialEq)]
pub struct OracleResult {
    pub name: String,
    pub expected: String,
    pub measured: String,
    pub pass: bool,
}

/// What an oracle may look at: the model after the run, the model before it (`None` for a
/// brief from scratch), the answer text, the IFC export when the case asks for one, and the
/// number of blocking findings the base model already had.
pub struct EvalCx<'a> {
    pub ed: &'a Editor,
    pub facts: &'a Facts,
    pub before: Option<&'a Facts>,
    pub answer: &'a str,
    pub ifc: Option<&'a str>,
    pub errors_before: usize,
    /// What the applied plans did, as the person would have been told.
    pub receipts: &'a [WriteReceipt],
}

pub trait Oracle {
    fn name(&self) -> String;
    fn check(&self, cx: &EvalCx) -> OracleResult;
}

pub fn result(name: &str, expected: String, measured: String, pass: bool) -> OracleResult {
    OracleResult { name: name.to_string(), expected, measured, pass }
}

/// The honest failure: the check could not be made.
pub fn no_capability(name: &str, expected: String, why: &str) -> OracleResult {
    result(name, expected, why.to_string(), false)
}

pub fn range_text(min: Option<f64>, max: Option<f64>) -> String {
    match (min, max) {
        (Some(a), Some(b)) => format!("{a}..{b}"),
        (Some(a), None) => format!(">= {a}"),
        (None, Some(b)) => format!("<= {b}"),
        (None, None) => "any".to_string(),
    }
}

pub fn in_range(v: f64, min: Option<f64>, max: Option<f64>) -> bool {
    min.is_none_or(|m| v >= m - 1e-9) && max.is_none_or(|m| v <= m + 1e-9)
}

pub fn num(p: &Map<String, J>, k: &str) -> Option<f64> {
    p.get(k).and_then(J::as_f64)
}

pub fn text(p: &Map<String, J>, k: &str) -> Option<String> {
    p.get(k).and_then(J::as_str).map(str::to_string)
}

struct Count {
    c: CountO,
    grow: bool,
}

impl Oracle for Count {
    fn name(&self) -> String {
        format!("{} {}", if self.grow { "grow" } else { "count" }, self.c.spec)
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let name = self.name();
        let exp = range_text(self.c.min, self.c.max);
        let now = match cx.facts.count(&self.c.spec) {
            Ok(n) => n,
            Err(e) => return no_capability(&name, exp, &e),
        };
        let (v, shown) = if self.grow {
            let was = match cx.before.map(|b| b.count(&self.c.spec)) {
                Some(Ok(n)) => n,
                Some(Err(_)) | None => 0,
            };
            ((now - was) as f64, format!("{} (was {was}, now {now})", now - was))
        } else {
            (now as f64, now.to_string())
        };
        result(&name, exp, shown, in_range(v, self.c.min, self.c.max))
    }
}

struct Text(TextO);

impl Oracle for Text {
    fn name(&self) -> String {
        format!("text {}", self.0.name)
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let exp = format!("answer matches /{}/", self.0.re);
        match rx(&self.0.re) {
            Err(e) => no_capability(&self.name(), exp, &e),
            Ok(r) => {
                let found = r.find(cx.answer).map(|m| m.as_str().chars().take(80).collect::<String>());
                result(&self.name(), exp, found.clone().unwrap_or_else(|| "absent".into()), found.is_some())
            }
        }
    }
}

struct Claim(ClaimO);

impl Oracle for Claim {
    fn name(&self) -> String {
        format!("claim {}", self.0.name)
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let exp = format!("if the answer says /{}/ the model has {} >= {}", self.0.say, self.0.have, self.0.min);
        let said = match rx(&self.0.say) {
            Ok(r) => r.is_match(cx.answer),
            Err(e) => return no_capability(&self.name(), exp, &e),
        };
        if !said {
            return result(&self.name(), exp, "not claimed".into(), true);
        }
        match cx.facts.count(&self.0.have) {
            Ok(n) => result(&self.name(), exp, format!("claimed; model has {n}"), n as f64 >= self.0.min),
            Err(e) => no_capability(&self.name(), exp, &format!("claimed; {e}")),
        }
    }
}

struct Ifc(IfcO);

fn entities(step: &str, class: &str) -> i64 {
    let key = format!("={}(", class.to_uppercase());
    step.matches(key.as_str()).count() as i64
}

impl Oracle for Ifc {
    fn name(&self) -> String {
        "ifc classes".to_string()
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let exp = self.0.classes_min.iter().map(|(k, v)| format!("{k} >= {v}")).collect::<Vec<_>>().join(", ");
        let Some(step) = cx.ifc else {
            return no_capability(&self.name(), exp, "no IFC export available");
        };
        let mut bad = vec![];
        let mut seen = vec![];
        for (cls, min) in &self.0.classes_min {
            let n: i64 = cls.split('|').map(|c| entities(step, c.trim())).sum();
            seen.push(format!("{cls}={n}"));
            if n < *min {
                bad.push(cls.clone());
            }
        }
        result(&self.name(), exp, seen.join(", "), bad.is_empty())
    }
}

/// Blocking findings of every check rule over the whole model.
pub fn blocking_findings(m: &ddc_model::Model) -> Result<usize, String> {
    use ddc_model::checks::{CheckScope, Severity};
    let mut found = vec![];
    for r in crate::ai::plan::all_rules(&[]) {
        r.run(m, &CheckScope::All, &mut found).map_err(|e| format!("{}: {e}", r.id()))?;
    }
    Ok(found.iter().filter(|f| f.severity == Severity::Error).count())
}

struct Checks(ChecksO);

impl Oracle for Checks {
    fn name(&self) -> String {
        "new blocking findings".to_string()
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let exp = format!("<= {} over the base", self.0.new_max);
        match blocking_findings(&cx.ed.model) {
            Ok(now) => {
                let new = now as i64 - cx.errors_before as i64;
                result(&self.name(), exp, format!("{new} new ({now} now, {} before)", cx.errors_before), new <= self.0.new_max)
            }
            Err(e) => no_capability(&self.name(), exp, &e),
        }
    }
}

struct Broken(String, String);

impl Oracle for Broken {
    fn name(&self) -> String {
        self.0.clone()
    }

    fn check(&self, _: &EvalCx) -> OracleResult {
        no_capability(&self.0, "a known oracle".into(), &self.1)
    }
}

/// An unknown oracle kind is a failing oracle, so a typo in a case file cannot pass silently.
pub fn from_geom(g: &GeomO) -> Box<dyn Oracle> {
    super::geom_oracles::build(g)
        .or_else(|| super::mep_oracles::build(g))
        .or_else(|| super::query_oracles::build_geom(g))
        .or_else(|| super::fire_oracles::build(g))
        .or_else(|| super::shape_oracles::build(g))
        .or_else(|| super::design_oracles::build(g))
        .or_else(|| super::form_oracles::build(g))
        .or_else(|| super::material_oracles::build(g))
        .unwrap_or_else(|| Box::new(Broken(format!("geom {}", g.kind), format!("unknown oracle kind {}", g.kind))))
}

pub fn build_oracles(case: &Case) -> Vec<Box<dyn Oracle>> {
    let o = &case.oracle;
    let mut v: Vec<Box<dyn Oracle>> = vec![];
    v.extend(o.count.iter().map(|c| Box::new(Count { c: c.clone(), grow: false }) as Box<dyn Oracle>));
    v.extend(o.grow.iter().map(|c| Box::new(Count { c: c.clone(), grow: true }) as Box<dyn Oracle>));
    v.extend(o.query.iter().map(|c| Box::new(super::query_oracles::Query(c.clone())) as Box<dyn Oracle>));
    v.extend(o.qty.iter().map(|c| Box::new(super::query_oracles::Qty(c.clone())) as Box<dyn Oracle>));
    v.extend(o.receipt.iter().map(|c| Box::new(super::query_oracles::Receipts(c.clone())) as Box<dyn Oracle>));
    v.extend(o.geom.iter().map(from_geom));
    v.extend(o.text.iter().map(|t| Box::new(Text(t.clone())) as Box<dyn Oracle>));
    v.extend(o.claim.iter().map(|c| Box::new(Claim(c.clone())) as Box<dyn Oracle>));
    if let Some(i) = &o.ifc {
        v.push(Box::new(Ifc(i.clone())));
    }
    if let Some(c) = &o.checks {
        v.push(Box::new(Checks(c.clone())));
    }
    v
}

/// Runs every oracle of the case against the model.
pub fn evaluate(case: &Case, cx: &EvalCx) -> Vec<OracleResult> {
    build_oracles(case).iter().map(|o| o.check(cx)).collect()
}
