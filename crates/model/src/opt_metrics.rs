// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Metrics row of a design option: areas, volumes, counts, cost and findings, with explicit
//! units. Computed on a materialised copy (never on the main model) and cached as JSON in
//! `ai_option.metrics`. In an `alt.db` copy the option filter of `level_totals_in` and
//! `material_totals_in` selects the option; in a branch copy the whole model is the option.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::ai_option::{OptionCopy, OptionMode};
use crate::checks::{run_all, CheckScope, Severity};
use crate::stats::hidden_sql;
use crate::{Error, LevelTotals, MaterialQty, Model, Totals};

/// Project price table: `ai_policy` key, JSON object `{material: price per m3}`.
pub const UNIT_COST_KEY: &str = "unit_cost";
/// Currency of that table: `ai_policy` key, ISO 4217 code (`EUR`, `JPY`).
pub const CURRENCY_KEY: &str = "currency";

/// Units of every number in a metrics row. Prices are per m3 of material volume.
pub const UNITS: Units = Units {
    length: "m",
    area: "m2",
    volume: "m3",
    price: "currency/m3",
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Units {
    pub length: &'static str,
    pub area: &'static str,
    pub volume: &'static str,
    pub price: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoomTypeArea {
    /// Room name: the room type.
    pub name: String,
    pub count: usize,
    pub area: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CostLine {
    pub material: String,
    pub volume: f64,
    pub unit_price: Option<f64>,
    pub cost: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cost {
    /// False when the project has no price table: only quantities are shown.
    pub priced: bool,
    pub lines: Vec<CostLine>,
    /// Sum over priced lines only.
    pub total: f64,
    /// Materials with a quantity and no price; their cost is not in `total`.
    pub missing: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FindingCounts {
    pub errors: usize,
    pub warnings: usize,
    pub info: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OptionMetrics {
    pub levels: Vec<LevelTotals>,
    pub totals: Totals,
    pub room_types: Vec<RoomTypeArea>,
    pub materials: Vec<MaterialQty>,
    pub cost: Cost,
    /// Checks run on the whole model, so only for a branch copy; `None` when not computed.
    pub findings: Option<FindingCounts>,
}

/// One cell of the comparison table.
#[derive(Clone, Debug, PartialEq)]
pub struct MetricCell {
    pub key: String,
    pub unit: &'static str,
    pub value: f64,
}

/// One row of the metrics table.
#[derive(Clone, Debug, PartialEq)]
pub struct MetricsRow {
    pub option: i64,
    pub name: String,
    pub metrics: OptionMetrics,
}

fn by_material(materials: &[MaterialQty]) -> BTreeMap<&str, f64> {
    let mut by_mat: BTreeMap<&str, f64> = BTreeMap::new();
    for m in materials {
        *by_mat.entry(&m.material).or_default() += m.volume;
    }
    by_mat
}

impl OptionMetrics {
    /// Flat cells in a fixed order: the columns of the comparison table.
    pub fn cells(&self) -> Vec<MetricCell> {
        let t = &self.totals;
        let mut v = Vec::new();
        let mut put = |key: &str, unit: &'static str, value: f64| {
            v.push(MetricCell {
                key: key.to_string(),
                unit,
                value,
            })
        };
        let (a, vo, n) = (UNITS.area, UNITS.volume, "count");
        put("walls", n, t.walls as f64);
        put("doors", n, t.doors as f64);
        put("windows", n, t.windows as f64);
        put("slabs", n, t.slabs as f64);
        put("rooms", n, t.rooms as f64);
        put("curtains", n, t.curtains as f64);
        put("wall_plan_area", a, t.wall_plan_area);
        put("slab_area", a, t.slab_area);
        put("room_area", a, t.room_area);
        put("curtain_panel_area", a, t.curtain_panel_area);
        put("wall_volume", vo, t.wall_volume);
        put("slab_volume", vo, t.slab_volume);
        put("room_volume", vo, t.room_volume);
        for l in &self.levels {
            put(&format!("level:{}:room_area", l.name), a, l.totals.room_area);
            put(&format!("level:{}:slab_area", l.name), a, l.totals.slab_area);
        }
        for r in &self.room_types {
            put(&format!("room_type:{}:area", r.name), a, r.area);
        }
        for (m, vol) in by_material(&self.materials) {
            put(&format!("material:{m}:volume"), vo, vol);
        }
        if self.cost.priced {
            put("cost", UNITS.price, self.cost.total);
            put("cost_missing_prices", n, self.cost.missing.len() as f64);
        }
        if let Some(f) = self.findings {
            put("findings_error", n, f.errors as f64);
            put("findings_warning", n, f.warnings as f64);
            put("findings_info", n, f.info as f64);
        }
        v
    }

    /// JSON snapshot for `ai_option.metrics`; every value sits next to its unit.
    pub fn to_json(&self) -> String {
        let cells: Vec<Value> = self
            .cells()
            .iter()
            .map(|c| json!({"key": c.key, "unit": c.unit, "value": c.value}))
            .collect();
        let lines: Vec<Value> = self
            .cost
            .lines
            .iter()
            .map(|l| {
                json!({"material": l.material, "volume": l.volume,
                       "unit_price": l.unit_price, "cost": l.cost})
            })
            .collect();
        let findings = self
            .findings
            .map(|f| json!({"error": f.errors, "warning": f.warnings, "info": f.info}));
        json!({
            "units": {"length": UNITS.length, "area": UNITS.area,
                      "volume": UNITS.volume, "price": UNITS.price},
            "cells": cells,
            "cost": {"priced": self.cost.priced, "total": self.cost.total,
                     "missing_prices": self.cost.missing, "lines": lines},
            "findings": findings,
        })
        .to_string()
    }
}

fn cost_of(materials: &[MaterialQty], prices: &BTreeMap<String, f64>) -> Cost {
    let mut cost = Cost {
        priced: !prices.is_empty(),
        lines: Vec::new(),
        total: 0.0,
        missing: Vec::new(),
    };
    for (material, volume) in by_material(materials) {
        let unit_price = prices.get(material).copied();
        let c = unit_price.map(|p| p * volume);
        match c {
            Some(c) => cost.total += c,
            None => cost.missing.push(material.to_string()),
        }
        cost.lines.push(CostLine {
            material: material.to_string(),
            volume,
            unit_price,
            cost: c,
        });
    }
    cost
}

impl Model {
    /// Price table of the project (`unit_cost`): per m3 of material. Empty when not set.
    pub fn unit_prices(&self) -> Result<BTreeMap<String, f64>, Error> {
        match self.ai_policy(UNIT_COST_KEY)? {
            None => Ok(BTreeMap::new()),
            Some(s) => serde_json::from_str(&s)
                .map_err(|e| Error::Invalid(format!("bad {UNIT_COST_KEY} table: {e}"))),
        }
    }

    /// Currency of the price table (ISO 4217, upper case); `None` when unset or not a code.
    pub fn currency(&self) -> Result<Option<String>, Error> {
        let c = self.ai_policy(CURRENCY_KEY)?.map(|s| s.trim().to_ascii_uppercase());
        Ok(c.filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase())))
    }

    /// Metrics of this model as the option `option` sees it (`None`: primary options, i.e. the
    /// whole model of a branch copy). Read-only. Findings run only with `findings`, and are
    /// reported only when `option` is `None`: checks do not know the option filter.
    pub fn option_metrics(
        &self,
        option: Option<i64>,
        prices: &BTreeMap<String, f64>,
        findings: bool,
    ) -> Result<OptionMetrics, Error> {
        let levels = self.level_totals_in(option)?;
        let mut totals = Totals::default();
        for l in &levels {
            totals.add(&l.totals);
        }
        let materials = self.material_totals_in(None, option)?;
        let room_types = {
            let sql = format!(
                "SELECT r.name, count(*), coalesce(sum(q.area),0) FROM room r \
                 LEFT JOIN room_qty q ON q.room=r.id WHERE ?1 IS NULL AND NOT {} \
                 GROUP BY r.name ORDER BY r.name",
                hidden_sql("'room'", "r.id")
            );
            let mut st = self.sql().prepare(&sql)?;
            let v = st
                .query_map(rusqlite::params![None::<i64>, option], |r| {
                    Ok(RoomTypeArea {
                        name: r.get(0)?,
                        count: r.get::<_, i64>(1)? as usize,
                        area: r.get(2)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            v
        };
        let findings = if findings && option.is_none() {
            let mut c = FindingCounts::default();
            for f in run_all(self, None, &CheckScope::All, usize::MAX)? {
                match f.severity {
                    Severity::Error => c.errors += 1,
                    Severity::Warning => c.warnings += 1,
                    Severity::Info => c.info += 1,
                }
            }
            Some(c)
        } else {
            None
        };
        Ok(OptionMetrics {
            cost: cost_of(&materials, prices),
            levels,
            totals,
            room_types,
            materials,
            findings,
        })
    }
}

/// Metrics of a materialised option. Opens only the copy; the main model is not touched.
/// A branch copy is the option as a whole, so checks run there; an `alt.db` copy is filtered
/// to the option's alternative.
pub fn option_copy_metrics(
    copy: &OptionCopy,
    prices: &BTreeMap<String, f64>,
) -> Result<OptionMetrics, Error> {
    let path = copy
        .path
        .to_str()
        .ok_or_else(|| Error::Invalid("non-UTF-8 path".into()))?;
    let m = Model::open_copy(path)?;
    match copy.mode {
        OptionMode::Alternative => m.option_metrics(copy.alternative, prices, false),
        OptionMode::Branch => m.option_metrics(None, prices, true),
    }
}

#[cfg(test)]
#[path = "opt_metrics_tests.rs"]
mod tests;
