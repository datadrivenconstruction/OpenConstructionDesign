//! Button audit fixes (crates/app): each button does what its label says and answers visibly.

use crate::editor::Sel;
use crate::shell::EditorApp;
use crate::tools::Kind;
use ddc_ui::Lang;

fn app() -> EditorApp {
    crate::shot_press::set_screenshot_mode(true);
    let mut a = crate::layout_contract::app_in(Lang::En);
    a.prefs.path = None;
    a
}

fn walled() -> EditorApp {
    let mut a = app();
    room(&mut a);
    a
}

// the app starts in Select; the room is typed with the wall tool
fn room(a: &mut EditorApp) {
    a.ed.set_mode(crate::wall_tool::Mode::Polyline);
    crate::schedule::test_room(&mut a.ed);
    a.ed.set_mode(crate::wall_tool::Mode::Select);
}

fn toast_has(a: &EditorApp, key: &str) -> bool {
    let want = a.sh.cat.t(key);
    a.sh.toasts.live.iter().any(|t| t.text == want)
}

#[test]
fn unpin_with_nothing_pinned_says_so_in_plain_words() {
    let mut a = walled();
    let w = a.ed.walls[0].id;
    a.ed.selection = vec![Sel::Wall(w)];
    assert!(a.run("mod.unpin"));
    assert_eq!(a.ed.status.as_deref(), Some(crate::tr("cmd_buttons.unpin.none", &[]).as_str()));
    // pinned: the same button unpins
    assert!(a.run("mod.pin"));
    assert!(a.ed.model.is_pinned("wall", w).unwrap());
    assert!(a.run("mod.unpin"));
    assert!(!a.ed.model.is_pinned("wall", w).unwrap());
}

#[test]
fn change_type_works_on_the_selection_not_on_the_type_editor() {
    use crate::shell::panels::type_library::with_swap;
    let mut a = walled();
    with_swap(|s| *s = None);
    a.ed.selection.clear();
    assert!(a.run("mod.change_type"));
    assert!(with_swap(|s| s.is_none()), "nothing selected: no dialog");
    assert_eq!(a.ed.status.as_deref(), Some(a.sh.cat.t("cmd_buttons.change_type.none").as_str()));
    a.ed.selection = vec![Sel::Wall(a.ed.walls[0].id)];
    assert!(a.run("mod.change_type"));
    assert!(with_swap(|s| s.is_some()), "walls selected: the swap dialog");
    with_swap(|s| *s = None);
}

#[test]
fn units_press_says_what_it_did_and_the_toast_switches_back() {
    let mut a = app();
    let before = a.ed.tool.units;
    assert!(a.run("mgr.units"));
    assert_ne!(a.ed.tool.units, before);
    assert!(toast_has(&a, "cmd_buttons.units.imperial") || toast_has(&a, "cmd_buttons.units.metric"));
    let t = a.sh.toasts.live.iter().find(|t| t.command.as_deref() == Some("mgr.units"));
    assert!(t.is_some(), "the toast offers the way back");
    assert!(a.run("mgr.units"));
    assert_eq!(a.ed.tool.units, before);
}

#[test]
fn underlay_press_gives_feedback_both_ways() {
    let mut a = app();
    assert!(a.ed.underlay_on);
    assert!(a.run("view.underlay"));
    assert!(!a.ed.underlay_on);
    assert!(toast_has(&a, "cmd_buttons.underlay.off"));
    assert!(a.run("view.underlay"));
    assert!(a.ed.underlay_on);
    assert!(toast_has(&a, "cmd_buttons.underlay.none") || toast_has(&a, "cmd_buttons.underlay.on"));
}

#[test]
fn workplane_sentence_always_names_the_level() {
    let a = app();
    let said = crate::edit3d::wp_win::describe(&a.view.e3, &a.ed, &a.sh.cat);
    let name = a.ed.level_name();
    assert!(!name.is_empty());
    assert!(said.contains(&name), "{said}");
    assert!(!said.starts_with(','), "{said}");
}

