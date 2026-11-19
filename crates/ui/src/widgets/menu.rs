//! Menu rows for context menus: one quiet row per command, a destructive row in error ink,
//! hairline divider between groups.
use super::paint_keycaps;
use crate::icons::IconCache;
use crate::theme::{control, icon_size, space, Role};
use crate::Theme;
use egui::containers::menu::{MenuState, SubMenu};
use egui::{pos2, vec2, Response, Sense, Ui, Vec2};
use std::sync::{Arc, Mutex};

/// How a menu row reads: neutral, or destructive (quiet, never filled).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuTone {
    Normal,
    Danger,
}

/// Colour token of a row's label.
pub fn ink_token(tone: MenuTone, enabled: bool) -> &'static str {
    match (enabled, tone) {
        (false, _) => "text.disabled",
        (true, MenuTone::Danger) => "error",
        (true, MenuTone::Normal) => "text.primary",
    }
}

/// Left edge of a row's label: after the fixed icon slot when the menu uses icons, so labels
/// line up whether or not a row has an icon.
pub fn label_x(left: f32, slot: bool) -> f32 {
    left + space::SM + if slot { icon_size::SM + space::SM } else { 0.0 }
}

/// What the right end of a row shows.
#[derive(Clone, Copy, PartialEq)]
enum Tail {
    Keys,
    Chevron,
}

type SharedIcons = Arc<Mutex<IconCache>>;

fn paint_icon(ui: &Ui, name: &str, rect: egui::Rect, color: egui::Color32) {
    let shared: SharedIcons = ui
        .ctx()
        .data_mut(|d| d.get_temp_mut_or_insert_with(egui::Id::new("ddc.menu.icons"), SharedIcons::default).clone());
    let mut cache = shared.lock().unwrap_or_else(|e| e.into_inner());
    cache.paint(ui, name, icon_size::SM as u32, rect, color);
}

/// Width a row needs to show its label and its tail (shortcut or chevron) whole, with the
/// standard gap between them.
fn row_need(ui: &Ui, th: &Theme, slot: bool, label: &str, keys: &str, tail: Tail) -> f32 {
    let p = ui.painter();
    let text = p.layout_no_wrap(label.to_owned(), th.font(Role::Body), th.c("text.primary"));
    label_x(0.0, slot) + text.size().x + tail_w(ui, th, keys, tail)
}

/// Right part of a row after its label: the gap, the shortcut or chevron, the right margin.
fn tail_w(ui: &Ui, th: &Theme, keys: &str, tail: Tail) -> f32 {
    match tail {
        Tail::Keys if keys.is_empty() => END,
        Tail::Keys => space::XL + super::keycaps_width(ui.painter(), th, keys) + END,
        Tail::Chevron => space::XL + END,
    }
}

/// Right margin of a row: the shortcut column and the chevron end here.
const END: f32 = space::MD;

/// Hover plate inset from the popover's side edges.
pub const HOVER_INSET: f32 = space::XS;
/// Popover open time and submenu open delay, seconds (`motion` base; spec 2.12). The popup is
/// removed by the toolkit on close, so only the enter is animated.
pub const ENTER_S: f32 = 0.16;
pub const SUB_DELAY_S: f32 = 0.15;
/// Row height in the compact and dense densities (the menu token is for comfortable).
pub const ROW_COMPACT_H: f32 = 24.0;

/// True once the pointer has rested `SUB_DELAY_S` on a submenu row (`since` = hover start).
pub fn sub_delay_elapsed(since: Option<f64>, now: f64) -> bool {
    since.is_some_and(|t| now - t >= SUB_DELAY_S as f64)
}

