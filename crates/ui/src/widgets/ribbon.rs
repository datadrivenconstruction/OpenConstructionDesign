// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Ribbon: tab strip (always visible) and a collapsible band of command buttons in titled groups
//! (surface R). Button size, collapse priority, split variants and group launchers come from the
//! commands json; geometry is the pure `ribbon_layout`. Commands the host has not implemented stay
//! in place, greyed, with a "coming soon" tooltip; a selection adds the accent-tinted contextual
//! tab "Modify | <category>" (TOOLS_UI section 6). Alt alone shows key tips.
use super::ribbon_layout::{self as rl, GroupIn, ItemIn, KeyTips, Size, SplitState, Stage};
use super::ribbon_layout::{TipHit, TipLevel};
use crate::px;
use crate::theme::{ribbon as k, stroke, Role, Width};
use crate::icons::{tint, IconCache, Tint};
use crate::{Catalog, Command, Lang, Registry, Theme};
use egui::{pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};
use std::collections::{HashMap, HashSet};

pub struct Ribbon {
    pub tab: String,
    pub collapsed: bool,
    /// Tabs only and a tab was clicked: the band shows over the canvas until a command runs,
    /// the pointer presses elsewhere or Esc; the docked height stays the tab strip's.
    pub peek: bool,
    /// Key of the host's fold command, shown in the collapse caret's tooltip.
    pub toggle_keys: String,
    /// Small-buttons mode: no large buttons and no group titles.
    pub small: bool,
    /// Commands the host implements; others are drawn disabled. `None` — all enabled.
    pub available: Option<HashSet<String>>,
    /// Commands the current selection cannot take: id to the reason shown as the tooltip.
    pub blocked: HashMap<String, String>,
    /// Selection category for the contextual tab (`wall`, `duct`, …) and its caption.
    pub context: Option<(String, String)>,
    /// Tab to return to when the contextual tab goes away.
    prev_tab: Option<String>,
    /// Variant last chosen per split button.
    pub split: SplitState,
    /// Tabs put first on the strip, in this order (the user's focus); the rest keep theirs.
    pub tab_order: Vec<String>,
    tips: KeyTips,
}

/// `tabs` with the known ones of `first` moved to the front in that order.
pub fn ordered_tabs(mut tabs: Vec<String>, first: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(tabs.len());
    for f in first {
        if let Some(i) = tabs.iter().position(|t| t == f) {
            out.push(tabs.remove(i));
        }
    }
    out.extend(tabs);
    out
}

/// Rectangle of a command's button drawn in this or the previous pass (tutorial highlight, F1
/// over a button); a split variant reports its parent's button. Hidden tabs report nothing.
pub fn button_rect(ctx: &egui::Context, id: &str) -> Option<Rect> {
    let (r, pass) = ctx.data(|d| d.get_temp::<(Rect, u64)>(egui::Id::new(("ribbon.rect", id))))?;
    (pass + 1 >= ctx.cumulative_pass_nr()).then_some(r)
}

fn note_rect(ctx: &egui::Context, id: &str, r: Rect) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(("ribbon.rect", id)), (r, pass)));
}

/// Contextual tab id.
pub const CTX_TAB: &str = "ctx";
/// Tab shown when a requested id is unknown.
pub const DEFAULT_TAB: &str = "arch";

fn tab_is_valid(id: &str, known: &[String], ctx_open: bool) -> bool {
    (id == CTX_TAB && ctx_open) || known.iter().any(|t| t == id)
}

/// Maps a requested tab id to one the ribbon can show: known ids stay, aliases are accepted,
/// anything else becomes `arch` (or the first known tab when `arch` is missing).
pub fn resolve_tab(id: &str, known: &[String], ctx_open: bool) -> String {
    let id = id.trim();
    if tab_is_valid(id, known, ctx_open) {
        return id.to_string();
    }
    let alias = match id.to_ascii_lowercase().as_str() {
        "structure" | "struct" => "struct",
        "architecture" | "arch" => "arch",
        _ => "",
    };
    if known.iter().any(|t| t == alias) {
        return alias.to_string();
    }
    if known.iter().any(|t| t == DEFAULT_TAB) {
        return DEFAULT_TAB.to_string();
    }
    known.first().cloned().unwrap_or_else(|| DEFAULT_TAB.to_string())
}

/// Fill of the contextual tab body: the normal ribbon surface, never a tinted grey band that
/// reads as disabled; the context shows in the tab chip and the accent top rule.
pub fn ctx_body_fill(th: &Theme) -> Color32 {
    th.c("bg.panel")
}
pub const TAB_H: f32 = k::TAB_H;

/// The one fill behind a part of a button; fills are translucent, so a part never gets two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PartFill {
    None,
    Hover,
    Selected,
    Pressed,
}

impl PartFill {
    fn color(self, th: &Theme) -> Option<Color32> {
        match self {
            PartFill::None => None,
            PartFill::Hover => Some(th.hover_fill()),
            PartFill::Selected => Some(th.c("accent.subtle")),
            PartFill::Pressed => Some(th.pressed_fill()),
        }
    }
}

/// Caption ink that reads on `fill`: the accent only on its own subtle fill.
fn fill_ink(th: &Theme, fill: PartFill) -> Color32 {
    if fill == PartFill::Selected {
        th.c("accent")
    } else {
        th.c("text.primary")
    }
}

/// Fill of one part, given `(hot, down)`: pressed beats selected beats hover.
fn part_fill(on: bool, (hot, down): (bool, bool)) -> PartFill {
    if down {
        PartFill::Pressed
    } else if on {
        PartFill::Selected
    } else if hot {
        PartFill::Hover
    } else {
        PartFill::None
    }
}

/// Fills of the main part and the caret (`None` caret: a plain button, second fill is none).
fn button_fills(on: bool, main: (bool, bool), caret: Option<(bool, bool)>) -> [PartFill; 2] {
    [part_fill(on, main), caret.map_or(PartFill::None, |c| part_fill(on, c))]
}

impl Default for Ribbon {
    fn default() -> Self {
        Self {
            tab: "arch".into(),
            collapsed: false,
            peek: false,
            toggle_keys: String::new(),
            small: false,
            available: None,
            blocked: Default::default(),
            context: None,
            prev_tab: None,
            split: SplitState::default(),
            tab_order: Vec::new(),
            tips: KeyTips::default(),
        }
    }
}

/// Commands whose icon everyone reads without a caption: the only ones sized `small` (icon
/// only when the band narrows). Every other ribbon command keeps its label.
pub const ICON_ONLY: [&str; 2] = ["mod.cut", "mod.copy_clip"];

/// Ribbon panel of a command: explicit `group` or the id-derived default.
pub fn group_name(c: &Command) -> &str {
    c.group.as_deref().unwrap_or_else(|| group_of(&c.id))
}

/// Ribbon group of a command (caption key `rgroup.<g>`); groups keep registry order.
pub fn group_of(id: &str) -> &'static str {
    match id {
        "arch.wall" | "arch.wall_room" | "arch.curtain_wall" | "str.wall" => "walls",
        "arch.door" | "arch.window" | "arch.opening" => "openings",
        "arch.column" | "arch.floor" | "arch.ceiling" | "arch.roof" => "build",
        "arch.stair" | "arch.ramp" | "arch.railing" => "circ",
        "arch.room" | "arch.room_sep" | "arch.zone" => "rooms",
        "arch.component" | "arch.shaft" => "model",
        "mod.move" | "mod.copy" | "mod.rotate" | "mod.mirror" | "mod.array" => "transform",
        "mod.align" | "mod.offset" | "mod.trim" | "mod.split" | "mod.join" => "geometry",
        "view.plan" | "view.section" | "view.elevation" | "view.3d" | "arch.section" => "views",
        "view.zoom_fit" => "nav",
        "view.style" | "view.filters" | "view.template" | "view.thin_lines" => "graphics",
        "view.tile" | "view.sheet" | "view.place" | "view.schedule" | "view.diff" => "sheets",
        id if id.starts_with("mod.") => "edit",
        _ => "main",
    }
}

/// Body height under the tab strip: button band and group titles.
pub fn body_h(_wc: Width) -> f32 {
    k::PAD_Y + k::BAND_H + k::TITLE_H
}

/// One button: the command it stands for and, for a split button, the variant it runs now.
struct Entry<'a> {
    cmd: &'a Command,
    show: &'a Command,
    label: String,
    words: Vec<String>,
    wrap: rl::Wrap,
    size: Size,
}

struct GroupDraw<'a> {
    title: String,
    /// The title repeats the only command's name: the caption row stays empty.
    title_hidden: bool,
    title_wrap: rl::Wrap,
    title_words: Vec<String>,
    launcher: Option<&'a Command>,
    entries: Vec<Entry<'a>>,
}

/// Commands of a tab as ribbon groups (variants and launchers folded in), large ones first in
/// each group, with the measured inputs of the pure layout. `tw(text, title)` measures one line.
fn build_groups<'a>(
    cmds: &[&'a Command],
    reg: &'a Registry,
    cat: &Catalog,
    split: &SplitState,
    tw: &dyn Fn(&str, bool) -> f32,
    m: &rl::Metrics,
) -> (Vec<GroupDraw<'a>>, Vec<GroupIn>) {
    build_groups_in(cmds, reg, cat, cat.lang, split, tw, m)
}

/// Layout inputs measured in every shipped language at once ([`rl::widest`]): the stage of a
/// group depends on the window width and the command priorities, never on the language.
fn basis_groups(
    cmds: &[&Command],
    reg: &Registry,
    cat: &Catalog,
    split: &SplitState,
    tw: &dyn Fn(&str, bool) -> f32,
    m: &rl::Metrics,
) -> Vec<GroupIn> {
    let sets: Vec<Vec<GroupIn>> = [Lang::En, Lang::Ru]
        .into_iter()
        .map(|l| build_groups_in(cmds, reg, cat, l, split, tw, m).1)
        .collect();
    rl::widest(&sets)
}

/// As [`build_groups`], with the captions of `lang`.
fn build_groups_in<'a>(
    cmds: &[&'a Command],
    reg: &'a Registry,
    cat: &Catalog,
    lang: Lang,
    split: &SplitState,
    tw: &dyn Fn(&str, bool) -> f32,
    m: &rl::Metrics,
) -> (Vec<GroupDraw<'a>>, Vec<GroupIn>) {
    let folded: HashSet<&str> = cmds
        .iter()
        .flat_map(|c| c.variants.iter().map(String::as_str))
        .collect();
    let space = |title: bool| tw("x x", title) - 2.0 * tw("x", title);
    let mut order: Vec<&str> = Vec::new();
    let mut by_group: HashMap<&str, (Vec<&'a Command>, Option<&'a Command>)> = HashMap::new();
    for c in cmds.iter().copied().filter(|c| !folded.contains(c.id.as_str())) {
        let g = group_name(c);
        if !order.contains(&g) {
            order.push(g);
        }
        let slot = by_group.entry(g).or_default();
        if c.launcher && slot.1.is_none() {
            slot.1 = Some(c);
        } else {
            slot.0.push(c);
        }
    }
    let mut draws = Vec::new();
    let mut ins = Vec::new();
    for g in order {
        let (mut list, mut launcher) = by_group.remove(g).unwrap_or_default();
        if list.is_empty() {
            // a group of only a launcher shows it as a button
            list.extend(launcher.take());
        }
        let mut entries: Vec<Entry> = list
            .into_iter()
            .map(|c| {
                let show = reg
                    .get(split.current(&c.id, &c.variants))
                    .unwrap_or(c);
                let label = short_label_in(cat, lang, show);
                let words: Vec<String> = label.split_whitespace().map(str::to_string).collect();
                let ww: Vec<f32> = words.iter().map(|w| tw(w, false)).collect();
                let mut wrap = rl::wrap2(&ww, space(false), !c.variants.is_empty(), m);
                // a label past the cap is elided when drawn; shipped locales stay under it
                wrap.w = wrap.w.min(k::LARGE_LABEL_MAX);
                Entry {
                    cmd: c,
                    show,
                    label,
                    words,
                    wrap,
                    size: c.size.unwrap_or_default(),
                }
            })
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.size));
        let title = cat.t_in(lang, &format!("rgroup.{g}"));
        let title_words: Vec<String> = title.split_whitespace().map(str::to_string).collect();
        let tws: Vec<f32> = title_words.iter().map(|w| tw(w, false)).collect();
        let title_wrap = rl::wrap2(&tws, space(false), true, m);
        // a lone command that carries the group's name needs no second caption
        let title_hidden = launcher.is_none()
            && entries.len() == 1
            && [&entries[0].label, &cat.t_in(lang, &entries[0].show.label_key())]
                .iter()
                .any(|l| l.trim().to_lowercase() == title.trim().to_lowercase());
        let items: Vec<ItemIn> = entries
            .iter()
            .map(|e| ItemIn {
                size: e.size,
                priority: e
                    .cmd
                    .priority
                    .unwrap_or_else(|| e.size.default_priority())
                    .clamp(1, 9),
                split: !e.cmd.variants.is_empty(),
                label_w: tw(&e.label, false).min(k::MEDIUM_LABEL_MAX),
                large_w: e.wrap.w,
            })
            .collect();
        ins.push(GroupIn {
            title_w: if title_hidden { 0.0 } else { tw(&title, true) },
            launcher: launcher.is_some(),
            collapsed_w: title_wrap.w,
            low_use: GroupIn::is_low_use(&items),
            items,
        });
        draws.push(GroupDraw {
            title_hidden,
            title,
            title_wrap,
            title_words,
            launcher,
            entries,
        });
    }
    (draws, ins)
}

