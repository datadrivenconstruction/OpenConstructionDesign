// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! User settings persisted between runs: properties width, area/volume precision.
//! File `prefs.txt` in `data_dir()` holds `key=value` lines; malformed lines are skipped.
use crate::units::{self, AREA_DECIMALS, VOLUME_DECIMALS};
use ddc_ui::workspace::LayoutDensity;
use ddc_ui::Lang;
use std::path::PathBuf;

pub const RECENT_MAX: usize = 6;
pub const RECENT_PROJECTS_MAX: usize = 12;

/// Shortest save reminder, min: more often it is noise. 0 turns it off.
pub const REMIND_MIN: u32 = 5;

#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub props_width: f32,
    /// AI panel: width, whether open, selected profile.
    pub ai_panel_w: f32,
    pub ai_panel_open: bool,
    pub ai_profile: String,
    pub area_decimals: usize,
    pub volume_decimals: usize,
    /// Inch fraction denominator (2…256).
    pub frac_denominator: usize,
    /// Show areas under 1 ft² in in² in an imperial project.
    pub small_area_in2: bool,
    /// "system" (follow the OS setting), "light" or "dark".
    pub theme: String,
    /// 3D effects (shadow, shading): "auto" (discrete GPU), "on", "off".
    pub effects: String,
    /// Performance level of this machine (`PerfLevel` key); never stored in a project.
    pub gfx_level: String,
    /// Cached fill probe: `<ms per megapixel>|<adapter identity>`.
    pub gfx_probe: String,
    /// Small ribbon buttons.
    pub ribbon_small: bool,
    /// Open the workbook in its program after an export to XLSX.
    pub xlsx_open_after: bool,
    /// Hint card with the keys of the current step.
    pub hint_card: bool,
    /// Recent files (newest first, at most `RECENT_MAX`).
    pub recent: Vec<String>,
    /// Recent projects (newest first): imports do not push them out.
    pub recent_projects: Vec<String>,
    /// UI layout (`classic`..`data`); whether the chooser window was shown.
    pub layout: String,
    pub first_run_done: bool,
    /// First-run card on the sample finished or dismissed: never shown again.
    pub first_card_done: bool,
    /// Commands-and-keys panel: open, and floating instead of docked.
    pub help_open: bool,
    pub help_float: bool,
    /// Autosave (TOOLS_UI §9): on/off, interval 1–60 min, after N changes (0 = time only),
    /// how many snapshots to keep, folder (empty = `data_dir()/autosave`),
    /// save reminder after N min (0 = off).
    pub autosave: bool,
    pub autosave_min: u32,
    pub autosave_edits: u32,
    pub autosave_keep: usize,
    pub autosave_dir: String,
    pub remind_min: u32,
    /// Remembered layout: browser width, folded docks, table height (0 — by layout), ribbon.
    pub browser_width: f32,
    pub browser_open: bool,
    /// The browser is folded for the assistant, as `props_folded` is for Properties.
    pub browser_folded: bool,
    pub props_open: bool,
    /// Properties are folded for the assistant, not by the person: never written, and `text`
    /// keeps them open in the file.
    pub props_folded: bool,
    pub table_height: f32,
    pub ribbon_collapsed: bool,
    /// Hidden ribbon, options and status bars, the AI tab's folded side.
    pub fold: crate::shell::fold::FoldPrefs,
    /// "auto" (follow the system), "ru", "en".
    pub lang: String,
    /// UI scale, one of the theme steps.
    pub ui_scale: f32,
    /// "auto" (follow the layout), "compact", "normal", "comfortable".
    pub density: String,
    /// Look (Settings → Appearance): preset id; the fields below start from it.
    pub look: String,
    /// Accent `#RRGGBB`; empty — the look's own.
    pub accent: String,
    /// Interface face: «inter», «plex», «system».
    pub font: String,
    /// Text size factor over the type roles (`looks::TEXT_MIN..=TEXT_MAX`).
    pub text_size: f32,
    /// Icon set id (`data/icon_style.json`).
    pub icon_set: String,
    /// Interface mode: «ribbon» (default), «rail» or «minimal» (`shell::ui_mode`).
    pub ui_mode: String,
    /// Panel layout of each work mode, and layouts saved by name.
    pub mode_layouts: crate::shell::mode_layout::Store,
    /// Work focus chosen in the setup wizard, primary first («ai,data»); empty: none chosen.
    pub focus: String,
    /// The setup wizard was finished or skipped; it opens again only on demand.
    pub onboarded: bool,
    /// Show the start screen on launch.
    pub start_screen: bool,
    /// Units last chosen for a new project on the start screen: "metric", "imperial" or empty.
    pub start_units: String,
    /// Snap capture radius, screen pixels.
    pub snap_px: f32,
    /// Thin-lines display toggle; applies to views without their own session choice.
    pub thin_lines: bool,
    /// Plan axes drawn as full lines; F9 shows them while held either way.
    pub full_axes: bool,
    /// The user turned the ERP link on (opens `.ddclink` files); off until asked.
    pub erp_link: bool,
    /// Assistant limits the person changed (Settings > AI), by `ddc_ai::limits` key; others keep
    /// their default. Stored as `ai.limit.<key>=<value>`.
    pub ai_limits: std::collections::BTreeMap<String, u64>,
    /// Where to save; `None` keeps settings in memory only (screenshots, tests).
    pub path: Option<PathBuf>,
    /// The last write of the settings files failed: not a setting, never written or compared.
    pub write_fail: WriteFail,
}

