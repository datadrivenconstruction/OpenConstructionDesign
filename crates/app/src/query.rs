// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Project element search query (WORKFLOWS W12, W1). Examples (Russian keywords are accepted too):
//! `doors width>900`, `walls type=Masonry 200 level=2`, `windows or doors L2`, `not rooms`,
//! `... -> width=width+100`. Parse → expression (and/or/not, parentheses) → rows of all levels (SQL);
//! assignment after `->` is one operation (one undo step); project filters; palette rows (Ctrl+K) and picking from it.
use crate::editor::{Editor, Sel};
use crate::shell::EditorApp;
use crate::expr_field::Scope;
use crate::units::{
    edit_length, parse_edit_length_lang, parse_locale_number, ui_decimal_mark, Units,
};
use ddc_ui::numparse::DecimalMark;
use ddc_ui::widgets::palette::Extra;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `~`: text contains.
    Has,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// Category key (`wall`, `door`, …; `opening` covers doors and windows).
    Cat(&'static str),
    Cmp {
        key: String,
        op: Op,
        val: String,
    },
    /// A word without an operator: contained in the type, name or mark; `#12` is an id.
    Text(String),
    /// An empty list means all elements.
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
}

/// New value: a literal or "field (+−*/) number" (`width=width+100`).
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Lit(String),
    Rel(char, String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Assign {
    pub key: String,
    pub val: Val,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub expr: Expr,
    pub set: Vec<Assign>,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String),
    Quoted(String),
    Op(Op),
    Open,
    Close,
    Arrow,
}

fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// Argument of a filter-language error: literal text or a locale key rendered per language.
#[derive(Debug, Clone, PartialEq)]
pub enum QArg {
    Text(String),
    Key(&'static str),
}

/// Filter-language error: locale key plus arguments, rendered in the caller's language.
#[derive(Debug, Clone, PartialEq)]
pub struct QErr {
    pub key: &'static str,
    pub args: Vec<(&'static str, QArg)>,
}

impl QErr {
    fn new(key: &'static str) -> Self {
        Self { key, args: vec![] }
    }
    fn with(mut self, name: &'static str, v: impl AsRef<str>) -> Self {
        self.args.push((name, QArg::Text(v.as_ref().to_string())));
        self
    }
    fn with_key(mut self, name: &'static str, key: &'static str) -> Self {
        self.args.push((name, QArg::Key(key)));
        self
    }
    /// Message text in Russian (`ru`) or English.
    pub fn render(&self, ru: bool) -> String {
        let args: Vec<(&str, String)> = self
            .args
            .iter()
            .map(|(n, a)| match a {
                QArg::Text(t) => (*n, t.clone()),
                QArg::Key(k) => (*n, crate::tr_lang(ru, k, &[])),
            })
            .collect();
        let refs: Vec<(&str, &str)> = args.iter().map(|(n, v)| (*n, v.as_str())).collect();
        crate::tr_lang(ru, self.key, &refs)
    }
}

/// Renders in the current UI language.
impl std::fmt::Display for QErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render(crate::ui_lang_now()))
    }
}

impl From<QErr> for String {
    fn from(e: QErr) -> String {
        e.to_string()
    }
}

/// Errors from the editor layer arrive as ready text.
impl From<String> for QErr {
    fn from(s: String) -> Self {
        QErr::new("query.err.raw").with("text", s)
    }
}

fn lex(s: &str) -> Result<Vec<Tok>, QErr> {
    let c: Vec<char> = s.chars().collect();
    let (mut out, mut w, mut i) = (vec![], String::new(), 0);
    fn flush(w: &mut String, out: &mut Vec<Tok>) {
        if !w.is_empty() {
            out.push(Tok::Word(std::mem::take(w)));
        }
    }
    while i < c.len() {
        let (ch, nx) = (c[i], c.get(i + 1).copied());
        match ch {
            _ if ch.is_whitespace() || ch == ';' => flush(&mut w, &mut out),
            // inches: `6"`, `3'-0"`
            '"' if w.ends_with(|c: char| c.is_ascii_digit() || c == '\'') => w.push(ch),
            '|' | '&' => {
                flush(&mut w, &mut out);
                out.push(Tok::Word(ch.to_string()));
                i += usize::from(nx == Some(ch));
            }
            '!' if nx != Some('=') => {
                flush(&mut w, &mut out);
                out.push(Tok::Word("!".into()));
            }
            '"' | '«' => {
                flush(&mut w, &mut out);
                let end = if ch == '"' { '"' } else { '»' };
                let j = c[i + 1..]
                    .iter()
                    .position(|&x| x == end)
                    .ok_or_else(|| QErr::new("query.err.unclosed_quote"))?;
                out.push(Tok::Quoted(c[i + 1..i + 1 + j].iter().collect()));
                i += j + 1;
            }
            '(' | ')' => {
                flush(&mut w, &mut out);
                out.push(if ch == '(' { Tok::Open } else { Tok::Close });
            }
            '-' if nx == Some('>') => {
                flush(&mut w, &mut out);
                out.push(Tok::Arrow);
                i += 1;
            }
            '→' => {
                flush(&mut w, &mut out);
                out.push(Tok::Arrow);
            }
            '=' | '<' | '>' | '!' | '~' | ':' | '≥' | '≤' | '≠' => {
                flush(&mut w, &mut out);
                let (op, two) = match (ch, nx) {
                    ('>', Some('=')) => (Op::Ge, true),
                    ('<', Some('=')) => (Op::Le, true),
                    ('<', Some('>')) | ('!', Some('=')) => (Op::Ne, true),
                    ('=', Some('=')) => (Op::Eq, true),
                    ('=' | ':', _) => (Op::Eq, false),
                    ('>', _) => (Op::Gt, false),
                    ('<', _) => (Op::Lt, false),
                    ('≥', _) => (Op::Ge, false),
                    ('≤', _) => (Op::Le, false),
                    ('~', _) => (Op::Has, false),
                    _ => (Op::Ne, false),
                };
                out.push(Tok::Op(op));
                i += usize::from(two);
            }
            _ => w.push(ch),
        }
        i += 1;
    }
    flush(&mut w, &mut out);
    Ok(out)
}

const AND: [&str; 4] = ["\u{438}", "and", "&", "&&"];
const OR: [&str; 4] = ["\u{438}\u{43b}\u{438}", "or", "|", "||"];
const NOT: [&str; 3] = ["\u{43d}\u{435}", "not", "!"];

fn is_kw(w: &str) -> bool {
    let l = lower(w);
    AND.contains(&l.as_str()) || OR.contains(&l.as_str()) || NOT.contains(&l.as_str())
}

/// Category word (Russian/English, singular/plural).
pub fn cat_word(w: &str) -> Option<&'static str> {
    Some(match lower(w).as_str() {
        "\u{441}\u{442}\u{435}\u{43d}\u{430}" | "\u{441}\u{442}\u{435}\u{43d}\u{44b}" | "wall" | "walls" => "wall",
        "\u{434}\u{432}\u{435}\u{440}\u{44c}" | "\u{434}\u{432}\u{435}\u{440}\u{438}" | "\u{434}\u{432}\u{435}\u{440}\u{435}\u{439}" | "door" | "doors" => "door",
        "\u{43e}\u{43a}\u{43d}\u{43e}" | "\u{43e}\u{43a}\u{43d}\u{430}" | "\u{43e}\u{43a}\u{43e}\u{43d}" | "window" | "windows" => "window",
        "\u{43f}\u{440}\u{43e}\u{451}\u{43c}" | "\u{43f}\u{440}\u{43e}\u{435}\u{43c}" | "\u{43f}\u{440}\u{43e}\u{451}\u{43c}\u{44b}" | "\u{43f}\u{440}\u{43e}\u{435}\u{43c}\u{44b}" | "opening" | "openings" => {
            "opening"
        }
        "\u{432}\u{438}\u{442}\u{440}\u{430}\u{436}" | "\u{432}\u{438}\u{442}\u{440}\u{430}\u{436}\u{438}" | "\u{43d}\u{430}\u{432}\u{435}\u{441}\u{43d}\u{44b}\u{435}" | "curtain" | "curtains" => {
            "curtain"
        }
        "\u{43f}\u{435}\u{440}\u{435}\u{43a}\u{440}\u{44b}\u{442}\u{438}\u{435}" | "\u{43f}\u{435}\u{440}\u{435}\u{43a}\u{440}\u{44b}\u{442}\u{438}\u{44f}" | "\u{43f}\u{43b}\u{438}\u{442}\u{430}" | "\u{43f}\u{43b}\u{438}\u{442}\u{44b}" | "floor" | "floors" | "slab" | "slabs" => {
            "floor"
        }
        "\u{43f}\u{43e}\u{43c}\u{435}\u{449}\u{435}\u{43d}\u{438}\u{435}" | "\u{43f}\u{43e}\u{43c}\u{435}\u{449}\u{435}\u{43d}\u{438}\u{44f}" | "\u{43a}\u{43e}\u{43c}\u{43d}\u{430}\u{442}\u{430}" | "\u{43a}\u{43e}\u{43c}\u{43d}\u{430}\u{442}\u{44b}" | "room" | "rooms" => {
            "room"
        }
        "\u{43a}\u{43e}\u{43b}\u{43e}\u{43d}\u{43d}\u{430}" | "\u{43a}\u{43e}\u{43b}\u{43e}\u{43d}\u{43d}\u{44b}" | "\u{43a}\u{43e}\u{43b}\u{43e}\u{43d}\u{43d}" | "column" | "columns" => "column",
        "\u{431}\u{430}\u{43b}\u{43a}\u{430}" | "\u{431}\u{430}\u{43b}\u{43a}\u{438}" | "\u{431}\u{430}\u{43b}\u{43e}\u{43a}" | "beam" | "beams" => "beam",
        "\u{43a}\u{440}\u{44b}\u{448}\u{430}" | "\u{43a}\u{440}\u{44b}\u{448}\u{438}" | "\u{43a}\u{440}\u{43e}\u{432}\u{43b}\u{44f}" | "roof" | "roofs" => "roof",
        "\u{43f}\u{43e}\u{442}\u{43e}\u{43b}\u{43e}\u{43a}" | "\u{43f}\u{43e}\u{442}\u{43e}\u{43b}\u{43a}\u{438}" | "\u{43f}\u{43e}\u{442}\u{43e}\u{43b}\u{43a}\u{43e}\u{432}" | "ceiling" | "ceilings" => {
            "ceiling"
        }
        "\u{43b}\u{435}\u{441}\u{442}\u{43d}\u{438}\u{446}\u{430}" | "\u{43b}\u{435}\u{441}\u{442}\u{43d}\u{438}\u{446}\u{44b}" | "\u{43b}\u{435}\u{441}\u{442}\u{43d}\u{438}\u{446}" | "stair" | "stairs" => "stair",
        "\u{43e}\u{433}\u{440}\u{430}\u{436}\u{434}\u{435}\u{43d}\u{438}\u{435}" | "\u{43e}\u{433}\u{440}\u{430}\u{436}\u{434}\u{435}\u{43d}\u{438}\u{44f}" | "railing" | "railings" => "railing",
        "\u{441}\u{435}\u{442}\u{44c}" | "\u{441}\u{435}\u{442}\u{438}" | "\u{442}\u{440}\u{443}\u{431}\u{44b}" | "\u{432}\u{43e}\u{437}\u{434}\u{443}\u{445}\u{43e}\u{432}\u{43e}\u{434}\u{44b}" | "mep" | "pipe" | "pipes" | "duct" | "ducts" => {
            "mep"
        }
        "\u{43a}\u{43e}\u{43c}\u{43f}\u{43e}\u{43d}\u{435}\u{43d}\u{442}" | "\u{43a}\u{43e}\u{43c}\u{43f}\u{43e}\u{43d}\u{435}\u{43d}\u{442}\u{44b}" | "\u{43a}\u{43e}\u{43c}\u{43f}\u{43e}\u{43d}\u{435}\u{43d}\u{442}\u{43e}\u{432}" | "comp" | "component" | "components" => {
            "comp"
        }
        "\u{442}\u{435}\u{43b}\u{43e}" | "\u{442}\u{435}\u{43b}\u{430}" | "\u{442}\u{435}\u{43b}" | "solid" | "solids" | "body" | "bodies" => "solid",
        "\u{43c}\u{430}\u{441}\u{441}\u{430}" | "\u{43c}\u{430}\u{441}\u{441}\u{44b}" | "mass" | "masses" => "mass",
        _ => return None,
    })
}

/// Canonical field name; an unknown one is an element parameter (import), lowercased.
pub fn canon(k: &str) -> String {
    let l = lower(k.trim());
    let c = match l.as_str() {
        "id" | "\u{438}\u{434}" | "#" | "№" => "id",
        "\u{43a}\u{430}\u{442}" | "\u{43a}\u{430}\u{442}\u{435}\u{433}\u{43e}\u{440}\u{438}\u{44f}" | "category" | "cat" => "category",
        "\u{443}\u{440}\u{43e}\u{432}\u{435}\u{43d}\u{44c}" | "\u{44d}\u{442}\u{430}\u{436}" | "\u{443}\u{440}" | "level" | "lvl" | "l" => "level",
        "\u{442}\u{438}\u{43f}" | "\u{442}\u{438}\u{43f}\u{43e}\u{440}\u{430}\u{437}\u{43c}\u{435}\u{440}" | "type" => "type",
        "\u{438}\u{43c}\u{44f}" | "\u{43d}\u{430}\u{437}\u{432}\u{430}\u{43d}\u{438}\u{435}" | "name" => "name",
        "\u{43c}\u{430}\u{440}\u{43a}\u{430}" | "\u{43d}\u{43e}\u{43c}\u{435}\u{440}" | "mark" | "number" | "tag" => "mark",
        "\u{448}\u{438}\u{440}\u{438}\u{43d}\u{430}" | "width" | "w" => "width",
        "\u{432}\u{44b}\u{441}\u{43e}\u{442}\u{430}" | "height" | "h" => "height",
        "\u{43f}\u{43e}\u{440}\u{43e}\u{433}" | "\u{43f}\u{43e}\u{434}\u{43e}\u{43a}\u{43e}\u{43d}\u{43d}\u{438}\u{43a}" | "sill" => "sill",
        "\u{434}\u{43b}\u{438}\u{43d}\u{430}" | "length" | "len" => "length",
        "\u{442}\u{43e}\u{43b}\u{449}\u{438}\u{43d}\u{430}" | "thickness" => "thickness",
        "\u{43f}\u{43b}\u{43e}\u{449}\u{430}\u{434}\u{44c}" | "area" => "area",
        "\u{441}\u{43c}\u{435}\u{449}\u{435}\u{43d}\u{438}\u{435}" | "\u{43e}\u{442}\u{43c}\u{435}\u{442}\u{43a}\u{430}" | "offset" => "offset",
        "\u{43e}\u{431}\u{44a}\u{451}\u{43c}" | "\u{43e}\u{431}\u{44a}\u{435}\u{43c}" | "volume" => "volume",
        _ => return l,
    };
    c.into()
}

/// Length fields: values in project units (mm / feet-inches).
const LENGTHS: [&str; 6] = ["width", "height", "sill", "length", "thickness", "offset"];

/// `L2` / Cyrillic `U2` / Cyrillic `E2` level prefixes: level 2.
fn level_short(w: &str) -> Option<String> {
    let mut ch = w.chars();
    let head = ch.next()?;
    let rest: String = ch.collect();
    (matches!(head, 'l' | 'L' | '\u{443}' | '\u{423}' | '\u{44d}' | '\u{42d}')
        && !rest.is_empty()
        && rest.chars().all(|c| c.is_ascii_digit()))
    .then_some(rest)
}

