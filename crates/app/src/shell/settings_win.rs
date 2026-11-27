// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Settings window: global options, by page, with search over every page. Changes apply
//! at once and are saved when the window has changed something. One project value sits on the
//! AI page: the open project's design depth, written to the model as an undo step.
mod branding;

use super::{licenses_win, looks_ui, EditorApp};
use ddc_ui::theme::UiFont;
use crate::units::Units;
use ddc_ui::theme::{space, Role, SCALE_STEPS};
const ICON_SM: f32 = ddc_ui::theme::icon_size::SM;
use ddc_ui::widgets::{self, shell_kit};
use ddc_ui::{Catalog, Lang, Theme};
use egui::{self, Align, Layout, RichText, Sense, UiBuilder, Vec2};

/// Share of the body kept for the control side of a row; the label column takes the rest.
/// A share, not a fixed width: the window grows to the workspace size, its controls with it.
const CONTROL_SHARE: f32 = 0.6;
/// Text field next to a button inside a control side.
const FIELD_W: f32 = 220.0;
/// Narrowest page list.
const NAV_W: f32 = 200.0;

/// (catalog key of the page title, icon); the index is `settings_page`.
pub(crate) const PAGES: [(&str, &str); 11] = [
    ("settings.page.appearance", "shading"),
    ("settings.page.interface", "maximize"),
    ("settings.page.language", "globe"),
    ("settings.page.units", "units"),
    ("settings.page.autosave", "record"),
    ("settings.page.keymap", "keyboard"),
    ("settings.page.performance", "3d-view"),
    ("settings.page.ai", "spark"),
    ("settings.page.library", "library"),
    ("settings.page.branding", "palette"),
    ("settings.page.about", "why"),
];

#[derive(Default)]
pub struct SettingsUi {
    pub query: String,
    /// «Reset panels» is applied where the egui context is at hand.
    pub reset_panels: bool,
    was_open: bool,
    /// Name of the ready AI connection, read when the window opens; None: not connected.
    pub(crate) ai_name: Option<String>,
    /// An imported layout for the current mode, put on screen where the context is at hand.
    pub(crate) apply_layout: Option<super::mode_layout::Layout>,
    /// A saved layout picked on the interface page, applied the same way.
    pub(crate) use_layout: Option<String>,
    /// «Save layout as…» from the palette: the name field takes the keyboard.
    pub(crate) focus_layout_name: bool,
    /// Preview and state of the company logo page.
    pub(crate) logo: branding::LogoState,
}

/// Index of the appearance and interface pages in `PAGES`.
pub(crate) const PAGE_APPEARANCE: u8 = 0;
pub(crate) const PAGE_INTERFACE: u8 = 1;

/// What a page asks for after the modal is drawn.
enum Out {
    Run(&'static str),
    Keys,
    Layout(String),
    Units(Units),
    Lang,
    Scale,
    Degrade,
    PickFolder,
    Copied,
    Reveal(std::path::PathBuf),
    Assoc(bool),
    AiPanel,
    Look(String),
    LookReset,
    Ribbon(ddc_ui::looks::RibbonLook),
    SaveLayout(String),
    UseLayout(String),
    /// The open project's design depth: one undo step in the model.
    Depth(ddc_model::DepthSettings),
    LogoPick,
    LogoRemove,
    LogoDefault(bool),
}

/// Name shown for the active AI connection: the stored choice (else the first profile) once it
/// names an address and a model, as the panel counts it; None reads "Not connected".
fn ai_status(list: &[ddc_ai::profile::Profile], active: &str) -> Option<String> {
    let p = list.iter().find(|p| p.id == active).or(list.first())?;
    if p.base_url.trim().is_empty() || p.model.trim().is_empty() {
        return None;
    }
    let name = if p.name.trim().is_empty() { &p.model } else { &p.name };
    Some(name.trim().to_string())
}

/// Primary language of the UI from the Windows language id.
pub fn lang_from_id(id: u16) -> Lang {
    if id & 0x3ff == 0x19 {
        Lang::Ru
    } else {
        Lang::En
    }
}

/// OS language, read once per run: every language decision after startup uses this value.
pub fn system_lang() -> Lang {
    static SYS: std::sync::OnceLock<Lang> = std::sync::OnceLock::new();
    *SYS.get_or_init(read_system_lang)
}

#[cfg(windows)]
fn read_system_lang() -> Lang {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultLocaleName(name: *mut u16, len: i32) -> i32;
    }
    let mut buf = [0u16; 85];
    // SAFETY: writes at most `len` UTF-16 units into `buf`; returns the length with the NUL, 0 on failure
    let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    if n > 1 {
        // "de-DE", "pt-BR", "es-MX": exact tag first, then the base language
        Lang::from_code(&String::from_utf16_lossy(&buf[..n as usize - 1]))
    } else {
        Lang::En
    }
}

#[cfg(not(windows))]
fn read_system_lang() -> Lang {
    std::env::var("LANG").map_or(Lang::En, |l| Lang::from_code(&l))
}

/// Search hit: every word of the query is somewhere in the text.
pub fn matches(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|w| text.contains(w))
}

/// Walks rows of one or all pages and filters them by the query.
struct Form {
    query: String,
    page: String,
    page_shown: bool,
    rows: usize,
}

impl Form {
    fn new(query: &str) -> Self {
        Self {
            query: query.trim().to_string(),
            page: String::new(),
            page_shown: false,
            rows: 0,
        }
    }

    fn searching(&self) -> bool {
        !self.query.is_empty()
    }

    fn enter(&mut self, page: String) {
        self.page = page;
        self.page_shown = false;
    }

    /// Search results carry the name of their page.
    fn title(&mut self, ui: &mut egui::Ui, th: &Theme) {
        if std::mem::replace(&mut self.page_shown, true) {
            return;
        }
        if self.rows > 0 {
            ui.add_space(space::SM);
        }
        shell_kit::section_title(ui, th, &self.page);
        ui.add_space(space::XS);
    }

    fn hit(&self, label: &str, desc: &str) -> bool {
        matches(&self.query, &format!("{} {label} {desc}", self.page))
    }
}

fn row(
    ui: &mut egui::Ui,
    th: &Theme,
    f: &mut Form,
    label: &str,
    desc: &str,
    add: impl FnOnce(&mut egui::Ui),
) {
    if !f.hit(label, desc) {
        return;
    }
    f.title(ui, th);
    f.rows += 1;
    let control_w = ui.available_width() * CONTROL_SHARE;
    let label_w = (ui.available_width() - control_w - ui.spacing().item_spacing.x).max(0.0);
    let line = th.d().control;
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(label_w);
            // the title's first line is centred in a band one control high, the band the
            // controls at the right are centred in; a long title wraps below that line
            let font = th.font(Role::Body);
            let row_h = ui.fonts(|f| f.row_height(&font));
            ui.add_space(space::centre(line, row_h));
            ui.add(
                egui::Label::new(RichText::new(label).font(font).color(th.c("text.primary")))
                    .wrap(),
            );
            ui.add_space(space::centre(line, row_h));
            if !desc.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(desc)
                            .font(th.font(Role::Caption))
                            .color(th.c("text.secondary")),
                    )
                    .wrap(),
                );
            }
        });
        // controls sit on the right edge; the first one added is the rightmost
        // the band is one control high and centres what it holds: a field, a button, a
        // toggle or a plain value all read on the title's line; taller content grows down
        let rest = ui.available_rect_before_wrap();
        let band = egui::Rect::from_min_size(rest.min, egui::vec2(rest.width(), line));
        let mut side = ui.new_child(
            UiBuilder::new()
                .max_rect(band)
                .layout(Layout::right_to_left(Align::Center)),
        );
        side.set_min_height(line);
        side.spacing_mut().interact_size.y = line;
        add(&mut side);
        ui.allocate_rect(side.min_rect(), Sense::hover());
    });
    ui.add_space(space::MD);
}

/// Catalog keys of one settings row: label and the one-line description under it.
#[derive(Clone, Copy)]
struct K(&'static str, &'static str);