/// Why settings are not reaching disk, and whether the user was told this session.
#[derive(Debug, Default)]
pub struct WriteFail(std::sync::Mutex<(Option<String>, bool)>);

impl WriteFail {
    fn with<R>(&self, f: impl FnOnce(&mut (Option<String>, bool)) -> R) -> R {
        let mut g = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut g)
    }

    /// Records the outcome of one write.
    pub fn set(&self, err: Option<String>) {
        self.with(|s| s.0 = err);
    }

    pub fn current(&self) -> Option<String> {
        self.with(|s| s.0.clone())
    }

    /// The failure to tell about: once per session, however many writes fail.
    pub fn untold(&self) -> Option<String> {
        self.with(|s| match &s.0 {
            Some(e) if !s.1 => {
                s.1 = true;
                Some(e.clone())
            }
            _ => None,
        })
    }
}

impl Clone for WriteFail {
    fn clone(&self) -> Self {
        Self(std::sync::Mutex::new(self.with(|s| s.clone())))
    }
}

impl PartialEq for WriteFail {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            props_width: crate::ui_kit::PROPS_W,
            ai_panel_w: 360.0,
            ai_panel_open: false,
            ai_profile: String::new(),
            area_decimals: AREA_DECIMALS,
            volume_decimals: VOLUME_DECIMALS,
            frac_denominator: units::DEFAULT_FRAC,
            small_area_in2: false,
            theme: "system".into(),
            effects: "auto".into(),
            gfx_level: "auto".into(),
            gfx_probe: String::new(),
            ribbon_small: false,
            xlsx_open_after: true,
            hint_card: true,
            recent: Vec::new(),
            recent_projects: Vec::new(),
            layout: "classic".into(),
            first_run_done: false,
            first_card_done: false,
            help_open: false,
            help_float: false,
            autosave: true,
            autosave_min: 5,
            autosave_edits: 100,
            autosave_keep: 5,
            autosave_dir: String::new(),
            remind_min: 30,
            browser_width: crate::shell::dock_ui::BROWSER_W,
            browser_open: true,
            browser_folded: false,
            props_open: true,
            props_folded: false,
            table_height: 0.0,
            ribbon_collapsed: false,
            fold: Default::default(),
            lang: "auto".into(),
            ui_scale: 1.0,
            density: "auto".into(),
            look: "studio".into(),
            accent: String::new(),
            font: "inter".into(),
            text_size: 1.0,
            icon_set: "duotone_all".into(),
            ui_mode: "ribbon".into(),
            mode_layouts: Default::default(),
            focus: String::new(),
            onboarded: false,
            start_screen: true,
            start_units: String::new(),
            snap_px: crate::snap_refine::SNAP_PX_DEFAULT,
            thin_lines: false,
            full_axes: false,
            erp_link: false,
            ai_limits: Default::default(),
            path: None,
            write_fail: WriteFail::default(),
        }
    }
}

