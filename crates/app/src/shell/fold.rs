//! Collapsible chrome as one system: the ribbon (full, tabs only, hidden), the options bar, the
//! status bar, the side docks, the AI tab's work column and preview, and a clean screen that
//! hides all of it but the canvas. The person's choices live in prefs; a fold for lack of room
//! and the clean screen are never written, so neither can overwrite a choice.
use super::EditorApp;
use crate::prefs::Prefs;
use ddc_ui::sem::space;
use ddc_ui::widgets;
use ddc_ui::Theme;
use egui::{self, pos2, vec2, Rect, Sense};

/// Fold and unfold time, seconds; none with reduced motion.
pub const ANIM_S: f32 = 0.14;
/// Below this window width Properties fold to a rail on their own; the setting stays.
pub const PROPS_RAIL_BELOW: f32 = 1280.0;
/// Pointer rest on a rail or on the hidden ribbon's handle that opens it.
pub const HOLD_S: f64 = 0.6;
/// Half width of the band around a splitter that takes a double-click.
pub const SPLIT_GRAB: f32 = 4.0;
/// Width of a folded side panel.
pub const RAIL_W: f32 = 40.0;
/// Height of the hidden ribbon's handle under the title bar.
pub const HANDLE_H: f32 = 10.0;
/// Widest side docks, also never more than `DOCK_MAX_SHARE` of the window.
pub const PANEL_MAX_W: f32 = 560.0;
pub const DOCK_MAX_SHARE: f32 = 0.4;

pub const CMD_RIBBON: &str = "ui.ribbon.toggle";
pub const CMD_OPTIONS: &str = "ui.bar.options";
pub const CMD_STATUS: &str = "ui.bar.status";
pub const CMD_CLEAN: &str = "ui.clean_screen";
pub const CMD_AI_WORK: &str = "ui.ai.work";
pub const CMD_AI_PREVIEW: &str = "ui.ai.preview";
pub const CMD_BROWSER: &str = "ui.panel.browser";
pub const CMD_PROPS: &str = "ui.panel.props";

/// How much of the ribbon shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RibbonMode {
    #[default]
    Full,
    /// The tab strip; a tab click opens the band over the canvas for one command.
    Tabs,
    /// A thin handle; a click or a rest on it opens the whole ribbon over the canvas.
    Hidden,
}

impl RibbonMode {
    pub fn of(p: &Prefs) -> Self {
        if p.fold.ribbon_hidden {
            Self::Hidden
        } else if p.ribbon_collapsed {
            Self::Tabs
        } else {
            Self::Full
        }
    }

    pub fn store(self, p: &mut Prefs) {
        p.fold.ribbon_hidden = self == Self::Hidden;
        p.ribbon_collapsed = self == Self::Tabs;
    }

    /// Ctrl+F1: full, tabs only, hidden, full again.
    pub fn next(self) -> Self {
        match self {
            Self::Full => Self::Tabs,
            Self::Tabs => Self::Hidden,
            Self::Hidden => Self::Full,
        }
    }

    /// Height of the docked ribbon in this mode, `body` being the band's.
    pub fn height(self, tab_h: f32, body: f32) -> f32 {
        match self {
            Self::Full => tab_h + body,
            Self::Tabs => tab_h,
            Self::Hidden => HANDLE_H,
        }
    }
}

/// Which side of an AI tab is folded to a rail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AiFold {
    #[default]
    Open,
    /// The work column is a rail; the preview takes the area.
    Work,
    /// The preview is a rail; the work column takes the area.
    Preview,
}

impl AiFold {
    pub fn key(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Work => "work",
            Self::Preview => "preview",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::Open, Self::Work, Self::Preview]
            .into_iter()
            .find(|f| f.key() == s)
    }

    /// The fold `side` toggled: folding one side unfolds the other.
    pub fn toggle(self, side: Self) -> Self {
        if self == side {
            Self::Open
        } else {
            side
        }
    }
}

/// The person's chrome choices beyond the docks and the ribbon's tabs-only state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FoldPrefs {
    pub ribbon_hidden: bool,
    pub options_bar: bool,
    pub status_bar: bool,
    pub ai: AiFold,
}

impl Default for FoldPrefs {
    fn default() -> Self {
        Self {
            ribbon_hidden: false,
            options_bar: true,
            status_bar: true,
            ai: AiFold::Open,
        }
    }
}

