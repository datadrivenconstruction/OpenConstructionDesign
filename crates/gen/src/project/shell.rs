// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Free-form envelope of a plate building (`design.shell`): a skin that swells and leans in
//! waves round the outline, a roof that rises in a crown to a central opening and rolls with
//! the eaves, both cut into patches of a size that can be made. The outline must be seen whole
//! from one inner point (a blob, an ellipse, a bowed bar); patches run between radial lines
//! from that point. Neighbouring patches are skinned through the same points on their shared
//! edge with the same parameter per row, so they meet on one curve; `ShellReport::seam` is the
//! measured largest gap between shared edges. The model's own freeform op skins with chord
//! parameters, so the rows it stores follow these edges to within that parametrisation only.
//!
//! Each patch is a cubic surface skinned through section curves that pass through the sampled
//! points of the field below, so the model draws the surface the generator measured. The skin
//! is glazed in panels: the generator panelises every skin patch with the model's own panel
//! code and reports panel count, flat / cold-bent / hot-bent shares, the largest diagonal gap
//! and the largest distance of a panel corner from the surface. Ribs follow the patch edges up
//! the skin and over the roof; a glazed dome with a diagrid closes the central opening.
//!
//! Field, with `s` the arc length along the outline from its east-most corner, `t` the share
//! of height, `rho` the share of the way from the opening to the eaves:
//! skin offset `o(s,t) = gap + bulge*swell(s)*sin(pi*t^q) + lean*lean(s)*t`, outward, clamped
//! to a share of the radius of curvature where the outline is concave; eaves
//! `top(s) = z0 + wave*cos(2*pi*waves*s/L)`; roof
//! `z = z0 + wave*cos(..)*rho^2 + crown*(1 + cos(pi*rho))/2`.

use super::rules::rule;
use super::{GProject, ProjectSpec};
use crate::GenError;
use ddc_geom::nurbs::{loft_through, Curve, Surface};
use ddc_geom::panelize::{panelize, PanelKind, PanelLimits, Pattern, Rows, Spacing};
use ddc_geom::{v3, V3};
use std::f64::consts::{PI, TAU};

pub type P = (f64, f64);
pub type P3 = [f64; 3];

/// Points of one section curve of a patch; the curve is a cubic through them.
pub const SECTION_POINTS: usize = 6;
/// Section curves across a skin patch (bottom to eaves) and across a roof band.
pub const SKIN_ROWS: usize = 5;
pub const ROOF_ROWS: usize = 4;
const CURVE_DEGREE: usize = 3;
const MIN_SECTORS: usize = 6;
const MAX_BANDS: usize = 6;
/// Candidates per side when no inner point sees the outline whole at its centroid.
const KERNEL_GRID: usize = 24;
/// Pull of a panel corner back to the surface while planarising (the model's value).
/// 0.25 left corners 0.0117 m off at 0.9 x 1.1 m panels; 0.3 trades gap for surface fit.
const STICK: f64 = 0.3;
/// Sample grid per patch for the curvature, the clearance and the seams.
const CURV_GRID: usize = 5;
const SAMPLE_GRID: usize = 7;
const SEAM_SAMPLES: usize = 9;
/// Points of a rib path: along a skin edge, and per band along a radial line of the roof.
const RIB_SKIN_POINTS: usize = 9;
const RIB_ROOF_POINTS_PER_BAND: usize = 3;
/// Samples round the outline and out from the opening when the roof height under a unit is read.
const ROOF_SCAN: (usize, usize) = (720, 24);
/// Build-up of the roof from the outside in; the shares come from the rules.
pub const ROOF_LAYER_NAMES: [&str; 3] = ["Cladding", "Insulation", "Deck"];

/// One patch: section curves, each a list of points to pass through.
#[derive(Clone, Debug, PartialEq)]
pub struct ShellPatch {
    pub name: String,
    pub sections: Vec<Vec<P3>>,
}

/// What was measured on the skin patches.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShellReport {
    pub patches: usize,
    pub panels: usize,
    pub flat: usize,
    pub cold: usize,
    pub hot: usize,
    /// Distinct panel shapes within the cluster tolerance, summed over patches.
    pub shapes: usize,
    /// Largest diagonal gap of a quad panel before and after planarising, m.
    pub raw_gap: f64,
    pub gap: f64,
    /// Largest distance of a panel corner from its surface, m.
    pub off_surface: f64,
    /// Patches curved both ways (radius under a kilometre along u and along v).
    pub double_curved: usize,
    /// Largest normal curvature along the facade and up it, 1/m.
    pub curvature_u: f64,
    pub curvature_v: f64,
    /// Nearest approach of the skin surface to the slab outline, m; negative if it crosses it.
    pub clearance: f64,
    /// Roof patches curved both ways, measured like the skin's.
    pub roof_double_curved: usize,
    /// Largest distance between the shared edge of two neighbouring patches, m.
    pub seam: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GShell {
    pub centre: P,
    /// Radius of the central opening and the height of its rim, m above the project base.
    pub oculus: f64,
    pub rim: f64,
    pub dome_rise: f64,
    pub skin: Vec<ShellPatch>,
    pub roof: Vec<ShellPatch>,
    pub skin_ribs: Vec<Vec<P3>>,
    pub roof_ribs: Vec<Vec<P3>>,
    /// Depth of the ribs and total thickness of the roof build-up, m.
    pub rib: f64,
    pub roof_thickness: f64,
    /// Panel size asked, m, rounds of planarising and the flat / cold limits, m.
    pub panel: (f64, f64),
    pub planarize: u32,
    pub flat: f64,
    pub cold: f64,
    pub sectors: usize,
    pub bands: usize,
    /// Share of the roof thickness of each layer in [`ROOF_LAYER_NAMES`] order.
    pub layer_shares: [f64; 3],
    /// Glass thickness, mullion width and depth and the rib width, m.
    pub glass: f64,
    pub mullion: (f64, f64),
    pub rib_width: f64,
    /// Catalogue section of the dome's diagrid.
    pub member: String,
    /// How far the eaves were raised to clear roof plant, m.
    pub lift: f64,
    pub report: ShellReport,
}

