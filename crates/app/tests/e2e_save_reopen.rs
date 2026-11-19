// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! End-to-end: a rich model is saved (`snapshot_to`) and reopened (`Model::open`); the data,
//! schema version and undo journal survive, the editor takes the reopened model, and
//! reopening is idempotent. The heavy variant does the same for an imported IFC file.

mod e2e_common;

use std::path::Path;
use std::sync::Arc;

use ddc_app::editor::{Editor, OpFn};
use ddc_model::{
    DimKind, DimRef, DimSpec, GridSpec, LayerSpec, Model, OpeningSpec, RoofKind, RoofSpec,
    RoomSpec, SchedulePlace, SheetDef, SlabSpec, TagCat, ViewRef, ViewportDef, WallSpec,
    DEFAULT_CUT, SCHEMA_VERSION,
};
use e2e_common::{assert_clean, assert_same, corpus, count, dump_data, journal_len, temp_file};

const TABLES: [&str; 13] = [
    "level",
    "wall_type",
    "wall",
    "opening",
    "slab_type",
    "slab",
    "roof",
    "room",
    "grid",
    "dim",
    "sheet",
    "viewport",
    "sheet_schedule",
];

fn layer(material: &str, thickness: f64) -> LayerSpec {
    LayerSpec {
        material: material.into(),
        thickness,
    }
}

fn opening(along: f64, width: f64, sill: f64, height: f64, kind: &str) -> OpeningSpec {
    OpeningSpec {
        along,
        width,
        sill,
        height,
        kind: kind.into(),
    }
}

/// Two levels, two wall types, a 10x6 ring with a partition, doors and windows, slab, roof,
/// two rooms, grid, dimension, tags, a sheet with a plan and a schedule. Returns the level.
fn build(m: &mut Model) -> i64 {
    let lv = m.add_level("L1", 0.0).unwrap();
    m.add_level("L2", 3.0).unwrap();
    let outer = m
        .add_wall_type(
            "Outer",
            &[
                layer("plaster", 0.02),
                layer("masonry", 0.25),
                layer("insulation", 0.1),
            ],
        )
        .unwrap();
    let inner = m.add_wall_type("Inner", &[layer("gypsum", 0.1)]).unwrap();
    for (name, a, b) in [
        ("A", (0.0, -1.0), (0.0, 7.0)),
        ("1", (-1.0, 0.0), (11.0, 0.0)),
    ] {
        m.add_grid(&GridSpec::line(name, a, b)).unwrap();
    }
    let c = [(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)];
    let wall = |a, b, wall_type| WallSpec {
        a,
        b,
        level: lv,
        base_offset: 0.0,
        height: 3.0,
        wall_type,
        offset0: None,
    };
    let ring: Vec<WallSpec> = (0..4).map(|k| wall(c[k], c[(k + 1) % 4], outer)).collect();
    let w = m.add_walls(&ring).unwrap();
    let w4 = m.add_wall(&wall((5.0, 0.0), (5.0, 6.0), inner)).unwrap();
    m.add_opening(w[0], &opening(1.0, 0.9, 0.0, 2.1, "door"))
        .unwrap();
    m.add_opening(w4, &opening(2.5, 0.9, 0.0, 2.1, "door"))
        .unwrap();
    m.add_opening(w[2], &opening(3.0, 1.5, 0.9, 1.5, "window"))
        .unwrap();
    m.add_opening(w[1], &opening(2.0, 1.2, 0.9, 1.5, "window"))
        .unwrap();
    let slab_t = m
        .add_slab_type("Slab 200", &[layer("concrete", 0.2)])
        .unwrap();
    // a layer text names a material; opening a file adds the row, so the fixture has it already
    m.add_material(&ddc_model::material_defaults("tile"))
        .unwrap();
    let roof_t = m.add_slab_type("Roof 200", &[layer("tile", 0.2)]).unwrap();
    m.add_slab(&SlabSpec {
        level: lv,
        slab_type: slab_t,
        offset: 0.0,
        outer: c.to_vec(),
        holes: vec![],
    })
    .unwrap();
    m.add_roof(&RoofSpec {
        level: lv,
        slab_type: roof_t,
        offset: 3.0,
        overhang: 0.4,
        fascia: 0.2,
        kind: RoofKind::Footprint {
            ring: c.to_vec(),
            slopes: vec![Some(30.0), None, Some(30.0), None],
            mansard: None,
        },
    })
    .unwrap();
    for (name, number, point) in [("Office", "101", (2.5, 3.0)), ("Store", "102", (7.5, 3.0))] {
        m.add_room(&RoomSpec {
            level: lv,
            name: name.into(),
            number: number.into(),
            point,
            height: 3.0,
        })
        .unwrap();
    }
    m.add_dim(&DimSpec {
        level: lv,
        kind: DimKind::Linear,
        refs: vec![
            DimRef::AxisEnd { wall: w[0], end: 0 },
            DimRef::AxisEnd { wall: w[0], end: 1 },
        ],
        dir: (0.0, 0.0),
        side: -1.0,
        offset: 1.0,
        tier: -1,
        auto: false,
    })
    .unwrap();
    m.tag_all(lv, &[TagCat::Door, TagCat::Window, TagCat::Room], false)
        .unwrap();
    let sheet = m.add_sheet(&SheetDef::new("A-101", "Plans")).unwrap();
    m.add_viewport(&ViewportDef {
        sheet,
        view: ViewRef::Plan {
            level: lv,
            cut: DEFAULT_CUT,
        },
        scale: 100.0,
        center: (150.0, 150.0),
        crop: None,
        title: "Plan".into(),
    })
    .unwrap();
    m.add_schedule_place(&SchedulePlace {
        sheet,
        schedule: "walls".into(),
        pos: (300.0, 280.0),
    })
    .unwrap();
    lv
}

