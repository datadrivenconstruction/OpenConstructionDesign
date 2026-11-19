// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Operation journal. Design choice: an operation stores inverse SQL commands (JSON) rather than
//! old/new pairs, so undo is uniform across all tables; row snapshots are taken by `Inv::restore`.

use rusqlite::{params_from_iter, types::Value, Connection, Savepoint, Transaction};
use serde_json::Value as J;

use crate::{Error, Model};

/// Journal kind of the one recompute after held operations (`Model::settle_derived`); inside
/// `Model::step` or `Model::atomic` it merges into their record.
pub(crate) const SETTLE_KIND: &str = "derived_settle";

/// Recording of one operation: its own transaction, or inside `Model::atomic` a savepoint in that
/// transaction (a BEGIN there would fail). Without `commit` it rolls back when the value is dropped.
pub(crate) enum Scope<'c> {
    Tx(Transaction<'c>),
    Sp(Savepoint<'c>),
}

impl std::ops::Deref for Scope<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match self {
            Scope::Tx(t) => t,
            Scope::Sp(s) => s,
        }
    }
}

impl Scope<'_> {
    pub(crate) fn commit(self) -> rusqlite::Result<()> {
        match self {
            Scope::Tx(t) => t.commit(),
            Scope::Sp(s) => s.commit(),
        }
    }
}

/// An ordinary transaction, except an already open one: this way a rollback outside `atomic` resets the change journal.
pub(crate) fn scope(db: &mut Connection) -> rusqlite::Result<Scope<'_>> {
    if db.is_autocommit() {
        db.transaction().map(Scope::Tx)
    } else {
        db.savepoint().map(Scope::Sp)
    }
}

/// Snapshots of rows changed inside a batch; undo restores them after deleting the new rows.
pub struct Snapshots(
    Inv,
    std::collections::HashSet<(String, String, i64)>,
    std::collections::HashMap<String, i64>,
    /// (table, rowid) already snapshotted in this batch.
    std::collections::HashSet<(String, i64)>,
);

impl Snapshots {
    /// Saves the rows `table WHERE col=id` (once per batch) before they are changed.
    pub fn save(&mut self, db: &Connection, table: &str, col: &str, id: i64) -> Result<(), Error> {
        if self.1.insert((table.into(), col.into(), id)) {
            // rows above the batch watermark are deleted by undo: a snapshot would bring them back
            let max = self.2.get(table).copied().unwrap_or(i64::MAX);
            // a row met again under another key (a door moved to a new wall part) keeps its first,
            // pre-batch snapshot: undo replays in order, so a later one would win
            let inv = std::mem::take(&mut self.0);
            let start = inv.0.len();
            let mut inv = inv.restore_upto(db, table, col, id, max)?;
            let new = inv.0.split_off(start);
            let seen = &mut self.3;
            inv.0.extend(new.into_iter().filter(|(_, args)| match args.first() {
                Some(Value::Integer(r)) => seen.insert((table.to_string(), *r)),
                _ => true,
            }));
            self.0 = inv;
        }
        Ok(())
    }

    /// Batch watermarks known before the batch body runs (tables of `batch_in`).
    pub(crate) fn with_marks(marks: std::collections::HashMap<String, i64>) -> Self {
        Snapshots(Inv::default(), Default::default(), marks, Default::default())
    }

    /// Inverse command run before the snapshots: later edits of one batch undo first.
    pub(crate) fn push_front(&mut self, sql: &str, args: Vec<Value>) {
        self.0 .0.insert(0, (sql.to_string(), args));
    }

    /// Rows of `table` inserted after this call are deleted by undo (before any restores):
    /// for tables outside the batch's list. Once per table per batch.
    pub fn watermark(&mut self, db: &Connection, table: &str) -> Result<(), Error> {
        if self.1.insert((table.into(), "rowid".into(), i64::MIN)) {
            let max: i64 = db.query_row(
                &format!("SELECT COALESCE(MAX(rowid),0) FROM {table}"),
                [],
                |r| r.get(0),
            )?;
            self.2.entry(table.to_string()).or_insert(max);
            let rest = std::mem::take(&mut self.0);
            self.0 = Inv::default()
                .sql(
                    &format!("DELETE FROM {table} WHERE rowid>?1"),
                    vec![Value::Integer(max)],
                )
                .then(rest);
        }
        Ok(())
    }