fn infeasible(s: impl Into<String>) -> GenError {
    GenError::Infeasible(s.into())
}

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}
fn dist(a: P, b: P) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}
fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}
fn dot(a: P, b: P) -> f64 {
    a.0 * b.0 + a.1 * b.1
}
fn norm(a: P) -> P {
    let l = a.0.hypot(a.1);
    if l < 1e-12 {
        (0.0, 0.0)
    } else {
        (a.0 / l, a.1 / l)
    }
}

fn signed_area(r: &[P]) -> f64 {
    let n = r.len();
    (0..n).map(|i| cross(r[i], r[(i + 1) % n])).sum::<f64>() / 2.0
}

fn seg_dist(p: P, a: P, b: P) -> f64 {
    let d = sub(b, a);
    let l2 = dot(d, d);
    let t = if l2 < 1e-18 { 0.0 } else { (dot(sub(p, a), d) / l2).clamp(0.0, 1.0) };
    dist(p, (a.0 + d.0 * t, a.1 + d.1 * t))
}

fn inside(r: &[P], p: P) -> bool {
    let n = r.len();
    let mut k = false;
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            k = !k;
        }
    }
    k
}

/// The slab outline as a counter-clockwise ring with arc length, outward normals at its
/// corners and signed curvature (positive where it turns left).
struct Ring {
    pts: Vec<P>,
    cum: Vec<f64>,
    en: Vec<P>,
    nrm: Vec<P>,
    kap: Vec<f64>,
    len: f64,
}

impl Ring {
    fn new(outer: &[P]) -> Result<Ring, GenError> {
        let mut pts: Vec<P> = vec![];
        for &q in outer {
            if pts.last().is_none_or(|l| dist(*l, q) > 1e-6) {
                pts.push(q);
            }
        }
        while pts.len() > 1 && dist(pts[0], pts[pts.len() - 1]) <= 1e-6 {
            pts.pop();
        }
        if pts.len() < 3 {
            return Err(infeasible("the shell needs a plan outline of at least three corners"));
        }
        if signed_area(&pts) < 0.0 {
            pts.reverse();
        }
        let n = pts.len();
        let east = (0..n)
            .max_by(|&a, &b| pts[a].0.total_cmp(&pts[b].0).then(pts[b].1.total_cmp(&pts[a].1)))
            .unwrap_or(0);
        pts.rotate_left(east);
        let mut cum = vec![0.0];
        for i in 0..n {
            cum.push(cum[i] + dist(pts[i], pts[(i + 1) % n]));
        }
        let len = cum[n];
        let dirs: Vec<P> = (0..n).map(|i| sub(pts[(i + 1) % n], pts[i])).collect();
        let en: Vec<P> = dirs.iter().map(|d| norm((d.1, -d.0))).collect();
        let nrm: Vec<P> = (0..n)
            .map(|i| {
                let e = en[(i + n - 1) % n];
                let m = norm((e.0 + en[i].0, e.1 + en[i].1));
                if m == (0.0, 0.0) {
                    en[i]
                } else {
                    m
                }
            })
            .collect();
        let kap: Vec<f64> = (0..n)
            .map(|i| {
                let (a, b) = (dirs[(i + n - 1) % n], dirs[i]);
                let turn = cross(a, b).atan2(dot(a, b));
                let lc = (a.0.hypot(a.1) + b.0.hypot(b.1)) / 2.0;
                turn / lc.max(1e-9)
            })
            .collect();
        Ok(Ring { pts, cum, en, nrm, kap, len })
    }

    /// Point, outward unit normal and signed curvature at arc length `s` (wrapping).
    fn at(&self, s: f64) -> (P, P, f64) {
        let n = self.pts.len();
        let s = s.rem_euclid(self.len);
        let i = (self.cum.partition_point(|&c| c <= s).max(1) - 1).min(n - 1);
        let j = (i + 1) % n;
        let t = ((s - self.cum[i]) / (self.cum[i + 1] - self.cum[i]).max(1e-12)).clamp(0.0, 1.0);
        let lerp = |a: P, b: P| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        let mut nn = norm(lerp(self.nrm[i], self.nrm[j]));
        if nn == (0.0, 0.0) {
            nn = self.en[i];
        }
        (lerp(self.pts[i], self.pts[j]), nn, self.kap[i] + (self.kap[j] - self.kap[i]) * t)
    }

