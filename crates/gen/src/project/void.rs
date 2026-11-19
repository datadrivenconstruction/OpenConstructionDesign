// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Clashes between everything that runs through the ceiling void: pipes, conduits, cable
//! trays, sprinkler mains and air ducts, and the columns, shafts, terminals and plant they
//! pass. Round pieces are capsules and rectangular ones oriented boxes, tested exactly; two
//! pieces clash when they cut into each other by more than the joint tolerance. A contact is
//! forgiven only where two runs are really joined: consecutive segments of one run, or the end
//! of a run on a run of the same system.

use super::rules::rule;
use super::svc::{obstacles_with, TOL};
use super::{GProject, GRun};
use ddc_geom::math::{v3, V3};
use ddc_geom::mep::{Domain, Section, Shape};

#[derive(Clone, Copy, Debug)]
enum Body {
    Cap { a: V3, b: V3, r: f64 },
    Obb { c: V3, ax: [V3; 3], h: [f64; 3] },
}

struct Piece {
    label: String,
    /// Run it belongs to, its place in it and the run's piece count; `None` for an obstacle.
    run: Option<(usize, usize, usize)>,
    a: V3,
    b: V3,
    body: Body,
    bounds: [f64; 6],
}

fn bounds_of(body: &Body) -> [f64; 6] {
    match *body {
        Body::Cap { a, b, r } => [
            a.x.min(b.x) - r,
            a.y.min(b.y) - r,
            a.z.min(b.z) - r,
            a.x.max(b.x) + r,
            a.y.max(b.y) + r,
            a.z.max(b.z) + r,
        ],
        Body::Obb { c, ax, h } => {
            let e = |k: usize| (0..3).map(|i| h[i] * [ax[i].x, ax[i].y, ax[i].z][k].abs()).sum::<f64>();
            [c.x - e(0), c.y - e(1), c.z - e(2), c.x + e(0), c.y + e(1), c.z + e(2)]
        }
    }
}

/// The body of a straight piece of a run: a capsule for a round section, else the box of its
/// outer size with insulation, wide across the plan and high across the height.
fn body_of(sec: &Section, a: V3, b: V3) -> Body {
    let (w, h) = sec.envelope().wh();
    let d = b - a;
    let len = d.len();
    if matches!(sec.outer(), Shape::Round { .. }) || len < TOL * TOL {
        return Body::Cap { a, b, r: w.max(h) / 2.0 };
    }
    let t = d * (1.0 / len);
    let side = if t.z.abs() > 1.0 - TOL * TOL { v3(1.0, 0.0, 0.0) } else { t.cross(v3(0.0, 0.0, 1.0)).normalized() };
    let up = side.cross(t);
    Body::Obb { c: (a + b) * 0.5, ax: [t, side, up], h: [len / 2.0, w / 2.0, h / 2.0] }
}

fn box_body(b: &[f64; 6]) -> Body {
    Body::Obb {
        c: v3((b[0] + b[3]) / 2.0, (b[1] + b[4]) / 2.0, (b[2] + b[5]) / 2.0),
        ax: [v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0), v3(0.0, 0.0, 1.0)],
        h: [(b[3] - b[0]) / 2.0, (b[4] - b[1]) / 2.0, (b[5] - b[2]) / 2.0],
    }
}

