// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Apply from a book read through a sheet alias: a re-export would put a fresh known sheet beside
//! the user's renamed one, so the rows the Apply created or matched get their ids in place, in the
//! user's own sheet. The new book is written beside it, read back, and moved over it only when
//! the book is still the version the Apply read and no other program holds it.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ddc_io::xlsx::{self, IdCell, MatchState, Report, Stamp};
use ddc_model::Model;

/// Refusal of an Apply whose new rows could not get their ids in place (the UI shows `refused_text`).
pub const REFUSED: &str = "new rows wait: their sheet cannot get ids while a renamed sheet is read";

/// How the write of the ids ended.
#[derive(Debug, PartialEq)]
pub enum Wrote {
    /// No row needed an id.
    Nothing,
    /// `n` ids are in the book, which is now at `stamp`.
    Done { n: usize, stamp: Option<Stamp> },
    /// Another program holds the book: nothing written.
    Open,
    /// Refused or failed (English reason); the book is as it was.
    Failed(String),
}

/// Sheets whose new rows the in-place write cannot give an id; Apply waits while any is listed.
pub fn refused_sheets(r: &Report, alias: &HashMap<String, String>) -> Vec<String> {
    if alias.is_empty() {
        return vec![];
    }
    let mut out: Vec<String> = r
        .changes
        .iter()
        .filter(|c| c.kind() == "added" && !xlsx::ID_SHEETS.contains(&c.sheet.as_str()))
        .map(|c| c.sheet.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn refused_text(sheets: &[String], ru: bool) -> String {
    let list = sheets.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", ");
    crate::tr_lang(ru, "xlsx_alias_ids.refused", &[("sheets", &list)])
}

/// Rows matched by key that the user accepted: (sheet, row, element).
pub fn matched(r: &Report) -> Vec<(String, u32, i64)> {
    r.matches
        .iter()
        .filter(|m| m.state == MatchState::Accepted && xlsx::ID_SHEETS.contains(&m.sheet.as_str()))
        .map(|m| (m.sheet.clone(), m.row, m.id))
        .collect()
}

/// Ids of the rows an Apply made (from the records it left) and matched. Also returns the made
/// rows that got an id and the count of made rows whose element is unknown.
pub fn cells(
    model: &Model,
    book: &Path,
    made: &[(String, u32, String)],
    matched: &[(String, u32, i64)],
) -> (Vec<IdCell>, Vec<(String, u32, String)>, usize) {
    let rec = super::xlsx_links::load(model, book);
    let mut out = Vec::new();
    let mut paired = Vec::new();
    let mut unpaired = 0;
    for (sheet, row, fp) in made {
        if !xlsx::ID_SHEETS.contains(&sheet.as_str()) {
            continue;
        }
        let id = rec.get(&(sheet.clone(), fp.clone())).and_then(|v| v.iter().find(|h| h.0 == *row)).map(|h| h.1);
        match id {
            Some(id) if id > 0 => {
                out.push(IdCell { sheet: sheet.clone(), row: *row, id });
                paired.push((sheet.clone(), *row, fp.clone()));
            }
            _ => unpaired += 1,
        }
    }
    for (sheet, row, id) in matched {
        if !out.iter().any(|c| c.sheet == *sheet && c.row == *row) {
            out.push(IdCell { sheet: sheet.clone(), row: *row, id: *id });
        }
    }
    (out, paired, unpaired)
}

fn part_of(book: &Path) -> PathBuf {
    ddc_io::fsutil::part_path(book)
}

/// Removes the part on every way out, a panic in the write included; after the commit it is gone.
struct PartGuard(PathBuf);

impl Drop for PartGuard {
    fn drop(&mut self) {
        if self.0.exists() {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// Writes `cells` into `book` in place; `read` is the version of the book the Apply came from.
pub fn write_in_place(book: &Path, read: Option<Stamp>, alias: &HashMap<String, String>, cells: &[IdCell]) -> Wrote {
    if cells.is_empty() {
        return Wrote::Nothing;
    }
    if super::xlsx_owner::check_replace(book).blocks() {
        return Wrote::Open;
    }
    let changed = || xlsx::book_changed(read, Stamp::of(book));
    if changed() {
        return Wrote::Failed(super::xlsx_job::BOOK_CHANGED.into());
    }
    let guard = PartGuard(part_of(book));
    let part = guard.0.as_path();
    let n = match xlsx::write_ids(book, part, alias, cells) {
        Ok(n) => n,
        Err(e) => return Wrote::Failed(e),
    };
    // saved by the user, or opened by another program, while the part was written: theirs stays
    if changed() {
        return Wrote::Failed(super::xlsx_job::BOOK_CHANGED.into());
    }
    if super::xlsx_owner::check_replace(book).blocks() {
        return Wrote::Open;
    }
    match ddc_io::fsutil::commit_part(part, book) {
        Ok(()) => Wrote::Done { n, stamp: Stamp::of(book) },
        Err(e) => {
            let t = super::xlsx_owner::failed(&e);
            if t == super::xlsx_owner::OPEN_ELSEWHERE {
                Wrote::Open
            } else {
                Wrote::Failed(t)
            }
        }
    }
}

/// Status line of a write: None when nothing needed an id and every new row is known.
pub fn status(w: &Wrote, unpaired: usize, wanted: usize, ru: bool) -> Option<String> {
    let mut parts = Vec::new();
    match w {
        Wrote::Nothing => {}
        Wrote::Done { n, .. } => parts.push(crate::tr_lang(ru, "xlsx_alias_ids.ids_written", &[("n", &n.to_string())])),
        Wrote::Open => parts.push(crate::tr_lang(ru, "xlsx_alias_ids.ids_open", &[("n", &wanted.to_string())])),
        Wrote::Failed(_) => parts.push(crate::tr_lang(ru, "xlsx_alias_ids.ids_failed", &[("n", &wanted.to_string())])),
    }
    if unpaired > 0 {
        parts.push(crate::tr_lang(ru, "xlsx_alias_ids.unpaired", &[("n", &unpaired.to_string())]));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// After the Apply step of a book read through an alias: the ids go into the book and the made
/// rows that got one are forgotten (the others stay remembered, so they are not created again).
pub fn after_apply(
    model: &Model,
    book: &Path,
    read: Option<Stamp>,
    alias: &HashMap<String, String>,
    made: &[(String, u32, String)],
    matched: &[(String, u32, i64)],
    ru: bool,
) -> AfterApply {
    let (cells, paired, unpaired) = cells(model, book, made, matched);
    let w = write_in_place(book, read, alias, &cells);
    let text = status(&w, unpaired, cells.len(), ru);
    let (in_sync, stamp) = match &w {
        Wrote::Done { stamp, .. } => {
            super::xlsx_links::forget(model, book, &paired);
            (unpaired == 0, *stamp)
        }
        Wrote::Nothing => (unpaired == 0, None),
        Wrote::Open | Wrote::Failed(_) => (false, None),
    };
    AfterApply { in_sync, stamp, text }
}

/// What the Apply step takes from the write of the ids.
#[derive(Debug)]
pub struct AfterApply {
    /// Every row the Apply made or matched has its id in the book.
    pub in_sync: bool,
    /// The book's stamp right after the write; None when it was not written.
    pub stamp: Option<Stamp>,
    pub text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::super::xlsx_job::Outcome;
    use super::super::xlsx_win::{apply_begin, apply_finish, XlsxPreview};
    use super::*;
    use crate::editor::Editor;
    use crate::xl_text::Lang;
    use calamine::{open_workbook, Data, Reader, Xlsx};

    type Grid = Vec<Vec<Data>>;

    const MINE: &str = "My walls";
    const H: &str = "height [m]";

    fn editor() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.level;
        let t = ed
            .model
            .add_wall_type("ext", &[ddc_model::LayerSpec { material: "brick".into(), thickness: 0.25 }])
            .unwrap();
        for (a, b) in [((0., 0.), (6., 0.)), ((6., 0.), (6., 4.))] {
            ed.model
                .add_wall(&ddc_model::WallSpec { a, b, level: lv, base_offset: 0.0, height: 3.0, wall_type: t, offset0: None })
                .unwrap();
        }
        ed.reload().unwrap();
        ed
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc_xs11b_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn grids(p: &Path) -> Vec<(String, Grid)> {
        let mut wb: Xlsx<_> = open_workbook(p).unwrap();
        wb.sheet_names()
            .into_iter()
            .map(|n| {
                let g: Grid = wb.worksheet_range(&n).unwrap().rows().map(|r| r.to_vec()).collect();
                (n, g)
            })
            .collect()
    }

    fn save(dst: &Path, sheets: &[(String, Grid)]) {
        let mut out = rust_xlsxwriter::Workbook::new();
        for (name, grid) in sheets {
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

    fn col(g: &Grid, header: &str) -> usize {
        g[0].iter().position(|d| d.to_string() == header).unwrap()
    }

    /// The exported book as the user keeps it: Walls renamed `MINE`, the first wall 4 m high, a
    /// new id-less wall in book row 4 (when `add`) and a sheet of their own.
    fn user_book(ed: &Editor, tag: &str, add: bool) -> PathBuf {
        let d = dir(tag);
        let src = d.join("export.xlsx");
        xlsx::export_xlsx(&ed.model, &src).unwrap();
        let mut sheets = grids(&src);
        for (n, g) in &mut sheets {
            if n.as_str() == "Walls" {
                let h = col(g, H);
                g[1][h] = Data::Float(4.0);
                if add {
                    let mut extra = g[1].clone();
                    extra[0] = Data::Empty;
                    for k in ["ax [m]", "bx [m]"] {
                        let c = col(g, k);
                        extra[c] = Data::Float(20.0 + c as f64);
                    }
                    // a read-only column of the copied row would not match the new wall
                    if let Some(c) = g[0].iter().position(|d| d.to_string().starts_with("length")) {
                        extra[c] = Data::Empty;
                    }
                    g.push(extra);
                }
                *n = MINE.to_string();
            }
        }
        sheets.push(("Notes".into(), vec![vec![Data::String("keep me".into())]]));
        let p = d.join("plan.xlsx");
        save(&p, &sheets);
        p
    }

    fn preview(ed: &mut Editor, p: &Path) -> XlsxPreview {
        let mut pv = XlsxPreview::load(&mut ed.model, p, false).unwrap();
        pv.read_as(&mut ed.model, "Walls", Some(MINE));
        pv.rev = ed.content_snap();
        super::super::xlsx_no_base::confirm(&mut pv);
        pv
    }

    fn walls(ed: &Editor) -> i64 {
        ed.model.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap()
    }

    fn newest(ed: &Editor) -> i64 {
        ed.model.sql().query_row("SELECT max(id) FROM wall", [], |r| r.get(0)).unwrap()
    }

    fn first_height(ed: &Editor) -> f64 {
        ed.model.sql().query_row("SELECT height FROM wall ORDER BY id LIMIT 1", [], |r| r.get(0)).unwrap()
    }

    /// Apply as the window does it, with the tail the re-export would get.
    fn apply(ed: &mut Editor, pv: &mut XlsxPreview) -> super::super::xlsx_job::ApplyTail {
        let opts = apply_begin(ed, pv, Lang::En).unwrap().unwrap();
        let tail = opts.apply.clone().unwrap();
        apply_finish(ed.content_snap(), &mut ed.model, pv, &tail, &Outcome::Cancelled);
        tail
    }

    fn id_cell(p: &Path, row: usize) -> Data {
        let g = grids(p).into_iter().find(|s| s.0 == MINE).unwrap().1;
        g[row][0].clone()
    }

    #[test]
    fn new_row_gets_its_id_in_the_renamed_sheet_and_a_second_sync_adds_nothing() {
        let mut ed = editor();
        let p = user_book(&ed, "new", true);
        let before = grids(&p);
        let mut pv = preview(&mut ed, &p);
        assert_eq!(pv.report.counts().added, 1, "{:?}", pv.report.changes);
        let n = walls(&ed);
        let tail = apply(&mut ed, &mut pv);
        assert!(tail.skip && tail.ids_in_place, "{tail:?}");
        assert_eq!(walls(&ed), n + 1);
        assert_eq!(first_height(&ed), 4.0, "the edit of an existing row applied");
        assert!(pv.synced);
        let id = newest(&ed);
        assert!(matches!(id_cell(&p, 3), Data::Float(x) if x == id as f64), "{:?}", id_cell(&p, 3));
        // same sheets, no fresh known sheet beside the renamed one, the rest as the user saved it
        let after = grids(&p);
        assert_eq!(before.iter().map(|s| &s.0).collect::<Vec<_>>(), after.iter().map(|s| &s.0).collect::<Vec<_>>());
        for ((name, a), (_, b)) in before.iter().zip(&after) {
            if name == MINE {
                assert_eq!(a[..3], b[..3]);
                assert_eq!(a[3][1..], b[3][1..]);
            } else if name == "Baseline" {
                assert_eq!(a[..], b[..a.len()], "the export's baseline rows stay");
            } else {
                assert_eq!(a, b, "sheet {name} untouched");
            }
        }
        // the baseline knows the element now, so deleting its row later deletes it
        let info = xlsx::book_info(&p).unwrap().unwrap();
        assert!(info.ids.walls.contains(&id), "{:?}", info.ids.walls);
        assert!(crate::shell::xlsx_links::load(&ed.model, &p).is_empty(), "the row has its id now");
        // the next Sync of the same book: the row is the element, nothing is created again
        pv.reload(&mut ed.model);
        assert_eq!(pv.report.counts().added, 0, "{:?}", pv.report.changes);
        assert!(pv.report.changes.is_empty(), "{:?}", pv.report.changes);
        pv.rev = ed.content_snap();
        let _ = apply_begin(&mut ed, &mut pv, Lang::En);
        assert_eq!(walls(&ed), n + 1, "nothing is created twice");
    }

    #[test]
    fn edits_only_leave_the_book_as_it_is() {
        let mut ed = editor();
        let p = user_book(&ed, "edit", false);
        let bytes = std::fs::read(&p).unwrap();
        let mut pv = preview(&mut ed, &p);
        assert_eq!(pv.report.counts().added, 0);
        let tail = apply(&mut ed, &mut pv);
        assert!(tail.skip && tail.ids_in_place, "no re-export, and the book needs nothing");
        assert!(pv.synced);
        assert_eq!(first_height(&ed), 4.0);
        assert_eq!(std::fs::read(&p).unwrap(), bytes, "nothing to write: the book is not touched");
        assert!(!grids(&p).iter().any(|s| s.0 == "Walls"), "no stale duplicate sheet");
    }

    #[test]
    fn open_book_is_not_written_and_the_line_says_so() {
        let ed = editor();
        let p = user_book(&ed, "open", true);
        let bytes = std::fs::read(&p).unwrap();
        let owner = super::super::xlsx_owner::owner_file(&p);
        std::fs::write(&owner, b"owner").unwrap();
        let alias = HashMap::from([("Walls".to_string(), MINE.to_string())]);
        let cells = [IdCell { sheet: "Walls".into(), row: 4, id: 99 }];
        let w = write_in_place(&p, Stamp::of(&p), &alias, &cells);
        assert_eq!(w, Wrote::Open);
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
        assert!(!part_of(&p).exists());
        let line = status(&w, 0, 1, false).unwrap();
        assert!(line.contains("open in another program"), "{line}");
        assert_ne!(status(&w, 0, 1, true), status(&w, 0, 1, false), "both languages have the line");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn share_locked_book_is_not_written() {
        use std::os::windows::fs::OpenOptionsExt;
        let ed = editor();
        let p = user_book(&ed, "lock", true);
        let bytes = std::fs::read(&p).unwrap();
        let alias = HashMap::from([("Walls".to_string(), MINE.to_string())]);
        let cells = [IdCell { sheet: "Walls".into(), row: 4, id: 99 }];
        let stamp = Stamp::of(&p);
        let hold = std::fs::OpenOptions::new().read(true).share_mode(0).open(&p).unwrap();
        assert_eq!(write_in_place(&p, stamp, &alias, &cells), Wrote::Open);
        drop(hold);
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
    }

    #[test]
    fn refused_write_leaves_the_book_and_no_part() {
        let ed = editor();
        let p = user_book(&ed, "refused", true);
        let bytes = std::fs::read(&p).unwrap();
        let alias = HashMap::from([("Walls".to_string(), MINE.to_string())]);
        // book row 2 already holds the id of the first wall
        let cells = [IdCell { sheet: "Walls".into(), row: 2, id: 99 }];
        assert!(matches!(write_in_place(&p, Stamp::of(&p), &alias, &cells), Wrote::Failed(_)));
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
        assert!(!part_of(&p).exists());
        // a stale part of an earlier crash is replaced and gone after a good write
        std::fs::write(part_of(&p), b"stale").unwrap();
        let cells = [IdCell { sheet: "Walls".into(), row: 4, id: 99 }];
        assert!(matches!(write_in_place(&p, Stamp::of(&p), &alias, &cells), Wrote::Done { n: 1, .. }));
        assert!(!part_of(&p).exists());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn book_saved_since_the_read_is_not_written() {
        let ed = editor();
        let p = user_book(&ed, "saved", true);
        let alias = HashMap::from([("Walls".to_string(), MINE.to_string())]);
        let cells = [IdCell { sheet: "Walls".into(), row: 4, id: 99 }];
        let old = Stamp::of(&p);
        let mut bytes = std::fs::read(&p).unwrap();
        bytes.extend_from_slice(b"  ");
        std::fs::write(&p, &bytes).unwrap();
        assert!(matches!(write_in_place(&p, old, &alias, &cells), Wrote::Failed(_)));
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
    }

    #[test]
    fn undo_of_the_apply_creates_nothing_on_the_next_sync() {
        let mut ed = editor();
        let p = user_book(&ed, "undo", true);
        let mut pv = preview(&mut ed, &p);
        let n = walls(&ed);
        let tail = apply(&mut ed, &mut pv);
        assert!(tail.ids_in_place);
        let id = newest(&ed);
        ed.undo();
        assert_eq!(walls(&ed), n);
        assert_eq!(first_height(&ed), 3.0, "the edit is undone with the new row");
        // the row now names an element that is gone: it is listed, never created silently
        pv.reload(&mut ed.model);
        assert_eq!(pv.report.counts().added, 0, "{:?}", pv.report.changes);
        let gone = format!("{id} no longer exists");
        assert!(pv.report.ignored.iter().any(|i| i.reason.contains(&gone)), "{:?}", pv.report.ignored);
        ed.redo();
        assert_eq!(walls(&ed), n + 1);
    }

    #[test]
    fn new_rows_of_a_sheet_without_ids_are_refused_before_anything_applies() {
        let mut r = Report::default();
        r.changes.push(xlsx::Change {
            sheet: "Columns".into(),
            row: 3,
            col: 0,
            column: "id".into(),
            id: None,
            old: String::new(),
            new: "created".into(),
        });
        assert_eq!(r.changes[0].kind(), "added");
        let alias = HashMap::from([("Walls".to_string(), MINE.to_string())]);
        assert!(refused_sheets(&r, &HashMap::new()).is_empty(), "no alias: the re-export gives ids");
        assert_eq!(refused_sheets(&r, &alias), vec!["Columns".to_string()]);
        let en = refused_text(&refused_sheets(&r, &alias), false);
        assert!(en.contains("'Columns'"), "{en}");
        assert_ne!(en, refused_text(&refused_sheets(&r, &alias), true));
        // a new row of a sheet that has ids is not refused
        r.changes[0].sheet = "Walls".into();
        assert!(refused_sheets(&r, &alias).is_empty());
    }
}
