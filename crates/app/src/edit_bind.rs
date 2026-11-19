//! Edit panels for walls and curtain walls on the selected model element: state is read from
//! `Editor`, each change after a frame is one `run_op` operation (one undo step), then the
//! panel is re-read from the model. Ribbon commands are the `commands.d/t94.json` fragment.

use crate::curtain_edit::CurtainEditor;
use crate::editor::{Editor, OpFn, Sel};
use crate::opening_tool::Kind;
use crate::ui_kit as kit;
use crate::wall_edit::{Corner, Insert, WallEdit};
use ddc_geom::curtain::Axis;
use ddc_geom::curtain_edit::CurtainEdit;
use ddc_geom::wall_edit::{Band, Flip, StackPart, Structure};
use ddc_geom::{v2, V2};
use ddc_model::{Model, Swing, WallChange};
use std::sync::Arc;

const EPS: f64 = 1e-6;

fn op(f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static) -> OpFn {
    OpFn(Arc::new(f))
}

fn run(ed: &mut Editor, f: OpFn) -> Result<(), String> {
    ed.run_op(f).map_err(|e| e.to_string())
}

/// Edit panel for one model wall.
pub struct WallPanel {
    pub wall: i64,
    pub wall_type: i64,
    /// (wall, end): whose joint each panel corner edits.
    pub ends: Vec<(i64, u8)>,
    /// Model opening ids in `edit.inserts` order.
    pub ids: Vec<i64>,
    pub edit: WallEdit,
    saved: WallEdit,
    /// Editor revision the panel is in sync with (an external undo means re-read).
    pub rev: u64,
}

/// A wall strip with real faces: layers run from `offset0` (the right face) leftward.
fn band_of(ed: &Editor, w: &crate::editor::Wall) -> (Band, Vec<f64>) {
    let layers = ed
        .types
        .iter()
        .find(|t| t.id == w.wall_type)
        .map_or(vec![0.2], |t| t.layers.clone());
    let total: f64 = layers.iter().sum();
    let off = ed.model.wall_offset0(w.id).unwrap_or(-total / 2.0);
    (
        Band {
            a: w.a,
            b: w.b,
            left: off + total,
            right: -off,
        },
        layers,
    )
}

fn on_segment(p: V2, a: V2, b: V2) -> bool {
    let d = b - a;
    let t = ((p - a).dot(d) / d.dot(d).max(1e-18)).clamp(0.0, 1.0);
    (a + d * t - p).len() < EPS
}

fn layer_prios(ed: &Editor, wtype: i64, layers: &[f64]) -> (Vec<(f64, u8)>, Vec<String>) {
    let flags = ed.model.wall_layer_flags(wtype).unwrap_or_default();
    let names = ed.model.wall_layer_materials(wtype).unwrap_or_default();
    (
        layers
            .iter()
            .enumerate()
            .map(|(k, &t)| (t, flags.get(k).map_or(1, |f| f.1)))
            .collect(),
        (0..layers.len())
            .map(|k| names.get(k).cloned().unwrap_or_default())
            .collect(),
    )
}

