//! Close -> Save -> a family draft that replaces project type values: the family window asks
//! first, the project action waits for that answer. No answer loses a draft or a project edit,
//! and nothing closes with unsaved work.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ddc_ui::{Catalog, Lang};

use super::{answer, close_requested, drafts, guard, poll_job, resume_drafts, start_save};
use super::{request_new, request_open, request_restore, unsaved};
use super::{Act, Notice, Pending};
use crate::editor::Editor;
use crate::family_editor as fe;
use crate::shell::EditorApp;

const LANGS: [Lang; 2] = [Lang::En, Lang::Ru];

fn wait(a: &mut EditorApp) {
    for _ in 0..2000 {
        if !a.project_ui.busy() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
        poll_job(a);
    }
    panic!("job did not finish");
}

/// A project saved to `x.ddc` holding family "Crate" (W = 1 m) and an edited, ticked tab of it
/// (W = 2 m) that is not in the project yet. Returns the app, its folder, the file, the family.
fn setup(tag: &str, lang: Lang) -> (EditorApp, PathBuf, PathBuf, i64) {
    let d = std::env::temp_dir().join(format!("ddc-r94b-{tag}-{lang:?}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let mut a = EditorApp::new(Editor::start_project().unwrap(), None, "test".into());
    a.prefs.autosave_dir = d.join("auto").display().to_string();
    a.sh.cat = Catalog::new(lang);
    fe::with(|s| *s = Default::default());
    drafts::discard_all();
    let fam = fe::seed_overwrite_for_test(&mut a.ed, &a.sh.cat);
    let file = d.join("x.ddc");
    assert!(start_save(&mut a, Some(file.clone())));
    wait(&mut a);
    assert!(!unsaved(&a).project(), "the project file holds the model");
    assert_eq!(drafts::count(&a.ed), 1, "the edited tab is a draft");
    (a, d, file, fam)
}

fn done(d: &Path) {
    fe::with(|s| *s = Default::default());
    drafts::discard_all();
    let _ = std::fs::remove_dir_all(d);
}

/// Close, then Save on the unsaved-changes question: stops on the overwrite question.
fn close_and_save(a: &mut EditorApp, fam: i64) {
    let steps = a.ed.undo_len();
    assert!(close_requested(a));
    let p = a.project_ui.prompt.take().expect("the close asks");
    assert_eq!(p, Pending::Close);
    answer(a, p, Act::Save);
    assert!(!a.project_ui.close_now, "nothing closes before the answer");
    assert!(a.project_ui.job.is_none(), "no project save before the answer");
    assert_eq!(a.project_ui.after_drafts, Some(Pending::Close), "the close waits");
    assert!(fe::asks_overwrite(), "the family window asks");
    let c = &a.sh.cat;
    assert_eq!(a.ed.status.as_deref(), Some(c.t("drafts_overwrite.drafts.ow_wait").as_str()), "says why");
    let text = fe::overwrite_text_for_test(c).expect("question text");
    assert!(text.contains(&c.t("drafts_overwrite.ow.project")), "{text}");
    assert!(text.contains("Std"), "{text}");
    assert!((fe::type_w_for_test(&a.ed.model, fam) - 1.0).abs() < 1e-9, "asking writes nothing");
    assert_eq!(a.ed.undo_len(), steps);
    // frames go by while the question is up: the close keeps waiting
    resume_drafts(a);
    resume_drafts(a);
    assert_eq!(a.project_ui.after_drafts, Some(Pending::Close));
    assert!(!a.project_ui.close_now);
}

#[test]
fn strings_exist_in_both_languages() {
    let (en, ru) = (Catalog::new(Lang::En), Catalog::new(Lang::Ru));
    for k in ["drafts_overwrite.ow.project", "drafts_overwrite.drafts.ow_wait", "drafts_overwrite.drafts.ow_back"] {
        assert_ne!(en.t(k), k, "en {k}");
        assert_ne!(ru.t(k), k, "ru {k}");
        assert_ne!(en.t(k), ru.t(k), "{k} is translated");
        assert!(ru.t(k).chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)), "ru {k}");
    }
}

