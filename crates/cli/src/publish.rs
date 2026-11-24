// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ddc publish <file.ddc> --out <rev.ddcrev>`: a revision package with no window and no GPU.
//! Built with the `link` feature only; the log lists every repair, skip and assumption.

use web_time::Instant;

use ddc_app::link::publish::{publish, PublishArgs, Report};
use serde_json::{json, Value};

use crate::{ms, progress};
use crate::{EXIT_FAILED, EXIT_OK, LOG_SCHEMA};

pub type Args = PublishArgs;

/// Returns (stdout text, exit code): 0 written, 2 nothing written. One file, never half.
pub fn run(a: &Args, json_out: bool, quiet: bool) -> (String, i32) {
    let t = Instant::now();
    let r = execute(a, quiet);
    let (code, mut log) = match &r {
        Ok(rep) => (EXIT_OK, report_json(rep)),
        Err(e) => {
            let mut j = json!({"error": {"kind": e.kind, "message": e.msg}});
            if let Some(d) = &e.details {
                j["error"]["details"] = d.clone();
            }
            (EXIT_FAILED, j)
        }
    };
    log["schema"] = json!(LOG_SCHEMA);
    log["command"] = json!("publish");
    log["exit"] = json!(code);
    log["file"] = json!(a.file.to_string_lossy());
    log["out"] = json!(a.out.to_string_lossy());
    log["ms"] = json!(ms(t));
    if json_out {
        return (log.to_string(), code);
    }
    let mut s = String::new();
    match &r {
        Ok(rep) => {
            for n in &rep.notes {
                s += &format!("note: {} {}\n", n["step"].as_str().unwrap_or_default(), n["what"].as_str().unwrap_or_default());
            }
            if rep.keys_saved {
                s += &format!("keys: new element keys were saved into {}\n", a.file.display());
            }
            s += &format!("ok: {} ({} bytes, revision {})\n", rep.out.display(), rep.bytes, rep.revision_id);
        }
        Err(e) => {
            s += &format!("error: {}\n", e.msg);
            if let Some(Value::Array(d)) = &e.details {
                for x in d.iter().take(20) {
                    // a check finding is an object: severity, rule, text
                    let f = |k: &str| x[k].as_str().unwrap_or_default().to_string();
                    let line = x.as_str().map(String::from).unwrap_or_else(|| format!("{} {}: {}", f("severity"), f("rule"), f("text")));
                    s += &format!("  {line}\n");
                }
            }
        }
    }
    (s, code)
}

fn execute(a: &Args, quiet: bool) -> Result<Report, ddc_app::link::publish::PubError> {
    use ddc_app::link::publish::PubError;
    if let Err(e) = crate::export::check_out(&a.out, &[&a.file]) {
        return Err(PubError { kind: "output", msg: e, details: None });
    }
    if let Some(p) = a.parent.as_ref().filter(|p| crate::export::check_out(&a.out, &[p.as_path()]).is_err()) {
        return Err(PubError { kind: "parent", msg: format!("the parent {} is the output file", p.display()), details: None });
    }
    // a build that panics is an error of the run, not a crash
    match crate::guarded(|| Ok(publish(a, &mut |s| progress(quiet, s)))) {
        Ok(r) => r,
        Err(e) => Err(PubError { kind: "internal", msg: e, details: None }),
    }
}

fn report_json(r: &Report) -> Value {
    json!({
        "revision_id": r.revision_id, "bytes": r.bytes, "files": r.files, "counts": r.counts,
        "notes": r.notes, "steps": r.steps, "keys_saved": r.keys_saved,
    })
}