/// Distance between segments `p1 q1` and `p2 q2`.
fn seg_dist(p1: V3, q1: V3, p2: V3, q2: V3) -> f64 {
    let (d1, d2, r) = (q1 - p1, q2 - p2, p1 - p2);
    let (a, e, f) = (d1.dot(d1), d2.dot(d2), d2.dot(r));
    let tiny = TOL * TOL;
    let (s, t) = if a <= tiny && e <= tiny {
        (0.0, 0.0)
    } else if a <= tiny {
        (0.0, (f / e).clamp(0.0, 1.0))
    } else {
        let c = d1.dot(r);
        if e <= tiny {
            ((-c / a).clamp(0.0, 1.0), 0.0)
        } else {
            let b = d1.dot(d2);
            let den = a * e - b * b;
            let s0 = if den > tiny * a * e { ((b * f - c * e) / den).clamp(0.0, 1.0) } else { 0.0 };
            let t0 = (b * s0 + f) / e;
            if t0 < 0.0 {
                ((-c / a).clamp(0.0, 1.0), 0.0)
            } else if t0 > 1.0 {
                (((b - c) / a).clamp(0.0, 1.0), 1.0)
            } else {
                (s0, t0)
            }
        }
    };
    ((p1 + d1 * s) - (p2 + d2 * t)).len()
}

fn point_box_dist(p: V3, c: V3, ax: &[V3; 3], h: &[f64; 3]) -> f64 {
    let d = p - c;
    (0..3).map(|i| (d.dot(ax[i]).abs() - h[i]).max(0.0).powi(2)).sum::<f64>().sqrt()
}

/// How deep two bodies cut into each other, m; zero or less when they are clear.
fn depth(x: &Body, y: &Body) -> f64 {
    match (*x, *y) {
        (Body::Cap { a, b, r }, Body::Cap { a: a2, b: b2, r: r2 }) => r + r2 - seg_dist(a, b, a2, b2),
        (Body::Cap { a, b, r }, Body::Obb { c, ax, h }) | (Body::Obb { c, ax, h }, Body::Cap { a, b, r }) => {
            // the distance from a point of the segment to a box is convex along it
            let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
            let at = |t: f64| point_box_dist(a + (b - a) * t, c, &ax, &h);
            for _ in 0..80 {
                let (m1, m2) = (lo + (hi - lo) / 3.0, hi - (hi - lo) / 3.0);
                if at(m1) < at(m2) {
                    hi = m2;
                } else {
                    lo = m1;
                }
            }
            r - at((lo + hi) / 2.0)
        }
        (Body::Obb { c: c1, ax: a1, h: h1 }, Body::Obb { c: c2, ax: a2, h: h2 }) => {
            let mut axes = vec![];
            axes.extend(a1);
            axes.extend(a2);
            for p in a1 {
                for q in a2 {
                    axes.push(p.cross(q));
                }
            }
            let mut least = f64::MAX;
            for l in axes {
                let n = l.len();
                if n < TOL * TOL {
                    continue;
                }
                let l = l * (1.0 / n);
                let r1: f64 = (0..3).map(|i| h1[i] * a1[i].dot(l).abs()).sum();
                let r2: f64 = (0..3).map(|i| h2[i] * a2[i].dot(l).abs()).sum();
                least = least.min(r1 + r2 - (c2 - c1).dot(l).abs());
            }
            least
        }
    }
}

