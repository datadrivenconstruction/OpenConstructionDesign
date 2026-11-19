// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Graphics modes in the app: the level from prefs, `Auto` from adapter facts and a cached probe,
//! the motion controller, the working view's style in the project, palette commands, the style
//! switcher button and the status note. Contract: docs/design/GRAPHICS_MODES.md.

use crate::shell::EditorApp;
use ddc_render::graphics::{self, Budgets, FrameFx, Health, Limits, PerfLevel, Tier};
use ddc_render::perf::GpuClass;
use crate::RenderState;
#[cfg(feature = "gui")]
use ddc_render::{wgpu, Renderer};
use ddc_render::{AdapterFacts, Adaptive, VisualStyle};
use std::path::{Path, PathBuf};
use web_time::{Duration, Instant};

/// How long a passing note stays in the status line.
const NOTE_FOR: Duration = Duration::from_secs(10);
/// Index of the performance page in the settings window.
const SETTINGS_PAGE: u8 = 6;
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
const PROBE_SAMPLES: u32 = 4;

pub struct GfxState {
    pub level: PerfLevel,
    pub facts: AdapterFacts,
    /// Adapter name, backend and driver for problem reports.
    pub adapter: String,
    pub probe: Option<f32>,
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    probed: bool,
    /// Recovery after a driver crash: lowest level, whatever the setting says.
    pub safe: bool,
    pub crashed: bool,
    /// Catalog key of why the start chose its back end (`gfx_guard::cause_key`), for the chip.
    pub why: String,
    /// The user asked for the graphics card at the next start.
    pub card_next: bool,
    pub tier: Tier,
    pub adaptive: Adaptive,
    pub health: Health,
    /// False until the adapter is known; such frames keep the renderer's own defaults.
    ready: bool,
    moved_last: bool,
    lowered_seen: u64,
    lowered_at: Option<Instant>,
    started: Instant,
    /// `None` until the first project; inner `None` is a project without a file.
    loaded_for: Option<Option<PathBuf>>,
    saved_style: VisualStyle,
    pub icons: ddc_ui::icons::IconCache,
}

impl Default for GfxState {
    fn default() -> Self {
        let facts = AdapterFacts::default();
        Self {
            level: PerfLevel::Auto,
            facts,
            adapter: String::new(),
            probe: None,
            probed: false,
            safe: false,
            crashed: false,
            why: String::new(),
            card_next: false,
            tier: Tier::Speed,
            adaptive: Adaptive::new(Budgets::of(Tier::Speed, facts.class)),
            health: Health::default(),
            ready: false,
            moved_last: false,
            lowered_seen: 0,
            lowered_at: None,
            started: Instant::now(),
            loaded_for: None,
            saved_style: VisualStyle::default(),
            icons: Default::default(),
        }
    }
}

impl GfxState {
    /// Tier and budgets for the level; equal budgets leave the controller alone.
    pub fn apply_level(&mut self, level: PerfLevel) {
        self.level = level;
        self.tier = graphics::resolve(level, &self.facts, self.probe, self.safe);
        self.adaptive
            .set_budgets(Budgets::of(self.tier, self.facts.class));
    }

    /// One 3D frame. `None` before the adapter is known.
    pub fn frame(&mut self, ms: f32, moving: bool, style: VisualStyle) -> Option<FrameFx> {
        if !self.ready {
            return None;
        }
        let rung = self.adaptive.frame(ms, moving);
        self.moved_last = moving;
        let lowered = self.adaptive.lowered_count();
        if lowered != self.lowered_seen {
            self.lowered_seen = lowered;
            self.lowered_at = Some(Instant::now());
        }
        // limits are applied by the renderer, the only one that knows what failed
        Some(graphics::frame_fx(
            style,
            self.adaptive.budgets(),
            &rung,
            &Limits::default(),
            moving,
        ))
    }

    /// A later frame must come even with no input: quality is below full or motion just ended.
    pub fn needs_repaint(&self) -> bool {
        self.ready && (self.moved_last || self.adaptive.needs_repaint())
    }

    fn lowered_age(&self) -> Option<Duration> {
        self.lowered_at.map(|t| t.elapsed())
    }

    /// The window draws on a software adapter (the processor), not on a graphics card.
    pub fn software(&self) -> bool {
        self.ready && self.facts.class == GpuClass::Cpu
    }

