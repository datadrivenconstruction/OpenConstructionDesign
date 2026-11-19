//! Undo button of the XLSX Apply toast: it takes back that Apply and nothing else.
//! A plain `edit.undo` would take back whatever edit came later while the toast was up.
use crate::editor::Editor;
use crate::shell::EditorApp;

/// Command of the toast button (registered in `xlsx_win::register`).
pub const CMD: &str = "xlsx.undo_apply";

/// The undo step one Apply wrote: stack depth and push count right after it. No push since
/// means the stack below and at this depth is the same, also after an undo and a redo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    len: usize,
    pushes: u64,
}

/// Why the toast button left the model alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Undo,
    /// Another edit was made after the Apply.
    Later,
    /// The Apply is already undone (Ctrl+Z, History) or wrote nothing.
    Gone,
    /// A drag or drawing is in progress: Ctrl+Z would cancel it first.
    Busy,
}

impl Verdict {
    /// i18n key of the refusal; `None` for `Undo`.
    pub fn key(self) -> Option<&'static str> {
        match self {
            Verdict::Undo => None,
            Verdict::Later => Some("xlsx_undo.later"),
            Verdict::Gone => Some("xlsx_undo.gone"),
            Verdict::Busy => Some("xlsx_undo.busy"),
        }
    }
}

impl Mark {
    /// Snapshot taken before an Apply.
    pub fn of(ed: &Editor) -> Mark {
        Mark { len: ed.undo_len(), pushes: ed.pushes }
    }

    /// The step the Apply wrote since `before`; `None` when it wrote none (0 operations, or
    /// the written step was taken back by the check).
    pub fn written(ed: &Editor, before: Mark) -> Option<Mark> {
        let now = Mark::of(ed);
        (now.pushes > before.pushes && now.len > before.len && top_is_xlsx(ed)).then_some(now)
    }
}

/// The top undo step is an XLSX Apply (bulk edits or the linked XLSX tool record).
fn top_is_xlsx(ed: &Editor) -> bool {
    matches!(ed.history().0.last().map(String::as_str), Some("hist.bulk" | "tool.xlsx"))
}

/// What the toast button may do now with the Apply marked `m`.
pub fn verdict(ed: &Editor, m: Option<Mark>) -> Verdict {
    let Some(m) = m else {
        return Verdict::Gone;
    };
    if ed.pushes != m.pushes {
        return Verdict::Later;
    }
    if ed.undo_len() < m.len {
        return Verdict::Gone;
    }
    if ed.undo_len() != m.len || !top_is_xlsx(ed) {
        return Verdict::Later;
    }
    if !ed.top_owns_ops() {
        return Verdict::Busy;
    }
    Verdict::Undo
}

/// Takes back the marked Apply only while it is the top undo step; otherwise the model stays.
pub fn undo(ed: &mut Editor, m: Option<Mark>) -> Verdict {
    let v = verdict(ed, m);
    if v != Verdict::Undo {
        return v;
    }
    let len = ed.undo_len();
    ed.undo();
    // a draft (solid, text, topo point) was cancelled instead of the step
    if ed.undo_len() + 1 != len {
        return Verdict::Busy;
    }
    Verdict::Undo
}

