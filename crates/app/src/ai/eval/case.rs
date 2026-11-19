// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! An eval case: a brief (one or more turns) and what the saved model must measure.
//! Cases are TOML data in `crates/ai/evals/cases/`; the executor lives here because it needs
//! the editor.

use serde::Deserialize;
use serde_json::{Map, Value as J};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Case {
    pub id: String,
    /// `hard`, `edit` or `smoke`; hard runs first.
    #[serde(default = "smoke")]
    pub tier: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub lang: String,
    /// `empty`, a fixture path, or `@id` — the saved result of an earlier case.
    #[serde(default = "empty")]
    pub base: String,
    #[serde(default, rename = "turn")]
    pub turns: Vec<Turn>,
    /// AI policy keys the person would have set (consents), as the editor stores them.
    #[serde(default)]
    pub policy: BTreeMap<String, String>,
    #[serde(default)]
    pub oracle: Oracles,
}

fn smoke() -> String {
    "smoke".into()
}

fn empty() -> String {
    "empty".into()
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Turn {
    pub say: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Oracles {
    #[serde(default)]
    pub count: Vec<CountO>,
    #[serde(default)]
    pub geom: Vec<GeomO>,
    #[serde(default)]
    pub text: Vec<TextO>,
    #[serde(default)]
    pub claim: Vec<ClaimO>,
    #[serde(default)]
    pub grow: Vec<CountO>,
    #[serde(default)]
    pub query: Vec<QueryO>,
    #[serde(default)]
    pub qty: Vec<QtyO>,
    #[serde(default)]
    pub receipt: Vec<ReceiptO>,
    pub ifc: Option<IfcO>,
    pub checks: Option<ChecksO>,
}

/// Elements matching a query of the search language, counted within `min..=max`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct QueryO {
    pub q: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// What the applied plans reported: `field` is created, changed, deleted, blocked,
/// `ifc:<Class>` or `qty:<key>[@<level id>]` (the change of a takeoff total, of one level when
/// named), summed over the receipts.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ReceiptO {
    pub field: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// A model total (`walls`, `slab_area`, `room_area` ...) within `tol` (a share, default 0.1) of `value`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct QtyO {
    pub key: String,
    pub level: Option<i64>,
    pub value: f64,
    pub tol: Option<f64>,
}

/// Rows of the model by spec (`Facts::count`), within `min..=max`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct CountO {
    pub spec: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// A geometry or network measure; `kind` picks it, the rest are its parameters.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct GeomO {
    pub kind: String,
    #[serde(flatten)]
    pub p: Map<String, J>,
}

/// The answer text must match `re`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct TextO {
    pub name: String,
    pub re: String,
}

