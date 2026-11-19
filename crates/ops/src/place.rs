//! Resolution of `Place` intents to a point and frame (AI_NATIVE.md section 4.6).
//!
//! Pure: the model is seen only through `PlaceModel`, so this runs in tests, in the app
//! (wrapping the snap service) and in a headless executor alike. Every failure is a
//! `Rejection` with a stable code, a readable reason and the refs involved; there is no
//! silent fallback to raw coordinates.
//!
//! Plan convention: lines live in the x/y plane; the left side of a line is the side the
//! direction a->b turns to counter-clockwise, normal `(-dy, dx)`.

use crate::geo::{End, Place, Side, SnapKind};
use crate::refs::Ref;
use crate::units::{Dir3, P3};
use crate::Frame;

/// Geometric tolerance, metres.
pub const TOL: f64 = 1e-6;

type V = [f64; 3];

/// What the model reports for a ref, in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Point(V),
    /// A plan line: wall axis (`half` = half thickness), grid or reference plane (`half` 0).
    /// `bounded` lines (walls, faces) keep their extent; unbounded ones (grids) extend.
    Line { a: V, b: V, half: f64, bounded: bool },
    /// Anything with a centre only (room, slab, solid bounding box).
    Box { min: V, max: V },
}

/// Read-only view of the model used by the resolver.
pub trait PlaceModel {
    /// Shape of a non-`Part` ref (`Id`, `Name`, `New`, `Set`); `None` when it does not exist.
    fn shape(&self, r: &Ref) -> Option<Shape>;

    /// Seam for named parts (`wall:5/face:left`, face refs of solids). Return `None` to let
    /// the resolver derive the built-in parts of lines: `axis`, `face:left`, `face:right`,
    /// `end:a`, `end:b`, `mid`, `center`. A non-vertical face has no plan shape yet.
    fn part(&self, _of: &Ref, _part: &str) -> Option<Shape> {
        None
    }

    /// Best snap candidate within `max` of `near` among `kinds` (the app wraps its snap
    /// service here). Default: no snap source.
    fn snap(&self, _near: V, _kinds: &[SnapKind], _max: f64) -> Option<(V, SnapKind)> {
        None
    }
}

pub use crate::reason::Rejection;
use crate::reason::Code;

/// Place failures as the one `Rejection`: the local cause maps to a stable `Code`, the readable
/// reason goes to `got`. Local causes: `unknown_ref`, `unknown_part`, `no_snap` (not found),
/// `not_a_line`, `parallel`, `conflict`, `outside_host`, `outside_extent` (off host),
/// `underdetermined`, `bad_param` (out of range), `empty` (empty selection).
fn reject(cause: &'static str, reason: String, refs: Vec<Ref>) -> Rejection {
    let code = match cause {
        "unknown_ref" | "unknown_part" | "no_snap" => Code::NotFound,
        "not_a_line" => Code::BadKind,
        "parallel" => Code::Parallel,
        "conflict" => Code::Conflict,
        "outside_host" => Code::OutsideHost,
        "outside_extent" => Code::OffHost,
        "empty" => Code::EmptySelection,
        "bad_param" => Code::OutOfRange,
        _ => Code::Invalid,
    };
    let mut r = Rejection::new(code).got(reason);
    r.refs = refs;
    r
}

/// Resolved placement, metres. `x` is the frame's horizontal direction (host or line
/// direction when there is one, else +X); `refs` are the refs the result depends on.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub at: V,
    pub x: V,
    pub refs: Vec<Ref>,
}

impl Placed {
    pub fn frame(&self) -> Frame {
        Frame {
            origin: P3::m(self.at[0], self.at[1], self.at[2]),
            x: Dir3 { x: self.x[0], y: self.x[1], z: self.x[2] },
            z: Dir3::Z,
        }
    }
}

/// The set of points a placement allows.
#[derive(Clone, Debug, PartialEq)]
enum Locus {
    Point(V),
    /// `p + d * s`, `d` unit in plan; `range` limits `s` for bounded lines.
    Line { p: V, d: V, range: Option<(f64, f64)> },
}

