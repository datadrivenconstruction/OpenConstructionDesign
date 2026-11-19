//! Assembly, one factorization for all load cases, recovery, diagrams, combinations, envelopes.
//! Equations are numbered node by node in RCM order, then DOF 0..6; fixed DOFs are dropped.

use crate::element::{self as el, MLoad, Props, M12, V12, V3, G4};
use crate::skyline::{rcm, Skyline};
use crate::{Axes, FemError, Frame, Load, LoadCase, Options, Restraint};

/// Internal forces along a member: `f[k]` = [N, Vy, Vz, T, My, Mz] at station `x[k]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagram {
    pub x: Vec<f64>,
    pub f: Vec<[f64; 6]>,
}

/// Resultants about the global origin: [Fx, Fy, Fz, Mx, My, Mz]; residual = load + reaction.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Equilibrium {
    pub load: [f64; 6],
    pub reaction: [f64; 6],
    pub residual: [f64; 6],
    /// max(|force residual| / force scale, |moment residual| / moment scale).
    pub relative: f64,
}

/// Largest translation (norm) and largest bending moment sqrt(My^2 + Mz^2) over stations.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Summary {
    pub max_disp: f64,
    pub max_disp_node: usize,
    pub max_moment: f64,
    pub max_moment_member: usize,
    pub max_moment_x: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CaseResult {
    pub name: String,
    /// Per node, global: [ux, uy, uz, rx, ry, rz].
    pub disp: Vec<[f64; 6]>,
    /// Supported nodes in ascending order, global; free DOFs report 0.
    pub reactions: Vec<(usize, [f64; 6])>,
    /// Per member [end i, end j], local axes at the flexible ends.
    pub end_forces: Vec<[[f64; 6]; 2]>,
    pub diagrams: Vec<Diagram>,
    pub equilibrium: Equilibrium,
    pub summary: Summary,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnvelopeResult {
    pub name: String,
    pub max: CaseResult,
    pub min: CaseResult,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Solution {
    pub cases: Vec<CaseResult>,
    pub combos: Vec<CaseResult>,
    pub envelopes: Vec<EnvelopeResult>,
    /// Free equations and stored profile entries of the factor.
    pub equations: usize,
    pub profile: usize,
}

const NONE: usize = usize::MAX;

struct Elem {
    l: f64,
    rot: [V3; 3],
    t: M12,
    k: M12,
    kfull: M12,
    rel: [bool; 12],
    props: Props,
    start: V3,
    nodes: [usize; 2],
    rho_a: f64,
}

impl Elem {
    fn to_local(&self, v: V3) -> V3 {
        [el::dot3(self.rot[0], v), el::dot3(self.rot[1], v), el::dot3(self.rot[2], v)]
    }
    fn to_global(&self, v: V3) -> V3 {
        let r = &self.rot;
        let mut o = [0.0; 3];
        for (c, oc) in o.iter_mut().enumerate() {
            *oc = r[0][c] * v[0] + r[1][c] * v[1] + r[2][c] * v[2];
        }
        o
    }
    fn gdof(&self, k: usize) -> usize {
        6 * self.nodes[k / 6] + k % 6
    }
}

fn bad(s: String) -> FemError {
    FemError::Input(s)
}

fn finite(v: &[f64]) -> bool {
    v.iter().all(|x| x.is_finite())
}

fn validate(fr: &Frame, opt: &Options) -> Result<(), FemError> {
    let nn = fr.nodes.len();
    if opt.stations < 2 {
        return Err(bad("stations must be at least 2".into()));
    }
    for (i, n) in fr.nodes.iter().enumerate() {
        if !finite(n) {
            return Err(bad(format!("node {i} has a non-finite coordinate")));
        }
    }
    for (i, m) in fr.materials.iter().enumerate() {
        if !(m.e > 0.0 && m.g > 0.0 && m.density >= 0.0) || !finite(&[m.e, m.g, m.density]) {
            return Err(bad(format!("material {i} needs E > 0, G > 0, density >= 0")));
        }
    }
    for (i, s) in fr.sections.iter().enumerate() {
        let av = [s.avy, s.avz];
        let av_ok = av.iter().all(|v| v.is_none_or(|x| x > 0.0 && x.is_finite()));
        let pos = [s.a, s.iy, s.iz, s.j].iter().all(|&x| x > 0.0 && x.is_finite());
        if !pos || !av_ok {
            return Err(bad(format!("section {i} needs A, Iy, Iz, J (and shear areas) > 0")));
        }
    }
    for (i, m) in fr.members.iter().enumerate() {
        if m.i >= nn || m.j >= nn || m.i == m.j {
            return Err(bad(format!("member {i} has invalid end nodes {} and {}", m.i, m.j)));
        }
        if m.material >= fr.materials.len() || m.section >= fr.sections.len() {
            return Err(bad(format!("member {i} refers to a missing material or section")));
        }
        if !finite(&m.offset[0]) || !finite(&m.offset[1]) {
            return Err(bad(format!("member {i} has a non-finite offset")));
        }
    }
    for s in &fr.supports {
        if s.node >= nn {
            return Err(bad(format!("support at missing node {}", s.node)));
        }
        for r in s.dof {
            if let Restraint::Spring(k) = r {
                if !(k >= 0.0 && k.is_finite()) {
                    return Err(bad(format!("support at node {} has a bad spring", s.node)));
                }
            }
        }
    }
    let nc = fr.cases.len();
    for c in &fr.combos {
        if c.factors.iter().any(|&(i, f)| i >= nc || !f.is_finite()) {
            return Err(bad(format!("combination {} refers to a missing case", c.name)));
        }
    }
    for e in &fr.envelopes {
        let ok = e.cases.iter().all(|&i| i < nc) && e.combos.iter().all(|&i| i < fr.combos.len());
        if !ok || (e.cases.is_empty() && e.combos.is_empty()) {
            return Err(bad(format!("envelope {} is empty or refers to a missing item", e.name)));
        }
    }
    Ok(())
}

fn restraints(fr: &Frame) -> Vec<[Restraint; 6]> {
    let mut r = vec![[Restraint::Free; 6]; fr.nodes.len()];
    for s in &fr.supports {
        for (d, &new) in s.dof.iter().enumerate() {
            let cur = r[s.node][d];
            r[s.node][d] = match (cur, new) {
                (Restraint::Fixed, _) | (_, Restraint::Fixed) => Restraint::Fixed,
                (Restraint::Spring(a), Restraint::Spring(b)) => Restraint::Spring(a + b),
                (Restraint::Free, x) | (x, Restraint::Free) => x,
            };
        }
    }
    r
}

fn build(fr: &Frame, opt: &Options) -> Result<Vec<Elem>, FemError> {
    let mut out = Vec::with_capacity(fr.members.len());
    for (mi, m) in fr.members.iter().enumerate() {
        let mat = fr.materials[m.material];
        let sec = fr.sections[m.section];
        let start = el::add3(fr.nodes[m.i], m.offset[0]);
        let end = el::add3(fr.nodes[m.j], m.offset[1]);
        let d = el::sub3(end, start);
        let l = el::norm3(d);
        if l.partial_cmp(&1e-9) != Some(std::cmp::Ordering::Greater) {
            return Err(bad(format!("member {mi} has a zero flexible length")));
        }
        let x = el::scale3(d, 1.0 / l);
        let rot = el::axes(x, m.orient).map_err(|s| bad(format!("member {mi}: {s}")))?;
        let phi = |i: f64, av: Option<f64>| match (opt.shear, av) {
            (true, Some(av)) => 12.0 * mat.e * i / (mat.g * av * l * l),
            _ => 0.0,
        };
        let props = Props {
            ea: mat.e * sec.a,
            gj: mat.g * sec.j,
            eiy: mat.e * sec.iy,
            eiz: mat.e * sec.iz,
            phy: phi(sec.iz, sec.avy),
            phz: phi(sec.iy, sec.avz),
        };
        let kfull = el::k_local(l, &props);
        let mut rel = [false; 12];
        for e in 0..2 {
            for dd in 0..6 {
                rel[6 * e + dd] = m.release[e][dd];
            }
        }
        let mut k = kfull;
        let mut zero = [0.0; 12];
        el::condense(&mut k, &mut zero, &rel)
            .map_err(|dof| FemError::Release { member: mi, dof })?;
        out.push(Elem {
            l,
            rot,
            t: el::transform(&rot, &m.offset),
            k,
            kfull,
            rel,
            props,
            start,
            nodes: [m.i, m.j],
            rho_a: mat.density * sec.a,
        });
    }
    Ok(out)
}

fn member_loads(fr: &Frame, els: &[Elem], case: &LoadCase) -> Result<Vec<Vec<MLoad>>, FemError> {
    let mut out = vec![Vec::new(); els.len()];
    let nm = els.len();
    let miss = |m: usize| bad(format!("case {}: load on missing member {m}", case.name));
    for ld in &case.loads {
        match ld {
            Load::Node { node, f } => {
                if *node >= fr.nodes.len() || !finite(f) {
                    return Err(bad(format!("case {}: bad load at node {node}", case.name)));
                }
            }
            Load::Point { member, a, f, m, axes } => {
                let e = els.get(*member).ok_or_else(|| miss(*member))?;
                let tol = 1e-9 * e.l;
                if !(*a >= -tol && *a <= e.l + tol) || !finite(f) || !finite(m) {
                    let s = format!("case {}: point load outside member {member}", case.name);
                    return Err(bad(s));
                }
                let (f, m) = match axes {
                    Axes::Local => (*f, *m),
                    Axes::Global => (e.to_local(*f), e.to_local(*m)),
                };
                out[*member].push(MLoad::Point { a: a.clamp(0.0, e.l), f, m });
            }
            Load::Dist { member, a, b, w1, w2, axes } => {
                let e = els.get(*member).ok_or_else(|| miss(*member))?;
                let b = b.min(e.l);
                if !(*a >= 0.0 && b > *a) || !finite(w1) || !finite(w2) {
                    let s = format!("case {}: bad line load range on member {member}", case.name);
                    return Err(bad(s));
                }
                let (w1, w2) = match axes {
                    Axes::Local => (*w1, *w2),
                    Axes::Global => (e.to_local(*w1), e.to_local(*w2)),
                };
                out[*member].push(MLoad::Dist { a: *a, b, w1, w2 });
            }
            Load::SelfWeight { dir, g } => {
                if !finite(dir) || !g.is_finite() {
                    return Err(bad(format!("case {}: bad self-weight", case.name)));
                }
                for (mi, e) in els.iter().enumerate().take(nm) {
                    let w = e.to_local(el::scale3(*dir, e.rho_a * g));
                    out[mi].push(MLoad::Dist { a: 0.0, b: e.l, w1: w, w2: w });
                }
            }
        }
    }
    Ok(out)
}

/// Diagram stations: k uniform points plus every point-load position of any case.
fn stations(els: &[Elem], all: &[Vec<Vec<MLoad>>], k: usize) -> Vec<Vec<f64>> {
    els.iter()
        .enumerate()
        .map(|(mi, e)| {
            let mut xs: Vec<f64> = (0..k).map(|i| e.l * i as f64 / (k - 1) as f64).collect();
            xs[k - 1] = e.l;
            for case in all {
                for ld in &case[mi] {
                    if let MLoad::Point { a, .. } = *ld {
                        xs.push(a);
                    }
                }
            }
            xs.sort_by(|a, b| a.total_cmp(b));
            xs.dedup_by(|a, b| (*a - *b).abs() <= 1e-9 * e.l);
            xs
        })
        .collect()
}

/// Internal forces by equilibrium of the part [0, x]: end force at i plus loads before x.
fn diagram(xs: &[f64], fi: &[f64; 6], loads: &[MLoad]) -> Vec<[f64; 6]> {
    xs.iter()
        .map(|&x| {
            let mut s = *fi;
            s[4] += x * fi[2];
            s[5] -= x * fi[1];
            for ld in loads {
                match *ld {
                    MLoad::Point { a, f, m } => {
                        if a <= x {
                            let r = a - x;
                            for k in 0..3 {
                                s[k] += f[k];
                                s[3 + k] += m[k];
                            }
                            s[4] -= r * f[2];
                            s[5] += r * f[1];
                        }
                    }
                    MLoad::Dist { a, b, w1, w2 } => {
                        let c = b.min(x);
                        if c > a {
                            for (g, wt) in G4 {
                                let sp = a + (c - a) * g;
                                let q = el::lerp3(w1, w2, (sp - a) / (b - a));
                                let w = (c - a) * wt;
                                let r = sp - x;
                                for k in 0..3 {
                                    s[k] += w * q[k];
                                }
                                s[4] -= w * r * q[2];
                                s[5] += w * r * q[1];
                            }
                        }
                    }
                }
            }
            s.map(|v| -v)
        })
        .collect()
}

fn push_resultant(acc: &mut [f64; 6], p: V3, f: V3, m: V3) {
    let pm = el::cross3(p, f);
    for k in 0..3 {
        acc[k] += f[k];
        acc[3 + k] += m[k] + pm[k];
    }
}

/// Applied load resultant about the origin, straight from the load definitions.
fn applied(fr: &Frame, els: &[Elem], case: &LoadCase, ml: &[Vec<MLoad>]) -> [f64; 6] {
    let mut acc = [0.0; 6];
    for ld in &case.loads {
        if let Load::Node { node, f } = ld {
            push_resultant(&mut acc, fr.nodes[*node], [f[0], f[1], f[2]], [f[3], f[4], f[5]]);
        }
    }
    for (e, loads) in els.iter().zip(ml) {
        let at = |s: f64| el::add3(e.start, el::scale3(e.rot[0], s));
        for ld in loads {
            match *ld {
                MLoad::Point { a, f, m } => {
                    push_resultant(&mut acc, at(a), e.to_global(f), e.to_global(m))
                }
                MLoad::Dist { a, b, w1, w2 } => {
                    let len = b - a;
                    let uni = e.to_global(el::scale3(w1, len));
                    let tri = e.to_global(el::scale3(el::sub3(w2, w1), 0.5 * len));
                    push_resultant(&mut acc, at(a + 0.5 * len), uni, [0.0; 3]);
                    push_resultant(&mut acc, at(a + 2.0 * len / 3.0), tri, [0.0; 3]);
                }
            }
        }
    }
    acc
}

fn n3(v: &[f64]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

impl CaseResult {
    /// Recomputes the equilibrium residual and the summary from the stored values.
    fn finish(&mut self) {
        let q = &mut self.equilibrium;
        for k in 0..6 {
            q.residual[k] = q.load[k] + q.reaction[k];
        }
        let rel = |r: &[f64], a: &[f64], b: &[f64]| {
            let s = n3(a).max(n3(b));
            if s > 0.0 {
                n3(r) / s
            } else {
                n3(r)
            }
        };
        let rf = rel(&q.residual[..3], &q.load[..3], &q.reaction[..3]);
        let rm = rel(&q.residual[3..], &q.load[3..], &q.reaction[3..]);
        q.relative = rf.max(rm);
        let mut s = Summary::default();
        for (n, d) in self.disp.iter().enumerate() {
            let v = n3(d);
            if v > s.max_disp {
                s.max_disp = v;
                s.max_disp_node = n;
            }
        }
        for (m, dg) in self.diagrams.iter().enumerate() {
            for (x, f) in dg.x.iter().zip(&dg.f) {
                let v = f[4].hypot(f[5]);
                if v > s.max_moment {
                    s.max_moment = v;
                    s.max_moment_member = m;
                    s.max_moment_x = *x;
                }
            }
        }
        self.summary = s;
    }

    /// Value-wise f(self, other) over every result array (same frame, same stations).
    fn map2(&self, o: &CaseResult, name: &str, f: &dyn Fn(f64, f64) -> f64) -> CaseResult {
        let a6 = |x: &[f64; 6], y: &[f64; 6]| {
            let mut r = [0.0; 6];
            for k in 0..6 {
                r[k] = f(x[k], y[k]);
            }
            r
        };
        let mut out = CaseResult {
            name: name.to_string(),
            disp: self.disp.iter().zip(&o.disp).map(|(x, y)| a6(x, y)).collect(),
            reactions: self
                .reactions
                .iter()
                .zip(&o.reactions)
                .map(|((n, x), (_, y))| (*n, a6(x, y)))
                .collect(),
            end_forces: self
                .end_forces
                .iter()
                .zip(&o.end_forces)
                .map(|(x, y)| [a6(&x[0], &y[0]), a6(&x[1], &y[1])])
                .collect(),
            diagrams: self
                .diagrams
                .iter()
                .zip(&o.diagrams)
                .map(|(x, y)| Diagram {
                    x: x.x.clone(),
                    f: x.f.iter().zip(&y.f).map(|(p, q)| a6(p, q)).collect(),
                })
                .collect(),
            equilibrium: Equilibrium {
                load: a6(&self.equilibrium.load, &o.equilibrium.load),
                reaction: a6(&self.equilibrium.reaction, &o.equilibrium.reaction),
                ..Equilibrium::default()
            },
            summary: Summary::default(),
        };
        out.finish();
        out
    }
}

/// Solves every load case with one factorization, then combinations and envelopes.
pub fn solve(fr: &Frame, opt: &Options) -> Result<Solution, FemError> {
    validate(fr, opt)?;
    let nn = fr.nodes.len();
    let restr = restraints(fr);
    let els = build(fr, opt)?;

    let mut adj = vec![Vec::new(); nn];
    for m in &fr.members {
        adj[m.i].push(m.j);
        adj[m.j].push(m.i);
    }
    for a in adj.iter_mut() {
        a.sort_unstable();
        a.dedup();
    }
    let mut eq = vec![NONE; 6 * nn];
    let mut owner = Vec::new();
    for n in rcm(&adj) {
        for d in 0..6 {
            if !matches!(restr[n][d], Restraint::Fixed) {
                eq[6 * n + d] = owner.len();
                owner.push((n, d));
            }
        }
    }
    let neq = owner.len();
    let mut first: Vec<usize> = (0..neq).collect();
    for e in &els {
        let qs: Vec<usize> = (0..12).map(|k| eq[e.gdof(k)]).filter(|&q| q != NONE).collect();
        if let Some(&mn) = qs.iter().min() {
            for &q in &qs {
                first[q] = first[q].min(mn);
            }
        }
    }
    let mut sky = Skyline::new(first);
    for e in &els {
        let kg = el::tkt(&e.t, &e.k);
        for a in 0..12 {
            let qa = eq[e.gdof(a)];
            if qa == NONE {
                continue;
            }
            for b in 0..12 {
                let qb = eq[e.gdof(b)];
                if qb != NONE && qb <= qa && kg[a][b] != 0.0 {
                    sky.add(qa, qb, kg[a][b]);
                }
            }
        }
    }
    for (n, r) in restr.iter().enumerate() {
        for (d, x) in r.iter().enumerate() {
            if let Restraint::Spring(k) = *x {
                sky.add(eq[6 * n + d], eq[6 * n + d], k);
            }
        }
    }
    sky.factor(opt.pivot_tol).map_err(|q| {
        let (node, dof) = owner[q];
        FemError::Mechanism { node, dof }
    })?;

    let all: Vec<Vec<Vec<MLoad>>> =
        fr.cases.iter().map(|c| member_loads(fr, &els, c)).collect::<Result<_, _>>()?;
    let xs = stations(&els, &all, opt.stations);
    let supported: Vec<usize> =
        (0..nn).filter(|&n| restr[n].iter().any(|r| !matches!(r, Restraint::Free))).collect();

    let mut cases = Vec::with_capacity(fr.cases.len());
    for (case, ml) in fr.cases.iter().zip(&all) {
        let mut pn = vec![[0.0; 6]; nn];
        for ld in &case.loads {
            if let Load::Node { node, f } = ld {
                for k in 0..6 {
                    pn[*node][k] += f[k];
                }
            }
        }
        let mut fstar: Vec<V12> = Vec::with_capacity(els.len());
        for (mi, (e, loads)) in els.iter().zip(ml).enumerate() {
            if loads.is_empty() {
                fstar.push([0.0; 12]);
                continue;
            }
            let mut k = e.kfull;
            let mut f = el::equiv(e.l, &e.props, loads);
            el::condense(&mut k, &mut f, &e.rel)
                .map_err(|dof| FemError::Release { member: mi, dof })?;
            fstar.push(f);
        }
        let mut rhs = vec![0.0; neq];
        for (n, p) in pn.iter().enumerate() {
            for d in 0..6 {
                if eq[6 * n + d] != NONE {
                    rhs[eq[6 * n + d]] += p[d];
                }
            }
        }
        for (e, f) in els.iter().zip(&fstar) {
            let fg = el::mul_t(&e.t, f);
            for k in 0..12 {
                let q = eq[e.gdof(k)];
                if q != NONE {
                    rhs[q] += fg[k];
                }
            }
        }
        sky.solve(&mut rhs);
        let mut disp = vec![[0.0; 6]; nn];
        for (n, dn) in disp.iter_mut().enumerate() {
            for d in 0..6 {
                if eq[6 * n + d] != NONE {
                    dn[d] = rhs[eq[6 * n + d]];
                }
            }
        }
        let mut msum = vec![[0.0; 6]; nn];
        let mut end_forces = Vec::with_capacity(els.len());
        let mut diagrams = Vec::with_capacity(els.len());
        for (mi, (e, f)) in els.iter().zip(&fstar).enumerate() {
            let mut ug = [0.0; 12];
            for (k, u) in ug.iter_mut().enumerate() {
                *u = disp[e.nodes[k / 6]][k % 6];
            }
            let ul = el::mul(&e.t, &ug);
            let mut fl = el::mul(&e.k, &ul);
            for k in 0..12 {
                fl[k] -= f[k];
                if e.rel[k] {
                    fl[k] = 0.0;
                }
            }
            let fg = el::mul_t(&e.t, &fl);
            for k in 0..12 {
                msum[e.nodes[k / 6]][k % 6] += fg[k];
            }
            let fi = [fl[0], fl[1], fl[2], fl[3], fl[4], fl[5]];
            let fj = [fl[6], fl[7], fl[8], fl[9], fl[10], fl[11]];
            diagrams.push(Diagram { x: xs[mi].clone(), f: diagram(&xs[mi], &fi, &ml[mi]) });
            end_forces.push([fi, fj]);
        }
        let mut reactions = Vec::with_capacity(supported.len());
        let mut rsum = [0.0; 6];
        for &n in &supported {
            let mut r = [0.0; 6];
            for d in 0..6 {
                r[d] = match restr[n][d] {
                    Restraint::Fixed => msum[n][d] - pn[n][d],
                    Restraint::Spring(k) => -k * disp[n][d],
                    Restraint::Free => 0.0,
                };
            }
            push_resultant(&mut rsum, fr.nodes[n], [r[0], r[1], r[2]], [r[3], r[4], r[5]]);
            reactions.push((n, r));
        }
        let mut res = CaseResult {
            name: case.name.clone(),
            disp,
            reactions,
            end_forces,
            diagrams,
            equilibrium: Equilibrium {
                load: applied(fr, &els, case, ml),
                reaction: rsum,
                ..Equilibrium::default()
            },
            summary: Summary::default(),
        };
        res.finish();
        cases.push(res);
    }

    let mut combos = Vec::with_capacity(fr.combos.len());
    for c in &fr.combos {
        let mut acc: Option<CaseResult> = None;
        for &(ci, fac) in &c.factors {
            let base = &cases[ci];
            acc = Some(match acc {
                None => base.map2(base, &c.name, &|x, _| fac * x),
                Some(a) => a.map2(base, &c.name, &|x, y| x + fac * y),
            });
        }
        let r = match acc {
            Some(r) => r,
            // An empty combination is the zero result.
            None => match cases.first() {
                Some(b) => b.map2(b, &c.name, &|_, _| 0.0),
                None => return Err(bad(format!("combination {} has no cases", c.name))),
            },
        };
        combos.push(r);
    }

    let mut envelopes = Vec::with_capacity(fr.envelopes.len());
    for env in &fr.envelopes {
        let items: Vec<&CaseResult> = env
            .cases
            .iter()
            .map(|&i| &cases[i])
            .chain(env.combos.iter().map(|&i| &combos[i]))
            .collect();
        let mut max = items[0].map2(items[0], &env.name, &|x, _| x);
        let mut min = max.clone();
        for it in &items[1..] {
            max = max.map2(it, &env.name, &f64::max);
            min = min.map2(it, &env.name, &f64::min);
        }
        envelopes.push(EnvelopeResult { name: env.name.clone(), max, min });
    }

    Ok(Solution { cases, combos, envelopes, equations: neq, profile: sky.profile() })
}
