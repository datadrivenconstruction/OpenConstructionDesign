// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Face surfaces: plane, cylinder, cone, sphere, torus and B-spline (rational too), with the
//! parameterisation of ISO 10303-42 so that `Su x Sv` is the surface normal; projection of
//! points to parameters, periods and degenerate (pole) sides.

use std::f64::consts::{FRAC_PI_2, TAU};

use super::curve::{placement, point, weights, Basis, MAXD};
use super::math::*;
use super::p21::{Store, P};

#[derive(Clone, Debug)]
pub struct NSurf {
    bu: Basis,
    bv: Basis,
    nv: usize,
    p: Vec<V>,
    w: Option<Vec<f64>>,
}

impl NSurf {
    fn at(&self, u: f64, v: f64) -> V {
        let (su, sv) = (self.bu.span(u), self.bv.span(v));
        let (mut fu, mut fv) = ([0.0; MAXD], [0.0; MAXD]);
        self.bu.funs(su, u, &mut fu);
        self.bv.funs(sv, v, &mut fv);
        let (du, dv) = (self.bu.deg, self.bv.deg);
        let mut acc = [0.0; 3];
        let mut ws = 0.0;
        for (i, &a) in fu.iter().enumerate().take(du + 1) {
            let row = (su - du + i) * self.nv;
            for (j, &b) in fv.iter().enumerate().take(dv + 1) {
                let k = row + sv - dv + j;
                let c = a * b * self.w.as_ref().map_or(1.0, |w| w[k]);
                acc = add(acc, mul(self.p[k], c));
                ws += c;
            }
        }
        if ws.abs() > 1e-300 {
            mul(acc, 1.0 / ws)
        } else {
            acc
        }
    }
}

#[derive(Clone, Debug)]
pub enum Kind {
    Plane,
    Cyl(f64),
    /// Radius at v = 0, tangent of the semi-angle.
    Cone(f64, f64),
    Sphere(f64),
    Torus(f64, f64),
    Spline(Box<NSurf>),
}

#[derive(Clone, Debug)]
pub struct Surf {
    pub ax: Ax,
    pub kind: Kind,
    pub per: [Option<f64>; 2],
    dom: [(f64, f64); 2],
    /// Spline only: [dir][min side, max side] collapses to a point.
    poles: [[bool; 2]; 2],
    grid: Vec<([f64; 2], V)>,
    tol: f64,
}

impl Surf {
    pub fn plane(ax: Ax, tol: f64) -> Surf {
        Surf {
            ax,
            kind: Kind::Plane,
            per: [None, None],
            dom: [(f64::MIN, f64::MAX); 2],
            poles: [[false; 2]; 2],
            grid: vec![],
            tol,
        }
    }

    /// `ang`: radians per file angle unit; `tol`: chord tolerance in file units.
    pub fn read(st: &Store, id: u64, ang: f64, tol: f64) -> Result<Surf, String> {
        if let Some(a) = st.rec(id, "B_SPLINE_SURFACE") {
            let k = st
                .rec(id, "B_SPLINE_SURFACE_WITH_KNOTS")
                .ok_or_else(|| format!("unsupported surface {}", st.describe(id)))?;
            let w = st.rec(id, "RATIONAL_B_SPLINE_SURFACE").and_then(|r| r.first());
            return spline(st, &a[..a.len().min(3)], k, w, tol);
        }
        let kind = st.kind(id).unwrap_or("?");
        let a = st.rec(id, kind).unwrap_or(&[]);
        let num = |i: usize| a.get(i).and_then(P::num).filter(|x| x.is_finite());
        let pos = || placement(st, a.get(1).and_then(P::id).unwrap_or(0));
        let rad = |i: usize| num(i).filter(|r| *r > 0.0).ok_or_else(|| format!("#{id}: bad radius"));
        let k = match kind {
            "PLANE" => return Ok(Surf::plane(pos()?, tol)),
            "CYLINDRICAL_SURFACE" => Kind::Cyl(rad(2)?),
            "CONICAL_SURFACE" => {
                let r = num(2).filter(|r| *r >= 0.0).ok_or_else(|| format!("#{id}: bad radius"))?;
                let t = (num(3).ok_or_else(|| format!("#{id}: bad semi-angle"))? * ang).tan();
                if !t.is_finite() || (r == 0.0 && t == 0.0) {
                    return Err(format!("#{id}: degenerate cone"));
                }
                Kind::Cone(r, t)
            }
            "SPHERICAL_SURFACE" => Kind::Sphere(rad(2)?),
            "TOROIDAL_SURFACE" => {
                let (big, small) = (rad(2)?, rad(3)?);
                if small >= big {
                    return Err(format!("unsupported surface {kind} (self-intersecting)"));
                }
                Kind::Torus(big, small)
            }
            "B_SPLINE_SURFACE_WITH_KNOTS" => {
                return spline(st, a.get(1..4).unwrap_or(&[]), a.get(8..).unwrap_or(&[]), None, tol)
            }
            _ => return Err(format!("unsupported surface {}", st.describe(id))),
        };
        let both = matches!(k, Kind::Torus(..));
        Ok(Surf {
            ax: pos()?,
            kind: k,
            per: [Some(TAU), if both { Some(TAU) } else { None }],
            dom: [(f64::MIN, f64::MAX); 2],
            poles: [[false; 2]; 2],
            grid: vec![],
            tol,
        })
    }

