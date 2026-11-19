//! Units from the header: `[mm]`, `[m]`, `[ft]`, `[in]`, `[m2]`, `[ft2]`, `[m3]`, `[ft3]`...
//! A length cell may hold `3500 mm`, `3.5 m`, `10'-6"`, `10' 6 1/2"`, `6"`, `12 ft`.
//! Text numbers: see [`number`]; a value that may mean two numbers is never guessed.

/// Dimension of a column by its canonical header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dim {
    None,
    Len,
    Area,
    Vol,
}

/// Reading of a delete-column cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DelWord {
    Yes,
    No,
    /// Not a known word (the trimmed cell text): the row is refused, never guessed.
    Unknown(String),
}

/// Words that mark a row for deletion (compared case-folded and trimmed). Non-ASCII ones are
/// escapes: Cyrillic x, the Russian words for "yes" and "delete", check marks.
const DEL_YES: [&str; 11] = [
    "delete", "x", "\u{0445}", "1", "true", "yes", "\u{0434}\u{0430}",
    "\u{0443}\u{0434}\u{0430}\u{043b}\u{0438}\u{0442}\u{044c}", "+", "\u{2713}", "\u{2714}",
];
/// Words that mean "keep" (an empty cell does too).
const DEL_NO: [&str; 5] = ["0", "no", "false", "\u{043d}\u{0435}\u{0442}", "-"];

/// The one parser of the delete column, shared by the main sheets, the family sheets and the
/// extra-parameter pass, so all three read a cell the same way.
pub(crate) fn delete_word(d: &calamine::Data) -> DelWord {
    use calamine::Data;
    match d {
        Data::Empty => DelWord::No,
        Data::Bool(b) => if *b { DelWord::Yes } else { DelWord::No },
        d => {
            let shown = d.to_string();
            let t = shown.trim();
            let folded = t.to_lowercase();
            if folded.is_empty() || DEL_NO.contains(&folded.as_str()) {
                DelWord::No
            } else if DEL_YES.contains(&folded.as_str()) {
                DelWord::Yes
            } else {
                DelWord::Unknown(t.to_string())
            }
        }
    }
}

pub(crate) fn dim(canonical: &str) -> Dim {
    if canonical.contains("[m3]") {
        Dim::Vol
    } else if canonical.contains("[m2]") {
        Dim::Area
    } else if canonical.contains("[m]") {
        Dim::Len
    } else {
        Dim::None
    }
}

