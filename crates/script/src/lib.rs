// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ddc-script`: geometry scripts in a sandbox (docs/design/AI_GEOMETRY_PLAN.md, g2).
//!
//! A script plus its params gives one ops envelope, the same wire MCP agents send, so the
//! structural check, dry run, draft and diff need nothing new. The sandbox has no files, no
//! network and no clock: the same script and params give the same envelope and hash.
//! Limits: CPU time, memory, operations (executed lines and loop turns) and elements.
//! Every created element has a stable key (`group/name#n`) so a rerun with new params becomes
//! reshape, create and delete ops on the elements of the recipe only (`rerun.rs`).

#[cfg(feature = "scripting")]
mod audit;
#[cfg(feature = "scripting")]
mod blocks;
#[cfg(feature = "scripting")]
mod blocks_build;
#[cfg(feature = "scripting")]
mod blocks_surf;
#[cfg(feature = "scripting")]
mod emit;
#[cfg(feature = "scripting")]
mod engine;
#[cfg(feature = "scripting")]
mod geo_build;
#[cfg(feature = "scripting")]
mod geo_curve;
#[cfg(feature = "scripting")]
mod hint;
pub mod recipes;
mod rerun;
#[cfg(feature = "scripting")]
mod span_blocks;
#[cfg(feature = "scripting")]
mod span_check;
#[cfg(feature = "scripting")]
mod stdlib;
mod units;
#[cfg(feature = "scripting")]
mod values;

use std::fmt;
use std::time::Duration;

use ddc_ops::{Actor, Envelope, Ref};
use serde_json::Value as J;
use sha2::{Digest, Sha256};

#[cfg(feature = "scripting")]
pub use blocks::{ifc_of, PartKind, ROLE_IFC};
pub use rerun::{rerun_envelope, Held, Link, RerunPlan};
pub use units::{parse_angle, parse_len};

/// Standard library version; stored with each recipe so a later build knows what ran.
/// 2: blocks, `surf`, `audit`; solids carry a name with role and section.
pub const STD_VERSION: u32 = 2;

/// Hard ceiling of elements even with `allow_many`.
pub const ELEMENTS_HARD: usize = 200_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// CPU time of the running thread.
    pub cpu: Duration,
    /// Interpreter heap, bytes.
    pub memory: usize,
    /// Executed lines plus loop turns of the script.
    pub ops: u64,
    /// Created elements; above it the person must allow many.
    pub elements: usize,
    pub allow_many: bool,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            cpu: Duration::from_secs(2),
            memory: 256 << 20,
            ops: 200_000,
            elements: 20_000,
            allow_many: false,
        }
    }
}

/// What a run needs besides the script text.
#[derive(Clone, Debug)]
pub struct RunOpts {
    /// Level every created element sits on; coordinates in the script are absolute.
    pub level: Ref,
    /// `{name: value}`; lengths as text with a unit (`"12 m"`) or `{v, unit}`.
    pub params: J,
    pub limits: Limits,
    pub actor: Actor,
    pub title: String,
}

impl RunOpts {
    pub fn new(level: Ref, params: J) -> RunOpts {
        RunOpts {
            level,
            params,
            limits: Limits::default(),
            actor: Actor::Script {
                name: "script".into(),
            },
            title: "script".into(),
        }
    }
}

/// One created element: its stable key and the envelope slot of the op that makes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartOut {
    pub key: String,
    pub slot: u32,
}

/// A parameter the script declared with `param(name, default)`.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamOut {
    pub name: String,
    /// Default as written in the script.
    pub default: J,
    /// `m` for lengths, `rad` for angles, `None` for plain numbers, text and flags.
    pub unit: Option<&'static str>,
    /// Value used in this run, in SI for lengths and angles.
    pub value: J,
}

/// A `measure(...)` result: exact, from the script's own geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct Measure {
    pub line: u32,
    pub value: f64,
    pub unit: &'static str,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub ops: u64,
    pub cpu_ms: f64,
    pub memory_peak: usize,
    pub elements: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScriptRun {
    pub env: Envelope,
    pub parts: Vec<PartOut>,
    pub params: Vec<ParamOut>,
    pub measures: Vec<Measure>,
    /// Constraints the script declared; all held, or the run failed.
    pub constraints: usize,
    pub log: Vec<String>,
    /// Hex SHA-256 of the ops and the part keys.
    pub hash: String,
    pub stats: Stats,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    Cpu,
    Memory,
    Ops,
    Elements,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Syntax,
    Runtime,
    Limit(LimitKind),
    Assert,
    Param,
}

/// A refused run: where (1-based line and column of the script) and what to change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptError {
    pub kind: ErrorKind,
    pub line: u32,
    pub col: u32,
    pub message: String,
    pub hint: String,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, col {}: {}", self.line, self.col, self.message)?;
        if !self.hint.is_empty() {
            write!(f, " (hint: {})", self.hint)?;
        }
        Ok(())
    }
}

impl std::error::Error for ScriptError {}

/// The script language seen by the editor and the assistant; the engine stays swappable.
pub trait GeoScript {
    fn run(&self, script: &str, opts: &RunOpts) -> Result<ScriptRun, ScriptError>;
    fn version(&self) -> u32;
}

/// The default sandboxed engine. A fresh interpreter per run: nothing leaks between runs.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sandbox;

impl GeoScript for Sandbox {
    #[cfg(feature = "scripting")]
    fn run(&self, script: &str, opts: &RunOpts) -> Result<ScriptRun, ScriptError> {
        engine::run(script, opts)
    }

    /// Built without the interpreter (the browser build): every run is refused.
    #[cfg(not(feature = "scripting"))]
    fn run(&self, _script: &str, _opts: &RunOpts) -> Result<ScriptRun, ScriptError> {
        Err(ScriptError {
            kind: ErrorKind::Runtime,
            line: 0,
            col: 0,
            message: "scripts are not available in this build".into(),
            hint: String::new(),
        })
    }

    fn version(&self) -> u32 {
        STD_VERSION
    }
}

/// Hash of what the run builds: ops and keys, not the title or the actor.
pub fn hash_of(env: &Envelope, parts: &[PartOut]) -> String {
    let mut h = Sha256::new();
    for g in &env.groups {
        h.update(serde_json::to_string(&g.ops).unwrap_or_default().as_bytes());
    }
    for p in parts {
        h.update(p.key.as_bytes());
        h.update(p.slot.to_le_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Names of the standard library, for the assistant's help and the spelling hints.
pub const STD_NAMES: &[&str] = &[
    "vec3",
    "identity",
    "translate",
    "rot_x",
    "rot_y",
    "rot_z",
    "rot",
    "len",
    "angle",
    "mm",
    "cm",
    "m",
    "deg",
    "param",
    "profile",
    "extrude",
    "sweep",
    "polyline",
    "arc",
    "revolve",
    "loft",
    "transform",
    "pattern",
    "pattern_linear",
    "pattern_radial",
    "part",
    "member",
    "parts",
    "group",
    "define_component",
    "place",
    "boolean",
    "constrain",
    "measure",
    "assert",
    "audit",
    "tower",
    "space_frame",
    "arch",
    "vault",
    "helix_stair",
    "helix_ramp",
    "fins",
    "bridge",
    "stadium_roof",
    "surf",
    "surface_grid",
    "geodesic",
    "hypar_umbrella",
    "pagoda",
    "membrane",
    "perforated_panel",
    "curve",
    "freeform",
    "lattice_between",
    "lattice_volume",
    "pattern_curve",
    "pattern_surface",
    "pattern_grid",
    "print",
    "pairs",
    "ipairs",
    "math",
    "string",
    "table",
];
