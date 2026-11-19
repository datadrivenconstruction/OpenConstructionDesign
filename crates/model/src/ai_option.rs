//! AI design options stored as recipes (`ai_option`), never as model copies. A recipe is
//! `(base_head, envelope, seed, params)`: replaying it on the same base gives the same rows, so
//! scratch copies are a cache. The table is outside undo like `ai_audit`: writing a recipe never
//! touches element tables of the main model; only `ai_option_apply` (the user's Apply) does.
//! Accepted is derived, never stored: Apply records its journal record `(seq, stamp)` in the
//! same transaction, and the option reads as accepted only while that record is in the journal,
//! so undo of the Apply returns it to draft. Merged is derived the same way: each applied merge
//! card writes an `ai_option_merge(option_id, seq, stamp, note)` row in its own seal.
//!
//! Materialisation (on scratch copies only):
//! - add-only option: its elements become one alternative of an `options.rs` set in one shared
//!   copy (`alt.db`) holding all add-only options of the call;
//! - option that changes or deletes shared elements: its own branch copy `opt-<id>.db`.
//!
//! The envelope executor is passed in as [`Lower`]: it must be a pure function of the model
//! content, the envelope, the seed and the params (no clock, no global random state).
//! The table is created at open and by schema step 41 (`migrate`): additive and order-independent.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use web_time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::ai_plan::{ModelDiff, RowKind};
use crate::{ElKey, Error, Model};

const DDL: &str = "
CREATE TABLE IF NOT EXISTS ai_option(id INTEGER PRIMARY KEY, set_name TEXT NOT NULL,
    name TEXT NOT NULL, base_head INTEGER NOT NULL, base_stamp INTEGER NOT NULL DEFAULT 0,
    envelope TEXT NOT NULL, seed INTEGER NOT NULL DEFAULT 0, params TEXT NOT NULL DEFAULT '{}',
    generator TEXT, metrics TEXT, note TEXT, why TEXT, status TEXT NOT NULL DEFAULT 'draft',
    ts INTEGER NOT NULL DEFAULT 0, accepted_seq INTEGER, accepted_stamp INTEGER);
CREATE INDEX IF NOT EXISTS ai_option_set_ix ON ai_option(set_name);
CREATE TABLE IF NOT EXISTS ai_option_merge(option_id INTEGER NOT NULL, seq INTEGER NOT NULL,
    stamp INTEGER NOT NULL, note TEXT);
CREATE INDEX IF NOT EXISTS ai_option_merge_ix ON ai_option_merge(option_id);
";

/// Rows compared per option; a larger diff is still classified, only reported truncated.
const DIFF_LIMIT: usize = 200_000;
const ALT_FILE: &str = "alt.db";

pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    for col in ["accepted_seq", "accepted_stamp"] {
        let has: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info('ai_option') WHERE name=?1",
            [col],
            |r| r.get(0),
        )?;
        if has == 0 {
            db.execute_batch(&format!("ALTER TABLE ai_option ADD COLUMN {col} INTEGER;"))?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionStatus {
    Draft,
    Accepted,
    /// Merged, whole or by parts, through a plan card the person applied.
    Merged,
    Rejected,
}

impl OptionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Accepted => "accepted",
            Self::Merged => "merged",
            Self::Rejected => "rejected",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "accepted" => Self::Accepted,
            "merged" => Self::Merged,
            "rejected" => Self::Rejected,
            _ => Self::Draft,
        }
    }
}

/// How an option was materialised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionMode {
    /// Only adds elements: one alternative in the shared `alt.db` copy.
    Alternative,
    /// Changes or deletes shared elements: own branch copy.
    Branch,
}

/// One option recipe. `base_head`/`base_stamp` name the top journal record of the base model.
#[derive(Clone, Debug, PartialEq)]
pub struct OptionRec {
    pub id: i64,
    pub set: String,
    pub name: String,
    pub base_head: i64,
    pub base_stamp: i64,
    pub envelope: String,
    pub seed: u64,
    pub params: String,
    pub generator: Option<String>,
    pub metrics: Option<String>,
    pub note: Option<String>,
    pub why: Option<String>,
    pub status: OptionStatus,
}

