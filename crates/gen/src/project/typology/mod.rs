//! A typology as data (`gen_typology.v1`): which spaces a kind of building has and how many,
//! what each needs (air changes, cooling, power, water, clear height, a structural load), how
//! the zones of the building sit against each other, what the exits must carry, whether and how
//! it goes below ground, and which plant it hands to the plant module. The builder reads the
//! model through a preset: the spaces above ground become the programme, the levels below
//! ground become the basement plan, the demands feed the ventilation and the schedules, and
//! every decision is explained by [`why`]. A model lies in a preset file (`data/typologies/`),
//! in a project property or in the user library, so a user opens, changes and saves it.

use super::program::{Access, Floor, Light, Program, Space};
use super::rules::{self, RuleSet};
use super::RoomKind;
use serde_json::{json, Map, Value as J};

pub mod below;
pub mod check;
pub mod why;

pub const SCHEMA: &str = "gen_typology.v1";
/// Most space lines a model may hold.
pub const MAX_SPACES: usize = 120;
/// Most zones of a model.
pub const MAX_ZONES: usize = 24;

/// A number the model states: a literal, or the value of a rule, so a country pack, a preset,
/// the project or the brief steers it.
#[derive(Clone, Debug, PartialEq)]
pub enum Num {
    Lit(f64),
    Rule(String),
}

impl Num {
    fn parse(v: &J, at: &str, errs: &mut Vec<String>) -> Option<Num> {
        let n = match v {
            J::Number(n) => n.as_f64().filter(|x| x.is_finite() && *x >= 0.0).map(Num::Lit),
            J::Object(o) => o
                .get("rule")
                .and_then(J::as_str)
                .filter(|id| rules::def(id).is_some_and(|d| d.default.is_some()))
                .map(|id| Num::Rule(id.to_string())),
            _ => None,
        };
        if n.is_none() {
            errs.push(format!("{at}: a number >= 0, or {{rule: <id of a rule with a default>}}"));
        }
        n
    }

    fn opt(v: &J, at: &str, errs: &mut Vec<String>) -> Option<Num> {
        if v.is_null() {
            None
        } else {
            Num::parse(v, at, errs)
        }
    }

    /// The value in force under `rs`.
    pub fn get(&self, rs: &RuleSet) -> f64 {
        match self {
            Num::Lit(x) => *x,
            Num::Rule(id) => rs.get(id),
        }
    }

    pub fn to_json(&self) -> J {
        match self {
            Num::Lit(x) => json!(x),
            Num::Rule(id) => json!({ "rule": id }),
        }
    }
}

/// How the air of a room sits against its surroundings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pressure {
    Neutral,
    /// More supply than extract: clean rooms keep the dirty air out.
    Positive,
    /// More extract than supply: contaminated rooms keep their air in.
    Negative,
}

impl Pressure {
    const ALL: [(&'static str, Pressure); 3] =
        [("neutral", Pressure::Neutral), ("positive", Pressure::Positive), ("negative", Pressure::Negative)];

    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|x| x.1 == self).map_or("neutral", |x| x.0)
    }
}

/// What a space needs from the services.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Demand {
    /// Air changes per hour of the room volume.
    pub ach: Option<Num>,
    /// Supply per floor area, m3/h per m2; the larger of this and `ach` rules.
    pub supply_m3h_m2: Option<Num>,
    pub pressure: Option<Pressure>,
    /// Heat to remove, W per m2 of floor.
    pub cooling_w_m2: Option<Num>,
    /// Electrical load, W per m2 of floor.
    pub power_w_m2: Option<Num>,
    /// The load stays on when the grid fails (generator, UPS).
    pub backup_power: bool,
    /// Water drawn per room, litres a day.
    pub water_l_day: Option<Num>,
    /// Piped media the room needs (oxygen, vacuum, compressed air, chilled water...).
    pub media: Vec<String>,
}

impl Demand {
    fn parse(v: &J, at: &str, errs: &mut Vec<String>) -> Demand {
        let mut d = Demand::default();
        let Some(o) = v.as_object() else {
            if !v.is_null() {
                errs.push(format!("{at}: an object"));
            }
            return d;
        };
        let known = ["ach", "supply_m3h_m2", "pressure", "cooling_w_m2", "power_w_m2", "backup_power", "water_l_day", "media"];
        for k in o.keys().filter(|k| !known.contains(&k.as_str())) {
            errs.push(format!("{at}.{k}: unknown field; use {}", known.join(", ")));
        }
        d.ach = Num::opt(&v["ach"], &format!("{at}.ach"), errs);
        d.supply_m3h_m2 = Num::opt(&v["supply_m3h_m2"], &format!("{at}.supply_m3h_m2"), errs);
        d.cooling_w_m2 = Num::opt(&v["cooling_w_m2"], &format!("{at}.cooling_w_m2"), errs);
        d.power_w_m2 = Num::opt(&v["power_w_m2"], &format!("{at}.power_w_m2"), errs);
        d.water_l_day = Num::opt(&v["water_l_day"], &format!("{at}.water_l_day"), errs);
        d.pressure = match v["pressure"].as_str() {
            None if v["pressure"].is_null() => None,
            Some(s) => {
                let p = Pressure::ALL.iter().find(|x| x.0 == s).map(|x| x.1);
                if p.is_none() {
                    errs.push(format!("{at}.pressure: neutral, positive or negative"));
                }
                p
            }
            None => {
                errs.push(format!("{at}.pressure: neutral, positive or negative"));
                None
            }
        };
        d.backup_power = match &v["backup_power"] {
            J::Null => false,
            J::Bool(b) => *b,
            _ => {
                errs.push(format!("{at}.backup_power: true or false"));
                false
            }
        };
        if let Some(a) = v["media"].as_array() {
            d.media = a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
        } else if !v["media"].is_null() {
            errs.push(format!("{at}.media: a list of names"));
        }
        d
    }

    fn to_json(&self) -> J {
        let mut o = Map::new();
        for (k, n) in [
            ("ach", &self.ach),
            ("supply_m3h_m2", &self.supply_m3h_m2),
            ("cooling_w_m2", &self.cooling_w_m2),
            ("power_w_m2", &self.power_w_m2),
            ("water_l_day", &self.water_l_day),
        ] {
            if let Some(n) = n {
                o.insert(k.into(), n.to_json());
            }
        }
        if let Some(p) = self.pressure {
            o.insert("pressure".into(), json!(p.name()));
        }
        if self.backup_power {
            o.insert("backup_power".into(), json!(true));
        }
        if !self.media.is_empty() {
            o.insert("media".into(), json!(self.media));
        }
        J::Object(o)
    }

    pub fn is_empty(&self) -> bool {
        *self == Demand::default()
    }
}

/// Where a space lies in the building.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Up(Floor),
    /// Storey below ground, 1 right under the ground floor.
    Below(u32),
}

impl Level {
    fn parse(v: &J) -> Option<Level> {
        match v {
            J::String(s) if s.starts_with("below:") => {
                s["below:".len()..].parse::<u32>().ok().filter(|n| *n >= 1).map(Level::Below)
            }
            J::Number(n) if n.as_i64().is_some_and(|x| x < 0) => {
                n.as_i64().and_then(|x| u32::try_from(-x).ok()).map(Level::Below)
            }
            x => Floor::parse(x).map(Level::Up),
        }
    }