struct P {
    t: Vec<Tok>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn word_is(&self, set: &[&str]) -> bool {
        matches!(self.peek(), Some(Tok::Word(w)) if set.contains(&lower(w).as_str()))
    }
    /// Value after an operator: a word with continuation (while the next word is not a new
    /// "field<op>", not a conjunction and not a category) or a quoted string.
    fn value(&mut self) -> String {
        match self.peek().cloned() {
            Some(Tok::Quoted(q)) => {
                self.i += 1;
                q
            }
            Some(Tok::Word(w)) => {
                self.i += 1;
                let mut v = w;
                while let Some(Tok::Word(x)) = self.peek() {
                    let next_op = matches!(self.t.get(self.i + 1), Some(Tok::Op(_)));
                    if next_op || is_kw(x) || cat_word(x).is_some() || level_short(x).is_some() {
                        break;
                    }
                    v.push(' ');
                    v.push_str(x);
                    self.i += 1;
                }
                v
            }
            _ => String::new(),
        }
    }
    fn or(&mut self) -> Result<Expr, QErr> {
        let mut alts = vec![self.and()?];
        while self.word_is(&OR) {
            self.i += 1;
            alts.push(self.and()?);
        }
        Ok(if alts.len() == 1 {
            alts.pop().unwrap()
        } else {
            Expr::Or(alts)
        })
    }
    fn and(&mut self) -> Result<Expr, QErr> {
        let mut v = vec![];
        loop {
            match self.peek() {
                None | Some(Tok::Close) | Some(Tok::Arrow) => break,
                _ if self.word_is(&OR) => break,
                _ if self.word_is(&AND) => self.i += 1,
                _ => v.push(self.unary()?),
            }
        }
        Ok(if v.len() == 1 {
            v.pop().unwrap()
        } else {
            Expr::And(v)
        })
    }
    fn unary(&mut self) -> Result<Expr, QErr> {
        if self.word_is(&NOT) {
            self.i += 1;
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        match self.peek().cloned() {
            Some(Tok::Open) => {
                self.i += 1;
                let e = self.or()?;
                if self.peek() != Some(&Tok::Close) {
                    return Err(QErr::new("query.err.unclosed_paren"));
                }
                self.i += 1;
                Ok(e)
            }
            Some(Tok::Word(w)) => {
                self.i += 1;
                if let Some(Tok::Op(op)) = self.peek().cloned() {
                    self.i += 1;
                    let val = self.value();
                    return Ok(Expr::Cmp { key: w, op, val });
                }
                if let Some(c) = cat_word(&w) {
                    return Ok(Expr::Cat(c));
                }
                if let Some(n) = level_short(&w) {
                    return Ok(Expr::Cmp {
                        key: "level".into(),
                        op: Op::Eq,
                        val: n,
                    });
                }
                Ok(Expr::Text(w))
            }
            Some(Tok::Quoted(q)) => {
                self.i += 1;
                Ok(Expr::Text(q))
            }
            Some(Tok::Op(_)) => Err(QErr::new("query.err.op_no_field")),
            Some(Tok::Close) => Err(QErr::new("query.err.extra_paren")),
            Some(Tok::Arrow) | None => Err(QErr::new("query.err.empty_cond")),
        }
    }
    fn assigns(&mut self) -> Result<Vec<Assign>, QErr> {
        let mut out = vec![];
        while let Some(t) = self.peek().cloned() {
            let Tok::Word(k) = t else {
                return Err(QErr::new("query.err.expect_assign"));
            };
            self.i += 1;
            if self.peek() != Some(&Tok::Op(Op::Eq)) {
                return Err(QErr::new("query.err.need_eq").with("k", k));
            }
            self.i += 1;
            let raw = self.value();
            let key = canon(&k);
            let val = rel(&key, &raw).unwrap_or(Val::Lit(raw));
            out.push(Assign { key, val });
        }
        if out.is_empty() {
            return Err(QErr::new("query.err.no_assign"));
        }
        Ok(out)
    }
}

/// "width+100" (a Russian field name works the same) with field `width` → Rel('+', "100").
pub(crate) fn rel(key: &str, raw: &str) -> Option<Val> {
    let pos = raw
        .char_indices()
        .skip(1)
        .find(|(_, c)| "+-*/".contains(*c))?;
    let (f, rest) = raw.split_at(pos.0);
    if canon(f) != key {
        return None;
    }
    let n = rest[1..].trim();
    (!n.is_empty()).then(|| Val::Rel(pos.1, n.to_string()))
}

pub fn parse(s: &str) -> Result<Query, QErr> {
    let mut p = P { t: lex(s)?, i: 0 };
    let expr = if matches!(p.peek(), Some(Tok::Arrow)) {
        Expr::And(vec![])
    } else {
        p.or()?
    };
    let set = match p.peek() {
        Some(Tok::Arrow) => {
            p.i += 1;
            p.assigns()?
        }
        Some(Tok::Close) => return Err(QErr::new("query.err.extra_paren")),
        _ => vec![],
    };
    Ok(Query { expr, set })
}

/// Has a category or a comparison (not just words).
pub fn structured(e: &Expr) -> bool {
    match e {
        Expr::Cat(_) | Expr::Cmp { .. } => true,
        Expr::Text(_) => false,
        Expr::Not(x) => structured(x),
        Expr::And(v) | Expr::Or(v) => v.iter().any(structured),
    }
}

/// Element row for a query (all project levels).
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub sel: Sel,
    pub cat: &'static str,
    pub level: i64,
    pub ty: String,
    pub name: String,
    pub mark: String,
    pub nums: Vec<(&'static str, f64)>,
    /// Element parameters (import): lowercase key, value as text.
    pub props: Vec<(String, String)>,
}

impl Row {
    pub fn id(&self) -> i64 {
        match self.sel {
            Sel::Wall(i)
            | Sel::Opening(i)
            | Sel::Curtain(i)
            | Sel::Panel(i, _)
            | Sel::Slab(i)
            | Sel::Room(i)
            | Sel::Mass(i)
            | Sel::Mep(i)
            | Sel::Comp(i)
            | Sel::Body(_, i)
            | Sel::Anno(i)
            | Sel::Ext(_, i) => i,
        }
    }
    fn prop(&self, k: &str) -> Option<&str> {
        self.props.iter().find(|p| p.0 == k).map(|p| p.1.as_str())
    }
    fn num(&self, k: &str) -> Option<f64> {
        if k == "id" {
            return Some(self.id() as f64);
        }
        self.nums
            .iter()
            .find(|n| n.0 == k)
            .map(|n| n.1)
            // stored text: invariant form or unambiguous legacy comma, never the UI mark
            .or_else(|| ddc_model::parse_stored(self.prop(k)?))
    }
    fn text(&self, k: &str) -> Option<String> {
        match k {
            "type" => Some(self.ty.clone()),
            "name" => Some(self.name.clone()),
            "mark" if !self.mark.is_empty() => Some(self.mark.clone()),
            "mark" => self.prop("mark").or(self.prop("\u{43c}\u{430}\u{440}\u{43a}\u{430}")).map(str::to_string),
            "id" => Some(self.id().to_string()),
            _ => self.prop(k).map(str::to_string),
        }
    }
}

/// Levels by elevation and units for comparison.
pub struct Env {
    pub levels: Vec<(i64, String)>,
    /// Level elevations (m), in the same order.
    pub elev: Vec<f64>,
    pub units: Units,
    /// How numbers in the query text are read.
    pub num: NumIn,
}

/// Source of the query text, which decides how its numbers are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumIn {
    /// Typed now: read in the UI decimal mark, passed explicitly.
    Typed(DecimalMark),
    /// Saved filter text: the invariant form, or a legacy comma by the stored-number rule.
    Stored,
    /// AI tool argument (machine text): point decimal; a comma only groups in an unambiguous
    /// run (`1,000.5`, `1,000,000`); a lone comma (`1,5`, `1,000`) is refused.
    Tool,
}

impl Env {
    /// Typed query in the current UI decimal mark.
    pub fn of(ed: &Editor) -> Env {
        Env::typed(ed, ui_decimal_mark())
    }
    pub fn typed(ed: &Editor, mark: DecimalMark) -> Env {
        Env::with(ed, NumIn::Typed(mark))
    }
    /// Saved filter text: the result does not depend on the UI language.
    pub fn saved(ed: &Editor) -> Env {
        Env::with(ed, NumIn::Stored)
    }
    /// AI tool filter: the same text reads the same in every UI language.
    pub fn tool(ed: &Editor) -> Env {
        Env::with(ed, NumIn::Tool)
    }
    fn with(ed: &Editor, num: NumIn) -> Env {
        let l = ed.levels();
        Env {
            elev: l.iter().map(|l| l.2).collect(),
            levels: l.into_iter().map(|l| (l.0, l.1)).collect(),
            units: ed.tool.units,
            num,
        }
    }
    fn ord(&self, id: i64) -> Option<usize> {
        self.levels.iter().position(|l| l.0 == id).map(|i| i + 1)
    }
    pub fn level_name(&self, id: i64) -> String {
        self.levels
            .iter()
            .find(|l| l.0 == id)
            .map_or_else(String::new, |l| l.1.clone())
    }
    /// Levels by value: the full name; the ordinal by elevation order; an elevation (`+3.000`, `3000`).
    fn resolve(&self, v: &str) -> Vec<(i64, usize)> {
        let v = lower(v.trim());
        let named: Vec<(i64, usize)> = self
            .levels
            .iter()
            .enumerate()
            .filter(|(_, l)| lower(l.1.trim()) == v)
            .map(|(i, l)| (l.0, i + 1))
            .collect();
        if !named.is_empty() {
            return named;
        }
        if let Ok(n) = v.parse::<usize>() {
            if n >= 1 && n <= self.levels.len() {
                return vec![(self.levels[n - 1].0, n)];
            }
        }
        let body = v.trim_start_matches(['+', '-']);
        let decimal = body.contains(['.', ','])
            && body
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.' || c == ',');
        // a metric elevation with a fraction is meters (`+3.000`), otherwise project units
        let v = v.trim_start_matches('+');
        let z = if decimal && self.units == Units::Metric {
            match self.num {
                NumIn::Typed(m) => parse_locale_number(v, m).ok(),
                NumIn::Stored => stored_text(v).and_then(|s| parse_locale_number(&s, DecimalMark::Point).ok()),
                NumIn::Tool => tool_text(v).and_then(|s| parse_locale_number(&s, DecimalMark::Point).ok()),
            }
        } else {
            qnum_in("offset", v, self.units, self.num)
        };
        let Some(z) = z else {
            return vec![];
        };
        self.elev
            .iter()
            .enumerate()
            .filter(|(_, e)| (**e - z).abs() < 5e-4)
            .map(|(i, _)| (self.levels[i].0, i + 1))
            .collect()
    }
}

/// Number from query text in field units: lengths as in property fields (a bare number is mm
/// or inches), signed (`-0,1 m` with a decimal comma, `-6"`); 0 is allowed.
#[cfg(test)]
fn qnum(key: &str, v: &str, u: Units) -> Option<f64> {
    qnum_mark(key, v, u, ui_decimal_mark())
}

/// [`qnum`] with the decimal mark given; an ambiguous `1,5` in the point locale is refused.
fn qnum_mark(key: &str, v: &str, u: Units, mark: DecimalMark) -> Option<f64> {
    let t = v.trim();
    if !LENGTHS.contains(&key) {
        return parse_locale_number(t, mark).ok();
    }
    let (neg, body) = match t.strip_prefix('-') {
        Some(r) => (true, r.trim_start()),
        None => (false, t.strip_prefix('+').unwrap_or(t).trim_start()),
    };
    let x = parse_edit_length_lang(body, u, &Scope::none(), mark).ok().or_else(|| {
        let bare = body
            .trim_end_matches("\u{43c}\u{43c}")
            .trim_end_matches("mm")
            .trim();
        let n = parse_locale_number(bare, mark).ok()?;
        Some(match u {
            Units::Metric => n / 1000.0,
            Units::Imperial => n * 0.0254,
        })
    })?;
    Some(if neg { -x } else { x })
}

/// [`qnum`] for the text source of `src`.
fn qnum_in(key: &str, v: &str, u: Units, src: NumIn) -> Option<f64> {
    match src {
        NumIn::Typed(m) => qnum_mark(key, v, u, m),
        NumIn::Stored => qnum_mark(key, &stored_text(v)?, u, DecimalMark::Point),
        NumIn::Tool => qnum_mark(key, &tool_text(v)?, u, DecimalMark::Point),
    }
}

/// Plain number (factor, count) for the text source of `src`.
fn plain_num_in(v: &str, src: NumIn) -> Option<f64> {
    let t = match src {
        NumIn::Typed(m) => return parse_locale_number(v.trim(), m).ok(),
        NumIn::Stored => stored_text(v)?,
        NumIn::Tool => tool_text(v)?,
    };
    parse_locale_number(t.trim(), DecimalMark::Point).ok()
}

/// One machine-written number run in the point form; a lone comma is ambiguous (None).
fn tool_run(run: &str) -> Option<String> {
    match run.matches(',').count() {
        0 => Some(run.to_string()),
        1 if !run.contains('.') => None,
        _ => ddc_ui::numparse::canonical(run, DecimalMark::Point).ok(),
    }
}

/// AI tool value with every number in the point form; None when a number is ambiguous.
pub fn tool_text(v: &str) -> Option<String> {
    map_num_runs(v, false, |r, _, _, _| tool_run(r).ok_or(())).ok()
}

/// AI tool filter or value text: an ambiguous number (`1,5`, `1,000`) refuses the call with
/// an error the model reads; quoted text is not checked.
pub fn check_tool_numbers(q: &str) -> Result<(), String> {
    map_num_runs(q, true, |r, _, _, _| {
        tool_run(r).ok_or_else(|| {
            format!(
                "number {r} is ambiguous: write numbers with a point as the decimal mark and no \
                 comma groups, e.g. 1.5 or 1000"
            )
        })
    })
    .map(|_| ())
}

/// AI tool filter: checked numbers, read the same in every UI language.
pub fn eval_tool(ed: &Editor, text: &str) -> Result<Vec<Sel>, String> {
    check_tool_numbers(text)?;
    Ok(eval_env(ed, &parse(text)?, &Env::tool(ed)))
}

/// Rewrites each number run of `s` (digits with inner `,`/`.`, not glued to a word) by `f`.
/// With `quotes`, quoted query text (`"..."`, guillemets) is kept as typed, as the lexer sees it.
/// `f` gets the run and its char span `[k, e)` in `cs`.
fn map_num_runs<E>(
    s: &str,
    quotes: bool,
    mut f: impl FnMut(&str, &[char], usize, usize) -> Result<String, E>,
) -> Result<String, E> {
    let cs: Vec<char> = s.chars().collect();
    let digit_at = |j: usize| cs.get(j).is_some_and(|c| c.is_ascii_digit());
    let mut out = String::with_capacity(s.len());
    let mut k = 0;
    while k < cs.len() {
        let c = cs[k];
        let prev = k.checked_sub(1).map(|j| cs[j]);
        let inch = prev.is_some_and(|p| p.is_ascii_digit() || p == '\'');
        if quotes && (c == '\u{ab}' || (c == '"' && !inch)) {
            let end = if c == '"' { '"' } else { '\u{bb}' };
            let j = cs[k + 1..].iter().position(|&x| x == end).map_or(cs.len(), |j| k + 2 + j);
            out.extend(&cs[k..j]);
            k = j;
            continue;
        }
        let in_word = prev.is_some_and(|p| p.is_alphanumeric() || p == '_');
        if in_word || !(c.is_ascii_digit() || (matches!(c, '.' | ',') && digit_at(k + 1))) {
            out.push(c);
            k += 1;
            continue;
        }
        let mut e = k;
        while e < cs.len() && (cs[e].is_ascii_digit() || (matches!(cs[e], ',' | '.') && digit_at(e + 1))) {
            e += 1;
        }
        let run: String = cs[k..e].iter().collect();
        out.push_str(&f(&run, &cs, k, e)?);
        k = e;
    }
    Ok(out)
}

/// One stored number run in the point form: one comma alone follows the stored-number rule
/// (legacy decimal, or ambiguous `1,000` = None); both marks or several commas are unambiguous.
fn stored_run(run: &str) -> Option<String> {
    match run.matches(',').count() {
        0 => Some(run.to_string()),
        1 if !run.contains('.') => ddc_model::parse_stored(run).map(|_| run.replace(',', ".")),
        _ => ddc_ui::numparse::canonical(run, DecimalMark::Point).ok(),
    }
}

/// Saved query value with every number in the point form; None when a number is ambiguous.
fn stored_text(v: &str) -> Option<String> {
    map_num_runs(v, false, |r, _, _, _| stored_run(r).ok_or(())).ok()
}

