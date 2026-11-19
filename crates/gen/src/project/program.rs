// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The room programme as data (`gen_program.v1`): what is built, independent of how. Each
//! space line names a kind, an area range, a count, the floor it belongs on, the light,
//! headroom and access it needs and the kinds it wants beside it. The builder reads the
//! lines; the solver scores light, height and adjacency against them.

use super::programme::{Line, Programme};
use super::RoomKind;
use serde_json::{json, Map, Value as J};

pub const SCHEMA: &str = "gen_program.v1";
/// Most space lines one programme may hold.
pub const MAX_SPACES: usize = 200;

/// The floor a space belongs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Floor {
    Ground,
    Typical,
    /// Wherever it fits: built on the typical floors.
    Any,
    /// Only in a one-storey building.
    Single,
    /// One storey, 0 for the ground floor.
    Level(u32),
}

impl Floor {
    pub(super) fn parse(v: &J) -> Option<Floor> {
        match v {
            J::Null => Some(Floor::Typical),
            J::Number(n) => n.as_u64().and_then(|x| u32::try_from(x).ok()).map(Floor::Level),
            J::String(s) => match s.as_str() {
                "ground" => Some(Floor::Ground),
                "typical" => Some(Floor::Typical),
                "any" => Some(Floor::Any),
                "single" => Some(Floor::Single),
                _ => None,
            },
            _ => None,
        }
    }

    pub(super) fn to_json(self) -> J {
        match self {
            Floor::Ground => json!("ground"),
            Floor::Typical => json!("typical"),
            Floor::Any => json!("any"),
            Floor::Single => json!("single"),
            Floor::Level(n) => json!(n),
        }
    }
}

/// What daylight a space needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Light {
    None,
    /// At least one window.
    Window,
    /// Glazing of at least this share of the floor area.
    Share(f64),
}

/// Who reaches the space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Public,
    Staff,
    /// Inside one apartment or unit.
    Unit,
}

impl Access {
    const NAMES: [(&'static str, Access); 3] =
        [("public", Access::Public), ("staff", Access::Staff), ("unit", Access::Unit)];

    pub fn name(self) -> &'static str {
        Self::NAMES.iter().find(|x| x.1 == self).map_or("public", |x| x.0)
    }
}

/// One line of the programme.
#[derive(Clone, Debug, PartialEq)]
pub struct Space {
    /// Optional handle other lines name in `adjacent_to`.
    pub id: Option<String>,
    pub kind: RoomKind,
    /// English and Russian names; `None` takes the typology's name of the kind.
    pub name: Option<(String, String)>,
    /// Area range, m2; `min == max` asks for one area.
    pub area: (f64, f64),
    pub count: u32,
    pub floor: Floor,
    pub light: Light,
    pub height_min: Option<f64>,
    pub access: Access,
    /// Kinds or ids wanted beside this space, with the weight of each wish.
    pub adjacent_to: Vec<(String, f64)>,
}

impl Space {
    /// The area the builder lays out: the middle of the range.
    pub fn target_area(&self) -> f64 {
        if self.area.0 == self.area.1 {
            self.area.0
        } else {
            (self.area.0 + self.area.1) / 2.0
        }
    }

    fn line(&self) -> Line {
        Line {
            kind: self.kind,
            area: self.target_area(),
            count: self.count,
            name: self.name.clone(),
        }
    }

    fn from_line(l: &Line, floor: Floor) -> Space {
        Space {
            id: None,
            kind: l.kind,
            name: l.name.clone(),
            area: (l.area, l.area),
            count: l.count,
            floor,
            light: if l.kind.habitable() { Light::Window } else { Light::None },
            height_min: None,
            access: Access::Public,
            adjacent_to: vec![],
        }
    }

    /// Whether this space answers a name in `adjacent_to`: its id or its kind.
    pub fn answers(&self, name: &str) -> bool {
        self.id.as_deref() == Some(name) || self.kind.key() == name
    }
}

/// The programme: space lines and the building-wide asks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Program {
    pub spaces: Vec<Space>,
    pub floors: Option<u32>,
    pub basements: Option<u32>,
    pub features: Vec<String>,
}

