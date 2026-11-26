// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One CSV writer for every export: UTF-8 with a byte order mark, CRLF lines, RFC 4180 quoting,
//! separator by the locale's decimal mark (`;` with a decimal comma, `,` with a point), and
//! text a spreadsheet would run as a formula (`=`, `+`, `-`, `@` first) prefixed with `'`.
use std::borrow::Cow;

/// Byte order mark: spreadsheets read the file as UTF-8 only with it.
pub const BOM: char = '\u{FEFF}';

/// Separator and decimal mark of one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub sep: char,
    pub decimal_comma: bool,
}

impl Style {
    /// `,` with `.` decimals.
    pub const EN: Style = Style { sep: ',', decimal_comma: false };
    /// `;` with `,` decimals: a decimal-comma locale's spreadsheet (de, fr, ru, ...) splits on `;`.
    pub const RU: Style = Style { sep: ';', decimal_comma: true };

    /// The style of a locale whose decimal mark is a comma (`true`) or a point.
    pub fn for_decimal_comma(decimal_comma: bool) -> Style {
        if decimal_comma {
            Style::RU
        } else {
            Style::EN
        }
    }

    /// Same as [`Style::for_decimal_comma`]; pass the locale's decimal mark, not "is Russian".
    pub fn for_lang(decimal_comma: bool) -> Style {
        Style::for_decimal_comma(decimal_comma)
    }

    /// `x` with `decimals` places and this style's decimal mark; `-0` is written as `0`.
    pub fn num(&self, x: f64, decimals: usize) -> String {
        let s = format!("{:.*}", decimals, x);
        let s = if s.trim_start_matches('-').chars().all(|c| c == '0' || c == '.') {
            s.trim_start_matches('-').to_string()
        } else {
            s
        };
        self.mark(s)
    }

    /// A number already written with `.` decimals, in this style's decimal mark.
    pub fn mark(&self, s: String) -> String {
        if self.decimal_comma {
            s.replace('.', ",")
        } else {
            s
        }
    }
}

/// One field: a leading `=`, `+`, `-` or `@` that is not a plain number gets `'` in front; then
/// quoted when it holds the separator, a quote or a line break.
pub fn field(s: &str, sep: char) -> Cow<'_, str> {
    let formula = match s.chars().next() {
        Some('=') | Some('@') => true,
        Some('+') | Some('-') => !is_number(s),
        _ => false,
    };
    let s: Cow<'_, str> = if formula { Cow::Owned(format!("'{s}")) } else { Cow::Borrowed(s) };
    if s.contains([sep, '"', '\n', '\r']) {
        Cow::Owned(format!("\"{}\"", s.replace('"', "\"\"")))
    } else {
        s
    }
}

/// Text read back from a file this writer made: the `'` added by [`field`] is dropped again.
pub fn unguard(s: &str) -> &str {
    match s.strip_prefix('\'') {
        Some(rest) if rest.starts_with(['=', '+', '-', '@']) => rest,
        _ => s,
    }
}

/// A signed number as shown in either language (`-5`, `+2.5`, `-1,5`, `-1 234,5`): kept as is.
pub fn is_number(s: &str) -> bool {
    let t: String = s
        .chars()
        .filter(|c| !matches!(c, ' ' | '\u{a0}' | '\u{202f}'))
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    let digits = t.trim_start_matches(['+', '-']);
    t.len() - digits.len() == 1
        && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
        && digits.chars().any(|c| c.is_ascii_digit())
        && digits.matches('.').count() <= 1
}

/// One line of fields joined by `sep`, without the line end.
pub fn line<S: AsRef<str>>(cells: impl IntoIterator<Item = S>, sep: char) -> String {
    let mut out = String::new();
    for (i, c) in cells.into_iter().enumerate() {
        if i > 0 {
            out.push(sep);
        }
        out.push_str(&field(c.as_ref(), sep));
    }
    out
}

/// Rows joined by `sep` and `eol`, every line ended; no byte order mark (clipboard, text boxes).
pub fn join<S, R>(rows: impl IntoIterator<Item = R>, sep: char, eol: &str) -> String
where
    S: AsRef<str>,
    R: IntoIterator<Item = S>,
{
    let mut out = String::new();
    for r in rows {
        out.push_str(&line(r, sep));
        out.push_str(eol);
    }
    out
}