/// Single-word comparison values the query may read as numbers (not level or category names).
fn num_cmp_vals(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Cmp { key, val, .. } => {
            let (k, v) = (canon(key), val.trim());
            if k != "level" && k != "category" && !v.is_empty() && !v.contains(char::is_whitespace) {
                out.insert(v.to_string());
            }
        }
        Expr::And(v) | Expr::Or(v) => v.iter().for_each(|x| num_cmp_vals(x, out)),
        Expr::Not(x) => num_cmp_vals(x, out),
        Expr::Cat(_) | Expr::Text(_) => {}
    }
}

/// The comparison value word around the run `[k, e)` of `cs` when an operator stands right
/// before that word (spaces allowed): `width>900,5mm` gives `900,5mm`; free words give None.
fn cmp_value_at(cs: &[char], k: usize, e: usize) -> Option<String> {
    let op = |c: char| matches!(c, '=' | '<' | '>' | '!' | '~' | ':' | '\u{2265}' | '\u{2264}' | '\u{2260}');
    let stop = |c: char| c.is_whitespace() || op(c) || matches!(c, '(' | ')' | '|' | '&' | ';');
    let a = cs[..k].iter().rposition(|&c| stop(c)).map_or(0, |i| i + 1);
    let b = cs[e..].iter().position(|&c| stop(c)).map_or(cs.len(), |i| e + i);
    let before = cs[..a].iter().rev().find(|c| !c.is_whitespace())?;
    op(*before).then(|| cs[a..b].iter().collect())
}

/// Query text typed in the locale of `mark`, as it is saved: valid query, numbers of
/// single-word comparison values in the invariant form (`1,000` in the point locale = `1000`,
/// `2,7` in the comma locale = `2.7`). Text is never rewritten: quoted strings, level names,
/// free words and multi-word values (`level=Floor 1,5`) stay as typed. A comparison number the
/// locale refuses (`1,5` in the point locale) refuses the whole text.
pub fn canon_query(q: &str, mark: DecimalMark) -> Result<String, QErr> {
    let q = q.trim().replace(['\t', '\n'], " ");
    let mut vals = HashSet::new();
    num_cmp_vals(&parse(&q)?.expr, &mut vals);
    let out = map_num_runs(&q, true, |r, cs, k, e| {
        if !cmp_value_at(cs, k, e).is_some_and(|w| vals.contains(&w)) {
            return Ok(r.to_string());
        }
        match ddc_ui::numparse::canonical(r, mark) {
            Ok(t) => Ok(t),
            Err(ddc_ui::numparse::NumError::CommaDecimal) => Err(QErr::new("query.err.nan").with("s", r)),
            // not a number on its own (`1.2.3` in a mark): text, kept as typed
            Err(_) => Ok(r.to_string()),
        }
    })?;
    parse(&out)?;
    Ok(out)
}

/// Partial number text for a `~` substring search: the comma becomes a point only where the
/// comma is the decimal mark.
fn point_text(s: &str, mark: DecimalMark) -> String {
    match mark {
        DecimalMark::Comma => s.replace(',', "."),
        DecimalMark::Point => s.to_string(),
    }
}

/// Number as in property fields (lengths in mm or feet-inches), for `~`.
fn num_text(key: &str, x: f64, u: Units) -> String {
    if !LENGTHS.contains(&key) {
        return format!("{x}");
    }
    match u {
        Units::Metric => format!("{}", (x * 10_000.0).round() / 10.0),
        u => lower(&edit_length(x, u)),
    }
}

fn glob(t: &str, v: &str) -> bool {
    match (v.strip_prefix('*'), v.strip_suffix('*')) {
        (Some(a), _) if a.ends_with('*') && a.len() > 1 => t.contains(&a[..a.len() - 1]),
        (Some(a), _) => t.ends_with(a),
        (_, Some(b)) => t.starts_with(b),
        _ => t == v,
    }
}

fn cat_is(r: &Row, c: &str) -> bool {
    r.cat == c || (c == "opening" && (r.cat == "door" || r.cat == "window"))
}

/// Condition prepared once per query (keys, values, levels, numbers).
enum Pred {
    Cat(&'static str),
    Level {
        op: Op,
        hits: Vec<(i64, usize)>,
        has: String,
    },
    Category {
        ne: bool,
        c: Option<&'static str>,
    },
    Empty {
        key: String,
        op: Op,
    },
    Field {
        key: String,
        op: Op,
        y: Option<f64>,
        lv: String,
        norm: String,
    },
    Id(String),
    Text(String),
    And(Vec<Pred>),
    Or(Vec<Pred>),
    Not(Box<Pred>),
}

/// Compiled query: `Filter::new(expr, env).test(row)`.
pub struct Filter<'a> {
    p: Pred,
    env: &'a Env,
}

fn prep(e: &Expr, env: &Env) -> Pred {
    match e {
        Expr::Cat(c) => Pred::Cat(*c),
        Expr::Text(t) => {
            let t = lower(t);
            match t.strip_prefix('#') {
                Some(id) => Pred::Id(id.to_string()),
                None => Pred::Text(t),
            }
        }
        Expr::And(v) => Pred::And(v.iter().map(|x| prep(x, env)).collect()),
        Expr::Or(v) => Pred::Or(v.iter().map(|x| prep(x, env)).collect()),
        Expr::Not(x) => Pred::Not(Box::new(prep(x, env))),
        Expr::Cmp { key, op, val } => {
            let (k, v) = (canon(key), val.trim());
            let lv = lower(v);
            if k == "level" {
                return Pred::Level {
                    op: *op,
                    hits: env.resolve(v),
                    has: lv,
                };
            }
            if k == "category" {
                return Pred::Category {
                    ne: *op == Op::Ne,
                    c: cat_word(v),
                };
            }
            if v.is_empty() || ["\u{43f}\u{443}\u{441}\u{442}\u{43e}", "\u{43d}\u{435}\u{442}", "empty", "none"].contains(&lv.as_str()) {
                return Pred::Empty { key: k, op: *op };
            }
            let y = qnum_in(&k, v, env.units, env.num);
            let norm = y.map_or_else(
                || match env.num {
                    NumIn::Typed(m) => point_text(&lv, m),
                    NumIn::Stored => stored_text(&lv).unwrap_or_else(|| lv.clone()),
                    NumIn::Tool => tool_text(&lv).unwrap_or_else(|| lv.clone()),
                },
                |y| num_text(&k, y, env.units),
            );
            Pred::Field {
                key: k,
                op: *op,
                y,
                lv,
                norm,
            }
        }
    }
}

impl<'a> Filter<'a> {
    pub fn new(e: &Expr, env: &'a Env) -> Filter<'a> {
        Filter {
            p: prep(e, env),
            env,
        }
    }
    pub fn test(&self, r: &Row) -> bool {
        self.eval(&self.p, r)
    }
    fn eval(&self, p: &Pred, r: &Row) -> bool {
        let env = self.env;
        match p {
            Pred::Cat(c) => cat_is(r, c),
            Pred::Id(id) => *id == r.id().to_string(),
            Pred::Text(t) => {
                [&r.ty, &r.name, &r.mark]
                    .iter()
                    .any(|f| lower(f).contains(t.as_str()))
                    || builtin_type_names(r.cat).iter().any(|a| lower(a).contains(t.as_str()))
                    || r.props.iter().any(|p| lower(&p.1) == *t)
            }
            Pred::And(v) => v.iter().all(|x| self.eval(x, r)),
            Pred::Or(v) => v.iter().any(|x| self.eval(x, r)),
            Pred::Not(x) => !self.eval(x, r),
            Pred::Level { op, hits, has } => match op {
                Op::Eq => hits.iter().any(|h| h.0 == r.level),
                Op::Ne => !hits.iter().any(|h| h.0 == r.level),
                Op::Has => lower(&env.level_name(r.level)).contains(has.as_str()),
                _ => {
                    let (Some(a), Some(b)) = (env.ord(r.level), hits.first().map(|h| h.1)) else {
                        return false;
                    };
                    match op {
                        Op::Lt => a < b,
                        Op::Le => a <= b,
                        Op::Gt => a > b,
                        _ => a >= b,
                    }
                }
            },
            Pred::Category { ne, c } => {
                let hit = c.is_some_and(|c| cat_is(r, c));
                hit != *ne
            }
            Pred::Empty { key, op } => {
                let has = r.num(key).is_some() || r.text(key).is_some_and(|s| !s.trim().is_empty());
                match op {
                    Op::Eq => !has,
                    Op::Ne => has,
                    _ => false,
                }
            }
            Pred::Field {
                key,
                op,
                y,
                lv,
                norm,
            } => {
                if let (Some(x), Some(y)) = (r.num(key), *y) {
                    let e = 1e-6;
                    return match op {
                        Op::Eq => (x - y).abs() < e,
                        Op::Ne => (x - y).abs() >= e,
                        Op::Lt => x < y - e,
                        Op::Le => x <= y + e,
                        Op::Gt => x > y + e,
                        Op::Ge => x >= y - e,
                        Op::Has => num_text(key, x, env.units).contains(norm.as_str()),
                    };
                }
                if *op == Op::Has {
                    if let Some(x) = r.num(key) {
                        return num_text(key, x, env.units).contains(norm.as_str());
                    }
                }
                let Some(t) = r.text(key).map(|t| lower(&t)) else {
                    return *op == Op::Ne;
                };
                // built-in type labels match in every UI language (matching is data, display is UI)
                let alt: &[String] = if key == "type" { builtin_type_names(r.cat) } else { &[] };
                let any = |f: &dyn Fn(&str) -> bool| f(&t) || alt.iter().any(|a| f(&lower(a)));
                match op {
                    Op::Eq => any(&|t: &str| glob(t, lv)),
                    Op::Ne => !any(&|t: &str| glob(t, lv)),
                    Op::Has => any(&|t: &str| t.contains(lv.as_str())),
                    _ => false,
                }
            }
        }
    }
}

/// Labels of the rows whose type is a built-in name shown in the UI language (curtain walls,
/// rooms), in every language: a saved or AI filter `type="..."` keeps matching after a switch.
fn builtin_type_names(cat: &str) -> &'static [String] {
    static N: std::sync::OnceLock<[Vec<String>; 2]> = std::sync::OnceLock::new();
    let n = N.get_or_init(|| {
        ["unit_fmt.q.name.curtain", "unit_fmt.q.name.room"]
            .map(|k| vec![crate::tr_lang(true, k, &[]), crate::tr_lang(false, k, &[])])
    });
    match cat {
        "curtain" => &n[0],
        "room" => &n[1],
        _ => &[],
    }
}

pub fn matches(e: &Expr, r: &Row, env: &Env) -> bool {
    Filter::new(e, env).test(r)
}

