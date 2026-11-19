// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Every number the UI puts into an editable field reads back through the matching parser
//! to the same value (within the precision shown), in both decimal marks and both unit systems.
//! No test changes the global UI language: marks and languages are always passed explicitly.
//!
//! Pairs covered (formatter -> parser):
//! - `edit_length_den_mark` / `edit_length_mark` -> `edit_text_canonical` + `parse_signed_unbounded`
//!   (signed, unbounded) and `parse_edit_length_lang` (the field parser, positive lengths).
//! - `fmt_num` / `fmt_num_en` (grouping, 0..6 decimals) -> `parse_locale_number`.
//! - `format_length_den` / `display_length_lang` -> length parsers.
//! - `fmt_area_lang` / `fmt_volume_lang` / `fmt_percent`: number part -> `parse_locale_number`.
//!   These carry a unit suffix, so they are display-only; the number part is still checked.
//!   Metric area/volume/percent follow the language (ru: comma, no-break groups; en: point,
//!   comma groups); imperial ones are English-style always.
//! - `fmt_fixed_mark` (link offsets) -> `parse_locale_number` with the same mark.

use crate::expr_field::Scope;
use crate::units::*;

const MARKS: [DecimalMark; 2] = [DecimalMark::Comma, DecimalMark::Point];
/// 0, negative, sub-unit fraction, grouped large, very large (metres or square metres).
/// 0.001234 m is 1.234 mm: in the English mark it must not read as a thousands group.
const VALUES: [f64; 10] = [0.0, -0.5, 0.0015, 1234.5678, 1e6, -1234.5678, -1e6, 12.3456789, 0.001, 0.001234];

fn near(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol + 1e-12 * b.abs()
}

/// Number of digits after the decimal mark in a formatted number.
fn shown_decimals(s: &str, mark: DecimalMark) -> i32 {
    let dec = if mark == DecimalMark::Comma { ',' } else { '.' };
    s.rsplit_once(dec).map_or(0, |(_, f)| f.len() as i32)
}

#[test]
fn metric_edit_text_reads_back_signed_unbounded() {
    for mark in MARKS {
        for v in VALUES {
            let s = edit_length_den_mark(v, Units::Metric, 16, mark);
            let canon = edit_text_canonical(&s, mark).unwrap_or_else(|e| panic!("{s:?} {mark:?}: {e:?}"));
            let got = parse_signed_unbounded(&canon, Units::Metric).unwrap();
            // millimetres are shown with up to 3 decimals
            assert!(near(got, v, 5e-7), "{v} -> {s:?} -> {got} ({mark:?})");
        }
    }
}

#[test]
fn metric_edit_text_has_marks_of_its_locale() {
    let ru = edit_length_den_mark(1234.5678, Units::Metric, 16, DecimalMark::Comma);
    assert_eq!(ru, "1\u{a0}234\u{a0}567,8");
    let en = edit_length_den_mark(1234.5678, Units::Metric, 16, DecimalMark::Point);
    assert_eq!(en, "1,234,567.8");
    // untouched zero and a sub-millimetre dust never show as "-0"
    for mark in MARKS {
        assert_eq!(edit_length_den_mark(-1e-9, Units::Metric, 16, mark), "0");
        assert_eq!(edit_length_den_mark(0.0, Units::Metric, 16, mark), "0");
    }
}

#[test]
fn field_parser_reads_positive_metric_text_back() {
    let sc = Scope::none();
    for mark in MARKS {
        for v in [0.0015, 0.5, 6.0, 12.3456789, 999.9] {
            let s = edit_length_mark(v, Units::Metric, mark);
            let got = parse_edit_length_lang(&s, Units::Metric, &sc, mark).unwrap_or_else(|e| panic!("{s:?}: {e:?}"));
            assert!(near(got, v, 5e-7), "{v} -> {s:?} -> {got} ({mark:?})");
        }
    }
}

