//! Type picker: closed field like a dropdown, popup with search, a muted detail per row,
//! keyboard navigation and an "Edit type…" footer entry.
use crate::theme::{control, space, ControlSize, Role};
use crate::Theme;
use egui::{Align2, Key, Modifiers, Response, Sense, Stroke, Ui, Vec2};

/// Search field appears above this many items.
pub const SEARCH_MIN_ITEMS: usize = 8;
/// Rows visible before the list scrolls.
const VISIBLE_ROWS: f32 = 9.0;

#[derive(Clone, Debug, PartialEq)]
pub struct PickItem {
    pub id: i64,
    pub name: String,
    /// Muted right-aligned hint, e.g. `300 mm · 3 layers`; also searched.
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Id(i64),
    EditType,
}

/// Strings of the picker; the caller fills them from the catalog.
#[derive(Clone, Debug, Default)]
pub struct PickTexts {
    pub search: String,
    pub empty: String,
    /// Footer entry; empty hides it.
    pub edit: String,
    /// Closed text when nothing is current and the selection is mixed.
    pub mixed: String,
}

/// Case, whitespace and Russian yo/ye folded form used for matching.
pub fn fold(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .map(|c| if c == '\u{451}' { '\u{435}' } else { c })
        .collect()
}

/// Indices of items whose name or detail contains the query; empty query keeps all.
pub fn filter(items: &[PickItem], query: &str) -> Vec<usize> {
    let q = fold(query);
    items
        .iter()
        .enumerate()
        .filter(|(_, it)| q.is_empty() || fold(&it.name).contains(&q) || fold(&it.detail).contains(&q))
        .map(|(i, _)| i)
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAct {
    Up,
    Down,
    Enter,
    Esc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// New highlighted position.
    Hi(usize),
    /// Chosen position.
    Choose(usize),
    Close,
}

/// Highlight moved by `delta` rows, clamped at both ends (`len` rows, may be 0).
pub fn key_step(cur: usize, len: usize, delta: i32) -> usize {
    if len == 0 {
        return 0;
    }
    let next = cur as i64 + i64::from(delta);
    next.clamp(0, len as i64 - 1) as usize
}

/// Reaction to a key over `len` rows with the highlight at `hi`.
pub fn step(hi: usize, len: usize, act: KeyAct) -> Step {
    match act {
        KeyAct::Up => Step::Hi(key_step(hi, len, -1)),
        KeyAct::Down => Step::Hi(key_step(hi, len, 1)),
        KeyAct::Enter if len > 0 => Step::Choose(hi.min(len - 1)),
        KeyAct::Enter => Step::Hi(0),
        KeyAct::Esc => Step::Close,
    }
}

#[derive(Clone, Default)]
struct State {
    query: String,
    hi: usize,
    fresh: bool,
}

pub struct TypePick<'a> {
    salt: egui::Id,
    items: &'a [PickItem],
    cur: Option<i64>,
    texts: PickTexts,
    width: f32,
    hover: Option<String>,
}

impl<'a> TypePick<'a> {
    pub fn new(salt: impl std::hash::Hash, items: &'a [PickItem], cur: Option<i64>) -> Self {
        Self {
            salt: egui::Id::new(salt),
            items,
            cur,
            texts: PickTexts::default(),
            width: control::DROPDOWN_W,
            hover: None,
        }
    }
    pub fn texts(mut self, t: PickTexts) -> Self {
        self.texts = t;
        self
    }
    pub fn width(mut self, w: f32) -> Self {
        self.width = w;
        self
    }
    /// Tooltip of the closed field; an elided name shows the full name instead.
    pub fn hover(mut self, text: impl Into<String>) -> Self {
        self.hover = Some(text.into());
        self
    }