fn query_rows<T>(
    ed: &Editor,
    sql: &str,
    f: impl FnMut(&rusqlite::Row) -> rusqlite::Result<T>,
) -> Vec<T> {
    let Ok(mut st) = ed.model.sql().prepare(sql) else {
        return vec![];
    };
    st.query_map([], f)
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

/// Connection, its changes, editor revision, units, last element and operation.
type RowsKey = (usize, u64, u64, bool, i64, i64);

thread_local! {
    static ROWS: RefCell<Option<(RowsKey, Rc<Vec<Row>>)>> = const { RefCell::new(None) };
}

fn rows_key(ed: &Editor) -> RowsKey {
    let c = ed.model.sql();
    let (e, o) = c
        .query_row(
            "SELECT coalesce((SELECT max(id) FROM element), 0),
                    coalesce((SELECT max(seq) FROM op), 0)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((-1, -1));
    (
        c as *const rusqlite::Connection as usize,
        c.total_changes(),
        ed.revision,
        ed.tool.units == Units::Metric,
        e,
        o,
    )
}

/// All selectable project elements (all levels); cached until the next edit/undo.
pub fn rows(ed: &Editor) -> Rc<Vec<Row>> {
    let key = rows_key(ed);
    if let Some(r) = ROWS.with(|c| {
        c.borrow()
            .as_ref()
            .filter(|x| x.0 == key)
            .map(|x| x.1.clone())
    }) {
        return r;
    }
    let r = Rc::new(load_rows(ed));
    ROWS.with(|c| *c.borrow_mut() = Some((key, r.clone())));
    r
}

/// Walls, doors, windows, curtain walls, slabs, rooms; with import parameters (`prop`).
fn load_rows(ed: &Editor) -> Vec<Row> {
    let mut out: Vec<Row> = vec![];
    let row = |sel, cat, level, ty: String, name: String, nums| Row {
        sel,
        cat,
        level,
        ty,
        name,
        mark: String::new(),
        nums,
        props: vec![],
    };
    out.extend(query_rows(
        ed,
        "SELECT w.id, w.level, t.name, w.height, w.ax, w.ay, w.bx, w.by,
            (SELECT coalesce(sum(thickness), 0) FROM wall_type_layer WHERE wtype=w.wtype)
         FROM wall w JOIN wall_type t ON t.id=w.wtype ORDER BY w.id",
        |r| {
            let (ax, ay, bx, by): (f64, f64, f64, f64) =
                (r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?);
            let th: f64 = r.get(8)?;
            Ok(row(
                Sel::Wall(r.get(0)?),
                "wall",
                r.get(1)?,
                r.get(2)?,
                String::new(),
                vec![
                    ("height", r.get(3)?),
                    ("length", (bx - ax).hypot(by - ay)),
                    ("thickness", th),
                    ("width", th),
                ],
            ))
        },
    ));
    out.extend(query_rows(
        ed,
        "SELECT o.id, w.level, o.kind, o.width, o.height, o.sill
         FROM opening o JOIN wall w ON w.id=o.wall ORDER BY o.id",
        |r| {
            let kind: String = r.get(2)?;
            let (w, h): (f64, f64) = (r.get(3)?, r.get(4)?);
            let cat = match kind.as_str() {
                "door" => "door",
                "void" => "opening",
                _ => "window",
            };
            let dim = |v: f64| match ed.tool.units {
                Units::Metric => format!("{:.0}", v * 1000.0),
                u => edit_length(v, u),
            };
            let ty = format!("{}×{}", dim(w), dim(h));
            Ok(row(
                Sel::Opening(r.get(0)?),
                cat,
                r.get(1)?,
                ty,
                String::new(),
                vec![("width", w), ("height", h), ("sill", r.get(5)?)],
            ))
        },
    ));
    out.extend(query_rows(
        ed,
        "SELECT id, level, name, height, ax, ay, bx, by FROM curtain_wall ORDER BY id",
        |r| {
            let (ax, ay, bx, by): (f64, f64, f64, f64) =
                (r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?);
            Ok(row(
                Sel::Curtain(r.get(0)?),
                "curtain",
                r.get(1)?,
                crate::tr("unit_fmt.q.name.curtain", &[]),
                r.get(2)?,
                vec![("height", r.get(3)?), ("length", (bx - ax).hypot(by - ay))],
            ))
        },
    ));
    out.extend(query_rows(
        ed,
        "SELECT s.id, s.level, t.name, s.offset,
            (SELECT coalesce(sum(thickness), 0) FROM slab_type_layer WHERE stype=s.stype)
         FROM slab s JOIN slab_type t ON t.id=s.stype ORDER BY s.id",
        |r| {
            Ok(row(
                Sel::Slab(r.get(0)?),
                "floor",
                r.get(1)?,
                r.get(2)?,
                String::new(),
                vec![("offset", r.get(3)?), ("thickness", r.get(4)?)],
            ))
        },
    ));
    out.extend(query_rows(
        ed,
        "SELECT r.id, r.level, r.name, r.number, r.height, q.area, q.volume
         FROM room r LEFT JOIN room_qty q ON q.room=r.id ORDER BY r.id",
        |r| {
            let mut nums = vec![("height", r.get::<_, f64>(4)?)];
            if let Some(a) = r.get::<_, Option<f64>>(5)? {
                nums.push(("area", a));
            }
            if let Some(v) = r.get::<_, Option<f64>>(6)? {
                nums.push(("volume", v));
            }
            let mut x = row(
                Sel::Room(r.get(0)?),
                "room",
                r.get(1)?,
                crate::tr("unit_fmt.q.name.room", &[]),
                r.get(2)?,
                nums,
            );
            x.mark = r.get(3)?;
            Ok(x)
        },
    ));
    let kind_of = |s: Sel| match s {
        Sel::Wall(_) => "wall",
        Sel::Opening(_) => "opening",
        Sel::Curtain(_) | Sel::Panel(..) => "curtain_wall",
        Sel::Slab(_) => "slab",
        Sel::Room(_) => "room",
        Sel::Mass(_) => "mass",
        Sel::Mep(_) => "mep_run",
        Sel::Comp(_) => "comp",
        Sel::Body(k, _) => k.table(),
        Sel::Ext(..) => crate::editor::structx::CATEGORY,
        Sel::Anno(_) => "anno",
    };
    let at: HashMap<(&'static str, i64), usize> = out
        .iter()
        .enumerate()
        .map(|(i, r)| ((kind_of(r.sel), r.id()), i))
        .collect();
    let props: Vec<(String, i64, String, String)> = query_rows(
        ed,
        "SELECT kind, ref, key, value FROM prop ORDER BY kind, ref",
        |r| {
            let v = match r.get_ref(3)? {
                rusqlite::types::ValueRef::Integer(i) => i.to_string(),
                rusqlite::types::ValueRef::Real(f) => f.to_string(),
                rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                _ => String::new(),
            };
            Ok((r.get(0)?, r.get(1)?, lower(&r.get::<_, String>(2)?), v))
        },
    );
    for (kind, id, k, v) in props {
        let kind = match kind.as_str() {
            "wall" => "wall",
            "opening" => "opening",
            "curtain_wall" => "curtain_wall",
            "slab" => "slab",
            "room" => "room",
            _ => continue,
        };
        if let Some(&i) = at.get(&(kind, id)) {
            out[i].props.push((k, v));
        }
    }
    out
}

/// Elements matching the query (order: walls, openings, curtain walls, slabs, rooms).
pub fn eval(ed: &Editor, q: &Query) -> Vec<Sel> {
    eval_env(ed, q, &Env::of(ed))
}

/// A saved filter: its numbers read the same in every UI language.
pub fn eval_saved(ed: &Editor, text: &str) -> Result<Vec<Sel>, QErr> {
    Ok(eval_env(ed, &parse(text)?, &Env::saved(ed)))
}

fn eval_env(ed: &Editor, q: &Query, env: &Env) -> Vec<Sel> {
    let f = Filter::new(&q.expr, env);
    rows(ed)
        .iter()
        .filter(|r| f.test(r))
        .map(|r| r.sel)
        .collect()
}

pub fn cat_label(c: &str, ru: bool) -> &'static str {
    match (c, ru) {
        ("wall", true) => "\u{421}\u{442}\u{435}\u{43d}\u{430}",
        ("door", true) => "\u{414}\u{432}\u{435}\u{440}\u{44c}",
        ("window", true) => "\u{41e}\u{43a}\u{43d}\u{43e}",
        ("opening", true) => "\u{41f}\u{440}\u{43e}\u{451}\u{43c}",
        ("curtain", true) => "\u{41d}\u{430}\u{432}\u{435}\u{441}\u{43d}\u{430}\u{44f} \u{441}\u{442}\u{435}\u{43d}\u{430}",
        ("floor", true) => "\u{41f}\u{435}\u{440}\u{435}\u{43a}\u{440}\u{44b}\u{442}\u{438}\u{435}",
        ("room", true) => "\u{41f}\u{43e}\u{43c}\u{435}\u{449}\u{435}\u{43d}\u{438}\u{435}",
        ("wall", _) => "Wall",
        ("door", _) => "Door",
        ("window", _) => "Window",
        ("opening", _) => "Opening",
        ("curtain", _) => "Curtain wall",
        ("floor", _) => "Floor",
        _ => "Room",
    }
}

/// A summary of found elements by category, e.g. "3 walls, 2 doors" (localized).
pub fn summary(rows: &[&Row], ru: bool) -> String {
    let mut parts = vec![];
    for c in crate::selection::CATEGORIES {
        let n = rows.iter().filter(|r| r.cat == c).count();
        if n == 0 {
            continue;
        }
        let w = if ru {
            let forms: [&str; 3] = match c {
                "wall" => ["\u{441}\u{442}\u{435}\u{43d}\u{430}", "\u{441}\u{442}\u{435}\u{43d}\u{44b}", "\u{441}\u{442}\u{435}\u{43d}"],
                "door" => ["\u{434}\u{432}\u{435}\u{440}\u{44c}", "\u{434}\u{432}\u{435}\u{440}\u{438}", "\u{434}\u{432}\u{435}\u{440}\u{435}\u{439}"],
                "window" => ["\u{43e}\u{43a}\u{43d}\u{43e}", "\u{43e}\u{43a}\u{43d}\u{430}", "\u{43e}\u{43a}\u{43e}\u{43d}"],
                "opening" => ["\u{43f}\u{440}\u{43e}\u{451}\u{43c}", "\u{43f}\u{440}\u{43e}\u{451}\u{43c}\u{430}", "\u{43f}\u{440}\u{43e}\u{451}\u{43c}\u{43e}\u{432}"],
                "curtain" => ["\u{43d}\u{430}\u{432}\u{435}\u{441}\u{43d}\u{430}\u{44f} \u{441}\u{442}\u{435}\u{43d}\u{430}", "\u{43d}\u{430}\u{432}\u{435}\u{441}\u{43d}\u{44b}\u{435} \u{441}\u{442}\u{435}\u{43d}\u{44b}", "\u{43d}\u{430}\u{432}\u{435}\u{441}\u{43d}\u{44b}\u{445} \u{441}\u{442}\u{435}\u{43d}"],
                "floor" => ["\u{43f}\u{435}\u{440}\u{435}\u{43a}\u{440}\u{44b}\u{442}\u{438}\u{435}", "\u{43f}\u{435}\u{440}\u{435}\u{43a}\u{440}\u{44b}\u{442}\u{438}\u{44f}", "\u{43f}\u{435}\u{440}\u{435}\u{43a}\u{440}\u{44b}\u{442}\u{438}\u{439}"],
                _ => ["\u{43f}\u{43e}\u{43c}\u{435}\u{449}\u{435}\u{43d}\u{438}\u{435}", "\u{43f}\u{43e}\u{43c}\u{435}\u{449}\u{435}\u{43d}\u{438}\u{44f}", "\u{43f}\u{43e}\u{43c}\u{435}\u{449}\u{435}\u{43d}\u{438}\u{439}"],
            };
            let (m10, m100) = (n % 10, n % 100);
            forms[if m10 == 1 && m100 != 11 {
                0
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                1
            } else {
                2
            }]
            .to_string()
        } else {
            let s = lower(cat_label(c, false));
            if n == 1 {
                s
            } else {
                format!("{s}s")
            }
        };
        parts.push(format!("{n} {w}"));
    }
    parts.join(", ")
}

// ---------- bulk edit ----------

fn length_or_num(key: &str, s: &str, u: Units, src: NumIn) -> Result<f64, QErr> {
    qnum_in(key, s, u, src).ok_or_else(|| QErr::new("query.err.nan").with("s", s))
}

pub(crate) fn new_num(a: &Assign, cur: f64, u: Units, src: NumIn) -> Result<f64, QErr> {
    match &a.val {
        Val::Lit(s) => length_or_num(&a.key, s, u, src),
        Val::Rel(op, s) => {
            let y = if "*/%".contains(*op) {
                plain_num_in(s, src).ok_or_else(|| QErr::new("query.err.nan").with("s", s))?
            } else {
                length_or_num(&a.key, s, u, src)?
            };
            Ok(match op {
                '+' => cur + y,
                '-' => cur - y,
                '*' => cur * y,
                // a percent of the current value: "-10" is ten percent less
                '%' => cur * (1.0 + y / 100.0),
                _ if y != 0.0 => cur / y,
                _ => return Err(QErr::new("query.err.div0")),
            })
        }
    }
}

fn lit(a: &Assign) -> Result<String, QErr> {
    match &a.val {
        Val::Lit(s) => Ok(s.clone()),
        Val::Rel(..) => Err(QErr::new("query.err.text_field").with("key", &a.key)),
    }
}

/// Type by name, case-insensitive (Cyrillic is handled in Rust: SQLite `lower()` is ASCII-only).
fn type_id(m: &ddc_model::Model, table: &str, name: &str) -> Result<i64, QErr> {
    let want = lower(name.trim());
    let types: Vec<(i64, String)> = m
        .sql()
        .prepare(&format!("SELECT id, name FROM {table} ORDER BY id"))
        .and_then(|mut st| {
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(|it| it.flatten().collect())
        })
        .unwrap_or_default();
    types
        .into_iter()
        .find(|t| lower(&t.1) == want)
        .map(|t| t.0)
        .ok_or_else(|| QErr::new("query.err.no_type").with("name", name.trim()))
}

fn positive(field: &'static str, v: f64) -> Result<f64, QErr> {
    if v > 0.0 && v.is_finite() {
        Ok(v)
    } else {
        Err(QErr::new("query.err.positive").with_key("key", field))
    }
}

/// Wall from the model: specification and arc sagitta.
pub fn wall_spec(
    m: &ddc_model::Model,
    id: i64,
) -> Result<(ddc_model::WallSpec, f64), ddc_model::Error> {
    let spec = m.sql().query_row(
        "SELECT ax,ay,bx,by,level,base_offset,height,wtype,offset0 FROM wall WHERE id=?1",
        [id],
        |r| {
            Ok(ddc_model::WallSpec {
                a: (r.get(0)?, r.get(1)?),
                b: (r.get(2)?, r.get(3)?),
                level: r.get(4)?,
                base_offset: r.get(5)?,
                height: r.get(6)?,
                wall_type: r.get(7)?,
                offset0: r.get(8)?,
            })
        },
    )?;
    Ok((spec, m.wall_bulge(id).unwrap_or(0.0)))
}

/// Slab from the model: (level, type, offset).
pub fn slab_spec(m: &ddc_model::Model, id: i64) -> Result<(i64, i64, f64), ddc_model::Error> {
    Ok(m.sql().query_row(
        "SELECT level, stype, offset FROM slab WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

/// Why a field cannot be assigned to an element of this category (known cases).
fn why_not(cat: &str, key: &str) -> Option<&'static str> {
    Some(match (cat, key) {
        ("wall", "width" | "thickness") => "query.why.wall_thickness",
        ("slab", "thickness") => "query.why.slab_thickness",
        ("opening", "type") => "query.why.opening_type",
        (_, "length" | "area" | "volume") => "query.why.computed",
        ("curtain", _) => "query.why.curtain",
        _ => return None,
    })
}

/// Model edits for assignments: elements without such fields are skipped; a field that
/// no element accepted is an error with an explanation.
pub fn assign_edits(
    ed: &Editor,
    sels: &[Sel],
    set: &[Assign],
) -> Result<Vec<ddc_model::Edit>, QErr> {
    assign_edits_in(&ed.model, ed.tool.units, NumIn::Typed(ui_decimal_mark()), sels, set)
}

/// The same without an editor: model and units only (the AI thread works on its own copy).
/// `src` says how the values' numbers are read (typed in a UI mark, or AI tool text).
pub fn assign_edits_in(
    m: &ddc_model::Model,
    u: Units,
    src: NumIn,
    sels: &[Sel],
    set: &[Assign],
) -> Result<Vec<ddc_model::Edit>, QErr> {
    use ddc_model::Edit;
    let mut out = vec![];
    let mut used = vec![false; set.len()];
    let mut why: Vec<Option<&'static str>> = vec![None; set.len()];
    let mut any = false;
    let mut note = |k: usize, cat: &str, ok: bool| {
        if ok {
            used[k] = true;
        } else if why[k].is_none() {
            why[k] = why_not(cat, &set[k].key);
        }
    };
    for s in sels {
        match *s {
            Sel::Wall(id) => {
                let Ok((mut spec, bulge)) = wall_spec(m, id) else {
                    continue;
                };
                any = true;
                let before = spec.clone();
                let lean0 = m.wall_lean(id).unwrap_or_default();
                let mut lean = lean0;
                for (k, a) in set.iter().enumerate() {
                    let ok = match a.key.as_str() {
                        "type" => {
                            spec.wall_type = type_id(m, "wall_type", &lit(a)?)?;
                            true
                        }
                        // degrees toward the wall's left side; top/base thickness ratio
                        "lean" => {
                            lean.angle = new_num(a, lean.angle.to_degrees(), u, src)?.to_radians();
                            true
                        }
                        "top_scale" => {
                            lean.top_scale = new_num(a, lean.top_scale, u, src)?;
                            true
                        }
                        "height" => {
                            spec.height = positive("query.field.height", new_num(a, spec.height, u, src)?)?;
                            true
                        }
                        "offset" => {
                            spec.base_offset = new_num(a, spec.base_offset, u, src)?;
                            true
                        }
                        _ => false,
                    };
                    note(k, "wall", ok);
                }
                if spec != before {
                    out.push(Edit::Wall { id, spec, bulge });
                }
                if lean != lean0 {
                    out.push(Edit::WallShape { id, lean: lean.angle, top_scale: lean.top_scale });
                }
            }
            Sel::Opening(id) => {
                let Ok((wall, mut spec)) = m.sql().query_row(
                    "SELECT wall, along, width, height, sill, kind FROM opening WHERE id=?1",
                    [id],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            ddc_model::OpeningSpec {
                                along: r.get(1)?,
                                width: r.get(2)?,
                                height: r.get(3)?,
                                sill: r.get(4)?,
                                kind: r.get(5)?,
                            },
                        ))
                    },
                ) else {
                    continue;
                };
                any = true;
                let before = spec.clone();
                for (k, a) in set.iter().enumerate() {
                    let ok = match a.key.as_str() {
                        "width" => {
                            spec.width = positive("query.field.width", new_num(a, spec.width, u, src)?)?;
                            true
                        }
                        "height" => {
                            spec.height = positive("query.field.height", new_num(a, spec.height, u, src)?)?;
                            true
                        }
                        "sill" => {
                            spec.sill = new_num(a, spec.sill, u, src)?;
                            true
                        }
                        _ => false,
                    };
                    note(k, "opening", ok);
                }
                if spec != before {
                    out.push(Edit::Opening { id, wall, spec });
                }
            }
            Sel::Room(id) => {
                let Ok(mut spec) = m.sql().query_row(
                    "SELECT level, name, number, px, py, height FROM room WHERE id=?1",
                    [id],
                    |r| {
                        Ok(ddc_model::RoomSpec {
                            level: r.get(0)?,
                            name: r.get(1)?,
                            number: r.get(2)?,
                            point: (r.get(3)?, r.get(4)?),
                            height: r.get(5)?,
                        })
                    },
                ) else {
                    continue;
                };
                any = true;
                let before = spec.clone();
                for (k, a) in set.iter().enumerate() {
                    let ok = match a.key.as_str() {
                        "name" => {
                            spec.name = lit(a)?;
                            true
                        }
                        "mark" => {
                            spec.number = lit(a)?;
                            true
                        }
                        "height" => {
                            spec.height = positive("query.field.height", new_num(a, spec.height, u, src)?)?;
                            true
                        }
                        _ => false,
                    };
                    note(k, "room", ok);
                }
                if spec != before {
                    out.push(Edit::Room { id, spec });
                }
            }
            Sel::Slab(id) => {
                let Ok((level, mut st, mut off)) = slab_spec(m, id) else {
                    continue;
                };
                any = true;
                let before = (st, off);
                for (k, a) in set.iter().enumerate() {
                    let ok = match a.key.as_str() {
                        "type" => {
                            st = type_id(m, "slab_type", &lit(a)?)?;
                            true
                        }
                        "offset" => {
                            off = new_num(a, off, u, src)?;
                            true
                        }
                        _ => false,
                    };
                    note(k, "slab", ok);
                }
                if (st, off) != before {
                    out.push(Edit::Slab {
                        id,
                        level,
                        slab_type: st,
                        offset: off,
                    });
                }
            }
            Sel::Curtain(_) | Sel::Panel(..) => {
                any = true;
                for k in 0..set.len() {
                    note(k, "curtain", false);
                }
            }
            // shapes, networks and components are edited in their own windows
            Sel::Mass(_)
            | Sel::Mep(_)
            | Sel::Comp(_)
            | Sel::Ext(..)
            | Sel::Body(..)
            | Sel::Anno(_) => {}
        }
    }
    if any {
        if let Some(k) = used.iter().position(|u| !u) {
            let key = &set[k].key;
            return Err(match why[k] {
                Some(w) => QErr::new("query.err.not_changed")
                    .with("key", key)
                    .with_key("why", w),
                None => QErr::new("query.err.cannot_change").with("key", key),
            });
        }
    }
    Ok(out)
}

/// Assign values to all `sels` in one operation (one undo step); returns the changed count.
pub fn assign(ed: &mut Editor, sels: &[Sel], set: &[Assign]) -> Result<usize, QErr> {
    let edits = assign_edits(ed, sels, set)?;
    let n = edits.len();
    if n == 0 {
        // these kinds have no formula fields: say so instead of reporting "no changes"
        let own_panel = |s: &Sel| {
            matches!(
                s,
                Sel::Mass(_) | Sel::Mep(_) | Sel::Comp(_) | Sel::Ext(..) | Sel::Body(..)
            )
        };
        ed.status = Some(if !sels.is_empty() && sels.iter().all(own_panel) {
            crate::tr("geom.formula_body", &[])
        } else {
            crate::tr("query.status.no_changes", &[])
        });
        return Ok(0);
    }
    // an opening size goes through the same checks as one opening edited in the panel
    let (edits, refused) = crate::multi_edit::screen_openings(ed, edits);
    let c = crate::multi_edit::apply(ed, edits, &refused, "query.status.changed")?;
    Ok(c.changed)
}

/// Query text: a condition selects; `-> field=value` assigns (an empty condition applies to the current selection).
pub fn run_text(ed: &mut Editor, text: &str) -> Result<(Vec<Sel>, usize), QErr> {
    let q = parse(text)?;
    let target = if q.expr == Expr::And(vec![]) && !q.set.is_empty() {
        ed.selection.clone()
    } else {
        eval(ed, &q)
    };
    let n = if q.set.is_empty() {
        0
    } else {
        assign(ed, &target, &q.set)?
    };
    Ok((target, n))
}

// ---------- "Select similar" ----------

/// Same category: in the view (visible ones on the current level) or across the whole project.
pub fn same_category(
    ed: &Editor,
    s: Sel,
    project: bool,
    visible: impl Fn(Sel) -> bool,
) -> Vec<Sel> {
    let rs = rows(ed);
    let Some(c) = rs.iter().find(|r| r.sel == s).map(|r| r.cat) else {
        return vec![];
    };
    rs.iter()
        .filter(|r| r.cat == c && (project || (r.level == ed.level && visible(r.sel))))
        .map(|r| r.sel)
        .collect()
}

/// Same type across the whole project (walls by type, openings by kind and size, slabs by type).
pub fn same_type_project(ed: &Editor, s: Sel) -> Vec<Sel> {
    let rs = rows(ed);
    let Some(me) = rs.iter().find(|r| r.sel == s) else {
        return vec![];
    };
    rs.iter()
        .filter(|r| r.cat == me.cat && (me.cat == "room" || r.ty == me.ty))
        .map(|r| r.sel)
        .collect()
}

fn select_cat(app: &mut EditorApp, project: bool) {
    let Some(&s) = app.ed.selection.first() else {
        app.ed.status = Some(crate::tr("query.status.none_selected", &[]));
        return;
    };
    let v = &app.plan.b.vis;
    let ed = &app.ed;
    let found = same_category(ed, s, project, |x| v.visible(ed, x));
    app.ed.status = Some(crate::tr("query.status.selected", &[("n", &found.len().to_string())]));
    app.ed.selection = found;
}

fn h_same_cat(a: &mut EditorApp) {
    select_cat(a, false);
}
fn h_same_cat_project(a: &mut EditorApp) {
    select_cat(a, true);
}

/// Query bar commands (`shell::REGISTER`).
pub fn register(app: &mut EditorApp) {
    app.register_command("mod.select_same_cat", h_same_cat);
    app.register_command("mod.select_same_cat_project", h_same_cat_project);
}

// ---------- project filters ----------

thread_local! {
    /// Read-only list of older builds: (read from disk, [(project key, query)]).
    static LEGACY: RefCell<(bool, Vec<(String, String)>)> = const { RefCell::new((false, Vec::new())) };
    /// Properties panel buffers: (selection, formula), (selection, [name, number]).
    static BUF: RefCell<(Vec<Sel>, String, [String; 2])> = RefCell::new(Default::default());
}

/// Filters kept per project; the oldest goes first past this count.
const MAX_SAVED: usize = 50;

/// Key older builds filed this project's filters under in `queries.tsv`: its legacy mark.
fn legacy_key(ed: &Editor) -> Option<String> {
    ed.model
        .legacy_filter_mark()
        .ok()
        .flatten()
        .map(|v| format!("uid:{v:x}"))
}

fn legacy_store() -> Option<std::path::PathBuf> {
    crate::gfx::data_dir().map(|d| d.join("queries.tsv"))
}

fn parse_legacy(t: &str) -> Vec<(String, String)> {
    t.lines()
        .filter_map(|l| l.split_once('\t'))
        .filter(|(k, q)| !k.is_empty() && !q.trim().is_empty())
        .map(|(k, q)| (k.to_string(), q.to_string()))
        .collect()
}

/// Read `queries.tsv` again on the next access (another instance may have run).
pub fn reload_saved() {
    LEGACY.with(|s| s.borrow_mut().0 = false);
}

#[cfg(test)]
fn seed_legacy(text: &str) {
    LEGACY.with(|s| *s.borrow_mut() = (true, parse_legacy(text)));
}

/// Filters an older build kept for this project outside the file, oldest first. Never written:
/// the first change stores the list in the project instead.
fn legacy(ed: &Editor) -> Vec<String> {
    let Some(k) = legacy_key(ed) else {
        return Vec::new();
    };
    LEGACY.with(|s| {
        let mut s = s.borrow_mut();
        if !s.0 {
            s.1 = legacy_store()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|t| parse_legacy(&t))
                .unwrap_or_default();
            s.0 = true;
        }
        let mut out: Vec<String> = Vec::new();
        for (_, q) in s.1.iter().filter(|x| x.0 == k) {
            out.retain(|x| x != q);
            out.push(q.clone());
        }
        out
    })
}