    fn sees_all(&self, c: P) -> bool {
        let n = self.pts.len();
        (0..n).all(|i| cross(sub(self.pts[i], c), sub(self.pts[(i + 1) % n], c)) > 1e-9)
    }

    fn clearance(&self, c: P) -> f64 {
        let n = self.pts.len();
        (0..n).map(|i| seg_dist(c, self.pts[i], self.pts[(i + 1) % n])).fold(f64::MAX, f64::min)
    }

    fn centroid(&self) -> P {
        let n = self.pts.len();
        let a = signed_area(&self.pts);
        if a.abs() < 1e-9 {
            return self.pts[0];
        }
        let (mut x, mut y) = (0.0, 0.0);
        for i in 0..n {
            let (p, q) = (self.pts[i], self.pts[(i + 1) % n]);
            let w = cross(p, q);
            x += (p.0 + q.0) * w;
            y += (p.1 + q.1) * w;
        }
        (x / (6.0 * a), y / (6.0 * a))
    }

    /// The inner point the radial lines run from: the centroid if it sees the whole outline,
    /// else the roomiest point that does.
    fn centre(&self) -> Result<(P, f64), GenError> {
        let c = self.centroid();
        if self.sees_all(c) {
            return Ok((c, self.clearance(c)));
        }
        let lo = self.pts.iter().fold((f64::MAX, f64::MAX), |m, p| (m.0.min(p.0), m.1.min(p.1)));
        let hi = self.pts.iter().fold((f64::MIN, f64::MIN), |m, p| (m.0.max(p.0), m.1.max(p.1)));
        let mut best: Option<(P, f64)> = None;
        for i in 0..=KERNEL_GRID {
            for j in 0..=KERNEL_GRID {
                let q = (
                    lo.0 + (hi.0 - lo.0) * i as f64 / KERNEL_GRID as f64,
                    lo.1 + (hi.1 - lo.1) * j as f64 / KERNEL_GRID as f64,
                );
                if self.sees_all(q) {
                    let c = self.clearance(q);
                    if best.is_none_or(|b| c > b.1) {
                        best = Some((q, c));
                    }
                }
            }
        }
        best.ok_or_else(|| {
            infeasible(
                "the free-form shell wraps a plan that one inner point sees whole; this outline \
                 has none (a courtyard or a deep L or U): give a rounder footprint",
            )
        })
    }
}

/// The field of the envelope.
struct Form {
    ring: Ring,
    c: P,
    r_in: f64,
    zg: f64,
    /// Mean eaves height; the eaves run `wave` above and below it.
    z0: f64,
    wave: f64,
    crown: f64,
    gap: f64,
    bulge: f64,
    /// Outward lean at the eaves, m, at the leaning crest of the wave.
    lean: f64,
    waves: f64,
    concave: f64,
    ph_swell: f64,
    ph_lean: f64,
    /// Exponent that puts the peak of the swell at the rule's height share.
    q: f64,
}

impl Form {
    fn phase(&self, s: f64) -> f64 {
        TAU * self.waves * s / self.ring.len
    }
    fn swell(&self, s: f64) -> f64 {
        0.5 + 0.5 * (self.phase(s) + self.ph_swell).cos()
    }
    fn lean_at(&self, s: f64) -> f64 {
        0.5 + 0.5 * (self.phase(s) + self.ph_lean).cos()
    }
    fn top(&self, s: f64) -> f64 {
        self.z0 + self.wave * self.phase(s).cos()
    }
    /// How far the skin stands out from the slab outline at arc length `s` and height share `t`.
    fn offset(&self, s: f64, t: f64) -> f64 {
        let k = self.ring.at(s).2;
        let raw = self.gap
            + self.bulge * self.swell(s) * (PI * t.clamp(0.0, 1.0).powf(self.q)).sin().max(0.0)
            + self.lean * self.lean_at(s) * t;
        if k < 0.0 {
            raw.min(self.concave / -k)
        } else {
            raw
        }
    }
    /// Point of the skin; `back` moves it that far inwards, along the normal.
    fn skin(&self, s: f64, t: f64, back: f64) -> P3 {
        let (p, n, _) = self.ring.at(s);
        let o = self.offset(s, t) - back;
        [p.0 + n.0 * o, p.1 + n.1 * o, self.zg + t * (self.top(s) - self.zg)]
    }
    /// Plan point of the eaves: the outer edge of the roof.
    fn eaves(&self, s: f64) -> P {
        let p = self.skin(s, 1.0, 0.0);
        (p[0], p[1])
    }
    /// Point of the roof at `rho` (0 on the rim of the opening, 1 at the eaves).
    fn roof(&self, s: f64, rho: f64) -> P3 {
        let e = self.eaves(s);
        let dir = norm(sub(e, self.c));
        let i = (self.c.0 + dir.0 * self.r_in, self.c.1 + dir.1 * self.r_in);
        let z = self.z0
            + self.wave * self.phase(s).cos() * rho * rho
            + self.crown * 0.5 * (1.0 + (PI * rho).cos());
        [i.0 + (e.0 - i.0) * rho, i.1 + (e.1 - i.1) * rho, z]
    }
    fn rim(&self) -> f64 {
        self.z0 + self.crown
    }
}

