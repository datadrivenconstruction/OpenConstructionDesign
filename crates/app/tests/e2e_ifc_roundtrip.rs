//! End-to-end: IFC import → query selection → bulk edit → wall type replace → IFC export →
//! re-import into a fresh model; counts, GlobalId links and edited values must survive.

mod e2e_common;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ddc_app::editor::{Editor, OpFn, Sel};
use ddc_app::query;
use ddc_io::ifc::{check_step, export_ifc, import_ifc, import_ifc_path, ImportReport};
use ddc_model::{LayerSpec, Model, OpeningSpec, RoomSpec, SlabSpec, WallSpec};
use e2e_common::{assert_clean, assert_same, corpus, count, dump_data, journal_len, temp_file};

/// (ext_ref kind, element table)
const KINDS: [(&str, &str); 4] = [
    ("wall", "wall"),
    ("opening", "opening"),
    ("slab", "slab"),
    ("room", "room"),
];

/// levels, walls, openings, slabs, rooms
fn counts(m: &Model) -> [i64; 5] {
    ["level", "wall", "opening", "slab", "room"].map(|t| count(m, t))
}

fn layer(material: &str, thickness: f64) -> LayerSpec {
    LayerSpec {
        material: material.into(),
        thickness,
    }
}

/// 6x4 "ext" box, one "int" partition, a door and a window, a slab, one enclosed room.
fn source_model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let ext = m
        .add_wall_type("ext", &[layer("brick", 0.2), layer("insulation", 0.1)])
        .unwrap();
    let int = m.add_wall_type("int", &[layer("gypsum", 0.125)]).unwrap();
    let p = [(0., 0.), (6., 0.), (6., 4.), (0., 4.)];
    let ws: Vec<WallSpec> = (0..4)
        .map(|i| WallSpec {
            a: p[i],
            b: p[(i + 1) % 4],
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ext,
            offset0: None,
        })
        .collect();
    let ids = m.add_walls(&ws).unwrap();
    m.add_wall(&WallSpec {
        a: (3.0, 0.15),
        b: (3.0, 3.85),
        level: lv,
        base_offset: 0.0,
        height: 2.7,
        wall_type: int,
        offset0: Some(0.0),
    })
    .unwrap();
    for (w, kind, along, width, sill, height) in [
        (0, "door", 1.5, 0.9, 0.0, 2.1),
        (1, "window", 1.0, 1.2, 0.9, 1.4),
    ] {
        m.add_opening(
            ids[w],
            &OpeningSpec {
                along,
                width,
                sill,
                height,
                kind: kind.into(),
            },
        )
        .unwrap();
    }
    let st = m
        .add_slab_type("floor", &[layer("screed", 0.05), layer("concrete", 0.15)])
        .unwrap();
    m.add_slab(&SlabSpec {
        level: lv,
        slab_type: st,
        offset: 0.0,
        outer: p.to_vec(),
        holes: vec![],
    })
    .unwrap();
    m.add_room(&RoomSpec {
        level: lv,
        name: "Office".into(),
        number: "101".into(),
        point: (1.5, 2.0),
        height: 2.7,
    })
    .unwrap();
    m
}

/// The app's import path: one `run_op`, one undo step.
fn import_op(ed: &mut Editor, path: &Path) -> ImportReport {
    let out: Arc<Mutex<Option<ImportReport>>> = Arc::new(Mutex::new(None));
    let (p, o) = (path.to_path_buf(), out.clone());
    let j = journal_len(&ed.model);
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        let r = import_ifc_path(m, &p).map_err(|e| ddc_model::Error::Invalid(e.to_string()))?;
        *o.lock().unwrap() = Some(r);
        Ok(())
    })))
    .unwrap_or_else(|e| panic!("{}: import failed: {e}", path.display()));
    assert_eq!(journal_len(&ed.model), j + 1, "import: one journal op");
    let r = out.lock().unwrap().take();
    r.expect("import report")
}

fn select(ed: &Editor, text: &str) -> Vec<Sel> {
    let q = query::parse(text).unwrap_or_else(|e| panic!("parse {text:?}: {e}"));
    query::eval(ed, &q)
}

