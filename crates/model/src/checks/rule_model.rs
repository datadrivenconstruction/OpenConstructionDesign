// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rules built on existing model checks: integrity, wall warnings, clashes,
//! element levels.

use super::util::{key, measured, Sink, Within};
use super::{CheckRule, CheckScope, Finding, Fix, Measure, Severity};
use crate::{ClashItem, ClashParams, ClashSide, ClashStatus, ElKey, Error, Model};

pub(super) struct Integrity;

impl CheckRule for Integrity {
    fn id(&self) -> &'static str {
        "integrity"
    }
    fn heavy(&self) -> bool {
        true
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        let mut s = Sink::new(out, self.id());
        for i in m.check_integrity()? {
            if !w.has(&i.table, i.id) || !w.in_level(m, &i.table, i.id) {
                continue;
            }
            let k = key(&i.table, i.id);
            // an opening without a wall can only be removed; a person decides the rest
            let fix = if i.table == "opening" {
                Fix::Delete(vec![k.clone()])
            } else {
                Fix::None
            };
            s.add(
                Severity::Error,
                vec![k],
                None,
                "ai.check.integrity",
                vec![("table", i.table), ("problem", i.problem)],
                fix,
                Measure::flag(),
            );
        }
        s.done();
        Ok(())
    }
}

pub(super) struct WallWarnings;

impl CheckRule for WallWarnings {
    fn id(&self) -> &'static str {
        "wall_warnings"
    }
    fn heavy(&self) -> bool {
        true
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        let mut s = Sink::new(out, self.id());
        for e in m.wall_warnings()?.into_iter().chain(m.cut_warnings()?) {
            if w.has(e.table, e.id) && w.in_level(m, e.table, e.id) {
                s.add(
                    Severity::Warning,
                    vec![key(e.table, e.id)],
                    None,
                    "ai.check.wall_warning",
                    vec![("problem", e.message)],
                    Fix::None,
                    Measure::flag(),
                );
            }
        }
        s.done();
        Ok(())
    }
}

pub(super) struct LevelMismatch;

impl CheckRule for LevelMismatch {
    fn id(&self) -> &'static str {
        "level_mismatch"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        let mut s = Sink::new(out, self.id());
        for table in ["col", "stair", "shaft"] {
            let sql = format!(
                "SELECT t.id, (a.elevation+t.base_offset)-(b.elevation+t.top_offset) FROM {table} t JOIN level a ON a.id=t.level JOIN level b ON b.id=t.top_level \
                 WHERE b.elevation+t.top_offset <= a.elevation+t.base_offset+1e-6{} ORDER BY t.id",
                w.level_sql("t.level")
            );
            // measured: top minus base, must exceed 0 m
            for (id, below) in measured(m, &sql)? {
                if w.has(table, id) {
                    s.add(
                        Severity::Error,
                        vec![key(table, id)],
                        None,
                        "ai.check.level_top_below_base",
                        vec![("table", table.to_string())],
                        Fix::None,
                        Measure::new(-below, 0.0, "m"),
                    );
                }
            }
        }
        let sql = format!(
            "SELECT id, height FROM wall WHERE height<=1e-6{} ORDER BY id",
            w.level_sql("level")
        );
        for (id, height) in measured(m, &sql)? {
            if w.has("wall", id) {
                s.add(
                    Severity::Error,
                    vec![key("wall", id)],
                    None,
                    "ai.check.level_top_below_base",
                    vec![("table", "wall".to_string())],
                    Fix::None,
                    Measure::new(height, 0.0, "m"),
                );
            }
        }
        for (column, host) in [
            ("host_wall", "wall"),
            ("host_slab", "slab"),
            ("host_ceiling", "ceiling"),
        ] {
            let sql = format!(
                "SELECT c.id, ABS((SELECT elevation FROM level WHERE id=c.level)-(SELECT elevation FROM level WHERE id=h.level)) FROM comp c JOIN {host} h ON h.id=c.{column} WHERE c.level<>h.level{} ORDER BY c.id",
                w.level_sql("c.level")
            );
            for (id, apart) in measured(m, &sql)? {
                if w.has("comp", id) {
                    s.add(
                        Severity::Warning,
                        vec![key("comp", id)],
                        None,
                        "ai.check.level_host_differs",
                        vec![("host", host.to_string())],
                        Fix::None,
                        Measure::new(apart, 0.0, "m"),
                    );
                }
            }
        }
        s.done();
        Ok(())
    }
}

