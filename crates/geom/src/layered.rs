// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Layered strips (walls) and their 2D joins (KERNEL.md §5, stage K1).
//!
//! Joins are solved in plan: at each node the wall ends are trimmed along the line between
//! the intersection points of adjacent faces. Layers of any thickness meet without gaps or
//! overlaps, then each layer is extruded.
//!
//! Layer priority (lower is more important): at a T node the layer of the abutting wall runs
//! through less important host layers (which receive a cut) up to the first equal or more
//! important one; at a corner node the join path follows the face lattice: equal layers
//! meet in a miter, the less important one butts against the other.

use std::cmp::Ordering;

use crate::brep::Solid;
use crate::grid::{bbox_of, Bbox, BoxGrid};
use crate::math::{Tol, V2};
use crate::profile::{Profile, ProfileError};
use crate::wall_edit::{EndPrefs, JoinKind};

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub thickness: f64,
    /// Lower is more important (1 = structural, 5 = finish).
    pub priority: u8,
}

impl Layer {
    pub const DEFAULT_PRIORITY: u8 = 1;
    pub fn new(thickness: f64) -> Self {
        Self::with_priority(thickness, Self::DEFAULT_PRIORITY)
    }
    pub fn with_priority(thickness: f64, priority: u8) -> Self {
        Layer {
            thickness,
            priority,
        }
    }
}

/// Wall in plan: axis a→b, layers are stacked toward the left normal starting at offset `offset0`.
#[derive(Clone, Debug)]
pub struct Strip {
    pub a: V2,
    pub b: V2,
    pub layers: Vec<Layer>,
    pub offset0: f64,
    pub base: f64,
    pub height: f64,
}

/// Rectangular opening in wall coordinates: measured from point `a` along the axis.
#[derive(Clone, Copy, Debug)]
pub struct Opening {
    pub along: f64,
    pub width: f64,
    pub sill: f64,
    pub height: f64,
}

#[derive(Debug, PartialEq)]
pub enum JoinError {
    /// A coordinate, offset, base or height is not a finite number.
    NonFinite {
        strip: usize,
    },
    ZeroLength {
        strip: usize,
    },
    NoLayers {
        strip: usize,
    },
    Overlapping {
        a: usize,
        b: usize,
    },
    Degenerate {
        strip: usize,
        layer: usize,
        cause: ProfileError,
    },
    OpeningOutside {
        strip: usize,
        opening: usize,
    },
    /// Walls `a` and `b` are nearly parallel: no join is built at the node.
    NearlyParallel {
        a: usize,
        b: usize,
    },
    /// The split point is not strictly inside the axis.
    SplitOutside,
    /// Opening `opening` straddles the split point.
    OpeningStraddlesSplit {
        opening: usize,
    },
    /// Two cuts in layer `layer` of wall `strip` (from walls `a` and `b`) overlap.
    CutOverlap {
        strip: usize,
        layer: usize,
        a: usize,
        b: usize,
    },
    /// The arc axis of wall `strip` is degenerate (crate::arc).
    Arc {
        strip: usize,
        cause: crate::arc::ArcError,
    },
}

/// End of a layer piece: the trim line. `None` is a square end along the normal at the wall end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cap {
    pub p: V2,
    pub d: V2,
}

/// Pieces of one layer along the axis: ends [start, end]. Through cuts split a layer into pieces.
pub type LayerPieces = Vec<[Option<Cap>; 2]>;

/// Layer without joins or cuts: a single piece with square ends.
pub fn plain_pieces(s: &Strip) -> Vec<LayerPieces> {
    vec![vec![[None, None]]; s.layers.len()]
}

#[derive(Debug)]
pub struct Joined {
    /// For each wall, for each layer: pieces with their ends.
    pub pieces: Vec<Vec<LayerPieces>>,
    /// Filler polygons at nodes of 3+ walls.
    pub fillers: Vec<Vec<V2>>,
    /// Owner of each filler (wall, layer): the highest-priority layer at the node,
    /// ties broken by the lower wall index. Its area counts toward the owner's quantities.
    pub filler_owners: Vec<(usize, usize)>,
}

#[derive(Debug, PartialEq)]
pub struct LayerQty {
    pub plan_area: f64,
    pub volume: f64,
}

impl Strip {
    pub fn dir(&self) -> V2 {
        let d = self.b - self.a;
        d * (1.0 / d.len())
    }
    pub fn normal(&self) -> V2 {
        let d = self.dir();
        V2 { x: -d.y, y: d.x }
    }
    pub fn length(&self) -> f64 {
        (self.b - self.a).len()
    }
    /// Layer face offsets: len = layers + 1, ascending.
    pub fn bounds(&self) -> Vec<f64> {
        let mut out = vec![self.offset0];
        for l in &self.layers {
            out.push(out.last().unwrap() + l.thickness);
        }
        out
    }
}

/// Lower bound on the sine of the angle for line intersection: below it the point falls
/// outside meaningful coordinates even with `tol.angle = 0`.
const MIN_SIN: f64 = 1e-12;

/// Intersection of lines p1+d1·s and p2+d2·u. `None` if the sine of the angle between them
/// (over normalized directions) is at most `max(angle, MIN_SIN)`.
fn intersect(p1: V2, d1: V2, p2: V2, d2: V2, angle: f64) -> Option<V2> {
    let (l1, l2) = (d1.len(), d2.len());
    if !(l1 > 0.0 && l2 > 0.0) {
        return None;
    }
    let sin = d1.cross(d2) / (l1 * l2);
    if !(sin.abs() > angle.max(MIN_SIN)) {
        return None;
    }
    let p = p1 + d1 * ((p2 - p1).cross(d2) / d1.cross(d2));
    (p.x.is_finite() && p.y.is_finite()).then_some(p)
}

/// Wall end at a node, in node coordinates: t runs from the node along the wall, n is the left normal of t.
struct End {
    strip: usize,
    at_b: bool,
    t: V2,
    n: V2,
    smin: f64,
    smax: f64,
}

fn end_frame(strips: &[Strip], strip: usize, at_b: bool) -> End {
    let s = &strips[strip];
    let bounds = s.bounds();
    let (lo, hi) = (bounds[0], *bounds.last().unwrap());
    let (t, n, smin, smax) = if at_b {
        (-s.dir(), -s.normal(), -hi, -lo)
    } else {
        (s.dir(), s.normal(), lo, hi)
    };
    End {
        strip,
        at_b,
        t,
        n,
        smin,
        smax,
    }
}

/// Splits a wall in two at the axis point at distance `t` from `a`; layers and height are preserved.
pub fn split_at(s: &Strip, t: f64, tol: Tol) -> Result<(Strip, Strip), JoinError> {
    let len = s.length();
    if len <= tol.length || t <= tol.length || t >= len - tol.length {
        return Err(JoinError::SplitOutside);
    }
    let m = s.a + s.dir() * t;
    Ok((Strip { b: m, ..s.clone() }, Strip { a: m, ..s.clone() }))
}

/// Distributes openings between the two halves of a split at `t`: right-hand ones get `along` measured from the new point.
/// An opening straddling `t` is an error carrying its index.
pub fn split_openings(
    openings: &[Opening],
    t: f64,
) -> Result<(Vec<Opening>, Vec<Opening>), JoinError> {
    let eps = Tol::default().length;
    let (mut left, mut right) = (vec![], vec![]);
    for (k, o) in openings.iter().enumerate() {
        if o.along + o.width <= t + eps {
            left.push(*o);
        } else if o.along >= t - eps {
            right.push(Opening {
                along: o.along - t,
                ..*o
            });
        } else {
            return Err(JoinError::OpeningStraddlesSplit { opening: k });
        }
    }
    Ok((left, right))
}

