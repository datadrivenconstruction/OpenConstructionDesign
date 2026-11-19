//! "Add to this project" without a UI stall: the worker imports into a copy of the model file
//! ([`stage`]); the UI thread attaches the copy and applies it, new rows and the rows the
//! import changed, as one transaction ([`Editor::add_staged`]), then reloads. A job that is
//! cancelled or fails only drops the copy; an Add after any edit of the model is refused.
use super::*;
use std::path::PathBuf;

/// The copy file goes away with its owner.
struct StageFile(PathBuf);

impl Drop for StageFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A finished import waiting in a copy of the model file.
pub struct Staged<R> {
    file: StageFile,
    marks: ddc_model::StageMarks,
    /// Change head of the model when the job started: any edit after it refuses the Add.
    since: u64,
    /// Level to show after the Add (the imported one with the most walls).
    pub level: Option<i64>,
    pub report: R,
}

/// Copies the model file at `model_path` and runs `work` on the copy (worker thread). `since`
/// is the model's [`Model::change_head`] taken on the UI thread before the job started.
pub fn stage<R>(
    model_path: &str,
    since: u64,
    work: impl FnOnce(&mut Model) -> Result<R, String>,
) -> Result<Staged<R>, String> {
    let out = ddc_model::stage_path(model_path);
    let file = StageFile(out.clone());
    let marks = ddc_model::stage_copy(model_path, &out).map_err(|e| e.to_string())?;
    let path = out.to_str().ok_or("non-UTF-8 path")?;
    let mut m = Model::open_copy(path).map_err(|e| e.to_string())?;
    m.stage_watch(&marks).map_err(|e| e.to_string())?;
    let report = work(&mut m)?;
    let level = imported_level(&m);
    m.stage_seal().map_err(|e| e.to_string())?;
    drop(m);
    Ok(Staged {
        file,
        marks,
        since,
        level,
        report,
    })
}

impl Editor {
    /// Applies a [`Staged`] import as one model operation (one undo step): the untouched
    /// default level goes in the same step. A refused or failed Add leaves the model and the
    /// shown level as they were. Redo needs a new import: the copy is used once.
    pub fn add_staged<R>(&mut self, st: Staged<R>, redo: String) -> Result<R, String> {
        let (report, stale) = self.add_staged_warn(st, redo)?;
        if stale.is_some() {
            self.status = stale;
        }
        Ok(report)
    }

    /// As [`Self::add_staged`], but a stale-view warning (the Add stands, only the refresh
    /// failed) is handed back instead of set, so a caller's own done text cannot overwrite it.
    pub(crate) fn add_staged_warn<R>(
        &mut self,
        st: Staged<R>,
        redo: String,
    ) -> Result<(R, Option<String>), String> {
        let Staged {
            file,
            marks,
            since,
            level,
            report,
        } = st;
        let old = self.level;
        let drop_level = level
            .filter(|l| *l != old)
            .and_then(|_| default_level(&self.model, old).then_some(old));
        let slot = std::sync::Mutex::new(Some((file, marks)));
        let op = OpFn(std::sync::Arc::new(move |m: &mut Model| {
            let (file, marks) = slot
                .lock()
                .ok()
                .and_then(|mut s| s.take())
                .ok_or_else(|| ddc_model::Error::Invalid(redo.clone()))?;
            m.stage_add(&file.0, &marks, since, drop_level)
        }));
        if let Some(l) = level {
            self.level = l;
        }
        let steps = self.undo.len();
        let r = self.run_op(op);
        // the copy cannot be detached inside the op's own transaction: release it now
        if let Err(e) = self.model.stage_release() {
            eprintln!("Add: copy not released yet: {e}");
        }
        match r {
            Ok(()) => Ok((report, None)),
            // applied and on the undo stack: only the view refresh failed, never a failed Add
            Err(e) if self.undo.len() > steps => {
                let e = e.to_string();
                let warn = crate::tr("edst.import_view_stale", &[("e", e.as_str())]);
                Ok((report, Some(warn)))
            }
            Err(e) => {
                // a refused Add put nothing on the stack: the old view stays
                if self.undo.len() == steps {
                    self.level = old;
                    let _ = self.reload();
                }
                Err(stage_text(crate::ui_lang_now(), &e))
            }
        }
    }
}

/// Reader-facing text of a refused Add: the typed reason in the UI language, any other error as is.
pub(crate) fn stage_text(ru: bool, e: &ddc_model::Error) -> String {
    match e {
        ddc_model::Error::Stage(s) => {
            let args = s.args();
            let args: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
            crate::tr_lang(ru, s.key(), &args)
        }
        _ => e.to_string(),
    }
}

