// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Interface mode, chosen on the first start and in Settings: Ribbon (the default), Rail (a
//! floating rail with section flyouts over the canvas) or Minimal (a dock of creation tools).
//! Rail and Minimal add the context bar over a selection, and Ctrl+K works in every mode. One
//! registry feeds every surface: rail buttons are surface T, the context bar surface C, flyout
//! tiles the ribbon groups (`rgroup.*`) of each section's tabs.
use super::EditorApp;
use ddc_ui::widgets::{self, ribbon};
use ddc_ui::{theme::Role, Registry};
use egui::{pos2, vec2, Align2, Rect, Sense, Stroke, StrokeKind, Vec2};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UiMode {
    #[default]
    Ribbon,
    Rail,
    Minimal,
}

impl UiMode {
    pub const ALL: [UiMode; 3] = [UiMode::Ribbon, UiMode::Rail, UiMode::Minimal];
    pub fn key(self) -> &'static str {
        match self {
            UiMode::Ribbon => "ribbon",
            UiMode::Rail => "rail",
            UiMode::Minimal => "minimal",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.key() == s.trim())
    }
    pub fn of(p: &crate::prefs::Prefs) -> Self {
        Self::parse(&p.ui_mode).unwrap_or_default()
    }
    pub fn label_key(self) -> String {
        format!("ui_mode.{}", self.key())
    }
    pub fn desc_key(self) -> String {
        format!("ui_mode.{}.desc", self.key())
    }
}

/// Geometry, points: mirrors the `rail`, `ctxbar` and `dock` groups of docs/design/tokens.json
/// (a test holds them equal and on the 4 pt grid). Plates are `radius.md` inside capsules of
/// `radius.lg` = md + inset: concentric corners.
pub mod k {
    pub const RAIL_W: f32 = 44.0;
    pub const RAIL_W_EXPANDED: f32 = 200.0;
    pub const BUTTON: f32 = 36.0;
    pub const ICON: f32 = 20.0;
    pub const INSET: f32 = 4.0;
    pub const GAP: f32 = 8.0;
    pub const EDGE: f32 = 12.0;
    pub const FLYOUT_W: f32 = 300.0;
    pub const TILE_W: f32 = 88.0;
    pub const TILE_H: f32 = 56.0;
    /// Label budgets: expanded rail, flyout tile.
    pub const LABEL_MAX: f32 = 140.0;
    pub const TILE_LABEL_MAX: f32 = 84.0;
    pub const CTXBAR_H: f32 = 44.0;
    pub const CTXBAR_OFFSET: f32 = 8.0;
    pub const CTXBAR_CMDS: usize = 4;
    pub const DOCK_BTN: f32 = 36.0;
    pub const DOCK_GAP: f32 = 4.0;
    pub const DOCK_TOOLS: usize = 8;
}

/// A rail section: its glyph and the ribbon tabs its flyout gathers.
pub struct Section {
    pub key: &'static str,
    pub icon: &'static str,
    pub tabs: &'static [&'static str],
}

pub const SECTIONS: [Section; 4] = [
    Section { key: "model", icon: "wall", tabs: &["arch", "struct", "mass"] },
    Section { key: "systems", icon: "duct", tabs: &["systems"] },
    Section { key: "document", icon: "dimension", tabs: &["annotate", "view"] },
    Section { key: "analyse", icon: "table", tabs: &["data", "manage"] },
];

/// Removes the selection; last on the context bar, after a rule, in the danger colour.
pub const DELETE_CMD: &str = "mod.delete";

fn tool(c: &ddc_ui::Command) -> bool {
    c.on("R") && !c.launcher && !c.icon.is_empty() && c.alias.is_none()
}

/// Flyout of `s`: (ribbon group, command ids), groups and commands in registry order.
pub fn flyout(reg: &Registry, s: &Section) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for c in reg.all().iter().filter(|c| tool(c) && c.tab.as_deref().is_some_and(|t| s.tabs.contains(&t))) {
        let g = ribbon::group_name(c);
        match out.iter_mut().find(|(n, _)| n == g) {
            Some((_, v)) => v.push(c.id.clone()),
            None => out.push((g.to_string(), vec![c.id.clone()])),
        }
    }
    out
}

