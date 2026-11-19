// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A locked parameter value holds on every write path the person can reach: an XLSX book,
//! the bulk editor and Match Properties, the assistant, save and reopen, IFC save and undo.
//! The model-level unit tests live in `param_lock_tests.rs`; these go through the editor.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ddc_app::editor::{Editor, OpFn, Sel};
use ddc_app::jobs::Progress;
use ddc_app::save::job::{run, SaveRequest};
use ddc_app::save::patch::Patch;
use ddc_app::save::SourceRef;
use ddc_io::ifc::{check_step, export_ifc, import_ifc_path};
use ddc_io::xlsx::{self, ImportOptions, Report, TableCell as C};
use ddc_model::{
    Edit, Error, LayerSpec, Model, PVal, ParamSet, ParamSpec, ParamType, WallSpec,
};
use e2e_ai_common::{add_door, add_wall, live_hash, tool, tool_results};
use e2e_common::{assert_clean, assert_same, dump_data, journal_len, temp_file};
use serde_json::json;

const MARK: &str = "Mark";
const FIRE: &str = "Fire rating";

struct Fx {
    ed: Editor,
    /// Walls: `a` and `b` carry locks, `c` is free.
    a: i64,
    b: i64,
    c: i64,
    /// Doors: `d1` (in `a`) carries locks, `d2` (in `c`) is free.
    d1: i64,
    d2: i64,
}

fn spec(name: &str) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        ptype: ParamType::Text,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into(), "door".into()],
    }
}

fn set(ed: &mut Editor, name: &str, kind: &str, id: i64, v: &str) {
    ed.model
        .apply_edits(&[Edit::ParamValue {
            param: name.into(),
            kind: kind.into(),
            id,
            value: Some(PVal::Text(v.into())),
        }])
        .unwrap();
}

fn val(m: &Model, name: &str, kind: &str, id: i64) -> String {
    let d = m.param_by_name(name).unwrap().unwrap();
    match m.param_value(d.id, kind, id).unwrap() {
        Some(PVal::Text(s)) => s,
        None => String::new(),
        other => panic!("{name} of {kind} {id}: {other:?}"),
    }
}

fn op(
    ed: &mut Editor,
    f: impl Fn(&mut Model) -> Result<(), Error> + Send + Sync + 'static,
) -> Result<(), Error> {
    ed.run_op(OpFn(Arc::new(f)))
}

/// One undo step: both parameters locked on each of `rows`.
fn lock_rows(ed: &mut Editor, rows: &[(&str, i64)]) {
    let items: Vec<(String, i64, String)> = rows
        .iter()
        .flat_map(|(k, id)| [MARK, FIRE].map(|n| (k.to_string(), *id, n.to_string())))
        .collect();
    op(ed, move |m| {
        let v: Vec<(&str, i64, &str)> = items
            .iter()
            .map(|(k, i, n)| (k.as_str(), *i, n.as_str()))
            .collect();
        m.set_param_lock(&v).map(drop)
    })
    .unwrap();
}

fn is_locked(ed: &Editor, kind: &str, id: i64, name: &str) -> bool {
    ed.model.param_locked(kind, id, name).unwrap()
}

/// Three walls, two doors, every value set, then Mark and Fire locked on `a`, `b` and `d1`.
fn fx() -> Fx {
    let mut ed = Editor::new_in_memory().unwrap();
    let l = ed.level;
    let a = add_wall(&mut ed, l, (0.0, 0.0), (6.0, 0.0));
    let b = add_wall(&mut ed, l, (6.0, 0.0), (6.0, 4.0));
    let c = add_wall(&mut ed, l, (6.0, 4.0), (0.0, 4.0));
    let d1 = add_door(&mut ed, a, 2.0);
    let d2 = add_door(&mut ed, c, 2.0);
    ed.model.add_params(&[spec(MARK), spec(FIRE)]).unwrap();
    for (kind, id, tag) in [
        ("wall", a, "A"),
        ("wall", b, "B"),
        ("wall", c, "C"),
        ("opening", d1, "D1"),
        ("opening", d2, "D2"),
    ] {
        set(&mut ed, MARK, kind, id, &format!("M{tag}"));
        set(&mut ed, FIRE, kind, id, &format!("F-M{tag}"));
    }
    ed.reload().unwrap();
    lock_rows(&mut ed, &[("wall", a), ("wall", b), ("opening", d1)]);
    assert_clean(&ed.model, "fixture");
    Fx { ed, a, b, c, d1, d2 }
}

