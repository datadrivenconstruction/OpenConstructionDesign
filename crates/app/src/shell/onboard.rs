// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! First-run setup: five short steps (focus, look, icons, menu and density, summary), each
//! skippable, every choice live on the app behind and saved at once. The focus builds the
//! workspace: panel layout of each work mode, ribbon tab order, the view a project opens in and
//! the start page. Opens once on the first start, again from Help, Settings or Ctrl+K.
use super::looks_ui;
use super::mode_layout::{Layout, WorkMode};
use super::EditorApp;
use ddc_ui::looks::{self, RibbonLook};
use ddc_ui::theme::{space, Role, UiFont};
use ddc_ui::widgets;
use ddc_ui::Theme;
use egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

/// What the person mostly does; the first one chosen is the primary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Focus {
    Ai,
    Drawings,
    Model,
    Data,
}

impl Focus {
    pub const ALL: [Focus; 4] = [Self::Ai, Self::Drawings, Self::Model, Self::Data];
    pub fn key(self) -> &'static str {
        match self {
            Self::Ai => "ai",
            Self::Drawings => "drawings",
            Self::Model => "model",
            Self::Data => "data",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.key() == s.trim())
    }
    /// The work mode whose layout this focus shapes.
    pub fn mode(self) -> WorkMode {
        match self {
            Self::Ai => WorkMode::Ai,
            Self::Drawings => WorkMode::Drawings,
            Self::Model => WorkMode::Model,
            Self::Data => WorkMode::Data,
        }
    }
    /// Ribbon tabs this focus wants first, in order.
    pub fn tabs(self) -> &'static [&'static str] {
        match self {
            Self::Ai => &[],
            Self::Drawings => &["annotate", "view"],
            Self::Model => &["arch", "struct", "systems", "modify"],
            Self::Data => &["data", "manage"],
        }
    }
    /// Tab the ribbon opens on.
    pub fn start_tab(self) -> &'static str {
        self.tabs().first().copied().unwrap_or("arch")
    }
    /// Panels of this focus over `base` (whose ribbon and bars it keeps).
    pub fn layout(self, base: &Layout) -> Layout {
        let mut l = base.clone();
        l.browser_open = true;
        l.props_open = true;
        l.table_height = 0.0;
        l.ai_panel_open = false;
        match self {
            Self::Ai => {
                l.browser_open = false;
                l.ai_panel_open = true;
                l.ai_panel_w = 420.0;
            }
            Self::Drawings => l.browser_width = 300.0,
            Self::Model => {}
            Self::Data => {
                l.props_open = false;
                l.table_height = 320.0;
            }
        }
        l
    }
}

