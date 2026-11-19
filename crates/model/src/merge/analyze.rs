//! Compares three files with SQL set operations in temp tables: `mg_map` maps their id to mine,
//! `mg_add` holds the peer's new rows, `mg_ch` holds base rows with the state on each side,
//! `mg_cd` holds owners of differing parts. Elements changed on both sides are then compared
//! field by field in Rust. Deterministic: preview and apply give the same result.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::{params, Connection, OptionalExtension, Statement};
use serde_json::Value as J;

use super::tables::{self, canon, mapped, q, ChildTable, RefCol, Schema, Table};
use super::{FieldConflict, MergeCase, MergeConflict, MergeCounts, MergePlan, Prepared};
use crate::source::{decide, enc, same_value, show, Decision, SqlValue};
use crate::Error;

pub(crate) const TAKE: i64 = 1;
pub(crate) const DELETE: i64 = 2;
pub(crate) const DEL_THERE: i64 = 4;
pub(crate) const DELETE_CASCADE: i64 = 6;
pub(crate) const RESTORE: i64 = 7;

const TEMP_DDL: &str = "
DROP TABLE IF EXISTS temp.mg_map;
DROP TABLE IF EXISTS temp.mg_add;
DROP TABLE IF EXISTS temp.mg_ch;
DROP TABLE IF EXISTS temp.mg_cd;
CREATE TEMP TABLE mg_map(tbl TEXT NOT NULL, their INTEGER NOT NULL, mine INTEGER NOT NULL,
    PRIMARY KEY(tbl, their)) WITHOUT ROWID;
CREATE INDEX temp.mg_map_mine ON mg_map(tbl, mine);
CREATE TEMP TABLE mg_add(tbl TEXT NOT NULL, their INTEGER NOT NULL, PRIMARY KEY(tbl, their)) WITHOUT ROWID;
-- b: id in base and theirs; t, m: 0 as in base, 1 changed, 2 deleted, 3 new work depends on it;
-- nb: row is not in base (added on both sides)
CREATE TEMP TABLE mg_ch(tbl TEXT NOT NULL, b INTEGER NOT NULL, t INTEGER NOT NULL DEFAULT 0,
    m INTEGER NOT NULL DEFAULT 0, nb INTEGER NOT NULL DEFAULT 0, act INTEGER NOT NULL DEFAULT 0, mine INTEGER,
    PRIMARY KEY(tbl, b)) WITHOUT ROWID;
CREATE INDEX temp.mg_ch_mine ON mg_ch(tbl, mine);
CREATE INDEX temp.mg_ch_act ON mg_ch(tbl, act);
CREATE TEMP TABLE mg_cd(side TEXT NOT NULL, k TEXT NOT NULL, o INTEGER NOT NULL, c INTEGER NOT NULL,
    PRIMARY KEY(side, k, o, c)) WITHOUT ROWID;
";

pub(crate) fn drop_temp(db: &Connection) -> Result<(), Error> {
    db.execute_batch(
        "DROP TABLE IF EXISTS temp.mg_map;
         DROP TABLE IF EXISTS temp.mg_add;
         DROP TABLE IF EXISTS temp.mg_ch;
         DROP TABLE IF EXISTS temp.mg_cd;",
    )?;
    Ok(())
}

/// Fields of an element changed on both sides: what is taken automatically and what awaits a choice.
pub(crate) struct Take {
    pub table: usize,
    pub b: i64,
    pub mine: i64,
    pub cols: Vec<String>,
    pub kids: Vec<(usize, String)>,
    pub conflict_cols: Vec<String>,
    pub conflict_kids: Vec<(usize, String)>,
}

pub(crate) struct Analysis {
    pub schema: Schema,
    pub takes: Vec<Take>,
    pub plan: MergePlan,
}

fn load_maps(db: &Connection, p: &Prepared) -> Result<(), Error> {
    if let Some(peer) = p.peer {
        db.execute(
            "INSERT OR IGNORE INTO temp.mg_map(tbl,their,mine) SELECT tbl,their,mine FROM main.merge_map WHERE peer=?1",
            [peer],
        )?;
    }
    for r in &p.reverse {
        // in their merge_map `their` is my id and `mine` is theirs
        db.execute(
            "INSERT OR IGNORE INTO temp.mg_map(tbl,their,mine) SELECT tbl,mine,their FROM their.merge_map WHERE peer=?1",
            [r],
        )?;
    }
    Ok(())
}

