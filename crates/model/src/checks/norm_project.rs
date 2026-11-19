//! Building-level inputs of the norm rules: the functional fire-hazard class and the building
//! height (typed, or the span of the levels), plus the people on the busiest floor above the
//! first. Stored as properties of `norm_project` row 0 (no schema change).

use rusqlite::types::Value;
use rusqlite::Connection;

use super::rule_cases;
use crate::ops::Inv;
use crate::{Error, Model};

const KIND: &str = "norm_project";
const KEY_CLASS: &str = "fire_class";
const KEY_HEIGHT: &str = "height_m";

/// Classes of functional fire hazard, as the norm writes them (the first letter is Cyrillic).
pub const FIRE_CLASSES: [&str; 25] = [
    "\u{424}1.1",
    "\u{424}1.2",
    "\u{424}1.3",
    "\u{424}1.4",
    "\u{424}2.1",
    "\u{424}2.2",
    "\u{424}2.3",
    "\u{424}2.4",
    "\u{424}3.1",
    "\u{424}3.2",
    "\u{424}3.3",
    "\u{424}3.4",
    "\u{424}3.5",
    "\u{424}3.6",
    "\u{424}4.1",
    "\u{424}4.2",
    "\u{424}4.3",
    "\u{424}4.4",
    "\u{424}5.1",
    "\u{424}5.2",
    "\u{424}5.3",
    "\u{424}5.4",
    "\u{424}5.5",
    "\u{424}5.6",
    "\u{424}5.7",
];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NormProject {
    /// One of `FIRE_CLASSES`, or empty when not set.
    pub fire_class: String,
    /// Building height typed by the engineer, m; `None` takes it from the levels.
    pub height_m: Option<f64>,
}

impl NormProject {
    fn check(&self) -> Result<(), Error> {
        if !self.fire_class.is_empty() && !FIRE_CLASSES.contains(&self.fire_class.as_str()) {
            return Err(Error::Invalid("fire class is not in the list".into()));
        }
        if self.height_m.is_some_and(|h| !h.is_finite() || h < 0.0) {
            return Err(Error::Invalid(
                "building height must be a positive number".into(),
            ));
        }
        Ok(())
    }
}

fn write(db: &Connection, p: &NormProject) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    let fields = [
        (
            KEY_CLASS,
            Value::Text(p.fire_class.clone()),
            !p.fire_class.is_empty(),
        ),
        (
            KEY_HEIGHT,
            Value::Real(p.height_m.unwrap_or(0.0)),
            p.height_m.is_some(),
        ),
    ];
    for (k, v, keep) in fields {
        inv = crate::imported::prop_restore(db, KIND, 0, k)?.then(inv);
        db.execute(
            "DELETE FROM prop WHERE kind=?1 AND ref=0 AND key=?2",
            rusqlite::params![KIND, k],
        )?;
        if keep {
            db.execute(
                "INSERT INTO prop(kind,ref,key,value,unit) VALUES (?1,0,?2,?3,'')",
                rusqlite::params![KIND, k, v],
            )?;
        }
    }
    Ok(inv)
}

const LOCALE_KEYS: [&str; 4] = ["pack", "region", "climate_zone", "typology"];

/// Which country pack judges the project. Empty `pack`: chosen from the UI language and
/// region each time (`effective_norm_pack`); a stored id is the engineer's choice.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NormLocale {
    pub pack: String,
    pub region: String,
    pub climate_zone: String,
    /// `house`, `apartment`, `office`, ...; empty: not stated.
    pub typology: String,
}

impl NormLocale {
    fn values(&self) -> [&str; 4] {
        [&self.pack, &self.region, &self.climate_zone, &self.typology]
    }
    fn check(&self) -> Result<(), Error> {
        if !self.pack.is_empty() && !super::norm_pack::valid_pack_id(&self.pack) {
            return Err(Error::Invalid(format!("norm pack id {:?}", self.pack)));
        }
        let word = |s: &str| s.chars().all(|c| c.is_alphanumeric() || "_-. ".contains(c));
        if !self.values()[1..].iter().all(|v| v.len() <= 64 && word(v)) {
            return Err(Error::Invalid("region, climate zone and typology are short words".into()));
        }
        Ok(())
    }
}