    /// Inverse of a rewrite of rows the batch already changed: it undoes before the snapshots.
    pub(crate) fn push_first(&mut self, inv: Inv) {
        let cur = std::mem::take(&mut self.0);
        self.0 = inv.then(cur);
    }

    /// Adds ready-made inverse commands (cascading deletes inside a batch).
    pub(crate) fn push(&mut self, inv: Inv) {
        let cur = std::mem::take(&mut self.0);
        self.0 = cur.then(inv);
    }

    /// An empty snapshot set for a single operation outside a batch (each row is snapshotted once).
    pub(crate) fn new() -> Self {
        Snapshots(Inv::default(), Default::default(), Default::default(), Default::default())
    }

    pub(crate) fn into_inv(self) -> Inv {
        self.0
    }
}

/// Inverse step with the table name as the only argument: drops the table only when it holds no
/// rows and no other table references it; rows written after the Add keep it ([`take_kept`]).
pub(crate) const DROP_IF_EMPTY: &str = "@drop_if_empty";

thread_local! {
    static KEPT: std::cell::RefCell<Vec<(String, i64)>> = const { std::cell::RefCell::new(vec![]) };
}

/// Tables an undo kept instead of dropping, with their row counts, since the last call.
pub(crate) fn take_kept() -> Vec<(String, i64)> {
    KEPT.with(|k| std::mem::take(&mut *k.borrow_mut()))
}

fn drop_if_empty(db: &Connection, args: &[Value]) -> Result<(), Error> {
    let Some(Value::Text(t)) = args.first() else {
        return Err(Error::Invalid("bad op log".into()));
    };
    let there: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_master WHERE type='table' AND name=?1)",
        [t],
        |r| r.get(0),
    )?;
    if !there {
        return Ok(());
    }
    let tq = format!("\"{}\"", t.replace('"', "\"\""));
    let n: i64 = db.query_row(&format!("SELECT count(*) FROM main.{tq}"), [], |r| r.get(0))?;
    // a kept child still points at its parent: the parent stays too, empty
    let held: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_master m, pragma_foreign_key_list(m.name, 'main') f          WHERE m.type='table' AND m.name<>?1 AND lower(f.\"table\")=lower(?1))",
        [t],
        |r| r.get(0),
    )?;
    if n > 0 {
        KEPT.with(|k| k.borrow_mut().push((t.clone(), n)));
    } else if !held {
        db.execute_batch(&format!("DROP TABLE main.{tq}"))?;
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct Inv(Vec<(String, Vec<Value>)>);

impl Inv {
    pub fn sql(mut self, sql: &str, args: Vec<Value>) -> Self {
        self.0.push((sql.to_string(), args));
        self
    }

    pub fn then(mut self, mut other: Inv) -> Self {
        self.0.append(&mut other.0);
        self
    }

    pub fn delete(self, table: &str, col: &str, id: i64) -> Self {
        self.sql(
            &format!("DELETE FROM {table} WHERE {col}=?1"),
            vec![Value::Integer(id)],
        )
    }

    /// Snapshot of the rows `table WHERE col=id`: undo inserts them back.
    pub fn restore(self, db: &Connection, table: &str, col: &str, id: i64) -> Result<Self, Error> {
        self.restore_where(db, table, &format!("{col}=?1"), &[Value::Integer(id)])
    }

    /// [`Self::restore`] of the rows with rowid <= `max` only.
    pub fn restore_upto(
        self,
        db: &Connection,
        table: &str,
        col: &str,
        id: i64,
        max: i64,
    ) -> Result<Self, Error> {
        self.restore_where(
            db,
            table,
            &format!("{col}=?1 AND rowid<=?2"),
            &[Value::Integer(id), Value::Integer(max)],
        )
    }

    /// Snapshot of `table WHERE <cond>` (`?1..` bound from `args`): undo puts exactly these rows back.
    pub fn restore_where(
        mut self,
        db: &Connection,
        table: &str,
        cond: &str,
        args: &[Value],
    ) -> Result<Self, Error> {
        let mut st = db.prepare_cached(&format!(
            "SELECT rowid AS rowid_, * FROM {table} WHERE {cond} ORDER BY rowid"
        ))?;
        // rowid is preserved: otherwise the restored row would land above the watermark
        // of an earlier batch operation and be deleted by its undo
        let mut cols: Vec<String> = st.column_names().map(|c| c.to_string()).collect();
        cols[0] = "rowid".into();
        let n = cols.len();
        let ph = (1..=n)
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "INSERT OR REPLACE INTO {table}({}) VALUES ({ph})",
            cols.join(",")
        );
        let rows = st.query_map(params_from_iter(args.iter()), |r| {
            (0..n)
                .map(|i| r.get::<_, Value>(i))
                .collect::<Result<Vec<_>, _>>()
        })?;
        for r in rows {
            self.0.push((sql.clone(), r?));
        }
        Ok(self)
    }

    fn encode(&self) -> String {
        let items = self.0.iter().map(|(s, a)| {
            J::Array(vec![
                J::String(s.clone()),
                J::Array(a.iter().map(to_json).collect()),
            ])
        });
        J::Array(items.collect()).to_string()
    }
}

