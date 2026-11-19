// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Preliminary member check of a hierarchical lattice. NOT a design calculation: the tower
//! lattice is solved as a rigid-jointed frame (latticed members as their chord groups) under
//! self weight, a floor load on the platform beams and a uniform wind pressure; chord forces
//! come from axial force and bending shared over the chords, and the capacity is an allowable
//! stress reduced for slenderness by the Rankine-Gordon form. It flags members to look at.

use super::hier::Hier;
use super::lattice::{Frame, MemRole};
use super::sections;
use crate::member::Shape;
use ddc_fem as fem;

/// Material constants of wrought iron and structural steel are close; one modulus is used.
/// Wind directions of the check, degrees from +x; the governing one is reported per member.
pub const WIND_DEG: [f64; 3] = [0.0, 45.0, 90.0];

const E_MOD: f64 = super::sections::E_STEEL;
const G_MOD: f64 = 77e9;
const DENSITY: f64 = 7850.0;
const GRAVITY: f64 = 9.81;

/// The numbers of the check (rules `steel.check.*`).
#[derive(Clone, Debug, PartialEq)]
pub struct CheckRules {
    /// Allowable stress, Pa.
    pub f_allow: f64,
    /// Rankine-Gordon constant a in f / (1 + a·λ²).
    pub rankine_a: f64,
    /// Wind pressure on the solid area, Pa.
    pub wind_q: f64,
    /// Share of a lattice's outline that is solid steel facing the wind.
    pub solidity: f64,
    /// Floor load on the platforms, Pa.
    pub live_q: f64,
}

impl Default for CheckRules {
    fn default() -> Self {
        CheckRules { f_allow: 100e6, rankine_a: 1.0 / 9000.0, wind_q: 1500.0, solidity: 0.3, live_q: 2000.0 }
    }
}

/// Exact section constants from the outline (outline x = member local y, outline y = local z,
/// the depth): area, second moments about local y and z through the centroid, the smallest
/// principal one, and the extreme fibre distances along local z and y.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Props {
    pub a: f64,
    pub iy: f64,
    pub iz: f64,
    pub imin: f64,
    pub cz: f64,
    pub cy: f64,
}

impl Props {
    /// Section moduli about local y and z.
    pub fn w(&self) -> (f64, f64) {
        (self.iy / self.cz.max(1e-9), self.iz / self.cy.max(1e-9))
    }
}

pub fn props(s: &Shape) -> Props {
    let (o, h) = s.rings();
    let (mut a, mut sx, mut sy, mut ixx, mut iyy, mut ixy) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for ring in std::iter::once(&o).chain(h.iter()) {
        let n = ring.len();
        for i in 0..n {
            let (p, q) = (ring[i], ring[(i + 1) % n]);
            let c = p.x * q.y - q.x * p.y;
            a += c / 2.0;
            sx += (p.x + q.x) * c / 6.0;
            sy += (p.y + q.y) * c / 6.0;
            ixx += (p.y * p.y + p.y * q.y + q.y * q.y) * c / 12.0;
            iyy += (p.x * p.x + p.x * q.x + q.x * q.x) * c / 12.0;
            ixy += (p.x * q.y + 2.0 * p.x * p.y + 2.0 * q.x * q.y + q.x * p.y) * c / 24.0;
        }
    }
    let (cx, cy) = (sx / a, sy / a);
    // ixx = ∫ (outline y)², bending about local y
    let (iy, iz, iyz) = (ixx - a * cy * cy, iyy - a * cx * cx, ixy - a * cx * cy);
    let imin = (iy + iz) / 2.0 - (((iy - iz) / 2.0).powi(2) + iyz * iyz).sqrt();
    let cz = o.iter().map(|p| (p.y - cy).abs()).fold(0.0, f64::max);
    let cyy = o.iter().map(|p| (p.x - cx).abs()).fold(0.0, f64::max);
    Props { a, iy, iz, imin, cz, cy: cyy }
}

/// Allowable compression force of area `a` with radius of gyration `r` over buckling length `l`.
pub fn rankine(a: f64, r: f64, l: f64, c: &CheckRules) -> f64 {
    let lam = l / r.max(1e-9);
    a * c.f_allow / (1.0 + c.rankine_a * lam * lam)
}

fn shape_props(name: &str) -> Result<Props, String> {
    sections::find(name).map(|s| props(&s)).ok_or_else(|| format!("no section '{name}'"))
}

