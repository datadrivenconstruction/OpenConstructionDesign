// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::collections::HashSet;

use rusqlite::params;
use rusqlite::types::Value as Sql;
use serde_json::{json, Value as J};

use super::*;
use crate::{schema, LayerSpec, Model, WallSpec};

const SRC: &str = "b|file";

fn model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m
        .add_wall_type(
            "T",
            &[LayerSpec {
                material: "m".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let w = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap();
    m.db.execute(
        "INSERT INTO ext_ref(source,guid,kind,ref) VALUES (?1,'g1','wall',?2)",
        params![SRC, w],
    )
    .unwrap();
    (m, w)
}

fn write() -> SourceWrite {
    SourceWrite {
        file_key: "file".into(),
        path: "C:/p/a.rvt".into(),
        format: "rvt".into(),
        reader: "b".into(),
        reader_version: "1".into(),
        at: 1_700_000_000,
        ..Default::default()
    }
}

/// One import pass: `rows` are (field, theirs) of wall g1, merged against the model now.
fn import(m: &mut Model, w: i64, rows: &[(&str, J)], policy: Policy, seen_g1: bool) -> MergeStats {
    let keys: Vec<String> = rows.iter().map(|r| r.0.to_string()).collect();
    m.batch_walls_edit("wall_test_import", |db, snaps| {
        let mut mg = Merge::new(SRC, policy);
        mg.begin(db, snaps)?;
        let mut seen = HashSet::new();
        if seen_g1 {
            seen.insert("g1".to_string());
            let now = current_fields(db, "wall", w, &keys)?.unwrap_or_default();
            let fields: Vec<(&str, J, J)> = rows
                .iter()
                .map(|(f, t)| (*f, now.get(*f).cloned().unwrap_or(J::Null), t.clone()))
                .collect();
            let dec = mg.row(db, snaps, "g1", "wall", w, &fields)?;
            for ((f, _), d) in rows.iter().zip(dec) {
                if d == Decision::Take && !f.starts_with("p:") {
                    db.execute(
                        &format!("UPDATE wall SET {f}=?2 WHERE id=?1"),
                        params![w, dec_or_null(&rows_value(rows, f))],
                    )?;
                }
            }
        }
        mg.finish(db, snaps, &seen, &write())?;
        Ok(mg.stats.clone())
    })
    .unwrap()
}

fn rows_value(rows: &[(&str, J)], f: &str) -> J {
    rows.iter()
        .find(|r| r.0 == f)
        .map(|r| r.1.clone())
        .unwrap_or(J::Null)
}

fn dec_or_null(j: &J) -> Sql {
    dec(j).unwrap_or(Sql::Null)
}

fn height(m: &Model, w: i64) -> f64 {
    m.db.query_row("SELECT height FROM wall WHERE id=?1", [w], |r| r.get(0))
        .unwrap()
}

fn set_height(m: &Model, w: i64, h: f64) {
    m.db.execute("UPDATE wall SET height=?2 WHERE id=?1", params![w, h])
        .unwrap();
}

#[test]
fn schema_has_source_tables() {
    let m = Model::open(":memory:").unwrap();
    assert_eq!(m.schema_version().unwrap(), schema::VERSION);
    for t in TABLES {
        let n: i64 =
            m.db.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [t],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{t}");
    }
    // idempotent
    migrate(&m.db).unwrap();
}

#[test]
fn decide_matrix() {
    let (a, b, c) = (json!(1.0), json!(2.0), json!(3.0));
    // unchanged both sides / same change both sides
    assert_eq!(decide(Some(&a), &a, &a), Decision::Same);
    assert_eq!(decide(Some(&a), &b, &b), Decision::Same);
    // only theirs changed → take; only mine changed → keep
    assert_eq!(decide(Some(&a), &a, &b), Decision::Take);
    assert_eq!(decide(Some(&a), &b, &a), Decision::Keep);
    // edit/edit
    assert_eq!(decide(Some(&a), &b, &c), Decision::Conflict);
    // no base (imported before tracking): source wins
    assert_eq!(decide(None, &b, &c), Decision::Take);
    // edit/delete of a parameter: theirs Null
    assert_eq!(decide(Some(&a), &b, &J::Null), Decision::Conflict);
    assert_eq!(decide(Some(&a), &a, &J::Null), Decision::Take);
    // add/add of a parameter: no base, equal values → same, different → source
    assert_eq!(decide(None, &a, &a), Decision::Same);
}

#[test]
fn values_encode_and_compare() {
    for v in [
        Sql::Null,
        Sql::Integer(-4),
        Sql::Real(2.5),
        Sql::Text("EI 60".into()),
    ] {
        assert_eq!(dec(&enc(&v)), Some(v.clone()), "{v:?}");
    }
    let blob = enc(&Sql::Blob(vec![1, 2, 3]));
    assert_eq!(dec(&blob), None);
    assert_eq!(blob, enc(&Sql::Blob(vec![1, 2, 3])));
    assert_ne!(blob, enc(&Sql::Blob(vec![1, 2, 4])));
    assert_eq!(show(&blob), "(geometry)");
    assert!(same_value(&json!(3.0), &json!(3.0 + 1e-12)));
    assert!(!same_value(&json!(3.0), &json!(3.001)));
    assert!(same_value(&json!(2), &json!(2)));
    assert!(!same_value(&json!("a"), &json!("b")));
}

#[test]
fn rings_round_trip() {
    let r = vec![
        vec![(0.0, 0.0), (4.5, 0.0), (4.5, 3.25)],
        vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.0)],
    ];
    assert_eq!(parse_rings(&rings_text(&r)), Some(r));
    assert_eq!(parse_rings("1,2 x"), None);
}

