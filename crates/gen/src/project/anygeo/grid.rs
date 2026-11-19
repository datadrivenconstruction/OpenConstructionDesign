//! The structural grid of a plate. `grid.form` picks it; `auto` reads the ground outline: a
//! curve that is most of the facade turns the grid about its centre (radial) or along it
//! (edge), else two line families follow the longest straight facades, square when they are
//! within `grid.ortho_tolerance` of a right angle and skewed otherwise. Spans are equal and at
//! most `grid.span`. Columns stand on the grid nodes the storeys leave free: inside a room
//! that is not circulation or core, clear of every wall and door; a column above the ground
//! always stands on the one below.

use super::curves::{smooth_runs, Run};
use super::{dist, dot, perp, seg_dist, sub, unit, P};
use crate::project::footprint::{self, Region};
use crate::project::rules::rule;
use crate::project::{offset, slots, GProject, PStorey, ProjectSpec, RoomKind};
use crate::{GColumn, GGrid};

/// Directions within this are one family of facades, degrees.
const SAME_DIR_DEG: f64 = 1.0;
/// A second family turns at least this far from the first, degrees.
const MIN_FAMILY_DEG: f64 = 20.0;
/// Shortest grid line kept, m.
const MIN_LINE: f64 = 0.5;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Form {
    /// Two square families, the first along `u`.
    Ortho { u: P },
    /// Lines along `u` and lines along `v`, at any angle.
    Skew { u: P, v: P },
    /// Rays from `c` and rings about it.
    Radial { c: P },
    /// Normals of a curved facade run, rows parallel to it.
    Edge { run: Vec<P> },
}

impl Form {
    /// Index of the form in the options of `grid.form`.
    pub(crate) fn index(&self) -> usize {
        match self {
            Form::Ortho { .. } => 1,
            Form::Skew { .. } => 2,
            Form::Radial { .. } => 3,
            Form::Edge { .. } => 4,
        }
    }

    /// The grid's first axis at `p`: rooms are measured in this local frame.
    pub(crate) fn axis_at(&self, p: P) -> P {
        match self {
            Form::Ortho { u } | Form::Skew { u, .. } => *u,
            Form::Radial { c } => {
                let d = sub(p, *c);
                if d.0.hypot(d.1) < 1e-9 {
                    (1.0, 0.0)
                } else {
                    perp(unit(d))
                }
            }
            Form::Edge { run } => run
                .windows(2)
                .min_by(|a, b| seg_dist(p, a[0], a[1]).total_cmp(&seg_dist(p, b[0], b[1])))
                .map_or((1.0, 0.0), |w| unit(sub(w[1], w[0]))),
        }
    }
}

pub(crate) struct Grid {
    pub form: Form,
    pub outline: Vec<P>,
}

fn turned(u: P, deg: f64) -> P {
    let (s, c) = deg.to_radians().sin_cos();
    (u.0 * c - u.1 * s, u.0 * s + u.1 * c)
}

/// The longest straight facade family and the next one turned at least `MIN_FAMILY_DEG`
/// from it, the second on the left of the first.
fn dominant(ring: &[P]) -> (P, Option<P>) {
    let n = ring.len();
    let mut fams: Vec<(f64, f64)> = vec![];
    for i in 0..n {
        let e = sub(ring[(i + 1) % n], ring[i]);
        let l = e.0.hypot(e.1);
        if l < 1e-9 {
            continue;
        }
        let a = e.1.atan2(e.0).to_degrees().rem_euclid(180.0);
        let near = |b: f64| {
            let d = (a - b).abs();
            d.min(180.0 - d) <= SAME_DIR_DEG
        };
        match fams.iter_mut().find(|f| near(f.0)) {
            Some(f) => f.1 += l,
            None => fams.push((a, l)),
        }
    }
    fams.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.total_cmp(&y.0)));
    let Some(&(a0, _)) = fams.first() else { return ((1.0, 0.0), None) };
    let u = turned((1.0, 0.0), a0);
    let v = fams
        .iter()
        .find(|f| {
            let d = (f.0 - a0).abs();
            d.min(180.0 - d) >= MIN_FAMILY_DEG
        })
        .map(|f| {
            let v = turned((1.0, 0.0), f.0);
            if super::cross(u, v) < 0.0 {
                (-v.0, -v.1)
            } else {
                v
            }
        });
    (u, v)
}

