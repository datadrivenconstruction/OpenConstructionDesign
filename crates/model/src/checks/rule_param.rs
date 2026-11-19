//! Unfilled project parameters and type marks.

use super::util::{ids, key, quoted, Sink, Within};
use super::{CheckRule, CheckScope, Finding, Fix, Measure, Severity};
use crate::{Edit, Error, Model, PVal, ParamSpec, ParamType};

/// One finding per parameter and category, not per element: hundreds of identical rows add nothing.
const SHOWN: usize = 50;

pub(super) struct MissingParams;

/// Table, selection and level expression for the category's rows.
pub(crate) fn category_rows(cat: &str) -> Option<(&'static str, &'static str, &'static str)> {
    const WALL_LEVEL: &str = "(SELECT level FROM wall WHERE id=t.wall)";
    Some(match cat {
        "door" => ("opening", " AND t.kind='door'", WALL_LEVEL),
        "window" => ("opening", " AND t.kind='window'", WALL_LEVEL),
        "wall" => ("wall", "", "t.level"),
        "curtain_wall" => ("curtain_wall", "", "t.level"),
        "slab" => ("slab", "", "t.level"),
        "room" => ("room", "", "t.level"),
        "col" => ("col", "", "t.level"),
        "beam" => ("beam", "", "t.level"),
        "roof" => ("roof", "", "t.level"),
        "ceiling" => ("ceiling", "", "t.level"),
        "stair" => ("stair", "", "t.level"),
        "railing" => ("railing", "", "t.level"),
        "mep_run" => ("mep_run", "", "t.level"),
        "comp" => ("comp", "", "t.level"),
        "shaft" => ("shaft", "", "t.level"),
        _ => return None,
    })
}

fn report(
    s: &mut Sink<'_>,
    w: &Within,
    table: &str,
    list: Vec<i64>,
    msg_key: &'static str,
    args: Vec<(&'static str, String)>,
) {
    let hit: Vec<i64> = list.into_iter().filter(|id| w.has(table, *id)).collect();
    if hit.is_empty() {
        return;
    }
    let mut full = args;
    full.push(("count", hit.len().to_string()));
    let elements = hit.iter().take(SHOWN).map(|id| key(table, *id)).collect();
    let n = hit.len();
    s.add(
        Severity::Warning,
        elements,
        None,
        msg_key,
        full,
        Fix::None,
        Measure::count(n, 0),
    );
}

impl CheckRule for MissingParams {
    fn id(&self) -> &'static str {
        "missing_params"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        let mut s = Sink::new(out, self.id());
        for d in m.params()? {
            for cat in &d.spec.categories {
                let Some((table, filter, level)) = category_rows(cat) else {
                    continue;
                };
                let (kind, reference) = if d.spec.per_type {
                    let Some(t) = crate::PARAM_TYPE_OF.iter().find(|t| t.0 == cat.as_str()) else {
                        continue;
                    };
                    (t.1, format!("t.{}", t.2))
                } else {
                    (table, "t.id".to_string())
                };
                let sql = format!(
                    "SELECT t.id FROM {table} t LEFT JOIN pparam_val v ON v.param={} AND v.kind={} AND v.ref={reference} \
                     WHERE (v.value IS NULL OR v.value=''){filter}{} ORDER BY t.id",
                    d.id,
                    quoted(kind),
                    w.level_sql(level)
                );
                let list = ids(m, &sql)?;
                report(
                    &mut s,
                    &w,
                    table,
                    list,
                    "ai.check.param_missing",
                    vec![("param", d.spec.name.clone()), ("category", cat.clone())],
                );
            }
        }
        for (col, msg) in [
            ("name", "ai.check.room_no_name"),
            ("number", "ai.check.room_no_number"),
        ] {
            let sql = format!(
                "SELECT id FROM room WHERE trim({col})=''{} ORDER BY id",
                w.level_sql("level")
            );
            report(&mut s, &w, "room", ids(m, &sql)?, msg, vec![]);
        }
        s.done();
        Ok(())
    }
}

const MARK_NAMES: [&str; 4] = ["Type Mark", "type_mark", "Mark", "\u{41c}\u{430}\u{440}\u{43a}\u{430} \u{442}\u{438}\u{43f}\u{430}"];
const NEW_MARK: &str = "Type Mark";

/// Type table, usage query, categories of its instances.
const USED_TYPES: [(&str, &str, &[&str]); 4] = [
    ("wall_type", "SELECT DISTINCT wtype FROM wall", &["wall"]),
    (
        "slab_type",
        "SELECT stype FROM slab UNION SELECT stype FROM roof UNION SELECT stype FROM ceiling",
        &["slab", "roof", "ceiling"],
    ),
    (
        "member_type",
        "SELECT mtype FROM col UNION SELECT mtype FROM beam",
        &["col", "beam"],
    ),
    ("comp_type", "SELECT DISTINCT ctype FROM comp", &["comp"]),
];

pub(super) struct TypeMarks;

/// A fix needs a parameter on the type bound to this category; if there is none at all, it is created.
fn mark_fix(table: &str, cats: &[&str], id: i64, mark: Option<&crate::ParamDef>) -> Fix {
    let value = Edit::ParamValue {
        param: NEW_MARK.to_string(),
        kind: table.to_string(),
        id,
        value: Some(PVal::Text(format!("T{id}"))),
    };
    let Some(d) = mark else {
        let spec = ParamSpec {
            name: NEW_MARK.to_string(),
            ptype: ParamType::Text,
            per_type: true,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: ["wall", "slab", "roof", "ceiling", "col", "beam", "comp"]
                .iter()
                .map(|c| c.to_string())
                .collect(),
        };
        return Fix::Edits(vec![Edit::NewParam(spec), value]);
    };
    if d.spec.name != NEW_MARK
        || !d.spec.per_type
        || !cats
            .iter()
            .all(|c| d.spec.categories.iter().any(|x| x == c))
    {
        return Fix::None;
    }
    Fix::Edits(vec![value])
}

impl CheckRule for TypeMarks {
    fn id(&self) -> &'static str {
        "type_marks"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let w = Within::new(scope);
        if w.level.is_some() {
            // a type does not belong to a level
            return Ok(());
        }
        let names = MARK_NAMES.map(quoted).join(",");
        let defs = m.params()?;
        let marks: Vec<i64> = defs
            .iter()
            .filter(|d| MARK_NAMES.contains(&d.spec.name.as_str()))
            .map(|d| d.id)
            .collect();
        let mark_list = marks
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let mark = defs.iter().find(|d| d.spec.name == NEW_MARK);
        let mut s = Sink::new(out, self.id());
        for (table, used, cats) in USED_TYPES {
            let sql = format!(
                "SELECT t.id FROM {table} t WHERE t.id IN ({used}) \
                 AND NOT EXISTS (SELECT 1 FROM pparam_val v WHERE v.kind={kind} AND v.ref=t.id AND v.param IN ({mark_list}) AND v.value<>'') \
                 AND NOT EXISTS (SELECT 1 FROM prop p WHERE p.kind={kind} AND p.ref=t.id AND p.key IN ({names}) AND p.value<>'') ORDER BY t.id",
                kind = quoted(table)
            );
            for id in ids(m, &sql)? {
                if w.has(table, id) {
                    let fix = mark_fix(table, cats, id, mark);
                    s.add(
                        Severity::Info,
                        vec![key(table, id)],
                        None,
                        "ai.check.type_mark",
                        vec![("table", table.to_string())],
                        fix,
                        Measure::flag(),
                    );
                }
            }
        }
        s.done();
        Ok(())
    }
}