/// What the executor receives for one replay.
#[derive(Clone, Copy, Debug)]
pub struct Replay<'a> {
    pub envelope: &'a str,
    pub seed: u64,
    pub params: &'a str,
}

/// Envelope executor: lowers the envelope into model calls on the given model.
pub type Lower<'a> = dyn FnMut(&mut Model, &Replay<'_>) -> Result<(), Error> + 'a;

/// A materialised option. `diff` is the option alone against the base, in both modes.
#[derive(Clone, Debug)]
pub struct OptionCopy {
    pub option: i64,
    pub mode: OptionMode,
    pub path: PathBuf,
    /// `design_option` id inside `alt.db` (alternative mode only).
    pub alternative: Option<i64>,
    pub diff: ModelDiff,
}

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn keep_going(_: u64, _: u64) -> bool {
    true
}

// a stored 'accepted' or 'merged' (older builds) without a live journal record reads as draft;
// the note of the latest live merge wins over the stored one, so undo takes it back too
const COLS: &str = "id, set_name, name, base_head, base_stamp, envelope, seed, params, generator, \
    metrics, coalesce((SELECT g.note FROM ai_option_merge g JOIN op ON op.seq=g.seq \
    AND coalesce(op.stamp, 0)=g.stamp WHERE g.option_id=ai_option.id AND g.note IS NOT NULL \
    ORDER BY g.seq DESC LIMIT 1), note), why, CASE WHEN status='rejected' THEN 'rejected' \
    WHEN accepted_seq IS NOT NULL AND EXISTS(SELECT 1 FROM op WHERE op.seq=ai_option.accepted_seq \
    AND coalesce(op.stamp, 0)=ai_option.accepted_stamp) THEN 'accepted' \
    WHEN EXISTS(SELECT 1 FROM ai_option_merge g JOIN op ON op.seq=g.seq \
    AND coalesce(op.stamp, 0)=g.stamp WHERE g.option_id=ai_option.id) THEN 'merged' \
    ELSE 'draft' END";
// a recipe store without a journal (session `recipes.db`): a recorded Apply reads as accepted
const STORE_COLS: &str = "id, set_name, name, base_head, base_stamp, envelope, seed, params, generator, \
    metrics, coalesce((SELECT g.note FROM ai_option_merge g WHERE g.option_id=ai_option.id \
    AND g.note IS NOT NULL ORDER BY g.seq DESC LIMIT 1), note), why, \
    CASE WHEN status='rejected' THEN 'rejected' \
    WHEN accepted_seq IS NOT NULL THEN 'accepted' \
    WHEN EXISTS(SELECT 1 FROM ai_option_merge g WHERE g.option_id=ai_option.id) THEN 'merged' \
    ELSE 'draft' END";

fn cols(db: &Connection) -> Result<&'static str, Error> {
    let has_op: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='op'",
        [],
        |r| r.get(0),
    )?;
    Ok(if has_op > 0 { COLS } else { STORE_COLS })
}

fn read_rec(r: &Row<'_>) -> rusqlite::Result<OptionRec> {
    Ok(OptionRec {
        id: r.get(0)?,
        set: r.get(1)?,
        name: r.get(2)?,
        base_head: r.get(3)?,
        base_stamp: r.get(4)?,
        envelope: r.get(5)?,
        seed: r.get::<_, i64>(6)? as u64,
        params: r.get(7)?,
        generator: r.get(8)?,
        metrics: r.get(9)?,
        note: r.get(10)?,
        why: r.get(11)?,
        status: OptionStatus::parse(&r.get::<_, String>(12)?),
    })
}

fn insert_rec(db: &Connection, rec: &OptionRec) -> Result<i64, Error> {
    db.execute(
        "INSERT INTO ai_option(set_name, name, base_head, base_stamp, envelope, seed, params,          generator, metrics, note, why, status, ts)          VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'draft',?12)",
        params![
            rec.set,
            rec.name,
            rec.base_head,
            rec.base_stamp,
            rec.envelope,
            rec.seed as i64,
            rec.params,
            rec.generator,
            rec.metrics,
            rec.note,
            rec.why,
            now_s()
        ],
    )?;
    Ok(db.last_insert_rowid())
}

