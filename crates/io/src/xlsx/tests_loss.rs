//! Loss check of a re-export: the old book is compared with the freshly written one and
//! only what really goes missing is listed.

use std::path::PathBuf;

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{LayerSpec, Model, WallSpec};
use rust_xlsxwriter::{DataValidation, Format, Workbook, Worksheet};

use super::tests::tmp;
use super::*;

type Grid = Vec<Vec<Data>>;

fn walls_model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = LayerSpec { material: "brick".into(), thickness: 0.25 };
    let t = m.add_wall_type("ext", &[layer]).unwrap();
    for x in [0.0, 3.0, 6.0] {
        let w = WallSpec {
            a: (x, 0.0),
            b: (x + 2.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        m.add_wall(&w).unwrap();
    }
    m
}

fn put(ws: &mut Worksheet, i: u32, j: u16, d: &Data) {
    match d {
        Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
        Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
        Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
        Data::Bool(b) => ws.write_boolean(i, j, *b).map(|_| ()),
        _ => Ok(()),
    }
    .unwrap();
}

/// Copy of `src`: `f` edits each sheet grid, `extra` adds sheets and package parts at the end.
fn rewrite(
    src: &PathBuf,
    name: &str,
    f: impl Fn(&str, &mut Grid),
    extra: impl Fn(&mut Workbook),
) -> PathBuf {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for n in wb.sheet_names() {
        let mut grid: Grid = wb.worksheet_range(&n).unwrap().rows().map(|r| r.to_vec()).collect();
        f(&n, &mut grid);
        let ws = out.add_worksheet();
        ws.set_name(&n).unwrap();
        for (i, row) in grid.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                put(ws, i as u32, j as u16, d);
            }
        }
    }
    extra(&mut out);
    let q = tmp(name);
    out.save(&q).unwrap();
    q
}

fn set(g: &mut Grid, r: usize, c: usize, v: Data) {
    while g.len() <= r {
        g.push(vec![]);
    }
    while g[r].len() <= c {
        g[r].push(Data::Empty);
    }
    g[r][c] = v;
}

/// Exports the model over `old` into a new file and lists what `old` would lose.
fn losses(m: &Model, old: &PathBuf, tag: &str, given: usize) -> Vec<String> {
    let new = tmp(&format!("{tag}_new"));
    export_xlsx_families_from(m, old, &new, Units::Metric, &[]).unwrap();
    reexport_losses(old, &new, &[], given).unwrap()
}

#[test]
fn plain_book_loses_nothing() {
    let m = walls_model();
    let p = tmp("loss_plain");
    export_xlsx(&m, &p).unwrap();
    assert_eq!(losses(&m, &p, "loss_plain", 0), Vec::<String>::new());
}

#[test]
fn missing_old_book_loses_nothing() {
    let m = walls_model();
    let new = tmp("loss_none_new");
    export_xlsx(&m, &new).unwrap();
    assert!(reexport_losses(&tmp("loss_none_missing"), &new, &[], 0).unwrap().is_empty());
}

#[test]
fn user_column_and_user_sheet_are_carried_so_not_listed() {
    let m = walls_model();
    let p = tmp("loss_user_src");
    export_xlsx(&m, &p).unwrap();
    let q = rewrite(
        &p,
        "loss_user",
        |n, g| {
            if n == WALLS {
                let c = g[0].len();
                set(g, 0, c, Data::String("Cost".into()));
                set(g, 1, c, Data::Float(12.5));
            }
        },
        |wb| {
            let ws = wb.add_worksheet();
            ws.set_name("Notes").unwrap();
            ws.write_string(0, 0, "remember").unwrap();
        },
    );
    assert_eq!(losses(&m, &q, "loss_user", 0), Vec::<String>::new());
}

#[test]
fn headerless_cells_and_rows_without_id_are_listed() {
    let m = walls_model();
    let p = tmp("loss_bare_src");
    export_xlsx(&m, &p).unwrap();
    let q = rewrite(
        &p,
        "loss_bare",
        |n, g| {
            if n == WALLS {
                let c = g[0].len() + 1;
                set(g, 1, c, Data::String("note without header".into()));
                let r = g.len() + 1;
                set(g, r, c, Data::String("total by hand".into()));
            }
        },
        |_| {},
    );
    let lost = losses(&m, &q, "loss_bare", 0);
    assert!(lost.contains(&format!("1 cells without a column header on sheet '{WALLS}'")), "{lost:?}");
    assert!(lost.contains(&format!("1 rows without an id on sheet '{WALLS}'")), "{lost:?}");
    // rows the Apply before the re-export numbered are not lost
    let lost = losses(&m, &q, "loss_bare_given", 1);
    assert!(!lost.iter().any(|l| l.contains("rows without an id")), "{lost:?}");
}