#[derive(Clone, Debug)]
struct Con {
    locus: Locus,
    x: Option<V>,
    refs: Vec<Ref>,
}

// ---- vector helpers ----

fn add(a: V, b: V) -> V {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn mul(a: V, k: f64) -> V {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: V, b: V) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn len(a: V) -> f64 {
    dot(a, a).sqrt()
}
fn dist(a: V, b: V) -> f64 {
    len(sub(a, b))
}
fn cross2(a: V, b: V) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}
fn left(d: V) -> V {
    [-d[1], d[0], 0.0]
}

/// Plan direction of a line (z ignored), unit; `None` for a degenerate line.
fn plan_dir(a: V, b: V) -> Option<(V, f64)> {
    let d = [b[0] - a[0], b[1] - a[1], 0.0];
    let l = len(d);
    (l > TOL).then(|| (mul(d, 1.0 / l), l))
}

fn center_of(s: Shape) -> V {
    match s {
        Shape::Point(p) => p,
        Shape::Line { a, b, .. } => mul(add(a, b), 0.5),
        Shape::Box { min, max } => mul(add(min, max), 0.5),
    }
}

// ---- ref lookup ----

fn shape_of(m: &dyn PlaceModel, r: &Ref) -> Result<Shape, Rejection> {
    match r {
        Ref::Part { of, part } => {
            if let Some(s) = m.part(of, part) {
                return Ok(s);
            }
            // One part grammar for refs, checks and placement.
            let name = crate::parts::PartName::parse(part).map_err(|why| {
                reject("unknown_part", format!("{} has no part `{part}`: {why}", of.label()), vec![r.clone()])
            })?;
            let canon = name.to_string();
            let base = shape_of(m, of)?;
            builtin_part(base, &canon).ok_or_else(|| {
                reject(
                    "unknown_part",
                    format!("{} has no part `{part}`", of.label()),
                    vec![r.clone()],
                )
            })
        }
        _ => m.shape(r).ok_or_else(|| {
            reject("unknown_ref", format!("{} does not exist", r.label()), vec![r.clone()])
        }),
    }
}

fn builtin_part(s: Shape, part: &str) -> Option<Shape> {
    match (s, part) {
        (_, "center") => Some(Shape::Point(center_of(s))),
        (Shape::Line { a, b, half, bounded }, _) => {
            let (d, _) = plan_dir(a, b)?;
            let face = |k: f64| {
                let o = mul(left(d), k);
                Shape::Line { a: add(a, o), b: add(b, o), half: 0.0, bounded }
            };
            match part {
                "axis" => Some(Shape::Line { a, b, half: 0.0, bounded }),
                "face:left" => Some(face(half)),
                "face:right" => Some(face(-half)),
                "end:a" => Some(Shape::Point(a)),
                "end:b" => Some(Shape::Point(b)),
                "mid" => Some(Shape::Point(center_of(s))),
                _ => None,
            }
        }
        _ => None,
    }
}

struct LineRef {
    a: V,
    d: V,
    len: f64,
    half: f64,
    bounded: bool,
}

fn line_of(m: &dyn PlaceModel, r: &Ref) -> Result<LineRef, Rejection> {
    match shape_of(m, r)? {
        Shape::Line { a, b, half, bounded } => {
            let (d, l) = plan_dir(a, b).ok_or_else(|| {
                reject("not_a_line", format!("{} has zero length", r.label()), vec![r.clone()])
            })?;
            Ok(LineRef { a, d, len: l, half, bounded })
        }
        _ => Err(reject("not_a_line", format!("{} is not a line", r.label()), vec![r.clone()])),
    }
}

fn line_locus(a: V, d: V, l: f64, bounded: bool) -> Locus {
    Locus::Line { p: a, d, range: bounded.then_some((0.0, l)) }
}