/// Sectors round the outline and bands out from the opening, within [`MAX_PATCHES`].
fn plan(len: f64, r_mean: f64, r_in: f64) -> (usize, usize) {
    let most = rule("massing.shell_patches_max") as usize;
    let mut w = rule("massing.shell_patch");
    loop {
        let k = ((len / w).round() as usize).max(MIN_SECTORS);
        let m = (((r_mean - r_in) / w).round() as usize).clamp(1, MAX_BANDS);
        if k + k * m <= most {
            return (k, m);
        }
        w *= 1.25;
    }
}

fn sections(f: impl Fn(f64) -> P3, s0: f64, ds: f64) -> Vec<P3> {
    (0..SECTION_POINTS).map(|j| f(s0 + ds * j as f64 / (SECTION_POINTS - 1) as f64)).collect()
}

/// The surface the model draws from these sections: curves through the points, skinned.
pub fn surface(sec: &[Vec<P3>]) -> Option<Surface> {
    let cs: Vec<Curve> = sec
        .iter()
        .map(|s| {
            let pts: Vec<V3> = s.iter().map(|p| v3(p[0], p[1], p[2])).collect();
            Curve::interpolate(&pts, CURVE_DEGREE).ok()
        })
        .collect::<Option<_>>()?;
    // one parameter per section row for every patch: neighbours meet on the same edge curve
    let last = (cs.len() - 1) as f64;
    let params: Vec<f64> = (0..cs.len()).map(|i| i as f64 / last).collect();
    loft_through(&cs, CURVE_DEGREE, &params).ok().map(|l| l.surface)
}

/// Largest normal curvature along u and along v of a surface, 1/m.
fn curvature(s: &Surface) -> (f64, f64) {
    let (u0, u1) = s.domain_u();
    let (v0, v1) = s.domain_v();
    let (mut ku, mut kv) = (0.0f64, 0.0f64);
    for a in 0..CURV_GRID {
        for b in 0..CURV_GRID {
            let u = u0 + (u1 - u0) * (a as f64 + 0.5) / CURV_GRID as f64;
            let v = v0 + (v1 - v0) * (b as f64 + 0.5) / CURV_GRID as f64;
            let Some(n) = s.normal(u, v) else { continue };
            let d = s.derivs(u, v, 2);
            let (su, sv) = (d[1][0], d[0][1]);
            ku = ku.max((d[2][0].dot(n) / su.dot(su).max(1e-12)).abs());
            kv = kv.max((d[0][2].dot(n) / sv.dot(sv).max(1e-12)).abs());
        }
    }
    (ku, kv)
}

/// Points along one edge of a patch: `side` 0 and 1 are u at its start and end, 2 and 3 v.
fn edge(s: &Surface, side: usize) -> Vec<V3> {
    let (u0, u1) = s.domain_u();
    let (v0, v1) = s.domain_v();
    (0..SEAM_SAMPLES)
        .map(|i| {
            let t = i as f64 / (SEAM_SAMPLES - 1) as f64;
            match side {
                0 => s.point(u0, v0 + (v1 - v0) * t),
                1 => s.point(u1, v0 + (v1 - v0) * t),
                2 => s.point(u0 + (u1 - u0) * t, v0),
                _ => s.point(u0 + (u1 - u0) * t, v1),
            }
        })
        .collect()
}

fn apart(a: &[V3], b: &[V3]) -> f64 {
    a.iter().zip(b).map(|(p, q)| (*p - *q).len()).fold(0.0, f64::max)
}