const THEME: K = K("settings.theme", "settings.theme_desc");
const SCALE: K = K("settings.scale", "settings.scale_desc");
const DENSITY: K = K("settings.density", "settings.density_desc");
const RIBBON: K = K("theme.ribbon", "theme.ribbon_desc");
const UI_MODE: K = K("ui_mode.title", "ui_mode.desc");
const LOOK: K = K("theme.looks", "theme.looks_desc");
const ACCENT: K = K("theme.accent", "theme.accent_desc");
const FONT: K = K("theme.font", "theme.font_desc");
const TEXT_SIZE: K = K("theme.text_size", "theme.text_size_desc");
const ICONS: K = K("theme.icons", "theme.icons_desc");
const LOOK_RESET: K = K("theme.look_reset", "theme.look_reset_desc");
const ONBOARD: K = K("onb.settings", "onb.settings_desc");
const MODE_LAYOUT: K = K("theme.layout", "theme.layout_desc");
const LAYOUT_SAVE: K = K("theme.layout.save_as", "theme.layout.save_as_desc");
const LAYOUT_SAVED: K = K("theme.layout.saved", "theme.layout.saved_desc");
const LAYOUT_FILE: K = K("theme.layout.file", "theme.layout.file_desc");
const XLSX: K = K("settings.spreadsheet", "settings.spreadsheet_desc");
const AXES: K = K("settings.axes_full", "settings.axes_full_desc");
const START: K = K("settings.start_screen", "settings.start_screen_desc");
const RESET_PANELS: K = K("settings.reset_panels", "settings.reset_panels_desc");
const SNAP: K = K("snap_refine.settings.snap_px", "snap_refine.settings.snap_px_desc");
const LANG: K = K("settings.lang", "settings.lang_desc");
const UNITS: K = K("settings.units", "settings.units_desc");
const AREA_DEC: K = K("settings.area_decimals", "settings.area_decimals_desc");
const VOL_DEC: K = K("settings.volume_decimals", "settings.volume_decimals_desc");
const FRAC: K = K("settings.frac", "settings.frac_desc");
const SMALL_AREA: K = K("settings.small_area", "settings.small_area_desc");
const SAMPLE: K = K("settings.sample", "settings.sample_desc");
const AUTOSAVE: K = K("autosave.title", "autosave.title_desc");
const KEYS: K = K("settings.keys", "settings.keys_desc");
const GFX: K = K("settings.gfx_level", "settings.gfx_level_desc");
const MESH: K = K("settings.mesh_mem", "settings.mesh_mem_desc");
const BACKEND: K = K("settings.backend", "settings.backend_desc");
const AI: K = K("settings.ai_status", "settings.ai_status_desc");
const AI_CONNECT: K = K("settings.ai_connect", "settings.ai_connect_desc");
const AI_PANEL: K = K("settings.ai_panel", "settings.ai_panel_desc");
const DEPTH: K = K("depth.settings", "depth.settings_desc");
const BRAND: K = K("start.brand", "settings.about_desc");
const NEWS: K = K("start.news", "settings.news_desc");
const BUILD: K = K("about.build", "about.copy_desc");
const LICENSES: K = K("about.licenses", "about.licenses_desc");
const UPDATES: K = K("update_check.title", "update_check.desc");
const ASSOC: K = K("open_file.assoc.title", "open_file.assoc.desc");
const ASSOC_DEFAULT: K = K("open_file.assoc.default_models", "settings.assoc_default_desc");
const DATA_DIR: K = K("settings.data_dir", "settings.data_dir_desc");
const LIB_OPEN: K = K("settings.lib_open", "settings.lib_open_desc");
const LIB_DIR: K = K("settings.lib_dir", "settings.lib_dir_desc");

/// Every row above; the keys test walks this list.
#[cfg(test)]
const ALL_ROWS: &[K] = &[
    LOOK,
    ACCENT,
    FONT,
    TEXT_SIZE,
    ICONS,
    LOOK_RESET,
    ONBOARD,
    MODE_LAYOUT,
    LAYOUT_SAVE,
    LAYOUT_SAVED,
    LAYOUT_FILE,
    DEPTH,
    BUILD,
    LICENSES,
    THEME,
    SCALE,
    DENSITY,
    UI_MODE,
    RIBBON,
    XLSX,
    AXES,
    START,
    RESET_PANELS,
    SNAP,
    LANG,
    UNITS,
    AREA_DEC,
    VOL_DEC,
    FRAC,
    SMALL_AREA,
    SAMPLE,
    AUTOSAVE,
    KEYS,
    GFX,
    MESH,
    BACKEND,
    AI,
    AI_CONNECT,
    AI_PANEL,
    BRAND,
    NEWS,
    UPDATES,
    ASSOC,
    ASSOC_DEFAULT,
    DATA_DIR,
    LIB_OPEN,
    LIB_DIR,
    K("settings.keys", "settings.keys_conflicts"),
    K("settings.gfx_level", "settings.gfx.auto.desc"),
];

/// Row with fixed label and description texts.
fn row_k(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &Catalog,
    f: &mut Form,
    k: K,
    add: impl FnOnce(&mut egui::Ui),
) {
    row(ui, th, f, &c.t(k.0), &c.t(k.1), add);
}

/// Dropdown over `items`; true when the choice moved.
fn drop_down(
    ui: &mut egui::Ui,
    th: &Theme,
    id: &str,
    name: &str,
    sel: &mut usize,
    items: &[String],
) -> bool {
    let v: Vec<&str> = items.iter().map(String::as_str).collect();
    let r = widgets::dropdown(ui, th, id, sel, &v, None);
    // the list is named by its row, so a screen reader says what it picks
    ui.ctx().accesskit_node_builder(r.id, |n| n.set_label(name));
    r.changed()
}

fn seg(ui: &mut egui::Ui, th: &Theme, labels: &[String], sel: usize) -> Option<usize> {
    let v: Vec<&str> = labels.iter().map(String::as_str).collect();
    widgets::segmented(ui, th, &v, sel)
}

fn pick_str(ui: &mut egui::Ui, th: &Theme, c: &Catalog, cur: &mut String, opts: &[(&str, &str)]) {
    let labels: Vec<String> = opts.iter().map(|(_, k)| c.t(k)).collect();
    let sel = opts
        .iter()
        .position(|(v, _)| *v == cur.as_str())
        .unwrap_or(0);
    if let Some(i) = seg(ui, th, &labels, sel) {
        *cur = opts[i].0.to_string();
    }
}