#[test]
fn user_cells_of_a_deleted_element_are_not_listed() {
    let mut m = walls_model();
    let p = tmp("loss_del_src");
    export_xlsx(&m, &p).unwrap();
    let q = rewrite(
        &p,
        "loss_del",
        |n, g| {
            if n == WALLS {
                let c = g[0].len();
                set(g, 0, c, Data::String("Cost".into()));
                for r in 1..g.len() {
                    set(g, r, c, Data::Float(r as f64));
                }
            }
        },
        |_| {},
    );
    let id: i64 = m.sql().query_row("SELECT MAX(id) FROM wall", [], |r| r.get(0)).unwrap();
    m.delete_wall(id).unwrap();
    assert_eq!(losses(&m, &q, "loss_del", 0), Vec::<String>::new());
}

#[test]
fn duplicate_id_row_listed_as_loss() {
    let mut m = walls_model();
    let p = tmp("loss_dup_src");
    export_xlsx(&m, &p).unwrap();
    let q = rewrite(
        &p,
        "loss_dup",
        |n, g| {
            if n == WALLS {
                // a copy-pasted row: same id, the user's note beside it
                let mut row = g[1].clone();
                let c = g[0].len();
                row.resize(c + 1, Data::Empty);
                row[c] = Data::String("second copy".into());
                g.push(row);
            }
        },
        |_| {},
    );
    // the preview names the row it ignores
    let o = ImportOptions { apply: false, delete_missing: false, new_params: false };
    let rep = import_xlsx_with(&mut m, &q, o).unwrap();
    assert!(rep.ignored.iter().any(|i| i.sheet == WALLS && i.reason.starts_with("duplicate id")), "{rep:?}");
    // the re-export does not drop it unasked
    let lost = losses(&m, &q, "loss_dup", 0);
    assert!(lost.contains(&format!("1 rows with a repeated id on sheet '{WALLS}'")), "{lost:?}");
    // a plain book has no repeated rows to list
    assert!(!losses(&m, &p, "loss_dup_plain", 0).iter().any(|l| l.contains("repeated id")));
}

#[test]
fn merges_validation_and_links_of_a_user_sheet_are_listed() {
    let m = walls_model();
    let p = tmp("loss_parts_src");
    export_xlsx(&m, &p).unwrap();
    let q = rewrite(
        &p,
        "loss_parts",
        |_, _| {},
        |wb| {
            let ws = wb.add_worksheet();
            ws.set_name("Notes").unwrap();
            ws.merge_range(0, 0, 0, 2, "plan", &Format::new()).unwrap();
            let dv = DataValidation::new().allow_list_strings(&["yes", "no"]).unwrap();
            ws.add_data_validation(2, 0, 5, 0, &dv).unwrap();
            ws.write_url(3, 1, "https://example.com").unwrap();
        },
    );
    let lost = losses(&m, &q, "loss_parts", 0);
    for want in [
        "merged cells on sheet 'Notes'",
        "data validation on sheet 'Notes'",
        "links on sheet 'Notes'",
    ] {
        assert!(lost.iter().any(|l| l == want), "{want}: {lost:?}");
    }
    // values of the user sheet are carried: not listed as cells
    assert!(!lost.iter().any(|l| l.contains("cells of sheet")), "{lost:?}");
}

#[test]
fn foreign_book_is_listed_whole() {
    let m = walls_model();
    let p = tmp("loss_foreign");
    let mut wb = Workbook::new();
    wb.add_worksheet().write_string(0, 0, "their data").unwrap();
    wb.save(&p).unwrap();
    let lost = losses(&m, &p, "loss_foreign", 0);
    assert_eq!(lost, vec![super::loss::NOT_OURS.to_string()]);
}

#[test]
fn refusal_names_only_items_not_accepted() {
    let lost = vec!["charts on sheet 'A'".to_string(), "macros of the workbook".to_string()];
    let e = refusal(&lost, &[]).expect("refused");
    assert!(e.starts_with(LOSS_PREFIX));
    assert_eq!(loss_items(&e), Some(lost.clone()));
    let e = refusal(&lost, &lost[..1]).expect("one item left");
    assert_eq!(loss_items(&e), Some(lost[1..].to_vec()));
    assert_eq!(refusal(&lost, &lost), None);
    assert_eq!(loss_items("cannot open workbook"), None);
}

