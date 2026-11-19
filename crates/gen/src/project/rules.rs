// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The rule set: every number a generation decision depends on, with its range, unit and the
//! layer that set its value. Layers override in a fixed order: built-in, norm pack, preset,
//! project, brief/chat. The builder reads values through [`rule`] while [`scoped`] holds the
//! set of the project being built; outside a build the built-in values apply.

use serde_json::{json, Map, Value as J};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::OnceLock;

const DATA: &str = include_str!("../../data/rules.json");
pub const SCHEMA: &str = "gen_rules.v1";

/// One rule of the built-in set.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleDef {
    pub id: String,
    pub group: String,
    pub unit: String,
    pub min: f64,
    pub max: f64,
    /// `None`: the typology preset decides.
    pub default: Option<f64>,
    /// The older brief field that reads and writes this rule.
    pub alias: Option<String>,
    /// Whole numbers only: counts, flags and choices.
    pub int: bool,
    /// A yes/no rule, stored as 0 or 1.
    pub flag: bool,
    /// The options of a choice rule, stored as the option's index.
    pub options: Vec<String>,
    /// Tier of a goal rule (the `goal` group): what its miss costs against other goals.
    pub tier: Option<Tier>,
    /// Weight of a goal within its tier.
    pub weight: f64,
    /// Why the rule exists and what breaking it costs, for the rules window and a finding.
    pub why: String,
    /// Country pack dimension keys that set the rule at the norm layer, first found wins.
    pub norm: Vec<String>,
}

/// Strength of a goal. A higher tier dominates completely: no amount of weak misses outweighs
/// one strong miss. `Hard` misses mark a variant as failing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    Hard,
    Strong,
    Medium,
    Weak,
}

impl Tier {
    pub const ALL: [Tier; 4] = [Tier::Hard, Tier::Strong, Tier::Medium, Tier::Weak];

    pub fn name(self) -> &'static str {
        match self {
            Tier::Hard => "hard",
            Tier::Strong => "strong",
            Tier::Medium => "medium",
            Tier::Weak => "weak",
        }
    }

    pub fn parse(s: &str) -> Option<Tier> {
        Tier::ALL.into_iter().find(|t| t.name() == s)
    }

    /// Position in [`Tier::ALL`], strongest first.
    pub fn rank(self) -> usize {
        self as usize
    }
}

impl RuleDef {
    /// A brief or patch value as the stored number: a number, `true`/`false` for a flag, an
    /// option name for a choice; `None` when it does not fit the rule.
    pub fn parse_value(&self, v: &J) -> Option<f64> {
        let x = match v {
            J::Bool(b) if self.flag => f64::from(*b),
            J::String(s) if !self.options.is_empty() => self.options.iter().position(|o| o == s)? as f64,
            J::Number(n) => n.as_f64()?,
            _ => return None,
        };
        self.contains(x).then_some(x)
    }

    /// The stored number as the brief writes it.
    pub fn value_json(&self, x: f64) -> J {
        if self.flag {
            json!(x != 0.0)
        } else if let Some(o) = self.options.get(x as usize).filter(|_| !self.options.is_empty()) {
            json!(o)
        } else if self.int {
            json!(x as i64)
        } else {
            json!(x)
        }
    }

    /// What the rule takes, for messages and the tool schema.
    pub fn takes(&self) -> String {
        if self.flag {
            "true or false".into()
        } else if !self.options.is_empty() {
            format!("one of {}", self.options.join(", "))
        } else {
            let whole = if self.int { "a whole number " } else { "" };
            format!("{whole}in {} {}", self.range(), self.unit)
        }
    }

    pub fn title_key(&self) -> String {
        format!("gen.rule.{}.title", self.id)
    }

    pub fn desc_key(&self) -> String {
        format!("gen.rule.{}.desc", self.id)
    }

    /// `2.7..6`, as messages and the tool schema print it.
    pub fn range(&self) -> String {
        format!("{}..{}", self.min, self.max)
    }

    pub fn contains(&self, x: f64) -> bool {
        x.is_finite() && (self.min..=self.max).contains(&x) && (!self.int || x.fract() == 0.0)
    }
}

