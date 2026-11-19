// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One expression parser for every numeric field: `+ - * / ( )`, unit suffixes, named project
//! parameters. Lengths are metres inside, angles degrees. A bare number keeps the field's own unit
//! (mm / in / deg), so `2*w+150` is "twice w plus 150 mm" and `(3000-200)/4` is 700 mm.
use crate::units::{InputError, Units, INCH};

const MAX_LEN: f64 = 1000.0;
const MAX_TEXT: usize = 2000;
const MAX_DEPTH: usize = 48;

/// What a project parameter name stands for.
#[derive(Clone, Debug, PartialEq)]
pub enum Binding {
    /// A length in metres.
    Length(f64),
    /// A plain number (count, ratio) or an angle in degrees.
    Number(f64),
    /// Formula text, evaluated in the same field domain; names inside resolve again.
    Formula(String),
}

/// Name lookup for the evaluator; `Scope::none()` means "no parameters known".
#[derive(Clone, Copy)]
pub struct Scope<'a> {
    look: Option<&'a dyn Fn(&str) -> Option<Binding>>,
}

impl<'a> Scope<'a> {
    pub fn none() -> Self {
        Scope { look: None }
    }
    pub fn new(look: &'a dyn Fn(&str) -> Option<Binding>) -> Self {
        Scope { look: Some(look) }
    }
    pub fn is_some(&self) -> bool {
        self.look.is_some()
    }
    fn get(&self, name: &str) -> Option<Binding> {
        self.look.and_then(|f| f(name))
    }
}

/// Field domain: a length (unit system of the project) or an angle in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dom {
    Length(Units),
    Angle,
}

#[derive(Clone, Copy, Debug)]
enum Val {
    /// Bare number in the field's own unit.
    Num(f64),
    /// Length in metres.
    Len(f64),
}

