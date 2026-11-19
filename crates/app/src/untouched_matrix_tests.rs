// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! An untouched book or panel writes nothing: {metric, imperial} x {point, comma mark} over the
//! schedule book (walls and doors with project parameters; numbers, and every numeric cell saved
//! as text) and the imported IFC properties (shown text committed unchanged, read back by the
//! parser). Units and marks are passed explicitly; the mark of the schedule reader is a
//! per-thread value that a guard restores; the interface language is never touched.
//! The XLSX book itself is covered in `ddc_io` (`xlsx/tests_untouched_matrix.rs`).

use ddc_io::xlsx::{read_sheet_cells, SheetCell};
use ddc_model::source::SqlValue as Sql;
use ddc_model::{OpeningSpec, PVal, ParamSet, ParamSpec, ParamType};
use rusqlite::params;

use crate::editor::Editor;
use crate::imported_props::{commit_in, parse_value, rows_of_in, Ty};
use crate::schedule::xlsx_in::{apply, plan, plan_keyed, Key};
use crate::schedule::{build, export_xlsx_keyed, order, Cat};
use crate::units::{set_ui_decimal_mark, ui_decimal_mark, DecimalMark, Units};

const UNITS: [Units; 2] = [Units::Metric, Units::Imperial];
const MARKS: [DecimalMark; 2] = [DecimalMark::Point, DecimalMark::Comma];

/// Sets the mark of this thread's schedule reader, restores the old one on drop.
struct Mark(DecimalMark);

impl Mark {
    fn set(m: DecimalMark) -> Mark {
        let old = ui_decimal_mark();
        set_ui_decimal_mark(m);
        Mark(old)
    }
}

impl Drop for Mark {
    fn drop(&mut self) {
        set_ui_decimal_mark(self.0);
    }
}

fn spec(name: &str, t: ParamType) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        ptype: t,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    }
}

/// A room of four walls, a door on the first, one parameter of every kind with values.
fn fixture(u: Units) -> Editor {
    let mut e = Editor::new_in_memory().unwrap();
    crate::schedule::test_room(&mut e);
    let wall = e.walls[0].id;
    let door = OpeningSpec { along: 1.0, width: 0.9, sill: 0.0, height: 2.1, kind: "door".into() };
    e.model.add_opening(wall, &door).unwrap();
    let kinds = [
        ParamType::Length,
        ParamType::Area,
        ParamType::Volume,
        ParamType::Number,
        ParamType::Int,
        ParamType::Text,
    ];
    let pids: Vec<i64> = kinds
        .iter()
        .enumerate()
        .map(|(i, t)| e.model.add_param(&spec(&format!("P{i}"), *t)).unwrap())
        .collect();
    let mut sets = vec![];
    for (k, w) in e.walls.iter().enumerate() {
        let x = k as f64 + 1.0;
        let vals = [
            PVal::Num(0.3048 * x + 0.0101),
            PVal::Num(12.3456 * x),
            PVal::Num(7.891 * x),
            PVal::Num(1234.5678 * x),
            PVal::Int(3 * (k as i64 + 1)),
            PVal::Text(format!("Z-{}", k + 1)),
        ];
        for (p, v) in pids.iter().zip(vals) {
            sets.push(ParamSet { param: *p, kind: "wall".into(), id: w.id, value: Some(v) });
        }
    }
    e.model.set_param_values(&sets).unwrap();
    e.reload().unwrap();
    e.tool.units = u;
    e
}

/// A number typed into a text cell in the mark of the book.
fn as_text(x: f64, mark: DecimalMark) -> String {
    let t = format!("{x}");
    match mark {
        DecimalMark::Comma => t.replace('.', ","),
        DecimalMark::Point => t,
    }
}

fn tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ddc_untouched_{tag}_{}.xlsx", std::process::id()))
}

fn one(cat: Cat, u: Units, mark: DecimalMark, text: bool) -> Result<(), String> {
    let _mark = Mark::set(mark);
    let mut e = fixture(u);
    let tb = build(&e, cat);
    if tb.rows.is_empty() {
        return Err("no rows".into());
    }
    let ord = order(&tb, &[], None, u);
    let cols: Vec<usize> = (0..tb.cols.len()).collect();
    let p = tmp(&format!("{cat:?}_{}{}{}", u == Units::Imperial, mark == DecimalMark::Comma, text));
    export_xlsx_keyed(&e.model, &tb, &ord, &cols, u, &p, &[])?;
    let key = Key::read(&p);
    let (sheet, mut grid) = read_sheet_cells(&p, None)?;
    if text {
        // the Id column and the total line stay; every other number becomes text
        for row in grid.iter_mut().skip(1) {
            if !matches!(row.first(), Some(SheetCell::Num(_))) {
                continue;
            }
            for c in row.iter_mut().skip(1) {
                if let SheetCell::Num(x) = c {
                    *c = SheetCell::Text(as_text(*x, mark));
                }
            }
        }
    }
    let _ = std::fs::remove_file(&p);
    for (what, pl) in [
        ("two-way", plan(&e, &tb, u, &sheet, &grid)),
        ("keyed", plan_keyed(&e, &tb, u, &sheet, &grid, key.as_ref())),
    ] {
        let pl = pl?;
        if pl.changed() != 0 || pl.bad != 0 || !pl.refusals.is_empty() || pl.conflicts != 0 {
            return Err(format!(
                "{what} preview: changed {} bad {} conflicts {} cells {:?} refusals {:?}",
                pl.changed(),
                pl.bad,
                pl.conflicts,
                pl.cells.iter().take(3).collect::<Vec<_>>(),
                pl.refusals.iter().take(3).collect::<Vec<_>>()
            ));
        }
        if !pl.ignored_cols.is_empty() {
            return Err(format!("{what}: ignored columns {:?}", pl.ignored_cols));
        }
        let (rev, top) = (e.revision, e.model.op_top().ok());
        let o = apply(&mut e, &tb, &pl);
        if o.done != 0 || o.skipped() != 0 || e.revision != rev || e.model.op_top().ok() != top {
            return Err(format!("{what} apply: done {} skipped {} wrote an operation", o.done, o.skipped()));
        }
    }
    Ok(())
}

