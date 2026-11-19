// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 3D overlays over the viewport: view bar (style, projection, saved views, graphics),
//! numeric section-box bounds, walk-mode hint, temporary-categories badge,
//! category visibility/transparency window, orbit pivot marker.

use super::{Action, CatRule, TempCats, CATS};
use crate::ui_kit as kit;
use crate::ui_kit::length_drag;
use crate::viewport::Viewport;
use ddc_render::{Aabb, Scene, VisualStyle};
use ddc_ui::theme::{radius, space, stroke};
use egui::{self, Pos2, Stroke};

pub(super) fn frame(dark: bool) -> egui::Frame {
    let ov = crate::overlay::pal(dark);
    let (fill, line) = (ov.plate, ov.plate_border);
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(stroke::HAIRLINE, line))
        .corner_radius(radius::MD)
        .inner_margin(egui::Margin::same(space::XS as i8))
}

/// One icon control of the view bar after the style switcher.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarItem {
    pub icon: &'static str,
    /// Drawn in the active (filled, accent) state.
    pub on: bool,
    /// i18n key of the tooltip; never empty.
    pub tip: &'static str,
}

/// The one projection switch (view bar and the cube's menu): orthographic ends a walk.
pub fn set_ortho(v: &mut crate::viewport::Viewport, on: bool) {
    v.camera.ortho = on;
    if on {
        v.nav.walk = None;
    }
}

/// Projection toggle, saved-views dropdown and graphics toggle, in bar order.
pub fn bar_items(ortho: bool, vg_open: bool) -> [BarItem; 3] {
    [
        BarItem {
            icon: if ortho { "ortho" } else { "3d-view" },
            on: ortho,
            tip: if ortho {
                "v3d.proj.to_persp"
            } else {
                "v3d.proj.to_ortho"
            },
        },
        BarItem {
            icon: "view-save",
            on: false,
            tip: "v3d.views",
        },
        BarItem {
            icon: "layers",
            on: vg_open,
            tip: "v3d.vg.title",
        },
    ]
}

/// Walk speed for the HUD: number in the model's unit system (locale decimals) and the i18n key
/// of its unit.
pub fn walk_text(speed_mps: f32, units: crate::units::Units, ru: bool) -> (String, &'static str) {
    let (v, key) = match units {
        crate::units::Units::Metric => (f64::from(speed_mps), "v3d.walk.unit.m"),
        crate::units::Units::Imperial => {
            (f64::from(speed_mps) / crate::units::FOOT, "v3d.walk.unit.ft")
        }
    };
    let n = crate::units::fmt_num_lang(v, 1, ru);
    (n, key)
}

/// Esc drops the temporary hide/isolate while the canvas owns the keys and walk mode (which
/// takes Esc first) is off.
pub fn esc_clears_temp(has_temp: bool, walking: bool, esc: bool, canvas_keys: bool) -> bool {
    has_temp && !walking && esc && canvas_keys
}