/// Splits walls whose axes cross at interior points: the wall with the higher index
/// is split at the crossing, so `join` sees two T joins on the through wall.
pub fn resolve_crossings(strips: &[Strip], tol: Tol) -> Vec<Strip> {
    // A wall is split by crossings with the finished pieces of lower-index walls; neighbors come from the grid.
    let boxes: Vec<Bbox> = strips
        .iter()
        .map(|s| bbox_of(&[s.a, s.b], tol.length))
        .collect();
    let grid = BoxGrid::new(&boxes);
    let mut done: Vec<Vec<Strip>> = Vec::with_capacity(strips.len());
    let mut cand = vec![];
    for (j, s) in strips.iter().enumerate() {
        let mut mine = vec![s.clone()];
        grid.query(&boxes[j], &mut cand);
        for &i in cand.iter().take_while(|&&i| i < j) {
            for si in &done[i] {
                // Line si crosses axis s at a single point: only the piece `mine` containing it
                // (ordered along s) and its neighbors at the tolerance boundary are checked.
                let (d1, ds) = (si.b - si.a, s.dir());
                let den = d1.cross(ds);
                if den.abs() < 1e-12 * d1.len() {
                    continue;
                }
                let tau = (si.a - s.a).cross(d1) / -den;
                let at = mine.partition_point(|m| (m.b - s.a).dot(ds) < tau);
                for q in at.saturating_sub(1)..(at + 2).min(mine.len()) {
                    if let Some((p, r)) = crossing_split(si, &mine[q], tol) {
                        mine[q] = p;
                        mine.insert(q + 1, r);
                        break;
                    }
                }
            }
        }
        done.push(mine);
    }
    done.into_iter().flatten().collect()
}

/// Splits `sj` at the axis crossing with `si` if it lies inside both walls.
fn crossing_split(si: &Strip, sj: &Strip, tol: Tol) -> Option<(Strip, Strip)> {
    if si.length() <= tol.length || sj.length() <= tol.length {
        return None;
    }
    let (d1, d2) = (si.b - si.a, sj.b - sj.a);
    let den = d1.cross(d2);
    if den.abs() < 1e-12 * d1.len() * d2.len() {
        return None;
    }
    let u = (sj.a - si.a).cross(d2) / den;
    let v = (sj.a - si.a).cross(d1) / den;
    let (ti, tj) = (u * d1.len(), v * d2.len());
    let inside = |t: f64, l: f64| t > tol.length && t < l - tol.length;
    if inside(ti, d1.len()) && inside(tj, d2.len()) {
        split_at(sj, tj, tol).ok()
    } else {
        None
    }
}

/// Solves all joins of a wall network. Deterministic: the traversal order is fixed.
pub fn join(strips: &[Strip], tol: Tol) -> Result<Joined, JoinError> {
    join_with(strips, &[], tol)
}

/// Join preferences per wall end [a, b]; `None` is the default join.
pub type JoinPrefs = [Option<EndPrefs>; 2];

/// An end overshoot beyond `REACH_K` thicknesses means an acute angle: the join is replaced by square ends.
pub const REACH_K: f64 = 4.0;

/// `join` with join overrides: disallow (the end joins neither at a corner, nor at a T, nor at a 3+ node),
/// corner type for two walls (butt / miter / square), overshoot limit at an acute angle.
pub fn join_with(strips: &[Strip], prefs: &[JoinPrefs], tol: Tol) -> Result<Joined, JoinError> {
    for (i, s) in strips.iter().enumerate() {
        let finite = [s.a.x, s.a.y, s.b.x, s.b.y, s.offset0, s.base, s.height]
            .iter()
            .all(|v| v.is_finite())
            && s.layers.iter().all(|l| l.thickness.is_finite());
        if !finite {
            return Err(JoinError::NonFinite { strip: i });
        }
        if s.length() <= tol.length {
            return Err(JoinError::ZeroLength { strip: i });
        }
        if s.layers.is_empty() {
            return Err(JoinError::NoLayers { strip: i });
        }
    }
    let point = |i: usize, at_b: bool| if at_b { strips[i].b } else { strips[i].a };
    let boxes: Vec<Bbox> = strips.iter().map(|s| strip_box(s, tol)).collect();
    let hosts = BoxGrid::new(&boxes);

    // Nodes: groups of ends coinciding within tolerance; neighbors come from the grid, candidates
    // in ascending index order, as in an exhaustive scan.
    let ends: Vec<(usize, bool)> = (0..strips.len())
        .flat_map(|i| [(i, false), (i, true)])
        .collect();
    let end_boxes: Vec<Bbox> = ends
        .iter()
        .map(|&(i, b)| bbox_of(&[point(i, b)], tol.length))
        .collect();
    let end_grid = BoxGrid::new(&end_boxes);
    let pref = |(i, at_b): (usize, bool)| prefs.get(i).and_then(|p| p[at_b as usize]);
    let free = |k: usize| pref(ends[k]).is_some_and(|p| !p.allowed);
    let mut used = vec![false; ends.len()];
    let mut groups: Vec<Vec<usize>> = vec![];
    let mut cand = vec![];
    for k in 0..ends.len() {
        if used[k] {
            continue;
        }
        if free(k) {
            used[k] = true;
            groups.push(vec![k]);
            continue;
        }
        let node = point(ends[k].0, ends[k].1);
        end_grid.query(&end_boxes[k], &mut cand);
        let group: Vec<usize> = cand
            .iter()
            .copied()
            .filter(|&m| {
                m >= k
                    && !used[m]
                    && !free(m)
                    && (point(ends[m].0, ends[m].1) - node).len() <= tol.length
            })
            .collect();
        group.iter().for_each(|&m| used[m] = true);
        groups.push(group);
    }

    let solve = |group: &Vec<usize>| -> Result<NodeOut, JoinError> {
        let mut cand = vec![];
        let node = point(ends[group[0]].0, ends[group[0]].1);
        hosts.query(&end_boxes[group[0]], &mut cand);
        let ends_of: Vec<(usize, bool)> = group.iter().map(|&m| ends[m]).collect();
        if group.len() == 1 && free(group[0]) {
            return Ok(NodeOut {
                caps: vec![],
                cuts: vec![],
                filler: None,
            });
        }
        let out = solve_node(strips, &cand, &ends_of, node, tol)?;
        Ok(match ends_of[..] {
            [e, f] => corner_override(strips, e, f, pref(e), pref(f), node, tol, out),
            _ => out,
        })
    };
    let outs: Vec<Result<NodeOut, JoinError>> = par_map(&groups, solve);

    let mut caps: Vec<[Vec<Option<Cap>>; 2]> = strips
        .iter()
        .map(|s| [vec![None; s.layers.len()], vec![None; s.layers.len()]])
        .collect();
    let mut cuts: Vec<Cut> = vec![];
    let mut fillers = vec![];
    let mut filler_owners = vec![];
    for o in outs {
        let o = o?;
        for (i, at_b, c) in o.caps {
            caps[i][at_b as usize] = c;
        }
        cuts.extend(o.cuts);
        if let Some((f, owner)) = o.filler {
            fillers.push(f);
            filler_owners.push(owner);
        }
    }
    let pieces = build_pieces(strips, &caps, &cuts, tol)?;
    Ok(Joined {
        pieces,
        fillers,
        filler_owners,
    })
}

/// Solution for one node: end trims, host cuts, filler.
struct NodeOut {
    caps: Vec<(usize, bool, Vec<Option<Cap>>)>,
    cuts: Vec<Cut>,
    filler: Option<(Vec<V2>, (usize, usize))>,
}

static THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Sets the kernel thread count; 0 means `DDC_THREADS` or the available count (at most 8).
/// Does not affect the result: it is bit-for-bit identical for any thread count.
pub fn set_threads(n: usize) {
    THREADS.store(n, std::sync::atomic::Ordering::Relaxed);
}

/// Current kernel thread count.
pub fn threads() -> usize {
    match THREADS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => std::env::var("DDC_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map_or(1, |n| n.get())
                    .min(8)
            })
            .max(1),
        n => n,
    }
}

/// Chunked map across threads; the result order equals the input order for any
/// thread count, so the output is bit-for-bit identical.
pub(crate) fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let n = threads();
    if n <= 1 || items.len() < 512 {
        return items.iter().map(f).collect();
    }
    let chunk = items.len().div_ceil(n);
    let f = &f;
    std::thread::scope(|sc| {
        let hs: Vec<_> = items
            .chunks(chunk)
            .map(|c| sc.spawn(move || c.iter().map(f).collect::<Vec<R>>()))
            .collect();
        // A worker-thread panic is a panic of f; a sequential pass would raise the same one.
        hs.into_iter()
            .flat_map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    })
}

/// Wall bounding box over the extreme faces, expanded by the tolerance.
fn strip_box(s: &Strip, tol: Tol) -> Bbox {
    let bounds = s.bounds();
    let (lo, hi) = (bounds[0], bounds[bounds.len() - 1]);
    let n = s.normal();
    bbox_of(
        &[s.a + n * lo, s.a + n * hi, s.b + n * lo, s.b + n * hi],
        2.0 * tol.length,
    )
}