/// Fade + scale-in of the popover `ui` belongs to: restarts whenever the popup was not drawn
/// the frame before. Instant with reduced motion.
fn animate_enter(ui: &mut Ui, th: &Theme) {
    let ctx = ui.ctx().clone();
    let key = ui.id().with("ddc.menu.enter");
    let frame = ctx.cumulative_pass_nr();
    let last: Option<u64> = ctx.data_mut(|d| d.get_temp(key));
    if last.is_none_or(|l| frame > l + 1) {
        ctx.animate_bool_with_time(key, false, 0.0);
    }
    ctx.data_mut(|d| d.insert_temp(key, frame));
    let time = if th.reduced_motion { 0.0 } else { ENTER_S };
    let t = ctx.animate_bool_with_time(key, true, time);
    if t < 1.0 {
        ui.set_opacity(t);
        let s = enter_scale(t, th.reduced_motion);
        let o = ui.max_rect().left_top().to_vec2();
        ctx.set_transform_layer(
            ui.layer_id(),
            egui::emath::TSTransform::new(o * (1.0 - s), s),
        );
    } else {
        ctx.set_transform_layer(ui.layer_id(), egui::emath::TSTransform::IDENTITY);
    }
}
/// Popover scale at the start of the enter animation.
pub const ENTER_SCALE_FROM: f32 = 0.98;

/// Hover / highlight plate of a row: inset from the popover edges.
pub fn hover_rect(row: egui::Rect) -> egui::Rect {
    row.shrink2(vec2(HOVER_INSET, 0.0))
}

/// Right edge of a row's shortcut column (keys end here, all rows alike).
pub fn keys_right(row: egui::Rect) -> f32 {
    row.right() - END
}

/// Popover scale at enter progress `t` (0..1); no scaling with reduced motion.
pub fn enter_scale(t: f32, reduced_motion: bool) -> f32 {
    if reduced_motion {
        return 1.0;
    }
    ENTER_SCALE_FROM + (1.0 - ENTER_SCALE_FROM) * t.clamp(0.0, 1.0)
}

/// Section title row of a menu (role section, caps), as wide as the rows.
pub fn menu_section(ui: &mut Ui, th: &Theme, title: &str) {
    let w = ui.available_width().clamp(control::MENU_W, control::MENU_MAX_W);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, row_h(th)), Sense::hover());
    let g = ui.painter().layout_no_wrap(title.to_uppercase(), th.font(Role::Section), th.c("text.secondary"));
    ui.painter().galley(
        pos2(rect.left() + space::SM + HOVER_INSET, rect.center().y - g.size().y / 2.0),
        g,
        th.c("text.secondary"),
    );
}

/// Height of a menu row: 28 (comfortable), 24 in the compact and dense densities.
pub fn row_h(th: &Theme) -> f32 {
    match th.density {
        crate::theme::Density::Comfortable => control::MENU_ROW_H,
        _ => ROW_COMPACT_H,
    }
}

/// Paints a row and returns its response and the label's left edge.
#[allow(clippy::too_many_arguments)]
fn row(
    ui: &mut Ui,
    th: &Theme,
    icon: Option<&str>,
    slot: bool,
    label: &str,
    keys: &str,
    tone: MenuTone,
    enabled: bool,
    tail: Tail,
    lit: bool,
) -> (Response, f32) {
    // the row grows to its label and shortcut up to the widest menu; past that the label ends
    // with «…» and shows whole on hover, the shortcut never moves
    if egui::containers::menu::is_in_menu(ui) {
        animate_enter(ui, th);
    }
    let need = row_need(ui, th, slot, label, keys, tail).min(control::MENU_MAX_W);
    let w = ui.available_width().clamp(control::MENU_W, control::MENU_MAX_W).max(need);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(w, row_h(th)), sense);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    let lx = label_x(rect.left(), slot);
    let room = (rect.right() - tail_w(ui, th, keys, tail) - lx).max(1.0);
    if ui.is_rect_visible(rect) {
        let ink = th.c(ink_token(tone, enabled));
        if enabled && (resp.hovered() || lit) {
            ui.painter().rect_filled(hover_rect(rect), th.radius("sm"), th.hover_fill());
        }
        if let Some(name) = icon {
            let side = icon_size::SM;
            let at = pos2(rect.left() + space::SM, rect.center().y - side / 2.0);
            paint_icon(ui, name, egui::Rect::from_min_size(at, Vec2::splat(side)), ink);
        }
        let g = super::elide(ui, label, th.font(Role::Body), ink, room);
        if g.elided {
            resp = resp.on_hover_text(label);
        }
        let p = ui.painter();
        p.galley(pos2(lx, rect.center().y - g.size().y / 2.0), g, ink);
        match tail {
            Tail::Keys if !keys.is_empty() => {
                paint_keycaps(p, th, pos2(keys_right(rect), rect.center().y), keys);
            }
            Tail::Keys => {}
            Tail::Chevron => {
                let side = icon_size::SM;
                let at = pos2(rect.right() - END - side, rect.center().y - side / 2.0);
                paint_icon(ui, crate::icons::ctl::CHEVRON_RIGHT, egui::Rect::from_min_size(at, Vec2::splat(side)), ink);
            }
        }
    }
    (resp, lx)
}

