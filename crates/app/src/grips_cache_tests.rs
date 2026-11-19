//! Grip cache: a hit while nothing changed, a miss after every change that can move grips, and
//! the cached list always equal to the uncached one.

use super::*;
use crate::editor::{bodies::BodyKind, Editor, Sel};
use crate::wall_tool::Event;
use ddc_model::ColumnSpec;
use std::sync::atomic::Ordering;

fn builds(ed: &Editor) -> usize {
    ed.grip.cache.builds.load(Ordering::Relaxed)
}

fn dim_builds(ed: &Editor) -> usize {
    ed.grip.cache.dim_builds.load(Ordering::Relaxed)
}

fn wall_type(ed: &mut Editor) -> i64 {
    let layer = [ddc_model::LayerSpec {
        material: "c".into(),
        thickness: 0.2,
    }];
    ed.model.add_wall_type("W", &layer).unwrap()
}

fn wall(ed: &mut Editor, wt: i64, level: i64, a: (f64, f64), b: (f64, f64)) -> i64 {
    ed.model
        .add_wall(&ddc_model::WallSpec {
            a,
            b,
            level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap()
}

fn col(ed: &mut Editor, level: i64, at: (f64, f64)) -> i64 {
    let have: Option<i64> = ed
        .model
        .sql()
        .query_row("SELECT id FROM member_type WHERE name='R 400x400'", [], |r| r.get(0))
        .ok();
    let mt = have.unwrap_or_else(|| ed.model.add_catalog_member_type("R 400x400").unwrap());
    ed.model
        .add_column(&ColumnSpec {
            level,
            top_level: None,
            member_type: mt,
            structural: true,
            at,
            base_offset: 0.0,
            top_offset: 3.0,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap()
}

/// One wall selected on the view level, select mode, grips built once.
fn one_wall() -> (Editor, i64) {
    let mut ed = Editor::new_in_memory().unwrap();
    let wt = wall_type(&mut ed);
    let l = ed.level;
    let id = wall(&mut ed, wt, l, (0.0, 0.0), (5.0, 0.0));
    ed.reload().unwrap();
    ed.selection = vec![Sel::Wall(id)];
    ed.set_mode(crate::wall_tool::Mode::Select);
    assert!(!visible(&ed).is_empty());
    (ed, id)
}

/// The cache missed exactly once and holds what an uncached build gives.
fn missed_once(ed: &Editor, before: usize, what: &str) -> Vec<Grip> {
    let gs = visible(ed);
    assert_eq!(builds(ed), before + 1, "{what}: cache must miss");
    assert_eq!(gs, build_visible(ed), "{what}: cached equals uncached");
    gs
}

#[test]
fn hover_and_press_frames_hit_the_cache() {
    let (mut ed, _) = one_wall();
    let b = builds(&ed);
    for _ in 0..50 {
        let _ = visible(&ed);
        handle(&mut ed, Event::Move(v2(-9.0, -9.0)));
        handle(&mut ed, Event::Press(v2(-9.0, -9.0)));
    }
    assert_eq!(builds(&ed), b, "nothing changed: no rebuild");
}

#[test]
fn edit_undo_and_redo_miss() {
    let (mut ed, _) = one_wall();
    let first = visible(&ed);
    let b = builds(&ed);
    assert!(nudge(&mut ed, 1, 0, false));
    let moved = missed_once(&ed, b, "edit");
    assert_ne!(moved, first);
    let b = builds(&ed);
    ed.undo();
    assert_eq!(missed_once(&ed, b, "undo"), first);
    let b = builds(&ed);
    ed.redo();
    assert_eq!(missed_once(&ed, b, "redo"), moved);
}

#[test]
fn reload_misses_even_without_a_change() {
    let (mut ed, id) = one_wall();
    let first = visible(&ed);
    let b = builds(&ed);
    ed.reload().unwrap();
    assert_eq!(missed_once(&ed, b, "reload"), first);
    // a write behind the editor, then the reload that every scoped reload also ends in
    ed.model
        .sql()
        .execute("UPDATE wall SET bx = 7.0 WHERE id = ?1", [id])
        .unwrap();
    ed.reload().unwrap();
    let b = builds(&ed);
    let gs = missed_once(&ed, b, "reload after a write");
    assert!(gs.iter().any(|g| (g.pos - v2(7.0, 0.0)).len() < 1e-9), "{gs:?}");
}

#[test]
fn journaled_write_without_reload_misses() {
    let mut ed = Editor::new_in_memory().unwrap();
    let l = ed.level;
    let c = col(&mut ed, l, (0.0, 0.0));
    ed.reload().unwrap();
    ed.selection = vec![Sel::Body(BodyKind::Column, c)];
    let _ = visible(&ed);
    let b = builds(&ed);
    let mut spec = ed.model.column(c).unwrap();
    spec.at = (2.0, 1.0);
    ed.model.update_column(c, &spec).unwrap();
    let gs = missed_once(&ed, b, "journaled write");
    assert!(gs.iter().any(|g| g.role == Role::Move && (g.pos - v2(2.0, 1.0)).len() < 1e-9));
}

#[test]
fn level_switch_misses_and_filters_by_level() {
    let mut ed = Editor::new_in_memory().unwrap();
    let wt = wall_type(&mut ed);
    let l1 = ed.level;
    let l2 = ed.model.add_level("L2", 3.0).unwrap();
    let w1 = wall(&mut ed, wt, l1, (0.0, 0.0), (5.0, 0.0));
    let w2 = wall(&mut ed, wt, l2, (0.0, 4.0), (5.0, 4.0));
    ed.reload().unwrap();
    ed.selection = vec![Sel::Wall(w1), Sel::Wall(w2)];
    let on1 = visible(&ed);
    assert!(!on1.is_empty());
    assert!(on1.iter().all(|g| g.id == w1), "a wall of another level shows no grips");
    let b = builds(&ed);
    ed.level = l2;
    let on2 = missed_once(&ed, b, "level switch");
    assert!(!on2.is_empty() && on2.iter().all(|g| g.id == w2));
    let b = builds(&ed);
    ed.level = l1;
    assert_eq!(missed_once(&ed, b, "level back"), on1);
}

#[test]
fn selection_and_focus_changes_miss() {
    let mut ed = Editor::new_in_memory().unwrap();
    let wt = wall_type(&mut ed);
    let l = ed.level;
    let w = wall(&mut ed, wt, l, (0.0, 0.0), (5.0, 0.0));
    let c = col(&mut ed, l, (8.0, 0.0));
    ed.reload().unwrap();
    ed.selection = vec![Sel::Wall(w)];
    let walls = visible(&ed);
    let b = builds(&ed);
    ed.selection = vec![Sel::Body(BodyKind::Column, c)];
    let cols = missed_once(&ed, b, "selection");
    assert!(cols.iter().all(|g| g.kind == Kind::Column));
    let b = builds(&ed);
    ed.selection.clear();
    assert!(missed_once(&ed, b, "cleared").is_empty());
    let b = builds(&ed);
    ed.grip.focus = vec![(Kind::Column, c)];
    assert_eq!(missed_once(&ed, b, "focus"), cols);
    let b = builds(&ed);
    ed.grip.focus.clear();
    ed.selection = vec![Sel::Wall(w)];
    assert_eq!(missed_once(&ed, b, "reselect"), walls);
}

#[test]
fn last_grip_dims_hit_then_miss_after_edit() {
    let (mut ed, id) = one_wall();
    let g = visible(&ed).into_iter().find(|g| !is_click(&g.role)).unwrap();
    ed.grip.last = Some((Kind::Wall, id, g.role.clone()));
    let d0 = dim_builds(&ed);
    let a = last_dims(&ed);
    assert_eq!(last_dims(&ed), a);
    assert_eq!(dim_builds(&ed), d0 + 1, "second call hits");
    assert!(nudge(&mut ed, 0, 1, false));
    ed.grip.last = Some((Kind::Wall, id, g.role.clone()));
    let _ = last_dims(&ed);
    assert_eq!(dim_builds(&ed), d0 + 2, "edit misses");
}

#[test]
fn level_lookup_of_a_missing_row_keeps_no_filter() {
    let ed = Editor::new_in_memory().unwrap();
    for k in [Kind::Wall, Kind::Slab, Kind::Stair, Kind::Railing, Kind::Mep, Kind::Opening] {
        assert!(matches!(level_of(&ed.model, k, 987_654), Ok(None)), "{k:?}");
    }
    assert!(level_of(&ed.model, Kind::Grid, 1).unwrap().is_none());
}

/// Small deterministic generator: the same model on every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn coord(&mut self) -> f64 {
        (self.next() % 400) as f64 * 0.25
    }
}

#[test]
fn cached_grips_equal_uncached_on_a_seeded_model() {
    let mut ed = Editor::new_in_memory().unwrap();
    let mut rng = Lcg(0x5eed_9a1f);
    let wt = wall_type(&mut ed);
    let l1 = ed.level;
    let l2 = ed.model.add_level("L2", 3.0).unwrap();
    let mut sel = vec![];
    for i in 0..60 {
        let lv = if i % 5 == 0 { l2 } else { l1 };
        let a = (rng.coord(), rng.coord());
        let b = (a.0 + 1.0 + rng.coord() * 0.1, a.1 + rng.coord() * 0.05);
        sel.push(Sel::Wall(wall(&mut ed, wt, lv, a, b)));
    }
    for i in 0..20 {
        let lv = if i % 4 == 0 { l2 } else { l1 };
        let at = (rng.coord(), rng.coord());
        sel.push(Sel::Body(BodyKind::Column, col(&mut ed, lv, at)));
    }
    ed.reload().unwrap();
    ed.set_mode(crate::wall_tool::Mode::Select);
    for step in 0..12 {
        let n = 1 + (rng.next() as usize % sel.len());
        let start = rng.next() as usize % sel.len();
        ed.selection = (0..n).map(|k| sel[(start + k) % sel.len()]).collect();
        let gs = visible(&ed);
        assert_eq!(gs, build_visible(&ed), "step {step}: first build");
        let b = builds(&ed);
        assert_eq!(visible(&ed), gs, "step {step}: hit");
        assert_eq!(builds(&ed), b);
        for g in &gs {
            if let Ok(Some(l)) = level_of(&ed.model, g.kind, g.id) {
                assert_eq!(l, ed.level, "step {step}: {:?} {} of another level", g.kind, g.id);
            }
        }
        match rng.next() % 4 {
            0 => {
                nudge(&mut ed, 1, 0, false);
            }
            1 => ed.undo(),
            2 => ed.level = if ed.level == l1 { l2 } else { l1 },
            _ => ed.reload().unwrap(),
        }
        assert_eq!(visible(&ed), build_visible(&ed), "step {step}: after a change");
    }
}
