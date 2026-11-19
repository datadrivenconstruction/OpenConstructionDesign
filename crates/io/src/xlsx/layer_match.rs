//! Layer rows have no id of their own: the layer sheets key them by (type, position), so after a
//! layer is inserted, removed or moved the old book's keys name other layers. This matches the
//! old book's layer rows (its Baseline) to the current ones per type by content, so the user's
//! cells and hidden rows stay with their own layer; a row with no confident match is named in
//! the loss list, never moved to a neighbour.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use calamine::{open_workbook, Xlsx};

use super::baseline::{self, BVal};
use super::export::V;
use super::extra::UserCounts;
use super::msg::fill_template;
use super::{Col, LAYERS, SLAB_LAYERS};

/// User cells of a layer row that matches no current layer: (locale key, English template).
pub(crate) const CELLS_UNPLACED: (&str, &str) = (
    "xl.msg.layer_cells_unplaced",
    "{} cells of type '{}' layer {} on sheet '{}' could not be placed",
);
/// A hidden layer row that matches no current layer.
pub(crate) const HIDDEN_UNPLACED: (&str, &str) = (
    "xl.msg.layer_hidden_unplaced",
    "hidden row of type '{}' layer {} on sheet '{}' could not be placed",
);

/// Type and position of a layer key (`layer_key`).
fn split(key: i64) -> (i64, i64) {
    (key.div_euclid(1_000_000), key.rem_euclid(1_000_000))
}

/// A layer row as an export recorded it in the Baseline.
#[derive(Clone)]
pub(crate) struct Row {
    key: i64,
    material: BVal,
    thickness: BVal,
    type_name: String,
}

impl Row {
    fn same(&self, o: &Row) -> bool {
        known(&self.material, &o.material) && known(&self.thickness, &o.thickness)
    }

    fn same_material(&self, o: &Row) -> bool {
        known(&self.material, &o.material)
    }
}

/// Equal and known: a formula or a missing value never matches.
fn known(a: &BVal, b: &BVal) -> bool {
    !matches!(a, BVal::Any) && !matches!(b, BVal::Any) && a.same(b)
}

/// An empty text reads back from the book as a blank cell.
fn norm(b: BVal) -> BVal {
    match b {
        BVal::S(s) if s.trim().is_empty() => BVal::Blank,
        b => b,
    }
}

/// Rows of a layer sheet as `layer_sheet` records them: (key, values in `cols` order).
fn rows_of(cols: &[Col], rows: &[(i64, Vec<V>)]) -> Vec<Row> {
    let at = |k: &str| cols.iter().position(|c| c.key == k);
    let (m, t, n) = (at("material"), at("thickness"), at("type_name"));
    let get = |r: &[V], j: Option<usize>| norm(j.and_then(|j| r.get(j)).map_or(BVal::Any, baseline::v_bval));
    rows.iter()
        .map(|(key, r)| Row {
            key: *key,
            material: get(r.as_slice(), m),
            thickness: get(r.as_slice(), t),
            type_name: get(r.as_slice(), n).text(),
        })
        .collect()
}

/// Layer rows of the Baseline of `path`, by sheet; empty for a book without one.
fn book_rows(path: &Path) -> HashMap<String, Vec<Row>> {
    let mut out = HashMap::new();
    let wb: Result<Xlsx<std::io::BufReader<std::fs::File>>, _> = open_workbook(path);
    let Ok(mut wb) = wb else {
        return out;
    };
    let Some(base) = baseline::read(&mut wb) else {
        return out;
    };
    for sheet in [LAYERS, SLAB_LAYERS] {
        if !base.has_sheet(sheet) {
            continue;
        }
        let get = |key: i64, k: &str| norm(base.get(sheet, key, k).cloned().unwrap_or(BVal::Any));
        let rows = base
            .ids(sheet)
            .into_iter()
            .map(|key| Row {
                key,
                material: get(key, "material"),
                thickness: get(key, "thickness"),
                type_name: get(key, "type_name").text(),
            })
            .collect();
        out.insert(sheet.to_string(), rows);
    }
    out
}

