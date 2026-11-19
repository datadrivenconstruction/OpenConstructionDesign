//! The rows of the workbook preview as a list the user can take away: copied as text for a
//! spreadsheet, or saved as a .csv file next to the book, so refused and conflicting cells can
//! be fixed in the book one by one. Both follow the filters of the change list.
use super::xl_diff::{self, DiffState, Item, Kind};
use crate::xl_text::{self, Lang};
use ddc_io::xlsx::Report;
use ddc_ui::widgets::Button;
use ddc_ui::Theme;
use ddc_geom::csv;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Columns of the list: sheet, cell, element id, column, book value, model value, reason.
pub const COLS: usize = 7;
pub type Row = [String; COLS];

/// Header row in the given language.
pub fn header(lang: Lang) -> Row {
    let ru = lang == Lang::Ru;
    [
        "xlw.head_sheet",
        "xlw.head_cell",
        "xlw.head_id",
        "xlw.head_column",
        "xlw.list_book_value",
        "xlw.list_model_value",
        "xlw.list_reason",
    ]
    .map(|k| crate::tr_lang(ru, k, &[]))
}

/// One list row per entry, in the order given.
pub fn issue_rows<'a>(items: impl Iterator<Item = Item<'a>>, lang: Lang) -> Vec<Row> {
    let ru = lang == Lang::Ru;
    items
        .map(|it| {
            let val = |v: &str| super::xlsx_win::shown_value(it.sheet, it.column, v);
            let (book, model) = match it.kind {
                Kind::Changed | Kind::Conflict | Kind::Locked | Kind::Ignored => (val(it.now), val(it.was)),
                _ => (String::new(), String::new()),
            };
            let reason = if !it.reason.is_empty() {
                xl_text::tr(it.reason, lang)
            } else {
                let key = match it.kind {
                    Kind::Added => "xlw.create",
                    Kind::Deleted => "xlw.delete",
                    Kind::Refused => "xlw.kind_refused",
                    Kind::Skipped => "xlw.skipped",
                    Kind::Conflict => "xlw.conflicts",
                    Kind::Changed => "xlw.changed",
                    Kind::Warning => "xlw.kind_warning",
                    Kind::Locked => "xlw.kind_locked",
                    Kind::Ignored => "xlw.kind_ignored",
                };
                crate::tr_lang(ru, key, &[])
            };
            [
                xl_text::sheet_name(it.sheet, lang),
                xl_diff::short_address(&it),
                it.id.map(|i| i.to_string()).unwrap_or_default(),
                it.column.to_string(),
                book,
                model,
                reason,
            ]
        })
        .collect()
}

/// Rows passing the current filters of the change list.
pub fn filtered_rows(diff: &DiffState, rep: &Report, lang: Lang) -> Vec<Row> {
    issue_rows(diff.view_items(rep), lang)
}

/// Field separator of the .csv file: spreadsheets with a Russian locale split on ';' (the comma
/// is their decimal mark), others on ','.
pub fn csv_sep(lang: Lang) -> char {
    csv::Style::for_lang(lang == Lang::Ru).sep
}

fn join(header: &Row, rows: &[Row], sep: char, eol: &str) -> String {
    csv::join(std::iter::once(header).chain(rows.iter()), sep, eol)
}

/// Text for the clipboard: tab separated, header first, one row per line.
pub fn to_tsv(header: &Row, rows: &[Row]) -> String {
    join(header, rows, '\t', "\n")
}

/// Content of the .csv file: UTF-8 with a byte order mark, [`csv_sep`] separated, CRLF lines.
pub fn to_csv(header: &Row, rows: &[Row], lang: Lang) -> String {
    csv::file(std::iter::once(header).chain(rows.iter()), csv::Style::for_lang(lang == Lang::Ru))
}

/// `<book>-issues.csv`, `<book>-issues-2.csv`, ... beside the book.
fn candidate(book: &Path, n: u32) -> PathBuf {
    let stem = book
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "book".to_string());
    let tail = if n <= 1 { String::new() } else { format!("-{n}") };
    book.with_file_name(format!("{stem}-issues{tail}.csv"))
}

