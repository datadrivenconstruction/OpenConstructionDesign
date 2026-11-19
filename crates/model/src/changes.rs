//! Journal of changed rows for partial view rebuilds: a SQLite hook fires on every insert,
//! update and delete (operations, undo/redo, recomputation of derived tables). Consumers keep
//! their own cursor and read changes after it; a window overflow means "everything changed".
//!
//! Uses the update hook (preupdate needs building bindings with libclang). The authorizer disables table
//! truncation, so `DELETE` without WHERE runs row by row. Rows removed by a REPLACE conflict
//! on UNIQUE are invisible to the hook; inserting the new row still marks the table. Rows accumulate until the transaction commits and are discarded on its rollback; a rollback to a
//! savepoint (ROLLBACK TO) does not discard them: an extra rebuild, but never a miss. A table
//! with thousands of rows per transaction (quantity recomputation) is written as a single `WHOLE` entry.
//! Only the `main` database, without internal `sqlite_*` tables.

use rusqlite::hooks::{Action, AuthAction, AuthContext, Authorization};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Entries in the journal window; older ones make the consumer rebuild everything.
const WINDOW: usize = 1 << 18;
/// Rows of one table per transaction after which "whole table" is recorded.
const COALESCE: usize = 1 << 12;
/// Row number meaning "the whole table changed" (`Changes::whole`).
pub const WHOLE: i64 = i64::MIN;

#[derive(Debug, Default)]
pub(crate) struct Tracker {
    /// Number of the first entry of `log`.
    base: u64,
    log: Vec<(Arc<str>, i64)>,
    tables: HashMap<String, Arc<str>>,
    /// Rows of the uncommitted transaction.
    pending: Vec<(Arc<str>, i64)>,
    /// Transactions ended (committed or rolled back): names the one `pending` belongs to.
    txn: u64,
    /// Tables written / deleted from in the open transaction (for gating per-op passes).
    tx_tables: HashSet<Arc<str>>,
    tx_deleted: HashSet<Arc<str>>,
    /// `[start, end)` of the commits that wrote an undo step (an `op` row), oldest first.
    steps: Vec<(u64, u64)>,
    /// Numbers of another tracker (another session of the file) mean nothing here.
    id: i64,
    /// Committed transactions that wrote a content table without logging an undo step.
    free: HashMap<Arc<str>, u64>,
    /// Committed transactions that wrote a content table, undo steps and undos too.
    written: HashMap<Arc<str>, u64>,
    /// First number this tracker gave out: no other connection of the process shares it.
    origin: u64,
}

/// Placed drawings, written by the app outside the undo journal: their own change kind.
pub const UNDERLAY: &str = "underlay";

/// Tables `changes_since` leaves out: the undo log, settings, the assistant's records and the
/// placed drawings (tracked by the app).
fn not_content(t: &str) -> bool {
    matches!(t, "op" | "meta" | "ai_audit" | "ai_policy" | "ai_option" | "ai_chat" | UNDERLAY)
        || crate::calc_store::is_run_table(t)
        || t == "issue"
        || t.starts_with("issue_")
}

/// Where the model stands, compared with a saved state: the newest logged operation (its
/// sequence number and random stamp) plus the writes made outside operations. Equal states
/// mean undo/redo brought the model back to the same content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditState {
    top: Option<(i64, Option<i64>)>,
    free: u64,
}

/// Names one moment of the model's content for a result kept across frames and built on it.
/// Equal marks mean the same content: `writes` only grows (an undo of a content step writes
/// too, so it never brings an old mark back) and `origin` differs per connection, also for a
/// reopened file. An undoable step that wrote only `meta` moves just the newest step: its undo
/// brings the old mark back together with the old content and `meta`, and a redo logs a new
/// random stamp, so a mark seen again names the same state. Reloads and settings do not move it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentMark {
    origin: u64,
    writes: u64,
    top: Option<(i64, Option<i64>)>,
}