/// Everything locked still holds its seed value.
fn assert_seed_kept(f: &Fx, ctx: &str) {
    let m = &f.ed.model;
    for (kind, id, tag) in [("wall", f.a, "A"), ("wall", f.b, "B"), ("opening", f.d1, "D1")] {
        assert_eq!(val(m, MARK, kind, id), format!("M{tag}"), "{ctx}: {kind} {id}");
        assert_eq!(val(m, FIRE, kind, id), format!("F-M{tag}"), "{ctx}: {kind} {id}");
        assert!(is_locked(&f.ed, kind, id, MARK) && is_locked(&f.ed, kind, id, FIRE), "{ctx}");
    }
}

// ---- XLSX ----

fn t(s: &str) -> C {
    C::Text(s.into())
}

fn query_rows(m: &Model, sql: &str, n: usize) -> Vec<Vec<C>> {
    let mut st = m.sql().prepare(sql).unwrap();
    let rows: Vec<Vec<C>> = st
        .query_map([], |r| {
            let mut v = vec![C::Num(r.get::<_, i64>(0)? as f64)];
            for i in 1..n {
                match r.get::<_, rusqlite::types::Value>(i)? {
                    rusqlite::types::Value::Text(s) => v.push(C::Text(s)),
                    rusqlite::types::Value::Integer(x) => v.push(C::Num(x as f64)),
                    rusqlite::types::Value::Real(x) => v.push(C::Num(x)),
                    _ => v.push(C::Text(String::new())),
                }
            }
            Ok(v)
        })
        .unwrap()
        .map(|x| x.unwrap())
        .collect();
    rows
}

fn write(path: &Path, sheet: &str, heads: &[&str], rows: &[Vec<C>]) {
    let h: Vec<String> = heads.iter().map(|s| s.to_string()).collect();
    xlsx::export_table(path, sheet, &h, &[], rows, None).unwrap();
}

fn preview(m: &mut Model, p: &Path) -> Report {
    let o = ImportOptions {
        apply: false,
        delete_missing: false,
        new_params: false,
    };
    xlsx::import_xlsx_with(m, p, o).unwrap()
}

fn locked_cells(rep: &Report) -> Vec<(String, Option<i64>, String)> {
    let mut v: Vec<_> = rep
        .ignored
        .iter()
        .filter(|i| i.reason == format!("value of '{}' is locked: skipped", i.column))
        .map(|i| (i.sheet.clone(), i.id, i.column.clone()))
        .collect();
    v.sort();
    v
}