impl Dom {
    fn conv(self, raw: f64) -> f64 {
        match self {
            // division, not `* 0.001`: 2800 * 0.001 = 2.8000000000000003
            Dom::Length(Units::Metric) => raw / 1000.0,
            Dom::Length(Units::Imperial) => raw * INCH,
            Dom::Angle => raw,
        }
    }
    fn unconv(self, v: f64) -> f64 {
        match self {
            Dom::Length(Units::Metric) => v * 1000.0,
            Dom::Length(Units::Imperial) => v / INCH,
            Dom::Angle => v,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Op(char),
    Operand(String),
}

fn is_name_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

/// Does `s` need the expression evaluator (otherwise the legacy sum parser keeps its exact
/// behaviour, including feet-inches and fractions)?
pub fn needs_expr(s: &str, u: Units, scope: &Scope) -> bool {
    if s.contains(['*', '(', ')']) {
        return true;
    }
    // `5'-6 1/2"` typed into a metric field is feet-inches with a fraction, not a division
    if u == Units::Metric && s.contains('/') && !s.contains(['\'', '"']) {
        return true;
    }
    if !scope.is_some() {
        return false;
    }
    let mut after_op = true;
    for c in s.chars() {
        if c.is_whitespace() {
            continue;
        }
        if after_op && is_name_start(c) {
            return true;
        }
        after_op = matches!(c, '+' | '-' | '*' | '/' | '(');
    }
    false
}

fn tokenize(s: &str, u: Units) -> Result<Vec<(usize, Tok)>, InputError> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let flush = |out: &mut Vec<(usize, Tok)>, from: usize, to: usize| {
        let t = &s[from..to];
        if !t.trim().is_empty() {
            let lead = t.len() - t.trim_start().len();
            out.push((from + lead, Tok::Operand(t.trim().to_string())));
        }
    };
    for (i, ch) in s.char_indices() {
        let cur = s[start..i].trim();
        let op = match ch {
            '(' | ')' | '*' => true,
            '+' | '-' => {
                // `12'-6"` and `1e-3` are one operand, not a subtraction
                let prev = s[..i].trim_end();
                let after_op = prev.is_empty() || prev.ends_with(['+', '-', '*', '/', '(']);
                after_op || crate::units::is_operator(b, i)
            }
            '/' => {
                // inch fractions `6 1/2"` stay inside the operand
                let frac = u == Units::Imperial
                    && cur.contains(['\'', ' '])
                    && cur.ends_with(|l: char| l.is_ascii_digit())
                    && b.get(i + 1).is_some_and(u8::is_ascii_digit);
                !frac
            }
            _ => false,
        };
        if op {
            flush(&mut out, start, i);
            out.push((i, Tok::Op(ch)));
            start = i + 1;
        }
    }
    flush(&mut out, start, s.len());
    Ok(out)
}

struct Eval<'a, 'b> {
    toks: Vec<(usize, Tok)>,
    pos: usize,
    dom: Dom,
    scope: &'a Scope<'b>,
    /// names being expanded (lower case) for cycle detection
    stack: &'a mut Vec<String>,
    depth: usize,
    end: usize,
}

impl Eval<'_, '_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.1)
    }
    fn here(&self) -> usize {
        self.toks.get(self.pos).map_or(self.end, |t| t.0) + 1
    }

    fn expr(&mut self) -> Result<Val, InputError> {
        let mut v = self.term()?;
        while let Some(Tok::Op(c @ ('+' | '-'))) = self.peek().cloned() {
            self.pos += 1;
            let r = self.term()?;
            v = add(self.dom, v, r, c == '-');
        }
        Ok(v)
    }

    fn term(&mut self) -> Result<Val, InputError> {
        let mut v = self.unary()?;
        while let Some(Tok::Op(c @ ('*' | '/'))) = self.peek().cloned() {
            let at = self.here();
            self.pos += 1;
            let r = self.unary()?;
            v = if c == '*' { mul(v, r, at)? } else { div(v, r, at)? };
        }
        Ok(v)
    }

    fn unary(&mut self) -> Result<Val, InputError> {
        match self.peek().cloned() {
            Some(Tok::Op('-')) => {
                self.pos += 1;
                Ok(match self.unary()? {
                    Val::Num(x) => Val::Num(-x),
                    Val::Len(x) => Val::Len(-x),
                })
            }
            Some(Tok::Op('+')) => {
                self.pos += 1;
                self.unary()
            }
            _ => self.atom(),
        }
    }

    fn atom(&mut self) -> Result<Val, InputError> {
        let at = self.here();
        match self.toks.get(self.pos).cloned() {
            Some((_, Tok::Op('('))) => {
                self.pos += 1;
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(InputError::BadExpr(at));
                }
                let v = self.expr()?;
                self.depth -= 1;
                match self.peek() {
                    Some(Tok::Op(')')) => {
                        self.pos += 1;
                        Ok(v)
                    }
                    _ => Err(InputError::BadExpr(self.here())),
                }
            }
            Some((p, Tok::Operand(t))) => {
                self.pos += 1;
                operand(&t, p, self.dom, self.scope, self.stack)
            }
            _ => Err(InputError::BadExpr(at)),
        }
    }
}

fn add(dom: Dom, a: Val, b: Val, sub: bool) -> Val {
    let s = if sub { -1.0 } else { 1.0 };
    match (a, b) {
        (Val::Num(x), Val::Num(y)) => Val::Num(x + s * y),
        (Val::Len(x), Val::Len(y)) => Val::Len(x + s * y),
        (Val::Num(x), Val::Len(y)) => Val::Len(dom.conv(x) + s * y),
        (Val::Len(x), Val::Num(y)) => Val::Len(x + s * dom.conv(y)),
    }
}

fn mul(a: Val, b: Val, at: usize) -> Result<Val, InputError> {
    match (a, b) {
        (Val::Num(x), Val::Num(y)) => Ok(Val::Num(x * y)),
        (Val::Num(k), Val::Len(l)) | (Val::Len(l), Val::Num(k)) => Ok(Val::Len(l * k)),
        _ => Err(InputError::BadExpr(at)),
    }
}

