//! f2 scenarios: floor, slab, ramp, separator, zone, level, empty opening.

use super::layer::{Cls, Layer};
use super::{arm, Kind};
use crate::editor::structx::{category_of, code_of, delete_all, kind_of};
use crate::editor::Sel;
use crate::editor::{Editor, OpFn};
use crate::modify_tools::{ModTool, Op, Out};
use crate::opening_tool::{Kind as OpKind, OpeningType};
use crate::sketch_mode::{NewSlab, SketchMode};
use crate::wall_tool::{Event, Key, Mode};
use ddc_geom::sketch::{pick_walls, Sketch};
use ddc_geom::v2;
use ddc_model::{Model, RampSpec, RoomSpec, SepSpec, SketchHost, TagCat, WallSpec, ZoneSpec};
use ddc_ui::i18n::{Catalog, Lang};
use std::sync::Arc;

fn ed() -> Editor {
    Editor::new_in_memory().unwrap()
}

fn count(e: &Editor, table: &str) -> i64 {
    e.model
        .sql()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn click(e: &mut Editor, x: f64, y: f64) {
    e.handle(Event::Move(v2(x, y)));
    e.handle(Event::Click(v2(x, y)));
}

fn enter(e: &mut Editor) {
    e.handle(Event::Key(Key::Enter));
}

fn esc(e: &mut Editor) {
    e.handle(Event::Key(Key::Escape));
}

fn pts(e: &Editor) -> usize {
    e.model_tool().map_or(0, |a| a.pts.len())
}

fn walls(e: &mut Editor) {
    e.set_mode(Mode::Rect);
    e.handle(Event::Move(v2(0.0, 0.0)));
    e.handle(Event::Press(v2(0.0, 0.0)));
    e.handle(Event::Move(v2(6.0, 4.0)));
    e.handle(Event::Release(v2(6.0, 4.0)));
    e.set_mode(Mode::Select);
}

/// A room in the 6×4 loop (if the walls did not create one themselves).
fn room(e: &mut Editor) -> i64 {
    walls(e);
    if e.rooms.is_empty() {
        let level = e.level;
        e.model
            .add_room(&RoomSpec {
                level,
                name: "R".into(),
                number: "1".into(),
                point: (3.0, 2.0),
                height: 2.7,
            })
            .unwrap();
        e.reload().unwrap();
    }
    e.rooms[0].id
}

/// Ramp without a top level: the rise is given by an offset.
fn free_ramp(level: i64) -> RampSpec {
    let mut s = RampSpec::new(level, None, vec![(0.0, 0.0), (8.0, 0.0)]);
    s.top_offset = 0.5;
    s
}

fn wall_segs(e: &mut Editor, segs: &[((f64, f64), (f64, f64))]) {
    let wall_type = e.current_type().unwrap().id;
    for &(a, b) in segs {
        let spec = WallSpec {
            a,
            b,
            level: e.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type,
            offset0: None,
        };
        e.model.add_wall(&spec).unwrap();
    }
    e.reload().unwrap();
}

fn outline(e: &mut Editor) {
    for (x, y) in [(0.0, 0.0), (5.0, 0.0), (5.0, 4.0), (0.0, 4.0)] {
        click(e, x, y);
    }
}

fn opts(e: &mut Editor) -> &mut super::Opts {
    &mut e.model_tool_mut().unwrap().opts
}

/// Undo reverts the whole gesture, redo restores it.
fn undo_redo(e: &mut Editor, table: &str, before: i64) {
    let n = count(e, table);
    assert!(n > before, "{table}: nothing created");
    e.set_mode(Mode::Select);
    e.undo();
    assert_eq!(count(e, table), before, "{table}: one undo step");
    e.redo();
    assert_eq!(count(e, table), n, "{table}: redo restores");
}

#[test]
fn floor_by_outline_is_one_undo_with_its_type() {
    let mut e = ed();
    let types = count(&e, "slab_type");
    arm(&mut e, Kind::Floor, Lang::En);
    outline(&mut e);
    enter(&mut e);
    assert_eq!(count(&e, "slab"), 1);
    assert_eq!(pts(&e), 0);
    assert_eq!(count(&e, "slab_type"), types + 1);
    undo_redo(&mut e, "slab", 0);
    e.undo();
    assert_eq!(
        count(&e, "slab_type"),
        types,
        "the new type leaves with the slab"
    );
}

#[test]
fn struct_slab_uses_own_types() {
    let mut e = ed();
    arm(&mut e, Kind::StructSlab, Lang::En);
    assert!(e.model_tool().unwrap().types[0].starts_with(&crate::app2_fx::fx("tf2_slab")));
    outline(&mut e);
    enter(&mut e);
    assert_eq!(count(&e, "slab"), 1);
    let name: String = e
        .model
        .sql()
        .query_row(
            "SELECT t.name FROM slab s JOIN slab_type t ON t.id=s.stype",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(name.starts_with(&crate::app2_fx::fx("tf2_slab")), "{name}");
}

#[test]
fn floor_escape_steps_back_one_point() {
    let mut e = ed();
    arm(&mut e, Kind::Floor, Lang::En);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 5.0, 0.0);
    click(&mut e, 5.0, 4.0);
    for left in [2, 1, 0] {
        esc(&mut e);
        assert_eq!(pts(&e), left);
        assert!(e.model_tool().is_some());
    }
    esc(&mut e);
    assert!(e.model_tool().is_none(), "empty tool exits on Esc");
    assert_eq!(count(&e, "slab"), 0);
}

#[test]
fn floor_two_points_do_not_close() {
    let mut e = ed();
    arm(&mut e, Kind::Floor, Lang::En);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 5.0, 0.0);
    enter(&mut e);
    assert_eq!(count(&e, "slab"), 0);
    assert_eq!(pts(&e), 2);
}

