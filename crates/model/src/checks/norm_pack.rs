// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Country norm packs: data folders `<id>/pack.json`, `sources.json`, `rules/*.json`,
//! `programme/*.json`, `assemblies.json`, `dims.json`, `drawing.json`, `vocab.json`. A pack
//! names its parent (`generic` -> `eu` -> `de` -> `de-by`); resolving merges the chain, a child
//! entry replacing the parent's one with the same id (`"disabled": true` drops it). Numbers are
//! SI. Every source carries its licence; a number not checked against its source
//! (`verified: false`) is advice and never a hard finding.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Map, Value as J};

use super::norm_kinds::{parse_kind, NormKind};
use super::norms::NormRule;
use super::{rule_from, Severity};
use crate::Error;

/// How a source may be shipped: law text is free, a paid standard gives numbers and the
/// clause only, `own` is our own default.
pub const LICENCES: [&str; 5] = ["public-law", "ogl", "open", "copyright-numbers-only", "own"];
pub const STATUSES: [&str; 5] = ["generic", "draft", "tier1", "tier2", "tier3"];
/// Check kinds that run through the company-rule engine.
pub const LEGACY_KINDS: [&str; 9] = [
    "param_present",
    "param_range",
    "param_in",
    "count",
    "clearance",
    "unique",
    "param_value",
    "cases",
    "compare",
];
pub const NEW_KINDS: [&str; 6] = ["formula", "ratio", "adjacency", "stack", "distance", "programme"];
/// Unit systems a pack may display in; numbers are stored SI either way.
pub const DISPLAY_SYSTEMS: [&str; 2] = ["metric", "imperial"];
/// What a stated dwelling or programme area measures: outside of the walls, inside of the
/// outer walls, the rooms only, or the rooms less some kinds (balconies, stores, shafts).
pub const AREA_STANDARDS: [&str; 4] = ["gross", "gross_internal", "net", "usable"];
/// Keys `applies` may hold, on a rule or a dimension.
pub const APPLIES_KEYS: [&str; 8] =
    ["typology", "room_kind", "region", "min_floors", "max_floors", "sprinklered", "height_min", "height_max"];
/// Field names that would carry the text of a standard; packs store numbers and our own words.
const FORBIDDEN_KEYS: [&str; 4] = ["quote", "verbatim", "excerpt", "source_text"];
/// A paraphrase is one short sentence of our own.
pub const PARAPHRASE_MAX: usize = 200;

/// Units a pack may state and the plausible range of a value in each (catches mm typed as m).
fn unit_range(unit: &str) -> Option<(f64, f64)> {
    match unit {
        "m" => Some((0.0, 500.0)),
        "m2" => Some((0.0, 100_000.0)),
        "m3" => Some((0.0, 10_000_000.0)),
        "count" => Some((0.0, 1_000_000.0)),
        "ratio" => Some((0.0, 1.0)),
        "h" => Some((0.0, 24.0)),
        "min" => Some((0.0, 1440.0)),
        "W/m2K" => Some((0.0, 10.0)),
        "dB" => Some((0.0, 120.0)),
        "kN/m2" => Some((0.0, 100.0)),
        "C" => Some((-60.0, 60.0)),
        "m3/h" => Some((0.0, 500.0)),
        "m3/h m2" => Some((0.0, 50.0)),
        "" => Some((f64::MIN, f64::MAX)),
        _ => None,
    }
}

/// SI unit and factor of a unit a source may state a number in (`orig` of a pack value).
pub fn si_of(unit: &str) -> Option<(&'static str, f64)> {
    match unit {
        "m" => Some(("m", 1.0)),
        "mm" => Some(("m", 0.001)),
        "cm" => Some(("m", 0.01)),
        "in" => Some(("m", 0.0254)),
        "ft" => Some(("m", 0.3048)),
        "m2" => Some(("m2", 1.0)),
        "ft2" => Some(("m2", 0.09290304)),
        "kN/m2" | "kPa" => Some(("kN/m2", 1.0)),
        "kgf/m2" => Some(("kN/m2", 0.00980665)),
        "psf" => Some(("kN/m2", 0.04788026)),
        "ratio" => Some(("ratio", 1.0)),
        "%" => Some(("ratio", 0.01)),
        "count" => Some(("count", 1.0)),
        "h" => Some(("h", 1.0)),
        "C" => Some(("C", 1.0)),
        "m3/h" => Some(("m3/h", 1.0)),
        "l/s" => Some(("m3/h", 3.6)),
        "cfm" => Some(("m3/h", 1.699011)),
        "m3/h m2" => Some(("m3/h m2", 1.0)),
        "l/s m2" => Some(("m3/h m2", 3.6)),
        "cfm/ft2" => Some(("m3/h m2", 18.288)),
        "min" => Some(("min", 1.0)),
        _ => None,
    }
}

/// A stored SI number in the unit system the person chose: metric keeps it, imperial gives
/// feet, square feet or pounds per square foot (inches below a metre).
pub fn display_value(si: f64, unit: &str, system: &str) -> (f64, &'static str) {
    let imperial = system == "imperial";
    match unit {
        "m" if imperial && si < 1.0 => (si / 0.0254, "in"),
        "m" if imperial => (si / 0.3048, "ft"),
        "m2" if imperial => (si / 0.09290304, "ft2"),
        "kN/m2" if imperial => (si / 0.04788026, "psf"),
        "C" if imperial => (si * 1.8 + 32.0, "F"),
        "m3/h" if imperial => (si / 1.699011, "cfm"),
        "m3/h m2" if imperial => (si / 18.288, "cfm/ft2"),
        "C" => (si, "C"),
        "m3/h" => (si, "m3/h"),
        "m3/h m2" => (si, "m3/h m2"),
        "m" => (si, "m"),
        "m2" => (si, "m2"),
        "kN/m2" => (si, "kN/m2"),
        "ratio" => (si, "ratio"),
        "count" => (si, "count"),
        "h" => (si, "h"),
        _ => (si, ""),
    }
}

/// `None` when `orig` (`{value, unit}` as the source states it) converts to `si` in `unit`.
fn orig_problem(orig: &J, si: &J, unit: &str) -> Option<String> {
    let ou = orig["unit"].as_str().unwrap_or("");
    let Some((to, k)) = si_of(ou) else {
        return Some(format!("orig: unknown unit {ou:?}"));
    };
    if to != unit {
        return Some(format!("orig: {ou} is not a {unit} unit"));
    }
    let pairs: Vec<(Option<f64>, Option<f64>)> = match (&orig["value"], si) {
        (J::Array(a), J::Array(b)) if a.len() == b.len() => a.iter().zip(b).map(|(x, y)| (x.as_f64(), y.as_f64())).collect(),
        (a, b) => vec![(a.as_f64(), b.as_f64())],
    };
    for (o, v) in pairs {
        let (Some(o), Some(v)) = (o, v) else {
            return Some("orig: value does not match the shape of value".into());
        };
        if ((o * k) - v).abs() > 0.01 * v.abs().max(1e-6) {
            return Some(format!("orig: {o} {ou} is {} {unit}, value says {v}", o * k));
        }
    }
    None
}