fn space(i: usize, r: &J, errs: &mut Vec<String>) -> Option<Space> {
    let at = format!("spaces[{i}]");
    let mut bad = |m: String| errs.push(format!("{at}.{m}"));
    let Some(o) = r.as_object() else {
        bad("must be an object".into());
        return None;
    };
    let known = [
        "id", "kind", "name", "area", "count", "floor", "light", "height_min", "access", "adjacent_to",
    ];
    for k in o.keys().filter(|k| !known.contains(&k.as_str())) {
        bad(format!("{k}: unknown field; use {}", known.join(", ")));
    }
    let k = r["kind"].as_str().unwrap_or_default();
    let kind = super::kinds::parse(k).map(|x| x.0);
    if kind.is_none() {
        bad(format!("kind: unknown '{k}'; one of {}", super::kinds::words().join(", ")));
    }
    let lim: super::limits::ParamDef = *super::limits::ROOM_AREA;
    let area = match &r["area"] {
        J::Number(n) => n.as_f64().map(|a| (a, a)),
        J::Array(a) if a.len() == 2 => a[0].as_f64().zip(a[1].as_f64()),
        J::Object(m) => m.get("min").and_then(J::as_f64).zip(m.get("max").and_then(J::as_f64)),
        _ => None,
    }
    .filter(|a| a.0 <= a.1 && lim.contains(a.0) && lim.contains(a.1));
    if area.is_none() {
        bad(format!("area: m2 in {}, a number or {{min, max}}", lim.range()));
    }
    let count = match &r["count"] {
        J::Null => Some(1),
        c => c.as_u64().filter(|c| *c <= u64::from(super::limits::MAX_ROOM_COUNT)).map(|c| c as u32),
    };
    if count.is_none() {
        bad(format!("count: a whole number 0..{}", super::limits::MAX_ROOM_COUNT));
    }
    let floor = Floor::parse(&r["floor"]);
    if floor.is_none() {
        bad("floor: ground, typical, any, single or a storey number".into());
    }
    let light = match &r["light"] {
        J::Null => kind.map(|k| if k.habitable() { Light::Window } else { Light::None }),
        J::String(s) if s == "none" => Some(Light::None),
        J::String(s) if s == "window" => Some(Light::Window),
        J::Object(m) => m
            .get("daylight_share")
            .and_then(J::as_f64)
            .filter(|x| (0.0..=1.0).contains(x))
            .map(Light::Share),
        _ => None,
    };
    if light.is_none() {
        bad("light: none, window or {daylight_share: 0..1}".into());
    }
    let height_min = r["height_min"].as_f64();
    if !r["height_min"].is_null() && !height_min.is_some_and(|h| super::limits::FLOOR_HEIGHT.contains(h)) {
        bad(format!("height_min: m in {}", super::limits::FLOOR_HEIGHT.range()));
    }
    let access = match r["access"].as_str() {
        None if r["access"].is_null() => Some(Access::Public),
        Some(s) => Access::NAMES.iter().find(|x| x.0 == s).map(|x| x.1),
        None => None,
    };
    if access.is_none() {
        bad("access: public, staff or unit".into());
    }
    let mut adjacent_to = vec![];
    match &r["adjacent_to"] {
        J::Null => {}
        J::Array(a) => {
            for (j, x) in a.iter().enumerate() {
                let w = match x {
                    J::String(s) => Some((s.clone(), 1.0)),
                    J::Object(m) => m.get("ref").and_then(J::as_str).map(|s| {
                        (s.to_string(), m.get("weight").and_then(J::as_f64).unwrap_or(1.0))
                    }),
                    _ => None,
                };
                match w {
                    Some((s, w)) if w.is_finite() && (0.0..=super::rules::MAX_WEIGHT).contains(&w) => {
                        adjacent_to.push((s, w))
                    }
                    _ => bad(format!("adjacent_to[{j}]: a kind or id, or {{ref, weight}}")),
                }
            }
        }
        _ => bad("adjacent_to: a list".into()),
    }
    let name = match &r["name"] {
        J::Null => None,
        J::String(x) => Some((x.clone(), x.clone())),
        n if n.is_object() => {
            let en = n["en"].as_str().unwrap_or(k).to_string();
            Some((en.clone(), n["ru"].as_str().map_or(en, str::to_string)))
        }
        _ => {
            bad("name: text or {en, ru}".into());
            None
        }
    };
    Some(Space {
        id: r["id"].as_str().map(str::to_string),
        kind: kind?,
        name,
        area: area?,
        count: count?,
        floor: floor?,
        light: light?,
        height_min,
        access: access?,
        adjacent_to,
    })
}

