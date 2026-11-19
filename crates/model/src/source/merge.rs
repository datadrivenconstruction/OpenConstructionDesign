//! 3-way re-import within an import batch (one undo). The importer reports each
//! element field (mine is current, theirs is from the new file); the decision compares with the base.
//! Non-conflicting changes are applied, conflicts are written to `src_conflict` (mine stays),
//! and at the end base = new file. Deleted in the source: unedited ones are deleted (policy), edited
//! ones or ones carrying local work are a conflict. Deleted here and unchanged there stay deleted.
//! If the file does not recognise most of the previous elements (another project or the id format changed),
//! the merge refuses entirely, with no mass deletion.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use rusqlite::types::Value as Sql;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value as J;

use super::{
    added_props, current_fields, diff, fields_text, fnv, hashes_blob, load_base, props_of,
    same_value, vhash, Base, Bv, ConflictCase, ParamDef, Shapes, SourceView, DELETABLE, TABLES,
};
use super::rebase::PENDING;
use crate::ops::Inv;
use crate::{Error, Snapshots};

static NULL: J = J::Null;

/// Re-import refuses if more than half of the previous elements (at least [`GUARD_MIN`]) are
/// missing from the file.
pub const GUARD_MIN: usize = 20;
/// Start of the error text for such a refusal (the window shows its own message).
pub const MASS_CHANGE: &str = "source ids changed";
/// How many of the latest import batches per source keep the previous base rows for undo.
const KEEP_BATCHES: i64 = 4;

/// A decision for one field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Mine = theirs: nothing.
    Same,
    /// Unchanged here, changed in the source: take theirs.
    Take,
    /// Unchanged in the source (or changed the same way): keep mine.
    Keep,
    /// Changed both here and there, differently: a conflict, mine stays until resolved.
    Conflict,
}

/// Base (what was), mine, theirs -> decision. Without a base (an element from an old schema) theirs is taken.
pub fn decide(base: Option<&J>, mine: &J, theirs: &J) -> Decision {
    decide_bv(base.map(Bv::Val), mine, theirs)
}

fn decide_bv(base: Option<Bv>, mine: &J, theirs: &J) -> Decision {
    if same_value(mine, theirs) {
        return Decision::Same;
    }
    match base {
        None => Decision::Take,
        Some(b) if b.same(mine) => Decision::Take,
        Some(b) if b.same(theirs) => Decision::Keep,
        Some(_) => Decision::Conflict,
    }
}

/// A model row deleted here came from the source again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dead {
    /// Stays deleted (unchanged there or a conflict is recorded).
    Stay,
    /// No base (old schema): create anew, as before.
    Recreate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Policy {
    /// Delete unedited elements that are absent from the new file (otherwise only report).
    pub delete_removed: bool,
    /// Do not refuse when the file does not recognise most of the previous elements.
    pub allow_mass_change: bool,
}

/// Merge results (for the report and the dialog).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MergeStats {
    /// New in the source.
    pub added: usize,
    /// Elements where at least one field was taken from the source.
    pub merged: usize,
    /// Fields taken from the source.
    pub taken: usize,
    /// Fields with a local edit that was kept (unchanged in the source).
    pub kept_local: usize,
    /// New conflicts.
    pub conflicts: usize,
    /// Deleted because they vanished from the source (not edited here).
    pub deleted_upstream: usize,
    /// Deleted here and left deleted.
    pub kept_deleted: usize,
    /// Parameters deleted from the source and deleted here.
    pub params_removed: usize,
    /// Pinned elements: source changes were not applied to them.
    pub pinned: usize,
    /// Which ones exactly: (table, id).
    pub pinned_ids: Vec<(String, i64)>,
    /// Locked parameter values the source would change, kept: (table, id, parameter).
    pub locked: Vec<(String, i64, String)>,
}

impl MergeStats {
    pub fn pairs(&self) -> [(&'static str, usize); 9] {
        [
            ("added", self.added),
            ("merged", self.merged),
            ("taken", self.taken),
            ("kept_local", self.kept_local),
            ("conflicts", self.conflicts),
            ("deleted_upstream", self.deleted_upstream),
            ("kept_deleted", self.kept_deleted),
            ("params_removed", self.params_removed),
            ("pinned", self.pinned),
        ]
    }
}

/// Source information for `src_file`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceWrite {
    pub file_key: String,
    pub path: String,
    pub format: String,
    pub reader: String,
    pub reader_version: String,
    pub info: BTreeMap<String, String>,
    /// Import results (extended with the merge results).
    pub report: BTreeMap<String, i64>,
    /// Import time, unix seconds; 0 means now.
    pub at: i64,
}

/// An element's new base: values (columns, outlines, name; a parameter only if the row
/// keeps a different value) and hashes of the other parameters (the key is the number in `Merge::names`).
struct Next {
    kind: String,
    id: i64,
    alive: bool,
    vals: BTreeMap<String, J>,
    hashed: Vec<(u32, u64)>,
}

/// Merge state of one import (one source).
pub struct Merge {
    source: String,
    pub policy: Policy,
    next: HashMap<String, Next>,
    old: HashMap<String, Option<Arc<Base>>>,
    shapes: Shapes,
    shape_ids: HashMap<String, i64>,
    keys: HashMap<String, u32>,
    names: Vec<String>,
    ifc: HashMap<String, String>,
    merged: HashSet<String>,
    /// Inserted by this import: their fields are neither decided nor counted.
    added: HashSet<String>,
    began: bool,
    batch: Option<i64>,
    prev: HashSet<i64>,
    pub stats: MergeStats,
}

