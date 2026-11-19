//! Reading chip at the cursor while drawing: length, absolute angle and the turn relative to the
//! previous segment. Pure numbers and text; the plan view only places the plate.

use crate::units::{fmt_num_lang, format_length, Units};
use ddc_geom::V2;

/// Canvas chip height (design system 2.21); label plates are never lower.
pub const CHIP_H: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub len: f64,
    /// Direction from the segment start, degrees in [0, 360).
    pub angle: f64,
    /// Turn from the previous segment, degrees in (-180, 180]; None for the first segment.
    pub turn: Option<f64>,
}

fn dir_deg(a: V2, b: V2) -> f64 {
    let d = b - a;
    d.y.atan2(d.x).to_degrees().rem_euclid(360.0)
}

/// Reading of the rubber segment `a -> b`; `prev` is the point before `a`.
pub fn reading(prev: Option<V2>, a: V2, b: V2) -> Option<Reading> {
    let len = (b - a).len();
    if !len.is_finite() || len < 1e-9 {
        return None;
    }
    let angle = dir_deg(a, b);
    let turn = prev.filter(|p| (a - *p).len() > 1e-9).map(|p| {
        let mut t = angle - dir_deg(p, a);
        while t > 180.0 {
            t -= 360.0;
        }
        while t <= -180.0 {
            t += 360.0;
        }
        t
    });
    Some(Reading { len, angle, turn })
}

fn deg(v: f64, ru: bool) -> String {
    let v = if v.abs() < 0.05 { 0.0 } else { v };
    let n = fmt_num_lang(v, 1, ru);
    format!("{n}°")
}

/// Chip text: `length · angle`, plus `(turn)` when there is a previous segment.
pub fn chip_text(r: &Reading, units: Units, ru: bool) -> String {
    let mut s = format!("{} · {}", format_length(r.len, units), deg(r.angle, ru));
    if let Some(t) = r.turn {
        s.push_str(&format!("  (Δ {})", deg(t, ru)));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::v2;

    #[test]
    fn length_and_angle() {
        let r = reading(None, v2(0.0, 0.0), v2(0.0, 2.0)).unwrap();
        assert!((r.len - 2.0).abs() < 1e-12 && (r.angle - 90.0).abs() < 1e-9);
        assert!(r.turn.is_none());
        let r = reading(None, v2(0.0, 0.0), v2(-1.0, -1.0)).unwrap();
        assert!((r.angle - 225.0).abs() < 1e-9);
    }

    #[test]
    fn turn_is_signed_and_wraps() {
        let left = reading(Some(v2(-1.0, 0.0)), v2(0.0, 0.0), v2(0.0, 1.0)).unwrap();
        assert!((left.turn.unwrap() - 90.0).abs() < 1e-9);
        let right = reading(Some(v2(-1.0, 0.0)), v2(0.0, 0.0), v2(0.0, -1.0)).unwrap();
        assert!((right.turn.unwrap() + 90.0).abs() < 1e-9);
        // heading 350 deg then 10 deg: a 20 deg turn, not -340
        let a = v2(0.0, 0.0);
        let p = v2(-(10f64.to_radians().cos()), 10f64.to_radians().sin());
        let b = v2(10f64.to_radians().cos(), 10f64.to_radians().sin());
        let w = reading(Some(p), a, b).unwrap();
        assert!((w.turn.unwrap() - 20.0).abs() < 1e-9, "{:?}", w.turn);
    }

    #[test]
    fn degenerate_segment_has_no_reading() {
        assert!(reading(None, v2(1.0, 1.0), v2(1.0, 1.0)).is_none());
        assert!(reading(None, v2(0.0, 0.0), v2(f64::NAN, 1.0)).is_none());
    }

    #[test]
    fn text_uses_project_units_and_locale_decimals() {
        let r = reading(Some(v2(-1.0, 0.0)), v2(0.0, 0.0), v2(3.0, 0.0)).unwrap();
        let ru = chip_text(&r, Units::Metric, true);
        let en = chip_text(&r, Units::Metric, false);
        assert!(ru.contains("0,0°"), "{ru}");
        assert!(en.contains("0.0°"), "{en}");
        let imp = chip_text(&r, Units::Imperial, false);
        assert_ne!(imp, en);
    }
}
