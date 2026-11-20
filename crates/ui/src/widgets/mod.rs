// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Shell widgets. Each takes `&Theme`/`&Catalog` explicitly; no globals.
pub mod bars;
pub mod button;
pub mod choice;
pub mod field;
pub mod form;
pub mod formula;
pub mod layout_cards;
pub mod menu;
pub mod palette;
pub mod props;
pub mod ribbon;
pub mod ribbon_layout;
pub mod scroll_hint;
pub mod shell_kit;
pub mod surface;
pub mod table;
pub mod tabs;
pub mod text;
pub mod toasts;
pub mod type_pick;

#[cfg(test)]
mod kit_tests;
#[cfg(test)]
mod polish_tests;

pub use button::{Button, Variant};
pub use choice::{
    checkbox, checkbox_error, checkbox_mixed, radio, radio_group, slider, toggle, toggle_bare,
};
pub use field::{
    dropdown, dropdown_h, dropdown_marked, dropdown_shown, NumberField, NumberFormat, TextField,
};
pub use menu::{menu_divider, menu_item, menu_item_icon, menu_sub, MenuTone};
pub use props::{prop_row, prop_text, prop_value, section};
pub use surface::{
    badge, card, card_button, empty_state, empty_state_compact, error_state, loading_state,
    progress, Tone,
};
pub use table::{Cell, Column, RowState, Sort, Table, TableOutput};
pub use tabs::tab_bar;
pub use text::{elide, elided_label};

use crate::theme::{control, icon_size, layout, space, stroke, Role};
use crate::{icons::IconCache, Theme};
use egui::{Align2, Color32, Key, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

/// Square icon button tinted by state (secondary → primary on hover; filled accent glyph if on).
pub fn icon_button(
    ui: &mut Ui,
    icons: &mut IconCache,
    th: &Theme,
    icon: &str,
    on: bool,
    tip: &str,
) -> egui::Response {
    let d = th.d();
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(d.control + 4.0), Sense::click());
    paint_state(ui, th, rect, &resp, on);
    let ic = Rect::from_center_size(rect.center(), Vec2::splat(d.icon));
    icons.paint(ui, &icon_name(icon, on), th.icon_px(), ic, icon_ink(ui, th, on));
    let (title, keys) = split_tip(tip);
    icon_button_info(ui, &resp, title);
    rich_tooltip(resp, th, title, "", keys)
}

/// An icon button is a named button for assistive technology and the layout checks: its tip
/// title is its name. Without a tip the caller names it.
fn icon_button_info(ui: &Ui, resp: &egui::Response, title: &str) {
    if !title.is_empty() {
        let enabled = ui.is_enabled();
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, title));
    }
}

/// Icon colour of a button in `ui` (ICONS_V4.md roles): accent when on, ink otherwise. A
/// disabled Ui does not fade its painter, so the ink is the disabled tint, applied once.
pub fn icon_ink(ui: &Ui, th: &Theme, on: bool) -> Color32 {
    use crate::icons::{tint, Tint};
    if !ui.is_enabled() {
        return tint(th, Tint::Disabled);
    }
    tint(th, if on { Tint::Active } else { Tint::Rest })
}

/// Quiet destructive icon button: secondary at rest, the glyph turns danger on hover/focus/press.
pub fn icon_button_danger(
    ui: &mut Ui,
    icons: &mut IconCache,
    th: &Theme,
    icon: &str,
    tip: &str,
) -> egui::Response {
    let d = th.d();
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(d.control + 4.0), Sense::click());
    let rest = paint_state(ui, th, rect, &resp, false);
    let hot = resp.hovered() || resp.has_focus() || resp.is_pointer_button_down_on();
    let col = if ui.is_enabled() && hot { th.c("danger") } else { rest };
    let ic = Rect::from_center_size(rect.center(), Vec2::splat(d.icon));
    icons.paint(ui, icon, th.icon_px(), ic, col);
    let (title, keys) = split_tip(tip);
    icon_button_info(ui, &resp, title);
    rich_tooltip(resp, th, title, "", keys)
}

