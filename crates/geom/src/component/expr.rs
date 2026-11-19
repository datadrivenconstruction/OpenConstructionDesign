//! Parameter formulas: numbers with units (mm cm m in ft ' " deg rad m2 m3), + - * /,
//! comparisons, and/or/not, functions min max if abs sqrt sin cos tan atan2 round floor ceil.
//! Dimensions are checked: a length cannot be added to a plain number, sin requires an angle or a number.
//!
//! Two dialects of one evaluator: components (units on numbers, `==` `!=`) and schedules
//! (`[Label]`, `;`, `=` `<>`, numbers without units that take the unit of their partner).

use std::fmt;

/// Dimension: powers of length, angle and mass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Dim {
    pub len: i8,
    pub ang: i8,
    pub mass: i8,
}

pub const NUMBER: Dim = Dim { len: 0, ang: 0, mass: 0 };
pub const LENGTH: Dim = Dim { len: 1, ang: 0, mass: 0 };
pub const AREA: Dim = Dim { len: 2, ang: 0, mass: 0 };
pub const VOLUME: Dim = Dim { len: 3, ang: 0, mass: 0 };
pub const ANGLE: Dim = Dim { len: 0, ang: 1, mass: 0 };
pub const MASS: Dim = Dim { len: 0, ang: 0, mass: 1 };

impl fmt::Display for Dim {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match (self.len, self.ang, self.mass) {
            (0, 0, 0) => write!(f, "number"),
            (1, 0, 0) => write!(f, "length"),
            (2, 0, 0) => write!(f, "area"),
            (3, 0, 0) => write!(f, "volume"),
            (0, 1, 0) => write!(f, "angle"),
            (0, 0, 1) => write!(f, "mass"),
            (l, a, 0) => write!(f, "length^{l}·angle^{a}"),
            (l, a, m) => write!(f, "length^{l}·angle^{a}·mass^{m}"),
        }
    }
}

/// Which formula language is read: the one of components or the one of schedules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Dialect {
    /// Numbers carry units (`1600mm`), equality is `==` and `!=`.
    #[default]
    Component,
    /// Names in `[brackets]`, `;` between arguments, `=` and `<>`, no units on numbers,
    /// function names in any case.
    Sheet,
}

/// What went wrong, apart from the English text: callers map the code to their own wording.
#[derive(Clone, Debug, PartialEq)]
pub enum Code {
    TooLong,
    Depth,
    Number(String),
    /// `[` without `]`.
    Bracket,
    /// `(` without `)`.
    Paren,
    /// A call whose arguments are not closed by `)`; carries the function name.
    CallParen(String),
    Truncated,
    /// Text left after a complete expression, starting with this character.
    Trailing(char),
    Unexpected(char),
    UnknownName(String),
    UnknownFunction(String),
    ArgCount(String),
    DivZero,
    NotFinite,
    SqrtNeg,
    /// Two operands of different dimensions.
    Mismatch { a: Dim, b: Dim },
    /// Square root of a value whose exponents are odd.
    Root(Dim),
    Condition(Dim),
    Trig(Dim),
    /// A rounding digit count that is not a whole number.
    RoundDigits,
    /// A rounding step that is zero or negative.
    RoundStep,
    /// Rounding a dimensioned value without a step.
    NeedsStep(Dim),
    Overflow,
    /// Error reported by the environment (a cell that is not a number, an empty group).
    Env(String),
}

/// An evaluation error: the code and the English text.
#[derive(Clone, Debug, PartialEq)]
pub struct Fail {
    pub code: Code,
    pub msg: String,
}

impl Fail {
    pub fn new(code: Code, msg: impl Into<String>) -> Self {
        Fail {
            code,
            msg: msg.into(),
        }
    }
    /// The environment refuses a name with a text of its own.
    pub fn env(msg: impl Into<String>) -> Self {
        let msg = msg.into();
        Fail::new(Code::Env(msg.clone()), msg)
    }
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

/// Quantity in SI (m, rad).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Q {
    pub v: f64,
    pub d: Dim,
}

pub const fn q(v: f64, d: Dim) -> Q {
    Q { v, d }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(Q),
    Var(String),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExprError {
    pub pos: usize,
    pub msg: String,
    pub code: Code,
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "at {}: {}", self.pos, self.msg)
    }
}

