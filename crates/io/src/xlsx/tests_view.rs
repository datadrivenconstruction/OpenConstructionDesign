//! Re-export keeps how the user arranged the app's sheets (sheet_view.rs): row order, hidden
//! rows and columns, widths; filters and sort state it cannot keep are listed as losses.

use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{LayerSpec, Model, WallSpec};
use rust_xlsxwriter::{FilterCondition, Workbook, Worksheet};

use super::sheet_view::{placement, read_book, user_width, View};
use super::tests::tmp;
use super::*;

type Grid = Vec<Vec<Data>>;

/// Walls on two levels: ids (w1 on L1, w2 on L2, w3 on L1).
fn model() -> (Model, [i64; 3]) {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let t = m.add_wall_type("ext", &[LayerSpec { material: "brick".into(), thickness: 0.25 }]).unwrap();
    let mut ids = [0; 3];
    for (i, (x, lv)) in [(0.0, l1), (3.0, l2), (6.0, l1)].into_iter().enumerate() {
        ids[i] = add(&mut m, x, lv, t);
    }
    (m, ids)
}

fn add(m: &mut Model, x: f64, level: i64, wall_type: i64) -> i64 {
    let w = WallSpec {
        a: (x, 0.0),
        b: (x + 2.0, 0.0),
        level,
        base_offset: 0.0,
        height: 3.0,
        wall_type,
        offset0: None,
    };
    m.add_wall(&w).unwrap()
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

fn col_of(g: &Grid, head: &str) -> u16 {
    g[0].iter().position(|d| units::norm(&d.to_string()) == head).unwrap() as u16
}

fn id_of(row: &[Data]) -> i64 {
    match row.first() {
        Some(Data::Float(x)) => *x as i64,
        Some(Data::Int(x)) => *x,
        d => panic!("no id: {d:?}"),
    }
}

/// Copy of `src` as a user would leave it: Walls rows sorted by level, `dress` sets the view of
/// Walls on the written sheet; `info` false drops the Info sheet (a book not made by the app).
fn arranged(src: &Path, name: &str, info: bool, dress: impl Fn(&Grid, &mut Worksheet)) -> PathBuf {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for n in wb.sheet_names() {
        if !info && n == INFO {
            continue;
        }
        let mut grid: Grid = wb.worksheet_range(&n).unwrap().rows().map(|r| r.to_vec()).collect();
        if n == WALLS {
            let lv = col_of(&grid, "level") as usize;
            grid[1..].sort_by_key(|r| r[lv].to_string());
        }
        let ws = out.add_worksheet();
        ws.set_name(&n).unwrap();
        for (i, row) in grid.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                put(ws, i as u32, j as u16, d);
            }
        }
        if n == WALLS {
            dress(&grid, ws);
        }
    }
    let q = tmp(name);
    out.save(&q).unwrap();
    q
}

/// The arrangement of the task: sorted by level, two hidden columns, one hidden row (w3),
/// a wide base offset column.
fn dress_user(g: &Grid, ws: &mut Worksheet) {
    ws.set_column_hidden(col_of(g, "height")).unwrap();
    ws.set_column_hidden(col_of(g, "offset0")).unwrap();
    ws.set_column_width(col_of(g, "base_offset"), 25.0).unwrap();
    // sorted rows are w1, w3 (both L1), w2 (L2): sheet row 2 is w3
    ws.set_row_hidden(2).unwrap();
}

fn walls_ids(p: &Path) -> Vec<i64> {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    let r = wb.worksheet_range(WALLS).unwrap();
    r.rows().skip(1).map(id_of).collect()
}

fn walls_view(p: &Path) -> View {
    read_book(p).remove(WALLS).expect("Walls view")
}

fn reexport(m: &Model, old: &Path, tag: &str) -> (PathBuf, Vec<String>) {
    let new = tmp(tag);
    export_xlsx_families_from(m, old, &new, Units::Metric, &[]).unwrap();
    let lost = reexport_losses(old, &new, &[], 0).unwrap();
    (new, lost)
}

fn hidden_cols(v: &View) -> Vec<String> {
    let mut h: Vec<String> = v.cols.iter().filter(|c| c.1.hidden).map(|c| c.0.clone()).collect();
    h.sort();
    h
}

