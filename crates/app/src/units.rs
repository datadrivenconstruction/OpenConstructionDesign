//! Units and length-input parsing: metric (bare number = mm) and imperial (bare number = inches), both first-class.
//! Internally everything is in metres.

use crate::expr_field::Scope;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Units {
    #[default]
    Metric,
    Imperial,
}

pub const FOOT: f64 = 0.3048;
pub const INCH: f64 = 0.0254;
pub(crate) const MAX_LEN: f64 = 1000.0;

#[derive(Clone, Debug, PartialEq)]
pub enum InputError {
    Empty,
    /// Not a number; character position (1-based).
    NotNumber(usize),
    UnknownUnit(String),
    NonPositive,
    TooLarge,
    BadAngle,
    /// Name is not a project parameter.
    UnknownName(String),
    /// Parameter formulas refer to each other; carries the name where the loop closed.
    Cycle(String),
    /// Malformed expression; position from 1.
    BadExpr(usize),
    /// Division by zero; position from 1.
    DivZero(usize),
    /// English comma that is not a thousands group (`1,5`): refused, the user types a point.
    CommaDecimal,
    /// Russian `6000,4000`: a comma between two whole numbers of 3+ digits was the old pair
    /// form; refused instead of being read as 6000.4.
    OldPair,
}

impl InputError {
    /// Message in an explicit language (`Display` uses the UI language).
    pub fn text(&self, ru: bool) -> String {
        let t = |k: &str, a: &[(&str, &str)]| crate::tr_lang(ru, k, a);
        match self {
            Self::Empty => t("unit_fmt.units.empty", &[]),
            Self::NotNumber(p) => t("unit_fmt.units.not_number", &[("p", p.to_string().as_str())]),
            Self::UnknownUnit(u) => t("unit_fmt.units.unknown_unit", &[("u", u.as_str())]),
            Self::NonPositive => t("unit_fmt.units.non_positive", &[]),
            Self::TooLarge => t("unit_fmt.units.too_large", &[("max", MAX_LEN.to_string().as_str())]),
            Self::BadAngle => t("unit_fmt.units.bad_angle", &[]),
            Self::UnknownName(n) => format!("unknown name \"{n}\""),
            Self::Cycle(n) => format!("formula loop at \"{n}\""),
            Self::BadExpr(p) => format!("bad expression at {p}"),
            Self::DivZero(p) => format!("division by zero at {p}"),
            Self::CommaDecimal => t("decimal_input.comma_decimal", &[]),
            Self::OldPair => t("pair_input.old_pair", &[]),
        }
    }
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text(crate::ui_is_ru()))
    }
}

/// What the user typed in the tool prompt.
#[derive(Clone, Debug, PartialEq)]
pub enum Typed {
    /// Length (m) and an optional angle (degrees from +X, counter-clockwise).
    Polar(Option<f64>, Option<f64>),
    /// Offset `@dx,dy` or rectangle size `w,h` (m).
    Pair(f64, f64),
}

/// Separator of the two values of a typed pair (`@1000;500`, `6000;4000`); the same `;` splits
/// count and step in the array bars. A comma is never a pair separator: it is a decimal mark in
/// Russian and a thousands group in English.
pub const PAIR_SEP: char = ';';

/// `3500`, `3500<90`, `<45`, `@1000;500`, `6000;4000`, numbers in the locale of `mark`.
pub fn parse_typed(s: &str, u: Units, mark: DecimalMark) -> Result<Typed, InputError> {
    parse_typed_in(s, u, &Scope::none(), mark)
}

/// Like [`parse_typed`], with project parameter names and `* / ( )` in every number.
pub fn parse_typed_in(s: &str, u: Units, sc: &Scope, mark: DecimalMark) -> Result<Typed, InputError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(InputError::Empty);
    }
    let t = t.strip_prefix('@').unwrap_or(t);
    if let Some((a, b)) = t.split_once(PAIR_SEP) {
        return Ok(Typed::Pair(signed_lang(a, u, sc, mark)?, signed_lang(b, u, sc, mark)?));
    }
    let (len, ang) = match t.split_once('<') {
        Some((l, a)) => (l.trim(), Some(a.trim())),
        None => (t, None),
    };
    if mark == DecimalMark::Comma && looks_like_old_pair(len) {
        return Err(InputError::OldPair);
    }
    let len = if len.is_empty() {
        None
    } else {
        Some(parse_edit_length_lang(len, u, sc, mark)?)
    };
    let ang = match ang {
        Some(a) => Some(
            parse_locale_number(a.strip_suffix('\u{b0}').unwrap_or(a).trim(), mark).map_err(|e| match e {
                NumError::CommaDecimal => InputError::CommaDecimal,
                _ => InputError::BadAngle,
            })?,
        ),
        None => None,
    };
    if ang.is_some_and(|a| !a.is_finite()) {
        return Err(InputError::BadAngle);
    }
    if len.is_none() && ang.is_none() {
        return Err(InputError::Empty);
    }
    Ok(Typed::Polar(len, ang))
}

/// Positive length in metres.
pub fn parse_length(s: &str, u: Units) -> Result<f64, InputError> {
    parse_length_in(s, u, &Scope::none())
}

/// Positive length in metres; the text may use `* / ( )` and project parameter names.
pub fn parse_length_in(s: &str, u: Units, sc: &Scope) -> Result<f64, InputError> {
    let v = parse_signed(s, u, sc)?;
    if v <= 0.0 {
        Err(InputError::NonPositive)
    } else {
        Ok(v)
    }
}

/// Sum/difference of terms: `3000+150`, `12'6"-1"`.
fn parse_signed(s: &str, u: Units, sc: &Scope) -> Result<f64, InputError> {
    parse_signed_lim(s, u, sc, MAX_LEN)
}

/// Signed length in metres (a leading minus is kept); callers refuse non-positive values themselves.
pub(crate) fn parse_signed_len(s: &str, u: Units) -> Result<f64, InputError> {
    parse_signed(s, u, &Scope::none())
}

/// Signed length in metres of any size (levels warn instead of refusing far values).
pub fn parse_signed_unbounded(s: &str, u: Units) -> Result<f64, InputError> {
    parse_signed_lim(s, u, &Scope::none(), f64::MAX)
}

fn parse_signed_lim(s: &str, u: Units, sc: &Scope, lim: f64) -> Result<f64, InputError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(InputError::Empty);
    }
    if crate::expr_field::needs_expr(s, u, sc) {
        return crate::expr_field::eval(s, crate::expr_field::Dom::Length(u), sc);
    }
    let mut total = 0.0;
    let mut start = 0;
    let mut sign = 1.0;
    let b = s.as_bytes();
    for i in 0..=b.len() {
        let end = i == b.len();
        if end || ((b[i] == b'+' || b[i] == b'-') && i > 0 && is_operator(b, i)) {
            // `12'-6"` — a hyphen after feet separates feet from inches rather than subtracting.
            let term = &s[start..i];
            let (sg, term_s, off) = match term.trim_start().strip_prefix('-') {
                Some(r) if start == 0 => (-1.0, r, start + term.len() - r.len()),
                _ => (1.0, term, start),
            };
            total += sign * sg * term_value(term_s, u, off)?;
            if !end {
                sign = if b[i] == b'+' { 1.0 } else { -1.0 };
                start = i + 1;
            }
        }
    }
    if total.abs() > lim {
        return Err(InputError::TooLarge);
    }
    Ok(total)
}

