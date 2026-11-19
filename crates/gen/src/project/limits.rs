// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The ranges a brief may ask for. The spec reader checks against them and the tool schema
//! shown to the assistant prints them, so the two never disagree.

use super::rules::{self, RuleDef};
use super::Typology;
use std::sync::LazyLock;

/// A number of the spec: its brief path, the range it may take, and the default when it is
/// the same for every typology (`None`: the typology decides). Read from the built-in rule set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamDef {
    pub path: &'static str,
    /// The rule this brief field is an alias of.
    pub rule: &'static str,
    pub min: f64,
    pub max: f64,
    pub default: Option<f64>,
}

impl ParamDef {
    /// The built-in rule `id` as a brief field. Panics for a rule without an alias: the rule
    /// file and this list are checked together by the tests.
    pub fn of(id: &str) -> ParamDef {
        let d: &'static RuleDef = rules::def(id).unwrap_or_else(|| panic!("no rule {id}"));
        ParamDef {
            path: d.alias.as_deref().unwrap_or_else(|| panic!("rule {id} has no brief field")),
            rule: &d.id,
            min: d.min,
            max: d.max,
            default: d.default,
        }
    }

    /// `2.7..6`, as the reader's messages and the schema print it.
    pub fn range(&self) -> String {
        format!("{}..{}", self.min, self.max)
    }

    pub fn contains(&self, x: f64) -> bool {
        x.is_finite() && (self.min..=self.max).contains(&x)
    }
}

/// A brief field of the built-in rule set.
pub type Field = LazyLock<ParamDef>;

macro_rules! field {
    ($(#[$m:meta])* $name:ident = $id:literal) => {
        $(#[$m])*
        pub static $name: $crate::project::limits::Field =
            std::sync::LazyLock::new(|| $crate::project::limits::ParamDef::of($id));
    };
}
pub(crate) use field;

field!(FLOOR_HEIGHT = "massing.floor_height");
field!(
    /// Depth of one band of rooms: a room deeper than 12 m is not lit from its facade.
    BAND_DEPTH = "depth.band"
);
field!(CORRIDOR = "corridor.width");
field!(WIDTH = "massing.width");
field!(
    /// Two bands of rooms and the corridor between them; the exact bounds follow the corridor.
    DEPTH = "massing.depth"
);
field!(GRID = "grid.span");
field!(STAIR_WIDTH = "core.stair_width");
field!(MAX_RISER = "core.max_riser");
field!(ROOF_SLOPE = "massing.roof_slope");
field!(ROOF_OVERHANG = "massing.roof_overhang");
field!(
    /// Up to a parking deck or warehouse hall.
    ROOM_AREA = "room.area"
);
field!(SETBACK = "massing.setback");
field!(CANTILEVER = "massing.cantilever");
field!(RHYTHM = "facade.rhythm");
field!(PAVING = "site.paving");
field!(
    /// How far a long facade bows out at its middle, m; at most a quarter of the length.
    CURVE = "facade.curve_s"
);
field!(CURVE_NORTH = "facade.curve_n");
field!(TREES = "site.trees");
field!(
    /// Parking storeys below ground a brief may ask for.
    BASEMENTS = "basement.count"
);
field!(RAFT_THICKNESS = "foundation.raft_thickness");
field!(STRIP_WIDTH = "foundation.strip_width");
field!(STRIP_DEPTH = "foundation.strip_depth");

/// Most trees a brief may ask for.
pub fn max_trees() -> u32 {
    TREES.max as u32
}

/// Most parking storeys below ground a brief may ask for.
pub fn max_basements() -> u32 {
    BASEMENTS.max as u32
}

/// The core fields of the spec, for the schema and the tests.
pub fn params() -> [ParamDef; 10] {
    [
        *FLOOR_HEIGHT,
        *BAND_DEPTH,
        *CORRIDOR,
        *WIDTH,
        *DEPTH,
        *GRID,
        *STAIR_WIDTH,
        *MAX_RISER,
        *ROOF_SLOPE,
        *ROOM_AREA,
    ]
}

pub const MAX_ROOMS_PER_FLOOR: u32 = 120;
/// Schema ceiling for apartments on one floor: the `max` of rule `room.apartments_max`.
pub const MAX_APARTMENTS_PER_FLOOR: u32 = 80;
/// Of one room line of the brief.
pub const MAX_ROOM_COUNT: u32 = 50;
pub const MAX_TEXT_CHARS: usize = 80;

impl Typology {
    pub const ALL: [Typology; 4] = [
        Typology::House,
        Typology::ApartmentSection,
        Typology::Office,
        Typology::School,
    ];

    /// The rule that caps this typology's storey count.
    pub fn floors_rule(self) -> &'static str {
        match self {
            Typology::House => "massing.floors_max_house",
            Typology::School => "massing.floors_max_school",
            Typology::ApartmentSection | Typology::Office => "massing.floors_max_tall",
        }
    }

    /// Storeys above ground this typology may have by the built-in rules; a project's own
    /// rules and the height rule (see [`floors_allowed`]) decide for a given brief.
    pub fn max_floors(self) -> u32 {
        let d = super::rules::def(self.floors_rule()).and_then(|d| d.default).unwrap_or(1.0);
        (d as u32).min(super::slots::MAX_STOREYS)
    }
}

/// Storeys above ground a brief may ask for: the least of the typology's count rule, the
/// height rule over the storey height, and the slot space; at least one.
pub fn floors_allowed(rules: &super::rules::RuleSet, t: Typology, floor_height: f64) -> u32 {
    let by_type = rules.get(t.floors_rule());
    let by_height = (rules.get("massing.height_max") / floor_height + 1e-9).floor();
    (by_type.min(by_height).max(1.0) as u32).min(super::slots::MAX_STOREYS)
}

/// Why the building is too tall for `massing.height_max`, if it is: the storeys add up to more.
pub fn height_refusal(rules: &super::rules::RuleSet, building_height: f64) -> Option<String> {
    let max = rules.get("massing.height_max");
    (building_height > max + 1e-9).then(|| {
        format!("the storeys add up to {building_height:.1} m, over massing.height_max {max} m; lower storey_heights or floors")
    })
}

/// `house 1..3, apartment_section 1..200, ...` for the schema.
pub fn floors_text() -> String {
    let each: Vec<String> = Typology::ALL
        .iter()
        .map(|t| format!("{} 1..{}", t.name(), t.max_floors()))
        .collect();
    each.join(", ")
}