/// The project's filters, oldest first: its stored list, else what an older build kept for it.
fn stored(ed: &Editor) -> Vec<String> {
    match ed.model.saved_filters() {
        Ok(Some(v)) => v,
        Ok(None) => legacy(ed),
        Err(e) => {
            eprintln!("saved filters unreadable: {e}");
            Vec::new()
        }
    }
}

/// Saved project filters (newest first).
pub fn saved(ed: &Editor) -> Vec<String> {
    let mut v = stored(ed);
    v.reverse();
    v
}

/// Saves a filter in the project: one model operation, one undo step.
pub fn save(ed: &mut Editor, q: &str) -> Result<String, QErr> {
    save_mark(ed, q, ui_decimal_mark())
}

/// [`save`] of text typed in the locale of `mark`: stored with invariant numbers; text that
/// is not a valid query writes nothing. Saving the newest filter again writes nothing either.
pub fn save_mark(ed: &mut Editor, q: &str, mark: DecimalMark) -> Result<String, QErr> {
    let q = canon_query(q, mark)?;
    if q.is_empty() {
        return Err(QErr::new("query.err.empty_cond"));
    }
    let mut list = stored(ed);
    list.retain(|x| *x != q);
    list.push(q.clone());
    if list.len() > MAX_SAVED {
        list.drain(..list.len() - MAX_SAVED);
    }
    let op = crate::editor::OpFn(std::sync::Arc::new(move |m: &mut ddc_model::Model| {
        m.set_saved_filters(&list).map(|_| ())
    }));
    if let Err(e) = ed.run_op(op) {
        // a failed reload after the write still leaves the filter stored
        let kept = ed.model.saved_filters().ok().flatten();
        if kept.as_ref().and_then(|v| v.last()) != Some(&q) {
            return Err(QErr::new("query.err.filter_not_saved").with("e", e.to_string()));
        }
    }
    Ok(q)
}

// ---------- palette ----------

fn sel_key(s: Sel) -> String {
    match s {
        Sel::Wall(i) => format!("w{i}"),
        Sel::Opening(i) => format!("o{i}"),
        Sel::Curtain(i) | Sel::Panel(i, _) => format!("c{i}"),
        Sel::Slab(i) => format!("s{i}"),
        Sel::Room(i) => format!("r{i}"),
        Sel::Mass(i) => format!("m{i}"),
        Sel::Mep(i) => format!("p{i}"),
        Sel::Comp(i) => format!("k{i}"),
        Sel::Anno(i) => format!("a{i}"),
        Sel::Ext(c, i) => format!("x{c}.{i}"),
        Sel::Body(k, i) => format!("b{}.{i}", k.table()),
    }
}

fn sel_from(k: &str) -> Option<Sel> {
    let (h, n) = k.split_at(k.char_indices().nth(1)?.0);
    if h == "x" {
        let (c, i) = n.split_once('.')?;
        return Some(Sel::Ext(c.parse().ok()?, i.parse().ok()?));
    }
    if h == "b" {
        let (t, i) = n.split_once('.')?;
        return Some(Sel::Body(
            crate::editor::bodies::BodyKind::from_table(t)?,
            i.parse().ok()?,
        ));
    }
    let id: i64 = n.parse().ok()?;
    Some(match h {
        "w" => Sel::Wall(id),
        "o" => Sel::Opening(id),
        "c" => Sel::Curtain(id),
        "s" => Sel::Slab(id),
        "r" => Sel::Room(id),
        "m" => Sel::Mass(id),
        "p" => Sel::Mep(id),
        "k" => Sel::Comp(id),
        "a" => Sel::Anno(id),
        _ => return None,
    })
}

fn el_extra(r: &Row, env: &Env, ru: bool) -> Extra {
    let what = match r.cat {
        "room" => format!("{} {}", r.mark, r.name).trim().to_string(),
        "curtain" if !r.name.is_empty() => r.name.clone(),
        _ => r.ty.clone(),
    };
    Extra {
        key: format!("@el:{}", sel_key(r.sel)),
        label: format!(
            "{} · {} · {}",
            cat_label(r.cat, ru),
            what,
            env.level_name(r.level)
        ),
        detail: format!("#{}", r.id()),
        icon: match r.cat {
            "door" => "door",
            "window" | "opening" => "window",
            "room" => "room",
            "floor" => "floor",
            "curtain" => "curtain-wall",
            _ => "wall",
        }
        .into(),
    }
}

/// Palette rows for a query: "Select: …", "Change …", elements, views, levels, filters.
/// The second value says whether to show them above commands.
pub fn extras(ed: &Editor, q: &str, ru: bool) -> (Vec<Extra>, bool) {
    let t = q.trim();
    let tr = |k: &str| crate::tr_lang(ru, k, &[]);
    let mut v: Vec<Extra> = vec![];
    let filters = saved(ed);
    if t.is_empty() {
        for f in filters.iter().take(4) {
            v.push(Extra {
                key: format!("@sq:{f}"),
                label: format!("{}: {f}", tr("query.pal.filter")),
                detail: String::new(),
                icon: "filter".into(),
            });
        }
        return (v, false);
    }
    let env = Env::of(ed);
    let rs = rows(ed);
    let mut first = false;
    match parse(t) {
        Ok(qq) => {
            let all_sel = qq.expr == Expr::And(vec![]);
            let hits: Vec<&Row> = if all_sel {
                let cur: HashSet<Sel> = ed.selection.iter().copied().collect();
                rs.iter().filter(|r| cur.contains(&r.sel)).collect()
            } else {
                let f = Filter::new(&qq.expr, &env);
                rs.iter().filter(|r| f.test(r)).collect()
            };
            let st = structured(&qq.expr);
            first = st || !qq.set.is_empty() || t.starts_with('#');
            if !qq.set.is_empty() {
                if !hits.is_empty() {
                    let what: Vec<&str> = t.splitn(2, "->").map(str::trim).collect();
                    v.push(Extra {
                        key: format!("@set:{t}"),
                        label: format!(
                            "{} {}: {}",
                            tr("query.pal.change"),
                            summary(&hits, ru),
                            what.last().copied().unwrap_or_default()
                        ),
                        detail: tr("query.pal.one_undo"),
                        icon: "params".into(),
                    });
                }
            } else if !hits.is_empty() && (st || hits.len() > 1) {
                let lv: BTreeSet<i64> = hits.iter().map(|r| r.level).collect();
                v.push(Extra {
                    key: format!("@q:{t}"),
                    label: format!("{}: {}", tr("query.pal.select"), summary(&hits, ru)),
                    detail: format!("{} {}", lv.len(), tr("query.pal.levels_short")),
                    icon: "select-similar".into(),
                });
                let as_saved = canon_query(t, ui_decimal_mark()).ok();
                if st && as_saved.is_some() && !filters.iter().any(|f| Some(f) == as_saved.as_ref()) {
                    v.push(Extra {
                        key: format!("@save:{t}"),
                        label: tr("query.pal.save_filter"),
                        detail: String::new(),
                        icon: "filter".into(),
                    });
                }
            }
            if qq.set.is_empty() {
                let mut els: Vec<&Row> = hits.iter().take(4).copied().collect();
                if !st && els.len() < 4 {
                    // fuzzy search over distinct strings (types, names, numbers), not over rows
                    let mut at: HashMap<&str, Vec<usize>> = HashMap::new();
                    for (i, r) in rs.iter().enumerate() {
                        for f in [&r.ty, &r.name, &r.mark] {
                            if !f.is_empty() {
                                at.entry(f.as_str()).or_default().push(i);
                            }
                        }
                    }
                    let ranked = ddc_ui::fuzzy::rank(
                        t,
                        at.into_iter().map(|(k, v)| (v, vec![k.to_string()])),
                    );
                    let mut seen: HashSet<Sel> = els.iter().map(|r| r.sel).collect();
                    'top: for (_, ids) in ranked {
                        for i in ids {
                            if els.len() >= 4 {
                                break 'top;
                            }
                            if seen.insert(rs[i].sel) {
                                els.push(&rs[i]);
                            }
                        }
                    }
                }
                for r in els.into_iter().take(4) {
                    v.push(el_extra(r, &env, ru));
                }
            }
        }
        Err(e) => v.push(Extra {
            key: "@none".into(),
            label: format!("{}: {e}", tr("query.pal.query")),
            detail: String::new(),
            icon: "warning".into(),
        }),
    }
    let mut places: Vec<(String, Extra)> = env
        .levels
        .iter()
        .map(|(id, n)| {
            (
                n.clone(),
                Extra {
                    key: format!("@lv:{id}"),
                    label: format!("{}: {n}", tr("query.pal.plan")),
                    detail: tr("query.pal.level"),
                    icon: "plan-view".into(),
                },
            )
        })
        .collect();
    places.extend(ed.views.iter().map(|(id, s)| {
        (
            s.name.clone(),
            Extra {
                key: format!("@vw:{id}"),
                label: s.name.clone(),
                detail: tr("query.pal.view"),
                icon: "section".into(),
            },
        )
    }));
    places.extend(filters.iter().map(|f| {
        (
            f.clone(),
            Extra {
                key: format!("@sq:{f}"),
                label: format!("{}: {f}", tr("query.pal.filter")),
                detail: String::new(),
                icon: "filter".into(),
            },
        )
    }));
    let ranked = ddc_ui::fuzzy::rank(
        t,
        places.into_iter().map(|(n, e)| {
            let l = e.label.clone();
            (e, vec![n, l])
        }),
    );
    for (s, e) in ranked.into_iter().take(3) {
        if s >= 500 && !v.iter().any(|x| x.key == e.key) {
            v.push(e);
        }
    }
    v.truncate(8);
    (v, first)
}