/// The sign at position `i` is an operator, not part of `12'-6"` or an exponent like `1e-3`.
pub(crate) fn is_operator(b: &[u8], i: usize) -> bool {
    let Some(p) = b[..i].iter().rposition(|&c| c != b' ') else {
        return false;
    };
    if b[p] == b'\'' {
        return false;
    }
    let exp = matches!(b[p], b'e' | b'E')
        && p == i - 1
        && p > 0
        && (b[p - 1].is_ascii_digit() || b[p - 1] == b'.')
        && b.get(i + 1).is_some_and(u8::is_ascii_digit);
    !exp
}

/// Start of the unit: the first letter, except an exponent `e5`/`e-5` inside a number.
pub(crate) fn unit_start(t: &str) -> usize {
    let cs: Vec<(usize, char)> = t.char_indices().collect();
    for (k, &(i, c)) in cs.iter().enumerate() {
        if !c.is_alphabetic() && c != '°' {
            continue;
        }
        if matches!(c, 'e' | 'E') && k > 0 && (cs[k - 1].1.is_ascii_digit() || cs[k - 1].1 == '.') {
            let d = match cs.get(k + 1).map(|x| x.1) {
                Some('+' | '-') => cs.get(k + 2),
                _ => cs.get(k + 1),
            };
            if d.is_some_and(|x| x.1.is_ascii_digit()) {
                continue;
            }
        }
        return i;
    }
    t.len()
}

pub(crate) fn term_value(t: &str, u: Units, off: usize) -> Result<f64, InputError> {
    let lead = t.len() - t.trim_start().len();
    let t = t.trim();
    let off = off + lead;
    if t.is_empty() {
        return Err(InputError::NotNumber(off + 1));
    }
    if t.contains('\'') || t.contains('"') {
        return imperial(t, off);
    }
    if u == Units::Imperial && t.contains('/') && unit_start(t) == t.len() {
        return imperial(t, off);
    }
    let split = unit_start(t);
    let (num, unit) = (t[..split].trim(), t[split..].trim());
    let v = number(num, off)?;
    let k = match (unit, u) {
        // by division: 2800 * 0.001 gives 2.8000000000000003 and would settle in the model
        ("", Units::Metric) | ("mm", _) | ("\u{43c}\u{43c}", _) => return Ok(v / 1000.0),
        ("cm", _) | ("\u{441}\u{43c}", _) => return Ok(v / 100.0),
        ("m", _) | ("\u{43c}", _) => 1.0,
        ("", Units::Imperial) | ("in", _) => INCH,
        ("ft", _) => FOOT,
        (x, _) => return Err(InputError::UnknownUnit(x.into())),
    };
    Ok(v * k)
}

/// `12'6"`, `12'-6"`, `12'`, `6"`, `6 1/2"`, `12'6`.
fn imperial(t: &str, off: usize) -> Result<f64, InputError> {
    let (ft, rest, roff) = match t.find('\'') {
        Some(i) => (number(t[..i].trim(), off)?, &t[i + 1..], off + i + 1),
        None => (0.0, t, off),
    };
    let r = rest.trim().trim_start_matches('-').trim();
    let r = r.strip_suffix('"').unwrap_or(r).trim();
    let inch = if r.is_empty() {
        0.0
    } else {
        let mut v = 0.0;
        for part in r.split_whitespace() {
            v += match part.split_once('/') {
                Some((n, d)) => {
                    let d = number(d, roff)?;
                    if d == 0.0 {
                        return Err(InputError::NotNumber(roff + 1));
                    }
                    number(n, roff)? / d
                }
                None => number(part, roff)?,
            };
        }
        v
    };
    Ok(ft * FOOT + inch * INCH)
}

fn number(s: &str, off: usize) -> Result<f64, InputError> {
    match s.parse::<f64>() {
        Ok(v) if v.is_finite() && !s.starts_with(['+', '-']) => Ok(v),
        _ => {
            let mut dot = false;
            let bad = s
                .char_indices()
                .find(|&(_, c)| {
                    let second_dot = c == '.' && dot;
                    dot |= c == '.';
                    second_dot || !(c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
                })
                .map_or(0, |(i, _)| i);
            Err(InputError::NotNumber(off + bad + 1))
        }
    }
}

/// Length label: metric — mm, imperial — feet-inches with 1/8" precision.
/// Fixed-point number for display; never prints negative zero ("-0.000" → "0.000").
pub fn fmt_fixed(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    match s.strip_prefix('-') {
        Some(rest) if rest.bytes().all(|b| b == b'0' || b == b'.') => rest.to_string(),
        _ => s,
    }
}

/// Display number in the UI language: ru `1 234,5` (no-break groups), en `1,234.5`.
/// Display only; stored and exported values keep their plain numeric form.
pub fn fmt_num(v: f64, decimals: usize) -> String {
    fmt_num_lang(v, decimals, crate::ui_is_ru())
}

/// Russian-style number whatever the UI language: no-break space groups, decimal comma.
pub fn fmt_num_ru(v: f64, decimals: usize) -> String {
    let s = fmt_fixed(v, decimals);
    let (sign, s) = s.strip_prefix('-').map_or(("", s.as_str()), |r| ("-", r));
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let mut g = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            g.push('\u{a0}');
        }
        g.push(c);
    }
    if frac.is_empty() {
        format!("{sign}{g}")
    } else {
        format!("{sign}{g},{frac}")
    }
}

/// Decimal places for display: one rule per quantity across the whole application
/// (properties, status, plan, XLSX cell format).
/// Defaults; the user changes them in "Settings → Precision" (`set_precision`).
pub const AREA_DECIMALS: usize = 2;
pub const VOLUME_DECIMALS: usize = 3;
pub const PERCENT_DECIMALS: usize = 1;
const MAX_DECIMALS: usize = 6;

static AREA_DEC: AtomicUsize = AtomicUsize::new(AREA_DECIMALS);
static VOLUME_DEC: AtomicUsize = AtomicUsize::new(VOLUME_DECIMALS);

/// Current display precision (area, volume) — shared by the whole application.
pub fn precision() -> (usize, usize) {
    (AREA_DEC.load(Relaxed), VOLUME_DEC.load(Relaxed))
}

pub fn set_precision(area: usize, volume: usize) {
    AREA_DEC.store(area.min(MAX_DECIMALS), Relaxed);
    VOLUME_DEC.store(volume.min(MAX_DECIMALS), Relaxed);
}

