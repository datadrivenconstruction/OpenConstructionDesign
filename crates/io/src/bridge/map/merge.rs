//! Mapper side of the 3-way re-import (ddc_model::source::Merge): every field of a matched row
//! is decided against the last import's base; only `Take` fields are written. Rows deleted here
//! stay deleted unless the source changed them (conflict). Names of unique-named rows (levels,
//! types) follow the source only when the source renamed them.

use std::collections::BTreeMap;

use ddc_bridge_proto::{ParamDef, View, P2};
use ddc_model::source::{self as src, Dead, Decision};
use rusqlite::types::Value as Sql;
use rusqlite::{params, OptionalExtension};
use serde_json::Value as J;

use super::{db_err, model_err, Mapper, Up};
use crate::bridge::BridgeError;

impl<'a> Mapper<'a> {
    /// Link to `table` exists but its row is gone (deleted here): the stale row id.
    fn dead_link(&self, guid: &str, table: &str) -> Result<Option<i64>, BridgeError> {
        let r: Option<i64> = self
            .db
            .prepare_cached("SELECT ref FROM ext_ref WHERE source=?1 AND guid=?2 AND kind=?3")
            .map_err(db_err)?
            .query_row(params![self.src(), guid, table], |r| r.get(0))
            .optional()
            .map_err(db_err)?;
        match r {
            Some(id) if !self.alive(table, id)? => Ok(Some(id)),
            _ => Ok(None),
        }
    }

    /// Writes columns `idx` of `cols`/`vals` into the row (snapshot first).
    fn update_cols(
        &mut self,
        table: &str,
        id: i64,
        cols: &[&str],
        vals: &[Sql],
        idx: &[usize],
    ) -> Result<(), BridgeError> {
        if idx.is_empty() {
            return Ok(());
        }
        self.snaps
            .save(self.db, table, "id", id)
            .map_err(model_err)?;
        let set: Vec<String> = idx
            .iter()
            .enumerate()
            .map(|(k, &i)| format!("{}=?{}", cols[i], k + 2))
            .collect();
        let mut args = vec![Sql::Integer(id)];
        args.extend(idx.iter().map(|&i| vals[i].clone()));
        self.db
            .prepare_cached(&format!("UPDATE {table} SET {} WHERE id=?1", set.join(",")))
            .map_err(db_err)?
            .execute(rusqlite::params_from_iter(args))
            .map_err(db_err)?;
        Ok(())
    }

    /// Existing linked row: merge every column with the base, write the taken ones.
    pub(super) fn merge_existing(
        &mut self,
        guid: &str,
        table: &str,
        id: i64,
        cols: &[&str],
        vals: &[Sql],
    ) -> Result<Up, BridgeError> {
        let old: Vec<Sql> = self
            .db
            .prepare_cached(&format!(
                "SELECT {} FROM {table} WHERE id=?1",
                cols.join(",")
            ))
            .map_err(db_err)?
            .query_row([id], |r| (0..cols.len()).map(|i| r.get(i)).collect())
            .map_err(db_err)?;
        let fields: Vec<(&str, J, J)> = cols
            .iter()
            .zip(&old)
            .zip(vals)
            .map(|((c, o), v)| (*c, src::enc(o), src::enc(v)))
            .collect();
        let dec = self
            .merge
            .row(self.db, &mut *self.snaps, guid, table, id, &fields)
            .map_err(model_err)?;
        // the base keeps only a hash of a geometry body: store theirs whole for "take theirs"
        for (i, d) in dec.iter().enumerate() {
            if let (Decision::Conflict, Sql::Blob(b)) = (d, &vals[i]) {
                self.merge
                    .conflict_theirs(self.db, guid, cols[i], &src::blob_json(b))
                    .map_err(model_err)?;
            }
        }
        let take: Vec<usize> = dec
            .iter()
            .enumerate()
            .filter(|(_, d)| **d == Decision::Take)
            .map(|(i, _)| i)
            .collect();
        if take.is_empty() {
            return Ok(Up::Same(id));
        }
        self.update_cols(table, id, cols, vals, &take)?;
        Ok(Up::Updated(id))
    }