/// Handler of [`CMD`]: a refusal is said in the status line and in a toast.
pub fn run(a: &mut EditorApp) {
    if let Some(key) = undo(&mut a.ed, a.xlsx_undo).key() {
        let t = crate::tr(key, &[]);
        a.ed.say(crate::status::Severity::Refusal, t.clone());
        a.sh.toasts.push(ddc_ui::notify::Level::Warning, t, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::xlsx_win::{apply_preview, demo_book, done_toast, Applied, XlsxPreview};
    use crate::shell::{xlsx_job, xlsx_no_base};
    use crate::xl_text::Lang;
    use ddc_io::xlsx;

    fn editor() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.level;
        let t = ed
            .model
            .add_wall_type("ext", &[ddc_model::LayerSpec { material: "brick".into(), thickness: 0.25 }])
            .unwrap();
        let p = [(0., 0.), (6., 0.), (6., 4.), (0., 4.)];
        for k in 0..4 {
            ed.model.add_wall(&wall(p[k], p[(k + 1) % 4], lv, t)).unwrap();
        }
        ed.reload().unwrap();
        ed
    }

    fn wall(a: (f64, f64), b: (f64, f64), level: i64, wall_type: i64) -> ddc_model::WallSpec {
        ddc_model::WallSpec { a, b, level, base_offset: 0.0, height: 3.0, wall_type, offset0: None }
    }

    /// Applies the demo book (one new wall row); returns the mark the toast would carry.
    fn apply(ed: &mut Editor, tag: &str) -> Option<Mark> {
        let p = std::env::temp_dir().join(format!("ddc_app_xs50_{tag}_{}.xlsx", std::process::id()));
        demo_book(&ed.model, &p).unwrap();
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.rev = ed.content_snap();
        xlsx_no_base::confirm(&mut pv);
        let before = Mark::of(ed);
        let r = apply_preview(ed, &mut pv, xlsx::Units::Metric, Lang::En).unwrap();
        assert!(matches!(r, Applied::Done { .. }));
        let _ = std::fs::remove_file(&p);
        Mark::written(ed, before)
    }

    /// Another user edit: one wall more, its own undo step.
    fn edit_wall(ed: &mut Editor) {
        let (lv, t) = (ed.level, ed.walls[0].wall_type);
        let op = crate::editor::OpFn(std::sync::Arc::new(move |m: &mut ddc_model::Model| {
            m.add_wall(&wall((20., 0.), (26., 0.), lv, t)).map(|_| ())
        }));
        ed.run_op(op).unwrap();
    }

    fn hash(ed: &Editor) -> u64 {
        ed.model.content_hash().unwrap()
    }

    #[test]
    fn right_after_apply_takes_back_that_apply() {
        let mut ed = editor();
        let (n0, steps) = (ed.walls.len(), ed.undo_len());
        let m = apply(&mut ed, "now");
        assert!(m.is_some());
        assert_eq!(ed.walls.len(), n0 + 1);
        assert_eq!(undo(&mut ed, m), Verdict::Undo);
        assert_eq!((ed.walls.len(), ed.undo_len()), (n0, steps));
    }

    #[test]
    fn after_another_edit_is_refused_and_both_stay() {
        let mut ed = editor();
        let n0 = ed.walls.len();
        let m = apply(&mut ed, "later");
        edit_wall(&mut ed);
        let (h, len) = (hash(&ed), ed.undo_len());
        assert_eq!(ed.walls.len(), n0 + 2);
        assert_eq!(undo(&mut ed, m), Verdict::Later);
        assert_eq!((hash(&ed), ed.undo_len(), ed.walls.len()), (h, len, n0 + 2));
        // the later edit undone by hand: the Apply is on top again, but a new push came after it
        ed.undo();
        let h = hash(&ed);
        assert_eq!(undo(&mut ed, m), Verdict::Later);
        assert_eq!(hash(&ed), h);
    }

    #[test]
    fn after_manual_undo_does_nothing() {
        let mut ed = editor();
        let m = apply(&mut ed, "gone");
        ed.undo();
        let (h, len, redo) = (hash(&ed), ed.undo_len(), ed.redo_len());
        assert_eq!(undo(&mut ed, m), Verdict::Gone);
        assert_eq!((hash(&ed), ed.undo_len(), ed.redo_len()), (h, len, redo));
        // a second click after the toast's own undo is refused the same way
        ed.redo();
        assert_eq!(undo(&mut ed, m), Verdict::Undo);
        let h = hash(&ed);
        assert_eq!(undo(&mut ed, m), Verdict::Gone);
        assert_eq!(hash(&ed), h);
    }

    #[test]
    fn after_redo_takes_back_the_redone_apply() {
        let mut ed = editor();
        let (n0, steps) = (ed.walls.len(), ed.undo_len());
        let m = apply(&mut ed, "redo");
        ed.undo();
        ed.redo();
        assert_eq!(ed.walls.len(), n0 + 1);
        assert_eq!(undo(&mut ed, m), Verdict::Undo);
        assert_eq!((ed.walls.len(), ed.undo_len()), (n0, steps));
        assert_eq!(ed.redo_len(), 1, "the Apply stays redoable");
    }

    #[test]
    fn apply_of_nothing_offers_no_undo() {
        let mut ed = editor();
        let p = std::env::temp_dir().join(format!("ddc_app_xs50_zero_{}.xlsx", std::process::id()));
        let out = xlsx_job::export_sync(&ed.model, &p, xlsx::Units::Metric, "en");
        assert!(matches!(out, xlsx_job::Outcome::Done));
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.rev = ed.content_snap();
        xlsx_no_base::confirm(&mut pv);
        let (before, h) = (Mark::of(&ed), hash(&ed));
        let _ = apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En).unwrap();
        let _ = std::fs::remove_file(&p);
        let m = Mark::written(&ed, before);
        assert_eq!(m, None);
        assert_eq!(hash(&ed), h);
        let mut toasts = ddc_ui::notify::Toasts::default();
        done_toast(&mut toasts, Lang::En, &ed.status, m.is_some());
        let t = toasts.live.last().unwrap();
        assert!(t.action.is_none() && t.command.is_none());
        assert_eq!(verdict(&ed, m), Verdict::Gone);
    }

    #[test]
    fn toast_carries_the_marked_command_and_replaces_an_older_one() {
        let mut toasts = ddc_ui::notify::Toasts::default();
        done_toast(&mut toasts, Lang::En, &Some("a".into()), true);
        done_toast(&mut toasts, Lang::En, &Some("b".into()), true);
        let undo: Vec<_> = toasts.live.iter().filter(|t| t.command.as_deref() == Some(CMD)).collect();
        assert_eq!(undo.len(), 1, "an older Apply's button would act on the newer Apply");
        assert!(undo[0].text.starts_with('b'));
    }

    #[test]
    fn refusal_texts_exist_in_both_languages() {
        for v in [Verdict::Later, Verdict::Gone, Verdict::Busy] {
            let k = v.key().unwrap();
            let (en, ru) = (crate::tr_lang(false, k, &[]), crate::tr_lang(true, k, &[]));
            assert!(!en.is_empty() && en != k && en != ru, "{k}");
        }
        assert_eq!(Verdict::Undo.key(), None);
    }
}
