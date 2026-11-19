// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Why the station came out as it did: one reason per decision, naming the rule, its value,
//! the layer that set it and the effect. The English template fills `effect` for the tool
//! reply; the UI renders `gen.why.e.<key>` from its own catalog with the same `{slots}`.

use serde_json::{json, Value as J};

/// `(key, English template)`; the UI catalog's `gen.why.e.<key>` in `en` is the same text.
pub const EN: &[(&str, &str)] = &[
    ("station_box", "box {l} x {w} m inside the walls: platform {pl} m and two end zones of {ez} m; platform {pw} m wide between two trackways of {tw} m"),
    ("station_levels", "rail {rail} m below ground: platform level at {pz} m, concourse at {cz} m, base slab top at {bz} m"),
    ("station_clear", "{h} m clear over the platform under the concourse slab, at least {min} m"),
    ("station_dwall", "diaphragm walls {t} m thick down to {bot} m: excavation {exc} m and a toe of {toe} m below it"),
    ("station_escalators", "{n} escalators in {banks} bank(s): peak {flow} people/h over {cap} per escalator, {sb} standby per bank"),
    ("station_well", "escalator well {len} x {wid} m: rise {rise} m at {angle} degrees plus landings of {land} m"),
    ("station_stairs", "{n} fixed stair(s) {w} m wide beside the escalators, each well {len} m long"),
    ("station_lifts", "{n} lift shaft(s) of {s} x {s} m from the platform to the street"),
    ("station_psd", "{n} platform screen doors on each platform edge, {w} m wide every {pitch} m, screen {h} m high"),
    ("station_vent", "{n} ventilation shaft(s) of {s} x {s} m: {flow} m3/s at {v} m/s per end"),
    ("station_columns", "{n} column(s) of {c} m along the centreline, at most {sp} m apart, none in a well"),
    ("station_vaults", "{n} barrel vault segments of {bay} m over the trackways and the platform, rise {k} of the span, crown under the concourse slab"),
    ("station_ground", "ground plane in four strips round the box walls, {m} m beyond them; the box is a hole, no earth over it"),
    ("station_evac", "platform cleared in {t} min: {people} people over {cap} per minute up the escalators and stairs, at most {lim} min"),
];

/// The English template of `key`.
pub fn template(key: &str) -> Option<&'static str> {
    EN.iter().find(|e| e.0 == key).map(|e| e.1)
}

/// `key` filled with `args`; an unknown key prints itself so a gap shows in the reply.
pub fn effect(key: &str, args: &[(&'static str, String)]) -> String {
    let mut s = template(key).unwrap_or(key).to_string();
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/// One reason: `effect` because `rule` = `value`.
#[derive(Clone, Debug, PartialEq)]
pub struct Why {
    /// The part it explains: `box`, `levels`, `walls`, `escalators`, `stairs`, `lifts`,
    /// `vent`, `columns`, `evac`.
    pub target: &'static str,
    pub rule: &'static str,
    pub value: f64,
    /// Layer of the value: `builtin`, `norm`, `preset`, `project` or `brief`.
    pub source: &'static str,
    pub key: &'static str,
    pub args: Vec<(&'static str, String)>,
    /// English, for the tool reply.
    pub effect: String,
}

impl Why {
    pub fn to_json(&self) -> J {
        json!({"ref": self.target, "rule": self.rule, "value": self.value, "source": self.source,
               "key": format!("gen.why.e.{}", self.key), "effect": self.effect})
    }
}
