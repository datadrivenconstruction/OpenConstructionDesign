// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Named marks on the change journal, and the findings that stood at each: `run_checks` with
//! `since` then tells only what the changes after the mark brought, and what they cleared.

use ddc_ai::refusal::{self, RefusalCode};
use ddc_model::checks::Finding;
use ddc_model::{ElKey, Model};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

/// Marks of one session; the oldest gives way.
pub const MAX_POINTS: usize = ddc_ai::limits::CHECKPOINTS.us();
/// Finding sets kept besides the marks' own (full runs): bounded the same way.
const MAX_LEDGER: usize = ddc_ai::limits::CHECKPOINT_LEDGER.us();

/// A finding's identity: rule, message and the elements it names.
pub type Print = HashMap<String, Vec<ElKey>>;

pub fn fingerprint(f: &Finding) -> String {
    let mut els: Vec<String> = f.elements.iter().map(|e| format!("{}:{}", e.0, e.1)).collect();
    els.sort();
    format!("{}|{}|{}", f.rule, f.key(), els.join(","))
}

pub fn print_of(found: &[Finding]) -> Print {
    found.iter().map(|f| (fingerprint(f), f.elements.clone())).collect()
}

pub fn token_text(head: u64) -> String {
    format!("c:{head}")
}

#[derive(Default)]
pub struct Checkpoints {
    points: VecDeque<(String, u64)>,
    ledger: BTreeMap<u64, Print>,
}

/// What `since` stands for: the journal head, the elements changed after it, and the findings
/// that stood there when they were recorded.
pub struct Since {
    pub head: u64,
    pub changed: Vec<ElKey>,
    pub baseline: Option<Print>,
}

impl Checkpoints {
    pub fn mark(&mut self, name: &str, head: u64, print: Print) {
        self.points.retain(|p| p.0 != name);
        self.points.push_back((name.to_string(), head));
        while self.points.len() > MAX_POINTS {
            self.points.pop_front();
        }
        self.record(head, print);
    }

    /// The findings that stand at `head`, for a later `since`.
    pub fn record(&mut self, head: u64, print: Print) {
        self.ledger.insert(head, print);
        while self.ledger.len() > MAX_LEDGER {
            let kept: Vec<u64> = self.points.iter().map(|p| p.1).collect();
            // the oldest head no mark names; if every one is named, the oldest of all
            let gone = self.ledger.keys().copied().find(|h| !kept.contains(h));
            match gone.or_else(|| self.ledger.keys().next().copied()) {
                Some(h) => self.ledger.remove(&h),
                None => break,
            };
        }
    }

    pub fn names(&self) -> Vec<&str> {
        self.points.iter().map(|p| p.0.as_str()).collect()
    }

    fn head_of(&self, since: &str) -> Result<u64, String> {
        if let Some(n) = since.strip_prefix("c:") {
            return n.parse().map_err(|_| bad_since(since));
        }
        self.points
            .iter()
            .find(|p| p.0 == since)
            .map(|p| p.1)
            .ok_or_else(|| bad_since(since))
    }

    pub fn since(&self, m: &Model, since: &str) -> Result<Since, String> {
        let head = self.head_of(since)?;
        let ch = m.changes_since(head);
        if ch.all {
            return Err(expired(since));
        }
        let mut set: BTreeSet<ElKey> = ch
            .rows
            .iter()
            .filter(|(t, _)| super::rows::ELEMENT_TABLES.contains(&t.as_str()) || t.as_str() == "grid")
            .flat_map(|(t, ids)| ids.iter().map(move |i| (t.clone(), *i)))
            .collect();
        add_neighbours(m, &mut set);
        let changed: Vec<ElKey> = set.into_iter().collect();
        Ok(Since {
            head,
            changed,
            baseline: self.ledger.get(&head).cloned(),
        })
    }
}

