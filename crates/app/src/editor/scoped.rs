//! Scoped reload after an edit (`run_op`, undo, redo). When the model journal shows only rows of
//! walls that stay straight on the current level (plus wall and room quantities, properties), the
//! wall list, plan, snap and host lists are rebuilt for those walls and for the walls whose joins
//! they reach; the lists that do not hang on single wall rows load as in `reload`. Anything else,
//! or a plan that was not one straight join, takes the full `reload`. The lists are the same
//! either way (tests below compare them). Edits of openings, slabs and rooms without a wall row
//! go to `scoped_more`. A scoped reload that fails, also after it patched some lists, falls back
//! to the full reload (`reload_since`); only an error of that full reload is reported.

use super::{wall_footprint, wall_row, Editor, Wall, WALL_SQL};
use crate::snap::SnapWall;
use ddc_geom::{Tol, V2};
use ddc_model::Model;
use rusqlite::OptionalExtension;
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// Tables the scoped wall reload follows; others go to `scoped_more::FOLLOWED_MORE` or take
/// the full reload. `room_qty`: a wall edit regenerates the rooms of its level, and
/// `reload_rest` reloads rooms and tags whole, so a level with a room keeps the scoped path.
pub(super) const FOLLOWED: &[&str] =
    &["wall", "wall_qty", "room_qty", "dim_geo", "prop", "pparam_val"];

/// Margin of a wall box, far above the join tolerance: every wall that can share a join node or
/// a T-join with an edited wall touches its padded box.
const PAD: f64 = 0.01;

