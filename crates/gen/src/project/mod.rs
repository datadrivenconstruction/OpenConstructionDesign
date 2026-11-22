// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Project from a brief: a typed building program the assistant fills, and a deterministic
//! builder that turns it into storeys of walls, openings, rooms, slabs, stairs and a roof.
//! The assistant never places geometry itself; the same spec always gives the same project.

mod anygeo;
mod arrange;
pub mod assembly;
mod basement;
pub mod block;
pub mod brief;
mod build;
pub mod care;
pub mod conflict;
mod coord;
pub mod critique;
mod curve;
pub mod dc;
mod elec;
mod circuits;
pub use circuits::{design_current, schedule_csv, voltage_drop};
pub mod lift;
mod sprinkler;
#[cfg(test)]
mod sprinkler_tests;
mod standpipe;
mod fire_check;
#[cfg(test)]
mod fire_tests;
mod fitout;
pub mod design;
mod facade;
#[cfg(test)]
mod fit_audit;
pub mod fire;
mod fit_rules;
pub mod footprint;
mod foundation;
mod furnish;
#[cfg(test)]
mod furnish_tests;
mod gates;
pub mod feature_words;
pub mod kinds;
pub mod landscape;
mod hvac;
mod keys;
pub mod locks;
pub mod limits;
mod massing;
mod typo_program;
pub mod mech;
pub mod names;
pub mod nav;
mod offset;
pub mod openings;
mod place;
pub mod railings;
mod plate;
mod shell;
pub mod presets;
pub mod program;
pub mod programme;
pub mod rules;
pub mod score;
pub mod site;
pub mod variants;
mod void;
mod spec;
pub mod stack;
pub mod structure;
pub mod timber;
pub mod steer;
mod svc;
mod tile;
mod trees;
pub use trees::dims as tree_dims;
pub use trees::species_of;
pub mod typology;
mod water;
pub mod why;
pub mod why_text;

pub use anygeo::{curve_walls, usable_rect, GCurve, Usable};
pub use build::build_project;
pub use design::{Balconies, Design, FacadeSystem, Massing, Structure};
pub use ddc_geom::axes::axis_letter;
pub use foundation::{plan_foundation, FoundationPlan, StripPlan};
pub use gates::{check_project, Gates};
pub use hvac::air_ports;
pub use keys::{project_keys, ProjectKeys, StoreyKeys, KEY_SCHEME};
pub use shell::{envelope as shell_envelope, GShell, ShellPatch, ShellReport, ROOF_LAYER_NAMES};
pub use programme::{default_programme, Programme};
pub use coord::route_metrics;
pub use svc::{duct_faults, is_info, services_check, services_errors, svc_ports, SvcPort};
pub use void::void_check;
pub use fire_check::{fire_check, hazard_of, Hazard};
pub use spec::{merge_patch, parse_spec, parse_spec_with, typology_names, SpecError, SpecIssue};

use crate::GLayout;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Typology {
    House,
    ApartmentSection,
    Office,
    School,
}

