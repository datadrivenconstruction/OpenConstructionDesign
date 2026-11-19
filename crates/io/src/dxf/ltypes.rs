//! Linetype table support for the model bridge: standard patterns by name and
//! a completeness pass so no layer or entity references an undefined LTYPE.
// the completeness pass serves only the test-only DXF writer (`dxf-write`)
#![cfg_attr(not(any(test, feature = "dxf-write")), allow(dead_code))]

use super::{DxfDocument, LineType};

/// Names the writer always defines itself.
pub(super) fn is_builtin(name: &str) -> bool {
    ["bylayer", "byblock", "continuous"]
        .iter()
        .any(|b| name.eq_ignore_ascii_case(b))
}

/// Description and dash pattern (positive = dash, negative = gap) of a well-known name.
/// Suffix `X2` doubles and suffix `2` halves the pattern.
pub(super) fn standard(name: &str) -> Option<(&'static str, Vec<f64>)> {
    let up = name.trim().to_ascii_uppercase();
    let (base, k) = if let Some(b) = up.strip_suffix("X2") {
        (b, 2.0)
    } else if let Some(b) = up.strip_suffix('2') {
        (b, 0.5)
    } else {
        (up.as_str(), 1.0)
    };
    let (d, p): (&str, &[f64]) = match base {
        "DASHED" => ("Dashed __ __ __", &[0.5, -0.25]),
        "HIDDEN" => ("Hidden __ __ __", &[0.25, -0.125]),
        "CENTER" => ("Center ____ _ ____ _", &[1.25, -0.25, 0.25, -0.25]),
        "PHANTOM" => (
            "Phantom ______  __  __",
            &[1.25, -0.25, 0.25, -0.25, 0.25, -0.25],
        ),
        "DASHDOT" => ("Dash dot __ . __ .", &[0.5, -0.25, 0.0, -0.25]),
        "DOT" => ("Dot . . . .", &[0.0, -0.25]),
        "DIVIDE" => (
            "Divide ____ . . ____",
            &[0.5, -0.25, 0.0, -0.25, 0.0, -0.25],
        ),
        "BORDER" => (
            "Border __ __ . __ __ .",
            &[0.5, -0.25, 0.5, -0.25, 0.0, -0.25],
        ),
        _ => return None,
    };
    Some((d, p.iter().map(|v| v * k).collect()))
}

/// Total pattern length (DXF group 40).
pub(super) fn length(pattern: &[f64]) -> f64 {
    pattern.iter().map(|v| v.abs()).sum()
}

/// Adds a record for every name in `used` that the document does not define:
/// a standard pattern for known names, otherwise a solid (empty pattern) record.
pub(super) fn complete<'a>(doc: &mut DxfDocument, used: impl IntoIterator<Item = &'a str>) {
    for name in used {
        let name = name.trim();
        if name.is_empty()
            || is_builtin(name)
            || doc.ltypes.iter().any(|t| t.name.eq_ignore_ascii_case(name))
        {
            continue;
        }
        let (description, pattern) = match standard(name) {
            Some((d, p)) => (d.to_string(), p),
            None => ("Solid line".to_string(), vec![]),
        };
        doc.ltypes.push(LineType {
            name: name.to_string(),
            description,
            flags: 0,
            pattern_length: length(&pattern),
            pattern,
            extra: vec![],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_and_scaled_names() {
        assert_eq!(standard("dashed").unwrap().1, vec![0.5, -0.25]);
        assert_eq!(standard("HIDDENX2").unwrap().1, vec![0.5, -0.25]);
        assert_eq!(standard("DASHED2").unwrap().1, vec![0.25, -0.125]);
        assert!(standard("MY_CUSTOM").is_none());
    }
}
