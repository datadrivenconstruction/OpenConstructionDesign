//! User notes and hidden rows on the layer sheets stay with their own layer when layers are
//! inserted, removed or reordered (layer_match.rs); what finds no layer is in the loss list.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{LayerSpec, Model};
use rust_xlsxwriter::{Workbook, Worksheet};

use super::layer_match::{CELLS_UNPLACED, HIDDEN_UNPLACED};
use super::msg::fill_template;
use super::sheet_view::read_book;
use super::tests::tmp;
use super::*;

const NOTE: &str = "my note";

fn ls(l: &[(&str, f64)]) -> Vec<LayerSpec> {
    l.iter().map(|(m, t)| LayerSpec { material: (*m).into(), thickness: *t }).collect()
}

const EXT: [(&str, f64); 3] = [("brick", 0.25), ("wool", 0.1), ("plaster", 0.02)];

/// Two wall types with the same layers: (model, ext id, int id).
fn model() -> (Model, i64, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let ext = m.add_wall_type("ext", &ls(&EXT)).unwrap();
    let int = m.add_wall_type("int", &ls(&EXT)).unwrap();
    (m, ext, int)
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

fn int(d: Option<&Data>) -> Option<i64> {
    match d? {
        Data::Float(x) => Some(*x as i64),
        Data::Int(x) => Some(*x),
        _ => None,
    }
}

fn col(head: &[Data], name: &str) -> Option<usize> {
    head.iter().position(|d| units::norm(&d.to_string()) == units::norm(name))
}

/// First export of `m`, then a copy as the user left it: a note column on Layers with `notes`
/// by (type, pos), and the rows of `hide` hidden.
fn dressed(m: &Model, tag: &str, notes: &[(i64, i64, &str)], hide: &[(i64, i64)]) -> PathBuf {
    let src = tmp(&format!("{tag}_src"));
    let _ = std::fs::remove_file(&src);
    export_xlsx(m, &src).unwrap();
    let mut wb: Xlsx<_> = open_workbook(&src).unwrap();
    let mut out = Workbook::new();
    for n in wb.sheet_names() {
        let grid: Vec<Vec<Data>> = wb.worksheet_range(&n).unwrap().rows().map(|r| r.to_vec()).collect();
        let ws = out.add_worksheet();
        ws.set_name(&n).unwrap();
        for (i, row) in grid.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                put(ws, i as u32, j as u16, d);
            }
        }
        if n != LAYERS {
            continue;
        }
        let (t, p) = (col(&grid[0], "type_id").unwrap(), col(&grid[0], "pos").unwrap());
        let nc = grid[0].len() as u16;
        ws.write_string(0, nc, NOTE).unwrap();
        for (i, row) in grid.iter().enumerate().skip(1) {
            let key = (int(row.get(t)).unwrap(), int(row.get(p)).unwrap());
            if let Some(x) = notes.iter().find(|x| (x.0, x.1) == key) {
                ws.write_string(i as u32, nc, x.2).unwrap();
            }
            if hide.contains(&key) {
                ws.set_row_hidden(i as u32).unwrap();
            }
        }
    }
    let q = tmp(&format!("{tag}_old"));
    out.save(&q).unwrap();
    q
}

fn reexport(m: &Model, old: &Path, tag: &str) -> (PathBuf, Vec<String>) {
    let new = tmp(tag);
    export_xlsx_families_from(m, old, &new, Units::Metric, &[]).unwrap();
    let lost = reexport_losses(old, &new, &[], 0).unwrap();
    (new, lost)
}

/// Notes of the Layers sheet of `p` by (type, pos), with each row's material.
fn notes(p: &Path) -> BTreeMap<(i64, i64), (String, String)> {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    let g: Vec<Vec<Data>> = wb.worksheet_range(LAYERS).unwrap().rows().map(|r| r.to_vec()).collect();
    let (t, pc, mc) = (col(&g[0], "type_id").unwrap(), col(&g[0], "pos").unwrap(), col(&g[0], "material").unwrap());
    let Some(nc) = col(&g[0], NOTE) else {
        return BTreeMap::new();
    };
    g.iter()
        .skip(1)
        .filter(|r| !matches!(r.get(nc), None | Some(Data::Empty)))
        .map(|r| {
            let key = (int(r.get(t)).unwrap(), int(r.get(pc)).unwrap());
            (key, (r[nc].to_string(), r[mc].to_string()))
        })
        .collect()
}

fn hidden(p: &Path) -> Vec<(i64, i64)> {
    let v = read_book(p).remove(LAYERS).expect("Layers view");
    let mut h: Vec<(i64, i64)> = v.hidden_rows.iter().map(|k| (k / 1_000_000, k % 1_000_000)).collect();
    h.sort_unstable();
    h
}