impl Prefs {
    /// UI language: chosen or system.
    pub fn ui_lang(&self, system: Lang) -> Lang {
        match self.lang.as_str() {
            "auto" => system,
            code => Lang::from_code(code),
        }
    }

    /// Language before the first frame, when the first project is seeded: a `--lang ru|en`
    /// argument wins, then the saved choice, then the OS language.
    pub fn startup_lang(&self, argv: &[String], system: Lang) -> Lang {
        match argv.iter().position(|s| s == "--lang").and_then(|i| argv.get(i + 1)) {
            Some(l) => Lang::from_code(l),
            None => self.ui_lang(system),
        }
    }

    /// Density chosen by the user on top of the layout.
    pub fn density_override(&self) -> Option<LayoutDensity> {
        match self.density.as_str() {
            "compact" => Some(LayoutDensity::Compact),
            "normal" => Some(LayoutDensity::Regular),
            "comfortable" => Some(LayoutDensity::Comfortable),
            _ => None,
        }
    }

    pub fn parse(s: &str) -> Self {
        let mut level_seen = false;
        let mut p = Self::default();
        for (k, v) in s.lines().filter_map(|l| l.split_once('=')) {
            let v = v.trim();
            match k.trim() {
                "props_width" => p.props_width = v.parse().unwrap_or(p.props_width),
                "ai_panel_w" => p.ai_panel_w = v.parse().unwrap_or(p.ai_panel_w),
                "ai_panel_open" => p.ai_panel_open = v == "1",
                "ai_profile" => p.ai_profile = v.to_string(),
                "area_decimals" => p.area_decimals = v.parse().unwrap_or(p.area_decimals),
                "frac_denominator" => {
                    p.frac_denominator = units::clamp_frac(v.parse().unwrap_or(0))
                }
                "small_area_in2" => p.small_area_in2 = v == "1",
                "volume_decimals" => p.volume_decimals = v.parse().unwrap_or(p.volume_decimals),
                "theme" if ["system", "light", "dark"].contains(&v) => p.theme = v.into(),
                "gfx_level" if ddc_render::PerfLevel::parse(v).is_some() => {
                    p.gfx_level = v.into();
                    level_seen = true;
                }
                "gfx_probe" => p.gfx_probe = v.to_string(),
                "effects" if ["auto", "on", "high", "medium", "off"].contains(&v) => {
                    p.effects = v.into()
                }
                "ribbon_small" => p.ribbon_small = v == "1",
                // "excel_open_after": stored name, kept for saved files
                "xlsx_open_after" | "excel_open_after" => p.xlsx_open_after = v != "0",
                "hint_card" => p.hint_card = v != "0",
                "thin_lines" => p.thin_lines = v == "1",
                "full_axes" => p.full_axes = v == "1",
                "erp_link" => p.erp_link = v == "1",
                "browser_width" => p.browser_width = v.parse().unwrap_or(p.browser_width),
                "panel.browser_open" => p.browser_open = v != "0",
                "panel.props_open" => p.props_open = v != "0",
                "panel.table_height" => p.table_height = v.parse().unwrap_or(p.table_height),
                "ribbon_collapsed" => p.ribbon_collapsed = v == "1",
                k if p.fold.read(k, v) => {}
                "lang" if v == "auto" || Lang::ALL.iter().any(|l| l.code() == v) => {
                    p.lang = v.into()
                }
                "ui_scale" => p.ui_scale = v.parse().unwrap_or(p.ui_scale),
                "density" if ["auto", "compact", "normal", "comfortable"].contains(&v) => {
                    p.density = v.into()
                }
                "look" if ddc_ui::looks::preset(v).is_some() => p.look = v.into(),
                "look.accent" if v.is_empty() || ddc_ui::theme::parse_hex(v).is_some() => {
                    p.accent = v.to_uppercase()
                }
                "look.font" if ddc_ui::theme::UiFont::parse(v).is_some() => p.font = v.into(),
                "look.text_size" => p.text_size = v.parse().unwrap_or(p.text_size),
                k if k.starts_with("ai.limit.") => {
                    let key = &k["ai.limit.".len()..];
                    if let (Some(l), Ok(n)) = (ddc_ai::limits::find(key), v.parse::<u64>()) {
                        if let ddc_ai::limits::Tier::Setting { min, max } = l.tier {
                            p.ai_limits.insert(l.key.to_string(), n.clamp(min, max));
                        }
                    }
                }
                "look.icons" if ddc_ui::icons::set(v).is_some() => p.icon_set = v.into(),
                "focus" => p.focus = crate::shell::onboard::clean_focus(v),
                "onboarded" => p.onboarded = v == "1",
                "ui.mode" if crate::shell::ui_mode::UiMode::parse(v).is_some() => p.ui_mode = v.into(),
                k if p.mode_layouts.read(k, v) => {}
                "start_screen" => p.start_screen = v != "0",
                "start.units" if ["metric", "imperial"].contains(&v) => p.start_units = v.into(),
                "snap.px" => p.snap_px = v.parse().unwrap_or(p.snap_px),
                "layout" => {
                    if let Some(l) = ddc_ui::workspace::parse_layout_value(v) {
                        p.layout = l;
                    }
                }
                "layout.first_run_done" => p.first_run_done = v == "1",
                "first_card.done" => p.first_card_done = v == "1",
                "help.open" => p.help_open = v == "1",
                "help.float" => p.help_float = v == "1",
                "autosave" => p.autosave = v != "0",
                "autosave.min" => p.autosave_min = v.parse().unwrap_or(p.autosave_min),
                "autosave.edits" => p.autosave_edits = v.parse().unwrap_or(p.autosave_edits),
                "autosave.keep" => p.autosave_keep = v.parse().unwrap_or(p.autosave_keep),
                "autosave.dir" => p.autosave_dir = v.into(),
                "autosave.remind" => p.remind_min = v.parse().unwrap_or(p.remind_min),
                "recent" => {
                    p.recent = v
                        .split('|')
                        .filter(|s| !s.is_empty())
                        .take(RECENT_MAX)
                        .map(String::from)
                        .collect()
                }
                "recent.projects" => {
                    p.recent_projects = v
                        .split('|')
                        .filter(|s| !s.is_empty())
                        .take(RECENT_PROJECTS_MAX)
                        .map(String::from)
                        .collect()
                }
                _ => {}
            }
        }
        let d = Self::default();
        // "NaN" and "inf" parse as numbers and pass through clamp
        let ok = |v: f32, or: f32| if v.is_finite() { v } else { or };
        p.props_width = ok(p.props_width, d.props_width).clamp(crate::ui_kit::PROPS_MIN_W, crate::shell::fold::PANEL_MAX_W);
        p.browser_width = ok(p.browser_width, d.browser_width)
            .clamp(crate::shell::dock_ui::BROWSER_MIN_W, crate::shell::fold::PANEL_MAX_W);
        p.table_height = ok(p.table_height, d.table_height).clamp(0.0, 2000.0);
        p.ui_scale = ok(p.ui_scale, d.ui_scale).clamp(0.75, 2.0);
        p.text_size = ok(p.text_size, d.text_size)
            .clamp(ddc_ui::looks::TEXT_MIN, ddc_ui::looks::TEXT_MAX);
        p.ai_panel_w = ok(p.ai_panel_w, d.ai_panel_w).clamp(280.0, 640.0);
        p.snap_px = crate::snap_refine::clamp_px(p.snap_px);
        p.area_decimals = p.area_decimals.min(6);
        p.volume_decimals = p.volume_decimals.min(6);
        p.frac_denominator = units::clamp_frac(p.frac_denominator);
        p.autosave_min = p.autosave_min.clamp(1, 60);
        // prefs written before levels existed keep what the user chose then
        if !level_seen {
            p.gfx_level = ddc_render::PerfLevel::from_legacy(&p.effects).key().into();
        }
        p.autosave_keep = p.autosave_keep.clamp(1, 50);
        p.remind_min = match p.remind_min {
            0 => 0,
            m => m.clamp(REMIND_MIN, 240),
        };
        p
    }

