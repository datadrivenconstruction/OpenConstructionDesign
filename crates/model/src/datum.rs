// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Grids (schema 14): straight and arc, bubbles at the ends, auto-numbered names, visibility by
//! level range; reference planes. Snapping: grid intersections, points on grids and planes.
//! If the model has grids, sections and plans show them instead of the supplied ones (auto-grids from walls).

use ddc_geom::datum::{intersections, next_name, Curve};
use ddc_geom::{v2, V2};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::views::GridAxis;
use crate::{Error, Model};

#[derive(Clone, Debug, PartialEq)]
pub struct GridSpec {
    /// None: the next name after the last grid (1 → 2, A → B); "1" in an empty model.
    pub name: Option<String>,
    pub a: (f64, f64),
    pub b: (f64, f64),
    /// 0: straight; otherwise an arc, bulge = tan(angle/4), > 0 is counter-clockwise.
    pub bulge: f64,
    /// Range of levels where the grid is visible (None: unbounded below/above).
    pub base_level: Option<i64>,
    pub top_level: Option<i64>,
    pub bubble_start: bool,
    pub bubble_end: bool,
}

impl GridSpec {
    pub fn line(name: &str, a: (f64, f64), b: (f64, f64)) -> Self {
        GridSpec {
            name: Some(name.into()),
            a,
            b,
            bulge: 0.0,
            base_level: None,
            top_level: None,
            bubble_start: true,
            bubble_end: true,
        }
    }
    pub fn curve(&self) -> Curve {
        Curve {
            a: v2(self.a.0, self.a.1),
            b: v2(self.b.0, self.b.1),
            bulge: self.bulge,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub id: i64,
    pub name: String,
    pub spec: GridSpec,
}

/// Snap point: an intersection of two grids or the nearest point on a grid/reference plane.
#[derive(Clone, Debug, PartialEq)]
pub enum Snap {
    Intersection { p: (f64, f64), a: String, b: String },
    OnGrid { p: (f64, f64), grid: String },
    OnRefPlane { p: (f64, f64), id: i64 },
}

impl Snap {
    pub fn point(&self) -> (f64, f64) {
        match self {
            Snap::Intersection { p, .. } | Snap::OnGrid { p, .. } | Snap::OnRefPlane { p, .. } => {
                *p
            }
        }
    }
}

fn invalid<T>(m: String) -> Result<T, Error> {
    Err(Error::Invalid(m))
}

fn check(db: &Connection, s: &GridSpec) -> Result<(), Error> {
    let v = [s.a.0, s.a.1, s.b.0, s.b.1, s.bulge];
    if v.iter().any(|x| !x.is_finite()) || (s.b.0 - s.a.0).hypot(s.b.1 - s.a.1) < 1e-6 {
        return invalid("grid: degenerate or non-finite line".into());
    }
    if s.bulge.abs() > 1e3 {
        return invalid("grid: bulge out of range".into());
    }
    let elev = |l: Option<i64>| -> Result<Option<f64>, Error> {
        match l {
            None => Ok(None),
            Some(id) => db
                .query_row("SELECT elevation FROM level WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .optional()?
                .map(Some)
                .ok_or_else(|| Error::Invalid(format!("grid: no level {id}"))),
        }
    };
    if let (Some(b), Some(t)) = (elev(s.base_level)?, elev(s.top_level)?) {
        if t < b {
            return invalid("grid: top level below base level".into());
        }
    }
    Ok(())
}

fn auto_name(db: &Connection) -> Result<String, Error> {
    let mut st = db.prepare("SELECT name FROM grid ORDER BY id DESC")?;
    let names: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let Some(last) = names.first() else {
        return Ok("1".into());
    };
    let mut n = next_name(last).unwrap_or_else(|| format!("{last}-1"));
    while names.contains(&n) {
        n = next_name(&n).unwrap_or_else(|| format!("{n}-1"));
    }
    Ok(n)
}

/// Crossings of every pair of grids with the ids of the pair (lower id first).
pub(crate) fn pair_points(db: &Connection) -> Result<Vec<(i64, i64, (f64, f64))>, Error> {
    let gs = load_grids(db)?;
    let mut out = vec![];
    for (i, g) in gs.iter().enumerate() {
        for h in &gs[i + 1..] {
            for p in intersections(&g.spec.curve(), &h.spec.curve()) {
                out.push((g.id, h.id, (p.x, p.y)));
            }
        }
    }
    Ok(out)
}

fn write(db: &Connection, id: Option<i64>, name: &str, s: &GridSpec) -> Result<i64, Error> {
    let bubbles = s.bubble_start as i64 | (s.bubble_end as i64) << 1;
    // an update by id must not replace the row that already holds the name
    let tail = if id.is_some() {
        " ON CONFLICT(id) DO UPDATE SET name=excluded.name,ax=excluded.ax,ay=excluded.ay,         bx=excluded.bx,by=excluded.by,bulge=excluded.bulge,base_level=excluded.base_level,         top_level=excluded.top_level,bubbles=excluded.bubbles"
    } else {
        ""
    };
    db.execute(
        &format!(
            "INSERT INTO grid(id,name,ax,ay,bx,by,bulge,base_level,top_level,bubbles)          VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10){tail}"
        ),
        params![
            id,
            name,
            s.a.0,
            s.a.1,
            s.b.0,
            s.b.1,
            s.bulge,
            s.base_level,
            s.top_level,
            bubbles
        ],
    )
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Error::Invalid(format!("grid name '{name}' is taken"))
        }
        e => e.into(),
    })?;
    Ok(db.last_insert_rowid())
}

