//! Project requirements read out of attached files: site size, storeys, rooms with areas,
//! limits and the norms a brief names. Every fact keeps where it came from (file, page,
//! region), so the person can check it before anything is built. A room is a label, a number
//! in any script and an area unit with a square sign; its kind is never guessed from the
//! label, the model sets the canonical kind. Word lists (Russian, English) only add hints
//! (site, total, height, limits); anything not recognised stays in the text the model reads.

use super::{Extracted, Kind, ScaleSrc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FactKind {
    Site,
    Storeys,
    Room,
    TotalArea,
    Height,
    Constraint,
    Norm,
    Scale,
}

impl FactKind {
    pub fn word(self) -> &'static str {
        match self {
            FactKind::Site => "site",
            FactKind::Storeys => "storeys",
            FactKind::Room => "room",
            FactKind::TotalArea => "total_area",
            FactKind::Height => "height",
            FactKind::Constraint => "constraint",
            FactKind::Norm => "norm",
            FactKind::Scale => "scale",
        }
    }
}

/// Where a fact was read: page is 1-based and only set for paged files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRef {
    pub file: String,
    pub page: Option<usize>,
    /// "line 12", "row 5", "dimensions" ...
    pub region: String,
}

impl SourceRef {
    pub fn show(&self) -> String {
        match self.page {
            Some(p) => format!("{}, p. {}, {}", self.file, p, self.region),
            None => format!("{}, {}", self.file, self.region),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fact {
    pub kind: FactKind,
    /// A room's name; empty for the others.
    pub label: String,
    /// The value as the person reads and edits it ("30 × 40 m", "3", a norm code such as "SP 54.13330").
    pub value: String,
    /// Square metres of a room or a total area.
    pub area_m2: Option<f64>,
    pub count: Option<u32>,
    pub src: SourceRef,
}

/// Facts of one file, in reading order; the same fact from two places is kept once.
pub fn from_extracted(file: &str, kind: Kind, e: &Extracted) -> Vec<Fact> {
    let mut out: Vec<Fact> = vec![];
    let paged = kind == Kind::Pdf || e.pages.len() > 1;
    if let Some(s) = &e.scale {
        let region = match &s.src {
            ScaleSrc::Text(t) => format!("text \u{ab}{t}\u{bb}"),
            ScaleSrc::Dimensions(n) => format!("{n} dimensions"),
        };
        out.push(fact(FactKind::Scale, "", format!("1:{}", s.ratio), src(file, Some(s.page + 1), region)));
    }
    if !e.storeys.is_empty() {
        let mut f = fact(FactKind::Storeys, "", e.storeys.len().to_string(), src(file, None, "IfcBuildingStorey".into()));
        f.count = Some(e.storeys.len() as u32);
        out.push(f);
    }
    let table_rooms = rooms_of_table(file, &e.table);
    let has_table = !table_rooms.is_empty();
    out.extend(table_rooms);
    for (pi, page) in e.pages.iter().enumerate() {
        let pg = paged.then_some(pi + 1);
        for (li, line) in page.lines().enumerate() {
            let at = || src(file, pg, format!("line {}", li + 1));
            facts_of_line(line, has_table, &at, &mut out);
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|f| seen.insert((f.kind, f.label.to_lowercase(), f.value.to_lowercase())));
    out
}

fn src(file: &str, page: Option<usize>, region: String) -> SourceRef {
    SourceRef {
        file: file.to_string(),
        page,
        region,
    }
}

fn fact(kind: FactKind, label: &str, value: String, src: SourceRef) -> Fact {
    Fact {
        kind,
        label: label.to_string(),
        value,
        area_m2: None,
        count: None,
        src,
    }
}

fn has_any(l: &str, words: &[String]) -> bool {
    words.iter().any(|w| l.contains(w.as_str()))
}

/// A word list of `data/brief_words.json` (site, total, limit, area_units, ...): the words
/// briefs use, in every language the reader knows, kept out of the code.
fn words(list: &str) -> &'static [String] {
    static W: std::sync::OnceLock<std::collections::BTreeMap<String, Vec<String>>> = std::sync::OnceLock::new();
    W.get_or_init(|| serde_json::from_str(include_str!("../../data/brief_words.json")).expect("brief_words.json"))
        .get(list)
        .map_or(&[], Vec::as_slice)
}

fn starts_any(rest: &str, list: &str) -> bool {
    words(list).iter().any(|w| rest.starts_with(w.as_str()))
}

fn facts_of_line(line: &str, has_table: bool, at: &dyn Fn() -> SourceRef, out: &mut Vec<Fact>) {
    // digits of any script read as ASCII first, so the byte offsets below are of this text
    let folded = fold_digits(line);
    let t = folded.trim();
    if t.is_empty() || t.chars().count() > 400 {
        return;
    }
    let l = t.to_lowercase();
    for n in norms_in(t) {
        out.push(fact(FactKind::Norm, "", n, at()));
    }
    if has_any(&l, words("site")) {
        if let Some((a, b)) = size_pair(&l) {
            out.push(fact(FactKind::Site, "", format!("{} \u{d7} {} m", fmt(a), fmt(b)), at()));
            return;
        }
    }
    if let Some(n) = storeys_in(&l) {
        let mut f = fact(FactKind::Storeys, "", n.to_string(), at());
        f.count = Some(n);
        out.push(f);
        return;
    }
    let area = area_in(&l);
    if has_any(&l, words("total")) {
        if let Some((v, _)) = area {
            let mut f = fact(FactKind::TotalArea, "", format!("{} m\u{b2}", fmt(v)), at());
            f.area_m2 = Some(v);
            out.push(f);
            return;
        }
    }
    if has_any(&l, words("height")) {
        if let Some(h) = metres_in(&l) {
            out.push(fact(FactKind::Height, "", format!("{} m \u{2014} {}", fmt(h), short(t)), at()));
            return;
        }
    }
    if let (Some((v, start)), false) = (area, has_table) {
        let name = room_name(t.get(..start).unwrap_or(""));
        if !name.is_empty() && !has_any(&l, words("site")) {
            let mut f = fact(FactKind::Room, &name, format!("{} m\u{b2}", fmt(v)), at());
            f.area_m2 = Some(v);
            f.count = count_in(&l);
            out.push(f);
            return;
        }
    }
    if has_any(&l, words("limit")) {
        out.push(fact(FactKind::Constraint, "", short(t), at()));
    }
}

fn short(t: &str) -> String {
    let s: String = t.chars().take(160).collect();
    if t.chars().count() > 160 { format!("{s}\u{2026}") } else { s }
}

pub fn fmt(v: f64) -> String {
    let s = format!("{:.2}", v);
    // only the fraction loses its zeros: 100.00 is 100, not 1
    match s.split_once('.') {
        Some((i, f)) if f.trim_end_matches('0').is_empty() => i.to_string(),
        Some((i, f)) => format!("{i}.{}", f.trim_end_matches('0')),
        None => s,
    }
}

/// Numbers in a text with their byte span: "12,5", "1 200", "40.0".
pub fn numbers(s: &str) -> Vec<(f64, usize, usize)> {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        // a Latin x between two numbers is a times sign ("30x40"), not a code like "B12"
        let times = i > 1 && matches!(b[i - 1], b'x' | b'X') && (b[i - 2].is_ascii_digit() || b[i - 2] == b' ');
        if !b[i].is_ascii_digit() || (i > 0 && !times && (b[i - 1].is_ascii_alphabetic() || b[i - 1] == b'.')) {
            i += 1;
            continue;
        }
        let start = i;
        let mut txt = String::new();
        while i < b.len() {
            let c = b[i];
            if c.is_ascii_digit() {
                txt.push(c as char);
            } else if (c == b',' || c == b'.') && i + 1 < b.len() && b[i + 1].is_ascii_digit() && !txt.contains('.') {
                txt.push('.');
            } else if c == b' ' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(u8::is_ascii_digit)
                && (i + 4 >= b.len() || !b[i + 4].is_ascii_digit()) && !txt.contains('.') && txt.len() <= 3
            {
                // thousands separated by a space: "1 200"
            } else {
                break;
            }
            i += 1;
        }
        if let Ok(v) = txt.parse::<f64>() {
            out.push((v, start, i));
        }
    }
    out
}

fn after(s: &str, end: usize) -> &str {
    s.get(end..).unwrap_or("").trim_start()
}

/// "30x40", "30 x 40 m", "30×40" — Latin x, Cyrillic x (U+0445), the times sign or an asterisk.
fn size_pair(l: &str) -> Option<(f64, f64)> {
    let ns = numbers(l);
    for w in ns.windows(2) {
        let mid = l.get(w[0].2..w[1].1)?.trim();
        if words("times").iter().any(|t| t == mid) && w[0].0 > 0.0 && w[1].0 > 0.0 {
            return Some((w[0].0, w[1].0));
        }
    }
    None
}

fn storeys_in(l: &str) -> Option<u32> {
    let ok = |v: f64| ((1.0..=200.0).contains(&v) && v.fract() == 0.0).then_some(v as u32);
    for (v, _, e) in numbers(l) {
        let rest = after(l, e).trim_start_matches(['-', ' ']);
        if starts_any(rest, "storeys_after") {
            if let Some(n) = ok(v) {
                return Some(n);
            }
        }
    }
    for key in words("storeys_key") {
        if let Some(p) = l.find(key) {
            if let Some((v, s, _)) = numbers(&l[p..]).first().copied() {
                if l[p + key.len()..p + s].trim_matches([':', ' ', '-', '\u{2014}']).is_empty() {
                    return ok(v);
                }
            }
        }
    }
    None
}

/// An area with its unit, and where its number starts.
fn area_in(l: &str) -> Option<(f64, usize)> {
    numbers(l).into_iter().find_map(|(v, s, e)| {
        let rest = after(l, e);
        (starts_any(rest, "area_units") || area_symbol(rest)).then_some((v, s)).filter(|(v, _)| *v > 0.0)
    })
}

/// A unit written with a square sign (`m²`, Cyrillic, Arabic or CJK variants such as `㎡`): an area in any language, the
/// word list above only adds units spelled out.
fn area_symbol(rest: &str) -> bool {
    let unit: String = rest.chars().take_while(|c| !c.is_whitespace() && !matches!(c, ',' | ';' | ')')).take(6).collect();
    unit.contains(['\u{b2}', '\u{33a1}'])
}

/// Decimal digits of every script that has them, start of each run of ten.
const DIGIT_ZEROS: [u32; 21] = [
    0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66, 0xDE6,
    0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0xFF10,
];

/// Digits of any script as ASCII, the Arabic decimal sign and full-width point as `.`; the
/// same folding the UI number fields use.
pub fn fold_digits(s: &str) -> std::borrow::Cow<'_, str> {
    if s.is_ascii() {
        return std::borrow::Cow::Borrowed(s);
    }
    let one = |c: char| {
        let u = c as u32;
        match DIGIT_ZEROS.iter().find(|z| (**z..**z + 10).contains(&u)) {
            Some(z) => char::from(b'0' + (u - z) as u8),
            None => match c {
                '\u{66B}' | '\u{FF0E}' => '.',
                '\u{66C}' | '\u{FF0C}' => ',',
                _ => c,
            },
        }
    };
    std::borrow::Cow::Owned(s.chars().map(one).collect())
}

fn metres_in(l: &str) -> Option<f64> {
    numbers(l).into_iter().find_map(|(v, _, e)| {
        let rest = after(l, e);
        let unit = words("metre").iter().find(|m| rest.starts_with(m.as_str()))?;
        let next = rest[unit.len()..].chars().next();
        let bare = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '²');
        (bare(next) && v > 0.0).then_some(v)
    })
}