impl WallPanel {
    pub fn for_wall(ed: &Editor, id: i64) -> Option<Self> {
        let w = ed.walls.iter().find(|w| w.id == id)?;
        let name = ed
            .types
            .iter()
            .find(|t| t.id == w.wall_type)
            .map(|t| t.name.clone())
            .unwrap_or_default();
        let (band, layers) = band_of(ed, w);
        let thick: f64 = layers.iter().sum();
        let (pri, names) = layer_prios(ed, w.wall_type, &layers);
        let (mut corners, mut ends) = (vec![], vec![]);
        let corner = |o: &crate::editor::Wall, at: V2, degree: usize, prefs| {
            let (ob, ol) = band_of(ed, o);
            let (opri, onames) = layer_prios(ed, o.wall_type, &ol);
            let mut c = Corner::new(band, ob, prefs, (pri.clone(), opri));
            c.names = (names.clone(), onames);
            c.at = at;
            c.degree = degree;
            c
        };
        // ends of this wall: a corner, a T-joint (end on a neighbor's middle) and joints of 3+ walls
        for (end, p) in [(0u8, w.a), (1u8, w.b)] {
            let near: Vec<&crate::editor::Wall> = ed
                .walls
                .iter()
                .filter(|o| o.id != id && on_segment(p, o.a, o.b))
                .collect();
            let Some(o) = near.first() else { continue };
            let prefs = ed.model.wall_join(id, end).unwrap_or_default();
            corners.push(corner(o, p, near.len() + 1, prefs));
            ends.push((id, end));
        }
        // walls whose end meets the middle of this one: their joint is edited from here too
        let mut faces = vec![];
        for o in ed.walls.iter().filter(|o| o.id != id) {
            for (end, p) in [(0u8, o.a), (1u8, o.b)] {
                if !on_segment(p, w.a, w.b) || (p - w.a).len() < EPS || (p - w.b).len() < EPS {
                    continue;
                }
                let prefs = ed.model.wall_join(o.id, end).unwrap_or_default();
                corners.push(corner(o, p, 2, prefs));
                ends.push((o.id, end));
                // the end face of the abutting wall is the face for an opening placed "by face"
                let (ob, _) = band_of(ed, o);
                let d = (o.b - o.a) * (1.0 / (o.b - o.a).len().max(1e-12));
                let n = v2(-d.y, d.x);
                let (l, r) = (p + n * ob.left, p - n * ob.right);
                faces.push(vec![(l, 0.0), (r, 0.0), (r, o.height), (l, o.height)]);
            }
        }
        let (mut inserts, mut ids) = (vec![], vec![]);
        for p in ed.openings.iter().filter(|p| p.wall == id) {
            inserts.push(Insert {
                along: p.along,
                width: p.width,
                sill: p.sill,
                height: p.height,
                door: p.kind == Kind::Door,
                flip: Flip {
                    facing: p.flip.side,
                    hand: p.flip.hinge,
                },
            });
            ids.push(p.id);
        }
        let n = layers.len();
        let structure = ed
            .model
            .wall_structure(w.wall_type, w.height)
            .unwrap_or_else(|_| Structure::new(layers.clone(), (0, n), w.height));
        let (sweep_ids, sweeps): (Vec<Option<i64>>, Vec<_>) = ed
            .model
            .wall_sweeps(id)
            .unwrap_or_default()
            .into_iter()
            .map(|(i, s)| (Some(i), s))
            .unzip();
        let stack_id = ed.model.wall_stack(id).ok().flatten();
        let stack = match ed.model.wall_stack_tiers(id, w.height).ok().flatten() {
            Some(t) => {
                let k = t.len();
                t.into_iter()
                    .enumerate()
                    .map(|(j, (t, name))| StackPart {
                        name,
                        thickness: t.thickness,
                        height: (j + 1 < k).then_some(t.z1 - t.z0),
                        offset: t.offset,
                    })
                    .collect()
            }
            None => vec![StackPart {
                name,
                thickness: thick,
                height: None,
                offset: 0.0,
            }],
        };
        let flags = ed.model.wall_layer_flags(w.wall_type).unwrap_or_default();
        let wi = ed.walls.iter().position(|x| x.id == id)?;
        let rings = ed
            .plan
            .iter()
            .enumerate()
            .filter(|(k, _)| {
                *k == wi
                    || ed
                        .walls
                        .get(*k)
                        .is_some_and(|o| corners.iter().any(|c| on_segment(c.at, o.a, o.b)))
            })
            .flat_map(|(k, rs)| rs.iter().map(move |r| (k == wi, r.clone())))
            .collect();
        let mut edit = WallEdit::new((w.b - w.a).len(), w.height, structure);
        edit.corners = corners;
        edit.stack = stack;
        edit.sweeps = sweeps;
        edit.sweep_ids = sweep_ids;
        edit.selected_insert = (!inserts.is_empty()).then_some(0);
        edit.inserts = inserts;
        edit.rings = rings;
        edit.band = Some(band);
        edit.faces = faces;
        edit.stack_types = ed.model.stack_types().unwrap_or_default();
        edit.stack_id = stack_id;
        edit.layer_info = (0..n)
            .map(|k| {
                let f = flags.get(k).copied().unwrap_or((false, 1));
                (names[k].clone(), layers[k], f.1, f.0)
            })
            .collect();
        Some(WallPanel {
            wall: id,
            wall_type: w.wall_type,
            ends,
            ids,
            saved: edit.clone(),
            edit,
            rev: ed.revision,
        })
    }

