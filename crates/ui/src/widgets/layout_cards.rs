// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Layout chooser: a live schematic of `arrange()` in the real panel colours (LAYOUTS §4) — no
//! second render, so a card always matches the actual layout. Also the title bar work-mode
//! group (view segments + layout button) and its popover, with the pure state they run on.
use super::{elide, icon_name, paint_keycaps, rich_tooltip};
use crate::icons::IconCache;
use crate::theme::{control, icon_size, layout, space, stroke, Role, Theme};
use crate::workspace::{arrange, PanelId, Workspace};
use egui::{pos2, vec2, Color32, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

pub const CARD: egui::Vec2 = egui::Vec2::new(240.0, 150.0);
/// Reference screen the thumbnails are computed for.
pub const THUMB_SCREEN: egui::Vec2 = egui::Vec2::new(1920.0, 1080.0);
/// Thumbnail height inside a settings card; decorations scale from it.
const THUMB_REF_H: f32 = CARD.y - space::SM - 34.0;

fn fill(th: &Theme, p: PanelId) -> egui::Color32 {
    match p {
        PanelId::TitleBar | PanelId::StatusBar => th.c("bg.app"),
        PanelId::ViewHost => th.c("bg.viewport"),
        PanelId::ViewHost2 => th.c("canvas.bg.bottom"),
        PanelId::ContextBar | PanelId::Radial => th.c("accent.subtle"),
        _ => th.c("bg.panel"),
    }
}

/// Paints `arrange(ws)` for a 1920×1080 screen scaled into `r`; details shrink with `r`.
pub fn paint_thumbnail(ui: &Ui, th: &Theme, ws: &Workspace, r: Rect) {
    let pl = arrange(
        ws,
        Rect::from_min_size(egui::Pos2::ZERO, THUMB_SCREEN),
        &th.tokens.breakpoint,
    );
    let s = (r.width() / THUMB_SCREEN.x).min(r.height() / THUMB_SCREEN.y);
    let o = r.center() - THUMB_SCREEN * s / 2.0;
    let map = |q: Rect| Rect::from_min_max(o + q.min.to_vec2() * s, o + q.max.to_vec2() * s);
    // one detail unit: 8 pt on a settings card, proportionally less on a popover row
    let u = space::SM * (r.height() / THUMB_REF_H).clamp(0.35, 1.0);
    let p = ui.painter().with_clip_rect(r);
    let line = Stroke::new(stroke::HAIRLINE, th.c("border.subtle"));
    let dim = th.c("border.strong");
    for (id, q) in &pl.rects {
        let q = map(*q);
        p.rect(q, 0.0, fill(th, *id), line, StrokeKind::Inside);
        match id {
            // a few "rows" so the panel reads as a list
            PanelId::Browser | PanelId::Inspector | PanelId::DataTable => {
                let mut y = q.top() + 0.75 * u;
                while y + 0.5 * u < q.bottom() - 0.5 * u && y < q.top() + 7.5 * u {
                    let w = (q.width() - 1.25 * u).max(2.0 * stroke::HAIRLINE);
                    p.rect_filled(
                        Rect::from_min_size(pos2(q.left() + 0.625 * u, y), vec2(w, 0.25 * u)),
                        stroke::HAIRLINE,
                        dim,
                    );
                    y += 0.875 * u;
                }
            }
            PanelId::CommandSurface => {
                let wide = q.width() > q.height();
                let n = if wide { 10 } else { 6 };
                for i in 0..n {
                    let step = 1.125 * u * i as f32;
                    let c = if wide {
                        pos2(q.left() + u + step, q.center().y)
                    } else {
                        pos2(q.center().x, q.top() + u + step)
                    };
                    p.rect_filled(
                        Rect::from_center_size(c, Vec2::splat(0.625 * u)),
                        stroke::HAIRLINE,
                        dim,
                    );
                }
            }
            PanelId::ViewHost | PanelId::ViewHost2 => {
                let c = q.center();
                let a = q.width().min(q.height()) * 0.25;
                p.rect_stroke(
                    Rect::from_center_size(c, vec2(a * 1.6, a)),
                    0.0,
                    Stroke::new(
                        (stroke::FOCUS * u / space::SM).max(stroke::HAIRLINE),
                        th.c("canvas.cut"),
                    ),
                    StrokeKind::Middle,
                );
            }
            _ => {}
        }
    }
    for (id, q) in &pl.overlays {
        let q = map(*q);
        let rad = if *id == PanelId::Radial {
            q.width() / 2.0
        } else {
            th.radius("sm")
        };
        p.rect(
            q,
            rad,
            fill(th, *id),
            Stroke::new(stroke::HAIRLINE, th.c("accent")),
            StrokeKind::Inside,
        );
    }
}

/// One selectable card: thumbnail, name, hotkey. Accent frame when selected.
pub fn card(
    ui: &mut Ui,
    th: &Theme,
    ws: &Workspace,
    name: &str,
    hotkey: &str,
    selected: bool,
) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(CARD, Sense::click());
    let p = ui.painter();
    let hov = resp.hovered();
    p.rect(
        r,
        th.radius("md"),
        th.c("bg.panel"),
        Stroke::new(
            if selected {
                stroke::FOCUS
            } else {
                stroke::HAIRLINE
            },
            th.c(if selected {
                "accent"
            } else if hov {
                "border.strong"
            } else {
                "border.subtle"
            }),
        ),
        StrokeKind::Inside,
    );
    let thumb = Rect::from_min_max(
        r.min + Vec2::splat(space::SM),
        pos2(r.max.x - space::SM, r.max.y - 34.0),
    );
    paint_thumbnail(ui, th, ws, thumb);
    let y = r.max.y - 17.0;
    // keycaps first: the name takes what is left of the row and is cut with an ellipsis
    let kx = paint_keycaps(p, th, pos2(r.right() - space::SM, y), hotkey);
    let nx = r.left() + 10.0;
    let ink = th.c(if selected { "accent" } else { "text.primary" });
    let role = if selected { Role::BodyStrong } else { Role::Body };
    let g = elide(ui, name, th.font(role), ink, (kx - space::SM - nx).max(1.0));
    p.galley(pos2(nx, y - g.size().y / 2.0), g, ink);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Grid of cards; returns the clicked workspace id. `name_of` gives the display name.
pub fn grid(
    ui: &mut Ui,
    th: &Theme,
    list: &[Workspace],
    active: &str,
    cols: usize,
    name_of: &dyn Fn(&Workspace) -> String,
) -> Option<String> {
    let mut out = None;
    // fewer columns where the room is short: a row of cards never widens its window
    let gap = ui.spacing().item_spacing.x;
    let fit = ((ui.available_width() + gap) / (CARD.x + gap)).floor().max(1.0) as usize;
    let cols = cols.max(1).min(fit);
    for row in list.chunks(cols).enumerate() {
        ui.horizontal(|ui| {
            for (j, ws) in row.1.iter().enumerate() {
                let n = row.0 * cols + j + 1;
                let keys = if n <= 5 {
                    format!("Ctrl+Alt+{n}")
                } else {
                    String::new()
                };
                if card(ui, th, ws, &name_of(ws), &keys, ws.id == active).clicked() {
                    out = Some(ws.id.clone());
                }
            }
        });
        ui.add_space(space::SM);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Title bar work-mode group

/// Width stage of the title bar group, widest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fit {
    /// Segments and layout button with labels.
    Full,
    /// Icon segments, layout button keeps its name.
    Mid,
    /// Icons only.
    Icons,
}

/// Search field width range: 30 % of the bar between these.
pub const SEARCH_MIN: f32 = 220.0;
pub const SEARCH_MAX: f32 = 420.0;
/// Longest layout name shown on the button before it is elided.
pub const NAME_MAX: f32 = control::DROPDOWN_W;
const CARET_W: f32 = space::SM;
/// Stroke of painted glyphs, same as the SVG icon set (ICONS_V4.md).
const GLYPH: f32 = 1.5;
/// Popover row height and width.
const ROW_H: f32 = space::XXXL + space::LG;
pub const POP_W: f32 = layout::DIALOG_CONFIRM;

/// Where the search field and the work-mode group go in the title bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TitleFit {
    pub fit: Fit,
    pub group: Rect,
    /// `None` when even a narrow field does not fit (the palette stays on Ctrl+K).
    pub search: Option<Rect>,
}

/// Places the centred search field and the right-hand group. `widths` are the group widths for
/// [`Fit::Full`], [`Fit::Mid`], [`Fit::Icons`]; `left_end` is where the menus end. The group
/// drops labels first; only then does the search move left and shrink. They never overlap.
pub fn title_fit(
    bar: Rect,
    left_end: f32,
    widths: [f32; 3],
    icons_below: f32,
    search_h: f32,
) -> TitleFit {
    let gap = space::LG;
    let pref = (bar.width() * 0.3).clamp(SEARCH_MIN, SEARCH_MAX);
    let centred = Rect::from_center_size(bar.center(), vec2(pref, search_h));
    let lo = left_end + gap;
    let stages = [Fit::Full, Fit::Mid, Fit::Icons];
    let first = if bar.width() < icons_below { 2 } else { 0 };
    let fit = stages[first..]
        .iter()
        .copied()
        .find(|f| {
            let gl = bar.right() - widths[*f as usize];
            centred.left() >= lo && centred.right() + gap <= gl
        })
        .unwrap_or(Fit::Icons);
    let w = widths[fit as usize];
    let group = Rect::from_min_max(pos2(bar.right() - w, bar.top()), bar.max);
    let right = centred.right().min(group.left() - gap);
    let left = (right - pref).max(lo);
    let search = (right - left >= control::DROPDOWN_W).then(|| {
        Rect::from_min_max(pos2(left, centred.top()), pos2(right, centred.bottom()))
    });
    TitleFit { fit, group, search }
}

/// One segment of the view control.
#[derive(Debug, Clone, Copy)]
pub struct Seg<'a> {
    pub icon: &'a str,
    pub label: &'a str,
    pub title: &'a str,
    pub desc: &'a str,
    pub keys: &'a str,
}

