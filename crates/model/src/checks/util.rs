//! What rules share: the check scope and a cap on findings per rule.

use std::collections::{HashMap, HashSet};

use super::{CheckScope, Finding, Fix, Measure, Severity};
use crate::{ElKey, Error, Model};

/// A person cannot work through more findings of one rule, and the response would become huge.
pub(super) const RULE_CAP: usize = 500;

pub(super) struct Within {
    ids: Option<HashMap<String, HashSet<i64>>>,
    pub level: Option<i64>,
}

impl Within {
    pub fn new(scope: &CheckScope) -> Self {
        match scope {
            CheckScope::All => Within {
                ids: None,
                level: None,
            },
            CheckScope::Level(l) => Within {
                ids: None,
                level: Some(*l),
            },
            CheckScope::Elements(list) => {
                let mut ids: HashMap<String, HashSet<i64>> = HashMap::new();
                for (t, id) in list {
                    ids.entry(t.clone()).or_default().insert(*id);
                }
                Within {
                    ids: Some(ids),
                    level: None,
                }
            }
        }
    }

    pub fn has(&self, table: &str, id: i64) -> bool {
        match &self.ids {
            None => true,
            Some(m) => m.get(table).is_some_and(|s| s.contains(&id)),
        }
    }

    pub fn is_elements(&self) -> bool {
        self.ids.is_some()
    }

    /// SQL level condition; `expr` is a column or a subquery of the rule itself.
    pub fn level_sql(&self, expr: &str) -> String {
        match self.level {
            Some(l) => format!(" AND {expr}={l}"),
            None => String::new(),
        }
    }

    /// For warnings the model emits for all levels at once.
    pub fn in_level(&self, m: &Model, table: &str, id: i64) -> bool {
        let Some(l) = self.level else { return true };
        if !LEVELED.contains(&table) {
            return true;
        }
        m.db.query_row(
            &format!("SELECT level FROM {table} WHERE id=?1"),
            [id],
            |r| r.get::<_, i64>(0),
        )
        .map_or(true, |x| x == l)
    }
}

const LEVELED: [&str; 12] = [
    "wall",
    "curtain_wall",
    "slab",
    "room",
    "col",
    "beam",
    "roof",
    "ceiling",
    "stair",
    "railing",
    "mep_run",
    "comp",
];

pub(super) struct Sink<'a> {
    out: &'a mut Vec<Finding>,
    rule: &'static str,
    kept: usize,
    over: usize,
}

impl<'a> Sink<'a> {
    pub fn new(out: &'a mut Vec<Finding>, rule: &'static str) -> Self {
        Sink {
            out,
            rule,
            kept: 0,
            over: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &mut self,
        severity: Severity,
        elements: Vec<ElKey>,
        at: Option<[f64; 3]>,
        msg_key: &'static str,
        args: Vec<(&'static str, String)>,
        fix: Fix,
        measure: Measure,
    ) {
        if self.kept >= RULE_CAP {
            self.over += 1;
            return;
        }
        self.kept += 1;
        self.out.push(Finding {
            rule: self.rule,
            severity,
            elements,
            at,
            msg_key,
            args,
            fix,
            measure: Some(measure),
        });
    }

    pub fn full(&self) -> bool {
        self.kept >= RULE_CAP
    }

    /// Summary: how many findings did not fit, so silence is not read as their absence.
    pub fn done(self) {
        if self.over > 0 {
            self.out.push(Finding {
                rule: self.rule,
                severity: Severity::Info,
                elements: vec![],
                at: None,
                msg_key: "ai.check.more",
                args: vec![("count", self.over.to_string())],
                fix: Fix::None,
                measure: None,
            });
        }
    }
}

pub(super) fn key(table: &str, id: i64) -> ElKey {
    (table.to_string(), id)
}

pub(super) fn ids(m: &Model, sql: &str) -> Result<Vec<i64>, Error> {
    let mut st = m.db.prepare(sql)?;
    let v = st
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<i64>, _>>()?;
    Ok(v)
}

/// SQL literal: company rule text reaches a query only this way.
pub(super) fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\0', "").replace('\'', "''"))
}

