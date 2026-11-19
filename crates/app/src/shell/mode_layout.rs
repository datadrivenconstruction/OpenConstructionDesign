// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Panel layout per work mode (modelling, drawings, data, AI): leaving a mode keeps how its
//! docks, bars and ribbon were arranged, entering one brings its own back. Layouts can be saved
//! by name, reset, and moved between machines as JSON. Stored in prefs.txt (`ws.mode.<key>`,
//! `ws.saved`), so they persist with the other settings.
use super::fold::{AiFold, FoldPrefs, RibbonMode};
use super::EditorApp;
use crate::prefs::Prefs;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkMode {
    Model,
    Drawings,
    Data,
    Ai,
}

impl WorkMode {
    pub const ALL: [WorkMode; 4] = [Self::Model, Self::Drawings, Self::Data, Self::Ai];
    pub fn key(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Drawings => "drawings",
            Self::Data => "data",
            Self::Ai => "ai",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.key() == s)
    }
    /// Catalog key of the mode's name.
    pub fn name_key(self) -> String {
        format!("theme.mode.{}", self.key())
    }
}

/// The work mode on screen: an AI tab, a sheet or drafting view, the data ribbon tab, else
/// modelling.
pub fn current(app: &EditorApp) -> WorkMode {
    if crate::ai::ui::modes::current().is_some() {
        WorkMode::Ai
    } else if crate::sheet_view::active().is_some() || crate::drafting_view::active().is_some() {
        WorkMode::Drawings
    } else if app.sh.ribbon.tab == "data" {
        WorkMode::Data
    } else {
        WorkMode::Model
    }
}

/// What one mode remembers: dock visibility and size, bars, ribbon state, AI folds.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub browser_open: bool,
    pub browser_width: f32,
    pub props_open: bool,
    pub props_width: f32,
    pub table_height: f32,
    pub ribbon: RibbonMode,
    pub ribbon_small: bool,
    pub options_bar: bool,
    pub status_bar: bool,
    pub ai_fold: AiFold,
    pub ai_panel_open: bool,
    pub ai_panel_w: f32,
}

impl Default for Layout {
    fn default() -> Self {
        Self::capture(&Prefs::default())
    }
}

fn ribbon_key(r: RibbonMode) -> &'static str {
    match r {
        RibbonMode::Full => "full",
        RibbonMode::Tabs => "tabs",
        RibbonMode::Hidden => "hidden",
    }
}

fn ribbon_parse(s: &str) -> Option<RibbonMode> {
    [RibbonMode::Full, RibbonMode::Tabs, RibbonMode::Hidden]
        .into_iter()
        .find(|r| ribbon_key(*r) == s)
}

impl Layout {
    pub fn capture(p: &Prefs) -> Self {
        Self {
            browser_open: p.browser_open || p.browser_folded,
            browser_width: p.browser_width,
            props_open: p.props_open || p.props_folded,
            props_width: p.props_width,
            table_height: p.table_height,
            ribbon: RibbonMode::of(p),
            ribbon_small: p.ribbon_small,
            options_bar: p.fold.options_bar,
            status_bar: p.fold.status_bar,
            ai_fold: p.fold.ai,
            ai_panel_open: p.ai_panel_open,
            ai_panel_w: p.ai_panel_w,
        }
    }

    /// Writes the layout into the prefs, sizes clamped to what the prefs accept.
    pub fn restore(&self, p: &mut Prefs) {
        let ok = |v: f32, or: f32| if v.is_finite() { v } else { or };
        let d = Prefs::default();
        p.browser_open = self.browser_open;
        p.browser_folded = false;
        p.browser_width = ok(self.browser_width, d.browser_width).clamp(super::dock_ui::BROWSER_MIN_W, super::fold::PANEL_MAX_W);
        p.props_open = self.props_open;
        p.props_folded = false;
        p.props_width =
            ok(self.props_width, d.props_width).clamp(crate::ui_kit::PROPS_MIN_W, super::fold::PANEL_MAX_W);
        p.table_height = ok(self.table_height, d.table_height).clamp(0.0, 2000.0);
        self.ribbon.store(p);
        p.ribbon_small = self.ribbon_small;
        p.fold = FoldPrefs {
            ribbon_hidden: p.fold.ribbon_hidden,
            options_bar: self.options_bar,
            status_bar: self.status_bar,
            ai: self.ai_fold,
        };
        p.ai_panel_open = self.ai_panel_open;
        p.ai_panel_w = ok(self.ai_panel_w, d.ai_panel_w).clamp(280.0, 640.0);
    }