    fn to_json(self) -> J {
        match self {
            Level::Up(f) => f.to_json(),
            Level::Below(n) => json!(format!("below:{n}")),
        }
    }

    /// How many storeys of a building with `floors` above and `basements` below the space
    /// repeats on.
    pub fn storeys(self, floors: u32, basements: u32) -> u32 {
        match self {
            Level::Up(Floor::Ground) | Level::Up(Floor::Level(0)) => 1,
            Level::Up(Floor::Typical) | Level::Up(Floor::Any) => floors.saturating_sub(1),
            Level::Up(Floor::Single) => u32::from(floors == 1),
            Level::Up(Floor::Level(n)) => u32::from(n < floors),
            Level::Below(n) => u32::from(n <= basements),
        }
    }
}

/// One line of a typology: a class of space with its needs.
#[derive(Clone, Debug, PartialEq)]
pub struct TSpace {
    /// Handle of the class: schedules, adjacency and the fill of a level name it.
    pub class: String,
    /// The kind the plan builder lays the room out as.
    pub kind: RoomKind,
    pub name: (String, String),
    /// Net area range of one room, m2.
    pub area: (f64, f64),
    pub count: u32,
    pub level: Level,
    pub light: Light,
    pub height_min: Option<f64>,
    pub access: Access,
    /// Classes or kinds wanted beside it, with the weight of the wish.
    pub adjacent_to: Vec<(String, f64)>,
    pub zone: Option<String>,
    pub demand: Demand,
    /// Floor area per person, m2, for the exit width.
    pub occupancy_m2: Option<Num>,
    /// Imposed floor load, kPa.
    pub live_load_kpa: Option<Num>,
    /// Room tags (`operating`, `isolation`, `clinical`, ...) the services read the class from.
    pub tags: super::RoomTags,
    /// The count follows this rule when the project is read (a unit mix); `count` is its default.
    pub count_num: Option<Num>,
    /// The space belongs to the periodic storeys of this id (a refuge floor), not to every one.
    pub periodic: Option<String>,
    /// Beds one room holds (a ward, an intensive care room); the schedule counts them.
    pub beds: u32,
}

/// A kind of apartment: its rooms, and how many of it a storey holds (a rule gives the mix).
#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    pub name: (String, String),
    pub count: Num,
    pub rooms: Vec<UnitRoom>,
}

/// A room of an apartment: living room, bedrooms by type, kitchen, bath, hall.
#[derive(Clone, Debug, PartialEq)]
pub struct UnitRoom {
    pub kind: RoomKind,
    pub area: Num,
    pub count: u32,
    pub name: Option<(String, String)>,
}

/// Storeys that repeat at an interval: the rooms of their spaces go on every `every`-th storey.
#[derive(Clone, Debug, PartialEq)]
pub struct Periodic {
    pub id: String,
    pub every: Num,
    /// Storeys (0 is the ground floor) that are this periodic storey whatever `every` says: a
    /// podium floor of a hotel.
    pub at: Vec<usize>,
    /// The storey holds only these spaces, not the typical programme.
    pub replaces: bool,
}

impl TSpace {
    /// The count: the rule's value under `rs` when the space follows one.
    pub fn count_in(&self, rs: Option<&RuleSet>) -> u32 {
        match (&self.count_num, rs) {
            (Some(n), Some(rs)) => n.get(rs).round().clamp(0.0, f64::from(super::limits::MAX_ROOM_COUNT)) as u32,
            _ => self.count,
        }
    }

    /// The room the plan builder is asked for on a periodic storey.
    pub fn req(&self, ru: bool, rs: &RuleSet) -> super::RoomReq {
        super::RoomReq {
            name: self.display(ru).to_string(),
            kind: self.kind,
            area: self.target_area(),
            count: self.count_in(Some(rs)),
            tags: self.tags,
        }
    }

    pub fn target_area(&self) -> f64 {
        (self.area.0 + self.area.1) / 2.0
    }

    pub fn display(&self, ru: bool) -> &str {
        if ru {
            &self.name.1
        } else {
            &self.name.0
        }
    }
}

/// A part of the building that wants to sit with other parts (the surgical suite beside the
/// intensive care) and may be its own fire compartment.
#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    pub id: String,
    pub name: (String, String),
    /// Zones this one wants beside it, with weights.
    pub adjacent_to: Vec<(String, f64)>,
}

/// What the exits of the building must carry.
#[derive(Clone, Debug, PartialEq)]
pub struct Egress {
    /// Fewest exits from the ground floor and fewest stairs from a storey.
    pub exits_min: u32,
}

/// The structural asks of the typology as a whole.
#[derive(Clone, Debug, PartialEq)]
pub struct Structure {
    /// Imposed load of a storey no space names its own for, kPa.
    pub live_load_kpa: Option<Num>,
}

/// One storey below ground.
#[derive(Clone, Debug, PartialEq)]
pub struct BelowLevel {
    pub name: (String, String),
    /// Class of the space that takes what the named spaces leave of the level.
    pub fill: String,
    /// Name the through corridor of the level takes (the trackway of a station).
    pub corridor: Option<(String, String)>,
}

/// Levels below ground; the first is right under the ground floor.
#[derive(Clone, Debug, PartialEq)]
pub struct Below {
    pub levels: Vec<BelowLevel>,
    /// The outside walls hold back the soil: they are retaining walls.
    pub retaining: bool,
    /// Storey height of the levels below ground, m; the building's storey height when absent
    /// (a station platform lies deeper than the floors above it).
    pub height: Option<f64>,
}

/// A plant the typology hands to the plant module: what it must deliver, from the loads of the
/// spaces or as a fixed number.
#[derive(Clone, Debug, PartialEq)]
pub struct PlantLine {
    /// `boiler_house`, `pumping_station`, `water_treatment` or `process_line`.
    pub kind: String,
    pub duty_kw: Option<Qty>,
    pub flow_m3h: Option<Qty>,
    pub head_m: Option<f64>,
    pub units: Option<u32>,
}

/// A quantity of a plant: a share of a load the schedule computes, or a fixed value.
#[derive(Clone, Debug, PartialEq)]
pub enum Qty {
    /// `from` is one of [`QTY_SOURCES`]; `cooling_water_m3h` is the water flow that carries
    /// the cooling load at the rule `typology.plant.delta_t`.
    Of { from: String, share: f64 },
    Fixed(f64),
}

pub const QTY_SOURCES: [&str; 6] = ["cooling_kw", "power_kw", "backup_kw", "air_m3h", "water_m3h", "cooling_water_m3h"];

