//! Computed schedule columns: a formula over the columns of a row (values in SI: m, m2, m3).
//! Parsing and evaluation belong to the shared formula evaluator (`ddc_geom::component::expr`)
//! in its schedule dialect: numbers (with a point), `+ - * / ( )`, comparisons
//! `< <= > >= = <>`, a column by key (`len`) or by label in brackets (`[Floor area]`),
//! functions round, min, max, abs, sqrt, if and the group functions sum, avg, count, total.
//! This file only binds names to columns and words the errors. A parse or evaluation error is
//! written into the cell, never a panic.

use super::{col, Col, Edit, Kind, Row, Table, Val};
use ddc_geom::component::expr::{self, Code, Cx, Dialect, Dim, Fail, Q, NUMBER};
pub use ddc_geom::component::expr::{MAX_DEPTH, MAX_LEN};

/// Aggregates take one column: `sum` and `avg` of the row's group, `total` of all rows.
const AGGS: [&str; 4] = ["sum", "avg", "count", "total"];

/// A parsed formula: the shared expression with columns and group functions as variables
/// (`#3` is column 3, `sum#3` the sum of column 3 over the group).
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    e: expr::Expr,
    aggs: Vec<(String, usize)>,
}

/// Description of a tab's computed column.
#[derive(Clone, Debug, PartialEq)]
pub struct Calc {
    pub name: String,
    pub expr: String,
    pub kind: Kind,
}

/// Value of a group function for `(function, column)`; `Err` is the cell error.
type Agg<'a> = &'a dyn Fn(&str, usize) -> Result<f64, String>;

/// Parses a formula; columns are looked up in `cols` (key, then label, case-insensitive).
pub fn parse(src: &str, cols: &[Col]) -> Result<Expr, String> {
    if src.trim().is_empty() {
        return Err(crate::tr("sch.f.empty", &[]));
    }
    let e = expr::parse_in(src, Dialect::Sheet).map_err(|e| parse_msg(&e))?;
    let mut lo = Lower {
        cols,
        aggs: vec![],
    };
    let e = lo.go(&e)?;
    Ok(Expr { e, aggs: lo.aggs })
}

/// A character the formula language has no use for where it stands; a `]` the parser stops at
/// closes no `[`, since a column reference consumes its own.
fn is_symbol(c: char) -> bool {
    !(c.is_alphanumeric() || "_.[(),;+-*/=<>".contains(c))
}

fn parse_msg(e: &expr::ExprError) -> String {
    let max = |n: usize| n.to_string();
    match &e.code {
        Code::TooLong => crate::tr("sch.f.too_long", &[("max", &max(MAX_LEN))]),
        Code::Depth => crate::tr("sch.f.depth", &[("max", &max(MAX_DEPTH))]),
        Code::Number(t) => crate::tr("sch.f.number", &[("t", t)]),
        Code::Bracket => crate::tr("sch.f.no_bracket", &[]),
        Code::Paren => crate::tr("sch.f.no_paren", &[]),
        Code::CallParen(f) => crate::tr("sch.f.call_paren", &[("f", f)]),
        Code::Truncated => crate::tr("sch.f.truncated", &[]),
        Code::Trailing(c) | Code::Unexpected(c) if is_symbol(*c) => {
            crate::tr("sch.f.symbol", &[("c", &c.to_string())])
        }
        Code::Trailing(_) => crate::tr("sch.f.trailing", &[]),
        Code::Unexpected(c) => crate::tr("sch.f.unexpected", &[("t", &c.to_string())]),
        _ => e.msg.clone(),
    }
}

/// Binds the names of a parsed formula to columns and checks the calls.
struct Lower<'a> {
    cols: &'a [Col],
    aggs: Vec<(String, usize)>,
}