fn solve_node(
    strips: &[Strip],
    cand: &[usize],
    group: &[(usize, bool)],
    node: V2,
    tol: Tol,
) -> Result<NodeOut, JoinError> {
    let mut out = NodeOut {
        caps: vec![],
        cuts: vec![],
        filler: None,
    };
    if group.len() == 1 {
        let (i, at_b) = group[0];
        if let Some(t) = tee(strips, cand, None, i, node, tol) {
            out.caps
                .push((i, at_b, t.caps.into_iter().map(Some).collect()));
            out.cuts.extend(t.cuts);
        }
        return Ok(out);
    }
    // Node inside another wall (X after splitting): each end abuts the host as a T.
    let tees: Option<Vec<Tee>> = group
        .iter()
        .map(|&(i, _)| tee(strips, cand, None, i, node, tol))
        .collect();
    if let Some(tees) = tees {
        for (&(i, at_b), t) in group.iter().zip(tees) {
            out.caps
                .push((i, at_b, t.caps.into_iter().map(Some).collect()));
            out.cuts.extend(t.cuts);
        }
        return Ok(out);
    }
    if let Some(cs) = continuation_tee(strips, cand, group, node, tol) {
        out.caps = cs;
        return Ok(out);
    }
    let mut node_ends: Vec<End> = group
        .iter()
        .map(|&(i, at_b)| end_frame(strips, i, at_b))
        .collect();
    node_ends.sort_by(|x, y| {
        let (ax, ay) = (x.t.y.atan2(x.t.x), y.t.y.atan2(y.t.x));
        ax.partial_cmp(&ay).unwrap_or(Ordering::Equal)
    });
    let n = node_ends.len();
    if n == 2 && node_ends[0].t.cross(node_ends[1].t).abs() > tol.angle {
        let (e, f) = (&node_ends[0], &node_ends[1]);
        let ([ce, cf], fill) = corner_caps(strips, e, f, node, tol)?;
        out.caps.push((e.strip, e.at_b, ce));
        out.caps.push((f.strip, f.at_b, cf));
        out.filler = fill.map(|r| (r, filler_owner(strips, &node_ends)));
        return Ok(out);
    }
    // left[j]: face smax of end j meets face smin of the next end counterclockwise.
    let mut left = Vec::with_capacity(n);
    for j in 0..n {
        let (e, f) = (&node_ends[j], &node_ends[(j + 1) % n]);
        let cross = e.t.cross(f.t);
        let p = if cross.abs() <= tol.angle {
            if e.t.x * f.t.x + e.t.y * f.t.y > 0.0 {
                return Err(JoinError::Overlapping {
                    a: e.strip,
                    b: f.strip,
                });
            }
            node + e.n * e.smax // straight continuation: square end along the normal
        } else {
            intersect(
                node + e.n * e.smax,
                e.t,
                node + f.n * f.smin,
                f.t,
                tol.angle,
            )
            .ok_or(JoinError::NearlyParallel {
                a: e.strip,
                b: f.strip,
            })?
        };
        left.push(p);
    }
    for j in 0..n {
        let right = left[(j + n - 1) % n];
        let e = &node_ends[j];
        let d = left[j] - right;
        let cap = if d.len() <= tol.length {
            Cap { p: node, d: e.n }
        } else {
            Cap { p: right, d }
        };
        let l = strips[e.strip].layers.len();
        out.caps.push((e.strip, e.at_b, vec![Some(cap); l]));
    }
    if n >= 3 {
        let area = crate::profile::signed_area(&left);
        if area.abs() > tol.length * tol.length {
            out.filler = Some((left, filler_owner(strips, &node_ends)));
        }
    }
    Ok(out)
}

type CornerOut = ([Vec<Option<Cap>>; 2], Option<Vec<V2>>);

/// Overshoot of the end past the node along the wall (over the extreme faces); negative means the end stops before the node.
fn reach(e: &End, c: Option<Cap>, node: V2, tol: Tol) -> f64 {
    let Some(c) = c else { return 0.0 };
    [e.smin, e.smax]
        .iter()
        .map(|&s| {
            let p0 = node + e.n * s;
            intersect(p0, e.t, c.p, c.d, tol.angle).map_or(f64::INFINITY, |x| -(x - p0).dot(e.t))
        })
        .fold(f64::NEG_INFINITY, f64::max)
}

/// The face `of` facing direction `dir` (largest projection of the offset onto `dir`).
fn face_toward(of: &End, dir: V2) -> f64 {
    if (of.n * of.smax).dot(dir) >= (of.n * of.smin).dot(dir) {
        of.smax
    } else {
        of.smin
    }
}

/// Square ends of a corner: the main wall `e` runs to the far face of `f` (no farther than `REACH_K`·t),
/// the second wall `f` runs to the near face of `e`.
fn square_caps(e: &End, f: &End, node: V2, tol: Tol) -> [Cap; 2] {
    let along = |a: &End, other: &End, s: f64, lim: f64| {
        let x = intersect(node, a.t, node + other.n * s, other.t, tol.angle).unwrap_or(node);
        (x - node).dot(a.t).clamp(-lim, lim)
    };
    let te = e.smax - e.smin;
    let tf = f.smax - f.smin;
    let ce = along(e, f, face_toward(f, -e.t), REACH_K * te).min(0.0);
    let cf = along(f, e, face_toward(e, f.t), REACH_K * tf).max(0.0);
    [
        Cap {
            p: node + e.t * ce,
            d: e.n,
        },
        Cap {
            p: node + f.t * cf,
            d: f.n,
        },
    ]
}

/// Corner override for two walls: join type of the main wall and the overshoot limit at an acute angle.
#[allow(clippy::too_many_arguments)]
fn corner_override(
    strips: &[Strip],
    e: (usize, bool),
    f: (usize, bool),
    pe: Option<EndPrefs>,
    pf: Option<EndPrefs>,
    node: V2,
    tol: Tol,
    mut out: NodeOut,
) -> NodeOut {
    let (fe, ff) = (end_frame(strips, e.0, e.1), end_frame(strips, f.0, f.1));
    if fe.t.cross(ff.t).abs() <= tol.angle {
        return out;
    }
    // the main wall is the end with an explicit "butt"/"square" type; with both, the lower index
    let pick = |p: Option<EndPrefs>| p.map(|p| p.kind).filter(|k| *k != JoinKind::Miter);
    let (prim, sec, kind) = match (pick(pe), pick(pf)) {
        (Some(k), _) => (&fe, &ff, Some(k)),
        (None, Some(k)) => (&ff, &fe, Some(k)),
        _ => (&fe, &ff, None),
    };
    let cur = |end: &End| {
        out.caps
            .iter()
            .find(|c| c.0 == end.strip && c.1 == end.at_b)
            .map(|c| c.2.clone())
    };
    let n_of = |end: &End| strips[end.strip].layers.len();
    let mut caps: [Vec<Option<Cap>>; 2] = match kind {
        Some(JoinKind::Butt) => {
            let sf = face_toward(sec, -prim.t);
            let sp = face_toward(prim, sec.t);
            let cp = Cap {
                p: node + sec.n * sf,
                d: sec.t,
            };
            let cs = Cap {
                p: node + prim.n * sp,
                d: prim.t,
            };
            [vec![Some(cp); n_of(prim)], vec![Some(cs); n_of(sec)]]
        }
        Some(_) => {
            let [a, b] = square_caps(prim, sec, node, tol);
            [vec![Some(a); n_of(prim)], vec![Some(b); n_of(sec)]]
        }
        None => match (cur(prim), cur(sec)) {
            (Some(a), Some(b)) => [a, b],
            _ => return out,
        },
    };
    let over = |end: &End, cs: &[Option<Cap>]| {
        let t = end.smax - end.smin;
        cs.iter()
            .any(|&c| reach(end, c, node, tol) > REACH_K * t + tol.length)
    };
    let mut changed = kind.is_some();
    // overshoot of an explicit join is capped at REACH_K·t (the default join is capped by the node solver)
    if kind.is_some() && (over(prim, &caps[0]) || over(sec, &caps[1])) {
        let [a, b] = square_caps(prim, sec, node, tol);
        caps = [vec![Some(a); n_of(prim)], vec![Some(b); n_of(sec)]];
        changed = true;
    }
    if !changed {
        return out;
    }
    let [cp, cs] = caps;
    out.caps = vec![(prim.strip, prim.at_b, cp), (sec.strip, sec.at_b, cs)];
    out.cuts.clear();
    out.filler = None;
    out
}