/// How many clash fixes to compute: each is a trace, and only the first few get looked at.
const CLASH_FIXES: usize = 100;
const CLASH_GAP: f64 = 0.05;

pub(super) struct Clashes;

fn model_key(k: &ElKey) -> ElKey {
    match k.0.as_str() {
        "column" => key("col", k.1),
        "duct" | "pipe" | "tray" | "conduit" => key("mep_run", k.1),
        _ => k.clone(),
    }
}

impl CheckRule for Clashes {
    fn id(&self) -> &'static str {
        "clashes"
    }
    fn heavy(&self) -> bool {
        true
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        let mut s = Sink::new(out, self.id());
        // structural elements against each other: the same selection as the clash tool with wall/column
        for h in m.clash_find(&ClashParams::structure())? {
            let (a, b) = (model_key(&h.a), model_key(&h.b));
            if !(w.has(&a.0, a.1) || w.has(&b.0, b.1)) {
                continue;
            }
            if let (Some(l), Some(hit)) = (w.level, h.level) {
                if l != hit {
                    continue;
                }
            }
            s.add(
                Severity::Warning,
                vec![a, b],
                Some(h.at()),
                "ai.check.clash",
                vec![("a", h.a.0.clone()), ("b", h.b.0.clone())],
                Fix::None,
                Measure::new(h.volume, 0.0, "m3"),
            );
        }
        let any: bool =
            m.db.query_row("SELECT EXISTS(SELECT 1 FROM mep_run)", [], |r| r.get(0))?;
        if !any {
            s.done();
            return Ok(());
        }
        let p = ClashParams {
            a: ClashSide::cats(&["duct", "pipe", "tray", "conduit"]),
            b: ClashSide::cats(&["wall", "slab", "column", "beam"]),
            ..ClashParams::default()
        };
        let test = p.test_key();
        let mut fixes = 0;
        for h in m.clash_find(&p)? {
            let (a, b) = (model_key(&h.a), model_key(&h.b));
            if !(w.has(&a.0, a.1) || w.has(&b.0, b.1)) {
                continue;
            }
            if let (Some(l), Some(hit)) = (w.level, h.level) {
                if l != hit {
                    continue;
                }
            }
            let mut fix = Fix::None;
            if fixes < CLASH_FIXES && !s.full() {
                fixes += 1;
                let item = ClashItem {
                    test: test.clone(),
                    a: h.a.clone(),
                    b: h.b.clone(),
                    status: ClashStatus::New,
                    comment: String::new(),
                    volume: h.volume,
                    lo: h.lo,
                    hi: h.hi,
                    level: h.level,
                    present: true,
                    first_run: 0,
                    last_run: 0,
                };
                let host_cut = h.b.0 == "wall" || h.b.0 == "slab";
                let made = if host_cut {
                    m.clash_fix_sleeve(&item, CLASH_GAP)
                } else {
                    m.clash_fix_reroute(&item, true, CLASH_GAP)
                };
                if let Ok(c) = made {
                    fix = Fix::Clash(c);
                }
            }
            let at = [
                (h.lo[0] + h.hi[0]) / 2.0,
                (h.lo[1] + h.hi[1]) / 2.0,
                (h.lo[2] + h.hi[2]) / 2.0,
            ];
            s.add(
                Severity::Error,
                vec![a, b],
                Some(at),
                "ai.check.clash",
                vec![("a", h.a.0.clone()), ("b", h.b.0.clone())],
                fix,
                Measure::new(h.volume, 0.0, "m3"),
            );
        }
        s.done();
        Ok(())
    }
}
