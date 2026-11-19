// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One busy gate for open, add and import, and the queue for requests that arrive meanwhile.
//! Pure: the app fills [`Jobs`], the rules live here.
use std::collections::VecDeque;

/// What is running or asking right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Jobs {
    /// A big-file job (plan, build, drawings, mesh cache).
    pub big: bool,
    /// A project save or open, or a finished build waiting for its answer.
    pub project: bool,
    /// A module import (.rvt, .rfa) started by the Import window.
    pub import: bool,
    /// A prompt or message is up.
    pub modal: bool,
}

impl Jobs {
    /// Something works on, or waits to replace, the model: no second job may start.
    pub fn running(self) -> bool {
        self.big || self.project || self.import
    }

    /// Nothing may start or ask a question now.
    pub fn busy(self) -> bool {
        self.running() || self.modal
    }
}

/// Most requests kept while a job runs; the rest are reported, not silently lost.
pub const QUEUE_CAP: usize = 64;

/// Puts `new` behind what already waits. Returns how many did not fit.
pub fn enqueue<T>(queue: &mut VecDeque<T>, new: Vec<T>) -> usize {
    let room = QUEUE_CAP.saturating_sub(queue.len());
    let dropped = new.len().saturating_sub(room);
    queue.extend(new.into_iter().take(room));
    dropped
}

/// What the Import window does with a file, by extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// IFC: planned on a worker with progress and cancel, then added as one undo step.
    Worker,
    /// Drawings: the placement form.
    Drawing,
    /// XLSX: the change preview.
    Table,
    /// .rvt, .rfa: the installed module, on a worker.
    Module,
}

pub fn route(ext: &str) -> Route {
    match ext.to_ascii_lowercase().as_str() {
        "ifc" | "ifczip" => Route::Worker,
        e if ddc_io::meshx::READ_EXTS.contains(&e) => Route::Worker,
        "dxf" | "dwg" | "pdf" => Route::Drawing,
        "xlsx" => Route::Table,
        _ => Route::Module,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_refused_while_import_job_runs() {
        let j = Jobs {
            import: true,
            ..Jobs::default()
        };
        assert!(j.running() && j.busy());
        let big = Jobs {
            big: true,
            ..Jobs::default()
        };
        let project = Jobs {
            project: true,
            ..Jobs::default()
        };
        assert!(big.running() && project.running());
        assert!(!Jobs::default().busy());
    }

    #[test]
    fn a_prompt_blocks_but_is_not_a_job() {
        let j = Jobs {
            modal: true,
            ..Jobs::default()
        };
        assert!(j.busy() && !j.running());
    }

    #[test]
    fn a_drop_during_a_job_is_queued_not_lost() {
        let mut q: VecDeque<u32> = VecDeque::from(vec![1]);
        assert_eq!(enqueue(&mut q, vec![2, 3]), 0);
        assert_eq!(q.iter().copied().collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn the_queue_reports_what_does_not_fit() {
        let mut q: VecDeque<usize> = (0..QUEUE_CAP - 1).collect();
        assert_eq!(enqueue(&mut q, vec![0, 0, 0]), 2);
        assert_eq!(q.len(), QUEUE_CAP);
    }

    #[test]
    fn import_window_ifc_runs_on_worker() {
        for e in ["ifc", "IFC", "ifczip"] {
            assert_eq!(route(e), Route::Worker, "{e}");
        }
        assert_eq!(route("dwg"), Route::Drawing);
        assert_eq!(route("xlsx"), Route::Table);
        assert_eq!(route("rvt"), Route::Module);
        assert_eq!(route("rfa"), Route::Module);
    }
}