/// Key tips of the commands of one tab, in group order: the mnemonic when it is free.
fn command_tips(groups: &[GroupDraw]) -> Vec<(String, String)> {
    let all: Vec<&Command> = groups
        .iter()
        .flat_map(|g| g.entries.iter().map(|e| e.cmd).chain(g.launcher))
        .collect();
    let items: Vec<(Option<&str>, &str)> = all
        .iter()
        .map(|c| {
            let hint = c.id.split_once('.').map_or(c.id.as_str(), |x| x.1);
            (c.mnemonic.as_deref(), hint)
        })
        .collect();
    let tips = rl::assign_tips(2, &items);
    all.iter().map(|c| c.id.clone()).zip(tips).collect()
}

/// A key typed while key tips are shown.
enum TipKey {
    Esc,
    Char(char),
}

/// A row of a caret, group or overflow menu.
struct Row {
    id: String,
    /// Split button the row belongs to, for remembering the pick.
    parent: Option<(String, Vec<String>)>,
    icon: String,
    label: String,
    keys: String,
    /// Tooltip description (empty when the catalog has none).
    desc: String,
    reason: String,
    live: bool,
    on: bool,
    /// Command definition says a pick runs once (never remembered by the split button).
    once: bool,
}

impl Ribbon {
    /// Command is implemented by the host (aliases follow their target).
    pub fn enabled(&self, reg: &Registry, id: &str) -> bool {
        !self.blocked.contains_key(id)
            && !self.blocked.contains_key(reg.resolve(id))
            && self
                .available
                .as_ref()
                .is_none_or(|a| a.contains(id) || a.contains(reg.resolve(id)))
    }

    /// Why a command is disabled by the selection, when it is.
    pub fn blocked_reason(&self, reg: &Registry, id: &str) -> Option<&str> {
        self.blocked
            .get(id)
            .or_else(|| self.blocked.get(reg.resolve(id)))
            .map(String::as_str)
    }

    /// Sets the selection category; the contextual tab opens on the first selection and
    /// closes (back to the previous tab) when the selection is cleared.
    pub fn set_context(&mut self, ctx: Option<(String, String)>) {
        match (&self.context, &ctx) {
            (None, Some(_)) if self.tab != CTX_TAB => {
                self.prev_tab = Some(std::mem::replace(&mut self.tab, CTX_TAB.into()));
            }
            (Some(_), None) if self.tab == CTX_TAB => {
                self.tab = self.prev_tab.take().unwrap_or_else(|| "arch".into());
            }
            _ => {}
        }
        self.context = ctx;
    }

    /// Opens a tab by id from outside (command line, saved layout, palette, tour). An unknown or
    /// stale id (aliases `structure`, `architecture` accepted) falls back to a valid tab.
    pub fn set_tab(&mut self, id: &str, known: &[String]) {
        let ctx_open = self.context.is_some();
        let tab = resolve_tab(id, known, ctx_open);
        if tab != id.trim() && cfg!(debug_assertions) {
            eprintln!("ribbon: tab id {id:?} is not known, using {tab:?}");
        }
        self.tab = tab;
    }

    /// Makes the active tab one the strip can show, so strip and body always agree.
    fn normalize_tab(&mut self, known: &[String]) {
        let ctx_open = self.context.is_some();
        if tab_is_valid(&self.tab, known, ctx_open) {
            return;
        }
        let stale = std::mem::take(&mut self.tab);
        let back = self.prev_tab.take().unwrap_or_default();
        let pick = if tab_is_valid(&back, known, ctx_open) { back } else { stale };
        self.set_tab(&pick, known);
    }

    /// Key tip level shown now (Alt toggles it).
    pub fn key_tips(&self) -> TipLevel {
        self.tips.level
    }

