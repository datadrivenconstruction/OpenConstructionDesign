// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Brief to delivery: one review, one Apply, the files in the background, one result card
//! (docs/design/FLOW_BRIEF_TO_DELIVERY.md). Every step is an existing piece: the drawing set,
//! the sheet print and the IFC save job; this module only orders them.

pub mod ui;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ddc_io::ifc::IfcSchema;
use serde_json::Value as J;

use crate::editor::Editor;
use crate::jobs::{Job, Poll, Progress};
use crate::save::job::{Failure, Outcome as IfcDone, RewriteAs, SaveRequest};
use crate::sheet_view::{Print, PrintRun};

/// The project as one database file the workers read: the work file, or a temp copy of a
/// project held in memory (removed when the last reader drops it).
pub struct Snapshot {
    path: PathBuf,
    temp: bool,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if self.temp {
            ddc_model::remove_db_files(&self.path);
        }
    }
}

impl Snapshot {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn snapshot_of(ed: &Editor) -> Result<Arc<Snapshot>, String> {
    if let Some(w) = ed.project.work_path() {
        return Ok(Arc::new(Snapshot {
            path: w.to_path_buf(),
            temp: false,
        }));
    }
    // in memory: no second connection can reach it, so a copy is taken on this thread
    let path = crate::save::temp_copy();
    let snapshot = Arc::new(Snapshot { path, temp: true });
    ed.model
        .save_project_to(&snapshot.path, &mut |_, _| Ok(()))
        .map_err(|e| e.to_string())?;
    Ok(snapshot)
}

pub fn project_stem(ed: &Editor) -> String {
    ed.project
        .path()
        .and_then(Path::file_stem)
        .map_or_else(|| "project".to_string(), |s| s.to_string_lossy().into_owned())
}

/// The files a delivery writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outputs {
    /// The drawing set as one PDF.
    pub pdf: bool,
    pub ifc: bool,
    pub schema: IfcSchema,
}

pub const SCHEMAS: [(IfcSchema, &str); 3] = [
    (IfcSchema::Ifc4x3, "IFC4X3"),
    (IfcSchema::Ifc4, "IFC4"),
    (IfcSchema::Ifc2x3, "IFC2X3"),
];

impl Outputs {
    /// PDF and IFC4X3.
    pub fn defaults() -> Self {
        Outputs {
            pdf: true,
            ifc: true,
            schema: IfcSchema::Ifc4x3,
        }
    }

    pub fn none() -> Self {
        Outputs {
            pdf: false,
            ifc: false,
            schema: IfcSchema::Ifc4x3,
        }
    }

    pub fn any(&self) -> bool {
        self.pdf || self.ifc
    }

    /// The print step: PDF of the sheets.
    pub fn prints(&self) -> bool {
        self.pdf
    }

    pub fn names(&self) -> Vec<&'static str> {
        [(self.pdf, "pdf"), (self.ifc, "ifc")]
        .into_iter()
        .filter_map(|(on, n)| on.then_some(n))
        .collect()
    }

    pub fn schema_name(&self) -> &'static str {
        SCHEMAS.iter().find(|s| s.0 == self.schema).map_or("IFC4X3", |s| s.1)
    }

    /// `outputs: ["pdf", "ifc"]` and an optional `ifc_schema`, as the AI tool takes them.
    pub fn from_json(a: &J) -> Result<Self, String> {
        let mut o = Outputs::none();
        let list = a["outputs"].as_array().ok_or("outputs must be a list")?;
        for v in list {
            match v.as_str() {
                Some("pdf") => o.pdf = true,
                Some("ifc") => o.ifc = true,
                _ => return Err(format!("unknown output {v}: use pdf or ifc")),
            }
        }
        if let Some(s) = a["ifc_schema"].as_str() {
            o.schema = SCHEMAS
                .iter()
                .find(|x| x.1 == s)
                .map(|x| x.0)
                .ok_or_else(|| format!("unknown ifc_schema {s}: use IFC4X3, IFC4 or IFC2X3"))?;
        }
        Ok(o)
    }
}

/// Where the files go: a folder named after the project, beside the project file.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub dir: PathBuf,
    pub stem: String,
}

impl Target {
    /// `<project folder>/<name>/`; before the first save, `<Documents>/<name>/`.
    pub fn default_for(ed: &Editor) -> Self {
        let stem = project_stem(ed);
        let base = ed
            .project
            .path()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(crate::shell::xlsx_win::writable_default);
        Target {
            dir: base.join(&stem),
            stem,
        }
    }

    pub fn file(&self, ext: &str) -> PathBuf {
        self.dir.join(format!("{}.{ext}", self.stem))
    }

