// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Member sizing of the lattice tower by a simple wind and dead-load rule (numbers from the
//! caller's rules, section ladders in `data/steel/tower_sizing.csv`) and the top sway of the
//! equivalent cantilever.
//! Preliminary: real sections by a rule of thumb, not a design.

use super::sections;
use super::tower::TowerParams;
use crate::math::V2;
use std::sync::OnceLock;

const DATA: &str = include_str!("../../data/steel/tower_sizing.csv");

/// The numbers of the sizing rule. The generator takes them from its rule set (`steel.check.*`);
/// the default is those rules' defaults (a generator test pins the two equal), the modulus is steel's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Loads {
    /// Wind pressure on the gross outline, kPa, and the force coefficient times solid share.
    pub wind_kpa: f64,
    pub face_factor: f64,
    /// Steel self weight per metre of height above a cut, kN/m; platform floor load, kPa.
    pub self_kn_m: f64,
    pub deck_kpa: f64,
    /// Allowable stress, MPa; Rankine constant of `A f / (1 + (L/r)^2 / a)`.
    pub f_allow_mpa: f64,
    pub rankine_a: f64,
    /// Share of the chord force a horizontal holds at its node.
    pub strut_share: f64,
    /// Elastic modulus for the sway, GPa.
    pub e_gpa: f64,
}

impl Default for Loads {
    fn default() -> Self {
        Loads { wind_kpa: 1.5, face_factor: 0.3, self_kn_m: 150.0, deck_kpa: 2.0, f_allow_mpa: 100.0, rankine_a: 9000.0, strut_share: 0.025, e_gpa: super::sections::E_STEEL / 1e9 }
    }
}

impl Loads {
    pub fn check(&self) -> Result<(), String> {
        let all = [self.wind_kpa, self.face_factor, self.self_kn_m, self.deck_kpa, self.f_allow_mpa, self.rankine_a, self.strut_share, self.e_gpa];
        if all.iter().any(|v| !(v.is_finite() && *v >= 0.0)) || self.f_allow_mpa <= 0.0 || self.rankine_a <= 0.0 || self.e_gpa <= 0.0 {
            return Err("tower loads: non-negative numbers; stress, Rankine constant and modulus positive".into());
        }
        Ok(())
    }
}

/// Section ladders per role, smallest first: the data of `data/steel/tower_sizing.csv`.
#[derive(Clone, Debug, PartialEq)]
pub struct Ladders {
    pub chord: Vec<String>,
    pub brace: Vec<String>,
    pub strut: Vec<String>,
}

impl Ladders {
    pub fn parse(text: &str) -> Result<Ladders, String> {
        let mut r = Ladders { chord: vec![], brace: vec![], strut: vec![] };
        // previous row per role (line, area): a ladder must grow strictly
        let mut last: [Option<(usize, f64)>; 3] = [None; 3];
        for (i, l) in text.lines().enumerate() {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let c: Vec<&str> = l.splitn(3, ',').map(str::trim).collect();
            if c.len() != 3 || c[0] != "ladder" {
                return Err(format!("line {}: ladder,<role>,<section>", i + 1));
            }
            let Some((area, _)) = props(c[2]) else {
                return Err(format!("line {}: no section '{}'", i + 1, c[2]));
            };
            let (k, list) = match c[1] {
                "chord" => (0, &mut r.chord),
                "brace" => (1, &mut r.brace),
                "strut" => (2, &mut r.strut),
                x => return Err(format!("line {}: unknown ladder '{x}'", i + 1)),
            };
            if let Some((pl, pa)) = last[k] {
                // relative margin: equal-area sections (an angle and a box) must not pass by rounding
                if area <= pa * (1.0 + 1e-6) {
                    return Err(format!("line {}: '{}' area {:.6} m2 not above line {pl} ({pa:.6} m2); ladder areas must rise", i + 1, c[2], area));
                }
            }
            last[k] = Some((i + 1, area));
            list.push(c[2].to_string());
        }
        for (k, l) in [("chord", &r.chord), ("brace", &r.brace), ("strut", &r.strut)] {
            if l.is_empty() {
                return Err(format!("ladder {k}: no rows"));
            }
        }
        Ok(r)
    }

