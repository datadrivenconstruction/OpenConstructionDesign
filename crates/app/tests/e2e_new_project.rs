// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! End-to-end: a new project built the way a user does it (levels, grids, walls, openings,
//! slab, roof, rooms, dimensions, tags, schedules, a sheet with a plan and schedules, PDF),
//! every edit one `run_op` = one undo step, then undo/redo all the way.

mod e2e_common;
#[path = "app2_fx/mod.rs"]
mod app2_fx;

use app2_fx::fx;

use std::sync::Arc;

use ddc_app::editor::{Editor, OpFn};
use ddc_app::schedule::{self, Cat};
use ddc_app::sheet_view::schedule_table;
use ddc_io::pdf::import::PdfFile;
use ddc_io::sheets::{export_sheets_pdf, Ctx, LengthUnits};
use ddc_model::{
    DimKind, DimRef, DimSpec, GridSpec, LayerSpec, Model, OpeningSpec, RoofKind, RoofSpec,
    RoomSpec, SchedulePlace, SheetDef, SlabSpec, TagCat, ViewRef, ViewportDef, WallSpec,
    DEFAULT_CUT,
};
use e2e_common::{assert_clean, assert_same, count, dump_data, ids, journal_len};

type R = Result<(), ddc_model::Error>;

/// One model edit as one undo step; the journal must grow by exactly one entry.
fn step(ed: &mut Editor, ctx: &str, f: impl Fn(&mut Model) -> R + Send + Sync + 'static) {
    let before = journal_len(&ed.model);
    ed.run_op(OpFn(Arc::new(f)))
        .unwrap_or_else(|e| panic!("{ctx}: {e}"));
    assert_eq!(
        journal_len(&ed.model),
        before + 1,
        "{ctx}: one run_op must be one journal entry (one undo)"
    );
    assert_clean(&ed.model, ctx);
}