fn locus_of_ref(m: &dyn PlaceModel, r: &Ref) -> Result<Con, Rejection> {
    let refs = vec![r.clone()];
    Ok(match shape_of(m, r)? {
        Shape::Line { a, b, bounded, .. } => {
            let (d, l) = plan_dir(a, b).ok_or_else(|| {
                reject("not_a_line", format!("{} has zero length", r.label()), refs.clone())
            })?;
            Con { locus: line_locus(a, d, l, bounded), x: Some(d), refs }
        }
        s => Con { locus: Locus::Point(center_of(s)), x: None, refs },
    })
}

// ---- single intent ----

fn constraint(m: &dyn PlaceModel, place: &Place) -> Result<Con, Rejection> {
    match place {
        Place::At { p } => Ok(Con { locus: Locus::Point(p.to_m()), x: None, refs: vec![] }),
        Place::Offset { from, d, side } => {
            let LineRef { a, d: dir, len: l, half, bounded } = line_of(m, from)?;
            let d = d.to_m();
            let k = match side {
                Side::Left => half + d,
                Side::Right => -(half + d),
                Side::Center => d,
            };
            let p = add(a, mul(left(dir), k));
            Ok(Con { locus: line_locus(p, dir, l, bounded), x: Some(dir), refs: vec![from.clone()] })
        }
        Place::OnHost { host, along, from_end, side, z } => {
            let LineRef { a, d: dir, len: l, half, .. } = line_of(m, host)?;
            let s = along.to_m();
            if s < -TOL || s > l + TOL {
                return Err(reject(
                    "outside_host",
                    format!("{:.3} m along {} is outside its length {:.3} m", s, host.label(), l),
                    vec![host.clone()],
                ));
            }
            let t = match from_end {
                End::A => s,
                End::B => l - s,
            };
            let k = match side {
                Side::Left => half,
                Side::Right => -half,
                Side::Center => 0.0,
            };
            let mut p = add(add(a, mul(dir, t)), mul(left(dir), k));
            p[2] += z.to_m();
            Ok(Con { locus: Locus::Point(p), x: Some(dir), refs: vec![host.clone()] })
        }
        Place::Crossing { a, b } => {
            let ca = locus_of_ref(m, a)?;
            let cb = locus_of_ref(m, b)?;
            for (c, r) in [(&ca, a), (&cb, b)] {
                if matches!(c.locus, Locus::Point(_)) {
                    return Err(reject(
                        "not_a_line",
                        format!("crossing needs two lines; {} is not a line", r.label()),
                        vec![r.clone()],
                    ));
                }
            }
            let x = ca.x;
            let mut c = meet(ca, cb)?;
            if matches!(c.locus, Locus::Line { .. }) {
                return Err(reject(
                    "parallel",
                    format!("{} and {} coincide; they have no single crossing", a.label(), b.label()),
                    c.refs,
                ));
            }
            c.x = x;
            Ok(c)
        }
        Place::Center { of } => {
            let s = shape_of(m, of)?;
            let x = match s {
                Shape::Line { a, b, .. } => plan_dir(a, b).map(|(d, _)| d),
                _ => None,
            };
            Ok(Con { locus: Locus::Point(center_of(s)), x, refs: vec![of.clone()] })
        }
        Place::AlignTo { refs } => {
            let mut it = refs.iter();
            let first = it.next().ok_or_else(|| {
                reject("empty", "align_to names no refs".to_owned(), vec![])
            })?;
            let mut c = locus_of_ref(m, first)?;
            for r in it {
                c = meet(c, locus_of_ref(m, r)?)?;
            }
            Ok(c)
        }
        Place::Between { a, b, t } => {
            if !(0.0..=1.0).contains(t) {
                return Err(reject(
                    "bad_param",
                    format!("between t = {t} is outside 0..1"),
                    vec![a.clone(), b.clone()],
                ));
            }
            let pa = center_of(shape_of(m, a)?);
            let pb = center_of(shape_of(m, b)?);
            let x = plan_dir(pa, pb).map(|(d, _)| d);
            let p = add(pa, mul(sub(pb, pa), *t));
            Ok(Con { locus: Locus::Point(p), x, refs: vec![a.clone(), b.clone()] })
        }
        Place::Snap { near, kinds, max } => {
            let (near, max) = (near.to_m(), max.to_m());
            match m.snap(near, kinds, max) {
                Some((p, _)) if dist(p, near) <= max + TOL => {
                    Ok(Con { locus: Locus::Point(p), x: None, refs: vec![] })
                }
                _ => Err(reject(
                    "no_snap",
                    format!("no snap of {kinds:?} within {max:.3} m"),
                    vec![],
                )),
            }
        }
    }
}

