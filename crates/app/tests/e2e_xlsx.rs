//! Workflow: XLSX export → edit cells → preview → apply → undo/redo, through the editor.
//! Edited books are written with `export_table` (one sheet per file), the only writer the
//! app crate can reach; each book is applied as its own `apply_xlsx` = one undo step.

mod e2e_common;

use std::path::Path;
use std::sync::Arc;

use ddc_app::editor::{Editor, OpFn};
use ddc_io::xlsx::{self, ImportOptions, Report, TableCell as C};
use ddc_model::{Model, OpeningSpec, RoomSpec, WallSpec};
use e2e_common::{assert_clean, assert_same, count, dump_data, ids, journal_len, temp_file};

fn op(
    ed: &mut Editor,
    f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) {
    ed.run_op(OpFn(Arc::new(f))).unwrap();
    assert_clean(&ed.model, "after op");
}

fn num(m: &Model, sql: &str) -> f64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn int(m: &Model, sql: &str) -> i64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn text(m: &Model, sql: &str) -> String {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// 6x4 box of four walls (ids 1..4) with a door on wall 2, a window on wall 1, a void on
/// wall 4, and one room inside.
fn scene() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let (l, t) = (ed.level, ed.types[1].id);
    let corners = [
        ((0.0, 0.0), (6.0, 0.0)),
        ((6.0, 0.0), (6.0, 4.0)),
        ((6.0, 4.0), (0.0, 4.0)),
        ((0.0, 4.0), (0.0, 0.0)),
    ];
    for (a, b) in corners {
        op(&mut ed, move |m| {
            m.add_wall(&WallSpec {
                a,
                b,
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .map(drop)
        });
    }
    let w = ids(&ed.model, "wall");
    assert_eq!(w.len(), 4);
    let holes = [
        (w[1], 1.0, 0.9, 0.0, 2.1, "door"),
        (w[0], 2.0, 1.2, 0.9, 1.2, "window"),
        (w[3], 1.0, 1.0, 0.0, 2.0, "void"),
    ];
    for (wall, along, width, sill, height, kind) in holes {
        op(&mut ed, move |m| {
            m.add_opening(
                wall,
                &OpeningSpec {
                    along,
                    width,
                    sill,
                    height,
                    kind: kind.into(),
                },
            )
            .map(drop)
        });
    }
    op(&mut ed, move |m| {
        m.add_room(&RoomSpec {
            level: l,
            name: "Kitchen".into(),
            number: "101".into(),
            point: (3.0, 2.0),
            height: 2.7,
        })
        .map(drop)
    });
    ed
}

fn query_rows(m: &Model, sql: &str, n: usize) -> Vec<Vec<C>> {
    let mut st = m.sql().prepare(sql).unwrap();
    let rows: Vec<Vec<C>> = st
        .query_map([], |r| {
            let mut v = vec![C::Num(r.get::<_, i64>(0)? as f64)];
            for i in 1..n {
                // text columns come first in the queries below, numbers after
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

fn walls_rows(m: &Model) -> Vec<Vec<C>> {
    query_rows(
        m,
        "SELECT w.id,lv.name,t.name,w.ax,w.ay,w.bx,w.by,w.base_offset,w.height FROM wall w \
         JOIN level lv ON lv.id=w.level JOIN wall_type t ON t.id=w.wtype ORDER BY w.id",
        9,
    )
}

fn write(path: &Path, sheet: &str, heads: &[&str], rows: &[Vec<C>]) {
    let h: Vec<String> = heads.iter().map(|s| s.to_string()).collect();
    xlsx::export_table(path, sheet, &h, &[], rows, None).unwrap();
}

/// Preview only: the model is not touched.
fn preview(m: &mut Model, p: &Path) -> Report {
    let o = ImportOptions {
        apply: false,
        delete_missing: false,
        new_params: false,
    };
    xlsx::import_xlsx_with(m, p, o).unwrap()
}

fn t(s: &str) -> C {
    C::Text(s.into())
}

#[test]
fn e2e_xlsx_unchanged_book_gives_zero_edits() {
    let mut ed = scene();
    let before = dump_data(&ed.model);
    let ops = journal_len(&ed.model);
    let p = temp_file("xlsx_unchanged.xlsx");
    xlsx::export_xlsx(&ed.model, &p).unwrap();
    assert_same(&dump_data(&ed.model), &before, "export reads only");

    let rep = preview(&mut ed.model, &p);
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
    assert!(rep.edits.is_empty() && rep.batch().is_empty());
    assert_eq!(rep.counts(), Default::default());

    // an empty batch is not an undo step
    let undo_len = ed.can_undo();
    ed.apply_xlsx(rep.batch(), "").unwrap();
    assert_eq!((ed.can_undo(), journal_len(&ed.model)), (undo_len, ops));
    assert_same(&dump_data(&ed.model), &before, "empty apply");
    assert_clean(&ed.model, "unchanged book");
}

#[test]
fn e2e_xlsx_edit_cells_sync_undo_redo() {
    let mut ed = scene();
    let w = ids(&ed.model, "wall");
    let lvl = text(&ed.model, "SELECT name FROM level LIMIT 1");
    let t_out = ids(&ed.model, "wall_type")[2];
    let t_out_name = text(
        &ed.model,
        &format!("SELECT name FROM wall_type WHERE id={t_out}"),
    );
    let t0 = int(&ed.model, "SELECT wtype FROM wall WHERE id=1");
    let t0_name = text(
        &ed.model,
        &format!("SELECT name FROM wall_type WHERE id={t0}"),
    );
    assert_ne!(t_out, t0);
    let state0 = dump_data(&ed.model);

    // 1. Walls: height as number, height as text with unit, other type, delete a wall, add a wall
    let p = temp_file("xlsx_walls.xlsx");
    let mut rows = walls_rows(&ed.model);
    rows[0][8] = C::Num(3.3);
    rows[1][8] = t("3200 mm");
    rows[2][2] = t(&t_out_name);
    for r in rows.iter_mut() {
        r.push(t(""));
    }
    rows[3][9] = t("delete");
    rows.push(vec![
        t(""),
        t(&lvl),
        t(&t0_name),
        C::Num(8.0),
        C::Num(0.0),
        C::Num(8.0),
        C::Num(4.0),
        C::Num(0.0),
        C::Num(2.8),
        t(""),
    ]);
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
        "delete",
    ];
    write(&p, "Walls", &heads, &rows);
    let rep = preview(&mut ed.model, &p);
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    let c = rep.counts();
    assert_eq!(
        (c.added, c.changed, c.deleted),
        (1, 3, 1),
        "{:?}",
        rep.changes
    );
    assert_same(&dump_data(&ed.model), &state0, "preview changes nothing");
    ed.apply_xlsx(rep.batch(), "walls").unwrap();
    assert_clean(&ed.model, "walls applied");
    let m = &ed.model;
    assert!(near(
        num(m, &format!("SELECT height FROM wall WHERE id={}", w[0])),
        3.3
    ));
    assert!(near(
        num(m, &format!("SELECT height FROM wall WHERE id={}", w[1])),
        3.2
    ));
    assert_eq!(
        int(m, &format!("SELECT wtype FROM wall WHERE id={}", w[2])),
        t_out
    );
    // untouched columns of an edited row stay
    assert!(near(
        num(m, &format!("SELECT bx FROM wall WHERE id={}", w[0])),
        6.0
    ));
    assert_eq!(
        int(m, &format!("SELECT wtype FROM wall WHERE id={}", w[0])),
        t0
    );
    assert!(near(
        num(m, &format!("SELECT height FROM wall WHERE id={}", w[2])),
        3.0
    ));
    // a new row may reuse the freed id (bulk.rs): the old wall is gone, whatever holds that id now
    let old = format!(
        "SELECT count(*) FROM wall WHERE id={} AND NOT (ax=8 AND bx=8)",
        w[3]
    );
    assert_eq!(int(m, &old), 0);
    // the wall's opening went with it; the new wall is the one at x=8
    assert_eq!((count(m, "wall"), count(m, "opening")), (4, 2));
    assert!(near(
        num(m, "SELECT height FROM wall WHERE ax=8 AND bx=8"),
        2.8
    ));
    let state1 = dump_data(&ed.model);

    // 2. Openings: widen the door, delete the window
    let p = temp_file("xlsx_openings.xlsx");
    let mut rows = query_rows(
        &ed.model,
        "SELECT id,wall,along,width,sill,height,kind FROM opening ORDER BY id",
        7,
    );
    assert_eq!(rows.len(), 2);
    rows[0][3] = C::Num(1.0);
    for r in rows.iter_mut() {
        r.push(t(""));
    }
    rows[1][7] = t("delete");
    let heads = [
        "id",
        "wall_id",
        "along [m]",
        "width [m]",
        "sill [m]",
        "height [m]",
        "kind",
        "delete",
    ];
    write(&p, "Openings", &heads, &rows);
    let rep = preview(&mut ed.model, &p);
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    let c = rep.counts();
    assert_eq!(
        (c.added, c.changed, c.deleted),
        (0, 1, 1),
        "{:?}",
        rep.changes
    );
    ed.apply_xlsx(rep.batch(), "openings").unwrap();
    assert_clean(&ed.model, "openings applied");
    let m = &ed.model;
    assert_eq!(count(m, "opening"), 1);
    assert_eq!(text(m, "SELECT kind FROM opening"), "door");
    assert!(near(num(m, "SELECT width FROM opening"), 1.0));
    let state2 = dump_data(&ed.model);

    // 3. Rooms: rename, renumber (a number cell is read as text), new height, one new room
    let p = temp_file("xlsx_rooms.xlsx");
    let mut rows = query_rows(
        &ed.model,
        "SELECT r.id,lv.name,r.name,r.number,r.px,r.py,r.height FROM room r \
         JOIN level lv ON lv.id=r.level ORDER BY r.id",
        7,
    );
    assert_eq!(rows.len(), 1);
    rows[0][2] = t("Living");
    rows[0][3] = C::Num(102.0);
    rows[0][6] = C::Num(3.0);
    rows.push(vec![
        t(""),
        t(&lvl),
        t("Hall"),
        t("103"),
        C::Num(7.0),
        C::Num(2.0),
        C::Num(2.5),
    ]);
    let heads = [
        "id",
        "level",
        "name",
        "number",
        "point x [m]",
        "point y [m]",
        "height [m]",
    ];
    write(&p, "Rooms", &heads, &rows);
    let rep = preview(&mut ed.model, &p);
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    let c = rep.counts();
    assert_eq!(
        (c.added, c.changed, c.deleted),
        (1, 1, 0),
        "{:?}",
        rep.changes
    );
    ed.apply_xlsx(rep.batch(), "rooms").unwrap();
    assert_clean(&ed.model, "rooms applied");
    let m = &ed.model;
    assert_eq!(count(m, "room"), 2);
    assert_eq!(text(m, "SELECT name FROM room WHERE id=1"), "Living");
    assert_eq!(text(m, "SELECT number FROM room WHERE id=1"), "102");
    assert!(near(num(m, "SELECT height FROM room WHERE id=1"), 3.0));
    assert_eq!(text(m, "SELECT name FROM room WHERE name='Hall'"), "Hall");
    let state3 = dump_data(&ed.model);

    // each book was one undo step: back through the states and forward again
    ed.undo();
    assert_clean(&ed.model, "undo rooms");
    assert_same(&dump_data(&ed.model), &state2, "undo rooms");
    ed.undo();
    assert_clean(&ed.model, "undo openings");
    assert_same(&dump_data(&ed.model), &state1, "undo openings");
    ed.undo();
    assert_clean(&ed.model, "undo walls");
    assert_same(&dump_data(&ed.model), &state0, "undo walls");
    assert_eq!(
        (count(&ed.model, "wall"), count(&ed.model, "opening")),
        (4, 3)
    );

    ed.redo();
    assert_same(&dump_data(&ed.model), &state1, "redo walls");
    ed.redo();
    assert_same(&dump_data(&ed.model), &state2, "redo openings");
    ed.redo();
    assert_clean(&ed.model, "redo rooms");
    assert_same(&dump_data(&ed.model), &state3, "redo rooms");
    assert!(!ed.can_redo());
}

#[test]
fn e2e_xlsx_rejected_cells_apply_nothing() {
    let mut ed = scene();
    let state0 = dump_data(&ed.model);
    let p = temp_file("xlsx_bad.xlsx");
    let mut rows = walls_rows(&ed.model);
    rows[0][2] = t("no such wall type");
    rows[1][8] = C::Num(-1.0);
    rows[2][8] = t("tall");
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
    ];
    write(&p, "Walls", &heads, &rows);
    let rep = preview(&mut ed.model, &p);
    // every bad cell is reported at once, with its address
    assert!(rep.rejected.len() >= 3, "{:?}", rep.rejected);
    assert!(rep
        .rejected
        .iter()
        .all(|r| r.row >= 2 && !r.address().is_empty()));
    assert_same(&dump_data(&ed.model), &state0, "rejected book");
    assert_clean(&ed.model, "rejected book");
}

#[test]
fn schedule_reimport_skips_totals_row() {
    let mut ed = scene();
    let p = temp_file("xlsx_totals.xlsx");
    let rows = walls_rows(&ed.model);
    let heads = [
        "id", "level", "type", "ax [m]", "ay [m]", "bx [m]", "by [m]", "base_offset [m]",
        "height [m]",
    ];
    let h: Vec<String> = heads.iter().map(|s| s.to_string()).collect();
    let mut tot = vec![C::CountLabel { pre: "Total: ".into(), post: String::new(), n: rows.len() }];
    tot.extend((1..9).map(|j| if j < 3 { t("") } else { C::Subtotal(109, 1.0) }));
    xlsx::export_table(&p, "Walls", &h, &[], &rows, Some(&tot)).unwrap();
    let rep = preview(&mut ed.model, &p);
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
    assert!(rep.ignored.is_empty(), "{:?}", rep.ignored);
    assert_eq!(rep.counts(), Default::default());
}

#[test]
fn grouped_reimport_skips_group_rows() {
    use ddc_io::xlsx::TableLine as L;
    let mut ed = scene();
    let p = temp_file("xlsx_groups.xlsx");
    let rows = walls_rows(&ed.model);
    let heads = [
        "id", "level", "type", "ax [m]", "ay [m]", "bx [m]", "by [m]", "base_offset [m]",
        "height [m]",
    ];
    let h: Vec<String> = heads.iter().map(|s| s.to_string()).collect();
    let sub: Vec<C> = (0..9).map(|j| if j < 3 { t("Group total: 2") } else { C::Subtotal(109, 1.0) }).collect();
    let mut lines = vec![L::Head { text: "type: A \u{b7} 2".into() }];
    lines.extend((0..rows.len()).map(L::Row));
    lines.push(L::Sub { cells: sub });
    let mut tot = vec![C::CountLabel { pre: "Total: ".into(), post: String::new(), n: rows.len() }];
    tot.extend((1..9).map(|j| if j < 3 { t("") } else { C::Subtotal(109, 1.0) }));
    xlsx::export_table_lines(&p, "Walls", &h, &[], &rows, &lines, Some(&tot)).unwrap();
    let rep = preview(&mut ed.model, &p);
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
    assert!(rep.ignored.is_empty(), "{:?}", rep.ignored);
    assert_eq!(rep.counts(), Default::default());
}
