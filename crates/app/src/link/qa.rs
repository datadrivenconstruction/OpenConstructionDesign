// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The check a model passes before its quantities go to the ERP: errors of model integrity,
//! of the project's information requirements and of DIN 276 cost groups stop the package, and
//! so do values the assistant filled that no person verified, unless the publisher accepts
//! them. Warnings travel with the package as notes. Passing is not proof of code compliance.

use ddc_model::checks::{run_all, CheckScope, Finding, Severity};
use ddc_model::Model;
use serde_json::{json, Value};

/// Rules the gate runs; the rest of the checks stay in the editor.
pub const RULES: [&str; 5] = ["integrity", "company_rules", "missing_params", "din276", "ai_unverified"];

/// Findings the gate reads; more of one kind add nothing to the decision.
const LIMIT: usize = 500;

/// One finding as JSON, with its English text: the package and the ERP log are read in English.
pub fn to_json(f: &Finding, cat: &ddc_ui::Catalog) -> Value {
    let mut text = cat.t(f.msg_key);
    for (i, (_, v)) in f.args.iter().enumerate() {
        text = text.replace(&format!("{{{i}}}"), v);
    }
    let sev = match f.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    };
    let els: Vec<String> = f.elements.iter().take(20).map(|e| format!("{}#{}", e.0, e.1)).collect();
    json!({"rule": f.rule, "severity": sev, "text": text, "elements": els})
}

/// Whether `f` stops the package: an error, or an unverified AI value the publisher did not accept.
pub fn blocks(f: &Finding, allow_unreviewed: bool) -> bool {
    match f.rule {
        "ai_unverified" => !allow_unreviewed,
        _ => f.severity == Severity::Error,
    }
}

/// (blocking, the rest), each as `to_json`.
pub fn run(m: &Model, allow_unreviewed: bool) -> Result<(Vec<Value>, Vec<Value>), String> {
    let found = run_all(m, Some(&RULES), &CheckScope::All, LIMIT).map_err(|e| e.to_string())?;
    let (stop, rest): (Vec<&Finding>, Vec<&Finding>) = found.iter().partition(|f| blocks(f, allow_unreviewed));
    let cat = ddc_ui::Catalog::new(ddc_ui::Lang::En);
    let js = |v: Vec<&Finding>| v.into_iter().map(|f| to_json(f, &cat)).collect();
    Ok((js(stop), js(rest)))
}
