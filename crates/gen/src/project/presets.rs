// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Presets (`gen_preset.v1`): a starting set of a typology, as data a user may open, change and
//! save as their own. A preset is a base typology, brief defaults, a programme
//! (`gen_program.v1`) and rule values at the preset layer. The built-in presets lie in
//! `data/presets/`; a project keeps its own under props `gen_preset|<name>`, the user library
//! in `%APPDATA%/ddc/gen_presets/`. The reply says what was approximated, so the assistant
//! never claims more than was built.

use super::merge_patch;
use super::program::Program;
use super::programme::{default_programme, Programme};
use super::rules::{self, Layer, RuleSet};
use super::{RoomKind, Typology};
use serde_json::{json, Map, Value as J};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const SCHEMA: &str = "gen_preset.v1";
/// Key prefix of a preset kept in the project.
pub const PROP_PREFIX: &str = "gen_preset|";

const BUILTIN: [&str; 16] = [
    include_str!("../../data/presets/hotel.json"),
    include_str!("../../data/presets/clinic.json"),
    include_str!("../../data/presets/warehouse.json"),
    include_str!("../../data/presets/mixed_use.json"),
    include_str!("../../data/presets/school_campus.json"),
    include_str!("../../data/presets/residential.json"),
    include_str!("../../data/typologies/hospital_wing.json"),
    include_str!("../../data/typologies/data_center.json"),
    include_str!("../../data/typologies/underground_station.json"),
    include_str!("../../data/typologies/process_plant_site.json"),
    include_str!("../../data/typologies/secondary_school.json"),
    include_str!("../../data/typologies/light_industrial.json"),
    include_str!("../../data/typologies/plant_hall.json"),
    include_str!("../../data/typologies/residential_tower.json"),
    include_str!("../../data/typologies/mixed_use_podium.json"),
    include_str!("../../data/typologies/conference_hotel.json"),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub name: String,
    pub aliases: Vec<String>,
    pub base: Typology,
    pub note: String,
    /// Brief fields the preset sets unless the brief does: name, floors, design...
    pub brief: J,
    pub program: Program,
    /// `{rule id: value}` at the preset layer.
    pub rules: J,
    /// The typology as data (`gen_typology.v1`): spaces with their needs, zones, levels below
    /// ground, plant. Its spaces above ground are the programme.
    pub model: Option<std::sync::Arc<super::typology::Model>>,
    programme: Programme,
}

fn all() -> &'static [Preset] {
    static T: OnceLock<Vec<Preset>> = OnceLock::new();
    T.get_or_init(|| {
        let list: Result<Vec<Preset>, String> = BUILTIN
            .iter()
            .map(|t| serde_json::from_str::<J>(t).map_err(|e| e.to_string()).and_then(|v| Preset::from_json(&v)))
            .collect();
        let list = list.unwrap_or_else(|e| panic!("data/presets: {e}"));
        check_names(&list).unwrap_or_else(|e| panic!("data/presets: {e}"));
        list
    })
}

/// Names and aliases are unique and none is a base typology's.
pub fn check_names(list: &[Preset]) -> Result<(), String> {
    for (i, t) in list.iter().enumerate() {
        let taken = |n: &str| {
            Typology::NAMES.iter().any(|x| *x == n)
                || list[..i].iter().any(|o| o.name == n || o.aliases.iter().any(|a| a == n))
        };
        if let Some(n) = std::iter::once(&t.name).chain(&t.aliases).find(|n| taken(n.as_str())) {
            return Err(format!("{}: the name '{n}' is already taken", t.name));
        }
    }
    Ok(())
}

