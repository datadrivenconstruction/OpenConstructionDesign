//! Records of the book rows an XLSX Apply created (`prop`, kind [`KIND`], ref 0; the app's
//! `xlsx_links` writes them inside the Apply step). Key `book|sheet|fingerprint|row` (older:
//! `book|sheet|row`), value the element id (older: `id@seq:hash`). An operation that deletes
//! an element takes its records with it in the same journal entry: undo of the delete brings
//! both back, and a later element on the reused id starts without a record, so its book row
//! is offered again instead of being taken for present.
use rusqlite::Connection;

use crate::ops::Inv;
use crate::Error;

pub const KIND: &str = "excel_created"; // stored name, kept for saved files

/// Sheet of the book -> table of its elements.
pub const SHEETS: [(&str, &str); 6] = [
    ("Levels", "level"),
    ("Walls", "wall"),
    ("Openings", "opening"),
    ("Rooms", "room"),
    ("Slabs", "slab"),
    ("Curtain walls", "curtain_wall"),
];

pub fn table_of(sheet: &str) -> Option<&'static str> {
    SHEETS.iter().find(|s| s.0 == sheet).map(|s| s.1)
}

/// (book, sheet, fingerprint, row) of a record key; the fingerprint is "" in older keys.
pub fn split_key(key: &str) -> Option<(&str, &str, &str, &str)> {
    let mut p = key.rsplitn(4, '|');
    let (row, a, b) = (p.next()?, p.next()?, p.next()?);
    Some(match p.next() {
        Some(book) => (book, b, a, row),
        None => (b, a, "", row),
    })
}

/// Element id of a record value and the journal stamp older records carry.
pub fn split_value(val: &str) -> Option<(i64, Option<&str>)> {
    let (id, stamp) = match val.split_once('@') {
        Some((i, s)) => (i, Some(s)),
        None => (val, None),
    };
    Some((id.parse().ok()?, stamp))
}

/// Before an operation: records at or below this rowid existed before it.
pub(crate) fn mark(db: &Connection) -> Result<i64, Error> {
    Ok(db
        .prepare_cached("SELECT COALESCE(MAX(rowid),0) FROM prop")?
        .query_row([], |r| r.get(0))?)
}

/// After an operation: records (older than `mark`) of the elements it deleted go, journaled.
/// A row deleted and inserted again in one operation is a new element (as phases see it).
pub(crate) fn prune(db: &Connection, mark: i64, mut inv: Inv) -> Result<Inv, Error> {
    let gone: Vec<(String, i64)> = {
        let mut st = db.prepare_cached(
            "SELECT DISTINCT tbl, id FROM ddc_dirty WHERE ev='d' AND tbl IN \
             ('level','wall','opening','room','slab','curtain_wall')",
        )?;
        let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        v
    };
    if gone.is_empty() {
        return Ok(inv);
    }
    let recs: Vec<(i64, String, Option<String>)> = {
        let mut st = db.prepare_cached(
            "SELECT rowid, key, CAST(value AS TEXT) FROM prop WHERE kind=?1 AND ref=0 AND rowid<=?2",
        )?;
        let v = st
            .query_map(rusqlite::params![KIND, mark], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        v
    };
    for (rowid, key, val) in recs {
        let Some(t) = split_key(&key).and_then(|k| table_of(k.1)) else {
            continue;
        };
        let Some((id, _)) = val.as_deref().and_then(split_value) else {
            continue;
        };
        if id != 0 && gone.iter().any(|g| g.0 == t && g.1 == id) {
            inv = inv.restore(db, "prop", "rowid", rowid)?;
            db.prepare_cached("DELETE FROM prop WHERE rowid=?1")?.execute([rowid])?;
        }
    }
    Ok(inv)
}