/// Runs `f` with the icon cache shared through the context (for code that holds only a `Ui`).
pub fn with_icons<R>(ctx: &egui::Context, f: impl FnOnce(&mut IconCache) -> R) -> R {
    use std::sync::{Arc, Mutex};
    let id = egui::Id::new("ddc.glyph_icons");
    let cache = ctx.data_mut(|d| {
        d.get_temp_mut_or_insert_with(id, || Arc::new(Mutex::new(IconCache::default())))
            .clone()
    });
    let mut icons = cache.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut icons)
}

/// Icon button for code that holds only a `Ui` (theme and icon textures come from the context).
/// A disabled button still shows `tip`; it never reports a click.
pub fn glyph_button(ui: &mut Ui, icon: &str, enabled: bool, tip: &str) -> egui::Response {
    let th = crate::theme::of(ui.ctx());
    let ctx = ui.ctx().clone();
    ui.add_enabled_ui(enabled, |ui| {
        with_icons(&ctx, |icons| icon_button(ui, icons, &th, icon, false, tip))
    })
    .inner
}

/// Compact close / remove button: the `close` icon on a small-control square with the usual
/// hover and pressed states. `tip` uses the `split_tip` form and may be empty.
pub fn close_button(ui: &mut Ui, tip: &str) -> egui::Response {
    close_button_tinted(ui, tip, None)
}

/// `close_button` on a coloured banner: `tint` is the banner's on-colour text.
pub fn close_button_tinted(ui: &mut Ui, tip: &str, tint: Option<Color32>) -> egui::Response {
    let th = crate::theme::of(ui.ctx());
    let side = th.d().control_sm;
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    let enabled = ui.is_enabled();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, tip));
    if ui.is_rect_visible(rect) {
        let col = paint_state(ui, &th, rect, &resp, false);
        let col = tint.unwrap_or(col);
        let px = icon_size::SM;
        let ic = Rect::from_center_size(rect.center(), Vec2::splat(px));
        let ctx = ui.ctx().clone();
        with_icons(&ctx, |icons| icons.paint(ui, "close", px as u32, ic, col));
    }
    let (title, keys) = split_tip(tip);
    rich_tooltip(resp, &th, title, "", keys)
}

/// `empty_state` with an icon, for code that holds only a `Ui`.
pub fn icon_empty_state(
    ui: &mut Ui,
    icon: &str,
    title: &str,
    hint: &str,
    action: Option<&str>,
) -> bool {
    let th = crate::theme::of(ui.ctx());
    let ctx = ui.ctx().clone();
    with_icons(&ctx, |icons| {
        empty_state(ui, &th, Some((icons, icon)), title, hint, action)
    })
}

/// Active tools use the filled glyph variant.
pub fn icon_name(icon: &str, on: bool) -> String {
    if on && crate::icons::source(&format!("{icon}.fill")).is_some() {
        format!("{icon}.fill")
    } else {
        icon.to_string()
    }
}

/// "Name  ·  Ctrl+K" → ("Name", "Ctrl+K").
pub fn split_tip(tip: &str) -> (&str, &str) {
    tip.split_once("  ·  ").unwrap_or((tip, ""))
}

/// Tooltip: title (semibold), optional description, shortcut keycaps on the right.
pub fn rich_tooltip(
    resp: egui::Response,
    th: &Theme,
    title: &str,
    desc: &str,
    keys: &str,
) -> egui::Response {
    rich_tooltip_reason(resp, th, title, desc, keys, "")
}

/// Tooltip with an optional last line in warning tone: why the control is disabled.
pub fn rich_tooltip_reason(
    resp: egui::Response,
    th: &Theme,
    title: &str,
    desc: &str,
    keys: &str,
    reason: &str,
) -> egui::Response {
    if title.is_empty() {
        return resp;
    }
    // a disabled control shows the same tooltip: it is where the reason is read
    let body = |ui: &mut Ui| {
        ui.set_max_width(layout::TOOLTIP_W);
        ui.horizontal(|ui| {
            ui.label(
                th.rich(Role::TooltipTitle, title),
            );
            if !keys.is_empty() {
                ui.add_space(space::SM);
                for k in keys.split(" / ").take(2) {
                    keycap(ui, th, k);
                }
            }
        });
        if !desc.is_empty() {
            ui.label(
                th.rich(Role::TooltipBody, desc),
            );
        }
        if !reason.is_empty() {
            ui.label(
                egui::RichText::new(reason)
                    .font(th.font(Role::Body))
                    .color(th.c("warning")),
            );
        }
    };
    show_tip(&resp, th, body);
    resp
}