    /// The panel's difference from the model as one model change (None if unchanged).
    pub fn change(&self) -> Option<WallChange> {
        let (e, s) = (&self.edit, &self.saved);
        let mut c = WallChange {
            wall: self.wall,
            ..Default::default()
        };
        for (k, x) in e.corners.iter().enumerate() {
            if s.corners.get(k).map(|y| y.prefs) != Some(x.prefs) {
                let (w, end) = self.ends[k];
                c.joins.push((w, end, x.prefs));
            }
        }
        for (k, i) in e.inserts.iter().enumerate() {
            let old = s.inserts.get(k).copied();
            if old == Some(*i) {
                continue;
            }
            let swing = (old.map(|o| o.flip) != Some(i.flip)).then_some(Swing {
                side: i.flip.facing,
                hinge: i.flip.hand,
            });
            c.openings
                .push((self.ids[k], i.along, i.width, i.sill, i.height, swing));
        }
        let kept: Vec<i64> = e.sweep_ids.iter().flatten().copied().collect();
        c.sweeps_del = s
            .sweep_ids
            .iter()
            .flatten()
            .copied()
            .filter(|i| !kept.contains(i))
            .collect();
        for (k, sw) in e.sweeps.iter().enumerate() {
            match e.sweep_ids.get(k).copied().flatten() {
                None => c.sweeps_add.push(sw.clone()),
                Some(id) => {
                    let was = s
                        .sweep_ids
                        .iter()
                        .position(|x| *x == Some(id))
                        .map(|j| &s.sweeps[j]);
                    if was != Some(sw) {
                        c.sweeps_set.push((id, sw.clone()));
                    }
                }
            }
        }
        if e.structure != s.structure {
            c.structure = Some((self.wall_type, e.structure.clone()));
        }
        if e.stack_id != s.stack_id {
            c.stack = Some(e.stack_id);
        }
        c.voids = e.voids.clone();
        let empty = c.joins.is_empty()
            && c.openings.is_empty()
            && c.sweeps_add.is_empty()
            && c.sweeps_set.is_empty()
            && c.sweeps_del.is_empty()
            && c.structure.is_none()
            && c.stack.is_none()
            && c.voids.is_empty();
        (!empty).then_some(c)
    }

    /// All panel changes are one operation (one undo step); on error the panel
    /// is re-read from the model and shows the error. Returns the operation count (0 or 1).
    pub fn flush(&mut self, ed: &mut Editor) -> Result<usize, String> {
        let Some(c) = self.change() else {
            return Ok(0);
        };
        let r = run(ed, op(move |m| m.apply_wall_change(&c)));
        self.refresh(ed);
        r.map(|_| 1)
            .inspect_err(|e| self.edit.status = Some(e.clone()))
    }

    /// Re-read from the model (after an edit or undo), keeping the opening selection; if the wall
    /// is gone, the panel is marked empty.
    pub fn refresh(&mut self, ed: &Editor) {
        let sel = self.edit.selected_insert;
        match WallPanel::for_wall(ed, self.wall) {
            Some(p) => {
                *self = p;
                if sel.is_some_and(|k| k < self.edit.inserts.len()) {
                    self.edit.selected_insert = sel;
                }
            }
            None => {
                self.edit = WallEdit::new(0.0, 0.0, self.edit.structure.clone());
                self.saved = self.edit.clone();
                self.edit.status = Some(crate::tr("modify_ops.bind.wall_gone", &[]));
            }
        }
        self.rev = ed.revision;
    }

    /// The wall still exists in the model.
    pub fn alive(&self, ed: &Editor) -> bool {
        ed.walls.iter().any(|w| w.id == self.wall)
    }
}

