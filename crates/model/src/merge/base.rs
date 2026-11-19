// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Finding the common base: project tag, journal comparison, rollback of the peer's file copy.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use super::Prepared;
use crate::{Error, Model};

/// Why the merge did not start; the UI shows its own simple message for each case.
#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    /// Different projects or no shared history.
    #[error("no common base")]
    NoCommonBase,
    #[error("file schema version {found} is newer than supported {supported}")]
    Newer { found: i64, supported: i64 },
    #[error("file is damaged: {0}")]
    Damaged(String),
    #[error(transparent)]
    Model(Error),
}

impl From<Error> for MergeError {
    fn from(e: Error) -> Self {
        match e {
            Error::NewerVersion { found, supported } => Self::Newer { found, supported },
            Error::Corrupt(s) => Self::Damaged(s),
            Error::Db(rusqlite::Error::SqliteFailure(f, m))
                if matches!(
                    f.code,
                    rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt
                ) =>
            {
                Self::Damaged(m.unwrap_or_else(|| f.to_string()))
            }
            e => Self::Model(e),
        }
    }
}

impl From<rusqlite::Error> for MergeError {
    fn from(e: rusqlite::Error) -> Self {
        Error::Db(e).into()
    }
}

const FNV_START: u64 = 0xcbf2_9ce4_8422_2325;

fn mix(mut h: u64, b: &[u8]) -> u64 {
    for x in b {
        h ^= u64::from(*x);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Journal entries: (seq, cumulative hash up to and including this entry). Equal hashes mean the same history.
fn chain(db: &Connection) -> Result<Vec<(i64, u64)>, Error> {
    let mut st =
        db.prepare("SELECT seq, element, kind, inv, COALESCE(stamp, 0) FROM main.op ORDER BY seq")?;
    let mut rows = st.query([])?;
    let mut out = vec![];
    let mut h = FNV_START;
    while let Some(r) = rows.next()? {
        let seq: i64 = r.get(0)?;
        let el: Option<i64> = r.get(1)?;
        h = mix(h, &seq.to_le_bytes());
        h = mix(h, &el.unwrap_or(0).to_le_bytes());
        h = mix(h, r.get_ref(2)?.as_bytes().unwrap_or_default());
        h = mix(h, r.get_ref(3)?.as_bytes().unwrap_or_default());
        h = mix(h, &r.get::<_, i64>(4)?.to_le_bytes());
        out.push((seq, h));
    }
    Ok(out)
}

fn uid(db: &Connection) -> Result<i64, Error> {
    Ok(db
        .query_row(
            "SELECT value FROM main.meta WHERE key='project_uid'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// What the background base search needs from the open model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lineage {
    uid: i64,
    chain: Vec<(i64, u64)>,
    /// (row, peer file seq, its hash) of past merges, newest first.
    peers: Vec<(i64, i64, i64)>,
}

impl Model {
    /// Project tag and journal of this model for [`prepare_merge`].
    pub fn merge_lineage(&self) -> Result<Lineage, Error> {
        let peers = self
            .db
            .prepare("SELECT id, their_seq, their_hash FROM main.merge_peer ORDER BY id DESC")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        Ok(Lineage {
            uid: uid(&self.db)?,
            chain: chain(&self.db)?,
            peers,
        })
    }
}

fn has_link(chain: &[(i64, u64)], seq: i64, hash: i64) -> bool {
    if seq == 0 {
        return hash == FNV_START as i64;
    }
    chain
        .binary_search_by_key(&seq, |x| x.0)
        .is_ok_and(|i| chain[i].1 as i64 == hash)
}

/// Copies the peer's file into `work` (in this build's schema), finds the common point and rolls
/// the copy back to it. The original file is not changed. Safe to call from a background thread.
pub fn prepare_merge(mine: &Lineage, source: &Path, work: &Path) -> Result<Prepared, MergeError> {
    std::fs::create_dir_all(work).map_err(Error::from)?;
    let their = work.join("their.ddc");
    let base = work.join("base.ddc");
    let m = Model::open_snapshot(source, &their)?;
    if mine.uid == 0 || uid(&m.db)? != mine.uid {
        return Err(MergeError::NoCommonBase);
    }
    let theirs = chain(&m.db)?;
    let (tip_seq, tip) = theirs.last().copied().unwrap_or((0, FNV_START));
    // an earlier merge with this history: the base is its state at that time
    let peer = mine
        .peers
        .iter()
        .find(|(_, s, h)| has_link(&theirs, *s, *h));
    let shared = mine
        .chain
        .iter()
        .zip(&theirs)
        .take_while(|(a, b)| a == b)
        .last()
        .map_or(0, |(a, _)| a.0);
    let base_seq = peer.map_or(shared, |p| p.1);
    let reverse: Vec<i64> =
        m.db.prepare("SELECT id, their_seq, their_hash FROM main.merge_peer")?
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|(_, s, h)| has_link(&mine.chain, *s, *h))
            .map(|x| x.0)
            .collect();
    m.snapshot_to(&base)?;
    drop(m);
    let mut db = Connection::open(&base)?;
    crate::ops::rewind(&mut db, base_seq)?;
    drop(db);
    Ok(Prepared {
        source: source.to_path_buf(),
        their,
        base,
        peer: peer.map(|p| p.0),
        base_seq,
        tip_seq,
        tip_hash: tip as i64,
        reverse,
        at: web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64),
    })
}
