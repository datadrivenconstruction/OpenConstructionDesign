// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The result for drawing a view: rules are evaluated over sets (one query per value source),
//! not per element, and the result is cached by the change journal. Editing an element recomputes
//! only it and its openings; editing settings, types, properties and parameters recomputes the whole view.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::{params_from_iter, types::Value, Connection};

use super::{
    category_ids, has_col, state, table_exists, ElemPaint, FilterRule, FilterUse, GfxOverride,
    RuleOp, ViewFilterSpec, ViewGfx, ViewPaint, GFX_CATEGORIES,
};
use crate::changes::Changes;
use crate::params::TYPE_OF;
use crate::phases::ViewKey;
use crate::{Error, Model, ParamDef, ParamType};

/// We keep no more views at once: a sheet with dozens of views must not accumulate the graphics of all.
const CACHE_VIEWS: usize = 64;

/// A write to any of these tables changes the values of many elements: the view is recomputed.
const CONFIG: [&str; 15] = [
    "view_gfx",
    "view_template",
    "view_filter",
    "object_style",
    "prop",
    "pparam",
    "pparam_cat",
    "pparam_val",
    "level",
    "wall_type",
    "slab_type",
    "member_type",
    "comp_type",
    "wall_type_layer",
    "slab_type_layer",
];

#[derive(Default)]
pub(crate) struct PaintCache(Mutex<HashMap<ViewKey, (u64, Arc<ViewPaint>)>>);

type Elems = HashMap<(&'static str, i64), ElemPaint>;
/// Changed elements by table.
type Dirty = HashMap<&'static str, Vec<i64>>;
const NO_IDS: &[i64] = &[];

/// A parameter value on an element.
#[derive(Clone, Debug, Default, PartialEq)]
struct Cell {
    text: String,
    /// The value in SI units, if the unit is known.
    si: Option<f64>,
}

fn cell_of_value(v: Value, unit: Option<&str>) -> Option<Cell> {
    let (text, x) = match v {
        Value::Null | Value::Blob(_) => return None,
        Value::Integer(i) => (i.to_string(), Some(i as f64)),
        Value::Real(x) => (x.to_string(), Some(x)),
        Value::Text(s) => {
            let x = rule_number(&s);
            (s, x)
        }
    };
    let si = match (x, unit.and_then(unit_factor)) {
        (Some(x), Some(f)) => Some(x * f),
        _ => rule_quantity(&text),
    };
    Some(Cell { text, si })
}

fn unit_factor(u: &str) -> Option<f64> {
    let u = u.trim().to_lowercase();
    UNITS
        .iter()
        .chain(SHORT_UNITS.iter())
        .find(|x| x.0 == u.as_str())
        .map(|x| x.1)
}

/// Rule value units and the multiplier to SI; long ones first so that the millimetre unit is not read as the metre unit.
const UNITS: [(&str, f64); 30] = [
    ("mm2", 1e-6),
    ("\u{43c}\u{43c}2", 1e-6),
    ("mm²", 1e-6),
    ("\u{43c}\u{43c}²", 1e-6),
    ("cm2", 1e-4),
    ("\u{441}\u{43c}2", 1e-4),
    ("cm²", 1e-4),
    ("\u{441}\u{43c}²", 1e-4),
    ("mm3", 1e-9),
    ("\u{43c}\u{43c}3", 1e-9),
    ("mm³", 1e-9),
    ("\u{43c}\u{43c}³", 1e-9),
    ("ft2", 0.09290304),
    ("ft²", 0.09290304),
    ("ft3", 0.028316846592),
    ("ft³", 0.028316846592),
    ("m2", 1.0),
    ("\u{43c}2", 1.0),
    ("m²", 1.0),
    ("\u{43c}²", 1.0),
    ("m3", 1.0),
    ("\u{43c}3", 1.0),
    ("m³", 1.0),
    ("\u{43c}³", 1.0),
    ("mm", 1e-3),
    ("\u{43c}\u{43c}", 1e-3),
    ("cm", 1e-2),
    ("\u{441}\u{43c}", 1e-2),
    ("km", 1e3),
    ("\u{43a}\u{43c}", 1e3),
];
/// Short units: checked after [`UNITS`].
const SHORT_UNITS: [(&str, f64); 6] = [
    ("ft", 0.3048),
    ("in", 0.0254),
    ("m", 1.0),
    ("\u{43c}", 1.0),
    ("l", 1e-3),
    ("\u{43b}", 1e-3),
];

/// A number with a decimal comma or point and space thousands separators. A separator that occurs
/// once is decimal; a repeated one is thousands ("1,234,567"); with both kinds the last one is decimal.
pub fn rule_number(s: &str) -> Option<f64> {
    let t: String = s
        .trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '\u{a0}' | '\u{202f}' | '\u{2009}'))
        .map(|c| if c == '\u{2212}' { '-' } else { c })
        .collect();
    if t.is_empty() {
        return None;
    }
    let sep = |c: char| c == ',' || c == '.';
    let (commas, dots) = (t.matches(',').count(), t.matches('.').count());
    let dec = match (commas, dots) {
        (0, 0) => None,
        (1, 0) => t.rfind(','),
        (0, 1) => t.rfind('.'),
        (_, 0) | (0, _) => None,
        _ => t.rfind(sep),
    };
    let int = dec.map_or(t.as_str(), |d| &t[..d]);
    // thousands separators: groups of exactly three digits, otherwise it is not a number ("1.2.3")
    if int.contains(sep) && int.split(sep).skip(1).any(|g| g.len() != 3) {
        return None;
    }
    let clean: String = t
        .char_indices()
        .filter_map(|(i, c)| match (sep(c), Some(i) == dec) {
            (true, true) => Some('.'),
            (true, false) => None,
            _ => Some(c),
        })
        .collect();
    clean.parse::<f64>().ok().filter(|x| x.is_finite())
}