/// Tooltip with the spec 2.14 timing and placement: shown after `TIP_DELAY_S` of hover (at
/// once within `TIP_REPEAT_S` of the previous one), faded in over `TIP_FADE_S`, `TIP_GAP` from
/// the anchor and never over it. Also shown for a disabled anchor.
fn show_tip(resp: &egui::Response, th: &Theme, body: impl FnOnce(&mut Ui)) {
    use crate::ctx_tips::{tip_delay, tip_rect, TIP_FADE_S};
    let ctx = &resp.ctx;
    let now = ctx.input(|i| i.time);
    let start_key = resp.id.with("ddc.tip.start");
    let last_key = egui::Id::new("ddc.tip.last_shown");
    let over = resp.hovered() || (resp.contains_pointer() && !ctx.input(|i| i.pointer.any_down()));
    if !over {
        if ctx.data_mut(|d| d.get_temp::<f64>(start_key)).is_some() {
            ctx.data_mut(|d| d.remove_temp::<f64>(start_key));
            // rearm the fade for the next show
            ctx.animate_bool_with_time(resp.id.with("ddc.tip").with("fade"), false, 0.0);
        }
        return;
    }
    let start: f64 = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(start_key, || now));
    let last: Option<f64> = ctx.data_mut(|d| d.get_temp(last_key));
    let wait = tip_delay(last, start);
    if now - start < wait {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait - (now - start)));
        return;
    }
    ctx.data_mut(|d| d.insert_temp(last_key, now));
    let id = resp.id.with("ddc.tip");
    let size = ctx
        .memory(|m| m.area_rect(id))
        .map_or(egui::vec2(layout::TOOLTIP_W / 2.0, 32.0), |r| r.size());
    let at = tip_rect(resp.rect, size, ctx.screen_rect());
    let t = ctx.animate_bool_with_time(id.with("fade"), true, if th.reduced_motion { 0.0 } else { TIP_FADE_S });
    egui::Area::new(id)
        .order(egui::Order::Tooltip)
        .fixed_pos(at.min)
        .interactable(false)
        .show(ctx, |ui| {
            ui.set_opacity(t);
            egui::Frame::popup(ui.style()).show(ui, body);
        });
}

/// Keyboard shortcut chip: each key in its own rounded cap.
pub fn keycap(ui: &mut Ui, th: &Theme, keys: &str) {
    let font = th.font(Role::Keycap);
    ui.spacing_mut().item_spacing.x = space::XS;
    for k in keys.split('+').filter(|k| !k.is_empty()) {
        let g = ui
            .painter()
            .layout_no_wrap(k.to_string(), font.clone(), th.c("text.secondary"));
        let size = Vec2::new(
            (g.size().x + space::MD).max(control::KEYCAP_H),
            control::KEYCAP_H,
        );
        let (r, _) = ui.allocate_exact_size(size, Sense::hover());
        let p = ui.painter();
        p.rect(
            r,
            th.radius("sm"),
            th.c("bg.app"),
            Stroke::new(stroke::HAIRLINE, th.c("border.subtle")),
            StrokeKind::Inside,
        );
        p.galley(r.center() - g.size() / 2.0, g, th.c("text.secondary"));
    }
}

/// Section header of a panel: caps caption, chevron, hairline; click toggles `open`.
pub fn section_header(ui: &mut Ui, th: &Theme, title: &str, open: &mut bool) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, control::SECTION_H), Sense::click());
    if resp.clicked() {
        *open = !*open;
    }
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, th.radius("sm"), th.hover_fill());
    }
    let slot = Rect::from_center_size(
        rect.left_center() + Vec2::new(space::SM + space::SM, 0.0),
        Vec2::splat(space::LG),
    );
    let name = if *open {
        crate::icons::ctl::CHEVRON_DOWN
    } else {
        crate::icons::ctl::CHEVRON_RIGHT
    };
    crate::diff_kind::paint_icon(ui, name, slot, th.c("text.secondary"));
    ui.painter().text(
        rect.left_center() + Vec2::new(space::SM + space::LG + space::XS, 0.0),
        Align2::LEFT_CENTER,
        title.to_uppercase(),
        th.font(Role::Section),
        th.c("text.secondary"),
    );
    resp
}

