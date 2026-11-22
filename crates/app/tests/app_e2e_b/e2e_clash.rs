// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Workflow: clash run → fix with sleeve → re-run → status, through the editor
//! (every edit is one `run_op` = one undo step, as the clash window does it).
//! Clash statuses are check data outside the journal, so model comparisons skip `clash_item`.

use crate::e2e_common;

use std::sync::Arc;

use ddc_app::editor::{Editor, OpFn};
use ddc_model::{
    ClashFix, ClashItem, ClashParams, ClashSide, ClashStatus, LayerSpec, Model, PipeMaterial,
    RunSpec, SlabSpec, SystemKind, WallSpec,
};
use e2e_common::{assert_clean, assert_same, count, dump_except, ids, TableRows};

fn op(
    ed: &mut Editor,
    f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) {
    ed.run_op(OpFn(Arc::new(f))).unwrap();
    assert_clean(&ed.model, "after op");
}

fn last(m: &Model, table: &str) -> i64 {
    *ids(m, table)
        .last()
        .unwrap_or_else(|| panic!("no rows in {table}"))
}

fn state(m: &Model) -> Vec<TableRows> {
    dump_except(m, &["clash_item"])
}

fn pipes_vs_hosts() -> ClashParams {
    ClashParams {
        a: ClashSide::cats(&["pipe"]),
        b: ClashSide::cats(&["wall", "slab"]),
        ..Default::default()
    }
}