fn div(a: Val, b: Val, at: usize) -> Result<Val, InputError> {
    let zero = match b {
        Val::Num(y) | Val::Len(y) => y == 0.0,
    };
    if zero {
        return Err(InputError::DivZero(at));
    }
    match (a, b) {
        (Val::Num(x), Val::Num(y)) => Ok(Val::Num(x / y)),
        (Val::Len(l), Val::Num(k)) => Ok(Val::Len(l / k)),
        (Val::Len(x), Val::Len(y)) => Ok(Val::Num(x / y)),
        _ => Err(InputError::BadExpr(at)),
    }
}

fn operand(
    t: &str,
    off: usize,
    dom: Dom,
    scope: &Scope,
    stack: &mut Vec<String>,
) -> Result<Val, InputError> {
    let first = t.chars().next().unwrap_or(' ');
    if is_name_start(first) && scope.is_some() {
        return name_value(t, dom, scope, stack);
    }
    match dom {
        Dom::Angle => {
            let n = t
                .strip_suffix('°')
                .or_else(|| t.strip_suffix("deg"))
                .unwrap_or(t)
                .trim();
            let n: String = squeeze_groups(n);
            match n.parse::<f64>() {
                Ok(v) if v.is_finite() => Ok(Val::Num(v)),
                _ => Err(InputError::NotNumber(off + 1)),
            }
        }
        Dom::Length(u) => {
            let g = if u == Units::Metric { squeeze_groups(t) } else { t.to_string() };
            let explicit = g.contains(['\'', '"']) || crate::units::unit_start(&g) < g.len();
            let m = crate::units::term_value(&g, u, off)?;
            if explicit {
                Ok(Val::Len(m))
            } else {
                Ok(Val::Num(dom.unconv(m)))
            }
        }
    }
}

/// "3 000" -> "3000": spaces between digits are group separators.
fn squeeze_groups(s: &str) -> String {
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (i, &c) in cs.iter().enumerate() {
        let sep = matches!(c, ' ' | '\u{a0}' | '\u{202f}')
            && i > 0
            && cs[i - 1].is_ascii_digit()
            && cs.get(i + 1).is_some_and(char::is_ascii_digit);
        if !sep {
            out.push(c);
        }
    }
    out
}

fn name_value(
    name: &str,
    dom: Dom,
    scope: &Scope,
    stack: &mut Vec<String>,
) -> Result<Val, InputError> {
    let key = name.to_lowercase();
    if stack.contains(&key) {
        return Err(InputError::Cycle(name.to_string()));
    }
    let Some(b) = scope.get(name) else {
        return Err(InputError::UnknownName(name.to_string()));
    };
    let v = match b {
        Binding::Length(m) => Val::Len(m),
        Binding::Number(n) => Val::Num(n),
        Binding::Formula(f) => {
            if stack.len() >= MAX_DEPTH {
                return Err(InputError::Cycle(name.to_string()));
            }
            stack.push(key);
            let r = eval_val(&f, dom, scope, stack);
            stack.pop();
            r?
        }
    };
    let finite = match v {
        Val::Num(x) | Val::Len(x) => x.is_finite(),
    };
    if finite {
        Ok(v)
    } else {
        Err(InputError::UnknownName(name.to_string()))
    }
}

fn eval_val(
    s: &str,
    dom: Dom,
    scope: &Scope,
    stack: &mut Vec<String>,
) -> Result<Val, InputError> {
    if s.len() > MAX_TEXT {
        return Err(InputError::BadExpr(MAX_TEXT));
    }
    if s.trim().is_empty() {
        return Err(InputError::Empty);
    }
    let u = match dom {
        Dom::Length(u) => u,
        Dom::Angle => Units::Metric,
    };
    let toks = tokenize(s, u)?;
    let mut e = Eval {
        toks,
        pos: 0,
        dom,
        scope,
        stack,
        depth: 0,
        end: s.len(),
    };
    let v = e.expr()?;
    if e.pos < e.toks.len() {
        return Err(InputError::BadExpr(e.here()));
    }
    Ok(v)
}

