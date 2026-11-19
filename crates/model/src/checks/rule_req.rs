// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Information requirements: per category, a parameter that must be filled and hold a value of
//! a given form (non-empty, one of a list, a range, a pattern, a unit). Rows arrive as text from
//! a workbook or an assistant; stored, they are ordinary company rules in their own pack, so
//! `run_checks` and `company_rules` see them too. Passing means only that these requirements
//! found nothing: it is not proof of code compliance.

use std::collections::HashSet;

use regex::{Regex, RegexBuilder};
use serde_json::{json, Value as J};

use super::rule_def::{self, Cat, Source};
use super::util::{key, quoted, Sink, Within};
use super::{
    bad, load_pack, rule_json, CheckScope, Finding, Fix, Measure, RuleCheck, RuleDef, RuleScope,
    Severity,
};
use crate::{ElKey, Error, Model, PVal, ParamType};

/// Settings key of the requirements pack; a `checks.pack.` key, so the rule engine reads it.
pub const REQ_PACK: &str = "checks.pack.requirements";
const ID_PREFIX: &str = "req.";
/// Compiled size cap of one value pattern: a workbook cannot make a check slow or huge.
const PATTERN_LIMIT: usize = 1 << 20;
const EPS: f64 = 1e-9;

/// Category words a requirement may name, as the rule engine knows them.
pub const REQ_CATS: &[&str] = &[
    "door",
    "window",
    "opening",
    "wall",
    "curtain_wall",
    "floor",
    "roof",
    "ceiling",
    "room",
    "column",
    "beam",
    "stair",
    "railing",
    "component",
    "mep_run",
    "shaft",
];

/// The rule word for a category in a cell: singular or plural, any case.
pub fn canon_cat(word: &str) -> Option<&'static str> {
    let w = word.trim().to_lowercase().replace(['-', ' '], "_");
    let w = match w.as_str() {
        "doors" => "door",
        "windows" => "window",
        "openings" => "opening",
        "walls" => "wall",
        "curtain" | "curtain_walls" | "curtainwall" => "curtain_wall",
        "floors" | "slab" | "slabs" => "floor",
        "roofs" => "roof",
        "ceilings" => "ceiling",
        "rooms" | "space" | "spaces" => "room",
        "col" | "columns" => "column",
        "beams" => "beam",
        "stairs" => "stair",
        "railings" => "railing",
        "comp" | "components" => "component",
        "mep" | "mep_runs" => "mep_run",
        "shafts" => "shaft",
        other => other,
    };
    REQ_CATS.iter().copied().find(|c| *c == w)
}

/// What a value must look like where it is present.
#[derive(Clone, Debug, PartialEq)]
pub enum ValueRule {
    NonEmpty,
    OneOf(Vec<String>),
    /// Bounds in base units: metres, square metres, cubic metres, or plain numbers.
    Range {
        min: Option<f64>,
        max: Option<f64>,
    },
    /// The whole value matches the pattern.
    Regex(String),
    /// The value is a number in this unit: a typed parameter of that quantity, or text with it.
    Unit(String),
}

