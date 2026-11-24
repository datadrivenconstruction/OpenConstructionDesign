// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! DIN 276 cost groups (`Classification.DIN276`) that are not a cost group: an ERP sums
//! quantities per group, so a typo puts them under none. Empty values are `missing_params`.

use std::collections::BTreeMap;

use super::util::{key, Sink, Within};
use super::{CheckRule, CheckScope, Finding, Fix, Measure, Severity};
use crate::{Error, Model, PVal, CLASSIFICATION_PREFIX};

const SHOWN: usize = 50;

pub(super) struct Din276;

/// A project parameter that holds DIN 276 cost groups: `Classification.DIN276`, `.DIN 276`,
/// `.KG` or `.Kostengruppe`.
pub fn is_din276(param: &str) -> bool {
    let Some(sys) = param.strip_prefix(CLASSIFICATION_PREFIX) else {
        return false;
    };
    let s: String = sys.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase();
    matches!(s.as_str(), "DIN276" | "KG" | "KOSTENGRUPPE")
}

/// `331`, `KG 331`, `331.10` or `331 Tragende Außenwände`: three digits, the first 1 to 8.
pub fn cost_group_ok(v: &str) -> bool {
    let v = v.trim();
    let v = v.strip_prefix("KG").or_else(|| v.strip_prefix("kg")).unwrap_or(v).trim_start();
    let code = v.split_whitespace().next().unwrap_or_default();
    let (head, sub) = match code.split_once('.') {
        Some((h, t)) => (h, !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit())),
        None => (code, true),
    };
    let b = head.as_bytes();
    sub && b.len() == 3 && (b'1'..=b'8').contains(&b[0]) && b.iter().all(u8::is_ascii_digit)
}

impl CheckRule for Din276 {
    fn id(&self) -> &'static str {
        "din276"
    }

    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        let mut s = Sink::new(out, self.id());
        for d in m.params()?.into_iter().filter(|d| is_din276(&d.spec.name)) {
            // one finding per wrong value: each is one fix for the person
            let mut by: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
            for ((t, id), v) in m.param_values(d.id)? {
                let text = match &v {
                    PVal::Text(t) => t.clone(),
                    PVal::Int(i) => i.to_string(),
                    PVal::Num(x) => x.to_string(),
                    PVal::Bool(_) => "?".into(),
                };
                if !text.trim().is_empty() && !cost_group_ok(&text) && w.has(&t, id) {
                    by.entry(text).or_default().push((t, id));
                }
            }
            for (value, mut els) in by {
                els.sort();
                let n = els.len();
                s.add(
                    Severity::Error,
                    els.iter().take(SHOWN).map(|(t, id)| key(t, *id)).collect(),
                    None,
                    "ai.check.din276_bad",
                    vec![("param", d.spec.name.clone()), ("value", value), ("count", n.to_string())],
                    Fix::None,
                    Measure::count(n, 0),
                );
            }
        }
        s.done();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_groups_are_three_digits_from_100_to_899() {
        for ok in ["331", "KG 331", "kg331", "300", "331.10", "331 Tragende Außenwände", " 410 "] {
            assert!(cost_group_ok(ok), "{ok}");
        }
        for bad in ["31", "3310", "931", "033", "33a", "331.", "Wand", "KG", "3 31"] {
            assert!(!cost_group_ok(bad), "{bad}");
        }
    }

    #[test]
    fn the_parameter_is_found_under_its_usual_names() {
        for n in ["Classification.DIN276", "Classification.DIN 276", "Classification.KG", "Classification.Kostengruppe"] {
            assert!(is_din276(n), "{n}");
        }
        assert!(!is_din276("Classification.Uniclass"));
        assert!(!is_din276("DIN276"));
    }
}
