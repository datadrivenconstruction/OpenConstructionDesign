// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One view range per plan. The four-plane range of the drawing quality window is a view of the
//! plan's stored view range (`view_gfx`, template aware, one undo step per change); ranges that
//! older builds saved under `range:<level>` in `drawq_style` move there on open.

use rusqlite::{params, Connection};

use super::ViewRange;
use crate::phases::ViewKey;
use crate::view_gfx::{state, write_view};
use crate::Error;

type PlanRange = crate::ViewRange;

/// Key prefix of a range saved by older builds.
const LEGACY: &str = "range:";

/// A migrated legacy range is kept under this key: never lost, never moved twice.
fn kept_key(level: i64) -> String {
    format!("legacy_range:{level}")
}

impl ViewRange {
    /// The stored form: the depth plane becomes a span below the bottom, an infinite top no top.
    pub fn to_plan(self) -> PlanRange {
        PlanRange {
            cut: self.cut,
            bottom: self.bottom,
            depth: (self.bottom - self.depth).max(0.0),
            top: self.top.is_finite().then_some(self.top),
        }
    }

    /// Four planes of a stored range; no top reads as an infinite one.
    pub fn from_plan(r: PlanRange) -> Self {
        Self {
            top: r.top.unwrap_or(f64::INFINITY),
            cut: r.cut,
            bottom: r.bottom,
            depth: r.bottom - r.depth,
        }
    }
}