impl ValueRule {
    pub fn kind(&self) -> &'static str {
        match self {
            ValueRule::NonEmpty => "non_empty",
            ValueRule::OneOf(_) => "one_of",
            ValueRule::Range { .. } => "range",
            ValueRule::Regex(_) => "regex",
            ValueRule::Unit(_) => "unit",
        }
    }

    /// The allowed values as one line: `EI30; EI60`, `0.9..2.4`, the pattern or the unit.
    pub fn text(&self) -> String {
        match self {
            ValueRule::NonEmpty => String::new(),
            ValueRule::OneOf(v) => v.join("; "),
            ValueRule::Range { min, max } => {
                let f = |x: &Option<f64>| x.map(|v| v.to_string()).unwrap_or_default();
                format!("{}..{}", f(min), f(max))
            }
            ValueRule::Regex(p) => p.clone(),
            ValueRule::Unit(u) => u.clone(),
        }
    }

    pub(super) fn to_json(&self) -> J {
        match self {
            ValueRule::NonEmpty => json!({"kind": "non_empty"}),
            ValueRule::OneOf(v) => json!({"kind": "one_of", "values": v}),
            ValueRule::Range { min, max } => json!({"kind": "range", "min": min, "max": max}),
            ValueRule::Regex(p) => json!({"kind": "regex", "pattern": p}),
            ValueRule::Unit(u) => json!({"kind": "unit", "unit": u}),
        }
    }

    /// A stored rule is checked as hard as a typed one: a bad pattern never reaches a run.
    pub(super) fn from_json(v: &J) -> Result<ValueRule, Error> {
        let num = |k: &str| match &v[k] {
            J::Null => Ok(None),
            x => x.as_f64().map_or_else(|| bad(k), |f| Ok(Some(f))),
        };
        match v["kind"].as_str() {
            Some("non_empty") => Ok(ValueRule::NonEmpty),
            Some("one_of") => {
                let list = v["values"].as_array().map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                });
                match list {
                    Some(l) if !l.is_empty() => Ok(ValueRule::OneOf(l)),
                    _ => bad("values"),
                }
            }
            Some("range") => {
                let (min, max) = (num("min")?, num("max")?);
                match (min, max) {
                    (None, None) => bad("range"),
                    (Some(a), Some(b)) if a > b => bad("range"),
                    _ => Ok(ValueRule::Range { min, max }),
                }
            }
            Some("regex") => {
                let p = v["pattern"].as_str().map_or_else(|| bad("pattern"), Ok)?;
                if let Err(e) = compile(p) {
                    return bad(&format!("pattern: {e}"));
                }
                Ok(ValueRule::Regex(p.to_string()))
            }
            Some("unit") => match v["unit"].as_str().map(str::trim) {
                Some(u) if !u.is_empty() => Ok(ValueRule::Unit(u.to_string())),
                _ => bad("unit"),
            },
            _ => bad("value rule kind"),
        }
    }
}

/// The whole value must match; the size cap keeps a hostile pattern cheap.
fn compile(p: &str) -> Result<Regex, String> {
    RegexBuilder::new(&format!("^(?:{p})$"))
        .size_limit(PATTERN_LIMIT)
        .build()
        .map_err(|e| e.to_string())
}

/// A column of a requirement row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReqField {
    Category,
    Level,
    Type,
    Phase,
    Parameter,
    Required,
    Rule,
    Value,
    Unit,
}

impl ReqField {
    pub const ALL: [ReqField; 9] = [
        ReqField::Category,
        ReqField::Level,
        ReqField::Type,
        ReqField::Phase,
        ReqField::Parameter,
        ReqField::Required,
        ReqField::Rule,
        ReqField::Value,
        ReqField::Unit,
    ];

    /// Machine name: the template header of an English book and the i18n key part.
    pub fn key(self) -> &'static str {
        match self {
            ReqField::Category => "category",
            ReqField::Level => "level",
            ReqField::Type => "type",
            ReqField::Phase => "phase",
            ReqField::Parameter => "parameter",
            ReqField::Required => "required",
            ReqField::Rule => "rule",
            ReqField::Value => "value",
            ReqField::Unit => "unit",
        }
    }
}

/// Why a row is refused: the column, a message key (`req.err.*`) and the text it is about.
#[derive(Clone, Debug, PartialEq)]
pub struct ReqError {
    pub field: ReqField,
    pub code: &'static str,
    pub arg: String,
}

fn err<T>(field: ReqField, code: &'static str, arg: &str) -> Result<T, ReqError> {
    Err(ReqError {
        field,
        code,
        arg: arg.to_string(),
    })
}

/// One row as text, cells trimmed or not; localized words are mapped by the caller.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReqText {
    pub cat: String,
    pub level: String,
    pub type_name: String,
    pub phase: String,
    pub param: String,
    pub required: String,
    pub kind: String,
    pub value: String,
    pub unit: String,
}

/// A requirement by names, before it meets a model.
#[derive(Clone, Debug, PartialEq)]
pub struct Requirement {
    pub cat: &'static str,
    pub level: Option<String>,
    pub type_name: Option<String>,
    pub phase: Option<String>,
    pub param: String,
    pub required: bool,
    pub rule: ValueRule,
}

fn opt(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn yes_no(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        // a blank cell keeps the default of an information requirement: it must be filled
        "" | "yes" | "y" | "true" | "1" | "required" | "x" => Some(true),
        "no" | "n" | "false" | "0" | "optional" | "-" => Some(false),
        _ => None,
    }
}

