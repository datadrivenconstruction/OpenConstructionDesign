//! Form finding of nets and gridshells.
//!
//! Force density: with a force density q = N / L per edge the equilibrium of the free nodes is
//! linear, D_ff x_f = p_f - D_fx x_x per coordinate, where D is the weighted graph Laplacian.
//! D_ff is symmetric positive definite when q > 0 and every free node reaches a support; it is
//! solved by the profile LDL^T of the frame solver in reverse Cuthill-McKee order.
//!
//! Dynamic relaxation: explicit pseudo-dynamics with kinetic damping (velocities reset at each
//! peak of kinetic energy) and fictitious masses from the nodal stiffness, for elastic edges and
//! for bending of continuous laths (three-node spline elements: moment EI k from the circle
//! through three nodes, shear pairs at the end nodes) so bending-active gridshells settle.

use ddc_fem::skyline::{rcm, Skyline};

use crate::math::{v3, V3};

#[derive(Clone, Debug, PartialEq)]
pub enum FormFindError {
    /// Inputs of the wrong size or out of range; the text names the item.
    Input(String),
    /// The system has no unique solution; the text names the free node.
    Singular(String),
    /// Dynamic relaxation stopped at the step limit.
    NotConverged { steps: usize, residual: f64 },
}

impl std::fmt::Display for FormFindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormFindError::Input(m) | FormFindError::Singular(m) => f.write_str(m),
            FormFindError::NotConverged { steps, residual } => write!(
                f,
                "form finding did not settle in {steps} steps (largest out-of-balance force {residual:.3e})"
            ),
        }
    }
}

fn err<T>(m: String) -> Result<T, FormFindError> {
    Err(FormFindError::Input(m))
}

fn check_graph(n: usize, edges: &[(u32, u32)], fixed: &[bool], load: &[V3]) -> Result<(), FormFindError> {
    if fixed.len() != n || load.len() != n {
        return err(format!(
            "{n} nodes need {n} support flags and {n} loads (got {} and {})",
            fixed.len(),
            load.len()
        ));
    }
    for (k, &(a, b)) in edges.iter().enumerate() {
        if a as usize >= n || b as usize >= n || a == b {
            return err(format!("edge {k} joins nodes {a} and {b} of {n}"));
        }
    }
    if let Some(i) = load.iter().position(|p| !(p.x.is_finite() && p.y.is_finite() && p.z.is_finite())) {
        return err(format!("load at node {i} is not finite"));
    }
    Ok(())
}

/// Result of a force-density solve.
#[derive(Clone, Debug, PartialEq)]
pub struct Fdm {
    pub pos: Vec<V3>,
    /// Edge forces q L (tension positive).
    pub force: Vec<f64>,
    /// Largest out-of-balance force over the free nodes after the solve.
    pub residual: f64,
}

/// Out-of-balance force at every node for force densities `q` (zero rows at supports).
pub fn fdm_residual(pos: &[V3], edges: &[(u32, u32)], q: &[f64], load: &[V3], fixed: &[bool]) -> Vec<V3> {
    let mut r = load.to_vec();
    for (k, &(a, b)) in edges.iter().enumerate() {
        let d = (pos[b as usize] - pos[a as usize]) * q[k];
        r[a as usize] = r[a as usize] + d;
        r[b as usize] = r[b as usize] - d;
    }
    for (i, f) in fixed.iter().enumerate() {
        if *f {
            r[i] = v3(0.0, 0.0, 0.0);
        }
    }
    r
}

