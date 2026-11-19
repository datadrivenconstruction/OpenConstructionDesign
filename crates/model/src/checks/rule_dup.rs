//! Coincident elements: identical geometry on one level, overlapping walls.

use std::collections::BTreeMap;

use super::util::{key, Sink, Within};
use super::{CheckRule, CheckScope, Finding, Fix, Measure, Severity};
use crate::{Error, Model};

pub(super) struct Duplicates;

/// SQL itself groups identical rows: for 200k elements this is a single table pass.
fn groups(m: &Model, table: &str, sql: &str, w: &Within, s: &mut Sink<'_>) -> Result<(), Error> {
    let mut st = m.db.prepare(sql)?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    for row in rows {
        let mut list: Vec<i64> = row?.split(',').filter_map(|x| x.parse().ok()).collect();
        list.sort_unstable();
        if list.len() < 2 || !list.iter().any(|id| w.has(table, *id)) {
            continue;
        }
        let copies = list.len();
        let elements = list.iter().map(|id| key(table, *id)).collect();
        let extra = list[1..].iter().map(|id| key(table, *id)).collect();
        s.add(
            Severity::Warning,
            elements,
            None,
            "ai.check.duplicate",
            vec![("table", table.to_string())],
            Fix::Delete(extra),
            Measure::count(copies, 1),
        );
    }
    Ok(())
}

struct Span {
    t0: f64,
    t1: f64,
    id: i64,
    z0: f64,
    z1: f64,
}

/// Straight walls on one line of a level that overlap.
fn collinear_walls(m: &Model, w: &Within, s: &mut Sink<'_>) -> Result<(), Error> {
    let sql = format!(
        "SELECT id, level, ax, ay, bx, by, base_offset, height FROM wall WHERE bulge=0{} ORDER BY id",
        w.level_sql("level")
    );
    let mut lines: BTreeMap<(i64, i64, i64), Vec<Span>> = BTreeMap::new();
    let mut st = m.db.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            (r.get::<_, f64>(2)?, r.get::<_, f64>(3)?),
            (r.get::<_, f64>(4)?, r.get::<_, f64>(5)?),
            r.get::<_, f64>(6)?,
            r.get::<_, f64>(7)?,
        ))
    })?;
    for row in rows {
        let (id, level, a, b, base, height) = row?;
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-3 {
            continue;
        }
        // direction is taken up to sign so that opposing walls land on the same line
        let (ux, uy) = if dx < -1e-9 || (dx.abs() <= 1e-9 && dy < 0.0) {
            (-dx / len, -dy / len)
        } else {
            (dx / len, dy / len)
        };
        let angle = (uy.atan2(ux) * 1000.0).round() as i64;
        let offset = ((a.1 * ux - a.0 * uy) * 1000.0).round() as i64;
        let (p, q) = (a.0 * ux + a.1 * uy, b.0 * ux + b.1 * uy);
        lines.entry((level, angle, offset)).or_default().push(Span {
            t0: p.min(q),
            t1: p.max(q),
            id,
            z0: base,
            z1: base + height,
        });
    }
    for spans in lines.values_mut() {
        if spans.len() < 2 {
            continue;
        }
        spans.sort_by(|x, y| x.t0.total_cmp(&y.t0).then(x.id.cmp(&y.id)));
        let mut far = 0;
        for i in 1..spans.len() {
            let (f, c) = (&spans[far], &spans[i]);
            if c.t0 < f.t1 - 0.01
                && c.z0 < f.z1 - 1e-6
                && f.z0 < c.z1 - 1e-6
                && (w.has("wall", f.id) || w.has("wall", c.id))
            {
                // only a wall lying entirely within another can be considered redundant
                let fix = if c.t1 <= f.t1 + 1e-3 && c.z0 >= f.z0 - 1e-6 && c.z1 <= f.z1 + 1e-6 {
                    Fix::Delete(vec![key("wall", c.id)])
                } else {
                    Fix::None
                };
                s.add(
                    Severity::Warning,
                    vec![key("wall", f.id), key("wall", c.id)],
                    None,
                    "ai.check.wall_overlap",
                    vec![],
                    fix,
                    // overlap along the shared line against the 1 cm tolerance
                    Measure::new(f.t1.min(c.t1) - c.t0, 0.01, "m"),
                );
            }
            if spans[i].t1 > spans[far].t1 {
                far = i;
            }
        }
    }
    Ok(())
}

impl CheckRule for Duplicates {
    fn id(&self) -> &'static str {
        "duplicates"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        let mut s = Sink::new(out, self.id());
        let same = |table: &str, cols: &str, level: &str| {
            format!(
                "SELECT group_concat(id) FROM {table} WHERE 1{} GROUP BY {cols} HAVING COUNT(*)>1 ORDER BY MIN(id)",
                w.level_sql(level)
            )
        };
        groups(
            m,
            "col",
            &same("col", "level, ROUND(x,3), ROUND(y,3)", "level"),
            &w,
            &mut s,
        )?;
        groups(m, "beam", &same("beam", "level, ROUND(ax,3), ROUND(ay,3), ROUND(az,3), ROUND(bx,3), ROUND(by,3), ROUND(bz,3)", "level"), &w, &mut s)?;
        groups(
            m,
            "comp",
            &same(
                "comp",
                "level, ctype, ROUND(x,3), ROUND(y,3), ROUND(z,3), ROUND(rotation,4)",
                "level",
            ),
            &w,
            &mut s,
        )?;
        let rooms = format!(
            "SELECT group_concat(r.id) FROM room r JOIN room_qty q ON q.room=r.id WHERE 1{} GROUP BY r.level, q.rings HAVING COUNT(*)>1 ORDER BY MIN(r.id)",
            w.level_sql("r.level")
        );
        groups(m, "room", &rooms, &w, &mut s)?;
        let slabs = format!(
            "SELECT group_concat(id) FROM (SELECT t.id id, t.level level, \
             (SELECT group_concat(printf('%.3f,%.3f', v.x, v.y), ';') FROM (SELECT x, y FROM slab_vertex WHERE slab=t.id AND ring=0 ORDER BY idx) v) outline \
             FROM slab t WHERE 1{}) GROUP BY level, outline HAVING COUNT(*)>1 ORDER BY MIN(id)",
            w.level_sql("t.level")
        );
        groups(m, "slab", &slabs, &w, &mut s)?;
        collinear_walls(m, &w, &mut s)?;
        s.done();
        Ok(())
    }
}