/// The layout button's content and tooltip.
#[derive(Debug, Clone, Copy)]
pub struct LayoutBtn<'a> {
    pub icon: &'a str,
    pub title: &'a str,
    pub desc: &'a str,
    pub keys: &'a str,
}

fn label_w(ui: &Ui, th: &Theme, text: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_string(), th.font(Role::Label), th.c("text.primary"))
        .size()
        .x
}

fn seg_w(ui: &Ui, th: &Theme, s: &Seg, labels: bool) -> f32 {
    if labels {
        space::SM + icon_size::SM + space::XS + label_w(ui, th, s.label) + space::SM
    } else {
        th.d().control
    }
}

pub fn segments_width(ui: &Ui, th: &Theme, segs: &[Seg], labels: bool) -> f32 {
    segs.iter().map(|s| seg_w(ui, th, s, labels)).sum::<f32>() + 2.0 * space::XXS
}

/// The layout button is a quiet icon with a caret at every stage; its name is in the tooltip.
pub fn layout_button_width() -> f32 {
    space::SM + icon_size::SM + space::SM + CARET_W + space::SM
}

/// Group widths for [`Fit::Full`], [`Fit::Mid`], [`Fit::Icons`] (segments, gap, button).
/// Mid and Icons are the same width: only the view segments lose their labels.
pub fn group_widths(ui: &Ui, th: &Theme, segs: &[Seg]) -> [f32; 3] {
    let b = layout_button_width();
    let icons = segments_width(ui, th, segs, false) + space::SM;
    [
        segments_width(ui, th, segs, true) + space::SM + b,
        icons + b,
        icons + b,
    ]
}