#[test]
fn untouched_schedule_book_writes_nothing_in_every_units_and_mark() {
    let mut bad = vec![];
    for cat in [Cat::Walls, Cat::Doors] {
        for u in UNITS {
            for mark in MARKS {
                for text in [false, true] {
                    if let Err(e) = one(cat, u, mark, text) {
                        bad.push(format!("{cat:?} {u:?} {mark:?} as_text {text}: {e}"));
                    }
                }
            }
        }
    }
    assert!(bad.is_empty(), "{} of 16 cells fail:\n{}", bad.len(), bad.join("\n"));
}

fn mesh(ed: &mut Editor) -> i64 {
    use ddc_geom::mesh::Mesh;
    use ddc_geom::v3;
    let m = Mesh {
        positions: vec![v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0)],
        triangles: vec![[0, 1, 2]],
        face_of: vec![0],
    };
    let (pb, ib) = ddc_model::mesh_blobs(&m);
    let level = ed.level;
    ed.model
        .batch_walls("wall_untouched_matrix_mesh", |db| {
            db.execute(
                "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,'Furniture','m',?2,?3)",
                (level, pb, ib),
            )?;
            Ok(db.last_insert_rowid())
        })
        .unwrap()
}

fn put(ed: &Editor, id: i64, key: &str, v: Sql, unit: &str) {
    ed.model
        .sql()
        .execute(
            "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES ('mesh',?1,?2,?3,?4)",
            params![id, key, v, unit],
        )
        .unwrap();
}

fn ops(ed: &Editor) -> i64 {
    ed.model.sql().query_row("SELECT count(*) FROM op", [], |r| r.get(0)).unwrap()
}

#[test]
fn untouched_imported_props_write_nothing_in_every_units_and_mark() {
    let mut bad = vec![];
    for u in UNITS {
        for mark in MARKS {
            let mut ed = Editor::new_in_memory().unwrap();
            let id = mesh(&mut ed);
            // lengths are whole inches so both unit systems show them exactly
            put(&ed, id, "Pset_A.Width", Sql::Real(0.0254 * 10.0), "m");
            put(&ed, id, "Pset_A.Depth", Sql::Real(0.3048 * 4.0), "m");
            put(&ed, id, "Pset_A.Area", Sql::Real(12.5), "m2");
            put(&ed, id, "Pset_A.Load", Sql::Real(1234.5), "");
            put(&ed, id, "Pset_A.Count", Sql::Integer(3), "");
            put(&ed, id, "Pset_A.Fire", Sql::Text("EI 30".into()), "");
            // numbers kept as text stay text, with their own mark
            put(&ed, id, "Pset_A.Code", Sql::Text("007".into()), "");
            put(&ed, id, "Pset_A.Ratio", Sql::Text("1,5".into()), "");
            put(&ed, id, "Pset_A.Size", Sql::Text("12.5".into()), "");
            let items = vec![("mesh", id)];
            let rows = rows_of_in(&ed.model, &items, u, mark);
            if rows.len() != 9 {
                bad.push(format!("{u:?} {mark:?}: {} rows", rows.len()));
                continue;
            }
            for r in &rows {
                let ctx = format!("{u:?} {mark:?} {}", r.key);
                let (Some(text), Some(unit)) = (r.value.clone(), r.unit.clone()) else {
                    bad.push(format!("{ctx}: shows no value"));
                    continue;
                };
                let n0 = ops(&ed);
                match commit_in(&mut ed, &items, r, &text, mark) {
                    Ok(false) if ops(&ed) == n0 => {}
                    other => bad.push(format!("{ctx}: unchanged text {text:?} -> {other:?}")),
                }
                // the shown text read by the parser is the stored value
                let stored = ed.model.props("mesh", id).unwrap().into_iter().find(|p| p.0 == r.key).unwrap().1;
                match (parse_value(&text, r.ty, &unit, u, mark), stored) {
                    (Ok(Sql::Real(a)), Sql::Real(b)) if (a - b).abs() <= 1e-9 * b.abs().max(1.0) => {}
                    (Ok(Sql::Integer(a)), Sql::Integer(b)) if a == b => {}
                    (Ok(Sql::Text(a)), Sql::Text(b)) if a == b && r.ty == Ty::Text => {}
                    (got, want) => bad.push(format!("{ctx}: {text:?} reads {got:?}, stored {want:?}")),
                }
            }
        }
    }
    assert!(bad.is_empty(), "{} failures:\n{}", bad.len(), bad.join("\n"));
}
