// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Barrel roofs. A roof asked as `barrel` is a profile drawn along the long axis of its
//! outline's tightest box: a half circle, a segment of a circle or a half ellipse across the
//! short side, by `massing.vault_profile` (`auto` takes the half circle up to
//! `massing.vault_arc_span` and the segment beyond, rising `massing.vault_rise` of the span).
//! The vault covers its box, so an outline filling less than `massing.vault_fill` of it keeps
//! its gable roof. The profile is cut into chords within `massing.arc_tolerance`.

use super::{dot, perp, sub, unit, P};
use crate::project::footprint::RoofForm;
use crate::project::rules::rule;
use crate::project::{GProject, PRoof, PVault, ProjectSpec};
use std::f64::consts::PI;

/// Chords of one profile at most.
const MAX_CHORDS: usize = 64;
/// Chords of one profile at least.
const MIN_CHORDS: usize = 4;

/// The tightest box of a ring over its edge directions: long axis, short axis, the ranges
/// along each.
fn bbox(ring: &[P]) -> Option<(P, P, (f64, f64), (f64, f64))> {
    let n = ring.len();
    let range = |d: P| ring.iter().map(|q| dot(*q, d)).fold((f64::MAX, f64::MIN), |r, x| (r.0.min(x), r.1.max(x)));
    let mut best: Option<(f64, P, P, (f64, f64), (f64, f64))> = None;
    for i in 0..n {
        let e = sub(ring[(i + 1) % n], ring[i]);
        if e.0.hypot(e.1) < 1e-9 {
            continue;
        }
        let (u, v) = (unit(e), perp(unit(e)));
        let (ru, rv) = (range(u), range(v));
        let area = (ru.1 - ru.0) * (rv.1 - rv.0);
        if best.as_ref().is_none_or(|b| area < b.0 - 1e-9) {
            let (l, c, rl, rc) = if ru.1 - ru.0 >= rv.1 - rv.0 { (u, v, ru, rv) } else { (v, u, rv, ru) };
            best = Some((area, l, c, rl, rc));
        }
    }
    best.map(|b| (b.1, b.2, b.3, b.4))
}

/// Points of the curve `f` from parameter `t0` to `t1`, chords within the arc tolerance of a
/// curve whose largest radius of curvature is `big`.
fn chords(f: impl Fn(f64) -> P, t0: f64, t1: f64, big: f64) -> Vec<P> {
    let tol = rule("massing.arc_tolerance").min(big);
    let step = 2.0 * (1.0 - tol / big).clamp(-1.0, 1.0).acos();
    let n = (((t1 - t0).abs() / step.max(1e-9)).ceil() as usize).clamp(MIN_CHORDS, MAX_CHORDS);
    (0..=n).map(|i| f(t0 + (t1 - t0) * i as f64 / n as f64)).collect()
}

/// The vault's profile across a span: `(u, z)` from `(0, 0)` to `(span, 0)`.
pub(crate) fn profile(span: f64) -> Vec<P> {
    let asked = rule("massing.vault_profile").round() as usize;
    let form = match asked {
        0 if span <= rule("massing.vault_arc_span") => 1,
        0 => 2,
        k => k,
    };
    let rise = rule("massing.vault_rise") * span;
    let half = span / 2.0;
    let mut pts = match form {
        1 => chords(|t| (half + half * t.cos(), half * t.sin()), PI, 0.0, half),
        2 => {
            let r = (rise * rise + half * half) / (2.0 * rise);
            let a = (half / r).clamp(-1.0, 1.0).asin();
            let cz = rise - r;
            chords(|t| (half + r * t.cos(), cz + r * t.sin()), PI / 2.0 + a, PI / 2.0 - a, r)
        }
        _ => {
            let big = (half * half / rise).max(rise * rise / half);
            chords(|t| (half + half * t.cos(), rise * t.sin()), PI, 0.0, big)
        }
    };
    // the springing points exactly on the eaves
    let n = pts.len();
    pts[0] = (0.0, 0.0);
    pts[n - 1] = (span, 0.0);
    pts
}

fn ring_area(r: &[P]) -> f64 {
    let n = r.len();
    (0..n).map(|i| r[i].0 * r[(i + 1) % n].1 - r[(i + 1) % n].0 * r[i].1).sum::<f64>().abs() / 2.0
}

/// The vault over one roof, or `None` when its outline fills too little of its box.
fn vault(r: &PRoof) -> Option<PVault> {
    let (l, c, rl, rc) = bbox(&r.ring)?;
    let (depth, span) = (rl.1 - rl.0, rc.1 - rc.0);
    if depth * span < 1e-9 || ring_area(&r.ring) < rule("massing.vault_fill") * depth * span {
        return None;
    }
    // a to b across the box, drawn to its left, which is the long axis
    let d = (l.1, -l.0);
    let m = if dot(d, c) > 0.0 { rc.0 } else { -rc.1 };
    let a = (l.0 * rl.0 + d.0 * m, l.1 * rl.0 + d.1 * m);
    let b = (a.0 + d.0 * span, a.1 + d.1 * span);
    Some(PVault { a, b, profile: profile(span), depth })
}

/// Turns every roof of a barrel brief into a vault where the rules allow; returns how many
/// roofs became vaults and how many stayed gables.
pub(crate) fn apply(spec: &ProjectSpec, p: &mut GProject) -> (usize, usize) {
    if spec.roof_form != Some(RoofForm::Barrel) {
        return (0, 0);
    }
    let (mut made, mut kept) = (0, 0);
    for r in std::iter::once(&mut p.roof).chain(&mut p.extra_roofs) {
        r.vault = vault(r);
        if r.vault.is_some() {
            made += 1;
        } else {
            kept += 1;
        }
    }
    p.metrics.push(("vaults", made as f64));
    (made, kept)
}
