// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Match type (MA): pick a source wall, slab, column or beam, then every click on a target of the
//! same category gives it the source type and nothing else. A window drag applies to every hit in
//! one step (the plan feeds `selection::in_box` hits to `apply`). Pinned and curtain targets and other categories are refused with a reason.

use crate::editor::bodies::BodyKind;
use crate::editor::{Editor, OpFn, Sel};
use crate::selection;
use ddc_geom::V2;
use ddc_model::Edit;
use std::sync::Arc;

pub enum MatchOut {
    Nothing,
    Done(String),
    Exit,
    Error(String),
}

/// Element families that carry a type this tool can copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Wall,
    Slab,
    Column,
    Beam,
}

impl Kind {
    pub fn of(s: Sel) -> Option<Kind> {
        match s {
            Sel::Wall(_) => Some(Kind::Wall),
            Sel::Slab(_) => Some(Kind::Slab),
            Sel::Body(BodyKind::Column, _) => Some(Kind::Column),
            Sel::Body(BodyKind::Beam, _) => Some(Kind::Beam),
            _ => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Kind::Wall => "match_type.kind.wall",
            Kind::Slab => "match_type.kind.slab",
            Kind::Column => "match_type.kind.column",
            Kind::Beam => "match_type.kind.beam",
        }
    }

    /// Row of the pin table the element lives in.
    fn pin_table(self) -> &'static str {
        match self {
            Kind::Wall => "wall",
            Kind::Slab => "slab",
            Kind::Column => "col",
            Kind::Beam => "beam",
        }
    }
}

