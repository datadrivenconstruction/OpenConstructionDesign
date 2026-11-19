// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The opening checks through a workbook, the panel and AI: each refusal reason with its
//! address, a mixed book applied as one step, an untouched book, the reasons in en/ru, and
//! wall edits that would leave a hosted opening out.
use super::*;
use crate::editor::Editor;
use crate::multi_edit::{counts_text_with, Counts};
use crate::shell::xlsx_win::{apply_preview, proposal_note, Applied, XlsxPreview};
use crate::xl_text::{self, Lang};
use ddc_io::xlsx;
use ddc_model::OpeningSpec;
use std::path::{Path, PathBuf};

struct Fx {
    ed: Editor,
    walls: Vec<i64>,
    doors: [i64; 2],
    dir: PathBuf,
}

/// A 6 x 4 m room of 3 m walls; doors 0.9 x 2.1 m at 1 m and 3 m along the first (6 m) wall.
fn fx(tag: &str) -> Fx {
    let mut ed = Editor::new_in_memory().unwrap();
    let lv = ed.level;
    let layer = ddc_model::LayerSpec { material: "brick".into(), thickness: 0.25 };
    let t = ed.model.add_wall_type("ext", &[layer]).unwrap();
    let p = [(0., 0.), (6., 0.), (6., 4.), (0., 4.)];
    let mut walls = vec![];
    for k in 0..4 {
        let spec = ddc_model::WallSpec {
            a: p[k],
            b: p[(k + 1) % 4],
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        walls.push(ed.model.add_wall(&spec).unwrap());
    }
    let a = ed.model.add_opening(walls[0], &door(1.0)).unwrap();
    let b = ed.model.add_opening(walls[0], &door(3.0)).unwrap();
    ed.reload().unwrap();
    let dir = std::env::temp_dir().join(format!("ddc_xlval_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    Fx { ed, walls, doors: [a, b], dir }
}

fn door(along: f64) -> OpeningSpec {
    OpeningSpec { along, width: 0.9, height: 2.1, sill: 0.0, kind: "door".into() }
}

/// The export of `f` with each `(sheet, id, header, value)` typed in, as a user edits it.
fn book(f: &Fx, name: &str, cells: &[(&str, i64, &str, f64)]) -> PathBuf {
    let fams = crate::elem::sheets::families();
    let mut src = f.dir.join(format!("{name}-0.xlsx"));
    xlsx::export_xlsx_families(&f.ed.model, &src, xlsx::Units::Metric, &fams).unwrap();
    for (k, (sheet, id, head, v)) in cells.iter().enumerate() {
        let dst = f.dir.join(format!("{name}-{}.xlsx", k + 1));
        xlsx::patch_cell(&src, &dst, sheet, *id, head, *v).unwrap();
        src = dst;
    }
    src
}

fn preview(f: &mut Fx, book: &Path) -> XlsxPreview {
    let mut pv = XlsxPreview::load(&mut f.ed.model, book, false).unwrap();
    pv.rev = f.ed.content_snap();
    pv
}

/// Book row of element `id` on `sheet` (row 1 is the header).
fn row_of(book: &Path, sheet: &str, id: i64) -> u32 {
    let ids = xlsx::sheet_ids(book, sheet).unwrap();
    ids.iter().position(|i| *i == id).unwrap() as u32 + 2
}

fn has_cyrillic(s: &str) -> bool {
    s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c))
}

/// Edits of the batch that touch element `id` of table `kind`.
fn touching(rep: &xlsx::Report, kind: &str, id: i64) -> usize {
    rep.edits
        .iter()
        .filter(|e| match e {
            Edit::Opening { id: i, .. } | Edit::OpeningSwing { id: i, .. } => {
                kind == "opening" && *i == id
            }
            Edit::Wall { id: i, .. } => kind == "wall" && *i == id,
            _ => false,
        })
        .count()
}

fn opening_size(ed: &Editor, id: i64) -> (f64, f64, f64) {
    ed.model
        .sql()
        .query_row("SELECT along, width, height FROM opening WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
}

fn wall_height(ed: &Editor, id: i64) -> f64 {
    let q = "SELECT height FROM wall WHERE id=?1";
    ed.model.sql().query_row(q, [id], |r| r.get(0)).unwrap()
}

/// One refused row: listed on `sheet` at the element's book row, skipped (not blocking),
/// none of its edits or changes left, the reason readable in both languages.
fn refused_once(tag: &str, cells: &[(&str, i64, &str, f64)], sheet: &str, kind: &str, id: i64) {
    let mut f = fx(tag);
    let p = book(&f, tag, cells);
    let pv = preview(&mut f, &p);
    let rep = &pv.report;
    assert_eq!(rep.rejected.len(), 1, "{tag}: {:?}", rep.rejected);
    let r = &rep.rejected[0];
    assert_eq!((r.sheet.as_str(), r.row), (sheet, row_of(&p, sheet, id)), "{tag}: {r:?}");
    assert!(!r.blocks() && r.col > 0, "{tag}: a refused cell only skips its row: {r:?}");
    assert_eq!(touching(rep, kind, id), 0, "{tag}: the refused row reaches the batch");
    assert!(!rep.changes.iter().any(|c| c.sheet == sheet && c.row == r.row), "{tag}");
    assert_eq!(xl_text::tr(&r.reason, Lang::En), r.reason, "{tag}");
    let ru = xl_text::tr(&r.reason, Lang::Ru);
    assert!(has_cyrillic(&ru) && ru != r.reason, "{tag}: not translated: {ru:?}");
}

const OP: &str = "Openings";

#[test]
fn xlsx_opening_width_zero_or_negative_refused() {
    let f = fx("probe");
    let a = f.doors[0];
    refused_once("w0", &[(OP, a, "width [m]", 0.0)], OP, "opening", a);
    refused_once("wneg", &[(OP, a, "width [m]", -0.5)], OP, "opening", a);
}

#[test]
fn xlsx_opening_outside_the_wall_refused() {
    let f = fx("probe2");
    let b = f.doors[1];
    refused_once("out", &[(OP, b, "along [m]", 5.5)], OP, "opening", b);
}

#[test]
fn xlsx_opening_above_the_wall_refused() {
    let mut f = fx("tall");
    let a = f.doors[0];
    let p = book(&f, "tall", &[(OP, a, "height [m]", 3.5)]);
    let pv = preview(&mut f, &p);
    let r = &pv.report.rejected[0];
    assert_eq!(r.reason, "opening top 3.5 m is above the wall height 3 m");
    assert!(r.column.starts_with("height"), "{r:?}");
    refused_once("tall2", &[(OP, a, "height [m]", 3.5)], OP, "opening", a);
}

#[test]
fn xlsx_opening_overlap_refused() {
    let mut f = fx("ovl");
    let [a, b] = f.doors;
    let p = book(&f, "ovl", &[(OP, a, "along [m]", 2.5)]);
    let pv = preview(&mut f, &p);
    assert_eq!(pv.report.rejected[0].reason, format!("opening overlaps opening {b}"));
    refused_once("ovl2", &[(OP, a, "along [m]", 2.5)], OP, "opening", a);
}

#[test]
fn xlsx_wall_lowered_below_its_door_refused() {
    let mut f = fx("low");
    let (w, a) = (f.walls[0], f.doors[0]);
    let p = book(&f, "low", &[("Walls", w, "height [m]", 2.0)]);
    let pv = preview(&mut f, &p);
    let want = format!("wall height 2 m is below the top 2.1 m of opening {a}");
    assert_eq!(pv.report.rejected[0].reason, want);
    refused_once("low2", &[("Walls", w, "height [m]", 2.0)], "Walls", "wall", w);
}

#[test]
fn xlsx_wall_shortened_past_its_door_refused() {
    let mut f = fx("short");
    let (w, b) = (f.walls[0], f.doors[1]);
    let p = book(&f, "short", &[("Walls", w, "bx [m]", 3.5)]);
    let pv = preview(&mut f, &p);
    let want = format!("wall length 3.5 m is shorter than the end 3.9 m of opening {b}");
    assert_eq!(pv.report.rejected[0].reason, want);
    refused_once("short2", &[("Walls", w, "bx [m]", 3.5)], "Walls", "wall", w);
}

#[test]
fn xlsx_mixed_book_applies_the_rest_as_one_step() {
    let mut f = fx("mixed");
    let ([a, b], w2) = (f.doors, f.walls[1]);
    let cells = [
        (OP, a, "height [m]", 3.5),
        (OP, b, "width [m]", 1.0),
        ("Walls", w2, "height [m]", 3.2),
    ];
    let p = book(&f, "mixed", &cells);
    let mut pv = preview(&mut f, &p);
    assert_eq!(pv.report.rejected.len(), 1, "{:?}", pv.report.rejected);
    assert_eq!(pv.report.rejected[0].row, row_of(&p, OP, a));
    assert_eq!(pv.report.counts().changed, 2, "{:?}", pv.report.changes);
    let (a0, d0) = (opening_size(&f.ed, a), f.ed.undo_len());
    let r = apply_preview(&mut f.ed, &mut pv, xlsx::Units::Metric, Lang::En);
    assert!(matches!(r, Ok(Applied::Done { .. })), "{r:?}");
    assert_eq!(opening_size(&f.ed, a), a0, "the refused row reached the model");
    assert!((opening_size(&f.ed, b).1 - 1.0).abs() < 1e-9);
    assert!((wall_height(&f.ed, w2) - 3.2).abs() < 1e-9);
    assert_eq!(f.ed.undo_len(), d0 + 1, "one undo step");
    f.ed.undo();
    assert!((opening_size(&f.ed, b).1 - 0.9).abs() < 1e-9);
    assert!((wall_height(&f.ed, w2) - 3.0).abs() < 1e-9);
}

#[test]
fn xlsx_untouched_book_gives_zero_changes() {
    let mut f = fx("same");
    let p = book(&f, "same", &[]);
    let pv = preview(&mut f, &p);
    assert_eq!(pv.report.counts(), Default::default(), "{:?}", pv.report.changes);
    assert!(pv.report.rejected.is_empty(), "{:?}", pv.report.rejected);
    assert!(pv.report.edits.is_empty() && pv.report.batch().is_empty());
    assert!(pv.report.proposals.is_empty() && pv.report.conflicts.is_empty());
}

const FIRE: &str = "Fire rating";

/// `src` saved as `dst` with a new last column `FIRE` on `sheet`, text `v` in the row of `id`.
fn with_column(src: &Path, dst: &Path, sheet: &str, vals: &[(i64, &str)]) {
    use calamine::{open_workbook, Data, Reader, Xlsx};
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = rust_xlsxwriter::Workbook::new();
    for name in wb.sheet_names() {
        let mut g: Vec<Vec<Data>> =
            wb.worksheet_range(&name).unwrap().rows().map(|r| r.to_vec()).collect();
        if name == sheet {
            let c = g[0].len();
            for (i, row) in g.iter_mut().enumerate() {
                let id = match row.first() {
                    Some(Data::Float(x)) => Some(*x as i64),
                    Some(Data::Int(x)) => Some(*x),
                    _ => None,
                };
                let v = match (i, id) {
                    (0, _) => Some(FIRE),
                    (_, Some(id)) => vals.iter().find(|x| x.0 == id).map(|x| x.1),
                    _ => None,
                };
                if let Some(v) = v {
                    row.resize(c, Data::Empty);
                    row.push(Data::String(v.into()));
                }
            }
        }
        let ws = out.add_worksheet();
        ws.set_name(&name).unwrap();
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
    out.save(dst).unwrap();
}

/// New-parameter edits of the batch for element `id` of table `kind` (`None`: any element).
fn param_edits(rep: &xlsx::Report, kind: &str, id: Option<i64>) -> usize {
    let b = rep.batch();
    b.iter()
        .filter(|e| match e {
            Edit::ParamValue { param, kind: k, id: i, .. } => {
                param == FIRE && k == kind && id.map_or(true, |id| id == *i)
            }
            Edit::NewParam(s) => id.is_none() && s.name == FIRE,
            _ => false,
        })
        .count()
}

#[test]
fn refused_row_proposal_is_not_applied() {
    let mut f = fx("prop");
    let (w0, w1) = (f.walls[0], f.walls[1]);
    // w0 drops below its doors (refused); w1 is a plain edit; both fill the new column
    let cells = [("Walls", w0, "height [m]", 2.0), ("Walls", w1, "height [m]", 3.2)];
    let p0 = book(&f, "prop", &cells);
    let p = f.dir.join("prop-col.xlsx");
    with_column(&p0, &p, "Walls", &[(w0, "EI60"), (w1, "EI90")]);
    let mut pv = preview(&mut f, &p);
    let rep = &mut pv.report;
    assert_eq!(rep.rejected.len(), 1, "{:?}", rep.rejected);
    let row0 = row_of(&p, "Walls", w0);
    assert_eq!(rep.rejected[0].row, row0);
    assert_eq!(rep.proposals.len(), 1, "{:?}", rep.proposals);
    let pr = &mut rep.proposals[0];
    assert_eq!(pr.refused, vec![row0], "{pr:?}");
    assert!(!pr.blocked() && pr.values.iter().all(|v| v.2 != w0), "{pr:?}");
    pr.accept = true;
    let note = proposal_note(pr, false).unwrap();
    assert!(note.contains(&row0.to_string()) && !has_cyrillic(&note), "{note}");
    let ru = proposal_note(pr, true).unwrap();
    assert!(has_cyrillic(&ru) && ru.contains(&row0.to_string()), "{ru}");
    assert_eq!(param_edits(rep, "wall", Some(w0)), 0, "the refused row's value is in the batch");
    assert_eq!(param_edits(rep, "wall", Some(w1)), 1);
    assert_eq!(touching(rep, "wall", w0), 0);
    let (h0, d0) = (wall_height(&f.ed, w0), f.ed.undo_len());
    let r = apply_preview(&mut f.ed, &mut pv, xlsx::Units::Metric, Lang::En);
    assert!(matches!(r, Ok(Applied::Done { .. })), "{r:?}");
    assert_eq!(wall_height(&f.ed, w0), h0, "the refused row reached the model");
    assert!((wall_height(&f.ed, w1) - 3.2).abs() < 1e-9);
    let d = f.ed.model.param_by_name(FIRE).unwrap().expect("the accepted column");
    let vals = f.ed.model.param_values(d.id).unwrap();
    assert!(!vals.contains_key(&("wall".to_string(), w0)), "{vals:?}");
    assert!(vals.contains_key(&("wall".to_string(), w1)), "{vals:?}");
    assert_eq!(f.ed.undo_len(), d0 + 1, "one undo step");
    f.ed.undo();
    assert!(f.ed.model.param_by_name(FIRE).unwrap().is_none());
    assert!((wall_height(&f.ed, w1) - 3.0).abs() < 1e-9);
}

#[test]
fn proposal_of_only_refused_rows_is_blocked() {
    let mut f = fx("propall");
    let w0 = f.walls[0];
    let p0 = book(&f, "propall", &[("Walls", w0, "height [m]", 2.0)]);
    let p = f.dir.join("propall-col.xlsx");
    with_column(&p0, &p, "Walls", &[(w0, "EI60")]);
    let mut pv = preview(&mut f, &p);
    let rep = &mut pv.report;
    assert_eq!(rep.rejected.len(), 1, "{:?}", rep.rejected);
    assert_eq!(rep.proposals.len(), 1, "{:?}", rep.proposals);
    let pr = &mut rep.proposals[0];
    assert!(pr.blocked() && !pr.accept, "{pr:?}");
    // a tick from an earlier read cannot add the parameter or any value
    pr.accept = true;
    let (en, ru) = (proposal_note(pr, false).unwrap(), proposal_note(pr, true).unwrap());
    assert!(en != ru && !has_cyrillic(&en) && has_cyrillic(&ru), "{en} / {ru}");
    assert_eq!(param_edits(rep, "wall", None), 0, "{:?}", rep.batch());
    assert_eq!(rep.accepted(), 0);
    assert!(rep.batch().is_empty(), "{:?}", rep.batch());
}

#[test]
fn every_reason_reads_in_both_languages() {
    let tall = || Box::new(PlaceError::TooTall { top: 2.1, height: 2.0 });
    let out = || Box::new(PlaceError::Outside { len: 3.5, from: 3.0, to: 3.9 });
    let all = [
        PlaceError::NoWall,
        PlaceError::NonPositive,
        *out(),
        *tall(),
        PlaceError::Overlap { other: 7 },
        PlaceError::Overlap { other: -12 },
        PlaceError::Hosts { opening: 7, err: tall() },
        PlaceError::Hosts { opening: 7, err: out() },
        PlaceError::Hosts { opening: 7, err: Box::new(PlaceError::NonPositive) },
    ];
    let mut seen = std::collections::HashSet::new();
    for e in &all {
        let en = reason(e);
        assert!(seen.insert(en.clone()), "two refusals read the same: {en}");
        assert_eq!(xl_text::tr(&en, Lang::En), en);
        let ru = xl_text::tr(&en, Lang::Ru);
        assert!(has_cyrillic(&ru), "{en:?} has no Russian template: {ru:?}");
        for n in en.split(' ').filter(|w| w.parse::<f64>().is_ok()) {
            assert!(ru.contains(n), "{ru:?} lost {n}");
        }
    }
    for key in ["xlval.pe_hosts", "xlval.walls_refused"] {
        let (en, ru) = (crate::tr_str(false, key), crate::tr_str(true, key));
        assert!(en != key && ru != key && en != ru && has_cyrillic(ru), "{key}");
    }
    let h = PlaceError::Hosts { opening: 7, err: tall() };
    let u = crate::units::Units::Metric;
    assert!(h.text(false, u).contains("#7") && has_cyrillic(&h.text(true, u)));
}

#[test]
fn nan_zero_negative_widths_refused_by_screen() {
    let f = fx("nan");
    let fit = Fit::load(&f.ed.model).unwrap();
    for w in [f64::NAN, 0.0, -0.01] {
        let spec = OpeningSpec { width: w, ..door(1.0) };
        let e = vec![Edit::Opening { id: f.doors[0], wall: f.walls[0], spec }];
        let r = screen(&fit, &e);
        assert_eq!(r.len(), 1, "{w}");
        assert_eq!(r[0].why, PlaceError::NonPositive, "{w}");
    }
}

#[test]
fn wall_and_its_door_changed_together_are_checked_as_they_will_stand() {
    let f = fx("both");
    let ([a, _], w) = (f.doors, f.walls[0]);
    let fit = Fit::load(&f.ed.model).unwrap();
    let wall = |h: f64| {
        let (spec, bulge) = match crate::shell::panels::wall_edit(&f.ed.model, w) {
            Some(Edit::Wall { mut spec, bulge, .. }) => {
                spec.height = h;
                (spec, bulge)
            }
            _ => panic!("no wall"),
        };
        Edit::Wall { id: w, spec, bulge }
    };
    let op = |h: f64| {
        let spec = OpeningSpec { height: h, ..door(1.0) };
        Edit::Opening { id: a, wall: w, spec }
    };
    // higher wall, taller door: both fit
    assert!(screen(&fit, &[wall(4.0), op(3.5)]).is_empty());
    // lower wall, lower door A: door B still stands at 2.1 m, so the wall is refused and A,
    // now checked against the 3 m wall, still fits
    let r = screen(&fit, &[wall(2.0), op(1.8)]);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!((r[0].index, r[0].id), (0, w));
    assert!(matches!(r[0].why, PlaceError::Hosts { opening, .. } if opening == f.doors[1]));
}

#[test]
fn panel_wall_height_keeps_hosted_doors() {
    let mut f = fx("panel");
    let (w, a) = (f.walls[0], f.doors[0]);
    let d0 = f.ed.undo_len();
    assert!(!crate::shell::panels::edit_walls(&mut f.ed, &[w], None, Some(2.0)));
    assert!((wall_height(&f.ed, w) - 3.0).abs() < 1e-9, "the lower wall reached the model");
    assert_eq!(f.ed.undo_len(), d0, "a refused edit leaves no undo step");
    let c = Counts { refused_walls: 1, ..Counts::default() };
    let tall = Box::new(PlaceError::TooTall { top: 2.1, height: 2.0 });
    let e = PlaceError::Hosts { opening: a, err: tall };
    let t = e.text(crate::ui_is_ru(), f.ed.tool.units);
    let why = crate::tr("lnp.why_at", &[("id", &w.to_string()), ("e", &t)]);
    let want = counts_text_with(&c, "type_dialog.status.walls_changed", None, Some(&why));
    assert_eq!(f.ed.status_severity(), Some(crate::status::Severity::Refusal));
    assert_eq!(f.ed.status.as_deref(), Some(want.1.as_str()));
    // a door-free wall and a height the doors still fit go through, one step
    assert!(crate::shell::panels::edit_walls(&mut f.ed, &[w, f.walls[1]], None, Some(2.5)));
    assert!((wall_height(&f.ed, w) - 2.5).abs() < 1e-9);
    assert_eq!(f.ed.undo_len(), d0 + 1);
    // mixed: the wall with doors is refused, the other one changes
    assert!(crate::shell::panels::edit_walls(&mut f.ed, &[w, f.walls[1]], None, Some(1.5)));
    assert!((wall_height(&f.ed, w) - 2.5).abs() < 1e-9);
    assert!((wall_height(&f.ed, f.walls[1]) - 1.5).abs() < 1e-9);
    assert_eq!(f.ed.status_severity(), Some(crate::status::Severity::Warning));
}

#[test]
fn ai_wall_height_that_drops_a_door_is_refused() {
    use crate::ai::plan::{compile_one, CompileCx, ElRef, PlanOp, Targets};
    use crate::query::{Assign, Val};
    let f = fx("ai");
    let w = f.walls[0];
    // the AI drafts on a copy of the model, as the session's open_scratch makes it
    let snap = std::env::temp_dir().join(format!("ddc-fit-ai-{}.db", std::process::id()));
    f.ed.model.snapshot_to(&snap).unwrap();
    let mut scratch = Editor::new_in_memory().unwrap();
    scratch.replace_model(ddc_model::Model::open_copy(&snap.to_string_lossy()).unwrap()).unwrap();
    scratch.set_level(f.ed.level);
    let sets = crate::ai::sets::Sets::default();
    let mut slots = std::collections::HashMap::new();
    let mut cx = CompileCx {
        ed: &mut scratch,
        slots: &mut slots,
        sets: &sets,
        gens: &[],
        rules: &[],
        declared: std::collections::HashSet::new(),
        skips: vec![],
    };
    let op = |h: &str| PlanOp::Assign {
        target: Targets::Refs(vec![ElRef::parse(&format!("wall:{w}")).unwrap()]),
        set: vec![Assign { key: "height".into(), val: Val::Lit(h.into()) }],
    };
    let Err(crate::ai::plan::PlanError(err)) = compile_one(&mut cx, &op("1500")) else {
        panic!("a 1.5 m wall under 2.1 m doors compiled");
    };
    assert!(err.contains(&format!("wall {w} cannot take this size")), "{err}");
    assert!((wall_height(cx.ed, w) - 3.0).abs() < 1e-9, "a refused item leaves the draft as it was");
    assert!(compile_one(&mut cx, &op("2500")).is_ok());
    assert!((wall_height(&scratch, w) - 2.5).abs() < 1e-9, "the size is drafted on the copy");
    assert!((wall_height(&f.ed, w) - 3.0).abs() < 1e-9, "compile never writes the model");
    drop(scratch);
    let _ = std::fs::remove_file(&snap);
}
