//! Folded chrome drawn as the whole app, headless, in both languages (explicit catalog) at
//! 1280, 1440 and 1920 px: every surface folds by its command and by its key, the canvas takes
//! the room, a folded dock leaves its rail, nothing leaves the window or is cut; and the
//! stacking order keeps every dialog reachable from the AI tabs above menus, panels and the
//! canvas overlays.
use crate::layout_contract::{app_in, driven_ctx, pass_on, Probe};
use crate::shell::fold::{self, AiFold, RibbonMode};
use crate::shell::EditorApp;
use ddc_ui::{Lang, Theme};
use egui::{self, accesskit::Role, Key, LayerId, Modifiers, Order};

const SCREENS: [(f32, f32); 3] = [(1280.0, 800.0), (1440.0, 900.0), (1920.0, 1080.0)];
const EPS: f32 = 0.5;
/// Frames of motion: 0.14 s at the headless 60 frames a second, and a margin.
const SETTLE: usize = 14;

struct Run {
    ctx: egui::Context,
    th: Theme,
    screen: (f32, f32),
}

impl Run {
    fn new(screen: (f32, f32)) -> Self {
        let (ctx, th) = driven_ctx();
        Self { ctx, th, screen }
    }

    fn frame(&mut self, a: &mut EditorApp, events: Vec<egui::Event>) -> Probe {
        let id = egui::Id::new("ddc.fold.probe");
        pass_on(&self.ctx, &mut self.th, id, self.screen, events, &mut |c| a.ui(c, None))
    }

    fn settle(&mut self, a: &mut EditorApp) -> Probe {
        let mut p = self.frame(a, vec![]);
        for _ in 1..SETTLE {
            p = self.frame(a, vec![]);
        }
        p
    }

    fn rect(&self) -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(self.screen.0, self.screen.1))
    }
}

/// A press and its release: a key still held would make the next press a repeat.
fn key(k: Key, m: Modifiers) -> Vec<egui::Event> {
    let ev = |pressed| egui::Event::Key {
        key: k,
        physical_key: Some(k),
        pressed,
        repeat: false,
        modifiers: m,
    };
    vec![ev(true), ev(false)]
}

/// Ctrl as the window reports it off macOS: `ctrl` and `command` both set.
fn ctrl() -> Modifiers {
    Modifiers {
        command: true,
        ..Modifiers::CTRL
    }
}

fn ctrl_alt() -> Modifiers {
    Modifiers {
        alt: true,
        ..ctrl()
    }
}

fn ctrl_shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..ctrl()
    }
}

fn canvas(a: &EditorApp) -> egui::Rect {
    a.chrome.fold.canvas.expect("the view area was drawn")
}

/// Nothing leaves the window, no control label is cut, the canvas is inside the window.
fn assert_whole(tag: &str, a: &EditorApp, p: &Probe, screen: egui::Rect) {
    let mut bad = vec![];
    for c in &p.controls {
        let r = c.rect;
        if r.left() < screen.left() - EPS
            || r.right() > screen.right() + EPS
            || r.top() < screen.top() - EPS
            || r.bottom() > screen.bottom() + EPS
        {
            bad.push(format!("{:?} {:?} {r:?} leaves the window", c.role, c.label));
        }
    }
    for e in &p.elided {
        if p.controls.iter().any(|c| c.label.trim() == e.trim()) {
            bad.push(format!("control text {e:?} is cut"));
        }
    }
    let v = canvas(a);
    if !v.is_positive() || !screen.expand(EPS).contains_rect(v) {
        bad.push(format!("view area {v:?} is not inside the window"));
    }
    assert!(bad.is_empty(), "{tag}: {}", bad.join("; "));
}

/// The rail button named `tip`, if one is drawn at the window's `left` or right edge. An
/// icon-only rail is named by its panel's title; the "show" text is its tooltip.
fn rail(p: &Probe, tip: &str, left: bool, screen: egui::Rect) -> Option<egui::Rect> {
    p.controls
        .iter()
        .filter(|c| c.role == Role::Button && c.label == tip)
        .map(|c| c.rect)
        .find(|r| {
            let edge = if left {
                (r.left() - screen.left()).abs() <= EPS
            } else {
                (r.right() - screen.right()).abs() <= EPS
            };
            edge && r.width() <= fold::RAIL_W + EPS && r.height() > 3.0 * fold::RAIL_W
        })
}

