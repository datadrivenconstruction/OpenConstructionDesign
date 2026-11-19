//! Fallback triggers of the scoped reload: a schema change, another active level, a change set
//! above the limit, undo and redo across many steps, an import and its undo. Each takes the
//! full reload where it must, and the lists always equal a fresh full reload afterwards.

use super::{hits, LIMIT_MIN};
use crate::editor::{Editor, OpFn, Sel};
use ddc_model::{Edit, LayerSpec, Model, OpeningSpec, WallSpec};
use std::sync::Arc;

/// 285 joined walls: 10 rows of 15 segments and 9 bands of 15 connectors; one door.
fn fixture() -> (Editor, Vec<i64>) {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let layer = |m: &str, t: f64| LayerSpec {
        material: m.into(),
        thickness: t,
    };
    let t = ed
        .model
        .add_wall_type("G two", &[layer("a", 0.1), layer("b", 0.2)])
        .unwrap();
    let mut specs = vec![];
    for r in 0..10 {
        let y = r as f64 * 5.0;
        for i in 0..15 {
            let x = i as f64 * 4.0;
            specs.push(((x, y), (x + 4.0, y)));
            if r < 9 {
                specs.push(((x, y), (x, y + 5.0)));
            }
        }
    }
    let specs: Vec<WallSpec> = specs
        .into_iter()
        .map(|(a, b)| WallSpec {
            a,
            b,
            level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .collect();
    let ids = ed.model.add_walls(&specs).unwrap();
    ed.model.add_opening(ids[0], &door_spec(1.0, 0.9)).unwrap();
    ed.reload().unwrap();
    (ed, ids)
}

fn door_spec(along: f64, width: f64) -> OpeningSpec {
    OpeningSpec {
        along,
        width,
        sill: 0.0,
        height: 2.1,
        kind: "door".into(),
    }
}

fn lists(ed: &Editor) -> String {
    let snap: Vec<_> = ed.tool.walls.iter().map(|s| (s.a, s.b, s.half)).collect();
    format!(
        "{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{}",
        ed.walls,
        ed.plan,
        ed.otool.walls,
        snap,
        ed.openings,
        ed.otool.openings,
        ed.selection,
        ed.rooms,
        ed.level
    )
}

fn same_as_full(ed: &mut Editor, what: &str) {
    let now = lists(ed);
    ed.reload().unwrap();
    assert!(now == lists(ed), "{what}: the lists differ from a full reload");
}

fn set(m: &mut Model, id: i64, f: impl FnOnce(&mut WallSpec)) -> Result<(), ddc_model::Error> {
    let (mut s, bulge) = crate::query::wall_spec(m, id)?;
    f(&mut s);
    m.apply_edits(&[Edit::Wall { id, spec: s, bulge }])
}

fn height(ed: &mut Editor, id: i64, v: f64) {
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| set(m, id, |s| s.height = v))))
        .unwrap();
}

#[test]
fn schema_change_takes_full() {
    let (mut ed, ids) = fixture();
    let w = ids[3];
    // the first op after a load regenerates the level
    height(&mut ed, w, 3.2);
    let h = hits();
    // the journal does not see DDL: the schema version does
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        m.sql()
            .execute_batch("CREATE TABLE fz_guard_extra(x REAL)")?;
        set(m, w, |s| s.height = 3.4)
    })))
    .unwrap();
    assert_eq!(hits(), h, "a schema change takes the full reload");
    same_as_full(&mut ed, "schema change");
    let h = hits();
    height(&mut ed, w, 3.6);
    assert_eq!(hits(), h + 1, "after the full reload the next edit is scoped again");
    same_as_full(&mut ed, "edit after the schema change");
}

#[test]
fn other_active_level_takes_full() {
    let (mut ed, ids) = fixture();
    height(&mut ed, ids[1], 3.1);
    let door = ed.openings[0].id;
    let other = ed.model.add_level("G2", 3.0).unwrap();
    // the lists still hold the first level
    ed.level = other;
    let h = hits();
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        m.update_opening(door, &door_spec(1.5, 0.9))
    })))
    .unwrap();
    assert_eq!(hits(), h, "lists of another level are never patched");
    assert!(ed.walls.is_empty() && ed.openings.is_empty(), "the lists follow the level");
    same_as_full(&mut ed, "level switch");
}