/// Refresh palette rows for the typed query (every frame; recomputed when the text changes).
pub fn palette_feed(app: &mut EditorApp, ctx: &egui::Context) {
    use web_time::{Duration, Instant};
    thread_local! {
        /// (palette was open, last text, when it was typed).
        static TYPED: RefCell<(bool, String, Option<Instant>)> = const {
            RefCell::new((false, String::new(), None))
        };
    }
    const QUIET: Duration = Duration::from_millis(150);
    demo(app);
    let p = &app.sh.palette;
    let opened = TYPED.with(|t| {
        let mut t = t.borrow_mut();
        let was = std::mem::replace(&mut t.0, p.open);
        p.open && !was
    });
    if opened {
        // filters may have been saved in another window
        reload_saved();
        app.sh.palette.extras_for = None;
    }
    let p = &app.sh.palette;
    if !p.open || p.extras_for.as_deref() == Some(p.query.as_str()) {
        return;
    }
    let now = Instant::now();
    let quiet = TYPED.with(|t| {
        let mut t = t.borrow_mut();
        if t.1 != p.query {
            t.1 = p.query.clone();
            t.2 = Some(now);
        }
        t.2.map_or(QUIET, |s| now - s)
    });
    // large model: recompute after a pause in typing
    if quiet < QUIET && p.extras_for.is_some() && rows(&app.ed).len() > 20_000 {
        ctx.request_repaint_after(QUIET - quiet);
        return;
    }
    let ru = app.sh.cat.lang == ddc_ui::Lang::Ru;
    let q = p.query.clone();
    // `@wall` asks for elements: the query language reads the text after the prefix
    let (sc, rest) = ddc_ui::widgets::palette::scope(&q);
    let eq = if sc == ddc_ui::widgets::palette::Scope::Elements { rest } else { q.as_str() };
    let (mut ex, mut first) = extras(&app.ed, eq, ru);
    let (catch_all, last) = crate::ai::ui::palette_ai(app, &q).merge(&mut ex, &mut first);
    // keys of the running step on top, while nothing is typed
    let now = if q.trim().is_empty() { crate::interact::hints::palette_rows(&app.ed) } else { vec![] };
    let catch_all = catch_all.map(|i| i + now.len());
    if !now.is_empty() {
        ex.splice(0..0, now);
        first = true;
    }
    let p = &mut app.sh.palette;
    p.catch_all = catch_all;
    p.catch_all_last = last;
    p.extras = ex;
    p.extras_first = first;
    p.extras_for = Some(q);
    p.extras_head = if ru {
        "\u{42d}\u{43b}\u{435}\u{43c}\u{435}\u{43d}\u{442}\u{44b}, \u{432}\u{438}\u{434}\u{44b}, \u{444}\u{438}\u{43b}\u{44c}\u{442}\u{440}\u{44b}"
    } else {
        "Elements, views, filters"
    }
    .into();
}

/// Select the results; if they are all on one other level, switch to it.
pub(crate) fn select(app: &mut EditorApp, sels: Vec<Sel>) {
    let rs = rows(&app.ed);
    let want: HashSet<Sel> = sels.iter().copied().collect();
    let lv: BTreeSet<i64> = rs
        .iter()
        .filter(|r| want.contains(&r.sel))
        .map(|r| r.level)
        .collect();
    if lv.len() == 1 {
        let l = *lv.first().unwrap();
        if l != app.ed.level {
            app.ed.set_level(l);
        }
    }
    app.section = None;
    app.ed.status = Some(crate::tr("unit_fmt.q.selected_n", &[("n", sels.len().to_string().as_str())]));
    app.ed.selection = sels;
}

/// A palette row with an `@…` key; false means it is a command.
pub fn palette_pick(app: &mut EditorApp, key: &str) -> bool {
    let Some(k) = key.strip_prefix('@') else {
        return false;
    };
    let (kind, arg) = k.split_once(':').unwrap_or((k, ""));
    if kind == "now" {
        return true;
    }
    let id = arg.parse::<i64>().ok();
    match kind {
        "ai" => crate::ai::ui::ask(app, arg),
        "aid" => crate::ai::ui::palette_rows::draft_pick(app, arg),
        "aic" => crate::ai::ui::palette_rows::connect_pick(app),
        "q" => match parse(arg) {
            Ok(q) => {
                let s = eval(&app.ed, &q);
                select(app, s);
            }
            Err(e) => {
                app.ed.status = Some(crate::tr("query.status.query_err", &[("e", &e.to_string())]));
            }
        },
        "find" => {
            let w = arg.to_lowercase();
            let s: Vec<Sel> = rows(&app.ed)
                .iter()
                .filter(|r| r.name.to_lowercase().contains(&w))
                .map(|r| r.sel)
                .collect();
            if s.is_empty() {
                app.ed.status = Some(crate::tr("palette.not_found", &[("q", arg)]));
            } else {
                select(app, s);
            }
        }
        "help" => crate::help::search(arg),
        "el" => {
            if let Some(s) = sel_from(arg) {
                select(app, vec![s]);
            }
        }
        "lv" => {
            if let Some(id) = id {
                app.ed.set_level(id);
                app.view3d = false;
                app.section = None;
            }
        }
        "vw" => {
            if let Some(id) = id {
                app.open_view(id);
            }
        }
        "save" => match save(&mut app.ed, arg) {
            Ok(name) => app.ed.status = Some(crate::tr("query.status.filter_saved", &[("name", &name)])),
            Err(e) => app.ed.status = Some(crate::tr("query.status.query_err", &[("e", &e.to_string())])),
        },
        "sq" => match eval_saved(&app.ed, arg) {
            Ok(s) => select(app, s),
            Err(e) => {
                app.ed.status = Some(crate::tr("query.status.query_err", &[("e", &e.to_string())]));
            }
        },
        "set" => match run_text(&mut app.ed, arg) {
            Ok((s, _)) => app.ed.selection = s,
            Err(e) => {
                app.ed.status = Some(crate::tr("query.status.not_changed", &[("e", &e.to_string())]));
            }
        },
        _ => {}
    }
    true
}

// ---------- properties panel: bulk edit ----------

/// Multi-select fields: room name/number and a formula for all (`width=width+100`).
pub fn props_bulk(ui: &mut egui::Ui, ru: bool, ed: &mut Editor, cat: &str) {
    let sels: Vec<Sel> = ed.selection.clone();
    let want: HashSet<Sel> = sels.iter().copied().collect();
    let all = rows(ed);
    let rs: Vec<&Row> = all
        .iter()
        .filter(|r| want.contains(&r.sel) && cat_is(r, cat))
        .collect();
    let various = if ru { "<\u{440}\u{430}\u{437}\u{43d}\u{44b}\u{435}>" } else { "<varies>" };
    let common = |f: &dyn Fn(&Row) -> String| -> String {
        match rs.first().map(|r| f(r)) {
            Some(v) if rs.iter().all(|r| f(r) == v) => v,
            Some(_) => various.to_string(),
            None => String::new(),
        }
    };
    let (mut f, mut rb) = BUF.with(|b| {
        let b = b.borrow();
        if b.0 == sels {
            (b.1.clone(), b.2.clone())
        } else {
            (
                String::new(),
                [common(&|r| r.name.clone()), common(&|r| r.mark.clone())],
            )
        }
    });
    let tip = if ru {
        "Enter — \u{43f}\u{440}\u{438}\u{43c}\u{435}\u{43d}\u{438}\u{442}\u{44c} \u{43a}\u{43e} \u{432}\u{441}\u{435}\u{43c} \u{432}\u{44b}\u{431}\u{440}\u{430}\u{43d}\u{43d}\u{44b}\u{43c} (\u{43e}\u{434}\u{438}\u{43d} \u{448}\u{430}\u{433} \u{43e}\u{442}\u{43c}\u{435}\u{43d}\u{44b})"
    } else {
        "Enter — apply to all selected (one undo step)"
    };
    // (an unparsed assignment: the name with quotes as is) or the formula text
    let mut todo: Option<Result<Assign, String>> = None;
    let mine: Vec<Sel> = rs.iter().map(|r| r.sel).collect();
    crate::ui_kit::props(ui, "q.bulk", |ui| {
        if cat == "room" {
            for (k, (key, ru_l, en_l)) in [("name", "\u{418}\u{43c}\u{44f}", "Name"), ("mark", "\u{41d}\u{43e}\u{43c}\u{435}\u{440}", "Number")]
                .into_iter()
                .enumerate()
            {
                let l = if ru { ru_l } else { en_l };
                if let Some(v) =
                    crate::ui_kit::kv_edit(ui, l, &mut rb[k], "", tip).filter(|v| v != various)
                {
                    todo = Some(Ok(Assign {
                        key: key.into(),
                        val: Val::Lit(v.trim().to_string()),
                    }));
                }
            }
        }
        let l = if ru { "\u{424}\u{43e}\u{440}\u{43c}\u{443}\u{43b}\u{430}" } else { "Formula" };
        if let Some(v) = crate::ui_kit::kv_edit(ui, l, &mut f, "", tip) {
            let v = v.trim().trim_start_matches("->").trim().to_string();
            if !v.is_empty() {
                todo = Some(Err(format!("-> {v}")));
            }
        }
    });
    crate::ui_kit::hint(
        ui,
        if ru {
            "\u{424}\u{43e}\u{440}\u{43c}\u{443}\u{43b}\u{430} \u{434}\u{43b}\u{44f} \u{432}\u{441}\u{435}\u{445}: \u{448}\u{438}\u{440}\u{438}\u{43d}\u{430}=\u{448}\u{438}\u{440}\u{438}\u{43d}\u{430}+100"
        } else {
            "Formula for all: width=width+100"
        },
    );
    if let Some(t) = todo {
        let r = match t {
            Ok(a) => assign(ed, &mine, &[a]).map(|_| ()),
            Err(t) => {
                let keep = ed.selection.clone();
                ed.selection = mine;
                let r = run_text(ed, &t).map(|_| ());
                ed.selection = keep;
                r
            }
        };
        match r {
            Ok(_) => {
                BUF.with(|b| b.borrow_mut().0.clear());
                return;
            }
            Err(e) => {
                ed.status = Some(crate::tr("query.status.not_changed", &[("e", &e.to_string())]));
            }
        }
    }
    BUF.with(|b| *b.borrow_mut() = (sels, f, rb));
}

/// Snapshots `--query-demo Q1..Q4`: palette query, selection, bulk edit, type replacement.
fn demo(app: &mut EditorApp) {
    thread_local!(static DONE: RefCell<bool> = const { RefCell::new(false) });
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let a: Vec<String> = std::env::args().collect();
    let Some(w) = a
        .iter()
        .position(|x| x == "--query-demo")
        .and_then(|i| a.get(i + 1).cloned())
    else {
        return;
    };
    prepare_demo(app, &w);
}

