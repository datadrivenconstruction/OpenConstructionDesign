// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Numbers the detail builders decide by. Each is a rule of the `detail` group in the generator
//! rule set (`crates/gen/data/rules.json`, same id, same default); callers with a rule set pass
//! its values through [`DetailRules::from_lookup`], the rest use the built-in defaults.

/// Rule values of the detail builders, metres unless noted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetailRules {
    /// A door at least this wide gets two leaves.
    pub door_double_from: f64,
    /// A door at least this wide gets glazed leaves (an entrance).
    pub door_glazed_from: f64,
    /// Lever handle height above the finished floor.
    pub door_handle_z: f64,
    /// Target width of one glazing field; the window splits into fields near it.
    pub window_pane: f64,
    /// A window at least this tall gets a transom.
    pub window_transom_from: f64,
    /// Height of the top light above the transom.
    pub window_top_light: f64,
    /// Outside sill: projection past the wall face.
    pub sill_projection: f64,
    /// Guard height at a free edge (balcony, landing, roof edge).
    pub guard_height: f64,
    /// Handrail height above the pitch line of a flight.
    pub handrail_height: f64,
    /// Clear gap between balusters.
    pub baluster_gap: f64,
    /// Greatest distance between railing posts.
    pub post_spacing: f64,
    /// Handrail diameter.
    pub handrail_d: f64,
    /// Tread nosing: projection past the riser below.
    pub nosing: f64,
    /// Parapet coping: overhang past each face.
    pub coping_overhang: f64,
    /// Roof drainage: greatest roof edge length served by one downpipe.
    pub downpipe_spacing: f64,
}

/// `(rule id, default)` of every field, in field order.
pub const RULES: [(&str, f64); 15] = [
    ("detail.door.double_from", 1.5),
    ("detail.door.glazed_from", 1.2),
    ("detail.door.handle_z", 1.0),
    ("detail.window.pane", 0.9),
    ("detail.window.transom_from", 1.7),
    ("detail.window.top_light", 0.5),
    ("detail.window.sill_projection", 0.03),
    ("detail.rail.guard_height", 1.1),
    ("detail.rail.handrail_height", 0.9),
    ("detail.rail.baluster_gap", 0.1),
    ("detail.rail.post_spacing", 1.2),
    ("detail.rail.handrail_d", 0.042),
    ("detail.stair.nosing", 0.025),
    ("detail.roof.coping_overhang", 0.04),
    ("detail.roof.downpipe_spacing", 20.0),
];

impl Default for DetailRules {
    fn default() -> Self {
        Self::from_lookup(|_| None)
    }
}

impl DetailRules {
    /// Values from a rule set; an id it does not know keeps the built-in default.
    pub fn from_lookup(get: impl Fn(&str) -> Option<f64>) -> Self {
        let v: Vec<f64> = RULES
            .iter()
            .map(|(id, d)| get(id).filter(|x| x.is_finite()).unwrap_or(*d))
            .collect();
        DetailRules {
            door_double_from: v[0],
            door_glazed_from: v[1],
            door_handle_z: v[2],
            window_pane: v[3].max(0.2),
            window_transom_from: v[4],
            window_top_light: v[5].max(0.1),
            sill_projection: v[6].max(0.0),
            guard_height: v[7],
            handrail_height: v[8],
            baluster_gap: v[9].max(0.02),
            post_spacing: v[10].max(0.3),
            handrail_d: v[11].max(0.01),
            nosing: v[12].max(0.0),
            coping_overhang: v[13].max(0.0),
            downpipe_spacing: v[14].max(2.0),
        }
    }
}