/// Draws the overlays; true — the cursor is over them (the click does not go to model picking).
pub fn show(
    v: &mut Viewport,
    ui: &mut egui::Ui,
    rect: egui::Rect,
    scene: &Scene,
    aspect: f32,
    elevation: f64,
) -> bool {
    let ctx = ui.ctx().clone();
    let mut rects: Vec<egui::Rect> = vec![];
    let id = ui.id();
    rects.extend(super::perf_overlay::show(v, ui, rect));

    // view bar: style, projection, saved views, graphics
    let r = egui::Area::new(id.with("v3d_bar"))
        .order(ddc_ui::z::Z::Canvas.order())
        .fixed_pos(rect.left_top() + egui::vec2(space::SM, space::SM))
        .show(&ctx, |ui| {
            let th = ddc_ui::theme::of(ui.ctx());
            frame(v.dark).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = space::XS;
                    for s in VisualStyle::ALL {
                        let tip = v
                            .nav
                            .t(&format!("v3d.style.tip.{}", s.key()))
                            .to_owned();
                        let icon = crate::gfx_ui::style_icon(s);
                        let on = v.nav.style == s;
                        let b = ddc_ui::widgets::icon_button(
                            ui,
                            &mut v.gfx.icons,
                            &th,
                            icon,
                            on,
                            &tip,
                        );
                        if b.clicked() {
                            v.nav.style = s;
                        }
                    }
                    ui.add_space(space::SM);
                    let [proj, views, graphics] = bar_items(v.camera.ortho, v.nav.vg_open);
                    let tip = v.nav.t(proj.tip).to_owned();
                    let b = ddc_ui::widgets::icon_button(
                        ui,
                        &mut v.gfx.icons,
                        &th,
                        proj.icon,
                        proj.on,
                        &tip,
                    );
                    if b.clicked() {
                        set_ortho(v, !v.camera.ortho);
                    }
                    let tip = v.nav.t(views.tip).to_owned();
                    let b = ddc_ui::widgets::icon_button(
                        ui,
                        &mut v.gfx.icons,
                        &th,
                        views.icon,
                        views.on,
                        &tip,
                    );
                    views_menu(v, &b, &th);
                    let tip = v.nav.t(graphics.tip).to_owned();
                    let b = ddc_ui::widgets::icon_button(
                        ui,
                        &mut v.gfx.icons,
                        &th,
                        graphics.icon,
                        graphics.on,
                        &tip,
                    );
                    if b.clicked() {
                        v.nav.vg_open = !v.nav.vg_open;
                    }
                });
            });
        });
    rects.push(r.response.rect);
    // the edit HUD stacks below the view bar in the same corner
    crate::quick::hud::reserve_corner(&ctx, r.response.rect);

    if v.section_box.is_some() {
        let r = egui::Area::new(id.with("v3d_box"))
            .order(ddc_ui::z::Z::Canvas.order())
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(rect.left_bottom() + egui::vec2(8.0, -8.0))
            .show(&ctx, |ui| {
                frame(v.dark).show(ui, |ui| box_panel(v, ui, scene, elevation))
            });
        rects.push(r.response.rect);
    }

    if let Some(w) = v.nav.walk {
        let key = if w.fly { "v3d.fly.hud" } else { "v3d.walk.hud" };
        let speed = v.nav.walk_speed(scene.bounds.as_ref());
        let ru = v.nav.labels_lang == Some(ddc_ui::Lang::Ru);
        let (num, unit_key) = walk_text(speed, v.nav.units, ru);
        let text = format!("{} · {}", v.nav.t(key), v.nav.t("v3d.walk.speed"));
        let unit = v.nav.t(unit_key).to_owned();
        let r = egui::Area::new(id.with("v3d_walk"))
            .order(ddc_ui::z::Z::Canvas.order())
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(rect.center_bottom() + egui::vec2(0.0, -space::MD))
            .interactable(false)
            .show(&ctx, |ui| {
                frame(v.dark).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::XS;
                        ui.label(text);
                        ui.strong(num);
                        ui.weak(unit);
                    })
                })
            });
        rects.push(r.response.rect);
        // crosshair
        let c = rect.center();
        let col = crate::overlay::pal(v.dark).selection;
        let p = ui.painter_at(rect);
        p.line_segment(
            [c - egui::vec2(6.0, 0.0), c + egui::vec2(6.0, 0.0)],
            Stroke::new(1.2, col),
        );
        p.line_segment(
            [c - egui::vec2(0.0, 6.0), c + egui::vec2(0.0, 6.0)],
            Stroke::new(1.2, col),
        );
    }

    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if esc_clears_temp(
        v.nav.temp.is_some(),
        v.nav.walk.is_some(),
        esc,
        crate::ui_kit::canvas_owns_keys(&ctx),
    ) {
        v.nav.temp = None;
    }
    if let Some(t) = v.nav.temp.clone() {
        let (key, set) = match &t {
            TempCats::Hide(s) => ("v3d.temp.hide", s),
            TempCats::Isolate(s) => ("v3d.temp.isolate", s),
        };
        let names: Vec<String> = set
            .iter()
            .map(|c| v.nav.t(&format!("v3d.cat.{c}")).to_owned())
            .collect();
        let text = format!("{}: {}", v.nav.t(key), names.join(", "));
        let show_all = v.nav.t("v3d.temp.show_all").to_owned();
        let r = egui::Area::new(id.with("v3d_temp"))
            .order(ddc_ui::z::Z::Canvas.order())
            .pivot(egui::Align2::CENTER_TOP)
            .fixed_pos(rect.center_top() + egui::vec2(0.0, space::SM))
            .show(&ctx, |ui| {
                frame(v.dark).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(text);
                        if ddc_ui::widgets::close_button(ui, &show_all).clicked() {
                            v.nav.temp = None;
                        }
                    })
                })
            });
        rects.push(r.response.rect);
    }

    if v.nav.vg_open {
        let first = rect.left_top() + egui::vec2(8.0, 48.0);
        rects.extend(vg_window(&ctx, v, id.with("v3d_vg"), first));
    }

    let empty = super::empty::show(v, ui, rect, scene);
    rects.extend(empty);

    // navigation hint: until the first action in this view
    if !v.nav.hint_used && empty.is_none() {
        let acted = ui.rect_contains_pointer(rect)
            && ui.input(|i| i.pointer.any_pressed() || i.smooth_scroll_delta.y != 0.0);
        v.nav.hint_used = acted;
        if !acted && v.nav.walk.is_none() {
            let text = v.nav.t("v3d.hint.nav").to_owned();
            egui::Area::new(id.with("v3d_hint"))
                .order(ddc_ui::z::Z::Canvas.order())
                .pivot(egui::Align2::CENTER_BOTTOM)
                .fixed_pos(rect.center_bottom() + egui::vec2(0.0, -space::MD))
                .interactable(false)
                .show(&ctx, |ui| frame(v.dark).show(ui, |ui| ui.label(text)));
        }
    }

    // orbit pivot
    if let Some(pv) = v.nav.pivot {
        let n = v.camera.project(pv, aspect);
        if n.z > 0.0 && n.z < 1.0 {
            let s = Pos2::new(
                rect.min.x + (n.x + 1.0) * 0.5 * rect.width(),
                rect.min.y + (1.0 - n.y) * 0.5 * rect.height(),
            );
            let ov = crate::overlay::pal(v.dark);
            let p = ui.painter_at(rect);
            p.circle(s, 4.0, ov.plate, Stroke::new(1.6, ov.selection));
        }
    }

    let hover = ctx.pointer_hover_pos();
    hover.is_some_and(|h| rects.iter().any(|r| r.contains(h)))
}