/// Where a value comes from, lowest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    Builtin,
    Norm,
    Preset,
    Project,
    Brief,
}

impl Layer {
    /// The layers a set may override, lowest first.
    pub const OVER: [Layer; 4] = [Layer::Norm, Layer::Preset, Layer::Project, Layer::Brief];

    pub fn name(self) -> &'static str {
        match self {
            Layer::Builtin => "builtin",
            Layer::Norm => "norm",
            Layer::Preset => "preset",
            Layer::Project => "project",
            Layer::Brief => "brief",
        }
    }

    pub fn parse(s: &str) -> Option<Layer> {
        [Layer::Builtin, Layer::Norm, Layer::Preset, Layer::Project, Layer::Brief]
            .into_iter()
            .find(|l| l.name() == s)
    }

    fn slot(self) -> Option<usize> {
        Layer::OVER.iter().position(|l| *l == self)
    }
}

/// What one layer says about a rule: its value, and its weight and hardness as a goal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Override {
    pub value: Option<f64>,
    pub weight: Option<f64>,
    pub hard: Option<bool>,
    pub tier: Option<Tier>,
}

impl Override {
    fn is_empty(&self) -> bool {
        self.value.is_none() && self.weight.is_none() && self.hard.is_none() && self.tier.is_none()
    }

    fn to_json(&self, d: &RuleDef) -> J {
        match (self.value, self.weight, self.hard, self.tier) {
            (Some(v), None, None, None) => d.value_json(v),
            _ => {
                let mut o = Map::new();
                if let Some(v) = self.value {
                    o.insert("value".into(), d.value_json(v));
                }
                if let Some(w) = self.weight {
                    o.insert("weight".into(), json!(w));
                }
                if let Some(h) = self.hard {
                    o.insert("hard".into(), json!(h));
                }
                if let Some(t) = self.tier {
                    o.insert("tier".into(), json!(t.name()));
                }
                J::Object(o)
            }
        }
    }
}

/// A refused rule change: the rule and why.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleError {
    pub rule: String,
    pub msg: String,
}

impl std::fmt::Display for RuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.rule, self.msg)
    }
}

fn err(rule: &str, msg: impl Into<String>) -> RuleError {
    RuleError {
        rule: rule.to_string(),
        msg: msg.into(),
    }
}

/// Highest weight a soft goal may carry.
pub const MAX_WEIGHT: f64 = 100.0;

fn parse_defs(text: &str) -> Result<Vec<RuleDef>, String> {
    let v: J = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if v["schema"] != SCHEMA {
        return Err(format!("schema must be {SCHEMA}"));
    }
    let list = v["rules"].as_array().ok_or("rules must be a list")?;
    let mut out: Vec<RuleDef> = vec![];
    for r in list {
        let id = r["id"].as_str().ok_or("a rule without id")?.to_string();
        let text = |k: &str| r[k].as_str().map(str::to_string).ok_or(format!("{id}: no {k}"));
        let num = |k: &str| r[k].as_f64().ok_or(format!("{id}: no {k}"));
        let flag = r["flag"].as_bool().unwrap_or(false);
        let options: Vec<String> =
            r["options"].as_array().into_iter().flatten().filter_map(|o| o.as_str().map(str::to_string)).collect();
        let (unit, min, max) = if flag {
            ("flag".to_string(), 0.0, 1.0)
        } else if !options.is_empty() {
            ("choice".to_string(), 0.0, (options.len() - 1) as f64)
        } else {
            (text("unit")?, num("min")?, num("max")?)
        };
        let mut d = RuleDef {
            group: text("group")?,
            unit,
            min,
            max,
            default: None,
            alias: r["alias"].as_str().map(str::to_string),
            int: flag || !options.is_empty() || r["int"].as_bool().unwrap_or(false),
            flag,
            options,
            tier: match r["tier"].as_str() {
                Some(t) => Some(Tier::parse(t).ok_or(format!("{id}: tier must be hard, strong, medium or weak"))?),
                None => None,
            },
            weight: r["weight"].as_f64().unwrap_or(1.0),
            why: r["why"].as_str().unwrap_or_default().to_string(),
            norm: r["norm"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect(),
            id,
        };
        if (d.group == "goal") != d.tier.is_some() {
            return Err(format!("{}: goal rules, and only they, carry a tier", d.id));
        }
        if !(0.0..=MAX_WEIGHT).contains(&d.weight) {
            return Err(format!("{}: weight outside 0..{MAX_WEIGHT}", d.id));
        }
        d.default = match &r["default"] {
            J::Null => None,
            v => Some(d.parse_value(v).ok_or(format!("{}: default must be {}", d.id, d.takes()))?),
        };
        if d.min > d.max || d.default.is_some_and(|x| !d.contains(x)) {
            return Err(format!("{}: default outside {}", d.id, d.range()));
        }
        if !d.id.starts_with(&format!("{}.", d.group)) {
            return Err(format!("{}: id must be group.name", d.id));
        }
        if out.iter().any(|o| o.id == d.id) {
            return Err(format!("{}: twice", d.id));
        }
        out.push(d);
    }
    Ok(out)
}

/// The built-in rules, in file order.
pub fn defs() -> &'static [RuleDef] {
    static D: OnceLock<Vec<RuleDef>> = OnceLock::new();
    D.get_or_init(|| parse_defs(DATA).unwrap_or_else(|e| panic!("rules.json: {e}")))
}

