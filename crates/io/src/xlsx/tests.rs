use std::path::PathBuf;

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{LayerSpec, Model, OpeningSpec, WallSpec};
use rust_xlsxwriter::Workbook;

use super::*;

type Grid = Vec<Vec<Data>>;

fn build(height: f64, brick: f64) -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layers = [("brick", brick), ("insulation", 0.1)].map(|(n, t)| LayerSpec {
        material: n.into(),
        thickness: t,
    });
    let t = m.add_wall_type("ext", &layers).unwrap();
    let p = [(0., 0.), (5., 0.), (5., 4.), (0., 4.)];
    for i in 0..4 {
        let w = WallSpec {
            a: p[i],
            b: p[(i + 1) % 4],
            level: lv,
            base_offset: 0.0,
            height,
            wall_type: t,
            offset0: None,
        };
        m.add_wall(&w).unwrap();
    }
    let o = OpeningSpec {
        along: 1.0,
        width: 0.9,
        sill: 0.0,
        height: 2.1,
        kind: "door".into(),
    };
    m.add_opening(1, &o).unwrap();
    m
}

pub(super) fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("ddc_xlsx_tests");
    std::fs::create_dir_all(&d).unwrap();
    d.join(format!("{}_{name}.xlsx", std::process::id()))
}

type Q = (i64, i64, String, f64, f64);

fn qty(m: &Model) -> Vec<Q> {
    let mut st = m
        .sql()
        .prepare("SELECT wall,pos,material,plan_area,volume FROM wall_qty ORDER BY wall,pos")
        .unwrap();
    let rows = st
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

fn count(m: &Model, table: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

/// Reads a workbook, lets the closure edit each sheet grid, writes a new file.
pub(super) fn edit(src: &PathBuf, dst: &PathBuf, f: impl Fn(&str, &mut Grid)) {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for name in wb.sheet_names() {
        let mut grid: Grid = wb
            .worksheet_range(&name)
            .unwrap()
            .rows()
            .map(|r| r.to_vec())
            .collect();
        f(&name, &mut grid);
        let ws = out.add_worksheet();
        ws.set_name(&name).unwrap();
        for (i, row) in grid.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                    Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                    Data::Bool(b) => ws.write_boolean(i, j, *b).map(|_| ()),
                    Data::DateTime(d) => ws
                        .write_number_with_format(
                            i,
                            j,
                            d.as_f64(),
                            &rust_xlsxwriter::Format::new().set_num_format("yyyy-mm-dd"),
                        )
                        .map(|_| ()),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
    }
    out.save(dst).unwrap();
}

pub(super) fn col(g: &Grid, header: &str) -> usize {
    g[0].iter().position(|d| d.to_string() == header).unwrap()
}

pub(super) fn set(g: &mut Grid, row: usize, header: &str, v: Data) {
    let c = col(g, header);
    while g[row].len() <= c {
        g[row].push(Data::Empty);
    }
    g[row][c] = v;
}

fn export(m: &Model, name: &str) -> PathBuf {
    let p = tmp(name);
    export_xlsx(m, &p).unwrap();
    p
}

fn zip_text(path: &PathBuf, name: &str) -> String {
    use std::io::Read;
    let f = std::fs::File::open(path).unwrap();
    let mut z = zip::ZipArchive::new(f).unwrap();
    let mut s = String::new();
    z.by_name(name).unwrap().read_to_string(&mut s).unwrap();
    s
}

#[test]
fn unchanged_roundtrip_has_zero_changes() {
    let mut m = build(3.0, 0.2);
    let p = export(&m, "noop");
    let ops = count(&m, "op");
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(
        rep.applied && rep.changes.is_empty() && rep.rejected.is_empty(),
        "{rep:?}"
    );
    assert_eq!(count(&m, "op"), ops);
}

#[test]
fn export_has_all_sheets_frozen_header_and_marked_columns() {
    let m = build(3.0, 0.2);
    let p = export(&m, "sheets");
    let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
    let names = wb.sheet_names();
    for s in [WALLS, TYPES, LAYERS, OPENINGS, LEVELS, QUANTITIES] {
        assert!(names.iter().any(|n| n == s), "{s}");
    }
    let r = wb.worksheet_range(WALLS).unwrap();
    let head: Vec<String> = r
        .rows()
        .next()
        .unwrap()
        .iter()
        .map(|d| d.to_string())
        .collect();
    assert!(head.contains(&"height [m]".to_string()));
    assert!(head.contains(&"length [m] (read-only)".to_string()));
    assert_eq!(r.height(), 5);
    let x = zip_text(&p, "xl/worksheets/sheet1.xml");
    assert!(x.contains("state=\"frozen\"") && x.contains("<autoFilter"));
}

fn edit_height_and_brick(src: &PathBuf, dst: &PathBuf) {
    edit(src, dst, |sheet, g| {
        if sheet == WALLS {
            for r in 1..g.len() {
                set(g, r, "height [m]", Data::Float(3.5));
            }
        }
        if sheet == LAYERS {
            set(g, 1, "thickness [m]", Data::Float(0.25));
        }
    });
}

#[test]
fn edited_height_and_thickness_recompute_quantities_exactly() {
    let mut m = build(3.0, 0.2);
    let p = export(&m, "edit_src");
    let q = tmp("edit_dst");
    edit_height_and_brick(&p, &q);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(rep.changes.len(), 5, "{:?}", rep.changes);
    assert_eq!(qty(&m), qty(&build(3.5, 0.25)));
}

#[test]
fn undo_reverts_whole_import() {
    let mut m = build(3.0, 0.2);
    let before = qty(&m);
    let ops = count(&m, "op");
    let p = export(&m, "undo_src");
    let q = tmp("undo_dst");
    edit_height_and_brick(&p, &q);
    import_xlsx(&mut m, &q).unwrap();
    assert_ne!(qty(&m), before);
    m.undo().unwrap();
    assert_eq!(qty(&m), before);
    assert_eq!(count(&m, "op"), ops);
    let h: f64 = m
        .sql()
        .query_row("SELECT height FROM wall WHERE id=2", [], |r| r.get(0))
        .unwrap();
    assert_eq!(h, 3.0);
}

#[test]
fn ambiguous_text_number_is_flagged_in_review_and_never_applied() {
    let mut m = build(3.0, 0.2);
    let p = export(&m, "amb_src");
    let q = tmp("amb_dst");
    edit(&p, &q, |sheet, g| {
        if sheet == WALLS {
            set(g, 1, "height [m]", Data::String("1,250".into()));
            set(g, 2, "height [m]", Data::String("3\u{a0}500 mm".into()));
            set(g, 3, "height [m]", Data::String("2,750.5 mm".into()));
        }
        if sheet == LAYERS {
            set(g, 1, "thickness [m]", Data::String("0,250".into()));
        }
        // a book without the Info decimal row (fb92e87a writes it): the text alone decides
        if sheet == INFO {
            g.retain(|r| r[0].to_string() != "decimal" && r[0].to_string() != "locale");
        }
    });
    let h = |m: &Model, id: i64| -> f64 {
        m.sql()
            .query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0))
            .unwrap()
    };
    let o = ImportOptions {
        apply: false,
        delete_missing: false,
        new_params: false,
    };
    let pre = import_xlsx_with(&mut m, &q, o).unwrap();
    assert!(!pre.applied && pre.blocking() == 0);
    let flag: Vec<_> = pre
        .rejected
        .iter()
        .filter(|r| r.reason.starts_with("ambiguous number"))
        .collect();
    assert_eq!(flag.len(), 1, "{:?}", pre.rejected);
    let f = flag[0];
    assert_eq!((f.sheet.as_str(), f.row, f.column.as_str()), (WALLS, 2, "height [m]"));
    let want = "ambiguous number '1,250': write 1250 (thousands) or 1.25 (decimal)";
    assert_eq!(f.reason, want);
    assert!(!pre.changes.iter().any(|c| c.sheet == WALLS && c.row == 2));
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied);
    assert_eq!(h(&m, 1), 3.0, "the ambiguous cell keeps the model value");
    assert!((h(&m, 2) - 3.5).abs() < 1e-9);
    assert!((h(&m, 3) - 2.7505).abs() < 1e-9);
    let brick: f64 = m
        .sql()
        .query_row(
            "SELECT thickness FROM wall_type_layer WHERE material='brick'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!((brick - 0.25).abs() < 1e-12, "a leading zero is never thousands");
}

