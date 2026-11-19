// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Locale-aware number reading shared by every numeric field (NumberField, length fields, option
//! bars). A number is read exactly as the UI language writes it or refused: a separator is never
//! guessed into a factor of 1000.
//!
//! - English ([`DecimalMark::Point`]): `.` decimal, `,` thousands in groups of three
//!   (`1,000` = 1000, `1,000.5` = 1000.5). A lone `,` that is not such a group (`1,5`, `0,250`,
//!   `1,2345`) is ambiguous and refused with [`NumError::CommaDecimal`].
//! - Russian ([`DecimalMark::Comma`]): `,` decimal, space / no-break space thousands
//!   (`1 000,5`); a point is still read as a decimal (`1.5`).
//! - Both: space groups, an exponent (`1e3`), a typographic minus. With both `,` and `.` the
//!   last one is the decimal and the other must group thousands (`1,234.5`, `1.234,5`).

use crate::i18n::Lang;

/// Decimal mark of the UI language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecimalMark {
    /// English: `1,000.5`.
    Point,
    /// Russian: `1 000,5`.
    Comma,
}

impl DecimalMark {
    pub fn for_ru(ru: bool) -> Self {
        if ru {
            Self::Comma
        } else {
            Self::Point
        }
    }

    /// The locale's decimal mark, from the same table the number fields format with.
    pub fn for_lang(l: Lang) -> Self {
        if crate::widgets::NumberFormat::for_lang(l).decimal == ',' {
            Self::Comma
        } else {
            Self::Point
        }
    }

    pub fn as_char(self) -> char {
        match self {
            Self::Point => '.',
            Self::Comma => ',',
        }
    }
}

/// Why a text is not a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumError {
    Empty,
    NotNumber,
    /// English comma that is not a thousands group (`1,5`): the user must type a point.
    CommaDecimal,
}

/// Space, no-break space or narrow no-break space: a thousands group inside a number.
/// Step multiplier of a number edited by arrows, steppers or label scrubbing: Shift x10,
/// Alt x0.1, both cancel out.
pub fn scrub_factor(shift: bool, alt: bool) -> f64 {
    match (shift, alt) {
        (true, false) => 10.0,
        (false, true) => 0.1,
        _ => 1.0,
    }
}

pub fn is_group_space(c: char) -> bool {
    matches!(c, ' ' | '\u{a0}' | '\u{202f}')
}

/// Zeros of the decimal digit runs a person may type: Arabic-Indic, Extended Arabic-Indic (Persian,
/// Urdu), NKo, Devanagari, Bengali, Gurmukhi, Gujarati, Oriya, Tamil, Telugu, Kannada, Malayalam,
/// Sinhala, Thai, Lao, Tibetan, Myanmar, Myanmar Shan, Khmer, Mongolian, full-width.
const DIGIT_ZEROS: [u32; 21] = [
    0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66, 0xDE6,
    0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0xFF10,
];

fn fold_char(c: char) -> char {
    let u = c as u32;
    if let Some(z) = DIGIT_ZEROS.iter().find(|z| (**z..**z + 10).contains(&u)) {
        return char::from(b'0' + (u - z) as u8);
    }
    match c {
        // Arabic decimal and thousands separators
        '\u{66B}' => '.',
        '\u{66C}' => ',',
        '\u{FF0E}' => '.',
        '\u{FF0C}' => ',',
        '\u{FF0D}' | '\u{FE63}' => '-',
        '\u{FF0B}' => '+',
        '\u{FF45}' => 'e',
        '\u{FF25}' => 'E',
        '\u{3000}' => ' ',
        _ => c,
    }
}

/// Digits of any script, full-width forms and the Arabic separators as ASCII (`٣٫٥` is `3.5`,
/// `１２` is `12`); every other character is kept.
pub fn fold_digits(s: &str) -> std::borrow::Cow<'_, str> {
    if s.is_ascii() {
        return std::borrow::Cow::Borrowed(s);
    }
    std::borrow::Cow::Owned(s.chars().map(fold_char).collect())
}

fn all_digits(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit())
}

/// `int,frac` with one comma is an English thousands group: 1-3 leading digits (no leading
/// zero) and exactly three after.
fn lone_group(int: &str, frac: &str) -> bool {
    frac.len() == 3
        && all_digits(frac)
        && (1..=3).contains(&int.len())
        && all_digits(int)
        && !int.starts_with('0')
}