fn appearance(app: &mut EditorApp, ui: &mut egui::Ui, f: &mut Form, out: &mut Vec<Out>) {
    if f.hit(&app.sh.cat.t(LOOK.0), &app.sh.cat.t(LOOK.1)) {
        f.title(ui, &app.sh.theme);
        f.rows += 1;
        let th = &app.sh.theme;
        ui.label(
            RichText::new(app.sh.cat.t(LOOK.0))
                .font(th.font(Role::Body))
                .color(th.c("text.primary")),
        );
        ui.add(
            egui::Label::new(
                RichText::new(app.sh.cat.t(LOOK.1))
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
            )
            .wrap(),
        );
        ui.add_space(space::SM);
        if let Some(id) = looks_ui::gallery(ui, th, &app.sh.cat, &app.prefs.look) {
            out.push(Out::Look(id));
        }
        ui.add_space(space::SM);
    }
    let th = &app.sh.theme;
    let c = &app.sh.cat;
    let p = &mut app.prefs;
    row_k(ui, th, c, f, THEME, |ui| {
        pick_str(
            ui,
            th,
            c,
            &mut p.theme,
            &[
                ("system", "settings.theme.system"),
                ("light", "settings.theme.light"),
                ("dark", "settings.theme.dark"),
            ],
        );
    });
    row_k(ui, th, c, f, ACCENT, |ui| {
        if widgets::Button::quiet(&c.t("theme.accent.own"))
            .small()
            .show(ui, th)
            .on_hover_text(c.t("theme.accent.own_tip"))
            .clicked()
        {
            p.accent.clear();
        }
        if let Some(Some(col)) = looks_ui::swatches(ui, th, c, &p.accent) {
            p.accent = ddc_ui::looks::hex(col);
        }
    });
    row_k(ui, th, c, f, FONT, |ui| {
        let names = [
            c.t("theme.font.inter"),
            c.t("theme.font.plex"),
            c.t("theme.font.system"),
        ];
        let mut sel = UiFont::ALL
            .iter()
            .position(|x| x.key() == p.font)
            .unwrap_or(0);
        if drop_down(ui, th, "theme.font", &c.t(FONT.0), &mut sel, &names) {
            p.font = UiFont::ALL[sel].key().into();
        }
    });
    row_k(ui, th, c, f, TEXT_SIZE, |ui| {
        let items: Vec<String> = looks_ui::TEXT_SIZES
            .iter()
            .map(|s| format!("{:.0} %", s * 100.0))
            .collect();
        let mut sel = looks_ui::TEXT_SIZES
            .iter()
            .position(|s| (s - p.text_size).abs() < 1e-3)
            .unwrap_or(1);
        if drop_down(ui, th, "theme.text_size", &c.t(TEXT_SIZE.0), &mut sel, &items) {
            p.text_size = looks_ui::TEXT_SIZES[sel];
        }
    });
    row_k(ui, th, c, f, SCALE, |ui| {
        let mut steps = looks_ui::scale_steps();
        // a step outside the range, set with Ctrl+= / Ctrl+-, stays shown as it is
        if !steps.iter().any(|s| (s - p.ui_scale).abs() < 1e-3) {
            steps.push(p.ui_scale);
            steps.sort_by(f32::total_cmp);
        }
        let items: Vec<String> = steps
            .iter()
            .map(|s| format!("{:.0} %", s * 100.0))
            .collect();
        let mut sel = steps
            .iter()
            .position(|s| (s - p.ui_scale).abs() < 1e-3)
            .unwrap_or(1);
        if drop_down(ui, th, "settings.scale", &c.t(SCALE.0), &mut sel, &items) {
            p.ui_scale = steps[sel];
            out.push(Out::Scale);
        }
    });
    row_k(ui, th, c, f, DENSITY, |ui| {
        let keys = ["auto", "compact", "normal", "comfortable"];
        let names: Vec<String> = keys
            .iter()
            .map(|k| c.t(&format!("settings.density.{k}")))
            .collect();
        let mut sel = keys.iter().position(|k| *k == p.density).unwrap_or(0);
        if drop_down(ui, th, "settings.density", &c.t(DENSITY.0), &mut sel, &names) {
            p.density = keys[sel].into();
        }
    });
    row_k(ui, th, c, f, ICONS, |ui| {
        let sets = ddc_ui::icons::sets();
        let names: Vec<String> = sets
            .iter()
            .map(|s| c.t(&format!("theme.icons.{}", s.id)))
            .collect();
        let mut sel = sets.iter().position(|s| s.id == p.icon_set).unwrap_or(0);
        if drop_down(ui, th, "theme.icons", &c.t(ICONS.0), &mut sel, &names) {
            p.icon_set = sets[sel].id.into();
        }
    });
    row_k(ui, th, c, f, UI_MODE, |ui| {
        use super::ui_mode::UiMode;
        let names: Vec<String> = UiMode::ALL.iter().map(|m| c.t(&m.label_key())).collect();
        let mut sel = UiMode::ALL.iter().position(|m| *m == UiMode::of(p)).unwrap_or(0);
        if drop_down(ui, th, "ui_mode", &c.t(UI_MODE.0), &mut sel, &names) {
            p.ui_mode = UiMode::ALL[sel].key().into();
        }
    });
    row_k(ui, th, c, f, RIBBON, |ui| {
        let all = ddc_ui::looks::RibbonLook::ALL;
        let names: Vec<String> = all
            .iter()
            .map(|r| c.t(&format!("theme.ribbon.{}", r.key())))
            .collect();
        let now = looks_ui::ribbon_now(p);
        let mut sel = all.iter().position(|r| *r == now).unwrap_or(0);
        if drop_down(ui, th, "theme.ribbon", &c.t(RIBBON.0), &mut sel, &names) {
            out.push(Out::Ribbon(all[sel]));
        }
    });
    row_k(ui, th, c, f, LOOK_RESET, |ui| {
        if widgets::Button::secondary(&c.t("theme.look_reset_btn"))
            .show(ui, th)
            .clicked()
        {
            out.push(Out::LookReset);
        }
    });
    row_k(ui, th, c, f, ONBOARD, |ui| {
        if widgets::Button::secondary(&c.t("onb.settings_btn"))
            .show(ui, th)
            .clicked()
        {
            out.push(Out::Run("ui.onboarding"));
        }
    });
    row_k(ui, th, c, f, XLSX, |ui| {
        widgets::toggle_bare(
            ui,
            th,
            &mut p.xlsx_open_after,
            &c.t("settings.spreadsheet_open_after"),
        );
    });
    row_k(ui, th, c, f, AXES, |ui| {
        widgets::toggle_bare(ui, th, &mut p.full_axes, &c.t("settings.axes_full_on"));
    });
    row_k(ui, th, c, f, START, |ui| {
        widgets::toggle_bare(ui, th, &mut p.start_screen, &c.t("settings.start_screen_on"));
    });
    #[cfg(feature = "link")]
    row_k(ui, th, c, f, K("erp.settings", "erp.settings_desc"), |ui| {
        widgets::toggle_bare(ui, th, &mut p.erp_link, &c.t("erp.settings_on"));
    });
}

fn interface(app: &mut EditorApp, ui: &mut egui::Ui, f: &mut Form, out: &mut Vec<Out>) {
    if !f.searching() || f.hit(&app.sh.cat.t("settings.layouts"), "") {
        f.title(ui, &app.sh.theme);
        f.rows += 1;
        if let Some(id) = app.interface_page(ui) {
            out.push(Out::Layout(id));
        }
        ui.add_space(space::MD);
    }
    let mode = app
        .chrome
        .work_mode
        .unwrap_or_else(|| super::mode_layout::current(app));
    let th = &app.sh.theme;
    let c = &app.sh.cat;
    let mode_name = c.t(&mode.name_key());
    let label = c.tf("theme.layout_of", &[("mode", &mode_name)]);
    let desc = c.t(MODE_LAYOUT.1);
    row(ui, th, f, &label, &desc, |ui| {
        if widgets::Button::secondary(&c.t("theme.layout.reset"))
            .show(ui, th)
            .clicked()
        {
            out.push(Out::Run("ui.layout.mode.reset"));
        }
    });
    let name = &mut app.chrome.layout_name;
    let focus = std::mem::take(&mut app.chrome.settings.focus_layout_name);
    row_k(ui, th, c, f, LAYOUT_SAVE, |ui| {
        let ok = widgets::Button::secondary(&c.t("theme.layout.save_btn"))
            .show(ui, th)
            .clicked();
        let hint = c.t("theme.layout.name_hint");
        let id = egui::Id::new("theme.layout.name");
        let r = ddc_ui::widgets::field::TextField::new(name)
            .id(id)
            .hint(&hint)
            .labelled_by(id.with("lbl"), &hint)
            .width(FIELD_W)
            .show(ui, th);
        if focus {
            r.response.request_focus();
        }
        let enter = r.response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ok || enter) && !name.trim().is_empty() {
            out.push(Out::SaveLayout(std::mem::take(name)));
        }
    });
    if !app.prefs.mode_layouts.saved.is_empty() {
        let names: Vec<String> = app
            .prefs
            .mode_layouts
            .saved
            .iter()
            .rev()
            .map(|(n, _)| n.clone())
            .collect();
        row_k(ui, th, c, f, LAYOUT_SAVED, |ui| {
            let mut sel = 0;
            let mut items = vec![c.t("theme.layout.pick")];
            items.extend(names.iter().cloned());
            if drop_down(ui, th, "theme.layout.saved", &c.t(LAYOUT_SAVED.0), &mut sel, &items) && sel > 0 {
                out.push(Out::UseLayout(names[sel - 1].clone()));
            }
        });
    }
    row_k(ui, th, c, f, LAYOUT_FILE, |ui| {
        if widgets::Button::secondary(&c.t("theme.layout.export_btn"))
            .show(ui, th)
            .clicked()
        {
            out.push(Out::Run("ui.layout.mode.export"));
        }
        if widgets::Button::secondary(&c.t("theme.layout.import_btn"))
            .show(ui, th)
            .clicked()
        {
            out.push(Out::Run("ui.layout.mode.import"));
        }
    });
    row_k(ui, th, c, f, RESET_PANELS, |ui| {
        if widgets::Button::secondary(&c.t("settings.reset_panels_btn"))
            .show(ui, th)
            .clicked()
        {
            out.push(Out::Run("ui.panels.reset"));
        }
    });
    let p = &mut app.prefs;
    row_k(ui, th, c, f, SNAP, |ui| {
        use crate::snap_refine::{SNAP_PX_MAX, SNAP_PX_MIN};
        let mut v = f64::from(p.snap_px);
        let unit = c.t("snap_refine.settings.px_suffix");
        let range = (f64::from(SNAP_PX_MIN), f64::from(SNAP_PX_MAX));
        let name = c.t(SNAP.0);
        let salt = "ddc.settings.snap_px";
        if crate::ui_kit::int_field(ui, th, c, salt, &name, &mut v, range, unit.trim()) {
            p.snap_px = v as f32;
        }
    });
}