    /// Commands on the current tab, registry order (contextual tab: its own ranking).
    fn tab_commands<'a>(&self, reg: &'a Registry) -> Vec<&'a Command> {
        if self.tab == CTX_TAB {
            let k = self.context.as_ref().map_or("*", |c| c.0.as_str());
            reg.contextual(k)
        } else {
            let mut v: Vec<&Command> = reg
                .all()
                .iter()
                .filter(|c| c.tab.as_deref() == Some(&self.tab) && c.on("R"))
                .collect();
            if self.tab == "modify" {
                // the contextual tab keeps these groups in this order (stable sort)
                v.sort_by_key(|c| crate::commands::modify_rank(c));
            }
            v
        }
    }

    /// Alt alone toggles the tips; while they show, letters and Esc belong to the ribbon.
    fn tip_input(&mut self, ui: &Ui) -> Vec<TipKey> {
        let (alt, events) = ui.input(|i| (i.modifiers.alt, i.events.clone()));
        let other = events.iter().any(|e| {
            matches!(
                e,
                egui::Event::Key { pressed: true, .. }
                    | egui::Event::PointerButton { pressed: true, .. }
            )
        });
        self.tips.alt(alt, other);
        if !self.tips.active() {
            return vec![];
        }
        let mut keys = vec![];
        for e in &events {
            match e {
                egui::Event::PointerButton { pressed: true, .. } => self.tips.off(),
                egui::Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    modifiers,
                    ..
                } if !modifiers.command && !modifiers.alt => {
                    if *key == egui::Key::Escape {
                        keys.push(TipKey::Esc);
                    } else if let Some(c) = key_char(physical_key.unwrap_or(*key)) {
                        keys.push(TipKey::Char(c));
                    }
                }
                _ => {}
            }
        }
        if self.tips.active() {
            ui.ctx().input_mut(|i| {
                i.events
                    .retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)))
            });
        }
        keys
    }

    /// Returns a clicked command id.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        th: &Theme,
        cat: &Catalog,
        reg: &Registry,
        icons: &mut IconCache,
        active: Option<&str>,
    ) -> Option<String> {
        let wc = th.ribbon_class(ui.ctx().screen_rect().width());
        self.normalize_tab(&reg.tabs());
        ui.spacing_mut().item_spacing.y = 0.0;
        let typed = self.tip_input(ui);
        let (strip, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), k::TAB_H), Sense::hover());
        let font = th.font(Role::Tab);
        let mut tabs: Vec<(String, String)> = ordered_tabs(reg.tabs(), &self.tab_order)
            .into_iter()
            .map(|t| (cat.t(&format!("tab.{t}")), t))
            .collect();
        if let Some((_, label)) = &self.context {
            tabs.push((format!("{} | {label}", cat.t("tab.ctx")), CTX_TAB.into()));
        }
        let tab_tips = rl::assign_tips(
            1,
            &tabs
                .iter()
                .map(|(_, t)| (None, t.as_str()))
                .collect::<Vec<_>>(),
        );
        if self.tips.level == TipLevel::Tabs {
            for key in &typed {
                match key {
                    TipKey::Esc => self.tips.escape(),
                    TipKey::Char(c) => {
                        if let TipHit::Hit(i) = self.tips.feed(*c, &tab_tips) {
                            self.tab = tabs[i].1.clone();
                            self.peek = self.collapsed;
                            self.tips.level = TipLevel::Commands;
                            break;
                        }
                    }
                }
            }
        }
        let widths: Vec<f32> = tabs
            .iter()
            .map(|(name, _)| text_w(ui, name, &font) + 24.0)
            .collect();
        let active_tab = tabs.iter().position(|(_, t)| *t == self.tab).unwrap_or(0);
        // the collapse toggle holds the right end of the strip
        let toggle_w = 36.0;
        let more_w = 28.0;
        let (shown, hidden) = fit_tabs(&widths, strip.width() - toggle_w, more_w, active_tab);
        let hidden_tabs: Vec<(String, String)> = hidden.iter().map(|&i| tabs[i].clone()).collect();
        let tips_layer = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("ribbon.tips"),
        ));
        let mut x = strip.left();
        for &i in &shown {
            let (name, t) = &tabs[i];
            let on = self.tab == *t;
            let ctx_tab = t == CTX_TAB;
            let g = ui
                .painter()
                .layout_no_wrap(name.clone(), font.clone(), Color32::PLACEHOLDER);
            let r = Rect::from_min_size(pos2(x, strip.top()), Vec2::new(widths[i], k::TAB_H));
            x = r.right();
            let resp = ui.interact(r, ui.id().with(("tab", t.as_str())), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, name.as_str()));
            let hov = fade(ui, th, resp.id, resp.hovered() && !on);
            let p = ui.painter();
            let pill = r.shrink2(Vec2::new(2.0, 5.0));
            if ctx_tab {
                p.rect_filled(pill, th.radius("sm"), th.c("accent"));
            } else if hov > 0.0 {
                p.rect_filled(pill, th.radius("sm"), th.hover_fill().gamma_multiply(hov));
            }
            let col = if ctx_tab {
                th.c("accent.text_on")
            } else if on {
                th.c("text.primary")
            } else {
                th.c("text.secondary").lerp_to_gamma(th.c("text.primary"), hov)
            };
            p.galley_with_override_text_color(r.center() - g.size() / 2.0, g, col);
            if on && !self.collapsed {
                let u = Rect::from_min_max(
                    pos2(r.left() + 10.0, r.bottom() - stroke::INDICATOR),
                    pos2(r.right() - 10.0, r.bottom()),
                );
                p.rect_filled(u, 1.0, th.c("accent"));
            }
            if resp.has_focus() {
                super::own_focus(p.ctx(), resp.id);
                focus_ring(p, th, pill);
            }
            if self.tips.level == TipLevel::Tabs {
                tip_badge(&tips_layer, th, pos2(r.center().x, r.bottom() - 2.0), &tab_tips[i], "");
            }
            if resp.clicked() {
                if self.collapsed {
                    // tabs only: the band opens over the canvas; the open tab closes it
                    self.peek = !(on && self.peek);
                    self.tab = t.clone();
                } else if on {
                    self.collapsed = true;
                } else {
                    self.tab = t.clone();
                }
            }
        }
        if !hidden_tabs.is_empty() {
            let r = Rect::from_min_size(
                pos2(x, strip.top() + 4.0),
                Vec2::new(more_w, k::TAB_H - 8.0),
            );
            let resp = ui.interact(r, ui.id().with("ribbon.tabs.more"), Sense::click());
            let fg = super::paint_state(ui, th, r, &resp, false);
            // painted dots: the UI font has no ellipsis-in-a-box glyph
            for d in [-5.0, 0.0, 5.0] {
                ui.painter()
                    .circle_filled(r.center() + Vec2::new(d, 0.0), 1.5, fg);
            }
            let resp = super::rich_tooltip(resp, th, &cat.t("ribbon.tabs.more"), "", "");
            egui::Popup::menu(&resp).show(|ui| {
                for (name, t) in &hidden_tabs {
                    if ui.button(name).clicked() {
                        self.tab = t.clone();
                        self.peek = self.collapsed;
                        ui.close();
                    }
                }
            });
        }
        let cr = Rect::from_center_size(
            strip.right_center() - Vec2::new(16.0, 0.0),
            Vec2::splat(24.0),
        );
        let resp = ui.interact(cr, ui.id().with("ribbon.toggle"), Sense::click());
        let fg = super::paint_state(ui, th, cr, &resp, false);
        let s = if self.collapsed { 2.0 } else { -2.0 };
        let c = pos2(
            cr.center().x.round(),
            px::snap(cr.center().y, ui.ctx().pixels_per_point()),
        );
        ui.painter().add(egui::Shape::line(
            vec![
                c + Vec2::new(-4.0, -s),
                c + Vec2::new(0.0, s),
                c + Vec2::new(4.0, -s),
            ],
            Stroke::new(1.5, fg),
        ));
        let key = if self.collapsed {
            "ribbon.expand"
        } else {
            "ribbon.collapse"
        };
        let tname = cat.t(key);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tname.as_str()));
        if super::rich_tooltip(resp, th, &cat.t(key), "", &self.toggle_keys).clicked() {
            self.collapsed = !self.collapsed;
            self.peek = false;
        }
        if self.collapsed {
            if !self.peek {
                return None;
            }
            return self.peek_band(ui, strip, th, (cat, reg), icons, active, typed, wc);
        }
        self.band(ui, th, (cat, reg), icons, active, typed, wc)
    }

    /// The band over the canvas while only the tabs are docked. It goes away after a command,
    /// a press outside it and the tab strip, or Esc.
    #[allow(clippy::too_many_arguments)]
    fn peek_band(
        &mut self,
        ui: &Ui,
        strip: Rect,
        th: &Theme,
        cr: (&Catalog, &Registry),
        icons: &mut IconCache,
        active: Option<&str>,
        typed: Vec<TipKey>,
        wc: Width,
    ) -> Option<String> {
        let ctx = ui.ctx().clone();
        // The floating band lays out like the docked one, with the strip's row spacing.
        let rows = ui.spacing().item_spacing;
        let pad = 6.0;
        let frame = egui::Frame::new()
            .fill(th.c("bg.panel"))
            .shadow(th.shadow(2))
            .inner_margin(egui::Margin::symmetric(pad as i8, 0));
        let shown = egui::Area::new(egui::Id::new("ribbon.peek"))
            .order(egui::Order::Foreground)
            .fixed_pos(strip.left_bottom() - vec2(pad, 0.0))
            .constrain(false)
            .show(&ctx, |ui| {
                frame
                    .show(ui, |ui| {
                        ui.set_min_width(strip.width());
                        ui.set_max_width(strip.width());
                        ui.spacing_mut().item_spacing = rows;
                        self.band(ui, th, cr, icons, active, typed, wc)
                    })
                    .inner
            });
        let out = shown.inner;
        let r = shown.response.rect;
        let menu = egui::Popup::is_any_open(&ctx);
        let away = ctx.input(|i| {
            i.pointer.any_pressed()
                && i
                    .pointer
                    .interact_pos()
                    .is_some_and(|p| !r.contains(p) && !strip.contains(p))
        });
        let esc = !menu
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if out.is_some() || (away && !menu) || esc {
            self.peek = false;
        }
        out
    }

    /// The band of the current tab; returns a clicked command id.
    #[allow(clippy::too_many_arguments)]
    fn band(
        &mut self,
        ui: &mut Ui,
        th: &Theme,
        (cat, reg): (&Catalog, &Registry),
        icons: &mut IconCache,
        active: Option<&str>,
        typed: Vec<TipKey>,
        wc: Width,
    ) -> Option<String> {
        let mut out = None;
        let m = rl::Metrics::default();
        let cmds = self.tab_commands(reg);
        let caption = th.font(Role::RibbonLabel);
        let title_font = th.font(Role::GroupTitle);
        let tw = |s: &str, title: bool| {
            text_w(&*ui, s, if title { &title_font } else { &caption })
        };
        let (groups, ins) = build_groups(&cmds, reg, cat, &self.split, &tw, &m);
        // stages on the widest language: en and ru collapse alike
        let basis = basis_groups(&cmds, reg, cat, &self.split, &tw, &m);
        let ctips = command_tips(&groups);
        if self.tips.level == TipLevel::Commands {
            let list: Vec<String> = ctips.iter().map(|t| t.1.clone()).collect();
            for key in &typed {
                match key {
                    TipKey::Esc => self.tips.escape(),
                    TipKey::Char(c) => {
                        if let TipHit::Hit(i) = self.tips.feed(*c, &list) {
                            let id = &ctips[i].0;
                            let run = reg.get(id).map_or(id.as_str(), |c| {
                                self.split.current(&c.id, &c.variants)
                            });
                            if self.enabled(reg, run) {
                                out = Some(run.to_string());
                            }
                            self.tips.off();
                            break;
                        }
                    }
                }
            }
        }
        let tip_of = |id: &str| ctips.iter().find(|t| t.0 == id).map(|t| t.1.as_str());
        let bh = self.body_height(wc);
        let (body, body_resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), bh), Sense::click());
        body_resp.context_menu(|ui| {
            if ui
                .checkbox(&mut self.small, cat.t("ribbon.small"))
                .clicked()
            {
                ui.close();
            }
        });
        let ctx_tab = self.tab == CTX_TAB;
        if ctx_tab {
            ui.painter().rect_filled(body, 0.0, ctx_body_fill(th));
        }
        let ppp = ui.ctx().pixels_per_point();
        let top_rule = th.c(if ctx_tab { "accent" } else { "border.subtle" });
        ui.painter().hline(
            body.x_range(),
            px::snap(body.top(), ppp),
            px::stroke(ppp, top_rule),
        );
        let min = if self.small {
            Stage::NoLarge
        } else {
            Stage::Full
        };
        let avail = body.width() - 2.0 * k::EDGE;
        let lay = rl::layout_by(&basis, &ins, avail, min, &m);
        let origin = pos2(body.left() + k::EDGE, body.top() + k::PAD_Y);
        let band_bottom = origin.y + k::BAND_H;
        let show_tips = self.tips.level == TipLevel::Commands;
        for (gi, (go, gd)) in lay.groups.iter().zip(&groups).enumerate() {
            let gx = origin.x + go.x;
            if gi + 1 < lay.groups.len() || lay.overflow_from < groups.len() {
                let sx = px::snap(gx + go.w + m.group_gap / 2.0, ppp);
                let bottom = if self.small {
                    band_bottom - 4.0
                } else {
                    band_bottom + k::TITLE_H - 8.0
                };
                ui.painter().vline(
                    sx,
                    egui::Rangef::new(origin.y + 6.0, bottom),
                    px::stroke(ppp, th.c("border.subtle")),
                );
            }
            if let Some(cr) = go.collapsed_rect {
                let r = cr.translate(origin.to_vec2());
                out = self
                    .collapsed_group(ui, th, cat, reg, icons, gd, r, active)
                    .or(out);
                continue;
            }
            for p in &go.items {
                let e = &gd.entries[p.item];
                let r = p.rect.translate(origin.to_vec2());
                let tip = show_tips.then(|| tip_of(&e.cmd.id)).flatten();
                out = self
                    .button(ui, th, cat, reg, icons, e, p.size, r, active, tip)
                    .or(out);
            }
            if !self.small && !gd.title_hidden {
                let row = Rect::from_min_size(
                    pos2(gx, band_bottom),
                    vec2(go.w, k::TITLE_H),
                );
                ui.painter().text(
                    pos2(row.center().x.round(), row.center().y),
                    Align2::CENTER_CENTER,
                    &gd.title,
                    title_font.clone(),
                    th.c("text.secondary"),
                );
                if let Some(l) = gd.launcher {
                    let lr = Rect::from_center_size(
                        pos2(row.right() - m.launcher_w / 2.0 - 2.0, row.center().y),
                        Vec2::splat(m.launcher_w),
                    );
                    let tip = show_tips.then(|| tip_of(&l.id)).flatten();
                    out = self.launcher(ui, th, cat, reg, l, lr, tip).or(out);
                }
            }
        }
        if lay.overflow_from < groups.len() {
            let rest: Vec<&Entry> = groups[lay.overflow_from..]
                .iter()
                .flat_map(|g| g.entries.iter())
                .collect();
            let label = format!("{} · {}", cat.t("ribbon.overflow"), rest.len());
            let r = Rect::from_min_size(
                pos2(origin.x + avail - m.overflow_w, origin.y),
                vec2(m.overflow_w, k::ROW_H),
            );
            let resp = ui.interact(r, ui.id().with("ribbon.more"), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.as_str()));
            let fg = super::paint_state(ui, th, r, &resp, false);
            ui.painter().text(
                pos2(r.left() + 8.0, r.center().y),
                Align2::LEFT_CENTER,
                label,
                caption.clone(),
                fg,
            );
            chevron(ui.painter(), pos2(r.right() - 9.0, r.center().y), fg);
            let resp = super::rich_tooltip(resp, th, &cat.t("ribbon.more_tip"), "", "");
            let rows: Vec<Row> = rest
                .iter()
                .flat_map(|e| self.rows_of(cat, reg, e.cmd, active))
                .collect();
            out = self.menu(&resp, th, icons, rows).or(out);
        }
        out
    }

    /// Menu rows of a command: the command and its variants.
    fn rows_of(&self, cat: &Catalog, reg: &Registry, c: &Command, active: Option<&str>) -> Vec<Row> {
        SplitState::menu(&c.id, &c.variants)
            .into_iter()
            .filter_map(|id| reg.get(id))
            .map(|v| Row {
                id: v.id.clone(),
                parent: (!c.variants.is_empty()).then(|| (c.id.clone(), c.variants.clone())),
                icon: v.icon.clone(),
                label: cat.t(&v.label_key()),
                keys: v.shortcuts.first().cloned().unwrap_or_default(),
                desc: {
                    let dk = format!("{}.desc", v.label_key());
                    let d = cat.t(&dk);
                    if d == dk { String::new() } else { d }
                },
                reason: self
                    .blocked_reason(reg, &v.id)
                    .map_or_else(|| cat.t("ribbon.soon_tip"), str::to_string),
                live: self.enabled(reg, &v.id),
                on: active == Some(reg.resolve(&v.id)),
                once: v.once,
            })
            .collect()
    }

    /// Drop-down menu under `resp`; returns the picked command and remembers split picks.
    fn menu(
        &mut self,
        resp: &egui::Response,
        th: &Theme,
        icons: &mut IconCache,
        rows: Vec<Row>,
    ) -> Option<String> {
        let mut out = None;
        let split = &mut self.split;
        // a list taller than the room under the button wraps into columns: a shifted-up menu
        // would cover the button and hide its first rows
        let row_h = k::ROW_H + 4.0;
        let room = resp.ctx.screen_rect().bottom() - resp.rect.bottom() - 2.0 * row_h;
        let fit = ((room / row_h).floor() as usize).max(4);
        let cols = rows.len().div_ceil(fit).max(1);
        let per_col = rows.len().div_ceil(cols).max(1);
        let mut draw = |ui: &mut Ui, chunk: &[Row]| {
            for r in chunk {
                let resp = menu_row(ui, th, icons, r);
                let resp = if r.live {
                    super::rich_tooltip(resp, th, &r.label, &r.desc, &r.keys)
                } else {
                    resp.on_hover_text(r.reason.as_str())
                };
                if r.live && resp.clicked() {
                    if let Some((p, vars)) = &r.parent {
                        split.choose(p, &r.id, vars, r.once);
                    }
                    out = Some(r.id.clone());
                    ui.close();
                }
            }
        };
        egui::Popup::menu(resp).show(|ui| {
            ui.set_min_width(k::MENU_W);
            ui.spacing_mut().item_spacing.y = 0.0;
            if cols == 1 {
                draw(ui, &rows);
                return;
            }
            ui.horizontal_top(|ui| {
                for chunk in rows.chunks(per_col) {
                    ui.vertical(|ui| {
                        ui.set_width(k::MENU_W);
                        draw(ui, chunk);
                    });
                }
            });
        });
        out
    }

    /// One command button at its size; a split button adds a caret part with the variants.
    #[allow(clippy::too_many_arguments)]
    fn button(
        &mut self,
        ui: &Ui,
        th: &Theme,
        cat: &Catalog,
        reg: &Registry,
        icons: &mut IconCache,
        e: &Entry,
        size: Size,
        rect: Rect,
        active: Option<&str>,
        tip: Option<&str>,
    ) -> Option<String> {
        let mut out = None;
        let split = !e.cmd.variants.is_empty();
        let live = self.enabled(reg, &e.show.id);
        let on = active.is_some_and(|a| {
            SplitState::menu(&e.cmd.id, &e.cmd.variants)
                .iter()
                .any(|v| a == reg.resolve(v))
        });
        let (main_r, caret_r) = parts(size, rect, split);
        let id = ui.id().with(("cmd", e.cmd.id.as_str()));
        let main = ui.interact(
            main_r,
            id,
            if live { Sense::click() } else { Sense::hover() },
        );
        let caret = caret_r.map(|r| ui.interact(r, id.with("caret"), Sense::click()));
        note_rect(ui.ctx(), &e.cmd.id, rect);
        for v in &e.cmd.variants {
            note_rect(ui.ctx(), v, caret_r.unwrap_or(rect));
        }
        let caret_hover = caret.as_ref().is_some_and(|c| c.hovered());
        let hover = (main.hovered() && live) || caret_hover;
        let t = fade(ui, th, id, hover || main.has_focus());
        let p = ui.painter();
        let radius = th.radius("md");
        let press_main = main.is_pointer_button_down_on() && live;
        let press_caret = caret.as_ref().is_some_and(|c| c.is_pointer_button_down_on());
        let pressed = press_main || press_caret;
        // one fill per part: the hot part of a split button reads as its own key
        let main_hot = (main.hovered() && live) || main.has_focus();
        let caret_hot = caret_hover || caret.as_ref().is_some_and(|c| c.has_focus());
        let fills = button_fills(
            on,
            (main_hot, press_main),
            caret_r.map(|_| (caret_hot, press_caret)),
        );
        let paint = |r: Rect, f: PartFill| {
            if let Some(c) = f.color(th) {
                let a = if f == PartFill::Hover { t } else { 1.0 };
                p.rect_filled(r, radius, c.gamma_multiply(a));
            }
        };
        match caret_r {
            Some(cr) if fills[0] != fills[1] => {
                paint(main_r, fills[0]);
                paint(cr, fills[1]);
            }
            _ => paint(rect, fills[0]),
        }
        if on {
            let edge = th.c("accent").gamma_multiply(0.35);
            p.rect_stroke(rect, radius, Stroke::new(stroke::HAIRLINE, edge), StrokeKind::Inside);
        }
        if let Some(cr) = caret_r {
            if t > 0.0 || on {
                let a = if on { 0.6 } else { t };
                let ppp = p.ctx().pixels_per_point();
                let line = px::stroke(ppp, th.c("border.subtle").gamma_multiply(a));
                if size == Size::Large {
                    p.hline(rect.x_range().shrink(4.0), px::snap(cr.top(), ppp), line);
                } else {
                    p.vline(px::snap(cr.left(), ppp), rect.y_range().shrink(4.0), line);
                }
            }
        }
        if main.has_focus() || caret.as_ref().is_some_and(|c| c.has_focus()) {
            super::own_focus(p.ctx(), main.id);
            if let Some(c) = &caret {
                super::own_focus(p.ctx(), c.id);
            }
            focus_ring(p, th, rect);
        }
        // the accent ink is paired with its own subtle fill only, not with the pressed one
        let sel = on && !pressed;
        // icon colour roles: ink at rest and on hover (the tile fill shows the hover), accent
        // when selected, ink at 40 % when the command cannot run
        let ink = if !live {
            tint(th, Tint::Disabled)
        } else if sel {
            tint(th, Tint::Active)
        } else {
            tint(th, Tint::Rest)
        };
        // caption ink comes from fill_ink so the contrast test covers what is painted
        let text_c = if !live {
            th.c("text.disabled")
        } else {
            fill_ink(th, if sel { PartFill::Selected } else { PartFill::None })
        };
        let caption = th.font(Role::RibbonLabel);
        let icon_at;
        match size {
            Size::Large => {
                let ir = Rect::from_min_size(
                    pos2(
                        (rect.center().x - k::LARGE_ICON / 2.0).round(),
                        rect.top() + k::LARGE_ICON_TOP,
                    ),
                    Vec2::splat(k::LARGE_ICON),
                );
                paint_icon(ui, icons, &e.show.icon, 24, ir, ink, on);
                icon_at = pos2(ir.center().x, ir.bottom() - 2.0);
                let cx = rect.center().x;
                let y0 = rect.top() + k::LABEL_TOP + k::LINE_H / 2.0;
                let (l1, l2) = (
                    e.words[..e.wrap.split].join(" "),
                    e.words[e.wrap.split..].join(" "),
                );
                let room = rect.width() - 2.0 * k::LARGE_PAD_X + 0.5;
                fit_text(p, pos2(cx, y0), Align2::CENTER_CENTER, &l1, &caption, text_c, room);
                let y1 = y0 + k::LINE_H;
                if split {
                    let l2w = e.wrap.line2.min(room - k::CARET_W);
                    let lx = (cx - (l2w + k::CARET_W) / 2.0).round();
                    if !l2.is_empty() {
                        let at = pos2(lx, y1);
                        fit_text(p, at, Align2::LEFT_CENTER, &l2, &caption, text_c, l2w + 0.5);
                    }
                    let cx2 = if e.wrap.line2 > 0.0 {
                        lx + l2w + k::CARET_W / 2.0
                    } else {
                        cx
                    };
                    chevron(p, pos2(cx2, y1), ink);
                } else if !l2.is_empty() {
                    fit_text(p, pos2(cx, y1), Align2::CENTER_CENTER, &l2, &caption, text_c, room);
                }
            }
            Size::Medium => {
                let ir = Rect::from_min_size(
                    pos2(rect.left() + k::MEDIUM_PAD_L, rect.center().y - k::MEDIUM_ICON / 2.0),
                    Vec2::splat(k::MEDIUM_ICON),
                );
                paint_icon(ui, icons, &e.show.icon, 16, ir, ink, on);
                icon_at = ir.center();
                let x0 = ir.right() + k::MEDIUM_GAP;
                let room = main_r.right() - k::MEDIUM_PAD_R - x0 + 0.5;
                let at = pos2(x0, rect.center().y);
                fit_text(p, at, Align2::LEFT_CENTER, &e.label, &caption, text_c, room);
            }
            Size::Small => {
                let ir = Rect::from_center_size(main_r.center(), Vec2::splat(k::MEDIUM_ICON));
                let ir = Rect::from_min_size(
                    pos2(ir.left().round(), ir.top().round()),
                    ir.size(),
                );
                paint_icon(ui, icons, &e.show.icon, 16, ir, ink, on);
                icon_at = ir.center();
            }
        }
        if let Some(cr) = caret_r {
            if size != Size::Large {
                chevron(p, cr.center(), ink);
            }
        }
        if let Some(tip) = tip {
            let layer = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("ribbon.tips"),
            ));
            tip_badge(&layer, th, icon_at, tip, &self.tips.typed);
        }
        let mut keys: Vec<String> = e.show.shortcuts.clone();
        if keys.is_empty() {
            // a second button of a tool shows the keys of the command it runs
            let target = reg.get(reg.resolve(&e.show.id));
            keys = target.map_or_else(Vec::new, |t| t.shortcuts.clone());
        }
        if let Some(mn) = &e.show.mnemonic {
            keys.push(mn.clone());
        }
        let full = cat.t(&e.show.label_key());
        let dk = format!("{}.desc", e.show.label_key());
        let reason = (!live).then(|| {
            self.blocked_reason(reg, &e.show.id)
                .map_or_else(|| cat.t("ribbon.soon_tip"), str::to_string)
        });
        let tip = tip_text(&full, &cat.t(&dk), &dk, &keys, reason);
        let name = a11y_label(&full, keys.first().map_or("", String::as_str), (!live).then_some(tip.reason.as_str()));
        main.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, live, name.as_str()));
        if super::rich_tooltip_reason(main, th, &tip.title, &tip.desc, &tip.keys, &tip.reason)
            .clicked()
            && live
        {
            out = Some(e.show.id.clone());
        }
        if let Some(c) = caret {
            let split_name = a11y_label(&format!("{} - {}", full, cat.t("ribbon.split_tip")), "", None);
            c.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, split_name.as_str()));
            let c = super::rich_tooltip(c, th, &cat.t("ribbon.split_tip"), "", "");
            let rows = self.rows_of(cat, reg, e.cmd, active);
            out = self.menu(&c, th, icons, rows).or(out);
        }
        out
    }

    /// A group shrunk to one drop-down: the first command's icon over the group title.
    #[allow(clippy::too_many_arguments)]
    fn collapsed_group(
        &mut self,
        ui: &Ui,
        th: &Theme,
        cat: &Catalog,
        reg: &Registry,
        icons: &mut IconCache,
        g: &GroupDraw,
        rect: Rect,
        active: Option<&str>,
    ) -> Option<String> {
        let id = ui.id().with(("group", g.title.as_str()));
        let resp = ui.interact(rect, id, Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, g.title.as_str()));
        for e in &g.entries {
            note_rect(ui.ctx(), &e.cmd.id, rect);
        }
        let t = fade(ui, th, id, resp.hovered() || resp.has_focus());
        let p = ui.painter();
        if resp.is_pointer_button_down_on() {
            p.rect_filled(rect, th.radius("md"), th.pressed_fill());
        } else if t > 0.0 {
            p.rect_filled(rect, th.radius("md"), th.hover_fill().gamma_multiply(t));
        }
        if resp.has_focus() {
            super::own_focus(p.ctx(), resp.id);
            focus_ring(p, th, rect);
        }
        let ink = th.c("text.secondary").lerp_to_gamma(th.c("text.primary"), t);
        let ir = Rect::from_min_size(
            pos2(
                (rect.center().x - k::LARGE_ICON / 2.0).round(),
                rect.top() + k::LARGE_ICON_TOP,
            ),
            Vec2::splat(k::LARGE_ICON),
        );
        if let Some(e) = g.entries.first() {
            paint_icon(ui, icons, &e.show.icon, 24, ir, ink, false);
        }
        let caption = th.font(Role::RibbonLabel);
        let w = &g.title_wrap;
        let cx = rect.center().x;
        let y0 = rect.top() + k::LABEL_TOP + k::LINE_H / 2.0;
        let y1 = y0 + k::LINE_H;
        let l1 = g.title_words[..w.split].join(" ");
        let l2 = g.title_words[w.split..].join(" ");
        let text_c = th.c("text.primary");
        p.text(pos2(cx, y0), Align2::CENTER_CENTER, l1, caption.clone(), text_c);
        let lx = (cx - (w.line2 + k::CARET_W) / 2.0).round();
        if !l2.is_empty() {
            p.text(pos2(lx, y1), Align2::LEFT_CENTER, l2, caption, text_c);
            chevron(p, pos2(lx + w.line2 + k::CARET_W / 2.0, y1), ink);
        } else {
            chevron(p, pos2(cx, y1), ink);
        }
        let resp = super::rich_tooltip(resp, th, &g.title, "", "");
        let rows: Vec<Row> = g
            .entries
            .iter()
            .flat_map(|e| self.rows_of(cat, reg, e.cmd, active))
            .chain(
                g.launcher
                    .into_iter()
                    .flat_map(|l| self.rows_of(cat, reg, l, active)),
            )
            .collect();
        self.menu(&resp, th, icons, rows)
    }

    /// Group settings launcher: a small corner arrow at the right end of the title row.
    #[allow(clippy::too_many_arguments)]
    fn launcher(
        &self,
        ui: &Ui,
        th: &Theme,
        cat: &Catalog,
        reg: &Registry,
        l: &Command,
        r: Rect,
        tip: Option<&str>,
    ) -> Option<String> {
        let live = self.enabled(reg, &l.id);
        let id = ui.id().with(("launcher", l.id.as_str()));
        let resp = ui.interact(r, id, if live { Sense::click() } else { Sense::hover() });
        let lname = cat.t(&l.label_key());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, live, lname.as_str()));
        note_rect(ui.ctx(), &l.id, r);
        let t = fade(ui, th, id, resp.hovered() && live);
        let p = ui.painter();
        if t > 0.0 {
            p.rect_filled(r, th.radius("sm"), th.hover_fill().gamma_multiply(t));
        }
        if resp.has_focus() {
            super::own_focus(p.ctx(), resp.id);
            focus_ring(p, th, r);
        }
        let ink = if live {
            th.c("text.secondary").lerp_to_gamma(th.c("text.primary"), t)
        } else {
            th.c("text.disabled")
        };
        // a corner bracket with an arrow out of it: "more settings for this group"
        let ppp = ui.ctx().pixels_per_point();
        let c = pos2(px::snap(r.center().x, ppp), px::snap(r.center().y, ppp));
        let s = px::stroke(ppp, ink);
        p.add(egui::Shape::line(
            vec![c + vec2(-3.5, 0.5), c + vec2(-3.5, -3.5), c + vec2(0.5, -3.5)],
            s,
        ));
        p.line_segment([c + vec2(-1.5, -1.5), c + vec2(3.5, 3.5)], s);
        p.add(egui::Shape::line(
            vec![c + vec2(3.5, 0.0), c + vec2(3.5, 3.5), c + vec2(0.0, 3.5)],
            s,
        ));
        if let Some(tip) = tip {
            let layer = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("ribbon.tips"),
            ));
            tip_badge(&layer, th, r.center(), tip, &self.tips.typed);
        }
        let label = cat.t(&l.label_key());
        let dk = format!("{}.desc", l.label_key());
        let reason = (!live).then(|| {
            self.blocked_reason(reg, &l.id)
                .map_or_else(|| cat.t("ribbon.soon_tip"), str::to_string)
        });
        let tip = tip_text(&label, &cat.t(&dk), &dk, &[], reason);
        (super::rich_tooltip_reason(resp, th, &tip.title, &tip.desc, &tip.keys, &tip.reason)
            .clicked()
            && live)
            .then(|| l.id.clone())
    }

    /// Body height: band and group titles; small-buttons mode drops the titles.
    pub fn body_height(&self, wc: Width) -> f32 {
        if self.small {
            k::PAD_Y + k::BAND_H + k::PAD_Y
        } else {
            body_h(wc)
        }
    }
}

