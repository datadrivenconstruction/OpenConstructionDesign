//! Creation directly in 3D on the work plane: wall (as a chain), slab (outline; Enter or a
//! click on the first point closes it), column (a point), beam (two ends; on a vertical
//! plane it is sloped). Walls, slabs and columns work only on a horizontal plane:
//! its height becomes the offset from the active level. Each element is one undo step
//! (together with a type created if needed).

use ddc_model::{BeamJoin, BeamSpec, ColumnSpec, Error, Just, Model, SlabSpec, WallSpec};
use ddc_render::glam::Vec3;

use super::plane::WorkPlane;
use crate::editor::Editor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Wall,
    Floor,
    Column,
    Beam,
}

/// New element: the slab/profile type is resolved at write time.
#[derive(Clone, Debug, PartialEq)]
pub enum New {
    Wall(WallSpec),
    Slab(SlabSpec),
    Column(ColumnSpec),
    Beam(BeamSpec),
}

/// Points collected by the creation tool.
#[derive(Clone, Debug, PartialEq)]
pub struct Create {
    pub kind: Kind,
    pub pts: Vec<Vec3>,
}

/// What to do after a click.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Point accepted, waiting for the next one.
    More,
    /// The element is ready.
    Done(New),
}

pub struct Ctx {
    pub level: i64,
    pub wall_type: i64,
    pub height: f64,
    /// Slab outline closing tolerance, m (screen-based: `m_per_px × 8`).
    pub close_tol: f32,
    /// Levels: (id, elevation from the active level), ascending.
    pub levels: Vec<(i64, f64)>,
}

impl Ctx {
    pub fn of(ed: &Editor) -> Ctx {
        let mut levels: Vec<(i64, f64)> = ed
            .levels()
            .into_iter()
            .map(|l| (l.0, l.2 - ed.elevation))
            .collect();
        levels.sort_by(|a, b| a.1.total_cmp(&b.1));
        Ctx {
            level: ed.level,
            wall_type: ed.current_type().map_or(0, |t| t.id),
            height: ed.height,
            close_tol: CLOSE_TOL,
            levels,
        }
    }

    /// The level below scene elevation `z` and the elevation above it: an element on the face of an
    /// upper slab sits on its own level, not on the active one with a large offset.
    pub fn on(&self, z: f64) -> (i64, f64) {
        match self.levels.iter().rev().find(|l| l.1 <= z + 1e-6) {
            Some(&(id, e)) if self.levels.iter().any(|l| l.0 == self.level) => (id, z - e),
            _ => (self.level, z),
        }
    }
}

fn bad(s: &str) -> Error {
    Error::Invalid(s.into())
}

fn flat(wp: &WorkPlane) -> Result<f64, Error> {
    if wp.is_horizontal() {
        Ok(wp.z() as f64)
    } else {
        Err(bad("e3d.err.horizontal"))
    }
}

fn xy(p: Vec3) -> (f64, f64) {
    (p.x as f64, p.y as f64)
}

/// Outline closing: a click closer than `tol` to the first point.
pub const CLOSE_TOL: f32 = 0.05;

impl Create {
    pub fn new(kind: Kind) -> Create {
        Create { kind, pts: vec![] }
    }

    /// Click at point `p` (already on the work plane).
    pub fn click(&mut self, c: &Ctx, wp: &WorkPlane, p: Vec3) -> Result<Step, Error> {
        match self.kind {
            Kind::Column => {
                let (level, z) = c.on(flat(wp)?);
                Ok(Step::Done(New::Column(ColumnSpec {
                    level,
                    top_level: None,
                    member_type: 0,
                    structural: true,
                    at: xy(p),
                    base_offset: z,
                    top_offset: z + c.height,
                    rotation: 0.0,
                    top_at: None,
                })))
            }
            Kind::Wall => {
                let (level, z) = c.on(flat(wp)?);
                let Some(&a) = self.pts.last() else {
                    self.pts.push(p);
                    return Ok(Step::More);
                };
                if (p - a).truncate().length() < 0.01 {
                    return Err(bad("e3d.err.wall_length"));
                }
                // chain: the end becomes the start of the next wall
                self.pts = vec![p];
                Ok(Step::Done(New::Wall(WallSpec {
                    a: xy(a),
                    b: xy(p),
                    level,
                    base_offset: z,
                    height: c.height,
                    wall_type: c.wall_type,
                    offset0: None,
                })))
            }
            Kind::Beam => {
                let Some(&a) = self.pts.last() else {
                    self.pts.push(p);
                    return Ok(Step::More);
                };
                if (p - a).length() < 0.01 {
                    return Err(bad("e3d.err.beam_length"));
                }
                self.pts.clear();
                // beam ends are measured from the elevation of the level below the lower end
                let (level, za) = c.on(a.z.min(p.z) as f64);
                let e = a.z.min(p.z) as f64 - za;
                Ok(Step::Done(New::Beam(BeamSpec {
                    level,
                    member_type: 0,
                    a: (a.x as f64, a.y as f64, a.z as f64 - e),
                    b: (p.x as f64, p.y as f64, p.z as f64 - e),
                    bulge: 0.0,
                    yjust: Just::Center,
                    zjust: Just::Center,
                    rotation: 0.0,
                    start_ext: 0.0,
                    end_ext: 0.0,
                    join_start: BeamJoin::default(),
                    join_end: BeamJoin::default(),
                })))
            }
            Kind::Floor => {
                flat(wp)?;
                if self.pts.len() >= 3 && (p - self.pts[0]).length() < c.close_tol.max(1e-4) {
                    return self.finish(c, wp);
                }
                if self.pts.last().is_some_and(|q| (*q - p).length() < 1e-4) {
                    return Ok(Step::More);
                }
                self.pts.push(p);
                Ok(Step::More)
            }
        }
    }