/// Tables the open transaction has touched so far; lets per-operation clean-up passes skip
/// tables the operation never wrote to.
#[derive(Clone, Debug, Default)]
pub struct Touch {
    any: HashSet<Arc<str>>,
    del: HashSet<Arc<str>>,
}

impl Touch {
    pub fn touched(&self, tables: &[&str]) -> bool {
        tables.iter().any(|t| self.any.contains(*t))
    }
    pub fn deleted(&self, tables: &[&str]) -> bool {
        tables.iter().any(|t| self.del.contains(*t))
    }
    /// Deleted from an element or data table (bookkeeping tables do not count).
    pub fn any_deleted(&self) -> bool {
        self.del
            .iter()
            .any(|t| !(matches!(&**t, "op" | "meta") || t.starts_with("ddc_") || t.starts_with("ai_")))
    }
}

impl Tracker {
    fn name(&mut self, table: &str) -> Arc<str> {
        match self.tables.get(table) {
            Some(t) => t.clone(),
            None => {
                let t: Arc<str> = table.into();
                self.tables.insert(table.to_string(), t.clone());
                t
            }
        }
    }
    fn note(&mut self, table: &str, rowid: i64, deleted: bool) {
        let t = self.name(table);
        self.tx_tables.insert(t.clone());
        if deleted {
            self.tx_deleted.insert(t.clone());
        }
        self.pending.push((t, rowid));
    }
    fn commit(&mut self) {
        self.txn += 1;
        let mut pending = std::mem::take(&mut self.pending);
        // the assistant chat and audit log are kept beside the model, not in it: writing them
        // (a turn, a key scrub of old rows) must not move the head
        pending.retain(|(t, _)| {
            !(matches!(&**t, "ai_chat" | "ai_audit") || crate::calc_store::is_run_table(t))
        });
        self.tx_tables.clear();
        self.tx_deleted.clear();
        let start = self.head();
        let step = pending.iter().any(|(t, _)| &**t == "op");
        let mut seen: HashSet<&Arc<str>> = HashSet::new();
        for (t, _) in &pending {
            if !not_content(t) && seen.insert(t) {
                *self.written.entry(t.clone()).or_default() += 1;
                if !step {
                    *self.free.entry(t.clone()).or_default() += 1;
                }
            }
        }
        let mut per: HashMap<Arc<str>, usize> = HashMap::new();
        if pending.len() > COALESCE {
            for (t, _) in &pending {
                *per.entry(t.clone()).or_default() += 1;
            }
        }
        let mut whole: HashSet<Arc<str>> = HashSet::new();
        for (t, rowid) in pending {
            if per.get(&t).is_some_and(|&n| n > COALESCE) {
                if whole.insert(t.clone()) {
                    self.push(t, WHOLE);
                }
            } else {
                self.push(t, rowid);
            }
        }
        if step && self.head() > start {
            self.steps.push((start, self.head()));
        }
    }
    fn rollback(&mut self) {
        self.txn += 1;
        self.pending.clear();
        self.tx_tables.clear();
        self.tx_deleted.clear();
    }
    fn push(&mut self, t: Arc<str>, rowid: i64) {
        if self.log.len() >= WINDOW {
            let drop = WINDOW / 2;
            self.log.drain(..drop);
            self.base += drop as u64;
            let base = self.base;
            self.steps.retain(|s| s.1 > base);
        }
        self.log.push((t, rowid));
    }
    fn head(&self) -> u64 {
        self.base + self.log.len() as u64
    }
}

