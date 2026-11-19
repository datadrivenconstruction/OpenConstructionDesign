// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Terrain scenarios: scripted plan input → model state; one action is one undo step.

use super::*;
use crate::editor::Editor;
use crate::wall_tool::{Event, Key, Mode};
use ddc_geom::v2;
use ddc_ui::i18n::Lang;

fn ed() -> Editor {
    Editor::new_in_memory().unwrap()
}

fn click(e: &mut Editor, x: f64, y: f64) {
    e.handle(Event::Move(v2(x, y)));
    e.handle(Event::Click(v2(x, y)));
}

fn typed(e: &mut Editor, s: &str) {
    for c in s.chars() {
        e.handle(Event::Char(c));
    }
    e.handle(Event::Key(Key::Enter));
}

/// Sloped plane z = 0.1·x on a 0..20 m grid with 5 m step.
fn plane(e: &mut Editor) -> i64 {
    let mut pts = vec![];
    for i in 0..=4 {
        for j in 0..=4 {
            let (x, y) = (i as f64 * 5.0, j as f64 * 5.0);
            pts.push((x, y, 0.1 * x));
        }
    }
    let id = e
        .model
        .add_toposolid(&ddc_model::TopoSpec {
            name: "T".into(),
            thickness: 1.0,
            material: EARTH.into(),
            points: pts,
            breaklines: vec![],
            boundary: vec![],
        })
        .unwrap();
    e.reload().unwrap();
    id
}

fn n_topo(e: &Editor) -> usize {
    e.model.toposolids().unwrap().len()
}

#[test]
fn click_points_create_one_toposolid_in_one_undo_step() {
    let mut e = ed();
    arm(&mut e, Tool::Points, Lang::Ru);
    typed(&mut e, "12.5");
    assert_eq!(e.topo.opts.elevation, 12.5);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 10.0, 0.0);
    click(&mut e, 0.0, 10.0);
    e.handle(Event::Key(Key::Enter));
    assert_eq!(n_topo(&e), 1);
    let id = e.topo.active.expect("new toposolid is active");
    let pts = e.model.topo_points(id).unwrap();
    assert_eq!(pts.len(), 3);
    assert!(pts.iter().all(|p| (p.1 .2 - 12.5).abs() < 1e-12));
    e.handle(Event::Key(Key::Escape));
    assert!(e.topo.tool.is_none());
    e.undo();
    assert_eq!(n_topo(&e), 0, "one undo removes the toposolid");
    e.redo();
    assert_eq!(n_topo(&e), 1, "redo restores it");
}

#[test]
fn too_few_points_do_not_create() {
    let mut e = ed();
    arm(&mut e, Tool::Points, Lang::Ru);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 1.0, 0.0);
    e.handle(Event::Key(Key::Enter));
    assert_eq!(n_topo(&e), 0);
    assert!(e.status.is_some());
}

#[test]
fn loop_tools_need_a_toposolid() {
    let mut e = ed();
    arm(&mut e, Tool::Pad, Lang::Ru);
    assert!(e.topo.tool.is_none(), "no toposolid — tool does not arm");
}

#[test]
fn pad_tool_commit_reports_and_undoes() {
    let mut e = ed();
    let id = plane(&mut e);
    arm(&mut e, Tool::Pad, Lang::Ru);
    typed(&mut e, "0.5");
    assert_eq!(e.topo.opts.pad_elevation, 0.5);
    for (x, y) in [(6.0, 6.0), (14.0, 6.0), (14.0, 14.0), (6.0, 14.0)] {
        click(&mut e, x, y);
    }
    e.handle(Event::Key(Key::Enter));
    let pads = e.model.pads(id).unwrap();
    assert_eq!(pads.len(), 1);
    assert_eq!(pads[0].1.outline.len(), 4);
    assert!(e.status.is_some(), "cut/fill report in the status");
    super::disarm(&mut e);
    e.undo();
    assert!(
        e.model.pads(id).unwrap().is_empty(),
        "one undo removes the pad"
    );
}

#[test]
fn loop_closes_on_first_point() {
    let mut e = ed();
    let id = plane(&mut e);
    arm(&mut e, Tool::Region, Lang::Ru);
    e.topo.opts.material = 1;
    for (x, y) in [(2.0, 2.0), (8.0, 2.0), (8.0, 8.0), (2.0, 2.0)] {
        click(&mut e, x, y);
    }
    let r = e.model.topo_regions(id).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].material, "asphalt");
    assert_eq!(r[0].ring.len(), 3);
}