// ---- combining ----

fn on_line(q: V, p: V, d: V) -> (f64, f64) {
    let w = sub(q, p);
    let s = dot([w[0], w[1], 0.0], d);
    (s, cross2(d, w).abs())
}

fn in_range(s: f64, range: Option<(f64, f64)>) -> bool {
    match range {
        Some((lo, hi)) => s >= lo - TOL && s <= hi + TOL,
        None => true,
    }
}

fn conflict(a: &Con, b: &Con, why: &str) -> Rejection {
    let refs: Vec<Ref> = a.refs.iter().chain(b.refs.iter()).cloned().collect();
    let names: Vec<String> = refs.iter().map(Ref::label).collect();
    let code = if why == "parallel" { "parallel" } else { "conflict" };
    let reason = if names.is_empty() {
        format!("placements disagree: {why}")
    } else {
        format!("placements disagree ({why}): {}", names.join(", "))
    };
    reject(code, reason, refs)
}

/// Intersection of two constraints in plan; z comes from the first point-like one.
fn meet(a: Con, b: Con) -> Result<Con, Rejection> {
    let refs: Vec<Ref> = a.refs.iter().chain(b.refs.iter()).cloned().collect();
    let x = a.x.or(b.x);
    let locus = match (&a.locus, &b.locus) {
        (Locus::Point(p), Locus::Point(q)) => {
            let dxy = len([p[0] - q[0], p[1] - q[1], 0.0]);
            if dxy > TOL {
                return Err(conflict(&a, &b, &format!("points {dxy:.3} m apart")));
            }
            Locus::Point(*p)
        }
        (Locus::Point(q), Locus::Line { p, d, range })
        | (Locus::Line { p, d, range }, Locus::Point(q)) => {
            let (s, off) = on_line(*q, *p, *d);
            if off > TOL {
                return Err(conflict(&a, &b, &format!("point is {off:.3} m off the line")));
            }
            if !in_range(s, *range) {
                return Err(conflict(&a, &b, "point is beyond the line extent"));
            }
            Locus::Point(*q)
        }
        (Locus::Line { p: p1, d: d1, range: r1 }, Locus::Line { p: p2, d: d2, range: r2 }) => {
            let den = cross2(*d1, *d2);
            if den.abs() < 1e-9 {
                let (_, off) = on_line(*p2, *p1, *d1);
                if off > TOL {
                    return Err(conflict(&a, &b, "parallel"));
                }
                // Coincident: keep the narrower extent.
                let range = match (r1, r2) {
                    (Some(_), _) => *r1,
                    (None, Some((lo, hi))) => {
                        let (s0, _) = on_line(add(*p2, mul(*d2, *lo)), *p1, *d1);
                        let (s1, _) = on_line(add(*p2, mul(*d2, *hi)), *p1, *d1);
                        Some((s0.min(s1), s0.max(s1)))
                    }
                    (None, None) => None,
                };
                Locus::Line { p: *p1, d: *d1, range }
            } else {
                let w = sub(*p2, *p1);
                let s = cross2(w, *d2) / den;
                let t = cross2(w, *d1) / den;
                if !in_range(s, *r1) || !in_range(t, *r2) {
                    return Err(conflict(&a, &b, "lines cross beyond their extent"));
                }
                Locus::Point(add(*p1, mul(*d1, s)))
            }
        }
    };
    Ok(Con { locus, x, refs })
}