fn language(app: &mut EditorApp, ui: &mut egui::Ui, f: &mut Form, out: &mut Vec<Out>) {
    let th = &app.sh.theme;
    let c = &app.sh.cat;
    let p = &mut app.prefs;
    row_k(ui, th, c, f, LANG, |ui| {
        let before = p.lang.clone();
        // "auto" plus all 45 locales, each named in its own language
        let mut names = vec![c.t("settings.lang.auto")];
        names.extend(Lang::ALL.iter().map(|l| l.native_name().to_string()));
        let mut sel = Lang::ALL
            .iter()
            .position(|l| l.code() == p.lang)
            .map_or(0, |i| i + 1);
        let was = sel;
        drop_down(ui, th, "settings.lang.pick", &c.t(LANG.0), &mut sel, &names);
        if sel != was {
            p.lang = match sel {
                0 => "auto".to_string(),
                i => Lang::ALL[i - 1].code().to_string(),
            };
        }
        if p.lang != before {
            out.push(Out::Lang);
        }
    });
}

fn units(app: &mut EditorApp, ui: &mut egui::Ui, f: &mut Form, out: &mut Vec<Out>) {
    let u = app.ed.tool.units;
    let th = &app.sh.theme;
    let c = &app.sh.cat;
    let p = &mut app.prefs;
    row_k(ui, th, c, f, UNITS, |ui| {
        let labels = [c.t("settings.units.metric"), c.t("settings.units.imperial")];
        let sel = usize::from(u == Units::Imperial);
        if let Some(i) = seg(ui, th, &labels, sel) {
            out.push(Out::Units(if i == 1 {
                Units::Imperial
            } else {
                Units::Metric
            }));
        }
    });
    row_k(ui, th, c, f, AREA_DEC, |ui| {
        let mut v = p.area_decimals as f64;
        let name = c.t(AREA_DEC.0);
        let salt = "ddc.settings.area_dec";
        if crate::ui_kit::int_field(ui, th, c, salt, &name, &mut v, (0.0, 6.0), "") {
            p.area_decimals = v as usize;
        }
    });
    row_k(ui, th, c, f, VOL_DEC, |ui| {
        let mut v = p.volume_decimals as f64;
        let name = c.t(VOL_DEC.0);
        let salt = "ddc.settings.vol_dec";
        if crate::ui_kit::int_field(ui, th, c, salt, &name, &mut v, (0.0, 6.0), "") {
            p.volume_decimals = v as usize;
        }
    });
    row_k(ui, th, c, f, FRAC, |ui| {
        let items: Vec<String> = crate::units::FRAC_DENOMS
            .iter()
            .map(|d| format!("1/{d}"))
            .collect();
        let mut sel = crate::units::FRAC_DENOMS
            .iter()
            .position(|d| *d == p.frac_denominator)
            .unwrap_or(0);
        if drop_down(ui, th, "settings.frac", &c.t(FRAC.0), &mut sel, &items) {
            p.frac_denominator = crate::units::FRAC_DENOMS[sel];
        }
    });
    row_k(ui, th, c, f, SMALL_AREA, |ui| {
        widgets::toggle_bare(ui, th, &mut p.small_area_in2, &c.t("settings.small_area_on"));
    });
    row_k(ui, th, c, f, SAMPLE, |ui| {
        ui.label(
            RichText::new(format!("{} · {}", crate::unit_text::area_ui(12.3456), crate::unit_text::vol_ui(1.23456)))
                .font(th.font(Role::Numeric))
                .color(th.c("text.primary")),
        );
    });
}

fn autosave(app: &mut EditorApp, ui: &mut egui::Ui, f: &mut Form, out: &mut Vec<Out>) {
    let th = &app.sh.theme;
    let c = &app.sh.cat;
    let p = &mut app.prefs;
    let on = p.autosave;
    row_k(ui, th, c, f, AUTOSAVE, |ui| {
        widgets::toggle_bare(ui, th, &mut p.autosave, &c.t("autosave.enable"));
    });
    let mut host = |ui: &mut egui::Ui, keys: [&'static str; 2], add: &mut dyn FnMut(&mut egui::Ui)| {
        row(ui, th, f, &c.t(keys[0]), &c.t(keys[1]), |ui| add(ui));
    };
    let r = crate::ui_kit::autosave_section(ui, th, c, p, on, &mut host);
    if r.browse {
        out.push(Out::PickFolder);
    }
}

fn keymap(app: &mut EditorApp, ui: &mut egui::Ui, f: &mut Form, out: &mut Vec<Out>) {
    let th = &app.sh.theme;
    let c = &app.sh.cat;
    let n = app.quick.bind.conflicts.len();
    let desc = if n > 0 {
        c.tf("settings.keys_conflicts", &[("n", n.to_string().as_str())])
    } else {
        c.t("settings.keys_desc")
    };
    row(ui, th, f, &c.t(KEYS.0), &desc, |ui| {
        if widgets::Button::secondary(&c.t("settings.keys_open"))
            .show(ui, th)
            .clicked()
        {
            out.push(Out::Keys);
        }
    });
}

fn performance(app: &mut EditorApp, ui: &mut egui::Ui, f: &mut Form, out: &mut Vec<Out>) {
    let th = &app.sh.theme;
    let c = &app.sh.cat;
    let p = &mut app.prefs;
    let level_desc = format!(
        "{} {}",
        c.t(&format!("settings.gfx.{}.desc", p.gfx_level)),
        c.t(GFX.1)
    );
    row(ui, th, f, &c.t(GFX.0), &level_desc, |ui| {
        let before = p.gfx_level.clone();
        pick_str(
            ui,
            th,
            c,
            &mut p.gfx_level,
            &[
                ("auto", "settings.gfx.auto"),
                ("speed", "settings.gfx.speed"),
                ("balanced", "settings.gfx.balanced"),
                ("quality", "settings.gfx.quality"),
            ],
        );
        if p.gfx_level != before {
            out.push(Out::Degrade);
        }
    });
    let cache = app.big.cache.bytes() >> 20;
    let (n, cap) = (app.big.cache.len(), crate::bigfile::MESH_BUDGET >> 20);
    row_k(ui, th, c, f, MESH, |ui| {
        ui.label(
            RichText::new(c.tf(
                "settings.mesh_mem_val",
                &[
                    ("used", cache.to_string().as_str()),
                    ("cap", cap.to_string().as_str()),
                    ("n", n.to_string().as_str()),
                ],
            ))
            .font(th.font(Role::Numeric))
            .color(th.c("text.primary")),
        );
    });
    row_k(ui, th, c, f, BACKEND, |ui| {
        ui.label(
            RichText::new(&app.backend)
                .font(th.font(Role::Body))
                .color(th.c("text.secondary")),
        );
    });
}

