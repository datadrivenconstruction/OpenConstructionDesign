// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Small building blocks of the window chrome: the one primary button, quiet destructive
//! button, elided text, progress bar, action row and drop frame.
use super::{icon_name, paint_state};
use crate::icons::IconCache;
use crate::theme::Role;
use crate::Theme;
use egui::{Color32, Galley, Rect, RichText, Sense, Stroke, Ui, Vec2};
use std::sync::Arc;

/// The single primary action of a dialog or screen.
pub fn primary(ui: &mut Ui, th: &Theme, text: &str) -> egui::Response {
    super::Button::primary(text).show(ui, th)
}

/// Secondary action: same height as [`primary`], no accent.
pub fn secondary(ui: &mut Ui, th: &Theme, text: &str) -> egui::Response {
    super::Button::secondary(text).show(ui, th)
}

/// Destructive action: never filled, never the default.
pub fn destructive(ui: &mut Ui, th: &Theme, text: &str) -> egui::Response {
    super::Button::danger(text).show(ui, th)
}

/// One line of text that ends in "…" when it does not fit `max_w`.
pub fn elided(ui: &Ui, text: &str, font: egui::FontId, color: Color32, max_w: f32) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.into(), font, color);
    job.wrap = egui::text::TextWrapping {
        max_width: max_w.max(16.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.fonts(|f| f.layout_job(job))
}

/// Section caption inside a screen or a settings page.
pub fn section_title(ui: &mut Ui, th: &Theme, text: &str) {
    ui.label(
        RichText::new(text)
            .font(th.font(Role::Section))
            .color(th.c("text.secondary")),
    );
}

/// Progress track; `None` — amount unknown: a moving block, or a still one with reduced
/// motion. Never a made-up percentage.
pub fn paint_bar(ui: &Ui, th: &Theme, rect: Rect, fraction: Option<f32>) {
    let p = ui.painter();
    let r = rect.height() / 2.0;
    p.rect_filled(rect, r, th.c("bg.sunken"));
    let fill = match fraction {
        Some(f) => Rect::from_min_size(
            rect.min,
            Vec2::new(rect.width() * f.clamp(0.0, 1.0), rect.height()),
        ),
        None => {
            let w = rect.width() * 0.3;
            let x = if th.reduced_motion {
                (rect.width() - w) / 2.0
            } else {
                let t = ui.input(|i| i.time) as f32 * 0.6;
                (rect.width() - w) * (1.0 - (t.fract() * 2.0 - 1.0).abs())
            };
            Rect::from_min_size(rect.min + Vec2::new(x, 0.0), Vec2::new(w, rect.height()))
        }
    };
    p.rect_filled(fill, r, th.c("accent"));
}

/// Wide row of the start screen: icon, title, second line, optional shortcut on the right.
/// `strong` paints the one primary action of the screen.
#[allow(clippy::too_many_arguments)]
pub fn action_row(
    ui: &mut Ui,
    th: &Theme,
    icons: &mut IconCache,
    icon: &str,
    title: &str,
    sub: &str,
    keys: &str,
    strong: bool,
) -> egui::Response {
    let w = ui.available_width();
    // the caption wraps to a second line before it is ever cut
    let keys_w = if keys.is_empty() {
        0.0
    } else {
        let font = th.font(Role::Caption);
        let g = ui.fonts(|f| f.layout_no_wrap(keys.to_owned(), font, Color32::PLACEHOLDER));
        g.size().x + 12.0
    };
    let sub_room = w - 48.0 - keys_w;
    let sub_g = (!sub.is_empty()).then(|| {
        let mut job = egui::text::LayoutJob::simple(
            sub.to_owned(),
            th.font(Role::Caption),
            Color32::PLACEHOLDER,
            sub_room.max(1.0),
        );
        job.wrap.max_rows = 2;
        ui.fonts(|f| f.layout_job(job))
    });
    let h = match &sub_g {
        None => 36.0,
        Some(g) => (g.size().y + 36.0).max(52.0),
    };
    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    let (fg, sub_col) = if strong {
        let fill = if resp.hovered() {
            th.c("accent").gamma_multiply(0.9)
        } else {
            th.c("accent")
        };
        ui.painter().rect_filled(r, th.radius("md"), fill);
        (th.c("accent.text_on"), th.c("accent.text_on"))
    } else {
        let fg = paint_state(ui, th, r, &resp, false);
        (fg, th.c("text.secondary"))
    };
    let ic = Rect::from_center_size(r.left_center() + Vec2::new(20.0, 0.0), Vec2::splat(20.0));
    icons.paint(ui, &icon_name(icon, false), 20, ic, fg);
    let mut room = r.width() - 40.0 - 8.0;
    if !keys.is_empty() {
        let g = ui.painter().text(
            r.right_center() - Vec2::new(10.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            keys,
            th.font(Role::Caption),
            sub_col,
        );
        room -= g.width() + 12.0;
    }
    let tg = elided(ui, title, th.font(Role::BodyStrong), fg, room);
    if let Some(sg) = sub_g {
        let top = r.center().y - (tg.size().y + sg.size().y + 2.0) / 2.0;
        let x = r.left() + 40.0;
        let y2 = top + tg.size().y + 2.0;
        ui.painter().galley(egui::pos2(x, top), tg, fg);
        ui.painter().galley(egui::pos2(x, y2), sg, sub_col);
    } else {
        let pos = r.left_center() + Vec2::new(40.0, -tg.size().y / 2.0);
        ui.painter().galley(pos, tg, fg);
    }
    resp
}

/// Dashed outline of a drop target.
pub fn dashed_frame(ui: &Ui, rect: Rect, color: Color32) {
    let pts = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    let shapes = egui::Shape::dashed_line(&pts, Stroke::new(1.5, color), 6.0, 4.0);
    ui.painter().extend(shapes);
}

/// Thin chevron button: `left` points to the left, otherwise to the right.
pub fn chevron_button(ui: &mut Ui, th: &Theme, left: bool, tip: &str) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    let fg = paint_state(ui, th, r, &resp, false);
    let c = r.center();
    let s = if left { 2.0 } else { -2.0 };
    ui.painter().add(egui::Shape::line(
        vec![
            c + Vec2::new(s, -4.0),
            c + Vec2::new(-s, 0.0),
            c + Vec2::new(s, 4.0),
        ],
        Stroke::new(1.5, fg),
    ));
    super::rich_tooltip(resp, th, tip, "", "")
}

/// Disclosure chevron of a list row: points right when closed, turns down when open.
pub fn disclosure(ui: &mut Ui, th: &Theme, open: bool, tip: &str) -> egui::Response {
    let side = th.d().control_sm;
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    let enabled = ui.is_enabled();
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, enabled, open, tip)
    });
    if ui.is_rect_visible(rect) {
        let fg = paint_state(ui, th, rect, &resp, false);
        let t = ui.ctx().animate_bool_with_time(
            resp.id.with("ddc.open"),
            open,
            ui.style().animation_time,
        );
        let a = (1.0 - t) * -std::f32::consts::FRAC_PI_2;
        let c = rect.center();
        let (sin, cos) = a.sin_cos();
        let rot = |x: f32, y: f32| c + Vec2::new(x * cos - y * sin, x * sin + y * cos);
        ui.painter().add(egui::Shape::line(
            vec![rot(-3.5, -1.75), rot(0.0, 1.75), rot(3.5, -1.75)],
            Stroke::new(1.25, fg),
        ));
    }
    super::rich_tooltip(resp, th, tip, "", "")
}

