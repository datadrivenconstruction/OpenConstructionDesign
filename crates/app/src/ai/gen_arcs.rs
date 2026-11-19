// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Curved facades on Apply. The generator keeps a curved facade as straight chords within the
//! arc tolerance and names each circular run of them ([`curve_walls`]); on Apply such a run is
//! one arc wall of the model. Its openings move onto the arc at the same place, measured
//! along the arc; wall-hosted fixtures follow their wall. Every other wall stays as it is.
//! A run that is not a circle becomes one spline wall once the storey is built: its chords
//! are the pieces' rows ([`SplineRun`]); the two halves of a closed curve are one closed axis.

use ddc_gen::project::{curve_walls, GCurve, GProject};
use ddc_gen::{GLayout, GWall};
use ddc_geom::nurbs::Curve;
use ddc_geom::v3;
use std::collections::BTreeMap;
use std::f64::consts::TAU;

type P = (f64, f64);

/// Two points closer than this are one, m.
const SAME: f64 = 1e-6;

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}

/// Signed angle from `u` to `v`.
fn angle(u: P, v: P) -> f64 {
    (u.0 * v.1 - u.1 * v.0).atan2(u.0 * v.0 + u.1 * v.1)
}

fn dist(a: P, b: P) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// One arc wall standing for a run.
struct Arc {
    a: P,
    b: P,
    c: P,
    r: f64,
    /// Signed sweep a->b, counter-clockwise positive.
    sweep: f64,
}

impl Arc {
    fn of(curve: &GCurve, l: &GLayout) -> Option<Arc> {
        let (c, r) = curve.arc?;
        let pts = &curve.through;
        let (&first, &last) = (pts.first()?, pts.last()?);
        let mut sweep: f64 = pts.windows(2).map(|w| angle(sub(w[0], c), sub(w[1], c))).sum();
        let roles = curve.walls.iter().map(|&w| l.walls[w].role);
        if curve.walls.len() < 2 || roles.clone().any(|x| Some(x) != roles.clone().next()) || sweep.abs() >= TAU - 1e-6 {
            return None;
        }
        // the arc runs the way its first chord wall runs
        let w0 = &l.walls[curve.walls[0]];
        let (a, b) = if dist(w0.a, first) < SAME || dist(w0.b, pts[1]) < SAME { (first, last) } else {
            sweep = -sweep;
            (last, first)
        };
        Some(Arc { a, b, c, r, sweep })
    }

    fn bulge(&self) -> f64 {
        (self.sweep / 4.0).tan()
    }

    /// Distance along the arc from `a` to the point of the arc nearest `q`.
    fn along(&self, q: P) -> f64 {
        let mut t = angle(sub(self.a, self.c), sub(q, self.c)) * self.sweep.signum();
        if t < -1e-9 {
            t += TAU;
        }
        self.r * t.min(self.sweep.abs())
    }

    fn length(&self) -> f64 {
        self.r * self.sweep.abs()
    }
}

/// A non-circular facade run: its walls in order along the axis and the axis.
#[derive(Clone, Debug)]
pub struct SplineRun {
    /// Wall indices of the merged layout, in axis order.
    pub walls: Vec<usize>,
    pub axis: Curve,
}

/// Cubic through `pts`; a closed run (first point repeated last) is a periodic cubic, smooth
/// across its start.
pub fn axis_through(pts: &[P], closed: bool) -> Option<Curve> {
    let p3 = |p: P| v3(p.0, p.1, 0.0);
    if !closed {
        let q: Vec<_> = pts.iter().map(|&p| p3(p)).collect();
        return Curve::interpolate(&q, 3).ok();
    }
    let q = &pts[..pts.len() - 1];
    let n = q.len();
    if n < 4 {
        return None;
    }
    // uniform periodic cubic: Q_j = (R_{j-1} + 4 R_j + R_{j+1}) / 6, cyclic, solved by
    // Gauss-Seidel (the system is diagonally dominant: the error halves every sweep)
    let mut r: Vec<P> = q.to_vec();
    for _ in 0..200 {
        for j in 0..n {
            let (a, b) = (r[(j + n - 1) % n], r[(j + 1) % n]);
            r[j] = ((6.0 * q[j].0 - a.0 - b.0) / 4.0, (6.0 * q[j].1 - a.1 - b.1) / 4.0);
        }
    }
    // the same curve clamped: span j is the Bezier Q_j, (2R_j + R_{j+1})/3, (R_j + 2R_{j+1})/3,
    // Q_{j+1}; inner knots of multiplicity 3
    let mut ctrl = vec![p3(q[0])];
    for j in 0..n {
        let (a, b) = (r[j], r[(j + 1) % n]);
        ctrl.push(p3(((2.0 * a.0 + b.0) / 3.0, (2.0 * a.1 + b.1) / 3.0)));
        ctrl.push(p3(((a.0 + 2.0 * b.0) / 3.0, (a.1 + 2.0 * b.1) / 3.0)));
        ctrl.push(p3(q[(j + 1) % n]));
    }
    let mut knots = vec![0.0; 4];
    for j in 1..n {
        knots.extend([j as f64; 3]);
    }
    knots.extend([n as f64; 4]);
    Curve::bspline(3, knots, ctrl).ok()
}

/// The axis points of a run in the direction its first wall runs.
fn directed(l: &GLayout, walls: &[usize], mut pts: Vec<P>, mut idx: Vec<usize>) -> (Vec<usize>, Vec<P>) {
    let w0 = &l.walls[walls[0]];
    if dist(w0.a, pts[0]) >= SAME {
        pts.reverse();
        idx.reverse();
    }
    (idx, pts)
}

