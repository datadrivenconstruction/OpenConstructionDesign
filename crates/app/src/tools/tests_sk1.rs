// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Sketch shapes for loop tools: every shape on every element kind.

use super::loop_shapes::{set_shape, SketchShape};
use super::{arm, Kind};
use crate::editor::Editor;
use crate::wall_tool::{Event, Key, Mode};
use ddc_geom::v2;
use ddc_ui::i18n::Lang;
use std::f64::consts::PI;

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

fn start(kind: Kind, shape: SketchShape) -> Editor {
    let mut e = ed();
    arm(&mut e, kind, Lang::En);
    set_shape(e.model_tool_mut().unwrap(), shape);
    e
}

fn pts(e: &Editor) -> usize {
    e.model_tool().map_or(0, |a| a.pts.len())
}

fn area_perim(r: &[(f64, f64)]) -> (f64, f64) {
    let n = r.len();
    let (mut a, mut p) = (0.0, 0.0);
    for i in 0..n {
        let (x0, y0) = r[i];
        let (x1, y1) = r[(i + 1) % n];
        a += x0 * y1 - x1 * y0;
        p += ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
    }
    (a / 2.0, p)
}

/// Table and ring of the first created element (None: ring not readable for roofs).
fn ring_of(e: &Editor, kind: Kind) -> Option<Vec<(f64, f64)>> {
    match kind {
        Kind::Floor | Kind::StructSlab => Some(e.model.slab_rings(1).unwrap().remove(0)),
        Kind::Ceiling => Some(e.model.ceiling_rings(1).unwrap().remove(0)),
        Kind::Shaft => Some(e.model.shaft_ring(1).unwrap()),
        _ => None,
    }
}

fn table(kind: Kind) -> &'static str {
    match kind {
        Kind::Floor | Kind::StructSlab => "slab",
        Kind::Roof => "roof",
        Kind::Ceiling => "ceiling",
        _ => "shaft",
    }
}

const KINDS: [Kind; 5] = [
    Kind::Floor,
    Kind::StructSlab,
    Kind::Roof,
    Kind::Ceiling,
    Kind::Shaft,
];

fn undo_removes(e: &mut Editor, kind: Kind) {
    e.set_mode(Mode::Select);
    e.undo();
    assert_eq!(count(e, table(kind)), 0, "{kind:?}: one undo step removes it");
}

#[test]
fn rect_floor_one_undo() {
    let mut e = start(Kind::Floor, SketchShape::Rect);
    click(&mut e, 0.0, 0.0);
    assert_eq!(pts(&e), 1);
    assert_eq!(count(&e, "slab"), 0);
    click(&mut e, 6.0, 4.0);
    assert_eq!(count(&e, "slab"), 1);
    assert_eq!(pts(&e), 0);
    let (a, p) = area_perim(&ring_of(&e, Kind::Floor).unwrap());
    assert!((a - 24.0).abs() < 1e-6 && (p - 20.0).abs() < 1e-6, "{a} {p}");
    undo_removes(&mut e, Kind::Floor);
}

#[test]
fn rect_every_kind() {
    for k in KINDS {
        let mut e = start(k, SketchShape::Rect);
        click(&mut e, 1.0, 1.0);
        click(&mut e, 4.0, 3.0);
        assert_eq!(count(&e, table(k)), 1, "{k:?}");
        if let Some(r) = ring_of(&e, k) {
            let (a, p) = area_perim(&r);
            assert!((a - 6.0).abs() < 1e-6 && (p - 10.0).abs() < 1e-6, "{k:?}");
        }
        undo_removes(&mut e, k);
    }
}

#[test]
fn circle_every_kind() {
    for k in KINDS {
        let mut e = start(k, SketchShape::Circle);
        click(&mut e, 0.0, 0.0);
        click(&mut e, 2.0, 0.0);
        assert_eq!(count(&e, table(k)), 1, "{k:?}");
        if let Some(r) = ring_of(&e, k) {
            assert!(r.len() >= 12, "{k:?}");
            let (a, p) = area_perim(&r);
            assert!((a - PI * 4.0).abs() / (PI * 4.0) < 1e-2, "{k:?} area {a}");
            assert!((p - 4.0 * PI).abs() / (4.0 * PI) < 1e-2, "{k:?} perimeter {p}");
        }
        undo_removes(&mut e, k);
    }
}