fn map_table(db: &Connection, t: &Table) -> Result<(), Error> {
    let n = &t.name;
    let nq = q(n);
    let fresh = format!(
        "t.id NOT IN (SELECT id FROM base.{nq}) AND t.id NOT IN (SELECT y.their FROM temp.mg_map y WHERE y.tbl='{n}')"
    );
    for u in &t.uniq {
        let on = u
            .iter()
            .map(|c| format!("m.{} IS {}", q(c), canon(&t.refs, "t", c)))
            .collect::<Vec<_>>()
            .join(" AND ");
        db.execute(
            &format!(
                "INSERT OR IGNORE INTO temp.mg_map(tbl,their,mine) SELECT '{n}', t.id, m.id \
                 FROM their.{nq} t JOIN main.{nq} m ON {on} WHERE {fresh}"
            ),
            [],
        )?;
    }
    if n == "site_info" {
        db.execute(
            &format!(
                "INSERT OR IGNORE INTO temp.mg_map(tbl,their,mine) SELECT '{n}', t.id, t.id \
                 FROM their.{nq} t WHERE t.id IN (SELECT id FROM main.{nq}) AND {fresh}"
            ),
            [],
        )?;
    }
    let max: i64 = db.query_row(
        &format!(
            "SELECT MAX(COALESCE((SELECT MAX(id) FROM main.{nq}),0), \
             COALESCE((SELECT MAX(y.mine) FROM temp.mg_map y WHERE y.tbl='{n}'),0))"
        ),
        [],
        |r| r.get(0),
    )?;
    db.execute(
        &format!(
            "INSERT INTO temp.mg_add(tbl,their) SELECT '{n}', t.id FROM their.{nq} t WHERE {fresh}"
        ),
        [],
    )?;
    db.execute(
        &format!(
            "INSERT INTO temp.mg_map(tbl,their,mine) SELECT '{n}', a.their, {max}+ROW_NUMBER() OVER (ORDER BY a.their) \
             FROM temp.mg_add a WHERE a.tbl='{n}'"
        ),
        [],
    )?;
    Ok(())
}

