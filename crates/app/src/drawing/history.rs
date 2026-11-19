//! Undo for underlay edits. Underlays live in the session, not in the model, so a step makes no
//! model operation: it stores the other side of the change, and undo and redo swap that with
//! the live state. The editor's stack keeps the order with model edits; `Editor` cannot reach
//! `Drawings`, so steps cross over in [`Drawings::sync_undo`].

use std::sync::{Arc, Mutex};

use ddc_geom::V2;
use ddc_io::drawing_model::Underlay;

use super::{Drawing, Drawings};
use crate::editor::Editor;

enum Stored {
    /// Geometry and placement of drawing `id`.
    Geometry {
        id: u64,
        u: Box<Underlay>,
        offset: V2,
        rot: f64,
        /// Edit generation of the other side.
        gen: u64,
    },
    /// Drawing `id`: in the list while `out` is empty, else parked here and returns to `at`.
    /// `back_to` is the drawing to make current when this one leaves the list.
    Presence {
        id: u64,
        at: usize,
        out: Option<Box<Drawing>>,
        back_to: Option<u64>,
    },
}

/// One underlay gesture on the undo stack. Clones share the stored state.
#[derive(Clone)]
pub struct Step {
    stored: Arc<Mutex<Stored>>,
    weight: usize,
}

impl std::fmt::Debug for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("Step")
    }
}

fn prims(u: &Underlay) -> usize {
    u.lines.len() + u.arcs.len() + u.texts.len() + u.closed.len() + u.fills.len()
}

impl Step {
    fn new(stored: Stored, weight: usize) -> Step {
        Step {
            stored: Arc::new(Mutex::new(stored)),
            weight,
        }
    }

    /// Call before the edit: holds the state undo returns to.
    pub(super) fn geometry(d: &Drawing) -> Step {
        let stored = Stored::Geometry {
            id: d.id,
            u: Box::new(d.u.clone()),
            offset: d.offset,
            rot: d.rot,
            gen: d.gen,
        };
        Step::new(stored, prims(&d.u))
    }

    /// A copy now in the list at `at`. It holds no extra memory: undo only parks it.
    pub(super) fn copied(id: u64, at: usize, source: u64) -> Step {
        let stored = Stored::Presence {
            id,
            at,
            out: None,
            back_to: Some(source),
        };
        Step::new(stored, 0)
    }

    /// A drawing taken out of the list at `at`.
    pub(super) fn removed(d: Drawing, at: usize) -> Step {
        let weight = prims(&d.u);
        let stored = Stored::Presence {
            id: d.id,
            at,
            out: Some(Box::new(d)),
            back_to: None,
        };
        Step::new(stored, weight)
    }

    /// Primitives this step keeps alive outside the list.
    pub fn weight(&self) -> usize {
        self.weight
    }

    /// The drawing this step acts on.
    pub fn drawing(&self) -> u64 {
        match &*self.stored.lock().unwrap_or_else(|e| e.into_inner()) {
            Stored::Geometry { id, .. } | Stored::Presence { id, .. } => *id,
        }
    }

    /// The drawing only this step holds: dropping the step frees it.
    pub fn parked(&self) -> Option<u64> {
        match &*self.stored.lock().unwrap_or_else(|e| e.into_inner()) {
            Stored::Presence {
                id, out: Some(_), ..
            } => Some(*id),
            _ => None,
        }
    }

