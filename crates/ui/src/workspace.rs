// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Switchable workspaces (docs/design/LAYOUTS.md): layouts are data, `arrange()` is a pure
//! placement function, persistence is `layout=` in prefs.txt plus `workspaces.json`.
use crate::theme::{Breakpoints, Mode};
use egui::{pos2, vec2, Rect};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const WORKSPACES_FILE: &str = "workspaces.json";
pub const WORKSPACES_VERSION: u32 = 1;
pub const PRESET_IDS: [&str; 5] = ["classic", "context", "split", "focus", "data"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelId {
    TitleBar,
    CommandSurface,
    ContextBar,
    Browser,
    Inspector,
    ViewHost,
    /// Second view of `SplitPlan3d` (3D).
    ViewHost2,
    DataTable,
    StatusBar,
    Radial,
}
impl PanelId {
    /// Stable egui id root: `ddc.panel.<id>`.
    pub fn egui_id(self) -> String {
        let s = serde_json::to_string(&self).unwrap_or_default();
        format!("ddc.panel.{}", s.trim_matches('"'))
    }
    /// Minimum size in points when expanded.
    pub fn min_size(self) -> egui::Vec2 {
        match self {
            Self::Browser => vec2(200.0, 160.0),
            Self::Inspector => vec2(240.0, 160.0),
            Self::ViewHost | Self::ViewHost2 => vec2(240.0, 160.0),
            Self::DataTable => vec2(240.0, 96.0),
            Self::CommandSurface => vec2(40.0, 40.0),
            Self::TitleBar | Self::StatusBar => vec2(200.0, 20.0),
            Self::ContextBar | Self::Radial => vec2(40.0, 32.0),
        }
    }
    pub fn is_view(self) -> bool {
        matches!(self, Self::ViewHost | Self::ViewHost2 | Self::DataTable)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    Ribbon,
    ContextBar,
    Radial,
    PaletteOnly,
    Toolbar,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
    /// Overlay window over the view: [x, y, w, h] in points.
    Float([f32; 4]),
    Hidden,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dock {
    pub panel: PanelId,
    pub side: Side,
    pub size: f32,
    pub order: u8,
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ViewArrangement {
    Single,
    SplitPlan3d {
        ratio: f32,
        sync: bool,
    },
    /// `ratio` = table share of the view area.
    ModelTable {
        ratio: f32,
        axis: Axis,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutDensity {
    Compact,
    Regular,
    Comfortable,
}
impl LayoutDensity {
    pub fn row(self) -> f32 {
        match self {
            Self::Compact => 24.0,
            Self::Regular => 28.0,
            Self::Comfortable => 32.0,
        }
    }
    pub fn pad_factor(self) -> f32 {
        match self {
            Self::Compact => 0.85,
            Self::Regular => 1.0,
            Self::Comfortable => 1.15,
        }
    }
}

/// What a layout may change about tokens. There is deliberately no accent field.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TokenOverrides {
    pub theme: Option<ThemeMode>,
    pub density: Option<LayoutDensity>,
    pub radius_scale: f32,
}
impl Default for TokenOverrides {
    fn default() -> Self {
        Self {
            theme: None,
            density: None,
            radius_scale: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub base: Option<String>,
    pub surface: Surface,
    pub docks: Vec<Dock>,
    pub views: ViewArrangement,
    pub status: bool,
    #[serde(default)]
    pub tokens: TokenOverrides,
    #[serde(default)]
    pub ribbon_order: Vec<String>,
    #[serde(default)]
    pub hidden_groups: Vec<String>,
}

fn dock(panel: PanelId, side: Side, size: f32, order: u8) -> Dock {
    Dock {
        panel,
        side,
        size,
        order,
        collapsed: false,
    }
}

/// The five approved layouts, in hotkey order 1..5.
pub fn presets() -> Vec<Workspace> {
    use PanelId::*;
    let ws = |id: &str, name: &str, surface, docks, views, status, tokens| Workspace {
        id: id.into(),
        name: name.into(),
        base: None,
        surface,
        docks,
        views,
        status,
        tokens,
        ribbon_order: Vec::new(),
        hidden_groups: Vec::new(),
    };
    let side_panels = || {
        vec![
            dock(CommandSurface, Side::Top, 92.0, 0),
            dock(Browser, Side::Left, 260.0, 1),
            dock(Inspector, Side::Right, 320.0, 2),
        ]
    };
    let mut context = side_panels();
    context[0].side = Side::Hidden;
    let toolbar = vec![
        dock(CommandSurface, Side::Left, 48.0, 0),
        dock(Browser, Side::Left, 240.0, 1),
        dock(Inspector, Side::Right, 300.0, 2),
    ];
    let data = vec![
        dock(CommandSurface, Side::Top, 64.0, 0),
        dock(Browser, Side::Left, 240.0, 1),
        dock(Inspector, Side::Right, 300.0, 2),
    ];
    let compact = TokenOverrides {
        density: Some(LayoutDensity::Compact),
        ..Default::default()
    };
    vec![
        ws(
            "classic",
            "Classic ribbon",
            Surface::Ribbon,
            side_panels(),
            ViewArrangement::Single,
            true,
            TokenOverrides::default(),
        ),
        ws(
            "context",
            "Contextual",
            Surface::ContextBar,
            context,
            ViewArrangement::Single,
            true,
            TokenOverrides::default(),
        ),
        ws(
            "split",
            "Plan + 3D",
            Surface::Toolbar,
            toolbar,
            ViewArrangement::SplitPlan3d {
                ratio: 0.5,
                sync: true,
            },
            true,
            TokenOverrides {
                theme: Some(ThemeMode::Dark),
                density: Some(LayoutDensity::Compact),
                radius_scale: 1.0,
            },
        ),
        ws(
            "focus",
            "Focus",
            Surface::Radial,
            vec![
                dock(Browser, Side::Hidden, 260.0, 1),
                dock(Inspector, Side::Hidden, 320.0, 2),
            ],
            ViewArrangement::Single,
            false,
            TokenOverrides::default(),
        ),
        ws(
            "data",
            "Model + table",
            Surface::Ribbon,
            data,
            ViewArrangement::ModelTable {
                ratio: 0.4,
                axis: Axis::Vertical,
            },
            true,
            compact,
        ),
    ]
}

pub fn preset(id: &str) -> Option<Workspace> {
    presets().into_iter().find(|w| w.id == id)
}

/// `--layout N|id` / `layout=` value → workspace key (`classic`.. or `custom:<name>`).
pub fn parse_layout_value(v: &str) -> Option<String> {
    let v = v.trim();
    if let Ok(n) = v.parse::<usize>() {
        return PRESET_IDS.get(n.checked_sub(1)?).map(|s| s.to_string());
    }
    if PRESET_IDS.contains(&v) {
        return Some(v.to_string());
    }
    v.strip_prefix("custom:")
        .filter(|n| !n.trim().is_empty())
        .map(|_| v.to_string())
}

/// Finds `--layout N|id` in argv; never written to prefs.
pub fn layout_from_args<S: AsRef<str>>(args: &[S]) -> Option<String> {
    let i = args.iter().position(|a| a.as_ref() == "--layout")?;
    parse_layout_value(args.get(i + 1)?.as_ref())
}

// ---------- placement ----------

const TITLE_H: f32 = 32.0;
const STATUS_H: f32 = 24.0;
const RAIL_W: f32 = 40.0;
const RIBBON_COMPACT_H: f32 = 64.0;
const NARROW_INSPECTOR: f32 = 280.0;
const NARROW_TABLE: f32 = 0.35;
const NARROW_FROM: f32 = 1600.0;
const CONTEXT_BAR: egui::Vec2 = egui::Vec2::new(360.0, 40.0);

#[derive(Debug, Clone, Default)]
pub struct Placement {
    /// Non-overlay panels: never intersect, all inside the screen.
    pub rects: Vec<(PanelId, Rect)>,
    /// Overlays over the view (Float docks, ContextBar, Radial).
    pub overlays: Vec<(PanelId, Rect)>,
    /// Panels shown as a collapsed icon rail.
    pub collapsed: Vec<PanelId>,
    /// V3 at ≤ 1366: plan|3D tabs instead of a split.
    pub view_tabs: bool,
    pub ribbon_compact: bool,
}
impl Placement {
    pub fn get(&self, p: PanelId) -> Option<Rect> {
        self.rects
            .iter()
            .chain(&self.overlays)
            .find(|(id, _)| *id == p)
            .map(|(_, r)| *r)
    }
}

/// Pure layout: screen in points (already divided by UI scale) → panel rectangles.
/// Degradation (LAYOUTS §5): <1600 narrow inspector/table, <compact_below browser rail,
/// <compact_below split → tabs, <icons_below icon-only ribbon.
pub fn arrange(ws: &Workspace, screen: Rect, bp: &Breakpoints) -> Placement {
    let w = screen.width();
    let mut out = Placement {
        ribbon_compact: w < bp.icons_below,
        ..Default::default()
    };
    let mut rest = screen;
    let take = |side: Side, size: f32, rest: &mut Rect| -> Rect {
        let r = *rest;
        match side {
            Side::Top => {
                let s = size.min(r.height());
                rest.min.y += s;
                Rect::from_min_size(r.min, vec2(r.width(), s))
            }
            Side::Bottom => {
                let s = size.min(r.height());
                rest.max.y -= s;
                Rect::from_min_max(pos2(r.min.x, r.max.y - s), r.max)
            }
            Side::Left => {
                let s = size.min(r.width());
                rest.min.x += s;
                Rect::from_min_size(r.min, vec2(s, r.height()))
            }
            _ => {
                let s = size.min(r.width());
                rest.max.x -= s;
                Rect::from_min_max(pos2(r.max.x - s, r.min.y), r.max)
            }
        }
    };
    out.rects
        .push((PanelId::TitleBar, take(Side::Top, TITLE_H, &mut rest)));
    if ws.status {
        out.rects
            .push((PanelId::StatusBar, take(Side::Bottom, STATUS_H, &mut rest)));
    }
    let view_min = PanelId::ViewHost.min_size();
    let mut docks: Vec<&Dock> = ws.docks.iter().collect();
    docks.sort_by_key(|d| d.order);
    let mut floats = Vec::new();
    for d in docks {
        if d.panel == PanelId::CommandSurface
            && matches!(
                ws.surface,
                Surface::ContextBar | Surface::Radial | Surface::PaletteOnly
            )
        {
            continue;
        }
        match d.side {
            Side::Hidden => continue,
            Side::Float(f) => {
                floats.push((d.panel, f));
                continue;
            }
            _ => {}
        }
        let horizontal = matches!(d.side, Side::Left | Side::Right);
        let mut size = d.size;
        let mut rail = d.collapsed;
        if d.panel == PanelId::Inspector && w < NARROW_FROM {
            size = size.min(NARROW_INSPECTOR);
        }
        if d.panel == PanelId::Browser && w < bp.compact_below {
            rail = true;
        }
        if d.panel == PanelId::CommandSurface && ws.surface == Surface::Ribbon && out.ribbon_compact
        {
            size = size.min(RIBBON_COMPACT_H);
        }
        let min = d.panel.min_size();
        let (avail, keep, pmin) = if horizontal {
            (rest.width(), view_min.x, min.x)
        } else {
            (rest.height(), view_min.y, min.y)
        };
        if rail {
            size = RAIL_W;
        } else {
            size = size.max(pmin);
            if avail - size < keep {
                size = RAIL_W; // no room: fold rather than overlap or starve the view
                rail = true;
            }
        }
        if avail - size < keep {
            continue;
        }
        if rail {
            out.collapsed.push(d.panel);
        }
        let r = take(d.side, size, &mut rest);
        out.rects.push((d.panel, r));
    }
    match ws.views {
        ViewArrangement::Single => out.rects.push((PanelId::ViewHost, rest)),
        ViewArrangement::SplitPlan3d { ratio, .. } => {
            if w < bp.compact_below {
                out.view_tabs = true;
                out.rects.push((PanelId::ViewHost, rest));
            } else {
                let x = rest.min.x + rest.width() * ratio.clamp(0.2, 0.8);
                out.rects.push((
                    PanelId::ViewHost,
                    Rect::from_min_max(rest.min, pos2(x, rest.max.y)),
                ));
                out.rects.push((
                    PanelId::ViewHost2,
                    Rect::from_min_max(pos2(x, rest.min.y), rest.max),
                ));
            }
        }
        ViewArrangement::ModelTable { ratio, axis } => {
            let mut ratio = ratio.clamp(0.15, 0.6);
            if w < NARROW_FROM {
                ratio = ratio.min(NARROW_TABLE);
            }
            let (model, table) = match axis {
                Axis::Vertical => {
                    let y = rest.max.y - rest.height() * ratio;
                    (
                        Rect::from_min_max(rest.min, pos2(rest.max.x, y)),
                        Rect::from_min_max(pos2(rest.min.x, y), rest.max),
                    )
                }
                Axis::Horizontal => {
                    let x = rest.max.x - rest.width() * ratio;
                    (
                        Rect::from_min_max(rest.min, pos2(x, rest.max.y)),
                        Rect::from_min_max(pos2(x, rest.min.y), rest.max),
                    )
                }
            };
            out.rects.push((PanelId::ViewHost, model));
            out.rects.push((PanelId::DataTable, table));
        }
    }
    let view = rest;
    let fit = |r: Rect| {
        let s = r.size().min(view.size());
        let min = r.min.clamp(view.min, view.max - s);
        Rect::from_min_size(min, s)
    };
    for (p, [x, y, fw, fh]) in floats {
        out.overlays
            .push((p, fit(Rect::from_min_size(pos2(x, y), vec2(fw, fh)))));
    }
    match ws.surface {
        Surface::ContextBar => {
            let r = Rect::from_center_size(
                pos2(view.center().x, view.max.y - CONTEXT_BAR.y),
                CONTEXT_BAR,
            );
            out.overlays.push((PanelId::ContextBar, fit(r)));
        }
        Surface::Radial => {
            let r = Rect::from_center_size(view.center(), vec2(280.0, 280.0));
            out.overlays.push((PanelId::Radial, fit(r)));
        }
        _ => {}
    }
    out
}

// ---------- tokens ----------

/// Theme resolution (LAYOUTS §2.4): explicit user choice > layout default > system.
pub fn resolve_mode(ws: &Workspace, user: ThemeMode, system: Mode) -> Mode {
    match (user, ws.tokens.theme) {
        (ThemeMode::Light, _) => Mode::Light,
        (ThemeMode::Dark, _) => Mode::Dark,
        (ThemeMode::System, Some(ThemeMode::Light)) => Mode::Light,
        (ThemeMode::System, Some(ThemeMode::Dark)) => Mode::Dark,
        _ => system,
    }
}
pub fn resolve_density(ws: &Workspace, user: Option<LayoutDensity>) -> LayoutDensity {
    user.or(ws.tokens.density).unwrap_or(LayoutDensity::Regular)
}

// ---------- persistence ----------

#[derive(Debug, Serialize, Deserialize)]
struct WorkspacesFile {
    version: u32,
    workspaces: Vec<Workspace>,
}

#[derive(Debug, Default)]
pub struct Loaded {
    /// Presets followed by valid custom workspaces.
    pub all: Vec<Workspace>,
    /// Set when a broken file was moved aside; the shell shows a toast.
    pub moved_bad: Option<PathBuf>,
}

/// `layout=` from prefs.txt text (bad lines skipped); unknown values → None.
pub fn layout_from_prefs(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| k.trim() == "layout")
        .filter_map(|(_, v)| parse_layout_value(v))
        .next()
}
pub fn first_run_done(text: &str) -> bool {
    text.lines().any(|l| {
        l.split_once('=')
            .is_some_and(|(k, v)| k.trim() == "layout.first_run_done" && v.trim() == "1")
    })
}

/// Writes UTF-8 (no BOM) atomically: tmp + rename.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, path)
}

pub fn save_custom(dir: &Path, custom: &[Workspace]) -> std::io::Result<()> {
    let f = WorkspacesFile {
        version: WORKSPACES_VERSION,
        workspaces: custom.to_vec(),
    };
    let text = serde_json::to_string_pretty(&f).map_err(std::io::Error::other)?;
    write_atomic(&dir.join(WORKSPACES_FILE), &text)
}

/// Loads presets + custom layouts; a corrupt file is renamed to `.bad`.
pub fn load(dir: &Path) -> Loaded {
    let mut out = Loaded {
        all: presets(),
        moved_bad: None,
    };
    let path = dir.join(WORKSPACES_FILE);
    let Ok(bytes) = std::fs::read(&path) else {
        return out;
    };
    let parsed = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|s| serde_json::from_str::<WorkspacesFile>(s.trim_start_matches('\u{feff}')).ok())
        .filter(|f| f.version == WORKSPACES_VERSION);
    match parsed {
        Some(f) => {
            for w in f.workspaces {
                if !out.all.iter().any(|p| p.id == w.id) {
                    out.all.push(w);
                }
            }
        }
        None => {
            let bad = path.with_extension("json.bad");
            if std::fs::rename(&path, &bad).is_ok() {
                out.moved_bad = Some(bad);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Density, Theme, Tokens};

    fn bp() -> Breakpoints {
        Tokens::embedded().unwrap().breakpoint
    }
    fn customs() -> Vec<Workspace> {
        let mut a = preset("classic").unwrap();
        a.id = "custom:mirror".into();
        a.base = Some("classic".into());
        for d in &mut a.docks {
            d.side = match d.side {
                Side::Left => Side::Right,
                Side::Right => Side::Left,
                s => s,
            };
        }
        a.docks.push(dock(PanelId::DataTable, Side::Hidden, 0.0, 9));
        let mut b = preset("data").unwrap();
        b.id = "custom:float".into();
        b.base = Some("data".into());
        b.docks[2].side = Side::Float([5000.0, -50.0, 320.0, 480.0]);
        b.docks[1].collapsed = true;
        b.views = ViewArrangement::ModelTable {
            ratio: 0.5,
            axis: Axis::Horizontal,
        };
        vec![a, b]
    }

    #[test]
    fn t2_no_overlap_matrix() {
        let sizes = [
            (1366.0, 768.0),
            (1440.0, 900.0),
            (1600.0, 900.0),
            (1920.0, 1080.0),
            (2560.0, 1440.0),
            (3840.0, 2160.0),
        ];
        let mut all = presets();
        all.extend(customs());
        for ws in &all {
            for (pw, ph) in sizes {
                for scale in [1.0f32, 1.25, 1.5] {
                    let screen =
                        Rect::from_min_size(egui::Pos2::ZERO, vec2(pw / scale, ph / scale));
                    let p = arrange(ws, screen, &bp());
                    let tag = format!("{} {pw}x{ph}@{scale}", ws.id);
                    for (i, (a, ra)) in p.rects.iter().enumerate() {
                        assert!(
                            screen.expand(0.01).contains_rect(*ra),
                            "{tag}: {a:?} off screen"
                        );
                        let min = a.min_size();
                        if !p.collapsed.contains(a) && !matches!(a, PanelId::CommandSurface) {
                            assert!(
                                ra.width() >= min.x - 0.01 && ra.height() >= min.y - 0.01,
                                "{tag}: {a:?} {ra:?} < min"
                            );
                        }
                        for (b, rb) in &p.rects[i + 1..] {
                            let x = ra.intersect(*rb);
                            assert!(
                                x.width() <= 0.01 || x.height() <= 0.01,
                                "{tag}: {a:?}∩{b:?}"
                            );
                        }
                    }
                    for (o, r) in &p.overlays {
                        assert!(
                            screen.expand(0.01).contains_rect(*r),
                            "{tag}: overlay {o:?}"
                        );
                    }
                    let view: f32 = p
                        .rects
                        .iter()
                        .filter(|(id, _)| id.is_view())
                        .map(|(_, r)| r.area())
                        .sum();
                    assert!(
                        view >= 0.4 * screen.area(),
                        "{tag}: view {:.0}%",
                        100.0 * view / screen.area()
                    );
                    if (pw, scale) == (1920.0, 1.0) && ph == 1080.0 {
                        println!("{tag}: {:?}", p.rects);
                    }
                }
            }
        }
    }

    #[test]
    fn degradation_rules() {
        let b = bp();
        let at = |id: &str, w: f32| {
            arrange(
                &preset(id).unwrap(),
                Rect::from_min_size(egui::Pos2::ZERO, vec2(w, 900.0)),
                &b,
            )
        };
        assert_eq!(
            at("classic", 1500.0)
                .get(PanelId::Inspector)
                .unwrap()
                .width(),
            280.0
        );
        assert_eq!(
            at("classic", 1920.0)
                .get(PanelId::Inspector)
                .unwrap()
                .width(),
            320.0
        );
        assert!(at("classic", 1400.0).collapsed.contains(&PanelId::Browser));
        assert!(!at("classic", 1500.0).collapsed.contains(&PanelId::Browser));
        assert!(
            at("split", 1366.0).view_tabs && at("split", 1366.0).get(PanelId::ViewHost2).is_none()
        );
        assert!(at("split", 1920.0).get(PanelId::ViewHost2).is_some());
        let focus = at("focus", 1920.0);
        assert!(focus.get(PanelId::Inspector).is_none() && focus.get(PanelId::Radial).is_some());
        assert!(at("context", 1920.0).get(PanelId::CommandSurface).is_none());
    }

    #[test]
    fn layout_args() {
        assert_eq!(
            layout_from_args(&["x", "--layout", "3"]).as_deref(),
            Some("split")
        );
        assert_eq!(
            layout_from_args(&["--layout", "focus"]).as_deref(),
            Some("focus")
        );
        let custom = format!("custom:{}", crate::test_fx::fx("layout_name"));
        assert_eq!(
            layout_from_args(&["--layout", custom.as_str()]).as_deref(),
            Some(custom.as_str())
        );
        assert_eq!(layout_from_args(&["--layout", "0"]), None);
        assert_eq!(layout_from_args(&["--layout", "6"]), None);
        assert_eq!(layout_from_args(&["--layout"]), None);
        assert_eq!(PanelId::Inspector.egui_id(), "ddc.panel.inspector");
    }

    #[test]
    fn t4_persistence() {
        let dir = std::env::temp_dir().join(format!("ddc_ws_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prefs = "props_width=320\nbroken line\nlayout=data\nlayout.first_run_done=1\n";
        assert_eq!(layout_from_prefs(prefs).as_deref(), Some("data"));
        assert!(first_run_done(prefs) && !first_run_done("layout=classic"));
        assert_eq!(layout_from_prefs("layout=bogus"), None);

        let c = customs();
        save_custom(&dir, &c).unwrap();
        let raw = std::fs::read(dir.join(WORKSPACES_FILE)).unwrap();
        assert!(!raw.starts_with(&[0xEF, 0xBB, 0xBF]));
        let l = load(&dir);
        assert!(l.moved_bad.is_none());
        assert_eq!(l.all.len(), 7);
        assert_eq!(&l.all[5..], &c[..]);

        std::fs::write(dir.join(WORKSPACES_FILE), b"{ not json").unwrap();
        let l = load(&dir);
        assert_eq!(l.all, presets());
        assert!(l.moved_bad.as_ref().unwrap().exists());
        assert!(!dir.join(WORKSPACES_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn t5_tokens() {
        let all = presets();
        for mode in [Mode::Light, Mode::Dark] {
            let accent = Theme::new(mode, Density::Comfortable).c("accent");
            for ws in &all {
                let m = resolve_mode(
                    ws,
                    if mode == Mode::Light {
                        ThemeMode::Light
                    } else {
                        ThemeMode::Dark
                    },
                    Mode::Light,
                );
                assert_eq!(
                    Theme::new(m, Density::Compact).c("accent"),
                    accent,
                    "{}",
                    ws.id
                );
            }
        }
        let split = preset("split").unwrap();
        assert_eq!(
            resolve_mode(&split, ThemeMode::System, Mode::Light),
            Mode::Dark
        );
        assert_eq!(
            resolve_mode(&split, ThemeMode::Light, Mode::Dark),
            Mode::Light
        );
        assert_eq!(
            resolve_mode(&preset("classic").unwrap(), ThemeMode::System, Mode::Dark),
            Mode::Dark
        );
        assert_eq!(resolve_density(&split, None), LayoutDensity::Compact);
        assert_eq!(
            resolve_density(&split, Some(LayoutDensity::Comfortable)),
            LayoutDensity::Comfortable
        );
        assert_eq!(LayoutDensity::Regular.row(), 28.0);
    }
}
