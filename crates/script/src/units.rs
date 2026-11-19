// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Quantities written as text with a unit: `"1200 mm"`, `"1.2m"`, `"30 deg"`. Scripts compute
//! in metres and radians; nothing is implied by display settings.

use serde_json::Value as J;

const LEN_UNITS: [(&str, f64); 5] = [
    ("mm", 0.001),
    ("cm", 0.01),
    ("m", 1.0),
    ("ft", 0.3048),
    ("in", 0.0254),
];
const ANGLE_UNITS: [(&str, f64); 2] = [("deg", std::f64::consts::PI / 180.0), ("rad", 1.0)];

fn split(s: &str) -> Option<(f64, &str)> {
    let t = s.trim();
    let b = t.as_bytes();
    let mut end = 0;
    while end < b.len() {
        let c = b[end];
        let sign = matches!(c, b'-' | b'+') && (end == 0 || matches!(b[end - 1], b'e' | b'E'));
        // an exponent only when a digit or sign follows: "2em" is not a number
        let exp = matches!(c, b'e' | b'E')
            && end > 0
            && b.get(end + 1)
                .is_some_and(|n| n.is_ascii_digit() || matches!(n, b'-' | b'+'));
        if c.is_ascii_digit() || c == b'.' || sign || exp {
            end += 1;
        } else {
            break;
        }
    }
    let (num, unit) = t.split_at(end);
    let v: f64 = num.parse().ok()?;
    v.is_finite().then_some((v, unit.trim()))
}

fn parse(s: &str, table: &[(&str, f64)], what: &str) -> Result<f64, String> {
    let (v, unit) = split(s).ok_or_else(|| format!("\"{s}\" is not a {what} with a unit"))?;
    if unit.is_empty() {
        let names: Vec<&str> = table.iter().map(|u| u.0).collect();
        return Err(format!(
            "\"{s}\" has no unit; add one of {}",
            names.join(", ")
        ));
    }
    table
        .iter()
        .find(|(n, _)| *n == unit)
        .map(|(_, k)| v * k)
        .ok_or_else(|| format!("\"{s}\": unknown {what} unit `{unit}`"))
}

/// Length in metres.
pub fn parse_len(s: &str) -> Result<f64, String> {
    parse(s, &LEN_UNITS, "length")
}

/// Angle in radians.
pub fn parse_angle(s: &str) -> Result<f64, String> {
    parse(s, &ANGLE_UNITS, "angle")
}

/// Kind of a quantity written as text: length, angle or neither.
pub(crate) fn dim_of(s: &str) -> Option<&'static str> {
    if parse_len(s).is_ok() {
        Some("m")
    } else if parse_angle(s).is_ok() {
        Some("rad")
    } else {
        None
    }
}

/// A param value given by the caller, converted to the SI unit `unit` (`m`, `rad`).
pub(crate) fn param_si(v: &J, unit: &str) -> Result<f64, String> {
    let text = match v {
        J::String(s) => s.clone(),
        J::Object(o) => {
            let n = o
                .get("v")
                .and_then(J::as_f64)
                .ok_or("{v, unit}: v must be a number")?;
            let u = o
                .get("unit")
                .and_then(J::as_str)
                .ok_or("{v, unit}: unit must be text")?;
            format!("{n} {u}")
        }
        J::Number(n) if n.as_f64() == Some(0.0) => return Ok(0.0),
        _ => return Err(format!("expected text with a unit like \"12 {unit}\"")),
    };
    if unit == "m" {
        parse_len(&text)
    } else {
        parse_angle(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_and_angles() {
        assert_eq!(parse_len("1200 mm").unwrap(), 1.2);
        assert_eq!(parse_len("1.5m").unwrap(), 1.5);
        assert!((parse_len("-2 ft").unwrap() + 0.6096).abs() < 1e-12);
        assert!(parse_len("12").unwrap_err().contains("no unit"));
        assert!(parse_len("3 yd").unwrap_err().contains("unknown"));
        assert!((parse_angle("180 deg").unwrap() - std::f64::consts::PI).abs() < 1e-12);
        assert_eq!(dim_of("30 deg"), Some("rad"));
        assert_eq!(dim_of("abc"), None);
    }
}