pub(crate) fn to_json(v: &Value) -> J {
    match v {
        Value::Integer(i) => J::from(*i),
        Value::Real(f) => serde_json::Number::from_f64(*f).map_or(J::Null, J::Number),
        Value::Text(s) => J::String(s.clone()),
        // blobs (mesh geometry) as {"blob": hex}: older logs never held one
        Value::Blob(b) => {
            let mut s = String::with_capacity(b.len() * 2);
            for x in b {
                s.push(char::from(HEX[(x >> 4) as usize]));
                s.push(char::from(HEX[(x & 15) as usize]));
            }
            serde_json::json!({ "blob": s })
        }
        Value::Null => J::Null,
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

pub(crate) fn from_json(j: &J) -> Value {
    match j {
        J::Number(n) if n.is_i64() => Value::Integer(n.as_i64().unwrap_or(0)),
        J::Number(n) => Value::Real(n.as_f64().unwrap_or(0.0)),
        J::String(s) => Value::Text(s.clone()),
        J::Object(o) => match o.get("blob").and_then(J::as_str) {
            Some(h) => {
                let d = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
                Value::Blob(
                    h.as_bytes()
                        .chunks_exact(2)
                        .map(|p| (d(p[0]) << 4) | d(p[1]))
                        .collect(),
                )
            }
            None => Value::Null,
        },
        _ => Value::Null,
    }
}

pub(crate) struct Meta<'a> {
    pub name: Option<&'a str>,
    pub old: Option<f64>,
    pub new: Option<f64>,
}

pub(crate) fn log(
    db: &Connection,
    kind: &str,
    element: i64,
    meta: Meta,
    inv: &Inv,
) -> Result<(), Error> {
    db.execute(
        "INSERT INTO op(element, kind, name, old, new, inv, actor) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![
            element,
            kind,
            meta.name,
            meta.old,
            meta.new,
            inv.encode(),
            crate::journal::USER
        ],
    )?;
    Ok(())
}

/// Geometry of an "orphaned" dimension is a memory of its former attachment and cannot be derived
/// from the model: undo restores it, otherwise a dimension temporarily attached to a wall with the same id loses it.
pub(crate) fn orphan_dims(db: &Connection) -> Result<Inv, Error> {
    let mut st = db.prepare_cached("SELECT dim, geom FROM dim_geo WHERE orphan=1")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Value>(1)?)))?;
    let mut inv = Inv::default();
    for r in rows {
        let (d, g) = r?;
        inv = inv.sql(
            "INSERT OR REPLACE INTO dim_geo(dim,orphan,geom) VALUES (?1,1,?2)",
            vec![Value::Integer(d), g],
        );
    }
    Ok(inv)
}

/// Memory of orphaned or unavailable measurements (last value, fallback points): not derivable
/// from the model, so undo brings it back like a dimension's.
pub(crate) fn orphan_measures(db: &Connection) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    if !crate::measure::has_geo(db)? {
        return Ok(inv);
    }
    let mut st = db.prepare_cached(
        "SELECT measure,state,value,geom,fb,reason FROM measure_geo WHERE state<>'live'",
    )?;
    let rows = st.query_map([], |r| {
        (0..6)
            .map(|i| r.get::<_, Value>(i))
            .collect::<rusqlite::Result<Vec<Value>>>()
    })?;
    for r in rows {
        inv = inv.sql(
            "INSERT OR REPLACE INTO measure_geo(measure,state,value,geom,fb,reason) VALUES (?1,?2,?3,?4,?5,?6)",
            r?,
        );
    }
    Ok(inv)
}

