// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Del on a vertex grip removes the vertex; Insert adds one; the host is never deleted.

use super::*;
use crate::editor::{Editor, Sel};
use ddc_model::{LayerSpec, SlabSpec};

const FIVE: [(f64, f64); 5] = [(0.0, 0.0), (4.0, 0.0), (5.0, 2.0), (4.0, 4.0), (0.0, 4.0)];

fn slab(ed: &mut Editor, outer: &[(f64, f64)]) -> i64 {
    slab_with(ed, outer, vec![])
}

fn slab_with(ed: &mut Editor, outer: &[(f64, f64)], holes: Vec<Vec<(f64, f64)>>) -> i64 {
    let layer = [LayerSpec {
        material: "c".into(),
        thickness: 0.2,
    }];
    let st = ed.model.add_slab_type("S", &layer).unwrap();
    let id = ed
        .model
        .add_slab(&SlabSpec {
            level: ed.level,
            slab_type: st,
            offset: 0.0,
            outer: outer.to_vec(),
            holes,
        })
        .unwrap();
    ed.reload().unwrap();
    ed.selection = vec![Sel::Slab(id)];
    id
}

fn n(ed: &Editor, id: i64) -> usize {
    ed.model.slab_rings(id).unwrap()[0].len()
}

fn click(ed: &mut Editor, id: i64, role: Role) {
    let g = grips(&ed.model, Kind::Slab, id)
        .unwrap()
        .into_iter()
        .find(|g| g.role == role)
        .unwrap();
    set_last(ed, &g);
}

#[test]
fn del_on_a_vertex_removes_it_and_undo_restores() {
    let mut ed = Editor::new_in_memory().unwrap();
    let id = slab(&mut ed, &FIVE);
    let before = ed.model.slab_rings(id).unwrap();
    click(&mut ed, id, Role::Vertex(2));
    assert!(delete_key(&mut ed));
    let r = ed.model.slab_rings(id).unwrap();
    assert_eq!(r[0].len(), 4);
    assert!(!r[0].contains(&FIVE[2]));
    ed.model.undo().unwrap();
    assert_eq!(ed.model.slab_rings(id).unwrap(), before);
}

#[test]
fn a_triangle_refuses_and_keeps_the_slab() {
    let mut ed = Editor::new_in_memory().unwrap();
    let id = slab(&mut ed, &FIVE[..3]);
    click(&mut ed, id, Role::Vertex(0));
    assert!(delete_key(&mut ed), "Del is consumed, the host is not deleted");
    assert_eq!(n(&ed, id), 3);
    assert!(ed.status.is_some());
}

#[test]
fn del_without_an_active_grip_leaves_the_key_to_element_delete() {
    let mut ed = Editor::new_in_memory().unwrap();
    let id = slab(&mut ed, &FIVE);
    assert!(!delete_key(&mut ed));
    assert_eq!(n(&ed, id), 5);
}

#[test]
fn removal_is_refused_when_an_opening_would_fall_outside() {
    let mut ed = Editor::new_in_memory().unwrap();
    let sq = [(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];
    let id = slab_with(&mut ed, &sq, vec![vec![(3.6, 3.0), (3.0, 3.0), (3.0, 3.6), (3.6, 3.6)]]);
    let before = ed.model.slab_rings(id).unwrap();
    click(&mut ed, id, Role::Vertex(2));
    assert!(delete_key(&mut ed));
    assert_eq!(ed.model.slab_rings(id).unwrap(), before);
    assert!(ed.status.is_some());
}

#[test]
fn insert_grip_adds_a_point_in_one_undo_step() {
    let mut ed = Editor::new_in_memory().unwrap();
    let id = slab(&mut ed, &FIVE);
    let g = grips(&ed.model, Kind::Slab, id)
        .unwrap()
        .into_iter()
        .find(|g| g.role == Role::Insert(0))
        .unwrap();
    assert!((g.pos - v2(1.0, 0.0)).len() < 1e-12);
    let before = ed.model.slab_rings(id).unwrap();
    apply(&mut ed.model, &g, v2(1.0, -0.5)).unwrap();
    let r = &ed.model.slab_rings(id).unwrap()[0];
    assert_eq!(r.len(), 6);
    assert_eq!(r[1], (1.0, -0.5));
    ed.model.undo().unwrap();
    assert_eq!(ed.model.slab_rings(id).unwrap(), before);
}

const SQ10: [(f64, f64); 4] = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];

fn grip(ed: &Editor, id: i64, role: Role) -> Grip {
    grips(&ed.model, Kind::Slab, id)
        .unwrap()
        .into_iter()
        .find(|g| g.role == role)
        .unwrap()
}