/// Column of header `h` in a sheet grid.
fn col_of(g: &Grid, h: &str) -> usize {
    g[0].iter().position(|d| d.to_string() == h).unwrap()
}

/// Grid row whose cells under `heads` hold the numbers `vals`.
fn row_with(g: &Grid, heads: &[&str], vals: &[i64]) -> usize {
    let num = |d: &Data| d.to_string().parse::<f64>().unwrap_or(f64::NAN);
    (1..g.len())
        .find(|&r| heads.iter().zip(vals).all(|(h, v)| num(&g[r][col_of(g, h)]) == *v as f64))
        .unwrap()
}

/// Model with one wall type of two layers and a second level.
fn layered_model() -> (Model, i64, i64) {
    let mut m = walls_model();
    let two = [
        LayerSpec { material: "brick".into(), thickness: 0.2 },
        LayerSpec { material: "wool".into(), thickness: 0.1 },
    ];
    let t = m.add_wall_type("two", &two).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    (m, t, l2)
}

/// Book of `m` with the user column "my note" on Levels (row of `l2`) and on both layers of `t`.
fn keyed_notes_book(m: &Model, t: i64, l2: i64, tag: &str) -> PathBuf {
    let p = tmp(&format!("{tag}_src"));
    export_xlsx(m, &p).unwrap();
    rewrite(
        &p,
        tag,
        |n, g| {
            let c = g[0].len();
            if n == LEVELS {
                let r = row_with(g, &["id"], &[l2]);
                set(g, 0, c, Data::String("my note".into()));
                set(g, r, c, Data::String("roof".into()));
            } else if n == LAYERS {
                let (a, b) = (row_with(g, &["type_id", "pos"], &[t, 0]), row_with(g, &["type_id", "pos"], &[t, 1]));
                set(g, 0, c, Data::String("my note".into()));
                set(g, a, c, Data::String("outer".into()));
                set(g, b, c, Data::String("inner".into()));
            }
        },
        |_| {},
    )
}

#[test]
fn user_columns_of_keyed_sheets_are_carried_so_not_listed() {
    // Levels and Layers carry the user's columns across a re-export: no loss to confirm
    let (m, t, l2) = layered_model();
    let q = keyed_notes_book(&m, t, l2, "loss_keyed");
    assert_eq!(losses(&m, &q, "loss_keyed", 0), Vec::<String>::new());
    assert_eq!(take_lost_cells(), LostCells::default());
}

#[test]
fn cells_of_removed_keyed_rows_are_reported_lost_not_asked() {
    // a removed level takes its user cells along (reported by the export, never a loss item);
    // a removed layer of a type that still has layers is a loss item: its row has no id to go with
    let (m, t, l2) = layered_model();
    let q = keyed_notes_book(&m, t, l2, "loss_keyed_del");
    m.sql().execute("DELETE FROM wall_type_layer WHERE wtype=?1 AND pos=1", [t]).unwrap();
    m.sql().execute("DELETE FROM level WHERE id=?1", [l2]).unwrap();
    let layer = super::msg::fill_template(super::layer_match::CELLS_UNPLACED.1, &["1", "two", "1", LAYERS]);
    assert_eq!(losses(&m, &q, "loss_keyed_del", 0), vec![layer]);
    let lost = take_lost_cells();
    assert_eq!(lost.cells, 2, "{:?}", lost.notes);
}

#[test]
fn headerless_cells_on_a_keyed_sheet_are_listed() {
    // only headed user columns are carried; a stray cell beside the Levels table is a real loss
    let (m, _, l2) = layered_model();
    let p = tmp("loss_keyed_bare_src");
    export_xlsx(&m, &p).unwrap();
    let q = rewrite(
        &p,
        "loss_keyed_bare",
        |n, g| {
            if n == LEVELS {
                let (c, r) = (g[0].len(), row_with(g, &["id"], &[l2]));
                set(g, r, c, Data::String("stray".into()));
            }
        },
        |_| {},
    );
    assert_eq!(losses(&m, &q, "loss_keyed_bare", 0), vec![format!("1 cells without a column header on sheet '{LEVELS}'")]);
}