/// Which docks are rails although their setting is open (the window is too narrow for them).
fn autos(a: &EditorApp, p: &Probe, sr: egui::Rect) -> (bool, bool) {
    let c = &a.sh.cat;
    let left = a.prefs.browser_open && rail(p, &c.t("browser.title"), true, sr).is_some();
    let right = a.prefs.props_open && rail(p, &c.t("props.title"), false, sr).is_some();
    (left, right)
}

fn has_button(p: &Probe, label: &str) -> bool {
    p.controls.iter().any(|c| c.role == Role::Button && c.label == label)
}

/// A fresh app with the chrome all open.
fn open_app(lang: Lang) -> EditorApp {
    let mut a = app_in(lang);
    a.set_ribbon_mode(RibbonMode::Full);
    a.prefs.browser_open = true;
    a.prefs.props_open = true;
    a.prefs.fold = Default::default();
    a
}

#[derive(Clone, Copy, Debug)]
enum Surface {
    Ribbon,
    Options,
    Status,
    Browser,
    Props,
    Clean,
}

impl Surface {
    const ALL: [Surface; 6] = [
        Self::Ribbon,
        Self::Options,
        Self::Status,
        Self::Browser,
        Self::Props,
        Self::Clean,
    ];

    fn command(self) -> &'static str {
        match self {
            Self::Ribbon => fold::CMD_RIBBON,
            Self::Options => fold::CMD_OPTIONS,
            Self::Status => fold::CMD_STATUS,
            Self::Browser => fold::CMD_BROWSER,
            Self::Props => fold::CMD_PROPS,
            Self::Clean => fold::CMD_CLEAN,
        }
    }

    fn keys(self) -> Vec<egui::Event> {
        match self {
            Self::Ribbon => key(Key::F1, ctrl()),
            Self::Options => key(Key::O, ctrl_alt()),
            Self::Status => key(Key::U, ctrl_alt()),
            Self::Browser => key(Key::B, ctrl()),
            Self::Props => key(Key::B, ctrl_alt()),
            Self::Clean => key(Key::F1, ctrl_shift()),
        }
    }

    /// The canvas gains room on this side when the surface folds.
    fn grows(self, before: egui::Rect, after: egui::Rect) -> bool {
        match self {
            Self::Ribbon | Self::Options => after.top() < before.top() - EPS,
            Self::Status => after.bottom() > before.bottom() + EPS,
            Self::Browser => after.left() < before.left() - EPS,
            Self::Props => after.right() > before.right() + EPS,
            Self::Clean => after.area() > before.area() + 1.0,
        }
    }
}

