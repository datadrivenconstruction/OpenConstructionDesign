// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Live link to a workbook: project parameters, stages, a read-only sheet, file polling,
//! carrying over user columns, report texts in the locale files.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use calamine::{open_workbook, Data, DataType, Reader, Xlsx};
use ddc_model::{
    Edit, LayerSpec, Model, OpeningSpec, PVal, ParamSet, ParamSpec, ParamType, WallSpec,
};
use rust_xlsxwriter::Workbook;

use crate::ru_fixtures::ru;

use super::*;

type Grid = Vec<Vec<Data>>;

fn model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = LayerSpec {
        material: "brick".into(),
        thickness: 0.2,
    };
    let t = m.add_wall_type("ext", &[layer]).unwrap();
    for (a, b) in [
        ((0., 0.), (5., 0.)),
        ((5., 0.), (5., 4.)),
        ((5., 4.), (0., 4.)),
    ] {
        let w = WallSpec {
            a,
            b,
            level: lv,
            base_offset: 0.0,
            height: 3.0,
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
    (m, t)
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("ddc_xlsx_tests");
    std::fs::create_dir_all(&d).unwrap();
    d.join(format!("{}_sync_{name}.xlsx", std::process::id()))
}

fn count(m: &Model, table: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn export(m: &Model, name: &str) -> PathBuf {
    let p = tmp(name);
    export_xlsx(m, &p).unwrap();
    p
}

/// Workbook overwrite: sheet grid edits and the user's sheets at the end.
fn edit_with(src: &Path, dst: &Path, extra: &[(&str, Grid)], f: impl Fn(&str, &mut Grid)) {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut sheets: Vec<(String, Grid)> = vec![];
    for name in wb.sheet_names() {
        let mut grid: Grid = wb
            .worksheet_range(&name)
            .unwrap()
            .rows()
            .map(|r| r.to_vec())
            .collect();
        f(&name, &mut grid);
        sheets.push((name, grid));
    }
    sheets.extend(extra.iter().map(|(n, g)| (n.to_string(), g.clone())));
    let mut out = Workbook::new();
    for (name, grid) in &sheets {
        let ws = out.add_worksheet();
        ws.set_name(name).unwrap();
        for (i, row) in grid.iter().enumerate() {
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
    out.save(dst).unwrap();
}

fn edit(src: &Path, dst: &Path, f: impl Fn(&str, &mut Grid)) {
    edit_with(src, dst, &[], f)
}

fn col(g: &Grid, header: &str) -> usize {
    g[0].iter()
        .position(|d| d.to_string() == header)
        .unwrap_or_else(|| panic!("no column {header}: {:?}", g[0]))
}

fn set(g: &mut Grid, row: usize, header: &str, v: Data) {
    let c = col(g, header);
    while g[row].len() <= c {
        g[row].push(Data::Empty);
    }
    g[row][c] = v;
}

/// A new column on the right: header and values of rows 1.. in order.
fn add_col(g: &mut Grid, header: &str, vals: &[Data]) {
    let w = g.iter().map(Vec::len).max().unwrap_or(0);
    for r in g.iter_mut() {
        r.resize(w, Data::Empty);
    }
    g[0].push(Data::String(header.into()));
    for (i, r) in g.iter_mut().enumerate().skip(1) {
        r.push(vals.get(i - 1).cloned().unwrap_or(Data::Empty));
    }
}

fn s(t: &str) -> Data {
    Data::String(t.into())
}

fn preview() -> ImportOptions {
    ImportOptions {
        apply: false,
        delete_missing: false,
        new_params: false,
    }
}

fn spec(name: &str, t: ParamType, per_type: bool, cats: &[&str]) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        ptype: t,
        per_type,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: cats.iter().map(|c| c.to_string()).collect(),
    }
}

fn read_sheet(p: &Path, sheet: &str) -> Grid {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    wb.worksheet_range(sheet)
        .unwrap()
        .rows()
        .map(|r| r.to_vec())
        .collect()
}

#[test]
fn new_column_becomes_param_with_values_in_one_undo() {
    let (mut m, _) = model();
    let p = export(&m, "newcol_src");
    let q = tmp("newcol_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            add_col(g, ru("work_code"), &[s(ru("kr1")), s(ru("kr2")), s(ru("kr3"))]);
        }
    });
    let pre = import_xlsx_with(&mut m, &q, preview()).unwrap();
    assert!(pre.rejected.is_empty(), "{:?}", pre.rejected);
    assert_eq!(pre.proposals.len(), 1, "{:?}", pre.proposals);
    let pr = &pre.proposals[0];
    assert_eq!(
        (pr.spec.name.as_str(), pr.spec.ptype, pr.spec.per_type),
        (ru("work_code"), ParamType::Text, false)
    );
    assert_eq!(pr.spec.categories, vec!["wall".to_string()]);
    assert_eq!(pr.values.len(), 3);
    assert_eq!(
        (pr.type_key(), pr.category_keys()),
        ("xl.ptype.text", vec!["xl.cat.wall"])
    );
    assert!(pre.edits.is_empty() && pre.counts() == Counts::default());
    assert!(pre.batch().is_empty(), "not accepted: nothing to apply");
    let mut pre = pre;
    pre.proposals[0].accept = true;
    let b = pre.batch();
    assert_eq!(b.len(), 4);
    assert!(matches!(&b[0], Edit::NewParam(sp) if sp.name == ru("work_code")));
    assert!(b[1..]
        .iter()
        .all(|e| matches!(e, Edit::ParamValue { kind, .. } if kind == "wall")));

    let ops = count(&m, "op");
    let o = ImportOptions {
        apply: true,
        new_params: true,
        ..preview()
    };
    let rep = import_xlsx_with(&mut m, &q, o).unwrap();
    assert!(rep.applied, "{:?}", rep.rejected);
    assert_eq!(count(&m, "op"), ops + 1, "one operation, one undo");
    let d = m.param_by_name(ru("work_code")).unwrap().unwrap();
    assert_eq!(
        m.param_value(d.id, "wall", 1).unwrap(),
        Some(PVal::Text(ru("kr1").into()))
    );
    // the column is now a parameter: re-import yields no edits and no proposals
    let again = import_xlsx(&mut m, &q).unwrap();
    assert!(
        again.applied && again.changes.is_empty() && again.proposals.is_empty(),
        "{again:?}"
    );
    assert_eq!(count(&m, "op"), ops + 1);
    m.undo().unwrap();
    assert!(m.param_by_name(ru("work_code")).unwrap().is_none());
}

#[test]
fn proposal_types_follow_header_units_and_values() {
    let (mut m, _) = model();
    let p = export(&m, "guess_src");
    let q = tmp("guess_dst");
    let f = Data::Float;
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            add_col(g, ru("price_rub"), &[f(1500.0), f(1600.5), f(10.0)]);
            add_col(g, ru("hdr_qty"), &[f(1.0), s("2"), f(3.0)]);
            add_col(g, ru("finish_mm"), &[f(20.0), f(25.0), Data::Empty]);
            add_col(g, ru("note_word"), &[s(ru("yes_word")), f(2.0), Data::Empty]);
            // codes and dates in text are not numbers: "007", "01.10", "1,50" would lose their form
            add_col(g, ru("code_word"), &[s("007"), s("01.10"), s("1,50")]);
            // a point book (the default export); `1,5` there awaits a decimal rule
            add_col(g, ru("share_word"),&[s("1.5"), f(2.0), Data::Empty]);
        }
    });
    let rep = import_xlsx_with(&mut m, &q, preview()).unwrap();
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    let by = |n: &str| rep.proposals.iter().find(|p| p.spec.name == n).unwrap();
    assert_eq!(
        (by(ru("price_word")).spec.ptype, by(ru("price_word")).spec.unit.as_str()),
        (ParamType::Number, ru("rub_unit"))
    );
    assert_eq!(by(ru("hdr_qty")).spec.ptype, ParamType::Int);
    let o = by(ru("finish_word"));
    assert_eq!((o.spec.ptype, o.values.len()), (ParamType::Length, 2));
    assert!((o.values[0].3.as_f64().unwrap() - 0.02).abs() < 1e-12);
    assert_eq!(by(ru("note_word")).spec.ptype, ParamType::Text);
    let k = by(ru("code_word"));
    assert_eq!(k.spec.ptype, ParamType::Text);
    assert_eq!(k.values[0].3, PVal::Text("007".into()));
    assert_eq!(k.values[1].3, PVal::Text("01.10".into()));
    assert_eq!(by(ru("share_word")).spec.ptype, ParamType::Number);
    assert_eq!(super::extra::iso_date(45566.5), "2024-10-01 12:00");
    assert_eq!(
        (by(ru("price_word")).type_key(), by(ru("price_word")).spec.unit.as_str()),
        ("xl.ptype.number", ru("rub_unit"))
    );
    assert_eq!(by(ru("finish_word")).type_key(), "xl.ptype.length");
    assert_eq!(by(ru("finish_word")).category_keys(), vec!["xl.cat.wall"]);
}