    pub fn show(self, ui: &mut Ui, th: &Theme) -> Option<Pick> {
        let cur_item = self.cur.and_then(|id| self.items.iter().find(|i| i.id == id));
        let (closed, muted) = match cur_item {
            Some(i) => (i.name.as_str(), false),
            None if self.cur.is_none() && !self.texts.mixed.is_empty() => {
                (self.texts.mixed.as_str(), true)
            }
            None => ("\u{2014}", true),
        };
        let h = th.control_h(ControlSize::Medium);
        let w = self.width.min(ui.available_width()).max(control::FIELD_MIN_W);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
        // stable id: the popup stays open when rows above the field change
        let mut resp = ui.interact(rect, ui.make_persistent_id(self.salt), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, closed));
        let sem = th.sem();
        let mut elided = false;
        if ui.is_rect_visible(rect) {
            let edge = if resp.has_focus() {
                sem.accent
            } else if resp.hovered() {
                sem.border_strong
            } else {
                sem.border_control
            };
            let p = ui.painter();
            p.rect_filled(rect, th.radius("sm"), sem.bg_input);
            p.rect_stroke(rect, th.radius("sm"), Stroke::new(1.0, edge), egui::StrokeKind::Inside);
            let text_w = rect.width() - 2.0 * space::SM - space::LG;
            let color = if muted { sem.text_secondary } else { sem.text_primary };
            let g = crate::widgets::text::elide(ui, closed, th.font(Role::Body), color, text_w);
            elided = g.elided;
            let pos = egui::pos2(rect.left() + space::SM, rect.center().y - g.size().y / 2.0);
            ui.painter().galley(pos, g, color);
            let c = egui::pos2(rect.right() - space::SM - 3.0, rect.center().y);
            ui.painter().add(egui::Shape::line(
                vec![c + Vec2::new(-3.5, -1.5), c + Vec2::new(0.0, 2.0), c + Vec2::new(3.5, -1.5)],
                Stroke::new(1.25, sem.text_secondary),
            ));
        }
        if elided {
            resp = resp.on_hover_text(closed);
        } else if let Some(t) = &self.hover {
            resp = resp.on_hover_text(t.as_str());
        }
        let sid = resp.id.with("pick.state");
        if resp.clicked() {
            ui.data_mut(|d| d.insert_temp(sid, State {
                    fresh: true,
                    // open on the current row, so Enter keeps it
                    hi: self.cur.and_then(|id| self.items.iter().position(|i| i.id == id)).unwrap_or(0),
                    ..State::default()
                }));
        }
        let mut out = None;
        let min_w = rect.width().max(control::DROPDOWN_W * 1.75);
        egui::Popup::menu(&resp)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_min_width(min_w);
                out = self.popup(ui, th, sid);
                if out.is_some() {
                    ui.close();
                }
            });
        out
    }

    fn popup(&self, ui: &mut Ui, th: &Theme, sid: egui::Id) -> Option<Pick> {
        let mut st: State = ui.data(|d| d.get_temp(sid)).unwrap_or_default();
        let has_edit = !self.texts.edit.is_empty();
        let sem = th.sem();
        let mut chosen: Option<usize> = None;
        let mut close = false;

        // keys first, so the search field never sees Up/Down/Enter
        let hits = filter(self.items, &st.query);
        let rows = hits.len() + usize::from(has_edit);
        let mut moved = false;
        for (key, act) in [
            (Key::ArrowUp, KeyAct::Up),
            (Key::ArrowDown, KeyAct::Down),
            (Key::Enter, KeyAct::Enter),
            (Key::Escape, KeyAct::Esc),
        ] {
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, key)) {
                match step(st.hi, rows, act) {
                    Step::Hi(h) => {
                        moved |= h != st.hi;
                        st.hi = h;
                    }
                    Step::Choose(p) => chosen = Some(p),
                    Step::Close => close = true,
                }
            }
        }

        if self.items.len() > SEARCH_MIN_ITEMS {
            let te = egui::TextEdit::singleline(&mut st.query)
                .hint_text(self.texts.search.as_str())
                .desired_width(f32::INFINITY)
                .font(th.font(Role::Body));
            let r = ui.add(te);
            if st.fresh {
                r.request_focus();
            }
            if r.changed() {
                st.hi = 0;
            }
            ui.add_space(space::XS);
        }
        st.fresh = false;

        let hits = filter(self.items, &st.query);
        let rows = hits.len() + usize::from(has_edit);
        st.hi = st.hi.min(rows.saturating_sub(1));
        let row_h = th.control_h(ControlSize::Medium);
        let mut clicked: Option<usize> = None;
        if hits.is_empty() {
            ui.add_space(space::SM);
            ui.label(
                egui::RichText::new(&self.texts.empty)
                    .font(th.font(Role::Caption))
                    .color(sem.text_secondary),
            );
            ui.add_space(space::SM);
        } else {
            egui::ScrollArea::vertical()
                .max_height(row_h * VISIBLE_ROWS)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (pos, &i) in hits.iter().enumerate() {
                        let it = &self.items[i];
                        let on = Some(it.id) == self.cur;
                        let r = self.row(ui, th, row_h, it, on, pos == st.hi);
                        if moved && pos == st.hi {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            clicked = Some(pos);
                        }
                    }
                });
        }
        if has_edit {
            crate::widgets::menu_divider(ui, th);
            let pos = hits.len();
            let r = footer_row(ui, th, row_h, &self.texts.edit, pos == st.hi);
            if r.clicked() {
                clicked = Some(pos);
            }
        }

        ui.data_mut(|d| d.insert_temp(sid, st));
        let sel = chosen.or(clicked);
        if close && sel.is_none() {
            ui.close();
        }
        match sel? {
            p if p < hits.len() => {
                let id = self.items[hits[p]].id;
                (Some(id) != self.cur).then_some(Pick::Id(id)).or_else(|| {
                    ui.close();
                    None
                })
            }
            _ => Some(Pick::EditType),
        }
    }

    fn row(
        &self,
        ui: &mut Ui,
        th: &Theme,
        h: f32,
        it: &PickItem,
        current: bool,
        hi: bool,
    ) -> Response {
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &it.name));
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let sem = th.sem();
        let p = ui.painter();
        if current {
            p.rect_filled(rect, th.radius("sm"), sem.accent_subtle);
        } else if hi || resp.hovered() {
            p.rect_filled(rect, th.radius("sm"), th.hover_fill());
        }
        if hi {
            p.rect_stroke(rect, th.radius("sm"), Stroke::new(1.0, sem.accent), egui::StrokeKind::Inside);
        }
        let mid = rect.center().y;
        if current {
            let c = egui::pos2(rect.left() + space::SM + 4.0, mid);
            p.add(egui::Shape::line(
                vec![c + Vec2::new(-3.5, 0.0), c + Vec2::new(-1.0, 3.0), c + Vec2::new(3.5, -3.0)],
                Stroke::new(1.5, sem.accent),
            ));
        }
        let left = rect.left() + space::SM + 14.0 + space::XS;
        let mut right = rect.right() - space::SM;
        if !it.detail.is_empty() {
            let g = ui.painter().layout_no_wrap(
                it.detail.clone(),
                th.font(Role::Caption),
                sem.text_secondary,
            );
            // the detail keeps its width; the name gives way
            let dw = g.size().x.min((right - left) * 0.5);
            let g = crate::widgets::text::elide(ui, &it.detail, th.font(Role::Caption), sem.text_secondary, dw);
            let size = g.size();
            ui.painter()
                .galley(egui::pos2(right - size.x, mid - size.y / 2.0), g, sem.text_secondary);
            right -= size.x + space::MD;
        }
        let g = crate::widgets::text::elide(ui, &it.name, th.font(Role::Body), sem.text_primary, right - left);
        let cut = g.elided;
        ui.painter()
            .galley(egui::pos2(left, mid - g.size().y / 2.0), g, sem.text_primary);
        if cut {
            resp.on_hover_text(&it.name)
        } else {
            resp
        }
    }
}