#[test]
fn purge_offers_everything_but_preselects_nothing() {
    let mut a = app();
    a.ed.model
        .add_wall_type("spare", &[ddc_model::LayerSpec { material: "m".into(), thickness: 0.1 }])
        .unwrap();
    a.ed.model
        .add_wall_type("spare 2", &[ddc_model::LayerSpec { material: "m".into(), thickness: 0.1 }])
        .unwrap();
    assert!(a.run("mgr.purge"));
    let items = a.data.purge.as_ref().expect("window open");
    assert!(items.len() >= 2);
    assert!(items.iter().all(|i| !i.1), "no tick the person did not make");
}

#[test]
fn equipment_arms_its_own_tool_with_equipment_families_only() {
    let mut a = app();
    assert!(a.run("mep.equipment"));
    let t = a.ed.model_tool().expect("tool armed");
    assert_eq!(t.kind, Kind::Equipment);
    assert_ne!(t.kind, Kind::Component);
    assert!(!t.types.is_empty(), "the library has equipment");
    let lib = ddc_model::library().unwrap();
    for label in &t.types {
        let fam = label.split(" : ").next().unwrap();
        let f = lib.iter().find(|f| f.name == fam).expect("family of the label");
        assert_eq!(f.category, ddc_model::CompCategory::Equipment, "{label}");
    }
    assert!(a.run("arch.component"));
    assert_eq!(a.ed.model_tool().unwrap().kind, Kind::Component);
}

#[test]
fn duplicate_buttons_are_one_command() {
    let mut a = app();
    for (dup, one) in [
        ("mgr.coords", "mgr.project_info"),
        ("mgr.load", "mgr.library"),
        ("mep.check_system", "mep.system"),
    ] {
        assert_eq!(a.sh.reg.resolve(dup), one, "{dup}");
        assert!(a.sh.ribbon.enabled(&a.sh.reg, dup), "{dup} still runs");
    }
    assert!(a.run("mgr.coords"));
    assert!(crate::data_ui::info_open(), "same window as project info");
}

#[test]
fn level_delete_with_content_asks_once_without_a_second_answer() {
    let mut a = app();
    let upper = a.ed.add_level("Upper", 3.0).unwrap();
    a.ed.set_level(upper);
    room(&mut a);
    a.ed.status = None;
    let before = a.ed.levels().len();
    assert!(a.run("mgr.level_delete"));
    assert_eq!(a.ed.levels().len(), before, "nothing is deleted before the answer");
    assert!(
        a.ed.status.as_deref().is_none_or(|s| !s.to_lowercase().contains("nothing was deleted")),
        "the window carries the summary: {:?}",
        a.ed.status
    );
}

#[test]
fn place_with_sheets_opens_the_first_instead_of_asking_for_a_new_one() {
    let mut a = app();
    let id = a.ed.model.add_sheet(&ddc_model::SheetDef::new("A-101", "a")).unwrap();
    crate::sheet_view::deactivate();
    assert!(a.run("view.place"));
    assert_eq!(crate::sheet_view::active(), Some(id));
    assert!(!toast_has(&a, "shv.open_a_sheet_or_create_one"), "no toast over the opened sheet");
    crate::sheet_view::deactivate();
}

#[test]
fn data_table_toast_says_a_table_opened() {
    let mut a = app();
    assert!(a.run("data.table"));
    assert!(toast_has(&a, "cmd_buttons.table.opened"));
    assert!(
        a.sh.toasts.live.iter().any(|t| t.command.as_deref() == Some("ui.layout.back")),
        "the way back stays"
    );
}

#[test]
fn fitting_button_selects_the_fittings_of_the_level_when_no_run_is_selected() {
    let mut a = app();
    a.ed.selection.clear();
    assert!(a.run("mep.fitting"));
    // an empty model has no fitting: the hint stays, the selection stays empty
    assert!(a.ed.selection.is_empty());
    assert!(a.ed.status.is_some());
}
