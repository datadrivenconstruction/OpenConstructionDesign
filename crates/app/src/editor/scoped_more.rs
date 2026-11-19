// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Scoped reload after an edit without wall rows (`run_op`, undo, redo): openings, slabs and
//! rooms. Openings are patched by id when no wall row changed, the host is on this level and no
//! form join cuts the host (its plan section would change); slab rows reload the curtain wall
//! and slab lists (`cw::load`) when no wall of the level hangs on a slab (its base follows the
//! slab) and no slab is in a form join; rooms, tags and the other level lists load as in
//! `reload_rest`. Wall quantities rewritten by the edit are read again for their walls only.
//! Anything else takes the full `reload`; the lists are the same either way (tests compare them).
//! An error here, also after some lists were patched, falls back to the full reload
//! (`scoped::reload_since`).

use super::{opening_row, Editor, Sel, OPENING_SQL};
use crate::opening_tool::Placed;
use ddc_model::Changes;
use rusqlite::OptionalExtension;
use std::collections::BTreeSet;

/// Tables of this scoped reload. No `wall`: an edit that writes a wall row takes the wall path
/// in `scoped` or the full reload.
pub(super) const FOLLOWED_MORE: &[&str] = &[
    "opening",
    "wall_qty",
    "dim_geo",
    "prop",
    "pparam_val",
    "slab",
    "slab_vertex",
    "slab_qty",
    "room",
    "room_qty",
    "room_sep",
];

const SLAB: &[&str] = &["slab", "slab_vertex", "slab_qty"];

/// A changed opening and its row now (None: deleted or no longer on this level).
type Patch = Vec<(i64, Option<Placed>)>;