#[test]
fn edit_point_z_by_typed_value_and_drag() {
    let mut e = ed();
    let id = plane(&mut e);
    arm(&mut e, Tool::Edit, Lang::Ru);
    click(&mut e, 5.0, 5.0);
    assert_eq!(e.topo.picked.len(), 1);
    let idx = e.topo.picked[0];
    typed(&mut e, "3");
    let z = |e: &Editor| {
        e.model
            .topo_points(id)
            .unwrap()
            .into_iter()
            .find(|p| p.0 == idx)
            .unwrap()
            .1
    };
    assert!((z(&e).2 - 3.0).abs() < 1e-12);
    // dragging in plan moves only xy
    e.handle(Event::Move(v2(5.0, 5.0)));
    e.handle(Event::Press(v2(5.0, 5.0)));
    e.handle(Event::Move(v2(6.0, 5.5)));
    e.handle(Event::Release(v2(6.0, 5.5)));
    let p = z(&e);
    assert!((p.0 - 6.0).abs() < 1e-9 && (p.1 - 5.5).abs() < 1e-9 && (p.2 - 3.0).abs() < 1e-12);
    super::disarm(&mut e);
    e.undo();
    assert!((z(&e).0 - 5.0).abs() < 1e-9, "undo the move");
    e.undo();
    assert!((z(&e).2 - 0.5).abs() < 1e-12, "undo the Z edit");
}

#[test]
fn delete_picked_points_is_one_step() {
    let mut e = ed();
    let id = plane(&mut e);
    arm(&mut e, Tool::Edit, Lang::Ru);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 20.0, 20.0);
    assert_eq!(e.topo.picked.len(), 2);
    e.delete_selection();
    assert_eq!(e.model.topo_points(id).unwrap().len(), 23);
    super::disarm(&mut e);
    e.undo();
    assert_eq!(e.model.topo_points(id).unwrap().len(), 25);
}

#[test]
fn contour_cache_follows_revision() {
    let mut e = ed();
    let id = plane(&mut e);
    refresh(&mut e);
    let c = e.topo.cache.as_ref().unwrap();
    assert_eq!(c.rev, e.cache_key());
    let it = c.items.iter().find(|i| i.id == id).unwrap();
    assert!(
        !it.contours.is_empty(),
        "plane 0..2 m with 0.5 m step has contours"
    );
    assert!(it.contours.iter().any(|c| c.major));
    let before = (c.rev, it.contours.len());
    e.run_op(op(move |m| m.add_topo_points(id, &[(12.0, 8.0, 6.0)])))
        .unwrap();
    refresh(&mut e);
    let c = e.topo.cache.as_ref().unwrap();
    assert_eq!(c.rev, e.cache_key());
    assert_ne!(c.rev, before.0);
    let it = c.items.iter().find(|i| i.id == id).unwrap();
    assert_ne!(it.contours.len(), before.1, "the peak adds contour levels");
    // changing the interval without editing the model also rebuilds
    e.topo.opts.interval = 0.25;
    refresh(&mut e);
    assert_eq!(e.topo.cache.as_ref().unwrap().key.0, 0.25f64.to_bits());
}

#[test]
fn contour_cache_follows_a_write_that_kept_the_revision() {
    let mut e = ed();
    let id = plane(&mut e);
    refresh(&mut e);
    let count = |e: &Editor| {
        let c = e.topo.cache.as_ref().unwrap();
        c.items.iter().find(|i| i.id == id).unwrap().points.len()
    };
    let n = count(&e);
    let rev = e.revision;
    e.model.add_topo_points(id, &[(12.0, 8.0, 6.0)]).unwrap();
    assert_eq!(e.revision, rev);
    refresh(&mut e);
    assert_eq!(count(&e), n + 1, "a swap or undo to an equal revision rebuilds the cache");
}

#[test]
fn scene_gets_terrain_meshes() {
    let mut e = ed();
    plane(&mut e);
    let mut v = vec![];
    append_scene(&e, &mut v);
    assert!(!v.is_empty());
    assert!(v.iter().all(|m| m.element == TOPO_ELEMENT));
    assert!(v.iter().all(|m| !m.triangles.is_empty()));
    assert_eq!(e.sel_of_element(TOPO_ELEMENT), None);
    let all = e.scene_all();
    assert!(all.meshes.iter().any(|m| m.element == TOPO_ELEMENT));
}

#[test]
fn tags_by_material() {
    assert_eq!(tag_of("grass", false), 21);
    assert_eq!(tag_of("earth", false), 20);
    assert_eq!(tag_of("grass", true), ddc_render::TAG_SLAB);
}

