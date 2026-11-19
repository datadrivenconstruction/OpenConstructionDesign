//! The user's Apply of an AI design option: the recipe's envelope goes through the model
//! executor inside `Editor::run_op`, so the option and its accepted mark are one undo step
//! (undo returns the option to draft). The run itself is `ops_exec::apply_option`; this maps
//! its outcome to the status text of the Options tab. An unsaved project's recipe lives in the
//! session store: the Apply step itself writes it into the model table (same base head and
//! stamp), inside its transaction, so a refused Apply leaves no row.

use std::path::Path;

use ddc_model::{OptionApplyError, OptionStatus, OptionStore};

use crate::shell::EditorApp;

/// Outcome of a committed Apply: the status text; `warn` when the change is saved but the view
/// did not reload (shown as a warning, not a success).
#[derive(Debug, PartialEq)]
pub struct Applied {
    pub text: String,
    pub warn: bool,
}

/// Applies option `id` from where the Options tab lists it; `Ok` is the status, `Err` the
/// reason nothing changed.
pub fn apply(app: &mut EditorApp, id: i64) -> Result<Applied, String> {
    let session = super::options_keep::live_session_dir();
    let src = super::options_keep::ui_store(&app.ed.model, session.as_deref());
    apply_from(app, id, src.as_ref().map(|s| s.1.as_path()))
}

/// `store`: the session recipe database `id` belongs to (`None`: the model's own table).
pub fn apply_from(app: &mut EditorApp, id: i64, store: Option<&Path>) -> Result<Applied, String> {
    let Some(db) = store.filter(|p| p.is_file()) else {
        return apply_model(app, id, id);
    };
    let rec = OptionStore::open(db)
        .and_then(|s| s.get(id))
        .map_err(|e| reason(id, &OptionApplyError::Model(e.to_string())))?
        .ok_or_else(|| reason(id, &OptionApplyError::Missing))?;
    if rec.status != OptionStatus::Draft {
        return Err(reason(id, &OptionApplyError::NotDraft(rec.status)));
    }
    if let Some(t) = super::options_keep::model_twin(&app.ed.model, &rec) {
        return apply_model(app, t.id, id);
    }
    // nothing is copied ahead: the row is written by the Apply step, after every gate
    let name = rec.name.clone();
    run(app, &name, id, |ed| crate::ops_exec::apply_option_rec(ed, rec))
}

/// Applies row `id` of the model table; refusals name the option as `shown`.
fn apply_model(app: &mut EditorApp, id: i64, shown: i64) -> Result<Applied, String> {
    let name = match app.ed.model.ai_option_get(id) {
        Ok(Some(r)) => r.name,
        Ok(None) => return Err(reason(shown, &OptionApplyError::Missing)),
        Err(e) => return Err(reason(shown, &OptionApplyError::Model(e.to_string()))),
    };
    run(app, &name, shown, |ed| crate::ops_exec::apply_option(ed, id))
}

fn run(
    app: &mut EditorApp,
    name: &str,
    shown: i64,
    f: impl FnOnce(&mut crate::editor::Editor) -> Result<(), OptionApplyError>,
) -> Result<Applied, String> {
    let ed = &mut app.ed;
    let top = ed.model.op_top().map_err(|e| crate::tr("ai.optx.failed", &[("e", &e.to_string())]))?;
    let pushes = ed.pushes;
    let res = f(ed);
    // committed with its undo step even though the run reported an error: only the reload failed
    let committed = committed(ed.model.op_top(), top, ed.pushes != pushes);
    outcome(name, shown, committed, res)
}

/// Whether the run wrote its step. A failed journal read after the run falls back to the undo
/// step `run_op` pushes only for a written step, so a commit is never shown as a refusal.
pub(crate) fn committed(read: Result<i64, ddc_model::Error>, top: i64, pushed: bool) -> bool {
    match read {
        Ok(now) => now != top,
        Err(_) => pushed,
    }
}

fn outcome(name: &str, shown: i64, committed: bool, res: Result<(), OptionApplyError>) -> Result<Applied, String> {
    match res {
        Ok(()) => Ok(Applied { text: crate::tr("ai.optx.applied", &[("name", name)]), warn: false }),
        Err(e) if committed => Ok(Applied {
            text: crate::tr("ai.optx.reload_failed", &[("name", name), ("e", &format!("{e:?}"))]),
            warn: true,
        }),
        Err(e) => Err(reason(shown, &e)),
    }
}

