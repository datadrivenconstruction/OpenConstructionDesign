//! The commands: one job (open, info, exports) and a list of jobs.

use std::path::{Path, PathBuf};
use web_time::Instant;

use serde_json::{json, Value};

use crate::export::{self, Format};
use crate::{exit_of, guarded, info, load, progress, Step};
use crate::{EXIT_FAILED, EXIT_OK, EXIT_PARTIAL, LOG_SCHEMA};

#[derive(Clone, Debug)]
pub struct Job {
    pub id: Option<String>,
    pub file: PathBuf,
    pub info: bool,
    pub exports: Vec<(Format, PathBuf)>,
}

pub struct JobResult {
    pub steps: Vec<Step>,
    pub info: Option<Value>,
}

/// Runs one job. `inputs` are all input files of the run: none may be overwritten.
pub fn run_job(job: &Job, inputs: &[&Path], quiet: bool) -> JobResult {
    let mut steps = vec![];
    let mut allowed = vec![];
    for (fmt, out) in &job.exports {
        let name = format!("export:{}", fmt.name());
        match export::check_out(out, inputs) {
            Ok(()) => allowed.push((*fmt, out.clone())),
            Err(e) => steps.push(Step::fail(&name, Instant::now(), e)),
        }
    }
    let t = Instant::now();
    progress(quiet, &format!("opening {}", job.file.display()));
    let loaded = match guarded(|| load::load(&job.file)) {
        Ok(l) => l,
        Err(e) => {
            steps.insert(0, Step::fail("open", t, e));
            return JobResult { steps, info: None };
        }
    };
    steps.insert(0, Step::pass("open", t, None, loaded.warnings.clone()));
    let mut doc = None;
    if job.info {
        doc = Some(info::document(&job.file.to_string_lossy(), &loaded));
    }
    for (fmt, out) in allowed {
        let t = Instant::now();
        progress(quiet, &format!("writing {}", out.display()));
        let name = format!("export:{}", fmt.name());
        let r = guarded(|| export::write_from(&loaded.model, fmt, &out, Some(&job.file)))
            .map(|w| (Some(out.to_string_lossy().into_owned()), w));
        steps.push(Step::of(&name, t, r));
    }
    JobResult { steps, info: doc }
}

fn status(steps: &[Step]) -> &'static str {
    match exit_of(steps) {
        EXIT_OK => "ok",
        EXIT_PARTIAL => "partial",
        _ => "failed",
    }
}

fn job_json(job: &Job, r: &JobResult) -> Value {
    json!({
        "id": job.id, "file": job.file.to_string_lossy(), "status": status(&r.steps),
        "steps": r.steps.iter().map(Step::to_json).collect::<Vec<_>>(), "info": r.info,
    })
}

fn human(job: &Job, r: &JobResult) -> String {
    let mut s = format!("{} {}\n", status(&r.steps), job.file.display());
    for st in &r.steps {
        let tail = st
            .out
            .clone()
            .or_else(|| st.error.clone())
            .unwrap_or_default();
        s += &format!(
            "  {} {} {tail}\n",
            if st.ok { "ok  " } else { "FAIL" },
            st.name
        );
    }
    s
}