fn footer_row(ui: &mut Ui, th: &Theme, h: f32, label: &str, hi: bool) -> Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let sem = th.sem();
    if hi || resp.hovered() {
        ui.painter().rect_filled(rect, th.radius("sm"), th.hover_fill());
    }
    if hi {
        ui.painter()
            .rect_stroke(rect, th.radius("sm"), Stroke::new(1.0, sem.accent), egui::StrokeKind::Inside);
    }
    ui.painter().text(
        egui::pos2(rect.left() + space::SM + 14.0 + space::XS, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        th.font(Role::Body),
        sem.accent,
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<PickItem> {
        let mk = |id, name: &str, detail: &str| PickItem {
            id,
            name: name.into(),
            detail: detail.into(),
        };
        vec![
            mk(1, "Basic Wall 200", "200 mm \u{b7} 1 layer"),
            mk(2, "Exterior Brick", "300 mm \u{b7} 3 layers"),
            mk(3, "\u{41f}\u{435}\u{440}\u{435}\u{433}\u{43e}\u{440}\u{43e}\u{434}\u{43a}\u{430} \u{401}\u{436}", "120 \u{43c}\u{43c}"),
        ]
    }

    #[test]
    fn filter_substring_case_and_empty_query() {
        let it = items();
        assert_eq!(filter(&it, ""), vec![0, 1, 2]);
        assert_eq!(filter(&it, "   "), vec![0, 1, 2]);
        assert_eq!(filter(&it, "BRICK"), vec![1]);
        assert_eq!(filter(&it, "wall 2"), vec![0]);
        assert!(filter(&it, "zzz").is_empty());
    }

    #[test]
    fn filter_by_thickness_and_layers_in_detail() {
        let it = items();
        assert_eq!(filter(&it, "300"), vec![1]);
        assert_eq!(filter(&it, "300 mm"), vec![1]);
        assert_eq!(filter(&it, "3 layers"), vec![1]);
    }

    #[test]
    fn filter_folds_cyrillic_case_and_yo() {
        let it = items();
        // lower-case query with "e" finds the name written with a capital yo
        assert_eq!(filter(&it, "\u{43f}\u{435}\u{440}\u{435}\u{433}\u{43e}\u{440}\u{43e}\u{434}\u{43a}\u{430} \u{435}\u{436}"), vec![2]);
        assert_eq!(filter(&it, "\u{41f}\u{415}\u{420}\u{415}"), vec![2]);
        assert_eq!(filter(&it, "120 \u{43c}\u{43c}"), vec![2]);
    }

    #[test]
    fn key_step_clamps_at_both_ends() {
        assert_eq!(key_step(0, 3, -1), 0);
        assert_eq!(key_step(2, 3, 1), 2);
        assert_eq!(key_step(1, 3, 1), 2);
        assert_eq!(key_step(1, 3, -1), 0);
        assert_eq!(key_step(5, 0, 1), 0);
    }

    #[test]
    fn keys_up_down_enter_esc() {
        assert_eq!(step(0, 4, KeyAct::Down), Step::Hi(1));
        assert_eq!(step(3, 4, KeyAct::Down), Step::Hi(3));
        assert_eq!(step(1, 4, KeyAct::Up), Step::Hi(0));
        assert_eq!(step(2, 4, KeyAct::Enter), Step::Choose(2));
        assert_eq!(step(0, 0, KeyAct::Enter), Step::Hi(0));
        assert_eq!(step(2, 4, KeyAct::Esc), Step::Close);
    }

    #[test]
    fn closed_picker_height_is_control_h() {
        let th = Theme::new(crate::Mode::Light, crate::Density::Compact);
        let ctx = egui::Context::default();
        let it = items();
        let mut h = 0.0;
        let _ = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                // the panel's min_rect already spans the panel: measure a scope of its own
                let shown = ui.scope(|ui| {
                    let _ = TypePick::new("t", &it, Some(2)).show(ui, &th);
                });
                h = shown.response.rect.height();
            });
        });
        assert!(h >= th.control_h(ControlSize::Medium) - 0.5);
    }
}