/// Axial force of every member of a frame of rolled sections (tension positive), rigid joints,
/// under nodal forces; for hand-checkable trusses and frames.
pub fn axial(f: &Frame, loads: &[(u32, [f64; 3])], supports: &[fem::Support]) -> Result<Vec<f64>, String> {
    let mut fr = fem::Frame { nodes: f.nodes.iter().map(|p| [p.x, p.y, p.z]).collect(), ..Default::default() };
    fr.materials.push(fem::Material { e: E_MOD, g: G_MOD, density: DENSITY });
    for s in &f.sections {
        let p = shape_props(s)?;
        fr.sections.push(fem::Section::new(p.a, p.iy, p.iz, (p.iy + p.iz) * 0.01));
    }
    for m in &f.members {
        fr.members.push(fem::Member::new(m.a as usize, m.b as usize, 0, m.sec as usize));
    }
    fr.supports = supports.to_vec();
    fr.cases.push(fem::LoadCase { name: "P".into(), loads: loads.iter().map(|(n, v)| fem::Load::Node { node: *n as usize, f: [v[0], v[1], v[2], 0.0, 0.0, 0.0] }).collect() });
    let s = fem::solve(&fr, &fem::Options::default()).map_err(|e| e.to_string())?;
    Ok(s.cases[0].diagrams.iter().map(|d| d.f[0][0]).collect())
}

/// One member's preliminary use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Use {
    pub member: u32,
    /// Compression in the worst chord (or the member), N.
    pub demand: f64,
    pub capacity: f64,
    pub ratio: f64,
    /// Governing slenderness.
    pub slender: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CheckReport {
    pub uses: Vec<Use>,
    pub max: f64,
    pub max_member: u32,
    pub over: usize,
    /// Largest horizontal displacement under wind, m.
    pub sway: f64,
    /// Relative equilibrium residual of the solve.
    pub residual: f64,
    pub equations: usize,
    pub preliminary: bool,
}

