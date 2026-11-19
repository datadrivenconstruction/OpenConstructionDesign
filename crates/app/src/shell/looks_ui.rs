// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Looks in the app: the chosen look and its fine controls reach the theme every frame (live,
//! no restart), a preset fills every control at once, and each option is a palette command.
use super::fold::RibbonMode;
use super::EditorApp;
use ddc_ui::looks::{self, Look, RibbonLook};
use ddc_ui::theme::{parse_hex, space, Mode, Role, UiFont, SCALE_STEPS};
use ddc_ui::Theme;
use egui::{self, Color32, Rect, Sense, Stroke, StrokeKind, Vec2};

/// Names of `looks::accents()`, same order (`theme.accent.<name>`).
pub const ACCENT_NAMES: [&str; 8] = [
    "blue", "teal", "violet", "orange", "rose", "green", "azure", "graphite",
];

/// Text sizes offered, factors over the type roles.
pub const TEXT_SIZES: [f32; 5] = [0.9, 1.0, 1.08, 1.15, 1.2];

/// UI scale steps inside the settings range (90–150 %).
pub fn scale_steps() -> Vec<f32> {
    SCALE_STEPS
        .iter()
        .copied()
        .filter(|s| (looks::SCALE_MIN - 1e-3..=looks::SCALE_MAX + 1e-3).contains(s))
        .collect()
}

/// The ribbon a look's key stands for.
pub fn ribbon_of(r: RibbonLook) -> (RibbonMode, bool) {
    match r {
        RibbonLook::Full => (RibbonMode::Full, false),
        RibbonLook::Tabs => (RibbonMode::Tabs, false),
        RibbonLook::Toolbar => (RibbonMode::Full, true),
    }
}

/// The ribbon on screen as a look key; the hidden ribbon reads as tabs.
pub fn ribbon_now(p: &crate::prefs::Prefs) -> RibbonLook {
    match RibbonMode::of(p) {
        RibbonMode::Full if p.ribbon_small => RibbonLook::Toolbar,
        RibbonMode::Full => RibbonLook::Full,
        _ => RibbonLook::Tabs,
    }
}

impl EditorApp {
    /// The look currently chosen (the default for an unknown id).
    pub(crate) fn look(&self) -> &'static Look {
        looks::preset(&self.prefs.look).unwrap_or_else(looks::default_look)
    }

    /// Prefs → theme and icons; cheap when nothing changed.
    pub(crate) fn apply_look(&mut self) {
        let look = self.look();
        let accent = parse_hex(&self.prefs.accent);
        let p = looks::patches_cached(look, accent);
        let th = &mut self.sh.theme;
        th.set_palette(&p.0, &p.1);
        th.font = UiFont::parse(&self.prefs.font).unwrap_or_else(|| look.face());
        th.text_scale = self.prefs.text_size;
        th.radius_scale *= look.radius_scale;
        let set = ddc_ui::icons::set(&self.prefs.icon_set)
            .or_else(|| ddc_ui::icons::set(&look.icons))
            .map(|s| s.style())
            .unwrap_or_else(ddc_ui::icons::style);
        self.sh.icons.set_style(set);
        let order = super::onboard::tab_order(&self.focus());
        if self.sh.ribbon.tab_order != order {
            self.sh.ribbon.tab_order = order;
        }
    }

    /// Every control from preset `id`, on screen at once, saved.
    pub fn use_look(&mut self, id: &str) {
        let Some(l) = looks::preset(id) else {
            return;
        };
        let p = &mut self.prefs;
        p.look = l.id.clone();
        p.theme = l.theme.clone();
        p.accent.clear();
        p.font = l.font.clone();
        p.text_size = l.text_size;
        p.ui_scale = l.ui_scale;
        p.icon_set = l.icons.clone();
        p.density = l.density.clone();
        self.set_ribbon_look(l.ribbon_look());
        self.sh.theme.set_scale(self.prefs.ui_scale);
        self.prefs.save();
    }

    pub(crate) fn set_ribbon_look(&mut self, r: RibbonLook) {
        let (mode, small) = ribbon_of(r);
        mode.store(&mut self.prefs);
        self.prefs.ribbon_small = small;
        self.sh.ribbon.collapsed = self.prefs.ribbon_collapsed;
        self.sh.ribbon.small = small;
    }

    /// The look's own values back, keeping the look.
    pub(crate) fn reset_look(&mut self) {
        let id = self.look().id.clone();
        self.use_look(&id);
    }

    fn set_pref(&mut self, f: impl FnOnce(&mut crate::prefs::Prefs)) {
        f(&mut self.prefs);
        self.sh.theme.set_scale(self.prefs.ui_scale);
        self.prefs.save();
    }
}

