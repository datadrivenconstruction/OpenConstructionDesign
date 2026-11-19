// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Per-view extents and tags of grids and levels (`datum_extent`, schema 18).
//! Grid: model (3D) endpoints and tags live in `grid`; a view row is a 2D override for that view.
//! Level: model extents along the section and tags are a row with view '' (no row means defaults);
//! a view row is a 2D override. Each write is one model operation.
//! A grid's 2D row stores endpoints as parameters along the model line (ax, bx are metres from its
//! start along the chord, ay = by = 0): moving or rotating the grid moves the view extents too.

use rusqlite::{params, Connection, OptionalExtension};

use std::collections::HashMap;

use crate::ops::Inv;
use crate::views::GridAxis;
use crate::{Error, Model};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DatumKind {
    Grid,
    Level,
}

impl DatumKind {
    fn code(self) -> &'static str {
        match self {
            DatumKind::Grid => "grid",
            DatumKind::Level => "level",
        }
    }
}

/// Endpoints and tags (start, end). For a level, `a.0`/`b.0` is x along the section, y is unused.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DatumExtent {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub bubbles: (bool, bool),
}

/// Key of the model (3D) extents.
pub const MODEL_VIEW: &str = "";
/// Default level extents when no row exists: x along the section, m.
pub const LEVEL_DEFAULT: (f64, f64) = (0.0, 20.0);

fn row_id(db: &Connection, view: &str, k: DatumKind, id: i64) -> Result<Option<i64>, Error> {
    Ok(db
        .query_row(
            "SELECT id FROM datum_extent WHERE view=?1 AND kind=?2 AND ref=?3",
            params![view, k.code(), id],
            |r| r.get(0),
        )
        .optional()?)
}

/// Model line of a grid: start, unit chord direction, whether it is an arc.
fn grid_frame(db: &Connection, id: i64) -> Result<((f64, f64), (f64, f64), bool), Error> {
    let g = crate::datum::load_grid(db, id)?
        .ok_or_else(|| Error::Invalid(format!("no such grid {id}")))?;
    let (a, b) = (g.spec.a, g.spec.b);
    let l = (b.0 - a.0).hypot(b.1 - a.1).max(1e-12);
    Ok((a, ((b.0 - a.0) / l, (b.1 - a.1) / l), g.spec.bulge != 0.0))
}