    fn swap(&self, ds: &mut Drawings) {
        let mut stored = self.stored.lock().unwrap_or_else(|e| e.into_inner());
        match &mut *stored {
            Stored::Geometry {
                id,
                u,
                offset,
                rot,
                gen,
            } => {
                let Some(k) = ds.list.iter().position(|d| d.id == *id) else {
                    return;
                };
                let d = &mut ds.list[k];
                std::mem::swap(&mut d.u, &mut **u);
                std::mem::swap(&mut d.offset, offset);
                std::mem::swap(&mut d.rot, rot);
                std::mem::swap(&mut d.gen, gen);
                d.dirty = d.gen != d.saved_gen;
                // a layer added or emptied by the edit comes and goes with it
                d.sync_layers();
                // both index primitives of the state that just left
                if ds.pick.as_ref().is_some_and(|p| p.0 == k) {
                    ds.pick = None;
                }
                if ds.recog.as_ref().is_some_and(|r| r.drawing == k) {
                    ds.recog = None;
                }
                ds.touch();
            }
            Stored::Presence {
                id,
                at,
                out,
                back_to,
            } => match out.take() {
                Some(d) => ds.put_back(*d, *at),
                None => {
                    let Some(k) = ds.list.iter().position(|d| d.id == *id) else {
                        return;
                    };
                    *out = ds.take(k).map(Box::new);
                    *at = k;
                    if let Some(i) = ds.list.iter().position(|d| Some(d.id) == *back_to) {
                        ds.sel = i;
                    }
                }
            },
        }
    }
}

impl Drawings {
    /// Hand finished edits to the editor's stack, then apply what undo and redo reached.
    /// `true` when the drawings changed.
    pub fn sync_undo(&mut self, ed: &mut Editor) -> bool {
        for s in self.steps.drain(..) {
            ed.push_underlay(s);
        }
        let due = ed.take_underlay_due();
        for s in &due {
            s.swap(self);
        }
        !due.is_empty()
    }

    /// Return a drawing to the list and make it current; pick and recognition keep their
    /// drawings.
    fn put_back(&mut self, d: Drawing, at: usize) {
        let at = at.min(self.list.len());
        self.list.insert(at, d);
        if let Some(p) = &mut self.pick {
            if p.0 >= at {
                p.0 += 1;
            }
        }
        if let Some(r) = &mut self.recog {
            if r.drawing >= at {
                r.drawing += 1;
            }
        }
        self.sel = at;
        self.touch();
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Drawings, Origin};
    use super::*;
    use ddc_geom::v2;
    use ddc_io::drawing_model as dm;

    type Sig = Vec<(u64, Underlay, V2, f64)>;

    fn sig(ds: &Drawings) -> Sig {
        ds.list
            .iter()
            .map(|d| (d.id, d.u.clone(), d.offset, d.rot))
            .collect()
    }

    fn plan(ds: &mut Drawings, level: i64, name: &str) {
        ds.add(
            &dm::fixture::sample_plan(0),
            name,
            None,
            level,
            None,
            Origin::AsIs,
            v2(0.0, 0.0),
        );
    }

    fn setup(n: usize) -> (Editor, Drawings) {
        let ed = Editor::new_in_memory().unwrap();
        let mut ds = Drawings::default();
        for k in 0..n {
            plan(&mut ds, ed.level, &format!("d{k}"));
        }
        (ed, ds)
    }

    fn undo(ed: &mut Editor, ds: &mut Drawings) {
        ed.undo();
        ds.sync_undo(ed);
    }

    fn redo(ed: &mut Editor, ds: &mut Drawings) {
        ed.redo();
        ds.sync_undo(ed);
    }

    /// `act` is one gesture: one step, and undo and redo return exactly, twice over.
    fn round_trip(ed: &mut Editor, ds: &mut Drawings, act: impl FnOnce(&mut Drawings)) {
        let before = sig(ds);
        let steps = ed.undo_len();
        act(ds);
        ds.sync_undo(ed);
        let after = sig(ds);
        assert_ne!(after, before, "the gesture changed nothing");
        assert_eq!(ed.undo_len(), steps + 1, "one gesture, one step");
        for _ in 0..2 {
            undo(ed, ds);
            assert_eq!(sig(ds), before, "undo");
            assert!(ed.can_redo());
            redo(ed, ds);
            assert_eq!(sig(ds), after, "redo");
        }
        undo(ed, ds);
        assert_eq!(sig(ds), before, "undo");
        assert_eq!(ed.undo_len(), steps);
    }

