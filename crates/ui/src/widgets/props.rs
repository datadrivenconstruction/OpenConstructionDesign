//! Property panel rows: «label — value — unit» on columns shared by the whole panel, numbers
//! right-aligned, units muted, long text elided with a tooltip.
use super::text::paint_in;
use crate::theme::{layout, space, Role};
use crate::Theme;
use egui::{Align, Layout, Rect, Response, Sense, Ui, UiBuilder, Vec2};

/// Column widths of a row: (key, value, unit). The key takes what the fixed columns leave.
pub fn prop_columns(avail: f32) -> (f32, f32, f32) {
    let key = (avail - layout::PROP_VALUE - layout::PROP_UNIT - 2.0 * layout::PROP_GAP)
        .clamp(layout::PROP_KEY_MIN, layout::PROP_KEY_MAX);
    (key, layout::PROP_VALUE, layout::PROP_UNIT)
}

struct Row {
    resp: Response,
    key: Rect,
    value: Rect,
    unit: Rect,
    /// Value and unit columns together, for text and widgets.
    span: Rect,
}

fn row(ui: &mut Ui, th: &Theme) -> Row {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, super::form::row_h(th)), Sense::hover());
    let (kw, vw, uw) = prop_columns(w);
    let at = |x: f32, width: f32| {
        Rect::from_min_size(
            egui::pos2(x, rect.top()),
            Vec2::new(width.max(0.0), rect.height()),
        )
    };
    let unit = at(rect.right() - uw, uw);
    let value = at(unit.left() - layout::PROP_GAP - vw, vw);
    let key_right = (rect.left() + kw).min(value.left() - layout::PROP_GAP);
    Row {
        resp,
        key: at(rect.left(), key_right - rect.left()),
        value,
        unit,
        span: Rect::from_min_max(value.min, unit.max),
    }
}

fn paint_key(ui: &Ui, th: &Theme, r: &Row, key: &str) -> bool {
    let font = th.font(Role::Body);
    paint_in(ui, r.key, key, font, th.sem().text_secondary, Align::Min)
}

/// Read-only number: right-aligned value, muted unit in its own column.
pub fn prop_value(ui: &mut Ui, th: &Theme, key: &str, value: &str, unit: &str) -> Response {
    let r = row(ui, th);
    let s = th.sem();
    let cut = paint_key(ui, th, &r, key);
    let val_cut = paint_in(
        ui,
        r.value,
        value,
        th.font(Role::Numeric),
        s.text_primary,
        Align::Max,
    );
    paint_in(
        ui,
        r.unit,
        unit,
        th.font(Role::Body),
        s.text_secondary,
        Align::Min,
    );
    let resp = r.resp;
    match (cut, val_cut) {
        (false, false) => resp,
        (true, false) => resp.on_hover_text(key),
        _ => resp.on_hover_text(format!("{key}: {value} {unit}")),
    }
}

/// Read-only text value: right-aligned across the value and unit columns, elided if long.
pub fn prop_text(ui: &mut Ui, th: &Theme, key: &str, value: &str) -> Response {
    let r = row(ui, th);
    let s = th.sem();
    let cut = paint_key(ui, th, &r, key);
    let val_cut = paint_in(
        ui,
        r.span,
        value,
        th.font(Role::Body),
        s.text_primary,
        Align::Max,
    );
    let resp = r.resp;
    if cut || val_cut {
        resp.on_hover_text(format!("{key}: {value}"))
    } else {
        resp
    }
}

/// Row of a multi-selection whose members disagree: the placeholder ink, right-aligned, no italics.
pub fn prop_mixed(ui: &mut Ui, th: &Theme, key: &str, label: &str) -> Response {
    let r = row(ui, th);
    let cut = paint_key(ui, th, &r, key);
    paint_in(ui, r.span, label, th.font(Role::Body), th.sem().text_placeholder, Align::Max);
    if cut {
        r.resp.on_hover_text(key)
    } else {
        r.resp
    }
}

/// Editable row: the closure draws the widget (field, dropdown, toggle) over value and unit.
pub fn prop_row<R>(ui: &mut Ui, th: &Theme, key: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    prop_row_marked(ui, th, key, false, add)
}

/// Marker for a value the AI assistant changed: 2×16 bar on the row's left edge.
pub fn ai_marker_rect(row: Rect) -> Rect {
    Rect::from_center_size(
        egui::pos2(row.left() + 1.0, row.center().y),
        Vec2::new(2.0, 16.0),
    )
}