/// The non-circular runs of a storey, closed curves joined from their two halves.
fn spline_runs(l: &GLayout, runs: &[GCurve]) -> Vec<(Vec<usize>, Vec<P>, bool)> {
    let same_role = |c: &GCurve| c.walls.iter().all(|&w| l.walls[w].role == l.walls[c.walls[0]].role);
    let open: Vec<&GCurve> = runs.iter().filter(|c| c.arc.is_none() && same_role(c)).collect();
    let mut used = vec![false; open.len()];
    let mut out = vec![];
    for i in 0..open.len() {
        if used[i] {
            continue;
        }
        used[i] = true;
        let a = open[i];
        let (a0, a1) = (a.through[0], *a.through.last().unwrap());
        let pair = (0..open.len()).find(|&j| {
            !used[j]
                && dist(open[j].through[0], a1) < SAME
                && dist(*open[j].through.last().unwrap(), a0) < SAME
                && l.walls[open[j].walls[0]].role == l.walls[a.walls[0]].role
        });
        match pair {
            Some(j) => {
                used[j] = true;
                let walls: Vec<usize> = a.walls.iter().chain(&open[j].walls).copied().collect();
                let pts: Vec<P> = a.through.iter().chain(&open[j].through[1..]).copied().collect();
                let (w, p) = directed(l, &walls, pts, walls.clone());
                out.push((w, p, true));
            }
            None => {
                let (w, p) = directed(l, &a.walls, a.through.clone(), a.walls.clone());
                out.push((w, p, false));
            }
        }
    }
    out
}

/// The storey with each circular facade run as one wall, and every wall's bulge.
pub fn merge_layout(l: &GLayout) -> (GLayout, Vec<f64>) {
    let (out, bulges, _) = merge_with_splines(l);
    (out, bulges)
}

/// [`merge_layout`] and the storey's spline runs over the merged walls.
pub fn merge_with_splines(l: &GLayout) -> (GLayout, Vec<f64>, Vec<SplineRun>) {
    let runs = curve_walls(l);
    let splines = spline_runs(l, &runs);
    let arcs: Vec<(GCurve, Arc)> = runs
        .into_iter()
        .filter_map(|c| Arc::of(&c, l).map(|a| (c, a)))
        .collect();
    let n = l.walls.len();
    let mut run_of: Vec<Option<usize>> = vec![None; n];
    for (k, (c, _)) in arcs.iter().enumerate() {
        for &w in &c.walls {
            run_of[w] = Some(k);
        }
    }
    let mut out = l.clone();
    out.walls.clear();
    let mut bulges = vec![];
    let mut new_idx = vec![0usize; n];
    let mut made: BTreeMap<usize, usize> = BTreeMap::new();
    for i in 0..n {
        match run_of[i] {
            None => {
                new_idx[i] = out.walls.len();
                out.walls.push(l.walls[i].clone());
                // an arc the generator made keeps its bulge
                bulges.push(l.walls[i].bulge);
            }
            Some(k) => match made.get(&k) {
                Some(&j) => new_idx[i] = j,
                None => {
                    let arc = &arcs[k].1;
                    new_idx[i] = out.walls.len();
                    made.insert(k, out.walls.len());
                    out.walls.push(GWall { a: arc.a, b: arc.b, role: l.walls[i].role, bulge: arc.bulge(), fire: l.walls[i].fire });
                    bulges.push(arc.bulge());
                }
            },
        }
    }
    for o in &mut out.openings {
        let k = o.wall;
        o.wall = new_idx[k];
        let Some(run) = run_of[k] else { continue };
        let arc = &arcs[run].1;
        let w = &l.walls[k];
        let len = dist(w.a, w.b).max(1e-12);
        let t = (o.along + o.width / 2.0) / len;
        let mid = (w.a.0 + (w.b.0 - w.a.0) * t, w.a.1 + (w.b.1 - w.a.1) * t);
        o.along = (arc.along(mid) - o.width / 2.0).clamp(0.0, (arc.length() - o.width).max(0.0));
    }
    for f in &mut out.fixtures {
        f.wall = f.wall.map(|k| new_idx[k]);
    }
    let splines = splines
        .into_iter()
        .filter_map(|(walls, pts, closed)| {
            Some(SplineRun { walls: walls.iter().map(|&w| new_idx[w]).collect(), axis: axis_through(&pts, closed)? })
        })
        .collect();
    (out, bulges, splines)
}

/// What Apply does with a storey's walls: the bulge of each, and the spline runs.
#[derive(Clone, Debug, Default)]
pub struct Curved {
    pub bulges: Vec<f64>,
    pub splines: Vec<SplineRun>,
}

/// The project with every storey's circular facade runs as arc walls, and per slot block the
/// bulges of the storey's walls and its spline runs.
pub fn merge(p: &GProject) -> (GProject, BTreeMap<u32, Curved>) {
    let mut q = p.clone();
    let mut curved = BTreeMap::new();
    let blocks: Vec<u32> = q.blocks().map(|b| b.0).collect();
    let storeys = q.storeys.iter_mut().chain(q.basements.iter_mut());
    for (k, s) in blocks.into_iter().zip(storeys) {
        let (l, bulges, splines) = merge_with_splines(&s.layout);
        s.layout = l;
        curved.insert(k, Curved { bulges, splines });
    }
    (q, curved)
}