/// One row of a menu. A disabled row never reports a click. `keys` is a shortcut («Ctrl+P»).
pub fn menu_item(
    ui: &mut Ui,
    th: &Theme,
    label: &str,
    keys: &str,
    tone: MenuTone,
    enabled: bool,
) -> Response {
    row(ui, th, None, false, label, keys, tone, enabled, Tail::Keys, false).0
}

/// Row of a menu with an icon slot: `icon` is a name from the icon set or `None`; the slot is
/// reserved either way, so a menu mixing both keeps its labels aligned.
pub fn menu_item_icon(
    ui: &mut Ui,
    th: &Theme,
    icon: Option<&str>,
    label: &str,
    keys: &str,
    tone: MenuTone,
    enabled: bool,
) -> Response {
    row(ui, th, icon, true, label, keys, tone, enabled, Tail::Keys, false).0
}

/// Submenu row (icon slot as in `menu_item_icon`, chevron at the right end); `add` fills the
/// popup that opens beside it. A disabled row opens nothing.
pub fn menu_sub<R>(
    ui: &mut Ui,
    th: &Theme,
    icon: Option<&str>,
    label: &str,
    enabled: bool,
    add: impl FnOnce(&mut Ui) -> R,
) -> Response {
    let id = ui.next_auto_id();
    let open = MenuState::from_ui(ui, |s, _| s.open_item == Some(SubMenu::id_from_widget_id(id)));
    let (resp, _) = row(ui, th, icon, true, label, "", MenuTone::Normal, enabled, Tail::Chevron, open);
    // opens after the pointer has rested SUB_DELAY_S (or on click / when already open); the
    // toolkit keeps it open while the pointer moves toward the popup (intent triangle)
    let now = ui.input(|i| i.time);
    let hkey = resp.id.with("ddc.sub.hover");
    let since: Option<f64> = ui.data_mut(|d| d.get_temp(hkey));
    let since = if resp.hovered() {
        let s = since.unwrap_or(now);
        ui.data_mut(|d| d.insert_temp(hkey, s));
        s
    } else {
        ui.data_mut(|d| d.remove_temp::<f64>(hkey));
        f64::NAN
    };
    let ready = open || resp.clicked() || sub_delay_elapsed(Some(since), now);
    if enabled && resp.hovered() && !ready {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f32(SUB_DELAY_S));
    }
    if enabled && ready {
        SubMenu::new().show(ui, &resp, |ui| {
            ui.set_min_width(control::MENU_W);
            add(ui)
        });
    }
    resp
}

