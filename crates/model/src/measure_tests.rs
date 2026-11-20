// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use ddc_geom::csg::ftag;

use crate::measure::{Anchor, MeasureKind, MeasureSpec, MeasureState, Pin, Reason, Value};
use crate::views::tests::room_6x4;
use crate::{DimRef, Form, MassSpec, Model, ViewKey};

/// Time of the workload in `machine_scale`, debug build, on the machine where the budgets
/// below were set, ms (measured with the other tests running, 134 to 164).
const REF_DEBUG_MS: f64 = 150.0;

/// How much slower this machine is than the one the budgets were set on, never below 1: the
/// same SQLite workload the measurements lean on, timed here (best of three).
fn machine_scale() -> f64 {
    let run = || {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t(a INTEGER PRIMARY KEY, b REAL, c INTEGER); CREATE INDEX tc ON t(c);").unwrap();
        let t = std::time::Instant::now();
        c.execute_batch("BEGIN").unwrap();
        {
            let mut st = c.prepare("INSERT INTO t(b, c) VALUES (?1, ?2)").unwrap();
            for i in 0..20_000i64 {
                st.execute(rusqlite::params![i as f64 * 0.5, i % 97]).unwrap();
            }
        }
        c.execute_batch("COMMIT").unwrap();
        let mut q = c.prepare("SELECT sum(b) FROM t WHERE c = ?1").unwrap();
        for k in 0..97i64 {
            let _: f64 = q.query_row([k], |r| r.get(0)).unwrap();
        }
        t.elapsed().as_secs_f64() * 1e3
    };
    if !cfg!(debug_assertions) {
        // release budgets have no reference measured yet: they stay as set
        return 1.0;
    }
    let ms = (0..3).map(|_| run()).fold(f64::INFINITY, f64::min);
    eprintln!("machine baseline {ms:.1} ms, reference {REF_DEBUG_MS:.1} ms");
    (ms / REF_DEBUG_MS).max(1.0)
}

fn walls(m: &Model, lv: i64) -> Vec<i64> {
    let mut st = m
        .sql()
        .prepare("SELECT id FROM wall WHERE level=?1 ORDER BY id")
        .unwrap();
    st.query_map([lv], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn face(wall: i64, face: u8) -> Anchor {
    Anchor::Dim(DimRef::WallEnd { wall, face, end: 0 })
}

fn spec(kind: MeasureKind, lv: i64, anchors: &[Anchor], m: &Model) -> MeasureSpec {
    MeasureSpec {
        kind,
        view: ViewKey::Plan(lv),
        pins: m.measure_pins(anchors).unwrap(),
        label: String::new(),
    }
}

fn ok(v: Value) -> f64 {
    match v {
        Value::Ok(x) => x,
        other => panic!("expected a number, got {other:?}"),
    }
}

/// South and north walls of the 6x4 room: inner faces 3.6 apart (200 mm walls).
fn gap(m: &mut Model, lv: i64) -> (i64, Vec<i64>) {
    let w = walls(m, lv);
    let s = spec(MeasureKind::Clear, lv, &[face(w[0], 1), face(w[2], 1)], m);
    (m.add_measure(&s).unwrap(), w)
}

#[test]
fn measure_follows_wall_move() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let (id, w) = gap(&mut m, lv);
    assert!((ok(m.measure_value(id).unwrap()) - 3.6).abs() < 1e-9);
    m.set_wall_axis(w[2], (6.0, 5.0), (0.0, 5.0)).unwrap();
    assert!((ok(m.measure_value(id).unwrap()) - 4.6).abs() < 1e-9);
    m.undo().unwrap();
    assert!((ok(m.measure_value(id).unwrap()) - 3.6).abs() < 1e-9);
}

/// A face pin is a plane: `clear` is the gap along the normal, not the end-point distance.
#[test]
fn clear_between_face_pins_is_the_plane_gap_not_the_end_distance() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let w = walls(&m, lv);
    // south and north walls run in opposite directions, so their start points are 6 m apart in x
    let clear = m.add_measure(&spec(MeasureKind::Clear, lv, &[face(w[0], 1), face(w[2], 1)], &m)).unwrap();
    let dist = m.add_measure(&spec(MeasureKind::Distance, lv, &[face(w[0], 1), face(w[2], 1)], &m)).unwrap();
    let gap = ok(m.measure_value(clear).unwrap());
    assert!((gap - 3.6).abs() < 1e-9, "{gap}");
    let end = ok(m.measure_value(dist).unwrap());
    assert!((end - (6.0f64 * 6.0 + 3.6 * 3.6).sqrt()).abs() < 1e-9, "{end}");
    // the two faces of one wall are a thickness apart
    let t = m.add_measure(&spec(MeasureKind::Clear, lv, &[face(w[0], 0), face(w[0], 1)], &m)).unwrap();
    assert!((ok(m.measure_value(t).unwrap()) - 0.2).abs() < 1e-9);
}