#[cfg(test)]
thread_local! {
    /// Gated clean-up passes run on this thread (tests prove they skip untouched tables).
    pub(crate) static PASSES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[inline]
fn pass_ran() {
    #[cfg(test)]
    PASSES.with(|c| c.set(c.get() + 1));
}

impl Model {
    /// One operation = one transaction: the change + a journal entry (+ wall regeneration).
    pub(crate) fn op<T>(
        &mut self,
        kind: &str,
        f: impl FnOnce(&Connection) -> Result<(T, i64, Inv), Error>,
    ) -> Result<T, Error> {
        let _all = crate::perf::span("model.op");
        crate::struct_ext::clear_stash();
        let tx = scope(&mut self.db)?;
        let pre = crate::perf::span("model.op.pre");
        let orphans = if crate::derives(kind) {
            orphan_dims(&tx)?.then(orphan_measures(&tx)?)
        } else {
            Inv::default()
        };
        crate::phases::begin(&tx)?;
        crate::mep::clash_begin(&tx)?;
        let rec_mark = crate::xlsx_rec::mark(&tx)?;
        drop(pre);
        let body = crate::perf::span("model.op.body");
        let (out, element, inv) = f(&tx)?;
        drop(body);
        // roof layers, material conductivity and level elevation change the roof underside and the load-bearing layer;
        // a slab above a wall and a roof cutout (dormer, shaft, lantern) also affect wall tops
        let inv = if matches!(
            kind,
            "level_edit" | "material_edit" | "material_replace" | "host_opening_add" | "host_opening_delete" | "shaft_add" | "shaft_edit" | "roof_skylight"
                | "roof_ext" | "roof_join"
        ) || kind.starts_with("slab_")
            || kind.starts_with("shell_")
            || kind.starts_with("freeform_")
        {
            crate::roof_auto::sync(&tx, inv, None)?
        } else {
            inv
        };
        let passes = crate::perf::span("model.op.passes");
        // related distributions and host tags: their snapshots come first (undo runs in order)
        let inv = if crate::derives(kind) {
            // relayout first, as before the gating: the cascade sees what it deleted
            let rl = crate::constraints::relayout(&tx)?;
            let casc = if crate::changes::touch_of(&self.tracker).any_deleted() {
                pass_ran();
                crate::annot::cascade(&tx)?
            } else {
                Inv::default()
            };
            rl.then(casc).then(inv)
        } else {
            inv
        };
        let inv = crate::mass::after_op(&tx, kind, inv)?.then(orphans);
        let inv = crate::groups::after_op(&tx, inv)?;
        // outline edges linked to walls follow them in the same step
        let touch = crate::changes::touch_of(&self.tracker);
        let inv = crate::boundary_link::after_op(&tx, &touch, inv)?;
        // solid attachments to deleted hosts are removed in the same entry
        // only tables this operation deleted from or wrote to can leave dangling rows
        let t = crate::changes::touch_of(&self.tracker);
        let inv = if crate::derives(kind) {
            let s = if t.deleted(&["wall", "slab", "roof", "solid"]) || t.touched(&["solid_host"]) {
                { pass_ran(); crate::solids::prune_hosts(&tx)? }
            } else {
                Inv::default()
            };
            let r = if crate::rebar::hosts_changed(&t) {
                { pass_ran(); crate::rebar::prune_hosts(&tx)? }
            } else {
                Inv::default()
            };
            inv.then(s).then(r)
        } else {
            inv
        };
        // connections of deleted components and systems go away in the same entry (undo restores them)
        let inv = if crate::changes::touch_of(&self.tracker).deleted(&["comp", "mep_system", "mep_run"])
            || t.touched(&["mep_port"])
        {
            pass_ran();
            inv.then(crate::mep::prune_ports(&tx)?)
        } else {
            inv
        };
        let inv = if t.deleted(&["comp", "mep_system"]) {
            pass_ran();
            inv.then(crate::mep::prune_circuits(&tx)?)
        } else {
            inv
        };
        // locks of deleted rows go with them (a reused rowid starts unlocked)
        let inv = if t.any_deleted() {
            pass_ran();
            inv.then(crate::groups::prune_locks(&tx)?)
        } else {
            inv
        };
        // the cascade of phases/parameters of deleted rows and the phase of new elements, in the same entry
        let inv = crate::phases::settle(&tx, inv)?;
        // XLSX created-row records of deleted elements: same entry, so undo keeps the pairing
        let inv = crate::xlsx_rec::prune(&tx, rec_mark, inv)?;
        drop(passes);
        // regeneration overrides (fallback geometry behind hosts) are undone first
        let held = crate::derives(kind)
            && self.regen.held
            && !crate::constraints::any_dist(&tx)?;
        let inv = if held {
            // `settle_derived` recomputes once for the rows of every held operation
            self.regen.owed = true;
            inv
        } else if crate::derives(kind) {
            let _r = crate::perf::span("model.op.regen");
            crate::regen::logged(&tx, &self.tracker, &mut self.regen)?.then(inv)
        } else {
            inv
        };
        let _fin = crate::perf::span("model.op.log_commit");
        // clash pairs of deleted elements (status, comment) follow the elements themselves
        let inv = crate::mep::clash_gone(&tx, inv)?;
        let meta = Meta {
            name: None,
            old: None,
            new: None,
        };
        log(&tx, kind, element, meta, &inv)?;
        tx.commit()?;
        if crate::derives(kind) {
            self.regen.promote(self.change_head());
        }
        self.adopt_struct();
        Ok(out)
    }

    /// A batch operation over `d_*`: any number of inserts = one journal entry and one undo.
    /// The inverse command deletes rows with rowid above the previous maximum of each table.
    /// Edits of existing rows inside a batch are not rolled back: a batch only adds.
    pub fn batch<T>(
        &mut self,
        kind: &str,
        f: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        const TABLES: [&str; 11] = [
            "d_layer",
            "d_ltype",
            "d_block",
            "d_line",
            "d_polyline",
            "d_arc",
            "d_text",
            "d_insert",
            "d_vertex",
            "d_attdef",
            "d_attr",
        ];
        self.batch_in(kind, &TABLES, |db, _| f(db))
    }

    /// A batch over levels, wall and slab types, walls, openings, slabs, rooms, meshes, external references and properties (BIM import): one undo,
    /// quantities are recomputed in the same transaction. `kind` starts with "wall".
    pub fn batch_walls<T>(
        &mut self,
        kind: &str,
        f: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.batch_walls_edit(kind, |db, _| f(db))
    }

    /// Like [`Self::batch_walls`], but edits of existing rows are undoable too: before UPDATE/DELETE
    /// the caller saves the rows via [`Snapshots::save`].
    pub fn batch_walls_edit<T>(
        &mut self,
        kind: &str,
        f: impl FnOnce(&Connection, &mut Snapshots) -> Result<T, Error>,
    ) -> Result<T, Error> {
        // regeneration is keyed by `derives(kind)` (1ee6bbdb), so any deriving prefix
        // (roof re-import, slab, ...) keeps quantities fresh in the same transaction
        if !crate::derives(kind) {
            return Err(Error::Invalid(
                "wall batch kind must start with 'wall' or another deriving prefix".into(),
            ));
        }
        const TABLES: [&str; 31] = [
            "level",
            "wall_type",
            "wall_type_layer",
            "wall",
            "opening",
            "slab_type",
            "slab_type_layer",
            "slab",
            "slab_vertex",
            "room",
            "room_sep",
            "mesh",
            "ext_ref",
            "prop",
            "curtain_wall",
            "curtain_panel",
            "grid",
            "ref_plane",
            "comp_family",
            "comp_type",
            "comp",
            "member_type",
            "col",
            "beam",
            "roof",
            "roof_vertex",
            "mep_system",
            "mep_run",
            "ceiling",
            "ceiling_vertex",
            "solid",
        ];
        self.batch_in(kind, &TABLES, f)
    }

    pub(crate) fn batch_in<T>(
        &mut self,
        kind: &str,
        tables: &[&str],
        f: impl FnOnce(&Connection, &mut Snapshots) -> Result<T, Error>,
    ) -> Result<T, Error> {
        // rowid grows monotonically even for tables with a composite key, so a per-table watermark
        // also undoes children added to already existing parents.
        self.op(kind, |db| {
            let mut before = vec![];
            for t in tables {
                before.push(db.query_row(
                    &format!("SELECT COALESCE(MAX(rowid),0) FROM {t}"),
                    [],
                    |r| r.get::<_, i64>(0),
                )?);
            }
            let mut snaps = Snapshots::with_marks(
                tables.iter().map(|t| t.to_string()).zip(before.iter().copied()).collect(),
            );
            let out = f(db, &mut snaps)?;
            let mut inv = Inv::default();
            for (t, max) in tables.iter().zip(before).rev() {
                // a table the batch added no rows to needs no watermark: with dozens of
                // tables the journal row would outgrow the edit itself
                let now: i64 = db.query_row(
                    &format!("SELECT COALESCE(MAX(rowid),0) FROM {t}"),
                    [],
                    |r| r.get(0),
                )?;
                if now <= max {
                    continue;
                }
                inv = inv.sql(
                    &format!("DELETE FROM {t} WHERE rowid>?1"),
                    vec![Value::Integer(max)],
                );
            }
            Ok((out, 0, inv.then(snaps.0)))
        })
    }

    /// Operations from here on write their rows but leave the recompute of derived tables
    /// (joins, rooms, quantities) to [`Self::settle_derived`]: a long run of operations then
    /// recomputes each neighbourhood once instead of once per operation. Undo still recomputes.
    pub fn hold_derived(&mut self) {
        self.regen.held = true;
    }

    /// Ends [`Self::hold_derived`]: one recompute for the rows of every held operation, logged
    /// as one more journal record (its undo commands run first, as an operation's own recompute
    /// does). The derived tables end as they would have op by op. Nothing held: no record.
    pub fn settle_derived(&mut self) -> Result<(), Error> {
        self.regen.held = false;
        if !std::mem::take(&mut self.regen.owed) {
            return Ok(());
        }
        let _s = crate::perf::span("model.settle_derived");
        crate::struct_ext::clear_stash();
        let res = (|| -> Result<(), Error> {
            let tx = scope(&mut self.db)?;
            // dimension geometry is still as before the held operations: undo brings it back
            let orphans = orphan_dims(&tx)?.then(orphan_measures(&tx)?);
            let inv = crate::regen::logged(&tx, &self.tracker, &mut self.regen)?.then(orphans);
            let meta = Meta {
                name: None,
                old: None,
                new: None,
            };
            log(&tx, SETTLE_KIND, 0, meta, &inv)?;
            tx.commit()?;
            Ok(())
        })();
        if res.is_err() {
            // the rows stay: the next recompute is a full one
            self.regen = Default::default();
            return res;
        }
        self.regen.promote(self.change_head());
        self.adopt_struct();
        Ok(())
    }

    /// `f` with the derived tables recomputed once at its end ([`Self::hold_derived`]); also
    /// after an error or a panic in `f`, so they never stay behind the rows it wrote.
    pub fn with_derived_held<T>(
        &mut self,
        f: impl FnOnce(&mut Model) -> Result<T, Error>,
    ) -> Result<T, Error> {
        if self.regen.held {
            // the outer hold settles
            return f(self);
        }
        self.hold_derived();
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut *self)));
        let settled = self.settle_derived();
        match run {
            Ok(Ok(v)) => settled.map(|()| v),
            Ok(Err(e)) => Err(e),
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    /// Tables the last [`Self::undo`] kept instead of dropping (name, rows): rows written
    /// after the Add that created them. Taken once; empty when every created table went.
    pub fn undo_kept(&self) -> Vec<(String, i64)> {
        take_kept()
    }

    pub fn undo(&mut self) -> Result<(), Error> {
        let _s = crate::perf::span("model.undo");
        let tx = scope(&mut self.db)?;
        let (seq, inv): (i64, String) = tx
            .query_row(
                "SELECT seq, inv FROM op ORDER BY seq DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Error::NothingToUndo,
                e => e.into(),
            })?;
        // only this step's notes: a failed earlier undo rolled its own back
        take_kept();
        run_inv(&tx, &inv, Some(&*self.tracker))?;
        tx.execute("DELETE FROM op WHERE seq=?1", [seq])?;
        crate::ai_audit::note_undo(&tx, seq)?;
        crate::struct_ext::clear_stash();
        crate::walls::LENIENT.with(|l| l.set(true));
        let regen = crate::regen::logged(&tx, &self.tracker, &mut self.regen);
        crate::walls::LENIENT.with(|l| l.set(false));
        regen?;
        tx.commit()?;
        self.regen.promote(self.change_head());
        self.adopt_struct();
        Ok(())
    }
}

