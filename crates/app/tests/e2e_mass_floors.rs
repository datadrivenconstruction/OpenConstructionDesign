//! Workflow: masses → floors from mass → elements by face → edit the mass (hosted elements
//! follow) → undo all → model equals the fresh project. Every edit is one `run_op`.

mod e2e_common;

use std::sync::Arc;

use ddc_app::editor::{Editor, OpFn};
use ddc_model::{FaceClass, Form, LayerSpec, MassSpec, Model};
use e2e_common::{assert_clean, assert_same, dump, ids, journal_len};

fn op(
    ed: &mut Editor,
    f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) {
    let j = journal_len(&ed.model);
    ed.run_op(OpFn(Arc::new(f))).unwrap();
    assert_eq!(
        journal_len(&ed.model),
        j + 1,
        "one edit = one journal entry"
    );
    assert_clean(&ed.model, "after op");
}

fn last(m: &Model, table: &str) -> i64 {
    *ids(m, table)
        .last()
        .unwrap_or_else(|| panic!("no rows in {table}"))
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Vec<(f64, f64)> {
    vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
}

fn slab_area(m: &Model, s: i64) -> f64 {
    m.slab_quantities(s).unwrap().iter().map(|q| q.area).sum()
}

/// Face tag of a planar face by class and outward normal.
fn face(m: &Model, mass: i64, class: FaceClass, n: (f64, f64, f64)) -> u32 {
    let fs = m.mass_faces(mass).unwrap();
    fs.iter()
        .find(|f| {
            f.class == class
                && (f.normal.0 - n.0).abs() < 1e-6
                && (f.normal.1 - n.1).abs() < 1e-6
                && (f.normal.2 - n.2).abs() < 1e-6
        })
        .map(|f| f.tag)
        .unwrap_or_else(|| panic!("no {class:?} face with normal {n:?}: {fs:?}"))
}

fn wall_geom(m: &Model, w: i64) -> (f64, f64, f64, f64, f64) {
    m.sql()
        .query_row(
            "SELECT ax,ay,bx,by,height FROM wall WHERE id=?1",
            [w],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap()
}

#[test]
fn e2e_mass_floors_faces_edit_and_undo_all() {
    let mut ed = Editor::new_in_memory().unwrap();
    let start = dump(&ed.model);
    let base = ed.level;
    let wt = ed.types[1].id;
    // levels at 0 (project default), 3, 6, 9
    for k in 1..4 {
        let z = 3.0 * k as f64;
        op(&mut ed, move |m| m.add_level(&format!("L{k}"), z).map(drop));
    }
    op(&mut ed, |m| {
        m.add_slab_type(
            "Floor 200",
            &[LayerSpec {
                material: "Concrete".into(),
                thickness: 0.2,
            }],
        )
        .map(drop)
    });
    let st = last(&ed.model, "slab_type");

    // 20 × 10 × 9 tower: floors at 0, 3, 6 (the top level 9 is the roof plane)
    let spec = MassSpec {
        level: base,
        name: "Tower".into(),
        void: false,
        inplace: false,
        form: Form::Extrude {
            ring: rect(0.0, 0.0, 20.0, 10.0),
            z0: 0.0,
            h: 9.0,
            ids: vec![],
        },
    };
    op(&mut ed, move |m| m.add_mass(&spec).map(drop));
    let mass = last(&ed.model, "mass");
    let rows = ed.model.mass_schedule().unwrap();
    assert_eq!(rows.len(), 3, "{rows:?}");
    for r in &rows {
        assert!((r.area - 200.0).abs() < 1e-6, "{r:?}");
        assert!((r.perimeter - 60.0).abs() < 1e-6, "{r:?}");
        assert!((r.volume - 1800.0).abs() < 1e-6, "{r:?}");
    }

    // floors from mass: one slab per crossing level, hosted by the mass
    op(&mut ed, move |m| m.floor_slabs(mass, st).map(drop));
    let floors: Vec<i64> = ed
        .model
        .mass_hosts(mass)
        .unwrap()
        .into_iter()
        .filter(|h| h.kind == "slab" && h.floor_level.is_some())
        .map(|h| h.id)
        .collect();
    assert_eq!(floors.len(), 3);
    for &s in &floors {
        assert!((slab_area(&ed.model, s) - 200.0).abs() < 1e-6);
    }

    // wall by the south face: full face height
    let south = face(&ed.model, mass, FaceClass::Vertical, (0.0, -1.0, 0.0));
    op(&mut ed, move |m| m.wall_by_face(mass, south, wt).map(drop));
    let wall = last(&ed.model, "wall");
    let (_, _, _, _, h) = wall_geom(&ed.model, wall);
    assert!((h - 9.0).abs() < 1e-9, "{h}");
    let built = dump(&ed.model);

    // push the east face out by 5 m: floor areas follow (250), schedule too
    let east = face(&ed.model, mass, FaceClass::Vertical, (1.0, 0.0, 0.0));
    op(&mut ed, move |m| {
        m.edit_mass_form(mass, |b| Form::PushPull {
            base: b,
            face: east,
            d: 5.0,
            keys: vec![],
        })
    });
    for &s in &floors {
        let a = slab_area(&ed.model, s);
        assert!((a - 250.0).abs() < 1e-6, "floor {s} after push-pull: {a}");
    }
    assert!(ed
        .model
        .mass_schedule()
        .unwrap()
        .iter()
        .all(|r| (r.area - 250.0).abs() < 1e-6));
    let (ax, _, bx, _, _) = wall_geom(&ed.model, wall);
    assert!(
        ((bx - ax).abs() - 25.0).abs() < 1e-6,
        "south wall follows its face: {ax} .. {bx}"
    );

    // push the top up by 3 m: a fourth floor appears in the schedule, the wall grows
    let top = face(&ed.model, mass, FaceClass::Up, (0.0, 0.0, 1.0));
    op(&mut ed, move |m| {
        m.edit_mass_form(mass, |b| Form::PushPull {
            base: b,
            face: top,
            d: 3.0,
            keys: vec![],
        })
    });
    assert_eq!(ed.model.mass_schedule().unwrap().len(), 4);
    let (_, _, _, _, h) = wall_geom(&ed.model, wall);
    assert!((h - 12.0).abs() < 1e-6, "{h}");

    // undo the two form edits: exactly the state with floors and wall
    ed.undo();
    ed.undo();
    assert_clean(&ed.model, "undo form edits");
    assert_same(&dump(&ed.model), &built, "undo form edits");
    for &s in &floors {
        assert!((slab_area(&ed.model, s) - 200.0).abs() < 1e-6);
    }

    // undo everything: the fresh project
    while ed.can_undo() {
        ed.undo();
        assert_clean(&ed.model, "undo");
    }
    assert_same(&dump(&ed.model), &start, "undo all");
    assert!(ed.model.masses().unwrap().is_empty());
}

#[test]
fn e2e_mass_floors_need_a_crossing_level_and_move_with_the_mass() {
    let mut ed = Editor::new_in_memory().unwrap();
    let base = ed.level;
    op(&mut ed, |m| m.add_level("L1", 3.0).map(drop));
    op(&mut ed, |m| {
        m.add_slab_type(
            "F",
            &[LayerSpec {
                material: "Concrete".into(),
                thickness: 0.2,
            }],
        )
        .map(drop)
    });
    let st = last(&ed.model, "slab_type");
    // a mass floating above every level: no floor can be made, nothing is written
    let high = MassSpec {
        level: base,
        name: "High".into(),
        void: false,
        inplace: false,
        form: Form::Cuboid {
            lo: (0.0, 0.0, 10.0),
            hi: (4.0, 4.0, 12.0),
        },
    };
    op(&mut ed, move |m| m.add_mass(&high).map(drop));
    let mass = last(&ed.model, "mass");
    let before = dump(&ed.model);
    let r = ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        m.floor_slabs(mass, st).map(drop)
    })));
    assert!(r.is_err(), "no level crosses the mass");
    assert_same(&dump(&ed.model), &before, "refused floors");

    // a box over both levels; moving the mass carries its floor slabs
    let low = MassSpec {
        level: base,
        name: "Low".into(),
        void: false,
        inplace: false,
        form: Form::Cuboid {
            lo: (0.0, 0.0, 0.0),
            hi: (6.0, 5.0, 6.0),
        },
    };
    op(&mut ed, move |m| m.add_mass(&low).map(drop));
    let box_ = last(&ed.model, "mass");
    op(&mut ed, move |m| m.floor_slabs(box_, st).map(drop));
    let fl = ids(&ed.model, "slab");
    assert_eq!(fl.len(), 2);
    let centroid = |m: &Model, s: i64| -> (f64, f64) {
        let r = &m.slab_rings(s).unwrap()[0];
        let n = r.len() as f64;
        (
            r.iter().map(|p| p.0).sum::<f64>() / n,
            r.iter().map(|p| p.1).sum::<f64>() / n,
        )
    };
    let c0 = centroid(&ed.model, fl[0]);
    op(&mut ed, move |m| m.move_mass(box_, (10.0, 0.0, 0.0)));
    let c1 = centroid(&ed.model, fl[0]);
    assert!(
        (c1.0 - c0.0 - 10.0).abs() < 1e-6 && (c1.1 - c0.1).abs() < 1e-6,
        "{c0:?} -> {c1:?}"
    );
    ed.undo();
    let c2 = centroid(&ed.model, fl[0]);
    assert!((c2.0 - c0.0).abs() < 1e-9 && (c2.1 - c0.1).abs() < 1e-9);
}