/// Copy of `src` with `f` applied to the worksheet XML of Walls only.
fn patch_walls(src: &Path, dst: &Path, f: impl Fn(&mut String)) {
    use std::io::{Read, Write};
    let walls = super::loss::sheet_xmls(src, |n| n == WALLS).remove(WALLS).unwrap();
    let mut z = zip::ZipArchive::new(std::fs::File::open(src).unwrap()).unwrap();
    let mut out = zip::ZipWriter::new(std::fs::File::create(dst).unwrap());
    let mut done = false;
    for i in 0..z.len() {
        let mut e = z.by_index(i).unwrap();
        let name = e.name().unwrap().into_owned();
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).unwrap();
        if buf == walls.as_bytes() {
            let mut s = walls.clone();
            f(&mut s);
            buf = s.into_bytes();
            done = true;
        }
        out.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        out.write_all(&buf).unwrap();
    }
    out.finish().unwrap();
    assert!(done, "Walls part not found");
}

#[test]
fn placement_keeps_old_order_appends_new_drops_gone() {
    // old order 3,1,2; the model now has 1,2,4 (3 deleted, 4 new)
    // relative order of the rest unchanged: identity, reported as None
    assert_eq!(placement(&[3, 1, 2], &[Some(1), Some(2), Some(4)]), None);
    let p = placement(&[2, 1], &[Some(1), Some(2), Some(5), Some(4)]).unwrap();
    assert_eq!(p, vec![1, 0, 2, 3]);
    assert_eq!(placement(&[1, 2], &[Some(1), Some(2)]), None);
    // a row without a key goes after the known ones
    assert_eq!(placement(&[2, 1], &[None, Some(1), Some(2)]).unwrap(), vec![2, 1, 0]);
}

#[test]
fn stored_width_converts_back_to_the_same_width() {
    // what the writer stores for a width, see its column writer (7 px digit, 5 px padding)
    let stored = |w: f64| {
        if w < 1.0 {
            ((w * 12.0).round() / 7.0 * 256.0).floor() / 256.0
        } else {
            (((w * 7.0).round() + 5.0) / 7.0 * 256.0).floor() / 256.0
        }
    };
    for w in [8.43, 12.0, 25.0, 40.5, 0.5] {
        let s = stored(w);
        let back = user_width(s);
        assert!((back - w).abs() <= 1.0 / 7.0, "{w} -> {back}");
        // stable: writing the read width again stores the same value
        assert_eq!(stored(back), s, "{w}");
    }
    assert_eq!(user_width(stored(25.0)), 25.0);
}

#[test]
fn reexport_keeps_order_hidden_rows_cols_and_widths() {
    let (m, [w1, w2, w3]) = model();
    let p = tmp("view_src");
    export_xlsx(&m, &p).unwrap();
    let old = arranged(&p, "view_old", true, dress_user);
    // the reader sees the arrangement of the old book
    let ov = walls_view(&old);
    assert_eq!(ov.order, vec![w1, w3, w2]);
    assert!(ov.hidden_rows.contains(&w3) && ov.hidden_rows.len() == 1, "{:?}", ov.hidden_rows);
    assert_eq!(hidden_cols(&ov), vec!["height", "offset0"]);
    assert_eq!(ov.cols["base_offset"].width, Some(25.0));

    let (new, lost) = reexport(&m, &old, "view_new");
    assert_eq!(lost, Vec::<String>::new());
    assert_eq!(walls_ids(&new), vec![w1, w3, w2]);
    let nv = walls_view(&new);
    assert_eq!(nv.order, ov.order);
    assert_eq!(nv.hidden_rows, ov.hidden_rows);
    assert_eq!(hidden_cols(&nv), vec!["height", "offset0"]);
    assert_eq!(nv.cols["base_offset"].width, Some(25.0));
}

