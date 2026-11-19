// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Check kinds of country packs that read the plan as a whole: `formula` (2R+T), `ratio`
//! (window/floor, circulation share), `adjacency`, `stack` (wet rooms over wet rooms),
//! `distance` (travel to an exit through doors) and `programme` (rooms against the pack's
//! programme variant). They run on `PlanFacts`, plain numbers taken from the model, so a
//! generator can be judged by the same code before it writes anything.

use std::collections::BTreeMap;

use serde_json::Value as J;

use super::norm_pack::{NormPack, PackCheck, PackRule, Programme};
use super::norms::run_norms;
use super::util::Within;
use super::{CheckScope, Severity};
use crate::{ElKey, Error, Model};

// ---------- expressions ----------

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(f64),
    Var(String),
    Neg(Box<Expr>),
    Bin(char, Box<Expr>, Box<Expr>),
}

impl Expr {
    /// `+ - * /`, parentheses, numbers and names: `2*riser+tread`.
    pub fn parse(src: &str) -> Result<Expr, String> {
        let toks = lex(src)?;
        let mut p = 0;
        let e = sum(&toks, &mut p)?;
        if p != toks.len() {
            return Err(format!("unexpected {:?} in {src:?}", toks[p]));
        }
        Ok(e)
    }

    pub fn eval(&self, get: &dyn Fn(&str) -> Option<f64>) -> Option<f64> {
        let v = match self {
            Expr::Num(x) => *x,
            Expr::Var(n) => get(n)?,
            Expr::Neg(e) => -e.eval(get)?,
            Expr::Bin(op, a, b) => {
                let (a, b) = (a.eval(get)?, b.eval(get)?);
                match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    _ if b == 0.0 => return None,
                    _ => a / b,
                }
            }
        };
        v.is_finite().then_some(v)
    }

    pub fn vars(&self, out: &mut Vec<String>) {
        match self {
            Expr::Var(n) => out.push(n.clone()),
            Expr::Neg(e) => e.vars(out),
            Expr::Bin(_, a, b) => {
                a.vars(out);
                b.vars(out);
            }
            Expr::Num(_) => {}
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Id(String),
    Op(char),
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = vec![];
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || c == '.' {
            let st = i;
            while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
                i += 1;
            }
            let t: String = cs[st..i].iter().collect();
            out.push(Tok::Num(t.parse().map_err(|_| format!("bad number {t:?}"))?));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let st = i;
            while i < cs.len() && (cs[i].is_ascii_alphanumeric() || cs[i] == '_') {
                i += 1;
            }
            out.push(Tok::Id(cs[st..i].iter().collect()));
        } else if "+-*/()".contains(c) {
            out.push(Tok::Op(c));
            i += 1;
        } else {
            return Err(format!("unexpected {c:?} in {s:?}"));
        }
    }
    Ok(out)
}

fn sum(t: &[Tok], p: &mut usize) -> Result<Expr, String> {
    let mut a = prod(t, p)?;
    while let Some(Tok::Op(o @ ('+' | '-'))) = t.get(*p) {
        *p += 1;
        a = Expr::Bin(*o, Box::new(a), Box::new(prod(t, p)?));
    }
    Ok(a)
}

fn prod(t: &[Tok], p: &mut usize) -> Result<Expr, String> {
    let mut a = atom(t, p)?;
    while let Some(Tok::Op(o @ ('*' | '/'))) = t.get(*p) {
        *p += 1;
        a = Expr::Bin(*o, Box::new(a), Box::new(atom(t, p)?));
    }
    Ok(a)
}

fn atom(t: &[Tok], p: &mut usize) -> Result<Expr, String> {
    let tok = t.get(*p).cloned().ok_or("expression ends early")?;
    *p += 1;
    match tok {
        Tok::Num(x) => Ok(Expr::Num(x)),
        Tok::Id(n) => Ok(Expr::Var(n)),
        Tok::Op('-') => Ok(Expr::Neg(Box::new(atom(t, p)?))),
        Tok::Op('(') => {
            let e = sum(t, p)?;
            if t.get(*p) != Some(&Tok::Op(')')) {
                return Err("missing )".into());
            }
            *p += 1;
            Ok(e)
        }
        Tok::Op(c) => Err(format!("unexpected {c:?}")),
    }
}

// ---------- check kinds ----------