    pub fn to_json(&self) -> Value {
        json!({
            "browser_open": self.browser_open,
            "browser_width": self.browser_width,
            "props_open": self.props_open,
            "props_width": self.props_width,
            "table_height": self.table_height,
            "ribbon": ribbon_key(self.ribbon),
            "ribbon_small": self.ribbon_small,
            "options_bar": self.options_bar,
            "status_bar": self.status_bar,
            "ai_fold": self.ai_fold.key(),
            "ai_panel_open": self.ai_panel_open,
            "ai_panel_w": self.ai_panel_w,
        })
    }

    /// Missing or bad fields keep the default; a non-object is None.
    pub fn from_json(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        let mut l = Self::default();
        let b = |k: &str, d: bool| o.get(k).and_then(Value::as_bool).unwrap_or(d);
        let f = |k: &str, d: f32| {
            o.get(k)
                .and_then(Value::as_f64)
                .map(|x| x as f32)
                .filter(|x| x.is_finite())
                .unwrap_or(d)
        };
        let s = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or("");
        l.browser_open = b("browser_open", l.browser_open);
        l.browser_width = f("browser_width", l.browser_width);
        l.props_open = b("props_open", l.props_open);
        l.props_width = f("props_width", l.props_width);
        l.table_height = f("table_height", l.table_height);
        l.ribbon = ribbon_parse(s("ribbon")).unwrap_or(l.ribbon);
        l.ribbon_small = b("ribbon_small", l.ribbon_small);
        l.options_bar = b("options_bar", l.options_bar);
        l.status_bar = b("status_bar", l.status_bar);
        l.ai_fold = AiFold::parse(s("ai_fold")).unwrap_or(l.ai_fold);
        l.ai_panel_open = b("ai_panel_open", l.ai_panel_open);
        l.ai_panel_w = f("ai_panel_w", l.ai_panel_w);
        Some(l)
    }
}

/// Layouts of the modes and the ones saved by name (newest last).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Store {
    pub modes: BTreeMap<WorkMode, Layout>,
    pub saved: Vec<(String, Layout)>,
}

/// Longest saved-layout name, characters.
pub const NAME_MAX: usize = 48;
/// Most layouts kept by name.
pub const SAVED_MAX: usize = 24;
const FORMAT: &str = "ddc.layouts";

impl Store {
    /// One prefs line; false when the key is not one of ours.
    pub fn read(&mut self, k: &str, v: &str) -> bool {
        if let Some(m) = k.strip_prefix("ws.mode.") {
            if let (Some(m), Ok(j)) = (WorkMode::parse(m), serde_json::from_str::<Value>(v)) {
                if let Some(l) = Layout::from_json(&j) {
                    self.modes.insert(m, l);
                }
            }
            return true;
        }
        if k == "ws.saved" {
            if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(v) {
                self.saved = saved_from(&a);
            }
            return true;
        }
        false
    }

    pub fn text(&self) -> String {
        let mut s = String::new();
        for (m, l) in &self.modes {
            s += &format!("ws.mode.{}={}\n", m.key(), l.to_json());
        }
        if !self.saved.is_empty() {
            s += &format!("ws.saved={}\n", Value::Array(self.saved_json()));
        }
        s
    }

    fn saved_json(&self) -> Vec<Value> {
        self.saved
            .iter()
            .map(|(n, l)| json!({"name": n, "layout": l.to_json()}))
            .collect()
    }

    /// Saves under `name` (trimmed, cut to `NAME_MAX`); the same name is replaced.
    pub fn save_as(&mut self, name: &str, l: Layout) -> Option<String> {
        let name: String = name.trim().chars().take(NAME_MAX).collect();
        if name.is_empty() {
            return None;
        }
        self.saved.retain(|(n, _)| *n != name);
        self.saved.push((name.clone(), l));
        if self.saved.len() > SAVED_MAX {
            self.saved.remove(0);
        }
        Some(name)
    }

    /// The export file: every mode and every saved layout.
    pub fn export(&self) -> String {
        let modes: serde_json::Map<String, Value> = self
            .modes
            .iter()
            .map(|(m, l)| (m.key().to_string(), l.to_json()))
            .collect();
        let v = json!({"format": FORMAT, "version": 1, "modes": modes, "saved": self.saved_json()});
        serde_json::to_string_pretty(&v).unwrap_or_default() + "\n"
    }

