// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `create_grids` arguments: two families of grid lines from a count, spacing, labelling,
//! an angle and an origin. Lengths are metres, angles degrees.

use crate::grid_set::{Family, GridSet};
use serde_json::Value as J;

fn num(v: &J, what: &str) -> Result<f64, String> {
    v.as_f64()
        .filter(|x| x.is_finite())
        .ok_or_else(|| format!("{what} must be a number"))
}

/// A gap, or a list of gaps (the last one repeats).
fn gaps(v: &J, what: &str) -> Result<Vec<f64>, String> {
    match v {
        J::Array(a) => a.iter().map(|x| num(x, what)).collect(),
        J::Null => Ok(vec![]),
        one => Ok(vec![num(one, what)?]),
    }
}

fn family(v: &J, what: &str, default_start: &str) -> Result<Family, String> {
    if v.is_null() {
        return Ok(Family::new(0, 1.0, default_start));
    }
    if !v.is_object() {
        return Err(format!("{what} must be an object"));
    }
    let count = match &v["count"] {
        J::Null => return Err(format!("{what}.count is needed")),
        c => c
            .as_u64()
            .ok_or_else(|| format!("{what}.count must be a whole number"))? as usize,
    };
    let start = match (v["start"].as_str(), v["labels"].as_str()) {
        (Some(s), _) => s.trim().to_string(),
        (None, Some("numbers")) => "1".to_string(),
        (None, Some("letters")) => "A".to_string(),
        (None, Some("letters_lower")) => "a".to_string(),
        (None, Some(other)) => {
            return Err(format!(
                "{what}.labels is numbers, letters or letters_lower, got: {}",
                crate::ai::fmt::text(other)
            ))
        }
        (None, None) => default_start.to_string(),
    };
    Ok(Family {
        count,
        spacing: gaps(&v["spacing"], &format!("{what}.spacing"))?,
        start,
        skew: match &v["skew"] {
            J::Null => 0.0,
            s => num(s, &format!("{what}.skew"))?,
        },
    })
}

/// The set an argument object describes. `x` lines are numbered 1, 2, 3 and `y` lines lettered
/// A, B, C unless the labelling says otherwise.
pub fn set_of(a: &J) -> Result<GridSet, String> {
    let origin = match &a["origin"] {
        J::Null => (0.0, 0.0),
        J::Array(p) if p.len() == 2 => (num(&p[0], "origin")?, num(&p[1], "origin")?),
        _ => return Err("origin must be [x, y] in metres".to_string()),
    };
    Ok(GridSet {
        origin,
        angle: match &a["angle"] {
            J::Null => 0.0,
            v => num(v, "angle")?,
        },
        overhang: match &a["overhang"] {
            J::Null => 1.5,
            v => num(v, "overhang")?,
        },
        x: family(&a["x"], "x", "1")?,
        y: family(&a["y"], "y", "A")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_number_x_and_letter_y() {
        let s = set_of(&json!({
            "x": {"count": 4, "spacing": 6},
            "y": {"count": 3, "spacing": [5, 4.5]},
            "angle": 30, "origin": [10, -2]
        }))
        .unwrap();
        assert_eq!((s.x.count, s.x.start.as_str(), s.x.spacing.clone()), (4, "1", vec![6.0]));
        assert_eq!((s.y.count, s.y.start.as_str(), s.y.spacing.clone()), (3, "A", vec![5.0, 4.5]));
        assert_eq!((s.angle, s.origin, s.overhang), (30.0, (10.0, -2.0), 1.5));
    }

    #[test]
    fn labelling_words_and_explicit_starts() {
        let s = set_of(&json!({
            "x": {"count": 2, "spacing": 5, "labels": "letters_lower"},
            "y": {"count": 2, "spacing": 5, "labels": "numbers", "start": "7"}
        }))
        .unwrap();
        assert_eq!((s.x.start.as_str(), s.y.start.as_str()), ("a", "7"));
        let e = set_of(&json!({"x": {"count": 2, "spacing": 5, "labels": "roman"}})).unwrap_err();
        assert!(e.contains("roman"), "{e}");
    }

    #[test]
    fn a_family_may_be_left_out_and_bad_values_are_named() {
        let s = set_of(&json!({"y": {"count": 2, "spacing": 3}})).unwrap();
        assert_eq!(s.x.count, 0);
        for bad in [
            json!({"x": {"spacing": 3}}),
            json!({"x": {"count": -1, "spacing": 3}}),
            json!({"x": {"count": 2, "spacing": "wide"}}),
            json!({"x": {"count": 2, "spacing": 3, "skew": "a"}}),
            json!({"x": 5}),
            json!({"x": {"count": 2, "spacing": 3}, "origin": [1]}),
            json!({"x": {"count": 2, "spacing": 3}, "angle": "up"}),
        ] {
            assert!(set_of(&bad).is_err(), "{bad}");
        }
    }
}