/// Main and caret parts of a button; a large split button opens its menu from the label.
fn parts(size: Size, r: Rect, split: bool) -> (Rect, Option<Rect>) {
    if !split {
        return (r, None);
    }
    match size {
        Size::Large => {
            let y = r.top() + k::LABEL_TOP - 2.0;
            (
                Rect::from_min_max(r.min, pos2(r.right(), y)),
                Some(Rect::from_min_max(pos2(r.left(), y), r.max)),
            )
        }
        _ => {
            let x = r.right() - k::CARET_W;
            (
                Rect::from_min_max(r.min, pos2(x, r.bottom())),
                Some(Rect::from_min_max(pos2(x, r.top()), r.max)),
            )
        }
    }
}

/// One line anchored at `at`, elided with "…" past `max_w`; the full name is in the tooltip.
fn fit_text(
    p: &egui::Painter,
    at: Pos2,
    align: Align2,
    s: &str,
    font: &FontId,
    color: Color32,
    max_w: f32,
) {
    let fmt = egui::TextFormat::simple(font.clone(), color);
    let mut job = egui::text::LayoutJob::single_section(s.to_owned(), fmt);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(1.0));
    let g = p.layout_job(job);
    p.galley(align.anchor_size(at, g.size()).min, g, color);
}

/// Tooltip parts of a ribbon button: title, description, shortcuts, optional disabled reason.
struct TipText {
    title: String,
    desc: String,
    keys: String,
    reason: String,
}