#[test]
fn measure_follows_wall_thickness() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let (id, _) = gap(&mut m, lv);
    let wt: i64 = m
        .sql()
        .query_row("SELECT id FROM wall_type", [], |r| r.get(0))
        .unwrap();
    m.set_layer_thickness(wt, 0, 0.3).unwrap();
    assert!((ok(m.measure_value(id).unwrap()) - 3.4).abs() < 1e-9);
    m.undo().unwrap();
    assert!((ok(m.measure_value(id).unwrap()) - 3.6).abs() < 1e-9);
}

#[test]
fn measure_orphan_on_delete_and_back_on_undo() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let (id, w) = gap(&mut m, lv);
    m.delete_wall(w[2]).unwrap();
    let i = m.measure_info(id).unwrap();
    assert_eq!(i.state, MeasureState::Orphan);
    assert_eq!(i.value, Value::Unavailable(Reason::Orphan(1)));
    assert!((i.last.unwrap() - 3.6).abs() < 1e-9, "last value is kept");
    // shown at the remembered point of the lost face
    assert!((i.points[1][1] - 3.8).abs() < 1e-9, "{:?}", i.points);
    m.undo().unwrap();
    let i = m.measure_info(id).unwrap();
    assert_eq!(i.state, MeasureState::Live);
    assert!((ok(i.value) - 3.6).abs() < 1e-9);
    // delete again, undo the deletion's neighbour (the measurement itself): memory is not needed then
    m.delete_wall(w[2]).unwrap();
    assert_eq!(m.measure_info(id).unwrap().state, MeasureState::Orphan);
    assert!((m.measure_info(id).unwrap().points[1][1] - 3.8).abs() < 1e-9);
}

fn tower(lv: i64) -> MassSpec {
    MassSpec {
        level: lv,
        name: "block".into(),
        void: false,
        inplace: false,
        form: Form::Extrude {
            ring: vec![(0., 0.), (10., 0.), (10., 6.), (0., 6.)],
            z0: 0.0,
            h: 9.0,
            ids: vec![],
        },
    }
}

#[test]
fn measure_mass_face_tag_survives_vertex_insert() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L0", 0.0).unwrap();
    let id = m.add_mass(&tower(lv)).unwrap();
    let (south, east, west) = (ftag(1, 2), ftag(1, 3), ftag(1, 5));
    let a = |m: &Model, t| m.mass_face_anchor(id, t).unwrap();
    let across = spec(
        MeasureKind::Clear,
        lv,
        &[a(&m, west), a(&m, east)],
        &m,
    );
    let across = m.add_measure(&across).unwrap();
    let on_south = spec(MeasureKind::Area, lv, &[a(&m, south)], &m);
    let on_south = m.add_measure(&on_south).unwrap();
    assert!((ok(m.measure_value(across).unwrap()) - 10.0).abs() < 1e-9);
    assert!((ok(m.measure_value(on_south).unwrap()) - 10.0 * 9.0).abs() < 1e-9);
    // a vertex splits the south face; the others keep their tags
    let mut sp = m.mass(id).unwrap();
    sp.form = Form::Extrude {
        ring: vec![(0., 0.), (5., 0.), (10., 0.), (10., 6.), (0., 6.)],
        z0: 0.0,
        h: 9.0,
        ids: vec![],
    };
    m.update_mass(id, &sp).unwrap();
    assert!((ok(m.measure_value(across).unwrap()) - 10.0).abs() < 1e-9);
    // the split face is not replaced by one of its halves
    let i = m.measure_info(on_south).unwrap();
    assert_eq!(i.value, Value::Unavailable(Reason::Orphan(1)), "{i:?}");
    // moving the east vertices moves the gap
    let mut sp = m.mass(id).unwrap();
    if let Form::Extrude { ring, .. } = &mut sp.form {
        ring[2] = (12., 0.);
        ring[3] = (12., 6.);
    }
    m.update_mass(id, &sp).unwrap();
    assert!((ok(m.measure_value(across).unwrap()) - 12.0).abs() < 1e-9);
}