pub fn def(id: &str) -> Option<&'static RuleDef> {
    defs().iter().find(|d| d.id == id)
}

/// The built-in rule a brief field is an alias of.
pub fn by_alias(path: &str) -> Option<&'static RuleDef> {
    defs().iter().find(|d| d.alias.as_deref() == Some(path))
}

/// The spec's `norms` from a country pack: per rule with norm keys, the first key `dim`
/// knows gives `(value, cite)`; a value outside the rule's range is left to the layers below.
pub fn norms_from(dim: impl Fn(&str) -> Option<(J, String)>) -> J {
    let mut out = Map::new();
    for d in defs() {
        let Some((value, cite)) = d.norm.iter().find_map(|k| dim(k)) else {
            continue;
        };
        // a null value is a source that sets no limit (the pack check allows it only so)
        if value.is_null() {
            out.insert(d.id.clone(), json!({"value": null, "unlimited": true, "cite": cite}));
        } else if d.parse_value(&value).is_some_and(|x| d.contains(x)) {
            out.insert(d.id.clone(), json!({"value": value, "cite": cite}));
        }
    }
    J::Object(out)
}

/// Rule groups in the order of the built-in file.
pub fn groups() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = vec![];
    for d in defs() {
        if !out.contains(&d.group.as_str()) {
            out.push(&d.group);
        }
    }
    out
}

/// The overrides of every layer above the built-in set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuleSet {
    layers: [BTreeMap<String, Override>; 4],
    /// Rule id -> the norm clause behind its norm-layer value.
    cites: BTreeMap<String, String>,
    /// Rule id -> the clause saying the norm sets no limit for it.
    unlimited: BTreeMap<String, String>,
}

impl RuleSet {
    pub fn new() -> RuleSet {
        RuleSet::default()
    }

    fn top(&self, id: &str, pick: impl Fn(&Override) -> Option<f64>) -> Option<(f64, Layer)> {
        Layer::OVER
            .iter()
            .rev()
            .find_map(|l| self.layers[l.slot()?].get(id).and_then(&pick).map(|x| (x, *l)))
    }

    /// The value in force and the layer it comes from; `None` for an unknown rule or one only
    /// a preset decides and none did.
    pub fn resolve(&self, id: &str) -> Option<(f64, Layer)> {
        let d = def(id)?;
        self.top(id, |o| o.value).or(d.default.map(|x| (x, Layer::Builtin)))
    }

    pub fn value(&self, id: &str) -> Option<f64> {
        self.resolve(id).map(|r| r.0)
    }

    /// The value in force. Panics for a rule without a built-in default that no layer set:
    /// the builder reads only rules with a default.
    pub fn get(&self, id: &str) -> f64 {
        self.value(id).unwrap_or_else(|| panic!("rule {id}: unknown or without a value"))
    }

    pub fn source(&self, id: &str) -> Option<Layer> {
        self.resolve(id).map(|r| r.1)
    }