    pub fn curved(&self) -> bool {
        !matches!(self.kind, Kind::Plane)
    }

    fn wrap(&self, d: usize, x: f64) -> f64 {
        let (lo, hi) = self.dom[d];
        match self.per[d] {
            Some(p) if lo > f64::MIN => lo + (x - lo).rem_euclid(p),
            _ => x.clamp(lo, hi),
        }
    }

    pub fn eval(&self, q: [f64; 2]) -> V {
        let [u, v] = q;
        let ax = &self.ax;
        match &self.kind {
            Kind::Plane => ax.at(u, v, 0.0),
            Kind::Cyl(r) => ax.at(r * u.cos(), r * u.sin(), v),
            Kind::Cone(r, t) => {
                let rho = r + v * t;
                ax.at(rho * u.cos(), rho * u.sin(), v)
            }
            Kind::Sphere(r) => ax.at(r * v.cos() * u.cos(), r * v.cos() * u.sin(), r * v.sin()),
            Kind::Torus(big, r) => {
                let rho = big + r * v.cos();
                ax.at(rho * u.cos(), rho * u.sin(), r * v.sin())
            }
            Kind::Spline(n) => n.at(self.wrap(0, u), self.wrap(1, v)),
        }
    }

    fn derivs(&self, q: [f64; 2]) -> (V, V) {
        let h = [0, 1].map(|d| {
            let (lo, hi) = self.dom[d];
            if lo > f64::MIN { (hi - lo) * 1e-6 } else { 1e-6 }
        });
        let fd = |d: usize| {
            let (mut a, mut b) = (q, q);
            a[d] -= h[d];
            b[d] += h[d];
            if self.per[d].is_none() && self.dom[d].0 > f64::MIN {
                a[d] = a[d].max(self.dom[d].0);
                b[d] = b[d].min(self.dom[d].1);
            }
            mul(sub(self.eval(b), self.eval(a)), 1.0 / (b[d] - a[d]))
        };
        (fd(0), fd(1))
    }

    /// Unit normal `Su x Sv` direction; None where it is undefined (cone apex).
    pub fn normal(&self, q: [f64; 2]) -> Option<V> {
        let [u, v] = q;
        let ax = &self.ax;
        match &self.kind {
            Kind::Plane => Some(ax.z),
            Kind::Cyl(_) => Some(ax.dir(u.cos(), u.sin(), 0.0)),
            Kind::Cone(r, t) => {
                let rho = r + v * t;
                if rho.abs() <= self.tol * 1e-3 {
                    return None;
                }
                unit(mul(ax.dir(u.cos(), u.sin(), -t), rho.signum()))
            }
            Kind::Sphere(_) | Kind::Torus(..) => {
                Some(ax.dir(v.cos() * u.cos(), v.cos() * u.sin(), v.sin()))
            }
            Kind::Spline(_) => {
                let (su, sv) = self.derivs(q);
                unit(cross(su, sv))
            }
        }
    }