/// Where the old book's layer rows went.
#[derive(Default)]
pub(crate) struct Plan {
    to_new: HashMap<i64, i64>,
    to_old: HashMap<i64, i64>,
    /// Keys of the old Baseline.
    known: HashSet<i64>,
    new_keys: HashSet<i64>,
    /// Old rows of a type that still has layers, with no confident match: key -> type name.
    lost: BTreeMap<i64, String>,
}

impl Plan {
    /// New key of the old row `old`; None when it has no layer to go to (unplaced, or its type
    /// is gone).
    pub(crate) fn new_of(&self, old: i64) -> Option<i64> {
        if let Some(n) = self.to_new.get(&old) {
            return Some(*n);
        }
        // a row the Baseline does not know keeps its key while no matched row takes it
        (!self.known.contains(&old) && self.new_keys.contains(&old) && !self.to_old.contains_key(&old))
            .then_some(old)
    }

    /// Old key whose cells and marks the new row `new` takes; None for a new layer.
    pub(crate) fn old_of(&self, new: i64) -> Option<i64> {
        if let Some(o) = self.to_old.get(&new) {
            return Some(*o);
        }
        (!self.known.contains(&new)).then_some(new)
    }

    /// Type name of the old row `old` when it is unplaced.
    pub(crate) fn lost(&self, old: i64) -> Option<&str> {
        self.lost.get(&old).map(String::as_str)
    }
}

fn by_type(rows: &[Row]) -> BTreeMap<i64, Vec<&Row>> {
    let mut g: BTreeMap<i64, Vec<&Row>> = BTreeMap::new();
    for r in rows {
        g.entry(split(r.key).0).or_default().push(r);
    }
    for v in g.values_mut() {
        v.sort_by_key(|r| split(r.key).1);
    }
    g
}

/// Matches the `old` layer rows to the `new` ones, type by type.
pub(crate) fn plan(old: &[Row], new: &[Row]) -> Plan {
    let mut p = Plan {
        known: old.iter().map(|r| r.key).collect(),
        new_keys: new.iter().map(|r| r.key).collect(),
        ..Plan::default()
    };
    let (a, b) = (by_type(old), by_type(new));
    for (t, olds) in &a {
        // a type that is gone takes its rows along, as a deleted element does
        let Some(news) = b.get(t).filter(|n| !n.is_empty()) else {
            continue;
        };
        for (i, j) in pair(olds, news).into_iter().enumerate() {
            let ok = olds[i].key;
            match j {
                Some(j) => {
                    p.to_new.insert(ok, news[j].key);
                    p.to_old.insert(news[j].key, ok);
                }
                None => {
                    p.lost.insert(ok, olds[i].type_name.clone());
                }
            }
        }
    }
    p
}

/// For each old row of one type (by position) the index of its new row: equal contents in the
/// longest common order first, then unique equal contents (a moved layer), then a unique
/// material (a moved or resized layer), then the same position when every other matched row kept
/// its place and the layer count is the same (layers edited in place). The rest stays unmatched.
fn pair(a: &[&Row], b: &[&Row]) -> Vec<Option<usize>> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i].same(b[j]) {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut out: Vec<Option<usize>> = vec![None; n];
    let mut used = vec![false; m];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i].same(b[j]) {
            out[i] = Some(j);
            used[j] = true;
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let passes: [fn(&Row, &Row) -> bool; 2] = [Row::same, Row::same_material];
    for eq in passes {
        for i in 0..n {
            if out[i].is_some() {
                continue;
            }
            let cand: Vec<usize> = (0..m).filter(|&j| !used[j] && eq(a[i], b[j])).collect();
            let &[j] = cand.as_slice() else {
                continue;
            };
            let rivals = (0..n).filter(|&k| out[k].is_none() && eq(a[k], b[j])).count();
            if rivals == 1 {
                out[i] = Some(j);
                used[j] = true;
            }
        }
    }
    let in_place = out.iter().enumerate().all(|(i, j)| j.is_none_or(|j| j == i));
    if n == m && in_place {
        for i in 0..n {
            if out[i].is_none() && !used[i] {
                out[i] = Some(i);
                used[i] = true;
            }
        }
    }
    out
}