#[cfg(test)]
thread_local! {
    static HITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static FALLBACKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static FAULT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Failed scoped reloads of this thread that fell back to the full reload.
#[cfg(test)]
pub(crate) fn fallbacks() -> u64 {
    FALLBACKS.with(|h| h.get())
}

/// Test hook: the next scoped reload fails right after it patched its first lists.
#[cfg(test)]
pub(crate) fn inject_fault() {
    FAULT.with(|f| f.set(true));
}

/// The fault point of the scoped reloads: Err once after `inject_fault` (tests), else Ok.
pub(super) fn fault() -> Result<(), ddc_model::Error> {
    #[cfg(test)]
    {
        if FAULT.with(|f| f.replace(false)) {
            return Err(ddc_model::Error::Invalid("injected scoped reload fault".into()));
        }
    }
    Ok(())
}

/// Walls in a form join (only they can have a cut plan section); None if unknown.
pub(super) fn cut_walls(m: &Model) -> Option<HashSet<i64>> {
    let mut st = m
        .sql()
        .prepare(
            "SELECT winner FROM geom_join WHERE winner_kind='wall' \
             UNION SELECT loser FROM geom_join WHERE loser_kind='wall'",
        )
        .ok()?;
    let ids = st
        .query_map([], |r| r.get::<_, i64>(0))
        .ok()?
        .collect::<Result<HashSet<i64>, _>>()
        .ok()?;
    Some(ids)
}

/// Plan section of wall `id` after form cuts (None: not cut). `Model::cut_section` is asked
/// only for walls in `cut`: for any other wall it has no cutter and gives None.
pub(super) fn cut_plan(
    m: &Model,
    cut: Option<&HashSet<i64>>,
    id: i64,
    z: f64,
) -> Option<Vec<Vec<V2>>> {
    if cut.is_some_and(|c| !c.contains(&id)) {
        return None;
    }
    let rs = m.cut_section("wall", id, z).ok()??;
    Some(
        rs.into_iter()
            .filter(|r| ddc_geom::poly::area(r) > 0.0)
            .collect(),
    )
}

/// Scoped reloads of this thread so far (tests and budgets check the path was taken).
#[cfg(test)]
pub(crate) fn hits() -> u64 {
    HITS.with(|h| h.get())
}

/// Changed rows a scoped reload always takes; above it and above half the level's walls and
/// openings the full reload runs.
const LIMIT_MIN: usize = 256;

/// Level and main schema version the lists were built for. The journal does not see DDL, so a
/// schema change (or another active level) is caught here; None if unknown.
pub(super) fn key(ed: &Editor) -> Option<(i64, i64)> {
    let v = ed
        .model
        .sql()
        .query_row("PRAGMA schema_version", [], |r| r.get::<_, i64>(0))
        .ok()?;
    Some((ed.level, v))
}

fn touch(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

impl Editor {
    /// Reload after the model changed since journal number `since`: scoped when it can be, else
    /// full. The one place that chooses: a scoped error may leave lists half patched, so it is
    /// never returned; the full reload rebuilds every list and only its error is reported.
    pub(super) fn reload_since(&mut self, since: u64) -> Result<(), ddc_model::Error> {
        match self.reload_scoped(since) {
            Ok(true) => {
                #[cfg(test)]
                HITS.with(|h| h.set(h.get() + 1));
                self.struct_reload();
                self.revision += 1;
                crate::room_watch::after_reload(self);
                return Ok(());
            }
            Ok(false) => {}
            Err(_) => {
                #[cfg(test)]
                FALLBACKS.with(|h| h.set(h.get() + 1));
            }
        }
        self.reload()
    }

    /// Axis box of a straight wall widened by half its thickness (the strip is centred on the
    /// axis) and `PAD`.
    fn wall_box(&self, w: &Wall) -> [f64; 4] {
        let r = self
            .type_by_id(w.wall_type)
            .map_or(0.05, |t| t.thickness() / 2.0)
            + PAD;
        [
            w.a.x.min(w.b.x) - r,
            w.a.y.min(w.b.y) - r,
            w.a.x.max(w.b.x) + r,
            w.a.y.max(w.b.y) + r,
        ]
    }

    fn snap_wall(&self, w: &Wall) -> SnapWall {
        SnapWall {
            a: w.a,
            b: w.b,
            half: self
                .type_by_id(w.wall_type)
                .map_or(0.0, |t| t.thickness() / 2.0),
        }
    }

    /// The scoped reload; false: not applicable, the editor is untouched.
    fn reload_scoped(&mut self, since: u64) -> Result<bool, ddc_model::Error> {
        let ch = self.model.changes_since(since);
        if ch.all || self.scoped_key.is_none() || key(self) != self.scoped_key {
            return Ok(false);
        }
        // a big change set: one pass over the level beats patching most of it
        let rows: usize = ch.rows.values().map(|r| r.len()).sum();
        if rows > LIMIT_MIN.max((self.walls.len() + self.openings.len()) / 2) {
            return Ok(false);
        }
        // no wall row: openings, slabs and rooms (`scoped_more`) or the full reload
        if !ch.only(FOLLOWED) {
            if ch.only(super::scoped_more::FOLLOWED_MORE) {
                return self.reload_more(&ch);
            }
            return Ok(false);
        }
        if ch.whole("wall") || ch.whole("wall_qty") {
            return Ok(false);
        }
        let n = self.walls.len();
        if !self.plan_joined
            || self.plan.len() != n
            || self.otool.walls.len() != n
            || self.tool.walls.len() != n
            || self.walls.iter().any(|w| w.bulge != 0.0)
        {
            return Ok(false);
        }
        // walls are ordered by id (`walls_on`)
        let index = |id: i64| self.walls.binary_search_by_key(&id, |w| w.id).ok();
        let db = self.model.sql();
        let mut st = db.prepare_cached(&format!("{WALL_SQL} WHERE w.id=?1 AND w.level=?2"))?;
        // edited walls: still on this level and straight, else the lists change shape
        let mut rows: BTreeMap<usize, Wall> = BTreeMap::new();
        for id in ch.ids("wall") {
            let Some(i) = index(id) else {
                return Ok(false);
            };
            let Some(w) = st.query_row([id, self.level], wall_row).optional()? else {
                return Ok(false);
            };
            if w.bulge != 0.0 {
                return Ok(false);
            }
            rows.insert(i, w);
        }
        let boxes: Vec<[f64; 4]> = (0..n)
            .map(|i| self.wall_box(rows.get(&i).unwrap_or(&self.walls[i])))
            .collect();
        let probes: Vec<[f64; 4]> = rows
            .iter()
            .flat_map(|(&i, w)| [self.wall_box(&self.walls[i]), self.wall_box(w)])
            .collect();
        // joins that can change: edited walls and walls touching their old or new box; their
        // joins read only walls touching their own boxes
        let keep: Vec<usize> = (0..n)
            .filter(|&i| rows.contains_key(&i) || probes.iter().any(|p| touch(p, &boxes[i])))
            .collect();
        let reach: Vec<[f64; 4]> = keep.iter().map(|&i| boxes[i]).collect();
        let all: Vec<usize> = if keep.is_empty() {
            vec![]
        } else {
            (0..n)
                .filter(|&i| reach.iter().any(|r| touch(r, &boxes[i])))
                .collect()
        };
        // quantities: walls of the rewritten `wall_qty` rows (the model regenerates a neighbourhood)
        let mut qty: BTreeSet<usize> = BTreeSet::new();
        {
            let mut q = db.prepare_cached("SELECT wall FROM wall_qty WHERE rowid=?1")?;
            for r in ch.ids("wall_qty") {
                let w = q.query_row([r], |r| r.get::<_, i64>(0)).optional()?;
                qty.extend(w.and_then(index));
            }
        }
        for &i in keep.iter().chain(&qty) {
            if rows.contains_key(&i) {
                continue;
            }
            let id = self.walls[i].id;
            let Some(w) = st.query_row([id, self.level], wall_row).optional()? else {
                return Ok(false);
            };
            rows.insert(i, w);
        }
        drop(st);
        let (idx, ws): (Vec<usize>, Vec<Wall>) = rows.into_iter().unzip();
        let ws = self.with_bases(ws)?;
        let row_of = |i: usize| idx.binary_search(&i).ok().map(|k| &ws[k]);
        let mut plan: Vec<(usize, Vec<Vec<V2>>)> = vec![];
        if !all.is_empty() {
            let cur: Vec<Wall> = all
                .iter()
                .map(|&i| row_of(i).unwrap_or(&self.walls[i]).clone())
                .collect();
            let strips = self.strips(&cur);
            let ids: Vec<i64> = cur.iter().map(|w| w.id).collect();
            let Ok(prefs) = self.model.join_prefs(&ids) else {
                return Ok(false);
            };
            // the level joined before and the walls outside `all` did not change: a join of
            // `all` that holds means the join of the level holds
            let Ok(j) = ddc_geom::layered::join_with(&strips, &prefs, Tol::default()) else {
                return Ok(false);
            };
            let z = self.elevation + crate::plan_range::range(self).cut;
            let cut = cut_walls(&self.model);
            for &i in &keep {
                let k = all.binary_search(&i).expect("keep is part of all");
                let fp = cut_plan(&self.model, cut.as_ref(), cur[k].id, z)
                    .unwrap_or_else(|| wall_footprint(&strips[k], Some(&j), k));
                plan.push((i, fp));
            }
        }
        for (i, w) in idx.into_iter().zip(ws) {
            self.otool.walls[i] = self.host(&w);
            self.tool.walls[i] = self.snap_wall(&w);
            self.walls[i] = w;
        }
        fault()?;
        for (i, fp) in plan {
            self.plan[i] = fp;
        }
        self.otool.preview = None;
        let l = super::cw::load(&self.model, self.level);
        (
            self.curtains,
            self.slabs,
            self.curtain_lines,
            self.elevation,
        ) = (l.curtains, l.slabs, l.lines, l.elevation);
        self.reload_rest(self.elevation + crate::plan_range::range(self).cut)?;
        Ok(true)
    }
}

#[cfg(test)]
#[path = "scoped_fuzz.rs"]
mod scoped_fuzz;
#[cfg(test)]
#[path = "scoped_guard.rs"]
mod scoped_guard;

#[cfg(test)]
mod tests {
    use super::{fallbacks, hits, inject_fault};
    use crate::editor::{Editor, OpFn};
    use ddc_model::{LayerSpec, Model, WallSpec};
    use std::sync::Arc;

    /// Rooms of four joined walls in a row (shared corners and T-joins), plus loose walls.
    fn fixture() -> (Editor, Vec<i64>) {
        let mut ed = Editor::new_in_memory().unwrap();
        let level = ed.level;
        let t = ed
            .model
            .add_wall_type(
                "W two",
                &[
                    LayerSpec {
                        material: "a".into(),
                        thickness: 0.1,
                    },
                    LayerSpec {
                        material: "b".into(),
                        thickness: 0.2,
                    },
                ],
            )
            .unwrap();
        let mut ids = vec![];
        let mut add = |a: (f64, f64), b: (f64, f64)| {
            ids.push(
                ed.model
                    .add_wall(&WallSpec {
                        a,
                        b,
                        level,
                        base_offset: 0.0,
                        height: 3.0,
                        wall_type: t,
                        offset0: None,
                    })
                    .unwrap(),
            );
        };
        for k in 0..4 {
            let x = k as f64 * 5.0;
            add((x, 0.0), (x + 5.0, 0.0));
            add((x, 4.0), (x + 5.0, 4.0));
            add((x, 0.0), (x, 4.0));
        }
        add((20.0, 0.0), (20.0, 4.0));
        for k in 0..6 {
            let x = k as f64 * 4.0;
            add((x, 10.0), (x + 3.0, 10.0));
        }
        ed.reload().unwrap();
        // the first op after a load regenerates everything (and keys the level cache)
        let w = ids[1];
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| set(m, w, |s| s.height = 3.0))))
            .unwrap();
        (ed, ids)
    }

    /// Lists a reload builds from wall rows, in a comparable form.
    fn lists(ed: &Editor) -> String {
        let snap: Vec<_> = ed.tool.walls.iter().map(|s| (s.a, s.b, s.half)).collect();
        format!(
            "{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{}",
            ed.walls,
            ed.plan,
            ed.otool.walls,
            snap,
            ed.selection,
            ed.tags.len(),
            ed.plan_joined
        )
    }

    fn edit(
        ed: &mut Editor,
        f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
    ) {
        let h = hits();
        ed.run_op(OpFn(Arc::new(f))).unwrap();
        assert_eq!(hits(), h + 1, "the edit takes the scoped reload");
        let scoped = lists(ed);
        ed.reload().unwrap();
        assert_eq!(scoped, lists(ed), "scoped reload equals the full one");
    }

    fn set(m: &mut Model, id: i64, f: impl FnOnce(&mut WallSpec)) -> Result<(), ddc_model::Error> {
        let (mut s, bulge) = crate::query::wall_spec(m, id)?;
        f(&mut s);
        m.apply_edits(&[ddc_model::Edit::Wall { id, spec: s, bulge }])
    }

    #[test]
    fn height_edit_matches_full_reload() {
        let (mut ed, ids) = fixture();
        for (k, &id) in ids.iter().enumerate() {
            let v = 2.5 + k as f64 * 0.1;
            edit(&mut ed, move |m| set(m, id, |s| s.height = v));
        }
    }

    #[test]
    fn moving_a_joined_end_matches_full_reload() {
        let (mut ed, ids) = fixture();
        // the shared corner becomes a T-join (old corner breaks) and comes back
        let first = ids[0];
        edit(&mut ed, move |m| set(m, first, |s| s.a = (-1.0, 0.0)));
        edit(&mut ed, move |m| set(m, first, |s| s.a = (0.0, 0.0)));
        // a loose wall moves onto the row: it joins in its new place
        let loose = ids[ids.len() - 1];
        edit(&mut ed, move |m| {
            set(m, loose, |s| {
                s.a = (7.5, 0.0);
                s.b = (7.5, 4.0);
            })
        });
        // wall type of a joined wall
        let t = ed.types[0].id;
        let mid = ids[4];
        edit(&mut ed, move |m| set(m, mid, |s| s.wall_type = t));
    }

    #[test]
    fn selection_and_undo_keep_the_full_lists() {
        let (mut ed, ids) = fixture();
        ed.selection = ids.iter().map(|&id| crate::editor::Sel::Wall(id)).collect();
        let w = ids[2];
        edit(&mut ed, move |m| set(m, w, |s| s.height = 4.0));
        let h = hits();
        ed.undo();
        assert_eq!(hits(), h + 1, "undo of the edit takes the scoped reload");
        let after = lists(&ed);
        ed.reload().unwrap();
        assert_eq!(after, lists(&ed));
    }

    #[test]
    fn other_changes_take_the_full_reload() {
        let (mut ed, ids) = fixture();
        let level = ed.level;
        let h = hits();
        // a new wall: the lists change shape
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            m.add_wall(&WallSpec {
                a: (50.0, 0.0),
                b: (53.0, 0.0),
                level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: 1,
                offset0: None,
            })
            .map(|_| ())
        })))
        .unwrap();
        // a wall moved to another level
        let other = ed.model.add_level("L2", 3.0).unwrap();
        let w = ids[0];
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| set(m, w, |s| s.level = other))))
            .unwrap();
        assert_eq!(hits(), h, "{}", lists(&ed));
        assert!(!ed.walls.iter().any(|x| x.id == w));
    }

    #[test]
    fn scoped_wall_error_falls_back_to_full() {
        let (mut ed, ids) = fixture();
        ed.selection = ids.iter().map(|&id| crate::editor::Sel::Wall(id)).collect();
        let (h, f) = (hits(), fallbacks());
        // the scoped reload fails with the walls patched and the plan not yet
        inject_fault();
        let w = ids[2];
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| set(m, w, |s| s.height = 4.0))))
            .unwrap();
        assert_eq!((hits(), fallbacks()), (h, f + 1), "the full reload took over");
        assert!(ed.walls.iter().any(|x| x.id == w && x.height == 4.0));
        let after = lists(&ed);
        ed.reload().unwrap();
        assert_eq!(after, lists(&ed), "the fallback equals the full reload");
        // a moved end changes the plan: no stale section of the failed reload is left
        let first = ids[0];
        inject_fault();
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            set(m, first, |s| s.a = (-1.0, 0.0))
        })))
        .unwrap();
        assert_eq!(fallbacks(), f + 2);
        let after = lists(&ed);
        ed.reload().unwrap();
        assert_eq!(after, lists(&ed));
        // the next edit takes the scoped reload again
        edit(&mut ed, move |m| set(m, w, |s| s.height = 3.5));
    }
}
