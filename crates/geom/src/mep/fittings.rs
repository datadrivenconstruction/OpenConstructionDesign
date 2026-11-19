// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Pressure loss of duct fittings, water flow in pipes, and fan and pump power.
//! Duct fittings: smooth round elbows by bend radius and angle, diverging and converging tees
//! (Idelchik form), contractions and conical expansions. Pipes: Darcy-Weisbach with the
//! Swamee-Jain friction factor, water properties by temperature.

use super::PipeSize;

/// Specific heat of air, J/(kg K).
pub const CP_AIR: f64 = 1005.0;
/// Specific heat of water, J/(kg K).
pub const CP_WATER: f64 = 4186.0;
/// Absolute roughness of commercial steel pipe, m.
pub const ROUGH_STEEL: f64 = 4.5e-5;
/// Standard motor ratings, kW.
pub const MOTOR_KW: [f64; 24] = [
    0.12, 0.18, 0.25, 0.37, 0.55, 0.75, 1.1, 1.5, 2.2, 3.0, 4.0, 5.5, 7.5, 11.0, 15.0, 18.5, 22.0,
    30.0, 37.0, 45.0, 55.0, 75.0, 90.0, 110.0,
];

/// Smooth round 90 degree elbow: loss coefficient by bend radius over diameter; r/D = 0 is a
/// mitred elbow.
const ELBOW_90: [(f64, f64); 7] = [
    (0.0, 1.2),
    (0.5, 0.71),
    (0.75, 0.33),
    (1.0, 0.22),
    (1.5, 0.15),
    (2.0, 0.13),
    (2.5, 0.12),
];
/// Factor on the 90 degree coefficient by the turn angle, degrees.
const ELBOW_ANGLE: [(f64, f64); 11] = [
    (0.0, 0.0),
    (20.0, 0.31),
    (30.0, 0.45),
    (45.0, 0.6),
    (60.0, 0.78),
    (75.0, 0.9),
    (90.0, 1.0),
    (110.0, 1.13),
    (130.0, 1.2),
    (150.0, 1.28),
    (180.0, 1.4),
];