/// Executes the inverse commands of one journal entry.
/// `tracker`: the session's change record; with it an Add's undo keeps rows written after the
/// Add outside undo steps ([`crate::stage::drop_added`]).
pub(crate) fn run_inv(
    db: &Connection,
    inv: &str,
    tracker: Option<&std::sync::Mutex<crate::changes::Tracker>>,
) -> Result<(), Error> {
    let J::Array(stmts) = serde_json::from_str::<J>(inv)? else {
        return Err(Error::Invalid("bad op log".into()));
    };
    for s in &stmts {
        let sql = s[0].as_str().unwrap_or_default();
        let args: Vec<Value> = s[1]
            .as_array()
            .map_or(vec![], |a| a.iter().map(from_json).collect());
        if sql == crate::meshes::MESH_XF {
            crate::meshes::undo_xf(db, &args)?;
            continue;
        }
        if sql == crate::stage::DROP_ADDED {
            crate::stage::drop_added(db, &args, tracker)?;
            continue;
        }
        if sql == DROP_IF_EMPTY {
            drop_if_empty(db, &args)?;
            continue;
        }
        inv_sql_allowed(sql)?;
        db.execute(sql, params_from_iter(args.iter()))?;
    }
    Ok(())
}

/// The op log comes from the file, so a crafted file chooses this SQL. Only one plain row
/// edit may run: ATTACH or VACUUM INTO would write any file on disk.
pub(crate) fn inv_sql_allowed(sql: &str) -> Result<(), Error> {
    if crate::sql_guard::is_row_edit(sql) {
        Ok(())
    } else {
        let head: String = sql.trim().chars().take(40).collect();
        Err(Error::Corrupt(format!("op log holds a statement that is not a row edit: {head}")))
    }
}

