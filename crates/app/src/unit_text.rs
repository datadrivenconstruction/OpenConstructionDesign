//! Lengths, areas and volumes for tool hints in the project's units and the UI language.
//! Thin wrappers over the `units` `*_lang` formatters, which already pick the Russian or the
//! English separators and unit symbols.

use crate::units::{display_length_lang, fmt_area_in_lang, fmt_volume_lang, Units};

/// Length with its unit: «6 000 mm in Russian form» / `6,000 mm` / feet-inches.
// display_length_lang already writes the English style; a second pass made 6,000 into 6.000
pub fn len(m: f64, ru: bool, u: Units) -> String {
    display_length_lang(m, u, ru)
}

/// Area with its unit: «12,35 m2 in Russian form» / `12.35 m²` / `ft²`.
pub fn area(m2: f64, ru: bool, u: Units) -> String {
    fmt_area_in_lang(m2, u, ru)
}

/// Volume with its unit: «1,234 m3 in Russian form» / `1.234 m³` / `ft³`.
pub fn vol(m3: f64, ru: bool, u: Units) -> String {
    fmt_volume_lang(m3, u, crate::units::precision().1, ru)
}

/// Area in the active project units and the current UI language.
pub fn area_ui(m2: f64) -> String {
    area(m2, crate::ui_is_ru(), crate::units::active_units())
}

/// Volume in the active project units and the current UI language.
pub fn vol_ui(m3: f64) -> String {
    vol(m3, crate::ui_is_ru(), crate::units::active_units())
}

/// One decimal count for a block that lists areas and volumes together: the finer of the two
/// user precisions, at most two, so "4.00 m²" sits above "12.00 m³".
pub fn summary_decimals() -> usize {
    let (a, v) = crate::units::precision();
    a.max(v).min(2)
}

/// Area with `d` decimals in the active units and UI language.
pub fn area_ui_d(m2: f64, d: usize) -> String {
    area_d(m2, d, crate::ui_is_ru(), crate::units::active_units())
}

/// Volume with `d` decimals in the active units and UI language.
pub fn vol_ui_d(m3: f64, d: usize) -> String {
    vol_d(m3, d, crate::ui_is_ru(), crate::units::active_units())
}

// the formatters already follow `ru` (number style and symbols): no `loc` pass on top
fn area_d(m2: f64, d: usize, ru: bool, u: Units) -> String {
    crate::units::fmt_area_lang(m2, u, d, false, ru)
}

fn vol_d(m3: f64, d: usize, ru: bool, u: Units) -> String {
    fmt_volume_lang(m3, u, d, ru)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_area_and_volume_share_decimals() {
        let d = summary_decimals();
        let a = area_ui_d(4.0, d);
        let v = vol_ui_d(12.0, d);
        let dec = |s: &str| {
            let num = s.split('\u{a0}').next().unwrap_or("");
            num.rsplit_once(['.', ','])
                .map_or(0, |(_, f)| f.chars().filter(char::is_ascii_digit).count())
        };
        assert_eq!(dec(&a), dec(&v), "{a} vs {v}");
    }

    #[test]
    fn summary_shows_at_most_two_decimals() {
        assert!(summary_decimals() <= 2);
        let s = area_ui_d(4.0, summary_decimals());
        let num = s.split('\u{a0}').next().unwrap_or("");
        let frac = num.rsplit_once(['.', ',']).map_or(0, |(_, f)| f.len());
        assert!(frac <= 2, "{s}");
    }

    #[test]
    fn summary_large_values_keep_one_number_style() {
        let m = Units::Metric;
        assert_eq!(area_d(1234.5, 3, false, m), "1,234.500\u{a0}m\u{b2}");
        assert_eq!(vol_d(1234.5, 3, false, m), "1,234.500\u{a0}m\u{b3}");
        assert_eq!(area_d(1234.5, 3, true, m), "1\u{a0}234,500\u{a0}\u{43c}\u{b2}");
        assert_eq!(vol_d(1234.5, 3, true, m), "1\u{a0}234,500\u{a0}\u{43c}\u{b3}");
    }

    #[test]
    fn layer_volume_follows_ui_lang() {
        let en = vol(0.123, false, Units::Metric);
        let ru = vol(0.123, true, Units::Metric);
        assert_eq!(en, "0.123\u{a0}m\u{b3}");
        assert!(ru.contains('\u{43c}'));
    }

    #[test]
    fn area_and_volume_en_vs_ru_explicit_lang() {
        assert_eq!(area(12.5, false, Units::Metric), "12.50\u{a0}m\u{b2}");
        assert_eq!(vol(1234.5, false, Units::Metric), "1,234.500\u{a0}m\u{b3}");
        assert_eq!(area(12.5, true, Units::Metric), "12,50\u{a0}\u{43c}\u{b2}");
        assert_eq!(vol(1.0, true, Units::Metric), "1,000\u{a0}\u{43c}\u{b3}");
    }

    #[test]
    fn english_units_have_no_cyrillic() {
        for s in [
            len(6.0, false, Units::Metric),
            len(0.25, false, Units::Metric),
            area(12.5, false, Units::Metric),
            vol(1.234, false, Units::Metric),
        ] {
            assert!(!s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)), "{s}");
        }
        assert_eq!(len(6.0, false, Units::Metric), "6,000\u{a0}mm");
        assert_eq!(len(6.0, true, Units::Metric), "6\u{a0}000\u{a0}\u{43c}\u{43c}");
        assert_eq!(len(0.25, false, Units::Metric), "250\u{a0}mm");
    }
}