fn reason(id: i64, e: &OptionApplyError) -> String {
    let id = id.to_string();
    match e {
        OptionApplyError::Missing => crate::tr("ai.optx.missing", &[("id", &id)]),
        OptionApplyError::NotDraft(OptionStatus::Accepted) => {
            crate::tr("ai.optx.accepted", &[("id", &id)])
        }
        OptionApplyError::NotDraft(_) => crate::tr("ai.optx.not_draft", &[("id", &id)]),
        OptionApplyError::Stale => crate::tr("ai.optx.stale", &[("id", &id)]),
        OptionApplyError::Envelope(e) => crate::tr("ai.optx.bad_recipe", &[("e", e)]),
        OptionApplyError::Rejected(r) => crate::tr(
            "ai.optx.refused",
            &[("code", r.code.key()), ("reason", &r.to_string())],
        ),
        OptionApplyError::Model(e) => crate::tr("ai.optx.failed", &[("e", e)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ui::compare::OptCache;
    use crate::editor::Editor;
    use ddc_model::{OptionRec, RecipeStore};
    use std::path::PathBuf;

    const LEVEL_ENV: &str = r#"{"v":1,"actor":{"kind":"user"},"title":"opt","why":null,
        "groups":[{"id":1,"title":"g","depends_on":[],"ops":[{"op":"level","action":"create",
        "slot":1,"name":"optx-L9","elevation":{"v":9.0,"unit":"m"}}]}]}"#;

    // an op kind the executor does not lower: rejected as `unsupported`
    const BAD_ENV: &str = r#"{"v":1,"actor":{"kind":"user"},"title":"opt","why":null,
        "groups":[{"id":1,"title":"g","depends_on":[],"ops":[{"op":"type","action":"duplicate",
        "slot":1,"from":{"by":"name","cat":"type","name":"none"},"name":"x","fields":[]}]}]}"#;

    fn app() -> EditorApp {
        EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
    }

    fn option(app: &EditorApp, envelope: &str) -> i64 {
        app.ed.model.ai_option_add(&recipe(app, envelope)).unwrap()
    }

    fn recipe(app: &EditorApp, envelope: &str) -> OptionRec {
        let (base_head, base_stamp) = app.ed.model.ai_option_head().unwrap();
        OptionRec {
            id: 0,
            set: "s".into(),
            name: "A".into(),
            base_head,
            base_stamp,
            envelope: envelope.into(),
            seed: 0,
            params: "{}".into(),
            generator: Some("test".into()),
            metrics: None,
            note: None,
            why: Some("option why".into()),
            status: OptionStatus::Draft,
        }
    }

    /// Tests run in parallel and two share a tag: each call gets its own folder.
    fn unique() -> String {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("{}-{n}", std::process::id())
    }

    /// A recipe of an unsaved project: only in a session `recipes.db`, not in the model.
    fn store_option(app: &EditorApp, tag: &str) -> (PathBuf, i64) {
        store_rec(tag, &recipe(app, LEVEL_ENV))
    }

    fn store_rec(tag: &str, rec: &OptionRec) -> (PathBuf, i64) {
        let dir = std::env::temp_dir().join(format!("ddc-optx-{tag}-{}", unique()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("recipes.db");
        let id = OptionStore::open(&db).unwrap().add_recipe(rec).unwrap();
        (db, id)
    }

    /// A project saved to a file, so a refusal can be checked byte for byte.
    fn file_app(tag: &str) -> (EditorApp, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ddc-optx-f-{tag}-{}", unique()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.ddc");
        let m = ddc_model::Model::open(path.to_str().unwrap()).unwrap();
        (EditorApp::new(Editor::seeded(m).unwrap(), None, "test".into()), path)
    }

    /// Everything a refused Apply must leave as it was: rows, heads, undo, content, file bytes.
    #[derive(Debug, PartialEq)]
    struct Snap {
        rows: usize,
        head: u64,
        top: i64,
        undo: usize,
        hash: u64,
        bytes: Vec<Vec<u8>>,
    }

    fn snap(app: &EditorApp, file: &Path) -> Snap {
        let side = |ext: &str| {
            let mut p = file.as_os_str().to_owned();
            p.push(ext);
            std::fs::read(PathBuf::from(p)).unwrap_or_default()
        };
        Snap {
            rows: model_rows(app),
            head: app.ed.model.change_head(),
            top: app.ed.model.op_top().unwrap(),
            undo: app.ed.undo_len(),
            hash: app.ed.model.content_hash().unwrap(),
            bytes: vec![std::fs::read(file).unwrap(), side("-wal")],
        }
    }

    fn filter_env() -> String {
        use ddc_ops::{Actor, Envelope, Field, Group, Op, Targets, Unit, Value};
        let mut env = Envelope::new(Actor::User, "option");
        env.groups.push(Group {
            id: 1,
            title: "g".into(),
            ops: vec![Op::Set {
                target: Targets::Filter { cat: None, query: "walls".into() },
                fields: vec![Field::new("height", Value::num(5.0, Unit::M))],
            }],
            depends_on: vec![],
            why: String::new(),
        });
        env.to_json().unwrap()
    }

    /// Each refusal of a session recipe: the reason, and the model, undo and file untouched.
    #[test]
    fn unsaved_refusals_leave_model_undo_and_file_identical() {
        let keys = ["ai.optx.stale", "ai.optx.not_draft", "ai.optx.bad_recipe", "ai.optx.refused"];
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            for k in keys {
                assert_ne!(cat.t(k), k, "{lang:?}: {k} has no text");
            }
        }
        let cases: [(&str, &str, &str); 5] = [
            ("stale", LEVEL_ENV, "ai.optx.stale"),
            ("rejected", "", "ai.optx.not_draft"),
            ("envelope", "{not json", "ai.optx.bad_recipe"),
            ("lowering", BAD_ENV, "not_found"),
            ("frozen", "", "unsupported"),
        ];
        for (tag, env, want) in cases {
            let (mut a, file) = file_app(tag);
            let env = match tag {
                "frozen" => filter_env(),
                "rejected" => LEVEL_ENV.to_string(),
                _ => env.to_string(),
            };
            let (db, id) = store_rec(tag, &recipe(&a, &env));
            if tag == "stale" {
                a.ed.model.add_level("optx-other", 20.0).unwrap();
            }
            if tag == "rejected" {
                OptionStore::open(&db).unwrap().reject_recipe(id).unwrap();
            }
            let before = snap(&a, &file);
            let err = apply_from(&mut a, id, Some(&db)).unwrap_err();
            let shown = id.to_string();
            match want {
                "ai.optx.stale" | "ai.optx.not_draft" => {
                    assert_eq!(err, crate::tr(want, &[("id", &shown)]), "{tag}")
                }
                "ai.optx.bad_recipe" => {
                    let head = crate::tr(want, &[("e", "")]);
                    assert!(err.starts_with(head.trim_end()), "{tag}: {err}")
                }
                code => assert!(err.contains(code), "{tag}: {err}"),
            }
            assert_eq!(snap(&a, &file), before, "{tag}: a refused Apply changed something");
            assert_eq!(levels(&a), 0, "{tag}");
            let _ = std::fs::remove_dir_all(db.parent().unwrap());
            drop(a);
            let _ = std::fs::remove_dir_all(file.parent().unwrap());
        }
    }

    /// The old clean-up deleted the copied row after a refusal and ignored a failed delete; now
    /// no row is written before the step, so a model that cannot delete keeps no row either.
    #[test]
    fn refused_unsaved_apply_needs_no_clean_up() {
        let mut a = app();
        a.ed.model
            .sql()
            .execute_batch(
                "CREATE TEMP TRIGGER optx_no_del BEFORE DELETE ON ai_option \
                 BEGIN SELECT RAISE(ABORT, 'delete refused'); END;",
            )
            .unwrap();
        let (db, id) = store_rec("nodel", &recipe(&a, BAD_ENV));
        let (head, top) = (a.ed.model.change_head(), a.ed.model.op_top().unwrap());
        let err = apply_from(&mut a, id, Some(&db)).unwrap_err();
        assert!(err.contains("not_found"), "{err}");
        assert_eq!(model_rows(&a), 0, "no row left behind");
        assert_eq!((a.ed.model.change_head(), a.ed.model.op_top().unwrap()), (head, top));
        assert!(!a.ed.can_undo());
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// Undo and redo of an Apply that wrote the row: redo reuses that row, never adds one.
    #[test]
    fn unsaved_apply_redo_reuses_its_row() {
        let mut a = app();
        let (db, id) = store_option(&a, "redo");
        apply_from(&mut a, id, Some(&db)).unwrap();
        let mid = a.ed.model.ai_option_list(None).unwrap()[0].id;
        a.ed.undo();
        assert_eq!((levels(&a), model_rows(&a)), (0, 1));
        assert_eq!(status(&a, mid), OptionStatus::Draft);
        a.ed.redo();
        assert_eq!((levels(&a), model_rows(&a)), (1, 1));
        assert_eq!(status(&a, mid), OptionStatus::Accepted);
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    fn model_rows(app: &EditorApp) -> usize {
        app.ed.model.ai_option_list(None).unwrap().len()
    }

    fn levels(app: &EditorApp) -> i64 {
        app.ed
            .model
            .sql()
            .query_row("SELECT count(*) FROM level WHERE name='optx-L9'", [], |r| r.get(0))
            .unwrap()
    }

    fn status(app: &EditorApp, id: i64) -> OptionStatus {
        app.ed.model.ai_option_get(id).unwrap().unwrap().status
    }

    #[test]
    fn apply_option_one_undo_step() {
        let mut a = app();
        let id = option(&a, LEVEL_ENV);
        let top = a.ed.model.op_top().unwrap();
        apply(&mut a, id).unwrap();
        assert_eq!(levels(&a), 1);
        assert_eq!(a.ed.model.op_top().unwrap(), top + 1, "one journal record");
        assert_eq!(status(&a, id), OptionStatus::Accepted);
        a.ed.undo();
        assert_eq!(levels(&a), 0);
        assert_eq!(a.ed.model.op_top().unwrap(), top, "one undo removes all of it");
        assert!(!a.ed.can_undo());
    }

    #[test]
    fn stale_option_refused_with_reason() {
        let mut a = app();
        let id = option(&a, LEVEL_ENV);
        a.ed.model.add_level("optx-other", 20.0).unwrap();
        let top = a.ed.model.op_top().unwrap();
        let undo_before = a.ed.can_undo();
        let err = apply(&mut a, id).unwrap_err();
        assert_eq!(err, crate::tr("ai.optx.stale", &[("id", &id.to_string())]));
        assert_eq!(levels(&a), 0);
        assert_eq!(a.ed.model.op_top().unwrap(), top);
        assert_eq!(a.ed.can_undo(), undo_before, "no undo step for a refused Apply");
        assert_eq!(status(&a, id), OptionStatus::Draft);
    }

    #[test]
    fn reload_failed_apply_is_warning_and_refusal_keeps_reason() {
        let ok = outcome("A", 1, true, Err(OptionApplyError::Model("boom".into()))).unwrap();
        assert!(ok.warn);
        assert!(ok.text.contains('A') && ok.text.contains("boom"), "{}", ok.text);
        assert_ne!(ok.text, crate::tr("ai.optx.applied", &[("name", "A")]));
        let done = outcome("A", 1, true, Ok(())).unwrap();
        assert!(!done.warn);
        // nothing committed: refused with the reason, not a status
        let refused = outcome("A", 7, false, Err(OptionApplyError::Stale)).unwrap_err();
        assert_eq!(refused, crate::tr("ai.optx.stale", &[("id", "7")]));
    }

    #[test]
    fn rejection_code_in_message() {
        let mut a = app();
        let id = option(&a, BAD_ENV);
        let top = a.ed.model.op_top().unwrap();
        let err = apply(&mut a, id).unwrap_err();
        // type.duplicate runs since 287f0f26: the missing source type is the rejection code now
        assert!(err.contains("not_found"), "{err}");
        assert_eq!(a.ed.model.op_top().unwrap(), top, "nothing written");
        assert!(!a.ed.can_undo());
        assert_eq!(status(&a, id), OptionStatus::Draft);
    }

    #[test]
    fn apply_then_undo_option_draft_again() {
        let mut a = app();
        let id = option(&a, LEVEL_ENV);
        apply(&mut a, id).unwrap();
        let again = apply(&mut a, id).unwrap_err();
        assert_eq!(again, crate::tr("ai.optx.accepted", &[("id", &id.to_string())]));
        assert_eq!(levels(&a), 1, "a second Apply adds nothing");
        a.ed.undo();
        assert_eq!(status(&a, id), OptionStatus::Draft);
        apply(&mut a, id).unwrap();
        assert_eq!(levels(&a), 1);
        assert_eq!(status(&a, id), OptionStatus::Accepted);
    }

    #[test]
    fn unsaved_recipe_listed_and_appliable() {
        let mut a = app();
        let (db, id) = store_option(&a, "list");
        let mut c = OptCache::default();
        assert!(c.sync_from(&a.ed.model, Some(&db)));
        assert_eq!(c.rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![id]);
        assert!(!c.sync_from(&a.ed.model, Some(&db)));
        let top = a.ed.model.op_top().unwrap();
        apply_from(&mut a, id, Some(&db)).unwrap();
        assert_eq!(levels(&a), 1);
        assert_eq!(a.ed.model.op_top().unwrap(), top + 1, "one journal record");
        assert_eq!(model_rows(&a), 1, "the recipe was copied into the model");
        assert!(c.sync_from(&a.ed.model, Some(&db)));
        assert!(c.accepted.contains(&id));
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn unsaved_project_option_applied_one_undo() {
        let mut a = app();
        let (db, id) = store_option(&a, "undo");
        let top = a.ed.model.op_top().unwrap();
        apply_from(&mut a, id, Some(&db)).unwrap();
        a.ed.undo();
        assert_eq!(levels(&a), 0);
        assert_eq!(a.ed.model.op_top().unwrap(), top, "one undo removes all of it");
        assert!(!a.ed.can_undo());
        // undo returns the copied row to draft: a second Apply reuses it
        apply_from(&mut a, id, Some(&db)).unwrap();
        assert_eq!((levels(&a), model_rows(&a)), (1, 1));
        let err = apply_from(&mut a, id, Some(&db)).unwrap_err();
        assert_eq!(err, crate::tr("ai.optx.accepted", &[("id", &id.to_string())]));
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// A journal read failing after the commit settles as applied, not refreshed (warning),
    /// never as a refusal; without a pushed step it stays a refusal. The second Apply is refused.
    #[test]
    fn op_top_error_after_commit_is_not_a_refusal() {
        let bad = || Err(ddc_model::Error::Invalid("disk read".into()));
        assert!(committed(bad(), 4, true), "the undo step was pushed: committed");
        assert!(!committed(bad(), 4, false), "no step: nothing committed");
        assert!(committed(Ok(5), 4, false) && !committed(Ok(4), 4, true), "a good read decides");
        let reload = Err(OptionApplyError::Model("reload".into()));
        let shown = outcome("A", 1, committed(bad(), 4, true), reload).unwrap();
        assert!(shown.warn, "applied, view not refreshed: a warning");
        let e = format!("{:?}", OptionApplyError::Model("reload".into()));
        let want = crate::tr("ai.optx.reload_failed", &[("name", "A"), ("e", &e)]);
        assert_eq!(shown.text, want);
        let mut a = app();
        let id = option(&a, LEVEL_ENV);
        let pushes = a.ed.pushes;
        apply_from(&mut a, id, None).unwrap();
        assert_eq!(a.ed.pushes, pushes + 1, "the fallback signal: one step pushed per commit");
        let err = apply_from(&mut a, id, None).unwrap_err();
        assert_eq!(err, crate::tr("ai.optx.accepted", &[("id", &id.to_string())]));
        assert_eq!(a.ed.pushes, pushes + 1, "a refused second Apply pushes nothing");
    }

    #[test]
    fn unsaved_stale_refused_leaves_no_copy() {
        let mut a = app();
        // its own folder: the refusal table test stores a "stale" recipe in parallel
        let (db, id) = store_option(&a, "stale-nocopy");
        a.ed.model.add_level("optx-other", 20.0).unwrap();
        let err = apply_from(&mut a, id, Some(&db)).unwrap_err();
        assert_eq!(err, crate::tr("ai.optx.stale", &[("id", &id.to_string())]));
        assert_eq!((levels(&a), model_rows(&a)), (0, 0));
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }
}