fn ai(app: &mut EditorApp, ui: &mut egui::Ui, f: &mut Form, out: &mut Vec<Out>) {
    let th = &app.sh.theme;
    let c = &app.sh.cat;
    // only the profile name: addresses and keys stay in the assistant's own settings
    let (text, col) = match &app.chrome.settings.ai_name {
        Some(name) => (name.clone(), "text.primary"),
        None => (c.t("settings.ai_not_connected"), "text.secondary"),
    };
    row_k(ui, th, c, f, AI, |ui| {
        ui.label(RichText::new(text).font(th.font(Role::Body)).color(th.c(col)));
    });
    row_k(ui, th, c, f, AI_CONNECT, |ui| {
        if widgets::Button::secondary(&c.t("settings.ai_connect_btn"))
            .show(ui, th)
            .clicked()
        {
            out.push(Out::Run("ai.settings"));
        }
    });
    row_k(ui, th, c, f, AI_PANEL, |ui| {
        if widgets::Button::secondary(&c.t("settings.ai_open_panel"))
            .show(ui, th)
            .clicked()
        {
            out.push(Out::AiPanel);
        }
    });
    limit_rows(app, ui, f);
    depth_rows(app, ui, f, out);
}

/// Catalog keys of a limit row: `ai_limits.<key>` and `ai_limits.<key>_desc` (the reason).
fn limit_keys(l: &ddc_ai::limits::Limit) -> (String, String) {
    (format!("ai_limits.{}", l.key), format!("ai_limits.{}_desc", l.key))
}

/// One row per setting-tier assistant limit; a value equal to the default is not stored.
fn limit_rows(app: &mut EditorApp, ui: &mut egui::Ui, f: &mut Form) {
    use ddc_ai::limits::{self, Tier, Unit};
    let th = &app.sh.theme;
    let c = &app.sh.cat;
    let p = &mut app.prefs;
    for l in limits::settings() {
        let Tier::Setting { min, max } = l.tier else { continue };
        let (label, desc) = limit_keys(l);
        let (label, desc) = (c.t(&label), c.t(&desc));
        let tip = format!("{desc}\n{}", c.t("ai_limits.default").replace("{n}", &l.value.to_string()));
        let unit = match l.unit {
            Unit::Seconds => c.t("ai_limits.unit_s"),
            Unit::MiB => c.t("ai_limits.unit_mb"),
            _ => String::new(),
        };
        row(ui, th, f, &label, &desc, |ui| {
            let mut v = p.ai_limits.get(l.key).copied().unwrap_or(l.value) as f64;
            let salt = format!("ddc.settings.ai_limit.{}", l.key);
            let range = (min as f64, max as f64);
            let r = ui.scope(|ui| crate::ui_kit::int_field(ui, th, c, &salt, &label, &mut v, range, &unit));
            r.response.on_hover_text(tip);
            if r.inner {
                let n = (v.round() as u64).clamp(min, max);
                if n == l.value {
                    p.ai_limits.remove(l.key);
                } else {
                    p.ai_limits.insert(l.key.to_string(), n);
                }
            }
        });
    }
}

/// The open project's design depth: the summary in the row, the editor under it.
fn depth_rows(app: &EditorApp, ui: &mut egui::Ui, f: &mut Form, out: &mut Vec<Out>) {
    let th = &app.sh.theme;
    let c = &app.sh.cat;
    let mut s = app.ed.model.depth_settings().unwrap_or_default();
    let (label, desc) = (c.t(DEPTH.0), c.t(DEPTH.1));
    let summary = crate::depth_ui::chip_text(c, &s);
    row(ui, th, f, &label, &desc, |ui| {
        ui.add(
            egui::Label::new(
                RichText::new(summary)
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
            )
            .truncate(),
        );
    });
    if !f.hit(&label, &desc) {
        return;
    }
    ui.push_id("settings.depth", |ui| {
        if crate::depth_ui::editor(ui, th, c, &mut s) {
            out.push(Out::Depth(s));
        }
    });
    ui.add_space(space::MD);
}

fn about(app: &mut EditorApp, ui: &mut egui::Ui, f: &mut Form, out: &mut Vec<Out>) {
    let th = &app.sh.theme;
    let c = &app.sh.cat;
    row_k(ui, th, c, f, BRAND, |ui| {
        ui.label(
            RichText::new(format!(
                "{} {}",
                c.t("about.product"),
                env!("CARGO_PKG_VERSION")
            ))
            .font(th.font(Role::Numeric))
            .color(th.c("text.primary")),
        );
    });
    crate::brand::family_block(ui, th, c);
    let info = licenses_win::about_info(app);
    row_k(ui, th, c, f, BUILD, |ui| {
        // right to left: the button is added first so that it ends up last
        if widgets::Button::secondary(&c.t("about.copy"))
            .show(ui, th)
            .clicked()
        {
            ui.ctx().copy_text(licenses_win::copy_text(&info, c));
            out.push(Out::Copied);
        }
        let (commit, date) = licenses_win::build_env();
        ui.label(
            RichText::new(licenses_win::build_label(commit.as_deref(), date.as_deref()))
                .font(th.font(Role::Numeric))
                .color(th.c("text.secondary")),
        );
    });
    if licenses_win::has_licenses() {
        row_k(ui, th, c, f, LICENSES, |ui| {
            if widgets::Button::secondary(&c.t("about.licenses.open"))
                .show(ui, th)
                .clicked()
            {
                out.push(Out::Run(licenses_win::COMMAND));
            }
        });
    }
    row_k(ui, th, c, f, NEWS, |ui| {
        if ui.link(c.t("start.news_all")).clicked() {
            out.push(Out::Run("help.whats_new"));
        }
    });
    {
        let freqs = crate::update::Freq::ALL;
        let labels: Vec<String> = freqs.iter().map(|f| c.t(f.label_key())).collect();
        let cur = crate::update::freq();
        let sel = freqs.iter().position(|f| *f == cur).unwrap_or(0);
        // right to left: the button is added first so that it ends up last
        row_k(ui, th, c, f, UPDATES, |ui| {
            if widgets::Button::secondary(&c.t("update_check.check_now"))
                .show(ui, th)
                .clicked()
            {
                out.push(Out::Run("help.check_update"));
            }
            if let Some(i) = seg(ui, th, &labels, sel) {
                crate::update::set_freq(freqs[i]);
            }
        });
    }
    // a user choice only: nothing is registered unless a button is pressed
    row_k(ui, th, c, f, ASSOC_DEFAULT, |ui| {
        // the row label already says it: the switch alone, named by that label
        widgets::toggle_bare(ui, th, &mut app.open.assoc_defaults, &c.t(ASSOC_DEFAULT.0));
    });
    row_k(ui, th, c, f, ASSOC, |ui| {
        if widgets::Button::secondary(&c.t("open_file.assoc.remove"))
            .show(ui, th)
            .clicked()
        {
            out.push(Out::Assoc(false));
        }
        if widgets::Button::secondary(&c.t("open_file.assoc.register"))
            .show(ui, th)
            .clicked()
        {
            out.push(Out::Assoc(true));
        }
    });
    if let Some(dir) = crate::gfx::data_dir() {
        let desc = format!("{}\n{}", c.t(DATA_DIR.1), dir.display());
        row(ui, th, f, &c.t(DATA_DIR.0), &desc, |ui| {
            if widgets::Button::secondary(&c.t("settings.data_dir_open"))
                .show(ui, th)
                .clicked()
            {
                out.push(Out::Reveal(dir.clone()));
            }
        });
    }
}