/// Problems of an `applies` object: known keys and their types only.
fn applies_problems(a: &J, at: &str, out: &mut Vec<String>) {
    let Some(m) = a.as_object() else {
        if !a.is_null() {
            out.push(format!("{at}: `applies` must be an object"));
        }
        return;
    };
    for (k, v) in m {
        let ok = match k.as_str() {
            "typology" | "room_kind" | "region" => {
                v.is_string() || v.as_array().is_some_and(|a| a.iter().all(J::is_string))
            }
            "min_floors" | "max_floors" => v.is_u64(),
            "sprinklered" => v.is_boolean(),
            "height_min" | "height_max" => v.as_f64().is_some_and(|h| (0.0..=1000.0).contains(&h)),
            _ => {
                out.push(format!("{at}: unknown applies key {k:?} (one of {APPLIES_KEYS:?})"));
                continue;
            }
        };
        if !ok {
            out.push(format!("{at}: applies.{k} has the wrong type"));
        }
    }
}

/// Keys that would carry quoted standard text, anywhere inside `v`.
fn forbidden_keys(v: &J, at: &str, out: &mut Vec<String>) {
    match v {
        J::Object(m) => {
            for (k, x) in m {
                if FORBIDDEN_KEYS.contains(&k.as_str()) {
                    out.push(format!("{at}: `{k}` is not allowed: store the number, clause and a paraphrase, never the text"));
                }
                forbidden_keys(x, at, out);
            }
        }
        J::Array(a) => a.iter().for_each(|x| forbidden_keys(x, at, out)),
        _ => {}
    }
}

/// A paraphrase is one line of at most `PARAPHRASE_MAX` characters.
fn paraphrase_problem(v: &J, at: &str, out: &mut Vec<String>) {
    match &v["paraphrase"] {
        J::Null => {}
        J::String(p) if p.chars().count() <= PARAPHRASE_MAX && !p.contains('\n') && !p.trim().is_empty() => {}
        _ => out.push(format!("{at}: `paraphrase` must be one sentence of at most {PARAPHRASE_MAX} characters")),
    }
}