impl Qty {
    fn parse(v: &J, at: &str, errs: &mut Vec<String>) -> Option<Qty> {
        match v {
            J::Null => None,
            J::Number(n) => n.as_f64().filter(|x| *x > 0.0 && x.is_finite()).map(Qty::Fixed).or_else(|| {
                errs.push(format!("{at}: a positive number"));
                None
            }),
            J::Object(_) => {
                let from = v["from"].as_str().unwrap_or_default();
                let share = v["share"].as_f64().unwrap_or(1.0);
                if !QTY_SOURCES.contains(&from) || !(share > 0.0 && share <= 10.0) {
                    errs.push(format!("{at}: {{from: one of {}, share: 0..10}}", QTY_SOURCES.join(", ")));
                    return None;
                }
                Some(Qty::Of { from: from.to_string(), share })
            }
            _ => {
                errs.push(format!("{at}: a number or {{from, share}}"));
                None
            }
        }
    }

    fn to_json(&self) -> J {
        match self {
            Qty::Fixed(x) => json!(x),
            Qty::Of { from, share } => json!({ "from": from, "share": share }),
        }
    }
}

/// The typology model.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    /// The preset the model came with; empty for a model read alone.
    pub id: String,
    pub title: (String, String),
    pub spaces: Vec<TSpace>,
    pub zones: Vec<Zone>,
    pub egress: Egress,
    pub structure: Structure,
    pub below: Option<Below>,
    pub plant: Vec<PlantLine>,
    /// What a stretch of band no space fills becomes; `None` keeps the base typology's.
    pub spare: Option<super::programme::Spare>,
    pub periodic: Vec<Periodic>,
    /// The apartments of an apartment section, with their mix; empty for other typologies.
    pub units: Vec<Unit>,
}

fn text(v: &J, at: &str, errs: &mut Vec<String>) -> Option<(String, String)> {
    match v {
        J::String(s) if !s.trim().is_empty() => Some((s.clone(), s.clone())),
        J::Object(_) => {
            let en = v["en"].as_str().filter(|s| !s.trim().is_empty());
            en.map(|en| (en.to_string(), v["ru"].as_str().unwrap_or(en).to_string()))
        }
        _ => None,
    }
    .or_else(|| {
        errs.push(format!("{at}: a name, or {{en, ru}}"));
        None
    })
}

fn name_json(n: &(String, String)) -> J {
    json!({ "en": n.0, "ru": n.1 })
}

fn weights(v: &J, at: &str, errs: &mut Vec<String>) -> Vec<(String, f64)> {
    let mut out = vec![];
    match v {
        J::Null => {}
        J::Array(a) => {
            for (j, x) in a.iter().enumerate() {
                let w = match x {
                    J::String(s) => Some((s.clone(), 1.0)),
                    J::Object(m) => m
                        .get("ref")
                        .and_then(J::as_str)
                        .map(|s| (s.to_string(), m.get("weight").and_then(J::as_f64).unwrap_or(1.0))),
                    _ => None,
                };
                match w {
                    Some((s, w)) if w.is_finite() && (0.0..=rules::MAX_WEIGHT).contains(&w) => out.push((s, w)),
                    _ => errs.push(format!("{at}[{j}]: a class, or {{ref, weight}}")),
                }
            }
        }
        _ => errs.push(format!("{at}: a list")),
    }
    out
}

fn weights_json(w: &[(String, f64)]) -> J {
    J::Array(w.iter().map(|(r, x)| json!({ "ref": r, "weight": x })).collect())
}