    /// Autosave snapshots folder.
    pub fn autosave_folder(&self) -> Option<PathBuf> {
        if self.autosave_dir.trim().is_empty() {
            crate::gfx::data_dir().map(|d| d.join("autosave"))
        } else {
            Some(PathBuf::from(self.autosave_dir.trim()))
        }
    }

    pub fn text(&self) -> String {
        let auto = format!(
            "autosave={}\nautosave.min={}\nautosave.edits={}\nautosave.keep={}\nautosave.dir={}\nautosave.remind={}\n",
            u8::from(self.autosave),
            self.autosave_min,
            self.autosave_edits,
            self.autosave_keep,
            self.autosave_dir.replace('\n', " "),
            self.remind_min
        );
        let shell = format!(
            "browser_width={:.0}\npanel.browser_open={}\npanel.props_open={}\npanel.table_height={:.0}\nribbon_collapsed={}\nlang={}\nui_scale={}\ndensity={}\nstart_screen={}\n",
            self.browser_width,
            u8::from(self.browser_open || self.browser_folded),
            u8::from(self.props_open || self.props_folded),
            self.table_height,
            u8::from(self.ribbon_collapsed),
            self.lang,
            self.ui_scale,
            self.density,
            u8::from(self.start_screen)
        );
        let projects = format!("recent.projects={}\n", self.recent_projects.join("|"));
        auto + &projects + &shell + &format!(
            "props_width={:.0}\narea_decimals={}\nvolume_decimals={}\nfrac_denominator={}\nsmall_area_in2={}\ntheme={}\neffects={}\nribbon_small={}\nrecent={}\nlayout={}\nlayout.first_run_done={}\n",
            self.props_width,
            self.area_decimals,
            self.volume_decimals,
            self.frac_denominator,
            u8::from(self.small_area_in2),
            self.theme,
            self.effects,
            u8::from(self.ribbon_small),
            self.recent.join("|"),
            self.layout,
            u8::from(self.first_run_done)
        ) + &format!(
            "\nai_panel_w={:.0}\nai_panel_open={}\nai_profile={}\nxlsx_open_after={}\nhint_card={}\n",
            self.ai_panel_w,
            u8::from(self.ai_panel_open),
            self.ai_profile,
            u8::from(self.xlsx_open_after),
            u8::from(self.hint_card)
        ) + &format!(
            "gfx_level={}\ngfx_probe={}\n",
            self.gfx_level, self.gfx_probe
        ) + &format!("snap.px={}\nthin_lines={}\n", self.snap_px, u8::from(self.thin_lines))
          + &format!("full_axes={}\n", u8::from(self.full_axes))
          + &format!("erp_link={}\n", u8::from(self.erp_link))
          + &self.ai_limits.iter().map(|(k, v)| format!("ai.limit.{k}={v}\n")).collect::<String>()
          + &self.fold.text()
          + &format!(
            "look={}
look.accent={}
look.font={}
look.text_size={}
look.icons={}
",
            self.look, self.accent, self.font, self.text_size, self.icon_set
        ) + &self.mode_layouts.text()
          + &format!("focus={}\nonboarded={}\n", self.focus, u8::from(self.onboarded))
          + &format!("ui.mode={}\n", self.ui_mode)
          + &format!("start.units={}\n", self.start_units)
          + &format!(
            "first_card.done={}\nhelp.open={}\nhelp.float={}\n",
            u8::from(self.first_card_done),
            u8::from(self.help_open),
            u8::from(self.help_float)
        )
    }