fn get_rec(db: &Connection, id: i64) -> Result<Option<OptionRec>, Error> {
    Ok(db
        .query_row(
            &format!("SELECT {} FROM ai_option WHERE id=?1", cols(db)?),
            [id],
            read_rec,
        )
        .optional()?)
}

fn list_recs(db: &Connection, set: Option<&str>) -> Result<Vec<OptionRec>, Error> {
    let mut st = db.prepare(&format!(
        "SELECT {} FROM ai_option WHERE ?1 IS NULL OR set_name=?1 ORDER BY id",
        cols(db)?
    ))?;
    let v = st
        .query_map([set], read_rec)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

fn delete_rec(db: &Connection, id: i64) -> Result<bool, Error> {
    // a reused id must not inherit merge marks of the removed row
    db.execute("DELETE FROM ai_option_merge WHERE option_id=?1", [id])?;
    Ok(db.execute("DELETE FROM ai_option WHERE id=?1", [id])? > 0)
}

/// Marks a draft rejected; the row stays. Refused for a missing, accepted, merged or rejected
/// option (`status='merged'` is a stored mark of older builds that now reads as draft).
fn reject_rec(db: &Connection, id: i64) -> Result<(), Error> {
    if get_rec(db, id)?.map(|r| r.status) != Some(OptionStatus::Draft) {
        return Err(Error::Invalid(format!("no draft option {id}")));
    }
    let n = db.execute(
        "UPDATE ai_option SET status='rejected' WHERE id=?1 AND status IN ('draft','merged')",
        [id],
    )?;
    if n == 0 {
        return Err(Error::Invalid(format!("no draft option {id}")));
    }
    Ok(())
}

/// `(rows, rejected rows)`: with the journal head, tells a view when to reload its rows.
fn rev_rec(db: &Connection) -> Result<(i64, i64), Error> {
    Ok(db.query_row(
        "SELECT count(*), coalesce(sum(status='rejected'), 0) FROM ai_option",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// Where recipes live: the open model or an [`OptionStore`]. Discard and listing go through
/// this, so the tool and the Options tab leave one state.
pub trait RecipeStore {
    fn add_recipe(&self, rec: &OptionRec) -> Result<i64, Error>;
    fn get_recipe(&self, id: i64) -> Result<Option<OptionRec>, Error>;
    fn reject_recipe(&self, id: i64) -> Result<(), Error>;
}

impl RecipeStore for Model {
    fn add_recipe(&self, rec: &OptionRec) -> Result<i64, Error> {
        self.ai_option_add(rec)
    }
    fn get_recipe(&self, id: i64) -> Result<Option<OptionRec>, Error> {
        get_rec(&self.db, id)
    }
    fn reject_recipe(&self, id: i64) -> Result<(), Error> {
        reject_rec(&self.db, id)
    }
}

/// The `ai_option` table of a project file, opened on its own light connection: no schema
/// upgrade, no change tracking, so a worker can store recipes in the file the editor has open.
/// Recipes are outside undo; this never touches element tables.
pub struct OptionStore {
    db: Connection,
}

impl OptionStore {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(DDL)?;
        Ok(Self { db })
    }

    pub fn get(&self, id: i64) -> Result<Option<OptionRec>, Error> {
        get_rec(&self.db, id)
    }

    pub fn list(&self, set: Option<&str>) -> Result<Vec<OptionRec>, Error> {
        list_recs(&self.db, set)
    }

    /// Removes the recipe row; the caller archives its branch copy.
    pub fn delete(&self, id: i64) -> Result<bool, Error> {
        delete_rec(&self.db, id)
    }

    /// `UPDATE status='rejected'` of a draft; the row stays.
    pub fn reject(&self, id: i64) -> Result<(), Error> {
        reject_rec(&self.db, id)
    }

    pub fn rev(&self) -> Result<(i64, i64), Error> {
        rev_rec(&self.db)
    }
}

impl RecipeStore for OptionStore {
    fn add_recipe(&self, rec: &OptionRec) -> Result<i64, Error> {
        insert_rec(&self.db, rec)
    }
    fn get_recipe(&self, id: i64) -> Result<Option<OptionRec>, Error> {
        get_rec(&self.db, id)
    }
    fn reject_recipe(&self, id: i64) -> Result<(), Error> {
        reject_rec(&self.db, id)
    }
}

fn remove_copy(path: &Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    crate::ai_plan::remove_sidecars(path)
}

impl Model {
    /// Top journal record `(seq, stamp)`: the head a recipe is based on. The stamp tells a new
    /// operation from an undone one that reused the same number.
    pub fn ai_option_head(&self) -> Result<(i64, i64), Error> {
        Ok(self
            .db
            .query_row(
                "SELECT seq, coalesce(stamp, 0) FROM op ORDER BY seq DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((0, 0)))
    }

    /// Stores a recipe (draft); `id` and `status` of `rec` are ignored. Not an undo step.
    pub fn ai_option_add(&self, rec: &OptionRec) -> Result<i64, Error> {
        insert_rec(&self.db, rec)
    }

    pub fn ai_option_get(&self, id: i64) -> Result<Option<OptionRec>, Error> {
        get_rec(&self.db, id)
    }

    /// Recipes of one set (all sets at `None`), by id.
    pub fn ai_option_list(&self, set: Option<&str>) -> Result<Vec<OptionRec>, Error> {
        list_recs(&self.db, set)
    }

    /// Removes a recipe row (any status). Its branch copy, if any, is the caller's to archive.
    pub fn ai_option_delete(&self, id: i64) -> Result<bool, Error> {
        delete_rec(&self.db, id)
    }

    /// Computed metrics (JSON) of a recipe; metrics are a cache, not part of the recipe.
    pub fn ai_option_set_metrics(&self, id: i64, metrics: &str) -> Result<(), Error> {
        let n = self
            .db
            .execute("UPDATE ai_option SET metrics=?2 WHERE id=?1", params![id, metrics])?;
        if n == 0 {
            return Err(Error::Invalid(format!("no option recipe {id}")));
        }
        Ok(())
    }

    pub fn ai_option_reject(&self, id: i64) -> Result<(), Error> {
        reject_rec(&self.db, id)
    }

    pub fn ai_option_rev(&self) -> Result<(i64, i64), Error> {
        rev_rec(&self.db)
    }

    /// A rejected option comes back as a draft: recipes are never dropped, so it can be compared
    /// and merged again.
    pub fn ai_option_revive(&self, id: i64) -> Result<(), Error> {
        let n = self.db.execute(
            "UPDATE ai_option SET status='draft' WHERE id=?1 AND status='rejected'",
            [id],
        )?;
        if n == 0 {
            return Err(Error::Invalid(format!("no rejected option {id}")));
        }
        Ok(())
    }

    /// [`Model::atomic`] for the Apply of a merge card of option `id`: the merge mark and the
    /// person's note why this one was chosen (`None` keeps the old note) go into the same seal,
    /// so the option reads as merged only while that entry is in the journal. Undo takes the
    /// mark back; redo re-runs this and writes it again. Part merges may follow each other.
    /// Status is checked when the card is made, not here: a redo must not fail on it.
    pub fn atomic_merge<T>(
        &mut self,
        kind: &str,
        id: i64,
        note: Option<&str>,
        f: impl FnOnce(&mut Model) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.atomic_in(kind, None, f, |db, (seq, stamp)| {
            db.execute(
                "INSERT INTO ai_option_merge(option_id, seq, stamp, note) VALUES (?1,?2,?3,?4)",
                params![id, seq, stamp, note],
            )?;
            Ok(())
        })
        .map(|(v, _)| v)
    }

    /// The user's Apply: replays the recipe on this model as one undo step. Refused when the
    /// model moved past the recipe's base (the option must be regenerated first) or while an
    /// earlier Apply of it is still in the journal.
    pub fn ai_option_apply(&mut self, id: i64, lower: &mut Lower<'_>) -> Result<(), Error> {
        let rec = self
            .ai_option_get(id)?
            .ok_or_else(|| Error::Invalid(format!("no option recipe {id}")))?;
        if rec.status != OptionStatus::Draft {
            return Err(Error::Invalid(format!(
                "option {id} is {}",
                rec.status.as_str()
            )));
        }
        if self.ai_option_head()? != (rec.base_head, rec.base_stamp) {
            return Err(Error::Invalid(format!(
                "option {id} is stale: the model changed after its base"
            )));
        }
        let rp = Replay {
            envelope: &rec.envelope,
            seed: rec.seed,
            params: &rec.params,
        };
        let prov = option_prov(&rec, id);
        self.atomic_in(
            "ai_option_apply",
            prov.as_ref(),
            |m| lower(m, &rp),
            |db, (seq, stamp)| accept_rec(db, id, (seq, stamp)),
        )
        .map(|(v, _)| v)
    }

    /// [`Model::ai_option_apply`] of a recipe not yet in this model (a session store's): its
    /// row is written inside the Apply's own transaction, so a refused Apply leaves no row.
    /// Returns the new row's id.
    pub fn ai_option_apply_new(
        &mut self,
        rec: &OptionRec,
        lower: &mut Lower<'_>,
    ) -> Result<i64, Error> {
        if rec.status != OptionStatus::Draft {
            return Err(Error::Invalid(format!("option is {}", rec.status.as_str())));
        }
        if self.ai_option_head()? != (rec.base_head, rec.base_stamp) {
            return Err(Error::Invalid("option is stale: the model changed after its base".into()));
        }
        let rp = Replay {
            envelope: &rec.envelope,
            seed: rec.seed,
            params: &rec.params,
        };
        let id = std::cell::Cell::new(0_i64);
        self.atomic_in(
            "ai_option_apply",
            None,
            |m| {
                id.set(insert_rec(&m.db, rec)?);
                lower(m, &rp)
            },
            |db, head| {
                // the id is known only now: the provenance goes on here, not through `atomic_in`
                if let Some(p) = option_prov(rec, id.get()) {
                    crate::journal::stamp_top(db, &p)?;
                }
                accept_rec(db, id.get(), head)
            },
        )?;
        Ok(id.get())
    }
}

/// A recipe in the op language also records who made it and why on the journal entry; the
/// actor is the AI that generated the option, so it lists as an AI batch.
fn option_prov(rec: &OptionRec, id: i64) -> Option<crate::journal::Provenance> {
    ddc_ops::Envelope::from_json(&rec.envelope).ok().map(|mut e| {
        e.actor = ddc_ops::Actor::Ai {
            session: format!("option:{id}"),
            model: rec.generator.clone().unwrap_or_default(),
        };
        if e.why.is_none() {
            e.why = rec.why.clone();
        }
        crate::journal::Provenance::of(&e)
    })
}

fn accept_rec(db: &Connection, id: i64, (seq, stamp): (i64, i64)) -> Result<(), Error> {
    db.execute(
        "UPDATE ai_option SET accepted_seq=?2, accepted_stamp=?3 WHERE id=?1",
        params![id, seq, stamp],
    )?;
    Ok(())
}

/// Replays one recipe on a fresh copy of `base` at `path`; returns the open copy and its diff.
fn replay_on_copy(
    base: &Path,
    path: &Path,
    rec: &OptionRec,
    lower: &mut Lower<'_>,
) -> Result<(Model, ModelDiff), Error> {
    remove_copy(path)?;
    Model::backup_file(base, path, &mut keep_going)?;
    let ps = path
        .to_str()
        .ok_or_else(|| Error::Invalid("non-UTF-8 path".into()))?;
    let mut m = Model::open_copy(ps)?;
    let head = m.change_head();
    let rp = Replay {
        envelope: &rec.envelope,
        seed: rec.seed,
        params: &rec.params,
    };
    m.atomic("ai_option", |m| lower(m, &rp))?;
    let diff = m.diff_from(base, &m.changes_since(head), DIFF_LIMIT)?;
    Ok((m, diff))
}

/// New elements of an add-only diff, or `None` when any row edits or deletes shared data.
pub(crate) fn added_elements(diff: &ModelDiff) -> Option<Vec<ElKey>> {
    let mut new: HashSet<ElKey> = HashSet::new();
    for r in diff.rows.iter().filter(|r| !r.secondary()) {
        if r.kind == RowKind::Created && r.owner.as_ref() == Some(&(r.table.clone(), r.rowid)) {
            new.insert((r.table.clone(), r.rowid));
        }
    }
    for r in diff.rows.iter().filter(|r| !r.secondary()) {
        let owned = r.owner.as_ref().is_some_and(|o| new.contains(o));
        if r.kind != RowKind::Created || !owned {
            return None;
        }
    }
    if new.is_empty() || new.iter().any(|(t, _)| matches!(t.as_str(), "level" | "grid")) {
        return None;
    }
    let mut v: Vec<ElKey> = new.into_iter().collect();
    v.sort();
    Some(v)
}

/// Materialises recipes on scratch copies of `base` (a snapshot of the model at the recipes'
/// head) inside `dir`. The main model is never opened here. Add-only options of one call share
/// `alt.db`, one design option set per recipe `set`.
pub fn materialize_options(
    base: &Path,
    recs: &[OptionRec],
    dir: &Path,
    lower: &mut Lower<'_>,
) -> Result<Vec<OptionCopy>, Error> {
    fs::create_dir_all(dir)?;
    let alt_path = dir.join(ALT_FILE);
    let mut alt: Option<Model> = None;
    let mut sets: Vec<(String, i64)> = Vec::new();
    let mut out = Vec::with_capacity(recs.len());
    for rec in recs {
        let path = dir.join(format!("opt-{}.db", rec.id));
        let (m, diff) = replay_on_copy(base, &path, rec, lower)?;
        let Some(added) = added_elements(&diff) else {
            drop(m);
            out.push(OptionCopy {
                option: rec.id,
                mode: OptionMode::Branch,
                path,
                alternative: None,
                diff,
            });
            continue;
        };
        drop(m);
        remove_copy(&path)?;
        if alt.is_none() {
            remove_copy(&alt_path)?;
            Model::backup_file(base, &alt_path, &mut keep_going)?;
            let ps = alt_path
                .to_str()
                .ok_or_else(|| Error::Invalid("non-UTF-8 path".into()))?;
            alt = Some(Model::open_copy(ps)?);
        }
        let a = alt.as_mut().expect("alt copy opened above");
        let head = a.change_head();
        let rp = Replay {
            envelope: &rec.envelope,
            seed: rec.seed,
            params: &rec.params,
        };
        a.atomic("ai_option", |m| lower(m, &rp))?;
        // ids in the shared copy may differ from the trial: take this replay's own new rows
        let ch = a.changes_since(head);
        let full = a.diff_from(base, &ch, DIFF_LIMIT)?;
        let mut items: Vec<ElKey> = full
            .rows
            .iter()
            .filter(|r| r.kind == RowKind::Created && ch.ids(&r.table).any(|i| i == r.rowid))
            .filter(|r| r.owner.as_ref() == Some(&(r.table.clone(), r.rowid)))
            .map(|r| (r.table.clone(), r.rowid))
            .collect();
        items.sort();
        items.dedup();
        if items.len() != added.len() {
            return Err(Error::Invalid(format!(
                "option {}: replay is not deterministic ({} vs {} new elements)",
                rec.id,
                items.len(),
                added.len()
            )));
        }
        let opt = match sets.iter().find(|(s, _)| *s == rec.set) {
            Some((_, set_id)) => a.add_option(*set_id, &rec.name)?,
            None => {
                let (set_id, opt) = a.add_option_set(&rec.set, &rec.name)?;
                sets.push((rec.set.clone(), set_id));
                opt
            }
        };
        a.add_to_option(opt, &items)?;
        out.push(OptionCopy {
            option: rec.id,
            mode: OptionMode::Alternative,
            path: alt_path.clone(),
            alternative: Some(opt),
            diff,
        });
    }
    Ok(out)
}

#[cfg(test)]
#[path = "ai_option_tests.rs"]
mod tests;