/// Owner of a node filler: the most important layer, ties broken by lower indices.
fn filler_owner(strips: &[Strip], ends: &[End]) -> (usize, usize) {
    ends.iter()
        .flat_map(|e| {
            strips[e.strip]
                .layers
                .iter()
                .enumerate()
                .map(move |(h, l)| (l.priority, e.strip, h))
        })
        .min_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2)))
        .map(|(_, s, h)| (s, h))
        .unwrap_or((ends[0].strip, 0))
}

/// A node of 3+ ends where two ends are a straight continuation of one wall (the wall was split at the
/// abutment point): the remaining ends abut it as T joins, no filler. The host is the virtual
/// union of the two halves; host cuts become the ends of the halves, per layer.
fn continuation_tee(
    strips: &[Strip],
    cand: &[usize],
    group: &[(usize, bool)],
    node: V2,
    tol: Tol,
) -> Option<Vec<(usize, bool, Vec<Option<Cap>>)>> {
    if group.len() < 3 {
        return None;
    }
    let same = |x: &Strip, y: &Strip| {
        let (bx, by) = (x.bounds(), y.bounds());
        bx.len() == by.len() && bx.iter().zip(&by).all(|(u, v)| (u - v).abs() <= tol.length)
    };
    // Pair: one end is b of the first half, the other is a of the second, directions coincide.
    let (pe, pf) = group.iter().enumerate().find_map(|(x, &(i, ib))| {
        let (y, _) = group.iter().enumerate().find(|&(_, &(j, jb))| {
            ib && !jb && j != i && {
                let (di, dj) = (strips[i].dir(), strips[j].dir());
                di.cross(dj).abs() <= tol.angle.max(1e-12)
                    && di.x * dj.x + di.y * dj.y > 0.0
                    && same(&strips[i], &strips[j])
            }
        })?;
        Some((x, y))
    })?;
    let (e, f) = (group[pe].0, group[pf].0);
    let virt = Strip {
        b: strips[f].b,
        ..strips[e].clone()
    };
    let d = strips[e].dir();
    let mut out = vec![];
    let mut cuts = vec![];
    for (k, &(g, gb)) in group.iter().enumerate() {
        if k == pe || k == pf {
            continue;
        }
        let t = tee(strips, cand, Some((e, &virt)), g, node, tol)?;
        if t.cuts.iter().any(|c| c.host != e) {
            return None;
        }
        out.push((g, gb, t.caps.into_iter().map(Some).collect()));
        cuts.extend(t.cuts);
    }
    let square = Cap {
        p: node,
        d: strips[e].normal(),
    };
    let l = strips[e].layers.len();
    let (mut ce, mut cf) = (vec![Some(square); l], vec![Some(square); l]);
    for h in 0..l {
        let at = |c: &Cap| {
            let p = intersect(node, d, c.p, c.d, tol.angle)?;
            Some(((p - node).x * d.x + (p - node).y * d.y, *c))
        };
        let lines: Vec<(f64, Cap)> = cuts
            .iter()
            .filter(|c| c.layer == h)
            .flat_map(|c| c.lines.iter())
            .map(at)
            .collect::<Option<_>>()?;
        if lines.is_empty() {
            continue;
        }
        // Half e lies before the node (t < 0), f after it.
        let lo = lines.iter().min_by(|x, y| x.0.total_cmp(&y.0))?;
        let hi = lines.iter().max_by(|x, y| x.0.total_cmp(&y.0))?;
        if !(lo.0 < -tol.length && hi.0 > tol.length) {
            return None;
        }
        ce[h] = Some(lo.1);
        cf[h] = Some(hi.1);
    }
    out.push((e, true, ce));
    out.push((f, false, cf));
    Some(out)
}

/// Through cut in a host layer made by the layers of the abutting wall `cutter` between lines `lines`.
struct Cut {
    host: usize,
    layer: usize,
    cutter: usize,
    lines: [Cap; 2],
}

struct Tee {
    caps: Vec<Cap>,
    cuts: Vec<Cut>,
}

/// Cuts layers into pieces at the cuts; cuts on one layer are ordered along the axis.
fn build_pieces(
    strips: &[Strip],
    caps: &[[Vec<Option<Cap>>; 2]],
    cuts: &[Cut],
    tol: Tol,
) -> Result<Vec<Vec<LayerPieces>>, JoinError> {
    let mut by_host: Vec<Vec<usize>> = vec![vec![]; strips.len()];
    for (k, c) in cuts.iter().enumerate() {
        by_host[c.host].push(k);
    }
    let idx: Vec<usize> = (0..strips.len()).collect();
    let one = |&si: &usize| -> Result<Vec<LayerPieces>, JoinError> {
        let s = &strips[si];
        let (d, n) = (s.dir(), s.normal());
        let bounds = s.bounds();
        let mut layers = Vec::with_capacity(s.layers.len());
        for h in 0..s.layers.len() {
            let mid = s.a + n * (0.5 * (bounds[h] + bounds[h + 1]));
            let along = |c: &Cap| {
                let p = intersect(mid, d, c.p, c.d, tol.angle).unwrap_or(mid);
                (p - s.a).x * d.x + (p - s.a).y * d.y
            };
            let mut mine: Vec<(f64, f64, Cap, Cap, usize)> = by_host[si]
                .iter()
                .map(|&k| &cuts[k])
                .filter(|c| c.layer == h)
                .map(|c| {
                    let (t0, t1) = (along(&c.lines[0]), along(&c.lines[1]));
                    if t0 <= t1 {
                        (t0, t1, c.lines[0], c.lines[1], c.cutter)
                    } else {
                        (t1, t0, c.lines[1], c.lines[0], c.cutter)
                    }
                })
                .collect();
            mine.sort_by(|x, y| {
                (x.0, x.4)
                    .partial_cmp(&(y.0, y.4))
                    .unwrap_or(Ordering::Equal)
            });
            for w in mine.windows(2) {
                if w[1].0 < w[0].1 - tol.length {
                    return Err(JoinError::CutOverlap {
                        strip: si,
                        layer: h,
                        a: w[0].4,
                        b: w[1].4,
                    });
                }
            }
            let mut pieces = vec![];
            let mut start = caps[si][0][h];
            for c in &mine {
                pieces.push([start, Some(c.2)]);
                start = Some(c.3);
            }
            pieces.push([start, caps[si][1][h]]);
            layers.push(pieces);
        }
        Ok(layers)
    };
    par_map(&idx, one).into_iter().collect()
}

/// Faces of a wall end in node coordinates by descending offset, and the layer indices between them.
fn frame_faces(s: &Strip, at_b: bool) -> (Vec<f64>, Vec<usize>) {
    let b = s.bounds();
    let l = s.layers.len();
    if at_b {
        (b.iter().map(|o| -o).collect(), (0..l).collect())
    } else {
        (b.iter().rev().copied().collect(), (0..l).rev().collect())
    }
}