/// Panels, curvature, clearance and seams of the patches.
fn measure(f: &Form, sh: &mut GShell) -> Result<(), GenError> {
    let limits = PanelLimits { flat: sh.flat, cold: sh.cold, ..PanelLimits::default() };
    let mut r = ShellReport { patches: sh.skin.len(), clearance: f64::MAX, ..ShellReport::default() };
    let flat = 1.0 / rule("massing.shell_flat_radius");
    let build = |ps: &[ShellPatch]| -> Result<Vec<Surface>, GenError> {
        ps.iter()
            .map(|p| {
                surface(&p.sections)
                    .ok_or_else(|| infeasible(format!("shell {}: the section curves make no surface", p.name)))
            })
            .collect()
    };
    let skin = build(&sh.skin)?;
    let roof = build(&sh.roof)?;
    let n = f.ring.pts.len();
    for (patch, s) in sh.skin.iter().zip(&skin) {
        let mut pn = panelize(
            s,
            Spacing::Width(sh.panel.0),
            &Rows::Spacing(Spacing::Width(sh.panel.1)),
            Pattern::Quad,
            limits,
        )
        .map_err(|e| infeasible(format!("shell {}: panels: {e}", patch.name)))?;
        r.raw_gap = r.raw_gap.max(pn.max_planarity());
        if sh.planarize > 0 {
            pn.planarize(s, sh.planarize as usize, STICK);
            pn.measure();
        }
        r.gap = r.gap.max(pn.max_planarity());
        r.off_surface = r.off_surface.max(pn.off_surface(s));
        r.panels += pn.panels.len();
        r.shapes += pn.clusters.len();
        for p in &pn.panels {
            match p.kind {
                PanelKind::Flat => r.flat += 1,
                PanelKind::ColdBent => r.cold += 1,
                PanelKind::HotBent => r.hot += 1,
            }
        }
        let (ku, kv) = curvature(s);
        r.curvature_u = r.curvature_u.max(ku);
        r.curvature_v = r.curvature_v.max(kv);
        if ku > flat && kv > flat {
            r.double_curved += 1;
        }
        // the clearance is read on the surface, not only at the sections
        let (u0, u1) = s.domain_u();
        let (v0, v1) = s.domain_v();
        for a in 0..SAMPLE_GRID {
            for b in 0..SAMPLE_GRID {
                let q = s.point(
                    u0 + (u1 - u0) * a as f64 / (SAMPLE_GRID - 1) as f64,
                    v0 + (v1 - v0) * b as f64 / (SAMPLE_GRID - 1) as f64,
                );
                let xy = (q.x, q.y);
                let d = (0..n)
                    .map(|i| seg_dist(xy, f.ring.pts[i], f.ring.pts[(i + 1) % n]))
                    .fold(f64::MAX, f64::min);
                r.clearance = r.clearance.min(if inside(&f.ring.pts, xy) { -d } else { d });
            }
        }
    }
    for s in &roof {
        let (ku, kv) = curvature(s);
        if ku > flat && kv > flat {
            r.roof_double_curved += 1;
        }
    }
    // neighbours along the outline meet on one edge; the skin top meets the outer roof band
    let k = sh.sectors;
    for a in 0..k {
        let b = (a + 1) % k;
        r.seam = r.seam.max(apart(&edge(&skin[a], 1), &edge(&skin[b], 0)));
        for band in 0..sh.bands {
            let (x, y) = (&roof[band * k + a], &roof[band * k + b]);
            r.seam = r.seam.max(apart(&edge(x, 1), &edge(y, 0)));
            if band + 1 < sh.bands {
                r.seam = r.seam.max(apart(&edge(x, 3), &edge(&roof[(band + 1) * k + a], 2)));
            }
        }
        r.seam = r.seam.max(apart(&edge(&skin[a], 3), &edge(&roof[(sh.bands - 1) * k + a], 3)));
    }
    let least = rule("massing.shell_clear_share") * rule("massing.shell_gap");
    if r.clearance < least {
        return Err(infeasible(format!(
            "the free-form skin comes {:.2} m from the slab edge, under the {:.2} m the rules ask: \
             a rounder outline, a smaller swell or a smaller stand-off",
            r.clearance, least
        )));
    }
    sh.report = r;
    Ok(())
}

/// Height of the roof over the plan point `xy`, read from the nearest sample of a scan of it.
fn roof_z_at(f: &Form, xy: P) -> f64 {
    let (ns, nr) = ROOF_SCAN;
    let mut best = (f64::MAX, 0.0);
    for i in 0..ns {
        let s = f.ring.len * i as f64 / ns as f64;
        for j in 0..=nr {
            let p = f.roof(s, j as f64 / nr as f64);
            let d = dist((p[0], p[1]), xy);
            if d < best.0 {
                best = (d, p[2]);
            }
        }
    }
    best.1
}