fn kind_of(s: &str) -> Option<&'static str> {
    let k = s.trim().to_lowercase().replace(['-', ' '], "_");
    Some(match k.as_str() {
        "non_empty" | "nonempty" | "not_empty" | "filled" | "present" => "non_empty",
        "one_of" | "oneof" | "list" | "in" | "enum" => "one_of",
        "range" | "between" => "range",
        "regex" | "pattern" | "regexp" | "mask" => "regex",
        "unit" | "units" => "unit",
        _ => return None,
    })
}

/// Unit word to its quantity (1 length, 2 area, 3 volume) and factor to the base unit.
fn unit_of(u: &str) -> Option<(u8, f64)> {
    Some(match norm_unit(u).as_str() {
        "mm" => (1, 0.001),
        "cm" => (1, 0.01),
        "m" => (1, 1.0),
        "mm2" => (2, 1e-6),
        "cm2" => (2, 1e-4),
        "m2" | "sqm" => (2, 1.0),
        "mm3" => (3, 1e-9),
        "cm3" => (3, 1e-6),
        "m3" => (3, 1.0),
        _ => return None,
    })
}

fn norm_unit(u: &str) -> String {
    u.trim()
        .to_lowercase()
        .replace('\u{b2}', "2")
        .replace('\u{b3}', "3")
        .replace(['.', ' ', '^'], "")
        // Cyrillic em and es: a unit typed on a Russian keyboard is the same unit
        .replace('\u{43c}', "m")
        .replace('\u{441}', "c")
}

/// A leading number (point or comma decimal) and the text after it.
fn num_unit(s: &str) -> Option<(f64, String)> {
    let t = s.trim();
    let in_number = |i: usize, c: char| {
        c.is_ascii_digit() || c == '.' || c == ',' || (i == 0 && (c == '+' || c == '-'))
    };
    let end = t
        .char_indices()
        .find(|(i, c)| !in_number(*i, *c))
        .map_or(t.len(), |(i, _)| i);
    let x = t[..end].replace(',', ".").parse::<f64>().ok()?;
    x.is_finite().then(|| (x, t[end..].trim().to_string()))
}

fn bound(s: &str) -> Option<f64> {
    let t = s.trim().replace(',', ".");
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok().filter(|x| x.is_finite())
}

/// `900..2400`, `900..`, `..2400`, `>= 900`, `<= 2400`; each bound times `factor`.
fn parse_range(v: &str, factor: f64) -> Result<ValueRule, ReqError> {
    let t = v.trim();
    let bad_range = || err(ReqField::Value, "req.err.range", t);
    let (min, max) = if let Some((a, b)) = t.split_once("..") {
        let (a, b) = (a.trim(), b.trim().trim_start_matches('.'));
        let pa = bound(a);
        let pb = bound(b);
        if (!a.is_empty() && pa.is_none()) || (!b.is_empty() && pb.is_none()) {
            return bad_range();
        }
        (pa, pb)
    } else if let Some(r) = t.strip_prefix(">=").or_else(|| t.strip_prefix('>')) {
        match bound(r) {
            Some(x) => (Some(x), None),
            None => return bad_range(),
        }
    } else if let Some(r) = t.strip_prefix("<=").or_else(|| t.strip_prefix('<')) {
        match bound(r) {
            Some(x) => (None, Some(x)),
            None => return bad_range(),
        }
    } else {
        return bad_range();
    };
    if min.is_none() && max.is_none() {
        return bad_range();
    }
    let (min, max) = (min.map(|x| x * factor), max.map(|x| x * factor));
    if let (Some(a), Some(b)) = (min, max) {
        if a > b + EPS {
            return err(ReqField::Value, "req.err.range_order", t);
        }
    }
    Ok(ValueRule::Range { min, max })
}

fn split_values(v: &str) -> Vec<String> {
    let seps: &[char] = if v.contains([';', '|', '\n']) {
        &[';', '|', '\n']
    } else {
        &[',']
    };
    let mut out: Vec<String> = vec![];
    for x in v.split(seps).map(str::trim).filter(|x| !x.is_empty()) {
        if !out.iter().any(|o| o == x) {
            out.push(x.to_string());
        }
    }
    out
}

