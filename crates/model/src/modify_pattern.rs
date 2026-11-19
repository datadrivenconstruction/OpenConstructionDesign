//! Patterns beyond the linear and radial arrays: along a curve, on a planar face, and equal
//! gaps between two reference walls. Placement data is computed by pure functions (the same
//! data drives the preview and the edit), the edits run as one `modify` operation.

use super::*;

const EDGE: f64 = 1e-9;

/// Upper bound on instances of one pattern: a typo in a count or a tiny spacing must be
/// refused with a reason, not run out of memory.
pub const MAX_PATTERN_COPIES: usize = 10_000;

fn too_many<T>() -> Result<T, Error> {
    me(ModifyError::TooMany(MAX_PATTERN_COPIES))
}

fn invalid<T>(s: &str) -> Result<T, Error> {
    Err(Error::Invalid(s.into()))
}

/// How many placements along a path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCount {
    /// Total instances including the source. Open path: first at the start, last at the end.
    /// Closed path: evenly around, no instance repeated at the seam.
    Count(usize),
    /// Distance along the path between instances; the remainder is reported, not stretched.
    Step(f64),
}

/// Division of one face direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GridDiv {
    /// Instances in this direction (1 puts a single row in the middle of the face).
    Count(usize),
    /// Distance between instances in this direction, starting at the face origin.
    Step(f64),
}

/// Orientation of instances on a face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceUp {
    /// Instances stay upright.
    Up,
    /// Instances follow the face normal; only accepted when the normal is vertical because
    /// plan-based elements cannot tilt.
    Normal,
}

/// Planar parallelogram face: `origin`, edge vectors `u` and `v` (full edge length).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PatternFace {
    pub origin: [f64; 3],
    pub u: [f64; 3],
    pub v: [f64; 3],
}