/// A last, simple edit: its undo must work on the reopened file.
fn last_op(m: &mut Model) {
    m.add_grid(&GridSpec::line("Z", (12.0, -1.0), (12.0, 7.0)))
        .unwrap();
}

fn cleanup(p: &Path) {
    for ext in ["", "-wal", "-shm"] {
        let mut s = p.as_os_str().to_owned();
        s.push(ext);
        let _ = std::fs::remove_file(s);
    }
}

fn open(p: &Path) -> Model {
    Model::open(p.to_str().unwrap()).unwrap_or_else(|e| panic!("open {}: {e}", p.display()))
}

fn kinds(m: &Model) -> Vec<String> {
    let mut st = m.sql().prepare("SELECT kind FROM op ORDER BY seq").unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .map(|x| x.unwrap())
        .collect()
}

fn undo_all(m: &mut Model) -> usize {
    let mut n = 0;
    loop {
        match m.undo() {
            Ok(()) => n += 1,
            Err(ddc_model::Error::NothingToUndo) => return n,
            Err(e) => panic!("undo {n}: {e}"),
        }
    }
}

#[test]
fn e2e_save_reopen_equal_data() {
    let mut m = Model::open(":memory:").unwrap();
    build(&mut m);
    assert_clean(&m, "built");
    for t in TABLES {
        assert!(count(&m, t) > 0, "rich model has {t}");
    }
    let want = dump_data(&m);
    let p = temp_file("reopen_equal.ddc");
    m.snapshot_to(&p).unwrap();
    assert!(e2e_common::exists(&p));

    let r = open(&p);
    assert_eq!(r.schema_version().unwrap(), SCHEMA_VERSION);
    assert_same(&dump_data(&r), &want, "reopened");
    assert_clean(&r, "reopened");
    for t in TABLES {
        assert_eq!(count(&r, t), count(&m, t), "{t} rows");
    }
    // the undo journal is part of the file
    assert_eq!(journal_len(&r), journal_len(&m));
    assert_eq!(kinds(&r), kinds(&m));
    drop(r);
    cleanup(&p);
}

#[test]
fn e2e_reopen_twice_is_idempotent() {
    let mut m = Model::open(":memory:").unwrap();
    build(&mut m);
    let want = dump_data(&m);
    let (p1, p2) = (temp_file("reopen_a.ddc"), temp_file("reopen_b.ddc"));
    m.snapshot_to(&p1).unwrap();

    let first = open(&p1);
    assert_same(&dump_data(&first), &want, "first reopen");
    drop(first);
    let second = open(&p1);
    assert_same(&dump_data(&second), &want, "second reopen of the same file");
    // save the reopened model again: still the same data
    second.snapshot_to(&p2).unwrap();
    drop(second);
    let third = open(&p2);
    assert_eq!(third.schema_version().unwrap(), SCHEMA_VERSION);
    assert_same(&dump_data(&third), &want, "re-saved copy");
    assert_clean(&third, "re-saved copy");
    drop(third);
    cleanup(&p1);
    cleanup(&p2);
}