impl Preset {
    /// Reads a preset; every problem names the preset.
    pub fn from_json(t: &J) -> Result<Preset, String> {
        let name = t["name"].as_str().filter(|n| !n.trim().is_empty()).ok_or("a preset without a name")?;
        let name = name.to_string();
        if t.get("schema").is_some_and(|s| s != SCHEMA && s != super::typology::SCHEMA) {
            return Err(format!("{name}: schema must be {SCHEMA} or {}", super::typology::SCHEMA));
        }
        let b = t["base"].as_str().unwrap_or_default();
        let base = Typology::ALL
            .into_iter()
            .find(|x| x.name() == b)
            .ok_or(format!("{name}: unknown base '{b}'"))?;
        let model = match &t["model"] {
            J::Null => None,
            m => {
                let mut md = super::typology::Model::from_json(m).map_err(|e| format!("{name}.model: {}", e.join("; ")))?;
                md.id = name.to_string();
                Some(std::sync::Arc::new(md))
            }
        };
        // a model's spaces above ground are the programme, unless the preset gives its own
        let program = match (&t["program"], &model) {
            (J::Null, Some(m)) => m.program(),
            (J::Null, None) => Program::default(),
            (p, _) => Program::from_json(p).map_err(|e| format!("{name}.program: {}", e.join("; ")))?,
        };
        let mut programme = program.over(&default_programme(base));
        if let Some(sp) = model.as_ref().and_then(|m| m.spare.clone()) {
            programme.spare = sp;
        }
        let mut all_lines = programme.ground.iter().chain(&programme.typical).chain(&programme.single);
        if base != Typology::House && all_lines.any(|l| l.kind == RoomKind::Hall) {
            return Err(format!("{name}: hall rooms belong to houses and apartments"));
        }
        let brief = match &t["brief"] {
            J::Null => J::Object(Map::new()),
            d if d.is_object() => d.clone(),
            _ => return Err(format!("{name}: brief must be an object")),
        };
        if brief.get("typology").is_some() || brief.get("preset").is_some() {
            return Err(format!("{name}: brief may not set the typology or preset"));
        }
        let rules = match &t["rules"] {
            J::Null => J::Object(Map::new()),
            r => r.clone(),
        };
        RuleSet::new()
            .apply_patch(Layer::Preset, &rules)
            .map_err(|e| format!("{name}.rules: {}", e.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("; ")))?;
        let aliases = t["aliases"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        Ok(Preset {
            note: t["note"].as_str().unwrap_or_default().to_string(),
            name,
            aliases,
            base,
            brief,
            program,
            rules,
            model,
            programme,
        })
    }

    pub fn to_json(&self) -> J {
        let mut o = json!({
            "schema": if self.model.is_some() { super::typology::SCHEMA } else { SCHEMA },
            "name": self.name,
            "aliases": self.aliases,
            "base": self.base.name(),
            "note": self.note,
            "brief": self.brief,
            "rules": self.rules,
        });
        // the programme is written with the model, so a programme the user edited survives a save
        o["program"] = self.program.to_json();
        if let Some(m) = &self.model {
            o["model"] = m.to_json();
        }
        o
    }

    /// A base typology as a preset: its built-in programme, no other defaults.
    pub fn for_typology(t: Typology) -> Preset {
        let programme = default_programme(t);
        Preset {
            name: t.name().to_string(),
            aliases: vec![],
            base: t,
            note: String::new(),
            brief: J::Object(Map::new()),
            program: Program::from_programme(&programme),
            rules: J::Object(Map::new()),
            model: None,
            programme,
        }
    }

    pub fn programme(&self) -> &Programme {
        &self.programme
    }

    /// What the reply says about the approximation; empty for a base typology.
    pub fn note(&self) -> String {
        if self.note.is_empty() {
            return String::new();
        }
        format!("approximated: {} (base typology {})", self.note, self.base.name())
    }

    /// The rule set with this preset's values at the preset layer.
    pub fn rule_set(&self) -> RuleSet {
        let mut rs = RuleSet::new();
        // checked when the preset was read
        let _ = rs.apply_patch(Layer::Preset, &self.rules);
        rs
    }

    /// The brief over the preset defaults, with the base typology: what the brief sets wins.
    /// Rules that are aliases of brief fields become those fields' defaults.
    pub fn expand(&self, brief: &J) -> J {
        let mut out = self.brief.clone();
        let mut defaults: Vec<(&str, J)> = vec![];
        let ru = brief["lang"].as_str() == Some("ru");
        if let Some(o) = out.as_object_mut() {
            if let Some(n) = o.get("name").filter(|n| n.is_object()).cloned() {
                let pick = n[if ru { "ru" } else { "en" }].as_str().or(n["en"].as_str());
                o.insert("name".into(), pick.map_or(J::Null, |x| J::String(x.to_string())));
            }
            for (id, val) in self.rules.as_object().into_iter().flatten() {
                let Some((path, x)) = rules::alias_value(id, val) else {
                    continue;
                };
                // a fixed depth sets the bands itself; a default band depth would contradict it
                if path == "band_depth" && !brief["footprint"]["depth"].is_null() {
                    continue;
                }
                // the brief's design replaces the default one whole: some fields exclude others
                if path.starts_with("design.") && !brief["design"].is_null() {
                    continue;
                }
                defaults.push((path, x));
            }
            if !brief["design"].is_null() {
                o.remove("design");
            }
        }
        for (path, x) in defaults {
            rules::set_path(&mut out, path, x);
        }
        merge_patch(&mut out, brief);
        if let Some(o) = out.as_object_mut() {
            o.remove("preset");
        }
        out["typology"] = J::String(self.base.name().to_string());
        out
    }
}

/// A built-in preset by name or alias.
pub fn find(name: &str) -> Option<&'static Preset> {
    // the brief's word in any case: "Krankenhaus" finds the alias "krankenhaus"
    let name = name.trim().to_lowercase();
    all().iter().find(|t| t.name == name || t.aliases.iter().any(|a| *a == name))
}

/// Built-in preset names, without aliases, in data order.
pub fn names() -> Vec<&'static str> {
    all().iter().map(|t| t.name.as_str()).collect()
}