/// Changes after number `since`: rows per table, or `all` (window lost).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    pub head: u64,
    pub all: bool,
    pub rows: HashMap<String, HashSet<i64>>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        !self.all && self.rows.is_empty()
    }
    /// The table changed (or it is unknown what changed).
    pub fn touched(&self, table: &str) -> bool {
        self.all || self.rows.contains_key(table)
    }
    /// The whole table changed or the rows are unknown: `ids` is incomplete, rebuild everything.
    pub fn whole(&self, table: &str) -> bool {
        self.all || self.rows.get(table).is_some_and(|r| r.contains(&WHOLE))
    }
    /// Changed rows (without the `WHOLE` marker; the list is incomplete when `whole`).
    pub fn ids(&self, table: &str) -> impl Iterator<Item = i64> + '_ {
        self.rows
            .get(table)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| i != WHOLE)
    }
    /// Only tables from the list changed.
    pub fn only(&self, tables: &[&str]) -> bool {
        !self.all && self.rows.keys().all(|t| tables.contains(&t.as_str()))
    }
}

/// `meta` keys that hold view state or preferences, never model content.
fn is_view_key(k: &str) -> bool {
    matches!(k, "phase_current" | "project_uid") || k.starts_with("view:") || k.starts_with("pref:")
}

/// User keys of `schema.meta` with their values, by key (view state left out).
pub(crate) fn user_meta(
    db: &rusqlite::Connection,
    schema: &str,
) -> Result<Vec<(String, rusqlite::types::Value)>, crate::Error> {
    let mut st = db.prepare(&format!("SELECT key, value FROM {schema}.meta ORDER BY key"))?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, rusqlite::types::Value>(1)?))
    })?;
    let mut v = vec![];
    for x in rows {
        let (k, val) = x?;
        if !is_view_key(&k) {
            v.push((k, val));
        }
    }
    Ok(v)
}

/// Start of the next model's numbers: numbers of different models do not overlap, so a cursor from
/// a closed model means "everything changed" for a new one.
static NEXT_BASE: AtomicU64 = AtomicU64::new(1 << 40);

pub(crate) fn install(db: &mut rusqlite::Connection) -> rusqlite::Result<Arc<Mutex<Tracker>>> {
    let base = NEXT_BASE.fetch_add(1 << 40, Ordering::Relaxed);
    let nanos = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let id = nanos ^ (u64::from(std::process::id()) << 32) ^ base.rotate_left(17);
    let t = Arc::new(Mutex::new(Tracker {
        base,
        origin: base,
        id: id as i64,
        ..Default::default()
    }));
    // IGNORE on DELETE keeps the statement but turns off the truncate optimisation, so
    // `DELETE FROM t` reaches the update hook row by row.
    // DROP TABLE also asks for Delete on the table, right after its own action; IGNORE there
    // would silently skip the DROP.
    let mut dropping = false;
    db.authorizer(Some(move |ctx: AuthContext<'_>| match ctx.action {
        AuthAction::DropTable { .. } | AuthAction::DropTempTable { .. } => {
            dropping = true;
            Authorization::Allow
        }
        AuthAction::Delete { table_name }
            if !table_name.starts_with("sqlite_") && !std::mem::take(&mut dropping) =>
        {
            Authorization::Ignore
        }
        _ => Authorization::Allow,
    }))?;
    let h = t.clone();
    db.update_hook(Some(move |a: Action, db: &str, table: &str, rowid: i64| {
        if db != "main" || table.starts_with("sqlite_") {
            return;
        }
        if let Ok(mut g) = h.lock() {
            g.note(table, rowid, matches!(a, Action::SQLITE_DELETE));
        }
    }))?;
    let h = t.clone();
    db.commit_hook(Some(move || {
        if let Ok(mut g) = h.lock() {
            g.commit();
        }
        false
    }))?;
    let h = t.clone();
    db.rollback_hook(Some(move || {
        if let Ok(mut g) = h.lock() {
            g.rollback();
        }
    }))?;
    Ok(t)
}

/// Rows after `since` together with the uncommitted rows of the current transaction (for
/// partial recomputation, `regen`); None means the window is lost or the number belongs to another model.
pub(crate) fn dirty(t: &Mutex<Tracker>, since: u64) -> Option<HashMap<String, HashSet<i64>>> {
    dirty_after(t, since, None)
}