/// What [`menu_title`] shows.
pub struct MenuTitle<'a> {
    /// Fixed id: the popup of this control is `id.with("popup")`, so a shortcut can open it.
    pub id: egui::Id,
    pub name: &'a str,
    pub dirty: bool,
    /// The menu is open: keeps the pressed look.
    pub open: bool,
    /// Longest name before it ends in "…".
    pub max_w: f32,
    /// Second tooltip line (the save state), empty when there is none.
    pub desc: &'a str,
    pub keys: &'a str,
}

/// Title bar control: the project name in primary text, a dot while unsaved and a chevron, one
/// hit target that opens the File menu. The full name is in the tooltip.
pub fn menu_title(ui: &mut Ui, th: &Theme, m: &MenuTitle) -> egui::Response {
    let pad = crate::theme::space::SM;
    let gap = crate::theme::space::XS;
    let (dot, caret) = (6.0, 8.0);
    let fg = th.c("text.primary");
    let gal = elided(ui, m.name, th.font(Role::BodyStrong), fg, m.max_w);
    let dot_w = if m.dirty { gap + dot } else { 0.0 };
    let w = pad + gal.size().x + dot_w + gap + caret + pad;
    let (r, _) = ui.allocate_exact_size(Vec2::new(w, th.d().control), Sense::hover());
    let resp = ui.interact(r, m.id, Sense::click());
    let enabled = ui.is_enabled();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, m.name));
    if m.open && enabled {
        ui.painter().rect_filled(r, th.radius("md"), th.pressed_fill());
    }
    let tint = paint_state(ui, th, r, &resp, false);
    let name_col = if enabled { fg } else { tint };
    let p = ui.painter();
    let mut x = r.left() + pad;
    p.galley(egui::pos2(x, r.center().y - gal.size().y / 2.0), gal.clone(), name_col);
    x += gal.size().x;
    if m.dirty {
        p.circle_filled(egui::pos2(x + gap + dot / 2.0, r.center().y), dot / 2.0, th.c("warning"));
        x += dot_w;
    }
    let c = egui::pos2(x + gap + caret / 2.0, r.center().y);
    let k = caret / 2.0;
    let dy = if m.open { -k / 2.0 } else { k / 2.0 };
    p.add(egui::Shape::line(
        vec![c + Vec2::new(-k, -dy), c + Vec2::new(0.0, dy), c + Vec2::new(k, -dy)],
        Stroke::new(1.5, tint),
    ));
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if m.open {
        resp
    } else {
        super::rich_tooltip(resp, th, m.name, m.desc, m.keys)
    }
}