#[test]
fn conflict_case_keys_round_trip() {
    for c in [
        ConflictCase::EditEdit,
        ConflictCase::EditDelete,
        ConflictCase::DeleteEdit,
    ] {
        assert_eq!(ConflictCase::parse(c.key()), Some(c));
    }
    assert_eq!(ConflictCase::parse("x"), None);
}

#[test]
fn first_import_records_base_and_provenance() {
    let (mut m, w) = model();
    let s = import(
        &mut m,
        w,
        &[("height", json!(3.0))],
        Policy::default(),
        true,
    );
    assert_eq!(s.conflicts, 0);
    let info = m.source_info(SRC).unwrap().unwrap();
    assert_eq!(
        (info.path.as_str(), info.format.as_str(), info.imports),
        ("C:/p/a.rvt", "rvt", 1)
    );
    assert_eq!(info.imported_at, 1_700_000_000);
    let p = m.provenance("wall", w).unwrap().unwrap();
    assert_eq!(p.guid, "g1");
    assert!(p.tracked && !p.edited);
    assert!(!m.is_edited("wall", w).unwrap());
    set_height(&m, w, 3.5);
    assert!(m.is_edited("wall", w).unwrap());
    let d = m.element_diff("wall", w).unwrap();
    assert_eq!(d.len(), 1);
    assert_eq!(
        (d[0].field.as_str(), &d[0].base, &d[0].mine),
        ("height", &json!(3.0), &json!(3.5))
    );
    let ch = m.local_changes(SRC, false).unwrap();
    assert_eq!(ch.len(), 1);
    assert_eq!(ch[0].change, ChangeKind::Edited);
    assert_eq!(
        m.element_by_guid(SRC, "g1").unwrap(),
        Some(("wall".into(), w))
    );
}

/// A project parameter value set on an imported element after the import is a local edit:
/// the save to the source file sees it as a `UPARAM_FIELD` field.
#[test]
fn project_param_value_after_import_is_a_local_edit() {
    use crate::{ParamSet, ParamSpec, ParamType, PVal};
    let (mut m, w) = model();
    import(&mut m, w, &[("height", json!(3.0))], Policy::default(), true);
    assert!(!m.is_edited("wall", w).unwrap());
    let p = m
        .add_param(&ParamSpec {
            name: "Mark".into(),
            ptype: ParamType::Text,
            per_type: false,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: vec!["wall".into()],
        })
        .unwrap();
    assert!(!m.is_edited("wall", w).unwrap(), "a parameter without a value edits nothing");
    m.set_param_values(&[ParamSet { param: p, kind: "wall".into(), id: w, value: Some(PVal::Text("W-01".into())) }])
        .unwrap();
    let d = m.element_diff("wall", w).unwrap();
    let f = format!("{UPARAM_FIELD}Mark");
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!((d[0].field.as_str(), &d[0].base, &d[0].mine), (f.as_str(), &J::Null, &json!("W-01")));
    let ch = m.local_changes(SRC, false).unwrap();
    assert_eq!(ch.len(), 1);
    assert!(ch[0].fields.iter().any(|x| x.field == f), "{:?}", ch[0].fields);
    m.set_param_values(&[ParamSet { param: p, kind: "wall".into(), id: w, value: None }]).unwrap();
    assert!(!m.is_edited("wall", w).unwrap(), "the value removed again: no edit left");
}

#[test]
fn element_by_guid_finds_tableless_spatial_links() {
    let (m, w) = model();
    for (g, k) in [("gp", "project"), ("gs", "site"), ("gb", "building")] {
        m.db.execute(
            "INSERT INTO ext_ref(source,guid,kind,ref) VALUES (?1,?2,?3,0)",
            params![SRC, g, k],
        )
        .unwrap();
        assert_eq!(m.element_by_guid(SRC, g).unwrap(), Some((k.into(), 0)));
    }
    assert_eq!(m.element_by_guid(SRC, "g1").unwrap(), Some(("wall".into(), w)));
    assert_eq!(m.element_by_guid(SRC, "nope").unwrap(), None);
}