    pub fn builtin() -> &'static Ladders {
        static R: OnceLock<Ladders> = OnceLock::new();
        R.get_or_init(|| Ladders::parse(DATA).unwrap_or_else(|e| panic!("tower_sizing.csv {e}")))
    }
}

/// Area (m2) and least radius of gyration (m, about the weak principal axis) of a section.
pub fn props(name: &str) -> Option<(f64, f64)> {
    let s = sections::find(name)?;
    let (outer, hole) = s.rings();
    // signed polygon sums about the origin: the hole runs the other way and subtracts
    let (mut a, mut sx, mut sy, mut ixx, mut iyy, mut ixy) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for r in std::iter::once(&outer).chain(hole.as_ref()) {
        let n = r.len();
        for i in 0..n {
            let (p, q): (V2, V2) = (r[i], r[(i + 1) % n]);
            let c = p.x * q.y - q.x * p.y;
            a += c / 2.0;
            sx += (p.x + q.x) * c / 6.0;
            sy += (p.y + q.y) * c / 6.0;
            ixx += (p.y * p.y + p.y * q.y + q.y * q.y) * c / 12.0;
            iyy += (p.x * p.x + p.x * q.x + q.x * q.x) * c / 12.0;
            ixy += (p.x * q.y + 2.0 * p.x * p.y + 2.0 * q.x * q.y + q.x * p.y) * c / 24.0;
        }
    }
    let k = a.signum();
    let (a, sx, sy, ixx, iyy, ixy) = (a * k, sx * k, sy * k, ixx * k, iyy * k, ixy * k);
    if a <= 1e-12 {
        return None;
    }
    let (cx, cy) = (sx / a, sy / a);
    let (ix, iy, ixy) = (ixx - a * cy * cy, iyy - a * cx * cx, ixy - a * cx * cy);
    let imin = (ix + iy) / 2.0 - (((ix - iy) / 2.0).powi(2) + ixy * ixy).sqrt();
    Some((a, (imin.max(0.0) / a).sqrt()))
}

/// Allowable force of a section, N: tension `A f`, compression by Rankine over length `l`.
pub fn allowable(name: &str, l: f64, compression: bool, r: &Loads) -> f64 {
    let Some((a, rg)) = props(name) else { return 0.0 };
    let f = a * r.f_allow_mpa * 1e6;
    if compression {
        f / (1.0 + (l / rg.max(1e-9)).powi(2) / r.rankine_a)
    } else {
        f
    }
}

/// The first section of `ladder` whose allowable force reaches `n`; the last one (and `false`)
/// when none does.
pub fn pick<'a>(ladder: &'a [String], n: f64, l: f64, compression: bool, r: &Loads) -> (&'a str, bool) {
    match ladder.iter().find(|s| allowable(s, l, compression, r) >= n) {
        Some(s) => (s, true),
        None => (ladder.last().map_or("", String::as_str), false),
    }
}

/// Wind shear (N), overturning moment (N m) and dead load (N) at a cut at height `z`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cut {
    pub v: f64,
    pub m: f64,
    pub g: f64,
}

/// Floor area of platform `i`; the first one is a ring round an open centre.
pub fn deck_area(p: &TowerParams, i: usize) -> f64 {
    let b = 2.0 * p.w(p.platforms[i]);
    if i == 0 {
        let hole = (b - 2.0 * p.ring1).max(0.0);
        b * b - hole * hole
    } else {
        b * b
    }
}