impl FoldPrefs {
    /// One `key=value` line of the prefs file; false when the key is not one of these.
    pub fn read(&mut self, k: &str, v: &str) -> bool {
        match k {
            "panel.ribbon_hidden" => self.ribbon_hidden = v == "1",
            "panel.options_bar" => self.options_bar = v != "0",
            "panel.status_bar" => self.status_bar = v != "0",
            "panel.ai_fold" => self.ai = AiFold::parse(v).unwrap_or_default(),
            _ => return false,
        }
        true
    }

    pub fn text(&self) -> String {
        format!(
            "panel.ribbon_hidden={}\npanel.options_bar={}\npanel.status_bar={}\npanel.ai_fold={}\n",
            u8::from(self.ribbon_hidden),
            u8::from(self.options_bar),
            u8::from(self.status_bar),
            self.ai.key()
        )
    }
}

/// What one frame draws: the person's choices after the clean screen and the window width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shown {
    pub title: bool,
    /// `None`: no ribbon at all, not even the handle (clean screen).
    pub ribbon: Option<RibbonMode>,
    pub options: bool,
    pub status: bool,
    /// Side docks and their rails; off only on the clean screen.
    pub docks: bool,
    /// Properties fold to a rail for lack of room; the person's setting is untouched.
    pub props_auto: bool,
}

pub fn shown(p: &Prefs, clean: bool, width: f32) -> Shown {
    let props_auto = width < PROPS_RAIL_BELOW;
    if clean {
        return Shown {
            title: false,
            ribbon: None,
            options: false,
            status: false,
            docks: false,
            props_auto,
        };
    }
    Shown {
        title: true,
        ribbon: Some(RibbonMode::of(p)),
        options: p.fold.options_bar,
        status: p.fold.status_bar,
        docks: true,
        props_auto,
    }
}

/// Widest a side dock may be dragged in a window `screen_w` wide.
pub fn dock_max(cap: f32, min: f32, screen_w: f32) -> f32 {
    (screen_w * DOCK_MAX_SHARE).min(cap).max(min)
}

/// A pointer resting on a target: it opens the target once it has rested `HOLD_S`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hold {
    since: Option<f64>,
    fired: bool,
}

impl Hold {
    /// Per frame: progress 0..=1 while the pointer rests, `true` once when it reaches the end.
    pub fn update(&mut self, hovered: bool, now: f64) -> (f32, bool) {
        if !hovered {
            *self = Self::default();
            return (0.0, false);
        }
        let t0 = *self.since.get_or_insert(now);
        let p = ((now - t0) / HOLD_S).clamp(0.0, 1.0) as f32;
        if p >= 1.0 && !self.fired {
            self.fired = true;
            return (1.0, true);
        }
        (if self.fired { 0.0 } else { p }, false)
    }
}

/// Session state of the folds; nothing here is saved.
#[derive(Default)]
pub struct FoldUi {
    pub clean: bool,
    /// The hidden ribbon shown over the canvas until a command runs or the pointer leaves.
    pub reveal: bool,
    pub hold_ribbon: Hold,
    pub hold_browser: Hold,
    pub hold_props: Hold,
    pub hold_ai: Hold,
    /// Where the hidden ribbon's handle was drawn; the shown ribbon opens from it.
    pub handle: Option<Rect>,
    /// The window was narrower than `PROPS_RAIL_BELOW` in the last frame.
    pub was_narrow: bool,
    /// The view area of the last frame.
    pub canvas: Option<Rect>,
    /// The folds of the last frame and when they last changed (the first frame sets no time).
    sig: Option<String>,
    changed_at: f64,
}

impl FoldUi {
    /// The folds `sig` at `now`: true while a fold change can still be moving the canvas, so
    /// that views keep the model under the same pixel then (a layout switch keeps its centre).
    pub fn follow(&mut self, sig: String, now: f64) -> bool {
        match &self.sig {
            Some(s) if *s == sig => {}
            Some(_) => {
                self.sig = Some(sig);
                self.changed_at = now;
            }
            None => {
                self.sig = Some(sig);
                self.changed_at = f64::NEG_INFINITY;
            }
        }
        now - self.changed_at <= f64::from(ANIM_S) + 0.1
    }
}

/// No motion: the system asked for it, or the style has no animation time.
pub fn calm(th: &Theme, ctx: &egui::Context) -> bool {
    th.reduced_motion || ctx.style().animation_time <= 0.0
}