/// Background of a control state; returns the foreground tint. `held` keeps the pressed look
/// (an open popover).
fn state_fill(
    ui: &Ui,
    th: &Theme,
    r: Rect,
    resp: &egui::Response,
    held: bool,
    rad: f32,
) -> Color32 {
    let p = ui.painter();
    if !ui.is_enabled() {
        // the disabled Ui fades its painter; the colour must not dim twice
        return th.c("text.secondary");
    }
    if held || resp.is_pointer_button_down_on() {
        p.rect_filled(r, rad, th.pressed_fill());
        return th.c("text.primary");
    }
    let t = ui.ctx().animate_bool_with_time(
        resp.id.with("ddc.hover"),
        resp.hovered() || resp.has_focus(),
        ui.style().animation_time,
    );
    if t > 0.0 {
        p.rect_filled(r, rad, th.hover_fill().gamma_multiply(t));
    }
    th.c("text.secondary").lerp_to_gamma(th.c("text.primary"), t)
}

/// Segmented view control with icons; `labels` false draws icons only. Returns a newly
/// picked index. Each segment is a Tab stop; Enter or Space picks it.
pub fn view_segments(
    ui: &mut Ui,
    th: &Theme,
    icons: &mut IconCache,
    segs: &[Seg],
    sel: usize,
    labels: bool,
) -> Option<usize> {
    let h = th.d().control;
    let ws: Vec<f32> = segs.iter().map(|s| seg_w(ui, th, s, labels)).collect();
    let total = ws.iter().sum::<f32>() + 2.0 * space::XXS;
    let (rect, _) = ui.allocate_exact_size(vec2(total, h), Sense::hover());
    ui.painter()
        .rect_filled(rect, th.radius("md"), th.c("bg.sunken"));
    let mut x = rect.left() + space::XXS;
    let mut out = None;
    for (i, s) in segs.iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(x, rect.top() + space::XXS),
            vec2(ws[i], h - 2.0 * space::XXS),
        );
        x += ws[i];
        let resp = ui.interact(r, egui::Id::new(("ddc.viewseg", i)), Sense::click());
        let on = i == sel;
        let fg = if on {
            ui.painter().rect(
                r,
                th.radius("sm"),
                th.c("bg.panel"),
                Stroke::new(stroke::HAIRLINE, th.c("border.subtle")),
                StrokeKind::Inside,
            );
            th.c("text.primary")
        } else {
            state_fill(ui, th, r, &resp, false, th.radius("sm"))
        };
        let cx = if labels {
            r.left() + space::SM + icon_size::SM / 2.0
        } else {
            r.center().x
        };
        let ic = Rect::from_center_size(pos2(cx, r.center().y), Vec2::splat(icon_size::SM));
        let tint = if on && ui.is_enabled() {
            th.c("accent")
        } else {
            fg
        };
        icons.paint(ui, &icon_name(s.icon, on), icon_size::SM as u32, ic, tint);
        if labels {
            let g = ui
                .painter()
                .layout_no_wrap(s.label.to_string(), th.font(Role::Label), fg);
            let pos = pos2(ic.right() + space::XS, r.center().y - g.size().y / 2.0);
            ui.painter().galley(pos, g, fg);
        }
        let resp = rich_tooltip(resp, th, s.title, s.desc, s.keys);
        if resp.clicked() && !on {
            out = Some(i);
        }
    }
    out
}

