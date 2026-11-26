// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Room words beyond the built-in kinds (foyer, gallery, atrium, ...), read from
//! `data/room_kinds.json`: each builds as a base kind and brings its name and its rules.

use super::rules::RuleSet;
use super::RoomKind;
use serde_json::Value as J;
use std::sync::OnceLock;

const DATA: &str = include_str!("../../data/room_kinds.json");

#[derive(Clone, Debug, PartialEq)]
pub struct KindDef {
    pub id: String,
    pub base: RoomKind,
    pub aliases: Vec<String>,
    pub name: super::names::Text,
    /// Rule holding the smallest area of this room, when it has one.
    pub min_area_rule: Option<String>,
    pub ground_only: bool,
    pub in_apartment: bool,
    pub note: Option<String>,
}

/// A room line that named a data kind, kept for the checks that need the final rule values.
#[derive(Clone, Debug, PartialEq)]
pub struct Asked {
    pub path: String,
    pub def: &'static KindDef,
    pub area: f64,
    pub apartment: bool,
}

pub fn defs() -> &'static [KindDef] {
    static CELL: OnceLock<Vec<KindDef>> = OnceLock::new();
    CELL.get_or_init(|| {
        let j: J = serde_json::from_str(DATA).expect("room_kinds.json");
        let text = |v: &J| v.as_str().expect("room_kinds.json text").to_string();
        j["kinds"]
            .as_array()
            .expect("room_kinds.json kinds")
            .iter()
            .map(|k| KindDef {
                id: text(&k["id"]),
                base: RoomKind::parse(k["base"].as_str().unwrap_or_default()).expect("room_kinds.json base kind"),
                aliases: k["aliases"].as_array().map_or(vec![], |a| a.iter().map(|x| text(x).to_lowercase()).collect()),
                name: super::names::Text::src(&text(&k["name"]["en"]), "kind"),
                min_area_rule: k["min_area_rule"].as_str().map(str::to_string),
                ground_only: k["ground_only"].as_bool().unwrap_or(false),
                in_apartment: k["in_apartment"].as_bool().unwrap_or(false),
                note: k["note"].as_str().map(str::to_string),
            })
            .collect()
    })
}

/// The data kind a word names: its id or any alias, in any case.
pub fn find(word: &str) -> Option<&'static KindDef> {
    let w = word.trim().to_lowercase();
    defs().iter().find(|d| d.id == w || d.aliases.contains(&w))
}

/// The kind a brief's word builds as, and the data kind behind it when there is one. A
/// built-in kind word without an entry has none.
pub fn parse(word: &str) -> Option<(RoomKind, Option<&'static KindDef>)> {
    match find(word) {
        Some(d) => Some((d.base, Some(d))),
        None => RoomKind::parse(word).map(|k| (k, None)),
    }
}

/// Every word a brief may use, for the hints of a wrong one.
pub fn words() -> Vec<&'static str> {
    let mut w: Vec<&'static str> = RoomKind::ASKED.to_vec();
    for d in defs() {
        for x in std::iter::once(&d.id).chain(&d.aliases) {
            if !w.contains(&x.as_str()) {
                w.push(x.as_str());
            }
        }
    }
    w
}

/// Problems of the data kinds a brief asked, against the final rule values. `multi_storey`:
/// the building has upper floors that the `rooms` list repeats on.
pub fn check(asked: &[Asked], rules: &RuleSet, multi_storey: bool, notes: &mut Vec<String>) -> Vec<(String, String)> {
    let mut bad = vec![];
    for a in asked {
        let d = a.def;
        if a.apartment && !d.in_apartment {
            bad.push((format!("{}.kind", a.path), format!("a {} is a room of the building, not of an apartment", d.id)));
            continue;
        }
        if let Some(r) = &d.min_area_rule {
            let least = rules.get(r);
            if a.area < least - f64::EPSILON {
                bad.push((format!("{}.area", a.path), format!("a {} is at least {least} m2 ({r}); {} m2 asked", d.id, a.area)));
            }
        }
        if d.ground_only && multi_storey && a.path.starts_with("rooms[") {
            bad.push((format!("{}.kind", a.path), format!("a {} belongs on the ground floor: put it in ground_rooms", d.id)));
        }
        if d.id != d.base.key() {
            let tail = d.note.as_deref().map(|n| format!("; {n}")).unwrap_or_default();
            notes.push(format!("{}: '{}' is built as a {}{tail}", a.path, d.id, d.base.key()));
        }
    }
    bad
}