    /// Enter: close the slab outline.
    pub fn finish(&mut self, c: &Ctx, wp: &WorkPlane) -> Result<Step, Error> {
        if self.kind != Kind::Floor {
            self.pts.clear();
            return Ok(Step::More);
        }
        let (level, z) = c.on(flat(wp)?);
        if self.pts.len() < 3 {
            return Err(bad("e3d.err.floor_points"));
        }
        let outer: Vec<(f64, f64)> = self.pts.iter().map(|&p| xy(p)).collect();
        self.pts.clear();
        Ok(Step::Done(New::Slab(SlabSpec {
            level,
            slab_type: 0,
            offset: z,
            outer,
            holes: vec![],
        })))
    }

    /// Preview: a polyline from the collected points to the cursor.
    pub fn preview(&self, cur: Vec3) -> Vec<Vec3> {
        let mut v = self.pts.clone();
        if self.kind != Kind::Column {
            v.push(cur);
        }
        if self.kind == Kind::Floor && self.pts.len() >= 2 {
            v.push(self.pts[0]);
        }
        v
    }
}

/// Beam/column profile: the first in the project or a standard one (then +1 operation).
pub fn member_type(m: &mut Model) -> Result<(i64, usize), Error> {
    if let Some(t) = m.member_types()?.first() {
        return Ok((t.0, 0));
    }
    Ok((m.add_catalog_member_type("R 400x400")?, 1))
}

/// Slab type: the first in the project or a standard one.
pub fn slab_type(m: &mut Model) -> Result<(i64, usize), Error> {
    let first: Option<i64> = m
        .sql()
        .query_row("SELECT id FROM slab_type ORDER BY id LIMIT 1", [], |r| {
            r.get(0)
        })
        .ok();
    match first {
        Some(id) => Ok((id, 0)),
        None => crate::tools::ensure_slab_type(m, "200"),
    }
}

/// Write to the model; Ok(n) is the number of operations.
pub fn write(m: &mut Model, n: &New) -> Result<usize, Error> {
    // a failed add takes back the type made for it: rollback to the journal mark, not by count
    let mark = m.op_top()?;
    let undo = |m: &mut Model, _k: usize| {
        let _ = m.rollback_to(mark);
    };
    match n {
        New::Wall(s) => m.add_wall(s).map(|_| 1),
        New::Slab(s) => {
            let (st, k) = slab_type(m)?;
            let mut s = s.clone();
            s.slab_type = st;
            m.add_slab(&s).map(|_| k + 1).inspect_err(|_| undo(m, k))
        }
        New::Column(s) => {
            let (mt, k) = member_type(m)?;
            let mut s = s.clone();
            s.member_type = mt;
            m.add_column(&s).map(|_| k + 1).inspect_err(|_| undo(m, k))
        }
        New::Beam(s) => {
            let (mt, k) = member_type(m)?;
            let mut s = s.clone();
            s.member_type = mt;
            m.add_beam(&s).map(|_| k + 1).inspect_err(|_| undo(m, k))
        }
    }
}

