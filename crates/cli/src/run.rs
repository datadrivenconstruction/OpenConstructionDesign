// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ddc run <project> <script.lua> --out <new>`: draft the script, apply it to a private
//! copy of the project, save the result to a new file. The input is never touched.

use std::path::{Path, PathBuf};
use web_time::Instant;

use ddc_ops::{Cat, Ref};
use ddc_script::{ErrorKind, GeoScript, RunOpts, Sandbox, ScriptError};
use serde_json::{json, Value};

use crate::export::{self, Format};
use crate::{guarded, load, progress, Step};
use crate::{EXIT_FAILED, EXIT_OK, LOG_SCHEMA};

#[derive(Clone, Debug)]
pub struct RunArgs {
    /// `None` with `--new`: the script runs on a fresh project of `levels` levels.
    pub project: Option<PathBuf>,
    pub script: PathBuf,
    pub out: PathBuf,
    /// `name=value`; the value is JSON when it parses, text otherwise.
    pub params: Vec<(String, String)>,
    pub level: Option<String>,
    pub allow_many: bool,
    pub levels: u32,
}

fn kind_name(k: ErrorKind) -> &'static str {
    match k {
        ErrorKind::Syntax => "syntax",
        ErrorKind::Runtime => "runtime",
        ErrorKind::Limit(_) => "limit",
        ErrorKind::Assert => "assert",
        ErrorKind::Param => "param",
    }
}

fn script_json(e: &ScriptError) -> Value {
    json!({"kind": kind_name(e.kind), "line": e.line, "col": e.col,
           "message": e.message, "hint": e.hint})
}

struct Outcome {
    steps: Vec<Step>,
    script_error: Option<ScriptError>,
    created: usize,
    hash: Option<String>,
    log: Vec<String>,
}

fn params_json(p: &[(String, String)]) -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in p {
        let val = serde_json::from_str::<Value>(v).unwrap_or_else(|_| Value::String(v.clone()));
        m.insert(k.clone(), val);
    }
    Value::Object(m)
}

pub(crate) fn save(model: &mut ddc_model::Model, out: &Path) -> Result<Vec<String>, String> {
    let ext = load::ext_of(out);
    if ext == "ddc" {
        let tmp = out.with_extension("ddc.part");
        model
            .save_project_to(&tmp, &mut |_, _| Ok(()))
            .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, out).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })?;
        Ok(vec![])
    } else {
        let f = Format::parse(&ext)
            .map_err(|_| format!("output must be .ddc, .ifc, .xlsx or .csv, got .{ext}"))?;
        export::write(model, f, out)
    }
}

fn execute(a: &RunArgs, quiet: bool) -> Outcome {
    let mut o = Outcome {
        steps: vec![],
        script_error: None,
        created: 0,
        hash: None,
        log: vec![],
    };
    let t = Instant::now();
    let mut inputs: Vec<&Path> = vec![&a.script];
    inputs.extend(a.project.as_deref());
    if let Err(e) = export::check_out(&a.out, &inputs) {
        o.steps.push(Step::fail("check", t, e));
        return o;
    }
    let text = match std::fs::read_to_string(&a.script) {
        Ok(s) => s,
        Err(e) => {
            let why = format!("cannot read {}: {e}", a.script.display());
            o.steps.push(Step::fail("script", t, why));
            return o;
        }
    };
    let loaded = match &a.project {
        Some(p) => {
            progress(quiet, &format!("opening {}", p.display()));
            guarded(|| load::load(p))
        }
        None => guarded(|| crate::new::fresh(a.levels).map(load::Loaded::of)),
    };
    let mut loaded = match loaded {
        Ok(l) => l,
        Err(e) => {
            o.steps.push(Step::fail("open", t, e));
            return o;
        }
    };
    o.steps.push(Step::pass("open", t, None, loaded.warnings.clone()));

    let t = Instant::now();
    let level = match &a.level {
        Some(n) => loaded.model.level_list().unwrap_or_default().into_iter().find(|l| &l.1 == n),
        None => loaded.model.lowest_level().unwrap_or_default(),
    };
    let Some(level) = level else {
        let why = match &a.level {
            Some(n) => format!("no level named '{n}'"),
            None => "the model has no level: add one before running a script".into(),
        };
        o.steps.push(Step::fail("script", t, why));
        return o;
    };
    let mut opts = RunOpts::new(Ref::id(Cat::Level, level.0), params_json(&a.params));
    opts.limits.allow_many = a.allow_many;
    progress(quiet, "running script");
    let run = match guarded(|| Ok(Sandbox.run(&text, &opts))) {
        Err(e) => {
            o.steps.push(Step::fail("script", t, e));
            return o;
        }
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            o.steps.push(Step::fail("script", t, format!("script error at {e}")));
            o.script_error = Some(e);
            return o;
        }
    };
    o.hash = Some(run.hash.clone());
    o.log = run.log.clone();
    o.steps.push(Step::pass("script", t, None, vec![]));

    let t = Instant::now();
    progress(quiet, "applying");
    let mut tick = |done: usize, total: usize| progress(quiet, &format!("applying {done}/{total}"));
    match guarded(|| Ok(loaded.model.apply_envelope_with(&run.env, &mut tick))) {
        Ok(Ok(r)) => {
            o.created = r.created.len();
            o.steps.push(Step::pass("apply", t, None, r.loss));
        }
        Ok(Err(e)) => {
            o.steps.push(Step::fail("apply", t, format!("rejected: {e}")));
            return o;
        }
        Err(e) => {
            o.steps.push(Step::fail("apply", t, e));
            return o;
        }
    }
    let t = Instant::now();
    progress(quiet, &format!("writing {}", a.out.display()));
    let out = a.out.to_string_lossy().into_owned();
    let r = guarded(|| save(&mut loaded.model, &a.out)).map(|w| (Some(out), w));
    o.steps.push(Step::of("save", t, r));
    o
}

/// Returns (stdout text, exit code). Any failed step fails the run: nothing is half-saved.
pub fn run(a: &RunArgs, json_out: bool, quiet: bool) -> (String, i32) {
    let o = execute(a, quiet);
    let code = if o.steps.iter().all(|s| s.ok) {
        EXIT_OK
    } else {
        EXIT_FAILED
    };
    if json_out {
        let j = json!({
            "schema": LOG_SCHEMA, "command": "run", "exit": code,
            "project": a.project.as_ref().map(|p| p.to_string_lossy()), "script": a.script.to_string_lossy(),
            "out": a.out.to_string_lossy(), "created": o.created, "hash": o.hash,
            "log": o.log, "script_error": o.script_error.as_ref().map(script_json),
            "steps": o.steps.iter().map(Step::to_json).collect::<Vec<_>>(),
        });
        return (j.to_string(), code);
    }
    let mut s = String::new();
    for l in &o.log {
        s += &format!("print: {l}\n");
    }
    for st in &o.steps {
        if let Some(e) = &st.error {
            s += &format!("error: {} failed: {e}\n", st.name);
        }
    }
    if let Some(se) = &o.script_error {
        if !se.hint.is_empty() {
            s += &format!("hint: {}\n", se.hint);
        }
    }
    if code == EXIT_OK {
        s += &format!(
            "ok: {} elements created, saved {}\n",
            o.created,
            a.out.display()
        );
    }
    (s, code)
}