/// Linear interpolation in a table sorted by x, clamped at its ends.
pub fn interp(t: &[(f64, f64)], x: f64) -> f64 {
    let (first, last) = (t[0], t[t.len() - 1]);
    if x <= first.0 {
        return first.1;
    }
    if x >= last.0 {
        return last.1;
    }
    for w in t.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        if x <= x1 {
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    last.1
}

/// Elbow loss coefficient, by its own velocity pressure: bend radius over the duct dimension
/// in the plane of the turn, and the turn angle in degrees.
pub fn elbow_zeta(r_d: f64, angle_deg: f64) -> f64 {
    interp(&ELBOW_90, r_d) * interp(&ELBOW_ANGLE, angle_deg)
}

/// Diverging 90 degree tee, coefficients by the upstream velocity pressure: (branch, straight).
/// Branch `a * (1 + (vb/vc)^2)`, `a` = 1 for a sharp edge; straight `0.4 * (1 - vs/vc)^2`.
pub fn tee_diverging(vc: f64, vb: f64, vs: f64, a: f64) -> (f64, f64) {
    if vc <= 0.0 {
        return (0.0, 0.0);
    }
    let (xb, xs) = (vb / vc, vs / vc);
    (a * (1.0 + xb * xb), 0.4 * (1.0 - xs).powi(2))
}

/// Converging 90 degree tee, coefficients by the downstream (common) velocity pressure:
/// branch `a * (1 + (vb/vc)^2 - 2 (1 - qb/qc)^2)`, straight `1.55 qb/qc - (qb/qc)^2`.
pub fn tee_converging(vc: f64, vb: f64, qb_qc: f64, a: f64) -> (f64, f64) {
    if vc <= 0.0 {
        return (0.0, 0.0);
    }
    let xb = vb / vc;
    let q = qb_qc.clamp(0.0, 1.0);
    let branch = a * (1.0 + xb * xb - 2.0 * (1.0 - q).powi(2));
    (branch.max(0.0), (1.55 * q - q * q).max(0.0))
}

/// Contraction from area `a1` to the smaller `a2` with the given half-angle (degrees), by the
/// downstream velocity pressure: abrupt `0.5 (1 - a2/a1)^0.75`, a taper of 30 degrees or less
/// a tenth of it, linear between.
pub fn contraction_zeta(a1: f64, a2: f64, half_deg: f64) -> f64 {
    if a1 <= 0.0 || a2 >= a1 {
        return 0.0;
    }
    let abrupt = 0.5 * (1.0 - a2 / a1).powf(0.75);
    let f = interp(&[(30.0, 0.1), (90.0, 1.0)], half_deg);
    abrupt * f
}

/// Conical expansion from area `a1` to the larger `a2` with the given half-angle (degrees), by
/// the upstream velocity pressure: `phi (1 - a1/a2)^2`, `phi = 3.2 tan(half)^1.25` up to 1.
pub fn expansion_zeta(a1: f64, a2: f64, half_deg: f64) -> f64 {
    if a2 <= 0.0 || a1 >= a2 {
        return 0.0;
    }
    let phi = (3.2 * half_deg.to_radians().tan().powf(1.25)).min(1.0);
    phi * (1.0 - a1 / a2).powi(2)
}

const WATER: [(f64, f64, f64); 11] = [
    // temperature C, density kg/m3, kinematic viscosity m2/s
    (0.0, 999.8, 1.787e-6),
    (10.0, 999.7, 1.306e-6),
    (20.0, 998.2, 1.004e-6),
    (30.0, 995.7, 0.801e-6),
    (40.0, 992.2, 0.658e-6),
    (50.0, 988.0, 0.553e-6),
    (60.0, 983.2, 0.475e-6),
    (70.0, 977.8, 0.413e-6),
    (80.0, 971.8, 0.365e-6),
    (90.0, 965.3, 0.326e-6),
    (100.0, 958.4, 0.294e-6),
];

/// Density of water, kg/m3, at `t` C.
pub fn water_rho(t: f64) -> f64 {
    let tab: Vec<(f64, f64)> = WATER.iter().map(|w| (w.0, w.1)).collect();
    interp(&tab, t)
}

/// Kinematic viscosity of water, m2/s, at `t` C.
pub fn water_nu(t: f64) -> f64 {
    let tab: Vec<(f64, f64)> = WATER.iter().map(|w| (w.0, w.2)).collect();
    interp(&tab, t)
}

/// Velocity, m/s, and friction loss, Pa/m, of `q` m3/h of water at `t` C in a pipe of inner
/// diameter `id`: laminar 64/Re, else Swamee-Jain.
pub fn pipe_friction(q: f64, id: f64, t: f64, rough: f64) -> (f64, f64) {
    if q <= 0.0 || id <= 0.0 {
        return (0.0, 0.0);
    }
    let v = q / 3600.0 / (std::f64::consts::PI * id * id / 4.0);
    let re = v * id / water_nu(t);
    let f = if re < 2300.0 {
        64.0 / re
    } else {
        0.25 / ((rough / (3.7 * id) + 5.74 / re.powf(0.9)).log10()).powi(2)
    };
    (v, f / id * water_rho(t) * v * v / 2.0)
}

/// Smallest pipe from `dn_min` up whose friction is at most `r_max` Pa/m and velocity at most
/// `v_max(dn)`; the largest when none is.
pub fn pick_pipe(
    q: f64,
    t: f64,
    r_max: f64,
    v_max: impl Fn(u32) -> f64,
    sizes: &[PipeSize],
    dn_min: u32,
) -> PipeSize {
    let mut s: Vec<PipeSize> = sizes.iter().filter(|s| s.dn >= dn_min).copied().collect();
    s.sort_by_key(|s| s.dn);
    s.iter()
        .find(|s| {
            let (v, r) = pipe_friction(q, s.id(), t, ROUGH_STEEL);
            r <= r_max && v <= v_max(s.dn)
        })
        .or(s.last())
        .copied()
        .unwrap_or(sizes[sizes.len() - 1])
}

/// Shaft power, kW, moving `q` m3/h against `dp` Pa at efficiency `eff`.
pub fn shaft_kw(q: f64, dp: f64, eff: f64) -> f64 {
    q / 3600.0 * dp / eff.max(1e-3) / 1000.0
}

/// The smallest standard motor of at least `kw`; the largest rating when none is.
pub fn motor_kw(kw: f64) -> f64 {
    MOTOR_KW.iter().copied().find(|m| *m + 1e-9 >= kw).unwrap_or(MOTOR_KW[MOTOR_KW.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mep::PipeMaterial;

    fn close(a: f64, b: f64, rel: f64) -> bool {
        (a - b).abs() <= rel * b.abs().max(1e-12)
    }

    #[test]
    fn elbow_follows_the_table() {
        assert!(close(elbow_zeta(1.5, 90.0), 0.15, 1e-9));
        assert!(close(elbow_zeta(1.0, 45.0), 0.22 * 0.6, 1e-9));
        // between table rows: r/D 1.25 is the mean of 0.22 and 0.15
        assert!(close(elbow_zeta(1.25, 90.0), 0.185, 1e-9));
        assert!(elbow_zeta(0.0, 90.0) > elbow_zeta(0.5, 90.0));
    }

    #[test]
    fn tees_and_transitions() {
        // branch at the upstream velocity: 1 + 1 = 2; straight unchanged velocity: no loss
        let (b, s) = tee_diverging(5.0, 5.0, 5.0, 1.0);
        assert!(close(b, 2.0, 1e-9) && s.abs() < 1e-12);
        let (b, s) = tee_converging(5.0, 2.5, 0.3, 1.0);
        assert!(close(b, 1.0 + 0.25 - 2.0 * 0.49, 1e-9), "{b}");
        assert!(close(s, 1.55 * 0.3 - 0.09, 1e-9));
        // 15 degree half-angle, area ratio 1/2: phi = 3.2 * tan(15)^1.25 = 0.6168, x 0.25
        assert!(close(expansion_zeta(0.5, 1.0, 15.0), 0.6168 * 0.25, 2e-3));
        assert!(close(contraction_zeta(1.0, 0.5, 15.0), 0.1 * 0.5 * 0.5f64.powf(0.75), 1e-9));
        assert_eq!(contraction_zeta(0.5, 1.0, 15.0), 0.0);
    }

    #[test]
    fn water_pipe_matches_hand_calculation() {
        // DN50 steel (id 52.48 mm), 5 m3/h at 10 C: v = 0.642 m/s, Re = 25800,
        // Swamee-Jain f = 0.0265, R = 0.0265 / 0.05248 * 999.7 * 0.642^2 / 2 = 104 Pa/m
        let s = PipeMaterial::Steel.by_dn(50).unwrap();
        let (v, r) = pipe_friction(5.0, s.id(), 10.0, ROUGH_STEEL);
        assert!(close(v, 0.642, 5e-3), "{v}");
        assert!(close(r, 104.0, 0.03), "{r}");
        let pick = pick_pipe(5.0, 10.0, 250.0, |_| 1.0, &PipeMaterial::Steel.sizes(), 15);
        // DN40 (id 40.94): v = 1.055 m/s over the 1.0 cap, so DN50
        assert_eq!(pick.dn, 50);
        assert_eq!(motor_kw(2.3), 3.0);
        assert!(close(shaft_kw(3600.0, 500.0, 0.5), 1.0, 1e-9));
    }
}