/// Corner of two walls: the join path along the face-intersection lattice from corner (e.smax, f.smin)
/// to (e.smin, f.smax). A step along both is a miter; otherwise the less important layer butts against the other's face.
fn corner_caps(
    strips: &[Strip],
    e: &End,
    f: &End,
    node: V2,
    tol: Tol,
) -> Result<CornerOut, JoinError> {
    let err = JoinError::NearlyParallel {
        a: e.strip,
        b: f.strip,
    };
    if intersect(node, e.t, node, f.t, tol.angle).is_none() {
        return Err(err);
    }
    let (fe, le) = frame_faces(&strips[e.strip], e.at_b);
    let (mut ff, mut lf) = frame_faces(&strips[f.strip], f.at_b);
    ff.reverse();
    lf.reverse();
    let (pe, pf) = (&strips[e.strip].layers, &strips[f.strip].layers);
    let v = |i: usize, j: usize| {
        // Same directions as in the check above: the intersection exists.
        intersect(node + e.n * fe[i], e.t, node + f.n * ff[j], f.t, tol.angle).unwrap_or(node)
    };
    let mut ce = vec![None; le.len()];
    let mut cf = vec![None; lf.len()];
    let (mut i, mut j) = (0, 0);
    let mut path = vec![v(0, 0)];
    while i < le.len() || j < lf.len() {
        let (di, dj) = if i == le.len() {
            (0, 1)
        } else if j == lf.len() {
            (1, 0)
        } else {
            match pe[le[i]].priority.cmp(&pf[lf[j]].priority) {
                Ordering::Equal => (1, 1),
                Ordering::Less => (0, 1),
                Ordering::Greater => (1, 0),
            }
        };
        let (p, q) = (v(i, j), v(i + di, j + dj));
        let cap = Some(Cap { p, d: q - p });
        if di == 1 {
            ce[le[i]] = cap;
        }
        if dj == 1 {
            cf[lf[j]] = cap;
        }
        i += di;
        j += dj;
        path.push(v(i, j));
    }
    // A miter is acceptable if each end line crosses the faces of its layer before the far end of
    // the wall and no farther than MITER_LIMIT·(Te+Tf) from the node (no spikes at acute and shallow angles).
    let reach = MITER_LIMIT * (fe[0] - fe[fe.len() - 1] + ff[ff.len() - 1] - ff[0]).abs();
    let ok = |c: &[Vec<Option<Cap>>; 2]| {
        caps_ok(strips, e, node, &c[0], reach, tol) && caps_ok(strips, f, node, &c[1], reach, tol)
    };
    let lattice = [ce, cf];
    if ok(&lattice) {
        return Ok((lattice, None));
    }
    let all = |c: Cap| [vec![Some(c); le.len()], vec![Some(c); lf.len()]];
    // A single line through the extreme lattice corners (a common miter for all layers).
    let (first, last) = (path[0], path[path.len() - 1]);
    if (last - first).len() > tol.length {
        let m = all(Cap {
            p: first,
            d: last - first,
        });
        if ok(&m) {
            return Ok((m, None));
        }
    }
    // Butt join along the bisector through the node: shallow bend and unequal thickness.
    let w = e.t - f.t;
    if w.len() > tol.angle.max(MIN_SIN) {
        let b = all(Cap {
            p: node,
            d: V2 { x: -w.y, y: w.x },
        });
        if ok(&b) {
            return Ok((b, None));
        }
    }
    // Bevel: both walls are cut square through the meeting point of the inner faces;
    // the outer wedge between the ends is the filler.
    let ccw = e.t.cross(f.t) > 0.0;
    let (ei, eo, fi, fo) = if ccw {
        (e.smax, e.smin, f.smin, f.smax)
    } else {
        (e.smin, e.smax, f.smax, f.smin)
    };
    let i = intersect(node + e.n * ei, e.t, node + f.n * fi, f.t, tol.angle).ok_or(err)?;
    let (se, sf) = ((i - node).dot(e.t), (i - node).dot(f.t));
    let (le_len, lf_len) = (strips[e.strip].length(), strips[f.strip].length());
    if !(se > tol.length && sf > tol.length && se < le_len - tol.length && sf < lf_len - tol.length)
    {
        return Err(JoinError::Overlapping {
            a: e.strip.min(f.strip),
            b: e.strip.max(f.strip),
        });
    }
    let oe = node + e.t * se + e.n * eo;
    let of = node + f.t * sf + f.n * fo;
    let mut tri = vec![i, oe, of];
    if crate::profile::signed_area(&tri) < 0.0 {
        tri.reverse();
    }
    let caps = [
        vec![Some(Cap { p: i, d: e.n }); le.len()],
        vec![Some(Cap { p: i, d: f.n }); lf.len()],
    ];
    let fill = (crate::profile::signed_area(&tri) > tol.length * tol.length).then_some(tri);
    Ok((caps, fill))
}

/// Miter length limit relative to the sum of the corner wall thicknesses.
const MITER_LIMIT: f64 = 2.0;

/// The ends of `e` cross both faces of their layers at a distance from the node of at most `reach`
/// and before the far end of the wall.
fn caps_ok(
    strips: &[Strip],
    e: &End,
    node: V2,
    caps: &[Option<Cap>],
    reach: f64,
    tol: Tol,
) -> bool {
    let s = &strips[e.strip];
    let (b, len) = (s.bounds(), s.length());
    caps.iter().enumerate().all(|(h, c)| {
        let Some(c) = c else { return false };
        [b[h], b[h + 1]].iter().all(|&o| {
            let o = if e.at_b { -o } else { o };
            intersect(node + e.n * o, e.t, c.p, c.d, 1e-9).is_some_and(|p| {
                (p - node).dot(e.t) < len - tol.length && (p - node).len() <= reach
            })
        })
    })
}

/// A wall end abuts the body of another wall (T join). Each layer runs through
/// less important host layers up to the first equal or more important one; the host layers passed
/// through receive a cut along the side faces of the passing layers.
/// `cand` lists host candidates in ascending order; `over` overrides the geometry of one wall.
fn tee(
    strips: &[Strip],
    cand: &[usize],
    over: Option<(usize, &Strip)>,
    i: usize,
    node: V2,
    tol: Tol,
) -> Option<Tee> {
    let own = &strips[i];
    let away = if (own.b - node).len() > (own.a - node).len() {
        own.dir()
    } else {
        -own.dir()
    };
    for &j in cand {
        if j == i {
            continue;
        }
        let host = match over {
            Some((e, s)) if e == j => s,
            _ => &strips[j],
        };
        let (d, n) = (host.dir(), host.normal());
        let rel = node - host.a;
        let along = rel.x * d.x + rel.y * d.y;
        let off = rel.x * n.x + rel.y * n.y;
        let bounds = host.bounds();
        let (lo, hi) = (bounds[0], *bounds.last().unwrap());
        let inside_len = along > tol.length && along < host.length() - tol.length;
        if inside_len && off >= lo - tol.length && off <= hi + tol.length {
            let side = away.x * n.x + away.y * n.y;
            if side.abs() <= tol.angle {
                continue; // parallel to the host: not a T
            }
            let l = host.layers.len();
            // Host layers from the near face inward.
            let order: Vec<usize> = if side > 0.0 {
                (0..l).rev().collect()
            } else {
                (0..l).collect()
            };
            let far = |h: usize| if side > 0.0 { bounds[h] } else { bounds[h + 1] };
            let near = if side > 0.0 { hi } else { lo };
            let depth: Vec<usize> = own
                .layers
                .iter()
                .map(|ol| {
                    order
                        .iter()
                        .take_while(|&&h| host.layers[h].priority > ol.priority)
                        .count()
                })
                .collect();
            let caps = depth
                .iter()
                .map(|&k| Cap {
                    p: host.a + n * if k == 0 { near } else { far(order[k - 1]) },
                    d,
                })
                .collect();
            let ob = own.bounds();
            let side_line = |o: f64| Cap {
                p: own.a + own.normal() * o,
                d: own.dir(),
            };
            let mut cuts = vec![];
            for (idx, &h) in order.iter().enumerate() {
                let mut k = 0;
                while k < depth.len() {
                    if depth[k] <= idx {
                        k += 1;
                        continue;
                    }
                    let k0 = k;
                    while k < depth.len() && depth[k] > idx {
                        k += 1;
                    }
                    cuts.push(Cut {
                        host: j,
                        layer: h,
                        cutter: i,
                        lines: [side_line(ob[k0]), side_line(ob[k])],
                    });
                }
            }
            return Some(Tee { caps, cuts });
        }
    }
    None
}

/// Plan of each wall layer: layer pieces (usually one, more with through cuts).
pub fn layer_footprints(
    s: &Strip,
    pieces: &[LayerPieces],
    strip: usize,
    tol: Tol,
) -> Result<Vec<Vec<Profile>>, JoinError> {
    let (d, n) = (s.dir(), s.normal());
    let at = |caps: &[Option<Cap>; 2], end: usize, o: f64| -> V2 {
        let base = if end == 0 { s.a } else { s.b } + n * o;
        match caps[end] {
            Some(c) => intersect(base, d, c.p, c.d, tol.angle).unwrap_or(base),
            None => base,
        }
    };
    let bounds = s.bounds();
    bounds
        .windows(2)
        .zip(pieces)
        .enumerate()
        .map(|(layer, (w, ps))| {
            ps.iter()
                .map(|c| {
                    let quad = vec![
                        at(c, 0, w[0]),
                        at(c, 1, w[0]),
                        at(c, 1, w[1]),
                        at(c, 0, w[1]),
                    ];
                    Profile::new(quad, vec![], tol).map_err(|cause| JoinError::Degenerate {
                        strip,
                        layer,
                        cause,
                    })
                })
                .collect()
        })
        .collect()
}