fn now_unix() -> i64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Merge {
    pub fn new(source: &str, policy: Policy) -> Self {
        Self {
            source: source.to_string(),
            policy,
            next: HashMap::new(),
            old: HashMap::new(),
            shapes: Shapes::new(),
            shape_ids: HashMap::new(),
            keys: HashMap::new(),
            names: vec![],
            ifc: HashMap::new(),
            merged: HashSet::new(),
            added: HashSet::new(),
            began: false,
            batch: None,
            prev: HashSet::new(),
            stats: MergeStats::default(),
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Called before the first write: new source-table rows are reverted by the batch undo.
    pub fn begin(&mut self, db: &Connection, snaps: &mut Snapshots) -> Result<(), Error> {
        if !self.began {
            for t in TABLES {
                snaps.watermark(db, t)?;
            }
            self.began = true;
        }
        Ok(())
    }

    fn old(&mut self, db: &Connection, guid: &str) -> Result<Option<Arc<Base>>, Error> {
        if let Some(o) = self.old.get(guid) {
            return Ok(o.clone());
        }
        let b = load_base(db, &mut self.shapes, &self.source, guid)?.map(Arc::new);
        self.old.insert(guid.to_string(), b.clone());
        Ok(b)
    }

    /// A base of the same kind: a row that changed table (mesh -> column) is compared as new.
    fn old_of(
        &mut self,
        db: &Connection,
        guid: &str,
        kind: &str,
    ) -> Result<Option<Arc<Base>>, Error> {
        Ok(self.old(db, guid)?.filter(|o| o.kind == kind))
    }

    fn key(&mut self, f: &str) -> u32 {
        if let Some(k) = self.keys.get(f) {
            return *k;
        }
        let k = self.names.len() as u32;
        self.names.push(f.to_string());
        self.keys.insert(f.to_string(), k);
        k
    }

    fn next_entry(&mut self, guid: &str, kind: &str, id: i64, alive: Option<bool>) -> &mut Next {
        let n = self.next.entry(guid.to_string()).or_insert_with(|| Next {
            kind: kind.to_string(),
            id,
            alive: alive.unwrap_or(true),
            vals: BTreeMap::new(),
            hashed: vec![],
        });
        n.kind = kind.to_string();
        n.id = id;
        if let Some(a) = alive {
            n.alive = a;
        }
        n
    }

    /// A new-base field; `value` means store the parameter value (the row keeps a different one).
    #[allow(clippy::too_many_arguments)]
    fn put(
        &mut self,
        guid: &str,
        kind: &str,
        id: i64,
        alive: Option<bool>,
        f: &str,
        theirs: J,
        value: bool,
    ) {
        let hashed = (f.starts_with("p:") && !value).then(|| self.key(f));
        let n = self.next_entry(guid, kind, id, alive);
        match hashed {
            Some(k) => {
                n.vals.remove(f);
                n.hashed.push((k, vhash(&theirs)));
            }
            None => {
                n.vals.insert(f.to_string(), theirs);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn conflict(
        &mut self,
        db: &Connection,
        snaps: &mut Snapshots,
        guid: &str,
        kind: &str,
        id: i64,
        field: &str,
        case: ConflictCase,
        base: &J,
        mine: &J,
        theirs: &J,
    ) -> Result<(), Error> {
        self.begin(db, snaps)?;
        let prev: Option<i64> = db
            .prepare_cached("SELECT id FROM src_conflict WHERE source=?1 AND guid=?2 AND field=?3")?
            .query_row(params![self.source, guid, field], |r| r.get(0))
            .optional()?;
        if let Some(p) = prev {
            snaps.save(db, "src_conflict", "id", p)?;
            db.execute("DELETE FROM src_conflict WHERE id=?1", [p])?;
        }
        db.prepare_cached(
            "INSERT INTO src_conflict(source,guid,kind,ref,field,case_kind,base,mine,theirs) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )?
        .execute(params![
            self.source,
            guid,
            kind,
            id,
            field,
            case.key(),
            base.to_string(),
            mine.to_string(),
            theirs.to_string()
        ])?;
        self.stats.conflicts += 1;
        Ok(())
    }

    /// The full "theirs" value of a conflict recorded by this import (a mesh body), so that
    /// "take from file" can apply it: the base stores only the body hash.
    pub fn conflict_theirs(
        &mut self,
        db: &Connection,
        guid: &str,
        field: &str,
        theirs: &J,
    ) -> Result<(), Error> {
        db.prepare_cached(
            "UPDATE src_conflict SET theirs=?3 WHERE source=?1 AND guid=?2 AND field=?4",
        )?
        .execute(params![self.source, guid, theirs.to_string(), field])?;
        Ok(())
    }

    /// Fields of an existing element: (field, mine, theirs) -> decisions in order. Records conflicts and
    /// the new base (theirs). The caller applies the fields with `Take`.
    pub fn row(
        &mut self,
        db: &Connection,
        snaps: &mut Snapshots,
        guid: &str,
        kind: &str,
        id: i64,
        fields: &[(&str, J, J)],
    ) -> Result<Vec<Decision>, Error> {
        self.row_in(db, snaps, guid, kind, id, fields, false)
    }

    /// [`Self::row`] for the generic properties (`p:` fields) of a row made here from the
    /// imported element in another table (mesh converted to a wall): the base of a property
    /// is taken whatever table the element had at the last import.
    pub fn props_row(
        &mut self,
        db: &Connection,
        snaps: &mut Snapshots,
        guid: &str,
        kind: &str,
        id: i64,
        fields: &[(&str, J, J)],
    ) -> Result<Vec<Decision>, Error> {
        self.row_in(db, snaps, guid, kind, id, fields, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn row_in(
        &mut self,
        db: &Connection,
        snaps: &mut Snapshots,
        guid: &str,
        kind: &str,
        id: i64,
        fields: &[(&str, J, J)],
        any_kind: bool,
    ) -> Result<Vec<Decision>, Error> {
        self.begin(db, snaps)?;
        if self.added.contains(guid) {
            for (f, _, theirs) in fields {
                self.put(guid, kind, id, Some(true), f, theirs.clone(), false);
            }
            return Ok(vec![Decision::Take; fields.len()]);
        }
        if crate::groups::is_pinned_in(db, kind, id)? {
            // the base moves forward: after unpinning, the divergence reads as a local edit
            for (f, _, theirs) in fields {
                self.put(guid, kind, id, Some(true), f, theirs.clone(), true);
            }
            self.note_pinned(kind, id);
            return Ok(vec![Decision::Keep; fields.len()]);
        }
        let old = if any_kind {
            self.old(db, guid)?
        } else {
            self.old_of(db, guid, kind)?
        };
        let mut out = Vec::with_capacity(fields.len());
        let mut took = false;
        for (f, mine, theirs) in fields {
            // a parameter absent from the previous base: added on both sides -> base is Null
            let base = match &old {
                None => None,
                Some(o) => match o.get(f) {
                    Some(b) => Some(b),
                    None if f.starts_with("p:") => Some(Bv::Val(&NULL)),
                    None => None,
                },
            };
            // the name in the source is compared only with the previous name in the source
            let d = if f.starts_with('~') {
                Decision::Same
            } else {
                decide_bv(base, mine, theirs)
            };
            match d {
                Decision::Take => {
                    self.stats.taken += 1;
                    took = true;
                }
                Decision::Keep => self.stats.kept_local += 1,
                Decision::Conflict => {
                    let shown = base.map_or(J::Null, |b| b.shown());
                    self.conflict(
                        db,
                        snaps,
                        guid,
                        kind,
                        id,
                        f,
                        ConflictCase::EditEdit,
                        &shown,
                        mine,
                        theirs,
                    )?
                }
                Decision::Same => {}
            }
            out.push(d);
            let keep_value = matches!(d, Decision::Keep | Decision::Conflict);
            self.put(guid, kind, id, Some(true), f, theirs.clone(), keep_value);
        }
        if took {
            self.merged.insert(guid.to_string());
            self.stats.merged = self.merged.len();
        }
        Ok(out)
    }

    /// The previous name in the source (`~name`) if the element was imported before.
    pub fn source_name(&mut self, db: &Connection, guid: &str) -> Result<Option<J>, Error> {
        self.base_value(db, guid, "~name")
    }

    /// The previous base field value (None for a parameter stored as a hash).
    pub fn base_value(
        &mut self,
        db: &Connection,
        guid: &str,
        field: &str,
    ) -> Result<Option<J>, Error> {
        Ok(self.old(db, guid)?.and_then(|o| match o.get(field) {
            Some(Bv::Val(v)) => Some(v.clone()),
            _ => None,
        }))
    }

    /// One parameter (`key` without `p:`) of an existing element.
    #[allow(clippy::too_many_arguments)]
    pub fn param(
        &mut self,
        db: &Connection,
        snaps: &mut Snapshots,
        guid: &str,
        kind: &str,
        id: i64,
        key: &str,
        mine: J,
        theirs: J,
    ) -> Result<Decision, Error> {
        let f = format!("p:{key}");
        let d = self.row(db, snaps, guid, kind, id, &[(f.as_str(), mine, theirs)])?;
        Ok(d[0])
    }

    /// A new element (inserted by the import): base = theirs.
    pub fn added(&mut self, guid: &str, kind: &str, id: i64, fields: &[(&str, J)]) {
        self.stats.added += 1;
        self.added.insert(guid.to_string());
        self.old.insert(guid.to_string(), None);
        for (f, v) in fields {
            self.put(guid, kind, id, Some(true), f, v.clone(), false);
        }
    }

    /// A base field without comparison (a new element, a parameter of an element deleted here).
    pub fn base_only(&mut self, guid: &str, kind: &str, id: i64, field: &str, theirs: J) {
        self.put(guid, kind, id, None, field, theirs, false);
    }

    /// A reference exists, the row does not (deleted here). Their values are compared with the base; the conflict
    /// lists the fields changed there.
    pub fn dead(
        &mut self,
        db: &Connection,
        snaps: &mut Snapshots,
        guid: &str,
        kind: &str,
        id: i64,
        fields: &[(&str, J)],
    ) -> Result<Dead, Error> {
        self.begin(db, snaps)?;
        let Some(old) = self.old_of(db, guid, kind)? else {
            return Ok(Dead::Recreate);
        };
        let changed: Vec<&str> = fields
            .iter()
            .filter(|(f, t)| !f.starts_with('~') && !old.get(f).is_some_and(|b| b.same(t)))
            .map(|(f, _)| *f)
            .collect();
        if changed.is_empty() {
            self.stats.kept_deleted += 1;
        } else {
            let what = J::String(changed.join(", "));
            self.conflict(
                db,
                snaps,
                guid,
                kind,
                id,
                "*",
                ConflictCase::DeleteEdit,
                &J::Null,
                &J::Null,
                &what,
            )?;
        }
        for (f, v) in fields {
            self.put(guid, kind, id, Some(false), f, v.clone(), false);
        }
        Ok(Dead::Stay)
    }

    /// The element changed table (mesh -> column): the previous base is not compared with the new row.
    pub fn retire(&mut self, guid: &str) {
        self.next.remove(guid);
    }

    fn note_pinned(&mut self, kind: &str, id: i64) {
        let it = (kind.to_string(), id);
        if !self.stats.pinned_ids.contains(&it) {
            self.stats.pinned_ids.push(it);
            self.stats.pinned += 1;
        }
    }

    /// Deleting the element would hit a pin: it stays, the caller skips the replacement.
    pub fn blocked_by_pin(&mut self, db: &Connection, kind: &str, id: i64) -> Result<bool, Error> {
        let blocked = crate::groups::pin_blocks_delete(db, kind, id)?;
        if blocked {
            self.note_pinned(kind, id);
        }
        Ok(blocked)
    }

    /// The same for `replace_kind`: the previous row of another kind is pinned.
    pub fn replace_blocked(
        &mut self,
        db: &Connection,
        guid: &str,
        kind: &str,
    ) -> Result<bool, Error> {
        let link: Option<(String, i64)> = db
            .prepare_cached("SELECT kind, ref FROM ext_ref WHERE source=?1 AND guid=?2")?
            .query_row(params![self.source, guid], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let Some((old_kind, id)) = link else {
            return Ok(false);
        };
        if old_kind == kind || !super::DELETABLE.contains(&old_kind.as_str()) {
            return Ok(false);
        }
        self.blocked_by_pin(db, &old_kind, id)
    }

    /// The guid came as another kind (mesh -> column): the previous row is deleted along with the reference
    /// and conflicts, the base is updated to the new kind, no duplicate. Some(true) means the previous
    /// row carried local work (the caller warns); None means no replacement happened.
    pub fn replace_kind(
        &mut self,
        db: &Connection,
        snaps: &mut Snapshots,
        guid: &str,
        kind: &str,
    ) -> Result<Option<bool>, Error> {
        let link: Option<(i64, String, i64)> = db
            .prepare_cached("SELECT rowid, kind, ref FROM ext_ref WHERE source=?1 AND guid=?2")?
            .query_row(params![self.source, guid], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        let Some((rowid, old_kind, id)) = link else {
            return Ok(None);
        };
        if old_kind == kind || !super::DELETABLE.contains(&old_kind.as_str()) {
            return Ok(None);
        }
        let n: i64 = db.query_row(
            &format!("SELECT count(*) FROM {old_kind} WHERE id=?1"),
            [id],
            |r| r.get(0),
        )?;
        let mut local = false;
        if n > 0 {
            local = local_or_edited(db, &self.source, &old_kind, id)?
                || hosts_local_work(db, &self.source, &old_kind, id, &|_| false)?;
            super::resolve::drop_element(db, snaps, &old_kind, id)?;
        }
        snaps.save(db, "ext_ref", "rowid", rowid)?;
        db.execute("DELETE FROM ext_ref WHERE rowid=?1", [rowid])?;
        let cs: Vec<i64> = db
            .prepare_cached("SELECT id FROM src_conflict WHERE source=?1 AND guid=?2")?
            .query_map(params![self.source, guid], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for c in cs {
            snaps.save(db, "src_conflict", "id", c)?;
            db.execute("DELETE FROM src_conflict WHERE id=?1", [c])?;
        }
        self.retire(guid);
        Ok(Some(local))
    }

    /// The element's GlobalId in the source.
    pub fn ifc_guid(&mut self, guid: &str, ifc: &str) {
        self.ifc.insert(guid.to_string(), ifc.to_string());
    }

    /// Source parameter definitions (they replace previous ones with the same key and scope).
    pub fn param_defs(
        &mut self,
        db: &Connection,
        snaps: &mut Snapshots,
        defs: &[ParamDef],
    ) -> Result<(), Error> {
        self.begin(db, snaps)?;
        for d in defs {
            let scope = if d.type_param { "type" } else { "instance" };
            let prev: Option<i64> = db
                .prepare_cached("SELECT id FROM src_param WHERE source=?1 AND key=?2 AND scope=?3")?
                .query_row(params![self.source, d.key, scope], |r| r.get(0))
                .optional()?;
            if let Some(p) = prev {
                snaps.save(db, "src_param", "id", p)?;
                db.execute(
                    "UPDATE src_param SET grp=?2, shared=?3, shared_guid=?4, spec=?5 WHERE id=?1",
                    params![
                        p,
                        d.group,
                        d.shared.map(|s| s as i64),
                        d.shared_guid,
                        d.spec
                    ],
                )?;
            } else {
                db.prepare_cached(
                    "INSERT INTO src_param(source,key,scope,grp,shared,shared_guid,spec) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                )?
                .execute(params![
                    self.source,
                    d.key,
                    scope,
                    d.group,
                    d.shared.map(|s| s as i64),
                    d.shared_guid,
                    d.spec
                ])?;
            }
        }
        Ok(())
    }

    /// Source views: the previous list is replaced.
    pub fn views(
        &mut self,
        db: &Connection,
        snaps: &mut Snapshots,
        views: &[SourceView],
    ) -> Result<(), Error> {
        self.begin(db, snaps)?;
        let ids: Vec<i64> = db
            .prepare("SELECT id FROM src_view WHERE source=?1")?
            .query_map([&self.source], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for id in ids {
            snaps.save(db, "src_view", "id", id)?;
        }
        db.execute("DELETE FROM src_view WHERE source=?1", [&self.source])?;
        let mut seen = HashSet::new();
        for v in views {
            if !seen.insert(v.guid.clone()) {
                continue;
            }
            db.prepare_cached(
                "INSERT INTO src_view(source,guid,name,kind,level) VALUES (?1,?2,?3,?4,?5)",
            )?
            .execute(params![self.source, v.guid, v.name, v.kind, v.level])?;
        }
        Ok(())
    }

    /// End of the import: the recognition check, parameters and elements that vanished from the source, the new
    /// base, stale conflicts, the `src_file` row. `seen` is the guids of all elements of the new file.
    /// Returns the guids of deleted elements.
    pub fn finish(
        &mut self,
        db: &Connection,
        snaps: &mut Snapshots,
        seen: &HashSet<String>,
        src: &SourceWrite,
    ) -> Result<Vec<String>, Error> {
        self.begin(db, snaps)?;
        self.guard(db, seen)?;
        self.removed_params(db, snaps)?;
        let deleted = self.removed_elements(db, snaps, seen)?;
        self.write_bases(db, snaps)?;
        self.drop_stale_conflicts(db, snaps, seen)?;
        self.write_source(db, snaps, src)?;
        Ok(deleted)
    }

    /// The file does not recognise more than half of the previous elements: refusal (the whole operation rolls back).
    fn guard(&self, db: &Connection, seen: &HashSet<String>) -> Result<(), Error> {
        if self.policy.allow_mass_change {
            return Ok(());
        }
        let mut st = db.prepare_cached("SELECT guid FROM src_base WHERE source=?1")?;
        let (mut total, mut missing) = (0usize, 0usize);
        for g in st.query_map([&self.source], |r| r.get::<_, String>(0))? {
            let g = g?;
            total += 1;
            if !seen.contains(&g) && !self.next.contains_key(&g) {
                missing += 1;
            }
        }
        if total >= GUARD_MIN && missing * 2 > total {
            return Err(Error::Invalid(format!(
                "{MASS_CHANGE}: {missing} of {total} elements imported before are not in this file \
                 (another project, or its element ids changed); nothing was changed"
            )));
        }
        Ok(())
    }

    fn removed_params(&mut self, db: &Connection, snaps: &mut Snapshots) -> Result<(), Error> {
        let guids: Vec<String> = self
            .next
            .iter()
            .filter(|(g, n)| n.alive && !self.added.contains(*g))
            .map(|(g, _)| g.clone())
            .collect();
        for g in guids {
            let (kind, id) = {
                let n = &self.next[&g];
                (n.kind.clone(), n.id)
            };
            let Some(old) = self.old_of(db, &g, &kind)? else {
                continue;
            };
            let gone: Vec<String> = {
                let n = &self.next[&g];
                let have: HashSet<&str> = n
                    .vals
                    .keys()
                    .map(String::as_str)
                    .chain(
                        n.hashed
                            .iter()
                            .map(|(k, _)| self.names[*k as usize].as_str()),
                    )
                    .collect();
                old.vals
                    .keys()
                    .chain(old.keys.iter())
                    .filter(|k| k.starts_with("p:") && !have.contains(k.as_str()))
                    .cloned()
                    .collect()
            };
            if gone.is_empty() {
                continue;
            }
            let props = props_of(db, &kind, id)?;
            for f in gone {
                let key = &f[2..];
                let mine = props.get(key).map_or(J::Null, |p| p.0.clone());
                if mine.is_null() {
                    continue;
                }
                let Some(b) = old.get(&f) else { continue };
                if b.same(&mine) {
                    snaps.save(db, "prop", "ref", id)?;
                    db.execute(
                        "DELETE FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
                        params![kind, id, key],
                    )?;
                    self.stats.params_removed += 1;
                } else {
                    let shown = b.shown();
                    self.conflict(
                        db,
                        snaps,
                        &g,
                        &kind,
                        id,
                        &f,
                        ConflictCase::EditDelete,
                        &shown,
                        &mine,
                        &J::Null,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Railings of a removed stair that are still in the new file stay: they become sketch
    /// railings at their absolute position.
    fn detach_present_railings(
        &self,
        db: &Connection,
        snaps: &mut Snapshots,
        stair: i64,
        seen: &HashSet<String>,
    ) -> Result<(), Error> {
        let rails: Vec<i64> = db
            .prepare_cached("SELECT id FROM railing WHERE stair=?1")?
            .query_map([stair], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for r in rails {
            let g: Option<String> = db
                .prepare_cached("SELECT guid FROM ext_ref WHERE source=?1 AND kind='railing' AND ref=?2")?
                .query_row(params![self.source, r], |x| x.get(0))
                .optional()?;
            if g.is_some_and(|g| seen.contains(&g) || self.next.contains_key(&g)) {
                snaps.save(db, "railing", "id", r)?;
                snaps.save(db, "railing_vertex", "railing", r)?;
                crate::stairs::detach_railing(db, r)?;
            }
        }
        Ok(())
    }

    fn removed_elements(
        &mut self,
        db: &Connection,
        snaps: &mut Snapshots,
        seen: &HashSet<String>,
    ) -> Result<Vec<String>, Error> {
        let rows: Vec<(i64, String)> = db
            .prepare("SELECT id, guid FROM src_base WHERE source=?1 ORDER BY id")?
            .query_map([&self.source], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut gone = vec![];
        let mut annotated: Option<HashSet<i64>> = None;
        for (row, guid) in rows {
            if seen.contains(&guid) || self.next.contains_key(&guid) {
                continue;
            }
            let Some(base) = load_base(db, &mut self.shapes, &self.source, &guid)? else {
                continue;
            };
            let link: Option<(String, i64)> = db
                .prepare_cached("SELECT kind, ref FROM ext_ref WHERE source=?1 AND guid=?2")?
                .query_row(params![self.source, guid], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            let live = match &link {
                Some((k, id)) if *k == base.kind => current_fields(db, k, *id, &base.names())?
                    .map(|now| (k.clone(), *id, Some(now))),
                Some((k, id)) => current_fields(db, k, *id, &Vec::<String>::new())?
                    .map(|_| (k.clone(), *id, None)),
                None => None,
            };
            let Some((k, id, now)) = live else {
                // deleted both here and there: forget it
                self.forget(db, snaps, &guid, row)?;
                continue;
            };
            if !self.policy.delete_removed || !DELETABLE.contains(&k.as_str()) {
                continue;
            }
            // the base stays: a removed pin will cause deletion on the next import
            if self.blocked_by_pin(db, &k, id)? {
                continue;
            }
            let edited = match &now {
                Some(now) => {
                    !diff(&base, now).is_empty() || added_props(db, &base, &k, id)?.next().is_some()
                }
                None => false,
            };
            if k == "wall" && annotated.is_none() {
                annotated = Some(annotated_walls(db)?);
            }
            let noted = k == "wall" && annotated.as_ref().is_some_and(|a| a.contains(&id));
            let in_file = |g: &str| seen.contains(g) || self.next.contains_key(g);
            let local = edited || noted || hosts_local_work(db, &self.source, &k, id, &in_file)?;
            if local {
                let pending: i64 = db.query_row(
                    "SELECT count(*) FROM src_conflict WHERE source=?1 AND guid=?2 AND field='*'",
                    params![self.source, guid],
                    |r| r.get(0),
                )?;
                if pending == 0 {
                    self.conflict(
                        db,
                        snaps,
                        &guid,
                        &k,
                        id,
                        "*",
                        ConflictCase::EditDelete,
                        &J::Null,
                        &J::Null,
                        &J::Null,
                    )?;
                }
            } else {
                gone.push((guid, k, id, row));
            }
        }
        let mut out = vec![];
        for (guid, k, id, row) in gone {
            // the cascade (wall -> its openings) may have already deleted the row
            if current_fields(db, &k, id, &Vec::<String>::new())?.is_some() {
                if k == "stair" {
                    self.detach_present_railings(db, snaps, id, seen)?;
                }
                super::resolve::drop_element(db, snaps, &k, id)?;
            }
            self.forget(db, snaps, &guid, row)?;
            self.stats.deleted_upstream += 1;
            out.push(guid);
        }
        Ok(out)
    }

    /// This import's batch: undo restores the previous base rows (or resets the source base if
    /// the batch has been displaced by newer ones). The journal entry is four commands, with no base copies.
    fn batch(&mut self, db: &Connection, snaps: &mut Snapshots) -> Result<i64, Error> {
        if let Some(b) = self.batch {
            return Ok(b);
        }
        db.execute(
            "DELETE FROM src_base_prev WHERE batch IN \
             (SELECT id FROM src_batch WHERE source=?1 ORDER BY id DESC LIMIT -1 OFFSET ?2)",
            params![self.source, KEEP_BATCHES - 1],
        )?;
        db.execute(
            "DELETE FROM src_batch WHERE source=?1 AND id NOT IN \
             (SELECT id FROM src_batch WHERE source=?1 ORDER BY id DESC LIMIT ?2)",
            params![self.source, KEEP_BATCHES - 1],
        )?;
        db.execute(
            "INSERT INTO src_batch(source, at) VALUES (?1, ?2)",
            params![self.source, now_unix()],
        )?;
        let b = db.last_insert_rowid();
        let (s, n) = (Sql::Text(self.source.clone()), Sql::Integer(b));
        snaps.push(
            Inv::default()
                .sql(
                    "DELETE FROM src_base WHERE source=?1 AND NOT EXISTS (SELECT 1 FROM src_batch WHERE id=?2)",
                    vec![s, n.clone()],
                )
                .sql(
                    "INSERT OR REPLACE INTO src_base(id,source,guid,kind,shape,hashes,vals,ifc_guid) \
                     SELECT id,source,guid,kind,shape,hashes,vals,ifc_guid FROM src_base_prev WHERE batch=?1",
                    vec![n.clone()],
                )
                .sql("DELETE FROM src_base_prev WHERE batch=?1", vec![n.clone()])
                .sql("DELETE FROM src_batch WHERE id=?1", vec![n]),
        );
        self.batch = Some(b);
        Ok(b)
    }

    /// The previous base row goes to `src_base_prev` (once per batch) before a change or deletion.
    fn keep_prev(&mut self, db: &Connection, snaps: &mut Snapshots, row: i64) -> Result<(), Error> {
        let b = self.batch(db, snaps)?;
        if self.prev.insert(row) {
            db.prepare_cached(
                "INSERT INTO src_base_prev(batch,id,source,guid,kind,shape,hashes,vals,ifc_guid) \
                 SELECT ?1,id,source,guid,kind,shape,hashes,vals,ifc_guid FROM src_base WHERE id=?2",
            )?
            .execute(params![b, row])?;
        }
        Ok(())
    }

    /// Shape (hash keys): a shared `src_shape` row; 0 means no hashes.
    fn shape_id(&mut self, db: &Connection, keys: &[String]) -> Result<i64, Error> {
        if keys.is_empty() {
            return Ok(0);
        }
        let text = J::Array(keys.iter().map(|k| J::String(k.clone())).collect()).to_string();
        if let Some(id) = self.shape_ids.get(&text) {
            return Ok(*id);
        }
        let h = (fnv(text.as_bytes()) >> 1) as i64;
        let found: Option<i64> = db
            .prepare_cached("SELECT id FROM src_shape WHERE hash=?1 AND keys=?2")?
            .query_row(params![h, text], |r| r.get(0))
            .optional()?;
        let id = match found {
            Some(id) => id,
            None => {
                db.prepare_cached("INSERT INTO src_shape(hash, keys) VALUES (?1, ?2)")?
                    .execute(params![h, text])?;
                db.last_insert_rowid()
            }
        };
        self.shapes.insert(id, Arc::new(keys.to_vec()));
        self.shape_ids.insert(text, id);
        Ok(id)
    }

    fn write_bases(&mut self, db: &Connection, snaps: &mut Snapshots) -> Result<(), Error> {
        let mut guids: Vec<String> = self.next.keys().cloned().collect();
        guids.sort();
        for g in guids {
            // an added row may replace a stale base (kind change, relinked guid): update that row
            let old = if self.added.contains(&g) {
                load_base(db, &mut self.shapes, &self.source, &g)?.map(Arc::new)
            } else {
                self.old(db, &g)?
            };
            let (kind, keys, hashes, vals_text, vals_same) = {
                let n = &self.next[&g];
                let mut h: Vec<(&str, u64)> = n
                    .hashed
                    .iter()
                    .map(|(k, v)| (self.names[*k as usize].as_str(), *v))
                    .filter(|(k, _)| !n.vals.contains_key(*k))
                    .collect();
                // stable sort: of repeated keys the last one remains
                h.sort_by(|a, b| a.0.cmp(b.0));
                let mut dd: Vec<(&str, u64)> = Vec::with_capacity(h.len());
                for x in h {
                    match dd.last_mut() {
                        Some(l) if l.0 == x.0 => *l = x,
                        _ => dd.push(x),
                    }
                }
                let keys: Vec<String> = dd.iter().map(|x| x.0.to_string()).collect();
                let hashes: Vec<u64> = dd.iter().map(|x| x.1).collect();
                // a link the file could not hold stays pending across a re-import
                let keep = old
                    .as_ref()
                    .and_then(|o| o.vals.get(PENDING))
                    .filter(|_| !n.vals.contains_key(PENDING));
                let (text, same) = match keep {
                    Some(p) => {
                        let mut v = n.vals.clone();
                        v.insert(PENDING.to_string(), p.clone());
                        let same = old.as_ref().is_some_and(|o| o.vals == v);
                        (fields_text(&v), same)
                    }
                    None => (
                        fields_text(&n.vals),
                        old.as_ref().is_some_and(|o| o.vals == n.vals),
                    ),
                };
                (n.kind.clone(), keys, hashes, text, same)
            };
            let shape = self.shape_id(db, &keys)?;
            let ifc = self
                .ifc
                .get(&g)
                .cloned()
                .or_else(|| old.as_ref().and_then(|o| o.ifc.clone()));
            let blob = hashes_blob(&hashes);
            match old {
                Some(o) => {
                    let same = vals_same
                        && o.kind == kind
                        && o.shape == shape
                        && o.hashes == hashes
                        && o.ifc == ifc;
                    if !same {
                        self.keep_prev(db, snaps, o.row)?;
                        db.prepare_cached(
                            "UPDATE src_base SET kind=?2, shape=?3, hashes=?4, vals=?5, ifc_guid=?6 WHERE id=?1",
                        )?
                        .execute(params![o.row, kind, shape, blob, vals_text, ifc])?;
                    }
                }
                None => {
                    db.prepare_cached(
                        "INSERT INTO src_base(source,guid,kind,shape,hashes,vals,ifc_guid) \
                         VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    )?
                    .execute(params![
                        self.source,
                        g,
                        kind,
                        shape,
                        blob,
                        vals_text,
                        ifc
                    ])?;
                }
            }
        }
        Ok(())
    }

    /// Whether `mine` matches the "theirs" field value from this import (None means the field did not arrive).
    fn next_same(&self, guid: &str, f: &str, mine: &J) -> Option<bool> {
        let n = self.next.get(guid)?;
        if let Some(t) = n.vals.get(f) {
            return Some(same_value(mine, t));
        }
        let k = *self.keys.get(f)?;
        n.hashed
            .iter()
            .rev()
            .find(|(x, _)| *x == k)
            .map(|(_, h)| *h == vhash(mine))
    }

    fn drop_stale_conflicts(
        &mut self,
        db: &Connection,
        snaps: &mut Snapshots,
        seen: &HashSet<String>,
    ) -> Result<(), Error> {
        let list: Vec<super::Conflict> = db
            .prepare(&format!(
                "SELECT {} FROM src_conflict WHERE source=?1",
                super::CONFLICT_COLS
            ))?
            .query_map([&self.source], super::conflict_row)?
            .collect::<Result<_, _>>()?;
        for c in list {
            let stale = match (c.case, c.field.as_str()) {
                (ConflictCase::EditDelete, "*") => seen.contains(&c.guid),
                (ConflictCase::DeleteEdit, "*") => {
                    current_fields(db, &c.kind, c.element, &Vec::<String>::new())?.is_some()
                }
                (_, f) => {
                    let key = vec![f.to_string()];
                    let mine = current_fields(db, &c.kind, c.element, &key)?
                        .and_then(|m| m.get(f).cloned());
                    match mine {
                        Some(m) => self.next_same(&c.guid, f, &m).unwrap_or(false),
                        // the element is deleted here, so a field conflict is meaningless
                        None => true,
                    }
                }
            };
            if stale {
                snaps.save(db, "src_conflict", "id", c.id)?;
                db.execute("DELETE FROM src_conflict WHERE id=?1", [c.id])?;
            }
        }
        Ok(())
    }

    fn write_source(
        &mut self,
        db: &Connection,
        snaps: &mut Snapshots,
        s: &SourceWrite,
    ) -> Result<(), Error> {
        let at = if s.at > 0 { s.at } else { now_unix() };
        let info = J::Object(
            s.info
                .iter()
                .map(|(k, v)| (k.clone(), J::String(v.clone())))
                .collect(),
        )
        .to_string();
        let mut rep: BTreeMap<String, J> = s
            .report
            .iter()
            .map(|(k, v)| (k.clone(), J::from(*v)))
            .collect();
        for (k, v) in self.stats.pairs() {
            rep.insert(k.to_string(), J::from(v as i64));
        }
        let rep = fields_text(&rep);
        let prev: Option<i64> = db
            .query_row(
                "SELECT id FROM src_file WHERE source=?1",
                [&self.source],
                |r| r.get(0),
            )
            .optional()?;
        match prev {
            Some(id) => {
                snaps.save(db, "src_file", "id", id)?;
                db.execute(
                    "UPDATE src_file SET file_key=?2, path=?3, format=?4, reader=?5, reader_version=?6, \
                     info=?7, imported_at=?8, imports=imports+1, report=?9 WHERE id=?1",
                    params![id, s.file_key, s.path, s.format, s.reader, s.reader_version, info, at, rep],
                )?;
            }
            None => {
                db.execute(
                    "INSERT INTO src_file(source,file_key,path,format,reader,reader_version,info,\
                     first_import,imported_at,imports,report) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8,1,?9)",
                    params![self.source, s.file_key, s.path, s.format, s.reader, s.reader_version, info, at, rep],
                )?;
            }
        }
        Ok(())
    }

    /// The reference, base and guid conflicts are deleted (the element vanished on both sides or was deleted there).
    fn forget(
        &mut self,
        db: &Connection,
        snaps: &mut Snapshots,
        guid: &str,
        base_row: i64,
    ) -> Result<(), Error> {
        self.keep_prev(db, snaps, base_row)?;
        db.execute("DELETE FROM src_base WHERE id=?1", [base_row])?;
        let cs: Vec<i64> = db
            .prepare_cached("SELECT id FROM src_conflict WHERE source=?1 AND guid=?2")?
            .query_map(params![self.source, guid], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for c in cs {
            snaps.save(db, "src_conflict", "id", c)?;
            db.execute("DELETE FROM src_conflict WHERE id=?1", [c])?;
        }
        let rowid: Option<i64> = db
            .query_row(
                "SELECT rowid FROM ext_ref WHERE source=?1 AND guid=?2",
                params![self.source, guid],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(r) = rowid {
            snaps.save(db, "ext_ref", "rowid", r)?;
            db.execute("DELETE FROM ext_ref WHERE rowid=?1", [r])?;
        }
        Ok(())
    }
}

/// Walls that edit constraints or dimensions rely on (local work).
fn annotated_walls(db: &Connection) -> Result<HashSet<i64>, Error> {
    let mut out: HashSet<i64> = crate::constraints::load(db)?
        .into_iter()
        .flat_map(|(_, s)| s.walls)
        .collect();
    let refs: Vec<String> = db
        .prepare("SELECT refs FROM dim")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut openings = HashSet::new();
    for t in refs {
        for r in t
            .split(';')
            .filter(|x| !x.is_empty())
            .filter_map(crate::annot::parse_ref)
        {
            use crate::annot::DimRef as D;
            match r {
                D::WallEnd { wall, .. }
                | D::AxisEnd { wall, .. }
                | D::AxisLine { wall }
                | D::ArcFace { wall, .. }
                | D::WallLevel { wall, .. }
                | D::GridAt { wall, .. } => {
                    out.insert(wall);
                }
                D::OpeningEdge { opening, .. } => {
                    openings.insert(opening);
                }
                D::Point(_) => {}
            }
        }
    }
    for o in openings {
        let w: Option<i64> = db
            .query_row("SELECT wall FROM opening WHERE id=?1", [o], |r| r.get(0))
            .optional()?;
        out.extend(w);
    }
    Ok(out)
}

/// A row with no reference to this source or edited after import.
fn local_or_edited(db: &Connection, source: &str, kind: &str, id: i64) -> Result<bool, Error> {
    let g: Option<String> = db
        .prepare_cached("SELECT guid FROM ext_ref WHERE source=?1 AND kind=?2 AND ref=?3")?
        .query_row(params![source, kind, id], |r| r.get(0))
        .optional()?;
    let Some(g) = g else { return Ok(true) };
    let Some(base) = load_base(db, &mut Shapes::new(), source, &g)? else {
        return Ok(false);
    };
    let now = current_fields(db, kind, id, &base.names())?;
    Ok(now.is_some_and(|now| !diff(&base, &now).is_empty()))
}

/// A host carries local work: a component or opening placed on it with no reference to this
/// source, or an edited one.
/// A railing still in the file (`in_file` by guid) is no such work: it outlives its stair as a
/// sketch railing.
fn hosts_local_work(
    db: &Connection,
    source: &str,
    kind: &str,
    id: i64,
    in_file: &dyn Fn(&str) -> bool,
) -> Result<bool, Error> {
    if kind == "stair" {
        let rails: Vec<i64> = db
            .prepare_cached("SELECT id FROM railing WHERE stair=?1")?
            .query_map([id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for r in rails {
            let g: Option<String> = db
                .prepare_cached(
                    "SELECT guid FROM ext_ref WHERE source=?1 AND kind='railing' AND ref=?2",
                )?
                .query_row(params![source, r], |x| x.get(0))
                .optional()?;
            if g.is_some_and(|g| in_file(&g)) {
                continue;
            }
            if local_or_edited(db, source, "railing", r)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    let col = match kind {
        "wall" => "host_wall",
        "slab" => "host_slab",
        "ceiling" => "host_ceiling",
        _ => return Ok(false),
    };
    let comps: Vec<i64> = db
        .prepare_cached(&format!("SELECT id FROM comp WHERE {col}=?1"))?
        .query_map([id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for c in comps {
        if local_or_edited(db, source, "comp", c)? {
            return Ok(true);
        }
    }
    if kind != "wall" {
        return Ok(false);
    }
    let ops: Vec<i64> = db
        .prepare_cached("SELECT id FROM opening WHERE wall=?1")?
        .query_map([id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for o in ops {
        if local_or_edited(db, source, "opening", o)? {
            return Ok(true);
        }
    }
    Ok(false)
}