/// A missing description (the catalog returns the key itself) falls back to the label;
/// a disabled button keeps its description and adds the reason.
fn tip_text(
    label: &str,
    desc: &str,
    desc_key: &str,
    keys: &[String],
    reason: Option<String>,
) -> TipText {
    let desc = if desc.is_empty() || desc == desc_key { label } else { desc };
    TipText {
        title: label.to_string(),
        desc: desc.to_string(),
        keys: keys.join(" / "),
        reason: reason.unwrap_or_default(),
    }
}

fn text_w(ui: &Ui, s: &str, font: &FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(s.to_owned(), font.clone(), Color32::PLACEHOLDER)
        .size()
        .x
}

/// Hover and press fade: about 120 ms, ease-out; instant with reduced motion.
fn fade(ui: &Ui, th: &Theme, id: egui::Id, on: bool) -> f32 {
    let time = if th.reduced_motion { 0.0 } else { k::MOTION_S };
    ui.ctx().animate_bool_with_time_and_easing(
        id.with("ribbon.fade"),
        on,
        time,
        egui::emath::easing::cubic_out,
    )
}

fn focus_ring(p: &egui::Painter, th: &Theme, r: Rect) {
    p.rect_stroke(
        r.expand(1.0),
        th.radius("md") + 1.0,
        Stroke::new(stroke::FOCUS, th.c("focus.ring")),
        StrokeKind::Outside,
    );
}

/// Small down caret, centred on whole pixels.
fn chevron(p: &egui::Painter, c: Pos2, color: Color32) {
    let c = pos2(c.x.round(), px::snap(c.y, p.ctx().pixels_per_point()));
    p.add(egui::Shape::line(
        vec![c + vec2(-3.0, -1.5), c + vec2(0.0, 1.5), c + vec2(3.0, -1.5)],
        Stroke::new(1.25, color),
    ));
}

/// Key tip badge centred on `at`; tips that no longer match the typed prefix are hidden.
fn tip_badge(p: &egui::Painter, th: &Theme, at: Pos2, tip: &str, typed: &str) {
    if !tip.starts_with(typed) {
        return;
    }
    let font = th.font(Role::Keycap);
    let g = p.layout_no_wrap(tip.to_string(), font, th.c("text.primary"));
    let w = (g.size().x + 8.0).max(k::TIP_H).round();
    let r = Rect::from_center_size(pos2(at.x.round(), at.y.round()), vec2(w, k::TIP_H));
    p.rect(
        r,
        th.radius("sm"),
        th.c("bg.elevated"),
        Stroke::new(stroke::HAIRLINE, th.c("border.overlay")),
        StrokeKind::Inside,
    );
    p.galley(r.center() - g.size() / 2.0, g, th.c("text.primary"));
}

fn key_char(k: egui::Key) -> Option<char> {
    let n = k.name();
    let mut it = n.chars();
    match (it.next(), it.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => Some(c.to_ascii_uppercase()),
        _ => None,
    }
}

/// Accessible name: "Title (Ctrl+S)", plus the reason when the command is unavailable.
pub(crate) fn a11y_label(title: &str, keys: &str, reason: Option<&str>) -> String {
    let mut s = title.to_string();
    if !keys.is_empty() {
        s.push_str(&format!(" ({keys})"));
    }
    if let Some(r) = reason.filter(|r| !r.is_empty()) {
        s.push_str(&format!(" - {r}"));
    }
    s
}

fn menu_row(
    ui: &mut Ui,
    th: &Theme,
    icons: &mut IconCache,
    r: &Row,
) -> egui::Response {
    let w = ui.available_width().max(k::MENU_W);
    let sense = if r.live {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, k::ROW_H + 4.0), sense);
    let name = a11y_label(&r.label, &r.keys, (!r.live).then_some(r.reason.as_str()));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, r.live, name.as_str()));
    let t = fade(ui, th, resp.id, resp.hovered() && r.live);
    let p = ui.painter();
    if t > 0.0 {
        p.rect_filled(rect, th.radius("sm"), th.hover_fill().gamma_multiply(t));
    }
    let (ink, text_c) = match (r.live, r.on) {
        (false, _) => (tint(th, Tint::Disabled), th.c("text.disabled")),
        (true, true) => (tint(th, Tint::Active), th.c("accent")),
        _ => (tint(th, Tint::Rest), th.c("text.primary")),
    };
    let ir = Rect::from_min_size(
        pos2(rect.left() + 6.0, (rect.center().y - k::MEDIUM_ICON / 2.0).round()),
        Vec2::splat(k::MEDIUM_ICON),
    );
    paint_icon(ui, icons, &r.icon, 16, ir, ink, r.on);
    p.text(
        pos2(ir.right() + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        &r.label,
        th.font(Role::Body),
        text_c,
    );
    if !r.keys.is_empty() {
        super::paint_keycaps(p, th, pos2(rect.right() - 6.0, rect.center().y), &r.keys);
    }
    resp
}

/// Paints a command icon in its slot size: the base direction, or the active one when `on`
/// (`data/icon_style.json`); the cache picks the optical master for the physical pixels.
fn paint_icon(
    ui: &Ui,
    icons: &mut IconCache,
    name: &str,
    logical: u32,
    rect: Rect,
    color: Color32,
    on: bool,
) {
    icons.paint(ui, &super::icon_name(name, on), logical, rect, color);
}

/// Tile caption: the short label (`cmd.<id>.short`) or the full name without the parenthesised
/// note and anything after " / "; the full name is in the tooltip.
pub fn short_label(cat: &Catalog, cmd: &Command) -> String {
    short_label_in(cat, cat.lang, cmd)
}

/// As [`short_label`], in `lang`.
pub fn short_label_in(cat: &Catalog, lang: Lang, cmd: &Command) -> String {
    let key = format!("{}.short", cmd.label_key());
    let s = cat.t_in(lang, &key);
    if s != key {
        return s;
    }
    let full = cat.t_in(lang, &cmd.label_key());
    let cut = full.split(" (").next().unwrap_or(&full);
    cut.split(" / ").next().unwrap_or(cut).trim().to_string()
}

/// Centered label wrapped to two lines at word breaks; longer text ends with "…"
/// (the full name is in the tooltip).
pub fn label_job(
    text: &str,
    font: &egui::FontId,
    color: egui::Color32,
    width: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), font.clone(), color, width);
    job.wrap.max_rows = 2;
    job.wrap.break_anywhere = false;
    job.halign = egui::Align::Center;
    job
}