    /// Pick one wall line of drawing 0 by a click on its middle.
    fn pick_line(ds: &mut Drawings, level: i64) {
        ds.sel = 0;
        ds.pick_mode = true;
        let l = ds.list[0]
            .u
            .lines
            .iter()
            .find(|l| l.layer == "A-WALL")
            .unwrap()
            .clone();
        let mid = v2((l.a.0 + l.b.0) / 2.0, (l.a.1 + l.b.1) / 2.0);
        assert!(ds.pick_click(level, mid, 0.01));
        assert!(ds.pick.is_some());
    }

    #[test]
    fn move_and_rotate_whole_drawing_round_trip() {
        let (mut ed, mut ds) = setup(1);
        for xf in [(10.0, -3.0, 0.0), (0.0, 0.0, 90.0), (1.5, 2.5, 33.0)] {
            ds.xf = xf;
            round_trip(&mut ed, &mut ds, |ds| {
                ds.apply_xf(true, false);
            });
        }
    }

    #[test]
    fn steps_stack_and_return_placement() {
        let (mut ed, mut ds) = setup(1);
        let start = sig(&ds);
        ds.xf = (3.0, -2.0, 90.0);
        ds.apply_xf(true, false);
        ds.sync_undo(&mut ed);
        let first = sig(&ds);
        ds.xf = (1.0, 0.0, 30.0);
        ds.apply_xf(true, false);
        ds.sync_undo(&mut ed);
        assert_ne!(ds.list[0].offset, first[0].2);
        undo(&mut ed, &mut ds);
        assert_eq!(sig(&ds), first);
        undo(&mut ed, &mut ds);
        assert_eq!(sig(&ds), start);
        assert_eq!(ds.list[0].rot, 0.0);
    }

    #[test]
    fn copy_whole_drawing_round_trip_keeps_id_and_current() {
        let (mut ed, mut ds) = setup(2);
        ds.sel = 0;
        let source = ds.list[0].id;
        ds.xf = (0.0, 5.0, 0.0);
        ds.apply_xf(true, true);
        ds.sync_undo(&mut ed);
        let copy = ds.list[2].id;
        assert_eq!(ds.sel, 2);
        undo(&mut ed, &mut ds);
        assert_eq!(ds.list.len(), 2);
        assert_eq!(ds.list[ds.sel].id, source, "back to the source");
        redo(&mut ed, &mut ds);
        assert_eq!(ds.list.len(), 3);
        assert_eq!(ds.list[2].id, copy, "redo brings the same drawing");
        undo(&mut ed, &mut ds);

        ds.sel = 0;
        round_trip(&mut ed, &mut ds, |ds| {
            ds.apply_xf(true, true);
        });
    }

    #[test]
    fn remove_drawing_round_trip_returns_it_to_its_place() {
        let (mut ed, mut ds) = setup(3);
        let id = ds.list[1].id;
        round_trip(&mut ed, &mut ds, |ds| ds.remove(1));
        assert_eq!(ds.list[1].id, id);
        assert_eq!(ds.list[1].name, "d1");
        assert_eq!(ds.sel, 1, "the restored drawing is current");
        assert!(!ds.list[1].layers.is_empty(), "layers came back");
        let name = ds.list[1].layers[0].name.clone();
        assert!(ds.list[1].layer(&name).is_some(), "layer lookup still works");
    }

    #[test]
    fn remove_undo_keeps_pick_and_recognition_on_their_drawings() {
        let (mut ed, mut ds) = setup(3);
        ds.sel = 2;
        ds.recognize(&ed);
        ds.pick = Some((
            2,
            dm::Pick {
                lines: vec![0],
                ..Default::default()
            },
        ));
        let id = ds.list[2].id;
        ds.remove(0);
        ds.sync_undo(&mut ed);
        assert_eq!(ds.recog.as_ref().map(|r| r.drawing), Some(1));
        undo(&mut ed, &mut ds);
        assert_eq!(ds.recog.as_ref().map(|r| ds.list[r.drawing].id), Some(id));
        assert_eq!(ds.pick.as_ref().map(|p| ds.list[p.0].id), Some(id));
    }

