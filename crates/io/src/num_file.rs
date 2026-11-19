// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Numbers in external files and drawing text (survey point files, room area labels, block
//! attributes). One rule, independent of the UI language and of any global setting:
//!
//! 1. One decimal mark per file. Comma-separated columns use `.`. Otherwise [`detect_mark`]
//!    looks at the file's number cells once: a cell that reads only one way votes (`12,5`,
//!    `0,250`, `1.234,5`, `1.234.567` for the comma; `12.5`, `1,234.5`, `1,234,567` for the
//!    point). Votes for one mark only make it the file's mark; no votes or both leave none.
//! 2. With a file mark, the other mark and spaces are digit-group separators and must form real
//!    groups (`1 250,5`, `1.234,5`, `1,234.5`); anything else is unreadable.
//! 3. Without one, each value decides alone: with both marks the last one is the decimal, a
//!    single `.` is a decimal, several equal marks are groups, a single `,` is a decimal unless
//!    it also reads as a thousands group (`1,000`, `1,250`): such a value is ambiguous.
//!
//! Ambiguous and unreadable values are never guessed: callers skip and count them so the import
//! summary reports them. Values read through a group mark are flagged for the summary as well.

use ddc_model::num_text::{group_like, parse_invariant};

/// Decimal mark of a file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    Point,
    Comma,
}

impl Mark {
    fn ch(self) -> char {
        match self {
            Mark::Point => '.',
            Mark::Comma => ',',
        }
    }
}

/// A value read from file text; `grouped` when it carried digit-group separators.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FileNum {
    pub value: f64,
    pub grouped: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NumErr {
    /// Reads two ways (`1,250`: 1.25 or 1250) and the file gives no mark.
    Ambiguous,
    /// Not a number under the rule.
    Invalid,
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\u{a0}' | '\u{202f}' | '\u{2009}')
}

/// Digits with marks or spaces only (after an optional sign): text that is meant as a number.
pub fn numeric_like(s: &str) -> bool {
    let t = s.trim();
    let t = t.strip_prefix(['+', '-']).unwrap_or(t);
    t.chars().any(|c| c.is_ascii_digit())
        && t.chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',' || is_space(c))
}

/// The mark a lone value implies (rule 3); None when it has no mark.
fn own_mark(body: &str) -> Result<Option<Mark>, NumErr> {
    match (body.rfind('.'), body.rfind(',')) {
        (Some(p), Some(c)) => Ok(Some(if p > c { Mark::Point } else { Mark::Comma })),
        (Some(_), None) if body.matches('.').count() > 1 => Ok(Some(Mark::Comma)),
        (Some(_), None) => Ok(Some(Mark::Point)),
        (None, Some(_)) if body.matches(',').count() > 1 => Ok(Some(Mark::Point)),
        (None, Some(_)) => {
            let (int, frac) = body.split_once(',').unwrap_or((body, ""));
            if group_like(int, frac) {
                Err(NumErr::Ambiguous)
            } else {
                Ok(Some(Mark::Comma))
            }
        }
        (None, None) => Ok(None),
    }
}

/// The mark a cell proves (rule 1), None when it reads both ways or has no mark.
fn vote(s: &str) -> Option<Mark> {
    if !numeric_like(s) {
        return None;
    }
    let t = s.trim();
    let (np, nc) = (t.matches('.').count(), t.matches(',').count());
    let single = |m: Mark| {
        let (int, frac) = t.split_once(m.ch())?;
        let int = int.trim_start_matches(['+', '-']);
        (!group_like(int, frac)).then_some(m)
    };
    match (np, nc) {
        (0, 0) => None,
        (1, 0) => single(Mark::Point),
        (0, 1) => single(Mark::Comma),
        (_, 0) => Some(Mark::Comma),
        (0, _) => Some(Mark::Point),
        _ => own_mark(t).ok().flatten(),
    }
}

/// The file's decimal mark from its cells (rule 1).
pub fn detect_mark<I, S>(cells: I) -> Option<Mark>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let (mut point, mut comma) = (false, false);
    for c in cells {
        match vote(c.as_ref()) {
            Some(Mark::Point) => point = true,
            Some(Mark::Comma) => comma = true,
            None => {}
        }
        if point && comma {
            return None;
        }
    }
    match (point, comma) {
        (true, false) => Some(Mark::Point),
        (false, true) => Some(Mark::Comma),
        _ => None,
    }
}

