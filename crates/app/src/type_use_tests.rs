//! A type or layer row of a workbook names how many elements it reaches, and Apply needs the
//! type tick whenever that number is not zero. Languages are passed explicitly, never switched.
use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_io::xlsx;
use ddc_model::{LayerSpec, WallSpec};

use super::*;
use crate::editor::Editor;
use crate::shell::xlsx_gate::TYPES_UNCONFIRMED;
use crate::shell::xlsx_win::{apply_preview, Applied, XlsxPreview};
use crate::shell::xl_diff::{Ctx, DiffState};
use crate::xl_text::Lang;

type Grid = Vec<Vec<Data>>;

fn cyr(s: &str) -> bool {
    s.chars().any(|c| ('\u{400}'..='\u{4FF}').contains(&c))
}

/// Three walls of `ext`; `spare` has the same layer and no walls.
fn editor() -> (Editor, i64, i64) {
    let mut ed = Editor::new_in_memory().unwrap();
    let lv = ed.level;
    let brick = || vec![LayerSpec { material: "brick".into(), thickness: 0.25 }];
    let t = ed.model.add_wall_type("ext", &brick()).unwrap();
    let spare = ed.model.add_wall_type("spare", &brick()).unwrap();
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
        ed.model.add_wall(&w).unwrap();
    }
    ed.reload().unwrap();
    (ed, t, spare)
}

fn book(ed: &Editor, tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc_xs67_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("plan.xlsx");
    xlsx::export_xlsx(&ed.model, &p).unwrap();
    p
}

/// The book saved again with the thickness of type `wtype`'s first layer set to `v`.
fn set_thickness(p: &Path, wtype: i64, v: f64) {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    let mut sheets: Vec<(String, Grid)> = wb
        .sheet_names()
        .into_iter()
        .map(|n| {
            let g: Grid = wb.worksheet_range(&n).unwrap().rows().map(|r| r.to_vec()).collect();
            (n, g)
        })
        .collect();
    drop(wb);
    let g = &mut sheets.iter_mut().find(|s| s.0 == "Layers").unwrap().1;
    let at = |h: &str| g[0].iter().position(|d| d.to_string() == h).unwrap();
    let (tc, kc) = (at("type_id"), at("thickness [m]"));
    let r = (1..g.len()).find(|r| g[*r][tc].to_string() == wtype.to_string()).unwrap();
    g[r][kc] = Data::Float(v);
    let mut out = rust_xlsxwriter::Workbook::new();
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
    out.save(p).unwrap();
}

fn preview(ed: &mut Editor, p: &Path) -> XlsxPreview {
    let mut pv = XlsxPreview::load(&mut ed.model, p, false).unwrap();
    pv.rev = ed.content_snap();
    pv
}

fn thickness(ed: &Editor, wtype: i64) -> f64 {
    let sql = "SELECT thickness FROM wall_type_layer WHERE wtype=?1 ORDER BY pos LIMIT 1";
    ed.model.sql().query_row(sql, [wtype], |r| r.get(0)).unwrap()
}

/// Column text of the first listed row of sheet `sheet`, in `lang`.
fn row_text(ed: &Editor, pv: &XlsxPreview, sheet: &str, lang: Lang) -> String {
    let mut d = DiffState::default();
    let (rep, model, gen, rev) = (&pv.report, &ed.model, pv.report_gen, ed.cache_key());
    let cx = Ctx { rep, model, gen, rev, lang };
    d.ensure(&cx);
    let row = d.view_items(&pv.report).position(|it| it.sheet == sheet).expect("row listed");
    d.column_text(&cx, row)
}

#[test]
fn one_counter_for_every_type_table() {
    let (ed, t, spare) = editor();
    assert_eq!(users(&ed.model, "wall_type", t), Some(3));
    assert_eq!(users(&ed.model, "wall_type", spare), Some(0));
    assert_eq!(users(&ed.model, "slab_type", 1), Some(0));
    assert_eq!(users(&ed.model, "member_type", 1), Some(0));
    assert_eq!(users(&ed.model, "comp_type", 1), Some(0));
    assert_eq!(users(&ed.model, "wall", t), None, "not a type table");
    // the assistant card reads the same counter
    let key: ddc_model::ElKey = ("wall_type".into(), t);
    assert_eq!(crate::ai::apply_verify::type_users(&ed.model, &key), Some(3));
}

#[test]
fn affects_text_in_both_languages() {
    assert_eq!(affects_text(false, "wall_type", 3).as_deref(), Some("affects 3 walls"));
    assert_eq!(affects_text(false, "wall_type", 1).as_deref(), Some("affects 1 wall"));
    assert_eq!(affects_text(false, "slab_type", 2).as_deref(), Some("affects 2 elements"));
    assert_eq!(affects_text(false, "wall_type", 0), None, "no users, no count");
    assert_eq!(affects_text(true, "wall_type", 0), None);
    let (one, few, many) = (
        affects_text(true, "wall_type", 1).unwrap(),
        affects_text(true, "wall_type", 3).unwrap(),
        affects_text(true, "wall_type", 5).unwrap(),
    );
    assert!(cyr(&one) && cyr(&few) && cyr(&many), "{one} / {few} / {many}");
    assert!(few.contains('3') && many.contains('5'), "{few} / {many}");
    assert!(one != few.replace('3', "1") && few.replace('3', "5") != many, "plural forms");
    let el = affects_text(true, "member_type", 5).unwrap();
    assert!(cyr(&el) && el.contains('5') && el != many, "{el}");
}