fn wall_ids(sels: &[Sel]) -> Vec<i64> {
    sels.iter()
        .filter_map(|s| match *s {
            Sel::Wall(i) => Some(i),
            _ => None,
        })
        .collect()
}

/// `-> field=value` on `sels` as one `run_op` (apply_edits: one journal op); number of edits.
fn assign_op(ed: &mut Editor, sels: &[Sel], set: &str) -> usize {
    let q = query::parse(set).unwrap_or_else(|e| panic!("parse {set:?}: {e}"));
    let edits = query::assign_edits(ed, sels, &q.set).unwrap_or_else(|e| panic!("{set}: {e}"));
    let n = edits.len();
    if n > 0 {
        let j = journal_len(&ed.model);
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.apply_edits(&edits))))
            .unwrap_or_else(|e| panic!("{set}: apply failed: {e}"));
        // Editor::undo pops one model op per run_op
        assert_eq!(journal_len(&ed.model), j + 1, "{set}: one journal op");
    }
    n
}

fn wall_height(m: &Model, id: i64) -> f64 {
    m.sql()
        .query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

fn wall_type_name(m: &Model, id: i64) -> String {
    m.sql()
        .query_row(
            "SELECT t.name FROM wall w JOIN wall_type t ON t.id=w.wtype WHERE w.id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

fn strings(m: &Model, sql: &str, arg: &str) -> Vec<String> {
    let mut st = m.sql().prepare(sql).unwrap();
    let v = st
        .query_map([arg], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    v
}

fn guids(m: &Model, kind: &str) -> Vec<String> {
    strings(
        m,
        "SELECT guid FROM ext_ref WHERE kind=?1 ORDER BY guid",
        kind,
    )
}

/// Wall type name → wall count, by name.
fn type_hist(m: &Model) -> Vec<(String, i64)> {
    let mut st = m
        .sql()
        .prepare(
            "SELECT t.name, count(*) FROM wall w JOIN wall_type t ON t.id=w.wtype \
             GROUP BY t.name ORDER BY t.name",
        )
        .unwrap();
    let v = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    v
}

/// Walls as (type name, axis, base offset, height) rounded to 0.1 mm, sorted.
fn wall_rows(m: &Model) -> Vec<(String, [i64; 6])> {
    let mut st = m
        .sql()
        .prepare(
            "SELECT t.name, w.ax, w.ay, w.bx, w.by, w.base_offset, w.height \
             FROM wall w JOIN wall_type t ON t.id=w.wtype",
        )
        .unwrap();
    let mut v: Vec<(String, [i64; 6])> = st
        .query_map([], |r| {
            let mut f = [0i64; 6];
            for (k, x) in f.iter_mut().enumerate() {
                *x = (r.get::<_, f64>(k + 1)? * 1e4).round() as i64;
            }
            Ok((r.get(0)?, f))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    v.sort();
    v
}

/// Every element row of the kind is linked to exactly one GlobalId, all distinct.
fn assert_linked(m: &Model, ctx: &str) {
    for (kind, table) in KINDS {
        let g = guids(m, kind);
        let uniq: HashSet<&String> = g.iter().collect();
        assert_eq!(uniq.len(), g.len(), "{ctx}: duplicate {kind} GUIDs");
        assert_eq!(
            g.len() as i64,
            count(m, table),
            "{ctx}: {kind} rows vs GUID links"
        );
    }
}

fn quoted(s: &str) -> Option<String> {
    if !s.contains('"') {
        Some(format!("\"{s}\""))
    } else if !s.contains('»') {
        Some(format!("«{s}»"))
    } else {
        None
    }
}

/// The whole workflow on one IFC file; `light` adds exact checks for [`source_model`].
fn chain(src: &Path, light: bool) {
    let tag = src.display().to_string();
    let mut ed = Editor::new_in_memory().unwrap();
    // an empty model: the default level and wall types would skew the counts
    ed.model = Model::open(":memory:").unwrap();

    // 1. import
    let rep = import_op(&mut ed, src);
    assert_clean(&ed.model, &format!("{tag}: import"));
    assert!(ed.can_undo(), "{tag}: import is an undo step");
    if light {
        assert!(rep.skipped.is_empty(), "{tag}: {:?}", rep.skipped);
        assert_eq!(counts(&ed.model), [1, 5, 2, 1, 1], "{tag}: {rep:?}");
        assert_linked(&ed.model, &format!("{tag}: import"));
    }
    let src_guids: HashSet<String> = KINDS.iter().flat_map(|k| guids(&ed.model, k.0)).collect();
    let base = dump_data(&ed.model);
    let mut steps = 0;

    // 2. select by query, 3. bulk edit on the selection
    let (hsel, hq) = if light {
        ("walls type=ext", "-> height=height*1.1")
    } else {
        ("walls", "-> height=height*1.1")
    };
    let sels = select(&ed, hsel);
    let hw = wall_ids(&sels);
    if light {
        let mut want: Vec<i64> = ddc_model_ids(
            &ed.model,
            "SELECT w.id FROM wall w JOIN wall_type t ON t.id=w.wtype WHERE t.name='ext' ORDER BY w.id",
        );
        let mut got = hw.clone();
        got.sort();
        want.sort();
        assert_eq!(got, want, "{tag}: query selection");
        assert_eq!(sels.len(), 4, "{tag}: only walls selected");
    }
    if hw.is_empty() {
        eprintln!("e2e: {tag}: no walls, bulk edit and type replace not covered");
    } else {
        let before: Vec<f64> = hw.iter().map(|&i| wall_height(&ed.model, i)).collect();
        let n = assign_op(&mut ed, &sels, hq);
        assert_eq!(n, hw.len(), "{tag}: one edit per selected wall");
        steps += 1;
        assert_clean(&ed.model, &format!("{tag}: bulk edit"));
        for (&i, h0) in hw.iter().zip(&before) {
            let h = wall_height(&ed.model, i);
            assert!(
                (h - h0 * 1.1).abs() < 1e-9,
                "{tag}: wall {i} height {h} want {}",
                h0 * 1.1
            );
        }
    }

    // 4. type replace on a query selection
    let replace = if light {
        Some((
            "walls type=int".to_string(),
            "int".to_string(),
            "ext".to_string(),
        ))
    } else {
        let mut hist = type_hist(&ed.model);
        hist.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        match (hist.first(), hist.get(1)) {
            (Some(a), Some(b)) => match (quoted(&a.0), quoted(&b.0)) {
                (Some(qa), Some(_)) => Some((format!("walls type={qa}"), a.0.clone(), b.0.clone())),
                _ => None,
            },
            _ => None,
        }
    };
    let mut replaced = vec![];
    match replace {
        None => eprintln!("e2e: {tag}: fewer than two wall types, type replace not covered"),
        Some((q, from, to)) => {
            // the type filter may match by substring: keep exact names, a few walls
            let sel: Vec<Sel> = select(&ed, &q)
                .into_iter()
                .filter(|s| matches!(*s, Sel::Wall(i) if wall_type_name(&ed.model, i) == from))
                .take(if light { usize::MAX } else { 5 })
                .collect();
            assert!(
                !sel.is_empty(),
                "{tag}: no walls of type {from:?} via {q:?}"
            );
            if light {
                assert_eq!(sel.len(), 1, "{tag}: one partition");
            }
            let set = format!("-> type={}", quoted(&to).unwrap());
            let n = assign_op(&mut ed, &sel, &set);
            assert_eq!(n, sel.len(), "{tag}: one edit per wall");
            steps += 1;
            assert_clean(&ed.model, &format!("{tag}: type replace"));
            replaced = wall_ids(&sel);
            for &i in &replaced {
                assert_eq!(wall_type_name(&ed.model, i), to, "{tag}: wall {i} type");
            }
        }
    }

    // undo of the edits returns the post-import data, redo the edited data
    let edited = dump_data(&ed.model);
    for _ in 0..steps {
        assert!(ed.can_undo());
        ed.undo();
        assert_clean(&ed.model, &format!("{tag}: undo"));
    }
    assert_same(
        &dump_data(&ed.model),
        &base,
        &format!("{tag}: undo of the edits"),
    );
    for _ in 0..steps {
        assert!(ed.can_redo());
        ed.redo();
        assert_clean(&ed.model, &format!("{tag}: redo"));
    }
    assert_same(
        &dump_data(&ed.model),
        &edited,
        &format!("{tag}: redo of the edits"),
    );

    // 5. export
    let text = export_ifc(&ed.model).unwrap_or_else(|e| panic!("{tag}: export: {e}"));
    check_step(&text).unwrap_or_else(|e| panic!("{tag}: exported STEP: {e}"));
    let again = export_ifc(&ed.model).unwrap();
    assert!(
        text == again,
        "{tag}: export is not deterministic (GUIDs would drift)"
    );

    // 6. re-import into a fresh model
    let mut back = Model::open(":memory:").unwrap();
    let rep2 =
        import_ifc(&mut back, text.as_bytes()).unwrap_or_else(|e| panic!("{tag}: re-import: {e}"));
    assert_clean(&back, &format!("{tag}: re-import"));
    assert_eq!(
        counts(&back),
        counts(&ed.model),
        "{tag}: levels/walls/openings/slabs/rooms {rep2:?}"
    );
    assert_linked(&back, &format!("{tag}: re-import"));
    for (kind, _) in KINDS {
        for g in guids(&back, kind) {
            // `#k` suffix: one product split into several walls
            let base_g = g.split('#').next().unwrap_or(&g);
            assert!(
                text.contains(&format!("'{base_g}'")),
                "{tag}: {kind} GUID {g} not in the export"
            );
        }
    }
    // the exported type name and height are what the edits wrote
    assert_eq!(type_hist(&back), type_hist(&ed.model), "{tag}: wall types");
    if light {
        assert_eq!(wall_rows(&back), wall_rows(&ed.model), "{tag}: wall rows");
        let h33 = wall_rows(&back).iter().filter(|w| w.1[5] == 33_000).count();
        assert_eq!(h33, 4, "{tag}: bulk-edited height survives");
        assert_eq!(
            type_hist(&back),
            vec![("ext".to_string(), 5)],
            "{tag}: replaced type survives"
        );
    }
    // not covered: the exporter writes GlobalIds from (kind, row id), not the GUIDs stored
    // in ext_ref on import, so the source file's GUIDs are kept only when row ids coincide
    let back_guids: HashSet<String> = KINDS.iter().flat_map(|k| guids(&back, k.0)).collect();
    eprintln!(
        "e2e: {tag}: source GUIDs kept after export {}/{}",
        src_guids.intersection(&back_guids).count(),
        src_guids.len()
    );

    // a second import of the same export links by GlobalId: no new rows, nothing updated
    let before = counts(&back);
    let rep3 = import_ifc(&mut back, text.as_bytes()).unwrap();
    assert_clean(&back, &format!("{tag}: second re-import"));
    assert_eq!(counts(&back), before, "{tag}: second re-import adds rows");
    assert_eq!(rep3.updated, 0, "{tag}: second re-import {rep3:?}");
    if light {
        assert_eq!(replaced.len(), 1);
    }
}

fn ddc_model_ids(m: &Model, sql: &str) -> Vec<i64> {
    let mut st = m.sql().prepare(sql).unwrap();
    let v = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    v
}

#[test]
fn e2e_ifc_roundtrip_synthetic() {
    let text = export_ifc(&source_model()).unwrap();
    let path = temp_file("ifc_roundtrip_src.ifc");
    std::fs::write(&path, &text).unwrap();
    chain(&path, true);
    let _ = std::fs::remove_file(&path);
}

#[test]
#[ignore = "corpus"]
fn e2e_ifc_roundtrip_corpus() {
    let files: Vec<PathBuf> = corpus("DDC_E2E_IFC", "").into_iter().collect();
    for f in files {
        chain(&f, false);
    }
}
