// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Selection as element keys (bridge `select`, both ways): the ERP shows the estimate lines of
//! the keys the editor selects, and the editor highlights the keys the ERP sends.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use crate::ai::rows::{key_of_sel, sel_of};
use crate::ai::sets::{table_of, Key};
use crate::editor::{Editor, Sel};

/// Categories that have a selection (`ai::rows::sel_of`), door before window: both are openings.
const CATS: [&str; 14] = [
    "wall", "door", "curtain", "floor", "room", "mep", "comp", "column", "beam", "ceiling", "imported", "roof", "stair",
    "railing",
];

/// `ifc_class` and `type_key` of a key, from the last rows the page holds (tip or draft).
pub type RowInfo = HashMap<String, (String, String)>;

fn door_of(ed: &Editor) -> impl Fn(i64) -> &'static str + '_ {
    |id| match ed.model.opening_kind(id).as_deref() {
        Ok("window") => "window",
        _ => "door",
    }
}

/// Element keys of the selection in its order, each once. An element without a stored key gets
/// the key the next package will mint for it (`ddc_model::minted_key`), so a view-only page
/// names it too.
pub fn keys_of_selection(ed: &Editor) -> Vec<String> {
    let door = door_of(ed);
    let mut seen = HashSet::new();
    ed.selection
        .iter()
        .filter_map(|s| key_of_sel(*s, &door))
        .filter_map(|k| match ed.model.element_key(k.table(), k.id) {
            Ok(Some((key, _))) => Some(key),
            Ok(None) => Some(ddc_model::minted_key(k.table(), k.id)),
            Err(_) => None,
        })
        .filter(|k| seen.insert(k.clone()))
        .collect()
}

/// The bridge `select` elements of the selection: the key, plus `ifc_class` and `type_key` when
/// `info` knows the key (an element added since the last package has the key only).
pub fn selection_elements(ed: &Editor, info: &RowInfo) -> Vec<Value> {
    keys_of_selection(ed)
        .into_iter()
        .map(|k| {
            let mut e = json!({ "element_key": k });
            if let Some((ifc, tkey)) = info.get(&k) {
                let class_ok = ifc.len() > 3 && ifc.starts_with("Ifc") && ifc[3..].chars().all(|c| c.is_ascii_alphabetic());
                if class_ok {
                    e["ifc_class"] = json!(ifc);
                }
                if !tkey.is_empty() && tkey.chars().count() <= 200 {
                    e["type_key"] = json!(tkey);
                }
            }
            e
        })
        .collect()
}