/// Content of a .csv file: byte order mark, `style.sep` separated, CRLF lines.
pub fn file<S, R>(rows: impl IntoIterator<Item = R>, style: Style) -> String
where
    S: AsRef<str>,
    R: IntoIterator<Item = S>,
{
    format!("{BOM}{}", join(rows, style.sep, "\r\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_starts_with_a_bom_and_ends_lines_with_crlf() {
        let s = file([["a", "b"], ["c", "d"]], Style::EN);
        assert_eq!(&s.as_bytes()[..3], &[0xEF, 0xBB, 0xBF]);
        assert_eq!(s.trim_start_matches(BOM), "a,b\r\nc,d\r\n");
    }

    #[test]
    fn russian_uses_semicolon_and_decimal_comma() {
        let st = Style::for_lang(true);
        let s = file([vec!["x".to_string(), st.num(12.345, 3), st.num(-1.5, 2)]], st);
        assert_eq!(s.trim_start_matches(BOM), "x;12,345;-1,50\r\n");
        // a comma inside a number needs no quotes under ';'
        assert!(!s.contains('"'));
    }

    #[test]
    fn english_uses_comma_and_decimal_point() {
        let st = Style::for_lang(false);
        let s = file([vec!["x".to_string(), st.num(12.345, 3), st.num(-0.0001, 2)]], st);
        assert_eq!(s.trim_start_matches(BOM), "x,12.345,0.00\r\n");
    }

    #[test]
    fn quotes_separators_and_line_breaks_are_quoted() {
        assert_eq!(field("a;b", ';').as_ref(), "\"a;b\"");
        assert_eq!(field("a;b", ',').as_ref(), "a;b");
        assert_eq!(field("x,y", ',').as_ref(), "\"x,y\"");
        assert_eq!(field("say \"hi\"", ',').as_ref(), "\"say \"\"hi\"\"\"");
        assert_eq!(field("l1\nl2", ',').as_ref(), "\"l1\nl2\"");
        assert_eq!(field("l1\r\nl2", ';').as_ref(), "\"l1\r\nl2\"");
        assert_eq!(field("\u{d8}12", ';').as_ref(), "\u{d8}12");
    }

    #[test]
    fn formula_looking_text_is_not_run_by_the_spreadsheet() {
        assert_eq!(field("=1+2", ',').as_ref(), "'=1+2");
        assert_eq!(field("@x", ',').as_ref(), "'@x");
        assert_eq!(field("+x", ';').as_ref(), "'+x");
        assert_eq!(field("-x", ',').as_ref(), "'-x");
        assert_eq!(field("=a;b", ';').as_ref(), "\"'=a;b\"");
        assert_eq!(unguard("'=1+2"), "=1+2");
        assert_eq!(unguard("'plain"), "'plain");
        assert_eq!(unguard(&field("-x", ',')), "-x");
    }

    #[test]
    fn negative_numbers_stay_numbers() {
        assert_eq!(field("-5", ',').as_ref(), "-5");
        assert_eq!(field("+2.5", ',').as_ref(), "+2.5");
        assert_eq!(field("-1,5", ';').as_ref(), "-1,5");
        assert_eq!(field("-1\u{a0}234,5", ';').as_ref(), "-1\u{a0}234,5");
        assert_eq!(field("-1,5", ',').as_ref(), "\"-1,5\"");
        assert_eq!(field("--5", ',').as_ref(), "'--5");
        assert_eq!(field("-", ',').as_ref(), "'-");
        assert_eq!(field("-1.2.3", ',').as_ref(), "'-1.2.3");
        assert_eq!(field("+1e5", ',').as_ref(), "'+1e5");
        assert_eq!(Style::RU.num(-3.25, 2), "-3,25");
        assert_eq!(field(&Style::RU.num(-3.25, 2), ';').as_ref(), "-3,25");
    }

    #[test]
    fn join_has_no_bom_and_keeps_the_given_line_end() {
        let s = join([["a\tb", "c"]], '\t', "\n");
        assert_eq!(s, "\"a\tb\"\tc\n");
        assert!(!s.starts_with(BOM));
    }
}