/// `--settings-page N|name`: the page index, or a page name (`library`, `ai`, `about`, ...).
/// An unknown name or an index past the last page gives None.
pub fn page_from_args(argv: &[String]) -> Option<u8> {
    let v = argv.get(argv.iter().position(|s| s == "--settings-page")? + 1)?;
    if let Ok(n) = v.parse::<u8>() {
        return (usize::from(n) < PAGES.len()).then_some(n);
    }
    let want = format!("settings.page.{}", v.to_lowercase());
    PAGES.iter().position(|(k, _)| *k == want).map(|i| i as u8)
}

/// Component library: opens the library window and the folder of the user's family files.
fn library(app: &mut EditorApp, ui: &mut egui::Ui, f: &mut Form, out: &mut Vec<Out>) {
    let th = &app.sh.theme;
    let c = &app.sh.cat;
    row_k(ui, th, c, f, LIB_OPEN, |ui| {
        if widgets::Button::secondary(&c.t("complib.browse")).show(ui, th).clicked() {
            out.push(Out::Run("mgr.library"));
        }
    });
    if let Some(dir) = crate::complib::user_dir() {
        let desc = format!("{}
{}", c.t(LIB_DIR.1), dir.display());
        row(ui, th, f, &c.t(LIB_DIR.0), &desc, |ui| {
            if widgets::Button::secondary(&c.t("complib.open_folder")).show(ui, th).clicked() {
                // the folder may not exist yet: opening it creates it
                let _ = std::fs::create_dir_all(&dir);
                out.push(Out::Reveal(dir.clone()));
            }
        });
    }
}

type Page = fn(&mut EditorApp, &mut egui::Ui, &mut Form, &mut Vec<Out>);
const BODIES: [Page; 11] = [
    appearance,
    interface,
    language,
    units,
    autosave,
    keymap,
    performance,
    ai,
    library,
    branding::page,
    about,
];

/// The settings window's width on every page and its height cap: the workspace class, or 90 %
/// of a smaller screen. A shorter page ends the window under its last row.
pub(crate) fn window_size(screen: Vec2) -> (f32, f32) {
    use ddc_ui::theme::layout::{DIALOG_WORKSPACE, DIALOG_WORKSPACE_H};
    let w = DIALOG_WORKSPACE.min(screen.x * 0.9);
    let h = DIALOG_WORKSPACE_H.min(screen.y * 0.9);
    (w.max(320.0), h.max(240.0))
}

/// Body height under a head of `head` px in a window of `total` px with `frame` px of margins.
fn body_height(total: f32, head: f32, frame: f32) -> f32 {
    (total - head - frame).max(120.0)
}

/// The page list is as wide as its longest name plus room for icon and padding, at least
/// `NAV_W`: a page name is never cut.
pub(crate) fn nav_width(widest_label: f32) -> f32 {
    (widest_label + 40.0).max(NAV_W)
}

fn nav_row(
    ui: &mut egui::Ui,
    th: &Theme,
    icons: &mut ddc_ui::icons::IconCache,
    icon: &str,
    text: &str,
    on: bool,
) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
    let fg = widgets::paint_state(ui, th, r, &resp, on);
    let px = ddc_ui::theme::icon_size::MD;
    let ic = egui::Rect::from_center_size(r.left_center() + Vec2::new(16.0, 0.0), Vec2::splat(px));
    icons.paint(ui, &widgets::icon_name(icon, on), px as u32, ic, fg);
    let font = th.font(if on { Role::BodyStrong } else { Role::Body });
    let col = if on {
        th.c("accent")
    } else {
        th.c("text.primary")
    };
    let g = shell_kit::elided(ui, text, font, col, r.width() - 40.0);
    let pos = r.left_center() + Vec2::new(32.0, -g.size().y / 2.0);
    ui.painter().galley(pos, g, col);
    resp
}