impl Typology {
    pub const NAMES: [&'static str; 4] = ["house", "apartment_section", "office", "school"];

    pub fn name(self) -> &'static str {
        match self {
            Typology::House => "house",
            Typology::ApartmentSection => "apartment_section",
            Typology::Office => "office",
            Typology::School => "school",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RoomKind {
    Living,
    Bedroom,
    Kitchen,
    Bath,
    Wc,
    Hall,
    Storage,
    Office,
    Meeting,
    Classroom,
    Lab,
    Staff,
    Lobby,
    Technical,
    Corridor,
    Stair,
    Lift,
    Parking,
    Retail,
    Dining,
    Workshop,
    Ward,
    Gym,
}

impl RoomKind {
    /// Kinds a brief may ask for; corridor, stair and lift come from the core.
    pub const ASKED: [&'static str; 20] = [
        "living",
        "bedroom",
        "kitchen",
        "bath",
        "wc",
        "hall",
        "storage",
        "office",
        "meeting",
        "classroom",
        "lab",
        "staff",
        "lobby",
        "technical",
        "parking",
        "retail",
        "dining",
        "workshop",
        "ward",
        "gym",
    ];

    pub fn parse(s: &str) -> Option<RoomKind> {
        use RoomKind::*;
        Some(match s {
            "living" => Living,
            "bedroom" => Bedroom,
            "kitchen" => Kitchen,
            "bath" => Bath,
            "wc" => Wc,
            "hall" => Hall,
            "storage" => Storage,
            "office" => Office,
            "meeting" => Meeting,
            "classroom" => Classroom,
            "lab" => Lab,
            "staff" => Staff,
            "lobby" | "atrium" | "foyer" => Lobby,
            // a gallery is a large room of its own kind of use: built as a hall
            "gallery" | "exhibition" | "exhibit" => Hall,
            "cafe" => Dining,
            "shop" => Retail,
            "technical" => Technical,
            "parking" => Parking,
            "retail" => Retail,
            "dining" => Dining,
            "workshop" => Workshop,
            "ward" => Ward,
            "gym" => Gym,
            _ => return None,
        })
    }

    /// The spec word of a kind; the core kinds have their own.
    pub fn key(self) -> &'static str {
        use RoomKind::*;
        match self {
            Corridor => "corridor",
            Stair => "stair",
            Lift => "lift",
            k => Self::ASKED
                .into_iter()
                .find(|n| Self::parse(n) == Some(k))
                .unwrap_or("room"),
        }
    }

    /// Rooms people stay in: each needs a window.
    pub fn habitable(self) -> bool {
        use RoomKind::*;
        matches!(
            self,
            Living
                | Bedroom
                | Kitchen
                | Office
                | Meeting
                | Classroom
                | Lab
                | Staff
                | Retail
                | Dining
                | Workshop
                | Ward
                | Gym
        )
    }

    /// Large open rooms that take the deep bay of a deep plan, between columns.
    pub fn open_hall(self) -> bool {
        use RoomKind::*;
        matches!(self, Parking | Retail | Workshop | Gym | Dining)
    }

    /// Rooms through which other rooms are reached.
    pub fn circulation(self) -> bool {
        matches!(self, RoomKind::Hall | RoomKind::Lobby | RoomKind::Corridor)
    }

    /// Inner zone of an apartment: no facade needed.
    pub fn inner(self) -> bool {
        matches!(
            self,
            RoomKind::Bath | RoomKind::Wc | RoomKind::Storage | RoomKind::Technical
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoomReq {
    pub name: String,
    pub kind: RoomKind,
    /// Net area asked for, m2, measured on wall centre lines.
    pub area: f64,
    pub count: u32,
    pub tags: RoomTags,
}

/// Canonical qualifiers a spec room carries (`"tags": ["accessible"]`). Fit-out rules match
/// them, never words of the room's name, so a room named in any language is fitted alike.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RoomTags(pub u16);

impl RoomTags {
    pub const IDS: [&'static str; 9] = [
        "accessible",
        "guest",
        "clinical",
        "data_hall",
        "ups_room",
        "battery_room",
        "cooling_plant",
        "operating",
        "isolation",
    ];

    pub fn parse(id: &str) -> Option<RoomTags> {
        Self::IDS.iter().position(|x| *x == id).map(|i| RoomTags(1 << i))
    }

    pub fn with(self, o: RoomTags) -> RoomTags {
        RoomTags(self.0 | o.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Shares at least one tag with `o`.
    pub fn meets(self, o: RoomTags) -> bool {
        self.0 & o.0 != 0
    }
}

/// Tags of the spec's rooms by kind and the exact name the spec gave: the builders carry a
/// room's kind and name, so the fit-out finds its tags without a field on every stage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoomTagTable(pub Vec<(RoomKind, String, RoomTags)>);

impl RoomTagTable {
    pub fn of(&self, kind: RoomKind, name: &str) -> RoomTags {
        self.0.iter().filter(|x| x.0 == kind && x.1 == name).fold(RoomTags::default(), |a, x| a.with(x.2))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ApartmentReq {
    pub name: String,
    pub count: u32,
    pub rooms: Vec<RoomReq>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoofKind {
    Flat,
    Pitched,
}

/// What carries the building into the ground. `Raft` is one slab under the footprint; `Strip` is a
/// strip under every bearing (exterior and party) wall. Sizes are metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FoundationSpec {
    Raft { thickness: f64 },
    Strip { width: f64, depth: f64 },
}

/// The building program. Every field has a typology default, so a short brief is enough.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectSpec {
    pub typology: Typology,
    pub name: String,
    /// Russian data tables (programmes, presets) that exist in en and ru only.
    pub ru: bool,
    /// UI locale of default room and level names (one of [`names::locales`]), `en` by default.
    pub lang: &'static str,
    pub room_tags: RoomTagTable,
    pub floors: u32,
    pub floor_height: f64,
    /// Floor to floor of every storey from the ground when the storeys differ (a tall level
    /// under normal ones); empty: all storeys are `floor_height`.
    pub storey_heights: Vec<f64>,
    /// Footprint along x and y; `None` lets the program decide.
    pub width: Option<f64>,
    pub depth: Option<f64>,
    /// Depth of the room bands on each side of the corridor.
    pub band_depth: f64,
    /// Deep plan: the open bay north of the corridor, in place of the second band.
    pub hall_depth: Option<f64>,
    pub corridor: f64,
    /// Largest distance between grid axes.
    pub grid: f64,
    pub stair_width: f64,
    pub max_riser: f64,
    pub lift: bool,
    /// Lift cars the brief names; `None` lets the traffic rule size them.
    pub lifts: Option<u32>,
    /// Sprinklers: the brief's yes or no, `None` for the height rule.
    pub sprinklers: Option<bool>,
    /// Automatic services, fire protection and ventilation; off leaves them to the person.
    pub mep_auto: bool,
    /// Rooms of a typical floor (not apartment sections).
    pub rooms: Vec<RoomReq>,
    /// Rooms of the ground floor; empty means the typical floor.
    pub ground_rooms: Vec<RoomReq>,
    /// Rooms added to one storey (from 0) on top of its programme.
    pub storey_rooms: std::collections::BTreeMap<usize, Vec<RoomReq>>,
    /// Apartments of a typical floor (apartment sections only).
    pub apartments: Vec<ApartmentReq>,
    pub roof: RoofKind,
    pub roof_slope: f64,
    pub fire_rating: String,
    pub floor_finish: String,
    pub wall_finish: String,
    pub ceiling_finish: String,
    pub origin: (f64, f64),
    /// `None`: no foundation is built (the spec did not ask for one).
    pub foundation: Option<FoundationSpec>,
    /// What spare band length becomes, from the programme template.
    pub spare: programme::Spare,
    /// The brief's patch of the fit-out rules (RFC 7386 over `data/furnish_rules.json`).
    pub furnish: Option<serde_json::Value>,
    /// The core half opposite the stair when there is no lift room: kind and name.
    pub core_rest: (RoomKind, String),
    /// Massing, facade, balconies, terrace and site; the default is the plain bar.
    pub design: Design,
    /// Power, lighting, water and drainage the brief asks for.
    pub services: Services,
    /// Typology template the brief named (hotel, clinic...), built as `typology`.
    pub template: Option<String>,
    /// Features the brief asked for, as keywords; the brief check says which were built.
    pub features: Vec<String>,
    /// Parking storeys below ground, 0..=3.
    pub basements: u32,
    /// What the reader decided for the brief instead of refusing it (a contradiction resolved).
    pub notes: Vec<String>,
    /// Any footprint polygon and its per-floor variation; `None` keeps the bar massings.
    pub plate: Option<footprint::Plate>,
    /// How a pitched roof meets its edges; `None` slopes every edge.
    pub roof_form: Option<footprint::RoofForm>,
    /// The rule layers above the built-in set; the builder reads every decision number here.
    pub rules: rules::RuleSet,
    /// The programme the rooms come from: the brief's, else the preset's or typology's.
    pub program: program::Program,
    /// The plot, its zoning limits, the terrain and the site locks.
    pub site: site::SiteSpec,
    /// Rooms a regeneration keeps where they are; the rest is placed freely around them.
    pub pins: Option<Pins>,
    /// The typology as data: spaces with their needs, zones, levels below ground, plant.
    pub model: Option<std::sync::Arc<typology::Model>>,
}

/// The plan rooms are pinned in: the footprint they were built on and the pinned rooms, all
/// in plan coordinates (before the brief's origin).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Pins {
    pub width: f64,
    pub depth: f64,
    /// Depth of the south side, below the corridor.
    pub south: f64,
    /// Where the core starts along the footprint.
    pub core_x: Option<f64>,
    pub rooms: Vec<Pin>,
    /// Zones of apartments with a pinned room (kind `Hall`, the owner the apartment): the
    /// zone stays, its unpinned rooms are laid out again inside it.
    pub flats: Vec<Pin>,
}

/// One room kept as it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Pin {
    pub storey: usize,
    pub kind: RoomKind,
    pub name: String,
    pub owner: PRole,
    pub polygon: Vec<(f64, f64)>,
}

impl Pins {
    /// The brief's `pins`: what a regeneration around geometry locks kept, so the same brief
    /// builds the same plan again.
    pub fn to_json(&self) -> serde_json::Value {
        let list = |v: &[Pin]| -> Vec<serde_json::Value> { v.iter().map(Pin::to_json).collect() };
        serde_json::json!({"width": self.width, "depth": self.depth, "south": self.south, "core_x": self.core_x,
                           "rooms": list(&self.rooms), "flats": list(&self.flats)})
    }

    pub fn from_json(v: &serde_json::Value) -> Result<Pins, String> {
        let num = |k: &str| v[k].as_f64().filter(|x| x.is_finite() && *x > 0.0).ok_or(format!("{k} must be a positive number"));
        let core_x = match &v["core_x"] {
            serde_json::Value::Null => None,
            x => Some(x.as_f64().ok_or("core_x must be a number")?),
        };
        let list = |key: &str, need: bool| -> Result<Vec<Pin>, String> {
            match v[key].as_array() {
                Some(a) => a.iter().enumerate().map(|(i, r)| Pin::from_json(r).map_err(|m| format!("{key}[{i}]: {m}"))).collect(),
                None if need => Err(format!("{key} must be a list")),
                None => Ok(vec![]),
            }
        };
        Ok(Pins { width: num("width")?, depth: num("depth")?, south: num("south")?, core_x, rooms: list("rooms", true)?, flats: list("flats", false)? })
    }
}

impl Pin {
    fn to_json(&self) -> serde_json::Value {
        let apt = match self.owner {
            PRole::Public => serde_json::Value::Null,
            PRole::Apartment(n) => serde_json::json!(n),
        };
        serde_json::json!({"storey": self.storey, "kind": self.kind.key(), "name": self.name, "apartment": apt,
                           "polygon": self.polygon.iter().map(|p| [p.0, p.1]).collect::<Vec<_>>()})
    }

    fn from_json(r: &serde_json::Value) -> Result<Pin, String> {
        let kind = r["kind"].as_str().and_then(RoomKind::parse).ok_or("unknown kind")?;
        let storey = r["storey"].as_u64().ok_or("storey must be a whole number")? as usize;
        let owner = match r["apartment"].as_u64() {
            Some(n) => PRole::Apartment(u32::try_from(n).map_err(|_| "apartment out of range")?),
            None => PRole::Public,
        };
        let mut polygon = vec![];
        for p in r["polygon"].as_array().ok_or("polygon must be a list")? {
            match (p[0].as_f64(), p[1].as_f64()) {
                (Some(x), Some(y)) => polygon.push((x, y)),
                _ => return Err("polygon points are [x, y]".to_string()),
            }
        }
        if polygon.len() < 3 {
            return Err("polygon needs three points".to_string());
        }
        Ok(Pin { storey, kind, name: r["name"].as_str().unwrap_or_default().to_string(), owner, polygon })
    }
}

/// Where the sockets and lights of an apartment are switched and protected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Boards {
    /// A board in every apartment's hall, fed from the floor board.
    #[default]
    Unit,
    /// One board per floor for everything on it.
    Floor,
}

impl Boards {
    pub const NAMES: [&'static str; 2] = ["unit", "floor"];
    pub const ALL: [Boards; 2] = [Boards::Unit, Boards::Floor];
}

/// Services asked by the brief; nothing is designed unless it is switched on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Services {
    pub electrical: bool,
    pub water: bool,
    /// Washing machine, dishwasher and cooker where the rooms take them.
    pub appliances: bool,
    pub boards: Boards,
    /// Mechanical ventilation: not set builds it as the typology asks, `false` leaves it out.
    pub hvac: Option<bool>,
    /// Circuit lines on the plans: home runs to the boards and the wiring between devices.
    pub wires: bool,
    /// A cable trace in 3D along the carriers from each board to its circuits' first device.
    pub traces: bool,
}

impl ProjectSpec {
    /// Floor to floor of storey `k`: its own height, else `floor_height`.
    pub fn storey_height(&self, k: usize) -> f64 {
        self.storey_heights.get(k).copied().unwrap_or(self.floor_height)
    }

    /// Level of storey `k` above the ground floor: the heights below it added up.
    pub fn storey_elevation(&self, k: usize) -> f64 {
        (0..k).map(|i| self.storey_height(i)).sum()
    }

    /// The top of the last storey, where the roof stands.
    pub fn building_height(&self) -> f64 {
        self.storey_elevation(self.floors as usize)
    }

    /// The tallest storey: the stairs and the core are fitted to it.
    pub fn tallest_storey(&self) -> f64 {
        (0..self.floors as usize).map(|k| self.storey_height(k)).fold(self.floor_height, f64::max)
    }

    /// The typology the brief named: the template when there is one.
    pub fn typology_name(&self) -> &str {
        self.template.as_deref().unwrap_or(self.typology.name())
    }

    /// The rooms of storey `k` when the typology makes it a periodic storey (a refuge floor):
    /// such a storey holds these rooms and no apartments.
    pub fn periodic_at(&self, k: usize) -> Option<Vec<RoomReq>> {
        self.model.as_ref()?.periodic_rooms(k, self.floors, self.ru, &self.rules, &self.rooms)
    }

    /// Storeys before `k` that hold apartments.
    pub fn flat_floors_before(&self, k: usize) -> u32 {
        (0..k).filter(|j| self.periodic_at(*j).is_none()).count() as u32
    }

    /// A stair and its core only where there is a floor to reach.
    pub fn has_stair(&self) -> bool {
        self.floors > 1
    }

    /// Rooms asked on floor `k` (from 0) outside apartment sections. A house brief without
    /// ground rooms is one list for the whole house: the day rooms and a toilet below, the
    /// bedrooms and baths shared out over the upper floors.
    pub fn floor_rooms(&self, k: usize) -> Vec<RoomReq> {
        let mut v = self.programme_rooms(k);
        v.extend(self.storey_rooms.get(&k).cloned().unwrap_or_default());
        v
    }

    fn programme_rooms(&self, k: usize) -> Vec<RoomReq> {
        if let Some(v) = self.periodic_at(k) {
            return v;
        }
        if !self.ground_rooms.is_empty() {
            return if k == 0 { &self.ground_rooms } else { &self.rooms }.clone();
        }
        if self.typology == Typology::House && self.floors > 1 {
            if let Some(f) = house_split(&self.rooms, self.floors as usize) {
                return f[k.min(f.len() - 1)].clone();
            }
        }
        self.rooms.clone()
    }
}

/// Night rooms round-robin over the upper floors, largest first; `None` when a floor would
/// stay empty, so the brief is repeated on every floor as written.
fn house_split(rooms: &[RoomReq], n: usize) -> Option<Vec<Vec<RoomReq>>> {
    let one = |r: &RoomReq| RoomReq {
        count: 1,
        ..r.clone()
    };
    let mut floors: Vec<Vec<RoomReq>> = vec![vec![]; n];
    let mut night: [Vec<RoomReq>; 2] = Default::default();
    for r in rooms {
        for _ in 0..r.count {
            match r.kind {
                RoomKind::Bedroom => night[0].push(one(r)),
                RoomKind::Bath => night[1].push(one(r)),
                _ => floors[0].push(one(r)),
            }
        }
    }
    for list in &mut night {
        list.sort_by(|a, b| b.area.total_cmp(&a.area));
        for (i, r) in list.drain(..).enumerate() {
            floors[1 + i % (n - 1)].push(r);
        }
    }
    floors.iter().all(|f| !f.is_empty()).then_some(floors)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PRole {
    Public,
    Apartment(u32),
}

/// A room with what the gates need beside its outline: the outline itself is `GRoom` at the
/// same index of the storey layout.
#[derive(Clone, Debug, PartialEq)]
pub struct PRoom {
    pub kind: RoomKind,
    pub number: String,
    pub owner: PRole,
    /// Area the spec asked for; `None` for rooms the builder sized (corridor, hall, core).
    pub asked: Option<f64>,
    /// The asked room took spare band length, so it may be larger than asked.
    pub grown: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PStorey {
    pub name: String,
    /// Above the project base level.
    pub elevation: f64,
    pub height: f64,
    pub layout: GLayout,
    pub rooms: Vec<PRoom>,
    /// Index of the stair room, whose outline the stair must stay within.
    pub stair_room: Option<usize>,
    /// How each furnished room was fitted out, in room order.
    pub fit: Vec<RoomFit>,
    /// Curtain walls standing off the outer wall faces of this storey.
    pub curtains: Vec<GCurtain>,
    /// Guard rails of this storey: stair sides, open void edges, the roof deck on the top one.
    pub railings: Vec<railings::GRailing>,
}

/// One curtain wall: a straight run of glazed and opaque panels between mullions, on the
/// outer face of an exterior wall bay. Lengths in metres, plan points in the storey frame.
#[derive(Clone, Debug, PartialEq)]
pub struct GCurtain {
    pub name: String,
    pub a: (f64, f64),
    pub b: (f64, f64),
    /// Height of the wall above the storey level.
    pub height: f64,
    /// Spacing of the vertical mullions and of the horizontal transoms of the vision rows.
    pub spacing: (f64, f64),
    /// Opaque band above the slab, 0 for none; the lowest row is solid when it is set.
    pub spandrel: f64,
    /// Mullion face width and depth.
    pub mullion: (f64, f64),
    /// Index of the exterior wall this wall stands on.
    pub wall: usize,
}

/// The fit-out of one room: the rule set it took and what the checks found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoomFit {
    /// Index into the storey rooms.
    pub room: usize,
    /// Id of the room rule set (`furnish.rooms[].id`).
    pub rule: String,
    /// The drain stack in the plan, when the room has drained pieces.
    pub stack: Option<(f64, f64)>,
    /// Item id, branch length to the stack along the walls, its drop at the rule fall.
    pub drains: Vec<(String, f64, f64)>,
    /// Floor fall drops of the walk-in showers.
    pub falls: Vec<f64>,
    /// Free turning circles: centre and diameter.
    pub turning: Vec<((f64, f64), f64)>,
    /// Work triangle legs of a kitchen run.
    pub triangle: Option<[f64; 3]>,
    /// What the rule set asked for and did not get, and drains past their limits.
    pub issues: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PRoof {
    pub name: String,
    pub elevation: f64,
    pub ring: Vec<(f64, f64)>,
    /// Degrees on every edge; a flat roof keeps the drainage fall [`FLAT_FALL_DEG`].
    pub slope: f64,
    pub overhang: f64,
    /// Per-edge slopes, degrees; `None` is a vertical gable; empty means `slope` everywhere.
    pub edges: Vec<Option<f64>>,
    /// The roof drawn as a vault instead: the edges above stay its gable outline.
    pub vault: Option<PVault>,
    /// Height of the roof's lowest edge above the roof level, m: a piece of a parted pitched roof
    /// hangs on its own edge.
    pub lift: f64,
    /// A plane piece whose eave line is off its ring (it touches it at a corner): the plane rises
    /// at `slope` to the left of a->b, and every ring edge is a gable.
    pub eave: Option<((f64, f64), (f64, f64))>,
}

/// A roof made by drawing a profile: the profile `(u, z)` stands over the segment `a`→`b`
/// (u from 0 at `a`, z above the roof elevation) and is drawn `depth` to its left.
#[derive(Clone, Debug, PartialEq)]
pub struct PVault {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub profile: Vec<(f64, f64)>,
    pub depth: f64,
}

/// Fall of a flat roof to its edges: 1.5 %, the least that still drains.
pub const FLAT_FALL_DEG: f64 = 0.859;

/// Thickness of the roof build-up of this slope (degrees) under the rules in force, m.
pub fn roof_thickness(slope: f64) -> f64 {
    assembly::roof_thickness(slope)
}

/// Parapet of a flat roof above the deck, m.
pub const PARAPET: f64 = 0.6;

/// What the builder gives: one entry per storey plus the roof, all in metres.
#[derive(Clone, Debug, PartialEq)]
pub struct GProject {
    pub storeys: Vec<PStorey>,
    pub roof: PRoof,
    /// Roofs of further wings and corner links; empty for a single bar.
    pub extra_roofs: Vec<PRoof>,
    /// Copies of the floor program on each storey: one per wing.
    pub copies: u32,
    /// Outline of an open courtyard, a hole through every slab.
    pub court: Option<Vec<(f64, f64)>>,
    /// Outline of the podium below a set-back upper part: the slab of its first storey.
    pub podium: Option<Vec<(f64, f64)>>,
    pub footprint: (f64, f64),
    pub apartments: u32,
    pub metrics: Vec<(&'static str, f64)>,
    pub hvac: GHvac,
    /// Power, lighting, water and drainage; empty unless the brief asks for them.
    pub services: GServices,
    /// Storeys below ground, the first right under the ground floor; negative elevations.
    pub basements: Vec<PStorey>,
    /// Envelope, ground level on the terrain, pad, retaining walls, approaches, earthworks.
    pub site: site::GSite,
    /// Free-form envelope over the plate; `design.shell`.
    pub shell: Option<Box<shell::GShell>>,
    /// Sprinkler system, pressurised stairs of a tall building.
    pub fire: GFire,
    /// Lift cars, shafts and the traffic they were sized for.
    pub lifts: GLifts,
}

/// The lifts of the core as the traffic rule sized them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GLifts {
    /// Cars in the bank (shafts side by side on every storey).
    pub cars: u32,
    /// Cars the traffic rule asked for before the bank limit.
    pub wanted: u32,
    /// Lift zones of the building height, and cars per zone.
    pub zones: u32,
    /// Persons in the busiest period, and the persons the cars carry in it.
    pub demand: f64,
    pub carried: f64,
    /// Average wait for a car in the busiest period, s; 0 when the brief named the count.
    pub interval: f64,
    /// Rated persons of one car.
    pub persons: u32,
    /// Cars that serve the fire brigade: one in a building over `fire.fighting_lift_height`.
    pub fighting: u32,
    /// What the lifts could not do: a cut bank, a car larger than the catalogue, a missing
    /// door or car. Kept apart from the fire issues.
    pub issues: Vec<String>,
}

/// What a run of the sprinkler system is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FireRun {
    Riser,
    Main,
    Branch,
    Drop,
}

/// One pipe run of the sprinkler system with the flow it was sized for, l/min.
#[derive(Clone, Debug, PartialEq)]
pub struct GFireRun {
    pub kind: FireRun,
    pub run: GRun,
    pub flow: f64,
    /// Heads on or below the run.
    pub heads: u32,
}

/// A sprinkler head or pump set joined to the system: storey, fixture index, connector.
#[derive(Clone, Debug, PartialEq)]
pub struct GFireLink {
    pub storey: usize,
    pub fixture: usize,
    pub connector: &'static str,
}

/// A pressure zone of the riser: the storeys it feeds and the pump set at its foot.
#[derive(Clone, Debug, PartialEq)]
pub struct GFireZone {
    pub first: usize,
    pub last: usize,
    /// Storey and fixture index of the pump set.
    pub pump: (usize, usize),
}

/// A part of the floor the head grid could not cover, for the report.
#[derive(Clone, Debug, PartialEq)]
pub struct GFireGap {
    pub storey: usize,
    pub room: usize,
    pub at: (f64, f64),
}

/// The fire protection the builder designed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GFire {
    /// Name of the sprinkler system.
    pub name: String,
    pub runs: Vec<GFireRun>,
    pub links: Vec<GFireLink>,
    pub zones: Vec<GFireZone>,
    /// Riser positions in plan.
    pub risers: Vec<(f64, f64)>,
    pub heads: u32,
    /// Floor area under a head and floor area the rooms hold, m2, over all storeys.
    pub covered: f64,
    pub area: f64,
    pub gaps: Vec<GFireGap>,
    /// Stairs fed with pressurisation air, and the fans placed.
    pub pressurised: bool,
    pub issues: Vec<String>,
    /// Standpipes for the fire service: one per stair core that has room for it.
    pub standpipes: Vec<GStandpipe>,
    /// Parts of a storey beyond the hose reach of every landing valve.
    pub reach_gaps: Vec<GFireGap>,
    /// Heads of the stairs that run on to the roof (`fire.stair_to_roof`).
    pub stair_heads: Vec<GStairHead>,
    /// Why a stair that runs on to the roof got no head, or its roof no opening.
    pub head_issues: Vec<String>,
}

/// The head of a stair that runs on to the roof: a rated enclosure on the roof round the top
/// flight, with the exit door on the side the flight arrives at.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GStairHead {
    /// Index of the stair on the top storey.
    pub stair: usize,
    /// Wall centre lines, counter-clockwise, on the roof level.
    pub ring: Vec<(f64, f64)>,
    /// Height of the walls above the roof level, m.
    pub height: f64,
    /// The exit door: ring edge, start along that edge, clear width and height, m.
    pub door: (usize, f64, f64, f64),
    /// Fire resistance of the walls and the door, minutes.
    pub fire: u32,
}

/// A standpipe in a stair core: an inlet at its foot, a landing valve on every storey, its own
/// pipe network (risers per storey and the valve branches).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GStandpipe {
    pub name: String,
    /// Plan point of the riser axis.
    pub at: (f64, f64),
    pub runs: Vec<GFireRun>,
    /// The inlet and the landing valves.
    pub links: Vec<GFireLink>,
    /// Landing valves: storey and fixture index.
    pub valves: Vec<(usize, usize)>,
}

impl GProject {
    /// Every storey with its slot block: the storeys from 0 up, then the basements.
    pub fn blocks(&self) -> impl Iterator<Item = (u32, &PStorey)> {
        let nb = self.basements.len();
        let up = self.storeys.iter().enumerate().map(move |(k, s)| (slots::storey_block(k, nb), s));
        let down = self.basements.iter().enumerate().map(|(b, s)| (slots::basement_block(b), s));
        up.chain(down)
    }
}

/// The four service systems of a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SvcSys {
    Power,
    Cold,
    Hot,
    Sanitary,
    /// The sprinkler system; not one of the four building services of [`SvcSys::ALL`].
    Fire,
}

impl SvcSys {
    pub const ALL: [SvcSys; 4] = [SvcSys::Power, SvcSys::Cold, SvcSys::Hot, SvcSys::Sanitary];

    pub fn kind(self) -> ddc_geom::mep::SystemKind {
        use ddc_geom::mep::SystemKind as K;
        match self {
            SvcSys::Power => K::Power,
            SvcSys::Cold => K::ColdWater,
            SvcSys::Hot => K::HotWater,
            SvcSys::Sanitary => K::Sanitary,
            SvcSys::Fire => K::Sprinkler,
        }
    }
}

/// One straight or bent run of pipe, conduit or cable tray; every end meets another run, a
/// connector or a cap.
#[derive(Clone, Debug, PartialEq)]
pub struct GRun {
    pub sys: SvcSys,
    /// Storey whose level the run belongs to.
    pub storey: usize,
    /// Axis points, z above the project base, m; drains carry their fall already.
    pub points: Vec<(f64, f64, f64)>,
    pub section: ddc_geom::mep::Section,
    /// Catalogue pipe: material and DN.
    pub pipe: Option<(ddc_geom::mep::PipeMaterial, u32)>,
    /// Elbow radius on the axis; `None` keeps the model default.
    pub bend: Option<f64>,
    pub caps: (bool, bool),
}

/// A family connector joined to a service system.
#[derive(Clone, Debug, PartialEq)]
pub struct GSvcLink {
    pub storey: usize,
    pub fixture: usize,
    pub connector: &'static str,
    pub sys: SvcSys,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircuitUse {
    Lighting,
    Sockets,
    Worktop,
    /// Sockets of a bath or shower room: behind a 30 mA residual current device.
    Wet,
    /// One appliance on its own circuit.
    Appliance,
    /// The cable to a sub-board.
    Feeder,
    /// Emergency luminaires of a room: their own circuit, on the safety supply.
    Emergency,
    /// Sockets of a room on backup power (theatre, ICU): the essential supply.
    Essential,
}

/// A circuit of a board: its devices by connector, the connected load and the cable route
/// length from the board to the farthest device along the trays and conduits; its protection,
/// cable and voltage drop as sized by the generator.
#[derive(Clone, Debug, PartialEq)]
pub struct GCircuit {
    /// The board: storey and fixture index.
    pub board: (usize, usize),
    pub name: String,
    pub use_: CircuitUse,
    pub devices: Vec<(usize, usize, &'static str)>,
    /// W.
    pub load: f64,
    /// m, with the cable left at both ends for termination.
    pub length: f64,
    /// Way on the board, from 1.
    pub number: u32,
    /// Miniature circuit breaker rating, A, characteristic B.
    pub breaker: f64,
    /// 1: single phase at the mains voltage; 3: three phase.
    pub poles: u32,
    /// Conductor cross-section, mm2, and core count of the cable (L N PE, or 3L N PE).
    pub section: f64,
    pub cores: u32,
    /// Residual current device group of the board, from 1; `None`: no RCD.
    pub rcd: Option<u32>,
    /// Points counted against the limit of the circuit: luminaires, or socket outlets.
    pub points: usize,
    /// Design load after diversity, W.
    pub demand: f64,
    /// Voltage drop to the farthest device, %: over this circuit, and from the main board.
    pub drop: f64,
    pub drop_total: f64,
}

impl GCircuit {
    /// Cable designation, as NYM-J 3x1.5.
    pub fn cable(&self) -> String {
        circuits::cable_name(self.cores, self.section)
    }
    /// Protective device, as B16 or 3xB16.
    pub fn protection(&self) -> String {
        circuits::breaker_name(self.poles, self.breaker)
    }
}

/// A residual current device on a board guarding a group of its circuits.
#[derive(Clone, Debug, PartialEq)]
pub struct GRcd {
    pub number: u32,
    /// 2 or 4.
    pub poles: u32,
    /// Rated residual current, mA.
    pub ma: f64,
    /// Rated current, A.
    pub rating: f64,
}

impl GRcd {
    /// As RCD1 4P 40A 30mA A (type A).
    pub fn label(&self) -> String {
        circuits::rcd_name(self)
    }
}

/// A distribution board: its label, the board feeding it and its residual current devices.
#[derive(Clone, Debug, PartialEq)]
pub struct GBoard {
    pub at: (usize, usize),
    pub label: String,
    pub parent: Option<(usize, usize)>,
    pub rcds: Vec<GRcd>,
}

/// A circuit line on a storey plan: wiring between two devices of a circuit, or the home run
/// from its first device towards the board, ending in an arrow with the circuit label.
#[derive(Clone, Debug, PartialEq)]
pub struct GWire {
    pub storey: usize,
    /// Index in `GServices::circuits`.
    pub circuit: usize,
    /// Plan points, m.
    pub points: Vec<(f64, f64)>,
    pub home: bool,
    /// Conductor ticks across the line: on home runs.
    pub ticks: u32,
    /// Board and circuit, as DB1 / F3: on home runs.
    pub label: String,
}

/// A cable in 3D along the carriers from a board to the first device of a circuit.
#[derive(Clone, Debug, PartialEq)]
pub struct GTrace {
    pub circuit: usize,
    pub storey: usize,
    /// Axis points, z above the project base, m.
    pub points: Vec<(f64, f64, f64)>,
}

/// Electrical and water services: runs by system, connector links and circuits.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GServices {
    pub elec: Vec<GRun>,
    pub water: Vec<GRun>,
    pub links: Vec<GSvcLink>,
    pub circuits: Vec<GCircuit>,
    pub boards: Vec<GBoard>,
    /// Circuit lines on the plans, when the brief asks for them.
    pub wires: Vec<GWire>,
    /// Cable traces in 3D, when the brief asks for them.
    pub traces: Vec<GTrace>,
    /// What the design left out and why: a device unpowered, a row unlit, a room unserved.
    pub issues: Vec<String>,
    /// System names in `SvcSys::ALL` order (power, cold, hot, sanitary), with services on or off.
    pub names: Vec<String>,
    /// Use class the supply pipes are sized for at peak flow; empty sizes them for the sum.
    pub water_use: String,
}

/// The two air systems of a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AirSys {
    Supply,
    Extract,
}

/// One straight or bent duct run; every end meets another run's end, a terminal connector or
/// a unit connector.
#[derive(Clone, Debug, PartialEq)]
pub struct GDuct {
    pub sys: AirSys,
    /// Storey whose level the run belongs to; `storeys.len()` for the roof.
    pub storey: usize,
    /// Axis points, z above the project base, m.
    pub points: Vec<(f64, f64, f64)>,
    /// Width and height of a rectangular duct; (d, 0) for a round one.
    pub size: (f64, f64),
    /// Air carried, m3/h.
    pub flow: f64,
    /// Elbow radius on the axis; `None` keeps the duct default.
    pub bend: Option<f64>,
    /// The ventilation system of the run: 0 the general one, 1 the dedicated outdoor air one.
    pub net: u8,
}

/// A family connector joined to an air system: `storey` is `storeys.len()` for roof plant.
#[derive(Clone, Debug, PartialEq)]
pub struct GAirLink {
    pub storey: usize,
    pub fixture: usize,
    pub connector: &'static str,
    pub sys: AirSys,
    /// The ventilation system of the connector, as `GDuct::net`.
    pub net: u8,
}

/// Mechanical ventilation: plant on the roof level, ducts, and the connector links.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GHvac {
    /// Air handling units at the roof level, z above that level.
    pub plant: Vec<crate::GFixture>,
    pub ducts: Vec<GDuct>,
    pub links: Vec<GAirLink>,
    /// System names: supply, extract.
    pub names: (String, String),
    /// Names of the dedicated systems that follow the general one: supply, extract.
    pub nets: Vec<(String, String)>,
    /// Design airflow, m3/h: supply, extract.
    pub flow: (f64, f64),
}

/// Appliances, water and power for the project in place, as the spec asks.
fn services(spec: &ProjectSpec, p: &mut GProject) {
    // named before the opt-out: consumers index the names by `SvcSys::ALL` even with no service on
    p.services.names = ["power", "cold", "hot", "sanitary"]
        .iter()
        .map(|k| svc::text(&["systems", k], spec.ru))
        .collect();
    let sv = spec.services;
    if !spec.mep_auto || !(sv.electrical || sv.water || sv.appliances) {
        shaft_note(p);
        return;
    }
    if sv.appliances {
        fitout::appliances(spec, p);
    }
    if sv.water {
        water::plumb(spec, p);
    }
    if sv.electrical {
        elec::wire(spec, p);
    }
    shaft_note(p);
}

/// Says when the core could not hold every service shaft the typology asks for.
fn shaft_note(p: &mut GProject) {
    let Some(n) = p.metrics.iter().find(|m| m.0 == "core_shafts_cut").map(|m| m.1 as usize) else { return };
    let what = match n {
        0 => "the systems keep their own shafts".to_string(),
        n => format!("the service shafts are merged into {n}"),
    };
    p.services.issues.push(format!("info: the core is too narrow for the service shafts the typology asks for; {what}"));
}

/// Fire protection of the finished building: sprinklers, stair pressurisation, lift cars.
fn protect(spec: &ProjectSpec, p: &mut GProject) {
    sprinkler::protect(spec, p);
    if spec.mep_auto {
        standpipe::standpipes(spec, p);
    }
    fire::stair_heads(spec, p);
}

/// Fixed slot blocks keep an element's reference when the spec changes elsewhere.
pub mod slots {
    pub const PER_STOREY: u32 = 1024;
    pub const WALLS: u32 = 0;
    pub const OPENINGS: u32 = 320;
    pub const ROOMS: u32 = 640;
    pub const SLAB: u32 = 880;
    pub const STAIRS: u32 = 881;
    pub const SHAFTS: u32 = 884;
    pub const GRIDS: u32 = 900;
    pub const COLUMNS: u32 = 960;
    /// Storey blocks in the original layout: the blocks below keep their slots for ever, so a
    /// stored project re-syncs to the same references.
    pub const LOW_STOREYS: u32 = 40;
    /// Storeys the slot space holds: the id space of a tall tower; the height a project may
    /// reach is the rule `massing.height_max`, never this count. Blocks from `LOW_STOREYS`
    /// up live in the tall blocks at the end of the space.
    pub const MAX_STOREYS: u32 = 200;
    /// Furniture and fixtures: a block per storey after the storeys and the roof.
    pub const FIXTURES: u32 = (LOW_STOREYS + 1) * PER_STOREY;
    pub const FIXTURES_PER_STOREY: u32 = 2048;
    /// Fixtures of one storey: the slots of a storey's block that are used.
    pub const FIXTURES_MAX: u32 = FIXTURES_PER_STOREY;
    /// Air handling units on the roof level, after the fixture blocks.
    pub const PLANT: u32 = FIXTURES + LOW_STOREYS * FIXTURES_PER_STOREY;
    pub const PLANT_MAX: u32 = 64;
    /// Air ducts of the whole project.
    pub const DUCTS: u32 = PLANT + PLANT_MAX;
    pub const DUCTS_MAX: u32 = 16384;
    /// Pipes of the water systems, then conduits and trays, then circuits: each its own
    /// block, so switching one off keeps the others' references.
    pub const WATER: u32 = DUCTS + DUCTS_MAX;
    pub const WATER_MAX: u32 = 16384;
    pub const ELEC: u32 = WATER + WATER_MAX;
    pub const ELEC_MAX: u32 = 16384;
    pub const CIRCUITS: u32 = ELEC + ELEC_MAX;
    pub const CIRCUITS_MAX: u32 = 4096;
    // The blocks after the circuits are chained, each `X = PREV + PREV_MAX`: a change that adds
    // one inserts it before the next and keeps the earlier ones where they are.
    /// Pipes of the sprinkler system: risers, mains, branches and drops.
    pub const SPRINKLER: u32 = CIRCUITS + CIRCUITS_MAX;
    pub const SPRINKLER_MAX: u32 = 32768;
    /// Standpipe pipes: the tail of the sprinkler block, at a fixed offset, so a change in
    /// the sprinkler count keeps their slots.
    pub const STANDPIPE_MAX: u32 = 4096;
    pub const SPRINKLER_RUNS: u32 = SPRINKLER_MAX - STANDPIPE_MAX;
    pub const STANDPIPE: u32 = SPRINKLER + SPRINKLER_RUNS;
    /// Storeys from `LOW_STOREYS` up: per block the storey's elements, its fixtures, then its
    /// split slots.
    pub const TALL: u32 = SPRINKLER + SPRINKLER_MAX;
    pub const TALL_BLOCKS: u32 = MAX_STOREYS - LOW_STOREYS;
    pub const TALL_FIXTURES: u32 = TALL + TALL_BLOCKS * PER_STOREY;
    pub const TALL_SPLIT: u32 = TALL_FIXTURES + TALL_BLOCKS * FIXTURES_PER_STOREY;
    pub const TALL_MAX: u32 = TALL_BLOCKS * (PER_STOREY + FIXTURES_PER_STOREY + SPLIT_PER_STOREY);
    /// Curtain walls: a run of `CURTAINS_MAX` slots per storey block.
    pub const CURTAINS: u32 = TALL + TALL_MAX;
    pub const CURTAINS_MAX: u32 = 256;
    pub const CURTAINS_ALL: u32 = MAX_STOREYS * CURTAINS_MAX;
    /// Units of the mechanical design past the air handling unit, then its water pipes.
    pub const MECH: u32 = CURTAINS + CURTAINS_ALL;
    pub const MECH_MAX: u32 = 4096;
    pub const MECH_PIPES: u32 = MECH + MECH_MAX;
    pub const MECH_PIPES_MAX: u32 = 16384;
    /// Data centre units (racks, cooling units, plant), then its pipes and trays.
    pub const DC_UNITS: u32 = MECH_PIPES + MECH_PIPES_MAX;
    pub const DC_UNITS_MAX: u32 = 8192;
    pub const DC_RUNS: u32 = DC_UNITS + DC_UNITS_MAX;
    pub const DC_RUNS_MAX: u32 = 16384;
    /// Surface patches, panels, ribs and dome of the free-form shell. Chain on merge: CIRCUITS,
    /// MECH, DC_UNITS/DC_RUNS, FIRE, then this block last; each starts where the one before ends.
    pub const SHELL: u32 = DC_RUNS + DC_RUNS_MAX;
    pub const SHELL_MAX: u32 = 1024;
    /// Columns past the storey block's own: a block per storey block after the circuits, so
    /// the first columns keep their references when a storey gets more.
    pub const COLUMNS_MORE: u32 = SHELL + SHELL_MAX;
    pub const COLUMNS_MORE_PER_STOREY: u32 = 192;
    /// Columns one storey holds.
    pub const MAX_COLUMNS: u32 = PER_STOREY - COLUMNS + COLUMNS_MORE_PER_STOREY;
    /// Guard rails: a run of `RAILINGS_MAX` slots per storey block, after the column overflow.
    pub const RAILINGS: u32 = COLUMNS_MORE + MAX_STOREYS * COLUMNS_MORE_PER_STOREY;
    pub const RAILINGS_MAX: u32 = 64;
    pub const RAILINGS_ALL: u32 = MAX_STOREYS * RAILINGS_MAX;
    /// Glulam beams of a timber frame, after the guard rails.
    pub const TIMBER: u32 = RAILINGS + RAILINGS_ALL;
    pub const TIMBER_MAX: u32 = 8192;
    /// CLT floor panels: the first of a storey is its slab slot, the others take these.
    pub const TIMBER_PANELS: u32 = TIMBER + TIMBER_MAX;
    pub const TIMBER_PANELS_MAX: u32 = 4096;
    /// Storeys, the roof block, the fixture blocks, the plant, the ducts, the services, the
    /// mechanical design, the data centre, the shell, the columns past a storey's block, the guard rails, then the timber frame.
    pub const TOTAL: u32 = TIMBER_PANELS + TIMBER_PANELS_MAX;