/// The rail's own buttons (surface T), registry order.
pub fn rail_cmds(reg: &Registry) -> Vec<String> {
    reg.all().iter().filter(|c| c.on("T")).map(|c| c.id.clone()).collect()
}

/// The dock: the creation tools of the model and document tabs, priority 1 first.
pub fn dock_tools(reg: &Registry) -> Vec<String> {
    let tabs = ["arch", "struct", "annotate"];
    let mut v: Vec<&ddc_ui::Command> =
        reg.all().iter().filter(|c| tool(c) && c.tab.as_deref().is_some_and(|t| tabs.contains(&t))).collect();
    v.sort_by_key(|c| c.priority.unwrap_or(5));
    v.into_iter().take(k::DOCK_TOOLS).map(|c| c.id.clone()).collect()
}

/// The context bar of a selection of `category`: its first commands on surface C.
pub fn ctx_cmds(reg: &Registry, category: &str) -> Vec<String> {
    let mut v: Vec<String> = reg
        .contextual(category)
        .into_iter()
        .filter(|c| c.on("C") && c.id != DELETE_CMD && !c.icon.is_empty())
        .map(|c| c.id.clone())
        .collect();
    v.dedup();
    v.truncate(k::CTXBAR_CMDS);
    v
}

/// Rail state for the session: open flyout, label width, pin, last tool of each section.
#[derive(Default)]
pub struct RailState {
    pub open: Option<usize>,
    pub expanded: bool,
    pub pinned: bool,
    pub last: [Option<String>; 4],
}

/// One rail/dock/flyout button: plate by state, glyph, tooltip with mnemonic; true on click.
#[allow(clippy::too_many_arguments)]
fn btn(
    ui: &mut egui::Ui,
    app: &mut EditorApp,
    r: Rect,
    id: egui::Id,
    icon: &str,
    label: &str,
    keys: &str,
    on: bool,
    enabled: bool,
) -> bool {
    let resp = ui.interact(r, id, if enabled { Sense::click() } else { Sense::hover() });
    let l = label.to_string();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, on, &l));
    let th = &app.sh.theme;
    if on {
        ui.painter().rect_filled(r, th.radius("md"), th.c("accent.subtle"));
    }
    let fg = widgets::paint_state(ui, th, r, &resp, false);
    let ink = if !enabled {
        ddc_ui::icons::tint(th, ddc_ui::icons::Tint::Disabled)
    } else if on {
        th.c("accent")
    } else {
        fg
    };
    let name = if on { format!("{icon}.fill") } else { icon.to_string() };
    let ic = Rect::from_center_size(pos2(r.left() + k::BUTTON / 2.0, r.center().y), Vec2::splat(k::ICON));
    app.sh.icons.paint(ui, &name, k::ICON as u32, ic, ink);
    widgets::rich_tooltip(resp, th, label, "", keys).clicked() && enabled
}

/// `p` on the physical pixel grid, so 1 px outlines stay crisp at 1x, 1.25x, 1.5x and 2x.
fn snap(ctx: &egui::Context, p: egui::Pos2) -> egui::Pos2 {
    let ppp = ctx.pixels_per_point();
    pos2((p.x * ppp).round() / ppp, (p.y * ppp).round() / ppp)
}

/// A floating capsule: canvas-chip fill, overlay outline in both themes, level-2 shadow.
fn capsule(th: &ddc_ui::Theme) -> egui::Frame {
    egui::Frame::new()
        .fill(th.c("bg.canvas_chip"))
        .stroke(Stroke::new(1.0_f32, th.c("border.overlay")))
        .corner_radius(th.radius("lg") as u8)
        .shadow(th.shadow(2))
        .inner_margin(egui::Margin::same(k::INSET as i8))
}

/// Width of the update pill (`update::badge`), 0 while no newer version is pending.
pub(super) fn badge_w(ui: &egui::Ui, th: &ddc_ui::Theme, cat: &ddc_ui::Catalog) -> f32 {
    crate::update::pending_version().map_or(0.0, |v| {
        let t = cat.tf("update_check.badge", &[("v", v.as_str())]);
        let g = ui.painter().layout_no_wrap(t, th.font(Role::Label), egui::Color32::PLACEHOLDER);
        crate::update::pill::width(g.size().x)
    })
}

