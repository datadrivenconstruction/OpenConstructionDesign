// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! What a plan item read and wrote: the ground for checking only the touched items when the
//! model changes under a plan, instead of calling the whole plan stale.
//!
//! Reads are noted at explicit points (target resolution, filters, frozen sets, read tools), not
//! through the database authorizer: it does not give row ids. A read by predicate (a filter, "all
//! walls of a level") is a table read: any insert or edit in that table touches it.

use super::sets::{table_of, Key};
use crate::query::Expr;
use ddc_model::{Changes, ElKey};
use rusqlite::Connection;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

/// A position in the live model's change journal (`Model::change_head`).
pub type ChangeToken = u64;

/// Rows and tables an item depends on, or rows it writes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadSet {
    /// Point reads: targets by id, hosts, types, levels.
    pub rows: HashMap<&'static str, BTreeSet<i64>>,
    /// Reads by predicate: any change in the table touches them.
    pub tables: BTreeSet<&'static str>,
    /// Project settings (units, phase) were read.
    pub meta: bool,
}

/// What one plan item depends on and changes, and the live journal position it was built at.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemSets {
    pub reads: ReadSet,
    /// The rows the item's steps changed on the copy, less the elements it created: those are
    /// made again, under new ids, by every replay.
    pub writes: ReadSet,
    pub base: ChangeToken,
    /// What the item left in the rows it writes (`conflict::written_fp`); 0: not taken yet.
    pub fp: u64,
}

impl ReadSet {
    pub fn row(&mut self, table: &'static str, id: i64) {
        if !self.tables.contains(table) {
            self.rows.entry(table).or_default().insert(id);
        }
    }

    pub fn table(&mut self, table: &'static str) {
        if self.tables.insert(table) {
            self.rows.remove(table);
        }
    }

    pub fn el(&mut self, k: &ElKey) {
        self.row(intern(&k.0), k.1);
    }

    /// Number of point reads.
    pub fn row_count(&self) -> usize {
        self.rows.values().map(BTreeSet::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        !self.meta && self.tables.is_empty() && self.rows.is_empty()
    }

    /// The row is in the set, by itself or through its whole table.
    pub fn covers(&self, table: &str, id: i64) -> bool {
        self.tables.contains(table) || self.rows.get(table).is_some_and(|r| r.contains(&id))
    }

    pub fn merge(&mut self, o: ReadSet) {
        self.meta |= o.meta;
        for t in o.tables {
            self.table(t);
        }
        for (t, ids) in o.rows {
            if self.tables.contains(t) {
                continue;
            }
            self.rows.entry(t).or_default().extend(ids);
        }
    }

    /// The rows of a change: a table the journal coalesced (or a lost window) counts whole.
    pub fn from_changes(ch: &Changes) -> ReadSet {
        Self::from_changes_except(ch, &HashSet::new())
    }

    /// [`Self::from_changes`] without the rows in `made`.
    pub fn from_changes_except(ch: &Changes, made: &HashSet<ElKey>) -> ReadSet {
        let mut w = ReadSet::default();
        if ch.all {
            w.meta = true;
            return w;
        }
        // derived rows are rebuilt with fresh ids on every replay; their owner is written already
        for t in ch.rows.keys().filter(|t| model_table(t)) {
            let t = intern(t);
            if ch.whole(t) {
                w.table(t);
            } else {
                let ids = ch.ids(t).filter(|id| made.is_empty() || !made.contains(&(t.to_string(), *id)));
                w.rows.entry(t).or_default().extend(ids);
            }
        }
        w.rows.retain(|_, ids| !ids.is_empty());
        w
    }

    /// Elements read by id, with what they stand on: level, type, host.
    pub fn keys(&mut self, conn: &Connection, keys: &[Key]) {
        let mut by: HashMap<&'static str, Vec<i64>> = HashMap::new();
        for k in keys {
            let t = k.table();
            if t.is_empty() {
                // a category without its own table: every element table, to be safe
                ELEMENT_TABLES.iter().for_each(|t| self.table(*t));
                continue;
            }
            self.row(t, k.id);
            by.entry(t).or_default().push(k.id);
        }
        self.deps(conn, by, 2);
    }

    /// A filter: the tables of its categories as predicate reads, then the hits by id.
    pub fn filter(&mut self, conn: &Connection, e: &Expr, hits: &[Key]) {
        match cats_of(e) {
            Some(cats) => {
                for c in cats {
                    match table_of(c) {
                        "" => ELEMENT_TABLES.iter().for_each(|t| self.table(*t)),
                        t => self.table(t),
                    }
                }
            }
            None => ELEMENT_TABLES.iter().for_each(|t| self.table(*t)),
        }
        let mut fields = vec![];
        compared(e, &mut fields);
        for f in fields {
            match f.as_str() {
                "level" => self.table("level"),
                // a type named in a filter: renaming another type may bring new hits
                "type" => TYPE_TABLES.iter().for_each(|t| self.table(*t)),
                f if BUILTIN.contains(&f) => {}
                // an element parameter
                _ => self.table("prop"),
            }
        }
        self.keys(conn, hits);
    }

    /// Short form for the agent: counts, the predicate tables by name.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "rows": self.row_count(),
            "tables": self.tables.iter().collect::<Vec<_>>(),
            "meta": self.meta,
        })
    }

    fn deps(&mut self, conn: &Connection, mut by: HashMap<&'static str, Vec<i64>>, depth: u8) {
        for _ in 0..depth {
            let mut next: HashMap<&'static str, Vec<i64>> = HashMap::new();
            for (t, ids) in by {
                let Some(links) = links(t) else { continue };
                for (target, id) in linked(conn, t, links, &ids) {
                    if !self.covers(target, id) {
                        next.entry(target).or_default().push(id);
                    }
                    self.row(target, id);
                }
            }
            if next.is_empty() {
                break;
            }
            by = next;
        }
    }
}