/// Snapshot scene: an 8×6 room (masonry 200 and exterior 380) with doors 800/1000/1200 and a window,
/// a partition, a room; two more walls on the second level (Russian name in the fixture).
pub fn demo_scene() -> Editor {
    let mut ed = Editor::new_in_memory().expect("model");
    let (l1, ty) = (ed.level, ed.types.iter().map(|t| t.id).collect::<Vec<_>>());
    let wall = |ed: &mut Editor, a, b, t: i64, level: i64| {
        ed.model
            .add_wall(&ddc_model::WallSpec {
                a,
                b,
                level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .expect("wall")
    };
    let s = wall(&mut ed, (0.0, 0.0), (8.0, 0.0), ty[2], l1);
    wall(&mut ed, (8.0, 0.0), (8.0, 6.0), ty[2], l1);
    let n = wall(&mut ed, (8.0, 6.0), (0.0, 6.0), ty[1], l1);
    wall(&mut ed, (0.0, 6.0), (0.0, 0.0), ty[1], l1);
    let p = wall(&mut ed, (5.0, 0.0), (5.0, 6.0), ty[0], l1);
    let op = |ed: &mut Editor, w, along, width, kind: &str| {
        ed.model
            .add_opening(
                w,
                &ddc_model::OpeningSpec {
                    along,
                    width,
                    height: if kind == "door" { 2.1 } else { 1.5 },
                    sill: if kind == "door" { 0.0 } else { 0.9 },
                    kind: kind.into(),
                },
            )
            .expect("opening");
    };
    op(&mut ed, s, 1.0, 1.2, "door");
    op(&mut ed, s, 5.5, 1.5, "window");
    op(&mut ed, n, 1.5, 1.0, "door");
    op(&mut ed, p, 2.5, 0.8, "door");
    let _ = ed.model.add_room(&ddc_model::RoomSpec {
        level: l1,
        name: "\u{413}\u{43e}\u{441}\u{442}\u{438}\u{43d}\u{430}\u{44f}".into(),
        number: "101".into(),
        point: (2.0, 3.0),
        height: 2.7,
    });
    if let Ok(l2) = ed.model.add_level("\u{42d}\u{442}\u{430}\u{436} 2", 3.0) {
        wall(&mut ed, (0.0, 0.0), (8.0, 0.0), ty[1], l2);
        wall(&mut ed, (0.0, 6.0), (8.0, 6.0), ty[1], l2);
    }
    let _ = ed.reload();
    ed
}

/// Scene and state of the acceptance snapshot (shared by `--query-demo` and the snapshot test).
pub fn prepare_demo(app: &mut EditorApp, id: &str) {
    let q = match id {
        "Q1" => "\u{434}\u{432}\u{435}\u{440}\u{438} \u{448}\u{438}\u{440}\u{438}\u{43d}\u{430}>900",
        "Q2" => "\u{441}\u{442}\u{435}\u{43d}\u{44b} \u{442}\u{438}\u{43f}=\u{41a}\u{43b}\u{430}\u{434}\u{43a}\u{430} 200 \u{443}\u{440}\u{43e}\u{432}\u{435}\u{43d}\u{44c}=1",
        _ => "",
    };
    match id {
        "Q1" | "Q2" => {
            app.sh.palette.open = true;
            app.sh.palette.query = q.into();
            app.sh.palette.sel = 0;
        }
        "Q3" => {
            let l = app.ed.level;
            app.ed.selection = rows(&app.ed)
                .iter()
                .filter(|r| r.cat == "door" && r.level == l)
                .map(|r| r.sel)
                .collect();
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modify_tools::tests::walls;

    fn vocab() -> &'static serde_json::Value {
        static V: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
        V.get_or_init(|| serde_json::from_str(include_str!("../tests/data/query_ru.json")).unwrap())
    }

    /// Russian word from the data file.
    fn rv(key: &str) -> String {
        vocab()[key].as_str().unwrap().to_string()
    }

    /// Fills `{key}` placeholders of a query template with Russian words.
    fn rt(t: &str) -> String {
        let mut s = t.to_string();
        for (k, v) in vocab().as_object().unwrap() {
            s = s.replace(&format!("{{{k}}}"), v.as_str().unwrap());
        }
        s
    }

    fn cmp_(k: &str, op: Op, v: &str) -> Expr {
        Expr::Cmp {
            key: k.into(),
            op,
            val: v.into(),
        }
    }

    #[test]
    fn parses_categories_comparisons_and_or_not() {
        let q = parse(&rt("{doors} {width}>900")).unwrap();
        assert_eq!(
            q.expr,
            Expr::And(vec![Expr::Cat("door"), cmp_(&rv("width"), Op::Gt, "900")])
        );
        let q = parse(&rt("{walls} {type}={masonry} 200 {level}=2")).unwrap();
        assert_eq!(
            q.expr,
            Expr::And(vec![
                Expr::Cat("wall"),
                cmp_(&rv("type"), Op::Eq, &rt("{masonry} 200")),
                cmp_(&rv("level"), Op::Eq, "2"),
            ])
        );
        let q = parse(&rt("{windows} {or} {doors} L2")).unwrap();
        assert_eq!(
            q.expr,
            Expr::Or(vec![
                Expr::Cat("window"),
                Expr::And(vec![Expr::Cat("door"), cmp_("level", Op::Eq, "2")]),
            ])
        );
        let q = parse(&rt("{not} ({rooms} {or} {slabs}) {height}>=2,7")).unwrap();
        assert!(matches!(&q.expr, Expr::And(v) if matches!(v[0], Expr::Not(_))));
        let q = parse(&rt(r#"{mark}="{dmark} 1" {and} {fire}:{empty}"#)).unwrap();
        assert_eq!(
            q.expr,
            Expr::And(vec![
                cmp_(&rv("mark"), Op::Eq, &rt("{dmark} 1")),
                cmp_(&rv("fire"), Op::Eq, &rv("empty")),
            ])
        );
        let q = parse(&rt("{doors} {width}<>900 -> {width}={width}+100 {height}=2100")).unwrap();
        assert_eq!(
            q.set,
            vec![
                Assign {
                    key: "width".into(),
                    val: Val::Rel('+', "100".into())
                },
                Assign {
                    key: "height".into(),
                    val: Val::Lit("2100".into())
                },
            ]
        );
        assert_eq!(parse(&rt("-> {type}={masonry} 200")).unwrap().expr, Expr::And(vec![]));
        assert!(parse(&rt("({walls}")).is_err());
        assert!(parse(&rt("{walls})")).is_err());
        assert!(parse("=900").is_err());
        assert!(parse(&rt("{mark}=\"{dmark}")).is_err());
        assert!(parse(&rt("{walls} -> ")).is_err());
        assert!(structured(&parse(&rv("doors")).unwrap().expr));
        assert!(!structured(&parse(&rt("{masonry} 200")).unwrap().expr));
    }

    /// Two walls on level "Floor 1", one on "Floor 2" (Russian names in the fixture); three doors (800, 1000, 1200), a window, a room.
    fn scene() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.current_type = 1;
        walls(
            &mut ed,
            &[((0.0, 0.0), (6.0, 0.0)), ((0.0, 4.0), (6.0, 4.0))],
        );
        let w = ed.walls[0].id;
        for (along, width, kind) in [(0.5, 0.8, "door"), (2.0, 1.0, "door"), (4.0, 1.2, "window")] {
            ed.model
                .add_opening(
                    w,
                    &ddc_model::OpeningSpec {
                        along,
                        width,
                        height: 2.1,
                        sill: if kind == "door" { 0.0 } else { 0.9 },
                        kind: kind.into(),
                    },
                )
                .unwrap();
        }
        let w2 = ed.walls[1].id;
        ed.model
            .add_opening(
                w2,
                &ddc_model::OpeningSpec {
                    along: 1.0,
                    width: 1.2,
                    height: 2.1,
                    sill: 0.0,
                    kind: "door".into(),
                },
            )
            .unwrap();
        let l2 = ed.model.add_level(&rt("{floor} 2"), 3.0).unwrap();
        ed.reload().unwrap();
        let cur = ed.level;
        ed.set_level(l2);
        ed.current_type = 2;
        walls(&mut ed, &[((0.0, 0.0), (6.0, 0.0))]);
        ed.set_level(cur);
        ed
    }

    fn n(ed: &Editor, q: &str) -> usize {
        eval(ed, &parse(q).unwrap()).len()
    }

    #[test]
    fn eval_over_all_levels() {
        let ed = scene();
        assert_eq!(n(&ed, &rt("{walls}")), 3);
        assert_eq!(n(&ed, &rt("{doors} {width}>900")), 2);
        assert_eq!(n(&ed, &rt("{doors} {width}>=1000 {width}<1200")), 1);
        assert_eq!(n(&ed, &rt("{doors} {width}=800")), 1);
        assert_eq!(n(&ed, &rt("{windows} {or} {doors}")), 4);
        assert_eq!(n(&ed, &rt("{openings} {sill}>0")), 1);
        assert_eq!(n(&ed, &rt("{walls} {type}={masonry} 200 {level}=1")), 2);
        assert_eq!(n(&ed, &rt("{walls} {type}={masonry_l} 200 {level}={floor} 2")), 0);
        assert_eq!(n(&ed, &rt("{walls} {type}={outer} 380 L2")), 1);
        assert_eq!(n(&ed, &rt("{walls} {level}>1")), 1);
        assert_eq!(n(&ed, &rt("{walls} {type}={masonry}*")), 2);
        assert_eq!(n(&ed, &rt("{walls} {type}~{masonry_l}")), 2);
        assert_eq!(n(&ed, &rt("{walls} {not} {type}={masonry} 200")), 1);
        assert_eq!(n(&ed, &rt("{walls} {thickness}=200")), 2);
        assert_eq!(n(&ed, &rt("{walls} {length}=6000")), 3);
        assert_eq!(n(&ed, &rt("{masonry} 200")), 2);
        assert_eq!(n(&ed, &rt("{doors} {mark}={empty}")), 3);
        assert_eq!(n(&ed, &rt("{category}={windows}")), 1);
        let w = ed.walls[0].id;
        assert!(eval(&ed, &parse(&format!("#{w}")).unwrap()).contains(&Sel::Wall(w)));
        assert_eq!(n(&ed, &rt(&format!("{{walls}} id={w}"))), 1);
    }

    #[test]
    fn bulk_assign_is_one_undo_step() {
        let mut ed = scene();
        let undo0 = ed.can_undo();
        let (sel, k) = run_text(&mut ed, &rt("{doors} -> {width}={width}+100")).unwrap();
        assert_eq!((sel.len(), k), (3, 3));
        assert_eq!(n(&ed, &rt("{doors} {width}=900")), 1);
        assert_eq!(n(&ed, &rt("{doors} {width}=1300")), 1);
        ed.undo();
        assert_eq!(n(&ed, &rt("{doors} {width}=800")), 1);
        assert_eq!(n(&ed, &rt("{doors} {width}=1200")), 1);
        assert_eq!(ed.can_undo(), undo0);
        ed.redo();
        assert_eq!(n(&ed, &rt("{doors} {width}=900")), 1);
        // type by name, for all levels; an unknown type is an error with no changes
        let (_, k) = run_text(&mut ed, &rt("{walls} -> {type}={partition_l} 100")).unwrap();
        assert_eq!(k, 3);
        assert_eq!(n(&ed, &rt("{walls} {type}={partition} 100")), 3);
        assert!(run_text(&mut ed, &rt("{walls} -> {type}={nosuch}")).is_err());
        assert!(run_text(&mut ed, &rt("{doors} -> {width}=0")).is_err());
        assert_eq!(n(&ed, &rt("{walls} {type}={partition} 100")), 3);
        // an empty condition applies to the current selection
        ed.selection = vec![Sel::Wall(ed.walls[0].id)];
        let (_, k) = run_text(&mut ed, &rt("-> {height}=3200")).expect("assign to selection");
        assert_eq!(k, 1);
        assert_eq!(n(&ed, &rt("{walls} {height}=3200")), 1);
        let (_, k) = run_text(&mut ed, &rt("-> {height}=3200")).unwrap();
        assert_eq!(k, 0, "no change — no step");
    }

    #[test]
    fn similar_by_category_and_type_in_project() {
        let ed = scene();
        let w = Sel::Wall(ed.walls[0].id);
        assert_eq!(same_category(&ed, w, false, |_| true).len(), 2);
        assert_eq!(same_category(&ed, w, true, |_| true).len(), 3);
        assert_eq!(same_type_project(&ed, w).len(), 2);
        let d = eval(&ed, &parse(&rt("{doors} {width}=1200")).unwrap())[0];
        assert_eq!(same_type_project(&ed, d).len(), 1);
        assert_eq!(same_category(&ed, d, true, |_| true).len(), 3);
    }

    #[test]
    fn palette_rows_and_filters() {
        let mut ed = scene();
        let (ex, first) = extras(&ed, &rt("{doors} {width}>900"), true);
        assert!(first);
        assert_eq!(ex[0].label, rt("{select}: 2 {doors}"));
        assert!(ex[1].key.starts_with("@save:"));
        assert!(ex.iter().any(|e| e.key.starts_with("@el:o")));
        let (ex, first) = extras(&ed, &rt("{floor_l} 2"), true);
        assert!(!first);
        assert!(ex.iter().any(|e| e.label == rt("{plan}: {floor} 2")));
        let (ex, _) = extras(&ed, &rt("({doors}"), true);
        assert_eq!(ex[0].key, "@none");
        save(&mut ed, &rt("{doors} {width}>900")).unwrap();
        assert_eq!(saved(&ed), vec![rt("{doors} {width}>900")]);
        let (ex, _) = extras(&ed, "", true);
        assert_eq!(ex[0].key, rt("@sq:{doors} {width}>900"));
        assert_eq!(sel_from(&sel_key(Sel::Room(7))), Some(Sel::Room(7)));
        assert_eq!(summary(&[], true), "");
    }

    #[test]
    fn filter_numbers_follow_the_decimal_mark() {
        use DecimalMark::{Comma as RU, Point as EN};
        let m = Units::Metric;
        let near = |a: Option<f64>, b: f64| a.is_some_and(|a| (a - b).abs() < 1e-9);
        // length field: bare number is millimetres
        assert!(near(qnum_mark("width", "1,000", m, EN), 1.0));
        assert!(near(qnum_mark("width", "1,000 mm", m, EN), 1.0));
        assert!(near(qnum_mark("width", "900", m, EN), 0.9));
        assert!(qnum_mark("width", "1,5", m, EN).is_none());
        assert!(qnum_mark("width", "1,5 mm", m, EN).is_none());
        assert!(near(qnum_mark("width", "1,5", m, RU), 0.0015));
        assert!(near(qnum_mark("width", "-0,1 \u{43c}", m, RU), -0.1));
        // plain number field
        assert_eq!(qnum_mark("count", "1,000", m, EN), Some(1000.0));
        assert_eq!(qnum_mark("count", "1,000.5", m, EN), Some(1000.5));
        assert!(qnum_mark("count", "1,5", m, EN).is_none());
        assert_eq!(qnum_mark("count", "1,5", m, RU), Some(1.5));
        assert_eq!(qnum_mark("count", "1 000,5", m, RU), Some(1000.5));
        assert!(qnum_mark("count", "abc", m, RU).is_none());
        // substring search text: the comma turns into a point only where it is the decimal mark
        assert_eq!(point_text("1,5", RU), "1.5");
        assert_eq!(point_text("1,5", EN), "1,5");
    }

    #[test]
    fn stored_prop_numbers_ignore_the_ui_mark() {
        let props = [("a", "1.5"), ("b", "1,5"), ("c", "1,000"), ("d", "1,250"), ("e", "1 250"), ("f", "-0.25")];
        let read = |mark: DecimalMark| {
            std::thread::spawn(move || {
                crate::units::set_ui_decimal_mark(mark);
                let r = Row {
                    sel: Sel::Room(1),
                    cat: "room",
                    level: 1,
                    ty: String::new(),
                    name: String::new(),
                    mark: String::new(),
                    nums: vec![],
                    props: props.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                };
                ["a", "b", "c", "d", "e", "f"].map(|k| r.num(k))
            })
            .join()
            .unwrap()
        };
        let want = [Some(1.5), Some(1.5), None, None, None, Some(-0.25)];
        assert_eq!(read(DecimalMark::Comma), want);
        assert_eq!(read(DecimalMark::Point), want);
    }

    #[test]
    fn tool_numbers_read_the_same_in_every_language() {
        fn lit(v: &str) -> Assign {
            Assign { key: "width".into(), val: Val::Lit(v.into()) }
        }
        fn mul(v: &str) -> Assign {
            Assign { key: "width".into(), val: Val::Rel('*', v.into()) }
        }
        // the same tool call with the UI mark set explicitly per thread
        let run = |mark: DecimalMark| {
            std::thread::spawn(move || {
                crate::units::set_ui_decimal_mark(mark);
                let ed = scene();
                let widths = |ed: &Editor| -> Vec<f64> {
                    let mut st = ed.model.sql().prepare("SELECT width FROM opening ORDER BY id").unwrap();
                    let v: Vec<f64> = st.query_map([], |r| r.get(0)).unwrap().flatten().collect();
                    v
                };
                let before = widths(&ed);
                let filters = ["door width>900.5", "door width>=1000", "door width>=1,000.5", "door width>=1,000", "door width>1,5"]
                    .map(|q| eval_tool(&ed, q).map(|v| v.len()));
                let doors = eval_tool(&ed, "door").unwrap();
                let set = |a: Assign| -> Result<Vec<f64>, String> {
                    let edits = assign_edits_in(&ed.model, ed.tool.units, NumIn::Tool, &doors, &[a]).map_err(String::from)?;
                    let mut w: Vec<f64> = edits
                        .iter()
                        .filter_map(|e| match e {
                            ddc_model::Edit::Opening { spec, .. } => Some(spec.width),
                            _ => None,
                        })
                        .collect();
                    w.sort_by(f64::total_cmp);
                    Ok(w)
                };
                let sets = [lit("1000"), lit("1,000"), lit("1,5"), mul("1.5"), mul("1,5")].map(set);
                assert_eq!(widths(&ed), before, "the model is unchanged until Apply");
                (filters, sets)
            })
            .join()
            .unwrap()
        };
        let (ru, en) = (run(DecimalMark::Comma), run(DecimalMark::Point));
        assert_eq!(ru, en);
        let (filters, sets) = en;
        assert_eq!(filters[0], Ok(2));
        assert_eq!(filters[1], Ok(2));
        assert_eq!(filters[2], Ok(1));
        assert!(filters[3].as_ref().is_err_and(|e| e.contains("1,000")), "{:?}", filters[3]);
        assert!(filters[4].as_ref().is_err_and(|e| e.contains("1,5")), "{:?}", filters[4]);
        let near = |v: &[f64], w: &[f64]| v.len() == w.len() && v.iter().zip(w).all(|(a, b)| (a - b).abs() < 1e-9);
        assert!(sets[0].as_ref().is_ok_and(|v| near(v, &[1.0, 1.0])), "set width = 1000 is 1 m: {:?}", sets[0]);
        assert!(sets[1].is_err(), "ambiguous 1,000 is refused");
        assert!(sets[2].is_err(), "ambiguous 1,5 is refused");
        assert!(sets[3].as_ref().is_ok_and(|v| near(v, &[1.2, 1.5, 1.8])), "{:?}", sets[3]);
        assert!(sets[4].is_err());
        // reading rules
        let m = Units::Metric;
        assert_eq!(qnum_in("count", "1.5", m, NumIn::Tool), Some(1.5));
        assert_eq!(qnum_in("count", "1,5", m, NumIn::Tool), None);
        assert_eq!(qnum_in("count", "1,000", m, NumIn::Tool), None);
        assert_eq!(qnum_in("count", "1,000.5", m, NumIn::Tool), Some(1000.5));
        assert_eq!(qnum_in("count", "1,000,000", m, NumIn::Tool), Some(1_000_000.0));
        assert!(check_tool_numbers("door name=\"1,5\"").is_ok(), "quoted text is not a number");
        assert!(check_tool_numbers("door mark=A1,5").is_ok(), "a number glued to a word is text");
    }

    #[test]
    fn saved_filter_numbers_read_the_same_in_every_language() {
        use DecimalMark::{Comma as RU, Point as EN};
        let mut ed = scene();
        let typed = |q: &str, m: DecimalMark| eval_env(&ed, &parse(q).unwrap(), &Env::typed(&ed, m)).len();
        let saved_n = |q: &str| eval_saved(&ed, q).unwrap().len();
        // one filter typed in either locale is saved as one invariant text, same result
        let ru = canon_query(&rt("{doors} {width}>900,5"), RU).unwrap();
        assert_eq!(ru, rt("{doors} {width}>900.5"));
        assert_eq!(canon_query(&rt("{doors} {width}>900.5"), EN).unwrap(), ru);
        assert_eq!(saved_n(&ru), 2);
        assert_eq!(saved_n(&ru), typed(&rt("{doors} {width}>900,5"), RU));
        assert_eq!(saved_n(&ru), typed(&rt("{doors} {width}>900.5"), EN));
        // legacy text saved in the comma locale keeps its meaning
        assert_eq!(saved_n(&rt("{doors} {width}>900,5")), 2);
        assert_eq!(typed(&rt("{doors} {width}>900,5"), EN), 0, "typed in the point locale it is refused");
        // a point-locale group is saved as the plain number
        let en = canon_query(&rt("{doors} {width}>=1,200"), EN).unwrap();
        assert_eq!(en, rt("{doors} {width}>=1200"));
        assert_eq!(saved_n(&en), 1);
        assert_eq!(saved_n(&en), typed(&rt("{doors} {width}>=1200"), EN));
        assert_eq!(canon_query("count=1,000", EN).unwrap(), "count=1000");
        assert_eq!(canon_query("count=1,000", RU).unwrap(), "count=1.000");
        // stored reading: invariant, legacy comma, unambiguous groups; `1,000` alone is ambiguous
        let m = Units::Metric;
        assert_eq!(qnum_in("count", "1.5", m, NumIn::Stored), Some(1.5));
        assert_eq!(qnum_in("count", "1,5", m, NumIn::Stored), Some(1.5));
        assert_eq!(qnum_in("count", "1000", m, NumIn::Stored), Some(1000.0));
        assert_eq!(qnum_in("count", "1,000.5", m, NumIn::Stored), Some(1000.5));
        assert_eq!(qnum_in("count", "1,000", m, NumIn::Stored), None);
        assert!(qnum_in("width", "1,5", m, NumIn::Stored).is_some_and(|x| (x - 0.0015).abs() < 1e-12));
        // quoted text, inches and names are kept as typed
        assert_eq!(canon_query(&rt(r#"{mark}="1,5" {width}>1,000"#), EN).unwrap(), rt(r#"{mark}="1,5" {width}>1000"#));
        assert_eq!(canon_query(&rt(r#"{doors} {width}>3'-0""#), EN).unwrap(), rt(r#"{doors} {width}>3'-0""#));
        assert_eq!(canon_query(&rt("{walls} {type}={masonry} 200 L2"), RU).unwrap(), rt("{walls} {type}={masonry} 200 L2"));
        // text is never rewritten: level names, multi-word values, free words, quotes
        for m in [EN, RU] {
            for q in [
                "walls level=Floor 1,5",
                "walls level=1,5",
                "walls 1,5",
                "walls mark=\"1,5\"",
                "walls mark=\u{ab}2,5\u{bb}",
                "walls mark=EI 1,5",
            ] {
                assert_eq!(canon_query(q, m).unwrap(), q, "{m:?}");
            }
        }
        assert_eq!(canon_query("walls mark=EI 1,5 width>1,000", EN).unwrap(), "walls mark=EI 1,5 width>1000");
        assert_eq!(canon_query("walls width > 1,000", EN).unwrap(), "walls width > 1000");
        assert_eq!(canon_query("walls width>=-2,5", RU).unwrap(), "walls width>=-2.5");
        // invalid input writes nothing: no filter, no undo step
        let (before, steps) = (saved(&ed), ed.history().0.len());
        assert!(save_mark(&mut ed, &rt("{doors} {width}>1,5"), EN).is_err());
        assert!(save_mark(&mut ed, &rt("({doors} {width}>900"), RU).is_err());
        assert!(save_mark(&mut ed, "  ", RU).is_err());
        assert_eq!(saved(&ed), before);
        assert_eq!(ed.history().0.len(), steps);
        assert_eq!(ed.model.saved_filters().unwrap(), None);
        assert_eq!(save_mark(&mut ed, &rt("{doors} {width}>900,5"), RU).unwrap(), ru);
        assert_eq!(saved(&ed).first(), Some(&ru));
    }

    fn steps(ed: &Editor) -> usize {
        ed.history().0.len()
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-xs58-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn saving_a_filter_is_one_undo_step_in_the_project() {
        let mut ed = scene();
        let n = steps(&ed);
        save(&mut ed, "doors width>900").unwrap();
        assert_eq!(steps(&ed), n + 1, "one undo step");
        assert_eq!(ed.model.saved_filters().unwrap(), Some(vec!["doors width>900".to_string()]));
        // no raw mark outside the operation path
        assert_eq!(ed.model.legacy_filter_mark().unwrap(), None);
        // the newest filter again: nothing changed, nothing written
        save(&mut ed, "doors width>900").unwrap();
        assert_eq!(steps(&ed), n + 1);
        save(&mut ed, "walls").unwrap();
        assert_eq!(saved(&ed), vec!["walls".to_string(), "doors width>900".to_string()]);
        ed.undo();
        assert_eq!(saved(&ed), vec!["doors width>900".to_string()]);
        ed.undo();
        assert!(saved(&ed).is_empty());
        ed.redo();
        assert_eq!(saved(&ed), vec!["doors width>900".to_string()]);
    }

    #[test]
    fn unsaved_project_filter_survives_first_save_and_reopen() {
        let d = tmp_dir("first");
        let p = d.join("a.ddc");
        let mut e = Editor::start_project().unwrap();
        save(&mut e, "doors width>900").unwrap();
        assert!(e.is_dirty(), "a new filter is an unsaved edit");
        e.save_project_as(&p).unwrap();
        assert!(!e.is_dirty());
        save(&mut e, "doors width>900").unwrap();
        assert!(!e.is_dirty(), "the same filter again writes nothing");
        drop(e);
        let mut b = Editor::start_project().unwrap();
        b.open_project(&p).unwrap();
        assert_eq!(saved(&b), vec!["doors width>900".to_string()]);
        assert!(!b.is_dirty(), "reading filters writes nothing");
        drop(b);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn save_as_copy_edits_its_own_filters() {
        let d = tmp_dir("copy");
        let (a, b) = (d.join("a.ddc"), d.join("b.ddc"));
        let mut e = Editor::start_project().unwrap();
        save(&mut e, "doors").unwrap();
        e.save_project_as(&a).unwrap();
        e.save_project_as(&b).unwrap();
        save(&mut e, "walls").unwrap();
        e.save_project().unwrap();
        assert_eq!(saved(&e), vec!["walls".to_string(), "doors".to_string()]);
        let mut o = Editor::start_project().unwrap();
        o.open_project_read_only(&a).unwrap();
        assert_eq!(saved(&o), vec!["doors".to_string()], "the original keeps its list");
        drop(o);
        drop(e);
        let mut c = Editor::start_project().unwrap();
        c.open_project(&b).unwrap();
        assert_eq!(saved(&c), vec!["walls".to_string(), "doors".to_string()]);
        drop(c);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn legacy_filters_migrate_once_into_the_project() {
        let mut ed = scene();
        // fixture: a file an older build marked, its filters in the user list
        ed.model
            .sql()
            .execute("INSERT INTO meta(key, value) VALUES (?1, 171)", [ddc_model::LEGACY_FILTER_MARK])
            .unwrap();
        seed_legacy("uid:ab\tdoors\nuid:cd\tforeign\nuid:ab\twalls\n\nbroken line\n");
        let n = steps(&ed);
        // reading shows them and writes nothing
        assert_eq!(saved(&ed), vec!["walls".to_string(), "doors".to_string()]);
        assert_eq!(steps(&ed), n);
        assert_eq!(ed.model.saved_filters().unwrap(), None);
        // the first change stores the whole list in the project, in one step
        save(&mut ed, "rooms").unwrap();
        assert_eq!(steps(&ed), n + 1);
        let all = vec!["doors".to_string(), "walls".to_string(), "rooms".to_string()];
        assert_eq!(ed.model.saved_filters().unwrap(), Some(all));
        assert_eq!(ed.model.legacy_filter_mark().unwrap(), None);
        // the user list no longer counts once the project has its own
        seed_legacy("uid:ab\tlate\n");
        assert_eq!(saved(&ed)[0], "rooms");
        assert!(!saved(&ed).contains(&"late".to_string()));
        // undo goes back to the marked file and its old list
        ed.undo();
        assert_eq!(ed.model.legacy_filter_mark().unwrap(), Some(171));
        seed_legacy("uid:ab\tdoors\nuid:ab\twalls\n");
        assert_eq!(saved(&ed), vec!["walls".to_string(), "doors".to_string()]);
    }

    #[test]
    fn imperial_inches_negatives_and_symbol_operators() {
        let (m, i) = (Units::Metric, Units::Imperial);
        let near = |a: Option<f64>, b: f64| a.is_some_and(|a| (a - b).abs() < 1e-9);
        assert!(near(qnum("width", "3'-0\"", i), 0.9144));
        assert!(near(qnum("width", "6\"", i), 0.1524));
        assert!(
            near(qnum("width", "6", i), 0.1524),
            "bare imperial = inches"
        );
        assert!(near(qnum("offset", "-6\"", i), -0.1524));
        assert!(near(qnum("offset", &rt("-0,1 {m}"), m), -0.1));
        assert!(near(qnum("offset", "-100", m), -0.1));
        assert!(near(qnum("offset", "0", m), 0.0));
        let q = parse(&rt(r#"{doors} {width}>3'-0""#)).unwrap();
        assert_eq!(
            q.expr,
            Expr::And(vec![Expr::Cat("door"), cmp_(&rv("width"), Op::Gt, "3'-0\"")])
        );
        assert_eq!(
            parse(&rt("{windows} | {doors}")).unwrap().expr,
            Expr::Or(vec![Expr::Cat("window"), Expr::Cat("door")])
        );
        assert_eq!(
            parse(&rt("{walls} & !{type}={masonry}")).unwrap().expr,
            Expr::And(vec![
                Expr::Cat("wall"),
                Expr::Not(Box::new(cmp_(&rv("type"), Op::Eq, &rv("masonry"))))
            ])
        );
        assert_eq!(
            parse(&rt("{width}!=900")).unwrap().expr,
            cmp_(&rv("width"), Op::Ne, "900")
        );
        let mut ed = scene();
        assert_eq!(n(&ed, &rt("{windows} || {doors} && {width}>900")), 3);
        assert_eq!(n(&ed, &rt("{doors} {width}>-1")), 3);
        ed.set_units(i);
        assert_eq!(n(&ed, &rt(r#"{doors} {width}>3'-0""#)), 2);
        assert_eq!(n(&ed, &rt("{doors} {width}>36")), 2);
    }

    #[test]
    fn level_exact_name_ordinal_or_elevation() {
        let ed = scene();
        assert_eq!(n(&ed, &rt("{walls} {level}={floor} 2")), 1);
        assert_eq!(n(&ed, &rt("{walls} {level}=+3.000")), 1);
        assert_eq!(n(&ed, &rt("{walls} {level}=3000")), 1);
        assert_eq!(n(&ed, &rt("{walls} {level}=2")), 1);
        assert_eq!(n(&ed, &rt("{walls} {level}={floor}")), 0, "no partial name match");
        assert_eq!(n(&ed, &rt("{walls} {level}=+7.000")), 0);
        // `~` on a number uses project units (mm)
        assert_eq!(n(&ed, &rt("{doors} {width}~200")), 1);
        assert_eq!(n(&ed, &rt("{doors} {width}~0,2")), 0);
    }

    #[test]
    fn unsupported_assignment_is_explicit_error() {
        let mut ed = scene();
        let e = run_text(&mut ed, &rt("{walls} -> {thickness}=300")).unwrap_err().render(true);
        assert!(e.contains(&rv("w_type")), "{e}");
        let e = run_text(&mut ed, &rt("{walls} -> {width}=300")).unwrap_err().render(true);
        assert!(e.contains(&rv("w_type")), "{e}");
        let e = run_text(&mut ed, &rt("{doors} -> {type}={masonry} 200")).unwrap_err().render(true);
        assert!(e.contains(&rv("w_dims")), "{e}");
        let e = run_text(&mut ed, &rt("{doors} -> {fire}=EI30")).unwrap_err().render(true);
        assert!(e.contains(&rv("w_cannot")), "{e}");
        // if at least one element accepted the field, the rest are skipped
        let (sel, k) = run_text(&mut ed, &rt("{walls} {or} {windows} -> {sill}=0")).unwrap();
        assert_eq!((sel.len(), k), (4, 1));
    }

    #[test]
    fn room_name_keeps_quotes() {
        let mut ed = demo_scene();
        let r = rows(&ed)
            .iter()
            .find(|x| x.cat == "room")
            .map(Row::id)
            .expect("room");
        let a = Assign {
            key: "name".into(),
            val: Val::Lit(rv("cafe")),
        };
        assert_eq!(assign(&mut ed, &[Sel::Room(r)], &[a]).unwrap(), 1);
        let name: String = ed
            .model
            .sql()
            .query_row("SELECT name FROM room WHERE id=?1", [r], |x| x.get(0))
            .unwrap();
        assert_eq!(name, rv("cafe"));
    }

    #[test]
    fn rows_cache_follows_edits_and_undo() {
        let mut ed = scene();
        let a = rows(&ed);
        assert!(Rc::ptr_eq(&a, &rows(&ed)), "cached");
        run_text(&mut ed, &rt("{doors} -> {width}={width}+100")).unwrap();
        assert!(!Rc::ptr_eq(&a, &rows(&ed)));
        assert_eq!(n(&ed, &rt("{doors} {width}=900")), 1);
        ed.undo();
        assert_eq!(n(&ed, &rt("{doors} {width}=800")), 1);
    }

    #[test]
    fn builtin_type_labels_match_in_both_languages() {
        let env = Env {
            levels: vec![(1, "L1".into())],
            elev: vec![0.0],
            units: Units::Metric,
            num: NumIn::Typed(DecimalMark::Comma),
        };
        let mk = |sel, cat, key: &str, ru: bool| Row {
            sel,
            cat,
            level: 1,
            ty: crate::tr_lang(ru, key, &[]),
            name: String::new(),
            mark: String::new(),
            nums: vec![],
            props: vec![],
        };
        for ru in [true, false] {
            let room = mk(Sel::Room(1), "room", "unit_fmt.q.name.room", ru);
            let cw = mk(Sel::Curtain(2), "curtain", "unit_fmt.q.name.curtain", ru);
            let hit = |q: &str, r: &Row| matches(&parse(q).unwrap().expr, r, &env);
            for q in [rt("{type}={room}"), "type=Room".to_string(), "type=room".to_string(), rt("{type}~{room_s}")] {
                assert!(hit(&q, &room), "{q} ru={ru}");
                assert!(!hit(&q, &cw), "{q} ru={ru}");
            }
            assert!(!hit(&rt("{type}!={room}"), &room) && !hit("type!=Room", &room));
            assert!(hit(&rt(r#"{type}="{curtain}""#), &cw) && hit(r#"type="Curtain wall""#, &cw));
            assert!(hit(&rv("room"), &room) && hit("Room", &room), "free text ru={ru}");
        }
        // the shown label still follows the row text, nothing else gains the built-in names
        let wall = Row { cat: "wall", ty: "Room".into(), ..mk(Sel::Wall(3), "wall", "x", false) };
        assert!(!matches(&parse(&rt("{type}={room}")).unwrap().expr, &wall, &env));
    }

    /// 100,000 rows: parse + filter < 50 ms (optimized build; ×10 in debug).
    #[test]
    fn query_100k_rows_is_fast() {
        let env = Env {
            levels: (1..=10).map(|i| (i, format!("{} {i}", rv("floor")))).collect(),
            elev: (0..10).map(|i| f64::from(i) * 3.0).collect(),
            units: Units::Metric,
            num: NumIn::Typed(DecimalMark::Comma),
        };
        let cats = ["wall", "door", "window", "floor", "room"];
        let rs: Vec<Row> = (0..100_000i64)
            .map(|i| Row {
                sel: Sel::Wall(i),
                cat: cats[(i % 5) as usize],
                level: i % 10 + 1,
                ty: format!("{} {}", rv("masonry"), 100 + (i % 4) * 100),
                name: String::new(),
                mark: String::new(),
                nums: vec![
                    ("height", 2.5 + (i % 7) as f64 * 0.1),
                    ("width", 0.1 * (1 + i % 4) as f64),
                ],
                props: vec![],
            })
            .collect();
        let t = web_time::Instant::now();
        let q = parse(&rt("{walls} {level}>=2 {type}={masonry}* {height}>2700 {or} {doors} {width}<300")).unwrap();
        let f = Filter::new(&q.expr, &env);
        let hits = rs.iter().filter(|r| f.test(r)).count();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(hits > 0);
        let limit = if cfg!(debug_assertions) { 500.0 } else { 50.0 };
        assert!(ms < limit, "{ms:.1} ms for 100k rows");
    }

    #[test]
    fn query_unclosed_quote_error_en() {
        let e = parse("name=\"Door").unwrap_err();
        assert_eq!(e.key, "query.err.unclosed_quote");
        assert_eq!(e.render(false), "closing quote missing");
    }

    #[test]
    fn query_error_ru_unchanged() {
        let ru = |t: &str| parse(t).unwrap_err().render(true);
        assert_eq!(ru(&rt("{mark}=\"{dmark}")), rv("err_quote"));
        assert_eq!(ru(&rt("({walls}")), rv("err_paren"));
        assert_eq!(ru(&rt("{walls})")), rv("err_extra"));
        assert_eq!(ru("=900"), rv("err_nofield"));
        assert_eq!(ru(&rt("{walls} -> ")), rv("err_noassign"));
        assert_eq!(ru(&rt("{walls} -> {width}")), rv("err_needeq"));
        let mut ed = scene();
        let e = run_text(&mut ed, &rt("{doors} -> {width}=0")).unwrap_err();
        assert_eq!(e.render(true), rv("err_pos"));
        assert_eq!(e.render(false), "width: the value must be greater than zero");
    }

    #[test]
    fn russian_keywords_still_parse() {
        let q = parse(&rt("{not} ({rooms} {or} {slabs}) {and} {doors} {width}>900")).unwrap();
        let en = parse("not (rooms or floors) and doors width>900").unwrap();
        // the field name stays as typed in the parsed tree and is resolved by `canon`
        let key = "\u{448}\u{438}\u{440}\u{438}\u{43d}\u{430}";
        assert_eq!(canon(key), "width");
        let shown = format!("{:?}", q.expr).replace(&format!("{key:?}"), "\"width\"");
        assert_eq!(shown, format!("{:?}", en.expr));
        assert_eq!(parse(&rv("walls")).unwrap().expr, parse("walls").unwrap().expr);
    }
}