/// Header without the bracketed units, lower case: `elevation [ft]` -> `elevation`.
pub(crate) fn norm(h: &str) -> String {
    let mut out = String::with_capacity(h.len());
    let mut depth = 0;
    for ch in h.chars() {
        match ch {
            '[' => depth += 1,
            ']' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.extend(ch.to_lowercase()),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text cell vs model text: equal when they differ only in outer spaces, CRLF/CR vs LF or
/// no-break / thin spaces. Inner runs of spaces still count, so a real edit is never hidden.
pub(crate) fn same_text(a: &str, b: &str) -> bool {
    fn canon(s: &str) -> String {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        let s: String = s
            .chars()
            .map(|c| match c {
                '\u{00A0}' | '\u{2007}' | '\u{2009}' | '\u{202F}' => ' ',
                c => c,
            })
            .collect();
        s.trim().to_string()
    }
    a == b || canon(a) == canon(b)
}

/// Length unit; Cyrillic is written with escape codes (no Cyrillic literals in .rs).
fn len_unit(u: &str) -> Option<f64> {
    let u = u.trim().trim_end_matches('.').trim().to_lowercase();
    Some(match u.as_str() {
        "\u{43c}\u{43c}" => 1e-3,
        "\u{441}\u{43c}" => 1e-2,
        "\u{434}\u{43c}" => 0.1,
        "\u{43c}" => 1.0,
        "\u{43a}\u{43c}" => 1e3,
        "\u{444}\u{443}\u{442}" | "\u{444}\u{442}" => 0.3048,
        "\u{434}\u{44e}\u{439}\u{43c}" => 0.0254,
        "mm" => 1e-3,
        "cm" => 1e-2,
        "dm" => 0.1,
        "m" => 1.0,
        "km" => 1e3,
        "ft" | "'" | "feet" | "foot" => 0.3048,
        "in" | "\"" | "inch" | "inches" => 0.0254,
        "yd" => 0.9144,
        _ => return None,
    })
}

/// Factor "header unit -> SI" from the brackets `[...]` when the dimension matches.
pub(crate) fn header_factor(header: &str, d: Dim) -> Option<f64> {
    let a = header.find('[')?;
    let b = a + header[a..].find(']')?;
    let u = header[a + 1..b].trim().to_lowercase();
    let u = u.replace('²', "2").replace('³', "3").replace('^', "");
    let (base, pow) = match d {
        Dim::Len => (u.as_str(), 1),
        Dim::Area => (u.strip_suffix('2')?, 2),
        Dim::Vol => (u.strip_suffix('3')?, 3),
        Dim::None => return None,
    };
    len_unit(base.trim()).map(|k| k.powi(pow))
}

/// Why a text cell is not a number.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum NumErr {
    /// Not a number at all.
    Invalid,
    /// One comma before exactly three digits (`1,250`): thousands or decimal, the text alone
    /// does not tell. Both readings are kept so the review can name them.
    Ambiguous { grouped: f64, decimal: f64 },
}

/// Decimal separator of the book, when it is known. No book records it yet, so callers pass
/// None and the ambiguous case goes to the review.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dec {
    Point,
    Comma,
}

thread_local! {
    static BOOK_DEC: std::cell::Cell<Option<Dec>> = const { std::cell::Cell::new(None) };
}

/// Keeps the decimal separator of the book being imported for [`number`] calls that pass
/// None; the previous value comes back on drop.
pub(crate) struct DecGuard(Option<Dec>);

impl DecGuard {
    pub(crate) fn set(d: Option<Dec>) -> Self {
        DecGuard(BOOK_DEC.with(|c| c.replace(d)))
    }
}

/// Decimal separator of the book being imported (None outside an import or when unknown).
pub(crate) fn book_dec() -> Option<Dec> {
    BOOK_DEC.with(|c| c.get())
}

impl Drop for DecGuard {
    fn drop(&mut self) {
        BOOK_DEC.with(|c| c.set(self.0));
    }
}

/// Full locale tag first (region exceptions), then the language list; a region of a
/// language that mixes both conventions and is not listed gives None (review).
fn dec_from_tag(tag: &str) -> Option<Dec> {
    let mut parts = tag.split(['-', '_']);
    let lang = parts.next().unwrap_or("");
    let region = parts.next().unwrap_or("");
    match (lang, region) {
        ("de" | "it", "ch") | ("de", "li") => return Some(Dec::Point),
        ("en", "za") => return Some(Dec::Comma),
        ("es", "mx" | "us" | "gt" | "hn" | "ni" | "pa" | "sv" | "do" | "pr" | "pe") => {
            return Some(Dec::Point)
        }
        ("es", "" | "es" | "ar" | "cl" | "co" | "cr" | "ec" | "py" | "uy" | "ve" | "bo" | "gq") => {
            return Some(Dec::Comma)
        }
        ("es", _) => return None,
        _ => {}
    }
    match lang {
        "en" | "ja" | "zh" | "ko" | "he" | "th" => Some(Dec::Point),
        "ru" | "de" | "fr" | "it" | "pt" | "nl" | "pl" | "cs" | "sk" | "uk" | "tr" | "sv" | "da"
        | "nb" | "no" | "fi" | "hu" | "ro" | "bg" => Some(Dec::Comma),
        _ => None,
    }
}

/// Info sheet rows `decimal` (point / comma) or `locale` (`de-DE`, `en_US`) -> separator.
pub(crate) fn dec_from_info<'a>(rows: impl Iterator<Item = (&'a str, &'a str)>) -> Option<Dec> {
    for (k, v) in rows {
        let v = v.trim().to_ascii_lowercase();
        match k.trim() {
            "decimal" if v == "point" => return Some(Dec::Point),
            "decimal" if v == "comma" => return Some(Dec::Comma),
            "locale" => return dec_from_tag(&v),
            _ => {}
        }
    }
    None
}

fn is_space_group(c: char) -> bool {
    matches!(c, ' ' | '\u{a0}' | '\u{202f}' | '\u{2009}')
}

fn is_quote_group(c: char) -> bool {
    matches!(c, '\'' | '\u{2019}')
}