/// One number in canonical form for `str::parse::<f64>`: optional `-`, digits, optional
/// `.digits`, optional `e[+-]digits`; groups removed. Surrounding spaces are ignored.
pub fn canonical(s: &str, mark: DecimalMark) -> Result<String, NumError> {
    // the Arabic thousands separator only ever groups: read as a group space, never as a comma
    // that a comma-decimal locale would take for the decimal mark
    let grouped;
    let s = if s.contains('\u{66C}') {
        grouped = s.replace('\u{66C}', "\u{202f}");
        grouped.as_str()
    } else {
        s
    };
    let folded = fold_digits(s);
    let t = folded.trim_matches(|c: char| c.is_whitespace() || is_group_space(c));
    if t.is_empty() {
        return Err(NumError::Empty);
    }
    let (neg, body) = match t.strip_prefix(['-', '\u{2212}']) {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (mant, exp) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    if let Some(e) = exp {
        let d = e.strip_prefix(['+', '-']).unwrap_or(e);
        if d.is_empty() || !all_digits(d) {
            return Err(NumError::NotNumber);
        }
    }
    let ok_char = |c: char| c.is_ascii_digit() || c == ',' || c == '.' || is_group_space(c);
    if mant.is_empty() || !mant.chars().all(ok_char) {
        return Err(NumError::NotNumber);
    }
    let commas = mant.matches(',').count();
    let points = mant.matches('.').count();
    let spaced = mant.contains(is_group_space);
    // decimal byte index and the group sign (' ' stands for any group space)
    let (dec, group) = match mant.rfind([',', '.']) {
        None => (None, spaced.then_some(' ')),
        Some(at) => {
            let sep = if mant[at..].starts_with(',') { ',' } else { '.' };
            let (same, other) = if sep == ',' {
                (commas, points)
            } else {
                (points, commas)
            };
            if other > 0 {
                if same != 1 || spaced {
                    return Err(NumError::NotNumber);
                }
                (Some(at), Some(if sep == ',' { '.' } else { ',' }))
            } else if same > 1 {
                if spaced {
                    return Err(NumError::NotNumber);
                }
                (None, Some(sep))
            } else if sep == ',' && mark == DecimalMark::Point {
                if !spaced && lone_group(&mant[..at], &mant[at + 1..]) {
                    (None, Some(','))
                } else {
                    return Err(NumError::CommaDecimal);
                }
            } else {
                (Some(at), spaced.then_some(' '))
            }
        }
    };
    let int = dec.map_or(mant, |at| &mant[..at]);
    let frac = dec.map(|at| &mant[at + 1..]);
    let digits = match group {
        None => {
            if !all_digits(int) {
                return Err(NumError::NotNumber);
            }
            int.to_string()
        }
        Some(g) => {
            let parts: Vec<&str> = if g == ' ' {
                int.split(is_group_space).collect()
            } else {
                int.split(g).collect()
            };
            let head_ok = (1..=3).contains(&parts[0].len());
            if !head_ok || parts[1..].iter().any(|p| p.len() != 3) || !parts.iter().all(|p| all_digits(p)) {
                return Err(NumError::NotNumber);
            }
            parts.concat()
        }
    };
    if frac.is_some_and(|f| !all_digits(f)) {
        return Err(NumError::NotNumber);
    }
    if digits.is_empty() && frac.is_none_or(str::is_empty) {
        return Err(NumError::NotNumber);
    }
    let mut out = String::with_capacity(digits.len() + 8);
    if neg {
        out.push('-');
    }
    out.push_str(if digits.is_empty() { "0" } else { &digits });
    if let Some(f) = frac.filter(|f| !f.is_empty()) {
        out.push('.');
        out.push_str(f);
    }
    if let Some(e) = exp {
        out.push('e');
        out.push_str(e);
    }
    Ok(out)
}

/// The number `s` in the locale of `mark`; finite values only.
pub fn parse(s: &str, mark: DecimalMark) -> Result<f64, NumError> {
    let c = canonical(s, mark)?;
    match c.parse::<f64>() {
        Ok(v) if v.is_finite() => Ok(v),
        _ => Err(NumError::NotNumber),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DecimalMark::{Comma as RU, Point as EN};

    #[test]
    fn english_comma_groups_point_decimal() {
        assert_eq!(parse("1,000", EN), Ok(1000.0));
        assert_eq!(parse("12,500", EN), Ok(12500.0));
        assert_eq!(parse("1,000.5", EN), Ok(1000.5));
        assert_eq!(parse("1,000,000", EN), Ok(1_000_000.0));
        assert_eq!(parse("1.000", EN), Ok(1.0));
        assert_eq!(parse("1.5", EN), Ok(1.5));
        assert_eq!(parse("1 000", EN), Ok(1000.0));
        assert_eq!(parse("1\u{a0}000.5", EN), Ok(1000.5));
        assert_eq!(parse("-0.5", EN), Ok(-0.5));
        assert_eq!(parse("\u{2212}2", EN), Ok(-2.0));
        assert_eq!(parse("1e3", EN), Ok(1000.0));
        assert_eq!(parse("1.5e-3", EN), Ok(0.0015));
        assert_eq!(parse(".5", EN), Ok(0.5));
    }

    #[test]
    fn english_ambiguous_comma_is_refused_not_scaled() {
        for s in ["1,5", "-0,5", "0,250", "1,2345", "1234,567", "1 000,5", ",5", "1,50"] {
            assert_eq!(parse(s, EN), Err(NumError::CommaDecimal), "{s}");
        }
    }

    #[test]
    fn russian_comma_decimal_space_groups() {
        assert_eq!(parse("1,000", RU), Ok(1.0));
        assert_eq!(parse("1,5", RU), Ok(1.5));
        assert_eq!(parse("-0,5", RU), Ok(-0.5));
        assert_eq!(parse("1 000", RU), Ok(1000.0));
        assert_eq!(parse("1\u{a0}000,5", RU), Ok(1000.5));
        assert_eq!(parse("1\u{202f}000", RU), Ok(1000.0));
        assert_eq!(parse("12,500", RU), Ok(12.5));
        assert_eq!(parse("1.5", RU), Ok(1.5));
        assert_eq!(parse("1.000", RU), Ok(1.0));
        assert_eq!(parse("1e3", RU), Ok(1000.0));
        assert_eq!(parse("1,5e3", RU), Ok(1500.0));
    }

    #[test]
    fn both_marks_read_the_last_as_decimal() {
        for m in [EN, RU] {
            assert_eq!(parse("1,234.5", m), Ok(1234.5));
            assert_eq!(parse("1.234,5", m), Ok(1234.5));
            assert_eq!(parse("1.000.000", m), Ok(1_000_000.0));
        }
    }

    #[test]
    fn garbage_and_empty_are_refused() {
        for m in [EN, RU] {
            assert_eq!(parse("", m), Err(NumError::Empty));
            assert_eq!(parse("  ", m), Err(NumError::Empty));
            for s in [
                "abc", "inf", "NaN", "1e", "e3", "-", "--5", "1 5", "1,23,456", "1,000,00",
                "1234,567.5", "1,2.5", "1.5,2,3", "1 000,000.5", "1'000", "12x", "1e400",
            ] {
                assert!(parse(s, m).is_err(), "{s} {m:?}");
            }
        }
        assert_eq!(parse(",", RU), Err(NumError::NotNumber));
    }

    #[test]
    fn scrub_factor_modifiers() {
        assert_eq!(scrub_factor(false, false), 1.0);
        assert_eq!(scrub_factor(true, false), 10.0);
        assert_eq!(scrub_factor(false, true), 0.1);
        assert_eq!(scrub_factor(true, true), 1.0);
    }

    #[test]
    fn canonical_text() {
        assert_eq!(canonical("1 000,5", RU).as_deref(), Ok("1000.5"));
        assert_eq!(canonical("1,000", EN).as_deref(), Ok("1000"));
        assert_eq!(canonical("-0,5", RU).as_deref(), Ok("-0.5"));
        assert_eq!(canonical("5.", EN).as_deref(), Ok("5"));
        assert_eq!(DecimalMark::for_lang(Lang::Ru), RU);
        assert_eq!(DecimalMark::for_ru(false).as_char(), '.');
    }

    /// Every UI locale has its marks in the table; a number formatted in a locale reads back in
    /// it to the same value.
    #[test]
    fn every_locale_formats_and_reads_back_its_own_numbers() {
        use crate::widgets::field::format_number;
        use crate::widgets::NumberFormat;
        for &l in Lang::ALL {
            let nf = NumberFormat::for_lang(l);
            assert_ne!(Some(nf.decimal), nf.group, "{}", l.code());
            let mark = DecimalMark::for_lang(l);
            assert_eq!(mark.as_char(), nf.decimal, "{}", l.code());
            for v in [1234567.25, 0.5, -42.125, 1000.0] {
                let s = format_number(v, 3, nf);
                assert_eq!(parse(&s, mark), Ok(v), "{} {s}", l.code());
            }
        }
        let comma: Vec<&str> = Lang::ALL.iter().filter(|l| DecimalMark::for_lang(**l) == RU).map(|l| l.code()).collect();
        for c in ["ru", "de", "fr", "es", "pt-BR", "tr", "uk", "pl"] {
            assert!(comma.contains(&c), "{c} writes a decimal comma");
        }
        for c in ["en", "ja", "zh", "hi", "he", "es-MX"] {
            assert!(!comma.contains(&c), "{c} writes a decimal point");
        }
    }

    #[test]
    fn digits_of_every_script_read_as_their_values() {
        // Arabic-Indic with the Arabic decimal mark, Persian, Devanagari, Bengali, Thai, full-width
        for (s, want) in [
            ("\u{663}\u{66B}\u{665}", 3.5),
            ("\u{661}\u{66C}\u{660}\u{660}\u{660}", 1000.0),
            ("\u{6F1}\u{6F2}", 12.0),
            ("\u{967}\u{968}.\u{96B}", 12.5),
            ("\u{9E8}\u{9E6}", 20.0),
            ("\u{E53}\u{E52}", 32.0),
            ("\u{FF11}\u{FF12}\u{FF0E}\u{FF15}", 12.5),
            ("\u{FF0D}\u{FF14}", -4.0),
        ] {
            assert_eq!(parse(s, EN), Ok(want), "{s}");
            assert_eq!(parse(s, RU), Ok(want), "{s}");
        }
        assert_eq!(fold_digits("12 m"), "12 m");
        assert_eq!(parse("\u{663}x", EN), Err(NumError::NotNumber));
        assert_eq!(parse("\u{2468}", EN), Err(NumError::NotNumber), "a circled digit is a symbol");
    }
}