const GRID_COLS: &str = "SELECT id,name,ax,ay,bx,by,bulge,base_level,top_level,bubbles FROM grid";

fn grid_row(r: &rusqlite::Row) -> rusqlite::Result<Grid> {
    let name: String = r.get(1)?;
    let bub: i64 = r.get(9)?;
    Ok(Grid {
        id: r.get(0)?,
        spec: GridSpec {
            name: Some(name.clone()),
            a: (r.get(2)?, r.get(3)?),
            b: (r.get(4)?, r.get(5)?),
            bulge: r.get(6)?,
            base_level: r.get(7)?,
            top_level: r.get(8)?,
            bubble_start: bub & 1 != 0,
            bubble_end: bub & 2 != 0,
        },
        name,
    })
}

pub(crate) fn load_grids(db: &Connection) -> Result<Vec<Grid>, Error> {
    let mut st = db.prepare(&format!("{GRID_COLS} ORDER BY id"))?;
    let r = st.query_map([], grid_row)?;
    Ok(r.collect::<Result<_, _>>()?)
}

pub(crate) fn load_grid(db: &Connection, id: i64) -> Result<Option<Grid>, Error> {
    let mut st = db.prepare_cached(&format!("{GRID_COLS} WHERE id=?1"))?;
    Ok(st.query_row([id], grid_row).optional()?)
}