/// Draws the update pill vertically centred in `r` (the room a host kept for it).
pub(super) fn badge_in(ui: &mut egui::Ui, th: &ddc_ui::Theme, cat: &ddc_ui::Catalog, r: Rect) {
    let lay = egui::Layout::left_to_right(egui::Align::Center);
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(lay));
    crate::update::badge(&mut c, th, cat);
}

impl EditorApp {
    pub fn ui_mode(&self) -> UiMode {
        UiMode::of(&self.prefs)
    }

    /// Switches the interface mode and saves it.
    pub fn set_ui_mode(&mut self, m: UiMode) {
        self.prefs.ui_mode = m.key().into();
        self.chrome.rail.open = None;
        self.prefs.save();
    }

    fn cmd_enabled(&self, id: &str) -> bool {
        self.sh.reg.get(id).is_some() && self.sh.ribbon.enabled(&self.sh.reg, id)
    }

    fn cmd_text(&self, id: &str) -> (String, String, String) {
        let c = &self.sh.cat;
        match self.sh.reg.get(id) {
            Some(cmd) => (cmd.icon.clone(), c.t(&cmd.label_key()), cmd.mnemonic.clone().unwrap_or_default()),
            None => (String::new(), id.to_string(), String::new()),
        }
    }

    /// The Rail or Minimal surfaces over the canvas `area`; nothing in Ribbon mode.
    pub(super) fn mode_surfaces(&mut self, ctx: &egui::Context, area: Rect, fire: &mut Vec<String>) {
        let mode = self.ui_mode();
        if mode == UiMode::Ribbon || !area.is_positive() || self.chrome_clean() {
            return;
        }
        match mode {
            UiMode::Rail => self.rail(ctx, area, fire),
            _ => self.dock(ctx, area, fire),
        }
        self.context_bar(ctx, area, fire);
        self.mode_badge(ctx, area);
    }