/// Metric length fields show whole millimetres unless the project asks for more.
pub const LENGTH_DECIMALS: usize = 0;
static LEN_DEC: AtomicUsize = AtomicUsize::new(LENGTH_DECIMALS);

/// Decimals of a metric length shown in a field (imperial uses the fraction denominator).
pub fn length_decimals() -> usize {
    LEN_DEC.load(Relaxed)
}

pub fn set_length_decimals(d: usize) {
    LEN_DEC.store(d.min(3), Relaxed);
}

/// Field text of a length at a given display precision: `dec` decimals of a millimetre
/// (trailing zeros dropped) or 1/`den` inch. Display only; the stored value is never rounded.
pub fn shown_length(m: f64, u: Units, mark: DecimalMark, dec: usize, den: usize) -> String {
    match u {
        Units::Metric => {
            let (s, point) = match mark {
                DecimalMark::Comma => (fmt_num_ru(m * 1000.0, dec), ','),
                DecimalMark::Point => (fmt_num_en(m * 1000.0, dec), '.'),
            };
            if s.contains(point) {
                s.trim_end_matches('0').trim_end_matches(point).to_string()
            } else {
                s
            }
        }
        Units::Imperial => format_length_den(m, u, den),
    }
}

static FRAC_DEN: AtomicUsize = AtomicUsize::new(DEFAULT_FRAC);
/// Setting: show areas below 1 ft² in in² (otherwise ft² with extra digits).
static SMALL_AREA_IN2: AtomicBool = AtomicBool::new(false);

pub fn set_small_area_in2(on: bool) {
    SMALL_AREA_IN2.store(on, Relaxed);
}

thread_local! {
    // Project units live in the UI thread (its `Editor::set_units`), not process-wide:
    // parallel tests and background threads do not see another thread's system.
    static ACTIVE_UNITS: std::cell::Cell<Units> = const { std::cell::Cell::new(Units::Metric) };
}

/// Project unit system for area/volume (lengths receive it as a parameter).
pub fn set_active_units(u: Units) {
    ACTIVE_UNITS.with(|c| c.set(u));
}

pub fn active_units() -> Units {
    ACTIVE_UNITS.with(|c| c.get())
}

/// Inch fraction denominator: a power of two, 2…256.
pub const FRAC_DENOMS: [usize; 8] = [2, 4, 8, 16, 32, 64, 128, 256];
pub const DEFAULT_FRAC: usize = 8;

/// Valid denominator (invalid input → 8).
pub fn clamp_frac(d: usize) -> usize {
    if FRAC_DENOMS.contains(&d) {
        d
    } else {
        DEFAULT_FRAC
    }
}

pub fn frac_denominator() -> usize {
    FRAC_DEN.load(Relaxed)
}

pub fn set_frac_denominator(d: usize) {
    FRAC_DEN.store(clamp_frac(d), Relaxed);
}

const FT2: f64 = FOOT * FOOT;
const IN2: f64 = INCH * INCH;
const FT3: f64 = FT2 * FOOT;

/// English-style number: «1,234.57».
pub fn fmt_num_en(v: f64, decimals: usize) -> String {
    let s = fmt_fixed(v, decimals);
    let (sign, s) = s.strip_prefix('-').map_or(("", s.as_str()), |r| ("-", r));
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let mut g = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            g.push(',');
        }
        g.push(c);
    }
    if frac.is_empty() {
        format!("{sign}{g}")
    } else {
        format!("{sign}{g}.{frac}")
    }
}

/// Area in the display units of system `u` (m² or ft²) — for table columns and export.
pub fn area_in_units(m2: f64, u: Units) -> f64 {
    match u {
        Units::Metric => m2,
        Units::Imperial => m2 / FT2,
    }
}

/// Volume in the display units of system `u` (m³ or ft³).
pub fn volume_in_units(m3: f64, u: Units) -> f64 {
    match u {
        Units::Metric => m3,
        Units::Imperial => m3 / FT3,
    }
}

/// Area in system `u`: "12,35 m²" (unit localized) / "132.90 ft²". Below 1 ft²: two extra digits
/// or (setting) in "in²".
pub fn fmt_area_in(m2: f64, u: Units) -> String {
    fmt_area_with(m2, u, precision().0, SMALL_AREA_IN2.load(Relaxed))
}

pub fn fmt_area_with(m2: f64, u: Units, d: usize, small_in2: bool) -> String {
    fmt_area_lang(m2, u, d, small_in2, crate::ui_is_ru())
}

/// Area in the project units for an explicit language (global precision and small-area setting).
pub fn fmt_area_in_lang(m2: f64, u: Units, ru: bool) -> String {
    fmt_area_lang(m2, u, precision().0, SMALL_AREA_IN2.load(Relaxed), ru)
}

/// Number in the style of the language: ru `1 234,5` (no-break groups), else `1,234.5`.
pub fn fmt_num_lang(v: f64, d: usize, ru: bool) -> String {
    if ru {
        fmt_num_ru(v, d)
    } else {
        fmt_num_en(v, d)
    }
}

/// Plain fixed-point text (no grouping) with the decimal mark of `mark`, for editable fields.
pub fn fmt_fixed_mark(v: f64, d: usize, mark: DecimalMark) -> String {
    let s = fmt_fixed(v, d);
    match mark {
        DecimalMark::Comma => s.replace('.', ","),
        DecimalMark::Point => s,
    }
}

/// As `fmt_area_with`, with the unit name and the number style in an explicit language.
pub fn fmt_area_lang(m2: f64, u: Units, d: usize, small_in2: bool, ru: bool) -> String {
    match u {
        Units::Metric => format!("{}\u{a0}{}", fmt_num_lang(m2, d, ru), crate::tr_str(ru, "unit_fmt.unit.m2")),
        Units::Imperial if m2.abs() < FT2 && m2 != 0.0 => {
            if small_in2 {
                format!("{}\u{a0}in²", fmt_num_en(m2 / IN2, d))
            } else {
                format!(
                    "{}\u{a0}ft²",
                    fmt_num_en(m2 / FT2, (d + 2).min(MAX_DECIMALS))
                )
            }
        }
        Units::Imperial => format!("{}\u{a0}ft²", fmt_num_en(m2 / FT2, d)),
    }
}

/// Volume in system `u`: "1,234 m³" (unit localized) / "43.570 ft³".
pub fn fmt_volume_in(m3: f64, u: Units) -> String {
    fmt_volume_lang(m3, u, precision().1, crate::ui_is_ru())
}

/// Volume with `d` decimals and the unit name in an explicit language.
pub fn fmt_volume_lang(m3: f64, u: Units, d: usize, ru: bool) -> String {
    match u {
        Units::Metric => format!("{}\u{a0}{}", fmt_num_lang(m3, d, ru), crate::tr_str(ru, "unit_fmt.unit.m3")),
        Units::Imperial => format!("{}\u{a0}ft³", fmt_num_en(m3 / FT3, d)),
    }
}