fn parse_space(i: usize, r: &J, errs: &mut Vec<String>) -> Option<TSpace> {
    let at = format!("spaces[{i}]");
    let Some(o) = r.as_object() else {
        errs.push(format!("{at}: an object"));
        return None;
    };
    let known = [
        "class", "kind", "name", "area", "count", "level", "light", "height_min", "access", "adjacent_to", "zone", "demand",
        "occupancy_m2", "live_load_kpa", "tags", "periodic", "beds",
    ];
    for k in o.keys().filter(|k| !known.contains(&k.as_str())) {
        errs.push(format!("{at}.{k}: unknown field; use {}", known.join(", ")));
    }
    let mut tags = super::RoomTags::default();
    match &r["tags"] {
        J::Null => {}
        J::Array(a) => {
            for t in a {
                match t.as_str().and_then(super::RoomTags::parse) {
                    Some(x) => tags = tags.with(x),
                    None => errs.push(format!("{at}.tags: one of {}", super::RoomTags::IDS.join(", "))),
                }
            }
        }
        _ => errs.push(format!("{at}.tags: a list")),
    }
    let n0 = errs.len();
    let class = r["class"].as_str().filter(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'));
    if class.is_none() {
        errs.push(format!("{at}.class: lowercase letters, digits and _"));
    }
    let k = r["kind"].as_str().unwrap_or_default();
    let kind = RoomKind::parse(k).filter(|k| *k != RoomKind::Hall);
    if kind.is_none() {
        errs.push(format!("{at}.kind: the layout kind, one of {}", RoomKind::ASKED.join(", ")));
    }
    let name = text(&r["name"], &format!("{at}.name"), errs);
    let lim = *super::limits::ROOM_AREA;
    let area = match &r["area"] {
        J::Number(n) => n.as_f64().map(|a| (a, a)),
        J::Array(a) if a.len() == 2 => a[0].as_f64().zip(a[1].as_f64()),
        J::Object(m) => m.get("min").and_then(J::as_f64).zip(m.get("max").and_then(J::as_f64)),
        _ => None,
    }
    .filter(|a| a.0 <= a.1 && lim.contains(a.0) && lim.contains(a.1));
    if area.is_none() {
        errs.push(format!("{at}.area: m2 in {}, a number or {{min, max}}", lim.range()));
    }
    let mut count_num = None;
    let count = match &r["count"] {
        J::Null => Some(1),
        J::Object(_) => Num::parse(&r["count"], &format!("{at}.count"), errs).map(|n| {
            let d = match &n {
                Num::Rule(id) => rules::def(id).and_then(|d| d.default).unwrap_or(0.0),
                Num::Lit(x) => *x,
            };
            count_num = Some(n);
            d.round().clamp(0.0, f64::from(super::limits::MAX_ROOM_COUNT)) as u32
        }),
        c => c.as_u64().filter(|c| *c <= u64::from(super::limits::MAX_ROOM_COUNT)).map(|c| c as u32),
    };
    if count.is_none() {
        errs.push(format!("{at}.count: a whole number 0..{}", super::limits::MAX_ROOM_COUNT));
    }
    let level = match &r["level"] {
        J::Null => Some(Level::Up(Floor::Typical)),
        v => Level::parse(v),
    };
    if level.is_none() {
        errs.push(format!("{at}.level: ground, typical, any, single, a storey number, or below:<n>"));
    }
    let light = match &r["light"] {
        J::Null => kind.map(|k| if k.habitable() { Light::Window } else { Light::None }),
        J::String(s) if s == "none" => Some(Light::None),
        J::String(s) if s == "window" => Some(Light::Window),
        J::Object(m) => m.get("daylight_share").and_then(J::as_f64).filter(|x| (0.0..=1.0).contains(x)).map(Light::Share),
        _ => None,
    };
    if light.is_none() {
        errs.push(format!("{at}.light: none, window or {{daylight_share: 0..1}}"));
    }
    let height_min = r["height_min"].as_f64();
    if !r["height_min"].is_null() && !height_min.is_some_and(|h| super::limits::FLOOR_HEIGHT.contains(h)) {
        errs.push(format!("{at}.height_min: m in {}", super::limits::FLOOR_HEIGHT.range()));
    }
    let access = match r["access"].as_str() {
        None if r["access"].is_null() => Some(Access::Public),
        Some("public") => Some(Access::Public),
        Some("staff") => Some(Access::Staff),
        Some("unit") => Some(Access::Unit),
        _ => None,
    };
    if access.is_none() {
        errs.push(format!("{at}.access: public, staff or unit"));
    }
    let adjacent_to = weights(&r["adjacent_to"], &format!("{at}.adjacent_to"), errs);
    let demand = Demand::parse(&r["demand"], &format!("{at}.demand"), errs);
    let occupancy_m2 = Num::opt(&r["occupancy_m2"], &format!("{at}.occupancy_m2"), errs);
    let live_load_kpa = Num::opt(&r["live_load_kpa"], &format!("{at}.live_load_kpa"), errs);
    if errs.len() > n0 {
        return None;
    }
    Some(TSpace {
        class: class?.to_string(),
        kind: kind?,
        name: name?,
        area: area?,
        count: count?,
        level: level?,
        light: light?,
        height_min,
        access: access?,
        adjacent_to,
        zone: r["zone"].as_str().map(str::to_string),
        demand,
        occupancy_m2,
        live_load_kpa,
        tags,
        count_num,
        periodic: r["periodic"].as_str().map(str::to_string),
        beds: r["beds"].as_u64().map_or(0, |b| b.min(u64::from(u8::MAX)) as u32),
    })
}

impl Model {
    /// Reads a model; every problem names its path.
    pub fn from_json(v: &J) -> Result<Model, Vec<String>> {
        let mut errs = vec![];
        let Some(o) = v.as_object() else {
            return Err(vec!["model: an object".into()]);
        };
        let known = ["title", "spaces", "zones", "egress", "structure", "below", "plant", "spare", "periodic", "units"];
        for k in o.keys().filter(|k| !known.contains(&k.as_str())) {
            errs.push(format!("{k}: unknown field; use {}", known.join(", ")));
        }
        let list = v["spaces"].as_array().cloned().unwrap_or_default();
        if list.is_empty() || list.len() > MAX_SPACES {
            errs.push(format!("spaces: 1..{MAX_SPACES} lines"));
        }
        let spaces: Vec<TSpace> = list.iter().enumerate().filter_map(|(i, r)| parse_space(i, r, &mut errs)).collect();
        let mut zones = vec![];
        match &v["zones"] {
            J::Null => {}
            J::Array(a) if a.len() <= MAX_ZONES => {
                for (i, z) in a.iter().enumerate() {
                    let at = format!("zones[{i}]");
                    let id = z["id"].as_str().filter(|s| !s.is_empty());
                    if id.is_none() {
                        errs.push(format!("{at}.id: a handle"));
                    }
                    let name = text(&z["name"], &format!("{at}.name"), &mut errs);
                    let adjacent_to = weights(&z["adjacent_to"], &format!("{at}.adjacent_to"), &mut errs);
                    if let (Some(id), Some(name)) = (id, name) {
                        zones.push(Zone { id: id.to_string(), name, adjacent_to });
                    }
                }
            }
            _ => errs.push(format!("zones: a list of at most {MAX_ZONES}")),
        }
        let egress = {
            let e = &v["egress"];
            let exits_min = match &e["exits_min"] {
                J::Null => Some(1),
                x => x.as_u64().filter(|n| (1..=8).contains(n)).map(|n| n as u32),
            };
            if exits_min.is_none() {
                errs.push("egress.exits_min: 1..8".into());
            }
            Egress { exits_min: exits_min.unwrap_or(1) }
        };
        let structure = Structure { live_load_kpa: Num::opt(&v["structure"]["live_load_kpa"], "structure.live_load_kpa", &mut errs) };
        let below = match &v["below"] {
            J::Null => None,
            b => {
                let mut levels = vec![];
                for (i, l) in b["levels"].as_array().cloned().unwrap_or_default().iter().enumerate() {
                    let at = format!("below.levels[{i}]");
                    let name = text(&l["name"], &format!("{at}.name"), &mut errs);
                    let fill = l["fill"].as_str().filter(|s| !s.is_empty());
                    if fill.is_none() {
                        errs.push(format!("{at}.fill: the class of the space that takes the rest of the level"));
                    }
                    let corridor = match &l["corridor"] {
                        J::Null => None,
                        c => text(c, &format!("{at}.corridor"), &mut errs),
                    };
                    if let (Some(name), Some(fill)) = (name, fill) {
                        levels.push(BelowLevel { name, fill: fill.to_string(), corridor });
                    }
                }
                let max = super::limits::max_basements() as usize;
                if levels.is_empty() || levels.len() > max {
                    errs.push(format!("below.levels: 1..{max} levels"));
                }
                let height = b["height"].as_f64();
                if height.is_some_and(|h| !(2.5..=12.0).contains(&h)) {
                    errs.push("below.height: 2.5..12 m".to_string());
                }
                Some(Below { levels, retaining: b["retaining"].as_bool().unwrap_or(true), height })
            }
        };
        let mut plant = vec![];
        for (i, pl) in v["plant"].as_array().cloned().unwrap_or_default().iter().enumerate() {
            let at = format!("plant[{i}]");
            let kind = pl["kind"].as_str().filter(|k| ["boiler_house", "pumping_station", "water_treatment", "process_line"].contains(k));
            if kind.is_none() {
                errs.push(format!("{at}.kind: boiler_house, pumping_station, water_treatment or process_line"));
            }
            let duty_kw = Qty::parse(&pl["duty_kw"], &format!("{at}.duty_kw"), &mut errs);
            let flow_m3h = Qty::parse(&pl["flow_m3h"], &format!("{at}.flow_m3h"), &mut errs);
            let units = pl["units"].as_u64().map(|n| n as u32);
            if let Some(kind) = kind {
                plant.push(PlantLine { kind: kind.to_string(), duty_kw, flow_m3h, head_m: pl["head_m"].as_f64(), units });
            }
        }
        let spare = match &v["spare"] {
            J::Null => None,
            sp => {
                let kind = sp["kind"].as_str().and_then(RoomKind::parse).filter(|k| *k != RoomKind::Hall);
                let grow = sp["grow_share"].as_f64().filter(|x| (0.0..=1.0).contains(x));
                let name = text(&sp["name"], "spare.name", &mut errs);
                if kind.is_none() || grow.is_none() {
                    errs.push("spare: {grow_share: 0..1, kind: a room kind, name}".into());
                }
                match (kind, grow, name) {
                    (Some(kind), Some(grow_share), Some(name)) => Some(super::programme::Spare { grow_share, kind, name }),
                    _ => None,
                }
            }
        };
        let mut units = vec![];
        for (i, x) in v["units"].as_array().cloned().unwrap_or_default().iter().enumerate() {
            let at = format!("units[{i}]");
            let name = text(&x["name"], &format!("{at}.name"), &mut errs);
            let count = Num::parse(&x["count"], &format!("{at}.count"), &mut errs);
            let mut rooms = vec![];
            for (j, r) in x["rooms"].as_array().cloned().unwrap_or_default().iter().enumerate() {
                let at = format!("{at}.rooms[{j}]");
                let kind = r["kind"].as_str().and_then(RoomKind::parse);
                let area = Num::parse(&r["area"], &format!("{at}.area"), &mut errs);
                let n = r["count"].as_u64().unwrap_or(1) as u32;
                let name = if r["name"].is_null() { None } else { text(&r["name"], &format!("{at}.name"), &mut errs) };
                match (kind, area) {
                    (Some(kind), Some(area)) if n >= 1 => rooms.push(UnitRoom { kind, area, count: n, name }),
                    _ => errs.push(format!("{at}: {{kind, area, count >= 1, name?}}")),
                }
            }
            if !rooms.iter().any(|r| r.kind.habitable() && !r.kind.inner()) {
                errs.push(format!("{at}.rooms: at least one living room or bedroom"));
            }
            if let (Some(name), Some(count)) = (name, count) {
                units.push(Unit { name, count, rooms });
            }
        }
        let mut periodic = vec![];
        for (i, x) in v["periodic"].as_array().cloned().unwrap_or_default().iter().enumerate() {
            let at = format!("periodic[{i}]");
            let id = x["id"].as_str().filter(|s| !s.is_empty());
            let at_k: Vec<usize> = x["at"].as_array().map(|a| a.iter().filter_map(|n| n.as_u64().map(|n| n as usize)).collect()).unwrap_or_default();
            let every = Num::parse(&x["every"], &format!("{at}.every"), &mut errs);
            if id.is_none() {
                errs.push(format!("{at}.id: the handle spaces name in `periodic`"));
            }
            if let (Some(id), Some(every)) = (id, every) {
                periodic.push(Periodic { id: id.to_string(), every, at: at_k, replaces: x["replaces"].as_bool().unwrap_or(false) });
            }
        }
        let title = match &v["title"] {
            J::Null => Some((String::new(), String::new())),
            t => text(t, "title", &mut errs),
        };
        let model = Model {
            id: String::new(),
            title: title.unwrap_or_default(),
            spaces,
            zones,
            egress,
            structure,
            below,
            plant,
            spare,
            periodic,
            units,
        };
        errs.extend(model.problems());
        if errs.is_empty() {
            Ok(model)
        } else {
            Err(errs)
        }
    }

    /// Cross-references: names unique, adjacency and zones named, below levels filled.
    fn problems(&self) -> Vec<String> {
        let mut errs = vec![];
        for (i, s) in self.spaces.iter().enumerate() {
            if self.spaces[..i].iter().any(|o| o.class == s.class) {
                errs.push(format!("spaces[{i}].class: '{}' twice", s.class));
            }
            if self.spaces[..i].iter().any(|o| o.name.0 == s.name.0 || o.name.1 == s.name.1) {
                errs.push(format!("spaces[{i}].name: '{}' twice; the demands find a room by its name", s.name.0));
            }
            if let Some(z) = &s.zone {
                if !self.zones.iter().any(|x| &x.id == z) {
                    errs.push(format!("spaces[{i}].zone: no zone '{z}'"));
                }
            }
            for (r, _) in &s.adjacent_to {
                let kind_word = RoomKind::parse(r).is_some() || r == "corridor" || r == "stair" || r == "lift";
                if !kind_word && !self.spaces.iter().any(|o| &o.class == r) {
                    errs.push(format!("spaces[{i}].adjacent_to: '{r}' is neither a class nor a kind"));
                }
            }
            if let Some(p) = &s.periodic {
                if !self.periodic.iter().any(|x| &x.id == p) {
                    errs.push(format!("spaces[{i}].periodic: no periodic storey '{p}'"));
                }
            }
            if matches!(s.level, Level::Below(_)) && self.below.is_none() {
                errs.push(format!("spaces[{i}].level: below ground, but the model has no `below`"));
            }
            if let Level::Below(n) = s.level {
                if self.below.as_ref().is_some_and(|b| n as usize > b.levels.len()) {
                    errs.push(format!("spaces[{i}].level: below:{n}, but the model has fewer levels below"));
                }
            }
        }
        for (i, z) in self.zones.iter().enumerate() {
            for (r, _) in &z.adjacent_to {
                if !self.zones.iter().any(|o| &o.id == r) {
                    errs.push(format!("zones[{i}].adjacent_to: no zone '{r}'"));
                }
            }
        }
        if let Some(b) = &self.below {
            for (i, l) in b.levels.iter().enumerate() {
                let fill = self.spaces.iter().find(|s| s.class == l.fill);
                match fill.map(|s| s.level) {
                    Some(Level::Below(n)) if n as usize == i + 1 => {}
                    _ => errs.push(format!(
                        "below.levels[{i}].fill: '{}' must be a class of level below:{}",
                        l.fill,
                        i + 1
                    )),
                }
            }
        }
        for (i, p) in self.plant.iter().enumerate() {
            if p.duty_kw.is_none() && p.flow_m3h.is_none() {
                errs.push(format!("plant[{i}]: needs duty_kw or flow_m3h"));
            }
        }
        errs
    }

    pub fn to_json(&self) -> J {
        let spaces: Vec<J> = self
            .spaces
            .iter()
            .map(|s| {
                let mut o = Map::new();
                o.insert("class".into(), json!(s.class));
                o.insert("kind".into(), json!(s.kind.key()));
                o.insert("name".into(), name_json(&s.name));
                let area = if s.area.0 == s.area.1 { json!(s.area.0) } else { json!({"min": s.area.0, "max": s.area.1}) };
                o.insert("area".into(), area);
                match &s.count_num {
                    Some(n) => o.insert("count".into(), n.to_json()),
                    None => o.insert("count".into(), json!(s.count)),
                };
                if let Some(p) = &s.periodic {
                    o.insert("periodic".into(), json!(p));
                }
                o.insert("level".into(), s.level.to_json());
                let light = match s.light {
                    Light::None => json!("none"),
                    Light::Window => json!("window"),
                    Light::Share(x) => json!({"daylight_share": x}),
                };
                o.insert("light".into(), light);
                if let Some(h) = s.height_min {
                    o.insert("height_min".into(), json!(h));
                }
                o.insert("access".into(), json!(s.access.name()));
                if !s.adjacent_to.is_empty() {
                    o.insert("adjacent_to".into(), weights_json(&s.adjacent_to));
                }
                if let Some(z) = &s.zone {
                    o.insert("zone".into(), json!(z));
                }
                if !s.demand.is_empty() {
                    o.insert("demand".into(), s.demand.to_json());
                }
                if let Some(n) = &s.occupancy_m2 {
                    o.insert("occupancy_m2".into(), n.to_json());
                }
                if s.beds > 0 {
                    o.insert("beds".into(), json!(s.beds));
                }
                if let Some(n) = &s.live_load_kpa {
                    o.insert("live_load_kpa".into(), n.to_json());
                }
                if !s.tags.is_empty() {
                    let ids: Vec<&str> = super::RoomTags::IDS.iter().copied().filter(|i| super::RoomTags::parse(i).is_some_and(|t| s.tags.meets(t))).collect();
                    o.insert("tags".into(), json!(ids));
                }
                J::Object(o)
            })
            .collect();
        let zones: Vec<J> = self
            .zones
            .iter()
            .map(|z| json!({"id": z.id, "name": name_json(&z.name), "adjacent_to": weights_json(&z.adjacent_to)}))
            .collect();
        let mut o = Map::new();
        if !self.title.0.is_empty() {
            o.insert("title".into(), name_json(&self.title));
        }
        o.insert("spaces".into(), J::Array(spaces));
        o.insert("zones".into(), J::Array(zones));
        o.insert(
            "egress".into(),
            json!({"exits_min": self.egress.exits_min}),
        );
        if let Some(n) = &self.structure.live_load_kpa {
            o.insert("structure".into(), json!({ "live_load_kpa": n.to_json() }));
        }
        if let Some(b) = &self.below {
            let levels: Vec<J> = b
                .levels
                .iter()
                .map(|l| {
                    let mut x = Map::new();
                    x.insert("name".into(), name_json(&l.name));
                    x.insert("fill".into(), json!(l.fill));
                    if let Some(c) = &l.corridor {
                        x.insert("corridor".into(), name_json(c));
                    }
                    J::Object(x)
                })
                .collect();
            let mut bj = json!({"levels": levels, "retaining": b.retaining});
            if let Some(h) = b.height {
                bj["height"] = json!(h);
            }
            o.insert("below".into(), bj);
        }
        if !self.units.is_empty() {
            let list: Vec<J> = self
                .units
                .iter()
                .map(|u| {
                    let rooms: Vec<J> = u
                        .rooms
                        .iter()
                        .map(|r| {
                            let mut x = Map::new();
                            x.insert("kind".into(), json!(r.kind.key()));
                            x.insert("area".into(), r.area.to_json());
                            x.insert("count".into(), json!(r.count));
                            if let Some(n) = &r.name {
                                x.insert("name".into(), name_json(n));
                            }
                            J::Object(x)
                        })
                        .collect();
                    json!({"name": name_json(&u.name), "count": u.count.to_json(), "rooms": rooms})
                })
                .collect();
            o.insert("units".into(), J::Array(list));
        }
        if !self.periodic.is_empty() {
            let list: Vec<J> = self
                .periodic
                .iter()
                .map(|p| {
                    let mut j = json!({"id": p.id, "every": p.every.to_json(), "replaces": p.replaces});
                    if !p.at.is_empty() {
                        j["at"] = json!(p.at);
                    }
                    j
                })
                .collect();
            o.insert("periodic".into(), J::Array(list));
        }
        if let Some(sp) = &self.spare {
            o.insert("spare".into(), json!({"grow_share": sp.grow_share, "kind": sp.kind.key(), "name": name_json(&sp.name)}));
        }
        if !self.plant.is_empty() {
            let plant: Vec<J> = self
                .plant
                .iter()
                .map(|p| {
                    let mut x = Map::new();
                    x.insert("kind".into(), json!(p.kind));
                    if let Some(q) = &p.duty_kw {
                        x.insert("duty_kw".into(), q.to_json());
                    }
                    if let Some(q) = &p.flow_m3h {
                        x.insert("flow_m3h".into(), q.to_json());
                    }
                    if let Some(h) = p.head_m {
                        x.insert("head_m".into(), json!(h));
                    }
                    if let Some(n) = p.units {
                        x.insert("units".into(), json!(n));
                    }
                    J::Object(x)
                })
                .collect();
            o.insert("plant".into(), J::Array(plant));
        }
        J::Object(o)
    }

    /// The space of a room by the layout kind and the name the plan gave it, in either language.
    pub fn of(&self, kind: RoomKind, name: &str) -> Option<&TSpace> {
        self.spaces.iter().find(|s| s.kind == kind && (s.name.0 == name || s.name.1 == name))
    }

    /// The spaces above ground as the programme the plan builder reads. A zone that wants
    /// another beside it makes every space of the one want every space of the other.
    pub fn program(&self) -> Program {
        self.program_with(None)
    }

    /// The programme with the counts that follow a rule read under `rs`; the periodic
    /// storeys' spaces are left out, they go on their own storeys.
    pub fn program_with(&self, rs: Option<&RuleSet>) -> Program {
        let mut spaces = vec![];
        for s in self.spaces.iter().filter(|s| matches!(s.level, Level::Up(_)) && s.periodic.is_none()) {
            let Level::Up(floor) = s.level else { continue };
            let mut adjacent_to = s.adjacent_to.clone();
            let zone = s.zone.as_ref().and_then(|z| self.zones.iter().find(|x| &x.id == z));
            for (other, w) in zone.iter().flat_map(|z| z.adjacent_to.iter()) {
                for t in self.spaces.iter().filter(|t| t.zone.as_ref() == Some(other) && t.class != s.class) {
                    if !adjacent_to.iter().any(|a| a.0 == t.class) {
                        adjacent_to.push((t.class.clone(), *w));
                    }
                }
            }
            spaces.push(Space {
                id: Some(s.class.clone()),
                kind: s.kind,
                name: Some(s.name.clone()),
                area: s.area,
                count: s.count_in(rs),
                floor,
                light: s.light,
                height_min: s.height_min,
                access: s.access,
                adjacent_to,
            });
        }
        Program {
            spaces,
            floors: None,
            basements: self.below.as_ref().map(|b| b.levels.len() as u32),
            features: vec![],
        }
    }

    /// The apartments of the section under `rs`: each kind as many times as its rule says.
    pub fn apartments(&self, ru: bool, rs: &RuleSet) -> Vec<super::ApartmentReq> {
        self.units
            .iter()
            .filter_map(|u| {
                let count = u.count.get(rs).round() as u32;
                (count > 0).then(|| super::ApartmentReq {
                    name: (if ru { &u.name.1 } else { &u.name.0 }).clone(),
                    count,
                    rooms: u
                        .rooms
                        .iter()
                        .map(|r| super::RoomReq {
                            name: r
                                .name
                                .as_ref()
                                .map_or_else(|| super::names::kind_name(r.kind, ru).to_string(), |n| (if ru { &n.1 } else { &n.0 }).clone()),
                            kind: r.kind,
                            area: r.area.get(rs),
                            count: r.count,
                            tags: Default::default(),
                        })
                        .collect(),
                })
            })
            .collect()
    }

    /// Whether storey `k` (0 is the ground floor) of a building of `floors` is a periodic
    /// storey `id`: every `every`-th storey counted from 1, never the top one.
    pub fn is_periodic(&self, id: &str, k: usize, floors: u32, rs: &RuleSet) -> bool {
        let Some(p) = self.periodic.iter().find(|p| p.id == id) else {
            return false;
        };
        if p.at.contains(&k) {
            return true;
        }
        let every = p.every.get(rs).round() as usize;
        every >= 2 && k >= 1 && (k + 1) % every == 0 && k + 1 < floors as usize
    }

    /// How many storeys of a building the space repeats on.
    pub fn storeys_of(&self, s: &TSpace, floors: u32, basements: u32, rs: &RuleSet) -> u32 {
        match &s.periodic {
            Some(id) => (0..floors as usize).filter(|k| self.is_periodic(id, *k, floors, rs)).count() as u32,
            None => s.level.storeys(floors, basements),
        }
    }

    /// The rooms of storey `k` when it is a periodic one: the typical rooms `typical` plus the
    /// periodic spaces, or the periodic spaces alone when the storey replaces them.
    pub fn periodic_rooms(&self, k: usize, floors: u32, ru: bool, rs: &RuleSet, typical: &[super::RoomReq]) -> Option<Vec<super::RoomReq>> {
        let p = self.periodic.iter().find(|p| self.is_periodic(&p.id, k, floors, rs))?;
        let mut out: Vec<super::RoomReq> = if p.replaces { vec![] } else { typical.to_vec() };
        out.extend(self.spaces.iter().filter(|s| s.periodic.as_deref() == Some(p.id.as_str()) && s.count_in(Some(rs)) > 0).map(|s| s.req(ru, rs)));
        Some(out)
    }

    /// The tags of the spaces, by kind and either language's name, for the spec's tag table.
    pub fn room_tags(&self) -> Vec<(RoomKind, String, super::RoomTags)> {
        self.spaces
            .iter()
            .filter(|s| !s.tags.is_empty())
            .flat_map(|s| [(s.kind, s.name.0.clone(), s.tags), (s.kind, s.name.1.clone(), s.tags)])
            .collect()
    }

    /// Supply and extract air of a room this model describes, m3/h; `None` for a room it
    /// states no air for, which the generic rates serve.
    pub fn air(&self, kind: RoomKind, name: &str, area: f64, clear: f64, rs: &RuleSet) -> Option<(f64, f64)> {
        let d = &self.of(kind, name)?.demand;
        if d.ach.is_none() && d.supply_m3h_m2.is_none() {
            return None;
        }
        Some(air_of(d, area, clear, rs))
    }

    /// The loads of the whole building: every space, its rooms repeated on the storeys it
    /// belongs on.
    pub fn schedule(&self, floors: u32, basements: u32, clear: f64, ru: bool, rs: &RuleSet) -> Schedule {
        let (cool_k, power_k) = (rs.get("typology.cooling.scale"), rs.get("typology.power.scale"));
        let rows = self
            .spaces
            .iter()
            .filter_map(|s| {
                let rooms = s.count_in(Some(rs)) * self.storeys_of(s, floors, basements, rs);
                if rooms == 0 {
                    return None;
                }
                let area = s.target_area() * f64::from(rooms);
                let one = air_of(&s.demand, s.target_area(), clear, rs);
                let n = f64::from(rooms);
                let per_m2 = |x: &Option<Num>, k: f64| x.as_ref().map_or(0.0, |x| x.get(rs) * k * area / 1000.0);
                let power_kw = per_m2(&s.demand.power_w_m2, power_k);
                let ppl = s.occupancy_m2.as_ref().map_or(0.0, |o| (area / o.get(rs).max(f64::EPSILON)).floor());
                Some(Row {
                    class: s.class.clone(),
                    name: s.display(ru).to_string(),
                    rooms,
                    area,
                    supply_m3h: one.0 * n,
                    extract_m3h: one.1 * n,
                    cooling_kw: per_m2(&s.demand.cooling_w_m2, cool_k),
                    power_kw,
                    backup_kw: if s.demand.backup_power { power_kw } else { 0.0 },
                    water_m3d: s.demand.water_l_day.as_ref().map_or(0.0, |w| w.get(rs) * n / 1000.0),
                    persons: ppl,
                    beds: f64::from(s.beds) * n,
                    live_load_kpa: s.live_load_kpa.as_ref().or(self.structure.live_load_kpa.as_ref()).map_or(0.0, |x| x.get(rs)),
                    below: matches!(s.level, Level::Below(_)),
                })
            })
            .collect();
        Schedule { rows }
    }

    /// The loads of the rooms the generator built: every room of the project, above and below
    /// ground, that this model describes, with its real plan area.
    pub fn schedule_built(&self, p: &super::GProject, spec: &super::ProjectSpec) -> Schedule {
        let rs = &spec.rules;
        let (cool_k, power_k) = (rs.get("typology.cooling.scale"), rs.get("typology.power.scale"));
        let mut rows: Vec<Row> = vec![];
        let tiers = p.storeys.iter().map(|s| (s, false)).chain(p.basements.iter().map(|s| (s, true)));
        for (st, below) in tiers {
            let clear = st.height - rs.get("services.slab");
            for (pr, gr) in st.rooms.iter().zip(&st.layout.rooms) {
                let Some(s) = self.of(pr.kind, &gr.name) else { continue };
                let area = poly_area(&gr.polygon);
                // a care class (operating, isolation, clinical ward) takes the norm of its tags
                let (qs, qe) = if s.demand.ach.is_none() && s.demand.supply_m3h_m2.is_none() {
                    super::care::air(s.kind, s.tags, area, clear).unwrap_or_default()
                } else {
                    air_of(&s.demand, area, clear, rs)
                };
                let per_m2 = |x: &Option<Num>, k: f64| x.as_ref().map_or(0.0, |x| x.get(rs) * k * area / 1000.0);
                let power_kw = per_m2(&s.demand.power_w_m2, power_k);
                let at = rows.iter().position(|r| r.class == s.class && r.below == below).unwrap_or_else(|| {
                    rows.push(Row {
                        class: s.class.clone(),
                        name: s.display(spec.ru).to_string(),
                        rooms: 0,
                        area: 0.0,
                        supply_m3h: 0.0,
                        extract_m3h: 0.0,
                        cooling_kw: 0.0,
                        power_kw: 0.0,
                        backup_kw: 0.0,
                        water_m3d: 0.0,
                        persons: 0.0,
                        beds: 0.0,
                        live_load_kpa: s.live_load_kpa.as_ref().or(self.structure.live_load_kpa.as_ref()).map_or(0.0, |x| x.get(rs)),
                        below,
                    });
                    rows.len() - 1
                });
                let r = &mut rows[at];
                r.rooms += 1;
                r.area += area;
                r.supply_m3h += qs;
                r.extract_m3h += qe;
                r.cooling_kw += per_m2(&s.demand.cooling_w_m2, cool_k);
                r.power_kw += power_kw;
                if s.demand.backup_power {
                    r.backup_kw += power_kw;
                }
                r.water_m3d += s.demand.water_l_day.as_ref().map_or(0.0, |w| w.get(rs) / 1000.0);
                r.beds += f64::from(s.beds);
                r.persons += s.occupancy_m2.as_ref().map_or(0.0, |o| (area / o.get(rs).max(f64::EPSILON)).floor());
            }
        }
        Schedule { rows }
    }

    /// The briefs of the plants this model needs, in the shape the plant module takes, every quantity read
    /// from the loads of `sch`. The plant module designs the equipment, pipes and platforms.
    pub fn plant_briefs(&self, sch: &Schedule, rs: &RuleSet) -> Vec<J> {
        let t = sch.totals();
        // kWh to warm one m3 of water by one kelvin
        let heat_per_m3k = 1.163;
        let qty = |q: &Qty| match q {
            Qty::Fixed(x) => *x,
            Qty::Of { from, share } => {
                let base = match from.as_str() {
                    "cooling_kw" => t.cooling_kw,
                    "power_kw" => t.power_kw,
                    "backup_kw" => t.backup_kw,
                    "air_m3h" => t.supply_m3h,
                    "cooling_water_m3h" => t.cooling_kw / (heat_per_m3k * rs.get("typology.plant.delta_t")),
                    _ => t.water_m3d / 24.0,
                };
                (base * share * 10.0).round() / 10.0
            }
        };
        self.plant
            .iter()
            .map(|p| {
                let mut o = Map::new();
                o.insert("kind".into(), json!(p.kind));
                if let Some(q) = &p.duty_kw {
                    o.insert("duty_kw".into(), json!(qty(q)));
                }
                if let Some(q) = &p.flow_m3h {
                    o.insert("flow_m3h".into(), json!(qty(q)));
                }
                if let Some(h) = p.head_m {
                    o.insert("head_m".into(), json!(h));
                }
                if let Some(n) = p.units {
                    o.insert("units".into(), json!(n));
                }
                J::Object(o)
            })
            .collect()
    }
}

/// Supply and extract of one room: the larger of the air change and the floor rate, rounded up
/// to the flow step; the pressure sets which side falls short by the offset.
fn air_of(d: &Demand, area: f64, clear: f64, rs: &RuleSet) -> (f64, f64) {
    let k = rs.get("typology.air.scale");
    let by_ach = d.ach.as_ref().map_or(0.0, |n| n.get(rs) * area * clear);
    let by_area = d.supply_m3h_m2.as_ref().map_or(0.0, |n| n.get(rs) * area);
    let step = rs.get("services.hvac.flow_step");
    let up = |q: f64| (q / step).ceil() * step;
    let q = up(by_ach.max(by_area) * k);
    if q <= 0.0 {
        return (0.0, 0.0);
    }
    // the lower side stays under the higher one by at least a flow step, so the pressure holds
    let low = up(q * (1.0 - rs.get("typology.air.pressure_offset"))).min((q - step).max(0.0));
    match d.pressure.unwrap_or(Pressure::Neutral) {
        Pressure::Neutral => (q, q),
        Pressure::Positive => (q, low),
        Pressure::Negative => (low, q),
    }
}

/// One class of space with its loads over the whole building.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub class: String,
    pub name: String,
    pub rooms: u32,
    pub area: f64,
    pub supply_m3h: f64,
    pub extract_m3h: f64,
    pub cooling_kw: f64,
    pub power_kw: f64,
    pub backup_kw: f64,
    pub water_m3d: f64,
    pub persons: f64,
    pub beds: f64,
    pub live_load_kpa: f64,
    pub below: bool,
}

