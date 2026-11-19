//! Two project files from one base, edited differently: the editor merges the colleague's file
//! (conflict choice, one undo step, redo) and compares versions. The merge window runs this
//! same sequence (`merge_lineage`, `prepare_merge`, `merge_preview`, `merge_apply` inside
//! `Editor::run_op`); only its native file picker is out of reach of a headless run.

mod e2e_common;

use std::path::Path;
use std::sync::{Arc, Mutex};

use ddc_app::collab::WinProbe;
use ddc_app::editor::{Editor, OpFn};
use ddc_model::merge::{prepare_merge, MergeCase, MergeError, MergeReport};
use ddc_model::source::Pick;
use ddc_model::{
    prepare_compare, DiffKind, LayerSpec, Model, VersionDiff, WallSpec,
};
use e2e_common as ec;

struct Base {
    level: i64,
    thin: i64,
    thick: i64,
    walls: [i64; 3],
}

fn wall(y: f64, level: i64, t: i64) -> WallSpec {
    WallSpec {
        a: (0.0, y),
        b: (5.0, y),
        level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: t,
        offset0: None,
    }
}

fn open(p: &Path) -> Model {
    Model::open(p.to_str().unwrap()).unwrap()
}

fn bx(m: &Model, id: i64) -> Option<f64> {
    m.sql()
        .query_row("SELECT bx FROM wall WHERE id=?1", [id], |r| r.get(0))
        .ok()
}

/// A saved project with three walls far apart, and a copy of the file for the colleague.
fn two_files() -> (Model, Model, Base, std::path::PathBuf, std::path::PathBuf) {
    let mine_path = ec::temp_file("mine.ddc");
    let theirs_path = ec::temp_file("theirs.ddc");
    let mut mine = open(&mine_path);
    let level = mine.add_level("L1", 0.0).unwrap();
    let layer = |t: f64| {
        vec![LayerSpec {
            material: "brick".into(),
            thickness: t,
        }]
    };
    let thin = mine.add_wall_type("thin", &layer(0.1)).unwrap();
    let thick = mine.add_wall_type("thick", &layer(0.3)).unwrap();
    let walls = [0.0, 20.0, 40.0].map(|y| mine.add_wall(&wall(y, level, thin)).unwrap());
    mine.snapshot_to(&theirs_path).unwrap();
    let theirs = open(&theirs_path);
    let base = Base {
        level,
        thin,
        thick,
        walls,
    };
    (mine, theirs, base, mine_path, theirs_path)
}

fn find<'a>(v: &'a VersionDiff, id: i64, kind: DiffKind) -> Option<&'a ddc_model::ElementDiff> {
    v.elements
        .iter()
        .find(|e| e.table == "wall" && e.id == id && e.kind == kind)
}