/// The envelope of the slab outline `outer` of a storey at `zg`, its walls `h` high. Roof plant
/// at `plant` (plan point, height of its origin above the roof level) gets the roof clear above
/// it: the whole shell rises as far as that takes.
pub fn envelope(outer: &[P], zg: f64, h: f64, plant: &[(P, f64)]) -> Result<GShell, GenError> {
    let ring = Ring::new(outer)?;
    let (c, clear) = ring.centre()?;
    let r_in = rule("massing.shell_oculus") * clear;
    let samples = 64;
    let r_mean = (0..samples).map(|i| dist(ring.at(ring.len * i as f64 / samples as f64).0, c)).sum::<f64>()
        / samples as f64;
    let (k, m) = plan(ring.len, r_mean, r_in);
    let wave = rule("massing.shell_wave");
    let lean = h * rule("massing.shell_lean").to_radians().tan();
    let mut f = Form {
        ring,
        c,
        r_in,
        zg,
        z0: zg + h + wave,
        wave,
        crown: rule("massing.shell_crown"),
        gap: rule("massing.shell_gap"),
        bulge: rule("massing.shell_bulge"),
        lean,
        waves: rule("massing.shell_waves").round().max(1.0),
        concave: rule("massing.shell_concave"),
        ph_swell: rule("massing.shell_phase_swell").to_radians(),
        ph_lean: rule("massing.shell_phase_lean").to_radians(),
        q: 0.5f64.ln() / rule("massing.shell_peak").ln(),
    };
    let want = rule("massing.shell_plant_clear");
    let lift = plant
        .iter()
        .map(|(xy, z)| zg + h + z + want - roof_z_at(&f, *xy))
        .fold(0.0, f64::max);
    f.z0 += lift;
    let ds = f.ring.len / k as f64;
    let mut skin = vec![];
    for a in 0..k {
        let s0 = a as f64 * ds;
        let secs = (0..SKIN_ROWS)
            .map(|i| {
                let t = i as f64 / (SKIN_ROWS - 1) as f64;
                sections(|s| f.skin(s, t, 0.0), s0, ds)
            })
            .collect();
        skin.push(ShellPatch { name: format!("Skin {}", a + 1), sections: secs });
    }
    let mut roof = vec![];
    for b in 0..m {
        for a in 0..k {
            let s0 = a as f64 * ds;
            let secs = (0..ROOF_ROWS)
                .map(|i| {
                    let rho = (b as f64 + i as f64 / (ROOF_ROWS - 1) as f64) / m as f64;
                    sections(|s| f.roof(s, rho), s0, ds)
                })
                .collect();
            roof.push(ShellPatch { name: format!("Roof {}.{}", b + 1, a + 1), sections: secs });
        }
    }
    let rib = rule("massing.shell_rib");
    let back = rule("massing.shell_rib_inset") * rib;
    let mut skin_ribs = vec![];
    let mut roof_ribs = vec![];
    for a in 0..k {
        let s = a as f64 * ds;
        skin_ribs.push(
            (0..RIB_SKIN_POINTS).map(|i| f.skin(s, i as f64 / (RIB_SKIN_POINTS - 1) as f64, back)).collect(),
        );
        let n = RIB_ROOF_POINTS_PER_BAND * m;
        roof_ribs.push(
            (0..=n)
                .map(|i| {
                    let mut p = f.roof(s, i as f64 / n as f64);
                    p[2] -= back;
                    p
                })
                .collect(),
        );
    }
    let deck = rule("massing.shell_roof_deck");
    let insul = rule("massing.shell_roof_insul").min(1.0 - deck - 0.05).max(0.0);
    let mut sh = GShell {
        centre: c,
        oculus: r_in,
        rim: f.rim(),
        dome_rise: rule("massing.shell_dome") * r_in,
        skin,
        roof,
        skin_ribs,
        roof_ribs,
        rib,
        roof_thickness: rule("massing.shell_roof"),
        panel: (rule("massing.shell_panel_w"), rule("massing.shell_panel_h")),
        planarize: rule("massing.shell_planarize").round() as u32,
        flat: rule("massing.shell_flat"),
        cold: rule("massing.shell_cold"),
        sectors: k,
        bands: m,
        layer_shares: [1.0 - deck - insul, insul, deck],
        glass: rule("massing.shell_glass"),
        mullion: (rule("massing.shell_mullion_w"), rule("massing.shell_mullion_d")),
        rib_width: rule("massing.shell_rib_width"),
        member: format!("CHS {}x{}", rule("massing.shell_member_d"), rule("massing.shell_member_t")),
        lift,
        report: ShellReport::default(),
    };
    measure(&f, &mut sh)?;
    Ok(sh)
}