fn count_in(l: &str) -> Option<u32> {
    for (v, _, e) in numbers(l) {
        let rest = after(l, e);
        if starts_any(rest, "count_after") && v >= 1.0 && v.fract() == 0.0 {
            return Some(v as u32);
        }
    }
    None
}

fn room_name(head: &str) -> String {
    let s = head.trim().trim_end_matches([':', '-', '\u{2014}', '\u{2013}', '\t', ' ', '=', '.', ',']).trim();
    let s = s.trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | ')' | ' ' | '-' | '\u{2022}' | '*'));
    if s.chars().any(char::is_alphabetic) && s.chars().count() <= 60 {
        s.trim().to_string()
    } else {
        String::new()
    }
}

/// Codes of norms: Russian "SP 54.13330.2022" or "GOST 21.501-2018" style codes (in Cyrillic), "EN 1991-1-1", "ISO 19650".
pub fn norms_in(t: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for h in words("norm_heads") {
        let h = h.as_str();
        let mut from = 0;
        while let Some(p) = t[from..].find(h) {
            let at = from + p;
            from = at + h.len();
            let before_ok = t[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
            let rest = &t[at + h.len()..];
            let code: String = rest
                .trim_start_matches(' ')
                .chars()
                .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | ':' | '/'))
                .collect();
            let code = code.trim_end_matches(['.', '-', ':', '/']);
            if before_ok && rest.starts_with(' ') && code.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                let n = format!("{h} {code}");
                if !out.iter().any(|o| o.contains(&n) || n.contains(o.as_str())) {
                    out.push(n);
                }
            }
        }
    }
    out
}