#[test]
fn merge_colleague_file_one_undo_step_and_compare() {
    let (mut mine, mut theirs, b, _mine_path, theirs_path) = two_files();
    let (w0, w1, w2) = (b.walls[0], b.walls[1], b.walls[2]);
    // both sides move the same wall end (a conflict); each side also has its own work
    mine.set_wall_axis(w0, (0.0, 0.0), (6.0, 0.0)).unwrap();
    mine.set_wall_type(w1, b.thick).unwrap();
    theirs.set_wall_axis(w0, (0.0, 0.0), (7.0, 0.0)).unwrap();
    // add first, then delete: a freed max id would be reused and pair as an edit
    let added = theirs.add_wall(&wall(60.0, b.level, b.thin)).unwrap();
    theirs.delete_wall(w2).unwrap();
    assert!(b.walls.iter().all(|&w| w != added), "the added id differs from every old id");
    drop(theirs);

    let mut ed = Editor::new_in_memory().unwrap();
    ed.replace_model(mine).unwrap();
    assert!(!ed.can_undo(), "a replaced model starts without history");

    // compare before merging: every kind of difference is listed with its place
    let work = ec::temp_file("cmp");
    let copy = prepare_compare(&theirs_path, &work).unwrap();
    let v = ed.model.compare_version(&copy).unwrap();
    let both = find(&v, w0, DiffKind::Differs).expect("conflicting wall differs");
    assert!(both.fields.iter().any(|f| f == "bx"), "{:?}", both.fields);
    assert!(find(&v, w1, DiffKind::Differs).is_some(), "own type change differs");
    assert!(find(&v, w2, DiffKind::HereOnly).is_some(), "deleted there, alive here");
    let there = find(&v, added, DiffKind::ThereOnly).expect("their new wall");
    assert_eq!(there.spot.unwrap().min, [0.0, 60.0]);
    assert!(v.total >= 4);
    assert!(!ed.can_undo(), "comparing is not an edit");

    // merge: preview writes nothing and lists one conflict
    let before = ed.model.content_hash().unwrap();
    let before_rows = ec::dump_except(&ed.model, &["op", "clash_item"]);
    let line = ed.model.merge_lineage().unwrap();
    let p = prepare_merge(&line, &theirs_path, &ec::temp_file("work")).unwrap();
    let plan = ed.model.merge_preview(&p).unwrap();
    assert_eq!(ed.model.content_hash().unwrap(), before, "preview writes nothing");
    assert_eq!(plan.conflicts.len(), 1, "{:?}", plan.conflicts);
    let c = &plan.conflicts[0];
    assert_eq!((c.table.as_str(), c.id, c.case), ("wall", w0, MergeCase::EditEdit));
    assert!(c.fields.iter().any(|f| f.field == "bx"));
    assert_eq!((plan.added, plan.deleted), (1, 1));

    // apply through the editor exactly as the merge window does, taking the colleague's side
    let out: Arc<Mutex<Option<MergeReport>>> = Arc::default();
    let slot = out.clone();
    let picks = vec![("wall".to_string(), w0, Pick::Theirs)];
    let (p2, plan2) = (p.clone(), plan.clone());
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        let r = m.merge_apply(&p2, &plan2, &picks)?;
        *slot.lock().unwrap() = Some(r);
        Ok(())
    })))
    .unwrap();
    let rep = out.lock().unwrap().take().unwrap();
    assert_eq!(rep.took_theirs, 1);
    assert_eq!(rep.kept_mine, 0);
    assert_eq!(bx(&ed.model, w0), Some(7.0), "colleague's side wins the conflict");
    assert_eq!(bx(&ed.model, w2), None, "their deletion arrives");
    let n_walls: i64 = ec::count(&ed.model, "wall");
    assert_eq!(n_walls, 3, "two old walls and their new one");
    ec::assert_clean(&ed.model, "after merge");
    let after = ec::dump_except(&ed.model, &["op", "clash_item"]);
    let after_hash = ed.model.content_hash().unwrap();
    assert_ne!(after_hash, before);

    // compare again: the resolved wall and the deletion no longer differ, own work still does
    let v2 = ed.model.compare_version(&copy).unwrap();
    assert!(find(&v2, w0, DiffKind::Differs).is_none(), "{:?}", v2.elements);
    assert!(find(&v2, w2, DiffKind::HereOnly).is_none());
    assert!(find(&v2, w1, DiffKind::Differs).is_some(), "own type change stays");

    // one undo step restores the exact model, redo reproduces the merge
    assert!(ed.can_undo());
    ed.undo();
    assert!(!ed.can_undo(), "the merge is one undo step");
    let back = ec::dump_except(&ed.model, &["op", "clash_item"]);
    assert_eq!(ec::hash_of(&back), ec::hash_of(&before_rows), "{}", ec::diff(&back, &before_rows));
    assert_eq!(ed.model.content_hash().unwrap(), before);
    ed.redo();
    let again = ec::dump_except(&ed.model, &["op", "clash_item"]);
    assert_eq!(ec::hash_of(&again), ec::hash_of(&after), "{}", ec::diff(&again, &after));
    assert_eq!(ed.model.content_hash().unwrap(), after_hash);
    ec::assert_clean(&ed.model, "after redo");
}

#[test]
fn merge_keeping_mine_changes_only_the_other_work() {
    let (mut mine, mut theirs, b, _m, theirs_path) = two_files();
    let w0 = b.walls[0];
    mine.set_wall_axis(w0, (0.0, 0.0), (6.0, 0.0)).unwrap();
    theirs.set_wall_axis(w0, (0.0, 0.0), (7.0, 0.0)).unwrap();
    theirs.add_wall(&wall(60.0, b.level, b.thin)).unwrap();
    let their_hash = theirs.content_hash().unwrap();
    drop(theirs);
    let line = mine.merge_lineage().unwrap();
    let p = prepare_merge(&line, &theirs_path, &ec::temp_file("work")).unwrap();
    let plan = mine.merge_preview(&p).unwrap();
    assert_eq!(plan.conflicts.len(), 1);
    // no pick given: the default for a conflict is mine
    let rep = mine.merge_apply(&p, &plan, &[]).unwrap();
    assert_eq!((rep.kept_mine, rep.took_theirs, rep.added), (1, 0, 1));
    assert_eq!(bx(&mine, w0), Some(6.0));
    assert_eq!(ec::count(&mine, "wall"), 4);
    // the colleague's file is only read
    assert_eq!(open(&theirs_path).content_hash().unwrap(), their_hash);
}