const UNITS: [(&str, f64, Dim); 12] = [
    ("mm2", 1e-6, AREA),
    ("mm3", 1e-9, VOLUME),
    ("mm", 1e-3, LENGTH),
    ("cm", 1e-2, LENGTH),
    ("m2", 1.0, AREA),
    ("m3", 1.0, VOLUME),
    ("m", 1.0, LENGTH),
    ("in", 0.0254, LENGTH),
    ("ft", 0.3048, LENGTH),
    ("deg", std::f64::consts::PI / 180.0, ANGLE),
    ("rad", 1.0, ANGLE),
    ("°", std::f64::consts::PI / 180.0, ANGLE),
];

struct P<'a> {
    s: &'a [char],
    i: usize,
    depth: usize,
    sheet: bool,
}

/// Safe parsing limits: nesting and text length. This is the one formula evaluator of the
/// product (dimensions, lazy if, division by zero and non-finite results are errors); schedule
/// formulas (app/schedule/formula.rs) are parsed and evaluated here too, in the `Sheet` dialect.
pub const MAX_DEPTH: usize = 64;
pub const MAX_LEN: usize = 1000;

impl P<'_> {
    fn err<T>(&self, code: Code, m: impl Into<String>) -> Result<T, ExprError> {
        Err(ExprError {
            pos: self.i,
            msg: m.into(),
            code,
        })
    }
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<char> {
        self.ws();
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, t: &str) -> bool {
        self.ws();
        let t: Vec<char> = t.chars().collect();
        if self.s[self.i..].starts_with(&t) {
            // a keyword operator must not be the start of an identifier
            let word = t[0].is_alphabetic();
            let next = self.s.get(self.i + t.len());
            if word && next.is_some_and(|c| c.is_alphanumeric() || *c == '_') {
                return false;
            }
            self.i += t.len();
            true
        } else {
            false
        }
    }
    fn or(&mut self) -> Result<Expr, ExprError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err(Code::Depth, format!("nesting deeper than {MAX_DEPTH}"));
        }
        let r = self.or_inner();
        self.depth -= 1;
        r
    }
    fn or_inner(&mut self) -> Result<Expr, ExprError> {
        let mut a = self.and()?;
        while self.eat("or") || self.eat("||") {
            a = Expr::Bin(Op::Or, a.into(), self.and()?.into());
        }
        Ok(a)
    }
    fn and(&mut self) -> Result<Expr, ExprError> {
        let mut a = self.cmp()?;
        while self.eat("and") || self.eat("&&") {
            a = Expr::Bin(Op::And, a.into(), self.cmp()?.into());
        }
        Ok(a)
    }
    fn cmp(&mut self) -> Result<Expr, ExprError> {
        let a = self.sum()?;
        let ops = [
            ("<=", Op::Le, false),
            (">=", Op::Ge, false),
            ("==", Op::Eq, false),
            ("!=", Op::Ne, false),
            ("<>", Op::Ne, true),
            ("<", Op::Lt, false),
            (">", Op::Gt, false),
            ("=", Op::Eq, true),
        ];
        for (t, op, sheet_only) in ops {
            if (self.sheet || !sheet_only) && self.eat(t) {
                return Ok(Expr::Bin(op, a.into(), self.sum()?.into()));
            }
        }
        Ok(a)
    }
    fn sum(&mut self) -> Result<Expr, ExprError> {
        let mut a = self.prod()?;
        loop {
            if self.eat("+") {
                a = Expr::Bin(Op::Add, a.into(), self.prod()?.into());
            } else if self.eat("-") {
                a = Expr::Bin(Op::Sub, a.into(), self.prod()?.into());
            } else {
                return Ok(a);
            }
        }
    }
    fn prod(&mut self) -> Result<Expr, ExprError> {
        let mut a = self.unary()?;
        loop {
            if self.eat("*") {
                a = Expr::Bin(Op::Mul, a.into(), self.unary()?.into());
            } else if self.eat("/") {
                a = Expr::Bin(Op::Div, a.into(), self.unary()?.into());
            } else {
                return Ok(a);
            }
        }
    }
    fn unary(&mut self) -> Result<Expr, ExprError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err(Code::Depth, format!("nesting deeper than {MAX_DEPTH}"));
        }
        let r = self.unary_inner();
        self.depth -= 1;
        r
    }
    fn unary_inner(&mut self) -> Result<Expr, ExprError> {
        if self.eat("-") {
            return Ok(Expr::Neg(self.unary()?.into()));
        }
        if self.eat("+") {
            return self.unary();
        }
        if self.eat("not") || self.eat("!") {
            return Ok(Expr::Not(self.unary()?.into()));
        }
        self.atom()
    }
    fn number(&mut self) -> Result<f64, ExprError> {
        let st = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == '.') {
            self.i += 1;
        }
        if self.i < self.s.len() && matches!(self.s[self.i], 'e' | 'E') {
            let save = self.i;
            self.i += 1;
            if self.i < self.s.len() && matches!(self.s[self.i], '+' | '-') {
                self.i += 1;
            }
            if self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                    self.i += 1;
                }
            } else {
                self.i = save;
            }
        }
        let t: String = self.s[st..self.i].iter().collect();
        t.parse::<f64>().or_else(|_| {
            self.i = st;
            self.err(Code::Number(t.clone()), format!("bad number '{t}'"))
        })
    }
    fn unit(&mut self) -> Option<(f64, Dim)> {
        match self.s.get(self.i) {
            Some('\'') => {
                self.i += 1;
                return Some((0.3048, LENGTH));
            }
            Some('"') => {
                self.i += 1;
                return Some((0.0254, LENGTH));
            }
            _ => {}
        }
        for (u, k, d) in UNITS {
            let t: Vec<char> = u.chars().collect();
            if self.s[self.i..].starts_with(&t) {
                let next = self.s.get(self.i + t.len());
                if next.is_some_and(|c| c.is_alphanumeric() || *c == '_') {
                    continue;
                }
                self.i += t.len();
                return Some((k, d));
            }
        }
        None
    }
    fn atom(&mut self) -> Result<Expr, ExprError> {
        let Some(c) = self.peek() else {
            return self.err(Code::Truncated, "unexpected end");
        };
        if c.is_ascii_digit() || c == '.' {
            let v = self.number()?;
            let unit = if self.sheet { None } else { self.unit() };
            let Some((k, d)) = unit else {
                return Ok(Expr::Num(q(v, NUMBER)));
            };
            let mut out = v * k;
            // 2'6" is feet with inches
            if k == 0.3048 && self.s.get(self.i).is_some_and(|c| c.is_ascii_digit()) {
                let save = self.i;
                let inch = self.number()?;
                if self.s.get(self.i) == Some(&'"') {
                    self.i += 1;
                    out += inch * 0.0254;
                } else {
                    self.i = save;
                }
            }
            return Ok(Expr::Num(q(out, d)));
        }
        if c == '(' {
            self.i += 1;
            let e = self.or()?;
            if !self.eat(")") {
                return self.err(Code::Paren, "expected ')'");
            }
            return Ok(e);
        }
        if self.sheet && c == '[' {
            let st = self.i + 1;
            let Some(len) = self.s[st..].iter().position(|&c| c == ']') else {
                return self.err(Code::Bracket, "expected ']'");
            };
            let name: String = self.s[st..st + len].iter().collect();
            self.i = st + len + 1;
            return Ok(Expr::Var(name.trim().to_string()));
        }
        if c.is_alphabetic() || c == '_' {
            let st = self.i;
            while self.i < self.s.len()
                && (self.s[self.i].is_alphanumeric() || self.s[self.i] == '_')
            {
                self.i += 1;
            }
            let name: String = self.s[st..self.i].iter().collect();
            match name.as_str() {
                "true" => return Ok(Expr::Num(q(1.0, NUMBER))),
                "false" => return Ok(Expr::Num(q(0.0, NUMBER))),
                "pi" => return Ok(Expr::Num(q(std::f64::consts::PI, NUMBER))),
                _ => {}
            }
            if self.peek() == Some('(') {
                self.i += 1;
                let name = if self.sheet { name.to_lowercase() } else { name };
                let mut args = vec![];
                if !self.eat(")") {
                    loop {
                        args.push(self.or()?);
                        if self.eat(")") {
                            break;
                        }
                        if !(self.eat(",") || (self.sheet && self.eat(";"))) {
                            let m = "expected ',' or ')'";
                            return self.err(Code::CallParen(name), m);
                        }
                    }
                }
                return Ok(Expr::Call(name, args));
            }
            return Ok(Expr::Var(name));
        }
        self.err(Code::Unexpected(c), format!("unexpected '{c}'"))
    }
}