/// Saved-views dropdown under the bar's views button: save field, then one row per view
/// (open, quiet Update, quiet remove). Delete is undoable through the status toast.
fn views_menu(v: &mut Viewport, anchor: &egui::Response, th: &ddc_ui::Theme) {
    egui::Popup::menu(anchor).gap(space::XS).show(|ui| views_body(v, ui, th));
}

fn views_body(v: &mut Viewport, ui: &mut egui::Ui, th: &ddc_ui::Theme) {
    let hint = v.nav.t("v3d.views.name").to_owned();
    let save = v.nav.t("v3d.views.save").to_owned();
    let none = v.nav.t("v3d.views.none").to_owned();
    let (upd, del) = (
        v.nav.t("v3d.views.update").to_owned(),
        v.nav.t("v3d.views.delete").to_owned(),
    );
    {
        ui.spacing_mut().item_spacing.y = space::XXS;
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut v.nav.save_name)
                    .hint_text(hint)
                    .desired_width(th.d().control * 6.0),
            );
            let ok = !v.nav.save_name.trim().is_empty();
            let b = ui
                .add_enabled_ui(ok, |ui| {
                    ddc_ui::widgets::Button::secondary(&save)
                        .small()
                        .show(ui, th)
                })
                .inner;
            if b.clicked() {
                let n = std::mem::take(&mut v.nav.save_name);
                v.nav.pending.push(Action::Save(n.trim().to_string()));
                ui.close();
            }
        });
        ddc_ui::widgets::menu_divider(ui, th);
        if v.nav.saved.is_empty() {
            ui.weak(none);
        }
        ui.set_min_width(th.d().control * 11.0);
        for (vid, name) in v.nav.saved.clone() {
            ui.horizontal(|ui| {
                let cur = v.nav.current == Some(vid);
                let c = th.d().control;
                let name_w = (ui.available_width() - c * 4.5).max(c * 4.0);
                if view_row(ui, th, cur, &name, name_w).clicked() {
                    v.nav.pending.push(Action::Restore(vid));
                    ui.close();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ddc_ui::widgets::close_button(ui, &del).clicked() {
                        v.nav.pending.push(Action::Delete(vid));
                        ui.close();
                    }
                    let b = ddc_ui::widgets::Button::quiet(&upd).small().show(ui, th);
                    if b.clicked() {
                        v.nav.pending.push(Action::Overwrite(vid));
                        ui.close();
                    }
                });
            });
        }
    }
}