/// One instance position. Index 0 of a preview is the source position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PatternPlacement {
    pub pos: [f64; 3],
    /// Absolute tangent angle (radians) for path patterns, 0 on faces.
    pub yaw: f64,
    pub normal: [f64; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub struct PatternPreview {
    pub placements: Vec<PatternPlacement>,
    /// Path length, or 0 for faces.
    pub length: f64,
    /// Path distance from the last instance to the end (open) or to the seam (closed).
    pub leftover: f64,
}

enum Seg {
    Line { a: P, u: P, len: f64 },
    Arc { arc: ArcAxis, len: f64 },
}

impl Seg {
    fn len(&self) -> f64 {
        match self {
            Seg::Line { len, .. } | Seg::Arc { len, .. } => *len,
        }
    }
    fn at(&self, s: f64) -> (P, P) {
        match self {
            Seg::Line { a, u, .. } => (add(*a, scale(*u, s)), *u),
            Seg::Arc { arc, .. } => {
                let p = arc.point_at(s);
                let t = arc.tangent_at(s);
                ((p.x, p.y), (t.x, t.y))
            }
        }
    }
}

fn segments(path: &[(P, f64)], closed: bool) -> Result<Vec<Seg>, Error> {
    let tol = Tol::default();
    let n = path.len();
    let count = if closed { n } else { n.saturating_sub(1) };
    let mut out = vec![];
    for i in 0..count {
        let (a, bulge) = path[i];
        let b = path[(i + 1) % n].0;
        if ![a.0, a.1, b.0, b.1, bulge].iter().all(|v| v.is_finite()) {
            return invalid("path has a non-finite coordinate");
        }
        let d = sub(b, a);
        let l = len(d);
        if l <= tol.length {
            continue;
        }
        if bulge.abs() < EDGE {
            out.push(Seg::Line {
                a,
                u: scale(d, 1.0 / l),
                len: l,
            });
            continue;
        }
        match ArcAxis::new(v2(a.0, a.1), v2(b.0, b.1), bulge, tol) {
            Ok(arc) => {
                let len = arc.length();
                out.push(Seg::Arc { arc, len });
            }
            Err(ddc_geom::arc::ArcError::Straight) => out.push(Seg::Line {
                a,
                u: scale(d, 1.0 / l),
                len: l,
            }),
            Err(e) => return Err(Error::Invalid(format!("path arc is invalid: {e:?}"))),
        }
    }
    Ok(out)
}

fn locate(segs: &[Seg], s: f64) -> (P, P) {
    let mut acc = 0.0;
    for (i, g) in segs.iter().enumerate() {
        if s < acc + g.len() - 1e-12 || i + 1 == segs.len() {
            return g.at((s - acc).clamp(0.0, g.len()));
        }
        acc += g.len();
    }
    ((0.0, 0.0), (1.0, 0.0))
}

/// Placements along a path of `(vertex, bulge)` pairs; the bulge belongs to the segment that
/// starts at the vertex. Refused with a reason for a zero-length path or a bad spacing.
pub fn path_preview(
    path: &[(P, f64)],
    closed: bool,
    mode: PathCount,
) -> Result<PatternPreview, Error> {
    let segs = segments(path, closed)?;
    let total: f64 = segs.iter().map(Seg::len).sum();
    if segs.is_empty() || total <= Tol::default().length {
        return invalid("path has zero length");
    }
    let (n, step) = match mode {
        PathCount::Count(n) => {
            if n < 2 {
                return me(ModifyError::BadCount);
            }
            if n > MAX_PATTERN_COPIES {
                return too_many();
            }
            (n, total / if closed { n } else { n - 1 } as f64)
        }
        PathCount::Step(d) => {
            if !(d.is_finite() && d > Tol::default().length) {
                return invalid("spacing must be a positive length");
            }
            let nf = if closed {
                (total / d - 1e-9).ceil()
            } else {
                (total / d + 1e-9).floor() + 1.0
            };
            if !(nf <= MAX_PATTERN_COPIES as f64) {
                return too_many();
            }
            let n = nf as usize;
            if n < 2 {
                return invalid("spacing is not shorter than the path");
            }
            (n, d)
        }
    };
    let mut placements = Vec::with_capacity(n);
    for i in 0..n {
        // the last open instance sits exactly on the end, not on an accumulated sum
        let s = if !closed && matches!(mode, PathCount::Count(_)) && i + 1 == n {
            total
        } else {
            step * i as f64
        };
        let (p, t) = locate(&segs, s);
        placements.push(PatternPlacement {
            pos: [p.0, p.1, 0.0],
            yaw: t.1.atan2(t.0),
            normal: [0.0, 0.0, 1.0],
        });
    }
    Ok(PatternPreview {
        placements,
        length: total,
        leftover: (total - step * (n - 1) as f64).max(0.0),
    })
}

type V = [f64; 3];

fn vlen(a: V) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
fn vcross(a: V, b: V) -> V {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn axis_positions(len: f64, div: GridDiv) -> Result<Vec<f64>, Error> {
    match div {
        GridDiv::Count(c) => {
            if c < 1 {
                return invalid("grid count must be at least 1");
            }
            if c > MAX_PATTERN_COPIES {
                return too_many();
            }
            if c == 1 {
                return Ok(vec![0.5]);
            }
            Ok((0..c).map(|i| i as f64 / (c - 1) as f64).collect())
        }
        GridDiv::Step(d) => {
            if !(d.is_finite() && d > Tol::default().length) {
                return invalid("grid spacing must be a positive length");
            }
            let nf = (len / d + 1e-9).floor() + 1.0;
            if !(nf <= MAX_PATTERN_COPIES as f64) {
                return too_many();
            }
            let n = nf as usize;
            Ok((0..n).map(|i| d * i as f64 / len).collect())
        }
    }
}

/// Grid on a planar face, row-major (u fastest). Placement 0 is the face origin side.
pub fn surface_preview(
    face: &PatternFace,
    nu: GridDiv,
    nv: GridDiv,
) -> Result<PatternPreview, Error> {
    let finite = face
        .origin
        .iter()
        .chain(&face.u)
        .chain(&face.v)
        .all(|v| v.is_finite());
    if !finite {
        return invalid("face has a non-finite coordinate");
    }
    let (lu, lv) = (vlen(face.u), vlen(face.v));
    let c = vcross(face.u, face.v);
    let lc = vlen(c);
    if lu <= Tol::default().length || lv <= Tol::default().length || lc <= 1e-9 * lu * lv {
        return invalid("face is degenerate");
    }
    let mut n = [c[0] / lc, c[1] / lc, c[2] / lc];
    if n[2] < -EDGE {
        n = [-n[0], -n[1], -n[2]];
    }
    let (us, vs) = (axis_positions(lu, nu)?, axis_positions(lv, nv)?);
    if us.len() * vs.len() < 2 {
        return me(ModifyError::BadCount);
    }
    if us.len() * vs.len() > MAX_PATTERN_COPIES {
        return too_many();
    }
    let mut placements = Vec::with_capacity(us.len() * vs.len());
    for &b in &vs {
        for &a in &us {
            let pos = [0, 1, 2].map(|k| face.origin[k] + face.u[k] * a + face.v[k] * b);
            placements.push(PatternPlacement {
                pos,
                yaw: 0.0,
                normal: n,
            });
        }
    }
    Ok(PatternPreview {
        placements,
        length: 0.0,
        leftover: 0.0,
    })
}

/// Interval of a straight wall across direction `n` (unit), thickness included.
fn wall_span(db: &Connection, id: i64, n: P) -> Result<(f64, f64), Error> {
    let w = straight(db, id, "distribute")?;
    let l = len(sub(w.b, w.a));
    if l <= Tol::default().length {
        return me(ModifyError::Degenerate(Target::Wall(id)));
    }
    let lft = left(scale(sub(w.b, w.a), 1.0 / l));
    let (total, off0) = wall_thickness(db, id)?;
    let (c, k) = (dot(w.a, n), dot(lft, n));
    let (f0, f1) = (c + k * off0, c + k * (off0 - total));
    let ends = dot(w.b, n) - c;
    let (lo, hi) = (f0.min(f1), f0.max(f1));
    Ok((lo.min(lo + ends), hi.max(hi + ends)))
}

/// Shifts (along the returned unit direction, in metres) that give equal gaps between wall
/// `a`, the items and wall `b`.
fn distribute_plan(
    db: &Connection,
    a: i64,
    b: i64,
    items: &[Target],
) -> Result<(P, Vec<(Target, f64)>), Error> {
    if items.is_empty() {
        return me(ModifyError::Empty);
    }
    if a == b {
        return invalid("reference walls must differ");
    }
    let (wa, wb) = (
        straight(db, a, "distribute")?,
        straight(db, b, "distribute")?,
    );
    let (la, lb) = (len(sub(wa.b, wa.a)), len(sub(wb.b, wb.a)));
    if la <= Tol::default().length || lb <= Tol::default().length {
        return me(ModifyError::Degenerate(Target::Wall(a)));
    }
    let ua = scale(sub(wa.b, wa.a), 1.0 / la);
    let ub = scale(sub(wb.b, wb.a), 1.0 / lb);
    if cross(ua, ub).abs() > 1e-6 {
        return me(ModifyError::NotParallel);
    }
    let mut n = left(ua);
    let mid = |w: &WallRow| scale(add(w.a, w.b), 0.5);
    if dot(sub(mid(&wb), mid(&wa)), n) < 0.0 {
        n = scale(n, -1.0);
    }
    let mut spans = vec![];
    for &t in items {
        let Target::Wall(id) = t else {
            return me(ModifyError::Unsupported("distribute", t));
        };
        if id == a || id == b {
            return invalid("a reference wall cannot be distributed");
        }
        let w = straight(db, id, "distribute")?;
        let lw = len(sub(w.b, w.a));
        if lw <= Tol::default().length {
            return me(ModifyError::Degenerate(t));
        }
        if dot(scale(sub(w.b, w.a), 1.0 / lw), n).abs() > 1e-6 {
            return me(ModifyError::NotParallel);
        }
        let (lo, hi) = wall_span(db, id, n)?;
        spans.push((t, lo, hi));
    }
    // deterministic order: by position, then by id
    spans.sort_by(|x, y| {
        x.1.partial_cmp(&y.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| table_of(x.0).1.cmp(&table_of(y.0).1))
    });
    let start = wall_span(db, a, n)?.1;
    let end = wall_span(db, b, n)?.0;
    let used: f64 = spans.iter().map(|s| s.2 - s.1).sum();
    let free = end - start - used;
    if free < -EDGE {
        return invalid("elements do not fit between the reference walls");
    }
    let gap = free / (spans.len() + 1) as f64;
    let mut cur = start + gap;
    let mut out = vec![];
    for (t, lo, hi) in spans {
        out.push((t, cur - lo));
        cur += hi - lo + gap;
    }
    Ok((n, out))
}

fn lift_one(db: &Connection, s: &mut Snapshots, t: Target, dz: f64) -> Result<(), Error> {
    save(db, s, t)?;
    match t {
        Target::Solid(id) => crate::solids::lift(db, id, dz)?,
        Target::Mass(id) => s.push(crate::mass::shift_in(db, id, (0.0, 0.0, dz))?),
        Target::Topo(id) => crate::topo::lift_in(db, id, dz)?,
        _ => return me(ModifyError::Unsupported("vertical move", t)),
    }
    Ok(())
}

impl Model {
    /// Preview of an array along a curve; no model change.
    pub fn array_path_preview(
        &self,
        path: &[(P, f64)],
        closed: bool,
        mode: PathCount,
    ) -> Result<PatternPreview, Error> {
        path_preview(path, closed, mode)
    }

    /// Array along a curve: copies at placements 1.. (the source stays and counts as
    /// placement 0). With `align`, copies turn about `anchor` by the tangent change since the
    /// start. One undo step.
    pub fn array_path(
        &mut self,
        ts: &[Target],
        anchor: P,
        path: &[(P, f64)],
        closed: bool,
        mode: PathCount,
        align: bool,
    ) -> Result<Vec<Target>, Error> {
        self.modify("array_path", |db, s| {
            validate(db, ts)?;
            let pv = path_preview(path, closed, mode)?;
            let p0 = pv.placements[0];
            let mut out = vec![];
            for p in &pv.placements[1..] {
                let mut xf = if align {
                    Xf::rotate(anchor, p.yaw - p0.yaw)
                } else {
                    Xf::translate((0.0, 0.0))
                };
                xf.t = add(xf.t, (p.pos[0] - p0.pos[0], p.pos[1] - p0.pos[1]));
                out.extend(apply(db, s, ts, xf, true)?);
            }
            Ok(out)
        })
    }

    /// Preview of an array on a face; no model change.
    pub fn array_surface_preview(
        &self,
        face: &PatternFace,
        nu: GridDiv,
        nv: GridDiv,
    ) -> Result<PatternPreview, Error> {
        surface_preview(face, nu, nv)
    }

    /// Array on a planar face. Copies move in plan and, for solids, masses and terrains, in
    /// height; other kinds are refused when the face is not level. One undo step.
    pub fn array_surface(
        &mut self,
        ts: &[Target],
        face: &PatternFace,
        nu: GridDiv,
        nv: GridDiv,
        orient: SurfaceUp,
    ) -> Result<Vec<Target>, Error> {
        self.modify("array_surface", |db, s| {
            validate(db, ts)?;
            let pv = surface_preview(face, nu, nv)?;
            let n = pv.placements[0].normal;
            if orient == SurfaceUp::Normal && n[0].hypot(n[1]) > 1e-9 {
                return invalid(
                    "normal alignment would tilt the elements; use upright orientation",
                );
            }
            let p0 = pv.placements[0].pos;
            let mut out = vec![];
            for p in &pv.placements[1..] {
                let d = (p.pos[0] - p0[0], p.pos[1] - p0[1]);
                let dz = p.pos[2] - p0[2];
                let copies = apply(db, s, ts, Xf::translate(d), true)?;
                if dz.abs() > EDGE {
                    for &t in &copies {
                        lift_one(db, s, t, dz)?;
                    }
                }
                out.extend(copies);
            }
            Ok(out)
        })
    }

    /// Shifts that `distribute_between` would make; no model change.
    pub fn distribute_between_preview(
        &self,
        a: i64,
        b: i64,
        items: &[Target],
    ) -> Result<Vec<(Target, f64)>, Error> {
        Ok(distribute_plan(self.sql(), a, b, items)?.1)
    }

    /// Move walls between reference walls `a` and `b` so that all gaps (reference, item, ...,
    /// reference) are equal. Items move across the references only. One undo step.
    pub fn distribute_between(&mut self, a: i64, b: i64, items: &[Target]) -> Result<(), Error> {
        self.modify("distribute_between", |db, s| {
            validate(db, items)?;
            let (n, plan) = distribute_plan(db, a, b, items)?;
            for (t, shift) in plan {
                if shift.abs() > EDGE {
                    apply(db, s, &[t], Xf::translate(scale(n, shift)), false)?;
                }
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, WallSpec};
    use std::f64::consts::FRAC_PI_2;

    struct Fx {
        m: Model,
        lv: i64,
        wt: i64,
    }

    fn fx() -> Fx {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let wt = m
            .add_wall_type(
                "W",
                &[LayerSpec {
                    material: "C".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        Fx { m, lv, wt }
    }

    impl Fx {
        fn wall(&mut self, a: P, b: P) -> i64 {
            self.m
                .add_wall(&WallSpec {
                    a,
                    b,
                    level: self.lv,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: self.wt,
                    offset0: None,
                })
                .unwrap()
        }
        fn axis(&self, id: i64) -> (P, P) {
            self.m
                .sql()
                .query_row("SELECT ax,ay,bx,by FROM wall WHERE id=?1", [id], |r| {
                    Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?)))
                })
                .unwrap()
        }
        fn count(&self, table: &str) -> i64 {
            self.m
                .sql()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        }
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn id_of(t: Target) -> i64 {
        table_of(t).1
    }

    /// Semicircle from (0,0) to (4,0) passing through (2,-2).
    const ARC: [(P, f64); 2] = [((0.0, 0.0), 1.0), ((4.0, 0.0), 0.0)];

    #[test]
    fn huge_counts_and_tiny_steps_are_refused() {
        fn refused(r: Result<PatternPreview, Error>) -> bool {
            matches!(r, Err(Error::Modify(ModifyError::TooMany(MAX_PATTERN_COPIES))))
        }
        let line = [((0.0, 0.0), 0.0), ((1000.0, 0.0), 0.0)];
        let n = MAX_PATTERN_COPIES;
        assert!(path_preview(&line, false, PathCount::Count(n)).is_ok());
        assert!(refused(path_preview(&line, false, PathCount::Count(n + 1))));
        assert!(refused(path_preview(&line, false, PathCount::Count(usize::MAX))));
        assert!(refused(path_preview(&line, true, PathCount::Step(1e-3))));
        let face = PatternFace {
            origin: [0.0; 3],
            u: [10.0, 0.0, 0.0],
            v: [0.0, 10.0, 0.0],
        };
        let c = GridDiv::Count(200);
        assert!(refused(surface_preview(&face, c, c)));
        assert!(refused(surface_preview(&face, GridDiv::Step(1e-4), GridDiv::Count(1))));
        assert!(surface_preview(&face, GridDiv::Count(100), GridDiv::Count(100)).is_ok());
    }

    #[test]
    fn along_arc_count_and_tangent() {
        let pv = path_preview(&ARC, false, PathCount::Count(3)).unwrap();
        assert_eq!(pv.placements.len(), 3);
        let want = [
            ((0.0, 0.0), -FRAC_PI_2),
            ((2.0, -2.0), 0.0),
            ((4.0, 0.0), FRAC_PI_2),
        ];
        for (p, (xy, yaw)) in pv.placements.iter().zip(want) {
            assert!(near(p.pos[0], xy.0) && near(p.pos[1], xy.1), "{p:?}");
            assert!(near(p.yaw, yaw), "{p:?}");
        }
        let mut f = fx();
        let w = f.wall((0.0, 0.0), (1.0, 0.0));
        let out = f
            .m
            .array_path(
                &[Target::Wall(w)],
                (0.0, 0.0),
                &ARC,
                false,
                PathCount::Count(3),
                true,
            )
            .unwrap();
        assert_eq!(out.len(), 2);
        // a quarter turn, moved to the arc midpoint
        let (a, b) = f.axis(id_of(out[0]));
        assert!(near(a.0, 2.0) && near(a.1, -2.0), "{a:?}");
        assert!(near(b.0, 2.0) && near(b.1, -1.0), "{b:?}");
        let (a, b) = f.axis(id_of(out[1]));
        assert!(near(a.0, 4.0) && near(a.1, 0.0), "{a:?}");
        assert!(near(b.0, 3.0) && near(b.1, 0.0), "{b:?}");
    }

    #[test]
    fn path_array_is_one_undo_and_deterministic() {
        let mut f = fx();
        let w = f.wall((0.0, 0.0), (1.0, 0.0));
        let line = [((0.0, 0.0), 0.0), ((10.0, 0.0), 0.0)];
        let before = f.count("wall");
        f.m.array_path(
            &[Target::Wall(w)],
            (0.0, 0.0),
            &line,
            false,
            PathCount::Count(5),
            false,
        )
        .unwrap();
        assert_eq!(f.count("wall"), before + 4);
        f.m.undo().unwrap();
        assert_eq!(f.count("wall"), before);
        assert_eq!(
            path_preview(&ARC, false, PathCount::Step(1.3)).unwrap(),
            path_preview(&ARC, false, PathCount::Step(1.3)).unwrap()
        );
    }

    #[test]
    fn spacing_that_does_not_divide_the_length() {
        let line = [((0.0, 0.0), 0.0), ((10.0, 0.0), 0.0)];
        let pv = path_preview(&line, false, PathCount::Step(3.0)).unwrap();
        let xs: Vec<f64> = pv.placements.iter().map(|p| p.pos[0]).collect();
        assert_eq!(xs.len(), 4);
        assert!(near(xs[3], 9.0));
        assert!(near(pv.leftover, 1.0));
        let exact = path_preview(&line, false, PathCount::Step(2.5)).unwrap();
        assert_eq!(exact.placements.len(), 5);
        assert!(near(exact.placements[4].pos[0], 10.0));
    }

    #[test]
    fn closed_curve_has_no_duplicate_at_the_seam() {
        let sq = [
            ((0.0, 0.0), 0.0),
            ((4.0, 0.0), 0.0),
            ((4.0, 4.0), 0.0),
            ((0.0, 4.0), 0.0),
        ];
        let pv = path_preview(&sq, true, PathCount::Count(4)).unwrap();
        let ps: Vec<P> = pv.placements.iter().map(|p| (p.pos[0], p.pos[1])).collect();
        assert_eq!(ps.len(), 4);
        assert!(near(ps[1].0, 4.0) && near(ps[1].1, 0.0));
        assert!(near(ps[3].0, 0.0) && near(ps[3].1, 4.0));
        for i in 0..4 {
            for j in i + 1..4 {
                assert!(len(sub(ps[i], ps[j])) > 1.0);
            }
        }
        // spacing equal to a quarter of the perimeter gives the same four, not five
        let st = path_preview(&sq, true, PathCount::Step(4.0)).unwrap();
        assert_eq!(st.placements.len(), 4);
    }

    #[test]
    fn zero_length_path_is_refused_with_reason() {
        let mut f = fx();
        let w = f.wall((0.0, 0.0), (1.0, 0.0));
        let dot_path = [((2.0, 2.0), 0.0), ((2.0, 2.0), 0.0)];
        let before = f.count("wall");
        let e = f
            .m
            .array_path(
                &[Target::Wall(w)],
                (0.0, 0.0),
                &dot_path,
                false,
                PathCount::Count(3),
                true,
            )
            .unwrap_err();
        assert!(e.to_string().contains("zero length"), "{e}");
        assert_eq!(f.count("wall"), before);
        assert!(path_preview(&[((0.0, 0.0), 0.0)], true, PathCount::Count(3)).is_err());
    }

    fn mass_min(f: &Fx, t: Target) -> (f64, f64, f64) {
        let (lo, _) = f.m.mass_body(id_of(t)).unwrap().bbox().unwrap();
        (lo.x, lo.y, lo.z)
    }

    #[test]
    fn surface_grid_on_sloped_face() {
        use crate::mass::{Form, MassSpec};
        let face = PatternFace {
            origin: [0.0, 0.0, 0.0],
            u: [6.0, 0.0, 3.0],
            v: [0.0, 4.0, 0.0],
        };
        let pv = surface_preview(&face, GridDiv::Count(3), GridDiv::Count(2)).unwrap();
        assert_eq!(pv.placements.len(), 6);
        assert!(near(pv.placements[1].pos[2], 1.5));
        assert!(pv.placements[0].normal[2] > 0.0);
        let mut f = fx();
        let mass = f
            .m
            .add_mass(&MassSpec {
                level: f.lv,
                name: "M".into(),
                void: false,
                inplace: false,
                form: Form::Cuboid {
                    lo: (0.0, 0.0, 0.0),
                    hi: (1.0, 1.0, 1.0),
                },
            })
            .unwrap();
        let out = f
            .m
            .array_surface(
                &[Target::Mass(mass)],
                &face,
                GridDiv::Count(3),
                GridDiv::Count(2),
                SurfaceUp::Up,
            )
            .unwrap();
        assert_eq!(out.len(), 5);
        let (x, y, z) = mass_min(&f, out[0]);
        assert!(near(x, 3.0) && near(y, 0.0) && near(z, 1.5), "{x} {y} {z}");
        let (x, y, z) = mass_min(&f, out[4]);
        assert!(near(x, 6.0) && near(y, 4.0) && near(z, 3.0), "{x} {y} {z}");
        // a wall cannot be lifted onto the slope: refused, nothing changes
        let w = f.wall((0.0, 0.0), (1.0, 0.0));
        let before = f.count("wall");
        let e = f.m.array_surface(
            &[Target::Wall(w)],
            &face,
            GridDiv::Count(3),
            GridDiv::Count(2),
            SurfaceUp::Up,
        );
        assert!(e.is_err());
        assert_eq!(f.count("wall"), before);
        // tilting to the normal is refused on a slope
        assert!(f
            .m
            .array_surface(
                &[Target::Mass(mass)],
                &face,
                GridDiv::Count(3),
                GridDiv::Count(2),
                SurfaceUp::Normal
            )
            .is_err());
    }

    #[test]
    fn surface_step_division_and_degenerate_face() {
        let face = PatternFace {
            origin: [0.0, 0.0, 2.0],
            u: [5.0, 0.0, 0.0],
            v: [0.0, 2.0, 0.0],
        };
        let pv = surface_preview(&face, GridDiv::Step(2.0), GridDiv::Count(1)).unwrap();
        let xs: Vec<f64> = pv.placements.iter().map(|p| p.pos[0]).collect();
        assert_eq!(xs, vec![0.0, 2.0, 4.0]);
        assert!(near(pv.placements[0].pos[1], 1.0));
        assert!(near(pv.placements[0].pos[2], 2.0));
        let flat = PatternFace {
            u: [1.0, 0.0, 0.0],
            v: [2.0, 0.0, 0.0],
            ..face
        };
        assert!(surface_preview(&flat, GridDiv::Count(2), GridDiv::Count(2)).is_err());
    }

    #[test]
    fn distribute_equal_gaps_between_walls() {
        let mut f = fx();
        let a = f.wall((0.0, 0.0), (4.0, 0.0));
        let b = f.wall((0.0, 10.0), (4.0, 10.0));
        let p1 = f.wall((0.0, 1.0), (4.0, 1.0));
        let p2 = f.wall((0.0, 2.0), (4.0, 2.0));
        let p3 = f.wall((0.0, 9.0), (4.0, 9.0));
        let items = [Target::Wall(p3), Target::Wall(p1), Target::Wall(p2)];
        let plan = f.m.distribute_between_preview(a, b, &items).unwrap();
        assert_eq!(plan.len(), 3);
        f.m.distribute_between(a, b, &items).unwrap();
        // free space 9.8 - 3 * 0.2 = 9.2, four gaps of 2.3
        for (w, y) in [(p1, 2.5), (p2, 5.0), (p3, 7.5)] {
            let (s, e) = f.axis(w);
            assert!(near(s.1, y) && near(e.1, y), "{w}: {s:?}");
            assert!(near(s.0, 0.0) && near(e.0, 4.0));
        }
        f.m.undo().unwrap();
        assert!(near(f.axis(p1).0 .1, 1.0));
        assert!(f.m.distribute_between(a, a, &items).is_err());
    }
}