/// Segmented control: equal-height pills in a sunken track; returns the new index if changed.
pub fn segmented(ui: &mut Ui, th: &Theme, items: &[&str], sel: usize) -> Option<usize> {
    segmented_enabled(ui, th, items, sel, &[])
}

/// Index the arrow keys move to from `from`, wrapping, skipping disabled pills.
/// `None` when no other pill is enabled.
pub(crate) fn seg_step(enabled: &[bool], from: usize, forward: bool) -> Option<usize> {
    let n = enabled.len();
    (1..n)
        .map(|k| if forward { (from + k) % n } else { (from + n - k) % n })
        .find(|&i| enabled[i])
}

fn seg_edge(enabled: &[bool], last: bool) -> Option<usize> {
    if last {
        enabled.iter().rposition(|&e| e)
    } else {
        enabled.iter().position(|&e| e)
    }
}

/// Accessible description of one pill: a radio button with its label and selected state.
pub(crate) fn seg_info(label: &str, enabled: bool, selected: bool) -> egui::WidgetInfo {
    egui::WidgetInfo::selected(egui::WidgetType::RadioButton, enabled, selected, label)
}

/// Segmented control with per-pill enable flags (`enabled_flags[i] == false` disables pill `i`;
/// a short or empty slice means the rest are enabled). The group is one Tab stop; Left/Right
/// (wrapping), Home and End move focus and selection between enabled pills.
pub fn segmented_enabled(
    ui: &mut Ui,
    th: &Theme,
    items: &[&str],
    sel: usize,
    enabled_flags: &[bool],
) -> Option<usize> {
    let font = th.font(Role::Label);
    let h = th.d().control;
    let group_on = ui.is_enabled();
    let en: Vec<bool> = (0..items.len())
        .map(|i| group_on && enabled_flags.get(i).copied().unwrap_or(true))
        .collect();
    let ids: Vec<egui::Id> = items
        .iter()
        .map(|t| ui.id().with(("seg", *t)))
        .collect();
    let mut out = None;
    // arrows act on the pill that holds focus; consumed before the pills are built
    let focused = ui.ctx().memory(|m| m.focused()).and_then(|f| ids.iter().position(|&i| i == f));
    let mut focus_to = None;
    if let Some(cur) = focused {
        // without the lock egui also moves focus by arrow direction, out of the group
        let lock = egui::EventFilter {
            horizontal_arrows: true,
            ..Default::default()
        };
        ui.memory_mut(|m| m.set_focus_lock_filter(ids[cur], lock));
        let (l, r, home, end) = ui.input_mut(|i| {
            let m = egui::Modifiers::NONE;
            (
                i.consume_key(m, Key::ArrowLeft),
                i.consume_key(m, Key::ArrowRight),
                i.consume_key(m, Key::Home),
                i.consume_key(m, Key::End),
            )
        });
        let to = if l {
            seg_step(&en, cur, false)
        } else if r {
            seg_step(&en, cur, true)
        } else if home {
            seg_edge(&en, false)
        } else if end {
            seg_edge(&en, true)
        } else {
            None
        };
        if let Some(t) = to.filter(|&t| t != cur) {
            focus_to = Some(t);
            if t != sel {
                out = Some(t);
            }
        }
    }
    // the only pill reachable by Tab: the focused one, else the selected one, else the first enabled
    let stop = focus_to
        .or(focused)
        .filter(|&i| en[i])
        .or_else(|| (sel < en.len() && en[sel]).then_some(sel))
        .or_else(|| seg_edge(&en, false));
    let gs: Vec<_> = items
        .iter()
        .map(|t| {
            ui.painter()
                .layout_no_wrap(t.to_string(), font.clone(), th.c("text.primary"))
        })
        .collect();
    // pills tighten their padding (down to XS) before the group outgrows a narrow panel
    let text_w: f32 = gs.iter().map(|g| g.size().x).sum();
    let room = ui.available_width();
    let n = gs.len().max(1) as f32;
    let natural = text_w + 2.0 * n * space::MD + 2.0 * space::XXS;
    let pad = if room.is_finite() && natural > room {
        ((room - text_w - 2.0 * space::XXS) / (2.0 * n)).clamp(space::XS, space::MD)
    } else {
        space::MD
    };
    let ws: Vec<f32> = gs.iter().map(|g| g.size().x + 2.0 * pad).collect();
    let total = ws.iter().sum::<f32>() + 2.0 * space::XXS;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(total, h), Sense::hover());
    ui.painter()
        .rect_filled(rect, th.radius("md"), th.c("bg.sunken"));
    // the selected pill is painted into this slot, under the labels, so it can slide
    let runner_slot = ui.painter().add(egui::Shape::Noop);
    // keyed by the item list: several controls in one Ui must not share one sliding runner
    let runner_id = ui.id().with(("seg_runner", items));
    // inner radius = outer radius - track padding
    let pill_r = (th.radius("md") - space::XXS).max(0.0);
    let shown = focus_to.unwrap_or(sel);
    let mut x = rect.left() + space::XXS;
    for (i, g) in gs.into_iter().enumerate() {
        let r = Rect::from_min_size(
            egui::pos2(x, rect.top() + space::XXS),
            Vec2::new(ws[i], h - 2.0 * space::XXS),
        );
        x += ws[i];
        let sense = if !en[i] {
            Sense::hover()
        } else if Some(i) == stop {
            Sense::click()
        } else {
            Sense::CLICK
        };
        let resp = ui.interact(r, ids[i], sense);
        if focus_to == Some(i) {
            resp.request_focus();
        }
        let on = i == shown;
        resp.widget_info(|| seg_info(items[i], en[i], on));
        let p = ui.painter();
        if on {
            let at = ui.style().animation_time;
            // slide along the track only: a track that moves with its row (a window settling,
            // a scroll) must not drag the pill over its neighbours
            let ax = ui.ctx().animate_value_with_time(runner_id, r.left() - rect.left(), at);
            let aw = ui.ctx().animate_value_with_time(runner_id.with("w"), r.width(), at);
            let ar = Rect::from_min_size(
                egui::pos2(rect.left() + ax, r.top()),
                Vec2::new(aw, r.height()),
            )
            .intersect(rect.shrink(space::XXS));
            p.set(
                runner_slot,
                egui::Shape::Vec(vec![
                    th.shadow(1).as_shape(ar, pill_r).into(),
                    egui::Shape::rect_filled(ar, pill_r, th.c("bg.elevated")),
                    egui::Shape::rect_stroke(
                        ar,
                        pill_r,
                        Stroke::new(stroke::HAIRLINE, th.c("border.subtle")),
                        StrokeKind::Inside,
                    ),
                ]),
            );
        } else if en[i] && resp.hovered() {
            p.rect_filled(r, pill_r, th.c("bg.hover"));
        }
        // 1 px divider between two neighbours that are both unselected
        if i > 0 && !on && i - 1 != shown {
            let x = r.left();
            p.vline(
                x,
                r.shrink2(Vec2::new(0.0, space::XS)).y_range(),
                Stroke::new(stroke::HAIRLINE, th.c("border.subtle")),
            );
        }
        let col = if !en[i] && group_on {
            th.c("text.disabled")
        } else if on {
            th.c("text.primary")
        } else {
            th.c("text.secondary")
        };
        p.galley_with_override_text_color(r.center() - g.size() / 2.0, g, col);
        if en[i] && resp.clicked() && i != sel {
            out = Some(i);
        }
    }
    out
}