/// One saved-view row: name elided to `w` (full name on hover), accent fill when current.
fn view_row(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    cur: bool,
    name: &str,
    w: f32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, th.d().row), egui::Sense::click());
    let on = ui.is_enabled();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, on, cur, name));
    let s = th.sem();
    if cur {
        ui.painter().rect_filled(rect, th.radius("sm"), s.selection_bg);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, th.radius("sm"), th.hover_fill());
    }
    let role = if cur { ddc_ui::theme::Role::BodyStrong } else { ddc_ui::theme::Role::Body };
    let inner = rect.shrink2(egui::vec2(space::SM, 0.0));
    let cut = ddc_ui::widgets::text::paint_in(
        ui,
        inner,
        name,
        th.font(role),
        s.text_primary,
        egui::Align::Min,
    );
    if cut {
        resp.on_hover_text(name)
    } else {
        resp
    }
}

/// Section-box bounds as numbers (z are absolute elevations), fill, fit to selection, turn off.
fn box_panel(v: &mut Viewport, ui: &mut egui::Ui, scene: &Scene, elevation: f64) {
    let Some(mut b) = v.section_box else {
        return;
    };
    let u = v.nav.units;
    let lim = 1.0e5;
    let th = ddc_ui::theme::of(ui.ctx());
    let ctl = th.d().control;
    ddc_ui::widgets::shell_kit::section_title(ui, &th, &v.nav.t("v3d.box.title").to_owned());
    egui::Grid::new("v3d_box_grid")
        .num_columns(4)
        .spacing([space::SM, space::XXS])
        .show(ui, |ui| {
            for (key, hi) in [("v3d.box.min", false), ("v3d.box.max", true)] {
                ui.label(v.nav.t(key).to_owned());
                for k in 0..3 {
                    let p = if hi { &mut b.max } else { &mut b.min };
                    let off = if k == 2 { elevation } else { 0.0 };
                    let mut x = p[k] as f64 + off;
                    let drag = length_drag(&mut x, u, lim).range(-lim..=lim);
                    let r = ui.add_sized([ctl * 3.0, ctl], drag);
                    if r.changed() {
                        p[k] = (x - off) as f32;
                    }
                }
                ui.end_row();
            }
        });
    for k in 0..3 {
        if b.max[k] < b.min[k] + 0.05 {
            b.max[k] = b.min[k] + 0.05;
        }
    }
    v.section_box = Some(b);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        let caps = v.nav.t("v3d.box.caps").to_owned();
        ddc_ui::widgets::checkbox(ui, &th, &mut v.nav.caps, &caps);
        let sel_tip = v.nav.t("v3d.box.sel").to_owned();
        let pick = ddc_ui::widgets::icon_button(
            ui,
            &mut v.gfx.icons,
            &th,
            "select",
            false,
            &sel_tip,
        );
        if pick.clicked() {
            let sel: Option<Aabb> = v.highlight.and_then(|r| scene.bounds_of(r));
            if let Some(s) = sel.or(scene.bounds) {
                let pad = ddc_render::glam::Vec3::splat(0.3);
                v.section_box = Some(Aabb {
                    min: s.min - pad,
                    max: s.max + pad,
                });
                v.nav.level_view = None;
            }
        }
        if let Some((_, cut)) = v.nav.level_view {
            let mut c = cut;
            ui.label(v.nav.t("v3d.box.level").to_owned());
            if ui
                .add_sized([ctl * 3.0, ctl], length_drag(&mut c, u, 100.0))
                .changed()
            {
                c = c.max(0.05);
                if let Some(bx) = &mut v.section_box {
                    bx.max.z = c as f32;
                }
                v.nav.level_view = v.nav.level_view.map(|(l, _)| (l, c));
            }
        }
        let off_tip = v.nav.t("v3d.box.off").to_owned();
        let off = ddc_ui::widgets::icon_button_danger(
            ui,
            &mut v.gfx.icons,
            &th,
            "delete",
            &off_tip,
        );
        if off.clicked() {
            v.section_box = None;
            v.nav.level_view = None;
        }
    });
}