/// Integer part with optional grouping by `sep`: first group 1-3 digits, the rest exactly 3.
/// No separator at all: any run of digits (empty allowed, as in `.5`).
/// A leading zero is no thousands group: `0,250` in a point book is refused, never 250.
fn int_part(t: &str, sep: impl Fn(char) -> bool) -> Option<String> {
    if !t.chars().any(&sep) {
        return t.chars().all(|c| c.is_ascii_digit()).then(|| t.to_string());
    }
    let groups: Vec<&str> = t.split(|c: char| sep(c)).collect();
    let ok = groups.iter().all(|g| g.chars().all(|c| c.is_ascii_digit()))
        && (1..=3).contains(&groups[0].len())
        && !groups[0].starts_with('0')
        && groups[1..].iter().all(|g| g.len() == 3);
    ok.then(|| groups.concat())
}

fn assemble(neg: bool, int: &str, frac: &str) -> Result<f64, NumErr> {
    if int.is_empty() && frac.is_empty() {
        return Err(NumErr::Invalid);
    }
    if !int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return Err(NumErr::Invalid);
    }
    let int = if int.is_empty() { "0" } else { int };
    let frac = if frac.is_empty() { "0" } else { frac };
    let sign = if neg { "-" } else { "" };
    format!("{sign}{int}.{frac}")
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or(NumErr::Invalid)
}

/// Text -> number without guessing. Accepts `1250`, `-3.5`, `3,5`, `1,250.5`, `1.250,5`,
/// `1,250,000`, `1.250.000`, `1 250,5` (space, no-break or thin space), `1'250.5`, a U+2212
/// minus and exponents (`1.5e3`). One comma before exactly three digits after a short integer
/// part (`1,250`) is [`NumErr::Ambiguous`] unless `dec` says which separator the book uses.
/// One point before exactly three digits after a short integer part (`1.250`) is ambiguous the
/// same way. With `dec` None the separator of the book being imported (see [`DecGuard`]) is used.
pub(crate) fn number(s: &str, dec: Option<Dec>) -> Result<f64, NumErr> {
    let dec = dec.or_else(|| BOOK_DEC.with(|c| c.get()));
    let t = s.trim();
    let (neg, body) = match t.strip_prefix(['-', '\u{2212}']) {
        Some(r) => (true, r.trim_start()),
        None => (false, t.strip_prefix('+').unwrap_or(t).trim_start()),
    };
    if body.is_empty() {
        return Err(NumErr::Invalid);
    }
    let spaces = body.chars().any(is_space_group);
    let quotes = body.chars().any(is_quote_group);
    let (c, p) = (body.matches(',').count(), body.matches('.').count());
    if body.contains(['e', 'E']) {
        if spaces || quotes || c > 0 {
            return Err(NumErr::Invalid);
        }
        let v: f64 = body.parse().map_err(|_| NumErr::Invalid)?;
        let v = if neg { -v } else { v };
        return v.is_finite().then_some(v).ok_or(NumErr::Invalid);
    }
    if spaces && quotes {
        return Err(NumErr::Invalid);
    }
    let grouped_by_mark = spaces || quotes;
    // (decimal mark, grouping mark), as settled by the text or the book
    let (decm, grpm): (Option<char>, Option<char>) = if c > 0 && p > 0 {
        let d = if body.rfind(',') > body.rfind('.') { ',' } else { '.' };
        let g = if d == ',' { '.' } else { ',' };
        if body.matches(d).count() != 1 || grouped_by_mark {
            return Err(NumErr::Invalid);
        }
        (Some(d), Some(g))
    } else if c > 1 {
        (None, Some(','))
    } else if p > 1 {
        (None, Some('.'))
    } else if c == 1 {
        if grouped_by_mark {
            (Some(','), None)
        } else {
            match dec {
                Some(Dec::Comma) => (Some(','), None),
                Some(Dec::Point) => (None, Some(',')),
                None => {
                    let (i, f) = body.split_once(',').unwrap_or((body, ""));
                    let digits = i.chars().chain(f.chars()).all(|ch| ch.is_ascii_digit());
                    if digits && (1..=3).contains(&i.len()) && i != "0" && f.len() == 3 {
                        let grouped = assemble(neg, &format!("{i}{f}"), "")?;
                        let decimal = assemble(neg, i, f)?;
                        return Err(NumErr::Ambiguous { grouped, decimal });
                    }
                    (Some(','), None)
                }
            }
        }
    } else if p == 1 {
        if grouped_by_mark {
            (Some('.'), None)
        } else {
            match dec {
                Some(Dec::Comma) => (None, Some('.')),
                Some(Dec::Point) => (Some('.'), None),
                None => {
                    let (i, f) = body.split_once('.').unwrap_or((body, ""));
                    let digits = i.chars().chain(f.chars()).all(|ch| ch.is_ascii_digit());
                    if digits && (1..=3).contains(&i.len()) && i != "0" && f.len() == 3 {
                        let grouped = assemble(neg, &format!("{i}{f}"), "")?;
                        let decimal = assemble(neg, i, f)?;
                        return Err(NumErr::Ambiguous { grouped, decimal });
                    }
                    (Some('.'), None)
                }
            }
        }
    } else {
        (None, None)
    };
    let (ip, fp) = match decm {
        Some(d) => body.split_once(d).unwrap_or((body, "")),
        None => (body, ""),
    };
    if decm.is_some() && fp.is_empty() {
        return Err(NumErr::Invalid);
    }
    let int = match grpm {
        Some(_) if grouped_by_mark => None,
        Some(g) => int_part(ip, |ch| ch == g),
        None if spaces => int_part(ip, is_space_group),
        None if quotes => int_part(ip, is_quote_group),
        None => int_part(ip, |_| false),
    }
    .ok_or(NumErr::Invalid)?;
    assemble(neg, &int, fp)
}