    /// Marks the adapter as known (tests and headless shots have no device).
    pub fn set_adapter_for_test(&mut self, facts: AdapterFacts) {
        self.facts = facts;
        self.ready = true;
    }
}

/// The always-visible status chip while drawing on the processor: label, tooltip, and whether
/// a click still does something (it asks for the graphics card at the next start).
pub fn soft_chip(g: &GfxState, c: &ddc_ui::Catalog) -> Option<(String, String, bool)> {
    if !g.software() {
        return None;
    }
    if g.card_next {
        return Some((c.t("gfx.chip.next"), c.t("gfx.chip.next.tip"), false));
    }
    let why = c.t(if g.why.is_empty() {
        "gfx.chip.why_unknown"
    } else {
        &g.why
    });
    Some((
        c.t("gfx.chip.soft"),
        c.tf("gfx.chip.soft.tip", &[("why", why.as_str())]),
        true,
    ))
}

/// What a click on the status note does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NoteAct {
    OpenSettings,
    NormalStart,
    SaferStart,
    /// From the software chip: the graphics card at the next start.
    TryCard,
}

/// Catalog key of the note and its action; the most important one wins.
pub fn note_key(
    safe: bool,
    health: &Health,
    level: PerfLevel,
    style: VisualStyle,
    lowered_age: Option<Duration>,
    low_tier_fresh: bool,
) -> Option<(&'static str, NoteAct)> {
    if !health.usable {
        return Some(("gfx.note.unusable", NoteAct::SaferStart));
    }
    if safe {
        return Some(("gfx.note.safe", NoteAct::NormalStart));
    }
    if style == VisualStyle::Realistic && health.realistic_failed {
        return Some(("gfx.note.realistic", NoteAct::OpenSettings));
    }
    if health.out_of_memory {
        return Some(("gfx.note.memory", NoteAct::OpenSettings));
    }
    if health.effects_failed && style.effects() {
        return Some(("gfx.note.effects", NoteAct::OpenSettings));
    }
    if level == PerfLevel::Auto {
        if lowered_age.is_some_and(|a| a < NOTE_FOR) {
            return Some(("gfx.note.lowered", NoteAct::OpenSettings));
        }
        if low_tier_fresh {
            return Some(("gfx.note.low_tier", NoteAct::OpenSettings));
        }
    }
    None
}

/// The status line's note: text, tooltip and what a click does.
pub fn status_note(a: &EditorApp, c: &ddc_ui::Catalog) -> Option<(String, String, NoteAct)> {
    let g = &a.view.gfx;
    let low = g.ready && g.tier == Tier::Speed && g.started.elapsed() < NOTE_FOR;
    let (key, act) = note_key(
        g.safe,
        &g.health,
        g.level,
        a.view.nav.style,
        g.lowered_age(),
        low,
    )?;
    Some((c.t(key), c.t(&format!("{key}.tip")), act))
}

pub fn note_clicked(a: &mut EditorApp, act: NoteAct) {
    match act {
        NoteAct::OpenSettings => {
            a.settings_page = SETTINGS_PAGE;
            a.run("mgr.settings");
        }
        NoteAct::NormalStart => {
            if let Some(d) = crate::gfx::data_dir() {
                crate::gfx_guard::reset(&d);
            }
            a.view.gfx.safe = false;
            a.view.gfx.crashed = false;
        }
        NoteAct::TryCard => {
            if let Some(d) = crate::gfx::data_dir() {
                crate::gfx_guard::reset(&d);
            }
            a.view.gfx.card_next = true;
            let text = crate::tr("gfx.chip.next.tip", &[]);
            a.sh.toasts.push(ddc_ui::notify::Level::Info, text, None);
        }
        // the window's recovery ladder; a headless build has no window to start
        #[cfg(feature = "gui")]
        NoteAct::SaferStart => {
            if let Some(d) = crate::gfx::data_dir() {
                let last = crate::gfx::recovery_ladder().len() as u32 - 1;
                crate::gfx_guard::step_down(&d, last);
            }
        }
        #[cfg(not(feature = "gui"))]
        NoteAct::SaferStart => {}
    }
}