/// A quantity with a mandatory unit, in SI: "3000 mm" -> 3.0, "2,5 m2" -> 2.5 (also in Russian units); None without a unit.
pub fn rule_quantity(s: &str) -> Option<f64> {
    let t = s.trim().to_lowercase();
    UNITS.iter().chain(SHORT_UNITS.iter()).find_map(|(u, f)| {
        let n = t.strip_suffix(*u)?;
        rule_number(n).map(|x| x * *f)
    })
}

/// A rule ready to check many values.
#[derive(Clone, Debug)]
pub(super) struct Prepared {
    op: RuleOp,
    lower: String,
    /// The number from the rule value as written.
    num: Option<f64>,
    /// The rule value in SI: the unit is given explicitly or the parameter has a quantity type.
    si: Option<f64>,
}

fn same_num(x: f64, y: f64) -> bool {
    (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0)
}

impl Prepared {
    pub(super) fn new(r: &FilterRule, ptype: Option<ParamType>) -> Self {
        let raw = r.value.trim();
        let (raw_yes_no, si) = match ptype {
            Some(ParamType::YesNo) => {
                let v = crate::params::parse_value(ParamType::YesNo, raw).ok();
                let yes = matches!(v, Some(crate::PVal::Bool(true)));
                (v.map(|_| if yes { "1" } else { "0" }), None)
            }
            Some(ParamType::Length | ParamType::Area | ParamType::Volume) => {
                (None, rule_quantity(raw).or_else(|| rule_number(raw)))
            }
            _ => (None, rule_quantity(raw)),
        };
        let text = raw_yes_no.unwrap_or(raw);
        Prepared {
            op: r.op,
            lower: text.to_lowercase(),
            num: rule_number(text),
            si,
        }
    }

    /// A pair of numbers to compare: in SI if the rule value has a unit.
    fn numbers(&self, c: &Cell, value: &str) -> Option<(f64, f64)> {
        match (self.si, self.num) {
            (Some(w), _) => c.si.or_else(|| rule_number(value)).map(|x| (x, w)),
            (None, Some(w)) => rule_number(value).map(|x| (x, w)),
            _ => None,
        }
    }

