//! Openings and components with a host: the host exists, the opening is inside the wall, openings do not overlap.

use super::util::{ids, key, Sink, Within};
use super::{CheckRule, CheckScope, Finding, Fix, Measure, Severity};
use crate::{Edit, Error, Model, OpeningSpec};

pub(super) struct NoHost;

impl CheckRule for NoHost {
    fn id(&self) -> &'static str {
        "no_host"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        let mut s = Sink::new(out, self.id());
        for id in ids(m, "SELECT o.id FROM opening o LEFT JOIN wall w ON w.id=o.wall WHERE w.id IS NULL ORDER BY o.id")? {
            if w.has("opening", id) {
                let k = key("opening", id);
                s.add(
                    Severity::Error,
                    vec![k.clone()],
                    None,
                    "ai.check.opening_no_host",
                    vec![],
                    Fix::Delete(vec![k]),
                    Measure::flag(),
                );
            }
        }
        let sql = format!(
            "SELECT c.id FROM comp c JOIN comp_type t ON t.id=c.ctype JOIN comp_family f ON f.id=t.family \
             LEFT JOIN wall h ON h.id=c.host_wall WHERE f.host='wall' AND h.id IS NULL{} ORDER BY c.id",
            w.level_sql("c.level")
        );
        for id in ids(m, &sql)? {
            if w.has("comp", id) {
                let k = key("comp", id);
                s.add(
                    Severity::Error,
                    vec![k.clone()],
                    None,
                    "ai.check.comp_no_host",
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

pub(super) struct OutsideWall;

struct Row {
    id: i64,
    wall: i64,
    spec: OpeningSpec,
}

/// Shift into the wall if the opening fits in it; otherwise the only option is to remove it.
fn outside_fix(m: &Model, r: &Row) -> Fix {
    let Ok(len) = m.wall_length(r.wall) else {
        return Fix::Delete(vec![key("opening", r.id)]);
    };
    let mut spec = r.spec.clone();
    spec.along = spec.along.min(len - spec.width).max(0.0);
    spec.sill = spec.sill.max(0.0);
    if m.opening_fits(r.wall, &spec).is_ok() {
        Fix::Edits(vec![Edit::Opening {
            id: r.id,
            wall: r.wall,
            spec,
        }])
    } else {
        Fix::Delete(vec![key("opening", r.id)])
    }
}

/// How far the opening sticks out of its wall, m: along the axis, below the foot or above the top.
fn overshoot(m: &Model, r: &Row) -> f64 {
    let len = m.wall_length(r.wall).unwrap_or(f64::INFINITY);
    let top: f64 = m
        .db
        .query_row("SELECT height FROM wall WHERE id=?1", [r.wall], |x| x.get(0))
        .unwrap_or(f64::INFINITY);
    let s = &r.spec;
    [-s.along, s.along + s.width - len, -s.sill, s.sill + s.height - top]
        .into_iter()
        .fold(0.0, f64::max)
}

impl CheckRule for OutsideWall {
    fn id(&self) -> &'static str {
        "opening_outside"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        // set-based selection for straight walls; arcs and the exact answer come from the model itself
        let sql = format!(
            "SELECT o.id, o.wall, o.along, o.width, o.sill, o.height, o.kind FROM opening o JOIN wall w ON w.id=o.wall \
             WHERE (o.along<-1e-6 OR o.sill<-1e-6 OR o.sill+o.height>w.height+1e-6 OR w.bulge<>0 \
             OR (o.along+o.width)*(o.along+o.width) > (w.bx-w.ax)*(w.bx-w.ax)+(w.by-w.ay)*(w.by-w.ay)+1e-6){} ORDER BY o.id",
            w.level_sql("w.level")
        );
        let rows: Vec<Row> = {
            let mut st = m.db.prepare(&sql)?;
            let v = st
                .query_map([], |r| {
                    Ok(Row {
                        id: r.get(0)?,
                        wall: r.get(1)?,
                        spec: OpeningSpec {
                            along: r.get(2)?,
                            width: r.get(3)?,
                            sill: r.get(4)?,
                            height: r.get(5)?,
                            kind: r.get(6)?,
                        },
                    })
                })?
                .collect::<Result<Vec<Row>, _>>()?;
            v
        };
        let mut s = Sink::new(out, self.id());
        for r in rows {
            if !w.has("opening", r.id) || m.opening_fits(r.wall, &r.spec).is_ok() {
                continue;
            }
            let fix = outside_fix(m, &r);
            let over = overshoot(m, &r);
            let measure = if over > 0.0 && over.is_finite() {
                Measure::new(over, 0.0, "m")
            } else {
                Measure::flag()
            };
            s.add(
                Severity::Error,
                vec![key("opening", r.id)],
                None,
                "ai.check.opening_outside",
                vec![("kind", r.spec.kind.clone())],
                fix,
                measure,
            );
        }
        s.done();
        Ok(())
    }
}

pub(super) struct OpeningOverlap;

impl CheckRule for OpeningOverlap {
    fn id(&self) -> &'static str {
        "opening_overlap"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        let sql = format!(
            "SELECT o.id, o.wall, o.along, o.along+o.width, o.sill, o.sill+o.height FROM opening o JOIN wall w ON w.id=o.wall \
             WHERE 1{} ORDER BY o.wall, o.along, o.id",
            w.level_sql("w.level")
        );
        let mut st = m.db.prepare(&sql)?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, f64>(5)?,
            ))
        })?;
        let mut s = Sink::new(out, self.id());
        // (wall, the opening with the farthest end and that end, bottom, top)
        let mut last: Option<(i64, i64, f64, f64, f64)> = None;
        for row in rows {
            let (id, wall, a, b, z0, z1) = row?;
            match last {
                Some((lw, lid, end, lz0, lz1)) if lw == wall => {
                    if a < end - 1e-6
                        && z0 < lz1 - 1e-6
                        && lz0 < z1 - 1e-6
                        && (w.has("opening", id) || w.has("opening", lid))
                    {
                        let pair = vec![key("opening", lid), key("opening", id)];
                        s.add(
                            Severity::Error,
                            pair,
                            None,
                            "ai.check.opening_overlap",
                            vec![],
                            Fix::Delete(vec![key("opening", id)]),
                            Measure::new(end - a, 0.0, "m"),
                        );
                    }
                    if b > end {
                        last = Some((wall, id, b, z0, z1));
                    }
                }
                _ => last = Some((wall, id, b, z0, z1)),
            }
        }
        s.done();
        Ok(())
    }
}