/// Rows (id, value): the violation measure computed by the same query as the selection.
pub(super) fn measured(m: &Model, sql: &str) -> Result<Vec<(i64, f64)>, Error> {
    let mut st = m.db.prepare(sql)?;
    let v = st
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

fn row3(m: &Model, sql: &str, id: i64) -> Option<[f64; 3]> {
    m.db.query_row(sql, [id], |r| {
        Ok([r.get::<_, f64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?])
    })
    .ok()
}

/// Element point in the world, m: axis midpoint or centre; types and tables without geometry take the point
/// of the first instance.
pub(super) fn point_of(m: &Model, table: &str, id: i64) -> Option<[f64; 3]> {
    match table {
        "wall" => row3(
            m,
            "SELECT (w.ax+w.bx)/2, (w.ay+w.by)/2, l.elevation+w.base_offset+w.height/2              FROM wall w JOIN level l ON l.id=w.level WHERE w.id=?1",
            id,
        ),
        "opening" => {
            let (a, along, width, z): ([f64; 4], f64, f64, f64) = m
                .db
                .query_row(
                    "SELECT w.ax, w.ay, w.bx, w.by, o.along, o.width,                      l.elevation+w.base_offset+o.sill+o.height/2                      FROM opening o JOIN wall w ON w.id=o.wall JOIN level l ON l.id=w.level WHERE o.id=?1",
                    [id],
                    |r| {
                        Ok((
                            [r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?],
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                        ))
                    },
                )
                .ok()?;
            let len = ((a[2] - a[0]).powi(2) + (a[3] - a[1]).powi(2)).sqrt();
            let t = if len > 1e-9 {
                ((along + width / 2.0) / len).clamp(0.0, 1.0)
            } else {
                0.0
            };
            Some([a[0] + (a[2] - a[0]) * t, a[1] + (a[3] - a[1]) * t, z])
        }
        "col" => row3(
            m,
            "SELECT t.x, t.y, l.elevation+t.base_offset FROM col t JOIN level l ON l.id=t.level WHERE t.id=?1",
            id,
        ),
        "beam" => row3(
            m,
            "SELECT (t.ax+t.bx)/2, (t.ay+t.by)/2, l.elevation+(t.az+t.bz)/2 FROM beam t JOIN level l ON l.id=t.level WHERE t.id=?1",
            id,
        ),
        "room" => row3(
            m,
            "SELECT t.px, t.py, l.elevation FROM room t JOIN level l ON l.id=t.level WHERE t.id=?1",
            id,
        ),
        "comp" => row3(
            m,
            "SELECT t.x, t.y, l.elevation+t.z FROM comp t JOIN level l ON l.id=t.level WHERE t.id=?1",
            id,
        ),
        "stair" => row3(
            m,
            "SELECT t.x, t.y, l.elevation+t.base_offset FROM stair t JOIN level l ON l.id=t.level WHERE t.id=?1",
            id,
        ),
        "slab" => row3(
            m,
            "SELECT v.x, v.y, l.elevation FROM slab t JOIN level l ON l.id=t.level              JOIN slab_vertex v ON v.slab=t.id AND v.ring=0 WHERE t.id=?1 ORDER BY v.idx LIMIT 1",
            id,
        ),
        "ceiling" => row3(
            m,
            "SELECT v.x, v.y, l.elevation+t.height FROM ceiling t JOIN level l ON l.id=t.level              JOIN ceiling_vertex v ON v.ceiling=t.id AND v.ring=0 WHERE t.id=?1 ORDER BY v.idx LIMIT 1",
            id,
        ),
        "shaft" => row3(
            m,
            "SELECT v.x, v.y, l.elevation+t.base_offset FROM shaft t JOIN level l ON l.id=t.level              JOIN shaft_vertex v ON v.shaft=t.id WHERE t.id=?1 ORDER BY v.idx LIMIT 1",
            id,
        ),
        "wall_type" | "slab_type" | "member_type" | "comp_type" => {
            let (inst, col) = match table {
                "wall_type" => ("wall", "wtype"),
                "slab_type" => ("slab", "stype"),
                "member_type" => ("col", "mtype"),
                _ => ("comp", "ctype"),
            };
            let first: Option<i64> = m
                .db
                .query_row(
                    &format!("SELECT MIN(id) FROM {inst} WHERE {col}=?1"),
                    [id],
                    |r| r.get(0),
                )
                .ok()
                .flatten();
            point_of(m, inst, first?)
        }
        _ => None,
    }
}
