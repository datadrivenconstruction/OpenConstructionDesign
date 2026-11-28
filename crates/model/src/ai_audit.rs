// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! AI action log and project settings (schema 31). Kept outside the undo journal, like `clash_item`:
//! `changes_since` does not see them, otherwise auditing would rebuild the scene and make a plan stale.
//! Event kinds: turn, snapshot, tool_write, tool_error, plan, apply, reject, cancel, mcp_connect, mcp_policy.
//! `op_seq` cannot be matched by number alone: after an undo the next operation takes the same
//! number (no AUTOINCREMENT), so undo marks the entry `op_undone`; matching uses `audit_op_live`.
//! Redo is a new operation: the app writes its own event for it.
//! The log is bounded (`AUDIT_ROWS`, `AUDIT_BYTES`); old entries are dropped on insert.

use std::path::Path;
use std::sync::OnceLock;
use web_time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use crate::{Error, Model};

const DDL: &str = "
CREATE TABLE IF NOT EXISTS ai_audit(seq INTEGER PRIMARY KEY, ts INTEGER NOT NULL, session TEXT NOT NULL,
    actor TEXT NOT NULL, event TEXT NOT NULL, endpoint TEXT, model TEXT, text TEXT, plan TEXT,
    op_seq INTEGER, tok_in INTEGER, tok_out INTEGER, cost REAL, status TEXT,
    op_undone INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS ai_policy(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS ai_chat(pos INTEGER PRIMARY KEY, kind TEXT NOT NULL, text TEXT NOT NULL);
";

/// Visible chat entries kept per project file; the oldest are dropped on save.
pub const CHAT_ROWS: usize = 500;

/// Log entries kept in the project file; the oldest are deleted.
const AUDIT_ROWS: i64 = 10_000;
/// Bytes of text and plans; checked once per `AUDIT_BYTES_EVERY` entries because it scans the log.
const AUDIT_BYTES: i64 = 64 << 20;
const AUDIT_BYTES_EVERY: i64 = 64;

/// Step 31: additive, independent of step 30 and of merge order.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    let has_undone: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('ai_audit') WHERE name='op_undone')",
        [],
        |r| r.get(0),
    )?;
    if !has_undone {
        db.execute_batch("ALTER TABLE ai_audit ADD COLUMN op_undone INTEGER NOT NULL DEFAULT 0")?;
    }
    Ok(())
}

/// Grants that must not travel with a copy of the file: cloud consent per host, snapshot
/// sharing, agent access. The local-only switch only restricts, so it stays.
const COPY_DROP: &str = "DELETE FROM ai_policy WHERE     (key LIKE 'egress.%' AND key <> 'egress.local_only') OR key LIKE 'mcp.%'";

/// Strips a finished copy of a project file in place: grants always, the journal unless kept.
/// Rows are wiped and the file vacuumed, so the removed text is not left in free pages.
pub(crate) fn strip_for_copy(path: &Path, keep_journal: bool) -> Result<(), Error> {
    let db = Connection::open(path)?;
    db.execute_batch("PRAGMA secure_delete=ON;")?;
    let has = |t: &str| -> Result<bool, Error> {
        Ok(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [t],
            |r| r.get(0),
        )?)
    };
    if has("ai_policy")? {
        db.execute(COPY_DROP, [])?;
    }
    if !keep_journal && has("ai_audit")? {
        db.execute("DELETE FROM ai_audit", [])?;
    }
    // the visible chat holds the same text as the journal
    if !keep_journal && has("ai_chat")? {
        db.execute("DELETE FROM ai_chat", [])?;
    }
    db.execute_batch("VACUUM;")?;
    db.close().map_err(|(_, e)| Error::Db(e))?;
    std::fs::OpenOptions::new().write(true).open(path)?.sync_all()?;
    Ok(())
}

/// Entry `seq` was removed by an undo: the next operation will take its number.
pub(crate) fn note_undo(db: &Connection, seq: i64) -> Result<(), Error> {
    db.prepare_cached("UPDATE ai_audit SET op_undone=1 WHERE op_seq=?1 AND op_undone=0")?
        .execute([seq])?;
    Ok(())
}

