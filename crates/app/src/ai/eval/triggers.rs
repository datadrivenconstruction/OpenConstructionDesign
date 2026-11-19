//! Trigger cases of the modelling guides: requests that must make an agent read a guide first
//! ("yes") and requests that must not ("no"). The oracle is the audit log of guide reads, never
//! the text of the answer. Data in `crates/ai/evals/triggers/<skill>.toml`.

use super::case::{evals_dir, to_json};
use crate::ai::mcp_bridge::RESOURCE_READ;
use ddc_model::Model;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The least evidence a guide needs before its trigger counts as measured.
pub const MIN_YES: usize = 5;
pub const MIN_NO: usize = 3;

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct TriggerSet {
    pub skill: String,
    pub yes: Vec<String>,
    pub no: Vec<String>,
}

pub fn triggers_dir() -> PathBuf {
    evals_dir().join("triggers")
}

impl TriggerSet {
    pub fn parse(text: &str) -> Result<TriggerSet, String> {
        let doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("toml: {e}"))?;
        let j = to_json(&toml_edit::Item::Table(doc.as_table().clone()));
        serde_json::from_value(j).map_err(|e| format!("trigger set: {e}"))
    }

    /// What is wrong with the set: too few cases, an unknown guide, a request both ways.
    pub fn problems(&self) -> Vec<String> {
        let mut out = vec![];
        if ddc_ai::skills::find(&self.skill).is_none() {
            out.push(format!("{}: no such guide", self.skill));
        }
        if self.yes.len() < MIN_YES {
            out.push(format!("{}: {} yes, needs {MIN_YES}", self.skill, self.yes.len()));
        }
        if self.no.len() < MIN_NO {
            out.push(format!("{}: {} no, needs {MIN_NO}", self.skill, self.no.len()));
        }
        for y in &self.yes {
            if self.no.contains(y) {
                out.push(format!("{}: '{y}' is both yes and no", self.skill));
            }
        }
        out
    }

    /// Cases that did not hold, given the guide each request made the agent read first.
    pub fn misses(&self, first: impl Fn(&str) -> Option<String>) -> Vec<String> {
        let cases = self.yes.iter().map(|p| (p, true)).chain(self.no.iter().map(|p| (p, false)));
        cases
            .filter(|(p, yes)| !ddc_ai::skills::trigger_holds(&self.skill, *yes, first(p).as_deref()))
            .map(|(p, yes)| format!("{} {}: {p}", self.skill, if yes { "yes" } else { "no" }))
            .collect()
    }
}

/// Every `*.toml` of the folder, sorted by file name.
pub fn load_dir(dir: &Path) -> Result<Vec<TriggerSet>, String> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    files
        .iter()
        .map(|p| {
            let t = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
            TriggerSet::parse(&t).map_err(|e| format!("{}: {e}", p.display()))
        })
        .collect()
}

/// The guide read first after audit row `after_seq`: the trigger oracle of one request.
pub fn first_guide_read(m: &Model, after_seq: i64) -> Option<&'static str> {
    let mut st = m
        .sql()
        .prepare("SELECT endpoint FROM ai_audit WHERE event = ?1 AND seq > ?2 AND endpoint IS NOT NULL ORDER BY seq")
        .ok()?;
    let rows = st.query_map(rusqlite::params![RESOURCE_READ, after_seq], |r| r.get::<_, String>(0)).ok()?;
    let uris: Vec<String> = rows.filter_map(Result::ok).collect();
    ddc_ai::skills::first_read(uris.iter().map(String::as_str))
}