#[test]
fn reimport_takes_upstream_keeps_local_and_lists_conflicts() {
    let (mut m, w) = model();
    import(
        &mut m,
        w,
        &[("height", json!(3.0)), ("base_offset", json!(0.0))],
        Policy::default(),
        true,
    );
    // only upstream changed → taken
    let s = import(
        &mut m,
        w,
        &[("height", json!(4.0)), ("base_offset", json!(0.0))],
        Policy::default(),
        true,
    );
    assert_eq!((s.taken, s.merged, s.conflicts), (1, 1, 0));
    assert_eq!(height(&m, w), 4.0);
    // only local changed → kept
    set_height(&m, w, 4.5);
    let s = import(
        &mut m,
        w,
        &[("height", json!(4.0)), ("base_offset", json!(0.0))],
        Policy::default(),
        true,
    );
    assert_eq!((s.taken, s.kept_local, s.conflicts), (0, 1, 0));
    assert_eq!(height(&m, w), 4.5);
    // both changed differently → conflict, local value stays until resolved
    let s = import(
        &mut m,
        w,
        &[("height", json!(5.0)), ("base_offset", json!(0.0))],
        Policy::default(),
        true,
    );
    assert_eq!(s.conflicts, 1);
    assert_eq!(height(&m, w), 4.5);
    let c = m.conflicts(Some(SRC)).unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(
        (c[0].case, c[0].field.as_str()),
        (ConflictCase::EditEdit, "height")
    );
    assert_eq!(
        (&c[0].base, &c[0].mine, &c[0].theirs),
        (&json!(4.0), &json!(4.5), &json!(5.0))
    );
    // the same conflict is not listed twice by the next import
    import(
        &mut m,
        w,
        &[("height", json!(5.0)), ("base_offset", json!(0.0))],
        Policy::default(),
        true,
    );
    assert_eq!(m.conflicts(Some(SRC)).unwrap().len(), 1);

    // take theirs, one undo step back
    let id = c[0].id;
    let r = m.resolve_conflicts(&[(id, Pick::Theirs)]).unwrap();
    assert_eq!(r.took_theirs, 1);
    assert_eq!(height(&m, w), 5.0);
    assert!(m.conflicts(None).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(height(&m, w), 4.5);
    assert_eq!(m.conflicts(None).unwrap().len(), 1);
    // keep mine
    let r = m.resolve_conflicts(&[(id, Pick::Mine)]).unwrap();
    assert_eq!(r.kept_mine, 1);
    assert_eq!(height(&m, w), 4.5);
    assert!(m.conflicts(None).unwrap().is_empty());
}

#[test]
fn deleted_upstream_unedited_is_removed_edited_becomes_conflict() {
    let (mut m, w) = model();
    import(
        &mut m,
        w,
        &[("height", json!(3.0))],
        Policy::default(),
        true,
    );
    // default policy: nothing deleted
    let s = import(&mut m, w, &[], Policy::default(), false);
    assert_eq!(s.deleted_upstream, 0);
    assert!(m.provenance("wall", w).unwrap().is_some());
    let del = Policy {
        delete_removed: true,
        ..Default::default()
    };
    // edited here → edit/delete conflict, element kept
    set_height(&m, w, 3.5);
    let s = import(&mut m, w, &[], del, false);
    assert_eq!((s.deleted_upstream, s.conflicts), (0, 1));
    let c = m.conflicts(None).unwrap();
    assert_eq!(
        (c[0].case, c[0].field.as_str()),
        (ConflictCase::EditDelete, "*")
    );
    assert_eq!(height(&m, w), 3.5);
    // take theirs → element deleted
    let r = m.resolve_conflicts(&[(c[0].id, Pick::Theirs)]).unwrap();
    assert_eq!(r.deleted, 1);
    let n: i64 =
        m.db.query_row("SELECT count(*) FROM wall WHERE id=?1", [w], |r| r.get(0))
            .unwrap();
    assert_eq!(n, 0);
    m.undo().unwrap();
    assert_eq!(height(&m, w), 3.5);
}

#[test]
fn deleted_upstream_unedited_is_deleted_with_undo() {
    let (mut m, w) = model();
    import(
        &mut m,
        w,
        &[("height", json!(3.0))],
        Policy::default(),
        true,
    );
    let s = import(
        &mut m,
        w,
        &[],
        Policy {
            delete_removed: true,
            ..Default::default()
        },
        false,
    );
    assert_eq!(s.deleted_upstream, 1);
    let n = |m: &Model| -> i64 {
        m.db.query_row("SELECT count(*) FROM wall WHERE id=?1", [w], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(n(&m), 0);
    m.undo().unwrap();
    assert_eq!(n(&m), 1);
    assert!(m.provenance("wall", w).unwrap().unwrap().tracked);
}

#[test]
fn set_source_prop_is_one_undo_step_and_marks_edited() {
    let (mut m, w) = model();
    import(
        &mut m,
        w,
        &[("height", json!(3.0)), ("p:Fire", json!("EI 60"))],
        Policy::default(),
        true,
    );
    m.db.execute(
        "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('wall',?1,'Fire','EI 60','')",
        [w],
    )
    .unwrap();
    assert!(!m.is_edited("wall", w).unwrap());
    m.set_source_prop("wall", w, "Fire", Sql::Text("EI 90".into()), "")
        .unwrap();
    let ps = m.source_params("wall", w).unwrap();
    let p = ps.iter().find(|p| p.key == "Fire").unwrap();
    // the base keeps only a hash of an unedited parameter: its old value is not shown
    assert_eq!(
        (&p.value, p.edited, p.base.as_ref()),
        (&json!("EI 90"), true, Some(&J::Null))
    );
    assert!(m.is_edited("wall", w).unwrap());
    m.undo().unwrap();
    assert!(!m.is_edited("wall", w).unwrap());
    // a parameter added here shows as a change too
    m.set_source_prop("wall", w, "Note", Sql::Text("x".into()), "")
        .unwrap();
    assert!(m
        .element_diff("wall", w)
        .unwrap()
        .iter()
        .any(|d| d.field == "p:Note"));
    // Null deletes
    m.set_source_prop("wall", w, "Note", Sql::Null, "").unwrap();
    assert!(!m
        .source_params("wall", w)
        .unwrap()
        .iter()
        .any(|p| p.key == "Note"));
}

#[test]
fn detach_keeps_element_and_forgets_source() {
    let (mut m, w) = model();
    import(
        &mut m,
        w,
        &[("height", json!(3.0))],
        Policy::default(),
        true,
    );
    m.detach_from_source("wall", w).unwrap();
    assert!(m.provenance("wall", w).unwrap().is_none());
    assert_eq!(height(&m, w), 3.0);
    m.undo().unwrap();
    assert!(m.provenance("wall", w).unwrap().is_some());
}

#[test]
fn source_ids_prefer_reported_global_id() {
    let (mut m, w) = model();
    m.batch_walls_edit("wall_test_import", |db, snaps| {
        let mut mg = Merge::new(SRC, Policy::default());
        mg.added("g1", "wall", w, &[("height", json!(3.0))]);
        mg.ifc_guid("g1", "0YvctVUKr0kugbFTf53O9L");
        let mut seen = HashSet::new();
        seen.insert("g1".to_string());
        mg.finish(db, snaps, &seen, &write())?;
        Ok(())
    })
    .unwrap();
    let ids = m.source_ids("wall").unwrap();
    assert_eq!(
        ids[&w],
        ("g1".to_string(), Some("0YvctVUKr0kugbFTf53O9L".to_string()))
    );
    let p = m.provenance("wall", w).unwrap().unwrap();
    assert_eq!(p.ifc_guid.as_deref(), Some("0YvctVUKr0kugbFTf53O9L"));
}

fn base_offset(m: &Model, w: i64) -> f64 {
    m.db.query_row("SELECT base_offset FROM wall WHERE id=?1", [w], |r| {
        r.get(0)
    })
    .unwrap()
}

#[test]
fn undo_of_several_resolved_conflicts_on_one_row_restores_all_fields() {
    let (mut m, w) = model();
    import(
        &mut m,
        w,
        &[("height", json!(3.0)), ("base_offset", json!(0.0))],
        Policy::default(),
        true,
    );
    m.db.execute(
        "UPDATE wall SET height=3.5, base_offset=0.1 WHERE id=?1",
        [w],
    )
    .unwrap();
    let s = import(
        &mut m,
        w,
        &[("height", json!(4.0)), ("base_offset", json!(0.2))],
        Policy::default(),
        true,
    );
    assert_eq!(s.conflicts, 2);
    let picks: Vec<(i64, Pick)> = m
        .conflicts(None)
        .unwrap()
        .iter()
        .map(|c| (c.id, Pick::Theirs))
        .collect();
    m.resolve_conflicts(&picks).unwrap();
    assert_eq!((height(&m, w), base_offset(&m, w)), (4.0, 0.2));
    m.undo().unwrap();
    assert_eq!((height(&m, w), base_offset(&m, w)), (3.5, 0.1));
    assert_eq!(m.conflicts(None).unwrap().len(), 2);
}

#[test]
fn cascade_delete_keeps_props_of_another_kind_with_the_same_id() {
    let (mut m, w) = model();
    m.db.execute(
        "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('slab',?1,'X','old','')",
        [w],
    )
    .unwrap();
    m.batch_walls_edit("wall_test_import", |db, snaps| {
        // the import first updates slab `w`'s parameter (snapshot first), then drops wall `w`
        snaps.save(db, "prop", "ref", w)?;
        db.execute(
            "UPDATE prop SET value='new' WHERE kind='slab' AND ref=?1",
            [w],
        )?;
        super::resolve::drop_element(db, snaps, "wall", w)?;
        Ok(())
    })
    .unwrap();
    m.undo().unwrap();
    let v: String =
        m.db.query_row(
            "SELECT value FROM prop WHERE kind='slab' AND ref=?1 AND key='X'",
            [w],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v, "old");
    assert_eq!(height(&m, w), 3.0);
}

#[test]
fn parameter_added_on_both_sides_is_a_conflict() {
    let (mut m, w) = model();
    import(
        &mut m,
        w,
        &[("height", json!(3.0)), ("p:Other", json!("a"))],
        Policy::default(),
        true,
    );
    m.db.execute(
        "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('wall',?1,'Fire','EI 90','')",
        [w],
    )
    .unwrap();
    let rows = [
        ("height", json!(3.0)),
        ("p:Other", json!("a")),
        ("p:Fire", json!("EI 60")),
    ];
    let s = import(&mut m, w, &rows, Policy::default(), true);
    assert_eq!((s.conflicts, s.taken), (1, 0));
    let c = m.conflicts(None).unwrap();
    assert_eq!(
        (c[0].field.as_str(), &c[0].mine, &c[0].theirs),
        ("p:Fire", &json!("EI 90"), &json!("EI 60"))
    );
}

#[test]
fn base_is_compact_and_reimport_undo_record_is_small() {
    let (mut m, w) = model();
    let rows: Vec<(String, J)> = (0..50)
        .map(|i| (format!("p:P{i}"), json!(format!("value {i}"))))
        .collect();
    let mut first: Vec<(&str, J)> = vec![("height", json!(3.0))];
    first.extend(rows.iter().map(|(k, v)| (k.as_str(), v.clone())));
    import(&mut m, w, &first, Policy::default(), true);
    for i in 0..50 {
        m.db.execute(
            "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('wall',?1,?2,?3,'')",
            params![w, format!("P{i}"), format!("value {i}")],
        )
        .unwrap();
    }
    let (hashes, vals): (Vec<u8>, String) =
        m.db.query_row(
            "SELECT hashes, vals FROM src_base WHERE guid='g1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    // parameters as 8-byte hashes, no copy of their values
    assert_eq!(hashes.len(), 50 * 8);
    assert!(!vals.contains("p:") && vals.contains("height"));
    // upstream changes every parameter: the undo record does not copy the base
    let mut second: Vec<(&str, J)> = vec![("height", json!(3.0))];
    let changed: Vec<(String, J)> = (0..50)
        .map(|i| (format!("p:P{i}"), json!(format!("new {i}"))))
        .collect();
    second.extend(changed.iter().map(|(k, v)| (k.as_str(), v.clone())));
    import(&mut m, w, &second, Policy::default(), true);
    let inv: String =
        m.db.query_row("SELECT inv FROM op ORDER BY seq DESC LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    // the previous base is copied by SQL, not written into the undo record
    assert!(!inv.contains("value 1") && !inv.contains("new 1"), "{inv}");
    let now: Vec<u8> =
        m.db.query_row("SELECT hashes FROM src_base WHERE guid='g1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_ne!(now, hashes);
    m.undo().unwrap();
    let back: Vec<u8> =
        m.db.query_row("SELECT hashes FROM src_base WHERE guid='g1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(back, hashes);
}

#[test]
fn mass_unmatched_reimport_is_refused_unless_allowed() {
    let mut m = Model::open(":memory:").unwrap();
    let run = |m: &mut Model, n: usize, policy: Policy| {
        m.batch_walls_edit("wall_test_import", |db, snaps| {
            let mut mg = Merge::new(SRC, policy);
            mg.begin(db, snaps)?;
            let mut seen = HashSet::new();
            for i in 0..n {
                let g = format!("g{i}");
                mg.added(&g, "wall", 1000 + i as i64, &[("height", json!(3.0))]);
                seen.insert(g);
            }
            mg.finish(db, snaps, &seen, &write())?;
            Ok(())
        })
    };
    run(&mut m, 30, Policy::default()).unwrap();
    let e = run(&mut m, 5, Policy::default()).unwrap_err();
    assert!(e.to_string().contains(MASS_CHANGE), "{e}");
    let n: i64 =
        m.db.query_row("SELECT count(*) FROM src_base", [], |r| r.get(0))
            .unwrap();
    assert_eq!(n, 30);
    run(
        &mut m,
        5,
        Policy {
            allow_mass_change: true,
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn hosted_local_component_turns_upstream_delete_into_a_conflict() {
    let (mut m, w) = model();
    import(
        &mut m,
        w,
        &[("height", json!(3.0))],
        Policy::default(),
        true,
    );
    m.db.execute(
        "INSERT INTO comp(level,ctype,x,y,z,rotation,mirror,host_wall,vals) VALUES (1,1,1.0,0.0,0.0,0.0,0,?1,'{}')",
        [w],
    )
    .unwrap();
    let del = Policy {
        delete_removed: true,
        ..Default::default()
    };
    let s = import(&mut m, w, &[], del, false);
    assert_eq!((s.deleted_upstream, s.conflicts), (0, 1));
    assert_eq!(height(&m, w), 3.0);
}

#[test]
fn geometry_conflict_without_body_is_deferred_to_next_import() {
    let (mut m, w) = model();
    import(
        &mut m,
        w,
        &[("height", json!(3.0))],
        Policy::default(),
        true,
    );
    set_height(&m, w, 3.5);
    m.db.execute(
        "INSERT INTO src_conflict(source,guid,kind,ref,field,case_kind,base,mine,theirs) \
         VALUES (?1,'g1','wall',?2,'height','edit_edit','3.0','3.5',?3)",
        params![SRC, w, J::String("\u{0}blob:8:00".into()).to_string()],
    )
    .unwrap();
    let id = m.conflicts(None).unwrap()[0].id;
    let r = m.resolve_conflicts(&[(id, Pick::Theirs)]).unwrap();
    assert_eq!(r.deferred, 1);
    // the base now equals the local value, so the next import takes the file's
    let s = import(
        &mut m,
        w,
        &[("height", json!(4.0))],
        Policy::default(),
        true,
    );
    assert_eq!((s.taken, s.conflicts), (1, 0));
    assert_eq!(height(&m, w), 4.0);
}

#[test]
fn geometry_body_of_a_conflict_is_applied_by_take_theirs() {
    let mut m = Model::open(":memory:").unwrap();
    m.db.execute(
        "INSERT INTO mesh(level,category,name,positions,indices) VALUES (NULL,'generic','m',x'0102',x'00')",
        [],
    )
    .unwrap();
    let id = m.db.last_insert_rowid();
    m.db.execute(
        "INSERT INTO src_conflict(source,guid,kind,ref,field,case_kind,base,mine,theirs) \
         VALUES (?1,'g9','mesh',?2,'positions','edit_edit','null','null',?3)",
        params![SRC, id, blob_json(&[9, 8, 7]).to_string()],
    )
    .unwrap();
    let cid = m.conflicts(None).unwrap()[0].id;
    m.resolve_conflicts(&[(cid, Pick::Theirs)]).unwrap();
    let p: Vec<u8> =
        m.db.query_row("SELECT positions FROM mesh WHERE id=?1", [id], |r| r.get(0))
            .unwrap();
    assert_eq!(p, vec![9, 8, 7]);
    m.undo().unwrap();
    let p: Vec<u8> =
        m.db.query_row("SELECT positions FROM mesh WHERE id=?1", [id], |r| r.get(0))
            .unwrap();
    assert_eq!(p, vec![1, 2]);
}

#[test]
fn deleted_here_changed_there_lists_changed_fields() {
    let (mut m, w) = model();
    import(
        &mut m,
        w,
        &[("height", json!(3.0)), ("base_offset", json!(0.0))],
        Policy::default(),
        true,
    );
    m.db.execute("DELETE FROM wall WHERE id=?1", [w]).unwrap();
    m.batch_walls_edit("wall_test_import", |db, snaps| {
        let mut mg = Merge::new(SRC, Policy::default());
        let d = mg.dead(
            db,
            snaps,
            "g1",
            "wall",
            w,
            &[("height", json!(4.0)), ("base_offset", json!(0.0))],
        )?;
        assert_eq!(d, Dead::Stay);
        let mut seen = HashSet::new();
        seen.insert("g1".to_string());
        mg.finish(db, snaps, &seen, &write())?;
        Ok(())
    })
    .unwrap();
    let c = m.conflicts(None).unwrap();
    assert_eq!(
        (c[0].case, &c[0].theirs),
        (ConflictCase::DeleteEdit, &json!("height"))
    );
}

#[test]
fn a_pinned_element_is_skipped_reported_and_retained() {
    let (mut m, w) = model();
    import(
        &mut m,
        w,
        &[("height", json!(3.0))],
        Policy::default(),
        true,
    );
    m.pin(&[("wall", w)]).unwrap();
    let s = import(
        &mut m,
        w,
        &[("height", json!(4.0))],
        Policy::default(),
        true,
    );
    assert_eq!(height(&m, w), 3.0);
    assert_eq!(s.taken, 0);
    assert_eq!(s.pinned, 1);
    assert_eq!(s.pinned_ids, vec![("wall".to_string(), w)]);
    assert!(s.pairs().contains(&("pinned", 1)));

    let del = Policy {
        delete_removed: true,
        ..Default::default()
    };
    let s = import(&mut m, w, &[], del, false);
    assert_eq!((s.deleted_upstream, s.pinned), (0, 1));
    let n: i64 =
        m.db.query_row("SELECT count(*) FROM wall WHERE id=?1", [w], |r| r.get(0))
            .unwrap();
    assert_eq!(n, 1);

    m.unpin(&[("wall", w)]).unwrap();
    let s = import(&mut m, w, &[], del, false);
    assert_eq!(s.pinned, 0);
}

#[test]
fn value_hash_agrees_with_tolerant_equality() {
    assert_eq!(vhash(&json!(3)), vhash(&json!(3.0)));
    assert_eq!(vhash(&json!(1e-12)), vhash(&json!(0)));
    assert_ne!(vhash(&json!("3")), vhash(&json!(3)));
    assert_ne!(vhash(&json!(3.0)), vhash(&json!(3.001)));
}

#[test]
fn set_source_props_is_one_step_and_all_or_nothing() {
    let (mut m, w) = model();
    let ops = |m: &Model| -> i64 {
        m.db.query_row("SELECT count(*) FROM op", [], |r| r.get(0))
            .unwrap()
    };
    let (level, wall_type): (i64, i64) = m
        .db
        .query_row("SELECT level, wtype FROM wall WHERE id=?1", [w], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    let w2 = m
        .add_wall(&WallSpec {
            a: (0.0, 2.0),
            b: (4.0, 2.0),
            level,
            base_offset: 0.0,
            height: 3.0,
            wall_type,
            offset0: None,
        })
        .unwrap();
    let before = ops(&m);
    m.set_source_props(&[("wall", w), ("wall", w2)], "Own.Fire", Sql::Text("EI 90".into()), "")
        .unwrap();
    assert_eq!(ops(&m), before + 1, "one journal entry");
    let fire = |m: &Model, id: i64| m.props("wall", id).unwrap().into_iter().find(|p| p.0 == "Own.Fire");
    assert!(fire(&m, w).is_some() && fire(&m, w2).is_some());
    m.undo().unwrap();
    assert!(fire(&m, w).is_none() && fire(&m, w2).is_none());
    // a missing element or a non-finite number writes nothing
    let before = ops(&m);
    assert!(m
        .set_source_props(&[("wall", w), ("wall", 9999)], "Own.Fire", Sql::Text("x".into()), "")
        .is_err());
    assert!(m
        .set_source_props(&[("wall", w)], "Own.Load", Sql::Real(f64::INFINITY), "kg")
        .is_err());
    assert_eq!(ops(&m), before);
    assert!(fire(&m, w).is_none());
}

fn add_roof(m: &Model, verts: &[(f64, f64)]) -> i64 {
    m.db.execute(
        "INSERT INTO roof(level,stype,kind,offset,overhang,fascia) VALUES (1,1,'footprint',0,0,0)",
        [],
    )
    .unwrap();
    let id = m.db.last_insert_rowid();
    for (i, (x, y)) in verts.iter().enumerate() {
        m.db.execute(
            "INSERT INTO roof_vertex(roof,idx,x,y) VALUES (?1,?2,?3,?4)",
            params![id, i as i64, x, y],
        )
        .unwrap();
    }
    id
}

fn kids(m: &Model, id: i64) -> String {
    kids_text(&m.db, "roof", id).unwrap().unwrap()
}

/// One merge pass of roof `id` (guid gr) with the file's `kids` value.
fn roof_pass(m: &mut Model, id: i64, theirs: &str) -> (Decision, MergeStats) {
    m.batch_walls_edit("roof_test_import", |db, snaps| {
        let mut mg = Merge::new(SRC, Policy::default());
        mg.begin(db, snaps)?;
        let now = J::String(kids_text(db, "roof", id)?.unwrap());
        let fields = [("kids", now, J::String(theirs.into()))];
        let d = mg.row(db, snaps, "gr", "roof", id, &fields)?[0];
        let seen: HashSet<String> = ["gr".to_string()].into();
        mg.finish(db, snaps, &seen, &write())?;
        Ok((d, mg.stats.clone()))
    })
    .unwrap()
}

#[test]
fn kids_hash_ignores_parent_id() {
    let (m, _) = model();
    let v = [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0)];
    let a = add_roof(&m, &v);
    let b = add_roof(&m, &v);
    assert_ne!(a, b);
    assert_eq!(kids(&m, a), kids(&m, b));
    let c = add_roof(&m, &[(0.0, 0.0), (4.0, 0.0), (4.0, 3.5)]);
    assert_ne!(kids(&m, a), kids(&m, c));
    assert_eq!(m.kids_count("roof", a), Some(3));
    assert_eq!(m.kids_count("wall", a), None);
}

#[test]
fn recreated_element_takes_file_children_no_conflict() {
    let (mut m, _) = model();
    let v = [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0)];
    let a = add_roof(&m, &v);
    m.db.execute(
        "INSERT INTO ext_ref(source,guid,kind,ref) VALUES (?1,'gr','roof',?2)",
        params![SRC, a],
    )
    .unwrap();
    let first = kids(&m, a);
    roof_pass(&mut m, a, &first);
    // the element is recreated with a new id and identical children
    m.db.execute("DELETE FROM roof_vertex WHERE roof=?1", [a]).unwrap();
    m.db.execute("DELETE FROM roof WHERE id=?1", [a]).unwrap();
    let b = add_roof(&m, &v);
    m.db.execute("UPDATE ext_ref SET ref=?1 WHERE guid='gr'", [b]).unwrap();
    // the file changed the children: taken, not a conflict
    let (d, s) = roof_pass(&mut m, b, "ffffffffffffffff");
    assert_eq!(d, Decision::Take);
    assert_eq!(s.conflicts, 0);
    assert!(m.conflicts(Some(SRC)).unwrap().is_empty());
}

/// Three walls g1..g3 (the first from `model()`), each with pending links.
fn three_pending() -> Model {
    let (mut m, a) = model();
    let (lv, t): (i64, i64) = (
        m.db.query_row("SELECT level FROM wall WHERE id=?1", [a], |r| r.get(0)).unwrap(),
        m.db.query_row("SELECT wtype FROM wall WHERE id=?1", [a], |r| r.get(0)).unwrap(),
    );
    let mut ids = vec![a];
    for k in 1..3 {
        ids.push(
            m.add_wall(&WallSpec {
                a: (0.0, k as f64),
                b: (4.0, k as f64),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap(),
        );
    }
    for (i, w) in ids.iter().enumerate() {
        let g = format!("g{}", i + 1);
        m.db.execute("DELETE FROM ext_ref WHERE guid=?1", [&g]).unwrap();
        m.db.execute(
            "INSERT INTO ext_ref(source,guid,kind,ref) VALUES (?1,?2,'wall',?3)",
            params![SRC, g, w],
        )
        .unwrap();
        m.db.execute(
            "INSERT OR REPLACE INTO src_base(source,guid,kind,vals) VALUES (?1,?2,'wall',?3)",
            params![SRC, g, r#"{"~link":["mat","type"]}"#],
        )
        .unwrap();
    }
    m
}

#[test]
fn drop_one_pending_link_keeps_the_others() {
    let mut m = three_pending();
    assert_eq!(m.pending_links(SRC).unwrap().len(), 3);
    assert_eq!(m.drop_pending_link(SRC, "g2", None).unwrap(), 2);
    let left: Vec<String> = m.pending_links(SRC).unwrap().into_iter().map(|l| l.guid).collect();
    assert_eq!(left, vec!["g1".to_string(), "g3".to_string()]);
    // dropped again, or unknown: nothing, no journal step
    assert_eq!(m.drop_pending_link(SRC, "g2", None).unwrap(), 0);
    assert_eq!(m.drop_pending_link(SRC, "nope", None).unwrap(), 0);
    m.undo().unwrap();
    assert_eq!(m.pending_links(SRC).unwrap().len(), 3);
}

#[test]
fn drop_one_relation_of_one_element() {
    let mut m = three_pending();
    assert_eq!(m.drop_pending_link(SRC, "g1", Some("mat")).unwrap(), 1);
    let list = m.pending_links(SRC).unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].rels, vec!["type".to_string()]);
    assert_eq!(list[1].rels.len(), 2);
}

/// Taking theirs on a locked parameter value is refused, reported, and leaves the conflict open.
#[test]
fn resolve_theirs_refuses_locked_parameter_value() {
    use crate::{Edit, PVal, ParamSpec, ParamType};
    let (mut m, w) = model();
    m.add_param(&ParamSpec {
        name: "Fire".into(),
        ptype: ParamType::Text,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    })
    .unwrap();
    let set = |m: &mut Model, v: &str| {
        m.apply_edits(&[Edit::ParamValue {
            param: "Fire".into(),
            kind: "wall".into(),
            id: w,
            value: Some(PVal::Text(v.into())),
        }])
        .unwrap();
    };
    let field = format!("{}Fire", UPARAM_FIELD);
    set(&mut m, "A");
    import(&mut m, w, &[(field.as_str(), json!("A"))], Policy::default(), true);
    set(&mut m, "B");
    let s = import(&mut m, w, &[(field.as_str(), json!("C"))], Policy::default(), true);
    assert_eq!(s.conflicts, 1);
    let id = m.conflicts(Some(SRC)).unwrap()[0].id;
    m.set_param_lock(&[("wall", w, "Fire")]).unwrap();
    let r = m.resolve_conflicts(&[(id, Pick::Theirs)]).unwrap();
    assert_eq!(r.took_theirs, 0);
    assert_eq!(r.refused, vec![("wall".to_string(), w, field.clone())]);
    assert_eq!(m.conflicts(Some(SRC)).unwrap().len(), 1, "conflict stays open");
    let slot = uparam_slot(&m.db, "wall", w, "Fire").unwrap().unwrap();
    assert_eq!(uparam_value(&m.db, &slot).unwrap(), json!("B"));
    // after the unlock the same choice applies
    m.clear_param_lock(&[("wall", w, "Fire")]).unwrap();
    let r = m.resolve_conflicts(&[(id, Pick::Theirs)]).unwrap();
    assert_eq!((r.took_theirs, r.refused.len()), (1, 0));
    assert_eq!(uparam_value(&m.db, &slot).unwrap(), json!("C"));
}