    /// The update pill at the canvas' top end corner, opposite the rail.
    fn mode_badge(&self, ctx: &egui::Context, area: Rect) {
        if crate::update::pending_version().is_none() {
            return;
        }
        let (th, cat) = (&self.sh.theme, &self.sh.cat);
        let h = ddc_ui::theme::ribbon::TAB_H;
        let rtl = ddc_ui::i18n::rtl();
        let (x, pivot) = if rtl {
            (area.left() + k::EDGE, Align2::LEFT_TOP)
        } else {
            (area.right() - k::EDGE, Align2::RIGHT_TOP)
        };
        egui::Area::new(egui::Id::new("ddc.mode.badge"))
            .order(egui::Order::Middle)
            .pivot(pivot)
            .fixed_pos(snap(ctx, pos2(x, area.top() + k::EDGE)))
            .show(ctx, |ui| {
                let w = badge_w(ui, th, cat);
                let (r, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
                badge_in(ui, th, cat, r);
            });
    }

    fn rail(&mut self, ctx: &egui::Context, area: Rect, fire: &mut Vec<String>) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Backslash)) {
            self.chrome.rail.expanded = !self.chrome.rail.expanded;
        }
        let rtl = ddc_ui::i18n::rtl();
        let expanded = self.chrome.rail.expanded;
        let w = if expanded { k::RAIL_W_EXPANDED } else { k::RAIL_W } - 2.0 * k::INSET;
        let x = if rtl { area.right() - k::EDGE - w - 2.0 * k::INSET } else { area.left() + k::EDGE };
        let th = self.sh.theme.clone();
        let own = rail_cmds(&self.sh.reg);
        let mut picked: Option<String> = None;
        let mut toggle: Option<usize> = None;
        let shown = egui::Area::new(egui::Id::new("ddc.rail"))
            .order(egui::Order::Middle)
            .fixed_pos(snap(ctx, pos2(x, area.top() + k::EDGE)))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, k::GAP);
                let row = |ui: &mut egui::Ui| ui.allocate_exact_size(vec2(w, k::BUTTON), Sense::hover()).0;
                capsule(&th).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for id in &own {
                        let r = row(ui);
                        let (icon, label, keys) = self.cmd_text(id);
                        let en = self.cmd_enabled(id);
                        if btn(ui, self, r, egui::Id::new(("rail.t", id.as_str())), &icon, &label, &keys, false, en) {
                            picked = Some(id.clone());
                        }
                        if expanded {
                            self.rail_label(ui, r, &label);
                        }
                    }
                });
                capsule(&th).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (i, s) in SECTIONS.iter().enumerate() {
                        let r = row(ui);
                        let label = self.sh.cat.t(&format!("rail.{}", s.key));
                        let on = self.chrome.rail.open == Some(i);
                        // the section shows the last tool taken from it, like a split button
                        let icon = self.chrome.rail.last[i]
                            .as_deref()
                            .and_then(|id| self.sh.reg.get(id))
                            .map_or(s.icon.to_string(), |c| c.icon.clone());
                        if btn(ui, self, r, egui::Id::new(("rail.s", i)), &icon, &label, "", on, true) {
                            toggle = Some(i);
                        }
                        if expanded {
                            self.rail_label(ui, r, &label);
                        }
                    }
                });
            });
        let rail_rect = shown.response.rect;
        if let Some(i) = toggle {
            let st = &mut self.chrome.rail;
            st.open = if st.open == Some(i) { None } else { Some(i) };
        }
        if let Some(i) = self.chrome.rail.open {
            let fx = if rtl { rail_rect.left() - k::GAP - k::FLYOUT_W } else { rail_rect.right() + k::GAP };
            if let Some(id) = self.flyout_panel(ctx, i, pos2(fx, rail_rect.top()), rail_rect) {
                self.chrome.rail.last[i] = Some(id.clone());
                if !self.chrome.rail.pinned {
                    self.chrome.rail.open = None;
                }
                fire.push(id);
            }
        }
        if let Some(id) = picked {
            fire.push(id);
        }
    }

    fn rail_label(&self, ui: &egui::Ui, r: Rect, label: &str) {
        let th = &self.sh.theme;
        let x = r.left() + k::BUTTON + k::INSET;
        let clip = Rect::from_min_max(pos2(x, r.top()), pos2((x + k::LABEL_MAX).min(r.right()), r.bottom()));
        ui.painter().with_clip_rect(clip).text(
            pos2(x, r.center().y),
            Align2::LEFT_CENTER,
            label,
            th.font(Role::Body),
            th.c("text.primary"),
        );
    }

    /// Tiles of section `i` by ribbon group; a pick returns the command. Esc or a press on the
    /// canvas closes it, the pin keeps it open as a palette.
    fn flyout_panel(&mut self, ctx: &egui::Context, i: usize, at: egui::Pos2, rail: Rect) -> Option<String> {
        let th = self.sh.theme.clone();
        let groups = flyout(&self.sh.reg, &SECTIONS[i]);
        let mut out = None;
        let shown = egui::Area::new(egui::Id::new("ddc.rail.flyout"))
            .order(egui::Order::Foreground)
            .fixed_pos(snap(ctx, at))
            .show(ctx, |ui| {
                capsule(&th).inner_margin(egui::Margin::same(k::GAP as i8)).show(ui, |ui| {
                    ui.set_width(k::FLYOUT_W - 2.0 * k::GAP);
                    ui.horizontal(|ui| {
                        let title = self.sh.cat.t(&format!("rail.{}", SECTIONS[i].key));
                        ui.label(th.rich(Role::BodyStrong, &title).color(th.c("text.primary")));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let tip = self.sh.cat.t("rail.pin");
                            let s = &mut self.sh;
                            let pin = self.chrome.rail.pinned;
                            if widgets::icon_button(ui, &mut s.icons, &s.theme, "pin", pin, &tip).clicked() {
                                self.chrome.rail.pinned = !pin;
                            }
                        });
                    });
                    egui::ScrollArea::vertical().max_height((ctx.screen_rect().height() - at.y - 48.0).max(160.0)).show(ui, |ui| {
                        for (g, ids) in &groups {
                            ui.add_space(k::INSET);
                            let title = self.sh.cat.t(&format!("rgroup.{g}"));
                            ui.label(th.rich(Role::Caption, &title).color(th.c("text.secondary")));
                            for chunk in ids.chunks(3) {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    for id in chunk {
                                        if let Some(id) = self.tile(ui, id) {
                                            out = Some(id);
                                        }
                                    }
                                });
                            }
                        }
                    });
                });
            });
        let r = shown.response.rect;
        let away = ctx.input(|inp| {
            inp.pointer.any_pressed()
                && inp.pointer.interact_pos().is_some_and(|p| !r.contains(p) && !rail.contains(p))
        });
        let esc = ctx.input_mut(|inp| inp.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc || (away && !self.chrome.rail.pinned) {
            self.chrome.rail.open = None;
        }
        out
    }

    /// One flyout tile: glyph over a one-line caption (budget `TILE_LABEL_MAX`), mnemonic chip.
    fn tile(&mut self, ui: &mut egui::Ui, id: &str) -> Option<String> {
        let (r, resp) = ui.allocate_exact_size(vec2(k::TILE_W, k::TILE_H), Sense::click());
        let (icon, label, keys) = self.cmd_text(id);
        let en = self.cmd_enabled(id);
        let l = label.clone();
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, en, &l));
        let th = &self.sh.theme;
        let on = self.active_command() == Some(id);
        if on {
            ui.painter().rect_filled(r, th.radius("md"), th.c("accent.subtle"));
        }
        let fg = widgets::paint_state(ui, th, r, &resp, false);
        let ink = if !en { ddc_ui::icons::tint(th, ddc_ui::icons::Tint::Disabled) } else if on { th.c("accent") } else { fg };
        let name = if on { format!("{icon}.fill") } else { icon };
        let ic = Rect::from_center_size(pos2(r.center().x, r.top() + 18.0), Vec2::splat(k::ICON));
        self.sh.icons.paint(ui, &name, k::ICON as u32, ic, ink);
        let p = ui.painter();
        let clip = Rect::from_center_size(pos2(r.center().x, r.bottom() - 14.0), vec2(k::TILE_LABEL_MAX, 16.0));
        let font = th.font(Role::RibbonLabel);
        let g = p.layout_no_wrap(label.clone(), font, th.c("text.primary"));
        let x = (clip.center().x - g.size().x / 2.0).max(clip.left());
        p.with_clip_rect(clip).galley(pos2(x, clip.center().y - g.size().y / 2.0), g, th.c("text.primary"));
        if !keys.is_empty() {
            p.text(r.right_top() + vec2(-4.0, 4.0), Align2::RIGHT_TOP, &keys, th.font(Role::Caption), th.c("text.secondary"));
        }
        (widgets::rich_tooltip(resp, th, &label, "", &keys).clicked() && en).then(|| id.to_string())
    }

    /// Minimal mode: the dock at the bottom centre: creation tools, then search and Ask AI.
    fn dock(&mut self, ctx: &egui::Context, area: Rect, fire: &mut Vec<String>) {
        let th = self.sh.theme.clone();
        let mut ids = dock_tools(&self.sh.reg);
        ids.extend([ribbon::PALETTE_CMD.to_string(), ribbon::ASK_AI_CMD.to_string()]);
        let n = ids.len() as f32;
        let w = n * k::DOCK_BTN + (n - 1.0) * k::DOCK_GAP + 2.0 * k::INSET;
        let at = pos2(area.center().x - w / 2.0, area.bottom() - k::EDGE - k::DOCK_BTN - 2.0 * k::INSET);
        let mut out = None;
        egui::Area::new(egui::Id::new("ddc.dock"))
            .order(egui::Order::Middle)
            .fixed_pos(snap(ctx, at))
            .show(ctx, |ui| {
                capsule(&th).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = k::DOCK_GAP;
                    ui.horizontal(|ui| {
                        for id in &ids {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(k::DOCK_BTN), Sense::hover());
                            let (icon, label, keys) = self.cmd_text(id);
                            let en = self.cmd_enabled(id);
                            let on = self.active_command() == Some(id.as_str());
                            if btn(ui, self, r, egui::Id::new(("dock", id.as_str())), &icon, &label, &keys, on, en) {
                                out = Some(id.clone());
                            }
                        }
                    });
                });
            });
        fire.extend(out);
    }

    /// Over a selection: its category's first commands, Ask AI, then delete after a rule.
    fn context_bar(&mut self, ctx: &egui::Context, area: Rect, fire: &mut Vec<String>) {
        if self.ed.selection.is_empty() {
            return;
        }
        let Some((cat_key, cat_label)) = self.sh.ribbon.context.clone() else {
            return;
        };
        let th = self.sh.theme.clone();
        let mut ids = ctx_cmds(&self.sh.reg, &cat_key);
        ids.push(ribbon::ASK_AI_CMD.to_string());
        let mut out = None;
        egui::Area::new(egui::Id::new("ddc.ctxbar"))
            .order(egui::Order::Middle)
            .pivot(Align2::CENTER_TOP)
            .fixed_pos(snap(ctx, pos2(area.center().x, area.top() + k::CTXBAR_OFFSET)))
            .show(ctx, |ui| {
                capsule(&th).show(ui, |ui| {
                    ui.set_height(k::CTXBAR_H - 2.0 * k::INSET);
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        ui.add_space(k::INSET);
                        ui.label(th.rich(Role::BodyStrong, &cat_label).color(th.c("text.primary")));
                        ui.add_space(k::GAP);
                        for id in &ids {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(k::BUTTON), Sense::hover());
                            let (icon, label, keys) = self.cmd_text(id);
                            let en = self.cmd_enabled(id);
                            if btn(ui, self, r, egui::Id::new(("ctxbar", id.as_str())), &icon, &label, &keys, false, en) {
                                out = Some(id.clone());
                            }
                        }
                        if self.sh.reg.get(DELETE_CMD).is_some() {
                            ui.separator();
                            let (r, resp) = ui.allocate_exact_size(Vec2::splat(k::BUTTON), Sense::click());
                            let label = self.sh.cat.t("cmd.mod.delete");
                            let l = label.clone();
                            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &l));
                            let ths = &self.sh.theme;
                            widgets::paint_state(ui, ths, r, &resp, false);
                            let icon = self.sh.reg.get(DELETE_CMD).map(|c| c.icon.clone()).unwrap_or_default();
                            let ic = Rect::from_center_size(r.center(), Vec2::splat(k::ICON));
                            self.sh.icons.paint(ui, &icon, k::ICON as u32, ic, ths.c("error"));
                            if widgets::rich_tooltip(resp, ths, &label, "", "Del").clicked() {
                                out = Some(DELETE_CMD.to_string());
                            }
                        }
                    });
                });
            });
        fire.extend(out);
    }
}