    /// Slot of column `i` of storey block `k`.
    pub fn column(k: u32, i: u32) -> u32 {
        let own = PER_STOREY - COLUMNS;
        if i < own {
            block_base(k) + COLUMNS + i
        } else {
            COLUMNS_MORE + k * COLUMNS_MORE_PER_STOREY + (i - own)
        }
    }
    /// Split levels, in the roof block past the roofs: per storey block the slab's other
    /// faces and its half-level flights, then the roofs' other pieces.
    pub const SPLIT: u32 = LOW_STOREYS * PER_STOREY + 512;
    pub const SPLIT_PER_STOREY: u32 = 8;
    pub const SPLIT_ROOF: u32 = SPLIT + LOW_STOREYS * SPLIT_PER_STOREY;

    /// First slot of storey block `k`.
    pub fn block_base(k: u32) -> u32 {
        if k < LOW_STOREYS {
            k * PER_STOREY
        } else {
            TALL + (k - LOW_STOREYS) * PER_STOREY
        }
    }

    /// First fixture slot of storey block `k`.
    pub fn fixture_base(k: u32) -> u32 {
        if k < LOW_STOREYS {
            FIXTURES + k * FIXTURES_PER_STOREY
        } else {
            TALL_FIXTURES + (k - LOW_STOREYS) * FIXTURES_PER_STOREY
        }
    }