    /// Parameters of `p` and, per direction, whether that parameter is undefined there (pole).
    pub fn project(&self, p: V, hint: Option<[f64; 2]>) -> ([f64; 2], [bool; 2]) {
        let l = self.ax.local(p);
        let rho = l[0].hypot(l[1]);
        let sing = rho <= self.tol * 1e-3;
        let u = l[1].atan2(l[0]);
        match &self.kind {
            Kind::Plane => ([l[0], l[1]], [false; 2]),
            Kind::Cyl(_) | Kind::Cone(..) => ([u, l[2]], [sing, false]),
            Kind::Sphere(_) => ([u, l[2].atan2(rho)], [sing, false]),
            Kind::Torus(big, _) => ([u, l[2].atan2(rho - big)], [sing, false]),
            Kind::Spline(_) => self.invert(p, hint),
        }
    }

    fn invert(&self, p: V, hint: Option<[f64; 2]>) -> ([f64; 2], [bool; 2]) {
        let mut best: Option<([f64; 2], f64)> = None;
        if let Some(h) = hint {
            let q = self.newton(p, h);
            best = Some((q, dist(self.eval(q), p)));
        }
        if best.map_or(true, |b| b.1 > self.tol * 0.05) {
            let g = self
                .grid
                .iter()
                .min_by(|a, b| dist(a.1, p).total_cmp(&dist(b.1, p)))
                .map_or([self.dom[0].0, self.dom[1].0], |g| g.0);
            let q = self.newton(p, g);
            let d = dist(self.eval(q), p);
            if best.map_or(true, |b| d < b.1) {
                best = Some((q, d));
            }
        }
        let q = best.map_or([self.dom[0].0, self.dom[1].0], |b| b.0);
        let (su, sv) = self.derivs(q);
        let r = [0, 1].map(|d| self.dom[d].1 - self.dom[d].0);
        (q, [len(su) * r[0] <= self.tol * 1e-2, len(sv) * r[1] <= self.tol * 1e-2])
    }

    fn newton(&self, p: V, q0: [f64; 2]) -> [f64; 2] {
        let mut q = q0;
        let r = [0, 1].map(|d| self.dom[d].1 - self.dom[d].0);
        for _ in 0..30 {
            let e = sub(p, self.eval(q));
            let (su, sv) = self.derivs(q);
            let (a, b, c) = (dot(su, su), dot(su, sv), dot(sv, sv));
            let (f, g) = (dot(su, e), dot(sv, e));
            let det = a * c - b * b;
            if det.abs() <= 1e-12 * (a * c).max(1e-300) {
                // one direction collapses (pole): move along the other one only
                if c > 1e-300 {
                    q[1] = self.wrap(1, q[1] + g / c);
                } else if a > 1e-300 {
                    q[0] = self.wrap(0, q[0] + f / a);
                }
                continue;
            }
            let du = (f * c - g * b) / det;
            let dv = (a * g - b * f) / det;
            q = [self.wrap(0, q[0] + du), self.wrap(1, q[1] + dv)];
            if du.abs() < 1e-13 * r[0] && dv.abs() < 1e-13 * r[1] {
                break;
            }
        }
        q
    }

    /// Parameter of the pole on the `side` (+1 / -1) of `from` in direction `e`.
    pub fn pole(&self, e: usize, side: i32, from: f64) -> Option<f64> {
        match &self.kind {
            Kind::Sphere(_) if e == 1 => Some(if side > 0 { FRAC_PI_2 } else { -FRAC_PI_2 }),
            Kind::Cone(r, t) if e == 1 && *t != 0.0 => {
                let va = -r / t;
                ((side > 0 && va > from) || (side < 0 && va < from)).then_some(va)
            }
            Kind::Spline(_) => {
                let s = usize::from(side > 0);
                self.poles[e][s].then_some(if s == 1 { self.dom[e].1 } else { self.dom[e].0 })
            }
            _ => None,
        }
    }