#[test]
fn floor_picks_wall_region() {
    let mut e = ed();
    walls(&mut e);
    arm(&mut e, Kind::Floor, Lang::En);
    opts(&mut e).pick = true;
    assert_eq!(e.model_tool().unwrap().step(), "pick");
    click(&mut e, 20.0, 20.0);
    assert_eq!(count(&e, "slab"), 0, "outside any wall loop");
    click(&mut e, 3.0, 2.0);
    assert_eq!(count(&e, "slab"), 1);
    undo_redo(&mut e, "slab", 0);
}

#[test]
fn floor_by_picked_walls() {
    let mut e = ed();
    walls(&mut e);
    arm(&mut e, Kind::Floor, Lang::En);
    opts(&mut e).walls = true;
    assert_eq!(e.model_tool().unwrap().step(), "walls");
    click(&mut e, 3.0, 0.0);
    enter(&mut e);
    assert_eq!(count(&e, "slab"), 0, "one wall does not close");
    click(&mut e, 3.0, 0.0);
    assert!(
        e.model_tool().unwrap().picked.is_empty(),
        "second click removes"
    );
    for (x, y) in [(3.0, 0.0), (6.0, 2.0), (3.0, 4.0), (0.0, 2.0)] {
        click(&mut e, x, y);
    }
    assert_eq!(e.model_tool().unwrap().picked.len(), 4);
    esc(&mut e);
    assert_eq!(e.model_tool().unwrap().picked.len(), 3);
    click(&mut e, 0.0, 2.0);
    enter(&mut e);
    assert_eq!(count(&e, "slab"), 1);
    assert!(e.model_tool().unwrap().picked.is_empty());
    undo_redo(&mut e, "slab", 0);
}

#[test]
fn floor_slope_goes_with_the_slab_in_one_undo() {
    let mut e = ed();
    arm(&mut e, Kind::Floor, Lang::En);
    opts(&mut e).slope = 2.0;
    outline(&mut e);
    enter(&mut e);
    let id: i64 = e
        .model
        .sql()
        .query_row("SELECT id FROM slab", [], |r| r.get(0))
        .unwrap();
    let pl = e.model.slab_slope(id).unwrap().unwrap();
    assert!((pl.gx - 0.02).abs() < 1e-9 && pl.gy.abs() < 1e-9, "{pl:?}");
    undo_redo(&mut e, "slab", 0);
}

#[test]
fn floor_from_sketch_is_one_undo() {
    let mut e = ed();
    let a = [
        (v2(0.0, 0.0), v2(5.0, 0.0)),
        (v2(5.0, 0.0), v2(5.0, 4.0)),
        (v2(5.0, 4.0), v2(0.0, 4.0)),
        (v2(0.0, 4.0), v2(0.0, 0.0)),
    ];
    let curves = pick_walls(&a, 0.0).unwrap();
    let mut m = SketchMode::new(SketchHost::Slab(0), Some(Sketch { curves }));
    m.new_slab = Some(NewSlab {
        level: e.level,
        type_name: "Slab 250".into(),
        offset: 0.0,
        slope: None,
    });
    let r = m.record(&e).unwrap();
    e.apply_ext(r).unwrap();
    assert_eq!(count(&e, "slab"), 1);
    undo_redo(&mut e, "slab", 0);
}

#[test]
fn ramp_by_axis_is_one_undo_and_escape_steps_back() {
    let mut e = ed();
    arm(&mut e, Kind::Ramp, Lang::En);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 4.0, 0.0);
    click(&mut e, 4.0, 3.0);
    esc(&mut e);
    assert_eq!(pts(&e), 2);
    enter(&mut e);
    assert_eq!(count(&e, "ramp"), 1);
    assert_eq!(pts(&e), 0);
    let id: i64 = e
        .model
        .sql()
        .query_row("SELECT id FROM ramp", [], |r| r.get(0))
        .unwrap();
    let q = e.model.ramp_qty(id).unwrap();
    assert!((q.rise - 0.6).abs() < 1e-9, "{}", q.rise);
    assert!((q.run - 7.2).abs() < 1e-6, "{}", q.run);
    undo_redo(&mut e, "ramp", 0);
}

#[test]
fn ramp_to_level_above_needs_one() {
    let mut e = ed();
    arm(&mut e, Kind::Ramp, Lang::En);
    if e.model_tool().unwrap().above.is_some() {
        return;
    }
    opts(&mut e).to_top = true;
    click(&mut e, 0.0, 0.0);
    assert_eq!(pts(&e), 0, "no level above: the click is refused");
    assert!(e.status.is_some());
}

#[test]
fn ramp_shows_in_layer_selection_and_props() {
    let mut e = ed();
    let spec = free_ramp(e.level);
    let id = e.model.add_ramp(&spec).unwrap();
    e.reload().unwrap();
    let code = code_of("ramp").unwrap();
    assert_eq!(kind_of(code), Some("ramp"));
    assert_eq!(category_of(code), "ramp");
    assert!(e.struct_fp.contains(code, id));
    let mut l = Layer::default();
    l.sync(&e.model, e.level, 1);
    assert!(l.polys.iter().any(|p| p.0 == Cls::Stair && p.2));
    assert_eq!(l.labels.len(), 1);
    let mut v = crate::struct_props::values(&e.model, "ramp", id).unwrap();
    assert_eq!(v.len(), crate::struct_props::fields("ramp").len());
    let old = v[0];
    v[0] = 1.5;
    let vals = v.clone();
    e.run_op(OpFn(Arc::new(move |m: &mut Model| {
        crate::struct_props::apply(m, "ramp", id, &vals)
    })))
    .unwrap();
    assert_eq!(e.model.ramp(id).unwrap().width, 1.5);
    e.undo();
    assert_eq!(e.model.ramp(id).unwrap().width, old);
}

