//! Collapsible side docks: a thin rail when folded, by the user or for lack of room; widths
//! and folded state live in prefs. A click on an automatic rail opens the panel just for now.
use super::{fold, EditorApp};
use crate::ui_kit::{self, Tokens};
use egui::{self, panel::Side};

pub const RAIL_W: f32 = fold::RAIL_W;
pub const BROWSER_W: f32 = 320.0;
pub const BROWSER_MIN_W: f32 = 240.0;
pub const PANEL_HEAD_H: f32 = 40.0;

/// A side panel's header: a fixed 40 px row so both docks share one baseline.
pub fn head<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let size = egui::vec2(ui.available_width(), PANEL_HEAD_H);
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), add)
        .inner
}

#[derive(Default)]
pub struct DockUi {
    peek_browser: bool,
    peek_props: bool,
    /// Drawn expanded in the last frame (not folded, not on a rail, not unplaced).
    shown_browser: bool,
    shown_props: bool,
    /// Folded to a rail for lack of room in the last frame; a new fold drops a peek.
    auto_browser: bool,
    auto_props: bool,
    pub browser_drag: DragSize,
    pub props_drag: DragSize,
    pub table_drag: DragSize,
}

/// A panel size worth saving is one the user dragged: a small window or a scale change also
/// squeezes panels, and that must not overwrite the saved size.
#[derive(Default)]
pub struct DragSize {
    last: f32,
    moved: bool,
}

impl DockUi {
    /// Panes drawn expanded since the last call; read once per frame by the F6 cycle.
    pub fn take_shown(&mut self) -> (bool, bool) {
        (
            std::mem::take(&mut self.shown_browser),
            std::mem::take(&mut self.shown_props),
        )
    }

    /// Rails opened over their auto fold: (browser, properties).
    pub fn peeks(&self) -> (bool, bool) {
        (self.peek_browser, self.peek_props)
    }
}

impl DragSize {
    /// Per frame. `Some` once, on release, after the size changed under a held pointer.
    pub fn update(&mut self, size: f32, held: bool) -> Option<f32> {
        let changed = self.last > 0.0 && (size - self.last).abs() > 0.5;
        self.last = size;
        if held {
            self.moved |= changed;
            return None;
        }
        std::mem::take(&mut self.moved).then_some(size)
    }
}

/// The panel is drawn in full: not folded by the user, and either has room or was asked for.
pub fn expanded(user_open: bool, auto_rail: bool, peek: bool) -> bool {
    user_open && (!auto_rail || peek)
}

/// One side dock as the rail and the motion see it.
struct Dock<'a> {
    rail_id: &'static str,
    side: Side,
    icon: &'static str,
    title: &'a str,
    tip: &'a str,
    keys: &'a str,
}

/// The folded dock: a rail with its icon and title that opens it on a click or a rest.
fn rail(ctx: &egui::Context, app: &mut EditorApp, d: &Dock<'_>, hold: Hold) -> bool {
    let s = &mut app.sh;
    let th = &s.theme;
    let icons = &mut s.icons;
    let mut out = fold::RailOut::default();
    let edge = match d.side {
        Side::Left => fold::Edge::Left,
        Side::Right => fold::Edge::Right,
    };
    let holds = &mut app.chrome.fold;
    let h = match hold {
        Hold::Browser => &mut holds.hold_browser,
        Hold::Props => &mut holds.hold_props,
    };
    egui::SidePanel::new(d.side, egui::Id::new(d.rail_id))
        .resizable(false)
        .exact_width(RAIL_W)
        .frame(egui::Frame::new().fill(th.c("bg.panel")))
        .show(ctx, |ui| {
            let text = fold::RailText {
                icon: d.icon,
                title: d.title,
                tip: d.tip,
                keys: d.keys,
            };
            let id = egui::Id::new(d.rail_id).with("button");
            out = fold::rail(ui, ui.max_rect(), id, th, icons, &text, edge, h);
        });
    out.clicked || out.held
}

#[derive(Clone, Copy)]
enum Hold {
    Browser,
    Props,
}

/// While a dock folds or unfolds: an empty panel moving between the rail and its width.
fn slide(ctx: &egui::Context, app: &EditorApp, id: &str, side: Side, w: f32, k: f32) {
    let w = RAIL_W + (w - RAIL_W) * k;
    egui::SidePanel::new(side, egui::Id::new(id).with("slide"))
        .resizable(false)
        .exact_width(w)
        .frame(egui::Frame::new().fill(app.sh.theme.c("bg.panel")))
        .show(ctx, |_| {});
}

/// The inner edge of the dock drawn as `id`: a double-click there folds it.
fn edge_double_clicked(ctx: &egui::Context, id: &str, side: Side) -> bool {
    let Some(r) = egui::panel::PanelState::load(ctx, egui::Id::new(id)).map(|s| s.rect) else {
        return false;
    };
    let x = match side {
        Side::Left => r.right(),
        Side::Right => r.left(),
    };
    fold::splitter_double_click(ctx, x, r.y_range())
}