/// Volumes and areas per layer: exact, openings are subtracted analytically.
pub fn layer_quantities(
    s: &Strip,
    footprints: &[Vec<Profile>],
    openings: &[Opening],
    strip: usize,
) -> Result<Vec<LayerQty>, JoinError> {
    for (k, o) in openings.iter().enumerate() {
        let fits = o.along >= 0.0
            && o.along + o.width <= s.length()
            && o.sill >= 0.0
            && o.sill + o.height <= s.height;
        if !fits || o.width <= 0.0 || o.height <= 0.0 {
            return Err(JoinError::OpeningOutside { strip, opening: k });
        }
    }
    let cut: f64 = openings.iter().map(|o| o.width * o.height).sum();
    Ok(footprints
        .iter()
        .zip(&s.layers)
        .map(|(f, l)| {
            let area: f64 = f.iter().map(Profile::area).sum();
            LayerQty {
                plan_area: area,
                volume: area * s.height - cut * l.thickness,
            }
        })
        .collect())
}

/// Layer solids (without opening cuts; those arrive with the K2 booleans).
pub fn layer_solids(s: &Strip, footprints: &[Vec<Profile>], tol: Tol) -> Vec<Vec<Solid>> {
    footprints
        .iter()
        .map(|ps| {
            ps.iter()
                .map(|f| Solid::extrude(f, s.height, tol).translated_z(s.base))
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::v2;

    #[test]
    fn split_openings_rebases_and_rejects_straddle() {
        let o = |along, width| Opening {
            along,
            width,
            sill: 0.0,
            height: 1.0,
        };
        let (l, r) = split_openings(&[o(1.0, 0.9), o(4.0, 1.2), o(3.0, 0.5)], 3.0).unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].along, r[1].along), (1.0, 0.0));
        let e = split_openings(&[o(1.0, 0.9), o(2.5, 1.0)], 3.0).unwrap_err();
        assert_eq!(e, JoinError::OpeningStraddlesSplit { opening: 1 });
    }

    fn wall(a: V2, b: V2, layers: &[f64]) -> Strip {
        let total: f64 = layers.iter().sum();
        Strip {
            a,
            b,
            layers: layers.iter().map(|&t| Layer::new(t)).collect(),
            offset0: -total / 2.0,
            base: 0.0,
            height: 3.0,
        }
    }

    fn plan_area(strips: &[Strip]) -> f64 {
        let tol = Tol::default();
        let j = join(strips, tol).unwrap();
        let mut sum = 0.0;
        for (i, s) in strips.iter().enumerate() {
            for f in layer_footprints(s, &j.pieces[i], i, tol)
                .unwrap()
                .iter()
                .flatten()
            {
                assert!(f.area() > 0.0);
                sum += f.area();
            }
        }
        sum + j
            .fillers
            .iter()
            .map(|f| crate::profile::signed_area(f).abs())
            .sum::<f64>()
    }

    #[test]
    fn l_corner_tiles_without_gap_or_overlap() {
        let s = [
            wall(v2(0., 0.), v2(5., 0.), &[0.3]),
            wall(v2(5., 0.), v2(5., 4.), &[0.3]),
        ];
        assert!((plan_area(&s) - 2.7).abs() < 1e-12);
    }

    #[test]
    fn l_corner_of_different_thicknesses_and_reversed_wall() {
        // The second wall is drawn toward the node rather than away from it; the result is the same.
        let s = [
            wall(v2(0., 0.), v2(5., 0.), &[0.3]),
            wall(v2(5., 4.), v2(5., 0.), &[0.2]),
        ];
        // Union: [0,5.1]×[-0.15,0.15] + [4.9,5.1]×[0.15,4]
        assert!((plan_area(&s) - (5.1 * 0.3 + 0.2 * 3.85)).abs() < 1e-12);
    }

    #[test]
    fn layered_corner_keeps_layer_order_and_total() {
        let s = [
            wall(v2(0., 0.), v2(5., 0.), &[0.1, 0.15, 0.05]),
            wall(v2(5., 0.), v2(5., 4.), &[0.1, 0.15, 0.05]),
        ];
        assert!((plan_area(&s) - 2.7).abs() < 1e-12);
    }

    #[test]
    fn tee_trims_to_near_face_of_host() {
        let s = [
            wall(v2(0., 0.), v2(10., 0.), &[0.3]),
            wall(v2(5., 4.), v2(5., 0.), &[0.3]),
        ];
        let tol = Tol::default();
        let j = join(&s, tol).unwrap();
        let f = layer_footprints(&s[1], &j.pieces[1], 1, tol).unwrap();
        assert!((f[0][0].area() - 0.3 * 3.85).abs() < 1e-12);
        // an axis shortened along the body to the host face (import) gives the same abutment
        let s = [s[0].clone(), wall(v2(5., 4.), v2(5., 0.15), &[0.3])];
        let j = join(&s, tol).unwrap();
        let f = layer_footprints(&s[1], &j.pieces[1], 1, tol).unwrap();
        assert!((f[0][0].area() - 0.3 * 3.85).abs() < 1e-12);
    }

    #[test]
    fn three_way_node_gets_filler_and_valid_layers() {
        let s = [
            wall(v2(0., 0.), v2(5., 0.), &[0.3]),
            wall(v2(0., 0.), v2(-3., 3.), &[0.2]),
            wall(v2(0., 0.), v2(-3., -3.), &[0.25]),
        ];
        let tol = Tol::default();
        let j = join(&s, tol).unwrap();
        assert_eq!(j.fillers.len(), 1);
        for (i, w) in s.iter().enumerate() {
            assert!(layer_footprints(w, &j.pieces[i], i, tol).is_ok());
        }
    }

    #[test]
    fn straight_continuation_is_square_cut() {
        let s = [
            wall(v2(0., 0.), v2(5., 0.), &[0.3]),
            wall(v2(5., 0.), v2(9., 0.), &[0.3]),
        ];
        assert!((plan_area(&s) - 9.0 * 0.3).abs() < 1e-12);
    }

    #[test]
    fn overlapping_walls_are_reported() {
        let s = [
            wall(v2(0., 0.), v2(5., 0.), &[0.3]),
            wall(v2(0., 0.), v2(3., 0.), &[0.3]),
        ];
        assert_eq!(
            join(&s, Tol::default()).unwrap_err(),
            JoinError::Overlapping { a: 0, b: 1 }
        );
    }

    #[test]
    fn openings_are_deducted_per_layer_exactly() {
        let w = wall(v2(0., 0.), v2(6., 0.), &[0.2, 0.1]);
        let tol = Tol::default();
        let f = layer_footprints(&w, &plain_pieces(&w), 0, tol).unwrap();
        let door = Opening {
            along: 1.0,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
        };
        let q = layer_quantities(&w, &f, &[door], 0).unwrap();
        assert!((q[0].volume - (6.0 * 0.2 * 3.0 - 0.9 * 2.1 * 0.2)).abs() < 1e-12);
        assert!((q[1].volume - (6.0 * 0.1 * 3.0 - 0.9 * 2.1 * 0.1)).abs() < 1e-12);
        let bad = Opening { along: 5.5, ..door };
        assert_eq!(
            layer_quantities(&w, &f, &[bad], 0).unwrap_err(),
            JoinError::OpeningOutside {
                strip: 0,
                opening: 0
            }
        );
    }

    #[test]
    fn layer_solids_are_valid_and_sit_on_base() {
        let mut w = wall(v2(0., 0.), v2(4., 0.), &[0.2]);
        w.base = 3.0;
        let tol = Tol::default();
        let f = layer_footprints(&w, &plain_pieces(&w), 0, tol).unwrap();
        let solids = layer_solids(&w, &f, tol);
        assert_eq!(solids[0][0].validate(0), Ok(()));
        assert!(solids[0][0].verts.iter().all(|v| v.z >= 3.0 && v.z <= 6.0));
        assert!((solids[0][0].volume() - 4.0 * 0.2 * 3.0).abs() < 1e-12);
    }

    #[test]
    fn x_crossing_area_equals_union() {
        let s = [
            wall(v2(0., 0.), v2(10., 0.), &[0.3]),
            wall(v2(5., -4.), v2(5., 4.), &[0.2]),
        ];
        let r = resolve_crossings(&s, Tol::default());
        assert_eq!(r.len(), 3);
        // 10*0.3 + 8*0.2 - 0.3*0.2
        assert!((plan_area(&r) - (3.0 + 1.6 - 0.06)).abs() < 1e-12);
    }

    #[test]
    fn split_preserves_area_and_volume() {
        let w = wall(v2(0., 0.), v2(6., 0.), &[0.2, 0.1]);
        let tol = Tol::default();
        let (p, q) = split_at(&w, 2.5, tol).unwrap();
        let vol = |s: &Strip| {
            let f = layer_footprints(s, &plain_pieces(s), 0, tol).unwrap();
            let a: f64 = f.iter().flatten().map(|x| x.area()).sum();
            (a, a * s.height)
        };
        let (a0, v0) = vol(&w);
        let (a1, v1) = vol(&p);
        let (a2, v2_) = vol(&q);
        assert!((a1 + a2 - a0).abs() < 1e-12 && (v1 + v2_ - v0).abs() < 1e-12);
        assert!((a0 - 1.8).abs() < 1e-12);
    }

    /// Finish / structural / finish with priorities 5 / 1 / 5.
    fn wall3(a: V2, b: V2, t: [f64; 3]) -> Strip {
        let mut w = wall(a, b, &t);
        for (l, p) in w.layers.iter_mut().zip([5, 1, 5]) {
            l.priority = p;
        }
        w
    }

    const T3: [f64; 3] = [0.02, 0.2, 0.02];

    #[test]
    fn tee_core_runs_through_host_finish() {
        let s = [
            wall3(v2(0., 0.), v2(10., 0.), T3),
            wall3(v2(5., 4.), v2(5., 0.), T3),
        ];
        let tol = Tol::default();
        let j = join(&s, tol).unwrap();
        let fa = layer_footprints(&s[1], &j.pieces[1], 1, tol).unwrap();
        // The structural layer reaches the structural host face y = 0.10, the finishes reach the host face y = 0.12.
        assert!((fa[1][0].area() - 0.2 * 3.9).abs() < 1e-12);
        assert!((fa[0][0].area() - 0.02 * 3.88).abs() < 1e-12);
        assert!((fa[2][0].area() - 0.02 * 3.88).abs() < 1e-12);
        let fh = layer_footprints(&s[0], &j.pieces[0], 0, tol).unwrap();
        assert_eq!(fh.iter().map(Vec::len).collect::<Vec<_>>(), [1, 1, 2]);
        let top: f64 = fh[2].iter().map(Profile::area).sum();
        assert!((top - 0.02 * 9.8).abs() < 1e-12);
        assert!((plan_area(&s) - (10.0 * 0.24 + 0.24 * 3.88)).abs() < 1e-12);
    }

    #[test]
    fn tee_from_below_cuts_the_other_finish() {
        let s = [
            wall3(v2(0., 0.), v2(10., 0.), T3),
            wall3(v2(5., -4.), v2(5., 0.), T3),
        ];
        let j = join(&s, Tol::default()).unwrap();
        assert_eq!(
            j.pieces[0].iter().map(Vec::len).collect::<Vec<_>>(),
            [2, 1, 1]
        );
        assert!((plan_area(&s) - (10.0 * 0.24 + 0.24 * 3.88)).abs() < 1e-12);
    }

    #[test]
    fn x_crossing_with_priorities_equals_union() {
        let s = [
            wall3(v2(0., 0.), v2(10., 0.), T3),
            wall3(v2(5., -4.), v2(5., 4.), T3),
        ];
        let r = resolve_crossings(&s, Tol::default());
        assert!((plan_area(&r) - (10.0 * 0.24 + 8.0 * 0.24 - 0.24 * 0.24)).abs() < 1e-12);
    }

    #[test]
    fn l_corner_with_mixed_priorities_equals_union() {
        let union = 5.15 * 0.24 + 0.3 * 3.88;
        for dy in [4.0, -4.0] {
            let core = wall(v2(5., 0.), v2(5., dy), &[0.3]);
            let s = [wall3(v2(0., 0.), v2(5., 0.), T3), core.clone()];
            assert!((plan_area(&s) - union).abs() < 1e-12);
            // Reversed order and wall direction give the same union.
            let r = [
                Strip {
                    a: core.b,
                    b: core.a,
                    ..core
                },
                wall3(v2(5., 0.), v2(0., 0.), T3),
            ];
            assert!((plan_area(&r) - union).abs() < 1e-12);
        }
        let s = [
            wall3(v2(0., 0.), v2(5., 0.), T3),
            wall3(v2(5., 0.), v2(5., 4.), [0.01, 0.3, 0.03]),
        ];
        assert!((plan_area(&s) - (5.17 * 0.24 + 0.34 * 3.88)).abs() < 1e-12);
    }

    #[test]
    fn overlapping_cuts_are_reported() {
        let s = [
            wall3(v2(0., 0.), v2(10., 0.), T3),
            wall3(v2(5., 4.), v2(5., 0.), T3),
            wall3(v2(5.1, 4.), v2(5.1, 0.), T3),
        ];
        assert_eq!(
            join(&s, Tol::default()).unwrap_err(),
            JoinError::CutOverlap {
                strip: 0,
                layer: 2,
                a: 1,
                b: 2
            }
        );
    }

    #[test]
    fn split_at_ends_errors() {
        let w = wall(v2(0., 0.), v2(6., 0.), &[0.2]);
        let tol = Tol::default();
        for t in [0.0, 6.0, -1.0, 7.0] {
            assert_eq!(split_at(&w, t, tol).unwrap_err(), JoinError::SplitOutside);
        }
    }

    #[test]
    fn nearly_parallel_walls_with_zero_angle_tol_are_typed_error() {
        let tol = Tol {
            length: 1e-6,
            angle: 0.0,
        };
        for eps in [1e-13, 1e-15] {
            let s = [
                wall(v2(0., 0.), v2(5., 0.), &[0.3]),
                wall(v2(5., 0.), v2(10., eps), &[0.3]),
            ];
            assert_eq!(
                join(&s, tol).unwrap_err(),
                JoinError::NearlyParallel { a: 0, b: 1 }
            );
        }
        let three = [
            wall(v2(0., 0.), v2(5., 0.), &[0.3]),
            wall(v2(0., 0.), v2(-5., 1e-14), &[0.3]),
            wall(v2(0., 0.), v2(0., 4.), &[0.3]),
        ];
        assert!(matches!(
            join(&three, tol),
            Err(JoinError::NearlyParallel { .. }) | Ok(_)
        ));
    }

    #[test]
    fn split_at_tee_point_keeps_total_exactly() {
        let a = wall(v2(0., 0.), v2(10., 0.), &[0.1, 0.2]);
        let tol = Tol::default();
        let (l, r) = split_at(&a, 5.0, tol).unwrap();
        let s = [l, r, wall(v2(5., 0.), v2(5., 4.), &[0.3])];
        let j = join(&s, tol).unwrap();
        assert!(j.fillers.is_empty());
        let split = plan_area(&s);
        let unsplit_t = plan_area(&[a, wall(v2(5., 0.), v2(5., 4.), &[0.3])]);
        assert!((split - unsplit_t).abs() < 1e-12, "{split} vs {unsplit_t}");
    }

    #[test]
    fn genuine_three_way_filler_has_owner() {
        let s = [
            wall(v2(0., 0.), v2(5., 0.), &[0.3]),
            wall(v2(0., 0.), v2(-3., 3.), &[0.2]),
            wall(v2(0., 0.), v2(-3., -3.), &[0.25]),
        ];
        let j = join(&s, Tol::default()).unwrap();
        assert_eq!(j.filler_owners, vec![(0, 0)]);
    }

    #[test]
    fn giant_wall_does_not_slow_join_or_change_results() {
        let mut walls: Vec<Strip> = (0..10_000)
            .map(|k| {
                let (x, y) = ((k % 100) as f64 * 5.0, (k / 100) as f64 * 5.0);
                wall(v2(x, y), v2(x + 1.0, y), &[0.2])
            })
            .collect();
        let t0 = web_time::Instant::now();
        let base = join(&walls, Tol::default()).unwrap();
        let t_base = t0.elapsed();
        walls.push(wall(v2(0.0, -50.0), v2(1e9, -50.0), &[0.2]));
        let t1 = web_time::Instant::now();
        let with = join(&walls, Tol::default()).unwrap();
        let t_with = t1.elapsed();
        assert_eq!(
            format!("{:?}", base.pieces),
            format!("{:?}", &with.pieces[..10_000])
        );
        assert_eq!(base.fillers, with.fillers);
        assert!(
            t_with <= t_base * 2 + std::time::Duration::from_millis(200),
            "{t_with:?} vs {t_base:?}"
        );
    }

    /// Corner of two walls at any bend: pieces neither overlap nor leave gaps at the node,
    /// vertices have no spikes, area is exact; otherwise only a typed wall-overlap error.
    #[test]
    fn corner_sweep_is_valid_at_every_bend() {
        use crate::predicates::point_in_ring;
        use std::f64::consts::PI;
        let tol = Tol::default();
        let cases: [(&[f64], &[f64], f64, f64); 5] = [
            (&[0.3], &[0.3], 5.0, 4.0),
            (&[0.3], &[0.2], 5.0, 4.0),
            (&[0.1, 0.15, 0.05], &[0.05, 0.1], 5.0, 1.0),
            (&[0.3], &[0.25], 5.0, 0.3),
            (&[0.3], &[0.3], 0.5, 0.5),
        ];
        let mut degs: Vec<f64> = vec![0.1, 0.5, 1.0, 2.0, 3.0, 4.0, 5.0, 7.0, 10.0];
        degs.extend((1..18).map(|k| k as f64 * 10.0));
        degs.extend([175.0, 178.0, 179.0, 179.5, 179.9]);
        let (mut ok, mut over) = (0, 0);
        for &(la, lb, len1, len2) in &cases {
            for &deg in &degs {
                for sg in [1.0, -1.0] {
                    let th = deg.to_radians() * sg;
                    let node = v2(len1, 0.);
                    let s = [
                        wall(v2(0., 0.), node, la),
                        wall(node, node + v2(th.cos(), th.sin()) * len2, lb),
                    ];
                    let j = match join(&s, tol) {
                        Ok(j) => j,
                        Err(JoinError::Overlapping { .. }) => {
                            // The inner faces meet beyond the end of one of the walls.
                            let phi = PI - deg.to_radians();
                            let (ha, hb) =
                                (la.iter().sum::<f64>() / 2.0, lb.iter().sum::<f64>() / 2.0);
                            let sa = (hb + ha * phi.cos()) / phi.sin();
                            let sb = (ha + hb * phi.cos()) / phi.sin();
                            assert!(
                                sa >= len1 - 1e-6 || sb >= len2 - 1e-6,
                                "{la:?} {lb:?} {deg}"
                            );
                            over += 1;
                            continue;
                        }
                        Err(e) => panic!("{la:?} {lb:?} {deg} {sg}: {e:?}"),
                    };
                    ok += 1;
                    let mut rings: Vec<Vec<V2>> = vec![];
                    let mut area = 0.0;
                    for (i, w) in s.iter().enumerate() {
                        let fp = layer_footprints(w, &j.pieces[i], i, tol)
                            .unwrap_or_else(|e| panic!("{la:?} {lb:?} {deg} {sg}: {e:?}"));
                        for p in fp.iter().flatten() {
                            assert!(p.area() > 0.0);
                            area += p.area();
                            rings.extend(p.rings().cloned());
                        }
                    }
                    for f in &j.fillers {
                        area += crate::profile::signed_area(f).abs();
                        rings.push(f.clone());
                    }
                    let (ta, tb): (f64, f64) = (la.iter().sum(), lb.iter().sum());
                    let reach = MITER_LIMIT * (ta + tb) + 1e-9;
                    // Wall rectangle from the far end to the node.
                    let inside = |w: &Strip, t: f64, p: V2| {
                        let (d, n) = (w.dir(), w.normal());
                        let r = p - w.a;
                        let (u, v) = (r.dot(d), r.dot(n));
                        u > -1e-9 && u < w.length() + 1e-9 && v.abs() < t / 2.0 + 1e-9
                    };
                    for r in &rings {
                        for &p in r {
                            assert!(
                                (p - node).len() <= reach
                                    || inside(&s[0], ta, p)
                                    || inside(&s[1], tb, p),
                                "spike {la:?} {lb:?} {deg} {sg}: {p:?}"
                            );
                        }
                    }
                    // Coverage: at most once everywhere, exactly once away from the node.
                    let m = 60;
                    let r = reach;
                    for a in 0..m {
                        for b in 0..m {
                            let p = node
                                + v2(
                                    -r + 2.0 * r * (a as f64 + 0.37) / m as f64,
                                    -r + 2.0 * r * (b as f64 + 0.61) / m as f64,
                                );
                            let cov = rings.iter().filter(|g| point_in_ring(p, g) > 0).count();
                            assert!(cov <= 1, "overlap {la:?} {lb:?} {deg} {sg} at {p:?}");
                        }
                    }
                    if la == lb && la.len() == 1 && j.fillers.is_empty() {
                        assert!(
                            (area - ta * (len1 + len2)).abs() < 1e-9,
                            "area {la:?} {deg} {sg}: {area}"
                        );
                    }
                }
            }
        }
        assert_eq!((ok, over), (248, 62));
    }

    fn corner(deg: f64) -> Vec<Strip> {
        let a = deg.to_radians();
        let mk = |a: V2, b: V2| Strip {
            a,
            b,
            layers: vec![Layer::new(0.1), Layer::new(0.2)],
            offset0: -0.15,
            base: 0.0,
            height: 3.0,
        };
        let o = crate::math::v2(0.0, 0.0);
        vec![
            mk(crate::math::v2(5.0, 0.0), o),
            mk(o, crate::math::v2(5.0 * a.cos(), 5.0 * a.sin())),
        ]
    }

    /// Footprint area and the largest vertex distance from node (0, 0), among vertices closer than 3 m.
    fn prefs_area(strips: &[Strip], prefs: &[JoinPrefs]) -> (f64, f64) {
        let tol = Tol::default();
        let j = join_with(strips, prefs, tol).unwrap();
        let mut sum = 0.0;
        let mut reach: f64 = 0.0;
        for (i, s) in strips.iter().enumerate() {
            for l in layer_footprints(s, &j.pieces[i], i, tol).unwrap() {
                for p in l {
                    sum += crate::profile::signed_area(&p.outer).abs();
                    for q in &p.outer {
                        if q.len() < 3.0 {
                            reach = reach.max(q.len());
                        }
                    }
                }
            }
        }
        (sum, reach)
    }

    #[test]
    fn join_kinds_change_footprint() {
        use crate::wall_edit::{EndPrefs, JoinKind};
        let s = corner(90.0);
        let (base, corner_reach) = prefs_area(&s, &[]);
        let p = |kind, allowed| Some(EndPrefs { kind, allowed });
        let butt = prefs_area(&s, &[[None, p(JoinKind::Butt, true)], [None, None]]).0;
        let free = prefs_area(
            &s,
            &[
                [None, p(JoinKind::Butt, false)],
                [p(JoinKind::Butt, false), None],
            ],
        );
        // butt: same corner area with no double counting; without a join the ends stop at the node
        // (no outer corner vertex)
        assert!((butt - base).abs() < 1e-9, "{butt} {base}");
        assert!(free.1 < corner_reach - 1e-6, "{:?} {corner_reach}", free);
        let sq = prefs_area(&s, &[[None, p(JoinKind::SquareOff, true)], [None, None]]).0;
        assert!(sq.is_finite() && sq > 0.0);
    }

    #[test]
    fn acute_corner_reach_is_clamped() {
        use crate::wall_edit::{EndPrefs, JoinKind};
        for deg in [10.0, 170.0] {
            let s = corner(deg);
            let (area, _) = prefs_area(&s, &[]);
            assert!(area.is_finite() && area > 0.0, "{deg}");
            for kind in [JoinKind::Butt, JoinKind::SquareOff] {
                let p = [
                    [
                        None,
                        Some(EndPrefs {
                            kind,
                            allowed: true,
                        }),
                    ],
                    [None, None],
                ];
                let (area, reach) = prefs_area(&s, &p);
                assert!(area.is_finite() && area > 0.0, "{deg} {kind:?}");
                // the end is no farther than REACH_K·t along the wall (+ half a thickness across)
                assert!(
                    reach <= REACH_K * 0.3 + 0.3,
                    "{deg} {kind:?}: reach {reach}"
                );
            }
        }
    }
}