/// The category visibility window of the 3D view; closing it clears `vg_open`.
fn vg_window(
    ctx: &egui::Context,
    v: &mut Viewport,
    id: egui::Id,
    first_pos: Pos2,
) -> Option<egui::Rect> {
    let mut open = true;
    let r = kit::Dialog::new(
        "view3d_graphics",
        v.nav.t("v3d.vg.title").to_owned(),
        kit::DialogSize::Hud,
    )
    .id(id)
    .first_pos(first_pos)
    .open(&mut open)
    .show(ctx, |ui| vg_panel(v, ui));
    v.nav.vg_open = open;
    r.map(|r| r.response.rect)
}

/// The window drawn headless with the labels of `cat`: the layout probe.
#[cfg(test)]
pub(crate) fn layout_probe(cat: &ddc_ui::Catalog) -> crate::layout_contract::Probe {
    let mut v = Viewport::new();
    v.nav.labels = super::Labels::load(cat);
    v.nav.vg_open = true;
    // two categories carry a rule so that the active slider row is drawn too
    v.nav.rules.insert(CATS[0].to_string(), CatRule { visible: true, transparency: 40 });
    let id = egui::Id::new("layout.v3d_vg");
    crate::layout_contract::probe_id(id, |ctx| {
        vg_window(ctx, &mut v, id, Pos2::new(40.0, 60.0));
    })
}

/// Which 3D panel `panel_probe` draws.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum PanelKind {
    Box,
    Views,
    Graphics,
}

/// One panel of the 3D view drawn headless in light or dark; `views` are the saved views.
#[cfg(test)]
pub(crate) fn panel_probe(
    cat: &ddc_ui::Catalog,
    kind: PanelKind,
    light: bool,
    views: &[&str],
) -> crate::layout_contract::Probe {
    use ddc_ui::{Density, Mode, Theme};
    let mut v = Viewport::new();
    v.nav.labels = super::Labels::load(cat);
    v.dark = !light;
    v.section_box = Some(Aabb {
        min: ddc_render::glam::Vec3::ZERO,
        max: ddc_render::glam::Vec3::splat(5.0),
    });
    v.nav.level_view = Some((1, 3.0));
    v.nav.saved = views.iter().enumerate().map(|(i, n)| (i as i64 + 1, n.to_string())).collect();
    v.nav.current = Some(1);
    for c in CATS {
        v.nav.rules.insert(c.to_string(), CatRule { visible: false, transparency: 30 });
    }
    let scene = Scene::default();
    let (ctx, _) = crate::layout_contract::driven_ctx();
    let mode = if light { Mode::Light } else { Mode::Dark };
    let mut th = Theme::new(mode, Density::Compact);
    let id = egui::Id::new("layout.v3d_panel");
    let mut last = Default::default();
    for _ in 0..4 {
        last = crate::layout_contract::pass(&ctx, &mut th, id, vec![], &mut |ctx| {
            egui::Area::new(id).fixed_pos(Pos2::new(40.0, 60.0)).show(ctx, |ui| {
                let t = ddc_ui::theme::of(ui.ctx());
                match kind {
                    // on its plate, as the view draws it
                    PanelKind::Box => {
                        let dark = v.dark;
                        frame(dark).show(ui, |ui| box_panel(&mut v, ui, &scene, 0.0));
                    }
                    PanelKind::Views => views_body(&mut v, ui, &t),
                    PanelKind::Graphics => vg_panel(&mut v, ui),
                }
            });
        });
    }
    // the box is a bare area on a plate, not a window: its room is the plate less its margin
    if kind == PanelKind::Box {
        last.inner = egui::AreaState::load(&ctx, id).map(|a| a.rect().shrink(space::XS));
    }
    last
}