/// 0 folded .. 1 open, eased; jumps at once when `calm`.
pub fn anim(ctx: &egui::Context, id: egui::Id, open: bool, calm: bool) -> f32 {
    let t = ctx.animate_bool_with_time(id, open, if calm { 0.0 } else { ANIM_S });
    ease(t)
}

/// A height or width moving to `target`; jumps at once when `calm`.
pub fn anim_to(ctx: &egui::Context, id: egui::Id, target: f32, calm: bool) -> f32 {
    ctx.animate_value_with_time(id, target, if calm { 0.0 } else { ANIM_S })
}

/// Smoothstep: no jolt at either end.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A double-click of the primary button on the vertical line at `x` over `ys`, not on a popup
/// or window above it.
pub fn splitter_double_click(ctx: &egui::Context, x: f32, ys: egui::Rangef) -> bool {
    let hit = ctx.input(|i| {
        i.pointer
            .button_double_clicked(egui::PointerButton::Primary)
            .then(|| i.pointer.interact_pos())
            .flatten()
    });
    hit.is_some_and(|p| {
        (p.x - x).abs() <= SPLIT_GRAB
            && ys.contains(p.y)
            && ctx.layer_id_at(p).is_none_or(|l| l.order == egui::Order::Background)
    })
}

/// Which way a rail's title reads and where its hold bar sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
}

/// What a rail asks for this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RailOut {
    pub clicked: bool,
    pub held: bool,
}

/// Text of a rail: its title and the button's tooltip with the key that opens it.
pub struct RailText<'a> {
    pub icon: &'a str,
    pub title: &'a str,
    pub tip: &'a str,
    pub keys: &'a str,
}

/// A folded panel drawn in `rect`: one button the whole rail tall with the panel's icon at
/// the top (the title is in the tooltip); a rest of `HOLD_S` opens it like a click.
#[allow(clippy::too_many_arguments)]
pub fn rail(
    ui: &mut egui::Ui,
    rect: Rect,
    id: egui::Id,
    th: &Theme,
    icons: &mut ddc_ui::icons::IconCache,
    text: &RailText<'_>,
    edge: Edge,
    hold: &mut Hold,
) -> RailOut {
    let resp = ui.interact(rect, id, Sense::click());
    // icon-only rail: the title is the accessible name, the tip names the key
    let label = text.title.to_string();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    let now = ui.input(|i| i.time);
    let (progress, held) = hold.update(resp.hovered() && !resp.is_pointer_button_down_on(), now);
    if progress > 0.0 {
        ui.ctx().request_repaint();
    }
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, th.c("bg.panel"));
    let hot = resp.hovered() || resp.has_focus();
    if hot {
        p.rect_filled(rect.shrink(2.0), th.radius("md"), th.hover_fill());
    }
    let ink = if hot { th.c("text.primary") } else { th.c("text.secondary") };
    let d = th.d();
    let ic_at = pos2(rect.center().x, rect.top() + 12.0 + d.icon / 2.0);
    let ic = Rect::from_center_size(ic_at, vec2(d.icon, d.icon));
    icons.paint(ui, text.icon, th.icon_px(), ic, ink);
    if progress > 0.0 {
        let h = rect.height() * progress;
        let bx = match edge {
            Edge::Left => rect.right() - 2.0,
            Edge::Right => rect.left(),
        };
        let bar = Rect::from_min_size(pos2(bx, rect.top()), vec2(2.0, h));
        p.rect_filled(bar, 0.0, th.c("accent"));
    }
    let resp = widgets::rich_tooltip(resp, th, text.tip, "", text.keys);
    RailOut {
        clicked: resp.clicked(),
        held,
    }
}

/// The chevron that folds a panel: a real button with a tooltip naming its key.
pub fn fold_button(
    ui: &mut egui::Ui,
    th: &Theme,
    left: bool,
    title: &str,
    keys: &str,
) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(egui::Vec2::splat(24.0), Sense::click());
    let label = title.to_string();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    let fg = widgets::paint_state(ui, th, r, &resp, false);
    let c = r.center();
    let s = if left { 2.0 } else { -2.0 };
    ui.painter().add(egui::Shape::line(
        vec![c + vec2(s, -4.0), c + vec2(-s, 0.0), c + vec2(s, 4.0)],
        egui::Stroke::new(1.5, fg),
    ));
    widgets::rich_tooltip(resp, th, title, "", keys)
}

