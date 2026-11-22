// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Performance levels and frame budgets, without a GPU: tier choice from adapter facts and a
//! measured probe, adaptive motion controller, fallback limits. Contract: docs/design/GRAPHICS_MODES.md.

use crate::perf::GpuClass;
use crate::style::VisualStyle;

/// The per-machine setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PerfLevel {
    #[default]
    Auto,
    Speed,
    Balanced,
    Quality,
}

impl PerfLevel {
    pub const ALL: [PerfLevel; 4] = [Self::Auto, Self::Speed, Self::Balanced, Self::Quality];
    pub fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Speed => "speed",
            Self::Balanced => "balanced",
            Self::Quality => "quality",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.key() == s)
    }
    /// The older `effects` preference.
    pub fn from_legacy(effects: &str) -> Self {
        match effects {
            "on" | "high" => Self::Quality,
            "medium" => Self::Balanced,
            "off" => Self::Speed,
            _ => Self::Auto,
        }
    }
}

/// A concrete level after `Auto` is resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Speed,
    Balanced,
    Quality,
}

/// Ambient occlusion: samples and resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AoMode {
    Off,
    /// 6 samples, half resolution.
    Low,
    /// 10 samples, half resolution.
    High,
    /// 10 samples, full resolution.
    Ultra,
}

impl AoMode {
    /// `FrameUniforms::params[2]`: 0 off, 1 six samples, 2 ten samples and the wide shadow filter.
    pub fn code(self) -> f32 {
        match self {
            Self::Off => 0.0,
            Self::Low => 1.0,
            Self::High | Self::Ultra => 2.0,
        }
    }
    pub fn divisor(self) -> u32 {
        if self == Self::Ultra {
            1
        } else {
            2
        }
    }
}

/// Explicit budgets of a level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budgets {
    pub render_scale: f32,
    /// Lowest scale the controller may reach while moving.
    pub min_scale: f32,
    /// 1 or 4.
    pub samples: u32,
    /// Shadow map side, 0 is no shadows.
    pub shadow_px: u32,
    pub ao: AoMode,
    pub tol_px: f32,
    pub moving_scale: f32,
    pub edges_moving: bool,
    /// While moving, big models only: chunks farther than this share of the scene diagonal are
    /// skipped; 0 is no limit.
    pub draw_distance: f32,
    pub target_ms: f32,
    /// Image textures in the realistic style; the Speed level draws flat colours.
    pub textures: bool,
}

impl Budgets {
    pub fn of(tier: Tier, class: GpuClass) -> Self {
        let mut b = match tier {
            Tier::Speed => Self {
                render_scale: 0.85,
                min_scale: 0.5,
                samples: 1,
                shadow_px: 0,
                ao: AoMode::Off,
                tol_px: 1.5,
                moving_scale: 6.0,
                edges_moving: false,
                draw_distance: 0.6,
                target_ms: 16.7,
                textures: false,
            },
            Tier::Balanced => Self {
                render_scale: 1.0,
                min_scale: 0.6,
                samples: 4,
                shadow_px: 1024,
                ao: AoMode::Low,
                tol_px: 0.5,
                moving_scale: 3.0,
                edges_moving: true,
                draw_distance: 0.0,
                target_ms: 20.0,
                textures: true,
            },
            Tier::Quality => Self {
                render_scale: 1.0,
                min_scale: 0.7,
                samples: 4,
                shadow_px: 2048,
                ao: AoMode::Ultra,
                tol_px: 0.25,
                moving_scale: 2.0,
                edges_moving: true,
                draw_distance: 0.0,
                target_ms: 33.0,
                textures: true,
            },
        };
        // a CPU rasterizer pays per pixel: stay usable rather than sharp
        if class == GpuClass::Cpu && tier == Tier::Speed {
            b.render_scale = 0.75;
            b.min_scale = 0.4;
            b.target_ms = 33.0;
        }
        b
    }
}

/// What the adapter reports. `system_ram_mb` is 0 when unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdapterFacts {
    pub class: GpuClass,
    pub gl: bool,
    pub max_texture: u32,
    pub system_ram_mb: u64,
}

