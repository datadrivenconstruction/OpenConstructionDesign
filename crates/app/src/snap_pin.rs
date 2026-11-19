// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Pin" of a snap hit: the alignment the hit stands for becomes a rule in `constraint_rule`,
//! and the solver keeps it afterwards. One table decides which kinds can be pinned: the rule
//! kinds only know walls, grid axes and levels, so end, middle and crossing points have no pin.

use crate::editor::Editor;
use crate::snap::{Guide, Snap, SnapKind};
use crate::snap_core::{Feat, SnapHit};
use ddc_model::{ConstraintKind, ConstraintSpec, Error, Target};

/// What the pin needs to know beyond the hit.
#[derive(Clone, Debug, Default)]
pub struct PinCtx {
    /// Grid axis the hit lies on or tracks.
    pub grid: Option<i64>,
    /// Wall a guide hit extends or runs parallel to.
    pub host: Option<i64>,
    /// Walls of the chain drawn so far, last one the previous step (for an equal step).
    pub chain: Vec<i64>,
    /// Distance typed along a guide from a grid axis, m.
    pub typed: Option<f64>,
}

/// The rule kind a snap kind pins to; `None` is the explicit "no pin" (every kind is listed).
pub const fn pin_kind(k: SnapKind) -> Option<ConstraintKind> {
    use ConstraintKind as C;
    match k {
        SnapKind::GridAxis => Some(C::AlignGrid),
        SnapKind::WallAxis | SnapKind::Extension => Some(C::Align),
        SnapKind::Perpendicular => Some(C::Perp),
        SnapKind::Parallel => Some(C::Parallel),
        SnapKind::EqualStep => Some(C::Equal),
        SnapKind::Tracking(Guide::X) | SnapKind::Tracking(Guide::Y) => Some(C::LockDistGrid),
        SnapKind::Tracking(Guide::Extension) => Some(C::Align),
        SnapKind::Tracking(Guide::Cross) => None,
        SnapKind::Endpoint
        | SnapKind::Midpoint
        | SnapKind::WallFace
        | SnapKind::Angle(_)
        | SnapKind::Grid
        | SnapKind::Intersection
        | SnapKind::RefPlane
        | SnapKind::WorkPlane
        | SnapKind::Nearest
        | SnapKind::AlignX
        | SnapKind::AlignY
        | SnapKind::AlignXY
        | SnapKind::Center
        | SnapKind::Tangent
        | SnapKind::Param
        | SnapKind::Quadrant
        | SnapKind::Free => None,
    }
}

fn wall_ref(hit: &SnapHit) -> Option<i64> {
    hit.refs.iter().flatten().find_map(|r| match (r.target, r.feat) {
        (Target::Wall(id), Feat::Axis | Feat::Line) => Some(id),
        _ => None,
    })
}

/// The rule for pinning `hit` of the wall `new` being placed; `None` when the hit has no pin.
pub fn pin_rule(hit: &SnapHit, new: i64, cx: &PinCtx) -> Option<ConstraintSpec> {
    let kind = pin_kind(hit.kind)?;
    let spec = |walls: Vec<i64>, value| ConstraintSpec { kind, walls, value };
    match hit.kind {
        SnapKind::GridAxis => Some(spec(vec![new, cx.grid?], None)),
        SnapKind::WallAxis => Some(spec(vec![wall_ref(hit)?, new], None)),
        SnapKind::Perpendicular => Some(spec(vec![wall_ref(hit)?, new], None)),
        SnapKind::Extension | SnapKind::Parallel | SnapKind::Tracking(Guide::Extension) => {
            Some(spec(vec![cx.host?, new], None))
        }
        SnapKind::EqualStep if cx.chain.len() >= 2 => {
            let mut w = cx.chain.clone();
            w.push(new);
            Some(spec(w, None))
        }
        SnapKind::Tracking(_) => Some(spec(vec![new, cx.grid?], Some(cx.typed?))),
        _ => None,
    }
}

impl Editor {
    /// Write the pin of `hit` into the model; `Ok(None)` when the hit has no pin. A rule the
    /// solver cannot hold comes back as `Error::Conflict`, the model is left as it was.
    pub fn pin_snap(&mut self, hit: &SnapHit, new: i64, cx: &PinCtx) -> Result<Option<i64>, Error> {
        match pin_rule(hit, new, cx) {
            Some(spec) => self.model.add_constraint(&spec).map(Some),
            None => Ok(None),
        }
    }
}