/// Force density method: positions of the free nodes for edge force densities `q` (> 0) and
/// nodal loads; supports keep the positions in `pos`.
pub fn force_density(
    pos: &[V3],
    edges: &[(u32, u32)],
    q: &[f64],
    fixed: &[bool],
    load: &[V3],
) -> Result<Fdm, FormFindError> {
    let n = pos.len();
    check_graph(n, edges, fixed, load)?;
    if q.len() != edges.len() {
        return err(format!("{} edges need {} force densities, got {}", edges.len(), edges.len(), q.len()));
    }
    if let Some(k) = q.iter().position(|v| !(v.is_finite() && *v > 0.0)) {
        return err(format!("force density of edge {k} must be positive, got {}", q[k]));
    }
    // free nodes, then RCM order of their graph
    let free: Vec<usize> = (0..n).filter(|&i| !fixed[i]).collect();
    let mut slot = vec![usize::MAX; n];
    for (k, &i) in free.iter().enumerate() {
        slot[i] = k;
    }
    let mut adj = vec![vec![]; free.len()];
    for &(a, b) in edges {
        let (sa, sb) = (slot[a as usize], slot[b as usize]);
        if sa != usize::MAX && sb != usize::MAX {
            adj[sa].push(sb);
            adj[sb].push(sa);
        }
    }
    for a in adj.iter_mut() {
        a.sort_unstable();
        a.dedup();
    }
    let order = rcm(&adj);
    let mut eq = vec![0usize; free.len()];
    for (k, &s) in order.iter().enumerate() {
        eq[s] = k;
    }
    let m = free.len();
    let mut first: Vec<usize> = (0..m).collect();
    for (s, nb) in adj.iter().enumerate() {
        for &t in nb {
            let (i, j) = (eq[s], eq[t]);
            if j < i {
                first[i] = first[i].min(j);
            }
        }
    }
    let mut sky = Skyline::new(first);
    let mut rhs = vec![[0.0f64; 3]; m];
    for (s, &i) in free.iter().enumerate() {
        let p = load[i];
        rhs[eq[s]] = [p.x, p.y, p.z];
    }
    for (k, &(a, b)) in edges.iter().enumerate() {
        let w = q[k];
        let (sa, sb) = (slot[a as usize], slot[b as usize]);
        for (s, other, so) in [(sa, b as usize, sb), (sb, a as usize, sa)] {
            if s == usize::MAX {
                continue;
            }
            let i = eq[s];
            sky.add(i, i, w);
            if so == usize::MAX {
                let x = pos[other];
                rhs[i][0] += w * x.x;
                rhs[i][1] += w * x.y;
                rhs[i][2] += w * x.z;
            } else if eq[so] < i {
                sky.add(i, eq[so], -w);
            }
        }
    }
    if let Err(i) = sky.factor(1e-12) {
        let s = order[i];
        return Err(FormFindError::Singular(format!(
            "free node {} has no path to a support",
            free[s]
        )));
    }
    let mut out = pos.to_vec();
    for c in 0..3 {
        let mut b: Vec<f64> = rhs.iter().map(|r| r[c]).collect();
        sky.solve(&mut b);
        for (s, &i) in free.iter().enumerate() {
            let x = b[eq[s]];
            match c {
                0 => out[i].x = x,
                1 => out[i].y = x,
                _ => out[i].z = x,
            }
        }
    }
    let force = edges
        .iter()
        .enumerate()
        .map(|(k, &(a, b))| q[k] * (out[b as usize] - out[a as usize]).len())
        .collect();
    let residual = fdm_residual(&out, edges, q, load, fixed)
        .iter()
        .map(|r| r.len())
        .fold(0.0, f64::max);
    Ok(Fdm {
        pos: out,
        force,
        residual,
    })
}

/// Force law of one edge in dynamic relaxation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EdgeLaw {
    /// Tension q L (a force-density edge: the same equilibrium as [`force_density`]).
    Density(f64),
    /// Linear elastic bar: N = EA (L - L0) / L0; compression allowed.
    Elastic { ea: f64, l0: f64 },
}

/// Bending of a continuous lath through nodes a, b, c (straight at rest).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bend {
    pub a: u32,
    pub b: u32,
    pub c: u32,
    pub ei: f64,
}

#[derive(Clone, Debug)]
pub struct Relax<'a> {
    pub edges: &'a [(u32, u32)],
    pub law: &'a [EdgeLaw],
    pub bends: &'a [Bend],
    pub fixed: &'a [bool],
    pub load: &'a [V3],
    /// Stop when the largest out-of-balance force on a free node is below this.
    pub tol: f64,
    pub max_steps: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Relaxed {
    pub pos: Vec<V3>,
    /// Axial edge forces (tension positive).
    pub force: Vec<f64>,
    pub steps: usize,
    pub residual: f64,
}

/// Bending moment of the lath a-b-c at b: EI times the curvature of the circle through them.
pub fn bend_moment(pa: V3, pb: V3, pc: V3, ei: f64) -> f64 {
    let (la, lb) = (pb - pa, pc - pb);
    let s = la.cross(lb).len() / (la.len() * lb.len());
    let chord = (pc - pa).len();
    if !(chord > 1e-15) {
        return 0.0;
    }
    ei * 2.0 * s / chord
}

fn edge_force(law: EdgeLaw, l: f64) -> f64 {
    match law {
        EdgeLaw::Density(q) => q * l,
        EdgeLaw::Elastic { ea, l0 } => ea * (l - l0) / l0,
    }
}

/// Out-of-balance forces and axial edge forces at positions `x`.
fn forces(x: &[V3], s: &Relax, r: &mut [V3], axial: &mut [f64]) {
    for (i, p) in s.load.iter().enumerate() {
        r[i] = *p;
    }
    for (k, &(a, b)) in s.edges.iter().enumerate() {
        let d = x[b as usize] - x[a as usize];
        let l = d.len();
        let n = edge_force(s.law[k], l);
        axial[k] = n;
        if l > 1e-15 {
            let f = d * (n / l);
            r[a as usize] = r[a as usize] + f;
            r[b as usize] = r[b as usize] - f;
        }
    }
    for bd in s.bends {
        let (pa, pb, pc) = (x[bd.a as usize], x[bd.b as usize], x[bd.c as usize]);
        let (la, lb) = (pb - pa, pc - pb);
        let nrm = la.cross(lb);
        let nl = nrm.len();
        if !(nl > 1e-14 * la.len() * lb.len()) {
            continue;
        }
        let m = bend_moment(pa, pb, pc, bd.ei);
        let bn = nrm * (1.0 / nl);
        // shear pairs perpendicular to each segment in the plane of the three nodes, toward
        // straightening; the middle node takes the balance
        let fa = bn.cross(la).normalized() * (-m / la.len());
        let fc = bn.cross(lb).normalized() * (-m / lb.len());
        r[bd.a as usize] = r[bd.a as usize] + fa;
        r[bd.c as usize] = r[bd.c as usize] + fc;
        r[bd.b as usize] = r[bd.b as usize] - fa - fc;
    }
    for (i, f) in s.fixed.iter().enumerate() {
        if *f {
            r[i] = v3(0.0, 0.0, 0.0);
        }
    }
}

