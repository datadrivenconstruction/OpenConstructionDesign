// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! An export cut off between moving the old book aside and putting the new one in place: the
//! leftovers are found, nothing is moved without the user, and no path loses a file.
use super::*;
use crate::editor::Editor;
use crate::shell::xlsx_project::model_uid;
use crate::shell::xlsx_relink::{self, Click};
use crate::shell::xlsx_win::{book_error, forget_link};
use crate::shell::EditorApp;
use crate::xl_text::Lang;
use ddc_io::xlsx;

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let lv = ed.level;
    let t = ed
        .model
        .add_wall_type("ext", &[ddc_model::LayerSpec { material: "brick".into(), thickness: 0.25 }])
        .unwrap();
    ed.model
        .add_wall(&ddc_model::WallSpec {
            a: (0., 0.),
            b: (6., 0.),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap();
    ed.reload().unwrap();
    ed
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc_app_xs30_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn export(ed: &Editor, path: &Path) {
    xlsx::export_xlsx_families(&ed.model, path, xlsx::Units::Metric, &crate::elem::sheets::families()).unwrap();
}

fn bytes(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap()
}

/// The state a crash leaves between the two moves: `Book.xlsx` renamed to `.bak`, the new book
/// complete at its part name, no `Book.xlsx`. Returns (book, part, bak, old bytes, new bytes).
fn crashed(ed: &Editor, d: &Path) -> (PathBuf, PathBuf, PathBuf, Vec<u8>, Vec<u8>) {
    let book = d.join("Book.xlsx");
    export(ed, &book);
    let old = bytes(&book);
    let bak = d.join("Book.xlsx.bak");
    std::fs::rename(&book, &bak).unwrap();
    let part = crate::shell::xlsx_job::part_of(&book);
    export(ed, &part);
    let new = bytes(&part);
    (book, part, bak, old, new)
}

#[test]
fn crash_after_bak_before_install_offers_both_and_restore_brings_the_old_book_back() {
    let ed = editor();
    let d = dir("restore");
    let (book, part, bak, old, new) = crashed(&ed, &d);
    let l = interrupted(&book, model_uid(&ed.model)).expect("leftovers are found");
    assert_eq!(l, Leftover { dest: book.clone(), part: Some(part.clone()), bak: Some(bak.clone()) });
    let text = line(&l, false);
    assert!(text.contains(&part.display().to_string()) && text.contains(&bak.display().to_string()));
    assert!(restore(&l, false).is_ok());
    assert_eq!(bytes(&book), old, "the old book is back byte for byte");
    assert_eq!(bytes(&part), new, "the new book is not thrown away");
    assert!(!bak.exists());
    assert_eq!(interrupted(&book, model_uid(&ed.model)), None, "a book in place is no leftover");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_complete_new_book_finishes_into_place_and_the_old_one_stays_as_bak() {
    let ed = editor();
    let d = dir("finish");
    let (book, part, bak, old, new) = crashed(&ed, &d);
    let l = interrupted(&book, model_uid(&ed.model)).unwrap();
    let said = finish(&l, false).expect("finished");
    assert!(said.contains(&bak.display().to_string()), "the kept old book is named: {said}");
    assert_eq!(bytes(&book), new);
    assert!(!part.exists());
    assert_eq!(bytes(&bak), old, "the old book is kept whole");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_cut_off_part_is_never_offered_the_bak_is_restored_and_the_part_kept() {
    let ed = editor();
    let d = dir("corrupt");
    let (book, part, bak, old, new) = crashed(&ed, &d);
    // a write cut off half way: the zip directory at its end is missing
    std::fs::write(&part, &new[..new.len() / 2]).unwrap();
    let l = interrupted(&book, model_uid(&ed.model)).expect("the .bak alone is a leftover");
    assert_eq!(l.part, None, "an unreadable part is never offered");
    assert_eq!(l.bak.as_deref(), Some(bak.as_path()));
    assert!(finish(&l, false).is_err());
    assert!(!book.exists(), "a refused finish moves nothing");
    assert!(restore(&l, false).is_ok());
    assert_eq!(bytes(&book), old);
    assert!(part.exists(), "the part is left for the user to look at");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_stray_part_of_another_project_is_ignored() {
    let ed = editor();
    let other = editor();
    assert_ne!(model_uid(&ed.model), model_uid(&other.model));
    let d = dir("stray");
    let book = d.join("Book.xlsx");
    let part = crate::shell::xlsx_job::part_of(&book);
    export(&other, &part);
    assert_eq!(interrupted(&book, model_uid(&ed.model)), None, "another project's part is no leftover");
    assert_eq!(interrupted(&book, None), None, "a project without uid takes no part");
    let found = interrupted(&book, model_uid(&other.model)).expect("its own project finds it");
    assert_eq!(found.part.as_deref(), Some(part.as_path()));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn nothing_left_gives_no_leftover_and_the_relink_card_says_the_book_is_gone() {
    let ed = editor();
    let d = dir("none");
    let book = d.join("Book.xlsx");
    assert_eq!(interrupted(&book, model_uid(&ed.model)), None);
    let none = Leftover { dest: book.clone(), part: None, bak: None };
    assert!(!finish(&none, false).unwrap_err().is_empty());
    assert!(!restore(&none, true).unwrap_err().is_empty());
    let body = xlsx_relink::body(&book, false);
    assert!(body.contains("Book.xlsx") && body.contains(&d.display().to_string()), "{body}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_failed_restore_names_the_bak_and_moves_nothing() {
    let ed = editor();
    let d = dir("fail");
    let (book, part, bak, old, new) = crashed(&ed, &d);
    let l = interrupted(&book, model_uid(&ed.model)).unwrap();
    // the .bak went away meanwhile: the move fails and says where it was looked for
    let gone = Leftover { bak: Some(d.join("Book.xlsx.7.bak")), ..l.clone() };
    for ru in [false, true] {
        let e = restore(&gone, ru).unwrap_err();
        assert!(e.contains(&d.join("Book.xlsx.7.bak").display().to_string()), "{e}");
    }
    // a book saved back meanwhile is never replaced, by either way
    std::fs::write(&book, b"user's own").unwrap();
    assert!(restore(&l, false).unwrap_err().contains(&book.display().to_string()));
    assert!(finish(&l, false).is_err());
    assert_eq!(bytes(&book), b"user's own");
    assert_eq!((bytes(&bak), bytes(&part)), (old, new), "no file was touched");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn an_export_that_could_not_move_the_old_book_back_names_the_bak_in_both_languages() {
    let bak = PathBuf::from("C:/work/Book.xlsx.bak");
    let e = not_restored(&bak);
    for lang in [Lang::En, Lang::Ru] {
        let t = book_error(&e, lang);
        assert!(t.contains(&bak.display().to_string()), "{t}");
        assert!(!t.contains(NOT_RESTORED), "the marker never reaches the user: {t}");
    }
    assert_eq!(not_restored_text("some other error", false), None);
    assert!(restore_failed(false, &bak, "denied").contains(&bak.display().to_string()));
}

#[test]
fn the_newest_backup_is_taken_by_generation() {
    let d = dir("gens");
    let book = d.join("Book.xlsx");
    for n in ["Book.xlsx.bak", "Book.xlsx.2.bak", "Book.xlsx.10.bak", "Other.xlsx.9.bak", "Book.xlsx.x.bak"] {
        std::fs::write(d.join(n), b"x").unwrap();
    }
    assert_eq!(newest_backup(&book), Some(d.join("Book.xlsx.10.bak")));
    assert_eq!(generation("Book.xlsx", "Book.xlsx.bak"), Some(1));
    assert_eq!(generation("Book.xlsx", "Book.xlsx.1.bak"), None);
    assert_eq!(newest_backup(&d.join("Missing.xlsx")), None);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_reused_first_generation_newer_than_old_ones_is_the_one_restored() {
    let d = dir("gens-gap");
    let book = d.join("Book.xlsx");
    let t0 = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for (n, age) in [("Book.xlsx.2.bak", 3), ("Book.xlsx.3.bak", 2), ("Book.xlsx.bak", 0)] {
        let p = d.join(n);
        std::fs::write(&p, n.as_bytes()).unwrap();
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(t0 + std::time::Duration::from_secs(600 * (3 - age))).unwrap();
    }
    assert_eq!(newest_backup(&book), Some(d.join("Book.xlsx.bak")));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn sync_after_the_crash_finds_the_bak_and_the_part_and_restore_relinks_the_book() {
    let d = dir("sync");
    let book = d.join("Book.xlsx");
    let ed = editor();
    export(&ed, &book);
    let mut a = EditorApp::new(ed, None, "test".into());
    a.xlsx_open_now(&book, false);
    assert!(a.xlsx.is_some(), "the book of this project is read and linked");
    a.xlsx = None;
    forget_link();
    let old = bytes(&book);
    let bak = d.join("Book.xlsx.bak");
    std::fs::rename(&book, &bak).unwrap();
    let part = crate::shell::xlsx_job::part_of(&book);
    export(&a.ed, &part);
    a.xlsx_sync();
    xlsx_relink::settle();
    let m = xlsx_relink::pending().expect("the card opens, never a silent fresh export");
    assert!(!a.xlsx_offer);
    let l = xlsx_relink::leftover().expect("the search looks at the .bak and the part");
    assert_eq!((l.part.as_deref(), l.bak.as_deref()), (Some(part.as_path()), Some(bak.as_path())));
    a.xlsx_relink_click(&m, Click::Restore(l));
    assert_eq!(bytes(&book), old);
    assert!(part.exists());
    assert!(a.ed.status.as_deref().is_some_and(|s| !s.is_empty()));
    assert_eq!(xlsx_relink::leftover(), None);
    xlsx_relink::forget();
    let _ = std::fs::remove_dir_all(&d);
}
