//! Journal provenance (schema 40): every `op` row keeps who made it and, for an edit that came
//! as an envelope, the forward ops next to the inverse SQL.
//! Undo reads only `inv`, so rows written before step 40 (no actor, no src) undo as before.
//! - `actor`: `ddc_ops::Actor` as JSON; NULL = written before step 40 (unknown).
//! - `title`, `why`: shown in the history without opening `src`.
//! - `src`: envelope JSON as TEXT up to `SRC_INLINE` bytes, a raw deflate BLOB above it.

use ddc_ops::{Actor, Envelope};
use rusqlite::{params, types::Value, Connection};

use crate::{Error, Model};

/// Envelope JSON longer than this is stored compressed.
pub(crate) const SRC_INLINE: usize = 4096;
/// Inflate limit: a damaged blob cannot make the history allocate without bound.
const SRC_MAX: usize = 64 << 20;
/// Actor of every row the model writes without a provenance (UI edits).
pub(crate) const USER: &str = r#"{"kind":"user"}"#;

const COLS: [(&str, &str); 4] = [
    ("actor", "TEXT"),
    ("title", "TEXT"),
    ("why", "TEXT"),
    ("src", "BLOB"),
];

/// Who made one journal entry and why; `envelope` is the forward edit when there is one.
#[derive(Clone, Debug, PartialEq)]
pub struct Provenance {
    pub actor: Actor,
    pub title: Option<String>,
    pub why: Option<String>,
    pub envelope: Option<Envelope>,
}

impl Provenance {
    /// Actor, title and why come from the envelope itself.
    pub fn of(env: &Envelope) -> Provenance {
        Provenance {
            actor: env.actor.clone(),
            title: Some(env.title.clone()),
            why: env.why.clone(),
            envelope: Some(env.clone()),
        }
    }

    /// An edit without forward ops (closure tools, imports not yet on envelopes).
    pub fn by(actor: Actor, title: &str) -> Provenance {
        Provenance {
            actor,
            title: Some(title.to_owned()),
            why: None,
            envelope: None,
        }
    }
}

/// Step 40: additive, by column existence; old rows keep NULL (unknown actor, no src).
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    for (c, ty) in COLS {
        let n: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info('op') WHERE name=?1",
            [c],
            |r| r.get(0),
        )?;
        if n == 0 {
            db.execute_batch(&format!("ALTER TABLE op ADD COLUMN {c} {ty}"))?;
        }
    }
    Ok(())
}

pub(crate) fn actor_json(a: &Actor) -> Result<String, Error> {
    Ok(serde_json::to_string(a)?)
}

/// An actor this build cannot read (a newer kind) lists as unknown, never fails the history.
pub(crate) fn parse_actor(s: Option<String>) -> Option<Actor> {
    s.and_then(|s| serde_json::from_str(&s).ok())
}

pub(crate) fn encode_src(env: &Envelope) -> Result<Value, Error> {
    let json = env
        .to_json()
        .map_err(|e| Error::Invalid(format!("journal src: {e}")))?;
    Ok(if json.len() > SRC_INLINE {
        Value::Blob(miniz_oxide::deflate::compress_to_vec(json.as_bytes(), 6))
    } else {
        Value::Text(json)
    })
}

pub(crate) fn decode_src(v: Value) -> Result<Option<String>, Error> {
    match v {
        Value::Null => Ok(None),
        Value::Text(s) => Ok(Some(s)),
        Value::Blob(b) => {
            let raw = miniz_oxide::inflate::decompress_to_vec_with_limit(&b, SRC_MAX)
                .map_err(|_| Error::Invalid("journal src: bad deflate data".into()))?;
            String::from_utf8(raw)
                .map(Some)
                .map_err(|_| Error::Invalid("journal src: not UTF-8".into()))
        }
        _ => Err(Error::Invalid("journal src: unexpected type".into())),
    }
}

/// Writes `p` onto the top journal row (the one an `atomic` just merged); returns its seq.
pub(crate) fn stamp_top(db: &Connection, p: &Provenance) -> Result<i64, Error> {
    let seq: i64 = db.query_row("SELECT coalesce(max(seq),0) FROM op", [], |r| r.get(0))?;
    if seq == 0 {
        return Err(Error::Invalid("provenance: no journal entry".into()));
    }
    let src = match &p.envelope {
        Some(e) => encode_src(e)?,
        None => Value::Null,
    };
    db.execute(
        "UPDATE op SET actor=?1, title=?2, why=?3, src=?4 WHERE seq=?5",
        params![actor_json(&p.actor)?, p.title, p.why, src, seq],
    )?;
    Ok(seq)
}

impl Model {
    /// The forward envelope of journal entry `seq`; `None` for edits made without one.
    pub fn journal_src(&self, seq: i64) -> Result<Option<Envelope>, Error> {
        let v: Value = match self
            .db
            .query_row("SELECT src FROM op WHERE seq=?1", [seq], |r| r.get(0))
        {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        match decode_src(v)? {
            Some(s) => Envelope::from_json(&s)
                .map(Some)
                .map_err(|e| Error::Invalid(format!("journal src: {e}"))),
            None => Ok(None),
        }
    }

    /// Like [`Model::atomic`], and the one journal entry records `prov`. Returns the result
    /// and the entry's seq (the `journal_seq` an audit row links to).
    pub fn atomic_with<T>(
        &mut self,
        kind: &str,
        prov: &Provenance,
        f: impl FnOnce(&mut Model) -> Result<T, Error>,
    ) -> Result<(T, i64), Error> {
        self.atomic_in(kind, Some(prov), f, |_, _| Ok(()))
    }
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
