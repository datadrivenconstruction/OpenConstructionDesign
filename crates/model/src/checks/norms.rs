//! Norm checks: rules as data files, each with its source document and clause. The model is
//! judged by code only: the same model and pack give the same findings in the same order. AI
//! may explain a finding or draft a fix; it never decides that a rule is broken.
//!
//! The pack format is JSON: `{"rules": [{"id", "title": {"en", "ru"}, "source": {"doc",
//! "clause"}, "example", "severity", "scope", "check"}]}`; `scope` and `check` are the same
//! shapes as company rules (`param_present`, `param_range`, `param_in`, `count`, `clearance`,
//! `unique`, `param_value`). Packs shipped here are examples to verify by an engineer.

use serde_json::{json, Value as J};

use super::rule_def::run_rules;
use super::{finalize, rule_from, CheckScope, Finding, RuleDef, Severity};
use crate::{ElKey, Error, Model};
use crate::{ParamSpec, ParamType};

/// Id of the country pack whose engine rules the norms panel shows.
pub const STARTER_PACK_ID: &str = "ru";

#[derive(Clone, Debug, PartialEq)]
pub struct NormRule {
    /// `def.title` is the English title, `def.source_text` the document and clause.
    pub def: RuleDef,
    pub title_ru: String,
    pub doc: String,
    pub clause: String,
    /// Shipped as an example: an engineer verifies it before relying on it.
    pub example: bool,
}

impl NormRule {
    pub fn id(&self) -> &str {
        &self.def.id
    }
    pub fn title(&self, ru: bool) -> &str {
        if ru && !self.title_ru.is_empty() {
            &self.title_ru
        } else {
            &self.def.title
        }
    }
    /// `SP 1.13130.2020 p.4.2.5`.
    pub fn citation(&self) -> String {
        format!("{} {}", self.doc, self.clause).trim().to_string()
    }
}

fn bad<T>(what: &str) -> Result<T, Error> {
    Err(Error::Invalid(format!("norm pack: {what}")))
}

fn text(v: &J, k: &str) -> Result<String, Error> {
    match v[k].as_str() {
        Some(s) if !s.trim().is_empty() => Ok(s.to_string()),
        _ => bad(k),
    }
}

/// Reads a pack. The first bad rule refuses the whole pack; ids must be unique in it.
pub fn load_norm_pack(json: &str) -> Result<Vec<NormRule>, Error> {
    let v: J = serde_json::from_str(json)?;
    let list = v.as_array().or_else(|| v["rules"].as_array());
    let Some(list) = list else {
        return bad("expected a list of rules");
    };
    let mut out: Vec<NormRule> = Vec::with_capacity(list.len());
    for r in list {
        let title_en = text(&r["title"], "en")?;
        let title_ru = r["title"]["ru"].as_str().unwrap_or("").to_string();
        let doc = text(&r["source"], "doc")?;
        let clause = text(&r["source"], "clause")?;
        let mut flat = r.clone();
        flat["title"] = json!(title_en);
        flat["source_text"] = json!(format!("{doc} {clause}"));
        let def = rule_from(&flat)?;
        if out.iter().any(|o| o.def.id == def.id) {
            return bad("duplicate id");
        }
        out.push(NormRule {
            def,
            title_ru,
            doc,
            clause,
            example: r["example"].as_bool().unwrap_or(false),
        });
    }
    Ok(out)
}

/// The shipped pack; a damaged pack is an error to show, never an empty set.
pub fn starter_pack() -> Result<Vec<NormRule>, Error> {
    let pack = super::norm_pack::PackSet::embedded().resolve(STARTER_PACK_ID)?;
    Ok(pack.legacy_rules())
}

/// One broken rule on one element (or on the whole scope when `elements` is empty).
#[derive(Clone, Debug, PartialEq)]
pub struct NormFinding {
    pub rule_id: String,
    pub severity: Severity,
    pub elements: Vec<ElKey>,
    pub at: Option<[f64; 3]>,
    /// What the model has and what the rule requires, in `unit` (`m`, `m2`, `m3`, `count`, or
    /// empty for a plain number).
    pub measured: Option<f64>,
    pub required: Option<f64>,
    pub unit: &'static str,
    /// Index of the rule in the pack that was run.
    pub rule_index: usize,
    /// The sub-clause when the rule cites a finer one per case (`p.4.4.1 b)`).
    pub clause: Option<String>,
}

impl NormFinding {
    /// The clause to cite: the case's own, else the rule's.
    pub fn clause_of<'a>(&'a self, rule: &'a NormRule) -> &'a str {
        self.clause.as_deref().unwrap_or(&rule.clause)
    }
}