/// A room programme: a header with a name and an area column within the first rows.
pub fn rooms_of_table(file: &str, rows: &[Vec<String>]) -> Vec<Fact> {
    let find = |r: &[String], list: &str| r.iter().position(|c| has_any(&c.to_lowercase(), words(list)));
    let Some((hi, name, area, count)) = rows.iter().take(10).enumerate().find_map(|(i, r)| {
        let n = find(r, "name_col")?;
        let a = r.iter().enumerate().position(|(j, c)| j != n && has_any(&c.to_lowercase(), words("area_col")))?;
        let k = r
            .iter()
            .enumerate()
            .position(|(j, c)| j != n && j != a && has_any(&c.to_lowercase(), words("count_col")));
        Some((i, n, a, k))
    }) else {
        return vec![];
    };
    let mut out = vec![];
    for (i, r) in rows.iter().enumerate().skip(hi + 1) {
        let label = r.get(name).map(|s| s.trim()).unwrap_or("");
        let Some(v) = r.get(area).and_then(|s| numbers(&fold_digits(s)).first().map(|n| n.0)).filter(|v| *v > 0.0) else {
            continue;
        };
        if label.is_empty() || has_any(&label.to_lowercase(), words("total")) {
            continue;
        }
        let mut f = fact(FactKind::Room, label, format!("{} m\u{b2}", fmt(v)), src(file, None, format!("row {}", i + 1)));
        f.area_m2 = Some(v);
        f.count = count
            .and_then(|k| r.get(k))
            .and_then(|s| numbers(&fold_digits(s)).first().map(|n| n.0))
            .filter(|c| *c >= 1.0 && c.fract() == 0.0)
            .map(|c| c as u32);
        out.push(f);
    }
    out
}
