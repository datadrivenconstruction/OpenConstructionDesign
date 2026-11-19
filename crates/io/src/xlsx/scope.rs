// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! What an export carries: the whole model, one level, a selection or chosen categories.
//! Rows outside the scope are never written, so the baseline never lists them and a re-import
//! (even with "delete missing") leaves those elements alone.

use std::collections::HashSet;

use ddc_model::Model;

/// A category sheet the scope can pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cat {
    Walls,
    Openings,
    Rooms,
    Slabs,
    Curtains,
}

/// Selected element ids per category.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selected {
    pub walls: Vec<i64>,
    pub openings: Vec<i64>,
    pub rooms: Vec<i64>,
    pub slabs: Vec<i64>,
    pub curtains: Vec<i64>,
}

/// Filters combine: an element is exported when it passes every filter that is set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scope {
    /// Human text for the Info sheet and the preview header ("Level 2, walls+doors").
    pub label: String,
    pub level: Option<i64>,
    pub selected: Option<Selected>,
    pub cats: Option<Vec<Cat>>,
}

impl Cat {
    fn code(self) -> &'static str {
        match self {
            Cat::Walls => "walls",
            Cat::Openings => "openings",
            Cat::Rooms => "rooms",
            Cat::Slabs => "slabs",
            Cat::Curtains => "curtains",
        }
    }

    fn from_code(c: &str) -> Option<Cat> {
        [Cat::Walls, Cat::Openings, Cat::Rooms, Cat::Slabs, Cat::Curtains]
            .into_iter()
            .find(|k| k.code() == c)
    }
}

impl Selected {
    /// Every element id of the model, per category.
    pub fn of_model(model: &Model) -> Result<Selected, String> {
        let all = |t: &str| -> Result<Vec<i64>, String> {
            let mut st = model
                .sql()
                .prepare(&format!("SELECT id FROM {t} ORDER BY id"))
                .map_err(|e| e.to_string())?;
            let rows = st.query_map([], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
        };
        Ok(Selected {
            walls: all("wall")?,
            openings: all("opening")?,
            rooms: all("room")?,
            slabs: all("slab")?,
            curtains: all("curtain_wall")?,
        })
    }

    /// Adds the ids present in `after` but not in `before` (what an Apply created).
    pub fn join_new(&mut self, before: &Selected, after: &Selected) {
        fn add(dst: &mut Vec<i64>, before: &[i64], after: &[i64]) {
            let old: HashSet<i64> = before.iter().copied().collect();
            for id in after {
                if !old.contains(id) && !dst.contains(id) {
                    dst.push(*id);
                }
            }
        }
        add(&mut self.walls, &before.walls, &after.walls);
        add(&mut self.openings, &before.openings, &after.openings);
        add(&mut self.rooms, &before.rooms, &after.rooms);
        add(&mut self.slabs, &before.slabs, &after.slabs);
        add(&mut self.curtains, &before.curtains, &after.curtains);
    }
}

impl Scope {
    pub fn is_whole(&self) -> bool {
        self.level.is_none() && self.selected.is_none() && self.cats.is_none()
    }

    /// Machine form for the Info sheet ("level=3;cats=walls,rooms;selection"); empty = whole.
    /// A selection's ids are not stored: the Baseline sheet already lists them.
    pub fn to_spec(&self) -> String {
        let mut parts = Vec::new();
        if let Some(l) = self.level {
            parts.push(format!("level={l}"));
        }
        if let Some(c) = &self.cats {
            let names: Vec<&str> = c.iter().map(|k| k.code()).collect();
            parts.push(format!("cats={}", names.join(",")));
        }
        if self.selected.is_some() {
            parts.push("selection".into());
        }
        parts.join(";")
    }