#[test]
fn xlsx_apply_skips_locked_cells() {
    let mut f = fx();
    let (a, b, c, d1, d2) = (f.a, f.b, f.c, f.d1, f.d2);
    let state0 = dump_data(&f.ed.model);

    // Walls: Mark and Fire edited on every row, and the height of the locked wall `a`
    let p = temp_file("lock_walls.xlsx");
    let mut rows = query_rows(
        &f.ed.model,
        "SELECT w.id,lv.name,t.name,w.ax,w.ay,w.bx,w.by,w.base_offset,w.height FROM wall w \
         JOIN level lv ON lv.id=w.level JOIN wall_type t ON t.id=w.wtype ORDER BY w.id",
        9,
    );
    for r in rows.iter_mut() {
        let C::Num(id) = r[0] else { panic!("id cell") };
        r.push(t(&format!("X{}", id as i64)));
        r.push(t(&format!("Y{}", id as i64)));
        if id as i64 == a {
            r[8] = C::Num(3.4);
        }
    }
    let heads = [
        "id",
        "level",
        "type",
        "ax [m]",
        "ay [m]",
        "bx [m]",
        "by [m]",
        "base_offset [m]",
        "height [m]",
        MARK,
        FIRE,
    ];
    write(&p, "Walls", &heads, &rows);
    let rep = preview(&mut f.ed.model, &p);
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    let mut want: Vec<_> = [a, b]
        .iter()
        .flat_map(|id| [FIRE, MARK].map(|n| ("Walls".to_string(), Some(*id), n.to_string())))
        .collect();
    want.sort();
    assert_eq!(locked_cells(&rep), want, "{:?}", rep.ignored);
    assert!(rep.ignored.iter().all(|i| i.row > 1 && i.col > 0));
    assert!(!rep.batch().iter().any(|e| matches!(
        e,
        Edit::ParamValue { kind, id, .. } if kind == "wall" && (*id == a || *id == b)
    )));
    assert_same(&dump_data(&f.ed.model), &state0, "preview changes nothing");

    f.ed.apply_xlsx(rep.batch(), "walls").unwrap();
    assert_clean(&f.ed.model, "walls applied");
    assert_seed_kept(&f, "walls book");
    let m = &f.ed.model;
    assert_eq!(val(m, MARK, "wall", c), format!("X{c}"));
    assert_eq!(val(m, FIRE, "wall", c), format!("Y{c}"));
    let h: f64 = m
        .sql()
        .query_row("SELECT height FROM wall WHERE id=?1", [a], |r| r.get(0))
        .unwrap();
    assert!((h - 3.4).abs() < 1e-9, "only the parameter is locked, not the wall: {h}");
    f.ed.undo();
    assert_same(&dump_data(&f.ed.model), &state0, "one undo reverts the walls book");

    // Openings: the same for a door
    let state1 = dump_data(&f.ed.model);
    let p = temp_file("lock_doors.xlsx");
    let mut rows = query_rows(
        &f.ed.model,
        "SELECT id,wall,along,width,sill,height,kind FROM opening ORDER BY id",
        7,
    );
    for r in rows.iter_mut() {
        let C::Num(id) = r[0] else { panic!("id cell") };
        r.push(t(&format!("X{}", id as i64)));
        r.push(t(&format!("Y{}", id as i64)));
    }
    let heads = [
        "id",
        "wall_id",
        "along [m]",
        "width [m]",
        "sill [m]",
        "height [m]",
        "kind",
        MARK,
        FIRE,
    ];
    write(&p, "Openings", &heads, &rows);
    let rep = preview(&mut f.ed.model, &p);
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    let mut want: Vec<_> = [FIRE, MARK]
        .map(|n| ("Openings".to_string(), Some(d1), n.to_string()))
        .into();
    want.sort();
    assert_eq!(locked_cells(&rep), want, "{:?}", rep.ignored);
    f.ed.apply_xlsx(rep.batch(), "doors").unwrap();
    assert_seed_kept(&f, "doors book");
    assert_eq!(val(&f.ed.model, MARK, "opening", d2), format!("X{d2}"));
    assert_eq!(val(&f.ed.model, FIRE, "opening", d2), format!("Y{d2}"));
    f.ed.undo();
    assert_same(&dump_data(&f.ed.model), &state1, "one undo reverts the doors book");
}

// ---- bulk edit and Match Properties ----

fn mark_edits(ids: &[i64], v: &str) -> Vec<Edit> {
    ids.iter()
        .flat_map(|id| {
            [MARK, FIRE].map(|p| Edit::ParamValue {
                param: p.into(),
                kind: "wall".into(),
                id: *id,
                value: Some(PVal::Text(v.into())),
            })
        })
        .collect()
}