/// `None` when `v` (a number or a list of numbers) fits `unit`, else the problem.
pub fn unit_problem(unit: &str, v: &J) -> Option<String> {
    let Some((lo, hi)) = unit_range(unit) else {
        return Some(format!("unknown unit {unit:?}"));
    };
    let nums: Vec<Option<f64>> = match v {
        J::Array(a) => a.iter().map(J::as_f64).collect(),
        other => vec![other.as_f64()],
    };
    if nums.is_empty() {
        return Some("empty value".into());
    }
    for n in nums {
        match n {
            Some(x) if x.is_finite() && x >= lo && x <= hi => {}
            Some(x) => return Some(format!("{x} is outside {lo}..{hi} {unit}")),
            None => return Some("value is not a number".into()),
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub id: String,
    pub title: String,
    pub licence: String,
    pub publisher: Option<String>,
    pub url: Option<String>,
    pub edition: Option<String>,
    /// Date the numbers were checked against the source; `None`: not checked.
    pub verified: Option<String>,
    pub pack: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Applies {
    pub typology: Vec<String>,
    pub room_kind: Vec<String>,
    pub region: Vec<String>,
    pub min_floors: Option<u32>,
    pub max_floors: Option<u32>,
    /// Only with (`true`) or without (`false`) sprinklers; `None`: either.
    pub sprinklered: Option<bool>,
    /// Height class: building height (m, ground to the top occupied floor) in this range.
    pub height_min: Option<f64>,
    pub height_max: Option<f64>,
}

impl Applies {
    pub fn from_json(a: &J) -> Applies {
        Applies {
            typology: strs(&a["typology"]),
            room_kind: strs(&a["room_kind"]),
            region: strs(&a["region"]),
            min_floors: a["min_floors"].as_u64().map(|x| x as u32),
            max_floors: a["max_floors"].as_u64().map(|x| x as u32),
            sprinklered: a["sprinklered"].as_bool(),
            height_min: a["height_min"].as_f64(),
            height_max: a["height_max"].as_f64(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PackCheck {
    /// Runs through the company-rule engine (`param_range`, `cases`, ...).
    Legacy(Box<NormRule>),
    Kind(NormKind),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackRule {
    pub id: String,
    /// The pack that defined the rule.
    pub pack: String,
    pub text: BTreeMap<String, String>,
    pub source: String,
    pub clause: String,
    pub severity: Severity,
    pub applies: Applies,
    pub kind: String,
    pub check: PackCheck,
    pub unit: String,
    pub value: Option<f64>,
    pub example: bool,
    pub verified: bool,
    /// Lowest design depth (1..5) the rule applies at.
    pub depth: u8,
    pub discipline: String,
    pub raw: J,
}

impl PackRule {
    pub fn text(&self, lang: &str) -> &str {
        self.text
            .get(lang)
            .or_else(|| self.text.get("en"))
            .map_or("", String::as_str)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoomReq {
    pub kind: String,
    pub min_area: Option<f64>,
    pub min_width: Option<f64>,
    pub daylight: bool,
    pub wet: bool,
    pub verified: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub code: String,
    /// Picks the variant: the count of rooms of these kinds in a dwelling.
    pub select: Option<(Vec<String>, usize)>,
    pub area_min: Option<f64>,
    pub rooms: Vec<RoomReq>,
    pub allowed: Vec<String>,
    pub circulation_max: Option<f64>,
    pub storage_max: Option<f64>,
    /// The circulation and storage limits were checked against the source.
    pub limits_verified: bool,
    pub source: String,
    pub clause: String,
    pub verified: bool,
    pub raw: J,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Programme {
    pub typology: String,
    pub pack: String,
    pub variants: Vec<Variant>,
}

impl Programme {
    /// The variant for `count` rooms of its select kinds; a programme without selectors has one.
    pub fn variant(&self, count: Option<usize>) -> Option<&Variant> {
        let sel = self.variants.iter().find(|v| match (&v.select, count) {
            (Some((_, n)), Some(c)) => *n == c,
            _ => false,
        });
        sel.or_else(|| {
            (self.variants.len() == 1 && self.variants[0].select.is_none())
                .then(|| &self.variants[0])
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DimItem {
    pub key: String,
    pub value: J,
    pub unit: String,
    pub source: String,
    pub clause: String,
    pub verified: bool,
    pub pack: String,
    pub applies: Applies,
    /// What the clause says, in one sentence of our own words.
    pub paraphrase: Option<String>,
}

/// How a country counts storeys and states areas, from `pack.json` `conventions`.
#[derive(Clone, Debug, PartialEq)]
pub struct Conventions {
    /// Number of the street-level storey: 0 (ground, then 1st) or 1 (1st, then 2nd).
    pub ground_floor: i32,
    /// What a stated dwelling or programme area measures, one of `AREA_STANDARDS`.
    pub area_standard: String,
    /// Net room area as a share of the stated area (1 for `net`); our own estimate unless
    /// the pack cites a source for it.
    pub net_share: f64,
}

/// One resolved pack: its chain merged root first.
#[derive(Clone, Debug, PartialEq)]
pub struct NormPack {
    pub id: String,
    /// Root first: `["generic", "ru"]`.
    pub chain: Vec<String>,
    pub meta: J,
    pub sources: Vec<Source>,
    pub rules: Vec<PackRule>,
    pub programmes: Vec<Programme>,
    pub assemblies: Vec<J>,
    pub dims: Vec<DimItem>,
    pub drawing: J,
    /// Room kind -> names that mean it, lower case.
    pub room_kinds: BTreeMap<String, Vec<String>>,
    /// `Purpose` parameter value -> room kind.
    pub purpose: BTreeMap<String, String>,
    /// Dim keys and rule ids the country marks not applicable (`"na": true`), with the reason.
    pub na: BTreeMap<String, String>,
}

impl NormPack {
    /// Rules for a design depth and discipline (`*` for all), in pack order.
    pub fn rules(&self, depth: u8, discipline: &str) -> Vec<&PackRule> {
        self.rules
            .iter()
            .filter(|r| r.depth <= depth && (discipline == "*" || r.discipline == discipline))
            .collect()
    }
    pub fn rule(&self, id: &str) -> Option<&PackRule> {
        self.rules.iter().find(|r| r.id == id)
    }
    pub fn source(&self, id: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.id == id)
    }
    pub fn programme(&self, typology: &str) -> Option<&Programme> {
        let t = if typology == "apartment_section" { "apartment" } else { typology };
        self.programmes.iter().find(|p| p.typology == t)
    }
    pub fn dim(&self, key: &str) -> Option<&DimItem> {
        self.dims.iter().find(|d| d.key == key)
    }
    /// A dimension's value and the cite a finding names: pack, key, document, clause, and
    /// `unverified` for a number not checked against its source.
    pub fn dim_cite(&self, key: &str) -> Option<(J, String)> {
        if let Some(rest) = key.strip_prefix("assembly.") {
            return self.assembly_cite(rest);
        }
        let d = self.dim(key)?;
        let title = self.source(&d.source).map_or(d.source.as_str(), |s| s.title.as_str());
        let mark = if d.verified { "" } else { ", unverified" };
        Some((d.value.clone(), format!("{} {}: {}, {}{mark}", d.pack, d.key, title, d.clause)))
    }
    /// `<role>.<field>` of the first assembly with that role (`exterior_wall.u_value`), cited
    /// like a dimension.
    fn assembly_cite(&self, key: &str) -> Option<(J, String)> {
        let (role, field) = key.rsplit_once('.')?;
        let a = self.assemblies.iter().find(|a| a["role"].as_str() == Some(role))?;
        let v = a.get(field)?.clone();
        let src = a["source"].as_str().unwrap_or("");
        let title = self.source(src).map_or(src, |s| s.title.as_str());
        let mark = if a["verified"].as_bool() == Some(true) { "" } else { ", unverified" };
        let id = a["id"].as_str().unwrap_or(role);
        Some((v, format!("{} assembly {id}: {title}, {field}{mark}", self.id)))
    }
    /// The pack claims every key of the reference pack (`"complete": true` in its own pack.json).
    /// A partial pack is labelled so wherever it is shown or exported.
    pub fn complete(&self) -> bool {
        self.meta["complete"].as_bool().unwrap_or(false)
    }

    /// Keys a partial pack knows it lacks (its own `missing` list), for the label and export.
    pub fn missing(&self) -> Vec<String> {
        strs(&self.meta["missing"])
    }

    /// Storey numbering and area measurement of the country; `generic` gives the defaults.
    pub fn conventions(&self) -> Conventions {
        let c = &self.meta["conventions"];
        let area_standard = c["area_standard"].as_str().unwrap_or("net").to_string();
        Conventions {
            ground_floor: c["ground_floor"].as_i64().unwrap_or(0) as i32,
            net_share: c["net_share"].as_f64().unwrap_or(if area_standard == "net" { 1.0 } else { 0.9 }),
            area_standard,
        }
    }
    /// The number people use for the storey `index` above ground (0 = street level).
    pub fn storey_number(&self, index: usize) -> i64 {
        index as i64 + i64::from(self.conventions().ground_floor)
    }
    /// A stated area (brief, programme) as the net room area it stands for.
    pub fn area_as_net(&self, stated: f64) -> f64 {
        stated * self.conventions().net_share
    }
    /// The unit system to show numbers in: the person's choice, else the pack's own.
    pub fn display_system<'a>(&'a self, chosen: Option<&'a str>) -> &'a str {
        chosen
            .filter(|s| DISPLAY_SYSTEMS.contains(s))
            .or_else(|| self.meta["units"]["display"].as_str())
            .unwrap_or("metric")
    }
    /// Rules the company-rule engine runs, in pack order, as the norms panel shows them.
    pub fn legacy_rules(&self) -> Vec<NormRule> {
        self.rules
            .iter()
            .filter_map(|r| match &r.check {
                PackCheck::Legacy(n) => Some((**n).clone()),
                PackCheck::Kind(_) => None,
            })
            .collect()
    }
    /// The room kind a room name means (`Bedroom 2`, or a localized name such as a Russian one), by the pack vocabulary.
    pub fn kind_of_name(&self, name: &str) -> Option<&str> {
        let n = name.trim().to_lowercase();
        let n = n.trim_end_matches(|c: char| c.is_ascii_digit() || c == ' ' || c == '.');
        self.room_kinds
            .iter()
            .find(|(k, names)| *k == n || names.iter().any(|x| x == n))
            .map(|(k, _)| k.as_str())
    }
}

#[derive(Clone, Debug, Default)]
struct Layer {
    meta: J,
    sources: Vec<J>,
    rules: Vec<J>,
    programmes: Vec<J>,
    assemblies: Vec<J>,
    dims: Vec<J>,
    drawing: J,
    vocab: J,
}

/// Packs as read, before resolving; `problems` lists every schema error found.
#[derive(Clone, Debug, Default)]
pub struct PackSet {
    layers: BTreeMap<String, Layer>,
    pub problems: Vec<String>,
}

macro_rules! packs {
    ($($p:literal),* $(,)?) => {
        &[$(($p, include_str!(concat!("../../data/norms/packs/", $p)))),*]
    };
}

/// The packs shipped with the editor, by relative path.
pub const EMBEDDED: &[(&str, &str)] = packs![
    "ae-du/dims.json",
    "ae-du/pack.json",
    "ae-du/rules/norms.json",
    "ae-du/sources.json",
    "ae/dims.json",
    "ae/pack.json",
    "ae/rules/norms.json",
    "ae/sources.json",
    "ar/dims.json",
    "ar/pack.json",
    "ar/rules/norms.json",
    "ar/sources.json",
    "au/dims.json",
    "au/pack.json",
    "au/rules/norms.json",
    "au/sources.json",
    "br/dims.json",
    "br/pack.json",
    "br/rules/norms.json",
    "br/sources.json",
    "ca/dims.json",
    "ca/pack.json",
    "ca/rules/norms.json",
    "ca/sources.json",
    "cn/dims.json",
    "cn/pack.json",
    "cn/rules/norms.json",
    "cn/sources.json",
    "cz/dims.json",
    "cz/pack.json",
    "cz/rules/norms.json",
    "cz/sources.json",
    "de/dims.json",
    "de/pack.json",
    "de/programme/apartment.json",
    "de/programme/office.json",
    "de/rules/norms.json",
    "de/sources.json",
    "de/vocab.json",
    "es-ct/dims.json",
    "es-ct/pack.json",
    "es-ct/sources.json",
    "es/dims.json",
    "es/pack.json",
    "es/programme/apartment.json",
    "es/programme/office.json",
    "es/rules/norms.json",
    "es/sources.json",
    "es/vocab.json",
    "eu/dims.json",
    "eu/pack.json",
    "eu/programme/apartment.json",
    "eu/programme/office.json",
    "eu/rules/norms.json",
    "eu/sources.json",
    "fr/dims.json",
    "fr/pack.json",
    "fr/programme/apartment.json",
    "fr/programme/office.json",
    "fr/rules/norms.json",
    "fr/sources.json",
    "fr/vocab.json",
    "generic/assemblies.json",
    "generic/dims.json",
    "generic/drawing.json",
    "generic/pack.json",
    "generic/programme/apartment.json",
    "generic/programme/house.json",
    "generic/programme/office.json",
    "generic/rules/baseline.json",
    "generic/sources.json",
    "generic/vocab.json",
    "id/dims.json",
    "id/pack.json",
    "id/rules/norms.json",
    "id/sources.json",
    "in/dims.json",
    "in/pack.json",
    "in/rules/norms.json",
    "in/sources.json",
    "it/dims.json",
    "it/pack.json",
    "it/rules/norms.json",
    "it/sources.json",
    "jp/dims.json",
    "jp/pack.json",
    "jp/rules/norms.json",
    "jp/sources.json",
    "kr/dims.json",
    "kr/pack.json",
    "kr/rules/norms.json",
    "kr/sources.json",
    "kz/dims.json",
    "kz/pack.json",
    "kz/programme/apartment.json",
    "kz/programme/office.json",
    "kz/rules/norms.json",
    "kz/sources.json",
    "kz/vocab.json",
    "mx/dims.json",
    "mx/pack.json",
    "mx/rules/norms.json",
    "mx/sources.json",
    "nl/dims.json",
    "nl/pack.json",
    "nl/rules/norms.json",
    "nl/sources.json",
    "pl/dims.json",
    "pl/pack.json",
    "pl/programme/apartment.json",
    "pl/programme/office.json",
    "pl/rules/norms.json",
    "pl/sources.json",
    "pl/vocab.json",
    "ru/assemblies.json",
    "ru/dims.json",
    "ru/drawing.json",
    "ru/pack.json",
    "ru/programme/apartment.json",
    "ru/rules/legacy.json",
    "ru/rules/sp54.json",
    "ru/sources.json",
    "ru/vocab.json",
    "sa/dims.json",
    "sa/pack.json",
    "sa/rules/norms.json",
    "sa/sources.json",
    "se/dims.json",
    "se/pack.json",
    "se/rules/norms.json",
    "se/sources.json",
    "tr/dims.json",
    "tr/pack.json",
    "tr/rules/norms.json",
    "tr/sources.json",
    "ua/dims.json",
    "ua/pack.json",
    "ua/programme/apartment.json",
    "ua/programme/office.json",
    "ua/rules/norms.json",
    "ua/sources.json",
    "ua/vocab.json",
    "uk/dims.json",
    "uk/pack.json",
    "uk/programme/apartment.json",
    "uk/programme/office.json",
    "uk/rules/norms.json",
    "uk/sources.json",
    "uk/vocab.json",
    "us/dims.json",
    "us/pack.json",
    "us/programme/apartment.json",
    "us/programme/office.json",
    "us/rules/norms.json",
    "us/sources.json",
    "us/vocab.json",
    "vn/dims.json",
    "vn/pack.json",
    "vn/rules/norms.json",
    "vn/sources.json",
    "za/dims.json",
    "za/pack.json",
    "za/rules/norms.json",
    "za/sources.json",
];

fn list(v: &J, key: &str) -> Vec<J> {
    v[key].as_array().cloned().unwrap_or_default()
}

fn s(v: &J, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

fn strs(v: &J) -> Vec<String> {
    match v {
        J::String(x) => vec![x.clone()],
        J::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => vec![],
    }
}

fn need_str(v: &J, k: &str, at: &str, out: &mut Vec<String>) {
    if v[k].as_str().is_none_or(|x| x.trim().is_empty()) {
        out.push(format!("{at}: `{k}` is required"));
    }
}

fn need_bool(v: &J, k: &str, at: &str, out: &mut Vec<String>) {
    if !v[k].is_boolean() {
        out.push(format!("{at}: `{k}` must be true or false"));
    }
}

/// Pack ids: `ru`, `de-by`, `generic`.
pub fn valid_pack_id(id: &str) -> bool {
    let mut parts = id.split('-');
    let head = parts.next().unwrap_or("");
    let ok = |p: &str, lo: usize, hi: usize| {
        (lo..=hi).contains(&p.len())
            && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    };
    ok(head, 2, 10) && head.chars().all(|c| c.is_ascii_lowercase()) && {
        let rest: Vec<&str> = parts.collect();
        rest.len() <= 1 && rest.iter().all(|p| ok(p, 1, 8))
    }
}

impl PackSet {
    pub fn embedded() -> PackSet {
        let files: Vec<(String, String)> = EMBEDDED
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect();
        PackSet::from_files(&files)
    }

    /// Reads packs from `(relative path, text)` pairs, `<pack id>/<file>`.
    pub fn from_files(files: &[(String, String)]) -> PackSet {
        let mut set = PackSet::default();
        set.add_files(files);
        set
    }

    /// Adds or replaces packs; a pack given here replaces a pack of the same id whole.
    pub fn add_files(&mut self, files: &[(String, String)]) {
        let mut sorted: Vec<&(String, String)> = files.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        let mut fresh: BTreeMap<String, Layer> = BTreeMap::new();
        for (path, text) in sorted {
            let path = path.replace('\\', "/");
            let Some((id, rest)) = path.split_once('/') else {
                self.problems.push(format!("{path}: not inside a pack folder"));
                continue;
            };
            let v: J = match serde_json::from_str(text) {
                Ok(v) => v,
                Err(e) => {
                    self.problems.push(format!("{path}: {e}"));
                    continue;
                }
            };
            let l = fresh.entry(id.to_string()).or_default();
            match rest {
                "pack.json" => l.meta = v,
                "sources.json" => l.sources = list(&v, "sources"),
                "assemblies.json" => l.assemblies = list(&v, "assemblies"),
                "dims.json" => l.dims = list(&v, "items"),
                "drawing.json" => l.drawing = v["drawing"].clone(),
                "vocab.json" => l.vocab = v,
                r if r.starts_with("rules/") && r.ends_with(".json") => {
                    l.rules.extend(list(&v, "rules"))
                }
                r if r.starts_with("programme/") && r.ends_with(".json") => l.programmes.push(v),
                _ => self.problems.push(format!("{path}: unknown pack file")),
            }
        }
        for (id, l) in fresh {
            self.problems.extend(check_layer(&id, &l));
            self.layers.insert(id, l);
        }
    }

    /// Adds every pack folder under `dir` (user packs next to the shipped ones).
    pub fn add_dir(&mut self, dir: &Path) -> Result<(), Error> {
        fn walk(base: &Path, d: &Path, out: &mut Vec<(String, String)>) -> std::io::Result<()> {
            for e in std::fs::read_dir(d)? {
                let p = e?.path();
                if p.is_dir() {
                    walk(base, &p, out)?;
                } else if p.extension().is_some_and(|x| x == "json") {
                    let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().to_string();
                    out.push((rel, std::fs::read_to_string(&p)?));
                }
            }
            Ok(())
        }
        let mut files = vec![];
        walk(dir, dir, &mut files).map_err(|e| Error::Invalid(format!("norm packs: {e}")))?;
        self.add_files(&files);
        Ok(())
    }

    pub fn ids(&self) -> Vec<&str> {
        self.layers.keys().map(String::as_str).collect()
    }

    pub fn has(&self, id: &str) -> bool {
        self.layers.contains_key(id)
    }

    /// Root first.
    fn chain(&self, id: &str) -> Result<Vec<String>, String> {
        let mut chain = vec![];
        let mut cur = Some(id.to_string());
        while let Some(c) = cur {
            if chain.contains(&c) {
                return Err(format!("pack {id}: parent cycle through {c}"));
            }
            let Some(l) = self.layers.get(&c) else {
                return Err(format!("pack {c} not found"));
            };
            cur = l.meta["parent"].as_str().map(String::from);
            chain.push(c);
        }
        chain.reverse();
        Ok(chain)
    }

    /// Merges the chain of `id` and checks references, units and licences; every problem of
    /// the chain is reported, a pack with any problem is refused whole.
    pub fn resolve(&self, id: &str) -> Result<NormPack, Error> {
        let fail = |p: Vec<String>| Error::Invalid(format!("norm pack: {}", p.join("; ")));
        let chain = self.chain(id).map_err(|e| fail(vec![e]))?;
        let mut problems: Vec<String> = self
            .problems
            .iter()
            .filter(|p| chain.iter().any(|c| p.starts_with(&format!("{c}/")) || p.starts_with(&format!("{c}:"))))
            .cloned()
            .collect();
        let mut meta = J::Object(Map::new());
        let mut sources: Vec<(String, J)> = vec![];
        let mut rules: Vec<(String, J)> = vec![];
        let mut programmes: Vec<(String, J)> = vec![];
        let mut assemblies: Vec<(String, J)> = vec![];
        let mut dims: Vec<(String, J)> = vec![];
        let mut drawing = J::Object(Map::new());
        let mut room_kinds: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut purpose: BTreeMap<String, String> = BTreeMap::new();
        for c in &chain {
            let l = &self.layers[c];
            deep_merge(&mut meta, &l.meta);
            deep_merge(&mut drawing, &l.drawing);
            let tag = |v: &J| {
                let mut v = v.clone();
                v["_pack"] = json!(c);
                v
            };
            merge_by(&mut sources, &l.sources, "id", &tag);
            merge_by(&mut rules, &l.rules, "id", &tag);
            merge_by(&mut programmes, &l.programmes, "typology", &tag);
            merge_by(&mut assemblies, &l.assemblies, "id", &tag);
            merge_by(&mut dims, &l.dims, "key", &tag);
            if let Some(m) = l.vocab["room_kinds"].as_object() {
                for (k, names) in m {
                    let e = room_kinds.entry(k.clone()).or_default();
                    for n in strs(names) {
                        let n = n.to_lowercase();
                        if !e.contains(&n) {
                            e.push(n);
                        }
                    }
                }
            }
            if let Some(m) = l.vocab["purpose"].as_object() {
                for (k, v) in m {
                    purpose.insert(k.clone(), v.as_str().unwrap_or("").to_string());
                }
            }
        }
        meta["id"] = json!(id);
        let sources: Vec<Source> = sources
            .into_iter()
            .map(|(_, v)| Source {
                id: s(&v, "id"),
                title: s(&v, "title"),
                licence: s(&v, "licence"),
                publisher: v["publisher"].as_str().map(String::from),
                url: v["url"].as_str().map(String::from),
                edition: v["edition"].as_str().map(String::from),
                verified: v["verified"].as_str().map(String::from),
                pack: s(&v, "_pack"),
            })
            .collect();
        let has_source = |sid: &str| sources.iter().any(|x| x.id == sid);
        let known_kind = |k: &str| room_kinds.contains_key(k);
        let mut out_rules = vec![];
        let mut na = BTreeMap::new();
        for (rid, v) in rules {
            if v["disabled"].as_bool() == Some(true) {
                if v["na"] == J::Bool(true) {
                    na.insert(rid, s(&v, "note"));
                }
                continue;
            }
            let at = format!("{}: rule {rid}", s(&v, "_pack"));
            let doc = s(&v["source"], "doc");
            let Some(src) = sources.iter().find(|x| x.id == doc) else {
                problems.push(format!("{at}: source {doc:?} is not in sources"));
                continue;
            };
            for k in strs(&v["applies"]["room_kind"]).iter().chain(strs(&v["check"]["room_kind"]).iter()) {
                if !known_kind(k) {
                    problems.push(format!("{at}: unknown room kind {k:?}"));
                }
            }
            match compile_rule(&v, src) {
                Ok(r) => out_rules.push(r),
                Err(e) => problems.push(format!("{at}: {e}")),
            }
        }
        let mut out_prog = vec![];
        for (typ, v) in programmes {
            let at = format!("{}: programme {typ}", s(&v, "_pack"));
            let mut variants = vec![];
            for var in list(&v, "variants") {
                let src = var["source"].as_str().map_or_else(|| s(&v, "source"), String::from);
                let clause = var["clause"].as_str().map_or_else(|| s(&v, "clause"), String::from);
                if !has_source(&src) {
                    problems.push(format!("{at}: source {src:?} is not in sources"));
                }
                match compile_variant(&var, src, clause) {
                    Ok(x) => {
                        for k in x.rooms.iter().map(|r| &r.kind).chain(x.allowed.iter()) {
                            if !known_kind(k) {
                                problems.push(format!("{at}/{}: unknown room kind {k:?}", x.code));
                            }
                        }
                        variants.push(x)
                    }
                    Err(e) => problems.push(format!("{at}: {e}")),
                }
            }
            out_prog.push(Programme {
                typology: typ,
                pack: s(&v, "_pack"),
                variants,
            });
        }
        let mut out_dims = vec![];
        for (key, v) in dims {
            // not applicable here: the key is dropped, so no parent value stands in for it
            if v["na"] == J::Bool(true) {
                na.insert(key, s(&v, "note"));
                continue;
            }
            let src = s(&v, "source");
            if !has_source(&src) {
                problems.push(format!("{}: dim {key}: source {src:?} is not in sources", s(&v, "_pack")));
            }
            out_dims.push(DimItem {
                key,
                value: v["value"].clone(),
                unit: s(&v, "unit"),
                source: src,
                clause: s(&v, "clause"),
                verified: v["verified"].as_bool().unwrap_or(false),
                pack: s(&v, "_pack"),
                applies: Applies::from_json(&v["applies"]),
                paraphrase: v["paraphrase"].as_str().map(String::from),
            });
        }
        for (aid, v) in &assemblies {
            let src = s(v, "source");
            if !has_source(&src) {
                problems.push(format!("{}: assembly {aid}: source {src:?} is not in sources", s(v, "_pack")));
            }
        }
        if !problems.is_empty() {
            return Err(fail(problems));
        }
        // completeness is the pack's own claim: a child never inherits its parent's
        let own = &self.layers[id].meta;
        meta["complete"] = json!(own["complete"].as_bool().unwrap_or(false));
        meta["missing"] = own["missing"].clone();
        Ok(NormPack {
            id: id.to_string(),
            chain,
            meta,
            sources,
            rules: out_rules,
            programmes: out_prog,
            assemblies: assemblies.into_iter().map(|(_, v)| v).collect(),
            dims: out_dims,
            drawing,
            room_kinds,
            purpose,
            na,
        })
    }
}

/// Objects merge key by key, the child winning; anything else is replaced.
pub fn deep_merge(base: &mut J, over: &J) {
    match (base, over) {
        (J::Object(b), J::Object(o)) => {
            for (k, v) in o {
                deep_merge(b.entry(k.clone()).or_insert(J::Null), v);
            }
        }
        (b, o) if !o.is_null() => *b = o.clone(),
        _ => {}
    }
}

fn merge_by(acc: &mut Vec<(String, J)>, items: &[J], key: &str, tag: &dyn Fn(&J) -> J) {
    for it in items {
        let k = s(it, key);
        let v = tag(it);
        match acc.iter_mut().find(|(x, _)| *x == k) {
            Some(slot) => slot.1 = v,
            None => acc.push((k, v)),
        }
    }
}

/// Schema of one pack's own files; references across the chain are checked on resolve.
fn check_layer(id: &str, l: &Layer) -> Vec<String> {
    let mut out = vec![];
    let at = format!("{id}/pack.json");
    if !valid_pack_id(id) {
        out.push(format!("{id}: pack id must be lower case like `de` or `de-by`"));
    }
    if l.meta.is_null() {
        out.push(format!("{at}: missing"));
        return out;
    }
    if l.meta["id"].as_str() != Some(id) {
        out.push(format!("{at}: `id` must be {id:?}"));
    }
    need_str(&l.meta, "version", &at, &mut out);
    if l.meta["units"]["length"].as_str() != Some("m") {
        out.push(format!("{at}: units.length must be \"m\" (numbers are SI)"));
    }
    if !STATUSES.contains(&l.meta["status"].as_str().unwrap_or("")) {
        out.push(format!("{at}: `status` must be one of {STATUSES:?}"));
    }
    if !(l.meta["parent"].is_null() || l.meta["parent"].is_string()) {
        out.push(format!("{at}: `parent` must be a pack id or null"));
    }
    let (complete, missing) = (&l.meta["complete"], &l.meta["missing"]);
    if !(complete.is_null() || complete.is_boolean()) {
        out.push(format!("{at}: `complete` must be true or false"));
    }
    if !missing.is_null() {
        if complete.as_bool() != Some(false) {
            out.push(format!("{at}: `missing` belongs to a pack with `complete: false`"));
        }
        if !missing.as_array().is_some_and(|a| a.iter().all(|k| k.as_str().is_some_and(|k| !k.is_empty()))) {
            out.push(format!("{at}: `missing` must list key names"));
        }
    }
    if let Some(d) = l.meta["units"].get("display").filter(|d| !d.is_null()) {
        if !d.as_str().is_some_and(|d| DISPLAY_SYSTEMS.contains(&d)) {
            out.push(format!("{at}: units.display must be one of {DISPLAY_SYSTEMS:?}"));
        }
    }
    let c = &l.meta["conventions"];
    if !c.is_null() {
        if !c.is_object() {
            out.push(format!("{at}: `conventions` must be an object"));
        }
        if !c["ground_floor"].is_null() && !matches!(c["ground_floor"].as_i64(), Some(0 | 1)) {
            out.push(format!("{at}: conventions.ground_floor must be 0 or 1"));
        }
        if !c["area_standard"].is_null() && !c["area_standard"].as_str().is_some_and(|a| AREA_STANDARDS.contains(&a)) {
            out.push(format!("{at}: conventions.area_standard must be one of {AREA_STANDARDS:?}"));
        }
        if !c["net_share"].is_null() && !c["net_share"].as_f64().is_some_and(|x| x > 0.5 && x <= 1.0) {
            out.push(format!("{at}: conventions.net_share must be a share over 0.5 and at most 1"));
        }
        if c["area_standard"].as_str() == Some("net") && c["net_share"].as_f64().is_some_and(|x| x != 1.0) {
            out.push(format!("{at}: a net area standard has net_share 1"));
        }
    }
    if l.meta["parent"].is_null() && id != "generic" {
        out.push(format!("{at}: only `generic` has no parent"));
    }
    forbidden_keys(&l.meta, &at, &mut out);
    for (i, src) in l.sources.iter().enumerate() {
        let at = format!("{id}/sources.json #{i}");
        forbidden_keys(src, &at, &mut out);
        if let Some(u) = src["url"].as_str() {
            if !(u.starts_with("https://") || u.starts_with("http://")) {
                out.push(format!("{at}: url must be a web address"));
            }
        }
        need_str(src, "id", &at, &mut out);
        need_str(src, "title", &at, &mut out);
        match src["licence"].as_str() {
            None => out.push(format!("{at}: `licence` is required")),
            Some(x) if !LICENCES.contains(&x) => {
                out.push(format!("{at}: licence {x:?} is not one of {LICENCES:?}"))
            }
            _ => {}
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for r in &l.rules {
        let rid = s(r, "id");
        let at = format!("{id}/rules: rule {rid}");
        if rid.is_empty() {
            out.push(format!("{id}/rules: a rule has no `id`"));
            continue;
        }
        if !seen.insert(rid.clone()) {
            out.push(format!("{at}: duplicate id"));
        }
        if r["disabled"].as_bool() == Some(true) {
            if r["na"] == J::Bool(true) {
                need_str(r, "note", &at, &mut out);
            }
            continue;
        }
        need_str(&r["text"], "en", &format!("{at} text"), &mut out);
        need_str(&r["source"], "doc", &format!("{at} source"), &mut out);
        need_str(&r["source"], "clause", &format!("{at} source"), &mut out);
        need_bool(r, "verified", &at, &mut out);
        applies_problems(&r["applies"], &at, &mut out);
        forbidden_keys(r, &at, &mut out);
        paraphrase_problem(r, &at, &mut out);
        paraphrase_problem(&r["source"], &at, &mut out);
        if !r["orig"].is_null() {
            if let Some(p) = orig_problem(&r["orig"], &r["value"], r["unit"].as_str().unwrap_or("")) {
                out.push(format!("{at}: {p}"));
            }
        }
        let kind = s(&r["check"], "kind");
        if !LEGACY_KINDS.contains(&kind.as_str()) && !NEW_KINDS.contains(&kind.as_str()) {
            out.push(format!("{at}: unknown check kind {kind:?}"));
        }
        if let Some(d) = r.get("depth").filter(|d| !d.is_null()) {
            if !d.as_u64().is_some_and(|d| (1..=5).contains(&d)) {
                out.push(format!("{at}: `depth` must be 1..5"));
            }
        }
        let unit = s(r, "unit");
        if unit_range(&unit).is_none() {
            out.push(format!("{at}: unknown unit {unit:?}"));
        } else if !r["value"].is_null() {
            if let Some(p) = unit_problem(&unit, &r["value"]) {
                out.push(format!("{at}: value: {p}"));
            }
        }
    }
    for p in &l.programmes {
        need_str(p, "typology", &format!("{id}/programme"), &mut out);
    }
    for (i, d) in l.dims.iter().enumerate() {
        let at = format!("{id}/dims.json #{i}");
        need_str(d, "key", &at, &mut out);
        need_bool(d, "verified", &at, &mut out);
        // a key the country does not have: `"na": true`, a null value and the reason in `note`
        if d["na"] == J::Bool(true) {
            if !d["value"].is_null() || d["unlimited"] == J::Bool(true) {
                out.push(format!("{at} {}: a not-applicable dim has a null value and no limit flag", s(d, "key")));
            }
            need_str(d, "note", &at, &mut out);
            forbidden_keys(d, &at, &mut out);
            continue;
        }
        // a source that sets no limit: `"unlimited": true` with a null value
        if d["unlimited"] == J::Bool(true) {
            if !d["value"].is_null() {
                out.push(format!("{at} {}: an unlimited dim has a null value", s(d, "key")));
            }
        } else if let Some(p) = unit_problem(&s(d, "unit"), &d["value"]) {
            out.push(format!("{at} {}: {p}", s(d, "key")));
        }
        if d["verified"].as_bool() == Some(true) && s(d, "clause").trim().is_empty() {
            out.push(format!("{at} {}: a verified number names its clause", s(d, "key")));
        }
        applies_problems(&d["applies"], &at, &mut out);
        forbidden_keys(d, &at, &mut out);
        paraphrase_problem(d, &at, &mut out);
        if !d["orig"].is_null() {
            if let Some(p) = orig_problem(&d["orig"], &d["value"], &s(d, "unit")) {
                out.push(format!("{at} {}: {p}", s(d, "key")));
            }
        }
    }
    for (i, a) in l.assemblies.iter().enumerate() {
        let at = format!("{id}/assemblies.json #{i}");
        need_str(a, "id", &at, &mut out);
        need_bool(a, "verified", &at, &mut out);
        for k in ["u_value"] {
            if !a[k].is_null() {
                if let Some(p) = unit_problem("W/m2K", &a[k]) {
                    out.push(format!("{at}: {k}: {p}"));
                }
            }
        }
    }
    out
}

/// A rule's unit must match what its check measures.
fn kind_unit(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "formula" => Some(&["m", ""]),
        "ratio" => Some(&["ratio"]),
        "distance" => Some(&["m"]),
        "programme" => Some(&["m2"]),
        "adjacency" | "stack" => Some(&[""]),
        _ => None,
    }
}

fn compile_rule(v: &J, src: &Source) -> Result<PackRule, String> {
    let kind = s(&v["check"], "kind");
    let clause = s(&v["source"], "clause");
    let unit = s(v, "unit");
    if let Some(ok) = kind_unit(&kind) {
        if !ok.contains(&unit.as_str()) {
            return Err(format!("unit {unit:?} does not fit a {kind} check ({ok:?})"));
        }
    }
    let text: BTreeMap<String, String> = v["text"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, x)| x.as_str().map(|x| (k.clone(), x.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let check = if LEGACY_KINDS.contains(&kind.as_str()) {
        let mut flat = v.clone();
        flat["title"] = json!(text.get("en").cloned().unwrap_or_default());
        flat["source_text"] = json!(format!("{} {clause}", src.title));
        let def = rule_from(&flat).map_err(|e| e.to_string())?;
        PackCheck::Legacy(Box::new(NormRule {
            def,
            title_ru: text.get("ru").cloned().unwrap_or_default(),
            doc: src.title.clone(),
            clause: clause.clone(),
            example: v["example"].as_bool().unwrap_or(false),
        }))
    } else {
        PackCheck::Kind(parse_kind(&kind, &v["check"])?)
    };
    let severity = match v["severity"].as_str() {
        Some("error") => Severity::Error,
        Some("warning") => Severity::Warning,
        Some("info") => Severity::Info,
        _ => return Err("severity must be error, warning or info".into()),
    };
    let a = &v["applies"];
    Ok(PackRule {
        id: s(v, "id"),
        pack: s(v, "_pack"),
        text,
        source: src.id.clone(),
        clause,
        severity,
        applies: Applies::from_json(a),
        kind,
        check,
        unit,
        value: v["value"].as_f64(),
        example: v["example"].as_bool().unwrap_or(false),
        verified: v["verified"].as_bool().unwrap_or(false),
        depth: v["depth"].as_u64().unwrap_or(1) as u8,
        discipline: v["discipline"].as_str().unwrap_or("arch").to_string(),
        raw: {
            let mut r = v.clone();
            if let Some(o) = r.as_object_mut() {
                o.remove("_pack");
            }
            r
        },
    })
}

fn area(v: &J, k: &str) -> Result<Option<f64>, String> {
    match &v[k] {
        J::Null => Ok(None),
        x => match unit_problem("m2", x) {
            None => Ok(x.as_f64()),
            Some(p) => Err(format!("{k}: {p}")),
        },
    }
}

fn compile_variant(v: &J, source: String, clause: String) -> Result<Variant, String> {
    let code = s(v, "code");
    if code.is_empty() {
        return Err("a variant has no `code`".into());
    }
    let verified = v["verified"].as_bool().ok_or(format!("{code}: `verified` is required"))?;
    let mut rooms = vec![];
    for r in list(v, "rooms") {
        let kind = s(&r, "kind");
        if kind.is_empty() {
            return Err(format!("{code}: a room has no `kind`"));
        }
        let min_width = match &r["min_width"] {
            J::Null => None,
            x => match unit_problem("m", x) {
                None => x.as_f64(),
                Some(p) => return Err(format!("{code}: min_width: {p}")),
            },
        };
        rooms.push(RoomReq {
            kind,
            min_area: area(&r, "min_area").map_err(|e| format!("{code}: {e}"))?,
            min_width,
            daylight: r["daylight"].as_bool().unwrap_or(false),
            wet: r["wet"].as_bool().unwrap_or(false),
            verified: r["verified"].as_bool().unwrap_or(verified),
        });
    }
    let ratio = |k: &str| -> Result<Option<f64>, String> {
        match &v[k] {
            J::Null => Ok(None),
            x => match unit_problem("ratio", x) {
                None => Ok(x.as_f64()),
                Some(p) => Err(format!("{code}: {k}: {p}")),
            },
        }
    };
    let select = match &v["select"] {
        J::Null => None,
        sel => Some((
            strs(&sel["room_kind"]),
            sel["count"].as_u64().ok_or(format!("{code}: select.count"))? as usize,
        )),
    };
    Ok(Variant {
        select,
        area_min: area(v, "area_min").map_err(|e| format!("{code}: {e}"))?,
        rooms,
        allowed: strs(&v["allowed"]),
        circulation_max: ratio("circulation_max")?,
        storage_max: ratio("storage_max")?,
        limits_verified: v["limits_verified"].as_bool().unwrap_or(false),
        source,
        clause,
        verified,
        raw: v.clone(),
        code,
    })
}

const EU: [&str; 27] = [
    "at", "be", "bg", "hr", "cy", "cz", "dk", "ee", "fi", "fr", "de", "gr", "hu", "ie", "it", "lv",
    "lt", "lu", "mt", "nl", "pl", "pt", "ro", "sk", "si", "es", "se",
];

/// The country (lower case) a UI language and OS region point to: `ru` -> `ru`, `pt-BR` ->
/// `br`, `es` + region `MX` -> `mx`, `en` -> `us`, `uk` (Ukrainian) -> `ua`.
pub fn country_for_locale(lang: &str, region: Option<&str>) -> String {
    let tag = lang.trim().to_lowercase().replace('_', "-");
    let (l, tag_region) = match tag.split_once('-') {
        Some((a, b)) => (a.to_string(), Some(b.to_string())),
        None => (tag.clone(), None),
    };
    let region = tag_region.or(region.map(|r| r.trim().to_lowercase())).filter(|r| r.len() == 2);
    let base = match l.as_str() {
        "uk" => "ua",
        "kk" => "kz",
        "ky" => "kg",
        "uz" => "uz",
        "mn" => "mn",
        "cs" => "cz",
        "el" => "gr",
        "et" => "ee",
        "da" => "dk",
        "sv" => "se",
        "no" | "nb" | "nn" => "no",
        "en" => "us",
        "he" => "il",
        "ar" => "sa",
        "fa" => "ir",
        "ur" => "pk",
        "hi" => "in",
        "bn" => "bd",
        "vi" => "vn",
        "fil" => "ph",
        "zh" => "cn",
        "ja" => "jp",
        "ko" => "kr",
        "ru" | "bg" | "hu" | "ro" | "hr" | "pl" | "de" | "fr" | "it" | "es" | "pt" | "nl"
        | "fi" | "tr" | "th" | "id" => l.as_str(),
        _ => "",
    };
    // languages spoken in several markets follow the region
    let multi = ["en", "es", "pt", "ar", "de", "fr", "nl"].contains(&l.as_str());
    match region {
        Some(r) if multi => if r == "gb" { "uk".into() } else { r },
        _ => base.to_string(),
    }
}

/// The nearest pack `set` has for a wanted id: itself, its country, `eu` for EU members,
/// then `generic`.
pub fn nearest_pack(set: &PackSet, wanted: &str) -> String {
    let w = wanted.to_lowercase();
    let country = w.split('-').next().unwrap_or("").to_string();
    let mut tries = vec![w.clone(), country.clone()];
    if EU.contains(&country.as_str()) {
        tries.push("eu".into());
    }
    tries.push("generic".into());
    tries
        .into_iter()
        .find(|t| set.has(t))
        .unwrap_or_else(|| "generic".into())
}