/// The element tables of the query rows (`rows::ELEMENT_TABLES`).
const ELEMENT_TABLES: &[&str] = &super::rows::ELEMENT_TABLES;

const TYPE_TABLES: &[&str] = &["wall_type", "slab_type", "member_type", "comp_type"];

/// Row fields of the query that are not element parameters (`query::canon`).
const BUILTIN: &[&str] = &[
    "id", "category", "name", "mark", "width", "height", "sill", "length", "thickness", "area",
    "offset", "volume",
];

/// Column → table a row stands on. `opening.wall` is a host; `mep_run.wall` is a thickness.
fn links(table: &str) -> Option<&'static [(&'static str, &'static str)]> {
    const LV: (&str, &str) = ("level", "level");
    const TOP: (&str, &str) = ("top_level", "level");
    let l: &'static [(&'static str, &'static str)] = match table {
        "wall" => &[LV, ("wtype", "wall_type")],
        "opening" => &[("wall", "wall")],
        "slab" | "roof" | "ceiling" => &[LV, ("stype", "slab_type")],
        "col" => &[LV, TOP, ("mtype", "member_type")],
        "beam" => &[LV, ("mtype", "member_type")],
        "stair" | "shaft" => &[LV, TOP],
        "railing" => &[LV, ("stair", "stair")],
        "comp" => &[
            LV,
            ("ctype", "comp_type"),
            ("host_wall", "wall"),
            ("host_slab", "slab"),
            ("host_ceiling", "ceiling"),
        ],
        "comp_type" => &[("family", "comp_family")],
        "mep_run" => &[LV, ("system", "mep_system")],
        "curtain_wall" | "room" | "mesh" => &[LV],
        "grid" => &[("base_level", "level"), TOP],
        _ => return None,
    };
    Some(l)
}

/// Above this many ids one pass over the table beats a lookup per id.
const SCAN_FROM: usize = 256;

