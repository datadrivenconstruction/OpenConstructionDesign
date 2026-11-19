//! Keyed version diff: elements of two versions are paired by element key, so renumbered rows
//! of a regenerated or re-imported model are the same elements. Key precedence is that of the
//! revision link: generator key, script part key, imported guid, minted key. Rows left over
//! pair by table and id unless both carry different keys (a reused id is another element).
//! Values compare in this file's id space (a reference compares the element it points to) and
//! numbers within a relative 1e-9; each change keeps its old and new value.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::types::Value;
use rusqlite::Connection;

use crate::keys::{load_keys, KEY_PROP, MINTED_PROP};
use crate::merge::tables::{q, Owner, RefCol, Schema, Table};
use crate::Error;

const REL_TOL: f64 = 1e-9;
const TEXT_SHOWN: usize = 200;
/// A reference to an element only the other version has (never equal to a value here).
const THERE_MARK: &str = "\u{1}there:";
/// Parts that are parameters of the element rather than its pieces.
const PARAM_PARTS: [&str; 3] = ["prop", "pparam_val", "param"];

/// What kind of value changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldGroup {
    /// A plain column of the element row.
    Column,
    /// A column that points at another element (level, host, type).
    Relation,
    /// A property or parameter (`prop.<name>`).
    Param,
    /// A part row group such as outline vertices (`#table`, row counts).
    Part,
}

/// One changed value; `None` is "no value" (absent or NULL).
#[derive(Clone, Debug, PartialEq)]
pub struct FieldChange {
    pub field: String,
    pub group: FieldGroup,
    pub old: Option<String>,
    pub new: Option<String>,
}

type Rows = BTreeMap<i64, Vec<Value>>;

/// One version: element keys and the rows of every compared table.
pub(crate) struct Side {
    pub schema: &'static str,
    keys: HashMap<(String, i64), String>,
    rows: HashMap<String, Rows>,
}

/// Both versions with their pairing (`here` id -> `there` id per table).
pub(crate) struct Paired {
    pub here: Side,
    pub there: Side,
    pairs: HashMap<String, BTreeMap<i64, i64>>,
    back: HashMap<String, HashMap<i64, i64>>,
}

fn load_rows(db: &Connection, schema: &str, t: &Table) -> Result<Rows, Error> {
    let cols: Vec<String> = t.cols.iter().map(|c| q(c)).collect();
    let list = if cols.is_empty() {
        String::new()
    } else {
        format!(", {}", cols.join(", "))
    };
    let mut st = db.prepare(&format!("SELECT id{list} FROM {schema}.{}", q(&t.name)))?;
    let n = t.cols.len();
    let mut out = Rows::new();
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let v = (0..n).map(|i| r.get::<_, Value>(i + 1)).collect::<Result<Vec<_>, _>>()?;
        out.insert(id, v);
    }
    Ok(out)
}

fn side(db: &Connection, s: &Schema, schema: &'static str) -> Result<Side, Error> {
    let mut rows = HashMap::new();
    for t in &s.tables {
        rows.insert(t.name.clone(), load_rows(db, schema, t)?);
    }
    Ok(Side {
        schema,
        keys: load_keys(db, schema)?,
        rows,
    })
}

impl Side {
    pub fn key(&self, table: &str, id: i64) -> Option<&str> {
        self.keys.get(&(table.to_string(), id)).map(String::as_str)
    }

    pub fn has(&self, table: &str, id: i64) -> bool {
        self.rows.get(table).is_some_and(|r| r.contains_key(&id))
    }

    pub fn ids(&self, table: &str) -> impl Iterator<Item = i64> + '_ {
        self.rows.get(table).into_iter().flat_map(|r| r.keys().copied())
    }

    /// How a reference to `table`/`id` reads: the element's name, else its key, else `#id`.
    fn label(&self, s: &Schema, table: &str, id: i64) -> String {
        let name = s
            .table(table)
            .and_then(|t| t.cols.iter().position(|c| c == "name"))
            .and_then(|i| self.rows.get(table)?.get(&id)?.get(i))
            .and_then(show)
            .filter(|n| !n.trim().is_empty());
        name.or_else(|| self.key(table, id).map(str::to_string))
            .unwrap_or_else(|| format!("#{id}"))
    }
}

