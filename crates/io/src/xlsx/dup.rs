//! Rows of one sheet that repeat an element id: a row copied in the spreadsheet. Copying a row
//! to make a similar element is the usual habit, so the import never lets the first row win:
//! rows still holding the exported values are ignored, exactly one edited row is the edit
//! wherever it sits, and two differently edited rows apply nothing for that id until the user
//! decides (`KeyDecisions::reject`: the copies are read as rows without an id, new elements).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};

use calamine::Data;

use super::{blank, same, Row, Tab};
use crate::xlsx::baseline::{extra_key, BVal};
use crate::xlsx::{msg, KeyDecisions, Rejection, Report};
use crate::xlsx::{CURTAINS, LEVELS, MATERIALS, OPENINGS, ROOMS, SLABS, SLAB_TYPES, TYPES, WALLS};

/// Outcome for one id: the row to read (index into `Tab::rows`), or the differing rows.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Pick {
    One(usize),
    Conflict(Vec<usize>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Act {
    /// Read as usual.
    Use,
    /// Same cells as the used row (its sheet row).
    Same(u32),
    /// Exported values while another row of the id is the edit or a conflict.
    Unchanged,
    /// Differs from another edited row of the id: refused.
    Refuse,
    /// The user's decision: read as a row without an id.
    New,
}

#[derive(Debug, Clone, Copy)]
struct Plan {
    id: i64,
    act: Act,
    /// No row of the id is read as it: the id is marked seen so `delete_missing` keeps it.
    mark: bool,
}

/// Rows of a sheet whose id repeats, keyed by sheet row.
pub(super) struct Dups {
    at: HashMap<u32, Plan>,
    /// id -> its differing rows, "5, 50".
    rows_of: HashMap<i64, String>,
    can_create: bool,
}

/// Sheets whose loop reads rows through `Dups`: Some(can the sheet create elements).
fn mode(tab: &Tab) -> Option<bool> {
    match tab.name {
        LEVELS | WALLS | OPENINGS | ROOMS | SLABS | CURTAINS => Some(true),
        TYPES | MATERIALS => Some(false),
        SLAB_TYPES if tab.idx.contains_key("name") => Some(false),
        _ => None,
    }
}

fn id_of(tab: &Tab, i: usize) -> Option<i64> {
    let (r, cells) = &tab.rows[i];
    let mut row = Row::new(tab, *r, cells);
    if row.empty() {
        return None;
    }
    row.parse_id(&mut Report::default(), "id").ok().flatten()
}

/// Cell of row `i` at column `j`, None when blank or under a merge.
fn cell(tab: &Tab, i: usize, j: usize) -> Option<&Data> {
    let (r, cells) = &tab.rows[i];
    if tab.covered.contains(&(*r, j)) {
        return None;
    }
    cells.get(j).filter(|d| !blank(d))
}

fn given(tab: &Tab, i: usize, j: usize) -> bool {
    !tab.covered.contains(&(tab.rows[i].0, j))
}

fn same_cell(a: Option<&Data>, b: Option<&Data>) -> bool {
    let num = |d: &Data| match d {
        Data::Float(f) => Some(*f),
        Data::Int(i) => Some(*i as f64),
        _ => None,
    };
    match (a, b) {
        (None, None) => true,
        (Some(Data::String(x)), Some(Data::String(y))) => x.trim() == y.trim(),
        (Some(x), Some(y)) => match (num(x), num(y)) {
            (Some(p), Some(q)) => same(p, q),
            _ => x == y,
        },
        _ => false,
    }
}

/// Format columns and the sheet's own parameter columns: index and baseline key.
fn columns(tab: &Tab) -> Vec<(usize, String)> {
    let rev: HashMap<usize, &str> = tab.idx.iter().map(|(k, i)| (*i, *k)).collect();
    (0..tab.head.len())
        .filter(|j| rev.get(j) != Some(&"id"))
        .filter_map(|j| match rev.get(&j) {
            Some(k) => Some((j, k.to_string())),
            None if !tab.head[j].trim().is_empty() => Some((j, extra_key(&tab.head[j]))),
            None => None,
        })
        .collect()
}

fn text(d: &Data) -> String {
    match d {
        Data::String(s) => s.trim().to_string(),
        other => other.to_string().trim().to_string(),
    }
}

/// A cell against its exported value: Some(true) unchanged, Some(false) edited, None when
/// the baseline cannot tell (formula, date or error at the export).
fn vs_base(tab: &Tab, i: usize, j: usize, key: &str, b: &BVal) -> Option<bool> {
    let d = cell(tab, i, j);
    Some(match (b, d) {
        (BVal::Blank, None) => true,
        (BVal::S(t), None) => t.trim().is_empty(),
        (BVal::N(_) | BVal::B(_), None) | (BVal::Blank, Some(_)) => false,
        (BVal::N(x), Some(d)) => {
            let v = if tab.idx.contains_key(key) {
                let (r, cells) = &tab.rows[i];
                Row::new(tab, *r, cells).parse_num(&mut Report::default(), key).ok().flatten()
            } else {
                match d {
                    Data::Float(f) => Some(*f),
                    Data::Int(n) => Some(*n as f64),
                    _ => None,
                }
            };
            v.is_some_and(|v| same(v, *x))
        }
        (BVal::S(t), Some(d)) => text(d) == t.trim(),
        (BVal::B(v), Some(Data::Bool(b))) => v == b,
        (BVal::B(_), Some(_)) => false,
        _ => return None,
    })
}

/// Rows `i` and `k` hold the same cells (the id aside).
fn identical(tab: &Tab, cols: &[(usize, String)], i: usize, k: usize) -> bool {
    cols.iter().all(|(j, _)| {
        !(given(tab, i, *j) && given(tab, k, *j)) || same_cell(cell(tab, i, *j), cell(tab, k, *j))
    })
}

/// The row differs from the export. A column the baseline cannot judge (or a book without a
/// baseline) counts as edited when the rows of the id disagree on it: never a silent pick.
fn edited(tab: &Tab, cols: &[(usize, String)], id: i64, i: usize, group: &[usize]) -> bool {
    let base = tab.base.as_deref().filter(|b| b.has_id(tab.name, id));
    cols.iter().any(|(j, key)| {
        if !given(tab, i, *j) {
            return false;
        }
        let judged = base.and_then(|b| b.get(tab.name, id, key)).and_then(|b| vs_base(tab, i, *j, key, b));
        match judged {
            Some(unchanged) => !unchanged,
            None => group
                .iter()
                .any(|&k| k != i && given(tab, k, *j) && !same_cell(cell(tab, i, *j), cell(tab, k, *j))),
        }
    })
}

/// Which of the rows `group` (indices into `tab.rows`, all with id `id`) is read.
pub(super) fn dup_pick(tab: &Tab, id: i64, group: &[usize]) -> Pick {
    let cols = columns(tab);
    let ed: Vec<usize> = group.iter().copied().filter(|&i| edited(tab, &cols, id, i, group)).collect();
    match ed.as_slice() {
        [] => Pick::One(group[0]),
        [one] => Pick::One(*one),
        [first, rest @ ..] if rest.iter().all(|&k| identical(tab, &cols, *first, k)) => Pick::One(*first),
        _ => Pick::Conflict(ed),
    }
}

impl Dups {
    /// Plan for the rows of `tab` whose id repeats; empty for other sheets.
    pub(super) fn plan(tab: &Tab, dec: &KeyDecisions) -> Dups {
        let can_create = mode(tab) == Some(true);
        let mut d = Dups { at: HashMap::new(), rows_of: HashMap::new(), can_create };
        if mode(tab).is_none() {
            return d;
        }
        let mut by_id: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
        for i in 0..tab.rows.len() {
            if let Some(id) = id_of(tab, i) {
                by_id.entry(id).or_default().push(i);
            }
        }
        let cols = columns(tab);
        for (id, all) in by_id.into_iter().filter(|(_, v)| v.len() > 1) {
            let r_of = |i: usize| tab.rows[i].0;
            let (new, rest): (Vec<usize>, Vec<usize>) = all
                .iter()
                .partition(|&&i| can_create && dec.reject.contains(&(tab.name.to_string(), r_of(i))));
            let pick = (!rest.is_empty()).then(|| dup_pick(tab, id, &rest));
            let used = match &pick {
                Some(Pick::One(u)) => Some(*u),
                _ => None,
            };
            let mark = used.is_none();
            let mut put = |i: usize, act: Act| {
                d.at.insert(r_of(i), Plan { id, act, mark });
            };
            for &i in &new {
                put(i, Act::New);
            }
            match &pick {
                Some(Pick::One(u)) => {
                    for &i in &rest {
                        let act = if i == *u {
                            Act::Use
                        } else if identical(tab, &cols, i, *u) {
                            Act::Same(r_of(*u))
                        } else {
                            Act::Unchanged
                        };
                        put(i, act);
                    }
                }
                Some(Pick::Conflict(v)) => {
                    for &i in &rest {
                        put(i, if v.contains(&i) { Act::Refuse } else { Act::Unchanged });
                    }
                    let rows: Vec<String> = v.iter().map(|&i| r_of(i).to_string()).collect();
                    d.rows_of.insert(id, rows.join(", "));
                }
                None => {}
            }
        }
        d
    }

    /// The sheet row is a copy the user turned into a new element.
    pub(super) fn is_new(&self, r: u32) -> bool {
        self.at.get(&r).is_some_and(|p| p.act == Act::New)
    }

    /// Reads the row's place in its id group: `false` skips the row (noted in the report).
    /// A row the user turned into a new element comes back with no id.
    pub(super) fn route(&self, row: &mut Row<'_>, rep: &mut Report, seen: &mut HashSet<i64>) -> bool {
        let Some(p) = self.at.get(&row.r).copied() else {
            return true;
        };
        if row.id != Some(p.id) {
            return true;
        }
        if p.mark {
            seen.insert(p.id);
        }
        let id = p.id.to_string();
        match p.act {
            Act::Use => true,
            Act::New => {
                row.id = None;
                true
            }
            Act::Same(u) => {
                let why = msg::fill_template(msg::DUP_SAME.1, &[&id, &u.to_string()]);
                row.skip(rep, "id", why);
                false
            }
            Act::Unchanged => {
                row.skip(rep, "id", msg::fill_template(msg::DUP_UNCHANGED.1, &[&id]));
                false
            }
            Act::Refuse => {
                let tpl = if self.can_create { msg::DUP_CONFLICT.1 } else { msg::DUP_CONFLICT_KEEP.1 };
                let rows = self.rows_of.get(&p.id).map_or("", String::as_str);
                rep.rejected.push(Rejection {
                    sheet: row.tab.name.into(),
                    row: row.r,
                    col: row.col_of("id"),
                    column: row.header("id"),
                    reason: msg::fill_template(tpl, &[&id, rows]),
                });
                false
            }
        }
    }
}

/// Rows the parameter reader sees: the same rows the sheet's loop reads, a new-element copy
/// without its id, so parameters never reach an element the sheet left alone.
pub(super) fn view_rows<'a>(tab: &'a Tab, dec: &KeyDecisions) -> Cow<'a, [(u32, Vec<Data>)]> {
    let plan = Dups::plan(tab, dec);
    if plan.at.is_empty() {
        return Cow::Borrowed(tab.rows.as_slice());
    }
    let idc = tab.idx.get("id").copied();
    let rows = tab
        .rows
        .iter()
        .filter_map(|(r, cells)| match plan.at.get(r).map(|p| p.act) {
            None | Some(Act::Use) => Some((*r, cells.clone())),
            Some(Act::New) => {
                let mut c = cells.clone();
                if let Some(cell) = idc.and_then(|j| c.get(j).is_some().then_some(j)) {
                    c[cell] = Data::Empty;
                }
                Some((*r, c))
            }
            Some(_) => None,
        })
        .collect();
    Cow::Owned(rows)
}