/// Every built-in preset: the base typologies, then the data presets.
pub fn builtin() -> Vec<Preset> {
    Typology::ALL.into_iter().map(Preset::for_typology).chain(all().iter().cloned()).collect()
}

/// The preset a brief asks for: `preset` as a whole preset object or a name, else a
/// `typology` that names a data preset. `None` keeps the brief's own typology.
pub fn of_brief(v: &J) -> Result<Option<Cow<'static, Preset>>, String> {
    match &v["preset"] {
        J::Object(_) => Preset::from_json(&v["preset"]).map(|p| Some(Cow::Owned(p))),
        J::String(s) => match find(s) {
            Some(p) => Ok(Some(Cow::Borrowed(p))),
            None => Typology::ALL
                .into_iter()
                .find(|t| t.name() == s)
                .map(|t| Some(Cow::Owned(Preset::for_typology(t))))
                .ok_or(format!("no preset '{s}'; one of {}", all_names().join(", "))),
        },
        J::Null => Ok(v["typology"].as_str().and_then(find).map(Cow::Borrowed)),
        _ => Err("a preset name or a whole preset object".into()),
    }
}

fn all_names() -> Vec<&'static str> {
    Typology::NAMES.into_iter().chain(names()).collect()
}

/// The user's preset library: `%APPDATA%/ddc/gen_presets`, elsewhere the config directory.
pub fn library_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("ddc").join("gen_presets"))
}

/// A file name for a preset name: letters, digits, `_` and `-`, the rest `_`.
pub fn file_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    format!("{s}.json")
}

/// The presets of a library folder in name order, with a message for each file that failed.
pub fn load_dir(dir: &Path) -> (Vec<Preset>, Vec<String>) {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    files.retain(|p| p.extension().is_some_and(|e| e == "json"));
    files.sort();
    let (mut ok, mut bad) = (vec![], vec![]);
    for f in files {
        let read = std::fs::read_to_string(&f)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<J>(&t).map_err(|e| e.to_string()))
            .and_then(|v| Preset::from_json(&v));
        match read {
            Ok(p) => ok.push(p),
            Err(e) => bad.push(format!("{}: {e}", f.display())),
        }
    }
    (ok, bad)
}

/// Writes a preset into a library folder, replacing one of the same name; the file path.
pub fn save_to(dir: &Path, p: &Preset) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(file_name(&p.name));
    let text = serde_json::to_string_pretty(&p.to_json()).map_err(std::io::Error::other)?;
    std::fs::write(&path, text + "\n")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_loads_and_names_do_not_clash() {
        let n = names();
        for want in ["hotel", "clinic", "warehouse", "mixed_use", "school_campus", "residential"] {
            assert!(n.contains(&want), "{want} in {n:?}");
        }
        assert_eq!(find("hospital").map(|t| t.name.as_str()), Some("hospital_wing"));
        assert_eq!(find("Medical_Centre").map(|t| t.name.as_str()), Some("clinic"));
        let a = Preset::from_json(&json!({"name": "a", "base": "office"})).unwrap();
        let b = Preset::from_json(&json!({"name": "b", "aliases": ["a"], "base": "office"})).unwrap();
        assert!(check_names(&[a, b]).unwrap_err().contains("already taken"));
        let bad = json!({"name": "x", "base": "office",
            "program": {"spaces": [{"kind": "hall", "area": 5, "floor": "typical"}]}});
        assert!(Preset::from_json(&bad).unwrap_err().contains("hall"));
        let rule = json!({"name": "x", "base": "office", "rules": {"corridor.width": 9}});
        assert!(Preset::from_json(&rule).unwrap_err().contains("corridor.width"));
    }

    #[test]
    fn the_brief_wins_over_the_defaults() {
        let t = find("warehouse").unwrap();
        let v = t.expand(&json!({"typology": "logistics", "floors": 2, "lang": "ru",
            "footprint": {"width": 60, "depth": 30}}));
        assert_eq!(v["typology"], "office");
        assert_eq!(v["floors"], 2);
        assert_eq!(v["floor_height"], 5.0);
        assert!(v["band_depth"].is_null(), "{v}");
        assert!(v["name"].as_str().is_some_and(|s| !s.is_ascii()), "{v}");
        assert_eq!(t.rule_set().resolve("depth.band"), Some((11.0, Layer::Preset)));
    }

    #[test]
    fn a_preset_survives_json_and_the_library() {
        for p in builtin() {
            let back = Preset::from_json(&p.to_json()).unwrap();
            assert_eq!(back.programme(), p.programme(), "{}", p.name);
            assert_eq!(back.rules, p.rules);
        }
        let dir = std::env::temp_dir().join(format!("ddc_presets_{}", std::process::id()));
        let mut mine = find("hotel").unwrap().clone();
        mine.name = "my hotel".into();
        mine.aliases.clear();
        let path = save_to(&dir, &mine).unwrap();
        assert!(path.ends_with("my_hotel.json"));
        let (got, bad) = load_dir(&dir);
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(got, vec![mine]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