/// Area: "12,35 m²" (or ft² in an imperial project).
pub fn fmt_area(m2: f64) -> String {
    fmt_area_in(m2, active_units())
}

/// Volume: "1,234 m³" (or ft³ in an imperial project).
pub fn fmt_volume(m3: f64) -> String {
    fmt_volume_in(m3, active_units())
}

/// Length without a unit for an input field: "3 000" (mm) / "9'-10 1/8""; parsed by `parse_length`.
pub fn edit_length(m: f64, u: Units) -> String {
    edit_length_den(m, u, frac_denominator())
}

/// `edit_length` with an explicit denominator: rounds to 1/den inch, carrying into inches and feet.
pub fn edit_length_den(m: f64, u: Units, den: usize) -> String {
    edit_length_den_mark(m, u, den, ui_decimal_mark())
}

/// [`edit_length`] in the locale of `mark`: «1 250,5» / «1,250.5»; reads back through
/// [`parse_edit_length_lang`] with the same mark.
pub fn edit_length_mark(m: f64, u: Units, mark: DecimalMark) -> String {
    edit_length_den_mark(m, u, frac_denominator(), mark)
}

pub fn edit_length_den_mark(m: f64, u: Units, den: usize, mark: DecimalMark) -> String {
    match u {
        // up to 3 decimals so a sub-mm value survives an untouched field
        Units::Metric => {
            let (s, dec) = match mark {
                DecimalMark::Comma => (fmt_num_ru(m * 1000.0, 3), ','),
                DecimalMark::Point => (fmt_num_en(m * 1000.0, 3), '.'),
            };
            if s.contains(dec) {
                s.trim_end_matches('0').trim_end_matches(dec).to_string()
            } else {
                s
            }
        }
        Units::Imperial => format_length_den(m, u, den),
    }
}

/// Unit of length fields goes in a separate column; imperial values carry ' and " themselves.
pub fn unit_label(u: Units) -> &'static str {
    unit_label_in(u, crate::ui_is_ru())
}

/// As `unit_label`, in the language of the caller instead of the global one.
pub fn unit_label_in(u: Units, ru: bool) -> &'static str {
    match u {
        Units::Metric => crate::tr_str(ru, "unit_fmt.unit.mm"),
        Units::Imperial => "",
    }
}

// ---- Locale-aware number input: one parser for every numeric field -------------------------
// Rules live in `ddc_ui::numparse` (NumberField uses them directly); fields typed in project
// units call the functions below. English: `1,000` = 1000, `1,5` refused; Russian: `1 000,5`.

pub use ddc_ui::numparse::{is_group_space, DecimalMark, NumError};

thread_local! {
    // Follows `crate::set_ui_lang` on the calling thread, like the project units: parallel tests
    // that switch the UI language never change how another thread reads numbers.
    static UI_MARK: std::cell::Cell<DecimalMark> = const { std::cell::Cell::new(DecimalMark::Comma) };
}

/// Decimal mark of the UI language on this thread (set by `crate::set_ui_lang`).
pub fn ui_decimal_mark() -> DecimalMark {
    UI_MARK.with(|c| c.get())
}

pub fn set_ui_decimal_mark(m: DecimalMark) {
    UI_MARK.with(|c| c.set(m));
}

/// One plain number typed in the locale of `mark` (no units): `1,000.5` / `1 000,5`.
pub fn parse_locale_number(s: &str, mark: DecimalMark) -> Result<f64, NumError> {
    ddc_ui::numparse::parse(s, mark)
}

fn num_input_error(e: NumError, pos: usize) -> InputError {
    match e {
        NumError::Empty => InputError::Empty,
        NumError::NotNumber => InputError::NotNumber(pos),
        NumError::CommaDecimal => InputError::CommaDecimal,
    }
}

/// Rewrites every number inside `s` (between units, operators, names) from the locale of `mark`
/// to the canonical `1000.5` the length parsers read; other text is kept as typed. Digits that
/// continue a name (`w1,5`) are left alone. `space_groups`: a space between digits groups
/// thousands (`3 000`); off where spaces separate parts (`6 1/2"`, expressions with names).
pub fn normalize_locale_numbers(s: &str, mark: DecimalMark, space_groups: bool) -> Result<String, InputError> {
    // digits of any script (Arabic-Indic, Devanagari, full-width...) read as ASCII digits
    let folded = ddc_ui::numparse::fold_digits(s);
    let s = folded.as_ref();
    let cs: Vec<(usize, char)> = s.char_indices().collect();
    let mut out = String::with_capacity(s.len());
    let mut k = 0;
    while k < cs.len() {
        let (i, c) = cs[k];
        let digit_at = |j: usize| cs.get(j).is_some_and(|x| x.1.is_ascii_digit());
        let in_word = k > 0 && (cs[k - 1].1.is_alphanumeric() || cs[k - 1].1 == '_');
        let starts = !in_word && (c.is_ascii_digit() || (matches!(c, '.' | ',') && digit_at(k + 1)));
        if !starts {
            out.push(c);
            k += 1;
            continue;
        }
        let mut e = k;
        while e < cs.len() {
            let ch = cs[e].1;
            let ok = ch.is_ascii_digit()
                || (matches!(ch, ',' | '.') && digit_at(e + 1))
                || (space_groups && is_group_space(ch) && e > k && digit_at(e - 1) && digit_at(e + 1));
            if !ok {
                break;
            }
            e += 1;
        }
        let end = cs.get(e).map_or(s.len(), |x| x.0);
        let run = ddc_ui::numparse::canonical(&s[i..end], mark).map_err(|er| num_input_error(er, i + 1))?;
        out.push_str(&run);
        k = e;
    }
    Ok(out)
}

/// Edit-box text without expressions to the canonical text `parse_length` reads: ft-in keeps its
/// spaces (`5'-6 1/2"`), metric groups are glued and a trailing `mm` dropped.
pub(crate) fn edit_text_canonical(s: &str, mark: DecimalMark) -> Result<String, InputError> {
    let s = s.trim();
    if s.contains(['\'', '"', '/']) {
        let t = s.split_whitespace().collect::<Vec<_>>().join(" ");
        return normalize_locale_numbers(&t, mark, false);
    }
    let t = s.trim_end_matches("\u{43c}\u{43c}").trim_end_matches("mm");
    // the unit goes back: in imperial units a bare number is inches, not millimetres
    let mm = if t.len() < s.len() { "mm" } else { "" };
    let t = normalize_locale_numbers(t, mark, true)?;
    Ok(t.chars().filter(|c| !is_group_space(*c)).chain(mm.chars()).collect())
}