pub fn parse(src: &str) -> Result<Expr, ExprError> {
    parse_in(src, Dialect::Component)
}

pub fn parse_in(src: &str, dialect: Dialect) -> Result<Expr, ExprError> {
    let s: Vec<char> = src.chars().collect();
    if s.len() > MAX_LEN {
        return Err(ExprError {
            pos: MAX_LEN,
            msg: format!("formula longer than {MAX_LEN} characters"),
            code: Code::TooLong,
        });
    }
    let mut p = P {
        s: &s,
        i: 0,
        depth: 0,
        sheet: dialect == Dialect::Sheet,
    };
    let e = p.or()?;
    if let Some(c) = p.peek() {
        return p.err(Code::Trailing(c), "unexpected text after expression");
    }
    Ok(e)
}

/// Fewest and most arguments of a function, `None` for a name that is not one.
pub fn arity(f: &str) -> Option<(usize, usize)> {
    Some(match f {
        "if" => (3, 3),
        "min" | "max" => (1, usize::MAX),
        "abs" | "sqrt" | "sin" | "cos" | "tan" => (1, 1),
        "atan2" => (2, 2),
        "round" | "floor" | "ceil" => (1, 2),
        _ => return None,
    })
}

/// A bare 0 fits any dimension: `poly(0,0, R,0, …)`.
pub fn zero_fits(v: Q, d: Dim) -> bool {
    v.d == d || (v.d == NUMBER && v.v == 0.0)
}