    #[test]
    fn primitive_move_copy_delete_round_trip() {
        let (mut ed, mut ds) = setup(1);
        let level = ed.level;
        ds.xf = (0.0, 1.0, 15.0);
        pick_line(&mut ds, level);
        round_trip(&mut ed, &mut ds, |ds| {
            ds.apply_xf(false, false);
        });
        assert!(ds.pick.is_none(), "stale indexes are gone");

        pick_line(&mut ds, level);
        let n = ds.list[0].u.lines.len();
        round_trip(&mut ed, &mut ds, |ds| {
            ds.apply_xf(false, true);
        });
        assert_eq!(ds.list[0].u.lines.len(), n);

        pick_line(&mut ds, level);
        round_trip(&mut ed, &mut ds, |ds| {
            ds.delete_pick();
        });
        assert_eq!(ds.list[0].u.lines.len(), n);
    }

    #[test]
    fn undo_keeps_order_with_model_edits_and_skips_reload() {
        let (mut ed, mut ds) = setup(1);
        let start = sig(&ds);
        ed.commit_walls(vec![(v2(0.0, 0.0), v2(3.0, 0.0))]);
        ds.xf = (2.0, 0.0, 0.0);
        ds.apply_xf(true, false);
        ds.sync_undo(&mut ed);
        let moved = sig(&ds);
        ed.commit_walls(vec![(v2(0.0, 3.0), v2(3.0, 3.0))]);
        assert_eq!(ed.walls.len(), 2);

        undo(&mut ed, &mut ds);
        assert_eq!((ed.walls.len(), sig(&ds) == moved), (1, true));
        let (head, revision) = (ed.model.change_head(), ed.revision);
        undo(&mut ed, &mut ds);
        assert_eq!((ed.walls.len(), sig(&ds) == start), (1, true));
        assert_eq!(ed.model.change_head(), head, "no model operation");
        assert_eq!(ed.revision, revision, "no reload");
        undo(&mut ed, &mut ds);
        assert_eq!(ed.walls.len(), 0);

        redo(&mut ed, &mut ds);
        assert_eq!((ed.walls.len(), sig(&ds) == start), (1, true));
        redo(&mut ed, &mut ds);
        assert_eq!((ed.walls.len(), sig(&ds) == moved), (1, true));
        redo(&mut ed, &mut ds);
        assert_eq!((ed.walls.len(), sig(&ds) == moved), (2, true));
    }

    #[test]
    fn several_undos_before_one_sync_apply_in_order() {
        let (mut ed, mut ds) = setup(1);
        let start = sig(&ds);
        for x in [1.0, 2.0, 3.0] {
            ds.xf = (x, 0.0, 10.0);
            ds.apply_xf(true, false);
            ds.sync_undo(&mut ed);
        }
        assert_eq!(ed.jump_history(0), 3);
        ds.sync_undo(&mut ed);
        assert_eq!(sig(&ds), start);
    }

    #[test]
    fn new_edit_after_undo_drops_redo() {
        let (mut ed, mut ds) = setup(1);
        let start = sig(&ds);
        ds.xf = (1.0, 0.0, 0.0);
        ds.apply_xf(true, false);
        ds.sync_undo(&mut ed);
        undo(&mut ed, &mut ds);
        ds.xf = (0.0, 4.0, 0.0);
        ds.apply_xf(true, false);
        ds.sync_undo(&mut ed);
        assert!(!ed.can_redo());
        undo(&mut ed, &mut ds);
        assert_eq!(sig(&ds), start);
    }

    #[test]
    fn nothing_to_do_makes_no_step() {
        let (mut ed, mut ds) = setup(1);
        let start = sig(&ds);
        ds.xf = (0.0, 0.0, 0.0);
        ds.apply_xf(true, false);
        ds.delete_pick();
        ds.remove(7);
        assert!(!ds.sync_undo(&mut ed));
        assert_eq!(ed.undo_len(), 0);
        assert_eq!(sig(&ds), start);
    }

    #[test]
    fn history_panel_names_the_step() {
        let (mut ed, mut ds) = setup(1);
        ds.remove(0);
        ds.sync_undo(&mut ed);
        assert_eq!(ed.history().0, vec!["hist.underlay".to_string()]);
    }