/// Each surface folds by its command and by its key at every width, in both languages, and
/// comes back the same way; the canvas grows, a dock leaves its rail, nothing is cut.
fn each_surface_folds(lang: Lang) {
    for screen in SCREENS {
        for s in Surface::ALL {
            for by_key in [false, true] {
                let mut a = open_app(lang);
                let mut r = Run::new(screen);
                let sr = r.rect();
                let tag = format!("{lang:?} {screen:?} {s:?} key={by_key}");
                let p = r.settle(&mut a);
                assert_whole(&format!("{tag} open"), &a, &p, sr);
                let before = canvas(&a);
                // a dock the window is too narrow for is a rail already: its key opens it
                let (auto_left, auto_right) = autos(&a, &p, sr);
                let toggle = |a: &mut EditorApp, r: &mut Run| {
                    if by_key {
                        r.frame(a, s.keys());
                    } else {
                        assert!(a.run(s.command()), "{tag}: not a command");
                    }
                    r.settle(a)
                };
                let p = toggle(&mut a, &mut r);
                let after = canvas(&a);
                assert_whole(&format!("{tag} folded"), &a, &p, sr);
                let opens = match s {
                    Surface::Browser => auto_left,
                    Surface::Props => auto_right,
                    _ => false,
                };
                if opens {
                    let ok = match s {
                        Surface::Browser => after.left() > before.left() + EPS,
                        _ => after.right() < before.right() - EPS,
                    };
                    assert!(ok, "{tag}: the rail did not open {before:?} {after:?}");
                } else {
                    assert!(s.grows(before, after), "{tag}: {before:?} -> {after:?}");
                }
                let c = &a.sh.cat;
                match s {
                    Surface::Browser if !opens => {
                        let tip = c.t("browser.title");
                        assert!(rail(&p, &tip, true, sr).is_some(), "{tag}: no browser rail");
                    }
                    Surface::Props if !opens => {
                        let tip = c.t("props.title");
                        assert!(rail(&p, &tip, false, sr).is_some(), "{tag}: no props rail");
                    }
                    Surface::Ribbon => {
                        // full to tabs only: the tab strip stays, the band goes
                        assert_eq!(RibbonMode::of(&a.prefs), RibbonMode::Tabs, "{tag}");
                    }
                    Surface::Clean => {
                        assert!(has_button(&p, &c.t("layout_fold.clean.exit")), "{tag}: no way back");
                        let v = after;
                        let full = (v.left(), v.right(), v.bottom());
                        assert_eq!(full, (0.0, screen.0, screen.1), "{tag}: chrome left");
                    }
                    _ => {}
                }
                // and back
                let p = if matches!(s, Surface::Ribbon) {
                    for want in [RibbonMode::Hidden, RibbonMode::Full] {
                        toggle(&mut a, &mut r);
                        assert_eq!(RibbonMode::of(&a.prefs), want, "{tag}");
                    }
                    r.settle(&mut a)
                } else {
                    toggle(&mut a, &mut r)
                };
                assert_whole(&format!("{tag} back"), &a, &p, sr);
                let back = canvas(&a);
                assert!((back.min - before.min).length() < 1.0, "{tag}: {before:?} {back:?}");
                assert!((back.max - before.max).length() < 1.0, "{tag}: {before:?} {back:?}");
            }
        }
    }
}

#[test]
fn each_surface_folds_en() {
    each_surface_folds(Lang::En);
}

#[test]
fn each_surface_folds_ru() {
    each_surface_folds(Lang::Ru);
}

/// Every combination of the ribbon's three states and the folds of the browser, properties,
/// options bar and status bar, and the clean screen, at three widths in both languages.
fn every_combination(lang: Lang) {
    for screen in SCREENS {
        let mut r = Run::new(screen);
        let sr = r.rect();
        let mut a = open_app(lang);
        // no motion: each state is measured in its first settled frame
        a.sh.theme.reduced_motion = true;
        r.frame(&mut a, vec![]);
        let p = r.frame(&mut a, vec![]);
        let (auto_left, auto_right) = autos(&a, &p, sr);
        let mut tops = vec![];
        for ribbon in [RibbonMode::Full, RibbonMode::Tabs, RibbonMode::Hidden] {
            for bits in 0..16u8 {
                let (left, right) = (bits & 1 != 0, bits & 2 != 0);
                let (options, status) = (bits & 4 != 0, bits & 8 != 0);
                a.set_ribbon_mode(ribbon);
                a.prefs.browser_open = left;
                a.prefs.props_open = right;
                a.prefs.fold.options_bar = options;
                a.prefs.fold.status_bar = status;
                let tag = format!("{lang:?} {screen:?} {ribbon:?} l={left} r={right} \
                                   o={options} s={status}");
                r.frame(&mut a, vec![]);
                let p = r.frame(&mut a, vec![]);
                assert_whole(&tag, &a, &p, sr);
                let v = canvas(&a);
                let c = &a.sh.cat;
                let (left_rail, right_rail) = (!left || auto_left, !right || auto_right);
                let browser_rail = rail(&p, &c.t("browser.title"), true, sr);
                assert_eq!(browser_rail.is_some(), left_rail, "{tag}: browser rail");
                let props_rail = rail(&p, &c.t("props.title"), false, sr);
                assert_eq!(props_rail.is_some(), right_rail, "{tag}: properties rail");
                if left_rail {
                    assert!((v.left() - fold::RAIL_W).abs() <= 1.0, "{tag}: {v:?}");
                }
                if right_rail {
                    assert!((v.right() - (screen.0 - fold::RAIL_W)).abs() <= 1.0, "{tag}");
                }
                if !status {
                    assert!((v.bottom() - screen.1).abs() <= EPS, "{tag}: {v:?}");
                }
                let hidden = ribbon == RibbonMode::Hidden;
                assert_eq!(has_button(&p, &c.t("layout_fold.ribbon.show")), hidden, "{tag}");
                if bits == 0 {
                    tops.push(v.top());
                }
            }
        }
        assert!(tops.windows(2).all(|w| w[1] < w[0] - EPS), "{lang:?} {screen:?}: {tops:?}");
        a.chrome.fold.clean = true;
        r.frame(&mut a, vec![]);
        let p = r.frame(&mut a, vec![]);
        let tag = format!("{lang:?} {screen:?} clean");
        assert_whole(&tag, &a, &p, sr);
        let v = canvas(&a);
        assert_eq!((v.left(), v.right(), v.bottom()), (0.0, screen.0, screen.1), "{tag}");
        assert!(rail(&p, &a.sh.cat.t("props.title"), false, sr).is_none(), "{tag}");
        // Esc brings it back
        let p = r.frame(&mut a, key(Key::Escape, Modifiers::NONE));
        assert!(!a.chrome_clean(), "{tag}: Esc");
        assert_whole(&format!("{tag} after Esc"), &a, &p, sr);
    }
}