#[test]
fn ambiguous_point_cell_rejected_in_import_report() {
    let mut m = build(3.0, 0.2);
    let p = export(&m, "ambpt_src");
    let q = tmp("ambpt_dst");
    edit(&p, &q, |sheet, g| {
        if sheet == WALLS {
            set(g, 1, "height [m]", Data::String("1.250".into()));
            set(g, 2, "height [m]", Data::String("3.5".into()));
        }
        // a book without the Info decimal row (fb92e87a writes it): the text alone decides
        if sheet == INFO {
            g.retain(|r| r[0].to_string() != "decimal" && r[0].to_string() != "locale");
        }
    });
    let h = |m: &Model, id: i64| -> f64 {
        m.sql()
            .query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0))
            .unwrap()
    };
    let rep = import_xlsx(&mut m, &q).unwrap();
    let flag: Vec<_> = rep
        .rejected
        .iter()
        .filter(|r| r.reason.starts_with("ambiguous number '1.250'"))
        .collect();
    assert_eq!(flag.len(), 1, "{:?}", rep.rejected);
    assert_eq!((flag[0].sheet.as_str(), flag[0].row), (WALLS, 2));
    assert_eq!(h(&m, 1), 3.0, "the ambiguous cell keeps the model value");
    assert!((h(&m, 2) - 3.5).abs() < 1e-9);
}

#[test]
fn invalid_values_are_skipped_and_name_cells() {
    let mut m = build(3.0, 0.2);
    let before = qty(&m);
    let ops = count(&m, "op");
    let p = export(&m, "bad_src");
    let q = tmp("bad_dst");
    edit(&p, &q, |sheet, g| {
        if sheet == WALLS {
            set(g, 1, "height [m]", Data::Float(9.0)); // a valid edit still applies
            set(g, 2, "type", Data::String("nope".into()));
        }
        if sheet == LAYERS {
            set(g, 1, "thickness [m]", Data::Float(-0.1));
            set(g, 2, "thickness [m]", Data::String("abc".into()));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.blocking() == 0);
    assert_eq!(rep.rejected.len(), 3, "{:?}", rep.rejected);
    let find = |s: &str, row: u32| {
        rep.rejected
            .iter()
            .find(|r| r.sheet == s && r.row == row)
            .unwrap()
    };
    let neg = find(LAYERS, 2);
    assert_eq!(neg.column, "thickness [m]");
    assert!(neg.reason.contains("> 0") && neg.col > 0);
    assert!(find(LAYERS, 3).reason.contains("not a number"));
    let ty = find(WALLS, 3);
    assert_eq!(ty.column, "type");
    assert!(ty.reason.contains("nope"));
    let h = |id: i64| -> f64 {
        m.sql()
            .query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0))
            .unwrap()
    };
    assert_eq!((h(1), h(2)), (9.0, 3.0), "only the good row applied");
    let thin: i64 = m
        .sql()
        .query_row("SELECT COUNT(*) FROM wall_type_layer WHERE thickness<=0 OR (material='brick' AND thickness<>0.2)", [], |r| r.get(0))
        .unwrap();
    assert_eq!(thin, 0, "bad layer cells left the model value");
    assert_ne!(qty(&m), before);
    assert_eq!(count(&m, "op"), ops + 1);
}

