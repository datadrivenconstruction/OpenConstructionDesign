//! A failed project save: the cause in plain words, Save as and Retry, the system text under
//! Details. The edits stay in memory and the project stays unsaved; the file on disk is only
//! ever replaced whole (temporary file, then rename), so a failure leaves it as it was.

use std::path::PathBuf;

use ddc_ui::theme::{layout, space};
use ddc_ui::Catalog;
use egui::{self, RichText};

use super::Notice;
use crate::io_kind::{kind_of_text, IoKind};
use crate::shell::EditorApp;
use crate::ui_kit as kit;

/// What the dialog's buttons do again.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Again {
    /// The project save, to this file when it was already known.
    Save(Option<PathBuf>),
    /// A copy for someone else: asked again from the start.
    Copy,
}

pub(super) struct SaveFail {
    pub kind: IoKind,
    pub raw: String,
    pub again: Again,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FailAct {
    SaveAs,
    Retry,
    Close,
}

/// Texts and offered actions for one cause.
#[derive(Clone, Debug, PartialEq)]
pub struct FailView {
    pub title: String,
    pub body: String,
    pub retry: bool,
}

/// Cause sentence with what to do; `None` for an unknown cause.
pub fn cause_key(kind: IoKind) -> Option<&'static str> {
    Some(match kind {
        IoKind::Full => "save_fail.cause.full",
        IoKind::Denied => "save_fail.cause.denied",
        IoKind::Locked => "save_fail.cause.locked",
        IoKind::Missing => "save_fail.cause.missing",
        IoKind::Offline => "save_fail.cause.offline",
        IoKind::TooLong => "save_fail.cause.too_long",
        IoKind::Other => return None,
    })
}

/// The cause of a failed write in the words of `c`, for any surface (autosave chip too).
pub fn cause_text(c: &Catalog, raw: &str) -> Option<String> {
    cause_key(kind_of_text(raw)).map(|k| c.t(k))
}

/// Dialog content for `kind`. Retrying a full disk or a too long path fails the same way, so
/// only Save as is offered there.
pub fn save_fail_view(c: &Catalog, kind: IoKind, raw: &str) -> FailView {
    let cause = match cause_key(kind) {
        Some(k) => c.t(k),
        None => {
            // the code stays under Details; the sentence carries the words only
            let words = raw.rfind(" (os error ").map_or(raw, |at| &raw[..at]);
            let detail = crate::ui_err::humanize(words, c.lang == ddc_ui::Lang::Ru);
            c.tf("save_fail.cause.other", &[("detail", detail.as_str())])
        }
    };
    FailView {
        title: c.t("save_fail.save_fail.title"),
        body: format!("{cause}\n\n{}", c.t("save_fail.save_fail.kept")),
        retry: !matches!(kind, IoKind::Full | IoKind::TooLong),
    }
}

/// Shows the failure. Whatever waited for the save (a close, an open) does not run.
pub(super) fn report(a: &mut EditorApp, raw: &str, again: Again) {
    report_as(a, kind_of_text(raw), raw, again);
}

/// As `report`, with the cause already read from a typed error.
pub(super) fn report_as(a: &mut EditorApp, kind: IoKind, raw: &str, again: Again) {
    super::forget_pending(a);
    a.project_ui.notice = Some(Notice::SaveFailed(SaveFail {
        kind,
        raw: raw.to_string(),
        again,
    }));
}

pub(super) fn window(a: &mut EditorApp, ctx: &egui::Context, f: SaveFail) {
    let view = save_fail_view(&a.sh.cat, f.kind, &f.raw);
    let retry = view.retry && f.again != Again::Copy;
    let mut pick = None;
    let (c, th) = (&a.sh.cat, &a.sh.theme);
    let m = kit::modal_dialog("save_fail.save_fail", ctx, |ui| {
        ui.set_width(layout::DIALOG_FORM);
        kit::modal_title(ui, view.title.as_str());
        ui.label(RichText::new(view.body.as_str()).color(th.c("text.secondary")));
        ui.add_space(space::SM);
        egui::CollapsingHeader::new(c.t("save_fail.save_fail.details"))
            .id_salt("save_fail.save_fail.details")
            .default_open(false)
            .show(ui, |ui| {
                let raw = RichText::new(f.raw.as_str()).monospace();
                ui.label(raw.color(th.c("text.secondary")));
                if kit::secondary_button(ui, c.t("save_fail.save_fail.copy")).clicked() {
                    ui.ctx().copy_text(f.raw.clone());
                }
            });
        kit::dialog_footer(ui, |ui| {
            if kit::primary_action(ui, c.t("save_fail.save_fail.save_as"), None) {
                pick = Some(FailAct::SaveAs);
            }
            if retry && kit::secondary_button(ui, c.t("save_fail.save_fail.retry")).clicked() {
                pick = Some(FailAct::Retry);
            }
            if kit::cancel_button(ui, c.t("save_fail.save_fail.close")).clicked() {
                pick = Some(FailAct::Close);
            }
        });
    });
    if pick.is_none() && m.should_close() {
        pick = Some(FailAct::Close);
    }
    match pick {
        None => a.project_ui.notice = Some(Notice::SaveFailed(f)),
        Some(act) => redo(a, act, f.again, |a: &mut EditorApp| super::pick_save_path(a)),
    }
}