/// Edit panel for one model curtain wall.
pub struct CurtainPanel {
    pub cw: i64,
    pub edit: CurtainEditor,
    saved: CurtainEdit,
    pub rev: u64,
}

impl CurtainPanel {
    pub fn for_curtain(ed: &Editor, id: i64) -> Option<Self> {
        let c = ed.curtains.iter().find(|c| c.id == id)?;
        let s = c.spec();
        let axis = Axis::new(v2(s.a.0, s.a.1), v2(s.b.0, s.b.1), s.bulge).ok()?;
        // edits made after the wall is resized are carried over to the new length/height
        let e = match ed.model.curtain_edit(id).ok().flatten() {
            Some(e) => e.fit(axis.length(), s.height),
            None => {
                let l = &c.layout.layout;
                let z = l.v.first().copied().unwrap_or(0.0);
                let v = l.v.iter().map(|x| x - z).collect();
                CurtainEdit::new(l.u.clone(), v, s.interior.clone(), s.border.clone())
            }
        };
        Some(CurtainPanel {
            cw: id,
            rev: ed.revision,
            saved: e.clone(),
            edit: CurtainEditor {
                axis,
                e,
                sel: vec![],
                sel_seg: None,
                status: None,
            },
        })
    }

    pub fn flush(&mut self, ed: &mut Editor) -> Result<usize, String> {
        if self.edit.e == self.saved {
            return Ok(0);
        }
        let (cw, e) = (self.cw, self.edit.e.clone());
        run(ed, op(move |m| m.save_curtain_edit(cw, &e)))?;
        self.saved = self.edit.e.clone();
        Ok(1)
    }

    /// Re-read (with no edits, the wall's original grid), keeping the selection.
    pub fn refresh(&mut self, ed: &Editor) {
        let (sel, seg) = (self.edit.sel.clone(), self.edit.sel_seg);
        if let Some(p) = CurtainPanel::for_curtain(ed, self.cw) {
            *self = p;
            self.edit.sel = sel;
            self.edit.sel_seg = seg;
        } else {
            self.edit.status = Some(crate::tr("modify_ops.bind.cw_gone", &[]));
        }
        self.rev = ed.revision;
    }
}

fn selected_wall(ed: &Editor) -> Option<i64> {
    ed.selection.iter().find_map(|s| match *s {
        Sel::Wall(id) => Some(id),
        Sel::Opening(o) => ed.openings.iter().find(|p| p.id == o).map(|p| p.wall),
        _ => None,
    })
}

fn selected_curtain(ed: &Editor) -> Option<i64> {
    ed.selection.iter().find_map(|s| match *s {
        Sel::Curtain(id) | Sel::Panel(id, _) => Some(id),
        _ => None,
    })
}

/// Commands of the contextual "Modify" ribbon.
pub fn register(a: &mut crate::shell::EditorApp) {
    a.register_command("mod.wall_edit", |a| {
        a.wall_edit = match a.wall_edit.take() {
            Some(_) => None,
            None => {
                let p = selected_wall(&a.ed).and_then(|id| WallPanel::for_wall(&a.ed, id));
                if p.is_none() {
                    a.ed.status = Some(crate::tr("modify_ops.bind.pick_wall", &[]));
                }
                p
            }
        };
    });
    a.register_command("mod.curtain_edit", |a| {
        a.curtain_edit = match a.curtain_edit.take() {
            Some(_) => None,
            None => {
                let p = selected_curtain(&a.ed).and_then(|id| CurtainPanel::for_curtain(&a.ed, id));
                if p.is_none() {
                    a.ed.status = Some(crate::tr("modify_ops.bind.pick_cw", &[]));
                }
                p
            }
        };
    });
}