impl Lower<'_> {
    fn column(&self, name: &str) -> Option<usize> {
        let low = name.to_lowercase();
        self.cols
            .iter()
            .position(|c| c.key.to_lowercase() == low)
            .or_else(|| self.cols.iter().position(|c| c.title.to_lowercase() == low))
    }

    fn go(&mut self, e: &expr::Expr) -> Result<expr::Expr, String> {
        use expr::Expr as E;
        Ok(match e {
            E::Num(_) => e.clone(),
            E::Var(n) => match self.column(n) {
                Some(j) => E::Var(format!("#{j}")),
                None => return Err(crate::tr("sch.f.no_column", &[("n", n)])),
            },
            E::Neg(x) => E::Neg(Box::new(self.go(x)?)),
            E::Not(x) => E::Not(Box::new(self.go(x)?)),
            E::Bin(o, a, b) => E::Bin(*o, Box::new(self.go(a)?), Box::new(self.go(b)?)),
            E::Call(f, a) if AGGS.contains(&f.as_str()) => {
                let col = match a.as_slice() {
                    [E::Var(n)] => self.column(n),
                    _ => None,
                };
                let Some(j) = col else {
                    return Err(crate::tr("schm.f.agg_arg", &[("f", f.as_str())]));
                };
                self.aggs.push((f.clone(), j));
                E::Var(format!("{f}#{j}"))
            }
            E::Call(f, a) => {
                let Some((lo, hi)) = expr::arity(f) else {
                    return Err(crate::tr("sch.f.no_function", &[("f", f.as_str())]));
                };
                if !(lo..=hi).contains(&a.len()) {
                    return Err(crate::tr("sch.f.arg_count", &[("f", f.as_str())]));
                }
                let args = a.iter().map(|x| self.go(x)).collect::<Result<_, _>>()?;
                E::Call(f.clone(), args)
            }
        })
    }
}

/// `#3` is column 3, `sum#3` a group function of it.
fn split(name: &str) -> (Option<&str>, usize) {
    let (f, j) = name.split_once('#').unwrap_or(("", name));
    (Some(f).filter(|f| !f.is_empty()), j.parse().unwrap_or(0))
}

/// The number in a cell: text cells are stored data (invariant form or an unambiguous legacy
/// comma only).
fn cell(r: &Row, j: usize) -> Result<f64, String> {
    match r.vals.get(j) {
        Some(Val::N(x)) => Ok(*x),
        Some(Val::T(s)) if s.trim().is_empty() => Ok(0.0),
        Some(Val::T(s)) => ddc_model::parse_stored(s)
            .ok_or_else(|| crate::tr("sch.f.not_number", &[("s", s)])),
        None => Err(crate::tr("sch.f.no_value", &[])),
    }
}

fn name_dim(d: Dim) -> String {
    match (d.len, d.mass) {
        (0, 0) => "1".to_string(),
        (l, 0) => format!("m^{l}"),
        (0, m) => format!("kg^{m}"),
        (l, m) => format!("m^{l}*kg^{m}"),
    }
}

/// The wording of an evaluation error in the interface language.
fn fail_msg(f: Fail) -> String {
    match f.code {
        Code::DivZero => crate::tr("sch.f.div0", &[]),
        Code::NotFinite => crate::tr("sch.f.not_finite", &[]),
        Code::SqrtNeg => crate::tr("sch.f.sqrt_neg", &[]),
        Code::UnknownName(n) => crate::tr("sch.f.no_column", &[("n", &n)]),
        Code::UnknownFunction(g) => crate::tr("sch.f.no_function", &[("f", &g)]),
        Code::ArgCount(g) => crate::tr("sch.f.arg_count", &[("f", &g)]),
        Code::RoundDigits => crate::tr("schm.f.round_digits", &[]),
        Code::Condition(_) => crate::tr("schm.f.condition", &[]),
        Code::Mismatch { a, b } => {
            let (a, b) = (name_dim(a), name_dim(b));
            crate::tr("schm.f.unit_mismatch", &[("a", &a), ("b", &b)])
        }
        Code::Root(d) => crate::tr("schm.f.unit_root", &[("a", &name_dim(d))]),
        Code::Env(m) => m,
        _ => f.msg,
    }
}

impl Expr {
    fn run(&self, r: &Row, ag: Agg) -> Result<f64, String> {
        // columns are plain numbers here: dimensions are checked once, by `unit`
        let env = |n: &str| -> Result<Q, Fail> {
            let (f, j) = split(n);
            let v = match f {
                Some(f) => ag(f, j),
                None => cell(r, j),
            };
            v.map(|v| Q { v, d: NUMBER }).map_err(Fail::env)
        };
        let cx = Cx {
            env: &env,
            lenient: true,
            dry: false,
        };
        self.e.eval_with(&cx).map(|q| q.v).map_err(fail_msg)
    }

    pub fn eval(&self, r: &Row) -> Result<f64, String> {
        self.run(r, &|f, _| Err(crate::tr("schm.f.agg_here", &[("f", f)])))
    }

