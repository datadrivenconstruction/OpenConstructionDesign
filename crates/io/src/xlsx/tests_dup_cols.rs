//! A copied column never decides silently what applies: equal copies apply once, a row where they
//! differ keeps the model value and is refused by name, a copy is never a new parameter and is
//! named by the loss check of a re-export.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{CeilingSpec, LayerSpec, Model, PVal, ParamSpec, ParamType, WallSpec};
use rust_xlsxwriter::Workbook;

use super::msg::{fill_template, match_template, DUP_COL_DIFFERS, DUP_COL_ID, DUP_COL_NOTE};
use super::*;

type Grid = Vec<Vec<Data>>;

const HEIGHT_RU: &str = "\u{412}\u{44b}\u{441}\u{43e}\u{442}\u{430}";
const RING: [(f64, f64); 4] = [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)];

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("ddc_xlsx_tests");
    std::fs::create_dir_all(&d).unwrap();
    d.join(format!("{}_dcl_{name}.xlsx", std::process::id()))
}

fn walls() -> (Model, Vec<i64>) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = LayerSpec { material: "brick".into(), thickness: 0.25 };
    let t = m.add_wall_type("ext", &[layer]).unwrap();
    let ids = [0.0, 3.0]
        .map(|x| {
            let w = WallSpec {
                a: (x, 0.0),
                b: (x + 2.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            };
            m.add_wall(&w).unwrap()
        })
        .to_vec();
    (m, ids)
}

fn height(m: &Model, id: i64) -> f64 {
    m.sql().query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0)).unwrap()
}

/// Copies a book cell by cell, `f` edits the grids.
fn edit(src: &Path, name: &str, f: impl Fn(&str, &mut Grid)) -> PathBuf {
    let dst = tmp(name);
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for sheet in wb.sheet_names() {
        let mut g: Grid = wb.worksheet_range(&sheet).unwrap().rows().map(|r| r.to_vec()).collect();
        f(&sheet, &mut g);
        let ws = out.add_worksheet();
        ws.set_name(&sheet).unwrap();
        for (i, row) in g.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                    Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
    }
    out.save(&dst).unwrap();
    dst
}

fn col(g: &Grid, header: &str) -> usize {
    g[0].iter()
        .position(|d| d.to_string() == header)
        .unwrap_or_else(|| panic!("no column {header}: {:?}", g[0]))
}

/// Puts a copy of column `from` at index `to`; returns nothing, every row shifts right of it.
fn copy_col(g: &mut Grid, from: usize, to: usize) {
    for row in g.iter_mut() {
        while row.len() <= from {
            row.push(Data::Empty);
        }
        let v = row[from].clone();
        let at = to.min(row.len());
        row.insert(at, v);
    }
}

fn opts(apply: bool) -> ImportOptions {
    ImportOptions { apply, delete_missing: false, new_params: true }
}

fn differs(rep: &Report) -> Vec<&Rejection> {
    rep.rejected.iter().filter(|r| match_template(DUP_COL_DIFFERS.1, &r.reason).is_some()).collect()
}

fn noted(rep: &Report) -> usize {
    rep.ignored.iter().filter(|i| match_template(DUP_COL_NOTE.1, &i.reason).is_some()).count()
}

/// Copies Height next to itself on the left or the right and lets `f` edit the copied book.
fn copied(
    m: &Model,
    name: &str,
    left: bool,
    f: impl Fn(&mut Grid, usize, usize) + 'static,
) -> PathBuf {
    let p = tmp(&format!("{name}_src"));
    export_xlsx(m, &p).unwrap();
    edit(&p, name, move |s, g| {
        if s == WALLS {
            let h = col(g, "height [m]");
            let (to, orig, copy) = if left { (h, h + 1, h) } else { (h + 1, h, h + 1) };
            copy_col(g, h, to);
            f(g, orig, copy);
        }
    })
}