fn n(ed: &Editor, sql: &str) -> i64 {
    ed.model.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn rows(ed: &Editor, cat: Cat) -> usize {
    schedule::build(ed, cat).rows.len()
}

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

#[test]
fn e2e_new_project_to_pdf() {
    let mut ed = Editor::new_in_memory().unwrap();
    let base = dump_data(&ed.model);
    let base_journal = journal_len(&ed.model);
    assert_clean(&ed.model, "fresh project");
    let lv = ed.level;
    let (outer_t, inner_t) = (ed.types[2].id, ed.types[0].id);
    let mut steps = 0usize;
    macro_rules! s {
        ($ctx:expr, $f:expr) => {{
            step(&mut ed, $ctx, $f);
            steps += 1;
        }};
    }

    // levels and grids
    assert_eq!(count(&ed.model, "level"), 1);
    s!("level 2", |m| m.add_level(&fx("np_floor2"), 3.0).map(|_| ()));
    assert_eq!(count(&ed.model, "level"), 2);
    for (name, a, b) in [
        ("A", (0.0, -1.0), (0.0, 7.0)),
        ("B", (10.0, -1.0), (10.0, 7.0)),
        ("1", (-1.0, 0.0), (11.0, 0.0)),
        ("2", (-1.0, 6.0), (11.0, 6.0)),
    ] {
        s!("grid", move |m| m
            .add_grid(&GridSpec::line(name, a, b))
            .map(|_| ()));
    }
    assert_eq!(count(&ed.model, "grid"), 4);

    // walls: closed 10x6 rectangle in one op, an inner partition in another
    let c = [(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)];
    let wall = move |a: (f64, f64), b: (f64, f64), wall_type: i64| WallSpec {
        a,
        b,
        level: lv,
        base_offset: 0.0,
        height: 3.0,
        wall_type,
        offset0: None,
    };
    let ring: Vec<WallSpec> = (0..4)
        .map(|k| wall(c[k], c[(k + 1) % 4], outer_t))
        .collect();
    s!("outer walls", move |m| m.add_walls(&ring).map(|_| ()));
    assert_eq!(count(&ed.model, "wall"), 4);
    let inner = wall((5.0, 0.0), (5.0, 6.0), inner_t);
    s!("inner wall", move |m| m.add_wall(&inner).map(|_| ()));
    assert_eq!(count(&ed.model, "wall"), 5);
    assert_eq!(ed.walls.len(), 5, "editor sees the walls of its level");
    let w = ids(&ed.model, "wall");

    // doors and windows in walls
    let (w0, w1, w2, w4) = (w[0], w[1], w[2], w[4]);
    s!("door", move |m| m
        .add_opening(w0, &opening(1.0, 0.9, 0.0, 2.1, "door"))
        .map(|_| ()));
    s!("inner door", move |m| m
        .add_opening(w4, &opening(2.5, 0.9, 0.0, 2.1, "door"))
        .map(|_| ()));
    s!("window top", move |m| m
        .add_opening(w2, &opening(3.0, 1.5, 0.9, 1.5, "window"))
        .map(|_| ()));
    s!("window east", move |m| m
        .add_opening(w1, &opening(2.0, 1.2, 0.9, 1.5, "window"))
        .map(|_| ()));
    assert_eq!(count(&ed.model, "opening"), 4);
    assert_eq!(ed.openings.len(), 4);

    // slab and roof (each with its own type)
    let types0 = count(&ed.model, "slab_type");
    s!("slab type", |m| m
        .add_slab_type(&fx("np_slab200"),&[layer("concrete", 0.2)])
        .map(|_| ()));
    s!("roof type", |m| m
        .add_slab_type(&fx("np_roof200"),&[layer("tile", 0.2)])
        .map(|_| ()));
    assert_eq!(count(&ed.model, "slab_type"), types0 + 2);
    let st = ids(&ed.model, "slab_type");
    let (slab_t, roof_t) = (st[st.len() - 2], st[st.len() - 1]);
    let slab = SlabSpec {
        level: lv,
        slab_type: slab_t,
        offset: 0.0,
        outer: c.to_vec(),
        holes: vec![],
    };
    s!("slab", move |m| m.add_slab(&slab).map(|_| ()));
    assert_eq!(count(&ed.model, "slab"), 1);
    assert_eq!(ed.slabs.len(), 1);
    let roof = RoofSpec {
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
    };
    s!("roof", move |m| m.add_roof(&roof).map(|_| ()));
    assert_eq!(count(&ed.model, "roof"), 1);

    // rooms: one point in each half split by the partition
    for (name, number, point) in [
        (fx("np_office"), "101", (2.5, 3.0)),
        (fx("np_storage"), "102", (7.5, 3.0)),
    ] {
        s!("room", move |m| m
            .add_room(&RoomSpec {
                level: lv,
                name: name.as_str().into(),
                number: number.into(),
                point,
                height: 3.0,
            })
            .map(|_| ()));
    }
    let rooms = ids(&ed.model, "room");
    assert_eq!(rooms.len(), 2);
    assert_eq!(ed.rooms.len(), 2);
    for r in &rooms {
        let q = ed
            .model
            .room_quantities(*r)
            .unwrap()
            .expect("room quantities");
        assert!(
            q.area > 10.0 && q.area < 40.0,
            "partition must split the 60 m2 plan, room {r} area {}",
            q.area
        );
        assert!(q.perimeter > 0.0 && q.volume > 0.0);
    }

    // dimensions and tags
    let dims0 = n(&ed, "SELECT count(*) FROM dim WHERE kind<>'tag'");
    s!("dim axis", move |m| m
        .add_dim(&DimSpec {
            level: lv,
            kind: DimKind::Linear,
            refs: vec![
                DimRef::AxisEnd { wall: w0, end: 0 },
                DimRef::AxisEnd { wall: w0, end: 1 },
            ],
            dir: (0.0, 0.0),
            side: -1.0,
            offset: 1.0,
            tier: -1,
            auto: false,
        })
        .map(|_| ()));
    s!("dim points", move |m| m
        .add_dim(&DimSpec {
            level: lv,
            kind: DimKind::Linear,
            refs: vec![DimRef::Point((10.0, 0.0)), DimRef::Point((10.0, 6.0))],
            dir: (0.0, 0.0),
            side: 1.0,
            offset: 1.0,
            tier: -1,
            auto: false,
        })
        .map(|_| ()));
    assert_eq!(
        n(&ed, "SELECT count(*) FROM dim WHERE kind<>'tag'"),
        dims0 + 2
    );
    s!("auto dims", move |m| m.auto_dimension(lv).map(|_| ()));
    assert!(n(&ed, "SELECT count(*) FROM dim WHERE kind<>'tag'") >= dims0 + 2);
    assert_eq!(n(&ed, "SELECT count(*) FROM dim WHERE kind='tag'"), 0);
    s!("tag all", move |m| m
        .tag_all(lv, &[TagCat::Door, TagCat::Window, TagCat::Room], false)
        .map(|_| ()));
    assert_eq!(
        n(&ed, "SELECT count(*) FROM dim WHERE kind='tag'"),
        6,
        "2 doors + 2 windows + 2 rooms"
    );
    assert_eq!(ed.tags.len(), 6);

    // schedules from the editor model
    for (cat, want) in [
        (Cat::Walls, 5),
        (Cat::Doors, 2),
        (Cat::Windows, 2),
        (Cat::Rooms, 2),
    ] {
        assert_eq!(rows(&ed, cat), want, "{cat:?} schedule rows");
        let key = ddc_app::sheet_view::schedule_key(cat);
        let t = schedule_table(&ed, &key).expect("schedule table");
        assert_eq!(t.rows.len(), want, "{key} sheet table rows");
        assert!(!t.header.is_empty() && t.rows.iter().all(|r| r.len() == t.header.len()));
    }
    assert_eq!(rows(&ed, Cat::Slabs), schedule::count(&ed, Cat::Slabs));

    // sheet with a plan viewport and three schedules
    s!("sheet", |m| m
        .add_sheet(&SheetDef::new("A-101", &fx("np_plans")))
        .map(|_| ()));
    let sheet = ids(&ed.model, "sheet")[0];
    s!("viewport", move |m| m
        .add_viewport(&ViewportDef {
            sheet,
            view: ViewRef::Plan {
                level: lv,
                cut: DEFAULT_CUT,
            },
            scale: 100.0,
            center: (150.0, 150.0),
            crop: None,
            title: fx("np_plan_floor1"),
        })
        .map(|_| ()));
    for (key, pos) in [
        ("walls", (300.0, 280.0)),
        ("doors", (300.0, 210.0)),
        ("rooms", (300.0, 160.0)),
    ] {
        s!("schedule place", move |m| m
            .add_schedule_place(&SchedulePlace {
                sheet,
                schedule: key.into(),
                pos,
            })
            .map(|_| ()));
    }
    assert_eq!(count(&ed.model, "viewport"), 1);
    assert_eq!(count(&ed.model, "sheet_schedule"), 3);
    s!("sheet set", move |m| m
        .add_sheet_set(&fx("np_set"), &[sheet])
        .map(|_| ()));

    // PDF of the sheet
    let sched = |k: &str| schedule_table(&ed, k);
    let ctx = Ctx {
        units: LengthUnits::Metric,
        schedules: &sched,
        index: None,
        paper: None,
        text: &|k: &str| k.to_string(),
        refs: None,
    };
    let pdf = export_sheets_pdf(&ed.model, &[sheet], &ctx).unwrap();
    assert!(pdf.starts_with(b"%PDF"), "PDF header");
    assert!(pdf.len() > 3000, "PDF is not trivial: {} bytes", pdf.len());
    let tail = &pdf[pdf.len().saturating_sub(64)..];
    assert!(tail.windows(5).any(|w| w == b"%%EOF"), "PDF trailer");
    assert_eq!(PdfFile::parse(&pdf).unwrap().page_count(), 1);

    // undo one step and redo it: same data
    let full = dump_data(&ed.model);
    assert!(ed.can_undo());
    ed.undo();
    assert_ne!(
        e2e_common::data_hash(&ed.model),
        e2e_common::hash_of(&full),
        "undo changed the model"
    );
    assert_clean(&ed.model, "after one undo");
    ed.redo();
    assert_same(&dump_data(&ed.model), &full, "undo + redo of the last step");

    // undo everything: the fresh project again
    for k in 0..steps {
        assert!(ed.can_undo(), "undo step {k} of {steps}");
        ed.undo();
        assert_clean(&ed.model, "undo");
    }
    assert!(!ed.can_undo(), "exactly {steps} undo steps");
    assert_same(&dump_data(&ed.model), &base, "all undone");
    assert_eq!(journal_len(&ed.model), base_journal);
    assert_eq!(ed.walls.len(), 0);
    assert_eq!(count(&ed.model, "opening"), 0);

    // redo everything: the finished project again
    for k in 0..steps {
        assert!(ed.can_redo(), "redo step {k} of {steps}");
        ed.redo();
    }
    assert_same(&dump_data(&ed.model), &full, "all redone");
    assert_clean(&ed.model, "all redone");
}
