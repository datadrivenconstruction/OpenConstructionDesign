//! Fit-out rules: library items with their plan bodies and free space, sets of items, kitchen
//! runs and the rule sets of the rooms. They are data (`data/furnish_rules.json`); a brief may
//! patch them with a `furnish` object.

use super::{RoomKind, RoomTags, Typology};
use crate::GFixture;
use serde_json::Value as J;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

pub(crate) const DEFAULT_RULES: &str = include_str!("../../data/furnish_rules.json");

/// x0, y0, x1, y1.
pub(crate) type R = [f64; 4];

pub(crate) const EPS: f64 = 1e-6;

/// Where the family's origin sits in its body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Org {
    /// Plan centre; local +y points to the back.
    Centre,
    /// Front edge centre; the body along local +y, to the back.
    Front,
    /// Front corner; the body along local +x and +y (to the back).
    Corner,
    /// On the wall face at the back; the body along local +y, into the room.
    Wall,
}

/// Where a piece's front looks in the frame of a wall: into the room, to the wall, or along it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Face {
    In,
    Out,
    Left,
    Right,
}

impl Face {
    pub fn mirrored(self) -> Face {
        match self {
            Face::Left => Face::Right,
            Face::Right => Face::Left,
            f => f,
        }
    }

    /// Body extent along and across the wall for a body `w` x `d`.
    pub fn extent(self, w: f64, d: f64) -> (f64, f64) {
        match self {
            Face::In | Face::Out => (w, d),
            Face::Left | Face::Right => (d, w),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Drain {
    /// Nominal size of the branch, m.
    pub dn: f64,
    /// Longest branch to the stack, m.
    pub max_run: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    pub id: String,
    pub fam: &'static str,
    pub ty: &'static str,
    pub ifc: String,
    /// Body along the wall it backs on, and into the room.
    pub w: f64,
    pub d: f64,
    pub org: Org,
    /// Free depth in front of the body.
    pub front: f64,
    /// Side bands: width, start from the back, how many must stay free.
    pub side: (f64, f64, u8),
    pub tall: bool,
    pub wet: bool,
    /// Mounted above the floor (grab rails, boards): only bodies and door leaves may not meet it.
    pub raised: bool,
    /// Sits on a piece of one of these items (a sink in its counter, a hood over its cooker).
    pub on: Vec<usize>,
    pub drain: Option<Drain>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Part {
    pub item: usize,
    /// Body corner nearest the set origin: along the wall, into the room.
    pub at: (f64, f64),
    pub face: Face,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Set {
    pub id: String,
    pub w: f64,
    pub d: f64,
    pub front: f64,
    /// Only in a room corner (mirrored at the far end).
    pub corner: bool,
    /// The floor of the set falls to its floor drain.
    pub fall: bool,
    pub parts: Vec<Part>,
}

impl Set {
    pub fn tall(&self, rules: &Rules) -> bool {
        self.parts.iter().any(|p| rules.items[p.item].tall)
    }

    /// The item the set is counted by: its first part.
    pub fn key(&self) -> usize {
        self.parts[0].item
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Slot {
    Module { set: usize, optional: bool },
    Gap(Vec<f64>),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Triangle {
    pub items: [usize; 3],
    pub leg: (f64, f64),
    pub sum: (f64, f64),
    /// Rooms from this area on must meet it.
    pub from_area: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Run {
    pub id: String,
    pub depth: f64,
    pub front: f64,
    pub turn: bool,
    pub tail: f64,
    /// Filler counters, widest first.
    pub fill: Vec<usize>,
    pub corner: Option<usize>,
    pub seqs: Vec<Vec<Slot>>,
    pub triangle: Option<Triangle>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Sides {
    All,
    Dark,
    Wet,
    Longest,
    Others,
    Of(usize),
    Opposite(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Count {
    N(u32),
    Per { item: usize, ratio: f64, min: u32 },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    Place {
        what: Vec<usize>,
        sides: Sides,
        centred: bool,
        count: Count,
        need: u32,
        once: bool,
        min_area: f64,
        /// Only in buildings of these typologies (any when empty).
        typologies: Vec<Typology>,
        /// The flag rule the step names is set.
        when: bool,
    },
    Choose(Vec<Vec<Step>>),
    /// A piece on or beside every placed piece of the `to` items: a hood on a cooker, wall
    /// cabinets on counters, a mirror on a basin, a holder beside a WC.
    Attach {
        item: usize,
        to: Vec<usize>,
        beside: bool,
        gap: f64,
        max: u32,
        need: u32,
    },
    Grid {
        set: usize,
        module: (f64, f64),
        start: f64,
        max: u32,
        rows: u32,
        sides: Sides,
        need: u32,
        min_area: f64,
        /// Floor area per person and persons per set: at most area / (a * n) sets.
        occupancy: Option<(f64, u32)>,
    },
    Run {
        run: usize,
        sides: Sides,
        need: u32,
    },
    Windows(Vec<usize>),
    Turning {
        d: f64,
        need: u32,
    },
    Bays {
        set: usize,
        stall: (f64, f64),
        aisle: f64,
        need: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Owner {
    Any,
    Dwelling,
    Public,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RoomRule {
    pub id: String,
    pub kinds: Vec<RoomKind>,
    pub owner: Owner,
    pub typologies: Vec<Typology>,
    pub min_area: f64,
    pub max_area: f64,
    pub min_width: f64,
    pub min_height: f64,
    /// Canonical tags a room needs (one of them); the spec sets them, whatever the language.
    pub tags: RoomTags,
    /// Words of a name: with `tags`, only a hint for a room the spec gave no tags; alone (an
    /// older patch), the condition itself.
    pub name_has: Vec<String>,
    pub priority: i64,
    pub steps: Vec<Step>,
}

/// What the rule matcher knows of a room.
pub(crate) struct RoomFacts<'a> {
    pub kind: RoomKind,
    pub dwelling: bool,
    pub typology: Typology,
    /// Clear floor area and the short side of the clear rectangle.
    pub area: f64,
    pub width: f64,
    pub height: f64,
    pub name: &'a str,
    pub tags: RoomTags,
}

impl RoomRule {
    pub fn matches(&self, f: &RoomFacts<'_>) -> bool {
        let name = f.name.to_lowercase();
        let named = || self.name_has.iter().any(|k| name.contains(k.as_str()));
        let tagged = if self.tags.is_empty() {
            self.name_has.is_empty() || named()
        } else {
            f.tags.meets(self.tags) || (f.tags.is_empty() && !self.name_has.is_empty() && named())
        };
        self.kinds.contains(&f.kind)
            && match self.owner {
                Owner::Any => true,
                Owner::Dwelling => f.dwelling,
                Owner::Public => !f.dwelling,
            }
            && (self.typologies.is_empty() || self.typologies.contains(&f.typology))
            && f.area + EPS >= self.min_area
            && f.area <= self.max_area + EPS
            && f.width + EPS >= self.min_width
            && f.height + EPS >= self.min_height
            && tagged
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Drainage {
    pub fall: f64,
    pub max_drop: f64,
    pub floor_fall: f64,
    pub max_floor_drop: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Rules {
    pub items: Vec<Item>,
    pub sets: Vec<Set>,
    /// Sets an item stands for when a step names it: its own zone with its side bands.
    pub item_sets: Vec<Vec<usize>>,
    pub runs: Vec<Run>,
    pub rooms: Vec<RoomRule>,
    pub drainage: Drainage,
    pub door_gap: f64,
    /// Wet rooms below this area have their door open outwards.
    pub outward_below: f64,
    by_type: HashMap<(&'static str, &'static str), usize>,
}

/// Family and type names live as long as the program: each distinct name is kept once.
pub(crate) fn intern(s: &str) -> &'static str {
    static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut pool = POOL
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(x) = pool.get(s) {
        return x;
    }
    let x: &'static str = Box::leak(s.to_owned().into_boxed_str());
    pool.insert(x);
    x
}

/// The default rules, or the default rules with the brief's patch.
pub(crate) fn rules_for(patch: Option<&J>) -> Result<Arc<Rules>, Vec<String>> {
    static DEFAULT: OnceLock<Arc<Rules>> = OnceLock::new();
    let resolved = |mut v: J| -> Result<Rules, Vec<String>> {
        let mut errs = vec![];
        resolve_rules(&mut v, "furnish", &mut errs);
        if !errs.is_empty() {
            return Err(errs);
        }
        Rules::parse(&v)
    };
    match patch {
        // the built-in rules once, while every `fit` rule has its built-in value
        None if fit_rules_builtin() => {
            if let Some(r) = DEFAULT.get() {
                return Ok(r.clone());
            }
            let r = Arc::new(resolved(default_json())?);
            Ok(DEFAULT.get_or_init(|| r).clone())
        }
        None => resolved(default_json()).map(Arc::new),
        Some(p) => {
            let mut v = default_json();
            super::merge_patch(&mut v, p);
            resolved(v).map(Arc::new)
        }
    }
}

/// Whether every `fit` rule of the rule set in scope has its built-in value.
fn fit_rules_builtin() -> bool {
    super::rules::defs()
        .iter()
        .filter(|d| d.group == "fit")
        .all(|d| d.default.is_none_or(|x| super::rules::rule(&d.id).to_bits() == x.to_bits()))
}

/// Values written as a rule id (`"fit.wc.front"`) read that rule of the set in scope, so every
/// clearance, threshold and count has its unit, range and reason in `rules.json`.
fn resolve_rules(v: &mut J, path: &str, errs: &mut Vec<String>) {
    match v {
        J::String(s) if s.starts_with("fit.") => match super::rules::def(s) {
            Some(_) => {
                let x = super::rules::rule(s);
                *v = if x.fract() == 0.0 && x.abs() < 1e15 { J::from(x as i64) } else { J::from(x) };
            }
            None => errs.push(format!("{path}: no rule '{s}' in the rule set")),
        },
        J::Array(a) => {
            for (i, x) in a.iter_mut().enumerate() {
                resolve_rules(x, &format!("{path}[{i}]"), errs);
            }
        }
        J::Object(o) => {
            for (k, x) in o.iter_mut().filter(|(k, _)| !matches!(k.as_str(), "about" | "note")) {
                resolve_rules(x, &format!("{path}.{k}"), errs);
            }
        }
        _ => {}
    }
}

fn default_json() -> J {
    serde_json::from_str(DEFAULT_RULES).expect("the built-in fit-out rules are JSON")
}

struct Rd {
    errs: Vec<String>,
}

impl Rd {
    fn bad(&mut self, path: &str, msg: impl Into<String>) {
        self.errs.push(format!("{path}: {}", msg.into()));
    }

    fn num(&mut self, path: &str, v: &J, def: Option<f64>, min: f64) -> f64 {
        match v.as_f64() {
            Some(x) if x.is_finite() && x >= min => x,
            Some(_) => {
                self.bad(path, format!("must be at least {min}"));
                min
            }
            None if v.is_null() && def.is_some() => def.unwrap_or(0.0),
            None => {
                self.bad(path, "must be a number");
                def.unwrap_or(min)
            }
        }
    }

    fn count(&mut self, path: &str, v: &J, def: u32) -> u32 {
        if v.is_null() {
            return def;
        }
        match v.as_u64() {
            Some(x) if x <= 10_000 => x as u32,
            _ => {
                self.bad(path, "must be a whole number up to 10000");
                def
            }
        }
    }

    fn pair(&mut self, path: &str, v: &J, min: f64) -> (f64, f64) {
        match v.as_array().map(|a| a.as_slice()) {
            Some([a, b]) => (
                self.num(&format!("{path}[0]"), a, None, min),
                self.num(&format!("{path}[1]"), b, None, min),
            ),
            _ => {
                self.bad(path, "must be [a, b]");
                (min, min)
            }
        }
    }

    fn flag(&mut self, path: &str, v: &J) -> bool {
        match v {
            J::Null => false,
            J::Bool(b) => *b,
            _ => {
                self.bad(path, "true or false");
                false
            }
        }
    }

    fn text(&mut self, path: &str, v: &J) -> String {
        match v.as_str() {
            Some(s) if !s.trim().is_empty() => s.to_string(),
            _ => {
                self.bad(path, "must be a non-empty text");
                String::new()
            }
        }
    }

    fn keys(&mut self, path: &str, v: &J, known: &[&str]) {
        let Some(o) = v.as_object() else {
            self.bad(path, "must be an object");
            return;
        };
        for k in o.keys() {
            if !known.contains(&k.as_str()) {
                self.bad(&format!("{path}.{k}"), format!("unknown key; known: {}", known.join(", ")));
            }
        }
    }
}

const ITEM_KEYS: [&str; 13] = [
    "family", "type", "ifc", "body", "org", "front", "side", "tall", "wet", "raised", "on",
    "drain", "note",
];

impl Rules {
    pub fn parse(v: &J) -> Result<Rules, Vec<String>> {
        let mut rd = Rd { errs: vec![] };
        rd.keys(
            "furnish",
            v,
            &["about", "door_gap", "outward_door_below", "drainage", "items", "sets", "runs", "rooms"],
        );
        let dr = &v["drainage"];
        rd.keys("furnish.drainage", dr, &["fall", "max_drop", "floor_fall", "max_floor_drop", "note"]);
        let drainage = Drainage {
            fall: rd.num("furnish.drainage.fall", &dr["fall"], None, 0.0),
            max_drop: rd.num("furnish.drainage.max_drop", &dr["max_drop"], None, 0.0),
            floor_fall: rd.num("furnish.drainage.floor_fall", &dr["floor_fall"], None, 0.0),
            max_floor_drop: rd.num("furnish.drainage.max_floor_drop", &dr["max_floor_drop"], None, 0.0),
        };
        let door_gap = rd.num("furnish.door_gap", &v["door_gap"], Some(0.15), 0.0);
        let outward_below = rd.num("furnish.outward_door_below", &v["outward_door_below"], Some(0.0), 0.0);

        // items: two passes, `on` may name an item further down
        let empty = serde_json::Map::new();
        let io = v["items"].as_object().unwrap_or_else(|| {
            rd.bad("furnish.items", "must be an object of items");
            &empty
        });
        let ids: Vec<&String> = io.keys().collect();
        let mut items = Vec::with_capacity(io.len());
        for (id, it) in io {
            let p = format!("furnish.items.{id}");
            rd.keys(&p, it, &ITEM_KEYS);
            let (w, d) = rd.pair(&format!("{p}.body"), &it["body"], 0.01);
            let org = match it["org"].as_str() {
                Some("centre") => Org::Centre,
                Some("front") => Org::Front,
                Some("corner") => Org::Corner,
                Some("wall") => Org::Wall,
                _ => {
                    rd.bad(&format!("{p}.org"), "one of centre, front, corner, wall");
                    Org::Centre
                }
            };
            let side = match &it["side"] {
                J::Null => (0.0, 0.0, 0),
                J::Array(a) if a.len() == 3 => {
                    let sw = rd.num(&format!("{p}.side[0]"), &a[0], None, 0.0);
                    let from = rd.num(&format!("{p}.side[1]"), &a[1], None, 0.0);
                    let n = rd.count(&format!("{p}.side[2]"), &a[2], 0);
                    if n > 2 {
                        rd.bad(&format!("{p}.side[2]"), "0, 1 or 2 bands");
                    }
                    if from >= d {
                        rd.bad(&format!("{p}.side[1]"), "must start within the body depth");
                    }
                    (sw, from, n.min(2) as u8)
                }
                _ => {
                    rd.bad(&format!("{p}.side"), "must be [width, from, how many free]");
                    (0.0, 0.0, 0)
                }
            };
            let hosts: Vec<J> = match &it["on"] {
                J::Null => vec![],
                J::Array(a) => a.clone(),
                x => vec![x.clone()],
            };
            let mut on = vec![];
            for h in &hosts {
                match h.as_str().and_then(|s| ids.iter().position(|k| *k == s)) {
                    Some(k) => on.push(k),
                    None => rd.bad(&format!("{p}.on"), format!("no item {h}; an item or a list of items")),
                }
            }
            let drain = match &it["drain"] {
                J::Null => None,
                dj => {
                    rd.keys(&format!("{p}.drain"), dj, &["dn", "max_run"]);
                    Some(Drain {
                        dn: rd.num(&format!("{p}.drain.dn"), &dj["dn"], None, 0.01),
                        max_run: rd.num(&format!("{p}.drain.max_run"), &dj["max_run"], None, 0.1),
                    })
                }
            };
            let ifc = rd.text(&format!("{p}.ifc"), &it["ifc"]);
            if !ifc.starts_with("Ifc") {
                rd.bad(&format!("{p}.ifc"), "an IFC class such as IfcFurniture");
            }
            items.push(Item {
                id: id.clone(),
                fam: intern(&rd.text(&format!("{p}.family"), &it["family"])),
                ty: intern(&rd.text(&format!("{p}.type"), &it["type"])),
                ifc,
                w,
                d,
                org,
                front: rd.num(&format!("{p}.front"), &it["front"], Some(0.0), 0.0),
                side,
                tall: rd.flag(&format!("{p}.tall"), &it["tall"]),
                wet: rd.flag(&format!("{p}.wet"), &it["wet"]),
                raised: rd.flag(&format!("{p}.raised"), &it["raised"]),
                on,
                drain,
            });
        }
        let mut by_type = HashMap::new();
        for (k, it) in items.iter().enumerate() {
            by_type.entry((it.fam, it.ty)).or_insert(k);
        }
        let item_ix = |s: &str| items.iter().position(|i| i.id == s);

        // the zone an item takes when a step names it
        let mut sets: Vec<Set> = vec![];
        let mut item_sets = vec![vec![]; items.len()];
        for (k, it) in items.iter().enumerate() {
            let (sw, _, n) = it.side;
            let mut variants: Vec<(f64, f64)> = vec![];
            match n {
                2 => variants.push((it.w + 2.0 * sw, sw)),
                1 => {
                    variants.push((it.w + 2.0 * sw, sw));
                    variants.push((it.w + sw, 0.0));
                    variants.push((it.w + sw, sw));
                }
                _ => variants.push((it.w, 0.0)),
            }
            for (w, u) in variants {
                item_sets[k].push(sets.len());
                sets.push(Set {
                    id: it.id.clone(),
                    w,
                    d: it.d,
                    front: it.front,
                    corner: false,
                    fall: false,
                    parts: vec![Part { item: k, at: (u, 0.0), face: Face::In }],
                });
            }
        }
        let so = v["sets"].as_object().cloned().unwrap_or_default();
        for (id, sj) in &so {
            let p = format!("furnish.sets.{id}");
            if item_ix(id).is_some() {
                rd.bad(&p, "an item has this name");
            }
            rd.keys(&p, sj, &["w", "d", "front", "corner", "fall", "parts", "note"]);
            let w = rd.num(&format!("{p}.w"), &sj["w"], None, 0.01);
            let d = rd.num(&format!("{p}.d"), &sj["d"], None, 0.01);
            let mut parts = vec![];
            let pa = sj["parts"].as_array().cloned().unwrap_or_default();
            if pa.is_empty() {
                rd.bad(&format!("{p}.parts"), "must list at least one part");
            }
            for (i, pj) in pa.iter().enumerate() {
                let pp = format!("{p}.parts[{i}]");
                rd.keys(&pp, pj, &["item", "at", "front"]);
                let name = rd.text(&format!("{pp}.item"), &pj["item"]);
                let Some(item) = item_ix(&name) else {
                    rd.bad(&format!("{pp}.item"), format!("no item '{name}'"));
                    continue;
                };
                let at = rd.pair(&format!("{pp}.at"), &pj["at"], 0.0);
                let face = match pj["front"].as_str() {
                    None | Some("in") => Face::In,
                    Some("out") => Face::Out,
                    Some("left") => Face::Left,
                    Some("right") => Face::Right,
                    Some(_) => {
                        rd.bad(&format!("{pp}.front"), "one of in, out, left, right");
                        Face::In
                    }
                };
                let it = &items[item];
                let (eu, ev) = face.extent(it.w, it.d);
                if at.0 + eu > w + 1e-4 || at.1 + ev > d + 1e-4 {
                    rd.bad(&pp, format!("outside the set zone {w} x {d}"));
                }
                parts.push(Part { item, at, face });
            }
            // parts may not overlap, except a piece sitting on its host
            for (i, a) in parts.iter().enumerate() {
                for b in &parts[..i] {
                    let ra = part_rect(&items, a);
                    let rb = part_rect(&items, b);
                    let seated = items[a.item].on.contains(&b.item) || items[b.item].on.contains(&a.item);
                    if hit(&ra, &rb) && !seated {
                        rd.bad(&format!("{p}.parts"), format!(
                            "{} and {} overlap",
                            items[a.item].id, items[b.item].id
                        ));
                    }
                }
            }
            for a in parts.iter().filter(|a| !items[a.item].on.is_empty()) {
                let hosts = &items[a.item].on;
                let ra = part_rect(&items, a);
                let held = parts.iter().any(|b| hosts.contains(&b.item) && inside(&ra, &part_rect(&items, b)));
                if !held {
                    rd.bad(&format!("{p}.parts"), format!(
                        "{} must sit within a {}",
                        items[a.item].id, items[hosts[0]].id
                    ));
                }
            }
            sets.push(Set {
                id: id.clone(),
                w,
                d,
                front: rd.num(&format!("{p}.front"), &sj["front"], Some(0.0), 0.0),
                corner: rd.flag(&format!("{p}.corner"), &sj["corner"]),
                fall: rd.flag(&format!("{p}.fall"), &sj["fall"]),
                parts,
            });
        }
        let named = |rd: &mut Rd, path: &str, s: &str| -> Vec<usize> {
            Ctx { items: &items, sets: &sets, runs: &[], item_sets: &item_sets }.named(rd, path, s)
        };

        let ro = v["runs"].as_object().cloned().unwrap_or_default();
        let mut runs = vec![];
        for (id, rj) in &ro {
            let p = format!("furnish.runs.{id}");
            rd.keys(&p, rj, &["depth", "front", "turn", "tail", "fill", "corner", "seqs", "triangle", "note"]);
            let mut fill: Vec<usize> = vec![];
            for (i, f) in rj["fill"].as_array().cloned().unwrap_or_default().iter().enumerate() {
                let fp = format!("{p}.fill[{i}]");
                let name = rd.text(&fp, f);
                match item_ix(&name) {
                    Some(k) => fill.push(k),
                    None => rd.bad(&fp, format!("no item '{name}'")),
                }
            }
            fill.sort_by(|a, b| items[*b].w.total_cmp(&items[*a].w));
            let corner = match rj["corner"].as_str() {
                None => None,
                Some(s) => {
                    let k = item_ix(s);
                    if k.is_none() {
                        rd.bad(&format!("{p}.corner"), format!("no item '{s}'"));
                    }
                    k
                }
            };
            let mut seqs = vec![];
            for (i, sq) in rj["seqs"].as_array().cloned().unwrap_or_default().iter().enumerate() {
                let mut seq = vec![];
                for (j, sl) in sq.as_array().cloned().unwrap_or_default().iter().enumerate() {
                    let sp = format!("{p}.seqs[{i}][{j}]");
                    if let Some(g) = sl["gap"].as_array() {
                        let gs: Vec<f64> = g
                            .iter()
                            .enumerate()
                            .map(|(n, x)| rd.num(&format!("{sp}.gap[{n}]"), x, None, 0.0))
                            .collect();
                        if gs.is_empty() {
                            rd.bad(&sp, "a gap lists at least one length");
                        }
                        seq.push(Slot::Gap(gs));
                        continue;
                    }
                    rd.keys(&sp, sl, &["set", "item", "optional"]);
                    let name = sl["set"].as_str().or(sl["item"].as_str()).unwrap_or("");
                    let k = named(&mut rd, &sp, name);
                    if let Some(&set) = k.first() {
                        seq.push(Slot::Module { set, optional: rd.flag(&format!("{sp}.optional"), &sl["optional"]) });
                    }
                }
                if !seq.iter().any(|s| matches!(s, Slot::Module { optional: false, .. })) {
                    rd.bad(&format!("{p}.seqs[{i}]"), "needs a module that is not optional");
                }
                seqs.push(seq);
            }
            if seqs.is_empty() {
                rd.bad(&format!("{p}.seqs"), "must list at least one sequence");
            }
            let triangle = match &rj["triangle"] {
                J::Null => None,
                t => {
                    let tp = format!("{p}.triangle");
                    rd.keys(&tp, t, &["items", "leg", "sum", "from_area"]);
                    let mut ks = [0usize; 3];
                    let names = t["items"].as_array().cloned().unwrap_or_default();
                    if names.len() != 3 {
                        rd.bad(&format!("{tp}.items"), "three items");
                    }
                    for (n, x) in names.iter().take(3).enumerate() {
                        let s = x.as_str().unwrap_or("");
                        match item_ix(s) {
                            Some(k) => ks[n] = k,
                            None => rd.bad(&format!("{tp}.items[{n}]"), format!("no item '{s}'")),
                        }
                    }
                    Some(Triangle {
                        items: ks,
                        leg: rd.pair(&format!("{tp}.leg"), &t["leg"], 0.0),
                        sum: rd.pair(&format!("{tp}.sum"), &t["sum"], 0.0),
                        from_area: rd.num(&format!("{tp}.from_area"), &t["from_area"], Some(0.0), 0.0),
                    })
                }
            };
            runs.push(Run {
                id: id.clone(),
                depth: rd.num(&format!("{p}.depth"), &rj["depth"], None, 0.3),
                front: rd.num(&format!("{p}.front"), &rj["front"], Some(0.9), 0.0),
                turn: rd.flag(&format!("{p}.turn"), &rj["turn"]),
                tail: rd.num(&format!("{p}.tail"), &rj["tail"], Some(0.0), 0.0),
                fill,
                corner,
                seqs,
                triangle,
            });
        }

        let mut rooms = vec![];
        for (p, id, rj) in room_list(&mut rd, &v["rooms"]) {
            let rj = &rj;
            rd.keys(
                &p,
                rj,
                &[
                    "id", "order", "kind", "owner", "typology", "min_area", "max_area", "min_width",
                    "min_height", "tags", "name_has", "priority", "steps", "note",
                ],
            );
            let kinds_j: Vec<J> = match &rj["kind"] {
                J::Array(a) => a.clone(),
                k => vec![k.clone()],
            };
            let mut kinds = vec![];
            for k in &kinds_j {
                match k.as_str().and_then(super::kinds::parse).map(|x| x.0) {
                    Some(x) => kinds.push(x),
                    None => rd.bad(&format!("{p}.kind"), format!("one of {}", super::kinds::words().join(", "))),
                }
            }
            let owner = match rj["owner"].as_str() {
                None => Owner::Any,
                Some("dwelling") => Owner::Dwelling,
                Some("public") => Owner::Public,
                Some(_) => {
                    rd.bad(&format!("{p}.owner"), "dwelling or public");
                    Owner::Any
                }
            };
            let mut typologies = vec![];
            for t in rj["typology"].as_array().cloned().unwrap_or_default() {
                match Typology::ALL.into_iter().find(|x| Some(x.name()) == t.as_str()) {
                    Some(x) => typologies.push(x),
                    None => rd.bad(&format!("{p}.typology"), format!("one of {}", Typology::NAMES.join(", "))),
                }
            }
            let name_has = rj["name_has"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str()).map(|s| s.to_lowercase()).collect())
                .unwrap_or_default();
            let mut tags = RoomTags::default();
            for t in rj["tags"].as_array().cloned().unwrap_or_default() {
                match t.as_str().and_then(RoomTags::parse) {
                    Some(x) => tags = tags.with(x),
                    None => rd.bad(&format!("{p}.tags"), format!("one of {}", RoomTags::IDS.join(", "))),
                }
            }
            let priority = match &rj["priority"] {
                J::Null => 1,
                x => x.as_i64().unwrap_or_else(|| {
                    rd.bad(&format!("{p}.priority"), "a whole number");
                    1
                }),
            };
            let ctx = Ctx { items: &items, sets: &sets, runs: &runs, item_sets: &item_sets };
            let steps = parse_steps(&mut rd, &ctx, &format!("{p}.steps"), &rj["steps"]);
            rooms.push(RoomRule {
                id,
                kinds,
                owner,
                typologies,
                min_area: rd.num(&format!("{p}.min_area"), &rj["min_area"], Some(0.0), 0.0),
                max_area: rd.num(&format!("{p}.max_area"), &rj["max_area"], Some(f64::MAX), 0.0),
                min_width: rd.num(&format!("{p}.min_width"), &rj["min_width"], Some(0.0), 0.0),
                min_height: rd.num(&format!("{p}.min_height"), &rj["min_height"], Some(0.0), 0.0),
                tags,
                name_has,
                priority,
                steps,
            });
        }
        if !rd.errs.is_empty() {
            return Err(rd.errs);
        }
        Ok(Rules { items, sets, item_sets, runs, rooms, drainage, door_gap, outward_below, by_type })
    }

    /// The rule item a placed fixture stands for.
    pub fn item_of(&self, f: &GFixture) -> Option<&Item> {
        self.by_type.get(&(f.family, f.ty)).map(|&k| &self.items[k])
    }

    pub fn item_ix(&self, id: &str) -> Option<usize> {
        self.items.iter().position(|i| i.id == id)
    }

    pub fn rule(&self, id: &str) -> Option<&RoomRule> {
        self.rooms.iter().find(|r| r.id == id)
    }
}

/// Room rule sets in matching order: (error path, id, rule). The rules are an object by id with
/// an `order` each, so a patch changes or removes (`null`) one rule; a list (the older form, or
/// a patch that replaces them all) keeps its own order.
fn room_list(rd: &mut Rd, v: &J) -> Vec<(String, String, J)> {
    let mut out: Vec<(f64, String, String, J)> = vec![];
    match v {
        J::Null => {}
        J::Object(o) => {
            for (id, rj) in o.iter().filter(|(_, r)| !r.is_null()) {
                let p = format!("furnish.rooms.{id}");
                if !rj.is_object() {
                    rd.bad(&p, "a room rule set object, or null to remove it");
                    continue;
                }
                if rj.get("id").is_some_and(|x| x.as_str() != Some(id.as_str())) {
                    rd.bad(&format!("{p}.id"), "the key is the id");
                }
                let order = rd.num(&format!("{p}.order"), &rj["order"], Some(ORDER_LAST), f64::MIN);
                out.push((order, id.clone(), p, rj.clone()));
            }
        }
        J::Array(a) => {
            let mut seen = HashSet::new();
            for (i, rj) in a.iter().enumerate() {
                let p = format!("furnish.rooms[{i}]");
                let id = rd.text(&format!("{p}.id"), &rj["id"]);
                if !seen.insert(id.clone()) {
                    rd.bad(&format!("{p}.id"), format!("'{id}' is used twice"));
                }
                let def = (i + 1) as f64 * 10.0;
                let order = rd.num(&format!("{p}.order"), &rj["order"], Some(def), f64::MIN);
                out.push((order, id, p, rj.clone()));
            }
        }
        _ => rd.bad("furnish.rooms", "an object of room rule sets by id"),
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    out.into_iter().map(|(_, id, p, rj)| (p, id, rj)).collect()
}

/// Order of a room rule set that names none: after all that do.
const ORDER_LAST: f64 = 1e6;

struct Ctx<'a> {
    items: &'a [Item],
    sets: &'a [Set],
    runs: &'a [Run],
    item_sets: &'a [Vec<usize>],
}

impl Ctx<'_> {
    fn named(&self, rd: &mut Rd, path: &str, s: &str) -> Vec<usize> {
        let item = self.items.iter().position(|i| i.id == s);
        if item.is_none() {
            if let Some(k) = self.sets.iter().position(|x| x.id == s) {
                return vec![k];
            }
        }
        match item {
            Some(i) => self.item_sets[i].clone(),
            None => {
                rd.bad(path, format!("no item or set '{s}'"));
                vec![]
            }
        }
    }

    fn item(&self, rd: &mut Rd, path: &str, s: &str) -> usize {
        self.items.iter().position(|i| i.id == s).unwrap_or_else(|| {
            rd.bad(path, format!("no item '{s}'"));
            0
        })
    }

    fn sides(&self, rd: &mut Rd, path: &str, v: &J) -> Sides {
        match v.as_str() {
            None => {
                if !v.is_null() {
                    rd.bad(path, "a side selector");
                }
                Sides::All
            }
            Some("all") => Sides::All,
            Some("dark") => Sides::Dark,
            Some("wet") => Sides::Wet,
            Some("longest") => Sides::Longest,
            Some("others") => Sides::Others,
            Some(s) => {
                if let Some(x) = s.strip_prefix("of:") {
                    Sides::Of(self.item(rd, path, x))
                } else if let Some(x) = s.strip_prefix("opposite:") {
                    Sides::Opposite(self.item(rd, path, x))
                } else {
                    rd.bad(path, "all, dark, wet, longest, others, of:<item> or opposite:<item>");
                    Sides::All
                }
            }
        }
    }
}

const VERBS: [&str; 8] = ["place", "choose", "grid", "run", "windows", "turning", "bays", "attach"];

fn parse_steps(rd: &mut Rd, cx: &Ctx<'_>, path: &str, v: &J) -> Vec<Step> {
    let Some(a) = v.as_array() else {
        rd.bad(path, "must be a list of steps");
        return vec![];
    };
    let mut out = vec![];
    for (i, s) in a.iter().enumerate() {
        let p = format!("{path}[{i}]");
        let verbs: Vec<&str> = VERBS.iter().copied().filter(|k| !s[*k].is_null()).collect();
        if verbs.len() != 1 {
            rd.bad(&p, format!("one of {} per step", VERBS.join(", ")));
            continue;
        }
        let need = rd.count(&format!("{p}.need"), &s["need"], 0);
        let min_area = rd.num(&format!("{p}.min_area"), &s["min_area"], Some(0.0), 0.0);
        let sides = cx.sides(rd, &format!("{p}.sides"), &s["sides"]);
        let step = match verbs[0] {
            "place" => {
                rd.keys(
                    &p,
                    s,
                    &["place", "sides", "centred", "count", "per", "need", "once", "min_area", "typology", "when"],
                );
                let mut typologies = vec![];
                for t in s["typology"].as_array().cloned().unwrap_or_default() {
                    match Typology::ALL.into_iter().find(|x| Some(x.name()) == t.as_str()) {
                        Some(x) => typologies.push(x),
                        None => rd.bad(&format!("{p}.typology"), format!("one of {}", Typology::NAMES.join(", "))),
                    }
                }
                // a flag rule, read when the rules are read
                let when = s["when"].is_null() || rd.num(&format!("{p}.when"), &s["when"], None, 0.0) > 0.5;
                let names: Vec<String> = match &s["place"] {
                    J::Array(xs) => xs.iter().filter_map(|x| x.as_str()).map(String::from).collect(),
                    x => x.as_str().map(|s| vec![s.to_string()]).unwrap_or_default(),
                };
                if names.is_empty() {
                    rd.bad(&format!("{p}.place"), "an item or set, or a list of them");
                }
                let what: Vec<usize> =
                    names.iter().flat_map(|n| cx.named(rd, &format!("{p}.place"), n)).collect();
                let count = if s["per"].is_null() {
                    Count::N(rd.count(&format!("{p}.count"), &s["count"], 1))
                } else {
                    let pj = &s["per"];
                    rd.keys(&format!("{p}.per"), pj, &["item", "ratio", "min"]);
                    let name = pj["item"].as_str().unwrap_or("");
                    Count::Per {
                        item: cx.item(rd, &format!("{p}.per.item"), name),
                        ratio: rd.num(&format!("{p}.per.ratio"), &pj["ratio"], None, 0.0),
                        min: rd.count(&format!("{p}.per.min"), &pj["min"], 0),
                    }
                };
                let once = match s["once"].as_str() {
                    None if s["once"].is_null() => false,
                    Some("dwelling") => true,
                    _ => {
                        rd.bad(&format!("{p}.once"), "only 'dwelling'");
                        false
                    }
                };
                Step::Place {
                    what,
                    sides,
                    centred: rd.flag(&format!("{p}.centred"), &s["centred"]),
                    count,
                    need,
                    once,
                    min_area,
                    typologies,
                    when,
                }
            }
            "attach" => {
                rd.keys(&p, s, &["attach", "to", "at", "gap", "max", "need"]);
                let name = s["attach"].as_str().unwrap_or("");
                let item = cx.item(rd, &format!("{p}.attach"), name);
                let to: Vec<usize> = s["to"]
                    .as_array()
                    .map(|a| a.iter().map(|x| cx.item(rd, &format!("{p}.to"), x.as_str().unwrap_or(""))).collect())
                    .unwrap_or_default();
                if to.is_empty() {
                    rd.bad(&format!("{p}.to"), "a list of the items it goes with");
                }
                let beside = match s["at"].as_str() {
                    Some("on") => false,
                    Some("beside") => true,
                    _ => {
                        rd.bad(&format!("{p}.at"), "on or beside");
                        false
                    }
                };
                if !beside {
                    for &h in &to {
                        if !cx.items[item].on.contains(&h) {
                            rd.bad(&format!("{p}.to"), format!("{} does not sit on {}", name, cx.items[h].id));
                        }
                    }
                }
                Step::Attach {
                    item,
                    to,
                    beside,
                    gap: rd.num(&format!("{p}.gap"), &s["gap"], Some(0.0), 0.0),
                    max: rd.count(&format!("{p}.max"), &s["max"], 10_000),
                    need,
                }
            }
            "choose" => {
                rd.keys(&p, s, &["choose"]);
                let opts = s["choose"].as_array().cloned().unwrap_or_default();
                if opts.is_empty() {
                    rd.bad(&format!("{p}.choose"), "a list of options, each a list of steps");
                }
                Step::Choose(
                    opts.iter()
                        .enumerate()
                        .map(|(k, o)| parse_steps(rd, cx, &format!("{p}.choose[{k}]"), o))
                        .collect(),
                )
            }
            "grid" => {
                rd.keys(&p, s, &["grid", "module", "start", "max", "rows", "sides", "need", "min_area", "occupancy"]);
                let occupancy = match &s["occupancy"] {
                    J::Null => None,
                    o => {
                        let (a, n) = rd.pair(&format!("{p}.occupancy"), o, 0.1);
                        if n.fract() != 0.0 {
                            rd.bad(&format!("{p}.occupancy[1]"), "persons per set, a whole number");
                        }
                        Some((a, n as u32))
                    }
                };
                let name = s["grid"].as_str().unwrap_or("");
                let set = cx.named(rd, &format!("{p}.grid"), name).first().copied().unwrap_or(0);
                let module = rd.pair(&format!("{p}.module"), &s["module"], 0.1);
                if let Some(st) = cx.sets.get(set) {
                    if st.w > module.0 + EPS || st.d > module.1 + EPS {
                        rd.bad(&format!("{p}.module"), format!("smaller than {} ({} x {})", st.id, st.w, st.d));
                    }
                }
                Step::Grid {
                    set,
                    module,
                    start: rd.num(&format!("{p}.start"), &s["start"], Some(0.0), 0.0),
                    max: rd.count(&format!("{p}.max"), &s["max"], 1),
                    rows: rd.count(&format!("{p}.rows"), &s["rows"], 10_000),
                    sides,
                    need,
                    min_area,
                    occupancy,
                }
            }
            "run" => {
                rd.keys(&p, s, &["run", "sides", "need"]);
                let name = s["run"].as_str().unwrap_or("");
                let run = cx.runs.iter().position(|r| r.id == name).unwrap_or_else(|| {
                    rd.bad(&format!("{p}.run"), format!("no run '{name}'"));
                    0
                });
                Step::Run { run, sides, need }
            }
            "windows" => {
                rd.keys(&p, s, &["windows"]);
                let names: Vec<String> = s["windows"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str()).map(String::from).collect())
                    .unwrap_or_default();
                let mut what: Vec<usize> =
                    names.iter().flat_map(|n| cx.named(rd, &format!("{p}.windows"), n)).collect();
                what.sort_by(|a, b| cx.sets[*b].w.total_cmp(&cx.sets[*a].w));
                Step::Windows(what)
            }
            "turning" => {
                rd.keys(&p, s, &["turning", "need"]);
                Step::Turning {
                    d: rd.num(&format!("{p}.turning"), &s["turning"], None, 0.5),
                    need,
                }
            }
            _ => {
                rd.keys(&p, s, &["bays", "stall", "aisle", "need"]);
                let name = s["bays"].as_str().unwrap_or("");
                let set = cx.named(rd, &format!("{p}.bays"), name).first().copied().unwrap_or(0);
                Step::Bays {
                    set,
                    stall: rd.pair(&format!("{p}.stall"), &s["stall"], 0.5),
                    aisle: rd.num(&format!("{p}.aisle"), &s["aisle"], None, 0.0),
                    need,
                }
            }
        };
        out.push(step);
    }
    out
}

// ---- geometry shared by the placer and the audit ----

pub(crate) fn hit(a: &R, b: &R) -> bool {
    a[0] < b[2] - EPS && b[0] < a[2] - EPS && a[1] < b[3] - EPS && b[1] < a[3] - EPS
}

pub(crate) fn inside(a: &R, room: &R) -> bool {
    a[0] >= room[0] - EPS && a[1] >= room[1] - EPS && a[2] <= room[2] + EPS && a[3] <= room[3] + EPS
}

/// A part's body in its set frame.
fn part_rect(items: &[Item], p: &Part) -> R {
    let it = &items[p.item];
    let (eu, ev) = p.face.extent(it.w, it.d);
    [p.at.0, p.at.1, p.at.0 + eu, p.at.1 + ev]
}

/// The body in the family's own frame.
fn local_body(it: &Item) -> R {
    let (w, d) = (it.w, it.d);
    match it.org {
        Org::Centre => [-w / 2.0, -d / 2.0, w / 2.0, d / 2.0],
        Org::Front | Org::Wall => [-w / 2.0, 0.0, w / 2.0, d],
        Org::Corner => [0.0, 0.0, w, d],
    }
}

/// Local +y in the plan for a piece whose front looks along `front`.
fn y_axis(org: Org, front: (f64, f64)) -> (f64, f64) {
    if org == Org::Wall {
        front
    } else {
        (-front.0, -front.1)
    }
}

/// Origin and turn of a family whose body is `body` in the plan, front along `front`.
pub(crate) fn fixture_at(it: &Item, body: R, front: (f64, f64)) -> ((f64, f64), f64) {
    let y = y_axis(it.org, front);
    let x = (y.1, -y.0);
    let lb = local_body(it);
    let (cx, cy) = ((lb[0] + lb[2]) / 2.0, (lb[1] + lb[3]) / 2.0);
    let c = ((body[0] + body[2]) / 2.0, (body[1] + body[3]) / 2.0);
    let at = (c.0 - x.0 * cx - y.0 * cy, c.1 - x.1 * cx - y.1 * cy);
    (at, (-y.0).atan2(y.1))
}

/// A placed piece read back from its fixture: body, front direction and the plan frame.
pub(crate) struct Placed {
    pub body: R,
    pub front: (f64, f64),
    x: (f64, f64),
    y: (f64, f64),
    at: (f64, f64),
}

impl Placed {
    pub fn of(it: &Item, f: &GFixture) -> Placed {
        let (s, c) = f.rotation.sin_cos();
        let x = (c, s);
        let y = (-s, c);
        let front = if it.org == Org::Wall { y } else { (-y.0, -y.1) };
        let mut p = Placed { body: [0.0; 4], front, x, y, at: f.at };
        p.body = p.world(local_body(it));
        p
    }

    fn world(&self, r: R) -> R {
        let pts = [(r[0], r[1]), (r[2], r[1]), (r[2], r[3]), (r[0], r[3])];
        let mut o = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for (u, v) in pts {
            let px = self.at.0 + self.x.0 * u + self.y.0 * v;
            let py = self.at.1 + self.x.1 * u + self.y.1 * v;
            o = [o[0].min(px), o[1].min(py), o[2].max(px), o[3].max(py)];
        }
        o
    }

    /// The free space in front of the body, if the item keeps any.
    pub fn front_band(&self, it: &Item) -> Option<R> {
        if it.front <= 0.0 {
            return None;
        }
        let b = local_body(it);
        let f = it.front;
        let r = match it.org {
            Org::Wall => [b[0], b[3], b[2], b[3] + f],
            _ => [b[0], b[1] - f, b[2], b[1]],
        };
        Some(self.world(r))
    }

    /// The two side bands (left, right of the body), if the item keeps any.
    pub fn side_bands(&self, it: &Item) -> Vec<R> {
        let (sw, from, n) = it.side;
        if n == 0 || sw <= 0.0 {
            return vec![];
        }
        let b = local_body(it);
        let (y0, y1) = match it.org {
            Org::Wall => (b[1] + from, b[3]),
            _ => (b[1], b[3] - from),
        };
        vec![self.world([b[0] - sw, y0, b[0], y1]), self.world([b[2], y0, b[2] + sw, y1])]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(org: Org) -> Item {
        Item {
            id: "x".into(),
            fam: "F",
            ty: "T",
            ifc: "IfcFurniture".into(),
            w: 1.2,
            d: 0.6,
            org,
            front: 0.5,
            side: (0.2, 0.1, 2),
            tall: false,
            wet: false,
            raised: false,
            on: vec![],
            drain: None,
        }
    }

    #[test]
    fn a_fixture_reads_back_the_body_and_front_it_was_placed_with() {
        let body = [2.0, 3.0, 3.2, 3.6];
        for org in [Org::Centre, Org::Front, Org::Corner, Org::Wall] {
            for front in [(0.0, 1.0), (0.0, -1.0), (1.0, 0.0), (-1.0, 0.0)] {
                let it = item(org);
                let b = if front.0 == 0.0 { body } else { [2.0, 3.0, 2.6, 4.2] };
                let (at, rotation) = fixture_at(&it, b, front);
                let f = GFixture { scale: 1.0, min_clear: 0.0, family: "F", ty: "T", at, rotation, wall: None, z: 0.0 };
                let p = Placed::of(&it, &f);
                for k in 0..4 {
                    assert!((p.body[k] - b[k]).abs() < 1e-9, "{org:?} {front:?}: {:?}", p.body);
                }
                assert!((p.front.0 - front.0).abs() < 1e-9 && (p.front.1 - front.1).abs() < 1e-9);
                // the free space sits on the front side, flush with the body
                let fb = p.front_band(&it).unwrap();
                let c = ((fb[0] + fb[2]) / 2.0 - (b[0] + b[2]) / 2.0, (fb[1] + fb[3]) / 2.0 - (b[1] + b[3]) / 2.0);
                assert!(c.0 * front.0 + c.1 * front.1 > 0.5, "{org:?} {front:?}");
                assert!(!hit(&fb, &b));
                for sb in p.side_bands(&it) {
                    assert!(!hit(&sb, &b));
                }
            }
        }
    }

    #[test]
    fn the_built_in_rules_parse() {
        let r = rules_for(None).unwrap_or_else(|e| panic!("{e:?}"));
        assert!(r.items.len() >= 40 && r.rooms.len() >= 20);
        assert!(r.item_of(&GFixture { scale: 1.0, min_clear: 0.0, family: "WC", ty: "Floor 650", at: (0.0, 0.0), rotation: 0.0, wall: None, z: 0.0 }).is_some());
    }

    #[test]
    fn broken_rules_say_where() {
        let bad = |p: J| rules_for(Some(&p)).err().unwrap_or_default().join("; ");
        let e = bad(serde_json::json!({"items": {"wc": {"body": [-1, 0.5]}}}));
        assert!(e.contains("furnish.items.wc.body[0]"), "{e}");
        let e = bad(serde_json::json!({"sets": {"x": {"w": 1, "d": 1, "parts": [{"item": "nope", "at": [0, 0]}]}}}));
        assert!(e.contains("furnish.sets.x.parts[0].item"), "{e}");
        let e = bad(serde_json::json!({"rooms": [{"id": "a", "kind": "attic", "steps": []}]}));
        assert!(e.contains("furnish.rooms[0].kind"), "{e}");
        let e = bad(serde_json::json!({"rooms": [{"id": "a", "kind": "wc", "steps": [{"place": "wc", "grid": "wc"}]}]}));
        assert!(e.contains("furnish.rooms[0].steps[0]"), "{e}");
        let e = bad(serde_json::json!({"sets": {"y": {"w": 1, "d": 1, "parts": [{"item": "bed2", "at": [0, 0]}]}}}));
        assert!(e.contains("outside the set zone"), "{e}");
        let e = bad(serde_json::json!({"rooms": {"wc": {"kind": "attic"}}}));
        assert!(e.contains("furnish.rooms.wc.kind"), "{e}");
        let e = bad(serde_json::json!({"rooms": {"wc": {"order": "first"}}}));
        assert!(e.contains("furnish.rooms.wc.order"), "{e}");
    }

    #[test]
    fn a_patch_changes_one_room_rule_by_id() {
        let ids = |r: &Rules| r.rooms.iter().map(|x| x.id.clone()).collect::<Vec<_>>();
        let base = rules_for(None).unwrap();
        // one rule changed: the others stay, in their order
        let p = rules_for(Some(&serde_json::json!({"rooms": {"wc": {"min_area": 2.0}}}))).unwrap();
        assert_eq!(ids(&p), ids(&base));
        assert_eq!(p.rule("wc").unwrap().min_area, 2.0);
        assert_eq!(p.rule("wc").unwrap().steps, base.rule("wc").unwrap().steps);
        assert_eq!(p.rule("bath"), base.rule("bath"));
        // removed with null, moved by its order, a new one slotted in by its order
        let p = rules_for(Some(&serde_json::json!({"rooms": {
            "wc_block": null,
            "parking": {"order": 0},
            "sauna": {"order": 15, "kind": "bath", "steps": [{"place": "shower", "need": 1}]}}})))
        .unwrap();
        let got = ids(&p);
        assert!(!got.contains(&"wc_block".to_string()));
        assert_eq!(got[0], "parking");
        let at = |id: &str| got.iter().position(|x| x == id).unwrap_or_else(|| panic!("{id} in {got:?}"));
        assert_eq!(at("sauna"), at("bath_accessible") + 1, "order 15 sits between orders 10 and 20: {got:?}");
        assert_eq!(at("bath_full"), at("sauna") + 1, "{got:?}");
        assert_eq!(got.len(), base.rooms.len() + 1 - 1);
        // the older list form replaces them all and keeps its own order
        let p = rules_for(Some(&serde_json::json!({"rooms": [
            {"id": "b", "kind": "wc", "steps": []}, {"id": "a", "kind": "wc", "steps": []}]})))
        .unwrap();
        assert_eq!(ids(&p), ["b", "a"]);
    }
}