#[test]
fn bulk_and_match_keep_locked() {
    let mut f = fx();
    let (a, b, c) = (f.a, f.b, f.c);

    // bulk edit over locked and free walls: the locked values stay, the free wall is written
    let before = dump_data(&f.ed.model);
    f.ed.apply_xlsx(mark_edits(&[a, b, c], "BULK"), "bulk").unwrap();
    assert_seed_kept(&f, "bulk");
    assert_eq!(val(&f.ed.model, MARK, "wall", c), "BULK");
    assert_eq!(val(&f.ed.model, FIRE, "wall", c), "BULK");
    f.ed.undo();
    assert_same(&dump_data(&f.ed.model), &before, "bulk is one undo step");

    // a batch of locked values only is not an undo step
    let j = journal_len(&f.ed.model);
    f.ed.apply_xlsx(mark_edits(&[a, b], "BULK"), "bulk").unwrap();
    assert_eq!(journal_len(&f.ed.model), j);
    assert_same(&dump_data(&f.ed.model), &before, "only locked values");

    // the value writer itself refuses a locked value (cell edits of a schedule)
    let d = f.ed.model.param_by_name(MARK).unwrap().unwrap().id;
    let r = op(&mut f.ed, move |m| {
        m.set_param_values(&[ParamSet {
            param: d,
            kind: "wall".into(),
            id: a,
            value: Some(PVal::Text("CELL".into())),
        }])
    });
    assert!(r.is_err());
    assert_same(&dump_data(&f.ed.model), &before, "refused write");

    // Match Properties from the free wall: Mark of `b` stays, its unlocked Fire follows
    op(&mut f.ed, move |m| {
        m.clear_param_lock(&[("wall", b, FIRE)]).map(drop)
    })
    .unwrap();
    let before = dump_data(&f.ed.model);
    op(&mut f.ed, move |m| {
        m.match_properties(("wall", c), &[("wall", a), ("wall", b)])
            .map(drop)
    })
    .unwrap();
    assert_eq!(val(&f.ed.model, MARK, "wall", a), "MA");
    assert_eq!(val(&f.ed.model, FIRE, "wall", a), "F-MA");
    assert_eq!(val(&f.ed.model, MARK, "wall", b), "MB");
    assert_eq!(val(&f.ed.model, FIRE, "wall", b), "F-MC");
    f.ed.undo();
    assert_same(&dump_data(&f.ed.model), &before, "match is one undo step");
}

#[test]
fn lock_unlock_one_undo_each() {
    let mut f = fx();
    let c = f.c;
    let free = dump_data(&f.ed.model);
    let j = journal_len(&f.ed.model);

    lock_rows(&mut f.ed, &[("wall", c)]);
    assert_eq!(journal_len(&f.ed.model), j + 1);
    assert!(is_locked(&f.ed, "wall", c, MARK) && is_locked(&f.ed, "wall", c, FIRE));
    f.ed.undo();
    assert!(!is_locked(&f.ed, "wall", c, MARK) && !is_locked(&f.ed, "wall", c, FIRE));
    assert_same(&dump_data(&f.ed.model), &free, "undo of lock");
    f.ed.redo();
    assert!(is_locked(&f.ed, "wall", c, MARK));

    let locked = dump_data(&f.ed.model);
    op(&mut f.ed, move |m| {
        m.clear_param_lock(&[("wall", c, MARK), ("wall", c, FIRE)])
            .map(drop)
    })
    .unwrap();
    assert!(!is_locked(&f.ed, "wall", c, MARK) && !is_locked(&f.ed, "wall", c, FIRE));
    f.ed.undo();
    assert_same(&dump_data(&f.ed.model), &locked, "undo of unlock");
    assert_seed_kept(&f, "lock and unlock");
}

// ---- save and reopen ----

fn sorted_locks(m: &Model) -> Vec<(String, i64, String)> {
    let mut v = m.param_locks().unwrap();
    v.sort();
    v
}

