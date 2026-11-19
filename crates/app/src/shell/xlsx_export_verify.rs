// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Proof of a written book before it replaces the old one: the new file is read back with
//! the own reader and dry-run against the model it was written from. Anything the dry-run
//! would do (a change, a refusal, a missing or extra row or sheet) refuses the replace; the
//! first differing cell is named and logged. Read-only and computed columns, cells the export
//! cut or blanked and rows with a kept conflict cell may differ.
//!
//! The dry-run reads with the baseline the book carries, as the next import will. A cell
//! wrong in both the sheet and the baseline shows there only as a count: a second read
//! without the baseline names it.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use ddc_io::xlsx;

/// Why the new book did not replace the old one.
#[derive(Clone, Debug, PartialEq)]
pub enum Fault {
    /// The book reads back other than the model; `total` differences, `first` is named.
    Differs { first: xlsx::Mismatch, total: usize },
    /// The check itself failed (the book or the model could not be read).
    Unreadable(String),
    /// The user stopped the check: not a refusal of the book.
    Cancelled,
}

/// True for the text of a refusal made by this check (not a lock or a stale book).
pub fn is_refusal(text: &str) -> bool {
    xlsx::VERIFY_TEMPLATES.iter().any(|(_, tpl)| {
        let head = tpl.split("{}").next().unwrap_or(tpl);
        !head.is_empty() && text.starts_with(head)
    })
}

/// Hook of the check for the job: reports `1 + fraction` (the write stage owns 0..=1), never
/// goes back, and stops the check when `cancel` is set.
pub fn gate(cancel: &Arc<AtomicBool>, progress: &Arc<AtomicU32>) -> xlsx::Hook {
    let (c, p) = (cancel.clone(), progress.clone());
    Arc::new(move |f| {
        // non-negative floats order like their bits
        let v = (1.0 + f.clamp(0.0, 1.0)).to_bits();
        p.fetch_max(v, Ordering::Relaxed);
        !c.load(Ordering::Relaxed)
    })
}