/// The loads by class of space.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schedule {
    pub rows: Vec<Row>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Totals {
    pub rooms: u32,
    pub area: f64,
    pub supply_m3h: f64,
    pub extract_m3h: f64,
    pub cooling_kw: f64,
    pub power_kw: f64,
    pub backup_kw: f64,
    pub water_m3d: f64,
    pub persons: f64,
    pub beds: f64,
    pub live_load_kpa: f64,
}

impl Schedule {
    pub fn totals(&self) -> Totals {
        let mut t = Totals::default();
        for r in &self.rows {
            t.rooms += r.rooms;
            t.area += r.area;
            t.supply_m3h += r.supply_m3h;
            t.extract_m3h += r.extract_m3h;
            t.cooling_kw += r.cooling_kw;
            t.power_kw += r.power_kw;
            t.backup_kw += r.backup_kw;
            t.water_m3d += r.water_m3d;
            t.persons += r.persons;
            t.beds += r.beds;
            t.live_load_kpa = t.live_load_kpa.max(r.live_load_kpa);
        }
        t
    }

    pub fn to_json(&self) -> J {
        let rows: Vec<J> = self
            .rows
            .iter()
            .map(|r| {
                json!({
                    "class": r.class, "name": r.name, "rooms": r.rooms, "area_m2": r.area,
                    "supply_m3h": r.supply_m3h, "extract_m3h": r.extract_m3h, "cooling_kw": r.cooling_kw,
                    "power_kw": r.power_kw, "backup_kw": r.backup_kw, "water_m3d": r.water_m3d,
                    "persons": r.persons, "beds": r.beds, "live_load_kpa": r.live_load_kpa, "below_ground": r.below,
                })
            })
            .collect();
        let t = self.totals();
        json!({
            "rows": rows,
            "totals": {
                "rooms": t.rooms, "area_m2": t.area, "supply_m3h": t.supply_m3h, "extract_m3h": t.extract_m3h,
                "cooling_kw": t.cooling_kw, "power_kw": t.power_kw, "backup_kw": t.backup_kw,
                "water_m3d": t.water_m3d, "persons": t.persons, "beds": t.beds, "live_load_kpa": t.live_load_kpa,
            },
        })
    }
}

