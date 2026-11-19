//! Tags as elements (W9): TG — tag by click (door/window → wall → room under the cursor),
//! head dragging, Delete removes the tag under the cursor; TA — "Tag all" with category
//! selection and a counter of new tags; NU — click numbering (number +1 per click, each
//! click is one undo step); AD — dimension chains along the selected walls (no selection — whole level).
//! A tag shows "?" for an empty number. Plan tag drawing — [`draw_tags`].

use crate::editor::{Editor, OpFn, Sel};
use crate::ui_kit::Tokens;
use ddc_geom::{v2, V2};
use ddc_model::{Tag, TagCat, TagHost};
use egui::{self, Color32, Pos2, Stroke};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnMode {
    Tag,
    Number,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumCat {
    Door,
    Window,
    Room,
}

pub enum AnnOut {
    Nothing,
    Done(String),
    Exit,
    Error(String),
}

#[derive(Clone, Debug)]
pub struct AnnTool {
    pub mode: AnnMode,
    pub leader: bool,
    pub cat: NumCat,
    /// Next numbering value.
    pub next: String,
    pub cursor: V2,
    /// Tag drag: id, head offset at start, grab point.
    pub drag: Option<(i64, (f64, f64), V2)>,
}

/// Tag host under the point: opening, then wall, then room.
pub fn host_at(ed: &Editor, p: V2, r: f64) -> Option<TagHost> {
    if let Some(o) = super::distribute::opening_at(ed, p, r) {
        return Some(TagHost::Opening(o));
    }
    let wall = ed
        .walls
        .iter()
        .map(|w| {
            let t = ed
                .types
                .iter()
                .find(|t| t.id == w.wall_type)
                .map_or(0.2, |t| t.thickness());
            let d = (crate::snap::closest_on_segment(p, w.a, w.b).0 - p).len();
            (d - t / 2.0, w.id)
        })
        .filter(|x| x.0 <= r)
        .min_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((_, id)) = wall {
        return Some(TagHost::Wall(id));
    }
    ed.room_pick(p).map(TagHost::Room)
}

/// Used numbers: opening tags or room numbers (host group of `h`).
fn taken_marks(ed: &Editor, h: TagHost) -> std::collections::BTreeSet<String> {
    let sql = match h {
        TagHost::Room(_) => "SELECT number FROM room",
        _ => "SELECT CAST(value AS TEXT) FROM prop WHERE kind='opening' AND key='mark'",
    };
    let Ok(mut st) = ed.model.sql().prepare(sql) else {
        return Default::default();
    };
    let taken = st
        .query_map([], |r| r.get::<_, Option<String>>(0))
        .map(|rows| {
            rows.flatten()
                .flatten()
                .map(|s| s.trim().to_string())
                .collect()
        })
        .unwrap_or_default();
    taken
}

/// Inline edit of a tag head (double click): the number/mark of a door, window or room, plus
/// the room name. Enter writes it as one operation, Esc drops it.
#[derive(Clone, Debug, PartialEq)]
pub struct TagEdit {
    pub tag: i64,
    pub host: TagHost,
    pub mark: String,
    pub name: String,
    pub orig: (String, String),
    /// World position of the head (the field is placed over it).
    pub head: V2,
    pub fresh: bool,
}

impl TagEdit {
    /// Opens the edit for tag `id`; `Err` is the reason a tag is read-only or locked.
    pub fn begin(ed: &Editor, id: i64) -> Result<TagEdit, String> {
        let t = ed
            .tags
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| crate::tr("tags.gone", &[]))?;
        let (kind, hid) = match t.spec.host {
            TagHost::Opening(h) => ("opening", h),
            TagHost::Room(h) => ("room", h),
            TagHost::Wall(_) => return Err(crate::tr("tags.readonly_wall", &[])),
            TagHost::Comp(_) => return Err(crate::tr("tags.readonly_comp", &[])),
        };
        if ed.model.is_pinned(kind, hid).unwrap_or(false) {
            return Err(crate::tr("tags.locked", &[]));
        }
        let mark = ed.model.mark(t.spec.host).map_err(|e| e.to_string())?;
        let name = if kind == "room" {
            t.text.name.clone()
        } else {
            String::new()
        };
        Ok(TagEdit {
            tag: id,
            host: t.spec.host,
            orig: (mark.clone(), name.clone()),
            mark,
            name,
            head: v2(t.head.0, t.head.1),
            fresh: true,
        })
    }

    pub fn is_room(&self) -> bool {
        matches!(self.host, TagHost::Room(_))
    }

    /// "D12 also on 2 doors": the typed mark is already used; it is still written.
    pub fn warning(&self, ed: &Editor) -> Option<String> {
        let n = ed.model.other_marks(self.host, &self.mark).ok()?;
        if n == 0 {
            return None;
        }
        let key = match self.host {
            TagHost::Room(_) => "tags.dup_room",
            TagHost::Opening(id) => match ed.openings.iter().find(|o| o.id == id).map(|o| o.kind) {
                Some(crate::opening_tool::Kind::Window) => "tags.dup_window",
                _ => "tags.dup_door",
            },
            _ => return None,
        };
        Some(crate::tr(key, &[("mark", self.mark.trim()), ("n", &n.to_string())]))
    }

    pub fn changed(&self) -> bool {
        (self.mark.trim(), self.name.trim()) != (self.orig.0.trim(), self.orig.1.trim())
    }

    /// Writes the edit as one undo step; an unchanged value writes nothing.
    pub fn commit(&self, ed: &mut Editor) -> AnnOut {
        if !self.changed() {
            return AnnOut::Nothing;
        }
        let (host, mark) = (self.host, self.mark.clone());
        let name = self.is_room().then(|| self.name.clone());
        let f = OpFn(Arc::new(move |m| m.set_tag_text(host, &mark, name.as_deref())));
        match ed.run_op(f) {
            Ok(()) => AnnOut::Done(crate::tr("tags.done", &[])),
            Err(e) if e.to_string().contains("element is pinned") => {
                AnnOut::Error(crate::tr("tags.locked", &[]))
            }
            Err(e) => AnnOut::Error(crate::tr("tags.failed", &[("e", &e.to_string())])),
        }
    }
}

