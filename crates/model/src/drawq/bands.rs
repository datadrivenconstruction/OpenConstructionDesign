//! Vertical extent of every plan element of a level and the band it falls into for a view range.
//! One table drives the live plan, the sheets and the range window, so all element kinds follow
//! the range, not walls alone.

use rusqlite::Connection;

use super::{Band, ViewRange};
use crate::{Error, Model};

/// Vertical span of one element above its level elevation, m.
#[derive(Clone, Debug, PartialEq)]
pub struct Extent {
    pub table: &'static str,
    pub id: i64,
    /// Category key of the view graphics (`GFX_CATEGORIES`): doors and windows differ.
    pub category: &'static str,
    pub z0: f64,
    pub z1: f64,
}

/// An element and the band of the range it lies in.
#[derive(Clone, Debug, PartialEq)]
pub struct ElemBand {
    pub table: &'static str,
    pub id: i64,
    pub category: &'static str,
    pub band: Band,
}

fn rows<T>(
    db: &Connection,
    sql: &str,
    level: i64,
    f: impl Fn(&rusqlite::Row) -> rusqlite::Result<T>,
) -> Result<Vec<T>, Error> {
    let mut st = db.prepare(sql)?;
    let v = st
        .query_map([level], |r| f(r))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

fn push(
    out: &mut Vec<Extent>,
    table: &'static str,
    category: &'static str,
    v: Vec<(i64, f64, f64)>,
) {
    out.extend(v.into_iter().map(|(id, a, b)| Extent {
        table,
        id,
        category,
        z0: a.min(b),
        z1: a.max(b),
    }));
}

impl Model {
    /// Spans of the walls, curtain walls, openings, slabs, ceilings, roofs, columns, beams, stairs,
    /// railings, shafts, components and MEP runs of a level. Rooms, masses and imported meshes have
    /// no span in a plan and are not listed.
    pub fn plan_extents(&self, level: i64) -> Result<Vec<Extent>, Error> {
        let db = &self.db;
        let mut out = vec![];
        let trio = |r: &rusqlite::Row| Ok((r.get(0)?, r.get(1)?, r.get(2)?));
        push(
            &mut out,
            "wall",
            "wall",
            rows(
                db,
                "SELECT id, base_offset, base_offset + height FROM wall WHERE level=?1",
                level,
                trio,
            )?,
        );
        push(
            &mut out,
            "curtain_wall",
            "curtain_wall",
            rows(
                db,
                "SELECT id, base_offset, base_offset + height FROM curtain_wall WHERE level=?1",
                level,
                trio,
            )?,
        );
        let openings = rows(
            db,
            "SELECT o.id, w.base_offset + o.sill, w.base_offset + o.sill + o.height, o.kind \
             FROM opening o JOIN wall w ON w.id = o.wall WHERE w.level=?1",
            level,
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )?;
        for (id, a, b, kind) in openings {
            out.push(Extent {
                table: "opening",
                id,
                category: if kind == "window" { "window" } else { "door" },
                z0: a.min(b),
                z1: a.max(b),
            });
        }
        push(
            &mut out,
            "slab",
            "slab",
            rows(
                db,
                "SELECT s.id, s.offset - COALESCE((SELECT sum(thickness) FROM slab_type_layer \
                 WHERE stype = s.stype), 0), s.offset FROM slab s WHERE s.level=?1",
                level,
                trio,
            )?,
        );
        push(
            &mut out,
            "ceiling",
            "ceiling",
            rows(db, "SELECT id, height, height FROM ceiling WHERE level=?1", level, trio)?,
        );
        push(
            &mut out,
            "roof",
            "roof",
            rows(db, "SELECT id, offset, offset FROM roof WHERE level=?1", level, trio)?,
        );
        push(
            &mut out,
            "col",
            "col",
            rows(
                db,
                "SELECT c.id, c.base_offset, (COALESCE(t.elevation, l.elevation) - l.elevation) \
                 + c.top_offset FROM col c JOIN level l ON l.id = c.level \
                 LEFT JOIN level t ON t.id = c.top_level WHERE c.level=?1",
                level,
                trio,
            )?,
        );
        push(
            &mut out,
            "beam",
            "beam",
            rows(db, "SELECT id, az, bz FROM beam WHERE level=?1", level, trio)?,
        );
        push(
            &mut out,
            "stair",
            "stair",
            rows(
                db,
                "SELECT s.id, s.base_offset, (t.elevation - l.elevation) + s.top_offset \
                 FROM stair s JOIN level l ON l.id = s.level JOIN level t ON t.id = s.top_level \
                 WHERE s.level=?1",
                level,
                trio,
            )?,
        );
        push(
            &mut out,
            "railing",
            "railing",
            rows(
                db,
                "SELECT r.id, COALESCE(s.base_offset, 0), COALESCE(s.base_offset, 0) + r.height \
                 FROM railing r LEFT JOIN stair s ON s.id = r.stair WHERE r.level=?1",
                level,
                trio,
            )?,
        );
        push(
            &mut out,
            "shaft",
            "shaft",
            rows(
                db,
                "SELECT s.id, s.base_offset, (t.elevation - l.elevation) + s.top_offset \
                 FROM shaft s JOIN level l ON l.id = s.level JOIN level t ON t.id = s.top_level \
                 WHERE s.level=?1",
                level,
                trio,
            )?,
        );
        push(
            &mut out,
            "comp",
            "comp",
            rows(db, "SELECT id, z, z FROM comp WHERE level=?1", level, trio)?,
        );
        let elev: f64 = db
            .query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
                r.get(0)
            })
            .map_err(|_| Error::Invalid(format!("level {level} not found")))?;
        let runs = rows(
            db,
            "SELECT id, points FROM mep_run WHERE level=?1",
            level,
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )?;
        for (id, pts) in runs {
            let Some(p) = crate::parse_points(&pts) else {
                continue;
            };
            let zs = p.iter().map(|q| q.2 - elev);
            let lo = zs.clone().fold(f64::INFINITY, f64::min);
            let hi = zs.fold(f64::NEG_INFINITY, f64::max);
            if lo.is_finite() && hi.is_finite() {
                out.push(Extent {
                    table: "mep_run",
                    id,
                    category: "mep_run",
                    z0: lo,
                    z1: hi,
                });
            }
        }
        Ok(out)
    }

    /// Band of every listed element in range `r`.
    pub fn plan_bands(&self, level: i64, r: &ViewRange) -> Result<Vec<ElemBand>, Error> {
        Ok(self
            .plan_extents(level)?
            .into_iter()
            .map(|e| ElemBand {
                table: e.table,
                id: e.id,
                category: e.category,
                band: r.classify(e.z0, e.z1),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BeamJoin, BeamSpec, ColumnSpec, Just, LayerSpec, MemberShape, SlabSpec, WallSpec};

    fn band(v: &[ElemBand], table: &str) -> Band {
        v.iter().find(|b| b.table == table).unwrap().band
    }

    /// Two levels (0 and 3 m) with one element of each kind on the first.
    fn house() -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l1 = m.add_level("L1", 0.0).unwrap();
        let l2 = m.add_level("L2", 3.0).unwrap();
        let layer = [LayerSpec {
            material: "Concrete".into(),
            thickness: 0.2,
        }];
        let wt = m.add_wall_type("W", &layer).unwrap();
        let st = m.add_slab_type("S", &layer).unwrap();
        m.add_walls(&[WallSpec {
            a: (0.0, 0.0),
            b: (5.0, 0.0),
            level: l1,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        }])
        .unwrap();
        m.add_slab(&SlabSpec {
            level: l1,
            slab_type: st,
            offset: 0.0,
            outer: vec![(0.0, 1.0), (5.0, 1.0), (5.0, 4.0), (0.0, 4.0)],
            holes: vec![],
        })
        .unwrap();
        let mt = m
            .add_member_type("R", &MemberShape::Rect { b: 0.3, h: 0.3 }, "Concrete")
            .unwrap();
        m.add_column(&ColumnSpec {
            level: l1,
            top_level: Some(l2),
            member_type: mt,
            structural: true,
            at: (1.0, 2.0),
            base_offset: 0.0,
            top_offset: 0.0,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap();
        m.add_beam(&BeamSpec {
            level: l1,
            member_type: mt,
            a: (0.0, 3.0, 2.8),
            b: (5.0, 3.0, 2.8),
            bulge: 0.0,
            yjust: Just::Center,
            zjust: Just::Center,
            rotation: 0.0,
            start_ext: 0.0,
            end_ext: 0.0,
            join_start: BeamJoin::Auto,
            join_end: BeamJoin::Auto,
        })
        .unwrap();
        (m, l1)
    }

    #[test]
    fn every_listed_kind_gets_a_span() {
        let (m, l1) = house();
        let ex = m.plan_extents(l1).unwrap();
        for t in ["wall", "slab", "col", "beam"] {
            assert!(ex.iter().any(|e| e.table == t), "{t}");
        }
        let slab = ex.iter().find(|e| e.table == "slab").unwrap();
        assert!((slab.z0 + 0.2).abs() < 1e-9 && slab.z1.abs() < 1e-9, "top at offset, layers down");
        let col = ex.iter().find(|e| e.table == "col").unwrap();
        assert!((col.z1 - 3.0).abs() < 1e-9, "top follows the upper level");
        assert!(ex.iter().all(|e| e.z0 <= e.z1));
    }

    #[test]
    fn all_kinds_follow_the_range() {
        let (m, l1) = house();
        let r = ViewRange::default();
        let b = m.plan_bands(l1, &r).unwrap();
        assert_eq!(band(&b, "wall"), Band::Cut);
        assert_eq!(band(&b, "col"), Band::Cut);
        assert_eq!(band(&b, "slab"), Band::Seen);
        assert_eq!(band(&b, "beam"), Band::Overhead);
        // a low top plane drops the beam, a high bottom plane lets the slab fall away
        let low = ViewRange { top: 2.0, ..r };
        assert_eq!(band(&m.plan_bands(l1, &low).unwrap(), "beam"), Band::Hidden);
        let high = ViewRange {
            top: 3.0,
            cut: 1.2,
            bottom: 1.0,
            depth: 0.5,
        };
        assert_eq!(band(&m.plan_bands(l1, &high).unwrap(), "slab"), Band::Hidden);
    }

    #[test]
    fn unknown_level_is_an_error() {
        let (m, _) = house();
        assert!(m.plan_extents(9999).is_err());
    }
}
