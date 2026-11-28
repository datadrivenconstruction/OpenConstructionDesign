// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A main sheet the user renamed is recognised by its header row. Nothing is read from it until
//! the user says so for this import (`KeyDecisions::alias`); the import never writes the workbook
//! (an Apply only fills the ids of new rows in place, see `id_write`) and two candidates are
//! never resolved by guessing.
use std::collections::HashMap;

use calamine::{Data, Reader, Xlsx};

use super::book::{self, KeyRows};
use super::{units, Col};

type Book = Xlsx<std::io::BufReader<std::fs::File>>;

/// A sheet is proposed when this share of the expected columns is in its header row.
pub(crate) const MIN_PERCENT: usize = 80;
/// Fewer matching columns never make a proposal, however short the expected list is.
const MIN_COLUMNS: usize = 3;

/// A sheet that holds the columns of a known one under another name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renamed {
    pub sheet: String,
    /// Whole percent of the expected columns found in its header row.
    pub score: u32,
}

/// Sheets that might be the known sheet `expected`; the user picks one (or none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetCandidate {
    pub expected: String,
    /// Best match first; more than one means the choice is ambiguous.
    pub found: Vec<Renamed>,
}

/// The sheet of the book to read for `expected`: the user's pick when the book has it.
pub(crate) fn target(have: &[String], alias: &HashMap<String, String>, expected: &str) -> String {
    match alias.get(expected) {
        Some(a) if have.iter().any(|h| h == a) => a.clone(),
        _ => expected.to_string(),
    }
}

/// True when the book has the sheet `expected` was told to be read from (it is not "missing").
pub(crate) fn is_aliased(have: &[String], alias: &HashMap<String, String>, expected: &str) -> bool {
    alias.get(expected).is_some_and(|a| have.iter().any(|h| h == a))
}

/// Sheets the importer reads or keeps for itself; never offered as a rename.
fn reserved(n: &str) -> bool {
    use super::*;
    [
        INFO, BASELINE, KEYS, LISTS, ROOMS, SLABS, CURTAINS, SUMMARY, COLUMNS, BEAMS, ROOFS,
        SOLIDS, CEILINGS, SLAB_TYPES, MEMBER_TYPES, LEVELS, TYPES, LAYERS, SLAB_LAYERS, MATERIALS, WALLS,
        OPENINGS, QUANTITIES,
    ]
    .contains(&n)
        || PARAM_SHEETS.iter().any(|p| p.sheet == n)
}

/// Whole percent of `cols` found in the header row `row` of `sheet`, 0 when it has no `id`.
fn score(keys: &KeyRows, expected: &str, cols: &[Col], c0: usize, row: &[String]) -> usize {
    let normed: Vec<String> = book::resolve(keys, expected, cols, c0, row)
        .texts
        .iter()
        .map(|t| units::norm(t))
        .collect();
    if !normed.contains(&units::norm("id")) {
        return 0;
    }
    let hit = cols.iter().filter(|c| normed.contains(&units::norm(c.header))).count();
    if hit < MIN_COLUMNS {
        return 0;
    }
    hit * 100 / cols.len().max(1)
}

/// Unread sheets of the book whose header row holds at least `MIN_PERCENT` of `cols`, best
/// first. Sheets already picked for another sheet are left out.
pub(crate) fn find(
    wb: &mut Book,
    have: &[String],
    expected: &str,
    cols: &[Col],
    keys: &KeyRows,
    alias: &HashMap<String, String>,
) -> Vec<Renamed> {
    let known: Vec<String> = cols.iter().map(|c| units::norm(c.header)).collect();
    let mut out = Vec::new();
    for n in have {
        if n == expected || reserved(n) || alias.values().any(|a| a == n) {
            continue;
        }
        let Ok(range) = wb.worksheet_range(n) else {
            continue;
        };
        let c0 = range.start().map_or(0, |s| s.1 as usize);
        let at = book::find_header(&range, &known, |row| {
            book::resolve(keys, expected, cols, c0, row).texts.iter().map(|t| units::norm(t)).collect()
        });
        let Some(row) = range.rows().nth(at) else {
            continue;
        };
        let head: Vec<String> = row.iter().map(|d: &Data| d.to_string().trim().to_string()).collect();
        let pct = score(keys, expected, cols, c0, &head);
        if pct >= MIN_PERCENT {
            out.push(Renamed { sheet: n.clone(), score: pct as u32 });
        }
    }
    out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.sheet.cmp(&b.sheet)));
    out
}