/// Each frame, before the 3D view is drawn.
pub fn sync(a: &mut EditorApp, rs: Option<&RenderState>) {
    let level = PerfLevel::parse(&a.prefs.gfx_level).unwrap_or_default();
    #[cfg(feature = "gui")]
    if let Some(rs) = rs {
        let g = &mut a.view.gfx;
        if !g.ready {
            g.facts = adapter_facts(rs);
            g.adapter = adapter_id(&rs.adapter.get_info());
            g.ready = true;
        }
        // `Auto` alone needs the measurement; it covers startup and a later switch to `Auto`
        if level == PerfLevel::Auto && !g.probed && !g.safe {
            g.probed = true;
            let id = adapter_id(&rs.adapter.get_info());
            g.probe = cached_probe(&a.prefs.gfx_probe, &id);
            if g.probe.is_none() {
                g.probe = ddc_render::gfx_probe::measure(&rs.device, &rs.queue, PROBE_SAMPLES);
                if let Some(ms) = g.probe {
                    a.prefs.gfx_probe = format!("{ms:.3}|{id}");
                    a.prefs.save();
                }
            }
        }
        let health = rs
            .renderer
            .read()
            .callback_resources
            .get::<Renderer>()
            .map(Renderer::health);
        if let Some(h) = health {
            a.view.gfx.health = h;
        }
    }
    #[cfg(not(feature = "gui"))]
    let _ = rs;
    a.view.gfx.apply_level(level);
    sync_style(a);
}

#[cfg(feature = "gui")]
fn adapter_facts(rs: &RenderState) -> AdapterFacts {
    let info = rs.adapter.get_info();
    AdapterFacts {
        class: GpuClass::of(info.device_type),
        gl: info.backend == wgpu::Backend::Gl,
        max_texture: rs.device.limits().max_texture_dimension_2d,
        system_ram_mb: system_ram_mb(),
    }
}

#[cfg(feature = "gui")]
/// A new driver or adapter measures again. The prefs file is line based.
fn adapter_id(i: &wgpu::AdapterInfo) -> String {
    let id = format!("{} {:?} {}", i.name, i.backend, i.driver_info);
    id.replace(['\n', '\r', '|'], " ").trim().to_string()
}

#[cfg_attr(not(feature = "gui"), allow(dead_code))]
fn cached_probe(saved: &str, id: &str) -> Option<f32> {
    let (ms, who) = saved.split_once('|')?;
    if who != id {
        return None;
    }
    ms.parse().ok().filter(|m: &f32| m.is_finite() && *m > 0.0)
}

#[cfg(windows)]
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
fn system_ram_mb() -> u64 {
    #[repr(C)]
    struct MemoryStatus {
        length: u32,
        load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page: u64,
        avail_page: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended: u64,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatus) -> i32;
    }
    let mut m = MemoryStatus {
        length: std::mem::size_of::<MemoryStatus>() as u32,
        load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page: 0,
        avail_page: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended: 0,
    };
    // SAFETY: the struct has the documented layout and its length field is set.
    if unsafe { GlobalMemoryStatusEx(&mut m) } == 0 {
        return 0;
    }
    m.total_phys >> 20
}

#[cfg(not(windows))]
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
fn system_ram_mb() -> u64 {
    0
}

/// Stable number of a style in the project file; `VisualStyle::code` is shared by the lit styles.
pub fn style_code(s: VisualStyle) -> i64 {
    match s {
        VisualStyle::ShadedEdges => 0,
        VisualStyle::Wireframe => 1,
        VisualStyle::HiddenLine => 2,
        VisualStyle::Shaded => 3,
        VisualStyle::Realistic => 4,
        VisualStyle::Consistent => 5,
        VisualStyle::XRay => 6,
    }
}

pub fn style_from_code(c: i64) -> VisualStyle {
    VisualStyle::ALL
        .into_iter()
        .find(|s| style_code(*s) == c)
        .unwrap_or_default()
}

/// Style of a project just opened. Without a saved one (older projects) it is the default, except
/// at the very first project, where a style already set from the command line stays.
pub fn style_for_project(stored: Option<i64>, current: VisualStyle, first: bool) -> VisualStyle {
    match stored {
        Some(c) => style_from_code(c),
        None if first => current,
        None => VisualStyle::default(),
    }
}

