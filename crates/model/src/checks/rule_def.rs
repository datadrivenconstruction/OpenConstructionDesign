//! Execution of company rules (`RuleDef`): a rule is data, and this code decides the violation.
//! The first finding argument is the rule id: one engine rule serves all company rules.

use std::cmp::Ordering;

use super::util::{key, quoted, Sink, Within};
use super::{
    CheckRule, CheckScope, Cond, Finding, Fix, Measure, RuleCheck, RuleDef, RuleScope, Severity,
};
use crate::{Edit, Error, Model, OpeningSpec, PVal, ParamDef, ParamType};

pub(super) struct Cat {
    pub(super) table: &'static str,
    /// Selection of the category's rows.
    filter: &'static str,
    pub(super) level: &'static str,
    /// Type table and the column referencing it.
    pub(super) of_type: Option<(&'static str, &'static str)>,
    /// Built-in values: a name for the rule mapped to a SQL expression over row `t`.
    builtin: &'static [(&'static str, &'static str)],
    /// x0, y0, x1, y1 in plan; without them the category does not take part in distance checks.
    plan: Option<[&'static str; 4]>,
    /// Extra SQL condition on the rows (` AND ...`), set per run from the rule's `when`.
    pub(super) extra: String,
}

const WALL_LEVEL: &str = "(SELECT level FROM wall WHERE id=t.wall)";
const LEN2D: &str = "sqrt((t.bx-t.ax)*(t.bx-t.ax)+(t.by-t.ay)*(t.by-t.ay))";
const WALL_PLAN: [&str; 4] = [
    "MIN(t.ax,t.bx)",
    "MIN(t.ay,t.by)",
    "MAX(t.ax,t.bx)",
    "MAX(t.ay,t.by)",
];
const POINT_PLAN: [&str; 4] = ["t.x", "t.y", "t.x", "t.y"];
const ROOM_PLAN: [&str; 4] = ["t.px", "t.py", "t.px", "t.py"];
// opening midpoint along the wall chord: for a distance rule the arc does not change the answer noticeably
const OPEN_X: &str = "(SELECT w.ax+(w.bx-w.ax)*(t.along+t.width/2)/NULLIF(sqrt((w.bx-w.ax)*(w.bx-w.ax)+(w.by-w.ay)*(w.by-w.ay)),0) FROM wall w WHERE w.id=t.wall)";
const OPEN_Y: &str = "(SELECT w.ay+(w.by-w.ay)*(t.along+t.width/2)/NULLIF(sqrt((w.bx-w.ax)*(w.bx-w.ax)+(w.by-w.ay)*(w.by-w.ay)),0) FROM wall w WHERE w.id=t.wall)";

const OPENING_VALUES: &[(&str, &str)] = &[
    ("width", "t.width"),
    ("height", "t.height"),
    ("sill", "t.sill"),
    ("along", "t.along"),
    ("kind", "t.kind"),
];

pub(super) fn cat_of(word: &str) -> Option<Cat> {
    let c = |table, level, of_type, builtin, plan| Cat {
        table,
        filter: "",
        level,
        of_type,
        builtin,
        plan,
        extra: String::new(),
    };
    Some(match word.trim().to_lowercase().as_str() {
        "door" => Cat {
            filter: " AND t.kind='door'",
            ..c(
                "opening",
                WALL_LEVEL,
                None,
                OPENING_VALUES,
                Some([OPEN_X, OPEN_Y, OPEN_X, OPEN_Y]),
            )
        },
        "window" => Cat {
            filter: " AND t.kind='window'",
            ..c(
                "opening",
                WALL_LEVEL,
                None,
                OPENING_VALUES,
                Some([OPEN_X, OPEN_Y, OPEN_X, OPEN_Y]),
            )
        },
        "opening" => c(
            "opening",
            WALL_LEVEL,
            None,
            OPENING_VALUES,
            Some([OPEN_X, OPEN_Y, OPEN_X, OPEN_Y]),
        ),
        "wall" => c(
            "wall",
            "t.level",
            Some(("wall_type", "wtype")),
            &[
                ("height", "t.height"),
                ("base_offset", "t.base_offset"),
                ("length", LEN2D),
                (
                    "thickness",
                    "(SELECT SUM(thickness) FROM wall_type_layer WHERE wtype=t.wtype)",
                ),
                ("type", "(SELECT name FROM wall_type WHERE id=t.wtype)"),
            ],
            Some(WALL_PLAN),
        ),
        "room" => c(
            "room",
            "t.level",
            None,
            &[
                ("name", "t.name"),
                ("number", "t.number"),
                ("height", "t.height"),
                ("area", "(SELECT area FROM room_qty WHERE room=t.id)"),
                (
                    "perimeter",
                    "(SELECT perimeter FROM room_qty WHERE room=t.id)",
                ),
                ("volume", "(SELECT volume FROM room_qty WHERE room=t.id)"),
            ],
            Some(ROOM_PLAN),
        ),
        "col" | "column" => c(
            "col",
            "t.level",
            Some(("member_type", "mtype")),
            &[
                ("base_offset", "t.base_offset"),
                ("top_offset", "t.top_offset"),
                ("rotation", "t.rotation"),
                ("type", "(SELECT name FROM member_type WHERE id=t.mtype)"),
            ],
            Some(POINT_PLAN),
        ),
        "beam" => c(
            "beam",
            "t.level",
            Some(("member_type", "mtype")),
            &[("type", "(SELECT name FROM member_type WHERE id=t.mtype)")],
            Some(WALL_PLAN),
        ),
        "slab" | "floor" => c(
            "slab",
            "t.level",
            Some(("slab_type", "stype")),
            &[
                ("offset", "t.offset"),
                ("area", "(SELECT MAX(area) FROM slab_qty WHERE slab=t.id)"),
                ("type", "(SELECT name FROM slab_type WHERE id=t.stype)"),
            ],
            None,
        ),
        "roof" => c(
            "roof",
            "t.level",
            Some(("slab_type", "stype")),
            &[("type", "(SELECT name FROM slab_type WHERE id=t.stype)")],
            None,
        ),
        "ceiling" => c(
            "ceiling",
            "t.level",
            Some(("slab_type", "stype")),
            &[("type", "(SELECT name FROM slab_type WHERE id=t.stype)")],
            None,
        ),
        "comp" | "component" => c(
            "comp",
            "t.level",
            Some(("comp_type", "ctype")),
            &[("type", "(SELECT name FROM comp_type WHERE id=t.ctype)")],
            Some(POINT_PLAN),
        ),
        "curtain" | "curtain_wall" => {
            c("curtain_wall", "t.level", None, &[("name", "t.name")], None)
        }
        "stair" => c(
            "stair",
            "t.level",
            None,
            &[
                ("width", "t.width"),
                ("landing", "t.landing"),
                ("riser", "(SELECT riser FROM stair_qty WHERE stair=t.id)"),
                ("risers", "(SELECT risers FROM stair_qty WHERE stair=t.id)"),
                ("tread", "(SELECT tread FROM stair_qty WHERE stair=t.id)"),
            ],
            None,
        ),
        "railing" => c("railing", "t.level", None, &[], None),
        "mep" | "mep_run" => c("mep_run", "t.level", None, &[], None),
        "shaft" => c("shaft", "t.level", None, &[], None),
        _ => return None,
    })
}

/// FROM clause and row conditions of the category within the rule scope and the check scope.
pub(super) fn selection(cat: &Cat, sc: &RuleScope, w: &Within) -> (String, String) {
    let from = format!("{} t", cat.table);
    let mut cond = format!("1{}{}", cat.filter, w.level_sql(cat.level));
    if let Some(levels) = &sc.levels {
        let list = levels
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        cond += &format!(" AND {} IN ({list})", cat.level);
    }
    if let (Some(name), Some((ty, col))) = (&sc.type_name, cat.of_type) {
        cond += &format!(
            " AND EXISTS (SELECT 1 FROM {ty} y WHERE y.id=t.{col} AND y.name={})",
            quoted(name)
        );
    }
    cond += &cat.extra;
    (from, cond)
}

/// ` AND t.id IN (...)` for a rule that applies only where `key` equals `want`; the value is
/// read like any rule value (built-in, project parameter or stored property).
fn when_sql(m: &Model, cat: &Cat, key: &str, want: &str) -> Result<String, Error> {
    let src = source(m, cat, key)?;
    let plain = RuleScope {
        cats: vec![],
        levels: None,
        type_name: None,
        when: None,
    };
    let inner = values_sql(cat, &plain, &Within::new(&CheckScope::All), &src, key);
    let w = want.trim().to_lowercase();
    let accepted: &[&str] = match w.as_str() {
        "yes" | "true" | "1" => &["yes", "true", "1"],
        "no" | "false" | "0" => &["no", "false", "0"],
        _ => &[],
    };
    let list = if accepted.is_empty() {
        quoted(&w)
    } else {
        accepted.iter().map(|a| quoted(a)).collect::<Vec<_>>().join(",")
    };
    Ok(format!(
        " AND t.id IN (SELECT id FROM ({inner}) WHERE lower(trim(CAST(val AS TEXT))) IN ({list}))"
    ))
}

pub(super) enum Source {
    Builtin(&'static str),
    Project(ParamDef),
    Prop,
}

pub(super) fn source(m: &Model, cat: &Cat, param: &str) -> Result<Source, Error> {
    let want = param.trim().to_lowercase();
    if let Some(b) = cat.builtin.iter().find(|b| b.0 == want) {
        return Ok(Source::Builtin(b.1));
    }
    Ok(m.param_by_name(param)?
        .map_or(Source::Prop, Source::Project))
}

/// Subquery (id, val): the parameter value for every row of the category.
pub(super) fn values_sql(
    cat: &Cat,
    sc: &RuleScope,
    w: &Within,
    src: &Source,
    param: &str,
) -> String {
    let (from, cond) = selection(cat, sc, w);
    let prop = format!(
        "LEFT JOIN prop p ON p.kind={} AND p.ref=t.id AND p.key={}",
        quoted(cat.table),
        quoted(param.trim())
    );
    match src {
        Source::Builtin(expr) => format!("SELECT t.id id, {expr} val FROM {from} WHERE {cond}"),
        Source::Prop => format!("SELECT t.id id, p.value val FROM {from} {prop} WHERE {cond}"),
        Source::Project(d) => {
            let on = match (d.spec.per_type, cat.of_type) {
                (true, Some((ty, col))) => format!("v.kind={} AND v.ref=t.{col}", quoted(ty)),
                _ => format!("v.kind={} AND v.ref=t.id", quoted(cat.table)),
            };
            format!(
                "SELECT t.id id, COALESCE(v.value, p.value) val FROM {from} LEFT JOIN pparam_val v ON v.param={} AND {on} {prop} WHERE {cond}",
                d.id
            )
        }
    }
}

pub(super) fn violators(m: &Model, sql: &str) -> Result<Vec<(i64, Option<String>)>, Error> {
    let mut st = m.db.prepare(sql)?;
    let v = st
        .query_map([], |r| {
            let val: rusqlite::types::Value = r.get(1)?;
            let text = match val {
                rusqlite::types::Value::Null => None,
                rusqlite::types::Value::Integer(i) => Some(i.to_string()),
                rusqlite::types::Value::Real(x) => Some(x.to_string()),
                rusqlite::types::Value::Text(t) => Some(t),
                rusqlite::types::Value::Blob(_) => Some(String::new()),
            };
            Ok((r.get::<_, i64>(0)?, text))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

pub(super) fn args(
    r: &RuleDef,
    param: &str,
    value: &str,
    limit: String,
) -> Vec<(&'static str, String)> {
    vec![
        ("rule", r.id.clone()),
        ("title", r.title.clone()),
        ("param", param.to_string()),
        ("value", value.to_string()),
        ("limit", limit),
    ]
}

pub(super) fn range_text(min: Option<f64>, max: Option<f64>) -> String {
    match (min, max) {
        (Some(a), Some(b)) => format!("{a}..{b}"),
        (Some(a), None) => format!(">= {a}"),
        (None, Some(b)) => format!("<= {b}"),
        (None, None) => String::new(),
    }
}

/// A door or window width changes keeping the midpoint, provided the opening still fits.
fn opening_width_fix(m: &Model, id: i64, width: f64) -> Result<Fix, Error> {
    let (wall, along, old, sill, height, kind): (i64, f64, f64, f64, f64, String) =
        m.db.query_row(
            "SELECT wall, along, width, sill, height, kind FROM opening WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )?;
    let spec = OpeningSpec {
        along: (along - (width - old) / 2.0).max(0.0),
        width,
        sill,
        height,
        kind,
    };
    if m.opening_fits(wall, &spec).is_err() {
        return Ok(Fix::None);
    }
    Ok(Fix::Edits(vec![Edit::Opening { id, wall, spec }]))
}

pub(super) fn value_edit(
    d: &ParamDef,
    cat: &Cat,
    m: &Model,
    id: i64,
    value: PVal,
) -> Result<Fix, Error> {
    let (kind, target) = match (d.spec.per_type, cat.of_type) {
        (true, Some((ty, col))) => {
            let target: i64 = m.db.query_row(
                &format!("SELECT {col} FROM {} WHERE id=?1", cat.table),
                [id],
                |r| r.get(0),
            )?;
            (ty, target)
        }
        (true, None) => return Ok(Fix::None),
        (false, _) => (cat.table, id),
    };
    Ok(Fix::Edits(vec![Edit::ParamValue {
        param: d.spec.name.clone(),
        kind: kind.to_string(),
        id: target,
        value: Some(value),
    }]))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn range_fix(
    m: &Model,
    cat: &Cat,
    src: &Source,
    param: &str,
    id: i64,
    value: Option<&str>,
    min: Option<f64>,
    max: Option<f64>,
) -> Result<Fix, Error> {
    let Some(v) = value.and_then(|t| t.trim().parse::<f64>().ok()) else {
        return Ok(Fix::None);
    };
    let mut to = v;
    if let Some(a) = min {
        to = to.max(a);
    }
    if let Some(b) = max {
        to = to.min(b);
    }
    match src {
        Source::Builtin(_)
            if cat.table == "opening" && param.trim().eq_ignore_ascii_case("width") =>
        {
            opening_width_fix(m, id, to)
        }
        Source::Project(d) => match d.spec.ptype {
            ParamType::Number | ParamType::Length | ParamType::Area | ParamType::Volume => {
                value_edit(d, cat, m, id, PVal::Num(to))
            }
            _ => Ok(Fix::None),
        },
        _ => Ok(Fix::None),
    }
}

/// Unit of a built-in value by its name.
pub(super) fn builtin_unit(param: &str) -> &'static str {
    match param.trim().to_lowercase().as_str() {
        "width" | "height" | "sill" | "along" | "length" | "thickness" | "riser" | "tread"
        | "base_offset" | "top_offset" | "offset" | "perimeter" | "landing" | "min_width" => "m",
        "area" => "m2",
        "volume" => "m3",
        _ => "",
    }
}

/// Value against the bound it breaks; yes/no conditions are 1 against 0.
fn param_measure(
    check: &RuleCheck,
    src: &Source,
    cat: &Cat,
    param: &str,
    value: Option<&str>,
    empty: bool,
) -> Measure {
    if empty {
        return Measure::count(0, 1);
    }
    let RuleCheck::ParamRange { min, max, .. } = check else {
        return Measure::flag();
    };
    let unit = source_unit(src, param);
    let Some(v) = value.and_then(|t| t.trim().parse::<f64>().ok()) else {
        return Measure::flag();
    };
    let limit = match (min, max) {
        (Some(a), _) if v < *a => *a,
        (_, Some(b)) => *b,
        (Some(a), None) => *a,
        (None, None) => v,
    };
    Measure::new(v, limit, unit)
}

/// Unit of a value in model terms (`m`, `m2`, `m3`); empty for a plain number or text.
pub(super) fn source_unit(src: &Source, param: &str) -> &'static str {
    match src {
        Source::Project(d) => match d.spec.ptype {
            ParamType::Length => "m",
            ParamType::Area => "m2",
            ParamType::Volume => "m3",
            _ => "",
        },
        Source::Builtin(_) => builtin_unit(param),
        _ => "",
    }
}

fn run_param(m: &Model, r: &RuleDef, cat: &Cat, w: &Within, s: &mut Sink<'_>) -> Result<(), Error> {
    let (param, filter) = match &r.check {
        RuleCheck::ParamPresent { param } => (param, "val IS NULL OR val=''".to_string()),
        RuleCheck::ParamRange { param, min, max } => {
            let mut f = "val IS NULL OR val=''".to_string();
            if let Some(a) = min {
                f += &format!(" OR CAST(val AS REAL)<{a:?}");
            }
            if let Some(b) = max {
                f += &format!(" OR CAST(val AS REAL)>{b:?}");
            }
            (param, f)
        }
        RuleCheck::ParamIn { param, allowed } => {
            let list = allowed
                .iter()
                .map(|a| quoted(a))
                .collect::<Vec<_>>()
                .join(",");
            (
                param,
                format!("val IS NULL OR CAST(val AS TEXT) NOT IN ({list})"),
            )
        }
        _ => return Ok(()),
    };
    let src = source(m, cat, param)?;
    let inner = values_sql(cat, &r.scope, w, &src, param);
    let sql = format!("SELECT id, val FROM ({inner}) WHERE {filter} ORDER BY id");
    for (id, value) in violators(m, &sql)? {
        if !w.has(cat.table, id) {
            continue;
        }
        let empty = value.as_deref().is_none_or(str::is_empty);
        let shown = value.as_deref().unwrap_or("");
        let (msg, limit, fix): (&'static str, String, Fix) = match &r.check {
            RuleCheck::ParamRange { min, max, .. } if !empty => (
                "ai.check.rule.param_range",
                range_text(*min, *max),
                range_fix(m, cat, &src, param, id, value.as_deref(), *min, *max)?,
            ),
            RuleCheck::ParamIn { allowed, .. } if !empty => {
                let fix = match (allowed.as_slice(), &src) {
                    ([one], Source::Project(d)) if d.spec.ptype == ParamType::Text => {
                        value_edit(d, cat, m, id, PVal::Text(one.clone()))?
                    }
                    _ => Fix::None,
                };
                ("ai.check.rule.param_in", allowed.join(", "), fix)
            }
            RuleCheck::ParamIn { allowed, .. } => {
                let fix = match (allowed.as_slice(), &src) {
                    ([one], Source::Project(d)) if d.spec.ptype == ParamType::Text => {
                        value_edit(d, cat, m, id, PVal::Text(one.clone()))?
                    }
                    _ => Fix::None,
                };
                ("ai.check.rule.param_missing", allowed.join(", "), fix)
            }
            _ => ("ai.check.rule.param_missing", String::new(), Fix::None),
        };
        let measure = param_measure(&r.check, &src, cat, param, value.as_deref(), empty);
        s.add(
            r.severity,
            vec![key(cat.table, id)],
            None,
            msg,
            args(r, param, shown, limit),
            fix,
            measure,
        );
    }
    Ok(())
}

fn run_count(
    m: &Model,
    r: &RuleDef,
    cats: &[Cat],
    w: &Within,
    min: Option<u32>,
    max: Option<u32>,
    s: &mut Sink<'_>,
) -> Result<(), Error> {
    // an element count concerns the whole model: it means nothing on a part of the model
    if w.is_elements() {
        return Ok(());
    }
    let mut total: i64 = 0;
    for cat in cats {
        let (from, cond) = selection(cat, &r.scope, w);
        let n: i64 = m.db.query_row(
            &format!("SELECT COUNT(*) FROM {from} WHERE {cond}"),
            [],
            |x| x.get(0),
        )?;
        total += n;
    }
    let low = min.is_some_and(|a| total < i64::from(a));
    let high = max.is_some_and(|b| total > i64::from(b));
    if low || high {
        let limit = range_text(min.map(f64::from), max.map(f64::from));
        s.add(
            r.severity,
            vec![],
            None,
            "ai.check.rule.count",
            args(r, "", &total.to_string(), limit),
            Fix::None,
            Measure::count(
                usize::try_from(total).unwrap_or(0),
                if low {
                    min.map_or(0, |a| a as usize)
                } else {
                    max.map_or(0, |b| b as usize)
                },
            ),
        );
    }
    Ok(())
}

#[derive(Clone)]
struct Plan {
    table: &'static str,
    id: i64,
    level: i64,
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

fn plan_rows(m: &Model, cat: &Cat, sc: &RuleScope, w: &Within) -> Result<Vec<Plan>, Error> {
    let Some(p) = cat.plan else { return Ok(vec![]) };
    let (from, cond) = selection(cat, sc, w);
    let sql = format!(
        "SELECT t.id, {}, {}, {}, {}, {} FROM {from} WHERE {cond} ORDER BY t.id",
        cat.level, p[0], p[1], p[2], p[3]
    );
    let mut st = m.db.prepare(&sql)?;
    let table = cat.table;
    let v = st
        .query_map([], |r| {
            Ok(Plan {
                table,
                id: r.get(0)?,
                level: r.get(1)?,
                x0: r.get(2)?,
                y0: r.get(3)?,
                x1: r.get(4)?,
                y1: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<Plan>, _>>()?;
    Ok(v)
}

fn gap(a: &Plan, b: &Plan) -> f64 {
    let dx = (a.x0 - b.x1).max(b.x0 - a.x1).max(0.0);
    let dy = (a.y0 - b.y1).max(b.y0 - a.y1).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

fn run_clearance(
    m: &Model,
    r: &RuleDef,
    cats: &[Cat],
    other: &[String],
    min_m: f64,
    w: &Within,
    s: &mut Sink<'_>,
) -> Result<(), Error> {
    let others: Vec<Option<Cat>> = other.iter().map(|o| cat_of(o)).collect();
    if cats.iter().any(|c| c.plan.is_none())
        || others
            .iter()
            .any(|c| c.as_ref().is_none_or(|x| x.plan.is_none()))
    {
        s.add(
            Severity::Info,
            vec![],
            None,
            "ai.check.rule.unsupported",
            args(r, "", "", String::new()),
            Fix::None,
            Measure::flag(),
        );
        return Ok(());
    }
    let all = RuleScope {
        cats: vec![],
        levels: r.scope.levels.clone(),
        type_name: None,
        when: None,
    };
    let mut bs: Vec<Plan> = vec![];
    for c in others.iter().flatten() {
        bs.extend(plan_rows(m, c, &all, &Within::new(&CheckScope::All))?);
    }
    bs.sort_by(|a, b| a.x0.total_cmp(&b.x0).then(a.id.cmp(&b.id)));
    // right to left with pruning: a long wall on the left prevents stopping too early
    let mut reach = Vec::with_capacity(bs.len());
    let mut far = f64::MIN;
    for b in &bs {
        far = far.max(b.x1);
        reach.push(far);
    }
    for cat in cats {
        for a in plan_rows(m, cat, &r.scope, w)? {
            if s.full() {
                return Ok(());
            }
            if !w.has(a.table, a.id) {
                continue;
            }
            let end = bs.partition_point(|b| b.x0 <= a.x1 + min_m);
            let mut best: Option<(f64, &Plan)> = None;
            for i in (0..end).rev() {
                if reach[i] < a.x0 - min_m {
                    break;
                }
                let b = &bs[i];
                if b.level != a.level || (b.table == a.table && b.id == a.id) {
                    continue;
                }
                let d = gap(&a, b);
                let closer = best.is_none_or(|(x, o)| match d.total_cmp(&x) {
                    Ordering::Less => true,
                    Ordering::Equal => (b.table, b.id) < (o.table, o.id),
                    Ordering::Greater => false,
                });
                if d < min_m - 1e-9 && closer {
                    best = Some((d, b));
                }
            }
            if let Some((d, b)) = best {
                let elements = vec![key(a.table, a.id), key(b.table, b.id)];
                s.add(
                    r.severity,
                    elements,
                    None,
                    "ai.check.rule.clearance",
                    args(r, "", &format!("{d:.3}"), format!("{min_m}")),
                    Fix::None,
                    Measure::new(d, min_m, "m"),
                );
            }
        }
    }
    Ok(())
}

pub(super) struct CompanyRules;

impl CheckRule for CompanyRules {
    fn id(&self) -> &'static str {
        "company_rules"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        run_rules(m, &m.project_rules()?, scope, out)
    }
}

/// Runs `rules` in order into `out`; one cap on findings for the whole call.
pub(super) fn run_rules(
    m: &Model,
    rules: &[RuleDef],
    scope: &CheckScope,
    out: &mut Vec<Finding>,
) -> Result<(), Error> {
    let w = Within::new(scope);
    let mut s = Sink::new(out, "company_rules");
    for r in rules {
        let mut cats = Vec::with_capacity(r.scope.cats.len());
        let mut unknown = false;
        for word in &r.scope.cats {
            match cat_of(word) {
                Some(mut c) => {
                    if let Some(Cond::Eq(k, v)) = &r.scope.when {
                        c.extra = when_sql(m, &c, k, v)?;
                    }
                    cats.push(c);
                }
                None => unknown = true,
            }
        }
        if unknown {
            s.add(
                Severity::Info,
                vec![],
                None,
                "ai.check.rule.unsupported",
                args(r, "", "", String::new()),
                Fix::None,
                Measure::flag(),
            );
            continue;
        }
        match &r.check {
            RuleCheck::Count { min, max } => run_count(m, r, &cats, &w, *min, *max, &mut s)?,
            RuleCheck::Clearance { other, min_m } => {
                run_clearance(m, r, &cats, other, *min_m, &w, &mut s)?
            }
            RuleCheck::Unique {
                param,
                case_sensitive,
            } => {
                for cat in &cats {
                    run_unique(m, r, cat, param, *case_sensitive, &w, &mut s)?;
                }
            }
            RuleCheck::Value { .. } => {
                for cat in &cats {
                    super::rule_req::run(m, r, cat, &w, &mut s)?;
                }
            }
            RuleCheck::Compare { .. } | RuleCheck::Cases { .. } => {
                for cat in &cats {
                    super::rule_cases::run(m, r, cat, &w, &mut s)?;
                }
            }
            _ => {
                for cat in &cats {
                    run_param(m, r, cat, &w, &mut s)?;
                }
            }
        }
    }
    s.done();
    Ok(())
}

/// Elements that share a trimmed non-blank value anywhere in the model; each one inside the
/// checked area is a finding with the size of its group.
fn run_unique(
    m: &Model,
    r: &RuleDef,
    cat: &Cat,
    param: &str,
    case_sensitive: bool,
    w: &Within,
    s: &mut Sink<'_>,
) -> Result<(), Error> {
    let src = source(m, cat, param)?;
    let whole = Within::new(&CheckScope::All);
    let inner = values_sql(cat, &r.scope, &whole, &src, param);
    let sql = format!("SELECT id, val FROM ({inner}) WHERE val IS NOT NULL ORDER BY id");
    let norm = |v: &str| {
        let t = v.trim();
        if case_sensitive {
            t.to_string()
        } else {
            t.to_lowercase()
        }
    };
    let area_sql = format!("SELECT id, val FROM ({})", values_sql(cat, &r.scope, w, &src, param));
    let in_area: std::collections::HashSet<i64> =
        violators(m, &area_sql)?.into_iter().map(|x| x.0).collect();
    let mut rows: Vec<(String, i64, String)> = violators(m, &sql)?
        .into_iter()
        .filter_map(|(id, v)| {
            let v = v?;
            let k = norm(&v);
            (!k.is_empty()).then_some((k, id, v.trim().to_string()))
        })
        .collect();
    rows.sort();
    let mut i = 0;
    while i < rows.len() {
        let mut j = i;
        while j < rows.len() && rows[j].0 == rows[i].0 {
            j += 1;
        }
        let n = j - i;
        if n > 1 {
            for (_, id, v) in &rows[i..j] {
                if !w.has(cat.table, *id) || !in_area.contains(id) {
                    continue;
                }
                s.add(
                    r.severity,
                    vec![key(cat.table, *id)],
                    None,
                    "ai.check.rule.unique",
                    args(r, param, v, "1".to_string()),
                    Fix::None,
                    Measure::count(n, 1),
                );
            }
        }
        i = j;
    }
    Ok(())
}