/// Focus keys of a prefs value, known ones only, no repeats, primary first.
pub fn focus_list(v: &str) -> Vec<Focus> {
    let mut out = vec![];
    for f in v.split(',').filter_map(Focus::parse) {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// The prefs value of a focus list.
pub fn focus_text(f: &[Focus]) -> String {
    f.iter().map(|f| f.key()).collect::<Vec<_>>().join(",")
}

/// A stored focus value cleaned of unknown keys.
pub fn clean_focus(v: &str) -> String {
    focus_text(&focus_list(v))
}

/// Ribbon tab order of a focus list: each focus's tabs, primary's first.
pub fn tab_order(f: &[Focus]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for t in f.iter().flat_map(|f| f.tabs()) {
        if !out.iter().any(|o| o == t) {
            out.push((*t).to_string());
        }
    }
    out
}

pub const STEPS: u8 = 5;
pub const STEP_FOCUS: u8 = 0;
pub const STEP_LOOK: u8 = 1;
pub const STEP_ICONS: u8 = 2;
pub const STEP_MENU: u8 = 3;
pub const STEP_SUMMARY: u8 = 4;

/// Commands drawn on the icon strips: what a ribbon shows first.
const STRIP: [&str; 8] = [
    "wall", "door", "window", "floor", "roof", "stair", "room", "3d-view",
];

#[derive(Default)]
pub struct Wizard {
    pub open: bool,
    pub step: u8,
    /// One icon cache per set, so every strip draws in its own set.
    caches: Vec<ddc_ui::icons::IconCache>,
    /// Step drawn last frame: a step change moves the keyboard to its first control.
    shown: Option<u8>,
    /// Never opens on its own (screenshots of other screens).
    pub quiet: bool,
    /// Weekly update checks as answered here; applied only when the wizard is finished, so
    /// nothing is sent before. None: untouched on a later visit (the setting stays as it is).
    pub updates: Option<bool>,
}

impl EditorApp {
    pub fn open_onboarding(&mut self, step: u8) {
        self.chrome.onboard.open = true;
        self.chrome.onboard.step = step.min(STEPS - 1);
        self.chrome.onboard.shown = None;
        // first start: the chooser offers weekly checks, on
        self.chrome.onboard.updates = (!self.prefs.onboarded).then_some(true);
        self.settings_open = false;
    }

    /// First start: the wizard opens once, until finished or skipped.
    pub(super) fn onboarding_first_start(&mut self) {
        let w = &self.chrome.onboard;
        if self.prefs.path.is_some() && !self.prefs.onboarded && !w.open && !w.quiet {
            self.open_onboarding(STEP_FOCUS);
        }
    }

    pub(crate) fn focus(&self) -> Vec<Focus> {
        focus_list(&self.prefs.focus)
    }

    /// The workspace of focus list `f` (primary first): every chosen mode gets its panels, the
    /// ribbon its tab order and tab, the screen the primary's panels and view. Saved.
    pub(crate) fn set_focus(&mut self, ctx: Option<&egui::Context>, f: &[Focus]) {
        self.prefs.focus = focus_text(f);
        self.sh.ribbon.tab_order = tab_order(f);
        let Some(&primary) = f.first() else {
            self.prefs.save();
            return;
        };
        let base = Layout::capture(&self.prefs);
        for x in f {
            self.prefs.mode_layouts.modes.insert(x.mode(), x.layout(&base));
        }
        let known = self.sh.reg.tabs();
        self.sh.ribbon.set_tab(primary.start_tab(), &known);
        self.default_view();
        // the mode on screen now wears the primary's panels, whatever mode the view gives
        let now = super::mode_layout::current(self);
        let l = primary.layout(&base);
        self.prefs.mode_layouts.modes.insert(now, l.clone());
        match ctx {
            Some(ctx) => self.apply_mode_layout(ctx, &l),
            None => l.restore(&mut self.prefs),
        }
        self.chrome.work_mode = Some(now);
        self.prefs.save();
    }

    /// The view the primary focus works in: 3D for modelling, the plan with the annotate tab
    /// for drawings, the data tab for data, the AI chat for AI. Nothing without a focus.
    pub(crate) fn default_view(&mut self) {
        let Some(&primary) = self.focus().first() else {
            return;
        };
        let known = self.sh.reg.tabs();
        match primary {
            Focus::Model => {
                self.run("view.3d");
            }
            Focus::Drawings => {
                self.run("view.plan");
                self.sh.ribbon.set_tab("annotate", &known);
            }
            Focus::Data => {
                self.run("view.plan");
                self.sh.ribbon.set_tab("data", &known);
            }
            Focus::Ai => {
                if crate::ai::ui::modes::current().is_none() {
                    crate::ai::ui::modes::enter(self, crate::ai::ui::modes::AiMode::Chat);
                }
            }
        }
    }

    fn toggle_focus(&mut self, ctx: &egui::Context, x: Focus) {
        let mut f = self.focus();
        match f.iter().position(|o| *o == x) {
            Some(i) => {
                f.remove(i);
            }
            None => f.push(x),
        }
        self.set_focus(Some(ctx), &f);
    }

    fn make_primary(&mut self, ctx: &egui::Context, x: Focus) {
        let mut f = self.focus();
        f.retain(|o| *o != x);
        f.insert(0, x);
        self.set_focus(Some(ctx), &f);
    }

    /// Finished or skipped: never again on its own; the focus re-applied so each mode's
    /// layout carries the ribbon chosen on the later steps.
    pub(crate) fn close_onboarding(&mut self, ctx: Option<&egui::Context>, finished: bool) {
        self.chrome.onboard.open = false;
        self.prefs.onboarded = true;
        if finished {
            if let Some(on) = self.chrome.onboard.updates.take() {
                use crate::update::{set_freq, Freq};
                set_freq(if on { Freq::Weekly } else { Freq::Never });
            }
            let f = self.focus();
            if !f.is_empty() {
                self.set_focus(ctx, &f);
            }
        }
        self.prefs.save();
    }

    pub(crate) fn onboarding_window(&mut self, ctx: &egui::Context) {
        if !self.chrome.onboard.open {
            return;
        }
        let screen = ctx.screen_rect().size();
        let w = (screen.x - 2.0 * space::XXL - 2.0 * space::XL).clamp(520.0, 960.0);
        let body_h = (screen.y - 260.0).clamp(300.0, 540.0);
        let step = self.chrome.onboard.step;
        let fresh = self.chrome.onboard.shown != Some(step);
        self.chrome.onboard.shown = Some(step);
        let mut act = None;
        let modal = crate::ui_kit::modal_dialog("onboard", ctx, |ui| {
            ui.set_width(w);
            self.ob_header(ui, step, &mut act);
            ui.add_space(space::LG);
            ui.allocate_ui(Vec2::new(w, body_h), |ui| {
                ui.set_min_size(Vec2::new(w, body_h));
                egui::ScrollArea::vertical()
                    .id_salt(("onboard.body", step))
                    .max_height(body_h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| match step {
                        STEP_FOCUS => self.ob_focus(ui, fresh, &mut act),
                        STEP_LOOK => self.ob_look(ui, &mut act),
                        STEP_ICONS => self.ob_icons(ui, fresh, &mut act),
                        STEP_MENU => self.ob_menu(ui, &mut act),
                        _ => self.ob_summary(ui),
                    });
            });
            ui.add_space(space::MD);
            self.ob_footer(ui, step, fresh, &mut act);
        });
        if modal.should_close() && !self.sh.palette.open {
            act = Some(Act::Skip);
        }
        match act {
            Some(Act::Skip) => self.close_onboarding(Some(ctx), false),
            Some(Act::Back) => self.chrome.onboard.step = step.saturating_sub(1),
            Some(Act::Next) if step + 1 >= STEPS => self.close_onboarding(Some(ctx), true),
            Some(Act::Next) => self.chrome.onboard.step = step + 1,
            Some(Act::Go(s)) => self.chrome.onboard.step = s.min(STEPS - 1),
            Some(Act::Toggle(f)) => self.toggle_focus(ctx, f),
            Some(Act::Primary(f)) => self.make_primary(ctx, f),
            Some(Act::Look(id)) => self.use_look(&id),
            Some(Act::Theme(t)) => self.ob_set(|p| p.theme = t.into()),
            Some(Act::Icons(id)) => self.ob_set(|p| p.icon_set = id.into()),
            Some(Act::Ribbon(r)) => {
                self.set_ribbon_look(r);
                self.prefs.save();
            }
            Some(Act::Density(d)) => self.ob_set(|p| p.density = d.into()),
            Some(Act::Mode(m)) => self.set_ui_mode(m),
            Some(Act::Updates(on)) => self.chrome.onboard.updates = Some(on),
            Some(Act::Font(f)) => self.ob_set(|p| p.font = f.key().into()),
            Some(Act::Scale(s)) => self.ob_set(|p| p.ui_scale = s),
            None => {}
        }
    }

    fn ob_set(&mut self, f: impl FnOnce(&mut crate::prefs::Prefs)) {
        f(&mut self.prefs);
        self.sh.theme.set_scale(self.prefs.ui_scale);
        self.prefs.save();
    }

    fn ob_header(&mut self, ui: &mut egui::Ui, step: u8, act: &mut Option<Act>) {
        let th = &self.sh.theme;
        let c = &self.sh.cat;
        ui.horizontal(|ui| {
            // step dots: done ones filled, the current one long; each goes to its step
            for s in 0..STEPS {
                let wd = if s == step { 22.0 } else { 8.0 };
                let (r, resp) = ui.allocate_exact_size(Vec2::new(wd, 8.0), Sense::click());
                let name = c.t(&format!("onb.step.{s}"));
                let label = c.tf("onb.step_of", &[("n", &(s + 1).to_string()), ("name", &name)]);
                let resp = resp.on_hover_text(&label);
                resp.widget_info(|| {
                    egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, s == step, &label)
                });
                let fill = if s <= step { th.c("accent") } else { th.c("border.strong") };
                ui.painter().rect_filled(r, 4.0, fill);
                if resp.has_focus() {
                    let ring = Stroke::new(2.0, th.c("focus.ring"));
                    ui.painter().rect_stroke(r.expand(2.0), 6.0, ring, StrokeKind::Outside);
                }
                if resp.clicked() {
                    *act = Some(Act::Go(s));
                }
            }
            ui.add_space(space::SM);
            let of = c.tf(
                "onb.counter",
                &[("n", &(step + 1).to_string()), ("total", &STEPS.to_string())],
            );
            ui.label(th.rich(Role::Caption, &of).color(th.c("text.secondary")));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let skip = c.t("onb.skip");
                if widgets::Button::quiet(&skip).show(ui, th).on_hover_text("Esc").clicked() {
                    *act = Some(Act::Skip);
                }
            });
        });
        ui.add_space(space::LG);
        let title = c.t(&format!("onb.step.{step}.title"));
        ui.label(th.rich(Role::Heading, &title).color(th.c("text.primary")));
        ui.add_space(space::XS);
        let sub = c.t(&format!("onb.step.{step}.sub"));
        ui.label(th.rich(Role::Body, &sub).color(th.c("text.secondary")));
    }

    fn ob_footer(&mut self, ui: &mut egui::Ui, step: u8, fresh: bool, act: &mut Option<Act>) {
        let c = &self.sh.cat;
        let th = &self.sh.theme;
        ui.separator();
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            let note = if step + 1 == STEPS { c.t("onb.later") } else { c.t("onb.keys") };
            ui.label(th.rich(Role::Caption, &note).color(th.c("text.secondary")));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let last = step + 1 == STEPS;
                let next = if last { c.t("onb.start") } else { c.t("onb.next") };
                // only the last step confirms with Enter, never on the frame it appears: a
                // stray or held Enter cannot walk past the choices
                let go = if last && !fresh {
                    crate::ui_kit::primary_action(ui, next, None)
                } else {
                    crate::ui_kit::primary_click(ui, next, None)
                };
                if go {
                    *act = Some(Act::Next);
                }
                if step > 0 {
                    let back = c.t("onb.back");
                    if crate::ui_kit::secondary_button(ui, back).on_hover_text(c.t("onb.back_keys")).clicked() {
                        *act = Some(Act::Back);
                    }
                }
            });
        });
        // Alt+arrows walk the steps from anywhere in the wizard
        let (back, next) = ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft),
                i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowRight),
            )
        });
        if back && step > 0 {
            *act = Some(Act::Back);
        } else if next {
            *act = Some(Act::Next);
        }
    }

    fn ob_focus(&mut self, ui: &mut egui::Ui, fresh: bool, act: &mut Option<Act>) {
        let th = &self.sh.theme;
        let c = &self.sh.cat;
        let chosen = focus_list(&self.prefs.focus);
        let gap = space::MD;
        let avail = ui.available_width();
        let per_row = if avail >= 4.0 * 190.0 + 3.0 * gap { 4 } else { 2 };
        let cw = ((avail - gap * (per_row as f32 - 1.0)) / per_row as f32).floor();
        let art_h = if per_row == 4 { 132.0 } else { 104.0 };
        // the tallest wrapped line text of any card sets the card height: a longer language
        // (Russian) wraps to three lines and must not run into the foot row
        let line_w = cw - 2.0 * space::MD;
        let line_h = Focus::ALL
            .iter()
            .map(|f| text_wrap(ui, th, Role::Caption, &c.t(&format!("onb.focus.{}.line", f.key())), line_w, "text.secondary").size().y)
            .fold(th.line_h(Role::Caption) * 2.0, f32::max);
        let text_h = th.line_h(Role::BodyStrong) + space::XXS + line_h + space::SM;
        let ch = art_h + space::MD + text_h + space::MD + 28.0 + space::MD;
        for (row_i, row) in Focus::ALL.chunks(per_row).enumerate() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (j, &f) in row.iter().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(cw, ch), Sense::click());
                    let on = chosen.contains(&f);
                    let primary = chosen.first() == Some(&f);
                    let name = c.t(&format!("onb.focus.{}", f.key()));
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, &name)
                    });
                    if fresh && row_i == 0 && j == 0 {
                        resp.request_focus();
                    }
                    if resp.clicked() {
                        *act = Some(Act::Toggle(f));
                    }
                    focus_card(ui, th, r, on, resp.hovered(), resp.has_focus());
                    let pad = space::MD;
                    let art = Rect::from_min_size(r.min + Vec2::splat(pad), Vec2::new(cw - 2.0 * pad, art_h));
                    illustration(ui.painter(), th, f, art, on);
                    let tx = art.min.x;
                    let mut y = art.max.y + space::MD;
                    let g = text_line(ui, th, Role::BodyStrong, &name, art.width(), "text.primary");
                    ui.painter().galley(Pos2::new(tx, y), g.clone(), th.c("text.primary"));
                    y += g.size().y + space::XXS;
                    let line = c.t(&format!("onb.focus.{}.line", f.key()));
                    let g = text_wrap(ui, th, Role::Caption, &line, art.width(), "text.secondary");
                    ui.painter().galley(Pos2::new(tx, y), g, th.c("text.secondary"));
                    // the primary mark, or the button that makes a chosen card the primary
                    let foot = Rect::from_min_size(
                        Pos2::new(tx, r.max.y - pad - 28.0),
                        Vec2::new(art.width(), 28.0),
                    );
                    if primary {
                        let t = c.t("onb.primary");
                        badge(ui, th, foot, &t);
                    } else if on {
                        let t = c.t("onb.make_primary");
                        let mut child = ui.new_child(
                            egui::UiBuilder::new().max_rect(foot).layout(egui::Layout::left_to_right(egui::Align::Center)),
                        );
                        if widgets::Button::quiet(&t).show(&mut child, th).clicked() {
                            *act = Some(Act::Primary(f));
                        }
                    }
                }
            });
            ui.add_space(gap);
        }
        let note = if chosen.is_empty() { c.t("onb.focus.none") } else { c.t("onb.focus.hint") };
        ui.label(th.rich(Role::Caption, &note).color(th.c("text.secondary")));
    }

    fn ob_look(&mut self, ui: &mut egui::Ui, act: &mut Option<Act>) {
        let th = &self.sh.theme;
        let c = &self.sh.cat;
        // the row is as tall as the switch from the start: egui centres the label in the height
        // the row had when the label was placed
        let row = egui::vec2(ui.available_width(), th.d().control);
        ui.allocate_ui_with_layout(row, egui::Layout::left_to_right(egui::Align::Center), |ui| {
            let title = th.rich(Role::BodyStrong, &c.t("settings.theme")).color(th.c("text.primary"));
            crate::ui_kit::caption_on_line(ui, title, &th.font(Role::Label));
            ui.add_space(space::MD);
            let keys = ["light", "dark", "system"];
            let names = [c.t("onb.theme.light"), c.t("onb.theme.dark"), c.t("onb.theme.auto")];
            let items: Vec<&str> = names.iter().map(String::as_str).collect();
            let sel = keys.iter().position(|k| *k == self.prefs.theme).unwrap_or(2);
            if let Some(i) = widgets::segmented(ui, th, &items, sel) {
                *act = Some(Act::Theme(keys[i]));
            }
        });
        ui.add_space(space::LG);
        if let Some(id) = looks_ui::gallery_sized(ui, th, c, &self.prefs.look, 200.0, 104.0) {
            *act = Some(Act::Look(id));
        }
        let l = self.look();
        let desc = c.t(&l.desc_key());
        let name = c.t(&l.name_key());
        ui.add_space(space::XS);
        ui.horizontal_wrapped(|ui| {
            ui.label(th.rich(Role::BodyStrong, &name).color(th.c("text.primary")));
            ui.label(th.rich(Role::Body, &desc).color(th.c("text.secondary")));
        });
    }

    fn ob_icons(&mut self, ui: &mut egui::Ui, fresh: bool, act: &mut Option<Act>) {
        let sets = ddc_ui::icons::sets();
        let wiz = &mut self.chrome.onboard;
        if wiz.caches.len() != sets.len() {
            wiz.caches = sets
                .iter()
                .map(|s| {
                    let mut k = ddc_ui::icons::IconCache::default();
                    k.set_style(s.style());
                    k
                })
                .collect();
        }
        let th = &self.sh.theme;
        let c = &self.sh.cat;
        let cur = ddc_ui::icons::set(&self.prefs.icon_set).map_or("", |s| s.id);
        let row_h = 56.0;
        let w = ui.available_width();
        let name_w = (w * 0.34).clamp(180.0, 300.0);
        for (i, s) in sets.iter().enumerate() {
            let (r, resp) = ui.allocate_exact_size(Vec2::new(w, row_h), Sense::click());
            let on = s.id == cur;
            let name = c.t(&format!("theme.icons.{}", s.id));
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, on, &name));
            if fresh && i == 0 {
                resp.request_focus();
            }
            if resp.clicked() {
                *act = Some(Act::Icons(s.id));
            }
            let p = ui.painter();
            let rad = th.radius("md");
            let fill = if on { th.c("accent.subtle") } else if resp.hovered() { th.c("bg.hover") } else { th.c("bg.panel") };
            p.rect_filled(r, rad, fill);
            let ring = if on { Stroke::new(2.0, th.c("accent")) } else { Stroke::new(1.0, th.c("border.subtle")) };
            p.rect_stroke(r, rad, ring, StrokeKind::Inside);
            if resp.has_focus() {
                p.rect_stroke(r.expand(2.0), rad + 2.0, Stroke::new(2.0, th.c("focus.ring")), StrokeKind::Outside);
            }
            let g = text_line(ui, th, if on { Role::BodyStrong } else { Role::Body }, &name, name_w - 2.0 * space::MD, "text.primary");
            let ty = r.center().y - g.size().y / 2.0;
            ui.painter().galley(Pos2::new(r.min.x + space::MD, ty), g, th.c("text.primary"));
            // a ribbon strip in this set: the first button drawn active, as the ribbon does
            let strip = Rect::from_min_max(Pos2::new(r.min.x + name_w, r.min.y + 8.0), Pos2::new(r.max.x - space::MD, r.max.y - 8.0));
            let px = 24.0;
            let step = ((strip.width() - px) / (STRIP.len() as f32 - 1.0)).min(px + 20.0);
            let cache = &mut self.chrome.onboard.caches[i];
            for (k, icon) in STRIP.iter().enumerate() {
                let at = Pos2::new(strip.min.x + k as f32 * step, strip.center().y - px / 2.0);
                let ir = Rect::from_min_size(at, Vec2::splat(px));
                let name = if k == 0 { format!("{icon}.fill") } else { (*icon).to_string() };
                let ink = if k == 0 { th.c("accent") } else { th.c("text.primary") };
                if k == 0 {
                    ui.painter().rect_filled(ir.expand(4.0), 6.0, th.c("accent.subtle"));
                }
                cache.paint(ui, &name, px as u32, ir, ink);
            }
            ui.add_space(space::SM);
        }
    }

    /// Interface mode: three cards with a small picture each, then the weekly update check.
    fn ob_mode(&mut self, ui: &mut egui::Ui, act: &mut Option<Act>) {
        use super::ui_mode::{preview, UiMode};
        let th = &self.sh.theme;
        let c = &self.sh.cat;
        ui.label(th.rich(Role::BodyStrong, &c.t("ui_mode.title")).color(th.c("text.primary")));
        ui.add_space(space::XS);
        let now = UiMode::of(&self.prefs);
        let gap = space::MD;
        let cw = ((ui.available_width() - 2.0 * gap) / 3.0).floor();
        let inner = cw - 2.0 * space::MD;
        let desc_h = UiMode::ALL
            .iter()
            .map(|m| text_wrap(ui, th, Role::Caption, &c.t(&m.desc_key()), inner, "text.secondary").size().y)
            .fold(0.0, f32::max);
        let (name_h, art_h) = (24.0, 64.0);
        let ch = 2.0 * space::MD + name_h + art_h + space::SM + desc_h;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for m in UiMode::ALL {
                let (r, resp) = ui.allocate_exact_size(Vec2::new(cw, ch), Sense::click());
                let name = c.t(&m.label_key());
                resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, m == now, &name));
                focus_card(ui, th, r, m == now, resp.hovered(), resp.has_focus());
                let x = r.min.x + space::MD;
                let g = text_line(ui, th, Role::BodyStrong, &name, inner - 28.0, "text.primary");
                ui.painter().galley(Pos2::new(x, r.min.y + space::MD), g, th.c("text.primary"));
                let art = Rect::from_min_size(Pos2::new(x, r.min.y + space::MD + name_h), Vec2::new(inner, art_h));
                preview(ui.painter(), th, art, m);
                let d = text_wrap(ui, th, Role::Caption, &c.t(&m.desc_key()), inner, "text.secondary");
                ui.painter().galley(Pos2::new(x, art.max.y + space::SM), d, th.c("text.secondary"));
                if resp.clicked() {
                    *act = Some(Act::Mode(m));
                }
            }
        });
        ui.add_space(space::LG);
        let mut on = self.chrome.onboard.updates.unwrap_or(crate::update::freq() != crate::update::Freq::Never);
        let was = on;
        widgets::toggle(ui, th, &mut on, &c.t("onb.updates"));
        if on != was {
            *act = Some(Act::Updates(on));
        }
        ui.add(egui::Label::new(th.rich(Role::Caption, &c.t("update_check.desc")).color(th.c("text.secondary"))).wrap());
        ui.add_space(space::LG);
    }

    fn ob_menu(&mut self, ui: &mut egui::Ui, act: &mut Option<Act>) {
        self.ob_mode(ui, act);
        let w = ui.available_width();
        let left = (w * 0.46).clamp(300.0, 440.0);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(left);
                let th = &self.sh.theme;
                let c = &self.sh.cat;
                let p = &self.prefs;
                let row = |ui: &mut egui::Ui, label: &str, items: &[String], sel: usize| {
                    ui.label(th.rich(Role::BodyStrong, label).color(th.c("text.primary")));
                    ui.add_space(space::XS);
                    let items: Vec<&str> = items.iter().map(String::as_str).collect();
                    let r = widgets::segmented(ui, th, &items, sel);
                    ui.add_space(space::LG);
                    r
                };
                let rl = RibbonLook::ALL;
                let names: Vec<String> = rl.iter().map(|r| c.t(&format!("theme.ribbon.{}", r.key()))).collect();
                let now = looks_ui::ribbon_now(p);
                if let Some(i) = row(ui, &c.t("theme.ribbon"), &names, rl.iter().position(|r| *r == now).unwrap_or(0)) {
                    *act = Some(Act::Ribbon(rl[i]));
                }
                let names: Vec<String> = looks::DENSITIES.iter().map(|d| c.t(&format!("settings.density.{d}"))).collect();
                // "auto" (the layout decides) lights no pill, as the summary names it
                let sel = looks::DENSITIES.iter().position(|d| *d == p.density).unwrap_or(usize::MAX);
                if let Some(i) = row(ui, &c.t("settings.density"), &names, sel) {
                    *act = Some(Act::Density(looks::DENSITIES[i]));
                }
                let faces = UiFont::ALL;
                let names: Vec<String> = faces.iter().map(|f| c.t(&format!("theme.font.{}", f.key()))).collect();
                let face = UiFont::parse(&p.font).unwrap_or_default();
                if let Some(i) = row(ui, &c.t("theme.font"), &names, faces.iter().position(|f| *f == face).unwrap_or(0)) {
                    *act = Some(Act::Font(faces[i]));
                }
                let steps = looks_ui::scale_steps();
                let names: Vec<String> = steps.iter().map(|s| format!("{:.0} %", s * 100.0)).collect();
                let sel = steps.iter().position(|s| (s - p.ui_scale).abs() < 1e-3).unwrap_or(1);
                if let Some(i) = row(ui, &c.t("settings.scale"), &names, sel) {
                    *act = Some(Act::Scale(steps[i]));
                }
            });
            ui.add_space(space::XL);
            let pw = ui.available_width();
            let ph = (pw * 0.62).min(ui.available_height().max(220.0));
            let (r, _) = ui.allocate_exact_size(Vec2::new(pw, ph), Sense::hover());
            self.menu_preview(ui, r);
        });
    }

    /// The app's top in the chosen ribbon, density and face: drawn with the live theme.
    fn menu_preview(&mut self, ui: &egui::Ui, r: Rect) {
        let th = &self.sh.theme;
        let c = &self.sh.cat;
        let p = ui.painter_at(r);
        let rad = th.radius("md");
        p.rect_filled(r, rad, th.c("bg.app"));
        p.rect_stroke(r, rad, Stroke::new(1.0, th.c("border.subtle")), StrokeKind::Inside);
        let look = looks_ui::ribbon_now(&self.prefs);
        let mut y = r.min.y + space::SM;
        let x0 = r.min.x + space::SM;
        // tab strip
        let tab_h = 26.0;
        let tabs = ["tab.arch", "tab.annotate", "tab.view", "tab.data"];
        let mut x = x0;
        for (i, t) in tabs.iter().enumerate() {
            let g = text_line(ui, th, Role::Tab, &c.t(t), 120.0, "text.primary");
            let tw = g.size().x + 2.0 * space::SM;
            if x + tw > r.max.x - space::SM {
                break;
            }
            let tr = Rect::from_min_size(Pos2::new(x, y), Vec2::new(tw, tab_h));
            let ink = if i == 0 { th.c("text.primary") } else { th.c("text.secondary") };
            p.galley(Pos2::new(x + space::SM, tr.center().y - g.size().y / 2.0), g, ink);
            if i == 0 {
                p.rect_filled(Rect::from_min_max(Pos2::new(tr.min.x + 4.0, tr.max.y - 2.0), Pos2::new(tr.max.x - 4.0, tr.max.y)), 1.0, th.c("accent"));
            }
            x += tw;
        }
        y += tab_h;
        let icons = &mut self.sh.icons;
        let band = |h: f32| Rect::from_min_size(Pos2::new(r.min.x + 1.0, y), Vec2::new(r.width() - 2.0, h));
        match look {
            RibbonLook::Full => {
                let b = band(78.0);
                p.rect_filled(b, 0.0, th.c("bg.panel"));
                let mut bx = x0;
                for (k, (icon, label)) in [("wall", "cmd.arch.wall"), ("door", "cmd.arch.door"), ("window", "cmd.arch.window"), ("floor", "cmd.arch.floor")].iter().enumerate() {
                    let bw = 64.0;
                    if bx + bw > b.max.x {
                        break;
                    }
                    let ir = Rect::from_center_size(Pos2::new(bx + bw / 2.0, b.min.y + 22.0), Vec2::splat(32.0));
                    let ink = if k == 0 { th.c("accent") } else { th.c("text.primary") };
                    icons.paint(ui, icon, 32, ir, ink);
                    let g = text_line(ui, th, Role::RibbonLabel, &c.t(label), bw, "text.primary");
                    p.galley(Pos2::new(bx + (bw - g.size().x) / 2.0, b.min.y + 44.0), g, th.c("text.primary"));
                    bx += bw + space::XS;
                }
                y = b.max.y;
            }
            RibbonLook::Toolbar => {
                let b = band(36.0);
                p.rect_filled(b, 0.0, th.c("bg.panel"));
                for (k, icon) in STRIP.iter().enumerate() {
                    let ir = Rect::from_min_size(Pos2::new(x0 + k as f32 * 28.0, b.center().y - 8.0), Vec2::splat(16.0));
                    if ir.max.x > b.max.x - space::SM {
                        break;
                    }
                    icons.paint(ui, icon, 16, ir, th.c("text.primary"));
                }
                y = b.max.y;
            }
            RibbonLook::Tabs => {
                p.hline(r.min.x..=r.max.x, y, Stroke::new(1.0, th.c("border.subtle")));
            }
        }
        // browser rows at the chosen density, in the chosen face
        y += space::SM;
        let row_h = th.d().row;
        let names = ["onb.preview.row1", "onb.preview.row2", "onb.preview.row3", "onb.preview.row4", "onb.preview.row5"];
        for (i, n) in names.iter().enumerate() {
            if y + row_h > r.max.y - space::SM {
                break;
            }
            let rr = Rect::from_min_size(Pos2::new(x0, y), Vec2::new(r.width() - 2.0 * space::SM, row_h));
            if i == 1 {
                p.rect_filled(rr, th.radius("sm"), th.c("accent.subtle"));
            }
            let g = text_line(ui, th, Role::Body, &c.t(n), rr.width() - space::SM * 2.0, "text.primary");
            p.galley(Pos2::new(rr.min.x + space::SM, rr.center().y - g.size().y / 2.0), g, th.c("text.primary"));
            y += row_h;
        }
    }

    fn ob_summary(&mut self, ui: &mut egui::Ui) {
        let th = &self.sh.theme;
        let c = &self.sh.cat;
        let f = self.focus();
        let w = ui.available_width();
        let list_w = (w * 0.48).clamp(300.0, 460.0);
        let focus_text = if f.is_empty() {
            c.t("onb.sum.focus_none")
        } else {
            f.iter()
                .enumerate()
                .map(|(i, x)| {
                    let n = c.t(&format!("onb.focus.{}", x.key()));
                    if i == 0 { c.tf("onb.sum.primary", &[("name", &n)]) } else { n }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let face = UiFont::parse(&self.prefs.font).unwrap_or_default();
        let dens = if looks::DENSITIES.contains(&self.prefs.density.as_str()) {
            c.t(&format!("settings.density.{}", self.prefs.density))
        } else {
            c.t("settings.density.auto")
        };
        let mode = c.t(&super::ui_mode::UiMode::of(&self.prefs).label_key());
        let rows: [(&str, &str, String); 7] = [
            ("spark", "onb.sum.focus", focus_text),
            ("layout-focus", "onb.sum.mode", mode),
            ("shading", "onb.sum.look", c.t(&self.look().name_key())),
            ("layout-ribbon", "onb.sum.icons", c.t(&format!("theme.icons.{}", self.prefs.icon_set))),
            ("fold-ribbon", "onb.sum.ribbon", c.t(&format!("theme.ribbon.{}", looks_ui::ribbon_now(&self.prefs).key()))),
            ("maximize", "onb.sum.density", format!("{dens} · {:.0} %", self.prefs.ui_scale * 100.0)),
            ("text", "onb.sum.font", c.t(&format!("theme.font.{}", face.key()))),
        ];
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(list_w);
                for (icon, k, v) in &rows {
                    ui.horizontal(|ui| {
                        let px = ddc_ui::theme::icon_size::MD;
                        let (ir, _) = ui.allocate_exact_size(Vec2::splat(px), Sense::hover());
                        self.sh.icons.paint(ui, icon, px as u32, ir, th.c("accent"));
                        ui.add_space(space::SM);
                        ui.vertical(|ui| {
                            ui.label(th.rich(Role::Caption, &c.t(k)).color(th.c("text.secondary")));
                            ui.add(egui::Label::new(th.rich(Role::BodyStrong, v).color(th.c("text.primary"))).wrap());
                        });
                    });
                    ui.add_space(space::MD);
                }
            });
            ui.add_space(space::XL);
            let pw = ui.available_width();
            let ph = (pw * 0.7).min(320.0);
            let (r, _) = ui.allocate_exact_size(Vec2::new(pw, ph), Sense::hover());
            workspace_map(ui, th, c, r, f.first().copied(), &self.prefs);
        });
    }
}