/// Number with an optional simple fraction (`1/2`).
fn num(t: &str) -> Result<f64, NumErr> {
    let t = t.trim();
    if let Some((a, b)) = t.split_once('/') {
        let (a, b) = (num(a)?, num(b)?);
        return if b != 0.0 { Ok(a / b) } else { Err(NumErr::Invalid) };
    }
    number(t, None)
}

/// Inches with a fraction: `6`, `6.5`, `6 1/2`, `1/2`.
fn inches(t: &str) -> Result<f64, NumErr> {
    let parts: Vec<&str> = t.split_whitespace().collect();
    match parts.as_slice() {
        [] => Ok(0.0),
        [a] => num(a),
        [a, b] if b.contains('/') => Ok(num(a)? + num(b)?),
        _ => Err(NumErr::Invalid),
    }
}

/// Text length -> metres; `default` is the factor for a bare number. None also for an
/// ambiguous number; [`len_value`] tells the two apart.
pub(crate) fn parse_len(s: &str, default: f64) -> Option<f64> {
    len_value(s, default).ok()
}

/// Text length -> metres, or why it is refused.
pub(crate) fn len_value(s: &str, default: f64) -> Result<f64, NumErr> {
    let t = s
        .trim()
        .replace(['\u{2019}', '\u{2032}'], "'")
        .replace(['\u{201d}', '\u{2033}'], "\"");
    if t.is_empty() {
        return Err(NumErr::Invalid);
    }
    match num(&t) {
        Ok(v) => return Ok(v * default),
        Err(e @ NumErr::Ambiguous { .. }) => return Err(e),
        Err(NumErr::Invalid) => {}
    }
    // feet-inches: 10'-6", 10' 6", -10'-6 1/2", 10', 6"
    if t.contains('\'') || t.ends_with('"') {
        let (neg, body) = match t.strip_prefix(['-', '\u{2212}']) {
            Some(r) => (true, r.trim()),
            None => (false, t.as_str()),
        };
        let (ft, rest) = match body.split_once('\'') {
            Some((f, r)) => (num(f)?, r),
            None => (0.0, body),
        };
        let rest = rest.trim().trim_start_matches('-').trim();
        let rest = rest.strip_suffix('"').unwrap_or(rest).trim();
        let inch = inches(rest)?;
        let v = ft * 0.3048 + inch * 0.0254;
        return Ok(if neg { -v } else { v });
    }
    // number + unit: 3500 mm, 3.5m, 12 ft
    let i = t
        .char_indices()
        .find(|(_, c)| c.is_alphabetic())
        .map(|(i, _)| i)
        .ok_or(NumErr::Invalid)?;
    let (n, u) = t.split_at(i);
    let k = len_unit(u).ok_or(NumErr::Invalid)?;
    Ok(num(n)? * k)
}

/// Review text for a refused number cell; `invalid` is the text for a plain non-number.
pub(crate) fn num_reason(e: NumErr, s: &str, invalid: String) -> String {
    match e {
        NumErr::Ambiguous { grouped, decimal } => {
            let t = s.trim();
            format!("ambiguous number '{t}': write {grouped} (thousands) or {decimal} (decimal)")
        }
        NumErr::Invalid => invalid,
    }
}

