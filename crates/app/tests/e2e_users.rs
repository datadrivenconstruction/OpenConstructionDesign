// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Scenarios as a real architect works: ribbon command by id, clicks and typed digits in the
//! plan, undo/redo from the keyboard. Nothing is written into the model directly except the
//! second storey (the level dialog is a window) and the sheet (its window state is not
//! reachable headless). After every step: counts, derived quantities, integrity.

mod e2e_common;

use std::sync::Arc;

use ddc_app::editor::{Editor, OpFn};
use ddc_app::shell::EditorApp;
use ddc_app::sheet_view::{place, print_to, Payload, Print};
use ddc_app::topo;
use ddc_app::wall_tool::{Event, Key, Mode};
use ddc_geom::v2;
use ddc_io::ifc::{export_ifc, import_ifc};
use ddc_model::{Model, SheetDef, ViewRef};
use e2e_common::{assert_clean, count, ids, temp_file};

/// A person at the keyboard and mouse.
struct User {
    app: EditorApp,
}

impl User {
    fn new() -> User {
        let ed = Editor::new_in_memory().unwrap();
        User {
            app: EditorApp::new(ed, None, "test".into()),
        }
    }

    fn ed(&mut self) -> &mut Editor {
        &mut self.app.ed
    }

    /// Ribbon button or palette entry.
    fn cmd(&mut self, id: &str) {
        assert!(self.app.run(id), "command {id} is not wired");
    }

    fn click(&mut self, x: f64, y: f64) {
        let e = self.ed();
        e.handle(Event::Move(v2(x, y)));
        e.handle(Event::Click(v2(x, y)));
    }

    fn drag(&mut self, a: (f64, f64), b: (f64, f64)) {
        let e = self.ed();
        e.handle(Event::Move(v2(a.0, a.1)));
        e.handle(Event::Press(v2(a.0, a.1)));
        e.handle(Event::Move(v2(b.0, b.1)));
        e.handle(Event::Release(v2(b.0, b.1)));
    }

    fn key(&mut self, k: Key) {
        self.ed().handle(Event::Key(k));
    }

    fn enter(&mut self) {
        self.key(Key::Enter);
    }

    fn esc(&mut self) {
        self.key(Key::Escape);
    }

    /// Digits typed into the active tool, then Enter.
    fn typed(&mut self, s: &str) {
        for c in s.chars() {
            self.ed().handle(Event::Char(c));
        }
        self.enter();
    }

    fn n(&mut self, table: &str) -> i64 {
        count(&self.ed().model, table)
    }

    fn undo(&mut self) {
        self.ed().set_mode(Mode::Select);
        self.cmd("edit.undo");
    }

    fn redo(&mut self) {
        self.cmd("edit.redo");
    }

    fn ok(&mut self, ctx: &str) {
        assert_clean(&self.ed().model, ctx);
    }

    fn opts(&mut self) -> &mut ddc_app::tools::Opts {
        &mut self.ed().model_tool_mut().expect("a model tool is armed").opts
    }

    fn status(&mut self) -> String {
        self.ed().status.clone().unwrap_or_default()
    }

    /// One gesture is one undo step: undo removes it, redo brings the same count back.
    fn undo_redo(&mut self, table: &str, before: i64, ctx: &str) {
        let after = self.n(table);
        assert!(after > before, "{ctx}: {table} {before} -> {after}, nothing was built");
        self.undo();
        assert_eq!(self.n(table), before, "{ctx}: undo");
        self.ok(ctx);
        self.redo();
        assert_eq!(self.n(table), after, "{ctx}: redo");
        self.ok(ctx);
    }

    /// Second storey at 3 m, as the level dialog would add it.
    fn second_storey(&mut self) {
        self.ed()
            .run_op(OpFn(Arc::new(|m: &mut Model| m.add_level("L2", 3.0).map(drop))))
            .unwrap();
        self.ed().reload().unwrap();
    }

    /// An 8 x 6 m house: the wall-room command and one drag.
    fn house(&mut self) {
        self.cmd("arch.wall_room");
        self.drag((0.0, 0.0), (8.0, 6.0));
        self.ed().set_mode(Mode::Select);
        assert_eq!(self.n("wall"), 4, "four walls from one drag");
        self.ok("house");
    }

    fn slab_area(&mut self, id: i64) -> f64 {
        self.ed()
            .model
            .slab_quantities(id)
            .unwrap()
            .iter()
            .map(|q| q.area)
            .sum()
    }
}