thread_local! {
    /// Layer rows of the old book of the export running on this thread, by sheet.
    static OLD: RefCell<HashMap<String, Vec<Row>>> = RefCell::new(HashMap::new());
    /// Plans of the layer sheets this export has written so far.
    static PLANS: RefCell<HashMap<String, Plan>> = RefCell::new(HashMap::new());
}

/// Starts an export from the old book `old`; `need` false (no user cells or row marks on the
/// layer sheets) skips reading its Baseline and keys stay as they are.
pub(crate) fn begin(old: &Path, need: bool) {
    let rows = if need && old.exists() { book_rows(old) } else { HashMap::new() };
    OLD.with(|o| *o.borrow_mut() = rows);
    PLANS.with(|p| p.borrow_mut().clear());
}

/// Matches the old book's rows of `sheet` to `rows`, the rows this export writes and records.
pub(crate) fn settle(sheet: &str, cols: &[Col], rows: &[(i64, Vec<V>)]) {
    let Some(old) = OLD.with(|o| o.borrow().get(sheet).cloned()) else {
        return;
    };
    let p = plan(&old, &rows_of(cols, rows));
    PLANS.with(|x| x.borrow_mut().insert(sheet.to_string(), p));
}

/// Key of the old book's row whose user cells and marks the written row `key` of `sheet` takes:
/// the key itself unless the rows of a layer sheet moved; None for a new layer.
pub(crate) fn old_key(sheet: &str, key: i64) -> Option<i64> {
    PLANS.with(|p| p.borrow().get(sheet).map_or(Some(key), |p| p.old_of(key)))
}