/// A number cell in a text column read against the model text (see [`num_text`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NumText {
    /// The model text reads as this number (`01`, `1.10`, `007`, `1E3`): no edit, the model
    /// text stays byte-exact.
    Same,
    /// A real edit; the new text in its plain form (`102`, never `102.0`).
    Edit(String),
    /// The number cannot hold the code (more than 15 significant digits): refused.
    Lossy,
}

/// A spreadsheet keeps 15 significant digits; numbers compare at that precision.
fn round15(x: f64) -> f64 {
    format!("{x:.14e}").parse().unwrap_or(x)
}

/// Plain text of a number cell: `102` (never `102.0`), noise past 15 digits dropped.
pub(crate) fn num_cell_text(x: f64) -> String {
    let r = round15(x);
    if r.fract() == 0.0 && r.abs() < 1e15 {
        format!("{}", r as i64)
    } else {
        format!("{r}")
    }
}

/// Significant digits of a code: mantissa digits without leading and trailing zeros.
fn sig_digits(model: &str) -> usize {
    let mant = model.split(['e', 'E']).next().unwrap_or("");
    let d: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    d.trim_start_matches('0').trim_end_matches('0').len()
}

/// Numbers the model text may have become in a spreadsheet; `dec` limits the separator guesses.
fn text_readings(model: &str, dec: Option<Dec>) -> Vec<f64> {
    // only a strict reading under the book's rule: groups of exactly three digits, one decimal
    // mark, so `1,5` is never 15 in a point book (that would swallow a real edit)
    let space = ['\'', ' ', '\u{a0}', '\u{202f}'];
    let rules: &[(char, char)] = match dec {
        Some(Dec::Point) => &[('.', ',')],
        Some(Dec::Comma) => &[(',', '.')],
        None => &[('.', ','), (',', '.')],
    };
    rules
        .iter()
        .filter_map(|&(mark, group)| strict_number(model, mark, &[group, space[0], space[1], space[2], space[3]]))
        .collect()
}

/// `model` as a number with decimal mark `mark` and optional thousands groups (one group
/// character, groups of exactly three digits); None when it does not read that way.
fn strict_number(model: &str, mark: char, groups: &[char]) -> Option<f64> {
    let s = model.trim();
    let (sign, s) = match s.strip_prefix(['+', '-']) {
        Some(rest) => (&model.trim()[..1], rest),
        None => ("", s),
    };
    let (mant, exp) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    if let Some(e) = exp {
        let digits = e.strip_prefix(['+', '-']).unwrap_or(e);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    let (int, frac) = match mant.split_once(mark) {
        Some((i, f)) => (i, Some(f)),
        None => (mant, None),
    };
    if let Some(f) = frac {
        if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    let int_digits: String = match int.chars().find(|c| groups.contains(c)) {
        None => {
            if !int.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            int.to_string()
        }
        Some(g) => {
            let parts: Vec<&str> = int.split(g).collect();
            let head_ok = (1..=3).contains(&parts[0].len());
            let all_digits = parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit()));
            if !head_ok || !all_digits || parts[1..].iter().any(|p| p.len() != 3) {
                return None;
            }
            parts.concat()
        }
    };
    if int_digits.is_empty() && frac.is_none() {
        return None;
    }
    let mut t = format!("{sign}{}", if int_digits.is_empty() { "0" } else { &int_digits });
    if let Some(f) = frac {
        t.push('.');
        t.push_str(f);
    }
    if let Some(e) = exp {
        t.push('e');
        t.push_str(e);
    }
    t.parse::<f64>().ok().filter(|x| x.is_finite())
}