fn note(n: &str, mat: &str) -> (String, String) {
    (n.to_string(), mat.to_string())
}

#[test]
fn layer_inserted_above_notes_and_hidden_row_follow_their_layer() {
    let (mut m, ext, _) = model();
    let old = dressed(&m, "lm_ins", &[(ext, 0, "check brick"), (ext, 2, "paint")], &[(ext, 1)]);
    let mut l = vec![("render", 0.01)];
    l.extend(EXT);
    m.set_wall_type_layers(ext, "ext", &ls(&l)).unwrap();
    let (new, lost) = reexport(&m, &old, "lm_ins_new");
    assert!(lost.is_empty(), "{lost:?}");
    let got = notes(&new);
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[&(ext, 1)], note("check brick", "brick"));
    assert_eq!(got[&(ext, 3)], note("paint", "plaster"));
    assert_eq!(hidden(&new), vec![(ext, 2)], "the hidden wool row stays hidden");
}

#[test]
fn removed_layer_note_and_hidden_row_are_reported_never_moved() {
    let (mut m, ext, _) = model();
    let old = dressed(&m, "lm_rm", &[(ext, 1, "wool note"), (ext, 2, "paint")], &[(ext, 1)]);
    m.set_wall_type_layers(ext, "ext", &ls(&[EXT[0], EXT[2]])).unwrap();
    let (new, lost) = reexport(&m, &old, "lm_rm_new");
    let got = notes(&new);
    assert_eq!(got.len(), 1, "the wool note lands on no other layer: {got:?}");
    assert_eq!(got[&(ext, 1)], note("paint", "plaster"));
    assert!(hidden(&new).is_empty(), "plaster is not hidden in place of the removed wool");
    let cells = fill_template(CELLS_UNPLACED.1, &["1", "ext", "1", LAYERS]);
    let row = fill_template(HIDDEN_UNPLACED.1, &["ext", "1", LAYERS]);
    assert_eq!(lost, vec![cells, row]);
}

#[test]
fn reordered_layers_take_their_notes_and_hidden_rows_along() {
    let (mut m, ext, _) = model();
    let old = dressed(&m, "lm_swap", &[(ext, 0, "outer"), (ext, 1, "core")], &[(ext, 0)]);
    m.set_wall_type_layers(ext, "ext", &ls(&[EXT[1], EXT[0], EXT[2]])).unwrap();
    let (new, lost) = reexport(&m, &old, "lm_swap_new");
    assert!(lost.is_empty(), "{lost:?}");
    let got = notes(&new);
    assert_eq!(got[&(ext, 0)], note("core", "wool"));
    assert_eq!(got[&(ext, 1)], note("outer", "brick"));
    assert_eq!(hidden(&new), vec![(ext, 1)], "the hidden brick row stays hidden");
}

#[test]
fn types_with_the_same_layers_keep_their_own_notes() {
    let (mut m, ext, int) = model();
    let old = dressed(&m, "lm_two", &[(ext, 0, "ext brick"), (int, 0, "int brick"), (int, 2, "int paint")], &[(int, 1)]);
    let mut l = vec![("render", 0.01)];
    l.extend(EXT);
    m.set_wall_type_layers(ext, "ext", &ls(&l)).unwrap();
    let (new, lost) = reexport(&m, &old, "lm_two_new");
    assert!(lost.is_empty(), "{lost:?}");
    let got = notes(&new);
    assert_eq!(got.len(), 3, "{got:?}");
    assert_eq!(got[&(ext, 1)], note("ext brick", "brick"));
    assert_eq!(got[&(int, 0)], note("int brick", "brick"));
    assert_eq!(got[&(int, 2)], note("int paint", "plaster"));
    assert_eq!(hidden(&new), vec![(int, 1)]);
}

#[test]
fn identical_layers_keep_their_positions() {
    let mut m = Model::open(":memory:").unwrap();
    let t = m.add_wall_type("dry", &ls(&[("board", 0.0125), ("board", 0.0125), ("stud", 0.1)])).unwrap();
    let old = dressed(&m, "lm_same", &[(t, 0, "first"), (t, 1, "second")], &[(t, 1)]);
    m.set_wall_type_layers(t, "dry", &ls(&[("board", 0.0125), ("board", 0.0125), ("stud", 0.075)])).unwrap();
    let (new, lost) = reexport(&m, &old, "lm_same_new");
    assert!(lost.is_empty(), "{lost:?}");
    let got = notes(&new);
    assert_eq!(got[&(t, 0)], note("first", "board"));
    assert_eq!(got[&(t, 1)], note("second", "board"));
    assert_eq!(hidden(&new), vec![(t, 1)]);
}

