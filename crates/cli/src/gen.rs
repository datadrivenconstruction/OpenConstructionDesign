//! `ddc gen --brief <brief.json> --out <model.ddc>`: generation with no window and no GPU.
//! Built with the `link` feature only; the log names every brief field the generator did not use.

use std::path::{Path, PathBuf};
use web_time::Instant;

use serde_json::{json, Value};

use crate::{export, ms, progress};
use crate::{EXIT_FAILED, EXIT_OK, LOG_SCHEMA};

#[derive(Clone, Debug)]
pub struct GenArgs {
    pub brief: PathBuf,
    pub out: PathBuf,
    pub force: bool,
}

/// Returns (stdout text, exit code): 0 saved, 2 nothing saved. One file is written, never half.
pub fn run(a: &GenArgs, json_out: bool, quiet: bool) -> (String, i32) {
    let t = Instant::now();
    let r = execute(a, quiet);
    let (code, mut log) = match &r {
        Ok(rep) => (EXIT_OK, report_json(rep)),
        Err((kind, msg, extra)) => {
            let mut j = json!({"error": {"kind": kind, "message": msg}});
            if let Some(e) = extra {
                j["error"]["details"] = e.clone();
            }
            (EXIT_FAILED, j)
        }
    };
    log["schema"] = json!(LOG_SCHEMA);
    log["command"] = json!("gen");
    log["exit"] = json!(code);
    log["brief"] = json!(a.brief.to_string_lossy());
    log["out"] = json!(a.out.to_string_lossy());
    log["ms"] = json!(ms(t));
    if json_out {
        return (log.to_string(), code);
    }
    let mut s = String::new();
    match &r {
        Ok(rep) => {
            for u in &rep.unsupported {
                s += &format!("not used: {} ({})\n", u["pointer"].as_str().unwrap_or_default(), u["reason"].as_str().unwrap_or_default());
            }
            for d in &rep.derived {
                s += &format!("derived: {} {}\n", d["pointer"].as_str().unwrap_or_default(), d["what"].as_str().unwrap_or_default());
            }
            s += &format!("ok: saved {}\n", a.out.display());
        }
        Err((_, msg, _)) => s += &format!("error: {msg}\n"),
    }
    (s, code)
}

type Fail = (&'static str, String, Option<Value>);

fn execute(a: &GenArgs, quiet: bool) -> Result<ddc_app::link::gen::Report, Fail> {
    use ddc_app::link::gen::{generate, GenError};
    if let Err(e) = export::check_out(&a.out, &[&a.brief]) {
        return Err(("output", e, None));
    }
    if load_ext(&a.out) != "ddc" {
        return Err(("output", "the output must be a .ddc project".into(), None));
    }
    if a.out.exists() && !a.force {
        return Err(("output", format!("{} exists: pass --force to replace it", a.out.display()), None));
    }
    let text = std::fs::read_to_string(&a.brief)
        .map_err(|e| ("input", format!("cannot read {}: {e}", a.brief.display()), None))?;
    let brief: Value = serde_json::from_str(&text).map_err(|e| ("input", format!("{} is not JSON: {e}", a.brief.display()), None))?;
    progress(quiet, "generating");
    // a build that panics is an error of the run, not a crash
    let res = crate::guarded(|| Ok(generate(&brief, &a.out))).map_err(|e| ("internal", e, None))?;
    res.map_err(|e| {
        let details = match &e {
            GenError::Schema(v) | GenError::Unconfirmed(v) => Some(json!(v)),
            _ => None,
        };
        (e.kind(), e.to_string(), details)
    })
}

fn load_ext(p: &Path) -> String {
    crate::load::ext_of(p)
}

fn report_json(r: &ddc_app::link::gen::Report) -> Value {
    json!({
        "spec": r.spec, "mapped": r.mapped, "unsupported": r.unsupported, "derived": r.derived,
        "project": r.summary, "counts": r.counts, "foundation": r.foundation,
    })
}
