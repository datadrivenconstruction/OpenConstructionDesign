// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rows an Apply created in the linked book: they have no id there until the re-export, so
//! the project remembers (book, sheet, row fingerprint) -> element and a second Apply of the
//! same book, even after a restart, does not create them again. The row number is only a
//! hint, so a row inserted above does not move the record onto the wrong row. Kept in `prop`
//! (kind `xlsx_rec::KIND`) and written through the journal inside the Apply step: undo removes
//! the records with the elements, redo writes them again. A step deleting the element takes
//! its record with it (`ddc_model::xlsx_rec`), so undo of the delete brings both back and an
//! element on the reused id has none. Reading never writes. Older records carry the journal
//! step of their Apply instead (`id@seq:hash`) and go stale when that step is undone.
use std::collections::HashMap;
use std::path::Path;

use ddc_io::xlsx::{level_fp, Made};
use ddc_model::xlsx_rec::{split_key, split_value, table_of, KIND, SHEETS};
use ddc_model::Model;

/// Element ids per sheet at one moment.
#[derive(Default)]
pub struct Snap(HashMap<&'static str, Vec<i64>>);

pub fn snapshot(model: &Model) -> Snap {
    let mut m = HashMap::new();
    for (sheet, table) in SHEETS {
        if let Ok(mut st) = model.sql().prepare(&format!("SELECT id FROM {table} ORDER BY id")) {
            if let Ok(rows) = st.query_map([], |r| r.get::<_, i64>(0)) {
                m.insert(sheet, rows.flatten().collect());
            }
        }
    }
    Snap(m)
}

fn book_key(book: &Path) -> String {
    book.to_string_lossy().replace('\\', "/").to_lowercase()
}

fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// Older records only: true while the journal still holds the step (not undone, nor replaced).
fn step_alive(model: &Model, stamp: &str) -> bool {
    let Some((seq, h)) = stamp.split_once(':') else {
        return false;
    };
    let Ok(seq) = seq.parse::<i64>() else {
        return false;
    };
    model
        .sql()
        .query_row("SELECT inv FROM op WHERE seq=?1", [seq], |r| r.get::<_, String>(0))
        .is_ok_and(|inv| format!("{:x}", hash(&inv)) == h)
}

/// Remembers the rows an Apply just created: `rows` are (sheet, book row, fingerprint) of
/// what was made, `before` is the model as it was. Levels pair by name, so a level the Apply
/// made on its own does not shift the others; the other sheets pair in row order, and a sheet
/// whose new ids do not pair up is remembered with id 0 (created, element unknown). Called
/// inside the Apply's model step: the records are journaled with it, so undo takes them away
/// and redo writes them again.
pub fn record(model: &mut Model, book: &Path, before: &Snap, rows: &[(String, u32, String)]) {
    sweep(model);
    let after = snapshot(model);
    let bk = book_key(book);
    let mut sheets: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    sheets.sort_unstable();
    sheets.dedup();
    for sheet in sheets {
        let mut mine: Vec<(u32, &str)> = rows.iter().filter(|r| r.0 == sheet).map(|r| (r.1, r.2.as_str())).collect();
        mine.sort_unstable();
        mine.dedup_by_key(|m| m.0);
        let old = before.0.get(sheet).map(Vec::as_slice).unwrap_or_default();
        let mut fresh: Vec<i64> = after
            .0
            .get(sheet)
            .map(|v| v.iter().copied().filter(|i| !old.contains(i)).collect())
            .unwrap_or_default();
        let ids: Vec<i64> = if sheet == "Levels" {
            let named: Vec<(i64, String)> = fresh
                .iter()
                .filter_map(|id| {
                    let n: String = model.sql().query_row("SELECT name FROM level WHERE id=?1", [id], |r| r.get(0)).ok()?;
                    Some((*id, level_fp(&n)))
                })
                .collect();
            let mut used = vec![false; named.len()];
            mine.iter()
                .map(|(_, fp)| {
                    let k = named.iter().enumerate().position(|(i, n)| !used[i] && n.1 == *fp);
                    k.map_or(0, |k| {
                        used[k] = true;
                        named[k].0
                    })
                })
                .collect()
        } else if fresh.len() == mine.len() {
            std::mem::take(&mut fresh)
        } else {
            vec![0; mine.len()]
        };
        for ((row, fp), id) in mine.iter().zip(ids) {
            let key = format!("{bk}|{sheet}|{fp}|{row}");
            // a stamp of the top entry would not survive the step merging its ops (cells + family
            // sheets): the journal itself ties the record to the Apply
            let _ = model.set_prop(KIND, 0, &key, Some((rusqlite::types::Value::Text(id.to_string()), "")));
        }
    }
}

/// Every record: (key, value).
fn all_records(model: &Model) -> Vec<(String, String)> {
    let Ok(mut st) = model.sql().prepare("SELECT key, CAST(value AS TEXT) FROM prop WHERE kind=?1 AND ref=0") else {
        return vec![];
    };
    let Ok(rows) = st.query_map([KIND], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))) else {
        return vec![];
    };
    rows.flatten().filter_map(|(k, v)| Some((k, v?))).collect()
}