/// Loss check of a re-export from `old` into `new`: puts the old book's layer-row counts into
/// the new book's keys and names the user cells and hidden rows that found no layer.
pub(crate) fn rekey_losses(old: &Path, new: &Path, counts: &mut UserCounts) -> Vec<String> {
    let mut out = vec![];
    let hidden = |s: &str| super::sheet_view::hidden_rows_of(old, s);
    let need = [LAYERS, SLAB_LAYERS]
        .iter()
        .any(|s| counts.cols.iter().any(|c| c.0 == *s) || !hidden(*s).is_empty());
    if !need {
        return out;
    }
    let (a, b) = (book_rows(old), book_rows(new));
    for sheet in [LAYERS, SLAB_LAYERS] {
        let (Some(oa), Some(nb)) = (a.get(sheet), b.get(sheet)) else {
            continue;
        };
        let p = plan(oa, nb);
        let mut cells: BTreeMap<i64, usize> = BTreeMap::new();
        for c in counts.cols.iter_mut().filter(|c| c.0 == sheet) {
            for id in &c.3 {
                if p.lost(*id).is_some() {
                    *cells.entry(*id).or_default() += 1;
                }
            }
            c.3 = c.3.iter().filter_map(|id| p.new_of(*id)).collect();
        }
        for c in counts.ids.iter_mut().filter(|c| c.0 == sheet) {
            c.1 = c.1.iter().filter_map(|id| p.new_of(*id)).collect();
        }
        for (key, n) in cells {
            let name = p.lost(key).unwrap_or_default();
            let pos = split(key).1.to_string();
            out.push(fill_template(CELLS_UNPLACED.1, &[&n.to_string(), name, &pos, sheet]));
        }
        let mut hid: Vec<i64> = hidden(sheet).into_iter().filter(|k| p.lost(*k).is_some()).collect();
        hid.sort_unstable();
        for key in hid {
            let name = p.lost(key).unwrap_or_default();
            let pos = split(key).1.to_string();
            out.push(fill_template(HIDDEN_UNPLACED.1, &[name, &pos, sheet]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(t: i64, pos: i64, mat: &str, th: f64) -> Row {
        Row {
            key: super::super::layer_key(t, pos),
            material: BVal::S(mat.into()),
            thickness: BVal::N(th),
            type_name: format!("T{t}"),
        }
    }

    fn rows(t: i64, l: &[(&str, f64)]) -> Vec<Row> {
        l.iter().enumerate().map(|(i, (m, th))| row(t, i as i64, m, *th)).collect()
    }

    fn k(t: i64, pos: i64) -> i64 {
        super::super::layer_key(t, pos)
    }

    #[test]
    fn insert_above_shifts_every_row_to_its_own_layer() {
        let old = rows(1, &[("brick", 0.25), ("wool", 0.1), ("plaster", 0.02)]);
        let new = rows(1, &[("render", 0.01), ("brick", 0.25), ("wool", 0.1), ("plaster", 0.02)]);
        let p = plan(&old, &new);
        assert_eq!((0..3).map(|i| p.new_of(k(1, i))).collect::<Vec<_>>(), vec![Some(k(1, 1)), Some(k(1, 2)), Some(k(1, 3))]);
        assert_eq!(p.old_of(k(1, 0)), None, "the new layer takes nothing");
        assert!(p.lost.is_empty());
    }

    #[test]
    fn swap_follows_and_removed_layer_is_lost_not_moved() {
        let old = rows(1, &[("brick", 0.25), ("wool", 0.1)]);
        let p = plan(&old, &rows(1, &[("wool", 0.1), ("brick", 0.25)]));
        assert_eq!(p.new_of(k(1, 0)), Some(k(1, 1)));
        assert_eq!(p.new_of(k(1, 1)), Some(k(1, 0)));
        let old = rows(1, &[("brick", 0.25), ("wool", 0.1), ("plaster", 0.02)]);
        let p = plan(&old, &rows(1, &[("brick", 0.25), ("plaster", 0.02)]));
        assert_eq!(p.new_of(k(1, 1)), None);
        assert_eq!(p.lost(k(1, 1)), Some("T1"));
        assert_eq!(p.old_of(k(1, 1)), Some(k(1, 2)), "pos 1 is now plaster, not the removed wool");
    }

    #[test]
    fn identical_layers_keep_position_and_types_stay_apart() {
        let mut old = rows(1, &[("board", 0.0125), ("board", 0.0125)]);
        old.extend(rows(2, &[("board", 0.0125), ("board", 0.0125)]));
        let p = plan(&old, &old.clone());
        for key in [k(1, 0), k(1, 1), k(2, 0), k(2, 1)] {
            assert_eq!(p.new_of(key), Some(key));
        }
        // a layer above in type 2 only: type 1 stays put, type 2 shifts
        let mut new = rows(1, &[("board", 0.0125), ("board", 0.0125)]);
        new.extend(rows(2, &[("stud", 0.1), ("board", 0.0125), ("board", 0.0125)]));
        let p = plan(&old, &new);
        assert_eq!(p.new_of(k(1, 1)), Some(k(1, 1)));
        assert_eq!(p.new_of(k(2, 0)), Some(k(2, 1)));
        assert_eq!(p.new_of(k(2, 1)), Some(k(2, 2)));
    }

    #[test]
    fn edited_in_place_follows_and_a_gone_type_is_not_lost() {
        let old = rows(1, &[("brick", 0.25), ("wool", 0.1)]);
        // thickness edited: same material follows
        let p = plan(&old, &rows(1, &[("brick", 0.25), ("wool", 0.12)]));
        assert_eq!(p.new_of(k(1, 1)), Some(k(1, 1)));
        // material and thickness replaced in place, nothing else moved
        let p = plan(&old, &rows(1, &[("brick", 0.25), ("cork", 0.08)]));
        assert_eq!(p.new_of(k(1, 1)), Some(k(1, 1)));
        // replaced and a layer added: no confident place
        let p = plan(&old, &rows(1, &[("render", 0.01), ("brick", 0.25), ("cork", 0.08)]));
        assert_eq!(p.new_of(k(1, 1)), None);
        assert!(p.lost(k(1, 1)).is_some());
        // the whole type deleted: its rows go with it
        let p = plan(&old, &rows(2, &[("brick", 0.25)]));
        assert_eq!(p.new_of(k(1, 0)), None);
        assert!(p.lost.is_empty());
    }
}
