use super::*;
use crate::views::tests::room_6x4;
use crate::{LayerSpec, OpeningSpec, PlanViewSpec, ViewPrim, WallSpec};

fn values(m: &Model, lv: i64) -> Vec<(i32, f64)> {
    let mut v: Vec<(i32, f64)> = m
        .dims(lv)
        .unwrap()
        .iter()
        .map(|a| (a.spec.tier, (a.geom.value() * 1e9).round() / 1e9))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    v
}

fn wall_ids(m: &Model, lv: i64) -> Vec<i64> {
    let mut st = m
        .sql()
        .prepare("SELECT id FROM wall WHERE level=?1 ORDER BY id")
        .unwrap();
    st.query_map([lv], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn opening_ids(m: &Model) -> Vec<i64> {
    let mut st = m
        .sql()
        .prepare("SELECT id FROM opening ORDER BY id")
        .unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn spec(lv: i64, kind: DimKind, refs: Vec<DimRef>) -> DimSpec {
    DimSpec {
        level: lv,
        kind,
        refs,
        dir: (0.0, 0.0),
        side: 1.0,
        offset: 0.0,
        tier: -1,
        auto: false,
    }
}

#[test]
fn auto_rect_chains_exact_and_replace_undo() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let ids = m.auto_dimension(lv).unwrap();
    assert_eq!(ids.len(), 10);
    // tier 0: door 1.0|0.9|4.1 (south), window 2.5|1.5|2.0 (north), west/east overall 4 (no openings);
    // tier 1: south/north overall 6
    let want = vec![
        (0, 0.9),
        (0, 1.0),
        (0, 1.5),
        (0, 2.0),
        (0, 2.5),
        (0, 4.0),
        (0, 4.0),
        (0, 4.1),
        (1, 6.0),
        (1, 6.0),
    ];
    assert_eq!(values(&m, lv), want);
    // all south/north dims sit on the exterior face line (y = 0 / y = 4)
    for a in m.dims(lv).unwrap() {
        let DimGeom::Linear(l) = a.geom else { panic!() };
        let on = [l.base.y, l.base.x]
            .iter()
            .any(|&c| [0.0, 4.0, 6.0].contains(&c));
        assert!(on, "{l:?}");
        assert!(l.n.x.abs() + l.n.y.abs() == 1.0);
    }
    let v = m
        .plan_view(&PlanViewSpec {
            level: lv,
            cut_height: 1.2,
            grids: vec![],
        })
        .unwrap();
    assert_eq!(v.count(|p| matches!(p, ViewPrim::Annot { .. })), 10);
    assert_eq!(v.count(|p| matches!(p, ViewPrim::Dim { .. })), 0);
    // second run replaces, undo restores the first set
    let again = m.auto_dimension(lv).unwrap();
    assert_eq!(again.len(), 10);
    assert_eq!(m.dims(lv).unwrap().len(), 10);
    m.undo().unwrap();
    let first: Vec<i64> = m.dims(lv).unwrap().iter().map(|a| a.id).collect();
    assert_eq!(first, ids);
    m.undo().unwrap();
    assert!(m.dims(lv).unwrap().is_empty());
}

#[test]
fn associative_update_orphan_and_undo() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    m.auto_dimension(lv).unwrap();
    let door = opening_ids(&m)[0];
    m.update_opening(
        door,
        &OpeningSpec {
            along: 2.0,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        },
    )
    .unwrap();
    let t0: Vec<f64> = values(&m, lv)
        .iter()
        .filter(|v| v.0 == 0)
        .map(|v| v.1)
        .collect();
    assert_eq!(t0, vec![0.9, 1.5, 2.0, 2.0, 2.5, 3.1, 4.0, 4.0]);
    m.delete_opening(door).unwrap();
    let orphans: Vec<f64> = m
        .dims(lv)
        .unwrap()
        .iter()
        .filter(|a| a.orphan)
        .map(|a| (a.geom.value() * 1e9).round() / 1e9)
        .collect();
    // last known geometry kept
    assert_eq!(orphans, vec![3.1, 0.9, 2.0]);
    m.undo().unwrap();
    assert!(m.dims(lv).unwrap().iter().all(|a| !a.orphan));
    // wall move: overall south dimension follows
    let w = wall_ids(&m, lv);
    let d = m
        .add_dim(&spec(
            lv,
            DimKind::Linear,
            vec![
                DimRef::WallEnd {
                    wall: w[0],
                    face: 0,
                    end: 0,
                },
                DimRef::WallEnd {
                    wall: w[0],
                    face: 0,
                    end: 1,
                },
            ],
        ))
        .unwrap();
    let val = |m: &Model| {
        m.dims(lv)
            .unwrap()
            .into_iter()
            .find(|a| a.id == d)
            .unwrap()
            .geom
            .value()
    };
    assert_eq!(val(&m), 6.0);
    m.set_wall_axis(w[0], (0.0, 0.0), (8.0, 0.0)).unwrap();
    assert_eq!(val(&m), 8.0);
    m.undo().unwrap();
    assert_eq!(val(&m), 6.0);
    m.delete_dim(d).unwrap();
    assert!(m.dims(lv).unwrap().iter().all(|a| a.id != d));
    m.undo().unwrap();
    assert_eq!(val(&m), 6.0);
}

#[test]
fn manual_kinds_exact() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let w = wall_ids(&m, lv);
    let wt: i64 = m
        .sql()
        .query_row("SELECT wtype FROM wall LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let diag = m
        .add_wall(&WallSpec {
            a: (10.0, 0.0),
            b: (13.0, 4.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: Some(0.0),
        })
        .unwrap();
    let lin = m
        .add_dim(&spec(
            lv,
            DimKind::Linear,
            vec![
                DimRef::AxisEnd { wall: diag, end: 0 },
                DimRef::AxisEnd { wall: diag, end: 1 },
            ],
        ))
        .unwrap();
    let mut horiz = spec(
        lv,
        DimKind::Linear,
        vec![
            DimRef::AxisEnd { wall: diag, end: 0 },
            DimRef::AxisEnd { wall: diag, end: 1 },
        ],
    );
    horiz.dir = (1.0, 0.0);
    let hz = m.add_dim(&horiz).unwrap();
    let ang = m
        .add_dim(&spec(
            lv,
            DimKind::Angular,
            vec![
                DimRef::AxisLine { wall: w[0] },
                DimRef::AxisLine { wall: diag },
            ],
        ))
        .unwrap();
    let spot = m
        .add_dim(&spec(
            lv,
            DimKind::Spot,
            vec![
                DimRef::WallLevel {
                    wall: w[0],
                    top: true,
                },
                DimRef::Point((3.0, -1.0)),
            ],
        ))
        .unwrap();
    let g = |m: &Model, id| {
        m.dims(lv)
            .unwrap()
            .into_iter()
            .find(|a| a.id == id)
            .unwrap()
            .geom
    };
    assert_eq!(g(&m, lin).value(), 5.0);
    assert_eq!(g(&m, hz).value(), 3.0);
    let DimGeom::Angular {
        center,
        a0,
        a1,
        value,
        ..
    } = g(&m, ang)
    else {
        panic!()
    };
    assert_eq!(center, (10.0, 0.0));
    assert_eq!((a0, a1), (0.0, 53.130102354));
    assert!((value - 53.130102354).abs() < 1e-12);
    assert_eq!(g(&m, spot).value(), 3.0);
    m.update_level(lv, "L1", 3.5).unwrap();
    assert_eq!(g(&m, spot).value(), 6.5);
    // references to a missing wall are refused at creation
    let bad = spec(
        lv,
        DimKind::Linear,
        vec![
            DimRef::AxisEnd { wall: 999, end: 0 },
            DimRef::Point((0.0, 0.0)),
        ],
    );
    assert!(m.add_dim(&bad).is_err());
    let bad = spec(lv, DimKind::Radial, vec![DimRef::AxisLine { wall: w[0] }]);
    assert!(m.add_dim(&bad).is_err());
}

/// Semicircle R = 5 (axis = exterior face) plus a straight chord wall; a 1.0 door at 7.0 along the arc.
#[test]
fn auto_arc_wall_lengths_and_radius() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let wt = m
        .add_wall_type(
            "W",
            &[LayerSpec {
                material: "Brick".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let ws = |a, b| WallSpec {
        a,
        b,
        level: lv,
        base_offset: 0.0,
        height: 3.0,
        wall_type: wt,
        offset0: Some(0.0),
    };
    let arc = m.add_arc_wall(&ws((5.0, 0.0), (-5.0, 0.0)), 1.0).unwrap();
    m.add_wall(&ws((-5.0, 0.0), (5.0, 0.0))).unwrap();
    m.add_opening(
        arc,
        &OpeningSpec {
            along: 7.0,
            width: 1.0,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        },
    )
    .unwrap();
    let ids = m.auto_dimension(lv).unwrap();
    assert_eq!(ids.len(), 6);
    let pi5 = (5.0 * std::f64::consts::PI * 1e9).round() / 1e9;
    let r = |x: f64| (x * 1e9).round() / 1e9;
    assert_eq!(
        values(&m, lv),
        vec![
            (-1, 5.0),
            (0, 1.0),
            (0, 7.0),
            (0, r(5.0 * std::f64::consts::PI - 8.0)),
            (0, 10.0),
            (1, pi5)
        ]
    );
    let rad = m
        .dims(lv)
        .unwrap()
        .into_iter()
        .find(|a| a.spec.kind == DimKind::Radial)
        .unwrap();
    let DimGeom::Radial {
        center,
        radius,
        angle,
    } = rad.geom
    else {
        panic!()
    };
    assert_eq!(
        (r(center.0), r(center.1), radius, r(angle)),
        (0.0, 0.0, 5.0, 90.0)
    );
    // bulge change: radius follows (half-sweep 60°, chord 10 → R = 5/sin 60°)
    m.set_wall_bulge(arc, (std::f64::consts::PI / 6.0).tan())
        .unwrap();
    let rad = m
        .dims(lv)
        .unwrap()
        .into_iter()
        .find(|a| a.spec.kind == DimKind::Radial)
        .unwrap();
    assert!((rad.geom.value() - 5.0 / (std::f64::consts::PI / 3.0).sin()).abs() < 1e-9);
}

#[test]
fn schema_11_file_gains_dim_tables() {
    let dir = std::env::temp_dir().join(format!("ddc_dim_mig_{}.ddc", std::process::id()));
    let p = dir.to_str().unwrap().to_string();
    let _ = std::fs::remove_file(&p);
    {
        let m = Model::open(&p).unwrap();
        m.sql().execute_batch("DROP TABLE dim; DROP TABLE dim_geo; UPDATE meta SET value=11 WHERE key='schema_version';").unwrap();
    }
    let mut m = Model::open(&p).unwrap();
    assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
    let lv = m.add_level("L1", 0.0).unwrap();
    assert!(m.auto_dimension(lv).unwrap().is_empty());
    drop(m);
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{p}{s}"));
    }
}