#[test]
fn project_params_roundtrip_and_reimport_as_param_values() {
    let (mut m, wt) = model();
    let code = m
        .add_param(&spec(ru("code_word"), ParamType::Text, false, &["wall"]))
        .unwrap();
    let off = m
        .add_param(&spec(ru("offset_word"), ParamType::Length, false, &["wall", "door"]))
        .unwrap();
    let mark = m
        .add_param(&spec(ru("type_mark"), ParamType::Text, true, &["wall"]))
        .unwrap();
    let pv = |param, kind: &str, id, v| ParamSet {
        param,
        kind: kind.into(),
        id,
        value: Some(v),
    };
    m.set_param_values(&[
        pv(code, "wall", 1, PVal::Text("101".into())),
        pv(off, "wall", 2, PVal::Num(0.25)),
        pv(off, "opening", 1, PVal::Num(0.1)),
        pv(mark, "wall_type", wt, PVal::Text(ru("ns1").into())),
    ])
    .unwrap();
    for u in [Units::Metric, Units::Imperial] {
        let p = tmp(&format!("prm_{}", u.name()));
        export_xlsx_units(&m, &p, u).unwrap();
        let ops = count(&m, "op");
        let rep = import_xlsx(&mut m, &p).unwrap();
        assert!(
            rep.applied
                && rep.changes.is_empty()
                && rep.rejected.is_empty()
                && rep.proposals.is_empty()
                && rep.ignored.is_empty(),
            "{u:?} {rep:?}"
        );
        assert_eq!(count(&m, "op"), ops);
    }
    let p = tmp("prm_ft");
    export_xlsx_units(&m, &p, Units::Imperial).unwrap();
    let g = read_sheet(&p, WALLS);
    let c = col(&g, ru("offset_ft"));
    assert!((g[2][c].as_f64().unwrap() - 0.25 / 0.3048).abs() < 1e-12);
    assert_eq!(g[1][col(&g, ru("code_word"))].to_string(), "101");
    let t = read_sheet(&p, TYPES);
    assert_eq!(t[1][col(&t, ru("type_mark"))].to_string(), ru("ns1"));
    assert!(read_sheet(&p, OPENINGS)[0]
        .iter()
        .any(|h| h.to_string() == ru("offset_ft")));

    let q = tmp("prm_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, ru("code_word"), s("B-2"));
            set(g, 2, ru("offset_ft"), s("6\""));
            set(g, 3, ru("code_word"), s("C-3"));
        }
        if sh == TYPES {
            set(g, 1, ru("type_mark"), s(ru("ns2")));
        }
    });
    let ops = count(&m, "op");
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{:?}", rep.rejected);
    assert_eq!(rep.changes.len(), 4, "{:?}", rep.changes);
    assert_eq!(count(&m, "op"), ops + 1);
    assert_eq!(
        m.param_value(code, "wall", 1).unwrap(),
        Some(PVal::Text("B-2".into()))
    );
    assert_eq!(
        m.param_value(code, "wall", 3).unwrap(),
        Some(PVal::Text("C-3".into()))
    );
    let v = m.param_value(off, "wall", 2).unwrap().unwrap();
    assert!((v.as_f64().unwrap() - 0.1524).abs() < 1e-12);
    assert_eq!(
        m.param_value(mark, "wall_type", wt).unwrap(),
        Some(PVal::Text(ru("ns2").into()))
    );
    m.undo().unwrap();
    assert_eq!(m.param_value(code, "wall", 3).unwrap(), None);

    // a value of the wrong type — rejection with the cell address, nothing applied
    let q = tmp("prm_bad");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, ru("offset_ft"), s(ru("wide_word")));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.len() == 1, "{rep:?}");
    assert_eq!(rep.rejected[0].row, 2);
    assert!(rep.rejected[0].reason.starts_with("not a number or length"));
}

#[test]
fn phase_columns_roundtrip_and_edit_as_phase_edits() {
    let (mut m, _) = model();
    let p = export(&m, "ph_src");
    let ops = count(&m, "op");
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(rep.applied && rep.changes.is_empty(), "{rep:?}");
    assert_eq!(count(&m, "op"), ops);
    let idx = m.phase_index().unwrap();
    assert!(idx.phases.len() >= 2);
    let (first, last) = (
        idx.phases.first().unwrap().clone(),
        idx.phases.last().unwrap().clone(),
    );
    let g = read_sheet(&p, WALLS);
    let e1 = m.element_phase("wall", 1).unwrap();
    let e2 = m.element_phase("wall", 2).unwrap();
    assert_eq!(
        g[1][col(&g, PHASE_CREATED)].to_string(),
        idx.name(e1.created)
    );
    assert!(read_sheet(&p, OPENINGS)[0]
        .iter()
        .any(|h| h.to_string() == PHASE_DEMOLISHED));

    let q = tmp("ph_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, PHASE_DEMOLISHED, s(&last.name));
            set(g, 2, PHASE_CREATED, s(&first.name));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{:?}", rep.rejected);
    let phase_edits = rep
        .edits
        .iter()
        .filter(|e| matches!(e, Edit::Phase { .. }))
        .count();
    assert_eq!(phase_edits, 1 + usize::from(e2.created != first.id));
    assert_eq!(
        m.element_phase("wall", 1).unwrap().demolished,
        Some(last.id)
    );
    assert_eq!(m.element_phase("wall", 2).unwrap().created, first.id);
    m.undo().unwrap();
    assert_eq!(m.element_phase("wall", 1).unwrap(), e1);

    let q = tmp("ph_bad");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, PHASE_CREATED, s(ru("no_such")));
            set(g, 2, PHASE_CREATED, s(&last.name));
            set(g, 2, PHASE_DEMOLISHED, s(&first.name));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.blocking() == 0);
    let r1 = rep.rejected.iter().find(|r| r.row == 2).unwrap();
    assert_eq!(r1.reason, format!("unknown phase '{}'", ru("no_such")));
    let r2 = rep.rejected.iter().find(|r| r.row == 3).unwrap();
    assert!(r2.reason.contains("precedes"), "{}", r2.reason);
}

#[test]
fn quantities_sheet_is_read_only_and_edits_are_reported_not_applied() {
    let (mut m, _) = model();
    let p = export(&m, "qty_src");
    {
        use std::io::Read;
        let f = std::fs::File::open(&p).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        let mut found = false;
        for i in 0..z.len() {
            let mut e = z.by_index(i).unwrap();
            if e.name().unwrap().starts_with("xl/comments") {
                let mut t = String::new();
                e.read_to_string(&mut t).unwrap();
                found |= t.contains(crate::words::text("en", "io.w.xl_read_only").split(':').next().unwrap());
            }
        }
        assert!(found, "read-only note on the sheet");
    }
    let q = tmp("qty_dst");
    edit(&p, &q, |sh, g| {
        if sh == QUANTITIES {
            set(g, 1, "volume [m3] (read-only)", Data::Float(99.0));
            g.push(vec![Data::Float(999.0), Data::Float(0.0), s("x")]);
        }
    });
    let ops = count(&m, "op");
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(
        rep.applied && rep.rejected.is_empty() && rep.changes.is_empty(),
        "{rep:?}"
    );
    assert_eq!(count(&m, "op"), ops);
    assert_eq!(rep.ignored.len(), 2, "{:?}", rep.ignored);
    let ig = &rep.ignored[0];
    assert_eq!(
        (ig.address().as_str(), ig.new.as_str()),
        ("Quantities!E2", "99")
    );
    assert_eq!(ig.reason, "read-only sheet; not applied");
    assert_eq!(
        rep.ignored[1].row,
        rep.ignored[0].row + count(&m, "wall_qty") as u32
    );
}