#[test]
fn equal_copies_apply_once_on_either_side() {
    for left in [true, false] {
        let (mut m, ids) = walls();
        let name = if left { "eq_left" } else { "eq_right" };
        let q = copied(&m, name, left, |g, a, b| {
            g[1][a] = Data::Float(4.0);
            g[1][b] = Data::Float(4.0);
        });
        let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
        assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
        assert_eq!(noted(&rep), 1, "{:?}", rep.ignored);
        assert!(rep.proposals.is_empty(), "{:?}", rep.proposals);
        assert_eq!(rep.changes.iter().filter(|c| c.column.contains("height")).count(), 1);
        assert_eq!(height(&m, ids[0]), 4.0);
    }
}

#[test]
fn untouched_copy_is_noted_and_changes_nothing() {
    for left in [true, false] {
        let (mut m, ids) = walls();
        let name = if left { "same_left" } else { "same_right" };
        let q = copied(&m, name, left, |_, _, _| {});
        let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
        assert!(rep.rejected.is_empty() && rep.proposals.is_empty(), "{rep:?}");
        assert_eq!(noted(&rep), 1, "{:?}", rep.ignored);
        assert_eq!(height(&m, ids[0]), 3.0);
    }
}

#[test]
fn different_copies_block_only_those_cells() {
    for left in [true, false] {
        let (mut m, ids) = walls();
        let name = if left { "diff_left" } else { "diff_right" };
        let q = copied(&m, name, left, |g, a, b| {
            // first wall: the original edited, the copy not; second wall: both agree
            g[1][a] = Data::Float(5.0);
            g[2][a] = Data::Float(4.0);
            g[2][b] = Data::Float(4.0);
            let by = col(g, "by [m]");
            g[1][by] = Data::Float(0.5);
        });
        let preview = import_xlsx_with(&mut m, &q, opts(false)).unwrap();
        assert_eq!(height(&m, ids[0]), 3.0, "preview writes nothing");
        assert_eq!(differs(&preview).len(), 1, "{:?}", preview.rejected);
        assert_eq!(preview.blocking(), 0, "{:?}", preview.rejected);
        let bad = differs(&preview)[0];
        assert_eq!(bad.row, 2);
        assert!(bad.reason.contains("height [m]") && bad.reason.contains("Walls!"), "{bad:?}");
        assert!(preview.proposals.is_empty(), "{:?}", preview.proposals);
        let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
        assert!(rep.applied, "{rep:?}");
        assert_eq!(height(&m, ids[0]), 3.0, "the model value stays");
        assert_eq!(height(&m, ids[1]), 4.0, "the equal row applies");
        let sql = "SELECT \"by\" FROM wall WHERE id=?1";
        let by: f64 = m.sql().query_row(sql, [ids[0]], |r| r.get(0)).unwrap();
        assert_eq!(by, 0.5, "the other cells of the row apply");
        m.undo().unwrap();
        assert_eq!(height(&m, ids[1]), 3.0, "one undo step");
    }
}

#[test]
fn copy_in_another_unit_compares_by_value() {
    let (mut m, ids) = walls();
    let q = copied(&m, "unit", false, |g, a, b| {
        g[0][b] = Data::String("height [mm]".into());
        g[1][a] = Data::Float(4.0);
        g[1][b] = Data::Float(4000.0);
        g[2][b] = Data::Float(3000.0);
    });
    let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
    // the mm column is read as a copy of height, not as a new column
    assert_eq!(noted(&rep), 1, "{:?}", rep.ignored);
    assert!(rep.proposals.is_empty(), "{:?}", rep.proposals);
    assert!(differs(&rep).is_empty(), "{:?}", rep.rejected);
    assert_eq!(height(&m, ids[0]), 4.0);
    // the same number without the unit applied is a real difference
    let (mut m, ids) = walls();
    let q = copied(&m, "unit_off", false, |g, a, b| {
        g[0][b] = Data::String("height [mm]".into());
        g[1][a] = Data::Float(4.0);
        g[1][b] = Data::Float(4.0);
        g[2][b] = Data::Float(3000.0);
    });
    let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
    assert_eq!(differs(&rep).len(), 1, "{:?}", rep.rejected);
    assert_eq!(height(&m, ids[0]), 3.0, "the model value stays");
}