impl Default for AdapterFacts {
    fn default() -> Self {
        Self {
            class: GpuClass::Integrated,
            gl: false,
            max_texture: 8192,
            system_ram_mb: 0,
        }
    }
}

/// Probe cost at or under this (ms per megapixel) can hold Maximum quality.
pub const PROBE_QUALITY_MS: f32 = 1.2;
/// Probe cost at or under this can hold Balanced.
pub const PROBE_BALANCED_MS: f32 = 4.0;
const LOW_RAM_MB: u64 = 6 * 1024;

/// `Auto`: the measured probe decides and the facts cap it. Without a probe, stay conservative.
pub fn pick_tier(f: &AdapterFacts, probe_ms_per_mpx: Option<f32>) -> Tier {
    if f.class == GpuClass::Cpu || f.max_texture < 4096 {
        return Tier::Speed;
    }
    let measured = match probe_ms_per_mpx.filter(|m| m.is_finite() && *m > 0.0) {
        Some(m) if m <= PROBE_QUALITY_MS => Tier::Quality,
        Some(m) if m <= PROBE_BALANCED_MS => Tier::Balanced,
        Some(_) => Tier::Speed,
        None if f.class == GpuClass::Discrete => Tier::Balanced,
        None => Tier::Speed,
    };
    let low_ram = f.system_ram_mb != 0 && f.system_ram_mb < LOW_RAM_MB;
    if f.gl || low_ram {
        measured.min(Tier::Balanced)
    } else {
        measured
    }
}

/// The tier for a level; `safe` is recovery after a driver crash and wins over the setting.
pub fn resolve(level: PerfLevel, f: &AdapterFacts, probe: Option<f32>, safe: bool) -> Tier {
    if safe {
        return Tier::Speed;
    }
    match level {
        PerfLevel::Auto => pick_tier(f, probe),
        PerfLevel::Speed => Tier::Speed,
        PerfLevel::Balanced => Tier::Balanced,
        PerfLevel::Quality => Tier::Quality,
    }
}

/// One step of the motion ladder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rung {
    pub scale: f32,
    pub samples: u32,
    pub ao: bool,
    pub shadows_live: bool,
    pub materials: bool,
    pub edges: bool,
}

/// Full quality first; cheap losses before visible ones, render scale last.
pub fn ladder(b: &Budgets) -> Vec<Rung> {
    let mut r = Rung {
        scale: b.render_scale,
        samples: b.samples,
        ao: b.ao != AoMode::Off,
        shadows_live: b.shadow_px > 0,
        materials: true,
        edges: true,
    };
    let mut v = vec![r];
    let push = |r: Rung, v: &mut Vec<Rung>| {
        if v.last() != Some(&r) {
            v.push(r);
        }
    };
    r.ao = false;
    push(r, &mut v);
    r.shadows_live = false;
    r.materials = false;
    push(r, &mut v);
    r.edges = false;
    push(r, &mut v);
    for f in [0.85, 0.7, 0.6, 0.5, 0.4] {
        let s = b.render_scale * f;
        if s >= b.min_scale - 1e-4 {
            r.scale = s;
            r.samples = 1;
            push(r, &mut v);
        }
    }
    v
}

/// Idle time before quality comes back.
pub const REST_MS: f32 = 150.0;
const EMA: f32 = 0.25;
const DOWN_AT: f32 = 1.1;
const UP_AT: f32 = 0.6;
const DOWN_FRAMES: u32 = 3;
const UP_FRAMES: u32 = 30;
const MIN_HOLD: u32 = 4;
const LONGEST_SAMPLE: f32 = 200.0;

/// Holds the frame-time target during navigation and restores everything after it.
#[derive(Clone, Debug)]
pub struct Adaptive {
    budgets: Budgets,
    rungs: Vec<Rung>,
    rung: usize,
    last_rung: usize,
    ema: f32,
    over: u32,
    under: u32,
    hold: u32,
    idle_ms: f32,
    moving: bool,
    lowered: u64,
}