#[test]
fn room_separators_chain_and_escape_steps_back() {
    let mut e = ed();
    arm(&mut e, Kind::RoomSep, Lang::En);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 2.0, 0.0);
    click(&mut e, 2.0, 2.0);
    assert_eq!(count(&e, "room_sep"), 2);
    assert_eq!(pts(&e), 1, "chain continues from the end");
    esc(&mut e);
    assert_eq!(pts(&e), 0);
    esc(&mut e);
    assert!(e.model_tool().is_none());
    e.undo();
    assert_eq!(count(&e, "room_sep"), 1, "each segment is one undo");
}

#[test]
fn room_separator_props_and_snap_layer() {
    let mut e = ed();
    let id = e
        .model
        .add_room_sep(&SepSpec {
            level: e.level,
            a: (0.0, 0.0),
            b: (3.0, 0.0),
        })
        .unwrap();
    e.reload().unwrap();
    assert!(e.struct_fp.contains(code_of("room_sep").unwrap(), id));
    let mut l = Layer::default();
    l.sync(&e.model, e.level, 1);
    assert_eq!(l.seps.len(), 1);
    assert!(l.polys.iter().any(|p| p.0 == Cls::RoomSep));
    let v = crate::struct_props::values(&e.model, "room_sep", id).unwrap();
    assert_eq!(v, vec![0.0, 0.0, 3.0, 0.0]);
}

#[test]
fn zone_from_clicked_rooms_is_one_undo() {
    let mut e = ed();
    let r = room(&mut e);
    arm(&mut e, Kind::Zone, Lang::En);
    enter(&mut e);
    assert_eq!(count(&e, "zone"), 0, "no rooms yet");
    click(&mut e, 3.0, 2.0);
    assert_eq!(e.model_tool().unwrap().picked, vec![r]);
    click(&mut e, 3.0, 2.0);
    assert!(
        e.model_tool().unwrap().picked.is_empty(),
        "second click removes"
    );
    click(&mut e, 3.0, 2.0);
    enter(&mut e);
    assert_eq!(count(&e, "zone"), 1);
    assert!(e.model.zone_of_room(r).unwrap().is_some());
    assert!(e.model_tool().unwrap().picked.is_empty());
    click(&mut e, 3.0, 2.0);
    assert!(
        e.model_tool().unwrap().picked.is_empty(),
        "a room already in a zone is refused"
    );
    undo_redo(&mut e, "zone", 0);
}

#[test]
fn zone_shows_in_layer_and_can_be_deleted() {
    let mut e = ed();
    let r = room(&mut e);
    let id = e
        .model
        .add_zone(&ZoneSpec {
            name: "Fire 1".into(),
            rooms: vec![r],
        })
        .unwrap();
    e.reload().unwrap();
    assert!(e.struct_fp.contains(code_of("zone").unwrap(), id));
    let mut l = Layer::default();
    l.sync(&e.model, e.level, 1);
    assert!(l.labels.iter().any(|x| x.1 == "Fire 1"));
    assert_eq!(
        crate::struct_props::values(&e.model, "zone", id).unwrap(),
        Vec::<f64>::new()
    );
    assert_eq!(delete_all(&mut e.model, &[("zone", id)]).unwrap(), 1);
    assert_eq!(count(&e, "zone"), 0);
    assert_eq!(count(&e, "room"), 1, "rooms stay");
}

#[test]
fn core_kinds_delete_together_and_roll_back() {
    let mut e = ed();
    let lv = e.level;
    let r = e.model.add_ramp(&free_ramp(lv)).unwrap();
    let s = e
        .model
        .add_room_sep(&SepSpec {
            level: lv,
            a: (0.0, 3.0),
            b: (3.0, 3.0),
        })
        .unwrap();
    assert_eq!(
        delete_all(&mut e.model, &[("ramp", r), ("room_sep", s)]).unwrap(),
        2
    );
    assert_eq!((count(&e, "ramp"), count(&e, "room_sep")), (0, 0));
    e.model.undo().unwrap();
    e.model.undo().unwrap();
    assert!(delete_all(&mut e.model, &[("ramp", r), ("room_sep", 9999)]).is_err());
    assert_eq!(count(&e, "ramp"), 1, "failure undoes the deleted ones");
}

#[test]
fn level_placed_by_elevation_keeps_active_level() {
    let mut e = ed();
    let cur = e.level;
    let before = count(&e, "level");
    let id = super::level_place(&mut e, 4.52).unwrap();
    assert_eq!(e.level, cur);
    let l = e.levels().into_iter().find(|l| l.0 == id).unwrap();
    assert!((l.2 - 4.5).abs() < 1e-9, "{}", l.2);
    assert!(l.1.starts_with(&crate::app2_fx::fx("tf2_floor")), "{}", l.1);
    assert!(super::level_place(&mut e, 4.5).is_none(), "taken");
    assert_eq!(count(&e, "level"), before + 1);
    e.undo();
    assert_eq!(count(&e, "level"), before, "one undo");
}