impl EditorApp {
    /// The clean screen is on: only the canvas is drawn.
    pub fn chrome_clean(&self) -> bool {
        self.chrome.fold.clean
    }

    /// Key hint of a command for a tooltip, as the person bound it.
    pub(crate) fn fold_keys(&self, id: &str) -> String {
        crate::quick::keys_text(self, id)
    }

    /// Leaves the clean screen; true when it was on, so the caller shows what was asked for
    /// instead of toggling it off.
    pub(super) fn wake(&mut self) -> bool {
        std::mem::take(&mut self.chrome.fold.clean)
    }

    /// The ribbon goes to `m` and is saved.
    pub fn set_ribbon_mode(&mut self, m: RibbonMode) {
        m.store(&mut self.prefs);
        self.sh.ribbon.collapsed = self.prefs.ribbon_collapsed;
        self.sh.ribbon.peek = false;
        self.chrome.fold.reveal = false;
        self.prefs.save();
    }

    /// The hidden ribbon's handle: a button the panel's width; a click or a rest shows the
    /// ribbon over the canvas.
    pub(super) fn ribbon_handle(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect().expand2(vec2(6.0, 0.0)).intersect(ui.ctx().screen_rect());
        self.chrome.fold.handle = Some(rect);
        let tip = self.sh.cat.t("layout_fold.ribbon.show");
        let keys = self.fold_keys(CMD_RIBBON);
        let th = &self.sh.theme;
        let resp = ui.interact(rect, egui::Id::new("ddc.ribbon.handle"), Sense::click());
        let label = tip.clone();
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
        let now = ui.input(|i| i.time);
        let rest = resp.hovered() && !self.chrome.fold.reveal;
        let (progress, held) = self.chrome.fold.hold_ribbon.update(rest, now);
        if progress > 0.0 {
            ui.ctx().request_repaint();
        }
        let p = ui.painter_at(rect);
        let hot = resp.hovered() || resp.has_focus();
        let ink = if hot { th.c("accent") } else { th.c("border.subtle") };
        let grip = Rect::from_center_size(rect.center(), vec2(48.0, 3.0));
        p.rect_filled(grip, 1.5, ink);
        if progress > 0.0 {
            let w = rect.width() * progress;
            let bar = Rect::from_min_size(pos2(rect.left(), rect.bottom() - 2.0), vec2(w, 2.0));
            p.rect_filled(bar, 0.0, th.c("accent"));
        }
        let resp = widgets::rich_tooltip(resp, th, &tip, "", &keys);
        if resp.clicked() || held {
            self.chrome.fold.reveal = !self.chrome.fold.reveal || held;
        }
    }

    /// The whole ribbon over the canvas under its handle, until a command runs, the caret or
    /// the open tab folds it, the pointer presses elsewhere, or Esc.
    pub(super) fn ribbon_reveal(&mut self, ctx: &egui::Context, fire: &mut Vec<String>) {
        let Some(at) = self.chrome.fold.handle else {
            self.chrome.fold.reveal = false;
            return;
        };
        let was = self.sh.ribbon.collapsed;
        self.sh.ribbon.collapsed = false;
        self.sh.ribbon.peek = false;
        let act = self.active_command();
        let wc = self.sh.theme.ribbon_class(ctx.screen_rect().width());
        let h = widgets::ribbon::TAB_H + self.sh.ribbon.body_height(wc);
        let s = &mut self.sh;
        let th = &s.theme;
        let frame = egui::Frame::new()
            .fill(th.c("bg.panel"))
            .shadow(th.shadow(2))
            .inner_margin(egui::Margin {
                left: space::SM as i8,
                right: space::SM as i8,
                ..egui::Margin::ZERO
            });
        let shown = egui::Area::new(egui::Id::new("ddc.ribbon.reveal"))
            .order(egui::Order::Foreground)
            .fixed_pos(at.left_top())
            .constrain(false)
            .show(ctx, |ui| {
                frame
                    .show(ui, |ui| {
                        ui.set_width(at.width() - 12.0);
                        ui.set_height(h);
                        s.ribbon.show(ui, th, &s.cat, &s.reg, &mut s.icons, act)
                    })
                    .inner
            });
        let folded = self.sh.ribbon.collapsed;
        self.sh.ribbon.collapsed = was;
        let r = shown.response.rect;
        let menu = egui::Popup::is_any_open(ctx);
        let away = ctx.input(|i| {
            i.pointer.any_pressed()
                && i
                    .pointer
                    .interact_pos()
                    .is_some_and(|p| !r.contains(p) && !at.contains(p))
        });
        let esc = !menu
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let ran = shown.inner.is_some();
        if let Some(id) = shown.inner {
            fire.push(id);
        }
        if ran || folded || (away && !menu) || esc {
            self.chrome.fold.reveal = false;
        }
    }