impl Adaptive {
    pub fn new(budgets: Budgets) -> Self {
        Self {
            budgets,
            rungs: ladder(&budgets),
            rung: 0,
            last_rung: 0,
            ema: 0.0,
            over: 0,
            under: 0,
            hold: 0,
            idle_ms: 0.0,
            moving: false,
            lowered: 0,
        }
    }

    pub fn budgets(&self) -> &Budgets {
        &self.budgets
    }

    /// New budgets start from full quality; equal ones keep the current state.
    pub fn set_budgets(&mut self, b: Budgets) {
        if self.budgets != b {
            let lowered = self.lowered;
            *self = Self::new(b);
            self.lowered = lowered;
        }
    }

    pub fn current(&self) -> Rung {
        self.rungs[self.rung]
    }

    /// Below full quality: a later frame must come to restore it.
    pub fn needs_repaint(&self) -> bool {
        self.rung > 0
    }

    /// Grows each time the controller lowers quality (for the status note).
    pub fn lowered_count(&self) -> u64 {
        self.lowered
    }

    /// One frame: `ms` since the previous one, `moving` is camera motion in this frame.
    pub fn frame(&mut self, ms: f32, moving: bool) -> Rung {
        if !moving {
            if self.moving {
                self.moving = false;
                self.last_rung = self.rung;
            }
            self.idle_ms += ms.clamp(0.0, 1000.0);
            if self.idle_ms >= REST_MS {
                self.rung = 0;
            }
            return self.current();
        }
        self.idle_ms = 0.0;
        if !self.moving {
            // this frame's time includes the idle before it: not a measurement
            self.moving = true;
            self.rung = self.rung.max(self.last_rung.saturating_sub(1));
            self.ema = 0.0;
            self.over = 0;
            self.under = 0;
            self.hold = 0;
            return self.current();
        }
        if !ms.is_finite() || ms < 1.0 {
            return self.current();
        }
        let s = ms.min(LONGEST_SAMPLE);
        self.ema = if self.ema == 0.0 {
            s
        } else {
            self.ema + (s - self.ema) * EMA
        };
        self.hold = self.hold.saturating_add(1);
        let target = self.budgets.target_ms;
        self.over = if self.ema > target * DOWN_AT {
            self.over + 1
        } else {
            0
        };
        self.under = if self.ema < target * UP_AT {
            self.under + 1
        } else {
            0
        };
        let top = self.rungs.len() - 1;
        if self.over >= DOWN_FRAMES && self.hold >= MIN_HOLD && self.rung < top {
            // far over target: two rungs, or a slow machine takes seconds to settle
            let by = if self.ema > target * 2.0 { 2 } else { 1 };
            self.rung = (self.rung + by).min(top);
            self.lowered += 1;
            self.over = 0;
            self.hold = 0;
        } else if self.under >= UP_FRAMES && self.rung > 0 {
            self.rung -= 1;
            self.under = 0;
            self.hold = 0;
        }
        self.current()
    }
}

/// Everything the renderer needs for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameFx {
    pub scale: f32,
    pub samples: u32,
    pub shadow_px: u32,
    pub shadows_live: bool,
    /// Occlusion this frame, and the mode its targets are allocated for: dropping it during
    /// motion must not reallocate.
    pub ao: AoMode,
    pub ao_alloc: AoMode,
    /// Crease edges of chunks that are not selected.
    pub edges: bool,
    /// Depth-jump silhouette in the composite.
    pub contour: bool,
    pub realistic: bool,
    /// Image textures on realistic faces (flat colours when false).
    pub textures: bool,
    pub materials: bool,
    pub tol_px: f32,
    pub moving_scale: f32,
    pub draw_distance: f32,
    /// Sway of foliage, m at 10 m above the base; 0 keeps it still (headless and report shots).
    pub wind: f32,
}

impl FrameFx {
    /// The look of the older `Quality` setting, for callers without a level (headless reports).
    pub fn from_quality(q: crate::renderer::Quality) -> Self {
        use crate::renderer::Quality;
        let d = crate::perf::PerfConfig::default();
        Self {
            scale: 1.0,
            samples: 4,
            shadow_px: match q {
                Quality::Off => 0,
                Quality::Medium => 1024,
                Quality::High => 2048,
            },
            shadows_live: true,
            ao: if q == Quality::High {
                AoMode::High
            } else {
                AoMode::Off
            },
            ao_alloc: AoMode::High,
            edges: true,
            contour: true,
            realistic: false,
            textures: false,
            materials: true,
            tol_px: d.tol_px,
            moving_scale: d.moving_scale,
            draw_distance: 0.0,
            wind: 0.0,
        }
    }