/// Element into the model as one editor undo step.
pub fn commit(ed: &mut Editor, n: New) -> Result<(), Error> {
    ed.apply_ext(crate::tools::Rec::new("edit3d_create", move |m| {
        write(m, &n)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        Ctx {
            level: 1,
            wall_type: 7,
            height: 3.0,
            close_tol: CLOSE_TOL,
            levels: vec![],
        }
    }

    #[test]
    fn element_goes_to_the_level_under_it_and_floor_tol_scales() {
        let mut c = ctx();
        c.levels = vec![(1, 0.0), (2, 3.0)];
        assert_eq!(c.on(1.0), (1, 1.0));
        assert_eq!(c.on(3.2), (2, 3.2 - 3.0));
        assert_eq!(c.on(-0.5), (1, -0.5), "below every level: the active one");
        let wp = WorkPlane::level(1, 3.0);
        let mut w = Create::new(Kind::Wall);
        w.click(&c, &wp, Vec3::new(0.0, 0.0, 3.0)).unwrap();
        let Step::Done(New::Wall(s)) = w.click(&c, &wp, Vec3::new(4.0, 0.0, 3.0)).unwrap() else {
            panic!()
        };
        assert_eq!(
            (s.level, s.base_offset),
            (2, 0.0),
            "on the upper slab face: level 2"
        );
        // closing is screen-based: the tolerance grows when zoomed out
        let wp = WorkPlane::level(1, 0.0);
        let mut f = Create::new(Kind::Floor);
        for p in [Vec3::ZERO, Vec3::X * 30.0, Vec3::new(30.0, 20.0, 0.0)] {
            f.click(&c, &wp, p).unwrap();
        }
        c.close_tol = 0.5;
        assert!(matches!(
            f.click(&c, &wp, Vec3::new(0.3, 0.2, 0.0)).unwrap(),
            Step::Done(New::Slab(_))
        ));
    }

    #[test]
    fn wall_chain_on_raised_plane() {
        let wp = WorkPlane::level(1, 1.5);
        let mut c = Create::new(Kind::Wall);
        assert_eq!(
            c.click(&ctx(), &wp, Vec3::new(0.0, 0.0, 1.5)).unwrap(),
            Step::More
        );
        let Step::Done(New::Wall(w)) = c.click(&ctx(), &wp, Vec3::new(4.0, 0.0, 1.5)).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (w.a, w.b, w.base_offset, w.wall_type),
            ((0.0, 0.0), (4.0, 0.0), 1.5, 7)
        );
        assert_eq!(c.pts, vec![Vec3::new(4.0, 0.0, 1.5)], "chain continues");
        assert!(c.click(&ctx(), &wp, Vec3::new(4.0, 0.0, 1.5)).is_err());
        // a wall cannot be built on a vertical plane
        let v = WorkPlane::vertical(1, (0.0, 0.0), (1.0, 0.0)).unwrap();
        assert!(Create::new(Kind::Wall)
            .click(&ctx(), &v, Vec3::ZERO)
            .is_err());
    }

    #[test]
    fn floor_closes_on_first_point_or_enter() {
        let wp = WorkPlane::level(1, 0.0);
        let mut c = Create::new(Kind::Floor);
        for p in [Vec3::ZERO, Vec3::X * 3.0, Vec3::new(3.0, 2.0, 0.0)] {
            assert_eq!(c.click(&ctx(), &wp, p).unwrap(), Step::More);
        }
        assert_eq!(c.preview(Vec3::Y).len(), 5);
        let Step::Done(New::Slab(s)) = c.click(&ctx(), &wp, Vec3::new(0.01, 0.0, 0.0)).unwrap()
        else {
            panic!()
        };
        assert_eq!(s.outer.len(), 3);
        assert!(c.pts.is_empty());
        let mut c = Create::new(Kind::Floor);
        c.click(&ctx(), &wp, Vec3::ZERO).unwrap();
        c.click(&ctx(), &wp, Vec3::X).unwrap();
        assert!(c.finish(&ctx(), &wp).is_err(), "two points");
    }

    #[test]
    fn sloped_beam_on_vertical_plane_and_column() {
        let v = WorkPlane::vertical(1, (0.0, 0.0), (1.0, 0.0)).unwrap();
        let mut c = Create::new(Kind::Beam);
        c.click(&ctx(), &v, Vec3::new(0.0, 0.0, 2.0)).unwrap();
        let Step::Done(New::Beam(b)) = c.click(&ctx(), &v, Vec3::new(4.0, 0.0, 3.0)).unwrap()
        else {
            panic!()
        };
        assert_eq!((b.a.2, b.b.2), (2.0, 3.0));
        let wp = WorkPlane::level(1, 0.5);
        let Step::Done(New::Column(col)) = Create::new(Kind::Column)
            .click(&ctx(), &wp, Vec3::new(1.0, 2.0, 0.5))
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (col.at, col.base_offset, col.top_offset),
            ((1.0, 2.0), 0.5, 3.5)
        );
    }

    #[test]
    fn commit_is_one_undo_step_with_new_type() {
        let mut ed = Editor::new_in_memory().unwrap();
        let wp = WorkPlane::level(ed.level, 0.0);
        let cx = Ctx::of(&ed);
        let had = ed.model.member_types().unwrap().len();
        let Step::Done(n) = Create::new(Kind::Column)
            .click(&cx, &wp, Vec3::new(1.0, 1.0, 0.0))
            .unwrap()
        else {
            panic!()
        };
        commit(&mut ed, n).unwrap();
        let cols = |ed: &Editor| -> i64 {
            ed.model
                .sql()
                .query_row("SELECT COUNT(*) FROM col", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(cols(&ed), 1);
        ed.undo();
        assert_eq!(cols(&ed), 0);
        assert_eq!(
            ed.model.member_types().unwrap().len(),
            had,
            "type creation undone too"
        );
        ed.redo();
        assert_eq!(cols(&ed), 1);
    }
}