    /// File opened or imported: move to the front of the recent list.
    pub fn touch_recent(&mut self, path: &str) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.replace('|', "_"));
        self.recent.truncate(RECENT_MAX);
    }

    /// Project opened or saved: move to the front of the projects list and the general recent list.
    pub fn touch_project(&mut self, path: &str) {
        self.touch_recent(path);
        let clean = path.replace('|', "_");
        self.recent_projects.retain(|p| *p != clean);
        self.recent_projects.insert(0, clean);
        self.recent_projects.truncate(RECENT_PROJECTS_MAX);
    }

    /// Read the settings file and apply the precision to the formatters.
    pub fn load() -> Self {
        Self::load_from(crate::gfx::data_dir().map(|d| d.join("prefs.txt")))
    }

    /// A damaged file falls back to `.bak` (notice queued); the damaged copy stays as `*.broken`.
    pub fn load_from(path: Option<PathBuf>) -> Self {
        let s = path
            .as_ref()
            .and_then(|p| {
                crate::safe_file::load_text(p, &|t| t.ends_with('\n') && t.contains('='))
            })
            .unwrap_or_default();
        let p = Self {
            path,
            ..Self::parse(&s)
        };
        p.apply();
        p
    }

    pub fn apply(&self) {
        units::set_precision(self.area_decimals, self.volume_decimals);
        units::set_frac_denominator(self.frac_denominator);
        units::set_small_area_in2(self.small_area_in2);
        crate::snap_refine::set_snap_px(self.snap_px);
        ddc_ai::limits::set_overrides(self.ai_limits.iter().map(|(k, v)| (k.as_str(), *v)));
    }

    /// A failed write does not stop the work: the settings stay for this session, the failure
    /// is kept in `write_fail` and told once (`project_ui::tell_settings_failure`).
    pub fn save(&self) {
        let r = self.try_save();
        if let Err(e) = &r {
            eprintln!("settings: not written: {e}");
        }
        self.write_fail.set(r.err());
    }

    /// Writes the settings file whole (temporary file, then rename) or not at all.
    pub fn try_save(&self) -> Result<(), String> {
        let Some(p) = &self.path else {
            return Ok(());
        };
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        crate::safe_file::write_atomic(p, self.text().as_bytes()).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
#[test]
fn prefs_round_trip_and_clamp() {
    let p = Prefs {
        props_width: 340.0,
        ai_panel_w: 420.0,
        ai_panel_open: true,
        ai_profile: "p2".into(),
        area_decimals: 1,
        volume_decimals: 4,
        frac_denominator: 64,
        small_area_in2: true,
        theme: "dark".into(),
        effects: "off".into(),
        gfx_level: "quality".into(),
        gfx_probe: "1.250|some adapter".into(),
        ribbon_small: true,
        xlsx_open_after: false,
        hint_card: false,
        recent: vec!["C:/a.ifc".into(), "D:/b.dwg".into()],
        recent_projects: vec!["C:/a.ddc".into(), "D:/b.ddc".into()],
        layout: "split".into(),
        first_run_done: true,
        first_card_done: true,
        help_open: true,
        help_float: true,
        autosave: false,
        autosave_min: 12,
        autosave_edits: 0,
        autosave_keep: 7,
        autosave_dir: "D:/snaps".into(),
        remind_min: 0,
        browser_width: 310.0,
        browser_open: false,
        browser_folded: false,
        props_open: false,
        props_folded: false,
        table_height: 240.0,
        ribbon_collapsed: true,
        fold: crate::shell::fold::FoldPrefs {
            ribbon_hidden: true,
            options_bar: false,
            status_bar: false,
            ai: crate::shell::fold::AiFold::Preview,
        },
        lang: "en".into(),
        ui_scale: 1.25,
        density: "comfortable".into(),
        look: "night".into(),
        accent: "#0F766E".into(),
        font: "plex".into(),
        text_size: 1.15,
        icon_set: "duotone_all".into(),
        ui_mode: "rail".into(),
        mode_layouts: {
            use crate::shell::mode_layout::{Layout, Store, WorkMode};
            let mut s = Store::default();
            let l = Layout {
                browser_width: 300.0,
                props_open: false,
                ..Layout::default()
            };
            s.modes.insert(WorkMode::Drawings, l.clone());
            s.save_as("Drawing narrow", l);
            s
        },
        focus: "data,ai".into(),
        onboarded: true,
        start_screen: false,
        start_units: "imperial".into(),
        snap_px: 20.0,
        thin_lines: true,
        full_axes: true,
        erp_link: true,
        ai_limits: [("copies".to_string(), 400)].into(),
        path: None,
        write_fail: WriteFail::default(),
    };
    assert_eq!(Prefs::parse(&p.text()), p);
    assert_eq!(p.ui_lang(Lang::Ru), Lang::En);
    assert_eq!(p.density_override(), Some(LayoutDensity::Comfortable));
    let auto = Prefs::default();
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(auto.startup_lang(&args(&[]), Lang::En), Lang::En);
    assert_eq!(auto.startup_lang(&args(&[]), Lang::Ru), Lang::Ru);
    assert_eq!(p.startup_lang(&args(&["--lang", "ru"]), Lang::En), Lang::Ru);
    assert_eq!(auto.startup_lang(&args(&["--lang", "en"]), Lang::Ru), Lang::En);
    assert_eq!(auto.ui_lang(Lang::Ru), Lang::Ru);
    assert_eq!(auto.density_override(), None);
    assert!(auto.browser_open && auto.props_open && auto.start_screen);
    let bad = Prefs::parse("lang=xx\nui_scale=9\nbrowser_width=1\ndensity=huge");
    assert_eq!((bad.lang.as_str(), bad.density.as_str()), ("auto", "auto"));
    assert_eq!((bad.ui_scale, bad.browser_width), (2.0, 240.0));
    let d = Prefs::parse("props_width=10\narea_decimals=x\nvolume_decimals=99\njunk");
    assert_eq!(d.props_width, crate::ui_kit::PROPS_MIN_W);
    assert_eq!((d.area_decimals, d.volume_decimals), (AREA_DECIMALS, 6));
    assert!(d.autosave && d.autosave_min == 5 && d.autosave_keep == 5);
    let c = Prefs::parse("autosave.min=0\nautosave.keep=999\nautosave.min=x");
    assert_eq!((c.autosave_min, c.autosave_keep), (1, 50));
}

#[cfg(test)]
#[test]
fn recent_projects_keep_newest_first_without_repeats() {
    let mut p = Prefs::default();
    for k in 0..(RECENT_PROJECTS_MAX + 3) {
        p.touch_project(&format!("C:/p{k}.ddc"));
    }
    assert_eq!(p.recent_projects.len(), RECENT_PROJECTS_MAX);
    assert_eq!(
        p.recent_projects[0],
        format!("C:/p{}.ddc", RECENT_PROJECTS_MAX + 2)
    );
    p.touch_project("C:/p5.ddc");
    assert_eq!(p.recent_projects[0], "C:/p5.ddc");
    assert_eq!(
        p.recent_projects
            .iter()
            .filter(|x| *x == "C:/p5.ddc")
            .count(),
        1
    );
    assert_eq!(p.recent.len(), RECENT_MAX, "the mixed list sees it too");
    // imports push only the mixed list
    for k in 0..RECENT_MAX {
        p.touch_recent(&format!("C:/i{k}.ifc"));
    }
    assert!(p.recent.iter().all(|x| x.ends_with(".ifc")));
    assert_eq!(p.recent_projects[0], "C:/p5.ddc");
    assert_eq!(Prefs::parse(&p.text()).recent_projects, p.recent_projects);
}

#[test]
fn bad_numbers_and_short_reminders_do_not_survive() {
    let d = Prefs::default();
    let p = Prefs::parse(
        "browser_width=NaN\npanel.table_height=inf\nprops_width=NaN\nui_scale=NaN\nautosave.remind=1\n",
    );
    assert_eq!(p.browser_width, d.browser_width);
    assert_eq!(p.table_height, d.table_height);
    assert_eq!(p.props_width, d.props_width);
    assert_eq!(p.ui_scale, d.ui_scale);
    assert_eq!(p.remind_min, REMIND_MIN);
    assert_eq!(Prefs::parse("autosave.remind=0\n").remind_min, 0);
}

#[cfg(test)]
mod gfx_level_tests {
    use super::Prefs;

    #[test]
    fn level_round_trips_and_older_prefs_map_once() {
        let p = Prefs {
            gfx_level: "balanced".into(),
            gfx_probe: "2.000|adapter".into(),
            ..Prefs::default()
        };
        let back = Prefs::parse(&p.text());
        assert_eq!(back.gfx_level, "balanced");
        assert_eq!(back.gfx_probe, "2.000|adapter");
        assert_eq!(Prefs::parse("effects=off\n").gfx_level, "speed");
        assert_eq!(Prefs::parse("effects=medium\n").gfx_level, "balanced");
        assert_eq!(Prefs::parse("effects=high\n").gfx_level, "quality");
        assert_eq!(Prefs::parse("").gfx_level, "auto");
        assert_eq!(Prefs::parse("gfx_level=nonsense\n").gfx_level, "auto");
        // the newer key wins over the older one
        let both = "effects=off\ngfx_level=quality\n";
        assert_eq!(Prefs::parse(both).gfx_level, "quality");
    }
}

#[cfg(test)]
mod safe_tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-prefs-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn prefs_partial_file_keeps_previous() {
        let p = dir("partial").join("prefs.txt");
        let mut a = Prefs::default();
        a.path = Some(p.clone());
        a.props_width = 333.0;
        a.save();
        a.props_width = 444.0;
        a.save();
        // an interrupted write only ever leaves a temp file; the real one is untouched
        std::fs::write(p.with_file_name("prefs.txt.tmp"), "props_wi").unwrap();
        assert_eq!(Prefs::load_from(Some(p)).props_width, 444.0);
    }

    #[test]
    fn bad_prefs_moved_aside() {
        let d = dir("bad");
        let p = d.join("prefs.txt");
        let mut a = Prefs::default();
        a.path = Some(p.clone());
        a.props_width = 333.0;
        a.save();
        a.props_width = 444.0;
        a.save();
        std::fs::write(&p, "props_wi").unwrap(); // torn write
        assert_eq!(Prefs::load_from(Some(p.clone())).props_width, 333.0);
        assert_eq!(std::fs::read_to_string(d.join("prefs.txt.broken")).unwrap(), "props_wi");
        // both generations bad: defaults, damaged files kept
        std::fs::write(d.join("prefs.txt.bak"), "").unwrap();
        std::fs::write(&p, "zzz").unwrap();
        assert_eq!(Prefs::load_from(Some(p)).props_width, Prefs::default().props_width);
        assert_eq!(std::fs::read_to_string(d.join("prefs.txt.broken1")).unwrap(), "zzz");
    }
}