/// Category visibility and transparency in this 3D view.
fn vg_panel(v: &mut Viewport, ui: &mut egui::Ui) {
    let (vis, tr) = (
        v.nav.t("v3d.vg.visible").to_owned(),
        v.nav.t("v3d.vg.transparency").to_owned(),
    );
    let th = ddc_ui::theme::of(ui.ctx());
    let ctl = th.d().control;
    // a grid cell offers the width its column had last frame and the checkbox shrinks to it,
    // so the column starts as wide as the longest category name, or that name stays cut
    let font = th.font(ddc_ui::theme::Role::Body);
    let widest = CATS
        .iter()
        .map(|c| {
            let name = v.nav.t(&format!("v3d.cat.{c}")).to_owned();
            ui.painter().layout_no_wrap(name, font.clone(), egui::Color32::PLACEHOLDER).size().x
        })
        .fold(0.0f32, f32::max);
    let check_w = ddc_ui::theme::control::CHECK + space::SM + widest.ceil();
    egui::Grid::new("v3d_vg_grid")
        .num_columns(2)
        .spacing([space::MD, space::XXS])
        .min_col_width((ctl * 5.0).max(check_w))
        .show(ui, |ui| {
            ddc_ui::widgets::shell_kit::section_title(ui, &th, &vis);
            ddc_ui::widgets::shell_kit::section_title(ui, &th, &tr);
            ui.end_row();
            for c in CATS {
                let name = v.nav.t(&format!("v3d.cat.{c}")).to_owned();
                let mut r = v.nav.rules.get(c).copied().unwrap_or_default();
                // the category name is the checkbox label, so it carries an accessible name
                ddc_ui::widgets::checkbox(ui, &th, &mut r.visible, &name);
                let mut t = r.transparency as u32;
                ui.add_enabled(r.visible, egui::Slider::new(&mut t, 0..=90).suffix(" %"));
                r.transparency = t as u8;
                if r == CatRule::default() {
                    v.nav.rules.remove(c);
                } else {
                    v.nav.rules.insert(c.to_string(), r);
                }
                ui.end_row();
            }
        });
    ui.add_space(space::XS);
    let reset = v.nav.t("v3d.vg.reset").to_owned();
    if ddc_ui::widgets::Button::secondary(&reset).small().show(ui, &th).clicked() {
        v.nav.rules.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::Units;
    use crate::view3d::{Labels, LABEL_KEYS};

    const TIPS: [&str; 5] = [
        "v3d.proj.to_ortho",
        "v3d.proj.to_persp",
        "v3d.views",
        "v3d.vg.title",
        "v3d.temp.show_all",
    ];

    #[test]
    fn panels_use_kit_widgets_only() {
        let src = include_str!("overlay.rs");
        let prod = src.split("#[cfg(test)]\nmod tests").next().unwrap_or(src);
        let raws = [".checkbox(", ".small_button(", ".selectable_label(", ".button("];
        for raw in raws {
            assert!(!prod.contains(raw), "raw widget {raw} in view3d/overlay.rs");
        }
    }

    #[test]
    fn panels_pass_the_layout_contract_in_both_themes_and_languages() {
        use super::{panel_probe, PanelKind};
        let long = "A very long saved view name that must be elided and never push the buttons out";
        let ru = "\u{41e}\u{447}\u{435}\u{43d}\u{44c} \u{434}\u{43b}\u{438}\u{43d}\u{43d}\u{43e}\
                  \u{435} \u{438}\u{43c}\u{44f} \u{441}\u{43e}\u{445}\u{440}\u{430}\u{43d}\u{451}\
                  \u{43d}\u{43d}\u{43e}\u{433}\u{43e} \u{432}\u{438}\u{434}\u{430} \u{434}\u{43b}\
                  \u{44f} \u{43f}\u{440}\u{43e}\u{432}\u{435}\u{440}\u{43a}\u{438}";
        let views = [long, ru, "N", "Plan"];
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            for kind in [PanelKind::Box, PanelKind::Views, PanelKind::Graphics] {
                for light in [false, true] {
                    let p = panel_probe(&cat, kind, light, &views);
                    let tag = format!("{kind:?} {lang:?} light={light}");
                    crate::layout_contract::assert_contract(&tag, &p, false);
                    assert!(!p.controls.is_empty(), "{tag}: nothing drawn");
                    if kind == PanelKind::Graphics {
                        let boxes = p
                            .controls
                            .iter()
                            .filter(|c| c.role == egui::accesskit::Role::CheckBox)
                            .count();
                        assert_eq!(boxes, CATS.len(), "{tag}: one named checkbox per category");
                        assert!(p
                            .controls
                            .iter()
                            .filter(|c| c.role == egui::accesskit::Role::CheckBox)
                            .all(|c| !c.label.is_empty()), "{tag}: unnamed checkbox");
                    }
                    if kind == PanelKind::Views {
                        assert!(p.elided.iter().any(|t| t == long), "{tag}: long name not elided");
                    }
                }
            }
        }
    }

    #[test]
    fn projection_item_follows_camera_and_graphics_follows_panel() {
        let [p, vw, g] = bar_items(false, false);
        assert!(!p.on && !g.on && !vw.on);
        assert_eq!(p.tip, "v3d.proj.to_ortho");
        let [p, _, g] = bar_items(true, true);
        assert!(p.on && g.on);
        assert_eq!(p.tip, "v3d.proj.to_persp");
        assert_ne!(bar_items(true, false)[0].icon, bar_items(false, false)[0].icon);
    }

    #[test]
    fn bar_items_use_embedded_icons_and_distinct_glyphs() {
        for (o, g) in [(false, false), (true, true)] {
            let items = bar_items(o, g);
            for it in items {
                assert!(ddc_ui::icons::source(it.icon).is_some(), "{}", it.icon);
            }
            assert!(items[0].icon != items[1].icon && items[1].icon != items[2].icon);
        }
    }

    #[test]
    fn tips_are_nonempty_in_both_languages() {
        for t in TIPS {
            assert!(LABEL_KEYS.contains(&t), "{t} missing from LABEL_KEYS");
        }
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let l = Labels::load(&ddc_ui::Catalog::new(lang));
            for o in [false, true] {
                for it in bar_items(o, false) {
                    assert_ne!(l.get(it.tip), it.tip, "{lang:?} {}", it.tip);
                    assert!(!l.get(it.tip).trim().is_empty());
                }
            }
            for k in ["v3d.temp.show_all", "v3d.walk.unit.m", "v3d.walk.unit.ft"] {
                assert_ne!(l.get(k), k, "{lang:?} {k}");
            }
            assert!(l.get("v3d.temp.show_all").contains("Esc"));
        }
    }

    #[test]
    fn walk_text_metric_has_unit_key_and_one_decimal() {
        let (n, k) = walk_text(1.5, Units::Metric, false);
        assert_eq!((n.as_str(), k), ("1.5", "v3d.walk.unit.m"));
    }

    #[test]
    fn walk_text_imperial_converts_and_uses_feet_key() {
        let (n, k) = walk_text(3.048, Units::Imperial, false);
        assert_eq!((n.as_str(), k), ("10.0", "v3d.walk.unit.ft"));
    }

    #[test]
    fn walk_text_russian_uses_decimal_comma() {
        let (n, _) = walk_text(1.5, Units::Metric, true);
        assert_eq!(n, "1,5");
    }

    #[test]
    fn walk_unit_labels_differ_per_language() {
        let en = Labels::load(&ddc_ui::Catalog::new(ddc_ui::Lang::En));
        let ru = Labels::load(&ddc_ui::Catalog::new(ddc_ui::Lang::Ru));
        for k in ["v3d.walk.unit.m", "v3d.walk.unit.ft"] {
            assert_ne!(en.get(k), ru.get(k), "{k}");
        }
    }

    #[test]
    fn deleted_view_message_names_the_view_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let c = ddc_ui::Catalog::new(lang);
            let s = c.tf("v3d.msg.deleted_named", &[("name", "North")]);
            assert!(s.contains("North") && !s.contains('{'), "{lang:?}: {s}");
        }
    }

    #[test]
    fn esc_drops_temp_only_when_canvas_owns_keys_and_not_walking() {
        assert!(esc_clears_temp(true, false, true, true));
        assert!(!esc_clears_temp(false, false, true, true));
        assert!(!esc_clears_temp(true, true, true, true));
        assert!(!esc_clears_temp(true, false, false, true));
        assert!(!esc_clears_temp(true, false, true, false));
    }
}