/// [`dirty`] without the rows of the open transaction up to `covered` ([`tx_mark`]), when it
/// names this transaction: a recompute inside it already took them in.
pub(crate) fn dirty_after(
    t: &Mutex<Tracker>,
    since: u64,
    covered: Option<(u64, usize)>,
) -> Option<HashMap<String, HashSet<i64>>> {
    let g = t.lock().ok()?;
    if since < g.base || since > g.head() {
        return None;
    }
    let skip = match covered {
        Some((txn, n)) if txn == g.txn && n <= g.pending.len() => n,
        _ => 0,
    };
    let mut rows: HashMap<String, HashSet<i64>> = HashMap::new();
    for (tb, id) in g.log[(since - g.base) as usize..].iter().chain(&g.pending[skip..]) {
        if matches!(&**tb, "op" | "meta" | UNDERLAY) {
            continue;
        }
        rows.entry(tb.to_string()).or_default().insert(*id);
    }
    Some(rows)
}

/// Where the open transaction stands: its number and the rows it has written so far.
pub(crate) fn tx_mark(t: &Mutex<Tracker>) -> Option<(u64, usize)> {
    t.lock().ok().map(|g| (g.txn, g.pending.len()))
}

/// Every row the open transaction wrote after `mark` is in a table `quiet` accepts; false
/// when `mark` names another transaction.
pub(crate) fn quiet_since(
    t: &Mutex<Tracker>,
    mark: (u64, usize),
    quiet: impl Fn(&str) -> bool,
) -> bool {
    let Ok(g) = t.lock() else {
        return false;
    };
    g.txn == mark.0
        && mark.1 <= g.pending.len()
        && g.pending[mark.1..].iter().all(|(tb, _)| quiet(tb))
}

/// Same as `Model::tx_touch`, usable while the transaction borrows the connection.
pub(crate) fn touch_of(t: &Mutex<Tracker>) -> Touch {
    t.lock().map_or_else(
        |_| Touch::default(),
        |t| Touch {
            any: t.tx_tables.clone(),
            del: t.tx_deleted.clone(),
        },
    )
}

/// Rows of one table written after a point, by commits that wrote no undo step.
#[derive(Debug, PartialEq)]
pub(crate) enum Loose {
    /// No record: the window is lost or the point is another session's.
    Unknown,
    /// The whole table, or rows not known one by one.
    Whole,
    Rows(HashSet<i64>),
}

/// Rows of `table` written after `since` (a head of tracker `id`) outside undo steps. Undo is
/// last-in-first-out, so the steps after `since` are undone by now and their rows are back.
pub(crate) fn loose_since(t: &Mutex<Tracker>, id: i64, since: u64, table: &str) -> Loose {
    let Ok(g) = t.lock() else {
        return Loose::Unknown;
    };
    if id != g.id || since < g.base || since > g.head() {
        return Loose::Unknown;
    }
    let mut rows = HashSet::new();
    let mut steps = g.steps.iter().filter(|s| s.1 > since).peekable();
    for (n, (tb, r)) in (since..).zip(&g.log[(since - g.base) as usize..]) {
        while steps.peek().is_some_and(|s| s.1 <= n) {
            steps.next();
        }
        if steps.peek().is_some_and(|s| s.0 <= n) || &**tb != table {
            continue;
        }
        if *r == WHOLE {
            return Loose::Whole;
        }
        rows.insert(*r);
    }
    Loose::Rows(rows)
}

impl crate::Model {
    /// Session of [`Self::change_head`] numbers: a number kept in the file is read only by
    /// the session that wrote it.
    pub(crate) fn change_session(&self) -> i64 {
        self.tracker.lock().map_or(0, |t| t.id)
    }

    /// Number of the last change-journal entry.
    pub fn change_head(&self) -> u64 {
        self.tracker.lock().map_or(0, |t| t.head())
    }

    /// Whether `meta` settings were written after `since`: `changes_since` does not show them.
    pub(crate) fn meta_written_since(&self, since: u64) -> bool {
        let Ok(t) = self.tracker.lock() else {
            return true;
        };
        since < t.base
            || since > t.head()
            || t.log[(since - t.base) as usize..]
                .iter()
                .any(|(tb, _)| &**tb == "meta")
    }

