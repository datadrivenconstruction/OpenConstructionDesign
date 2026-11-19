//! Palette arguments: Russian "wall 300 concrete", "door 900x2100", "wall 200 @Level 2". Leading words
//! name the command; numbers (with optional unit, also as the next word: "300 cm"), the remaining
//! words and `@level` are arguments. `@` takes the rest of the query (the host matches the longest
//! level name, [`Args::take_level`]) or a quoted name: `@"Level 2"`, `@«Level 2»`.
//! Numbers are read with the UI decimal mark ([`numparse`]): Russian `1,5`, English `1.5` and
//! `1,000`; whitespace always separates arguments (`900 600` is two numbers in both). Text the mark refuses (English `1,5`) is [`Tok::Bad`], never a value.

use crate::numparse::{self, DecimalMark, NumError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Mm,
    Cm,
    M,
    Ft,
    In,
    Deg,
    Pct,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Word(String),
    Num(f64, Option<Unit>),
    /// `@text` — level reference: the rest of the query, or a quoted name (`true`).
    Level(String, bool),
    /// Number-like text the UI mark refuses (English `1,5`, `1,23,4`): the typed text.
    Bad(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Args {
    pub nums: Vec<(f64, Option<Unit>)>,
    pub words: Vec<String>,
    pub level: Option<String>,
    /// `level` was quoted: an exact name.
    pub level_exact: bool,
    /// Refused numbers ([`Tok::Bad`]): the host applies nothing while any is present.
    pub bad: Vec<String>,
}

impl Args {
    pub fn is_empty(&self) -> bool {
        self.nums.is_empty()
            && self.words.is_empty()
            && self.level.is_none()
            && self.bad.is_empty()
    }
    /// Number `i` as a length in metres, zero allowed. Without a unit — the project unit, the
    /// same rule as every length field: metric — millimetres, imperial — inches.
    pub fn length_or_zero(&self, i: usize, metric: bool) -> Option<f64> {
        let (v, u) = *self.nums.get(i)?;
        let m = match u {
            Some(Unit::Mm) => v / 1000.0,
            Some(Unit::Cm) => v / 100.0,
            Some(Unit::M) => v,
            Some(Unit::Ft) => v * 0.3048,
            Some(Unit::In) => v * 0.0254,
            Some(Unit::Deg | Unit::Pct) => return None,
            None if metric => v / 1000.0,
            None => v * 0.0254,
        };
        (m.is_finite() && m >= 0.0).then_some(m)
    }
    /// [`Self::length_or_zero`], positive only.
    pub fn length(&self, i: usize, metric: bool) -> Option<f64> {
        self.length_or_zero(i, metric).filter(|m| *m > 0.0)
    }
    /// Number `i` as a plain value (angles, percent, counts).
    pub fn value(&self, i: usize) -> Option<f64> {
        self.nums.get(i).map(|n| n.0).filter(|v| v.is_finite())
    }
    /// Append the numbers and words of `text` (what followed the level name), Russian mark.
    pub fn extend_with(&mut self, text: &str) {
        self.extend_with_in(text, DecimalMark::Comma);
    }
    /// [`Self::extend_with`] with the UI decimal mark.
    pub fn extend_with_in(&mut self, text: &str, mark: DecimalMark) {
        for t in tokens_in(text, mark) {
            match t {
                Tok::Word(w) => self.words.push(w),
                Tok::Num(v, u) => self.nums.push((v, u)),
                Tok::Bad(b) => self.bad.push(b),
                Tok::Level(..) => {}
            }
        }
    }
    /// [`Self::take_level_in`] with the Russian mark.
    pub fn take_level(&mut self, names: &[&str]) -> Option<Result<usize, String>> {
        self.take_level_in(names, DecimalMark::Comma)
    }
    /// Resolve `@…` against the level names: quoted — the exact name; otherwise the longest
    /// name the tail starts with (whole words), else the first word as a name prefix. What
    /// follows the name becomes arguments again (read with `mark`). `None` — no level given;
    /// `Err` — not found.
    pub fn take_level_in(
        &mut self,
        names: &[&str],
        mark: DecimalMark,
    ) -> Option<Result<usize, String>> {
        let tail = self.level.take()?;
        let low = tail.trim().to_lowercase();
        let norm: Vec<String> = names.iter().map(|n| n.trim().to_lowercase()).collect();
        if self.level_exact {
            return Some(norm.iter().position(|n| *n == low).ok_or(tail));
        }
        let mut best: Option<(usize, usize)> = None;
        for (i, n) in norm.iter().enumerate() {
            if n.is_empty() || !low.starts_with(n.as_str()) {
                continue;
            }
            if low[n.len()..]
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace())
            {
                continue;
            }
            if best.is_none_or(|b| n.len() > b.1) {
                best = Some((i, n.len()));
            }
        }
        let (found, word, rest) = match best {
            Some((i, l)) => (Some(i), String::new(), low[l..].to_string()),
            None => {
                let (w, rest) = low
                    .split_once(char::is_whitespace)
                    .unwrap_or((low.as_str(), ""));
                let i = if w.is_empty() {
                    None
                } else {
                    norm.iter().position(|n| n.starts_with(w))
                };
                (i, w.to_string(), rest.to_string())
            }
        };
        self.extend_with_in(&rest, mark);
        Some(found.ok_or(word))
    }
}

const UNITS: &[(&str, Unit)] = &[
    ("mm", Unit::Mm),
    ("\u{43c}\u{43c}", Unit::Mm),
    ("cm", Unit::Cm),
    ("\u{441}\u{43c}", Unit::Cm),
    ("m", Unit::M),
    ("\u{43c}", Unit::M),
    ("ft", Unit::Ft),
    ("'", Unit::Ft),
    ("in", Unit::In),
    ("\"", Unit::In),
    ("°", Unit::Deg),
    ("deg", Unit::Deg),
    ("\u{433}\u{440}\u{430}\u{434}", Unit::Deg),
    ("%", Unit::Pct),
];

fn unit_of(t: &str) -> Option<Unit> {
    let t = t.trim().to_lowercase();
    UNITS.iter().find(|(s, _)| *s == t).map(|(_, u)| *u)
}

/// A character of a number's digit run: digits, both marks, a leading sign. Never a space:
/// whitespace separates arguments, so it is never a thousands group here.
fn run_char(i: usize, c: char) -> bool {
    c.is_ascii_digit()
        || c == '.'
        || c == ','
        || (i == 0 && matches!(c, '-' | '\u{2212}'))
}

/// `s` is a whole digit run with at least one digit.
fn numeric(s: &str) -> bool {
    s.char_indices().all(|(i, c)| run_char(i, c)) && s.bytes().any(|b| b.is_ascii_digit())
}

/// `8'6"`, `8'-6"` → inches, both parts read with `mark`; `None` for another shape.
fn feet_inches(t: &str, mark: DecimalMark) -> Option<Result<f64, NumError>> {
    let (ft, rest) = t.split_once('\'')?;
    let ft = ft.trim();
    let rest = rest.trim().trim_start_matches('-').trim();
    let inch = rest.strip_suffix('"')?.trim();
    if !numeric(ft) || !numeric(inch) {
        return None;
    }
    let v = numparse::parse(ft, mark)
        .and_then(|f| numparse::parse(inch, mark).map(|i| f * 12.0 + i))
        .and_then(|v| {
            if v.is_finite() {
                Ok(v)
            } else {
                Err(NumError::NotNumber)
            }
        });
    Some(v)
}

/// [`number_in`] with the Russian mark, refused text as `None`.
pub fn number(t: &str) -> Option<(f64, Option<Unit>)> {
    number_in(t, DecimalMark::Comma)?.ok()
}

/// `300`, `3,5m` (Russian unit letter), `30cm`, `12'`, `8'6"`, `90°` read with the UI decimal mark. `None` — not a
/// number with a unit (a word); `Some(Err)` — a number the mark refuses (English `1,5`).
pub fn number_in(t: &str, mark: DecimalMark) -> Option<Result<(f64, Option<Unit>), NumError>> {
    let t = t.trim();
    let end = t
        .char_indices()
        .find(|&(i, c)| !run_char(i, c))
        .map_or(t.len(), |(i, _)| i);
    let (num, rest) = t.split_at(end);
    if !num.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    let rest = rest.trim();
    let unit = if rest.is_empty() {
        None
    } else if let Some(u) = unit_of(rest) {
        Some(u)
    } else {
        return feet_inches(t, mark).map(|r| r.map(|i| (i, Some(Unit::In))));
    };
    Some(numparse::parse(num, mark).map(|v| (v, unit)))
}


/// Closing mark of a quoted level name.
fn close_of(open: char) -> char {
    if open == '«' {
        '»'
    } else {
        '"'
    }
}

/// Tokens of a query; `900x2100` / `900` + Cyrillic x + `2100` / `900×2100` give two numbers; a
/// unit word right after a plain number is its unit ("300 cm", also Russian). Russian mark: [`tokens_in`].
pub fn tokens(q: &str) -> Vec<Tok> {
    tokens_in(q, DecimalMark::Comma)
}

/// [`tokens`] with numbers read in the UI decimal mark `mark`.
pub fn tokens_in(q: &str, mark: DecimalMark) -> Vec<Tok> {
    let mut out: Vec<Tok> = vec![];
    let mut rest = q;
    loop {
        let r = rest.trim_start();
        if r.is_empty() {
            break;
        }
        if let Some(after) = r.strip_prefix('@') {
            if let Some(o) = after.chars().next().filter(|c| matches!(c, '"' | '«')) {
                let body = &after[o.len_utf8()..];
                let c = close_of(o);
                let end = body.find(c).unwrap_or(body.len());
                out.push(Tok::Level(body[..end].trim().to_string(), true));
                rest = body.get(end + c.len_utf8()..).unwrap_or("");
                continue;
            }
            out.push(Tok::Level(after.trim().to_string(), false));
            break;
        }
        // any whitespace (no-break and thin spaces too) separates arguments: «900 600» is two
        let end = r.find(char::is_whitespace).unwrap_or(r.len());
        let raw = &r[..end];
        rest = &r[end..];
        if let Some(u) = unit_of(raw) {
            match out.last_mut() {
                Some(Tok::Num(_, pu @ None)) => {
                    *pu = Some(u);
                    continue;
                }
                Some(Tok::Bad(b)) => {
                    b.push(' ');
                    b.push_str(raw);
                    continue;
                }
                _ => {}
            }
        }
        let parts: Vec<&str> = raw.split(['x', '\u{445}', '×', 'X', '\u{425}']).collect();
        let nums: Option<Vec<_>> = if parts.len() > 1 {
            parts.iter().map(|p| number_in(p, mark)).collect()
        } else {
            None
        };
        if let Some(ns) = nums {
            if ns.iter().any(|n| n.is_err()) {
                out.push(Tok::Bad(raw.to_string()));
            } else {
                out.extend(ns.into_iter().flatten().map(|(v, u)| Tok::Num(v, u)));
            }
        } else {
            out.push(match number_in(raw, mark) {
                Some(Ok((v, u))) => Tok::Num(v, u),
                Some(Err(_)) => Tok::Bad(raw.to_string()),
                None => Tok::Word(raw.to_string()),
            });
        }
    }
    out
}

/// Penalty per word moved from the command name into the arguments.
pub const WORD_PENALTY: i32 = 60;

/// Ways to read a query: (command text, arguments, penalty). The whole query as the name comes
/// first with no penalty; then the leading words before the first non-word, shortest last.
/// Russian mark: [`heads_in`].
pub fn heads(q: &str) -> Vec<(String, Args, i32)> {
    heads_in(q, DecimalMark::Comma)
}

/// [`heads`] with argument numbers read in the UI decimal mark `mark`.
pub fn heads_in(q: &str, mark: DecimalMark) -> Vec<(String, Args, i32)> {
    let toks = tokens_in(q, mark);
    let mut out = vec![(q.trim().to_string(), Args::default(), 0)];
    let lead = toks
        .iter()
        .position(|t| !matches!(t, Tok::Word(_)))
        .unwrap_or(toks.len());
    for k in (1..=lead).rev() {
        if k == toks.len() {
            continue; // same as the whole query
        }
        let head: Vec<&str> = toks[..k]
            .iter()
            .filter_map(|t| match t {
                Tok::Word(w) => Some(w.as_str()),
                _ => None,
            })
            .collect();
        let mut a = Args::default();
        let mut dropped = 0;
        for t in &toks[k..] {
            match t {
                Tok::Word(w) => {
                    dropped += 1;
                    a.words.push(w.clone());
                }
                Tok::Num(v, u) => a.nums.push((*v, *u)),
                Tok::Bad(b) => a.bad.push(b.clone()),
                Tok::Level(l, exact) => {
                    a.level = Some(l.clone());
                    a.level_exact = *exact;
                }
            }
        }
        out.push((head.join(" "), a, dropped * WORD_PENALTY));
    }
    out
}

/// Arguments for a chosen command: the best reading whose head is not empty.
pub fn args_of(q: &str, head_len_words: usize) -> Args {
    args_of_in(q, head_len_words, DecimalMark::Comma)
}

/// [`args_of`] with the UI decimal mark `mark`.
pub fn args_of_in(q: &str, head_len_words: usize, mark: DecimalMark) -> Args {
    heads_in(q, mark)
        .into_iter()
        .find(|(h, _, _)| h.split_whitespace().count() == head_len_words)
        .map(|x| x.1)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fx::fx;

    #[test]
    fn numbers_units_and_tokens() {
        assert_eq!(number("300"), Some((300.0, None)));
        assert_eq!(number(&format!("3,5{}", fx("m"))), Some((3.5, Some(Unit::M))));
        assert_eq!(number("30cm"), Some((30.0, Some(Unit::Cm))));
        assert_eq!(number("12'"), Some((12.0, Some(Unit::Ft))));
        assert_eq!(number("-5%"), Some((-5.0, Some(Unit::Pct))));
        assert_eq!(number(fx("concrete")), None);
        assert_eq!(number("2x"), None);
        assert_eq!(
            tokens("door 900x2100 @2"),
            vec![
                Tok::Word("door".into()),
                Tok::Num(900.0, None),
                Tok::Num(2100.0, None),
                Tok::Level("2".into(), false)
            ]
        );
        // unit as the next word; feet-inches
        let q = format!("{} 300 {}", fx("wall"), fx("cm"));
        assert_eq!(tokens(&q)[1], Tok::Num(300.0, Some(Unit::Cm)));
        assert_eq!(
            tokens("wall 12 ft 9")[1..],
            [Tok::Num(12.0, Some(Unit::Ft)), Tok::Num(9.0, None)]
        );
        assert_eq!(number("8'6\""), Some((102.0, Some(Unit::In))));
        assert_eq!(number("8'-6\""), Some((102.0, Some(Unit::In))));
        assert_eq!(number("8'"), Some((8.0, Some(Unit::Ft))));
        assert_eq!(args_of(&q, 1).length(0, true), Some(3.0));
        let a = &heads(&format!("{} 300 {}", fx("wall"), fx("concrete")))[1].1;
        assert_eq!(a.length(0, true), Some(0.3));
        assert_eq!(a.words, vec![fx("concrete").to_string()]);
        let a = &heads("wall 3,2m")[1].1;
        assert_eq!(a.length(0, true), Some(3.2));
        assert_eq!(args_of("wall 8", 1).length(0, false), Some(8.0 * 0.0254));
        // unitless metric is always millimetres (as in every length field)
        assert_eq!(args_of("wall 3", 1).length(0, true), Some(0.003));
        assert_eq!(args_of("wall 12", 1).length(0, true), Some(0.012));
        assert_eq!(args_of("door 900x2100 0", 1).length(2, true), None);
        assert_eq!(
            args_of("door 900x2100 0", 1).length_or_zero(2, true),
            Some(0.0)
        );
    }

    #[test]
    fn numbers_follow_the_ui_mark() {
        use DecimalMark::{Comma as RU, Point as EN};
        // English: comma groups thousands, a lone comma decimal is refused, not scaled
        assert_eq!(number_in("1,000", EN), Some(Ok((1000.0, None))));
        assert_eq!(number_in("12,500", EN), Some(Ok((12500.0, None))));
        assert_eq!(number_in("1,000.5mm", EN), Some(Ok((1000.5, Some(Unit::Mm)))));
        assert_eq!(number_in("1.5m", EN), Some(Ok((1.5, Some(Unit::M)))));
        assert_eq!(number_in("1,5", EN), Some(Err(NumError::CommaDecimal)));
        assert_eq!(number_in("1,5m", EN), Some(Err(NumError::CommaDecimal)));
        assert!(matches!(number_in("1,23,4", EN), Some(Err(_))));
        // Russian: comma decimal; a space is never a thousands group in arguments
        let ru_m = format!("3,5{}", fx("m"));
        assert_eq!(number_in(&ru_m, RU), Some(Ok((3.5, Some(Unit::M)))));
        assert_eq!(number_in("-2,5", RU), Some(Ok((-2.5, None))));
        assert_eq!(number_in("\u{2212}2,5", RU), Some(Ok((-2.5, None))));
        assert_eq!(number_in("1,000", RU), Some(Ok((1.0, None))));
        for m in [EN, RU] {
            assert_eq!(number_in("1\u{a0}000", m), None);
            assert_eq!(number_in("1\u{202f}000", m), None);
        }
        // feet-inches parts on the same parser
        for m in [EN, RU] {
            assert_eq!(number_in("8'6\"", m), Some(Ok((102.0, Some(Unit::In)))));
            assert_eq!(number_in("8'-6\"", m), Some(Ok((102.0, Some(Unit::In)))));
        }
        assert_eq!(number_in("8'6,5\"", RU), Some(Ok((102.5, Some(Unit::In)))));
        assert_eq!(number_in("8'6.5\"", EN), Some(Ok((102.5, Some(Unit::In)))));
        assert!(matches!(number_in("8'6,5\"", EN), Some(Err(_))));
        // words stay words in both marks
        for m in [EN, RU] {
            assert_eq!(number_in("wall", m), None);
            assert_eq!(number_in("2x", m), None);
            assert_eq!(number_in("-", m), None);
        }
    }

    #[test]
    fn query_tokens_follow_the_ui_mark() {
        use DecimalMark::{Comma as RU, Point as EN};
        let a = args_of_in("wall 1,000 3,000", 1, EN);
        assert_eq!(a.nums, vec![(1000.0, None), (3000.0, None)]);
        assert!(a.bad.is_empty());
        // ambiguous English comma: no value at all, the typed text kept for the message
        let a = args_of_in("wall 1,5 concrete", 1, EN);
        assert!(a.nums.is_empty(), "{a:?}");
        assert_eq!(a.bad, vec!["1,5".to_string()]);
        assert_eq!(a.words, vec!["concrete".to_string()]);
        assert!(!a.is_empty());
        assert_eq!(args_of_in("wall 1,5 m", 1, EN).bad, vec!["1,5 m".to_string()]);
        assert_eq!(args_of_in("door 1,5x2100", 1, EN).bad, vec!["1,5x2100".to_string()]);
        assert!(args_of_in("door 1,5x2100", 1, EN).nums.is_empty());
        assert_eq!(
            args_of_in("door 1,000x2,100", 1, EN).nums,
            vec![(1000.0, None), (2100.0, None)]
        );
        // whitespace always separates arguments, in both marks: "900 600" is never 900600
        for m in [EN, RU] {
            assert_eq!(
                args_of_in(&format!("{} 900 600", fx("window")), 1, m).nums,
                vec![(900.0, None), (600.0, None)],
                "{m:?}"
            );
            assert_eq!(
                args_of_in("window 900\u{a0}600", 1, m).nums,
                vec![(900.0, None), (600.0, None)],
                "{m:?}"
            );
            assert_eq!(
                args_of_in("window 900\u{202f}600", 1, m).nums,
                vec![(900.0, None), (600.0, None)],
                "{m:?}"
            );
            assert_eq!(
                args_of_in(&format!("{} 1 000", fx("wall")), 1, m).nums,
                vec![(1.0, None), (0.0, None)],
                "{m:?}"
            );
            assert_eq!(
                args_of_in(&format!("{} 200 300", fx("wall")), 1, m).nums,
                vec![(200.0, None), (300.0, None)],
                "{m:?}"
            );
        }
        assert_eq!(
            args_of_in(&format!("{} 1,5 2,5", fx("wall")), 1, RU).nums,
            vec![(1.5, None), (2.5, None)]
        );
        assert_eq!(
            args_of_in("wall 1,000 2", 1, EN).nums,
            vec![(1000.0, None), (2.0, None)]
        );
        assert_eq!(
            args_of_in(&format!("{} 1,5 {}", fx("wall"), fx("mm")), 1, RU).nums,
            vec![(1.5, Some(Unit::Mm))]
        );
        assert_eq!(args_of_in(&format!("wall 3,5{}", fx("m")), 1, RU).nums, vec![(3.5, Some(Unit::M))]);
        assert_eq!(args_of_in("wall -2,5", 1, RU).nums, vec![(-2.5, None)]);
        // after a level name the same mark applies
        let mut a = args_of_in("wall @L2 1,5", 1, EN);
        assert_eq!(a.take_level_in(&["L2"], EN), Some(Ok(0)));
        assert_eq!(a.bad, vec!["1,5".to_string()]);
        let l2 = format!("{} 2", fx("level"));
        let mut a = args_of_in(&format!("{} @{l2} 1,5", fx("wall")), 1, RU);
        assert_eq!(a.take_level_in(&[l2.as_str()], RU), Some(Ok(0)));
        assert_eq!(a.nums, vec![(1.5, None)]);
    }

    #[test]
    fn level_names_with_spaces() {
        let (lv, wall) = (fx("level"), fx("wall"));
        let owned = [format!("{lv} 1"), format!("{lv} 2"), format!("{lv} 10"), fx("roof").to_string()];
        let names: Vec<&str> = owned.iter().map(String::as_str).collect();
        // the whole tail: the longest level name wins, numbers after it stay arguments
        let mut a = args_of(&format!("{} 3000 @{lv} 2", fx("column")), 1);
        assert_eq!(a.take_level(&names), Some(Ok(1)));
        assert_eq!(a.nums, vec![(3000.0, None)]);
        let mut a = args_of(&format!("{wall} 200 @{} 10 3000", fx("level_lc")), 1);
        assert_eq!(a.take_level(&names), Some(Ok(2)));
        assert_eq!(a.nums, vec![(200.0, None), (3000.0, None)]);
        // quoted name, arguments after it
        let mut a = args_of(&format!("{wall} @\"{lv} 2\" 250"), 1);
        assert!(a.level_exact);
        assert_eq!(a.take_level(&names), Some(Ok(1)));
        assert_eq!(a.nums, vec![(250.0, None)]);
        let mut a = args_of(&format!("{wall} @«{lv} 1» 250"), 1);
        assert_eq!(a.take_level(&names), Some(Ok(0)));
        assert_eq!(a.nums, vec![(250.0, None)]);
        // a name prefix still works; unknown names are reported, numbers kept
        let mut a = args_of(&format!("{wall} @{} 3000", fx("roof_pre")), 1);
        assert_eq!(a.take_level(&names), Some(Ok(3)));
        assert_eq!(a.nums, vec![(3000.0, None)]);
        let mut a = args_of(&format!("{wall} @{} 3000", fx("basement")), 1);
        assert_eq!(a.take_level(&names), Some(Err(fx("basement").into())));
        assert_eq!(a.nums, vec![(3000.0, None)]);
        let mut a = args_of(&format!("{wall} @"), 1);
        assert_eq!(a.take_level(&names), Some(Err(String::new())));
        assert_eq!(args_of(&format!("{wall} 300"), 1).take_level(&names), None);
    }

    #[test]
    fn heads_order_and_penalty() {
        let h = heads("export ifc");
        assert_eq!(h[0].0, "export ifc");
        assert_eq!(h[1].0, "export");
        assert_eq!(h[1].2, WORD_PENALTY);
        assert_eq!(h[1].1.words, vec!["ifc".to_string()]);
        // numbers stop the name; nothing is dropped from it
        let h = heads("wall 300 concrete");
        assert_eq!(h.len(), 2);
        assert_eq!(h[1].0, "wall");
        assert_eq!(h[1].2, WORD_PENALTY);
        assert!(heads("").len() == 1);
    }
}