    /// Whole parameter rectangle of a closed surface.
    pub fn full(&self) -> Option<[[f64; 2]; 2]> {
        match &self.kind {
            Kind::Sphere(_) => Some([[0.0, TAU], [-FRAC_PI_2, FRAC_PI_2]]),
            Kind::Torus(..) => Some([[0.0, TAU], [0.0, TAU]]),
            Kind::Spline(_) => {
                let closed = |d: usize| {
                    let e = 1 - d;
                    self.per[d].is_some() && (self.per[e].is_some() || (self.poles[e][0] && self.poles[e][1]))
                };
                (closed(0) || closed(1)).then(|| {
                    [[self.dom[0].0, self.dom[0].1], [self.dom[1].0, self.dom[1].1]]
                })
            }
            _ => None,
        }
    }
}

fn spline(st: &Store, a: &[P], k: &[P], w: Option<&P>, tol: f64) -> Result<Surf, String> {
    if a.len() < 3 || k.len() < 4 {
        return Err("B-spline surface with missing attributes".into());
    }
    let rows: Vec<Vec<u64>> = a[2].list().unwrap_or(&[]).iter().map(P::ids).collect();
    let (nu, nv) = (rows.len(), rows.first().map_or(0, Vec::len));
    if nu < 2 || nv < 2 || rows.iter().any(|r| r.len() != nv) {
        return Err("B-spline surface with a ragged control net".into());
    }
    let p = rows
        .iter()
        .flatten()
        .map(|&i| point(st, i))
        .collect::<Result<Vec<V>, String>>()?;
    let bu = Basis::new(a[0].num().unwrap_or(0.0), &k[0].nums(), &k[2].nums(), nu)?;
    let bv = Basis::new(a[1].num().unwrap_or(0.0), &k[1].nums(), &k[3].nums(), nv)?;
    let w = match w {
        Some(wp) => {
            let flat: Vec<P> = wp.list().unwrap_or(&[]).iter().flat_map(|r| r.list().unwrap_or(&[]).to_vec()).collect();
            weights(Some(&P::List(flat)), nu * nv)?
        }
        None => None,
    };
    let n = NSurf { bu, bv, nv, p, w };
    let dom = [n.bu.dom(), n.bv.dom()];
    let mut s = Surf {
        ax: Ax::new([0.0; 3], None, None),
        kind: Kind::Plane,
        per: [None, None],
        dom,
        poles: [[false; 2]; 2],
        grid: vec![],
        tol,
    };
    let spans = [n.bu.spans(), n.bv.spans()];
    s.kind = Kind::Spline(Box::new(n));
    let eps = tol * 0.1;
    for d in 0..2 {
        let e = 1 - d;
        let line = |x: f64| -> Vec<V> {
            (0..=6)
                .map(|i| {
                    let mut q = [0.0; 2];
                    q[d] = x;
                    q[e] = dom[e].0 + (dom[e].1 - dom[e].0) * i as f64 / 6.0;
                    s.eval(q)
                })
                .collect()
        };
        let (lo, hi) = (line(dom[d].0), line(dom[d].1));
        let closed = lo.iter().zip(&hi).all(|(a, b)| dist(*a, *b) <= eps);
        let collapsed = |l: &[V]| l.iter().all(|p| dist(*p, l[0]) <= eps);
        // a collapsed side of direction d is a pole of the other direction's parameter
        s.poles[d] = [collapsed(&lo), collapsed(&hi)];
        if closed && !(s.poles[d][0] && s.poles[d][1]) {
            s.per[d] = Some(dom[d].1 - dom[d].0);
        }
    }
    let g = [0, 1].map(|d| (spans[d] * 4).clamp(8, 64));
    for i in 0..=g[0] {
        for j in 0..=g[1] {
            let q = [
                dom[0].0 + (dom[0].1 - dom[0].0) * i as f64 / g[0] as f64,
                dom[1].0 + (dom[1].1 - dom[1].0) * j as f64 / g[1] as f64,
            ];
            let p = s.eval(q);
            s.grid.push((q, p));
        }
    }
    Ok(s)
}