#[cfg(test)]
#[test]
fn legacy_open_after_key_still_loads() {
    // the old key name, as older builds wrote it
    let old = Prefs::parse("excel_open_after=0\n");
    assert!(!old.xlsx_open_after);
    let new = Prefs::parse("xlsx_open_after=0\n");
    assert!(!new.xlsx_open_after);
    assert!(Prefs::parse("").xlsx_open_after);
    assert!(!Prefs::parse(&new.text()).xlsx_open_after);
}

#[cfg(test)]
#[test]
fn thin_lines_pref_roundtrips() {
    let on = Prefs {
        thin_lines: true,
        ..Prefs::default()
    };
    assert!(Prefs::parse(&on.text()).thin_lines);
    assert!(!Prefs::parse(&Prefs::default().text()).thin_lines);
}

#[cfg(test)]
#[test]
fn full_axes_pref_roundtrips_and_defaults_off() {
    let on = Prefs {
        full_axes: true,
        ..Prefs::default()
    };
    assert!(Prefs::parse(&on.text()).full_axes);
    assert!(!Prefs::parse(&Prefs::default().text()).full_axes);
    assert!(!Prefs::parse("lang=en\n").full_axes);
}

#[cfg(test)]
#[test]
fn erp_link_is_off_until_turned_on() {
    assert!(!Prefs::default().erp_link);
    assert!(!Prefs::parse("lang=en\n").erp_link);
    let on = Prefs { erp_link: true, ..Prefs::default() };
    assert!(Prefs::parse(&on.text()).erp_link);
}

#[cfg(test)]
#[test]
fn missing_pref_defaults_off() {
    assert!(!Prefs::parse("lang=en
snap.px=12
").thin_lines);
    assert!(!Prefs::default().thin_lines);
}

#[cfg(test)]
#[test]
fn ai_limit_settings_persist_clamp_and_apply() {
    let p = Prefs::parse("ai.limit.copies=900\nai.limit.terrain_points=1\nai.limit.query_page=5\nai.limit.nope=3\n");
    assert_eq!(p.ai_limits.get("copies"), Some(&900));
    assert_eq!(p.ai_limits.get("terrain_points"), Some(&100), "kept within bounds");
    assert!(!p.ai_limits.contains_key("query_page"), "a guard is not a setting");
    assert_eq!(Prefs::parse(&p.text()).ai_limits, p.ai_limits);
    p.apply();
    assert_eq!(crate::ai::modify_cmds::max_copies(), 900);
    assert!(ddc_ai::limits::describe("copy_elements").contains("900"));
    Prefs::default().apply();
    assert_eq!(crate::ai::modify_cmds::max_copies(), 200);
}