/// One row to a requirement, without a model: the first bad cell is the answer.
pub fn parse_requirement(t: &ReqText) -> Result<Requirement, ReqError> {
    let Some(cat) = canon_cat(&t.cat) else {
        return err(ReqField::Category, "req.err.category", t.cat.trim());
    };
    let param = t.param.trim();
    if param.is_empty() {
        return err(ReqField::Parameter, "req.err.param", "");
    }
    let Some(required) = yes_no(&t.required) else {
        return err(ReqField::Required, "req.err.required", t.required.trim());
    };
    let type_name = opt(&t.type_name);
    if type_name.is_some() && rule_def::cat_of(cat).is_some_and(|c| c.of_type.is_none()) {
        return err(ReqField::Type, "req.err.no_types", cat);
    }
    let (value, unit) = (t.value.trim(), t.unit.trim());
    let kind = if t.kind.trim().is_empty() && value.is_empty() && unit.is_empty() {
        "non_empty"
    } else {
        match kind_of(&t.kind) {
            Some(k) => k,
            None => return err(ReqField::Rule, "req.err.kind", t.kind.trim()),
        }
    };
    let rule = match kind {
        "non_empty" => ValueRule::NonEmpty,
        "one_of" => {
            let list = split_values(value);
            if list.is_empty() {
                return err(ReqField::Value, "req.err.values", "");
            }
            ValueRule::OneOf(list)
        }
        "range" => {
            let factor = if unit.is_empty() {
                1.0
            } else {
                match unit_of(unit) {
                    Some((_, f)) => f,
                    None => return err(ReqField::Unit, "req.err.range_unit", unit),
                }
            };
            parse_range(value, factor)?
        }
        "regex" => {
            if value.is_empty() {
                return err(ReqField::Value, "req.err.pattern_empty", "");
            }
            if let Err(e) = compile(value) {
                let first = e.lines().last().unwrap_or("").trim().to_string();
                return err(ReqField::Value, "req.err.pattern", &first);
            }
            ValueRule::Regex(value.to_string())
        }
        _ => {
            // the unit stands in its own column; a book that wrote it under value is read too
            let u = if unit.is_empty() { value } else { unit };
            if u.is_empty() {
                return err(ReqField::Unit, "req.err.unit", "");
            }
            ValueRule::Unit(u.to_string())
        }
    };
    Ok(Requirement {
        cat,
        level: opt(&t.level),
        type_name,
        phase: opt(&t.phase),
        param: param.to_string(),
        required,
        rule,
    })
}

impl Model {
    /// Requirement `n` of a source row `origin` (`Sheet!4`) as a rule of this model: the level
    /// and the phase must exist here; a type is matched by name when the check runs.
    pub fn requirement_rule(
        &self,
        n: usize,
        origin: &str,
        r: &Requirement,
    ) -> Result<RuleDef, ReqError> {
        let levels = match &r.level {
            None => None,
            Some(name) => {
                let want = name.trim().to_lowercase();
                let hit = self
                    .level_names()
                    .map_err(|e| ReqError {
                        field: ReqField::Level,
                        code: "req.err.level",
                        arg: e.to_string(),
                    })?
                    .into_iter()
                    .find(|(_, n)| n.trim().to_lowercase() == want);
                match hit {
                    Some((id, _)) => Some(vec![id]),
                    None => return err(ReqField::Level, "req.err.level", name),
                }
            }
        };
        if let Some(p) = &r.phase {
            let phases = crate::phases::load_phases(&self.db).unwrap_or_default();
            if crate::find_phase(&phases, p).is_none() {
                return err(ReqField::Phase, "req.err.phase", p);
            }
        }
        Ok(RuleDef {
            id: format!("{ID_PREFIX}{n}"),
            title: format!("{}: {}", r.cat, r.param),
            source_text: Some(origin.to_string()),
            severity: if r.required {
                Severity::Error
            } else {
                Severity::Warning
            },
            scope: RuleScope {
                cats: vec![r.cat.to_string()],
                levels,
                type_name: r.type_name.clone(),
                when: None,
            },
            check: RuleCheck::Value {
                param: r.param.clone(),
                required: r.required,
                rule: r.rule.clone(),
                phase: r.phase.clone(),
            },
        })
    }

    fn level_names(&self) -> Result<Vec<(i64, String)>, Error> {
        let mut st = self.db.prepare("SELECT id, name FROM level ORDER BY id")?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<(i64, String)>, _>>()?;
        Ok(v)
    }