    /// First curtain wall slot of storey block `k`.
    pub fn curtain_base(k: u32) -> u32 {
        CURTAINS + k * CURTAINS_MAX
    }

    /// First guard rail slot of storey block `k`.
    pub fn railing_base(k: u32) -> u32 {
        RAILINGS + k * RAILINGS_MAX
    }

    /// The storey block and the offset in it of a slot of a storey's own elements.
    pub fn locate(rel: u32) -> Option<(u32, u32)> {
        if rel < LOW_STOREYS * PER_STOREY {
            Some((rel / PER_STOREY, rel % PER_STOREY))
        } else if (TALL..TALL_FIXTURES).contains(&rel) {
            let r = rel - TALL;
            Some((LOW_STOREYS + r / PER_STOREY, r % PER_STOREY))
        } else {
            None
        }
    }

    /// First split slot of storey block `k`: the slab's other faces, then the flights.
    pub fn split_block(k: u32) -> u32 {
        if k < LOW_STOREYS {
            SPLIT + k * SPLIT_PER_STOREY
        } else {
            TALL_SPLIT + (k - LOW_STOREYS) * SPLIT_PER_STOREY
        }
    }

    /// The roof block: the slot of the main roof, then the wings' roofs and the split pieces.
    pub const ROOF: u32 = LOW_STOREYS * PER_STOREY;
    /// Stair heads on the roof, in the roof block before the split pieces: per head its four
    /// walls, the exit door, then its cover.
    pub const HEADS: u32 = ROOF + 384;
    pub const HEAD_SLOTS: u32 = 8;
    pub const HEADS_MAX: u32 = 128;