#[test]
fn e2e_reopen_keeps_undo_journal() {
    // journal semantics (model/src/ops.rs): the `op` table is stored in the file, so a reopened
    // model undoes the last edit made before saving, one journal entry per `undo()`.
    let mut m = Model::open(":memory:").unwrap();
    build(&mut m);
    let before_last = dump_data(&m);
    last_op(&mut m);
    let after_last = dump_data(&m);
    let p = temp_file("reopen_undo.ddc");
    m.snapshot_to(&p).unwrap();

    let mut r = open(&p);
    assert_same(&dump_data(&r), &after_last, "reopened");
    let j = journal_len(&r);
    assert_eq!(j, journal_len(&m));
    assert!(j > 0);
    r.undo().unwrap();
    assert_eq!(journal_len(&r), j - 1);
    assert_same(
        &dump_data(&r),
        &before_last,
        "undo of the last edit after reopen",
    );
    assert_clean(&r, "after undo");
    drop(r);
    cleanup(&p);
}

#[test]
fn e2e_reopen_undo_all_matches_original() {
    let mut m = Model::open(":memory:").unwrap();
    build(&mut m);
    last_op(&mut m);
    let p = temp_file("reopen_undo_all.ddc");
    m.snapshot_to(&p).unwrap();
    let mut r = open(&p);

    let n_orig = undo_all(&mut m);
    let n_re = undo_all(&mut r);
    assert_eq!(n_re, n_orig, "same number of undo steps");
    assert_eq!(journal_len(&r), 0);
    assert_same(
        &dump_data(&r),
        &dump_data(&m),
        "everything undone on the reopened copy",
    );
    assert_clean(&r, "undone");
    assert!(matches!(r.undo(), Err(ddc_model::Error::NothingToUndo)));
    drop(r);
    cleanup(&p);
}

#[test]
fn e2e_editor_takes_reopened_model() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = build(&mut m);
    last_op(&mut m);
    let p = temp_file("reopen_editor.ddc");
    m.snapshot_to(&p).unwrap();
    let want = dump_data(&m);
    let walls = e2e_common::ids(&m, "wall").len();

    let mut ed = Editor::new_in_memory().unwrap();
    ed.replace_model(open(&p)).unwrap();
    assert_eq!(ed.level, lv, "first level of the file stays the active one");
    assert_eq!(ed.walls.len(), walls);
    assert_eq!(ed.openings.len(), 4);
    assert_eq!(ed.slabs.len(), 1);
    assert_eq!(ed.rooms.len(), 2);
    assert_same(&dump_data(&ed.model), &want, "editor model");

    // replace_model drops the editor history even though the file keeps its journal
    assert!(!ed.can_undo() && !ed.can_redo());
    ed.undo();
    assert_same(
        &dump_data(&ed.model),
        &want,
        "undo without history changes nothing",
    );
    assert!(journal_len(&ed.model) > 0);

    // a new edit on the reopened model is a normal undo step
    let before = journal_len(&ed.model);
    let spec = WallSpec {
        a: (20.0, 0.0),
        b: (24.0, 0.0),
        level: ed.level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: ed.types[0].id,
        offset0: None,
    };
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        m.add_wall(&spec).map(|_| ())
    })))
    .unwrap();
    assert_eq!(journal_len(&ed.model), before + 1);
    assert_eq!(ed.walls.len(), walls + 1);
    assert_clean(&ed.model, "new wall after reopen");
    ed.undo();
    assert_eq!(ed.walls.len(), walls);
    assert_same(&dump_data(&ed.model), &want, "undo of the new wall");
    ed.redo();
    assert_eq!(ed.walls.len(), walls + 1);
    drop(ed);
    cleanup(&p);
}

#[test]
#[ignore = "corpus"]
fn e2e_ifc_import_save_reopen() {
    let Some(file) = corpus("DDC_E2E_IFC_SMALL", "") else {
        return;
    };
    let mut m = Model::open(":memory:").unwrap();
    let r = ddc_io::ifc::import_ifc_file(&mut m, &file)
        .unwrap_or_else(|e| panic!("{}: {e:?}", file.display()));
    assert!(
        r.walls + r.slabs + r.rooms + r.levels > 0,
        "something was imported"
    );
    let issues = format!("{:?}", m.check_integrity().unwrap());
    let want = dump_data(&m);
    let p = temp_file("reopen_ifc.ddc");
    m.snapshot_to(&p).unwrap();

    let mut re = open(&p);
    assert_eq!(re.schema_version().unwrap(), SCHEMA_VERSION);
    assert_same(&dump_data(&re), &want, "reopened IFC import");
    assert_eq!(format!("{:?}", re.check_integrity().unwrap()), issues);
    assert_eq!(journal_len(&re), journal_len(&m));
    // the whole import is one undo step in the saved journal as well
    m.undo().unwrap();
    re.undo().unwrap();
    assert_same(
        &dump_data(&re),
        &dump_data(&m),
        "import undone after reopen",
    );
    drop(re);
    cleanup(&p);
}