/// Layout button: a quiet icon and a caret, no resting fill; the layout name is in the
/// tooltip. `open` keeps the pressed look while the popover shows. The caller adds the tooltip.
pub fn layout_button(
    ui: &mut Ui,
    th: &Theme,
    icons: &mut IconCache,
    b: &LayoutBtn,
    open: bool,
) -> egui::Response {
    let w = layout_button_width();
    let (r, _) = ui.allocate_exact_size(vec2(w, th.d().control), Sense::hover());
    let resp = ui.interact(r, egui::Id::new("ddc.layout.button"), Sense::click());
    let enabled = ui.is_enabled();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, b.title));
    let fg = state_fill(ui, th, r, &resp, open, th.radius("md"));
    let ic = Rect::from_center_size(
        pos2(r.left() + space::SM + icon_size::SM / 2.0, r.center().y),
        Vec2::splat(icon_size::SM),
    );
    icons.paint(ui, b.icon, icon_size::SM as u32, ic, fg);
    let c = pos2(r.right() - space::SM - CARET_W / 2.0, r.center().y);
    let k = CARET_W / 2.0;
    let dy = if open { -k / 2.0 } else { k / 2.0 };
    ui.painter().add(egui::Shape::line(
        vec![
            c + vec2(-k, -dy),
            c + vec2(0.0, dy),
            c + vec2(k, -dy),
        ],
        Stroke::new(GLYPH, fg),
    ));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// What the group reports back this frame.
pub struct GroupOut {
    pub view: Option<usize>,
    pub button: egui::Response,
}

/// The whole group, left to right: view segments, gap, layout button.
#[allow(clippy::too_many_arguments)]
pub fn mode_group(
    ui: &mut Ui,
    th: &Theme,
    icons: &mut IconCache,
    segs: &[Seg],
    sel: usize,
    b: &LayoutBtn,
    open: bool,
    fit: Fit,
) -> GroupOut {
    ui.spacing_mut().item_spacing.x = space::SM;
    let view = view_segments(ui, th, icons, segs, sel, fit == Fit::Full);
    let button = layout_button(ui, th, icons, b, open);
    let button = if open {
        button
    } else {
        rich_tooltip(button, th, b.title, b.desc, b.keys)
    };
    GroupOut { view, button }
}

// ---------------------------------------------------------------------------------------------
// Layout popover

