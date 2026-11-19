// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Scenarios over every showcase project (tests/showcase): each passes the model check, builds
//! the same twice, saves and reopens to the same content and the same bytes, and undoes back to
//! a fresh model. The XLSX and editor scenarios live in ddc-io and ddc-app.

mod showcase;

use ddc_model::Model;
use showcase::{
    build, build_raw, count, dump, each, fresh, huge_level_n, remove_db, same, temp_file,
    undo_all, valid, Keep, PROJECTS,
};

/// Saves `m`, reopens the file and checks content, model check and the bytes of a second save.
fn save_reopen(m: &Model, name: &str) -> Result<(), String> {
    let f1 = temp_file(&format!("{name}_1.ddc"));
    let f2 = temp_file(&format!("{name}_2.ddc"));
    m.snapshot_to(&f1).map_err(|e| format!("save: {e}"))?;
    // the saved bytes, before opening the file can touch its header
    let a = std::fs::read(&f1).unwrap();
    let r = (|| {
        let back = Model::open(f1.to_str().unwrap()).map_err(|e| format!("reopen: {e}"))?;
        same(&dump(&back, Keep::All), &dump(m, Keep::All), "reopened")?;
        valid(&back, "reopened")?;
        let cookie = |b: &[u8]| i64::from(u32::from_be_bytes([b[40], b[41], b[42], b[43]]));
        let now: i64 = back.sql().query_row("PRAGMA schema_version", [], |r| r.get(0)).unwrap();
        if now != cookie(&a) {
            return Err(format!("opening changed the schema: cookie {} -> {now}", cookie(&a)));
        }
        back.snapshot_to(&f2).map_err(|e| format!("save again: {e}"))?;
        drop(back);
        let mut b = std::fs::read(&f2).unwrap();
        // every save (VACUUM INTO) writes its source's schema cookie + 1, so a reopened file
        // saves one higher; the check above already proved the open changed no schema
        if b.len() >= 44 && a.len() >= 44 {
            b[40..44].copy_from_slice(&a[40..44]);
        }
        if a != b {
            let at = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
            return Err(format!("saves differ: {} vs {} bytes, first at {at}", a.len(), b.len()));
        }
        Ok(())
    })();
    remove_db(&f1);
    remove_db(&f2);
    r
}

#[test]
fn every_project_passes_the_model_check() {
    assert!(PROJECTS.len() >= 8);
    each(|p, m| valid(&m, p.name));
}

#[test]
fn every_project_builds_the_same_twice() {
    each(|p, m| same(&dump(&build(p.name), Keep::All), &dump(&m, Keep::All), "second build"));
}

#[test]
fn save_and_reopen_give_identical_content_and_bytes() {
    each(|p, m| save_reopen(&m, p.name));
}

#[test]
fn undo_of_everything_restores_the_start() {
    let start = dump(&fresh(), Keep::User);
    each(|p, mut m| {
        let steps = undo_all(&mut m)?;
        if p.name != "edge_empty" && steps == 0 {
            return Err("nothing to undo".into());
        }
        same(&dump(&m, Keep::User), &start, "after undo of everything")?;
        valid(&m, "after undo")
    });
}

#[test]
fn projects_hold_what_they_promise() {
    let office = build("office");
    assert_eq!(count(&office, "level"), 3);
    for t in [
        "grid", "stair", "railing", "roof", "sheet", "viewport", "sheet_schedule", "comp",
        "mep_run", "mep_system", "dim",
    ] {
        let n = if showcase::has_table(&office, t) { count(&office, t) } else { 0 };
        assert!(n > 0, "office has no {t}");
    }
    assert!(count(&office, "room") >= 27);
    assert!(count(&office, "wall_type") >= 4);
    assert!(count(&office, "sheet") == 2);
    let res = build("residential");
    assert_eq!(count(&res, "level"), 2);
    assert!(count(&res, "curtain_wall") >= 2);
    let cyr: i64 = res
        .sql()
        .query_row("SELECT count(*) FROM room WHERE name GLOB '*[\u{430}-\u{44f}]*'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(cyr >= 8, "Cyrillic room names: {cyr}");
    let longest: i64 = res
        .sql()
        .query_row("SELECT max(length(name)) FROM room", [], |r| r.get(0))
        .unwrap();
    assert!(longest >= 300, "longest room name {longest}");
    assert_eq!(count(&build("edge_empty"), "wall"), 0);
}

#[test]
fn deleted_hosts_leave_no_dependants() {
    let m = build("edge_deleted_host");
    // the door and the first window went with their wall, the basin with it, the bed with
    // the slab; the window and the radiator of the other wall and the free table stay
    assert_eq!(count(&m, "opening"), 1);
    assert_eq!(count(&m, "comp"), 2);
    assert_eq!(count(&m, "slab"), 0);
    assert_eq!(count(&m, "wall"), 3);
}

#[test]
fn a_refused_wall_is_no_step() {
    let mut m = fresh();
    let lv = m.add_level("L", 0.0).unwrap();
    let t = showcase::wall_type(&mut m, "T", &[("brick", 0.2)]);
    let ops = count(&m, "op");
    let zero = showcase::wall(lv, t, (1.0, 1.0), (1.0, 1.0), 3.0);
    assert!(m.add_wall(&zero).is_err());
    assert_eq!(count(&m, "op"), ops);
    assert_eq!(count(&m, "wall"), 0);
}

/// What a file from an older build holds: a material named only as text. Current builds add the
/// material row with the element, so the case is written in, on the first table that has one.
fn orphan_one_material(m: &Model) {
    let tables: Vec<String> = m
        .sql()
        .prepare(
            "SELECT m.name FROM sqlite_master m, pragma_table_info(m.name) p \
             WHERE m.type='table' AND p.name='material' AND upper(p.type)='TEXT' \
             AND m.name<>'material' AND m.name NOT LIKE 'sqlite_%' AND m.name NOT LIKE 'src_%' \
             AND m.name NOT LIKE '%_qty' ORDER BY m.name",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|x| x.unwrap())
        .collect();
    for t in tables {
        let sql = format!(
            "UPDATE \"{t}\" SET material='Legacy lime plaster' \
             WHERE rowid=(SELECT min(rowid) FROM \"{t}\" WHERE typeof(material)='text')"
        );
        // a WITHOUT ROWID table has no rowid: the next table is used
        if m.sql().execute(&sql, []).is_ok_and(|n| n == 1) {
            return;
        }
    }
    panic!("no table names a material as text");
}

/// Material texts without a row stay without one when a current file is reopened: opening adds
/// nothing outside the undo journal, so the saves are byte for byte the same.
#[test]
fn reopen_adds_no_rows_to_a_project_without_material_rows() {
    for name in ["office", "residential"] {
        let m = build_raw(name);
        if showcase::unmaterialized(&m).is_empty() {
            orphan_one_material(&m);
        }
        assert!(!showcase::unmaterialized(&m).is_empty(), "{name} lacks no material row");
        save_reopen(&m, name).unwrap();
    }
}

#[test]
#[ignore = "60 x 60 cells of 20 m on one level, slow in debug builds"]
fn huge_level_full_size() {
    let mut m = fresh();
    huge_level_n(&mut m, 60);
    showcase::ensure_used_materials(&mut m);
    valid(&m, "huge 60").unwrap();
    assert_eq!(count(&m, "room"), 3600);
    save_reopen(&m, "huge60").unwrap();
    let start = dump(&fresh(), Keep::User);
    undo_all(&mut m).unwrap();
    same(&dump(&m, Keep::User), &start, "huge 60 undone").unwrap();
}