impl Model {
    pub fn norm_locale(&self) -> Result<NormLocale, Error> {
        let mut p = NormLocale::default();
        for (key, v, _) in self.props(KIND, 0)? {
            if let Value::Text(t) = v {
                match key.as_str() {
                    "pack" => p.pack = t,
                    "region" => p.region = t,
                    "climate_zone" => p.climate_zone = t,
                    "typology" => p.typology = t,
                    _ => {}
                }
            }
        }
        Ok(p)
    }

    /// Saves the pack choice in one undo step; `false` when nothing differs.
    pub fn set_norm_locale(&mut self, p: &NormLocale) -> Result<bool, Error> {
        p.check()?;
        if *p == self.norm_locale()? {
            return Ok(false);
        }
        let vals: Vec<String> = p.values().iter().map(|s| s.to_string()).collect();
        self.op("norm_locale", |db| {
            let mut inv = Inv::default();
            for (k, v) in LOCALE_KEYS.iter().zip(&vals) {
                inv = crate::imported::prop_restore(db, KIND, 0, k)?.then(inv);
                db.execute(
                    "DELETE FROM prop WHERE kind=?1 AND ref=0 AND key=?2",
                    rusqlite::params![KIND, k],
                )?;
                if !v.is_empty() {
                    db.execute(
                        "INSERT INTO prop(kind,ref,key,value,unit) VALUES (?1,0,?2,?3,'')",
                        rusqlite::params![KIND, k, v],
                    )?;
                }
            }
            Ok((true, 0, inv))
        })
    }

    /// The pack that judges the project: the stored choice when `set` has it, else the
    /// nearest pack to the UI language and region.
    pub fn effective_norm_pack(
        &self,
        set: &super::norm_pack::PackSet,
        ui_lang: &str,
        ui_region: Option<&str>,
    ) -> Result<String, Error> {
        let p = self.norm_locale()?;
        if !p.pack.is_empty() && set.has(&p.pack) {
            return Ok(p.pack);
        }
        let wanted = super::norm_pack::country_for_locale(ui_lang, ui_region);
        let wanted = if p.region.is_empty() {
            wanted
        } else {
            format!("{wanted}-{}", p.region.to_lowercase())
        };
        Ok(super::norm_pack::nearest_pack(set, &wanted))
    }

    pub fn norm_project(&self) -> Result<NormProject, Error> {
        let mut p = NormProject::default();
        for (key, v, _) in self.props(KIND, 0)? {
            match (key.as_str(), v) {
                (KEY_CLASS, Value::Text(t)) => p.fire_class = t,
                (KEY_HEIGHT, Value::Real(x)) => p.height_m = Some(x),
                _ => {}
            }
        }
        Ok(p)
    }

    /// Saves the class and the typed height in one undo step; `false` when nothing differs.
    pub fn set_norm_project(&mut self, p: &NormProject) -> Result<bool, Error> {
        p.check()?;
        if *p == self.norm_project()? {
            return Ok(false);
        }
        self.op("norm_project", |db| Ok((true, 0, write(db, p)?)))
    }

    /// The typed height, else the top level's elevation above the lowest one, m; `None` with
    /// no levels.
    pub fn building_height(&self) -> Result<Option<f64>, Error> {
        if let Some(h) = self.norm_project()?.height_m {
            return Ok(Some(h));
        }
        let (lo, hi): (Option<f64>, Option<f64>) = self.db.query_row(
            "SELECT MIN(elevation), MAX(elevation) FROM level",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(lo.zip(hi).map(|(a, b)| b - a))
    }

    /// The most people (sum of `Occupants` of the rooms) on any floor but the lowest; `None`
    /// when no room has the count.
    pub fn people_upper_floor(&self) -> Result<Option<i64>, Error> {
        rule_cases::people_upper_floor(self)
    }
}