    /// Whether user data in `meta` changed after `since`: view-state and preference keys
    /// (`phase_current`, `project_uid`, `view:*`, `pref:*`) are not user data. After any `meta`
    /// write the user keys are compared by value with `at_copy` ([`user_meta`] at the copy), not
    /// by rowid: OR REPLACE moves a rewritten key to a new rowid. A lost window counts as changed.
    pub(crate) fn meta_user_written_since(
        &self,
        since: u64,
        at_copy: &[(String, rusqlite::types::Value)],
    ) -> bool {
        {
            let Ok(t) = self.tracker.lock() else {
                return true;
            };
            if since < t.base || since > t.head() {
                return true;
            }
            if !t.log[(since - t.base) as usize..]
                .iter()
                .any(|(tb, _)| &**tb == "meta")
            {
                return false;
            }
        }
        user_meta(&self.db, "main").map_or(true, |now| now != at_copy)
    }

    /// Placed drawings were written after `since` (or the window is lost): a change kind of its
    /// own, never part of `changes_since`.
    pub fn underlays_written_since(&self, since: u64) -> bool {
        let Ok(t) = self.tracker.lock() else {
            return true;
        };
        since < t.base
            || since > t.head()
            || t.log[(since - t.base) as usize..]
                .iter()
                .any(|(tb, _)| &**tb == UNDERLAY)
    }