fn sync_style(a: &mut EditorApp) {
    let work = a.ed.project.work_path();
    let same = a
        .view
        .gfx
        .loaded_for
        .as_ref()
        .is_some_and(|p| p.as_deref() == work);
    if !same {
        let path = work.map(Path::to_path_buf);
        let stored = a.ed.model.view3d_style_code().ok().flatten();
        let first = a.view.gfx.loaded_for.is_none();
        let style = style_for_project(stored, a.view.nav.style, first);
        a.view.nav.style = style;
        a.view.gfx.saved_style = style;
        a.view.gfx.loaded_for = Some(path);
        return;
    }
    let style = a.view.nav.style;
    if style != a.view.gfx.saved_style {
        a.view.gfx.saved_style = style;
        // a read-only project keeps the style for this session only
        let _ = a.ed.model.set_view3d_style_code(style_code(style));
    }
}

/// `--style KEY`, `--gfx-level KEY` for screenshots.
pub fn apply_cli(a: &mut EditorApp, argv: &[String]) {
    let val = |f: &str| {
        argv.iter()
            .position(|s| s == f)
            .and_then(|i| argv.get(i + 1))
    };
    if let Some(s) = val("--style").and_then(|k| VisualStyle::parse(k)) {
        a.view.nav.style = s;
    }
    if let Some(l) = val("--gfx-level").and_then(|k| PerfLevel::parse(k)) {
        a.prefs.gfx_level = l.key().into();
    }
}

pub fn style_icon(s: VisualStyle) -> &'static str {
    match s {
        VisualStyle::ShadedEdges => "style-shaded",
        VisualStyle::Shaded => "style-plain",
        VisualStyle::Realistic => "style-realistic",
        VisualStyle::Consistent => "style-consistent",
        VisualStyle::HiddenLine => "style-hidden",
        VisualStyle::Wireframe => "style-wire",
        VisualStyle::XRay => "style-xray",
    }
}

/// A switcher button of the view bar: the bar lives inside `Viewport`, without the shell's theme,
/// so colours come from the egui visuals the theme installed, like the bar's other controls.
pub fn style_button(
    ui: &mut egui::Ui,
    icons: &mut ddc_ui::icons::IconCache,
    icon: &str,
    on: bool,
    tip: &str,
) -> egui::Response {
    const SIDE: f32 = 24.0;
    const ICON: f32 = 16.0;
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(SIDE), egui::Sense::click());
    let vis = ui.style().interact_selectable(&resp, on);
    if on || resp.hovered() {
        ui.painter()
            .rect_filled(rect, vis.corner_radius, vis.weak_bg_fill);
    }
    let ic = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(ICON));
    let name = ddc_ui::widgets::icon_name(icon, on);
    icons.paint(ui, &name, ICON as u32, ic, vis.fg_stroke.color);
    resp.on_hover_text(tip)
}

fn set_level(a: &mut EditorApp, l: PerfLevel) {
    a.prefs.gfx_level = l.key().into();
    a.prefs.save();
    a.view.gfx.apply_level(l);
}

fn set_style(a: &mut EditorApp, s: VisualStyle) {
    a.view.nav.style = s;
    a.view3d = true;
}

fn h_plain(a: &mut EditorApp) {
    set_style(a, VisualStyle::Shaded);
}
fn h_realistic(a: &mut EditorApp) {
    set_style(a, VisualStyle::Realistic);
}
fn h_auto(a: &mut EditorApp) {
    set_level(a, PerfLevel::Auto);
}
fn h_speed(a: &mut EditorApp) {
    set_level(a, PerfLevel::Speed);
}
fn h_balanced(a: &mut EditorApp) {
    set_level(a, PerfLevel::Balanced);
}
fn h_quality(a: &mut EditorApp) {
    set_level(a, PerfLevel::Quality);
}