fn lines_form(ring: &[P]) -> Form {
    let (u, v) = dominant(ring);
    let ang = rule("grid.angle");
    let u2 = turned(u, ang);
    match v {
        Some(v) => {
            let between = dot(u, v).clamp(-1.0, 1.0).acos().to_degrees();
            if (between - 90.0).abs() > rule("grid.ortho_tolerance") {
                Form::Skew { u: u2, v: turned(v, ang) }
            } else {
                Form::Ortho { u: u2 }
            }
        }
        None => Form::Ortho { u: u2 },
    }
}

fn perimeter(r: &[P]) -> f64 {
    let n = r.len();
    (0..n).map(|i| dist(r[i], r[(i + 1) % n])).sum()
}

/// The grid of a plate whose ground outline is `outline`, by `grid.form`.
pub(crate) fn choose(outline: &[P]) -> Grid {
    let runs = smooth_runs(outline, true);
    let longest: Option<&Run> = runs.iter().max_by(|a, b| a.len.total_cmp(&b.len));
    let centroid = footprint::centroid(outline);
    let ((x0, y0), (x1, y1)) = footprint::bbox(outline);
    let size = (x1 - x0).hypot(y1 - y0);
    // an arc whose centre is no farther than the plate is wide turns the grid about it
    let centre = |r: &Run| r.arc.filter(|a| a.1 <= size).map(|a| a.0);
    let pick = rule("grid.form").round() as usize;
    let form = match pick {
        1 => Form::Ortho { u: turned(dominant(outline).0, rule("grid.angle")) },
        2 => {
            let u = turned(dominant(outline).0, rule("grid.angle"));
            Form::Skew { u, v: turned(u, rule("grid.skew")) }
        }
        3 => Form::Radial { c: longest.and_then(centre).unwrap_or(centroid) },
        4 => match longest {
            Some(r) => Form::Edge { run: r.pts.clone() },
            None => lines_form(outline),
        },
        _ => match longest.filter(|r| r.len >= rule("grid.curve_share") * perimeter(outline)) {
            Some(r) if r.closed => Form::Radial { c: centre(r).unwrap_or(centroid) },
            Some(r) => match centre(r) {
                Some(c) => Form::Radial { c },
                None => Form::Edge { run: r.pts.clone() },
            },
            None => lines_form(outline),
        },
    };
    Grid { form, outline: outline.to_vec() }
}

fn range(ring: &[P], d: P) -> (f64, f64) {
    ring.iter().map(|p| dot(*p, d)).fold((f64::MAX, f64::MIN), |r, x| (r.0.min(x), r.1.max(x)))
}

/// Equal steps over `len`, each at most `span`, at most `max` of them.
fn steps(len: f64, span: f64, max: usize) -> usize {
    ((len / span - 1e-9).ceil().max(1.0) as usize).min(max)
}

/// Lines along `d` over the outline at equal spans: (a, b, offset along the normal).
fn family(ring: &[P], d: P, span: f64, over: f64, max: usize) -> Vec<(P, P, f64)> {
    let n = perp(d);
    let (lo, hi) = range(ring, n);
    let (t0, t1) = range(ring, d);
    let k = steps(hi - lo, span, max);
    (0..=k)
        .map(|i| {
            let c = lo + (hi - lo) * i as f64 / k as f64;
            let at = |t: f64| (n.0 * c + d.0 * t, n.1 * c + d.1 * t);
            (at(t0 - over), at(t1 + over), c)
        })
        .collect()
}