    /// Files a run would replace: the PDF and the IFC of the last delivery.
    pub fn replaces(&self, o: &Outputs) -> Vec<PathBuf> {
        [(o.pdf, "pdf"), (o.ifc, "ifc")]
            .into_iter()
            .filter(|(on, _)| *on)
            .map(|(_, e)| self.file(e))
            .filter(|p| p.is_file())
            .collect()
    }
}

/// Sheets a delivery prints: the drawing set, else every sheet of the project.
pub fn sheets_to_print(ed: &Editor) -> Vec<i64> {
    let set: Vec<i64> = ed
        .model
        .drawing_set_sheets()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.0)
        .collect();
    if !set.is_empty() {
        return set;
    }
    ed.model
        .sheets()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.0)
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// PDF of the sheets.
    Print,
    Ifc,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Waiting,
    Running,
    /// The files written, the main one first, with what the person should know.
    Done {
        files: Vec<PathBuf>,
        note: Option<String>,
    },
    Failed(String),
    Cancelled,
}

impl State {
    pub fn ended(&self) -> bool {
        !matches!(self, State::Waiting | State::Running)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub kind: Kind,
    pub state: State,
}

enum Active {
    Print(Box<PrintRun>),
    Ifc(Job<Result<IfcDone, Failure>>, PathBuf),
}

/// The outputs one after another: the print is stepped on this thread (it reads the model),
/// the IFC runs on a worker from a snapshot.
pub struct Run {
    rows: Vec<Row>,
    outputs: Outputs,
    target: Target,
    sheets: Vec<i64>,
    snapshot: Option<Arc<Snapshot>>,
    active: Option<Active>,
    ru: bool,
}

impl Run {
    pub fn start(outputs: Outputs, target: Target, sheets: Vec<i64>, ru: bool) -> Run {
        let mut rows = vec![];
        for (on, kind) in [
            (outputs.prints(), Kind::Print),
            (outputs.ifc, Kind::Ifc),
        ] {
            if on {
                rows.push(Row {
                    kind,
                    state: State::Waiting,
                });
            }
        }
        Run {
            rows,
            outputs,
            target,
            sheets,
            snapshot: None,
            active: None,
            ru,
        }
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    pub fn outputs(&self) -> Outputs {
        self.outputs
    }

    pub fn finished(&self) -> bool {
        self.rows.iter().all(|r| r.state.ended())
    }

    /// Share of the whole run: ended rows plus the running one's own share.
    pub fn fraction(&self) -> f32 {
        if self.rows.is_empty() {
            return 1.0;
        }
        let ended = self.rows.iter().filter(|r| r.state.ended()).count() as f32;
        let part = match &self.active {
            Some(Active::Print(p)) => p.fraction(),
            Some(Active::Ifc(j, _)) => j.fraction(),
            None => 0.0,
        };
        ((ended + part) / self.rows.len() as f32).min(1.0)
    }

    /// The first written file of a kind, for its Open button.
    pub fn file_of(&self, kind: Kind) -> Option<&Path> {
        self.rows.iter().find(|r| r.kind == kind).and_then(|r| match &r.state {
            State::Done { files, .. } => files.first().map(PathBuf::as_path),
            _ => None,
        })
    }

    /// Stops the running output; the ones not started are marked cancelled.
    pub fn cancel(&mut self) {
        match &mut self.active {
            Some(Active::Print(p)) => p.cancel(),
            Some(Active::Ifc(j, _)) => j.cancel(),
            None => {}
        }
        for r in &mut self.rows {
            if r.state == State::Waiting {
                r.state = State::Cancelled;
            }
        }
    }

    /// A new run of the failed and cancelled outputs only, into the same folder.
    pub fn retry(&self) -> Option<Run> {
        let again = |k: Kind| {
            self.rows
                .iter()
                .any(|r| r.kind == k && matches!(r.state, State::Failed(_) | State::Cancelled))
        };
        let o = Outputs {
            pdf: self.outputs.pdf && again(Kind::Print),
            ifc: again(Kind::Ifc),
            schema: self.outputs.schema,
        };
        o.any()
            .then(|| Run::start(o, self.target.clone(), self.sheets.clone(), self.ru))
    }

    /// One frame of work; true once every output has ended.
    pub fn step(&mut self, ed: &Editor) -> bool {
        if self.active.is_none() {
            self.begin_next(ed);
        }
        self.poll(ed);
        if self.active.is_none() && !self.finished() {
            self.begin_next(ed);
        }
        self.finished()
    }

    fn at(&mut self, kind: Kind) -> &mut State {
        let i = self.rows.iter().position(|r| r.kind == kind).unwrap_or(0);
        &mut self.rows[i].state
    }

    fn begin_next(&mut self, ed: &Editor) {
        let Some(i) = self.rows.iter().position(|r| r.state == State::Waiting) else {
            return;
        };
        let kind = self.rows[i].kind;
        if let Err(e) = std::fs::create_dir_all(&self.target.dir) {
            let shown = self.target.dir.display().to_string();
            let why = crate::tr_lang(self.ru, "deliver.err.folder", &[("dir", &shown)]);
            self.rows[i].state = State::Failed(format!("{why} {e}"));
            return;
        }
        self.rows[i].state = State::Running;
        match kind {
            Kind::Print => self.begin_print(ed, i),
            Kind::Ifc => self.begin_ifc(ed, i),
        }
    }

    fn begin_print(&mut self, ed: &Editor, i: usize) {
        if self.sheets.is_empty() {
            let m = crate::tr_lang(self.ru, "deliver.err.no_sheets", &[]);
            self.rows[i].state = State::Failed(m);
            return;
        }
        let mut p = Print::new(None);
        p.pdf = self.outputs.pdf;
        let target = self.target.file("pdf");
        let run = PrintRun::start(ed, &p, &self.sheets, &target);
        self.active = Some(Active::Print(Box::new(run)));
    }

    fn snapshot(&mut self, ed: &Editor) -> Result<Arc<Snapshot>, String> {
        if let Some(s) = &self.snapshot {
            return Ok(s.clone());
        }
        let s = snapshot_of(ed)?;
        self.snapshot = Some(s.clone());
        Ok(s)
    }

    fn begin_ifc(&mut self, ed: &Editor, i: usize) {
        let snap = match self.snapshot(ed) {
            Ok(s) => s,
            Err(e) => return self.rows[i].state = State::Failed(e),
        };
        let target = self.target.file("ifc");
        let req = SaveRequest {
            snapshot: snap.path().to_path_buf(),
            target: target.clone(),
            backend: Arc::new(RewriteAs(self.outputs.schema)),
            over_source: false,
            last_hash: String::new(),
            last_journal: None,
            replace_wait: ddc_model::RENAME_WAIT,
        };
        let job = Job::spawn("deliver.job.ifc", move |c: &Arc<Progress>| {
            // the snapshot lives until the file is written
            let _hold = &snap;
            Ok(crate::save::job::run(&req, c, &mut |_| Ok(())))
        });
        self.active = Some(Active::Ifc(job, target));
    }

    fn poll(&mut self, ed: &Editor) {
        let ru = self.ru;
        let Some(active) = self.active.take() else {
            return;
        };
        match active {
            Active::Print(mut p) => {
                if !p.step(ed) {
                    self.active = Some(Active::Print(p));
                    return;
                }
                *self.at(Kind::Print) = print_state(p.finish(ru), &self.target, ru);
            }
            Active::Ifc(mut j, target) => match j.poll_finished() {
                Poll::Running => self.active = Some(Active::Ifc(j, target)),
                Poll::Done(r) => *self.at(Kind::Ifc) = ifc_state(r, &target, ru),
                Poll::Failed(e) => *self.at(Kind::Ifc) = State::Failed(e),
                Poll::Cancelled => *self.at(Kind::Ifc) = State::Cancelled,
            },
        }
        if self.active.is_none() && self.rows.iter().all(|r| r.state != State::Waiting) {
            // the temp copy of a project in memory goes once nothing reads it
            self.snapshot = None;
        }
    }
}

fn print_state(d: crate::sheet_view::PrintDone, t: &Target, ru: bool) -> State {
    if d.cancelled {
        return State::Cancelled;
    }
    if d.files == 0 {
        return State::Failed(d.report);
    }
    let mut files: Vec<PathBuf> = d.pdf.into_iter().collect();
    if files.is_empty() {
        files.push(t.dir.clone());
    }
    let n = d.files.to_string();
    let head = crate::tr_lang(ru, "deliver.done.print", &[("n", &n)]);
    let note = if d.clean {
        head
    } else {
        format!("{head}\n{}", d.report)
    };
    State::Done {
        files,
        note: Some(note),
    }
}

fn ifc_state(r: Result<IfcDone, Failure>, target: &Path, ru: bool) -> State {
    match r {
        Ok(o) => {
            let file = crate::save::written_file(target, &o);
            let skipped = o.report.not_written_total;
            let note = (skipped > 0).then(|| {
                crate::tr_lang(ru, "deliver.done.ifc_skipped", &[("n", &skipped.to_string())])
            });
            State::Done {
                files: vec![file],
                note,
            }
        }
        Err(f) if f.cancelled => State::Cancelled,
        // the finished file could not take the target's place: it is kept under this name
        Err(Failure {
            kept: Some(k),
            message,
            ..
        }) => State::Done {
            files: vec![k],
            note: Some(message),
        },
        Err(f) => State::Failed(f.message),
    }
}