impl Editor {
    /// The scoped reload of `ch` (only `FOLLOWED_MORE` tables); false: not applicable, the
    /// editor is untouched.
    pub(super) fn reload_more(&mut self, ch: &Changes) -> Result<bool, ddc_model::Error> {
        if ch.all || ch.whole("opening") || ch.whole("wall_qty") {
            return Ok(false);
        }
        let ids: BTreeSet<i64> = ch.ids("opening").collect();
        let patch = if ids.is_empty() {
            vec![]
        } else {
            match self.opening_patch(&ids)? {
                Some(p) => p,
                None => return Ok(false),
            }
        };
        let slabs = SLAB.iter().any(|t| ch.touched(t));
        if slabs && !self.slabs_free() {
            return Ok(false);
        }
        // quantities: walls of the rewritten `wall_qty` rows and the old and new hosts
        let sums = {
            let index = |id: i64| self.walls.binary_search_by_key(&id, |w| w.id).ok();
            let db = self.model.sql();
            let mut qty: BTreeSet<usize> = BTreeSet::new();
            let mut q = db.prepare_cached("SELECT wall FROM wall_qty WHERE rowid=?1")?;
            for r in ch.ids("wall_qty") {
                let w = q.query_row([r], |r| r.get::<_, i64>(0)).optional()?;
                qty.extend(w.and_then(index));
            }
            for (id, new) in &patch {
                let os = &self.openings;
                let old = os.binary_search_by_key(id, |o| o.id).ok().map(|k| os[k].wall);
                let hosts = old.into_iter().chain(new.as_ref().map(|o| o.wall));
                qty.extend(hosts.filter_map(index));
            }
            // the same sums as `WALL_SQL`
            let mut s = db.prepare_cached(
                "SELECT COALESCE(SUM(plan_area),0), COALESCE(SUM(volume),0) \
                 FROM wall_qty WHERE wall=?1",
            )?;
            let mut sums: Vec<(usize, f64, f64)> = vec![];
            for i in qty {
                let (a, v) = s.query_row([self.walls[i].id], |r| {
                    Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?))
                })?;
                sums.push((i, a, v));
            }
            sums
        };
        let l = slabs.then(|| super::cw::load(&self.model, self.level));
        for (i, a, v) in sums {
            let w = &mut self.walls[i];
            w.plan_area = a;
            w.volume = v;
        }
        let mut gone: Vec<i64> = vec![];
        for (id, new) in patch {
            match (self.openings.binary_search_by_key(&id, |o| o.id), new) {
                (Ok(k), Some(o)) => {
                    self.openings[k] = o;
                    self.otool.openings[k] = o;
                }
                (Err(k), Some(o)) => {
                    self.openings.insert(k, o);
                    self.otool.openings.insert(k, o);
                }
                (Ok(k), None) => {
                    self.openings.remove(k);
                    self.otool.openings.remove(k);
                    gone.push(id);
                }
                (Err(_), None) => {}
            }
        }
        super::scoped::fault()?;
        if !gone.is_empty() {
            self.selection
                .retain(|s| !matches!(s, Sel::Opening(id) if gone.contains(id)));
        }
        if let Some(l) = l {
            (
                self.curtains,
                self.slabs,
                self.curtain_lines,
                self.elevation,
            ) = (l.curtains, l.slabs, l.lines, l.elevation);
            let (cs, ss) = (&self.curtains, &self.slabs);
            self.selection.retain(|s| match s {
                Sel::Curtain(id) => cs.iter().any(|c| c.id == *id),
                Sel::Panel(id, k) => cs
                    .iter()
                    .any(|c| c.id == *id && (*k as usize) < c.layout.layout.cells.len()),
                Sel::Slab(id) => ss.iter().any(|x| x.id == *id),
                _ => true,
            });
        }
        self.otool.preview = None;
        self.reload_rest(self.elevation + crate::plan_range::range(self).cut)?;
        Ok(true)
    }

    /// Rows of the changed openings `ids`; None: the list cannot be patched (it is not the
    /// ordered list of the last reload, a host is not loaded, or a form join cuts a host).
    fn opening_patch(&self, ids: &BTreeSet<i64>) -> Result<Option<Patch>, ddc_model::Error> {
        let os = &self.openings;
        if self.otool.openings != *os || os.windows(2).any(|p| p[0].id >= p[1].id) {
            return Ok(None);
        }
        let Some(cut) = super::scoped::cut_walls(&self.model) else {
            return Ok(None);
        };
        let loaded = |id: i64| self.walls.binary_search_by_key(&id, |w| w.id).is_ok();
        let db = self.model.sql();
        let mut st = db.prepare_cached(&format!("{OPENING_SQL} WHERE o.id=?1 AND w.level=?2"))?;
        let mut out: Patch = vec![];
        for &id in ids {
            let old = os.binary_search_by_key(&id, |o| o.id).ok().map(|k| os[k].wall);
            let new = st.query_row([id, self.level], opening_row).optional()?;
            let new_host = new.as_ref().map(|o| o.wall);
            if old.into_iter().chain(new_host).any(|h| cut.contains(&h)) {
                return Ok(None);
            }
            if new_host.is_some_and(|h| !loaded(h)) {
                return Ok(None);
            }
            out.push((id, new));
        }
        Ok(Some(out))
    }

    /// No wall of this level is attached to a slab (its base profile follows the slab) and no
    /// slab is in a form join (it may cut a wall's plan section); false also when unknown.
    fn slabs_free(&self) -> bool {
        let db = self.model.sql();
        let hung = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM wall_attach a JOIN wall w ON w.id=a.wall \
             WHERE w.level=?1)",
            [self.level],
            |r| r.get::<_, bool>(0),
        );
        let joined = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM geom_join WHERE winner_kind='slab' OR loser_kind='slab')",
            [],
            |r| r.get::<_, bool>(0),
        );
        matches!((hung, joined), (Ok(false), Ok(false)))
    }
}

#[cfg(test)]
#[path = "scoped_more_fuzz.rs"]
mod scoped_more_fuzz;

#[cfg(test)]
mod tests {
    use crate::editor::scoped::{fallbacks, hits, inject_fault};
    use crate::editor::{Editor, OpFn, Sel};
    use ddc_model::{LayerSpec, Model, OpeningSpec, RoomSpec, SlabSpec, WallSpec};
    use std::sync::Arc;

    struct Fx {
        ed: Editor,
        walls: Vec<i64>,
        door: i64,
        slab: i64,
        room: i64,
    }

