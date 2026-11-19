// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A book that fails the read-back check, or a check that is stopped, leaves nothing new on
//! the disk (also on the path after Apply) and the old book byte for byte; the check reports
//! progress that never goes back and ends at done. The UI language is never switched here.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;

use ddc_io::xlsx;

use super::super::xlsx_export_verify::{gate, is_refusal, BEFORE_READ};
use super::{export_sync, run, ApplyTail, XlsxJob, JobOpts, Outcome};
use crate::editor::Editor;

fn file_editor(tag: &str) -> (Editor, PathBuf) {
    let dir = std::env::temp_dir().join(format!("ddc_xs6b_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let m = ddc_model::Model::open(dir.join("m.ddc").to_str().unwrap()).unwrap();
    (Editor::seeded(m).unwrap(), dir)
}

/// Names of the workbook-side files beside the model (books, parts, copies, backups).
fn book_files(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("xlsx") || n.contains(".bak") || n.contains("part"))
        .collect()
}

fn names(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn first_book(tag: &str) -> (Editor, PathBuf, PathBuf) {
    let (mut ed, dir) = file_editor(tag);
    ed.model.add_level("L2", 3.0).unwrap();
    let book = dir.join("b.xlsx");
    assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
    (ed, dir, book)
}

fn unreadable_next() {
    BEFORE_READ.with(|b| *b.borrow_mut() = Some(Box::new(|p: &Path| std::fs::write(p, b"not a workbook").unwrap())));
}

fn run_with(ed: &Editor, book: &Path, cancel: &Arc<AtomicBool>, opts: &JobOpts) -> Outcome {
    let (s, p) = (Arc::new(AtomicU8::new(0)), Arc::new(AtomicU32::new(0)));
    run(&ed.model, book, xlsx::Units::Metric, "en", cancel, &s, &p, opts)
}

fn apply_opts() -> JobOpts {
    JobOpts { apply: Some(ApplyTail::default()), ..Default::default() }
}

#[test]
fn failed_check_after_apply_leaves_no_extra_file() {
    let (ed, dir, book) = first_book("apply_fail");
    let before = std::fs::read(&book).unwrap();
    unreadable_next();
    let out = run_with(&ed, &book, &Arc::new(AtomicBool::new(false)), &apply_opts());
    let Outcome::Failed(e) = out else { panic!("a refused book must fail, not become a copy") };
    assert!(is_refusal(&e), "{e}");
    assert_eq!(std::fs::read(&book).unwrap(), before, "original untouched");
    assert_eq!(book_files(&dir), names(&["b.xlsx"]), "nothing new beside the book");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failed_check_on_plain_export_leaves_no_extra_file() {
    let (ed, dir, book) = first_book("plain_fail");
    let before = std::fs::read(&book).unwrap();
    unreadable_next();
    let out = run_with(&ed, &book, &Arc::new(AtomicBool::new(false)), &JobOpts::default());
    assert!(matches!(out, Outcome::Failed(ref e) if is_refusal(e)));
    assert_eq!(std::fs::read(&book).unwrap(), before);
    assert_eq!(book_files(&dir), names(&["b.xlsx"]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failed_check_message_has_both_languages() {
    let (ed, dir, book) = first_book("msg");
    unreadable_next();
    let Outcome::Failed(e) = run_with(&ed, &book, &Arc::new(AtomicBool::new(false)), &apply_opts()) else {
        panic!("must fail");
    };
    let en = crate::xl_text::tr(&e, crate::xl_text::Lang::En);
    let ru = crate::xl_text::tr(&e, crate::xl_text::Lang::Ru);
    assert!(!en.is_empty() && ru != en, "{en} / {ru}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_during_check_writes_nothing() {
    for apply in [false, true] {
        let (ed, dir, book) = first_book(if apply { "cancel_apply" } else { "cancel_plain" });
        let before = std::fs::read(&book).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        // the stop comes once the written file is about to be read back
        BEFORE_READ.with(|b| *b.borrow_mut() = Some(Box::new(move |_: &Path| c.store(true, Ordering::Relaxed))));
        let opts = if apply { apply_opts() } else { JobOpts::default() };
        let out = run_with(&ed, &book, &cancel, &opts);
        assert!(matches!(out, Outcome::Cancelled), "apply={apply}");
        assert_eq!(std::fs::read(&book).unwrap(), before, "original intact");
        assert_eq!(book_files(&dir), names(&["b.xlsx"]), "part file removed, no copy");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn cancel_during_check_of_a_first_export_leaves_no_book() {
    let (ed, dir) = file_editor("cancel_first");
    let book = dir.join("b.xlsx");
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    BEFORE_READ.with(|b| *b.borrow_mut() = Some(Box::new(move |_: &Path| c.store(true, Ordering::Relaxed))));
    assert!(matches!(run_with(&ed, &book, &cancel, &JobOpts::default()), Outcome::Cancelled));
    assert!(book_files(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clean_check_replaces_the_book_and_leaves_nothing() {
    for apply in [false, true] {
        let (mut ed, dir, book) = first_book(if apply { "ok_apply" } else { "ok_plain" });
        let before = std::fs::read(&book).unwrap();
        ed.model.add_level("L3", 6.0).unwrap();
        let opts = if apply { apply_opts() } else { JobOpts::default() };
        let out = run_with(&ed, &book, &Arc::new(AtomicBool::new(false)), &opts);
        assert!(matches!(out, Outcome::Done), "apply={apply}");
        assert_ne!(std::fs::read(&book).unwrap(), before, "book replaced");
        assert_eq!(book_files(&dir), names(&["b.xlsx"]), "no leftovers");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn refusal_texts_are_told_from_other_failures() {
    for (_, tpl) in xlsx::VERIFY_TEMPLATES {
        assert!(is_refusal(&tpl.replace("{}", "x")), "{tpl}");
    }
    assert!(!is_refusal(crate::shell::xlsx_owner::OPEN_ELSEWHERE));
    assert!(!is_refusal("disk full"));
    assert!(!is_refusal(""));
}

#[test]
fn check_progress_never_goes_back_and_stays_in_its_range() {
    let (cancel, p) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicU32::new(1.0f32.to_bits())));
    let hook = gate(&cancel, &p);
    let mut last = 1.0f32;
    for f in [0.0f32, 0.3, 0.9, 0.1, 0.5, 2.0, -1.0] {
        assert!(hook(f), "not cancelled");
        let now = f32::from_bits(p.load(Ordering::Relaxed));
        assert!(now >= last && (1.0..=2.0).contains(&now), "{last} -> {now}");
        last = now;
    }
    cancel.store(true, Ordering::Relaxed);
    assert!(!hook(0.5), "a stop is passed on");
}

#[test]
fn job_progress_is_monotonic_and_ends_done() {
    let (mut ed, dir) = file_editor("progress");
    for i in 0..40 {
        ed.model.add_level(&format!("Lv{i}"), 3.0 * f64::from(i)).unwrap();
    }
    let book = dir.join("b.xlsx");
    let db = ed.model.db_path().expect("file model");
    let opts = JobOpts::default();
    let job = XlsxJob::start_db(db, &book, false, xlsx::Units::Metric, "en", None, opts);
    let mut seen = vec![job.fraction()];
    let out = loop {
        if let Some(o) = job.poll() {
            break o;
        }
        seen.push(job.fraction());
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    seen.push(job.fraction());
    assert!(matches!(out, Outcome::Done));
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
    assert_eq!(*seen.last().unwrap(), 1.0);
    assert_eq!(book_files(&dir), names(&["b.xlsx"]));
    let _ = std::fs::remove_dir_all(&dir);
}