#[test]
fn replace_loads_the_family_writes_the_file_and_closes() {
    for lang in LANGS {
        let (mut a, d, _file, fam) = setup("replace", lang);
        close_and_save(&mut a, fam);
        let steps = a.ed.undo_len();
        fe::answer_overwrite_for_test(&mut a.ed, &a.sh.cat, true);
        assert!(!fe::asks_overwrite());
        assert!((fe::type_w_for_test(&a.ed.model, fam) - 2.0).abs() < 1e-9, "{lang:?}");
        assert_eq!(a.ed.undo_len(), steps + 1, "{lang:?}: one undo step for the load");
        resume_drafts(&mut a);
        wait(&mut a);
        assert!(a.project_ui.close_now, "{lang:?}: the close goes on after the save");
        assert!(!unsaved(&a).any(), "{lang:?}: everything is in the file: {:?}", unsaved(&a));
        assert_eq!(drafts::count(&a.ed), 0, "{lang:?}");
        assert!(a.project_ui.after_drafts.is_none());
        done(&d);
    }
}

#[test]
fn back_keeps_the_draft_and_the_project_and_stays_open() {
    for lang in LANGS {
        let (mut a, d, file, fam) = setup("back", lang);
        let before = std::fs::read(&file).unwrap();
        close_and_save(&mut a, fam);
        let steps = a.ed.undo_len();
        fe::answer_overwrite_for_test(&mut a.ed, &a.sh.cat, false);
        resume_drafts(&mut a);
        assert!(!a.project_ui.close_now, "{lang:?}: Back never closes");
        assert!(a.project_ui.after_drafts.is_none() && a.project_ui.prompt.is_none());
        assert!(a.project_ui.job.is_none(), "{lang:?}: no save ran");
        let c = &a.sh.cat;
        assert_eq!(a.ed.status.as_deref(), Some(c.t("drafts_overwrite.drafts.ow_back").as_str()));
        assert_eq!(drafts::count(&a.ed), 1, "{lang:?}: the edits stay a draft");
        assert!((fe::type_w_for_test(&a.ed.model, fam) - 1.0).abs() < 1e-9, "{lang:?}");
        assert_eq!(a.ed.undo_len(), steps, "{lang:?}: the model is as it was");
        assert_eq!(std::fs::read(&file).unwrap(), before, "{lang:?}: the file is untouched");
        // the next close asks again
        assert!(close_requested(&mut a));
        assert_eq!(a.project_ui.prompt, Some(Pending::Close));
        done(&d);
    }
}

#[test]
fn a_question_gone_unanswered_counts_as_back() {
    for lang in LANGS {
        let (mut a, d, _file, fam) = setup("gone", lang);
        close_and_save(&mut a, fam);
        fe::with(|s| s.ow = None);
        resume_drafts(&mut a);
        assert!(!a.project_ui.close_now && a.project_ui.after_drafts.is_none(), "{lang:?}");
        assert_eq!(drafts::count(&a.ed), 1, "{lang:?}");
        assert!((fe::type_w_for_test(&a.ed.model, fam) - 1.0).abs() < 1e-9);
        done(&d);
    }
}

#[test]
fn a_refused_family_after_replace_writes_nothing_and_stays_open() {
    for lang in LANGS {
        let (mut a, d, _file, fam) = setup("refused", lang);
        fe::seed_broken_for_test();
        close_and_save(&mut a, fam);
        let steps = a.ed.undo_len();
        fe::answer_overwrite_for_test(&mut a.ed, &a.sh.cat, true);
        resume_drafts(&mut a);
        assert!(!a.project_ui.close_now, "{lang:?}");
        assert!(a.project_ui.after_drafts.is_none() && a.project_ui.job.is_none());
        let st = a.ed.status.clone().unwrap_or_default();
        assert!(st.contains("Broken") && !st.contains("family_load."), "{lang:?}: says which: {st}");
        assert_eq!(drafts::count(&a.ed), 2, "{lang:?}: both drafts kept");
        assert!((fe::type_w_for_test(&a.ed.model, fam) - 1.0).abs() < 1e-9, "{lang:?}");
        assert_eq!(a.ed.undo_len(), steps, "{lang:?}: all or none");
        done(&d);
    }
}

/// The waiting close stays, the edits stay, and the refusal says why.
fn still_waits(a: &EditorApp, fam: i64, lang: Lang) {
    assert_eq!(a.project_ui.after_drafts, Some(Pending::Close), "{lang:?}: the close waits");
    assert!(a.project_ui.prompt.is_none() && a.project_ui.job.is_none(), "{lang:?}");
    assert!(!a.project_ui.close_now, "{lang:?}");
    let c = &a.sh.cat;
    assert_eq!(a.ed.status.as_deref(), Some(c.t("drafts_overwrite.drafts.ow_wait").as_str()));
    assert_eq!(drafts::count(&a.ed), 1, "{lang:?}: the draft stays");
    assert!((fe::type_w_for_test(&a.ed.model, fam) - 1.0).abs() < 1e-9, "{lang:?}");
}