#[test]
fn polygon_every_kind() {
    for k in KINDS {
        let mut e = start(k, SketchShape::Polygon);
        e.model_tool_mut().unwrap().opts.sides = 4;
        click(&mut e, 0.0, 0.0);
        click(&mut e, 2.0, 0.0);
        assert_eq!(count(&e, table(k)), 1, "{k:?}");
        if let Some(r) = ring_of(&e, k) {
            assert_eq!(r.len(), 4);
            // square inscribed in a circle of radius 2: area 8, perimeter 8 * sqrt(2) / 2 * 2
            let (a, p) = area_perim(&r);
            assert!((a - 8.0).abs() < 1e-6, "{k:?} area {a}");
            assert!((p - 8.0 * 2f64.sqrt()).abs() < 1e-6, "{k:?} perimeter {p}");
        }
        undo_removes(&mut e, k);
    }
}

#[test]
fn zero_size_refused_every_kind() {
    for shape in [SketchShape::Rect, SketchShape::Circle, SketchShape::Polygon] {
        for k in KINDS {
            let mut e = start(k, shape);
            click(&mut e, 2.0, 2.0);
            click(&mut e, 2.0, 2.0);
            assert_eq!(count(&e, table(k)), 0, "{k:?} {shape:?}");
            assert_eq!(pts(&e), 1, "first point stays");
            assert!(e.status.is_some(), "reason is shown");
        }
    }
    // a flat rectangle is zero size too
    let mut e = start(Kind::Floor, SketchShape::Rect);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 5.0, 0.0);
    assert_eq!(count(&e, "slab"), 0);
}

#[test]
fn escape_cancels_shape_and_a_cycles_shapes() {
    let mut e = start(Kind::Floor, SketchShape::Rect);
    click(&mut e, 0.0, 0.0);
    e.handle(Event::Key(Key::Escape));
    assert_eq!(pts(&e), 0);
    assert_eq!(count(&e, "slab"), 0);
    let shape = |e: &Editor| e.model_tool().unwrap().opts.shape;
    e.handle(Event::Key(Key::Arc));
    assert_eq!(shape(&e), SketchShape::Circle);
    e.handle(Event::Key(Key::Arc));
    e.handle(Event::Key(Key::Arc));
    assert_eq!(shape(&e), SketchShape::Points);
}

#[test]
fn ceiling_auto_is_left_when_a_shape_is_chosen() {
    let mut e = start(Kind::Ceiling, SketchShape::Rect);
    assert!(!e.model_tool().unwrap().opts.pick);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 3.0, 3.0);
    assert_eq!(count(&e, "ceiling"), 1);
}

#[test]
fn points_mode_unchanged() {
    let mut e = start(Kind::Shaft, SketchShape::Points);
    for (x, y) in [(0.0, 0.0), (2.0, 0.0), (2.0, 2.0)] {
        click(&mut e, x, y);
    }
    assert_eq!(count(&e, "shaft"), 0);
    e.handle(Event::Key(Key::Enter));
    assert_eq!(count(&e, "shaft"), 1);
}

#[test]
fn roof_pick_mode_after_a_shape_draws_by_picking() {
    let mut e = start(Kind::Roof, SketchShape::Rect);
    let a = e.model_tool_mut().unwrap();
    assert!(super::loop_shapes::active(a));
    // the mode combo picks "inside a loop" after the shape switch turned picking off
    a.opts.pick = true;
    super::roof::mode_switched(&mut a.opts);
    assert_eq!(a.opts.shape, SketchShape::Points);
    assert!(!super::loop_shapes::active(a));
    // back to drawing keeps the points shape until the user picks another
    a.opts.pick = false;
    super::roof::mode_switched(&mut a.opts);
    assert_eq!(a.opts.shape, SketchShape::Points);
}
