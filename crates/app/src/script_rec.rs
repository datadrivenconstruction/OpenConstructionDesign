//! Recording to a script (`mgr.record`): the first call starts, the next one stops and saves the
//! edits made meanwhile as one script in the scripts folder. Only edits that came as envelopes
//! (scripts, tables, proposals, envelope tools) have a script form; the others are counted and
//! named in the status. An edit undone before the stop is left out. Rows made by an earlier
//! recorded edit are named by slot in the script, so a run makes its own rows again. The result is
//! a toast; after a save it offers "Scripts" to open the window and run the script.

use std::cell::RefCell;
use std::sync::Arc;

use ddc_ops::{Actor, Envelope, Group, Ref};
use serde_json::{json, Value};

use crate::shell::EditorApp;
use ddc_ui::notify::Level;

/// One recorded envelope: the undo entry that applied it and the rows it made.
struct Taken {
    entry: u64,
    env: Arc<Envelope>,
    created: Vec<(u32, Ref)>,
}

struct Rec {
    /// Undo entries up to this one were made before the start.
    from: u64,
    taken: Vec<Taken>,
}

thread_local! {
    static REC: RefCell<Option<Rec>> = const { RefCell::new(None) };
}

pub(crate) fn recording() -> bool {
    REC.with(|r| r.borrow().is_some())
}

/// Called by `Editor::run_ops` for every envelope it applied as undo entry `entry`.
pub(crate) fn note(entry: u64, env: &Arc<Envelope>, created: &[(u32, Ref)]) {
    REC.with(|r| {
        if let Some(rec) = r.borrow_mut().as_mut() {
            rec.taken.push(Taken { entry, env: env.clone(), created: created.to_vec() });
        }
    });
}

