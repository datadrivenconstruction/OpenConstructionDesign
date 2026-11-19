//! The plan lane inside a drafting view: preview of the edit tools in the view's own
//! coordinates, and the dimension, distribute and mark tools on the view's items. The hidden plan
//! is never read or written here; snaps come from the view's items alone.

use super::*;
use ddc_model::{AnnoShape, AnnoView, AnnoXf, Leader, TextAlign, TextNote};

/// The drafting-view tool that is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fk {
    Dim,
    Dist,
    Tag,
    Number,
}

/// Points of the running drafting-view tool; lives in the lane and is reset by every tool start.
#[derive(Debug, Default)]
pub(super) struct FState {
    kind: Option<Fk>,
    pts: Vec<V2>,
    cursor: V2,
}

/// Result of one click of a drafting-view tool.
#[derive(Debug, PartialEq)]
pub(super) enum Res {
    Nothing,
    Done,
    Refused(String),
    Error(String),
}

// ---------- pure geometry (view coordinates) ----------

/// The point of `vs` within `r` of `p`, nearest first; `p` itself when there is none.
pub(super) fn snap_pick(vs: &[V2], p: V2, r: f64) -> V2 {
    vs.iter()
        .copied()
        .filter(|q| (*q - p).len() <= r)
        .min_by(|a, b| (*a - p).len().total_cmp(&(*b - p).len()))
        .unwrap_or(p)
}

/// Dimension bracket: the two measured points, then the same two moved to the side of `place`.
pub(super) fn dim_polyline(a: V2, b: V2, place: V2) -> Option<[V2; 4]> {
    let d = b - a;
    let l = d.len();
    if l < 1e-9 {
        return None;
    }
    let n = v2(-d.y / l, d.x / l);
    let off = (place - a).dot(n);
    Some([a, a + n * off, b + n * off, b])
}

/// Shapes of a linear dimension: the bracket and its length text. `k`: view metres per sheet mm.
pub(super) fn dim_shapes(a: V2, b: V2, place: V2, label: &str, k: f64) -> Option<Vec<AnnoShape>> {
    let pl = dim_polyline(a, b, place)?;
    let mid = (pl[1] + pl[2]) * 0.5;
    let d = b - a;
    let mut rot = d.y.atan2(d.x).to_degrees();
    while rot > 90.0 {
        rot -= 180.0;
    }
    while rot <= -90.0 {
        rot += 180.0;
    }
    let (s, c) = rot.to_radians().sin_cos();
    // text pos is the top of the line: it hangs just above the dimension line
    let pos = mid + v2(-s, c) * (3.5 * k);
    let mut note = TextNote::new((pos.x, pos.y), label);
    note.rot_deg = rot;
    note.align = TextAlign::Center;
    Some(vec![
        AnnoShape::Line {
            pts: pl.iter().map(|q| (q.x, q.y)).collect(),
            closed: false,
        },
        AnnoShape::Text(note),
    ])
}

/// Offsets of the copies of a distribute run: `n` items from `start` to `end`, the original first.
pub(super) fn dist_offsets(start: V2, end: V2, n: usize) -> Vec<V2> {
    crate::annot_text::linear_xfs(end - start, n.max(2), true)
        .into_iter()
        .filter_map(|x| match x {
            AnnoXf::Move(d) => Some(v2(d.0, d.1)),
            _ => None,
        })
        .collect()
}

/// Text of a mark at `at`; with `leader` it sits `r * 4` up and right of `at` and points back.
pub(super) fn mark_shape(at: V2, r: f64, text: &str, leader: bool) -> AnnoShape {
    if !leader {
        let mut n = TextNote::new((at.x, at.y), text);
        n.align = TextAlign::Center;
        return AnnoShape::Text(n);
    }
    let mut n = TextNote::new((at.x + r * 4.0, at.y + r * 4.0), text);
    n.leaders.push(Leader {
        to: (at.x, at.y),
        elbow: None,
    });
    AnnoShape::Text(n)
}

/// Guide polylines the tool draws while it waits for the next point.
pub(super) fn preview_lines(kind: Fk, pts: &[V2], cursor: V2) -> Vec<Vec<V2>> {
    match (kind, pts) {
        (Fk::Dim, [a]) => vec![vec![*a, cursor]],
        (Fk::Dim, [a, b]) => dim_polyline(*a, *b, cursor)
            .map(|p| vec![p.to_vec()])
            .unwrap_or_default(),
        (Fk::Dist, [a]) => vec![vec![*a, cursor]],
        _ => vec![],
    }
}