    /// [`Expr::eval`] with group functions: `agg(function, column)` is their value, `None`
    /// when the group has no numbers for it.
    pub fn eval_agg(
        &self,
        r: &Row,
        agg: &dyn Fn(&str, usize) -> Option<f64>,
    ) -> Result<f64, String> {
        let look =
            |f: &str, j: usize| agg(f, j).ok_or_else(|| crate::tr("schm.f.agg_empty", &[("f", f)]));
        self.run(r, &look)
    }

    /// Columns the formula aggregates, as `(function, column)`.
    pub fn aggs(&self) -> Vec<(String, usize)> {
        self.aggs.clone()
    }

    /// Unit of the result over the columns: length and area columns multiply to area, a length
    /// plus an area is refused. A bare number takes the unit of its partner.
    pub fn unit(&self, cols: &[Col]) -> Result<Kind, String> {
        let env = |n: &str| -> Result<Q, Fail> {
            let (f, j) = split(n);
            let k = cols.get(j).map_or(Kind::Num, |c| c.kind);
            let d = if f == Some("count") { NUMBER } else { col_dim(k) };
            Ok(Q { v: 1.0, d })
        };
        let cx = Cx {
            env: &env,
            lenient: true,
            dry: true,
        };
        let q = self.e.eval_with(&cx).map_err(fail_msg)?;
        dim_kind(q.d)
    }
}

fn col_dim(k: Kind) -> Dim {
    match k {
        Kind::Len => expr::LENGTH,
        Kind::Area => expr::AREA,
        Kind::Vol => expr::VOLUME,
        Kind::Mass => expr::MASS,
        _ => NUMBER,
    }
}

fn dim_kind(d: Dim) -> Result<Kind, String> {
    Ok(match (d.len, d.ang, d.mass) {
        (0, 0, 0) => Kind::Num,
        (1, 0, 0) => Kind::Len,
        (2, 0, 0) => Kind::Area,
        (3, 0, 0) => Kind::Vol,
        (0, 0, 1) => Kind::Mass,
        _ => return Err(crate::tr("schm.f.unit_unknown", &[("a", &name_dim(d))])),
    })
}

