//! Outline of a tapered or curved tower: half-width over height, either the closed rule
//! `w(z) = a + b·exp(-z/k)` or a user spline through `(z, w)` points (monotone cubic, so the
//! leg never bulges between the points the user gave).

#[derive(Clone, Debug, PartialEq)]
pub enum Profile {
    Exp { a: f64, b: f64, k: f64 },
    /// Points with strictly rising z; held constant outside the first and last point.
    Spline(Vec<(f64, f64)>),
}

impl Profile {
    pub fn check(&self) -> Result<(), String> {
        match self {
            Profile::Exp { a, b, k } => {
                if !(a.is_finite() && b.is_finite() && k.is_finite()) || *a <= 0.0 || *b < 0.0 || *k <= 0.0 {
                    return Err("profile: a > 0, b >= 0, k > 0".into());
                }
            }
            Profile::Spline(p) => {
                if p.len() < 2 {
                    return Err("profile: a spline needs at least two (z, w) points".into());
                }
                if p.iter().any(|q| !q.0.is_finite() || !q.1.is_finite() || q.1 <= 0.0) {
                    return Err("profile: every half-width must be a positive number".into());
                }
                if p.windows(2).any(|w| w[1].0 - w[0].0 < 1e-3) {
                    return Err("profile: spline heights must rise strictly".into());
                }
            }
        }
        Ok(())
    }

    /// Half-width at height z.
    pub fn w(&self, z: f64) -> f64 {
        match self {
            Profile::Exp { a, b, k } => a + b * (-z / k).exp(),
            Profile::Spline(p) => hermite(p, z),
        }
    }

    /// Reads `[[z, w], ...]` (metres).
    pub fn from_points(v: &[(f64, f64)]) -> Result<Profile, String> {
        let p = Profile::Spline(v.to_vec());
        p.check()?;
        Ok(p)
    }
}

/// Fritsch-Carlson monotone cubic Hermite interpolation.
pub(crate) fn hermite(p: &[(f64, f64)], z: f64) -> f64 {
    let n = p.len();
    if z <= p[0].0 {
        return p[0].1;
    }
    if z >= p[n - 1].0 {
        return p[n - 1].1;
    }
    let d: Vec<f64> = p.windows(2).map(|w| (w[1].1 - w[0].1) / (w[1].0 - w[0].0)).collect();
    let mut m = vec![0.0; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = if d[i - 1] * d[i] <= 0.0 { 0.0 } else { (d[i - 1] + d[i]) / 2.0 };
    }
    for i in 0..n - 1 {
        if d[i] == 0.0 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
            continue;
        }
        let (a, b) = (m[i] / d[i], m[i + 1] / d[i]);
        let s = a * a + b * b;
        if s > 9.0 {
            let t = 3.0 / s.sqrt();
            m[i] = t * a * d[i];
            m[i + 1] = t * b * d[i];
        }
    }
    let i = p.windows(2).position(|w| z < w[1].0).unwrap_or(n - 2);
    let h = p[i + 1].0 - p[i].0;
    let t = (z - p[i].0) / h;
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * t3 - 3.0 * t2 + 1.0) * p[i].1 + (t3 - 2.0 * t2 + t) * h * m[i] + (-2.0 * t3 + 3.0 * t2) * p[i + 1].1 + (t3 - t2) * h * m[i + 1]
}