fn finish(c: Con, hint: Option<V>) -> Result<Placed, Rejection> {
    let x = c.x.unwrap_or([1.0, 0.0, 0.0]);
    let at = match c.locus {
        Locus::Point(p) => p,
        Locus::Line { p, d, range } => {
            // Under-determined: move the given point the least (project it onto the line).
            let Some(h) = hint else {
                return Err(reject(
                    "underdetermined",
                    "placement leaves a line of positions; give a second ref or a current point"
                        .to_owned(),
                    c.refs,
                ));
            };
            let (s, _) = on_line(h, p, d);
            if !in_range(s, range) {
                let names: Vec<String> = c.refs.iter().map(Ref::label).collect();
                return Err(reject(
                    "outside_extent",
                    format!("projected point lies beyond {}", names.join(", ")),
                    c.refs,
                ));
            }
            let q = add(p, mul(d, s));
            [q[0], q[1], h[2]]
        }
    };
    Ok(Placed { at, x, refs: c.refs })
}

/// Resolve one intent. `hint` is the current or proposed point (metres) used only when the
/// intent leaves a line of positions; it is moved the least.
pub fn resolve(m: &dyn PlaceModel, place: &Place, hint: Option<V>) -> Result<Placed, Rejection> {
    finish(constraint(m, place)?, hint)
}

