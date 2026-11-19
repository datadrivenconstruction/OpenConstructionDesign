// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Moving the base after a save over the source (contract 6.4, step 7): the base says what the file
//! holds, so the next save and the next update see only what changed since. The plan is taken from
//! the snapshot the file was written from: edits made during the save stay changes.

use std::collections::{BTreeMap, HashSet};

use rusqlite::params;
use serde_json::{json, Value as J};

use super::{current_fields, fields_text, ident, load_base, props_of, ChangeKind, Shapes, GONE};
use crate::ops::Inv;
use crate::{Error, Model};

/// An element the write added to the file: it gets its link, and unless it is a second link of one
/// row (`opening_fill`) a base of its own.
#[derive(Clone, Debug, PartialEq)]
pub struct WrittenLink {
    pub link: String,
    pub id: i64,
    pub guid: String,
    pub vals: Option<BTreeMap<String, J>>,
    /// Relations of the element the file could not hold: kept in the base, tried again next save.
    pub pending: Vec<String>,
}

/// An element in the file whose material or type link is still to be written.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingLink {
    pub guid: String,
    /// The element's table and row.
    pub table: String,
    pub id: i64,
    pub rels: Vec<String>,
}

/// Base key of the relations an appended element still lacks (ignored by every diff).
pub(crate) const PENDING: &str = "~link";

fn pending_json(rels: &[String]) -> J {
    J::Array(rels.iter().map(|r| J::String(r.clone())).collect())
}