/// `open`: one job. Returns (stdout text, exit code).
pub fn open(job: &Job, json_out: bool, quiet: bool) -> (String, i32) {
    if let Some(e) = duplicate_out(std::slice::from_ref(job)) {
        let t = if json_out {
            json!({"schema": LOG_SCHEMA, "command": "open", "exit": EXIT_FAILED, "error": e})
                .to_string()
        } else {
            format!("error: {e}
")
        };
        return (t, EXIT_FAILED);
    }
    let r = run_job(job, &[&job.file], quiet);
    let code = exit_of(&r.steps);
    let text = if json_out {
        let mut j = job_json(job, &r);
        j["schema"] = json!(LOG_SCHEMA);
        j["command"] = json!("open");
        j["exit"] = json!(code);
        j.to_string()
    } else {
        human(job, &r)
    };
    (text, code)
}

/// `info`: the info document (`--json`) or its text.
pub fn info(file: &Path, json_out: bool, quiet: bool) -> (String, i32) {
    let job = Job {
        id: None,
        file: file.into(),
        info: true,
        exports: vec![],
    };
    let r = run_job(&job, &[file], quiet);
    match r.info {
        Some(d) if json_out => (d.to_string(), EXIT_OK),
        Some(d) => (info::text(&d), EXIT_OK),
        None => {
            let e = r
                .steps
                .first()
                .and_then(|s| s.error.clone())
                .unwrap_or_default();
            let text = if json_out {
                json!({"schema": info::INFO_SCHEMA, "file": file.to_string_lossy(), "error": e})
                    .to_string()
            } else {
                format!("error: {e}\n")
            };
            (text, EXIT_FAILED)
        }
    }
}

pub fn parse_jobs(text: &str, base: &Path) -> Result<Vec<Job>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("jobs file: {e}"))?;
    let list = v
        .get("jobs")
        .unwrap_or(&v)
        .as_array()
        .ok_or("jobs file: expected a list of jobs")?;
    let path = |s: &str| {
        let p = PathBuf::from(s);
        if p.is_absolute() {
            p
        } else {
            base.join(p)
        }
    };
    let mut out = vec![];
    for (i, j) in list.iter().enumerate() {
        let n = i + 1;
        let file = j["file"]
            .as_str()
            .ok_or(format!("job {n}: missing \"file\""))?;
        let mut exports = vec![];
        for e in j["export"].as_array().into_iter().flatten() {
            let f = e["format"]
                .as_str()
                .ok_or(format!("job {n}: export without format"))?;
            let o = e["out"]
                .as_str()
                .ok_or(format!("job {n}: export without out"))?;
            let fmt = Format::parse(f).map_err(|m| format!("job {n}: {m}"))?;
            exports.push((fmt, path(o)));
        }
        out.push(Job {
            id: j["id"].as_str().map(String::from),
            file: path(file),
            info: j["info"].as_bool().unwrap_or(false),
            exports,
        });
    }
    if out.is_empty() {
        return Err("jobs file: no jobs".into());
    }
    Ok(out)
}

/// Two exports to the same file would silently overwrite each other: refuse the batch.
fn duplicate_out(jobs: &[Job]) -> Option<String> {
    let outs: Vec<&Path> = jobs
        .iter()
        .flat_map(|j| j.exports.iter().map(|(_, o)| o.as_path()))
        .collect();
    for (i, a) in outs.iter().enumerate() {
        if outs[..i].iter().any(|b| export::same_file(a, b)) {
            return Some(format!("two exports write the same file: {}", a.display()));
        }
    }
    None
}

/// `batch`: every job runs; a failure is recorded and the next job starts.
pub fn batch(jobs_file: &Path, json_out: bool, quiet: bool) -> (String, i32) {
    let parsed = std::fs::read_to_string(jobs_file)
        .map_err(|e| format!("{}: {e}", jobs_file.display()))
        .and_then(|t| parse_jobs(&t, jobs_file.parent().unwrap_or(Path::new(""))));
    let jobs = match parsed {
        Ok(j) => j,
        Err(e) => {
            let t = if json_out {
                json!({"schema": LOG_SCHEMA, "command": "batch", "exit": EXIT_FAILED, "error": e})
                    .to_string()
            } else {
                format!("error: {e}\n")
            };
            return (t, EXIT_FAILED);
        }
    };
    if let Some(e) = duplicate_out(&jobs) {
        let t = if json_out {
            json!({"schema": LOG_SCHEMA, "command": "batch", "exit": EXIT_FAILED, "error": e})
                .to_string()
        } else {
            format!("error: {e}
")
        };
        return (t, EXIT_FAILED);
    }
    let mut inputs: Vec<&Path> = jobs.iter().map(|j| j.file.as_path()).collect();
    inputs.push(jobs_file);
    let mut results = vec![];
    for (i, j) in jobs.iter().enumerate() {
        progress(quiet, &format!("job {}/{}", i + 1, jobs.len()));
        results.push(run_job(j, &inputs, quiet));
    }
    let stat: Vec<&str> = results.iter().map(|r| status(&r.steps)).collect();
    let n = |s: &str| stat.iter().filter(|x| **x == s).count();
    let code = if n("ok") == jobs.len() {
        EXIT_OK
    } else if n("failed") == jobs.len() {
        EXIT_FAILED
    } else {
        EXIT_PARTIAL
    };
    let text = if json_out {
        json!({
            "schema": LOG_SCHEMA, "command": "batch", "exit": code,
            "summary": {"ok": n("ok"), "partial": n("partial"), "failed": n("failed")},
            "jobs": jobs.iter().zip(&results).map(|(j, r)| job_json(j, r)).collect::<Vec<_>>(),
        })
        .to_string()
    } else {
        let mut s: String = jobs
            .iter()
            .zip(&results)
            .map(|(j, r)| human(j, r))
            .collect();
        s += &format!(
            "{} ok, {} partial, {} failed\n",
            n("ok"),
            n("partial"),
            n("failed")
        );
        s
    };
    (text, code)
}
