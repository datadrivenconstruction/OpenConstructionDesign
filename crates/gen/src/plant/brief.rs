// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The brief of a plant: its kind and duty from fields or from words in English or a locale's
//! word list ("boiler house 2 MW with 3 boilers", "pumping station 120 m3/h head 45 m"); a storage
//! tank's diameter, height and nozzles also from German or Spanish words.

use serde_json::{json, Value as J};

/// Largest heat output a brief may ask for, kW.
pub const MAX_DUTY_KW: f64 = 20000.0;
/// Largest flow a brief may ask for, m³/h.
pub const MAX_FLOW: f64 = 1000.0;
/// Largest pump head a brief may ask for, m.
pub const MAX_HEAD: f64 = 200.0;
/// Most units of the main kind a brief may ask for.
pub const MAX_UNITS: usize = 8;
/// Largest storage tank a brief may ask for: diameter and shell height, m; most nozzles.
pub const MAX_TANK_D: f64 = 60.0;
pub const MAX_TANK_H: f64 = 30.0;
pub const MAX_NOZZLES: usize = 12;
/// kW in one MW, one Gcal/h, and m³/h in one l/s.
const KW_PER_MW: f64 = 1000.0;
const KW_PER_GCALH: f64 = 1163.0;
const M3H_PER_LS: f64 = 3.6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    BoilerHouse,
    PumpingStation,
    WaterTreatment,
    ProcessLine,
    StorageTank,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::BoilerHouse, Kind::PumpingStation, Kind::WaterTreatment, Kind::ProcessLine, Kind::StorageTank];
    pub fn name(self) -> &'static str {
        match self {
            Kind::BoilerHouse => "boiler_house",
            Kind::PumpingStation => "pumping_station",
            Kind::WaterTreatment => "water_treatment",
            Kind::ProcessLine => "process_line",
            Kind::StorageTank => "storage_tank",
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.name() == s)
    }
    /// Words that name the kind, checked in this order (a boiler house has pumps too).
    fn words(self) -> Vec<&'static str> {
        let (en, key): (&[&'static str], &str) = match self {
            Kind::BoilerHouse => (&["boiler"], "kind_boiler_house"),
            Kind::WaterTreatment => (&["water treatment", "filtration", "filter"], "kind_water_treatment"),
            Kind::ProcessLine => (&["process", "reactor", "batch", "chemical", "food", "distill"], "kind_process_line"),
            Kind::PumpingStation => (&["pumping", "pump station", "pump"], "kind_pumping_station"),
            Kind::StorageTank => (&["tank", "tanque"], "kind_storage_tank"),
        };
        en.iter().copied().chain(loc_list(key).iter().map(String::as_str)).collect()
    }
}

/// What the plant must do.
#[derive(Clone, Debug, PartialEq)]
pub struct Brief {
    pub kind: Kind,
    /// Heat output, kW (a boiler house).
    pub duty_kw: Option<f64>,
    /// Flow, m³/h (a pumping station, water treatment, a process line).
    pub flow_m3h: Option<f64>,
    /// Pump head, m (a pumping station).
    pub head_m: Option<f64>,
    /// Units of the main kind (boilers, duty pumps, filters).
    pub units: Option<usize>,
    /// A process line with a distillation column, reboiler and condenser.
    pub distill: bool,
    /// Feed tanks of a process line.
    pub feed_tanks: usize,
    /// Open air: the rack stands on portal frames instead of hanging from a roof.
    pub outdoor: bool,
    /// Height of the pipe rack the brief asks for, m.
    pub rack_m: Option<f64>,
    /// A storage tank: diameter and shell height, m, and nozzles besides the manway.
    pub diameter_m: Option<f64>,
    pub height_m: Option<f64>,
    pub nozzles: Option<usize>,
    pub text: String,
}

const NUMBER_WORDS: [(&str, usize); 6] = [("one", 1), ("two", 2), ("three", 3), ("four", 4), ("five", 5), ("six", 6)];