impl Fault {
    /// English refusal text; the UI translates it by its template (`xl.msg.xs6_*`).
    pub fn text(&self) -> String {
        match self {
            Fault::Cancelled => ddc_io::job::CANCELLED.to_string(),
            Fault::Differs { first, .. } => first.message(),
            Fault::Unreadable(e) => xlsx::unreadable_message(e),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Runs once on the written file right before it is read back.
    pub(super) static BEFORE_READ: std::cell::RefCell<Option<Box<dyn FnOnce(&Path)>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
fn before_read(part: &Path) {
    if let Some(f) = BEFORE_READ.with(|m| m.borrow_mut().take()) {
        f(part);
    }
}

#[cfg(not(test))]
fn before_read(_: &Path) {}

/// A second handle on the model's data: the import wants a mutable model, the export has a
/// shared one. A model without a file is copied to a temp file, removed on drop.
struct Handle {
    model: Option<ddc_model::Model>,
    temp: Option<PathBuf>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        // the connection is closed before its file goes
        self.model.take();
        if let Some(t) = self.temp.take() {
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", t.display()));
            }
        }
    }
}

fn handle(model: &ddc_model::Model) -> Result<Handle, String> {
    if let Some(db) = model.db_path() {
        let model = ddc_model::Model::open_copy(&db).map_err(|e| e.to_string())?;
        return Ok(Handle { model: Some(model), temp: None });
    }
    static N: AtomicU32 = AtomicU32::new(0);
    let tmp = std::env::temp_dir().join(format!(
        "ddc_xl_verify_{}_{}.ddc",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    // from here on the guard removes the copy, also when it cannot be opened
    let mut h = Handle { model: None, temp: Some(tmp.clone()) };
    model.backup_to(&tmp).map_err(|e| e.to_string())?;
    h.model = Some(ddc_model::Model::open_copy(&tmp.to_string_lossy()).map_err(|e| e.to_string())?);
    Ok(h)
}

impl Handle {
    fn get(&mut self) -> &mut ddc_model::Model {
        self.model.as_mut().expect("opened in handle()")
    }
}

/// Dry-run of `part` as the book linked at `dest` (its created-row records apply).
fn dry(
    model: &mut ddc_model::Model,
    part: &Path,
    dest: &Path,
    hook: Option<xlsx::Hook>,
) -> Result<xlsx::Report, String> {
    let o = xlsx::ImportOptions { apply: false, delete_missing: false, new_params: false };
    let take = xlsx::Take { made: super::xlsx_links::load(model, dest), ..Default::default() };
    let fams = crate::elem::sheets::families();
    let dec = xlsx::KeyDecisions::default();
    xlsx::import_xlsx_families_hooked(model, part, o, &fams, &take, &dec, hook)
}

/// Every difference of the book at `part` from `model`, first one first; Err when the check
/// could not run. `kept` and `notes` are the export's kept conflict cells and cut cells.
pub fn differences(
    model: &ddc_model::Model,
    part: &Path,
    dest: &Path,
    kept: &[xlsx::KeptCell],
    notes: &[xlsx::ExportNote],
) -> Result<Vec<xlsx::Mismatch>, String> {
    differences_hooked(model, part, dest, kept, notes, None)
}

/// `differences` with a progress/cancel hook; a stop is the error `ddc_io::job::CANCELLED`.
pub fn differences_hooked(
    model: &ddc_model::Model,
    part: &Path,
    dest: &Path,
    kept: &[xlsx::KeptCell],
    notes: &[xlsx::ExportNote],
    hook: Option<xlsx::Hook>,
) -> Result<Vec<xlsx::Mismatch>, String> {
    before_read(part);
    let mut h = handle(model)?;
    let tol = xlsx::Tolerated::new(kept, notes);
    let rep = dry(h.get(), part, dest, hook.clone())?;
    let mut list = xlsx::mismatches(&rep, &tol);
    if list.is_empty() {
        if let Some(n) = xlsx::newer(&rep) {
            // without the baseline only cell values are compared: rows the baseline explains
            // (scoped books, rows outside the book) would read as missing there
            let plain = xlsx::without_baseline(|| dry(h.get(), part, dest, hook))?;
            list = xlsx::mismatches(&plain, &tol)
                .into_iter()
                .filter(|m| m.kind == xlsx::MismatchKind::Value)
                .collect();
            // cut or blanked cells count there too; with none of them the count is a difference
            if list.is_empty() && !tol.has_cells() {
                list.push(n);
            }
        }
    }
    Ok(list)
}

/// Ok when the book at `part` reads back as `model`; the refusal is logged.
pub fn check(
    model: &ddc_model::Model,
    part: &Path,
    dest: &Path,
    kept: &[xlsx::KeptCell],
    notes: &[xlsx::ExportNote],
) -> Result<(), Fault> {
    check_hooked(model, part, dest, kept, notes, None)
}

/// `check` with a progress/cancel hook; a stop is `Fault::Cancelled` and is not logged.
pub fn check_hooked(
    model: &ddc_model::Model,
    part: &Path,
    dest: &Path,
    kept: &[xlsx::KeptCell],
    notes: &[xlsx::ExportNote],
    hook: Option<xlsx::Hook>,
) -> Result<(), Fault> {
    let fault = match differences_hooked(model, part, dest, kept, notes, hook) {
        Ok(list) if list.is_empty() => return Ok(()),
        Ok(list) => {
            let total = list.len();
            Fault::Differs { first: list.into_iter().next().expect("not empty"), total }
        }
        Err(e) if e == ddc_io::job::CANCELLED => return Err(Fault::Cancelled),
        Err(e) => Fault::Unreadable(e),
    };
    let total = match &fault {
        Fault::Differs { total, .. } => *total,
        Fault::Unreadable(_) | Fault::Cancelled => 0,
    };
    eprintln!("xlsx export: {} ({total} differences); {} kept", fault.text(), dest.display());
    Err(fault)
}

#[cfg(test)]
#[path = "xlsx_export_verify_tests.rs"]
mod tests;