#[test]
fn every_combination_en() {
    every_combination(Lang::En);
}

#[test]
fn every_combination_ru() {
    every_combination(Lang::Ru);
}

/// The fold moves over several frames and lands exactly; with reduced motion it lands at once.
#[test]
fn folds_move_unless_motion_is_reduced() {
    for calm in [false, true] {
        let mut a = open_app(Lang::En);
        a.sh.theme.reduced_motion = calm;
        let mut r = Run::new(SCREENS[1]);
        r.settle(&mut a);
        let before = canvas(&a);
        assert!(a.run(fold::CMD_PROPS));
        r.frame(&mut a, vec![]);
        let first = canvas(&a);
        let landed = r.settle(&mut a);
        let last = canvas(&a);
        assert_whole("motion", &a, &landed, r.rect());
        assert!(last.right() > before.right() + 100.0, "{before:?} {last:?}");
        if calm {
            assert_eq!(first, last, "reduced motion jumps");
        } else {
            assert!(first.right() < last.right() - 1.0, "{first:?} {last:?}");
        }
    }
}

/// Folding a dock leaves the plan's model where it was on screen; nothing jumps.
#[test]
fn a_fold_keeps_the_plan_under_the_same_pixel() {
    // where the model origin is drawn
    let origin = |a: &EditorApp| {
        let c = a.plan.drawn_rect().expect("plan drawn").center();
        let (m, s) = (a.plan.center, a.plan.scale);
        (c.x as f64 - m.x * s, c.y as f64 + m.y * s)
    };
    for cmd in [fold::CMD_BROWSER, fold::CMD_PROPS, fold::CMD_RIBBON, fold::CMD_STATUS] {
        let mut a = open_app(Lang::En);
        let mut r = Run::new(SCREENS[2]);
        r.settle(&mut a);
        let (v0, o0) = (canvas(&a), origin(&a));
        assert!(a.run(cmd));
        r.settle(&mut a);
        let (v1, o1) = (canvas(&a), origin(&a));
        assert_ne!(v0, v1, "{cmd}: the canvas did not change");
        assert!((o0.0 - o1.0).abs() < 1.0 && (o0.1 - o1.1).abs() < 1.0, "{cmd}: {o0:?} {o1:?}");
    }
}

/// Below 1280 px Properties fold to a rail on their own; the setting stays and the panel is
/// back when the window is wide again.
#[test]
fn narrow_window_rails_properties_and_keeps_the_choice() {
    let mut a = open_app(Lang::En);
    let mut r = Run::new((1200.0, 800.0));
    let p = r.settle(&mut a);
    let tip = a.sh.cat.t("props.title");
    assert!(rail(&p, &tip, false, r.rect()).is_some(), "rail at 1200");
    assert!(a.prefs.props_open, "the setting stays");
    let mut r = Run::new((1440.0, 900.0));
    let p = r.settle(&mut a);
    assert!(rail(&p, &tip, false, r.rect()).is_none(), "back at 1440");
}

