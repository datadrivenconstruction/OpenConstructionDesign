// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rules that compare two values of one element (`Compare`) and rules whose bounds depend on
//! other values of the element (`Cases`): the corridor width by the people it serves, a room
//! height by the room's purpose. A missing input is a note, never a verdict.

use std::collections::BTreeMap;

use super::rule_def::{
    args, builtin_unit, range_text, source, source_unit, values_sql, violators, Cat,
};
use super::util::{key, Sink, Within};
use super::{Case, CmpOp, Fix, Measure, RuleCheck, RuleDef, Severity};
use crate::{Error, Model};

/// Value name for a room: the narrowest clear width of its boundary, m.
pub const MIN_WIDTH: &str = "min_width";

type Column = BTreeMap<i64, Option<String>>;

fn column(m: &Model, cat: &Cat, r: &RuleDef, w: &Within, param: &str) -> Result<Column, Error> {
    let src = source(m, cat, param)?;
    let inner = values_sql(cat, &r.scope, w, &src, param);
    let sql = format!("SELECT id, val FROM ({inner}) ORDER BY id");
    Ok(violators(m, &sql)?.into_iter().collect())
}

fn number(v: Option<&String>) -> Option<f64> {
    v?.trim().parse::<f64>().ok().filter(|x| x.is_finite())
}

/// The ray from the middle of every long edge, straight into the room, to the next boundary:
/// the least of those lengths is the clear width of a corridor-like room. `None` for a ring
/// with fewer than three points.
pub fn ring_min_width(ring: &[(f64, f64)]) -> Option<f64> {
    let n = ring.len();
    if n < 3 {
        return None;
    }
    let area2: f64 = (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    if area2.abs() < 1e-12 {
        return None;
    }
    let ccw = area2 > 0.0;
    let mut best: Option<f64> = None;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = dx.hypot(dy);
        if len < 0.2 {
            continue;
        }
        // inward normal: left of the direction for a counter-clockwise ring
        let (nx, ny) = if ccw {
            (-dy / len, dx / len)
        } else {
            (dy / len, -dx / len)
        };
        for t in [0.2, 0.35, 0.5, 0.65, 0.8] {
            let p = (a.0 + dx * t, a.1 + dy * t);
            let hit = (0..n)
                .filter(|j| *j != i)
                .filter_map(|j| ray_segment(p, (nx, ny), ring[j], ring[(j + 1) % n]))
                .fold(f64::INFINITY, f64::min);
            if hit.is_finite() {
                best = Some(best.map_or(hit, |x| x.min(hit)));
            }
        }
    }
    best
}

/// Distance along the unit ray `d` from `p` to segment `a`-`b`, when it hits ahead.
fn ray_segment(p: (f64, f64), d: (f64, f64), a: (f64, f64), b: (f64, f64)) -> Option<f64> {
    let (ex, ey) = (b.0 - a.0, b.1 - a.1);
    let den = d.0 * ey - d.1 * ex;
    if den.abs() < 1e-12 {
        return None;
    }
    let (qx, qy) = (a.0 - p.0, a.1 - p.1);
    let s = (qx * ey - qy * ex) / den;
    let u = (qx * d.1 - qy * d.0) / den;
    (s > 1e-9 && (-1e-9..=1.0 + 1e-9).contains(&u)).then_some(s)
}

fn room_widths(m: &Model, ids: &[i64]) -> Result<Column, Error> {
    let mut out = Column::new();
    for id in ids {
        let w = m
            .room_quantities(*id)?
            .and_then(|q| q.rings.first().and_then(|r| ring_min_width(r)));
        out.insert(*id, w.map(|x| x.to_string()));
    }
    Ok(out)
}

fn value_column(
    m: &Model,
    cat: &Cat,
    r: &RuleDef,
    w: &Within,
    name: &str,
) -> Result<Column, Error> {
    if name == MIN_WIDTH && cat.table == "room" {
        let ids: Vec<i64> = column(m, cat, r, w, "area")?.into_keys().collect();
        return room_widths(m, &ids);
    }
    column(m, cat, r, w, name)
}

pub(super) fn run(
    m: &Model,
    r: &RuleDef,
    cat: &Cat,
    w: &Within,
    s: &mut Sink<'_>,
) -> Result<(), Error> {
    match &r.check {
        RuleCheck::Compare { a, op, b, factor } => compare(m, r, cat, w, (a, *op, b, *factor), s),
        RuleCheck::Cases {
            value,
            require,
            cases,
        } => cases_run(m, r, cat, w, (value, require, cases), s),
        _ => Ok(()),
    }
}

fn compare(
    m: &Model,
    r: &RuleDef,
    cat: &Cat,
    w: &Within,
    (a, op, b, factor): (&str, CmpOp, &str, f64),
    s: &mut Sink<'_>,
) -> Result<(), Error> {
    let (ca, cb) = (column(m, cat, r, w, a)?, column(m, cat, r, w, b)?);
    for (id, va) in &ca {
        if !w.has(cat.table, *id) {
            continue;
        }
        let (Some(x), Some(y)) = (
            number(va.as_ref()),
            number(cb.get(id).and_then(|v| v.as_ref())),
        ) else {
            continue;
        };
        let limit = y * factor;
        let ok = match op {
            CmpOp::Lt => x < limit - 1e-9,
            CmpOp::Le => x <= limit + 1e-9,
            CmpOp::Gt => x > limit + 1e-9,
            CmpOp::Ge => x >= limit - 1e-9,
        };
        if ok {
            continue;
        }
        let want = if (factor - 1.0).abs() < 1e-12 {
            format!("{} {b}", op.symbol())
        } else {
            format!("{} {b} x {factor}", op.symbol())
        };
        s.add(
            r.severity,
            vec![key(cat.table, *id)],
            None,
            "ai.check.rule.compare",
            args(r, a, &x.to_string(), want),
            Fix::None,
            Measure::new(x, limit, builtin_unit(a)),
        );
    }
    Ok(())
}