fn id_of(s: Sel) -> i64 {
    match s {
        Sel::Wall(i) | Sel::Slab(i) | Sel::Body(_, i) => i,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    Category,
    Curtain,
    Pinned,
}

impl Why {
    pub fn text(self, src: Kind) -> String {
        let cat = crate::tr(src.key(), &[]);
        match self {
            Why::Category => crate::tr("match_type.refuse.category", &[("cat", &cat)]),
            Why::Curtain => crate::tr("match_type.refuse.curtain", &[]),
            Why::Pinned => crate::tr("match_type.refuse.pinned", &[]),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Source {
    pub sel: Sel,
    pub kind: Kind,
    pub ty: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verdict {
    None,
    Ok(Sel),
    Refused(Sel, Why),
}

#[derive(Clone, Debug)]
pub struct MatchTool {
    pub src: Option<Source>,
    pub cursor: V2,
    pub verdict: Verdict,
}

/// Type id of a wall, slab, column or beam.
pub fn type_of(ed: &Editor, s: Sel) -> Option<i64> {
    match s {
        Sel::Wall(id) => ed.walls.iter().find(|w| w.id == id).map(|w| w.wall_type),
        Sel::Slab(id) => crate::query::slab_spec(&ed.model, id).ok().map(|x| x.1),
        Sel::Body(BodyKind::Column, id) => ed.model.column(id).ok().map(|c| c.member_type),
        Sel::Body(BodyKind::Beam, id) => ed.model.beam(id).ok().map(|b| b.member_type),
        _ => None,
    }
}

/// The model edit that sets `ty` on `s` and touches nothing else; None when the type is
/// already `ty` or the element is gone.
fn edit_for(ed: &Editor, s: Sel, ty: i64) -> Option<Edit> {
    match s {
        Sel::Wall(id) => {
            let (mut spec, bulge) = crate::query::wall_spec(&ed.model, id).ok()?;
            if spec.wall_type == ty {
                return None;
            }
            spec.wall_type = ty;
            Some(Edit::Wall { id, spec, bulge })
        }
        Sel::Slab(id) => {
            let (level, st, offset) = crate::query::slab_spec(&ed.model, id).ok()?;
            (st != ty).then_some(Edit::Slab {
                id,
                level,
                slab_type: ty,
                offset,
            })
        }
        Sel::Body(BodyKind::Column, id) => {
            let mut spec = ed.model.column(id).ok()?;
            if spec.member_type == ty {
                return None;
            }
            spec.member_type = ty;
            Some(Edit::Column { id, spec })
        }
        Sel::Body(BodyKind::Beam, id) => {
            let mut spec = ed.model.beam(id).ok()?;
            if spec.member_type == ty {
                return None;
            }
            spec.member_type = ty;
            Some(Edit::Beam { id, spec })
        }
        _ => None,
    }
}

impl MatchTool {
    pub fn new(ed: &Editor) -> Self {
        let mut t = Self {
            src: None,
            cursor: ddc_geom::v2(0.0, 0.0),
            verdict: Verdict::None,
        };
        // a single supported element selected in advance is the source
        if let [s] = ed.selection[..] {
            let _ = t.pick(ed, s);
        }
        t
    }

    pub fn step_key(&self) -> &'static str {
        if self.src.is_none() {
            "pick"
        } else {
            "apply"
        }
    }

    /// Takes `s` as the source; Err is the reason it cannot be one.
    pub fn pick(&mut self, ed: &Editor, s: Sel) -> Result<(), String> {
        if matches!(s, Sel::Curtain(_) | Sel::Panel(..)) {
            return Err(Why::Curtain.text(Kind::Wall));
        }
        let (Some(kind), Some(ty)) = (Kind::of(s), type_of(ed, s)) else {
            return Err(crate::tr("match_type.pick_bad", &[]));
        };
        self.src = Some(Source { sel: s, kind, ty });
        Ok(())
    }

    /// Why `t` cannot take the source type; None when it can.
    pub fn refusal(&self, ed: &Editor, t: Sel) -> Option<Why> {
        let src = self.src?;
        if matches!(t, Sel::Curtain(_) | Sel::Panel(..)) {
            return Some(Why::Curtain);
        }
        if Kind::of(t) != Some(src.kind) {
            return Some(Why::Category);
        }
        if ed.model.is_pinned(src.kind.pin_table(), id_of(t)).unwrap_or(false) {
            return Some(Why::Pinned);
        }
        None
    }

    /// Element under the point: the first one of the source category, else the topmost
    /// candidate (so a refusal can name the reason).
    pub fn target_at(&self, ed: &Editor, p: V2, r: f64) -> Option<Sel> {
        let tops: Vec<Sel> = selection::candidates(ed, p, r, |_| true)
            .into_iter()
            .filter_map(|g| g.first().copied())
            .filter(|s| !matches!(s, Sel::Anno(_)))
            .collect();
        let want = self.src.map(|s| s.kind);
        tops.iter()
            .copied()
            .find(|s| want.is_some() && Kind::of(*s) == want)
            .or_else(|| tops.first().copied())
    }

    pub fn hover(&mut self, ed: &Editor, p: V2, r: f64) {
        self.cursor = p;
        self.verdict = match (self.src, self.target_at(ed, p, r)) {
            (Some(src), Some(t)) if t != src.sel => match self.refusal(ed, t) {
                Some(w) => Verdict::Refused(t, w),
                None => Verdict::Ok(t),
            },
            _ => Verdict::None,
        };
    }

    pub fn click(&mut self, ed: &mut Editor, p: V2, r: f64) -> MatchOut {
        let Some(t) = self.target_at(ed, p, r) else {
            return MatchOut::Nothing;
        };
        let Some(src) = self.src else {
            return match self.pick(ed, t) {
                Ok(()) => MatchOut::Nothing,
                Err(m) => MatchOut::Error(m),
            };
        };
        if t == src.sel {
            return MatchOut::Nothing;
        }
        if let Some(w) = self.refusal(ed, t) {
            return MatchOut::Error(w.text(src.kind));
        }
        self.apply(ed, &[t])
    }

    /// Gives `targets` the source type in one operation. Other categories are ignored,
    /// pinned and curtain targets are counted as skipped.
    pub fn apply(&mut self, ed: &mut Editor, targets: &[Sel]) -> MatchOut {
        let Some(src) = self.src else {
            return MatchOut::Nothing;
        };
        let (mut edits, mut pinned, mut curtain, mut other, mut eligible) =
            (vec![], 0usize, 0usize, 0usize, 0usize);
        for &t in targets {
            if t == src.sel {
                continue;
            }
            match self.refusal(ed, t) {
                Some(Why::Category) => other += 1,
                Some(Why::Curtain) => curtain += 1,
                Some(Why::Pinned) => pinned += 1,
                None => {
                    eligible += 1;
                    edits.extend(edit_for(ed, t, src.ty));
                }
            }
        }
        if edits.is_empty() {
            return if pinned > 0 {
                MatchOut::Error(Why::Pinned.text(src.kind))
            } else if curtain > 0 {
                MatchOut::Error(Why::Curtain.text(src.kind))
            } else if eligible > 0 {
                MatchOut::Done(crate::tr("match_type.same", &[]))
            } else if other > 0 {
                MatchOut::Error(Why::Category.text(src.kind))
            } else {
                MatchOut::Nothing
            };
        }
        let n = edits.len();
        let f = OpFn(Arc::new(move |m| m.apply_edits(&edits)));
        match ed.run_op(f) {
            Ok(()) => {
                let (ns, sk) = (n.to_string(), (pinned + curtain).to_string());
                MatchOut::Done(if pinned + curtain > 0 {
                    crate::tr("match_type.done_skipped", &[("n", &ns), ("skipped", &sk)])
                } else {
                    crate::tr("match_type.done", &[("n", &ns)])
                })
            }
            Err(e) => MatchOut::Error(crate::tr("match_type.failed", &[("err", &e.to_string())])),
        }
    }

    /// Esc: drop the source, then leave.
    pub fn escape(&mut self) -> MatchOut {
        if self.src.is_some() {
            self.src = None;
            self.verdict = Verdict::None;
            MatchOut::Nothing
        } else {
            MatchOut::Exit
        }
    }
}

/// Source outline, hover outline (accent when allowed, error when refused) and the refusal
/// reason in a pill at the cursor.
pub fn draw(
    p: &egui::Painter,
    ed: &Editor,
    m: &MatchTool,
    to_s: &dyn Fn(V2) -> egui::Pos2,
    t: &crate::ui_kit::Tokens,
) {
    let ov = t.ov();
    let outline = |sel: Sel, col: egui::Color32| {
        for o in selection::outlines(ed, sel) {
            let pts: Vec<egui::Pos2> = o.iter().map(|&q| to_s(q)).collect();
            if pts.len() < 3 {
                continue;
            }
            p.add(egui::Shape::convex_polygon(
                pts.clone(),
                col.gamma_multiply(0.3),
                egui::Stroke::NONE,
            ));
            let mut c = pts.clone();
            c.push(pts[0]);
            p.add(egui::Shape::line(c, egui::Stroke::new(1.5, col)));
        }
    };
    if let Some(src) = m.src {
        outline(src.sel, ov.preview);
    }
    match m.verdict {
        Verdict::None => {}
        Verdict::Ok(s) => outline(s, ov.hover),
        Verdict::Refused(s, why) => {
            outline(s, ov.error);
            let Some(src) = m.src else { return };
            let font = egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION);
            let text = why.text(src.kind);
            let at = to_s(m.cursor) + egui::vec2(14.0, 14.0);
            let g = p.layout_no_wrap(text, font, t.paper);
            let r = egui::Rect::from_min_size(at, g.size()).expand2(egui::vec2(6.0, 3.0));
            p.rect_filled(r, 3.0, ov.error);
            p.add(egui::epaint::TextShape::new(at, g, t.paper));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Sel;
    use crate::selection::BoxMode;
    use crate::modify_tools::tests::walls;
    use ddc_geom::v2;
    use ddc_model::{BeamSpec, ColumnSpec, LayerSpec, MemberShape, SlabSpec};

    fn layer() -> Vec<LayerSpec> {
        vec![LayerSpec {
            material: "concrete".into(),
            thickness: 0.2,
        }]
    }

    fn column(ed: &mut Editor, mt: i64, at: (f64, f64)) -> i64 {
        ed.model
            .add_column(&ColumnSpec {
                level: ed.level,
                top_level: None,
                member_type: mt,
                structural: true,
                at,
                base_offset: 0.0,
                top_offset: 3.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap()
    }

    fn beam(ed: &mut Editor, mt: i64, y: f64) -> i64 {
        ed.model
            .add_beam(&BeamSpec {
                level: ed.level,
                member_type: mt,
                a: (0.0, y, 3.0),
                b: (4.0, y, 3.0),
                bulge: 0.0,
                yjust: Default::default(),
                zjust: Default::default(),
                rotation: 0.0,
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: Default::default(),
                join_end: Default::default(),
            })
            .unwrap()
    }

    fn slab(ed: &mut Editor, ty: i64, x0: f64) -> i64 {
        ed.model
            .add_slab(&SlabSpec {
                level: ed.level,
                slab_type: ty,
                offset: 0.0,
                outer: vec![(x0, 0.0), (x0 + 3.0, 0.0), (x0 + 3.0, 3.0), (x0, 3.0)],
                holes: vec![],
            })
            .unwrap()
    }

    fn set_wall_type(ed: &mut Editor, id: i64, ty: i64) {
        let (mut spec, bulge) = crate::query::wall_spec(&ed.model, id).unwrap();
        spec.wall_type = ty;
        ed.model.apply_edits(&[Edit::Wall { id, spec, bulge }]).unwrap();
    }

    fn done(o: MatchOut) -> bool {
        matches!(o, MatchOut::Done(_))
    }

    #[test]
    fn match_wall_type_one_undo() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (ta, tb) = (
            ed.model.add_wall_type("A", &layer()).unwrap(),
            ed.model.add_wall_type("B", &layer()).unwrap(),
        );
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0)), ((0.0, 5.0), (4.0, 5.0))]);
        let (w0, w1) = (ed.walls[0].id, ed.walls[1].id);
        set_wall_type(&mut ed, w0, ta);
        set_wall_type(&mut ed, w1, tb);
        ed.reload().unwrap();
        let before = crate::query::wall_spec(&ed.model, w1).unwrap();
        let steps = ed.undo_len();
        let mut t = MatchTool::new(&ed);
        // first click picks the source, the second changes the target
        assert!(matches!(t.click(&mut ed, v2(2.0, 0.0), 0.1), MatchOut::Nothing));
        assert_eq!(t.src.map(|s| s.sel), Some(Sel::Wall(w0)));
        assert!(done(t.click(&mut ed, v2(2.0, 5.0), 0.1)));
        assert_eq!(ed.undo_len(), steps + 1);
        let after = crate::query::wall_spec(&ed.model, w1).unwrap();
        assert_eq!(after.0.wall_type, ta);
        // only the type changed
        let mut expect = before.0.clone();
        expect.wall_type = ta;
        assert_eq!(after.0, expect);
        assert_eq!(after.1, before.1);
        // source stays armed; one undo brings the old type back
        assert!(t.src.is_some());
        ed.undo();
        assert_eq!(crate::query::wall_spec(&ed.model, w1).unwrap().0.wall_type, tb);
        assert_eq!(ed.undo_len(), steps);
    }

    #[test]
    fn match_slab_and_column_types() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (sa, sb) = (
            ed.model.add_slab_type("SA", &layer()).unwrap(),
            ed.model.add_slab_type("SB", &layer()).unwrap(),
        );
        let s0 = slab(&mut ed, sa, 0.0);
        let s1 = slab(&mut ed, sb, 10.0);
        let shape = MemberShape::parse("rect 0.3 0.3").unwrap();
        let (ma, mb) = (
            ed.model.add_member_type("MA", &shape, "concrete").unwrap(),
            ed.model.add_member_type("MB", &shape, "concrete").unwrap(),
        );
        let c0 = column(&mut ed, ma, (1.0, 1.0));
        let c1 = column(&mut ed, mb, (2.0, 2.0));
        let b0 = beam(&mut ed, ma, 7.0);
        let b1 = beam(&mut ed, mb, 8.0);
        ed.reload().unwrap();
        let (slab_before, col_before, beam_before) = (
            crate::query::slab_spec(&ed.model, s1).unwrap(),
            ed.model.column(c1).unwrap(),
            ed.model.beam(b1).unwrap(),
        );

        let mut t = MatchTool::new(&ed);
        t.pick(&ed, Sel::Slab(s0)).unwrap();
        let steps = ed.undo_len();
        assert!(done(t.apply(&mut ed, &[Sel::Slab(s1)])));
        let after = crate::query::slab_spec(&ed.model, s1).unwrap();
        assert_eq!((after.0, after.1, after.2), (slab_before.0, sa, slab_before.2));
        assert_eq!(ed.undo_len(), steps + 1);

        t.pick(&ed, Sel::Body(BodyKind::Column, c0)).unwrap();
        assert!(done(t.apply(&mut ed, &[Sel::Body(BodyKind::Column, c1)])));
        let mut expect = col_before.clone();
        expect.member_type = ma;
        assert_eq!(ed.model.column(c1).unwrap(), expect);
        assert_eq!(ed.undo_len(), steps + 2);

        t.pick(&ed, Sel::Body(BodyKind::Beam, b0)).unwrap();
        assert!(done(t.apply(&mut ed, &[Sel::Body(BodyKind::Beam, b1)])));
        let mut expect = beam_before.clone();
        expect.member_type = ma;
        assert_eq!(ed.model.beam(b1).unwrap(), expect);
        assert_eq!(ed.undo_len(), steps + 3);

        // a target that already has the type is no undo step
        assert!(done(t.apply(&mut ed, &[Sel::Body(BodyKind::Beam, b1)])));
        assert_eq!(ed.undo_len(), steps + 3);
        ed.undo();
        assert_eq!(ed.model.beam(b1).unwrap(), beam_before);
    }

    #[test]
    fn other_category_refused_nothing_changes() {
        let mut ed = Editor::new_in_memory().unwrap();
        let st = ed.model.add_slab_type("SA", &layer()).unwrap();
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
        let w = ed.walls[0].id;
        let s = slab(&mut ed, st, 10.0);
        ed.add_curtains(&[(v2(0.0, 20.0), v2(6.0, 20.0), 0.0)]);
        let cw = ed.curtains[0].id;
        ed.reload().unwrap();
        let (head, steps) = (ed.model.change_head(), ed.undo_len());
        let mut t = MatchTool::new(&ed);
        t.pick(&ed, Sel::Wall(w)).unwrap();
        // slab, curtain wall and room are other categories
        for target in [Sel::Slab(s), Sel::Curtain(cw), Sel::Room(1)] {
            assert!(t.refusal(&ed, target).is_some());
            assert!(matches!(t.apply(&mut ed, &[target]), MatchOut::Error(_)));
        }
        // click on the slab (other category) is refused with text, model untouched
        let o = t.click(&mut ed, v2(11.0, 1.0), 0.1);
        assert!(matches!(&o, MatchOut::Error(m) if !m.is_empty()));
        assert_eq!(t.refusal(&ed, Sel::Curtain(cw)), Some(Why::Curtain));
        assert_eq!(t.refusal(&ed, Sel::Slab(s)), Some(Why::Category));
        assert!(matches!(t.apply(&mut ed, &[Sel::Curtain(cw)]), MatchOut::Error(_)));
        assert_eq!(ed.model.change_head(), head);
        assert_eq!(ed.undo_len(), steps);
        // a curtain wall is not a source either
        let mut t2 = MatchTool::new(&ed);
        assert!(t2.pick(&ed, Sel::Curtain(cw)).is_err());
        assert!(t2.src.is_none());
    }

    #[test]
    fn pinned_target_is_skipped_and_reported() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (ta, tb) = (
            ed.model.add_wall_type("A", &layer()).unwrap(),
            ed.model.add_wall_type("B", &layer()).unwrap(),
        );
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0)), ((0.0, 5.0), (4.0, 5.0)), ((0.0, 9.0), (4.0, 9.0))]);
        let ids: Vec<i64> = ed.walls.iter().map(|w| w.id).collect();
        set_wall_type(&mut ed, ids[0], ta);
        set_wall_type(&mut ed, ids[1], tb);
        set_wall_type(&mut ed, ids[2], tb);
        ed.model.pin(&[("wall", ids[1])]).unwrap();
        ed.reload().unwrap();
        let mut t = MatchTool::new(&ed);
        t.pick(&ed, Sel::Wall(ids[0])).unwrap();
        // pinned alone: refused, nothing changes
        let (head, steps) = (ed.model.change_head(), ed.undo_len());
        assert_eq!(t.refusal(&ed, Sel::Wall(ids[1])), Some(Why::Pinned));
        assert!(matches!(t.apply(&mut ed, &[Sel::Wall(ids[1])]), MatchOut::Error(_)));
        assert_eq!(ed.model.change_head(), head);
        assert_eq!(ed.undo_len(), steps);
        // pinned plus free: the free one changes, one step
        assert!(done(t.apply(&mut ed, &[Sel::Wall(ids[1]), Sel::Wall(ids[2])])));
        assert_eq!(ed.undo_len(), steps + 1);
        assert_eq!(type_of(&ed, Sel::Wall(ids[2])), Some(ta));
        assert_eq!(type_of(&ed, Sel::Wall(ids[1])), Some(tb));
    }

    #[test]
    fn window_match_one_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (ta, tb) = (
            ed.model.add_wall_type("A", &layer()).unwrap(),
            ed.model.add_wall_type("B", &layer()).unwrap(),
        );
        walls(
            &mut ed,
            &[
                ((0.0, 0.0), (4.0, 0.0)),
                ((0.0, 5.0), (4.0, 5.0)),
                ((0.0, 8.0), (4.0, 8.0)),
                ((0.0, 30.0), (4.0, 30.0)),
            ],
        );
        let ids: Vec<i64> = ed.walls.iter().map(|w| w.id).collect();
        for (i, id) in ids.iter().enumerate() {
            set_wall_type(&mut ed, *id, if i == 0 { ta } else { tb });
        }
        ed.reload().unwrap();
        let mut t = MatchTool::new(&ed);
        t.pick(&ed, Sel::Wall(ids[0])).unwrap();
        let steps = ed.undo_len();
        // the window takes walls 1 and 2 (and the source); the far wall stays
        let hits = selection::in_box(&ed, v2(-1.0, 3.0), v2(5.0, 9.0), BoxMode::Window, |_| true);
        assert_eq!(hits.len(), 2);
        let o = t.apply(&mut ed, &hits);
        assert!(matches!(&o, MatchOut::Done(m) if !m.is_empty()));
        assert_eq!(ed.undo_len(), steps + 1);
        assert_eq!(type_of(&ed, Sel::Wall(ids[1])), Some(ta));
        assert_eq!(type_of(&ed, Sel::Wall(ids[2])), Some(ta));
        assert_eq!(type_of(&ed, Sel::Wall(ids[3])), Some(tb));
        ed.undo();
        assert_eq!(type_of(&ed, Sel::Wall(ids[1])), Some(tb));
        assert_eq!(type_of(&ed, Sel::Wall(ids[2])), Some(tb));
        assert_eq!(ed.undo_len(), steps);
    }

    #[test]
    fn escape_drops_source_then_leaves() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
        let w = ed.walls[0].id;
        ed.selection = vec![Sel::Wall(w)];
        let mut t = MatchTool::new(&ed);
        assert_eq!(t.step_key(), "apply");
        assert!(matches!(t.escape(), MatchOut::Nothing));
        assert!(t.src.is_none());
        assert_eq!(t.step_key(), "pick");
        assert!(matches!(t.escape(), MatchOut::Exit));
    }
}