/// Selects the elements of `keys` (unknown keys are skipped); returns how many were found.
/// Keys not stored yet are matched by the key they will be minted with.
pub fn select_keys(ed: &mut Editor, keys: &[String]) -> Result<usize, String> {
    let want: HashSet<&str> = keys.iter().map(String::as_str).collect();
    let sel_in = |table: &str, id: i64| CATS.iter().find(|c| table_of(c) == table).and_then(|cat| sel_of(Key { cat: *cat, id }));
    let mut found: Vec<(String, Sel)> = vec![];
    let stored = ed.model.element_keys().map_err(|e| e.to_string())?;
    for ((table, id), key) in &stored {
        if want.contains(key.as_str()) {
            if let Some(s) = sel_in(table, *id) {
                found.push((key.clone(), s));
            }
        }
    }
    // pending keys `x/<table>/<guid>`: only tables that have a selection, ids without a stored key
    let pending: HashSet<&str> = want.iter().filter(|k| !found.iter().any(|f| f.0 == **k)).filter_map(|k| k.strip_prefix("x/")?.split('/').next()).collect();
    for table in pending {
        if !CATS.iter().any(|c| table_of(c) == table) {
            continue;
        }
        let ids: Vec<i64> = {
            let mut st = ed.model.sql().prepare(&format!("SELECT id FROM \"{table}\"")).map_err(|e| e.to_string())?;
            let rows = st.query_map([], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())?;
            rows.filter_map(Result::ok).collect()
        };
        for id in ids {
            if stored.contains_key(&(table.to_string(), id)) {
                continue;
            }
            let key = ddc_model::minted_key(table, id);
            if want.contains(key.as_str()) {
                if let Some(s) = sel_in(table, id) {
                    found.push((key, s));
                }
            }
        }
    }
    // the order the host sent
    found.sort_by_key(|(k, _)| keys.iter().position(|x| x == k));
    ed.selection = found.into_iter().map(|(_, s)| s).collect();
    Ok(ed.selection.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Editor {
        let brief: serde_json::Value =
            serde_json::from_str(include_str!("../../../io/contract/examples/brief.de-residential.json")).unwrap();
        crate::link::gen::build(&brief).unwrap().0
    }

    #[test]
    fn selected_walls_give_their_keys_and_the_keys_select_them_back() {
        let mut ed = example();
        let walls: Vec<i64> = ed.model.element_keys().unwrap().keys().filter(|(t, _)| t == "wall").map(|(_, id)| *id).take(2).collect();
        assert_eq!(walls.len(), 2, "the example has keyed walls");
        ed.selection = walls.iter().map(|w| Sel::Wall(*w)).collect();
        let keys = keys_of_selection(&ed);
        assert_eq!(keys.len(), 2);
        ed.selection.clear();
        let mut asked = keys.clone();
        asked.push("no/such/key".into());
        asked.push("x/sqlite_master/abc".into());
        assert_eq!(select_keys(&mut ed, &asked).unwrap(), 2);
        assert_eq!(ed.selection, walls.iter().map(|w| Sel::Wall(*w)).collect::<Vec<_>>());
        assert_eq!(keys_of_selection(&ed), keys, "round trip in the order sent");
        assert_eq!(select_keys(&mut ed, &[]).unwrap(), 0);
        assert!(ed.selection.is_empty());
    }

    #[test]
    fn an_element_without_a_key_is_named_by_the_key_it_will_be_minted_with() {
        let mut ed = example();
        let wall = a_wall(&ed);
        // drop its stored key: an element drawn by hand in the page has none
        ed.model.sql().execute("DELETE FROM prop WHERE kind='wall' AND ref=?1", [wall]).unwrap();
        assert!(ed.model.element_key("wall", wall).unwrap().is_none());
        ed.selection = vec![Sel::Wall(wall)];
        let keys = keys_of_selection(&ed);
        assert_eq!(keys, vec![ddc_model::minted_key("wall", wall)]);
        ed.selection.clear();
        assert_eq!(select_keys(&mut ed, &keys).unwrap(), 1, "found again before it is stored");
        assert_eq!(ed.selection, vec![Sel::Wall(wall)]);
        // the package mints exactly that key
        assert_eq!(ed.model.ensure_element_key("wall", wall).unwrap().0, keys[0]);
    }

    fn a_wall(ed: &Editor) -> i64 {
        ed.model.element_keys().unwrap().keys().filter(|(t, _)| t == "wall").map(|(_, id)| *id).min().expect("a keyed wall")
    }

    #[test]
    fn select_elements_carry_class_and_type_when_the_rows_know_the_key() {
        let mut ed = example();
        let wall = a_wall(&ed);
        ed.selection = vec![Sel::Wall(wall)];
        let key = keys_of_selection(&ed).remove(0);
        assert_eq!(selection_elements(&ed, &RowInfo::new()), vec![json!({"element_key": key})]);
        let info = RowInfo::from([(key.clone(), ("IfcWall".to_string(), "wall/brick-240".to_string()))]);
        assert_eq!(selection_elements(&ed, &info), vec![json!({"element_key": key, "ifc_class": "IfcWall", "type_key": "wall/brick-240"})]);
        let bad = RowInfo::from([(key.clone(), ("wall".to_string(), String::new()))]);
        assert_eq!(selection_elements(&ed, &bad), vec![json!({"element_key": key})], "off the contract: left out");
    }
}