/// Thin vertical divider between toolbar groups.
pub fn vdivider(ui: &mut Ui, th: &Theme, h: f32) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(space::SM + stroke::HAIRLINE, h), Sense::hover());
    let ppp = ui.ctx().pixels_per_point();
    ui.painter().vline(
        crate::px::snap(r.center().x, ppp),
        r.shrink2(Vec2::new(0.0, space::XS)).y_range(),
        crate::px::stroke(ppp, th.c("border.subtle")),
    );
}

/// Keyboard focus ring over everything: visible on any widget, including text inputs.
/// Drawn on the widget's own layer and clipped by its clip-rect; only after keyboard navigation.
pub fn focus_ring(ctx: &egui::Context, th: &Theme) {
    let visible = track_nav(ctx);
    let own: Vec<egui::Id> = ctx
        .data_mut(|d| d.remove_temp(egui::Id::new(OWN_FOCUS)))
        .unwrap_or_default();
    let Some(id) = ctx.memory(|m| m.focused()) else {
        return;
    };
    if !visible || own.contains(&id) {
        return;
    }
    let Some(resp) = ctx.read_response(id) else {
        return;
    };
    let top_is_own = ctx
        .layer_id_at(resp.interact_rect.center())
        .map_or(true, |l| l == resp.layer_id);
    let Some(r) = ring_rect(resp.rect, resp.interact_rect, ctx.screen_rect(), top_is_own) else {
        return;
    };
    let w = crate::theme::focus::OFFSET + crate::theme::focus::WIDTH;
    let p = ctx
        .layer_painter(resp.layer_id)
        .with_clip_rect(ring_clip(resp.rect, r, w));
    th.paint_focus_ring(&p, r, th.radius("md"));
}