/// A building-level input (`project.*`) as text; `None` when it is not set.
fn project_input(m: &Model, name: &str) -> Result<Option<String>, Error> {
    Ok(match name {
        "project.fire_class" => Some(m.norm_project()?.fire_class).filter(|c| !c.is_empty()),
        "project.height" => m.building_height()?.map(|h| h.to_string()),
        "project.people_upper" => m.people_upper_floor()?.map(|n| n.to_string()),
        _ => None,
    })
}

/// The most people on a floor above the lowest: sum of the rooms' `Occupants` per level.
pub(super) fn people_upper_floor(m: &Model) -> Result<Option<i64>, Error> {
    let Some(cat) = super::rule_def::cat_of("room") else {
        return Ok(None);
    };
    let src = source(m, &cat, "Occupants")?;
    let plain = super::RuleScope {
        cats: vec![],
        levels: None,
        type_name: None,
        when: None,
    };
    let inner = values_sql(
        &cat,
        &plain,
        &Within::new(&super::CheckScope::All),
        &src,
        "Occupants",
    );
    let sql = format!("SELECT id, val FROM ({inner}) ORDER BY id");
    let counts: BTreeMap<i64, i64> = violators(m, &sql)?
        .into_iter()
        .filter_map(|(id, v)| Some((id, v?.trim().parse::<f64>().ok()? as i64)))
        .collect();
    if counts.is_empty() {
        return Ok(None);
    }
    let lowest: Option<i64> = m.db.query_row(
        "SELECT id FROM level ORDER BY elevation, id LIMIT 1",
        [],
        |r| r.get(0),
    )?;
    let mut per_level: BTreeMap<i64, i64> = BTreeMap::new();
    let mut st = m.db.prepare("SELECT id, level FROM room")?;
    for row in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (id, level) = row?;
        if let Some(n) = counts.get(&id) {
            *per_level.entry(level).or_default() += n;
        }
    }
    Ok(per_level
        .into_iter()
        .filter(|(l, _)| Some(*l) != lowest)
        .map(|(_, n)| n)
        .max()
        .or(Some(0)))
}

fn note(s: &mut Sink<'_>, r: &RuleDef, table: &str, id: i64, msg: &'static str, param: &str) {
    s.add(
        Severity::Info,
        vec![key(table, id)],
        None,
        msg,
        args(r, param, "", String::new()),
        Fix::None,
        Measure::flag(),
    );
}

fn cases_run(
    m: &Model,
    r: &RuleDef,
    cat: &Cat,
    w: &Within,
    (value, require, cases): (&str, &[String], &[Case]),
    s: &mut Sink<'_>,
) -> Result<(), Error> {
    let values = value_column(m, cat, r, w, value)?;
    let unit = match source_unit(&source(m, cat, value)?, value) {
        "" => builtin_unit(value),
        u => u,
    };
    let mut inputs: BTreeMap<String, Column> = BTreeMap::new();
    let mut building: BTreeMap<String, Option<String>> = BTreeMap::new();
    let names = cases
        .iter()
        .flat_map(|c| c.conds.iter().map(|k| k.param()))
        .chain(require.iter().map(String::as_str));
    for n in names {
        if n.starts_with("project.") {
            if !building.contains_key(n) {
                building.insert(n.to_string(), project_input(m, n)?);
            }
        } else if !inputs.contains_key(n) {
            inputs.insert(n.to_string(), column(m, cat, r, w, n)?);
        }
    }
    let input = |p: &str, id: i64| -> Option<&str> {
        match building.get(p) {
            Some(v) => v.as_deref(),
            None => inputs
                .get(p)
                .and_then(|c| c.get(&id))
                .and_then(|v| v.as_deref()),
        }
    };
    let blank = |p: &str, id: i64| input(p, id).is_none_or(|v| v.trim().is_empty());
    for (id, v) in &values {
        if !w.has(cat.table, *id) {
            continue;
        }
        if let Some(p) = require.iter().find(|p| blank(p, *id)) {
            note(s, r, cat.table, *id, "ai.check.rule.unset", p);
            continue;
        }
        let chosen = cases.iter().find(|c| {
            c.conds
                .iter()
                .all(|k| k.holds(input(k.param(), *id)) == Some(true))
        });
        let Some(case) = chosen else {
            // nothing fits: a note only when an input is missing, otherwise the rule is silent
            let gap = cases
                .iter()
                .flat_map(|c| &c.conds)
                .find(|k| k.holds(input(k.param(), *id)).is_none());
            if let Some(k) = gap {
                note(s, r, cat.table, *id, "ai.check.rule.unset", k.param());
            }
            continue;
        };
        if case.undefined {
            note(s, r, cat.table, *id, "ai.check.rule.undefined", value);
            continue;
        }
        let Some(x) = number(v.as_ref()) else {
            note(s, r, cat.table, *id, "ai.check.rule.unset", value);
            continue;
        };
        let below = case.min.filter(|a| x < *a - 1e-9);
        let above = case.max.filter(|b| x > *b + 1e-9);
        let Some(limit) = below.or(above) else {
            continue;
        };
        let mut a = args(r, value, &x.to_string(), range_text(case.min, case.max));
        if let Some(c) = &case.clause {
            a.push(("clause", c.clone()));
        }
        s.add(
            r.severity,
            vec![key(cat.table, *id)],
            None,
            "ai.check.rule.param_range",
            a,
            Fix::None,
            Measure::new(x, limit, unit),
        );
    }
    Ok(())
}