    /// Weight of the rule as a soft goal within its tier: the highest layer that set one, else
    /// the built-in weight (1 for a rule that is not a goal).
    pub fn weight(&self, id: &str) -> f64 {
        self.top(id, |o| o.weight).map_or_else(|| def(id).map_or(1.0, |d| d.weight), |w| w.0)
    }

    /// The tier in force: the highest layer that set a tier or `hard`, else the built-in tier.
    /// `hard: false` on a hard goal makes it strong; on a rule that is no goal it says nothing.
    pub fn tier(&self, id: &str) -> Option<Tier> {
        let builtin = def(id)?.tier;
        for l in Layer::OVER.iter().rev() {
            let Some(o) = l.slot().and_then(|k| self.layers[k].get(id)) else {
                continue;
            };
            if let Some(t) = o.tier {
                return Some(t);
            }
            match o.hard {
                Some(true) => return Some(Tier::Hard),
                Some(false) => return builtin.map(|t| if t == Tier::Hard { Tier::Strong } else { t }),
                None => {}
            }
        }
        builtin
    }

    /// A hard rule cuts a variant off; a soft one costs its weight.
    pub fn hard(&self, id: &str) -> bool {
        self.tier(id) == Some(Tier::Hard)
    }

    /// Sets the tier of a goal at `layer`.
    pub fn set_tier(&mut self, layer: Layer, id: &str, tier: Tier) -> Result<(), RuleError> {
        self.slot_of(layer, id)?.tier = Some(tier);
        Ok(())
    }

    /// Sets the norm-layer value of `id` with the clause it comes from.
    pub fn set_norm(&mut self, id: &str, v: &J, cite: &str) -> Result<(), RuleError> {
        self.set_json(Layer::Norm, id, v)?;
        self.cites.insert(id.to_string(), cite.to_string());
        Ok(())
    }

    /// Records that the norm sets no limit for `id`, with the clause that says so.
    pub fn set_norm_unlimited(&mut self, id: &str, cite: &str) -> Result<(), RuleError> {
        if def(id).is_none() {
            return Err(err(id, "unknown rule"));
        }
        self.unlimited.insert(id.to_string(), cite.to_string());
        Ok(())
    }

    /// The clause of a norm that sets no limit for `id`, while no preset, project or brief
    /// value overrides it: the check is off then.
    pub fn unlimited(&self, id: &str) -> Option<&str> {
        match self.top(id, |o| o.value) {
            None => self.unlimited.get(id).map(String::as_str),
            Some(_) => None,
        }
    }

    /// The norm clause behind the value in force; `None` when a layer above the norm or the
    /// built-in default decides.
    pub fn cite(&self, id: &str) -> Option<&str> {
        match self.source(id) {
            Some(Layer::Norm) => self.cites.get(id).map(String::as_str),
            _ => None,
        }
    }

    /// What `layer` says about `id`.
    pub fn layer(&self, layer: Layer, id: &str) -> Option<&Override> {
        self.layers[layer.slot()?].get(id)
    }

    fn slot_of(&mut self, layer: Layer, id: &str) -> Result<&mut Override, RuleError> {
        let k = layer.slot().ok_or_else(|| err(id, "the built-in layer is read only"))?;
        def(id).ok_or_else(|| err(id, "no such rule"))?;
        Ok(self.layers[k].entry(id.to_string()).or_default())
    }

    /// Sets the value at `layer` after checking the rule's range.
    pub fn set(&mut self, layer: Layer, id: &str, value: f64) -> Result<(), RuleError> {
        let d = def(id).ok_or_else(|| err(id, "no such rule"))?;
        if !d.contains(value) {
            return Err(err(id, format!("must be {}", d.takes())));
        }
        self.slot_of(layer, id)?.value = Some(value);
        Ok(())
    }

    /// Sets the value at `layer` from its brief form: a number, a flag's `true`/`false`, a
    /// choice's option name.
    pub fn set_json(&mut self, layer: Layer, id: &str, v: &J) -> Result<(), RuleError> {
        let d = def(id).ok_or_else(|| err(id, "no such rule"))?;
        let x = d.parse_value(v).ok_or_else(|| err(id, format!("must be {}", d.takes())))?;
        self.set(layer, id, x)
    }