    fn door_spec(along: f64, width: f64) -> OpeningSpec {
        OpeningSpec {
            along,
            width,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        }
    }

    /// Two joined rooms sharing a wall, a door and a window, a floor slab under both, two rooms.
    fn fixture() -> Fx {
        let mut ed = Editor::new_in_memory().unwrap();
        let level = ed.level;
        let one = |m: &str| {
            [LayerSpec {
                material: m.into(),
                thickness: 0.2,
            }]
        };
        let t = ed.model.add_wall_type("W pf4", &one("a")).unwrap();
        let mut walls = vec![];
        for (a, b) in [
            ((0.0, 0.0), (6.0, 0.0)),
            ((6.0, 0.0), (10.0, 0.0)),
            ((10.0, 0.0), (10.0, 4.0)),
            ((10.0, 4.0), (6.0, 4.0)),
            ((6.0, 4.0), (0.0, 4.0)),
            ((0.0, 4.0), (0.0, 0.0)),
            ((6.0, 0.0), (6.0, 4.0)),
        ] {
            let spec = WallSpec {
                a,
                b,
                level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            };
            walls.push(ed.model.add_wall(&spec).unwrap());
        }
        let door = ed.model.add_opening(walls[0], &door_spec(2.0, 0.9)).unwrap();
        let window = OpeningSpec {
            along: 2.0,
            width: 1.2,
            sill: 0.9,
            height: 1.2,
            kind: "window".into(),
        };
        ed.model.add_opening(walls[2], &window).unwrap();
        let st = ed.model.add_slab_type("F pf4", &one("c")).unwrap();
        let slab = ed
            .model
            .add_slab(&SlabSpec {
                level,
                slab_type: st,
                offset: 0.0,
                outer: vec![(0.0, 0.0), (10.0, 0.0), (10.0, 4.0), (0.0, 4.0)],
                holes: vec![],
            })
            .unwrap();
        let mut room = |name: &str, number: &str, point: (f64, f64)| {
            ed.model
                .add_room(&RoomSpec {
                    level,
                    name: name.into(),
                    number: number.into(),
                    point,
                    height: 2.7,
                })
                .unwrap()
        };
        let hall = room("Hall", "1", (3.0, 2.0));
        room("Store", "2", (8.0, 2.0));
        ed.reload().unwrap();
        // the first op after a load regenerates everything (and keys the level cache)
        let w = walls[1];
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            let (mut s, bulge) = crate::query::wall_spec(m, w)?;
            s.height = 3.0;
            m.apply_edits(&[ddc_model::Edit::Wall { id: w, spec: s, bulge }])
        })))
        .unwrap();
        Fx {
            ed,
            walls,
            door,
            slab,
            room: hall,
        }
    }

    /// Every list a reload builds, in a comparable form.
    fn lists(ed: &Editor) -> String {
        let snap: Vec<_> = ed.tool.walls.iter().map(|s| (s.a, s.b, s.half)).collect();
        let slabs: Vec<String> = ed
            .slabs
            .iter()
            .map(|s| {
                format!(
                    "{} {:?} {:?} {:?} {} {} {} {}",
                    s.id,
                    s.rings,
                    s.cuts,
                    s.slope,
                    s.top,
                    s.thickness,
                    s.area,
                    s.volume
                )
            })
            .collect();
        let curtains: Vec<i64> = ed.curtains.iter().map(|c| c.id).collect();
        format!(
            "{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{}\n{}",
            ed.walls,
            ed.plan,
            ed.otool.walls,
            snap,
            ed.openings,
            ed.otool.openings,
            ed.selection,
            slabs,
            curtains,
            ed.curtain_lines,
            ed.rooms,
            ed.tags,
            ed.mass_plan,
            ed.views,
            ed.underlay,
            ed.elevation,
            ed.plan_joined
        )
    }

    fn same(ed: &mut Editor) {
        let scoped = lists(ed);
        ed.reload().unwrap();
        assert_eq!(scoped, lists(ed), "scoped reload equals the full one");
    }

    fn scoped(
        ed: &mut Editor,
        f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
    ) {
        let h = hits();
        ed.run_op(OpFn(Arc::new(f))).unwrap();
        assert_eq!(hits(), h + 1, "the edit takes the scoped reload");
        same(ed);
    }

    fn width(ed: &Editor, id: i64) -> Option<f64> {
        ed.opening(id).map(|o| o.width)
    }

    #[test]
    fn door_width_edit_matches_full_reload() {
        let mut f = fixture();
        let door = f.door;
        f.ed.selection = vec![
            Sel::Opening(door),
            Sel::Wall(f.walls[0]),
            Sel::Slab(f.slab),
            Sel::Room(f.room),
        ];
        for w in [1.0, 1.1, 0.8] {
            scoped(&mut f.ed, move |m| m.update_opening(door, &door_spec(2.0, w)));
            assert_eq!(width(&f.ed, door), Some(w));
        }
        // moved along its wall
        scoped(&mut f.ed, move |m| m.update_opening(door, &door_spec(2.5, 0.8)));
        assert!(f.ed.selection.contains(&Sel::Opening(door)));
        // a new opening and its removal: the lists match whichever path they take
        let host = f.walls[4];
        f.ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            m.add_opening(host, &door_spec(2.0, 0.9)).map(|_| ())
        })))
        .unwrap();
        same(&mut f.ed);
        let new = f.ed.openings.iter().map(|o| o.id).max().unwrap();
        assert_ne!(new, door);
        f.ed.selection.push(Sel::Opening(new));
        f.ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.delete_opening(new))))
            .unwrap();
        assert!(!f.ed.selection.contains(&Sel::Opening(new)));
        assert!(width(&f.ed, new).is_none());
        same(&mut f.ed);
    }

    #[test]
    fn slab_vertex_move_matches_full_reload() {
        let mut f = fixture();
        let slab = f.slab;
        f.ed.selection = vec![Sel::Slab(slab), Sel::Opening(f.door)];
        let area = f.ed.slabs.iter().find(|s| s.id == slab).unwrap().area;
        scoped(&mut f.ed, move |m| {
            m.set_slab_boundary(slab, &[(0.0, 0.0), (10.0, 0.0), (10.0, 5.0), (0.0, 4.0)], &[])
        });
        let moved = f.ed.slabs.iter().find(|s| s.id == slab).unwrap().area;
        assert!(moved > area + 1.0, "{moved} after {area}");
        scoped(&mut f.ed, move |m| {
            m.set_slab_boundary(slab, &[(0.0, 0.0), (10.0, 0.0), (10.0, 4.0), (0.0, 4.0)], &[])
        });
        assert!(f.ed.selection.contains(&Sel::Slab(slab)));
    }

    #[test]
    fn room_rename_matches_full_reload() {
        let mut f = fixture();
        let (room, level) = (f.room, f.ed.level);
        f.ed.selection = vec![Sel::Room(room)];
        let spec = move |name: &str, point: (f64, f64)| RoomSpec {
            level,
            name: name.into(),
            number: "1".into(),
            point,
            height: 2.7,
        };
        scoped(&mut f.ed, move |m| m.update_room(room, &spec("Lobby", (3.0, 2.0))));
        assert!(f.ed.rooms.iter().any(|r| r.id == room && r.name == "Lobby"));
        // its point moves within the same walls: the outline is read again
        scoped(&mut f.ed, move |m| m.update_room(room, &spec("Lobby", (2.0, 1.0))));
        // deleted: gone from the list and the selection, whichever path it takes
        f.ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.delete_room(room))))
            .unwrap();
        assert!(!f.ed.rooms.iter().any(|r| r.id == room));
        assert!(!f.ed.selection.contains(&Sel::Room(room)));
        same(&mut f.ed);
    }

    #[test]
    fn opening_on_edited_wall_takes_full() {
        let mut f = fixture();
        let (w, door) = (f.walls[0], f.door);
        let h = hits();
        f.ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            let (mut s, bulge) = crate::query::wall_spec(m, w)?;
            s.height = 3.5;
            m.apply_edits(&[
                ddc_model::Edit::Wall { id: w, spec: s, bulge },
                ddc_model::Edit::Opening {
                    id: door,
                    wall: w,
                    spec: door_spec(2.0, 1.0),
                },
            ])
        })))
        .unwrap();
        assert_eq!(hits(), h, "a wall row in the edit takes the full reload");
        assert_eq!(width(&f.ed, door), Some(1.0));
        same(&mut f.ed);
    }

    #[test]
    fn undo_of_door_edit_scoped() {
        let mut f = fixture();
        let door = f.door;
        f.ed.selection = vec![Sel::Opening(door)];
        scoped(&mut f.ed, move |m| m.update_opening(door, &door_spec(2.0, 1.2)));
        let h = hits();
        f.ed.undo();
        assert_eq!(hits(), h + 1, "undo of the door edit takes the scoped reload");
        assert_eq!(width(&f.ed, door), Some(0.9));
        same(&mut f.ed);
        assert!(f.ed.selection.contains(&Sel::Opening(door)));
    }

    #[test]
    fn scoped_more_error_falls_back_to_full() {
        let mut f = fixture();
        let (door, slab) = (f.door, f.slab);
        f.ed.selection = vec![Sel::Opening(door), Sel::Slab(slab), Sel::Room(f.room)];
        let (h, n) = (hits(), fallbacks());
        // the scoped reload fails with the openings patched and the slabs not yet
        inject_fault();
        f.ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            m.update_opening(door, &door_spec(2.0, 1.1))?;
            m.set_slab_boundary(slab, &[(0.0, 0.0), (10.0, 0.0), (10.0, 5.0), (0.0, 4.0)], &[])
        })))
        .unwrap();
        assert_eq!((hits(), fallbacks()), (h, n + 1), "the full reload took over");
        assert_eq!(width(&f.ed, door), Some(1.1));
        let area = f.ed.slabs.iter().find(|s| s.id == slab).unwrap().area;
        assert!(area > 41.0, "the slab list is new too: {area}");
        same(&mut f.ed);
        // moved along its wall: the fallback again, the selection kept
        inject_fault();
        f.ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            m.update_opening(door, &door_spec(2.5, 1.1))
        })))
        .unwrap();
        assert_eq!(fallbacks(), n + 2);
        assert!(f.ed.selection.contains(&Sel::Opening(door)));
        same(&mut f.ed);
        // the next edit takes the scoped reload again
        let host = f.walls[2];
        let win = f.ed.openings.iter().find(|o| o.wall == host).unwrap().id;
        let spec = OpeningSpec {
            along: 2.0,
            width: 1.0,
            sill: 0.9,
            height: 1.2,
            kind: "window".into(),
        };
        scoped(&mut f.ed, move |m| m.update_opening(win, &spec));
    }

    #[test]
    fn scoped_fallback_equals_full_reload() {
        let mut f = fixture();
        let door = f.door;
        f.ed.selection = vec![Sel::Opening(door), Sel::Room(f.room)];
        // a broken saved view: the scoped and the full reload both read it and fail
        f.ed.model
            .sql()
            .execute_batch(
                "INSERT INTO view_def(kind,name,ax,ay,bx,by,depth,far_clip) \
                 VALUES('broken','x',0,0,1,0,1,0)",
            )
            .unwrap();
        let (h, n) = (hits(), fallbacks());
        let r = f.ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            m.update_opening(door, &door_spec(2.0, 1.2))
        })));
        assert!(r.is_err(), "the error of the full reload is reported");
        assert_eq!((hits(), fallbacks()), (h, n + 1), "the scoped error fell back");
        assert_eq!(width(&f.ed, door), Some(1.2));
        // the editor holds what a full reload leaves, not a scoped half
        let after = lists(&f.ed);
        assert!(f.ed.reload().is_err());
        assert_eq!(after, lists(&f.ed), "the fallback equals the full reload");
        // repaired: the lists load again
        f.ed.model
            .sql()
            .execute_batch("DELETE FROM view_def WHERE kind='broken'")
            .unwrap();
        f.ed.reload().unwrap();
        assert_eq!(width(&f.ed, door), Some(1.2));
        assert!(f.ed.selection.contains(&Sel::Opening(door)));
    }
}