/// Integer part with group separators of one kind: 1-3 leading digits (not `0`), then threes.
fn ungroup(int: &str) -> Result<String, NumErr> {
    let mut kind = None;
    let mut parts = vec![String::new()];
    for c in int.chars() {
        if c.is_ascii_digit() {
            if let Some(p) = parts.last_mut() {
                p.push(c);
            }
            continue;
        }
        let k = if is_space(c) { ' ' } else { c };
        if !matches!(k, ' ' | '.' | ',') || kind.is_some_and(|x| x != k) {
            return Err(NumErr::Invalid);
        }
        kind = Some(k);
        parts.push(String::new());
    }
    let ok = (1..=3).contains(&parts[0].len())
        && !parts[0].starts_with('0')
        && parts[1..].iter().all(|p| p.len() == 3);
    if ok {
        Ok(parts.concat())
    } else {
        Err(NumErr::Invalid)
    }
}

/// `s` under the file's mark (`None`: rule 3). Surrounding whitespace is ignored.
pub fn parse_num(s: &str, mark: Option<Mark>) -> Result<FileNum, NumErr> {
    let t = s.trim();
    let (sign, body) = match t.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", t.strip_prefix('+').unwrap_or(t)),
    };
    let (mant, exp) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], &body[i..]),
        None => (body, ""),
    };
    let dec = match mark {
        Some(m) => m,
        None => own_mark(mant)?.unwrap_or(Mark::Point),
    };
    let (int, frac) = match mant.split_once(dec.ch()) {
        Some((a, b)) => (a, Some(b)),
        None => (mant, None),
    };
    let grouped = int.chars().any(|c| c == '.' || c == ',' || is_space(c));
    let int = if grouped {
        if !exp.is_empty() {
            return Err(NumErr::Invalid);
        }
        ungroup(int)?
    } else {
        int.to_string()
    };
    let text = match frac {
        Some(f) => format!("{sign}{int}.{f}{exp}"),
        None => format!("{sign}{int}{exp}"),
    };
    parse_invariant(&text)
        .map(|value| FileNum { value, grouped })
        .ok_or(NumErr::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str, m: Option<Mark>) -> Result<(f64, bool), NumErr> {
        parse_num(s, m).map(|n| (n.value, n.grouped))
    }

    #[test]
    fn table_without_file_mark() {
        use NumErr::*;
        let cases: [(&str, Result<(f64, bool), NumErr>); 12] = [
            ("1,5", Ok((1.5, false))),
            ("1,250", Err(Ambiguous)),
            ("1,000", Err(Ambiguous)),
            ("1,234.5", Ok((1234.5, true))),
            ("1.234,5", Ok((1234.5, true))),
            ("1 250,5", Ok((1250.5, true))),
            ("1\u{a0}250.5", Ok((1250.5, true))),
            ("-0,250", Ok((-0.25, false))),
            ("1,234,567", Ok((1234567.0, true))),
            ("12.5", Ok((12.5, false))),
            ("1.5e3", Ok((1500.0, false))),
            ("1.2.3,5,6", Err(Invalid)),
        ];
        for (s, want) in cases {
            assert_eq!(v(s, None), want, "{s:?}");
        }
        for s in ["", "abc", "1,25,0", "12 34", "0 250,5", "1,5e3x", "1 250.5,5"] {
            assert!(v(s, None).is_err(), "{s:?}");
        }
    }

    #[test]
    fn table_with_file_mark() {
        let c = Some(Mark::Comma);
        let p = Some(Mark::Point);
        assert_eq!(v("1,5", c), Ok((1.5, false)));
        assert_eq!(v("1,250", c), Ok((1.25, false)));
        assert_eq!(v("1.234,5", c), Ok((1234.5, true)));
        assert_eq!(v("1 250,5", c), Ok((1250.5, true)));
        assert_eq!(v("1,234.5", c), Err(NumErr::Invalid));
        assert_eq!(v("1.5", c), Err(NumErr::Invalid));
        assert_eq!(v("1,250", p), Ok((1250.0, true)));
        assert_eq!(v("1,234.5", p), Ok((1234.5, true)));
        assert_eq!(v("1 250.5", p), Ok((1250.5, true)));
        assert_eq!(v("1,5", p), Err(NumErr::Invalid));
        assert_eq!(v("1.234,5", p), Err(NumErr::Invalid));
        assert_eq!(v("1.5e3", p), Ok((1500.0, false)));
    }

    #[test]
    fn file_mark_from_cells() {
        assert_eq!(detect_mark(["1,250", "2,5", "x"]), Some(Mark::Comma));
        assert_eq!(detect_mark(["1.234,5"]), Some(Mark::Comma));
        assert_eq!(detect_mark(["1,250", "12.75"]), Some(Mark::Point));
        assert_eq!(detect_mark(["1,234.5"]), Some(Mark::Point));
        assert_eq!(detect_mark(["1,250", "2,500", "1.000"]), None, "all ambiguous");
        assert_eq!(detect_mark(["1.5", "2,5"]), None, "mixed");
        assert_eq!(detect_mark(["10", "P1", ""]), None);
    }
}