#[test]
fn lock_survives_save_reopen() {
    let f = fx();
    let (a, c) = (f.a, f.c);
    let want = sorted_locks(&f.ed.model);
    assert_eq!(want.len(), 6);
    let data = dump_data(&f.ed.model);

    let p1 = temp_file("lock_reopen_a.ddc");
    f.ed.model.snapshot_to(&p1).unwrap();
    let mut r = Model::open(p1.to_str().unwrap()).unwrap();
    assert_eq!(sorted_locks(&r), want);
    assert_same(&dump_data(&r), &data, "reopened");

    // the lock still holds after reopening: the writer refuses, a free value is written
    let edit = |id: i64, v: &str| Edit::ParamValue {
        param: MARK.into(),
        kind: "wall".into(),
        id,
        value: Some(PVal::Text(v.into())),
    };
    assert!(r.apply_edits(&[edit(a, "NEW")]).is_err());
    assert_eq!(val(&r, MARK, "wall", a), "MA");
    r.apply_edits(&[edit(c, "NEW")]).unwrap();
    assert_eq!(val(&r, MARK, "wall", c), "NEW");
    r.undo().unwrap();

    // the journal came along: unlock then undo gives the lock back
    r.clear_param_lock(&[("wall", a, MARK)]).unwrap();
    assert!(!r.param_locked("wall", a, MARK).unwrap());
    r.undo().unwrap();
    assert!(r.param_locked("wall", a, MARK).unwrap());

    // a second generation keeps them too
    let p2 = temp_file("lock_reopen_b.ddc");
    r.snapshot_to(&p2).unwrap();
    drop(r);
    let r2 = Model::open(p2.to_str().unwrap()).unwrap();
    assert_eq!(sorted_locks(&r2), want);
    assert_same(&dump_data(&r2), &data, "second generation");
}

// ---- IFC save ----

fn layer() -> LayerSpec {
    LayerSpec {
        material: "brick".into(),
        thickness: 0.2,
    }
}

/// An IFC file with four walls on disk.
fn ifc_source() -> std::path::PathBuf {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let ty = m.add_wall_type("ext", &[layer()]).unwrap();
    let p = [(0., 0.), (6., 0.), (6., 4.), (0., 4.)];
    let ws: Vec<WallSpec> = (0..4)
        .map(|i| WallSpec {
            a: p[i],
            b: p[(i + 1) % 4],
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ty,
            offset0: None,
        })
        .collect();
    m.add_walls(&ws).unwrap();
    let path = temp_file("lock_source.ifc");
    std::fs::write(&path, export_ifc(&m).unwrap()).unwrap();
    path
}

fn wall_ids(m: &Model) -> Vec<i64> {
    let mut st = m.sql().prepare("SELECT id FROM wall ORDER BY id").unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .map(|x| x.unwrap())
        .collect()
}