    /// The guid was imported as another kind (mesh → column): the old row goes, so the new one
    /// is inserted and linked in its place instead of becoming a duplicate. True when the old
    /// row is pinned and stays: the caller skips the new one.
    pub(super) fn replace_kind(&mut self, guid: &str, table: &str) -> Result<bool, BridgeError> {
        if self
            .merge
            .replace_blocked(self.db, guid, table)
            .map_err(model_err)?
        {
            self.seen.insert(guid.to_string());
            self.warn(format!("'{guid}' stays as it is: the element is pinned"));
            return Ok(true);
        }
        let r = self
            .merge
            .replace_kind(self.db, &mut *self.snaps, guid, table)
            .map_err(model_err)?;
        if r == Some(true) {
            self.warn(format!(
                "'{guid}' became a {table}: the replaced element had local changes (undo restores it)"
            ));
        }
        Ok(false)
    }

    /// True when the row was deleted here and stays deleted (the caller skips it).
    pub(super) fn merge_dead(
        &mut self,
        guid: &str,
        table: &str,
        cols: &[&str],
        vals: &[Sql],
    ) -> Result<bool, BridgeError> {
        let Some(dead) = self.dead_link(guid, table)? else {
            return Ok(false);
        };
        let fields: Vec<(&str, J)> = cols
            .iter()
            .zip(vals)
            .map(|(c, v)| (*c, src::enc(v)))
            .collect();
        let d = self
            .merge
            .dead(self.db, &mut *self.snaps, guid, table, dead, &fields)
            .map_err(model_err)?;
        Ok(d == Dead::Stay)
    }

    /// Base of a row this import inserted.
    pub(super) fn merge_added(
        &mut self,
        guid: &str,
        table: &str,
        id: i64,
        cols: &[&str],
        vals: &[Sql],
    ) {
        self.guids.insert((table.to_string(), id), guid.to_string());
        let fields: Vec<(&str, J)> = cols
            .iter()
            .zip(vals)
            .map(|(c, v)| (*c, src::enc(v)))
            .collect();
        self.merge.added(guid, table, id, &fields);
    }

    /// An opening whose host wall was deleted here (and stays deleted) is skipped with it.
    pub(super) fn host_deleted_here(
        &mut self,
        guid: &str,
        host: &str,
    ) -> Result<bool, BridgeError> {
        if self.dead_link(host, "wall")?.is_none() || !self.seen.contains(host) {
            return Ok(false);
        }
        self.seen.insert(guid.to_string());
        self.merge.stats.kept_deleted += 1;
        Ok(true)
    }