fn last(u: &mut User, table: &str) -> i64 {
    *ids(&u.ed().model, table).last().unwrap_or_else(|| panic!("no rows in {table}"))
}

#[test]
fn walls_by_drag_and_by_chain() {
    let mut u = User::new();
    u.cmd("arch.wall_room");
    u.drag((0.0, 0.0), (8.0, 6.0));
    u.ed().set_mode(Mode::Select);
    assert_eq!(u.n("wall"), 4);
    u.undo_redo("wall", 0, "rect walls");
    // a partition by clicks: two points, then Enter ends the chain
    u.cmd("arch.wall");
    u.click(4.0, 0.0);
    u.click(4.0, 6.0);
    u.enter();
    u.esc();
    u.ed().set_mode(Mode::Select);
    assert!(u.n("wall") >= 5, "partition added: {}", u.n("wall"));
    u.ok("partition");
}

#[test]
fn floor_by_picked_walls() {
    let mut u = User::new();
    u.house();
    u.cmd("arch.floor");
    u.opts().walls = true;
    for (x, y) in [(4.0, 0.0), (8.0, 3.0), (4.0, 6.0), (0.0, 3.0)] {
        u.click(x, y);
    }
    assert_eq!(u.n("slab"), 0, "picking alone builds nothing");
    u.enter();
    assert_eq!(u.n("slab"), 1, "status: {}", u.status());
    let s = last(&mut u, "slab");
    let a = u.slab_area(s);
    assert!(a > 40.0 && a < 56.0, "slab area {a}");
    u.ok("floor by walls");
    u.undo_redo("slab", 0, "floor by walls");
}

#[test]
fn floor_by_contour_clicks() {
    let mut u = User::new();
    u.cmd("arch.floor");
    for (x, y) in [(0.0, 0.0), (8.0, 0.0), (8.0, 6.0), (0.0, 6.0)] {
        u.click(x, y);
    }
    assert_eq!(u.n("slab"), 0, "contour is open until Enter");
    u.enter();
    assert_eq!(u.n("slab"), 1, "status: {}", u.status());
    let s = last(&mut u, "slab");
    let a = u.slab_area(s);
    assert!((a - 48.0).abs() < 0.05, "slab area {a}");
    u.ok("floor by contour");
    u.undo_redo("slab", 0, "floor by contour");
}

#[test]
fn floor_by_click_inside_walls() {
    let mut u = User::new();
    u.house();
    u.cmd("arch.floor");
    u.opts().pick = true;
    u.click(30.0, 30.0);
    assert_eq!(u.n("slab"), 0, "outside the loop builds nothing");
    u.click(4.0, 3.0);
    assert_eq!(u.n("slab"), 1, "status: {}", u.status());
    let s = last(&mut u, "slab");
    let a = u.slab_area(s);
    assert!(a > 40.0 && a < 56.0, "slab area {a}");
    u.undo_redo("slab", 0, "floor by click");
}

#[test]
fn slab_with_a_shaft_opening() {
    let mut u = User::new();
    u.house();
    u.second_storey();
    u.cmd("arch.floor");
    u.opts().pick = true;
    u.click(4.0, 3.0);
    assert_eq!(u.n("slab"), 1, "status: {}", u.status());
    let s = last(&mut u, "slab");
    let full = u.slab_area(s);
    u.cmd("arch.shaft");
    for (x, y) in [(2.0, 2.0), (4.0, 2.0), (4.0, 4.0), (2.0, 4.0)] {
        u.click(x, y);
    }
    u.enter();
    assert_eq!(u.n("shaft"), 1, "status: {}", u.status());
    let cut = u.slab_area(s);
    assert!((full - cut - 4.0).abs() < 0.05, "full {full}, with opening {cut}");
    u.ok("shaft");
    u.undo();
    assert_eq!(u.n("shaft"), 0);
    let back = u.slab_area(s);
    assert!((back - full).abs() < 1e-6, "undo restores the slab: {back} vs {full}");
    u.redo();
    let again = u.slab_area(s);
    assert!((again - cut).abs() < 1e-6, "redo cuts again: {again} vs {cut}");
}

#[test]
fn roof_by_picked_walls_and_by_click() {
    let mut u = User::new();
    u.house();
    u.cmd("arch.roof");
    u.opts().walls = true;
    u.opts().overhang = 0.5;
    for (x, y) in [(4.0, 0.0), (8.0, 3.0), (4.0, 6.0), (0.0, 3.0)] {
        u.click(x, y);
    }
    assert_eq!(u.n("roof"), 0);
    u.enter();
    assert_eq!(u.n("roof"), 1, "status: {}", u.status());
    u.ok("roof by walls");
    u.undo_redo("roof", 0, "roof by walls");
    u.cmd("arch.roof");
    u.opts().pick = true;
    u.click(4.0, 3.0);
    assert_eq!(u.n("roof"), 2, "status: {}", u.status());
    u.ok("roof by click");
}