/// Pairs the elements of the open file (`main`) and the other version (`their`).
pub(crate) fn pair(db: &Connection, s: &Schema) -> Result<Paired, Error> {
    let here = side(db, s, "main")?;
    let there = side(db, s, "their")?;
    let mut pairs: HashMap<String, BTreeMap<i64, i64>> = HashMap::new();
    let mut back: HashMap<String, HashMap<i64, i64>> = HashMap::new();
    for t in &s.tables {
        let name = t.name.as_str();
        let mut by_key: HashMap<&str, i64> = HashMap::new();
        for id in there.ids(name) {
            if let Some(k) = there.key(name, id) {
                by_key.entry(k).or_insert(id);
            }
        }
        let mut p = BTreeMap::new();
        let mut taken: BTreeSet<i64> = BTreeSet::new();
        for id in here.ids(name) {
            if let Some(&o) = here.key(name, id).and_then(|k| by_key.get(k)) {
                if taken.insert(o) {
                    p.insert(id, o);
                }
            }
        }
        for id in here.ids(name) {
            if p.contains_key(&id) || taken.contains(&id) || !there.has(name, id) {
                continue;
            }
            let both_keyed = here.key(name, id).is_some() && there.key(name, id).is_some();
            if !both_keyed {
                p.insert(id, id);
                taken.insert(id);
            }
        }
        back.insert(name.to_string(), p.iter().map(|(a, b)| (*b, *a)).collect());
        pairs.insert(name.to_string(), p);
    }
    Ok(Paired {
        here,
        there,
        pairs,
        back,
    })
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Real(r) => Some(*r),
        _ => None,
    }
}

/// Equal values; numbers within a relative 1e-9 (an integer equals the same real).
pub(crate) fn same(a: &Value, b: &Value) -> bool {
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x == y || (x - y).abs() <= REL_TOL * x.abs().max(y.abs()).max(1.0),
        _ => a == b,
    }
}

/// A value as text for the change table; `None` for NULL.
pub(crate) fn show(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::Integer(i) => Some(i.to_string()),
        Value::Real(r) => {
            let s = format!("{r:.6}");
            let s = s.trim_end_matches('0').trim_end_matches('.');
            Some(if s == "-0" { "0".into() } else { s.to_string() })
        }
        Value::Text(t) => Some(t.chars().take(TEXT_SHOWN).collect()),
        Value::Blob(b) => Some(format!("<{} B>", b.len())),
    }
}

/// Reference columns of a row: (column index, parent table or `None` when the kind column at
/// the second index names it).
fn ref_cols(cols: &[String], refs: &[RefCol]) -> Vec<(usize, Option<String>, Option<usize>)> {
    let at = |c: &str| cols.iter().position(|x| x == c);
    refs.iter()
        .filter_map(|r| match r {
            RefCol::To { col, parent } => Some((at(col)?, Some(parent.clone()), None)),
            RefCol::Poly { kind, col } => Some((at(col)?, None, Some(at(kind)?))),
        })
        .collect()
}

impl Paired {
    /// Here id of a paired element of the other version.
    pub fn here_of(&self, table: &str, there: i64) -> Option<i64> {
        self.back.get(table)?.get(&there).copied()
    }

    pub fn there_of(&self, table: &str, here: i64) -> Option<i64> {
        self.pairs.get(table)?.get(&here).copied()
    }