#[test]
fn hole_edge_crossing_outer_refused() {
    let mut ed = Editor::new_in_memory().unwrap();
    let hole = vec![(2.0, 5.0), (8.0, 0.5), (2.0, 1.0)];
    let id = slab_with(&mut ed, &SQ10, vec![hole]);
    let before = ed.model.slab_rings(id).unwrap();
    // every hole vertex stays inside, but the hole edge would cut across the new boundary
    let g = grip(&ed, id, Role::Vertex(2));
    assert!(apply(&mut ed.model, &g, v2(5.0, 2.0)).is_err());
    assert_eq!(ed.model.slab_rings(id).unwrap(), before);
}

#[test]
fn hole_vertex_del_overlapping_hole_refused() {
    let mut ed = Editor::new_in_memory().unwrap();
    let a = vec![(4.0, 4.0), (6.0, 4.0), (6.0, 6.0), (4.0, 6.0)];
    let c = vec![
        (1.0, 1.0),
        (9.0, 1.0),
        (9.0, 2.0),
        (2.0, 2.0),
        (2.0, 8.0),
        (9.0, 8.0),
        (9.0, 9.0),
        (1.0, 9.0),
    ];
    let id = slab_with(&mut ed, &SQ10, vec![a, c]);
    let before = ed.model.slab_rings(id).unwrap();
    click(&mut ed, id, Role::HoleVertex(1, 4));
    assert!(delete_key(&mut ed));
    assert_eq!(ed.model.slab_rings(id).unwrap(), before);
    assert!(ed.status.is_some());
}

#[test]
fn hole_drag_self_intersection_refused() {
    let mut ed = Editor::new_in_memory().unwrap();
    // holes are stored clockwise, so the grip index follows this order; vertex 2 is (6, 6)
    let hole = vec![(3.0, 3.0), (3.0, 6.0), (6.0, 6.0), (6.0, 3.0)];
    let id = slab_with(&mut ed, &SQ10, vec![hole]);
    let before = ed.model.slab_rings(id).unwrap();
    let g = grip(&ed, id, Role::HoleVertex(0, 2));
    assert!(apply(&mut ed.model, &g, v2(7.0, 1.0)).is_err());
    assert_eq!(ed.model.slab_rings(id).unwrap(), before);
}

#[test]
fn ring_refusal_keys_exist_in_en_and_ru() {
    use ddc_ui::i18n::Lang;
    for k in RING_KEYS {
        let en = crate::tr_in(Lang::En, k, &[]);
        let ru = crate::tr_in(Lang::Ru, k, &[]);
        assert!(!en.is_empty() && en != k, "{k} en");
        assert!(!ru.is_empty() && ru != k, "{k} ru");
        assert_ne!(en, ru, "{k}: ru falls back to en");
    }
}

/// The refusal names its reason in the current UI language (whatever it is: tests never flip
/// it), and the status text keeps that reason instead of the generic one.
#[test]
fn ring_refusal_is_localized_and_kept_in_status() {
    let outer = SQ10.iter().map(|&p| pv(p)).collect::<Vec<_>>();
    let cases: [(Vec<Vec<(f64, f64)>>, &str); 3] = [
        (vec![vec![(3.0, 3.0), (6.0, 6.0), (6.0, 3.0), (3.0, 6.0)]], RING_SHAPE),
        (vec![vec![(2.0, 5.0), (12.0, 5.0), (2.0, 6.0)]], RING_BOUNDARY),
        (
            vec![
                vec![(2.0, 2.0), (5.0, 2.0), (5.0, 5.0), (2.0, 5.0)],
                vec![(4.0, 4.0), (7.0, 4.0), (7.0, 7.0), (4.0, 7.0)],
            ],
            RING_OVERLAP,
        ),
    ];
    use ddc_ui::i18n::Lang;
    for (holes, key) in cases {
        let e = rings_ok(&outer, &holes).unwrap_err();
        let Error::Degenerate(m) = &e else { panic!("{key}: {e}") };
        // read against both languages: another test may switch the UI language meanwhile
        let ok = [Lang::En, Lang::Ru].into_iter().any(|l| crate::tr_in(l, key, &[]) == *m);
        assert!(ok, "{key}: {m}");
        assert_eq!(crate::constraints_ui::report(&e), *m);
    }
}

#[test]
fn valid_hole_edit_one_undo() {
    let mut ed = Editor::new_in_memory().unwrap();
    // clockwise, as stored: vertex 2 is (6, 6)
    let hole = vec![(3.0, 3.0), (3.0, 6.0), (6.0, 6.0), (6.0, 3.0)];
    let id = slab_with(&mut ed, &SQ10, vec![hole]);
    let before = ed.model.slab_rings(id).unwrap();
    let g = grip(&ed, id, Role::HoleVertex(0, 2));
    apply(&mut ed.model, &g, v2(7.0, 7.0)).unwrap();
    assert_eq!(ed.model.slab_rings(id).unwrap()[1][2], (7.0, 7.0));
    ed.model.undo().unwrap();
    assert_eq!(ed.model.slab_rings(id).unwrap(), before);
}