#[test]
fn stamp_change_detection_and_poll_throttle() {
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let a = Stamp {
        mtime: Some(t),
        len: 10,
    };
    let b = Stamp {
        mtime: Some(t + Duration::from_secs(1)),
        ..a
    };
    assert!(!book_changed(Some(a), Some(a)));
    assert!(book_changed(Some(a), Some(b)));
    assert!(book_changed(Some(a), Some(Stamp { len: 11, ..a })));
    assert!(
        !book_changed(Some(a), None),
        "file briefly missing while saved"
    );
    assert!(book_changed(None, Some(a)));

    let mut w = Watch::new(Path::new("no such workbook.xlsx"));
    assert_eq!(w.seen, None);
    w.mark(Some(a));
    let t0 = Instant::now();
    assert_eq!(w.check(t0, |_| Some(a)), None);
    assert_eq!(
        w.check(t0 + Duration::from_millis(500), |_| Some(b)),
        None,
        "polled at most once per second"
    );
    assert_eq!(w.check(t0 + POLL, |_| Some(b)), Some(b));
    w.mark(Some(b));
    assert_eq!(w.check(t0 + POLL * 2, |_| Some(b)), None);

    let p = tmp("watch");
    std::fs::write(&p, b"1").unwrap();
    let mut w = Watch::new(&p);
    std::fs::write(&p, b"22").unwrap();
    assert!(w.poll(Instant::now()).is_some(), "size changed");
}

#[test]
fn reexport_keeps_user_columns_by_id_and_user_sheets() {
    let (m, _) = model();
    let p = export(&m, "keep_src");
    let q = tmp("keep_book");
    let summary: Grid = vec![vec![s(ru("total_lc")), Data::Float(42.0)]];
    edit_with(&p, &q, &[(ru("summary_word"), summary)], |sh, g| {
        if sh == WALLS {
            add_col(g, ru("remark_word"), &[s("a"), Data::Empty, s("c")]);
            g.swap(1, 3); // the user sorted the rows
        }
    });
    export_xlsx(&m, &q).unwrap();
    let g = read_sheet(&q, WALLS);
    let (ci, cn) = (col(&g, "id"), col(&g, ru("remark_word")));
    let note = |id: f64| {
        let r = g.iter().find(|r| r[ci].as_f64() == Some(id)).unwrap();
        r.get(cn).map_or(String::new(), |d| d.to_string())
    };
    assert_eq!(
        (note(1.0), note(2.0), note(3.0)),
        ("a".into(), String::new(), "c".into())
    );
    let sv = read_sheet(&q, ru("summary_word"));
    assert_eq!(sv[0][1].as_f64(), Some(42.0));

    assert_eq!(
        super::extra::shift_row("=H5*1.2+SUM(A5:B5)-$C$5+C$5+Walls!H5", 5, 2),
        "=H2*1.2+SUM(A2:B2)-$C$5+C$5+Walls!H5"
    );
    assert_eq!(super::extra::shift_row("=\"A5\"&B5", 5, 7), "=\"A5\"&B7");
}

/// Source string literals that look like report text (`{…}` → `{}`).
fn report_literals(src: &str) -> Vec<String> {
    let mut out = vec![];
    for line in src.lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let b: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < b.len() {
            if b[i] != '"' {
                i += 1;
                continue;
            }
            let (mut j, mut lit) = (i + 1, String::new());
            while j < b.len() && b[j] != '"' {
                // a `\u{..}` escape is the character it names, as the compiled text has it
                if b[j] == '\\' && b.get(j + 1) == Some(&'u') && b.get(j + 2) == Some(&'{') {
                    let end = (j + 3..b.len()).find(|&k| b[k] == '}');
                    let hex: Option<String> = end.map(|e| b[j + 3..e].iter().collect());
                    let ch = hex.and_then(|h| u32::from_str_radix(&h, 16).ok());
                    if let (Some(e), Some(c)) = (end, ch.and_then(char::from_u32)) {
                        lit.push(c);
                        j = e + 1;
                        continue;
                    }
                }
                if b[j] == '\\' && j + 1 < b.len() {
                    if b[j + 1] != '"' {
                        lit.push('\\');
                    }
                    lit.push(b[j + 1]);
                    j += 2;
                    continue;
                }
                lit.push(b[j]);
                j += 1;
            }
            i = j + 1;
            let Some(c0) = lit.chars().next() else {
                continue;
            };
            if !(c0.is_ascii_lowercase() || c0 == '\'' || c0 == '{') || !lit.contains(' ') {
                continue;
            }
            let (mut norm, mut bare, mut depth) = (String::new(), String::new(), 0);
            for ch in lit.chars() {
                match ch {
                    '{' => {
                        if depth == 0 {
                            norm.push_str("{}");
                        }
                        depth += 1;
                    }
                    '}' if depth > 0 => depth -= 1,
                    _ if depth == 0 => {
                        norm.push(ch);
                        bare.push(ch);
                    }
                    _ => {}
                }
            }
            let words = bare
                .split(|c: char| !c.is_ascii_lowercase())
                .filter(|w| w.len() >= 2)
                .count();
            if words >= 2 {
                out.push(norm);
            }
        }
    }
    out
}

/// Locale entries of one language: key -> text.
fn locale(src: &str) -> std::collections::BTreeMap<String, String> {
    serde_json::from_str(src).expect("locale file is valid JSON")
}

#[test]
fn every_report_text_exists_in_russian_and_english() {
    let mut en = locale(include_str!("../../../ui/data/i18n/en/xl.json"));
    let mut ru = locale(include_str!("../../../ui/data/i18n/ru/xl.json"));
    // the delete-safety report texts live in their own locale file
    en.extend(locale(include_str!("../../../ui/data/i18n/en/delsafe.json")));
    ru.extend(locale(include_str!("../../../ui/data/i18n/ru/delsafe.json")));
    assert_eq!(
        en.keys().collect::<Vec<_>>(),
        ru.keys().collect::<Vec<_>>(),
        "en and ru locale keys differ"
    );
    let templates: Vec<&String> = en
        .iter()
        .filter(|(k, _)| k.starts_with("xl.msg."))
        .map(|(_, v)| v)
        .collect();
    for (k, e) in &en {
        let r = &ru[k];
        assert!(!e.trim().is_empty() && !r.trim().is_empty(), "{k}");
        assert_ne!(e, r, "{k}");
        assert_eq!(e.matches("{}").count(), r.matches("{}").count(), "{k}");
        assert!(
            r.chars().any(|c| (0x400..=0x4FF).contains(&(c as u32))),
            "{k}: ru text has no Cyrillic"
        );
    }
    let mut seen = 0;
    // family.rs: the helpers after the sheet reader are test tools, not report texts
    let family = include_str!("family.rs");
    let family = family.split("/// Ids of the first column").next().unwrap_or(family);
    // column headers of the family sheets are book texts, not report texts
    // and a cell number format is a book text too
    const HEADS: [&str; 5] = [
        "{} (type id)",
        "level (read-only)",
        "name (read-only)",
        "kind (read-only)",
        "yyyy-mm-dd hh:mm:ss",
    ];
    let units_src = include_str!("units.rs");
    let units_src = units_src.split("#[cfg(test)]").next().unwrap_or(units_src);
    // extra.rs keeps its own tests at the end: their literals are not report texts
    let extra_src = include_str!("extra.rs");
    let extra_src = extra_src.split("#[cfg(test)]").next().unwrap_or(extra_src);
    for src in [include_str!("import.rs"), extra_src, family, units_src] {
        for lit in report_literals(src).into_iter().filter(|l| !HEADS.contains(&l.as_str())) {
            seen += 1;
            assert!(
                templates.iter().any(|t| **t == lit),
                "no locale entry for {lit:?}"
            );
        }
    }
    assert!(seen > 40, "scanner found {seen} texts");
    for t in ParamType::ALL {
        assert!(en.contains_key(ptype_key(t)), "{t:?}");
    }
    for (c, _) in ddc_model::PARAM_CATEGORIES {
        assert!(en.contains_key(category_key(c)), "{c}");
        assert_ne!(category_key(c), "xl.cat.other", "{c}");
    }
    assert_eq!(
        match_template("unknown wall id {}", "unknown wall id 7"),
        Some(vec!["7"])
    );
    assert_eq!(fill_template("a {} b {}", &["1", "2"]), "a 1 b 2");
}