    pub fn pairs_of(&self, table: &str) -> impl Iterator<Item = (i64, i64)> + '_ {
        self.pairs.get(table).into_iter().flat_map(|m| m.iter().map(|(a, b)| (*a, *b)))
    }

    /// The element key on either side.
    pub fn key_of(&self, table: &str, here: Option<i64>, there: Option<i64>) -> Option<String> {
        here.and_then(|i| self.here.key(table, i))
            .or_else(|| there.and_then(|i| self.there.key(table, i)))
            .map(str::to_string)
    }

    /// A reference value of the other version in this file's id space; a reference to an
    /// element only the other version has can never equal one here.
    fn canon_there(&self, v: &Value, parent: &str) -> Value {
        match v {
            Value::Integer(i) if self.there.has(parent, *i) => match self.here_of(parent, *i) {
                Some(h) => Value::Integer(h),
                None => Value::Text(format!("{THERE_MARK}{parent}:{i}")),
            },
            o => o.clone(),
        }
    }

    fn ref_text(&self, s: &Schema, here_side: bool, v: &Value, parent: &str) -> Option<String> {
        match v {
            Value::Integer(i) => {
                let sd = if here_side { &self.here } else { &self.there };
                if sd.has(parent, *i) {
                    Some(sd.label(s, parent, *i))
                } else {
                    show(v)
                }
            }
            Value::Text(t) if t.starts_with(THERE_MARK) => {
                let id = t.rsplit(':').next().and_then(|x| x.parse().ok())?;
                Some(self.there.label(s, parent, id))
            }
            o => show(o),
        }
    }

    /// Parent table of reference `r` in `row`.
    fn parent_of(r: &(usize, Option<String>, Option<usize>), row: &[Value]) -> Option<String> {
        match r {
            (_, Some(p), _) => Some(p.clone()),
            (_, None, Some(k)) => match row.get(*k) {
                Some(Value::Text(t)) => Some(t.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Changed columns of a paired element.
    pub fn column_changes(&self, s: &Schema, t: &Table, here: i64, there: i64) -> Vec<FieldChange> {
        let (Some(a), Some(b)) = (
            self.here.rows.get(&t.name).and_then(|r| r.get(&here)),
            self.there.rows.get(&t.name).and_then(|r| r.get(&there)),
        ) else {
            return vec![];
        };
        let refs = ref_cols(&t.cols, &t.refs);
        let mut out = vec![];
        for (i, col) in t.cols.iter().enumerate() {
            let r = refs.iter().find(|r| r.0 == i);
            let parent = r.and_then(|r| Self::parent_of(r, b));
            let bv = match &parent {
                Some(p) => self.canon_there(&b[i], p),
                None => b[i].clone(),
            };
            if same(&a[i], &bv) {
                continue;
            }
            let (old, new, group) = match (&parent, r.and_then(|r| Self::parent_of(r, a))) {
                (Some(pb), Some(pa)) => (
                    self.ref_text(s, false, &b[i], pb),
                    self.ref_text(s, true, &a[i], &pa),
                    FieldGroup::Relation,
                ),
                _ => (show(&b[i]), show(&a[i]), FieldGroup::Column),
            };
            out.push(FieldChange {
                field: col.clone(),
                group,
                old,
                new,
            });
        }
        out
    }

    /// Changed part rows of every paired element, by (table, here id).
    pub fn part_changes(
        &self,
        db: &Connection,
        s: &Schema,
    ) -> Result<HashMap<(String, i64), Vec<FieldChange>>, Error> {
        let mut out: HashMap<(String, i64), Vec<FieldChange>> = HashMap::new();
        for c in &s.children {
            let name = c.spec.table;
            let at = |n: &str| c.cols.iter().position(|x| x == n);
            let (Some(oi), kind_i, fixed) = (match c.spec.owner {
                Owner::Col(o, p) => (at(o), None, Some(p)),
                Owner::Poly(k, o) => (at(o), at(k), None),
            }) else {
                continue;
            };
            let key_i = c.spec.key.and_then(at);
            let refs = ref_cols(&c.cols, &c.refs);
            let group = if PARAM_PARTS.contains(&name) {
                FieldGroup::Param
            } else {
                FieldGroup::Part
            };
            type Groups = HashMap<(String, i64), Vec<Vec<Value>>>;
            let load = |schema: &str, there: bool| -> Result<Groups, Error> {
                let cols: Vec<String> = c.cols.iter().map(|x| q(x)).collect();
                let mut st =
                    db.prepare(&format!("SELECT {} FROM {schema}.{}", cols.join(", "), q(name)))?;
                let mut g: Groups = HashMap::new();
                let mut rows = st.query([])?;
                while let Some(r) = rows.next()? {
                    let row = (0..c.cols.len())
                        .map(|i| r.get::<_, Value>(i))
                        .collect::<Result<Vec<_>, _>>()?;
                    let owner_t = match (fixed, kind_i.and_then(|k| row.get(k))) {
                        (Some(p), _) => p.to_string(),
                        (None, Some(Value::Text(t))) => t.clone(),
                        _ => continue,
                    };
                    let Value::Integer(oid) = row[oi] else {
                        continue;
                    };
                    let here_id = if there {
                        self.here_of(&owner_t, oid)
                    } else {
                        self.there_of(&owner_t, oid).map(|_| oid)
                    };
                    let Some(hid) = here_id else {
                        continue;
                    };
                    let mut row = row;
                    if there {
                        for r in &refs {
                            if let Some(p) = Self::parent_of(r, &row) {
                                row[r.0] = self.canon_there(&row[r.0], &p);
                            }
                        }
                    }
                    g.entry((owner_t, hid)).or_default().push(row);
                }
                Ok(g)
            };
            let here = load("main", false)?;
            let there = load("their", true)?;
            let skip = |i: usize| i == oi || Some(i) == kind_i;
            let owners: BTreeSet<&(String, i64)> = here.keys().chain(there.keys()).collect();
            for owner in owners {
                let a = here.get(owner).map(Vec::as_slice).unwrap_or(&[]);
                let b = there.get(owner).map(Vec::as_slice).unwrap_or(&[]);
                let changes = match key_i {
                    Some(ki) => self.keyed_rows(s, name, ki, &refs, group, a, b, &skip),
                    None => unkeyed_rows(name, group, a, b, &skip),
                };
                if !changes.is_empty() {
                    out.entry(owner.clone()).or_default().extend(changes);
                }
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn keyed_rows(
        &self,
        s: &Schema,
        name: &str,
        ki: usize,
        refs: &[(usize, Option<String>, Option<usize>)],
        group: FieldGroup,
        a: &[Vec<Value>],
        b: &[Vec<Value>],
        skip: &dyn Fn(usize) -> bool,
    ) -> Vec<FieldChange> {
        let key_text = |row: &[Value]| -> String {
            let parent = refs.iter().find(|r| r.0 == ki).and_then(|r| Self::parent_of(r, row));
            match parent {
                Some(p) => self.ref_text(s, true, &row[ki], &p).unwrap_or_default(),
                None => show(&row[ki]).unwrap_or_default(),
            }
        };
        let by_key = |rows: &[Vec<Value>]| -> BTreeMap<String, Vec<Value>> {
            rows.iter().map(|r| (canon_key(&r[ki]), r.clone())).collect()
        };
        let (ma, mb) = (by_key(a), by_key(b));
        let mut out = vec![];
        let keys: BTreeSet<&String> = ma.keys().chain(mb.keys()).collect();
        for k in keys {
            let (ra, rb) = (ma.get(k), mb.get(k));
            let row = ra.or(rb).expect("key from one side");
            let label = key_text(row);
            if name == "prop" && (label == KEY_PROP || label == MINTED_PROP) {
                continue;
            }
            let rest = |r: &Vec<Value>| -> Vec<Value> {
                r.iter()
                    .enumerate()
                    .filter(|(i, _)| *i != ki && !skip(*i))
                    .map(|(_, v)| v.clone())
                    .collect()
            };
            if let (Some(x), Some(y)) = (ra, rb) {
                let (x, y) = (rest(x), rest(y));
                if x.len() == y.len() && x.iter().zip(&y).all(|(p, q)| same(p, q)) {
                    continue;
                }
            }
            out.push(FieldChange {
                field: format!("{name}.{label}"),
                group,
                old: rb.map(|r| row_text(&rest(r))),
                new: ra.map(|r| row_text(&rest(r))),
            });
        }
        out
    }
}

fn canon_key(v: &Value) -> String {
    match v {
        Value::Real(r) if r.fract() == 0.0 && r.abs() < 9e15 => format!("i{}", *r as i64),
        Value::Integer(i) => format!("i{i}"),
        o => format!("{o:?}"),
    }
}

fn row_text(v: &[Value]) -> String {
    v.iter().filter_map(show).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Unkeyed part rows compare as a whole group: same rows in any order, or a change that
/// gives the row counts.
fn unkeyed_rows(
    name: &str,
    group: FieldGroup,
    a: &[Vec<Value>],
    b: &[Vec<Value>],
    skip: &dyn Fn(usize) -> bool,
) -> Vec<FieldChange> {
    let strip = |rows: &[Vec<Value>]| -> Vec<Vec<Value>> {
        let mut v: Vec<Vec<Value>> = rows
            .iter()
            .map(|r| r.iter().enumerate().filter(|(i, _)| !skip(*i)).map(|(_, x)| x.clone()).collect())
            .collect();
        v.sort_by(|x, y| format!("{x:?}").cmp(&format!("{y:?}")));
        v
    };
    let (x, y) = (strip(a), strip(b));
    let equal = x.len() == y.len()
        && x.iter().zip(&y).all(|(p, q)| p.len() == q.len() && p.iter().zip(q).all(|(u, w)| same(u, w)));
    if equal {
        return vec![];
    }
    vec![FieldChange {
        field: format!("#{name}"),
        group,
        old: Some(b.len().to_string()),
        new: Some(a.len().to_string()),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DiffKind, ElementDiff, LayerSpec, Model, VersionDiff, WallSpec};
    use std::path::PathBuf;

    struct Tmp(PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static N: AtomicU64 = AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "ddc-keyed-diff-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// One wall of a synthetic revision: key, start x, length, fire rating.
    struct W(&'static str, f64, f64, &'static str);

    /// A revision built from scratch: `extra_level` first shifts every level id; walls are
    /// created in the given order, so their ids follow it.
    fn revision(path: &std::path::Path, extra_level: bool, walls: &[W]) {
        let mut m = Model::open(path.to_str().unwrap()).unwrap();
        if extra_level {
            let l0 = m.add_level("Basement", -3.0).unwrap();
            m.set_prop("level", l0, KEY_PROP, Some((Value::Text("lvl-b".into()), ""))).unwrap();
        }
        let level = m.add_level("L1", 0.0).unwrap();
        m.set_prop("level", level, KEY_PROP, Some((Value::Text("lvl-1".into()), ""))).unwrap();
        let layer = vec![LayerSpec {
            material: "brick".into(),
            thickness: 0.2,
        }];
        let t = m.add_wall_type("std", &layer).unwrap();
        for (i, w) in walls.iter().enumerate() {
            let y = 10.0 * i as f64;
            let id = m
                .add_wall(&WallSpec {
                    a: (w.1, y),
                    b: (w.1 + w.2, y),
                    level,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: t,
                    offset0: None,
                })
                .unwrap();
            m.set_prop("wall", id, KEY_PROP, Some((Value::Text(w.0.into()), ""))).unwrap();
            m.set_prop("wall", id, "Fire", Some((Value::Text(w.3.into()), ""))).unwrap();
        }
    }

    fn diff(old: &[W], new: &[W], renumber_levels: bool) -> (VersionDiff, Tmp) {
        let d = Tmp::new("rev");
        revision(&d.0.join("old.ddc"), false, old);
        revision(&d.0.join("new.ddc"), renumber_levels, new);
        let copy = crate::prepare_compare(&d.0.join("old.ddc"), &d.0.join("cmp")).unwrap();
        let m = Model::open(d.0.join("new.ddc").to_str().unwrap()).unwrap();
        (m.compare_version(&copy).unwrap(), d)
    }

    fn by_key<'v>(v: &'v VersionDiff, key: &str) -> Option<&'v ElementDiff> {
        v.elements.iter().find(|e| e.key.as_deref() == Some(key))
    }

    fn change<'e>(e: &'e ElementDiff, field: &str) -> &'e FieldChange {
        e.changes.iter().find(|c| c.field == field).unwrap_or_else(|| panic!("{field}: {e:?}"))
    }

    #[test]
    fn renumbered_revisions_pair_by_key() {
        let old = [
            W("k1", 0.0, 5.0, "EI60"),
            W("k2", 0.0, 5.0, "EI60"),
            W("k3", 0.0, 5.0, "EI60"),
            W("k4", 0.0, 5.0, "EI60"),
        ];
        // new order renumbers every wall; k5 takes the id k4 had, k1 the id of removed k3
        let new = [
            W("k4", 1e-12, 5.0, "EI60"),
            W("k2", 0.0, 6.0, "EI60"),
            W("k1", 0.0, 5.0, "EI90"),
            W("k5", 0.0, 5.0, "EI30"),
        ];
        let (v, _d) = diff(&old, &new, true);
        let walls: Vec<_> = v.elements.iter().filter(|e| e.table == "wall").collect();
        assert_eq!(walls.len(), 4, "{walls:?}");
        assert!(by_key(&v, "k4").is_none(), "float noise and a new id are no change");

        let k2 = by_key(&v, "k2").unwrap();
        assert_eq!(k2.kind, DiffKind::Differs);
        let bx = change(k2, "bx");
        assert_eq!((bx.group, bx.old.as_deref(), bx.new.as_deref()), (FieldGroup::Column, Some("5"), Some("6")));
        assert_eq!(k2.fields, vec!["bx".to_string()], "the level moved id but is the same level");

        let k1 = by_key(&v, "k1").unwrap();
        let fire = change(k1, "prop.Fire");
        assert_eq!(fire.group, FieldGroup::Param);
        assert_eq!((fire.old.as_deref(), fire.new.as_deref()), (Some("EI60"), Some("EI90")));
        assert_eq!(k1.changes.len(), 1, "identity keys are not changes: {:?}", k1.changes);
        assert_ne!(k1.other, Some(k1.id), "paired across ids");

        let k3 = by_key(&v, "k3").unwrap();
        assert_eq!(k3.kind, DiffKind::ThereOnly);
        let k5 = by_key(&v, "k5").unwrap();
        assert_eq!(k5.kind, DiffKind::HereOnly);
        assert_eq!(k5.id, 4, "k5 reuses the id of old k4");

        let levels: Vec<_> = v.elements.iter().filter(|e| e.table == "level").collect();
        assert_eq!(levels.len(), 1, "{levels:?}");
        assert_eq!((levels[0].kind, levels[0].key.as_deref()), (DiffKind::HereOnly, Some("lvl-b")));
    }

    #[test]
    fn a_moved_reference_reads_as_the_other_element() {
        let d = Tmp::new("ref");
        let w = [W("k1", 0.0, 5.0, "EI60")];
        revision(&d.0.join("old.ddc"), false, &w);
        revision(&d.0.join("new.ddc"), true, &w);
        let m = Model::open(d.0.join("new.ddc").to_str().unwrap()).unwrap();
        let wall: i64 = m.db.query_row("SELECT id FROM wall", [], |r| r.get(0)).unwrap();
        let basement: i64 =
            m.db.query_row("SELECT id FROM level WHERE name='Basement'", [], |r| r.get(0)).unwrap();
        m.db.execute("UPDATE wall SET level=?1 WHERE id=?2", [basement, wall]).unwrap();
        let copy = crate::prepare_compare(&d.0.join("old.ddc"), &d.0.join("cmp")).unwrap();
        let v = m.compare_version(&copy).unwrap();
        let e = by_key(&v, "k1").unwrap();
        let lv = change(e, "level");
        assert_eq!(lv.group, FieldGroup::Relation);
        assert_eq!((lv.old.as_deref(), lv.new.as_deref()), (Some("L1"), Some("Basement")));
    }

    #[test]
    fn rows_without_keys_pair_by_id_and_a_regenerated_key_wins() {
        let d = Tmp::new("ids");
        let w = [W("k1", 0.0, 5.0, "EI60"), W("k2", 0.0, 5.0, "EI60")];
        revision(&d.0.join("old.ddc"), false, &w);
        revision(&d.0.join("new.ddc"), false, &w);
        let m = Model::open(d.0.join("new.ddc").to_str().unwrap()).unwrap();
        // a generator wrote new keys: the element key replaces the old one
        m.db.execute("UPDATE prop SET value='k2-new' WHERE value='k2'", []).unwrap();
        // an unkeyed wall type edited in place still pairs by id
        m.db.execute("UPDATE wall_type SET name='std-2'", []).unwrap();
        let copy = crate::prepare_compare(&d.0.join("old.ddc"), &d.0.join("cmp")).unwrap();
        let v = m.compare_version(&copy).unwrap();
        assert_eq!(by_key(&v, "k2-new").unwrap().kind, DiffKind::HereOnly);
        assert_eq!(by_key(&v, "k2").unwrap().kind, DiffKind::ThereOnly);
        assert!(by_key(&v, "k1").is_none());
        let t = v.elements.iter().find(|e| e.table == "wall_type").unwrap();
        assert_eq!(t.kind, DiffKind::Differs);
        let n = change(t, "name");
        assert_eq!((n.old.as_deref(), n.new.as_deref()), (Some("std"), Some("std-2")));
    }

    #[test]
    fn script_parts_pair_by_instance_and_part_key() {
        use crate::{Form, MassSpec, Recipe, RecipePart};
        let d = Tmp::new("parts");
        let mut m = Model::open(d.0.join("old.ddc").to_str().unwrap()).unwrap();
        let level = m.add_level("L1", 0.0).unwrap();
        let cube = |m: &mut Model, x: f64| {
            m.add_mass(&MassSpec {
                level,
                name: "box".into(),
                void: false,
                inplace: false,
                form: Form::Cuboid {
                    lo: (x, 0.0, 0.0),
                    hi: (x + 1.0, 1.0, 1.0),
                },
            })
            .unwrap()
        };
        let rec = |n: &str| Recipe {
            id: 0,
            name: n.into(),
            script: "box()".into(),
            params_json: "{}".into(),
            hash: "h".into(),
            version: 1,
        };
        let part = |el| {
            vec![RecipePart {
                key: "p0".into(),
                tbl: "mass".into(),
                el,
            }]
        };
        let (a, b) = (cube(&mut m, 0.0), cube(&mut m, 5.0));
        m.recipe_add(&rec("a"), &part(a)).unwrap();
        m.recipe_add(&rec("b"), &part(b)).unwrap();
        drop(m);
        // the new revision regenerated both scripts: the parts swapped row ids
        std::fs::copy(d.0.join("old.ddc"), d.0.join("new.ddc")).unwrap();
        let m = Model::open(d.0.join("new.ddc").to_str().unwrap()).unwrap();
        m.db.execute(
            "UPDATE recipe_part SET el=CASE el WHEN ?1 THEN ?2 WHEN ?2 THEN ?1 ELSE el END",
            [a, b],
        )
        .unwrap();
        let keys = load_keys(&m.db, "main").unwrap();
        let (ka, kb) = (&keys[&("mass".to_string(), a)], &keys[&("mass".to_string(), b)]);
        assert_ne!(ka, kb, "one part key in two scripts is two elements");
        let copy = crate::prepare_compare(&d.0.join("old.ddc"), &d.0.join("cmp")).unwrap();
        let v = m.compare_version(&copy).unwrap();
        let masses: Vec<_> = v.elements.iter().filter(|e| e.table == "mass").collect();
        assert_eq!(masses.len(), 2, "{masses:?}");
        assert!(masses.iter().all(|e| e.kind == DiffKind::Differs && e.other != Some(e.id)));
    }

    #[test]
    fn script_part_and_imported_keys_rank_below_the_generator_key() {
        let d = Tmp::new("rank");
        let w = [W("k1", 0.0, 5.0, "EI60")];
        revision(&d.0.join("old.ddc"), false, &w);
        let db = Connection::open(d.0.join("old.ddc")).unwrap();
        db.execute(
            "INSERT INTO ext_ref(source, kind, ref, guid) SELECT 'f.ifc', 'wall', id, 'g-1' FROM wall",
            [],
        )
        .unwrap();
        let k = load_keys(&db, "main").unwrap();
        let wall: i64 = db.query_row("SELECT id FROM wall", [], |r| r.get(0)).unwrap();
        assert_eq!(k.get(&("wall".to_string(), wall)).map(String::as_str), Some("k1"));
        db.execute("DELETE FROM prop WHERE key=?1", [KEY_PROP]).unwrap();
        let k = load_keys(&db, "main").unwrap();
        assert_eq!(k.get(&("wall".to_string(), wall)).map(String::as_str), Some("g-1"));
    }

    #[test]
    fn numbers_compare_with_a_relative_tolerance() {
        assert!(same(&Value::Real(1.0), &Value::Integer(1)));
        assert!(same(&Value::Real(1000.0), &Value::Real(1000.0 + 1e-7)));
        assert!(!same(&Value::Real(1.0), &Value::Real(1.001)));
        assert!(same(&Value::Null, &Value::Null));
        assert!(!same(&Value::Null, &Value::Integer(0)));
        assert!(!same(&Value::Text("1".into()), &Value::Integer(1)));
    }

    #[test]
    fn values_read_as_text() {
        assert_eq!(show(&Value::Real(2.5)), Some("2.5".into()));
        assert_eq!(show(&Value::Real(3.0)), Some("3".into()));
        assert_eq!(show(&Value::Real(-1e-9)), Some("0".into()));
        assert_eq!(show(&Value::Null), None);
        assert_eq!(show(&Value::Blob(vec![1, 2])), Some("<2 B>".into()));
    }
}
