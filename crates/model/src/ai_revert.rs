//! Review and revert of AI batches. An AI batch is a journal operation named by a live `apply`
//! audit record (`ai_audit.op_seq`, not undone), or one whose journal actor is an AI or an agent
//! and that no such record names (an envelope applied without an audit). Reverting runs the batch's inverse commands
//! out of order, which is safe only when no later operation touched the same rows: the
//! footprint of every inverse statement (table plus row key) is compared with the footprints of
//! the later operations. Any overlap, or any statement that cannot be read, refuses the whole
//! revert before a single row is written. The revert is a new journal operation whose own
//! inverse is a snapshot of every row it can touch, so it is one undo step and the original
//! batches stay undoable in order.

use rusqlite::types::Value;
use serde_json::Value as J;

use ddc_ops::Actor;

use crate::ops::{from_json, run_inv, Inv, Snapshots};
use crate::{Error, Model};

/// Rows a single revert may snapshot; above this the revert is refused, not truncated.
const MAX_ROWS: usize = 100_000;

/// One AI batch in the journal.
#[derive(Clone, Debug, PartialEq)]
pub struct AiBatch {
    /// `ai_audit.seq` of the apply record; `-op_seq` for an envelope batch without one (the
    /// revert record lists this value, so audit seqs and envelope batches never collide).
    pub audit: i64,
    /// Journal operation of the batch.
    pub op_seq: i64,
    /// Who applied: the person (`user`) or an agent name.
    pub actor: String,
    pub model: Option<String>,
    /// First line of the recorded text.
    pub title: String,
    /// Remaining lines of the recorded text.
    pub why: Option<String>,
    /// A live revert operation covers this batch.
    pub reverted: bool,
}

/// Why a revert was not done. Nothing was written in any of these cases.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// No batches were named.
    Empty,
    /// The operation is not a live AI batch.
    NotAiBatch(i64),
    AlreadyReverted(i64),
    /// A later operation changed rows the batch changed.
    Conflict {
        batch: i64,
        with_op: i64,
        kind: String,
        table: String,
        /// Row id both operations wrote, when the statements name one.
        row: Option<i64>,
    },
    /// The batch has a command whose target cannot be read, or is too large to snapshot.
    Opaque { op: i64 },
}

impl Refusal {
    /// Plain English reason for logs; the UI has its own translated texts.
    pub fn reason(&self) -> String {
        match self {
            Refusal::Empty => "no AI batch selected".into(),
            Refusal::NotAiBatch(s) => format!("operation {s} is not an AI batch"),
            Refusal::AlreadyReverted(s) => format!("batch {s} is already reverted"),
            Refusal::Conflict {
                batch,
                with_op,
                kind,
                table,
                ..
            } => format!(
                "batch {batch} cannot be reverted: later operation {with_op} ({kind}) changed {table}"
            ),
            Refusal::Opaque { op } => format!("batch {op} cannot be reverted safely"),
        }
    }
}

#[derive(Debug)]
pub enum RevertError {
    Refused(Refusal),
    Db(Error),
}

impl From<Error> for RevertError {
    fn from(e: Error) -> Self {
        RevertError::Db(e)
    }
}

impl From<rusqlite::Error> for RevertError {
    fn from(e: rusqlite::Error) -> Self {
        RevertError::Db(e.into())
    }
}

impl RevertError {
    pub fn into_error(self) -> Error {
        match self {
            RevertError::Refused(r) => Error::Invalid(r.reason()),
            RevertError::Db(e) => e,
        }
    }
}

/// Which rows of a table a statement writes.
#[derive(Clone, Debug, PartialEq)]
enum Key {
    /// `col = value`.
    Eq(String, Value),
    /// `rowid > n`.
    After(i64),
    /// The whole table (or not readable).
    Any,
}

type Target = (String, Key);

struct OpRow {
    seq: i64,
    kind: String,
    inv: String,
    targets: Vec<Target>,
}

fn vs(v: &Value) -> String {
    match v {
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Text(s) => s.clone(),
        Value::Blob(_) => "blob".into(),
        Value::Null => "null".into(),
    }
}

fn ident(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then(|| s.to_string())
}

fn param(s: &str) -> Option<usize> {
    s.trim()
        .strip_prefix('?')?
        .parse::<usize>()
        .ok()
        .filter(|i| *i >= 1)
}