fn tall_walls(m: &Model) -> i64 {
    m.sql()
        .query_row(
            "SELECT count(*) FROM wall WHERE abs(height-3.3)<1e-6",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

/// Project parameters are not part of the IFC file, so the save is checked around the write: the
/// project state the job snapshots, the live model, the file, and the untouched source.
#[test]
fn ifc_save_keeps_locked_value() {
    let src = ifc_source();
    let src_bytes = std::fs::read(&src).unwrap();
    let mut ed = Editor::new_in_memory().unwrap();
    ed.model = Model::open(":memory:").unwrap();
    let path = src.clone();
    op(&mut ed, move |m| {
        import_ifc_path(m, &path)
            .map(drop)
            .map_err(|e| Error::Invalid(e.to_string()))
    })
    .unwrap();
    let ids = wall_ids(&ed.model);
    assert_eq!(ids.len(), 4);
    let (w0, w1) = (ids[0], ids[1]);

    ed.model.add_params(&[spec(MARK), spec(FIRE)]).unwrap();
    for id in &ids {
        set(&mut ed, MARK, "wall", *id, "KEEP");
        set(&mut ed, FIRE, "wall", *id, "EI90");
    }
    lock_rows(&mut ed, &[("wall", w0)]);

    // edits on top: a locked value in a bulk batch, and a real change the file must carry
    ed.apply_xlsx(mark_edits(&[w0, w1], "LOST"), "bulk").unwrap();
    let (_, n) = ddc_app::query::run_text(&mut ed, "walls -> height=height*1.1").unwrap();
    assert_eq!(n, 4);
    assert_eq!(val(&ed.model, MARK, "wall", w0), "KEEP");
    assert_eq!(val(&ed.model, MARK, "wall", w1), "LOST");

    let info = ed
        .model
        .sources()
        .unwrap()
        .into_iter()
        .find(|s| !s.path.is_empty())
        .expect("the import records its source file");
    let sref = SourceRef {
        key: info.source.clone(),
        path: std::path::PathBuf::from(&info.path),
        is_ifc: true,
    };
    let target = ddc_app::save::default_target(None, Some(&sref.path)).unwrap();
    assert!(target.to_string_lossy().ends_with("-edited.ifc"), "{target:?}");

    let snapshot = temp_file("lock_snapshot.ddc");
    ed.model
        .save_project_to(&snapshot, &mut |_, _| Ok(()))
        .unwrap();
    let req = SaveRequest {
        snapshot: snapshot.clone(),
        target: target.clone(),
        backend: Arc::new(Patch::new(sref, false)),
        over_source: false,
        last_hash: String::new(),
        last_journal: None,
        replace_wait: Duration::from_millis(200),
    };
    run(&req, &Progress::new(), &mut |_| Ok(())).unwrap_or_else(|f| panic!("save: {f:?}"));

    // the file is there, valid, carries the height edit; the source is as it was
    let text = std::fs::read_to_string(&target).unwrap();
    check_step(&text).unwrap();
    assert_eq!(std::fs::read(&src).unwrap(), src_bytes, "the source is not replaced");
    let mut back = Model::open(":memory:").unwrap();
    import_ifc_path(&mut back, &target).unwrap();
    assert_eq!(tall_walls(&back), 4);

    // the lock and its value are the same in the live model and in what the job read
    assert_eq!(val(&ed.model, MARK, "wall", w0), "KEEP");
    assert_eq!(val(&ed.model, FIRE, "wall", w0), "EI90");
    assert!(is_locked(&ed, "wall", w0, MARK) && is_locked(&ed, "wall", w0, FIRE));
    let snap = Model::open(snapshot.to_str().unwrap()).unwrap();
    assert_eq!(val(&snap, MARK, "wall", w0), "KEEP");
    assert!(snap.param_locked("wall", w0, MARK).unwrap());
    assert!(snap.param_locked("wall", w0, FIRE).unwrap());
    assert!(!snap.param_locked("wall", w1, MARK).unwrap());
}

// ---- delete and undo ----

#[test]
fn delete_undo_restores_lock() {
    let mut f = fx();
    let (a, d1) = (f.a, f.d1);
    let before = dump_data(&f.ed.model);

    f.ed.selection = vec![Sel::Wall(a)];
    f.ed.delete_selection();
    let m = &f.ed.model;
    let gone: i64 = m
        .sql()
        .query_row("SELECT count(*) FROM wall WHERE id=?1", [a], |r| r.get(0))
        .unwrap();
    assert_eq!(gone, 0, "the wall is deleted");
    assert!(!m.param_locked("wall", a, MARK).unwrap(), "no lock outlives its wall");
    assert!(!m.param_locked("opening", d1, FIRE).unwrap(), "nor the lock of its door");

    f.ed.undo();
    assert_same(&dump_data(&f.ed.model), &before, "undo of delete");
    assert!(is_locked(&f.ed, "wall", a, MARK) && is_locked(&f.ed, "wall", a, FIRE));
    assert!(is_locked(&f.ed, "opening", d1, MARK) && is_locked(&f.ed, "opening", d1, FIRE));
    assert_seed_kept(&f, "delete and undo");
    // and it still refuses a write
    assert!(f
        .ed
        .model
        .apply_edits(&mark_edits(&[a], "NEW"))
        .is_err());
}

// ---- the assistant ----

#[test]
fn ai_apply_refuses_locked_wall_value() {
    let mut f = fx();
    f.ed.revision += 1;
    let before = live_hash(&f.ed.model);
    let (s, provider) = e2e_ai_common::run(
        &mut f.ed,
        vec![tool(
            "update_elements",
            json!({"title": "No", "target": {"filter": "wall"}, "set": {"Mark": "AI"}}),
        )],
    );
    let results = tool_results(&provider);
    assert!(results[0].1, "{results:?}");
    assert!(results[0].0.contains("locked"), "{results:?}");
    assert!(s.plan().is_none());
    assert_eq!(live_hash(&f.ed.model), before);
    assert_seed_kept(&f, "assistant");
}