#[test]
fn imperial_edit_text_reads_back_within_one_fraction_step() {
    let sc = Scope::none();
    for mark in MARKS {
        for den in [2usize, 8, 16, 64] {
            let tol = INCH / (2.0 * den as f64) * 1.0001;
            for v in VALUES {
                let s = edit_length_den_mark(v, Units::Imperial, den, mark);
                let canon = edit_text_canonical(&s, mark).unwrap_or_else(|e| panic!("{s:?} {mark:?}: {e:?}"));
                let got = parse_signed_unbounded(&canon, Units::Imperial).unwrap();
                assert!(near(got, v, tol), "{v} -> {s:?} -> {got} (den {den}, {mark:?})");
            }
            for v in [0.02, 0.5, 3.0, 12.3456789, 300.0] {
                let s = edit_length_den_mark(v, Units::Imperial, den, mark);
                let got = parse_edit_length_lang(&s, Units::Imperial, &sc, mark).unwrap_or_else(|e| panic!("{s:?}: {e:?}"));
                assert!(near(got, v, tol), "{v} -> {s:?} -> {got} (den {den}, {mark:?})");
            }
        }
    }
}

#[test]
fn imperial_text_is_the_same_in_both_marks() {
    for v in VALUES {
        assert_eq!(
            edit_length_den_mark(v, Units::Imperial, 16, DecimalMark::Comma),
            edit_length_den_mark(v, Units::Imperial, 16, DecimalMark::Point)
        );
    }
}

#[test]
fn plain_numbers_with_groups_read_back_in_their_mark() {
    for d in 0..=6usize {
        let tol = 0.5 * 10f64.powi(-(d as i32)) * 1.0001;
        for v in VALUES {
            let ru = fmt_num_ru(v, d);
            let en = fmt_num_en(v, d);
            let a = parse_locale_number(&ru, DecimalMark::Comma).unwrap_or_else(|e| panic!("{ru:?}: {e:?}"));
            let b = parse_locale_number(&en, DecimalMark::Point).unwrap_or_else(|e| panic!("{en:?}: {e:?}"));
            assert!(near(a, v, tol), "{v} -> {ru:?} -> {a}");
            assert!(near(b, v, tol), "{v} -> {en:?} -> {b}");
        }
    }
}

#[test]
fn point_text_without_groups_reads_in_both_marks() {
    // link offsets are printed as `{:.3}`; a Russian user still reads them back unchanged
    for v in VALUES {
        let s = format!("{v:.3}");
        for mark in MARKS {
            let got = parse_locale_number(&s, mark).unwrap_or_else(|e| panic!("{s:?} {mark:?}: {e:?}"));
            assert!(near(got, v, 5e-4), "{v} -> {s:?} -> {got} ({mark:?})");
        }
    }
}

#[test]
fn display_length_number_part_reads_back() {
    for ru in [true, false] {
        for v in VALUES {
            let s = display_length_lang(v, Units::Metric, ru);
            let (num, _unit) = s.rsplit_once('\u{a0}').unwrap();
            // the number part is whole millimetres in the language's own number style
            let mark = if ru { DecimalMark::Comma } else { DecimalMark::Point };
            let mm = parse_locale_number(num, mark).unwrap_or_else(|e| panic!("{s:?}: {e:?}"));
            let got = mm / 1000.0;
            assert!(near(got, v, 0.5e-3 * 1.0001), "{v} -> {s:?} -> {got}");
        }
    }
    for v in VALUES {
        // explicit denominator: the global one may be changed by other tests
        let s = format_length_den(v, Units::Imperial, 16);
        let got = parse_signed_unbounded(&s, Units::Imperial).unwrap();
        assert!(near(got, v, INCH / 32.0 * 1.0001), "{v} -> {s:?} -> {got}");
    }
}