#[derive(Clone, Debug, PartialEq)]
pub struct Agg {
    /// Room field summed (default `area`).
    pub field: String,
    /// Room kinds counted; empty: every room.
    pub room_kind: Vec<String>,
    /// `(field, op, value)` all of which hold.
    pub when: Vec<(String, String, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NormKind {
    Formula {
        cat: String,
        expr: Expr,
        min: Option<f64>,
        max: Option<f64>,
    },
    /// `num / den` of one element (`per: element`, fields) or of a group's sums.
    RatioElement {
        cat: String,
        room_kind: Vec<String>,
        num: String,
        den: String,
        min: Option<f64>,
        max: Option<f64>,
    },
    RatioTotal {
        group_by: String,
        num: Agg,
        den: Agg,
        min: Option<f64>,
        max: Option<f64>,
    },
    Adjacency {
        require: Vec<(String, Vec<String>)>,
        forbid: Vec<(String, String)>,
    },
    Stack {
        room_kind: Vec<String>,
    },
    Distance {
        max: f64,
        exit_kinds: Vec<String>,
    },
    Programme {
        typology: String,
    },
}

fn strs(v: &J) -> Vec<String> {
    match v {
        J::String(x) => vec![x.clone()],
        J::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => vec![],
    }
}

fn bounds(c: &J) -> Result<(Option<f64>, Option<f64>), String> {
    let (min, max) = (c["min"].as_f64(), c["max"].as_f64());
    if min.is_none() && max.is_none() {
        return Err("`min` or `max` is required".into());
    }
    if let (Some(a), Some(b)) = (min, max) {
        if a > b {
            return Err("`min` is above `max`".into());
        }
    }
    Ok((min, max))
}

const OPS: [&str; 6] = ["<", "<=", ">", ">=", "=", "!="];

fn agg(v: &J) -> Result<Agg, String> {
    let mut when = vec![];
    for w in v["where"].as_array().cloned().unwrap_or_default() {
        let op = w["op"].as_str().unwrap_or("");
        if !OPS.contains(&op) {
            return Err(format!("where: op {op:?} is not one of {OPS:?}"));
        }
        let f = w["field"].as_str().ok_or("where: `field`")?;
        if !ROOM_FIELDS.contains(&f) {
            return Err(format!("where: unknown room field {f:?}"));
        }
        when.push((f.to_string(), op.to_string(), w["value"].as_f64().ok_or("where: `value`")?));
    }
    let field = v["field"].as_str().unwrap_or("area").to_string();
    if !ROOM_FIELDS.contains(&field.as_str()) {
        return Err(format!("unknown room field {field:?}"));
    }
    Ok(Agg {
        field,
        room_kind: strs(&v["room_kind"]),
        when,
    })
}

/// Fields a room offers to formulas and ratios.
pub const ROOM_FIELDS: [&str; 5] = ["area", "height", "width", "window_area", "storey"];
/// Fields a stair offers.
/// `headroom`: least clear height above the pitch line to the underside of a floor slab over it;
/// a stair with no slab over its flights has none and is not judged by a headroom rule.
pub const STAIR_FIELDS: [&str; 6] = ["riser", "tread", "risers", "width", "landing", "headroom"];
/// Fields a door offers (leaf opening, m).
pub const DOOR_FIELDS: [&str; 2] = ["width", "height"];
/// Fields a railing (guard) offers.
pub const RAILING_FIELDS: [&str; 1] = ["height"];

fn fields_of(cat: &str) -> Result<&'static [&'static str], String> {
    match cat {
        "room" => Ok(&ROOM_FIELDS),
        "stair" => Ok(&STAIR_FIELDS),
        "door" => Ok(&DOOR_FIELDS),
        "railing" => Ok(&RAILING_FIELDS),
        _ => Err(format!("cat {cat:?}: only room, stair, door and railing")),
    }
}

/// Element fields a fix may set straight to the limit: the rest are results of other inputs.
pub fn settable(cat: &str, field: &str) -> bool {
    matches!((cat, field), ("door", "width" | "height") | ("railing", "height") | ("stair", "width"))
}

/// Reads a `check` object of a new kind; every name it uses must exist.
pub fn parse_kind(kind: &str, c: &J) -> Result<NormKind, String> {
    match kind {
        "formula" => {
            let cat = c["cat"].as_str().unwrap_or("").to_string();
            let fields = fields_of(&cat)?;
            let expr = Expr::parse(c["expr"].as_str().ok_or("`expr` is required")?)?;
            let mut vars = vec![];
            expr.vars(&mut vars);
            if let Some(v) = vars.iter().find(|v| !fields.contains(&v.as_str())) {
                return Err(format!("unknown {cat} field {v:?}"));
            }
            let (min, max) = bounds(c)?;
            Ok(NormKind::Formula { cat, expr, min, max })
        }
        "ratio" => {
            let (min, max) = bounds(c)?;
            match c["per"].as_str().unwrap_or("element") {
                "element" => {
                    let cat = c["cat"].as_str().unwrap_or("room").to_string();
                    let fields = fields_of(&cat)?;
                    let (num, den) = (
                        c["num"].as_str().unwrap_or("").to_string(),
                        c["den"].as_str().unwrap_or("").to_string(),
                    );
                    for f in [&num, &den] {
                        if !fields.contains(&f.as_str()) {
                            return Err(format!("unknown {cat} field {f:?}"));
                        }
                    }
                    Ok(NormKind::RatioElement {
                        cat,
                        room_kind: strs(&c["room_kind"]),
                        num,
                        den,
                        min,
                        max,
                    })
                }
                "total" => {
                    let group_by = c["group_by"].as_str().unwrap_or("model").to_string();
                    if !["model", "level", "unit"].contains(&group_by.as_str()) {
                        return Err("group_by must be model, level or unit".into());
                    }
                    Ok(NormKind::RatioTotal {
                        group_by,
                        num: agg(&c["num"])?,
                        den: agg(&c["den"])?,
                        min,
                        max,
                    })
                }
                p => Err(format!("per {p:?} must be element or total")),
            }
        }
        "adjacency" => {
            let mut require = vec![];
            for r in c["require"].as_array().cloned().unwrap_or_default() {
                let a = r["a"].as_str().ok_or("require: `a`")?.to_string();
                let b = strs(&r["b"]);
                if b.is_empty() {
                    return Err("require: `b` is empty".into());
                }
                require.push((a, b));
            }
            let mut forbid = vec![];
            for f in c["forbid"].as_array().cloned().unwrap_or_default() {
                let p = strs(&f);
                if p.len() != 2 {
                    return Err("forbid: pairs of two kinds".into());
                }
                forbid.push((p[0].clone(), p[1].clone()));
            }
            if require.is_empty() && forbid.is_empty() {
                return Err("`require` or `forbid` is required".into());
            }
            Ok(NormKind::Adjacency { require, forbid })
        }
        "stack" => {
            let room_kind = strs(&c["room_kind"]);
            if room_kind.is_empty() {
                return Err("`room_kind` is required".into());
            }
            Ok(NormKind::Stack { room_kind })
        }
        "distance" => {
            let max = c["max"].as_f64().filter(|m| *m > 0.0).ok_or("`max` > 0 is required")?;
            Ok(NormKind::Distance {
                max,
                exit_kinds: strs(&c["exit_kinds"]),
            })
        }
        "programme" => Ok(NormKind::Programme {
            typology: c["typology"].as_str().ok_or("`typology` is required")?.to_string(),
        }),
        k => Err(format!("unknown kind {k:?}")),
    }
}

// ---------- plan facts ----------

#[derive(Clone, Debug, PartialEq)]
pub struct FactRoom {
    pub key: ElKey,
    pub level: i64,
    /// 0 for the lowest level, by elevation.
    pub storey: usize,
    pub name: String,
    /// Pack room kind, empty when unknown.
    pub kind: String,
    /// Dwelling mark (`Unit` parameter), if any.
    pub unit: Option<String>,
    pub area: f64,
    pub height: f64,
    pub width: Option<f64>,
    pub window_area: f64,
    pub centroid: (f64, f64),
    /// x0, y0, x1, y1.
    pub bbox: [f64; 4],
}

impl FactRoom {
    pub fn field(&self, f: &str) -> Option<f64> {
        match f {
            "area" => Some(self.area),
            "height" => Some(self.height),
            "width" => self.width,
            "window_area" => Some(self.window_area),
            "storey" => Some(self.storey as f64),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FactDoor {
    pub key: ElKey,
    pub at: (f64, f64),
    /// Indices into `rooms`: one for an outer door, two between rooms.
    pub rooms: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FactStair {
    pub key: ElKey,
    pub vals: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanFacts {
    pub rooms: Vec<FactRoom>,
    pub doors: Vec<FactDoor>,
    pub stairs: Vec<FactStair>,
    /// Every door with its size, hosted or not (`doors` holds only those between rooms).
    pub door_sizes: Vec<FactStair>,
    pub railings: Vec<FactStair>,
}

impl PlanFacts {
    /// Elements of a formula category other than rooms.
    fn els(&self, cat: &str) -> Option<&[FactStair]> {
        match cat {
            "stair" => Some(&self.stairs),
            "door" => Some(&self.door_sizes),
            "railing" => Some(&self.railings),
            _ => None,
        }
    }
}

/// Slabs as (underside z, outer ring, holes), absolute elevations.
fn slab_undersides(db: &rusqlite::Connection) -> Result<Vec<(f64, Vec<(f64, f64)>, Vec<Vec<(f64, f64)>>)>, Error> {
    let mut st = db.prepare(
        "SELECT s.id, l.elevation + s.offset - COALESCE((SELECT SUM(thickness) FROM slab_type_layer \
         WHERE stype=s.stype), 0) FROM slab s JOIN level l ON l.id=s.level ORDER BY s.id",
    )?;
    let slabs = st
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut vs = db.prepare("SELECT ring, x, y FROM slab_vertex WHERE slab=?1 ORDER BY ring, idx")?;
    let mut out = vec![];
    for (id, z) in slabs {
        let mut rings: BTreeMap<i64, Vec<(f64, f64)>> = BTreeMap::new();
        for row in vs.query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?)))? {
            let (ring, x, y) = row?;
            rings.entry(ring).or_default().push((x, y));
        }
        let outer = rings.remove(&0).unwrap_or_default();
        if outer.len() >= 3 {
            out.push((z, outer, rings.into_values().collect()));
        }
    }
    Ok(out)
}

/// Least clear height over the flights' pitch line (nosings, mid-width) to a slab underside
/// above it; `None` when no slab is over any tread.
pub fn stair_headroom(
    st: &ddc_geom::stair::Stair,
    slabs: &[(f64, Vec<(f64, f64)>, Vec<Vec<(f64, f64)>>)],
) -> Option<f64> {
    let mut best: Option<f64> = None;
    for f in &st.flights {
        let mid = f.start + f.right() * (f.width / 2.0);
        for i in 0..f.treads() {
            let along = (i as f64 + 0.5) * st.tread;
            let p = mid + f.dir * along;
            let z = f.z0 + (i + 1) as f64 * st.riser;
            for (under, outer, holes) in slabs {
                let pt = (p.x, p.y);
                if *under > z + 1e-6 && inside(pt, outer) && !holes.iter().any(|h| inside(pt, h)) {
                    let h = under - z;
                    best = Some(best.map_or(h, |b: f64| b.min(h)));
                }
            }
        }
    }
    best
}

fn inside(p: (f64, f64), ring: &[(f64, f64)]) -> bool {
    let mut c = false;
    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + n - 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            c = !c;
        }
    }
    c
}

fn centroid(ring: &[(f64, f64)]) -> Option<(f64, f64)> {
    let (mut a, mut cx, mut cy) = (0.0, 0.0, 0.0);
    let n = ring.len();
    for i in 0..n {
        let (p, q) = (ring[i], ring[(i + 1) % n]);
        let cr = p.0 * q.1 - q.0 * p.1;
        a += cr;
        cx += (p.0 + q.0) * cr;
        cy += (p.1 + q.1) * cr;
    }
    (a.abs() > 1e-12).then(|| (cx / (3.0 * a), cy / (3.0 * a)))
}

impl PlanFacts {
    /// Rooms with an area (closed by walls), doors and windows found on either side of their
    /// wall, stairs with their computed steps. Room kind: `RoomKind` text parameter, else
    /// `Purpose` through the pack, else the name through the pack vocabulary.
    pub fn from_model(m: &Model, pack: &NormPack) -> Result<PlanFacts, Error> {
        let db = &m.db;
        let mut storey: BTreeMap<i64, usize> = BTreeMap::new();
        {
            let mut st = db.prepare("SELECT id FROM level ORDER BY elevation, id")?;
            let ids = st.query_map([], |r| r.get::<_, i64>(0))?;
            for (i, id) in ids.enumerate() {
                storey.insert(id?, i);
            }
        }
        let rows: Vec<(i64, i64, String, f64, f64, f64)> = {
            let mut st = db.prepare("SELECT id, level, name, height, px, py FROM room ORDER BY id")?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            v
        };
        let mut f = PlanFacts::default();
        let mut rings: Vec<Vec<Vec<(f64, f64)>>> = vec![];
        for (id, level, name, height, px, py) in rows {
            let Some(q) = m.room_quantities(id)? else {
                continue;
            };
            let outer = q.rings.first().cloned().unwrap_or_default();
            let (mut kind, mut unit) = (String::new(), None);
            let mut purpose = None;
            for (k, v, _) in m.props("room", id)? {
                let text = match v {
                    rusqlite::types::Value::Text(t) => t,
                    rusqlite::types::Value::Integer(i) => i.to_string(),
                    _ => continue,
                };
                match k.as_str() {
                    "RoomKind" => kind = text.trim().to_lowercase(),
                    "Unit" | "Apartment" if !text.trim().is_empty() => unit = Some(text),
                    "Purpose" => purpose = Some(text),
                    _ => {}
                }
            }
            if !pack.room_kinds.contains_key(&kind) {
                kind = purpose
                    .and_then(|p| pack.purpose.get(p.trim()).cloned())
                    .or_else(|| pack.kind_of_name(&name).map(String::from))
                    .unwrap_or_default();
            }
            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for p in &outer {
                x0 = x0.min(p.0);
                y0 = y0.min(p.1);
                x1 = x1.max(p.0);
                y1 = y1.max(p.1);
            }
            f.rooms.push(FactRoom {
                key: ("room".to_string(), id),
                level,
                storey: storey.get(&level).copied().unwrap_or(0),
                name,
                kind,
                unit,
                area: q.area,
                height,
                width: super::rule_cases::ring_min_width(&outer),
                window_area: 0.0,
                centroid: centroid(&outer).unwrap_or((px, py)),
                bbox: [x0, y0, x1, y1],
            });
            rings.push(q.rings);
        }
        let room_at = |level: i64, p: (f64, f64)| -> Option<usize> {
            f.rooms.iter().enumerate().position(|(i, r)| {
                r.level == level
                    && rings[i].first().is_some_and(|o| inside(p, o))
                    && !rings[i].iter().skip(1).any(|h| inside(p, h))
            })
        };
        let ops: Vec<(i64, String, f64, f64, f64, i64, [f64; 4], f64)> = {
            let mut st = db.prepare(
                "SELECT o.id, o.kind, o.along, o.width, o.height, w.level, w.ax, w.ay, w.bx, w.by, \
                 COALESCE((SELECT SUM(thickness) FROM wall_type_layer WHERE wtype=w.wtype), 0.2) \
                 FROM opening o JOIN wall w ON w.id=o.wall ORDER BY o.id",
            )?;
            let v = st
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        [r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?],
                        r.get(10)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            v
        };
        let mut windows: Vec<(usize, f64)> = vec![];
        let mut doors = vec![];
        for (id, kind, along, width, height, level, w, t) in ops {
            let (dx, dy) = (w[2] - w[0], w[3] - w[1]);
            let len = (dx * dx + dy * dy).sqrt();
            if len < 1e-9 {
                continue;
            }
            let (ux, uy) = (dx / len, dy / len);
            let s = along + width / 2.0;
            let c = (w[0] + ux * s, w[1] + uy * s);
            let off = t / 2.0 + 0.05;
            let sides = [(c.0 - uy * off, c.1 + ux * off), (c.0 + uy * off, c.1 - ux * off)];
            let mut rooms: Vec<usize> = sides.iter().filter_map(|p| room_at(level, *p)).collect();
            rooms.dedup();
            match kind.as_str() {
                "window" => {
                    for r in rooms {
                        windows.push((r, width * height));
                    }
                }
                "door" if !rooms.is_empty() => doors.push(FactDoor {
                    key: ("opening".to_string(), id),
                    at: c,
                    rooms,
                }),
                _ => {}
            }
        }
        for (r, a) in windows {
            f.rooms[r].window_area += a;
        }
        f.doors = doors;
        let mut st = db.prepare(
            "SELECT s.id, s.width, s.landing, q.riser, q.tread, q.risers FROM stair s \
             LEFT JOIN stair_qty q ON q.stair=s.id ORDER BY s.id",
        )?;
        let stairs = st
            .query_map([], |r| {
                let mut vals = BTreeMap::new();
                for (i, k) in ["width", "landing", "riser", "tread", "risers"].iter().enumerate() {
                    if let Some(x) = r.get::<_, Option<f64>>(i + 1)? {
                        vals.insert(k.to_string(), x);
                    }
                }
                Ok(FactStair {
                    key: ("stair".to_string(), r.get(0)?),
                    vals,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        f.stairs = stairs;
        let slabs = slab_undersides(db)?;
        if !slabs.is_empty() {
            for s in &mut f.stairs {
                // a stair whose layout fails has no steps to stand on: its other rules say so
                if let Some(h) = m.stair_layout(s.key.1).ok().and_then(|l| stair_headroom(&l, &slabs)) {
                    s.vals.insert("headroom".into(), h);
                }
            }
        }
        let mut st = db.prepare("SELECT id, width, height FROM opening WHERE kind='door' ORDER BY id")?;
        f.door_sizes = st
            .query_map([], |r| {
                let vals = BTreeMap::from([("width".to_string(), r.get(1)?), ("height".to_string(), r.get(2)?)]);
                Ok(FactStair { key: ("opening".to_string(), r.get(0)?), vals })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut st = db.prepare("SELECT id, height FROM railing ORDER BY id")?;
        f.railings = st
            .query_map([], |r| {
                let vals = BTreeMap::from([("height".to_string(), r.get(1)?)]);
                Ok(FactStair { key: ("railing".to_string(), r.get(0)?), vals })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(f)
    }
}

// ---------- evaluation ----------

/// One result of a new kind, before the rule's severity and verification are applied.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub elements: Vec<ElKey>,
    pub measured: Option<f64>,
    pub required: Option<f64>,
    /// `min`, `max`, `no_value`, `missing_room`, `room_area`, `area_min`, `foreign_room`,
    /// `no_variant`, `storage_share`, `circulation_share`, `adjacency_missing`,
    /// `adjacency_forbidden`, `not_stacked`, `no_route`.
    pub what: &'static str,
    pub detail: String,
    /// The numbers behind this hit were checked against the source.
    pub verified: bool,
}

fn hit(elements: Vec<ElKey>, measured: Option<f64>, required: Option<f64>, what: &'static str) -> Hit {
    Hit {
        elements,
        measured,
        required,
        what,
        detail: String::new(),
        verified: true,
    }
}

const EPS: f64 = 1e-9;

fn range_hits(el: Vec<ElKey>, v: f64, min: Option<f64>, max: Option<f64>) -> Option<Hit> {
    if let Some(lo) = min.filter(|lo| v < lo - EPS) {
        return Some(hit(el, Some(v), Some(lo), "min"));
    }
    if let Some(hi) = max.filter(|hi| v > hi + EPS) {
        return Some(hit(el, Some(v), Some(hi), "max"));
    }
    None
}

fn kind_in(k: &str, list: &[String]) -> bool {
    list.is_empty() || list.iter().any(|x| x == k)
}

fn op_holds(a: f64, op: &str, b: f64) -> bool {
    match op {
        "<" => a < b,
        "<=" => a <= b,
        ">" => a > b,
        ">=" => a >= b,
        "=" => (a - b).abs() < EPS,
        _ => (a - b).abs() >= EPS,
    }
}

fn agg_sum(a: &Agg, rooms: &[&FactRoom]) -> (f64, Vec<ElKey>) {
    let mut sum = 0.0;
    let mut els = vec![];
    for r in rooms {
        if !kind_in(&r.kind, &a.room_kind) {
            continue;
        }
        if !a.when.iter().all(|(f, op, v)| r.field(f).is_some_and(|x| op_holds(x, op, *v))) {
            continue;
        }
        if let Some(x) = r.field(&a.field) {
            sum += x;
            els.push(r.key.clone());
        }
    }
    (sum, els)
}

/// Groups of rooms: one for the model, one per level, or one per dwelling mark (rooms
/// without a mark are common parts and left out; with no marks at all the model is one
/// dwelling).
fn groups<'a>(f: &'a PlanFacts, by: &str) -> Vec<Vec<&'a FactRoom>> {
    let mut g: BTreeMap<String, Vec<&FactRoom>> = BTreeMap::new();
    let any_unit = f.rooms.iter().any(|r| r.unit.is_some());
    for r in &f.rooms {
        let k = match by {
            "level" => format!("{:08}", r.level),
            "unit" if any_unit => match &r.unit {
                Some(u) => u.clone(),
                None => continue,
            },
            _ => String::new(),
        };
        g.entry(k).or_default().push(r);
    }
    g.into_values().collect()
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Shortest travel from each room's centroid to an exit through doors: centroid to door and
/// door to door inside a room are straight lines. Exits are outer doors on the lowest storey
/// and rooms of `exit_kinds` (with their doors). `None`: no route.
pub fn travel_distances(f: &PlanFacts, exit_kinds: &[String]) -> Vec<Option<f64>> {
    let nr = f.rooms.len();
    let n = nr + f.doors.len();
    let pos = |i: usize| if i < nr { f.rooms[i].centroid } else { f.doors[i - nr].at };
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
    let mut link = |a: usize, b: usize, adj: &mut Vec<Vec<usize>>| {
        adj[a].push(b);
        adj[b].push(a);
    };
    let mut doors_of: Vec<Vec<usize>> = vec![vec![]; nr];
    for (d, door) in f.doors.iter().enumerate() {
        for r in &door.rooms {
            link(*r, nr + d, &mut adj);
            doors_of[*r].push(nr + d);
        }
    }
    for ds in &doors_of {
        for (i, a) in ds.iter().enumerate() {
            for b in &ds[i + 1..] {
                link(*a, *b, &mut adj);
            }
        }
    }
    let low = f.rooms.iter().map(|r| r.storey).min().unwrap_or(0);
    let mut best = vec![f64::INFINITY; n];
    for (d, door) in f.doors.iter().enumerate() {
        if door.rooms.len() == 1 && f.rooms[door.rooms[0]].storey == low {
            best[nr + d] = 0.0;
        }
    }
    for (i, r) in f.rooms.iter().enumerate() {
        if exit_kinds.iter().any(|k| *k == r.kind) {
            best[i] = 0.0;
            for d in &doors_of[i] {
                best[*d] = 0.0;
            }
        }
    }
    let mut done = vec![false; n];
    loop {
        let next = (0..n)
            .filter(|i| !done[*i] && best[*i].is_finite())
            .min_by(|a, b| best[*a].total_cmp(&best[*b]));
        let Some(u) = next else { break };
        done[u] = true;
        for v in adj[u].clone() {
            let c = best[u] + dist(pos(u), pos(v));
            if c < best[v] - EPS {
                best[v] = c;
            }
        }
    }
    (0..nr).map(|i| best[i].is_finite().then_some(best[i])).collect()
}

fn overlap(a: &[f64; 4], b: &[f64; 4]) -> f64 {
    let w = a[2].min(b[2]) - a[0].max(b[0]);
    let h = a[3].min(b[3]) - a[1].max(b[1]);
    if w > 0.0 && h > 0.0 {
        w * h
    } else {
        0.0
    }
}

/// `net_share`: net room area per unit of the area the country states (`Conventions`); the
/// dwelling total is compared in the country's own measure.
fn programme_hits(f: &PlanFacts, p: &Programme, net_share: f64) -> Vec<Hit> {
    let mut out = vec![];
    for g in groups(f, "unit") {
        let keys: Vec<ElKey> = g.iter().map(|r| r.key.clone()).collect();
        let sel_count = p.variants.iter().find_map(|v| v.select.as_ref()).map(|(kinds, _)| {
            g.iter().filter(|r| kinds.iter().any(|k| *k == r.kind)).count()
        });
        let Some(v) = p.variant(sel_count) else {
            let mut h = hit(keys, sel_count.map(|c| c as f64), None, "no_variant");
            h.verified = false;
            out.push(h);
            continue;
        };
        let total: f64 = g.iter().map(|r| r.area).sum();
        let stated = total / net_share;
        if let Some(min) = v.area_min.filter(|m| stated < m - EPS) {
            let mut h = hit(keys.clone(), Some(stated), Some(min), "area_min");
            h.detail = v.code.clone();
            h.verified = v.verified;
            out.push(h);
        }
        let mut by_kind: BTreeMap<&str, Vec<&crate::checks::norm_pack::RoomReq>> = BTreeMap::new();
        for r in &v.rooms {
            by_kind.entry(r.kind.as_str()).or_default().push(r);
        }
        for (kind, mut reqs) in by_kind {
            reqs.sort_by(|a, b| b.min_area.unwrap_or(0.0).total_cmp(&a.min_area.unwrap_or(0.0)));
            let mut have: Vec<&&FactRoom> = g.iter().filter(|r| r.kind == kind).collect();
            have.sort_by(|a, b| b.area.total_cmp(&a.area).then(a.key.cmp(&b.key)));
            for (i, req) in reqs.iter().enumerate() {
                match have.get(i) {
                    None => {
                        let mut h = hit(keys.clone(), Some(have.len() as f64), Some(reqs.len() as f64), "missing_room");
                        h.detail = kind.to_string();
                        h.verified = v.verified && req.verified;
                        out.push(h);
                        break;
                    }
                    Some(r) => {
                        if let Some(min) = req.min_area.filter(|m| r.area < m - EPS) {
                            let mut h = hit(vec![r.key.clone()], Some(r.area), Some(min), "room_area");
                            h.detail = kind.to_string();
                            h.verified = v.verified && req.verified;
                            out.push(h);
                        }
                    }
                }
            }
        }
        let share = |kinds: &[&str]| -> (f64, Vec<ElKey>) {
            let rs: Vec<&&FactRoom> = g.iter().filter(|r| kinds.contains(&r.kind.as_str())).collect();
            let a: f64 = rs.iter().map(|r| r.area).sum();
            (if total > 0.0 { a / total } else { 0.0 }, rs.iter().map(|r| r.key.clone()).collect())
        };
        for (lim, kinds, what) in [
            (v.storage_max, &["storage"][..], "storage_share"),
            (v.circulation_max, &["corridor", "hall"][..], "circulation_share"),
        ] {
            let (x, els) = share(kinds);
            if let Some(m) = lim.filter(|m| x > m + EPS) {
                let mut h = hit(els, Some(x), Some(m), what);
                h.verified = v.verified && v.limits_verified;
                out.push(h);
            }
        }
        for r in &g {
            let known = v.rooms.iter().any(|q| q.kind == r.kind)
                || v.allowed.contains(&r.kind)
                || v.select.as_ref().is_some_and(|s| s.0.contains(&r.kind));
            if !r.kind.is_empty() && !known {
                let mut h = hit(vec![r.key.clone()], None, None, "foreign_room");
                h.detail = r.kind.clone();
                h.verified = false;
                out.push(h);
            }
        }
    }
    out
}

/// Results of one new-kind check on the facts. `room_kind` narrows the rooms further (the
/// rule's `applies.room_kind`).
pub fn eval_kind(k: &NormKind, f: &PlanFacts, pack: &NormPack, room_kind: &[String]) -> Vec<Hit> {
    let rooms: Vec<&FactRoom> = f.rooms.iter().filter(|r| kind_in(&r.kind, room_kind)).collect();
    let mut out = vec![];
    match k {
        NormKind::Formula { cat, expr, min, max } => {
            let mut vars = vec![];
            expr.vars(&mut vars);
            let optional = vars.iter().any(|v| v == "headroom");
            let subjects: Vec<(ElKey, Box<dyn Fn(&str) -> Option<f64> + '_>)> = if let Some(els) = f.els(cat) {
                els.iter()
                    .filter(|s| !optional || s.vals.contains_key("headroom"))
                    .map(|s| (s.key.clone(), Box::new(move |n: &str| s.vals.get(n).copied()) as Box<dyn Fn(&str) -> Option<f64>>))
                    .collect()
            } else {
                rooms
                    .iter()
                    .map(|r| (r.key.clone(), Box::new(move |n: &str| r.field(n)) as Box<dyn Fn(&str) -> Option<f64>>))
                    .collect()
            };
            for (key, get) in subjects {
                match expr.eval(&*get) {
                    None => {
                        let mut h = hit(vec![key], None, None, "no_value");
                        h.verified = false;
                        out.push(h);
                    }
                    Some(v) => out.extend(range_hits(vec![key], v, *min, *max)),
                }
            }
        }
        NormKind::RatioElement { cat, room_kind: rk, num, den, min, max } => {
            if let Some(els) = f.els(cat) {
                for s in els {
                    if let (Some(a), Some(b)) = (s.vals.get(num), s.vals.get(den).filter(|b| **b > 0.0)) {
                        out.extend(range_hits(vec![s.key.clone()], a / b, *min, *max));
                    }
                }
            } else {
                for r in rooms.iter().filter(|r| kind_in(&r.kind, rk)) {
                    if let (Some(a), Some(b)) = (r.field(num), r.field(den).filter(|b| *b > 0.0)) {
                        out.extend(range_hits(vec![r.key.clone()], a / b, *min, *max));
                    }
                }
            }
        }
        NormKind::RatioTotal { group_by, num, den, min, max } => {
            let narrowed = PlanFacts {
                rooms: rooms.iter().map(|r| (*r).clone()).collect(),
                ..PlanFacts::default()
            };
            for g in groups(&narrowed, group_by) {
                let (a, els) = agg_sum(num, &g);
                let (b, all) = agg_sum(den, &g);
                if b > 0.0 {
                    let el = if els.is_empty() { all } else { els };
                    out.extend(range_hits(el, a / b, *min, *max));
                }
            }
        }
        NormKind::Adjacency { require, forbid } => {
            let linked = |i: usize, kinds: &[String]| {
                f.doors.iter().any(|d| {
                    d.rooms.len() == 2
                        && d.rooms.contains(&i)
                        && d.rooms.iter().any(|j| *j != i && kinds.contains(&f.rooms[*j].kind))
                })
            };
            for (a, b) in require {
                for (i, r) in f.rooms.iter().enumerate() {
                    if r.kind != *a || !kind_in(&r.kind, room_kind) {
                        continue;
                    }
                    let near = f.rooms.iter().any(|o| {
                        o.level == r.level && o.unit == r.unit && b.contains(&o.kind)
                    });
                    if near && !linked(i, b) {
                        let mut h = hit(vec![r.key.clone()], None, None, "adjacency_missing");
                        h.detail = format!("{a} -> {}", b.join("|"));
                        out.push(h);
                    }
                }
            }
            for d in &f.doors {
                if d.rooms.len() != 2 {
                    continue;
                }
                let (x, y) = (&f.rooms[d.rooms[0]], &f.rooms[d.rooms[1]]);
                for (a, b) in forbid {
                    if (x.kind == *a && y.kind == *b) || (x.kind == *b && y.kind == *a) {
                        let mut h = hit(vec![d.key.clone(), x.key.clone(), y.key.clone()], None, None, "adjacency_forbidden");
                        h.detail = format!("{a} - {b}");
                        out.push(h);
                    }
                }
            }
        }
        NormKind::Stack { room_kind: wet } => {
            for r in rooms.iter().filter(|r| wet.contains(&r.kind)) {
                let below = f.rooms.iter().filter(|o| o.storey < r.storey).map(|o| o.storey).max();
                let Some(b) = below else { continue };
                let over = f
                    .rooms
                    .iter()
                    .filter(|o| o.storey == b && wet.contains(&o.kind))
                    .map(|o| overlap(&r.bbox, &o.bbox))
                    .fold(0.0, f64::max);
                if over <= 0.01 {
                    out.push(hit(vec![r.key.clone()], Some(over), None, "not_stacked"));
                }
            }
        }
        NormKind::Distance { max, exit_kinds } => {
            let d = travel_distances(f, exit_kinds);
            for (i, r) in f.rooms.iter().enumerate() {
                if exit_kinds.contains(&r.kind) || !kind_in(&r.kind, room_kind) {
                    continue;
                }
                match d[i] {
                    None => out.push(hit(vec![r.key.clone()], None, Some(*max), "no_route")),
                    Some(x) if x > max + EPS => out.push(hit(vec![r.key.clone()], Some(x), Some(*max), "max")),
                    _ => {}
                }
            }
        }
        NormKind::Programme { typology } => match pack.programme(typology) {
            Some(p) => out.extend(programme_hits(f, p, pack.conventions().net_share)),
            None => {
                let mut h = hit(vec![], None, None, "no_variant");
                h.detail = format!("no programme for {typology}");
                h.verified = false;
                out.push(h);
            }
        },
    }
    out
}

// ---------- running a pack ----------

/// What a pack run is for: design depth 1..5, discipline (`*` all), and the project.
#[derive(Clone, Debug, PartialEq)]
pub struct PackCtx {
    pub depth: u8,
    pub discipline: String,
    pub typology: Option<String>,
    pub region: Option<String>,
    /// The building has sprinklers; `None`: not known, rules for sprinklered buildings skip.
    pub sprinklered: Option<bool>,
    /// Building height (m) for height-class rules; `None`: not known, they all run.
    pub height: Option<f64>,
}

impl Default for PackCtx {
    fn default() -> Self {
        PackCtx {
            depth: 5,
            discipline: "*".into(),
            typology: None,
            region: None,
            sprinklered: None,
            height: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackFinding {
    pub rule_id: String,
    /// The pack that defined the rule.
    pub pack: String,
    pub severity: Severity,
    /// Verified rule and numbers: a finding to act on; otherwise advice (severity info).
    pub hard: bool,
    pub elements: Vec<ElKey>,
    pub measured: Option<f64>,
    pub required: Option<f64>,
    pub unit: String,
    pub source: String,
    pub clause: String,
    pub what: String,
    pub detail: String,
    /// Proposed fix: set this field of the element to this value (m). Only for a formula of
    /// one settable field (`settable`); an agent drafts it, the person applies it.
    pub fix: Option<(String, f64)>,
}

/// The value a `min`/`max` hit of a one-field formula proposes for its element.
pub fn fix_of(k: &NormKind, h: &Hit) -> Option<(String, f64)> {
    let NormKind::Formula { cat, expr: Expr::Var(field), .. } = k else {
        return None;
    };
    let v = h.required.filter(|_| matches!(h.what, "min" | "max") && h.elements.len() == 1)?;
    settable(cat, field).then(|| (field.clone(), v))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackReport {
    pub findings: Vec<PackFinding>,
    pub ran: Vec<String>,
    /// Rule id and why it did not run (`typology`, `region`, `floors`).
    pub skipped: Vec<(String, String)>,
}

impl PackReport {
    pub fn hard(&self) -> impl Iterator<Item = &PackFinding> {
        self.findings.iter().filter(|f| f.hard)
    }
}

fn skip_reason(r: &PackRule, ctx: &PackCtx, floors: usize) -> Option<&'static str> {
    let a = &r.applies;
    if !a.typology.is_empty() && !ctx.typology.as_ref().is_some_and(|t| a.typology.contains(t)) {
        return Some("typology");
    }
    if !a.region.is_empty() && !ctx.region.as_ref().is_some_and(|t| a.region.contains(t)) {
        return Some("region");
    }
    if a.min_floors.is_some_and(|m| floors < m as usize) || a.max_floors.is_some_and(|m| floors > m as usize) {
        return Some("floors");
    }
    // a sprinkler relief needs sprinklers known; an unsprinklered limit holds unless known
    match a.sprinklered {
        Some(true) if ctx.sprinklered != Some(true) => return Some("sprinklers"),
        Some(false) if ctx.sprinklered == Some(true) => return Some("sprinklers"),
        _ => {}
    }
    if let Some(h) = ctx.height {
        if a.height_min.is_some_and(|m| h < m) || a.height_max.is_some_and(|m| h > m) {
            return Some("height");
        }
    }
    None
}

fn finding(r: &PackRule, pack: &NormPack, h: Hit) -> PackFinding {
    let hard = r.verified && h.verified;
    PackFinding {
        rule_id: r.id.clone(),
        pack: r.pack.clone(),
        severity: if hard { r.severity } else { Severity::Info },
        hard,
        elements: h.elements,
        measured: h.measured,
        required: h.required,
        unit: match h.what {
            "missing_room" | "no_variant" => "count".into(),
            "storage_share" | "circulation_share" => "ratio".into(),
            "room_area" | "area_min" => "m2".into(),
            _ => r.unit.clone(),
        },
        source: pack.source(&r.source).map_or(r.source.clone(), |s| s.title.clone()),
        clause: r.clause.clone(),
        what: h.what.to_string(),
        detail: h.detail,
        fix: None,
    }
}

/// Runs the pack's rules for `ctx` on the model. A rule not verified against its source
/// only gives advice (severity info, `hard: false`), never a hard finding. Same model and
/// pack, same report.
pub fn run_pack(m: &Model, pack: &NormPack, scope: &CheckScope, ctx: &PackCtx) -> Result<PackReport, Error> {
    let facts = PlanFacts::from_model(m, pack)?;
    let floors = {
        let mut s: Vec<usize> = facts.rooms.iter().map(|r| r.storey).collect();
        s.sort_unstable();
        s.dedup();
        s.len()
    };
    let w = Within::new(scope);
    let in_scope = |els: &[ElKey]| -> bool {
        let lvl_ok = |k: &ElKey| match (w.level, k.0.as_str()) {
            (Some(l), "room") => facts.rooms.iter().any(|r| r.key == *k && r.level == l),
            _ => true,
        };
        els.is_empty() && matches!(scope, CheckScope::All)
            || els.iter().any(|k| w.has(&k.0, k.1) && lvl_ok(k))
    };
    let mut rep = PackReport::default();
    for r in pack.rules(ctx.depth, &ctx.discipline) {
        if let Some(why) = skip_reason(r, ctx, floors) {
            rep.skipped.push((r.id.clone(), why.to_string()));
            continue;
        }
        rep.ran.push(r.id.clone());
        match &r.check {
            PackCheck::Legacy(n) => {
                for f in run_norms(m, std::slice::from_ref(n), scope)? {
                    let h = Hit {
                        elements: f.elements,
                        measured: f.measured,
                        required: f.required,
                        what: "rule",
                        detail: f.clause.unwrap_or_default(),
                        verified: true,
                    };
                    let mut pf = finding(r, pack, h);
                    pf.unit = f.unit.to_string();
                    if r.verified {
                        pf.severity = f.severity;
                    }
                    if !pf.detail.is_empty() {
                        pf.clause = std::mem::take(&mut pf.detail);
                    }
                    rep.findings.push(pf);
                }
            }
            PackCheck::Kind(k) => {
                for h in eval_kind(k, &facts, pack, &r.applies.room_kind) {
                    if in_scope(&h.elements) {
                        let fix = fix_of(k, &h);
                        let mut pf = finding(r, pack, h);
                        pf.fix = fix;
                        rep.findings.push(pf);
                    }
                }
            }
        }
    }
    let order: BTreeMap<&str, usize> = pack.rules.iter().enumerate().map(|(i, r)| (r.id.as_str(), i)).collect();
    rep.findings.sort_by(|a, b| {
        (a.severity, order.get(a.rule_id.as_str()), &a.elements, &a.what)
            .cmp(&(b.severity, order.get(b.rule_id.as_str()), &b.elements, &b.what))
    });
    Ok(rep)
}