/// Hairline between groups of menu rows.
pub fn menu_divider(ui: &mut Ui, th: &Theme) {
    // as wide as the rows: never the one item that stretches the menu
    let w = ui.available_width().clamp(control::MENU_W, control::MENU_MAX_W);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, space::SM), Sense::hover());
    let ppp = ui.ctx().pixels_per_point();
    ui.painter().hline(
        rect.x_range(),
        crate::px::snap(rect.center().y, ppp),
        crate::px::stroke(ppp, th.c("border.subtle")),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ink_follows_tone_and_state() {
        assert_eq!(ink_token(MenuTone::Normal, true), "text.primary");
        assert_eq!(ink_token(MenuTone::Danger, true), "error");
        assert_eq!(ink_token(MenuTone::Danger, false), "text.disabled");
        assert_eq!(ink_token(MenuTone::Normal, false), "text.disabled");
    }

    #[test]
    fn label_x_ignores_the_icon_but_not_the_slot() {
        assert_eq!(label_x(10.0, true), label_x(10.0, true));
        assert!(label_x(10.0, true) > label_x(10.0, false));
        assert_eq!(label_x(0.0, false), space::SM);
        assert_eq!(label_x(0.0, true), space::SM + icon_size::SM + space::SM);
    }

    #[test]
    fn icon_and_plain_rows_share_label_x_and_size() {
        use crate::theme::{Density, Mode};
        let th = Theme::new(Mode::Light, Density::Comfortable);
        let ctx = egui::Context::default();
        let mut got = Vec::new();
        let _ = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                let w = ui.available_width().max(control::DROPDOWN_W);
                for icon in [Some("open"), None, Some("no-such-icon")] {
                    let (r, lx) = row(
                        ui, &th, icon, true, "Row", "", MenuTone::Normal, true, Tail::Keys, false,
                    );
                    got.push((lx - r.rect.left(), r.rect.size()));
                }
                let (r, lx) = row(
                    ui, &th, None, false, "Row", "", MenuTone::Normal, true, Tail::Keys, false,
                );
                got.push((lx - r.rect.left(), r.rect.size()));
                assert!(w > 0.0);
            });
        });
        assert_eq!(got.len(), 4);
        assert_eq!(got[0], got[1]);
        assert_eq!(got[0], got[2]);
        assert_eq!(got[3].1, got[0].1);
        assert!(got[3].0 < got[0].0);
    }

    #[test]
    fn submenu_renders_and_opens_on_hover() {
        use crate::theme::{Density, Mode};
        use std::cell::Cell;
        let th = Theme::new(Mode::Light, Density::Comfortable);
        let ctx = egui::Context::default();
        let sub_rect = Cell::new(egui::Rect::NOTHING);
        let opened = Cell::new(false);
        let clock = Cell::new(1.0_f64);
        let anchor = Cell::new(egui::Rect::NOTHING);
        let frame = |events: Vec<egui::Event>| {
            // 0.1 s per frame: the submenu opens only after SUB_DELAY_S of hover
            clock.set(clock.get() + 0.1);
            let input = egui::RawInput { events, time: Some(clock.get()), ..Default::default() };
            let _ = ctx.run(input, |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    let r = ui.label("anchor");
                    anchor.set(r.rect);
                    r.interact(Sense::click()).context_menu(|ui| {
                        let rr = menu_sub(ui, &th, Some("select-all"), "Sub", true, |ui| {
                            opened.set(true);
                            menu_item_icon(ui, &th, None, "Inner", "", MenuTone::Normal, true);
                        });
                        sub_rect.set(rr.rect);
                    });
                });
            });
        };
        frame(vec![]);
        let at = anchor.get().center();
        let btn = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: Default::default(),
        };
        frame(vec![egui::Event::PointerMoved(at), btn(true), btn(false)]);
        for _ in 0..3 {
            frame(vec![]);
        }
        assert!(sub_rect.get().is_positive(), "submenu row was not laid out");
        frame(vec![egui::Event::PointerMoved(sub_rect.get().center())]);
        frame(vec![]);
        assert!(!opened.get(), "submenu opened before the {SUB_DELAY_S} s hover delay");
        for _ in 0..4 {
            frame(vec![egui::Event::PointerMoved(sub_rect.get().center())]);
        }
        assert!(opened.get(), "submenu did not open");
    }

    #[test]
    fn rows_fit_label_and_keys_within_the_menu_width_range() {
        use crate::theme::{Density, Mode};
        let th = Theme::new(Mode::Light, Density::Compact);
        let ctx = crate::theme::headless_ctx();
        let mut got = vec![];
        let _ = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                ui.set_max_width(2000.0);
                let long = "Export the whole project to a spreadsheet book with every sheet";
                for (label, keys) in [("Open", "Ctrl+O"), (long, "Ctrl+Shift+E")] {
                    let (r, lx) = row(
                        ui, &th, None, true, label, keys, MenuTone::Normal, true, Tail::Keys, false,
                    );
                    let need = row_need(ui, &th, true, label, keys, Tail::Keys);
                    got.push((r.rect, lx, need));
                }
            });
        });
        for (r, _, _) in &got {
            // spec 2.12: the compact density's menu row is 24, comfortable keeps the 28 token
            assert_eq!(r.height(), ROW_COMPACT_H, "{r:?}");
            assert!(r.width() >= control::MENU_W && r.width() <= control::MENU_MAX_W, "{r:?}");
        }
        assert!(got[1].2 > control::MENU_MAX_W, "the long label needs more than the widest menu");
    }

    #[test]
    fn hover_plate_is_inset_by_4_and_keys_share_one_right_edge() {
        let a = egui::Rect::from_min_size(pos2(10.0, 0.0), vec2(220.0, 28.0));
        let b = egui::Rect::from_min_size(pos2(10.0, 28.0), vec2(220.0, 28.0));
        let h = hover_rect(a);
        assert_eq!(h.left() - a.left(), 4.0);
        assert_eq!(a.right() - h.right(), 4.0);
        assert_eq!((h.top(), h.bottom()), (a.top(), a.bottom()));
        assert_eq!(keys_right(a), keys_right(b));
        assert_eq!(a.right() - keys_right(a), END);
    }

    #[test]
    fn menu_geometry_is_on_the_grid() {
        for v in [control::MENU_ROW_H, control::MENU_W, control::MENU_MAX_W, HOVER_INSET, END] {
            assert_eq!(v % 4.0, 0.0, "{v}");
        }
        assert_eq!(space::SM, 8.0, "divider: 4 above, 4 below the hairline");
    }

    #[test]
    fn menu_motion_follows_tokens_and_reduced_motion() {
        assert_eq!((ENTER_S, SUB_DELAY_S), (0.16, 0.15));
        assert!(!sub_delay_elapsed(None, 5.0));
        assert!(!sub_delay_elapsed(Some(5.0), 5.1));
        assert!(sub_delay_elapsed(Some(5.0), 5.15));
        use crate::theme::{Density, Mode};
        assert_eq!(row_h(&Theme::new(Mode::Light, Density::Comfortable)), 28.0);
        assert_eq!(row_h(&Theme::new(Mode::Light, Density::Compact)), 24.0);
        assert_eq!(row_h(&Theme::new(Mode::Light, Density::Dense)), 24.0);
        assert_eq!(enter_scale(0.0, false), 0.98);
        assert_eq!(enter_scale(1.0, false), 1.0);
        assert_eq!(enter_scale(0.0, true), 1.0);
    }

    #[test]
    fn section_row_is_as_tall_as_an_item_row() {
        use crate::theme::{Density, Mode};
        let th = Theme::new(Mode::Light, Density::Comfortable);
        let ctx = crate::theme::headless_ctx();
        let mut h = 0.0;
        let _ = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                let y = ui.cursor().top();
                menu_section(ui, &th, "Edit");
                h = ui.cursor().top() - y;
            });
        });
        assert_eq!(h, control::MENU_ROW_H);
    }

    #[test]
    fn plain_menu_item_signature_is_unchanged() {
        let src = include_str!("menu.rs");
        assert!(src.contains(
            "pub fn menu_item(\n    ui: &mut Ui,\n    th: &Theme,\n    label: &str,\n    keys: &str,\n    tone: MenuTone,\n    enabled: bool,\n) -> Response {"
        ));
    }
}