    /// Current [`EditState`]; writes outside operations to the `ignore` tables (rebuildable
    /// data) do not count.
    pub fn edit_state(&self, ignore: &[&str]) -> Result<EditState, crate::Error> {
        use rusqlite::OptionalExtension;
        let top: Option<(i64, Option<i64>)> = self
            .db
            .query_row("SELECT seq, stamp FROM op ORDER BY seq DESC LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        let t = self
            .tracker
            .lock()
            .map_err(|_| crate::Error::Invalid("change journal unavailable".into()))?;
        let free: u64 = t
            .free
            .iter()
            .filter(|(tb, _)| !ignore.contains(&&***tb))
            .map(|(_, n)| *n)
            .sum();
        Ok(EditState { top, free })
    }

    /// Current [`ContentMark`]; writes to the `ignore` tables (rebuildable data) do not count.
    pub fn content_mark(&self, ignore: &[&str]) -> Result<ContentMark, crate::Error> {
        let top = self.edit_state(ignore)?.top;
        let t = self
            .tracker
            .lock()
            .map_err(|_| crate::Error::Invalid("change journal unavailable".into()))?;
        let writes: u64 = t
            .written
            .iter()
            .filter(|(tb, _)| !ignore.contains(&&***tb))
            .map(|(_, n)| *n)
            .sum();
        Ok(ContentMark { origin: t.origin, writes, top })
    }

    /// Rows changed after `since`; the `op`/`meta` bookkeeping, the assistant's records and the
    /// placed drawings are left out.
    pub fn changes_since(&self, since: u64) -> Changes {
        let Ok(t) = self.tracker.lock() else {
            return Changes {
                all: true,
                ..Default::default()
            };
        };
        let head = t.head();
        if since < t.base || since > head {
            return Changes {
                head,
                all: true,
                rows: HashMap::new(),
            };
        }
        let mut rows: HashMap<String, HashSet<i64>> = HashMap::new();
        for (tb, id) in &t.log[(since - t.base) as usize..] {
            if not_content(tb) {
                continue;
            }
            rows.entry(tb.to_string()).or_default().insert(*id);
        }
        Changes {
            head,
            all: false,
            rows,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Model;

    #[test]
    fn op_undo_and_window_report_rows() {
        let mut m = Model::open(":memory:").unwrap();
        let h0 = m.change_head();
        let l = m.add_level("L", 0.0).unwrap();
        let c = m.changes_since(h0);
        assert!(c.ids("level").any(|i| i == l), "{c:?}");
        assert!(!c.touched("op") && !c.all);
        let h1 = m.change_head();
        m.undo().unwrap();
        assert!(m.changes_since(h1).ids("level").any(|i| i == l));
        assert!(m.changes_since(u64::MAX).all, "future cursor = rebuild");
        assert!(m.changes_since(m.change_head()).is_empty());
        let other = Model::open(":memory:").unwrap();
        assert!(other.changes_since(h1).all, "cursor of another model");
        assert!(m.changes_since(0).all);
    }

    #[test]
    fn underlay_writes_are_their_own_change_kind() {
        let mut m = Model::open(":memory:").unwrap();
        let h0 = m.change_head();
        let s0 = m.edit_state(&[]).unwrap();
        m.sql()
            .execute_batch(
                "INSERT INTO underlay(pos, name, level, unit, off_x, off_y, rot, visible, \
                 halftone, layers, entities, geom) \
                 VALUES (0, 'd', 1, 1, 0, 0, 0, 1, 0, '[]', 0, x'01')",
            )
            .unwrap();
        assert!(m.changes_since(h0).is_empty(), "not an element change");
        assert!(m.underlays_written_since(h0));
        assert_eq!(m.edit_state(&[]).unwrap(), s0, "drawings are tracked by the app");
        let h1 = m.change_head();
        m.add_level("L", 3.0).unwrap();
        assert!(!m.underlays_written_since(h1));
    }

    #[test]
    fn content_mark_never_comes_back() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("A", 3.0).unwrap();
        let (saved, state) = (m.content_mark(&[]).unwrap(), m.edit_state(&[]).unwrap());
        assert_eq!(m.content_mark(&[]).unwrap(), saved, "reading moves nothing");
        m.add_level("B", 6.0).unwrap();
        assert_ne!(m.content_mark(&[]).unwrap(), saved);
        m.undo().unwrap();
        assert_eq!(m.edit_state(&[]).unwrap(), state, "the edit state does come back");
        assert_ne!(m.content_mark(&[]).unwrap(), saved, "an undo is a write too");
        // a rolled back write left nothing, settings are not content
        let s = m.content_mark(&[]).unwrap();
        m.sql().execute_batch("BEGIN").unwrap();
        m.sql().execute("UPDATE level SET name='T' WHERE name='A'", []).unwrap();
        m.sql().execute_batch("ROLLBACK").unwrap();
        assert_eq!(m.content_mark(&[]).unwrap(), s);
        m.sql().execute("INSERT OR REPLACE INTO meta(key,value) VALUES('pref:x','1')", []).unwrap();
        assert_eq!(m.content_mark(&[]).unwrap(), s);
        // a write outside operations counts unless its table is rebuildable
        m.sql().execute("UPDATE level SET name='C' WHERE name='A'", []).unwrap();
        assert_ne!(m.content_mark(&[]).unwrap(), s);
        let s = m.content_mark(&["level"]).unwrap();
        m.sql().execute("UPDATE level SET name='D' WHERE name='C'", []).unwrap();
        assert_eq!(m.content_mark(&["level"]).unwrap(), s);
        // another connection in the same state is another model
        let (mut a, mut b) = (Model::open(":memory:").unwrap(), Model::open(":memory:").unwrap());
        assert_ne!(a.content_mark(&[]).unwrap(), b.content_mark(&[]).unwrap());
        a.add_level("A", 3.0).unwrap();
        b.add_level("A", 3.0).unwrap();
        assert_ne!(a.content_mark(&[]).unwrap(), b.content_mark(&[]).unwrap());
    }

    #[test]
    fn meta_only_step_undone_brings_its_mark_back_with_its_state() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("A", 3.0).unwrap();
        let (m0, f0) = (m.content_mark(&[]).unwrap(), m.saved_filters().unwrap());
        let list = ["walls height>3".to_string()];
        assert!(m.set_saved_filters(&list).unwrap());
        let m1 = m.content_mark(&[]).unwrap();
        assert_ne!(m1, m0, "the step's journal row moves the mark");
        m.undo().unwrap();
        // the old mark comes back only with the content and settings it named
        assert_eq!(m.saved_filters().unwrap(), f0);
        assert_eq!(m.content_mark(&[]).unwrap(), m0);
        // a redo runs the step again: a new random stamp, a mark not seen before
        assert!(m.set_saved_filters(&list).unwrap());
        let m2 = m.content_mark(&[]).unwrap();
        assert_ne!(m2, m0);
        assert_ne!(m2, m1, "the same settings again, yet a new mark");
    }

    #[test]
    fn edit_state_comes_back_with_undo_only() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("A", 3.0).unwrap();
        let saved = m.edit_state(&[]).unwrap();
        m.add_level("B", 6.0).unwrap();
        assert_ne!(m.edit_state(&[]).unwrap(), saved);
        m.undo().unwrap();
        assert_eq!(m.edit_state(&[]).unwrap(), saved, "undo to the saved step");
        // same sequence number reused by another step: the stamp tells them apart
        m.undo().unwrap();
        m.add_level("C", 9.0).unwrap();
        assert_ne!(m.edit_state(&[]).unwrap(), saved);
        let s = m.edit_state(&[]).unwrap();
        m.sql().execute("UPDATE level SET name='D' WHERE name='C'", []).unwrap();
        assert_ne!(m.edit_state(&[]).unwrap(), s, "a write outside operations");
        let s = m.edit_state(&["level"]).unwrap();
        m.sql().execute("UPDATE level SET name='E' WHERE name='D'", []).unwrap();
        assert_eq!(m.edit_state(&["level"]).unwrap(), s, "ignored tables do not count");
    }