#[test]
fn roof_by_sketch() {
    let mut u = User::new();
    u.cmd("arch.roof");
    for (x, y) in [(0.0, 0.0), (8.0, 0.0), (8.0, 6.0), (0.0, 6.0)] {
        u.click(x, y);
    }
    u.enter();
    assert_eq!(u.n("roof"), 1, "status: {}", u.status());
    u.ok("roof sketch");
    u.undo_redo("roof", 0, "roof sketch");
}

/// The survey of a hill: clicked points at their own elevations.
fn hill(u: &mut User) -> i64 {
    u.cmd("site.toposolid");
    for (x, y, z) in [
        (-10.0, -10.0, 0.0),
        (30.0, -10.0, 0.5),
        (30.0, 30.0, 2.0),
        (-10.0, 30.0, 1.0),
        (10.0, 10.0, 0.8),
        (10.0, -10.0, 0.2),
    ] {
        u.typed(&z.to_string());
        u.click(x, y);
    }
    u.enter();
    assert_eq!(u.n("toposolid"), 1, "status: {}", u.status());
    last(u, "toposolid")
}

#[test]
fn terrain_from_clicked_points() {
    let mut u = User::new();
    let t = hill(&mut u);
    assert_eq!(u.ed().model.topo_points(t).unwrap().len(), 6);
    let pieces = u.ed().model.topo_pieces(t).unwrap();
    assert!(!pieces.is_empty(), "the surface is built");
    u.ok("terrain points");
    u.esc();
    u.undo_redo("toposolid", 0, "terrain points");
}

#[test]
fn terrain_from_contour_lines() {
    let mut u = User::new();
    let ring = |r: f64, z: f64| -> Vec<(f64, f64, f64)> {
        vec![(-r, -r, z), (r, -r, z), (r, r, z), (-r, r, z), (-r, -r, z)]
    };
    let src = ddc_io::topo::TopoSource {
        breaklines: vec![ring(30.0, 0.0), ring(20.0, 1.0), ring(10.0, 2.0)],
        ..Default::default()
    };
    topo::import_source(u.ed(), "Site", src);
    assert_eq!(u.n("toposolid"), 1, "status: {}", u.status());
    let t = last(&mut u, "toposolid");
    assert!(!u.ed().model.topo_pieces(t).unwrap().is_empty(), "surface from contours");
    u.ok("contours");
    u.undo_redo("toposolid", 0, "contours");
}

#[test]
fn terrain_pad_under_the_house() {
    let mut u = User::new();
    let t = hill(&mut u);
    u.esc();
    u.cmd("site.pad");
    assert!(u.ed().topo.tool.is_some(), "pad tool armed: {}", u.status());
    u.typed("0.5");
    for (x, y) in [(-1.0, -1.0), (9.0, -1.0), (9.0, 7.0), (-1.0, 7.0)] {
        u.click(x, y);
    }
    u.enter();
    assert_eq!(u.ed().model.pads(t).unwrap().len(), 1, "status: {}", u.status());
    assert!(!u.ed().model.topo_pieces(t).unwrap().is_empty(), "surface with the pad is built");
    u.ok("pad");
    topo::disarm(u.ed());
    u.undo();
    assert!(u.ed().model.pads(t).unwrap().is_empty());
    u.redo();
    assert_eq!(u.ed().model.pads(t).unwrap().len(), 1);
}

#[test]
fn stairs_between_storeys() {
    let mut u = User::new();
    u.cmd("arch.stair");
    u.click(0.0, 0.0);
    assert_eq!(u.n("stair"), 0, "no storey above");
    u.esc();
    u.second_storey();
    u.cmd("arch.stair");
    u.click(1.0, 1.0);
    u.click(1.0, 5.0);
    assert_eq!(u.n("stair"), 1, "status: {}", u.status());
    u.ok("stair");
    u.undo_redo("stair", 0, "stair");
}

