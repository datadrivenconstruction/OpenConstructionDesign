//! Grids and reference planes in plan: straight or arc (a, b, bulge = tan(angle/4)), intersections
//! for snapping, automatic name numbering (1, 2, ... / A ... Z, AA ...).

use crate::math::{v2, V2};

/// Grid line: a segment or a circular arc from `a` to `b`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve {
    pub a: V2,
    pub b: V2,
    pub bulge: f64,
}

impl Curve {
    pub fn line(a: V2, b: V2) -> Self {
        Curve { a, b, bulge: 0.0 }
    }

    /// Center, radius, start angle and sweep (sign is the direction) for an arc.
    pub fn arc(&self) -> Option<(V2, f64, f64, f64)> {
        if self.bulge == 0.0 {
            return None;
        }
        let sweep = 4.0 * self.bulge.atan();
        let d = self.b - self.a;
        let c = d.len();
        let r = c / (2.0 * (sweep / 2.0).sin()).abs();
        let m = (self.a + self.b) * 0.5;
        let n = v2(-d.y, d.x) * (1.0 / c);
        // center is to the left of the chord when bulge > 0 and |angle| < π
        let h = (r * r - c * c / 4.0).max(0.0).sqrt();
        let side = if (sweep.abs() < std::f64::consts::PI) == (sweep > 0.0) {
            1.0
        } else {
            -1.0
        };
        let ctr = m + n * (h * side);
        let a0 = (self.a.y - ctr.y).atan2(self.a.x - ctr.x);
        Some((ctr, r, a0, sweep))
    }

    pub fn length(&self) -> f64 {
        match self.arc() {
            Some((_, r, _, s)) => r * s.abs(),
            None => (self.b - self.a).len(),
        }
    }

    /// Point at fraction t ∈ [0, 1].
    pub fn at(&self, t: f64) -> V2 {
        match self.arc() {
            Some((c, r, a0, s)) => {
                let a = a0 + s * t;
                c + v2(a.cos(), a.sin()) * r
            }
            None => self.a + (self.b - self.a) * t,
        }
    }

    /// Midpoint (for an arc, on the arc).
    pub fn mid(&self) -> V2 {
        self.at(0.5)
    }

    /// Fraction t of a point p lying on the line (extension of a straight line is allowed).
    fn param(&self, p: V2) -> f64 {
        match self.arc() {
            Some((c, _, a0, s)) => {
                let a = (p.y - c.y).atan2(p.x - c.x);
                let tau = 2.0 * std::f64::consts::PI;
                let da = if s > 0.0 {
                    (a - a0).rem_euclid(tau)
                } else {
                    -(a0 - a).rem_euclid(tau)
                };
                da / s
            }
            None => {
                let d = self.b - self.a;
                (p - self.a).dot(d) / d.dot(d)
            }
        }
    }

    /// Closest point on the line (within its endpoints) and the distance to it.
    pub fn nearest(&self, p: V2) -> (V2, f64) {
        let q = match self.arc() {
            Some((c, r, _, _)) => {
                let v = p - c;
                let l = v.len();
                let on = if l > 0.0 { c + v * (r / l) } else { self.a };
                if (0.0..=1.0).contains(&self.param(on)) {
                    on
                } else if (p - self.a).len() <= (p - self.b).len() {
                    self.a
                } else {
                    self.b
                }
            }
            None => self.at(self.param(p).clamp(0.0, 1.0)),
        };
        (q, (p - q).len())
    }

    /// Polyline for display: a segment gives 2 points, an arc uses chords no longer than `step`.
    pub fn points(&self, step: f64) -> Vec<V2> {
        let n = if self.bulge == 0.0 {
            1
        } else {
            ((self.length() / step.max(1e-6)).ceil() as usize).clamp(2, 512)
        };
        (0..=n).map(|i| self.at(i as f64 / n as f64)).collect()
    }
}

const EPS: f64 = 1e-9;

/// Intersection points of two lines (within endpoints, parameter tolerance 1e-9).
pub fn intersections(p: &Curve, q: &Curve) -> Vec<V2> {
    let cand: Vec<V2> = match (p.arc(), q.arc()) {
        (None, None) => {
            let (d, e) = (p.b - p.a, q.b - q.a);
            let den = d.cross(e);
            if den.abs() < 1e-15 {
                vec![]
            } else {
                vec![p.a + d * ((q.a - p.a).cross(e) / den)]
            }
        }
        (Some((c, r, _, _)), None) => line_circle(q.a, q.b, c, r),
        (None, Some((c, r, _, _))) => line_circle(p.a, p.b, c, r),
        (Some((c1, r1, _, _)), Some((c2, r2, _, _))) => circle_circle(c1, r1, c2, r2),
    };
    cand.into_iter()
        .filter(|x| {
            let (tp, tq) = (p.param(*x), q.param(*x));
            (-EPS..=1.0 + EPS).contains(&tp) && (-EPS..=1.0 + EPS).contains(&tq)
        })
        .collect()
}