    #[test]
    fn budget_drops_oldest_underlay_steps_only() {
        let (mut ed, mut ds) = setup(1);
        ed.commit_walls(vec![(v2(0.0, 0.0), v2(3.0, 0.0))]);
        let mut states = vec![];
        for x in [1.0, 2.0, 3.0] {
            ds.xf = (x, 0.0, 0.0);
            ds.apply_xf(true, false);
            ds.sync_undo(&mut ed);
            states.push(sig(&ds));
        }
        assert_eq!(ed.undo_len(), 4);
        ed.trim_underlay(2 * prims(&ds.list[0].u));
        assert_eq!(ed.undo_len(), 3, "oldest underlay step dropped");
        assert_eq!(ed.history().0[0], "hist.walls_add", "model step kept");
        undo(&mut ed, &mut ds);
        assert_eq!(sig(&ds), states[1]);
        undo(&mut ed, &mut ds);
        assert_eq!(sig(&ds), states[0], "the dropped step stays applied");
        undo(&mut ed, &mut ds);
        assert_eq!(ed.walls.len(), 0);

        // the newest step survives any budget
        redo(&mut ed, &mut ds);
        redo(&mut ed, &mut ds);
        redo(&mut ed, &mut ds);
        ed.trim_underlay(0);
        assert_eq!(ed.undo_len(), 2);
        undo(&mut ed, &mut ds);
        assert_eq!(sig(&ds), states[1]);
    }

    #[test]
    fn freed_copy_takes_its_copy_step_along() {
        let (mut ed, mut ds) = setup(2);
        ds.sel = 0;
        ds.xf = (0.0, 5.0, 0.0);
        ds.apply_xf(true, true);
        ds.sync_undo(&mut ed);
        ds.remove(2);
        ds.sync_undo(&mut ed);
        ds.sel = 0;
        let before = sig(&ds);
        ds.xf = (1.0, 0.0, 0.0);
        ds.apply_xf(true, false);
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), 3);
        ed.trim_underlay(0);
        assert_eq!(ed.undo_len(), 1, "no step left that undoes nothing");
        undo(&mut ed, &mut ds);
        assert_eq!(sig(&ds), before, "the newest step still works");
    }

    #[test]
    fn strings_are_in_both_languages() {
        use ddc_ui::i18n::{Catalog, Lang};
        let cyrillic = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c));
        let c = Catalog::new(Lang::En);
        let keys = [
            "hist.underlay",
            "dlg.dw.st.dwg_unavailable",
            "dlg.dw.st.unknown_format",
            "dlg.dw.st.no_settings_dir",
            "dlg.dw.st.modes_off_closed",
            "dlg.dw.st.modes_off_tool",
            "dlg.dw.st.trace_no_line",
            "dlg.dw.st.no_drawing",
            "dlg.dw.st.recognized",
            "dlg.dw.st.nothing_to_accept",
            "dlg.dw.st.drawing_gone",
            "dlg.dw.st.nothing_picked",
            "dlg.dw.st.created",
            "dlg.dw.st.accepted",
            "dlg.dw.st.xf_zero",
            "dlg.dw.st.copy_name",
            "dlg.dw.st.copied",
            "dlg.dw.st.moved",
            "dlg.dw.st.prims_copied",
            "dlg.dw.st.prims_changed",
            "dlg.dw.st.prims_deleted",
            "dlg.dw.st.removed",
            "dlg.dw.st.undone",
            "dlg.dw.st.hit_door",
            "dlg.dw.st.hit_window",
            "dlg.dw.st.hit_column",
            "dlg.dw.st.hit_wall",
            "dlg.dw.st.hit_grid",
            "dlg.dw.st.hit_room",
        ];
        for k in keys {
            let (en, ru) = (c.t_in(Lang::En, k), c.t_in(Lang::Ru, k));
            assert_ne!(en, k, "missing in en");
            assert!(!cyrillic(&en), "{k}: {en}");
            assert!(cyrillic(&ru), "{k}: missing in ru: {ru}");
        }
    }
}