#[test]
fn void_opening_has_no_filler_and_stays_void() {
    let mut e = ed();
    walls(&mut e);
    e.start_opening(OpeningType::void());
    assert_eq!(e.otool.ty.kind, OpKind::Void);
    assert_eq!(e.otool.ty.sill, 0.0);
    click(&mut e, 3.0, 0.0);
    assert_eq!(count(&e, "opening"), 1);
    e.reload().unwrap();
    assert_eq!(e.openings[0].kind, OpKind::Void);
    assert_eq!(OpKind::from_model(OpKind::Void.model_name()), OpKind::Void);
    e.set_mode(Mode::Select);
    e.undo();
    assert_eq!(count(&e, "opening"), 0);
}

#[test]
fn ramp_and_separator_follow_move_copy_rotate_mirror() {
    let mut e = ed();
    let lv = e.level;
    let r = e.model.add_ramp(&free_ramp(lv)).unwrap();
    let s = e
        .model
        .add_room_sep(&SepSpec {
            level: lv,
            a: (0.0, 5.0),
            b: (4.0, 5.0),
        })
        .unwrap();
    e.reload().unwrap();
    let sel = |kind: &str, id| crate::editor::structx::sel(kind, id).unwrap();
    e.selection = vec![sel("ramp", r), sel("room_sep", s)];
    // move: everything moves, one undo, status without caveats
    let mut t = ModTool::new(Op::Move, &e.selection);
    assert_eq!(t.click(&mut e, v2(0.0, 0.0)), Out::Nothing);
    assert_eq!(t.click(&mut e, v2(1.0, 2.0)), Out::Exit);
    assert_eq!(e.model.ramp(r).unwrap().path[0], (1.0, 2.0));
    assert_eq!(e.model.room_sep(s).unwrap().a, (1.0, 7.0));
    assert!(e.status.as_deref().unwrap().ends_with(&crate::app2_fx::fx("tf2_done")));
    e.undo();
    assert_eq!(e.model.ramp(r).unwrap().path[0], (0.0, 0.0));
    assert_eq!(e.model.room_sep(s).unwrap().a, (0.0, 5.0));
    // copy
    let mut t = ModTool::new(Op::Copy, &e.selection);
    t.opts.multiple = false;
    t.click(&mut e, v2(0.0, 0.0));
    t.click(&mut e, v2(0.0, 10.0));
    assert_eq!((count(&e, "ramp"), count(&e, "room_sep")), (2, 2));
    e.undo();
    assert_eq!((count(&e, "ramp"), count(&e, "room_sep")), (1, 1));
    // rotate 90° about the origin
    let mut t = ModTool::new(Op::Rotate, &e.selection);
    t.click(&mut e, v2(0.0, 0.0));
    t.click(&mut e, v2(1.0, 0.0));
    assert_eq!(t.click(&mut e, v2(0.0, 1.0)), Out::Exit);
    let p = e.model.ramp(r).unwrap().path;
    assert!(p[1].0.abs() < 1e-9 && (p[1].1 - 8.0).abs() < 1e-9, "{p:?}");
    let q = e.model.room_sep(s).unwrap();
    assert!((q.a.0 + 5.0).abs() < 1e-9 && q.a.1.abs() < 1e-9, "{q:?}");
    e.undo();
    // mirror about x=10
    let mut t = ModTool::new(Op::MirrorDraw, &e.selection);
    t.opts.copy = false;
    t.click(&mut e, v2(10.0, 0.0));
    assert_eq!(t.click(&mut e, v2(10.0, 1.0)), Out::Exit);
    let p = e.model.ramp(r).unwrap().path;
    assert!(
        (p[0].0 - 20.0).abs() < 1e-9 && (p[1].0 - 12.0).abs() < 1e-9,
        "{p:?}"
    );
    assert!((e.model.room_sep(s).unwrap().b.0 - 16.0).abs() < 1e-9);
}

#[test]
fn modify_tools_never_claim_success_over_a_part_of_the_selection() {
    let mut e = ed();
    let r = room(&mut e);
    let zone = e
        .model
        .add_zone(&ZoneSpec {
            name: "Z".into(),
            rooms: vec![r],
        })
        .unwrap();
    e.reload().unwrap();
    assert!(crate::editor::structx::sel("zone", zone).is_some());
    // ddf8c071 made the zone a full family: a pipe fitting is a kind still refused by name
    let zone_sel = Sel::Ext(crate::scene_ids::EXT_FITTING, 1);
    // unsupported only: immediate refusal naming it, model untouched
    e.selection = vec![zone_sel];
    let rev = e.revision;
    let name = crate::tr(
        &format!("selcat.{}", crate::selection::category(&e, zone_sel)),
        &[],
    );
    for op in [Op::Move, Op::Copy, Op::Rotate, Op::MirrorDraw] {
        let mut t = ModTool::new(op, &e.selection);
        match t.click(&mut e, v2(0.0, 0.0)) {
            Out::Refused(m) => assert!(m.contains(&name), "{m}"),
            o => panic!("{op:?}: {o:?}"),
        }
    }
    assert_eq!(e.revision, rev);
    // mixed: the wall moves, the zone is named in the result, no "done" (Russian) marker
    let wall = e.walls[0].id;
    let a0 = e.walls[0].a;
    e.selection = vec![Sel::Wall(wall), zone_sel];
    let mut t = ModTool::new(Op::Move, &e.selection);
    t.click(&mut e, v2(0.0, 0.0));
    assert_eq!(t.click(&mut e, v2(0.0, -1.0)), Out::Exit);
    let moved = e.walls.iter().find(|w| w.id == wall).unwrap().a;
    assert!((moved - a0 - v2(0.0, -1.0)).len() < 1e-9);
    let st = e.status.clone().unwrap();
    assert!(st.contains(&name) && !st.contains(&crate::app2_fx::fx("tf2_done")), "{st}");
}