    /// Slab or ceiling (`owner`) outline as one merged field; true when the caller should write
    /// `rings`.
    pub(super) fn merge_rings(
        &mut self,
        owner: &str,
        guid: &str,
        id: i64,
        rings: &[Vec<P2>],
        new: bool,
    ) -> Result<bool, BridgeError> {
        let theirs = J::String(src::rings_text(rings));
        if new {
            self.merge.base_only(guid, owner, id, "rings", theirs);
            return Ok(true);
        }
        let mine: String = {
            let mut st = self
                .db
                .prepare_cached(&format!(
                    "SELECT ring, x, y FROM {owner}_vertex WHERE {owner}=?1 ORDER BY ring, idx"
                ))
                .map_err(db_err)?;
            let rows = st
                .query_map([id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, f64>(1)?,
                        r.get::<_, f64>(2)?,
                    ))
                })
                .map_err(db_err)?;
            let mut out: Vec<Vec<P2>> = vec![];
            let mut cur = -1;
            for r in rows {
                let (ring, x, y) = r.map_err(db_err)?;
                if ring != cur {
                    out.push(vec![]);
                    cur = ring;
                }
                if let Some(l) = out.last_mut() {
                    l.push((x, y));
                }
            }
            src::rings_text(&out)
        };
        let d = self
            .merge
            .row(
                self.db,
                &mut *self.snaps,
                guid,
                owner,
                id,
                &[("rings", J::String(mine), theirs)],
            )
            .map_err(model_err)?;
        Ok(matches!(d[0], Decision::Take | Decision::Same))
    }

    /// Unique-named row (level, type) matched again: the name follows a rename in the source,
    /// a local rename is kept; `others` are merged like any column.
    pub(super) fn merge_named(
        &mut self,
        guid: &str,
        table: &str,
        id: i64,
        src_name: &str,
        others: &[(&str, Sql)],
    ) -> Result<(), BridgeError> {
        let mut cols = vec!["name"];
        cols.extend(others.iter().map(|o| o.0));
        let cur: Vec<Sql> = self
            .db
            .prepare_cached(&format!(
                "SELECT {} FROM {table} WHERE id=?1",
                cols.join(",")
            ))
            .map_err(db_err)?
            .query_row([id], |r| (0..cols.len()).map(|i| r.get(i)).collect())
            .map_err(db_err)?;
        let cur_name = match &cur[0] {
            Sql::Text(t) => t.clone(),
            _ => String::new(),
        };
        let old_src = self.merge.source_name(self.db, guid).map_err(model_err)?;
        let base_name = self
            .merge
            .base_value(self.db, guid, "name")
            .map_err(model_err)?;
        let theirs_name = match (old_src, base_name) {
            (Some(J::String(o)), Some(J::String(b))) if o == src_name => b,
            (Some(_), _) if src_name == cur_name => cur_name.clone(),
            (Some(_), _) => self.free_name(table, src_name)?,
            // imported before provenance existed: names are left alone
            (None, _) => cur_name.clone(),
        };
        let old_src_j = self
            .merge
            .source_name(self.db, guid)
            .map_err(model_err)?
            .unwrap_or(J::Null);
        let mut vals = vec![Sql::Text(theirs_name.clone())];
        vals.extend(others.iter().map(|o| o.1.clone()));
        let mut fields: Vec<(&str, J, J)> = vec![
            ("name", J::String(cur_name), J::String(theirs_name)),
            ("~name", old_src_j, J::String(src_name.to_string())),
        ];
        for (i, (c, v)) in others.iter().enumerate() {
            fields.push((*c, src::enc(&cur[i + 1]), src::enc(v)));
        }
        let dec = self
            .merge
            .row(self.db, &mut *self.snaps, guid, table, id, &fields)
            .map_err(model_err)?;
        // fields[0] is the name, fields[2..] the others (vals index = field index - 1 past ~name)
        let mut take = vec![];
        if dec[0] == Decision::Take {
            take.push(0);
        }
        for (k, d) in dec.iter().enumerate().skip(2) {
            if *d == Decision::Take {
                take.push(k - 1);
            }
        }
        self.update_cols(table, id, &cols, &vals, &take)
    }

    /// Base of a unique-named row found or inserted by this import.
    pub(super) fn record_named(
        &mut self,
        guid: &str,
        table: &str,
        id: i64,
        src_name: &str,
        others: &[&str],
        new: bool,
    ) -> Result<(), BridgeError> {
        let mut cols = vec!["name"];
        cols.extend_from_slice(others);
        let cur: Vec<Sql> = self
            .db
            .prepare_cached(&format!(
                "SELECT {} FROM {table} WHERE id=?1",
                cols.join(",")
            ))
            .map_err(db_err)?
            .query_row([id], |r| (0..cols.len()).map(|i| r.get(i)).collect())
            .map_err(db_err)?;
        self.guids.insert((table.to_string(), id), guid.to_string());
        let mut fields: Vec<(&str, J)> = cols
            .iter()
            .zip(&cur)
            .map(|(c, v)| (*c, src::enc(v)))
            .collect();
        fields.push(("~name", J::String(src_name.to_string())));
        if new {
            self.merge.added(guid, table, id, &fields);
        } else {
            for (f, v) in fields {
                self.merge.base_only(guid, table, id, f, v);
            }
        }
        Ok(())
    }

    /// Property of a known row: false when the merge keeps the local value (or the row is gone).
    pub(super) fn prop_merge(
        &mut self,
        kind: &str,
        id: i64,
        key: &str,
        val: &Sql,
        mine: Option<&Sql>,
    ) -> Result<bool, BridgeError> {
        let Some(guid) = self.guids.get(&(kind.to_string(), id)).cloned() else {
            return Ok(true);
        };
        if !self.alive(kind, id)? {
            self.merge
                .base_only(&guid, kind, id, &format!("p:{key}"), src::enc(val));
            return Ok(false);
        }
        let mine = mine.map_or(J::Null, src::enc);
        let d = self
            .merge
            .param(
                self.db,
                &mut *self.snaps,
                &guid,
                kind,
                id,
                key,
                mine,
                src::enc(val),
            )
            .map_err(model_err)?;
        Ok(matches!(d, Decision::Take | Decision::Same))
    }

    pub(super) fn param_defs(&mut self, v: Vec<ParamDef>) -> Result<(), BridgeError> {
        for d in &v {
            self.specs.insert(d.key.clone(), d.spec.clone());
        }
        let defs: Vec<src::ParamDef> = v
            .into_iter()
            .map(|d| src::ParamDef {
                key: d.key,
                group: d.group,
                type_param: d.type_param,
                shared: d.shared,
                shared_guid: d.shared_guid,
                spec: d.spec,
            })
            .collect();
        self.merge
            .param_defs(self.db, &mut *self.snaps, &defs)
            .map_err(model_err)
    }

    pub(super) fn views(&mut self, v: Vec<View>) -> Result<(), BridgeError> {
        let views: Vec<src::SourceView> = v
            .into_iter()
            .map(|w| src::SourceView {
                guid: w.guid,
                name: w.name,
                kind: w.kind,
                level: w.level,
            })
            .collect();
        self.merge
            .views(self.db, &mut *self.snaps, &views)
            .map_err(model_err)
    }

    /// End of stream: removed elements and params, new base, provenance row.
    pub(super) fn merge_finish(&mut self) -> Result<(), BridgeError> {
        let r = &self.report;
        let report: BTreeMap<String, i64> = [
            ("levels", r.levels),
            ("walls", r.walls),
            ("doors", r.doors),
            ("windows", r.windows),
            ("slabs", r.slabs),
            ("rooms", r.rooms),
            ("meshes", r.meshes),
            ("curtain_walls", r.curtain_walls),
            ("properties", r.properties),
            ("unchanged", r.unchanged),
            ("updated", r.updated),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v as i64))
        .collect();
        let format = std::path::Path::new(&self.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let w = src::SourceWrite {
            file_key: self.file.clone(),
            path: self.path.clone(),
            format,
            reader: self.report.bridge.clone(),
            reader_version: self.report.bridge_version.clone(),
            info: self.info.clone(),
            report,
            at: 0,
        };
        let deleted = self
            .merge
            .finish(self.db, &mut *self.snaps, &self.seen, &w)
            .map_err(model_err)?;
        if let Some(s) = &self.stamp {
            src::file::stamp_db(self.db, self.src(), s, None).map_err(model_err)?;
        }
        self.report.deleted = deleted;
        self.report.merge = self.merge.stats.clone();
        let pinned = &self.merge.stats.pinned_ids;
        if !pinned.is_empty() {
            let list: Vec<String> = pinned
                .iter()
                .take(20)
                .map(|(k, i)| format!("{k} {i}"))
                .collect();
            let msg = format!(
                "{} pinned element(s) were left as they are: {}",
                pinned.len(),
                list.join(", ")
            );
            self.warn(msg);
        }
        Ok(())
    }
}