/// Brief words of every locale (`data/lang/<locale>/plant_brief.json`): stems and units by key.
fn loc_list(key: &str) -> &'static [String] {
    crate::lang::words("plant_brief", key)
}

/// English stems followed by the locales' ones under `key`.
fn word_set(en: &[&'static str], key: &str) -> Vec<&'static str> {
    en.iter().copied().chain(loc_list(key).iter().map(String::as_str)).collect()
}

/// True when `w` starts with a locale's stem or unit under `key`.
fn starts_loc(w: &str, key: &str) -> bool {
    loc_list(key).iter().any(|u| w.starts_with(u.as_str()))
}

/// The count a number word names (English, the locale lists, and with `more` German and Spanish).
fn number_word(w: &str, more: bool) -> Option<usize> {
    let en = NUMBER_WORDS.iter().find(|(x, _)| *x == w).map(|&(_, n)| n);
    let loc = (1..=6).find(|n| loc_list(&format!("n{n}")).iter().any(|x| x == w));
    let other = if more { MORE_NUMBER_WORDS.iter().find(|(x, _)| *x == w).map(|&(_, n)| n) } else { None };
    en.or(loc).or(other)
}

/// Counts in German and Spanish, for the nozzles of a storage tank.
const MORE_NUMBER_WORDS: [(&str, usize); 10] =
    [("zwei", 2), ("drei", 3), ("vier", 4), ("fünf", 5), ("sechs", 6), ("dos", 2), ("tres", 3), ("cuatro", 4), ("cinco", 5), ("seis", 6)];
/// Stems of a nozzle, a diameter and a height; prepositions between a size and its word.
const NOZZLE_WORDS: [&str; 3] = ["nozzle", "stutzen", "boquilla"];
const DIAMETER_WORDS: [&str; 4] = ["diam", "diám", "durchmesser", "ø"];
const HEIGHT_WORDS: [&str; 7] = ["tall", "high", "height", "höhe", "hoch", "altura", "alto"];
const LINK_WORDS: [&str; 4] = ["de", "of", "in", "im"];

/// Words after a count that name the main units.
const UNIT_WORDS: [&str; 4] = ["boiler", "filter", "pump", "unit"];

fn number_at(chars: &[char], i: usize) -> Option<(f64, usize)> {
    let mut j = i;
    let mut s = String::new();
    while j < chars.len() && (chars[j].is_ascii_digit() || ((chars[j] == '.' || chars[j] == ',') && !s.is_empty())) {
        s.push(if chars[j] == ',' { '.' } else { chars[j] });
        j += 1;
    }
    let s = s.trim_end_matches('.');
    // num-lint: brief text; the caller split digits and one decimal mark
    s.parse::<f64>().ok().map(|x| (x, j))
}

/// The word after position `j`, lowercase, skipping spaces.
fn word_after(chars: &[char], mut j: usize) -> String {
    while j < chars.len() && chars[j] == ' ' {
        j += 1;
    }
    chars[j..].iter().take_while(|c| !c.is_whitespace() && **c != ',' && **c != ';').collect()
}

impl Brief {
    pub fn parse(v: &J) -> Result<Brief, String> {
        let text = ["text", "brief", "description"]
            .iter()
            .find_map(|k| v.get(*k).and_then(J::as_str))
            .unwrap_or("")
            .to_string();
        let low = text.to_lowercase();
        // a reboiler is a column's heater, not a boiler house
        let mut low_k = low.replace("reboiler", " ");
        for r in loc_list("reboiler") {
            low_k = low_k.replace(r.as_str(), " ");
        }
        let kind = match v.get("kind").and_then(J::as_str) {
            Some(k) => Kind::parse(k).ok_or(format!(
                "kind is one of {}",
                Kind::ALL.iter().map(|k| k.name()).collect::<Vec<_>>().join(", ")
            ))?,
            None => [Kind::BoilerHouse, Kind::WaterTreatment, Kind::ProcessLine, Kind::PumpingStation, Kind::StorageTank]
                .into_iter()
                .find(|k| k.words().iter().any(|w| low_k.contains(w)))
                .ok_or("say what plant: kind boiler_house, pumping_station, water_treatment or process_line")?,
        };
        let mut b = Brief {
            kind,
            duty_kw: None,
            flow_m3h: None,
            head_m: None,
            units: None,
            distill: false,
            feed_tanks: 1,
            outdoor: false,
            rack_m: None,
            diameter_m: None,
            height_m: None,
            nozzles: None,
            text,
        };
        b.scan(&low);
        let num = |k: &str| v.get(k).and_then(J::as_f64);
        if let Some(x) = num("duty_kw") {
            b.duty_kw = Some(x);
        }
        if let Some(x) = num("flow_m3h") {
            b.flow_m3h = Some(x);
        }
        if let Some(x) = num("head_m") {
            b.head_m = Some(x);
        }
        if let Some(x) = v.get("units").and_then(J::as_u64) {
            b.units = Some(x as usize);
        }
        if let Some(x) = v.get("distill").and_then(J::as_bool) {
            b.distill = x;
        }
        if let Some(x) = v.get("feed_tanks").and_then(J::as_u64) {
            b.feed_tanks = x as usize;
        }
        if let Some(x) = v.get("outdoor").and_then(J::as_bool) {
            b.outdoor = x;
        }
        if let Some(x) = num("rack_m") {
            b.rack_m = Some(x);
        }
        if b.kind == Kind::StorageTank {
            b.scan_tank(&low);
        }
        if let Some(x) = num("diameter_m") {
            b.diameter_m = Some(x);
        }
        if let Some(x) = num("height_m") {
            b.height_m = Some(x);
        }
        if let Some(x) = v.get("nozzles").and_then(J::as_u64) {
            b.nozzles = Some(x as usize);
        }
        b.check()?;
        Ok(b)
    }

    /// Duty, flow, head and unit count from the words of the brief.
    fn scan(&mut self, low: &str) {
        let chars: Vec<char> = low.chars().collect();
        let unit_words = word_set(&UNIT_WORDS, "unit");
        let mut i = 0;
        while i < chars.len() {
            let starts = chars[i].is_ascii_digit() && (i == 0 || !chars[i - 1].is_alphanumeric());
            if !starts {
                i += 1;
                continue;
            }
            let Some((x, j)) = number_at(&chars, i) else {
                i += 1;
                continue;
            };
            let w = word_after(&chars, j);
            let before: String = chars[i.saturating_sub(24)..i].iter().collect();
            if w.starts_with("mw") || starts_loc(&w, "mw") {
                self.duty_kw = Some(x * KW_PER_MW);
            } else if w.starts_with("kw") || starts_loc(&w, "kw") {
                self.duty_kw = Some(x);
            } else if w.starts_with("gcal") || starts_loc(&w, "gcal") {
                self.duty_kw = Some(x * KW_PER_GCALH);
            } else if ["m3/h", "m³/h", "m3/hr", "cmh"].iter().any(|u| w.starts_with(u)) || starts_loc(&w, "flow") {
                self.flow_m3h = Some(x);
            } else if w.starts_with("l/s") || starts_loc(&w, "ls") {
                self.flow_m3h = Some(x * M3H_PER_LS);
            } else if (w == "m"
                || w.starts_with("m ")
                || w == "m."
                || loc_list("meter").iter().any(|u| w == *u || w == format!("{u}.")))
                && (before.contains("head") || loc_list("head").iter().any(|h| before.contains(h.as_str())))
            {
                self.head_m = Some(x);
            } else if (w == "m" || w.starts_with("m ") || w == "m.") && before.contains("rack") && self.rack_m.is_none() {
                self.rack_m = Some(x);
            } else if unit_words.iter().any(|u| w.starts_with(u)) && self.units.is_none() {
                self.units = Some(x as usize);
            }
            i = j.max(i + 1);
        }
        self.distill = word_set(&["distill", "rectif"], "distill").iter().any(|w| low.contains(w))
            || (low.contains("column") && self.kind == Kind::ProcessLine)
            || (loc_list("column").iter().any(|w| low.contains(w.as_str())) && self.kind == Kind::ProcessLine);
        self.outdoor = word_set(&["outdoor", "outside", "open air", "open-air"], "outdoor").iter().any(|w| low.contains(w));
        {
            let words: Vec<&str> = low.split(|c: char| !c.is_alphanumeric()).filter(|s| !s.is_empty()).collect();
            for p in words.windows(3) {
                if let Some(n) = number_word(p[0], false) {
                    if p[1] == "feed" && p[2].starts_with("tank") {
                        self.feed_tanks = n;
                    }
                }
            }
        }
        if self.units.is_none() {
            let words: Vec<&str> = low.split(|c: char| !c.is_alphanumeric()).filter(|s| !s.is_empty()).collect();
            for p in words.windows(2) {
                if let Some(n) = number_word(p[0], false) {
                    if unit_words.iter().any(|u| p[1].starts_with(u)) {
                        self.units = Some(n);
                        break;
                    }
                }
            }
        }
    }

    /// A tank's diameter and height from a size in metres and the word just after it (past a
    /// preposition) or just before it; the nozzles from a number or count word before a nozzle.
    fn scan_tank(&mut self, low: &str) {
        let words: Vec<&str> = low
            .split(|c: char| !(c.is_alphanumeric() || c == '.' || c == ','))
            .map(|w| w.trim_matches(|c: char| c == '.' || c == ','))
            .filter(|w| !w.is_empty())
            .collect();
        let is = |w: &str, set: &[&str]| set.iter().any(|s| w.starts_with(s));
        let nozzle_words = word_set(&NOZZLE_WORDS, "nozzle");
        let diameter_words = word_set(&DIAMETER_WORDS, "diameter");
        let height_words = word_set(&HEIGHT_WORDS, "height");
        for (i, &w) in words.iter().enumerate() {
            let digits: String = w.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',').collect();
            // num-lint: brief text in any language; either decimal mark is read
            let Ok(x) = digits.replace(',', ".").parse::<f64>() else { continue };
            let glued = &w[digits.len()..];
            let (unit, mut j) = if glued.is_empty() { (words.get(i + 1).copied().unwrap_or(""), i + 2) } else { (glued, i + 1) };
            if unit != "m" && !loc_list("meter").iter().any(|u| u == unit) {
                if words.get(i + 1).is_some_and(|&n| is(n, &nozzle_words)) {
                    self.nozzles = Some(x as usize);
                }
                continue;
            }
            while words.get(j).is_some_and(|n| LINK_WORDS.contains(n)) {
                j += 1;
            }
            let after = words.get(j).copied().unwrap_or("");
            let before = if i > 0 { words[i - 1] } else { "" };
            if is(after, &diameter_words) || (!is(after, &height_words) && is(before, &diameter_words)) {
                self.diameter_m = self.diameter_m.or(Some(x));
            } else if is(after, &height_words) || is(before, &height_words) {
                self.height_m = self.height_m.or(Some(x));
            }
        }
        for p in words.windows(2) {
            let count = number_word(p[0], true);
            if let Some(n) = count.filter(|_| is(p[1], &nozzle_words)) {
                self.nozzles = Some(n);
            }
        }
    }

    fn check(&self) -> Result<(), String> {
        let within = |x: Option<f64>, max: f64, what: &str| match x {
            Some(v) if !(v > 0.0 && v <= max) => Err(format!("{what} must be above 0 and at most {max}")),
            _ => Ok(()),
        };
        within(self.duty_kw, MAX_DUTY_KW, "duty_kw")?;
        within(self.flow_m3h, MAX_FLOW, "flow_m3h")?;
        within(self.head_m, MAX_HEAD, "head_m")?;
        if let Some(n) = self.units {
            if n == 0 || n > MAX_UNITS {
                return Err(format!("units must be 1 to {MAX_UNITS}"));
            }
        }
        if self.feed_tanks == 0 || self.feed_tanks > MAX_UNITS {
            return Err(format!("feed_tanks must be 1 to {MAX_UNITS}"));
        }
        within(self.diameter_m, MAX_TANK_D, "diameter_m")?;
        within(self.height_m, MAX_TANK_H, "height_m")?;
        if let Some(n) = self.nozzles {
            if n == 0 || n > MAX_NOZZLES {
                return Err(format!("nozzles must be 1 to {MAX_NOZZLES}"));
            }
        }
        match self.kind {
            Kind::BoilerHouse if self.duty_kw.is_none() => {
                Err("a boiler house needs its heat output: duty_kw, or e.g. \"2 MW\" in the text".into())
            }
            Kind::PumpingStation | Kind::WaterTreatment | Kind::ProcessLine if self.flow_m3h.is_none() => Err(format!(
                "a {} needs its flow: flow_m3h, or e.g. \"120 m3/h\" in the text",
                self.kind.name().replace('_', " ")
            )),
            _ => Ok(()),
        }
    }

    pub fn to_json(&self) -> J {
        json!({"kind": self.kind.name(), "duty_kw": self.duty_kw, "flow_m3h": self.flow_m3h,
               "head_m": self.head_m, "units": self.units, "distill": self.distill, "feed_tanks": self.feed_tanks,
               "outdoor": self.outdoor, "rack_m": self.rack_m, "diameter_m": self.diameter_m, "height_m": self.height_m,
               "nozzles": self.nozzles, "text": self.text})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Russian sample text from the test data file.
    fn ru(key: &str) -> String {
        let m: J = serde_json::from_str(include_str!("../../tests/data/ru_samples.json")).unwrap();
        m[key].as_str().unwrap_or_else(|| panic!("no ru sample {key}")).to_string()
    }

    #[test]
    fn words_give_kind_duty_and_units() {
        let b = Brief::parse(&json!({"text": "Boiler house 2 MW with 3 boilers"})).unwrap();
        assert_eq!((b.kind, b.duty_kw, b.units), (Kind::BoilerHouse, Some(2000.0), Some(3)));
        let b = Brief::parse(&json!({"text": ru("boiler")})).unwrap();
        assert_eq!((b.kind, b.duty_kw, b.units), (Kind::BoilerHouse, Some(1500.0), Some(3)));
        let b = Brief::parse(&json!({"text": ru("pump")})).unwrap();
        assert_eq!((b.kind, b.flow_m3h, b.head_m), (Kind::PumpingStation, Some(120.0), Some(45.0)));
        let b = Brief::parse(&json!({"text": "water treatment 20 l/s"})).unwrap();
        assert_eq!(b.kind, Kind::WaterTreatment);
        assert!((b.flow_m3h.unwrap() - 72.0).abs() < 1e-9);
        assert!(Brief::parse(&json!({"text": "boiler house"})).is_err());
        assert!(Brief::parse(&json!({"kind": "mill", "flow_m3h": 3})).is_err());
        assert!(Brief::parse(&json!({"kind": "process_line", "flow_m3h": 5000})).is_err());
    }

    #[test]
    fn a_tank_brief_gives_its_diameter_height_and_nozzles_in_four_languages() {
        for t in [
            "A vertical storage tank 12 m diameter 14 m tall with a conical roof, manway, four nozzles".to_string(),
            ru("tank"),
            "Ein stehender Lagertank mit 12 m Durchmesser und 14 m Höhe mit Kegeldach, Mannloch, vier Stutzen".to_string(),
            "Un tanque de almacenamiento vertical de 12 m de diámetro y 14 m de altura, cuatro boquillas".to_string(),
        ] {
            let b = Brief::parse(&json!({"text": t})).unwrap();
            assert_eq!((b.kind, b.diameter_m, b.height_m, b.nozzles), (Kind::StorageTank, Some(12.0), Some(14.0), Some(4)), "{t}");
        }
        assert!(Brief::parse(&json!({"kind": "storage_tank", "diameter_m": 80})).is_err());
    }
}