/// Wraps the building in its envelope when the brief asks for `design.shell`. The shell
/// follows the largest storey outline; a storey that sticks out of it is refused.
pub(super) fn apply(spec: &ProjectSpec, p: &mut GProject) -> Result<(), GenError> {
    if !spec.design.shell {
        return Ok(());
    }
    let Some(ground) = p.storeys.first() else { return Ok(()) };
    let base = p
        .storeys
        .iter()
        .max_by(|a, b| signed_area(&a.layout.slab).abs().total_cmp(&signed_area(&b.layout.slab).abs()))
        .unwrap_or(ground);
    let ring = &base.layout.slab;
    let n = ring.len();
    let tol = rule("massing.shell_gap") * 0.01;
    for s in &p.storeys {
        let out = s.layout.slab.iter().any(|q| {
            !inside(ring, *q) && (0..n).map(|i| seg_dist(*q, ring[i], ring[(i + 1) % n])).fold(f64::MAX, f64::min) > tol
        });
        if out {
            return Err(infeasible(format!(
                "storey {} sticks out of the outline of {}, which the free-form shell is built on: \
                 keep the storeys inside the largest one",
                s.name, base.name
            )));
        }
    }
    let plant: Vec<(P, f64)> = p.hvac.plant.iter().map(|u| (u.at, u.z)).collect();
    let sh = envelope(ring, ground.elevation, p.roof.elevation - ground.elevation, &plant)?;
    let r = &sh.report;
    let pct = |n: usize| if r.panels == 0 { 0.0 } else { 100.0 * n as f64 / r.panels as f64 };
    p.metrics.extend([
        ("shell_patches", (sh.skin.len() + sh.roof.len()) as f64),
        ("shell_double_curved", (r.double_curved + r.roof_double_curved) as f64),
        ("shell_panels", r.panels as f64),
        ("shell_panel_flat_pct", pct(r.flat)),
        ("shell_panel_cold_pct", pct(r.cold)),
        ("shell_panel_hot_pct", pct(r.hot)),
        ("shell_panel_shapes", r.shapes as f64),
        ("shell_gap_mm", r.gap * 1000.0),
        ("shell_off_surface_mm", r.off_surface * 1000.0),
        ("shell_seam_mm", r.seam * 1000.0),
        ("shell_ribs", (sh.skin_ribs.len() + sh.roof_ribs.len()) as f64),
        ("shell_clearance_m", r.clearance),
        ("shell_lift_m", sh.lift),
    ]);
    p.shell = Some(Box::new(sh));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::rules::{scoped, Layer, RuleSet};
    use serde_json::json;

    fn ellipse(a: f64, b: f64, n: usize) -> Vec<P> {
        (0..n).map(|i| { let t = TAU * i as f64 / n as f64; (a * t.cos(), b * t.sin()) }).collect()
    }

    fn with<T>(patch: serde_json::Value, f: impl FnOnce() -> T) -> T {
        let mut rs = RuleSet::new();
        rs.apply_patch(Layer::Brief, &patch).unwrap();
        scoped(&rs, f)
    }

    fn env(a: f64, b: f64) -> GShell {
        envelope(&ellipse(a, b, 72), 0.0, 12.0, &[]).unwrap()
    }

    #[test]
    fn an_ellipse_gets_curved_patches_panels_and_a_clear_gap() {
        let sh = env(30.0, 20.0);
        let r = &sh.report;
        // the panel defaults' cost, for the bench line: panels, gap, corner offset
        eprintln!("shell 30x20 ellipse: {} panels, gap {:.4} m, off {:.4} m", r.panels, r.gap, r.off_surface);
        assert!(sh.skin.len() + sh.roof.len() <= rule("massing.shell_patches_max") as usize);
        assert_eq!(sh.skin.len(), sh.sectors);
        assert_eq!(sh.roof.len(), sh.sectors * sh.bands);
        assert!(r.panels > 100, "panels {}", r.panels);
        assert!(r.double_curved >= sh.skin.len() / 2, "skin double curved {} of {}", r.double_curved, sh.skin.len());
        assert!(r.roof_double_curved >= sh.roof.len() / 2, "roof double curved {} of {}", r.roof_double_curved, sh.roof.len());
        assert!(r.curvature_u > 1e-3 && r.curvature_v > 1e-3);
        assert!(r.gap <= sh.cold, "diagonal gap {} m after planarising", r.gap);
        assert!(r.gap <= r.raw_gap + 1e-9);
        assert!(r.off_surface < 0.01, "panel corners {} m off the surface", r.off_surface);
        assert!(r.clearance > 0.3, "skin {} m from the slab edge", r.clearance);
        assert_eq!(r.flat + r.cold + r.hot, r.panels);
    }

    /// The eval oracle's measure: Gaussian curvature of the node heights on a 7 x 7 grid.
    #[test]
    fn the_museum_shell_is_curved_both_ways_over_most_of_its_surface() {
        let rect = [(-35.0, -20.0), (35.0, -20.0), (35.0, 20.0), (-35.0, 20.0)];
        let sh = envelope(&rect, 0.0, 12.0, &[]).unwrap();
        let pts: Vec<P3> = sh.skin.iter().chain(&sh.roof).flat_map(|p| p.sections.iter().flatten().copied()).collect();
        const N: usize = 7;
        let (x0, x1) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p[0]), a.1.max(p[0])));
        let (y0, y1) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p[1]), a.1.max(p[1])));
        let (mut sum, mut cnt) = ([[0.0; N]; N], [[0usize; N]; N]);
        for p in &pts {
            let i = (((p[0] - x0) / (x1 - x0) * N as f64) as usize).min(N - 1);
            let j = (((p[1] - y0) / (y1 - y0) * N as f64) as usize).min(N - 1);
            sum[i][j] += p[2];
            cnt[i][j] += 1;
        }
        let z = |i: usize, j: usize| sum[i][j] / cnt[i][j].max(1) as f64;
        let (hx, hy, span) = ((x1 - x0) / N as f64, (y1 - y0) / N as f64, (x1 - x0).max(y1 - y0));
        let (mut cells, mut curved) = (0, 0);
        for i in 1..N - 1 {
            for j in 1..N - 1 {
                if [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (-1, 1), (1, -1), (-1, -1)]
                    .iter()
                    .any(|(a, b)| cnt[(i as i32 + a) as usize][(j as i32 + b) as usize] == 0)
                {
                    continue;
                }
                let (zx, zy) = ((z(i + 1, j) - z(i - 1, j)) / (2.0 * hx), (z(i, j + 1) - z(i, j - 1)) / (2.0 * hy));
                let zxx = (z(i + 1, j) - 2.0 * z(i, j) + z(i - 1, j)) / (hx * hx);
                let zyy = (z(i, j + 1) - 2.0 * z(i, j) + z(i, j - 1)) / (hy * hy);
                let zxy = (z(i + 1, j + 1) - z(i - 1, j + 1) - z(i + 1, j - 1) + z(i - 1, j - 1)) / (4.0 * hx * hy);
                let k = (zxx * zyy - zxy * zxy) / (1.0 + zx * zx + zy * zy).powi(2) * span * span;
                cells += 1;
                curved += (k.abs() > 0.05) as usize;
            }
        }
        assert!(cells >= 9 && curved as f64 >= 0.6 * cells as f64, "{curved} of {cells} cells curved");
        let r = &sh.report;
        assert!(r.roof_double_curved >= sh.roof.len() / 2 && r.curvature_u > 1e-3 && r.curvature_v > 1e-3);
    }

    #[test]
    fn neighbouring_patches_meet_within_a_millimetre_on_every_shared_edge() {
        let sh = env(30.0, 20.0);
        assert!(sh.report.seam < 1e-6, "seam {} m", sh.report.seam);
        let sh = envelope(&ellipse(25.0, 25.0, 64), 3.0, 9.0, &[]).unwrap();
        assert!(sh.report.seam < 1e-6, "seam {} m", sh.report.seam);
        for a in 0..sh.sectors {
            let top = sh.skin[a].sections.last().unwrap();
            let outer = sh.roof[(sh.bands - 1) * sh.sectors + a].sections.last().unwrap();
            for (p, q) in top.iter().zip(outer) {
                assert!((0..3).all(|k| (p[k] - q[k]).abs() < 1e-9), "{p:?} vs {q:?}");
            }
        }
    }

    #[test]
    fn the_surface_passes_through_its_sections() {
        let sh = env(30.0, 20.0);
        let s = surface(&sh.skin[0].sections).unwrap();
        let (u0, u1) = s.domain_u();
        let (v0, v1) = s.domain_v();
        let c = s.point(u0, v0);
        let p = sh.skin[0].sections[0][0];
        assert!((c.x - p[0]).abs() < 1e-6 && (c.y - p[1]).abs() < 1e-6 && (c.z - p[2]).abs() < 1e-6);
        let e = s.point(u1, v1);
        let q = sh.skin[0].sections.last().unwrap().last().unwrap();
        assert!((e.x - q[0]).abs() < 1e-6 && (e.z - q[2]).abs() < 1e-6);
    }

    #[test]
    fn a_courtyard_outline_is_refused_with_the_reason() {
        let u = vec![(0.0, 0.0), (30.0, 0.0), (30.0, 30.0), (22.0, 30.0), (22.0, 8.0), (8.0, 8.0), (8.0, 30.0), (0.0, 30.0)];
        match envelope(&u, 0.0, 9.0, &[]) {
            Err(GenError::Infeasible(m)) => assert!(m.contains("inner point"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_clockwise_outline_gives_the_same_patches() {
        let a = ellipse(20.0, 14.0, 48);
        let mut b = a.clone();
        b.reverse();
        let (x, y) = (envelope(&a, 0.0, 9.0, &[]).unwrap(), envelope(&b, 0.0, 9.0, &[]).unwrap());
        assert_eq!(x.skin, y.skin);
        assert_eq!(x.roof, y.roof);
    }

    #[test]
    fn a_narrow_outline_gives_a_shell_or_a_reason_never_a_panic() {
        match envelope(&ellipse(40.0, 3.0, 96), 0.0, 9.0, &[]) {
            Ok(sh) => assert!(sh.report.clearance >= rule("massing.shell_clear_share") * rule("massing.shell_gap")),
            Err(GenError::Infeasible(m)) => assert!(!m.is_empty()),
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn with_no_swell_wave_or_lean_the_skin_is_a_plain_wall_and_stays_clear() {
        let sh = with(
            json!({"massing.shell_bulge": 0.0, "massing.shell_wave": 0.0, "massing.shell_lean": 0.0}),
            || env(30.0, 20.0),
        );
        for patch in &sh.skin {
            let xy: Vec<_> = patch.sections.iter().map(|s| (s[0][0], s[0][1])).collect();
            assert!(xy.windows(2).all(|w| dist(w[0], w[1]) < 1e-9), "a vertical wall stays over its line");
        }
        assert!(sh.report.clearance > 0.3);
    }

    #[test]
    fn a_tiny_outline_is_refused_or_clear_not_degenerate() {
        match envelope(&ellipse(1.0, 1.0, 24), 0.0, 3.0, &[]) {
            Ok(sh) => assert!(sh.oculus > 0.0 && sh.report.clearance > 0.0),
            Err(GenError::Infeasible(m)) => assert!(!m.is_empty()),
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn roof_plant_gets_the_roof_raised_above_it() {
        let ring = ellipse(30.0, 20.0, 72);
        let free = envelope(&ring, 0.0, 12.0, &[]).unwrap();
        assert_eq!(free.lift, 0.0);
        // a unit by the eaves, where the roof is lowest
        let low = envelope(&ring, 0.0, 12.0, &[((27.0, 0.0), 0.0)]).unwrap();
        assert!(low.lift > 0.5, "lift {}", low.lift);
        assert!(low.rim > free.rim + 0.5);
        assert!((low.rim - free.rim - low.lift).abs() < 1e-9);
    }
}