#[test]
fn unrelated_project_is_refused_and_a_stale_preview_too() {
    let (mut mine, mut theirs, b, _m, theirs_path) = two_files();
    // a project with another identity has no common base
    let other_path = ec::temp_file("other.ddc");
    drop(open(&other_path));
    let line = mine.merge_lineage().unwrap();
    let res = prepare_merge(&line, &other_path, &ec::temp_file("work"));
    assert!(matches!(res, Err(MergeError::NoCommonBase)), "{res:?}");

    // the model edited after the preview: the apply is refused and nothing is written
    theirs.set_wall_axis(b.walls[0], (0.0, 0.0), (7.0, 0.0)).unwrap();
    drop(theirs);
    let p = prepare_merge(&line, &theirs_path, &ec::temp_file("work")).unwrap();
    let plan = mine.merge_preview(&p).unwrap();
    mine.set_wall_axis(b.walls[1], (0.0, 20.0), (6.0, 20.0)).unwrap();
    let h = mine.content_hash().unwrap();
    assert!(mine.merge_apply(&p, &plan, &[]).is_err());
    assert_eq!(mine.content_hash().unwrap(), h);
}

/// Runs collab frames until `done` holds for the probe (the windows poll their jobs per frame).
fn pump(
    a: &mut ddc_app::shell::EditorApp,
    ctx: &egui::Context,
    done: impl Fn(&(WinProbe, WinProbe)) -> bool,
) -> (WinProbe, WinProbe) {
    for _ in 0..400 {
        let _ = ctx.run(egui::RawInput::default(), |c| ddc_app::collab::frame(a, c));
        let p = ddc_app::collab::probe(a);
        if done(&p) {
            return p;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("window did not settle: {:?}", ddc_app::collab::probe(a));
}

#[test]
fn merge_and_compare_commands_open_windows_with_the_chosen_file() {
    let (mut mine, mut theirs, b, _mine_path, theirs_path) = two_files();
    mine.set_wall_axis(b.walls[0], (0.0, 0.0), (6.0, 0.0)).unwrap();
    theirs.set_wall_axis(b.walls[0], (0.0, 0.0), (7.0, 0.0)).unwrap();
    theirs.set_wall_type(b.walls[1], b.thick).unwrap();
    drop(theirs);
    let mut ed = Editor::new_in_memory().unwrap();
    ed.replace_model(mine).unwrap();
    let mut a = ddc_app::shell::EditorApp::new(ed, None, "test".into());
    let ctx = ddc_ui::theme::headless_ctx();
    let settled = |p: &WinProbe| !matches!(p, WinProbe::Working);

    // merge: the picked colleague file is shown with the preview's conflict count
    ddc_app::file_pick::script_picks(vec![theirs_path.clone()]);
    assert!(a.run("data.merge_file"));
    let (m, d) = pump(&mut a, &ctx, |p| settled(&p.0));
    assert_eq!(d, WinProbe::Closed, "compare stays closed");
    match &m {
        WinProbe::Shown { source, count } => {
            assert_eq!(source.file_name(), theirs_path.file_name());
            assert_eq!(*count, 1, "one wall moved on both sides");
        }
        other => panic!("merge window: {other:?}"),
    }
    assert!(!a.ed.can_undo(), "opening the merge window is not an edit");

    // compare: the window lists the differences of the same file
    ddc_app::file_pick::script_picks(vec![theirs_path.clone()]);
    assert!(a.run("data.compare_version"));
    let (_, d) = pump(&mut a, &ctx, |p| settled(&p.1));
    match d {
        WinProbe::Shown { source, count } => {
            assert_eq!(source, theirs_path);
            assert!(count >= 2, "both edited walls differ, got {count}");
        }
        other => panic!("compare window: {other:?}"),
    }
    assert!(!a.ed.can_undo(), "comparing is not an edit");

    // a cancelled picker opens nothing new
    ddc_app::file_pick::script_picks(vec![]);
    assert!(a.run("data.merge_file"));
    let _ = ctx.run(egui::RawInput::default(), |c| ddc_app::collab::frame(&mut a, c));
    ddc_app::file_pick::clear_script();
}