// ---- round trip: baseline, conflicts, never silent ----

fn wall_h(m: &Model, id: i64) -> f64 {
    m.sql()
        .query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

fn set_wall_h(m: &Model, id: i64, h: f64) {
    m.sql()
        .execute(
            "UPDATE wall SET height=?1 WHERE id=?2",
            rusqlite::params![h, id],
        )
        .unwrap();
}

fn rename_sheet(src: &Path, dst: &Path, from: &str, to: &str) {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for name in wb.sheet_names() {
        let grid: Grid = wb
            .worksheet_range(&name)
            .unwrap()
            .rows()
            .map(|r| r.to_vec())
            .collect();
        let ws = out.add_worksheet();
        ws.set_name(if name == from { to } else { name.as_str() })
            .unwrap();
        for (i, row) in grid.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::String(t) => ws.write_string(i, j, t).map(|_| ()),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
    }
    out.save(dst).unwrap();
}

fn apply_opts(delete_missing: bool) -> ImportOptions {
    ImportOptions {
        apply: true,
        delete_missing,
        new_params: false,
    }
}

/// A new wall row at the end of the Walls grid.
fn push_wall(g: &mut Grid) {
    let n = g[0].len();
    g.push(vec![Data::Empty; n]);
    let r = g.len() - 1;
    set(g, r, "level", s("L1"));
    set(g, r, "type", s("ext"));
    for (h, v) in [
        ("ax [m]", 0.0),
        ("ay [m]", 5.0),
        ("bx [m]", 5.0),
        ("by [m]", 5.0),
        ("height [m]", 3.0),
    ] {
        set(g, r, h, Data::Float(v));
    }
}

#[test]
fn export_carries_revision_and_hidden_baseline() {
    let (m, _) = model();
    let p = export(&m, "base_sheets");
    let info = read_sheet(&p, INFO);
    assert_eq!(info[1][0].to_string(), "revision");
    let base = read_sheet(&p, BASELINE);
    assert!(base.len() > 4, "a header row per sheet and a row per element");
    let wb: Xlsx<_> = open_workbook(&p).unwrap();
    let hidden = wb
        .sheets_metadata()
        .iter()
        .any(|s| s.name == BASELINE && s.visible == calamine::SheetVisible::Hidden);
    assert!(hidden, "the baseline sheet is hidden");
}

#[test]
fn model_edit_after_export_is_kept_not_reverted() {
    let (mut m, _) = model();
    let p = export(&m, "late_model");
    set_wall_h(&m, 1, 3.5);
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert!(rep.changes.is_empty() && rep.conflicts.is_empty(), "{rep:?}");
    assert_eq!(rep.model_newer, 1, "the kept model edit is counted");
    assert!((wall_h(&m, 1) - 3.5).abs() < 1e-12, "the editor change stays");
}

#[test]
fn cell_changed_in_book_and_model_is_a_conflict_never_applied() {
    let (mut m, _) = model();
    let p = export(&m, "conf_src");
    let q = tmp("conf_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, "height [m]", Data::Float(4.0));
            set(g, 2, "height [m]", Data::Float(3.2));
        }
    });
    set_wall_h(&m, 1, 3.5);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(rep.conflicts.len(), 1, "{:?}", rep.conflicts);
    let c = &rep.conflicts[0];
    assert_eq!((c.address().as_str(), c.id), ("Walls!I2", Some(1)));
    assert_eq!(
        (c.base.as_str(), c.book.as_str(), c.model.as_str()),
        ("3", "4", "3.5")
    );
    assert_eq!(rep.counts().changed, 1, "only the book-only edit");
    assert!((wall_h(&m, 1) - 3.5).abs() < 1e-12, "conflict not applied");
    assert!((wall_h(&m, 2) - 3.2).abs() < 1e-12, "book-only edit applied");
}

#[test]
fn new_row_param_value_survives_apply() {
    let (mut m, _) = model();
    m.apply_edits(&[Edit::NewParam(spec(
        "Code",
        ParamType::Text,
        false,
        &["wall"],
    ))])
    .unwrap();
    let p = export(&m, "newrow_src");
    let q = tmp("newrow_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            push_wall(g);
            let r = g.len() - 1;
            set(g, r, "Code", s("N-1"));
        }
    });
    let pre = import_xlsx_with(&mut m, &q, preview()).unwrap();
    assert!(pre.ignored.is_empty(), "{:?}", pre.ignored);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(count(&m, "wall"), 4);
    let d = m.param_by_name("Code").unwrap().unwrap();
    assert_eq!(
        m.param_value(d.id, "wall", 4).unwrap(),
        Some(PVal::Text("N-1".into()))
    );
    // the re-export writes it back from the model
    export_xlsx(&m, &q).unwrap();
    let g = read_sheet(&q, WALLS);
    let c = col(&g, "Code");
    assert_eq!(g[4][c].to_string(), "N-1");
    m.undo().unwrap();
    assert_eq!(count(&m, "wall"), 3);
}

fn push_opening(g: &mut Grid, kind: &str) {
    let n = g[0].len();
    g.push(vec![Data::Empty; n]);
    let r = g.len() - 1;
    set(g, r, "wall_id", Data::Float(2.0));
    for (h, v) in [
        ("along [m]", 1.0),
        ("width [m]", 0.9),
        ("sill [m]", 0.0),
        ("height [m]", 2.1),
    ] {
        set(g, r, h, Data::Float(v));
    }
    set(g, r, "kind", s(kind));
}

#[test]
fn new_door_row_user_param_applies() {
    let (mut m, _) = model();
    m.apply_edits(&[Edit::NewParam(spec(
        "Fire rating",
        ParamType::Text,
        false,
        &["door"],
    ))])
    .unwrap();
    let p = export(&m, "newdoor_src");
    let q = tmp("newdoor_dst");
    edit(&p, &q, |sh, g| {
        if sh == OPENINGS {
            push_opening(g, "door");
            let r = g.len() - 1;
            set(g, r, "Fire rating", s("EI30"));
        }
    });
    let pre = import_xlsx_with(&mut m, &q, preview()).unwrap();
    assert!(pre.ignored.is_empty(), "{:?}", pre.ignored);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(count(&m, "opening"), 2);
    let d = m.param_by_name("Fire rating").unwrap().unwrap();
    assert_eq!(
        m.param_value(d.id, "opening", 2).unwrap(),
        Some(PVal::Text("EI30".into()))
    );
    // the re-export keeps the cell
    export_xlsx(&m, &q).unwrap();
    let g = read_sheet(&q, OPENINGS);
    let c = col(&g, "Fire rating");
    assert_eq!(g[2][c].to_string(), "EI30");
    m.undo().unwrap();
    assert_eq!(count(&m, "opening"), 1);
}

#[test]
fn new_window_row_door_only_param_ignored_with_reason() {
    let (mut m, _) = model();
    m.apply_edits(&[Edit::NewParam(spec(
        "Fire rating",
        ParamType::Text,
        false,
        &["door"],
    ))])
    .unwrap();
    let p = export(&m, "newwin_src");
    let q = tmp("newwin_dst");
    edit(&p, &q, |sh, g| {
        if sh == OPENINGS {
            push_opening(g, "window");
            let r = g.len() - 1;
            set(g, r, "Fire rating", s("EI30"));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(count(&m, "opening"), 2, "the window row itself is created");
    let d = m.param_by_name("Fire rating").unwrap().unwrap();
    assert_eq!(m.param_value(d.id, "opening", 2).unwrap(), None);
    let ig = rep
        .ignored
        .iter()
        .find(|i| i.sheet == OPENINGS && i.column == "Fire rating")
        .unwrap_or_else(|| panic!("no ignored cell: {:?}", rep.ignored));
    assert!(ig.reason.contains("not bound to this category"), "{}", ig.reason);
}

#[test]
fn new_row_value_of_an_accepted_new_column_is_applied_too() {
    let (mut m, _) = model();
    let p = export(&m, "newcol_row_src");
    let q = tmp("newcol_row_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            push_wall(g);
            add_col(g, "Tag", &[s("a"), s("b"), s("c"), s("d")]);
        }
    });
    let o = ImportOptions {
        new_params: true,
        ..apply_opts(false)
    };
    let rep = import_xlsx_with(&mut m, &q, o).unwrap();
    assert!(rep.applied, "{rep:?}");
    let d = m.param_by_name("Tag").unwrap().unwrap();
    assert_eq!(
        m.param_value(d.id, "wall", 4).unwrap(),
        Some(PVal::Text("d".into()))
    );
    assert_eq!(
        m.param_value(d.id, "wall", 1).unwrap(),
        Some(PVal::Text("a".into()))
    );
}

#[test]
fn removed_row_is_reported_when_delete_is_off() {
    let (mut m, _) = model();
    let p = export(&m, "rm_src");
    let q = tmp("rm_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            g.pop();
        }
    });
    let rep = import_xlsx_with(&mut m, &q, apply_opts(false)).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(rep.removed_count(), 1);
    assert_eq!((rep.removed[0].sheet.as_str(), rep.removed[0].id), (WALLS, 3));
    assert_eq!(count(&m, "wall"), 3, "not deleted unless asked");
    let rep = import_xlsx_with(&mut m, &q, apply_opts(true)).unwrap();
    assert!(rep.removed.is_empty() && rep.counts().deleted == 1, "{rep:?}");
    assert_eq!(count(&m, "wall"), 2);
}