impl EditorApp {
    /// How open the dock `id` is drawn: 0 a rail, 1 full, between while it moves.
    fn dock_motion(&self, ctx: &egui::Context, id: &str, open: bool) -> f32 {
        let calm = fold::calm(&self.sh.theme, ctx);
        fold::anim(ctx, egui::Id::new(id).with("fold"), open, calm)
    }

    pub(super) fn dock_browser(&mut self, ctx: &egui::Context, auto_rail: bool) {
        if auto_rail && !self.chrome.dock.auto_browser {
            self.chrome.dock.peek_browser = false;
        }
        self.chrome.dock.auto_browser = auto_rail;
        let open = self.prefs.browser_open;
        let full = expanded(open, auto_rail, self.chrome.dock.peek_browser);
        let k = self.dock_motion(ctx, "ddc-browser", full);
        if full && k >= 1.0 {
            self.chrome.dock.shown_browser = true;
            self.browser(ctx);
            if edge_double_clicked(ctx, "ddc-browser", Side::Left) {
                self.fold_browser();
            }
            return;
        }
        if k > 0.0 {
            slide(ctx, self, "ddc-browser", Side::Left, self.prefs.browser_width, k);
            return;
        }
        let (title, tip) = (self.sh.cat.t("browser.title"), self.sh.cat.t("dock.show_browser"));
        let keys = self.fold_keys(fold::CMD_BROWSER);
        let d = Dock {
            rail_id: "ddc-rail-browser",
            side: Side::Left,
            icon: "layers",
            title: &title,
            tip: &tip,
            keys: &keys,
        };
        if rail(ctx, self, &d, Hold::Browser) {
            self.prefs.browser_open = true;
            self.chrome.dock.peek_browser = auto_rail;
            self.prefs.save();
        }
    }

    pub(super) fn fold_browser(&mut self) {
        self.prefs.browser_open = false;
        self.chrome.dock.peek_browser = false;
        self.prefs.save();
    }

    pub(super) fn dock_props(&mut self, ctx: &egui::Context, t: &Tokens, auto_rail: bool) {
        if auto_rail && !self.chrome.dock.auto_props {
            self.chrome.dock.peek_props = false;
        }
        self.chrome.dock.auto_props = auto_rail;
        let open = self.prefs.props_open;
        let full = expanded(open, auto_rail, self.chrome.dock.peek_props);
        let k = self.dock_motion(ctx, "ddc-props", full);
        if k > 0.0 && (k < 1.0 || !full) {
            slide(ctx, self, "ddc-props", Side::Right, self.prefs.props_width, k);
            return;
        }
        if full {
            self.chrome.dock.shown_props = true;
            let w = ui_kit::side_panel(ctx, t, self.prefs.props_width, |ui| {
                super::pane_focus::anchor(ui, super::pane_focus::Pane::Props);
                ui_kit::with_hint_strip(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("ddc-props.scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.props(ui, t));
                });
            });
            let held = ctx.input(|i| i.pointer.any_down());
            if let Some(w) = self.chrome.dock.props_drag.update(w, held) {
                self.prefs.props_width = w;
                self.prefs.save();
            }
            if edge_double_clicked(ctx, "ddc-props", Side::Right) {
                self.fold_props();
            }
            return;
        }
        let (title, tip) = (self.sh.cat.t("props.title"), self.sh.cat.t("dock.show_props"));
        let keys = self.fold_keys(fold::CMD_PROPS);
        let d = Dock {
            rail_id: "ddc-rail-props",
            side: Side::Right,
            icon: "params",
            title: &title,
            tip: &tip,
            keys: &keys,
        };
        if rail(ctx, self, &d, Hold::Props) {
            self.prefs.props_open = true;
            self.chrome.dock.peek_props = auto_rail;
            self.prefs.save();
        }
    }

    pub(super) fn fold_props(&mut self) {
        self.prefs.props_open = false;
        self.chrome.dock.peek_props = false;
        self.prefs.save();
    }

    /// Back to default sizes; egui's own panel state goes too, or the old width would win.
    pub(super) fn panels_reset(&mut self, ctx: &egui::Context) {
        let p = &mut self.prefs;
        p.browser_open = true;
        p.props_open = true;
        p.browser_width = BROWSER_W;
        p.props_width = ui_kit::PROPS_W;
        p.table_height = 0.0;
        p.ribbon_collapsed = false;
        p.fold = Default::default();
        p.save();
        self.sh.ribbon.collapsed = false;
        self.sh.ribbon.peek = false;
        self.chrome.fold = Default::default();
        self.chrome.dock = DockUi::default();
        ctx.memory_mut(|m| {
            for id in ["ddc-browser", "ddc-props"] {
                m.data.remove::<egui::panel::PanelState>(egui::Id::new(id));
            }
            m.data.remove::<egui::panel::PanelState>(
                ddc_ui::workspace::PanelId::DataTable.egui_id().into(),
            );
        });
    }
}

/// A dock's key: a full dock folds; a rail, folded by the person or for lack of room, opens;
/// on the clean screen the chrome comes back with this dock open.
fn toggle(open: bool, auto: bool, peek: bool, woke: bool) -> (bool, bool) {
    let full = expanded(open, auto, peek);
    if full && !woke {
        (false, false)
    } else {
        (true, true)
    }
}