/// Outlines of the edit tool's targets after the pending transform, in view coordinates.
pub(super) fn ghost_outlines(ed: &Editor, tool: &ModTool) -> Vec<Vec<V2>> {
    let Some(f) = tool.xf(ed) else {
        return vec![];
    };
    tool.targets
        .iter()
        .flat_map(|&s| selection::outlines(ed, s))
        .map(|o| o.iter().map(|&q| f(q)).collect())
        .collect()
}

// ---------- model actions ----------

fn drafting_view(ed: &Editor) -> Option<AnnoView> {
    match crate::annot_text::tool_view(ed) {
        v @ AnnoView::Drafting(_) => Some(v),
        _ => None,
    }
}

fn plan_only() -> Res {
    Res::Refused(crate::tr("drafting_tools.free.plan_only", &[]))
}

fn zero() -> Res {
    Res::Refused(crate::tr("drafting_tools.free.zero", &[]))
}

fn run(
    ed: &mut Editor,
    f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) -> Res {
    match ed.run_op(OpFn(Arc::new(f))) {
        Ok(()) => Res::Done,
        Err(e) => Res::Error(e.to_string()),
    }
}

/// New items in the view as one undo step.
fn add(ed: &mut Editor, view: AnnoView, shapes: Vec<AnnoShape>) -> Res {
    run(ed, move |m| {
        for s in &shapes {
            m.add_anno(view, s, None, None)?;
        }
        Ok(())
    })
}

/// Dimension: two points, then the side; the chain goes on after each dimension.
pub(super) fn dim_click(ed: &mut Editor, pts: &mut Vec<V2>, p: V2) -> Res {
    let Some(view) = drafting_view(ed) else {
        return plan_only();
    };
    match pts.len() {
        0 => {
            pts.push(p);
            Res::Nothing
        }
        1 => {
            if (p - pts[0]).len() < 1e-9 {
                return zero();
            }
            pts.push(p);
            Res::Nothing
        }
        _ => {
            let (a, b) = (pts[0], pts[1]);
            pts.clear();
            let k = ed.model.anno_scale(view).unwrap_or(100.0) / 1000.0;
            let label = display_len(ed, (b - a).len());
            match dim_shapes(a, b, p, &label, k) {
                Some(s) => add(ed, view, s),
                None => zero(),
            }
        }
    }
}

/// Distribute: the selected items of the view, `n` of them between two points.
pub(super) fn dist_click(ed: &mut Editor, pts: &mut Vec<V2>, p: V2, n: usize) -> Res {
    if drafting_view(ed).is_none() {
        return plan_only();
    }
    let ids: Vec<i64> = crate::annot_text::plan_targets(ed)
        .iter()
        .filter_map(|s| match s {
            Sel::Anno(id) => Some(*id),
            _ => None,
        })
        .collect();
    if ids.is_empty() {
        pts.clear();
        return Res::Refused(crate::tr("drafting_tools.free.select_first", &[]));
    }
    let Some(&start) = pts.first() else {
        pts.push(p);
        return Res::Nothing;
    };
    if (p - start).len() < 1e-9 {
        return zero();
    }
    pts.clear();
    let xfs = crate::annot_text::linear_xfs(p - start, n.max(2), true);
    run(ed, move |m| m.copy_annos(&ids, &xfs).map(|_| ()))
}

/// Mark: a numbered text at the click, or (`tag`) beside the item under it with a leader.
pub(super) fn mark_click(ed: &mut Editor, p: V2, r: f64, next: &mut String, tag: bool) -> Res {
    let Some(view) = drafting_view(ed) else {
        return plan_only();
    };
    if tag && crate::annot_text::item_at(ed, p, r).is_none() {
        return Res::Refused(crate::tr("drafting_tools.free.pick_item", &[]));
    }
    let shape = mark_shape(p, r, next, tag);
    let res = add(ed, view, vec![shape]);
    if res == Res::Done {
        *next = ddc_model::next_mark(next);
    }
    res
}

// ---------- the lane ----------