    /// Reads an export file; imported modes and names replace ours, the rest stay.
    pub fn import(&mut self, text: &str) -> Result<usize, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if v.get("format").and_then(Value::as_str) != Some(FORMAT) {
            return Err("format".into());
        }
        let mut n = 0;
        if let Some(m) = v.get("modes").and_then(Value::as_object) {
            for (k, l) in m {
                if let (Some(k), Some(l)) = (WorkMode::parse(k), Layout::from_json(l)) {
                    self.modes.insert(k, l);
                    n += 1;
                }
            }
        }
        if let Some(Value::Array(a)) = v.get("saved") {
            for (name, l) in saved_from(a) {
                self.save_as(&name, l);
                n += 1;
            }
        }
        Ok(n)
    }
}

fn saved_from(a: &[Value]) -> Vec<(String, Layout)> {
    a.iter()
        .filter_map(|e| {
            let name: String = e.get("name")?.as_str()?.trim().chars().take(NAME_MAX).collect();
            let l = Layout::from_json(e.get("layout")?)?;
            (!name.is_empty()).then_some((name, l))
        })
        .take(SAVED_MAX)
        .collect()
}

impl EditorApp {
    /// Once per frame: on a mode change the old mode keeps its layout and the new one's comes
    /// back. The first frame adopts what is on screen.
    pub(crate) fn sync_mode_layout(&mut self, ctx: &egui::Context) {
        let now = current(self);
        match self.chrome.work_mode {
            None => self.chrome.work_mode = Some(now),
            Some(prev) if prev != now => {
                let cap = Layout::capture(&self.prefs);
                self.prefs.mode_layouts.modes.insert(prev, cap);
                if let Some(l) = self.prefs.mode_layouts.modes.get(&now).cloned() {
                    self.apply_mode_layout(ctx, &l);
                }
                self.chrome.work_mode = Some(now);
                self.prefs.save();
            }
            Some(_) => {}
        }
    }

    /// Puts `l` on screen: prefs, the ribbon, and egui's remembered dock sizes.
    pub(crate) fn apply_mode_layout(&mut self, ctx: &egui::Context, l: &Layout) {
        l.restore(&mut self.prefs);
        self.sh.ribbon.collapsed = self.prefs.ribbon_collapsed;
        self.sh.ribbon.small = self.prefs.ribbon_small;
        self.sh.ribbon.peek = false;
        self.chrome.fold = Default::default();
        self.chrome.dock = Default::default();
        ctx.memory_mut(|m| {
            for id in ["ddc-browser", "ddc-props"] {
                m.data.remove::<egui::panel::PanelState>(egui::Id::new(id));
            }
            m.data.remove::<egui::panel::PanelState>(
                ddc_ui::workspace::PanelId::DataTable.egui_id().into(),
            );
        });
    }

    fn mode_now(&self) -> WorkMode {
        self.chrome.work_mode.unwrap_or_else(|| current(self))
    }

    /// «Reset»: the current mode forgets its layout and gets the default one.
    pub(crate) fn mode_layout_reset(&mut self) {
        let m = self.mode_now();
        self.prefs.mode_layouts.modes.remove(&m);
        self.chrome.settings.reset_panels = true;
    }

    /// «Save as…»: the layout on screen under a name.
    pub(crate) fn mode_layout_save_as(&mut self, name: &str) -> Option<String> {
        let l = Layout::capture(&self.prefs);
        let n = self.prefs.mode_layouts.save_as(name, l);
        if n.is_some() {
            self.prefs.save();
        }
        n
    }