/// On open: each legacy row becomes the plan's view range unless the plan already holds a range
/// of its own, then the row is renamed to [`kept_key`]. No legacy row, no write.
pub(crate) fn migrate_legacy_range(db: &Connection) -> Result<(), Error> {
    let has: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='drawq_style'",
        [],
        |r| r.get(0),
    )?;
    if has == 0 {
        return Ok(());
    }
    let rows: Vec<(String, String)> = {
        let mut st = db.prepare("SELECT key, spec FROM drawq_style WHERE key LIKE 'range:%'")?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        v
    };
    for (key, spec) in rows {
        let Some(level) = key.strip_prefix(LEGACY).and_then(|s| s.parse::<i64>().ok()) else {
            continue;
        };
        let legacy = serde_json::from_str(&spec)
            .ok()
            .and_then(|j| ViewRange::from_json(&j));
        if let Some(mut l) = legacy {
            // the stored form needs the cut strictly above the bottom
            if l.cut <= l.bottom {
                l.bottom = l.cut - 0.001;
            }
            let r = l.to_plan();
            let v = ViewKey::Plan(level);
            let st = state(db, v)?;
            if r.issue().is_none() && st.own.range == PlanRange::default() {
                let mut own = st.own;
                own.range = r;
                write_view(db, v, st.template, &own)?;
            }
        }
        db.execute(
            "UPDATE OR REPLACE drawq_style SET key=?1 WHERE key=?2",
            params![kept_key(level), key],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::{LineClass, ViewPrim};
    use crate::{LayerSpec, Model, WallSpec};

    fn legacy() -> ViewRange {
        ViewRange {
            top: 3.0,
            cut: 1.25,
            bottom: 0.5,
            depth: 0.25,
        }
    }

    /// A level with parallel walls (base_offset, height) and a legacy range row.
    fn old_project(zs: &[(f64, f64)]) -> (Model, i64, Vec<i64>) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let wt = m
            .add_wall_type(
                "W",
                &[LayerSpec {
                    material: "Brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let specs: Vec<WallSpec> = zs
            .iter()
            .enumerate()
            .map(|(i, (b, h))| WallSpec {
                a: (0.0, 2.0 * i as f64),
                b: (5.0, 2.0 * i as f64),
                level: lv,
                base_offset: *b,
                height: *h,
                wall_type: wt,
                offset0: None,
            })
            .collect();
        let ids = m.add_walls(&specs).unwrap();
        m.db.execute_batch(super::super::DDL).unwrap();
        m.db.execute(
            "INSERT INTO drawq_style(key, spec) VALUES (?1, ?2)",
            params![format!("range:{lv}"), legacy().to_json().to_string()],
        )
        .unwrap();
        (m, lv, ids)
    }

    fn reopen(m: &Model, name: &str) -> (Model, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("ddc_vru_{name}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.ddc");
        let _ = std::fs::remove_file(&path);
        m.backup_to(&path).unwrap();
        (Model::open(path.to_str().unwrap()).unwrap(), dir)
    }

    fn keys(m: &Model) -> Vec<String> {
        let mut st = m
            .db
            .prepare("SELECT key FROM drawq_style WHERE key LIKE '%range:%' ORDER BY key")
            .unwrap();
        let v = st.query_map([], |r| r.get(0)).unwrap();
        v.map(Result::unwrap).collect()
    }

    fn wall_classes(m: &Model, lv: i64, id: i64) -> Vec<LineClass> {
        let v = m.plan_view_ranged(lv, vec![]).unwrap();
        let mut c: Vec<LineClass> = v
            .prims
            .iter()
            .filter_map(|p| match p {
                ViewPrim::Edge {
                    table: "wall",
                    element,
                    class,
                    ..
                } if *element == id => Some(*class),
                _ => None,
            })
            .collect();
        c.dedup();
        c
    }

    #[test]
    fn conversion_round_trips() {
        let l = legacy();
        let p = l.to_plan();
        let want = PlanRange {
            cut: 1.25,
            bottom: 0.5,
            depth: 0.25,
            top: Some(3.0),
        };
        assert_eq!(p, want);
        assert_eq!(ViewRange::from_plan(p), l);
        // no top in the stored range is an open top here, and back
        let open = ViewRange::from_plan(PlanRange::default());
        assert!(open.check().is_ok());
        assert_eq!(open.top, f64::INFINITY);
        assert_eq!(open.to_plan(), PlanRange::default());
    }

    #[test]
    fn old_project_opens_with_the_same_range() {
        // through the cut, overhead, seen, beyond, below the depth, above the top
        let (m, lv, ids) = old_project(&[
            (0.0, 3.0),
            (2.0, 0.5),
            (0.0, 1.0),
            (0.0, 0.375),
            (0.0, 0.125),
            (3.5, 1.0),
        ]);
        let (re, dir) = reopen(&m, "open");
        assert_eq!(re.plan_range(lv).unwrap(), legacy());
        assert_eq!(re.plan_view_range(lv), legacy().to_plan(), "one stored range");
        assert_eq!(re.plan_range_saved(lv).unwrap(), Some(legacy()));
        assert_eq!(wall_classes(&re, lv, ids[1]), vec![LineClass::Overhead]);
        assert_eq!(wall_classes(&re, lv, ids[2]), vec![LineClass::Projection]);
        assert_eq!(wall_classes(&re, lv, ids[3]), vec![LineClass::Beyond]);
        assert!(wall_classes(&re, lv, ids[4]).is_empty(), "below the depth");
        assert!(wall_classes(&re, lv, ids[5]).is_empty(), "above the top");
        // the old row is kept under a new key, nothing is lost
        assert_eq!(keys(&re), vec![format!("legacy_range:{lv}")]);
        drop(re);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_runs_once_and_keeps_newer_ranges() {
        let (mut m, lv, _) = old_project(&[(0.0, 3.0)]);
        let l2 = m.add_level("L2", 3.0).unwrap();
        m.db.execute(
            "INSERT INTO drawq_style(key, spec) VALUES (?1, ?2)",
            params![format!("range:{l2}"), legacy().to_json().to_string()],
        )
        .unwrap();
        // L2 already holds a range set in the view graphics window: it wins
        let newer = PlanRange {
            cut: 0.75,
            bottom: 0.0,
            depth: 0.5,
            top: None,
        };
        m.set_view_range(ViewKey::Plan(l2), newer).unwrap();
        let (mut re, dir) = reopen(&m, "once");
        assert_eq!(re.plan_view_range(l2), newer);
        assert_eq!(re.plan_view_range(lv), legacy().to_plan());
        // a reset after the move stays a reset on the next open
        re.reset_plan_range(lv).unwrap();
        let (again, dir2) = reopen(&re, "once2");
        assert_eq!(again.plan_view_range(lv), PlanRange::default());
        assert_eq!(again.plan_range_saved(lv).unwrap(), None);
        assert_eq!(keys(&again).len(), 2);
        drop((re, again));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn plain_project_open_writes_nothing() {
        let m = Model::open(":memory:").unwrap();
        let before = m.db.total_changes();
        migrate_legacy_range(&m.db).unwrap();
        assert_eq!(m.db.total_changes(), before);
    }

    #[test]
    fn set_here_is_one_undo_step_and_seen_everywhere() {
        let (mut m, lv, _) = old_project(&[(0.0, 3.0)]);
        migrate_legacy_range(&m.db).unwrap();
        let r = ViewRange {
            top: 2.5,
            cut: 1.0,
            bottom: 0.0,
            depth: -0.5,
        };
        m.set_plan_range(lv, r).unwrap();
        assert_eq!(m.plan_view_range(lv), r.to_plan(), "the view graphics range follows");
        assert_eq!(m.plan_range(lv).unwrap(), r);
        m.undo().unwrap();
        assert_eq!(m.plan_view_range(lv), legacy().to_plan(), "one undo step");
        // equal cut and bottom cannot be stored: refused, nothing written
        let flat = ViewRange {
            top: 2.0,
            cut: 1.0,
            bottom: 1.0,
            depth: 0.0,
        };
        assert!(m.set_plan_range(lv, flat).is_err());
        assert_eq!(m.plan_range(lv).unwrap(), legacy());
    }
}