/// Oldest entries beyond the limit; numbers keep growing, so the newest entry always stays.
fn trim_rows(db: &Connection, rows: i64) -> Result<(), Error> {
    db.execute(
        "DELETE FROM ai_audit WHERE seq <= (SELECT max(seq) FROM ai_audit) - ?1",
        [rows],
    )?;
    Ok(())
}

fn trim_bytes(db: &Connection, bytes: i64) -> Result<(), Error> {
    db.execute(
        "DELETE FROM ai_audit WHERE seq < (SELECT max(seq) FROM ai_audit) AND seq <= (\
         SELECT max(seq) FROM (SELECT seq, sum(coalesce(length(CAST(plan AS BLOB)),0) + \
         coalesce(length(CAST(text AS BLOB)),0)) OVER (ORDER BY seq DESC) AS total \
         FROM ai_audit) WHERE total > ?1)",
        [bytes],
    )?;
    Ok(())
}

/// A log entry; `ts` and `seq` are assigned by the model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AuditEntry {
    pub session: String,
    pub actor: String,
    pub event: String,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub text: Option<String>,
    pub plan: Option<String>,
    pub op_seq: Option<i64>,
    pub tok_in: u64,
    pub tok_out: u64,
    pub cost: Option<f64>,
    pub status: String,
}

static SCRUB: OnceLock<fn(&str) -> String> = OnceLock::new();

/// A filter every journal field passes through on its way into the file (the first one set wins).
pub fn set_audit_scrubber(f: fn(&str) -> String) {
    let _ = SCRUB.set(f);
}