#[test]
fn different_id_copies_stop_the_import() {
    let (mut m, ids) = walls();
    let p = tmp("id_src");
    export_xlsx(&m, &p).unwrap();
    let q = edit(&p, "id", |s, g| {
        if s == WALLS {
            let c = col(g, "id");
            copy_col(g, c, c + 1);
            g[1][c + 1] = Data::Float(9999.0);
            let h = col(g, "height [m]");
            g[1][h] = Data::Float(5.0);
        }
    });
    let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
    assert!(rep.blocking() > 0 && !rep.applied, "{rep:?}");
    assert!(rep.rejected.iter().any(|r| match_template(DUP_COL_ID.1, &r.reason).is_some()));
    assert_eq!(height(&m, ids[0]), 3.0);
}

#[test]
fn styled_book_copy_under_a_caption_is_no_new_parameter() {
    let (mut m, ids) = walls();
    let caps: HashMap<String, String> = [("height".to_string(), HEIGHT_RU.to_string())].into();
    let p = tmp("ru_src");
    let style = Style::professional(caps);
    export_xlsx_styled(&m, &p, &p, Units::Metric, &[], &style).unwrap();
    let shown = format!("{HEIGHT_RU} [m]");
    let t = shown.clone();
    let q = edit(&p, "ru", move |s, g| {
        if s == WALLS {
            let h = col(g, &t);
            copy_col(g, h, h);
            g[1][h] = Data::Float(5.0);
        }
    });
    let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
    assert!(rep.proposals.is_empty(), "{:?}", rep.proposals);
    assert_eq!(noted(&rep), 1, "{:?}", rep.ignored);
    assert_eq!(differs(&rep).len(), 1, "{:?}", rep.rejected);
    assert!(differs(&rep)[0].reason.contains(&shown), "{:?}", rep.rejected);
    assert_eq!(height(&m, ids[0]), 3.0);
}

#[test]
fn parameter_copies_follow_the_same_rule_and_stay_one_parameter() {
    let (mut m, ids) = walls();
    let spec = ParamSpec {
        name: "Fire".into(),
        ptype: ParamType::Text,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    };
    m.add_param(&spec).unwrap();
    let p = tmp("par_src");
    export_xlsx(&m, &p).unwrap();
    let q = edit(&p, "par", |s, g| {
        if s == WALLS {
            let c = col(g, "Fire");
            copy_col(g, c, c);
            g[1][c] = Data::String("EI30".into());
            g[1][c + 1] = Data::String("EI60".into());
            g[2][c] = Data::String("EI90".into());
            g[2][c + 1] = Data::String("EI90".into());
        }
    });
    let rep = import_xlsx_with(&mut m, &q, opts(true)).unwrap();
    assert!(rep.proposals.is_empty(), "{:?}", rep.proposals);
    assert_eq!(differs(&rep).len(), 1, "{:?}", rep.rejected);
    let d = m.param_by_name("Fire").unwrap().unwrap();
    assert_eq!(m.param_value(d.id, "wall", ids[0]).unwrap(), None);
    assert_eq!(m.param_value(d.id, "wall", ids[1]).unwrap(), Some(PVal::Text("EI90".into())));
    assert_eq!(m.params().unwrap().len(), 1);
}

#[test]
fn new_column_typed_twice_is_one_proposal() {
    let (mut m, _) = walls();
    let p = tmp("cand_src");
    export_xlsx(&m, &p).unwrap();
    let q = edit(&p, "cand", |s, g| {
        if s == WALLS {
            let n = g[0].len();
            for (r, row) in g.iter_mut().enumerate() {
                row.resize(n, Data::Empty);
                let (a, b) = if r == 0 { ("Note", "note") } else { ("x", "x") };
                row.push(Data::String(a.into()));
                row.push(Data::String(b.into()));
            }
        }
    });
    let rep = import_xlsx_with(&mut m, &q, opts(false)).unwrap();
    assert_eq!(rep.proposals.len(), 1, "{:?}", rep.proposals);
    assert_eq!(noted(&rep), 1, "{:?}", rep.ignored);
}