/// Palette of the theme a card previews: the look's own theme, else the one on screen.
fn preview_mode(look: &Look, now: Mode) -> Mode {
    match look.theme.as_str() {
        "dark" => Mode::Dark,
        "light" => Mode::Light,
        _ => now,
    }
}

/// Cards of the presets, as many per row as fit. Returns the id clicked.
pub fn gallery(ui: &mut egui::Ui, th: &Theme, cat: &ddc_ui::Catalog, current: &str) -> Option<String> {
    gallery_sized(ui, th, cat, current, 120.0, 58.0)
}

/// As `gallery`, cards at least `min_w` wide with previews `preview_h` high.
pub fn gallery_sized(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &ddc_ui::Catalog,
    current: &str,
    min_w: f32,
    preview_h: f32,
) -> Option<String> {
    let gap = space::SM;
    let all = looks::presets();
    let avail = ui.available_width().max(min_w);
    let per_row = (((avail + gap) / (min_w + gap)).floor() as usize).clamp(1, all.len());
    let w = ((avail - gap * (per_row as f32 - 1.0)) / per_row as f32).floor();
    let name_h = th.line_h(Role::Body) * th.text_scale.max(1.0) + space::SM;
    let h = preview_h + name_h + space::XS;
    let mut picked = None;
    for row in all.chunks(per_row) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for l in row {
                let name = cat.t(&l.name_key());
                let (r, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
                let resp = resp.on_hover_text(cat.t(&l.desc_key()));
                resp.widget_info(|| {
                    egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, l.id == current, &name)
                });
                if resp.clicked() {
                    picked = Some(l.id.clone());
                }
                card(ui, th, l, r, preview_h, &name, l.id == current, resp.hovered());
            }
        });
        ui.add_space(gap);
    }
    picked
}

#[allow(clippy::too_many_arguments)]
fn card(ui: &egui::Ui, th: &Theme, l: &Look, r: Rect, preview_h: f32, name: &str, on: bool, hover: bool) {
    let p = looks::patches_cached(l, None);
    let mode = preview_mode(l, th.mode);
    let base = looks::base_tokens();
    let patch = if mode == Mode::Dark { &p.1 } else { &p.0 };
    let col = |n: &str| {
        patch
            .get(n)
            .and_then(|v| parse_hex(v))
            .or_else(|| base.color(mode, n).ok())
            .unwrap_or_else(|| th.c("text.disabled"))
    };
    let painter = ui.painter_at(r.expand(2.0));
    let rad = th.radius("md");
    let prev = Rect::from_min_size(r.min, Vec2::new(r.width(), preview_h));
    miniature(&painter, prev, rad, l.radius_scale, &col);
    painter.rect_stroke(prev, rad, Stroke::new(1.0, col("border.subtle")), StrokeKind::Inside);
    let ring = if on {
        Stroke::new(2.0, th.c("accent"))
    } else if hover {
        Stroke::new(1.0, th.c("border.strong"))
    } else {
        Stroke::new(1.0, th.c("border.subtle"))
    };
    painter.rect_stroke(r, rad, ring, StrokeKind::Inside);
    let mut job = egui::text::LayoutJob::simple_singleline(
        name.to_string(),
        th.font(if on { Role::BodyStrong } else { Role::Body }),
        th.c("text.primary"),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(r.width() - 2.0 * space::SM);
    let g = ui.fonts(|f| f.layout_job(job));
    let ty = prev.max.y + (r.max.y - prev.max.y - g.size().y) / 2.0;
    painter.galley(egui::pos2(r.min.x + space::SM, ty), g, th.c("text.primary"));
}

/// The app in miniature in `prev`: ribbon strip, browser rows, canvas, an accent button;
/// sizes follow the preview height.
pub(crate) fn miniature(
    painter: &egui::Painter,
    prev: Rect,
    rad: f32,
    radius_scale: f32,
    col: &dyn Fn(&str) -> Color32,
) {
    let u = (prev.height() / 58.0).max(0.5);
    let k = radius_scale.clamp(0.0, 2.0);
    painter.rect_filled(prev, rad, col("bg.app"));
    let inner = prev.shrink(6.0 * u);
    let rib = Rect::from_min_size(inner.min, Vec2::new(inner.width(), 10.0 * u));
    painter.rect_filled(rib, 2.0 * k * u, col("bg.panel"));
    let side = Rect::from_min_max(
        egui::pos2(inner.min.x, rib.max.y + 4.0 * u),
        egui::pos2(inner.min.x + inner.width() * 0.3, inner.max.y),
    );
    painter.rect_filled(side, 2.0 * k * u, col("bg.panel"));
    for (i, c) in ["text.primary", "text.secondary", "text.secondary"].iter().enumerate() {
        let y = side.min.y + (5.0 + i as f32 * 7.0) * u;
        let lw = side.width() * if i == 0 { 0.7 } else { 0.5 };
        let bar = Rect::from_min_size(
            egui::pos2(side.min.x + 4.0 * u, y),
            Vec2::new(lw - 4.0 * u, 3.0 * u),
        );
        painter.rect_filled(bar, u, col(c));
    }
    let canvas = Rect::from_min_max(egui::pos2(side.max.x + 4.0 * u, side.min.y), inner.max);
    painter.rect_filled(canvas, 2.0 * k * u, col("bg.viewport"));
    let btn = Rect::from_min_size(
        canvas.right_bottom() - Vec2::new(30.0, 12.0) * u,
        Vec2::new(26.0, 9.0) * u,
    );
    painter.rect_filled(btn, 4.5 * k * u, col("accent"));
}

/// Accent swatches; returns the chosen colour (None in the outer option: the look's own).
pub fn swatches(ui: &mut egui::Ui, th: &Theme, cat: &ddc_ui::Catalog, cur: &str) -> Option<Option<Color32>> {
    let d = 20.0;
    let mut out = None;
    let cur = parse_hex(cur);
    for (c, name) in looks::accents().into_iter().zip(ACCENT_NAMES).rev() {
        let (r, resp) = ui.allocate_exact_size(Vec2::splat(d), Sense::click());
        let label = cat.t(&format!("theme.accent.{name}"));
        let resp = resp.on_hover_text(&label);
        let on = cur == Some(c);
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, on, &label));
        let p = ui.painter();
        p.circle_filled(r.center(), d / 2.0 - 2.0, c);
        if on {
            p.circle_stroke(r.center(), d / 2.0 - 0.5, Stroke::new(1.5, th.c("text.primary")));
        } else if resp.hovered() {
            p.circle_stroke(r.center(), d / 2.0 - 0.5, Stroke::new(1.0, th.c("border.strong")));
        }
        if resp.clicked() {
            out = Some(Some(c));
        }
    }
    out
}