#[test]
fn element_made_after_export_is_not_deleted_as_a_removed_row() {
    let (mut m, t) = model();
    let p = export(&m, "late_el");
    let lv: i64 = m
        .sql()
        .query_row("SELECT id FROM level", [], |r| r.get(0))
        .unwrap();
    m.add_wall(&WallSpec {
        a: (0., 0.),
        b: (0., 3.),
        level: lv,
        base_offset: 0.0,
        height: 3.0,
        wall_type: t,
        offset0: None,
    })
    .unwrap();
    let rep = import_xlsx_with(&mut m, &p, apply_opts(true)).unwrap();
    assert!(
        rep.applied && rep.removed.is_empty() && rep.counts().deleted == 0,
        "{rep:?}"
    );
    assert_eq!(count(&m, "wall"), 4);
}

#[test]
fn read_only_edit_does_not_block_apply() {
    let (mut m, _) = model();
    let p = export(&m, "ro_src");
    let q = tmp("ro_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, "length [m] (read-only)", Data::Float(99.0));
            set(g, 2, "height [m]", Data::Float(3.3));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    let ig = rep.ignored.iter().find(|i| i.row == 2).expect("a reason");
    assert!(ig.reason.contains("read-only"), "{ig:?}");
    assert!((wall_h(&m, 2) - 3.3).abs() < 1e-12);
}

#[test]
fn stale_id_row_is_ignored_not_blocking() {
    let (mut m, _) = model();
    let p = export(&m, "stale_src");
    let q = tmp("stale_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, "height [m]", Data::Float(3.4));
            let dup = g[2].clone();
            g.push(dup);
        }
    });
    m.apply_edits(&[Edit::Delete { table: "wall", id: 3 }]).unwrap();
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    let why = |row: u32| rep.ignored.iter().find(|i| i.row == row).map(|i| i.reason.clone());
    assert!(why(4).unwrap().contains("no longer exists"), "{:?}", rep.ignored);
    // row 5 repeats row 3 exactly: nothing to choose, so no "clear the id" request any more
    assert!(why(5).unwrap().contains("same values"), "{:?}", rep.ignored);
    assert!((wall_h(&m, 1) - 3.4).abs() < 1e-12, "the rest still applies");
}

#[test]
fn renamed_sheet_reported() {
    let (mut m, _) = model();
    let p = export(&m, "ren_src");
    let q = tmp("ren_dst");
    rename_sheet(&p, &q, WALLS, "Walls renamed");
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    let names: Vec<&str> = rep.sheet_issues.iter().map(|s| s.sheet.as_str()).collect();
    assert_eq!(names, vec![WALLS]);
    assert_eq!(count(&m, "wall"), 3);
}

#[test]
fn pinned_wall_is_skipped_with_a_reason_in_preview_and_apply() {
    let (mut m, _) = model();
    let p = export(&m, "pin_src");
    let q = tmp("pin_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, "height [m]", Data::Float(3.6));
            set(g, 2, "height [m]", Data::Float(3.7));
        }
    });
    m.pin(&[("wall", 1)]).unwrap();
    let pre = import_xlsx_with(&mut m, &q, preview()).unwrap();
    assert!(
        pre.ignored.iter().any(|i| i.reason.contains("wall 1 is pinned")),
        "{pre:?}"
    );
    assert!(pre.changes.iter().all(|c| c.id != Some(1)), "{:?}", pre.changes);
    assert!(pre
        .batch()
        .iter()
        .all(|e| !matches!(e, Edit::Wall { id: 1, .. })));
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert!((wall_h(&m, 1) - 3.0).abs() < 1e-12);
    assert!((wall_h(&m, 2) - 3.7).abs() < 1e-12);
}

#[test]
fn locked_param_skipped_by_xlsx_with_reason() {
    let (mut m, _) = model();
    m.apply_edits(&[Edit::NewParam(spec("Code", ParamType::Text, false, &["wall"]))])
        .unwrap();
    let set_code = |m: &mut Model, id: i64, v: &str| {
        m.apply_edits(&[Edit::ParamValue {
            param: "Code".into(),
            kind: "wall".into(),
            id,
            value: Some(PVal::Text(v.into())),
        }])
        .unwrap();
    };
    set_code(&mut m, 1, "A");
    set_code(&mut m, 2, "B");
    let p = export(&m, "lock_src");
    let q = tmp("lock_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, "Code", s("A2"));
            set(g, 2, "Code", s("B2"));
        }
    });
    m.set_param_lock(&[("wall", 1, "Code")]).unwrap();
    // the skipped cell is named with its sheet, address, values and reason
    let skipped = |rep: &Report| {
        rep.ignored.iter().any(|i| {
            i.sheet == WALLS
                && i.row > 1
                && i.col > 0
                && i.column == "Code"
                && i.id == Some(1)
                && i.new.contains("A2")
                && i.reason == "value of 'Code' is locked: skipped"
        })
    };
    let locked_change = |rep: &Report| {
        rep.changes
            .iter()
            .any(|c| c.id == Some(1) && c.column == "Code")
    };
    let pre = import_xlsx_with(&mut m, &q, preview()).unwrap();
    assert!(skipped(&pre), "{:?}", pre.ignored);
    assert!(!locked_change(&pre), "{:?}", pre.changes);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert!(skipped(&rep), "{:?}", rep.ignored);
    assert!(!locked_change(&rep), "{:?}", rep.changes);
    let d = m.param_by_name("Code").unwrap().unwrap();
    let v = |id| m.param_value(d.id, "wall", id).unwrap();
    assert_eq!(v(1), Some(PVal::Text("A".into())), "the locked value is kept");
    assert_eq!(v(2), Some(PVal::Text("B2".into())), "an unlocked value is still written");
}

// ---- bad cells skip alone; conflicts on parameter columns; take the book value ----

fn wall_offset(m: &Model, id: i64) -> f64 {
    m.sql()
        .query_row("SELECT base_offset FROM wall WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

#[test]
fn bad_value_skips_only_that_cell_and_never_blocks() {
    let (mut m, _) = model();
    let p = export(&m, "bad_src");
    let q = tmp("bad_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, "height [m]", s("tall"));
            set(g, 1, "base_offset [m]", Data::Float(0.5));
            set(g, 2, "height [m]", Data::Float(-1.0));
            set(g, 3, "height [m]", s("3.1 parsecs"));
            set(g, 3, "base_offset [m]", Data::Float(0.25));
        }
    });
    let pre = import_xlsx_with(&mut m, &q, preview()).unwrap();
    assert_eq!((pre.blocking(), pre.skipped()), (0, 3), "{:?}", pre.rejected);
    for r in 2..=4 {
        assert!(
            pre.rejected.iter().any(|x| x.row == r && !x.reason.is_empty()),
            "row {r}: {:?}",
            pre.rejected
        );
    }
    // the good cells of the same rows are still edits
    assert_eq!(pre.counts().changed, 2, "{:?}", pre.changes);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    for id in 1..=3 {
        assert!((wall_h(&m, id) - 3.0).abs() < 1e-12, "wall {id} height kept");
    }
    assert!((wall_offset(&m, 1) - 0.5).abs() < 1e-12);
    assert!((wall_offset(&m, 3) - 0.25).abs() < 1e-12);
}