/// Every `slot` number shifted by `off`; a ref to a row in `made` (cat, id, slot) becomes that
/// slot. Refs are tagged objects (`{"by": "id", ...}`), slots plain numbers.
fn remap(v: &mut Value, off: u32, made: &[(Value, i64, u32)]) {
    match v {
        Value::Object(o) => {
            if o.get("by").and_then(Value::as_str) == Some("id") {
                let hit = made.iter().find(|m| o.get("cat") == Some(&m.0) && o.get("id").and_then(Value::as_i64) == Some(m.1));
                if let Some(m) = hit {
                    *v = json!({"by": "new", "slot": m.2});
                }
                return;
            }
            if let Some(n) = o.get("slot").and_then(Value::as_u64) {
                o.insert("slot".into(), json!(n + u64::from(off)));
            }
            for (k, x) in o.iter_mut() {
                if k != "slot" {
                    remap(x, off, made);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| remap(x, off, made)),
        _ => {}
    }
}

/// The recorded envelopes, in order, as one script by `name`: slots renumbered past the earlier
/// envelopes, refs to their rows turned into slots, each group after the one before it.
fn merge(name: &str, parts: &[(&Envelope, &[(u32, Ref)])]) -> Result<Envelope, String> {
    let mut out = Envelope::new(Actor::Script { name: name.into() }, name);
    let mut made: Vec<(Value, i64, u32)> = vec![];
    let mut off = 0u32;
    for (env, created) in parts {
        let mut v = serde_json::to_value(&env.groups).map_err(|e| e.to_string())?;
        remap(&mut v, off, &made);
        let groups: Vec<Group> = serde_json::from_value(v).map_err(|e| e.to_string())?;
        let mut next = off;
        for mut g in groups {
            for s in g.ops.iter().filter_map(|o| o.slot()) {
                next = next.max(s + 1);
            }
            let id = out.groups.len() as u32 + 1;
            g.id = id;
            g.depends_on = if id > 1 { vec![id - 1] } else { vec![] };
            out.groups.push(g);
        }
        for (s, r) in created.iter() {
            if let Ref::Id { cat, id } = r {
                made.push((serde_json::to_value(cat).map_err(|e| e.to_string())?, *id, off + s));
            }
        }
        off = next;
    }
    if let Some(i) = out.check().first() {
        return Err(format!("{i:?}"));
    }
    Ok(out)
}

/// Stops the recording: the script saved in `dir` and the status line, or why nothing was saved.
fn finish(a: &mut EditorApp, rec: Rec, dir: Option<std::path::PathBuf>) -> (Level, String) {
    let c = &a.sh.cat;
    let (_, live) = a.ed.undo_entries();
    let after: Vec<u64> = live.iter().copied().filter(|e| *e > rec.from).collect();
    let kept: Vec<&Taken> = rec.taken.iter().filter(|t| after.contains(&t.entry)).collect();
    let skipped = after.len() - kept.len();
    let tail = if skipped > 0 { format!(" {}", c.tf("script_rec.skipped", &[("k", &skipped.to_string())])) } else { String::new() };
    if kept.is_empty() {
        return (Level::Warning, format!("{}{tail}", c.t("script_rec.nothing")));
    }
    let Some(dir) = dir else {
        return (Level::Error, c.tf("script_rec.failed", &[("why", &c.t("script_rec.no_folder"))]));
    };
    let stem = c.t("cmd.mgr.record.short");
    let saved = (|| {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = crate::scripts_win::free_path(&dir, &stem);
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let parts: Vec<(&Envelope, &[(u32, Ref)])> = kept.iter().map(|t| (&*t.env, t.created.as_slice())).collect();
        let env = merge(&name, &parts)?;
        std::fs::write(&path, env.to_json_pretty().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        Ok::<_, String>((path, name))
    })();
    match saved {
        Ok((path, name)) => {
            crate::scripts_win::show(path);
            (Level::Success, format!("{}{tail}", c.tf("script_rec.saved", &[("name", &name), ("n", &kept.len().to_string())])))
        }
        Err(why) => (Level::Error, c.tf("script_rec.failed", &[("why", &why)])),
    }
}

fn h_record(a: &mut EditorApp) {
    let rec = REC.with(|r| r.borrow_mut().take());
    let (level, line) = match rec {
        None => {
            let (from, _) = a.ed.undo_entries();
            REC.with(|r| *r.borrow_mut() = Some(Rec { from, taken: vec![] }));
            (Level::Info, a.sh.cat.t("script_rec.started"))
        }
        Some(rec) => finish(a, rec, crate::scripts_win::folder()),
    };
    a.ed.status = Some(line.clone());
    if level == Level::Success {
        let action = a.sh.cat.t("cmd.mgr.scripts");
        a.sh.toasts.push_cmd(level, line, action, "mgr.scripts");
    } else {
        a.sh.toasts.push(level, line, None);
    }
}

/// Registration in the shell (`shell::REGISTER`).
pub fn register(app: &mut EditorApp) {
    app.register_command("mgr.record", h_record);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use ddc_geom::v2;

    fn app() -> EditorApp {
        crate::editor::ops_run::set_thread_exec(Some(Arc::new(crate::ops_exec::ModelExec)));
        EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
    }

    fn wall(a: &mut EditorApp, x: f64) {
        let env = a.ed.wall_envelope(&[(v2(x, 0.0), v2(x + 4.0, 0.0))]).unwrap();
        a.ed.run_ops(env).unwrap();
    }

    #[test]
    fn records_envelope_edits_and_runs_them_again() {
        let dir = std::env::temp_dir().join(format!("ddc_rec1_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut a = app();
        wall(&mut a, -10.0);
        h_record(&mut a);
        assert!(recording());
        wall(&mut a, 0.0);
        wall(&mut a, 10.0);
        wall(&mut a, 20.0);
        a.ed.undo();
        let rec = REC.with(|r| r.borrow_mut().take()).unwrap();
        let (level, line) = finish(&mut a, rec, Some(dir.clone()));
        assert_eq!(level, Level::Success);
        assert!(!recording());
        assert!(line.contains('2'), "{line}");
        let files = crate::scripts_win::list(&dir);
        assert_eq!(files.len(), 1, "{line}");
        let env = files[0].read.clone().unwrap();
        assert_eq!(env.groups.len(), 2, "the undone wall is left out");
        let before = a.ed.walls.len();
        a.ed.run_ops(env).unwrap();
        assert_eq!(a.ed.walls.len(), before + 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refs_to_rows_made_while_recording_become_slots() {
        let mut a = app();
        let first = a.ed.wall_envelope(&[(v2(0.0, 0.0), v2(4.0, 0.0))]).unwrap();
        let r1 = a.ed.run_ops(first.clone()).unwrap();
        let (_, made) = r1.created[0].clone();
        let mut del = Envelope::new(Actor::User, "delete");
        del.groups.push(Group {
            id: 1,
            title: "delete".into(),
            ops: vec![ddc_ops::Op::Delete { target: ddc_ops::Targets::Refs { refs: vec![made.clone()] } }],
            depends_on: vec![],
            why: String::new(),
        });
        let r2 = a.ed.run_ops(del.clone()).unwrap();
        let env = merge("s", &[(&first, &r1.created), (&del, &r2.created)]).unwrap();
        let ops = &env.groups.last().unwrap().ops;
        let slot = r1.created[0].0;
        assert_eq!(ops[0], ddc_ops::Op::Delete { target: ddc_ops::Targets::Refs { refs: vec![Ref::slot(slot)] } });
        // run again: a wall of its own is made and deleted, the other walls stay
        let n = a.ed.walls.len();
        a.ed.run_ops(env).unwrap();
        assert_eq!(a.ed.walls.len(), n);
        // a ref to a row made before the recording stays as it is
        let mut v = json!({"slot": 0, "target": {"by": "id", "cat": "wall", "id": 8}});
        remap(&mut v, 3, &[(json!("wall"), 7, 1)]);
        assert_eq!(v, json!({"slot": 3, "target": {"by": "id", "cat": "wall", "id": 8}}));
    }

    #[test]
    fn stopping_without_envelope_edits_saves_nothing() {
        let mut a = app();
        h_record(&mut a);
        let rec = REC.with(|r| r.borrow_mut().take()).unwrap();
        let dir = std::env::temp_dir().join(format!("ddc_rec1_none_{}", std::process::id()));
        let (level, line) = finish(&mut a, rec, Some(dir.clone()));
        assert_eq!(level, Level::Warning);
        assert_eq!(line, a.sh.cat.t("script_rec.nothing"));
        assert!(!dir.exists());
    }

    fn layout_app() -> EditorApp {
        crate::editor::ops_run::set_thread_exec(Some(Arc::new(crate::ops_exec::ModelExec)));
        REC.with(|r| *r.borrow_mut() = None);
        crate::layout_contract::app_in(ddc_ui::Lang::En)
    }

    #[test]
    fn two_presses_save_the_edits_between_them_as_a_script() {
        let d = std::env::temp_dir().join(format!("ddc_rec1_press_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut a = layout_app();
        crate::scripts_win::open_for_layout(Some(d.clone()), None);
        assert!(a.run("mgr.record"));
        assert!(recording());
        wall(&mut a, 0.0);
        assert!(a.run("mgr.record"));
        assert!(!recording());
        let files = crate::scripts_win::list(&d);
        assert_eq!(files.len(), 1);
        let env = files[0].read.clone().expect("the recording is a valid script");
        assert_eq!(env.groups.len(), 1);
        assert_eq!(env.groups[0].id, 1);
        let mut b = layout_app();
        let walls = b.ed.walls.len();
        b.ed.run_ops(env).unwrap();
        assert_eq!(b.ed.walls.len(), walls + 1);
        crate::scripts_win::open_for_layout(None, None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_recording_without_edits_saves_nothing_and_says_so() {
        let d = std::env::temp_dir().join(format!("ddc_rec1_empty_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut a = layout_app();
        crate::scripts_win::open_for_layout(Some(d.clone()), None);
        assert!(a.run("mgr.record"));
        assert!(a.run("mgr.record"));
        assert!(!recording());
        assert!(crate::scripts_win::list(&d).is_empty());
        assert_eq!(a.ed.status.as_deref(), Some(a.sh.cat.t("script_rec.nothing").as_str()));
        crate::scripts_win::open_for_layout(None, None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
