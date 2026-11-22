// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ddc`: batch work on model files without the editor window.
//!
//! This layer only calls the io and model crates (open, count, export). When the typed command
//! registry lands, the `run` step can switch to it without touching the argument handling.

pub mod args;
pub mod cmd;
pub mod export;
#[cfg(feature = "link")]
pub mod gen;
pub mod info;
pub mod load;
#[cfg(feature = "link")]
pub mod publish;
pub mod run;

use serde_json::{json, Value};

/// Exit codes: everything done, partly done, nothing done (or bad usage).
pub const EXIT_OK: i32 = 0;
pub const EXIT_PARTIAL: i32 = 1;
pub const EXIT_FAILED: i32 = 2;

pub const LOG_SCHEMA: &str = "ddc-cli-log/1";

pub const HELP: &str = "\
ddc <command> [options]     batch work on model files, no window, no GPU

  ddc open <file> --export <ifc|xlsx|csv|stl|obj|step|3mf|svg> <out> [--export <fmt> <out> ...]
      Open a file and write the exports. The input file is never overwritten.
      ifc keeps the schema of an IFC input (IFC4X3_ADD2 otherwise); ifc2x3, ifc4 and ifc4x3
      choose it. csv: one row per element with its columns, GlobalId and properties.
      stl (binary) and obj (+ .mtl, a group per element key) take a unit: stl:mm, obj:m
      (metres by default). They hold the bodies the model itself builds; doors, windows and
      components need the editor's export (ddc-app --file <in> --export <out>).
      step (AP214) and step242 write exact faces of the solids, the other bodies as planar
      faces; millimetres by default (step:m). 3mf checks every body is watertight and takes
      um, mm (default), cm, in, ft or m: 3mf:cm. svg writes every sheet, the first at <out>,
      the others as <out stem>-<sheet number>.svg.
  ddc info <file> [--json]
      Element counts by category, levels and import warnings.
  ddc batch <jobs.json>
      Run a list of independent jobs; a failed job does not stop the others.
      jobs.json: {\"jobs\": [{\"id\": \"a\", \"file\": \"in.ifc\", \"info\": true,
                  \"export\": [{\"format\": \"xlsx\", \"out\": \"out.xlsx\"}]}]}
      (a bare array of jobs works too)
  ddc run <project> <script.lua> --out <new.ddc|.ifc|.xlsx|.csv>
         [--param name=value]... [--level <name>] [--allow-many]
      Run a geometry script on a private copy of the project and save the result to a NEW
      file (never the input). Script errors show line and column. Exit 0 saved, 2 failed.

  ddc gen --brief <brief.json> --out <model.ddc> [--metrics <file.json>] [--force]
      (build with --features link) Generate a building from a ddc-brief/1 document and save it
      to a NEW .ddc. The log lists the brief fields the generator did not use and the values
      it derived, and the routing metrics of the services (the mep key), which --metrics also writes
      to a file. Exit 0 saved, 2 nothing saved (bad brief, unconfirmed values, build refused).

  ddc publish <file.ddc> --out <rev.ddcrev> [--parent <prev.ddcrev>] [--sheets] [--ifc] [--brief <brief.json>]
         [--depth D1..D5] [--name <text>] [--author <text>] [--message <text>] [--project-id <uuid>]
         [--erp-url <url>] [--imported-from <file>] [--revision-id <uuid>] [--created-at <time>] [--force]
      (build with --features link) Write a revision package (ddc-erp-link/1): model, geometry.glb,
      elements.parquet, diff against --parent, optional sheets (PDF) and IFC, manifest with sha256
      per file. Element keys that were missing are minted and saved into the .ddc. The package is
      checked against the contract schemas and re-read before it replaces the output.
      Exit 0 written, 2 nothing written.

Input: .ddc project, .ifc, .ifczip, .rvt (through an installed, trusted module), and the
       mesh formats .glb, .gltf, .step, .stp, .stl, .obj (STL/OBJ without units: see the
       import note in the log).

Options:
  --json     one JSON document on stdout (info: the info document; others: a log)
  --quiet    no progress lines on stderr
  --help, --version

Exit codes (one rule for every command): 0 everything done; 1 partly done (some job or
            export failed while others were written); 2 nothing done, or bad arguments.
            `run` writes one file, so it exits 0 (saved) or 2 (nothing saved), never 1.
Progress goes to stderr, results to stdout.
";

/// One finished step of a command, for the log.
#[derive(Clone, Debug)]
pub struct Step {
    pub name: String,
    pub ok: bool,
    pub ms: f64,
    pub out: Option<String>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

impl Step {
    pub fn to_json(&self) -> Value {
        json!({
            "step": self.name, "ok": self.ok, "ms": self.ms, "out": self.out,
            "error": self.error, "warnings": self.warnings,
        })
    }
}

impl Step {
    pub fn pass(name: &str, t: web_time::Instant, out: Option<String>, warnings: Vec<String>) -> Step {
        Step {
            name: name.into(),
            ok: true,
            ms: ms(t),
            out,
            error: None,
            warnings,
        }
    }

    pub fn fail(name: &str, t: web_time::Instant, error: String) -> Step {
        Step {
            name: name.into(),
            ok: false,
            ms: ms(t),
            out: None,
            error: Some(error),
            warnings: vec![],
        }
    }

    /// Step from a result of `guarded`: the output path and warnings, or the error.
    pub fn of(name: &str, t: web_time::Instant, r: Result<(Option<String>, Vec<String>), String>) -> Step {
        match r {
            Ok((out, w)) => Step::pass(name, t, out, w),
            Err(e) => Step::fail(name, t, e),
        }
    }
}

/// Runs `f`; a panic in a reader or writer becomes an error instead of a crash (exit 101).
pub fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err("internal error".into()))
}

/// 0 when all steps are ok, 2 when none is, 1 otherwise (and 2 for no steps).
pub fn exit_of(steps: &[Step]) -> i32 {
    // opening alone is not work done when something was asked of the file
    let steps: Vec<&Step> = if steps.len() > 1 {
        steps.iter().filter(|s| s.name != "open").collect()
    } else {
        steps.iter().collect()
    };
    let ok = steps.iter().filter(|s| s.ok).count();
    match (ok, steps.len()) {
        (_, 0) | (0, _) => EXIT_FAILED,
        (n, t) if n == t => EXIT_OK,
        _ => EXIT_PARTIAL,
    }
}

pub fn ms(t: web_time::Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 1e5).round() / 100.0
}

/// Progress line on stderr unless quiet.
pub fn progress(quiet: bool, msg: &str) {
    if !quiet {
        eprintln!("[ddc] {msg}");
    }
}