/// Solves the tower lattice and checks every member's worst chord. Labelled preliminary.
pub fn check(h: &Hier, c: &CheckRules) -> Result<CheckReport, String> {
    let f = &h.skeleton;
    let root: std::collections::HashMap<u32, usize> = h.roots.iter().enumerate().map(|(i, r)| (r.member, i)).collect();
    let mut fr = fem::Frame { nodes: f.nodes.iter().map(|p| [p.x, p.y, p.z]).collect(), ..Default::default() };
    fr.materials.push(fem::Material { e: E_MOD, g: G_MOD, density: DENSITY });
    let mut node_g = vec![[0.0f64; 3]; f.nodes.len()];
    let dirs: Vec<(f64, f64)> = WIND_DEG.iter().map(|a| (a.to_radians().cos(), a.to_radians().sin())).collect();
    let mut node_w = vec![vec![[0.0f64; 3]; f.nodes.len()]; dirs.len()];
    let mut live = vec![];
    let sec_props: Vec<Result<Props, String>> = f.sections.iter().map(|s| shape_props(s)).collect();
    for (i, m) in f.members.iter().enumerate() {
        let (a, b) = (f.nodes[m.a as usize], f.nodes[m.b as usize]);
        let l = (b - a).len();
        let u = (b - a) * (1.0 / l);
        let (sec, mass, face, orient) = match root.get(&(i as u32)) {
            Some(&ri) => {
                let r = &h.roots[ri];
                let p = &h.protos[r.proto as usize];
                let i2 = p.chord_area * (p.axis_w / 2.0).powi(2);
                let sec = fem::Section::new(p.chord_area, i2, i2, 0.2 * i2);
                (sec, p.mass, c.solidity * p.width, fem::Orient::Vector([r.xf.y.x, r.xf.y.y, r.xf.y.z]))
            }
            None => {
                let p = sec_props[m.sec as usize].clone()?;
                let depth = sections::find(&f.sections[m.sec as usize]).map_or(0.1, |s| s.size().0.max(s.size().1));
                (fem::Section::new(p.a, p.iy, p.iz, (p.iy + p.iz) * 0.01), p.a * l * DENSITY, depth, fem::Orient::Auto)
            }
        };
        fr.sections.push(sec);
        let mut mem = fem::Member::new(m.a as usize, m.b as usize, 0, i);
        mem.orient = orient;
        fr.members.push(mem);
        let wg = mass * GRAVITY / 2.0;
        for n in [m.a as usize, m.b as usize] {
            node_g[n][2] -= wg;
        }
        // wind from each direction on the area the member shows across it
        for (k, d) in dirs.iter().enumerate() {
            let along = u.x * d.0 + u.y * d.1;
            let ww = c.wind_q * face * l * (1.0 - along * along).max(0.0).sqrt() / 2.0;
            for n in [m.a as usize, m.b as usize] {
                node_w[k][n][0] += ww * d.0;
                node_w[k][n][1] += ww * d.1;
            }
        }
        if m.role == MemRole::Beam {
            // floor beams run both ways: each direction carries half its strip
            live.push(fem::Load::udl(i, [0.0, 0.0, -c.live_q * h.params.floor_grid / 2.0], fem::Axes::Global));
        }
    }
    for (n, p) in f.nodes.iter().enumerate() {
        if p.z <= 1e-6 {
            fr.supports.push(fem::Support::fixed(n));
        }
    }
    let nodal = |v: &[[f64; 3]]| -> Vec<fem::Load> { v.iter().enumerate().filter(|(_, f)| f.iter().any(|x| *x != 0.0)).map(|(n, f)| fem::Load::Node { node: n, f: [f[0], f[1], f[2], 0.0, 0.0, 0.0] }).collect() };
    let mut gl = nodal(&node_g);
    gl.extend(live);
    fr.cases.push(fem::LoadCase { name: "G".into(), loads: gl });
    for (k, w) in node_w.iter().enumerate() {
        fr.cases.push(fem::LoadCase { name: format!("W{}", WIND_DEG[k]), loads: nodal(w) });
        fr.combos.push(fem::Combo { name: format!("G+W{}", WIND_DEG[k]), factors: vec![(0, 1.0), (k + 1, 1.0)] });
    }
    let sol = fem::solve(&fr, &fem::Options { stations: 3, ..Default::default() }).map_err(|e| e.to_string())?;
    let sway = sol.cases[1..].iter().flat_map(|cs| cs.disp.iter()).map(|d| d[0].hypot(d[1])).fold(0.0, f64::max);
    let mut uses: Vec<Use> = Vec::with_capacity(f.members.len());
    for res in &sol.combos {
    for (i, m) in f.members.iter().enumerate() {
        let d = &res.diagrams[i];
        let (demand, capacity, slender) = match root.get(&(i as u32)) {
            Some(&ri) => {
                let p = &h.protos[h.roots[ri].proto as usize];
                let nc = if p.planar { 2.0 } else { 4.0 };
                let worst = d.f.iter().map(|q| -q[0] / nc + (q[4].abs() + q[5].abs()) / (p.axis_w * nc / 2.0)).fold(f64::NEG_INFINITY, f64::max);
                let panel = p.len / p.panels as f64;
                let a1 = p.chord_area / nc;
                let (r, lb) = match p.kids.first() {
                    Some((k, _)) => {
                        // laced pair: the pair over the parent panel, one angle over its own panel
                        let kp = &h.protos[*k as usize];
                        let ang = kp.leaves.iter().find(|l| l.role == MemRole::Chord).map(|l| h.sections[l.sec as usize].clone());
                        let r1 = ang.and_then(|s| shape_props(&s).ok()).map_or(0.02, |q| (q.imin / q.a).sqrt());
                        let (lam_pair, lam_one) = (panel / (kp.width / 2.0), (kp.len / kp.panels as f64) / r1);
                        if lam_pair >= lam_one { (kp.width / 2.0, panel) } else { (r1, kp.len / kp.panels as f64) }
                    }
                    None => {
                        let s = p.leaves.iter().find(|l| l.role == MemRole::Chord).map(|l| h.sections[l.sec as usize].clone());
                        (s.and_then(|s| shape_props(&s).ok()).map_or(0.02, |q| (q.imin / q.a).sqrt()), panel)
                    }
                };
                let (demand, capacity, slender) = (worst.max(0.0), rankine(a1, r, lb, c), lb / r);
                // the bracing of the member carries its shear: one diagonal per face in work
                let shear = d.f.iter().map(|q| q[1].abs().max(q[2].abs())).fold(0.0, f64::max);
                let lac = p.leaves.iter().find(|l| l.role == MemRole::Diagonal).and_then(|l| shape_props(&h.sections[l.sec as usize]).ok().map(|q| (l.len(), q)));
                match lac {
                    Some((ld, q)) if ld > 1e-6 => {
                        let force = shear / (2.0 * (p.axis_w / ld).clamp(0.05, 1.0));
                        let cap = rankine(q.a, (q.imin / q.a).sqrt(), ld, c);
                        if force / cap > demand / capacity.max(1e-9) { (force, cap, ld / (q.imin / q.a).sqrt()) } else { (demand, capacity, slender) }
                    }
                    _ => (demand, capacity, slender),
                }
            }
            None => {
                let p = sec_props[m.sec as usize].clone()?;
                let r = (p.imin / p.a).sqrt();
                let l = f.length(m);
                let (wy, wz) = p.w();
                // axial compression against the buckling capacity plus bending against the stress
                let worst = d.f.iter().map(|q| (-q[0]).max(0.0) / rankine(p.a, r, l, c) + (q[4].abs() / wy + q[5].abs() / wz) / c.f_allow).fold(0.0, f64::max);
                (worst, 1.0, l / r)
            }
        };
        let ratio = demand / capacity.max(1e-9);
        let u = Use { member: i as u32, demand, capacity, ratio, slender };
        if i < uses.len() {
            if ratio > uses[i].ratio {
                uses[i] = u;
            }
        } else {
            uses.push(u);
        }
    }
    }
    let (max_member, max) = uses.iter().map(|u| (u.member, u.ratio)).fold((0, 0.0), |a, b| if b.1 > a.1 { b } else { a });
    let over = uses.iter().filter(|u| u.ratio > 1.0).count();
    Ok(CheckReport { uses, max, max_member, over, sway, residual: sol.combos.iter().map(|r| r.equilibrium.relative).fold(0.0, f64::max), equations: sol.equations, preliminary: true })
}