#[test]
fn hidden_state_and_widths_survive_two_reexports() {
    let (m, [w1, w2, w3]) = model();
    let p = tmp("view2_src");
    export_xlsx(&m, &p).unwrap();
    let old = arranged(&p, "view2_old", true, dress_user);
    let (b1, l1) = reexport(&m, &old, "view2_b1");
    let (b2, l2) = reexport(&m, &b1, "view2_b2");
    assert!(l1.is_empty() && l2.is_empty(), "{l1:?} {l2:?}");
    let (v1, v2) = (walls_view(&b1), walls_view(&b2));
    assert_eq!(v2.order, vec![w1, w3, w2]);
    assert_eq!(v2.hidden_rows, v1.hidden_rows);
    assert_eq!(v2.cols, v1.cols);
    assert_eq!(v2.cols["base_offset"].width, Some(25.0));
    assert_eq!(hidden_cols(&v2), vec!["height", "offset0"]);
}

#[test]
fn new_element_goes_last_in_model_order() {
    let (mut m, [w1, w2, w3]) = model();
    let p = tmp("view_add_src");
    export_xlsx(&m, &p).unwrap();
    let old = arranged(&p, "view_add_old", true, dress_user);
    let (lv, t): (i64, i64) = m
        .sql()
        .query_row("SELECT level, wtype FROM wall WHERE id=?1", [w1], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    let w4 = add(&mut m, 9.0, lv, t);
    let w5 = add(&mut m, 12.0, lv, t);
    let (new, lost) = reexport(&m, &old, "view_add_new");
    assert!(lost.is_empty(), "{lost:?}");
    assert_eq!(walls_ids(&new), vec![w1, w3, w2, w4, w5]);
    let v = walls_view(&new);
    assert_eq!(v.hidden_rows.iter().copied().collect::<Vec<_>>(), vec![w3]);
}

#[test]
fn deleted_element_row_goes_rest_keeps_order() {
    let (mut m, [w1, w2, w3]) = model();
    let p = tmp("view_del_src");
    export_xlsx(&m, &p).unwrap();
    let old = arranged(&p, "view_del_old", true, dress_user);
    m.delete_wall(w1).unwrap();
    let (new, lost) = reexport(&m, &old, "view_del_new");
    assert!(lost.is_empty(), "{lost:?}");
    assert_eq!(walls_ids(&new), vec![w3, w2]);
    // the hidden row's element deleted: its row goes, no hidden-rows loss is reported
    m.delete_wall(w3).unwrap();
    let (new, lost) = reexport(&m, &old, "view_del_new2");
    assert!(lost.is_empty(), "{lost:?}");
    assert_eq!(walls_ids(&new), vec![w2]);
    assert!(walls_view(&new).hidden_rows.is_empty());
}

#[test]
fn filter_criterion_is_listed_as_loss() {
    let (m, _) = model();
    let p = tmp("view_flt_src");
    export_xlsx(&m, &p).unwrap();
    let old = arranged(&p, "view_flt_old", true, |g, ws| {
        let last = g[0].len() as u16 - 1;
        ws.autofilter(0, 0, g.len() as u32 - 1, last).unwrap();
        ws.filter_column(col_of(g, "level"), &FilterCondition::new().add_list_filter("L1")).unwrap();
    });
    let (_, lost) = reexport(&m, &old, "view_flt_new");
    assert_eq!(lost, vec![format!("filter on sheet '{WALLS}'")]);
    let rows = refusal(&lost, &[]).unwrap();
    assert_eq!(loss_items(&rows).unwrap(), lost);
}

#[test]
fn sort_state_is_listed_as_loss() {
    let (m, [w1, w2, w3]) = model();
    let p = tmp("view_sort_src");
    export_xlsx(&m, &p).unwrap();
    let old = arranged(&p, "view_sort_mid", true, |_, _| {});
    let q = tmp("view_sort_old");
    patch_walls(&old, &q, |s| {
        *s = s.replacen(
            "</sheetData>",
            "</sheetData><sortState ref=\"A2:M4\"><sortCondition ref=\"B2:B4\"/></sortState>",
            1,
        );
    });
    let (new, lost) = reexport(&m, &q, "view_sort_new");
    assert_eq!(lost, vec![format!("sort settings on sheet '{WALLS}'")]);
    // the sorted order itself is kept
    assert_eq!(walls_ids(&new), vec![w1, w3, w2]);
}

#[test]
fn hidden_column_without_header_is_listed() {
    let (m, _) = model();
    let p = tmp("view_spare_src");
    export_xlsx(&m, &p).unwrap();
    let old = arranged(&p, "view_spare_old", true, |g, ws| {
        ws.set_column_hidden(g[0].len() as u16 + 3).unwrap();
    });
    let (_, lost) = reexport(&m, &old, "view_spare_new");
    assert_eq!(lost, vec![format!("hidden columns on sheet '{WALLS}'")]);
}

/// `<col>` elements of Walls with hidden set, as (min, max).
fn hidden_spans(p: &Path) -> Vec<(u32, u32)> {
    let xml = super::loss::sheet_xmls(p, |n| n == WALLS).remove(WALLS).unwrap();
    let num = |t: &str, k: &str| super::loss::attr(t, k).unwrap().parse::<u32>().unwrap();
    super::loss::all_tags(&xml)
        .filter(|(n, t)| *n == "col" && super::loss::attr(t, "hidden") == Some("1"))
        .map(|(_, t)| (num(t, "min"), num(t, "max")))
        .collect()
}

/// Hidden columns of Walls one by one (1-based), whatever `<col>` ranges hold them.
fn hidden_set(p: &Path) -> Vec<u32> {
    let mut v: Vec<u32> = hidden_spans(p).into_iter().flat_map(|(a, b)| a..=b).collect();
    v.sort_unstable();
    v
}

/// The `<cols>` element of Walls, byte for byte.
fn cols_xml(p: &Path) -> String {
    let xml = super::loss::sheet_xmls(p, |n| n == WALLS).remove(WALLS).unwrap();
    let a = xml.find("<cols>").expect("cols");
    let b = xml.find("</cols>").expect("cols end");
    xml[a..b].to_string()
}

#[test]
fn hidden_range_to_the_sheet_end_survives_two_reexports() {
    let (m, _) = model();
    let p = tmp("view_tail_src");
    export_xlsx(&m, &p).unwrap();
    let old = arranged(&p, "view_tail_old", true, |g, ws| {
        ws.set_column_range_hidden(g[0].len() as u16, 16_383).unwrap();
    });
    let tail = hidden_spans(&old);
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].1, 16_384);
    let (new1, lost) = reexport(&m, &old, "view_tail_new1");
    assert!(lost.is_empty(), "{lost:?}");
    assert_eq!(hidden_set(&new1), hidden_set(&old), "the tail is written again");
    let (new2, lost) = reexport(&m, &new1, "view_tail_new2");
    assert!(lost.is_empty(), "second re-export: {lost:?}");
    assert_eq!(hidden_set(&new2), hidden_set(&old));
    assert_eq!(walls_view(&new2).hidden_tail, Some(0));
    assert!(hidden_cols(&walls_view(&new2)).is_empty(), "no model column is hidden");
}