#[test]
fn broken_ramp_stays_selectable_and_names_itself_when_blocking_a_level() {
    let mut e = ed();
    let a = e.level;
    let b = e.model.add_level("Top", 0.5).unwrap();
    let id = e
        .model
        .add_ramp(&RampSpec::new(a, Some(b), vec![(0.0, 0.0), (6.0, 0.0)]))
        .unwrap();
    e.model
        .sql()
        .execute("UPDATE level SET elevation=-1 WHERE id=?1", [b])
        .unwrap();
    e.reload().unwrap();
    let code = code_of("ramp").unwrap();
    assert!(e.struct_fp.contains(code, id), "footprint from the axis");
    let mut l = Layer::default();
    l.sync(&e.model, e.level, 99);
    assert!(l.polys.iter().any(|p| p.0 == Cls::Stair && p.2));
    assert_eq!(l.labels.len(), 1, "marked in the plan");
    assert_eq!(l.labels[0].1, crate::tr("core_tools.ramp.broken_mark", &[]));
    // visible in warnings and in the human-readable text
    let w = e.model.ramp_warnings().unwrap();
    assert_eq!(w.len(), 1);
    assert!(w[0].message.contains("rise"), "{}", w[0].message);
    // deleting a level names the real reason
    assert!(!e.delete_level(b));
    let st = e.status.clone().unwrap();
    assert!(st.contains(&format!("#{id}")), "{st}");
}

#[test]
fn pick_walls_works_at_t_junctions_and_reports_the_cause() {
    let mut e = ed();
    wall_segs(
        &mut e,
        &[
            ((-2.0, 0.0), (8.0, 0.0)),
            ((6.0, 0.0), (6.0, 4.0)),
            ((0.0, 4.0), (6.0, 4.0)),
            ((0.0, 0.0), (0.0, 4.0)),
        ],
    );
    arm(&mut e, Kind::Floor, Lang::En);
    opts(&mut e).walls = true;
    opts(&mut e).overhang = 0.0;
    click(&mut e, 3.0, 0.0);
    {
        let a = e.model_tool().unwrap();
        let err = match super::floor::from_walls(&e, a) {
            Err(m) => m,
            Ok(_) => panic!("one wall"),
        };
        assert_eq!(err, crate::tr("tool.err.walls_few", &[]));
    }
    for (x, y) in [(6.0, 2.0), (3.0, 4.0), (0.0, 2.0)] {
        click(&mut e, x, y);
    }
    let ring = super::floor::walls_ring(&e, e.model_tool().unwrap()).unwrap();
    assert!(
        ring.iter().all(|p| p.x >= -1e-9 && p.x <= 6.0 + 1e-9),
        "{ring:?}"
    );
    enter(&mut e);
    assert_eq!(count(&e, "slab"), 1);
}

#[test]
fn arc_wall_gets_its_own_message_in_pick_walls() {
    let mut e = ed();
    let wall_type = e.current_type().unwrap().id;
    let spec = WallSpec {
        a: (0.0, 0.0),
        b: (4.0, 0.0),
        level: e.level,
        base_offset: 0.0,
        height: 3.0,
        wall_type,
        offset0: None,
    };
    e.model.add_arc_wall(&spec, 0.5).unwrap();
    e.reload().unwrap();
    arm(&mut e, Kind::Floor, Lang::En);
    opts(&mut e).walls = true;
    let want = Catalog::new(Lang::En).t("tool.err.pick_arc");
    let mut seen = vec![];
    for y in [1.0, -1.0] {
        click(&mut e, 2.0, y);
        seen.push(e.status.clone().unwrap_or_default());
    }
    assert!(seen.contains(&want), "{seen:?}");
    assert!(e.model_tool().unwrap().picked.is_empty());
}

#[test]
fn floor_slope_does_not_depend_on_click_order() {
    let ring = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
    let orders: [[usize; 4]; 3] = [[0, 1, 2, 3], [1, 2, 3, 0], [3, 2, 1, 0]];
    for order in orders {
        let mut e = ed();
        arm(&mut e, Kind::Floor, Lang::En);
        opts(&mut e).slope = 2.0;
        for i in order {
            click(&mut e, ring[i].0, ring[i].1);
        }
        enter(&mut e);
        let id: i64 = e
            .model
            .sql()
            .query_row("SELECT id FROM slab", [], |r| r.get(0))
            .unwrap();
        let pl = e.model.slab_slope(id).unwrap().unwrap();
        assert!(pl.rise(v2(0.0, 5.0)).abs() < 1e-9, "{order:?} {pl:?}");
        assert!(
            (pl.rise(v2(10.0, 5.0)) - 0.2).abs() < 1e-9,
            "{order:?} {pl:?}"
        );
    }
}

#[test]
fn floor_pick_ignores_room_separators_and_mode_change_drops_picks() {
    let mut e = ed();
    walls(&mut e);
    let level = e.level;
    e.model
        .add_room_sep(&SepSpec {
            level,
            a: (3.0, 0.0),
            b: (3.0, 4.0),
        })
        .unwrap();
    e.reload().unwrap();
    let half = super::ceiling::region_at(&e, v2(1.0, 2.0)).unwrap();
    assert!(half.outer.iter().all(|p| p.x <= 3.0 + 1e-9));
    let hall = super::floor::region_at(&e, v2(1.0, 2.0)).unwrap();
    assert!(hall.outer.iter().any(|p| p.x > 5.0), "the whole wall loop");

    arm(&mut e, Kind::Floor, Lang::En);
    opts(&mut e).walls = true;
    click(&mut e, 3.0, 4.0);
    let a = e.model_tool_mut().unwrap();
    assert_eq!(a.picked.len(), 1);
    super::floor::mode_changed(&mut a.picked, &mut a.pts);
    assert!(a.picked.is_empty() && a.pts.is_empty());
}