#[test]
fn big_change_set_takes_full() {
    let (mut ed, ids) = fixture();
    assert!(ids.len() > LIMIT_MIN, "the fixture must exceed the limit");
    let all = ids.clone();
    let h = hits();
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        let mut edits = vec![];
        for &id in &all {
            let (mut spec, bulge) = crate::query::wall_spec(m, id)?;
            spec.height = 3.7;
            edits.push(Edit::Wall { id, spec, bulge });
        }
        m.apply_edits(&edits)
    })))
    .unwrap();
    assert_eq!(hits(), h, "a change set above the limit takes the full reload");
    assert!(ed.walls.iter().all(|w| w.height == 3.7));
    same_as_full(&mut ed, "big change");
    let h = hits();
    height(&mut ed, ids[7], 3.1);
    assert_eq!(hits(), h + 1, "one wall: scoped");
    same_as_full(&mut ed, "small change");
    // undo of the big change is big too
    ed.undo();
    let h = hits();
    ed.undo();
    assert_eq!(hits(), h, "undo of the big change takes the full reload");
    assert!(ed.walls.iter().all(|w| w.height == 3.0));
    same_as_full(&mut ed, "undo of the big change");
}

#[test]
fn undo_across_many_steps_equals_full() {
    let (mut ed, ids) = fixture();
    height(&mut ed, ids[1], 3.0);
    ed.selection = ids.iter().step_by(5).map(|&id| Sel::Wall(id)).collect();
    let shape = |ed: &Editor| -> Vec<_> {
        ed.walls
            .iter()
            .map(|w| (w.id, w.a, w.b, w.height, w.wall_type))
            .collect()
    };
    let start = shape(&ed);
    let depth = ed.undo_len();
    let (level, t) = (ed.level, ed.types[0].id);
    let door = ed.openings[0].id;
    for k in 0..24 {
        let id = ids[(k * 7) % ids.len()];
        let v = 2.5 + k as f64 * 0.05;
        let f: OpFn = match k % 6 {
            0 | 3 => OpFn(Arc::new(move |m: &mut Model| set(m, id, |s| s.height = v))),
            1 => OpFn(Arc::new(move |m: &mut Model| {
                set(m, id, |s| s.a = (s.a.0 + 0.5, s.a.1))
            })),
            2 => OpFn(Arc::new(move |m: &mut Model| set(m, id, |s| s.wall_type = t))),
            4 => {
                let y = 100.0 + k as f64;
                OpFn(Arc::new(move |m: &mut Model| {
                    m.add_wall(&WallSpec {
                        a: (0.0, y),
                        b: (3.0, y),
                        level,
                        base_offset: 0.0,
                        height: 3.0,
                        wall_type: t,
                        offset0: None,
                    })
                    .map(|_| ())
                }))
            }
            _ => {
                let w = 0.8 + k as f64 * 0.01;
                OpFn(Arc::new(move |m: &mut Model| {
                    m.update_opening(door, &door_spec(1.0, w))
                }))
            }
        };
        ed.run_op(f).unwrap();
    }
    assert_eq!(ed.undo_len(), depth + 24);
    let top = shape(&ed);
    let h = hits();
    assert_eq!(ed.jump_history(depth), 24);
    let scoped = hits() - h;
    assert!(
        scoped > 0 && scoped < 24,
        "{scoped} of 24 undo steps scoped: an undone add takes the full reload"
    );
    same_as_full(&mut ed, "undo of 24 steps");
    assert_eq!(shape(&ed), start, "undo of every step restores the walls");
    let h = hits();
    assert_eq!(ed.jump_history(depth + 24), 24);
    assert!(hits() > h, "redo steps take the scoped reload too");
    same_as_full(&mut ed, "redo of 24 steps");
    assert_eq!(shape(&ed), top, "redo of every step restores the edits");
}

#[test]
fn import_and_its_undo_take_full() {
    let (mut ed, ids) = fixture();
    height(&mut ed, ids[1], 3.0);
    let old = ed.level;
    let p = std::env::temp_dir().join("ddc_scoped_guard_import.rvt");
    std::fs::write(&p, b"x").unwrap();
    let h = hits();
    ed.apply_import(&p, crate::editor::tests::import_frames_one_wall())
        .unwrap();
    assert_eq!(hits(), h, "an import takes the full reload");
    same_as_full(&mut ed, "import");
    ed.undo();
    assert_eq!(hits(), h, "undo of an import takes the full reload");
    same_as_full(&mut ed, "undo of the import");
    if ed.level != old {
        ed.level = old;
        ed.reload().unwrap();
    }
    let h = hits();
    height(&mut ed, ids[2], 3.3);
    assert_eq!(hits(), h + 1, "after the import a wall edit is scoped again");
    same_as_full(&mut ed, "edit after the import");
}