#[test]
fn hidden_range_from_a_model_column_to_the_sheet_end_is_kept() {
    let (m, _) = model();
    let p = tmp("view_tail_part_src");
    export_xlsx(&m, &p).unwrap();
    let old = arranged(&p, "view_tail_part_old", true, |g, ws| {
        let c = g[0].len() as u16 - 2;
        ws.set_column_range_hidden(c, 16_383).unwrap();
    });
    let heads = hidden_cols(&walls_view(&old));
    assert_eq!(heads.len(), 2, "the last two model columns are hidden: {heads:?}");
    let all = hidden_set(&old);
    assert_eq!(all.last(), Some(&16_384));
    let (new1, lost) = reexport(&m, &old, "view_tail_part_new1");
    assert!(lost.is_empty(), "{lost:?}");
    assert_eq!(hidden_cols(&walls_view(&new1)), heads);
    let (new2, lost) = reexport(&m, &new1, "view_tail_part_new2");
    assert!(lost.is_empty(), "second re-export: {lost:?}");
    assert_eq!(hidden_cols(&walls_view(&new2)), heads);
    assert_eq!(hidden_set(&new2), all, "from the model column to the sheet end, nothing else");
}

#[test]
fn hidden_model_columns_are_kept_byte_for_byte() {
    let (m, _) = model();
    let p = tmp("view_keep_src");
    export_xlsx(&m, &p).unwrap();
    let old = arranged(&p, "view_keep_old", true, |g, ws| {
        ws.set_column_hidden(col_of(g, "height")).unwrap();
        ws.set_column_hidden(col_of(g, "offset0")).unwrap();
    });
    let (new1, lost) = reexport(&m, &old, "view_keep_new1");
    assert!(lost.is_empty(), "{lost:?}");
    assert_eq!(hidden_set(&new1), hidden_set(&old));
    assert_eq!(hidden_set(&new1).len(), 2);
    assert_eq!(walls_view(&new1).hidden_tail, None, "no tail is invented");
    let (new2, lost) = reexport(&m, &new1, "view_keep_new2");
    assert!(lost.is_empty(), "{lost:?}");
    assert_eq!(cols_xml(&new2), cols_xml(&new1));
}