#[test]
fn doors_and_windows_in_walls() {
    let mut u = User::new();
    u.house();
    u.cmd("arch.door");
    u.click(2.0, 0.0);
    assert_eq!(u.n("opening"), 1, "door: {}", u.status());
    u.esc();
    u.cmd("arch.window");
    u.click(8.0, 3.0);
    assert_eq!(u.n("opening"), 2, "window: {}", u.status());
    u.esc();
    u.ed().set_mode(Mode::Select);
    u.ok("openings");
    let kinds: Vec<String> = {
        let ed = u.ed();
        let mut st = ed.model.sql().prepare("SELECT kind FROM opening ORDER BY id").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().flatten().collect()
    };
    assert_eq!(kinds, ["door", "window"]);
    u.undo_redo("opening", 1, "window");
}

#[test]
fn rooms_by_click_inside_walls() {
    let mut u = User::new();
    u.house();
    let before = u.n("room");
    u.cmd("arch.room");
    u.click(4.0, 3.0);
    assert_eq!(u.n("room"), before + 1, "status: {}", u.status());
    assert!(!u.ed().rooms.is_empty(), "the room has an outline");
    u.ok("room");
    u.undo_redo("room", before, "room");
}

#[test]
fn section_view_by_three_clicks() {
    let mut u = User::new();
    u.house();
    u.cmd("arch.section");
    u.click(-2.0, 3.0);
    u.click(10.0, 3.0);
    assert!(u.ed().views.is_empty(), "the depth is still to set");
    u.click(4.0, 8.0);
    assert_eq!(u.ed().views.len(), 1, "status: {}", u.status());
    u.ok("section");
    assert_eq!(u.n("view_def"), 1);
}

#[test]
fn sheet_with_plan_and_section_prints_to_pdf() {
    let mut u = User::new();
    u.house();
    u.cmd("arch.section");
    u.click(-2.0, 3.0);
    u.click(10.0, 3.0);
    u.click(4.0, 8.0);
    let view = u.ed().views[0].0;
    u.cmd("view.sheet");
    u.ed()
        .run_op(OpFn(Arc::new(|m: &mut Model| {
            m.add_sheet(&SheetDef::new("A-101", "Plan and section")).map(drop)
        })))
        .unwrap();
    let sheet = last(&mut u, "sheet");
    let level = u.ed().level;
    place(
        u.ed(),
        sheet,
        &Payload::View(ViewRef::Plan { level, cut: 1.2 }, "Plan".into()),
        (150.0, 150.0),
        0.0,
    )
    .unwrap();
    place(u.ed(), sheet, &Payload::View(ViewRef::View(view), "Section".into()), (320.0, 150.0), 0.0)
        .unwrap();
    assert_eq!(u.n("viewport"), 2);
    u.ok("sheet");
    let dir = temp_file("users_sheets");
    std::fs::create_dir_all(&dir).unwrap();
    let mut p = Print::new(None);
    p.separate = true;
    p.pdf = true;
    let rep = print_to(u.ed(), false, &p, &[sheet], &dir).unwrap_or_else(|e| panic!("print: {e}"));
    let pdfs: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|f| f.file_name().to_string_lossy().to_ascii_lowercase().ends_with(".pdf"))
        .collect();
    assert_eq!(pdfs.len(), 1, "one PDF: {rep}");
    assert!(pdfs[0].metadata().unwrap().len() > 1000, "PDF has content: {rep}");
}

/// Tables that must survive IFC with their row count.
const KEPT: [&str; 6] = ["wall", "opening", "slab", "room", "roof", "stair"];

#[test]
fn ifc_export_and_reimport_of_a_built_house() {
    let mut u = User::new();
    u.house();
    u.second_storey();
    u.cmd("arch.door");
    u.click(2.0, 0.0);
    u.esc();
    u.cmd("arch.window");
    u.click(8.0, 3.0);
    u.esc();
    u.cmd("arch.floor");
    u.opts().pick = true;
    u.click(4.0, 3.0);
    u.cmd("arch.roof");
    u.opts().pick = true;
    u.click(4.0, 3.0);
    u.cmd("arch.room");
    u.click(4.0, 3.0);
    u.cmd("arch.stair");
    u.click(1.0, 1.0);
    u.click(1.0, 5.0);
    u.ed().set_mode(Mode::Select);
    let want: Vec<(&str, i64)> = KEPT.iter().map(|t| (*t, u.n(t))).collect();
    for (t, n) in &want {
        assert!(*n > 0, "{t} was not built: {}", u.status());
    }
    u.ok("built house");
    let text = export_ifc(&u.ed().model).expect("export");
    let mut fresh = Model::open(":memory:").unwrap();
    import_ifc(&mut fresh, text.as_bytes()).expect("import");
    assert_clean(&fresh, "re-imported");
    for (t, n) in want {
        assert_eq!(count(&fresh, t), n, "{t} after IFC round trip");
    }
}