#[test]
fn measure_area_room_matches_rooms() {
    // the fixture already places a room at (3, 2); a second room there is a zero-count duplicate
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let r: i64 = m
        .sql()
        .query_row("SELECT id FROM room WHERE level=?1", [lv], |r| r.get(0))
        .unwrap();
    let q = m.room_quantities(r).unwrap().unwrap();
    let mk = |m: &mut Model, k| {
        let s = spec(k, lv, &[Anchor::Room { id: r }], m);
        m.add_measure(&s).unwrap()
    };
    let (a, p, v) = (
        mk(&mut m, MeasureKind::Area),
        mk(&mut m, MeasureKind::Perimeter),
        mk(&mut m, MeasureKind::Volume),
    );
    assert!((ok(m.measure_value(a).unwrap()) - q.area).abs() < 1e-9);
    assert!((ok(m.measure_value(p).unwrap()) - q.perimeter).abs() < 1e-9);
    assert!((ok(m.measure_value(v).unwrap()) - q.volume).abs() < 1e-9);
    assert!((q.area - 3.6 * 5.6).abs() < 1e-6, "{}", q.area);
    // the room follows its walls, and so does the measurement
    let wt: i64 = m
        .sql()
        .query_row("SELECT id FROM wall_type", [], |r| r.get(0))
        .unwrap();
    m.set_layer_thickness(wt, 0, 0.3).unwrap();
    let q2 = m.room_quantities(r).unwrap().unwrap();
    assert!(q2.area < q.area - 0.1);
    assert!((ok(m.measure_value(a).unwrap()) - q2.area).abs() < 1e-9);
    assert!((ok(m.measure_value(p).unwrap()) - q2.perimeter).abs() < 1e-9);
}

#[test]
fn measure_unavailable_reason() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let w = walls(&m, lv);
    // faces of perpendicular walls have no clear gap
    let s = spec(MeasureKind::Clear, lv, &[face(w[0], 1), face(w[1], 1)], &m);
    let id = m.add_measure(&s).unwrap();
    assert_eq!(
        m.measure_value(id).unwrap(),
        Value::Unavailable(Reason::NotPlanar)
    );
    assert_eq!(m.measure_info(id).unwrap().state, MeasureState::Unavailable);
    // the volume of a point, the clear gap of points: a reason, never zero
    let pt = |x| Anchor::Point([x, 0.0, 0.0]);
    let s = spec(MeasureKind::Volume, lv, &[pt(1.0)], &m);
    assert_eq!(
        m.eval_measure_spec(&s),
        Value::Unavailable(Reason::Unsupported("volume_of"))
    );
    let s = spec(MeasureKind::Clear, lv, &[pt(1.0), pt(2.0)], &m);
    assert_eq!(
        m.eval_measure_spec(&s),
        Value::Unavailable(Reason::Unsupported("clear_needs_faces"))
    );
    // a vertical "slope" has no run
    let s = spec(
        MeasureKind::Slope,
        lv,
        &[Anchor::Point([0.0, 0.0, 0.0]), Anchor::Point([0.0, 0.0, 2.0])],
        &m,
    );
    assert_eq!(
        m.eval_measure_spec(&s),
        Value::Unavailable(Reason::Unsupported("slope_run"))
    );
    // wrong pin count is refused, a missing element too
    let mut bad = spec(MeasureKind::Distance, lv, &[pt(0.0), pt(1.0)], &m);
    bad.pins.pop();
    assert!(m.add_measure(&bad).is_err());
    let gone = MeasureSpec {
        kind: MeasureKind::Distance,
        view: ViewKey::Plan(lv),
        pins: vec![
            Pin {
                anchor: face(9999, 0),
                fallback: [0.0; 3],
            },
            Pin {
                anchor: pt(1.0),
                fallback: [1.0, 0.0, 0.0],
            },
        ],
        label: String::new(),
    };
    assert!(m.add_measure(&gone).is_err());
}

