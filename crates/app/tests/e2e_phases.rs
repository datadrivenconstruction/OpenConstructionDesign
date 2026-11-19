//! Workflow: existing walls → demolish in later phases → new walls → schedules by phase.
//! Default phases are Existing / Demolition / New (positions 0, 1, 2); new elements take the
//! current phase, "demolish" keeps the element and carries its openings.

mod e2e_common;

use std::sync::Arc;

use ddc_app::editor::{Editor, OpFn};
use ddc_app::phase_ui;
use ddc_app::schedule::extra::{phase_ok, PhaseMode, NO_DEMOLITION, PH_CREATED, PH_DEMOLISHED};
use ddc_app::schedule::{self, Cat, Val};
use ddc_model::{
    ElemPhase, Model, OpeningSpec, PhaseFilter, PhaseStatus, ViewKey, ViewPhase, WallSpec,
};
use e2e_common::{assert_clean, assert_same, dump_data, ids, journal_len};

fn op(
    ed: &mut Editor,
    f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) {
    ed.run_op(OpFn(Arc::new(f))).unwrap();
    assert_clean(&ed.model, "after op");
}

fn add_wall(ed: &mut Editor, a: (f64, f64), b: (f64, f64)) -> i64 {
    let (l, t) = (ed.level, ed.types[1].id);
    op(ed, move |m| {
        m.add_wall(&WallSpec {
            a,
            b,
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .map(drop)
    });
    *ids(&ed.model, "wall").last().unwrap()
}

fn add_opening(ed: &mut Editor, wall: i64, along: f64, kind: &'static str) -> i64 {
    op(ed, move |m| {
        m.add_opening(
            wall,
            &OpeningSpec {
                along,
                width: 0.9,
                sill: if kind == "door" { 0.0 } else { 0.9 },
                height: if kind == "door" { 2.1 } else { 1.2 },
                kind: kind.into(),
            },
        )
        .map(drop)
    });
    *ids(&ed.model, "opening").last().unwrap()
}

fn demolish(ed: &mut Editor, items: Vec<(&'static str, i64)>, phase: i64) {
    op(ed, move |m| m.demolish(&items, phase).map(drop));
}

/// Ids of the schedule rows that pass the phase filter, ascending.
fn rows(ed: &Editor, cat: Cat, mode: PhaseMode) -> Vec<i64> {
    let tb = schedule::build(ed, cat);
    let mut v: Vec<i64> = (0..tb.rows.len())
        .filter(|&i| phase_ok(&tb, i, mode))
        .map(|i| tb.rows[i].id)
        .collect();
    v.sort_unstable();
    v
}

fn sorted(mut v: Vec<i64>) -> Vec<i64> {
    v.sort_unstable();
    v
}

/// Walls shown (any graphics) in phase `phase` with `filter`.
fn shown(m: &Model, walls: &[i64], phase: i64, filter: PhaseFilter) -> usize {
    let ix = m.phase_index().unwrap();
    let vp = ViewPhase { phase, filter };
    walls
        .iter()
        .filter(|w| ix.show("wall", **w, vp) != ddc_model::PhaseShow::Hidden)
        .count()
}

#[test]
fn e2e_phases_demolish_new_and_schedules_by_phase() {
    let mut ed = Editor::new_in_memory().unwrap();
    let ps: Vec<i64> = ed.model.phases().unwrap().iter().map(|p| p.id).collect();
    assert_eq!(ps.len(), 3);
    let (ex, demo, new) = (ps[0], ps[1], ps[2]);
    // a fresh model builds in the last phase
    assert_eq!(ed.model.current_phase().unwrap(), new);

    // existing box with a door on wall 2 and a window on wall 1
    ed.model.set_current_phase(ex).unwrap();
    let e1 = add_wall(&mut ed, (0.0, 0.0), (6.0, 0.0));
    let e2 = add_wall(&mut ed, (6.0, 0.0), (6.0, 4.0));
    let e3 = add_wall(&mut ed, (6.0, 4.0), (0.0, 4.0));
    let e4 = add_wall(&mut ed, (0.0, 4.0), (0.0, 0.0));
    let door = add_opening(&mut ed, e2, 1.0, "door");
    let window = add_opening(&mut ed, e1, 2.0, "window");
    // new construction
    ed.model.set_current_phase(new).unwrap();
    let n1 = add_wall(&mut ed, (8.0, 0.0), (8.0, 4.0));
    let n2 = add_wall(&mut ed, (10.0, 0.0), (10.0, 4.0));
    let old = [e1, e2, e3, e4];
    let all = sorted(vec![e1, e2, e3, e4, n1, n2]);

    for w in old {
        assert_eq!(
            ed.model.element_phase("wall", w).unwrap(),
            ElemPhase {
                created: ex,
                demolished: None
            }
        );
    }
    for w in [n1, n2] {
        assert_eq!(
            ed.model.element_phase("wall", w).unwrap(),
            ElemPhase {
                created: new,
                demolished: None
            }
        );
    }
    assert_eq!(ed.model.element_phase("opening", door).unwrap().created, ex);

    // baseline: nothing demolished; the first phase has only the old walls
    assert_eq!(rows(&ed, Cat::Walls, PhaseMode::All), all);
    assert_eq!(
        rows(&ed, Cat::Walls, PhaseMode::In(ex)),
        sorted(old.to_vec())
    );
    assert_eq!(rows(&ed, Cat::Walls, PhaseMode::In(new)), all);
    for p in [ex, demo, new] {
        assert!(rows(&ed, Cat::Walls, PhaseMode::Demolished(p)).is_empty());
    }
    let built = dump_data(&ed.model);

    // schedule columns show created / demolished phase labels
    let tb = schedule::build(&ed, Cat::Walls);
    let jc = tb.cols.iter().position(|c| c.key == PH_CREATED).unwrap();
    let jd = tb.cols.iter().position(|c| c.key == PH_DEMOLISHED).unwrap();
    let labels: Vec<String> = ed
        .model
        .phases()
        .unwrap()
        .iter()
        .map(|p| phase_ui::label(p, true))
        .collect();
    let r = |id: i64| tb.rows.iter().find(|r| r.id == id).unwrap();
    assert_eq!(r(n1).vals[jc], Val::T(labels[2].clone()));
    assert_eq!(r(e1).vals[jc], Val::T(labels[0].clone()));
    assert_eq!(r(e1).vals[jd], Val::T(NO_DEMOLITION.into()));

    // demolish two old walls in the Demolition phase
    demolish(&mut ed, vec![("wall", e3), ("wall", e4)], demo);
    let after_b = dump_data(&ed.model);
    assert_eq!(
        rows(&ed, Cat::Walls, PhaseMode::Demolished(demo)),
        sorted(vec![e3, e4])
    );
    assert!(rows(&ed, Cat::Walls, PhaseMode::Demolished(new)).is_empty());
    // the walls stay in the model; they stand in phases up to Demolition and are gone after it
    assert_eq!(rows(&ed, Cat::Walls, PhaseMode::All), all);
    assert_eq!(rows(&ed, Cat::Walls, PhaseMode::In(ex)).len(), 4);
    assert_eq!(
        rows(&ed, Cat::Walls, PhaseMode::In(demo)),
        sorted(old.to_vec())
    );
    assert_eq!(
        rows(&ed, Cat::Walls, PhaseMode::In(new)),
        sorted(vec![e1, e2, n1, n2])
    );
    let ix = ed.model.phase_index().unwrap();
    assert_eq!(ix.status("wall", e3, demo), PhaseStatus::Demolished);
    assert_eq!(ix.status("wall", e3, new), PhaseStatus::Gone);
    assert_eq!(ix.status("wall", n1, demo), PhaseStatus::Future);
    assert_eq!(ix.status("wall", n1, new), PhaseStatus::New);
    let d = ed.model.demolition(demo).unwrap();
    assert_eq!(d.count, 2);
    assert!(
        d.volume > 0.0 && d.by_kind.len() == 1 && d.by_kind[0].0 == "wall" && d.by_kind[0].1 == 2
    );
    assert_eq!(
        rows(&ed, Cat::Doors, PhaseMode::Demolished(demo)),
        Vec::<i64>::new()
    );

    // demolish wall 2 in the New phase: its door goes with it, the window stays
    demolish(&mut ed, vec![("wall", e2)], new);
    assert_eq!(rows(&ed, Cat::Walls, PhaseMode::Demolished(new)), vec![e2]);
    assert_eq!(
        rows(&ed, Cat::Doors, PhaseMode::Demolished(new)),
        vec![door]
    );
    assert!(rows(&ed, Cat::Windows, PhaseMode::Demolished(new)).is_empty());
    assert_eq!(rows(&ed, Cat::Windows, PhaseMode::In(new)), vec![window]);
    // a wall demolished in the phase still stands in that phase's schedule
    assert_eq!(
        rows(&ed, Cat::Walls, PhaseMode::In(new)),
        sorted(vec![e1, e2, n1, n2])
    );
    assert_eq!(ed.model.demolition(new).unwrap().count, 2);
    assert_eq!(ed.model.demolition(demo).unwrap().count, 2);
    let after_c = dump_data(&ed.model);

    // what each plan filter shows in the New phase: E1 stands, E2 demolished, E3/E4 gone, N1/N2 new
    let m = &ed.model;
    let counts = |p: i64| PhaseFilter::ALL.map(|f| shown(m, &all, p, f));
    // order of PhaseFilter::ALL: Off, Complete, Existing, Demolition, Project, NewOnly, All
    assert_eq!(counts(new), [4, 3, 2, 2, 3, 2, 4]);
    // in the Demolition phase: E1/E2 stand, E3/E4 demolished, new walls are in the future
    assert_eq!(counts(demo), [4, 2, 4, 4, 2, 0, 4]);

    // the plan's phase view is its own undo step
    let vp = ViewPhase {
        phase: new,
        filter: PhaseFilter::Project,
    };
    assert_eq!(
        ed.model.view_phase(ViewKey::Plan(ed.level)).unwrap(),
        ViewPhase {
            phase: new,
            filter: PhaseFilter::Off
        }
    );
    let l = ed.level;
    op(&mut ed, move |m| m.set_view_phase(ViewKey::Plan(l), vp));
    assert_eq!(ed.model.view_phase(ViewKey::Plan(ed.level)).unwrap(), vp);
    let after_view = dump_data(&ed.model);

    // schedule cell: a new wall demolished in its own phase is temporary
    let tb = schedule::build(&ed, Cat::Walls);
    let row = tb.rows.iter().position(|r| r.id == n2).unwrap();
    schedule::apply_edit(&mut ed, &tb, row, jd, &labels[2]);
    assert_clean(&ed.model, "schedule demolish cell");
    assert_eq!(
        ed.model.phase_index().unwrap().status("wall", n2, new),
        PhaseStatus::Temporary
    );
    assert_eq!(
        rows(&ed, Cat::Walls, PhaseMode::Demolished(new)),
        sorted(vec![e2, n2])
    );
    assert_eq!(shown(&ed.model, &all, new, PhaseFilter::Complete), 2);
    let tb = schedule::build(&ed, Cat::Walls);
    let row = tb.rows.iter().position(|r| r.id == n2).unwrap();
    assert_eq!(tb.rows[row].vals[jd], Val::T(labels[2].clone()));
    let after_cell = dump_data(&ed.model);

    // undo the cell, the view, both demolitions: the built model comes back each time
    ed.undo();
    assert_clean(&ed.model, "undo cell");
    assert_same(&dump_data(&ed.model), &after_view, "undo cell");
    assert_eq!(ed.model.element_phase("wall", n2).unwrap().demolished, None);
    ed.undo();
    assert_same(&dump_data(&ed.model), &after_c, "undo view");
    assert_eq!(
        ed.model.view_phase(ViewKey::Plan(ed.level)).unwrap(),
        ViewPhase {
            phase: new,
            filter: PhaseFilter::Off
        }
    );
    ed.undo();
    assert_clean(&ed.model, "undo wall 2");
    assert_same(&dump_data(&ed.model), &after_b, "undo wall 2");
    assert_eq!(
        rows(&ed, Cat::Doors, PhaseMode::Demolished(new)),
        Vec::<i64>::new()
    );
    assert_eq!(
        ed.model.element_phase("opening", door).unwrap().demolished,
        None
    );
    ed.undo();
    assert_clean(&ed.model, "undo walls 3 and 4");
    assert_same(&dump_data(&ed.model), &built, "undo walls 3 and 4");
    assert_eq!(rows(&ed, Cat::Walls, PhaseMode::In(new)), all);
    assert_eq!(ed.model.demolition(demo).unwrap().count, 0);

    // redo everything
    for _ in 0..4 {
        ed.redo();
    }
    assert_clean(&ed.model, "redo all");
    assert_same(&dump_data(&ed.model), &after_cell, "redo all");
    assert_eq!(
        rows(&ed, Cat::Walls, PhaseMode::Demolished(new)),
        sorted(vec![e2, n2])
    );
    assert_eq!(
        rows(&ed, Cat::Doors, PhaseMode::Demolished(new)),
        vec![door]
    );
}

#[test]
fn e2e_phases_refusals_and_delete_keep_phase_rows_consistent() {
    let mut ed = Editor::new_in_memory().unwrap();
    let ps: Vec<i64> = ed.model.phases().unwrap().iter().map(|p| p.id).collect();
    let (ex, new) = (ps[0], ps[2]);
    ed.model.set_current_phase(ex).unwrap();
    let w = add_wall(&mut ed, (0.0, 0.0), (5.0, 0.0));
    let keep = add_wall(&mut ed, (0.0, 3.0), (5.0, 3.0));

    // a wall created in New cannot be demolished in Existing: refused, nothing recorded
    op(&mut ed, move |m| {
        m.set_element_phases(&[("wall", w)], Some(new), None)
    });
    let (journal, state) = (journal_len(&ed.model), dump_data(&ed.model));
    let r = ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        m.demolish(&[("wall", w)], ex).map(drop)
    })));
    assert!(r.is_err());
    assert_eq!(journal_len(&ed.model), journal);
    assert_same(&dump_data(&ed.model), &state, "refused demolish");
    assert_clean(&ed.model, "refused demolish");

    // demolish, then delete the demolished wall: the phase row goes with it and comes back
    op(&mut ed, move |m| {
        m.set_element_phases(&[("wall", w)], Some(ex), Some(Some(new)))
    });
    let demolished = dump_data(&ed.model);
    op(&mut ed, move |m| m.delete_wall(w));
    assert_eq!(rows(&ed, Cat::Walls, PhaseMode::All), vec![keep]);
    assert_eq!(ed.model.demolition(new).unwrap().count, 0);
    ed.undo();
    assert_clean(&ed.model, "undo delete");
    assert_same(&dump_data(&ed.model), &demolished, "undo delete");
    assert_eq!(
        ed.model.element_phase("wall", w).unwrap(),
        ElemPhase {
            created: ex,
            demolished: Some(new)
        }
    );
    assert_eq!(rows(&ed, Cat::Walls, PhaseMode::Demolished(new)), vec![w]);
    // the schedule lists the demolished wall as "existing, demolished in New"
    assert!(rows(&ed, Cat::Walls, PhaseMode::In(new)).contains(&w));
    assert!(rows(&ed, Cat::Walls, PhaseMode::In(ex)).contains(&keep));
}
