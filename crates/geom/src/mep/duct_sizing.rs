//! Air duct sizing by equal friction and pressure loss of a duct path.
//! Friction: Darcy-Weisbach, friction factor by the Altshul formula (steel duct, k = 0.15 mm).

/// Air density, kg/m3.
pub const RHO: f64 = 1.2;
/// Kinematic viscosity of air, m2/s.
pub const NU: f64 = 1.5e-5;
/// Absolute roughness of galvanised steel duct, m.
pub const ROUGH: f64 = 0.00015;
/// Loss coefficient of a smooth radius elbow (R/D = 1.5).
pub const ZETA_ELBOW: f64 = 0.25;
/// Largest rectangular aspect ratio.
pub const MAX_ASPECT: f64 = 4.0;
/// Standard rectangular duct sides, m.
/// Standard round duct diameters, mm.
pub const ROUND_MM: [u32; 12] = [80, 100, 125, 160, 200, 250, 315, 400, 500, 630, 800, 1000];
pub const RECT_STEPS: [f64; 16] = [
    0.1, 0.15, 0.2, 0.25, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.2, 1.4, 1.6, 2.0,
];

/// Velocity, m/s, of `q` m3/h through `area` m2.
pub fn velocity(q: f64, area: f64) -> f64 {
    q / 3600.0 / area
}

/// Velocity pressure, Pa.
pub fn dynamic(v: f64) -> f64 {
    RHO * v * v / 2.0
}

/// Friction loss, Pa/m, of `q` m3/h through a section of `area` and hydraulic diameter `dh`.
pub fn friction(q: f64, dh: f64, area: f64) -> f64 {
    if q <= 0.0 || dh <= 0.0 || area <= 0.0 {
        return 0.0;
    }
    let v = velocity(q, area);
    let re = v * dh / NU;
    let lambda = 0.11 * (ROUGH / dh + 68.0 / re).powf(0.25);
    lambda / dh * dynamic(v)
}

pub fn friction_round(q: f64, d: f64) -> f64 {
    friction(q, d, std::f64::consts::PI * d * d / 4.0)
}

pub fn friction_rect(q: f64, w: f64, h: f64) -> f64 {
    friction(q, 2.0 * w * h / (w + h), w * h)
}

/// Smallest catalog diameter (m) with friction <= `pa_m` and velocity <= `v_max`, not below `min_d`.
pub fn pick_round(q: f64, pa_m: f64, v_max: f64, min_d: f64, catalog_mm: &[u32]) -> f64 {
    catalog_mm
        .iter()
        .map(|d| *d as f64 / 1000.0)
        .find(|&d| {
            d + 1e-9 >= min_d
                && friction_round(q, d) <= pa_m
                && velocity(q, std::f64::consts::PI * d * d / 4.0) <= v_max
        })
        .unwrap_or_else(|| *catalog_mm.last().unwrap_or(&1000) as f64 / 1000.0)
}

/// Rectangular section (w, h), m, w >= h, aspect <= `MAX_ASPECT`, h <= `h_max`: least area meeting
/// the friction and velocity limits; ties go to the squarer section.
pub fn pick_rect(q: f64, pa_m: f64, v_max: f64, h_max: f64, min_side: f64) -> (f64, f64) {
    let mut best: Option<(f64, f64, f64)> = None;
    for &h in RECT_STEPS.iter().filter(|h| **h <= h_max + 1e-9 && **h + 1e-9 >= min_side) {
        for &w in RECT_STEPS.iter().filter(|w| **w + 1e-9 >= h && **w <= h * MAX_ASPECT + 1e-9) {
            if friction_rect(q, w, h) > pa_m || velocity(q, w * h) > v_max {
                continue;
            }
            let key = w * h + (w / h) * 1e-6;
            if best.is_none_or(|b| key < b.0) {
                best = Some((key, w, h));
            }
        }
    }
    best.map_or((2.0, h_max.clamp(min_side, 2.0)), |b| (b.1, b.2))
}