#[test]
fn area_volume_percent_number_part_reads_back() {
    for ru in [true, false] {
        for u in [Units::Metric, Units::Imperial] {
            for small_in2 in [false, true] {
                for d in [2usize, 3] {
                    for v in VALUES.into_iter().chain([0.05, -0.05]) {
                        let a = fmt_area_lang(v, u, d, small_in2, ru);
                        let (num, unit) = a.rsplit_once('\u{a0}').unwrap();
                        // number style follows the language (imperial is English-style always)
                        let mark = if u == Units::Metric && ru { DecimalMark::Comma } else { DecimalMark::Point };
                        let got = parse_locale_number(num, mark).unwrap_or_else(|e| panic!("{a:?}: {e:?}"));
                        let want = if unit == "in\u{b2}" { v / (INCH * INCH) } else { area_in_units(v, u) };
                        let tol = 0.5 * 10f64.powi(-shown_decimals(num, mark)) * 1.0001;
                        assert!(near(got, want, tol), "{v} -> {a:?} -> {got} want {want}");

                        let m = fmt_volume_lang(v, u, d, ru);
                        let (num, _) = m.rsplit_once('\u{a0}').unwrap();
                        let got = parse_locale_number(num, mark).unwrap_or_else(|e| panic!("{m:?}: {e:?}"));
                        let tol = 0.5 * 10f64.powi(-shown_decimals(num, mark)) * 1.0001;
                        assert!(near(got, volume_in_units(v, u), tol), "{v} -> {m:?} -> {got}");
                    }
                }
            }
        }
    }
    for p in [0.0, -0.5, 2.0, 12.34, 1234.5] {
        for (ru, mark) in [(true, DecimalMark::Comma), (false, DecimalMark::Point)] {
            let s = fmt_percent_lang(p, ru);
            let (num, _) = s.rsplit_once('\u{a0}').unwrap();
            let got = parse_locale_number(num, mark).unwrap();
            assert!(near(got, p, 0.05 * 1.0001), "{p} -> {s:?} -> {got}");
        }
    }
}

#[test]
fn metric_area_volume_percent_follow_language() {
    let m = Units::Metric;
    assert_eq!(fmt_area_lang(1234.5678, m, 2, false, true), "1\u{a0}234,57\u{a0}\u{43c}\u{b2}");
    assert_eq!(fmt_area_lang(1234.5678, m, 2, false, false), "1,234.57\u{a0}m\u{b2}");
    assert_eq!(fmt_volume_lang(1234.5678, m, 3, true), "1\u{a0}234,568\u{a0}\u{43c}\u{b3}");
    assert_eq!(fmt_volume_lang(1234.5678, m, 3, false), "1,234.568\u{a0}m\u{b3}");
    assert_eq!(fmt_percent_lang(1234.5, true), "1\u{a0}234,5\u{a0}%");
    assert_eq!(fmt_percent_lang(1234.5, false), "1,234.5\u{a0}%");
    // imperial is unchanged by the language
    for ru in [true, false] {
        assert_eq!(fmt_area_lang(10.0, Units::Imperial, 2, false, ru), "107.64\u{a0}ft\u{b2}");
    }
}

#[test]
fn exports_do_not_depend_on_display_language() {
    // exports read raw numbers (`area_in_units` / `volume_in_units` and fixed-point text),
    // never the localized display strings above
    assert_eq!(area_in_units(12.5, Units::Metric), 12.5);
    assert_eq!(volume_in_units(0.5, Units::Metric), 0.5);
    assert_eq!(fmt_fixed(1234.5678, 2), "1234.57");
}

#[test]
fn link_offset_fields_use_the_mark() {
    assert_eq!(fmt_fixed_mark(1234.5, 3, DecimalMark::Comma), "1234,500");
    assert_eq!(fmt_fixed_mark(1234.5, 3, DecimalMark::Point), "1234.500");
    assert_eq!(fmt_fixed_mark(-0.0001, 3, DecimalMark::Comma), "0,000");
    for v in VALUES {
        for mark in MARKS {
            let s = fmt_fixed_mark(v, 3, mark);
            let got = parse_locale_number(&s, mark).unwrap_or_else(|e| panic!("{s:?} {mark:?}: {e:?}"));
            assert!(near(got, v, 5e-4), "{v} -> {s:?} -> {got}");
        }
    }
}
