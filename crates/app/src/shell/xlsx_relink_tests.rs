//! A renamed or moved linked book stays linked; another project's book is never offered or
//! linked; nothing is linked without the user's pick.
use super::*;
use crate::editor::Editor;
use crate::shell::xlsx_job::{sync_action, SyncAction};
use crate::shell::xlsx_project::is_foreign;
use crate::shell::xlsx_win::{forget_link, last_book};
use ddc_io::xlsx;

/// Four walls, ids 1..4 in every fresh project.
fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let lv = ed.level;
    let t = ed
        .model
        .add_wall_type("ext", &[ddc_model::LayerSpec { material: "brick".into(), thickness: 0.25 }])
        .unwrap();
    let p = [(0., 0.), (6., 0.), (6., 4.), (0., 4.)];
    for k in 0..4 {
        ed.model
            .add_wall(&ddc_model::WallSpec {
                a: p[k],
                b: p[(k + 1) % 4],
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
    }
    ed.reload().unwrap();
    ed
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc_app_xs22_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn export(model: &Model, path: &Path) {
    xlsx::export_xlsx_families(model, path, xlsx::Units::Metric, &crate::elem::sheets::families()).unwrap();
}

fn first_wall(ed: &Editor) -> i64 {
    ed.model.sql().query_row("SELECT min(id) FROM wall", [], |r| r.get(0)).unwrap()
}

fn stored_path(a: &EditorApp) -> PathBuf {
    stored(&a.ed.model).expect("a stored link").path
}

/// A book linked through the normal read, then the window closed and the session link
/// dropped, as after a restart: only the project's `meta` knows the book.
fn linked(tag: &str) -> (EditorApp, PathBuf, PathBuf) {
    let d = dir(tag);
    let ed = editor();
    let book = d.join("Project.xlsx");
    export(&ed.model, &book);
    let mut a = EditorApp::new(ed, None, "test".into());
    a.xlsx_open_now(&book, false);
    assert!(a.xlsx.is_some(), "the book of this project is read");
    assert_eq!(stored_path(&a), book, "the read stores the link");
    a.xlsx = None;
    forget_link();
    (a, d, book)
}

#[test]
fn link_round_trip_through_meta_across_a_reopen() {
    let d = dir("reopen");
    let db = d.join("p.ddc");
    let book = d.join("Project.xlsx");
    let uid = {
        let m = Model::open(&db.to_string_lossy()).unwrap();
        export(&m, &book);
        store(&m, &book);
        model_uid(&m)
    };
    assert!(uid.is_some());
    let m = Model::open(&db.to_string_lossy()).unwrap();
    assert_eq!(model_uid(&m), uid);
    let l = stored(&m).expect("the link survives the reopen");
    assert_eq!(l.path, book);
    assert_eq!(l.project, uid);
    assert_eq!(l.id, book_id(&book).map(|b| b.1));
    assert!(l.id.is_some());
    assert_eq!(sync_action(false, stored_target(&m)), SyncAction::Open(book.clone(), false));
    drop(m);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn renamed_in_the_same_folder_is_offered_never_linked_by_itself() {
    let (mut a, d, book) = linked("rename");
    let v2 = d.join("Project v2.xlsx");
    std::fs::rename(&book, &v2).unwrap();
    assert_eq!(
        sync_action(a.xlsx.is_some(), stored_target(&a.ed.model)),
        SyncAction::Missing(book.clone()),
        "a gone book is Missing, never OfferExport"
    );
    a.xlsx_sync();
    assert!(!a.xlsx_offer, "no fresh-export offer for a moved book");
    // the card opens at once; the look runs on a worker and its result lands on a later frame
    assert_eq!(pending(), Some(Missing { old: book.clone(), found: vec![] }));
    assert!(searching());
    settle();
    assert!(!searching());
    assert_eq!(pending(), Some(Missing { old: book.clone(), found: vec![v2.clone()] }));
    assert!(a.xlsx.is_none() && a.xlsx_load.is_none(), "nothing is read before the user confirms");
    assert_eq!(stored_path(&a), book);
    // the user confirms: the normal read moves the link to the new name
    a.xlsx_open_now(&v2, false);
    assert!(a.xlsx.as_ref().is_some_and(|p| !is_foreign(p)));
    assert_eq!(stored_path(&a), v2);
    assert_eq!(last_book(), Some(v2.clone()));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_book_edited_and_saved_again_after_the_rename_is_still_found() {
    let (mut a, d, book) = linked("edited");
    let edited = d.join("Project edited.xlsx");
    xlsx::patch_cell(&book, &edited, "Walls", first_wall(&a.ed), "height [m]", 4.0).unwrap();
    std::fs::remove_file(&book).unwrap();
    assert_eq!(book_id(&edited).map(|b| b.1), stored(&a.ed.model).unwrap().id, "cell edits keep the export fingerprint");
    a.xlsx_sync();
    settle();
    assert_eq!(pending().map(|m| m.found), Some(vec![edited.clone()]));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_book_moved_into_a_subfolder_is_found_one_moved_far_is_picked_by_the_user() {
    let (mut a, d, book) = linked("move");
    let sub = d.join("to client");
    std::fs::create_dir_all(&sub).unwrap();
    let moved = sub.join("Project.xlsx");
    std::fs::rename(&book, &moved).unwrap();
    a.xlsx_sync();
    settle();
    assert_eq!(pending(), Some(Missing { old: book.clone(), found: vec![moved.clone()] }));
    // moved far away: nothing near the old place; "Find it" is the way back
    let far_dir = dir("move_far");
    let far = far_dir.join("Project.xlsx");
    std::fs::rename(&moved, &far).unwrap();
    a.xlsx_sync();
    settle();
    assert_eq!(pending(), Some(Missing { old: book.clone(), found: vec![] }));
    assert_eq!(stored_path(&a), book);
    // the user's pick goes through the normal read and is linked
    a.xlsx_open_now(&far, false);
    assert!(a.xlsx.as_ref().is_some_and(|p| !is_foreign(p)));
    assert_eq!(stored_path(&a), far);
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&far_dir);
}

#[test]
fn a_book_of_another_project_is_never_offered_and_its_pick_is_refused() {
    let (mut a, d, book) = linked("foreign");
    let other = editor();
    assert_ne!(model_uid(&other.model), model_uid(&a.ed.model));
    let theirs = d.join("Project v2.xlsx");
    export(&other.model, &theirs);
    std::fs::remove_file(&book).unwrap();
    a.xlsx_sync();
    settle();
    assert_eq!(pending().map(|m| m.found), Some(vec![]), "another project's book is no candidate");
    // a link naming another project offers nothing at all
    let mut l = stored(&a.ed.model).unwrap();
    l.project = model_uid(&other.model);
    assert!(candidates(model_uid(&a.ed.model), &l).is_empty());
    // the user picks it anyway: refused by the project gate, the link stays
    a.xlsx_open_now(&theirs, false);
    assert!(a.xlsx.as_ref().is_some_and(is_foreign));
    assert_eq!(stored_path(&a), book);
    assert_ne!(last_book(), Some(theirs.clone()));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn two_candidates_none_is_picked_the_user_chooses() {
    let (mut a, d, book) = linked("two");
    let v2 = d.join("Project v2.xlsx");
    let copy = d.join("Project copy.xlsx");
    std::fs::rename(&book, &v2).unwrap();
    std::fs::copy(&v2, &copy).unwrap();
    a.xlsx_sync();
    settle();
    assert_eq!(pending(), Some(Missing { old: book.clone(), found: vec![copy.clone(), v2.clone()] }));
    assert!(a.xlsx.is_none() && a.xlsx_load.is_none());
    assert_eq!(stored_path(&a), book, "no candidate is linked by itself");
    a.xlsx_open_now(&v2, false);
    assert_eq!(stored_path(&a), v2);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn export_new_keeps_the_old_link_until_the_new_book_is_written() {
    let (mut a, d, book) = linked("export");
    std::fs::remove_file(&book).unwrap();
    a.xlsx_sync();
    let m = pending().expect("relink card");
    a.xlsx_relink_click(&m, Click::Close);
    assert_eq!(stored_path(&a), book, "Close changes nothing");
    a.xlsx_relink_click(&m, Click::ExportNew);
    assert!(a.xlsx_card.is_some(), "the export starts with its scope card");
    assert_eq!(stored_path(&a), book);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_vanished_preview_book_counts_only_after_the_grace_and_never_while_busy() {
    let p = Path::new("Z:/gone/book.xlsx");
    let t0 = Instant::now();
    let mut s = None;
    assert!(!gone_step(&mut s, p, false, t0));
    assert!(!gone_step(&mut s, p, false, t0 + GRACE / 2));
    assert!(gone_step(&mut s, p, false, t0 + GRACE));
    // back within the grace (a save in progress): the watch starts over
    let mut s = None;
    assert!(!gone_step(&mut s, p, false, t0));
    assert!(!gone_step(&mut s, p, true, t0 + GRACE / 2));
    assert!(!gone_step(&mut s, p, false, t0 + GRACE));
    // the open preview's book
    let (mut a, d, book) = linked("stale");
    a.xlsx_open_now(&book, false);
    std::fs::remove_file(&book).unwrap();
    assert!(!vanished(&book, true, t0 + GRACE * 2), "an export may be replacing it");
    assert!(!vanished(&book, false, t0));
    assert!(vanished(&book, false, t0 + GRACE));
    a.xlsx_missing(book.clone());
    assert_eq!(pending().map(|m| m.old), Some(book.clone()));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn link_and_pref_writes_keep_the_content_hash_a_real_edit_changes_it() {
    let d = dir("hash");
    let ed = editor();
    let book = d.join("Project.xlsx");
    export(&ed.model, &book);
    let h0 = ed.model.content_hash().unwrap();
    store(&ed.model, &book);
    assert!(stored(&ed.model).is_some());
    assert_eq!(ed.model.content_hash().unwrap(), h0, "a link write is no model change");
    store(&ed.model, &d.join("Project v2.xlsx"));
    assert_eq!(ed.model.content_hash().unwrap(), h0, "a link moved to another book neither");
    let put = |k: &str, v: &str| {
        ed.model.sql().execute("INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)", [k, v]).unwrap();
    };
    put("pref:panel.width", "320");
    put("view:zoom", "3");
    assert_eq!(ed.model.content_hash().unwrap(), h0, "pref and view keys are UI state");
    // user meta that only looks alike is content
    put("viewer_note", "1");
    let h1 = ed.model.content_hash().unwrap();
    assert_ne!(h1, h0, "a user meta key is content");
    let w = first_wall(&ed);
    ed.model.sql().execute("UPDATE wall SET height = height + 1.5 WHERE id = ?1", [w]).unwrap();
    assert_ne!(ed.model.content_hash().unwrap(), h1, "a real parameter edit changes the hash");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_fingerprint_is_read_once_per_path_and_stamp() {
    let cache = IdCache::new();
    let reads = std::cell::Cell::new(0);
    let read = |_: &Path| {
        reads.set(reads.get() + 1);
        Some((Some(7), 42))
    };
    let p = Path::new("D:/work/Project.xlsx");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    assert_eq!(cache.get(p, Some((t0, 10)), read), Some((Some(7), 42)));
    assert_eq!(cache.get(p, Some((t0, 10)), read), Some((Some(7), 42)));
    assert_eq!(reads.get(), 1, "same stamp: a hit");
    // saved again: new modified time, or new size at the same time
    cache.get(p, Some((t0 + Duration::from_secs(1), 10)), read);
    assert_eq!(reads.get(), 2, "new modified time: read again");
    cache.get(p, Some((t0 + Duration::from_secs(1), 11)), read);
    assert_eq!(reads.get(), 3, "new size: read again");
    cache.get(p, Some((t0 + Duration::from_secs(1), 11)), read);
    assert_eq!(reads.get(), 3);
    // another path is its own entry; no stamp (no file) is never cached
    cache.get(Path::new("D:/work/Other.xlsx"), Some((t0, 10)), read);
    assert_eq!(reads.get(), 4);
    cache.get(p, None, read);
    cache.get(p, None, read);
    assert_eq!(reads.get(), 6);
    // an unreadable book (locked while saved) is read again next time, and drops the old entry
    let none = |_: &Path| {
        reads.set(reads.get() + 1);
        None
    };
    assert_eq!(cache.get(p, Some((t0, 99)), none), None);
    assert_eq!(cache.get(p, Some((t0, 99)), none), None);
    assert_eq!(reads.get(), 8);
    cache.get(p, Some((t0 + Duration::from_secs(1), 11)), read);
    assert_eq!(reads.get(), 9);
}

#[test]
fn the_cache_stays_bounded() {
    let cache = IdCache::new();
    let t0 = SystemTime::UNIX_EPOCH;
    for i in 0..IdCache::CAP + 5 {
        cache.get(&PathBuf::from(format!("D:/b{i}.xlsx")), Some((t0, 1)), |_| Some((None, 1)));
    }
    assert!(cache.0.lock().unwrap().len() <= IdCache::CAP);
}

#[test]
fn a_real_book_read_through_the_cache_matches_a_direct_read() {
    let d = dir("cache_real");
    let ed = editor();
    let book = d.join("Project.xlsx");
    export(&ed.model, &book);
    assert!(stamp(&book).is_some());
    assert_eq!(book_id_cached(&book), book_id(&book));
    assert_eq!(book_id_cached(&book), book_id(&book));
    assert_eq!(stamp(&d.join("none.xlsx")), None);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_stopped_look_reads_nothing() {
    let (a, d, book) = linked("stopped");
    let v2 = d.join("Project v2.xlsx");
    std::fs::rename(&book, &v2).unwrap();
    let link = stored(&a.ed.model).unwrap();
    let uid = model_uid(&a.ed.model);
    assert_eq!(candidates(uid, &link), vec![v2.clone()]);
    assert!(candidates_until(uid, &link, &AtomicBool::new(true)).is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_look_is_stopped_and_dropped_when_another_project_opens() {
    let (mut a, d, book) = linked("switch");
    std::fs::rename(&book, d.join("Project v2.xlsx")).unwrap();
    a.xlsx_sync();
    assert!(searching());
    let stop = search_stop().expect("a running look");
    assert!(!stop.load(Ordering::Relaxed));
    // what after_swap does on a project switch
    forget_link();
    assert!(stop.load(Ordering::Relaxed), "the worker is told to stop");
    assert!(!searching());
    assert_eq!(pending(), None, "the old project's card is gone");
    // a late result has nowhere to land
    settle();
    assert_eq!(pending(), None);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn closing_the_card_stops_the_look() {
    let (mut a, d, book) = linked("close");
    std::fs::remove_file(&book).unwrap();
    a.xlsx_sync();
    let stop = search_stop().expect("a running look");
    let m = pending().expect("relink card");
    a.xlsx_relink_click(&m, Click::Close);
    assert!(stop.load(Ordering::Relaxed));
    assert!(!searching());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn texts_in_both_languages_without_flipping_the_ui_language() {
    let keys = [
        "xlsx_relink.title",
        "xlsx_relink.body",
        "xlsx_relink.found_one",
        "xlsx_relink.found_many",
        "xlsx_relink.found_none",
        "xlsx_relink.link",
        "xlsx_relink.find",
        "xlsx_relink.find_title",
        "xlsx_relink.export_new",
        "xlsx_relink.close",
        "xlsx_relink.searching",
    ];
    for k in keys {
        let (en, ru) = (crate::tr_lang(false, k, &[]), crate::tr_lang(true, k, &[]));
        assert!(!en.is_empty() && en != k, "{k} en");
        assert_ne!(en, ru, "{k} has its own ru text");
    }
    let cyr = |s: &str| s.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c));
    let old = Path::new("D:/work/Project.xlsx");
    let one = [PathBuf::from("D:/work/Project v2.xlsx")];
    let two = [PathBuf::from("D:/work/a.xlsx"), PathBuf::from("D:/work/b.xlsx")];
    for ru in [false, true] {
        let b = body(old, ru);
        assert!(b.contains("Project.xlsx") && !b.contains('{'), "{b}");
        let f = found_line(&one, ru);
        assert!(f.contains("Project v2.xlsx") && !f.contains('{'), "{f}");
        let n = found_line(&two, ru);
        assert!(n.contains('2') && !n.contains('{'), "{n}");
        assert!(!found_line(&[], ru).contains('{'));
        assert!(link_label(&one[0], ru).contains("Project v2.xlsx"));
        assert_eq!(cyr(&b), ru);
        assert_eq!(cyr(&found_line(&[], ru)), ru);
    }
}

fn warm() -> egui::Context {
    let ctx = egui::Context::default();
    let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
    // the first pass installs the fonts, the second is the one that lays text out
    for _ in 0..2 {
        let _ = ctx.run(Default::default(), |c| {
            th.apply(c);
        });
    }
    ctx
}

fn screen(w: f32) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, 800.0))),
        ..Default::default()
    }
}

fn long_missing(n: usize) -> Missing {
    let deep = "D:/Projects/Very Long Customer Name/Tower B/Structure/Revision 14/Exports/Spreadsheets";
    Missing {
        old: PathBuf::from(format!("{deep}/Book.xlsx")),
        found: (0..n).map(|i| PathBuf::from(format!("{deep}/Archive {i}/Project v2.xlsx"))).collect(),
    }
}

#[test]
fn card_has_one_primary_in_every_state() {
    for found in 0..4 {
        for (part, bak) in [(false, false), (true, false), (false, true), (true, true)] {
            let list = actions(&CardState { found, part, bak });
            let accent = list.iter().filter(|(_, t)| *t == Tone::Accent).count();
            assert_eq!(accent, 1, "found {found} part {part} bak {bak}: {list:?}");
            assert!(list.iter().any(|(k, _)| *k == "xlsx_relink.close"));
        }
    }
    let lead = |s: CardState| actions(&s).into_iter().find(|(_, t)| *t == Tone::Accent).unwrap().0;
    assert_eq!(lead(CardState { found: 1, part: false, bak: false }), "xlsx_relink.link");
    assert_eq!(lead(CardState { found: 3, part: false, bak: false }), "xlsx_relink.find");
    assert_eq!(lead(CardState { found: 1, part: true, bak: true }), "xlsx_recover.finish");
    assert_eq!(lead(CardState { found: 1, part: false, bak: true }), "xlsx_recover.restore");
}

#[test]
fn card_buttons_dispatch_the_same_clicks() {
    let m = long_missing(1);
    let l = Leftover { dest: m.old.clone(), part: Some(PathBuf::from("p")), bak: Some(PathBuf::from("b")) };
    assert_eq!(click_of("xlsx_relink.link", &m, None), Some(Click::Link(m.found[0].clone())));
    assert_eq!(click_of("xlsx_relink.find", &m, None), Some(Click::Find));
    assert_eq!(click_of("xlsx_relink.export_new", &m, None), Some(Click::ExportNew));
    assert_eq!(click_of("xlsx_relink.close", &m, None), Some(Click::Close));
    assert_eq!(click_of("xlsx_recover.finish", &m, Some(&l)), Some(Click::Finish(l.clone())));
    assert_eq!(click_of("xlsx_recover.restore", &m, Some(&l)), Some(Click::Restore(l.clone())));
    assert_eq!(click_of("xlsx_recover.finish", &m, None), None);
    assert_eq!(click_of("xlsx_relink.link", &long_missing(2), None), None);
}

#[test]
fn idle_card_clicks_nothing_and_changes_nothing() {
    let ctx = warm();
    for ru in [false, true] {
        for m in [long_missing(0), long_missing(1), long_missing(3)] {
            for _ in 0..3 {
                let mut out = Some(Click::Close);
                let _ = ctx.run(screen(1280.0), |c| out = card(c, &m, None, ru, false));
                assert_eq!(out, None, "no click without the user");
            }
            assert_eq!(pending(), None, "the card state is untouched");
        }
    }
}

#[test]
fn card_renders_en_ru_and_fits_a_narrow_screen() {
    use ddc_ui::theme::layout;
    let ctx = warm();
    let left = Leftover { dest: long_missing(0).old, part: Some(PathBuf::from("D:/x/part.xlsx")), bak: None };
    for ru in [false, true] {
        for w in [1280.0_f32, 520.0] {
            for (m, l) in [(long_missing(3), None), (long_missing(1), Some(&left)), (long_missing(0), None)] {
                for _ in 0..2 {
                    let _ = ctx.run(screen(w), |c| {
                        let _ = card(c, &m, l, ru, false);
                    });
                }
                let r = ctx.memory(|mem| mem.area_rect(egui::Id::new("xlsx_relink"))).expect("card shown");
                assert!(r.width() <= layout::DIALOG_FORM + 48.0, "{} wide at screen {w}", r.width());
                assert!(r.width() <= w, "card {} wider than screen {w}", r.width());
            }
        }
    }
}

#[test]
fn relink_card_uses_no_raw_widgets() {
    let src = include_str!("xlsx_relink.rs");
    let prod = src.split("#[path = ").next().unwrap();
    for raw in ["ui.button(", "egui::Button::new", "ui.add_enabled(", "ui.small_button(", "Frame::popup"] {
        assert!(!prod.contains(raw), "raw widget {raw} in the relink card");
    }
}
