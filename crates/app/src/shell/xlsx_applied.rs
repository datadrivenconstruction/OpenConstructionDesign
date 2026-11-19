//! Cells an Apply took from each linked book this session. An Apply that does not rewrite the
//! whole book (ids written in place under a renamed sheet, a skipped, failed or cancelled
//! re-export) leaves the book's baseline at the export; its next read compares with these
//! values instead (`xlsx::Take::applied`), so a value put back in the book is an edit and not
//! "model newer". The book file is never rewritten for it. A fresh re-export drops the entry.
//! Entries are per book and project (`meta.project_uid`, as the schedule import keys its own):
//! another project reading a book at the same path never sees this project's base.
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use ddc_io::xlsx::{Applied, AppliedCell};
use ddc_model::Model;

/// (book, project uid) -> cells applied from that book into that project.
type Key = (String, Option<i64>);

static STORE: Mutex<Option<HashMap<Key, Applied>>> = Mutex::new(None);

/// One book, however its path was spelled, in one project.
fn key(project: Option<i64>, book: &Path) -> Key {
    let p = std::fs::canonicalize(book).unwrap_or_else(|_| book.to_path_buf());
    (p.to_string_lossy().replace('\\', "/").to_lowercase(), project)
}

fn project(model: &Model) -> Option<i64> {
    super::xlsx_project::model_uid(model)
}

fn with<T>(f: impl FnOnce(&mut HashMap<Key, Applied>) -> T) -> T {
    let mut g = STORE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

fn get_in(project: Option<i64>, book: &Path) -> Applied {
    let k = key(project, book);
    with(|m| m.get(&k).cloned().unwrap_or_default())
}

/// What was applied from `book` into `model` since the book's baseline was last written.
pub fn get(model: &Model, book: &Path) -> Applied {
    get_in(project(model), book)
}

/// Remembers the cells one Apply took from `book` into `model`.
pub fn record(model: &Model, book: &Path, cells: &[AppliedCell]) {
    if cells.is_empty() {
        return;
    }
    let k = key(project(model), book);
    with(|m| m.entry(k).or_default().record(cells));
}

/// The book was written again from `model` with a fresh baseline.
pub fn clear(model: &Model, book: &Path) {
    let k = key(project(model), book);
    with(|m| m.remove(&k));
}

/// The editor's model `old` was replaced by `new` (New, Open, import, restore): the closed
/// project's entries go, unless the same project came back (a reopen keeps its base). Entries
/// of a model without a uid are never taken for the same project.
pub fn on_model_replaced(old: &Model, new: &Model) {
    replaced(project(old), project(new));
}

fn replaced(old: Option<i64>, new: Option<i64>) {
    if old.is_some() && old == new {
        return;
    }
    with(|m| m.retain(|k, _| k.1 != old));
}

#[cfg(test)]
#[path = "xlsx_applied_tests.rs"]
mod tests;