    /// The stored requirements, in workbook order; none when no book was loaded.
    pub fn requirements(&self) -> Result<Vec<RuleDef>, Error> {
        match self.ai_policy(REQ_PACK)? {
            Some(s) => load_pack(&s),
            None => Ok(vec![]),
        }
    }

    /// Replaces the whole requirements pack; an empty list clears it. Every rule must be a
    /// value requirement: the pack never carries other company rules.
    pub fn save_requirements(&self, rules: &[RuleDef]) -> Result<(), Error> {
        if rules.iter().any(|r| !matches!(r.check, RuleCheck::Value { .. })) {
            return bad("requirements hold value rules only");
        }
        let mut ids = HashSet::new();
        if rules.iter().any(|r| r.id.is_empty() || !ids.insert(r.id.as_str())) {
            return bad("requirement ids must be unique and not empty");
        }
        let text = J::Array(rules.iter().map(rule_json).collect()).to_string();
        // the text must read back before it replaces what is stored
        load_pack(&text)?;
        self.set_ai_policy(REQ_PACK, &text)
    }
}

/// How a parameter's values are measured, for the range and unit rules.
enum Dim {
    Qty(u8),
    Number(String),
    Text,
}

fn dim_of(src: &Source, param: &str) -> Dim {
    match src {
        Source::Project(d) => match d.spec.ptype {
            ParamType::Length => Dim::Qty(1),
            ParamType::Area => Dim::Qty(2),
            ParamType::Volume => Dim::Qty(3),
            ParamType::Number | ParamType::Int => Dim::Number(d.spec.unit.clone()),
            _ => Dim::Text,
        },
        Source::Builtin(_) => match param.trim().to_lowercase().as_str() {
            "width" | "height" | "sill" | "along" | "base_offset" | "top_offset" | "offset"
            | "length" | "thickness" | "perimeter" => Dim::Qty(1),
            "area" => Dim::Qty(2),
            "volume" => Dim::Qty(3),
            _ => Dim::Text,
        },
        Source::Prop => Dim::Text,
    }
}

/// A present value against the rule; `re` is the compiled pattern of a regex rule.
fn holds(rule: &ValueRule, re: Option<&Regex>, dim: &Dim, v: &str) -> bool {
    let v = v.trim();
    match rule {
        ValueRule::NonEmpty => true,
        ValueRule::OneOf(list) => list.iter().any(|a| a.trim() == v),
        ValueRule::Regex(_) => re.is_some_and(|r| r.is_match(v)),
        ValueRule::Range { min, max } => {
            let Some((x, suffix)) = num_unit(v) else {
                return false;
            };
            // a typed value is already in base units; text may carry its own unit
            let x = match (dim, unit_of(&suffix)) {
                (Dim::Text, Some((_, f))) => x * f,
                (Dim::Text, None) => x,
                _ if suffix.is_empty() => x,
                _ => return false,
            };
            min.is_none_or(|a| x >= a - EPS) && max.is_none_or(|b| x <= b + EPS)
        }
        ValueRule::Unit(u) => {
            let Some((_, suffix)) = num_unit(v) else {
                return false;
            };
            match dim {
                Dim::Qty(q) => suffix.is_empty() && unit_of(u).is_some_and(|(d, _)| d == *q),
                Dim::Number(label) => {
                    suffix.is_empty() && norm_unit(label) == norm_unit(u) && !label.is_empty()
                }
                Dim::Text => !suffix.is_empty() && norm_unit(&suffix) == norm_unit(u),
            }
        }
    }
}

pub(super) struct Hit {
    pub id: i64,
    pub value: Option<String>,
    pub missing: bool,
    pub fix: Fix,
}

pub(super) struct Eval {
    pub checked: usize,
    pub hits: Vec<Hit>,
}