/// Rolls the file's journal back to entry `to` (it is kept, inclusive) without recomputing derived data:
/// the file copy becomes the state at that history point (the merge base).
pub(crate) fn rewind(db: &mut Connection, to: i64) -> Result<(), Error> {
    let tx = db.transaction()?;
    let rows: Vec<String> = tx
        .prepare("SELECT inv FROM op WHERE seq>?1 ORDER BY seq DESC")?
        .query_map([to], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for inv in &rows {
        run_inv(&tx, inv, None)?;
    }
    tx.execute("DELETE FROM op WHERE seq>?1", [to])?;
    tx.commit()?;
    take_kept();
    Ok(())
}

/// The journal operations after `before` are merged into one (`kind`): undo runs their inverse
/// commands from last to first.
pub(crate) fn merge_ops(db: &mut Connection, kind: &str, before: i64) -> Result<(), Error> {
    let _s = crate::perf::span("model.merge_ops");
    let tx = scope(db)?;
    type Row = (i64, String, Option<String>, Option<String>, Option<String>, Value);
    let rows: Vec<Row> = tx
        .prepare(
            "SELECT element, inv, actor, title, why, src FROM op WHERE seq>?1 ORDER BY seq DESC",
        )?
        .query_map([before], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .collect::<Result<_, _>>()?;
    if rows.len() < 2 {
        return Ok(());
    }
    let mut all = vec![];
    for r in &rows {
        if let J::Array(mut a) = serde_json::from_str::<J>(&r.1)? {
            all.append(&mut a);
        }
    }
    let element = rows.last().map_or(0, |r| r.0);
    // provenance survives the merge: the newest row with an envelope, else the newest actor
    let keep = rows
        .iter()
        .find(|r| !matches!(r.5, Value::Null))
        .or_else(|| rows.iter().find(|r| r.2.is_some()));
    let (actor, title, why, src) = match keep {
        Some(r) => (r.2.clone(), r.3.clone(), r.4.clone(), r.5.clone()),
        None => (None, None, None, Value::Null),
    };
    let actor = actor.unwrap_or_else(|| crate::journal::USER.to_owned());
    tx.execute("DELETE FROM op WHERE seq>?1", [before])?;
    tx.execute(
        "INSERT INTO op(element, kind, inv, actor, title, why, src) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![element, kind, J::Array(all).to_string(), actor, title, why, src],
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod gating_tests {
    use super::PASSES;
    use crate::walls::{LayerSpec, WallSpec};
    use crate::Model;

    fn passes() -> u32 {
        PASSES.with(|c| c.get())
    }

    #[test]
    fn moving_a_wall_skips_prune_passes_deleting_runs_them() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "one",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.3,
                }],
            )
            .unwrap();
        let mut ids = vec![];
        for i in 0..200 {
            let y = i as f64 * 5.0;
            ids.push(
                m.add_wall(&WallSpec {
                    a: (0., y),
                    b: (4., y),
                    level: lv,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: t,
                    offset0: None,
                })
                .unwrap(),
            );
        }
        let p0 = passes();
        m.set_wall_axis(ids[0], (0., 0.), (6., 0.)).unwrap();
        assert_eq!(passes(), p0, "a move deletes nothing: no prune pass runs");
        m.delete_wall(ids[1]).unwrap();
        assert!(passes() > p0, "a delete runs the passes");
    }
}