fn linked(
    conn: &Connection,
    table: &str,
    links: &[(&'static str, &'static str)],
    ids: &[i64],
) -> Vec<(&'static str, i64)> {
    fn take(
        r: &rusqlite::Row<'_>,
        links: &[(&'static str, &'static str)],
        out: &mut Vec<(&'static str, i64)>,
    ) {
        for (i, l) in links.iter().enumerate() {
            if let Ok(Some(v)) = r.get::<_, Option<i64>>(i + 1) {
                out.push((l.1, v));
            }
        }
    }
    let list = links.iter().map(|l| l.0).collect::<Vec<_>>().join(", ");
    let mut out = vec![];
    if ids.len() > SCAN_FROM {
        let want: HashSet<i64> = ids.iter().copied().collect();
        let Ok(mut st) = conn.prepare(&format!("SELECT id, {list} FROM {table}")) else {
            return out;
        };
        let Ok(mut rows) = st.query([]) else {
            return out;
        };
        while let Ok(Some(r)) = rows.next() {
            if r.get::<_, i64>(0).is_ok_and(|id| want.contains(&id)) {
                take(r, links, &mut out);
            }
        }
    } else {
        let sql = format!("SELECT id, {list} FROM {table} WHERE id=?1");
        let Ok(mut st) = conn.prepare_cached(&sql) else {
            return out;
        };
        for id in ids {
            if let Ok(mut rows) = st.query([id]) {
                if let Ok(Some(r)) = rows.next() {
                    take(r, links, &mut out);
                }
            }
        }
    }
    out
}

/// Categories a filter is limited to; `None`: any element may match.
fn cats_of(e: &Expr) -> Option<BTreeSet<&'static str>> {
    match e {
        Expr::Cat(c) => Some([*c].into()),
        Expr::And(v) => v.iter().filter_map(cats_of).reduce(|a, b| &a & &b),
        Expr::Or(v) => v.iter().map(cats_of).try_fold(BTreeSet::new(), |mut a, b| {
            a.extend(b?);
            Some(a)
        }),
        Expr::Not(_) | Expr::Cmp { .. } | Expr::Text(_) => None,
    }
}

/// Fields a filter compares; a bare word is matched against type, name and mark.
fn compared(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Cmp { key, .. } => out.push(crate::query::canon(key)),
        Expr::Text(_) => out.extend(["type".to_string(), "name".to_string()]),
        Expr::And(v) | Expr::Or(v) => v.iter().for_each(|x| compared(x, out)),
        Expr::Not(x) => compared(x, out),
        Expr::Cat(_) => {}
    }
}

/// Element, type and level tables: rows people edit. Other tables hold derived data, rebuilt
/// with the element they belong to.
pub fn model_table(t: &str) -> bool {
    ELEMENT_TABLES.iter().chain(KNOWN).any(|k| *k == t)
}

/// Table names of the journal as `&'static str`: a schema has a bounded set of them.
pub fn intern(t: &str) -> &'static str {
    if let Some(k) = ELEMENT_TABLES.iter().chain(KNOWN).find(|k| **k == t) {
        return *k;
    }
    static MORE: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut m = MORE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(k) = m.get(t) {
        return *k;
    }
    let k: &'static str = Box::leak(t.to_string().into_boxed_str());
    m.insert(k);
    k
}

const KNOWN: &[&str] = &[
    "level",
    "wall_type",
    "slab_type",
    "member_type",
    "comp_type",
    "comp_family",
    "mep_system",
    "prop",
    "wall_type_layer",
    "slab_type_layer",
];

thread_local! {
    static REC: RefCell<Option<ReadSet>> = const { RefCell::new(None) };
}

/// Notes a read while [`record`] runs on this thread; the closure runs only then.
pub fn note(f: impl FnOnce(&mut ReadSet)) {
    REC.with(|r| {
        if let Some(rs) = r.borrow_mut().as_mut() {
            f(rs)
        }
    })
}