/// A cell of a text column against the model text `model` (empty for a new element), with the
/// book decimal mark `dec`. A number the model text reads as is [`NumText::Same`]; a code past
/// 15 significant digits, or a number of 16+ integer digits, is [`NumText::Lossy`].
pub(crate) fn num_text(cell: &calamine::Data, model: &str, dec: Option<Dec>) -> NumText {
    use calamine::Data;
    let n = match cell {
        Data::Float(x) => *x,
        Data::Int(i) => *i as f64,
        Data::String(s) if same_text(s, model) => return NumText::Same,
        Data::String(s) => return NumText::Edit(s.trim().to_string()),
        d => return NumText::Edit(d.to_string().trim().to_string()),
    };
    if !n.is_finite() {
        return NumText::Edit(n.to_string());
    }
    let text = num_cell_text(n);
    let m = model.trim();
    if m == text {
        return NumText::Same;
    }
    let r = round15(n);
    if !m.is_empty() && text_readings(m, dec).into_iter().any(|x| round15(x) == r) {
        return if sig_digits(m) > 15 { NumText::Lossy } else { NumText::Same };
    }
    if r.abs() >= 1e15 {
        return NumText::Lossy;
    }
    NumText::Edit(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-12)
    }

    #[test]
    fn lengths_metric_imperial_and_mixed() {
        assert!(close(parse_len("3500 mm", 1.0), 3.5));
        assert!(close(parse_len("3,5", 1.0), 3.5));
        assert!(close(parse_len("3.5m", 1.0), 3.5));
        assert!(close(parse_len("2", 1e-3), 0.002));
        assert!(close(parse_len("10'-6\"", 1.0), 3.2004));
        assert!(close(parse_len("10' 6\"", 1.0), 3.2004));
        assert!(close(parse_len("10'", 1.0), 3.048));
        assert!(close(parse_len("6\"", 1.0), 0.1524));
        assert!(close(parse_len("1' 6 1/2\"", 1.0), 0.3048 + 6.5 * 0.0254));
        assert!(close(parse_len("-2'-0\"", 1.0), -0.6096));
        assert!(close(parse_len("12 ft", 1.0), 3.6576));
        assert!(parse_len("abc", 1.0).is_none());
        assert!(parse_len("10'-x\"", 1.0).is_none());
        assert!(parse_len("5 parsecs", 1.0).is_none());
    }

    #[test]
    fn cyrillic_units_in_cells() {
        let cases = [
            ("3500 \u{43c}\u{43c}", 3.5),
            ("350 \u{441}\u{43c}", 3.5),
            ("35 \u{434}\u{43c}", 3.5),
            ("3.5 \u{43c}", 3.5),
            ("0,0035 \u{43a}\u{43c}", 3.5),
            ("3,5 \u{43c}", 3.5),
            ("3,5\u{a0}\u{43c}", 3.5),
            ("\u{a0}3500\u{a0}\u{41c}\u{41c}\u{a0}", 3.5),
            ("3500 \u{43c}\u{43c}.", 3.5),
            ("12 \u{444}\u{443}\u{442}", 3.6576),
        ];
        for (t, want) in cases {
            assert!(close(parse_len(t, 1.0), want), "{t}");
        }
        assert!(parse_len("5 \u{43f}\u{430}\u{440}\u{441}\u{435}\u{43a}", 1.0).is_none());
    }

    #[test]
    fn cyrillic_units_in_headers() {
        let h = |name: &str, unit: &str| format!("{name} [{unit}]");
        let (mm, cm, dm, m, km) = (
            "\u{43c}\u{43c}",
            "\u{441}\u{43c}",
            "\u{434}\u{43c}",
            "\u{43c}",
            "\u{43a}\u{43c}",
        );
        assert_eq!(header_factor(&h("h", mm), Dim::Len), Some(1e-3));
        assert_eq!(header_factor(&h("h", cm), Dim::Len), Some(1e-2));
        assert_eq!(header_factor(&h("h", dm), Dim::Len), Some(0.1));
        assert_eq!(header_factor(&h("h", m), Dim::Len), Some(1.0));
        assert_eq!(header_factor(&h("h", km), Dim::Len), Some(1e3));
        assert_eq!(header_factor(&h("h", "\u{41c}\u{41c}"), Dim::Len), Some(1e-3));
        assert_eq!(header_factor("h [\u{a0}\u{43c}\u{a0}]", Dim::Len), Some(1.0));
        for a in ["\u{43c}2", "\u{43c}\u{b2}", "\u{43c}^2"] {
            assert_eq!(header_factor(&h("a", a), Dim::Area), Some(1.0), "{a}");
        }
        for v in ["\u{43c}3", "\u{43c}\u{b3}", "\u{43c}^3"] {
            assert_eq!(header_factor(&h("v", v), Dim::Vol), Some(1.0), "{v}");
        }
        let k = header_factor(&h("a", "\u{441}\u{43c}\u{b2}"), Dim::Area).unwrap();
        assert!((k - 1e-4).abs() < 1e-16);
        assert_eq!(header_factor(&h("h", "\u{43c}\u{43c}2"), Dim::Len), None);
    }

    #[test]
    fn unknown_unit_header_is_none() {
        assert_eq!(
            header_factor("h [\u{43f}\u{430}\u{440}\u{441}\u{435}\u{43a}]", Dim::Len),
            None
        );
        assert_eq!(header_factor("h [xyz]", Dim::Len), None);
    }

    #[test]
    fn text_numbers_parse_only_when_unambiguous() {
        let ok: &[(&str, f64)] = &[
            ("1250", 1250.0),
            ("-3.5", -3.5),
            ("+2", 2.0),
            ("3,5", 3.5),
            ("0,250", 0.25),
            ("12,50", 12.5),
            ("1234,567", 1234.567),
            (".5", 0.5),
            ("1.2500", 1.25),
            ("1,250.5", 1250.5),
            ("1.250,5", 1250.5),
            ("1,250,000", 1_250_000.0),
            ("1.250.000", 1_250_000.0),
            ("1,250,000.25", 1_250_000.25),
            ("1 250", 1250.0),
            ("1 250,5", 1250.5),
            ("1 250.5", 1250.5),
            ("1\u{a0}250,5", 1250.5),
            ("1\u{202f}250\u{202f}000", 1_250_000.0),
            ("1\u{2009}250", 1250.0),
            ("1'250.5", 1250.5),
            ("1\u{2019}250", 1250.0),
            ("\u{2212}1 250,5", -1250.5),
            ("1.5e3", 1500.0),
        ];
        for (s, v) in ok {
            assert_eq!(number(s, None), Ok(*v), "{s:?}");
        }
        let bad = [
            "", "-", "abc", "1,2,3", "1,25,000", "12 5", "1 250'000", "1,250.000,5", "1.2.3,4",
            "1,", "1e", "inf", "NaN", "1,5e3", "1 250,000.5", "--5", "1'250 5",
        ];
        for s in bad {
            assert_eq!(number(s, None), Err(NumErr::Invalid), "{s:?}");
        }
    }

    #[test]
    fn ambiguous_comma_thousands_rejected() {
        let cases: &[(&str, f64, f64)] = &[
            ("1,250", 1250.0, 1.25),
            ("12,500", 12500.0, 12.5),
            ("999,999", 999_999.0, 999.999),
            ("-1,250", -1250.0, -1.25),
            ("\u{2212}3,000", -3000.0, -3.0),
        ];
        for (s, g, d) in cases {
            let e = NumErr::Ambiguous { grouped: *g, decimal: *d };
            assert_eq!(number(s, None), Err(e), "{s:?}");
            // never a value: lengths with or without a unit refuse it too
            assert_eq!(len_value(s, 1.0), Err(e), "{s:?}");
            assert_eq!(parse_len(s, 1.0), None, "{s:?}");
            assert_eq!(len_value(&format!("{s} mm"), 1.0), Err(e), "{s:?}");
        }
        assert_eq!(
            num_reason(number("1,250", None).unwrap_err(), " 1,250 ", String::new()),
            "ambiguous number '1,250': write 1250 (thousands) or 1.25 (decimal)"
        );
        assert_eq!(num_reason(NumErr::Invalid, "x", "fallback".into()), "fallback");
    }

    #[test]
    fn book_separator_settles_the_ambiguous_case() {
        let cases: &[(&str, Dec, Result<f64, NumErr>)] = &[
            ("1,250", Dec::Point, Ok(1250.0)),
            ("1,250", Dec::Comma, Ok(1.25)),
            ("1.250", Dec::Comma, Ok(1250.0)),
            ("1.250", Dec::Point, Ok(1.25)),
            ("1,25", Dec::Point, Err(NumErr::Invalid)),
            ("1.25", Dec::Comma, Err(NumErr::Invalid)),
            ("0,250", Dec::Point, Err(NumErr::Invalid)),
            ("-0,201", Dec::Point, Err(NumErr::Invalid)),
            ("0.250", Dec::Comma, Err(NumErr::Invalid)),
            ("1,250.5", Dec::Comma, Ok(1250.5)),
            ("1 250,5", Dec::Point, Ok(1250.5)),
        ];
        for (s, d, want) in cases {
            assert_eq!(number(s, Some(*d)), *want, "{s:?} {d:?}");
        }
    }

    #[test]
    fn point_thousands_three_digits_ambiguous() {
        let cases: &[(&str, f64, f64)] = &[
            ("1.250", 1250.0, 1.25),
            ("12.500", 12500.0, 12.5),
            ("-1.250", -1250.0, -1.25),
        ];
        for (s, g, d) in cases {
            let e = NumErr::Ambiguous { grouped: *g, decimal: *d };
            assert_eq!(number(s, None), Err(e), "{s:?}");
            assert_eq!(parse_len(s, 1.0), None, "{s:?}");
        }
        // not three fraction digits, or a leading zero: still decimal
        assert_eq!(number("0.250", None), Ok(0.25));
        assert_eq!(number("1.25", None), Ok(1.25));
        assert_eq!(number("1234.567", None), Ok(1234.567));
    }

    fn loc(tag: &str) -> Option<Dec> {
        dec_from_info([("locale", tag)].into_iter())
    }

    #[test]
    fn info_locale_de_ch_point() {
        assert_eq!(loc("de-CH"), Some(Dec::Point));
        assert_eq!(loc("de_LI"), Some(Dec::Point));
        let _g = DecGuard::set(loc("de-CH"));
        assert_eq!(number("1.250", None), Ok(1.25));
    }

    #[test]
    fn info_locale_es_mx_point() {
        assert_eq!(loc("es-MX"), Some(Dec::Point));
        assert_eq!(loc("es_US"), Some(Dec::Point));
        assert_eq!(loc("es-419"), None);
        let _g = DecGuard::set(loc("es-MX"));
        assert_eq!(number("1.250", None), Ok(1.25));
    }

    #[test]
    fn info_locale_de_de_comma() {
        assert_eq!(loc("de-DE"), Some(Dec::Comma));
        assert_eq!(loc("de"), Some(Dec::Comma));
        assert_eq!(loc("es-ES"), Some(Dec::Comma));
        let _g = DecGuard::set(loc("de-DE"));
        assert_eq!(number("1.250", None), Ok(1250.0));
    }

    #[test]
    fn point_decimal_three_digits_resolved_by_book_locale() {
        let rows = [("units", "metric"), ("locale", "en-US")];
        assert_eq!(dec_from_info(rows.into_iter()), Some(Dec::Point));
        assert_eq!(dec_from_info([("locale", "de_DE")].into_iter()), Some(Dec::Comma));
        assert_eq!(dec_from_info([("decimal", "comma")].into_iter()), Some(Dec::Comma));
        assert_eq!(dec_from_info([("units", "metric")].into_iter()), None);
        {
            let _g = DecGuard::set(dec_from_info(rows.into_iter()));
            assert_eq!(number("1.250", None), Ok(1.25));
            assert_eq!(number("1,250", None), Ok(1250.0));
        }
        {
            let _g = DecGuard::set(Some(Dec::Comma));
            assert_eq!(number("1.250", None), Ok(1250.0));
            assert_eq!(number("1,250", None), Ok(1.25));
        }
        assert!(matches!(number("1.250", None), Err(NumErr::Ambiguous { .. })));
    }

    #[test]
    fn nbsp_thousands_and_unicode_minus_parse() {
        assert!(close(parse_len("1\u{a0}250 mm", 1.0), 1.25));
        assert!(close(parse_len("\u{2212}1 250,5", 1e-3), -1.2505));
        assert!(close(parse_len("\u{2212}2'-0\"", 1.0), -0.6096));
        assert!(close(parse_len("1'250.5", 1e-3), 1.2505));
    }

    #[test]
    fn mixed_separators_parse() {
        assert!(close(parse_len("1,250.5", 1e-3), 1.2505));
        assert!(close(parse_len("1.250,5 mm", 1.0), 1.2505));
        assert!(close(parse_len("2,5 m", 1.0), 2.5));
        assert!(close(parse_len("6 1/2\"", 1.0), 6.5 * 0.0254));
    }

    #[test]
    fn headers_carry_units() {
        assert_eq!(norm("elevation [ft]"), "elevation");
        assert_eq!(norm("area [m2] (read-only)"), "area (read-only)");
        assert_eq!(header_factor("height [mm]", Dim::Len), Some(1e-3));
        assert_eq!(
            header_factor("area [ft2]", Dim::Area),
            Some(0.3048 * 0.3048)
        );
        assert_eq!(header_factor("area [m²]", Dim::Area), Some(1.0));
        assert_eq!(header_factor("height [ft2]", Dim::Len), None);
        assert_eq!(header_factor("name", Dim::Len), None);
    }
}