/// Appends computed columns `f:<n>`; a formula may reference earlier ones.
/// Returns the parse error for each (None if parsed).
pub fn append(tb: &mut Table, calcs: &[Calc]) -> Vec<Option<String>> {
    let mut errs = vec![];
    for (n, c) in calcs.iter().enumerate() {
        let title = if c.name.trim().is_empty() {
            crate::tr("sch.f.default_name", &[("n", &(n + 1).to_string())])
        } else {
            c.name.trim().to_string()
        };
        let parsed = parse(&c.expr, &tb.cols);
        for r in tb.rows.iter_mut() {
            let v = match &parsed {
                Ok(e) => match e.eval(r) {
                    Ok(x) => Val::N(x),
                    Err(m) => Val::T(format!("#{m}")),
                },
                Err(_) => Val::T(crate::tr("sch.f.bad", &[])),
            };
            r.vals.push(v);
        }
        tb.cols.push(col(format!("f:{n}"), title, c.kind, Edit::No));
        errs.push(parsed.err());
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<Col> {
        vec![
            col("len", "Length", Kind::Len, Edit::No),
            col("h", "Height", Kind::Len, Edit::No),
            col("name", "Name", Kind::Text, Edit::No),
        ]
    }

    fn row(vals: Vec<Val>) -> Row {
        Row {
            sel: None,
            id: 1,
            sub: 0,
            vals,
        }
    }

    #[test]
    fn precedence_columns_functions() {
        let r = row(vec![Val::N(4.0), Val::N(3.0), Val::T("a".into())]);
        let c = cols();
        let e = |s: &str| parse(s, &c).and_then(|e| e.eval(&r));
        assert_eq!(e("1 + 2 * 3"), Ok(7.0));
        assert_eq!(e("(1 + 2) * 3"), Ok(9.0));
        assert_eq!(e("-len + [Height]"), Ok(-1.0));
        assert_eq!(e("len * h * 1.2"), Ok(4.0 * 3.0 * 1.2));
        assert_eq!(e("round(10 / 3; 2)"), Ok(3.33));
        assert_eq!(e("max(len, h, 10)"), Ok(10.0));
        assert_eq!(e("if(len > h, 1, 2)"), Ok(1.0));
        assert_eq!(e("len <> 4"), Ok(0.0));
    }

    #[test]
    fn errors_are_values_not_panics() {
        let r = row(vec![Val::N(4.0), Val::N(0.0), Val::T("a".into())]);
        let c = cols();
        assert!(parse("len / h", &c).unwrap().eval(&r).is_err());
        assert!(parse("[No such column]", &c).is_err());
        assert!(parse("name * 2", &c).unwrap().eval(&r).is_err());
        assert!(parse("1 +", &c).is_err());
        assert!(parse("(1", &c).is_err());
        assert!(parse("round()", &c).is_err());
        assert!(parse("2 $ 3", &c).is_err());
        assert!(parse("", &c).is_err());
        assert!(parse("sqrt(-1)", &c).unwrap().eval(&r).is_err());
    }

    #[test]
    fn text_cells_read_the_same_in_every_ui_mark() {
        use crate::units::{set_ui_decimal_mark, DecimalMark};
        let run = |mark: DecimalMark| {
            std::thread::spawn(move || {
                set_ui_decimal_mark(mark);
                let c = cols();
                let e = parse("name * 2 + 0.5", &c).unwrap();
                [
                    "1.5", "2,5", "-0,250", "1,000", "1,250", "1 250", "1,250.5", "abc",
                ]
                .map(|s| {
                    e.eval(&row(vec![Val::N(1.0), Val::N(1.0), Val::T(s.into())]))
                        .ok()
                })
            })
            .join()
            .unwrap()
        };
        let want = [
            Some(3.5),
            Some(5.5),
            Some(0.0),
            None,
            None,
            None,
            None,
            None,
        ];
        assert_eq!(run(DecimalMark::Comma), want);
        assert_eq!(run(DecimalMark::Point), want);
    }

    #[test]
    fn deep_or_long_formulas_are_errors() {
        let c = cols();
        let deep = format!("{}1{}", "(".repeat(10_000), ")".repeat(10_000));
        assert!(parse(&deep, &c).is_err());
        assert!(parse(
            &"-".repeat(500)
                .chars()
                .chain("1".chars())
                .collect::<String>(),
            &c
        )
        .is_err());
        let ok = format!("{}1{}", "(".repeat(20), ")".repeat(20));
        assert!(parse(&ok, &c).is_ok());
        let long = vec!["1"; 600].join("+");
        assert!(parse(&long, &c).is_err());
        assert!(parse(&vec!["1"; 400].join("+"), &c).is_ok());
    }

    #[test]
    fn appended_column_chain() {
        let mut tb = Table {
            cat: super::super::Cat::Walls,
            cols: cols(),
            rows: vec![row(vec![Val::N(2.0), Val::N(3.0), Val::T(String::new())])],
            row_phase: vec![None],
            phases: vec![],
        };
        let calcs = vec![
            Calc {
                name: "S".into(),
                expr: "len*h".into(),
                kind: Kind::Area,
            },
            Calc {
                name: "S2".into(),
                expr: "[S] * 2".into(),
                kind: Kind::Area,
            },
            Calc {
                name: String::new(),
                expr: "len /".into(),
                kind: Kind::Num,
            },
        ];
        let errs = append(&mut tb, &calcs);
        assert_eq!(tb.rows[0].vals[3], Val::N(6.0));
        assert_eq!(tb.rows[0].vals[4], Val::N(12.0));
        assert!(errs[2].is_some() && errs[0].is_none());
        // interface language is a process global another test may switch: accept either
        let t = &tb.cols[5].title;
        assert!(t == "\u{424}\u{43e}\u{440}\u{43c}\u{443}\u{43b}\u{430} 3" || t == "Formula 3", "{t}");
    }

    /// `msg` is the catalog text `key` in either interface language (the language is a process
    /// global another test may switch).
    fn is_msg(msg: &str, key: &str, args: &[(&str, &str)]) -> bool {
        [false, true]
            .iter()
            .any(|&ru| msg == crate::tr_lang(ru, key, args))
    }

    fn fail(src: &str, r: &Row) -> String {
        let c = cols();
        match parse(src, &c) {
            Err(m) => m,
            Ok(e) => e.eval(r).expect_err(src),
        }
    }

    fn num_row() -> Row {
        row(vec![Val::N(4.0), Val::N(3.0), Val::T("a".into())])
    }

    #[test]
    fn every_function_has_a_value() {
        let r = num_row();
        let c = cols();
        let e = |s: &str| parse(s, &c).and_then(|e| e.eval(&r));
        assert_eq!(e("round(2.567)"), Ok(3.0));
        assert_eq!(e("round(2.567, 1)"), Ok(2.6));
        assert_eq!(e("round(1234.5; -2)"), Ok(1200.0));
        assert_eq!(e("min(len, h)"), Ok(3.0));
        assert_eq!(e("min(5)"), Ok(5.0));
        assert_eq!(e("max(len, h)"), Ok(4.0));
        assert_eq!(e("abs(-2.5)"), Ok(2.5));
        assert_eq!(e("abs(h - len)"), Ok(1.0));
        assert_eq!(e("sqrt(16)"), Ok(4.0));
        assert_eq!(e("if(len > h, 10, 20)"), Ok(10.0));
        assert_eq!(e("if(len < h, 10, 20)"), Ok(20.0));
        assert_eq!(e("if(len >= 4, 1, 0) + if(h <= 2, 1, 0)"), Ok(1.0));
        assert_eq!(e("if(len = 4, 1, 0)"), Ok(1.0));
        assert_eq!(e("6 / 4 - 1"), Ok(0.5));
        assert_eq!(e("+3"), Ok(3.0));
    }

    #[test]
    fn function_names_ignore_case_and_equality_has_two_spellings() {
        let r = num_row();
        let c = cols();
        let e = |s: &str| parse(s, &c).and_then(|e| e.eval(&r));
        assert_eq!(e("ROUND(2.567; 1)"), Ok(2.6));
        assert_eq!(e("Max(len, h)"), Ok(4.0));
        assert_eq!(e("len == 4"), Ok(1.0));
        assert_eq!(e("len != 4"), Ok(0.0));
        assert_eq!(e("len = 4"), Ok(1.0));
        assert_eq!(e("len <> 3"), Ok(1.0));
    }

    #[test]
    fn group_functions_need_a_schedule_and_one_column() {
        let c = cols();
        let r = num_row();
        for f in ["sum", "avg", "count", "total"] {
            let e = parse(&format!("{f}(len)"), &c).unwrap();
            assert_eq!(e.aggs(), vec![(f.to_string(), 0)]);
            let m = e.eval(&r).unwrap_err();
            assert!(is_msg(&m, "schm.f.agg_here", &[("f", f)]), "{m}");
            assert_eq!(e.eval_agg(&r, &|_, _| Some(7.0)), Ok(7.0));
            let m = e.eval_agg(&r, &|_, _| None).unwrap_err();
            assert!(is_msg(&m, "schm.f.agg_empty", &[("f", f)]), "{m}");
        }
        for bad in ["sum(1)", "sum(len, h)", "sum()", "avg(len + 1)"] {
            let m = fail(bad, &r);
            let f = bad.split('(').next().unwrap();
            assert!(is_msg(&m, "schm.f.agg_arg", &[("f", f)]), "{bad}: {m}");
        }
        let both = parse("sum(len) / count(h) + 1", &c).unwrap();
        assert_eq!(both.aggs().len(), 2);
        let g = |f: &str, _: usize| Some(if f == "sum" { 8.0 } else { 2.0 });
        assert_eq!(both.eval_agg(&r, &g), Ok(5.0));
    }

    #[test]
    fn units_follow_the_formula() {
        let c = vec![
            col("l", "L", Kind::Len, Edit::No),
            col("a", "A", Kind::Area, Edit::No),
            col("v", "V", Kind::Vol, Edit::No),
            col("n", "N", Kind::Int, Edit::No),
            col("m", "M", Kind::Mass, Edit::No),
        ];
        let k = |s: &str| parse(s, &c).and_then(|e| e.unit(&c));
        assert_eq!(k("l * l"), Ok(Kind::Area));
        assert_eq!(k("a * l"), Ok(Kind::Vol));
        assert_eq!(k("v / a"), Ok(Kind::Len));
        assert_eq!(k("l * 2 + l"), Ok(Kind::Len));
        assert_eq!(k("a / a"), Ok(Kind::Num));
        assert_eq!(k("n + 1"), Ok(Kind::Num));
        assert_eq!(k("sqrt(a)"), Ok(Kind::Len));
        assert_eq!(k("sum(a)"), Ok(Kind::Area));
        assert_eq!(k("count(a)"), Ok(Kind::Num));
        assert_eq!(k("m * 3"), Ok(Kind::Mass));
        assert_eq!(k("if(l > 1, a, a * 2)"), Ok(Kind::Area));
        assert_eq!(k("round(l, 2)"), Ok(Kind::Len));
        // a quantity as the second argument is the rounding step, not a count of decimals
        assert_eq!(k("round(a, a)"), Ok(Kind::Area));
        assert!(k("round(l, a)").is_err());
        assert_eq!(k("m / m"), Ok(Kind::Num));
        let m = k("m * m").unwrap_err();
        assert!(is_msg(&m, "schm.f.unit_unknown", &[("a", "kg^2")]), "{m}");
        assert_eq!(k("max(l, 5)"), Ok(Kind::Len));
        let m = k("l + a").unwrap_err();
        assert!(
            is_msg(&m, "schm.f.unit_mismatch", &[("a", "m^1"), ("b", "m^2")]),
            "{m}"
        );
        assert!(k("max(l, a)").is_err() && k("if(1, l, a)").is_err() && k("l > a").is_err());
        let m = k("sqrt(l)").unwrap_err();
        assert!(is_msg(&m, "schm.f.unit_root", &[("a", "m^1")]), "{m}");
        let m = k("v * v").unwrap_err();
        assert!(is_msg(&m, "schm.f.unit_unknown", &[("a", "m^6")]), "{m}");
    }

    /// One case per catalog error of the formula language.
    #[test]
    fn every_error_code_is_reported() {
        let r = num_row();
        let short = row(vec![Val::N(1.0)]);
        let case = |src: &str, key: &str, args: &[(&str, &str)]| {
            let m = fail(src, &r);
            assert!(is_msg(&m, key, args), "{src}: {m} is not {key}");
        };
        case("1.2.3", "sch.f.number", &[("t", "1.2.3")]);
        case("[abc", "sch.f.no_bracket", &[]);
        case("2 $ 3", "sch.f.symbol", &[("c", "$")]);
        case("(1", "sch.f.no_paren", &[]);
        case("max(1 2)", "sch.f.call_paren", &[("f", "max")]);
        case("round()", "sch.f.arg_count", &[("f", "round")]);
        case("if(1, 2)", "sch.f.arg_count", &[("f", "if")]);
        case("1 +", "sch.f.truncated", &[]);
        case("", "sch.f.empty", &[]);
        case("   ", "sch.f.empty", &[]);
        case("1 2", "sch.f.trailing", &[]);
        case("nothing", "sch.f.no_column", &[("n", "nothing")]);
        case("[No such]", "sch.f.no_column", &[("n", "No such")]);
        case("name * 2", "sch.f.not_number", &[("s", "a")]);
        case("len / 0", "sch.f.div0", &[]);
        case("len / (h - 3)", "sch.f.div0", &[]);
        case("sqrt(0 - 1)", "sch.f.sqrt_neg", &[]);
        case("foo(1)", "sch.f.no_function", &[("f", "foo")]);
        case("round(2.5, 0.5)", "schm.f.round_digits", &[]);
        case("1 & 2", "sch.f.symbol", &[("c", "&")]);
        case("]", "sch.f.symbol", &[("c", "]")]);
        let big = vec!["100000000000000"; 25].join("*");
        case(&big, "sch.f.not_finite", &[]);
        let max = MAX_LEN.to_string();
        case(&"1+".repeat(MAX_LEN), "sch.f.too_long", &[("max", &max)]);
        let d = MAX_DEPTH.to_string();
        case(
            &format!("{}1{}", "(".repeat(100), ")".repeat(100)),
            "sch.f.depth",
            &[("max", &d)],
        );
        case(
            &format!("{}1", "-".repeat(100)),
            "sch.f.depth",
            &[("max", &d)],
        );
        // a row shorter than the table
        let c = cols();
        let m = parse("h", &c).unwrap().eval(&short).unwrap_err();
        assert!(is_msg(&m, "sch.f.no_value", &[]), "{m}");
        // an operator where a value belongs is an error, not a panic
        assert!(!fail("* 2", &r).is_empty());
    }

    #[test]
    fn a_broken_column_shows_the_bad_mark_in_every_cell() {
        let mut tb = Table {
            cat: super::super::Cat::Walls,
            cols: cols(),
            rows: vec![row(vec![Val::N(2.0), Val::N(0.0), Val::T(String::new())])],
            row_phase: vec![None],
            phases: vec![],
        };
        let calcs = vec![
            Calc {
                name: "A".into(),
                expr: "len +".into(),
                kind: Kind::Num,
            },
            Calc {
                name: "B".into(),
                expr: "len / h".into(),
                kind: Kind::Num,
            },
        ];
        let errs = append(&mut tb, &calcs);
        assert!(errs[0].is_some() && errs[1].is_none());
        assert!(
            matches!(&tb.rows[0].vals[3], Val::T(t) if !t.is_empty()),
            "parse error: a mark"
        );
        assert!(
            matches!(&tb.rows[0].vals[4], Val::T(t) if t.starts_with('#')),
            "eval error"
        );
    }
}