fn h_browser(app: &mut EditorApp) {
    let woke = app.wake();
    let d = &mut app.chrome.dock;
    (app.prefs.browser_open, d.peek_browser) =
        toggle(app.prefs.browser_open, d.auto_browser, d.peek_browser, woke);
    app.prefs.save();
}

fn h_props(app: &mut EditorApp) {
    let woke = app.wake();
    let d = &mut app.chrome.dock;
    (app.prefs.props_open, d.peek_props) =
        toggle(app.prefs.props_open, d.auto_props, d.peek_props, woke);
    app.prefs.save();
}

fn h_reset(app: &mut EditorApp) {
    app.chrome.settings.reset_panels = true;
}

fn h_start(app: &mut EditorApp) {
    app.open_start();
}

/// Ctrl+=/ Ctrl+-: the interface scale goes through the saved setting, never around it.
fn scale_by(app: &mut EditorApp, up: bool) {
    let s = super::settings_win::step_scale(app.prefs.ui_scale, up);
    app.prefs.ui_scale = s;
    app.sh.theme.set_scale(s);
    app.prefs.save();
    let pct = format!("{:.0}", s * 100.0);
    crate::quick::toast(app, ddc_ui::notify::Level::Info, "ui.scale.set", &[("pct", &pct)]);
}

fn h_scale_up(app: &mut EditorApp) {
    scale_by(app, true);
}

fn h_scale_down(app: &mut EditorApp) {
    scale_by(app, false);
}

/// F11: the frame loop sends the viewport command when `full` differs from `full_sent`.
fn h_maximize(app: &mut EditorApp) {
    app.chrome.full = !app.chrome.full;
}

fn h_issues(app: &mut EditorApp) {
    app.warnings_open = !app.warnings_open;
}

/// Table, schedule: the data layout shows the table beside the model; `ui.layout.back` returns.
fn h_data_layout(app: &mut EditorApp) {
    app.set_layout("data");
    // the button opened a table: say that, with the same way back, not which layout it picked
    let text = app.sh.cat.t("cmd_buttons.table.opened");
    for t in &mut app.sh.toasts.live {
        if t.command.as_deref() == Some(super::chrome::LAYOUT_BACK) {
            t.text = text.clone();
        }
    }
}

pub fn register(app: &mut EditorApp) {
    app.register_command("ui.scale_up", h_scale_up);
    app.register_command("ui.scale_down", h_scale_down);
    app.register_command("view.maximize", h_maximize);
    app.register_command("panel.issues", h_issues);
    app.register_command("data.table", h_data_layout);
    app.register_command("view.schedule", h_data_layout);
    app.register_command("ui.panel.browser", h_browser);
    app.register_command("ui.panel.props", h_props);
    app.register_command("ui.panels.reset", h_reset);
    app.register_command("ui.start", h_start);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_grid_contract() {
        assert_eq!(RAIL_W, 40.0, "collapsed rail");
        assert_eq!(super::super::status_bar::BAR_H, 28.0, "single-line status bar");
        assert_eq!(PANEL_HEAD_H, 40.0);
        assert!(BROWSER_MIN_W <= BROWSER_W && BROWSER_W <= fold::PANEL_MAX_W);
        assert_eq!(fold::SPLIT_GRAB * 2.0, 8.0, "splitter hit zone");
        for v in [RAIL_W, BROWSER_W, BROWSER_MIN_W, fold::PANEL_MAX_W, PANEL_HEAD_H] {
            assert_eq!(v % 4.0, 0.0, "{v} is off the 4 px grid");
        }
    }

    #[test]
    fn only_a_drag_changes_the_saved_size() {
        let mut d = DragSize::default();
        assert_eq!(d.update(300.0, false), None);
        assert_eq!(d.update(200.0, false), None, "squeezed by the window");
        assert_eq!(d.update(200.0, true), None);
        assert_eq!(d.update(240.0, true), None);
        assert_eq!(d.update(240.0, false), Some(240.0));
        assert_eq!(d.update(240.0, false), None);
    }

    #[test]
    fn shown_flags_are_read_once() {
        let mut d = DockUi::default();
        d.shown_props = true;
        assert_eq!(d.take_shown(), (false, true));
        assert_eq!(d.take_shown(), (false, false));
    }

    #[test]
    fn panel_shows_when_open_and_has_room_or_peek() {
        assert!(expanded(true, false, false));
        assert!(!expanded(false, false, false));
        assert!(!expanded(true, true, false));
        assert!(expanded(true, true, true));
        assert!(!expanded(false, true, true));
    }

    #[test]
    fn the_key_folds_a_full_dock_and_opens_any_rail() {
        assert_eq!(toggle(true, false, false, false), (false, false), "full folds");
        assert_eq!(toggle(false, false, false, false), (true, true), "own rail opens");
        assert_eq!(toggle(true, true, false, false), (true, true), "narrow rail opens");
        assert_eq!(toggle(true, true, true, false), (false, false), "peek folds");
        assert_eq!(toggle(true, false, false, true), (true, true), "clean screen: shown");
    }
}