    /// Scope of a reopened book: `spec` from its Info sheet, `label` the human text, `base`
    /// the ids its Baseline lists. None = a whole-model book. A scoped book whose spec is
    /// missing or unreadable (older version) falls back to its Baseline ids: narrower, never wider.
    pub fn from_spec(spec: &str, label: &str, base: &Selected) -> Option<Scope> {
        if spec.is_empty() && label.is_empty() {
            return None;
        }
        let fallback = || Scope {
            label: label.to_string(),
            selected: Some(base.clone()),
            ..Scope::default()
        };
        if spec.is_empty() {
            return Some(fallback());
        }
        let mut sc = Scope { label: label.to_string(), ..Scope::default() };
        for t in spec.split(';') {
            if let Some(v) = t.strip_prefix("level=") {
                match v.parse() {
                    Ok(l) => sc.level = Some(l),
                    Err(_) => return Some(fallback()),
                }
            } else if let Some(v) = t.strip_prefix("cats=") {
                let mut cats = Vec::new();
                for c in v.split(',').filter(|c| !c.is_empty()) {
                    match Cat::from_code(c) {
                        Some(k) => cats.push(k),
                        None => return Some(fallback()),
                    }
                }
                sc.cats = Some(cats);
            } else if t == "selection" {
                sc.selected = Some(base.clone());
            } else {
                return Some(fallback());
            }
        }
        if sc.is_whole() {
            return Some(fallback());
        }
        Some(sc)
    }
}

/// Rows of `old` (ids a book lists) that an export with `scope` would not write; None = whole model.
pub fn rows_outside(model: &Model, scope: Option<&Scope>, old: &Selected) -> Result<usize, String> {
    let Some(sc) = scope else {
        return Ok(0);
    };
    let a = resolve(model, sc)?;
    let n = |ids: &[i64], ok: &Option<HashSet<i64>>| match ok {
        None => 0,
        Some(set) => ids.iter().filter(|i| !set.contains(i)).count(),
    };
    Ok(n(&old.walls, &a.walls)
        + n(&old.openings, &a.openings)
        + n(&old.rooms, &a.rooms)
        + n(&old.slabs, &a.slabs)
        + n(&old.curtains, &a.curtains))
}

/// Ids allowed per category; None = every element.
#[derive(Default)]
pub(crate) struct Allowed {
    pub walls: Option<HashSet<i64>>,
    pub openings: Option<HashSet<i64>>,
    pub rooms: Option<HashSet<i64>>,
    pub slabs: Option<HashSet<i64>>,
    pub curtains: Option<HashSet<i64>>,
}

fn ids_of(model: &Model, sql: &str, level: i64) -> Result<HashSet<i64>, String> {
    let mut st = model.sql().prepare(sql).map_err(|e| e.to_string())?;
    let rows = st
        .query_map([level], |r| r.get::<_, i64>(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

fn narrow(slot: &mut Option<HashSet<i64>>, with: HashSet<i64>) {
    *slot = Some(match slot.take() {
        Some(s) => s.intersection(&with).copied().collect(),
        None => with,
    });
}

pub(crate) fn resolve(model: &Model, sc: &Scope) -> Result<Allowed, String> {
    let mut a = Allowed::default();
    if let Some(lv) = sc.level {
        narrow(&mut a.walls, ids_of(model, "SELECT id FROM wall WHERE level=?1", lv)?);
        narrow(
            &mut a.openings,
            ids_of(
                model,
                "SELECT o.id FROM opening o JOIN wall w ON w.id=o.wall WHERE w.level=?1",
                lv,
            )?,
        );
        narrow(&mut a.rooms, ids_of(model, "SELECT id FROM room WHERE level=?1", lv)?);
        narrow(&mut a.slabs, ids_of(model, "SELECT id FROM slab WHERE level=?1", lv)?);
        narrow(
            &mut a.curtains,
            ids_of(model, "SELECT id FROM curtain_wall WHERE level=?1", lv)?,
        );
    }
    if let Some(s) = &sc.selected {
        let set = |v: &[i64]| v.iter().copied().collect::<HashSet<_>>();
        narrow(&mut a.walls, set(&s.walls));
        narrow(&mut a.openings, set(&s.openings));
        narrow(&mut a.rooms, set(&s.rooms));
        narrow(&mut a.slabs, set(&s.slabs));
        narrow(&mut a.curtains, set(&s.curtains));
    }
    if let Some(cats) = &sc.cats {
        let none = |c: Cat, slot: &mut Option<HashSet<i64>>| {
            if !cats.contains(&c) {
                *slot = Some(HashSet::new());
            }
        };
        none(Cat::Walls, &mut a.walls);
        none(Cat::Openings, &mut a.openings);
        none(Cat::Rooms, &mut a.rooms);
        none(Cat::Slabs, &mut a.slabs);
        none(Cat::Curtains, &mut a.curtains);
    }
    Ok(a)
}

/// Keeps the rows whose first cell (an id) is allowed.
pub(crate) fn keep(rows: Vec<Vec<super::export::V>>, allowed: &Option<HashSet<i64>>) -> Vec<Vec<super::export::V>> {
    match allowed {
        None => rows,
        Some(set) => rows
            .into_iter()
            .filter(|r| matches!(r.first(), Some(super::export::V::N(v)) if set.contains(&(*v as i64))))
            .collect(),
    }
}