/// Where the ray from `c` along `d` is inside the outline: the farthest crossing.
fn ray_reach(ring: &[P], c: P, d: P) -> Option<f64> {
    let n = ring.len();
    let mut far: Option<f64> = None;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let e = sub(b, a);
        let den = super::cross(d, e);
        if den.abs() < 1e-12 {
            continue;
        }
        let w = sub(a, c);
        let s = super::cross(w, e) / den;
        let t = super::cross(w, d) / den;
        if s > 0.0 && (-1e-9..=1.0 + 1e-9).contains(&t) {
            far = Some(far.map_or(s, |f: f64| f.max(s)));
        }
    }
    far
}

/// Grid lines (family 0 numbered, family 1 lettered) and the candidate column nodes with
/// their rotation.
fn layout(g: &Grid, span: f64, over: f64) -> (Vec<(P, P, usize)>, Vec<(P, f64)>) {
    // both families together stay within the storey's block of grid slots
    let max = ((slots::COLUMNS - slots::GRIDS) / 2) as usize - 1;
    let ring = &g.outline;
    match &g.form {
        Form::Ortho { u } | Form::Skew { u, .. } => {
            let v = match &g.form {
                Form::Skew { v, .. } => *v,
                _ => perp(*u),
            };
            let fa = family(ring, *u, span, over, max);
            let fb = family(ring, v, span, over, max);
            let (na, nb) = (perp(*u), perp(v));
            let det = super::cross(na, nb);
            let mut nodes = vec![];
            if det.abs() > 1e-9 {
                for a in &fa {
                    for b in &fb {
                        let (ci, cj) = (a.2, b.2);
                        let p = ((ci * nb.1 - cj * na.1) / det, (na.0 * cj - nb.0 * ci) / det);
                        nodes.push((p, u.1.atan2(u.0)));
                    }
                }
            }
            let lines = fa.iter().map(|l| (l.0, l.1, 0)).chain(fb.iter().map(|l| (l.0, l.1, 1))).collect();
            (lines, nodes)
        }
        Form::Radial { c } => {
            let far = ring.iter().map(|p| dist(*p, *c)).fold(0.0, f64::max);
            let nr = ((360.0 / rule("grid.ray") - 1e-9).ceil() as usize).clamp(3, 2 * max);
            let rings = steps(far, span, max);
            let mut lines = vec![];
            let mut nodes = vec![];
            for i in 0..nr {
                let th = (rule("grid.angle") + 360.0 * i as f64 / nr as f64).to_radians();
                let d = (th.cos(), th.sin());
                let Some(s) = ray_reach(ring, *c, d) else { continue };
                let b = (c.0 + d.0 * (s + over), c.1 + d.1 * (s + over));
                lines.push((*c, b, 0));
                for j in 1..=rings {
                    let r = far * j as f64 / rings as f64;
                    nodes.push(((c.0 + d.0 * r, c.1 + d.1 * r), th));
                }
            }
            (lines, nodes)
        }
        Form::Edge { run } => {
            let total: f64 = run.windows(2).map(|w| dist(w[0], w[1])).sum();
            let ns = steps(total, span, 2 * max);
            let mut lines = vec![];
            let mut nodes = vec![];
            for i in 0..=ns {
                let s = total * i as f64 / ns as f64;
                // the station and the run's direction there
                let (mut acc, mut q, mut t) = (0.0, run[0], unit(sub(run[1], run[0])));
                for w in run.windows(2) {
                    let l = dist(w[0], w[1]);
                    t = unit(sub(w[1], w[0]));
                    if acc + l >= s - 1e-9 {
                        let f = ((s - acc) / l.max(1e-12)).clamp(0.0, 1.0);
                        q = (w[0].0 + (w[1].0 - w[0].0) * f, w[0].1 + (w[1].1 - w[0].1) * f);
                        break;
                    }
                    acc += l;
                    q = w[1];
                }
                let n = perp(t);
                let reach = ring.iter().map(|p| dot(sub(*p, q), n)).fold(0.0, f64::max);
                lines.push(((q.0 - n.0 * over, q.1 - n.1 * over), (q.0 + n.0 * (reach + over), q.1 + n.1 * (reach + over)), 0));
                let rows = steps(reach, span, max);
                for j in 0..=rows {
                    let r = reach * j as f64 / rows as f64;
                    nodes.push(((q.0 + n.0 * r, q.1 + n.1 * r), t.1.atan2(t.0)));
                }
            }
            (lines, nodes)
        }
    }
}