/// Pressure loss, Pa, along each root-ward path: `parent[i]` is the piece piece `i` drains into,
/// `loss[i]` its own loss. Returns the largest path loss and the pieces on it from leaf to root.
pub fn critical_path(parent: &[Option<usize>], loss: &[f64]) -> (f64, Vec<usize>) {
    let mut best = (0.0, vec![]);
    for leaf in 0..parent.len() {
        let mut path = vec![];
        let mut sum = 0.0;
        let mut at = Some(leaf);
        while let Some(i) = at {
            if path.len() > parent.len() {
                break;
            }
            path.push(i);
            sum += loss[i];
            at = parent[i];
        }
        if sum > best.0 + 1e-12 {
            best = (sum, path);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAT: [u32; 12] = [80, 100, 125, 160, 200, 250, 315, 400, 500, 630, 800, 1000];

    fn close(a: f64, b: f64, rel: f64) -> bool {
        (a - b).abs() <= rel * b.abs()
    }

    #[test]
    fn friction_matches_hand_calculation() {
        // D315, 1000 m3/h: v = 3.564 m/s, Re = 74850, lambda = 0.02122, R = 0.5134 Pa/m
        assert!(close(friction_round(1000.0, 0.315), 0.5134, 0.005));
        // D250: v = 5.659, Re = 94300, lambda = 0.02097, R = 1.611 Pa/m
        assert!(close(friction_round(1000.0, 0.25), 1.611, 0.005));
        assert_eq!(friction_round(0.0, 0.2), 0.0);
    }

    #[test]
    fn equal_friction_picks_315_for_1000_at_1_pa() {
        assert!(close(pick_round(1000.0, 1.0, 8.0, 0.0, &CAT), 0.315, 1e-9));
        // a looser rate allows the smaller size, a velocity cap forbids it
        assert!(close(pick_round(1000.0, 2.0, 8.0, 0.0, &CAT), 0.25, 1e-9));
        assert!(close(pick_round(1000.0, 2.0, 5.0, 0.0, &CAT), 0.315, 1e-9));
        // never below the terminal neck
        assert!(close(pick_round(100.0, 1.0, 8.0, 0.25, &CAT), 0.25, 1e-9));
    }

    #[test]
    fn rect_keeps_aspect_and_height_limit() {
        for q in [300.0, 1000.0, 3000.0, 8000.0] {
            let (w, h) = pick_rect(q, 1.0, 8.0, 0.4, 0.1);
            assert!(w >= h && w / h <= MAX_ASPECT + 1e-9, "{q}: {w}x{h}");
            assert!(h <= 0.4 + 1e-9);
            assert!(friction_rect(q, w, h) <= 1.0);
            assert!(velocity(q, w * h) <= 8.0);
        }
        // 1000 m3/h at 1 Pa/m, height held at 200: 400x200 gives 0.603 Pa/m, 300x200 fails
        assert!(friction_rect(1000.0, 0.3, 0.2) > 1.0);
        let (w, h) = pick_rect(1000.0, 1.0, 8.0, 0.2, 0.2);
        assert!(close(w, 0.4, 1e-9) && close(h, 0.2, 1e-9), "{w}x{h}");
        // free height under 200: 500x150 (0.075 m2) beats 400x200 (0.08 m2)
        let (w, h) = pick_rect(1000.0, 1.0, 8.0, 0.2, 0.1);
        assert!(close(w, 0.5, 1e-9) && close(h, 0.15, 1e-9), "{w}x{h}");
        let (w, h) = pick_rect(1000.0, 1.0, 8.0, 0.2, 0.2);
        assert!(close(w, 0.4, 1e-9) && close(h, 0.2, 1e-9), "{w}x{h}");
        assert!(close(friction_rect(1000.0, 0.4, 0.2), 0.6027, 0.005));
    }

    #[test]
    fn three_segment_path_loss_matches_hand_calculation() {
        // 1200 m3/h D315 10 m (0.7183 Pa/m) + 800 D250 6 m (1.0650) + 400 D200 4 m (0.8932)
        // + two elbows on D200 (v = 3.537 m/s, pd = 7.505 Pa, zeta 0.25): 20.90 Pa by hand
        let seg = [(1200.0, 0.315, 10.0), (800.0, 0.25, 6.0), (400.0, 0.2, 4.0)];
        let mut loss: Vec<f64> = seg.iter().map(|(q, d, l)| friction_round(*q, *d) * l).collect();
        let v = velocity(400.0, std::f64::consts::PI * 0.04 / 4.0);
        loss[2] += 2.0 * ZETA_ELBOW * dynamic(v);
        // a short side branch from the main must not be the critical one
        loss.push(1.0);
        let parent = [None, Some(0), Some(1), Some(0)];
        let (pa, path) = critical_path(&parent, &loss);
        assert!(close(pa, 20.90, 0.02), "{pa}");
        assert_eq!(path, vec![2, 1, 0]);
    }
}