/// Commands of this module (`commands.d/gfx1.json`).
pub fn register(a: &mut EditorApp) {
    let list: &[(&str, crate::shell::Handler)] = &[
        ("view.style_shaded_plain", h_plain),
        ("view.style_realistic", h_realistic),
        ("view.gfx_auto", h_auto),
        ("view.gfx_speed", h_speed),
        ("view.gfx_balanced", h_balanced),
        ("view.gfx_quality", h_quality),
    ];
    for (id, h) in list {
        a.register_command(id, *h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_numbers_are_distinct_and_round_trip() {
        for s in VisualStyle::ALL {
            assert_eq!(style_from_code(style_code(s)), s);
        }
        assert_eq!(style_from_code(99), VisualStyle::default());
    }

    #[test]
    fn projects_without_a_style_get_the_default() {
        let d = VisualStyle::default();
        assert_eq!(d, VisualStyle::ShadedEdges);
        let r = VisualStyle::Realistic;
        assert_eq!(style_for_project(None, r, false), d);
        // a style from the command line survives the first project
        assert_eq!(style_for_project(None, r, true), r);
        assert_eq!(
            style_for_project(Some(style_code(VisualStyle::Wireframe)), r, true),
            VisualStyle::Wireframe
        );
    }

    #[test]
    fn probe_cache_belongs_to_one_adapter() {
        assert_eq!(cached_probe("1.500|gpu a", "gpu a"), Some(1.5));
        assert_eq!(cached_probe("1.500|gpu a", "gpu b"), None);
        assert_eq!(cached_probe("nan|gpu a", "gpu a"), None);
        assert_eq!(cached_probe("", "gpu a"), None);
        // an empty driver string must not leave a trailing blank: prefs trim it on load
        let mut info = wgpu::AdapterInfo {
            name: "gpu a".into(),
            vendor: 0,
            device: 0,
            device_type: wgpu::DeviceType::DiscreteGpu,
            driver: String::new(),
            driver_info: String::new(),
            backend: wgpu::Backend::Dx12,
        };
        let saved = crate::prefs::Prefs::parse(&format!("gfx_probe=1.500|{}\n", adapter_id(&info)));
        assert_eq!(cached_probe(&saved.gfx_probe, &adapter_id(&info)), Some(1.5));
        info.driver_info = "31.0".into();
        assert_eq!(adapter_id(&info), "gpu a Dx12 31.0");
    }

    #[test]
    fn notes_rank_and_auto_only_speaks_for_itself() {
        let h = Health::default();
        let s = VisualStyle::ShadedEdges;
        let fresh = Some(Duration::from_secs(1));
        let k = |safe, h: &Health, l, st, age, low| note_key(safe, h, l, st, age, low).map(|n| n.0);
        assert_eq!(k(false, &h, PerfLevel::Auto, s, None, false), None);
        assert_eq!(
            k(false, &h, PerfLevel::Auto, s, fresh, false),
            Some("gfx.note.lowered")
        );
        assert_eq!(
            k(false, &h, PerfLevel::Auto, s, Some(NOTE_FOR), true),
            Some("gfx.note.low_tier")
        );
        assert_eq!(k(false, &h, PerfLevel::Quality, s, fresh, true), None);
        let bad = Health {
            realistic_failed: true,
            ..h
        };
        assert_eq!(k(false, &bad, PerfLevel::Quality, s, None, false), None);
        assert_eq!(
            k(
                false,
                &bad,
                PerfLevel::Quality,
                VisualStyle::Realistic,
                None,
                false
            ),
            Some("gfx.note.realistic")
        );
        let (key, act) = note_key(true, &bad, PerfLevel::Auto, s, fresh, true).unwrap();
        assert_eq!((key, act), ("gfx.note.safe", NoteAct::NormalStart));
    }

    #[test]
    fn frames_keep_renderer_defaults_until_the_adapter_is_known() {
        let mut g = GfxState::default();
        assert_eq!(g.frame(16.0, true, VisualStyle::Shaded), None);
        assert!(!g.needs_repaint());
        g.ready = true;
        g.facts = AdapterFacts {
            class: GpuClass::Discrete,
            ..AdapterFacts::default()
        };
        g.apply_level(PerfLevel::Balanced);
        assert!(g.frame(16.0, true, VisualStyle::Shaded).is_some());
        // the frame after motion must come: edges and effects return there
        assert!(g.needs_repaint());
        g.frame(16.0, false, VisualStyle::Shaded);
        assert!(!g.needs_repaint());
    }

    #[test]
    fn safe_start_forces_the_lowest_tier() {
        let mut g = GfxState {
            safe: true,
            ..GfxState::default()
        };
        g.apply_level(PerfLevel::Quality);
        assert_eq!(g.tier, Tier::Speed);
        g.safe = false;
        g.apply_level(PerfLevel::Quality);
        assert_eq!(g.tier, Tier::Quality);
    }
}