/// A click on a rail opens its dock; a rest of the pointer on it does too.
#[test]
fn a_rail_opens_on_click_and_on_rest() {
    for rest in [false, true] {
        let mut a = open_app(Lang::En);
        a.prefs.props_open = false;
        let mut r = Run::new(SCREENS[2]);
        let p = r.settle(&mut a);
        let tip = a.sh.cat.t("props.title");
        let at = rail(&p, &tip, false, r.rect()).expect("rail").center();
        r.frame(&mut a, vec![egui::Event::PointerMoved(at)]);
        if rest {
            for _ in 0..((fold::HOLD_S * 60.0) as usize + 4) {
                r.frame(&mut a, vec![]);
            }
        } else {
            let click = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            r.frame(&mut a, vec![click(true)]);
            r.frame(&mut a, vec![click(false)]);
        }
        assert!(a.prefs.props_open, "rest={rest}");
    }
}

/// The folds are saved and a new session starts with them.
#[test]
fn folds_survive_a_restart() {
    let mut a = open_app(Lang::En);
    for c in [fold::CMD_RIBBON, fold::CMD_RIBBON, fold::CMD_OPTIONS, fold::CMD_STATUS] {
        assert!(a.run(c));
    }
    assert!(a.run(fold::CMD_BROWSER));
    let saved = a.prefs.text();
    let mut b = app_in(Lang::En);
    b.prefs = crate::prefs::Prefs::parse(&saved);
    let mut r = Run::new(SCREENS[2]);
    let p = r.settle(&mut b);
    assert_eq!(RibbonMode::of(&b.prefs), RibbonMode::Hidden);
    assert!(has_button(&p, &b.sh.cat.t("layout_fold.ribbon.show")));
    assert!(!b.prefs.fold.options_bar && !b.prefs.fold.status_bar && !b.prefs.browser_open);
    assert!(rail(&p, &b.sh.cat.t("browser.title"), true, r.rect()).is_some());
}

/// On an AI tab both folds work, and with both sides open the preview keeps at least 560 px.
fn ai_tab_folds(lang: Lang) {
    use crate::ai::ui::modes::{self, AiMode};
    for screen in SCREENS {
        for f in [AiFold::Open, AiFold::Work, AiFold::Preview] {
            let mut a = open_app(lang);
            a.sh.theme.reduced_motion = true;
            a.prefs.fold.ai = f;
            modes::enter(&mut a, AiMode::Build);
            let mut r = Run::new(screen);
            let p = r.settle(&mut a);
            let tag = format!("{lang:?} {screen:?} {f:?}");
            assert_whole(&tag, &a, &p, r.rect());
            let v = canvas(&a);
            let c = &a.sh.cat;
            let plan = a.plan.drawn_rect();
            match f {
                AiFold::Preview => {
                    assert!(has_button(&p, &c.t("layout_fold.ai.preview")), "{tag}: rail");
                }
                AiFold::Work => {
                    assert!(has_button(&p, &c.t("layout_fold.ai.work")), "{tag}: rail");
                    let w = plan.expect("plan drawn").width();
                    assert!(w >= v.width() - fold::RAIL_W - 1.0, "{tag}: {w}");
                }
                AiFold::Open => {
                    // no room for both: the preview folds to a rail rather than shrink
                    if !has_button(&p, &c.t("layout_fold.ai.preview")) {
                        let w = plan.map_or(0.0, |r| r.width());
                        assert!(w >= modes::PREVIEW_MIN - 1.0, "{tag}: preview {w} px");
                        assert!(has_button(&p, &c.t("layout_fold.ai.work.hide")), "{tag}");
                        assert!(has_button(&p, &c.t("layout_fold.ai.preview.hide")), "{tag}");
                    }
                }
            }
        }
    }
}

#[test]
fn ai_tab_folds_en() {
    ai_tab_folds(Lang::En);
}

#[test]
fn ai_tab_folds_ru() {
    ai_tab_folds(Lang::Ru);
}