/// `None` when the requirement cannot run here (a phase this model does not have).
pub(super) fn eval(m: &Model, r: &RuleDef, cat: &Cat, w: &Within) -> Result<Option<Eval>, Error> {
    let RuleCheck::Value {
        param,
        required,
        rule,
        phase,
    } = &r.check
    else {
        return Ok(None);
    };
    let phase_sql = match phase {
        None => String::new(),
        Some(p) => {
            let phases = crate::phases::load_phases(&m.db)?;
            let Some(pid) = crate::find_phase(&phases, p) else {
                return Ok(None);
            };
            // an element without a phase row stands in the first phase
            let first = phases.first().map_or(0, |x| x.id);
            let created = format!(
                "(SELECT created FROM elem_phase WHERE kind={} AND ref=q.id)",
                quoted(cat.table)
            );
            format!(" AND COALESCE({created},{first})={pid}")
        }
    };
    let re = match rule {
        ValueRule::Regex(p) => {
            Some(compile(p).map_err(|e| Error::Invalid(format!("pattern: {e}")))?)
        }
        _ => None,
    };
    let src = rule_def::source(m, cat, param)?;
    let dim = dim_of(&src, param);
    let inner = rule_def::values_sql(cat, &r.scope, w, &src, param);
    let sql = format!("SELECT q.id, q.val FROM ({inner}) q WHERE 1{phase_sql} ORDER BY q.id");
    let mut out = Eval {
        checked: 0,
        hits: vec![],
    };
    for (id, value) in rule_def::violators(m, &sql)? {
        if !w.has(cat.table, id) {
            continue;
        }
        out.checked += 1;
        let empty = value.as_deref().is_none_or(|v| v.trim().is_empty());
        if empty && !*required {
            continue;
        }
        if !empty && holds(rule, re.as_ref(), &dim, value.as_deref().unwrap_or("")) {
            continue;
        }
        let fix = match (rule, &src) {
            (ValueRule::OneOf(list), Source::Project(d))
                if list.len() == 1 && d.spec.ptype == ParamType::Text =>
            {
                rule_def::value_edit(d, cat, m, id, PVal::Text(list[0].clone()))?
            }
            (ValueRule::Range { min, max }, _) if !empty => {
                rule_def::range_fix(m, cat, &src, param, id, value.as_deref(), *min, *max)?
            }
            _ => Fix::None,
        };
        out.hits.push(Hit {
            id,
            value: if empty { None } else { value },
            missing: empty,
            fix,
        });
    }
    Ok(Some(out))
}

/// The findings of one requirement, through the company rules engine.
pub(super) fn run(
    m: &Model,
    r: &RuleDef,
    cat: &Cat,
    w: &Within,
    s: &mut Sink<'_>,
) -> Result<(), Error> {
    let RuleCheck::Value { param, rule, .. } = &r.check else {
        return Ok(());
    };
    let Some(ev) = eval(m, r, cat, w)? else {
        s.add(
            Severity::Info,
            vec![],
            None,
            "ai.check.rule.unsupported",
            rule_def::args(r, "", "", String::new()),
            Fix::None,
            Measure::flag(),
        );
        return Ok(());
    };
    // past its cap the sink only counts, so the summary still says how many were left out
    for h in ev.hits {
        let msg = if h.missing {
            "ai.check.req.missing"
        } else {
            "ai.check.req.invalid"
        };
        let shown = h.value.unwrap_or_default();
        s.add(
            r.severity,
            vec![key(cat.table, h.id)],
            None,
            msg,
            rule_def::args(r, param, &shown, rule.text()),
            h.fix,
            Measure::flag(),
        );
    }
    Ok(())
}

/// Findings of the stored requirements alone, under the `company_rules` id that re-finds them
/// when a fix is compiled.
pub fn requirement_findings(m: &Model, scope: &CheckScope) -> Result<Vec<Finding>, Error> {
    let w = Within::new(scope);
    let mut out = vec![];
    let mut s = Sink::new(&mut out, "company_rules");
    for r in m.requirements()? {
        let Some(cat) = r.scope.cats.first().and_then(|c| rule_def::cat_of(c)) else {
            continue;
        };
        run(m, &r, &cat, &w, &mut s)?;
    }
    s.done();
    super::finalize(m, &mut out);
    Ok(out)
}

/// One element that fails a requirement.
#[derive(Clone, Debug, PartialEq)]
pub struct ReqHit {
    pub el: ElKey,
    pub value: Option<String>,
    pub missing: bool,
    /// The engine has an automatic fix (a single allowed value, a value clamped into range).
    pub fixable: bool,
}

/// One requirement in the report.
#[derive(Clone, Debug, PartialEq)]
pub struct ReqLine {
    pub rule_id: String,
    /// Where the row came from (`Requirements!4`).
    pub origin: Option<String>,
    pub param: String,
    pub required: bool,
    pub rule: ValueRule,
    pub scope: RuleScope,
    pub phase: Option<String>,
    /// False when the requirement could not run on this model (an unknown phase).
    pub ran: bool,
    /// Elements the requirement applied to (its filter met).
    pub checked: usize,
    pub hits: Vec<ReqHit>,
}