impl EditorApp {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let before = self.prefs.clone();
        let mut out: Vec<Out> = vec![];
        let first = !self.chrome.settings.was_open;
        if first {
            self.chrome.settings.logo.reset();
            // plain read: the key store and the address-key migration stay with the panel
            let list = crate::gfx::data_dir()
                .and_then(|d| ddc_ai::profile::load_profiles(&d).ok())
                .unwrap_or_default();
            self.chrome.settings.ai_name = ai_status(&list, &self.prefs.ai_profile);
        }
        let (w, total_h) = window_size(ctx.screen_rect().size());
        let nav_w = {
            let (th, c) = (&self.sh.theme, &self.sh.cat);
            let labels: Vec<String> = PAGES.iter().map(|(k, _)| c.t(k)).collect();
            let font = th.font(Role::BodyStrong);
            let white = egui::Color32::PLACEHOLDER;
            let widest = ctx.fonts(|f| {
                labels
                    .iter()
                    .map(|s| f.layout_no_wrap(s.clone(), font.clone(), white).size().x)
                    .fold(0.0, f32::max)
            });
            nav_width(widest)
        };
        let modal = crate::ui_kit::modal_dialog("settings", ctx, |ui| {
            // one width for every page; the height fits the page up to the class cap
            ui.set_width(w);
            ui.set_max_width(w);
            {
                let th = &self.sh.theme;
                let c = &self.sh.cat;
                let q = &mut self.chrome.settings.query;
                // one row as tall as its tallest item: egui centres each item in the height the
                // row had when it was placed, so a title placed before the taller field sits high
                let title_h = ui.fonts(|f| f.row_height(&th.font(Role::DialogTitle)));
                // the close icon button is the tallest item (control + 4, ui widgets::icon_button)
                let row = egui::vec2(ui.available_width(), (th.d().control + 4.0).max(title_h));
                let centred = egui::Layout::left_to_right(egui::Align::Center);
                ui.allocate_ui_with_layout(row, centred, |ui| {
                    let title = RichText::new(c.t("settings.title"))
                        .font(th.font(Role::DialogTitle))
                        .color(th.c("text.primary"));
                    crate::ui_kit::caption_on_line(ui, title, &th.font(Role::Body));
                    ui.add_space(space::LG);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let tip = c.t("settings.close");
                        let ctx = ui.ctx().clone();
                        let x = widgets::with_icons(&ctx, |icons| {
                            widgets::icon_button(ui, icons, th, "close", false, &tip)
                        });
                        let named = egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &tip);
                        x.widget_info(|| named.clone());
                        if x.on_hover_text("Esc").clicked() {
                            open = false;
                        }
                        // the kit search field: inner padding, glyph, clear; Esc empties it first
                        let (hint, tip) = (c.t("settings.search"), c.t("browser.reset_search"));
                        let id = egui::Id::new("settings.search");
                        let glyph = space::SM;
                        let out = ddc_ui::widgets::field::TextField::new(q)
                            .id(id)
                            .hint(&hint)
                            .labelled_by(id.with("lbl"), &hint)
                            .width(FIELD_W)
                            .search("find", glyph, glyph + ICON_SM + space::SM, &tip)
                            .show(ui, th);
                        if first {
                            out.response.request_focus();
                        }
                    });
                });
            }
            ui.separator();
            ui.add_space(space::SM);
            let query = self.chrome.settings.query.clone();
            let mut form = Form::new(&query);
            let body_w = (w - nav_w - space::MD).max(1.0);
            // the head (title, search, rule) is measured, so the window keeps the class height
            let head = ui.cursor().top() - ui.min_rect().top();
            let h = body_height(total_h, head, ui.style().spacing.window_margin.sum().y);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(nav_w);
                    let s = &mut self.sh;
                    for (i, (key, icon)) in PAGES.iter().enumerate() {
                        let on = !form.searching() && usize::from(self.settings_page) == i;
                        let text = s.cat.t(key);
                        if nav_row(ui, &s.theme, &mut s.icons, icon, &text, on).clicked() {
                            self.settings_page = i as u8;
                            self.chrome.settings.query.clear();
                        }
                    }
                });
                ui.add_space(space::MD);
                ui.vertical(|ui| {
                    ui.set_width(body_w);
                    let th = self.sh.theme.clone();
                    // a short page ends the window under its last row; past the cap it scrolls
                    let shown = egui::ScrollArea::vertical()
                        .id_salt("settings.body")
                        .max_height(h)
                        // a long page scrolls at the class height, not at what the window area
                        // had free on its first frame (that cut the second row of cards)
                        .min_scrolled_height(h)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            // what the bar's gutter leaves, never under the bar
                            ui.set_max_width(ui.available_width().min(body_w));
                            if form.searching() {
                                for (i, body) in BODIES.iter().enumerate() {
                                    form.enter(self.sh.cat.t(PAGES[i].0));
                                    body(self, ui, &mut form, &mut out);
                                }
                                if form.rows == 0 {
                                    let th = &self.sh.theme;
                                    let c = &self.sh.cat;
                                    ui.add_space(space::XL);
                                    ui.vertical_centered(|ui| {
                                        ui.label(
                                            RichText::new(c.t("settings.no_results"))
                                                .color(th.c("text.secondary")),
                                        );
                                        if ui.link(c.t("settings.reset_search")).clicked() {
                                            self.chrome.settings.query.clear();
                                        }
                                    });
                                }
                            } else {
                                let i = usize::from(self.settings_page).min(BODIES.len() - 1);
                                form.enter(self.sh.cat.t(PAGES[i].0));
                                BODIES[i](self, ui, &mut form, &mut out);
                            }
                        });
                    crate::ui_kit::mark_scroll(ui, &shown);
                    // a cut edge shows there is more above or below
                    widgets::scroll_hint::scroll_fade(ui, &th, &shown);
                });
            });
        });
        // Esc under the open palette is the palette's
        if !self.sh.palette.open && modal.should_close() {
            open = false;
        }
        if self.prefs != before {
            self.prefs.apply();
            self.prefs.save();
        }
        for o in out {
            match o {
                Out::Run(id) => {
                    open = false;
                    self.run(id);
                }
                Out::Keys => {
                    open = false;
                    self.quick.keys_open = true;
                }
                Out::AiPanel => {
                    open = false;
                    crate::ai::ui::open_panel(self);
                }
                Out::Depth(s) => {
                    if let Err(e) = crate::depth_ui::apply(&mut self.ed, s) {
                        self.sh
                            .toasts
                            .push(ddc_ui::notify::Level::Warning, e.to_string(), None);
                    }
                }
                Out::LogoPick => branding::pick(self),
                Out::LogoRemove => branding::set(self, None),
                Out::LogoDefault(on) => branding::set_default(self, on),
                Out::Layout(id) => self.set_layout(&id),
                Out::Look(id) => self.use_look(&id),
                Out::LookReset => self.reset_look(),
                Out::Ribbon(r) => {
                    self.set_ribbon_look(r);
                    self.prefs.save();
                }
                Out::SaveLayout(n) => {
                    if let Some(n) = self.mode_layout_save_as(&n) {
                        let lvl = ddc_ui::notify::Level::Info;
                        crate::quick::toast(self, lvl, "theme.layout.saved_as", &[("name", &n)]);
                    }
                }
                Out::UseLayout(n) => self.chrome.settings.use_layout = Some(n),
                Out::Units(u) => self.ed.set_units(u),
                Out::Lang => {
                    self.apply_lang();
                    self.chrome.start.drop_news();
                }
                Out::Scale => self.sh.theme.set_scale(self.prefs.ui_scale),
                Out::Degrade => {
                    let l = ddc_render::PerfLevel::parse(&self.prefs.gfx_level).unwrap_or_default();
                    self.view.gfx.apply_level(l);
                }
                Out::Copied => {
                    let msg = self.sh.cat.t("about.copied");
                    self.sh.toasts.push(ddc_ui::notify::Level::Info, msg, None);
                }
                Out::PickFolder => {
                    if let Some(p) = crate::file_pick::Dlg::new().pick_folder() {
                        self.prefs.autosave_dir = p.display().to_string();
                        self.prefs.save();
                    }
                }
                Out::Assoc(register) => {
                    let req = crate::os_assoc::Req {
                        register,
                        default_models: register && self.open.assoc_defaults,
                    };
                    crate::open::assoc_start(self, req);
                }
                Out::Reveal(dir) => {
                    if open_folder(&dir).is_err() {
                        let text = self.sh.cat.t("save.warn.open_folder");
                        self.sh
                            .toasts
                            .push(ddc_ui::notify::Level::Warning, text, None);
                    }
                }
            }
        }
        self.chrome.settings.was_open = open;
        if !open {
            self.chrome.settings.query.clear();
        }
        self.settings_open = open;
    }
}

/// A picture file dropped on the open window while the branding page is shown.
pub(crate) fn logo_dropped(app: &mut EditorApp, path: std::path::PathBuf) {
    branding::dropped(app, path);
}

/// Pages of the window, for the layout contract.
#[cfg(test)]
pub(crate) const PAGE_COUNT: usize = PAGES.len();

/// Index of the AI assistant page in `PAGES`.
#[cfg(test)]
pub(crate) const AI_PAGE: u8 = 7;

/// Index of the «Project / Branding» page in `PAGES`.
pub(crate) const BRANDING_PAGE: u8 = 9;

/// Index of the component library page in `PAGES` (`--settings-page library`).
pub(crate) const LIBRARY_PAGE: u8 = 8;

/// One frame of the settings window on `page` (`query` filters all pages when not empty).
#[cfg(test)]
pub(crate) fn layout_frame(app: &mut EditorApp, ctx: &egui::Context, page: u8, query: &str) {
    app.settings_page = page;
    app.chrome.settings.query = query.to_string();
    app.settings_window(ctx);
}

fn open_folder(dir: &std::path::Path) -> std::io::Result<()> {
    let tool = if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(tool)
        .arg(dir)
        .spawn()
        .map(|_| ())
}

/// Index of `cur` in `SCALE_STEPS`; the 100 % step for a value off the grid.
fn scale_index(cur: f32) -> usize {
    SCALE_STEPS
        .iter()
        .position(|s| (cur - s).abs() < 0.001)
        .unwrap_or_else(|| {
            SCALE_STEPS
                .iter()
                .position(|s| (s - 1.0).abs() < 0.001)
                .unwrap_or(0)
        })
}