/// Runs the chosen action; `pick` asks for the Save as file (a file dialog outside tests).
pub(super) fn redo(
    a: &mut EditorApp,
    act: FailAct,
    again: Again,
    pick: impl FnOnce(&mut EditorApp) -> Option<PathBuf>,
) {
    match (act, again) {
        (FailAct::Close, _) => {}
        (_, Again::Copy) => super::h_save_copy(a),
        (FailAct::Retry, Again::Save(t)) => {
            super::start_save(a, t);
        }
        (FailAct::SaveAs, Again::Save(_)) => {
            if let Some(p) = pick(a) {
                super::start_save(a, Some(p));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{handle_close, poll_job, unsaved, Pending, Unsaved};
    use super::*;
    use crate::editor::{Editor, OpFn};
    use ddc_model::Model;
    use ddc_ui::Lang;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    const KINDS: [IoKind; 7] = [
        IoKind::Full,
        IoKind::Denied,
        IoKind::Locked,
        IoKind::Missing,
        IoKind::Offline,
        IoKind::TooLong,
        IoKind::Other,
    ];

    fn app(tag: &str) -> (EditorApp, PathBuf) {
        let d = std::env::temp_dir().join(format!("ddc-rl5-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut a = EditorApp::new(Editor::start_project().unwrap(), None, "test".into());
        a.prefs.autosave_dir = d.join("auto").display().to_string();
        (a, d)
    }

    fn edit(a: &mut EditorApp) {
        let op = OpFn(Arc::new(|m: &mut Model| m.add_level("rl5", 3.0).map(|_| ())));
        a.ed.run_op(op).unwrap();
    }

    fn settle(a: &mut EditorApp) {
        for _ in 0..2000 {
            if !a.project_ui.busy() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
            poll_job(a);
        }
        panic!("save did not finish");
    }

    fn save(a: &mut EditorApp, to: &Path) {
        if super::super::start_save(a, Some(to.to_path_buf())) {
            settle(a);
        }
    }

    fn failure(a: &EditorApp) -> Option<(IoKind, Again)> {
        match &a.project_ui.notice {
            Some(Notice::SaveFailed(f)) => Some((f.kind, f.again.clone())),
            _ => None,
        }
    }

    fn has_level(a: &EditorApp) -> bool {
        a.ed.model.level_list().unwrap().iter().any(|l| l.1 == "rl5")
    }

    #[test]
    fn each_cause_has_its_own_text_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let raw = "raw (os error 99999)";
            let bodies: Vec<String> =
                KINDS.iter().map(|k| save_fail_view(&c, *k, raw).body).collect();
            for (i, b) in bodies.iter().enumerate() {
                assert!(!b.contains("save_fail.") && !b.contains("os error"), "{lang:?}: {b}");
                assert!(bodies.iter().skip(i + 1).all(|o| o != b), "{lang:?}: {b}");
            }
            for k in [
                "save_fail.save_fail.title",
                "save_fail.save_fail.kept",
                "save_fail.save_fail.details",
                "save_fail.save_fail.copy",
                "save_fail.save_fail.save_as",
                "save_fail.save_fail.retry",
                "save_fail.save_fail.close",
            ] {
                assert_ne!(c.t(k), k, "{k}");
            }
        }
        let (en, ru) = (Catalog::new(Lang::En), Catalog::new(Lang::Ru));
        for k in KINDS {
            assert_ne!(save_fail_view(&en, k, "x").body, save_fail_view(&ru, k, "x").body);
        }
    }

    #[test]
    fn full_disk_offers_no_retry_and_a_lock_does() {
        let c = Catalog::new(Lang::En);
        assert!(!save_fail_view(&c, IoKind::Full, "").retry);
        assert!(!save_fail_view(&c, IoKind::TooLong, "").retry);
        for k in [IoKind::Locked, IoKind::Offline, IoKind::Missing, IoKind::Denied, IoKind::Other] {
            assert!(save_fail_view(&c, k, "").retry, "{k:?}");
        }
    }

    #[test]
    fn autosave_reason_uses_the_same_words() {
        let c = Catalog::new(Lang::En);
        assert_eq!(cause_text(&c, "database or disk is full"), Some(c.t("save_fail.cause.full")));
        assert_eq!(cause_text(&c, "odd"), None);
    }

    #[test]
    fn failed_save_keeps_edits_and_retry_works_once_the_folder_is_back() {
        let (mut a, d) = app("retry");
        edit(&mut a);
        let to = d.join("gone").join("x.ddc");
        save(&mut a, &to);
        let (kind, again) = failure(&a).expect("the failure is shown");
        assert_eq!(kind, IoKind::Missing);
        assert_eq!(again, Again::Save(Some(to.clone())));
        assert!(!to.exists());
        assert!(has_level(&a) && a.ed.is_dirty());
        assert!(unsaved(&a).model);
        // the cause is cleared, the same button runs again
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        a.project_ui.notice = None;
        redo(&mut a, FailAct::Retry, again, |_: &mut EditorApp| None);
        settle(&mut a);
        assert!(failure(&a).is_none());
        assert!(to.exists());
        assert_eq!(a.ed.project.path(), Some(to.as_path()));
        assert!(!a.ed.is_dirty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn save_as_from_the_dialog_writes_the_new_file() {
        let (mut a, d) = app("saveas");
        edit(&mut a);
        let bad = d.join("gone").join("x.ddc");
        save(&mut a, &bad);
        let (_, again) = failure(&a).expect("the failure is shown");
        a.project_ui.notice = None;
        let other = d.join("other.ddc");
        let pick = other.clone();
        redo(&mut a, FailAct::SaveAs, again, move |_: &mut EditorApp| Some(pick));
        settle(&mut a);
        assert!(other.exists() && !bad.exists());
        assert_eq!(a.ed.project.path(), Some(other.as_path()));
        assert!(!a.ed.is_dirty() && has_level(&a));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn close_does_nothing_but_close_the_dialog() {
        let (mut a, d) = app("closebtn");
        edit(&mut a);
        save(&mut a, &d.join("gone").join("x.ddc"));
        let (_, again) = failure(&a).unwrap();
        a.project_ui.notice = None;
        redo(&mut a, FailAct::Close, again, |_: &mut EditorApp| panic!("no file dialog"));
        assert!(!a.project_ui.busy() && a.ed.is_dirty() && has_level(&a));
        let _ = std::fs::remove_dir_all(d);
    }

    /// Windows refuses to replace a read-only file; elsewhere the folder decides, not the file.
    #[cfg(windows)]
    #[test]
    #[allow(clippy::permissions_set_readonly_false)]
    fn failed_save_leaves_the_existing_file_byte_identical() {
        let (mut a, d) = app("intact");
        let to = d.join("x.ddc");
        save(&mut a, &to);
        assert!(failure(&a).is_none() && to.exists());
        let before = std::fs::read(&to).unwrap();
        edit(&mut a);
        let mut p = std::fs::metadata(&to).unwrap().permissions();
        p.set_readonly(true);
        std::fs::set_permissions(&to, p.clone()).unwrap();
        save(&mut a, &to);
        let (kind, _) = failure(&a).expect("the failure is shown");
        assert_eq!(kind, IoKind::Denied);
        assert_eq!(std::fs::read(&to).unwrap(), before, "the old file is untouched");
        assert!(a.ed.is_dirty() && has_level(&a));
        let left: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(left.is_empty(), "no temporary file is left: {left:?}");
        p.set_readonly(false);
        let _ = std::fs::set_permissions(&to, p);
        let _ = std::fs::remove_dir_all(d);
    }

    /// Runs one frame with a window-close request; true when the close was held back.
    fn close_held(a: &mut EditorApp) -> bool {
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
        let out = ctx.run(raw, |ctx| handle_close(a, ctx));
        out.viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|v| {
                v.commands
                    .iter()
                    .any(|c| matches!(c, egui::ViewportCommand::CancelClose))
            })
    }

    #[test]
    fn closing_with_a_failed_save_asks_again_and_loses_nothing() {
        let (mut a, d) = app("close");
        edit(&mut a);
        // the close waits for the save, the save fails
        a.project_ui.after_save = Some(Pending::Close);
        save(&mut a, &d.join("gone").join("x.ddc"));
        assert!(failure(&a).is_some(), "the failure is shown");
        assert!(!a.project_ui.close_now, "the window does not close");
        assert_eq!(a.project_ui.after_save, None);
        assert_eq!(unsaved(&a), Unsaved { model: true, ..Default::default() });
        // the next close asks about the unsaved work again
        assert!(close_held(&mut a));
        assert_eq!(a.project_ui.prompt, Some(Pending::Close));
        assert!(has_level(&a));
        let _ = std::fs::remove_dir_all(d);
    }
}
