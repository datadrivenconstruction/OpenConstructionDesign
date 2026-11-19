// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::edit3d::pick::{Cat, Target as Pick3d};
use crate::geom_props::rows;
use crate::geom_props::tests::fixture;
use crate::selection::outlines;
use crate::wall_tool::{Event, Mode};
use ddc_geom::v2;
use ddc_model::{RailingSpec, StairShape, StairSpec};

fn count(ed: &Editor, table: &str) -> i64 {
    ed.model
        .sql()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn sel(kind: &str, id: i64) -> Sel {
    structx::sel(kind, id).unwrap()
}

/// A straight stair at the origin with a railing on each edge, and a sketch railing apart.
fn scene() -> (Editor, i64, [i64; 2], i64) {
    let mut ed = Editor::new_in_memory().unwrap();
    let top = ed.model.add_level("Top", ed.elevation + 3.0).unwrap();
    let stair = ed
        .model
        .add_stair(&StairSpec::straight(ed.level, top, (0.0, 0.0), 0.0))
        .unwrap();
    let tpl = RailingSpec::new(ed.level, RailingHost::Path(vec![]));
    let hosted = ed.model.add_stair_railings(stair, &tpl).unwrap();
    let sketch = ed
        .model
        .add_railing(&RailingSpec::new(
            ed.level,
            RailingHost::Path(vec![(20.0, 0.0, 0.0), (24.0, 0.0, 0.0)]),
        ))
        .unwrap();
    ed.reload().unwrap();
    ed.set_mode(Mode::Select);
    (ed, stair, hosted, sketch)
}

fn row(kind: &str, key: &str) -> geom_props::Row {
    rows()
        .into_iter()
        .find(|r| r.kind == kind && r.key == key)
        .unwrap()
}

fn set(ed: &mut Editor, r: &geom_props::Row, id: i64, v: f64) {
    geom_props::commit(ed, r, id, v);
    assert_eq!(ed.status.as_deref(), Some(tr("geom.changed", &[]).as_str()));
}

#[test]
fn stair_and_railing_are_clicked_in_plan() {
    let (mut ed, stair, _, sketch) = scene();
    let code = |k| structx::code_of(k).unwrap();
    assert!(ed.struct_fp.contains(code("stair"), stair));
    assert!(ed.struct_fp.contains(code("railing"), sketch));

    let ring = outlines(&ed, sel("stair", stair)).remove(0);
    let n = ring.len() as f64;
    let mid = ring.iter().fold(v2(0.0, 0.0), |a, p| a + *p * (1.0 / n));
    ed.selection.clear();
    ed.handle(Event::Click(mid));
    assert!(ed.selection.contains(&sel("stair", stair)), "{:?}", ed.selection);

    ed.selection.clear();
    ed.handle(Event::Click(v2(22.0, 0.0)));
    assert_eq!(ed.selection, vec![sel("railing", sketch)]);
}

#[test]
fn selection_maps_between_plan_and_3d() {
    let (ed, stair, _, sketch) = scene();
    for (cat, kind, id) in [(Cat::Stair, "stair", stair), (Cat::Railing, "railing", sketch)] {
        let t = Pick3d::new(cat, id);
        assert_eq!(t.sel(), Some(sel(kind, id)));
        assert_eq!(Pick3d::from_sel(sel(kind, id)), Some(t));
    }
    // the plan selection shows the grips of the element
    let mut ed = ed;
    ed.selection = vec![sel("stair", stair)];
    assert!(crate::grips::visible(&ed).iter().any(|g| g.id == stair));
}

#[test]
fn delete_takes_the_stair_with_its_railings_in_one_step() {
    let (mut ed, stair, _, sketch) = scene();
    assert_eq!(count(&ed, "railing"), 3);
    ed.selection = vec![sel("stair", stair)];
    ed.delete_selection();
    assert_eq!((count(&ed, "stair"), count(&ed, "railing")), (0, 1));
    assert!(ed.selection.is_empty());
    assert!(ed.status.as_deref().is_some_and(|s| !s.is_empty()));
    ed.undo();
    assert_eq!((count(&ed, "stair"), count(&ed, "railing")), (1, 3));
    assert!(ed.model.railing_path(sketch).is_ok());
}

#[test]
fn delete_with_a_wall_is_one_undo_step() {
    let mut fx = fixture();
    let (stair, railing, wall) = (fx.ids["stair"], fx.ids["railing"], fx.ids["wall"]);
    let ed = &mut fx.ed;
    let before = (count(ed, "stair"), count(ed, "railing"), count(ed, "wall"));
    ed.selection = vec![sel("stair", stair), sel("railing", railing), Sel::Wall(wall)];
    ed.delete_selection();
    assert_eq!(
        (count(ed, "stair"), count(ed, "railing"), count(ed, "wall")),
        (before.0 - 1, before.1 - 1, before.2 - 1)
    );
    ed.undo();
    assert_eq!(
        (count(ed, "stair"), count(ed, "railing"), count(ed, "wall")),
        before
    );
}

#[test]
fn hosted_railings_go_first_and_are_not_doubled() {
    let (ed, stair, hosted, sketch) = scene();
    let items = vec![
        ("stair".to_string(), stair),
        ("railing".to_string(), hosted[0]),
        ("railing".to_string(), sketch),
    ];
    let (list, added) = delete_list(&ed.model, items);
    assert_eq!(added, 1);
    assert_eq!(list.len(), 4);
    let last_rail = list.iter().rposition(|x| x.0 == "railing").unwrap();
    let stair_at = list.iter().position(|x| x.0 == "stair").unwrap();
    assert!(last_rail < stair_at);
}

#[test]
fn riser_tread_and_shape_are_edited_after_creation() {
    let (mut ed, stair, _, _) = scene();
    let risers = |ed: &Editor| ed.model.stair_quantities(stair).unwrap().risers;
    let r0 = risers(&ed);

    set(&mut ed, &row("stair", "geom.max_riser"), stair, 0.15);
    assert!(risers(&ed) > r0, "a lower riser needs more risers");
    ed.undo();
    assert_eq!(risers(&ed), r0);

    set(&mut ed, &row("stair", "geom.tread"), stair, 0.28);
    let q = ed.model.stair_quantities(stair).unwrap();
    assert!((q.tread - 0.28).abs() < 1e-9);
    tread_by_rule(&mut ed, stair);
    assert_eq!(ed.model.stair(stair).unwrap().tread, None);

    let shape = row("stair", "geom.stair_shape");
    assert!(matches!(ed.model.stair(stair).unwrap().shape, StairShape::Straight));
    set(&mut ed, &shape, stair, 1.0);
    assert!(matches!(ed.model.stair(stair).unwrap().shape, StairShape::L { left: true, .. }));
    set(&mut ed, &row("stair", "geom.stair_turn"), stair, 1.0);
    assert!(matches!(ed.model.stair(stair).unwrap().shape, StairShape::L { left: false, .. }));
    set(&mut ed, &shape, stair, 2.0);
    assert!(matches!(ed.model.stair(stair).unwrap().shape, StairShape::U { left: false, .. }));
    set(&mut ed, &shape, stair, 0.0);
    assert!(matches!(ed.model.stair(stair).unwrap().shape, StairShape::Straight));
    // a straight stair has no turn to show
    assert!((row("stair", "geom.stair_turn").get)(&ed.model, stair).is_err());
}

#[test]
fn shape_edit_is_one_undo_step() {
    let (mut ed, stair, _, _) = scene();
    let before = format!("{:?}", ed.model.stair(stair).unwrap());
    set(&mut ed, &row("stair", "geom.stair_shape"), stair, 2.0);
    assert_ne!(before, format!("{:?}", ed.model.stair(stair).unwrap()));
    ed.undo();
    assert_eq!(before, format!("{:?}", ed.model.stair(stair).unwrap()));
}

#[test]
fn a_path_ring_doubles_back_and_has_no_area() {
    let ring = path_ring(&[(0.0, 0.0), (4.0, 0.0), (4.0, 3.0)]);
    assert_eq!(ring, [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (4.0, 0.0)]);
    assert_eq!(path_ring(&[(0.0, 0.0), (1.0, 0.0)]).len(), 2);
}

#[test]
fn strings_exist_in_both_locales() {
    use ddc_ui::i18n::{Catalog, Lang};
    let c = Catalog::new(Lang::En);
    for key in [
        "selcat.stair",
        "selcat.railing",
        "quick.cat.stair",
        "quick.cat.railing",
        "stair.risers",
        "stair.riser",
        "stair.comfort",
        "stair.tread_auto",
        "stair.tread_auto_tip",
        "stair.broken",
        "railing.host_stair",
        "railing.host_path",
        "railing.length",
        "stair.deleted_with",
        "elem.stair.one",
        "elem.railing.many",
        "elem.deleted_what",
        "elem.not_deleted_what",
    ] {
        for l in [Lang::En, Lang::Ru] {
            assert!(c.table(l).contains_key(key), "{key}");
        }
    }
}

#[test]
fn typed_tread_is_one_undo_step_and_refused_text_writes_nothing() {
    use crate::units::{DecimalMark, Units};
    let (mut ed, stair, _, _) = scene();
    let before = ed.model.stair(stair).unwrap().tread;
    let r = row("stair", "geom.tread");
    // English: "1,5" is refused, so there is no value to commit
    assert!(crate::ui_kit::len_commit("28,5", Units::Metric, 0.0, 10.0, DecimalMark::Point).is_err());
    assert_eq!(ed.model.stair(stair).unwrap().tread, before);
    // "280" mm commits once; a single undo restores the stored tread
    let v = crate::ui_kit::len_commit("280", Units::Metric, 0.0, 10.0, DecimalMark::Point).unwrap();
    set(&mut ed, &r, stair, v);
    assert!((ed.model.stair(stair).unwrap().tread.unwrap() - 0.28).abs() < 1e-9);
    ed.undo();
    assert_eq!(ed.model.stair(stair).unwrap().tread, before);
}