    pub(super) fn test(&self, cell: Option<&Cell>) -> bool {
        let Some((c, v)) = cell
            .map(|c| (c, c.text.trim()))
            .filter(|(_, v)| !v.is_empty())
        else {
            return self.op == RuleOp::Missing;
        };
        let same = || match self.numbers(c, v) {
            Some((x, y)) => same_num(x, y),
            None => v.to_lowercase() == self.lower,
        };
        let numeric = self.num.is_some() || self.si.is_some();
        // a number is not compared with text as a string: "EI60" is not greater than 30
        let order = || match self.numbers(c, v) {
            Some((x, y)) if same_num(x, y) => Some(std::cmp::Ordering::Equal),
            Some((x, y)) => x.partial_cmp(&y),
            None if numeric => None,
            None => Some(v.to_lowercase().as_str().cmp(self.lower.as_str())),
        };
        use std::cmp::Ordering::{Greater, Less};
        match self.op {
            RuleOp::Exists => true,
            RuleOp::Missing => false,
            RuleOp::Eq => same(),
            RuleOp::Ne => !same(),
            RuleOp::Contains => v.to_lowercase().contains(&self.lower),
            RuleOp::NotContains => !v.to_lowercase().contains(&self.lower),
            RuleOp::Lt => order() == Some(Less),
            RuleOp::Le => matches!(order(), Some(o) if o != Greater),
            RuleOp::Gt => order() == Some(Greater),
            RuleOp::Ge => matches!(order(), Some(o) if o != Less),
        }
    }

    /// Check of plain text (a value with no type and no column unit).
    pub(super) fn test_text(&self, v: Option<&str>) -> bool {
        let c = v.map(|s| Cell {
            text: s.to_string(),
            si: rule_quantity(s),
        });
        self.test(c.as_ref())
    }
}

/// Values of one parameter on elements of one category.
type Column = HashMap<i64, Cell>;

fn id_list(scope: Option<&[i64]>, col: &str) -> String {
    match scope {
        Some(ids) => {
            let list: Vec<String> = ids.iter().map(i64::to_string).collect();
            format!(" AND {col} IN ({})", list.join(","))
        }
        None => String::new(),
    }
}

/// Adds the query values `(id, value[, unit])`; leaves those already filled by a higher-priority source.
fn fill(db: &Connection, col: &mut Column, sql: &str, args: &[Value]) -> Result<(), Error> {
    let mut st = db.prepare(sql)?;
    let has_unit = st.column_count() > 2;
    let rows = st.query_map(params_from_iter(args.iter()), |r| {
        let unit: Option<String> = if has_unit { r.get(2)? } else { None };
        Ok((r.get::<_, i64>(0)?, r.get::<_, Value>(1)?, unit))
    })?;
    for x in rows {
        let (id, v, unit) = x?;
        if !col.contains_key(&id) {
            if let Some(c) = cell_of_value(v, unit.as_deref()) {
                col.insert(id, c);
            }
        }
    }
    Ok(())
}

/// Values of one parameter of all table elements: one pass per source.
struct Columns<'a> {
    db: &'a Connection,
    /// Property keys by table: lower-cased here, since SQLite lower() understands only ASCII.
    prop_keys: HashMap<String, Vec<(String, String)>>,
}