/// Length field text in project units, read in the locale of `mark`; expressions and parameter
/// names allowed. The reason comes back when the text is refused.
pub fn parse_edit_length_lang(s: &str, u: Units, sc: &Scope, mark: DecimalMark) -> Result<f64, InputError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(InputError::Empty);
    }
    if crate::expr_field::needs_expr(s, u, sc) {
        // names keep their inner spaces
        return parse_length_in(&normalize_locale_numbers(s, mark, false)?, u, sc);
    }
    parse_length(&edit_text_canonical(s, mark)?, u)
}

/// `6000,4000` / `-1500,500`: two whole numbers of 3+ digits around one comma (group spaces
/// allowed); real decimals with 3+ fraction digits of a millimetre are not typed in practice.
fn looks_like_old_pair(s: &str) -> bool {
    let t: String = s.chars().filter(|c| !matches!(c, ' ' | '\u{a0}' | '\u{202f}')).collect();
    let t = t.trim_start_matches(['-', '+', '\u{2212}']);
    let Some((a, b)) = t.split_once(',') else { return false };
    let b = b.trim_start_matches(['-', '+', '\u{2212}']);
    let whole = |x: &str| x.len() >= 3 && x.bytes().all(|c| c.is_ascii_digit());
    whole(a) && whole(b)
}

/// Signed length (one value of a typed pair) in the locale of `mark`; a typographic minus counts.
fn signed_lang(s: &str, u: Units, sc: &Scope, mark: DecimalMark) -> Result<f64, InputError> {
    let s = s.trim().replace('\u{2212}', "-");
    if s.is_empty() {
        return Err(InputError::Empty);
    }
    if crate::expr_field::needs_expr(&s, u, sc) {
        return parse_signed(&normalize_locale_numbers(&s, mark, false)?, u, sc);
    }
    parse_signed(&edit_text_canonical(&s, mark)?, u, sc)
}

/// One signed length typed on the canvas (shape-edit elevation): zero and a minus are allowed,
/// numbers in the locale of `mark`, a bare number in the units of `u`; `6000,4000` is refused.
pub fn parse_typed_signed(s: &str, u: Units, mark: DecimalMark) -> Result<f64, InputError> {
    if mark == DecimalMark::Comma && looks_like_old_pair(s) {
        return Err(InputError::OldPair);
    }
    signed_lang(s, u, &Scope::none(), mark)
}

/// Why typed canvas text was refused, in the language `ru`; a comma that cannot be a number
/// also names the pair separator, since `6000,4000` used to be a pair.
pub fn typed_error_text(e: &InputError, text: &str, ru: bool) -> String {
    let why = e.text(ru);
    if *e == InputError::OldPair {
        return why;
    }
    if *e == InputError::CommaDecimal && text.contains(',') && !text.contains(PAIR_SEP) {
        let hint = crate::tr_lang(ru, "pair_input.pair_hint", &[]);
        return format!("{why}. {hint}");
    }
    why
}

/// Length field text in the UI language: thousands groups and decimal mark by its locale.
pub fn parse_edit_length(s: &str, u: Units) -> Option<f64> {
    parse_edit_length_in(s, u, &Scope::none())
}

/// [`parse_edit_length`] with expressions and parameter names.
pub fn parse_edit_length_in(s: &str, u: Units, sc: &Scope) -> Option<f64> {
    parse_edit_length_lang(s, u, sc, ui_decimal_mark()).ok()
}

/// Percent (slope): "2,0 %".
pub fn fmt_percent(p: f64) -> String {
    fmt_percent_lang(p, crate::ui_is_ru())
}

/// Percent in the number style of the language: «2,0 %» / «2.0 %».
pub fn fmt_percent_lang(p: f64, ru: bool) -> String {
    format!("{}\u{a0}%", fmt_num_lang(p, PERCENT_DECIMALS, ru))
}

#[cfg(test)]
#[test]
fn one_rule_per_quantity_with_locale_separators() {
    // explicit language: other tests may switch the UI language at any time
    let m = Units::Metric;
    assert_eq!(fmt_area_lang(1234.5678, m, 2, false, true), "1\u{a0}234,57\u{a0}\u{43c}²");
    assert_eq!(fmt_area_lang(1234.5678, m, 2, false, false), "1,234.57\u{a0}m²");
    assert_eq!(fmt_volume_lang(0.5, m, 3, true), "0,500\u{a0}\u{43c}³");
    assert_eq!(fmt_volume_lang(0.5, m, 3, false), "0.500\u{a0}m³");
    assert_eq!(fmt_percent_lang(2.0, true), "2,0\u{a0}%");
    assert_eq!(fmt_percent_lang(2.0, false), "2.0\u{a0}%");
    assert_eq!(fmt_volume_lang(-0.0001, m, 3, true), "0,000\u{a0}\u{43c}³");
}

/// Length with a unit for properties and status: "6 000 mm" (localized) / "19'-8 1/4"".
pub fn display_length(m: f64, u: Units) -> String {
    display_length_lang(m, u, crate::ui_is_ru())
}

/// As `display_length`, with the unit name in an explicit language.
pub fn display_length_lang(m: f64, u: Units, ru: bool) -> String {
    match u {
        Units::Metric => {
            let n = fmt_num_lang(m * 1000.0, 0, ru);
            format!("{n}\u{a0}{}", crate::tr_str(ru, "unit_fmt.unit.mm"))
        }
        Units::Imperial => format_length(m, u),
    }
}

pub fn format_length(m: f64, u: Units) -> String {
    format_length_den(m, u, frac_denominator())
}