/// Write `text` next to `book` under a name that is not taken yet: through a temporary file in
/// the same folder, then a link that fails when the name appeared meanwhile. Nothing existing,
/// the book included, is replaced. Returns the path written.
pub fn save_csv(book: &Path, text: &str) -> std::io::Result<PathBuf> {
    let first = candidate(book, 1);
    let tmp = first.with_file_name(format!(".{}.{}.tmp", first.file_name().unwrap_or_default().to_string_lossy(), std::process::id()));
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    let written = f.write_all(text.as_bytes()).and_then(|_| f.sync_all());
    drop(f);
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let mut last = std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free file name");
    for n in 1..1000 {
        let target = candidate(book, n);
        if target == book || target.exists() {
            continue;
        }
        match std::fs::hard_link(&tmp, &target) {
            Ok(()) => {
                let _ = std::fs::remove_file(&tmp);
                return Ok(target);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                // no links on this volume: rename, which is safe as the name was just seen free
                if !target.exists() && std::fs::rename(&tmp, &target).is_ok() {
                    return Ok(target);
                }
                last = e;
                break;
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last)
}

/// Copy and save buttons under the change list; the status line text of what was done.
pub fn toolbar(ui: &mut egui::Ui, th: &Theme, diff: &DiffState, rep: &Report, book: &Path, lang: Lang) -> Option<String> {
    let ru = lang == Lang::Ru;
    let t = |k: &'static str| crate::tr_str(ru, k);
    let any = diff.shown() > 0;
    let mut msg = None;
    ui.horizontal(|ui| {
        let copy = ui.add_enabled_ui(any, |ui| Button::quiet(t("xlw.copy_list")).show(ui, th)).inner;
        let copy = copy.on_hover_text(t("xlw.copy_list_tip"));
        let save = ui.add_enabled_ui(any, |ui| Button::quiet(t("xlw.save_list")).show(ui, th)).inner;
        let save = save.on_hover_text(t("xlw.save_list_tip"));
        if !copy.clicked() && !save.clicked() {
            return;
        }
        let rows = filtered_rows(diff, rep, lang);
        let n = rows.len().to_string();
        if rows.is_empty() {
            msg = Some(t("xlw.list_nothing").to_string());
        } else if copy.clicked() {
            ui.ctx().copy_text(to_tsv(&header(lang), &rows));
            msg = Some(crate::tr_lang(ru, "xlw.list_copied", &[("n", &n)]));
        } else {
            msg = Some(match save_csv(book, &to_csv(&header(lang), &rows, lang)) {
                Ok(p) => {
                    let file = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    crate::tr_lang(ru, "xlw.list_saved", &[("n", &n), ("file", &file)])
                }
                Err(e) => format!("{}: {e}", t("xlw.list_save_failed")),
            });
        }
    });
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use ddc_geom::csv::field;
    use ddc_io::xlsx::{Change, Conflict, Rejection};
    use super::xl_diff::Ctx;

    fn row(cells: [&str; COLS]) -> Row {
        cells.map(str::to_string)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-xs7-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn reject(sheet: &str, row: u32, col: u32, column: &str, reason: &str) -> Rejection {
        Rejection {
            sheet: sheet.into(),
            row,
            col,
            column: column.into(),
            reason: reason.into(),
        }
    }

    fn sample() -> Report {
        let mut r = Report::default();
        r.rejected.push(reject("Walls", 7, 27, "mark", "not a number"));
        r.rejected.push(reject("Walls", 8, 2, "mark", "bad"));
        r.changes.push(Change {
            sheet: "Doors".into(),
            row: 3,
            col: 4,
            column: "mark".into(),
            id: Some(12),
            old: "a".into(),
            new: "b".into(),
        });
        r.conflicts.push(Conflict {
            sheet: "Doors".into(),
            row: 5,
            col: 4,
            column: "mark".into(),
            id: Some(13),
            base: "o".into(),
            model: "m".into(),
            book: "k".into(),
            take_book: false,
        });
        r
    }

    #[test]
    fn column_27_is_aa_and_row_has_seven_fields() {
        let r = sample();
        let ed = Editor::new_in_memory().unwrap();
        let cx = Ctx { rep: &r, model: &ed.model, gen: 0, rev: ed.cache_key(), lang: Lang::En };
        let mut d = DiffState::default();
        d.ensure(&cx);
        let rows = filtered_rows(&d, &r, Lang::En);
        assert_eq!(rows.len(), 4);
        let first = rows.iter().find(|x| x[1] == "AA7").expect("AA7 row");
        assert_eq!(first[0], "Walls");
        assert_eq!(first[3], "mark");
        assert_eq!(first[6], "not a number");
        let ch = rows.iter().find(|x| x[2] == "12").unwrap();
        assert_eq!((ch[4].as_str(), ch[5].as_str()), ("b", "a"));
        let co = rows.iter().find(|x| x[2] == "13").unwrap();
        assert_eq!((co[4].as_str(), co[5].as_str()), ("k", "m"));
    }

    #[test]
    fn rows_follow_the_filters() {
        let r = sample();
        let ed = Editor::new_in_memory().unwrap();
        let cx = Ctx { rep: &r, model: &ed.model, gen: 0, rev: ed.cache_key(), lang: Lang::En };
        let mut d = DiffState::default();
        d.ensure(&cx);
        d.set_sheet(Some("Doors"));
        d.ensure(&cx);
        assert_eq!(filtered_rows(&d, &r, Lang::En).len(), 2);
        d.set_sheet(None);
        d.set_errors_only(true);
        d.ensure(&cx);
        assert_eq!(filtered_rows(&d, &r, Lang::En).len(), 2);
        d.set_errors_only(false);
        d.set_search("AA7");
        d.ensure(&cx);
        assert_eq!(filtered_rows(&d, &r, Lang::En).len(), 1);
        d.set_search("no such text");
        d.ensure(&cx);
        assert!(filtered_rows(&d, &r, Lang::En).is_empty());
    }

    #[test]
    fn header_is_localized_without_touching_the_global_language() {
        let (en, ru) = (header(Lang::En), header(Lang::Ru));
        assert_ne!(en, ru);
        assert!(en.iter().chain(ru.iter()).all(|h| !h.is_empty() && !h.starts_with("xlw.")));
    }

    #[test]
    fn clipboard_text_is_tab_separated_with_header_first() {
        let h = row(["a", "b", "c", "d", "e", "f", "g"]);
        let t = to_tsv(&h, &[row(["1", "2", "3", "4", "5", "6", "7"])]);
        assert_eq!(t, "a\tb\tc\td\te\tf\tg\n1\t2\t3\t4\t5\t6\t7\n");
    }

    #[test]
    fn tabs_quotes_and_newlines_are_escaped() {
        let h = row(["h"; COLS]);
        let r = row(["a\tb", "x,y", "say \"hi\"", "l1\nl2", "l1\r\nl2", "plain", ""]);
        let tsv = to_tsv(&h, &[r.clone()]);
        assert!(tsv.contains("\"a\tb\""));
        assert!(tsv.contains("\"say \"\"hi\"\"\""));
        assert!(tsv.contains("\"l1\nl2\""));
        assert!(tsv.contains("x,y\t"));
        let csv = to_csv(&h, &[r.clone()], Lang::En);
        assert!(csv.contains("\"x,y\""));
        assert!(!csv.contains("\"a\tb\""));
        assert!(csv.contains("\"l1\r\nl2\""));
        assert!(csv.contains("\"say \"\"hi\"\"\""));
        assert!(csv.ends_with(",plain,\r\n"));
        let ru = to_csv(&h, &[r], Lang::Ru);
        assert!(ru.contains(";x,y;"), "comma is not the ru separator");
        assert!(ru.ends_with(";plain;\r\n"));
        assert!(ru.trim_start_matches('\u{FEFF}').starts_with("h;h;"));
    }

    #[test]
    fn russian_numbers_with_a_sign_are_not_marked_as_text() {
        assert_eq!(field("-1,5", ';').as_ref(), "-1,5");
        assert_eq!(field("-1\u{a0}234,5", ';').as_ref(), "-1\u{a0}234,5");
        assert_eq!(field("-1,5", ',').as_ref(), "\"-1,5\"");
        assert_eq!(field("--5", ',').as_ref(), "'--5");
        assert_eq!(field("-", ',').as_ref(), "'-");
        assert_eq!(field("-1.2.3", ',').as_ref(), "'-1.2.3");
        assert_eq!(field("+1e5", ',').as_ref(), "'+1e5");
    }

    #[test]
    fn formula_looking_values_are_not_run_by_the_spreadsheet() {
        assert_eq!(field("=1+1", ',').as_ref(), "'=1+1");
        assert_eq!(field("@x", ',').as_ref(), "'@x");
        assert_eq!(field("-x", ',').as_ref(), "'-x");
        assert_eq!(field("-5", ',').as_ref(), "-5");
        assert_eq!(field("+2.5", ',').as_ref(), "+2.5");
    }

    #[test]
    fn unicode_survives_and_csv_starts_with_a_bom() {
        let h = row(["h"; COLS]);
        let r = row(["\u{041B}\u{0438}\u{0441}\u{0442}", "A1", "", "\u{00E4}\u{4E2D}", "\u{1F600}", "", ""]);
        let csv = to_csv(&h, &[r.clone()], Lang::En);
        assert!(csv.starts_with('\u{FEFF}'));
        assert_eq!(&csv.as_bytes()[..3], &[0xEF, 0xBB, 0xBF]);
        assert!(csv.contains(&r[0]) && csv.contains(&r[3]) && csv.contains(&r[4]));
        assert!(!to_tsv(&h, &[r]).starts_with('\u{FEFF}'));
    }

    #[test]
    fn list_uses_the_shared_writer() {
        let src = include_str!("xl_issue_list.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(code.contains("csv::file(") && code.contains("csv::join("));
        assert!(!code.contains("fn field(") && !code.contains("fn is_number("), "own writer is back");
    }

    #[test]
    fn empty_list_is_the_header_only() {
        let h = header(Lang::En);
        assert_eq!(to_tsv(&h, &[]).lines().count(), 1);
        let csv = to_csv(&h, &[], Lang::En);
        assert_eq!(csv.trim_start_matches('\u{FEFF}').lines().count(), 1);
        let ed = Editor::new_in_memory().unwrap();
        let r = Report::default();
        let cx = Ctx { rep: &r, model: &ed.model, gen: 0, rev: ed.cache_key(), lang: Lang::En };
        let mut d = DiffState::default();
        d.ensure(&cx);
        assert!(filtered_rows(&d, &r, Lang::En).is_empty());
    }

    #[test]
    fn saves_beside_the_book_and_never_over_the_book() {
        let dir = temp_dir("save");
        let book = dir.join("plan.xlsx");
        std::fs::write(&book, b"workbook bytes").unwrap();
        let p = save_csv(&book, "\u{FEFF}a,b\r\n").unwrap();
        assert_eq!(p, dir.join("plan-issues.csv"));
        assert_ne!(p, book);
        assert_eq!(std::fs::read(&book).unwrap(), b"workbook bytes");
        assert_eq!(std::fs::read(&p).unwrap(), "\u{FEFF}a,b\r\n".as_bytes());
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_taken_name_is_never_replaced() {
        let dir = temp_dir("taken");
        let book = dir.join("plan.xlsx");
        std::fs::write(&book, b"w").unwrap();
        std::fs::write(dir.join("plan-issues.csv"), b"mine").unwrap();
        std::fs::write(dir.join("plan-issues-2.csv"), b"mine too").unwrap();
        let p = save_csv(&book, "new").unwrap();
        assert_eq!(p, dir.join("plan-issues-3.csv"));
        assert_eq!(std::fs::read(dir.join("plan-issues.csv")).unwrap(), b"mine");
        assert_eq!(std::fs::read(dir.join("plan-issues-2.csv")).unwrap(), b"mine too");
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_csv_book_is_not_overwritten_either() {
        let dir = temp_dir("csvbook");
        let book = dir.join("plan-issues.csv");
        std::fs::write(&book, b"book").unwrap();
        let p = save_csv(&book, "x").unwrap();
        assert_ne!(p, book);
        assert_eq!(std::fs::read(&book).unwrap(), b"book");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn candidate_never_equals_an_xlsx_path() {
        for n in 1..5 {
            assert_ne!(candidate(Path::new("d/plan.xlsx"), n), PathBuf::from("d/plan.xlsx"));
            assert_eq!(candidate(Path::new("d/plan.xlsx"), n).extension().unwrap(), "csv");
        }
    }

    #[test]
    fn missing_folder_is_an_error_and_leaves_nothing() {
        let dir = temp_dir("gone");
        let book = dir.join("no-such-sub").join("plan.xlsx");
        assert!(save_csv(&book, "x").is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