impl Editor {
    /// Create an element and write the rule of its pinned snap hit in the same operation: one
    /// undo step removes both, redo brings both back. `make` returns the new wall's id.
    pub fn run_pinned(
        &mut self,
        make: impl Fn(&mut ddc_model::Model) -> Result<i64, Error> + Send + Sync + 'static,
        hit: Option<SnapHit>,
        cx_of: impl Fn(&ddc_model::Model, i64) -> PinCtx + Send + Sync + 'static,
    ) -> Result<(), Error> {
        let f = crate::editor::OpFn(std::sync::Arc::new(move |m: &mut ddc_model::Model| {
            let id = make(m)?;
            if let Some(spec) = hit.as_ref().and_then(|h| pin_rule(h, id, &cx_of(m, id))) {
                m.add_constraint(&spec)?;
            }
            Ok(())
        }));
        self.run_op(f)
    }
}

impl Editor {
    /// Commit the walls of a gesture; with a pin armed on the tool, the last wall and the rule of
    /// the pinned hit are written in one operation (one undo step).
    pub fn commit_walls_or_pinned(&mut self, segs: Vec<(ddc_geom::V2, ddc_geom::V2)>) {
        let (Some(mut hit), Some(t)) = (self.tool.pin_armed.take(), self.current_type()) else {
            return self.commit_walls(segs);
        };
        hit.refs = self.snap_refs(&Snap { p: hit.xy(), kind: hit.kind }, None);
        let (level, height, wall_type) = (self.level, self.height, t.id);
        let specs: Vec<ddc_model::WallSpec> = segs
            .iter()
            .map(|(a, b)| ddc_model::WallSpec { a: (a.x, a.y), b: (b.x, b.y), level, base_offset: 0.0, height, wall_type, offset0: None })
            .collect();
        let make = move |m: &mut ddc_model::Model| {
            let ids = m.add_walls(&specs)?;
            ids.last().copied().ok_or_else(|| Error::Invalid("no walls".into()))
        };
        let cx = |m: &ddc_model::Model, id: i64| PinCtx { grid: crate::constraints_ui::near_grid(m, id), ..Default::default() };
        self.status = Some(match self.run_pinned(make, Some(hit), cx) {
            Ok(()) => crate::tr("edst.walls_added", &[("n", &segs.len().to_string())]),
            Err(e) => crate::constraints_ui::report(&e),
        });
    }
}