impl Columns<'_> {
    fn keys_of(&mut self, kind: &str, lower: &str) -> Result<Vec<String>, Error> {
        if !self.prop_keys.contains_key(kind) {
            let mut st = self
                .db
                .prepare_cached("SELECT DISTINCT key FROM prop WHERE kind=?1")?;
            let v: Vec<String> = st
                .query_map([kind], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            let v = v
                .into_iter()
                .map(|k| (k.trim().to_lowercase(), k))
                .collect();
            self.prop_keys.insert(kind.to_string(), v);
        }
        Ok(self.prop_keys[kind]
            .iter()
            .filter(|k| k.0 == lower)
            .map(|k| k.1.clone())
            .collect())
    }

    fn props(
        &mut self,
        col: &mut Column,
        kind: &str,
        lower: &str,
        scope: Option<&[i64]>,
        via: Option<(&str, &str)>,
    ) -> Result<(), Error> {
        let keys = self.keys_of(kind, lower)?;
        if keys.is_empty() {
            return Ok(());
        }
        let marks = vec!["?"; keys.len()].join(",");
        let mut args = vec![Value::Text(kind.to_string())];
        args.extend(keys.into_iter().map(Value::Text));
        let sql = match via {
            None => format!(
                "SELECT p.ref, p.value, p.unit FROM prop p WHERE p.kind=?1 AND p.key IN ({marks}){}",
                id_list(scope, "p.ref")
            ),
            Some((table, tcol)) => format!(
                "SELECT e.id, p.value, p.unit FROM {table} e JOIN prop p ON p.kind=?1 AND p.ref=e.{tcol} \
                 WHERE p.key IN ({marks}){}",
                id_list(scope, "e.id")
            ),
        };
        fill(self.db, col, &sql, &args)
    }

    fn builtin(
        &self,
        col: &mut Column,
        key: &str,
        cat: &str,
        table: &str,
        scope: Option<&[i64]>,
    ) -> Result<(), Error> {
        let db = self.db;
        let where_ = format!(" WHERE 1=1{}", id_list(scope, "e.id"));
        let ty = TYPE_OF.iter().find(|t| t.0 == table).copied();
        let own = |c: &mut Column, name: &str| -> Result<(), Error> {
            if has_col(db, table, name)? {
                let sql = format!("SELECT e.id, e.{name} FROM {table} e{where_}");
                fill(db, c, &sql, &[])?;
            }
            Ok(())
        };
        match key {
            "category" | "\u{43a}\u{430}\u{442}\u{435}\u{433}\u{43e}\u{440}\u{438}\u{44f}" => {
                let sql = format!("SELECT e.id, ?1 FROM {table} e{where_}");
                fill(db, col, &sql, &[Value::Text(cat.to_string())])?;
            }
            "id" => {
                let sql = format!("SELECT e.id, e.id FROM {table} e{where_}");
                fill(db, col, &sql, &[])?;
            }
            "name" | "\u{438}\u{43c}\u{44f}" => own(col, "name")?,
            "number" | "\u{43d}\u{43e}\u{43c}\u{435}\u{440}" => own(col, "number")?,
            "mark" | "\u{43c}\u{430}\u{440}\u{43a}\u{430}" => own(col, "mark")?,
            "type" | "\u{442}\u{438}\u{43f}" => {
                if let Some((_, tt, tcol)) = ty {
                    let sql = format!(
                        "SELECT e.id, t.name FROM {table} e JOIN {tt} t ON t.id=e.{tcol}{where_}"
                    );
                    fill(db, col, &sql, &[])?;
                }
            }
            "level" | "\u{443}\u{440}\u{43e}\u{432}\u{435}\u{43d}\u{44c}" => {
                if table == "opening" {
                    let sql = format!(
                        "SELECT e.id, l.name FROM opening e JOIN wall w ON w.id=e.wall \
                         JOIN level l ON l.id=w.level{where_}"
                    );
                    fill(db, col, &sql, &[])?;
                } else if has_col(db, table, "level")? {
                    let sql = format!(
                        "SELECT e.id, l.name FROM {table} e JOIN level l ON l.id=e.level{where_}"
                    );
                    fill(db, col, &sql, &[])?;
                }
            }
            "material" | "\u{43c}\u{430}\u{442}\u{435}\u{440}\u{438}\u{430}\u{43b}" => {
                own(&mut *col, "material")?;
                let layers = ty.and_then(|t| match t.1 {
                    "wall_type" => Some(("wall_type_layer", "wtype", t.2)),
                    "slab_type" => Some(("slab_type_layer", "stype", t.2)),
                    _ => None,
                });
                if let Some((lt, lc, tcol)) = layers {
                    // one name is the exact value for "equals", several are comma-separated in layer order
                    let mut names: HashMap<i64, Vec<String>> = HashMap::new();
                    let mut st = db.prepare(&format!(
                        "SELECT {lc}, material FROM {lt} ORDER BY {lc}, pos"
                    ))?;
                    let rows =
                        st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
                    for x in rows {
                        let (t, n) = x?;
                        names.entry(t).or_default().push(n);
                    }
                    let mut st =
                        db.prepare(&format!("SELECT e.id, e.{tcol} FROM {table} e{where_}"))?;
                    let rows =
                        st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
                    for x in rows {
                        let (id, t) = x?;
                        if let (false, Some(n)) = (col.contains_key(&id), names.get(&t)) {
                            col.insert(
                                id,
                                Cell {
                                    text: n.join(", "),
                                    si: None,
                                },
                            );
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// A project parameter, built-in, element property, its type's property; the name is case-insensitive.
    fn column(
        &mut self,
        def: Option<&ParamDef>,
        key: &str,
        cat: &str,
        table: &str,
        scope: Option<&[i64]>,
    ) -> Result<Column, Error> {
        let mut col = Column::new();
        let ty = TYPE_OF.iter().find(|t| t.0 == table).copied();
        if let Some(d) = def {
            let quantity = matches!(
                d.spec.ptype,
                ParamType::Length | ParamType::Area | ParamType::Volume
            );
            let link = if d.spec.per_type {
                ty.map(|t| (t.1, t.2))
            } else {
                Some((table, "id"))
            };
            if let Some((kind, rcol)) = link {
                let sql = format!(
                    "SELECT e.id, v.value FROM {table} e JOIN pparam_val v \
                     ON v.param=?1 AND v.kind=?2 AND v.ref=e.{rcol} WHERE 1=1{}",
                    id_list(scope, "e.id")
                );
                let args = [Value::Integer(d.id), Value::Text(kind.to_string())];
                fill(self.db, &mut col, &sql, &args)?;
                // quantity values are stored in SI
                if quantity {
                    for c in col.values_mut() {
                        c.si = rule_number(&c.text);
                    }
                }
            }
        }
        self.builtin(&mut col, key, cat, table, scope)?;
        self.props(&mut col, table, key, scope, None)?;
        if let Some(t) = ty {
            self.props(&mut col, t.1, key, scope, Some((table, t.2)))?;
        }
        Ok(col)
    }
}

/// A view filter with parsed rules.
struct Compiled<'a> {
    using: FilterUse,
    spec: &'a ViewFilterSpec,
    rules: Vec<(String, Prepared)>,
}

/// Evaluates the view's elements (`scope` limits to these) and puts those differing from the default into `out`.
fn eval(m: &Model, g: &ViewGfx, scope: Option<&Dirty>, out: &mut Elems) -> Result<(), Error> {
    let db = &m.db;
    let styles = m.object_styles()?;
    let mut specs: Vec<(FilterUse, ViewFilterSpec)> = vec![];
    for u in g.filters.iter().filter(|u| u.enabled) {
        if let Some(f) = super::load_filter(db, u.filter)? {
            specs.push((*u, f.spec));
        }
    }
    let defs: HashMap<String, ParamDef> = if specs.is_empty() {
        HashMap::new()
    } else {
        m.params()?
            .into_iter()
            .map(|d| (d.spec.name.trim().to_lowercase(), d))
            .collect()
    };
    let key_of = |p: &str| p.trim().to_lowercase();
    let compiled: Vec<Compiled<'_>> = specs
        .iter()
        .map(|(u, s)| Compiled {
            using: *u,
            spec: s,
            rules: s
                .rules
                .iter()
                .map(|r| {
                    let k = key_of(&r.param);
                    let p = Prepared::new(r, defs.get(&k).map(|d| d.spec.ptype));
                    (k, p)
                })
                .collect(),
        })
        .collect();
    let mut sources = Columns {
        db,
        prop_keys: HashMap::new(),
    };
    for (cat, table) in GFX_CATEGORIES {
        let c = g.categories.get(cat).copied().unwrap_or_default();
        let style = styles.get(cat).copied().unwrap_or_default();
        let base = ElemPaint {
            hidden: !c.visible,
            over: c.over.or(style),
        };
        let fs: Vec<&Compiled<'_>> = compiled.iter().filter(|u| u.spec.applies_to(cat)).collect();
        if base == ElemPaint::default() && fs.is_empty() {
            continue;
        }
        let part: Option<&[i64]> = scope.map(|s| s.get(table).map_or(NO_IDS, |v| v.as_slice()));
        if part.is_some_and(<[i64]>::is_empty) {
            continue;
        }
        let ids = category_ids(db, cat, table, part)?;
        if ids.is_empty() {
            continue;
        }
        let mut cols: HashMap<&str, Column> = HashMap::new();
        for f in &fs {
            for (k, _) in &f.rules {
                if !cols.contains_key(k.as_str()) {
                    let col = sources.column(defs.get(k), k, cat, table, part)?;
                    cols.insert(k.as_str(), col);
                }
            }
        }
        for id in ids {
            let mut p = base;
            let mut hit = GfxOverride::default();
            for f in &fs {
                let one = |(k, r): &(String, Prepared)| {
                    r.test(cols.get(k.as_str()).and_then(|c| c.get(&id)))
                };
                let matched = if f.rules.is_empty() {
                    true
                } else if f.spec.any {
                    f.rules.iter().any(one)
                } else {
                    f.rules.iter().all(one)
                };
                if matched {
                    p.hidden |= !f.using.visible;
                    hit = hit.or(f.using.over);
                }
            }
            p.over = hit.or(p.over);
            if p != ElemPaint::default() {
                out.insert((table, id), p);
            }
        }
    }
    Ok(())
}

/// An opening of a hidden wall is hidden: otherwise the door hangs in the air. `is_hidden` tells whether the wall is hidden.
fn hide_hosted(
    db: &Connection,
    scope: Option<&[i64]>,
    is_hidden: &dyn Fn(i64) -> bool,
    out: &mut Elems,
) -> Result<(), Error> {
    let sql = format!(
        "SELECT id, wall FROM opening WHERE 1=1{}",
        id_list(scope, "id")
    );
    let mut st = db.prepare(&sql)?;
    let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    for x in rows {
        let (id, wall) = x?;
        if is_hidden(wall) {
            out.entry(("opening", id)).or_default().hidden = true;
        }
    }
    Ok(())
}

pub(super) fn full(m: &Model, v: ViewKey) -> Result<ViewPaint, Error> {
    let g = state(&m.db, v)?.effective;
    let mut elems = Elems::new();
    eval(m, &g, None, &mut elems)?;
    if elems.iter().any(|(k, e)| k.0 == "wall" && e.hidden) {
        let walls: Elems = elems
            .iter()
            .filter(|(k, _)| k.0 == "wall")
            .map(|(k, e)| (*k, *e))
            .collect();
        hide_hosted(
            &m.db,
            None,
            &|w| walls.get(&("wall", w)).is_some_and(|e| e.hidden),
            &mut elems,
        )?;
    }
    let hidden_cats = GFX_CATEGORIES
        .iter()
        .map(|c| c.0)
        .filter(|c| g.categories.get(*c).is_some_and(|x| !x.visible))
        .map(str::to_string)
        .collect();
    Ok(ViewPaint {
        detail: g.detail,
        thin_lines: g.thin_lines,
        hidden_cats,
        any_hidden: elems.values().any(|e| e.hidden),
        elems,
    })
}

enum Touch {
    Nothing,
    Everything,
    Elements(Dirty),
}

fn touch_of(db: &Connection, ch: &Changes) -> Result<Touch, Error> {
    if ch.all || CONFIG.iter().any(|t| ch.touched(t)) {
        return Ok(Touch::Everything);
    }
    let mut dirty = Dirty::new();
    for (_, table) in GFX_CATEGORIES {
        if dirty.contains_key(table) || !ch.touched(table) {
            continue;
        }
        if ch.whole(table) {
            return Ok(Touch::Everything);
        }
        dirty.insert(table, ch.ids(table).collect());
    }
    // an opening's level and hiding come from the host wall
    if let Some(walls) = dirty.get("wall") {
        if table_exists(db, "opening")? {
            let sql = format!(
                "SELECT id FROM opening WHERE 1=1{}",
                id_list(Some(walls.as_slice()), "wall")
            );
            let mut st = db.prepare(&sql)?;
            let hosted: Vec<i64> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            dirty.entry("opening").or_default().extend(hosted);
        }
    }
    Ok(if dirty.is_empty() {
        Touch::Nothing
    } else {
        Touch::Elements(dirty)
    })
}

/// New graphics after editing elements; with no differences, the same `Arc`.
fn patched(
    m: &Model,
    v: ViewKey,
    prev: &Arc<ViewPaint>,
    dirty: &Dirty,
) -> Result<Arc<ViewPaint>, Error> {
    let g = state(&m.db, v)?.effective;
    let mut patch = Elems::new();
    eval(m, &g, Some(dirty), &mut patch)?;
    if let Some(ops) = dirty.get("opening") {
        let wall_hidden = |w: i64| {
            let fresh = dirty.get("wall").is_some_and(|d| d.contains(&w));
            let e = if fresh {
                patch.get(&("wall", w))
            } else {
                prev.elems.get(&("wall", w))
            };
            e.is_some_and(|e| e.hidden)
        };
        let mut hosted = Elems::new();
        hide_hosted(&m.db, Some(ops.as_slice()), &wall_hidden, &mut hosted)?;
        for (k, e) in hosted {
            patch.entry(k).or_default().hidden = e.hidden;
        }
    }
    let keys = || {
        dirty
            .iter()
            .flat_map(|(t, ids)| ids.iter().map(move |i| (*t, *i)))
    };
    if keys().all(|k| patch.get(&k) == prev.elems.get(&k)) {
        return Ok(prev.clone());
    }
    let mut next = (**prev).clone();
    for k in keys() {
        next.elems.remove(&k);
    }
    next.elems.extend(patch);
    next.any_hidden = next.elems.values().any(|e| e.hidden);
    Ok(Arc::new(next))
}

impl Model {
    /// View graphics per element: object style, view category, filters in order (the upper one
    /// wins, any matching hiding filter hides). With no model changes, the same `Arc`.
    pub fn view_paint(&self, v: ViewKey) -> Result<Arc<ViewPaint>, Error> {
        // inside a transaction the data may roll back and the journal will not know
        if !self.db.is_autocommit() {
            return Ok(Arc::new(full(self, v)?));
        }
        let head = self.change_head();
        let prev = self
            .paint_cache
            .0
            .lock()
            .ok()
            .and_then(|c| c.get(&v).cloned());
        let next = match prev {
            Some((h, p)) if h == head => return Ok(p),
            Some((h, p)) => match touch_of(&self.db, &self.changes_since(h))? {
                Touch::Nothing => p,
                Touch::Elements(d) => patched(self, v, &p, &d)?,
                Touch::Everything => Arc::new(full(self, v)?),
            },
            None => Arc::new(full(self, v)?),
        };
        if let Ok(mut c) = self.paint_cache.0.lock() {
            if c.len() >= CACHE_VIEWS && !c.contains_key(&v) {
                c.clear();
            }
            c.insert(v, (head, next.clone()));
        }
        Ok(next)
    }
}