fn get_h(m: &Model, id: i64) -> Result<f64, ddc_model::Error> {
    Ok(m.sql().query_row("SELECT height FROM ceiling WHERE id=?1", [id], |r| r.get(0))?)
}

fn set_h(m: &mut Model, id: i64, v: f64) -> Result<(), ddc_model::Error> {
    let (level, slab_type): (i64, i64) = m.sql().query_row(
        "SELECT level,stype FROM ceiling WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let s = CeilingSpec { level, slab_type, height: v, outer: RING.to_vec(), holes: vec![] };
    m.update_ceiling(id, &s)
}

fn ceiling_fams() -> Vec<FamilySheet> {
    vec![FamilySheet {
        sheet: "Ceilings",
        table: "ceiling",
        cols: vec![FamilyCol {
            key: "height",
            label: "Height".into(),
            dim: FamDim::Len,
            labels: vec![],
            lo: 0.0,
            hi: 100.0,
            get: get_h,
            set: set_h,
        }],
        delete: RowDelete::Allowed,
    }]
}

fn ceilings() -> (Model, [i64; 2]) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = LayerSpec { material: "plaster".into(), thickness: 0.02 };
    let st = m.add_slab_type("ceiling", &[layer]).unwrap();
    let ids = [2.75, 2.6].map(|h| {
        let outer = RING.to_vec();
        let s = CeilingSpec { level: lv, slab_type: st, height: h, outer, holes: vec![] };
        m.add_ceiling(&s).unwrap()
    });
    (m, ids)
}

#[test]
fn family_column_and_parameter_of_one_name_stay_apart() {
    let fams = ceiling_fams();
    let (mut m, ids) = ceilings();
    let spec = ParamSpec {
        name: "Height".into(),
        ptype: ParamType::Text,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["ceiling".into()],
    };
    let pid = m.add_param(&spec).unwrap();
    let p = tmp("fam_par_src");
    export_xlsx_families(&m, &p, Units::Metric, &fams).unwrap();
    let head = grid_head(&p, "Ceilings");
    let tagged = format!("Height (#{pid})");
    assert!(head.contains(&tagged), "{head:?}");
    let shown = head.iter().find(|h| units::norm(h) == "height").unwrap().clone();
    let t = tagged.clone();
    let q = edit(&p, "fam_par", move |s, g| {
        if s == "Ceilings" {
            let (a, b) = (col(g, &shown), col(g, &t));
            g[1][a] = Data::Float(3.1);
            g[1][b] = Data::String("tall".into());
        }
    });
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(get_h(&m, ids[0]).unwrap(), 3.1);
    assert_eq!(m.param_value(pid, "ceiling", ids[0]).unwrap(), Some(PVal::Text("tall".into())));
}

#[test]
fn family_sheet_copies_block_only_those_cells() {
    let fams = ceiling_fams();
    let (mut m, ids) = ceilings();
    let p = tmp("fam_src");
    export_xlsx_families(&m, &p, Units::Metric, &fams).unwrap();
    let head = grid_head(&p, "Ceilings");
    let shown = head.iter().find(|h| units::norm(h) == "height").unwrap().clone();
    let q = edit(&p, "fam", move |s, g| {
        if s == "Ceilings" {
            let c = col(g, &shown);
            copy_col(g, c, c);
            g[1][c] = Data::Float(3.0);
            g[2][c] = Data::Float(2.5);
            g[2][c + 1] = Data::Float(2.5);
        }
    });
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams).unwrap();
    assert_eq!(differs(&rep).len(), 1, "{:?}", rep.rejected);
    assert!(rep.proposals.is_empty(), "{:?}", rep.proposals);
    assert_eq!(noted(&rep), 1, "{:?}", rep.ignored);
    assert_eq!(get_h(&m, ids[0]).unwrap(), 2.75);
    assert_eq!(get_h(&m, ids[1]).unwrap(), 2.5);
}

