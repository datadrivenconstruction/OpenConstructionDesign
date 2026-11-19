// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Virtualized table: sortable headers, row states, numbers right-aligned with muted units,
//! long cells elided with a tooltip. The body is one Tab stop; arrows report a move and the
//! caller owns the selection.
use super::text::paint_in;
use crate::theme::{space, stroke, Role, Sem};
use crate::Theme;
use egui::{Align, Color32, EventFilter, Id, Key, Rect, Sense, Stroke, Ui, Vec2};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColWidth {
    Fixed(f32),
    /// Share of what the fixed columns leave.
    Fill(f32),
}

#[derive(Debug, Clone, Copy)]
pub struct Column<'a> {
    pub title: &'a str,
    pub width: ColWidth,
    pub align: Align,
    pub sortable: bool,
}

impl<'a> Column<'a> {
    /// Text column taking the free width.
    pub fn text(title: &'a str) -> Self {
        Self {
            title,
            width: ColWidth::Fill(1.0),
            align: Align::Min,
            sortable: true,
        }
    }
    /// Number column: right-aligned.
    pub fn number(title: &'a str, width: f32) -> Self {
        Self {
            title,
            width: ColWidth::Fixed(width),
            align: Align::Max,
            sortable: true,
        }
    }
    pub fn fixed(mut self, w: f32) -> Self {
        self.width = ColWidth::Fixed(w);
        self
    }
    pub fn fill(mut self, weight: f32) -> Self {
        self.width = ColWidth::Fill(weight);
        self
    }
    pub fn sortable(mut self, on: bool) -> Self {
        self.sortable = on;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub col: usize,
    pub desc: bool,
}

/// Header click cycle: ascending → descending → unsorted; another column starts over.
pub fn next_sort(cur: Option<Sort>, col: usize) -> Option<Sort> {
    match cur {
        Some(Sort {
            col: c,
            desc: false,
        }) if c == col => Some(Sort { col, desc: true }),
        Some(Sort { col: c, desc: true }) if c == col => None,
        _ => Some(Sort { col, desc: false }),
    }
}

/// Hit width of the column-resize grip.
pub const COL_GRIP: f32 = 8.0;
/// Narrowest a column can be dragged to.
pub const COL_MIN_W: f32 = 32.0;

/// Column width after a drag of `dx`, never below `COL_MIN_W`, whole pixels.
pub fn resized_width(cur: f32, dx: f32) -> f32 {
    (cur + dx).max(COL_MIN_W).round()
}

/// Header height: 32 in the 28 px row density, 28 in compact (row + 4), always on the 4 grid.
pub fn header_h(row_h: f32) -> f32 {
    row_h + space::XS
}

/// Column widths for the available width; fill columns never collapse below one padding.
pub fn column_widths(cols: &[Column<'_>], avail: f32) -> Vec<f32> {
    let fixed: f32 = cols
        .iter()
        .map(|c| match c.width {
            ColWidth::Fixed(w) => w,
            ColWidth::Fill(_) => 0.0,
        })
        .sum();
    let weight: f32 = cols
        .iter()
        .map(|c| match c.width {
            ColWidth::Fill(k) => k.max(0.0),
            ColWidth::Fixed(_) => 0.0,
        })
        .sum();
    let free = (avail - fixed).max(0.0);
    cols.iter()
        .map(|c| match c.width {
            ColWidth::Fixed(w) => w,
            ColWidth::Fill(k) if weight > 0.0 => (free * k.max(0.0) / weight).max(space::LG),
            ColWidth::Fill(_) => space::LG,
        })
        .collect()
}

/// Scroll offset that brings a row into view, moving as little as possible.
pub fn reveal_offset(offset: f32, row: usize, row_h: f32, view_h: f32) -> f32 {
    let top = row as f32 * row_h;
    if top < offset {
        top
    } else if top + row_h > offset + view_h {
        top + row_h - view_h
    } else {
        offset
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowState {
    #[default]
    Normal,
    Selected,
    Disabled,
    Warning,
    Error,
}

/// Colours of a row: (fill, text, muted text). Pairs are covered by the contrast test.
pub fn row_colors(s: &Sem, state: RowState, hovered: bool) -> (Color32, Color32, Color32) {
    let hover = if hovered {
        s.bg_hover
    } else {
        Color32::TRANSPARENT
    };
    match state {
        RowState::Normal => (hover, s.text_primary, s.text_secondary),
        RowState::Selected => {
            let fill = if hovered { s.bg_selected_hover } else { s.bg_selected };
            (fill, s.accent, s.accent)
        }
        // a state of the row, the Ui stays enabled: nothing else fades it
        RowState::Disabled => (Color32::TRANSPARENT, s.text_disabled, s.text_disabled),
        RowState::Warning => (s.warning_subtle, s.text_primary, s.text_secondary),
        RowState::Error => (s.danger_subtle, s.text_primary, s.text_secondary),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Cell {
    pub text: String,
    pub unit: String,
    pub muted: bool,
    /// Shown on hover when the text itself is not cut.
    pub tip: String,
    /// Text colour override (a status pill); None keeps the row ink.
    pub tone: Option<Color32>,
    /// Alignment override for this cell; None follows the column.
    pub align: Option<Align>,
    /// Icon name drawn before the text.
    pub icon: Option<&'static str>,
    /// Icon colour; None, or a selected row, uses the text colour.
    pub icon_tone: Option<Color32>,
    /// Keep the icon's width free even without an icon, so labels of a column line up.
    pub lead: bool,
    /// Columns after this one that the cell also covers; they are not asked for.
    pub span: usize,
    /// Accent bar on the row's left edge, drawn by the first column's cell.
    pub edge: Option<Color32>,
}

impl Cell {
    /// Icon before the text; `None` still reserves its place.
    pub fn with_icon(mut self, name: Option<&'static str>, tone: Option<Color32>) -> Self {
        self.icon = name;
        self.icon_tone = tone;
        self.lead = true;
        self
    }
    pub fn with_span(mut self, extra_columns: usize) -> Self {
        self.span = extra_columns;
        self
    }
    pub fn with_edge(mut self, c: Option<Color32>) -> Self {
        self.edge = c;
        self
    }
    pub fn with_align(mut self, a: Align) -> Self {
        self.align = Some(a);
        self
    }
    pub fn with_tone(mut self, c: Color32) -> Self {
        self.tone = Some(c);
        self
    }
    pub fn with_tip(mut self, tip: impl Into<String>) -> Self {
        self.tip = tip.into();
        self
    }
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            text: s.into(),
            ..Default::default()
        }
    }
    pub fn muted(s: impl Into<String>) -> Self {
        Self {
            text: s.into(),
            muted: true,
            ..Default::default()
        }
    }
    /// Number with its unit, drawn muted after the value.
    pub fn num(s: impl Into<String>, unit: impl Into<String>) -> Self {
        Self {
            text: s.into(),
            unit: unit.into(),
            muted: false,
            tip: String::new(),
            tone: None,
            align: None,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TableOutput {
    /// The sort state changed: re-sort the rows.
    pub sort_changed: bool,
    pub clicked: Option<usize>,
    pub double_clicked: Option<usize>,
    pub secondary_clicked: Option<usize>,
    /// Arrow, Page or Home/End key: rows to move the selection by.
    pub moved: Option<isize>,
    /// Enter on the focused body: open the selected row.
    pub activated: bool,
    /// Height the body took, rows and the `min_height` room under them.
    pub body_h: f32,
}

pub struct Table<'a> {
    id: Id,
    cols: &'a [Column<'a>],
    rows: usize,
    max_height: f32,
    min_height: f32,
    scroll_to: Option<usize>,
}

impl<'a> Table<'a> {
    pub fn new(id_salt: impl std::hash::Hash, cols: &'a [Column<'a>], rows: usize) -> Self {
        Self {
            id: Id::new(id_salt),
            cols,
            rows,
            max_height: f32::INFINITY,
            min_height: 0.0,
            scroll_to: None,
        }
    }
    pub fn max_height(mut self, h: f32) -> Self {
        self.max_height = h;
        self
    }
    /// The body takes at least this height (never more than `max_height`): a short list
    /// keeps the room under it, so what follows the table stays put.
    pub fn min_height(mut self, h: f32) -> Self {
        self.min_height = h;
        self
    }
    /// Bring a row into view this pass (after keyboard navigation).
    pub fn scroll_to(mut self, row: Option<usize>) -> Self {
        self.scroll_to = row;
        self
    }

    pub fn show(
        self,
        ui: &mut Ui,
        th: &Theme,
        sort: &mut Option<Sort>,
        state: impl Fn(usize) -> RowState,
        mut cell: impl FnMut(usize, usize) -> Cell,
    ) -> TableOutput {
        let s = th.sem();
        let d = th.d();
        let id = ui.id().with(self.id);
        let mut out = TableOutput::default();
        let cols = self.cols;
        let rows = self.rows;
        let row_h = d.row;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let w = ui.available_width();
            let over_id = id.with("colw");
            let eff: Vec<Column<'_>> = cols
                .iter()
                .enumerate()
                .map(|(i, c)| match ui.data_mut(|m| m.get_persisted::<f32>(over_id.with(i))) {
                    Some(w) => c.fixed(w),
                    None => *c,
                })
                .collect();
            let cols = &eff[..];
            let widths = column_widths(cols, w);
            let head_h = header_h(row_h);
            let (head, _) = ui.allocate_exact_size(Vec2::new(w, head_h), Sense::hover());
            ui.painter().rect_filled(head, 0.0, s.bg_panel);
            let mut x = head.left();
            for (i, c) in cols.iter().enumerate() {
                let r = Rect::from_min_size(egui::pos2(x, head.top()), Vec2::new(widths[i], head_h));
                x += widths[i];
                let sense = if c.sortable {
                    Sense::click()
                } else {
                    Sense::hover()
                };
                let resp = ui.interact(r, id.with(("head", i)), sense);
                if c.sortable && resp.clicked() {
                    *sort = next_sort(*sort, i);
                    out.sort_changed = true;
                }
                if resp.hovered() && c.sortable {
                    ui.painter().rect_filled(r, 0.0, s.bg_hover);
                }
                let active = sort.filter(|q| q.col == i);
                let col = if active.is_some() || (resp.hovered() && c.sortable) {
                    s.text_primary
                } else {
                    s.text_secondary
                };
                let inner = r.shrink2(Vec2::new(d.pad_x, 0.0));
                let arrow_w = if active.is_some() { space::LG } else { 0.0 };
                let text_rect = Rect::from_min_max(
                    inner.min,
                    egui::pos2(inner.right() - arrow_w, inner.bottom()),
                );
                let font = th.font(Role::TableHeader);
                let cut = paint_in(ui, text_rect, c.title, font, col, c.align);
                if let Some(q) = active {
                    let name = if q.desc {
                        crate::icons::ctl::SORT_DESC
                    } else {
                        crate::icons::ctl::SORT_ASC
                    };
                    let ir = Rect::from_center_size(
                        egui::pos2(inner.right() - space::SM, inner.center().y),
                        Vec2::splat(space::LG),
                    );
                    crate::diff_kind::paint_icon(ui, name, ir, s.accent);
                }
                if cut {
                    resp.on_hover_text(c.title);
                }
                // 8 px grab zone straddling the column's right edge
                let grip = Rect::from_center_size(
                    egui::pos2(r.right(), r.center().y),
                    Vec2::new(COL_GRIP, head_h),
                );
                let g = ui.interact(grip, id.with(("grip", i)), Sense::drag());
                if g.hovered() || g.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    ui.painter().vline(
                        r.right(),
                        r.y_range(),
                        Stroke::new(stroke::FOCUS, s.accent),
                    );
                }
                if g.dragged() {
                    let w = resized_width(widths[i], g.drag_delta().x);
                    ui.data_mut(|m| m.insert_persisted(over_id.with(i), w));
                }
                if g.double_clicked() {
                    ui.data_mut(|m| m.remove::<f32>(over_id.with(i)));
                }
            }
            ui.painter().hline(
                head.x_range(),
                head.bottom() - stroke::HAIRLINE / 2.0,
                Stroke::new(stroke::HAIRLINE, s.border_strong),
            );

            let body_id = id.with("body");
            let view_h = (rows as f32 * row_h)
                .min(self.max_height)
                .min(ui.available_height().max(row_h));
            let body_rect = Rect::from_min_size(ui.next_widget_position(), Vec2::new(w, view_h));
            // one Tab stop for the whole body, however many rows there are
            let focus = ui.interact(body_rect, body_id, Sense::focusable_noninteractive());
            if focus.has_focus() {
                ui.memory_mut(|m| {
                    m.set_focus_lock_filter(
                        body_id,
                        EventFilter {
                            vertical_arrows: true,
                            ..Default::default()
                        },
                    )
                });
                let page = ((view_h / row_h) as isize).max(1);
                let n = rows as isize;
                ui.input(|i| {
                    if i.key_pressed(Key::ArrowUp) {
                        out.moved = Some(-1);
                    } else if i.key_pressed(Key::ArrowDown) {
                        out.moved = Some(1);
                    } else if i.key_pressed(Key::PageUp) {
                        out.moved = Some(-page);
                    } else if i.key_pressed(Key::PageDown) {
                        out.moved = Some(page);
                    } else if i.key_pressed(Key::Home) {
                        out.moved = Some(-n);
                    } else if i.key_pressed(Key::End) {
                        out.moved = Some(n);
                    }
                    out.activated = i.key_pressed(Key::Enter);
                });
            }
            let mut area = egui::ScrollArea::vertical()
                .id_salt(id.with("scroll"))
                .auto_shrink([false, true])
                .max_height(self.max_height);
            let offset_id = id.with("offset");
            if let Some(i) = self.scroll_to {
                let prev = ui.data(|m| m.get_temp::<f32>(offset_id)).unwrap_or(0.0);
                area = area.vertical_scroll_offset(reveal_offset(prev, i, row_h, view_h));
            }
            let shown = area.show_rows(ui, row_h, rows, |ui, range| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                for row in range {
                    let w = ui.available_width();
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, row_h), Sense::CLICK);
                    let st = state(row);
                    let (fill, fg, muted) = row_colors(s, st, resp.hovered());
                    if fill != Color32::TRANSPARENT {
                        ui.painter().rect_filled(r, 0.0, fill);
                    }
                    if resp.clicked() {
                        out.clicked = Some(row);
                        ui.memory_mut(|m| m.request_focus(body_id));
                    }
                    if resp.double_clicked() {
                        out.double_clicked = Some(row);
                    }
                    if resp.secondary_clicked() {
                        out.secondary_clicked = Some(row);
                    }
                    let mut x = r.left();
                    let pointer = resp.hover_pos();
                    let mut covered = 0usize;
                    for (i, c) in cols.iter().enumerate() {
                        let mut cr = Rect::from_min_size(
                            egui::pos2(x, r.top()),
                            Vec2::new(widths[i], row_h),
                        );
                        x += widths[i];
                        if covered > 0 {
                            covered -= 1;
                            continue;
                        }
                        let cl = cell(row, i);
                        let more = cl.span.min(cols.len() - 1 - i);
                        cr.max.x += widths[i + 1..=i + more].iter().sum::<f32>();
                        covered = more;
                        if let Some(c) = cl.edge {
                            let bar = Rect::from_min_size(
                                r.min,
                                Vec2::new(stroke::CONTROL * 3.0, row_h),
                            );
                            ui.painter().rect_filled(bar, 0.0, c);
                        }
                        let inner = cr.shrink2(Vec2::new(d.pad_x, 0.0));
                        let text_col = cl.tone.unwrap_or(if cl.muted { muted } else { fg });
                        let mut text_rect = inner;
                        if cl.lead {
                            let side = crate::sem::icon_size::SM;
                            if let Some(name) = cl.icon {
                                let ir = Rect::from_min_size(
                                    egui::pos2(inner.left(), inner.center().y - side / 2.0),
                                    Vec2::splat(side),
                                );
                                let ink = match cl.icon_tone {
                                    Some(t) if st != RowState::Selected => t,
                                    _ => text_col,
                                };
                                crate::diff_kind::paint_icon(ui, name, ir, ink);
                            }
                            text_rect.min.x = (inner.left() + side + space::XS).min(inner.right());
                        }
                        if !cl.unit.is_empty() {
                            let g = ui.painter().layout_no_wrap(
                                cl.unit.clone(),
                                th.font(Role::TableCell),
                                muted,
                            );
                            let uw = g.size().x;
                            let at =
                                egui::pos2(inner.right() - uw, inner.center().y - g.size().y / 2.0);
                            ui.painter().galley(at, g, muted);
                            text_rect.max.x = (inner.right() - uw - space::XS).max(inner.left());
                        }
                        let font = th.font(Role::TableCell);
                        let al = cl.align.unwrap_or(c.align);
                        let cut = paint_in(ui, text_rect, &cl.text, font, text_col, al);
                        if pointer.is_some_and(|p| cr.contains(p)) {
                            if cut {
                                resp.clone().on_hover_text(cl.text.clone());
                            } else if !cl.tip.is_empty() {
                                resp.clone().on_hover_text(cl.tip.clone());
                            }
                        }
                    }
                }
            });
            ui.data_mut(|m| m.insert_temp(offset_id, shown.state.offset.y));
            super::scroll_hint::scroll_fade(ui, th, &shown);
            // what the body really took: a scroll area that shrinks to its rows reports the
            // room it was offered, not the room it kept
            let used = (ui.min_rect().bottom() - body_rect.top()).max(0.0);
            let rest = self.min_height.min(self.max_height) - used;
            if rest > 0.0 {
                let _ = ui.allocate_space(Vec2::new(w, rest));
            }
            out.body_h = used + rest.max(0.0);
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{contrast, sem};

    #[test]
    fn header_sits_on_the_grid_above_the_row() {
        for row in [24.0, 28.0] {
            let h = header_h(row);
            assert_eq!(h % 4.0, 0.0);
            assert_eq!(h, row + 4.0);
        }
    }

    #[test]
    fn selected_row_uses_the_selected_roles_and_text_stays_legible() {
        for dark in [false, true] {
            let s = sem(dark);
            for hov in [false, true] {
                let (fill, ink, muted) = row_colors(&s, RowState::Selected, hov);
                assert_eq!(fill, if hov { s.bg_selected_hover } else { s.bg_selected });
                assert_eq!(ink, s.accent);
                assert!(contrast(ink, fill) >= 4.5, "dark={dark} hover={hov}");
                assert_eq!(muted, ink);
            }
        }
    }

    #[test]
    fn number_columns_are_right_aligned() {
        assert_eq!(Column::number("Area", 80.0).align, Align::Max);
        assert_eq!(Column::text("Name").align, Align::Min);
    }
}

#[cfg(test)]
mod resize_tests {
    use super::*;

    #[test]
    fn resize_is_clamped_and_whole_pixel() {
        assert_eq!(resized_width(100.0, 20.4), 120.0);
        assert_eq!(resized_width(40.0, -100.0), COL_MIN_W);
        assert_eq!(COL_GRIP, 8.0);
    }

    #[test]
    fn a_dragged_column_becomes_fixed_and_the_rest_refill() {
        let cols = [Column::text("a"), Column::text("b").fixed(100.0)];
        let w = column_widths(&cols, 300.0);
        assert_eq!(w, vec![200.0, 100.0]);
        let cols = [Column::text("a").fixed(120.0), Column::text("b").fixed(100.0)];
        assert_eq!(column_widths(&cols, 300.0), vec![120.0, 100.0]);
    }
}