#[test]
fn deleted_rows_stay_delete_column_deletes_new_rows_create() {
    let mut m = build(3.0, 0.2);
    let p = export(&m, "rows_src");
    // a wall row removed from the workbook leaves the wall in the model
    let q = tmp("rows_a");
    edit(&p, &q, |s, g| {
        if s == WALLS {
            g.remove(4);
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.changes.is_empty(), "{rep:?}");
    assert_eq!(count(&m, "wall"), 4);
    // explicit delete flag + a new row without id
    let q = tmp("rows_b");
    edit(&p, &q, |s, g| {
        if s == WALLS {
            set(g, 4, "delete", Data::String("delete".into()));
            g.push(vec![Data::Empty; g[0].len()]);
            let n = g.len() - 1;
            set(g, n, "level", Data::String("L1".into()));
            set(g, n, "type", Data::String("ext".into()));
            let vals = [
                ("ax [m]", 0.0),
                ("ay [m]", 10.0),
                ("bx [m]", 5.0),
                ("by [m]", 10.0),
                ("height [m]", 2.8),
            ];
            for (h, v) in vals {
                set(g, n, h, Data::Float(v));
            }
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(count(&m, "wall"), 4); // one deleted, one created
    assert_eq!(count(&m, "wall WHERE ay=10"), 1);
    m.undo().unwrap();
    assert_eq!(count(&m, "wall"), 4);
    assert_eq!(count(&m, "wall WHERE ay=10"), 0);
    assert_eq!(qty(&m), qty(&build(3.0, 0.2)));
}

fn build_full() -> Model {
    let mut m = build(3.0, 0.2);
    let st = m
        .add_slab_type(
            "floor",
            &[LayerSpec {
                material: "concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    m.add_slab(&ddc_model::SlabSpec {
        level: 1,
        slab_type: st,
        offset: 0.0,
        outer: vec![(0., 0.), (5., 0.), (5., 4.), (0., 4.)],
        holes: vec![],
    })
    .unwrap();
    m.add_room(&ddc_model::RoomSpec {
        level: 1,
        name: "Hall".into(),
        number: "101".into(),
        point: (2.5, 2.0),
        height: 3.0,
    })
    .unwrap();
    m
}

fn snapshot(m: &Model) -> Vec<String> {
    let q = |sql: &str| -> Vec<String> {
        let mut st = m.sql().prepare(sql).unwrap();
        let n = st.column_count();
        st.query_map([], |r| {
            Ok((0..n)
                .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                .collect::<Vec<_>>()
                .join("|"))
        })
        .unwrap()
        .map(|x| x.unwrap())
        .collect()
    };
    [
        "opening", "room", "slab", "room_qty", "slab_qty", "level", "wall",
    ]
    .iter()
    .flat_map(|t| q(&format!("SELECT * FROM {t} ORDER BY 1")))
    .collect()
}

#[test]
fn all_categories_roundtrip_is_noop_with_summary_and_protection() {
    let mut m = build_full();
    let p = export(&m, "full_noop");
    let (ops, snap) = (count(&m, "op"), snapshot(&m));
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(
        rep.applied && rep.changes.is_empty() && rep.rejected.is_empty(),
        "{rep:?}"
    );
    assert_eq!((count(&m, "op"), snapshot(&m)), (ops, snap));
    let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
    let names = wb.sheet_names();
    for s in [ROOMS, SLABS, SUMMARY, INFO, LEVELS, OPENINGS] {
        assert!(names.iter().any(|n| n == s), "{s}");
    }
    let r = wb.worksheet_range(ROOMS).unwrap();
    assert_eq!(r.height(), 2);
    let sm = wb.worksheet_range(SUMMARY).unwrap();
    let cats: Vec<String> = sm.rows().skip(1).map(|r| r[0].to_string()).collect();
    for c in ["opening", "room", "slab", "wall"] {
        assert!(cats.iter().any(|x| x == c), "{c} in {cats:?}");
    }
    assert!(zip_text(&p, "xl/worksheets/sheet1.xml").contains("<sheetProtection"));
}

#[test]
fn xlsx_rooms_sheet_no_double_area() {
    let mut m = build_full();
    let (first, lv) = m
        .sql()
        .query_row("SELECT id, level FROM room ORDER BY id LIMIT 1", [], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })
        .unwrap();
    // a second room in the region the first one holds, as a new Rooms row would make
    let second = m
        .add_room(&ddc_model::RoomSpec {
            level: lv,
            name: "Hall copy".into(),
            number: "102".into(),
            point: (1.0, 1.0),
            height: 3.0,
        })
        .unwrap();
    let held = m.room_quantities(first).unwrap().unwrap();
    assert!(held.area > 1.0);
    let p = export(&m, "rooms_no_double");
    let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
    let num = |d: &Data| match d {
        Data::Float(x) => *x,
        Data::Int(x) => *x as f64,
        _ => 0.0,
    };
    let g: Grid = wb
        .worksheet_range(ROOMS)
        .unwrap()
        .rows()
        .map(|r| r.to_vec())
        .collect();
    let ci = col(&g, "id");
    let ca = col(&g, "area [m2] (read-only)");
    let cv = col(&g, "volume [m3] (read-only)");
    let sum: f64 = g[1..].iter().map(|r| num(&r[ca])).sum();
    assert!((sum - held.area).abs() < 1e-6, "{sum} vs {}", held.area);
    let vol: f64 = g[1..].iter().map(|r| num(&r[cv])).sum();
    assert!((vol - held.volume).abs() < 1e-6, "{vol} vs {}", held.volume);
    let row = g[1..].iter().find(|r| num(&r[ci]) as i64 == second).unwrap();
    assert_eq!(num(&row[ca]), 0.0);
    let sm: Grid = wb
        .worksheet_range(SUMMARY)
        .unwrap()
        .rows()
        .map(|r| r.to_vec())
        .collect();
    let sa = col(&sm, "area [m2]");
    let room_row = sm.iter().find(|r| r[0].to_string() == "room").unwrap();
    assert!((num(&room_row[sa]) - held.area).abs() < 1e-6);
}

#[test]
fn door_width_and_room_name_change_exactly_and_undo_as_one() {
    let mut m = build_full();
    let before = snapshot(&m);
    let ops = count(&m, "op");
    let p = export(&m, "chg_src");
    let q = tmp("chg_dst");
    edit(&p, &q, |s, g| {
        if s == OPENINGS {
            set(g, 1, "width [m]", Data::Float(1.0));
            set(g, 1, "hinge (start/end)", Data::String("end".into()));
        }
        if s == ROOMS {
            set(g, 1, "name", Data::String("Lobby".into()));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(rep.changes.len(), 3, "{:?}", rep.changes);
    assert_eq!(count(&m, "op"), ops + 1);
    let w: f64 = m
        .sql()
        .query_row("SELECT width FROM opening WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(w, 1.0);
    assert!(m.opening_swing(1).unwrap().hinge);
    let name: String = m
        .sql()
        .query_row("SELECT name FROM room WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "Lobby");
    m.undo().unwrap();
    assert_eq!(snapshot(&m), before);
}

#[test]
fn invalid_width_is_rejected_read_only_edit_and_unknown_ids_are_ignored() {
    let mut m = build_full();
    let before = snapshot(&m);
    let p = export(&m, "inv_src");
    for (bad, why) in [
        (Data::Float(0.0), "> 0"),
        (Data::Float(-1.0), "> 0"),
        (Data::String("wide".into()), "not a number"),
    ] {
        let q = tmp("inv_w");
        edit(&p, &q, |s, g| {
            if s == OPENINGS {
                set(g, 1, "width [m]", bad.clone());
            }
        });
        let rep = import_xlsx(&mut m, &q).unwrap();
        assert!(rep.applied && rep.rejected.len() == 1, "{rep:?}");
        let r = &rep.rejected[0];
        assert_eq!((r.sheet.as_str(), r.row, r.col), (OPENINGS, 2, 4));
        assert!(r.reason.contains(why), "{}", r.reason);
    }
    let q = tmp("inv_ro");
    edit(&p, &q, |s, g| {
        if s == ROOMS {
            set(g, 1, "area [m2] (read-only)", Data::Float(99.0));
            let c = g[1].len();
            g.push(g[1].clone());
            set(g, 2, "id", Data::Float(777.0));
            let _ = c;
        }
        if s == SLABS {
            set(g, 1, "volume [m3] (read-only)", Data::Float(1.0));
        }
    });
    // read-only edits and rows of unknown ids are ignored with a reason, not blocking
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    let find = |sh: &str, row| rep.ignored.iter().find(|r| r.sheet == sh && r.row == row);
    assert!(find(ROOMS, 2).unwrap().reason.contains("read-only"));
    assert!(find(ROOMS, 3)
        .unwrap()
        .reason
        .contains("room 777 no longer exists"));
    assert!(find(SLABS, 2).unwrap().reason.contains("read-only"));
    assert_eq!(snapshot(&m), before);
}

#[test]
fn imperial_workbook_roundtrips_and_converts_on_import() {
    let mut m = build_full();
    let p = tmp("imp_src");
    export_xlsx_units(&m, &p, Units::Imperial).unwrap();
    let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
    let g: Grid = wb
        .worksheet_range(OPENINGS)
        .unwrap()
        .rows()
        .map(|r| r.to_vec())
        .collect();
    let wi = col(&g, "width [ft]");
    assert!(
        (match g[1][wi] {
            Data::Float(x) => x,
            _ => panic!(),
        } - 0.9 / 0.3048)
            .abs()
            < 1e-12
    );
    let (ops, snap) = (count(&m, "op"), snapshot(&m));
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(rep.applied && rep.changes.is_empty(), "{rep:?}");
    assert_eq!((count(&m, "op"), snapshot(&m)), (ops, snap));
    let q = tmp("imp_dst");
    edit(&p, &q, |s, g| {
        if s == OPENINGS {
            set(g, 1, "width [ft]", Data::Float(3.0));
        }
        if s == SLABS {
            set(g, 1, "offset [ft]", Data::Float(1.0));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.changes.len() == 2, "{rep:?}");
    let w: f64 = m
        .sql()
        .query_row("SELECT width FROM opening WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let o: f64 = m
        .sql()
        .query_row("SELECT offset FROM slab WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert!((w - 0.9144).abs() < 1e-12 && (o - 0.3048).abs() < 1e-12);
    // headers carry the units: an imperial sheet in a book marked metric still reads as feet
    let q = tmp("imp_mix");
    edit(&p, &q, |s, g| {
        if s == INFO {
            set_cell(g, 0, 1, Data::String("metric".into()));
        }
    });
    // the cells still equal the export baseline (22b83543): read as feet they are no edit and
    // count as model-newer; read as metres they would differ from it and show as changes
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.changes.is_empty() && rep.model_newer == 2, "{rep:?}");
}

fn set_cell(g: &mut Grid, r: usize, c: usize, v: Data) {
    g[r][c] = v;
}

/// Semicircle R=5: (5,0)→(−5,0), bulge 1; axis length 5π ≈ 15.708.
fn semicircle_model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m
        .add_wall_type(
            "c",
            &[LayerSpec {
                material: "concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let w = m
        .add_arc_wall(
            &WallSpec {
                a: (5.0, 0.0),
                b: (-5.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            },
            1.0,
        )
        .unwrap();
    (m, w)
}

fn cell_f(g: &Grid, row: usize, header: &str) -> f64 {
    match &g[row][col(g, header)] {
        Data::Float(x) => *x,
        d => panic!("{d:?}"),
    }
}

#[test]
fn arc_wall_length_bulge_column_and_opening_along_arc() {
    let (mut m, w) = semicircle_model();
    let p = export(&m, "arc_src");
    let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
    let g: Grid = wb
        .worksheet_range(WALLS)
        .unwrap()
        .rows()
        .map(|r| r.to_vec())
        .collect();
    let len = cell_f(&g, 1, "length [m] (read-only)");
    assert!((len - 15.708).abs() < 1e-3, "{len}");
    assert_eq!(cell_f(&g, 1, "bulge (tan(angle/4), 0 = straight)"), 1.0);
    // An unchanged workbook yields no edits.
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(rep.changes.is_empty() && rep.rejected.is_empty(), "{rep:?}");
    // A new opening at 12 m along the arc (longer than the 10 m chord) is accepted.
    let q = tmp("arc_dst");
    edit(&p, &q, |sheet, g| {
        if sheet == OPENINGS {
            while g.len() < 2 {
                g.push(vec![]);
            }
            set(g, 1, "wall_id", Data::Float(w as f64));
            set(g, 1, "along [m]", Data::Float(12.0));
            set(g, 1, "width [m]", Data::Float(0.9));
            set(g, 1, "sill [m]", Data::Float(0.0));
            set(g, 1, "height [m]", Data::Float(2.1));
            set(g, 1, "kind", Data::String("door".into()));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(count(&m, "opening"), 1);
    // Editing bulge: 0 — straight (length 10), invalid — rejected with the cell.
    let r = tmp("arc_bad");
    edit(&p, &r, |sheet, g| {
        if sheet == WALLS {
            set(
                g,
                1,
                "bulge (tan(angle/4), 0 = straight)",
                Data::Float(5000.0),
            );
        }
    });
    let rep = import_xlsx(&mut m, &r).unwrap();
    assert!(rep.applied && rep.blocking() == 0);
    assert_eq!(rep.rejected[0].column, "bulge (tan(angle/4), 0 = straight)");
    // Moving an end keeps the arc (bulge), the length follows the new arc.
    let s = tmp("arc_move");
    let p2 = export(&m, "arc_src2");
    edit(&p2, &s, |sheet, g| {
        if sheet == WALLS {
            set(g, 1, "bx [m]", Data::Float(-6.0));
            set(g, 1, "ax [m]", Data::Float(6.0));
        }
    });
    let rep = import_xlsx(&mut m, &s).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(m.wall_bulge(w).unwrap(), 1.0);
    assert!((m.wall_length(w).unwrap() - 6.0 * std::f64::consts::PI).abs() < 1e-9);
}

#[test]
fn export_table_roundtrip_numbers_text_totals() {
    use crate::ru_fixtures::ru;
    use calamine::{open_workbook, Data, Reader, Xlsx};
    let dir = std::env::temp_dir().join(format!("ddc_tbl_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("t.xlsx");
    let rows = vec![
        vec![TableCell::Text(ru("xl_wall").into()), TableCell::Num(3000.0)],
        vec![TableCell::Text(ru("xl_masonry").into()), TableCell::Num(1500.5)],
    ];
    let tot = [TableCell::Text(ru("total").into()), TableCell::Num(4500.5)];
    export_table(
        &p,
        ru("xl_walls"),
        &[ru("type_word").into(), ru("xl_length_mm").into()],
        &["", "0"],
        &rows,
        Some(&tot),
    )
    .unwrap();
    let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
    let r = wb.worksheet_range(ru("xl_walls")).unwrap();
    assert_eq!(r.get((0, 1)), Some(&Data::String(ru("xl_length_mm").into())));
    assert_eq!(r.get((2, 1)), Some(&Data::Float(1500.5)));
    assert_eq!(r.get((3, 0)), Some(&Data::String(ru("total").into())));
    let _ = std::fs::remove_dir_all(&dir);
}

fn build_all() -> Model {
    let mut m = build_full();
    let sec = ddc_model::MullionSection::Rect { w: 0.05, d: 0.1 };
    m.add_curtain_wall(&ddc_model::CurtainSpec {
        level: 1,
        name: "CW".into(),
        a: (0.0, 6.0),
        b: (4.0, 6.0),
        bulge: 0.0,
        base_offset: 0.0,
        height: 3.0,
        u: ddc_model::GridLines::Spacing(1.0),
        v: ddc_model::GridLines::Spacing(1.5),
        interior: sec,
        border: sec,
        lines: Default::default(),
        top: vec![],
    })
    .unwrap();
    m
}

fn snapshot_all(m: &Model) -> Vec<String> {
    let mut v = snapshot(m);
    for t in [
        "curtain_wall",
        "curtain_panel",
        "slab_vertex",
        "wall_type_layer",
    ] {
        let mut st = m
            .sql()
            .prepare(&format!("SELECT * FROM {t} ORDER BY 1,2"))
            .unwrap();
        let n = st.column_count();
        let rows = st
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap();
        v.extend(rows.map(|x| x.unwrap()));
    }
    v
}

fn noop(m: &mut Model, name: &str) {
    let p = export(m, name);
    let ops = count(m, "op");
    let rep = import_xlsx(m, &p).unwrap();
    assert!(
        rep.applied && rep.changes.is_empty(),
        "{:?} {:?}",
        rep.changes,
        rep.rejected
    );
    assert_eq!(count(m, "op"), ops);
}

fn push_row(g: &mut Grid, vals: &[(&str, Data)]) {
    let n = g[0].len();
    g.push(vec![Data::Empty; n]);
    let r = g.len() - 1;
    for (h, v) in vals {
        set(g, r, h, v.clone());
    }
}

fn s(t: &str) -> Data {
    Data::String(t.into())
}

#[test]
fn every_category_creates_updates_deletes_in_one_undo_and_reexport_is_noop() {
    let mut m = build_all();
    noop(&mut m, "all_noop");
    let before = snapshot_all(&m);
    let ops = count(&m, "op");
    let p = export(&m, "all_src");
    let q = tmp("all_dst");
    edit(&p, &q, |sh, g| match sh {
        LEVELS => push_row(g, &[("name", s("L2")), ("elevation [m]", Data::Float(3.0))]),
        WALLS => {
            set(g, 1, "height [m]", Data::Float(3.2));
            push_row(
                g,
                &[
                    ("level", s("L1")),
                    ("type", s("ext")),
                    ("ax [m]", Data::Float(10.0)),
                    ("ay [m]", Data::Float(0.0)),
                    ("bx [m]", Data::Float(14.0)),
                    ("by [m]", Data::Float(0.0)),
                    ("height [m]", s("2800 mm")),
                ],
            );
        }
        OPENINGS => push_row(
            g,
            &[
                ("wall_id", Data::Float(2.0)),
                ("along [m]", Data::Float(1.0)),
                ("width [m]", Data::Float(1.2)),
                ("sill [m]", Data::Float(0.9)),
                ("height [m]", Data::Float(1.4)),
                ("kind", s("window")),
            ],
        ),
        ROOMS => {
            set(g, 1, "number", s("102"));
            push_row(
                g,
                &[
                    ("level", s("L1")),
                    ("name", s("Store")),
                    // outside Hall's region: a new room inside a held one is refused (e0acc221)
                    ("point x [m]", Data::Float(30.0)),
                    ("point y [m]", Data::Float(30.0)),
                    ("height [m]", Data::Float(2.7)),
                ],
            );
        }
        SLABS => {
            set(g, 1, "offset [m]", Data::Float(-0.05));
            push_row(
                g,
                &[
                    ("level", s("L1")),
                    ("type", s("floor")),
                    ("outline [m] (x y; x y; ...)", s("10 0; 14 0; 14 3; 10 3")),
                ],
            );
        }
        CURTAINS => {
            set(g, 1, "name", s("Facade"));
            push_row(
                g,
                &[
                    ("level", s("L1")),
                    ("ax [m]", Data::Float(0.0)),
                    ("ay [m]", Data::Float(8.0)),
                    ("bx [m]", Data::Float(3.0)),
                    ("by [m]", Data::Float(8.0)),
                    ("height [m]", Data::Float(2.5)),
                    ("u grid (s spacing | x positions, metres)", s("x 1 2")),
                ],
            );
        }
        _ => {}
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{:?}", rep.rejected);
    let c = rep.counts();
    assert_eq!(
        (c.added, c.changed, c.deleted),
        (6, 4, 0),
        "{:?}",
        rep.changes
    );
    assert_eq!(count(&m, "op"), ops + 1);
    for (t, n) in [
        ("level", 2),
        ("wall", 5),
        ("opening", 2),
        ("room", 2),
        ("slab", 2),
        ("curtain_wall", 2),
    ] {
        assert_eq!(count(&m, t), n, "{t}");
    }
    let h: f64 = m
        .sql()
        .query_row("SELECT height FROM wall WHERE id=5", [], |r| r.get(0))
        .unwrap();
    assert!((h - 2.8).abs() < 1e-12);
    noop(&mut m, "all_again");
    m.undo().unwrap();
    assert_eq!(snapshot_all(&m), before);

    let q = tmp("all_del");
    edit(&p, &q, |sh, g| {
        if [ROOMS, SLABS, CURTAINS].contains(&sh) {
            set(g, 1, "delete", s("delete"));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.counts().deleted == 3, "{rep:?}");
    for t in [
        "room",
        "slab",
        "curtain_wall",
        "slab_vertex",
        "curtain_panel",
    ] {
        assert_eq!(count(&m, t), 0, "{t}");
    }
    m.undo().unwrap();
    assert_eq!(snapshot_all(&m), before);
}

#[test]
fn removed_rows_delete_only_when_asked_and_preview_changes_nothing() {
    let mut m = build_all();
    let before = snapshot_all(&m);
    let p = export(&m, "miss_src");
    let q = tmp("miss_dst");
    edit(&p, &q, |sh, g| {
        if [WALLS, ROOMS, SLABS].contains(&sh) {
            // wall 1 (with its door); the only room and slab leave their sheets header-only
            g.remove(1);
        }
    });
    let keep = import_xlsx(&mut m, &q).unwrap();
    assert!(keep.applied && keep.changes.is_empty(), "{keep:?}");
    let o = ImportOptions {
        apply: false,
        delete_missing: true,
        new_params: false,
    };
    let pre = import_xlsx_with(&mut m, &q, o).unwrap();
    assert!(!pre.applied && pre.rejected.is_empty());
    assert_eq!(pre.counts().deleted, 1, "{:?}", pre.changes);
    let named: Vec<(&str, SheetIssueKind)> =
        pre.sheet_issues.iter().map(|i| (i.sheet.as_str(), i.kind)).collect();
    assert_eq!(named, [(ROOMS, SheetIssueKind::Empty), (SLABS, SheetIssueKind::Empty)]);
    assert_eq!(snapshot_all(&m), before);
    let done = import_xlsx_with(&mut m, &q, ImportOptions { apply: true, ..o }).unwrap();
    assert!(done.applied);
    assert_eq!((count(&m, "wall"), count(&m, "opening")), (3, 0));
    assert_eq!((count(&m, "room"), count(&m, "slab")), (1, 1));
    m.undo().unwrap();
    assert_eq!(snapshot_all(&m), before);
}

#[test]
fn header_units_and_feet_inches_cells_convert() {
    let mut m = build_all();
    let p = export(&m, "u_src");
    let q = tmp("u_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            let c = col(g, "height [m]");
            g[0][c] = s("height [mm]");
            for row in g.iter_mut().skip(1) {
                row[c] = Data::Float(3000.0);
            }
            set(g, 1, "base_offset [m]", s("1'-6\""));
        }
        if sh == LEVELS {
            let c = col(g, "elevation [m]");
            g[0][c] = s("elevation [ft]");
            g[1][c] = s("10'-6 1/2\"");
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{:?}", rep.rejected);
    let z: f64 = m
        .sql()
        .query_row("SELECT elevation FROM level WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert!((z - (10.0 * 0.3048 + 6.5 * 0.0254)).abs() < 1e-12);
    let (h, b): (f64, f64) = m
        .sql()
        .query_row("SELECT height, base_offset FROM wall WHERE id=1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert!((h - 3.0).abs() < 1e-12 && (b - 0.4572).abs() < 1e-12);
    // 3000 mm = the current 3 m: only the level and wall 1 base offset change
    assert_eq!(rep.counts().changed, 2, "{:?}", rep.changes);
}

#[test]
fn malformed_cells_are_reported_with_sheet_cell_addresses() {
    let mut m = build_all();
    let before = snapshot_all(&m);
    let p = export(&m, "bad2_src");
    let q = tmp("bad2_dst");
    edit(&p, &q, |sh, g| match sh {
        WALLS => set(g, 1, "height [m]", s("tall")),
        ROOMS => set(g, 1, "height [m]", s("10'-x\"")),
        SLABS => set(g, 1, "outline [m] (x y; x y; ...)", s("0 0; 1")),
        CURTAINS => {
            set(g, 1, "height [m]", Data::Float(9.0));
            set(g, 1, "u grid (s spacing | x positions, metres)", s("zz"));
        }
        LEVELS => {
            let c = col(g, "elevation [m]");
            g[0][c] = s("elevation [furlong]");
        }
        _ => {}
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(!rep.applied);
    let addrs: Vec<String> = rep.rejected.iter().map(|r| r.address()).collect();
    for a in [
        "Walls!I2",
        "Rooms!G2",
        "Slabs!E2",
        "'Curtain walls'!J2",
        "'Curtain walls'!K2",
        "Levels!C1",
    ] {
        assert!(addrs.iter().any(|x| x == a), "{a} not in {addrs:?}");
    }
    assert_eq!(snapshot_all(&m), before);
}

#[test]
fn cell_refs_use_letters_and_quote_spaced_sheets() {
    assert_eq!(cell_ref("Walls", 7, 4), "Walls!D7");
    assert_eq!(cell_ref("Walls", 2, 28), "Walls!AB2");
    assert_eq!(cell_ref("Curtain walls", 3, 0), "'Curtain walls'!3:3");
}

/// `cargo test --release -p ddc-io bench_10k -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_10k_rows_import_under_a_second() {
    let mut m = Model::open(":memory:").unwrap();
    m.add_level("L1", 0.0).unwrap();
    let layer = LayerSpec {
        material: "brick".into(),
        thickness: 0.2,
    };
    m.add_wall_type("w", &[layer]).unwrap();
    let p = tmp("bench_src");
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name(WALLS).unwrap();
    let heads = [
        "id",
        "level",
        "type",
        "ax [m]",
        "ay [m]",
        "bx [m]",
        "by [m]",
        "height [m]",
    ];
    for (j, h) in heads.iter().enumerate() {
        ws.write_string(0, j as u16, *h).unwrap();
    }
    for i in 0..10_000u32 {
        let (x, y) = ((i % 100) as f64 * 2.0, (i / 100) as f64 * 2.0);
        let r = i + 1;
        ws.write_string(r, 1, "L1").unwrap();
        ws.write_string(r, 2, "w").unwrap();
        for (j, v) in [x, y, x + 1.5, y].iter().enumerate() {
            ws.write_number(r, 3 + j as u16, *v).unwrap();
        }
        ws.write_number(r, 7, 3.0).unwrap();
    }
    wb.save(&p).unwrap();
    let t0 = std::time::Instant::now();
    let rep = import_xlsx(&mut m, &p).unwrap();
    let create = t0.elapsed();
    assert!(
        rep.applied && rep.counts().added == 10_000,
        "{:?}",
        rep.rejected.first()
    );
    let p2 = export(&m, "bench_rt");
    let q = tmp("bench_edit");
    edit(&p2, &q, |sh, g| {
        if sh == WALLS {
            for r in 1..g.len() {
                set(g, r, "height [m]", Data::Float(3.1));
            }
        }
    });
    let t1 = std::time::Instant::now();
    let o = ImportOptions {
        apply: false,
        delete_missing: false,
        new_params: false,
    };
    let _ = import_xlsx_with(&mut m, &q, o).unwrap();
    let parse = t1.elapsed();
    let t1 = std::time::Instant::now();
    let rep = import_xlsx(&mut m, &q).unwrap();
    let upd = t1.elapsed();
    println!("preview {parse:?}");
    assert!(rep.applied && rep.counts().changed == 10_000);
    println!("10k rows: preview {parse:?}, create {create:?}, update {upd:?}");
    // parse + validate + diff; applying adds the model's derived-quantity regeneration
    assert!(parse.as_secs_f64() < 1.0, "{parse:?}");
}

#[test]
fn rejected_date_cell_changes_nothing_and_has_a_human_reason() {
    let mut m = build_all();
    let before = snapshot_all(&m);
    let p = export(&m, "date_src");
    let q = tmp("date_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            let d = calamine::ExcelDateTime::new(45000.0, calamine::ExcelDateTimeType::DateTime, false);
            set(g, 1, "height [m]", Data::DateTime(d));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    // the bad cell skips alone (36c71265): nothing else was edited, so nothing changes
    assert!(rep.blocking() == 0 && rep.rejected.len() == 1, "{rep:?}");
    assert!(rep.rejected[0].reason.contains("turned this value into a date"));
    assert!(rep.edits.is_empty(), "{:?}", rep.edits);
    assert_eq!(snapshot_all(&m), before);
}

fn info_rows(p: &PathBuf) -> Vec<(String, String)> {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    wb.worksheet_range(INFO)
        .unwrap()
        .rows()
        .map(|r| (r[0].to_string(), r.get(1).map(|d| d.to_string()).unwrap_or_default()))
        .collect()
}

fn info_get(p: &PathBuf, key: &str) -> Option<String> {
    info_rows(p).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn export_with(m: &Model, name: &str, st: &Style) -> PathBuf {
    let p = tmp(name);
    export_xlsx_styled(m, &p, &p, Units::Metric, &[], st).unwrap();
    p
}

#[test]
fn export_writes_info_decimal() {
    let m = build(3.0, 0.2);
    assert_eq!(info_get(&export(&m, "infodec_def"), "decimal").as_deref(), Some("point"));
    assert_eq!(info_get(&export(&m, "infodec_def2"), "locale"), None);
    let st = Style::default().with_number_format(true, "de-DE");
    let p = export_with(&m, "infodec_de", &st);
    assert_eq!(info_get(&p, "decimal").as_deref(), Some("comma"));
    assert_eq!(info_get(&p, "locale").as_deref(), Some("de-DE"));
}

#[test]
fn export_de_locale_writes_comma() {
    let m = build(3.0, 0.2);
    for (i, loc) in ["de-DE", "fr", "es_ES", "it", "ru"].iter().enumerate() {
        let st = Style::default().with_number_format(false, loc);
        let p = export_with(&m, &format!("infodec_loc{i}"), &st);
        assert_eq!(info_get(&p, "decimal").as_deref(), Some("comma"), "{loc}");
    }
}

#[test]
fn unknown_locale_no_decimal_row_stays_ambiguous() {
    let m = build(3.0, 0.2);
    let st = Style::default().with_number_format(true, "xx-XX");
    let p = export_with(&m, "infodec_xx", &st);
    assert_eq!(info_get(&p, "decimal"), None);
    assert_eq!(info_get(&p, "locale").as_deref(), Some("xx-XX"));
}

#[test]
fn en_locale_writes_point() {
    let m = build(3.0, 0.2);
    let st = Style::default().with_number_format(true, "en-US");
    let p = export_with(&m, "infodec_en", &st);
    assert_eq!(info_get(&p, "decimal").as_deref(), Some("point"));
}

fn ambiguous_edit(src: &PathBuf, dst: &PathBuf, drop_decimal: bool) {
    edit(src, dst, |sheet, g| {
        // 2.250, not 1.250: wall 1 holds a 2.1 m door, a lower wall fails the geometry check
        if sheet == WALLS {
            set(g, 1, "height [m]", Data::String("2.250".into()));
        }
        if sheet == INFO && drop_decimal {
            g.retain(|r| r[0].to_string() != "decimal" && r[0].to_string() != "locale");
        }
    });
}

fn height1(m: &Model) -> f64 {
    m.sql().query_row("SELECT height FROM wall WHERE id=1", [], |r| r.get(0)).unwrap()
}

#[test]
fn roundtrip_point_three_digits_resolved_after_export() {
    let mut m = build(3.0, 0.2);
    let p = export(&m, "rtpt_src");
    let q = tmp("rtpt_dst");
    ambiguous_edit(&p, &q, false);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(!rep.rejected.iter().any(|r| r.reason.contains("ambiguous")), "{:?}", rep.rejected);
    assert!((height1(&m) - 2.25).abs() < 1e-9);
    // comma book: the same text is a thousands group
    let mut m2 = build(3.0, 0.2);
    let st = Style::default().with_number_format(true, "de-DE");
    let p2 = export_with(&m2, "rtpt_src_de", &st);
    let q2 = tmp("rtpt_dst_de");
    ambiguous_edit(&p2, &q2, false);
    let rep2 = import_xlsx(&mut m2, &q2).unwrap();
    assert!(!rep2.rejected.iter().any(|r| r.reason.contains("ambiguous")), "{:?}", rep2.rejected);
}

#[test]
fn book_without_info_decimal_still_ambiguous() {
    let mut m = build(3.0, 0.2);
    let p = export(&m, "noinfodec_src");
    let q = tmp("noinfodec_dst");
    ambiguous_edit(&p, &q, true);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(
        rep.rejected.iter().any(|r| r.reason.starts_with("ambiguous number '2.250'")),
        "{:?}",
        rep.rejected
    );
    assert_eq!(height1(&m), 3.0);
}

fn height_of(m: &Model, id: i64) -> f64 {
    m.sql()
        .query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

/// Walls sheet with `above` rows pushed over the header, the first data row set to 3.5.
fn shifted(name: &str, above: Vec<Vec<Data>>) -> (Model, PathBuf) {
    let m = build(3.0, 0.2);
    let p = export(&m, &format!("{name}_src"));
    let q = tmp(&format!("{name}_dst"));
    edit(&p, &q, |sheet, g| {
        if sheet == WALLS {
            set(g, 1, "height [m]", Data::Float(3.5));
            for (i, r) in above.iter().enumerate() {
                g.insert(i, r.clone());
            }
        }
    });
    (m, q)
}

fn txt(t: &str) -> Vec<Data> {
    vec![Data::String(t.into())]
}

#[test]
fn title_rows_above_header_read() {
    let (mut m, q) = shifted("title", vec![txt("Wall schedule"), txt("note")]);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{:?}", rep.rejected);
    assert!((height_of(&m, 1) - 3.5).abs() < 1e-9);
}

#[test]
fn row_numbers_follow_found_header() {
    let (mut m, q) = shifted("rows", vec![txt("title"), txt("note")]);
    let o = ImportOptions {
        apply: false,
        delete_missing: false,
        new_params: false,
    };
    let rep = import_xlsx_with(&mut m, &q, o).unwrap();
    let c = rep.changes.iter().find(|c| c.sheet == WALLS).unwrap();
    assert_eq!(c.row, 4, "header on row 3, first data row 4");
}

#[test]
fn no_matching_row_keeps_rejection() {
    let m = build(3.0, 0.2);
    let p = export(&m, "nomatch_src");
    let q = tmp("nomatch_dst");
    edit(&p, &q, |sheet, g| {
        if sheet == WALLS {
            for c in g[0].iter_mut() {
                *c = Data::String("zzz".into());
            }
        }
    });
    let mut m = m;
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(!rep.applied);
    assert!(rep.rejected.iter().any(|r| r.sheet == WALLS && r.reason.contains("missing")));
}

#[test]
fn two_candidate_rows_picks_the_better_match() {
    let decoy = vec![Data::String("id".into()), Data::String("height [m]".into())];
    let (mut m, q) = shifted("two", vec![decoy]);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{:?}", rep.rejected);
    assert!((height_of(&m, 1) - 3.5).abs() < 1e-9);
}

#[test]
fn empty_first_rows() {
    let (mut m, q) = shifted("empty", vec![vec![], vec![], vec![]]);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{:?}", rep.rejected);
    assert!((height_of(&m, 1) - 3.5).abs() < 1e-9);
}

#[test]
fn rooms_sheet_rows_equal_model_rooms_and_zero_room_model_is_empty() {
    let m = build_full();
    let p = export(&m, "rooms_count");
    let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
    let rows = wb.worksheet_range(ROOMS).unwrap().height() - 1;
    assert_eq!(rows, count(&m, "room") as usize);
    let s = crate::import_summary::ImportSummary {
        native: vec![("room".into(), rows)],
        ..Default::default()
    };
    assert_eq!(s.counts().rooms, rows);
    // a model without rooms: no rows in the sheet
    m.sql().execute("DELETE FROM room", []).unwrap();
    let p = export(&m, "rooms_none");
    let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
    assert_eq!(wb.worksheet_range(ROOMS).unwrap().height().saturating_sub(1), 0);
}
