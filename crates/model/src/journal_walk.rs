// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Journal walk behind the "Journal -> Replay" view: a copy of the model file on which operations
//! are undone from newest to oldest; each step reports which rows the undone operation touched.
//! Playback runs in the reverse order (from the first entry). The live model and its file are
//! never opened for writing: the copy is taken consistently (`backup_file`) and removed with the walk.

use std::path::{Path, PathBuf};

use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::{Changes, ElKey, Error, Model, RevealChange, RevealStep, PHASED};

/// One journal operation (or `stride` consecutive ones): the oldest sequence number, kind and touched rows.
#[derive(Clone, Debug, PartialEq)]
pub struct WalkStep {
    pub seq: i64,
    pub kind: String,
    pub changes: Changes,
    /// Touched elements with the step outcome when playing forward: appeared, disappeared or changed.
    pub steps: Vec<RevealStep>,
}

pub struct JournalWalk {
    // Option: the connections close before the files are removed (Windows cannot delete open files)
    m: Option<Model>,
    /// The "now" state, read-only: it tells whether elements existed before the first undo.
    head: Option<Connection>,
    /// Whether the element exists in the state from which the next undo is made.
    alive: HashMap<ElKey, bool>,
    paths: [PathBuf; 2],
    stride: usize,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl JournalWalk {
    /// The copy in the "now" state (before the first undo).
    pub fn model(&self) -> &Model {
        self.m.as_ref().expect("open until drop")
    }

    /// Cancel flag, checked before every step; a cancelled walk yields an error.
    pub fn with_cancel(mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }
}

impl Drop for JournalWalk {
    fn drop(&mut self) {
        self.head = None;
        self.m = None;
        for p in &self.paths {
            crate::remove_db_files(p);
        }
    }
}

fn exists(db: &Connection, table: &str, id: i64) -> Result<bool, Error> {
    Ok(db
        .query_row(&format!("SELECT 1 FROM \"{table}\" WHERE rowid=?1"), [id], |_| Ok(()))
        .optional()?
        .is_some())
}

impl Iterator for JournalWalk {
    type Item = Result<WalkStep, Error>;

    /// Undoes `stride` operations (at least one) and returns their combined trace. The end is an empty journal.
    fn next(&mut self) -> Option<Self::Item> {
        let go = |w: &mut Self| -> Result<Option<WalkStep>, Error> {
            if w.cancel.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
                return Err(Error::Invalid("cancelled".into()));
            }
            let head = w.m.as_ref().unwrap().change_head();
            let mut last = None;
            // element -> (existed before undoing the newest operation, exists after the oldest)
            let mut net: BTreeMap<ElKey, (bool, bool)> = BTreeMap::new();
            for _ in 0..w.stride.max(1) {
                let seq = w.m.as_ref().unwrap().op_top()?;
                if seq == 0 {
                    break;
                }
                let kind: String = w
                    .m
                    .as_ref()
                    .unwrap()
                    .sql()
                    .query_row("SELECT kind FROM op WHERE seq=?1", [seq], |r| r.get(0))?;
                let before = w.m.as_ref().unwrap().change_head();
                w.m.as_mut().unwrap().undo()?;
                let ch = w.m.as_ref().unwrap().changes_since(before);
                for t in PHASED {
                    for &id in ch.rows.get(t).into_iter().flatten() {
                        let old = exists(w.m.as_ref().unwrap().sql(), t, id)?;
                        let key = (t.to_string(), id);
                        let new = match w.alive.get(&key) {
                            Some(&b) => b,
                            // not touched by newer operations: same as in "now"
                            None => exists(w.head.as_ref().unwrap(), t, id)?,
                        };
                        w.alive.insert(key.clone(), old);
                        net.entry(key).and_modify(|e| e.1 = old).or_insert((new, old));
                    }
                }
                last = Some((seq, kind));
            }
            let steps = net
                .into_iter()
                .filter_map(|(k, (new, old))| {
                    let change = match (old, new) {
                        (false, true) => RevealChange::Added,
                        (true, false) => RevealChange::Removed,
                        (true, true) => RevealChange::Modified,
                        (false, false) => return None,
                    };
                    Some(RevealStep::new(k, change))
                })
                .collect();
            Ok(last.map(|(seq, kind)| WalkStep {
                seq,
                kind,
                changes: w.m.as_ref().unwrap().changes_since(head),
                steps,
            }))
        };
        go(self).transpose()
    }
}