impl Lane {
    /// Input of the drafting tab for the dimension, distribute and mark tools; `None`: none of
    /// them is running.
    pub(super) fn drafting_free(
        &mut self,
        ui: &mut egui::Ui,
        resp: &egui::Response,
        ed: &mut Editor,
        to_w: &dyn Fn(Pos2) -> V2,
    ) -> Option<bool> {
        let kind = if let Some(d) = &self.dim {
            if d.mode != DimMode::Linear {
                ed.say(Severity::Refusal, crate::tr("drafting_tools.free.dim_kind", &[]));
                self.clear_tools(ed);
                return Some(true);
            }
            Fk::Dim
        } else if self.dist.is_some() {
            Fk::Dist
        } else if let Some(a) = &self.ann {
            if a.mode == AnnMode::Tag {
                Fk::Tag
            } else {
                Fk::Number
            }
        } else {
            return None;
        };
        let r = (to_w(Pos2::new(8.0, 0.0)) - to_w(Pos2::new(0.0, 0.0))).len();
        if self.free.kind != Some(kind) {
            self.free.kind = Some(kind);
            self.free.pts.clear();
        }
        let snap = |ed: &Editor, p: V2| {
            if matches!(kind, Fk::Dim | Fk::Dist) {
                snap_pick(&crate::annot_text::view_vertices(ed), p, r)
            } else {
                p
            }
        };
        if let Some(h) = resp.hover_pos() {
            let c = snap(ed, to_w(h));
            self.free.cursor = c;
            if let Some(d) = &mut self.dim {
                d.cursor = c;
            }
            if let Some(d) = &mut self.dist {
                d.cursor = c;
            }
            if let Some(a) = &mut self.ann {
                a.cursor = c;
            }
        }
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let p = snap(ed, to_w(pos));
                let res = self.free_click(ed, kind, p, r);
                match res {
                    Res::Refused(m) => ed.say(Severity::Refusal, m),
                    Res::Error(m) => ed.say(Severity::Error, m),
                    Res::Done if kind == Fk::Dist => self.clear_tools(ed),
                    Res::Done | Res::Nothing => {}
                }
            }
        }
        if crate::ui_kit::canvas_owns_keys(ui.ctx()) && !ui.ctx().wants_keyboard_input() {
            let pressed = |k: egui::Key| ui.input(|i| i.key_pressed(k));
            if pressed(egui::Key::Escape) {
                let had = !self.free.pts.is_empty();
                self.free.pts.clear();
                if !had {
                    self.clear_tools(ed);
                    return Some(true);
                }
            } else if pressed(egui::Key::Backspace) {
                self.free.pts.pop();
            }
            if let Some(d) = &mut self.dist {
                if pressed(egui::Key::ArrowUp) {
                    d.n += 1;
                }
                if pressed(egui::Key::ArrowDown) {
                    d.n = d.n.saturating_sub(1).max(2);
                }
            }
        }
        Some(true)
    }

    fn free_click(&mut self, ed: &mut Editor, kind: Fk, p: V2, r: f64) -> Res {
        let mut pts = std::mem::take(&mut self.free.pts);
        let res = match kind {
            Fk::Dim => dim_click(ed, &mut pts, p),
            Fk::Dist => {
                let n = self.dist.as_ref().map_or(4, |d| d.n);
                dist_click(ed, &mut pts, p, n)
            }
            Fk::Tag | Fk::Number => match &mut self.ann {
                Some(a) => mark_click(ed, p, r, &mut a.next, kind == Fk::Tag),
                None => Res::Nothing,
            },
        };
        self.free.pts = pts;
        res
    }

    /// Painting of the drafting tab over the view's items: edit-tool preview and the guides of
    /// the dimension and distribute tools, all in the view's own coordinates.
    pub fn draw_drafting(
        &self,
        ui: &egui::Ui,
        p: &egui::Painter,
        ed: &Editor,
        to_s: &dyn Fn(V2) -> Pos2,
        t: &Tokens,
    ) {
        let ov = t.ov();
        let (preview, temp) = (ov.preview, ov.snap);
        if let Some(tool) = crate::interact::sessions::modify::tool(ed) {
            if matches!(
                tool.op,
                Op::Move | Op::Copy | Op::Rotate | Op::MirrorDraw | Op::Array
            ) {
                draw_tool(p, ed, tool, to_s, preview, temp, t);
                let frame = ui.ctx().cumulative_pass_nr();
                if let Some(g) = tool.trial.ghost(ed.model.serial(), frame) {
                    crate::trial::paint(p, g, to_s, ov.ghost());
                }
            }
        }
        let kind = if self.dim.is_some() {
            Fk::Dim
        } else if self.dist.is_some() {
            Fk::Dist
        } else {
            return;
        };
        let (pts, cursor): (&[V2], V2) = (&self.free.pts, self.free.cursor);
        let st = Stroke::new(1.5, preview);
        for line in preview_lines(kind, pts, cursor) {
            let sp: Vec<Pos2> = line.iter().map(|&q| to_s(q)).collect();
            p.extend(egui::Shape::dashed_line(&sp, st, 6.0, 4.0));
        }
        for &q in pts {
            p.circle_stroke(to_s(q), 4.0, Stroke::new(1.5, temp));
        }
        p.circle_stroke(to_s(cursor), 5.0, Stroke::new(1.5, temp));
        if let (Fk::Dist, [a], Some(d)) = (kind, pts, &self.dist) {
            let targets = crate::annot_text::plan_targets(ed);
            for off in dist_offsets(*a, cursor, d.n) {
                draw_ghost(p, ed, &targets, &|q| q + off, to_s, preview);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Editor with a drafting view on screen holding one line, and one plan line.
    fn setup() -> (Editor, AnnoView, i64) {
        let mut e = Editor::new_in_memory().unwrap();
        let line = AnnoShape::Line {
            pts: vec![(0.0, 0.0), (4.0, 0.0)],
            closed: false,
        };
        let level = e.level;
        let l2 = line.clone();
        e.run_op(OpFn(Arc::new(move |m| {
            m.add_anno(AnnoView::Plan(level), &l2, None, None).map(|_| ())
        })))
        .unwrap();
        e.run_op(OpFn(Arc::new(|m| m.add_drafting("Detail", 10.0).map(|_| ()))))
            .unwrap();
        let d = e.model.draftings().unwrap()[0].id;
        let view = AnnoView::Drafting(d);
        e.run_op(OpFn(Arc::new(move |m| {
            m.add_anno(view, &line, None, None).map(|_| ())
        })))
        .unwrap();
        crate::annot_text::sync_drafting(&mut e, d);
        let did = e.model.annos(view).unwrap()[0].id;
        (e, view, did)
    }

    fn counts(e: &Editor, view: AnnoView) -> (usize, usize) {
        (
            e.model.annos(view).unwrap().len(),
            e.model.annos(AnnoView::Plan(e.level)).unwrap().len(),
        )
    }

    #[test]
    fn dim_makes_bracket_and_text_in_the_view_only_one_undo() {
        let (mut e, view, _) = setup();
        let steps = e.undo_len();
        let mut pts = vec![];
        for p in [v2(0.0, 0.0), v2(4.0, 0.0), v2(2.0, 1.5)] {
            let r = dim_click(&mut e, &mut pts, p);
            assert!(!matches!(r, Res::Error(_) | Res::Refused(_)), "{r:?}");
        }
        assert_eq!(counts(&e, view), (3, 1), "line + bracket + length text; plan unchanged");
        assert_eq!(e.undo_len(), steps + 1);
        let all = e.model.annos(view).unwrap();
        match &all[1].shape {
            AnnoShape::Line { pts, .. } => {
                assert_eq!(pts[1], (0.0, 1.5));
                assert_eq!(pts[2], (4.0, 1.5));
            }
            s => panic!("{s:?}"),
        }
        e.undo();
        assert_eq!(counts(&e, view), (1, 1));
        assert_eq!(e.undo_len(), steps);
    }

    #[test]
    fn restarted_tool_starts_without_old_points() {
        let (mut e, _, _) = setup();
        let mut lane = Lane::default();
        lane.start_dim(DimMode::Linear, &mut e);
        lane.free.kind = Some(Fk::Dim);
        lane.free.pts = vec![v2(1.0, 1.0)];
        lane.start_dim(DimMode::Linear, &mut e);
        assert!(lane.free.pts.is_empty());
        assert_eq!(lane.free.kind, None);
        // another lane never sees these points
        lane.free.pts = vec![v2(2.0, 2.0)];
        assert!(Lane::default().free.pts.is_empty());
    }

    #[test]
    fn dim_refuses_the_plan_and_coincident_points_without_a_step() {
        let (mut e, view, _) = setup();
        let steps = e.undo_len();
        let mut pts = vec![v2(1.0, 1.0)];
        assert!(matches!(dim_click(&mut e, &mut pts, v2(1.0, 1.0)), Res::Refused(_)));
        crate::annot_text::sync_view(&mut e, false, None);
        let mut pts = vec![];
        for p in [v2(0.0, 0.0), v2(4.0, 0.0), v2(2.0, 1.0)] {
            assert!(matches!(dim_click(&mut e, &mut pts, p), Res::Refused(_)));
        }
        assert_eq!(e.undo_len(), steps);
        assert_eq!(e.model.annos(view).unwrap().len(), 1);
        assert_eq!(e.model.annos(AnnoView::Plan(e.level)).unwrap().len(), 1);
    }

    #[test]
    fn distribute_copies_selected_view_items_one_undo() {
        let (mut e, view, did) = setup();
        e.selection = vec![Sel::Anno(did)];
        let steps = e.undo_len();
        let mut pts = vec![];
        assert_eq!(dist_click(&mut e, &mut pts, v2(0.0, 0.0), 4), Res::Nothing);
        assert_eq!(dist_click(&mut e, &mut pts, v2(0.0, 9.0), 4), Res::Done);
        assert_eq!(counts(&e, view), (4, 1), "the original and three copies; plan unchanged");
        assert_eq!(e.undo_len(), steps + 1);
        e.undo();
        assert_eq!(counts(&e, view), (1, 1));
        // nothing selected: refused, no step
        e.selection.clear();
        let mut pts = vec![];
        assert!(matches!(dist_click(&mut e, &mut pts, v2(0.0, 0.0), 4), Res::Refused(_)));
        assert_eq!(e.undo_len(), steps);
    }

    #[test]
    fn number_and_tag_add_one_view_item_per_click() {
        let (mut e, view, _) = setup();
        let steps = e.undo_len();
        let mut next = "7".to_string();
        assert_eq!(mark_click(&mut e, v2(1.0, 3.0), 0.1, &mut next, false), Res::Done);
        assert_eq!(next, "8");
        assert_eq!(mark_click(&mut e, v2(2.0, 3.0), 0.1, &mut next, false), Res::Done);
        assert_eq!(e.undo_len(), steps + 2);
        assert_eq!(counts(&e, view), (3, 1));
        // tag: needs an item under the click
        assert!(matches!(
            mark_click(&mut e, v2(2.0, 8.0), 0.1, &mut next, true),
            Res::Refused(_)
        ));
        assert_eq!(next, "9", "a refused click does not use a number");
        assert_eq!(mark_click(&mut e, v2(2.0, 0.0), 0.1, &mut next, true), Res::Done);
        assert_eq!(e.undo_len(), steps + 3);
        let last = e.model.annos(view).unwrap().pop().unwrap();
        match last.shape {
            AnnoShape::Text(t) => assert_eq!(t.leaders[0].to, (2.0, 0.0)),
            s => panic!("{s:?}"),
        }
        e.undo();
        assert_eq!(counts(&e, view), (3, 1));
    }

    #[test]
    fn snaps_come_from_view_items_only() {
        let (e, _, _) = setup();
        let vs = crate::annot_text::view_vertices(&e);
        assert!(vs.contains(&v2(4.0, 0.0)));
        assert_eq!(snap_pick(&vs, v2(3.95, 0.05), 0.2), v2(4.0, 0.0));
        assert_eq!(snap_pick(&vs, v2(2.0, 2.0), 0.2), v2(2.0, 2.0));
    }

    #[test]
    fn preview_geometry_is_in_view_coordinates() {
        let (e, _, did) = setup();
        let mut t = ModTool::new(Op::Move, &[Sel::Anno(did)]);
        t.free = true;
        t.pts = vec![v2(0.0, 0.0)];
        t.cursor = v2(0.0, 2.0);
        let g = ghost_outlines(&e, &t);
        assert_eq!(g.len(), 1);
        assert!(g[0].iter().all(|q| (q.y - 2.0).abs() < 1e-9), "{g:?}");
        assert!(g[0].iter().any(|q| (q.x - 4.0).abs() < 1e-9));
        // the guides of the dimension tool
        let a = v2(0.0, 0.0);
        let b = v2(4.0, 0.0);
        assert_eq!(preview_lines(Fk::Dim, &[a], b), vec![vec![a, b]]);
        let br = &preview_lines(Fk::Dim, &[a, b], v2(1.0, 2.0))[0];
        assert_eq!(br.len(), 4);
        assert!((br[1].y - 2.0).abs() < 1e-9 && (br[2].y - 2.0).abs() < 1e-9);
        assert!(preview_lines(Fk::Dim, &[a, a], v2(1.0, 2.0)).is_empty());
        let offs = dist_offsets(a, v2(0.0, 9.0), 4);
        assert_eq!(offs.len(), 3);
        assert!((offs[2].y - 9.0).abs() < 1e-9);
    }
}