    #[test]
    fn drop_table_is_not_swallowed_by_the_delete_rule() {
        let m = Model::open(":memory:").unwrap();
        let has = |n: &str| -> i64 {
            m.db.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name=?1",
                [n],
                |r| r.get(0),
            )
            .unwrap()
        };
        m.db.execute_batch("CREATE TABLE zz(a); CREATE TEMP TABLE zt(a); INSERT INTO zz VALUES(1)")
            .unwrap();
        m.db.execute_batch("DELETE FROM zz; DROP TABLE zz; DROP TABLE zt")
            .unwrap();
        assert_eq!(has("zz"), 0);
    }

    #[test]
    fn rollback_truncate_and_replace_are_seen_right() {
        let m = Model::open(":memory:").unwrap();
        let db = m.sql();
        db.execute_batch(
            "CREATE TABLE t(id INTEGER PRIMARY KEY, k TEXT UNIQUE);
             INSERT INTO t(k) VALUES ('a'), ('b');",
        )
        .unwrap();
        let h0 = m.change_head();
        db.execute_batch("BEGIN; INSERT INTO t(k) VALUES ('c'); ROLLBACK;")
            .unwrap();
        assert!(
            m.changes_since(h0).is_empty(),
            "rolled back rows are dropped"
        );
        db.execute("INSERT OR REPLACE INTO t(k) VALUES ('a')", [])
            .unwrap();
        let c = m.changes_since(h0);
        // update hook misses the row REPLACE removed (id 1) but reports the new one.
        assert!(
            c.ids("t").any(|i| i == 3) && !c.whole("t"),
            "REPLACE on UNIQUE: {c:?}"
        );
        let h1 = m.change_head();
        db.execute("DELETE FROM t", []).unwrap();
        assert_eq!(
            m.changes_since(h1).ids("t").count(),
            2,
            "DELETE without WHERE"
        );
        let h2 = m.change_head();
        db.execute_batch(
            "BEGIN; WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<5000)
             INSERT INTO t(k) SELECT 'x'||i FROM n; COMMIT;",
        )
        .unwrap();
        let c = m.changes_since(h2);
        assert!(
            c.whole("t") && c.ids("t").count() == 0 && !c.whole("level"),
            "{:?}",
            c.rows.len()
        );
    }
}