/// Keyboard and pointer state of the layout popover; pure, so it is tested without a GPU.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Picker {
    pub open: bool,
    /// Highlighted row: follows arrows and the moving pointer.
    pub hi: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKey {
    Prev,
    Next,
    First,
    Last,
    Accept,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickOut {
    Stay,
    Picked(usize),
    Closed,
}

impl Picker {
    /// Opens on the current layout, or closes.
    pub fn toggle(&mut self, current: usize) {
        *self = if self.open {
            Picker::default()
        } else {
            Picker {
                open: true,
                hi: current,
            }
        };
    }
    pub fn close(&mut self) {
        self.open = false;
    }
    /// Arrows wrap; Enter picks the highlighted row and closes; Esc closes.
    pub fn key(&mut self, k: PickKey, n: usize) -> PickOut {
        if !self.open {
            return PickOut::Stay;
        }
        if n == 0 {
            self.open = false;
            return PickOut::Closed;
        }
        self.hi = self.hi.min(n - 1);
        match k {
            PickKey::Prev => self.hi = (self.hi + n - 1) % n,
            PickKey::Next => self.hi = (self.hi + 1) % n,
            PickKey::First => self.hi = 0,
            PickKey::Last => self.hi = n - 1,
            PickKey::Accept => {
                self.open = false;
                return PickOut::Picked(self.hi);
            }
            PickKey::Cancel => {
                self.open = false;
                return PickOut::Closed;
            }
        }
        PickOut::Stay
    }
    pub fn click(&mut self, i: usize) -> PickOut {
        self.open = false;
        PickOut::Picked(i)
    }
}

fn pick_key(k: egui::Key) -> Option<PickKey> {
    use egui::Key::*;
    Some(match k {
        ArrowUp | ArrowLeft => PickKey::Prev,
        ArrowDown | ArrowRight => PickKey::Next,
        Home => PickKey::First,
        End => PickKey::Last,
        Enter | Space => PickKey::Accept,
        Escape => PickKey::Cancel,
        _ => return None,
    })
}

/// Takes the popover keys out of this frame's input, in order, so tools, views and focused
/// buttons never see them. Arrows repeat; Enter, Space and Esc act once per press.
pub fn take_picker_keys(ctx: &egui::Context) -> Vec<PickKey> {
    ctx.input_mut(|i| {
        let mut out = Vec::new();
        i.events.retain(|e| match e {
            egui::Event::Key {
                key,
                pressed,
                repeat,
                modifiers,
                ..
            } if modifiers.is_none() => match pick_key(*key) {
                Some(a) => {
                    let once = matches!(a, PickKey::Accept | PickKey::Cancel);
                    if *pressed && !(once && *repeat) {
                        out.push(a);
                    }
                    false
                }
                None => true,
            },
            _ => true,
        });
        out
    })
}

/// One popover row's text.
pub struct Card<'a> {
    pub ws: &'a Workspace,
    pub name: &'a str,
    pub purpose: &'a str,
    pub keys: &'a str,
}

/// Popover chrome strings.
pub struct PopText<'a> {
    pub title: &'a str,
    pub cycle: &'a str,
    pub cycle_keys: &'a str,
    pub current: &'a str,
    pub settings: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopOut {
    Pick(usize),
    Settings,
}

fn row(
    ui: &mut Ui,
    th: &Theme,
    c: &Card,
    current: bool,
    hi: bool,
    cur_txt: &str,
) -> egui::Response {
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::click());
    let p = ui.painter();
    if resp.is_pointer_button_down_on() {
        p.rect_filled(r, th.radius("md"), th.pressed_fill());
    } else if hi {
        p.rect_filled(r, th.radius("md"), th.hover_fill());
    }
    if current {
        let bar = Rect::from_min_size(
            pos2(r.left(), r.top() + space::MD),
            vec2(stroke::INDICATOR, r.height() - 2.0 * space::MD),
        );
        p.rect_filled(bar, stroke::HAIRLINE, th.c("accent"));
    }
    let th_h = r.height() - 2.0 * space::SM;
    let tr = Rect::from_min_size(
        pos2(r.left() + space::MD, r.center().y - th_h / 2.0),
        vec2(th_h * THUMB_SCREEN.x / THUMB_SCREEN.y, th_h),
    );
    p.rect_filled(tr, th.radius("sm"), th.c("bg.app"));
    paint_thumbnail(ui, th, c.ws, tr);
    let p = ui.painter();
    p.rect_stroke(
        tr,
        th.radius("sm"),
        if current {
            Stroke::new(stroke::FOCUS, th.c("accent"))
        } else {
            Stroke::new(stroke::HAIRLINE, th.c("border.subtle"))
        },
        StrokeKind::Outside,
    );
    let mut kx = r.right() - space::SM;
    if !c.keys.is_empty() {
        kx = paint_keycaps(p, th, pos2(kx, r.center().y), c.keys) - space::XS;
    }
    if current {
        let g = p.layout_no_wrap(
            cur_txt.to_string(),
            th.font(Role::Caption),
            th.c("text.secondary"),
        );
        kx -= g.size().x;
        p.galley(
            pos2(kx, r.center().y - g.size().y / 2.0),
            g,
            th.c("text.secondary"),
        );
        kx -= space::SM;
    }
    let tx = tr.right() + space::MD;
    let tw = (kx - space::SM - tx).max(1.0);
    let name = elide(ui, c.name, th.font(Role::BodyStrong), th.c("text.primary"), tw);
    let purpose = elide(ui, c.purpose, th.font(Role::Body), th.c("text.secondary"), tw);
    let total = name.size().y + space::XXS + purpose.size().y;
    let y0 = r.center().y - total / 2.0;
    let p = ui.painter();
    let ny = y0 + name.size().y + space::XXS;
    p.galley(pos2(tx, y0), name, th.c("text.primary"));
    p.galley(pos2(tx, ny), purpose, th.c("text.secondary"));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Layout popover under `anchor`, right-aligned to it and kept on screen. Rows: live
