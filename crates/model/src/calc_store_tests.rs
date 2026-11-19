//! Schema step 44: structural analysis intent (undoable, validated) and runs (outside undo,
//! retention to an archive, never deleted).

use serde_json::json;

use super::*;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc-calc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn has_table(m: &Model, t: &str) -> bool {
    m.db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [t],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

fn has_col(m: &Model, c: &str) -> bool {
    m.db.query_row(
        "SELECT count(*) FROM pragma_table_info('material') WHERE name=?1",
        [c],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

fn ov(target: &str, field: &str, value: J) -> AnOverride {
    AnOverride {
        id: 0,
        target: target.into(),
        field: field.into(),
        value,
        anchor: None,
        locked: false,
        src: "manual".into(),
        why: None,
    }
}

fn case(name: &str) -> LoadCase {
    LoadCase {
        id: 0,
        name: name.into(),
        category: "permanent".into(),
        self_weight: true,
        note: String::new(),
        locked: false,
        src: "manual".into(),
    }
}

fn point(lcase: i64, f: f64) -> Load {
    Load {
        id: 0,
        lcase,
        kind: "point".into(),
        target: "m:beam:4".into(),
        frame: "global".into(),
        dir: "-z".into(),
        value: json!({ "f": f }),
        grp: None,
        locked: false,
        src: "manual".into(),
        why: None,
    }
}

fn combo(name: &str, terms: Vec<(i64, f64)>) -> LoadCombo {
    LoadCombo {
        id: 0,
        name: name.into(),
        state: "uls".into(),
        generated: false,
        rule: None,
        locked: false,
        src: "manual".into(),
        terms,
    }
}

fn run(hash: &str) -> CalcRun {
    CalcRun {
        id: 0,
        created: 0,
        input_hash: hash.into(),
        fem_version: "fem 0.1".into(),
        status: "ok".into(),
        stats: json!({ "nodes": 4 }),
        pinned: false,
        src: "manual".into(),
        note: String::new(),
    }
}

fn invalid(r: Result<impl std::fmt::Debug, Error>, what: &str) {
    match r {
        Err(Error::Invalid(t)) => assert!(t.contains(what), "{t} lacks {what}"),
        other => panic!("expected a refusal naming {what}, got {other:?}"),
    }
}

#[test]
fn a_fresh_model_has_every_table_and_material_column() {
    let m = Model::open(":memory:").unwrap();
    for t in CALC_TABLES {
        assert!(has_table(&m, t), "{t}");
    }
    for c in MECH_COLS {
        assert!(has_col(&m, c), "{c}");
    }
    assert!(crate::schema::STEPS.contains(&44));
    assert!(m.check_integrity().unwrap().is_empty());
}

/// A file of the previous version (no step 44 tables, no material mechanics) opens, gets the
/// step, keeps its content and leaves a backup; a current file reopens with no schema change.
#[test]
fn an_old_file_opens_and_gets_the_step() {
    let dir = temp_dir("old");
    let p = dir.join("old.ddc");
    let p = p.to_str().unwrap();
    let (hash, mats) = {
        let mut m = Model::open(p).unwrap();
        m.add_level("L1", 0.0).unwrap();
        m.add_level("L2", 3.0).unwrap();
        let h = m.content_hash().unwrap();
        let mats = m.materials().unwrap();
        for t in CALC_TABLES {
            m.db.execute_batch(&format!("DROP TABLE {t}")).unwrap();
        }
        for c in MECH_COLS {
            m.db.execute_batch(&format!("ALTER TABLE material DROP COLUMN {c}")).unwrap();
        }
        m.db.execute("UPDATE meta SET value=43 WHERE key='schema_version'", []).unwrap();
        (h, mats)
    };
    let m = Model::open(p).unwrap();
    for t in CALC_TABLES {
        assert!(has_table(&m, t), "{t}");
    }
    for c in MECH_COLS {
        assert!(has_col(&m, c), "{c}");
    }
    let v: i64 = m
        .db
        .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, crate::schema::VERSION);
    assert_eq!(m.content_hash().unwrap(), hash, "content survives the step");
    assert_eq!(m.materials().unwrap(), mats);
    assert_eq!(m.level_list().unwrap().len(), 2);
    assert!(m.check_integrity().unwrap().is_empty());
    let backup = std::fs::read_dir(&dir)
        .unwrap()
        .any(|e| e.unwrap().file_name().to_string_lossy().starts_with("old.ddc.bak-v43"));
    assert!(backup, "a copy of the old file is kept");
    let cookie = |m: &Model| -> i64 {
        m.db.query_row("PRAGMA schema_version", [], |r| r.get(0)).unwrap()
    };
    let before = cookie(&m);
    drop(m);
    let m = Model::open(p).unwrap();
    assert_eq!(cookie(&m), before, "a current file opens with no schema change");
    drop(m);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overrides_upsert_validate_and_undo() {
    let mut m = Model::open(":memory:").unwrap();
    let ecc = json!({ "frame": "global", "d": [0.0, 0.1, 0.0] });
    let id = m.set_an_override(&ov("m:beam:7", "ecc_start", ecc.clone())).unwrap();
    let again = json!({ "frame": "local", "d": [0.0, 0.0, 0.2] });
    assert_eq!(m.set_an_override(&ov("m:beam:7", "ecc_start", again.clone())).unwrap(), id);
    assert_eq!(m.an_overrides().unwrap()[0].value, again);
    m.undo().unwrap();
    assert_eq!(m.an_overrides().unwrap()[0].value, ecc, "one undo step per edit");
    m.undo().unwrap();
    assert!(m.an_overrides().unwrap().is_empty());

    invalid(m.set_an_override(&ov("beam 7", "ecc_start", ecc.clone())), "target");
    invalid(m.set_an_override(&ov("m:beam:7", "dofs", json!("FFFPPP"))), "field");
    let far = json!({ "frame": "global", "d": [11.0, 0.0, 0.0] });
    invalid(m.set_an_override(&ov("m:beam:7", "ecc_end", far)), "ecc_end");
    let rel = json!({ "mask": "FFFFPS" });
    invalid(m.set_an_override(&ov("m:beam:7", "release_end", rel)), "release_end");
    let mat = json!({ "name": "unobtainium" });
    invalid(m.set_an_override(&ov("m:column:2", "material", mat)), "material");
    invalid(m.set_an_override(&ov("p:slab:3", "role", json!("shell2"))), "role");
    invalid(m.set_an_override(&ov("global", "merge", json!(2.0))), "merge");
    let mut bad_src = ov("s:footing:1", "dofs", json!("FFFPPP"));
    bad_src.src = "robot".into();
    invalid(m.set_an_override(&bad_src), "src");
    m.set_an_override(&ov("s:footing:1", "dofs", json!("FFFPPP"))).unwrap();
    m.set_an_override(&ov("m:column:2", "material", json!({ "name": "concrete" }))).unwrap();
    m.set_an_override(&ov("p:slab:3", "role", json!("load_only"))).unwrap();
    m.set_an_override(&ov("n:m:beam:7:end", "move", json!({ "d": [0.0, 0.0, 0.05] })))
        .unwrap();
}

#[test]
fn locked_rows_refuse_automation() {
    let mut m = Model::open(":memory:").unwrap();
    let mut o = ov("m:beam:7", "role", json!("truss"));
    o.locked = true;
    let id = m.set_an_override(&o).unwrap();
    let mut ai = ov("m:beam:7", "role", json!("frame"));
    ai.src = "ai:s1".into();
    invalid(m.set_an_override(&ai), "locked");
    invalid(m.delete_an_override(id, "ai:s1"), "locked");
    invalid(m.set_calc_locked("an_override", id, false, "ai:s1"), "locked");
    m.set_calc_locked("an_override", id, false, "manual").unwrap();
    m.set_an_override(&ai).unwrap();
    assert_eq!(m.an_overrides().unwrap()[0].src, "ai:s1");
    m.undo().unwrap();
    m.undo().unwrap();
    assert!(m.an_overrides().unwrap()[0].locked, "undo locks it again");
}

#[test]
fn cases_loads_and_combos_round_trip_with_cascade_and_undo() {
    let mut m = Model::open(":memory:").unwrap();
    let g = m.add_load_case(&case("G")).unwrap();
    let mut q = case("Q");
    q.category = "imposed_office".into();
    let q = m.add_load_case(&q).unwrap();
    invalid(m.add_load_case(&case("G")), "name");
    let mut bad = case("X");
    bad.category = "seismic".into();
    invalid(m.add_load_case(&bad), "category");

    let l1 = m.add_load(&point(g, -10e3)).unwrap();
    m.add_load(&point(q, -5e3)).unwrap();
    let mut line = point(g, 0.0);
    line.kind = "line".into();
    line.value = json!({ "q1": -2e3, "x1": 1.0, "x2": 0.5 });
    invalid(m.add_load(&line), "x2");
    line.value = json!({ "q1": -2e3, "q2": -3e3, "x1": 0.5, "x2": 2.0 });
    m.add_load(&line).unwrap();
    let mut area = point(g, 0.0);
    area.kind = "area".into();
    area.target = "filter:slab level=L1".into();
    area.value = json!({ "p": -1.5e3 });
    m.add_load(&area).unwrap();
    invalid(m.add_load(&point(999, 1.0)), "lcase");
    let mut nan = point(g, 0.0);
    nan.value = json!({ "f": "a lot" });
    invalid(m.add_load(&nan), "value");
    let mut dir = point(g, 1.0);
    dir.dir = "[0, 0, 0]".into();
    invalid(m.add_load(&dir), "dir");
    dir.dir = "[1, 0, -1]".into();
    m.set_load(l1, &dir).unwrap();
    assert_eq!(m.loads(Some(g)).unwrap().len(), 3);
    assert_eq!(m.loads(None).unwrap().len(), 4);

    let c = m.add_load_combo(&combo("ULS-01", vec![(g, 1.35), (q, 1.5)])).unwrap();
    invalid(m.add_load_combo(&combo("ULS-02", vec![(g, 1.0), (g, 1.0)])), "terms");
    invalid(m.add_load_combo(&combo("ULS-02", vec![])), "terms");
    m.set_load_combo(c, &combo("ULS-01", vec![(g, 1.0)])).unwrap();
    assert_eq!(m.load_combos().unwrap()[0].terms, vec![(g, 1.0)]);
    m.undo().unwrap();
    assert_eq!(m.load_combos().unwrap()[0].terms, vec![(g, 1.35), (q, 1.5)]);
    assert!(m.check_integrity().unwrap().is_empty());

    let before = m.content_hash().unwrap();
    assert_eq!(m.delete_load_case(g, "manual").unwrap(), (3, 1));
    assert!(m.loads(Some(g)).unwrap().is_empty());
    assert_eq!(m.load_combos().unwrap()[0].terms, vec![(q, 1.5)]);
    assert!(m.check_integrity().unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.content_hash().unwrap(), before, "one undo brings case, loads, terms back");
}

#[test]
fn automation_cannot_delete_a_case_a_locked_combo_uses() {
    let mut m = Model::open(":memory:").unwrap();
    let g = m.add_load_case(&case("G")).unwrap();
    let mut c = combo("SLS-C-01", vec![(g, 1.0)]);
    c.state = "sls_char".into();
    c.locked = true;
    let c = m.add_load_combo(&c).unwrap();
    invalid(m.delete_load_case(g, "ai:s1"), "locked");
    invalid(m.delete_load_combo(c, "mcp:client"), "locked");
    m.delete_load_combo(c, "manual").unwrap();
    m.delete_load_case(g, "ai:s1").unwrap();
}

#[test]
fn settings_and_material_mechanics_are_undoable() {
    let mut m = Model::open(":memory:").unwrap();
    let rules = json!({ "permanent": { "uls": [1.35, 1.0] } });
    m.set_calc_setting("combo_rules", Some(&rules)).unwrap();
    assert_eq!(m.calc_setting("combo_rules").unwrap(), Some(rules.clone()));
    m.set_calc_setting("combo_rules", None).unwrap();
    assert_eq!(m.calc_setting("combo_rules").unwrap(), None);
    m.undo().unwrap();
    assert_eq!(m.calc_setting("combo_rules").unwrap(), Some(rules));
    m.undo().unwrap();
    assert_eq!(m.calc_setting("combo_rules").unwrap(), None);
    invalid(m.set_calc_setting("bad key", Some(&json!(1))), "key");

    let steel = MatMech {
        e: Some(210e9),
        nu: Some(0.3),
        fy: Some(355e6),
        alpha: Some(1.2e-5),
        ..MatMech::default()
    };
    m.set_material_mech("steel", &steel).unwrap();
    let back = m.material_mech("steel").unwrap();
    assert_eq!(back, steel);
    assert!((back.shear().unwrap() - 210e9 / 2.6).abs() < 1.0);
    m.undo().unwrap();
    assert_eq!(m.material_mech("steel").unwrap(), MatMech::default());
    let bad = MatMech {
        nu: Some(0.5),
        ..MatMech::default()
    };
    invalid(m.set_material_mech("steel", &bad), "Poisson");
    invalid(m.set_material_mech("unobtainium", &steel), "material");
}

#[test]
fn runs_live_outside_undo_and_the_content_hash() {
    let mut m = Model::open(":memory:").unwrap();
    m.add_load_case(&case("G")).unwrap();
    let hash = m.content_hash().unwrap();
    let parts: [(&str, &[u8]); 2] = [("input", b"in"), ("nodes", b"\x01\x02")];
    let id = m.add_calc_run(&run("abc"), &parts).unwrap();
    assert_eq!(m.content_hash().unwrap(), hash, "a run is not content");
    assert_eq!(m.calc_blob(id, "nodes").unwrap().as_deref(), Some(&b"\x01\x02"[..]));
    assert_eq!(m.calc_run_for("abc").unwrap(), Some(id));
    assert_eq!(m.calc_run_for("other").unwrap(), None);
    m.set_calc_run_status(id, "stale").unwrap();
    invalid(m.set_calc_run_status(id, "fine"), "status");
    let twice: [(&str, &[u8]); 2] = [("a", b""), ("a", b"")];
    invalid(m.add_calc_run(&run("abc"), &twice), "part");
    m.undo().unwrap();
    assert!(m.load_cases().unwrap().is_empty());
    let r = m.calc_run(id).unwrap().unwrap();
    assert_eq!((r.status.as_str(), r.stats.clone()), ("stale", json!({ "nodes": 4 })));
    assert!(m.check_integrity().unwrap().is_empty());
}

#[test]
fn retention_archives_old_unpinned_runs_and_never_deletes() {
    let mut mem = Model::open(":memory:").unwrap();
    for i in 0..7 {
        mem.add_calc_run(&run(&format!("h{i}")), &[]).unwrap();
    }
    assert!(mem.retain_calc_runs().unwrap().is_empty());
    assert_eq!(mem.calc_runs().unwrap().len(), 7, "a model in memory keeps every run");
    mem.add_load_case(&case("G")).unwrap();

    let dir = temp_dir("keep");
    let p = dir.join("p.ddc");
    let m = Model::open(p.to_str().unwrap()).unwrap();
    let mut pinned = run("pinned");
    pinned.pinned = true;
    let first = m.add_calc_run(&pinned, &[("input", &b"x"[..])]).unwrap();
    let mut ids = vec![];
    for i in 0..7u8 {
        let data = [i];
        ids.push(m.add_calc_run(&run(&format!("h{i}")), &[("input", &data[..])]).unwrap());
    }
    let files = m.retain_calc_runs().unwrap();
    assert_eq!(files.len(), 2);
    let arch = m.calc_archive_dir().unwrap();
    assert!(arch.ends_with(Path::new("p_archive").join("calc")), "{arch:?}");
    assert!(files.iter().all(|f| f.is_file() && f.starts_with(&arch)));
    let left: Vec<i64> = m.calc_runs().unwrap().iter().map(|r| r.id).collect();
    let mut want: Vec<i64> = ids[2..].iter().rev().copied().collect();
    want.push(first);
    assert_eq!(left, want, "the newest 5 unpinned and the pinned run stay");
    assert!(m.calc_blob(ids[0], "input").unwrap().is_none());
    for (f, id) in files.iter().zip(&ids[..2]) {
        let a = Connection::open(f).unwrap();
        let (n, data): (i64, Vec<u8>) = a
            .query_row(
                "SELECT (SELECT count(*) FROM calc_run WHERE id=?1), data FROM calc_blob \
                 WHERE run=?1 AND part='input'",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(data, vec![(id - ids[0]) as u8]);
    }
    assert!(m.retain_calc_runs().unwrap().is_empty(), "nothing more to move");
    m.set_calc_run_pinned(first, false).unwrap();
    assert_eq!(m.retain_calc_runs().unwrap().len(), 1, "an unpinned run joins the queue");
    drop(m);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_tables_stay_out_of_merge_and_intent_tables_join_it() {
    let m = Model::open(":memory:").unwrap();
    let s = crate::merge::tables::load(&m.db, &[]).unwrap();
    for t in ["calc_run", "calc_blob"] {
        assert!(is_run_table(t));
        assert!(s.table(t).is_none(), "{t}");
    }
    for t in ["an_override", "load_case", "load", "load_combo"] {
        assert!(!is_run_table(t));
        assert!(s.table(t).is_some(), "{t}");
    }
    let terms = s.children.iter().find(|c| c.spec.table == "load_combo_term").unwrap();
    assert!(terms.refs.len() >= 2, "combo owner and load case ids are mapped on merge");
    let load = s.table("load").unwrap();
    assert!(!load.refs.is_empty(), "load.lcase is mapped on merge");
}