/// The schedule of the rooms a built project holds; `None` for a typology without a model.
pub fn schedule_of(spec: &super::ProjectSpec, p: &super::GProject) -> Option<Schedule> {
    Some(spec.model.as_ref()?.schedule_built(p, spec))
}

fn poly_area(poly: &[(f64, f64)]) -> f64 {
    let n = poly.len();
    (0..n).map(|i| poly[i].0 * poly[(i + 1) % n].1 - poly[(i + 1) % n].0 * poly[i].1).sum::<f64>().abs() / 2.0
}

/// Adds the loads of the model to the project metrics.
pub(super) fn apply(spec: &super::ProjectSpec, p: &mut super::GProject) {
    let (Some(m), Some(sch)) = (spec.model.as_ref(), schedule_of(spec, p)) else {
        return;
    };
    let t = sch.totals();
    let retaining = p
        .basements
        .iter()
        .flat_map(|s| &s.layout.walls)
        .filter(|w| w.role == crate::WallRole::Retaining)
        .count();
    p.metrics.extend([
        ("typ_rooms", f64::from(t.rooms)),
        ("typ_supply_m3h", t.supply_m3h),
        ("typ_extract_m3h", t.extract_m3h),
        ("typ_cooling_kw", t.cooling_kw),
        ("typ_power_kw", t.power_kw),
        ("typ_backup_kw", t.backup_kw),
        ("typ_water_m3d", t.water_m3d),
        ("typ_persons", t.persons),
        ("typ_live_load_kpa", t.live_load_kpa),
        ("typ_levels_below", m.below.as_ref().map_or(0.0, |b| b.levels.len() as f64)),
        ("typ_retaining_walls", retaining as f64),
    ]);
    if t.beds > 0.0 {
        p.metrics.push(("typ_beds", t.beds));
    }
    if m.below.as_ref().is_some_and(|b| b.retaining) {
        p.metrics.push(("typ_retaining_t", spec.rules.get("underground.retaining_thickness")));
    }
}