/// Value of the expression: metres for a length field, degrees for an angle field. Zero and
/// negative values are allowed here; callers add their own range rule.
pub fn eval(s: &str, dom: Dom, scope: &Scope) -> Result<f64, InputError> {
    let mut stack = Vec::new();
    let v = match eval_val(s, dom, scope, &mut stack)? {
        Val::Num(x) => dom.conv(x),
        Val::Len(x) => {
            if matches!(dom, Dom::Angle) {
                return Err(InputError::BadExpr(1));
            }
            x
        }
    };
    if !v.is_finite() {
        return Err(InputError::BadExpr(1));
    }
    if matches!(dom, Dom::Length(_)) && v.abs() > MAX_LEN {
        return Err(InputError::TooLarge);
    }
    Ok(v)
}

/// Positive length in metres from an expression.
pub fn eval_length(s: &str, u: Units, scope: &Scope) -> Result<f64, InputError> {
    let v = eval(s, Dom::Length(u), scope)?;
    if v <= 0.0 {
        Err(InputError::NonPositive)
    } else {
        Ok(v)
    }
}

/// Angle in degrees from an expression.
pub fn eval_angle(s: &str, scope: &Scope) -> Result<f64, InputError> {
    eval(s, Dom::Angle, scope)
}

/// Commit rule of a numeric field: the new value when the text is valid, otherwise the old value
/// is kept and the reason is returned for the line under the field.
pub fn commit_length(text: &str, old: f64, u: Units, scope: &Scope) -> (f64, Option<InputError>) {
    match eval_length(text, u, scope) {
        Ok(v) => (v, None),
        Err(e) => (old, Some(e)),
    }
}

/// Trailing name fragment of the text (what completion replaces); empty when the cursor is not
/// inside a name.
pub fn name_fragment(text: &str) -> &str {
    let mut start = text.len();
    for (i, c) in text.char_indices().rev() {
        if c.is_alphanumeric() || c == '_' {
            start = i;
        } else {
            break;
        }
    }
    let frag = &text[start..];
    if frag.chars().next().is_some_and(is_name_start) {
        frag
    } else {
        ""
    }
}

/// Names that start with the fragment (case-insensitive), sorted, at most `limit`.
pub fn complete(text: &str, names: &[String], limit: usize) -> Vec<String> {
    let frag = name_fragment(text).to_lowercase();
    if frag.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<String> = names
        .iter()
        .filter(|n| n.to_lowercase().starts_with(&frag) && n.to_lowercase() != frag)
        .cloned()
        .collect();
    v.sort_by_key(|n| (n.chars().count(), n.to_lowercase()));
    v.truncate(limit);
    v
}

/// Text with the trailing fragment replaced by `name` (Enter on a suggestion).
pub fn apply_completion(text: &str, name: &str) -> String {
    let frag = name_fragment(text);
    format!("{}{}", &text[..text.len() - frag.len()], name)
}

/// Caption for a field bound to a formula: `name = formula = value`.
pub fn bound_caption(name: &str, formula: &str, value: &str) -> String {
    format!("{name} = {formula} = {value}")
}

/// Locale key and named argument for the line under a field; every variant has one.
pub fn reason_key(e: &InputError) -> (&'static str, Option<(&'static str, String)>) {
    match e {
        InputError::Empty => ("units.err.empty", None),
        InputError::NotNumber(p) => ("units.err.not_number", Some(("pos", p.to_string()))),
        InputError::UnknownUnit(u) => ("units.err.unknown_unit", Some(("unit", u.clone()))),
        InputError::NonPositive => ("units.err.non_positive", None),
        InputError::TooLarge => (
            "units.err.too_large",
            Some(("max", crate::units::MAX_LEN.to_string())),
        ),
        InputError::BadAngle => ("units.err.bad_angle", None),
        InputError::UnknownName(n) => ("units.err.unknown_name", Some(("name", n.clone()))),
        InputError::Cycle(n) => ("units.err.cycle", Some(("name", n.clone()))),
        InputError::BadExpr(p) => ("units.err.bad_expr", Some(("pos", p.to_string()))),
        InputError::DivZero(p) => ("units.err.div_zero", Some(("pos", p.to_string()))),
        InputError::CommaDecimal => ("decimal_input.comma_decimal", None),
        InputError::OldPair => ("pair_input.old_pair", None),
    }
}