fn from_finding(f: &Finding, rule_index: usize, rule: &NormRule) -> NormFinding {
    // a note ("not set", "not defined") has nothing measured
    let note = ["ai.check.rule.unset", "ai.check.rule.undefined"].contains(&f.msg_key);
    let numbers = f.measure.filter(|_| !note);
    NormFinding {
        rule_id: rule.def.id.clone(),
        severity: f.severity,
        elements: f.elements.clone(),
        at: f.at,
        measured: numbers.map(|m| m.value),
        required: numbers.map(|m| m.limit),
        unit: numbers.map_or("", |m| m.unit),
        rule_index,
        clause: f.args.iter().find(|a| a.0 == "clause").map(|a| a.1.clone()),
    }
}

/// Runs `rules` on `m`: errors first, then pack order, then element keys, so a repeat run on
/// the same model is identical.
pub fn run_norms(
    m: &Model,
    rules: &[NormRule],
    scope: &CheckScope,
) -> Result<Vec<NormFinding>, Error> {
    let mut out = vec![];
    for (i, r) in rules.iter().enumerate() {
        let mut fs: Vec<Finding> = vec![];
        run_rules(m, std::slice::from_ref(&r.def), scope, &mut fs)?;
        finalize(m, &mut fs);
        out.extend(fs.iter().map(|f| from_finding(f, i, r)));
    }
    out.sort_by(|a, b| {
        (a.severity, a.rule_index, &a.elements).cmp(&(b.severity, b.rule_index, &b.elements))
    });
    Ok(out)
}

fn priority(s: Severity) -> &'static str {
    match s {
        Severity::Error => "High",
        Severity::Warning => "Normal",
        Severity::Info => "Low",
    }
}

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

/// Topics in the BCF shape (title, priority, components) with the rule, clause and numbers.
/// The topic id is derived from the rule and elements, so an export is repeatable.
pub fn findings_json(rules: &[NormRule], findings: &[NormFinding]) -> String {
    let topics: Vec<J> = findings
        .iter()
        .filter_map(|f| {
            let r = rules.get(f.rule_index)?;
            let els: Vec<String> = f.elements.iter().map(|(t, i)| format!("{t}:{i}")).collect();
            Some(json!({
                "id": format!("{}|{}", f.rule_id, els.join(",")),
                "title": r.def.title,
                "title_ru": r.title_ru,
                "priority": priority(f.severity),
                "severity": severity_name(f.severity),
                "rule": {
                    "id": r.def.id,
                    "document": r.doc,
                    "clause": f.clause_of(r),
                    "example": r.example,
                },
                "components": f.elements.iter()
                    .map(|(t, i)| json!({"type": t, "id": i}))
                    .collect::<Vec<_>>(),
                "measured": f.measured,
                "required": f.required,
                "unit": f.unit,
                "point": f.at,
            }))
        })
        .collect();
    let doc = json!({"format": "ddc-norm-findings", "version": 1, "topics": topics});
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

/// Project parameters the norm rules read: (name, type, category). The names are the contract
/// between the editor's property panels and the rule packs.
pub const NORM_PARAMS: [(&str, ParamType, &str); 5] = [
    ("Purpose", ParamType::Text, "room"),
    ("Occupants", ParamType::Int, "room"),
    ("EvacuationExit", ParamType::YesNo, "door"),
    ("Accessible", ParamType::YesNo, "door"),
    ("Headroom", ParamType::Length, "stair"),
];

/// Values of `Purpose` the starter pack understands.
pub const PURPOSES: [&str; 8] = [
    "residential",
    "kitchen",
    "corridor",
    "office",
    "public",
    "service",
    "technical",
    "workplace_passage",
];

impl Model {
    /// Names of the norm parameters the project does not have yet.
    pub fn norm_params_missing(&self) -> Result<Vec<&'static str>, Error> {
        let mut out = vec![];
        for (name, _, _) in NORM_PARAMS {
            if self.param_by_name(name)?.is_none() {
                out.push(name);
            }
        }
        Ok(out)
    }

    /// Adds the missing norm parameters in one undoable step; returns how many were added.
    pub fn ensure_norm_params(&mut self) -> Result<usize, Error> {
        let specs: Vec<ParamSpec> = self
            .norm_params_missing()?
            .into_iter()
            .filter_map(|n| NORM_PARAMS.iter().find(|p| p.0 == n))
            .map(|(name, ptype, cat)| ParamSpec {
                name: (*name).to_string(),
                ptype: *ptype,
                per_type: false,
                unit: String::new(),
                group: "Norms".to_string(),
                guid: None,
                categories: vec![(*cat).to_string()],
            })
            .collect();
        if specs.is_empty() {
            return Ok(0);
        }
        Ok(self.add_params(&specs)?.len())
    }
}