    /// The value in force as the brief writes it.
    pub fn value_json(&self, id: &str) -> Option<J> {
        Some(def(id)?.value_json(self.value(id)?))
    }

    /// Sets the weight (0..=MAX_WEIGHT) and hardness of a rule as a goal at `layer`.
    pub fn set_goal(
        &mut self,
        layer: Layer,
        id: &str,
        weight: Option<f64>,
        hard: Option<bool>,
    ) -> Result<(), RuleError> {
        if let Some(w) = weight {
            if !(w.is_finite() && (0.0..=MAX_WEIGHT).contains(&w)) {
                return Err(err(id, format!("weight must be in 0..{MAX_WEIGHT}")));
            }
        }
        let o = self.slot_of(layer, id)?;
        o.weight = weight.or(o.weight);
        o.hard = hard.or(o.hard);
        Ok(())
    }

    /// Drops what `layer` says about `id`: the value falls back to the layers below.
    pub fn clear(&mut self, layer: Layer, id: &str) {
        if let Some(k) = layer.slot() {
            self.layers[k].remove(id);
        }
    }

    /// Drops a whole layer.
    pub fn clear_layer(&mut self, layer: Layer) {
        if let Some(k) = layer.slot() {
            self.layers[k].clear();
        }
    }

    /// The rules `layer` overrides, in id order.
    pub fn overridden(&self, layer: Layer) -> Vec<&str> {
        layer.slot().map_or(vec![], |k| self.layers[k].keys().map(String::as_str).collect())
    }

    /// Applies `{id: value | {value?, weight?, hard?}}` at `layer`: every entry is checked and
    /// either all apply or none.
    pub fn apply_patch(&mut self, layer: Layer, patch: &J) -> Result<(), Vec<RuleError>> {
        let Some(o) = patch.as_object() else {
            return Err(vec![err("", "rules must be an object of rule id to value")]);
        };
        let mut next = self.clone();
        let mut errs = vec![];
        for (id, v) in o {
            let r = match v {
                J::Number(_) | J::Bool(_) | J::String(_) => next.set_json(layer, id, v),
                J::Null => {
                    next.clear(layer, id);
                    Ok(())
                }
                J::Object(g) => {
                    let known = ["value", "weight", "hard", "tier"];
                    let tier = g.get("tier").map(|t| t.as_str().and_then(Tier::parse));
                    if let Some(k) = g.keys().find(|k| !known.contains(&k.as_str())) {
                        Err(err(id, format!("unknown field {k}; use value, weight, hard, tier")))
                    } else if tier == Some(None) {
                        Err(err(id, "tier is hard, strong, medium or weak"))
                    } else {
                        if let Some(Some(t)) = tier {
                            if let Err(e) = next.set_tier(layer, id, t) {
                                errs.push(e);
                            }
                        }
                        let val = match g.get("value") {
                            None | Some(J::Null) => Ok(()),
                            Some(x) => next.set_json(layer, id, x),
                        };
                        let w = g.get("weight").and_then(J::as_f64);
                        let h = g.get("hard").and_then(J::as_bool);
                        let bad_w = g.get("weight").is_some_and(|x| !x.is_number());
                        let bad_h = g.get("hard").is_some_and(|x| !x.is_boolean());
                        val.and_then(|_| {
                            if bad_w || bad_h {
                                Err(err(id, "weight is a number, hard is true or false"))
                            } else {
                                next.set_goal(layer, id, w, h)
                            }
                        })
                    }
                }
                _ => Err(err(id, "must be a value, null or {value, weight, hard}")),
            };
            if let Err(e) = r {
                errs.push(e);
            }
        }
        if errs.is_empty() {
            *self = next;
            Ok(())
        } else {
            Err(errs)
        }
    }