pub fn cut(p: &TowerParams, r: &Loads, z: f64) -> Cut {
    let q = r.wind_kpa * 1e3 * r.face_factor;
    let n = ((p.height - z) / 0.5).ceil().max(1.0) as usize;
    let dz = (p.height - z) / n as f64;
    let (mut v, mut m) = (0.0, 0.0);
    for i in 0..n {
        let zz = z + dz * (i as f64 + 0.5);
        let f = q * 2.0 * p.w(zz) * dz;
        v += f;
        m += f * (zz - z);
    }
    let decks: f64 = (0..3).filter(|&i| p.platforms[i] > z).map(|i| deck_area(p, i)).sum();
    Cut { v, m, g: r.self_kn_m * 1e3 * (p.height - z) + r.deck_kpa * 1e3 * decks }
}

/// Side of a leg box at height `z` (the legs merge into the shaft at the second platform).
pub fn leg_side(p: &TowerParams, z: f64) -> f64 {
    let p2 = p.platforms[1];
    p.leg0 + (p.w(p2) - p.leg0) * z / p2
}

/// Sections of one height of the tower lattice and how many of them reach their force.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sizing {
    pub chord: &'static str,
    pub diag: &'static str,
    pub strut: &'static str,
    /// Chord force, N (compression).
    pub chord_n: f64,
    /// Roles whose largest section still falls short (0..3).
    pub over: usize,
}

/// Sections of the legs (below the second platform) or the shaft at height `z`: chords take the
/// dead load and the overturning moment on the leeward side, each X diagonal the whole shear
/// of its face in tension, a horizontal a share of the chord force in compression.
pub fn size_at(p: &TowerParams, r: &Loads, z: f64) -> Sizing {
    let lad = Ladders::builtin();
    let c = cut(p, r, z);
    let w = p.w(z);
    let legs = z < p.platforms[1];
    let (chord_n, face_v, b, panel) = if legs {
        let s = leg_side(p, z);
        // two legs on the leeward side, four chords a leg; eight faces along the wind
        let lever = (2.0 * (w - s / 2.0)).max(s);
        (c.g / 16.0 + c.m / lever / 8.0, c.v / 8.0, s, p.panel_leg)
    } else {
        (c.g / 4.0 + c.m / (2.0 * w) / 2.0, c.v / 2.0, 2.0 * w, p.panel_shaft)
    };
    // X: one diagonal over the face width, in tension; K: two to the strut middle, one pushed
    let x = matches!(p.pattern, super::lattice::Pattern::X);
    let span = if x { b } else { b / 2.0 };
    let ld = span.hypot(panel);
    let diag_n = if x { face_v * ld / b } else { face_v / 2.0 * ld / span };
    let (chord, ok1) = pick(&lad.chord, chord_n, panel, true, r);
    let (diag, ok2) = pick(&lad.brace, diag_n, ld, !x, r);
    let (strut, ok3) = pick(&lad.strut, r.strut_share * chord_n, span, true, r);
    Sizing { chord, diag, strut, chord_n, over: [ok1, ok2, ok3].iter().filter(|o| !**o).count() }
}

/// Top sway of the equivalent cantilever, m, and the number of 1 m cuts with a role over its
/// largest section: chords as flanges (second moment sum of A x2), unit-load integral of
/// M m / (E I); shear strain left out.
pub fn sway(p: &TowerParams, r: &Loads) -> (f64, usize) {
    let n = p.height.ceil().max(1.0) as usize;
    let dz = p.height / n as f64;
    let e = r.e_gpa * 1e9;
    let (mut d, mut over) = (0.0, 0);
    for i in 0..n {
        let z = dz * (i as f64 + 0.5);
        let s = size_at(p, r, z);
        over += usize::from(s.over > 0);
        let a = props(s.chord).map_or(0.0, |x| x.0);
        let w = p.w(z);
        let inertia = if z < p.platforms[1] {
            let side = leg_side(p, z);
            a * (8.0 * w * w + 8.0 * (w - side).powi(2))
        } else {
            4.0 * a * w * w
        };
        d += cut(p, r, z).m * (p.height - z) / (e * inertia.max(1e-12)) * dz;
    }
    (d, over)
}