fn line_circle(a: V2, b: V2, c: V2, r: f64) -> Vec<V2> {
    let d = b - a;
    let f = a - c;
    let (qa, qb, qc) = (d.dot(d), 2.0 * f.dot(d), f.dot(f) - r * r);
    let disc = qb * qb - 4.0 * qa * qc;
    if disc < 0.0 || qa == 0.0 {
        return vec![];
    }
    let s = disc.sqrt();
    let mut v = vec![a + d * ((-qb - s) / (2.0 * qa))];
    if s > 0.0 {
        v.push(a + d * ((-qb + s) / (2.0 * qa)));
    }
    v
}

fn circle_circle(c1: V2, r1: f64, c2: V2, r2: f64) -> Vec<V2> {
    let d = c2 - c1;
    let l = d.len();
    if l == 0.0 || l > r1 + r2 || l < (r1 - r2).abs() {
        return vec![];
    }
    let a = (r1 * r1 - r2 * r2 + l * l) / (2.0 * l);
    let h = (r1 * r1 - a * a).max(0.0).sqrt();
    let m = c1 + d * (a / l);
    let n = v2(-d.y, d.x) * (1.0 / l);
    if h == 0.0 {
        vec![m]
    } else {
        vec![m + n * h, m - n * h]
    }
}

/// Next grid name: "7" -> "8", "A" -> "B", "Z" -> "AA", "B2" -> "B3"; anything else gives None.
pub fn next_name(prev: &str) -> Option<String> {
    let digits = prev.len() - prev.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits > 0 {
        let (head, num) = prev.split_at(prev.len() - digits);
        let n: u64 = num.parse().ok()?;
        return Some(format!("{head}{}", n + 1));
    }
    if prev.is_empty() || !prev.chars().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    // letter counting: A..Z, AA..ZZ, ...
    let mut b: Vec<u8> = prev.bytes().collect();
    let mut i = b.len();
    loop {
        if i == 0 {
            b.insert(0, b'A');
            break;
        }
        i -= 1;
        if b[i] == b'Z' {
            b[i] = b'A';
        } else {
            b[i] += 1;
            break;
        }
    }
    String::from_utf8(b).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(next_name("1").as_deref(), Some("2"));
        assert_eq!(next_name("9").as_deref(), Some("10"));
        assert_eq!(next_name("A").as_deref(), Some("B"));
        assert_eq!(next_name("Z").as_deref(), Some("AA"));
        assert_eq!(next_name("AZ").as_deref(), Some("BA"));
        assert_eq!(next_name("B2").as_deref(), Some("B3"));
        assert_eq!(next_name("\u{0430}"), None);
    }

    #[test]
    fn line_line_and_arc() {
        let p = Curve::line(v2(0.0, -1.0), v2(0.0, 5.0));
        let q = Curve::line(v2(-1.0, 2.0), v2(9.0, 2.0));
        assert_eq!(intersections(&p, &q), vec![v2(0.0, 2.0)]);
        // semicircle of radius 5 about the origin, counter-clockwise: (5,0) -> (-5,0), bulge = tan(π/4) = 1
        let arc = Curve {
            a: v2(5.0, 0.0),
            b: v2(-5.0, 0.0),
            bulge: 1.0,
        };
        let (c, r, _, s) = arc.arc().unwrap();
        assert!(
            c.len() < 1e-12 && (r - 5.0).abs() < 1e-12 && (s - std::f64::consts::PI).abs() < 1e-12
        );
        assert!((arc.mid() - v2(0.0, 5.0)).len() < 1e-12);
        let x = intersections(&p, &arc);
        assert_eq!(x.len(), 1);
        assert!((x[0] - v2(0.0, 5.0)).len() < 1e-12);
        let vert = Curve::line(v2(0.0, -9.0), v2(0.0, 9.0));
        let x = intersections(&vert, &arc);
        assert_eq!(x.len(), 1);
        assert!((x[0] - v2(0.0, 5.0)).len() < 1e-12);
        assert!((arc.length() - 5.0 * std::f64::consts::PI).abs() < 1e-12);
        let (n, d) = arc.nearest(v2(0.0, 7.0));
        assert!((n - v2(0.0, 5.0)).len() < 1e-12 && (d - 2.0).abs() < 1e-12);
    }
}