/// `format_length` with an explicit fraction denominator (2…256).
pub fn format_length_den(m: f64, u: Units, den: usize) -> String {
    match u {
        Units::Metric => fmt_fixed(m * 1000.0, 0),
        Units::Imperial => {
            let den = clamp_frac(den) as i64;
            let parts = (m / INCH * den as f64).round() as i64;
            let (sign, e) = if parts < 0 {
                ("-", -parts)
            } else {
                ("", parts)
            };
            let (ft, rem) = (e / (12 * den), e % (12 * den));
            let (inch, fr) = (rem / den, rem % den);
            let frac = match fr {
                0 => String::new(),
                f => {
                    let g = gcd(f, den);
                    format!(" {}/{}", f / g, den / g)
                }
            };
            format!("{sign}{ft}'-{inch}{frac}\"")
        }
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn display_numbers_grouped_with_units() {
        use super::*;
        assert_eq!(fmt_num_ru(1234567.891, 2), "1\u{a0}234\u{a0}567,89");
        assert_eq!(fmt_num_ru(-0.0001, 3), "0,000");
        assert_eq!(fmt_num_ru(-1500.0, 0), "-1\u{a0}500");
        assert_eq!(display_length_lang(6.0, Units::Metric, true), "6\u{a0}000\u{a0}\u{43c}\u{43c}");
        assert_eq!(display_length_lang(0.25, Units::Metric, true), "250\u{a0}\u{43c}\u{43c}");
        assert_eq!(display_length_lang(0.25, Units::Metric, false), "250\u{a0}mm");
    }

    #[test]
    fn display_numbers_follow_the_ui_language() {
        use super::*;
        let (m, mm_ru) = (Units::Metric, "\u{43c}\u{43c}");
        assert_eq!(display_length_lang(1.2, m, false), "1,200\u{a0}mm");
        assert_eq!(display_length_lang(1.2, m, true), format!("1\u{a0}200\u{a0}{mm_ru}"));
        assert_eq!(display_length_lang(-2.0, m, false), "-2,000\u{a0}mm");
        assert_eq!(display_length_lang(-2.0, m, true), format!("-2\u{a0}000\u{a0}{mm_ru}"));
        assert_eq!(display_length_lang(1234.5678, m, false), "1,234,568\u{a0}mm");
        assert_eq!(display_length_lang(0.0004, m, false), "0\u{a0}mm");
        assert_eq!(fmt_num_lang(1234567.891, 2, false), "1,234,567.89");
        assert_eq!(fmt_num_lang(1234567.891, 2, true), "1\u{a0}234\u{a0}567,89");
        assert_eq!(fmt_num_lang(-0.5, 1, false), "-0.5");
        assert_eq!(fmt_num_lang(-0.5, 1, true), "-0,5");
        // property fields and the scale bar now group the same way in each language
        assert_eq!(shown_length(1.2, m, DecimalMark::Point, 0, 8), "1,200");
        assert_eq!(shown_length(1.2, m, DecimalMark::Comma, 0, 8), "1\u{a0}200");
        assert_eq!(shown_length(-1.2346, m, DecimalMark::Point, 1, 8), "-1,234.6");
        // the default formatter reads the language of this thread only
        crate::with_test_lang(false, || {
            assert_eq!(fmt_num(3000.0, 0), "3,000");
            assert_eq!(fmt_num(-1234.25, 2), "-1,234.25");
            assert_eq!(display_length(1.2, m), "1,200\u{a0}mm");
        });
        crate::with_test_lang(true, || {
            assert_eq!(fmt_num(3000.0, 0), "3\u{a0}000");
            assert_eq!(fmt_num(-1234.25, 2), "-1\u{a0}234,25");
            assert_eq!(display_length(1.2, m), format!("1\u{a0}200\u{a0}{mm_ru}"));
        });
        // the explicit ru form ignores the UI language
        crate::with_test_lang(false, || assert_eq!(fmt_num_ru(2500.5, 1), "2\u{a0}500,5"));
    }
    #[test]
    fn no_negative_zero() {
        use super::*;
        assert_eq!(fmt_fixed(-0.0, 3), "0.000");
        assert_eq!(fmt_fixed(-0.0001, 3), "0.000");
        assert_eq!(fmt_fixed(-0.0006, 3), "-0.001");
        assert_eq!(fmt_fixed(f64::NAN, 1), "NaN");
        assert_eq!(fmt_fixed(-2.5, 1), "-2.5");
        assert_eq!(format_length(-0.0002, Units::Metric), "0");
        assert_eq!(format_length(-0.0001, Units::Imperial), "0'-0\"");
    }

    use super::*;
    use Units::*;

    const PT: DecimalMark = DecimalMark::Point;
    const CM: DecimalMark = DecimalMark::Comma;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "{a} != {b}");
    }

    #[test]
    fn metric() {
        close(parse_length("3500", Metric).unwrap(), 3.5);
        close(parse_length("350cm", Metric).unwrap(), 3.5);
        close(parse_length("3.5 m", Metric).unwrap(), 3.5);
        close(parse_length("3000+150", Metric).unwrap(), 3.15);
        close(parse_length("4000-100", Metric).unwrap(), 3.9);
        close(parse_length("10ft", Metric).unwrap(), 3.048);
    }

    #[test]
    fn imperial_forms() {
        close(parse_length("12'6\"", Imperial).unwrap(), 12.5 * FOOT);
        close(parse_length("12'-6\"", Imperial).unwrap(), 12.5 * FOOT);
        close(parse_length("12'6", Imperial).unwrap(), 12.5 * FOOT);
        close(parse_length("12'", Imperial).unwrap(), 12.0 * FOOT);
        close(parse_length("6\"", Imperial).unwrap(), 6.0 * INCH);
        close(parse_length("6 1/2\"", Imperial).unwrap(), 6.5 * INCH);
        close(parse_length("150", Imperial).unwrap(), 150.0 * INCH);
        close(parse_length("3500mm", Imperial).unwrap(), 3.5);
        close(
            parse_length("12'6\"+1\"", Imperial).unwrap(),
            12.5 * FOOT + INCH,
        );
    }

    #[test]
    fn bad_input_is_error_with_message() {
        assert_eq!(parse_length("", Metric), Err(InputError::Empty));
        assert_eq!(parse_length("abc", Metric), Err(InputError::NotNumber(1)));
        assert_eq!(
            parse_length("35x0", Metric),
            Err(InputError::UnknownUnit("x0".into()))
        );
        assert_eq!(parse_length("3.5.1", Metric), Err(InputError::NotNumber(4)));
        assert_eq!(parse_length("0", Metric), Err(InputError::NonPositive));
        assert_eq!(parse_length("-5", Metric), Err(InputError::NonPositive));
        assert_eq!(parse_length("1e9", Metric), Err(InputError::TooLarge));
        assert_eq!(
            parse_length("1e", Metric),
            Err(InputError::UnknownUnit("e".into()))
        );
        assert_eq!(parse_length("2000m", Metric), Err(InputError::TooLarge));
        assert!(parse_length("6/0\"", Imperial).is_err());
        assert!(parse_length("3000+", Metric).is_err());
        assert_eq!(parse_typed("3000<x", Metric, PT), Err(InputError::BadAngle));
        assert_eq!(parse_typed("<", Metric, PT), Err(InputError::BadAngle));
        assert_eq!(InputError::NotNumber(3).text(true), "\u{43d}\u{435} \u{447}\u{438}\u{441}\u{43b}\u{43e} \u{432} \u{43f}\u{43e}\u{437}\u{438}\u{446}\u{438}\u{438} 3");
        assert_eq!(InputError::NotNumber(3).text(false), "not a number at position 3");
        for junk in [
            "'", "\"", "''", "1/", "@", ",", ";", "@;", "--", "+", "<<", "1'2'3", "\u{0}",
        ] {
            for m in [PT, CM] {
                let _ = parse_typed(junk, Imperial, m);
                let _ = parse_typed(junk, Metric, m);
            }
        }
    }

    #[test]
    fn typed_forms() {
        for m in [PT, CM] {
            assert_eq!(parse_typed("3500<90", Metric, m), Ok(Typed::Polar(Some(3.5), Some(90.0))));
            assert_eq!(parse_typed("<45", Metric, m), Ok(Typed::Polar(None, Some(45.0))));
            assert_eq!(parse_typed("@1000;-500", Metric, m), Ok(Typed::Pair(1.0, -0.5)));
            assert_eq!(parse_typed("@1000; \u{2212}500", Metric, m), Ok(Typed::Pair(1.0, -0.5)));
            assert_eq!(parse_typed("6000;4000", Metric, m), Ok(Typed::Pair(6.0, 4.0)));
            assert_eq!(parse_typed("6 000;4 000", Metric, m), Ok(Typed::Pair(6.0, 4.0)));
            assert_eq!(parse_typed("3 000", Metric, m), Ok(Typed::Polar(Some(3.0), None)));
        }
        // the decimal mark of the tool's language, never a pair
        assert_eq!(parse_typed("3,5", Metric, CM), Ok(Typed::Polar(Some(0.0035), None)));
        assert_eq!(parse_typed("3,5", Metric, PT), Err(InputError::CommaDecimal));
        assert_eq!(parse_typed("3,000", Metric, PT), Ok(Typed::Polar(Some(3.0), None)));
        assert_eq!(parse_typed("3,000", Metric, CM), Ok(Typed::Polar(Some(0.003), None)));
        assert_eq!(parse_typed("6000,4000", Metric, PT), Err(InputError::CommaDecimal));
        // ru: an old comma pair is refused visibly, a real decimal still reads
        for s in ["6000,4000", "@-1500,500", "6 000,4 000", "3000,250<90"] {
            assert_eq!(parse_typed(s, Metric, CM), Err(InputError::OldPair), "{s}");
        }
        for (s, want) in [("6000,5", 6.0005), ("12,125", 0.012125), ("1,500", 0.0015)] {
            match parse_typed(s, Metric, CM) {
                Ok(Typed::Polar(Some(v), None)) => close(v, want),
                r => panic!("{s}: {r:?}"),
            }
        }
        assert_ne!(InputError::OldPair.text(true), InputError::OldPair.text(false));
        assert_eq!(parse_typed("@1000;2,5", Metric, CM), Ok(Typed::Pair(1.0, 0.0025)));
        assert_eq!(parse_typed("@1000;2,5", Metric, PT), Err(InputError::CommaDecimal));
        assert_eq!(parse_typed("3000<22,5", Metric, CM), Ok(Typed::Polar(Some(3.0), Some(22.5))));
        assert_eq!(parse_typed("3000<22,5", Metric, PT), Err(InputError::CommaDecimal));
        // the English refusal of an old comma pair names the separator
        let why = typed_error_text(&InputError::CommaDecimal, "6000,4000", false);
        assert!(why.len() > InputError::CommaDecimal.text(false).len(), "{why}");
        assert_eq!(typed_error_text(&InputError::Empty, "", true), InputError::Empty.text(true));
    }

    #[test]
    fn format_roundtrip() {
        assert_eq!(format_length(3.5, Metric), "3500");
        assert_eq!(format_length(12.5 * FOOT, Imperial), "12'-6\"");
        assert_eq!(format_length(6.5 * INCH, Imperial), "0'-6 1/2\"");
        let s = format_length(3.81, Imperial);
        close(parse_length(&s, Imperial).unwrap(), 12.5 * FOOT);
    }

    #[test]
    fn cyrillic_units_spaces_exponent_degree() {
        close(parse_length("350\u{43c}\u{43c}", Metric).unwrap(), 0.35);
        close(parse_length("35 \u{441}\u{43c}", Metric).unwrap(), 0.35);
        close(parse_length("3 \u{43c}", Metric).unwrap(), 3.0);
        close(parse_length("3000 + 150", Metric).unwrap(), 3.15);
        close(parse_length("4000 - 100", Metric).unwrap(), 3.9);
        close(parse_length("12' -6\"", Imperial).unwrap(), 12.5 * FOOT);
        close(parse_length("1.5e-3m", Metric).unwrap(), 0.0015);
        close(parse_length("2E2", Metric).unwrap(), 0.2);
        assert_eq!(
            parse_typed("<45°", Metric, PT),
            Ok(Typed::Polar(None, Some(45.0)))
        );
        assert_eq!(
            parse_typed("3500<90°", Metric, CM),
            Ok(Typed::Polar(Some(3.5), Some(90.0)))
        );
    }

    #[test]
    fn imperial_area_volume() {
        assert_eq!(fmt_area_lang(10.0, Metric, 2, false, true), "10,00\u{a0}\u{43c}\u{b2}");
        assert_eq!(fmt_area_in(FT2 * 1234.5, Imperial), "1,234.50\u{a0}ft²");
        assert_eq!(
            fmt_area_with(IN2 * 36.0, Imperial, 2, true),
            "36.00\u{a0}in²"
        );
        assert_eq!(
            fmt_area_with(IN2 * 36.0, Imperial, 2, false),
            "0.2500\u{a0}ft²"
        );
        assert_eq!(fmt_area_with(FT2 * 3.0, Imperial, 2, true), "3.00\u{a0}ft²");
        assert_eq!(fmt_area_with(0.0, Imperial, 2, true), "0.00\u{a0}ft²");
        close(area_in_units(FT2 * 7.0, Imperial), 7.0);
        close(volume_in_units(FT3 * 7.0, Imperial), 7.0);
        close(area_in_units(7.0, Metric), 7.0);
        assert_eq!(fmt_volume_in(FT3 * 2.0, Imperial), "2.000\u{a0}ft³");
        assert_eq!(fmt_volume_lang(1.0, Metric, precision().1, true), "1,000\u{a0}\u{43c}\u{b3}");
    }

    #[test]
    fn rounding_carries_into_inches_and_feet() {
        let m = (5.0 * 12.0 + 11.0 + 63.0 / 64.0) * INCH;
        assert_eq!(edit_length_den(m, Imperial, 64), "5'-11 63/64\"");
        assert_eq!(edit_length_den(m, Imperial, 8), "6'-0\"");
        assert_eq!(edit_length_den(-m, Imperial, 8), "-6'-0\"");
        assert_eq!(edit_length_den(m, Imperial, 2), "6'-0\"");
        let n = (11.0 + 15.0 / 16.0) * INCH;
        assert_eq!(edit_length_den(n, Imperial, 8), "1'-0\"");
        assert_eq!(edit_length_den(n, Imperial, 16), "0'-11 15/16\"");
        assert_eq!(
            edit_length_den((6.0 + 3.0 / 8.0) * INCH, Imperial, 4),
            "0'-6 1/2\""
        );
        // metric fields keep sub-mm values since d8b682ec
        assert_eq!(edit_length_den(0.0031, Metric, 8), "3,1");
    }

    #[test]
    fn fractional_precision() {
        let m = (5.0 * 12.0 + 6.0 + 1.0 / 64.0) * INCH;
        assert_eq!(format_length_den(m, Imperial, 2), "5'-6\"");
        assert_eq!(format_length_den(m, Imperial, 64), "5'-6 1/64\"");
        assert_eq!(format_length_den(6.5 * INCH, Imperial, 2), "0'-6 1/2\"");
        assert_eq!(format_length_den(6.25 * INCH, Imperial, 256), "0'-6 1/4\"");
        assert_eq!(format_length_den(6.25 * INCH, Imperial, 3), "0'-6 1/4\"");
        assert_eq!(format_length_den(-m, Imperial, 64), "-5'-6 1/64\"");
        assert_eq!(clamp_frac(7), DEFAULT_FRAC);
    }

    #[test]
    fn mixed_edit_input() {
        let p = |s: &str, u| parse_edit_length(s, u).unwrap();
        close(p("5'-6 1/2\"", Imperial), 66.5 * INCH);
        close(p("5'-6 1/2\"", Metric), 66.5 * INCH);
        close(p("1.5m", Imperial), 1.5);
        close(p("1,5 m", Metric), 1.5);
        close(p("150 cm", Imperial), 1.5);
        close(p("3 000", Metric), 3.0);
        close(p("3 000 \u{43c}\u{43c}", Metric), 3.0);
        close(p("10'6\" + 300mm", Imperial), 10.5 * FOOT + 0.3);
        close(p("6 1/2", Imperial), 6.5 * INCH);
        assert!(parse_edit_length("", Metric).is_none());
    }

    const EN: DecimalMark = DecimalMark::Point;
    const RU: DecimalMark = DecimalMark::Comma;

    fn pl(s: &str, u: Units, m: DecimalMark) -> Result<f64, InputError> {
        parse_edit_length_lang(s, u, &Scope::none(), m)
    }

    /// The same text per locale, in mm fields: English comma groups, Russian comma is decimal.
    #[test]
    fn locale_number_table_in_length_fields() {
        let rows: [(&str, Option<f64>, Option<f64>); 14] = [
            // text, English metres, Russian metres
            ("1,000", Some(1.0), Some(0.001)),
            ("12,500", Some(12.5), Some(0.0125)),
            ("1,000.5", Some(1.0005), Some(1.0005)),
            ("1,5", None, Some(0.0015)),
            ("1 000,5", None, Some(1.0005)),
            ("1\u{a0}000,5", None, Some(1.0005)),
            ("1.000", Some(0.001), Some(0.001)),
            ("1 000", Some(1.0), Some(1.0)),
            ("1.5", Some(0.0015), Some(0.0015)),
            ("1e3", Some(1.0), Some(1.0)),
            ("1,000 mm", Some(1.0), Some(0.001)),
            ("2,5 m", None, Some(2.5)),
            ("abc", None, None),
            ("1 5", None, None),
        ];
        for (s, en, ru) in rows {
            for (m, want) in [(EN, en), (RU, ru)] {
                match (pl(s, Metric, m), want) {
                    (Ok(v), Some(w)) => close(v, w),
                    (Err(_), None) => {}
                    (got, w) => panic!("{s:?} {m:?}: {got:?}, want {w:?}"),
                }
            }
        }
        assert_eq!(pl("1,5", Metric, EN), Err(InputError::CommaDecimal));
        assert_eq!(pl("-0,5", Metric, EN), Err(InputError::CommaDecimal));
        assert_eq!(pl("", Metric, EN), Err(InputError::Empty));
        assert_eq!(pl("", Metric, RU), Err(InputError::Empty));
        // a plain length is positive; signed fields strip the sign first (opt_field)
        assert_eq!(pl("-0,5", Metric, RU), Err(InputError::NonPositive));
        assert_eq!(parse_locale_number("-0,5", RU), Ok(-0.5));
        assert_eq!(parse_locale_number("-0.5", EN), Ok(-0.5));
        assert!(InputError::CommaDecimal.text(false).contains("1.5"));
        assert!(InputError::CommaDecimal.text(true).contains("1.5"));
    }

    #[test]
    fn locale_numbers_with_units_ft_in_and_expressions() {
        for m in [EN, RU] {
            close(pl("5'-6 1/2\"", Imperial, m).unwrap(), 66.5 * INCH);
            close(pl("10'6\" + 300mm", Imperial, m).unwrap(), 10.5 * FOOT + 0.3);
            close(pl("6 1/2", Imperial, m).unwrap(), 6.5 * INCH);
            close(pl("3000+150", Metric, m).unwrap(), 3.15);
            close(pl("(1000+500)*2", Metric, m).unwrap(), 3.0);
            close(pl("3000/2", Metric, m).unwrap(), 1.5);
        }
        close(pl("12.5\"", Imperial, EN).unwrap(), 12.5 * INCH);
        close(pl("12,5\"", Imperial, RU).unwrap(), 12.5 * INCH);
        assert_eq!(pl("12,5\"", Imperial, EN), Err(InputError::CommaDecimal));
        close(pl("1,000*2", Metric, EN).unwrap(), 2.0);
        close(pl("1,5*2", Metric, RU).unwrap(), 0.003);
        assert_eq!(pl("1,5*2", Metric, EN), Err(InputError::CommaDecimal));
        close(pl("1,5 \u{43c}", Metric, RU).unwrap(), 1.5);
    }

    #[test]
    fn normalize_keeps_names_units_and_operators() {
        let n = |s: &str, m, sp| normalize_locale_numbers(s, m, sp).unwrap();
        assert_eq!(n("1,000 mm + 2,5m", RU, true), "1.000 mm + 2.5m");
        assert_eq!(n("1,000 mm + 2.5m", EN, true), "1000 mm + 2.5m");
        assert_eq!(n("w1,5 * 2", EN, false), "w1,5 * 2");
        assert_eq!(n("3 000", RU, true), "3000");
        assert_eq!(n("3 000", RU, false), "3 000");
        assert_eq!(n("1.5e-3m", EN, true), "1.5e-3m");
        assert_eq!(
            normalize_locale_numbers("x 1 5", RU, true),
            Err(InputError::NotNumber(3))
        );
    }

    #[test]
    fn edit_text_round_trips_per_locale() {
        for m in [EN, RU] {
            for v in [0.0125, 3.0, 12.3456, 0.0005, 250.0] {
                let shown = edit_length_mark(v, Metric, m);
                close(pl(&shown, Metric, m).unwrap(), v);
            }
        }
        assert_eq!(edit_length_mark(3.0, Metric, EN), "3,000");
        assert_eq!(edit_length_mark(3.0, Metric, RU), "3\u{a0}000");
        assert_eq!(edit_length_mark(0.0125, Metric, EN), "12.5");
        assert_eq!(edit_length_mark(0.0125, Metric, RU), "12,5");
    }

    /// Fresh threads read numbers the Russian way until their own `set_ui_lang`; tests never
    /// switch the global language for this.
    #[test]
    fn ui_mark_is_per_thread() {
        let other = std::thread::spawn(|| {
            set_ui_decimal_mark(DecimalMark::Point);
            ui_decimal_mark()
        })
        .join()
        .unwrap();
        assert_eq!(other, DecimalMark::Point);
        assert_eq!(std::thread::spawn(ui_decimal_mark).join().unwrap(), DecimalMark::Comma);
    }
}