fn pending_of(vals: &BTreeMap<String, J>) -> Vec<String> {
    vals.get(PENDING)
        .and_then(J::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceRebase {
    /// Base guid and the fields that now read as the file does.
    pub moved: Vec<(String, Vec<(String, J)>)>,
    /// No longer in the file.
    pub gone: Vec<String>,
    pub linked: Vec<WrittenLink>,
    /// Elements already in the file whose pending links were tried again: guid and what is still
    /// pending (empty clears it).
    pub relinked: Vec<(String, Vec<String>)>,
}

/// Every field a base of this row holds, as an import would record it.
fn row_fields(
    db: &rusqlite::Connection,
    table: &str,
    id: i64,
) -> Result<Option<BTreeMap<String, J>>, Error> {
    let table = ident(table)?;
    let mut keys: Vec<String> = db
        .prepare_cached("SELECT name FROM pragma_table_info(?1)")?
        .query_map([table], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    keys.retain(|k| k != "id");
    if table == "slab" {
        keys.push("rings".to_string());
    }
    keys.extend(
        props_of(db, table, id)?
            .into_keys()
            .map(|k| format!("p:{k}")),
    );
    current_fields(db, table, id, &keys)
}

/// An edit the file could not hold: (kind, row id, field). Id -1 stands for every row of the kind,
/// an empty field for every field of the row.
pub type Unwritten = HashSet<(String, i64, String)>;

fn held_back(u: &Unwritten, kind: &str, id: i64, field: &str) -> bool {
    [id, -1].into_iter().any(|i| {
        [field, ""]
            .into_iter()
            .any(|f| u.contains(&(kind.to_string(), i, f.to_string())))
    })
}

impl Model {
    /// What to move once the file is in place. `unwritten` are the edits the file could not hold:
    /// they stay changes, one by one, so nothing reads as saved that is not in the file and
    /// nothing saved is written again. `linked`:
    /// (link kind, row, guid) of elements the write added.
    pub fn plan_source_rebase(
        &self,
        source: &str,
        unwritten: &Unwritten,
        linked: &[(String, i64, String)],
    ) -> Result<SourceRebase, Error> {
        let mut r = SourceRebase::default();
        for c in self.local_changes(source, false)? {
            match c.change {
                ChangeKind::Edited => {
                    let fields: Vec<_> = c
                        .fields
                        .into_iter()
                        .filter(|f| !f.field.starts_with('~'))
                        .filter(|f| !held_back(unwritten, &c.kind, c.id, &f.field))
                        .map(|f| (f.field, f.mine))
                        .collect();
                    r.moved.push((c.guid, fields));
                }
                ChangeKind::DeletedLocally => {
                    if !held_back(unwritten, &c.kind, c.id, "") {
                        r.gone.push(c.guid)
                    }
                }
                ChangeKind::AddedLocally => {}
            }
        }
        for (link, id, guid) in linked {
            let second = link == "opening_fill";
            let table = if second { "opening" } else { link.as_str() };
            if held_back(unwritten, table, *id, "") {
                continue;
            }
            let Some(vals) = row_fields(&self.db, table, *id)? else {
                continue;
            };
            r.linked.push(WrittenLink {
                link: link.clone(),
                id: *id,
                guid: guid.clone(),
                vals: (!second).then_some(vals),
                pending: vec![],
            });
        }
        Ok(r)
    }

    /// Elements the file holds without a material or type link that an earlier save could not
    /// write. A deleted element has nothing to link any more.
    pub fn pending_links(&self, source: &str) -> Result<Vec<PendingLink>, Error> {
        let mut st = self.db.prepare(
            "SELECT b.guid, b.kind, b.vals, e.kind, e.ref FROM src_base b              JOIN ext_ref e ON e.source=b.source AND e.guid=b.guid              WHERE b.source=?1 AND instr(CAST(b.vals AS TEXT), '\"~link\"') > 0 ORDER BY b.guid",
        )?;
        let rows = st
            .query_map([source], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = vec![];
        for (guid, text, kind, id) in rows {
            let vals: BTreeMap<String, J> = super::parse_fields(&text);
            let rels = pending_of(&vals);
            if rels.is_empty() || vals.contains_key(GONE) {
                continue;
            }
            let table = if kind == "opening_fill" { "opening" } else { kind.as_str() };
            let q = format!("SELECT EXISTS(SELECT 1 FROM {} WHERE id=?1)", ident(table)?);
            if self.db.query_row(&q, [id], |r| r.get::<_, i64>(0))? == 0 {
                continue;
            }
            out.push(PendingLink {
                guid,
                table: table.to_string(),
                id,
                rels,
            });
        }
        Ok(out)
    }

    /// Gives up the pending links of `source` (those of one table, or all): the file keeps what it
    /// holds, the model keeps its links, and the next save no longer retries them. One undo step.
    pub fn drop_pending_links(&mut self, source: &str, table: Option<&str>) -> Result<usize, Error> {
        let list: Vec<PendingLink> = self
            .pending_links(source)?
            .into_iter()
            .filter(|l| table.map_or(true, |t| l.table == t))
            .collect();
        if list.is_empty() {
            return Ok(0);
        }
        self.op("src_drop_link", |db| {
            let mut inv = Inv::default();
            let mut shapes = Shapes::new();
            let mut n = 0;
            for l in &list {
                let Some(mut base) = load_base(db, &mut shapes, source, &l.guid)? else {
                    continue;
                };
                inv = inv.restore(db, "src_base", "id", base.row)?;
                base.vals.remove(PENDING);
                db.execute(
                    "UPDATE src_base SET vals=?2 WHERE id=?1",
                    params![base.row, fields_text(&base.vals)],
                )?;
                n += 1;
            }
            Ok((n, 0, inv))
        })
    }

    /// Gives up one pending link of one element (`rel`), or all of its links when `rel` is None.
    /// Other elements of the same table keep theirs. Returns how many links were dropped. One undo
    /// step.
    pub fn drop_pending_link(
        &mut self,
        source: &str,
        guid: &str,
        rel: Option<&str>,
    ) -> Result<usize, Error> {
        let has = self
            .pending_links(source)?
            .into_iter()
            .find(|l| l.guid == guid)
            .map_or(0, |l| l.rels.iter().filter(|r| rel.map_or(true, |x| *r == x)).count());
        if has == 0 {
            return Ok(0);
        }
        self.op("src_drop_link", |db| {
            let mut shapes = Shapes::new();
            let Some(mut base) = load_base(db, &mut shapes, source, guid)? else {
                return Ok((0, 0, Inv::default()));
            };
            let inv = Inv::default().restore(db, "src_base", "id", base.row)?;
            let rest: Vec<String> = pending_of(&base.vals)
                .into_iter()
                .filter(|r| rel.is_some_and(|x| r != x))
                .collect();
            if rest.is_empty() {
                base.vals.remove(PENDING);
            } else {
                base.vals.insert(PENDING.to_string(), pending_json(&rest));
            }
            db.execute(
                "UPDATE src_base SET vals=?2 WHERE id=?1",
                params![base.row, fields_text(&base.vals)],
            )?;
            Ok((has, 0, inv))
        })
    }

    /// Applies a plan to the live model, whatever was edited since it was taken. Outside undo: the
    /// file does not change back with an undo, so an undone edit is an edit against this base.
    pub fn apply_source_rebase(&self, source: &str, r: &SourceRebase) -> Result<(), Error> {
        let tx = self.db.unchecked_transaction()?;
        let mut shapes = Shapes::new();
        for (guid, fields) in &r.moved {
            let Some(mut base) = load_base(&tx, &mut shapes, source, guid)? else {
                continue;
            };
            base.vals.extend(fields.iter().cloned());
            tx.execute(
                "UPDATE src_base SET vals=?2 WHERE id=?1",
                params![base.row, fields_text(&base.vals)],
            )?;
        }
        // the row stays: an undo that brings the element back must read as an addition
        let gone = json!({ GONE: true }).to_string();
        for guid in &r.gone {
            tx.execute(
                "UPDATE src_base SET shape=0, hashes=x'', vals=?3 WHERE source=?1 AND guid=?2",
                params![source, guid, gone],
            )?;
        }
        for l in &r.linked {
            tx.execute(
                "INSERT OR REPLACE INTO ext_ref(source,guid,kind,ref) VALUES (?1,?2,?3,?4)",
                params![source, l.guid, l.link, l.id],
            )?;
            let Some(vals) = &l.vals else { continue };
            let mut vals = vals.clone();
            if !l.pending.is_empty() {
                vals.insert(PENDING.to_string(), pending_json(&l.pending));
            }
            let text = fields_text(&vals);
            let n = tx.execute(
                "UPDATE src_base SET kind=?3, shape=0, hashes=x'', vals=?4, ifc_guid=?2 \
                 WHERE source=?1 AND guid=?2",
                params![source, l.guid, l.link, text],
            )?;
            if n == 0 {
                tx.execute(
                    "INSERT INTO src_base(source,guid,kind,shape,hashes,vals,ifc_guid) \
                     VALUES (?1,?2,?3,0,x'',?4,?2)",
                    params![source, l.guid, l.link, text],
                )?;
            }
        }
        for (guid, rels) in &r.relinked {
            let Some(mut base) = load_base(&tx, &mut shapes, source, guid)? else {
                continue;
            };
            if base.vals.contains_key(GONE) {
                continue;
            }
            if rels.is_empty() {
                base.vals.remove(PENDING);
            } else {
                base.vals.insert(PENDING.to_string(), pending_json(rels));
            }
            tx.execute(
                "UPDATE src_base SET vals=?2 WHERE id=?1",
                params![base.row, fields_text(&base.vals)],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
