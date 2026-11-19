//! Offset surface by sampling and refit: every control point moves along the surface normal
//! at its Greville parameters; the result is checked against S + d n on a sample grid and the
//! net is refined (midpoint knots) until the deviation is within the tolerance. Self-
//! intersection of the offset (curvature radius below |d|) is refused, not repaired.

use super::{NurbsError, Surface};

/// Offset surface and its measured deviation from the true offset.
#[derive(Clone, Debug)]
pub struct Offset {
    pub surface: Surface,
    pub max_dev: f64,
    pub converged: bool,
}

const ROUNDS: usize = 6;
/// Cap on the refined control net.
const MAX_CTRL: usize = 40_000;
/// Samples per knot span and direction for the deviation check.
const SAMPLES: usize = 4;

fn moved(s: &Surface, d: f64) -> Result<Surface, NurbsError> {
    let (gu, gv) = (s.greville_u(), s.greville_v());
    let nv = s.nv();
    let mut ctrl = s.ctrl().to_vec();
    for (i, &u) in gu.iter().enumerate() {
        for (j, &v) in gv.iter().enumerate() {
            let n = s.normal(u, v).ok_or(NurbsError::NoNormal)?;
            ctrl[i * nv + j] = ctrl[i * nv + j] + n * d;
        }
    }
    Surface::new(
        s.deg_u(),
        s.deg_v(),
        s.knots_u().to_vec(),
        s.knots_v().to_vec(),
        s.nu(),
        nv,
        ctrl,
        s.weights().to_vec(),
    )
}

fn samples(knots: &[f64], p: usize, n: usize) -> Vec<f64> {
    let mut d: Vec<f64> = knots[p..=n].to_vec();
    d.dedup();
    let mut out = vec![d[0]];
    for w in d.windows(2) {
        for k in 1..=SAMPLES {
            out.push(w[0] + (w[1] - w[0]) * k as f64 / SAMPLES as f64);
        }
    }
    out
}

/// Largest |O(u, v) - (S(u, v) + d n(u, v))| over the sample grid of `o`'s spans.
fn deviation(s: &Surface, o: &Surface, d: f64) -> Result<f64, NurbsError> {
    let us = samples(o.knots_u(), o.deg_u(), o.nu());
    let vs = samples(o.knots_v(), o.deg_v(), o.nv());
    let mut dev = 0.0f64;
    for &u in &us {
        for &v in &vs {
            let n = s.normal(u, v).ok_or(NurbsError::NoNormal)?;
            dev = dev.max((o.point(u, v) - (s.point(u, v) + n * d)).len());
        }
    }
    Ok(dev)
}

/// Principal curvature check: refuses when |d| reaches the smallest radius of curvature on
/// the side the offset goes to (the offset would fold over).
pub fn check_offset(s: &Surface, d: f64) -> Result<(), NurbsError> {
    let us = samples(s.knots_u(), s.deg_u(), s.nu());
    let vs = samples(s.knots_v(), s.deg_v(), s.nv());
    for &u in &us {
        for &v in &vs {
            let dd = s.derivs(u, v, 2);
            let Some(n) = s.normal(u, v) else { continue };
            let (su, sv) = (dd[1][0], dd[0][1]);
            let (e, f, g) = (su.dot(su), su.dot(sv), sv.dot(sv));
            let (l, m, nn) = (dd[2][0].dot(n), dd[1][1].dot(n), dd[0][2].dot(n));
            let den = e * g - f * f;
            // a pole (the apex of a dome) has no first-order metric: its curvature is noise
            if !(den > 1e-12 * (e + g).powi(2)) {
                continue;
            }
            let h = (e * nn - 2.0 * f * m + g * l) / (2.0 * den);
            let k = (l * nn - m * m) / den;
            let disc = (h * h - k).max(0.0).sqrt();
            for kappa in [h + disc, h - disc] {
                // x + d n has the metric factor 1 - d * kappa (kappa = II / I, II along +n)
                if 1.0 - d * kappa <= 1e-9 {
                    return Err(NurbsError::BadThickness);
                }
            }
        }
    }
    Ok(())
}

/// Offset of `s` by `d` metres along its normal, within `tol` if the refinement allows.
pub fn offset(s: &Surface, d: f64, tol: f64) -> Result<Offset, NurbsError> {
    if !d.is_finite() || !(tol > 0.0) {
        return Err(NurbsError::BadThickness);
    }
    if d == 0.0 {
        return Ok(Offset { surface: s.clone(), max_dev: 0.0, converged: true });
    }
    check_offset(s, d)?;
    let mut base = s.clone();
    let mut best: Option<Offset> = None;
    for _ in 0..ROUNDS {
        let o = moved(&base, d)?;
        let dev = deviation(s, &o, d)?;
        let done = dev <= tol;
        best = Some(Offset { surface: o, max_dev: dev, converged: done });
        if done || base.nu() * base.nv() * 4 > MAX_CTRL {
            break;
        }
        base = base.refined_once()?;
    }
    best.ok_or(NurbsError::Degenerate)
}