/// Removes an element's extents and tags in all views (for a level, also its plan rows); undo
/// restores them. Otherwise a new element with the same id would inherit stale extents.
pub(crate) fn drop_for_datum(
    db: &Connection,
    mut inv: Inv,
    k: DatumKind,
    id: i64,
) -> Result<Inv, Error> {
    let plan = format!("plan:{id}");
    let ids: Vec<i64> = {
        let mut st = db.prepare(
            "SELECT id FROM datum_extent WHERE (kind=?1 AND ref=?2) OR (?1='level' AND view=?3)              ORDER BY id",
        )?;
        let v = st
            .query_map(params![k.code(), id, plan], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        v
    };
    for r in ids {
        inv = inv.restore(db, "datum_extent", "id", r)?;
        db.execute("DELETE FROM datum_extent WHERE id=?1", [r])?;
    }
    Ok(inv)
}

/// Removes a view's 2D rows (when a section/elevation is deleted); undo restores them.
pub(crate) fn drop_for_view(db: &Connection, mut inv: Inv, view: &str) -> Result<Inv, Error> {
    let ids: Vec<i64> = {
        let mut st = db.prepare("SELECT id FROM datum_extent WHERE view=?1 ORDER BY id")?;
        let v = st
            .query_map([view], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        v
    };
    for r in ids {
        inv = inv.restore(db, "datum_extent", "id", r)?;
        db.execute("DELETE FROM datum_extent WHERE id=?1", [r])?;
    }
    Ok(inv)
}

fn exists(db: &Connection, k: DatumKind, id: i64) -> Result<bool, Error> {
    let t = match k {
        DatumKind::Grid => "SELECT count(*) FROM grid WHERE id=?1",
        DatumKind::Level => "SELECT count(*) FROM level WHERE id=?1",
    };
    Ok(db.query_row(t, [id], |r| r.get::<_, i64>(0))? > 0)
}

impl Model {
    /// View row (or a level's model row when `view` = [`MODEL_VIEW`]); None if absent.
    pub fn datum_extent(
        &self,
        view: &str,
        k: DatumKind,
        id: i64,
    ) -> Result<Option<DatumExtent>, Error> {
        let raw = self
            .db
            .query_row(
                "SELECT ax, ay, bx, by, bubbles FROM datum_extent \
                 WHERE view=?1 AND kind=?2 AND ref=?3",
                params![view, k.code(), id],
                |r| {
                    let b: i64 = r.get(4)?;
                    Ok(DatumExtent {
                        a: (r.get(0)?, r.get(1)?),
                        b: (r.get(2)?, r.get(3)?),
                        bubbles: (b & 1 != 0, b & 2 != 0),
                    })
                },
            )
            .optional()?;
        let Some(e) = raw else {
            return Ok(None);
        };
        if k == DatumKind::Level {
            return Ok(Some(e));
        }
        let (a, u, arc) = grid_frame(&self.db, id)?;
        if arc {
            // for an arc a view changes only the tags: endpoints are the model's
            let g = self.grid(id)?;
            return Ok(Some(DatumExtent {
                a: g.spec.a,
                b: g.spec.b,
                ..e
            }));
        }
        let at = |t: f64| (a.0 + u.0 * t, a.1 + u.1 * t);
        Ok(Some(DatumExtent {
            a: at(e.a.0),
            b: at(e.b.0),
            ..e
        }))
    }

    /// Extents in effect in a view: the view's 2D ones, else the model's (for a grid, from `grid`).
    pub fn datum_effective(&self, view: &str, k: DatumKind, id: i64) -> Result<DatumExtent, Error> {
        if !view.is_empty() {
            if let Some(e) = self.datum_extent(view, k, id)? {
                return Ok(e);
            }
        }
        match k {
            DatumKind::Grid => {
                let g = self.grid(id)?;
                Ok(DatumExtent {
                    a: g.spec.a,
                    b: g.spec.b,
                    bubbles: (g.spec.bubble_start, g.spec.bubble_end),
                })
            }
            DatumKind::Level => {
                if let Some(e) = self.datum_extent(MODEL_VIEW, k, id)? {
                    return Ok(e);
                }
                if !exists(&self.db, k, id)? {
                    return Err(Error::Invalid(format!("no such level {id}")));
                }
                Ok(DatumExtent {
                    a: (LEVEL_DEFAULT.0, 0.0),
                    b: (LEVEL_DEFAULT.1, 0.0),
                    bubbles: (true, true),
                })
            }
        }
    }

    /// Grids of a level plan with the 2D extents and tags of view "plan:<level>" (as on screen and sheet):
    /// a straight grid is one segment; an arc is its chords, start tag on the first, end tag on the last.
    pub fn plan_grid_axes(&self, level: i64) -> Result<Vec<(GridAxis, (bool, bool))>, Error> {
        let view = format!("plan:{level}");
        let ids: HashMap<String, i64> = self.grids()?.into_iter().map(|g| (g.name, g.id)).collect();
        let segs = self.grid_axes(Some(level))?;
        let mut out = vec![];
        let mut k = 0;
        while k < segs.len() {
            let n = segs[k..]
                .iter()
                .take_while(|s| s.label == segs[k].label)
                .count();
            let run = &segs[k..k + n];
            let e = match ids.get(&run[0].label) {
                Some(&id) => Some(self.datum_effective(&view, DatumKind::Grid, id)?),
                None => None,
            };
            let bub = e.map_or((true, true), |e| e.bubbles);
            match (n, e) {
                (1, Some(e)) => out.push((
                    GridAxis {
                        label: run[0].label.clone(),
                        a: e.a,
                        b: e.b,
                    },
                    bub,
                )),
                _ => {
                    for (i, g) in run.iter().enumerate() {
                        out.push((g.clone(), (i == 0 && bub.0, i + 1 == n && bub.1)));
                    }
                }
            }
            k += n;
        }
        Ok(out)
    }

    /// Writes (Some) or removes (None: the view reverts to the model's) a row; one undo.
    /// A grid's model extents are written by `update_grid`; here a grid's `view` is non-empty. Grid
    /// endpoints are projected onto its model line (2D extents are collinear with the model); an arc keeps the model's.
    pub fn set_datum_extent(
        &mut self,
        view: &str,
        k: DatumKind,
        id: i64,
        e: Option<DatumExtent>,
    ) -> Result<(), Error> {
        if k == DatumKind::Grid && view.is_empty() {
            return Err(Error::Invalid(
                "grid model extents are edited with update_grid".into(),
            ));
        }
        let e = match (e, k) {
            (Some(e), DatumKind::Grid) => {
                let (a, u, arc) = grid_frame(&self.db, id)?;
                let t = |p: (f64, f64)| (p.0 - a.0) * u.0 + (p.1 - a.1) * u.1;
                let (t0, t1) = if arc {
                    let g = self.grid(id)?;
                    (0.0, (g.spec.b.0 - a.0).hypot(g.spec.b.1 - a.1))
                } else {
                    (t(e.a), t(e.b))
                };
                Some(DatumExtent {
                    a: (t0, 0.0),
                    b: (t1, 0.0),
                    ..e
                })
            }
            (e, _) => e,
        };
        if let Some(e) = &e {
            // for a grid these are already parameters along the line: length is |t1 − t0|
            let ok = [e.a.0, e.a.1, e.b.0, e.b.1].iter().all(|v| v.is_finite());
            let len = (e.b.0 - e.a.0).abs();
            if !ok || len <= 1e-3 {
                return Err(Error::Degenerate(format!(
                    "{} extents: ends coincide",
                    k.code()
                )));
            }
        }
        self.op("datum_extent", |db| {
            if !exists(db, k, id)? {
                return Err(Error::Invalid(format!("no such {} {id}", k.code())));
            }
            let old = row_id(db, view, k, id)?;
            let mut inv = Inv::default();
            if let Some(r) = old {
                inv = inv.restore(db, "datum_extent", "id", r)?;
                db.execute("DELETE FROM datum_extent WHERE id=?1", [r])?;
            }
            let mut rid = old.unwrap_or(id);
            if let Some(e) = e {
                let bub = e.bubbles.0 as i64 | (e.bubbles.1 as i64) << 1;
                db.execute(
                    "INSERT INTO datum_extent(view, kind, ref, ax, ay, bx, by, bubbles) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![view, k.code(), id, e.a.0, e.a.1, e.b.0, e.b.1, bub],
                )?;
                rid = db.last_insert_rowid();
                // undo: first remove the new row, then restore the old one
                inv = Inv::default().delete("datum_extent", "id", rid).then(inv);
            }
            Ok(((), rid, inv))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GridSpec;

    #[test]
    fn view_override_bubbles_and_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let g = m
            .add_grid(&GridSpec::line("1", (0.0, 0.0), (0.0, 10.0)))
            .unwrap();
        let v = format!("plan:{lv}");
        let model = m.datum_effective(&v, DatumKind::Grid, g).unwrap();
        assert_eq!(model.b, (0.0, 10.0));
        let e = DatumExtent {
            a: (0.0, 2.0),
            b: (0.0, 8.0),
            bubbles: (false, true),
        };
        m.set_datum_extent(&v, DatumKind::Grid, g, Some(e)).unwrap();
        assert_eq!(m.datum_effective(&v, DatumKind::Grid, g).unwrap(), e);
        assert_eq!(
            m.datum_effective("plan:99", DatumKind::Grid, g).unwrap(),
            model
        );
        let e2 = DatumExtent { b: (0.0, 9.0), ..e };
        m.set_datum_extent(&v, DatumKind::Grid, g, Some(e2))
            .unwrap();
        m.undo().unwrap();
        assert_eq!(m.datum_extent(&v, DatumKind::Grid, g).unwrap(), Some(e));
        m.set_datum_extent(&v, DatumKind::Grid, g, None).unwrap();
        assert_eq!(m.datum_extent(&v, DatumKind::Grid, g).unwrap(), None);
        m.undo().unwrap();
        assert_eq!(m.datum_extent(&v, DatumKind::Grid, g).unwrap(), Some(e));
        m.undo().unwrap();
        assert_eq!(m.datum_extent(&v, DatumKind::Grid, g).unwrap(), None);
        // level: default model row; degenerate endpoints are rejected without a write
        let d = m.datum_effective(&v, DatumKind::Level, lv).unwrap();
        assert_eq!((d.a.0, d.b.0), LEVEL_DEFAULT);
        let bad = DatumExtent {
            a: (3.0, 0.0),
            b: (3.0, 0.0),
            bubbles: (true, true),
        };
        assert!(matches!(
            m.set_datum_extent(MODEL_VIEW, DatumKind::Level, lv, Some(bad)),
            Err(Error::Degenerate(_))
        ));
        assert!(m
            .set_datum_extent(MODEL_VIEW, DatumKind::Grid, g, Some(e))
            .is_err());
    }

    #[test]
    fn view_extents_follow_grid_moves_and_project_onto_it() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let g = m
            .add_grid(&GridSpec::line("1", (0.0, 0.0), (0.0, 10.0)))
            .unwrap();
        let v = format!("plan:{lv}");
        let e = DatumExtent {
            a: (1.0, 2.0),
            b: (0.0, 8.0),
            bubbles: (false, true),
        };
        m.set_datum_extent(&v, DatumKind::Grid, g, Some(e)).unwrap();
        let got = m.datum_effective(&v, DatumKind::Grid, g).unwrap();
        assert_eq!(
            (got.a, got.b),
            ((0.0, 2.0), (0.0, 8.0)),
            "projected onto the grid"
        );
        let near =
            |p: (f64, f64), q: (f64, f64)| (p.0 - q.0).abs() < 1e-9 && (p.1 - q.1).abs() < 1e-9;
        // moving a grid moves its 2D extents
        m.update_grid(g, &GridSpec::line("1", (3.0, 0.0), (3.0, 10.0)))
            .unwrap();
        let got = m.datum_effective(&v, DatumKind::Grid, g).unwrap();
        assert!(
            near(got.a, (3.0, 2.0)) && near(got.b, (3.0, 8.0)),
            "{got:?}"
        );
        // so does rotation about the origin
        m.update_grid(g, &GridSpec::line("1", (3.0, 0.0), (13.0, 0.0)))
            .unwrap();
        let got = m.datum_effective(&v, DatumKind::Grid, g).unwrap();
        assert!(
            near(got.a, (5.0, 0.0)) && near(got.b, (11.0, 0.0)),
            "{got:?}"
        );
        assert_eq!(got.bubbles, (false, true));
        // arc: the view holds only tags, endpoints are the model's
        let mut arc = GridSpec::line("1", (0.0, 0.0), (10.0, 0.0));
        arc.bulge = 0.5;
        m.update_grid(g, &arc).unwrap();
        let got = m.datum_effective(&v, DatumKind::Grid, g).unwrap();
        assert_eq!(
            (got.a, got.b, got.bubbles),
            ((0.0, 0.0), (10.0, 0.0), (false, true))
        );
        let ax = m.plan_grid_axes(lv).unwrap();
        assert!(ax.len() > 2, "arc drawn by its chords");
        assert!(
            !ax[0].1 .0 && !ax[0].1 .1,
            "no bubble at first chord's far end"
        );
        assert!(ax[1..].iter().all(|x| !x.1 .0), "start bubble once");
        assert!(ax.last().unwrap().1 .1);
        assert_eq!(ax.iter().filter(|x| x.1 .1).count(), 1, "end bubble once");
    }

    #[test]
    fn deleting_grid_or_level_drops_rules_and_extents_undo_restores_no_id_reuse() {
        use crate::constraints::Ref;
        use crate::{ConstraintKind, ConstraintSpec, LayerSpec, WallSpec};
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let l2 = m.add_level("L2", 4.0).unwrap();
        let t = m
            .add_wall_type(
                "W",
                &[LayerSpec {
                    material: "c".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (0.0, 5.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        let g = m
            .add_grid(&GridSpec::line("1", (0.0, -1.0), (0.0, 9.0)))
            .unwrap();
        let rule = |kind, walls| ConstraintSpec {
            kind,
            walls,
            value: None,
        };
        m.add_constraint(&rule(ConstraintKind::AlignGrid, vec![w, g]))
            .unwrap();
        m.add_constraint(&rule(ConstraintKind::TopToLevel, vec![w, l2]))
            .unwrap();
        let ext = DatumExtent {
            a: (0.0, 1.0),
            b: (0.0, 6.0),
            bubbles: (true, false),
        };
        let v = format!("plan:{lv}");
        m.set_datum_extent(&v, DatumKind::Grid, g, Some(ext))
            .unwrap();
        let lext = DatumExtent {
            a: (-2.0, 4.0),
            b: (9.0, 4.0),
            bubbles: (true, true),
        };
        m.set_datum_extent(MODEL_VIEW, DatumKind::Level, l2, Some(lext))
            .unwrap();
        m.set_datum_extent(&format!("plan:{l2}"), DatumKind::Grid, g, Some(ext))
            .unwrap();
        let rows = |m: &Model| -> i64 {
            m.sql()
                .query_row("SELECT count(*) FROM datum_extent", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(rows(&m), 3);
        // grid: its rule and extents go away; one undo restores everything
        m.delete_grid(g).unwrap();
        assert!(m.constraints_on(Ref::Grid(g)).unwrap().is_empty());
        assert_eq!(rows(&m), 1);
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(m.constraints_on(Ref::Grid(g)).unwrap().len(), 1);
        assert_eq!(m.datum_extent(&v, DatumKind::Grid, g).unwrap(), Some(ext));
        assert_eq!(rows(&m), 3);
        // a new grid with the same id does not inherit rules or extents
        m.delete_grid(g).unwrap();
        let g2 = m
            .add_grid(&GridSpec::line("2", (5.0, -1.0), (5.0, 9.0)))
            .unwrap();
        assert_eq!(g2, g, "sqlite reuses the max id");
        assert!(m.constraints_on(Ref::Grid(g2)).unwrap().is_empty());
        assert_eq!(m.datum_extent(&v, DatumKind::Grid, g2).unwrap(), None);
        m.update_grid(g2, &GridSpec::line("2", (6.0, -1.0), (6.0, 9.0)))
            .unwrap();
        let wx: f64 = m
            .sql()
            .query_row("SELECT ax FROM wall WHERE id=?1", [w], |r| r.get(0))
            .unwrap();
        assert_eq!(wx, 0.0, "wall not dragged by the new grid");
        // level: the top rule, model extents and its plan rows go away
        m.delete_level(l2).unwrap();
        assert!(m.constraints_on(Ref::Level(l2)).unwrap().is_empty());
        assert_eq!(rows(&m), 0);
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(m.constraints_on(Ref::Level(l2)).unwrap().len(), 1);
        assert_eq!(
            m.datum_extent(MODEL_VIEW, DatumKind::Level, l2).unwrap(),
            Some(lext)
        );
        m.delete_level(l2).unwrap();
        let l3 = m.add_level("L3", 7.0).unwrap();
        assert_eq!(l3, l2);
        m.update_level(l3, "L3", 7.5).unwrap();
        let h: f64 = m
            .sql()
            .query_row("SELECT height FROM wall WHERE id=?1", [w], |r| r.get(0))
            .unwrap();
        assert_eq!(h, 4.0, "wall top stays where the deleted level left it");
        assert_eq!(
            m.datum_extent(MODEL_VIEW, DatumKind::Level, l3).unwrap(),
            None
        );
    }
}