#[test]
fn hidden_unheaded_column_inside_the_table_is_still_listed_with_a_tail() {
    let (m, _) = model();
    let p = tmp("view_tail_lost_src");
    export_xlsx(&m, &p).unwrap();
    let old = arranged(&p, "view_tail_lost_old", true, |g, ws| {
        // a user cell two columns past the header widens the table; the column between is hidden
        let n = g[0].len() as u16;
        ws.write_string(1, n + 1, "note").unwrap();
        ws.set_column_hidden(n).unwrap();
        ws.set_column_range_hidden(n + 2, 16_383).unwrap();
    });
    let (_, lost) = reexport(&m, &old, "view_tail_lost_new");
    assert!(lost.contains(&format!("hidden columns on sheet '{WALLS}'")), "{lost:?}");
}

#[test]
fn book_without_info_is_not_ours_and_order_is_the_model_one() {
    let (m, [w1, w2, w3]) = model();
    let p = tmp("view_foreign_src");
    export_xlsx(&m, &p).unwrap();
    let old = arranged(&p, "view_foreign_old", false, dress_user);
    assert!(read_book(&old).is_empty());
    let new = tmp("view_foreign_new");
    export_xlsx_families_from(&m, &old, &new, Units::Metric, &[]).unwrap();
    assert_eq!(reexport_losses(&old, &new, &[], 0).unwrap(), vec![super::loss::NOT_OURS.to_string()]);
    assert_eq!(walls_ids(&new), vec![w1, w2, w3]);
    assert!(walls_view(&new).hidden_rows.is_empty());
}

#[test]
fn first_export_is_unchanged() {
    let (m, [w1, w2, w3]) = model();
    let p = tmp("view_first");
    let _ = std::fs::remove_file(&p);
    export_xlsx(&m, &p).unwrap();
    assert_eq!(walls_ids(&p), vec![w1, w2, w3]);
    let v = walls_view(&p);
    assert!(v.hidden_rows.is_empty() && hidden_cols(&v).is_empty() && v.heights.is_empty());
}

#[test]
fn view_loss_items_have_en_and_ru_texts() {
    let cat = |src: &str| -> std::collections::BTreeMap<String, String> { serde_json::from_str(src).unwrap() };
    let en = cat(include_str!("../../../ui/data/i18n/en/xl_filters.json"));
    let ru = cat(include_str!("../../../ui/data/i18n/ru/xl_filters.json"));
    assert_eq!(en.keys().collect::<Vec<_>>(), ru.keys().collect::<Vec<_>>());
    let tpls = ["filter on sheet '{}'", "sort settings on sheet '{}'", "hidden rows on sheet '{}'", "hidden columns on sheet '{}'"];
    for t in tpls {
        let (k, _) = en.iter().find(|e| e.1 == t).unwrap_or_else(|| panic!("no en text for {t}"));
        assert!(ru[k].chars().any(|c| ('\u{400}'..='\u{4FF}').contains(&c)) && ru[k].contains("{}"), "{k}");
    }
}

#[test]
fn import_of_rearranged_reexport_changes_nothing() {
    let (mut m, _) = model();
    let p = tmp("view_imp_src");
    export_xlsx(&m, &p).unwrap();
    let old = arranged(&p, "view_imp_old", true, dress_user);
    let (new, lost) = reexport(&m, &old, "view_imp_new");
    assert!(lost.is_empty(), "{lost:?}");
    let rep = import_xlsx(&mut m, &new).unwrap();
    assert!(rep.applied && rep.changes.is_empty() && rep.rejected.is_empty(), "{rep:?}");
}