#[test]
fn bad_new_row_is_skipped_and_the_rest_applies() {
    let (mut m, _) = model();
    let p = export(&m, "badrow_src");
    let q = tmp("badrow_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            push_wall(g);
            let r = g.len() - 1;
            set(g, r, "ax [m]", s("far"));
            set(g, 2, "height [m]", Data::Float(3.3));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.blocking() == 0, "{rep:?}");
    assert_eq!(rep.skipped(), 1, "{:?}", rep.rejected);
    assert_eq!(count(&m, "wall"), 3, "the bad new row is not created");
    assert!((wall_h(&m, 2) - 3.3).abs() < 1e-12);
}

#[test]
fn header_problem_still_blocks_the_import() {
    let (mut m, _) = model();
    let p = export(&m, "hdr_src");
    let q = tmp("hdr_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            let c = col(g, "height [m]");
            g[0][c] = s("height [parsec]");
            set(g, 1, "base_offset [m]", Data::Float(0.5));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(!rep.applied && rep.blocking() > 0, "{rep:?}");
    assert!((wall_offset(&m, 1) - 0.0).abs() < 1e-12);
}

fn code_model() -> Model {
    let (mut m, _) = model();
    m.apply_edits(&[Edit::NewParam(spec("Code", ParamType::Text, false, &["wall"]))])
        .unwrap();
    set_code(&mut m, 1, "A");
    m
}

fn set_code(m: &mut Model, id: i64, v: &str) {
    m.apply_edits(&[Edit::ParamValue {
        param: "Code".into(),
        kind: "wall".into(),
        id,
        value: Some(PVal::Text(v.into())),
    }])
    .unwrap();
}

fn code_of(m: &Model, id: i64) -> Option<PVal> {
    let d = m.param_by_name("Code").unwrap().unwrap();
    m.param_value(d.id, "wall", id).unwrap()
}

#[test]
fn parameter_cell_edited_in_book_and_model_is_a_conflict() {
    let mut m = code_model();
    let p = export(&m, "pconf_src");
    let q = tmp("pconf_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, "Code", s("B"));
        }
    });
    set_code(&mut m, 1, "C");
    let pre = import_xlsx_with(&mut m, &q, preview()).unwrap();
    assert_eq!(pre.conflicts.len(), 1, "{pre:?}");
    let c = pre.conflicts[0].clone();
    assert_eq!((c.sheet.as_str(), c.row, c.id), (WALLS, 2, Some(1)));
    assert_eq!(
        (c.base.as_str(), c.book.as_str(), c.model.as_str(), c.take_book),
        ("A", "B", "C", false)
    );
    assert_eq!(pre.counts().changed, 0, "{:?}", pre.changes);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(code_of(&m, 1), Some(PVal::Text("C".into())), "the model value stays");
    // the book value is applied only when taken, cell by cell
    let take = Take {
        all: false,
        cells: [(c.sheet.clone(), c.row, c.col)].into_iter().collect(),
        ..Take::default()
    };
    let rep = baseline::with_take(&take, || import_xlsx(&mut m, &q)).unwrap();
    assert!(rep.applied && rep.conflicts[0].take_book, "{rep:?}");
    assert_eq!(code_of(&m, 1), Some(PVal::Text("B".into())));
}

#[test]
fn parameter_cell_untouched_in_book_keeps_a_later_model_edit() {
    let mut m = code_model();
    let p = export(&m, "pnewer_src");
    set_code(&mut m, 1, "C");
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(rep.applied && rep.changes.is_empty() && rep.conflicts.is_empty(), "{rep:?}");
    assert_eq!(rep.model_newer, 1);
    assert_eq!(code_of(&m, 1), Some(PVal::Text("C".into())));
}

#[test]
fn main_sheet_conflict_keeps_model_unless_book_value_is_taken() {
    let (mut m, _) = model();
    let p = export(&m, "take_src");
    let q = tmp("take_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, "height [m]", Data::Float(4.0));
            set(g, 2, "height [m]", Data::Float(4.2));
        }
    });
    set_wall_h(&m, 1, 3.5);
    set_wall_h(&m, 2, 3.6);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert_eq!(rep.conflicts.len(), 2);
    assert!((wall_h(&m, 1) - 3.5).abs() < 1e-12 && (wall_h(&m, 2) - 3.6).abs() < 1e-12);
    // one cell taken, the other kept
    let cells: Vec<_> = rep.conflicts.iter().map(|c| (c.sheet.clone(), c.row, c.col)).collect();
    let mut take = Take::default();
    take.set(cells[0].clone(), true, &cells);
    let pre = baseline::with_take(&take, || import_xlsx_with(&mut m, &q, preview())).unwrap();
    assert_eq!(pre.conflicts.iter().filter(|c| c.take_book).count(), 1);
    assert_eq!(pre.counts().changed, 1, "{:?}", pre.changes);
    let rep = baseline::with_take(&take, || import_xlsx(&mut m, &q)).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert!((wall_h(&m, 1) - 4.0).abs() < 1e-12, "taken");
    assert!((wall_h(&m, 2) - 3.6).abs() < 1e-12, "kept");
    // all of them
    set_wall_h(&m, 1, 3.5);
    let all = Take { all: true, ..Take::default() };
    let rep = baseline::with_take(&all, || import_xlsx(&mut m, &q)).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert!((wall_h(&m, 1) - 4.0).abs() < 1e-12 && (wall_h(&m, 2) - 4.2).abs() < 1e-12);
    // choosing "keep" for one cell out of "all" keeps the others taken
    let mut t = Take { all: true, ..Take::default() };
    t.set(cells[0].clone(), false, &cells);
    assert!(!t.has(&cells[0].0, cells[0].1, cells[0].2));
    assert!(t.has(&cells[1].0, cells[1].1, cells[1].2));
}

#[test]
fn parameter_cells_use_the_baseline_too() {
    let (mut m, _) = model();
    m.apply_edits(&[Edit::NewParam(spec("Code", ParamType::Text, false, &["wall"]))])
        .unwrap();
    let set_code = |m: &mut Model, id: i64, v: &str| {
        m.apply_edits(&[Edit::ParamValue {
            param: "Code".into(),
            kind: "wall".into(),
            id,
            value: Some(PVal::Text(v.into())),
        }])
        .unwrap();
    };
    set_code(&mut m, 1, "A");
    set_code(&mut m, 2, "B");
    let p = export(&m, "par_src");
    let q = tmp("par_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 2, "Code", s("B2"));
        }
    });
    set_code(&mut m, 1, "A2");
    set_code(&mut m, 2, "B3");
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(rep.model_newer, 1, "wall 1: the model edit is kept");
    assert_eq!(rep.conflicts.len(), 1, "wall 2: edited on both sides");
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
    let d = m.param_by_name("Code").unwrap().unwrap();
    let v = |id| m.param_value(d.id, "wall", id).unwrap();
    assert_eq!(v(1), Some(PVal::Text("A2".into())));
    assert_eq!(v(2), Some(PVal::Text("B3".into())));
}