fn grid_head(p: &Path, sheet: &str) -> Vec<String> {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    let range = wb.worksheet_range(sheet).unwrap();
    range.rows().next().unwrap().iter().map(|d| d.to_string()).collect()
}

#[test]
fn reexport_names_a_copy_in_the_loss_report() {
    let (m, _) = walls();
    let q = copied(&m, "loss", false, |g, _, b| {
        g[1][b] = Data::Float(7.0);
    });
    let out = tmp("loss_out");
    export_xlsx(&m, &out).unwrap();
    let lost = reexport_losses(&q, &out, &[], 0).unwrap();
    assert!(lost.iter().any(|l| l.contains("height [m]") && l.contains(WALLS)), "{lost:?}");
}

#[test]
fn reexport_names_a_copied_user_column_too() {
    let (m, _) = walls();
    let p = tmp("loss_user_src");
    export_xlsx(&m, &p).unwrap();
    let q = edit(&p, "loss_user", |s, g| {
        if s == WALLS {
            let n = g[0].len();
            for (r, row) in g.iter_mut().enumerate() {
                row.resize(n, Data::Empty);
                let (a, b) = if r == 0 { ("Note", "note") } else { ("x", "y") };
                row.push(Data::String(a.into()));
                row.push(Data::String(b.into()));
            }
        }
    });
    let out = tmp("loss_user_out");
    export_xlsx(&m, &out).unwrap();
    let lost = reexport_losses(&q, &out, &[], 0).unwrap();
    assert!(lost.iter().any(|l| l.contains("'note'") && l.contains(WALLS)), "{lost:?}");
}

#[test]
fn own_export_has_no_copies_to_report() {
    let (mut m, _) = walls();
    m.add_param(&ParamSpec {
        name: "Height".into(),
        ptype: ParamType::Length,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    })
    .unwrap();
    let a = tmp("own_a");
    export_xlsx(&m, &a).unwrap();
    let b = tmp("own_b");
    export_xlsx(&m, &b).unwrap();
    assert!(reexport_losses(&a, &b, &[], 0).unwrap().is_empty());
    let rep = import_xlsx_with(&mut m, &a, opts(false)).unwrap();
    assert_eq!(noted(&rep), 0, "{:?}", rep.ignored);
}

#[test]
fn dup_col_reasons_have_en_and_ru_texts() {
    let cat = |src: &str| -> std::collections::BTreeMap<String, String> {
        serde_json::from_str(src).unwrap()
    };
    let en = cat(include_str!("../../../ui/data/i18n/en/xl_param_head.json"));
    let ru = cat(include_str!("../../../ui/data/i18n/ru/xl_param_head.json"));
    let cyr = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4FF}').contains(&c));
    for (key, tpl) in msg::DUP_COL_REASONS {
        assert_eq!(en.get(key).map(String::as_str), Some(tpl), "{key}");
        let r = &ru[key];
        assert!(cyr(r) && r != tpl, "{key}: {r}");
        assert_eq!(tpl.matches("{}").count(), r.matches("{}").count(), "{key}");
    }
    // a non-ASCII column name passes through the template into the Russian text
    let text = fill_template(DUP_COL_DIFFERS.1, &[HEIGHT_RU, HEIGHT_RU, "Walls!E2", "Walls!F2"]);
    let args = match_template(DUP_COL_DIFFERS.1, &text).unwrap();
    let shown = fill_template(&ru[DUP_COL_DIFFERS.0], &args);
    assert!(shown.contains(HEIGHT_RU) && shown.contains("Walls!F2"), "{shown}");
    assert!(!shown.contains("{}"), "{shown}");
}