    /// Internal target size for a window-sized view.
    pub fn scaled(&self, full: (u32, u32)) -> (u32, u32) {
        let s = self.scale.clamp(0.1, 1.0);
        (
            ((full.0 as f32 * s).round() as u32).max(1),
            ((full.1 as f32 * s).round() as u32).max(1),
        )
    }
}

/// Line-only styles stay readable: scale never drops under this.
const LINE_STYLE_MIN_SCALE: f32 = 0.75;

/// Frame settings from the style, the level's budgets, the controller's rung and fallback limits.
pub fn frame_fx(style: VisualStyle, b: &Budgets, r: &Rung, lim: &Limits, moving: bool) -> FrameFx {
    let lines = matches!(style, VisualStyle::Wireframe | VisualStyle::HiddenLine);
    let scale = if lines {
        r.scale.max(LINE_STYLE_MIN_SCALE.min(b.render_scale))
    } else {
        r.scale
    };
    let edges_now = r.edges && (b.edges_moving || !moving);
    let ao_alloc = if lim.ao { b.ao } else { AoMode::Off };
    FrameFx {
        scale: scale.min(lim.max_scale),
        samples: r.samples.min(lim.max_samples).max(1),
        shadow_px: b.shadow_px.min(lim.max_shadow),
        shadows_live: r.shadows_live,
        ao: if r.ao { ao_alloc } else { AoMode::Off },
        ao_alloc,
        edges: style.has_edges() && (lines || edges_now),
        contour: style.has_edges(),
        realistic: style == VisualStyle::Realistic && lim.realistic,
        textures: style == VisualStyle::Realistic && lim.realistic && b.textures,
        materials: r.materials,
        tol_px: b.tol_px,
        moving_scale: b.moving_scale,
        draw_distance: b.draw_distance,
        wind: 0.0,
    }
}

/// Sway of the crowns in the window, m at 10 m above the base.
pub const WIND_SWAY: f32 = 0.06;

/// What failures left available. Starts open; only ever narrows for a device's lifetime.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub max_samples: u32,
    pub max_scale: f32,
    pub max_shadow: u32,
    pub ao: bool,
    pub realistic: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_samples: 4,
            max_scale: 1.0,
            max_shadow: 4096,
            ao: true,
            realistic: true,
        }
    }
}

impl Limits {
    /// A frame's settings within what is still available.
    pub fn apply(&self, mut fx: FrameFx) -> FrameFx {
        fx.scale = fx.scale.min(self.max_scale);
        fx.samples = fx.samples.min(self.max_samples).max(1);
        fx.shadow_px = fx.shadow_px.min(self.max_shadow);
        if !self.ao {
            fx.ao = AoMode::Off;
            fx.ao_alloc = AoMode::Off;
        }
        fx.realistic = fx.realistic && self.realistic;
        fx.textures = fx.textures && fx.realistic;
        fx
    }

    /// Target allocation failed: give up one thing; false means nothing is left to give up.
    pub fn on_out_of_memory(&mut self, used_shadow: u32) -> bool {
        if self.max_samples > 1 {
            self.max_samples = 1;
        } else if used_shadow > 1024 {
            self.max_shadow = 1024;
        } else if used_shadow > 0 {
            self.max_shadow = 0;
        } else if self.ao {
            self.ao = false;
        } else if self.max_scale > 0.36 {
            self.max_scale *= 0.7;
        } else {
            return false;
        }
        true
    }
}

/// Renderer state the UI reports. `usable` false means nothing can be drawn on this device.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Health {
    pub usable: bool,
    pub realistic_failed: bool,
    /// The texture array was refused (memory or limits): flat colours.
    pub textures_failed: bool,
    pub effects_failed: bool,
    pub out_of_memory: bool,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            usable: true,
            realistic_failed: false,
            textures_failed: false,
            effects_failed: false,
            out_of_memory: false,
        }
    }
}

