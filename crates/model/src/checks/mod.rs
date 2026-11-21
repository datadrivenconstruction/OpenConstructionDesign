// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Model check engine (docs/design/AI_ASSISTANT.md §9), not tied to a country. Whether a rule is
//! violated is decided deterministically by code; the AI explains and picks fixes. Rules live in `rule_*.rs`;
//! company rules and packs are `RuleDef` data in the project settings (`ai_policy`).

use serde_json::{json, Value as J};

use crate::{ClashFix, Edit, ElKey, Error, Model};

mod fix;
mod fix_op;
mod rule_def;
mod rule_dup;
mod rule_model;
mod rule_opening;
mod rule_param;
mod rule_req;
#[cfg(test)]
mod rule_req_tests;
mod rule_room;
#[cfg(test)]
mod rule_tests;
mod norm_project;
pub mod norm_kinds;
pub mod norm_pack;
#[cfg(test)]
mod norm_pack_tests;
mod rule_cases;
mod rule_value_src;
mod util;
pub mod realism;
mod realism_input;
#[cfg(test)]
mod realism_tests;
pub mod norms;
#[cfg(test)]
mod norms_tests;

pub use norm_project::{NormLocale, NormProject, FIRE_CLASSES};
pub use fix_op::{FixDryRun, FixEnvelope, FixOp, FixParamType, FixVal};
pub(crate) use rule_param::category_rows;
pub use rule_req::{
    canon_cat, parse_requirement, requirement_findings, requirements_report, CatReport, ReqError,
    ReqField, ReqHit, ReqLine, ReqReport, ReqText, Requirement, ValueRule, REQ_CATS, REQ_PACK,
};

const KEY_RULES: &str = "checks.rules";
const KEY_PACK: &str = "checks.pack.";

/// Variant order is the order in the check result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CheckScope {
    All,
    Level(i64),
    Elements(Vec<ElKey>),
}

#[derive(Clone, Debug)]
pub enum Fix {
    Edits(Vec<Edit>),
    Clash(ClashFix),
    Delete(Vec<ElKey>),
    None,
}

/// Measured value against the limit it breaks, in one unit (`m`, `m2`, `m3`, `mm`, `count`).
/// A yes/no condition is `count`: 1 found against 0 allowed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measure {
    pub value: f64,
    pub limit: f64,
    pub unit: &'static str,
}

impl Measure {
    pub fn new(value: f64, limit: f64, unit: &'static str) -> Self {
        Measure { value, limit, unit }
    }
    /// A broken yes/no condition.
    pub fn flag() -> Self {
        Measure::new(1.0, 0.0, "count")
    }
    pub fn count(n: usize, limit: usize) -> Self {
        Measure::new(n as f64, limit as f64, "count")
    }
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub rule: &'static str,
    pub severity: Severity,
    pub elements: Vec<ElKey>,
    /// World point, m. Rules fill it when they know it; `run_all` fills the rest from the elements.
    pub at: Option<[f64; 3]>,
    /// Absent only on the summary findings that name no element.
    pub measure: Option<Measure>,
    pub msg_key: &'static str,
    pub args: Vec<(&'static str, String)>,
    pub fix: Fix,
}

impl Finding {
    /// What was found, without the finding number: a plan edit uses it to verify it is still the same finding.
    /// The fix as data the op layer can run; `None` when the finding has no automatic fix.
    pub fn envelope(&self) -> Option<FixEnvelope> {
        self.fix.envelope()
    }

