//! Grid lines and reference planes: pick on the line, edit, move, copy, delete, one undo each.

use crate::editor::{structx, Editor, OpFn, Sel};
use crate::struct_props::{apply, fields, rename_datum, values};
use crate::wall_tool::{Event, Mode};
use ddc_geom::v2;
use ddc_model::{GridSpec, Model, Target};
use std::sync::Arc;

fn editor() -> Editor {
    let mut e = Editor::new_in_memory().unwrap();
    e.set_mode(Mode::Select);
    e
}

fn sel(kind: &str, id: i64) -> Sel {
    structx::sel(kind, id).unwrap()
}

fn click(e: &mut Editor, x: f64, y: f64) -> Vec<Sel> {
    e.selection.clear();
    e.handle(Event::Click(v2(x, y)));
    e.selection.clone()
}

#[test]
fn click_picks_the_line_and_nothing_beside_it() {
    let mut e = editor();
    let g = e.model.add_grid(&GridSpec::line("A", (3.0, 0.0), (3.0, 10.0))).unwrap();
    let r = e.model.add_ref_plane("p", (6.0, 0.0), (6.0, 10.0)).unwrap();
    e.reload().unwrap();
    assert_eq!(click(&mut e, 3.0, 5.0), vec![sel("grid", g)]);
    assert_eq!(click(&mut e, 6.02, 5.0), vec![sel("ref_plane", r)]);
    assert!(click(&mut e, 4.5, 5.0).is_empty());
}

#[test]
fn an_arc_grid_is_picked_on_the_arc_not_inside_it() {
    let mut e = editor();
    let spec = GridSpec {
        bulge: 1.0,
        ..GridSpec::line("R", (0.0, 0.0), (4.0, 0.0))
    };
    let g = e.model.add_grid(&spec).unwrap();
    e.reload().unwrap();
    let pts = spec.curve().points(0.25);
    let top = pts[pts.len() / 2];
    assert_eq!(click(&mut e, top.x, top.y), vec![sel("grid", g)]);
    let chord_mid = (spec.a.0 + spec.b.0) / 2.0;
    assert!(click(&mut e, chord_mid, 0.0).is_empty());
}

#[test]
fn a_grid_outside_the_level_range_is_not_pickable() {
    let mut e = editor();
    let upper = e.model.add_level("U", 6.0).unwrap();
    let mut s = GridSpec::line("H", (1.0, 0.0), (1.0, 5.0));
    s.base_level = Some(upper);
    let g = e.model.add_grid(&s).unwrap();
    e.reload().unwrap();
    assert!(!e.struct_fp.contains(structx::code_of("grid").unwrap(), g));
}

#[test]
fn delete_is_one_undo_step_and_selection_clears() {
    let mut e = editor();
    let g = e.model.add_grid(&GridSpec::line("A", (3.0, 0.0), (3.0, 10.0))).unwrap();
    let r = e.model.add_ref_plane("p", (6.0, 0.0), (6.0, 10.0)).unwrap();
    e.reload().unwrap();
    e.selection = vec![sel("grid", g), sel("ref_plane", r)];
    e.delete_selection();
    assert!(e.model.grids().unwrap().is_empty());
    assert!(e.model.ref_planes().unwrap().is_empty());
    e.undo();
    assert_eq!(e.model.grids().unwrap().len(), 1);
    assert_eq!(e.model.ref_planes().unwrap().len(), 1);
}

#[test]
fn whole_axis_move_follows_the_pick_outline_and_undoes() {
    let mut e = editor();
    let g = e.model.add_grid(&GridSpec::line("A", (3.0, 0.0), (3.0, 10.0))).unwrap();
    e.reload().unwrap();
    let f = super::of_sel(sel("grid", g)).unwrap();
    let super::Cap::Yes(target) = f.target else {
        panic!("grid has no Target")
    };
    let t = target(g);
    assert_eq!(t, Target::Grid(g));
    e.run_op(OpFn(Arc::new(move |m: &mut Model| m.move_elements(&[t], (2.0, 0.0)))))
        .unwrap();
    let code = structx::code_of("grid").unwrap();
    assert_eq!(e.struct_fp.rings(code, g)[0][0], v2(5.0, 0.0));
    e.undo();
    assert_eq!(e.struct_fp.rings(code, g)[0][0], v2(3.0, 0.0));
}

#[test]
fn panel_fields_and_rename_are_single_undo_steps() {
    let mut e = editor();
    let g = e.model.add_grid(&GridSpec::line("A", (3.0, 0.0), (3.0, 10.0))).unwrap();
    let r = e.model.add_ref_plane("p", (6.0, 0.0), (6.0, 10.0)).unwrap();
    for (kind, id) in [("grid", g), ("ref_plane", r)] {
        let mut v = values(&e.model, kind, id).unwrap();
        assert_eq!(v.len(), fields(kind).len());
        v[0] += 1.5;
        v[2] += 1.5;
        let vals = v.clone();
        e.run_op(OpFn(Arc::new(move |m: &mut Model| apply(m, kind, id, &vals))))
            .unwrap();
        assert_eq!(values(&e.model, kind, id).unwrap(), v);
        e.undo();
        assert_eq!(values(&e.model, kind, id).unwrap()[0], v[0] - 1.5);
    }
    e.run_op(OpFn(Arc::new(move |m: &mut Model| rename_datum(m, "grid", g, "Z"))))
        .unwrap();
    assert_eq!(e.model.grid(g).unwrap().name, "Z");
    e.run_op(OpFn(Arc::new(move |m: &mut Model| rename_datum(m, "ref_plane", r, "Q"))))
        .unwrap();
    assert_eq!(e.model.ref_plane(r).unwrap().0, "Q");
    e.undo();
    e.undo();
    assert_eq!(e.model.grid(g).unwrap().name, "A");
    assert_eq!(e.model.ref_plane(r).unwrap().0, "p");
}

#[test]
fn a_taken_grid_name_is_refused_without_a_change() {
    let mut e = editor();
    e.model.add_grid(&GridSpec::line("A", (0.0, 0.0), (0.0, 5.0))).unwrap();
    let b = e.model.add_grid(&GridSpec::line("B", (4.0, 0.0), (4.0, 5.0))).unwrap();
    let head = e.model.change_head();
    let r = e.run_op(OpFn(Arc::new(move |m: &mut Model| rename_datum(m, "grid", b, "A"))));
    assert!(r.is_err());
    assert_eq!(e.model.change_head(), head);
}