impl Model {
    /// Walks the journal of file `src` on copies in `scratch` (`scratch` and `scratch.walk`);
    /// `progress(done, total)` counts copied pages, `false` cancels (an error, no copies left).
    /// `stride` is how many operations make one step.
    pub fn journal_walk(
        src: &Path,
        scratch: &Path,
        stride: usize,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<JournalWalk, Error> {
        let mut walk = scratch.as_os_str().to_owned();
        walk.push(".walk");
        let paths = [scratch.to_path_buf(), PathBuf::from(walk)];
        for p in &paths {
            crate::remove_db_files(p);
        }
        let built = (|| {
            Model::backup_file(src, scratch, progress)?;
            std::fs::copy(scratch, &paths[1])?;
            let p = paths[1].to_str().ok_or_else(|| Error::Invalid("non-UTF-8 path".into()))?;
            let m = Model::open_copy(p)?;
            let head = Connection::open_with_flags(scratch, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            Ok::<_, Error>((m, head))
        })();
        match built {
            Ok((m, head)) => Ok(JournalWalk { m: Some(m), head: Some(head), alive: HashMap::new(), paths, stride, cancel: None }),
            Err(e) => {
                for p in &paths {
                    crate::remove_db_files(p);
                }
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc_jw_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn live(d: &Path, n: usize) -> (Model, PathBuf) {
        let p = d.join("live.ddc");
        let mut m = Model::open(p.to_str().unwrap()).unwrap();
        for i in 0..n {
            m.add_level(&format!("L{i}"), 3.0 * i as f64).unwrap();
        }
        (m, p)
    }

    fn go(_: u64, _: u64) -> bool {
        true
    }

    #[test]
    fn journal_replay_end_equals_live_model() {
        let d = dir("end");
        let (m, p) = live(&d, 6);
        let want = m.content_hash().unwrap();
        let w = Model::journal_walk(&p, &d.join("c.ddc"), 1, &mut go).unwrap();
        assert_eq!(w.model().content_hash().unwrap(), want, "first state = live model");
        let steps: Vec<_> = w.map(|s| s.unwrap()).collect();
        assert_eq!(steps.len(), 6, "one step per operation");
        let seqs: Vec<i64> = steps.iter().map(|s| s.seq).collect();
        assert!(seqs.windows(2).all(|w| w[0] > w[1]), "newest to oldest: {seqs:?}");
        assert!(steps.iter().all(|s| s.kind == "level_add" && !s.changes.is_empty()), "{steps:?}");
        assert!(steps.iter().all(|s| s.steps.is_empty()), "a level is not a reveal element");
        assert!(!d.join("c.ddc").exists() && !d.join("c.ddc.walk").exists(), "the copy is removed together with the walk");
    }

    #[test]
    fn journal_walk_stride_groups_operations() {
        let d = dir("stride");
        let (_m, p) = live(&d, 7);
        let w = Model::journal_walk(&p, &d.join("c.ddc"), 3, &mut go).unwrap();
        let n: Vec<_> = w.map(|s| s.unwrap().seq).collect();
        assert_eq!(n.len(), 3, "7 operations in batches of 3: 3+3+1");
    }

    #[test]
    fn journal_replay_does_not_touch_live_db() {
        let d = dir("live");
        let (m, p) = live(&d, 5);
        let (h, top, n) = (m.content_hash().unwrap(), m.op_top().unwrap(), m.journal_len().unwrap());
        let w = Model::journal_walk(&p, &d.join("c.ddc"), 1, &mut go).unwrap();
        let end = w.count();
        assert_eq!(end, 5);
        assert_eq!((m.content_hash().unwrap(), m.op_top().unwrap(), m.journal_len().unwrap()), (h, top, n));
        // the file can be reopened: same journal
        drop(m);
        let again = Model::open(p.to_str().unwrap()).unwrap();
        assert_eq!((again.content_hash().unwrap(), again.journal_len().unwrap()), (h, n));
        let cancelled = Model::journal_walk(&p, &d.join("x.ddc"), 1, &mut |_, _| false);
        assert!(cancelled.is_err() && !d.join("x.ddc").exists());
    }

    #[test]
    fn walk_classifies_elements_for_forward_replay() {
        use crate::{LayerSpec, WallSpec};
        let d = dir("cls");
        let p = d.join("live.ddc");
        let mut m = Model::open(p.to_str().unwrap()).unwrap();
        let lv = m.add_level("L0", 0.0).unwrap();
        let wt = m
            .add_wall_type("w", &[LayerSpec { material: "m".into(), thickness: 0.2 }])
            .unwrap();
        let spec = |x: f64| WallSpec {
            a: (x, 0.0),
            b: (x, 4.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        };
        let w1 = m.add_wall(&spec(0.0)).unwrap();
        let w2 = m.add_wall(&spec(5.0)).unwrap();
        m.delete_wall(w1).unwrap();
        let w = Model::journal_walk(&p, &d.join("c.ddc"), 1, &mut go).unwrap();
        let steps: Vec<_> = w.map(|s| s.unwrap()).collect();
        let wall = |s: &WalkStep| -> Vec<(i64, RevealChange)> {
            s.steps.iter().filter(|x| x.key.0 == "wall").map(|x| (x.key.1, x.change)).collect()
        };
        assert_eq!(wall(&steps[0]), vec![(w1, RevealChange::Removed)]);
        assert_eq!(wall(&steps[1]), vec![(w2, RevealChange::Added)]);
        assert_eq!(wall(&steps[2]), vec![(w1, RevealChange::Added)]);
        assert!(wall(&steps[3]).is_empty() && wall(&steps[4]).is_empty());
    }

    /// Model with walls, openings and slabs; the content hash is recorded after each operation.
    fn rich(m: &mut Model, base_walls: usize, ops: usize) -> Vec<u64> {
        use crate::{LayerSpec, OpeningSpec, SlabSpec, WallSpec};
        let lv = m.add_level("L0", 0.0).unwrap();
        let wt = m.add_wall_type("w", &[LayerSpec { material: "m".into(), thickness: 0.2 }]).unwrap();
        let st = m.add_slab_type("s", &[LayerSpec { material: "c".into(), thickness: 0.25 }]).unwrap();
        let spec = |i: usize| WallSpec {
            a: ((i % 100) as f64 * 5.0, (i / 100) as f64 * 5.0),
            b: ((i % 100) as f64 * 5.0 + 4.0, (i / 100) as f64 * 5.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        };
        let specs: Vec<WallSpec> = (0..base_walls).map(spec).collect();
        let ids = m.add_walls(&specs).unwrap();
        let mut hashes = vec![];
        let mut extra = vec![];
        for k in 0..ops {
            match k % 5 {
                0 => extra.push(m.add_wall(&spec(base_walls + k)).unwrap()),
                1 => {
                    let o = OpeningSpec { along: 1.0, width: 0.9, sill: 0.0, height: 2.1, kind: "door".into() };
                    m.add_opening(ids[k % ids.len()], &o).unwrap();
                }
                2 => {
                    let x = 1000.0 + k as f64 * 12.0;
                    m.add_slab(&SlabSpec {
                        level: lv,
                        slab_type: st,
                        offset: 0.0,
                        outer: vec![(x, 0.0), (x + 10.0, 0.0), (x + 10.0, 8.0), (x, 8.0)],
                        holes: vec![],
                    })
                    .unwrap();
                }
                3 => m.delete_wall(extra.pop().unwrap_or(ids[k % ids.len()])).unwrap(),
                _ => m.set_wall_bulge(ids[(k * 7) % ids.len()], 0.1).unwrap(),
            }
            hashes.push(m.content_hash().unwrap());
        }
        hashes
    }

    #[test]
    fn walk_rewinds_walls_slabs_openings_and_hashes_match_every_step() {
        let d = dir("rich");
        let p = d.join("live.ddc");
        let mut m = Model::open(p.to_str().unwrap()).unwrap();
        let base_ops = m.journal_len().unwrap();
        let hashes = rich(&mut m, 30, 40);
        let n = m.journal_len().unwrap() - base_ops;
        assert!(n >= 40);
        let mut w = Model::journal_walk(&p, &d.join("c.ddc"), 1, &mut go).unwrap();
        assert_eq!(w.model().content_hash().unwrap(), m.content_hash().unwrap());
        let total = m.journal_len().unwrap();
        let mut i = 0;
        let mut kinds = std::collections::BTreeSet::new();
        while let Some(step) = w.next() {
            let step = step.unwrap();
            kinds.insert(step.kind.clone());
            i += 1;
            // after undoing the k-th operation from the end, the content equals the one recorded after the (k+1)-th from the end
            if i <= hashes.len() - 1 {
                assert_eq!(
                    w.model().content_hash().unwrap(),
                    hashes[hashes.len() - 1 - i],
                    "step {i} ({})",
                    step.kind
                );
            }
        }
        assert_eq!(i, total, "one step per journal record");
        assert!(["wall_add", "wall_opening_add", "slab_add"].iter().all(|k| kinds.contains(*k)), "{kinds:?}");
    }

    /// Budget: a 500-operation journal on a 10k-wall model, timeline <= 3 s, first step <= 300 ms.
    /// The threshold is 25x looser in debug builds.
    #[test]
    fn journal_walk_budget_500_ops_on_10k_walls() {
        let d = dir("perf");
        let p = d.join("live.ddc");
        let mut m = Model::open(p.to_str().unwrap()).unwrap();
        rich(&mut m, 10_000, 500);
        let k = if cfg!(debug_assertions) { 25.0 } else { 1.0 };
        let t = std::time::Instant::now();
        let mut w = Model::journal_walk(&p, &d.join("c.ddc"), 1, &mut go).unwrap();
        let first = w.next().unwrap().unwrap();
        let first_ms = t.elapsed().as_secs_f64() * 1000.0;
        let n = 1 + w.map(|s| s.unwrap()).count();
        let all = t.elapsed().as_secs_f64();
        eprintln!("journal_walk: {n} steps, first {first_ms:.0} ms, total {all:.2} s ({:?})", first.kind);
        assert!(n >= 500);
        assert!(first_ms <= 300.0 * k, "first step {first_ms:.0} ms");
        assert!(all <= 3.0 * k, "whole scale {all:.2} s");
    }

    #[test]
    fn cancelled_walk_stops_and_cleans_up() {
        use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc};
        let d = dir("cancel");
        let (_m, p) = live(&d, 5);
        let flag = Arc::new(AtomicBool::new(false));
        let mut w = Model::journal_walk(&p, &d.join("c.ddc"), 1, &mut go).unwrap().with_cancel(flag.clone());
        assert!(w.next().unwrap().is_ok());
        flag.store(true, Ordering::Relaxed);
        assert!(w.next().unwrap().is_err(), "cancelled before the next step");
        drop(w);
        assert!(!d.join("c.ddc").exists() && !d.join("c.ddc.walk").exists());
    }
}