/// Next interface scale from `SCALE_STEPS`; stays put at either end.
pub fn step_scale(cur: f32, up: bool) -> f32 {
    let i = SCALE_STEPS
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1 - cur).abs().total_cmp(&(b.1 - cur).abs()))
        .map_or(0, |(i, _)| i);
    let j = if up {
        (i + 1).min(SCALE_STEPS.len() - 1)
    } else {
        i.saturating_sub(1)
    };
    SCALE_STEPS[j]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_steps_clamp() {
        assert_eq!(step_scale(1.0, true), 1.1);
        assert_eq!(step_scale(1.0, false), 0.9);
        assert_eq!(step_scale(2.0, true), 2.0);
        assert_eq!(step_scale(0.75, false), 0.75);
        assert_eq!(step_scale(1.02, true), 1.1);
    }

    #[test]
    fn search_needs_every_word() {
        assert!(matches("", "anything"));
        let fx = crate::app2_fx::fx;
        let hay = fx("sw_theme_hay");
        assert!(matches(&fx("sw_theme_upper"), &hay));
        assert!(matches(&fx("sw_theme_two_words"), &hay));
        assert!(!matches(&fx("sw_theme_roof"), &hay));
    }

    #[test]
    fn language_from_windows_id() {
        assert_eq!(lang_from_id(0x0419), Lang::Ru);
        assert_eq!(lang_from_id(0x0409), Lang::En);
        assert_eq!(lang_from_id(0x0407), Lang::En);
    }

    fn table_text(lang: Lang, key: &str) -> Option<String> {
        match Catalog::new(lang).table(lang).get(key) {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            _ => None,
        }
    }

    #[test]
    fn settings_rows_have_desc() {
        for lang in [Lang::En, Lang::Ru] {
            for k in ALL_ROWS {
                for key in [k.0, k.1] {
                    let text = table_text(lang, key);
                    assert!(
                        text.is_some_and(|t| !t.trim().is_empty()),
                        "{key} missing in {lang:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_row_const_is_in_the_list() {
        let src = include_str!("settings_win.rs");
        let body = src.split("mod tests {").next().unwrap_or("");
        let mut n = 0;
        for line in body.lines().filter(|l| l.starts_with("const ") && l.contains(": K = K(")) {
            n += 1;
            let args = line.split("K(").nth(1).unwrap_or("");
            let keys: Vec<&str> = args.split('"').skip(1).step_by(2).collect();
            assert_eq!(keys.len(), 2, "{line}");
            assert!(
                ALL_ROWS.iter().any(|k| k.0 == keys[0] && k.1 == keys[1]),
                "{line}"
            );
        }
        // ALL_ROWS adds two composite rows written inline, with no const of their own
        assert_eq!(n, ALL_ROWS.len() - 2);
    }

    #[test]
    fn search_finds_every_row() {
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for k in ALL_ROWS {
                let (label, desc) = (cat.t(k.0), cat.t(k.1));
                assert!(Form::new(&label).hit(&label, &desc), "{}", k.0);
                if let Some(word) = desc.split_whitespace().next() {
                    assert!(Form::new(word).hit(&label, &desc), "{}", k.1);
                }
                assert!(!Form::new("zzqqxx").hit(&label, &desc), "{}", k.0);
            }
        }
    }

    #[test]
    fn page_code_uses_only_kit_widgets_and_tokens() {
        let src = include_str!("settings_win.rs");
        let body = src.split("mod tests {").next().unwrap_or("");
        for raw in [".checkbox(", "ComboBox", "egui::Button", "ui.button(", "selectable_value"] {
            assert!(!body.contains(raw), "raw widget {raw}");
        }
        for line in body.lines() {
            if let Some(i) = line.find("add_space(") {
                let arg = &line[i + "add_space(".len()..];
                assert!(arg.starts_with("space::"), "literal spacing: {line}");
            }
        }
    }

    #[test]
    fn scale_index_maps_steps() {
        assert_eq!(SCALE_STEPS[scale_index(1.25)], 1.25);
        assert_eq!(SCALE_STEPS[scale_index(2.0)], 2.0);
        assert_eq!(SCALE_STEPS[scale_index(1.03)], 1.0);
    }

    #[test]
    fn row_layout_renders_kit_controls_in_both_languages() {
        let th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            let mut form = Form::new("");
            form.enter("page".to_string());
            let (mut on, mut sel) = (true, 1usize);
            let items = vec!["a".to_string(), "b".to_string()];
            ddc_ui::theme::headless_ctx().run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    row_k(ui, &th, &cat, &mut form, RIBBON, |ui| {
                        widgets::toggle(ui, &th, &mut on, "x");
                    });
                    row_k(ui, &th, &cat, &mut form, SCALE, |ui| {
                        drop_down(ui, &th, "t.drop", "Scale", &mut sel, &items);
                    });
                });
            });
            assert_eq!(form.rows, 2);
            assert!(on, "rendering alone changes nothing");
            assert_eq!(sel, 1);
        }
    }

    fn profile(id: &str, name: &str, url: &str, model: &str) -> ddc_ai::profile::Profile {
        let mut p = crate::ai::ui::connect::blank_profile(id.into(), name.into());
        p.base_url = url.into();
        p.model = model.into();
        p
    }

    #[test]
    fn ai_status_names_only_a_ready_profile() {
        assert_eq!(ai_status(&[], "a"), None, "no profiles: not connected");
        let ready = profile("a", "Office", "http://127.0.0.1:8080", "m1");
        let blank = profile("b", "Draft", " ", "m2");
        let list = vec![ready.clone(), blank.clone()];
        assert_eq!(ai_status(&list, "a").as_deref(), Some("Office"));
        assert_eq!(ai_status(&list, "b"), None, "no address: not connected");
        assert_eq!(ai_status(&list, "gone").as_deref(), Some("Office"), "falls back to the first");
        let unnamed = profile("c", " ", "http://h", "m3");
        assert_eq!(ai_status(&[unnamed], "c").as_deref(), Some("m3"));
        assert_eq!(ai_status(&[blank, ready], "gone"), None, "first is not ready");
    }

    #[test]
    fn ai_page_is_a_live_section() {
        assert_eq!(PAGES[usize::from(AI_PAGE)].0, "settings.page.ai");
        let src = include_str!("settings_win.rs");
        let body = src.split("mod tests {").next().unwrap_or("");
        assert!(!body.contains("add_enabled_ui(false"), "no disabled placeholder field");
        // built at run time so the key scanners do not count the removed key as used
        let soon = ["settings", "ai_soon"].join(".");
        assert!(!body.contains(&soon));
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            assert_eq!(cat.t(&soon), soon, "{lang:?}: key removed");
            for k in [AI, AI_CONNECT, AI_PANEL] {
                let text = format!("{} {}", cat.t(k.0), cat.t(k.1)).to_lowercase();
                for bad in ["soon", "later release", "\u{441}\u{43a}\u{43e}\u{440}\u{43e}"] {
                    assert!(!text.contains(bad), "{lang:?}: {text}");
                }
            }
        }
    }

    #[test]
    fn every_ai_limit_setting_has_a_row_text_in_every_language() {
        assert!(ddc_ai::limits::settings().count() >= 8);
        for lang in Lang::ALL.iter().copied() {
            let cat = Catalog::new(lang);
            for l in ddc_ai::limits::settings() {
                let (a, b) = limit_keys(l);
                for k in [a, b] {
                    assert_ne!(cat.t(&k), k, "{lang:?}: {k}");
                }
            }
            for k in ["ai_limits.default", "ai_limits.unit_s", "ai_limits.unit_mb"] {
                assert!(cat.t(k) != k, "{lang:?}: {k}");
            }
            assert!(cat.t("ai_limits.default").contains("{n}"), "{lang:?}");
        }
    }

    #[test]
    fn window_keeps_the_workspace_size_or_ninety_percent() {
        let (w, h) = window_size(Vec2::new(1920.0, 1080.0));
        assert_eq!((w, h), (980.0, 640.0));
        let (w, h) = window_size(Vec2::new(900.0, 600.0));
        assert_eq!((w, h), (810.0, 540.0));
        assert_eq!(body_height(468.0, 100.0, 32.0), 336.0, "the head comes off the window");
        assert_eq!(nav_width(120.0), NAV_W, "never narrower than the floor");
        assert_eq!(nav_width(190.0), 230.0, "longest name plus 40");
    }

    #[test]
    fn branding_page_sits_before_about_at_its_index() {
        assert_eq!(PAGES[usize::from(BRANDING_PAGE)].0, "settings.page.branding");
        assert_eq!(PAGES[usize::from(BRANDING_PAGE) + 1].0, "settings.page.about");
    }

    #[test]
    fn settings_page_option_takes_an_index_or_a_name() {
        let a = |v: &[&str]| page_from_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&["app", "--settings-page", "library"]), Some(LIBRARY_PAGE));
        assert_eq!(a(&["app", "--settings-page", "8"]), Some(LIBRARY_PAGE));
        assert_eq!(a(&["app", "--settings-page", "About"]), Some(PAGES.len() as u8 - 1));
        assert_eq!(a(&["app", "--settings-page", "99"]), None);
        assert_eq!(a(&["app", "--settings-page", "nope"]), None);
        assert_eq!(a(&["app", "--settings-page"]), None);
        assert_eq!(a(&["app"]), None);
        assert_eq!(PAGES[usize::from(LIBRARY_PAGE)].0, "settings.page.library");
    }

    #[test]
    fn pages_and_bodies_line_up() {
        assert_eq!(PAGES.len(), BODIES.len());
        for (_, icon) in PAGES {
            assert!(ddc_ui::icons::source(icon).is_some(), "{icon}");
        }
    }
}