const OWN_FOCUS: &str = "ddc.focus.own";
const NAV_STATE: &str = "ddc.focus.nav";

/// A widget that paints its own focus mark tells the global ring to stay away (valid for this frame).
pub fn own_focus(ctx: &egui::Context, id: egui::Id) {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Vec<egui::Id>>(egui::Id::new(OWN_FOCUS))
            .push(id)
    });
}

#[derive(Clone, Default)]
struct NavState {
    seq: u64,
    key: Option<u64>,
    pointer: Option<u64>,
}

/// Whether focus is keyboard-driven now, counting this frame's events without consuming them
/// (widgets that paint their own ring ask this while drawing).
pub fn keyboard_focus(ctx: &egui::Context) -> bool {
    let mut st: NavState = ctx
        .data(|d| d.get_temp(egui::Id::new(NAV_STATE)))
        .unwrap_or_default();
    replay_nav(ctx, &mut st);
    focus_visible(st.key, st.pointer)
}

/// Replays this frame's input events in order and says whether focus is keyboard-driven now.
fn track_nav(ctx: &egui::Context) -> bool {
    let mut st: NavState = ctx
        .data(|d| d.get_temp(egui::Id::new(NAV_STATE)))
        .unwrap_or_default();
    replay_nav(ctx, &mut st);
    let v = focus_visible(st.key, st.pointer);
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(NAV_STATE), st));
    v
}

fn replay_nav(ctx: &egui::Context, st: &mut NavState) {
    ctx.input(|i| {
        for e in &i.events {
            match e {
                egui::Event::Key { key, pressed: true, .. }
                    if matches!(
                        key,
                        Key::Tab | Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight
                    ) =>
                {
                    st.seq += 1;
                    st.key = Some(st.seq);
                }
                egui::Event::PointerButton { pressed: true, .. } => {
                    st.seq += 1;
                    st.pointer = Some(st.seq);
                }
                _ => {}
            }
        }
    });
}

/// Focus ring shows only when the last navigation event was a key, not a pointer press.
pub fn focus_visible(last_key_nav: Option<u64>, last_pointer: Option<u64>) -> bool {
    match (last_key_nav, last_pointer) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(k), Some(p)) => k > p,
    }
}

/// Visible part of a widget rect for the ring; `None` when scrolled out, off screen or covered.
pub fn ring_rect(rect: Rect, interact_rect: Rect, screen: Rect, top_is_own: bool) -> Option<Rect> {
    if !top_is_own {
        return None;
    }
    let r = rect.intersect(interact_rect).intersect(screen);
    (r.width() > 0.0 && r.height() > 0.0).then_some(r)
}

/// Clip for the ring: the visible rect, widened by the ring width only on sides the widget is not cut on.
fn ring_clip(full: Rect, visible: Rect, w: f32) -> Rect {
    let lo = |v: f32, f: f32| if v <= f { v - w } else { v };
    let hi = |v: f32, f: f32| if v >= f { v + w } else { v };
    Rect::from_min_max(
        egui::pos2(lo(visible.min.x, full.min.x), lo(visible.min.y, full.min.y)),
        egui::pos2(hi(visible.max.x, full.max.x), hi(visible.max.y, full.max.y)),
    )
}