    /// A saved layout onto the current mode.
    pub(crate) fn mode_layout_use(&mut self, ctx: &egui::Context, name: &str) {
        let Some(l) = self
            .prefs
            .mode_layouts
            .saved
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, l)| l.clone())
        else {
            return;
        };
        self.apply_mode_layout(ctx, &l);
        let m = self.mode_now();
        self.prefs.mode_layouts.modes.insert(m, l);
        self.prefs.save();
    }

    pub(crate) fn mode_layout_export(&mut self) {
        let Some(path) = crate::file_pick::Dlg::new()
            .add_filter("JSON", &["json"])
            .set_file_name("ddc-layouts.json")
            .save_file()
        else {
            return;
        };
        let (lvl, key) = match std::fs::write(&path, self.prefs.mode_layouts.export()) {
            Ok(()) => (ddc_ui::notify::Level::Info, "theme.layout.exported"),
            Err(_) => (ddc_ui::notify::Level::Error, "theme.layout.export_failed"),
        };
        let p = path.display().to_string();
        crate::quick::toast(self, lvl, key, &[("path", &p)]);
    }

    pub(crate) fn mode_layout_import(&mut self) {
        let Some(path) = crate::file_pick::Dlg::new()
            .add_filter("JSON", &["json"])
            .pick_file()
        else {
            return;
        };
        let res = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| self.prefs.mode_layouts.import(&t));
        match res {
            Ok(n) => {
                if let Some(l) = self.prefs.mode_layouts.modes.get(&self.mode_now()).cloned() {
                    self.chrome.settings.apply_layout = Some(l);
                }
                self.prefs.save();
                let n = n.to_string();
                crate::quick::toast(self, ddc_ui::notify::Level::Info, "theme.layout.imported", &[("n", &n)]);
            }
            Err(_) => {
                let p = path.display().to_string();
                crate::quick::toast(self, ddc_ui::notify::Level::Error, "theme.layout.import_failed", &[("path", &p)]);
            }
        }
    }
}

fn h_reset(app: &mut EditorApp) {
    app.mode_layout_reset();
}

fn h_save(app: &mut EditorApp) {
    // the name is typed on the interface page, where «Save as…» lives
    app.chrome.settings.focus_layout_name = true;
    app.open_settings(super::settings_win::PAGE_INTERFACE);
}

fn h_export(app: &mut EditorApp) {
    app.mode_layout_export();
}

fn h_import(app: &mut EditorApp) {
    app.mode_layout_import();
}

pub fn register(app: &mut EditorApp) {
    app.register_command("ui.layout.mode.reset", h_reset);
    app.register_command("ui.layout.mode.save_as", h_save);
    app.register_command("ui.layout.mode.export", h_export);
    app.register_command("ui.layout.mode.import", h_import);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn odd() -> Layout {
        Layout {
            browser_open: false,
            browser_width: 333.0,
            props_open: false,
            props_width: 411.0,
            table_height: 222.0,
            ribbon: RibbonMode::Tabs,
            ribbon_small: true,
            options_bar: false,
            status_bar: false,
            ai_fold: AiFold::Work,
            ai_panel_open: true,
            ai_panel_w: 500.0,
        }
    }

    #[test]
    fn capture_restore_round_trip() {
        let mut p = Prefs::default();
        odd().restore(&mut p);
        assert_eq!(Layout::capture(&p), odd());
        let mut q = Prefs::default();
        Layout::capture(&p).restore(&mut q);
        assert_eq!(Layout::capture(&q), odd());
    }

    #[test]
    fn store_survives_prefs_text_and_export() {
        let mut s = Store::default();
        s.modes.insert(WorkMode::Data, odd());
        s.modes.insert(WorkMode::Ai, Layout::default());
        assert_eq!(s.save_as("  My view = 2  ", odd()).as_deref(), Some("My view = 2"));
        assert_eq!(s.save_as("   ", odd()), None);
        let mut back = Store::default();
        for (k, v) in s.text().lines().filter_map(|l| l.split_once('=')) {
            assert!(back.read(k, v), "{k}");
        }
        assert_eq!(back, s);
        let mut imp = Store::default();
        assert_eq!(imp.import(&s.export()), Ok(3));
        assert_eq!(imp, s);
        assert!(imp.import("{\"modes\":{}}").is_err());
        assert!(imp.import("not json").is_err());
    }

    #[test]
    fn bad_values_fall_back_and_sizes_clamp() {
        let l = Layout::from_json(&json!({"browser_width": 5000.0, "ribbon": "x", "ai_fold": 3}))
            .unwrap();
        assert_eq!(l.ribbon, Layout::default().ribbon);
        let mut p = Prefs::default();
        l.restore(&mut p);
        assert_eq!(p.browser_width, super::super::fold::PANEL_MAX_W);
        assert!(Layout::from_json(&json!([1, 2])).is_none());
    }

    #[test]
    fn saved_names_replace_and_cap() {
        let mut s = Store::default();
        for i in 0..(SAVED_MAX + 5) {
            s.save_as(&format!("n{i}"), Layout::default());
        }
        assert_eq!(s.saved.len(), SAVED_MAX);
        assert_eq!(s.saved[0].0, "n5");
        s.save_as("n9", odd());
        assert_eq!(s.saved.last().unwrap(), &("n9".to_string(), odd()));
        assert_eq!(s.saved.iter().filter(|(n, _)| n == "n9").count(), 1);
    }
}