fn scrubbed(e: &AuditEntry) -> AuditEntry {
    let Some(f) = SCRUB.get() else {
        return e.clone();
    };
    AuditEntry {
        session: f(&e.session),
        actor: f(&e.actor),
        event: f(&e.event),
        endpoint: e.endpoint.as_deref().map(f),
        model: e.model.as_deref().map(f),
        text: e.text.as_deref().map(f),
        plan: e.plan.as_deref().map(f),
        status: f(&e.status),
        ..e.clone()
    }
}

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Model {
    /// Replaces the stored chat with the newest `CHAT_ROWS` of `rows` (kind, text) in one
    /// transaction. Text is scrubbed like the journal. Outside undo: no journal record.
    pub fn chat_save(&self, rows: &[(String, String)]) -> Result<(), Error> {
        let from = rows.len().saturating_sub(CHAT_ROWS);
        let tx = self.db.unchecked_transaction()?;
        tx.execute_batch(DDL)?;
        tx.execute("DELETE FROM ai_chat", [])?;
        {
            let mut st = tx.prepare("INSERT INTO ai_chat(pos, kind, text) VALUES (?1, ?2, ?3)")?;
            for (i, (kind, text)) in rows[from..].iter().enumerate() {
                let text = match SCRUB.get() {
                    Some(f) => f(text),
                    None => text.clone(),
                };
                st.execute(params![i as i64, kind, text])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The stored chat, oldest first.
    pub fn chat_load(&self) -> Result<Vec<(String, String)>, Error> {
        self.db.execute_batch(DDL)?;
        let mut st = self
            .db
            .prepare("SELECT kind, text FROM ai_chat ORDER BY pos")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Appends an entry (time in Unix seconds); returns its number.
    pub fn audit_append(&self, e: &AuditEntry) -> Result<i64, Error> {
        let e = &scrubbed(e);
        self.db.execute(
            "INSERT INTO ai_audit(ts,session,actor,event,endpoint,model,text,plan,op_seq,\
             tok_in,tok_out,cost,status) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                now_s(),
                e.session,
                e.actor,
                e.event,
                e.endpoint,
                e.model,
                e.text,
                e.plan,
                e.op_seq,
                e.tok_in as i64,
                e.tok_out as i64,
                e.cost,
                e.status
            ],
        )?;
        let seq = self.db.last_insert_rowid();
        trim_rows(&self.db, AUDIT_ROWS)?;
        if seq % AUDIT_BYTES_EVERY == 0 {
            trim_bytes(&self.db, AUDIT_BYTES)?;
        }
        Ok(seq)
    }

    /// Rewrites what older versions stored: `endpoint` through `endpoint`, the text fields
    /// and the chat through `text`. Only changed rows are written; returns their count.
    pub fn audit_rewrite(
        &self,
        endpoint: fn(&str) -> String,
        text: fn(&str) -> String,
    ) -> Result<usize, Error> {
        self.db.execute_batch(DDL)?;
        let tx = self.db.unchecked_transaction()?;
        let mut n = 0;
        {
            let mut st = tx.prepare("SELECT seq, endpoint, text, plan, status FROM ai_audit")?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut up = tx.prepare(
                "UPDATE ai_audit SET endpoint=?2, text=?3, plan=?4, status=?5 WHERE seq=?1",
            )?;
            for (seq, e, t, p, s) in rows {
                let (e2, t2) = (e.as_deref().map(endpoint), t.as_deref().map(text));
                let (p2, s2) = (p.as_deref().map(text), s.as_deref().map(text));
                if (&e2, &t2, &p2, &s2) != (&e, &t, &p, &s) {
                    up.execute(params![seq, e2, t2, p2, s2])?;
                    n += 1;
                }
            }
            let mut st = tx.prepare("SELECT pos, text FROM ai_chat")?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            let mut up = tx.prepare("UPDATE ai_chat SET text=?2 WHERE pos=?1")?;
            for (pos, t) in rows {
                let t2 = text(&t);
                if t2 != t {
                    up.execute(params![pos, t2])?;
                    n += 1;
                }
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// The apply recorded in row `seq` has not been undone: it is the same journal entry, not
    /// a different one under the same number. An entry without `op_seq` yields `false`.
    pub fn audit_op_live(&self, seq: i64) -> Result<bool, Error> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_audit a JOIN op ON op.seq=a.op_seq \
             WHERE a.seq=?1 AND a.op_undone=0)",
            [seq],
            |r| r.get(0),
        )?)
    }

    /// Number of the newest operation in the undo log; `None` when there is nothing to undo.
    pub fn last_op_seq(&self) -> Result<Option<i64>, Error> {
        Ok(self
            .db
            .query_row("SELECT max(seq) FROM op", [], |r| r.get(0))?)
    }

    /// Clears the log before the project file is handed over (prompts and addresses do not leave with it).
    pub fn audit_clear(&self) -> Result<(), Error> {
        self.db.execute("DELETE FROM ai_audit", [])?;
        Ok(())
    }

    /// Entries after number `after` in ascending order: (number, time, entry).
    pub fn audit_list(
        &self,
        after: i64,
        limit: usize,
    ) -> Result<Vec<(i64, i64, AuditEntry)>, Error> {
        let mut st = self.db.prepare(
            "SELECT seq,ts,session,actor,event,endpoint,model,text,plan,op_seq,\
             coalesce(tok_in,0),coalesce(tok_out,0),cost,coalesce(status,'') \
             FROM ai_audit WHERE seq>?1 ORDER BY seq LIMIT ?2",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let v = st
            .query_map(params![after, limit], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    AuditEntry {
                        session: r.get(2)?,
                        actor: r.get(3)?,
                        event: r.get(4)?,
                        endpoint: r.get(5)?,
                        model: r.get(6)?,
                        text: r.get(7)?,
                        plan: r.get(8)?,
                        op_seq: r.get(9)?,
                        tok_in: r.get::<_, i64>(10)?.max(0) as u64,
                        tok_out: r.get::<_, i64>(11)?.max(0) as u64,
                        cost: r.get(12)?,
                        status: r.get(13)?,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn ai_policy(&self, k: &str) -> Result<Option<String>, Error> {
        Ok(self
            .db
            .query_row("SELECT value FROM ai_policy WHERE key=?1", [k], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_ai_policy(&self, k: &str, v: &str) -> Result<(), Error> {
        self.db.execute(
            "INSERT OR REPLACE INTO ai_policy(key, value) VALUES (?1, ?2)",
            [k, v],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::VERSION;

    fn entry(event: &str) -> AuditEntry {
        AuditEntry {
            session: "s1".into(),
            actor: "chat".into(),
            event: event.into(),
            endpoint: Some("http://127.0.0.1:1".into()),
            model: Some("m".into()),
            text: None,
            plan: Some("{}".into()),
            op_seq: Some(7),
            tok_in: 12,
            tok_out: 34,
            cost: Some(0.5),
            status: "ok".into(),
        }
    }

    #[test]
    fn audit_rewrite_cleans_stored_rows_and_the_chat() {
        fn origin(s: &str) -> String {
            s.split('?').next().unwrap_or_default().to_string()
        }
        fn mask(s: &str) -> String {
            s.replace("FAKEKEY123", "***")
        }
        let m = Model::open(":memory:").unwrap();
        let mut e = entry("apply");
        e.endpoint = Some("https://h.test/v1?key=FAKEKEY123".into());
        e.text = Some("sent to https://h.test/v1?key=FAKEKEY123".into());
        m.audit_append(&e).unwrap();
        m.audit_append(&entry("turn")).unwrap();
        m.chat_save(&[("user".into(), "my FAKEKEY123".into())]).unwrap();
        let head = m.change_head();
        assert_eq!(m.audit_rewrite(origin, mask).unwrap(), 2);
        let all = m.audit_list(0, 10).unwrap();
        assert_eq!(all[0].2.endpoint.as_deref(), Some("https://h.test/v1"));
        assert!(!format!("{all:?}").contains("FAKEKEY123"));
        assert!(!format!("{:?}", m.chat_load().unwrap()).contains("FAKEKEY123"));
        assert_eq!(m.audit_rewrite(origin, mask).unwrap(), 0);
        assert_eq!(m.change_head(), head);
    }

    #[test]
    fn audit_round_trip_pages_and_stays_out_of_undo() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L1", 0.0).unwrap();
        let (top, head) = (m.op_top().unwrap(), m.change_head());
        let a = m.audit_append(&entry("turn")).unwrap();
        let b = m.audit_append(&entry("apply")).unwrap();
        assert!(b > a);
        let all = m.audit_list(0, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].2, entry("turn"));
        assert!(all[0].1 > 0, "timestamp");
        let tail = m.audit_list(a, 10).unwrap();
        assert_eq!((tail.len(), tail[0].0), (1, b));
        assert_eq!(m.audit_list(0, 1).unwrap().len(), 1);
        assert_eq!(m.op_top().unwrap(), top, "not in the undo journal");
        assert!(m.changes_since(head).is_empty(), "does not rebuild views");
        m.undo().unwrap();
        assert_eq!(m.audit_list(0, 10).unwrap().len(), 2, "undo keeps the log");
    }

    #[test]
    fn undone_plan_is_not_live_when_the_next_op_takes_its_number() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L1", 0.0).unwrap();
        let seq = m.op_top().unwrap();
        let mut e = entry("apply");
        e.op_seq = Some(seq);
        let a = m.audit_append(&e).unwrap();
        assert!(m.audit_op_live(a).unwrap());
        m.undo().unwrap();
        assert!(!m.audit_op_live(a).unwrap(), "no such record");
        m.add_level("OTHER", 3.0).unwrap();
        assert_eq!(m.op_top().unwrap(), seq, "the number is reused");
        assert!(!m.audit_op_live(a).unwrap(), "another record under it");
        e.op_seq = None;
        let b = m.audit_append(&e).unwrap();
        assert!(!m.audit_op_live(b).unwrap());
        assert!(!m.audit_op_live(b + 1).unwrap());
    }

    #[test]
    fn the_log_is_bounded_and_can_be_cleared() {
        let m = Model::open(":memory:").unwrap();
        let mut e = entry("plan");
        e.plan = Some("x".repeat(100));
        for _ in 0..10 {
            m.audit_append(&e).unwrap();
        }
        trim_rows(&m.db, 4).unwrap();
        let kept = m.audit_list(0, 100).unwrap();
        assert_eq!(kept.len(), 4);
        assert_eq!(kept[3].0, 10, "the newest stay");
        trim_bytes(&m.db, 250).unwrap();
        let kept = m.audit_list(0, 100).unwrap();
        assert_eq!(
            kept.len(),
            2,
            "two rows of 100 bytes fit, the third goes over"
        );
        assert_eq!(kept[1].0, 10);
        trim_bytes(&m.db, 1).unwrap();
        assert_eq!(
            m.audit_list(0, 100).unwrap().len(),
            1,
            "the newest is never cut"
        );
        assert_eq!(m.audit_append(&e).unwrap(), 11, "numbers keep growing");
        m.audit_clear().unwrap();
        assert!(m.audit_list(0, 100).unwrap().is_empty());
    }

    #[test]
    fn policy_set_get_replace() {
        let m = Model::open(":memory:").unwrap();
        let head = m.change_head();
        assert_eq!(m.ai_policy("mcp.policy").unwrap(), None);
        m.set_ai_policy("mcp.policy", "confirm").unwrap();
        m.set_ai_policy("mcp.policy", "auto").unwrap();
        assert_eq!(m.ai_policy("mcp.policy").unwrap().as_deref(), Some("auto"));
        assert!(m.changes_since(head).is_empty());
    }

    #[test]
    fn older_file_gets_tables_without_losing_rows() {
        let d = std::env::temp_dir().join(format!("ddc-ai-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("m.ddc");
        let ps = p.to_str().unwrap();
        {
            let m = Model::open(ps).unwrap();
            assert_eq!(m.schema_version().unwrap(), VERSION);
        }
        {
            // own connection: the model's authorizer turns DROP TABLE into a silent no-op
            let c = Connection::open(ps).unwrap();
            c.execute_batch(
                "DROP TABLE ai_audit; DROP TABLE ai_policy; \
                 CREATE TABLE ai_audit(seq INTEGER PRIMARY KEY, ts INTEGER NOT NULL, \
                 session TEXT NOT NULL, actor TEXT NOT NULL, event TEXT NOT NULL, endpoint TEXT, \
                 model TEXT, text TEXT, plan TEXT, op_seq INTEGER, tok_in INTEGER, \
                 tok_out INTEGER, cost REAL, status TEXT);",
            )
            .unwrap();
            c.execute(
                "UPDATE meta SET value=?1 WHERE key='schema_version'",
                [VERSION - 1],
            )
            .unwrap();
        }
        {
            let m = Model::open(ps).unwrap();
            assert_eq!(m.schema_version().unwrap(), VERSION);
            m.audit_append(&entry("turn")).unwrap();
            m.set_ai_policy("k", "v").unwrap();
        }
        let m = Model::open(ps).unwrap();
        assert_eq!(m.audit_list(0, 10).unwrap().len(), 1, "reopen is additive");
        assert_eq!(m.ai_policy("k").unwrap().as_deref(), Some("v"));
        drop(m);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn shared_copy_drops_consent_and_journal_unless_kept() {
        let m = Model::open(":memory:").unwrap();
        for (k, v) in [
            ("egress.consent.https://a.test", "1"),
            ("egress.snapshots", "1"),
            ("egress.local_only", "1"),
            ("mcp.policy", "auto"),
            ("limits.max_cost", "2"),
        ] {
            m.set_ai_policy(k, v).unwrap();
        }
        m.audit_append(&entry("turn")).unwrap();
        for keep in [false, true] {
            let ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dst = std::env::temp_dir().join(format!("ddc-copy-{}-{ns}.ddc", std::process::id()));
            m.save_shared_copy(&dst, keep, &mut |_, _| Ok(())).unwrap();
            let c = Model::open(dst.to_str().unwrap()).unwrap();
            assert_eq!(c.ai_policy("egress.consent.https://a.test").unwrap(), None);
            assert_eq!(c.ai_policy("egress.snapshots").unwrap(), None);
            assert_eq!(c.ai_policy("mcp.policy").unwrap(), None);
            assert_eq!(c.ai_policy("egress.local_only").unwrap().as_deref(), Some("1"));
            assert_eq!(c.ai_policy("limits.max_cost").unwrap().as_deref(), Some("2"));
            assert_eq!(c.audit_list(0, 10).unwrap().len(), usize::from(keep));
            drop(c);
            crate::project::remove_db_files(&dst);
        }
        assert!(m.ai_policy("egress.snapshots").unwrap().is_some(), "source keeps its grants");
        assert_eq!(m.audit_list(0, 10).unwrap().len(), 1);
    }
}