    /// `{"schema": "gen_rules.v1", "<layer>": {id: value | {value, weight, hard}}}`, empty
    /// layers left out.
    pub fn to_json(&self) -> J {
        let mut o = Map::new();
        o.insert("schema".into(), json!(SCHEMA));
        for l in Layer::OVER {
            let m: Map<String, J> = self.layers[l.slot().unwrap_or(0)]
                .iter()
                .filter(|(_, v)| !v.is_empty())
                .filter_map(|(k, v)| def(k).map(|d| (k.clone(), v.to_json(d))))
                .collect();
            if !m.is_empty() {
                o.insert(l.name().into(), J::Object(m));
            }
        }
        J::Object(o)
    }

    pub fn from_json(v: &J) -> Result<RuleSet, Vec<RuleError>> {
        let Some(o) = v.as_object() else {
            return Err(vec![err("", "a rule set is an object")]);
        };
        if o.get("schema").is_some_and(|s| s != SCHEMA) {
            return Err(vec![err("schema", format!("must be {SCHEMA}"))]);
        }
        let mut rs = RuleSet::new();
        let mut errs = vec![];
        for (k, p) in o {
            if k == "schema" {
                continue;
            }
            match Layer::parse(k).filter(|l| l.slot().is_some()) {
                Some(l) => {
                    if let Err(e) = rs.apply_patch(l, p) {
                        errs.extend(e);
                    }
                }
                None => errs.push(err(k, "unknown layer; use norm, preset, project, brief")),
            }
        }
        if errs.is_empty() {
            Ok(rs)
        } else {
            Err(errs)
        }
    }

    /// Every value in force, for fast reads while building.
    fn values(&self) -> HashMap<String, f64> {
        defs().iter().filter_map(|d| self.value(&d.id).map(|x| (d.id.clone(), x))).collect()
    }
}

/// The brief field and value a rule patch entry (`value` or `{value, ...}`) stands for; `None`
/// for a rule without a brief field or a value that does not fit it.
pub fn alias_value(id: &str, v: &J) -> Option<(&'static str, J)> {
    let d = def(id)?;
    let path = d.alias.as_deref()?;
    let raw = match v {
        J::Object(o) => o.get("value")?,
        x => x,
    };
    Some((path, d.value_json(d.parse_value(raw)?)))
}

/// Writes `x` at a dotted brief path (`design.setback`), making the objects on the way. A path
/// through a list (`rooms[].area`), the footprint polygon or a non-object is left alone;
/// whether it was written.
pub fn set_path(v: &mut J, path: &str, x: J) -> bool {
    if path.contains("[]") || path == "footprint.polygon" || !v.is_object() {
        return false;
    }
    let keys: Vec<&str> = path.split('.').collect();
    let mut at = v;
    for k in &keys[..keys.len() - 1] {
        if at[*k].is_null() {
            at[*k] = J::Object(Map::new());
        }
        if !at[*k].is_object() {
            return false;
        }
        at = &mut at[*k];
    }
    at[keys[keys.len() - 1]] = x;
    true
}

thread_local! {
    static ACTIVE: RefCell<Vec<Rc<HashMap<String, f64>>>> = const { RefCell::new(Vec::new()) };
}

fn builtin_values() -> &'static HashMap<String, f64> {
    static V: OnceLock<HashMap<String, f64>> = OnceLock::new();
    V.get_or_init(|| RuleSet::new().values())
}

struct Pop;

impl Drop for Pop {
    fn drop(&mut self) {
        ACTIVE.with(|a| a.borrow_mut().pop());
    }
}

/// Runs `f` with `rs` as the rules [`rule`] reads on this thread.
pub fn scoped<T>(rs: &RuleSet, f: impl FnOnce() -> T) -> T {
    ACTIVE.with(|a| a.borrow_mut().push(Rc::new(rs.values())));
    let _pop = Pop;
    f()
}

/// The value in force of a rule with a built-in default: the scoped set's, else the built-in.
pub fn rule(id: &str) -> f64 {
    let top = ACTIVE.with(|a| a.borrow().last().cloned());
    let v = match &top {
        Some(m) => m.get(id).copied(),
        None => builtin_values().get(id).copied(),
    };
    v.unwrap_or_else(|| panic!("rule {id}: unknown or without a value"))
}

/// [`rule`] in whole millimetres.
pub fn rule_mm(id: &str) -> i64 {
    ddc_geom::to_mm(rule(id))
}