/// Operands fit when their dimensions are equal, a bare 0 stands for any dimension and, in
/// schedules (`lenient`), any dimensionless number takes the dimension of its partner.
fn fits(a: Q, b: Q, lenient: bool) -> bool {
    a.d == b.d
        || (lenient && (a.d == NUMBER || b.d == NUMBER))
        || zero_fits(a, b.d)
        || zero_fits(b, a.d)
}

fn same(a: Q, b: Q, what: &str, lenient: bool) -> Result<(), Fail> {
    if fits(a, b, lenient) {
        Ok(())
    } else {
        let code = Code::Mismatch { a: a.d, b: b.d };
        Err(Fail::new(code, format!("{what}: {} and {}", a.d, b.d)))
    }
}

/// Dimension of the result: a bare 0 (in schedules any dimensionless number) takes the one of
/// the second operand.
fn pick(a: Q, b: Q, lenient: bool) -> Dim {
    if a.d == NUMBER && (lenient || a.v == 0.0) {
        b.d
    } else {
        a.d
    }
}

fn add_dim(a: Dim, b: Dim, k: i8) -> Result<Dim, Fail> {
    let f = |x: i8, y: i8| x.checked_add(k.checked_mul(y)?);
    match (f(a.len, b.len), f(a.ang, b.ang), f(a.mass, b.mass)) {
        (Some(len), Some(ang), Some(mass)) => Ok(Dim { len, ang, mass }),
        _ => Err(Fail::new(Code::Overflow, "dimension exponent overflow")),
    }
}

fn truth(v: Q) -> Result<bool, Fail> {
    if v.d != NUMBER {
        let m = format!("condition must be a number or comparison, got {}", v.d);
        return Err(Fail::new(Code::Condition(v.d), m));
    }
    Ok(v.v != 0.0)
}

fn b(x: bool) -> Q {
    q(x as i32 as f64, NUMBER)
}