impl ReqLine {
    pub fn missing(&self) -> usize {
        self.hits.iter().filter(|h| h.missing).count()
    }
    pub fn invalid(&self) -> usize {
        self.hits.len() - self.missing()
    }
    pub fn elements(&self) -> Vec<ElKey> {
        self.hits.iter().map(|h| h.el.clone()).collect()
    }
}

/// One category: how many of its elements meet every requirement that applies to them.
#[derive(Clone, Debug, PartialEq)]
pub struct CatReport {
    pub cat: String,
    pub total: usize,
    pub compliant: usize,
    pub lines: Vec<ReqLine>,
}

impl CatReport {
    /// Every element failing some requirement of the category, each once, in id order.
    pub fn failing(&self) -> Vec<ElKey> {
        let mut seen = HashSet::new();
        let mut v: Vec<ElKey> = self
            .lines
            .iter()
            .flat_map(|l| l.hits.iter().map(|h| h.el.clone()))
            .filter(|k| seen.insert(k.clone()))
            .collect();
        v.sort();
        v
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReqReport {
    pub cats: Vec<CatReport>,
}

impl ReqReport {
    pub fn requirements(&self) -> usize {
        self.cats.iter().map(|c| c.lines.len()).sum()
    }
    pub fn total(&self) -> usize {
        self.cats.iter().map(|c| c.total).sum()
    }
    pub fn compliant(&self) -> usize {
        self.cats.iter().map(|c| c.compliant).sum()
    }
    /// Failing (element, requirement) pairs: (with an automatic fix, without one).
    pub fn fixable_split(&self) -> (usize, usize) {
        let hits = self.cats.iter().flat_map(|c| c.lines.iter().flat_map(|l| l.hits.iter()));
        hits.fold((0, 0), |(a, b), h| if h.fixable { (a + 1, b) } else { (a, b + 1) })
    }
}

fn cat_ids(m: &Model, cat: &Cat, w: &Within) -> Result<Vec<i64>, Error> {
    let all = RuleScope {
        cats: vec![],
        levels: None,
        type_name: None,
        when: None,
    };
    let (from, cond) = rule_def::selection(cat, &all, w);
    let ids = super::util::ids(m, &format!("SELECT t.id FROM {from} WHERE {cond}"))?;
    Ok(ids.into_iter().filter(|id| w.has(cat.table, *id)).collect())
}

/// The stored requirements run on `scope`, grouped by category in workbook order.
pub fn requirements_report(m: &Model, scope: &CheckScope) -> Result<ReqReport, Error> {
    let w = Within::new(scope);
    let mut rep = ReqReport::default();
    for r in m.requirements()? {
        let RuleCheck::Value {
            param,
            required,
            rule,
            phase,
        } = &r.check
        else {
            continue;
        };
        let Some(word) = r.scope.cats.first().and_then(|c| canon_cat(c)) else {
            continue;
        };
        let Some(cat) = rule_def::cat_of(word) else {
            continue;
        };
        let ev = eval(m, &r, &cat, &w)?;
        let line = ReqLine {
            rule_id: r.id.clone(),
            origin: r.source_text.clone(),
            param: param.clone(),
            required: *required,
            rule: rule.clone(),
            scope: r.scope.clone(),
            phase: phase.clone(),
            ran: ev.is_some(),
            checked: ev.as_ref().map_or(0, |e| e.checked),
            hits: ev
                .map(|e| {
                    e.hits
                        .into_iter()
                        .map(|h| ReqHit {
                            el: key(cat.table, h.id),
                            value: h.value,
                            missing: h.missing,
                            fixable: !matches!(h.fix, Fix::None),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        let i = match rep.cats.iter().position(|c| c.cat == word) {
            Some(i) => i,
            None => {
                let total = cat_ids(m, &cat, &w)?.len();
                rep.cats.push(CatReport {
                    cat: word.to_string(),
                    total,
                    compliant: total,
                    lines: vec![],
                });
                rep.cats.len() - 1
            }
        };
        rep.cats[i].lines.push(line);
    }
    for c in &mut rep.cats {
        c.compliant = c.total.saturating_sub(c.failing().len());
    }
    Ok(rep)
}