macro_rules! cmds {
    ($($f:ident => $id:literal, |$a:ident| $body:expr;)*) => {
        $(fn $f($a: &mut EditorApp) { $body; })*
        /// Every palette command of the appearance settings.
        pub const COMMANDS: &[&str] = &[$($id),*];
        pub fn register(app: &mut EditorApp) {
            $(app.register_command($id, $f);)*
        }
    };
}

fn accent(app: &mut EditorApp, i: usize) {
    if let Some(c) = looks::accents().get(i) {
        let h = looks::hex(*c);
        app.set_pref(|p| p.accent = h);
    }
}

fn icons(app: &mut EditorApp, id: &'static str) {
    app.set_pref(|p| p.icon_set = id.into());
}

fn text_step(app: &mut EditorApp, up: bool) {
    let cur = app.prefs.text_size;
    let i = TEXT_SIZES
        .iter()
        .position(|s| (s - cur).abs() < 1e-3)
        .unwrap_or(1);
    let j = if up { (i + 1).min(TEXT_SIZES.len() - 1) } else { i.saturating_sub(1) };
    app.set_pref(|p| p.text_size = TEXT_SIZES[j]);
}

cmds! {
    h_open => "ui.appearance", |a| a.open_settings(super::settings_win::PAGE_APPEARANCE);
    h_look_studio => "ui.look.studio", |a| a.use_look("studio");
    h_look_engineer => "ui.look.engineer", |a| a.use_look("engineer");
    h_look_night => "ui.look.night", |a| a.use_look("night");
    h_look_focus => "ui.look.focus", |a| a.use_look("focus");
    h_look_contrast => "ui.look.contrast", |a| a.use_look("contrast");
    h_look_colour => "ui.look.colour", |a| a.use_look("colour");
    h_look_classic => "ui.look.classic", |a| a.use_look("classic");
    h_look_reset => "ui.look.reset", |a| a.reset_look();
    h_theme_light => "ui.theme.light", |a| a.set_pref(|p| p.theme = "light".into());
    h_theme_dark => "ui.theme.dark", |a| a.set_pref(|p| p.theme = "dark".into());
    h_theme_auto => "ui.theme.auto", |a| a.set_pref(|p| p.theme = "system".into());
    h_accent_0 => "ui.accent.blue", |a| accent(a, 0);
    h_accent_1 => "ui.accent.teal", |a| accent(a, 1);
    h_accent_2 => "ui.accent.violet", |a| accent(a, 2);
    h_accent_3 => "ui.accent.orange", |a| accent(a, 3);
    h_accent_4 => "ui.accent.rose", |a| accent(a, 4);
    h_accent_5 => "ui.accent.green", |a| accent(a, 5);
    h_accent_6 => "ui.accent.azure", |a| accent(a, 6);
    h_accent_7 => "ui.accent.graphite", |a| accent(a, 7);
    h_accent_reset => "ui.accent.reset", |a| a.set_pref(|p| p.accent.clear());
    h_font_inter => "ui.font.inter", |a| a.set_pref(|p| p.font = "inter".into());
    h_font_plex => "ui.font.plex", |a| a.set_pref(|p| p.font = "plex".into());
    h_font_system => "ui.font.system", |a| a.set_pref(|p| p.font = "system".into());
    h_text_up => "ui.text.larger", |a| text_step(a, true);
    h_text_down => "ui.text.smaller", |a| text_step(a, false);
    h_text_reset => "ui.text.reset", |a| a.set_pref(|p| p.text_size = 1.0);
    h_scale_reset => "ui.scale.reset", |a| a.set_pref(|p| p.ui_scale = 1.0);
    h_density_compact => "ui.density.compact", |a| a.set_pref(|p| p.density = "compact".into());
    h_density_normal => "ui.density.normal", |a| a.set_pref(|p| p.density = "normal".into());
    h_density_comfortable => "ui.density.comfortable", |a| a.set_pref(|p| p.density = "comfortable".into());
    h_density_auto => "ui.density.auto", |a| a.set_pref(|p| p.density = "auto".into());
    h_icons_ribbon => "ui.icons.ribbon", |a| icons(a, "ribbon");
    h_icons_studio => "ui.icons.studio", |a| icons(a, "studio");
    h_icons_outline => "ui.icons.outline", |a| icons(a, "outline");
    h_icons_duotone => "ui.icons.duotone", |a| icons(a, "duotone");
    h_icons_filled => "ui.icons.filled", |a| icons(a, "filled");
    h_icons_outline_duotone => "ui.icons.outline_duotone", |a| icons(a, "outline_duotone");
    h_icons_duotone_all => "ui.icons.duotone_all", |a| icons(a, "duotone_all");
    h_icons_filled_outline => "ui.icons.filled_outline", |a| icons(a, "filled_outline");
    h_icons_color => "ui.icons.color", |a| icons(a, "color");
    h_icons_hierarchical => "ui.icons.hierarchical", |a| icons(a, "hierarchical");
    h_icons_isometric => "ui.icons.isometric", |a| icons(a, "isometric");
    h_icons_monoline => "ui.icons.monoline", |a| icons(a, "monoline");
    h_ribbon_full => "ui.ribbon.full", |a| { a.set_ribbon_look(RibbonLook::Full); a.prefs.save() };
    h_ribbon_tabs => "ui.ribbon.tabs", |a| { a.set_ribbon_look(RibbonLook::Tabs); a.prefs.save() };
    h_ribbon_toolbar => "ui.ribbon.toolbar", |a| { a.set_ribbon_look(RibbonLook::Toolbar); a.prefs.save() };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_sets_line_up() {
        assert_eq!(looks::accents().len(), ACCENT_NAMES.len());
        let sets: Vec<&str> = ddc_ui::icons::sets().iter().map(|s| s.id).collect();
        for s in &sets {
            assert!(COMMANDS.contains(&format!("ui.icons.{s}").as_str()), "{s}");
        }
        for l in looks::presets() {
            assert!(COMMANDS.contains(&format!("ui.look.{}", l.id).as_str()), "{}", l.id);
        }
        for n in ACCENT_NAMES {
            assert!(COMMANDS.contains(&format!("ui.accent.{n}").as_str()), "{n}");
        }
        assert_eq!(scale_steps(), [0.9, 1.0, 1.1, 1.25, 1.5]);
        for r in RibbonLook::ALL {
            let (m, s) = ribbon_of(r);
            let mut p = crate::prefs::Prefs::default();
            m.store(&mut p);
            p.ribbon_small = s;
            assert_eq!(ribbon_now(&p), r);
        }
    }
}