#[cfg(test)]
mod snapshot_mark_tests {
    use crate::Model;
    use rusqlite::types::Value;

    #[test]
    fn value_inserted_then_updated_in_one_batch_is_gone_after_undo() {
        let mut m = Model::open(":memory:").unwrap();
        m.batch_walls_edit("wall_snap_test", |db, s| {
            s.watermark(db, "pparam")?;
            db.execute(
                "INSERT INTO pparam(name, ptype, per_type, unit, grp) VALUES ('P','text',0,'','')",
                [],
            )?;
            let slot = (db.last_insert_rowid(), "wall".to_string(), 7);
            crate::source::write_uparam(db, s, &slot, &Value::Text("a".into()))?;
            crate::source::write_uparam(db, s, &slot, &Value::Text("b".into()))?;
            // prop is a batch table: a row inserted and then snapshotted by ref
            db.execute(
                "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('wall',7,'k','a','')",
                [],
            )?;
            s.save(db, "prop", "ref", 7)?;
            db.execute("UPDATE prop SET value='b' WHERE ref=7", [])?;
            Ok(())
        })
        .unwrap();
        m.undo().unwrap();
        let n = |sql: &str| -> i64 { m.db.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(n("SELECT count(*) FROM pparam_val"), 0);
        assert_eq!(n("SELECT count(*) FROM pparam"), 0);
        assert_eq!(n("SELECT count(*) FROM prop WHERE ref=7"), 0);
    }
}

#[cfg(test)]
mod sec_undo_tests {
    use super::inv_sql_allowed;
    use crate::{Error, Model};

