//! Rooms and zero sizes: unplaced, unenclosed, zero-area.

use super::util::{key, measured, Sink, Within};
use super::{CheckRule, CheckScope, Finding, Fix, Measure, Severity};
use crate::{ElKey, Error, Model};

/// No `room_qty` row means unenclosed; a point outside the level's wall bounds means not placed at all.
fn rooms_without_area(m: &Model, w: &Within) -> Result<Vec<(i64, f64, f64, bool)>, Error> {
    let sql = format!(
        "WITH b AS (SELECT level, MIN(MIN(ax,bx)) x0, MAX(MAX(ax,bx)) x1, MIN(MIN(ay,by)) y0, MAX(MAX(ay,by)) y1 FROM wall GROUP BY level) \
         SELECT r.id, r.px, r.py, b.level IS NOT NULL AND r.px>=b.x0 AND r.px<=b.x1 AND r.py>=b.y0 AND r.py<=b.y1 \
         FROM room r LEFT JOIN room_qty q ON q.room=r.id LEFT JOIN b ON b.level=r.level \
         WHERE q.room IS NULL{} ORDER BY r.id",
        w.level_sql("r.level")
    );
    let mut st = m.db.prepare(&sql)?;
    let v = st
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0))
        })?
        .collect::<Result<Vec<(i64, f64, f64, bool)>, _>>()?;
    Ok(v)
}

pub(super) struct RoomUnplaced;

impl CheckRule for RoomUnplaced {
    fn id(&self) -> &'static str {
        "room_unplaced"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        let mut s = Sink::new(out, self.id());
        for (id, x, y, _) in rooms_without_area(m, &w)?.into_iter().filter(|r| !r.3) {
            if w.has("room", id) {
                let k = key("room", id);
                s.add(
                    Severity::Warning,
                    vec![k.clone()],
                    Some([x, y, 0.0]),
                    "ai.check.room_unplaced",
                    vec![],
                    Fix::Delete(vec![k]),
                    Measure::flag(),
                );
            }
        }
        s.done();
        Ok(())
    }
}

pub(super) struct RoomOpen;

impl CheckRule for RoomOpen {
    fn id(&self) -> &'static str {
        "room_open"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        let mut s = Sink::new(out, self.id());
        for (id, x, y, _) in rooms_without_area(m, &w)?.into_iter().filter(|r| r.3) {
            if w.has("room", id) {
                s.add(
                    Severity::Warning,
                    vec![key("room", id)],
                    Some([x, y, 0.0]),
                    "ai.check.room_open",
                    vec![],
                    Fix::None,
                    Measure::flag(),
                );
            }
        }
        s.done();
        Ok(())
    }
}

pub(super) struct ZeroSize;

impl CheckRule for ZeroSize {
    fn id(&self) -> &'static str {
        "zero_size"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        let mut s = Sink::new(out, self.id());
        let mut emit = |table: &str,
                        sql: String,
                        fix: fn(ElKey) -> Fix,
                        measure: fn(f64) -> Measure|
         -> Result<(), Error> {
            for (id, v) in measured(m, &sql)? {
                if w.has(table, id) {
                    let k = key(table, id);
                    s.add(
                        Severity::Warning,
                        vec![k.clone()],
                        None,
                        "ai.check.zero_size",
                        vec![("table", table.to_string())],
                        fix(k),
                        measure(v),
                    );
                }
            }
            Ok(())
        };
        let delete = |k: ElKey| Fix::Delete(vec![k]);
        emit(
            "wall",
            format!(
                "SELECT id, (bx-ax)*(bx-ax)+(by-ay)*(by-ay) FROM wall WHERE bulge=0 AND (bx-ax)*(bx-ax)+(by-ay)*(by-ay)<1e-6{} ORDER BY id",
                w.level_sql("level")
            ),
            delete,
            |sq| Measure::new(sq.sqrt(), 1e-3, "m"),
        )?;
        emit(
            "room",
            format!(
                "SELECT q.room, q.area FROM room_qty q JOIN room r ON r.id=q.room WHERE q.area<1e-6 \
                 AND NOT EXISTS (SELECT 1 FROM room_qty k JOIN room kr ON kr.id=k.room \
                 WHERE kr.level=r.level AND k.rings=q.rings AND k.room<q.room){} ORDER BY q.room",
                w.level_sql("r.level")
            ),
            delete,
            |a| Measure::new(a, 1e-6, "m2"),
        )?;
        emit(
            "slab",
            format!(
                "SELECT q.slab, MAX(q.area) FROM slab_qty q JOIN slab t ON t.id=q.slab WHERE 1{} GROUP BY q.slab HAVING MAX(q.area)<1e-6 ORDER BY q.slab",
                w.level_sql("t.level")
            ),
            delete,
            |a| Measure::new(a, 1e-6, "m2"),
        )?;
        s.done();
        Ok(())
    }
}