fn near_seg(p: V3, a: V3, b: V3) -> bool {
    let d = b - a;
    let l2 = d.dot(d);
    let t = if l2 > 0.0 { ((p - a).dot(d) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (p - (a + d * t)).len() <= TOL
}

/// Whether the contact of two pieces is a real joint.
fn joined(x: &Piece, y: &Piece) -> bool {
    let (Some((rx, ix, nx)), Some((ry, iy, ny))) = (x.run, y.run) else { return false };
    if rx == ry {
        return ix.abs_diff(iy) <= 1;
    }
    if x.label != y.label {
        return false;
    }
    let ends = |p: &Piece, i: usize, n: usize| -> Vec<V3> {
        let mut e = vec![];
        if i == 0 {
            e.push(p.a);
        }
        if i + 1 == n {
            e.push(p.b);
        }
        e
    };
    let touch = ends(x, ix, nx).into_iter().any(|e| near_seg(e, y.a, y.b))
        || ends(y, iy, ny).into_iter().any(|e| near_seg(e, x.a, x.b));
    if !touch {
        return false;
    }
    // a joint meets at a point: pieces lying along each other over a length are not joined
    let (tx, ty) = (x.b - x.a, y.b - y.a);
    let (lx, ly) = (tx.len(), ty.len());
    if lx < TOL || ly < TOL || tx.dot(ty).abs() / (lx * ly) < 1.0 - TOL * TOL {
        return true;
    }
    let t = tx * (1.0 / lx);
    let (s0, s1) = (x.a.dot(t).min(x.b.dot(t)), x.a.dot(t).max(x.b.dot(t)));
    let (u0, u1) = (y.a.dot(t).min(y.b.dot(t)), y.a.dot(t).max(y.b.dot(t)));
    s1.min(u1) - s0.max(u0) <= TOL
}

/// Contacts the design wants: a duct meets the terminals it feeds, the units it leaves and the
/// shafts it rises in.
fn allowed(x: &Piece, y: &Piece) -> bool {
    let duct = |p: &Piece| p.label.starts_with("duct");
    let fitting =
        |p: &Piece| matches!(p.label.as_str(), "air terminal" | "air terminal neck" | "air handling unit");
    // a run in the shaft built for its kind
    let shaft = |a: &Piece, b: &Piece| a.run.is_some() && b.run.is_none() && super::svc::shaft_takes(&b.label, &a.label);
    (duct(x) && fitting(y)) || (duct(y) && fitting(x)) || shaft(x, y) || shaft(y, x)
}

fn run_pieces(label: &str, id: usize, sec: &Section, pts: &[V3], out: &mut Vec<Piece>) {
    let n = pts.len().saturating_sub(1);
    for (i, w) in pts.windows(2).enumerate() {
        let body = body_of(sec, w[0], w[1]);
        out.push(Piece { label: label.to_string(), run: Some((id, i, n)), a: w[0], b: w[1], bounds: bounds_of(&body), body });
    }
}

fn run_pts(r: &GRun) -> Vec<V3> {
    r.points.iter().map(|q| v3(q.0, q.1, q.2)).collect()
}

fn pieces(p: &GProject) -> Vec<Piece> {
    let mut out = vec![];
    let s = &p.services;
    let mut id = 0;
    for r in s.water.iter().chain(&s.elec) {
        run_pieces(&format!("{:?}", r.sys), id, &r.section, &run_pts(r), &mut out);
        id += 1;
    }
    for f in &p.fire.runs {
        run_pieces("sprinkler", id, &f.run.section, &run_pts(&f.run), &mut out);
        id += 1;
    }
    for d in &p.hvac.ducts {
        let shape = if d.size.1 > 0.0 { Shape::Rect { w: d.size.0, h: d.size.1 } } else { Shape::Round { d: d.size.0 } };
        let sec = Section::new(Domain::Duct, shape);
        let pts: Vec<V3> = d.points.iter().map(|q| v3(q.0, q.1, q.2)).collect();
        run_pieces(&format!("duct {:?}", d.sys), id, &sec, &pts, &mut out);
        id += 1;
    }
    for (b, what) in obstacles_with(p, false) {
        let body = box_body(&b);
        out.push(Piece { label: what.to_string(), run: None, a: v3(b[0], b[1], b[2]), b: v3(b[3], b[4], b[5]), bounds: b, body });
    }
    out
}

/// Every pair of pieces that cut into each other, with the depth.
fn clashing(all: &[Piece]) -> Vec<(usize, usize, f64)> {
    let mut order: Vec<usize> = (0..all.len()).collect();
    order.sort_by(|&i, &j| all[i].bounds[0].total_cmp(&all[j].bounds[0]));
    let mut out = vec![];
    for (k, &i) in order.iter().enumerate() {
        let a = &all[i];
        for &j in &order[k + 1..] {
            let b = &all[j];
            if b.bounds[0] > a.bounds[3] {
                break;
            }
            if a.run.is_none() && b.run.is_none() {
                continue;
            }
            if (1..3).any(|c| a.bounds[c] > b.bounds[c + 3] || b.bounds[c] > a.bounds[c + 3]) {
                continue;
            }
            if a.bounds[5] < b.bounds[2] || b.bounds[5] < a.bounds[2] {
                continue;
            }
            let d = depth(&a.body, &b.body);
            if d > TOL && !joined(a, b) && !allowed(a, b) {
                out.push((i.min(j), i.max(j), d));
            }
        }
    }
    out.sort_by_key(|c| (c.0, c.1));
    out
}

fn report(all: &[Piece], found: &[(usize, usize, f64)]) -> Vec<String> {
    let cap = rule("services.clash_report") as usize;
    let at = |p: &Piece| format!("{} {:.2?}-{:.2?}", p.label, (p.a.x, p.a.y, p.a.z), (p.b.x, p.b.y, p.b.z));
    let mut out: Vec<String> = found
        .iter()
        .take(cap)
        .map(|&(i, j, d)| format!("clash {} with {}, {:.0} mm deep", at(&all[i]), at(&all[j]), d * 1000.0))
        .collect();
    if found.len() > cap {
        out.push(format!("{} clashes in all", found.len()));
    }
    out
}

/// Clashes of every system's runs in the ceiling void with each other and with the columns,
/// shafts, terminals and plant. Empty when each system keeps to its own layer or goes round.
pub fn void_check(p: &GProject) -> Vec<String> {
    let all = pieces(p);
    report(&all, &clashing(&all))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(label: &str, id: usize, d: f64, pts: &[(f64, f64, f64)]) -> Vec<Piece> {
        let sec = Section::new(Domain::Pipe, Shape::Round { d });
        let p: Vec<V3> = pts.iter().map(|q| v3(q.0, q.1, q.2)).collect();
        let mut out = vec![];
        run_pieces(label, id, &sec, &p, &mut out);
        out
    }

    fn column() -> Piece {
        let b = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        Piece { label: "column".into(), run: None, a: v3(0.0, 0.0, 0.0), b: v3(1.0, 1.0, 1.0), bounds: b, body: box_body(&b) }
    }

    #[test]
    fn a_duct_through_a_lift_shaft_is_a_clash_but_one_through_its_own_duct_shaft_is_not() {
        let shaft = |label: &str| {
            let b = [0.0, 0.0, 0.0, 1.0, 1.0, 3.0];
            Piece { label: label.into(), run: None, a: v3(0.0, 0.0, 0.0), b: v3(1.0, 1.0, 3.0), bounds: b, body: box_body(&b) }
        };
        let duct = || run("duct Supply", 0, 0.2, &[(0.5, 0.5, -1.0), (0.5, 0.5, 2.0)]);
        let mut lift = duct();
        lift.push(shaft("shaft"));
        assert_eq!(clashing(&lift).len(), 1, "a lift shaft takes no duct");
        let mut own = duct();
        own.push(shaft("shaft [duct]"));
        assert!(clashing(&own).is_empty(), "the duct shaft takes it");
    }

    #[test]
    fn a_diagonal_pipe_near_the_edge_of_a_box_is_clear_though_its_bounding_box_is_not() {
        // x + y = 3 passes 0.707 m from the corner (1, 1); its bounding box covers the column
        let mut all = run("Cold", 0, 0.1, &[(0.5, 2.5, 0.5), (2.5, 0.5, 0.5)]);
        all.push(column());
        let b = bounds_of(&all[0].body);
        assert!(b[0] < 1.0 && b[3] > 1.0 && b[1] < 1.0 && b[4] > 1.0);
        assert!(clashing(&all).is_empty());
        // x + y = 2.05 passes 0.035 m from the corner, inside the pipe's 0.05 m radius
        let mut near = run("Cold", 0, 0.1, &[(0.5, 1.55, 0.5), (1.55, 0.5, 0.5)]);
        near.push(column());
        assert_eq!(clashing(&near).len(), 1);
    }

    #[test]
    fn pipes_touching_within_a_millimetre_are_clear_and_cutting_in_deeper_is_not() {
        let side = |gap: f64| {
            let mut all = run("Cold", 0, 0.1, &[(0.0, 0.0, 0.0), (3.0, 0.0, 0.0)]);
            all.extend(run("Hot", 1, 0.1, &[(0.0, 0.1 + gap, 0.0), (3.0, 0.1 + gap, 0.0)]));
            clashing(&all).len()
        };
        assert_eq!(side(0.001), 0, "a 1 mm gap");
        assert_eq!(side(-0.0005), 0, "cut in by half the joint tolerance");
        assert_eq!(side(-0.002), 1, "cut in by 2 mm");
    }

    #[test]
    fn a_run_crossing_another_at_its_joint_is_a_clash_even_in_the_same_system() {
        let mut all = run("Cold", 0, 0.1, &[(-1.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 1.0, 0.0)]);
        all.extend(run("Cold", 1, 0.1, &[(-0.5, -0.5, 0.0), (0.5, 0.5, 0.0)]));
        let found = clashing(&all);
        assert_eq!(found.len(), 2, "both segments of the bent run meet the crossing run: {found:?}");
    }

    #[test]
    fn a_branch_ending_on_a_main_of_its_system_is_joined_but_not_one_of_another_system() {
        let mut same = run("Cold", 0, 0.1, &[(0.0, 0.0, 0.0), (4.0, 0.0, 0.0)]);
        same.extend(run("Cold", 1, 0.05, &[(2.0, 0.0, 0.0), (2.0, 2.0, 0.0)]));
        assert!(clashing(&same).is_empty());
        let mut other = run("Cold", 0, 0.1, &[(0.0, 0.0, 0.0), (4.0, 0.0, 0.0)]);
        other.extend(run("Hot", 1, 0.05, &[(2.0, 0.0, 0.0), (2.0, 2.0, 0.0)]));
        assert_eq!(clashing(&other).len(), 1);
        // a branch that runs on along the main is no joint
        let mut along = run("Cold", 0, 0.1, &[(0.0, 0.0, 0.0), (4.0, 0.0, 0.0)]);
        along.extend(run("Cold", 1, 0.05, &[(1.0, 0.0, 0.0), (3.0, 0.0, 0.0)]));
        assert_eq!(clashing(&along).len(), 1);
    }

    #[test]
    fn a_run_that_loops_across_itself_is_a_clash_but_its_own_bends_are_not() {
        let bent = run("Cold", 0, 0.1, &[(0.0, 0.0, 0.0), (2.0, 0.0, 0.0), (2.0, 2.0, 0.0)]);
        assert!(clashing(&bent).is_empty());
        let looped = run("Cold", 0, 0.1, &[(0.0, 0.0, 0.0), (2.0, 0.0, 0.0), (2.0, 2.0, 0.0), (1.0, 2.0, 0.0), (1.0, -1.0, 0.0)]);
        assert_eq!(clashing(&looped).len(), 1);
    }

    #[test]
    fn a_rectangular_duct_is_tested_as_a_box_not_a_capsule() {
        let duct = |z: f64| {
            let sec = Section::new(Domain::Duct, Shape::Rect { w: 0.6, h: 0.2 });
            let mut v = vec![];
            run_pieces("duct Supply", 0, &sec, &[v3(0.0, 0.0, z), v3(3.0, 0.0, z)], &mut v);
            v.extend(run("Cold", 1, 0.05, &[(1.0, -1.0, 0.0), (1.0, 1.0, 0.0)]));
            clashing(&v).len()
        };
        // a 0.05 m pipe at z 0 against a 0.2 m high duct: clear 0.075 m above, cutting in at 0.1
        assert_eq!(duct(0.2), 0);
        assert_eq!(duct(0.1), 1);
    }
}