/// Panel windows; edits go to the model after each frame, and the close button closes.
pub fn windows(a: &mut crate::shell::EditorApp, ctx: &egui::Context) {
    let t = crate::shell::tokens(&a.sh.theme);
    let size = egui::vec2(980.0, 460.0);
    if let Some(mut p) = a.wall_edit.take() {
        if a.ed.revision != p.rev {
            p.refresh(&a.ed);
        }
        let mut open = true;
        kit::Dialog::new(
            "wall_edit",
            a.sh.cat.t("cmd.mod.wall_edit"),
            kit::DialogSize::Workspace,
        )
        .size(size)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_min_size(size);
            p.edit.ui(ui, &t)
        });
        if let Err(e) = p.flush(&mut a.ed) {
            p.edit.status = Some(e);
        }
        p.rev = a.ed.revision;
        a.wall_edit = (open && p.alive(&a.ed)).then_some(p);
    }
    if let Some(mut p) = a.curtain_edit.take() {
        if a.ed.revision != p.rev {
            p.refresh(&a.ed);
        }
        let mut open = true;
        kit::Dialog::new(
            "curtain_edit",
            a.sh.cat.t("cmd.mod.curtain_edit"),
            kit::DialogSize::Workspace,
        )
        .size(size)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_min_size(size);
            p.edit.ui(ui, &t)
        });
        if let Err(e) = p.flush(&mut a.ed) {
            p.edit.status = Some(e);
        }
        p.rev = a.ed.revision;
        let alive = a.ed.curtains.iter().any(|c| c.id == p.cw);
        a.curtain_edit = (open && alive).then_some(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::curtain_edit::PanelType;
    use ddc_model::{OpeningSpec, WallSpec};

    /// A corner of three "Outer 380" walls, a door and a window on the lower one, a curtain wall on top.
    fn scene() -> (Editor, i64, i64) {
        let mut ed = Editor::new_in_memory().unwrap();
        let (lv, t) = (ed.level, ed.types[2].id);
        let segs = [
            ((0.0, 0.0), (6.0, 0.0)),
            ((6.0, 0.0), (6.0, 4.0)),
            ((0.0, 0.0), (0.0, 4.0)),
        ];
        ed.run_op(op(move |m| {
            for (a, b) in segs {
                let s = WallSpec {
                    a,
                    b,
                    level: lv,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: t,
                    offset0: None,
                };
                let w = m.add_wall(&s)?;
                if a == (0.0, 0.0) && b == (6.0, 0.0) {
                    for (along, width, sill, height, kind) in
                        [(0.8, 1.0, 0.0, 2.1, "door"), (3.2, 1.6, 0.9, 1.5, "window")]
                    {
                        let kind = kind.into();
                        m.add_opening(
                            w,
                            &OpeningSpec {
                                along,
                                width,
                                sill,
                                height,
                                kind,
                            },
                        )?;
                    }
                }
            }
            Ok(())
        }))
        .unwrap();
        // a T-abutment to the middle of the lower wall
        ed.run_op(op(move |m| {
            m.add_wall(&WallSpec {
                a: (3.0, 0.0),
                b: (3.0, 2.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .map(|_| ())
        }))
        .unwrap();
        ed.add_curtains(&[(v2(0.0, 4.0), v2(6.0, 4.0), 0.0)]);
        let w = ed.walls.iter().find(|w| w.b == v2(6.0, 0.0)).unwrap().id;
        let cw = ed.curtains[0].id;
        (ed, w, cw)
    }

    #[test]
    fn wall_panel_edits_model_in_one_undo_step() {
        let (mut ed, w, _) = scene();
        let mut p = WallPanel::for_wall(&ed, w).unwrap();
        // two corners plus a T-joint from the wall in the middle; its end face is for an opening "by face"
        assert_eq!((p.edit.corners.len(), p.edit.inserts.len()), (3, 2));
        assert_eq!(p.edit.faces.len(), 1);
        let tee = p.ends.iter().position(|e| e.0 != w).unwrap();
        assert!((p.edit.corners[tee].at - v2(3.0, 0.0)).len() < 1e-9);
        let band = p.edit.band.unwrap();
        let t: f64 = ed.types[2].layers.iter().sum();
        assert!((band.left + band.right - t).abs() < 1e-9, "real faces");
        let k0 = p.edit.corners[0].prefs.kind;
        p.edit.click_join(0, false);
        p.edit.click_join(tee, true);
        p.edit.commit_dim(1, "sill", 1.0).unwrap();
        p.edit.inserts[0].flip = p.edit.inserts[0].flip.flip_hand();
        let r = p.edit.structure.split(0, 0.9).unwrap();
        p.edit.structure.assign(r, 1).unwrap();
        p.edit.add_sweep(false);
        let rev = ed.revision;
        assert_eq!(p.flush(&mut ed).unwrap(), 1, "one model op");
        assert!(ed.revision > rev);
        let (jw, je) = p.ends[0];
        assert_eq!(ed.model.wall_join(jw, je).unwrap().kind, k0.cycle());
        let (tw, te) = p.ends[tee];
        assert!(!ed.model.wall_join(tw, te).unwrap().allowed);
        let win = ed.openings.iter().find(|o| o.id == p.ids[1]).unwrap();
        assert!((win.sill - 1.0).abs() < 1e-9);
        assert!(
            ed.openings
                .iter()
                .find(|o| o.id == p.ids[0])
                .unwrap()
                .flip
                .hinge
        );
        assert_eq!(p.edit.sweep_ids.len(), 1);
        assert!(
            p.edit.sweep_ids[0].is_some(),
            "panel re-read with model ids"
        );
        assert_eq!(p.flush(&mut ed).unwrap(), 0);
        // a profile edit and a delete are also one step each
        p.edit.sweeps[0].s1 = 3.0;
        assert_eq!(p.flush(&mut ed).unwrap(), 1);
        assert!((ed.model.wall_sweeps(w).unwrap()[0].1.s1 - 3.0).abs() < 1e-12);
        p.edit.delete_sweep(0);
        assert_eq!(p.flush(&mut ed).unwrap(), 1);
        assert!(ed.model.wall_sweeps(w).unwrap().is_empty());
        ed.undo();
        ed.undo();
        ed.undo();
        let win = ed.openings.iter().find(|o| o.id == p.ids[1]).unwrap();
        assert!(
            (win.sill - 0.9).abs() < 1e-9,
            "one undo reverts the whole panel edit"
        );
        assert_eq!(ed.model.wall_join(jw, je).unwrap().kind, k0);
        assert!(ed.model.wall_sweeps(w).unwrap().is_empty());
        p.refresh(&ed);
        assert_eq!(p.edit.inserts[1].sill, 0.9);
    }

    #[test]
    fn sweep_field_commit_and_delete_are_one_undo_step_each() {
        use crate::wall_edit::SweepField;
        let (mut ed, w, _) = scene();
        let mut p = WallPanel::for_wall(&ed, w).unwrap();
        p.edit.add_sweep(false);
        assert_eq!(p.flush(&mut ed).unwrap(), 1);
        let s1 = ed.model.wall_sweeps(w).unwrap()[0].1.s1;
        let steps = ed.undo_len();
        p.edit.set_sweep(0, SweepField::From, 0.5).unwrap();
        assert_eq!(p.flush(&mut ed).unwrap(), 1);
        assert_eq!(ed.undo_len(), steps + 1, "one value, one step");
        let sw = ed.model.wall_sweeps(w).unwrap()[0].1.clone();
        assert!((sw.s0 - 0.5).abs() < 1e-12 && (sw.s1 - s1).abs() < 1e-12);
        assert!(p.edit.set_sweep(0, SweepField::From, s1 + 1.0).is_err());
        assert_eq!(p.flush(&mut ed).unwrap(), 0, "refused value writes nothing");
        ed.undo();
        assert!(ed.model.wall_sweeps(w).unwrap()[0].1.s0.abs() < 1e-12);
        p.refresh(&ed);
        p.edit.delete_sweep(0);
        assert_eq!(p.flush(&mut ed).unwrap(), 1);
        assert_eq!(ed.undo_len(), steps + 1);
        ed.undo();
        assert_eq!(ed.model.wall_sweeps(w).unwrap().len(), 1, "delete undone in one step");
    }

    #[test]
    fn panel_voids_keep_kind_and_errors_refresh() {
        let (mut ed, w, _) = scene();
        let mut p = WallPanel::for_wall(&ed, w).unwrap();
        let face = p.edit.faces[0].clone();
        p.edit.opening_by_face(&face).unwrap();
        assert_eq!(p.flush(&mut ed).unwrap(), 1);
        let void: i64 = ed
            .model
            .sql()
            .query_row(
                "SELECT id FROM opening WHERE wall=?1 AND kind='void'",
                [w],
                |r| r.get(0),
            )
            .unwrap();
        // editing a door from the panel does not change opening kinds: a door stays a door, an empty one stays empty
        let mut p = WallPanel::for_wall(&ed, w).unwrap();
        p.edit.inserts[0].along += 0.1;
        assert_eq!(p.flush(&mut ed).unwrap(), 1);
        assert_eq!(ed.model.opening_kind(void).unwrap(), "void");
        assert_eq!(ed.model.opening_kind(p.ids[0]).unwrap(), "door");
        // model error: the panel is re-read and the error is shown
        p.edit.inserts[0].along = p.edit.inserts[1].along;
        p.edit.inserts[0].sill = p.edit.inserts[1].sill;
        assert!(p.flush(&mut ed).is_err());
        assert!(p.edit.status.is_some());
        assert_ne!(p.edit.inserts[0].along, p.edit.inserts[1].along);
    }

    #[test]
    fn sweep_and_stack_change_wall_mesh() {
        let (mut ed, w, _) = scene();
        let vol = |ed: &Editor| ed.walls.iter().find(|x| x.id == w).unwrap().volume;
        let tris = |ed: &Editor| -> usize {
            let i = ed.walls.iter().position(|x| x.id == w).unwrap() as u32;
            ed.scene_report()
                .0
                .meshes
                .iter()
                .filter(|m| m.element == i)
                .map(|m| m.triangles.len())
                .sum()
        };
        let (v0, t0) = (vol(&ed), tris(&ed));
        let mut p = WallPanel::for_wall(&ed, w).unwrap();
        p.edit.add_sweep(false);
        p.flush(&mut ed).unwrap();
        assert!(vol(&ed) > v0 + 1e-6, "sweep adds volume");
        assert!(tris(&ed) > t0, "sweep body in the 3D mesh");
        ed.undo();
        assert!((vol(&ed) - v0).abs() < 1e-9);
        let t = ed.types[2].id;
        let st = ed.model.stack_types().unwrap();
        assert!(st.is_empty());
        ed.run_op(op(move |m| {
            m.add_stack_type("Plinth+wall", &[(t, Some(0.6), 0.05), (t, None, 0.0)])
                .map(|_| ())
        }))
        .unwrap();
        let mut p = WallPanel::for_wall(&ed, w).unwrap();
        p.edit.stack_id = Some(p.edit.stack_types[0].0);
        p.flush(&mut ed).unwrap();
        assert_eq!(p.edit.stack.len(), 2, "tiers persisted and re-read");
        assert!(tris(&ed) > 0);
    }

    #[test]
    fn curtain_panel_edits_model() {
        let (mut ed, _, cw) = scene();
        let mut p = CurtainPanel::for_curtain(&ed, cw).unwrap();
        p.edit.e.set_panel((0, 0), PanelType::Door).unwrap();
        assert_eq!(p.flush(&mut ed).unwrap(), 1);
        let e = ed.model.curtain_edit(cw).unwrap().unwrap();
        assert_eq!(e.kind_of((0, 0)), PanelType::Door);
        assert_eq!(p.flush(&mut ed).unwrap(), 0);
        ed.undo();
        assert!(ed.model.curtain_edit(cw).unwrap().is_none());
        p.refresh(&ed);
    }

    #[test]
    fn ribbon_commands_on_selection() {
        let (ed, w, cw) = scene();
        let mut a = crate::shell::EditorApp::new(ed, None, "test".into());
        for (id, cat) in [("mod.wall_edit", "wall"), ("mod.curtain_edit", "curtain")] {
            let ids: Vec<_> =
                a.sh.reg
                    .contextual(cat)
                    .iter()
                    .map(|c| c.id.clone())
                    .collect();
            assert!(ids.iter().any(|i| i == id), "{id} not on ctx tab for {cat}");
        }
        a.ed.selection = vec![Sel::Wall(w)];
        assert!(a.run("mod.wall_edit"));
        assert_eq!(a.wall_edit.as_ref().map(|p| p.wall), Some(w));
        a.ed.selection = vec![Sel::Curtain(cw)];
        assert!(a.run("mod.curtain_edit"));
        assert_eq!(a.curtain_edit.as_ref().map(|p| p.cw), Some(cw));
    }

    /// Snapshot scene: a changed joint, a cornice on the lower wall, a composite right wall,
    /// and a curtain wall with a door, an added line and merged panels.
    fn edited() -> (Editor, i64, i64) {
        let (mut ed, w, cw) = scene();
        let mut p = WallPanel::for_wall(&ed, w).unwrap();
        p.edit.click_join(0, false);
        p.edit.add_sweep(false);
        p.flush(&mut ed).unwrap();
        let t = ed.types[2].id;
        ed.run_op(op(move |m| {
            m.add_stack_type("Plinth+wall", &[(t, Some(0.8), 0.06), (t, None, 0.0)])
                .map(|_| ())
        }))
        .unwrap();
        let right = ed.walls.iter().find(|x| x.a == v2(6.0, 0.0)).unwrap().id;
        let mut r = WallPanel::for_wall(&ed, right).unwrap();
        r.edit.stack_id = r.edit.stack_types.first().map(|s| s.0);
        r.flush(&mut ed).unwrap();
        let mut c = CurtainPanel::for_curtain(&ed, cw).unwrap();
        let _ = c.edit.e.set_panel((1, 0), PanelType::Door);
        let _ = c.edit.e.merge((3, 1), (3, 2));
        let mid = (c.edit.e.u[0] + c.edit.e.u[1]) / 2.0;
        let _ = c.edit.e.add_line(true, mid);
        c.flush(&mut ed).unwrap();
        ed.status = None;
        (ed, w, cw)
    }

    /// The app's 3D view (light+dark) in docs/design/shots/edit/<name>-<theme>.png.
    fn shoot_app(name: &str, open: &dyn Fn(&mut crate::shell::EditorApp, i64, i64)) {
        let (pick, _) = crate::gfx::choose(&crate::gfx::candidates(None, false), &mut |_| {})
            .expect("graphics adapter");
        let rs = crate::shot::render_state(pick).unwrap();
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/design/shots/edit");
        std::fs::create_dir_all(&dir).unwrap();
        let size = (1440u32, 900u32);
        for theme in ["light", "dark"] {
            let (ed, w, cw) = edited();
            let mut a = crate::shell::EditorApp::new(ed, Some(&rs), "shot".into());
            a.view3d = true;
            a.prefs.theme = theme.into();
            open(&mut a, w, cw);
            let ctx = egui::Context::default();
            let px = crate::shot::frame(&rs, &ctx, size, 1.0, 8, None, |c, _| a.ui(c, Some(&rs)));
            crate::shot::save_png(&dir.join(format!("{name}-{theme}.png")), size, &px).unwrap();
        }
    }

    /// Snapshots of panels and 3D on model elements (light+dark) in docs/design/shots/edit/.
    /// `cargo test -p ddc-app shots_edit -- --ignored`
    #[test]
    #[ignore]
    fn shots_edit() {
        let (ed, w, cw) = edited();
        let mut p = WallPanel::for_wall(&ed, w).unwrap();
        p.edit.selected_insert = Some(1);
        crate::curtain_edit::tests::shoot("wall-joins", |ui, t| p.edit.ui(ui, t));
        let right = ed.walls.iter().find(|x| x.a == v2(6.0, 0.0)).unwrap().id;
        let r = WallPanel::for_wall(&ed, right).unwrap();
        crate::curtain_edit::tests::shoot("wall-stacked", |ui, t| r.edit.clone().ui(ui, t));
        let mut c = CurtainPanel::for_curtain(&ed, cw).unwrap();
        c.edit.sel = vec![(2, 1)];
        crate::curtain_edit::tests::shoot("curtain", |ui, t| c.edit.ui(ui, t));
        shoot_app("edit-3d", &|_, _, _| {});
        shoot_app("curtain-3d", &|a, _, cw| {
            a.ed.selection = vec![Sel::Curtain(cw)];
        });
    }
}