    #[test]
    fn sec_inv_sql_allows_row_edits_only() {
        for ok in [
            "DELETE FROM wall WHERE id=?1",
            "  insert or replace into prop(rowid, kind) VALUES (?1,?2);",
            "UPDATE level SET elevation=?2 WHERE id=?1",
            "REPLACE INTO prop(kind) VALUES (?1)",
            "UPDATE prop SET v='a; b' WHERE id=?1",
        ] {
            assert!(inv_sql_allowed(ok).is_ok(), "{ok}");
        }
        for bad in [
            "ATTACH DATABASE 'x.bat' AS z",
            "attach 'x' as z",
            "AtTaCh 'x' AS z",
            "UPDATE prop SET v='a' WHERE id=?1; AtTaCh 'x' AS z",
            "WITH g AS (SELECT 1) DELETE FROM wall",
            "VACUUM INTO 'x.db'",
            "PRAGMA writable_schema=ON",
            "DETACH z",
            "/* c */ ATTACH 'x' AS z",
            "DELETE FROM wall; ATTACH 'x' AS z",
            "CREATE TABLE z.t(a)",
            "DROP TABLE wall",
            "SELECT 1",
            "DELETE",
            "",
        ] {
            assert!(inv_sql_allowed(bad).is_err(), "{bad}");
        }
    }

    /// A crafted file whose op log attaches a new database must not write that file on undo.
    #[test]
    fn sec_undo_of_a_crafted_op_log_writes_no_file() {
        let dir = std::env::temp_dir().join(format!("ddc-sec-undo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("dropped.bat");
        let _ = std::fs::remove_file(&target);
        let mut m = Model::open(":memory:").unwrap();
        let path = target.to_string_lossy().replace('\'', "''");
        let inv = serde_json::json!([
            [format!("ATTACH DATABASE '{path}' AS z"), []],
            ["CREATE TABLE z.t(a)", []],
            ["INSERT INTO z.t VALUES ('payload')", []]
        ])
        .to_string();
        m.sql()
            .execute("INSERT INTO op(element, kind, inv) VALUES (0, 'set', ?1)", [&inv])
            .unwrap();
        let r = m.undo();
        assert!(matches!(r, Err(Error::Corrupt(_))), "{r:?}");
        assert!(!target.exists(), "undo wrote {}", target.display());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