#[test]
fn measure_basic_kinds() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let w = walls(&m, lv);
    let s = spec(
        MeasureKind::Distance,
        lv,
        &[Anchor::Dim(DimRef::AxisEnd { wall: w[0], end: 0 }), Anchor::Dim(DimRef::AxisEnd { wall: w[0], end: 1 })],
        &m,
    );
    assert!((ok(m.eval_measure_spec(&s)) - 6.0).abs() < 1e-9);
    let s = spec(
        MeasureKind::Angle,
        lv,
        &[Anchor::Dim(DimRef::AxisLine { wall: w[0] }), Anchor::Dim(DimRef::AxisLine { wall: w[1] })],
        &m,
    );
    assert!((ok(m.eval_measure_spec(&s)) - 90.0).abs() < 1e-9);
    let s = spec(
        MeasureKind::Slope,
        lv,
        &[Anchor::Point([0.0, 0.0, 0.0]), Anchor::Point([4.0, 0.0, 0.5])],
        &m,
    );
    assert!((ok(m.eval_measure_spec(&s)) - 12.5).abs() < 1e-9);
    let sq: Vec<Anchor> = [[0., 0., 0.], [3., 0., 0.], [3., 2., 0.], [0., 2., 0.]]
        .into_iter()
        .map(Anchor::Point)
        .collect();
    let s = spec(MeasureKind::Area, lv, &sq, &m);
    assert!((ok(m.eval_measure_spec(&s)) - 6.0).abs() < 1e-9);
    let s = spec(MeasureKind::Perimeter, lv, &sq, &m);
    assert!((ok(m.eval_measure_spec(&s)) - 10.0).abs() < 1e-9);
    // one journal step per edit
    let id = m.add_measure(&s).unwrap();
    assert_eq!(m.measures_in_view(ViewKey::Plan(lv)).unwrap().len(), 1);
    m.delete_measure(id).unwrap();
    assert!(m.measure_info(id).is_err());
    m.undo().unwrap();
    assert_eq!(m.measures_in_view(ViewKey::Plan(lv)).unwrap().len(), 1);
    assert!(m.measures_in_view(ViewKey::ThreeD).unwrap().is_empty());
}

#[test]
fn measure_file_roundtrip_and_old_file_opens() {
    let p = std::env::temp_dir()
        .join(format!("ddc_measure_mig_{}.ddc", std::process::id()))
        .to_str()
        .unwrap()
        .to_string();
    let _ = std::fs::remove_file(&p);
    let id;
    {
        let mut m = Model::open(&p).unwrap();
        let lv2 = m.add_level("L1", 0.0).unwrap();
        let s = MeasureSpec {
            kind: MeasureKind::Distance,
            view: ViewKey::Plan(lv2),
            pins: vec![
                Pin { anchor: Anchor::Point([0.0, 0.0, 0.0]), fallback: [0.0; 3] },
                Pin { anchor: Anchor::Point([3.0, 4.0, 0.0]), fallback: [3.0, 4.0, 0.0] },
            ],
            label: "diag".into(),
        };
        id = m.add_measure(&s).unwrap();
    }
    {
        // reopening twice changes nothing (idempotent step)
        let m = Model::open(&p).unwrap();
        assert!((ok(m.measure_value(id).unwrap()) - 5.0).abs() < 1e-9);
        assert_eq!(m.schema_version().unwrap(), Model::open(":memory:").unwrap().schema_version().unwrap());
    }
    {
        let m = Model::open(&p).unwrap();
        assert!((ok(m.measure_value(id).unwrap()) - 5.0).abs() < 1e-9);
        // a file from before the step: no tables
        m.sql()
            .execute_batch(
                "DROP TABLE measure; DROP TABLE measure_geo; DROP TABLE measure_ref; \
                 UPDATE meta SET value=54 WHERE key='schema_version';",
            )
            .unwrap();
    }
    let mut m = Model::open(&p).unwrap();
    assert_eq!(m.schema_version().unwrap(), Model::open(":memory:").unwrap().schema_version().unwrap());
    let lv = m.add_level("L2", 3.0).unwrap();
    assert!(m.measures_in_view(ViewKey::Plan(lv)).unwrap().is_empty());
    drop(m);
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{p}{s}"));
    }
}

