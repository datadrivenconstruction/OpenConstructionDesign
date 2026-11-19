// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rules for trimming walls by a roof: project settings (`meta`) with bounds and defaults.
//! Each is explained by the text `why.rt_<key>` (en + ru); changed via `Model::set_roof_rule`
//! (undoable, walls are recomputed in the same operation) and the "Why" window in the app.

use rusqlite::Connection;

use crate::ops::Inv;
use crate::{Error, Model};

pub struct RoofRule {
    pub key: &'static str,
    /// Key in `meta`.
    pub meta: &'static str,
    pub default: f64,
    pub min: f64,
    pub max: f64,
    pub unit: &'static str,
}

/// Conductivity threshold: below it a layer is insulation and is not chosen as the load-bearing layer.
pub const INSULATION_LAMBDA: &str = "insulation_lambda";
/// How far below the roof underside a flat wall top may be for the wall to count as reaching the roof.
pub const REACH: &str = "reach";
/// Fraction of the axis under the roof from which the wall counts as standing under it.
pub const MIN_COVER: &str = "min_cover";
/// Minimum wall height under the roof: below it the attachment is rejected.
pub const MIN_HEIGHT: &str = "min_height";

pub const RULES: [RoofRule; 4] = [
    RoofRule { key: INSULATION_LAMBDA, meta: "insulation_lambda", default: 0.1, min: 0.01, max: 1.0, unit: "W/(m K)" },
    RoofRule { key: REACH, meta: "roof_trim.reach", default: 0.6, min: 0.0, max: 3.0, unit: "m" },
    RoofRule { key: MIN_COVER, meta: "roof_trim.min_cover", default: 0.5, min: 0.1, max: 1.0, unit: "" },
    RoofRule { key: MIN_HEIGHT, meta: "roof_trim.min_height", default: 0.05, min: 0.0, max: 1.0, unit: "m" },
];

pub fn rule_info(key: &str) -> Option<&'static RoofRule> {
    RULES.iter().find(|r| r.key == key)
}

pub(crate) fn value(db: &Connection, key: &str) -> f64 {
    let r = rule_info(key).expect("known roof rule");
    db.query_row("SELECT value FROM meta WHERE key=?1", [r.meta], |x| x.get::<_, f64>(0))
        .ok()
        .filter(|v| v.is_finite() && (r.min..=r.max).contains(v))
        .unwrap_or(r.default)
}

impl Model {
    pub fn roof_rule(&self, key: &str) -> f64 {
        value(&self.db, key)
    }

    /// Project conductivity threshold, W/(m·K).
    pub fn insulation_lambda(&self) -> f64 {
        value(&self.db, INSULATION_LAMBDA)
    }

    pub fn set_insulation_lambda(&mut self, v: f64) -> Result<(), Error> {
        self.set_roof_rule(INSULATION_LAMBDA, v)
    }

    /// Changes a project rule within its bounds; walls attached by the rule are recomputed in the
    /// same operation, with a single undo.
    pub fn set_roof_rule(&mut self, key: &str, v: f64) -> Result<(), Error> {
        let Some(r) = rule_info(key) else {
            return Err(Error::Invalid(format!("unknown roof rule '{key}'")));
        };
        if !v.is_finite() || !(r.min..=r.max).contains(&v) {
            return Err(Error::Invalid(format!("roof rule {key}: {}..{} {}", r.min, r.max, r.unit)));
        }
        if (v - self.roof_rule(key)).abs() < 1e-12 {
            return Ok(());
        }
        self.op("wall_roof_rule", |db| {
            let was: Option<f64> = db.query_row("SELECT value FROM meta WHERE key=?1", [r.meta], |x| x.get(0)).ok();
            let text = |s: &str| rusqlite::types::Value::Text(s.into());
            let inv = match was {
                Some(w) => Inv::default().sql(
                    "INSERT OR REPLACE INTO meta(key,value) VALUES (?1,?2)",
                    vec![text(r.meta), rusqlite::types::Value::Real(w)],
                ),
                None => Inv::default().sql("DELETE FROM meta WHERE key=?1", vec![text(r.meta)]),
            };
            db.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,?2)", rusqlite::params![r.meta, v])?;
            let inv = crate::roof_auto::sync(db, inv, None)?;
            Ok(((), 0, inv))
        })
    }
}
