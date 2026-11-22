// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Heavy edits off the UI thread. The UI thread reads the input, a worker computes
//! under the job's cancel token and never touches the model, and the frame loop applies the
//! result as one model operation and one undo step. One heavy edit at a time: a second is
//! refused. With `Editor::bg` off the same compute and apply run in place.

use std::any::Any;
use std::sync::Arc;

use crate::editor::Editor;
use crate::jobs::{Job, JobView, Poll, Progress, CANCELLED};

type Apply = Box<dyn FnOnce(&mut Editor, Box<dyn Any + Send>) -> Result<(), String>>;

pub struct Heavy {
    pub job: Job<Box<dyn Any + Send>>,
    apply: Option<Apply>,
    /// `change_head` when the input was read.
    base: u64,
    /// The result is data computed from that base: dropped if the model moved meanwhile.
    fresh_base: bool,
}

/// How a heavy edit ended, as `Editor::pump_heavy` reports it.
#[derive(Clone, Debug, PartialEq)]
pub enum HeavyEnd {
    Applied,
    Cancelled,
    /// The model changed under a `fresh_base` edit: nothing applied.
    Stale,
    Failed(String),
}

const STAGES: &[&str] = &["job.stage.kernel"];

/// Kernel error text for a worker: a stop becomes the bare cancel marker.
pub fn err_text(e: ddc_model::Error) -> String {
    match e {
        ddc_model::Error::Cancelled => CANCELLED.to_string(),
        e => e.to_string(),
    }
}

impl Editor {
    /// Starts a heavy edit: `work` computes from values read now, `apply` writes the result
    /// (through `apply_ext` or `run_op`, so it is one undo step). False when another heavy
    /// edit runs: the status line says so and nothing starts.
    pub fn start_heavy<T, W, A>(&mut self, title: &'static str, fresh_base: bool, work: W, apply: A) -> bool
    where
        T: Send + 'static,
        W: FnOnce(&Arc<Progress>) -> Result<T, String> + Send + 'static,
        A: FnOnce(&mut Editor, T) -> Result<(), String> + 'static,
    {
        if self.heavy.is_some() {
            self.status = Some(crate::tr("job.busy_heavy", &[]));
            return false;
        }
        if !self.bg {
            let p = Progress::new();
            let tok = p.token();
            let r = ddc_geom::cancel::scope(&tok, || work(&p));
            match r.and_then(|v| apply(self, v)) {
                Ok(()) => {}
                Err(e) if e == CANCELLED => self.status = Some(crate::tr("job.cancelled", &[])),
                Err(e) => self.status = Some(e),
            }
            return true;
        }
        let base = self.model.change_head();
        let job = Job::spawn(title, move |c: &Arc<Progress>| {
            c.phase("job.stage.kernel", 0.0, 1.0);
            work(c).map(|v| Box::new(v) as Box<dyn Any + Send>)
        });
        let apply: Apply = Box::new(move |ed: &mut Editor, v: Box<dyn Any + Send>| match v.downcast::<T>() {
            Ok(v) => apply(ed, *v),
            Err(_) => Err(crate::jobs::internal_error()),
        });
        self.heavy = Some(Heavy {
            job,
            apply: Some(apply),
            base,
            fresh_base,
        });
        true
    }

    /// Per frame: applies a finished heavy edit, or clears a cancelled or failed one.
    pub fn pump_heavy(&mut self) -> Option<HeavyEnd> {
        let h = self.heavy.as_mut()?;
        let end = match h.job.poll() {
            Poll::Running => return None,
            Poll::Done(v) => {
                let (apply, stale) = (h.apply.take(), h.fresh_base && self.model.change_head() != h.base);
                self.heavy = None;
                match apply {
                    _ if stale => HeavyEnd::Stale,
                    Some(f) => match f(self, v) {
                        Ok(()) => HeavyEnd::Applied,
                        Err(e) => HeavyEnd::Failed(e),
                    },
                    None => HeavyEnd::Failed(crate::jobs::internal_error()),
                }
            }
            Poll::Cancelled => HeavyEnd::Cancelled,
            Poll::Failed(e) => HeavyEnd::Failed(e),
        };
        self.heavy = None;
        match &end {
            HeavyEnd::Applied => {}
            HeavyEnd::Cancelled => self.status = Some(crate::tr("job.cancelled", &[])),
            HeavyEnd::Stale => self.status = Some(crate::tr("job.stale", &[])),
            HeavyEnd::Failed(e) => self.status = Some(e.clone()),
        }
        Some(end)
    }

    pub fn cancel_heavy(&self) {
        if let Some(h) = &self.heavy {
            h.job.cancel();
        }
    }

    pub fn heavy_view(&self) -> Option<JobView> {
        self.heavy.as_ref().map(|h| h.job.view(STAGES, None))
    }
}

#[cfg(test)]
#[path = "heavy_tests.rs"]
mod tests;