/// Tag labels: lines (first is number/mark, "?" when empty).
pub fn tag_lines(t: &Tag) -> Vec<String> {
    let mark = if t.text.mark.trim().is_empty() {
        "?".to_string()
    } else {
        t.text.mark.clone()
    };
    match t.cat {
        TagCat::Room => {
            let mut v = vec![format!("{} {}", mark, t.text.name).trim().to_string()];
            if let Some(a) = t.text.area {
                v.push(crate::unit_text::area_ui(a));
            }
            v
        }
        _ => vec![mark],
    }
}

/// Screen rectangle of the tag head.
fn tag_rect(p: &egui::Painter, t: &Tag, to_s: &dyn Fn(V2) -> Pos2, px: f32) -> egui::Rect {
    let font = egui::FontId::proportional(px);
    let w = tag_lines(t)
        .iter()
        .map(|s| {
            p.layout_no_wrap(s.clone(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
        .fold(0.0f32, f32::max);
    let h = px * 1.25 * tag_lines(t).len() as f32;
    egui::Rect::from_center_size(
        to_s(v2(t.head.0, t.head.1)),
        egui::vec2(w + px * 0.9, h + px * 0.5),
    )
}

/// Tag font size: 0.22 m of paper at the current scale, 9…16 px.
fn tag_px(to_s: &dyn Fn(V2) -> Pos2) -> f32 {
    let s = (to_s(v2(1.0, 0.0)) - to_s(v2(0.0, 0.0))).length();
    // never below the caption size: at fit zoom a smaller tag is unreadable
    (0.22 * s).clamp(ddc_ui::theme::type_size::CAPTION, 16.0)
}

/// Tag under a plan point: head extent in metres (as in the model layout) plus the pick margin.
pub fn tag_at(ed: &Editor, p: V2, r: f64) -> Option<Tag> {
    ed.tags
        .iter()
        .find(|t| {
            let (w, h) = ddc_model::tag_size(t.cat, &t.text);
            (p.x - t.head.0).abs() <= w / 2.0 + r && (p.y - t.head.1).abs() <= h / 2.0 + r
        })
        .cloned()
}

/// Tool preview: the host under the cursor and what the click will affect.
pub fn draw_ann(
    p: &egui::Painter,
    ed: &Editor,
    a: &AnnTool,
    to_s: &dyn Fn(V2) -> Pos2,
    col: Color32,
    t: &Tokens,
) {
    if let Some((id, off, grab)) = a.drag {
        if let Some(tag) = ed.tags.iter().find(|x| x.id == id) {
            let d = a.cursor - grab;
            let head = v2(tag.anchor.0 + off.0 + d.x, tag.anchor.1 + off.1 + d.y);
            p.line_segment(
                [to_s(v2(tag.anchor.0, tag.anchor.1)), to_s(head)],
                Stroke::new(1.0, col),
            );
            p.circle_stroke(to_s(head), 5.0, Stroke::new(1.5, col));
        }
        return;
    }
    if a.target(ed, a.cursor, 0.15).is_none() {
        return;
    }
    let s = match a.mode {
        AnnMode::Tag => "+".to_string(),
        AnnMode::Number => a.next.clone(),
    };
    let at = to_s(a.cursor) + egui::vec2(14.0, -14.0);
    let font = egui::FontId::proportional(ddc_ui::theme::type_size::LABEL);
    let g = p.layout_no_wrap(s, font, col);
    let r = egui::Rect::from_min_size(at, g.size()).expand(3.0);
    p.rect_filled(r, ddc_ui::theme::radius::SM, t.paper);
    p.rect_stroke(r, 3.0, Stroke::new(1.0, col), egui::StrokeKind::Inside);
    p.galley(at, g, col);
}

/// Level tags: frame by category, leader to the host.
pub fn draw_tags(
    p: &egui::Painter,
    ed: &Editor,
    to_s: &dyn Fn(V2) -> Pos2,
    t: &Tokens,
    hot: Option<i64>,
) {
    draw_tag_list(p, &ed.tags, to_s, t, hot);
}

/// Tags of any level the same way (a read-only plan pane draws another level's).
pub fn draw_tag_list(
    p: &egui::Painter,
    tags: &[ddc_model::Tag],
    to_s: &dyn Fn(V2) -> Pos2,
    t: &Tokens,
    hot: Option<i64>,
) {
    let px = tag_px(to_s);
    let font = egui::FontId::proportional(px);
    for tag in tags {
        let r = tag_rect(p, tag, to_s, px);
        let col = if hot == Some(tag.id) {
            t.ov().hover
        } else {
            t.text
        };
        let anchor = to_s(v2(tag.anchor.0, tag.anchor.1));
        if tag.spec.leader && !r.expand(2.0).contains(anchor) {
            // leader from the nearest frame point to the host, dot on the host
            let from = egui::pos2(
                anchor.x.clamp(r.left(), r.right()),
                anchor.y.clamp(r.top(), r.bottom()),
            );
            p.line_segment([from, anchor], Stroke::new(1.0, col));
            p.circle_filled(anchor, 2.2, col);
        }
        let st = Stroke::new(1.0, col);
        match tag.cat {
            TagCat::Door => {
                p.rect_filled(r, r.height() / 2.0, t.paper);
                p.rect_stroke(r, r.height() / 2.0, st, egui::StrokeKind::Inside);
            }
            TagCat::Window => {
                let c = r.center();
                let (hw, hh) = (r.width() / 2.0 + px * 0.2, r.height() / 2.0);
                let hex = vec![
                    egui::pos2(c.x - hw, c.y),
                    egui::pos2(c.x - hw + hh * 0.6, c.y - hh),
                    egui::pos2(c.x + hw - hh * 0.6, c.y - hh),
                    egui::pos2(c.x + hw, c.y),
                    egui::pos2(c.x + hw - hh * 0.6, c.y + hh),
                    egui::pos2(c.x - hw + hh * 0.6, c.y + hh),
                ];
                p.add(egui::Shape::convex_polygon(hex, t.paper, st));
            }
            TagCat::Room => {
                p.rect_filled(r, ddc_ui::theme::radius::SM, t.paper.gamma_multiply(0.85));
                if hot == Some(tag.id) {
                    p.rect_stroke(r, ddc_ui::theme::radius::SM, st, egui::StrokeKind::Inside);
                }
                if tag_lines(tag).len() > 1 {
                    p.hline(
                        r.x_range().shrink(px * 0.3),
                        r.center().y,
                        Stroke::new(0.75, t.text_dim),
                    );
                }
            }
            TagCat::Wall | TagCat::Comp => {
                p.rect_filled(r, 0.0, t.paper);
                p.rect_stroke(r, 0.0, st, egui::StrokeKind::Inside);
            }
        }
        let lines = tag_lines(tag);
        let lh = px * 1.25;
        let y0 = r.center().y - lh * lines.len() as f32 / 2.0;
        for (i, s) in lines.iter().enumerate() {
            // hover is shown by a frame: the hover colour in the light theme is below the text contrast threshold
            let c = if i == 0 { t.text } else { t.text_dim };
            let g = p.layout_no_wrap(s.clone(), font.clone(), c);
            let at = egui::pos2(
                r.center().x - g.size().x / 2.0,
                y0 + lh * i as f32 + (lh - g.size().y) / 2.0,
            );
            p.galley(at, g, c);
        }
    }
}

impl AnnTool {
    pub fn new(mode: AnnMode) -> Self {
        Self {
            mode,
            leader: false,
            cat: NumCat::Door,
            next: "1".into(),
            cursor: v2(0.0, 0.0),
            drag: None,
        }
    }

    pub fn hint_tool(&self) -> &'static str {
        match self.mode {
            AnnMode::Tag => "tag",
            AnnMode::Number => "number",
        }
    }

    fn fits(&self, ed: &Editor, h: TagHost) -> bool {
        let kind = |id: i64| ed.openings.iter().find(|o| o.id == id).map(|o| o.kind);
        use crate::opening_tool::Kind as Op;
        match (self.cat, h) {
            (NumCat::Room, TagHost::Room(_)) => true,
            (NumCat::Door, TagHost::Opening(id)) => kind(id) == Some(Op::Door),
            (NumCat::Window, TagHost::Opening(id)) => kind(id) == Some(Op::Window),
            _ => false,
        }
    }

    /// Host the click will affect (highlight).
    pub fn target(&self, ed: &Editor, p: V2, r: f64) -> Option<TagHost> {
        let h = match self.mode {
            AnnMode::Number if self.cat == NumCat::Room => ed.room_pick(p).map(TagHost::Room),
            _ => host_at(ed, p, r),
        }?;
        (self.mode == AnnMode::Tag || self.fits(ed, h)).then_some(h)
    }

    pub fn click(&mut self, ed: &mut Editor, p: V2, r: f64) -> AnnOut {
        let Some(h) = self.target(ed, p, r) else {
            return AnnOut::Nothing;
        };
        match self.mode {
            AnnMode::Tag => {
                let tagged = ed.tags.iter().any(|t| t.spec.host == h);
                if tagged {
                    return AnnOut::Error(crate::tr("sketch_edit.tag_dup", &[]));
                }
                let (lv, leader) = (ed.level, self.leader);
                let f = OpFn(Arc::new(move |m| m.tag_element(lv, h, leader).map(|_| ())));
                match ed.run_op(f) {
                    Ok(()) => AnnOut::Done(crate::tr("sketch_edit.tag_ok", &[])),
                    Err(e) => AnnOut::Error(crate::tr("sketch_edit.tag_fail", &[("e", &e.to_string())])),
                }
            }
            AnnMode::Number => {
                let s = self.next.trim().to_string();
                if s.is_empty() {
                    return AnnOut::Error(crate::tr("sketch_edit.num_need", &[]));
                }
                let v = s.clone();
                let f = OpFn(Arc::new(move |m| m.set_mark(h, &v)));
                match ed.run_op(f) {
                    Ok(()) => {
                        // used numbers of the same group (openings / rooms) are skipped
                        let taken = taken_marks(ed, h);
                        let mut next = ddc_model::next_mark(&s);
                        for _ in 0..10_000 {
                            if !taken.contains(&next) {
                                break;
                            }
                            next = ddc_model::next_mark(&next);
                        }
                        self.next = next;
                        AnnOut::Done(crate::tr(
                            "sketch_edit.num_ok",
                            &[("s", &s), ("next", &self.next)],
                        ))
                    }
                    Err(e) => AnnOut::Error(crate::tr("sketch_edit.num_fail", &[("e", &e.to_string())])),
                }
            }
        }
    }

    /// Releasing a dragged tag: the new offset in one operation.
    pub fn drop_drag(&mut self, ed: &mut Editor, at: V2) -> AnnOut {
        let Some((id, off, grab)) = self.drag.take() else {
            return AnnOut::Nothing;
        };
        let d = at - grab;
        if d.len() < 1e-6 {
            return AnnOut::Nothing;
        }
        let o = (off.0 + d.x, off.1 + d.y);
        match ed.run_op(OpFn(Arc::new(move |m| m.move_tag(id, o)))) {
            Ok(()) => AnnOut::Nothing,
            Err(e) => AnnOut::Error(format!("{e}")),
        }
    }

    pub fn delete(&mut self, ed: &mut Editor, id: i64) -> AnnOut {
        match ed.run_op(OpFn(Arc::new(move |m| m.delete_tag(id)))) {
            Ok(()) => AnnOut::Done(crate::tr("sketch_edit.tag_deleted", &[])),
            Err(e) => AnnOut::Error(format!("{e}")),
        }
    }

    pub fn escape(&mut self) -> AnnOut {
        if self.drag.take().is_some() {
            AnnOut::Nothing
        } else {
            AnnOut::Exit
        }
    }
}

/// "Tag all" dialog: categories, leader, number of new tags.
#[derive(Clone, Debug)]
pub struct TagAllDlg {
    pub cats: Vec<(TagCat, bool)>,
    pub leader: bool,
    /// Count of untagged elements per category and the (revision, level) it was computed for.
    pub counts: Vec<usize>,
    pub counted: Option<(u64, i64)>,
}

impl Default for TagAllDlg {
    fn default() -> Self {
        Self {
            cats: vec![
                (TagCat::Door, true),
                (TagCat::Window, true),
                (TagCat::Room, true),
                (TagCat::Wall, false),
            ],
            leader: false,
            counts: vec![],
            counted: None,
        }
    }
}

pub fn cat_key(c: TagCat) -> &'static str {
    match c {
        TagCat::Door => "tg.cat.door",
        TagCat::Window => "tg.cat.window",
        TagCat::Room => "tg.cat.room",
        TagCat::Wall => "tg.cat.wall",
        TagCat::Comp => "tg.cat.comp",
    }
}

impl TagAllDlg {
    /// Recount untagged elements only after a model edit or level change (not every frame).
    pub fn refresh(&mut self, ed: &Editor) {
        let key = (ed.revision, ed.level);
        if self.counted != Some(key) {
            self.counts = self
                .cats
                .iter()
                .map(|(c, _)| ed.model.untagged(ed.level, &[*c]).map_or(0, |v| v.len()))
                .collect();
            self.counted = Some(key);
        }
    }

    pub fn chosen(&self) -> Vec<TagCat> {
        self.cats.iter().filter(|c| c.1).map(|c| c.0).collect()
    }

    pub fn commit(&self, ed: &mut Editor) -> AnnOut {
        let (lv, cats, leader) = (ed.level, self.chosen(), self.leader);
        let n = ed.model.untagged(lv, &cats).map_or(0, |v| v.len());
        if n == 0 {
            return AnnOut::Done(crate::tr("sketch_edit.tag_all_done", &[]));
        }
        let f = OpFn(Arc::new(move |m| m.tag_all(lv, &cats, leader).map(|_| ())));
        match ed.run_op(f) {
            Ok(()) => AnnOut::Done(crate::tr("sketch_edit.tag_all_ok", &[("n", &n.to_string())])),
            Err(e) => AnnOut::Error(crate::tr("sketch_edit.tag_all_fail", &[("e", &e.to_string())])),
        }
    }
}

/// AD: selected walls — chains along them; no selection — auto-dimension the level.
pub fn dim_auto(ed: &mut Editor) -> AnnOut {
    let walls: Vec<i64> = ed
        .selection
        .iter()
        .filter_map(|s| match s {
            Sel::Wall(w) => Some(*w),
            _ => None,
        })
        .collect();
    if !walls.is_empty()
        && ed
            .model
            .chain_skipped(&walls)
            .is_ok_and(|n| n == walls.len())
    {
        return AnnOut::Error(crate::annot_text::cat().t("annot_text.dim_no_straight"));
    }
    let lv = ed.level;
    let f = if walls.is_empty() {
        OpFn(Arc::new(move |m| m.auto_dimension(lv).map(|_| ())))
    } else {
        OpFn(Arc::new(move |m| {
            m.auto_dimension_chain(lv, &walls).map(|_| ())
        }))
    };
    let before = ed.model.dims(lv).map_or(0, |d| d.len());
    match ed.run_op(f) {
        Ok(()) => {
            let after = ed.model.dims(lv).map_or(0, |d| d.len());
            AnnOut::Done(crate::tr(
                "sketch_edit.dims_ok",
                &[("after", &after.to_string()), ("before", &before.to_string())],
            ))
        }
        Err(e) => AnnOut::Error(crate::tr("sketch_edit.dims_fail", &[("e", &e.to_string())])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modify_tools::tests::walls;

    #[test]
    fn tag_text_stays_readable_at_fit_zoom() {
        for scale in [2.0_f32, 10.0, 30.0, 200.0] {
            let to_s = move |p: V2| egui::pos2(p.x as f32 * scale, -p.y as f32 * scale);
            let px = tag_px(&to_s);
            assert!(px >= ddc_ui::theme::type_size::CAPTION && px <= 16.0, "{scale}: {px}");
        }
    }

    fn room(ed: &mut Editor) -> (i64, i64) {
        walls(
            ed,
            &[
                ((0.0, 0.0), (6.0, 0.0)),
                ((6.0, 0.0), (6.0, 4.0)),
                ((6.0, 4.0), (0.0, 4.0)),
                ((0.0, 4.0), (0.0, 0.0)),
            ],
        );
        let s = ed.walls[0].id;
        let d = ed
            .model
            .add_opening(
                s,
                &ddc_model::OpeningSpec {
                    along: 1.0,
                    width: 0.9,
                    sill: 0.0,
                    height: 2.1,
                    kind: "door".into(),
                },
            )
            .unwrap();
        ed.reload().unwrap();
        (s, d)
    }

    #[test]
    fn tag_click_follows_door_move_and_undo() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (_, d) = room(&mut ed);
        let mut t = AnnTool::new(AnnMode::Tag);
        assert!(matches!(
            t.click(&mut ed, v2(1.45, 0.02), 0.1),
            AnnOut::Done(_)
        ));
        assert!(matches!(
            t.click(&mut ed, v2(1.45, 0.02), 0.1),
            AnnOut::Error(_)
        ));
        let tag = ed.model.tags(ed.level).unwrap()[0].clone();
        assert_eq!(tag.spec.host, TagHost::Opening(d));
        assert_eq!(tag_lines(&tag), vec!["?".to_string()]);
        // moving the door — the tag follows, the head offset is preserved
        let f = OpFn(Arc::new(move |m| {
            m.move_elements(&[ddc_model::Target::Opening(d)], (2.0, 0.0))
        }));
        ed.run_op(f).unwrap();
        let moved = ed.model.tags(ed.level).unwrap()[0].clone();
        assert!((moved.head.0 - tag.head.0 - 2.0).abs() < 1e-9);
        ed.undo();
        ed.undo();
        assert!(ed.model.tags(ed.level).unwrap().is_empty());
    }

    #[test]
    fn numbering_clicks_increment_each_one_undo() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (s, _) = room(&mut ed);
        let d2 = ed
            .model
            .add_opening(
                s,
                &ddc_model::OpeningSpec {
                    along: 4.0,
                    width: 0.9,
                    sill: 0.0,
                    height: 2.1,
                    kind: "door".into(),
                },
            )
            .unwrap();
        ed.reload().unwrap();
        let mut t = AnnTool::new(AnnMode::Number);
        t.next = "D1".into();
        assert!(matches!(
            t.click(&mut ed, v2(1.45, 0.0), 0.1),
            AnnOut::Done(_)
        ));
        assert!(matches!(
            t.click(&mut ed, v2(4.45, 0.0), 0.1),
            AnnOut::Done(_)
        ));
        assert_eq!(t.next, "D3");
        assert_eq!(ed.model.mark(TagHost::Opening(d2)).unwrap(), "D2");
        // a wall under the cursor is not numbered
        assert!(matches!(
            t.click(&mut ed, v2(3.0, 4.0), 0.1),
            AnnOut::Nothing
        ));
        ed.undo();
        assert_eq!(ed.model.mark(TagHost::Opening(d2)).unwrap(), "");
    }

    #[test]
    fn tag_all_dialog_counts_and_dim_auto_selected_wall() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (s, _) = room(&mut ed);
        let dlg = TagAllDlg::default();
        assert!(matches!(dlg.commit(&mut ed), AnnOut::Done(_)));
        assert_eq!(ed.model.tags(ed.level).unwrap().len(), 1);
        ed.selection = vec![Sel::Wall(s)];
        assert!(matches!(dim_auto(&mut ed), AnnOut::Done(_)));
        // openings 1.0 | 0.9 | 4.1 and extent 6
        assert_eq!(ed.model.dims(ed.level).unwrap().len(), 4);
        ed.undo();
        assert!(ed.model.dims(ed.level).unwrap().is_empty());
    }
    fn tag_of(ed: &Editor, h: TagHost) -> i64 {
        ed.tags.iter().find(|t| t.spec.host == h).unwrap().id
    }

    fn lane_edit(ed: &mut Editor, h: TagHost) -> crate::modify_tools::Lane {
        let id = match ed.tags.iter().find(|t| t.spec.host == h) {
            Some(t) => t.id,
            None => {
                let lv = ed.level;
                ed.model.tag_element(lv, h, false).unwrap();
                ed.reload().unwrap();
                tag_of(ed, h)
            }
        };
        let mut lane = crate::modify_tools::Lane::default();
        lane.hot_tag = Some(id);
        assert!(lane.begin_tag_edit(ed));
        lane
    }

    #[test]
    fn double_click_tag_edits_mark_one_undo() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (_, d) = room(&mut ed);
        ed.model.set_mark(TagHost::Opening(d), "D1").unwrap();
        let mut lane = lane_edit(&mut ed, TagHost::Opening(d));
        // the field shows the value the tag shows
        assert_eq!(lane.tag_edit.as_ref().unwrap().mark, "D1");
        lane.tag_edit.as_mut().unwrap().mark = " D7 ".into();
        let n = ed.undo_len();
        lane.finish_tag_edit(&mut ed, true);
        assert!(lane.tag_edit.is_none());
        assert_eq!(ed.model.mark(TagHost::Opening(d)).unwrap(), "D7");
        assert_eq!(tag_lines(&ed.tags[0]), vec!["D7".to_string()]);
        assert_eq!(ed.undo_len(), n + 1);
        ed.undo();
        assert_eq!(ed.model.mark(TagHost::Opening(d)).unwrap(), "D1");
        // empty clears the mark: the tag shows the placeholder again
        let mut lane = lane_edit(&mut ed, TagHost::Opening(d));
        lane.tag_edit.as_mut().unwrap().mark = "  ".into();
        lane.finish_tag_edit(&mut ed, true);
        assert_eq!(ed.model.mark(TagHost::Opening(d)).unwrap(), "");
        assert_eq!(tag_lines(&ed.tags[0]), vec!["?".to_string()]);
    }

    #[test]
    fn esc_cancels_tag_edit() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (_, d) = room(&mut ed);
        ed.model.set_mark(TagHost::Opening(d), "D1").unwrap();
        let mut lane = lane_edit(&mut ed, TagHost::Opening(d));
        lane.tag_edit.as_mut().unwrap().mark = "ZZ".into();
        let n = ed.undo_len();
        lane.finish_tag_edit(&mut ed, false);
        assert!(lane.tag_edit.is_none());
        assert_eq!(ed.model.mark(TagHost::Opening(d)).unwrap(), "D1");
        assert_eq!(ed.undo_len(), n);
        // an unchanged Enter writes nothing either
        let mut lane = lane_edit(&mut ed, TagHost::Opening(d));
        lane.finish_tag_edit(&mut ed, true);
        assert_eq!(ed.undo_len(), n);
    }

    #[test]
    fn duplicate_mark_warns_and_writes() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (s, d) = room(&mut ed);
        let mk = |along: f64| ddc_model::OpeningSpec {
            along,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        };
        let d2 = ed.model.add_opening(s, &mk(3.0)).unwrap();
        let d3 = ed.model.add_opening(s, &mk(4.5)).unwrap();
        ed.model.set_mark(TagHost::Opening(d2), "D12").unwrap();
        ed.model.set_mark(TagHost::Opening(d3), "D12").unwrap();
        ed.reload().unwrap();
        let mut lane = lane_edit(&mut ed, TagHost::Opening(d));
        let e = lane.tag_edit.as_mut().unwrap();
        e.mark = "D12".into();
        let w = e.warning(&ed).expect("warning");
        assert!(w.contains("D12") && w.contains('2'), "{w}");
        // a free mark has no warning
        e.mark = "D99".into();
        assert!(e.warning(&ed).is_none());
        e.mark = "D12".into();
        lane.finish_tag_edit(&mut ed, true);
        assert_eq!(ed.model.mark(TagHost::Opening(d)).unwrap(), "D12");
    }

    #[test]
    fn room_tag_edits_number_and_name() {
        let mut ed = Editor::new_in_memory().unwrap();
        room(&mut ed);
        let rid = ed
            .model
            .add_room(&ddc_model::RoomSpec {
                level: ed.level,
                name: "Old".into(),
                number: "1".into(),
                point: (3.0, 2.0),
                height: 2.7,
            })
            .unwrap();
        ed.reload().unwrap();
        let mut lane = lane_edit(&mut ed, TagHost::Room(rid));
        let e = lane.tag_edit.as_mut().unwrap();
        assert!(e.is_room());
        assert_eq!((e.mark.as_str(), e.name.as_str()), ("1", "Old"));
        e.mark = "205".into();
        e.name = "Hall".into();
        let n = ed.undo_len();
        lane.finish_tag_edit(&mut ed, true);
        assert_eq!(ed.undo_len(), n + 1);
        let t = ed.tags.iter().find(|t| t.spec.host == TagHost::Room(rid)).unwrap();
        assert_eq!(tag_lines(t)[0], "205 Hall");
        ed.undo();
        let t = ed.tags.iter().find(|t| t.spec.host == TagHost::Room(rid)).unwrap();
        assert_eq!(tag_lines(t)[0], "1 Old");
    }

    #[test]
    fn wall_tag_is_read_only_and_pinned_host_is_locked() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (s, d) = room(&mut ed);
        let lv = ed.level;
        ed.model.tag_element(lv, TagHost::Wall(s), false).unwrap();
        ed.model.tag_element(lv, TagHost::Opening(d), false).unwrap();
        ed.reload().unwrap();
        let wall_tag = tag_of(&ed, TagHost::Wall(s));
        assert!(TagEdit::begin(&ed, wall_tag).is_err());
        let door_tag = tag_of(&ed, TagHost::Opening(d));
        assert!(TagEdit::begin(&ed, door_tag).is_ok());
        ed.model.pin(&[("opening", d)]).unwrap();
        assert!(TagEdit::begin(&ed, door_tag).is_err());
        // a lock set while the field is open still refuses the write and keeps the value
        ed.model.unpin(&[("opening", d)]).unwrap();
        let mut e = TagEdit::begin(&ed, door_tag).unwrap();
        e.mark = "X1".into();
        ed.model.pin(&[("opening", d)]).unwrap();
        assert!(matches!(e.commit(&mut ed), AnnOut::Error(_)));
        assert_eq!(ed.model.mark(TagHost::Opening(d)).unwrap(), "");
    }
}