#[test]
fn level_names_follow_the_level_below() {
    let mut e = ed();
    e.model.add_level("L01", 3.0).unwrap();
    e.reload().unwrap();
    assert_eq!(e.next_level_name(6.0), "L02");
    let below = crate::tr("level.auto_below", &[("n", "1")]);
    assert_eq!(e.next_level_name(-3.0), below);
    e.model.add_level("Roof", 9.0).unwrap();
    e.model.add_level("L02", 6.0).unwrap();
    e.reload().unwrap();
    assert_eq!(e.next_level_name(4.0), "L03", "L02 is taken");
    let auto = crate::tr("level.auto_name", &[("n", "5")]);
    assert_eq!(e.next_level_name(12.0), auto, "no number to continue");
    assert_eq!(e.next_level_defaults().1, 12.0);
}

#[test]
fn void_is_its_own_category_and_is_picked_by_ray() {
    let mut e = ed();
    walls(&mut e);
    e.start_opening(OpeningType::void());
    click(&mut e, 3.0, 0.0);
    e.set_mode(Mode::Select);
    e.reload().unwrap();
    let o = e.openings[0].clone();
    assert_eq!(
        crate::selection::category(&e, Sel::Opening(o.id)),
        "opening"
    );
    assert!(e
        .model
        .untagged(e.level, &[TagCat::Window])
        .unwrap()
        .is_empty());
    assert_eq!(crate::view3d::sel_category(&e, Sel::Opening(o.id)), "other");
    // ray across the wall: through the opening, above it, and when a body is closer
    let h = e.host_of(o.wall).unwrap();
    let from = h.at(o.along + o.width / 2.0, -5.0);
    let n = h.normal();
    let dir = [n.x, n.y, 0.0];
    let z = o.sill + o.height / 2.0;
    let ray = |z: f64, limit: f64| {
        crate::opening_tool::void_under_ray(&e, [from.x, from.y, z], dir, limit)
    };
    let el = ray(z, f64::MAX).expect("hit");
    assert_eq!(e.sel_of_element(el), Some(Sel::Opening(o.id)));
    assert_eq!(ray(o.sill + o.height + 0.1, f64::MAX), None);
    assert_eq!(ray(z, 1.0), None);
}

#[test]
fn zone_rooms_can_be_edited_and_the_name_does_not_stick() {
    let mut e = ed();
    let r = room(&mut e);
    assert_eq!(super::core_items::free_rooms(&e.model).len(), e.rooms.len());
    arm(&mut e, Kind::Zone, Lang::En);
    opts(&mut e).name = "A".into();
    click(&mut e, 3.0, 2.0);
    enter(&mut e);
    assert_eq!(count(&e, "zone"), 1);
    assert!(opts(&mut e).name.is_empty());
    assert!(!super::core_items::free_rooms(&e.model)
        .iter()
        .any(|x| x.0 == r));
}

#[test]
fn ramp_is_listed_next_to_the_other_categories() {
    use crate::schedule::{build, count as sched_count, Cat};
    let mut e = ed();
    let lv = e.level;
    e.model.add_ramp(&free_ramp(lv)).unwrap();
    e.reload().unwrap();
    assert_eq!(sched_count(&e, Cat::Ramps), 1);
    assert_eq!(build(&e, Cat::Ramps).rows.len(), 1);
    assert_eq!(crate::phase_ui::kind_name("ramp", true), crate::app2_fx::fx("tf2_ramps"));
    assert!(ddc_model::PARAM_CATEGORIES.iter().any(|c| c.0 == "ramp"));
}

#[test]
fn f2_keys_resolve_ru_en() {
    let keys = [
        "tool.floor",
        "tool.struct_slab",
        "tool.ramp",
        "tool.room_sep",
        "tool.zone",
        "tool.zone.name",
        "hint.floor.pick",
        "hint.floor.walls",
        "hint.floor.next",
        "hint.struct_slab.pick",
        "hint.struct_slab.walls",
        "hint.struct_slab.next",
        "hint.ramp.next",
        "hint.room_sep.end",
        "hint.zone.place",
        "hint.level.place",
        "selcat.ramp",
        "selcat.room_sep",
        "selcat.zone",
        "tool.err.zone_taken",
        "tool.err.level_taken",
        "core_tools.mode.contour",
        "core_tools.mode.pick",
        "core_tools.mode.walls",
        "core_tools.offset",
        "core_tools.sketch",
        "core_tools.landing",
        "core_tools.max_run",
        "core_tools.to_top",
        "core_tools.height",
        "core_tools.ramp_bad",
        "core_tools.run",
        "core_tools.rise",
        "core_tools.landings",
        "core_tools.rooms",
        "core_tools.zone.name",
        "core_tools.void",
        "core_tools.sep.ax",
        "core_tools.sep.ay",
        "core_tools.sep.bx",
        "core_tools.sep.by",
        "mt.unsupported",
        "mt.partial",
        "tool.err.pick_arc",
        "tool.err.walls_few",
        "tool.err.walls_open",
        "tool.err.walls_branch",
        "tool.err.walls_stray",
        "tool.err.walls_flips",
        "core_tools.ramp.broken_mark",
        "core_tools.ramp.top",
        "core_tools.ramp.top_none",
        "warn.ramp_broken",
        "warn.zone_empty",
        "core_tools.zone.remove",
        "core_tools.zone.add",
        "core_tools.type.struct_slab",
        "core_tools.type.floor",
        "core_tools.browser.ramp",
        "core_tools.browser.sep",
        "core_tools.browser.select",
        "core_tools.browser.delete",
        "level.auto_name",
        "level.auto_below",
        "level.blocked",
        "selcat.opening",
        "opt.direction",
        "opt.top_off",
    ];
    for lang in [Lang::Ru, Lang::En] {
        let c = Catalog::new(lang);
        for k in keys {
            assert_ne!(c.t(k), k, "{k} missing");
        }
    }
}