#[test]
fn other_tool_disarms_topo() {
    let mut e = ed();
    arm(&mut e, Tool::Points, Lang::En);
    click(&mut e, 1.0, 1.0);
    assert_eq!(command(&e), Some("site.toposolid"));
    assert_eq!(hint_pair(&e), Some(("topo.points", "next")));
    e.set_mode(Mode::Polyline);
    assert!(e.topo.tool.is_none(), "the wall tool takes over");
    assert!(e.topo.pts.is_empty());
    assert!(
        !handle(&mut e, Event::Click(v2(0.0, 0.0))),
        "events go to the wall tool"
    );
}

#[test]
fn undo_while_drawing_removes_last_point() {
    let mut e = ed();
    plane(&mut e);
    arm(&mut e, Tool::Boundary, Lang::Ru);
    click(&mut e, 1.0, 1.0);
    click(&mut e, 5.0, 1.0);
    e.undo();
    assert_eq!(e.topo.pts.len(), 1);
    assert_eq!(n_topo(&e), 1, "the model is untouched");
}

#[test]
fn flatten_without_change_adds_no_undo_step() {
    let mut e = ed();
    let id = plane(&mut e);
    e.run_op(op(move |m| m.add_topo_points(id, &[(12.0, 12.0, 3.0)])))
        .unwrap();
    arm(&mut e, Tool::Flatten, Lang::Ru);
    typed(&mut e, "0");
    // column x = 0 is already at elevation 0
    for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 21.0), (-1.0, 21.0)] {
        click(&mut e, x, y);
    }
    e.handle(Event::Key(Key::Enter));
    assert!(e.topo.pts.is_empty());
    super::disarm(&mut e);
    e.undo();
    assert_eq!(
        e.model.topo_points(id).unwrap().len(),
        25,
        "undo removes the added point"
    );
    assert!(!e.can_undo(), "the no-op flatten left no undo step");
}

fn src(points: Vec<(f64, f64, f64)>) -> ddc_io::topo::TopoSource {
    ddc_io::topo::TopoSource {
        points,
        ..Default::default()
    }
}

#[test]
fn georeferenced_survey_moves_to_project_origin() {
    let site = ddc_model::SiteProps::default();
    assert_eq!(
        survey_shift(&src(vec![(10.0, 20.0, 1.0)]), &site),
        None,
        "local survey stays"
    );
    let mut s = src(vec![
        (512345.6, 6123456.7, 101.0),
        (512400.0, 6123500.0, 102.0),
    ]);
    let sh = survey_shift(&s, &site).unwrap();
    assert!(sh.new_base);
    assert_eq!(sh.origin, (512300.0, 6123400.0, 0.0));
    apply_shift(&mut s, &sh);
    assert!((s.points[0].0 - 45.6).abs() < 1e-6 && (s.points[0].1 - 56.7).abs() < 1e-6);
    assert_eq!(s.points[0].2, 101.0);
    // explicit base point and north at 90°: survey north runs along project −X
    let based = ddc_model::SiteProps {
        north_deg: 90.0,
        base: (512000.0, 6123000.0, 100.0),
        ..Default::default()
    };
    let mut t = src(vec![(512000.0, 6123010.0, 101.0)]);
    let sh = survey_shift(&t, &based).unwrap();
    assert!(!sh.new_base);
    apply_shift(&mut t, &sh);
    let p = t.points[0];
    assert!((p.0 + 10.0).abs() < 1e-9 && p.1.abs() < 1e-9 && (p.2 - 1.0).abs() < 1e-12);
}

#[test]
fn import_far_survey_sets_base_point_in_one_undo_step() {
    let mut e = ed();
    let pts = vec![
        (512300.0, 6123400.0, 100.0),
        (512350.0, 6123400.0, 101.0),
        (512300.0, 6123450.0, 102.0),
    ];
    import_source(&mut e, "Survey", src(pts));
    let id = e.topo.active.expect("imported toposolid is active");
    let p = e.model.topo_points(id).unwrap();
    assert!(
        p.iter()
            .all(|q| q.1 .0.abs() < 100.0 && q.1 .1.abs() < 100.0),
        "near the origin"
    );
    let base = e.model.site().unwrap().base;
    assert_eq!((base.0, base.1), (512300.0, 6123400.0));
    e.undo();
    assert_eq!(n_topo(&e), 0);
    assert_eq!(
        e.model.site().unwrap().base.0,
        0.0,
        "one undo also restores the site"
    );
}

#[test]
fn contours_toggle_says_what_happened() {
    let mut e = ed();
    toggle_contours(&mut e);
    assert!(e.topo.hide_contours);
    assert_eq!(e.status.as_deref(), Some(crate::tr("sketch_hints.contours_none", &[]).as_str()));
    toggle_contours(&mut e);
    assert!(!e.topo.hide_contours);
}