/// Resting frame limit of the measurement tool, ms.
pub const BENCH_REST_MS: f64 = 250.0;

/// The budgets the measurement tool checks.
pub fn bench_pass(b: &Budgets, moving_p95: f64, still_p95: f64) -> bool {
    moving_p95 <= f64::from(b.target_ms) && still_p95 <= BENCH_REST_MS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::Quality;

    fn facts(class: GpuClass) -> AdapterFacts {
        AdapterFacts {
            class,
            gl: false,
            max_texture: 16384,
            system_ram_mb: 32 * 1024,
        }
    }

    #[test]
    fn level_keys_round_trip_and_legacy_maps() {
        for l in PerfLevel::ALL {
            assert_eq!(PerfLevel::parse(l.key()), Some(l));
        }
        assert_eq!(PerfLevel::parse("ultra"), None);
        assert_eq!(PerfLevel::from_legacy("off"), PerfLevel::Speed);
        assert_eq!(PerfLevel::from_legacy("medium"), PerfLevel::Balanced);
        assert_eq!(PerfLevel::from_legacy("high"), PerfLevel::Quality);
        assert_eq!(PerfLevel::from_legacy("on"), PerfLevel::Quality);
        assert_eq!(PerfLevel::from_legacy("auto"), PerfLevel::Auto);
    }

    #[test]
    fn tier_comes_from_the_measurement_not_the_type() {
        let i = facts(GpuClass::Integrated);
        let d = facts(GpuClass::Discrete);
        assert_eq!(pick_tier(&i, Some(0.8)), Tier::Quality);
        assert_eq!(pick_tier(&d, Some(9.0)), Tier::Speed);
        assert_eq!(pick_tier(&d, Some(2.0)), Tier::Balanced);
    }

    #[test]
    fn tier_caps_and_missing_probe() {
        assert_eq!(pick_tier(&facts(GpuClass::Cpu), Some(0.1)), Tier::Speed);
        let mut f = facts(GpuClass::Discrete);
        assert_eq!(pick_tier(&f, None), Tier::Balanced);
        assert_eq!(pick_tier(&facts(GpuClass::Integrated), None), Tier::Speed);
        assert_eq!(pick_tier(&f, Some(f32::NAN)), Tier::Balanced);
        f.gl = true;
        assert_eq!(pick_tier(&f, Some(0.5)), Tier::Balanced);
        f.gl = false;
        f.system_ram_mb = 4096;
        assert_eq!(pick_tier(&f, Some(0.5)), Tier::Balanced);
        f.system_ram_mb = 0;
        assert_eq!(pick_tier(&f, Some(0.5)), Tier::Quality);
        f.max_texture = 2048;
        assert_eq!(pick_tier(&f, Some(0.5)), Tier::Speed);
    }

    #[test]
    fn safe_start_wins_over_any_level() {
        let f = facts(GpuClass::Discrete);
        for l in PerfLevel::ALL {
            assert_eq!(resolve(l, &f, Some(0.1), true), Tier::Speed);
        }
        assert_eq!(resolve(PerfLevel::Quality, &f, None, false), Tier::Quality);
    }

    #[test]
    fn software_budgets_are_lighter_and_levels_are_ordered() {
        let cpu = Budgets::of(Tier::Speed, GpuClass::Cpu);
        let int = Budgets::of(Tier::Speed, GpuClass::Integrated);
        assert!(cpu.render_scale < int.render_scale && cpu.target_ms > int.target_ms);
        let (b, q) = (
            Budgets::of(Tier::Balanced, GpuClass::Discrete),
            Budgets::of(Tier::Quality, GpuClass::Discrete),
        );
        assert!(int.shadow_px < b.shadow_px && b.shadow_px < q.shadow_px);
        assert!(int.ao < b.ao && b.ao < q.ao);
        assert!(int.samples == 1 && q.samples == 4);
    }

    #[test]
    fn ladder_starts_full_ends_low_and_respects_the_floor() {
        for t in [Tier::Speed, Tier::Balanced, Tier::Quality] {
            let b = Budgets::of(t, GpuClass::Discrete);
            let l = ladder(&b);
            assert_eq!(l[0].scale, b.render_scale);
            assert_eq!(l[0].samples, b.samples);
            let last = l[l.len() - 1];
            assert!(last.scale >= b.min_scale - 1e-3 && last.scale < b.render_scale);
            assert!(!last.ao && !last.edges && last.samples == 1);
            assert!(l
                .windows(2)
                .all(|w| w[0] != w[1] && w[1].scale <= w[0].scale));
        }
    }

    fn run(a: &mut Adaptive, ms: f32, n: usize) -> Rung {
        let mut r = a.current();
        for _ in 0..n {
            r = a.frame(ms, true);
        }
        r
    }

    #[test]
    fn slow_frames_lower_and_rest_restores_at_once() {
        let b = Budgets::of(Tier::Balanced, GpuClass::Discrete);
        let mut a = Adaptive::new(b);
        let full = a.current();
        assert!(!a.needs_repaint());
        let low = run(&mut a, 60.0, 80);
        assert!(low.scale < full.scale, "reached scale steps: {low:?}");
        assert!(a.lowered_count() > 0 && a.needs_repaint());
        // a short pause in a drag does not pop
        assert_eq!(a.frame(50.0, false), low);
        assert_eq!(a.frame(120.0, false), full);
        assert!(!a.needs_repaint());
    }

    #[test]
    fn fast_frames_never_lower_and_vsync_is_not_slow() {
        let mut a = Adaptive::new(Budgets::of(Tier::Speed, GpuClass::Discrete));
        let full = a.current();
        assert_eq!(run(&mut a, 16.7, 300), full);
        assert_eq!(a.lowered_count(), 0);
    }

    #[test]
    fn first_frame_after_idle_is_not_measured() {
        let mut a = Adaptive::new(Budgets::of(Tier::Balanced, GpuClass::Discrete));
        let full = a.current();
        a.frame(16.0, false);
        assert_eq!(a.frame(5000.0, true), full);
        assert_eq!(a.frame(16.0, true), full);
    }

    #[test]
    fn no_oscillation_and_recovery_while_moving() {
        let b = Budgets::of(Tier::Balanced, GpuClass::Discrete);
        let mut a = Adaptive::new(b);
        run(&mut a, 30.0, 30);
        let lowered = a.lowered_count();
        // inside the band between the thresholds nothing changes
        let mid = b.target_ms * 0.9;
        let held = run(&mut a, mid, 30);
        let c = a.lowered_count();
        assert_eq!(run(&mut a, mid, 200), held);
        assert_eq!(a.lowered_count(), c);
        assert!(lowered > 0);
        // clearly fast: climbs back rung by rung
        let top = run(&mut a, 5.0, 2000);
        assert_eq!(top, ladder(&b)[0]);
    }

    #[test]
    fn next_motion_resumes_one_rung_lower() {
        let b = Budgets::of(Tier::Balanced, GpuClass::Discrete);
        let l = ladder(&b);
        let mut a = Adaptive::new(b);
        let low = run(&mut a, 200.0, 200);
        let i = l.iter().position(|r| *r == low).unwrap();
        a.frame(16.0, false);
        a.frame(200.0, false);
        assert_eq!(a.current(), l[0]);
        assert_eq!(a.frame(16.0, true), l[i - 1]);
    }

    #[test]
    fn budgets_change_resets_only_when_different() {
        let b = Budgets::of(Tier::Balanced, GpuClass::Discrete);
        let mut a = Adaptive::new(b);
        let low = run(&mut a, 200.0, 50);
        a.set_budgets(b);
        assert_eq!(a.current(), low);
        a.set_budgets(Budgets::of(Tier::Speed, GpuClass::Discrete));
        assert_eq!(a.current(), ladder(a.budgets())[0]);
        assert!(a.lowered_count() > 0);
    }

    #[test]
    fn style_decides_edges_and_line_styles_keep_them() {
        let b = Budgets::of(Tier::Speed, GpuClass::Discrete);
        let top = ladder(&b)[0];
        let lim = Limits::default();
        assert!(!frame_fx(VisualStyle::Shaded, &b, &top, &lim, false).edges);
        assert!(frame_fx(VisualStyle::ShadedEdges, &b, &top, &lim, false).edges);
        assert!(!frame_fx(VisualStyle::ShadedEdges, &b, &top, &lim, true).edges);
        for s in [VisualStyle::Wireframe, VisualStyle::HiddenLine] {
            let f = frame_fx(s, &b, ladder(&b).last().unwrap(), &lim, true);
            assert!(f.edges, "{s:?} is nothing but edges");
            assert!(f.scale >= 0.75);
        }
    }

    #[test]
    fn realistic_falls_back_and_limits_apply() {
        let b = Budgets::of(Tier::Quality, GpuClass::Discrete);
        let r = ladder(&b)[0];
        let mut lim = Limits::default();
        assert!(frame_fx(VisualStyle::Realistic, &b, &r, &lim, false).realistic);
        lim.realistic = false;
        lim.max_samples = 1;
        lim.max_shadow = 1024;
        lim.ao = false;
        let f = frame_fx(VisualStyle::Realistic, &b, &r, &lim, false);
        assert!(!f.realistic);
        assert_eq!((f.samples, f.shadow_px, f.ao), (1, 1024, AoMode::Off));
    }

    #[test]
    fn textures_follow_level_style_and_limits() {
        let r = |b: &Budgets| ladder(b)[0];
        let lim = Limits::default();
        let speed = Budgets::of(Tier::Speed, GpuClass::Integrated);
        let bal = Budgets::of(Tier::Balanced, GpuClass::Integrated);
        assert!(!frame_fx(VisualStyle::Realistic, &speed, &r(&speed), &lim, false).textures);
        assert!(frame_fx(VisualStyle::Realistic, &bal, &r(&bal), &lim, false).textures);
        assert!(!frame_fx(VisualStyle::Shaded, &bal, &r(&bal), &lim, false).textures);
        let off = Limits {
            realistic: false,
            ..lim
        };
        assert!(!frame_fx(VisualStyle::Realistic, &bal, &r(&bal), &off, false).textures);
        let mut f = frame_fx(VisualStyle::Realistic, &bal, &r(&bal), &lim, false);
        f.realistic = false;
        assert!(!off.apply(f).textures);
    }

    #[test]
    fn out_of_memory_gives_up_in_order_and_ends() {
        let mut l = Limits::default();
        assert!(l.on_out_of_memory(2048));
        assert_eq!(l.max_samples, 1);
        assert!(l.on_out_of_memory(2048));
        assert_eq!(l.max_shadow, 1024);
        assert!(l.on_out_of_memory(1024));
        assert_eq!(l.max_shadow, 0);
        assert!(l.on_out_of_memory(0));
        assert!(!l.ao);
        let mut n = 0;
        while l.on_out_of_memory(0) {
            n += 1;
            assert!(n < 20);
        }
        assert!(l.max_scale > 0.1);
    }

    #[test]
    fn legacy_quality_keeps_its_look_and_scale_rounds() {
        let f = FrameFx::from_quality(Quality::High);
        assert_eq!((f.scale, f.samples, f.shadow_px), (1.0, 4, 2048));
        assert_eq!(f.ao, AoMode::High);
        assert_eq!(FrameFx::from_quality(Quality::Medium).ao, AoMode::Off);
        assert_eq!(FrameFx::from_quality(Quality::Off).shadow_px, 0);
        assert_eq!(f.scaled((1920, 1080)), (1920, 1080));
        let half = FrameFx { scale: 0.5, ..f };
        assert_eq!(half.scaled((1, 3)), (1, 2));
    }

    #[test]
    fn bench_budget_check() {
        let b = Budgets::of(Tier::Balanced, GpuClass::Discrete);
        assert!(bench_pass(&b, 20.0, 100.0));
        assert!(!bench_pass(&b, 20.1, 100.0));
        assert!(!bench_pass(&b, 10.0, 251.0));
    }
}