#[test]
fn unchanged_model_rewrites_the_layer_sheet_byte_for_byte() {
    let (m, ext, int) = model();
    let old = dressed(&m, "lm_same_bytes", &[(ext, 1, "a"), (int, 2, "b")], &[(int, 0)]);
    let (b1, l1) = reexport(&m, &old, "lm_bytes_b1");
    let (b2, l2) = reexport(&m, &b1, "lm_bytes_b2");
    assert!(l1.is_empty() && l2.is_empty(), "{l1:?} {l2:?}");
    assert_eq!(notes(&b1), notes(&old));
    assert_eq!(hidden(&b1), vec![(int, 0)]);
    let xml = |p: &Path| super::loss::sheet_xmls(p, |n| n == LAYERS).remove(LAYERS).unwrap();
    assert_eq!(xml(&b1), xml(&b2));
}

/// A user formula beside a layer row is moved to the row its layer ends on: `final_rows`
/// places layer rows exactly as `arrange` does (old book keys, a new layer below its neighbour).
#[test]
fn user_formula_follows_its_layer_after_an_insert() {
    let (mut m, ext, _) = model();
    let src = tmp("lm_fx_src");
    let _ = std::fs::remove_file(&src);
    export_xlsx(&m, &src).unwrap();
    let mut wb: Xlsx<_> = open_workbook(&src).unwrap();
    let mut out = Workbook::new();
    for n in wb.sheet_names() {
        let grid: Vec<Vec<Data>> = wb.worksheet_range(&n).unwrap().rows().map(|r| r.to_vec()).collect();
        let ws = out.add_worksheet();
        ws.set_name(&n).unwrap();
        for (i, row) in grid.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                put(ws, i as u32, j as u16, d);
            }
        }
        if n != LAYERS {
            continue;
        }
        let letter = (b'A' + col(&grid[0], "material").unwrap() as u8) as char;
        let nc = grid[0].len() as u16;
        ws.write_string(0, nc, "check").unwrap();
        for (i, row) in grid.iter().enumerate().skip(1) {
            let f = rust_xlsxwriter::Formula::new(format!("={letter}{}", i + 1)).set_result(row[col(&grid[0], "material").unwrap()].to_string());
            ws.write_formula(i as u32, nc, f).unwrap();
        }
    }
    let old = tmp("lm_fx_old");
    out.save(&old).unwrap();
    let mut l = vec![("render", 0.01)];
    l.extend(EXT);
    m.set_wall_type_layers(ext, "ext", &ls(&l)).unwrap();
    let (new, _) = reexport(&m, &old, "lm_fx_new");
    let mut wb: Xlsx<_> = open_workbook(&new).unwrap();
    let g: Vec<Vec<Data>> = wb.worksheet_range(LAYERS).unwrap().rows().map(|r| r.to_vec()).collect();
    let fs = wb.worksheet_formula(LAYERS).unwrap();
    let (t, p, fc) = (col(&g[0], "type_id").unwrap(), col(&g[0], "pos").unwrap(), col(&g[0], "check").unwrap());
    let letter = (b'A' + col(&g[0], "material").unwrap() as u8) as char;
    let mut checked = 0;
    for (i, row) in g.iter().enumerate().skip(1) {
        let key = (int(row.get(t)).unwrap(), int(row.get(p)).unwrap());
        let r = i as u32;
        let got = fs.get_value((r, fc as u32)).map(|s| s.trim().trim_start_matches('=').to_string()).filter(|s| !s.is_empty());
        if key == (ext, 0) {
            assert_eq!(got, None, "the new layer has no formula");
        } else {
            assert_eq!(got, Some(format!("{letter}{}", r + 1)), "{key:?}");
            checked += 1;
        }
    }
    assert_eq!(checked, 2 * EXT.len());
}

#[test]
fn unplaced_loss_items_have_en_and_ru_texts() {
    let cat = |src: &str| -> BTreeMap<String, String> { serde_json::from_str(src).unwrap() };
    let en = cat(include_str!("../../../ui/data/i18n/en/xl_layer_match.json"));
    let ru = cat(include_str!("../../../ui/data/i18n/ru/xl_layer_match.json"));
    for (key, tpl) in [CELLS_UNPLACED, HIDDEN_UNPLACED] {
        assert_eq!(en[key], tpl, "{key}");
        let r = &ru[key];
        assert!(r.chars().any(|c| ('\u{400}'..='\u{4FF}').contains(&c)), "{key}");
        assert_eq!(r.matches("{}").count(), tpl.matches("{}").count(), "{key}");
    }
}
