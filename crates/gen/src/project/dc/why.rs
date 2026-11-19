// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Why the data centre came out as it did: each decision names its `dc.*` rule and the numbers
//! it gave. The UI renders `gen.why.e.<key>` with the same slots in its own language.

use std::fmt::Display;

use crate::project::rules::{self, Layer};
use crate::project::why::Why;
use crate::project::why_text::{fill, Args};
use crate::project::ProjectSpec;

/// English templates of the fit-out's reasons.
pub const EN: &[(&str, &str)] = &[
    ("dc_raised", "racks stand on a {h} m raised floor; the chilled water runs in its void"),
    ("dc_rows", "{hall}: {n} racks in {rows} rows, cold aisles {cold} m, hot aisles {hot} m"),
    ("dc_orient", "{hall}: rows run between the cooling unit walls {span} m apart, {throw} m of throw from each"),
    ("dc_throw_over", "{hall}: the cooling unit walls are {span} m apart, more than twice the throw {throw} m"),
    ("dc_racks_short", "{asked} racks asked, {n} fit in the halls"),
    ("dc_crah", "{hall}: {n} cooling units of {kw} kW for {heat} kW of heat, {spare} spare"),
    ("dc_chillers_room", "{n} chillers of {kw} kW for {heat} kW of heat, {spare} spare, in {room}"),
    ("dc_chillers_yard", "{n} chillers of {kw} kW for {heat} kW of heat, {spare} spare, in the yard"),
    ("dc_ups", "path {path}: {n} UPS of {kw} kW for {load} kW of IT load, {spare} spare"),
    ("dc_battery", "path {path}: {n} battery cabinets, {kwh} kWh for {min} min at full load"),
    ("dc_gensets", "{n} generators of {kw} kW for {load} kW of facility load, {spare} spare"),
    ("dc_dn_hall", "{hall} headers: DN {dn} carries {flow} L/s at {v} m/s"),
    ("dc_dn_plant", "plant headers: DN {dn} carries {flow} L/s at {v} m/s"),
    ("dc_trays", "{n} cable trays {w} x {h} m, {gap} m over the racks, two per row on paths A and B"),
    ("dc_contain", "{hall}: {n} containment doors {w} m wide close the cold aisles at the row ends"),
    ("dc_pdus", "{hall}: {n} PDUs at the ends of {rows} rows, one per power path"),
    ("dc_unplaced_crah", "{hall}: {n} of {need} cooling units fit along the walls"),
    ("dc_unplaced_ups", "path {path}: {n} of {need} UPS fit in {room}"),
    ("dc_unplaced_battery", "path {path}: {n} of {need} battery cabinets fit in {room}"),
    ("dc_no_room_ups", "no room for the UPS of path {path}: tag one ups_room or name it so"),
    ("dc_no_room_chillers", "no plant room or yard space for {n} chillers: tag a room cooling_plant or change dc.yard_side"),
    ("dc_route_supply", "{hall} supply main {path}: no route clear of the columns and other runs, its end is capped"),
    ("dc_route_return", "{hall} return main {path}: no route clear of the columns and other runs, its end is capped"),
    ("dc_route_feeder", "{hall} tray feeder {path}: no route clear of the columns and other runs, its end is capped"),
    ("dc_no_feed", "{hall} tray feeder {path}: no UPS on path {path}, its end is capped"),
];

pub fn template(key: &str) -> Option<&'static str> {
    EN.iter().find(|(k, _)| *k == key).map(|(_, t)| *t)
}

pub(crate) fn s(x: impl Display) -> String {
    x.to_string()
}

/// A number as the reasons print it: two decimals at most (`format!` rounds them).
pub(crate) fn n2(x: f64) -> String {
    let t = format!("{x:.2}");
    t.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The reasons gathered while laying out.
pub(crate) struct Whys<'a> {
    spec: &'a ProjectSpec,
    pub out: Vec<Why>,
    /// Redundancy records: system, duty units, built units.
    pub red: Vec<(&'static str, usize, usize)>,
}

impl<'a> Whys<'a> {
    pub fn new(spec: &'a ProjectSpec) -> Self {
        Whys { spec, out: vec![], red: vec![] }
    }

    /// A reason of `target` from `rule`, its effect filled from the template `key`.
    pub fn add(&mut self, target: &str, rule: &str, key: &'static str, args: Args) {
        let x = self.spec.rules.value(rule).unwrap_or_else(|| rules::rule(rule));
        let value = rules::def(rule).map_or(serde_json::json!(x), |d| d.value_json(x));
        let source = match self.spec.rules.source(rule) {
            Some(Layer::Builtin) | None => "builtin".to_string(),
            Some(l) => match self.spec.rules.cite(rule) {
                Some(c) => format!("{}: {c}", l.name()),
                None => l.name().to_string(),
            },
        };
        let effect = fill(template(key).unwrap_or(key), &args);
        self.out.push(Why { target: target.to_string(), rule: rule.to_string(), value, source, effect, key, args });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_slots_balanced() {
        let mut seen = std::collections::BTreeSet::new();
        for (k, t) in EN {
            assert!(seen.insert(*k), "duplicate {k}");
            assert!(k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{k}");
            assert_eq!(t.matches('{').count(), t.matches('}').count(), "{k}");
        }
    }
}