    /// Esc on the clean screen brings the chrome back, unless a tool, a field, a menu or a
    /// window has the key.
    pub(super) fn clean_screen_esc(&mut self, ctx: &egui::Context) {
        if !self.chrome.fold.clean
            || ctx.wants_keyboard_input()
            || !crate::ui_kit::canvas_owns_keys(ctx)
            || egui::Popup::is_any_open(ctx)
            || !self.seq_ready()
            || !self.ed.selection.is_empty()
        {
            return;
        }
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            self.chrome.fold.clean = false;
        }
    }

    /// On the clean screen: one quiet button at the canvas' top right that brings the chrome
    /// back, so a mouse alone can leave it.
    pub(super) fn clean_screen_exit(&mut self, ctx: &egui::Context, area: Rect) {
        if !self.chrome.fold.clean {
            return;
        }
        let title = self.sh.cat.t("layout_fold.clean.exit");
        let keys = self.fold_keys(CMD_CLEAN);
        let th = &self.sh.theme;
        let mut leave = false;
        let at = pos2(area.right() - 12.0, area.top() + 12.0);
        egui::Area::new(egui::Id::new("ddc.fold.clean_exit"))
            .order(ddc_ui::z::Z::Canvas.order())
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(at)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(th.c("bg.panel").gamma_multiply(0.92))
                    .corner_radius(th.radius("md"))
                    .stroke(egui::Stroke::new(1.0, th.c("border.subtle")))
                    .inner_margin(egui::Margin {
                        left: space::XS as i8,
                        right: space::XS as i8,
                        top: space::XS as i8,
                        bottom: space::XS as i8,
                    })
                    .show(ui, |ui| {
                        let r = widgets::Button::secondary(&title).small().show(ui, th);
                        leave = widgets::rich_tooltip(r, th, &title, "", &keys).clicked();
                    });
            });
        if leave {
            self.chrome.fold.clean = false;
        }
    }
}

fn h_ribbon(app: &mut EditorApp) {
    let m = if app.wake() {
        RibbonMode::of(&app.prefs)
    } else {
        RibbonMode::of(&app.prefs).next()
    };
    app.set_ribbon_mode(m);
}

fn h_options(app: &mut EditorApp) {
    let f = &mut app.prefs.fold;
    f.options_bar = !f.options_bar;
    if app.wake() {
        app.prefs.fold.options_bar = true;
    }
    app.prefs.save();
}

fn h_status(app: &mut EditorApp) {
    let f = &mut app.prefs.fold;
    f.status_bar = !f.status_bar;
    if app.wake() {
        app.prefs.fold.status_bar = true;
    }
    app.prefs.save();
}

fn h_clean(app: &mut EditorApp) {
    let f = &mut app.chrome.fold;
    f.clean = !f.clean;
    f.reveal = false;
    app.sh.ribbon.peek = false;
}

fn ai_fold(app: &mut EditorApp, side: AiFold) {
    let woke = app.wake();
    let f = &mut app.prefs.fold.ai;
    *f = if woke && *f == side { *f } else { f.toggle(side) };
    app.prefs.save();
}

fn h_ai_work(app: &mut EditorApp) {
    ai_fold(app, AiFold::Work);
}

fn h_ai_preview(app: &mut EditorApp) {
    ai_fold(app, AiFold::Preview);
}