/// (sheet, row hint, element id) of a record that still holds: its older stamp's step is in
/// the journal and its element exists (id 0: created, element unknown).
fn live(model: &Model, key: &str, val: &str) -> Option<(String, String, u32, i64)> {
    let (_, sheet, fp, row) = split_key(key)?;
    let (id, stamp) = split_value(val)?;
    let row = row.parse::<u32>().ok()?;
    let ok = stamp.is_none_or(|s| step_alive(model, s))
        && (id == 0
            || table_of(sheet).is_some_and(|t| {
                model.sql().query_row(&format!("SELECT 1 FROM {t} WHERE id=?1"), [id], |_| Ok(())).is_ok()
            }));
    ok.then(|| (sheet.to_string(), fp.to_string(), row, id))
}

/// (sheet, fingerprint) -> [(row hint, element id)] for the book; records whose Apply was
/// undone, or whose element is gone, are skipped. Writes nothing: a sync outside the journal
/// must not forget a record an undo would need back.
pub fn load(model: &Model, book: &Path) -> Made {
    let bk = book_key(book);
    let mut out = Made::new();
    for (key, val) in all_records(model) {
        if split_key(&key).is_none_or(|k| k.0 != bk) {
            continue;
        }
        if let Some((sheet, fp, row, id)) = live(model, &key, &val) {
            out.entry((sheet, fp)).or_default().push((row, id));
        }
    }
    out
}

/// Inside an Apply step: records that no longer hold (an element deleted by an older version,
/// or outside an operation) go, journaled, so an element on the reused id cannot claim them.
fn sweep(model: &mut Model) {
    for (key, val) in all_records(model) {
        if live(model, &key, &val).is_none() {
            let _ = model.set_prop(KIND, 0, &key, None);
        }
    }
}

/// Forgets only `rows` (sheet, book row, fingerprint) of the book: their ids were written into it.
pub fn forget(model: &Model, book: &Path, rows: &[(String, u32, String)]) {
    let bk = book_key(book);
    for (sheet, row, fp) in rows {
        let key = format!("{bk}|{sheet}|{fp}|{row}");
        let _ = model.sql().execute("DELETE FROM prop WHERE kind=?1 AND ref=0 AND key=?2", rusqlite::params![KIND, key]);
    }
}