fn blocked(kind: RoomKind) -> bool {
    kind.circulation() || matches!(kind, RoomKind::Stair | RoomKind::Lift)
}

/// Distance to the nearest wall when storey `st` leaves the node `q` free: inside the slab
/// and a room that is not circulation or core, at least `grid.column_clear` from every wall
/// and `core.door_zone` from every door.
fn free(st: &PStorey, q: P, clear: f64, zone: f64) -> Option<f64> {
    let l = &st.layout;
    let slab = Region { outer: l.slab.clone(), holes: l.slab_holes.clone() };
    if l.slab.len() < 3 || !offset::contains(&slab, q) || l.shafts.iter().any(|s| crate::check::inside(&s.ring, q)) {
        return None;
    }
    let room = l.rooms.iter().position(|r| crate::check::inside(&r.polygon, q));
    if room.and_then(|i| st.rooms.get(i)).is_none_or(|r| blocked(r.kind)) {
        return None;
    }
    let wall = l.walls.iter().map(|w| seg_dist(q, w.a, w.b)).fold(f64::MAX, f64::min);
    let door = l.openings.iter().filter(|o| o.door).any(|o| {
        l.walls.get(o.wall).is_some_and(|w| {
            let t = (o.along + o.width / 2.0) / dist(w.a, w.b).max(1e-12);
            dist((w.a.0 + (w.b.0 - w.a.0) * t, w.a.1 + (w.b.1 - w.a.1) * t), q) < zone
        })
    });
    (wall >= clear && !door).then_some(wall)
}

/// Columns of each storey: the ground's free nodes, the clearest first when there are more
/// than the slots take, and up the building those that every storey below left free too.
/// A column never stands on nothing, and a change to a storey moves only its columns and
/// those above.
fn columns(nodes: &[(P, f64)], p: &GProject) -> Vec<Vec<GColumn>> {
    let clear = rule("grid.column_clear");
    let zone = rule("core.door_zone");
    let size = rule("grid.column");
    let cap = (slots::PER_STOREY - slots::COLUMNS) as usize;
    let Some(ground) = p.storeys.first() else { return vec![] };
    let mut keep: Vec<(f64, P, f64)> = vec![];
    for &(q, rot) in nodes {
        if keep.iter().any(|k| dist(k.1, q) < clear) {
            continue;
        }
        if let Some(c) = free(ground, q, clear, zone) {
            keep.push((c, q, rot));
        }
    }
    let by_place = |a: &(f64, P, f64), b: &(f64, P, f64)| a.1 .0.total_cmp(&b.1 .0).then(a.1 .1.total_cmp(&b.1 .1));
    if keep.len() > cap {
        keep.sort_by(|a, b| b.0.total_cmp(&a.0).then(by_place(a, b)));
        keep.truncate(cap);
    }
    keep.sort_by(by_place);
    let mut out = vec![];
    for (k, st) in p.storeys.iter().enumerate() {
        if k > 0 {
            keep.retain(|c| free(st, c.1, clear, zone).is_some());
        }
        out.push(keep.iter().map(|&(_, at, rotation)| GColumn { at, rotation, section: (size, size) }).collect());
    }
    out
}

/// The grid axes of the ground storey and the columns of each storey.
pub(crate) fn place(spec: &ProjectSpec, g: &Grid, p: &GProject) -> (Vec<GGrid>, Vec<Vec<GColumn>>) {
    let (lines, nodes) = layout(g, spec.grid, rule("grid.overrun"));
    let mut count = [0usize; 2];
    let mut grids = vec![];
    for (a, b, fam) in lines {
        if dist(a, b) < MIN_LINE {
            continue;
        }
        let i = count[fam];
        count[fam] += 1;
        let name = if fam == 0 { (i + 1).to_string() } else { ddc_geom::axes::axis_letter(i, spec.ru) };
        grids.push(GGrid { name, a, b });
    }
    (grids, columns(&nodes, p))
}