pub fn register(app: &mut EditorApp) {
    app.register_command(CMD_RIBBON, h_ribbon);
    app.register_command(CMD_OPTIONS, h_options);
    app.register_command(CMD_STATUS, h_status);
    app.register_command(CMD_CLEAN, h_clean);
    app.register_command(CMD_AI_WORK, h_ai_work);
    app.register_command(CMD_AI_PREVIEW, h_ai_preview);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ribbon_cycles_full_tabs_hidden_and_round_trips() {
        let mut p = Prefs::default();
        assert_eq!(RibbonMode::of(&p), RibbonMode::Full);
        let mut seen = vec![];
        for _ in 0..3 {
            let m = RibbonMode::of(&p).next();
            m.store(&mut p);
            let back = Prefs::parse(&p.text());
            assert_eq!(RibbonMode::of(&back), m, "saved and read back");
            seen.push(m);
        }
        assert_eq!(seen, [RibbonMode::Tabs, RibbonMode::Hidden, RibbonMode::Full]);
    }

    #[test]
    fn older_prefs_keep_their_collapsed_ribbon() {
        let p = Prefs::parse("ribbon_collapsed=1\n");
        assert_eq!(RibbonMode::of(&p), RibbonMode::Tabs);
        let p = Prefs::parse("lang=en\n");
        assert_eq!(p.fold, FoldPrefs::default());
    }

    #[test]
    fn fold_prefs_round_trip_every_value() {
        for ai in [AiFold::Open, AiFold::Work, AiFold::Preview] {
            for bits in 0..8u8 {
                let p = Prefs {
                    fold: FoldPrefs {
                        ribbon_hidden: bits & 1 != 0,
                        options_bar: bits & 2 != 0,
                        status_bar: bits & 4 != 0,
                        ai,
                    },
                    browser_open: bits & 1 == 0,
                    props_open: bits & 2 == 0,
                    ..Prefs::default()
                };
                let back = Prefs::parse(&p.text());
                assert_eq!(back.fold, p.fold);
                assert_eq!((back.browser_open, back.props_open), (p.browser_open, p.props_open));
            }
        }
        let mut f = FoldPrefs::default();
        assert!(f.read("panel.ai_fold", "nonsense"));
        assert_eq!(f.ai, AiFold::Open);
        assert!(!f.read("panel.other", "1"));
    }

    #[test]
    fn ai_fold_toggles_one_side_at_a_time() {
        let f = AiFold::Open.toggle(AiFold::Work);
        assert_eq!(f, AiFold::Work);
        assert_eq!(f.toggle(AiFold::Preview), AiFold::Preview);
        assert_eq!(AiFold::Preview.toggle(AiFold::Preview), AiFold::Open);
    }

    #[test]
    fn clean_screen_hides_everything_and_keeps_the_choices() {
        let mut p = Prefs::default();
        p.fold.options_bar = false;
        let s = shown(&p, true, 1920.0);
        let all = (s.title, s.ribbon, s.options, s.status, s.docks);
        assert_eq!(all, (false, None, false, false, false));
        let s = shown(&p, false, 1920.0);
        assert_eq!(s.ribbon, Some(RibbonMode::Full));
        assert!(s.title && !s.options && s.status && s.docks && !s.props_auto);
        assert!(!p.fold.options_bar, "the clean screen wrote nothing");
    }

    #[test]
    fn narrow_window_folds_properties_without_touching_the_setting() {
        let p = Prefs::default();
        assert!(shown(&p, false, 1279.0).props_auto);
        assert!(!shown(&p, false, 1280.0).props_auto);
        assert!(p.props_open);
    }

    #[test]
    fn hold_fires_once_after_the_rest() {
        let mut h = Hold::default();
        assert_eq!(h.update(true, 10.0), (0.0, false));
        let (p, f) = h.update(true, 10.3);
        assert!((p - 0.5).abs() < 1e-3 && !f);
        assert_eq!(h.update(true, 10.6), (1.0, true));
        assert_eq!(h.update(true, 11.5), (0.0, false), "once per rest");
        assert_eq!(h.update(false, 11.6), (0.0, false));
        let (p, f) = h.update(true, 12.0);
        assert!(p == 0.0 && !f, "a new rest starts over");
    }

    #[test]
    fn dock_width_has_a_floor_and_a_ceiling() {
        assert_eq!(dock_max(PANEL_MAX_W, 200.0, 1920.0), 560.0);
        assert_eq!(dock_max(PANEL_MAX_W, 200.0, 1000.0), 400.0);
        assert_eq!(dock_max(PANEL_MAX_W, 200.0, 300.0), 200.0);
    }

    #[test]
    fn ease_is_monotone_and_ends_exactly() {
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        let mut last = 0.0;
        for i in 1..=20 {
            let v = ease(i as f32 / 20.0);
            assert!(v >= last);
            last = v;
        }
    }

    #[test]
    fn ribbon_heights_by_mode() {
        assert_eq!(RibbonMode::Full.height(30.0, 90.0), 120.0);
        assert_eq!(RibbonMode::Tabs.height(30.0, 90.0), 30.0);
        assert_eq!(RibbonMode::Hidden.height(30.0, 90.0), HANDLE_H);
    }
}
