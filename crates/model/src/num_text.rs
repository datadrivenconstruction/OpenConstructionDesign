// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Numbers kept as text in the model (parameter values, imported properties, schedule text
//! cells) have one invariant form, independent of the UI language and of any thread's decimal
//! mark: optional sign, digits, at most one `.`, optional exponent (`-1250.5`, `1.5e3`, `.5`).
//! Grouping characters (`,`, spaces) are never part of it, and only finite values are numbers.
//! Writers use `f64` `Display` (`x.to_string()`), which is this form. Typed user input is read
//! with the UI mark elsewhere (the UI number parser) and never comes here.
//!
//! Legacy rule ([`parse_stored`]): text written by older builds may carry a comma decimal. Text
//! with exactly one `,`, digits on both sides, no `.`, no spaces and no exponent reads as that
//! decimal (`1,5` = 1.5, `0,250` = 0.25, `1234,567` = 1234.567) unless it also reads as an
//! English thousands group: 1-3 leading digits without a leading zero and exactly three digits
//! after (`1,000`, `-12,500`). Such text is ambiguous and is not a number (it stays text, a
//! formula reports it); a stored value is never rewritten on read.

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn all_digits(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit())
}

/// `s` in the invariant form, surrounding whitespace ignored; None for anything else.
pub fn parse_invariant(s: &str) -> Option<f64> {
    let t = s.trim();
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    let (mant, exp) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    if let Some(e) = exp {
        if !digits(e.strip_prefix(['+', '-']).unwrap_or(e)) {
            return None;
        }
    }
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    if (int.is_empty() && frac.is_empty()) || !all_digits(int) || !all_digits(frac) {
        return None;
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// `1,000`-like: reads as an English thousands group as well as a comma decimal.
pub fn group_like(int: &str, frac: &str) -> bool {
    frac.len() == 3 && (1..=3).contains(&int.len()) && !int.starts_with('0')
}

/// Stored number text: the invariant form, or an unambiguous legacy comma decimal (module doc).
pub fn parse_stored(s: &str) -> Option<f64> {
    parse_invariant(s).or_else(|| {
        let t = s.trim();
        let (sign, body) = match t.strip_prefix('-') {
            Some(r) => ("-", r),
            None => ("", t.strip_prefix('+').unwrap_or(t)),
        };
        let (int, frac) = body.split_once(',')?;
        if !digits(int) || !digits(frac) || group_like(int, frac) {
            return None;
        }
        parse_invariant(&format!("{sign}{int}.{frac}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invariant_form_only() {
        assert_eq!(parse_invariant("1.5"), Some(1.5));
        assert_eq!(parse_invariant(" -1250.5 "), Some(-1250.5));
        assert_eq!(parse_invariant("+2"), Some(2.0));
        assert_eq!(parse_invariant(".5"), Some(0.5));
        assert_eq!(parse_invariant("1.5e3"), Some(1500.0));
        assert_eq!(parse_invariant("1E-3"), Some(0.001));
        for s in [
            "", " ", "1,5", "1,250", "1 250", "1\u{a0}250", "1.2.3", "1e", "e3", ".", "-",
            "inf", "NaN", "infinity", "1e999", "0x10", "1_000", "12a", "--1",
        ] {
            assert_eq!(parse_invariant(s), None, "{s:?}");
        }
    }

    #[test]
    fn stored_reads_invariant_and_unambiguous_legacy_comma() {
        assert_eq!(parse_stored("1.5"), Some(1.5));
        assert_eq!(parse_stored("1,5"), Some(1.5));
        assert_eq!(parse_stored("-12,5"), Some(-12.5));
        assert_eq!(parse_stored("0,250"), Some(0.25));
        assert_eq!(parse_stored("1,25"), Some(1.25));
        assert_eq!(parse_stored("1,2500"), Some(1.25));
        assert_eq!(parse_stored("1234,567"), Some(1234.567));
        // ambiguous with a thousands group: not a number, never 1.0 or 1000.0
        for s in ["1,000", "1,250", "-12,500", "999,999"] {
            assert_eq!(parse_stored(s), None, "{s:?}");
        }
        // grouping, two marks, exponent with a comma: not a number
        for s in ["1 250,5", "1,250.5", "1.250,5", "1,2,3", "1,5e3", ",5", "5,", "1, 5"] {
            assert_eq!(parse_stored(s), None, "{s:?}");
        }
    }

    #[test]
    fn display_round_trips_through_the_invariant_form() {
        for x in [0.0, -0.5, 1250.5, 1e-9, 3.0e15, 0.1 + 0.2] {
            assert_eq!(parse_invariant(&x.to_string()), Some(x), "{x}");
            assert_eq!(parse_stored(&x.to_string()), Some(x), "{x}");
        }
    }
}