/// Paint hover/pressed/on background; return the foreground tint.
pub fn paint_state(ui: &Ui, th: &Theme, rect: Rect, resp: &egui::Response, on: bool) -> Color32 {
    let r = th.radius("md");
    let p = ui.painter();
    if !ui.is_enabled() {
        // the disabled Ui fades its painter, the colour must not dim a second time
        return th.c("text.secondary");
    }
    if on {
        p.rect_filled(rect, r, th.c("accent.subtle"));
        return th.c("accent");
    }
    if resp.is_pointer_button_down_on() {
        p.rect_filled(rect, r, th.pressed_fill());
        return th.c("text.primary");
    }
    // hover fades in/out over the theme motion time (instant with reduced motion)
    let t = ui.ctx().animate_bool_with_time(
        resp.id.with("ddc.hover"),
        resp.hovered() || resp.has_focus(),
        ui.style().animation_time,
    );
    if t > 0.0 {
        p.rect_filled(rect, r, th.hover_fill().gamma_multiply(t));
    }
    th.c("text.secondary")
        .lerp_to_gamma(th.c("text.primary"), t)
}

/// Keycaps painted right-aligned at `right` (vertical centre); returns their left edge.
pub fn paint_keycaps(p: &egui::Painter, th: &Theme, right: egui::Pos2, keys: &str) -> f32 {
    let font = th.font(Role::Keycap);
    let mut x = right.x;
    for k in keys.split(['+', ' ']).filter(|k| !k.is_empty()).rev() {
        let g = p.layout_no_wrap(k.to_string(), font.clone(), th.c("text.secondary"));
        let w = (g.size().x + space::MD).max(control::KEYCAP_H);
        let r = Rect::from_min_size(
            egui::pos2(x - w, right.y - control::KEYCAP_H / 2.0),
            Vec2::new(w, control::KEYCAP_H),
        );
        p.rect(
            r,
            th.radius("sm"),
            th.c("bg.app"),
            Stroke::new(stroke::HAIRLINE, th.c("border.subtle")),
            StrokeKind::Inside,
        );
        p.galley(r.center() - g.size() / 2.0, g, th.c("text.secondary"));
        x = r.left() - space::XS;
    }
    x
}

/// Width `paint_keycaps` takes for `keys`, measured without painting.
pub fn keycaps_width(p: &egui::Painter, th: &Theme, keys: &str) -> f32 {
    let font = th.font(Role::Keycap);
    let caps = keys.split(['+', ' ']).filter(|k| !k.is_empty());
    let w: f32 = caps
        .map(|k| {
            let g = p.layout_no_wrap(k.to_string(), font.clone(), th.c("text.secondary"));
            (g.size().x + space::MD).max(control::KEYCAP_H) + space::XS
        })
        .sum();
    (w - space::XS).max(0.0)
}

#[cfg(test)]
mod tip_tests {
    use super::*;
    use crate::theme::{Density, Mode};

    /// Runs frames with the pointer on a button; returns the tooltip area's rect per frame.
    fn run(times: &[f64]) -> (egui::Rect, Vec<Option<egui::Rect>>) {
        let th = Theme::new(Mode::Light, Density::Comfortable);
        let ctx = crate::theme::headless_ctx();
        let mut anchor = egui::Rect::NOTHING;
        let mut out = vec![];
        let tip_id = std::cell::Cell::new(egui::Id::NULL);
        for (n, t) in times.iter().enumerate() {
            let ev = if n == 0 { vec![] } else { vec![egui::Event::PointerMoved(anchor.center())] };
            let input = egui::RawInput { events: ev, time: Some(*t), ..Default::default() };
            let _ = ctx.run(input, |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    let r = ui.button("anchor");
                    anchor = r.rect;
                    tip_id.set(r.id.with("ddc.tip"));
                    rich_tooltip(r, &th, "Title", "", "");
                });
            });
            out.push(ctx.memory(|m| m.area_rect(tip_id.get())));
        }
        (anchor, out)
    }

    #[test]
    fn tooltip_waits_500ms_and_never_covers_the_anchor() {
        // frame 1 starts the hover at t=1.1; the tip may show from t=1.6
        let (anchor, shown) = run(&[1.0, 1.1, 1.3, 1.7, 2.0]);
        assert!(shown[1].is_none() && shown[2].is_none(), "shown before 500 ms: {shown:?}");
        let r = shown[3].expect("tooltip after the delay");
        assert!(!r.intersects(anchor), "{r:?} covers {anchor:?}");
        assert_eq!(r.top() - anchor.bottom(), crate::ctx_tips::TIP_GAP);
    }
}