#[test]
fn new_rows_pair_by_id() {
    let (mut m, _) = model();
    m.apply_edits(&[Edit::NewParam(spec("Code", ParamType::Text, false, &["wall"]))])
        .unwrap();
    let p = export(&m, "pair_src");
    let q = tmp("pair_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            // a good row, a row that is not created, a good row: each value goes to its own wall
            for (code, bad) in [("A", false), ("X", true), ("B", false)] {
                push_wall(g);
                let r = g.len() - 1;
                set(g, r, "Code", s(code));
                if bad {
                    set(g, r, "ax [m]", s("far"));
                }
            }
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(count(&m, "wall"), 5);
    let d = m.param_by_name("Code").unwrap().unwrap();
    assert_eq!(m.param_value(d.id, "wall", 4).unwrap(), Some(PVal::Text("A".into())));
    assert_eq!(m.param_value(d.id, "wall", 5).unwrap(), Some(PVal::Text("B".into())));
    // the marks are in the batch right after their creating edits
    let marks: Vec<u32> = rep
        .edits
        .iter()
        .filter_map(|e| match e {
            Edit::Mark { row, .. } => Some(*row),
            _ => None,
        })
        .collect();
    assert_eq!(marks.len(), 2, "{marks:?}");
}

/// Copy of `src` with a chart sheet `name` added to the package, as a user would add one.
fn add_chart_sheet(src: &Path, dst: &Path, name: &str) {
    use std::io::{Read, Write};
    let mut a = zip::ZipArchive::new(std::fs::File::open(src).unwrap()).unwrap();
    let mut w = zip::ZipWriter::new(std::fs::File::create(dst).unwrap());
    let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for i in 0..a.len() {
        let mut f = a.by_index(i).unwrap();
        let n = f.name().unwrap().into_owned();
        let mut b = Vec::new();
        f.read_to_end(&mut b).unwrap();
        let edit = |from: &str, to: &str| {
            let t = String::from_utf8(b.clone()).unwrap();
            assert!(t.contains(from), "{n} lacks {from}");
            t.replacen(from, &format!("{to}{from}"), 1).into_bytes()
        };
        let b = match n.as_str() {
            "xl/workbook.xml" => edit(
                "</sheets>",
                &format!("<sheet name=\"{name}\" sheetId=\"900\" r:id=\"rIdChartSheet\"/>"),
            ),
            "xl/_rels/workbook.xml.rels" => edit(
                "</Relationships>",
                "<Relationship Id=\"rIdChartSheet\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/chartsheet\" Target=\"chartsheets/sheet1.xml\"/>",
            ),
            "[Content_Types].xml" => edit(
                "</Types>",
                "<Override PartName=\"/xl/chartsheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.chartsheet+xml\"/>",
            ),
            _ => b,
        };
        w.start_file(n, o).unwrap();
        w.write_all(&b).unwrap();
    }
    w.start_file("xl/chartsheets/sheet1.xml", o).unwrap();
    w.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><chartsheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"/>").unwrap();
    w.finish().unwrap();
}

fn chart_book(m: &Model, name: &str) -> PathBuf {
    let plain = export(m, &format!("{name}_plain"));
    let p = tmp(name);
    add_chart_sheet(&plain, &p, "Chart1");
    let _ = std::fs::remove_file(&plain);
    p
}

fn sheet_list(p: &Path) -> Vec<String> {
    let wb: Xlsx<_> = open_workbook(p).unwrap();
    wb.sheet_names()
}

#[test]
fn chart_sheet_does_not_block_reexport() {
    let (m, _) = model();
    let p = chart_book(&m, "chart_block");
    assert!(sheet_list(&p).iter().any(|s| s == "Chart1"), "the fixture holds the chart sheet");
    let k = super::extra::Kept::read(&p, &[]).expect("a chart sheet must not refuse the book");
    assert_eq!(k.dropped, vec!["Chart1".to_string()]);
    let out = tmp("chart_block_out");
    export_xlsx_families_from(&m, &p, &out, Units::Metric, &[]).expect("re-export works");
    let names = sheet_list(&out);
    assert!(names.iter().any(|s| s == INFO), "{names:?}");
    assert!(!names.iter().any(|s| s == "Chart1"), "the new book is a plain one");
}

#[test]
fn chart_sheet_named_in_outcome() {
    let (m, _) = model();
    let p = chart_book(&m, "chart_named");
    assert_eq!(dropped_sheets(&p), vec!["Chart1".to_string()]);
    assert!(dropped_sheets(&tmp("chart_named_missing")).is_empty(), "a missing book drops nothing");
    let garbage = tmp("chart_named_garbage");
    std::fs::write(&garbage, b"not a zip").unwrap();
    assert!(dropped_sheets(&garbage).is_empty(), "an unreadable book is refused elsewhere");
}

#[test]
fn old_book_kept_when_sheet_dropped() {
    // the file move itself lives in the app; here the old book must stay intact after a re-export
    let (m, _) = model();
    let p = chart_book(&m, "chart_old");
    let before = std::fs::read(&p).unwrap();
    let out = tmp("chart_old_out");
    export_xlsx_families_from(&m, &p, &out, Units::Metric, &[]).unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), before, "writing beside the old book leaves it untouched");
    assert!(sheet_list(&p).iter().any(|s| s == "Chart1"));
}

#[test]
fn plain_book_reexport_unchanged() {
    let (m, _) = model();
    let p = export(&m, "chart_plain");
    assert!(dropped_sheets(&p).is_empty());
    let k = super::extra::Kept::read(&p, &[]).unwrap();
    assert!(k.dropped.is_empty());
    let out = tmp("chart_plain_out");
    export_xlsx_families_from(&m, &p, &out, Units::Metric, &[]).unwrap();
    assert_eq!(sheet_list(&out), sheet_list(&p));
}

#[test]
fn two_removed_rows_are_counted_and_nothing_is_deleted_when_delete_is_off() {
    let (mut m, _) = model();
    let p = export(&m, "rm2_src");
    let q = tmp("rm2_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            g.pop();
            g.pop();
        }
    });
    let rep = import_xlsx_with(&mut m, &q, apply_opts(false)).unwrap();
    assert_eq!(rep.removed_count(), 2, "{rep:?}");
    assert_eq!(count(&m, "wall"), 3);
}

/// Copies `src` with typed user columns on the walls sheet (by row id) and a user sheet of
/// typed cells. K1 becomes a literal error cell (no formula), as typed by hand.
fn typed_book(src: &Path, dst: &Path) {
    use rust_xlsxwriter::{Format, Formula};
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    let date = Format::new().set_num_format("dd.mm.yyyy");
    for name in wb.sheet_names() {
        let grid: Grid = wb.worksheet_range(&name).unwrap().rows().map(|r| r.to_vec()).collect();
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
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
        if name == WALLS {
            let (ci, c) = (col(&grid, "id"), grid[0].len() as u16);
            ws.write_string(0, c, "Due").unwrap();
            ws.write_string(0, c + 1, "Checked").unwrap();
            for (i, row) in grid.iter().enumerate().skip(1) {
                let Some(id) = row[ci].as_f64() else { continue };
                ws.write_number_with_format(i as u32, c, 45677.0 + id, &date).unwrap();
                ws.write_boolean(i as u32, c + 1, id as i64 % 2 == 1).unwrap();
            }
        }
    }
    let ws = out.add_worksheet();
    ws.set_name("Typed").unwrap();
    let f = |c: &str| Format::new().set_num_format(c);
    ws.write_number_with_format(0, 0, 45678.0, &date).unwrap();
    ws.write_number_with_format(0, 1, 45678.5, &f("dd.mm.yyyy hh:mm")).unwrap();
    ws.write_number_with_format(0, 2, 0.25, &f("hh:mm")).unwrap();
    ws.write_number_with_format(0, 3, 1.5, &f("[h]:mm:ss")).unwrap();
    ws.write_boolean(0, 4, true).unwrap();
    ws.write_boolean(0, 5, false).unwrap();
    ws.write_number(0, 6, 3.25).unwrap();
    ws.write_string(0, 7, "abc").unwrap();
    ws.write_formula(0, 9, Formula::new("=G1*2")).unwrap();
    ws.write_formula(0, 10, Formula::new("=#N/A").set_result("#N/A")).unwrap();
    ws.write_formula(0, 11, Formula::new("=NA()")).unwrap();
    ws.write_string(1, 0, "end").unwrap();
    let raw = dst.with_extension("raw.xlsx");
    out.save(&raw).unwrap();
    // the writer has no literal error cell: turn K1 into one in the sheet xml
    let mut zin = zip::ZipArchive::new(std::fs::File::open(&raw).unwrap()).unwrap();
    let mut zout = zip::ZipWriter::new(std::fs::File::create(dst).unwrap());
    let mut hit = false;
    for i in 0..zin.len() {
        let mut e = zin.by_index(i).unwrap();
        let name = e.name().unwrap().into_owned();
        let mut buf = vec![];
        std::io::Read::read_to_end(&mut e, &mut buf).unwrap();
        if name.starts_with("xl/worksheets/") {
            let t = String::from_utf8(buf.clone()).unwrap();
            // older writers typed a cached error result "str", newer ones "e"
            for from in [
                r##"<c r="K1" t="str"><f>#N/A</f><v>#N/A</v></c>"##,
                r##"<c r="K1" t="e"><f>#N/A</f><v>#N/A</v></c>"##,
            ] {
                if t.contains(from) {
                    hit = true;
                    buf = t.replace(from, r##"<c r="K1" t="e"><v>#N/A</v></c>"##).into_bytes();
                }
            }
        }
        zout.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut zout, &buf).unwrap();
    }
    zout.finish().unwrap();
    assert!(hit, "literal error cell not planted");
    let _ = std::fs::remove_file(&raw);
}