/// Which tabs fit the strip, in order, and which go to the "more" menu. The active tab always
/// stays visible, taking the place of the last one that would still fit.
pub fn fit_tabs(
    widths: &[f32],
    avail: f32,
    more_w: f32,
    active: usize,
) -> (Vec<usize>, Vec<usize>) {
    let all: Vec<usize> = (0..widths.len()).collect();
    if widths.iter().sum::<f32>() <= avail {
        return (all, vec![]);
    }
    let room = avail - more_w;
    let reserve = widths.get(active).copied().unwrap_or(0.0);
    let mut used = reserve;
    let mut shown: Vec<usize> = vec![];
    for (i, w) in widths.iter().enumerate() {
        if i == active {
            continue;
        }
        if used + w > room {
            break;
        }
        used += w;
        shown.push(i);
    }
    if active < widths.len() {
        shown.push(active);
        shown.sort_unstable();
    }
    let hidden = all.into_iter().filter(|i| !shown.contains(i)).collect();
    (shown, hidden)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Lang;

    #[test]
    fn tabs_fit_or_go_to_more_with_active_kept() {
        let w = [100.0, 100.0, 100.0, 100.0];
        let (s, h) = fit_tabs(&w, 400.0, 28.0, 0);
        assert_eq!((s.len(), h.len()), (4, 0));
        let (s, h) = fit_tabs(&w, 250.0, 28.0, 0);
        assert_eq!((s, h), (vec![0, 1], vec![2, 3]));
        // the active tab stays, others make room
        let (s, h) = fit_tabs(&w, 250.0, 28.0, 3);
        assert_eq!((s, h), (vec![0, 3], vec![1, 2]));
        // a window narrower than one tab still shows the active one
        let (s, h) = fit_tabs(&w, 50.0, 28.0, 2);
        assert_eq!((s, h), (vec![2], vec![0, 1, 3]));
    }

    #[test]
    fn enabled_tooltip_has_desc_and_keys() {
        let keys = vec!["Ctrl+Z".to_string(), "UN".to_string()];
        let t = tip_text("Undo", "Revert the last action", "k.desc", &keys, None);
        assert_eq!(t.title, "Undo");
        assert_eq!(t.desc, "Revert the last action");
        assert_eq!(t.keys, "Ctrl+Z / UN");
        assert!(t.reason.is_empty());
    }

    #[test]
    fn disabled_tooltip_keeps_desc_and_reason() {
        let t = tip_text("Undo", "Revert", "k.desc", &[], Some("Nothing selected".into()));
        assert_eq!(t.desc, "Revert");
        assert_eq!(t.reason, "Nothing selected");
    }

    #[test]
    fn missing_desc_falls_back_to_label() {
        let t = tip_text("Undo", "k.desc", "k.desc", &[], Some("r".into()));
        assert_eq!(t.desc, "Undo");
        assert_eq!(tip_text("Undo", "", "k.desc", &[], None).desc, "Undo");
    }

    #[test]
    fn split_menu_rows_carry_description() {
        let (cat, reg) = (Catalog::new(Lang::En), Registry::builtin());
        let rb = Ribbon::default();
        let (mut with, mut total) = (0, 0);
        for c in reg.all().iter().filter(|c| !c.variants.is_empty()) {
            for r in rb.rows_of(&cat, &reg, c, None) {
                total += 1;
                let dk = format!("cmd.{}.desc", r.id);
                let has = cat.t(&dk) != dk;
                assert_eq!(!r.desc.is_empty(), has, "{}", r.id);
                with += usize::from(has);
            }
        }
        assert!(total > 0 && with > 0);
    }

    #[test]
    fn ribbon_labels_short() {
        let (cat, reg) = (Catalog::new(Lang::En), Registry::builtin());
        let get = |id: &str| short_label(&cat, reg.get(id).unwrap());
        assert_eq!(get("arch.wall_room"), "By outline");
        assert_eq!(get("arch.column"), "Column");
        assert_eq!(get("arch.curtain_wall"), "Curtain wall");
    }

    fn known_tabs() -> Vec<String> {
        Registry::builtin().tabs()
    }

    #[test]
    fn unknown_tab_falls_back_to_arch() {
        let known = known_tabs();
        for bad in ["", "nope", "ctx", "Modify | Walls", "  "] {
            assert_eq!(resolve_tab(bad, &known, false), "arch", "{bad:?}");
        }
        let mut r = Ribbon {
            tab: "view".into(),
            ..Default::default()
        };
        r.set_tab("zzz", &known);
        assert_eq!(r.tab, "arch");
        // a stale contextual tab without a selection also falls back
        r.tab = CTX_TAB.into();
        r.normalize_tab(&known);
        assert_eq!(r.tab, "arch");
        // no `arch` among the known tabs: the first known one
        let only = vec!["view".to_string(), "data".to_string()];
        assert_eq!(resolve_tab("zzz", &only, false), "view");
    }

    #[test]
    fn known_tab_is_unchanged() {
        let known = known_tabs();
        for t in &known {
            assert_eq!(&resolve_tab(t, &known, false), t);
            let mut r = Ribbon::default();
            r.set_tab(t, &known);
            assert_eq!(&r.tab, t);
        }
        assert_eq!(resolve_tab(CTX_TAB, &known, true), CTX_TAB);
    }

    #[test]
    fn alias_structure_opens_struct() {
        let known = known_tabs();
        assert!(known.iter().any(|t| t == "struct"));
        assert_eq!(resolve_tab("structure", &known, false), "struct");
        assert_eq!(resolve_tab("Structure", &known, false), "struct");
        assert_eq!(resolve_tab("architecture", &known, false), "arch");
    }

    #[test]
    fn strip_and_body_agree() {
        let reg = Registry::builtin();
        let known = reg.tabs();
        for bad in ["nope", "structure", CTX_TAB, "view"] {
            let mut r = Ribbon {
                tab: bad.into(),
                ..Default::default()
            };
            r.normalize_tab(&known);
            assert!(known.contains(&r.tab), "{bad}: {}", r.tab);
            assert!(!r.tab_commands(&reg).is_empty(), "{bad}: empty body on {}", r.tab);
        }
    }

    #[test]
    fn headless_render_with_unknown_tab_keeps_a_body() {
        let ctx = egui::Context::default();
        let _ = crate::theme::install_fonts(&ctx);
        let (cat, reg) = (Catalog::new(Lang::En), Registry::builtin());
        let th = Theme::new(crate::Mode::Light, crate::Density::Comfortable);
        let mut icons = IconCache::default();
        for ctx_on in [false, true] {
            let mut r = Ribbon {
                tab: "no-such-tab".into(),
                ..Default::default()
            };
            if ctx_on {
                r.set_context(Some(("wall".into(), "Walls".into())));
                r.tab = "no-such-tab".into();
            }
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    r.show(ui, &th, &cat, &reg, &mut icons, None);
                });
            });
            assert!(known_tabs().contains(&r.tab));
            assert!(!r.tab_commands(&reg).is_empty());
        }
    }

    #[test]
    fn ctx_body_uses_surface_and_accent_tokens() {
        use crate::theme::contrast;
        for mode in [crate::Mode::Light, crate::Mode::Dark] {
            let th = Theme::new(mode, crate::Density::Comfortable);
            let fill = ctx_body_fill(&th);
            assert_eq!(fill, th.c("bg.panel"));
            // not the flat grey of a disabled band, nor the page background
            assert_ne!(fill, th.c("bg.sunken"), "{mode:?}");
            assert_ne!(fill, th.c("bg.app"), "{mode:?}");
            // the chip is the accent with its text pair
            let (chip, text) = (th.c("accent"), th.c("accent.text_on"));
            assert!(contrast(text, chip) >= 4.5, "{mode:?} chip text contrast");
            // the top rule must stand out from the surface
            assert!(contrast(chip, fill) >= 3.0, "{mode:?} rule contrast");
        }
    }

    #[test]
    fn large_label_single_line() {
        let ctx = egui::Context::default();
        let _ = crate::theme::install_fonts(&ctx);
        let reg = Registry::builtin();
        let m = rl::Metrics::default();
        let mut bad: Vec<String> = vec![];
        let _ = ctx.run(Default::default(), |ctx| {
            let f = crate::typo::font(Role::RibbonLabel);
            let tw = |s: &str| {
                ctx.fonts(|fo| {
                    fo.layout_no_wrap(s.to_owned(), f.clone(), Color32::PLACEHOLDER)
                        .size()
                        .x
                })
            };
            let space = tw("x x") - 2.0 * tw("x");
            for lang in [Lang::En, Lang::Ru] {
                let cat = Catalog::new(lang);
                for c in reg.all() {
                    if !c.on("R") || c.context.is_empty() || c.size != Some(Size::Large) {
                        continue;
                    }
                    let label = short_label(&cat, c);
                    let ww: Vec<f32> = label.split_whitespace().map(|w| tw(w)).collect();
                    let w = rl::wrap2(&ww, space, !c.variants.is_empty(), &m);
                    if w.lines() != 1 {
                        bad.push(format!("{lang:?} {} {label:?}", c.id));
                    }
                }
            }
        });
        assert!(bad.is_empty(), "two-line contextual labels: {bad:?}");
    }

    /// A caption fits when it is one line under the medium cap and, on a large button, wraps to
    /// at most two lines under the large cap.
    fn caption_fits(label: &str, large: bool, margin: f32, tw: &dyn Fn(&str) -> f32) -> bool {
        if tw(label) > k::MEDIUM_LABEL_MAX - margin {
            return false;
        }
        if !large {
            return true;
        }
        let space = tw("x x") - 2.0 * tw("x");
        let ww: Vec<f32> = label.split_whitespace().map(|w| tw(w)).collect();
        let w = rl::wrap2(&ww, space, false, &rl::Metrics::default());
        w.lines() <= 2 && w.w <= k::LARGE_LABEL_MAX - margin
    }

    #[test]
    fn ru_captions_fit_with_margin_and_short_keys_pair_up() {
        let ctx = egui::Context::default();
        let _ = crate::theme::install_fonts(&ctx);
        let reg = Registry::builtin();
        let mut bad: Vec<String> = vec![];
        let _ = ctx.run(Default::default(), |ctx| {
            let f = crate::typo::font(Role::RibbonLabel);
            let tw = |s: &str| {
                ctx.fonts(|fo| {
                    fo.layout_no_wrap(s.to_owned(), f.clone(), Color32::PLACEHOLDER)
                        .size()
                        .x
                })
            };
            let cat = Catalog::new(Lang::Ru);
            for c in reg.all().iter().filter(|c| c.on("R")) {
                let large = c.size == Some(Size::Large);
                if !caption_fits(&short_label(&cat, c), large, 8.0, &tw) {
                    bad.push(c.id.clone());
                }
            }
        });
        assert!(bad.is_empty(), "ru captions without room (add a .short key): {bad:?}");

        let keys = |frags: &[(&str, &str)]| -> std::collections::BTreeSet<String> {
            let mut out = std::collections::BTreeSet::new();
            for (name, body) in frags {
                let m: HashMap<String, serde_json::Value> =
                    serde_json::from_str(body).unwrap_or_else(|e| panic!("{name}: {e}"));
                out.extend(m.into_keys().filter(|k| k.ends_with(".short")));
            }
            out
        };
        let (en, ru) = (
            keys(crate::fragments::I18N_EN_FRAGMENTS),
            keys(crate::fragments::I18N_RU_FRAGMENTS),
        );
        assert_eq!(en, ru, "en and ru .short keys differ");
    }

    #[test]
    fn caption_budget_rejects_planted_long_label_until_short_exists() {
        let tw = |s: &str| s.chars().count() as f32 * 7.0;
        let long = "A caption that is clearly longer than the button";
        assert!(!caption_fits(long, false, 0.0, &tw));
        assert!(!caption_fits(long, true, 0.0, &tw));
        assert!(caption_fits("Caption", true, 0.0, &tw));
    }

    #[test]
    fn short_label_falls_back_to_full_and_tooltip_keeps_full() {
        let reg = Registry::builtin();
        let ru = Catalog::new(Lang::Ru);
        // no .short key: the full name, cut at the parenthesised note
        let plain = reg.get("str.brace").unwrap();
        let key = format!("{}.short", plain.label_key());
        assert_eq!(ru.t(&key), key);
        let full = ru.t(&plain.label_key());
        assert_eq!(short_label(&ru, plain), full.split(" (").next().unwrap().trim());
        // with .short: the caption is shorter, the tooltip title stays the full name
        for id in ["arch.room_auto", "data.export_ifc", "view.place_sheet_list"] {
            let c = reg.get(id).unwrap();
            let full = ru.t(&c.label_key());
            let short = short_label(&ru, c);
            assert!(short != full && short.len() < full.len(), "{id}: {short}");
            let tip = tip_text(&full, &ru.t(&format!("{}.desc", c.label_key())), "-", &[], None);
            assert_eq!(tip.title, full, "{id}");
        }
    }

    #[test]
    fn contextual_tab_opens_and_returns() {
        let mut r = Ribbon {
            tab: "view".into(),
            ..Default::default()
        };
        r.set_context(Some(("wall".into(), "Walls".into())));
        assert_eq!(r.tab, CTX_TAB);
        r.set_context(Some(("duct".into(), "Ducts".into())));
        assert_eq!(r.tab, CTX_TAB);
        r.set_context(None);
        assert_eq!(r.tab, "view");
        let reg = Registry::builtin();
        r.available = Some(["arch.level".to_string()].into_iter().collect());
        assert!(r.enabled(&reg, "str.level") && !r.enabled(&reg, "str.beam"));
    }

    #[test]
    fn long_labels_wrap_to_two_rows() {
        let ctx = crate::theme::headless_ctx();
        let _ = ctx.run(Default::default(), |ctx| {
            let f = crate::typo::font(Role::RibbonLabel);
            let g = ctx.fonts(|fo| {
                fo.layout_job(label_job(
                    "Wall by room outline",
                    &f,
                    egui::Color32::WHITE,
                    60.0,
                ))
            });
            assert_eq!(g.rows.len(), 2);
            assert!(g.size().x <= 60.5, "{}", g.size().x);
            let g =
                ctx.fonts(|fo| fo.layout_job(label_job("Door", &f, egui::Color32::WHITE, 60.0)));
            assert_eq!(g.rows.len(), 1);
        });
    }

    /// The caption of every ribbon command fits its button at the ribbon label size in every
    /// shipped locale: one line under the medium cap, two lines under the large cap, no "…".
    #[test]
    fn every_locale_label_fits_its_button() {
        let ctx = egui::Context::default();
        let _ = crate::theme::install_fonts(&ctx);
        let reg = Registry::builtin();
        let m = rl::Metrics::default();
        let mut bad: Vec<String> = vec![];
        let _ = ctx.run(Default::default(), |ctx| {
            let f = crate::typo::font(Role::RibbonLabel);
            let tw = |s: &str| {
                ctx.fonts(|fo| {
                    fo.layout_no_wrap(s.to_owned(), f.clone(), Color32::PLACEHOLDER)
                        .size()
                        .x
                })
            };
            let space = tw("x x") - 2.0 * tw("x");
            for lang in [Lang::En, Lang::Ru] {
                let cat = Catalog::new(lang);
                for c in reg.all() {
                    if !c.on("R") {
                        continue;
                    }
                    let label = short_label(&cat, c);
                    let one = tw(&label);
                    if one > k::MEDIUM_LABEL_MAX {
                        bad.push(format!("{lang:?} {} one line {one:.1}", c.id));
                    }
                    if c.size != Some(Size::Large) {
                        continue;
                    }
                    let ww: Vec<f32> = label.split_whitespace().map(|w| tw(w)).collect();
                    let w = rl::wrap2(&ww, space, !c.variants.is_empty(), &m);
                    let job = label_job(&label, &f, Color32::WHITE, k::LARGE_LABEL_MAX);
                    let g = ctx.fonts(|fo| fo.layout_job(job));
                    if w.w > k::LARGE_LABEL_MAX || w.lines() > 2 || g.elided {
                        bad.push(format!("{lang:?} {} two lines {:.1}", c.id, w.w));
                    }
                }
            }
        });
        let msg = bad.join("\n");
        assert!(bad.is_empty(), "{} labels over the cap:\n{msg}", bad.len());
    }

    /// Variants and launchers in the commands json point at real ribbon commands of the same tab.
    #[test]
    fn ribbon_metadata_consistent() {
        let reg = Registry::builtin();
        let mut owner: HashMap<&str, &str> = HashMap::new();
        for c in reg.all() {
            if let Some(p) = c.priority {
                assert!((1..=9).contains(&p), "{}: priority {p}", c.id);
            }
            for v in &c.variants {
                let vc = reg.get(v).unwrap_or_else(|| panic!("{}: unknown variant {v}", c.id));
                assert!(vc.on("R"), "{v}: variant not on the ribbon");
                assert_eq!(vc.tab, c.tab, "{v}: variant of {} on another tab", c.id);
                assert!(vc.variants.is_empty(), "{v}: nested variants");
                assert!(!vc.launcher, "{v}: a variant cannot be a launcher");
                assert!(owner.insert(v, &c.id).is_none(), "{v}: variant of two buttons");
            }
            if c.launcher || c.size.is_some() {
                assert!(c.on("R"), "{}: ribbon metadata off the ribbon", c.id);
            }
        }
        // the everyday tools are large
        for id in [
            "arch.wall",
            "arch.door",
            "arch.window",
            "arch.floor",
            "arch.roof",
            "arch.column",
            "arch.room",
            "ann.dim",
            "mod.move",
            "mod.copy",
            "mod.rotate",
            "str.beam",
            "mep.duct",
            "view.plan",
            "view.3d",
        ] {
            assert_eq!(reg.get(id).unwrap().size, Some(Size::Large), "{id}");
        }
    }

    /// Lays out every tab with real labels and the UI font at the standard window widths.
    fn each_layout(lang: Lang, f: &mut dyn FnMut(&str, f32, &[GroupDraw], &[GroupIn], &rl::Layout)) {
        let ctx = egui::Context::default();
        let _ = crate::theme::install_fonts(&ctx);
        // fonts load on the first frame; measure from the second
        let _ = ctx.run(Default::default(), |_| {});
        let cat = Catalog::new(lang);
        let reg = Registry::builtin();
        let th = Theme::new(crate::theme::Mode::Light, crate::theme::Density::Compact);
        let m = rl::Metrics::default();
        let _ = ctx.run(Default::default(), |ctx| {
            let caption = th.font(Role::RibbonLabel);
            let title = th.font(Role::GroupTitle);
            let tw = |s: &str, t: bool| {
                ctx.fonts(|fo| {
                    fo.layout_no_wrap(
                        s.to_owned(),
                        if t { title.clone() } else { caption.clone() },
                        Color32::PLACEHOLDER,
                    )
                    .size()
                    .x
                })
            };
            let mut tabs = reg.tabs();
            tabs.push(CTX_TAB.into());
            for tab in &tabs {
                let r = Ribbon {
                    tab: tab.clone(),
                    context: Some(("wall".into(), "Walls".into())),
                    ..Default::default()
                };
                let cmds = r.tab_commands(&reg);
                let (draws, ins) = build_groups(&cmds, &reg, &cat, &r.split, &tw, &m);
                let basis = basis_groups(&cmds, &reg, &cat, &r.split, &tw, &m);
                for w in [1024.0, 1280.0, 1440.0, 1600.0, 1920.0, 2560.0] {
                    // the band spans the window inside the panel frame and the edge insets
                    let avail = w - 12.0 - 2.0 * k::EDGE;
                    let lay = rl::layout_by(&basis, &ins, avail, Stage::Full, &m);
                    f(tab, w, &draws, &ins, &lay);
                }
            }
        });
    }

    /// Audit 21: the level of every group comes from the window width and the command
    /// priorities, so English and Russian draw each group at the same stage.
    #[test]
    fn both_languages_draw_every_group_at_the_same_stage() {
        let mut seen: HashMap<(String, u32), Vec<Vec<Stage>>> = HashMap::new();
        for lang in [Lang::En, Lang::Ru] {
            each_layout(lang, &mut |tab, w, _, _, lay| {
                let stages = lay.groups.iter().map(|g| g.stage).collect();
                seen.entry((tab.to_string(), w as u32)).or_default().push(stages);
            });
        }
        assert!(seen.len() >= 40, "{} tab widths", seen.len());
        for ((tab, w), v) in &seen {
            assert_eq!(v.len(), 2, "{tab} @{w}");
            assert_eq!(v[0], v[1], "{tab} @{w}: en vs ru stages");
        }
    }

    /// Audit 20: a narrowing window never strips captions; a group folds into its labelled
    /// drop-down instead. Only the icons in [`ICON_ONLY`] may go bare.
    #[test]
    fn no_command_loses_its_caption() {
        let reg = Registry::builtin();
        for c in reg.all().iter().filter(|c| c.on("R")) {
            if c.size == Some(Size::Small) {
                assert!(ICON_ONLY.contains(&c.id.as_str()), "{} is icon-only", c.id);
            }
        }
        for lang in [Lang::En, Lang::Ru] {
            each_layout(lang, &mut |tab, w, draws, _, lay| {
                for (go, gd) in lay.groups.iter().zip(draws) {
                    assert_ne!(go.stage, Stage::IconOnly, "{lang:?} {tab} @{w}");
                    for p in go.items.iter().filter(|p| p.size == Size::Small) {
                        let id = gd.entries[p.item].cmd.id.as_str();
                        assert!(ICON_ONLY.contains(&id), "{lang:?} {tab} @{w}: {id} bare");
                    }
                }
            });
        }
    }

    #[test]
    fn layout_fits_without_overlap_or_clipping() {
        for lang in [Lang::En, Lang::Ru] {
            each_layout(lang, &mut |tab, w, draws, ins, lay| {
                let m = rl::Metrics::default();
                let avail = w - 12.0 - 2.0 * k::EDGE;
                let tag = format!("{lang:?} {tab} @{w}");
                assert!(lay.width <= avail + 0.01, "{tag}: {} > {avail}", lay.width);
                if w >= 1280.0 {
                    assert_eq!(lay.overflow_from, ins.len(), "{tag}: overflow menu");
                }
                let mut rects: Vec<Rect> = vec![];
                for (go, (gi, gd)) in lay.groups.iter().zip(ins.iter().zip(draws)) {
                    let frame = Rect::from_min_size(pos2(go.x, 0.0), vec2(go.w, m.band_h));
                    let title_need = gi.title_w + 2.0 * m.title_pad_x
                        + if gi.launcher { 2.0 * m.launcher_w } else { 0.0 };
                    // a collapsed group wraps its title over two lines inside the drop-down
                    let title_need = if go.collapsed_rect.is_some() {
                        gi.collapsed_w + 2.0 * m.large_pad_x
                    } else {
                        title_need
                    };
                    assert!(title_need <= go.w + 0.5, "{tag}: title of {}", gd.title);
                    for p in &go.items {
                        assert!(frame.contains_rect(p.rect), "{tag}: {:?} outside", p.rect);
                        let it = &gi.items[p.item];
                        let e = &gd.entries[p.item];
                        let need = rl::button_w(it, p.size, &m);
                        assert!(need <= p.rect.width() + 0.01, "{tag}: {} clipped", e.label);
                        if p.size == Size::Large {
                            assert!(e.wrap.lines() <= 2, "{tag}: {}", e.label);
                            assert!(
                                e.wrap.w + 2.0 * m.large_pad_x <= p.rect.width() + 0.01,
                                "{tag}: {} wider than its button",
                                e.label
                            );
                        }
                        // whole points: edges on pixels at 1x and 2x
                        assert_eq!(p.rect.min.x.fract(), 0.0, "{tag}: {}", e.label);
                        assert_eq!(p.rect.width().fract(), 0.0, "{tag}: {}", e.label);
                        rects.push(p.rect);
                    }
                    rects.extend(go.collapsed_rect);
                }
                for (i, a) in rects.iter().enumerate() {
                    for b in &rects[i + 1..] {
                        let o = a.intersect(*b);
                        assert!(
                            o.width() <= 0.0 || o.height() <= 0.0,
                            "{tag}: {a:?} overlaps {b:?}"
                        );
                    }
                }
            });
        }
    }

    #[test]
    fn structure_groups_keep_footings_together_and_no_lone_button() {
        let reg = Registry::builtin();
        let on_struct: Vec<&Command> = reg
            .all()
            .iter()
            .filter(|c| c.tab.as_deref() == Some("struct") && c.on("R"))
            .collect();
        let group_of_id = |id: &str| {
            let c = on_struct.iter().find(|c| c.id == id).unwrap_or_else(|| panic!("{id}"));
            group_name(c).to_string()
        };
        let found = group_of_id("str.footing");
        for id in ["str.strip_footing", "str.raft", "str.footing_all"] {
            assert_eq!(group_of_id(id), found, "{id}");
        }
        let ids: Vec<&str> = on_struct
            .iter()
            .filter(|c| group_name(c) == found)
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["str.footing", "str.strip_footing", "str.raft", "str.footing_all"]
        );
        assert_eq!(group_of_id("str.slab_opening"), group_of_id("str.slab"));
        let rebar = group_of_id("str.rebar");
        for id in ["str.rebar_schedule", "str.rebar_toggle", "str.rebar_delete", "str.analytical"] {
            assert_eq!(group_of_id(id), rebar, "{id}");
        }
        let mut count: std::collections::BTreeMap<String, usize> = Default::default();
        for c in &on_struct {
            *count.entry(group_name(c).to_string()).or_default() += 1;
        }
        for (g, n) in &count {
            assert!(*n >= 2, "struct group {g} holds a single command");
        }
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for g in count.keys() {
                let key = format!("rgroup.{g}");
                assert_ne!(cat.t(&key), key, "{lang:?}: {key}");
            }
        }
    }

    #[test]
    fn structure_tab_labels_every_button_at_default_width() {
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            let mut checked = 0;
            each_layout(lang, &mut |tab, w, draws, ins, lay| {
                if tab != "struct" || w < 1600.0 {
                    return;
                }
                assert_eq!(lay.overflow_from, ins.len(), "{lang:?} @{w}");
                for (go, gd) in lay.groups.iter().zip(draws) {
                    assert_eq!(go.stage, Stage::Full, "{lang:?} @{w}: {}", gd.title);
                    for p in &go.items {
                        let e = &gd.entries[p.item];
                        assert_ne!(p.size, Size::Small, "{lang:?} @{w}: {} has no label", e.label);
                        assert!(!e.label.trim().is_empty(), "{lang:?} @{w}: {}", e.cmd.id);
                        // the tooltip title is the full label whatever the button shows
                        let full = cat.t(&e.show.label_key());
                        assert_eq!(tip_text(&full, "", "", &[], None).title, full);
                        checked += 1;
                    }
                }
            });
            assert!(checked >= 10, "{lang:?}: {checked} buttons checked");
        }
    }

    #[test]
    fn narrow_windows_drop_small_button_labels_before_large_shrink() {
        for lang in [Lang::En, Lang::Ru] {
            each_layout(lang, &mut |tab, w, draws, _ins, lay| {
                for (go, gd) in lay.groups.iter().zip(draws) {
                    let tag = format!("{lang:?} {tab} @{w}: {}", gd.title);
                    for p in &go.items {
                        let e = &gd.entries[p.item];
                        // a small-sized command shows a label only at the widest stage
                        if e.size == Size::Small && p.size != Size::Small {
                            assert_eq!(go.stage, Stage::Full, "{tag}: {}", e.label);
                        }
                        // a large button never shrinks while a label is still being shown on a
                        // small-sized command of its own group
                        if go.stage > Stage::Bare {
                            assert!(
                                go.items.iter().all(|q| gd.entries[q.item].size != Size::Small
                                    || q.size == Size::Small),
                                "{tag}"
                            );
                        }
                    }
                }
            });
        }
    }

    #[test]
    fn main_tabs_have_no_nameless_command_from_1440() {
        for lang in [Lang::En, Lang::Ru] {
            let mut checked = 0;
            each_layout(lang, &mut |tab, w, draws, ins, lay| {
                if w < 1440.0 || !matches!(tab, "arch" | "struct" | "systems") {
                    return;
                }
                let tag = format!("{lang:?} {tab} @{w}");
                assert_eq!(lay.overflow_from, ins.len(), "{tag}: overflow menu");
                for (go, gd) in lay.groups.iter().zip(draws) {
                    // a collapsed group is one drop-down that carries the group title
                    if go.collapsed_rect.is_some() {
                        assert!(!gd.title.trim().is_empty(), "{tag}: untitled drop-down");
                        continue;
                    }
                    for p in &go.items {
                        let e = &gd.entries[p.item];
                        assert_ne!(p.size, Size::Small, "{tag}: {} has no label", e.label);
                        checked += 1;
                    }
                }
                // collapsing keeps every command reachable: nothing is dropped
                let shown: usize = lay.groups.iter().map(|g| g.items.len()).sum();
                let held: usize = lay
                    .groups
                    .iter()
                    .zip(draws)
                    .filter(|(g, _)| g.collapsed_rect.is_some())
                    .map(|(_, d)| d.entries.len())
                    .sum();
                let all: usize = draws.iter().map(|d| d.entries.len()).sum();
                assert_eq!(shown + held, all, "{tag}");
            });
            assert!(checked >= 30, "{lang:?}: {checked} buttons checked");
        }
    }

    #[test]
    fn low_use_groups_are_full_or_one_labelled_drop_down() {
        let m = rl::Metrics::default();
        for lang in [Lang::En, Lang::Ru] {
            let mut seen_low = 0;
            each_layout(lang, &mut |tab, w, draws, ins, lay| {
                let tag = format!("{lang:?} {tab} @{w}");
                let others_lost_a_label = lay
                    .groups
                    .iter()
                    .zip(ins)
                    .any(|(g, i)| {
                        !i.low_use && i.priority() < rl::LOW_USE_PRIORITY && g.stage >= Stage::Bare
                    });
                for (i, (g, gi)) in lay.groups.iter().zip(ins).enumerate() {
                    if !gi.low_use {
                        continue;
                    }
                    seen_low += 1;
                    assert!(
                        matches!(g.stage, Stage::Full | Stage::Collapsed),
                        "{tag}: {} at {:?}",
                        draws[i].title,
                        g.stage
                    );
                    // no other group loses a label while a low-use group could still collapse
                    let useful = rl::group_at(gi, Stage::Collapsed, &m).0
                        < rl::group_at(gi, Stage::Full, &m).0;
                    if others_lost_a_label && useful {
                        assert_eq!(g.stage, Stage::Collapsed, "{tag}: {}", draws[i].title);
                    }
                }
            });
            assert!(seen_low >= 6, "{lang:?}: {seen_low} low-use group views");
        }
    }

    #[test]
    fn site_and_generic_model_are_low_use_and_collapse_first() {
        let reg = Registry::builtin();
        let cat = Catalog::new(Lang::En);
        let r = Ribbon::default();
        let cmds = r.tab_commands(&reg);
        let m = rl::Metrics::default();
        let tw = |s: &str, _t: bool| s.chars().count() as f32 * 6.0;
        let (draws, ins) = build_groups(&cmds, &reg, &cat, &r.split, &tw, &m);
        for g in ["site", "gmodel"] {
            let i = draws
                .iter()
                .position(|d| d.entries.iter().any(|e| group_name(e.cmd) == g))
                .unwrap_or_else(|| panic!("group {g} missing on the default tab"));
            assert!(ins[i].low_use, "{g} is not low-use");
        }
        let order = rl::shrink_order(&ins);
        let low: Vec<usize> = (0..ins.len()).filter(|&i| ins[i].low_use).collect();
        assert!(!low.is_empty());
        // the marked groups are the first to give up width
        for (n, &i) in order.iter().take(low.len()).enumerate() {
            assert!(ins[i].low_use, "shrink order position {n} is group {i}");
        }
    }

    #[test]
    fn surface_and_lattice_tools_are_low_use_groups() {
        let reg = Registry::builtin();
        let cat = Catalog::new(Lang::En);
        let m = rl::Metrics::default();
        let tw = |s: &str, _t: bool| s.chars().count() as f32 * 6.0;
        for (tab, g, ids) in [
            (
                "mass",
                "mass_free",
                &[
                    "mass.freeform",
                    "mass.panelize",
                    "mass.surface_shell",
                    "mass.thicken",
                    "mass.timeline_family",
                ][..],
            ),
            ("struct", "str_space", &["str.truss", "str.gridshell"][..]),
        ] {
            let r = Ribbon {
                tab: tab.into(),
                ..Default::default()
            };
            let cmds = r.tab_commands(&reg);
            let (draws, ins) = build_groups(&cmds, &reg, &cat, &r.split, &tw, &m);
            let i = draws
                .iter()
                .position(|d| d.entries.iter().any(|e| group_name(e.cmd) == g))
                .unwrap_or_else(|| panic!("group {g} missing on {tab}"));
            let got: Vec<&str> = draws[i].entries.iter().map(|e| e.cmd.id.as_str()).collect();
            for id in ids {
                assert!(got.contains(id), "{tab}: {id} not in {g}: {got:?}");
            }
            assert!(ins[i].low_use, "{g} is not low-use");
            // the group gives up width before every other group of its tab
            assert_eq!(rl::shrink_order(&ins).first(), Some(&i), "{tab}");
        }
    }

    #[test]
    fn a_lone_command_does_not_repeat_its_group_title() {
        let reg = Registry::builtin();
        let tw = |s: &str, _t: bool| s.chars().count() as f32 * 6.0;
        let m = rl::Metrics::default();
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for tab in reg.tabs() {
                let r = Ribbon { tab, ..Default::default() };
                let cmds = r.tab_commands(&reg);
                let (draws, ins) = build_groups(&cmds, &reg, &cat, &r.split, &tw, &m);
                for (d, i) in draws.iter().zip(&ins) {
                    if d.title_hidden {
                        assert_eq!(d.entries.len(), 1, "{lang:?}: {}", d.title);
                        assert_eq!(i.title_w, 0.0, "{lang:?}: {}", d.title);
                    } else if d.entries.len() == 1 && d.launcher.is_none() {
                        let full = cat.t(&d.entries[0].show.label_key());
                        assert_ne!(
                            full.trim().to_lowercase(),
                            d.title.trim().to_lowercase(),
                            "{lang:?}: {}",
                            d.title
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn wide_windows_keep_the_main_tools_large() {
        each_layout(Lang::En, &mut |tab, w, draws, _ins, lay| {
            if w < 1920.0 || tab != "arch" {
                return;
            }
            // the assistant leads the tab; the building tools are the first group after it
            let gi = draws
                .iter()
                .position(|d| d.entries.iter().any(|e| e.cmd.id == "arch.wall"))
                .expect("arch.wall on the tab");
            assert!(gi <= 1, "arch.wall in group {gi}");
            let g = &lay.groups[gi];
            // stages follow the widest language: the Russian tab does not fit whole at
            // 1920, so the group may give its secondary tools up (Hero), never its main ones
            assert!(matches!(g.stage, Stage::Full | Stage::Hero), "arch @{w}: {:?}", g.stage);
            for id in ["arch.wall", "arch.door", "arch.window", "arch.floor"] {
                let p = g
                    .items
                    .iter()
                    .find(|p| draws[gi].entries[p.item].cmd.id == id)
                    .unwrap_or_else(|| panic!("{id} not in the first group"));
                assert_eq!(p.size, Size::Large, "{id} @{w}");
            }
        });
    }

    #[test]
    fn narrow_windows_shrink_the_least_important_groups_first() {
        each_layout(Lang::En, &mut |tab, w, _draws, ins, lay| {
            let hero = rl::shrink_order(ins).last().copied();
            if let Some(h) = hero.filter(|&h| h < lay.groups.len()) {
                let hs = lay.groups[h].step;
                for (i, g) in lay.groups.iter().enumerate() {
                    if ins[i].priority() > ins[h].priority() {
                        assert!(
                            g.step >= hs,
                            "{tab} @{w}: group {i} at {:?} behind the hero group at {hs:?}",
                            g.step
                        );
                    }
                    assert!(g.step >= g.stage, "{tab} @{w}");
                }
            }
        });
    }

    #[test]
    fn key_tips_unique_per_tab() {
        let reg = Registry::builtin();
        let cat = Catalog::new(Lang::En);
        let m = rl::Metrics::default();
        let tw = |s: &str, _t: bool| s.chars().count() as f32 * 6.0;
        let tabs = reg.tabs();
        let tt = rl::assign_tips(
            1,
            &tabs.iter().map(|t| (None, t.as_str())).collect::<Vec<_>>(),
        );
        let uniq: HashSet<&String> = tt.iter().collect();
        assert_eq!(uniq.len(), tt.len(), "{tt:?}");
        for tab in &tabs {
            let r = Ribbon {
                tab: tab.clone(),
                ..Default::default()
            };
            let cmds = r.tab_commands(&reg);
            let (draws, _) = build_groups(&cmds, &reg, &cat, &r.split, &tw, &m);
            let tips = command_tips(&draws);
            let uniq: HashSet<&String> = tips.iter().map(|t| &t.1).collect();
            assert_eq!(uniq.len(), tips.len(), "{tab}");
            assert!(tips.iter().all(|t| t.1.len() == 2), "{tab}: {tips:?}");
            // mnemonics stay the tips where they are unique
            if let Some(t) = tips.iter().find(|t| t.0 == "arch.wall") {
                assert_eq!(t.1, "WA");
            }
        }
    }

    #[test]
    fn split_button_runs_the_remembered_variant() {
        let reg = Registry::builtin();
        let cat = Catalog::new(Lang::En);
        let m = rl::Metrics::default();
        let tw = |s: &str, _t: bool| s.chars().count() as f32 * 6.0;
        let mut r = Ribbon::default();
        let wall = reg.get("arch.wall").unwrap();
        assert!(r.split.choose("arch.wall", "arch.curtain_wall", &wall.variants, false));
        let cmds = r.tab_commands(&reg);
        let (draws, _) = build_groups(&cmds, &reg, &cat, &r.split, &tw, &m);
        let e = draws
            .iter()
            .flat_map(|g| &g.entries)
            .find(|e| e.cmd.id == "arch.wall")
            .unwrap();
        assert_eq!(e.show.id, "arch.curtain_wall");
        // variants have no button of their own
        assert!(draws
            .iter()
            .flat_map(|g| &g.entries)
            .all(|e| e.cmd.id != "arch.curtain_wall" && e.cmd.id != "arch.wall_room"));
        let rows = r.rows_of(&cat, &reg, wall, Some("arch.curtain_wall"));
        assert_eq!(rows[0].id, "arch.wall");
        assert!(rows.iter().any(|x| x.id == "arch.curtain_wall" && x.on));
        assert!(rows.iter().all(|x| x.parent.as_ref().is_some_and(|p| p.0 == "arch.wall")));
        r.split.choose("arch.wall", "arch.wall", &wall.variants, false);
        assert_eq!(r.split.current("arch.wall", &wall.variants), "arch.wall");
    }

    #[test]
    fn a_removing_or_bulk_variant_is_never_remembered() {
        let reg = Registry::builtin();
        let mut seen = 0;
        for c in reg.all() {
            for v in c.variants.iter().filter(|v| reg.get(v).is_some_and(|d| d.once)) {
                let mut s = SplitState::default();
                assert!(s.choose(&c.id, v, &c.variants, true), "{v}");
                assert_eq!(s.current(&c.id, &c.variants), c.id, "{v} stays on the main part");
                seen += 1;
            }
        }
        // mod.unjoin, mod.group_delete, arch.roof_unjoin, str.footing_all and the like
        assert!(seen >= 4, "{seen}");
    }

    #[test]
    fn every_destructive_command_declares_once() {
        let reg = Registry::builtin();
        let words = ["delete", "remove", "unjoin", "clear", "purge", "reset", "disconnect", "_all"];
        let mut seen = 0;
        for c in reg.all() {
            if words.iter().any(|w| c.id.contains(w)) {
                assert!(c.once, "{} must set once", c.id);
                seen += 1;
            }
        }
        assert!(seen >= 10, "{seen}");
        // the field is explicit: ordinary drawing tools never carry it
        assert!(!reg.get("arch.wall").unwrap().once);
    }

    #[test]
    fn split_main_part_never_repeats_a_once_variant() {
        let reg = Registry::builtin();
        for c in reg.all().iter().filter(|c| !c.variants.is_empty()) {
            for v in &c.variants {
                let once = reg.get(v).is_some_and(|d| d.once);
                let mut s = SplitState::default();
                assert!(s.choose(&c.id, v, &c.variants, once), "{v}");
                let cur = s.current(&c.id, &c.variants);
                assert_eq!(cur == c.id, once, "{} -> {v}", c.id);
            }
        }
    }

    #[test]
    fn one_fill_per_part_and_readable_in_both_themes() {
        use crate::theme::{contrast, over_rgba};
        let states = [(false, false), (true, false), (false, true), (true, true)];
        for mode in [crate::Mode::Light, crate::Mode::Dark] {
            let th = Theme::new(mode, crate::Density::Comfortable);
            let bg = th.c("bg.panel");
            for on in [false, true] {
                for main in states {
                    for caret in states.map(Some).into_iter().chain([None]) {
                        let f = button_fills(on, main, caret);
                        for (i, fill) in f.iter().enumerate() {
                            let part = if i == 0 { Some(main) } else { caret };
                            let want = part.map_or(PartFill::None, |s| match s {
                                (_, true) => PartFill::Pressed,
                                _ if on => PartFill::Selected,
                                (true, _) => PartFill::Hover,
                                _ => PartFill::None,
                            });
                            let ctx = format!("{mode:?} on={on} {main:?} {caret:?} part {i}");
                            assert_eq!(*fill, want, "{ctx}");
                            let Some(c) = fill.color(&th) else { continue };
                            let flat = over_rgba(c, bg);
                            assert!(contrast(fill_ink(&th, *fill), flat) >= 4.5, "{ctx} ink");
                            if *fill != PartFill::Selected {
                                assert!(contrast(th.c("text.secondary"), flat) >= 4.5, "{ctx}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn moved_commands_stay_reachable_from_search() {
        let reg = Registry::builtin();
        let cat = Catalog::new(Lang::En);
        for id in ["str.slab_opening", "str.footing_all"] {
            let found = crate::fuzzy::search(&reg, &cat, id);
            assert!(found.iter().any(|c| c.id == id), "{id} not found");
        }
    }

}