fn diff_table(db: &Connection, t: &Table, identity: bool) -> Result<(), Error> {
    let n = &t.name;
    let nq = q(n);
    let bm = if identity {
        "b.id".to_string()
    } else {
        mapped("b.id", n)
    };
    if !t.cols.is_empty() {
        let dt = t
            .cols
            .iter()
            .map(|c| format!("t.{0} IS NOT b.{0}", q(c)))
            .collect::<Vec<_>>()
            .join(" OR ");
        db.execute(
            &format!(
                "INSERT INTO temp.mg_ch(tbl,b,t) SELECT '{n}', t.id, 1 FROM their.{nq} t JOIN base.{nq} b ON b.id=t.id \
                 WHERE {dt} ON CONFLICT(tbl,b) DO UPDATE SET t=1"
            ),
            [],
        )?;
        let dm = t
            .cols
            .iter()
            .map(|c| {
                let right = if identity {
                    format!("b.{}", q(c))
                } else {
                    canon(&t.refs, "b", c)
                };
                format!("m.{} IS NOT {right}", q(c))
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        db.execute(
            &format!(
                "INSERT INTO temp.mg_ch(tbl,b,m) SELECT '{n}', b.id, 1 FROM base.{nq} b CROSS JOIN main.{nq} m ON m.id={bm} \
                 WHERE {dm} ON CONFLICT(tbl,b) DO UPDATE SET m=1"
            ),
            [],
        )?;
    }
    db.execute(
        &format!(
            "INSERT INTO temp.mg_ch(tbl,b,t) SELECT '{n}', b.id, 2 FROM base.{nq} b \
             WHERE NOT EXISTS(SELECT 1 FROM their.{nq} t WHERE t.id=b.id) ON CONFLICT(tbl,b) DO UPDATE SET t=2"
        ),
        [],
    )?;
    db.execute(
        &format!(
            "INSERT INTO temp.mg_ch(tbl,b,m) SELECT '{n}', b.id, 2 FROM base.{nq} b \
             WHERE NOT EXISTS(SELECT 1 FROM main.{nq} m WHERE m.id={bm}) ON CONFLICT(tbl,b) DO UPDATE SET m=2"
        ),
        [],
    )?;
    // known on both sides, absent from the base (same unique name, or merged earlier the other way)
    let dk = if t.cols.is_empty() {
        "0".to_string()
    } else {
        t.cols
            .iter()
            .map(|c| format!("m.{} IS NOT {}", q(c), canon(&t.refs, "t", c)))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    let tm = mapped("t.id", n);
    db.execute(
        &format!(
            "INSERT OR IGNORE INTO temp.mg_ch(tbl,b,t,m,nb) SELECT '{n}', t.id, 1, 1, 1 \
             FROM their.{nq} t CROSS JOIN main.{nq} m ON m.id={tm} \
             WHERE t.id NOT IN (SELECT id FROM base.{nq}) \
             AND t.id NOT IN (SELECT a.their FROM temp.mg_add a WHERE a.tbl='{n}') AND ({dk})"
        ),
        [],
    )?;
    Ok(())
}

/// Part rows that differ between files: owners go to `mg_cd`, state to `mg_ch`.
fn diff_children(db: &Connection, s: &Schema, identity: bool) -> Result<(), Error> {
    for (ci, c) in s.children.iter().enumerate() {
        let cq = q(c.spec.table);
        let plain = |schema: &str| {
            let cols = c
                .cols
                .iter()
                .map(|x| format!("r.{0} AS {0}", q(x)))
                .collect::<Vec<_>>()
                .join(",");
            format!("SELECT {cols} FROM {schema}.{cq} r")
        };
        let base_m = if identity {
            plain("base")
        } else {
            let cols = c
                .cols
                .iter()
                .map(|x| format!("{} AS {}", canon(&c.refs, "r", x), q(x)))
                .collect::<Vec<_>>()
                .join(",");
            format!("SELECT {cols} FROM base.{cq} r")
        };
        let k = match c.spec.owner {
            tables::Owner::Col(_, parent) => format!("'{parent}'"),
            tables::Owner::Poly(kc, _) => q(kc),
        };
        let o = q(c.owner_col());
        let their = plain("their");
        let base = plain("base");
        let main = plain("main");
        for (side, a, b) in [
            ("t", &their, &base),
            ("t", &base, &their),
            ("m", &main, &base_m),
            ("m", &base_m, &main),
        ] {
            db.execute(
                &format!(
                    "INSERT OR IGNORE INTO temp.mg_cd(side,k,o,c) SELECT '{side}', {k}, {o}, {ci} \
                     FROM ({a} EXCEPT {b}) WHERE {o} IS NOT NULL"
                ),
                [],
            )?;
        }
    }
    for t in &s.tables {
        let n = &t.name;
        let nq = q(n);
        db.execute(
            &format!(
                "INSERT INTO temp.mg_ch(tbl,b,t) SELECT DISTINCT '{n}', d.o, 1 FROM temp.mg_cd d \
                 WHERE d.side='t' AND d.k='{n}' AND d.o IN (SELECT id FROM base.{nq}) \
                 ON CONFLICT(tbl,b) DO UPDATE SET t=MAX(t,1)"
            ),
            [],
        )?;
        let back = if identity {
            "d.o".to_string()
        } else {
            format!(
                "COALESCE((SELECT y.their FROM temp.mg_map y WHERE y.tbl='{n}' AND y.mine=d.o),d.o)"
            )
        };
        db.execute(
            &format!(
                "INSERT INTO temp.mg_ch(tbl,b,m) SELECT DISTINCT '{n}', {back}, 1 FROM temp.mg_cd d \
                 WHERE d.side='m' AND d.k='{n}' AND {back} IN (SELECT id FROM base.{nq}) \
                 ON CONFLICT(tbl,b) DO UPDATE SET m=MAX(m,1)"
            ),
            [],
        )?;
    }
    Ok(())
}

/// A deletion with dependent work on the other side becomes a conflict.
fn escalate(db: &Connection, s: &Schema) -> Result<(), Error> {
    db.execute_batch(
        "UPDATE temp.mg_ch SET mine=COALESCE((SELECT x.mine FROM temp.mg_map x WHERE x.tbl=mg_ch.tbl AND x.their=mg_ch.b), mg_ch.b);",
    )?;
    for t in s.tables.iter().rev() {
        let (c, cq) = (&t.name, q(&t.name));
        for r in &t.refs {
            let RefCol::To { col, parent } = r else {
                continue;
            };
            if s.table(parent).is_none() || parent == c {
                continue;
            }
            let col = q(col);
            db.execute(
                &format!(
                    "UPDATE temp.mg_ch SET m=3 WHERE tbl='{parent}' AND t=2 AND m=0 AND mine IN \
                     (SELECT r.{col} FROM main.{cq} r WHERE r.{col} IS NOT NULL AND r.id NOT IN \
                      (SELECT z.mine FROM temp.mg_ch z WHERE z.tbl='{c}' AND z.t=2 AND z.m=0))"
                ),
                [],
            )?;
            db.execute(
                &format!(
                    "UPDATE temp.mg_ch SET t=3 WHERE tbl='{parent}' AND m=2 AND t=0 AND b IN \
                     (SELECT r.{col} FROM their.{cq} r JOIN temp.mg_add a ON a.tbl='{c}' AND a.their=r.id)"
                ),
                [],
            )?;
        }
    }
    db.execute_batch(
        "UPDATE temp.mg_ch SET act=CASE
            WHEN t=1 AND m=0 THEN 1
            WHEN t=2 AND m=0 THEN 2
            WHEN t=1 AND m=1 THEN 3
            WHEN t=2 AND m IN (1,3) THEN 4
            WHEN t IN (1,3) AND m=2 THEN 5
            ELSE 0 END;",
    )?;
    Ok(())
}

/// SQL does not translate ids inside a row's text references (dimensions, constraints, edges); Rust does.
pub(crate) fn canon_text(
    table: &str,
    col: &str,
    text: &str,
    kind: Option<&str>,
    map: &mut dyn FnMut(&str, i64) -> Result<i64, Error>,
) -> Result<String, Error> {
    use crate::annot::{parse_ref, refs_text, DimRef as D};
    use crate::constraints::{ConstraintKind, ConstraintSpec, Ref};
    Ok(match (table, col) {
        ("dim", "refs") => {
            let parsed: Option<Vec<D>> = text.split(';').map(parse_ref).collect();
            let Some(mut rs) = parsed else {
                return Ok(text.to_string());
            };
            for r in &mut rs {
                match r {
                    D::Point(_) => {}
                    D::WallEnd { wall, .. }
                    | D::AxisEnd { wall, .. }
                    | D::AxisLine { wall }
                    | D::ArcFace { wall, .. }
                    | D::WallLevel { wall, .. } => *wall = map("wall", *wall)?,
                    D::OpeningEdge { opening, .. } => *opening = map("opening", *opening)?,
                    D::GridAt { grid, wall } => {
                        *grid = map("grid", *grid)?;
                        *wall = map("wall", *wall)?;
                    }
                }
            }
            refs_text(&rs)
        }
        ("constraint_rule", "walls") => {
            let Some(k) = kind.and_then(ConstraintKind::parse) else {
                return Ok(text.to_string());
            };
            let walls: Result<Vec<i64>, _> = text.split_whitespace().map(str::parse).collect();
            let Ok(walls) = walls else {
                return Ok(text.to_string());
            };
            let spec = ConstraintSpec {
                kind: k,
                walls,
                value: None,
            };
            let mut out = vec![];
            for r in spec.refs() {
                out.push(
                    match r {
                        Ref::Wall(i) => map("wall", i)?,
                        Ref::Grid(i) => map("grid", i)?,
                        Ref::Level(i) => map("level", i)?,
                    }
                    .to_string(),
                );
            }
            out.join(" ")
        }
        ("sweep", "hosts") => {
            let Ok(ids) = serde_json::from_str::<Vec<i64>>(text) else {
                return Ok(text.to_string());
            };
            let tbl = kind.unwrap_or("wall");
            let mut out = Vec::with_capacity(ids.len());
            for i in ids {
                out.push(map(tbl, i)?);
            }
            serde_json::to_string(&out)?
        }
        _ => text.to_string(),
    })
}

/// Column with text references and the kind column (see [`canon_text`]).
pub(crate) fn text_ref(table: &str) -> Option<(&'static str, Option<&'static str>)> {
    match table {
        "dim" => Some(("refs", None)),
        "constraint_rule" => Some(("walls", Some("kind"))),
        "sweep" => Some(("hosts", Some("host_kind"))),
        _ => None,
    }
}

pub(crate) fn map_lookup(db: &Connection) -> Result<Statement<'_>, Error> {
    Ok(db.prepare("SELECT x.mine FROM temp.mg_map x WHERE x.tbl=?1 AND x.their=?2")?)
}

pub(crate) fn lookup(st: &mut Statement, tbl: &str, id: i64) -> Result<i64, Error> {
    Ok(st
        .query_row(params![tbl, id], |r| r.get(0))
        .optional()?
        .unwrap_or(id))
}

fn row_sql(t: &Table, schema: &str, translate: bool) -> String {
    let cols = t
        .cols
        .iter()
        .map(|c| {
            if translate {
                canon(&t.refs, "r", c)
            } else {
                format!("r.{}", q(c))
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let cols = if cols.is_empty() {
        "r.id".to_string()
    } else {
        cols
    };
    format!("SELECT {cols} FROM {schema}.{} r WHERE r.id=?1", q(&t.name))
}

fn kid_sql(c: &ChildTable, schema: &str, translate: bool) -> String {
    let cols = c
        .cols
        .iter()
        .map(|x| {
            if translate {
                canon(&c.refs, "r", x)
            } else {
                format!("r.{}", q(x))
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let filter = match c.spec.owner {
        tables::Owner::Col(..) => "?2=?2".to_string(),
        tables::Owner::Poly(k, _) => format!("r.{}=?2", q(k)),
    };
    format!(
        "SELECT {cols} FROM {schema}.{} r WHERE r.{}=?1 AND {filter}",
        q(c.spec.table),
        q(c.owner_col())
    )
}

fn read_row(st: &mut Statement, id: i64, n: usize) -> Result<Option<Vec<J>>, Error> {
    Ok(st
        .query_row([id], |r| {
            (0..n)
                .map(|i| r.get::<_, SqlValue>(i).map(|v| enc(&v)))
                .collect::<Result<Vec<_>, _>>()
        })
        .optional()?)
}

/// Part group by field key: whole (`""`) or by the value of the key column.
fn read_kids(
    st: &mut Statement,
    c: &ChildTable,
    id: i64,
    parent: &str,
) -> Result<BTreeMap<String, J>, Error> {
    let n = c.cols.len();
    let key_at = c.spec.key.and_then(|k| c.cols.iter().position(|x| x == k));
    let rows = st
        .query_map(params![id, parent], |r| {
            (0..n)
                .map(|i| r.get::<_, SqlValue>(i).map(|v| enc(&v)))
                .collect::<Result<Vec<_>, _>>()
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = BTreeMap::new();
    match key_at {
        Some(k) => {
            for row in rows {
                out.insert(show(&row[k]), J::Array(row));
            }
        }
        None if !rows.is_empty() => {
            let mut text: Vec<String> = rows
                .iter()
                .map(|r| J::Array(r.clone()).to_string())
                .collect();
            text.sort();
            out.insert(
                String::new(),
                J::Array(text.into_iter().map(J::String).collect()),
            );
        }
        None => {}
    }
    Ok(out)
}

fn field_name(c: &ChildTable, key: &str) -> String {
    if key.is_empty() {
        format!("#{}", c.spec.table)
    } else {
        format!("#{}:{key}", c.spec.table)
    }
}

/// `source`: 0 is base, 1 is mine, 2 is the peer; (schema, whether to translate ids).
const SIDES: [(&str, bool); 3] = [("base", true), ("main", false), ("their", true)];

/// Elements changed on both sides: field by field.
fn by_fields(db: &Connection, s: &Schema, plan: &mut MergePlan) -> Result<Vec<Take>, Error> {
    let rows: Vec<(String, i64, i64, i64)> = db
        .prepare("SELECT tbl, b, mine, nb FROM temp.mg_ch WHERE act=3 ORDER BY tbl, b")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    let mut takes = vec![];
    if rows.is_empty() {
        return Ok(takes);
    }
    let mut mapst = map_lookup(db)?;
    let mut row_st: HashMap<(usize, usize), Statement> = HashMap::new();
    let mut kid_st: HashMap<(usize, usize), Statement> = HashMap::new();
    for (tn, b, mine, nb) in rows {
        let Some(ti) = s.index(&tn) else { continue };
        let t = &s.tables[ti];
        let width = t.cols.len().max(1);
        let mut read = |side: usize, id: i64| -> Result<Option<Vec<J>>, Error> {
            if !row_st.contains_key(&(ti, side)) {
                let (name, tr) = SIDES[side];
                row_st.insert((ti, side), db.prepare(&row_sql(t, name, tr))?);
            }
            match row_st.get_mut(&(ti, side)) {
                Some(st) => read_row(st, id, width),
                None => Ok(None),
            }
        };
        let mut base_row = if nb == 1 { None } else { read(0, b)? };
        let (Some(mine_row), Some(mut their_row)) = (read(1, mine)?, read(2, b)?) else {
            continue;
        };
        if let Some((col, kind_col)) = text_ref(&t.name) {
            if let Some(at) = t.cols.iter().position(|c| c == col) {
                let kind_at = kind_col.and_then(|k| t.cols.iter().position(|c| c == k));
                for row in [Some(&mut their_row), base_row.as_mut()]
                    .into_iter()
                    .flatten()
                {
                    if let J::String(text) = &row[at] {
                        let kind = kind_at.and_then(|k| row[k].as_str().map(str::to_string));
                        let fixed =
                            canon_text(&t.name, col, text, kind.as_deref(), &mut |tb, i| {
                                lookup(&mut mapst, tb, i)
                            })?;
                        row[at] = J::String(fixed);
                    }
                }
            }
        }
        let mut take = Take {
            table: ti,
            b,
            mine,
            cols: vec![],
            kids: vec![],
            conflict_cols: vec![],
            conflict_kids: vec![],
        };
        let mut fields = vec![];
        for (i, col) in t.cols.iter().enumerate() {
            let basev = base_row.as_ref().map(|r| &r[i]);
            match verdict(basev, &mine_row[i], &their_row[i]) {
                Decision::Take => take.cols.push(col.clone()),
                Decision::Conflict => {
                    take.conflict_cols.push(col.clone());
                    fields.push(FieldConflict {
                        field: col.clone(),
                        base: basev.cloned().unwrap_or(J::Null),
                        mine: mine_row[i].clone(),
                        theirs: their_row[i].clone(),
                    });
                }
                _ => {}
            }
        }
        for (ci, c) in s.kids_of(&t.name) {
            let mut kids = |side: usize, id: i64| -> Result<BTreeMap<String, J>, Error> {
                if !kid_st.contains_key(&(ci, side)) {
                    let (name, tr) = SIDES[side];
                    kid_st.insert((ci, side), db.prepare(&kid_sql(c, name, tr))?);
                }
                match kid_st.get_mut(&(ci, side)) {
                    Some(st) => read_kids(st, c, id, &t.name),
                    None => Ok(BTreeMap::new()),
                }
            };
            let kb = if nb == 1 { None } else { Some(kids(0, b)?) };
            let km = kids(1, mine)?;
            let kt = kids(2, b)?;
            let keys: BTreeSet<&String> = km
                .keys()
                .chain(kt.keys())
                .chain(kb.iter().flat_map(|x| x.keys()))
                .collect();
            let null = J::Null;
            for key in keys {
                let base_k = kb.as_ref().map(|x| x.get(key).unwrap_or(&null));
                let (mv, tv) = (km.get(key).unwrap_or(&null), kt.get(key).unwrap_or(&null));
                match verdict(base_k, mv, tv) {
                    Decision::Take => take.kids.push((ci, key.clone())),
                    Decision::Conflict => {
                        take.conflict_kids.push((ci, key.clone()));
                        fields.push(FieldConflict {
                            field: field_name(c, key),
                            base: base_k.cloned().unwrap_or(J::Null),
                            mine: mv.clone(),
                            theirs: tv.clone(),
                        });
                    }
                    _ => {}
                }
            }
        }
        if !fields.is_empty() {
            plan.conflicts.push(MergeConflict {
                table: t.name.clone(),
                id: mine,
                case: if nb == 1 {
                    MergeCase::AddAdd
                } else {
                    MergeCase::EditEdit
                },
                fields,
            });
        }
        if !take.cols.is_empty() || !take.kids.is_empty() {
            plan.changed += 1;
            count(plan, &t.name).changed += 1;
        }
        takes.push(take);
    }
    Ok(takes)
}

/// Like [`decide`], but without a base any difference is a conflict: the peer is not "newer" by definition.
fn verdict(base: Option<&J>, mine: &J, theirs: &J) -> Decision {
    match base {
        None if same_value(mine, theirs) => Decision::Same,
        None => Decision::Conflict,
        Some(_) => decide(base, mine, theirs),
    }
}

fn count<'p>(plan: &'p mut MergePlan, table: &str) -> &'p mut MergeCounts {
    let at = plan
        .by_table
        .iter()
        .position(|c| c.table == table)
        .unwrap_or_else(|| {
            plan.by_table.push(MergeCounts {
                table: table.to_string(),
                ..Default::default()
            });
            plan.by_table.len() - 1
        });
    &mut plan.by_table[at]
}

fn sum_up(db: &Connection, plan: &mut MergePlan) -> Result<(), Error> {
    let added: Vec<(String, i64)> = db
        .prepare("SELECT tbl, count(*) FROM temp.mg_add GROUP BY tbl")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (t, n) in added {
        plan.added += n as usize;
        count(plan, &t).added += n as usize;
    }
    let acts: Vec<(String, i64, i64)> = db
        .prepare("SELECT tbl, act, count(*) FROM temp.mg_ch WHERE act IN (1,2) GROUP BY tbl, act")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    for (t, act, n) in acts {
        if act == TAKE {
            plan.changed += n as usize;
            count(plan, &t).changed += n as usize;
        } else {
            plan.deleted += n as usize;
            count(plan, &t).deleted += n as usize;
        }
    }
    let gone: Vec<(String, i64, i64)> = db
        .prepare("SELECT tbl, mine, act FROM temp.mg_ch WHERE act IN (4,5) ORDER BY tbl, mine")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    for (t, id, act) in gone {
        plan.conflicts.push(MergeConflict {
            table: t,
            id,
            case: if act == DEL_THERE {
                MergeCase::DeletedThere
            } else {
                MergeCase::DeletedHere
            },
            fields: vec![],
        });
    }
    for i in 0..plan.conflicts.len() {
        let t = plan.conflicts[i].table.clone();
        count(plan, &t).conflicts += 1;
    }
    plan.by_table.sort_by(|a, b| a.table.cmp(&b.table));
    plan.nothing =
        plan.added == 0 && plan.changed == 0 && plan.deleted == 0 && plan.conflicts.is_empty();
    Ok(())
}

pub(crate) fn analyze(db: &Connection, p: &Prepared) -> Result<Analysis, Error> {
    db.execute_batch(TEMP_DDL)?;
    let schema = tables::load(db, &["their", "base"])?;
    // without an earlier merge base ids are shared; otherwise `mg_map` translates them
    let identity = p.peer.is_none();
    load_maps(db, p)?;
    for t in &schema.tables {
        map_table(db, t)?;
    }
    for t in &schema.tables {
        diff_table(db, t, identity)?;
    }
    diff_children(db, &schema, identity)?;
    escalate(db, &schema)?;
    let mut plan = MergePlan {
        head: super::head(db)?,
        ..Default::default()
    };
    let takes = by_fields(db, &schema, &mut plan)?;
    sum_up(db, &mut plan)?;
    Ok(Analysis {
        schema,
        takes,
        plan,
    })
}