/// The layer of the dialog the command opens.
fn dialog_layer(cmd: &str) -> LayerId {
    let dialog = |k: &str| LayerId::new(Order::Middle, egui::Id::new(("ddc.dialog", k)));
    match cmd {
        "ai.settings" => dialog("ai.settings"),
        "ai.what_sent" => dialog("ai.sent"),
        "ai.audit" => dialog("ai.audit"),
        "ai.mcp_settings" => dialog("mcp.settings"),
        "settings" => LayerId::new(Order::Foreground, egui::Id::new(("ddc.modal", "settings"))),
        _ => unreachable!("{cmd}"),
    }
}

/// Layers drawn above `l` this frame whose area covers part of `r`; a window the dialog opened
/// itself (a peer floating window, stacked by focus) does not count.
fn covering(ctx: &egui::Context, l: LayerId, r: egui::Rect) -> Vec<String> {
    let peers: Vec<egui::Id> = ctx.memory(|m| m.layer_ids().map(|x| x.id).collect());
    let peers: Vec<egui::Id> =
        peers.into_iter().filter(|id| *id != l.id && ddc_ui::z::is_window(ctx, *id)).collect();
    ctx.memory(|m| {
        let seen = m.areas().visible_layer_ids();
        // paint order: everything after the dialog's layer is drawn over it
        let above: Vec<LayerId> = m.layer_ids().skip_while(|x| *x != l).skip(1).collect();
        above
            .into_iter()
            .filter(|x| seen.contains(x) && !peers.contains(&x.id))
            .filter_map(|x| m.area_rect(x.id).filter(|a| a.intersects(r)).map(|a| (x, a)))
            .map(|(x, a)| format!("{x:?} {a:?}"))
            .collect()
    })
}

/// Each dialog reachable from the AI tabs and their setup page opens over everything: with a
/// wall selected (its edit bar on the canvas), the ribbon's band open over the canvas, the AI
/// tab and its preview shown. The dialog closes the band, nothing covers it, it is all inside
/// the window.
fn dialogs_on_top(lang: Lang) {
    use crate::ai::ui::modes::{self, AiMode};
    for screen in SCREENS {
        for cmd in ["ai.settings", "ai.what_sent", "ai.audit", "ai.mcp_settings", "settings"] {
            let mut a = open_app(lang);
            a.sh.theme.reduced_motion = true;
            crate::modify_tools::tests::walls(&mut a.ed, &[((0.0, 0.0), (6.0, 0.0))]);
            a.ed.selection = vec![crate::editor::Sel::Wall(a.ed.walls[0].id)];
            modes::enter(&mut a, AiMode::Build);
            a.set_ribbon_mode(RibbonMode::Tabs);
            let mut r = Run::new(screen);
            r.settle(&mut a);
            a.sh.ribbon.peek = true;
            r.frame(&mut a, vec![]);
            let tag = format!("{lang:?} {screen:?} {cmd}");
            let band = LayerId::new(Order::Foreground, egui::Id::new("ribbon.peek"));
            assert!(r.ctx.memory(|m| m.areas().is_visible(&band)), "{tag}: the band is open");
            if cmd == "settings" {
                a.open_settings(1);
            } else {
                assert!(a.run(cmd), "{tag}: not a command");
            }
            let p = r.settle(&mut a);
            assert!(!a.sh.ribbon.peek, "{tag}: the band stayed open under the dialog");
            let l = dialog_layer(cmd);
            let dr = r.ctx.memory(|m| m.area_rect(l.id));
            let dr = dr.unwrap_or_else(|| panic!("{tag}: the dialog was not drawn"));
            assert!(r.rect().expand(EPS).contains_rect(dr), "{tag}: {dr:?} leaves the window");
            let over = covering(&r.ctx, l, dr);
            assert!(over.is_empty(), "{tag}: covered by {over:?}");
            assert!(!p.controls.is_empty(), "{tag}");
        }
    }
}

#[test]
fn dialogs_on_top_en() {
    dialogs_on_top(Lang::En);
}

#[test]
fn dialogs_on_top_ru() {
    dialogs_on_top(Lang::Ru);
}