/// Text of a field error in the catalog language (the `Display` text is for logs only).
pub fn error_text(c: &ddc_ui::Catalog, e: &InputError) -> String {
    match reason_key(e) {
        (k, Some((n, v))) => c.tf(k, &[(n, v.as_str())]),
        (k, None) => c.t(k),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::{parse_length, parse_length_in, Units::*, FOOT};

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "{a} != {b}");
    }
    fn look(name: &str) -> Option<Binding> {
        match name.to_lowercase().as_str() {
            "w" => Some(Binding::Length(2.0)),
            "n" => Some(Binding::Number(3.0)),
            "h" => Some(Binding::Formula("w*2+500".into())),
            "a" => Some(Binding::Formula("b+1".into())),
            "b" => Some(Binding::Formula("a+1".into())),
            "wall height" => Some(Binding::Length(3.0)),
            _ => None,
        }
    }

    #[test]
    fn units_mul_div_parens() {
        let s = Scope::none();
        close(parse_length("2*1000", Metric).unwrap(), 2.0);
        close(parse_length("(3000-200)/4", Metric).unwrap(), 0.7);
        close(parse_length("3000/4+150", Metric).unwrap(), 0.9);
        close(parse_length("2*(1m+500)", Metric).unwrap(), 3.0);
        close(parse_length("-(100-500)", Metric).unwrap(), 0.4);
        close(parse_length("1m/2", Metric).unwrap(), 0.5);
        close(eval_length("2 * 12'6\"", Imperial, &s).unwrap(), 25.0 * FOOT);
        close(eval_length("(6 1/2\" + 1\")*2", Imperial, &s).unwrap(), 15.0 * INCH);
        // legacy forms keep working
        close(parse_length("12'-6\"", Imperial).unwrap(), 12.5 * FOOT);
        close(parse_length("6 1/2", Imperial).unwrap(), 6.5 * INCH);
        assert!(parse_length("(1+2", Metric).is_err());
        assert!(parse_length("1+*2", Metric).is_err());
        assert!(parse_length("1m*1m", Metric).is_err());
        assert!(matches!(
            parse_length("5/0", Metric),
            Err(InputError::DivZero(_))
        ));
    }

    #[test]
    fn units_param_name_resolves() {
        let sc = Scope::new(&look);
        close(parse_length_in("2*w+150", Metric, &sc).unwrap(), 4.15);
        close(parse_length_in("w/n", Metric, &sc).unwrap(), 2.0 / 3.0);
        close(parse_length_in("W", Metric, &sc).unwrap(), 2.0);
        close(parse_length_in("h + 100", Metric, &sc).unwrap(), 4.6);
        close(parse_length_in("Wall Height - 500", Metric, &sc).unwrap(), 2.5);
        close(eval_angle("n*30+15", &sc).unwrap(), 105.0);
        close(eval_angle("90 - 45°", &sc).unwrap(), 45.0);
        // without parameters an unknown word keeps the old error
        assert_eq!(parse_length("abc", Metric), Err(InputError::NotNumber(1)));
    }

    #[test]
    fn unknown_name_keeps_old_value() {
        let sc = Scope::new(&look);
        let (v, e) = commit_length("zz*2", 3.5, Metric, &sc);
        assert_eq!(v, 3.5);
        assert_eq!(e, Some(InputError::UnknownName("zz".into())));
        let (v, e) = commit_length("a+1", 3.5, Metric, &sc);
        assert_eq!(v, 3.5);
        assert!(matches!(e, Some(InputError::Cycle(_))), "{e:?}");
        let (v, e) = commit_length("(2", 3.5, Metric, &sc);
        assert_eq!((v, e.is_some()), (3.5, true));
        let (v, e) = commit_length("w*2", 3.5, Metric, &sc);
        assert_eq!((v, e), (4.0, None));
        let (v, e) = commit_length("0*w", 3.5, Metric, &sc);
        assert_eq!((v, e), (3.5, Some(InputError::NonPositive)));
        assert_eq!(
            reason_key(&InputError::Cycle("a".into())),
            ("units.err.cycle", Some(("name", "a".into())))
        );
    }

    #[test]
    fn completion_and_caption() {
        let names: Vec<String> = ["Width", "Wall Height", "wing", "Depth"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(name_fragment("2*wi"), "wi");
        assert_eq!(name_fragment("2*"), "");
        assert_eq!(name_fragment("2*5"), "");
        assert_eq!(complete("2*wi", &names, 5), vec!["wing", "Width"]);
        assert_eq!(apply_completion("2*wi", "Width"), "2*Width");
        assert_eq!(bound_caption("h", "w*2", "4000"), "h = w*2 = 4000");
    }

    #[test]
    fn deep_nesting_is_an_error_not_a_crash() {
        let s = format!("{}1{}", "(".repeat(5000), ")".repeat(5000));
        assert!(parse_length(&s, Metric).is_err());
    }

    fn every_error() -> Vec<InputError> {
        vec![
            InputError::Empty,
            InputError::NotNumber(7),
            InputError::UnknownUnit("qq".into()),
            InputError::NonPositive,
            InputError::TooLarge,
            InputError::BadAngle,
            InputError::UnknownName("Width".into()),
            InputError::Cycle("Loop".into()),
            InputError::BadExpr(4),
            InputError::DivZero(9),
        ]
    }

    fn has_cyr(t: &str) -> bool {
        t.chars().any(|ch| ('\u{400}'..='\u{4ff}').contains(&ch))
    }

    #[test]
    fn input_error_every_variant_has_key() {
        use ddc_ui::{Catalog, Lang};
        assert_eq!(every_error().len(), 10);
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            for e in every_error() {
                let (k, arg) = reason_key(&e);
                assert!(k.starts_with("units.err."), "{k}");
                assert!(c.table(lang).contains_key(k), "{k} missing in {lang:?}");
                let text = error_text(&c, &e);
                assert_ne!(text, k);
                assert!(!text.contains('{'), "unfilled placeholder: {text}");
                if let Some((_, v)) = arg {
                    assert!(text.contains(&v), "{text} lacks {v}");
                }
            }
        }
    }

    #[test]
    fn input_error_en_has_no_cyrillic() {
        use ddc_ui::{Catalog, Lang};
        let c = Catalog::new(Lang::En);
        for e in every_error() {
            let t = error_text(&c, &e);
            assert!(!has_cyr(&t), "{t}");
            // explicit language: Display follows the shared UI flag
            let d = e.text(false);
            assert!(!has_cyr(&d), "{d}");
        }
    }

    #[test]
    fn input_error_ru_texts() {
        use ddc_ui::{Catalog, Lang};
        let ru = Catalog::new(Lang::Ru);
        let en = Catalog::new(Lang::En);
        for e in every_error() {
            let t = error_text(&ru, &e);
            assert!(has_cyr(&t), "{t}");
            assert_ne!(t, error_text(&en, &e));
        }
    }

    #[test]
    fn expr_field_shows_catalog_text() {
        use ddc_ui::{Catalog, Lang};
        let sc = Scope::new(&look);
        let (v, e) = commit_length("zz*2", 3.5, Metric, &sc);
        assert_eq!(v, 3.5);
        let e = e.unwrap();
        let en = error_text(&Catalog::new(Lang::En), &e);
        assert_eq!(en, "Unknown name: zz. Value kept.");
        let ru = error_text(&Catalog::new(Lang::Ru), &e);
        assert!(ru.contains("zz") && ru != en);
        let (_, e) = commit_length("abc", 3.5, Metric, &Scope::none());
        let t = error_text(&Catalog::new(Lang::En), &e.unwrap());
        assert!(t.contains('1') && !has_cyr(&t), "{t}");
    }
}