#[test]
fn measure_refresh_budget_many_measures() {
    // 200 walls, ~10 000 measurements on one level: editing one wall refreshes only its own
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let wt = m
        .add_wall_type("W", &[crate::LayerSpec { material: "B".into(), thickness: 0.2 }])
        .unwrap();
    let specs: Vec<crate::WallSpec> = (0..200)
        .map(|k| crate::WallSpec {
            a: (0.0, k as f64 * 3.0),
            b: (5.0, k as f64 * 3.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: Some(0.0),
        })
        .collect();
    let ids = m.add_walls(&specs).unwrap();
    let mut batch = vec![];
    for w in ids.windows(2) {
        let s = spec(MeasureKind::Clear, lv, &[face(w[0], 1), face(w[1], 1)], &m);
        batch.extend(std::iter::repeat(s).take(50));
    }
    assert_eq!(m.add_measures(&batch).unwrap().len(), 199 * 50);
    // one journal step for the whole batch
    m.undo().unwrap();
    assert!(m.measures_in_view(ViewKey::Plan(lv)).unwrap().is_empty());
    let n_before = m.add_measures(&batch).unwrap().len();
    assert_eq!(n_before, 199 * 50);
    let all = m.measures_in_view(ViewKey::Plan(lv)).unwrap();
    assert_eq!(all.len(), 199 * 50);
    assert!(all.iter().all(|i| (ok(i.value) - 3.0).abs() < 1e-9));
    let t = std::time::Instant::now();
    m.set_wall_axis(ids[100], (0.0, 301.0), (5.0, 301.0)).unwrap();
    let dt = t.elapsed();
    let touched = m
        .measures_in_view(ViewKey::Plan(lv))
        .unwrap()
        .iter()
        .filter(|i| (ok(i.value) - 3.0).abs() > 1e-6)
        .count();
    assert_eq!(touched, 100, "the moved wall belongs to two neighbours' 50 each");
    // budgets are for release; a debug build gets ten times as much, a slower machine its
    // measured share more
    let k = (if cfg!(debug_assertions) { 10.0 } else { 1.0 } * machine_scale()).ceil() as u128;
    assert!(dt.as_millis() < 50 * k, "edit with refresh took {dt:?}");
    // the refresh alone: the moved wall's measurements, nothing else
    let one: std::collections::BTreeSet<i64> = [ids[100]].into();
    let none = std::collections::BTreeSet::new();
    let touch = crate::measure::Touch {
        walls: &one,
        slabs: &none,
        rooms: &none,
        rooms_any: false,
        measures: &none,
    };
    let t = std::time::Instant::now();
    crate::measure::refresh(m.sql(), &touch).unwrap();
    let dt = t.elapsed();
    assert!(dt.as_millis() < 5 * k, "refresh of one wall took {dt:?}");
    // a full recompute of ~10 000 measurements
    let t = std::time::Instant::now();
    crate::measure::regenerate(m.sql()).unwrap();
    let dt = t.elapsed();
    assert!(dt.as_millis() < 200 * k, "full recompute took {dt:?}");
}

fn axis_y(m: &Model, w: i64) -> f64 {
    m.sql().query_row("SELECT ay FROM wall WHERE id=?1", [w], |r| r.get(0)).unwrap()
}

#[test]
fn drive_clear_moves_far_wall() {
    use crate::MeasureSide;
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let (id, w) = gap(&mut m, lv);
    let steps = m.journal_len().unwrap();
    m.drive_measure(id, 4.6, MeasureSide::B).unwrap();
    assert!((ok(m.measure_value(id).unwrap()) - 4.6).abs() < 1e-9);
    assert!((axis_y(&m, w[2]) - 5.0).abs() < 1e-6, "far wall moved");
    assert!((axis_y(&m, w[0]) - 0.0).abs() < 1e-9, "near wall stays");
    assert_eq!(m.journal_len().unwrap(), steps + 1, "one journal record");
    m.undo().unwrap();
    assert!((ok(m.measure_value(id).unwrap()) - 3.6).abs() < 1e-9);
    assert!((axis_y(&m, w[2]) - 4.0).abs() < 1e-9);
    // the near wall moves when A is asked to
    m.drive_measure(id, 2.6, MeasureSide::A).unwrap();
    assert!((axis_y(&m, w[0]) - 1.0).abs() < 1e-9);
    assert!(m.drive_measure(id, -1.0, MeasureSide::B).is_err());
}

#[test]
fn drive_conflict_rolls_back() {
    use crate::{ConstraintKind, ConstraintSpec};
    use crate::MeasureSide;
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let (id, w) = gap(&mut m, lv);
    m.add_constraint(&ConstraintSpec { kind: ConstraintKind::LockDist, walls: vec![w[0], w[2]], value: None })
        .unwrap();
    let before = axis_y(&m, w[2]);
    let r = m.drive_measure(id, 4.6, MeasureSide::B);
    match &r {
        Err(crate::Error::Conflict(t)) => {
            assert!(t.contains("lock_dist") && t.contains("driving measurement"), "{t}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(axis_y(&m, w[2]), before, "model untouched");
    assert!((ok(m.measure_value(id).unwrap()) - 3.6).abs() < 1e-9);
}

#[test]
fn drive_level_gap_moves_upper_levels() {
    use crate::{ConstraintKind, ConstraintSpec};
    use crate::MeasureSide;
    let mut m = Model::open(":memory:").unwrap();
    // level names are UNIQUE in the schema
    let l: Vec<i64> = [0.0, 3.0, 6.0].iter().map(|z| m.add_level(&format!("L{z}"), *z).unwrap()).collect();
    m.add_constraint(&ConstraintSpec { kind: ConstraintKind::LevelGap, walls: vec![l[1], l[2]], value: None })
        .unwrap();
    let anchors = [Anchor::Level { id: l[0] }, Anchor::Level { id: l[1] }];
    let s = MeasureSpec {
        kind: MeasureKind::Distance,
        view: ViewKey::ThreeD,
        pins: m.measure_pins(&anchors).unwrap(),
        label: String::new(),
    };
    let id = m.add_measure(&s).unwrap();
    m.drive_measure(id, 3.5, MeasureSide::B).unwrap();
    let z = |m: &Model, l: i64| -> f64 {
        m.sql().query_row("SELECT elevation FROM level WHERE id=?1", [l], |r| r.get(0)).unwrap()
    };
    assert!((z(&m, l[1]) - 3.5).abs() < 1e-9);
    assert!((z(&m, l[2]) - 6.5).abs() < 1e-9, "the level above keeps its gap");
    assert!((ok(m.measure_value(id).unwrap()) - 3.5).abs() < 1e-9);
    m.undo().unwrap();
    assert!((z(&m, l[2]) - 6.0).abs() < 1e-9);
}

fn three_walls(m: &mut Model, lv: i64, n: usize, step: f64) -> Vec<i64> {
    let wt = m
        .add_wall_type("W3", &[crate::LayerSpec { material: "B".into(), thickness: 0.2 }])
        .unwrap();
    let specs: Vec<crate::WallSpec> = (0..n)
        .map(|k| crate::WallSpec {
            a: (0.0, k as f64 * step),
            b: (5.0, k as f64 * step),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: Some(0.0),
        })
        .collect();
    m.add_walls(&specs).unwrap()
}

#[test]
fn drive_keeps_eq_and_parallel() {
    use crate::{ConstraintKind, ConstraintSpec, MeasureSide};
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let w = three_walls(&mut m, lv, 3, 4.0);
    m.add_constraint(&ConstraintSpec { kind: ConstraintKind::Equal, walls: w.clone(), value: None }).unwrap();
    m.add_constraint(&ConstraintSpec { kind: ConstraintKind::Parallel, walls: vec![w[0], w[2]], value: None })
        .unwrap();
    let s = spec(MeasureKind::Clear, lv, &[face(w[1], 1), face(w[2], 1)], &m);
    let id = m.add_measure(&s).unwrap();
    assert!((ok(m.measure_value(id).unwrap()) - 4.0).abs() < 1e-9);
    m.drive_measure(id, 5.0, MeasureSide::B).unwrap();
    let y: Vec<f64> = w.iter().map(|&k| axis_y(&m, k)).collect();
    assert!((y[1] - 4.0).abs() < 1e-6, "the fixed support stays: {y:?}");
    assert!((y[2] - y[1] - 5.0).abs() < 1e-6, "{y:?}");
    assert!(((y[1] - y[0]) - (y[2] - y[1])).abs() < 1e-6, "equal steps kept: {y:?}");
    for &k in &w {
        let by: f64 = m.sql().query_row("SELECT by FROM wall WHERE id=?1", [k], |r| r.get(0)).unwrap();
        assert!((by - axis_y(&m, k)).abs() < 1e-6, "still parallel to the x axis");
    }
    // the number is held: moving the fixed wall carries the far one along
    m.set_wall_axis(w[1], (0.0, 5.0), (5.0, 5.0)).unwrap();
    assert!((ok(m.measure_value(id).unwrap()) - 5.0).abs() < 1e-6);
}

#[test]
fn drive_mass_face_push_pull() {
    use crate::MeasureSide;
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L0", 0.0).unwrap();
    let mass = m.add_mass(&tower(lv)).unwrap();
    let a = |m: &Model, t| m.mass_face_anchor(mass, t).unwrap();
    let s = spec(MeasureKind::Clear, lv, &[a(&m, ftag(1, 5)), a(&m, ftag(1, 3))], &m);
    let id = m.add_measure(&s).unwrap();
    assert!((ok(m.measure_value(id).unwrap()) - 10.0).abs() < 1e-9);
    m.drive_measure(id, 12.0, MeasureSide::B).unwrap();
    assert!((ok(m.measure_value(id).unwrap()) - 12.0).abs() < 1e-6);
    m.undo().unwrap();
    assert!((ok(m.measure_value(id).unwrap()) - 10.0).abs() < 1e-9);
}

fn twins(m: &Model) -> i64 {
    m.sql()
        .query_row("SELECT COUNT(*) FROM constraint_rule WHERE kind='measure'", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn driving_delete_removes_rule_undo() {
    use crate::MeasureSide;
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let (id, _) = gap(&mut m, lv);
    assert!(!m.measure_info(id).unwrap().driving);
    m.drive_measure(id, 4.0, MeasureSide::B).unwrap();
    assert_eq!((twins(&m), m.measure_info(id).unwrap().driving), (1, true));
    m.delete_measure(id).unwrap();
    assert_eq!(twins(&m), 0, "the twin goes with the measurement");
    m.undo().unwrap();
    assert_eq!((twins(&m), m.measure_info(id).unwrap().driving), (1, true));
    m.set_measure_driving(id, false).unwrap();
    assert_eq!(twins(&m), 0);
    m.undo().unwrap();
    assert_eq!(twins(&m), 1);
}

#[test]
fn drive_budget_200_linked_walls() {
    use crate::MeasureSide;
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let w = three_walls(&mut m, lv, 200, 3.0);
    let mut ids = vec![];
    for p in w.windows(2) {
        let s = spec(MeasureKind::Clear, lv, &[face(p[0], 1), face(p[1], 1)], &m);
        ids.push(m.add_measure(&s).unwrap());
    }
    for &id in &ids {
        m.set_measure_driving(id, true).unwrap();
    }
    let t = std::time::Instant::now();
    m.drive_measure(ids[100], 3.5, MeasureSide::B).unwrap();
    let dt = t.elapsed();
    // walls stand 3 m apart (y = 3k); the far support w[101] moves by 3.5 - 3 off the fixed
    // w[100] at y = 300, and the 98 locked gaps beyond carry w[199] along by the same 0.5
    assert!((axis_y(&m, w[100]) - 300.0).abs() < 1e-9, "the fixed support stays");
    assert!((axis_y(&m, w[101]) - 303.5).abs() < 1e-5);
    assert!((axis_y(&m, w[199]) - 597.5).abs() < 1e-5, "the walls beyond keep their gaps");
    let k = (if cfg!(debug_assertions) { 20.0 } else { 1.0 } * machine_scale()).ceil() as u128;
    assert!(dt.as_millis() < 30 * k, "drive of 200 linked walls took {dt:?}");
}

#[test]
fn drive_refusals_are_typed_and_leave_the_model() {
    use crate::MeasureSide;
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let w = walls(&m, lv);
    let before = axis_y(&m, w[2]);
    // a distance between wall points is not a gap
    let s = spec(MeasureKind::Distance, lv, &[face(w[0], 1), face(w[2], 1)], &m);
    let d = m.add_measure(&s).unwrap();
    let e = m.drive_measure(d, 5.0, MeasureSide::B).unwrap_err().to_string();
    assert!(e.contains("unsupported: wall_points_need_clear"), "{e}");
    // an axis anchor
    let ax = Anchor::Dim(DimRef::AxisLine { wall: w[0] });
    let s = spec(MeasureKind::Distance, lv, &[ax, face(w[2], 1)], &m);
    let a = m.add_measure(&s).unwrap();
    let e = m.drive_measure(a, 5.0, MeasureSide::B).unwrap_err().to_string();
    assert!(e.contains("unsupported"), "{e}");
    assert_eq!(axis_y(&m, w[2]), before);
}

#[test]
fn drive_mass_faces_with_zero_gap_push_outward() {
    use crate::MeasureSide;
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L0", 0.0).unwrap();
    let mass = m.add_mass(&tower(lv)).unwrap();
    let a = |m: &Model, t| m.mass_face_anchor(mass, t).unwrap();
    // the same face twice: gap 0
    let s = spec(MeasureKind::Distance, lv, &[a(&m, ftag(1, 3)), a(&m, ftag(1, 3))], &m);
    let id = m.add_measure(&s).unwrap();
    assert!(ok(m.measure_value(id).unwrap()).abs() < 1e-9);
    m.drive_measure(id, 2.0, MeasureSide::B).unwrap();
}

#[test]
fn driving_survives_reopen_and_migration_is_idempotent() {
    use crate::MeasureSide;
    let p = std::env::temp_dir().join(format!("ddc_drv_{}.ddc", std::process::id()));
    let p = p.to_string_lossy().into_owned();
    let id;
    {
        let mut m = Model::open(&p).unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let w = three_walls(&mut m, lv, 2, 4.0);
        let s = spec(MeasureKind::Clear, lv, &[face(w[0], 1), face(w[1], 1)], &m);
        id = m.add_measure(&s).unwrap();
        m.drive_measure(id, 5.0, MeasureSide::B).unwrap();
    }
    for _ in 0..2 {
        let m = Model::open(&p).unwrap();
        let i = m.measure_info(id).unwrap();
        assert!(i.driving);
        assert!((ok(i.value) - 5.0).abs() < 1e-6);
        assert_eq!(m.schema_version().unwrap(), Model::open(":memory:").unwrap().schema_version().unwrap());
    }
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{p}{s}"));
    }
}

#[test]
fn axis_distance_is_between_lines_and_drives() {
    use crate::MeasureSide;
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let w = walls(&m, lv);
    let ax = |i: usize| Anchor::Dim(DimRef::AxisLine { wall: w[i] });
    // the south and north walls are 6 m long, 4 m apart: the lines, not the start points
    let s = spec(MeasureKind::Distance, lv, &[ax(0), ax(2)], &m);
    assert!((ok(m.eval_measure_spec(&s)) - 4.0).abs() < 1e-9);
    // perpendicular axes have no distance
    let s2 = spec(MeasureKind::Distance, lv, &[ax(0), ax(1)], &m);
    assert!(matches!(m.eval_measure_spec(&s2), Value::Unavailable(Reason::Unsupported("axes_not_parallel"))));
    let id = m.add_measure(&s).unwrap();
    let steps = m.journal_len().unwrap();
    m.drive_measure(id, 5.0, MeasureSide::B).unwrap();
    assert!((ok(m.measure_value(id).unwrap()) - 5.0).abs() < 1e-6);
    assert!((axis_y(&m, w[2]) - 5.0).abs() < 1e-6, "far wall moved");
    assert!((axis_y(&m, w[0]) - 0.0).abs() < 1e-9, "near wall stays");
    assert_eq!(m.journal_len().unwrap(), steps + 1);
    m.undo().unwrap();
    assert!((axis_y(&m, w[2]) - 4.0).abs() < 1e-9);
}

#[test]
fn old_axis_distance_loads_and_shows_the_line_value() {
    let p = std::env::temp_dir()
        .join(format!("ddc_axis_mig_{}.ddc", std::process::id()))
        .to_str()
        .unwrap()
        .to_string();
    let _ = std::fs::remove_file(&p);
    let (id, old);
    {
        let mut m = Model::open(&p).unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let wt = m
            .add_wall_type("W", &[crate::LayerSpec { material: "B".into(), thickness: 0.2 }])
            .unwrap();
        let mk = |a: (f64, f64), b: (f64, f64)| crate::WallSpec {
            a,
            b,
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: Some(0.0),
        };
        let w = m.add_walls(&[mk((0.0, 0.0), (6.0, 0.0)), mk((2.0, 4.0), (8.0, 4.0))]).unwrap();
        let ax = |i: usize| Anchor::Dim(DimRef::AxisLine { wall: w[i] });
        let s = spec(MeasureKind::Distance, lv, &[ax(0), ax(1)], &m);
        id = m.add_measure(&s).unwrap();
        assert!((ok(m.measure_value(id).unwrap()) - 4.0).abs() < 1e-9, "lines are 4 apart");
        // what the previous version stored: the distance between the start points
        old = (2.0f64 * 2.0 + 4.0 * 4.0).sqrt();
        m.sql().execute("UPDATE measure_geo SET value=?2 WHERE measure=?1", rusqlite::params![id, old]).unwrap();
        let stored: f64 = m.sql().query_row("SELECT value FROM measure_geo WHERE measure=?1", [id], |r| r.get(0)).unwrap();
        assert!((stored - old).abs() < 1e-12, "the old number is what is stored");
    }
    for _ in 0..2 {
        let m = Model::open(&p).unwrap();
        let i = m.measure_info(id).unwrap();
        assert!((ok(i.value) - 4.0).abs() < 1e-9, "the reopened file shows the line value");
        assert_eq!(i.state, MeasureState::Live);
        assert_eq!(i.points.len(), 2);
        // reading wrote nothing: the file still holds the old number
        let stored: f64 = m.sql().query_row("SELECT value FROM measure_geo WHERE measure=?1", [id], |r| r.get(0)).unwrap();
        assert!((stored - old).abs() < 1e-12, "open and read leave the old row alone: {stored}");
    }
    // the next regeneration (any save) replaces it
    {
        let m = Model::open(&p).unwrap();
        crate::measure::regenerate(m.sql()).unwrap();
        let stored: f64 = m.sql().query_row("SELECT value FROM measure_geo WHERE measure=?1", [id], |r| r.get(0)).unwrap();
        assert!((stored - 4.0).abs() < 1e-9, "{stored}");
    }
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{p}{s}"));
    }
}