    /// Storey block of basement `b` (0 right below the ground floor): blocks from the top of
    /// the original layout down, whatever the storey count, so adding a storey on top never
    /// moves a basement. Basements number fewer than `LOW_STOREYS` (see `basement::add`).
    pub fn basement_block(b: usize) -> u32 {
        LOW_STOREYS - 1 - b as u32
    }

    /// Storey block of storey `k` (0 the ground floor) over `basements` basements: its own
    /// block, but a low block a basement holds goes to the top tall block `MAX_STOREYS - 1 - b`
    /// of that basement `b`; no block depends on the storey count, so adding a storey keeps
    /// every reference of the floors and basements already there.
    pub fn storey_block(k: usize, basements: usize) -> u32 {
        let k = k as u32;
        if (LOW_STOREYS.saturating_sub(basements as u32)..LOW_STOREYS).contains(&k) {
            MAX_STOREYS - LOW_STOREYS + k
        } else {
            k
        }
    }

    /// What holds a storey block: the inverse of [`storey_block`] and [`basement_block`].
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Floor {
        Storey(usize),
        Basement(usize),
    }

    /// The storey or basement of block `k` in a project with `basements` basements.
    pub fn floor_of(k: u32, basements: usize) -> Option<Floor> {
        let held = LOW_STOREYS.saturating_sub(basements as u32);
        match k {
            _ if k < held => Some(Floor::Storey(k as usize)),
            _ if k < LOW_STOREYS => Some(Floor::Basement((LOW_STOREYS - 1 - k) as usize)),
            _ if k >= MAX_STOREYS => None,
            _ if k + LOW_STOREYS >= MAX_STOREYS + held => Some(Floor::Storey((k + LOW_STOREYS - MAX_STOREYS) as usize)),
            _ => Some(Floor::Storey(k as usize)),
        }
    }
}