/// Undo status: `done`, then one note per table an undone Add kept for rows written after it.
pub(crate) fn undo_status(done: String, kept: &[(String, i64)]) -> String {
    kept.iter().fold(done, |s, (t, n)| {
        let note = crate::tr("stage_err.kept", &[("t", t), ("n", &n.to_string())]);
        format!("{s} · {note}")
    })
}

/// Status after several undo steps: the last step's own notes give way to the notes of every
/// step (`all`), each said once.
pub(crate) fn undo_status_all(
    status: Option<String>,
    last: &[(String, i64)],
    all: &[(String, i64)],
) -> Option<String> {
    if all.is_empty() {
        return status;
    }
    let own = undo_status(String::new(), last);
    let s = status.unwrap_or_default();
    let base = s.strip_suffix(own.as_str()).unwrap_or(&s).to_string();
    let mut seen: Vec<&(String, i64)> = vec![];
    for k in all {
        if !seen.contains(&k) {
            seen.push(k);
        }
    }
    let once: Vec<(String, i64)> = seen.into_iter().cloned().collect();
    Some(undo_status(base, &once))
}

/// Test helper: an Add that creates table `t` (unique `v`) with rows 'a' and 'b'.
#[cfg(test)]
pub(crate) fn add_created_table(e: &mut Editor, path: &str, t: &'static str) {
    let st = stage(path, e.model.change_head(), move |m| {
        m.sql()
            .execute_batch(&format!(
                "CREATE TABLE {t}(id INTEGER PRIMARY KEY, v TEXT NOT NULL UNIQUE);
                 INSERT INTO {t}(v) VALUES ('a'), ('b');"
            ))
            .map_err(|e| e.to_string())
    })
    .unwrap();
    e.add_staged(st, "redo".into()).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on_disk(tag: &str) -> (Editor, String) {
        let p = std::env::temp_dir().join(format!("ddc-ad1-{}-{tag}.ddc", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let path = p.to_string_lossy().into_owned();
        (Editor::seeded(Model::open(&path).unwrap()).unwrap(), path)
    }

    /// Row counts and highest rowids of every plain table except the journal and settings.
    fn table_hash(m: &Model) -> u64 {
        use std::hash::{Hash, Hasher};
        let db = m.sql();
        let names: Vec<String> = db
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' \
                 AND name NOT IN ('op','meta') AND sql NOT LIKE 'CREATE VIRTUAL%' \
                 AND sql NOT LIKE '%WITHOUT ROWID%' ORDER BY name",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|x| x.unwrap())
            .collect();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for t in names {
            let rows: i64 = db
                .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
                .unwrap();
            let max: i64 = db
                .query_row(
                    &format!("SELECT COALESCE(MAX(rowid),0) FROM \"{t}\""),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            (t, rows, max).hash(&mut h);
        }
        h.finish()
    }

    /// A staged import that brings its own level (marked as imported) with a wall.
    fn bring(m: &mut Model) -> Result<(), String> {
        let wt: i64 = m
            .sql()
            .query_row("SELECT MIN(id) FROM wall_type", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let l = m.add_level("Brought", 3.0).map_err(|e| e.to_string())?;
        m.sql()
            .execute(
                "INSERT INTO ext_ref(source, guid, kind, ref) VALUES ('x', 'L-ad1', 'level', ?1)",
                [l],
            )
            .map_err(|e| e.to_string())?;
        m.add_walls(&[ddc_model::WallSpec {
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        }])
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[test]
    fn undo_status_names_kept_table_in_both_languages() {
        use ddc_ui::i18n::{Catalog, Lang};
        let c = Catalog::new(Lang::En);
        for lang in [Lang::En, Lang::Ru] {
            let s = c.t_in(lang, "stage_err.kept");
            assert!(s.contains("{t}") && s.contains("{n}"), "{lang:?}: {s}");
        }
        let s = undo_status("done".into(), &[("ad5_extra".into(), 2)]);
        assert!(s.starts_with("done · ") && s.contains("ad5_extra") && s.contains('2'), "{s}");
        assert_eq!(undo_status("done".into(), &[]), "done");
    }

    #[test]
    fn add_one_undo_restores_table_hash() {
        let (mut e, path) = on_disk("undo");
        let before = table_hash(&e.model);
        let st = stage(&path, e.model.change_head(), bring).unwrap();
        let n = e.undo.len();
        e.add_staged(st, "redo".into()).unwrap();
        assert_eq!(e.undo.len(), n + 1, "one Add, one undo step");
        assert!(!e.walls.is_empty());
        assert_ne!(table_hash(&e.model), before);
        e.undo();
        assert_eq!(table_hash(&e.model), before);
    }

    #[test]
    fn add_cancel_leaves_model_unchanged() {
        let (mut e, path) = on_disk("cancel");
        let before = table_hash(&e.model);
        let n = e.undo.len();
        let level = e.level;
        // a cancelled or failed worker: the copy is dropped, nothing reaches the model
        assert!(stage::<()>(&path, e.model.change_head(), |_| Err("cancelled".into())).is_err());
        assert_eq!(table_hash(&e.model), before);
        // an Add refused because the model got rows meanwhile changes nothing either
        let st = stage(&path, e.model.change_head(), bring).unwrap();
        e.model.add_level("Meanwhile", 9.0).unwrap();
        let after_edit = table_hash(&e.model);
        assert!(e.add_staged(st, "redo".into()).is_err());
        assert_eq!(table_hash(&e.model), after_edit);
        assert_eq!(e.level, level);
        assert_eq!(e.undo.len(), n);
        assert_ne!(before, after_edit);
    }

    #[test]
    fn add_tidies_untouched_default_level() {
        let (mut e, path) = on_disk("tidy");
        let old = e.level;
        let st = stage(&path, e.model.change_head(), bring).unwrap();
        e.add_staged(st, "redo".into()).unwrap();
        let has = |e: &Editor, id: i64| e.model.level_list().unwrap().iter().any(|l| l.0 == id);
        assert!(!has(&e, old), "empty default level goes with the Add");
        assert_ne!(e.level, old);
        e.undo();
        assert!(has(&e, old), "one undo brings it back");
    }

    /// Every row of the tables an Add of `bring` touches, rowid order.
    fn state(m: &Model) -> Vec<Vec<String>> {
        ["level", "wall", "ext_ref"]
            .iter()
            .map(|t| {
                let mut st = m
                    .sql()
                    .prepare(&format!("SELECT * FROM {t} ORDER BY rowid"))
                    .unwrap();
                let n = st.column_count();
                let v: Vec<String> = st
                    .query_map([], |r| {
                        (0..n)
                            .map(|i| {
                                r.get::<_, rusqlite::types::Value>(i)
                                    .map(|v| format!("{v:?}"))
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .unwrap()
                    .map(|x| x.unwrap().join("|"))
                    .collect();
                v
            })
            .collect()
    }

    /// Re-Add of the same source: the import moves its level, rewrites its ext_ref row with
    /// OR REPLACE (the old row goes through the unique key, no delete trigger) and swaps its wall.
    fn bring_again(m: &mut Model) -> Result<(), String> {
        let e = |x: rusqlite::Error| x.to_string();
        let (l, wt): (i64, i64) = m
            .sql()
            .query_row(
                "SELECT ref, (SELECT MIN(id) FROM wall_type) FROM ext_ref WHERE guid='L-ad1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(e)?;
        let db = m.sql();
        db.execute("UPDATE level SET elevation=7.5 WHERE id=?1", [l])
            .map_err(e)?;
        db.execute(
            "INSERT OR REPLACE INTO ext_ref(source, guid, kind, ref) VALUES ('x', 'L-ad1', 'level', ?1)",
            [l],
        )
        .map_err(e)?;
        db.execute("DELETE FROM wall WHERE level=?1", [l]).map_err(e)?;
        m.add_walls(&[ddc_model::WallSpec {
            a: (0.0, 0.0),
            b: (0.0, 6.0),
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        }])
        .map_err(|x| x.to_string())?;
        Ok(())
    }

    #[test]
    fn readd_same_file_complete_or_refused() {
        let (mut e, path) = on_disk("readd");
        let st = stage(&path, e.model.change_head(), bring).unwrap();
        e.add_staged(st, "redo".into()).unwrap();
        let first = state(&e.model);
        let lid: i64 = e
            .model
            .sql()
            .query_row("SELECT ref FROM ext_ref WHERE guid='L-ad1'", [], |r| r.get(0))
            .unwrap();
        let walls = |e: &Editor| -> Vec<i64> {
            let mut st = e.model.sql().prepare("SELECT id FROM wall WHERE level=?1").unwrap();
            let v: Vec<i64> = st
                .query_map([lid], |r| r.get(0))
                .unwrap()
                .map(|x| x.unwrap())
                .collect();
            v
        };
        let w0 = walls(&e);
        assert_eq!(w0.len(), 1);
        let n = e.undo.len();
        let st = stage(&path, e.model.change_head(), bring_again).unwrap();
        e.add_staged(st, "redo".into()).unwrap();
        assert_eq!(e.undo.len(), n + 1, "re-Add is one undo step");
        let (elev, refs): (f64, i64) = e
            .model
            .sql()
            .query_row(
                "SELECT (SELECT elevation FROM level WHERE id=?1), \
                 (SELECT count(*) FROM ext_ref WHERE guid='L-ad1')",
                [lid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(elev, 7.5, "the change to an existing level came along");
        assert_eq!(refs, 1, "the replaced ext_ref row did not stay beside the new one");
        let w1 = walls(&e);
        assert_eq!(w1.len(), 1, "the old wall went, the new one came");
        // the new wall may reuse the freed id: tell them apart by geometry
        let by: f64 = e
            .model
            .sql()
            .query_row("SELECT by FROM wall WHERE id=?1", [w1[0]], |r| r.get(0))
            .unwrap();
        assert_eq!(by, 6.0, "the wall is the one the re-Add brought");
        e.undo();
        assert_eq!(state(&e.model), first, "one undo restores the first Add exactly");
    }

    #[test]
    fn add_concurrent_edit_refused_model_unchanged() {
        let (mut e, path) = on_disk("edit");
        let st = stage(&path, e.model.change_head(), bring).unwrap();
        // an edit that adds no row: the row marks stay, only the change journal can tell
        let marks = e.model.stage_marks().unwrap();
        e.model
            .sql()
            .execute(
                "UPDATE level SET elevation=elevation+1 WHERE id=?1",
                [e.level],
            )
            .unwrap();
        assert_eq!(e.model.stage_marks().unwrap(), marks);
        let edited = state(&e.model);
        let (n, level) = (e.undo.len(), e.level);
        let err = e.add_staged(st, "redo".into()).unwrap_err();
        let want = crate::tr_lang(crate::ui_lang_now(), "stage_err.stage.model_changed", &[]);
        assert_eq!(err, want);
        assert_eq!(state(&e.model), edited);
        assert_eq!(e.undo.len(), n);
        assert_eq!(e.level, level);
    }

    #[test]
    fn staged_add_refusal_text_localized_ru() {
        let (mut e, path) = on_disk("refru");
        let st = stage(&path, e.model.change_head(), bring).unwrap();
        e.model
            .sql()
            .execute("UPDATE level SET elevation=elevation+1 WHERE id=?1", [e.level])
            .unwrap();
        let err = e
            .model
            .stage_add(&st.file.0, &st.marks, st.since, None)
            .unwrap_err();
        assert!(matches!(
            err,
            ddc_model::Error::Stage(ddc_model::StageErr::ModelChanged)
        ));
        let ru = stage_text(true, &err);
        assert!(!ru.is_ascii(), "{ru}");
        assert_eq!(ru, crate::tr_lang(true, "stage_err.stage.model_changed", &[]));
        assert!(!ru.contains("Add refused"), "{ru}");
        let en = stage_text(false, &err);
        assert!(en.contains("model changed"), "{en}");
        let t = ddc_model::Error::Stage(ddc_model::StageErr::TableGone("wall".into()));
        assert!(stage_text(true, &t).contains("wall"));
    }

    #[test]
    fn view_meta_write_does_not_block_add() {
        let (mut e, path) = on_disk("viewmeta");
        let st = stage(&path, e.model.change_head(), bring).unwrap();
        e.model
            .sql()
            .execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES ('view:zoom', 3)",
                [],
            )
            .unwrap();
        e.model
            .sql()
            .execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES ('phase_current', 1)",
                [],
            )
            .unwrap();
        e.add_staged(st, "redo".into()).unwrap();
        let n: i64 = e
            .model
            .sql()
            .query_row("SELECT count(*) FROM ext_ref WHERE guid='L-ad1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the Add went through");
    }

    #[test]
    fn user_edit_still_blocks_add() {
        // a meta key that is not view state is user data
        let (mut e, path) = on_disk("usermeta");
        let st = stage(&path, e.model.change_head(), bring).unwrap();
        e.model
            .sql()
            .execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES ('note_count', 1)",
                [],
            )
            .unwrap();
        let before = state(&e.model);
        let err = e.add_staged(st, "redo".into()).unwrap_err();
        let want = crate::tr_lang(crate::ui_lang_now(), "stage_err.stage.model_changed", &[]);
        assert_eq!(err, want);
        assert_eq!(state(&e.model), before);
        // and a table edit blocks it as before
        let st = stage(&path, e.model.change_head(), bring).unwrap();
        e.model
            .sql()
            .execute("UPDATE level SET elevation=elevation+1 WHERE id=?1", [e.level])
            .unwrap();
        assert!(e.add_staged(st, "redo".into()).is_err());
    }
}