/// schematic, name, one-line purpose, shortcut; the current layout has the accent marks.
/// A press outside it (and outside `anchor`) closes it.
#[allow(clippy::too_many_arguments)]
pub fn popover(
    ctx: &egui::Context,
    th: &Theme,
    icons: &mut IconCache,
    anchor: Rect,
    cards: &[Card],
    current: usize,
    pick: &mut Picker,
    text: &PopText,
) -> Option<PopOut> {
    if !pick.open {
        return None;
    }
    let screen = ctx.screen_rect();
    let x_min = screen.left() + space::SM;
    let x_max = (screen.right() - POP_W - space::SM).max(x_min);
    let x = (anchor.right() - POP_W).clamp(x_min, x_max);
    let pos = pos2(x, anchor.bottom() + space::XS);
    let moved = ctx.input(|i| i.pointer.delta() != Vec2::ZERO);
    let mut out = None;
    let area = egui::Area::new(egui::Id::new("ddc.layout.popover"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(th.c("bg.elevated"))
                .stroke(Stroke::new(stroke::HAIRLINE, th.c("border.overlay")))
                .corner_radius(th.radius("lg"))
                .shadow(th.shadow(2))
                .inner_margin(egui::Margin::same(space::SM as i8))
                .show(ui, |ui| {
                    ui.set_width(POP_W - 2.0 * space::SM);
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    let w = ui.available_width();
                    let (hr, _) = ui.allocate_exact_size(vec2(w, th.d().control), Sense::hover());
                    let p = ui.painter();
                    p.text(
                        pos2(hr.left() + space::SM, hr.center().y),
                        egui::Align2::LEFT_CENTER,
                        text.title.to_uppercase(),
                        th.font(Role::Section),
                        th.c("text.secondary"),
                    );
                    if !text.cycle_keys.is_empty() {
                        let kx = paint_keycaps(
                            p,
                            th,
                            pos2(hr.right() - space::SM, hr.center().y),
                            text.cycle_keys,
                        );
                        p.text(
                            pos2(kx - space::XS, hr.center().y),
                            egui::Align2::RIGHT_CENTER,
                            text.cycle,
                            th.font(Role::Caption),
                            th.c("text.secondary"),
                        );
                    }
                    ui.add_space(space::XS);
                    for (i, c) in cards.iter().enumerate() {
                        let resp = row(ui, th, c, i == current, i == pick.hi, text.current);
                        if resp.hovered() && moved {
                            pick.hi = i;
                        }
                        if resp.clicked() {
                            out = Some(PopOut::Pick(i));
                        }
                    }
                    ui.add_space(space::XS);
                    let (sr, _) =
                        ui.allocate_exact_size(vec2(w, stroke::HAIRLINE), Sense::hover());
                    let line = Stroke::new(stroke::HAIRLINE, th.c("border.subtle"));
                    ui.painter().hline(sr.x_range(), sr.center().y, line);
                    ui.add_space(space::XS);
                    let fh = th.d().control + space::SM;
                    let (fr, _) = ui.allocate_exact_size(vec2(w, fh), Sense::hover());
                    let fid = egui::Id::new("ddc.layout.popover.settings");
                    let fresp = ui.interact(fr, fid, Sense::click());
                    let fg = state_fill(ui, th, fr, &fresp, false, th.radius("md"));
                    let ic = Rect::from_center_size(
                        pos2(fr.left() + space::SM + icon_size::SM / 2.0, fr.center().y),
                        Vec2::splat(icon_size::SM),
                    );
                    icons.paint(ui, "layout-settings", icon_size::SM as u32, ic, fg);
                    ui.painter().text(
                        pos2(ic.right() + space::SM, fr.center().y),
                        egui::Align2::LEFT_CENTER,
                        text.settings,
                        th.font(Role::Body),
                        fg,
                    );
                    if fresp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        out = Some(PopOut::Settings);
                    }
                });
        });
    let rect = area.response.rect;
    let outside = ctx.input(|i| {
        i.pointer.any_pressed()
            && i
                .pointer
                .interact_pos()
                .is_some_and(|p| !rect.contains(p) && !anchor.contains(p))
    });
    match out {
        Some(PopOut::Pick(i)) => {
            pick.click(i);
        }
        Some(PopOut::Settings) => pick.close(),
        None if outside => pick.close(),
        None => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Density, Mode};
    use crate::workspace::presets;
    use crate::{Catalog, Lang};

    fn names() -> Box<dyn Fn(&Workspace) -> String> {
        Box::new(|w: &Workspace| w.id.clone())
    }

    /// Every preset paints without panics; nothing is picked without a click.
    #[test]
    fn cards_paint_all_presets() {
        let ctx = egui::Context::default();
        let mut th = Theme::new(Mode::Dark, Density::Compact);
        let mut hit = None;
        let name_of = names();
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |c| {
                if th.apply(c) {
                    egui::CentralPanel::default().show(c, |ui| {
                        hit = grid(ui, &th, &presets(), "split", 3, &*name_of);
                    });
                }
            });
        }
        assert_eq!(hit, None);
    }

    #[test]
    fn picker_opens_on_current_and_toggles() {
        let mut p = Picker::default();
        p.toggle(3);
        assert_eq!(p, Picker { open: true, hi: 3 });
        p.toggle(3);
        assert!(!p.open);
        // keys do nothing while closed
        assert_eq!(p.key(PickKey::Accept, 5), PickOut::Stay);
        assert!(!p.open);
    }

    #[test]
    fn picker_arrows_wrap_and_enter_picks() {
        let mut p = Picker::default();
        p.toggle(0);
        assert_eq!(p.key(PickKey::Prev, 5), PickOut::Stay);
        assert_eq!(p.hi, 4, "up from the first row wraps to the last");
        assert_eq!(p.key(PickKey::Next, 5), PickOut::Stay);
        assert_eq!(p.hi, 0);
        p.key(PickKey::Next, 5);
        p.key(PickKey::Next, 5);
        assert_eq!(p.hi, 2);
        p.key(PickKey::Last, 5);
        assert_eq!(p.hi, 4);
        p.key(PickKey::First, 5);
        assert_eq!(p.hi, 0);
        p.key(PickKey::Next, 5);
        assert_eq!(p.key(PickKey::Accept, 5), PickOut::Picked(1));
        assert!(!p.open, "a pick closes");
    }

    #[test]
    fn picker_escape_and_click_close() {
        let mut p = Picker::default();
        p.toggle(2);
        p.key(PickKey::Next, 5);
        assert_eq!(p.key(PickKey::Cancel, 5), PickOut::Closed);
        assert!(!p.open);
        p.toggle(2);
        assert_eq!(p.hi, 2, "reopening starts at the current layout again");
        assert_eq!(p.click(4), PickOut::Picked(4));
        assert!(!p.open);
        // a stale highlight past a shrunken list is clamped, an empty list closes
        let mut q = Picker { open: true, hi: 9 };
        assert_eq!(q.key(PickKey::Accept, 5), PickOut::Picked(4));
        let mut e = Picker { open: true, hi: 0 };
        assert_eq!(e.key(PickKey::Next, 0), PickOut::Closed);
    }

    #[test]
    fn picker_keys_are_taken_in_order_and_only_plain() {
        let ctx = egui::Context::default();
        let key = |k, m: egui::Modifiers, pressed, repeat| egui::Event::Key {
            key: k,
            physical_key: Some(k),
            pressed,
            repeat,
            modifiers: m,
        };
        let none = egui::Modifiers::NONE;
        let input = egui::RawInput {
            events: vec![
                key(egui::Key::ArrowDown, none, true, false),
                key(egui::Key::ArrowDown, none, true, true),
                key(egui::Key::Enter, none, true, false),
                key(egui::Key::Enter, none, true, true),
                key(egui::Key::Escape, none, false, false),
                key(egui::Key::L, egui::Modifiers::CTRL, true, false),
                key(egui::Key::ArrowUp, egui::Modifiers::SHIFT, true, false),
            ],
            ..Default::default()
        };
        let mut got = vec![];
        let mut left = 0;
        let _ = ctx.run(input, |c| {
            got = take_picker_keys(c);
            left = c.input(|i| i.events.len());
        });
        assert_eq!(got, [PickKey::Next, PickKey::Next, PickKey::Accept]);
        assert_eq!(left, 2, "modified keys stay for the shortcut map");
    }

    fn segs<'a>(l: &'a [String; 3]) -> [Seg<'a>; 3] {
        let s = |i: usize, icon| Seg {
            icon,
            label: &l[i],
            title: &l[i],
            desc: "",
            keys: "",
        };
        [s(0, "plan-view"), s(1, "3d-view"), s(2, "layout-split")]
    }

    /// Real fonts, both languages, the longest preset name: at 1024 the group is icons only,
    /// from 1920 it has labels; never over the search field or the menus, always on screen.
    #[test]
    fn title_group_fits_common_widths() {
        let ctx = egui::Context::default();
        let mut th = Theme::new(Mode::Light, Density::Compact);
        // menus, undo and redo end about here on a real title bar
        let left_end = 360.0;
        let mut checked = 0;
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |c| {
                if !th.apply(c) {
                    return;
                }
                egui::CentralPanel::default().show(c, |ui| {
                    for lang in [Lang::En, Lang::Ru] {
                        let cat = Catalog::new(lang);
                        let l = ["workspace.view.plan", "workspace.view.3d", "workspace.view.split"]
                            .map(|k| cat.t(k));
                        let ws = group_widths(ui, &th, &segs(&l));
                        assert!(ws[0] > ws[1] && ws[1] >= ws[2], "{ws:?}");
                        for w in [1024.0_f32, 1280.0, 1920.0, 2560.0] {
                            let bar = Rect::from_min_max(pos2(10.0, 0.0), pos2(w - 10.0, 40.0));
                            let below = th.tokens.breakpoint.icons_below;
                            let f = title_fit(bar, left_end, ws, below, 28.0);
                            assert!(f.group.left() > left_end, "{w} {lang:?}");
                            assert!(f.group.right() <= bar.right() + 0.01);
                            assert!(f.group.width() + 0.01 >= ws[f.fit as usize]);
                            let s = f.search.expect("search shown");
                            assert!(s.right() + space::SM <= f.group.left(), "{w} {lang:?}");
                            assert!(s.left() >= left_end, "{w} {lang:?}");
                            if w <= 1024.0 {
                                assert_eq!(f.fit, Fit::Icons, "{lang:?}");
                            }
                            if w >= 1920.0 {
                                assert_eq!(f.fit, Fit::Full, "{lang:?}");
                            }
                            checked += 1;
                        }
                    }
                });
            });
        }
        assert!(checked >= 8);
    }

    #[test]
    fn title_fit_squeezes_search_before_overlapping() {
        let bar = Rect::from_min_max(pos2(0.0, 0.0), pos2(900.0, 40.0));
        // a huge icon group: the search moves left and shrinks, then disappears
        let f = title_fit(bar, 100.0, [600.0, 500.0, 330.0], 1100.0, 28.0);
        assert_eq!(f.fit, Fit::Icons);
        let s = f.search.unwrap();
        assert!(s.right() + space::LG <= f.group.left() + 0.01);
        assert!(s.left() >= 116.0 - 0.01);
        let g = title_fit(bar, 300.0, [600.0, 500.0, 450.0], 1100.0, 28.0);
        assert_eq!(g.search, None, "too narrow for a usable field");
    }

    #[test]
    fn popover_and_group_paint_headless() {
        let ctx = egui::Context::default();
        let mut icons = IconCache::default();
        for mode in [Mode::Light, Mode::Dark] {
            let mut th = Theme::new(mode, Density::Compact);
            let mut pick = Picker::default();
            pick.toggle(1);
            let list = presets();
            let cards: Vec<Card> = list
                .iter()
                .map(|w| Card {
                    ws: w,
                    name: &w.id,
                    purpose: &w.id,
                    keys: "Ctrl+Alt+1",
                })
                .collect();
            let text = PopText {
                title: "t",
                cycle: "c",
                cycle_keys: "F4",
                current: "cur",
                settings: "s",
            };
            let l = ["a".to_string(), "b".to_string(), "c".to_string()];
            let mut out = Some(PopOut::Settings);
            for _ in 0..3 {
                let _ = ctx.run(egui::RawInput::default(), |c| {
                    if !th.apply(c) {
                        return;
                    }
                    egui::CentralPanel::default().show(c, |ui| {
                        let b = LayoutBtn {
                            icon: "layout-ribbon",
                            title: "t",
                            desc: "",
                            keys: "F4",
                        };
                        for fit in [Fit::Full, Fit::Mid, Fit::Icons] {
                            let g = mode_group(ui, &th, &mut icons, &segs(&l), 0, &b, false, fit);
                            assert_eq!(g.view, None);
                        }
                    });
                    let anchor = Rect::from_min_size(pos2(900.0, 8.0), vec2(120.0, 24.0));
                    out = popover(c, &th, &mut icons, anchor, &cards, 1, &mut pick, &text);
                });
            }
            assert_eq!(out, None);
            assert!(pick.open, "painting alone never closes it");
        }
    }
}