/// Fictitious masses from the nodal stiffness (Barnes): axial EA/L0 plus |N|/L, q for density
/// edges, 4 EI / (la lb min) for the three nodes of a bend; doubled for margin with dt = 1.
fn masses(x: &[V3], s: &Relax, axial: &[f64]) -> Vec<f64> {
    let mut m = vec![0.0; x.len()];
    for (k, &(a, b)) in s.edges.iter().enumerate() {
        let l = (x[b as usize] - x[a as usize]).len().max(1e-12);
        let k_ax = match s.law[k] {
            EdgeLaw::Density(q) => q,
            EdgeLaw::Elastic { ea, l0 } => ea / l0 + axial[k].abs() / l,
        };
        m[a as usize] += k_ax;
        m[b as usize] += k_ax;
    }
    for bd in s.bends {
        let la = (x[bd.b as usize] - x[bd.a as usize]).len().max(1e-12);
        let lb = (x[bd.c as usize] - x[bd.b as usize]).len().max(1e-12);
        let kb = 4.0 * bd.ei / (la * lb * la.min(lb));
        for i in [bd.a, bd.b, bd.c] {
            m[i as usize] += kb * if i == bd.b { 2.0 } else { 1.0 };
        }
    }
    for v in m.iter_mut() {
        *v = 2.0 * v.max(1e-12);
    }
    m
}

/// Dynamic relaxation from `start` with kinetic damping.
pub fn relax(start: &[V3], s: &Relax) -> Result<Relaxed, FormFindError> {
    let n = start.len();
    check_graph(n, s.edges, s.fixed, s.load)?;
    if s.law.len() != s.edges.len() {
        return err(format!("{} edges need {} laws, got {}", s.edges.len(), s.edges.len(), s.law.len()));
    }
    for (k, l) in s.law.iter().enumerate() {
        let ok = match *l {
            EdgeLaw::Density(q) => q.is_finite() && q > 0.0,
            EdgeLaw::Elastic { ea, l0 } => ea.is_finite() && ea > 0.0 && l0.is_finite() && l0 > 0.0,
        };
        if !ok {
            return err(format!("edge {k} needs a positive force density or EA and rest length"));
        }
    }
    for (k, b) in s.bends.iter().enumerate() {
        let ids = [b.a, b.b, b.c];
        if ids.iter().any(|&i| i as usize >= n) || b.a == b.b || b.b == b.c || b.a == b.c {
            return err(format!("bend {k} needs three distinct nodes"));
        }
        if !(b.ei.is_finite() && b.ei > 0.0) {
            return err(format!("bend {k} needs a positive EI"));
        }
    }
    if !(s.tol > 0.0) {
        return err("tolerance must be positive".into());
    }
    let mut x = start.to_vec();
    let mut v = vec![v3(0.0, 0.0, 0.0); n];
    let mut r = vec![v3(0.0, 0.0, 0.0); n];
    let mut axial = vec![0.0; s.edges.len()];
    forces(&x, s, &mut r, &mut axial);
    let mut m = masses(&x, s, &axial);
    let mut ke_prev = 0.0;
    let resid = |r: &[V3]| r.iter().map(|f| f.len()).fold(0.0, f64::max);
    for step in 0..s.max_steps {
        let res = resid(&r);
        if res < s.tol {
            return Ok(Relaxed {
                pos: x,
                force: axial,
                steps: step,
                residual: res,
            });
        }
        let mut ke = 0.0;
        for i in 0..n {
            if s.fixed[i] {
                continue;
            }
            v[i] = v[i] + r[i] * (1.0 / m[i]);
            ke += 0.5 * m[i] * v[i].dot(v[i]);
        }
        if ke < ke_prev {
            // kinetic energy peak passed: step back half a move, stop and restart from rest
            for i in 0..n {
                x[i] = x[i] - v[i] * 0.5;
                v[i] = v3(0.0, 0.0, 0.0);
            }
            ke_prev = 0.0;
            forces(&x, s, &mut r, &mut axial);
            m = masses(&x, s, &axial);
            continue;
        }
        ke_prev = ke;
        for i in 0..n {
            x[i] = x[i] + v[i];
        }
        forces(&x, s, &mut r, &mut axial);
    }
    Err(FormFindError::NotConverged {
        steps: s.max_steps,
        residual: resid(&r),
    })
}

#[cfg(test)]
#[path = "formfind_tests.rs"]
mod tests;