/// First row of the user sheet, its formulas, and the walls user columns by id.
type Typed = (Vec<Data>, Vec<String>, Vec<(f64, Data, Data)>);

fn typed_cells(p: &Path) -> Typed {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    let vals = wb.worksheet_range("Typed").unwrap();
    let forms = wb.worksheet_formula("Typed").unwrap();
    let row = (0..12u32)
        .map(|c| vals.get_value((0, c)).cloned().unwrap_or(Data::Empty))
        .collect();
    let fs = (0..12u32)
        .map(|c| forms.get_value((0, c)).cloned().unwrap_or_default())
        .collect();
    let g = read_sheet(p, WALLS);
    let (ci, cd, cb) = (col(&g, "id"), col(&g, "Due"), col(&g, "Checked"));
    let mut walls: Vec<(f64, Data, Data)> = g[1..]
        .iter()
        .filter_map(|r| Some((r.get(ci)?.as_f64()?, r.get(cd)?.clone(), r.get(cb)?.clone())))
        .collect();
    walls.sort_by(|a, b| a.0.total_cmp(&b.0));
    (row, fs, walls)
}

#[test]
fn kept_user_sheet_keeps_dates_bools_errors() {
    use calamine::{CellErrorType, ExcelDateTime, ExcelDateTimeType};
    let (m, _) = model();
    let p = export(&m, "types_src");
    let q = tmp("types_book");
    typed_book(&p, &q);
    let (src, _, src_walls) = typed_cells(&q);
    assert_eq!(src[10], Data::Error(CellErrorType::NA), "source holds a literal error");

    export_xlsx(&m, &q).unwrap();
    let first = typed_cells(&q);
    export_xlsx(&m, &q).unwrap();
    let second = typed_cells(&q);
    assert_eq!(first, second, "second re-export changes nothing");
    assert_eq!(first.2, src_walls, "walls user columns as in the user's book");

    let (row, fs, walls) = first;
    let dt = |x: f64, t| Data::DateTime(ExcelDateTime::new(x, t, false));
    assert_eq!(row[0], dt(45678.0, ExcelDateTimeType::DateTime), "date");
    assert_eq!(row[1], dt(45678.5, ExcelDateTimeType::DateTime), "date-time");
    assert_eq!(row[2], dt(0.25, ExcelDateTimeType::DateTime), "time");
    assert_eq!(row[3], dt(1.5, ExcelDateTimeType::TimeDelta), "duration");
    assert_eq!(row[4], Data::Bool(true));
    assert_eq!(row[5], Data::Bool(false));
    assert_eq!(row[6], Data::Float(3.25));
    assert_eq!(row[7], Data::String("abc".into()));
    assert_eq!(row[8], Data::Empty);
    assert_eq!(fs[9], "G1*2", "formula stays a formula");
    assert_eq!(fs[10], "#N/A", "literal error is written as the same error constant");
    assert_eq!(fs[11], "NA()");
    assert_eq!(walls.len(), 3);
    for (id, due, ok) in &walls {
        assert_eq!(*due, dt(45677.0 + id, ExcelDateTimeType::DateTime), "date of wall {id}");
        assert_eq!(*ok, Data::Bool(*id as i64 % 2 == 1), "bool of wall {id}");
    }
    let _ = (std::fs::remove_file(&p), std::fs::remove_file(&q));
}

fn look_alike_of(rep: &Report, name: &str) -> Vec<String> {
    rep.look_alikes.iter().find(|l| l.expected == name).map(|l| l.found.clone()).unwrap_or_default()
}

fn renamed_report(tag: &str, to: &str) -> Report {
    let (mut m, _) = model();
    let p = export(&m, &format!("{tag}_src"));
    let q = tmp(&format!("{tag}_dst"));
    rename_sheet(&p, &q, WALLS, to);
    import_xlsx(&mut m, &q).unwrap()
}

#[test]
fn case_renamed_sheet_warned_not_read() {
    let rep = renamed_report("la_case", "walls");
    assert_eq!(look_alike_of(&rep, WALLS), vec!["walls".to_string()]);
    assert!(rep.changes.is_empty(), "the look-alike sheet is not read: {:?}", rep.changes);
}

#[test]
fn copied_sheet_suffix_warned() {
    let rep = renamed_report("la_copy", "Walls (2)");
    assert_eq!(look_alike_of(&rep, WALLS), vec!["Walls (2)".to_string()]);
}

#[test]
fn spaces_variant_warned() {
    let rep = renamed_report("la_space", "Wa lls");
    assert_eq!(look_alike_of(&rep, WALLS), vec!["Wa lls".to_string()]);
}

#[test]
fn two_candidates_both_named() {
    let (mut m, _) = model();
    let p = export(&m, "la_two_src");
    let mid = tmp("la_two_mid");
    let q = tmp("la_two_dst");
    rename_sheet(&p, &mid, WALLS, "walls");
    let extra: Grid = vec![vec![Data::String("x".into())]];
    edit_with(&mid, &q, &[("Walls (2)", extra)], |_, _| {});
    let rep = import_xlsx(&mut m, &q).unwrap();
    let mut found = look_alike_of(&rep, WALLS);
    found.sort();
    assert_eq!(found, vec!["Walls (2)".to_string(), "walls".to_string()]);
}

#[test]
fn exact_match_no_warning() {
    let (mut m, _) = model();
    let p = export(&m, "la_exact_src");
    let q = tmp("la_exact_dst");
    let extra: Grid = vec![vec![Data::String("x".into())]];
    // a look-alike beside the exact sheet; "walls" itself cannot sit beside "Walls" (sheet
    // names are unique without case: the writer refuses it)
    edit_with(&p, &q, &[("Walls (2)", extra)], |_, _| {});
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.look_alikes.is_empty(), "{:?}", rep.look_alikes);
}

#[test]
fn unrelated_sheet_no_warning() {
    let rep = renamed_report("la_other", "Notes");
    assert!(look_alike_of(&rep, WALLS).is_empty());
}

fn st(len: u64) -> Option<Stamp> {
    Some(Stamp { mtime: None, len })
}

#[test]
fn settle_reads_once_after_the_file_stops_changing() {
    let mut w = Watch::new(Path::new("none.xlsx"));
    w.seen = st(1);
    let mut t = Instant::now();
    let step = Duration::from_millis(500);
    // still growing: three different stamps, never ready
    for len in [10, 20, 30] {
        assert!(w.settled(t, |_| st(len)).is_none());
        t += step;
    }
    // two equal stamps in a row: ready, exactly once
    assert!(w.settled(t, |_| st(40)).is_none());
    t += step;
    assert_eq!(w.settled(t, |_| st(40)), st(40));
    w.mark(st(40));
    t += POLL;
    assert!(w.settled(t, |_| st(40)).is_none());
}

#[test]
fn settle_with_an_unchanged_file_does_nothing() {
    let mut w = Watch::new(Path::new("none.xlsx"));
    w.seen = st(5);
    let mut t = Instant::now();
    for _ in 0..6 {
        assert!(w.settled(t, |_| st(5)).is_none());
        t += POLL;
    }
}

#[test]
fn settle_keeps_waiting_while_the_file_is_missing() {
    let mut w = Watch::new(Path::new("none.xlsx"));
    w.seen = st(5);
    let mut t = Instant::now();
    for _ in 0..4 {
        assert!(w.settled(t, |_| None).is_none());
        t += POLL;
    }
}

#[test]
fn mass_delete_threshold() {
    // 20 % exactly is fine, 21 % warns, 19 % does not, a whole sheet always warns
    assert!(!is_mass_delete(19, 100));
    assert!(!is_mass_delete(20, 100));
    assert!(is_mass_delete(21, 100));
    assert!(is_mass_delete(1, 1));
    assert!(is_mass_delete(3, 3));
    assert!(!is_mass_delete(0, 0));
    let d = SheetDeletion { sheet: "Walls".into(), deleted: 21, total: 100 };
    assert_eq!(d.percent(), 21);
    assert!(d.is_mass());
    let mut r = Report::default();
    r.note_deletions("Walls", 0, 10);
    assert!(r.deletions.is_empty(), "no deletion, no entry");
    r.note_deletions("Walls", 2, 10);
    r.note_deletions("Doors", 3, 3);
    assert_eq!(r.mass_deletes().len(), 1);
    assert_eq!(r.mass_deletes()[0].sheet, "Doors");
}