/// A small picture of `mode` for the chooser cards: tabs and band, a rail, or a dock.
pub fn preview(p: &egui::Painter, th: &ddc_ui::Theme, r: Rect, mode: UiMode) {
    let rad = th.radius("sm");
    p.rect_filled(r, rad, th.c("bg.viewport"));
    p.rect_stroke(r, rad, Stroke::new(1.0_f32, th.c("border.subtle")), StrokeKind::Inside);
    let chip = th.c("bg.canvas_chip");
    let edge = Stroke::new(1.0_f32, th.c("border.overlay"));
    let ink = th.c("text.secondary");
    let accent = th.c("accent");
    // a floor plan, faint, so the cards read as the canvas
    let plan = r.shrink(r.height() * 0.22);
    p.rect_stroke(plan, 0.0, Stroke::new(1.0_f32, th.c("border.strong")), StrokeKind::Inside);
    p.vline(plan.center().x, plan.y_range(), Stroke::new(1.0_f32, th.c("border.strong")));
    match mode {
        UiMode::Ribbon => {
            let band = Rect::from_min_size(r.min, vec2(r.width(), r.height() * 0.3));
            p.rect_filled(band, rad, th.c("bg.panel"));
            p.hline(band.x_range(), band.bottom(), Stroke::new(1.0_f32, th.c("border.subtle")));
            for i in 0..4 {
                let x = band.left() + 6.0 + i as f32 * 16.0;
                p.rect_filled(Rect::from_min_size(pos2(x, band.top() + 4.0), vec2(12.0, 3.0)), 1.0, if i == 0 { accent } else { ink });
            }
            for i in 0..6 {
                let x = band.left() + 6.0 + i as f32 * 14.0;
                p.rect_filled(Rect::from_min_size(pos2(x, band.top() + 10.0), vec2(10.0, 10.0)), 2.0, ink.gamma_multiply(0.5));
            }
        }
        UiMode::Rail => {
            let rail = Rect::from_min_size(r.min + vec2(5.0, 5.0), vec2(12.0, r.height() * 0.7));
            p.rect_filled(rail, 4.0, chip);
            p.rect_stroke(rail, 4.0, edge, StrokeKind::Inside);
            for i in 0..5 {
                let c = pos2(rail.center().x, rail.top() + 7.0 + i as f32 * 10.0);
                p.circle_filled(c, 3.0, if i == 1 { accent } else { ink });
            }
            let fly = Rect::from_min_size(pos2(rail.right() + 4.0, rail.top() + 8.0), vec2(r.width() * 0.36, r.height() * 0.4));
            p.rect_filled(fly, 4.0, chip);
            p.rect_stroke(fly, 4.0, edge, StrokeKind::Inside);
        }
        UiMode::Minimal => {
            let dock = Rect::from_center_size(pos2(r.center().x, r.bottom() - 10.0), vec2(r.width() * 0.6, 12.0));
            p.rect_filled(dock, 6.0, chip);
            p.rect_stroke(dock, 6.0, edge, StrokeKind::Inside);
            for i in 0..6 {
                let x = dock.left() + 8.0 + i as f32 * (dock.width() - 16.0) / 5.0;
                p.circle_filled(pos2(x, dock.center().y), 2.5, if i == 0 { accent } else { ink });
            }
            let bar = Rect::from_center_size(pos2(r.center().x, plan.top() - 2.0), vec2(r.width() * 0.4, 9.0));
            p.rect_filled(bar, 4.0, chip);
            p.rect_stroke(bar, 4.0, edge, StrokeKind::Inside);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_round_trip_and_ribbon_is_the_default() {
        for m in UiMode::ALL {
            assert_eq!(UiMode::parse(m.key()), Some(m));
        }
        assert_eq!(UiMode::parse("nope"), None);
        assert_eq!(UiMode::default(), UiMode::Ribbon);
        assert_eq!(UiMode::of(&crate::prefs::Prefs::default()), UiMode::Ribbon);
    }

    #[test]
    fn every_surface_is_fed_from_the_registry() {
        let reg = Registry::builtin();
        for s in &SECTIONS {
            let f = flyout(&reg, s);
            assert!(!f.is_empty(), "{}: empty flyout", s.key);
            assert!(f.iter().all(|(_, ids)| !ids.is_empty()));
            assert!(ddc_ui::icons::svg(ddc_ui::icons::Direction::Outline, 24, s.icon).is_some(), "{}", s.icon);
        }
        let own = rail_cmds(&reg);
        assert!(own.contains(&ribbon::PALETTE_CMD.to_string()) && own.contains(&ribbon::ASK_AI_CMD.to_string()));
        let dock = dock_tools(&reg);
        assert_eq!(dock.len(), k::DOCK_TOOLS);
        assert!(dock.contains(&"arch.wall".to_string()), "{dock:?}");
        assert!(reg.get(DELETE_CMD).is_some());
        let wall = ctx_cmds(&reg, "wall");
        assert!(!wall.is_empty() && wall.len() <= k::CTXBAR_CMDS, "{wall:?}");
        assert!(!wall.contains(&DELETE_CMD.to_string()));
    }

    /// Every metric of the rail, flyout, context bar and dock comes from tokens.json.
    #[test]
    fn metrics_come_from_tokens() {
        let t: serde_json::Value = serde_json::from_str(ddc_ui::theme::TOKENS_JSON).unwrap();
        let n = |g: &str, key: &str| t[g][key].as_f64().unwrap_or_else(|| panic!("{g}.{key}")) as f32;
        let pairs = [
            ("rail", "w", k::RAIL_W), ("rail", "w_expanded", k::RAIL_W_EXPANDED), ("rail", "button", k::BUTTON),
            ("rail", "icon", k::ICON), ("rail", "inset", k::INSET), ("rail", "gap", k::GAP), ("rail", "edge", k::EDGE),
            ("rail", "flyout_w", k::FLYOUT_W), ("rail", "tile_w", k::TILE_W), ("rail", "tile_h", k::TILE_H),
            ("rail", "label_max", k::LABEL_MAX), ("rail", "tile_label_max", k::TILE_LABEL_MAX),
            ("ctxbar", "h", k::CTXBAR_H), ("ctxbar", "offset", k::CTXBAR_OFFSET),
            ("dock", "button", k::DOCK_BTN), ("dock", "gap", k::DOCK_GAP),
            ("strip", "search_w", ribbon::SEARCH_W), ("strip", "button", ribbon::STRIP_BTN),
        ];
        for (g, key, v) in pairs {
            assert_eq!(n(g, key), v, "{g}.{key}");
            assert_eq!(v % 4.0, 0.0, "{g}.{key} = {v} is off the 4 pt grid");
        }
        assert_eq!(n("ctxbar", "commands") as usize, k::CTXBAR_CMDS);
        assert_eq!(n("dock", "tools") as usize, k::DOCK_TOOLS);
        assert_eq!(n("strip", "pinned") as usize, ribbon::PINNED_MAX);
        // concentric corners: capsule = plate + inset; hit targets of at least 28 pt
        let r = &t["radius"];
        assert_eq!(r["lg"].as_f64().unwrap() as f32, r["md"].as_f64().unwrap() as f32 + k::INSET);
        assert!(k::BUTTON >= 28.0 && ribbon::STRIP_BTN >= 28.0 && k::TILE_H >= 28.0);
        // the bar holds a button with an inset above and below
        assert_eq!(k::CTXBAR_H, k::BUTTON + 2.0 * k::INSET);
    }

    #[test]
    fn geometry_is_on_the_grid() {
        for v in [k::RAIL_W, k::RAIL_W_EXPANDED, k::BUTTON, k::ICON, k::INSET, k::GAP, k::EDGE, k::FLYOUT_W, k::TILE_W, k::CTXBAR_H, k::CTXBAR_OFFSET, k::DOCK_BTN] {
            assert_eq!(v % 4.0, 0.0, "{v}");
        }
        // three tiles in a row fit the flyout
        assert!(3.0 * k::TILE_W + 2.0 * 4.0 <= k::FLYOUT_W - 2.0 * k::GAP);
        assert_eq!(k::RAIL_W, k::BUTTON + 2.0 * k::INSET);
    }
}

/// Screens of each mode, light and dark, at 1x and 2x, into `C:/tmp/ui_mode_shots` (or
/// `$DDC_SHOTS`): `cargo test -p ddc-app --lib ui_mode_shots -- --ignored`.
#[cfg(all(test, feature = "gui"))]
mod shots {
    use super::*;
    use crate::editor::{Editor, Sel};

    #[test]
    #[ignore]
    fn ui_mode_shots() {
        let (pick, desc) = crate::gfx::choose(&crate::gfx::candidates(None, false), &mut |_| {})
            .expect("graphics adapter");
        let rs = crate::shot::render_state(pick).unwrap();
        let dir = std::env::var("DDC_SHOTS").unwrap_or_else(|_| "C:/tmp/ui_mode_shots".into());
        std::fs::create_dir_all(&dir).unwrap();
        for mode in UiMode::ALL {
            for theme in ["light", "dark"] {
                for scale in [1.0f32, 2.0] {
                    let mut ed = Editor::new_in_memory().unwrap();
                    let t = ed.types[0].id;
                    let id = ed
                        .model
                        .add_wall(&ddc_model::WallSpec {
                            a: (0.0, 0.0),
                            b: (8.0, 0.0),
                            level: ed.level,
                            base_offset: 0.0,
                            height: 3.0,
                            wall_type: t,
                            offset0: None,
                        })
                        .unwrap();
                    ed.reload().unwrap();
                    ed.set_mode(crate::wall_tool::Mode::Select);
                    ed.selection = vec![Sel::Wall(id)];
                    let mut app = EditorApp::new(ed, Some(&rs), desc.clone());
                    app.prefs.theme = theme.into();
                    app.prefs.ui_mode = mode.key().into();
                    app.prefs.start_screen = false;
                    app.prefs.onboarded = true;
                    app.chrome.rail.open = (mode == UiMode::Rail).then_some(0);
                    app.chrome.rail.pinned = true;
                    app.plan.fit(&app.ed);
                    let size = ((1280.0 * scale) as u32, (800.0 * scale) as u32);
                    let ctx = egui::Context::default();
                    let px = crate::shot::frame(&rs, &ctx, size, scale, 8, None, |c, _| app.ui(c, Some(&rs)));
                    let f = format!("{dir}/{}_{theme}_{scale:.0}x.png", mode.key());
                    crate::shot::save_png(std::path::Path::new(&f), size, &px).unwrap();
                }
            }
        }
    }
}