#[test]
fn layer_thickness_edit_with_three_walls_says_three() {
    let (mut ed, t, _) = editor();
    let p = book(&ed, "three");
    set_thickness(&p, t, 0.3);
    let pv = preview(&mut ed, &p);
    assert_eq!(pv.report.counts().changed, 1, "{:?}", pv.report.changes);
    assert_eq!(report_users(&ed.model, &pv.report), 3);
    let en = row_text(&ed, &pv, "Layers", Lang::En);
    assert!(en.ends_with("affects 3 walls"), "{en}");
    let ru = row_text(&ed, &pv, "Layers", Lang::Ru);
    assert!(cyr(&ru) && ru.contains('3') && !ru.contains("affects"), "{ru}");
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn unticked_type_change_is_refused_and_leaves_the_model_untouched() {
    let (mut ed, t, _) = editor();
    let p = book(&ed, "refused");
    set_thickness(&p, t, 0.3);
    let mut pv = preview(&mut ed, &p);
    let (snap, undo) = (ed.content_snap(), ed.undo_len());
    let r = apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En);
    assert_eq!(r.err().as_deref(), Some(TYPES_UNCONFIRMED));
    // a tick of another report, or of another count, is no tick for this one
    for stale in [(pv.report_gen + 1, 3), (pv.report_gen, 2)] {
        pv.confirm_types = Some(stale);
        let r = apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En);
        assert_eq!(r.err().as_deref(), Some(TYPES_UNCONFIRMED), "{stale:?}");
    }
    assert_eq!(thickness(&ed, t), 0.25);
    assert_eq!((ed.content_snap(), ed.undo_len()), (snap, undo), "nothing written");
    assert!(!pv.report.changes.is_empty(), "the preview stays whole for the next try");
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn ticked_type_change_round_trips_with_nothing_lost() {
    let (mut ed, t, spare) = editor();
    let p = book(&ed, "ticked");
    set_thickness(&p, t, 0.3);
    let mut pv = preview(&mut ed, &p);
    pv.confirm_types = Some((pv.report_gen, 3));
    let r = apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En);
    assert!(matches!(r, Ok(Applied::Done { .. })), "{:?}", r.err());
    assert!((thickness(&ed, t) - 0.3).abs() < 1e-9);
    assert_eq!(thickness(&ed, spare), 0.25, "the other type stays");
    assert_eq!(users(&ed.model, "wall_type", t), Some(3), "the walls keep their type");
    // the book written back reads as the model: no change, refusal, conflict or loss
    let again = preview(&mut ed, &p);
    let rep = &again.report;
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert!(rep.conflicts.is_empty() && rep.proposals.is_empty() && !rep.no_base, "{rep:?}");
    assert_eq!(report_users(&ed.model, rep), 0);
    // one undo step takes the type back for all three walls
    ed.undo();
    assert_eq!(thickness(&ed, t), 0.25);
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn a_type_nobody_uses_has_no_count_and_no_tick() {
    let (mut ed, _, spare) = editor();
    let p = book(&ed, "spare");
    set_thickness(&p, spare, 0.4);
    let mut pv = preview(&mut ed, &p);
    assert_eq!(report_users(&ed.model, &pv.report), 0);
    let en = row_text(&ed, &pv, "Layers", Lang::En);
    assert!(!en.contains("affects"), "{en}");
    let r = apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En);
    assert!(matches!(r, Ok(Applied::Done { .. })), "{:?}", r.err());
    assert!((thickness(&ed, spare) - 0.4).abs() < 1e-9);
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn a_kept_type_conflict_names_no_reach_and_a_taken_one_does() {
    let (ed, t, _) = editor();
    let p = std::env::temp_dir().join(format!("ddc_xs67_kept_{}.xlsx", std::process::id()));
    let mut pv = XlsxPreview::linked(&p, false);
    pv.report.conflicts.push(xlsx::Conflict {
        sheet: "WallTypes".into(),
        row: 2,
        col: 1,
        column: "name".into(),
        id: Some(t),
        base: "ext".into(),
        book: "ext2".into(),
        model: "ext1".into(),
        take_book: false,
    });
    for lang in [Lang::En, Lang::Ru] {
        let kept = row_text(&ed, &pv, "WallTypes", lang);
        assert!(!kept.contains('3'), "{lang:?}: a kept conflict writes nothing: {kept}");
    }
    pv.report.conflicts[0].take_book = true;
    let en = row_text(&ed, &pv, "WallTypes", Lang::En);
    assert!(en.ends_with("affects 3 walls"), "{en}");
    let ru = row_text(&ed, &pv, "WallTypes", Lang::Ru);
    assert!(cyr(&ru) && ru.contains('3'), "{ru}");
}
