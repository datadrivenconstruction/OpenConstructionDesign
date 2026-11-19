//! Design depth: the schema step, save and reopen, undo, the "depth >= N" filter, the
//! project setting, request words and the pre-run estimate.

use super::*;
use crate::{LayerSpec, SlabSpec, WallSpec};

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ddc_dp1_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Four walls and a slab on one level.
fn model_at(path: &str) -> (Model, Vec<i64>, i64) {
    let mut m = Model::open(path).unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m
        .add_wall_type("w", &[LayerSpec { material: "brick".into(), thickness: 0.2 }])
        .unwrap();
    let walls: Vec<i64> = (0..4)
        .map(|i| {
            let y = f64::from(i) * 3.0;
            m.add_wall(&WallSpec {
                a: (0.0, y),
                b: (5.0, y),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap()
        })
        .collect();
    let st = m
        .add_slab_type("s", &[LayerSpec { material: "concrete".into(), thickness: 0.2 }])
        .unwrap();
    let slab = m
        .add_slab(&SlabSpec {
            level: lv,
            slab_type: st,
            offset: 0.0,
            outer: vec![(0.0, 0.0), (5.0, 0.0), (5.0, 9.0), (0.0, 9.0)],
            holes: vec![],
        })
        .unwrap();
    (m, walls, slab)
}

fn version(m: &Model) -> i64 {
    m.db
        .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn step_51_is_the_version_and_follows_the_component_library_step() {
    // 50 belongs to the component library; later steps add above 51
    assert!(crate::schema::VERSION >= 51);
    assert!(crate::schema::STEPS.contains(&51));
}

#[test]
fn a_file_of_the_previous_version_opens_and_gets_the_tables() {
    let d = dir("mig");
    let p = d.join("old.ddc");
    let ps = p.to_str().unwrap();
    let w0;
    {
        let (m, w, _) = model_at(ps);
        w0 = w[0];
        m.db.execute_batch("DROP TABLE elem_depth; DROP TABLE depth_cfg").unwrap();
        m.db.execute("UPDATE meta SET value=49 WHERE key='schema_version'", []).unwrap();
    }
    let mut m = Model::open(ps).unwrap();
    assert!(table_exists(&m.db, TABLE).unwrap() && table_exists(&m.db, CFG_TABLE).unwrap());
    assert_eq!(version(&m), crate::schema::VERSION);
    // elements made before depth existed read as not set, the setting as the default
    assert_eq!(m.elem_depth("wall", w0).unwrap(), None);
    assert_eq!(m.depth_settings().unwrap(), DepthSettings::default());
    m.set_elem_depth(&[("wall", w0)], 3, None).unwrap();
    assert_eq!(m.elem_depth("wall", w0).unwrap().map(|e| e.depth), Some(3));
    drop(m);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn depth_and_setting_survive_save_and_reopen() {
    let d = dir("rt");
    let p = d.join("m.ddc");
    let ps = p.to_str().unwrap();
    let (w, slab);
    let mut s = DepthSettings { project: 3, ..DepthSettings::default() };
    s.disc[Discipline::Mep.index()] = DiscSetting { on: false, own: Some(4) };
    s.disc[Discipline::Doc.index()] = DiscSetting { on: true, own: Some(2) };
    s.disc[Discipline::Struct.index()] = DiscSetting { on: false, own: None };
    {
        let (mut m, ws, sl) = model_at(ps);
        (w, slab) = (ws, sl);
        m.set_elem_depth(&[("wall", w[0]), ("wall", w[1])], 4, None).unwrap();
        m.set_elem_depth(&[("slab", slab)], 3, None).unwrap();
        m.set_elem_depth(&[("wall", w[2])], 2, Some(Discipline::Struct)).unwrap();
        assert!(m.set_depth_settings(&s).unwrap());
    }
    let m = Model::open(ps).unwrap();
    let want = |depth, discipline| Some(ElemDepth { depth, discipline });
    assert_eq!(m.elem_depth("wall", w[0]).unwrap(), want(4, Discipline::Arch));
    assert_eq!(m.elem_depth("wall", w[1]).unwrap(), want(4, Discipline::Arch));
    assert_eq!(m.elem_depth("slab", slab).unwrap(), want(3, Discipline::Struct));
    assert_eq!(m.elem_depth("wall", w[2]).unwrap(), want(2, Discipline::Struct));
    assert_eq!(m.elem_depth("wall", w[3]).unwrap(), None);
    assert_eq!(m.depth_settings().unwrap(), s);
    assert_eq!(s.of(Discipline::Arch), Some(3));
    assert_eq!(s.of(Discipline::Mep), None);
    assert_eq!(s.of(Discipline::Doc), Some(2));
    assert_eq!(s.of(Discipline::Struct), None);
    assert_eq!(version(&m), crate::schema::VERSION);
    drop(m);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn query_depth_at_least_returns_the_exact_set() {
    let (mut m, w, slab) = model_at(":memory:");
    m.set_elem_depth(&[("wall", w[0])], 1, None).unwrap();
    m.set_elem_depth(&[("wall", w[1])], 3, None).unwrap();
    m.set_elem_depth(&[("wall", w[2])], 4, None).unwrap();
    m.set_elem_depth(&[("slab", slab)], 5, None).unwrap();
    let got = m.elems_at_depth(3, None).unwrap();
    assert_eq!(
        got,
        vec![
            ("slab".to_string(), slab),
            ("wall".to_string(), w[1]),
            ("wall".to_string(), w[2]),
        ]
    );
    assert_eq!(
        m.elems_at_depth(3, Some(Discipline::Struct)).unwrap(),
        vec![("slab".to_string(), slab)]
    );
    assert_eq!(m.elems_at_depth(1, None).unwrap().len(), 4);
    assert!(m.elems_at_depth(5, Some(Discipline::Arch)).unwrap().is_empty());
}

#[test]
fn a_deleted_element_leaves_the_filter() {
    let (mut m, w, _) = model_at(":memory:");
    m.set_elem_depth(&[("wall", w[0]), ("wall", w[1])], 3, None).unwrap();
    m.db.execute("DELETE FROM wall WHERE id=?1", [w[0]]).unwrap();
    assert_eq!(m.elems_at_depth(1, None).unwrap(), vec![("wall".to_string(), w[1])]);
}

#[test]
fn every_write_is_one_undo_step() {
    let (mut m, w, _) = model_at(":memory:");
    m.set_elem_depth(&[("wall", w[0])], 2, None).unwrap();
    m.set_elem_depth(&[("wall", w[0]), ("wall", w[1])], 4, None).unwrap();
    m.undo().unwrap();
    assert_eq!(m.elem_depth("wall", w[0]).unwrap().map(|e| e.depth), Some(2));
    assert_eq!(m.elem_depth("wall", w[1]).unwrap(), None);
    let s = DepthSettings { project: 4, ..DepthSettings::default() };
    m.set_depth_settings(&s).unwrap();
    assert_eq!(m.depth_settings().unwrap().project, 4);
    m.undo().unwrap();
    assert_eq!(m.depth_settings().unwrap(), DepthSettings::default());
    // an unchanged setting writes nothing
    assert!(!m.set_depth_settings(&DepthSettings::default()).unwrap());
}

#[test]
fn bad_writes_are_refused() {
    let (mut m, w, _) = model_at(":memory:");
    assert!(m.set_elem_depth(&[("wall", w[0])], 0, None).is_err());
    assert!(m.set_elem_depth(&[("wall", w[0])], 6, None).is_err());
    assert!(m.set_elem_depth(&[("level", 1)], 3, None).is_err());
    assert!(m.set_elem_depth(&[("wall", 999_999)], 3, None).is_err());
    assert!(m.elem_depths().unwrap().is_empty());
}

#[test]
fn every_depth_table_has_a_known_discipline_and_is_unique() {
    let mut seen = std::collections::HashSet::new();
    for (t, d) in TABLES {
        assert!(seen.insert(*t), "{t} twice");
        assert_eq!(Discipline::of_table(t), Some(*d));
    }
    for d in Discipline::ALL {
        assert_eq!(Discipline::parse(d.as_str()), Some(d));
        assert!(TABLES.iter().any(|t| t.1 == d), "{d:?} owns no table");
    }
}

#[test]
fn request_words_name_the_depth() {
    for (text, want) in [
        (crate::test_ru::ru("depth_do_d4"), Some(4)),
        (crate::test_ru::ru("depth_d3_ar"), Some(3)),
        ("LOD 300 please", Some(3)),
        ("lod350", Some(4)),
        (crate::test_ru::ru("depth_working"), Some(4)),
        (crate::test_ru::ru("depth_working_set"), Some(4)),
        (crate::test_ru::ru("depth_concept_only"), Some(1)),
        (crate::test_ru::ru("depth_sketch_house"), Some(2)),
        ("from D2 to D4", Some(4)),
        ("shop drawings for the facade", Some(5)),
        (crate::test_ru::ru("depth_office_seats"), None),
        (crate::test_ru::ru("depth_show_3d"), None),
        ("wall ad3 type", None),
        ("D7 is not a depth", None),
        ("", None),
    ] {
        assert_eq!(parse_words(text), want, "{text}");
    }
}

#[test]
fn lod_figures() {
    assert_eq!([1, 2, 3, 4, 5].map(lod), [100, 200, 300, 350, 400]);
    assert_eq!(lod(0), 100);
    assert_eq!(lod(9), 400);
}

/// Gate counts of the generator for a 4-storey office (task graph sizes of the D2 draft).
fn office() -> Vec<(&'static str, usize)> {
    vec![
        ("levels", 5),
        ("walls", 240),
        ("doors", 96),
        ("windows", 120),
        ("rooms", 100),
        ("slabs", 4),
        ("stairs", 4),
        ("shafts", 8),
        ("roofs", 1),
        ("grids", 12),
    ]
}

#[test]
fn estimate_grows_with_depth_and_drops_with_disciplines() {
    let at = |d: u8| estimate(&office(), &DepthSettings { project: d, ..Default::default() });
    let e: Vec<Estimate> = (1..=5).map(at).collect();
    for w in e.windows(2) {
        assert!(w[1].elements > w[0].elements, "{w:?}");
        assert!(w[1].stages > w[0].stages && w[1].calls > w[0].calls);
        assert!(w[1].seconds > w[0].seconds && w[1].tokens > w[0].tokens);
        assert!(w[1].sheets >= w[0].sheets);
    }
    // D2 is the draft the counts describe: arch elements are the counts, nothing is added
    let d2 = &e[1];
    assert_eq!(d2.by_disc[Discipline::Arch.index()], 240 + 96 + 120 + 100 + 4 + 1);
    // 12 grid lines: 6 x 6 crossings on each of 4 storeys, plus 4 slabs and the grids
    assert_eq!(d2.by_disc[Discipline::Struct.index()], 4 + 12 + 6 * 6 * 4);
    assert_eq!(d2.added, 0);
    assert_eq!(d2.stages, 8);
    assert_eq!(d2.tokens, u64::from(d2.calls) * TOKENS_PER_CALL);
    // only arch at D3: fewer stages, elements and calls than all four disciplines
    let mut only = DepthSettings { project: 3, ..Default::default() };
    for d in [Discipline::Struct, Discipline::Mep, Discipline::Doc] {
        only.disc[d.index()].on = false;
    }
    let a = estimate(&office(), &only);
    assert_eq!(a.stages, 3);
    assert_eq!(a.sheets, 0);
    assert!(a.elements < e[2].elements && a.calls < e[2].calls);
    assert_eq!(a.by_disc[Discipline::Mep.index()], 0);
    // all off: nothing to run
    let mut none = DepthSettings::default();
    for s in &mut none.disc {
        s.on = false;
    }
    let z = estimate(&office(), &none);
    assert_eq!((z.elements, z.stages, z.calls, z.tokens), (0, 0, 0, 0));
}

#[test]
fn estimate_from_the_model_rows_matches_the_counts() {
    let (m, _, _) = model_at(":memory:");
    let c = m.depth_base_counts().unwrap();
    let n = |k: &str| c.iter().find(|x| x.0 == k).map(|x| x.1);
    assert_eq!(n("walls"), Some(4));
    assert_eq!(n("slabs"), Some(1));
    assert_eq!(n("levels"), Some(1));
    let e = estimate(&c, &DepthSettings::default());
    assert_eq!(e.by_disc[Discipline::Arch.index()], 4);
    assert!(e.elements > 4 && e.calls > 0);
}