fn drawing(e: &mut Editor, rects: &[[f64; 4]]) {
    let mut segs = vec![];
    for r in rects {
        let c = [v2(r[0], r[1]), v2(r[2], r[1]), v2(r[2], r[3]), v2(r[0], r[3])];
        segs.extend((0..4).map(|i| (c[i], c[(i + 1) % 4])));
    }
    e.tool.underlay_segs = Arc::new(segs);
}

fn num(e: &Editor, sql: &str) -> f64 {
    e.model.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn floor_from_closed_drawing_loop() {
    let mut e = ed();
    drawing(&mut e, &[[10.0, 10.0, 16.0, 14.0], [0.0, 0.0, 40.0, 40.0]]);
    arm(&mut e, Kind::Floor, Lang::En);
    opts(&mut e).pick = true;
    click(&mut e, 12.0, 12.0);
    assert_eq!(count(&e, "slab"), 1);
    assert_eq!(num(&e, "SELECT MAX(x) FROM slab_vertex"), 16.0, "the smallest loop");
    assert_eq!(num(&e, "SELECT MIN(y) FROM slab_vertex"), 10.0);
    undo_redo(&mut e, "slab", 0);
}

#[test]
fn ceiling_from_drawing_loop_with_hole() {
    let mut e = ed();
    drawing(&mut e, &[[0.0, 0.0, 10.0, 10.0], [2.0, 2.0, 4.0, 4.0]]);
    arm(&mut e, Kind::Ceiling, Lang::En);
    opts(&mut e).pick = true;
    click(&mut e, 7.0, 7.0);
    assert_eq!(count(&e, "ceiling"), 1);
    assert_eq!(num(&e, "SELECT COUNT(DISTINCT ring) FROM ceiling_vertex"), 2.0);
    undo_redo(&mut e, "ceiling", 0);
    // undo_redo ends on redo: outline and hole are back
    assert_eq!(num(&e, "SELECT COUNT(DISTINCT ring) FROM ceiling_vertex"), 2.0);
}

#[test]
fn open_drawing_contour_refused() {
    let mut e = ed();
    drawing(&mut e, &[[0.0, 0.0, 10.0, 10.0]]);
    let mut segs = (*e.tool.underlay_segs).clone();
    segs.pop();
    e.tool.underlay_segs = Arc::new(segs);
    arm(&mut e, Kind::Floor, Lang::En);
    opts(&mut e).pick = true;
    click(&mut e, 5.0, 5.0);
    assert_eq!(count(&e, "slab"), 0);
    assert_eq!(
        e.status.as_deref(),
        Some("The drawing lines around this point do not close into a loop")
    );
    let bow = [
        (v2(0.0, 0.0), v2(4.0, 4.0)),
        (v2(4.0, 4.0), v2(4.0, 0.0)),
        (v2(4.0, 0.0), v2(0.0, 4.0)),
        (v2(0.0, 4.0), v2(0.0, 0.0)),
    ];
    e.tool.underlay_segs = Arc::new(bow.to_vec());
    click(&mut e, 3.0, 2.0);
    assert_eq!(count(&e, "slab"), 0);
    assert_eq!(
        e.status.as_deref(),
        Some("The drawing loop around this point crosses itself")
    );
    drawing(&mut e, &[[0.0, 0.0, 10.0, 10.0]]);
    click(&mut e, 50.0, 50.0);
    assert_eq!(count(&e, "slab"), 0);
    assert_eq!(
        e.status.as_deref(),
        Some("No closed wall loop under the cursor")
    );
}

#[test]
fn wall_region_wins_over_drawing() {
    let mut e = ed();
    walls(&mut e);
    drawing(&mut e, &[[1.0, 1.0, 2.0, 3.0]]);
    arm(&mut e, Kind::Floor, Lang::En);
    opts(&mut e).pick = true;
    click(&mut e, 1.5, 2.0);
    assert_eq!(count(&e, "slab"), 1);
    assert!(num(&e, "SELECT MAX(x) FROM slab_vertex") > 5.0, "wall loop, not the drawing");
}

fn roof_ring(e: &Editor) -> (Vec<(f64, f64)>, Vec<Option<f64>>, f64) {
    let id: i64 = e
        .model
        .sql()
        .query_row("SELECT id FROM roof", [], |r| r.get(0))
        .unwrap();
    let r = e.model.roof(id).unwrap();
    let ddc_model::RoofKind::Footprint { ring, slopes, .. } = r.kind else {
        panic!("footprint roof")
    };
    (ring, slopes, r.overhang)
}

/// Outer faces of the 6x4 test box: the wall type's half thickness beyond the axes.
fn assert_outer_faces(e: &Editor, ring: &[(f64, f64)]) {
    let h = e.current_type().unwrap().thickness() / 2.0;
    assert_eq!(ring.len(), 4);
    let (x0, x1) = ring.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
    let (y0, y1) = ring.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
    for (got, want) in [(x0, -h), (x1, 6.0 + h), (y0, -h), (y1, 4.0 + h)] {
        assert!((got - want).abs() < 1e-6, "{got} vs {want}");
    }
}

fn pick_four(e: &mut Editor) {
    for (x, y) in [(3.0, 0.0), (6.0, 2.0), (3.0, 4.0), (0.0, 2.0)] {
        click(e, x, y);
    }
}

#[test]
fn roof_from_picked_walls_with_overhang() {
    let mut e = ed();
    walls(&mut e);
    arm(&mut e, Kind::Roof, Lang::En);
    opts(&mut e).walls = true;
    opts(&mut e).overhang = 0.5;
    assert_eq!(e.model_tool().unwrap().step(), "walls");
    pick_four(&mut e);
    enter(&mut e);
    assert_eq!(count(&e, "roof"), 1);
    let (ring, _, overhang) = roof_ring(&e);
    assert_outer_faces(&e, &ring);
    assert!((overhang - 0.5).abs() < 1e-9, "overhang is applied by the roof");
    assert!(e.model_tool().unwrap().picked.is_empty());
}

#[test]
fn roof_click_inside_region() {
    let mut e = ed();
    walls(&mut e);
    arm(&mut e, Kind::Roof, Lang::En);
    opts(&mut e).pick = true;
    assert_eq!(e.model_tool().unwrap().step(), "pick");
    click(&mut e, 20.0, 20.0);
    assert_eq!(count(&e, "roof"), 0, "outside any wall loop");
    click(&mut e, 3.0, 2.0);
    assert_eq!(count(&e, "roof"), 1);
    assert_outer_faces(&e, &roof_ring(&e).0);
}

#[test]
fn roof_open_wall_chain_refused() {
    let mut e = ed();
    walls(&mut e);
    arm(&mut e, Kind::Roof, Lang::En);
    opts(&mut e).walls = true;
    for (x, y) in [(3.0, 0.0), (6.0, 2.0), (3.0, 4.0)] {
        click(&mut e, x, y);
    }
    enter(&mut e);
    assert_eq!(count(&e, "roof"), 0);
    let msg = e.status.clone().unwrap_or_default();
    assert!(msg.contains("(0.00; 4.00)") || msg.contains("(0.00; 0.00)"), "names the gap: {msg}");
    assert_eq!(e.model_tool().unwrap().picked.len(), 3, "picks stay");
}

#[test]
fn roof_edges_all_sloped_space_toggles() {
    let mut e = ed();
    walls(&mut e);
    arm(&mut e, Kind::Roof, Lang::En);
    opts(&mut e).pick = true;
    click(&mut e, 3.0, 2.0);
    let (_, slopes, _) = roof_ring(&e);
    assert!(slopes.iter().all(|s| s.is_some()), "every edge sloped by default");
    // second roof: Space near the bottom edge makes it a gable
    e.handle(Event::Move(v2(3.0, 0.6)));
    e.handle(Event::Char(' '));
    click(&mut e, 3.0, 0.6);
    assert_eq!(count(&e, "roof"), 2);
    let id: i64 = e
        .model
        .sql()
        .query_row("SELECT MAX(id) FROM roof", [], |r| r.get(0))
        .unwrap();
    let ddc_model::RoofKind::Footprint { ring, slopes, .. } = e.model.roof(id).unwrap().kind else {
        panic!("footprint roof")
    };
    let flat: Vec<usize> = (0..slopes.len()).filter(|i| slopes[*i].is_none()).collect();
    assert_eq!(flat.len(), 1);
    let (p, q) = (ring[flat[0]], ring[(flat[0] + 1) % ring.len()]);
    let h = e.current_type().unwrap().thickness() / 2.0;
    assert!((p.1 + h).abs() < 1e-6 && (q.1 + h).abs() < 1e-6, "the hovered bottom edge");
}

#[test]
fn roof_pick_one_undo() {
    let mut e = ed();
    walls(&mut e);
    arm(&mut e, Kind::Roof, Lang::En);
    opts(&mut e).pick = true;
    click(&mut e, 3.0, 2.0);
    undo_redo(&mut e, "roof", 0);
}

/// i18n_default_type_names: with the English UI no default type name has a Cyrillic letter, and
/// with the Russian UI every one that carries a word has one.
#[test]
fn default_type_names_follow_the_ui_language() {
    let cyr = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c));
    let kinds = [
        Kind::Column,
        Kind::Roof,
        Kind::Ceiling,
        Kind::Stair,
        Kind::Railing,
        Kind::Shaft,
        Kind::Grid,
        Kind::RefPlane,
    ];
    for k in kinds {
        for (lang, want_ru) in [(Lang::En, false), (Lang::Ru, true)] {
            let mut e = ed();
            arm(&mut e, k, lang);
            let names = e.model_tool().unwrap().types.clone();
            assert!(!names.is_empty(), "{k:?}");
            for n in &names {
                assert_eq!(cyr(n), want_ru, "{k:?} {lang:?}: {n:?}");
            }
        }
    }
    // an English column type still gets its section, and the stored Russian name keeps working
    let mut e = ed();
    arm(&mut e, Kind::Column, Lang::En);
    let a = e.model_tool().unwrap();
    assert!(a.type_name().starts_with("Column "), "{}", a.type_name());
    assert!(super::column::arch_shape(a.type_name()).is_some());
    assert!(super::column::arch_shape("\u{41a}\u{43e}\u{43b}\u{43e}\u{43d}\u{43d}\u{430} 400×400").is_some());
    assert!(super::column::arch_shape("IPE 200").is_none());
}