fn parse_where(w: &str, args: &[Value]) -> Key {
    let w = w.trim();
    let up = w.to_ascii_uppercase();
    if up.contains(" AND ") || up.contains(" OR ") || w.contains(">=") || w.contains('<') {
        return Key::Any;
    }
    if let Some((l, r)) = w.split_once('=') {
        if let (Some(col), Some(i)) = (ident(l), param(r)) {
            if let Some(v) = args.get(i - 1) {
                return Key::Eq(col, v.clone());
            }
        }
        return Key::Any;
    }
    if let Some((l, r)) = w.split_once('>') {
        if l.trim().eq_ignore_ascii_case("rowid") {
            if let Some(i) = param(r) {
                if let Some(Value::Integer(n)) = args.get(i - 1) {
                    return Key::After(*n);
                }
            }
        }
    }
    Key::Any
}

fn table_of(rest: &str) -> String {
    rest.trim_start()
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Table and rows written by one inverse command. `*` means unreadable: it overlaps everything.
fn parse_stmt(sql: &str, args: &[Value]) -> Target {
    let s = sql.trim();
    let up = s.to_ascii_uppercase();
    let sub = up.contains("SELECT");
    let opaque = || ("*".to_string(), Key::Any);
    if up.starts_with("DELETE FROM ") || up.starts_with("UPDATE ") {
        let skip = if up.starts_with("UPDATE ") { 7 } else { 12 };
        let t = table_of(&s[skip..]);
        if ident(&t).is_none() {
            return opaque();
        }
        let key = if sub {
            Key::Any
        } else {
            match up.find(" WHERE ") {
                Some(i) => parse_where(&s[i + 7..], args),
                None => Key::Any,
            }
        };
        return (t, key);
    }
    for p in [
        "INSERT OR REPLACE INTO ",
        "INSERT OR IGNORE INTO ",
        "INSERT INTO ",
    ] {
        if up.starts_with(p) {
            let rest = &s[p.len()..];
            let t = table_of(rest);
            if ident(&t).is_none() {
                return opaque();
            }
            let after = rest.trim_start()[t.len()..].trim_start();
            let key = match after.strip_prefix('(') {
                Some(cols) if !sub => {
                    let first = cols.split(',').next().unwrap_or("").trim();
                    if first.eq_ignore_ascii_case("rowid") {
                        args.first()
                            .map_or(Key::Any, |v| Key::Eq("rowid".into(), v.clone()))
                    } else {
                        Key::Any
                    }
                }
                _ => Key::Any,
            };
            return (t, key);
        }
    }
    opaque()
}

fn norm(c: &str) -> String {
    let c = c.to_ascii_lowercase();
    if c == "id" {
        "rowid".into()
    } else {
        c
    }
}

fn keys_overlap(a: &Key, b: &Key) -> bool {
    match (a, b) {
        (Key::Any, _) | (_, Key::Any) => true,
        (Key::After(_), Key::After(_)) => true,
        (Key::After(n), Key::Eq(c, v)) | (Key::Eq(c, v), Key::After(n)) => {
            if norm(c) == "rowid" {
                match v {
                    Value::Integer(i) => i > n,
                    _ => true,
                }
            } else {
                true
            }
        }
        (Key::Eq(c1, v1), Key::Eq(c2, v2)) => norm(c1) != norm(c2) || vs(v1) == vs(v2),
    }
}

fn row_key(k: &Key) -> Option<i64> {
    match k {
        Key::Eq(c, Value::Integer(i)) if norm(c) == "rowid" => Some(*i),
        _ => None,
    }
}

/// The table of the first pair of targets that may touch the same rows, and the row id when
/// either statement names one.
fn overlap(a: &[Target], b: &[Target]) -> Option<(String, Option<i64>)> {
    for x in a {
        for y in b {
            let same = x.0 == "*" || y.0 == "*" || x.0.eq_ignore_ascii_case(&y.0);
            if same && keys_overlap(&x.1, &y.1) {
                let t = if y.0 == "*" { x.0.clone() } else { y.0.clone() };
                return Some((t, row_key(&y.1).or_else(|| row_key(&x.1))));
            }
        }
    }
    None
}

fn targets_of(inv: &str) -> Result<Vec<Target>, Error> {
    let J::Array(stmts) = serde_json::from_str::<J>(inv)? else {
        return Err(Error::Invalid("bad op log".into()));
    };
    Ok(stmts
        .iter()
        .map(|s| {
            let sql = s[0].as_str().unwrap_or_default();
            let args: Vec<Value> = s[1]
                .as_array()
                .map_or(vec![], |a| a.iter().map(from_json).collect());
            if sql == crate::meshes::MESH_XF {
                return ("*".to_string(), Key::Any);
            }
            parse_stmt(sql, &args)
        })
        .collect())
}

/// The `revert` audit row of the operation being recorded on `db`. `op.seq` is an integer
/// primary key, so the operation logged at the end of this transaction takes `max(seq) + 1`.
fn write_revert_audit(db: &rusqlite::Connection, audits: &[i64]) -> Result<(), Error> {
    let next: i64 = db.query_row("SELECT coalesce(max(seq),0)+1 FROM op", [], |r| r.get(0))?;
    let ts = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let plan = format!(
        "reverted={}",
        audits
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    db.execute(
        "INSERT INTO ai_audit(ts,session,actor,event,plan,op_seq,tok_in,tok_out,status)          VALUES (?1,'','user','revert',?2,?3,0,0,'ok')",
        rusqlite::params![ts, plan, next],
    )?;
    Ok(())
}

impl Model {
    /// Live AI batches, oldest first. A batch whose operation was undone is not listed.
    pub fn ai_batches(&self) -> Result<Vec<AiBatch>, Error> {
        let mut covered: Vec<i64> = vec![];
        let mut st = self.db.prepare(
            "SELECT a.plan FROM ai_audit a JOIN op ON op.seq=a.op_seq \
             WHERE a.event='revert' AND a.op_undone=0",
        )?;
        let plans = st.query_map([], |r| r.get::<_, Option<String>>(0))?;
        for p in plans {
            if let Some(list) = p?.as_deref().and_then(|p| p.strip_prefix("reverted=")) {
                covered.extend(list.split(',').filter_map(|n| n.trim().parse::<i64>().ok()));
            }
        }
        let mut st = self.db.prepare(
            "SELECT a.seq, a.op_seq, a.actor, a.model, coalesce(a.text,'') \
             FROM ai_audit a JOIN op ON op.seq=a.op_seq \
             WHERE a.event='apply' AND a.status='ok' AND a.op_undone=0 ORDER BY a.op_seq",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut out = vec![];
        for r in rows {
            let (audit, op_seq, actor, model, text) = r?;
            let (title, why) = match text.split_once('\n') {
                Some((t, w)) => (t.trim().to_string(), Some(w.trim().to_string())),
                None => (text.trim().to_string(), None),
            };
            out.push(AiBatch {
                audit,
                op_seq,
                actor,
                model,
                title,
                why: why.filter(|w| !w.is_empty()),
                reverted: covered.contains(&audit),
            });
        }
        for b in self.envelope_batches()? {
            let reverted = covered.contains(&b.audit);
            out.push(AiBatch { reverted, ..b });
        }
        out.sort_by_key(|b| b.op_seq);
        Ok(out)
    }

    /// Journal operations made by an AI or an agent that no live `apply` record names; title and
    /// why come from the stored envelope, then from the row's own columns.
    fn envelope_batches(&self) -> Result<Vec<AiBatch>, Error> {
        let mut st = self.db.prepare(
            "SELECT op.seq, op.actor, op.title, op.why FROM op \
             WHERE op.actor IS NOT NULL AND op.actor<>?1 AND NOT EXISTS (\
               SELECT 1 FROM ai_audit a \
               WHERE a.op_seq=op.seq AND a.event='apply' AND a.op_undone=0) \
             ORDER BY op.seq",
        )?;
        let rows = st
            .query_map([crate::journal::USER], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = vec![];
        for (op_seq, actor, title, why) in rows {
            let (actor, model) = match crate::journal::parse_actor(actor) {
                // a chat plan is applied by the person
                Some(Actor::Ai { model, .. }) => ("user".to_string(), Some(model)),
                Some(Actor::Agent { name }) => (name, None),
                _ => continue,
            };
            // a damaged envelope still lists the batch, from the row's own columns
            let (title, why) = match self.journal_src(op_seq).ok().flatten() {
                Some(env) => (env.title, env.why),
                None => (title.unwrap_or_default(), why),
            };
            out.push(AiBatch {
                audit: -op_seq,
                op_seq,
                actor,
                model,
                title: title.trim().to_string(),
                why: why
                    .map(|w| w.trim().to_string())
                    .filter(|w| !w.is_empty()),
                reverted: false,
            });
        }
        Ok(out)
    }

    fn load_ops(&self, from: i64) -> Result<Vec<OpRow>, Error> {
        let mut st = self
            .db
            .prepare("SELECT seq, kind, inv FROM op WHERE seq>=?1 ORDER BY seq")?;
        let rows = st.query_map([from], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = vec![];
        for r in rows {
            let (seq, kind, inv) = r?;
            let targets = targets_of(&inv)?;
            out.push(OpRow {
                seq,
                kind,
                inv,
                targets,
            });
        }
        Ok(out)
    }

    /// Checks that the named batches can be reverted as they stand; reads only.
    pub fn ai_revert_check(&self, seqs: &[i64]) -> Result<(), RevertError> {
        self.plan_revert(seqs).map(|_| ())
    }

    /// The `name` of a row a revert conflict points at; `None` when the table has no such
    /// column, the row is gone or the name is empty.
    pub fn conflict_row_name(&self, table: &str, row: i64) -> Option<String> {
        let t = ident(table)?;
        let sql = format!("SELECT name FROM \"{t}\" WHERE rowid=?1");
        let name: Option<String> = self.db.query_row(&sql, [row], |r| r.get(0)).ok()?;
        name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    fn plan_revert(&self, seqs: &[i64]) -> Result<(Vec<OpRow>, Vec<i64>), RevertError> {
        let mut seqs = seqs.to_vec();
        seqs.sort_unstable();
        seqs.dedup();
        let Some(&first) = seqs.first() else {
            return Err(RevertError::Refused(Refusal::Empty));
        };
        let batches = self.ai_batches()?;
        let mut audits = vec![];
        for s in &seqs {
            match batches.iter().find(|b| b.op_seq == *s) {
                None => return Err(RevertError::Refused(Refusal::NotAiBatch(*s))),
                Some(b) if b.reverted => {
                    return Err(RevertError::Refused(Refusal::AlreadyReverted(*s)))
                }
                Some(b) => audits.push(b.audit),
            }
        }
        let rows = self.load_ops(first)?;
        let (sel, later): (Vec<OpRow>, Vec<OpRow>) =
            rows.into_iter().partition(|r| seqs.contains(&r.seq));
        for a in &sel {
            if a.targets.iter().any(|t| t.0 == "*") {
                return Err(RevertError::Refused(Refusal::Opaque { op: a.seq }));
            }
            for l in later.iter().filter(|l| l.seq > a.seq) {
                if let Some((table, row)) = overlap(&a.targets, &l.targets) {
                    return Err(RevertError::Refused(Refusal::Conflict {
                        batch: a.seq,
                        with_op: l.seq,
                        kind: l.kind.clone(),
                        table,
                        row,
                    }));
                }
            }
        }
        Ok((sel, audits))
    }

    /// Reverts the named AI batches as one undo step, keeping every later edit. Refuses, with
    /// nothing written, when a later operation touched the same rows. Returns the batch count.
    pub fn revert_ai(&mut self, seqs: &[i64]) -> Result<usize, RevertError> {
        let (sel, audits) = self.plan_revert(seqs)?;
        let n = sel.len();
        // the tracker lets the inverse see rows written after the batch (loose rows survive)
        let tracker = self.tracker.clone();
        self.op("ai_revert", move |db| {
            let mut snaps = Snapshots::new();
            let mut conds: Vec<Target> = vec![];
            for r in &sel {
                for t in &r.targets {
                    if !conds.contains(t) {
                        conds.push(t.clone());
                    }
                }
            }
            // The undo of the revert first deletes whatever the revert inserts, then restores
            // every row that existed: all deletes are queued before any restore.
            let mut rows: Vec<(String, i64)> = vec![];
            for (t, key) in &conds {
                let (cond, args) = match key {
                    Key::Eq(c, v) => (format!("{c} = ?1"), vec![v.clone()]),
                    Key::After(n) => ("rowid > ?1".to_string(), vec![Value::Integer(*n)]),
                    Key::Any => ("1=1".to_string(), vec![]),
                };
                let mut st = db.prepare(&format!("SELECT rowid FROM {t} WHERE {cond}"))?;
                let ids = st
                    .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                        r.get::<_, i64>(0)
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                snaps.push(Inv::default().sql(&format!("DELETE FROM {t} WHERE {cond}"), args));
                rows.extend(ids.into_iter().map(|i| (t.clone(), i)));
                if rows.len() > MAX_ROWS {
                    return Err(Error::Invalid("too many rows to revert".into()));
                }
            }
            for (t, i) in &rows {
                snaps.save(db, t, "rowid", *i)?;
            }
            // The audit row goes in the same transaction, before the inverse runs: any later
            // failure rolls both back, so a batch is never reverted without its record.
            write_revert_audit(db, &audits)?;
            for r in sel.iter().rev() {
                run_inv(db, &r.inv, Some(&*tracker))?;
            }
            Ok(((), 0, snaps.into_inv()))
        })?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_audit::AuditEntry;
    use rusqlite::params;

    fn level(m: &Model, id: i64, name: &str) {
        m.sql()
            .execute(
                "INSERT INTO level(id,name,elevation) VALUES (?1,?2,0)",
                params![id, name],
            )
            .unwrap();
    }

    fn raw_op(m: &Model, kind: &str, inv: &str) -> i64 {
        m.sql()
            .execute(
                "INSERT INTO op(element,kind,inv) VALUES (0,?1,?2)",
                params![kind, inv],
            )
            .unwrap();
        m.sql().last_insert_rowid()
    }

    fn drop_inv(id: i64) -> String {
        format!(r#"[["DELETE FROM level WHERE id=?1",[{id}]]]"#)
    }

    fn mark_ai(m: &Model, seq: i64, text: &str) -> i64 {
        m.audit_append(&AuditEntry {
            session: "s".into(),
            actor: "chat".into(),
            event: "apply".into(),
            model: Some("m".into()),
            text: Some(text.into()),
            op_seq: Some(seq),
            status: "ok".into(),
            ..AuditEntry::default()
        })
        .unwrap()
    }

    fn names(m: &Model) -> Vec<String> {
        m.level_list()
            .unwrap()
            .into_iter()
            .map(|l| l.1)
            .collect()
    }

    #[test]
    fn statements_are_read_into_row_keys() {
        let i = |n: i64| vec![Value::Integer(n)];
        assert_eq!(
            parse_stmt("DELETE FROM level WHERE id=?1", &i(4)),
            ("level".into(), Key::Eq("id".into(), Value::Integer(4)))
        );
        assert_eq!(
            parse_stmt("DELETE FROM wall WHERE rowid>?1", &i(9)),
            ("wall".into(), Key::After(9))
        );
        assert_eq!(
            parse_stmt("INSERT OR REPLACE INTO wall(rowid,id,ax) VALUES (?1,?2,?3)", &i(7)),
            ("wall".into(), Key::Eq("rowid".into(), Value::Integer(7)))
        );
        assert_eq!(
            parse_stmt("UPDATE level SET name=?2 WHERE id=?1", &i(3)).1,
            Key::Eq("id".into(), Value::Integer(3))
        );
        assert_eq!(parse_stmt("DELETE FROM t", &[]).1, Key::Any);
        assert_eq!(parse_stmt("PRAGMA x", &[]).0, "*");
        assert!(!keys_overlap(
            &Key::Eq("id".into(), Value::Integer(1)),
            &Key::Eq("rowid".into(), Value::Integer(2))
        ));
        assert!(keys_overlap(&Key::After(5), &Key::Eq("id".into(), Value::Integer(6))));
        assert!(!keys_overlap(&Key::After(5), &Key::Eq("id".into(), Value::Integer(5))));
    }

    #[test]
    fn lists_ai_batches_with_title_why_and_state() {
        let mut m = Model::open(":memory:").unwrap();
        level(&m, 10, "AI");
        let s = raw_op(&m, "level_add", &drop_inv(10));
        let a = mark_ai(&m, s, "Add level\nthe brief asks for it");
        level(&m, 11, "U");
        raw_op(&m, "level_add", &drop_inv(11));
        let b = m.ai_batches().unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(
            (b[0].audit, b[0].op_seq, b[0].title.as_str()),
            (a, s, "Add level")
        );
        assert_eq!(b[0].why.as_deref(), Some("the brief asks for it"));
        assert!(!b[0].reverted);
        m.revert_ai(&[s]).unwrap();
        assert!(m.ai_batches().unwrap()[0].reverted);
        m.undo().unwrap();
        assert!(!m.ai_batches().unwrap()[0].reverted);
        m.undo().unwrap();
        m.undo().unwrap();
        assert!(m.ai_batches().unwrap().is_empty(), "undone batch is gone");
    }

    #[test]
    fn revert_ai_only_when_independent() {
        let mut m = Model::open(":memory:").unwrap();
        level(&m, 10, "AI");
        let s = raw_op(&m, "level_add", &drop_inv(10));
        mark_ai(&m, s, "Add level");
        let top = m.op_top().unwrap();
        // a person's operation is not an AI batch
        level(&m, 11, "U");
        let u = raw_op(&m, "level_add", &drop_inv(11));
        assert!(matches!(
            m.revert_ai(&[u]),
            Err(RevertError::Refused(Refusal::NotAiBatch(_)))
        ));
        assert_eq!(m.op_top().unwrap(), u);
        m.undo().unwrap();
        assert_eq!(m.op_top().unwrap(), top);
        assert_eq!(m.revert_ai(&[s]).unwrap(), 1);
        assert!(names(&m).is_empty());
        assert!(matches!(
            m.revert_ai(&[s]),
            Err(RevertError::Refused(Refusal::AlreadyReverted(_)))
        ));
        assert!(matches!(
            m.revert_ai(&[]),
            Err(RevertError::Refused(Refusal::Empty))
        ));
    }

    #[test]
    fn revert_with_an_edit_on_another_element_keeps_that_edit() {
        let mut m = Model::open(":memory:").unwrap();
        level(&m, 10, "AI");
        let s = raw_op(&m, "level_add", &drop_inv(10));
        mark_ai(&m, s, "Add level");
        level(&m, 11, "U");
        raw_op(&m, "level_add", &drop_inv(11));
        m.revert_ai(&[s]).unwrap();
        assert_eq!(names(&m), vec!["U".to_string()]);
        // one undo step brings the AI level back and leaves the person's level
        m.undo().unwrap();
        assert_eq!(names(&m), vec!["AI".to_string(), "U".to_string()]);
    }

    #[test]
    fn dependent_revert_is_refused_with_a_reason_and_changes_nothing() {
        let mut m = Model::open(":memory:").unwrap();
        level(&m, 10, "AI");
        let s = raw_op(&m, "level_add", &drop_inv(10));
        mark_ai(&m, s, "Add level");
        // a later person's edit renames the AI level
        m.sql()
            .execute("UPDATE level SET name='Mine' WHERE id=10", [])
            .unwrap();
        let u = raw_op(
            &m,
            "level_edit",
            r#"[["UPDATE level SET name=?2, elevation=?3 WHERE id=?1",[10,"AI",0.0]]]"#,
        );
        let audits = m.audit_list(0, 100).unwrap().len();
        let r = m.revert_ai(&[s]);
        match r {
            Err(RevertError::Refused(Refusal::Conflict {
                batch,
                with_op,
                table,
                row,
                ..
            })) => {
                assert_eq!((batch, with_op, table.as_str(), row), (s, u, "level", Some(10)));
                assert_eq!(m.conflict_row_name(&table, 10).as_deref(), Some("Mine"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(m.conflict_row_name("level", 999), None);
        assert_eq!(m.conflict_row_name("level; DROP TABLE level", 10), None);
        assert_eq!(m.conflict_row_name("wall", 1), None, "a table without a name column");
        assert_eq!(names(&m), vec!["Mine".to_string()]);
        assert_eq!(m.op_top().unwrap(), u);
        assert_eq!(m.audit_list(0, 100).unwrap().len(), audits);
        assert!(m.ai_revert_check(&[s]).is_err());
    }

    #[test]
    fn two_batches_revert_together_around_a_person_edit() {
        let mut m = Model::open(":memory:").unwrap();
        level(&m, 10, "A1");
        let s1 = raw_op(&m, "level_add", &drop_inv(10));
        mark_ai(&m, s1, "One");
        level(&m, 11, "U");
        raw_op(&m, "level_add", &drop_inv(11));
        level(&m, 12, "A2");
        let s2 = raw_op(&m, "level_add", &drop_inv(12));
        mark_ai(&m, s2, "Two");
        assert_eq!(m.revert_ai(&[s2, s1]).unwrap(), 2);
        assert_eq!(names(&m), vec!["U".to_string()]);
        m.undo().unwrap();
        assert_eq!(names(&m).len(), 3);
    }

    fn reverts(m: &Model) -> Vec<(i64, Option<String>)> {
        let mut st = m
            .sql()
            .prepare("SELECT op_seq, plan FROM ai_audit WHERE event='revert' ORDER BY seq")
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn revert_and_audit_one_transaction() {
        let mut m = Model::open(":memory:").unwrap();
        level(&m, 10, "AI");
        let s = raw_op(&m, "level_add", &drop_inv(10));
        let a = mark_ai(&m, s, "Add level");
        let top = m.op_top().unwrap();
        m.revert_ai(&[s]).unwrap();
        // exactly one new journal record, and the audit row names it
        assert_eq!(m.op_top().unwrap(), top + 1);
        let kind: String = m
            .sql()
            .query_row("SELECT kind FROM op WHERE seq=?1", [top + 1], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "ai_revert");
        assert_eq!(reverts(&m), vec![(top + 1, Some(format!("reverted={a}")))]);
        let row: i64 = m
            .sql()
            .query_row("SELECT max(seq) FROM ai_audit", [], |r| r.get(0))
            .unwrap();
        assert!(m.audit_op_live(row).unwrap());
        // undo of the revert takes its audit row with it: the batch is live again
        m.undo().unwrap();
        assert!(!m.audit_op_live(row).unwrap());
        assert!(!m.ai_batches().unwrap()[0].reverted);
        assert_eq!(names(&m), vec!["AI".to_string()]);
    }

    #[test]
    fn second_revert_of_batch_refused() {
        let mut m = Model::open(":memory:").unwrap();
        level(&m, 10, "A1");
        let s1 = raw_op(&m, "level_add", &drop_inv(10));
        mark_ai(&m, s1, "One");
        level(&m, 11, "A2");
        let s2 = raw_op(&m, "level_add", &drop_inv(11));
        mark_ai(&m, s2, "Two");
        m.revert_ai(&[s1]).unwrap();
        let top = m.op_top().unwrap();
        let audits = m.audit_list(0, 100).unwrap().len();
        // alone or together with a fresh batch, the reverted one is refused with a reason
        for seqs in [vec![s1], vec![s2, s1]] {
            match m.revert_ai(&seqs) {
                Err(RevertError::Refused(f)) => {
                    assert_eq!(f, Refusal::AlreadyReverted(s1));
                    assert!(f.reason().contains("already reverted"));
                }
                other => panic!("{other:?}"),
            }
            assert!(m.ai_revert_check(&seqs).is_err());
        }
        assert_eq!(m.op_top().unwrap(), top);
        assert_eq!(m.audit_list(0, 100).unwrap().len(), audits);
        assert_eq!(names(&m), vec!["A2".to_string()]);
        assert_eq!(reverts(&m).len(), 1);
        // once the revert is undone the batch may be reverted again
        m.undo().unwrap();
        assert_eq!(m.revert_ai(&[s1]).unwrap(), 1);
        assert_eq!(names(&m), vec!["A2".to_string()]);
    }

    fn stamp(m: &Model, seq: i64, actor: Actor, title: &str, why: &str) {
        let mut env = ddc_ops::Envelope::new(actor, title);
        env.why = Some(why.into());
        assert_eq!(m.op_top().unwrap(), seq);
        crate::journal::stamp_top(m.sql(), &crate::journal::Provenance::of(&env)).unwrap();
    }

    #[test]
    fn agent_envelope_listed_as_batch() {
        let m = Model::open(":memory:").unwrap();
        level(&m, 10, "A");
        let s1 = raw_op(&m, "level_add", &drop_inv(10));
        let agent = Actor::Agent {
            name: "planner".into(),
        };
        stamp(&m, s1, agent, "Add level", "agent brief");
        level(&m, 11, "B");
        let s2 = raw_op(&m, "level_add", &drop_inv(11));
        let ai = Actor::Ai {
            session: "s".into(),
            model: "m1".into(),
        };
        stamp(&m, s2, ai.clone(), "Chat level", "chat why");
        level(&m, 12, "C");
        let s3 = raw_op(&m, "level_add", &drop_inv(12));
        stamp(&m, s3, ai, "Audited", "x");
        let a3 = mark_ai(&m, s3, "Audited text\nfrom audit");
        let b = m.ai_batches().unwrap();
        assert_eq!(
            b.iter().map(|b| b.op_seq).collect::<Vec<_>>(),
            vec![s1, s2, s3],
            "one entry per operation, oldest first"
        );
        assert_eq!(
            (b[0].audit, b[0].actor.as_str(), b[0].model.as_deref()),
            (-s1, "planner", None)
        );
        assert_eq!(
            (b[0].title.as_str(), b[0].why.as_deref()),
            ("Add level", Some("agent brief"))
        );
        assert_eq!(
            (b[1].actor.as_str(), b[1].model.as_deref(), b[1].title.as_str()),
            ("user", Some("m1"), "Chat level")
        );
        // the audit record wins for an operation it names
        assert_eq!((b[2].audit, b[2].title.as_str()), (a3, "Audited text"));
        assert!(b.iter().all(|b| !b.reverted));
    }

    #[test]
    fn user_envelope_not_listed() {
        let m = Model::open(":memory:").unwrap();
        level(&m, 10, "U");
        let s1 = raw_op(&m, "level_add", &drop_inv(10));
        stamp(&m, s1, Actor::User, "Add level", "mine");
        level(&m, 11, "S");
        let s2 = raw_op(&m, "level_add", &drop_inv(11));
        stamp(&m, s2, Actor::Script { name: "x".into() }, "Script", "s");
        level(&m, 12, "E");
        let s3 = raw_op(&m, "level_add", &drop_inv(12));
        stamp(&m, s3, Actor::Xlsx, "Sheet", "e");
        // a row written before provenance (NULL actor) is not a batch either
        level(&m, 13, "Old");
        raw_op(&m, "level_add", &drop_inv(13));
        assert!(m.ai_batches().unwrap().is_empty());
        assert!(matches!(
            m.ai_revert_check(&[s1]),
            Err(RevertError::Refused(Refusal::NotAiBatch(_)))
        ));
    }

    #[test]
    fn envelope_batch_revert_restores_once() {
        let mut m = Model::open(":memory:").unwrap();
        level(&m, 10, "AI");
        let s = raw_op(&m, "level_add", &drop_inv(10));
        let agent = Actor::Agent { name: "opt".into() };
        stamp(&m, s, agent, "Add level", "option");
        level(&m, 11, "U");
        raw_op(&m, "level_add", &drop_inv(11));
        let top = m.op_top().unwrap();
        assert_eq!(m.revert_ai(&[s]).unwrap(), 1);
        assert_eq!(names(&m), vec!["U".to_string()]);
        // one journal record, and its audit row is written with it
        assert_eq!(m.op_top().unwrap(), top + 1);
        assert_eq!(reverts(&m), vec![(top + 1, Some(format!("reverted={}", -s)))]);
        assert!(m.ai_batches().unwrap()[0].reverted);
        assert!(matches!(
            m.revert_ai(&[s]),
            Err(RevertError::Refused(Refusal::AlreadyReverted(_)))
        ));
        assert_eq!(reverts(&m).len(), 1);
        // one undo step brings the level back exactly once and frees the batch
        m.undo().unwrap();
        assert_eq!(names(&m), vec!["AI".to_string(), "U".to_string()]);
        assert_eq!(m.op_top().unwrap(), top);
        let b = m.ai_batches().unwrap();
        assert_eq!((b.len(), b[0].reverted), (1, false));
    }

    #[test]
    fn failed_revert_writes_no_audit() {
        let mut m = Model::open(":memory:").unwrap();
        level(&m, 10, "AI");
        // a readable target whose inverse fails when it runs, after the audit row is written
        let s = raw_op(
            &m,
            "level_edit",
            r#"[["UPDATE level SET no_such_col=?2 WHERE id=?1",[10,"x"]]]"#,
        );
        mark_ai(&m, s, "Edit level");
        assert!(m.ai_revert_check(&[s]).is_ok());
        let top = m.op_top().unwrap();
        let audits = m.audit_list(0, 100).unwrap().len();
        assert!(matches!(m.revert_ai(&[s]), Err(RevertError::Db(_))));
        assert_eq!(m.op_top().unwrap(), top);
        assert_eq!(m.audit_list(0, 100).unwrap().len(), audits);
        assert!(reverts(&m).is_empty());
        assert!(!m.ai_batches().unwrap()[0].reverted);
        assert_eq!(names(&m), vec!["AI".to_string()]);
    }
}