enum Act {
    Skip,
    Back,
    Next,
    Go(u8),
    Toggle(Focus),
    Primary(Focus),
    Look(String),
    Theme(&'static str),
    Icons(&'static str),
    Ribbon(RibbonLook),
    Density(&'static str),
    Mode(super::ui_mode::UiMode),
    Updates(bool),
    Font(UiFont),
    Scale(f32),
}

fn text_line(ui: &egui::Ui, th: &Theme, role: Role, s: &str, w: f32, ink: &str) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(s.to_string(), th.font(role), th.c(ink));
    job.wrap = egui::text::TextWrapping::truncate_at_width(w.max(1.0));
    ui.fonts(|f| f.layout_job(job))
}

fn text_wrap(ui: &egui::Ui, th: &Theme, role: Role, s: &str, w: f32, ink: &str) -> std::sync::Arc<egui::Galley> {
    ui.fonts(|f| f.layout(s.to_string(), th.font(role), th.c(ink), w.max(1.0)))
}

fn focus_card(ui: &egui::Ui, th: &Theme, r: Rect, on: bool, hover: bool, focused: bool) {
    let p = ui.painter();
    let rad = th.radius("lg");
    let fill = if on { th.c("accent.subtle") } else if hover { th.c("bg.hover") } else { th.c("bg.panel") };
    p.rect_filled(r, rad, fill);
    let ring = if on {
        Stroke::new(2.0, th.c("accent"))
    } else if hover {
        Stroke::new(1.0, th.c("border.strong"))
    } else {
        Stroke::new(1.0, th.c("border.subtle"))
    };
    p.rect_stroke(r, rad, ring, StrokeKind::Inside);
    if focused {
        p.rect_stroke(r.expand(2.0), rad + 2.0, Stroke::new(2.0, th.c("focus.ring")), StrokeKind::Outside);
    }
    // the check in the corner
    let d = 20.0;
    let cc = Pos2::new(r.max.x - space::MD - d / 2.0, r.min.y + space::MD + d / 2.0);
    if on {
        p.circle_filled(cc, d / 2.0, th.c("accent"));
        let ink = th.c("accent.text_on");
        let pts = [cc + Vec2::new(-4.5, 0.0), cc + Vec2::new(-1.0, 3.5), cc + Vec2::new(5.0, -3.5)];
        p.line_segment([pts[0], pts[1]], Stroke::new(2.0, ink));
        p.line_segment([pts[1], pts[2]], Stroke::new(2.0, ink));
    } else {
        p.circle_stroke(cc, d / 2.0 - 0.75, Stroke::new(1.5, th.c("border.strong")));
    }
}

fn badge(ui: &egui::Ui, th: &Theme, foot: Rect, text: &str) {
    let g = text_line(ui, th, Role::Label, text, foot.width() - 2.0 * space::SM, "accent.text_on");
    let w = g.size().x + 2.0 * space::SM + 14.0;
    let r = Rect::from_min_size(Pos2::new(foot.min.x, foot.center().y - 11.0), Vec2::new(w, 22.0));
    let p = ui.painter();
    p.rect_filled(r, 11.0, th.c("accent"));
    let star = Pos2::new(r.min.x + space::SM + 5.0, r.center().y);
    star_shape(p, star, 5.5, th.c("accent.text_on"));
    p.galley(Pos2::new(r.min.x + space::SM + 14.0, r.center().y - g.size().y / 2.0), g, th.c("accent.text_on"));
}

fn star_shape(p: &egui::Painter, c: Pos2, rad: f32, ink: Color32) {
    let pts: Vec<Pos2> = (0..10)
        .map(|i| {
            let a = std::f32::consts::PI * (i as f32) / 5.0 - std::f32::consts::FRAC_PI_2;
            let rr = if i % 2 == 0 { rad } else { rad * 0.45 };
            c + Vec2::new(a.cos(), a.sin()) * rr
        })
        .collect();
    // a star is not convex: a fan of triangles from its centre
    for i in 0..10 {
        let tri = vec![c, pts[i], pts[(i + 1) % 10]];
        p.add(egui::Shape::convex_polygon(tri, ink, Stroke::NONE));
    }
}

/// The picture of a focus card: vector art in the theme colours.
fn illustration(p: &egui::Painter, th: &Theme, f: Focus, r: Rect, on: bool) {
    let rad = th.radius("md");
    let bg = th.c("bg.viewport");
    p.rect_filled(r, rad, bg);
    let acc = th.c("accent");
    let ink = th.c("text.secondary");
    let faint = th.c("border.strong");
    let thin = Stroke::new(1.5, ink);
    let c = r.center();
    let s = (r.height() / 132.0).min(r.width() / 200.0);
    let at = |x: f32, y: f32| c + Vec2::new(x, y) * s;
    match f {
        Focus::Ai => {
            // a chat: question bubble, answer bubble with a spark
            let q = Rect::from_min_max(at(-70.0, -44.0), at(20.0, -14.0));
            p.rect_filled(q, 10.0 * s, th.c("bg.panel"));
            p.rect_stroke(q, 10.0 * s, Stroke::new(1.0, faint), StrokeKind::Inside);
            for (i, wv) in [60.0, 40.0].iter().enumerate() {
                let y = -34.0 + i as f32 * 10.0;
                p.line_segment([at(-60.0, y), at(-60.0 + wv, y)], Stroke::new(3.0 * s, ink));
            }
            let a = Rect::from_min_max(at(-20.0, 0.0), at(72.0, 44.0));
            p.rect_filled(a, 10.0 * s, acc.gamma_multiply(if on { 1.0 } else { 0.85 }));
            let w = th.c("accent.text_on");
            for (i, wv) in [64.0, 52.0, 30.0].iter().enumerate() {
                let y = 10.0 + i as f32 * 11.0;
                p.line_segment([at(-10.0, y), at(-10.0 + wv, y)], Stroke::new(3.0 * s, w));
            }
            sparkle(p, at(-48.0, 22.0), 14.0 * s, acc);
            sparkle(p, at(-62.0, 6.0), 6.0 * s, acc);
        }
        Focus::Drawings => {
            // a sheet: border, title block, a plan with a dimension
            let sh = Rect::from_min_max(at(-80.0, -50.0), at(80.0, 50.0));
            p.rect_filled(sh, 2.0, th.c("bg.panel"));
            p.rect_stroke(sh, 2.0, Stroke::new(1.0, faint), StrokeKind::Inside);
            let tb = Rect::from_min_max(at(30.0, 28.0), at(76.0, 46.0));
            p.rect_stroke(tb, 0.0, thin, StrokeKind::Inside);
            p.line_segment([at(30.0, 37.0), at(76.0, 37.0)], Stroke::new(1.0, ink));
            let room = Rect::from_min_max(at(-66.0, -38.0), at(14.0, 30.0));
            p.rect_stroke(room, 0.0, Stroke::new(3.0 * s, th.c("text.primary")), StrokeKind::Inside);
            p.line_segment([at(-26.0, -38.0), at(-26.0, 30.0)], Stroke::new(2.0 * s, th.c("text.primary")));
            // door swing
            p.line_segment([at(-66.0, 4.0), at(-50.0, 4.0)], Stroke::new(1.0, acc));
            p.add(egui::Shape::line(
                (0..=8).map(|i| {
                    let a = std::f32::consts::FRAC_PI_2 * i as f32 / 8.0;
                    at(-66.0 + 16.0 * a.cos(), 4.0 - 16.0 * a.sin())
                }).collect(),
                Stroke::new(1.0, acc),
            ));
            let dy = -44.0;
            p.line_segment([at(-66.0, dy), at(14.0, dy)], Stroke::new(1.0, acc));
            for x in [-66.0, 14.0] {
                p.line_segment([at(x - 3.0, dy + 3.0), at(x + 3.0, dy - 3.0)], Stroke::new(1.5, acc));
            }
            for (i, wv) in [30.0, 22.0].iter().enumerate() {
                let y = -30.0 + i as f32 * 8.0;
                p.line_segment([at(30.0, y), at(30.0 + wv, y)], Stroke::new(2.0 * s, ink));
            }
        }
        Focus::Model => {
            // an isometric building block with an accent roof edge
            let top = [at(0.0, -52.0), at(56.0, -24.0), at(0.0, 4.0), at(-56.0, -24.0)];
            let left = [at(-56.0, -24.0), at(0.0, 4.0), at(0.0, 52.0), at(-56.0, 24.0)];
            let right = [at(0.0, 4.0), at(56.0, -24.0), at(56.0, 24.0), at(0.0, 52.0)];
            p.add(egui::Shape::convex_polygon(top.to_vec(), th.c("bg.panel"), Stroke::new(1.5, th.c("text.primary"))));
            p.add(egui::Shape::convex_polygon(left.to_vec(), faint.gamma_multiply(0.5), Stroke::new(1.5, th.c("text.primary"))));
            p.add(egui::Shape::convex_polygon(right.to_vec(), faint.gamma_multiply(0.25), Stroke::new(1.5, th.c("text.primary"))));
            // windows on the right face
            for (i, j) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                let x = 12.0 + i * 22.0;
                let y = -2.0 - i * 11.0 + j * 22.0;
                let q = vec![at(x, y), at(x + 12.0, y - 6.0), at(x + 12.0, y + 6.0), at(x, y + 12.0)];
                p.add(egui::Shape::convex_polygon(q, acc.gamma_multiply(0.75), Stroke::NONE));
            }
            p.line_segment([top[3], top[0]], Stroke::new(3.0 * s, acc));
            p.line_segment([top[0], top[1]], Stroke::new(3.0 * s, acc));
        }
        Focus::Data => {
            // a table with a header row and a highlighted column, and a bar chart beside it
            let t = Rect::from_min_max(at(-80.0, -46.0), at(24.0, 46.0));
            p.rect_filled(t, 4.0 * s, th.c("bg.panel"));
            let hd = Rect::from_min_max(t.min, Pos2::new(t.max.x, t.min.y + 16.0 * s));
            p.rect_filled(hd, 4.0 * s, acc);
            let col = Rect::from_min_max(Pos2::new(t.min.x + 52.0 * s, hd.max.y), Pos2::new(t.min.x + 78.0 * s, t.max.y));
            p.rect_filled(col, 0.0, acc.gamma_multiply(0.18));
            for i in 1..5 {
                let y = hd.max.y + i as f32 * 15.0 * s;
                p.line_segment([Pos2::new(t.min.x, y), Pos2::new(t.max.x, y)], Stroke::new(1.0, faint));
            }
            for x in [26.0, 52.0, 78.0] {
                let x = t.min.x + x * s;
                p.line_segment([Pos2::new(x, hd.max.y), Pos2::new(x, t.max.y)], Stroke::new(1.0, faint));
            }
            p.rect_stroke(t, 4.0 * s, Stroke::new(1.0, faint), StrokeKind::Inside);
            for (i, h) in [30.0, 54.0, 40.0, 70.0].iter().enumerate() {
                let x = 36.0 + i as f32 * 12.0;
                let b = Rect::from_min_max(at(x, 46.0 - h), at(x + 8.0, 46.0));
                p.rect_filled(b, 1.5 * s, if i == 3 { acc } else { ink });
            }
        }
    }
}

fn sparkle(p: &egui::Painter, c: Pos2, r: f32, ink: Color32) {
    let k = r * 0.28;
    let pts = vec![
        c + Vec2::new(0.0, -r),
        c + Vec2::new(k, -k),
        c + Vec2::new(r, 0.0),
        c + Vec2::new(k, k),
        c + Vec2::new(0.0, r),
        c + Vec2::new(-k, k),
        c + Vec2::new(-r, 0.0),
        c + Vec2::new(-k, -k),
    ];
    for i in 0..4 {
        let tri = vec![c, pts[2 * i], pts[2 * i + 1]];
        p.add(egui::Shape::convex_polygon(tri, ink, Stroke::NONE));
        let tri = vec![c, pts[2 * i + 1], pts[(2 * i + 2) % 8]];
        p.add(egui::Shape::convex_polygon(tri, ink, Stroke::NONE));
    }
}

/// The workspace the primary focus gives, as a labelled panel map in the live theme.
fn workspace_map(ui: &egui::Ui, th: &Theme, c: &ddc_ui::Catalog, r: Rect, primary: Option<Focus>, prefs: &crate::prefs::Prefs) {
    let p = ui.painter_at(r);
    let rad = th.radius("md");
    p.rect_filled(r, rad, th.c("bg.app"));
    p.rect_stroke(r, rad, Stroke::new(1.0, th.c("border.subtle")), StrokeKind::Inside);
    let base = Layout::capture(prefs);
    let l = primary.map_or(base.clone(), |f| f.layout(&base));
    let g = space::XS;
    let inner = r.shrink(space::SM);
    let rib_h = match looks_ui::ribbon_now(prefs) {
        RibbonLook::Full => 30.0,
        RibbonLook::Toolbar => 18.0,
        RibbonLook::Tabs => 10.0,
    };
    let rib = Rect::from_min_size(inner.min, Vec2::new(inner.width(), rib_h));
    let mut body = Rect::from_min_max(Pos2::new(inner.min.x, rib.max.y + g), inner.max);
    let mut boxes: Vec<(Rect, String, bool)> = vec![(rib, c.t("onb.map.ribbon"), false)];
    let side = (inner.width() * 0.24).min(150.0);
    if l.browser_open {
        let b = Rect::from_min_max(body.min, Pos2::new(body.min.x + side, body.max.y));
        boxes.push((b, c.t("onb.map.browser"), false));
        body.min.x = b.max.x + g;
    }
    if l.ai_panel_open {
        let b = Rect::from_min_max(Pos2::new(body.max.x - side * 1.2, body.min.y), body.max);
        boxes.push((b, c.t("onb.map.ai"), true));
        body.max.x = b.min.x - g;
    }
    if l.props_open {
        let b = Rect::from_min_max(Pos2::new(body.max.x - side, body.min.y), body.max);
        boxes.push((b, c.t("onb.map.props"), false));
        body.max.x = b.min.x - g;
    }
    if l.table_height > 0.0 {
        let b = Rect::from_min_max(Pos2::new(body.min.x, body.max.y - body.height() * 0.42), body.max);
        boxes.push((b, c.t("onb.map.table"), true));
        body.max.y = b.min.y - g;
    }
    let view = match primary {
        Some(Focus::Model) => "onb.map.view3d",
        Some(Focus::Drawings) => "onb.map.sheet",
        _ => "onb.map.plan",
    };
    boxes.push((body, c.t(view), true));
    for (b, name, strong) in boxes {
        let fill = if strong { th.c("accent.subtle") } else { th.c("bg.panel") };
        p.rect_filled(b, th.radius("sm"), fill);
        p.rect_stroke(b, th.radius("sm"), Stroke::new(1.0, th.c("border.subtle")), StrokeKind::Inside);
        let gl = text_line(ui, th, Role::Caption, &name, b.width() - 2.0 * space::XS, "text.primary");
        if gl.size().y <= b.height() {
            p.galley(b.center() - gl.size() / 2.0, gl, th.c("text.primary"));
        }
    }
}

fn h_open(a: &mut EditorApp) {
    a.open_onboarding(STEP_FOCUS);
}

fn focus_cmd(a: &mut EditorApp, f: Focus) {
    let mut list = a.focus();
    list.retain(|o| *o != f);
    list.insert(0, f);
    a.set_focus(None, &list);
}

/// Every palette command of the wizard.
pub const COMMANDS: &[&str] = &[
    "ui.onboarding",
    "ui.focus.ai",
    "ui.focus.drawings",
    "ui.focus.model",
    "ui.focus.data",
];

pub fn register(app: &mut EditorApp) {
    app.register_command("ui.onboarding", h_open);
    app.register_command("ui.focus.ai", |a| focus_cmd(a, Focus::Ai));
    app.register_command("ui.focus.drawings", |a| focus_cmd(a, Focus::Drawings));
    app.register_command("ui.focus.model", |a| focus_cmd(a, Focus::Model));
    app.register_command("ui.focus.data", |a| focus_cmd(a, Focus::Data));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_values_round_trip_and_order_tabs() {
        assert_eq!(clean_focus("data, ai,nope,data"), "data,ai");
        assert_eq!(focus_list(""), vec![]);
        let f = [Focus::Drawings, Focus::Model];
        assert_eq!(focus_list(&focus_text(&f)), f);
        assert_eq!(tab_order(&f), ["annotate", "view", "arch", "struct", "systems", "modify"]);
        let tabs: Vec<String> = ["arch", "struct", "annotate", "view", "data"].map(String::from).into();
        let o = ddc_ui::widgets::ribbon::ordered_tabs(tabs, &tab_order(&[Focus::Data, Focus::Drawings]));
        assert_eq!(o, ["data", "annotate", "view", "arch", "struct"]);
    }

    #[test]
    fn every_strip_icon_exists() {
        for i in STRIP {
            assert!(ddc_ui::icons::names().contains(&i), "{i}");
        }
    }
}