    pub fn key(&self) -> String {
        match self.args.first() {
            Some(a) => format!("{}:{}", self.msg_key, a.1),
            None => self.msg_key.to_string(),
        }
    }
}

pub trait CheckRule: Send + Sync {
    fn id(&self) -> &'static str;
    /// Too slow to run at every plan step; runs only on request.
    fn heavy(&self) -> bool {
        false
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error>;
}

/// Rule registry.
pub fn rules() -> &'static [&'static dyn CheckRule] {
    &[
        &rule_model::Integrity,
        &rule_model::WallWarnings,
        &rule_model::LevelMismatch,
        &rule_opening::NoHost,
        &rule_opening::OutsideWall,
        &rule_opening::OpeningOverlap,
        &rule_room::RoomUnplaced,
        &rule_room::RoomOpen,
        &rule_room::ZeroSize,
        &rule_dup::Duplicates,
        &rule_param::MissingParams,
        &rule_param::TypeMarks,
        &rule_model::Clashes,
        &realism::Realism,
        &rule_def::CompanyRules,
        &rule_value_src::AiUnverified,
    ]
}

/// Selected rules (all when `None`); errors first, and within a severity, rule order.
pub fn run_all(
    m: &Model,
    ids: Option<&[&str]>,
    scope: &CheckScope,
    limit: usize,
) -> Result<Vec<Finding>, Error> {
    run_with(rules(), m, ids, scope, limit)
}

fn run_with(
    rules: &[&dyn CheckRule],
    m: &Model,
    ids: Option<&[&str]>,
    scope: &CheckScope,
    limit: usize,
) -> Result<Vec<Finding>, Error> {
    let mut out = vec![];
    for r in rules {
        if ids.is_none_or(|l| l.contains(&r.id())) {
            r.run(m, scope, &mut out)?;
        }
    }
    finalize(m, &mut out);
    out.sort_by_key(|f| f.severity);
    out.truncate(limit);
    Ok(out)
}

/// Findings without a point take the first point their elements have.
pub fn finalize(m: &Model, out: &mut [Finding]) {
    for f in out.iter_mut().filter(|f| f.at.is_none()) {
        f.at = f.elements.iter().find_map(|k| util::point_of(m, &k.0, k.1));
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuleScope {
    pub cats: Vec<String>,
    pub levels: Option<Vec<i64>>,
    pub type_name: Option<String>,
    /// Only elements where this holds; always `Cond::Eq` (a stored value equals a text).
    pub when: Option<Cond>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Lt,
    Le,
    Gt,
    Ge,
}

impl CmpOp {
    pub fn symbol(self) -> &'static str {
        match self {
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        }
    }

    fn from_symbol(s: Option<&str>) -> Option<CmpOp> {
        [CmpOp::Lt, CmpOp::Le, CmpOp::Gt, CmpOp::Ge].into_iter().find(|o| Some(o.symbol()) == s)
    }

    pub fn holds(self, a: f64, b: f64) -> bool {
        match self {
            CmpOp::Lt => a < b,
            CmpOp::Le => a <= b,
            CmpOp::Gt => a > b,
            CmpOp::Ge => a >= b,
        }
    }
}

/// A condition on a value of the element; text compares trimmed and ignoring case.
#[derive(Clone, Debug, PartialEq)]
pub enum Cond {
    Eq(String, String),
    In(String, Vec<String>),
    NotIn(String, Vec<String>),
    /// A number compared with a bound: `Cmp("Occupants", CmpOp::Gt, 50.0)` is "more than 50".
    Cmp(String, CmpOp, f64),
}

impl Cond {
    pub fn param(&self) -> &str {
        match self {
            Cond::Eq(p, _)
            | Cond::In(p, _)
            | Cond::NotIn(p, _)
            | Cond::Cmp(p, ..) => p,
        }
    }
    /// None when the value is blank or not a number for a numeric condition.
    pub fn holds(&self, value: Option<&str>) -> Option<bool> {
        let v = value.map(str::trim).filter(|v| !v.is_empty())?;
        let low = v.to_lowercase();
        let listed = |l: &[String]| l.iter().any(|w| w.trim().to_lowercase() == low);
        // num-lint: rule value text of a check; either decimal mark is read
        let num = || v.replace(',', ".").parse::<f64>().ok();
        match self {
            Cond::Eq(_, want) => Some(low == want.trim().to_lowercase()),
            Cond::In(_, l) => Some(listed(l)),
            Cond::NotIn(_, l) => Some(!listed(l)),
            Cond::Cmp(_, op, x) => num().map(|n| op.holds(n, *x)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Case {
    pub conds: Vec<Cond>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// The sub-clause this case comes from (`p.4.4.1 b)`); replaces the rule's clause.
    pub clause: Option<String>,
    /// The norm leaves this case open: a note for the engineer, never a verdict.
    pub undefined: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuleCheck {
    ParamPresent {
        param: String,
    },
    ParamRange {
        param: String,
        min: Option<f64>,
        max: Option<f64>,
    },
    ParamIn {
        param: String,
        allowed: Vec<String>,
    },
    Count {
        min: Option<u32>,
        max: Option<u32>,
    },
    /// A non-blank value of `param` (trimmed) is held by one element of the whole model only;
    /// findings name the elements inside the checked area.
    Unique {
        param: String,
        case_sensitive: bool,
    },
    /// `a op b * factor` must hold for one element, both read like rule values.
    Compare {
        a: String,
        op: CmpOp,
        b: String,
        factor: f64,
    },
    /// The first case whose conditions hold sets the bounds of `value`. An element no case
    /// fits is skipped, but when a condition value is blank it is reported as not set.
    /// `value` is a rule value or `min_width` (smallest clear width of a room).
    Cases {
        value: String,
        /// Inputs that must be set (`project.fire_class`, `project.height`,
        /// `project.people_upper` read the building); a blank one is a note.
        require: Vec<String>,
        cases: Vec<Case>,
    },
    Clearance {
        other: Vec<String>,
        min_m: f64,
    },
    /// An information requirement: `rule` holds wherever a value is present; `required` also
    /// flags a blank. `phase` narrows to elements created in that phase (name or key).
    Value {
        param: String,
        required: bool,
        rule: ValueRule,
        phase: Option<String>,
    },
}

/// A rule as data: the parameters a person sees and saves.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleDef {
    pub id: String,
    pub title: String,
    pub source_text: Option<String>,
    pub severity: Severity,
    pub scope: RuleScope,
    pub check: RuleCheck,
}

fn bad<T>(what: &str) -> Result<T, Error> {
    Err(Error::Invalid(format!("rule definition: {what}")))
}

fn req_str(v: &J, k: &str) -> Result<String, Error> {
    match v[k].as_str() {
        Some(s) => Ok(s.to_string()),
        None => bad(k),
    }
}

fn opt_str(v: &J, k: &str) -> Result<Option<String>, Error> {
    match &v[k] {
        J::Null => Ok(None),
        J::String(s) => Ok(Some(s.clone())),
        _ => bad(k),
    }
}

fn opt_f64(v: &J, k: &str) -> Result<Option<f64>, Error> {
    match &v[k] {
        J::Null => Ok(None),
        x => x.as_f64().map_or_else(|| bad(k), |f| Ok(Some(f))),
    }
}

fn opt_u32(v: &J, k: &str) -> Result<Option<u32>, Error> {
    match &v[k] {
        J::Null => Ok(None),
        x => x
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map_or_else(|| bad(k), |n| Ok(Some(n))),
    }
}

fn str_list(v: &J, k: &str) -> Result<Vec<String>, Error> {
    let Some(a) = v[k].as_array() else {
        return bad(k);
    };
    a.iter()
        .map(|x| x.as_str().map_or_else(|| bad(k), |s| Ok(s.to_string())))
        .collect()
}

fn severity_text(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

fn check_json(c: &RuleCheck) -> J {
    match c {
        RuleCheck::ParamPresent { param } => json!({"kind": "param_present", "param": param}),
        RuleCheck::ParamRange { param, min, max } => {
            json!({"kind": "param_range", "param": param, "min": min, "max": max})
        }
        RuleCheck::ParamIn { param, allowed } => {
            json!({"kind": "param_in", "param": param, "allowed": allowed})
        }
        RuleCheck::Count { min, max } => json!({"kind": "count", "min": min, "max": max}),
        RuleCheck::Compare { a, op, b, factor } => {
            json!({"kind": "compare", "a": a, "op": op.symbol(), "b": b, "factor": factor})
        }
        RuleCheck::Cases {
            value,
            require,
            cases,
        } => json!({"kind": "cases", "value": value, "require": require,
            "cases": cases.iter().map(case_json).collect::<Vec<_>>()}),
        RuleCheck::Unique {
            param,
            case_sensitive,
        } => json!({"kind": "unique", "param": param, "case_sensitive": case_sensitive}),
        RuleCheck::Clearance { other, min_m } => {
            json!({"kind": "clearance", "other": other, "min_m": min_m})
        }
        RuleCheck::Value {
            param,
            required,
            rule,
            phase,
        } => json!({"kind": "param_value", "param": param, "required": required,
            "rule": rule.to_json(), "phase": phase}),
    }
}

fn check_from(v: &J) -> Result<RuleCheck, Error> {
    match v["kind"].as_str() {
        Some("param_present") => Ok(RuleCheck::ParamPresent {
            param: req_str(v, "param")?,
        }),
        Some("param_range") => Ok(RuleCheck::ParamRange {
            param: req_str(v, "param")?,
            min: opt_f64(v, "min")?,
            max: opt_f64(v, "max")?,
        }),
        Some("param_in") => Ok(RuleCheck::ParamIn {
            param: req_str(v, "param")?,
            allowed: str_list(v, "allowed")?,
        }),
        Some("count") => Ok(RuleCheck::Count {
            min: opt_u32(v, "min")?,
            max: opt_u32(v, "max")?,
        }),
        Some("compare") => Ok(RuleCheck::Compare {
            a: req_str(v, "a")?,
            op: match v["op"].as_str() {
                o => match CmpOp::from_symbol(o) {
                    Some(op) => op,
                    None => return bad("op"),
                },
            },
            b: req_str(v, "b")?,
            factor: match v["factor"].as_f64() {
                Some(f) if f.is_finite() && f > 0.0 => f,
                _ => return bad("factor"),
            },
        }),
        Some("cases") => {
            let Some(list) = v["cases"].as_array().filter(|l| !l.is_empty()) else {
                return bad("cases");
            };
            let require = match &v["require"] {
                J::Null => vec![],
                _ => str_list(v, "require")?,
            };
            Ok(RuleCheck::Cases {
                value: req_str(v, "value")?,
                require,
                cases: list.iter().map(case_from).collect::<Result<_, _>>()?,
            })
        }
        Some("unique") => Ok(RuleCheck::Unique {
            param: req_str(v, "param")?,
            case_sensitive: match &v["case_sensitive"] {
                J::Null => true,
                x => x.as_bool().map_or_else(|| bad("case_sensitive"), Ok)?,
            },
        }),
        Some("clearance") => match v["min_m"].as_f64() {
            Some(min_m) if min_m.is_finite() && min_m >= 0.0 => Ok(RuleCheck::Clearance {
                other: str_list(v, "other")?,
                min_m,
            }),
            _ => bad("min_m"),
        },
        Some("param_value") => Ok(RuleCheck::Value {
            param: req_str(v, "param")?,
            required: v["required"].as_bool().map_or_else(|| bad("required"), Ok)?,
            rule: ValueRule::from_json(&v["rule"])?,
            phase: opt_str(v, "phase")?,
        }),
        _ => bad("check kind"),
    }
}

fn cond_json(k: &Cond) -> J {
    match k {
        Cond::Eq(p, x) => json!({"param": p, "eq": x}),
        Cond::In(p, x) => json!({"param": p, "in": x}),
        Cond::NotIn(p, x) => json!({"param": p, "not_in": x}),
        Cond::Cmp(p, op, x) => json!({"param": p, "op": op.symbol(), "value": x}),
    }
}

fn cond_from(k: &J) -> Result<Cond, Error> {
    let p = req_str(k, "param")?;
    if let Some(x) = k["eq"].as_str() {
        Ok(Cond::Eq(p, x.to_string()))
    } else if k["in"].is_array() {
        Ok(Cond::In(p, str_list(k, "in")?))
    } else if k["not_in"].is_array() {
        Ok(Cond::NotIn(p, str_list(k, "not_in")?))
    } else if let (Some(op), Some(x)) = (CmpOp::from_symbol(k["op"].as_str()), k["value"].as_f64()) {
        Ok(Cond::Cmp(p, op, x))
    } else {
        bad("condition")
    }
}

fn case_json(c: &Case) -> J {
    let conds: Vec<J> = c.conds.iter().map(cond_json).collect();
    json!({"if": conds, "min": c.min, "max": c.max, "clause": c.clause,
        "undefined": c.undefined})
}

fn case_from(v: &J) -> Result<Case, Error> {
    let Some(list) = v["if"].as_array() else {
        return bad("if");
    };
    let conds = list.iter().map(cond_from).collect::<Result<Vec<_>, Error>>()?;
    let (min, max) = (opt_f64(v, "min")?, opt_f64(v, "max")?);
    let undefined = v["undefined"].as_bool().unwrap_or(false);
    if min.is_none() && max.is_none() && !undefined {
        return bad("case without a bound");
    }
    Ok(Case {
        conds,
        min,
        max,
        clause: opt_str(v, "clause")?,
        undefined,
    })
}

fn when_from(sc: &J) -> Result<Option<Cond>, Error> {
    match &sc["when"] {
        J::Null => Ok(None),
        w => match cond_from(w)? {
            c @ Cond::Eq(..) => Ok(Some(c)),
            _ => bad("when"),
        },
    }
}

fn rule_json(r: &RuleDef) -> J {
    json!({
        "id": r.id,
        "title": r.title,
        "source_text": r.source_text,
        "severity": severity_text(r.severity),
        "scope": {"cats": r.scope.cats, "levels": r.scope.levels, "type_name": r.scope.type_name,
            "when": r.scope.when.as_ref().map(cond_json)},
        "check": check_json(&r.check),
    })
}

fn rule_from(v: &J) -> Result<RuleDef, Error> {
    let id = req_str(v, "id")?;
    if id.is_empty() {
        return bad("empty id");
    }
    let severity = match v["severity"].as_str() {
        Some("error") => Severity::Error,
        Some("warning") => Severity::Warning,
        Some("info") => Severity::Info,
        _ => return bad("severity"),
    };
    let sc = &v["scope"];
    let levels = match &sc["levels"] {
        J::Null => None,
        J::Array(a) => {
            let ids: Option<Vec<i64>> = a.iter().map(J::as_i64).collect();
            match ids {
                Some(l) => Some(l),
                None => return bad("levels"),
            }
        }
        _ => return bad("levels"),
    };
    Ok(RuleDef {
        id,
        title: req_str(v, "title")?,
        source_text: opt_str(v, "source_text")?,
        severity,
        scope: RuleScope {
            cats: str_list(sc, "cats")?,
            levels,
            type_name: opt_str(sc, "type_name")?,
            when: when_from(sc)?,
        },
        check: check_from(&v["check"])?,
    })
}

/// A rule pack: an array of definitions or `{"rules": [...]}`. The first error rejects the whole pack.
pub fn load_pack(json: &str) -> Result<Vec<RuleDef>, Error> {
    let v: J = serde_json::from_str(json)?;
    let list = v.as_array().or_else(|| v["rules"].as_array());
    let Some(list) = list else {
        return bad("expected a list of rules");
    };
    list.iter().map(rule_from).collect()
}

impl Model {
    /// Project rules, then pack rules; on an id clash the project rule wins.
    pub fn project_rules(&self) -> Result<Vec<RuleDef>, Error> {
        let mut out = match self.ai_policy(KEY_RULES)? {
            Some(s) => load_pack(&s)?,
            None => vec![],
        };
        let packs: Vec<String> = {
            let mut st = self
                .db
                .prepare("SELECT value FROM ai_policy WHERE substr(key,1,?1)=?2 ORDER BY key")?;
            let v = st
                .query_map(rusqlite::params![KEY_PACK.len() as i64, KEY_PACK], |r| {
                    r.get(0)
                })?
                .collect::<Result<Vec<String>, _>>()?;
            v
        };
        for p in packs {
            for r in load_pack(&p)? {
                if !out.iter().any(|o| o.id == r.id) {
                    out.push(r);
                }
            }
        }
        Ok(out)
    }

    /// Saves a project rule; the same id replaces the earlier one.
    pub fn save_rule(&self, r: &RuleDef) -> Result<(), Error> {
        if r.id.is_empty() {
            return bad("empty id");
        }
        let mut list = match self.ai_policy(KEY_RULES)? {
            Some(s) => load_pack(&s)?,
            None => vec![],
        };
        match list.iter_mut().find(|o| o.id == r.id) {
            Some(o) => *o = r.clone(),
            None => list.push(r.clone()),
        }
        let text = J::Array(list.iter().map(rule_json).collect()).to_string();
        self.set_ai_policy(KEY_RULES, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, check: RuleCheck) -> RuleDef {
        RuleDef {
            id: id.into(),
            title: "Doors EI30".into(),
            source_text: Some("all doors EI30".into()),
            severity: Severity::Warning,
            scope: RuleScope {
                cats: vec!["door".into()],
                levels: Some(vec![3]),
                type_name: None,
                when: None,
            },
            check,
        }
    }

    struct Fixed(&'static str, Severity);

    impl CheckRule for Fixed {
        fn id(&self) -> &'static str {
            self.0
        }
        fn run(&self, _: &Model, _: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
            out.push(Finding {
                rule: self.0,
                severity: self.1,
                elements: vec![("wall".into(), 1)],
                at: None,
                measure: None,
                msg_key: "ai.check.test",
                args: vec![],
                fix: Fix::None,
            });
            Ok(())
        }
    }

    #[test]
    fn registry_has_unique_ids_and_an_empty_model_is_clean() {
        let m = Model::open(":memory:").unwrap();
        let mut ids: Vec<&str> = rules().iter().map(|r| r.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), rules().len());
        assert!(run_all(&m, None, &CheckScope::All, 10).unwrap().is_empty());
    }

    #[test]
    fn run_filters_orders_by_severity_then_limits() {
        let m = Model::open(":memory:").unwrap();
        let (a, b, c) = (
            Fixed("a", Severity::Info),
            Fixed("b", Severity::Error),
            Fixed("c", Severity::Warning),
        );
        let list: [&dyn CheckRule; 3] = [&a, &b, &c];
        let all = run_with(&list, &m, None, &CheckScope::All, 10).unwrap();
        let ids: Vec<&str> = all.iter().map(|f| f.rule).collect();
        assert_eq!(ids, ["b", "c", "a"]);
        let top = run_with(&list, &m, None, &CheckScope::All, 1).unwrap();
        assert_eq!(top[0].rule, "b", "limit keeps the most severe");
        let want = ["a"];
        let only = run_with(&list, &m, Some(&want[..]), &CheckScope::Level(1), 10).unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].rule, "a");
    }

    #[test]
    fn rules_round_trip_every_check_kind() {
        let m = Model::open(":memory:").unwrap();
        let all = [
            rule("a", RuleCheck::ParamPresent { param: "EI".into() }),
            rule(
                "b",
                RuleCheck::ParamRange {
                    param: "w".into(),
                    min: Some(0.9),
                    max: None,
                },
            ),
            rule(
                "c",
                RuleCheck::ParamIn {
                    param: "EI".into(),
                    allowed: vec!["EI30".into(), "EI60".into()],
                },
            ),
            rule(
                "d",
                RuleCheck::Count {
                    min: None,
                    max: Some(4),
                },
            ),
            rule(
                "e",
                RuleCheck::Clearance {
                    other: vec!["column".into()],
                    min_m: 0.5,
                },
            ),
        ];
        for r in &all {
            m.save_rule(r).unwrap();
        }
        assert_eq!(m.project_rules().unwrap(), all);
        let mut again = all[0].clone();
        again.title = "new".into();
        m.save_rule(&again).unwrap();
        let got = m.project_rules().unwrap();
        assert_eq!((got.len(), got[0].title.as_str()), (all.len(), "new"));
    }

    #[test]
    fn packs_merge_and_project_rule_wins() {
        let m = Model::open(":memory:").unwrap();
        let own = rule("a", RuleCheck::ParamPresent { param: "EI".into() });
        m.save_rule(&own).unwrap();
        let pack = J::Array(vec![
            rule_json(&rule(
                "a",
                RuleCheck::Count {
                    min: None,
                    max: None,
                },
            )),
            rule_json(&rule(
                "z",
                RuleCheck::Count {
                    min: Some(1),
                    max: None,
                },
            )),
        ])
        .to_string();
        m.set_ai_policy("checks.pack.base", &pack).unwrap();
        let got = m.project_rules().unwrap();
        let ids: Vec<&str> = got.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["a", "z"]);
        assert_eq!(got[0], own);
        let wrapped = format!("{{\"rules\": {pack}}}");
        assert_eq!(load_pack(&wrapped).unwrap().len(), 2);
    }

    #[test]
    fn bad_definitions_are_rejected_whole() {
        for bad_json in [
            "{}",
            "[{\"id\":\"a\"}]",
            "not json",
            "[{\"id\":\"\",\"title\":\"t\",\"severity\":\"info\",\"scope\":{\"cats\":[]},\"check\":{\"kind\":\"count\"}}]",
            "[{\"id\":\"a\",\"title\":\"t\",\"severity\":\"nope\",\"scope\":{\"cats\":[]},\"check\":{\"kind\":\"count\"}}]",
            "[{\"id\":\"a\",\"title\":\"t\",\"severity\":\"info\",\"scope\":{\"cats\":[]},\"check\":{\"kind\":\"clearance\",\"other\":[],\"min_m\":-1}}]",
        ] {
            assert!(load_pack(bad_json).is_err(), "{bad_json}");
        }
        let ok = "[{\"id\":\"a\",\"title\":\"t\",\"severity\":\"info\",\"scope\":{\"cats\":[]},\"check\":{\"kind\":\"count\"}}]";
        assert_eq!(load_pack(ok).unwrap().len(), 1);
    }
}