/// As `prop_row`; `ai_changed` adds the `ai.changed` marker at the left edge.
pub fn prop_row_marked<R>(
    ui: &mut Ui,
    th: &Theme,
    key: &str,
    ai_changed: bool,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    let r = row(ui, th);
    if ai_changed {
        ui.painter().rect_filled(
            ai_marker_rect(r.resp.rect),
            egui::CornerRadius::same(1),
            th.c("ai.changed"),
        );
    }
    let cut = paint_key(ui, th, &r, key);
    if cut {
        r.resp.clone().on_hover_text(key);
    }
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(r.span)
            .layout(Layout::right_to_left(Align::Center)),
    );
    add(&mut child)
}

/// Collapsible section: state persists per title. Returns what `add` returned when open.
pub fn section<R>(
    ui: &mut Ui,
    th: &Theme,
    title: &str,
    add: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    let id = ui.id().with(("ddc.section", title));
    let mut open = ui.data_mut(|d| *d.get_persisted_mut_or(id, true));
    let head = super::section_header(ui, th, title, &mut open);
    let head_rect = head.rect;
    if head.clicked() {
        ui.data_mut(|d| d.insert_persisted(id, open));
    }
    if !open {
        return None;
    }
    let out = add(ui);
    ui.add_space(space::SM);
    // sticky: while the header is scrolled above the view, pin a copy to the top of it until
    // the body's end pushes it out
    let clip = ui.clip_rect();
    let y = sticky_y(head_rect.top(), clip.top(), ui.cursor().top(), head_rect.height());
    if y > head_rect.top() {
        let pin = Rect::from_min_size(egui::pos2(head_rect.left(), y), head_rect.size());
        ui.painter().rect_filled(pin, 0.0, th.sem().bg_panel);
        let resp = ui.interact(pin, id.with("sticky"), Sense::click());
        let mut shown = open;
        let mut sub = ui.new_child(UiBuilder::new().max_rect(pin));
        sub.set_clip_rect(clip);
        let h2 = super::section_header(&mut sub, th, title, &mut shown);
        if h2.clicked() || resp.clicked() {
            ui.data_mut(|d| d.insert_persisted(id, false));
        }
    }
    Some(out)
}

/// Top of a section header pinned in a scroll view: its own place, or the view's top when
/// scrolled past, never lower than where the body (ending at `body_end`) leaves room for it.
pub fn sticky_y(head_top: f32, view_top: f32, body_end: f32, head_h: f32) -> f32 {
    view_top.max(head_top).min((body_end - head_h).max(head_top))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_and_unit_columns_are_the_same_in_every_group() {
        for w in [260.0, 320.0, 480.0] {
            let (k, v, u) = prop_columns(w);
            assert_eq!((v, u), (layout::PROP_VALUE, layout::PROP_UNIT));
            assert!((layout::PROP_KEY_MIN..=layout::PROP_KEY_MAX).contains(&k));
        }
        assert_eq!(prop_columns(320.0), prop_columns(320.0));
    }

    #[test]
    fn columns_are_on_the_grid() {
        for v in [layout::PROP_VALUE, layout::PROP_UNIT, layout::PROP_GAP, layout::PROP_KEY_MIN] {
            assert_eq!(v % 4.0, 0.0);
        }
    }
}

#[cfg(test)]
mod sticky_tests {
    use super::*;

    #[test]
    fn header_pins_to_the_view_top_and_leaves_with_its_body() {
        assert_eq!(sticky_y(100.0, 0.0, 400.0, 28.0), 100.0, "not scrolled: stays");
        assert_eq!(sticky_y(100.0, 150.0, 400.0, 28.0), 150.0, "scrolled past: pinned");
        assert_eq!(sticky_y(100.0, 390.0, 400.0, 28.0), 372.0, "pushed out by the body end");
    }

    #[test]
    fn ai_marker_is_2_by_16_on_the_left_edge() {
        let row = Rect::from_min_size(egui::pos2(10.0, 40.0), Vec2::new(300.0, 28.0));
        let m = ai_marker_rect(row);
        assert_eq!((m.width(), m.height()), (2.0, 16.0));
        assert_eq!(m.left(), row.left());
        assert_eq!(m.center().y, row.center().y);
    }
}