/// Grids visible at elevation `z` (None: all), as segments for views (an arc is taken as the chord
/// of its endpoints and not subdivided: a section needs the crossing point, the plan draws `grid_curves`).
pub(crate) fn axes_at(db: &Connection, z: Option<f64>) -> Result<Vec<GridAxis>, Error> {
    let el = |l: Option<i64>| -> Result<Option<f64>, Error> {
        Ok(match l {
            None => None,
            Some(id) => db
                .query_row("SELECT elevation FROM level WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .optional()?,
        })
    };
    let mut out = vec![];
    for g in load_grids(db)? {
        if let Some(z) = z {
            if el(g.spec.base_level)?.is_some_and(|b| z < b - 1e-9)
                || el(g.spec.top_level)?.is_some_and(|t| z > t + 1e-9)
            {
                continue;
            }
        }
        if g.spec.bulge == 0.0 {
            out.push(GridAxis {
                label: g.name.clone(),
                a: g.spec.a,
                b: g.spec.b,
            });
        } else {
            let pts = g.spec.curve().points(0.25);
            for w in pts.windows(2) {
                out.push(GridAxis {
                    label: g.name.clone(),
                    a: (w[0].x, w[0].y),
                    b: (w[1].x, w[1].y),
                });
            }
        }
    }
    Ok(out)
}

/// A copy of a grid row under the next free name; the caller moves it.
pub(crate) fn copy_grid_row(db: &Connection, id: i64) -> Result<i64, Error> {
    let g = load_grid(db, id)?.ok_or_else(|| Error::Invalid(format!("no such grid {id}")))?;
    let name = auto_name(db)?;
    write(db, None, &name, &g.spec)
}

pub(crate) fn has_grids(db: &Connection) -> Result<bool, Error> {
    Ok(db.query_row("SELECT count(*) FROM grid", [], |r| r.get::<_, i64>(0))? > 0)
}

impl Model {
    pub fn add_grid(&mut self, s: &GridSpec) -> Result<i64, Error> {
        check(&self.db, s)?;
        self.op("grid_add", |db| {
            let name = match &s.name {
                Some(n) if !n.trim().is_empty() => n.clone(),
                _ => auto_name(db)?,
            };
            let id = write(db, None, &name, s)?;
            Ok((id, id, Inv::default().delete("grid", "id", id)))
        })
    }

    /// A row of parallel grids: `count` of them at pitch `step` (vector), auto-numbered names from `first`.
    pub fn add_grid_row(
        &mut self,
        first: &str,
        a: (f64, f64),
        b: (f64, f64),
        step: (f64, f64),
        count: usize,
    ) -> Result<Vec<i64>, Error> {
        let mut specs = vec![];
        let mut name = first.to_string();
        for k in 0..count {
            let o = (step.0 * k as f64, step.1 * k as f64);
            let mut g = GridSpec::line(&name, (a.0 + o.0, a.1 + o.1), (b.0 + o.0, b.1 + o.1));
            g.name = Some(name.clone());
            check(&self.db, &g)?;
            specs.push(g);
            name = next_name(&name).unwrap_or_else(|| format!("{name}-1"));
        }
        self.op("grid_add", |db| {
            let mut ids = vec![];
            let mut inv = Inv::default();
            for g in &specs {
                let id = write(db, None, g.name.as_deref().unwrap_or_default(), g)?;
                inv = inv.delete("grid", "id", id);
                ids.push(id);
            }
            Ok((ids.clone(), ids.first().copied().unwrap_or(0), inv))
        })
    }

    pub fn update_grid(&mut self, id: i64, s: &GridSpec) -> Result<(), Error> {
        check(&self.db, s)?;
        self.op("grid_edit", |db| {
            let old: Option<String> = db
                .query_row("SELECT name FROM grid WHERE id=?1", [id], |r| r.get(0))
                .optional()?;
            let Some(old) = old else {
                return invalid(format!("no such grid {id}"));
            };
            let inv = Inv::default().restore(db, "grid", "id", id)?;
            let name = s.name.clone().unwrap_or(old);
            write(db, Some(id), &name, s)?;
            let edited = [crate::constraints::Ref::Grid(id)];
            let inv = inv.then(crate::constraints::propagate_refs(db, &edited)?);
            Ok(((), id, inv))
        })
    }

    /// Deletes a grid together with its rules and per-view extents (undo restores everything).
    pub fn delete_grid(&mut self, id: i64) -> Result<(), Error> {
        self.delete_ref_with("grid", "grid_delete", id, &[], |db, inv| {
            let inv = crate::constraints::drop_for_ref(db, inv, crate::constraints::Ref::Grid(id))?;
            crate::datum_view::drop_for_datum(db, inv, crate::DatumKind::Grid, id)
        })
    }

    pub fn grids(&self) -> Result<Vec<Grid>, Error> {
        load_grids(&self.db)
    }

    pub fn grid(&self, id: i64) -> Result<Grid, Error> {
        load_grid(&self.db, id)?.ok_or_else(|| Error::Invalid(format!("no such grid {id}")))
    }

    /// Grids for level views (by level range), for plan and section.
    pub fn grid_axes(&self, level: Option<i64>) -> Result<Vec<GridAxis>, Error> {
        let z = match level {
            None => None,
            Some(l) => Some(
                self.db
                    .query_row("SELECT elevation FROM level WHERE id=?1", [l], |r| r.get(0))
                    .map_err(|_| Error::Invalid(format!("no level {l}")))?,
            ),
        };
        axes_at(&self.db, z)
    }

    /// All grid intersections: (point, first name, second name), ordered by pair ids.
    pub fn grid_intersections(&self) -> Result<Vec<((f64, f64), String, String)>, Error> {
        let gs = load_grids(&self.db)?;
        let mut out = vec![];
        for (i, g) in gs.iter().enumerate() {
            for h in &gs[i + 1..] {
                for p in intersections(&g.spec.curve(), &h.spec.curve()) {
                    out.push(((p.x, p.y), g.name.clone(), h.name.clone()));
                }
            }
        }
        Ok(out)
    }

    /// Snap to point `p` within radius `r`: grid intersections first, then grids and reference planes.
    pub fn snap_datum(&self, p: (f64, f64), r: f64) -> Result<Option<Snap>, Error> {
        let q = v2(p.0, p.1);
        let d = |x: (f64, f64)| (v2(x.0, x.1) - q).len();
        if let Some(best) = self
            .grid_intersections()?
            .into_iter()
            .filter(|x| d(x.0) <= r)
            .min_by(|x, y| d(x.0).total_cmp(&d(y.0)))
        {
            return Ok(Some(Snap::Intersection {
                p: best.0,
                a: best.1,
                b: best.2,
            }));
        }
        let mut best: Option<(f64, Snap)> = None;
        let mut take = |dist: f64, s: Snap| {
            if dist <= r && best.as_ref().map_or(true, |b| dist < b.0) {
                best = Some((dist, s));
            }
        };
        for g in load_grids(&self.db)? {
            let (n, dist) = g.spec.curve().nearest(q);
            take(
                dist,
                Snap::OnGrid {
                    p: (n.x, n.y),
                    grid: g.name,
                },
            );
        }
        for (id, a, b) in self.ref_planes()? {
            let c = Curve::line(v2(a.0, a.1), v2(b.0, b.1));
            // a reference plane is infinite: project onto the line
            let dd = c.b - c.a;
            let t = (q - c.a).dot(dd) / dd.dot(dd);
            let n: V2 = c.a + dd * t;
            take((n - q).len(), Snap::OnRefPlane { p: (n.x, n.y), id });
        }
        Ok(best.map(|b| b.1))
    }

    pub fn add_ref_plane(
        &mut self,
        name: &str,
        a: (f64, f64),
        b: (f64, f64),
    ) -> Result<i64, Error> {
        if ![a.0, a.1, b.0, b.1].iter().all(|v| v.is_finite())
            || (b.0 - a.0).hypot(b.1 - a.1) < 1e-6
        {
            return invalid("ref plane: degenerate line".into());
        }
        self.op("ref_plane_add", |db| {
            db.execute(
                "INSERT INTO ref_plane(name,ax,ay,bx,by) VALUES (?1,?2,?3,?4,?5)",
                params![name, a.0, a.1, b.0, b.1],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("ref_plane", "id", id)))
        })
    }

    /// Moves the ends of a reference plane (a grip drag): one undo step.
    pub fn set_ref_plane(&mut self, id: i64, a: (f64, f64), b: (f64, f64)) -> Result<(), Error> {
        if ![a.0, a.1, b.0, b.1].iter().all(|v| v.is_finite())
            || (b.0 - a.0).hypot(b.1 - a.1) < 1e-6
        {
            return invalid("ref plane: degenerate line".into());
        }
        self.op("ref_plane_edit", |db| {
            let inv = Inv::default().restore(db, "ref_plane", "id", id)?;
            let n = db.execute(
                "UPDATE ref_plane SET ax=?2,ay=?3,bx=?4,by=?5 WHERE id=?1",
                params![id, a.0, a.1, b.0, b.1],
            )?;
            if n == 0 {
                return invalid(format!("no such ref plane {id}"));
            }
            Ok(((), id, inv))
        })
    }

    pub fn delete_ref_plane(&mut self, id: i64) -> Result<(), Error> {
        self.delete_ref("ref_plane", "ref_plane_delete", id, &[])
    }

    /// Name and end points of a reference plane.
    pub fn ref_plane(&self, id: i64) -> Result<(String, (f64, f64), (f64, f64)), Error> {
        self.db
            .query_row(
                "SELECT name,ax,ay,bx,by FROM ref_plane WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?), (r.get(3)?, r.get(4)?))),
            )
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no such ref plane {id}")))
    }

    /// Renames or re-places a reference plane in one operation.
    pub fn update_ref_plane(
        &mut self,
        id: i64,
        name: &str,
        a: (f64, f64),
        b: (f64, f64),
    ) -> Result<(), Error> {
        if ![a.0, a.1, b.0, b.1].iter().all(|v| v.is_finite())
            || (b.0 - a.0).hypot(b.1 - a.1) < 1e-6
        {
            return invalid("ref plane: degenerate line".into());
        }
        self.op("ref_plane_edit", |db| {
            let n: i64 = db.query_row("SELECT count(*) FROM ref_plane WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
            if n == 0 {
                return invalid(format!("no such ref plane {id}"));
            }
            let inv = Inv::default().restore(db, "ref_plane", "id", id)?;
            db.execute(
                "UPDATE ref_plane SET name=?2,ax=?3,ay=?4,bx=?5,by=?6 WHERE id=?1",
                params![id, name, a.0, a.1, b.0, b.1],
            )?;
            Ok(((), id, inv))
        })
    }

    /// Reference planes: (id, a, b).
    pub fn ref_planes(&self) -> Result<Vec<(i64, (f64, f64), (f64, f64))>, Error> {
        let mut st = self
            .db
            .prepare("SELECT id,ax,ay,bx,by FROM ref_plane ORDER BY id")?;
        let r = st.query_map([], |r| {
            Ok((r.get(0)?, (r.get(1)?, r.get(2)?), (r.get(3)?, r.get(4)?)))
        })?;
        Ok(r.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_5x4_names_intersections_undo() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_grid_row("1", (0.0, -1.0), (0.0, 13.0), (6.0, 0.0), 5)
            .unwrap();
        m.add_grid_row("A", (-1.0, 0.0), (25.0, 0.0), (0.0, 4.0), 4)
            .unwrap();
        let g = m.grids().unwrap();
        let names: Vec<&str> = g.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["1", "2", "3", "4", "5", "A", "B", "C", "D"]);
        let x = m.grid_intersections().unwrap();
        assert_eq!(x.len(), 20);
        assert!(x.contains(&((18.0, 8.0), "4".into(), "C".into())));
        // auto-numbering continues from the last grid
        let id = m
            .add_grid(&GridSpec {
                name: None,
                ..GridSpec::line("", (-1.0, 16.0), (25.0, 16.0))
            })
            .unwrap();
        assert_eq!(m.grids().unwrap().last().unwrap().name, "E");
        assert_eq!(m.grids().unwrap().last().unwrap().id, id);
        assert!(m
            .add_grid(&GridSpec::line("A", (0.0, 0.0), (1.0, 1.0)))
            .is_err());
        match m.snap_datum((12.1, 3.9), 0.5).unwrap() {
            Some(Snap::Intersection { p, a, b }) => {
                assert_eq!((p, a.as_str(), b.as_str()), ((12.0, 4.0), "3", "B"))
            }
            s => panic!("{s:?}"),
        }
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(m.grids().unwrap().len(), 5);
        m.assert_integrity();
    }

    #[test]
    fn level_range_and_arc_snap() {
        let mut m = Model::open(":memory:").unwrap();
        let l1 = m.add_level("L1", 0.0).unwrap();
        let l2 = m.add_level("L2", 3.0).unwrap();
        let l3 = m.add_level("L3", 6.0).unwrap();
        let mut g = GridSpec::line("1", (0.0, 0.0), (0.0, 10.0));
        g.top_level = Some(l2);
        m.add_grid(&g).unwrap();
        m.add_grid(&GridSpec {
            bulge: 1.0,
            ..GridSpec::line("R", (5.0, 0.0), (-5.0, 0.0))
        })
        .unwrap();
        assert!(m
            .grid_axes(Some(l1))
            .unwrap()
            .iter()
            .any(|a| a.label == "1"));
        assert!(!m
            .grid_axes(Some(l3))
            .unwrap()
            .iter()
            .any(|a| a.label == "1"));
        let x = m.grid_intersections().unwrap();
        assert_eq!(x.len(), 1);
        assert!((x[0].0 .0).abs() < 1e-12 && (x[0].0 .1 - 5.0).abs() < 1e-12);
        let rp = m.add_ref_plane("ref", (0.0, 20.0), (1.0, 20.0)).unwrap();
        match m.snap_datum((40.0, 20.3), 0.5).unwrap() {
            Some(Snap::OnRefPlane { p, id }) => assert_eq!((p, id), ((40.0, 20.0), rp)),
            s => panic!("{s:?}"),
        }
        assert!(matches!(m.delete_level(l2), Err(Error::Referenced { .. })));
        m.assert_integrity();
    }

    #[test]
    fn grid_transforms_name_the_copy_and_undo_exactly() {
        use crate::Target;
        let mut m = Model::open(":memory:").unwrap();
        let a = m.add_grid(&GridSpec::line("A", (0.0, 0.0), (0.0, 10.0))).unwrap();
        let before = m.grids().unwrap();
        m.move_elements(&[Target::Grid(a)], (2.0, 1.0)).unwrap();
        let g = m.grid(a).unwrap().spec;
        assert_eq!((g.a, g.b), ((2.0, 1.0), (2.0, 11.0)));
        m.undo().unwrap();
        assert_eq!(m.grids().unwrap(), before);

        let copies = m.copy_elements(&[Target::Grid(a)], &[(5.0, 0.0)]).unwrap();
        let Target::Grid(c) = copies[0] else {
            panic!("{copies:?}")
        };
        assert_eq!(m.grid(c).unwrap().name, "B");
        assert_eq!(m.grid(c).unwrap().spec.a, (5.0, 0.0));
        m.undo().unwrap();
        assert_eq!(m.grids().unwrap(), before);

        let arr = m
            .array_linear(&[Target::Grid(a)], 3, crate::Spacing::Step((3.0, 0.0)))
            .unwrap();
        assert_eq!(arr.len(), 2);
        let names: Vec<_> = m.grids().unwrap().into_iter().map(|g| g.name).collect();
        assert_eq!(names, ["A", "B", "C"]);
        m.undo().unwrap();
        assert_eq!(m.grids().unwrap(), before);
        m.assert_integrity();
    }

    #[test]
    fn mirror_flips_an_arc_grid_and_a_ref_plane_follows() {
        use crate::Target;
        let mut m = Model::open(":memory:").unwrap();
        let g = m
            .add_grid(&GridSpec {
                bulge: 0.5,
                ..GridSpec::line("R", (1.0, 0.0), (1.0, 4.0))
            })
            .unwrap();
        let r = m.add_ref_plane("p", (1.0, 0.0), (1.0, 4.0)).unwrap();
        m.mirror_elements(&[Target::Grid(g), Target::RefPlane(r)], (0.0, 0.0), (0.0, 1.0), false)
            .unwrap();
        let s = m.grid(g).unwrap().spec;
        assert_eq!((s.a, s.b, s.bulge), ((-1.0, 0.0), (-1.0, 4.0), -0.5));
        assert_eq!(m.ref_plane(r).unwrap().1, (-1.0, 0.0));
        m.undo().unwrap();
        assert_eq!(m.grid(g).unwrap().spec.a, (1.0, 0.0));
        assert_eq!(m.ref_plane(r).unwrap().1, (1.0, 0.0));
    }

    #[test]
    fn ref_plane_edit_and_delete_undo_in_one_step() {
        let mut m = Model::open(":memory:").unwrap();
        let r = m.add_ref_plane("p", (0.0, 0.0), (0.0, 5.0)).unwrap();
        m.update_ref_plane(r, "q", (1.0, 0.0), (1.0, 5.0)).unwrap();
        assert_eq!(m.ref_plane(r).unwrap(), ("q".into(), (1.0, 0.0), (1.0, 5.0)));
        assert!(m.update_ref_plane(r, "q", (1.0, 0.0), (1.0, 0.0)).is_err());
        m.undo().unwrap();
        assert_eq!(m.ref_plane(r).unwrap().0, "p");
        m.delete_ref_plane(r).unwrap();
        assert!(m.ref_plane(r).is_err());
        m.undo().unwrap();
        assert_eq!(m.ref_plane(r).unwrap().0, "p");
        m.assert_integrity();
    }
}