/// Replace after a refused action: the close still goes on, with everything in the file.
fn replace_closes(a: &mut EditorApp, lang: Lang) {
    fe::answer_overwrite_for_test(&mut a.ed, &a.sh.cat, true);
    resume_drafts(a);
    wait(a);
    assert!(a.project_ui.close_now, "{lang:?}: the close goes on after the save");
    assert!(!unsaved(a).any(), "{lang:?}: {:?}", unsaved(a));
}

#[test]
fn new_open_and_sample_while_waiting_are_refused_and_the_close_goes_on() {
    for lang in LANGS {
        let (mut a, d, file, fam) = setup("refnew", lang);
        let before = std::fs::read(&file).unwrap();
        close_and_save(&mut a, fam);
        for p in [Pending::New, Pending::Open(file.clone()), Pending::Sample] {
            a.ed.status = None;
            assert!(!guard(&mut a, p.clone()), "{lang:?}: {p:?} is refused");
            still_waits(&a, fam, lang);
        }
        a.ed.status = None;
        request_new(&mut a);
        still_waits(&a, fam, lang);
        request_open(&mut a, file.clone());
        still_waits(&a, fam, lang);
        assert_eq!(std::fs::read(&file).unwrap(), before, "{lang:?}: nothing saved unasked");
        replace_closes(&mut a, lang);
        done(&d);
    }
}

#[test]
fn restore_while_waiting_gives_the_dialog_back_and_keeps_the_close() {
    for lang in LANGS {
        let (mut a, d, file, fam) = setup("refrst", lang);
        let before = std::fs::read(&file).unwrap();
        let auto = d.join("auto");
        let snap = a
            .ed
            .model
            .autosave_as(&auto.join(crate::shell::recover_win::UNTITLED), 3)
            .unwrap();
        crate::shell::recover_win::mark_undecided(&auto, None).unwrap();
        let r = crate::shell::recover_win::scan_for(&auto, true, None).unwrap();
        close_and_save(&mut a, fam);
        a.ed.status = None;
        request_restore(&mut a, r, snap);
        still_waits(&a, fam, lang);
        assert!(a.project_ui.parked.is_none(), "{lang:?}: nothing held for a refused restore");
        assert!(a.recover.is_some(), "{lang:?}: the restore choice comes back");
        assert_eq!(std::fs::read(&file).unwrap(), before, "{lang:?}: nothing saved unasked");
        replace_closes(&mut a, lang);
        done(&d);
    }
}

#[test]
fn a_build_finishing_while_waiting_keeps_the_close() {
    for lang in LANGS {
        let (mut a, d, _file, fam) = setup("refblt", lang);
        close_and_save(&mut a, fam);
        a.ed.status = None;
        assert!(!guard(&mut a, Pending::AdoptBuilt), "{lang:?}");
        still_waits(&a, fam, lang);
        assert!(a.project_ui.built.is_none(), "{lang:?}");
        replace_closes(&mut a, lang);
        done(&d);
    }
}

/// Windows refuses to replace a read-only file: the save after Replace fails.
#[cfg(windows)]
#[test]
#[allow(clippy::permissions_set_readonly_false)]
fn replace_then_a_failed_write_keeps_the_work_and_stays_open() {
    for lang in LANGS {
        let (mut a, d, file, fam) = setup("rofail", lang);
        let before = std::fs::read(&file).unwrap();
        let mut perm = std::fs::metadata(&file).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&file, perm.clone()).unwrap();
        close_and_save(&mut a, fam);
        fe::answer_overwrite_for_test(&mut a.ed, &a.sh.cat, true);
        resume_drafts(&mut a);
        wait(&mut a);
        assert!(!a.project_ui.close_now, "{lang:?}: a failed save never closes");
        assert!(matches!(a.project_ui.notice, Some(Notice::SaveFailed(_))), "{lang:?}");
        assert!(a.project_ui.after_save.is_none() && a.project_ui.after_drafts.is_none());
        assert_eq!(std::fs::read(&file).unwrap(), before, "{lang:?}: the old file is intact");
        assert!(unsaved(&a).model, "{lang:?}: the loaded family still waits for a save");
        assert!((fe::type_w_for_test(&a.ed.model, fam) - 2.0).abs() < 1e-9, "{lang:?}");
        perm.set_readonly(false);
        let _ = std::fs::set_permissions(&file, perm);
        done(&d);
    }
}