impl Program {
    /// Reads a programme; every problem names its path.
    pub fn from_json(v: &J) -> Result<Program, Vec<String>> {
        let mut errs = vec![];
        let Some(o) = v.as_object() else {
            return Err(vec!["program: an object with spaces".into()]);
        };
        if o.get("schema").is_some_and(|s| s != SCHEMA) {
            errs.push(format!("schema: must be {SCHEMA}"));
        }
        for k in o.keys() {
            if !["schema", "spaces", "floors", "basements", "features"].contains(&k.as_str()) {
                errs.push(format!("{k}: unknown field; use spaces, floors, basements, features"));
            }
        }
        let list = v["spaces"].as_array().cloned().unwrap_or_default();
        if list.len() > MAX_SPACES {
            errs.push(format!("spaces: at most {MAX_SPACES}"));
        }
        let spaces: Vec<Space> = list.iter().enumerate().filter_map(|(i, r)| space(i, r, &mut errs)).collect();
        let int = |k: &str, errs: &mut Vec<String>| match &v[k] {
            J::Null => None,
            x => {
                let n = x.as_u64().and_then(|n| u32::try_from(n).ok());
                if n.is_none() {
                    errs.push(format!("{k}: a whole number"));
                }
                n
            }
        };
        let floors = int("floors", &mut errs);
        let basements = int("basements", &mut errs);
        let features = v["features"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if errs.is_empty() {
            Ok(Program {
                spaces,
                floors,
                basements,
                features,
            })
        } else {
            Err(errs)
        }
    }

    pub fn to_json(&self) -> J {
        let spaces: Vec<J> = self
            .spaces
            .iter()
            .map(|s| {
                let mut o = Map::new();
                if let Some(id) = &s.id {
                    o.insert("id".into(), json!(id));
                }
                o.insert("kind".into(), json!(s.kind.key()));
                if let Some((en, ru)) = &s.name {
                    o.insert("name".into(), json!({"en": en, "ru": ru}));
                }
                let area = if s.area.0 == s.area.1 { json!(s.area.0) } else { json!({"min": s.area.0, "max": s.area.1}) };
                o.insert("area".into(), area);
                o.insert("count".into(), json!(s.count));
                o.insert("floor".into(), s.floor.to_json());
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
                    let adj: Vec<J> = s.adjacent_to.iter().map(|(r, w)| json!({"ref": r, "weight": w})).collect();
                    o.insert("adjacent_to".into(), J::Array(adj));
                }
                J::Object(o)
            })
            .collect();
        let mut o = Map::new();
        o.insert("schema".into(), json!(SCHEMA));
        o.insert("spaces".into(), J::Array(spaces));
        if let Some(f) = self.floors {
            o.insert("floors".into(), json!(f));
        }
        if let Some(b) = self.basements {
            o.insert("basements".into(), json!(b));
        }
        if !self.features.is_empty() {
            o.insert("features".into(), json!(self.features));
        }
        J::Object(o)
    }

    /// A typology programme as space lines: ground, typical, then the one-storey lines.
    pub fn from_programme(p: &Programme) -> Program {
        let lines = |ls: &[Line], f: Floor| ls.iter().map(move |l| Space::from_line(l, f)).collect::<Vec<_>>();
        let mut spaces = lines(&p.ground, Floor::Ground);
        spaces.extend(lines(&p.typical, Floor::Typical));
        spaces.extend(lines(&p.single, Floor::Single));
        Program {
            spaces,
            ..Program::default()
        }
    }

    fn lines_where(&self, keep: impl Fn(Floor) -> bool) -> Option<Vec<Line>> {
        let out: Vec<Line> = self.spaces.iter().filter(|s| keep(s.floor)).map(Space::line).collect();
        (!out.is_empty()).then_some(out)
    }

    /// `base` with the lines this programme gives: ground lines replace the ground floor,
    /// typical and any lines the floors above, single lines a one-storey building. A floor
    /// the programme names no line for keeps the base lines.
    pub fn over(&self, base: &Programme) -> Programme {
        let mut p = base.clone();
        if let Some(l) = self.lines_where(|f| matches!(f, Floor::Ground | Floor::Level(0))) {
            p.ground = l;
        }
        if let Some(l) = self.lines_where(|f| matches!(f, Floor::Typical | Floor::Any) || matches!(f, Floor::Level(n) if n > 0)) {
            p.typical = l;
        }
        if let Some(l) = self.lines_where(|f| f == Floor::Single) {
            p.single = l;
        }
        p
    }

    /// The weight two kinds want to be neighbours with, summed over both directions.
    pub fn adjacency(&self, a: RoomKind, b: RoomKind) -> f64 {
        let wish = |x: RoomKind, y: RoomKind| -> f64 {
            self.spaces
                .iter()
                .filter(|s| s.kind == x)
                .flat_map(|s| &s.adjacent_to)
                .filter(|(r, _)| r == y.key() || self.spaces.iter().any(|t| t.kind == y && t.answers(r)))
                .map(|(_, w)| *w)
                .sum()
        };
        wish(a, b) + if a == b { 0.0 } else { wish(b, a) }
    }
}