/// Forgets the book's rows: a re-export gave them their ids.
pub fn clear(model: &Model, book: &Path) {
    let bk = book_key(book);
    let _ = model.sql().execute(
        "DELETE FROM prop WHERE kind=?1 AND ref=0 AND substr(key,1,?2)=?3",
        rusqlite::params![KIND, bk.chars().count() as i64 + 1, format!("{bk}|")],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::editor::Editor;

    fn wall(m: &mut Model, level: i64, x: f64) -> i64 {
        let wall_type = match m.sql().query_row("SELECT id FROM wall_type LIMIT 1", [], |r| r.get::<_, i64>(0)) {
            Ok(t) => t,
            Err(_) => m.add_wall_type("t", &[ddc_model::LayerSpec { material: "m".into(), thickness: 0.2 }]).unwrap(),
        };
        m.add_wall(&ddc_model::WallSpec { a: (x, 0.0), b: (x + 2.0, 0.0), level, base_offset: 0.0, height: 3.0, wall_type, offset0: None })
            .unwrap()
    }

    fn records(m: &Model) -> i64 {
        m.sql().query_row("SELECT count(*) FROM prop WHERE kind=?1", [KIND], |r| r.get(0)).unwrap()
    }

    #[test]
    fn unpaired_record_goes_with_undo() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (book, level) = (Path::new("C:/b/book.xlsx"), ed.level);
        let before = snapshot(&ed.model);
        // two walls appear but the book had one row: the pairing is unknown, the id 0
        let rows = [("Walls".to_string(), 7, "ab".to_string())];
        ed.model
            .step(|m| {
                wall(m, level, 0.0);
                wall(m, level, 5.0);
                record(m, book, &before, &rows);
                Ok(())
            })
            .unwrap();
        let m = load(&ed.model, book);
        assert_eq!(m.get(&("Walls".to_string(), "ab".to_string())), Some(&vec![(7, 0)]));
        ed.model.undo().unwrap();
        assert!(load(&ed.model, book).is_empty(), "the Apply was undone, the record goes");
        assert_eq!(records(&ed.model), 0, "undo leaves no record row behind");
        // another step now sits where the undone one was: still no record
        wall(&mut ed.model, level, 9.0);
        assert!(load(&ed.model, book).is_empty());
    }

    /// An Apply of cells and family sheet rows is several model ops merged into one journal
    /// entry: the records outlive the merge.
    #[test]
    fn record_survives_merged_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (book, level) = (Path::new("C:/b/book.xlsx"), ed.level);
        let before = snapshot(&ed.model);
        let rows = [("Walls".to_string(), 3, "a".to_string()), ("Walls".to_string(), 4, "b".to_string())];
        let (ids, wrote) = ed
            .model
            .step(|m| {
                let ids = [wall(m, level, 0.0), wall(m, level, 5.0)];
                record(m, book, &before, &rows);
                Ok(ids)
            })
            .unwrap();
        assert!(wrote);
        let m = load(&ed.model, book);
        assert_eq!(m.get(&("Walls".to_string(), "a".to_string())), Some(&vec![(3, ids[0])]));
        assert_eq!(m.get(&("Walls".to_string(), "b".to_string())), Some(&vec![(4, ids[1])]));
        ed.model.undo().unwrap();
        assert_eq!(records(&ed.model), 0);
    }

    #[test]
    fn legacy_stamped_record_still_read() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (book, level) = (Path::new("C:/b/book.xlsx"), ed.level);
        let id = wall(&mut ed.model, level, 0.0);
        let (seq, inv): (i64, String) =
            ed.model.sql().query_row("SELECT seq, inv FROM op ORDER BY seq DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        let key = format!("{}|Walls|fp|5", book_key(book));
        ed.model
            .sql()
            .execute(
                "INSERT INTO prop(kind,ref,key,value,unit) VALUES (?1,0,?2,?3,'')",
                rusqlite::params![KIND, key, format!("{id}@{seq}:{:x}", hash(&inv))],
            )
            .unwrap();
        assert_eq!(load(&ed.model, book).get(&("Walls".to_string(), "fp".to_string())), Some(&vec![(5, id)]));
        ed.model.undo().unwrap();
        assert!(load(&ed.model, book).is_empty(), "its step was undone");
    }

    #[test]
    fn implicit_level_keeps_pairing() {
        let mut ed = Editor::new_in_memory().unwrap();
        let book = Path::new("C:/b/book.xlsx");
        let before = snapshot(&ed.model);
        ed.model.add_level("Made on its own", 9.0).unwrap();
        let l2 = ed.model.add_level("L2", 3.0).unwrap();
        record(&mut ed.model, book, &before, &[("Levels".into(), 3, level_fp(" l2 "))]);
        let m = load(&ed.model, book);
        assert_eq!(m.get(&("Levels".to_string(), level_fp("L2"))), Some(&vec![(3, l2)]));
    }

    /// One Apply creating a wall from row 3 (fingerprint "fp") of the book; the new wall.
    fn apply_one(ed: &mut Editor, book: &Path, x: f64) -> i64 {
        let level = ed.level;
        let before = snapshot(&ed.model);
        let rows = [("Walls".to_string(), 3, "fp".to_string())];
        let (w, wrote) = ed
            .model
            .step(|m| {
                let w = wall(m, level, x);
                record(m, book, &before, &rows);
                Ok(w)
            })
            .unwrap();
        assert!(wrote);
        w
    }

    fn walls_made(m: &Model, book: &Path) -> Option<Vec<(u32, i64)>> {
        load(m, book).get(&("Walls".to_string(), "fp".to_string())).cloned()
    }

    /// (rowid, key, value) of every record.
    fn prop_rows(m: &Model) -> Vec<(i64, String, String)> {
        let mut st = m.sql().prepare("SELECT rowid, key, CAST(value AS TEXT) FROM prop WHERE kind=?1 ORDER BY rowid").unwrap();
        st.query_map([KIND], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(|x| x.unwrap()).collect()
    }

    fn put_raw(m: &Model, key: &str, val: &str) {
        m.sql()
            .execute("INSERT INTO prop(kind,ref,key,value,unit) VALUES (?1,0,?2,?3,'')", rusqlite::params![KIND, key, val])
            .unwrap();
    }

    /// Apply #1 creates E, a delete removes it, a sync runs, the delete is undone: E and its
    /// record are back together, so the row is not offered again (no duplicate).
    #[test]
    fn undo_of_delete_brings_the_record_back() {
        let mut ed = Editor::new_in_memory().unwrap();
        let book = Path::new("C:/b/book.xlsx");
        let e = apply_one(&mut ed, book, 0.0);
        let rec = prop_rows(&ed.model);
        assert_eq!(rec.len(), 1);
        ed.model.delete_walls(&[e]).unwrap();
        assert_eq!(records(&ed.model), 0, "the delete takes the record in its own step");
        assert_eq!(walls_made(&ed.model, book), None, "while the wall is gone its row is offered");
        ed.model.undo().unwrap();
        assert_eq!(prop_rows(&ed.model), rec, "undo of the delete restores the record, same rowid");
        assert_eq!(walls_made(&ed.model, book), Some(vec![(3, e)]), "the row is known again");
        ed.model.undo().unwrap();
        assert_eq!(records(&ed.model), 0, "undo of the Apply still takes it");
    }

    /// E deleted, then an unrelated wall gets E's id: the row is offered, not silently skipped.
    #[test]
    fn reused_id_does_not_claim_the_row() {
        let mut ed = Editor::new_in_memory().unwrap();
        let book = Path::new("C:/b/book.xlsx");
        let e = apply_one(&mut ed, book, 0.0);
        ed.model.delete_walls(&[e]).unwrap();
        let level = ed.level;
        let other = wall(&mut ed.model, level, 9.0);
        assert_eq!(other, e, "fixture: the new wall reuses the id");
        assert_eq!(walls_made(&ed.model, book), None, "the row is not taken for the unrelated wall");
        assert_eq!(records(&ed.model), 0);
    }

    /// A delete and a new element in one step: the reused id starts without the record.
    #[test]
    fn reuse_inside_one_step_drops_the_record() {
        let mut ed = Editor::new_in_memory().unwrap();
        let book = Path::new("C:/b/book.xlsx");
        let e = apply_one(&mut ed, book, 0.0);
        let level = ed.level;
        let (other, _) = ed
            .model
            .step(|m| {
                m.delete_walls(&[e])?;
                Ok(wall(m, level, 9.0))
            })
            .unwrap();
        assert_eq!(other, e, "fixture: the new wall reuses the id");
        assert_eq!(walls_made(&ed.model, book), None);
        ed.model.undo().unwrap();
        assert_eq!(walls_made(&ed.model, book), Some(vec![(3, e)]));
    }

    /// A sync only reads: stale and orphan records stay until an Apply step sweeps them.
    #[test]
    fn load_writes_nothing() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (book, level) = (Path::new("C:/b/book.xlsx"), ed.level);
        let bk = book_key(book);
        let w = wall(&mut ed.model, level, 0.0);
        put_raw(&ed.model, &format!("{bk}|Walls|gone|1"), "999");
        put_raw(&ed.model, &format!("{bk}|Walls|dead|2"), &format!("{w}@77:abc"));
        put_raw(&ed.model, &format!("{bk}|Walls|ok|4"), &w.to_string());
        let rows = prop_rows(&ed.model);
        let m = load(&ed.model, book);
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(m.get(&("Walls".to_string(), "ok".to_string())), Some(&vec![(4, w)]));
        let _ = load(&ed.model, book);
        assert_eq!(prop_rows(&ed.model), rows, "reading changed no record");
    }

    /// Records written by older versions: a key without fingerprint and a plain id are read; an
    /// orphan is swept inside the next Apply step, journaled, and comes back with its undo.
    #[test]
    fn old_records_read_and_swept_in_the_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (book, level) = (Path::new("C:/b/book.xlsx"), ed.level);
        let bk = book_key(book);
        let w = wall(&mut ed.model, level, 0.0);
        put_raw(&ed.model, &format!("{bk}|Walls|6"), &w.to_string());
        put_raw(&ed.model, &format!("{bk}|Walls|5"), "999");
        assert_eq!(load(&ed.model, book).get(&("Walls".to_string(), String::new())), Some(&vec![(6, w)]));
        let rows = prop_rows(&ed.model);
        let e = apply_one(&mut ed, book, 5.0);
        let now = prop_rows(&ed.model);
        assert!(!now.iter().any(|r| r.2 == "999"), "the orphan is swept: {now:?}");
        assert!(now.iter().any(|r| r.1 == format!("{bk}|Walls|6")), "the live old record stays");
        assert_eq!(walls_made(&ed.model, book), Some(vec![(3, e)]));
        ed.model.undo().unwrap();
        assert_eq!(prop_rows(&ed.model), rows, "undo of the Apply restores the swept orphan as it was");
    }

    #[test]
    fn book_key_ignores_case_and_separators() {
        assert_eq!(book_key(Path::new("C:\\Dir\\Book.XLSX")), "c:/dir/book.xlsx");
    }
}

#[cfg(test)]
#[path = "xlsx_links_fuzz.rs"]
mod xlsx_links_fuzz;