/// Resolve several intents that must hold together (for example an offset from a wall face
/// plus alignment to a grid). Disagreeing intents are rejected with all refs involved.
pub fn resolve_all(
    m: &dyn PlaceModel,
    places: &[Place],
    hint: Option<V>,
) -> Result<Placed, Rejection> {
    let mut it = places.iter();
    let first = it
        .next()
        .ok_or_else(|| reject("empty", "no placement given".to_owned(), vec![]))?;
    let mut c = constraint(m, first)?;
    for p in it {
        c = meet(c, constraint(m, p)?)?;
    }
    finish(c, hint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refs::Cat;
    use crate::units::Len;

    struct M {
        items: Vec<(Ref, Shape)>,
        snaps: Vec<(V, SnapKind)>,
    }

    impl PlaceModel for M {
        fn shape(&self, r: &Ref) -> Option<Shape> {
            self.items.iter().find(|(k, _)| k == r).map(|(_, s)| *s)
        }
        fn snap(&self, near: V, kinds: &[SnapKind], max: f64) -> Option<(V, SnapKind)> {
            self.snaps
                .iter()
                .filter(|(p, k)| kinds.contains(k) && dist(*p, near) <= max)
                .min_by(|a, b| dist(a.0, near).total_cmp(&dist(b.0, near)))
                .copied()
        }
    }

    fn wall() -> Ref {
        Ref::id(Cat::Wall, 1)
    }
    fn grid(n: &str) -> Ref {
        Ref::name(Cat::Grid, n)
    }
    fn grid_line(a: [f64; 2], b: [f64; 2]) -> Shape {
        Shape::Line { a: [a[0], a[1], 0.0], b: [b[0], b[1], 0.0], half: 0.0, bounded: false }
    }

    /// Wall (0,0)-(10,0), 200 mm thick; grids A,B vertical at x=0,6; grids 1,2 horizontal.
    fn model() -> M {
        M {
            items: vec![
                (
                    wall(),
                    Shape::Line { a: [0.0, 0.0, 0.0], b: [10.0, 0.0, 0.0], half: 0.1, bounded: true },
                ),
                (grid("A"), grid_line([0.0, -5.0], [0.0, 20.0])),
                (grid("B"), grid_line([6.0, -5.0], [6.0, 20.0])),
                (grid("1"), grid_line([-5.0, 0.0], [20.0, 0.0])),
                (grid("2"), grid_line([-5.0, 4.0], [20.0, 4.0])),
                (Ref::id(Cat::Room, 7), Shape::Box { min: [0.0, 0.0, 0.0], max: [6.0, 4.0, 3.0] }),
            ],
            snaps: vec![([10.0, 0.0, 0.0], SnapKind::Endpoint)],
        }
    }

    fn near(a: V, b: V) -> bool {
        dist(a, b) < 1e-9
    }

    #[test]
    fn at_is_taken_as_given() {
        let p = resolve(&model(), &Place::At { p: P3::m(1.0, 2.0, 3.0) }, None).unwrap();
        assert!(near(p.at, [1.0, 2.0, 3.0]));
    }

    #[test]
    fn offset_from_wall_face() {
        let m = model();
        let off = Place::Offset { from: wall(), d: Len::mm(500.0), side: Side::Left };
        let p = resolve(&m, &off, Some([3.0, 2.0, 0.0])).unwrap();
        assert!(near(p.at, [3.0, 0.6, 0.0]), "{:?}", p.at);
        assert!(near(p.x, [1.0, 0.0, 0.0]));
        // Same point through the explicit face part, measured from the face itself.
        let face = Ref::part(wall(), "face:left");
        let via = Place::Offset { from: face, d: Len::mm(500.0), side: Side::Center };
        let q = resolve(&m, &via, Some([3.0, 2.0, 0.0])).unwrap();
        assert!(near(q.at, p.at));
        let right = Place::Offset { from: wall(), d: Len::m(0.4), side: Side::Right };
        let r = resolve(&m, &right, Some([3.0, 2.0, 0.0])).unwrap();
        assert!(near(r.at, [3.0, -0.5, 0.0]), "{:?}", r.at);
        // A line alone needs a second ref or a current point.
        assert_eq!(resolve(&m, &off, None).unwrap_err().code, Code::Invalid);
    }

    #[test]
    fn offset_along_wall_stays_within_wall_or_is_refused() {
        let m = model();
        let host = |s: f64, end: End| Place::OnHost {
            host: wall(),
            along: Len::m(s),
            from_end: end,
            side: Side::Left,
            z: Len::m(0.9),
        };
        let p = resolve(&m, &host(2.5, End::A), None).unwrap();
        assert!(near(p.at, [2.5, 0.1, 0.9]), "{:?}", p.at);
        let q = resolve(&m, &host(2.5, End::B), None).unwrap();
        assert!(near(q.at, [7.5, 0.1, 0.9]), "{:?}", q.at);
        let e = resolve(&m, &host(10.5, End::A), None).unwrap_err();
        assert_eq!(e.code, Code::OutsideHost);
        assert_eq!(e.refs, vec![wall()]);
        let why = e.got.clone().unwrap_or_default();
        assert!(why.contains("10.500"), "{why}");
        assert_eq!(resolve(&m, &host(-0.1, End::B), None).unwrap_err().code, Code::OutsideHost);
        // An offset line from a bounded wall does not extend past the wall ends.
        let off = Place::Offset { from: wall(), d: Len::m(1.0), side: Side::Left };
        let e = resolve(&m, &off, Some([12.0, 3.0, 0.0])).unwrap_err();
        assert_eq!(e.code, Code::OffHost);
    }

    #[test]
    fn grid_crossing_point() {
        let c = Place::Crossing { a: grid("B"), b: grid("2") };
        let p = resolve(&model(), &c, None).unwrap();
        assert!(near(p.at, [6.0, 4.0, 0.0]), "{:?}", p.at);
        assert_eq!(p.refs, vec![grid("B"), grid("2")]);
    }

    #[test]
    fn grid_crossing_of_parallel_grids_is_refused() {
        let c = Place::Crossing { a: grid("A"), b: grid("B") };
        let e = resolve(&model(), &c, None).unwrap_err();
        assert_eq!(e.code, Code::Parallel);
        assert_eq!(e.refs, vec![grid("A"), grid("B")]);
        let why = e.got.clone().unwrap_or_default();
        assert!(why.contains("grid:A") && why.contains("grid:B"), "{why}");
    }

    #[test]
    fn align_three_columns_to_grid() {
        let m = model();
        let align = Place::AlignTo { refs: vec![grid("A")] };
        let cols = [[0.12, 1.0, 0.0], [-0.07, 5.0, 0.0], [0.03, 9.0, 0.0]];
        let out: Vec<V> = cols.iter().map(|h| resolve(&m, &align, Some(*h)).unwrap().at).collect();
        for (o, h) in out.iter().zip(cols) {
            assert!(o[0].abs() < 1e-9, "{o:?}");
            assert!((o[1] - h[1]).abs() < 1e-9, "least move keeps y: {o:?}");
        }
        // Two grids fix the point fully.
        let both = Place::AlignTo { refs: vec![grid("A"), grid("2")] };
        assert!(near(resolve(&m, &both, None).unwrap().at, [0.0, 4.0, 0.0]));
    }

    #[test]
    fn conflicting_constraints_rejected() {
        let m = model();
        let on_wall = Place::OnHost {
            host: wall(),
            along: Len::m(2.0),
            from_end: End::A,
            side: Side::Center,
            z: Len::m(0.0),
        };
        let at_grid = Place::Crossing { a: grid("B"), b: grid("1") };
        let e = resolve_all(&m, &[on_wall.clone(), at_grid], None).unwrap_err();
        assert_eq!(e.code, Code::Conflict);
        assert_eq!(e.refs, vec![wall(), grid("B"), grid("1")]);
        // Consistent pair resolves: 6 m along the wall is grid B on grid 1.
        let ok = Place::OnHost {
            host: wall(),
            along: Len::m(6.0),
            from_end: End::A,
            side: Side::Center,
            z: Len::m(0.0),
        };
        let at = Place::Crossing { a: grid("B"), b: grid("1") };
        assert!(near(resolve_all(&m, &[ok, at], None).unwrap().at, [6.0, 0.0, 0.0]));
        // Parallel alignments that never meet.
        let par = Place::AlignTo { refs: vec![grid("1"), grid("2")] };
        assert_eq!(resolve(&m, &par, None).unwrap_err().code, Code::Parallel);
        assert_eq!(resolve_all(&m, &[], None).unwrap_err().code, Code::EmptySelection);
    }

    #[test]
    fn center_between_snap_and_unknown_refs() {
        let m = model();
        let c = resolve(&m, &Place::Center { of: Ref::id(Cat::Room, 7) }, None).unwrap();
        assert!(near(c.at, [3.0, 2.0, 1.5]));
        let w = resolve(&m, &Place::Center { of: wall() }, None).unwrap();
        assert!(near(w.at, [5.0, 0.0, 0.0]));
        let ends = Place::Between {
            a: Ref::part(wall(), "end:a"),
            b: Ref::part(wall(), "end:b"),
            t: 0.25,
        };
        assert!(near(resolve(&m, &ends, None).unwrap().at, [2.5, 0.0, 0.0]));
        let bad_t = Place::Between { a: wall(), b: grid("A"), t: 1.5 };
        assert_eq!(resolve(&m, &bad_t, None).unwrap_err().code, Code::OutOfRange);
        let snap = |max: f64| Place::Snap {
            near: P3::m(9.95, 0.02, 0.0),
            kinds: vec![SnapKind::Endpoint],
            max: Len::m(max),
        };
        assert!(near(resolve(&m, &snap(0.1), None).unwrap().at, [10.0, 0.0, 0.0]));
        assert_eq!(resolve(&m, &snap(0.01), None).unwrap_err().code, Code::NotFound);
        let ghost = Place::Center { of: Ref::id(Cat::Column, 99) };
        let e = resolve(&m, &ghost, None).unwrap_err();
        assert_eq!((e.code, e.refs), (Code::NotFound, vec![Ref::id(Cat::Column, 99)]));
        let part = Place::Center { of: Ref::part(wall(), "face:top") };
        assert_eq!(resolve(&m, &part, None).unwrap_err().code, Code::NotFound);
        let pt = Place::Crossing { a: Ref::id(Cat::Room, 7), b: grid("A") };
        assert_eq!(resolve(&m, &pt, None).unwrap_err().code, Code::BadKind);
    }
}