/// Wall on x=3 crossed by a horizontal pipe at z=1; slab at +3.2 crossed by a riser.
fn scene(ed: &mut Editor) -> (i64, i64) {
    let l = ed.level;
    let wt = ed.types[1].id;
    op(ed, move |m| {
        m.add_wall(&WallSpec {
            a: (3.0, -5.0),
            b: (3.0, 5.0),
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .map(drop)
    });
    let wall = last(&ed.model, "wall");
    op(ed, |m| {
        m.add_slab_type(
            "S200",
            &[LayerSpec {
                material: "Concrete".into(),
                thickness: 0.2,
            }],
        )
        .map(drop)
    });
    let st = last(&ed.model, "slab_type");
    op(ed, move |m| {
        m.add_slab(&SlabSpec {
            level: l,
            slab_type: st,
            offset: 3.2,
            outer: vec![(0.0, -5.0), (10.0, -5.0), (10.0, 5.0), (0.0, 5.0)],
            holes: vec![],
        })
        .map(drop)
    });
    let slab = last(&ed.model, "slab");
    op(ed, |m| {
        m.add_mep_system("CW", SystemKind::ColdWater).map(drop)
    });
    let sys = last(&ed.model, "mep_system");
    for pts in [
        vec![(0.0, 0.0, 1.0), (8.0, 0.0, 1.0)],
        vec![(6.0, 2.0, 2.0), (6.0, 2.0, 5.0)],
    ] {
        op(ed, move |m| {
            m.add_mep_run(&RunSpec::pipe(
                sys,
                l,
                PipeMaterial::Steel,
                50,
                pts.clone(),
            )?)
            .map(drop)
        });
    }
    (wall, slab)
}

/// Clash run + merge as the clash window does it; returns the merge summary.
fn rerun(m: &mut Model, p: &ClashParams) -> ddc_model::ClashMerge {
    let hits = m.clash_find(p).unwrap();
    m.clash_merge(&p.test_key(), &hits).unwrap()
}

fn item(m: &Model, host: &str) -> ClashItem {
    m.clash_items()
        .unwrap()
        .into_iter()
        .find(|i| i.a.0 == host || i.b.0 == host)
        .unwrap_or_else(|| panic!("no clash item with {host}"))
}

fn sleeve(ed: &mut Editor, it: &ClashItem) {
    let fix = ed.model.clash_fix_sleeve(it, 0.03).unwrap();
    op(ed, move |m| m.apply_clash_fix(&fix));
}

#[test]
fn e2e_clash_sleeve_fix_rerun_status_and_undo() {
    let mut ed = Editor::new_in_memory().unwrap();
    let (wall, slab) = scene(&mut ed);
    let built = state(&ed.model);
    let p = pipes_vs_hosts();

    // run 1: both pairs new
    let r1 = rerun(&mut ed.model, &p);
    assert_eq!((r1.new, r1.still, r1.resolved), (2, 0, 0), "{r1:?}");
    let w_it = item(&ed.model, "wall");
    assert_eq!(w_it.status, ClashStatus::New);
    assert!(w_it.a == ("wall".to_string(), wall) || w_it.b == ("wall".to_string(), wall));
    // checks write no journal entries and change no model data
    assert_same(&state(&ed.model), &built, "clash run");

    // sleeve in the wall: a void opening around the pipe
    let fix = ed.model.clash_fix_sleeve(&w_it, 0.03).unwrap();
    let ClashFix::Opening { wall: host, spec } = &fix else {
        panic!("wall sleeve must be an opening: {fix:?}")
    };
    assert_eq!((*host, spec.kind.as_str()), (wall, "void"));
    sleeve(&mut ed, &w_it);
    assert_eq!(count(&ed.model, "opening"), 1);

    // run 2: wall pair resolved, slab pair seen again → active
    let r2 = rerun(&mut ed.model, &p);
    assert_eq!((r2.new, r2.still, r2.resolved), (0, 1, 1), "{r2:?}");
    let w2 = item(&ed.model, "wall");
    assert_eq!((w2.status, w2.present), (ClashStatus::Resolved, false));
    assert_eq!(item(&ed.model, "slab").status, ClashStatus::Active);

    // sleeve in the slab: a hole ring
    let s_it = item(&ed.model, "slab");
    sleeve(&mut ed, &s_it);
    assert_eq!(ed.model.slab_rings(slab).unwrap().len(), 2);
    let r3 = rerun(&mut ed.model, &p);
    assert_eq!((r3.new, r3.still, r3.resolved), (0, 0, 1), "{r3:?}");
    assert!(ed.model.clash_find(&p).unwrap().is_empty());
    let fixed = state(&ed.model);

    // undo the slab hole: the pair comes back active (status history kept)
    ed.undo();
    assert_clean(&ed.model, "undo slab sleeve");
    assert_eq!(ed.model.slab_rings(slab).unwrap().len(), 1);
    let r4 = rerun(&mut ed.model, &p);
    assert_eq!((r4.new, r4.still), (0, 1), "{r4:?}");
    assert_eq!(item(&ed.model, "slab").status, ClashStatus::Active);

    // undo the wall sleeve: model is back to the built scene, both pairs present
    ed.undo();
    assert_clean(&ed.model, "undo wall sleeve");
    assert_same(&state(&ed.model), &built, "undo both sleeves");
    let r5 = rerun(&mut ed.model, &p);
    assert_eq!((r5.new, r5.still), (0, 2), "{r5:?}");
    assert!(ed
        .model
        .clash_items()
        .unwrap()
        .iter()
        .all(|i| i.present && i.status == ClashStatus::Active));

    // redo both fixes: the fixed model again, no clashes
    ed.redo();
    ed.redo();
    assert_clean(&ed.model, "redo sleeves");
    assert_same(&state(&ed.model), &fixed, "redo both sleeves");
    assert!(ed.model.clash_find(&p).unwrap().is_empty());
}

#[test]
fn e2e_clash_reviewed_status_survives_reruns_and_second_sleeve_refused() {
    let mut ed = Editor::new_in_memory().unwrap();
    scene(&mut ed);
    let p = pipes_vs_hosts();
    rerun(&mut ed.model, &p);
    let it = item(&ed.model, "wall");
    ed.model
        .clash_set_status(&p.test_key(), &it.a, &it.b, ClashStatus::Reviewed, "agreed")
        .unwrap();
    let r = rerun(&mut ed.model, &p);
    assert_eq!(r.still, 2);
    let again = item(&ed.model, "wall");
    assert_eq!(
        (again.status, again.comment.as_str()),
        (ClashStatus::Reviewed, "agreed")
    );
    sleeve(&mut ed, &again);
    // a second sleeve on the same spot would overlap the first opening
    assert!(ed.model.clash_fix_sleeve(&again, 0.03).is_err());
    let journal = e2e_common::journal_len(&ed.model);
    assert!(ed.can_undo());
    ed.undo();
    assert_eq!(e2e_common::journal_len(&ed.model), journal - 1);
}

/// k walls × n pipes: every crossing is a hit; one sleeve per hit, each one undo step.
#[test]
fn e2e_clash_grid_every_crossing_found_fixed_and_undone() {
    let (k, n) = (4usize, 3usize);
    let mut ed = Editor::new_in_memory().unwrap();
    let (l, wt) = (ed.level, ed.types[1].id);
    for i in 0..k {
        let x = 2.0 + 2.0 * i as f64;
        op(&mut ed, move |m| {
            m.add_wall(&WallSpec {
                a: (x, 0.0),
                b: (x, 10.0),
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .map(drop)
        });
    }
    op(&mut ed, |m| {
        m.add_mep_system("HW", SystemKind::HotWater).map(drop)
    });
    let sys = last(&ed.model, "mep_system");
    for j in 0..n {
        let y = 2.0 + 2.5 * j as f64;
        op(&mut ed, move |m| {
            let s = RunSpec::pipe(
                sys,
                l,
                PipeMaterial::Steel,
                50,
                vec![(0.5, y, 1.2), (2.0 * k as f64 + 1.5, y, 1.2)],
            )?;
            m.add_mep_run(&s).map(drop)
        });
    }
    let built = state(&ed.model);
    let p = ClashParams {
        a: ClashSide::cats(&["pipe"]),
        b: ClashSide::cats(&["wall"]),
        ..Default::default()
    };
    let r = rerun(&mut ed.model, &p);
    assert_eq!(r.new, k * n, "{r:?}");
    let items = ed.model.clash_items().unwrap();
    for it in &items {
        sleeve(&mut ed, it);
    }
    assert_eq!(count(&ed.model, "opening"), (k * n) as i64);
    let r2 = rerun(&mut ed.model, &p);
    assert_eq!((r2.new, r2.still, r2.resolved), (0, 0, k * n), "{r2:?}");
    for _ in 0..k * n {
        ed.undo();
    }
    assert_clean(&ed.model, "undo all sleeves");
    assert_same(&state(&ed.model), &built, "undo all sleeves");
    assert_eq!(ed.model.clash_find(&p).unwrap().len(), k * n);
}