/// If the answer says `say`, the model must have at least `min` of `have`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ClaimO {
    pub name: String,
    pub say: String,
    pub have: String,
    pub min: f64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct IfcO {
    /// Class (`A|B` sums alternatives) → least number of entities in the export.
    pub classes_min: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ChecksO {
    /// New blocking findings allowed over the base model.
    pub new_max: i64,
}

pub(super) fn to_json(item: &toml_edit::Item) -> J {
    use toml_edit::{Item, Value};
    fn val(v: &Value) -> J {
        match v {
            Value::String(s) => J::from(s.value().as_str()),
            Value::Integer(i) => J::from(*i.value()),
            Value::Float(f) => J::from(*f.value()),
            Value::Boolean(b) => J::from(*b.value()),
            Value::Datetime(d) => J::from(d.value().to_string()),
            Value::Array(a) => J::Array(a.iter().map(val).collect()),
            Value::InlineTable(t) => J::Object(t.iter().map(|(k, v)| (k.to_string(), val(v))).collect()),
        }
    }
    match item {
        Item::None => J::Null,
        Item::Value(v) => val(v),
        Item::Table(t) => J::Object(t.iter().map(|(k, v)| (k.to_string(), to_json(v))).collect()),
        Item::ArrayOfTables(a) => J::Array(
            a.iter()
                .map(|t| J::Object(t.iter().map(|(k, v)| (k.to_string(), to_json(v))).collect()))
                .collect(),
        ),
    }
}

impl Case {
    pub fn parse(text: &str) -> Result<Case, String> {
        let doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("toml: {e}"))?;
        let j = to_json(&toml_edit::Item::Table(doc.as_table().clone()));
        serde_json::from_value(j).map_err(|e| format!("case: {e}"))
    }

    pub fn load(path: &Path) -> Result<Case, String> {
        let t = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Case::parse(&t).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The same case in the TOML `parse` reads: the round trip the data files must survive.
    pub fn to_toml(&self) -> String {
        let mut s = String::new();
        let q = |t: &str| J::from(t).to_string();
        s += &format!("id = {}\ntier = {}\ngroup = {}\ntitle = {}\nlang = {}\nbase = {}\n", q(&self.id), q(&self.tier), q(&self.group), q(&self.title), q(&self.lang), q(&self.base));
        for t in &self.turns {
            s += &format!("\n[[turn]]\nsay = {}\n", q(&t.say));
        }
        if !self.policy.is_empty() {
            s += "\n[policy]\n";
            for (k, v) in &self.policy {
                s += &format!("{} = {}\n", q(k), q(v));
            }
        }
        let num = |v: Option<f64>, k: &str| v.map(|v| format!("{k} = {v:?}\n")).unwrap_or_default();
        for (table, list) in [("count", &self.oracle.count), ("grow", &self.oracle.grow)] {
            for c in list {
                s += &format!("\n[[oracle.{table}]]\nspec = {}\n{}{}", q(&c.spec), num(c.min, "min"), num(c.max, "max"));
            }
        }
        for c in &self.oracle.query {
            s += &format!("
[[oracle.query]]
q = {}
{}{}", q(&c.q), num(c.min, "min"), num(c.max, "max"));
        }
        for c in &self.oracle.qty {
            let lv = c.level.map(|l| format!("level = {l}
")).unwrap_or_default();
            s += &format!("
[[oracle.qty]]
key = {}
{lv}value = {:?}
{}", q(&c.key), c.value, num(c.tol, "tol"));
        }
        for c in &self.oracle.receipt {
            s += &format!("\n[[oracle.receipt]]\nfield = {}\n{}{}", q(&c.field), num(c.min, "min"), num(c.max, "max"));
        }
        for g in &self.oracle.geom {
            s += &format!("\n[[oracle.geom]]\nkind = {}\n", q(&g.kind));
            for (k, v) in &g.p {
                s += &format!("{k} = {v}\n");
            }
        }
        for t in &self.oracle.text {
            s += &format!("\n[[oracle.text]]\nname = {}\nre = {}\n", q(&t.name), q(&t.re));
        }
        for c in &self.oracle.claim {
            s += &format!("\n[[oracle.claim]]\nname = {}\nsay = {}\nhave = {}\nmin = {:?}\n", q(&c.name), q(&c.say), q(&c.have), c.min);
        }
        if let Some(i) = &self.oracle.ifc {
            s += "\n[oracle.ifc.classes_min]\n";
            for (k, v) in &i.classes_min {
                s += &format!("{} = {v}\n", q(k));
            }
        }
        if let Some(c) = &self.oracle.checks {
            s += &format!("\n[oracle.checks]\nnew_max = {}\n", c.new_max);
        }
        s
    }
}

/// Every `*.toml` of a directory, parsed; a file that does not parse is an error naming it.
pub fn load_dir(dir: &Path) -> Result<Vec<Case>, String> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    let cases = files.iter().map(|p| Case::load(p)).collect::<Result<Vec<_>, _>>()?;
    Ok(ordered(cases))
}

/// Hard cases first, then edits, then smoke; inside a tier by id.
pub fn ordered(mut v: Vec<Case>) -> Vec<Case> {
    let rank = |t: &str| match t {
        "hard" => 0,
        "edit" => 1,
        _ => 2,
    };
    v.sort_by(|a, b| (rank(&a.tier), &a.id).cmp(&(rank(&b.tier), &b.id)));
    v
}

/// `crates/ai/evals`, next to the sources; the binary and the tests find the data here.
pub fn evals_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ai/evals")
}