/// A change reaches what hangs on it and what meets it: the host wall of an opening, the
/// openings of a wall, and the walls that join it at an end.
fn add_neighbours(m: &Model, set: &mut BTreeSet<ElKey>) {
    let db = m.sql();
    let mut add: Vec<ElKey> = vec![];
    let host = db.prepare("SELECT wall FROM opening WHERE id = ?1");
    let holes = db.prepare("SELECT id FROM opening WHERE wall = ?1");
    let ends = db.prepare("SELECT ax, ay, bx, by, level FROM wall WHERE id = ?1");
    let meet = db.prepare(
        "SELECT id FROM wall WHERE id <> ?1 AND level = ?2 AND          ((abs(ax-?3) < 1e-6 AND abs(ay-?4) < 1e-6) OR (abs(bx-?3) < 1e-6 AND abs(by-?4) < 1e-6))",
    );
    let (Ok(mut host), Ok(mut holes), Ok(mut ends), Ok(mut meet)) = (host, holes, ends, meet) else {
        return;
    };
    for (t, id) in set.iter() {
        match t.as_str() {
            "opening" => {
                if let Ok(w) = host.query_row([id], |r| r.get::<_, i64>(0)) {
                    add.push(("wall".to_string(), w));
                }
            }
            "wall" => {
                if let Ok(rows) = holes.query_map([id], |r| r.get::<_, i64>(0)) {
                    add.extend(rows.flatten().map(|o| ("opening".to_string(), o)));
                }
                let Ok((ax, ay, bx, by, level)) = ends.query_row([id], |r| {
                    Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?, r.get::<_, i64>(4)?))
                }) else {
                    continue;
                };
                for (x, y) in [(ax, ay), (bx, by)] {
                    if let Ok(rows) = meet.query_map(rusqlite::params![id, level, x, y], |r| r.get::<_, i64>(0)) {
                        add.extend(rows.flatten().map(|w| ("wall".to_string(), w)));
                    }
                }
            }
            _ => {}
        }
    }
    set.extend(add);
}

fn bad_since(since: &str) -> String {
    refusal::wire(
        &refusal::of(RefusalCode::InvalidArgs)
            .arg("since", since)
            .detail("since is a name given to mark_checkpoint or c:<number>"),
    )
}

fn expired(since: &str) -> String {
    refusal::wire(
        &refusal::of(RefusalCode::SinceExpired)
            .arg("since", since)
            .fix(refusal::call_tool("run_checks", vec![])),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::WallSpec;

    #[test]
    fn the_scope_of_a_change_reaches_its_host_and_its_joint_neighbours() {
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        let (level, wall_type) = (ed.level, ed.types[0].id);
        let mut wall = |a, b| {
            ed.model
                .add_wall(&WallSpec { a, b, level, base_offset: 0.0, height: 3.0, wall_type, offset0: None })
                .unwrap()
        };
        let (w1, w2, far) = (wall((0.0, 0.0), (5.0, 0.0)), wall((5.0, 0.0), (5.0, 4.0)), wall((20.0, 0.0), (25.0, 0.0)));
        let mut set: BTreeSet<ElKey> = [("wall".to_string(), w1)].into();
        add_neighbours(&ed.model, &mut set);
        assert!(set.contains(&("wall".to_string(), w2)), "the wall that joins at an end: {set:?}");
        assert!(!set.contains(&("wall".to_string(), far)), "{set:?}");
    }

    #[test]
    fn the_oldest_mark_gives_way_and_a_name_moves() {
        let mut c = Checkpoints::default();
        for i in 0..=MAX_POINTS as u64 {
            c.mark(&format!("m{i}"), i, Print::new());
        }
        assert_eq!(c.names().len(), MAX_POINTS);
        assert!(c.head_of("m0").is_err(), "the oldest was evicted");
        assert_eq!(c.head_of("m1"), Ok(1));
        c.mark("m1", 99, Print::new());
        assert_eq!(c.head_of("m1"), Ok(99));
        assert_eq!(c.names().len(), MAX_POINTS);
        assert_eq!(c.head_of("c:7"), Ok(7));
        assert!(c.head_of("c:x").is_err());
    }
}
