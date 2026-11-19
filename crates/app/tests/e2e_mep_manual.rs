//! Manual MEP through the commands: draw a duct run, attach a terminal to its system, route a
//! free run between two points. Each gesture is one undo step that restores the model exactly
//! and redo reproduces it.

mod e2e_common;

use ddc_app::editor::Editor;
use ddc_app::shell::EditorApp;
use ddc_app::wall_tool::{Event, Key, Mode};
use ddc_geom::v2;
use e2e_common::{self as ec, TableRows};

const SKIP: &[&str] = &["op", "clash_item"];

fn app() -> EditorApp {
    let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    a.ed.set_mode(Mode::Select);
    a
}

fn count(a: &EditorApp, table: &str) -> i64 {
    a.ed.model
        .sql()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn data(a: &EditorApp) -> Vec<TableRows> {
    ec::dump_except(&a.ed.model, SKIP)
}

fn click(a: &mut EditorApp, x: f64, y: f64) {
    a.ed.handle(Event::Move(v2(x, y)));
    a.ed.handle(Event::Click(v2(x, y)));
}

/// Leaves the tool, undoes the last gesture, checks the model is back to `before` and no
/// further step is left, then redoes and checks it equals `after`.
fn one_step(a: &mut EditorApp, before: &[TableRows], after: &[TableRows], what: &str) {
    a.ed.set_mode(Mode::Select);
    let depth = a.ed.undo_len();
    a.ed.undo();
    assert_eq!((a.ed.undo_len(), a.ed.redo_len()), (depth - 1, 1), "{what}: one gesture is one step");
    let back = data(a);
    assert_eq!(
        ec::hash_of(&back),
        ec::hash_of(before),
        "{what}: undo does not restore{}",
        ec::diff(&back, before)
    );
    a.ed.redo();
    let again = data(a);
    assert_eq!(
        ec::hash_of(&again),
        ec::hash_of(after),
        "{what}: redo does not reproduce{}",
        ec::diff(&again, after)
    );
    assert_eq!((a.ed.undo_len(), a.ed.redo_len()), (depth, 0), "{what}: no further step left");
}

#[test]
fn manual_mep_run_attach_route_each_one_undo_step() {
    let mut a = app();
    assert!(!a.ed.can_undo());
    let h0 = data(&a);

    // draw: a duct run in a new system
    assert!(a.run("mep.duct"));
    assert_eq!(a.active_command(), Some("mep.duct"));
    click(&mut a, 0.0, 0.0);
    click(&mut a, 8.0, 0.0);
    a.ed.handle(Event::Key(Key::Enter));
    assert_eq!(count(&a, "mep_run"), 1);
    let sys = a.ed.model.mep_systems().unwrap();
    assert_eq!(sys.len(), 1, "the run made its system in the same step");
    let sys = sys[0].id;
    let h1 = data(&a);
    one_step(&mut a, &h0, &h1, "draw");
    assert_eq!(count(&a, "mep_run"), 1);

    // attach: a terminal placed into that system joins the run through its port
    let ran = a.run("mep.terminal");
    assert!(ran, "mep.terminal run; active {:?}", a.active_command());
    a.ed.model_tool_mut().unwrap().mep.system = Some(sys);
    click(&mut a, 4.0, 3.0);
    assert_eq!(count(&a, "comp"), 1, "terminal placed; status {:?} tool {:?}", a.ed.status, a.ed.model_tool().map(|t| (t.types.len(), t.opts.ty, t.type_name().to_string())));
    assert_eq!(count(&a, "mep_port"), 1);
    assert_eq!(count(&a, "mep_run"), 2, "stub from the terminal to the run");
    let h2 = data(&a);
    one_step(&mut a, &h1, &h2, "attach");

    // route: two free ends far from the existing network, system picked in the tool
    assert!(a.run("mep.route"));
    assert_eq!(a.active_command(), Some("mep.route"));
    a.ed.model_tool_mut().unwrap().mep.system = Some(sys);
    click(&mut a, 0.0, 10.0);
    assert_eq!(count(&a, "mep_run"), 2, "first click only starts the route");
    click(&mut a, 8.0, 14.0);
    assert_eq!(count(&a, "mep_run"), 3, "one routed run");
    assert_eq!(a.ed.model.mep_systems().unwrap().len(), 1, "no extra system");
    let routed = *a.ed.model.mep_runs(Some(sys)).unwrap().iter().max().unwrap();
    let run = a.ed.model.mep_run(routed).unwrap();
    assert!(run.points.len() >= 3, "an elbow between two free ends");
    let near = |p: &(f64, f64, f64), x: f64, y: f64| (p.0 - x).abs() < 1e-6 && (p.1 - y).abs() < 1e-6;
    let (f, l) = (run.points.first().unwrap(), run.points.last().unwrap());
    assert!(
        (near(f, 0.0, 10.0) && near(l, 8.0, 14.0)) || (near(l, 0.0, 10.0) && near(f, 8.0, 14.0)),
        "ends at the two clicks"
    );
    let h3 = data(&a);
    one_step(&mut a, &h2, &h3, "route");

    let iss = a.ed.model.check_integrity().unwrap();
    assert!(iss.is_empty(), "integrity {iss:?}");
}

#[test]
fn route_without_a_system_refuses_and_changes_nothing() {
    let mut a = app();
    assert!(a.run("mep.route"));
    let before = data(&a);
    click(&mut a, 0.0, 10.0);
    click(&mut a, 8.0, 14.0);
    assert_eq!(ec::hash_of(&data(&a)), ec::hash_of(&before));
    a.ed.set_mode(Mode::Select);
    assert!(!a.ed.can_undo(), "a refused route leaves no undo step");
    assert!(a.ed.status.is_some(), "the refusal is worded");
}