/// Convenience for tests and callers holding a bare candidate.
pub fn bare(s: Snap) -> SnapHit {
    SnapHit::bare(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snap_core::SnapRef;
    use ddc_geom::v2;

    const ALL_KINDS: [SnapKind; 26] = [
        SnapKind::Endpoint,
        SnapKind::Midpoint,
        SnapKind::Perpendicular,
        SnapKind::WallAxis,
        SnapKind::WallFace,
        SnapKind::Angle(45),
        SnapKind::Grid,
        SnapKind::Intersection,
        SnapKind::GridAxis,
        SnapKind::RefPlane,
        SnapKind::WorkPlane,
        SnapKind::Nearest,
        SnapKind::AlignX,
        SnapKind::AlignY,
        SnapKind::AlignXY,
        SnapKind::Center,
        SnapKind::Tangent,
        SnapKind::Param,
        SnapKind::Quadrant,
        SnapKind::Extension,
        SnapKind::Parallel,
        SnapKind::Tracking(Guide::X),
        SnapKind::Tracking(Guide::Y),
        SnapKind::Tracking(Guide::Cross),
        SnapKind::EqualStep,
        SnapKind::Free,
    ];

    #[test]
    fn pin_table_covers_all_kinds() {
        use ConstraintKind as C;
        let want = |k| match k {
            SnapKind::GridAxis => Some(C::AlignGrid),
            SnapKind::WallAxis | SnapKind::Extension => Some(C::Align),
            SnapKind::Perpendicular => Some(C::Perp),
            SnapKind::Parallel => Some(C::Parallel),
            SnapKind::EqualStep => Some(C::Equal),
            SnapKind::Tracking(Guide::X | Guide::Y) => Some(C::LockDistGrid),
            _ => None,
        };
        let mut bits = 0u32;
        for k in ALL_KINDS {
            assert_eq!(pin_kind(k), want(k), "{k:?}");
            bits |= k.bit();
        }
        // every kind has its own bit (angles and aligns share theirs)
        assert_eq!(bits.count_ones(), 22);
        // corners, midpoints and crossings never pin: the rules only know walls, axes, levels
        for k in [SnapKind::Endpoint, SnapKind::Midpoint, SnapKind::Intersection, SnapKind::Free] {
            assert!(pin_rule(&SnapHit::bare(Snap { p: v2(0.0, 0.0), kind: k }), 1, &PinCtx::default()).is_none());
        }
    }

    #[test]
    fn rules_name_the_right_walls() {
        let hit = |kind, refs| SnapHit { kind, p: ddc_geom::v3(0.0, 0.0, 0.0), refs };
        let on_wall = [Some(SnapRef { target: Target::Wall(7), feat: Feat::Axis }), None];
        let cx = PinCtx { grid: Some(3), host: Some(9), chain: vec![4, 5], typed: Some(1.2) };
        let r = pin_rule(&hit(SnapKind::GridAxis, [None, None]), 11, &cx).unwrap();
        assert_eq!((r.kind, r.walls), (ConstraintKind::AlignGrid, vec![11, 3]));
        let r = pin_rule(&hit(SnapKind::WallAxis, on_wall), 11, &cx).unwrap();
        assert_eq!((r.kind, r.walls), (ConstraintKind::Align, vec![7, 11]));
        let r = pin_rule(&hit(SnapKind::Parallel, [None, None]), 11, &cx).unwrap();
        assert_eq!((r.kind, r.walls), (ConstraintKind::Parallel, vec![9, 11]));
        let r = pin_rule(&hit(SnapKind::EqualStep, [None, None]), 11, &cx).unwrap();
        assert_eq!((r.kind, r.walls), (ConstraintKind::Equal, vec![4, 5, 11]));
        let r = pin_rule(&hit(SnapKind::Tracking(Guide::X), [None, None]), 11, &cx).unwrap();
        assert_eq!((r.kind, r.walls, r.value), (ConstraintKind::LockDistGrid, vec![11, 3], Some(1.2)));
        // a hit without the wall or grid it names pins nothing
        assert!(pin_rule(&hit(SnapKind::WallAxis, [None, None]), 11, &cx).is_none());
        assert!(pin_rule(&hit(SnapKind::GridAxis, [None, None]), 11, &PinCtx::default()).is_none());
    }

    /// Pin a wall to grid "A", then move the axis: the wall follows through the solver.
    #[test]
    fn pin_grid_then_move_grid() {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::modify_tools::tests::walls(&mut ed, &[((6.0, 0.0), (6.0, 5.0))]);
        let wall = ed.walls[0].id;
        let g = ed.model.add_grid(&ddc_model::GridSpec::line("A", (6.0, -2.0), (6.0, 7.0))).unwrap();
        let hit = SnapHit::bare(Snap { p: v2(6.0, 2.0), kind: SnapKind::GridAxis });
        let cx = PinCtx { grid: Some(g), ..Default::default() };
        let id = ed.pin_snap(&hit, wall, &cx).unwrap().expect("pinned");
        assert!(ed.model.constraints().unwrap().iter().any(|c| c.0 == id && c.1.kind == ConstraintKind::AlignGrid));
        ed.model.update_grid(g, &ddc_model::GridSpec::line("A", (6.5, -2.0), (6.5, 7.0))).unwrap();
        ed.reload().unwrap();
        let w = &ed.walls[0];
        assert!((w.a.x - 6.5).abs() < 1e-9 && (w.b.x - 6.5).abs() < 1e-9, "{:?}", (w.a, w.b));
        // a hit without a pin writes nothing
        let mid = SnapHit::bare(Snap { p: v2(6.5, 2.5), kind: SnapKind::Midpoint });
        assert_eq!(ed.pin_snap(&mid, wall, &cx).unwrap(), None);
    }

    #[test]
    fn pin_is_one_undo_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let ty = ed.current_type().expect("type").id;
        let level = ed.level;
        let g = ed.model.add_grid(&ddc_model::GridSpec::line("A", (6.0, -2.0), (6.0, 7.0))).unwrap();
        let hit = SnapHit::bare(Snap { p: v2(6.0, 2.0), kind: SnapKind::GridAxis });
        let make = move |m: &mut ddc_model::Model| {
            m.add_wall(&ddc_model::WallSpec { a: (6.0, 0.0), b: (6.0, 5.0), level, base_offset: 0.0, height: 3.0, wall_type: ty, offset0: None })
        };
        ed.run_pinned(make, Some(hit), move |_, _| PinCtx { grid: Some(g), ..Default::default() }).unwrap();
        let rows = |ed: &Editor| (ed.model.constraints().unwrap().len(), ed.model.sql().query_row("SELECT COUNT(*) FROM wall", [], |r| r.get::<_, i64>(0)).unwrap());
        assert_eq!(rows(&ed), (1, 1));
        ed.undo();
        assert_eq!(rows(&ed), (0, 0), "one undo removes the wall and its rule");
        ed.redo();
        assert_eq!(rows(&ed), (1, 1), "redo brings both back");
    }

    #[test]
    fn every_family_has_a_usable_snap_profile() {
        for f in crate::elem::FAMILIES {
            let p = f.snap;
            assert!(p.has(SnapKind::Free), "{}: the free point is always there", f.table);
            assert!(p.kinds.count_ones() >= 4, "{}: a profile that offers almost nothing", f.table);
        }
        let col = crate::elem::by_table("col").unwrap().snap;
        assert!(col.has(SnapKind::Intersection) && col.has(SnapKind::Center));
        assert!(!col.has(SnapKind::WallFace) && !col.has(SnapKind::WallAxis));
    }
}