/// Comparison with a relative tolerance: formulas over decimal mm do not yield exact equality.
fn eq(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

impl Expr {
    /// Variable names of the expression (with repeats).
    pub fn vars(&self, out: &mut Vec<String>) {
        match self {
            Expr::Num(_) => {}
            Expr::Var(n) => out.push(n.clone()),
            Expr::Neg(e) | Expr::Not(e) => e.vars(out),
            Expr::Bin(_, a, b) => {
                a.vars(out);
                b.vars(out);
            }
            Expr::Call(_, a) => a.iter().for_each(|e| e.vars(out)),
        }
    }

    /// Evaluates with `env` giving the quantity of a name; the error is the English text.
    pub fn eval(&self, env: &dyn Fn(&str) -> Option<Q>) -> Result<Q, String> {
        let known = |n: &str| {
            env(n).ok_or_else(|| {
                Fail::new(Code::UnknownName(n.into()), format!("unknown name '{n}'"))
            })
        };
        let cx = Cx {
            env: &known,
            lenient: false,
            dry: false,
        };
        self.eval_with(&cx).map_err(|f| f.msg)
    }

    /// Evaluates in the context `cx`; the error carries a code.
    pub fn eval_with(&self, cx: &Cx) -> Result<Q, Fail> {
        ev(self, cx)
    }
}

/// How an expression is evaluated: where names come from and which rules apply.
pub struct Cx<'a> {
    /// The quantity of a name, or why it has none.
    pub env: &'a dyn Fn(&str) -> Result<Q, Fail>,
    /// A dimensionless number takes the dimension of its partner (schedules).
    pub lenient: bool,
    /// Only the dimension of the result matters: values never fail (division by zero, negative
    /// roots, a step of zero) and `env` may answer with any number of the right dimension.
    pub dry: bool,
}

fn ev(e: &Expr, cx: &Cx) -> Result<Q, Fail> {
    let r = match e {
        Expr::Num(v) => *v,
        Expr::Var(n) => (cx.env)(n)?,
        Expr::Neg(x) => {
            let v = ev(x, cx)?;
            q(-v.v, v.d)
        }
        Expr::Not(x) => b(!truth(ev(x, cx)?)?),
        Expr::Bin(op, x, y) => {
            let a = ev(x, cx)?;
            if matches!(op, Op::And | Op::Or) {
                let l = truth(a)?;
                if (*op == Op::And && !l) || (*op == Op::Or && l) {
                    return Ok(b(l));
                }
                return Ok(b(truth(ev(y, cx)?)?));
            }
            let c = ev(y, cx)?;
            match op {
                Op::Add | Op::Sub => {
                    same(a, c, "cannot add or subtract", cx.lenient)?;
                    let v = if *op == Op::Add { a.v + c.v } else { a.v - c.v };
                    q(v, pick(a, c, cx.lenient))
                }
                Op::Mul => q(a.v * c.v, add_dim(a.d, c.d, 1)?),
                Op::Div => {
                    let d = add_dim(a.d, c.d, -1)?;
                    if c.v == 0.0 {
                        if !cx.dry {
                            return Err(Fail::new(Code::DivZero, "division by zero"));
                        }
                        q(a.v, d)
                    } else {
                        q(a.v / c.v, d)
                    }
                }
                _ => {
                    same(a, c, "cannot compare", cx.lenient)?;
                    b(match op {
                        Op::Lt => a.v < c.v && !eq(a.v, c.v),
                        Op::Le => a.v < c.v || eq(a.v, c.v),
                        Op::Gt => a.v > c.v && !eq(a.v, c.v),
                        Op::Ge => a.v > c.v || eq(a.v, c.v),
                        Op::Eq => eq(a.v, c.v),
                        _ => !eq(a.v, c.v),
                    })
                }
            }
        }
        Expr::Call(f, args) => call(f, args, cx)?,
    };
    if !cx.dry && !r.v.is_finite() {
        return Err(Fail::new(Code::NotFinite, "result is not finite"));
    }
    Ok(r)
}

/// Rounds `k` to a whole number; a value within 1e-9 of one is that number (0.1/0.01 is a hair
/// under 10 in binary and would otherwise round down).
fn whole(f: &str, k: f64) -> f64 {
    if (k - k.round()).abs() < 1e-9 {
        return k.round();
    }
    match f {
        "round" => k.round(),
        "floor" => k.floor(),
        _ => k.ceil(),
    }
}