/// Runs `f` and returns what it read. An outer recording also gets these reads.
pub fn record<T>(f: impl FnOnce() -> T) -> (T, ReadSet) {
    /// Puts the outer recording back if `f` panics.
    struct Restore(Option<Option<ReadSet>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(outer) = self.0.take() {
                REC.with(|r| *r.borrow_mut() = outer);
            }
        }
    }
    let outer = REC.with(|r| r.borrow_mut().replace(ReadSet::default()));
    let mut guard = Restore(Some(outer));
    let out = f();
    let got = REC.with(|r| r.borrow_mut().take()).unwrap_or_default();
    let mut outer = guard.0.take().flatten();
    if let Some(o) = outer.as_mut() {
        o.merge(got.clone());
    }
    REC.with(|r| *r.borrow_mut() = outer);
    (out, got)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{Model, OpeningSpec, WallSpec};

    fn model() -> (Model, i64, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        crate::editor::Editor::seed_model(&mut m).unwrap();
        let lv = m.add_level("L2", 3.0).unwrap();
        let ty: i64 = m
            .sql()
            .query_row("SELECT id FROM wall_type LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let spec = WallSpec {
            a: (0.0, 0.0),
            b: (8.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ty,
            offset0: None,
        };
        let w = m.add_walls(&[spec]).unwrap()[0];
        let d = OpeningSpec {
            along: 2.0,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        };
        let door = m.add_opening(w, &d).unwrap();
        (m, lv, w, door)
    }

    #[test]
    fn readset_get_element_row() {
        let (m, lv, w, _) = model();
        let mut r = ReadSet::default();
        r.keys(m.sql(), &[Key { cat: "wall", id: w }]);
        assert!(r.covers("wall", w));
        assert!(r.covers("level", lv), "{r:?}");
        assert!(r.tables.is_empty(), "a read by id is not a table read");
        assert!(!r.covers("wall", w + 1));
    }

    #[test]
    fn readset_hosts_and_types() {
        let (m, lv, w, door) = model();
        let ty: i64 = m
            .sql()
            .query_row("SELECT wtype FROM wall WHERE id=?1", [w], |r| r.get(0))
            .unwrap();
        let mut r = ReadSet::default();
        r.keys(m.sql(), &[Key { cat: "door", id: door }]);
        assert!(r.covers("opening", door));
        assert!(r.covers("wall", w), "the host");
        assert!(r.covers("level", lv), "the host's level");
        assert!(r.covers("wall_type", ty), "the host's type");
        // the same through one pass over the table
        let many: Vec<Key> = (0..SCAN_FROM as i64 + 5)
            .map(|i| Key { cat: "door", id: door + 1000 + i })
            .chain([Key { cat: "door", id: door }])
            .collect();
        let mut s = ReadSet::default();
        s.keys(m.sql(), &many);
        assert!(s.covers("wall", w) && s.covers("level", lv) && s.covers("wall_type", ty));
    }

    #[test]
    fn readset_filter_is_table_read() {
        let (m, lv, w, door) = model();
        let q = crate::query::parse("door level=L2").unwrap();
        let mut r = ReadSet::default();
        r.filter(m.sql(), &q.expr, &[Key { cat: "door", id: door }]);
        assert!(r.tables.contains("opening"), "{r:?}");
        assert!(r.covers("opening", door + 77), "a new door may match");
        assert!(!r.tables.contains("wall"), "walls are read by id only");
        assert!(r.covers("wall", w) && r.covers("level", lv));
        // no category: every element table
        let q = crate::query::parse("width>500").unwrap();
        let mut all = ReadSet::default();
        all.filter(m.sql(), &q.expr, &[]);
        assert!(ELEMENT_TABLES.iter().all(|t| all.tables.contains(t)));
        let q = crate::query::parse("door | window").unwrap();
        let mut or = ReadSet::default();
        or.filter(m.sql(), &q.expr, &[]);
        assert_eq!(or.tables, ["opening"].into());
    }

    #[test]
    fn table_read_absorbs_rows_and_merges() {
        let mut a = ReadSet::default();
        a.row("wall", 1);
        a.row("wall", 2);
        a.table("wall");
        assert!(a.rows.is_empty() && a.covers("wall", 9));
        let mut b = ReadSet::default();
        b.row("wall", 3);
        b.row("level", 1);
        b.meta = true;
        a.merge(b);
        assert!(!a.rows.contains_key("wall") && a.covers("level", 1) && a.meta);
        assert_eq!(a.row_count(), 1);
    }

    #[test]
    fn writes_come_from_the_journal() {
        let (m, _, w, door) = model();
        let h = m.change_head();
        m.sql()
            .execute("UPDATE opening SET width=1.0 WHERE id=?1", [door])
            .unwrap();
        let ws = ReadSet::from_changes(&m.changes_since(h));
        assert!(ws.covers("opening", door) && !ws.covers("wall", w), "{ws:?}");
        assert!(ReadSet::from_changes(&m.changes_since(u64::MAX)).meta);
        assert!(std::ptr::eq(intern("some_table"), intern("some_table")));
    }

    /// One plan item per wall: its height by reference.
    fn wall_plan(n: usize) -> (crate::editor::Editor, super::super::plan::PlanState, Vec<i64>) {
        use super::super::plan::{ElRef, PlanItem, PlanOp, PlanState, Targets};
        use crate::query::{Assign, Val};
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        let specs: Vec<WallSpec> = (0..n)
            .map(|i| WallSpec {
                a: (0.0, i as f64),
                b: (5.0, i as f64),
                level: ed.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: ed.types[0].id,
                offset0: None,
            })
            .collect();
        let walls = ed.model.add_walls(&specs).unwrap();
        ed.reload().unwrap();
        let mut st = PlanState::default();
        for (i, w) in walls.iter().enumerate() {
            let id = i as u32 + 1;
            st.items.push(PlanItem {
                id,
                title: format!("wall {w}"),
                why: String::new(),
                op: PlanOp::Assign {
                    target: Targets::Refs(vec![ElRef::parse(&format!("wall:{w}")).unwrap()]),
                    set: vec![Assign {
                        key: "height".into(),
                        val: Val::Lit("3500".into()),
                    }],
                },
                checked: true,
                depends_on: vec![],
                actor: "ai".into(),
                conflict: None,
                sets: Default::default(),
            });
            st.next_item = id;
        }
        (ed, st, walls)
    }

    #[test]
    fn readset_survives_replay() {
        use super::super::plan::replay;
        use super::super::sets::Sets;
        let (mut ed, mut st, walls) = wall_plan(2);
        // what a read tool saw before the item: replay cannot see it again
        let mut told = ItemSets::default();
        told.reads.table("opening");
        told.base = 41;
        st.items[0].sets = std::sync::Arc::new(told);
        let top = ed.model.op_top().unwrap();
        replay(&mut st, &mut ed, &Sets::default(), &[], &[]).unwrap();
        let s = &st.items[0].sets;
        assert!(s.reads.tables.contains("opening"), "the tool read stays: {s:?}");
        assert!(s.reads.covers("wall", walls[0]) && s.reads.covers("level", ed.level));
        assert!(!s.reads.covers("wall", walls[1]), "the other item's wall");
        assert!(s.writes.covers("wall", walls[0]) && !s.writes.covers("wall", walls[1]));
        assert_eq!(s.base, 41, "replay keeps the base");
        // back to the base and again, as a re-plan does: the same sets
        let first = (*st.items[1].sets).clone();
        ed.model.rollback_to(top).unwrap();
        ed.reload().unwrap();
        replay(&mut st, &mut ed, &Sets::default(), &[], &[]).unwrap();
        assert_eq!(*st.items[1].sets, first);
    }

    /// The budget: noting reads and writes costs at most 5 % of compiling the plan.
    #[test]
    fn readset_overhead_of_a_1000_item_plan() {
        use super::super::plan::replay;
        use super::super::sets::Sets;
        let (mut ed, mut st, walls) = wall_plan(1000);
        let mut compile = std::time::Duration::MAX;
        let mut noting = std::time::Duration::MAX;
        for _ in 0..3 {
            let top = ed.model.op_top().unwrap();
            let t = std::time::Instant::now();
            replay(&mut st, &mut ed, &Sets::default(), &[], &[]).unwrap();
            compile = compile.min(t.elapsed());
            assert!(st.items.iter().all(|i| i.conflict.is_none() && !i.sets.writes.is_empty()));
            ed.model.rollback_to(top).unwrap();
            ed.reload().unwrap();
            // the same bookkeeping alone
            let head = ed.model.change_head();
            let t = std::time::Instant::now();
            for (it, w) in st.items.iter().zip(&walls) {
                let ((), r) = record(|| {
                    note(|r| r.keys(ed.model.sql(), &[Key { cat: "wall", id: *w }]))
                });
                let mut own = (*it.sets).clone();
                own.reads.merge(r);
                own.writes = ReadSet::from_changes(&ed.model.changes_since(head));
                std::hint::black_box(std::sync::Arc::new(own));
            }
            noting = noting.min(t.elapsed());
        }
        assert!(
            noting.as_secs_f64() <= 0.05 * compile.as_secs_f64(),
            "noting {noting:?} vs compiling {compile:?}"
        );
    }

    #[test]
    fn recording_nests_and_ends() {
        note(|r| r.row("wall", 1));
        let ((), outer) = record(|| {
            note(|r| r.row("wall", 2));
            let ((), inner) = record(|| note(|r| r.row("wall", 3)));
            assert!(inner.covers("wall", 3) && !inner.covers("wall", 2));
        });
        assert!(outer.covers("wall", 2) && outer.covers("wall", 3));
        assert!(!outer.covers("wall", 1), "nothing is noted outside a recording");
        let ((), after) = record(|| {});
        assert!(after.is_empty());
    }
}