fn call(f: &str, args: &[Expr], cx: &Cx) -> Result<Q, Fail> {
    let n = |k: usize| -> Result<(), Fail> {
        if args.len() == k {
            Ok(())
        } else {
            let m = format!("{f}() takes {k} argument(s)");
            Err(Fail::new(Code::ArgCount(f.into()), m))
        }
    };
    let ev = |i: usize| ev(&args[i], cx);
    let lenient = cx.lenient;
    match f {
        "if" => {
            n(3)?;
            // branches are lazy: an unselected one (L/w with w=0) must not fail the formula; its dimension
            // is checked only if it is evaluated
            let c = truth(ev(0)?)?;
            let (pi, oi) = if c { (1, 2) } else { (2, 1) };
            let v = ev(pi)?;
            if let Ok(o) = ev(oi) {
                same(v, o, "if() branches differ", lenient)?;
                return Ok(q(v.v, pick(v, o, lenient)));
            }
            Ok(v)
        }
        "min" | "max" => {
            if args.is_empty() {
                let m = format!("{f}() needs arguments");
                return Err(Fail::new(Code::ArgCount(f.into()), m));
            }
            let mut acc = ev(0)?;
            for i in 1..args.len() {
                let v = ev(i)?;
                same(acc, v, "min/max arguments", lenient)?;
                let d = pick(acc, v, lenient);
                if (f == "min") == (v.v < acc.v) {
                    acc = v;
                }
                acc.d = d;
            }
            Ok(acc)
        }
        "abs" => {
            n(1)?;
            let v = ev(0)?;
            Ok(q(v.v.abs(), v.d))
        }
        "sqrt" => {
            n(1)?;
            let v = ev(0)?;
            let m = format!("sqrt() of {} {}", v.d, v.v);
            let d = v.d;
            if d.len % 2 != 0 || d.ang % 2 != 0 || d.mass % 2 != 0 {
                return Err(Fail::new(Code::Root(d), m));
            }
            if v.v < 0.0 && !cx.dry {
                return Err(Fail::new(Code::SqrtNeg, m));
            }
            let half = Dim {
                len: d.len / 2,
                ang: d.ang / 2,
                mass: d.mass / 2,
            };
            Ok(q(v.v.abs().sqrt(), half))
        }
        "sin" | "cos" | "tan" => {
            n(1)?;
            let v = ev(0)?;
            if v.d != ANGLE && v.d != NUMBER {
                return Err(Fail::new(Code::Trig(v.d), format!("{f}() of {}", v.d)));
            }
            let r = match f {
                "sin" => v.v.sin(),
                "cos" => v.v.cos(),
                _ => v.v.tan(),
            };
            Ok(q(r, NUMBER))
        }
        "atan2" => {
            n(2)?;
            let (y, x) = (ev(0)?, ev(1)?);
            same(y, x, "atan2 arguments", lenient)?;
            Ok(q(y.v.atan2(x.v), ANGLE))
        }
        "round" | "floor" | "ceil" => {
            if !(1..=2).contains(&args.len()) {
                let m = format!("{f}() takes 1 or 2 arguments");
                return Err(Fail::new(Code::ArgCount(f.into()), m));
            }
            let v = ev(0)?;
            if args.len() == 1 {
                if v.d != NUMBER {
                    let m = format!("{f}() of {} needs a step: {f}(x, 10mm)", v.d);
                    return Err(Fail::new(Code::NeedsStep(v.d), m));
                }
                return Ok(q(whole(f, v.v), NUMBER));
            }
            let s = ev(1)?;
            if s.d == NUMBER {
                // a dimensionless second argument is the number of decimals
                if (s.v - s.v.round()).abs() > 1e-9 && !cx.dry {
                    let m = "rounding digits must be a whole number";
                    return Err(Fail::new(Code::RoundDigits, m));
                }
                let k = 10f64.powi(s.v.round().clamp(-9.0, 9.0) as i32);
                return Ok(q(whole(f, v.v * k) / k, v.d));
            }
            // a quantity is the step
            same(v, s, "rounding step", lenient)?;
            if s.v <= 0.0 {
                if !cx.dry {
                    return Err(Fail::new(Code::RoundStep, "rounding step must be positive"));
                }
                return Ok(v);
            }
            Ok(q(whole(f, v.v / s.v) * s.v, v.d))
        }
        _ => {
            let m = format!("unknown function {f}()");
            Err(Fail::new(Code::UnknownFunction(f.into()), m))
        }
    }
}

/// Quantity text that `parse` reads back to the same f64.
pub fn literal(v: Q) -> String {
    let u = match (v.d.len, v.d.ang) {
        (0, 0) => "",
        (1, 0) => "m",
        (2, 0) => "m2",
        (3, 0) => "m3",
        (0, 1) => "rad",
        _ => "",
    };
    let s = format!("{:?}", v.v.abs());
    let s = if s.contains('e') {
        format!("{:.17e}", v.v.abs())
    } else {
        s
    };
    if v.v < 0.0 {
        format!("-{s}{u}")
    } else {
        format!("{s}{u}")
    }
}
