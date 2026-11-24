// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Layout contract of the tool dialogs, drawn headless at the class size of the dialog in both
//! languages (explicit catalog, the global language is never touched).
//!
//! Every interactive control must lie inside the window's inner rect (sideways always, and
//! vertically when it starts inside), and no control label may be elided. Controls come from the
//! accessibility tree the frame emits, so the probe needs no per-widget hooks.
//!
//! Form rows (`row_faults`, part of every check): a field's label and unit sit within a pixel
//! of the field's centre line, no number breaks over lines, no line of text is cut sideways by
//! its clip. `app_form_rows` runs them over the whole app with walls selected and the File menu
//! open, at 1280, 1440 and 1920 points in both languages.
//!
//! Covered: project parameters, phases, links, family editor, sources, drawing quality (all four
//! tabs), view templates, level question, clashes, AI agent access, AI changes (audit), legends,
//! open-source licenses, design options (empty, with sets, with a selection), replace type, type
//! properties (wall, member, long names), XLSX preview (also long names, 40 rows), the add-rebar
//! form (column, slab, mixed; all tabs), the history "revert all AI changes" confirmation, object
//! styles, materials, the 3D category window, the family parameter editor (both tabs) and the
//! floor, ceiling and shaft outline clean-up preview, the wall type editor and the wall type
//! library (list, import, purge, swap).
//! Dialogs known to break the contract carry `#[ignore = "<reason>"]`: un-ignore once fixed.

use crate::editor::Editor;
use crate::shell::EditorApp;
use ddc_ui::{Catalog, Density, Lang, Mode, Theme};
use egui::{self, accesskit};

/// Screen of the headless frames; dialogs open centred at their class size.
const SCREEN: (f32, f32) = (1600.0, 1000.0);
const EPS: f32 = 0.5;
/// Frames drawn so that sizes and scroll areas settle; the last one is measured.
const FRAMES: usize = 4;

/// One interactive control of the last frame.
#[derive(Clone, Debug)]
pub(crate) struct Control {
    pub role: accesskit::Role,
    pub label: String,
    pub rect: egui::Rect,
    /// The widget id as the accessibility tree names it; compares with the focused id.
    pub id: u64,
    /// Its clip hides it whole (a row scrolled out of its list): it shows nothing and is
    /// nobody's neighbour on screen.
    pub clipped_out: bool,
}

/// What the last frame of a dialog drew.
#[derive(Default)]
pub(crate) struct Probe {
    /// Window rect without the frame margin.
    pub inner: Option<egui::Rect>,
    pub controls: Vec<Control>,
    /// Text of every galley that was cut with an ellipsis.
    pub elided: Vec<String>,
    /// Text of every galley the last frame painted (labels included).
    pub text: Vec<String>,
    /// The same galleys with the rect each one occupies.
    pub spans: Vec<(String, egui::Rect)>,
    /// Every shape of the last frame, for paint checks (stroke lengths, colours).
    pub shapes: Vec<egui::Shape>,
    /// Bounds of every painted shape and the area it can show in.
    pub clips: Vec<(egui::Rect, egui::Rect)>,
    /// The screen of the frame.
    pub screen: Option<egui::Rect>,
    /// Labels of nodes whose `labelled_by` names a node the frame did not emit.
    pub dangling: Vec<String>,
    /// Every painted galley with its row count and the area it can show in.
    pub lines: Vec<Line>,
    /// Drawing areas (plan, section, 3D): their text is model text, cut by the view edge.
    pub canvas: Vec<egui::Rect>,
    /// Top layer at each control's centre (same order as `controls`); empty when not measured.
    pub ctl_top: Vec<Option<egui::LayerId>>,
    /// A person's own names drawn with a whole-text tooltip (`ui_kit::user_text_tip`): the
    /// only captions an ellipsis may cut.
    pub user: Vec<String>,
    /// Viewports of the kit scroll areas (`ui_kit::mark_scroll_viewport`) the frame drew.
    pub scrolls: Vec<egui::Rect>,
    /// Viewports of sideways scrolling areas (`ui_kit::mark_hscroll_viewport`).
    pub hscrolls: Vec<egui::Rect>,
}

/// One painted galley of the last frame.
#[derive(Clone, Debug)]
pub(crate) struct Line {
    pub text: String,
    pub rect: egui::Rect,
    /// Rows the galley broke into (1 for a single line).
    pub rows: usize,
    /// The area the galley's container lays out in (its clip less egui's clip margin).
    pub clip: egui::Rect,
    /// The clip itself: pixels outside it are cut.
    pub view: egui::Rect,
    /// Where the eye reads the first row: half a capital above its baseline, from the glyphs
    /// as painted (a galley's box carries line gap and descent, its middle is not the text's).
    pub mid: f32,
    /// Top layer at the text's centre once the frame is done.
    pub top: Option<egui::LayerId>,
    /// An opaque fill painted after it covers the text's centre (a dialog or popup over it, a
    /// widget's background): nobody sees it.
    pub hidden: bool,
}

/// Opaque filled rectangles of `shape` (a window, popup or widget background), within `clip`.
fn opaque_fills(shape: &egui::Shape, clip: egui::Rect, out: &mut Vec<egui::Rect>) {
    match shape {
        egui::Shape::Rect(r) if r.fill.a() == 255 => out.push(r.rect.intersect(clip)),
        egui::Shape::Vec(v) => v.iter().for_each(|s| opaque_fills(s, clip, out)),
        _ => {}
    }
}

/// Visual centre line of a galley's first row painted at `pos`: baseline less half the cap
/// height (Inter and the CJK fallbacks: caps 0.75 of the ascent).
pub(crate) fn text_mid(g: &egui::Galley, pos: egui::Pos2) -> f32 {
    let Some(row) = g.rows.first() else { return pos.y };
    let Some(gl) = row.row.glyphs.iter().find(|x| !x.chr.is_whitespace()) else {
        return pos.y + row.pos.y + row.row.size.y / 2.0;
    };
    pos.y + row.pos.y + gl.pos.y - 0.375 * gl.font_ascent
}

fn lines(s: &egui::Shape, (clip, view): (egui::Rect, egui::Rect), out: &mut Vec<Line>) {
    match s {
        egui::Shape::Text(t) if !t.galley.text().trim().is_empty() => out.push(Line {
            text: t.galley.text().to_string(),
            // a centred or right-aligned galley starts left of its anchor
            rect: t.galley.rect.translate(t.pos.to_vec2()),
            rows: t.galley.rows.len(),
            clip,
            view,
            mid: text_mid(&t.galley, t.pos),
            top: None,
            hidden: false,
        }),
        egui::Shape::Vec(v) => v.iter().for_each(|x| lines(x, (clip, view), out)),
        _ => {}
    }
}

pub(crate) fn font_ctx() -> egui::Context {
    ddc_ui::theme::headless_ctx()
}

fn is_control(role: accesskit::Role) -> bool {
    use accesskit::Role;
    matches!(
        role,
        Role::Button
            | Role::TextInput
            | Role::MultilineTextInput
            | Role::CheckBox
            | Role::RadioButton
            | Role::ComboBox
            | Role::Slider
            | Role::SpinButton
            | Role::Link
            | Role::Tab
    )
}

fn texts(s: &egui::Shape, out: &mut Vec<String>) {
    match s {
        egui::Shape::Text(t) if t.galley.elided => out.push(t.galley.text().to_string()),
        egui::Shape::Vec(v) => v.iter().for_each(|x| texts(x, out)),
        _ => {}
    }
}

fn all_texts(s: &egui::Shape, out: &mut Vec<String>) {
    match s {
        egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
        egui::Shape::Vec(v) => v.iter().for_each(|x| all_texts(x, out)),
        _ => {}
    }
}

fn spans(s: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
    match s {
        egui::Shape::Text(t) => {
            let r = egui::Rect::from_min_size(t.pos, t.galley.size());
            out.push((t.galley.text().to_string(), r));
        }
        egui::Shape::Vec(v) => v.iter().for_each(|x| spans(x, out)),
        _ => {}
    }
}

/// Nodes whose `labelled_by` points at a node missing from the tree, by their label.
fn dangling_labels(nodes: &[(accesskit::NodeId, accesskit::Node)]) -> Vec<String> {
    let ids: std::collections::HashSet<_> = nodes.iter().map(|(id, _)| *id).collect();
    nodes
        .iter()
        .filter(|(_, n)| n.labelled_by().iter().any(|l| !ids.contains(l)))
        .map(|(_, n)| format!("{:?} {:?}", n.role(), n.label().unwrap_or("")))
        .collect()
}

/// Draws `frame` for several passes with the dialog `key` and returns what the last pass drew.
pub(crate) fn probe(key: &str, frame: impl FnMut(&egui::Context)) -> Probe {
    probe_id(egui::Id::new(("ddc.dialog", key)), frame)
}

/// As `probe`, for a dialog that sets its own window id.
pub(crate) fn probe_id(id: egui::Id, frame: impl FnMut(&egui::Context)) -> Probe {
    probe_on(id, SCREEN, frame)
}

/// As `probe_id`, on a screen of `screen` points.
pub(crate) fn probe_on(
    id: egui::Id,
    screen: (f32, f32),
    mut frame: impl FnMut(&egui::Context),
) -> Probe {
    let ctx = font_ctx();
    ctx.enable_accesskit();
    let mut th = Theme::new(Mode::Dark, Density::Compact);
    let mut seen = Probe::default();
    for _ in 0..FRAMES {
        seen = pass_on(&ctx, &mut th, id, screen, vec![], &mut frame);
    }
    seen
}

/// A context for driving `pass` by hand (input between passes): fonts installed, accesskit on.
pub(crate) fn driven_ctx() -> (egui::Context, Theme) {
    let ctx = font_ctx();
    ctx.enable_accesskit();
    (ctx, Theme::new(Mode::Dark, Density::Compact))
}

/// One frame with `events` as input; what it drew, as `probe_id` reports for its last pass.
pub(crate) fn pass(
    ctx: &egui::Context,
    th: &mut Theme,
    id: egui::Id,
    events: Vec<egui::Event>,
    frame: &mut dyn FnMut(&egui::Context),
) -> Probe {
    pass_on(ctx, th, id, SCREEN, events, frame)
}

/// As `pass`, on a screen of `screen` points.
pub(crate) fn pass_on(
    ctx: &egui::Context,
    th: &mut Theme,
    id: egui::Id,
    screen: (f32, f32),
    events: Vec<egui::Event>,
    frame: &mut dyn FnMut(&egui::Context),
) -> Probe {
    let mut seen;
    {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(screen.0, screen.1));
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            events,
            // no clock: every pass advances time by exactly this step, on any machine
            predicted_dt: 1.0 / 60.0,
            ..egui::RawInput::default()
        };
        ctx.data_mut(|d| d.remove::<Vec<String>>(crate::ui_kit::user_text_id()));
        ctx.data_mut(|d| d.remove::<Vec<egui::Rect>>(crate::ui_kit::scroll_viewport_id()));
        ctx.data_mut(|d| d.remove::<Vec<egui::Rect>>(crate::ui_kit::hscroll_viewport_id()));
        let out = ctx.run(raw, |ctx| {
            let _ = th.apply(ctx);
            frame(ctx);
        });
        let m = ctx.style().spacing.window_margin;
        seen = Probe::default();
        seen.screen = Some(screen);
        seen.user = ctx.data(|d| d.get_temp::<Vec<String>>(crate::ui_kit::user_text_id()))
            .unwrap_or_default();
        seen.scrolls = ctx
            .data(|d| d.get_temp::<Vec<egui::Rect>>(crate::ui_kit::scroll_viewport_id()))
            .unwrap_or_default();
        seen.hscrolls = ctx
            .data(|d| d.get_temp::<Vec<egui::Rect>>(crate::ui_kit::hscroll_viewport_id()))
            .unwrap_or_default();
        seen.inner = egui::AreaState::load(ctx, id).map(|a| {
            let r = a.rect();
            egui::Rect::from_min_max(
                r.min + egui::vec2(f32::from(m.left), f32::from(m.top)),
                r.max - egui::vec2(f32::from(m.right), f32::from(m.bottom)),
            )
        });
        // widgets whose interact rect (the rect within its clip) is empty, by accessibility id
        let gone: std::collections::HashSet<u64> = ctx.viewport(|v| {
            let w = &v.prev_pass.widgets;
            w.layer_ids()
                .flat_map(|l| w.get_layer(l))
                .filter(|r| !r.interact_rect.is_positive())
                .map(|r| r.id.value())
                .collect()
        });
        if let Some(update) = out.platform_output.accesskit_update {
            seen.dangling = dangling_labels(&update.nodes);
            for (node_id, node) in update.nodes {
                if let (Some(b), accesskit::Role::Canvas) = (node.bounds(), node.role()) {
                    let (min, max) = ((b.x0 as f32, b.y0 as f32), (b.x1 as f32, b.y1 as f32));
                    seen.canvas.push(egui::Rect::from_min_max(min.into(), max.into()));
                }
                let (Some(b), true) = (node.bounds(), is_control(node.role())) else {
                    continue;
                };
                let rect = egui::Rect::from_min_max(
                    egui::pos2(b.x0 as f32, b.y0 as f32),
                    egui::pos2(b.x1 as f32, b.y1 as f32),
                );
                if rect.width() < 1.0 || rect.height() < 1.0 {
                    continue;
                }
                let label = node.label().or_else(|| node.value()).unwrap_or("").to_string();
                let clipped_out = gone.contains(&node_id.0);
                seen.controls.push(Control { role: node.role(), label, rect, id: node_id.0, clipped_out });
            }
        }
        // egui widens every clip by this margin; without it the clip is the viewport itself
        let margin = ctx.style().visuals.clip_rect_margin;
        // opaque fills in paint order, each with the count of texts painted before it
        let mut fills: Vec<(usize, egui::Rect)> = vec![];
        for c in &out.shapes {
            let before = seen.lines.len();
            texts(&c.shape, &mut seen.elided);
            all_texts(&c.shape, &mut seen.text);
            spans(&c.shape, &mut seen.spans);
            lines(&c.shape, (c.clip_rect.shrink(margin), c.clip_rect), &mut seen.lines);
            let mut f = vec![];
            opaque_fills(&c.shape, c.clip_rect, &mut f);
            fills.extend(f.into_iter().filter(|r| r.is_positive()).map(|r| (before, r)));
            seen.shapes.push(c.shape.clone());
            seen.clips.push((c.shape.visual_bounding_rect(), c.clip_rect.shrink(margin)));
        }
        // the shapes come in paint order, layers included: a text under an opaque fill painted
        // later (a dialog over a panel) does not show and belongs to no row there
        for (i, l) in seen.lines.iter_mut().enumerate() {
            let c = l.rect.center();
            l.top = ctx.layer_id_at(c);
            l.hidden = fills.iter().any(|(n, r)| *n > i && r.contains(c));
        }
        seen.ctl_top = seen.controls.iter().map(|c| ctx.layer_id_at(c.rect.center())).collect();
    }
    seen
}

/// The part of control `r` its own shapes can show, None when it paints nothing. A list row
/// half out of its scroll viewport shows only what is inside; the rest scrolls into view. The
/// tightest clip among the shapes inside `r` rules: a row scrolled under the title bar must not
/// borrow the title text's wide clip. The result is empty (not positive) when nothing shows.
fn shown_part(p: &Probe, r: egui::Rect) -> Option<egui::Rect> {
    let own = r.expand(1.0);
    p.clips
        .iter()
        .filter(|(b, _)| b.is_positive() && own.contains_rect(*b))
        .map(|(_, clip)| *clip)
        .reduce(|a, b| a.intersect(b))
        .map(|clip| r.intersect(clip))
}

/// The clip of the scroll viewport that holds `r`: the shapes inside `r` are cut by a clip
/// that ends before `r` does. None when `r` is not inside such a viewport.
pub(crate) fn viewport_clip(p: &Probe, r: egui::Rect) -> Option<egui::Rect> {
    let own = r.expand(1.0);
    let clip = p
        .clips
        .iter()
        .filter(|(b, _)| b.is_positive() && own.contains_rect(*b))
        .map(|(_, clip)| *clip)
        .reduce(|a, b| a.intersect(b))?;
    (clip.height() < r.height() - 1.0 || clip.bottom() < r.bottom() - 1.0).then_some(clip)
}

/// A scroll viewport is on screen: some control is shown only in part.
fn has_scroll_viewport(p: &Probe) -> bool {
    let part = p.controls.iter().any(|c| {
        shown_part(p, c.rect).is_some_and(|v| v.is_positive() && v.height() < c.rect.height() - 1.0)
    });
    // rows can end exactly on the viewport edge, so the next row shows nothing: only a clip that
    // is a recorded scroll viewport may hide it; any other nested clip that cuts a row is a fault
    part || p.controls.iter().any(|c| viewport_clip(p, c.rect).is_some_and(|k| is_scroll_clip(p, k)))
}

/// `clip` is the content clip of a recorded scroll viewport: it lies within it vertically, ends
/// on its top or bottom edge and overlaps it sideways (egui keeps the parent's sides).
fn is_scroll_clip(p: &Probe, clip: egui::Rect) -> bool {
    p.scrolls.iter().any(|v| {
        let within = clip.top() >= v.top() - 1.0 && clip.bottom() <= v.bottom() + 1.0;
        let edge = (clip.top() - v.top()).abs() <= 1.0 || (clip.bottom() - v.bottom()).abs() <= 1.0;
        within && edge && clip.left() < v.right() && clip.right() > v.left()
    })
}

/// Violations of the contract; empty when the dialog keeps it. With `strict` any elided text
/// counts, otherwise only the text of a control (buttons, tabs, fields).
pub(crate) fn violations(p: &Probe, strict: bool) -> Vec<String> {
    let mut bad = vec![];
    let Some(win) = p.inner else {
        return vec!["the window was not drawn".to_string()];
    };
    if p.controls.is_empty() {
        bad.push("no interactive control was drawn".to_string());
    }
    let scrolled = has_scroll_viewport(p);
    for c in &p.controls {
        let r = c.rect;
        let name = format!("{:?} {:?} {:?}", c.role, c.label, r);
        if r.left() < win.left() - EPS || r.right() > win.right() + EPS {
            bad.push(format!("{name} leaves the window sideways {win:?}"));
        }
        // a window as wide as its widest row holds that row and still ends past the screen
        if p.screen.is_some_and(|s| r.left() < s.left() - EPS || r.right() > s.right() + EPS) {
            bad.push(format!("{name} leaves the screen"));
        }
        // a control scrolled out of its body is not a fault; only what it shows must fit
        let Some(v) = shown_part(p, r) else {
            continue;
        };
        if !v.is_positive() {
            // nothing of it shows: scrolled out of a list, or a footer pushed under the window
            // edge; without a list on screen that is a cut footer
            if !scrolled && r.top() < win.bottom() - EPS && r.bottom() > win.bottom() + EPS {
                bad.push(format!("{name} leaves the window vertically {win:?}"));
            }
            continue;
        }
        let starts_inside = v.top() < win.bottom() - EPS && v.bottom() > win.top() + EPS;
        if starts_inside && (v.top() < win.top() - EPS || v.bottom() > win.bottom() + EPS) {
            bad.push(format!("{name} leaves the window vertically {win:?}"));
        }
    }
    for d in &p.dangling {
        bad.push(format!("{d} is labelled by a node that does not exist"));
    }
    for e in &p.elided {
        // a person's own name with its tooltip may be cut; an interface caption never
        if p.user.iter().any(|u| u.trim() == e.trim()) {
            continue;
        }
        let own = p.controls.iter().any(|c| c.label.trim() == e.trim());
        if strict || own {
            bad.push(format!("text {e:?} is elided"));
        }
    }
    bad.extend(row_faults(p, false));
    bad
}

/// Farthest a field's label may stand left of it, and its unit right of it, in points.
const LABEL_GAP: f32 = 240.0;
const UNIT_GAP: f32 = 24.0;
/// Tallest control measured as one line; taller ones are cards and list rows.
const MAX_ROW_H: f32 = 40.0;

/// A control's own text: painted in its clip or in a tighter clip inside it.
fn same_clip_or_inside(l: &Line, clip: egui::Rect) -> bool {
    same_clip(l.clip, clip) || clip.expand(1.0).contains_rect(l.clip)
}

/// The area a control's own shapes show in: the widest clip among them (a field's text is
/// clipped tighter than its frame), `None` when it paints nothing.
fn control_clip(p: &Probe, r: egui::Rect) -> Option<egui::Rect> {
    let own = r.expand(1.0);
    p.clips
        .iter()
        .filter(|(b, _)| b.is_positive() && own.contains_rect(*b))
        .map(|(_, clip)| *clip)
        .reduce(|a, b| a.union(b))
}

fn same_clip(a: egui::Rect, b: egui::Rect) -> bool {
    (a.min - b.min).length() <= 1.0 && (a.max - b.max).length() <= 1.0
}

/// Share of the shorter of two vertical spans that they have in common.
fn v_overlap(a: egui::Rect, b: egui::Rect) -> f32 {
    let common = a.bottom().min(b.bottom()) - a.top().max(b.top());
    common.max(0.0) / a.height().min(b.height()).max(1.0)
}

/// Form-row faults of a frame: a field whose label or unit sits off its centre line by more
/// than a pixel, a number broken over lines or cut sideways by its clip; with `all_text` any
/// line of text cut sideways (the form surfaces; lists elsewhere keep their own elide checks).
pub(crate) fn row_faults(p: &Probe, all_text: bool) -> Vec<String> {
    use accesskit::Role;
    let mut bad = vec![];
    let in_control =
        |r: egui::Rect| p.controls.iter().any(|c| c.rect.expand(1.0).contains(r.center()));
    // a wrapped label is read by its first row
    let first = |l: &Line| {
        let half = if l.rows == 1 { l.rect.height() / 2.0 } else { 6.0 };
        egui::Rect::from_x_y_ranges(l.rect.x_range(), (l.mid - half)..=(l.mid + half))
    };
    // text a higher layer covers is nobody's label: the eye sees the cover there
    let free: Vec<&Line> = p.lines.iter().filter(|l| !l.hidden && !in_control(l.rect)).collect();
    for (i, c) in p.controls.iter().enumerate() {
        let r = c.rect;
        // only text shown on the control's own layer: a window under a dialog is hidden
        let top = p.ctl_top.get(i).copied().flatten();
        let shown = |l: &Line| !l.hidden && (top.is_none() || l.top.is_none() || l.top == top);
        // one-line controls only: list rows and cards hold several lines of their own
        let field = matches!(c.role, Role::TextInput | Role::ComboBox | Role::SpinButton);
        // toggles and sliders of a form row; buttons hold icons over captions (ribbon
        // tiles) and steppers inside fields, so they are measured by their own contract
        let line_ctl = matches!(c.role, Role::CheckBox | Role::RadioButton | Role::Slider);
        if !(field || line_ctl) || r.height() > MAX_ROW_H || c.clipped_out {
            continue;
        }
        let Some(clip) = control_clip(p, r) else { continue };
        if !r.intersects(clip) {
            continue;
        }
        // the control's own text (a field's value, a button's caption) is where the eye reads
        // the control's line; a box with no text is read at its middle
        let own = p.lines.iter().find(|l| {
            let inside = r.expand(1.0).contains_rect(l.rect);
            l.rows == 1 && shown(l) && same_clip_or_inside(l, clip) && inside
        });
        let line_y = own.map_or(r.center().y, |l| l.mid);
        if let Some(o) = own {
            let dy = (o.mid - r.center().y).abs();
            if dy > 1.0 && !matches!(c.role, Role::CheckBox | Role::RadioButton) {
                let (t, role) = (&o.text, c.role);
                bad.push(format!("text {t:?} is {dy:.1} px off the middle of its {role:?}"));
            }
        }
        let beside =
            |l: &&&Line| shown(l) && same_clip(l.clip, clip) && v_overlap(first(l), r) >= 0.5;
        // nothing interactive and no other text between the text and the control
        let clear = |a: f32, b: f32| {
            let ctl = p.controls.iter().any(|o| {
                let between = o.rect.left() >= a && o.rect.right() <= b;
                o.rect != r && v_overlap(o.rect, r) >= 0.5 && between
            });
            let txt = free.iter().any(|l| {
                l.rect.left() >= a + 1.0 && l.rect.right() <= b - 1.0 && v_overlap(first(l), r) >= 0.5
            });
            !(ctl || txt)
        };
        let gap = if field { f32::INFINITY } else { LABEL_GAP };
        let label = free
            .iter()
            .filter(beside)
            .filter(|l| l.rect.right() <= r.left() + 1.0 && r.left() - l.rect.right() <= gap)
            .filter(|l| clear(l.rect.right() - 1.0, r.left() + 1.0))
            .max_by(|a, b| a.rect.right().total_cmp(&b.rect.right()));
        let unit = free
            .iter()
            .filter(beside)
            // a unit is a known designation from the kit's unit table, not any short word
            .filter(|l| l.rows == 1 && crate::ui_kit::is_unit_token(l.text.trim()))
            .filter(|l| l.rect.left() >= r.right() - 1.0 && l.rect.left() - r.right() <= UNIT_GAP)
            .min_by(|a, b| a.rect.left().total_cmp(&b.rect.left()));
        for (what, l) in [("label", label), ("unit", unit)] {
            let Some(l) = l else { continue };
            let dy = (l.mid - line_y).abs();
            if dy > 1.0 {
                let (t, role, name) = (&l.text, c.role, &c.label);
                bad.push(format!("{what} {t:?} is {dy:.1} px off the line of {role:?} {name:?}"));
            }
        }
    }
    // a text field scrolls what is typed in it; model text on a drawing is cut by the view edge
    let scrolls = |l: &Line| {
        let c = l.rect.center();
        let typed = p.controls.iter().any(|f| {
            let field = matches!(f.role, Role::TextInput | Role::MultilineTextInput);
            field && f.rect.expand(1.0).contains(c)
        });
        let drawn = p.canvas.iter().any(|v| v.contains(c) && l.view.contains_rect(v.shrink(1.0)));
        // a wide table scrolls sideways: text cut at the side of its viewport scrolls into view
        // (the clip less egui's margin is what lines up with the viewport). egui clips a
        // sideways area only at its sides, the clip keeps its parent's top and bottom (a totals
        // row under the rows' own vertical scroll): the text itself lies within the viewport
        let (v, k) = (l.view, l.clip);
        let wide = p.hscrolls.iter().any(|h| {
            let within = l.rect.top() >= h.top() - 1.0 && l.rect.bottom() <= h.bottom() + 1.0;
            let at_left = l.rect.left() < v.left() - 1.0 && (k.left() - h.left()).abs() <= 1.0;
            let at_right = l.rect.right() > v.right() + 1.0 && (k.right() - h.right()).abs() <= 1.0;
            within && (at_left || at_right)
        });
        typed || drawn || wide
    };
    for l in &p.lines {
        let number = ddc_ui::widgets::form::is_numeric_text(&l.text);
        if l.rows > 1 && !l.text.contains('\n') && number {
            bad.push(format!("number {:?} breaks over {} lines", l.text, l.rows));
        }
        if !(number || all_text) || l.hidden {
            continue;
        }
        let v = l.view;
        let shown = l.rect.intersects(v);
        let v_inside = l.rect.top() >= v.top() - 1.0 && l.rect.bottom() <= v.bottom() + 1.0;
        let cut = l.rect.left() < v.left() - 1.0 || l.rect.right() > v.right() + 1.0;
        if l.rows == 1 && shown && v_inside && cut && !scrolls(l) {
            bad.push(format!("text {:?} is cut sideways by its clip", l.text));
        }
    }
    bad
}

/// What a failing probe drew, one `DUMP` line per item (window, controls, one-line texts), so
/// a log names the rects a fault came from.
pub(crate) fn dump(tag: &str, p: &Probe, bad: &[String]) -> String {
    let cut = |s: &str| s.chars().take(40).collect::<String>();
    // what the faults name, and what lies on or inside it, first; the rest fills up to the cap
    let all = bad.join("\n");
    let named: Vec<egui::Rect> = (p.controls.iter().map(|c| (&c.label, c.rect)))
        .chain(p.lines.iter().map(|l| (&l.text, l.rect)))
        .filter(|(t, _)| !t.is_empty() && all.contains(&format!("{t:?}")))
        .map(|(_, r)| r.expand(4.0))
        .collect();
    let near = |r: egui::Rect| named.iter().any(|n| n.intersects(r));
    let mut out = vec![format!(
        "DUMP {tag} window {:?} screen {:?} scrolls {:?} hscrolls {:?}",
        p.inner, p.screen, p.scrolls, p.hscrolls
    )];
    let (mut ctl, rest): (Vec<_>, Vec<_>) = p.controls.iter().partition(|c| near(c.rect));
    ctl.extend(rest.into_iter().take(20));
    for c in ctl.into_iter().take(80) {
        out.push(format!("DUMP {tag} ctl {:?} {:?} {:?}", c.role, cut(&c.label), c.rect));
    }
    let (mut lines, rest): (Vec<_>, Vec<_>) = p.lines.iter().partition(|l| near(l.rect));
    lines.extend(rest.into_iter().take(20));
    for l in lines.into_iter().take(80) {
        let (t, r, rows, mid, clip) = (cut(&l.text), l.rect, l.rows, l.mid, l.clip);
        let (view, hidden) = (l.view, l.hidden);
        out.push(format!(
            "DUMP {tag} line {t:?} {r:?} rows {rows} mid {mid:.1} clip {clip:?} view {view:?} hidden {hidden}"
        ));
    }
    out.join("\n")
}

pub(crate) fn assert_contract(tag: &str, p: &Probe, strict: bool) {
    let bad = violations(p, strict);
    if !bad.is_empty() {
        eprintln!("{}", dump(tag, p, &bad));
    }
    assert!(bad.is_empty(), "{tag}: {}", bad.join("; "));
}

pub(crate) fn app_in(lang: Lang) -> EditorApp {
    let ed = Editor::new_in_memory().unwrap();
    let mut app = EditorApp::new(ed, None, "test".into());
    app.sh.cat = Catalog::new(lang);
    app
}

fn project_parameters(ru: bool) {
    let mut ed = Editor::new_in_memory().unwrap();
    let mut toasts = ddc_ui::notify::Toasts::default();
    crate::params_ui::open_window();
    let p = probe("project_parameters", |ctx| {
        crate::params_ui::window(ctx, &mut ed, &mut toasts, ru)
    });
    assert_contract(if ru { "project_parameters ru" } else { "project_parameters en" }, &p, false);
}

fn schedules_multi(ru: bool) {
    let mut ed = Editor::new_in_memory().unwrap();
    crate::schedule::test_room(&mut ed);
    let def = crate::schedule::multi::MultiDef {
        name: "Walls".into(),
        cats: vec![crate::schedule::Cat::Walls, crate::schedule::Cat::Doors],
        columns: crate::schedule::multi::parse_columns("id; len; h; Twice = h * 2"),
        totals: true,
        ..Default::default()
    };
    crate::schedule::multi::save(&mut ed, &[def]).unwrap();
    crate::schedule::multi_ui::open_window();
    let p = probe("schedules_multi", |ctx| crate::schedule::multi_ui::window(ctx, &mut ed, ru));
    assert_contract(if ru { "schedules_multi ru" } else { "schedules_multi en" }, &p, false);
}

fn phases(ru: bool) {
    let mut ed = Editor::new_in_memory().unwrap();
    crate::schedule::test_room(&mut ed);
    crate::phase_ui::open_window();
    let p = probe("phases", |ctx| crate::phase_ui::window(ctx, &mut ed, ru));
    assert_contract(if ru { "phases ru" } else { "phases en" }, &p, false);
}

fn links(lang: Lang) {
    let mut app = app_in(lang);
    app.links.open = true;
    let p = probe("links", |ctx| crate::links_ui::frame(&mut app, ctx));
    assert_contract(&format!("links {lang:?}"), &p, false);
}

/// Linked models with `n` long paths: empty (0), one, many. One Add control, no path field,
/// the list fills the window down to the footer.
fn links_list(lang: Lang, n: usize) {
    let ru = matches!(lang, Lang::Ru);
    let mut app = app_in(lang);
    app.links.open = true;
    let dir = "C:/Projects/Very Long Customer Name/Building Complex/Structure/Revisions/2026";
    let paths: Vec<String> = (0..n).map(|i| format!("{dir}/Tower_{i}_architecture.ifc")).collect();
    crate::links_ui::test_list(&mut app, &paths);
    let p = probe("links", |ctx| crate::links_ui::layout_frame(&mut app, ctx));
    let tag = format!("links {lang:?} n={n}");
    assert_contract(&tag, &p, false);
    let add = crate::tr_lang(ru, "links.add_pick", &[]);
    let adds: Vec<&Control> = p.controls.iter().filter(|c| c.label == add).collect();
    assert_eq!(adds.len(), 1, "{tag}: one Add control, got {:?}", p.controls);
    let win = p.inner.unwrap();
    let btn = adds[0].rect;
    if n == 0 {
        assert!(
            p.controls.iter().all(|c| c.role != accesskit::Role::TextInput),
            "{tag}: no path field"
        );
        assert!(win.bottom() - btn.bottom() <= 80.0, "{tag}: hole under the action");
    } else {
        // rows scrolled out of the list are below the footer by position only
        let lowest = p
            .controls
            .iter()
            .filter_map(|c| shown_part(&p, c.rect))
            .map(|v| v.bottom())
            .fold(0.0, f32::max);
        assert!((lowest - btn.bottom()).abs() < 1.0, "{tag}: Add is the footer");
        assert!(win.bottom() - btn.bottom() <= 24.0, "{tag}: footer sits at the bottom");
        assert!(btn.right() > win.right() - 24.0, "{tag}: Add is right-aligned");
    }
}

#[test]
fn links_lists_en() {
    for n in [0, 1, 8] {
        links_list(Lang::En, n);
    }
}

#[test]
fn links_lists_ru() {
    for n in [0, 1, 8] {
        links_list(Lang::Ru, n);
    }
}

fn circuit_editor(lang: Lang) {
    let mut app = app_in(lang);
    crate::circuit_edit::open_for_layout(&mut app);
    let p = probe("mep.circuits", |ctx| crate::circuit_edit::window(&mut app, ctx));
    assert_contract(&format!("circuit_edit {lang:?}"), &p, false);
    let cat = Catalog::new(lang);
    for k in ["ce.f.cos", "ce.f.laying", "ce.f.ambient", "ce.f.grouped", "ce.f.breaker", "ce.f.cable", "ce.apply", "ce.autosize"] {
        assert!(has_text(&p, &cat.t(k)), "circuit_edit {lang:?}: {k} not drawn");
    }
}

fn sources(lang: Lang) {
    let mut app = app_in(lang);
    app.src.panel = true;
    let p = probe("sources", |ctx| crate::source_ui::frame(&mut app, ctx));
    assert_contract(&format!("sources {lang:?}"), &p, false);
}

fn clashes(lang: Lang) {
    let mut app = app_in(lang);
    app.clash.open = true;
    let p = probe("clash", |ctx| crate::shell::clash_win::layout_frame(&mut app, ctx));
    assert_contract(&format!("clashes {lang:?}"), &p, false);
}

fn issues(lang: Lang, form: bool) {
    let mut app = app_in(lang);
    let p = probe(if form { "ag4.issue_new" } else { "issues.issues" }, |ctx| crate::issues_ui::layout_frame(&mut app, ctx, form));
    assert_contract(&format!("issues {lang:?} form {form}"), &p, false);
}

#[test]
fn issues_list_en_ru() {
    issues(Lang::En, false);
    issues(Lang::Ru, false);
}

#[test]
fn issues_form_en_ru() {
    issues(Lang::En, true);
    issues(Lang::Ru, true);
}

fn xlsx_preview(lang: Lang) {
    use crate::shell::xlsx_win::XlsxPreview;
    let mut app = app_in(lang);
    let path = std::env::temp_dir().join(format!("ddc_layout_xl_{}_{:?}.xlsx", std::process::id(), lang));
    ddc_io::xlsx::export_xlsx_units(&app.ed.model, &path, ddc_io::xlsx::Units::Metric).unwrap();
    let mut pv = XlsxPreview::load(&mut app.ed.model, &path, true).unwrap();
    // load() of a book equal to the model shows the linked card, not this window
    pv.synced = false;
    for (i, kind) in ["created", "deleted", "1"].into_iter().enumerate() {
        pv.report.changes.push(ddc_io::xlsx::Change {
            sheet: "Walls".into(),
            row: 2 + i as u32,
            col: 0,
            column: "id".into(),
            id: Some(i as i64 + 90),
            old: String::new(),
            new: kind.into(),
        });
    }
    app.xlsx = Some(pv);
    let p = probe("xlsx_preview", |ctx| crate::shell::xlsx_win::layout_frame(&mut app, ctx));
    assert_contract(&format!("xlsx_preview {lang:?}"), &p, false);
}

/// A short list keeps the footer on the bottom edge (the table takes the room, no band
/// under the buttons); a book with no comparison data shows no zero "deleted" chip.
fn xlsx_preview_footer(lang: Lang) {
    use crate::shell::xlsx_win::XlsxPreview;
    let mut app = app_in(lang);
    let name = format!("ddc_layout_xlf_{}_{:?}.xlsx", std::process::id(), lang);
    let path = std::env::temp_dir().join(name);
    ddc_io::xlsx::export_xlsx_units(&app.ed.model, &path, ddc_io::xlsx::Units::Metric).unwrap();
    let mut pv = XlsxPreview::load(&mut app.ed.model, &path, true).unwrap();
    pv.synced = false;
    pv.report.no_base = true;
    for (i, kind) in ["created", "1"].into_iter().enumerate() {
        pv.report.changes.push(ddc_io::xlsx::Change {
            sheet: "Walls".into(),
            row: 2 + i as u32,
            col: 0,
            column: "id".into(),
            id: Some(i as i64 + 90),
            old: String::new(),
            new: kind.into(),
        });
    }
    app.xlsx = Some(pv);
    let ru = lang == Lang::Ru;
    let (cancel, deleted) = (crate::tr_str(ru, "xlw.cancel"), crate::tr_str(ru, "xlw.deleted"));
    let id = egui::Id::new(("ddc.dialog", "xlsx_preview"));
    let (ctx, mut th) = driven_ctx();
    let mut frame = |c: &egui::Context| crate::shell::xlsx_win::layout_frame(&mut app, c);
    let mut p = Probe::default();
    for _ in 0..FRAMES * 2 {
        p = pass_on(&ctx, &mut th, id, SCREEN, vec![], &mut frame);
    }
    let tag = format!("xlsx_preview footer {lang:?}");
    let win = p.inner.unwrap_or_else(|| panic!("{tag}: window not drawn"));
    let foot = named(&p, cancel).unwrap_or_else(|| panic!("{tag}: no {cancel:?}")).rect;
    let under = win.bottom() - foot.bottom();
    assert!(under <= 2.0 * th.d().row, "{tag}: {under} px empty under the buttons");
    assert!(under >= -EPS, "{tag}: the footer left the window");
    assert!(!has_text(&p, deleted), "{tag}: a zero deleted chip beside not removed");
}

/// The site properties window shows the coordinate system rows and a hint button that
/// names the code; both languages fit the contract.
fn site_crs(lang: Lang) {
    let mut app = app_in(lang);
    app.ed.topo.window_open = true;
    let mut s = ddc_model::SiteProps::default();
    s.latitude = 48.1;
    s.longitude = 11.6;
    app.ed.model.set_site(&s).unwrap();
    let p = probe("topo_site", |ctx| crate::topo::window::window(&mut app, ctx));
    let tag = format!("site_crs {lang:?}");
    assert_contract(&tag, &p, false);
    assert!(
        p.text.iter().any(|t| t.contains("EPSG:32632")),
        "{tag}: no suggestion button"
    );
}

macro_rules! en_ru {
    ($run:expr, $en:ident, $ru:ident) => {
        #[test]
        fn $en() {
            $run(Lang::En);
        }

        #[test]
        fn $ru() {
            $run(Lang::Ru);
        }
    };
}

en_ru!(xlsx_preview_footer, xlsx_preview_footer_en, xlsx_preview_footer_ru);
en_ru!(circuit_editor, circuit_editor_en, circuit_editor_ru);

en_ru!(site_crs, site_crs_en, site_crs_ru);

/// In a Properties-wide grid the standard "new" phase reads in full in the created
/// list (it grows into the free part of its label cell).
fn props_phase_names(lang: Lang) {
    use ddc_model::{Phase, DEFAULT_PHASES};
    let ru = lang == Lang::Ru;
    let (key, name) = DEFAULT_PHASES[2];
    let ph = |id: i64, n: &str, k: Option<&str>| Phase {
        id,
        name: n.into(),
        seq: id,
        key: k.map(str::to_string),
    };
    let phases = vec![ph(1, "Old", None), ph(2, name, Some(key))];
    let long = crate::phase_ui::label(&phases[1], ru);
    let (mut c, mut d) = (2i64, None);
    let id = egui::Id::new("s17.props");
    let (ctx, mut th) = driven_ctx();
    let mut frame = |c_: &egui::Context| {
        egui::SidePanel::right(id).exact_width(300.0).show(c_, |ui| {
            crate::ui_kit::props(ui, "s17.props.grid", |ui| {
                crate::phase_ui::phase_rows(ui, &phases, Some(1), &mut c, &mut d, ru);
            });
        });
    };
    let mut p = Probe::default();
    for _ in 0..FRAMES {
        p = pass_on(&ctx, &mut th, id, (1280.0, 800.0), vec![], &mut frame);
    }
    let tag = format!("props phase names {lang:?}");
    assert!(has_text(&p, &long), "{tag}: {long:?} not drawn in {:?}", p.text);
    assert!(!p.elided.iter().any(|s| *s == long), "{tag}: {long:?} cut");
}

en_ru!(props_phase_names, props_phase_names_en, props_phase_names_ru);

fn family_editor(lang: Lang) {
    let mut app = app_in(lang);
    crate::family_editor::open_for_layout(&mut app);
    let p = probe("family_editor", |ctx| crate::family_editor::windows(ctx, &mut app));
    // tabs must read in full: any elided text is a fault here
    assert_contract(&format!("family_editor {lang:?}"), &p, true);
    family_tab_strip(lang, &p);
    family_empty_tab(lang, &p);
}

/// An empty parameter tab is a compact card and the form follows right under it.
fn family_empty_tab(lang: Lang, p: &Probe) {
    let cat = Catalog::new(lang);
    let key = format!("empty.{}.title", crate::empty_kind::Kind::FamilyParams.what());
    let titles = [false, true].map(|ru| crate::tr_lang(ru, &key, &[]));
    let tag = format!("family_editor {lang:?}");
    let top = p
        .spans
        .iter()
        .find(|(t, _)| titles.contains(t))
        .map(|(_, r)| r.top())
        .unwrap_or_else(|| panic!("{tag}: empty parameter card not drawn"));
    let form = p
        .spans
        .iter()
        .find(|(t, _)| *t == cat.t("fe.param.form"))
        .map(|(_, r)| r.top())
        .unwrap_or_else(|| panic!("{tag}: parameter form not drawn"));
    assert!(form > top, "{tag}: form above the card");
    assert!(form - top <= 96.0, "{tag}: empty tab hole {} px", form - top);
}

/// The section tabs sit in at most two kit-height rows at the top of the right column, and no
/// control of the parameter panel ends below the window.
fn family_tab_strip(lang: Lang, p: &Probe) {
    let cat = Catalog::new(lang);
    let mut names: Vec<String> = vec![];
    for k in ["params", "types", "constraints", "solids", "visibility", "nested", "flex", "text"] {
        names.push(cat.t(&format!("fe.tab.{k}")));
        names.push(cat.t(&format!("fe.tab.{k}.short")));
    }
    let win = p.inner.expect("window drawn");
    let tabs: Vec<&Control> = p
        .controls
        .iter()
        .filter(|c| names.contains(&c.label) && c.rect.left() > win.center().x)
        .collect();
    assert!(!tabs.is_empty(), "{lang:?}: no section tab found");
    let tag = format!("family_editor {lang:?}");
    let want = ddc_ui::theme::control::TAB_H;
    let top = tabs.iter().map(|c| c.rect.top()).fold(f32::MAX, f32::min);
    let bottom = tabs.iter().map(|c| c.rect.bottom()).fold(f32::MIN, f32::max);
    for c in &tabs {
        let h = c.rect.height();
        assert!((h - want).abs() < 1.0, "{tag}: {:?} is {h} high", c.label);
    }
    let mut rows: Vec<i32> = tabs.iter().map(|c| c.rect.top().round() as i32).collect();
    rows.sort_unstable();
    rows.dedup();
    assert!(rows.len() <= 2, "{tag}: tab rows {rows:?}");
    assert!(
        bottom - top <= 2.0 * want + 12.0,
        "{tag}: strip {} high, rows are one token each",
        bottom - top
    );
    // top-aligned: the strip starts in the upper part of the window, not mid-panel
    assert!(top < win.top() + win.height() * 0.5, "{tag}: tabs float at {top} in {win:?}");
    // a row scrolled out of the tab body is cut by its viewport; only what it shows counts
    let low = p
        .controls
        .iter()
        .filter_map(|c| shown_part(&p, c.rect))
        .map(|v| v.bottom())
        .fold(f32::MIN, f32::max);
    assert!(low <= win.bottom() + EPS, "{tag}: control ends at {low}, window {win:?}");
}

fn has_cyrillic(s: &str) -> bool {
    s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c))
}

/// App whose stored level name is neutral: the seed names follow the global language, which
/// would leak Cyrillic into an English dialog through data and not through the dialog's text.
fn app_neutral(lang: Lang) -> EditorApp {
    let mut app = app_in(lang);
    app.ed.model.sql().execute("UPDATE level SET name='Ground'", []).unwrap();
    // the rename above is fixture set-up, not an edit: the project starts saved-clean
    app.ed.project.mark_clean(&app.ed.model);
    app
}

/// The dialog must speak the language it was given, whatever the global flag says (it starts
/// as Russian): English shows no Cyrillic in a control, Russian shows some.
fn assert_language(tag: &str, p: &Probe, lang: Lang) {
    let any = p.controls.iter().any(|c| has_cyrillic(&c.label));
    match lang {
        Lang::Ru => {
            if !any {
                eprintln!("{}", dump(tag, p, &[]));
            }
            assert!(any, "{tag}: no Russian control label");
        }
        _ => {
            // a language picker names each language in itself: the Russian endonym is not a leak
            let endonym = Catalog::new(Lang::En).t("settings.lang.ru");
            let bad: Vec<&str> = p
                .controls
                .iter()
                .map(|c| c.label.as_str())
                .filter(|l| has_cyrillic(l) && *l != endonym)
                .collect();
            assert!(bad.is_empty(), "{tag}: Russian labels in English: {bad:?}");
        }
    }
}

fn drawing_quality(lang: Lang) {
    let mut app = app_neutral(lang);
    for tab in 0..4 {
        crate::range_ui::open_tab_for_layout(tab);
        let p = probe("r3.drawq", |ctx| crate::range_ui::frame(&mut app, ctx));
        let tag = format!("drawing quality tab {tab} {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
    }
}

fn view_templates(lang: Lang) {
    let mut app = app_neutral(lang);
    let plan = ddc_model::ViewKey::Plan(app.ed.level);
    app.ed
        .model
        .template_from_view("Plans", plan, &ddc_model::TemplateField::ALL)
        .unwrap();
    crate::view_gfx_ui::open_templates_for_layout(&app.ed, lang == Lang::Ru);
    let p = probe_id(egui::Id::new("f5.templates.win"), |ctx| {
        crate::view_gfx_ui::frame(&mut app, ctx)
    });
    let tag = format!("view templates {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
}

fn level_question(lang: Lang) {
    let mut app = app_neutral(lang);
    let level = app.ed.level;
    app.ed.add_level("Upper", 3.0).unwrap();
    app.ed.set_level(level);
    crate::level_ask::open(&app.ed, level, vec![("wall", 1), ("wall", 2), ("slab", 3)]);
    let p = probe("level-delete", |ctx| crate::level_ask::ui(&mut app, ctx));
    crate::level_ask::clear();
    let tag = format!("level question {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
}

fn history_revert_all(lang: Lang) {
    let mut app = app_neutral(lang);
    app.quick.hist_ask = Some(vec![1, 2, 3]);
    let p = probe("history.revert_all", |ctx| crate::quick::hist_win::confirm_ui(&mut app, ctx));
    let tag = format!("history revert all {lang:?}");
    assert_contract(&tag, &p, true);
    assert_language(&tag, &p, lang);
    assert_eq!(app.quick.hist_ask.as_ref().map(Vec::len), Some(3), "{tag}: drawing never answers");
}

/// 30 steps, every fifth one made by the AI, the last three undone (redo rows).
fn history_list(lang: Lang) {
    let mut app = app_neutral(lang);
    let word = match lang {
        Lang::Ru => "\u{423}\u{440}\u{43e}\u{432}\u{435}\u{43d}\u{44c}",
        _ => "Level",
    };
    for i in 0..30 {
        app.ed.add_level(&format!("{word} {i}"), 3.0 * f64::from(i)).unwrap();
        if i % 5 == 4 {
            let seq = app.ed.model.op_top().unwrap();
            let entry = ddc_model::AuditEntry {
                session: "s".into(),
                actor: "user".into(),
                event: "apply".into(),
                model: Some("m".into()),
                text: Some(if lang == Lang::Ru {
                    "\u{414}\u{43e}\u{431}\u{430}\u{432}\u{438}\u{442}\u{44c} \
                     \u{443}\u{440}\u{43e}\u{432}\u{435}\u{43d}\u{44c}"
                        .into()
                } else {
                    "Add level".into()
                }),
                op_seq: Some(seq),
                status: "ok".into(),
                ..Default::default()
            };
            app.ed.model.audit_append(&entry).unwrap();
        }
    }
    for _ in 0..3 {
        app.ed.undo();
    }
    app.quick.hist_open = true;
    let p = probe("history", |ctx| crate::quick::hist_win::ui(&mut app, ctx));
    let tag = format!("history {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    assert!(p.controls.len() > 5, "{tag}: the step rows were not drawn");
}

#[test]
fn history_list_en() {
    history_list(Lang::En);
}

#[test]
fn history_list_ru() {
    history_list(Lang::Ru);
}

fn shortcuts(lang: Lang) {
    let mut app = app_neutral(lang);
    app.quick.keys_open = true;
    let p = probe("shortcuts", |ctx| crate::quick::keys_win::ui(&mut app, ctx));
    let tag = format!("shortcuts {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
}

#[test]
fn shortcuts_en() {
    shortcuts(Lang::En);
}

#[test]
fn shortcuts_ru() {
    shortcuts(Lang::Ru);
}

fn fault_list(lang: Lang, n: usize) {
    let mut app = app_neutral(lang);
    let p = probe("ri3.faults", |ctx| crate::shell::faults::layout_frame(&mut app, ctx, n));
    let tag = format!("faults {n} {lang:?}");
    assert_contract(&tag, &p, false);
}

#[test]
fn fault_list_en() {
    fault_list(Lang::En, 0);
    fault_list(Lang::En, 12);
}

#[test]
fn fault_list_ru() {
    fault_list(Lang::Ru, 0);
    fault_list(Lang::Ru, 12);
}

fn warning_list(lang: Lang, n: usize) {
    let mut app = app_neutral(lang);
    let p = probe("warnings", |ctx| crate::shell::warn::layout_frame(&mut app, ctx, n));
    let tag = format!("warnings {n} {lang:?}");
    assert_contract(&tag, &p, false);
    if n > 0 {
        assert_language(&tag, &p, lang);
        return;
    }
    // empty: only the cross, named alike in every language (c2f4aa13e dropped the disabled
    // export), so the title and the empty note carry the language
    let ru = p.lines.iter().any(|l| has_cyrillic(&l.text));
    assert_eq!(ru, lang == Lang::Ru, "{tag}: dialog text is not in {lang:?}");
}

#[test]
fn warning_list_en() {
    warning_list(Lang::En, 0);
    warning_list(Lang::En, 10);
}

#[test]
fn warning_list_ru() {
    warning_list(Lang::Ru, 0);
    warning_list(Lang::Ru, 10);
}

fn autosave(lang: Lang) {
    let mut app = app_neutral(lang);
    let p = probe("autosave", |ctx| crate::shell::recover_win::layout_frame(&mut app, ctx));
    let tag = format!("autosave {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
}

#[test]
fn autosave_en() {
    autosave(Lang::En);
}

#[test]
fn autosave_ru() {
    autosave(Lang::Ru);
}

#[test]
fn history_revert_all_en() {
    history_revert_all(Lang::En);
}

// ---- view graphics, 3D category window and family parameters ----

/// The project of the view graphics probes: a plan view with a template, two filters, a wall
/// type, a member type, two project parameters and one object style.
fn fixture_editor() -> Editor {
    use ddc_model::{
        FilterRule, GfxOverride, LayerSpec, MemberShape, ParamSpec, ParamType, RuleOp,
        TemplateField, ViewFilterSpec, ViewKey,
    };
    let mut ed = Editor::new_in_memory().unwrap();
    let plan = ViewKey::Plan(ed.level);
    ed.model.template_from_view("Plans", plan, &TemplateField::ALL).unwrap();
    for (name, cat) in [("Walls only", "wall"), ("Doors only", "door")] {
        let rule = FilterRule { param: "category".into(), op: RuleOp::Eq, value: cat.into() };
        let spec = ViewFilterSpec { name: name.into(), rules: vec![rule], ..Default::default() };
        ed.model.add_view_filter(&spec).unwrap();
    }
    let layer = LayerSpec { material: "Brick".into(), thickness: 0.2 };
    ed.model.add_wall_type("Brick 200", &[layer]).unwrap();
    let shape = MemberShape::Rect { b: 0.4, h: 0.4 };
    ed.model.add_member_type("Column 400", &shape, "Concrete").unwrap();
    // a seeded type may already own the row; the probe needs a stored material, not a new one
    if ed.model.material("Concrete").unwrap().id == 0 {
        ed.model.add_material(&ddc_model::material_defaults("Concrete")).unwrap();
    }
    for (name, ptype) in [("Fire rating", ParamType::Text), ("Cost", ParamType::Number)] {
        let spec = ParamSpec {
            name: name.into(),
            ptype,
            per_type: false,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: vec!["wall".into()],
        };
        ed.model.add_param(&spec).unwrap();
    }
    let paint = GfxOverride { line: Some(0x33_66_99), ..Default::default() };
    ed.model.set_object_styles(&[("wall".to_string(), paint)]).unwrap();
    ed
}

/// Left edges (rounded) of the controls with `role`, in drawing order.
fn lefts(p: &Probe, role: accesskit::Role) -> Vec<i32> {
    p.controls.iter().filter(|c| c.role == role).map(|c| c.rect.left().round() as i32).collect()
}

fn view_graphics_windows(lang: Lang) {
    for key in ["view.styles", "view.materials"] {
        let ru = lang == Lang::Ru;
        let p = crate::with_test_lang(ru, || {
            crate::view_gfx_ui::layout_probe(key, &mut fixture_editor())
        });
        let tag = format!("{key} {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
    }
}

fn view3d_graphics(lang: Lang) {
    let p = crate::view3d::overlay::layout_probe(&Catalog::new(lang));
    let tag = format!("{} {lang:?}", "view3d_graphics");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    // one row per category: the checkboxes share one x and the sliders share another
    for role in [accesskit::Role::CheckBox, accesskit::Role::Slider] {
        let xs = lefts(&p, role);
        assert_eq!(xs.len(), crate::view3d::CATS.len(), "{tag}: {role:?} rows");
        assert!(xs.iter().all(|x| *x == xs[0]), "{tag}: {role:?} columns differ {xs:?}");
    }
}

fn param_editor(lang: Lang) {
    use crate::shell::panels::param_editor::layout_probe;
    let ru = lang == Lang::Ru;
    for tab in 0..2 {
        let p = crate::with_test_lang(ru, || layout_probe(tab));
        let tag = format!("{} tab {tab} {lang:?}", "param_editor");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
        if tab == 0 {
            let xs = lefts(&p, accesskit::Role::ComboBox);
            assert_eq!(xs.len(), 4, "{tag}: one kind dropdown per parameter");
            assert!(xs.iter().all(|x| *x == xs[0]), "{tag}: kind column differs {xs:?}");
        } else {
            // three type parameters in two type columns: every column has one x
            let mut cols = lefts(&p, accesskit::Role::TextInput);
            cols.sort_unstable();
            cols.dedup();
            assert_eq!(cols.len(), 2, "{tag}: type columns {cols:?}");
        }
    }
}

#[test]
fn view_graphics_windows_en() {
    view_graphics_windows(Lang::En);
}

#[test]
fn view_graphics_windows_ru() {
    view_graphics_windows(Lang::Ru);
}

#[test]
fn view3d_graphics_en() {
    view3d_graphics(Lang::En);
}

#[test]
fn view3d_graphics_ru() {
    view3d_graphics(Lang::Ru);
}

#[test]
fn param_editor_en() {
    param_editor(Lang::En);
}

#[test]
fn param_editor_ru() {
    param_editor(Lang::Ru);
}

#[test]
fn history_revert_all_ru() {
    history_revert_all(Lang::Ru);
}

/// Go-to-view, query and scripts windows, filled and empty, in `lang`.
fn cmd1_windows(lang: Lang) {
    let mut app = app_neutral(lang);
    for i in 0..12 {
        app.ed.add_level(&format!("Level {i}"), 3.0 * (i as f64 + 1.0)).unwrap();
    }
    for q in ["", "Level 1", "zzz"] {
        crate::view_cmds::open_goto_for_layout(q);
        let p = probe("view_cmds.goto", |ctx| crate::view_cmds::frame(&mut app, ctx));
        let tag = format!("go to view {q:?} {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
    }
    crate::view_cmds::open_goto_for_layout("");
    let ru = lang == Lang::Ru;
    let bad = if ru {
        "(\u{441}\u{442}\u{435}\u{43d}\u{44b}"
    } else {
        "(walls"
    };
    for q in ["", "walls", bad, "walls -> height=3200"] {
        crate::query_win::open_for_layout(q);
        let p = probe("view_cmds.query", |ctx| crate::query_win::frame(&mut app, ctx));
        let tag = format!("query {q:?} {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
    }
    crate::query_win::open_for_layout("");
    let dir = std::env::temp_dir().join(format!("ddc_cmd1_layout_{lang:?}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (dir, sel) in [(dir.clone(), None), (dir.clone(), Some("broken.json"))] {
        if sel.is_some() {
            std::fs::write(dir.join("broken.json"), "{\"v\":2}").unwrap();
        }
        crate::scripts_win::open_for_layout(Some(dir), sel);
        let p = probe("view_cmds.scripts", |ctx| crate::scripts_win::frame(&mut app, ctx));
        let tag = format!("scripts {sel:?} {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
    }
    crate::scripts_win::open_for_layout(None, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cmd1_windows_en() {
    cmd1_windows(Lang::En);
}

#[test]
fn cmd1_windows_ru() {
    cmd1_windows(Lang::Ru);
}

fn find_replace(lang: Lang, replace_mode: bool, levels: usize) {
    let mut app = app_neutral(lang);
    for i in 0..levels {
        app.ed.add_level(&format!("L{i}"), 3.0 * (i as f64 + 1.0)).unwrap();
    }
    crate::find_replace::open_for_layout(replace_mode, "Office", "Lab");
    let p = probe("find_replace.find", |ctx| crate::find_replace::frame(&mut app, ctx));
    crate::find_replace::open_for_layout(false, "", "");
    let tag = format!("find and replace replace={replace_mode} levels={levels} {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
}

#[test]
fn find_replace_en() {
    for (mode, levels) in [(false, 0), (true, 0), (true, 14)] {
        find_replace(Lang::En, mode, levels);
    }
}

#[test]
fn find_replace_ru() {
    for (mode, levels) in [(false, 0), (true, 0), (true, 14)] {
        find_replace(Lang::Ru, mode, levels);
    }
}

fn replace_type(lang: Lang) {
    use crate::shell::panels::replace_type::{layout_probe, Case};
    for case in [Case::Fresh, Case::Chosen, Case::Many, Case::NoOther] {
        let p = layout_probe(lang == Lang::Ru, case);
        let tag = format!("replace type {case:?} {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
    }
}

#[test]
fn replace_type_en() {
    replace_type(Lang::En);
}

#[test]
fn replace_type_ru() {
    replace_type(Lang::Ru);
}

fn component_library(lang: Lang) {
    use crate::shell::panels::library_win::{layout_probe, Case};
    for case in [Case::Fresh, Case::Picked, Case::Long, Case::NoMatch] {
        let p = layout_probe("component_library", lang == Lang::Ru, case).probe;
        let tag = format!("component library {case:?} {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
    }
}

#[test]
fn component_library_en() {
    component_library(Lang::En);
}

#[test]
fn component_library_ru() {
    component_library(Lang::Ru);
}

fn type_properties(lang: Lang) {
    use crate::shell::panels::type_dialog::{layout_probe, Case};
    for case in [Case::Wall, Case::WallLong, Case::Member, Case::MemberLong] {
        let (p, _) = layout_probe(lang == Lang::Ru, case);
        let tag = format!("type properties {case:?} {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
    }
}

#[test]
fn type_properties_en() {
    type_properties(Lang::En);
}

#[test]
fn type_properties_ru() {
    type_properties(Lang::Ru);
}

fn wall_type_library(lang: Lang) {
    use crate::shell::panels::{type_library as tl, wall_type_editor as we};
    let ru = lang == Lang::Ru;
    for case in [we::Case::Plain, we::Case::Busy] {
        let p = we::layout_probe(ru, case);
        let tag = format!("wall type editor {case:?} {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
    }
    for case in [tl::Case::List, tl::Case::Import, tl::Case::Purge, tl::Case::Swap] {
        let p = tl::layout_probe(ru, case);
        let tag = format!("wall type library {case:?} {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
    }
}

#[test]
fn wall_type_library_en() {
    wall_type_library(Lang::En);
}

#[test]
fn wall_type_library_ru() {
    wall_type_library(Lang::Ru);
}

/// The dialogs take their language from the caller: no read of the global one in them.
#[test]
fn these_dialogs_do_not_read_the_global_language() {
    let files = [
        ("range_ui", include_str!("range_ui.rs")),
        ("range_styles", include_str!("range_styles.rs")),
        ("range_libs", include_str!("range_libs.rs")),
        ("level_ask", include_str!("level_ask.rs")),
        ("template_win", include_str!("view_gfx_ui/template_win.rs")),
        ("replace_type", include_str!("shell/replace_type.rs")),
        ("library_win", include_str!("shell/library_win.rs")),
        ("wall_type_editor", include_str!("shell/wall_type_editor.rs")),
        ("type_library", include_str!("shell/type_library.rs")),
    ];
    for (name, src) in files {
        let code = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        for global in ["ui_is_ru", "ui_lang_now", "crate::tr(", "tr_n(", "view_gfx_ui::t", "{run, t}"] {
            assert!(!code.contains(global), "{name} reads the global language: {global}");
        }
    }
}

#[test]
fn drawing_quality_en() {
    drawing_quality(Lang::En);
}

#[test]
fn drawing_quality_ru() {
    drawing_quality(Lang::Ru);
}

#[test]
fn view_templates_en() {
    view_templates(Lang::En);
}

#[test]
fn view_templates_ru() {
    view_templates(Lang::Ru);
}

#[test]
fn level_question_en() {
    level_question(Lang::En);
}

#[test]
fn level_question_ru() {
    level_question(Lang::Ru);
}

/// A key sent in the address: its mode and parameter row fit, and the address shows no key.
fn ai_settings_query(lang: Lang) {
    let p = crate::ai::ui::settings::layout_probe_query(lang);
    let tag = format!("ai settings query {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    let cat = Catalog::new(lang);
    for k in ["ai_safety.auth.query", "ai_safety.auth.query_name"] {
        assert!(p.text.iter().any(|t| *t == cat.t(k)), "{tag}: {k} shown");
    }
    assert!(!p.text.iter().any(|t| t.contains("key=")), "{tag}: no key in the address");
}

#[test]
fn ai_settings_query_en() {
    ai_settings_query(Lang::En);
}

#[test]
fn ai_settings_query_ru() {
    ai_settings_query(Lang::Ru);
}

/// A key the store refused on load: the note says so, the key shows nowhere in clear.
fn ai_settings_held(lang: Lang) {
    const FAKE: &str = "fake-SECRETSECRET-r107held";
    let p = crate::ai::ui::settings::layout_probe_held(lang, FAKE);
    let tag = format!("ai settings held {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    let note = Catalog::new(lang).t("ai.settings.key_held");
    assert!(p.text.iter().any(|t| *t == note), "{tag}: the note is shown");
    assert!(!p.text.iter().any(|t| t.contains("SECRET")), "{tag}: no key in clear");
}

#[test]
fn ai_settings_held_en() {
    ai_settings_held(Lang::En);
}

#[test]
fn ai_settings_held_ru() {
    ai_settings_held(Lang::Ru);
}

fn ai_panel(lang: Lang, connected: bool) {
    let p = crate::ai::ui::tests_states::layout_probe(lang, connected);
    assert_contract(&format!("ai_panel {lang:?} connected={connected}"), &p, false);
}

/// Not connected: the message is said once, the one primary action is there, no dead composer.
fn ai_panel_not_connected_once(lang: Lang) {
    let p = crate::ai::ui::tests_states::layout_probe(lang, false);
    let cat = Catalog::new(lang);
    let said = |k: &str| p.text.iter().filter(|t| t.as_str() == cat.t(k)).count();
    assert_eq!(said("ai.state.no_profile"), 1, "{lang:?}: not connected is said once");
    assert_eq!(said("ai.composer.hint_off"), 0, "{lang:?}: no composer placeholder");
    let connect = cat.t("ai.state.no_profile.action");
    let n = p.controls.iter().filter(|c| c.label == connect).count();
    assert_eq!(n, 1, "{lang:?}: one connect action");
    assert!(!p.controls.iter().any(is_text_field), "{lang:?}: an input while disconnected");
}

/// A one-line field or the composer, which takes several lines and says so to screen readers.
fn is_text_field(c: &Control) -> bool {
    matches!(c.role, accesskit::Role::TextInput | accesskit::Role::MultilineTextInput)
}

/// Not connected with an error: the error is said once above the one connect action.
fn ai_panel_not_connected_error(lang: Lang) {
    let p = crate::ai::ui::tests_states::layout_probe_error(lang);
    let tag = format!("ai_panel_error {lang:?}");
    assert_contract(&tag, &p, false);
    let cat = Catalog::new(lang);
    let title = cat.t("ai.err.stale");
    let said = p.text.iter().filter(|t| **t == title).count();
    assert_eq!(said, 1, "{tag}: the error is said once");
    let connect = cat.t("ai.state.no_profile.action");
    assert_eq!(p.controls.iter().filter(|c| c.label == connect).count(), 1, "{tag}: connect");
    let top = |s: &str| p.spans.iter().find(|(t, _)| t == s).map(|(_, r)| r.top());
    let card = top(&cat.t("ai.state.no_profile")).expect("the connect card");
    assert!(top(&title).expect("the error") < card, "{tag}: the error sits above the card");
}

#[test]
fn ai_panel_not_connected_error_en() {
    ai_panel_not_connected_error(Lang::En);
}

#[test]
fn ai_panel_not_connected_error_ru() {
    ai_panel_not_connected_error(Lang::Ru);
}

#[test]
fn ai_panel_not_connected_once_en() {
    ai_panel_not_connected_once(Lang::En);
}

#[test]
fn ai_panel_not_connected_once_ru() {
    ai_panel_not_connected_once(Lang::Ru);
}

#[test]
fn ai_panel_not_connected_en() {
    ai_panel(Lang::En, false);
}

#[test]
fn ai_panel_not_connected_ru() {
    ai_panel(Lang::Ru, false);
}

/// A rebased draft with values editable under its rows: fields fit, labelled, said once.
fn ai_card_values(lang: Lang) {
    let p = crate::ai::ui::tests_card_keys::layout_probe(lang);
    let tag = format!("ai_card_values {lang:?}");
    assert_contract(&tag, &p, false);
    let cat = Catalog::new(lang);
    let fields = p.controls.iter().filter(|c| c.role == accesskit::Role::TextInput).count();
    assert!(fields >= 3, "{tag}: the expanded wall's values are fields ({fields})");
    let apply = cat.t("ai.plan.apply_updated");
    assert_eq!(p.controls.iter().filter(|c| c.label == apply).count(), 1, "{tag}: Apply");
}

#[test]
fn ai_card_values_en() {
    ai_card_values(Lang::En);
}

#[test]
fn ai_card_values_ru() {
    ai_card_values(Lang::Ru);
}

/// A value the person entered mid-request that the agent changed too: both values and the two
/// picks fit and are labelled; Apply is drawn but held off.
fn ai_card_value_choice(lang: Lang) {
    let p = crate::ai::ui::tests_card_keys::layout_probe_choice(lang);
    let tag = format!("ai_card_value_choice {lang:?}");
    assert_contract(&tag, &p, false);
    let cat = Catalog::new(lang);
    // "Keep {agent}" is matched by the text before the agent's value
    for key in ["ai.value.use_mine", "ai.value.keep_agent"] {
        let label = cat.tf(key, &[("mine", "7"), ("agent", "")]);
        let n = p.controls.iter().filter(|c| c.label.starts_with(label.trim())).count();
        assert_eq!(n, 1, "{tag}: {key} is one labelled control");
    }
}

#[test]
fn ai_card_value_choice_en() {
    ai_card_value_choice(Lang::En);
}

#[test]
fn ai_card_value_choice_ru() {
    ai_card_value_choice(Lang::Ru);
}

#[test]
fn ai_panel_connected_en() {
    ai_panel(Lang::En, true);
}

#[test]
fn ai_panel_connected_ru() {
    ai_panel(Lang::Ru, true);
}

fn ai_access(lang: Lang, on: bool) {
    let p = crate::ai::mcp_bridge::layout_probe(lang, on);
    let tag = format!("ai_access {lang:?} on={on}");
    assert_contract(&tag, &p, false);
    let close = ddc_ui::Catalog::new(lang).t("dlg.close");
    assert_footer(&tag, &p, &[close]);
    let token = crate::tr_in(lang, "mcp.set.new", &[]);
    assert_eq!(p.controls.iter().any(|c| c.label == token), on, "{tag}: new token button");
    if on {
        // no switch for agent writes: one line says they wait for the person, inside the window
        let drafts = crate::tr_in(lang, "agent_draft.set.drafts", &[]);
        let inner = p.inner.expect("window drawn");
        let span = p.spans.iter().find(|(t, _)| *t == drafts);
        let (_, r) = span.unwrap_or_else(|| panic!("{tag}: drafts line cut or missing"));
        assert!(r.max.x <= inner.max.x + EPS, "{tag}: drafts line past the window");
        let stem = match lang {
            Lang::Ru => "\u{43f}\u{440}\u{438}\u{43c}\u{435}\u{43d}",
            _ => "apply",
        };
        assert!(drafts.contains(stem), "{tag}: the line names the person's apply");
        let confirm = crate::tr_in(lang, "mcp.set.confirm", &[]);
        assert!(!p.controls.iter().any(|c| c.label == confirm), "{tag}: no policy switch");
    }
}

#[test]
fn ai_access_off_en() {
    ai_access(Lang::En, false);
}

#[test]
fn ai_access_off_ru() {
    ai_access(Lang::Ru, false);
}

#[test]
fn ai_access_on_en() {
    ai_access(Lang::En, true);
}

#[test]
fn ai_access_on_ru() {
    ai_access(Lang::Ru, true);
}

fn ai_audit(lang: Lang) {
    use crate::ai::ui::audit_ui::{layout_probe, LayoutCase};
    for (tag, case) in [
        ("empty", LayoutCase::Empty),
        ("rows", LayoutCase::Rows),
        ("no match", LayoutCase::NoMatch),
    ] {
        let p = layout_probe(lang, case);
        assert_contract(&format!("ai_audit {tag} {lang:?}"), &p, false);
    }
}

#[test]
fn ai_audit_en() {
    ai_audit(Lang::En);
}

#[test]
fn ai_audit_ru() {
    ai_audit(Lang::Ru);
}

fn licenses(lang: Lang) {
    let app = app_in(lang);
    crate::shell::licenses_win::open_window();
    let p = probe("licenses", |ctx| crate::shell::licenses_win::frame(&app, ctx));
    assert_contract(&format!("licenses {lang:?}"), &p, false);
    assert!(
        p.controls.iter().any(|c| c.role == accesskit::Role::TextInput),
        "the search field is a control of the window"
    );
}

#[test]
fn licenses_en() {
    licenses(Lang::En);
}

#[test]
fn licenses_ru() {
    licenses(Lang::Ru);
}

/// "Clean up outline" preview: footer in reach, labels whole, Esc cancels, Enter never fires
/// on a frame nobody pressed.
fn outline_cleanup(lang: Lang) {
    use crate::outline_cleanup::{modal, Ask, Host, Pending};
    let ru = lang == Lang::Ru;
    for host in [Host::Floor(1), Host::Ceiling(1), Host::Shaft(1)] {
        let p = Pending {
            host,
            before: vec![vec![(1., 1.), (4., 4.), (2.9, 2.9), (0., 3.)]],
            after: vec![vec![(1., 1.), (2.9, 2.9), (0., 3.)]],
            spikes: 1,
        };
        let tag = format!("outline cleanup {host:?} {lang:?}");
        let mut got = None;
        let pr = probe_id(modal_id("spike1.clean"), |ctx| got = got.or(modal(ctx, &p, ru)));
        assert_contract(&tag, &pr, false);
        assert_language(&tag, &pr, lang);
        assert_eq!(got, None, "{tag}: drawing never answers");
        let mut got = None;
        pressed(egui::Key::Escape, |ctx| got = got.or(modal(ctx, &p, ru)));
        assert_eq!(got, Some(Ask::Cancel), "{tag}: Esc did not cancel");
    }
}

#[test]
fn outline_cleanup_en() {
    outline_cleanup(Lang::En);
}

#[test]
fn outline_cleanup_ru() {
    outline_cleanup(Lang::Ru);
}

fn legends(lang: Lang, filled: bool) {
    let mut app = app_in(lang);
    if filled {
        app.ed.model.add_legend("A", 100.0).unwrap();
    }
    crate::annot_text::h_legend(&mut app);
    let p = probe("annot.legend", |ctx| crate::annot_text::window(ctx, &mut app.ed, lang));
    assert_contract(&format!("legends {lang:?} filled={filled}"), &p, false);
}

#[test]
fn legends_en() {
    legends(Lang::En, false);
    legends(Lang::En, true);
}

#[test]
fn legends_ru() {
    legends(Lang::Ru, false);
    legends(Lang::Ru, true);
}

/// Design options window with two sets (one with a very long name), the first with a selection.
fn design_options(lang: Lang, empty: bool, selected: bool) -> Probe {
    let mut app = app_neutral(lang);
    if !empty {
        let long = "A very long option set name that has to be cut with an ellipsis and a tip";
        let (set, _) = app.ed.model.add_option_set("Facade", "Option 1").unwrap();
        app.ed.model.add_option(set, long).unwrap();
        app.ed.model.add_option_set(long, "Second").unwrap();
    }
    if selected {
        app.ed.selection.push(crate::editor::Sel::Wall(1));
    }
    crate::collab::options_win::open(&mut app);
    probe_id(egui::Id::new("ddc_options"), |ctx| {
        crate::collab::options_win::layout_frame(&mut app, ctx)
    })
}

fn design_options_checks(lang: Lang) {
    for (empty, selected) in [(true, false), (false, false), (false, true)] {
        let p = design_options(lang, empty, selected);
        let tag = format!("design options {lang:?} empty={empty} selected={selected}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
        let unnamed: Vec<_> = p.controls.iter().filter(|c| c.label.trim().is_empty()).collect();
        assert!(unnamed.is_empty(), "{tag}: unnamed controls {unnamed:?}");
        let cat = Catalog::new(lang);
        let count = |key: &str, name: &str| {
            let want = cat.tf(key, &[("name", name)]);
            p.controls
                .iter()
                .filter(|c| c.role == accesskit::Role::RadioButton && c.label == want)
                .count()
        };
        if empty {
            continue;
        }
        for key in ["options.show_plan", "options.show_3d", "options.primary_named"] {
            assert_eq!(count(key, "Option 1"), 1, "{tag}: {key} for Option 1");
        }
        let add_sel = cat.t("options.add_selection");
        let rows = p.controls.iter().filter(|c| c.label == add_sel).count();
        assert_eq!(rows, 3, "{tag}: one quiet Add selection per option");
        let more = cat.tf("options.more", &[("name", "Option 1")]);
        assert!(p.controls.iter().any(|c| c.label == more), "{tag}: overflow button");
        // accept and delete live in the overflow menu, not as row buttons
        for key in ["options.accept", "options.delete"] {
            let l = cat.t(key);
            assert!(!p.controls.iter().any(|c| c.label == l), "{tag}: row button {key}");
        }
    }
}

#[test]
fn design_options_en() {
    design_options_checks(Lang::En);
}

#[test]
fn design_options_ru() {
    design_options_checks(Lang::Ru);
}

/// The norm-check section of the Checks tab: never run, and with findings (rows, filters,
/// export buttons), in a form-sized window.
fn norm_checks(lang: Lang) {
    use ddc_model::{LayerSpec, OpeningSpec, WallSpec};
    let mut m = ddc_model::Model::open(":memory:").unwrap();
    let level = m.add_level("L1", 0.0).unwrap();
    let layer = [LayerSpec { material: "Brick".into(), thickness: 0.2 }];
    let wt = m.add_wall_type("W200", &layer).unwrap();
    let spec = WallSpec {
        a: (0.0, 0.0),
        b: (10.0, 0.0),
        level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: wt,
        offset0: None,
    };
    let wall = m.add_wall(&spec).unwrap();
    let door = OpeningSpec { along: 2.0, width: 0.7, sill: 0.0, height: 2.1, kind: "door".into() };
    let d = m.add_opening(wall, &door).unwrap();
m.ensure_norm_params().unwrap();
    let pid = m.param_by_name("EvacuationExit").unwrap().unwrap().id;
    let yes = Some(ddc_model::PVal::Bool(true));
    let set = ddc_model::ParamSet { param: pid, kind: "opening".into(), id: d, value: yes };
    m.set_param_values(&[set]).unwrap();
    let cat = Catalog::new(lang);
    for (ran, missing) in [(false, false), (true, false), (false, true)] {
        let mut nm = if ran {
            crate::ai::ui::norms::Norms::ran_on(&m)
        } else {
            crate::ai::ui::norms::Norms::default()
        };
        if missing {
            nm.missing = ddc_model::checks::norms::NORM_PARAMS.iter().map(|p| p.0).collect();
        }
        let key = format!("t.norms.{ran}.{missing}");
        let mut th = Theme::new(Mode::Dark, Density::Compact);
        let p = probe(&key, |ctx| {
            th.apply(ctx);
            crate::ui_kit::Dialog::new(&key, "T", crate::ui_kit::DialogSize::Form)
                .scroll_body()
                .show(ctx, |ui| {
                    let (mut acts, mut hot) = (vec![], None);
                    let units = crate::units::active_units();
                    crate::ai::ui::norms::view(ui, &mut nm, &th, &cat, units, &mut acts, &mut hot);
                });
        });
        let tag = format!("norm checks {lang:?} ran={ran} missing={missing}");
        assert_contract(&tag, &p, false);
        let run = cat.t(if ran { "norms.run_again" } else { "norms.run" });
        assert!(p.controls.iter().any(|c| c.label == run), "{tag}: run button");
        let export = cat.t("norms.export_xlsx");
        assert_eq!(p.controls.iter().any(|c| c.label == export), ran, "{tag}: export button");
        let add = cat.t("norms.fields_add");
        assert_eq!(p.controls.iter().any(|c| c.label == add), missing, "{tag}: add button");
    }
}

#[test]
fn norm_checks_en() {
    norm_checks(Lang::En);
}

#[test]
fn norm_checks_ru() {
    norm_checks(Lang::Ru);
}

#[test]
fn probe_sees_a_control_cut_by_a_narrow_window() {
    let p = probe("t.layout.narrow", |ctx| {
        crate::ui_kit::Dialog::new("t.layout.narrow", "T", crate::ui_kit::DialogSize::Form)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(2000.0); // design-lint: data (overflow probe)
                    let _ = ui.button("Save"); // design-lint: widget (probe fixture)
                });
            });
    });
    assert!(
        !violations(&p, false).is_empty(),
        "a button far past the window edge was not reported"
    );
}

/// A footer pushed under the window bottom is a fault; a row cut by its scroll viewport is not.
#[test]
fn probe_tells_a_cut_footer_from_a_scrolled_row() {
    let draw = |ctx: &egui::Context, key: &str, footer: bool| {
        crate::ui_kit::Dialog::new(key, "T", crate::ui_kit::DialogSize::Tool).stable().show(
            ctx,
            |ui| {
                if footer {
                    ui.add_space(ui.available_height()); // design-lint: data (overflow probe)
                    let _ = ui.button("Save"); // design-lint: widget (probe fixture)
                    return;
                }
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for i in 0..200 {
                        // design-lint: widget (probe fixture)
                        let _ = ui.button(format!("Row {i}")); // design-lint: widget (probe)
                    }
                });
            },
        );
    };
    let cut = probe("t.layout.cut", |ctx| draw(ctx, "t.layout.cut", true));
    assert!(!violations(&cut, false).is_empty(), "a footer below the window was not reported");
    let rows = probe("t.layout.rows", |ctx| draw(ctx, "t.layout.rows", false));
    assert_contract("scrolled rows", &rows, false);
}

/// Rows ending exactly on a nested clip: the next row shows nothing and hangs past the window.
/// Only a recorded scroll viewport may hide it; a plain nested clip is a cut row.
#[test]
fn nested_clip_hides_a_row_only_when_it_scrolls() {
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| egui::Rect::from_min_max((x0, y0).into(), (x1, y1).into());
    let (win, body) = (r(0.0, 0.0, 300.0, 200.0), r(0.0, 40.0, 300.0, 150.0));
    let (a, b) = (r(10.0, 100.0, 100.0, 150.0), r(10.0, 150.0, 100.0, 210.0));
    let ctl = |label: &str, rect, id| Control {
        role: accesskit::Role::Button,
        label: label.into(),
        rect,
        id,
        clipped_out: false,
    };
    let mut p = Probe {
        inner: Some(win),
        controls: vec![ctl("A", a, 1), ctl("B", b, 2)],
        // the title draws in the window's own clip, above the nested one
        clips: vec![(r(10.0, 4.0, 100.0, 20.0), win), (a, body), (b, body)],
        ..Probe::default()
    };
    let cut = |p: &Probe| violations(p, false).iter().any(|v| v.contains("\"B\"") && v.contains("vertically"));
    assert!(cut(&p), "a row hidden by a nested non-scroll clip was not reported");
    p.scrolls = vec![body];
    assert!(!cut(&p), "a row scrolled out of a recorded viewport was reported");
}

/// A number cut at the side of its clip is a fault, unless the clip is the side of a recorded
/// sideways scrolling viewport; a vertical-only viewport does not excuse it, nor does a cut
/// on the side the viewport does not end on.
#[test]
fn number_cut_sideways_is_excused_only_by_a_sideways_scroll() {
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| egui::Rect::from_min_max((x0, y0).into(), (x1, y1).into());
    let view = r(100.0, 50.0, 400.0, 200.0);
    let line = |x0: f32, x1: f32| Line {
        text: "12.00".into(),
        rect: r(x0, 60.0, x1, 76.0),
        rows: 1,
        clip: view,
        view,
        mid: 68.0,
        top: None,
        hidden: false,
    };
    let cut = |p: &Probe| row_faults(p, false).iter().any(|v| v.contains("cut sideways"));
    let mut p = Probe { lines: vec![line(380.0, 420.0)], ..Probe::default() };
    assert!(cut(&p), "a number past the clip was not reported");
    p.scrolls = vec![view];
    assert!(cut(&p), "a vertical viewport excused a sideways cut");
    p.hscrolls = vec![r(100.0, 40.0, 400.0, 210.0)];
    assert!(!cut(&p), "a number at the side of a wide table was reported");
    p.hscrolls = vec![r(100.0, 40.0, 500.0, 210.0)];
    assert!(cut(&p), "a cut short of the viewport side was excused");
    // a row under the rows' own vertical scroll keeps the parent's clip top and bottom
    let tall = r(100.0, 0.0, 400.0, 300.0);
    let low = |y: f32| Line { rect: r(380.0, y, 420.0, y + 16.0), clip: tall, view: tall, mid: y + 8.0, ..line(380.0, 420.0) };
    p.hscrolls = vec![r(100.0, 40.0, 400.0, 210.0)];
    p.lines = vec![low(190.0)];
    assert!(!cut(&p), "a totals row at the side of a wide table was reported");
    p.lines = vec![low(250.0)];
    assert!(cut(&p), "a number below the wide table was excused");
}

#[test]
fn project_parameters_en() {
    project_parameters(false);
}

#[test]
fn project_parameters_ru() {
    project_parameters(true);
}

#[test]
fn schedules_multi_en() {
    schedules_multi(false);
}

#[test]
fn schedules_multi_ru() {
    schedules_multi(true);
}

#[test]
fn phases_en() {
    phases(false);
}

#[test]
fn phases_ru() {
    phases(true);
}

#[test]
fn links_en() {
    links(Lang::En);
}

#[test]
fn links_ru() {
    links(Lang::Ru);
}

#[test]
fn sources_en() {
    sources(Lang::En);
}

#[test]
fn sources_ru() {
    sources(Lang::Ru);
}

#[test]
fn clashes_en() {
    clashes(Lang::En);
}

#[test]
fn clashes_ru() {
    clashes(Lang::Ru);
}

#[test]
fn family_editor_en() {
    family_editor(Lang::En);
}

#[test]
fn family_editor_ru() {
    family_editor(Lang::Ru);
}

#[test]
fn xlsx_preview_en() {
    xlsx_preview(Lang::En);
}

#[test]
fn xlsx_preview_ru() {
    xlsx_preview(Lang::Ru);
}

/// Preview with rows missing from the workbook: the delete toggle is a checkbox at the footer
/// left, on the same row and centre line as the buttons, the note sits between them.
fn xlsx_footer_toggle(lang: Lang, no_base: bool) {
    use crate::shell::xlsx_win::XlsxPreview;
    let ru = lang == Lang::Ru;
    let mut app = app_in(lang);
    let tag = format!("ddc_layout_xlf_{}_{:?}_{no_base}.xlsx", std::process::id(), lang);
    let path = std::env::temp_dir().join(tag);
    ddc_io::xlsx::export_xlsx_units(&app.ed.model, &path, ddc_io::xlsx::Units::Metric).unwrap();
    let mut pv = XlsxPreview::load(&mut app.ed.model, &path, true).unwrap();
    // load() of a book equal to the model shows the linked card, not this window
    pv.synced = false;
    pv.report.changes.push(ddc_io::xlsx::Change {
        sheet: "Walls".into(),
        row: 2,
        col: 0,
        column: "id".into(),
        id: Some(90),
        old: String::new(),
        new: "1".into(),
    });
    pv.report.removed.push(ddc_io::xlsx::Removed { sheet: "Walls".into(), id: 5 });
    pv.report.no_base = no_base;
    app.xlsx = Some(pv);
    let p = probe("xlsx_preview", |ctx| crate::shell::xlsx_win::layout_frame(&mut app, ctx));
    let label = crate::tr_str(ru, "xlsx_delete.delete_missing");
    let boxed = p
        .controls
        .iter()
        .find(|c| c.role == accesskit::Role::CheckBox && c.label == label)
        .unwrap_or_else(|| panic!("{lang:?}: no delete checkbox in {:?}", p.controls));
    let cancel_label = crate::tr_str(ru, "xlw.cancel");
    let cancel = p
        .controls
        .iter()
        .find(|c| c.role == accesskit::Role::Button && c.label == cancel_label)
        .expect("cancel button");
    let dy = (boxed.rect.center().y - cancel.rect.center().y).abs();
    assert!(dy <= 1.0, "{lang:?}: footer is one row, dy={dy}");
    assert!(boxed.rect.right() <= cancel.rect.left(), "{lang:?}: checkbox left of the buttons");
    let note = crate::tr_str(ru, "xlw.one_undo_step_ctrl_z");
    if let Some((_, r)) = p.spans.iter().find(|(t, _)| t == note) {
        let dn = (r.center().y - cancel.rect.center().y).abs();
        assert!(dn <= 1.5, "{lang:?}: note on the buttons' centre line, dn={dn}");
        assert!(r.left() >= boxed.rect.right() - EPS, "{lang:?}: note right of the checkbox");
        assert!(r.right() <= cancel.rect.left() + EPS, "{lang:?}: note left of the buttons");
    }
    assert_contract(&format!("xlsx_footer_toggle {lang:?} no_base={no_base}"), &p, false);
}

#[test]
fn xlsx_footer_toggle_en() {
    xlsx_footer_toggle(Lang::En, false);
    xlsx_footer_toggle(Lang::En, true);
}

#[test]
fn xlsx_footer_toggle_ru() {
    xlsx_footer_toggle(Lang::Ru, false);
    xlsx_footer_toggle(Lang::Ru, true);
}

/// Preview with long sheet and column names and many changes of every kind.
fn xlsx_preview_long(lang: Lang) {
    use crate::shell::xlsx_win::XlsxPreview;
    let mut app = app_in(lang);
    let path = std::env::temp_dir().join(format!("ddc_layout_xl_long_{}_{:?}.xlsx", std::process::id(), lang));
    ddc_io::xlsx::export_xlsx_units(&app.ed.model, &path, ddc_io::xlsx::Units::Metric).unwrap();
    let mut pv = XlsxPreview::load(&mut app.ed.model, &path, true).unwrap();
    // load() of a book equal to the model shows the linked card, not this window
    pv.synced = false;
    let sheet = "Walls \u{b7} Partition type with a very long descriptive sheet name";
    let column = "Fire resistance rating of the assembly including finishes (minutes)";
    for i in 0..40usize {
        let new = match i % 4 {
            0 => "created".to_string(),
            1 => "deleted".to_string(),
            2 => "A very long replacement value that cannot fit into any list column".to_string(),
            _ => i.to_string(),
        };
        pv.report.changes.push(ddc_io::xlsx::Change {
            sheet: sheet.into(),
            row: 2 + i as u32,
            col: 0,
            column: column.into(),
            id: Some(i as i64 + 90),
            old: "Previous long value of the same cell in the model".into(),
            new,
        });
    }
    app.xlsx = Some(pv);
    let p = probe("xlsx_preview", |ctx| crate::shell::xlsx_win::layout_frame(&mut app, ctx));
    assert_contract(&format!("xlsx_preview_long {lang:?}"), &p, false);
}

#[test]
fn xlsx_preview_long_en() {
    xlsx_preview_long(Lang::En);
}

#[test]
fn xlsx_preview_long_ru() {
    xlsx_preview_long(Lang::Ru);
}

fn xl_preview(app: &mut EditorApp, tag: &str) {
    use crate::shell::xlsx_win::XlsxPreview;
    let path = std::env::temp_dir().join(format!("ddc_layout_{tag}_{}.xlsx", std::process::id()));
    ddc_io::xlsx::export_xlsx_units(&app.ed.model, &path, ddc_io::xlsx::Units::Metric).unwrap();
    let mut pv = XlsxPreview::load(&mut app.ed.model, &path, true).unwrap();
    // load() of a book equal to the model shows the linked card, not this window
    pv.synced = false;
    app.xlsx = Some(pv);
}

fn one_change() -> ddc_io::xlsx::Change {
    ddc_io::xlsx::Change {
        sheet: "Walls".into(),
        row: 2,
        col: 0,
        column: "id".into(),
        id: Some(90),
        old: String::new(),
        new: "1".into(),
    }
}

/// An idle preview that gets changes in the same session grows to the workspace size. With
/// `saved`, the idle form was dragged before (its geometry is stored, a small size included):
/// the workspace still opens at its own size where the idle form stood.
fn xlsx_idle_then_changes(lang: Lang, screen: (f32, f32), saved: Option<[f32; 4]>) {
    use crate::shell::xlsx_win::PREVIEW_WORK_GEOM;
    let tag = format!("{lang:?} {screen:?} saved={}", saved.is_some());
    let mut app = app_in(lang);
    xl_preview(&mut app, &format!("idle_then_{lang:?}_{}_{}", screen.0, saved.is_some()));
    crate::ui_kit::forget_geometry("xlsx_preview");
    crate::ui_kit::forget_geometry(PREVIEW_WORK_GEOM);
    if let Some(g) = saved {
        crate::ui_kit::save_geometry("xlsx_preview", g);
    }
    let (ctx, mut th) = driven_ctx();
    let none = egui::Id::new("none");
    let rect_of = |ctx: &egui::Context| {
        egui::AreaState::load(ctx, crate::shell::xlsx_win::current_preview_id(ctx))
            .map(|a| a.rect())
            .expect("preview window")
    };
    let mut frame = |ctx: &egui::Context| {
        crate::shell::xlsx_win::layout_frame(&mut app, ctx);
    };
    for _ in 0..FRAMES {
        let _ = pass_on(&ctx, &mut th, none, screen, vec![], &mut frame);
    }
    let idle = rect_of(&ctx);
    drop(frame);
    app.xlsx.as_mut().unwrap().report.changes.push(one_change());
    let mut frame = |ctx: &egui::Context| {
        crate::shell::xlsx_win::layout_frame(&mut app, ctx);
    };
    for _ in 0..FRAMES {
        let _ = pass_on(&ctx, &mut th, none, screen, vec![], &mut frame);
    }
    let work = rect_of(&ctx);
    let idle_geom = crate::ui_kit::saved_geometry("xlsx_preview");
    let work_geom = crate::ui_kit::saved_geometry(PREVIEW_WORK_GEOM);
    crate::ui_kit::forget_geometry("xlsx_preview");
    crate::ui_kit::forget_geometry(PREVIEW_WORK_GEOM);
    assert!(work.height() > idle.height() + 100.0, "{tag}: {idle:?} -> {work:?}");
    assert!(work.width() > idle.width() + 50.0, "{tag}: {idle:?} -> {work:?}");
    assert!((work.min.x - idle.min.x).abs() < 1.0, "{tag}: left edge kept");
    let scr = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(screen.0, screen.1));
    assert!(scr.expand(EPS).contains_rect(work), "{tag}: {work:?} leaves the screen");
    if let Some(g) = saved {
        assert!(work.width() > g[2] + 50.0, "{tag}: the idle size was reused: {work:?}");
        assert!(work.height() > g[3] + 100.0, "{tag}: the idle size was reused: {work:?}");
        assert!((idle.min.x - g[0]).abs() < 1.0, "{tag}: idle form ignores its saved spot");
    }
    // opening never writes geometry; each class keeps only what its own drag stored
    assert_eq!(idle_geom, saved, "{tag}: idle geometry changed");
    assert_eq!(work_geom, None, "{tag}: workspace geometry written without a drag");
}

#[test]
fn xlsx_idle_then_changes_en() {
    xlsx_idle_then_changes(Lang::En, SCREEN, None);
}

#[test]
fn xlsx_idle_then_changes_ru() {
    xlsx_idle_then_changes(Lang::Ru, SCREEN, None);
}

/// The idle form dragged to the top-left corner region, with a small stored size.
const IDLE_DRAGGED: [f32; 4] = [16.0, 90.0, 300.0, 180.0];

#[test]
fn xlsx_idle_dragged_then_changes_en() {
    for screen in [(1024.0, 768.0), (1280.0, 720.0)] {
        xlsx_idle_then_changes(Lang::En, screen, Some(IDLE_DRAGGED));
    }
}

#[test]
fn xlsx_idle_dragged_then_changes_ru() {
    for screen in [(1024.0, 768.0), (1280.0, 720.0)] {
        xlsx_idle_then_changes(Lang::Ru, screen, Some(IDLE_DRAGGED));
    }
}

/// A book that names no project is a warning: its line shows with the disclosure closed.
fn xlsx_no_project_visible(lang: Lang) {
    let ru = lang == Lang::Ru;
    let mut app = app_in(lang);
    xl_preview(&mut app, &format!("no_project_{lang:?}"));
    let pv = app.xlsx.as_mut().unwrap();
    pv.report.changes.push(one_change());
    pv.report.no_project = true;
    let p = probe("xlsx_preview", |ctx| crate::shell::xlsx_win::layout_frame(&mut app, ctx));
    let line = crate::tr_str(ru, "xlsx_project.no_project");
    assert!(p.text.iter().any(|t| t.as_str() == line), "{lang:?}: no_project line missing");
    assert_contract(&format!("xlsx_no_project {lang:?}"), &p, false);
}

#[test]
fn xlsx_no_project_visible_en() {
    xlsx_no_project_visible(Lang::En);
}

#[test]
fn xlsx_no_project_visible_ru() {
    xlsx_no_project_visible(Lang::Ru);
}

/// What the XLSX workspace preview is fed: how many changes, whether the book has no baseline
/// (blocks Apply), how many proposed new columns, and how many book-level issues.
struct XlCase {
    changes: usize,
    no_base: bool,
    proposals: usize,
    issues: usize,
    long: bool,
}

/// Draws the preview for `case` on `screen` and returns what the last frame drew.
fn xlsx_case(lang: Lang, screen: (f32, f32), case: &XlCase, tag: &str) -> Probe {
    let mut app = xlsx_app(lang, case, tag);
    let id = egui::Id::new(("ddc.dialog", "xlsx_preview"));
    probe_on(id, screen, |ctx| crate::shell::xlsx_win::layout_frame(&mut app, ctx))
}

/// Name of the proposed column `i` of `case`.
fn xl_case_column(case: &XlCase, i: usize) -> String {
    if case.long {
        format!("Fire resistance rating of the assembly including finishes {i}")
    } else {
        format!("Work code {i}")
    }
}

/// An app whose XLSX preview holds what `case` describes.
fn xlsx_app(lang: Lang, case: &XlCase, tag: &str) -> EditorApp {
    use crate::shell::xlsx_win::XlsxPreview;
    let mut app = app_in(lang);
    let file = format!("ddc_layout_xlw_{}_{:?}_{tag}.xlsx", std::process::id(), lang);
    let path = std::env::temp_dir().join(file);
    ddc_io::xlsx::export_xlsx_units(&app.ed.model, &path, ddc_io::xlsx::Units::Metric).unwrap();
    let mut pv = XlsxPreview::load(&mut app.ed.model, &path, true).unwrap();
    // load() of a book equal to the model shows the linked card, not this window
    pv.synced = false;
    let (sheet, column) = if case.long {
        (
            "Walls \u{b7} Partition type with a very long descriptive sheet name",
            "Fire resistance rating of the assembly including finishes (minutes)",
        )
    } else {
        ("Walls", "Height [mm]")
    };
    for i in 0..case.changes {
        pv.report.changes.push(ddc_io::xlsx::Change {
            sheet: sheet.into(),
            row: 2 + i as u32,
            col: 9,
            column: column.into(),
            id: Some(i as i64 + 90),
            old: "3000".into(),
            new: "3200".into(),
        });
    }
    for i in 0..case.issues {
        pv.report.rejected.push(ddc_io::xlsx::Rejection {
            sheet: "(model)".into(),
            row: 0,
            col: 0,
            column: String::new(),
            reason: format!("required column missing: column {i}"),
        });
    }
    for i in 0..case.proposals {
        let name = xl_case_column(case, i);
        pv.report.proposals.push(ddc_io::xlsx::Proposal {
            sheet: "Walls".into(),
            row: 1,
            col: 12 + i as u32,
            header: name.clone(),
            spec: ddc_model::ParamSpec {
                name,
                ptype: ddc_model::ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec![],
            },
            values: vec![],
            accept: false,
            // long cases: the last column's rows were all refused (disabled tick and its note)
            refused: if case.long && i + 1 == case.proposals { vec![7, 12] } else { vec![] },
        });
    }
    pv.report.no_base = case.no_base;
    app.xlsx = Some(pv);
    app
}

/// Visible table rows: distinct lines of the id column that lie inside the window.
fn xlsx_rows(p: &Probe, n: usize) -> usize {
    let win = p.inner.expect("window");
    let mut ys: Vec<i32> = vec![];
    for (t, r) in &p.spans {
        let is_id = t.parse::<usize>().is_ok_and(|v| (90..90 + n).contains(&v));
        if is_id && r.top() >= win.top() && r.bottom() <= win.bottom() {
            ys.push(r.center().y.round() as i32);
        }
    }
    ys.sort_unstable();
    ys.dedup();
    ys.len()
}

fn checkboxes(p: &Probe) -> usize {
    p.controls.iter().filter(|c| c.role == accesskit::Role::CheckBox).count()
}

/// Workspace preview: the table keeps at least 12 rows at 1280x720 whatever is folded above it,
/// the issues fold into one "to check" disclosure, Apply stays in the window, no clipped text.
fn xlsx_workspace(lang: Lang) {
    let ru = lang == Lang::Ru;
    let cancel_label = crate::tr_str(ru, "xlw.cancel");
    let screen = (1280.0, 720.0);
    let heavy = XlCase { changes: 30, no_base: true, proposals: 1, issues: 3, long: false };
    let long = XlCase { long: true, ..heavy };
    let quiet = XlCase { changes: 30, no_base: false, proposals: 1, issues: 0, long: false };
    let bare = XlCase { proposals: 0, ..quiet };
    let word = if ru {
        "\u{41a} \u{41f}\u{420}\u{41e}\u{412}\u{415}\u{420}\u{41a}\u{415}"
    } else {
        "TO CHECK"
    };
    let mut open_boxes = 0;
    for (name, case) in [("heavy", &heavy), ("long", &long)] {
        let p = xlsx_case(lang, screen, case, name);
        assert_contract(&format!("xlsx_workspace {name} {lang:?}"), &p, false);
        let rows = xlsx_rows(&p, case.changes);
        assert!(rows >= 12, "{lang:?} {name}: only {rows} table rows are visible");
        assert!(p.text.iter().any(|t| t.contains(word)), "{lang:?} {name}: no disclosure");
        let cancel = p
            .controls
            .iter()
            .find(|c| c.role == accesskit::Role::Button && c.label == cancel_label)
            .unwrap_or_else(|| panic!("{lang:?} {name}: no Cancel in {:?}", p.controls));
        let apply = p
            .controls
            .iter()
            .any(|c| c.role == accesskit::Role::Button && c.rect.left() >= cancel.rect.right());
        assert!(apply, "{lang:?} {name}: Apply is not in the window");
        open_boxes = checkboxes(&p);
    }
    // blocking items open the disclosure by themselves, a quiet preview keeps it closed
    let q = xlsx_case(lang, screen, &quiet, "quiet");
    let b = xlsx_case(lang, screen, &bare, "bare");
    assert_contract(&format!("xlsx_workspace quiet {lang:?}"), &q, false);
    assert!(xlsx_rows(&q, 30) >= 12, "{lang:?}: quiet preview shows too few rows");
    assert_eq!(checkboxes(&q), checkboxes(&b), "{lang:?}: quiet disclosure must stay closed");
    assert!(open_boxes > checkboxes(&q), "{lang:?}: blocking items must open the disclosure");
}

#[test]
fn xlsx_workspace_en() {
    xlsx_workspace(Lang::En);
}

#[test]
fn xlsx_workspace_ru() {
    xlsx_workspace(Lang::Ru);
}

/// No changes at all: the compact empty card, no disclosure, nothing clipped.
fn xlsx_workspace_none(lang: Lang) {
    let none = XlCase { changes: 0, no_base: false, proposals: 0, issues: 0, long: false };
    let p = xlsx_case(lang, SCREEN, &none, "none");
    assert_contract(&format!("xlsx_workspace none {lang:?}"), &p, false);
}

#[test]
fn xlsx_workspace_none_en() {
    xlsx_workspace_none(Lang::En);
}

#[test]
fn xlsx_workspace_none_ru() {
    xlsx_workspace_none(Lang::Ru);
}

/// Narrow and short window: every control stays inside it, in both languages.
fn xlsx_workspace_narrow(lang: Lang) {
    let many = XlCase { changes: 30, no_base: true, proposals: 1, issues: 3, long: true };
    let p = xlsx_case(lang, (640.0, 560.0), &many, "narrow");
    assert_contract(&format!("xlsx_workspace narrow {lang:?}"), &p, false);
}

#[test]
fn xlsx_workspace_narrow_en() {
    xlsx_workspace_narrow(Lang::En);
}

#[test]
fn xlsx_workspace_narrow_ru() {
    xlsx_workspace_narrow(Lang::Ru);
}

/// A short table leaves its room to the open "to check" disclosure: the new-column heading under
/// the no-comparison card is drawn whole, not cut by the disclosure's scroll edge.
fn xlsx_short_table_check(lang: Lang) {
    let short = XlCase { changes: 4, no_base: true, proposals: 1, issues: 0, long: false };
    let p = xlsx_case(lang, (1600.0, 1000.0), &short, "short");
    let tag = format!("xlsx_workspace short {lang:?}");
    assert_contract(&tag, &p, false);
    let head = crate::tr_str(lang == Lang::Ru, "xlw.new_columns_project_parameters_tic");
    let (_, r) = p
        .spans
        .iter()
        .find(|(t, _)| t == head)
        .unwrap_or_else(|| panic!("{tag}: new-column heading not drawn"));
    let whole = p.clips.iter().any(|(b, clip)| {
        (b.center() - r.center()).length() < 4.0 && clip.contains_rect(r.shrink(1.0))
    });
    assert!(whole, "{tag}: new-column heading {r:?} is cut by its scroll area");
}

#[test]
fn xlsx_short_table_check_en() {
    xlsx_short_table_check(Lang::En);
}

#[test]
fn xlsx_short_table_check_ru() {
    xlsx_short_table_check(Lang::Ru);
}

/// The tick of new column `i`, fully shown (not cut by its scroll area or the window).
fn xl_tick_shown(p: &Probe, case: &XlCase, i: usize) -> Option<bool> {
    let name = xl_case_column(case, i);
    let tick = p
        .controls
        .iter()
        .find(|c| c.role == accesskit::Role::CheckBox && c.label.contains(&name))?;
    let win = p.inner.expect("window");
    let shown = shown_part(p, tick.rect).unwrap_or(egui::Rect::NOTHING);
    Some(shown.expand(EPS).contains_rect(tick.rect) && win.expand(EPS).contains_rect(tick.rect))
}

const XL_SCREENS: [(f32, f32); 2] = [(1024.0, 768.0), (1280.0, 720.0)];

/// No comparison book, one new column, 4 changed rows: the "to check" area takes what the
/// short table leaves, so the new-column tick is fully in view; short and long names.
fn xlsx_check_room(lang: Lang) {
    for screen in XL_SCREENS {
        for long in [false, true] {
            let case = XlCase { changes: 4, no_base: true, proposals: 1, issues: 0, long };
            let tag = format!("xlsx_check_room {lang:?} {screen:?} long={long}");
            let p = xlsx_case(lang, screen, &case, &format!("room_{}_{long}", screen.0));
            assert_contract(&tag, &p, false);
            let shown = xl_tick_shown(&p, &case, 0);
            assert_eq!(shown, Some(true), "{tag}: new-column tick hidden {:?}", p.controls);
            assert!(xlsx_rows(&p, 4) == 4, "{tag}: a changed row is out of view");
        }
    }
}

#[test]
fn xlsx_check_room_en() {
    xlsx_check_room(Lang::En);
}

#[test]
fn xlsx_check_room_ru() {
    xlsx_check_room(Lang::Ru);
}

/// Many new columns: the chip opens the check area at them, the first tick comes fully in view,
/// nothing leaves the window; then Apply semantics are untouched (the ticks stay off).
fn xlsx_new_columns_chip(lang: Lang) {
    use crate::shell::xlsx_counts::{chip_label, Chip};
    let ru = lang == Lang::Ru;
    for screen in XL_SCREENS {
        let case = XlCase { changes: 4, no_base: true, proposals: 14, issues: 2, long: true };
        let tag = format!("xlsx_new_columns_chip {lang:?} {screen:?}");
        let mut app = xlsx_app(lang, &case, &format!("chip_{}", screen.0));
        let (ctx, mut th) = driven_ctx();
        let id = egui::Id::new(("ddc.dialog", "xlsx_preview"));
        let mut frame = |c: &egui::Context| crate::shell::xlsx_win::layout_frame(&mut app, c);
        let mut seen = Probe::default();
        for _ in 0..FRAMES {
            seen = pass_on(&ctx, &mut th, id, screen, vec![], &mut frame);
        }
        assert_contract(&format!("{tag} before"), &seen, false);
        let label = chip_label(ru, Chip::NewColumns, case.proposals);
        let at = seen
            .spans
            .iter()
            .find(|(t, _)| *t == label)
            .unwrap_or_else(|| panic!("{tag}: no chip {label:?} in {:?}", seen.text))
            .1
            .center();
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        pass_on(&ctx, &mut th, id, screen, vec![egui::Event::PointerMoved(at)], &mut frame);
        pass_on(&ctx, &mut th, id, screen, vec![button(true)], &mut frame);
        pass_on(&ctx, &mut th, id, screen, vec![button(false)], &mut frame);
        // the scroll is animated
        for _ in 0..40 {
            seen = pass_on(&ctx, &mut th, id, screen, vec![], &mut frame);
        }
        drop(frame);
        assert_contract(&format!("{tag} after"), &seen, false);
        assert_eq!(xl_tick_shown(&seen, &case, 0), Some(true), "{tag}: first tick not in view");
        let pv = app.xlsx.as_ref().expect("preview");
        assert!(pv.report.proposals.iter().all(|p| !p.accept), "{tag}: a chip ticked a column");
    }
}

#[test]
fn xlsx_new_columns_chip_en() {
    xlsx_new_columns_chip(Lang::En);
}

#[test]
fn xlsx_new_columns_chip_ru() {
    xlsx_new_columns_chip(Lang::Ru);
}

/// A 400-row diff keeps its table rows; the check area keeps at least 2 XXL, and 5 lines where
/// the screen has room. A short table gives the check area everything past its 5 rows.
#[test]
fn xlsx_check_cap_shares_the_height() {
    use crate::shell::xlsx_win::check_cap;
    use ddc_ui::theme::space::{SM, XXL};
    let row = 22.0;
    let keep5 = row * 5.0 + XXL * 3.0 + SM * 4.0;
    for free in [300.0, 500.0, 800.0] {
        let short = check_cap(free, row, 4);
        assert!((short - (free - keep5).max(XXL * 2.0)).abs() < 0.5, "{free}: {short}");
        assert!(short >= check_cap(free, row, 400), "{free}: a short table takes less room");
        assert!(check_cap(free, row, 400) >= XXL * 2.0, "{free}: check area vanished");
    }
    assert!(check_cap(800.0, row, 400) >= row * 5.0, "a tall screen keeps 5 check lines");
    assert!(check_cap(800.0, row, 400) <= XXL * 8.0, "a long table keeps its room");
}

/// A 400-row diff with long names at 1280x720: the table keeps 12 rows, nothing is clipped.
#[test]
fn xlsx_long_diff_keeps_table_rows() {
    for lang in [Lang::En, Lang::Ru] {
        let case = XlCase { changes: 400, no_base: true, proposals: 3, issues: 1, long: true };
        let p = xlsx_case(lang, (1280.0, 720.0), &case, "long_diff");
        assert_contract(&format!("xlsx_long_diff {lang:?}"), &p, false);
        let rows = xlsx_rows(&p, case.changes);
        assert!(rows >= 12, "{lang:?}: only {rows} table rows with a 400-row diff");
    }
}

/// One row of every kind in the preview table, with long names and empty values.
fn xlsx_kind_rows(lang: Lang, screen: (f32, f32), long: bool, tag: &str) -> Probe {
    use crate::shell::xlsx_win::XlsxPreview;
    use ddc_io::xlsx::{Change, Conflict, Ignored, Rejection};
    let mut app = app_in(lang);
    let file = format!("ddc_layout_xlk_{}_{:?}_{tag}.xlsx", std::process::id(), lang);
    let path = std::env::temp_dir().join(file);
    ddc_io::xlsx::export_xlsx_units(&app.ed.model, &path, ddc_io::xlsx::Units::Metric).unwrap();
    let mut pv = XlsxPreview::load(&mut app.ed.model, &path, true).unwrap();
    let (sheet, column) = if long {
        (
            "Walls \u{b7} Partition type with a very long descriptive name",
            "Fire resistance rating of the assembly (minutes)",
        )
    } else {
        ("Walls", "Height [mm]")
    };
    let chg = |row: u32, col: u32, column: &str, old: &str, new: &str, id: Option<i64>| Change {
        sheet: sheet.into(),
        row,
        col,
        column: column.into(),
        id,
        old: old.into(),
        new: new.into(),
    };
    pv.report.changes.push(chg(2, 0, "id", "", "created", None));
    pv.report.changes.push(chg(3, 0, "id", "", "deleted", Some(91)));
    pv.report.changes.push(chg(4, 9, column, "", "", Some(92)));
    pv.report.changes.push(chg(5, 9, column, "3000", "3200", Some(93)));
    pv.report.rejected.push(Rejection {
        sheet: "(model)".into(),
        row: 0,
        col: 0,
        column: String::new(),
        reason: "required column missing: id".into(),
    });
    pv.report.conflicts.push(Conflict {
        sheet: sheet.into(),
        row: 6,
        col: 9,
        column: column.into(),
        id: Some(94),
        base: "3000".into(),
        book: "3300".into(),
        model: "3100".into(),
        take_book: false,
    });
    pv.report.ignored.push(Ignored {
        sheet: sheet.into(),
        row: 7,
        col: 9,
        column: column.into(),
        id: Some(95),
        old: "3000".into(),
        new: "3400".into(),
        reason: "wall 95 is pinned: skipped".into(),
    });
    // the report was edited after the read: it is no longer the quiet "in sync" one
    pv.synced = false;
    app.xlsx = Some(pv);
    let id = egui::Id::new(("ddc.dialog", "xlsx_preview"));
    probe_on(id, screen, |ctx| crate::shell::xlsx_win::layout_frame(&mut app, ctx))
}

/// Created, changed, deleted, refused, conflicting and locked rows: nothing clipped in a normal
/// and a narrow window, a created row shows its book row instead of a cell address, an empty
/// value is named, and the kind cells carry their label next to the icon.
fn xlsx_kind_row_contract(lang: Lang) {
    let ru = lang == Lang::Ru;
    for (name, screen, long) in [
        ("wide", (1280.0, 720.0), false),
        ("long", (1280.0, 720.0), true),
        ("narrow", (640.0, 560.0), true),
    ] {
        let p = xlsx_kind_rows(lang, screen, long, name);
        assert_contract(&format!("xlsx_kind_rows {name} {lang:?}"), &p, false);
        if name == "narrow" {
            continue;
        }
        let tag = format!("xlsx_kind_rows {name} {lang:?}");
        let has = |s: &str| p.text.iter().any(|t| t == s);
        assert!(!has("2:2"), "{tag}: a created row shows a cell address");
        let book = crate::tr_lang(ru, "xl_diff.book_row", &[("n", "2")]);
        assert!(has(&book), "{tag}: no book row {book:?} in {:?}", p.text);
        assert!(has(crate::tr_str(ru, "xl_diff.empty")), "{tag}: an empty value is not named");
        for key in ["xlw.deleted", "xlw.kind_refused", "xlw.kind_locked", "xlw.changed"] {
            let word = crate::tr_str(ru, key);
            assert!(has(word), "{tag}: no kind label {word:?}");
        }
    }
}

#[test]
fn xlsx_kind_rows_en() {
    xlsx_kind_row_contract(Lang::En);
}

#[test]
fn xlsx_kind_rows_ru() {
    xlsx_kind_row_contract(Lang::Ru);
}

/// Add-rebar form at its fixed minimum width, every tab, for a host pick.
fn rebar_form(lang: Lang, hosts: &[(&str, &str)], tag: &str) {
    let mut app = app_in(lang);
    app.ed
        .model
        .add_rebar_type("Reinforcing bar class B500 with a long descriptive name", 0.016, "B500", 0.064)
        .unwrap();
    for tab in 0..4 {
        let p = probe("rebar.form", |ctx| crate::rebar_ui::layout_frame(&mut app, ctx, hosts, tab));
        assert_contract(&format!("rebar_form {tag} tab {tab} {lang:?}"), &p, false);
    }
}

const REBAR_COLUMNS: [(&str, &str); 2] = [
    ("column", "Concrete column 400 x 400 with a long type name"),
    ("column", "Concrete column 600 x 600 with a long type name"),
];
const REBAR_SLABS: [(&str, &str); 2] = [
    ("slab", "Floor slab 250 mm reinforced concrete"),
    ("slab", "Floor slab 300 mm reinforced concrete"),
];
const REBAR_MIXED: [(&str, &str); 5] = [
    ("column", "Concrete column 400 x 400"),
    ("beam", "Concrete beam 300 x 600"),
    ("wall", "Basic wall 200"),
    ("slab", "Floor slab 250 mm"),
    ("footing", "Isolated footing 1200"),
];

#[test]
fn rebar_form_column_en() {
    rebar_form(Lang::En, &REBAR_COLUMNS, "column");
}

#[test]
fn rebar_form_column_ru() {
    rebar_form(Lang::Ru, &REBAR_COLUMNS, "column");
}

#[test]
fn rebar_form_slab_en() {
    rebar_form(Lang::En, &REBAR_SLABS, "slab");
}

#[test]
fn rebar_form_slab_ru() {
    rebar_form(Lang::Ru, &REBAR_SLABS, "slab");
}

#[test]
fn rebar_form_mixed_en() {
    rebar_form(Lang::En, &REBAR_MIXED, "mixed");
}

#[test]
fn rebar_form_mixed_ru() {
    rebar_form(Lang::Ru, &REBAR_MIXED, "mixed");
}

/// Project information (`which` "info"), purge with items ("purge") and the empty purge ("none").
fn data_window(lang: Lang, which: &str) {
    let key = if which == "info" { "data_export.info" } else { "data_export.purge" };
    let mut app = app_neutral(lang);
    crate::data_ui::open_for_layout(&mut app, which);
    let win = if which == "info" { "ddc.f7.info" } else { "ddc.f7.purge" };
    let p = probe_id(egui::Id::new(win), |ctx| {
        crate::data_ui::frame(&mut app, ctx)
    });
    let tag = format!("{key} {which} {lang:?}");
    assert_contract(&tag, &p, false);
    // item names are model data (seed names may follow the global language), not dialog text
    let chrome = Probe {
        inner: p.inner,
        controls: p.controls.iter().filter(|c| c.role != accesskit::Role::CheckBox).cloned().collect(),
        elided: vec![],
        text: vec![],
        spans: vec![],
        shapes: vec![],
        clips: vec![],
        screen: None,
        dangling: vec![],
        lines: vec![],
        canvas: vec![],
        ctl_top: vec![],
        user: vec![],
        scrolls: vec![],
        hscrolls: vec![],
    };
    assert_language(&tag, &chrome, lang);
    // footer: the quiet way out is always there, named in the dialog's language
    let out = ddc_ui::Catalog::new(lang).t(if which == "none" { "dlg.close" } else { "dlg.cancel" });
    assert!(p.controls.iter().any(|c| c.label == out), "{tag}: no {out:?} button");
    if which == "info" {
        let fields = p.controls.iter().filter(|c| c.role == accesskit::Role::TextInput).count();
        assert!(fields >= 10, "{tag}: {fields} fields");
        assert!(
            p.controls.iter().filter(|c| c.role == accesskit::Role::TextInput).all(|c| !c.label.is_empty()),
            "{tag}: a field without a name"
        );
    }
    if which == "purge" {
        let boxes: Vec<&Control> =
            p.controls.iter().filter(|c| c.role == accesskit::Role::CheckBox).collect();
        assert!(boxes.len() >= 2, "{tag}: {} checkboxes", boxes.len());
        assert!(boxes.iter().all(|c| !c.label.is_empty()), "{tag}: an unnamed checkbox");
    }
}

#[test]
fn data_info_en() {
    data_window(Lang::En, "info");
}

#[test]
fn data_info_ru() {
    data_window(Lang::Ru, "info");
}

#[test]
fn data_purge_en() {
    data_window(Lang::En, "purge");
}

#[test]
fn data_purge_ru() {
    data_window(Lang::Ru, "purge");
}

#[test]
fn data_purge_empty_en() {
    data_window(Lang::En, "none");
}

/// STL / OBJ export: scope and units as named radio buttons, export and cancel in the footer.
fn mesh_export(lang: Lang, fmt: crate::mesh_io::Fmt) {
    let mut app = app_neutral(lang);
    crate::mesh_io::open_for_layout(&mut app, fmt);
    let p = probe_id(egui::Id::new("ddc.mx"), |ctx| crate::data_ui::frame(&mut app, ctx));
    let tag = format!("mx {fmt:?} {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    let c = ddc_ui::Catalog::new(lang);
    for k in ["mx.scope.all", "mx.units.m", "mx.units.mm", "mx.export", "dlg.cancel"] {
        let l = c.t(k);
        assert!(p.controls.iter().any(|x| x.label == l), "{tag}: no {l:?}");
    }
    let radios = p.controls.iter().filter(|x| x.role == accesskit::Role::RadioButton).count();
    assert!(radios >= 4, "{tag}: {radios} radio buttons");
}

#[test]
fn mesh_export_stl_en() {
    mesh_export(Lang::En, crate::mesh_io::Fmt::Stl);
}

#[test]
fn mesh_export_obj_ru() {
    mesh_export(Lang::Ru, crate::mesh_io::Fmt::Obj);
}

#[test]
fn mesh_export_step_en_and_3mf_ru() {
    mesh_export(Lang::En, crate::mesh_io::Fmt::Step);
    mesh_export(Lang::Ru, crate::mesh_io::Fmt::ThreeMf);
}

/// Shared checks of the help and keyboard windows: contract, language, a named search field.
fn search_window(tag: &str, p: &Probe, lang: Lang) {
    assert_contract(tag, p, false);
    assert_language(tag, p, lang);
    let fields: Vec<&Control> =
        p.controls.iter().filter(|c| c.role == accesskit::Role::TextInput).collect();
    assert!(!fields.is_empty(), "{tag}: no search field");
}

fn help_window(lang: Lang, query: &str) {
    use crate::help::panel::{show, Panel};
    let app = app_neutral(lang);
    let lib = ddc_ui::help::Library::builtin(lang);
    let mut panel = Panel::default();
    panel.open = true;
    panel.query = query.to_string();
    let mut acts = vec![];
    let p = probe("help", |ctx| show(ctx, &mut panel, &lib, &app.sh.cat, &app.sh.reg, &mut acts));
    search_window(&format!("help {query:?} {lang:?}"), &p, lang);
}

fn help_news(lang: Lang) {
    let app = app_neutral(lang);
    let (mut open, mut acts) = (true, vec![]);
    let p = probe("help_news", |ctx| crate::help::news(ctx, &mut open, lang, &app, &mut acts));
    let tag = format!("help news {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
}

fn help_reference(lang: Lang) {
    let app = app_neutral(lang);
    let p = probe_id(egui::Id::new("ddc.help.ref.win"), |ctx| {
        crate::help::refdock::layout_frame(&app, ctx)
    });
    search_window(&format!("{} {lang:?}", "help.ref"), &p, lang);
}

fn shortcut_sheet(lang: Lang, filter: &str) {
    let mut app = app_neutral(lang);
    crate::quick::rebuild_keys(&mut app);
    app.quick.sheet.open = true;
    app.quick.sheet.filter = filter.to_string();
    let p = probe_id(egui::Id::new(("ddc.modal", "shortcut_sheet")), |ctx| {
        crate::quick::sheet::ui(&mut app, ctx)
    });
    let tag = format!("shortcut sheet {filter:?} {lang:?}");
    search_window(&tag, &p, lang);
    let close = Catalog::new(lang).t("sheet.close");
    assert!(p.controls.iter().any(|c| c.label == close), "{tag}: no {close:?} button");
}

/// First-run layout chooser on the smallest supported screen.
fn first_run(lang: Lang) {
    let mut app = app_neutral(lang);
    let screen = (1280.0, 720.0);
    let p = probe_on(egui::Id::new(("ddc.modal", "first_run")), screen, |ctx| {
        app.first_run_chooser(ctx)
    });
    let tag = format!("first run {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    let win = p.inner.expect("drawn");
    let fits = win.left() >= 0.0 && win.top() >= 0.0;
    let fits = fits && win.right() <= screen.0 && win.bottom() <= screen.1;
    assert!(fits, "{tag}: the card {win:?} does not fit {screen:?}");
    let start = Catalog::new(lang).t("dlg.first.start");
    let n = p.controls.iter().filter(|c| c.label == start).count();
    assert_eq!(n, 1, "{tag}: {n} primary buttons {start:?}");
}

/// Problem report with every optional part shown: one primary action (save), copy and cancel.
fn report_form(lang: Lang) {
    let app = app_neutral(lang);
    let p = probe("report", |ctx| crate::shell::report_win::layout_frame(&app, ctx));
    let tag = format!("report {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    let c = Catalog::new(lang);
    for key in ["report.save", "report.copy", "report.cancel"] {
        let want = c.t(key);
        let n = p.controls.iter().filter(|x| x.label == want).count();
        assert_eq!(n, 1, "{tag}: {n} buttons {want:?}");
    }
    // the report's inputs are multiline boxes
    let fields = p
        .controls
        .iter()
        .filter(|x| {
            matches!(x.role, accesskit::Role::TextInput | accesskit::Role::MultilineTextInput)
        })
        .count();
    assert!(fields >= 3, "{tag}: {fields} fields");
    let boxes = p.controls.iter().filter(|x| x.role == accesskit::Role::CheckBox).count();
    assert!(boxes >= 3, "{tag}: {boxes} checkboxes");
}

#[test]
fn help_window_en() {
    help_window(Lang::En, "");
}

#[test]
fn help_window_ru() {
    help_window(Lang::Ru, "");
}

#[test]
fn help_window_no_hits_en() {
    help_window(Lang::En, "zzqqxx");
}

#[test]
fn help_window_no_hits_ru() {
    help_window(Lang::Ru, "zzqqxx");
}

#[test]
fn help_news_en() {
    help_news(Lang::En);
}

#[test]
fn help_news_ru() {
    help_news(Lang::Ru);
}

#[test]
fn help_reference_en() {
    help_reference(Lang::En);
}

#[test]
fn help_reference_ru() {
    help_reference(Lang::Ru);
}

#[test]
fn shortcut_sheet_en() {
    shortcut_sheet(Lang::En, "");
}

#[test]
fn shortcut_sheet_ru() {
    shortcut_sheet(Lang::Ru, "");
}

#[test]
fn shortcut_sheet_no_hits_en() {
    shortcut_sheet(Lang::En, "zzqqxx");
}

#[test]
fn shortcut_sheet_no_hits_ru() {
    shortcut_sheet(Lang::Ru, "zzqqxx");
}

#[test]
fn first_run_en() {
    first_run(Lang::En);
}

#[test]
fn first_run_ru() {
    first_run(Lang::Ru);
}

#[test]
fn report_form_en() {
    report_form(Lang::En);
}

#[test]
fn report_form_ru() {
    report_form(Lang::Ru);
}

#[test]
fn data_purge_empty_ru() {
    data_window(Lang::Ru, "none");
}

fn drafting_list(lang: Lang) {
    use crate::drafting_view as dv;
    let mut app = app_in(lang);
    let ru = matches!(lang, Lang::Ru);
    for name in ["Eave", "Stair", "Door jamb"] {
        let id = dv::create(&mut app.ed).unwrap();
        assert!(dv::rename(&mut app.ed, id, name));
    }
    dv::show_list();
    let p = probe("dv1.list", |ctx| dv::window_in(&mut app, ctx, ru));
    assert_contract(&format!("drafting_list {lang:?}"), &p, false);
}

#[test]
fn drafting_list_en() {
    drafting_list(Lang::En);
}

#[test]
fn drafting_list_ru() {
    drafting_list(Lang::Ru);
}

fn vv_window(lang: Lang) {
    use ddc_model::{FilterRule, FilterUse, RuleOp, TemplateField, ViewFilterSpec, ViewKey};
    crate::lang_on_this_thread(lang == Lang::Ru);
    let cat = Catalog::new(lang);
    for (filters, linked, attached) in [
        (false, false, true),
        (false, true, true),
        (true, false, true),
        (true, true, true),
        (true, false, false),
        (true, true, false),
    ] {
        let mut app = app_neutral(lang);
        let plan = ViewKey::Plan(app.ed.level);
        if attached {
            let long = "A very long filter name that has to be cut with an ellipsis and a tip";
            let spec = ViewFilterSpec {
                name: long.to_string(),
                rules: vec![FilterRule {
                    param: "level".into(),
                    op: RuleOp::Eq,
                    value: "Ground".into(),
                }],
                ..Default::default()
            };
            let id = app.ed.model.add_view_filter(&spec).unwrap();
            let over = Default::default();
            let on = FilterUse { filter: id, enabled: true, visible: true, over };
            app.ed.model.edit_view_gfx(plan, |g| g.filters.push(on)).unwrap();
        }
        if linked {
            let t = app.ed.model.template_from_view("Plans", plan, &TemplateField::ALL).unwrap();
            app.ed.model.link_view_template(Some(t), &[plan]).unwrap();
        }
        crate::view_gfx_ui::open_vv_for_layout(filters);
        let p = probe("view.vv", |ctx| crate::view_gfx_ui::frame(&mut app, ctx));
        let tag = format!("vv {lang:?} filters={filters} linked={linked} attached={attached}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
        let note = cat.tf("view_template.locked", &[("name", "Plans")]);
        let edit = cat.t("view_template.edit_template");
        let shown = p.text.iter().any(|t| *t == note);
        let button = p.controls.iter().any(|c| c.label == edit);
        assert_eq!(shown, linked, "{tag}: lock note {note:?}");
        assert_eq!(button, linked, "{tag}: Edit template button");
        let empty = cat.t("view_template.no_filters");
        let empty_shown = p.text.iter().any(|t| *t == empty);
        assert_eq!(empty_shown, filters && !attached, "{tag}: empty note");
        let unnamed: Vec<_> = p.controls.iter().filter(|c| c.label.trim().is_empty()).collect();
        assert!(unnamed.is_empty(), "{tag}: unnamed controls {unnamed:?}");
    }
}

#[test]
fn vv_window_en() {
    vv_window(Lang::En);
}

#[test]
fn vv_window_ru() {
    vv_window(Lang::Ru);
}

// Confirm and question dialogs: delete with dependents, close with unsaved work, phase and
// template delete, failed save, closing note, blocking model error. Long Russian names and
// three drafts; the safe action is the default, the danger action never answers Enter, Esc
// cancels where the dialog can be cancelled.

/// A window is at most this wide: a long name wraps, it never widens a confirm dialog.
const CONFIRM_MAX_W: f32 = 600.0;

/// 80 characters of Russian text with word breaks, made without Cyrillic in the source.
fn long_ru(shift: u32) -> String {
    (0..80u32)
        .map(|i| match i % 9 {
            8 => ' ',
            _ => char::from_u32(0x430 + (i + shift) % 26).unwrap(),
        })
        .collect()
}

/// Window id of a `modal_dialog`.
fn modal_id(key: &str) -> egui::Id {
    egui::Id::new(("ddc.modal", key))
}

/// Draws `frame` until it settles, then once more with `key` pressed.
fn pressed(key: egui::Key, mut frame: impl FnMut(&egui::Context)) {
    let ctx = font_ctx();
    let mut th = Theme::new(Mode::Dark, Density::Compact);
    for i in 0..=FRAMES {
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(SCREEN.0, SCREEN.1),
            )),
            ..egui::RawInput::default()
        };
        if i == FRAMES {
            raw.events.push(egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let _ = ctx.run(raw, |ctx| {
            let _ = th.apply(ctx);
            frame(ctx);
        });
    }
}

fn button<'a>(p: &'a Probe, tag: &str, label: &str) -> &'a Control {
    p.controls
        .iter()
        .find(|c| c.role == accesskit::Role::Button && c.label == label)
        .unwrap_or_else(|| panic!("{tag}: no {label:?} button in {:?}", p.controls))
}

/// The buttons exist, sit apart, and follow `labels` from the far right to the left.
fn assert_footer(tag: &str, p: &Probe, labels: &[String]) {
    let rects: Vec<egui::Rect> = labels.iter().map(|l| button(p, tag, l).rect).collect();
    for (w, l) in rects.windows(2).zip(labels.iter().skip(1)) {
        assert!(w[1].max.x <= w[0].min.x + EPS, "{tag}: {l:?} is not left of its neighbour");
    }
}

fn assert_narrow(tag: &str, p: &Probe) {
    let w = p.inner.map_or(0.0, |r| r.width());
    assert!(w <= CONFIRM_MAX_W, "{tag}: the window is {w} wide");
}

/// A wall with more dependents than the quiet limit, deleted: the confirm waits.
fn delete_ask_editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    crate::modify_tools::tests::walls(&mut ed, &[((0.0, 0.0), (20.0, 0.0))]);
    let w = ed.walls[0].id;
    for i in 0..=crate::editor::delete_deps::QUIET_MAX {
        let door = ddc_model::OpeningSpec {
            along: 1.0 + 1.5 * i as f64,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        };
        ed.model.add_opening(w, &door).unwrap();
    }
    ed.reload().unwrap();
    ed.selection = vec![crate::editor::Sel::Wall(w)];
    ed.delete_selection();
    let p = ed.pending_delete.as_mut().expect("the delete asks");
    // every optional line of the dialog at once
    p.deps.other_level = 2;
    p.deps.rooms = 3;
    p.deps.unknown = true;
    ed
}

fn delete_confirm(lang: Lang) {
    let ru = lang == Lang::Ru;
    let cat = Catalog::new(lang);
    let mut ed = delete_ask_editor();
    let p = probe("del.confirm", |ctx| crate::editor::delete_deps::dialog_in(&mut ed, ctx, ru));
    let tag = format!("del.confirm {lang:?}");
    assert_contract(&tag, &p, true);
    assert_language(&tag, &p, lang);
    assert_narrow(&tag, &p);
    let (all, no) = (cat.t("del.confirm.all"), cat.t("del.confirm.cancel"));
    assert_footer(&tag, &p, &[all, no]);
    pressed(egui::Key::Enter, |ctx| crate::editor::delete_deps::dialog_in(&mut ed, ctx, ru));
    assert!(ed.pending_delete.is_some(), "{tag}: Enter answered the question");
    pressed(egui::Key::Escape, |ctx| crate::editor::delete_deps::dialog_in(&mut ed, ctx, ru));
    assert!(ed.pending_delete.is_none(), "{tag}: Esc did not cancel");
    assert_eq!(ed.walls.len(), 1, "{tag}: the wall must stay");
}

fn phase_delete(lang: Lang) {
    let ru = lang == Lang::Ru;
    let cat = Catalog::new(lang);
    let mut ed = Editor::new_in_memory().unwrap();
    let long = ed.model.add_phase(&long_ru(0), None).unwrap();
    let keep = ed.model.add_phase("Phase B", None).unwrap();
    ed.model.add_phase(&long_ru(5), None).unwrap();
    let count = ed.model.phases().unwrap().len();
    let mut ask = Some((long, keep));
    let p = probe("phases.delete", |ctx| {
        crate::phase_ui::layout_delete(ctx, &mut ed, ru, &mut ask)
    });
    let tag = format!("phases.delete {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    assert_narrow(&tag, &p);
    let (go, no) = (cat.t("phases.del_ok"), cat.t("dlg.cancel"));
    assert_footer(&tag, &p, &[go, no]);
    let mut ask = Some((long, keep));
    pressed(egui::Key::Enter, |ctx| {
        crate::phase_ui::layout_delete(ctx, &mut ed, ru, &mut ask)
    });
    assert!(ask.is_some(), "{tag}: Enter answered the question");
    pressed(egui::Key::Escape, |ctx| {
        crate::phase_ui::layout_delete(ctx, &mut ed, ru, &mut ask)
    });
    assert!(ask.is_none(), "{tag}: Esc did not cancel");
    assert_eq!(ed.model.phases().unwrap().len(), count, "{tag}: a phase was deleted");
}

fn template_delete(lang: Lang, used: usize) {
    let ru = lang == Lang::Ru;
    let cat = Catalog::new(lang);
    let name = long_ru(3);
    let ask =
        |ctx: &egui::Context| crate::view_gfx_ui::template_delete_layout(ctx, &name, used, ru);
    let p = probe_id(modal_id("f5.tpl.delete"), |ctx| {
        let _ = ask(ctx);
    });
    let tag = format!("f5.tpl.delete {lang:?} used {used}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    assert_narrow(&tag, &p);
    let (go, no) = (cat.t("view_template.delete.confirm"), cat.t("view_gfx.cancel"));
    assert_footer(&tag, &p, &[go, no]);
    let mut got = None;
    pressed(egui::Key::Enter, |ctx| got = got.or(ask(ctx)));
    assert_ne!(got, Some(true), "{tag}: Enter confirmed the delete");
    let mut got = None;
    pressed(egui::Key::Escape, |ctx| got = got.or(ask(ctx)));
    assert_eq!(got, Some(false), "{tag}: Esc did not cancel");
}

fn unsaved_ask(lang: Lang) {
    use crate::project_ui::drafts::{label, Draft};
    let cat = Catalog::new(lang);
    let mut app = app_neutral(lang);
    let held: Vec<String> = [
        Draft::Family(long_ru(1)),
        Draft::Type(long_ru(7)),
        Draft::Params(long_ru(13)),
    ]
    .iter()
    .map(|d| label(&cat, d))
    .collect();
    let p = probe_id(modal_id("project.unsaved"), |ctx| {
        let _ = crate::project_ui::layout_prompt(&mut app, ctx, &held);
    });
    let tag = format!("project.unsaved {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    assert_narrow(&tag, &p);
    // Save all (Enter) on the far right, then Cancel, and the danger Discard farthest from it
    let (save, no, drop) =
        (cat.t("drafts.btn.save_all"), cat.t("project.btn.cancel"), cat.t("project.btn.discard"));
    assert_footer(&tag, &p, &[save, no, drop]);
    let mut open = true;
    pressed(egui::Key::Escape, |ctx| {
        open = crate::project_ui::layout_prompt(&mut app, ctx, &held)
    });
    assert!(!open, "{tag}: Esc did not cancel");
    assert!(!crate::project_ui::layout_closes(&app), "{tag}: Esc closed the project");
}

fn closing_note_probe(lang: Lang) {
    let mut app = app_neutral(lang);
    let mut first = true;
    let p = probe_id(egui::Id::new("save.closing"), |ctx| {
        crate::save::layout_closing(&mut app, ctx, first);
        first = false;
    });
    let tag = format!("save.closing {lang:?}");
    assert_contract(&tag, &p, true);
    assert_language(&tag, &p, lang);
    assert_narrow(&tag, &p);
    for key in [egui::Key::Enter, egui::Key::Escape] {
        let mut app = app_neutral(lang);
        let (mut first, mut shown) = (true, false);
        pressed(key, |ctx| {
            shown = crate::save::layout_closing(&mut app, ctx, first);
            first = false;
        });
        assert!(shown, "{tag}: {key:?} stopped the running save");
    }
}

fn save_failed(lang: Lang, kind: crate::io_kind::IoKind, raw: &str, retry: bool) {
    let cat = Catalog::new(lang);
    let mut app = app_neutral(lang);
    let mut first = Some(kind);
    let p = probe_id(modal_id("save_fail.save_fail"), |ctx| {
        let _ = crate::project_ui::layout_save_fail(&mut app, ctx, first.take(), raw);
    });
    let tag = format!("save_fail.save_fail {lang:?} {kind:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    assert_narrow(&tag, &p);
    let mut want = vec![cat.t("save_fail.save_fail.save_as")];
    if retry {
        want.push(cat.t("save_fail.save_fail.retry"));
    }
    want.push(cat.t("save_fail.save_fail.close"));
    assert_footer(&tag, &p, &want);
    let retry_label = cat.t("save_fail.save_fail.retry");
    let has = p.controls.iter().any(|c| c.label == retry_label);
    assert_eq!(has, retry, "{tag}: Retry shown {has}");
    // Enter would open the Save as file dialog, so only Esc is driven here
    let mut first = Some(kind);
    let mut open = true;
    pressed(egui::Key::Escape, |ctx| {
        open = crate::project_ui::layout_save_fail(&mut app, ctx, first.take(), raw)
    });
    assert!(!open, "{tag}: Esc did not close");
}

fn save_failed_all(lang: Lang) {
    use crate::io_kind::IoKind;
    save_failed(lang, IoKind::Denied, "permission denied (os error 5)", true);
    save_failed(lang, IoKind::Full, "no space left on the device (os error 112)", false);
    let long = format!("{} (os error 5)", "C:/very_long_folder_name/".repeat(4));
    save_failed(lang, IoKind::Other, &long, true);
}

fn damaged_file(lang: Lang, prev: bool, snaps: bool) {
    let cat = Catalog::new(lang);
    let mut app = app_neutral(lang);
    let mut first = true;
    let p = probe_id(modal_id("project.damaged"), |ctx| {
        let _ = crate::project_ui::layout_damaged(&mut app, ctx, first, prev, snaps);
        first = false;
    });
    let tag = format!("project.damaged {lang:?} prev {prev} snaps {snaps}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    assert_narrow(&tag, &p);
    let mut want = vec![];
    if prev {
        want.push(cat.t("project.damaged.open_prev"));
    }
    if snaps {
        want.push(cat.t("project.damaged.snaps"));
    }
    want.push(cat.t("project.damaged.close"));
    assert_footer(&tag, &p, &want);
    let show = cat.t("xlsx_readonly.show_in_folder");
    assert!(p.controls.iter().any(|c| c.label == show), "{tag}: no Show in folder");
    let open = cat.t("project.damaged.open_prev");
    assert_eq!(p.controls.iter().any(|c| c.label == open), prev, "{tag}: Open previous copy");
    // Enter would open the copy, so only Esc is driven here
    let mut first = true;
    let mut shown = true;
    pressed(egui::Key::Escape, |ctx| {
        shown = crate::project_ui::layout_damaged(&mut app, ctx, first, prev, snaps);
        first = false;
    });
    assert!(!shown, "{tag}: Esc did not close");
}

fn damaged_all(lang: Lang) {
    damaged_file(lang, true, true);
    damaged_file(lang, true, false);
    damaged_file(lang, false, true);
    damaged_file(lang, false, false);
}

fn blocking_error(lang: Lang, with_elements: bool) {
    let cat = Catalog::new(lang);
    let elements = if with_elements { vec![crate::editor::Sel::Wall(1)] } else { vec![] };
    let text = long_ru(2);
    let mut app = app_neutral(lang);
    app.block_error(text.clone(), elements.clone());
    let p = probe_id(modal_id("blocking"), |ctx| app.blocking_window(ctx));
    let tag = format!("blocking {lang:?} elements {with_elements}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    assert_narrow(&tag, &p);
    // Undo (Enter) on the far right; the danger Delete is never the default
    let (undo, del, show) =
        (cat.t("warn.block.undo"), cat.t("warn.block.delete"), cat.t("warn.show"));
    assert_footer(&tag, &p, &[undo, del, show]);
    // Enter runs Undo at most; Esc leaves the error open: it demands a choice
    let mut app = app_neutral(lang);
    app.block_error(text.clone(), elements.clone());
    pressed(egui::Key::Enter, |ctx| app.blocking_window(ctx));
    assert!(app.ed.selection.is_empty(), "{tag}: Enter deleted or selected the elements");
    let mut app = app_neutral(lang);
    app.block_error(text, elements);
    pressed(egui::Key::Escape, |ctx| app.blocking_window(ctx));
    assert!(app.blocking.is_some(), "{tag}: Esc dismissed an error that needs a choice");
}

en_ru!(delete_confirm, delete_confirm_en, delete_confirm_ru);
en_ru!(phase_delete, phase_delete_en, phase_delete_ru);
fn template_delete_both(lang: Lang) {
    template_delete(lang, 0);
    template_delete(lang, 3);
}

fn blocking_both(lang: Lang) {
    blocking_error(lang, true);
    blocking_error(lang, false);
}

en_ru!(template_delete_both, template_delete_en, template_delete_ru);
en_ru!(unsaved_ask, unsaved_ask_en, unsaved_ask_ru);
en_ru!(closing_note_probe, closing_note_en, closing_note_ru);
en_ru!(save_failed_all, save_failed_en, save_failed_ru);
en_ru!(damaged_all, damaged_file_en, damaged_file_ru);
en_ru!(blocking_both, blocking_en, blocking_ru);

/// The overwrite question raised by the project's Save: Back to editing on the far right (also
/// Enter), Replace values beside it, Esc goes back.
fn project_overwrite(lang: Lang) {
    let cat = Catalog::new(lang);
    let ask = |ctx: &egui::Context| crate::family_editor::layout_project_modal(ctx, &cat);
    let p = probe_id(modal_id("family_overwrite.ow"), |ctx| {
        let _ = ask(ctx);
    });
    let tag = format!("family_overwrite.ow project {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    assert_footer(&tag, &p, &[cat.t("family_overwrite.ow.back"), cat.t("family_overwrite.ow.replace")]);
    let mut got = None;
    pressed(egui::Key::Enter, |ctx| got = got.or(ask(ctx)));
    assert_ne!(got, Some(true), "{tag}: Enter replaced the values");
    let mut got = None;
    pressed(egui::Key::Escape, |ctx| got = got.or(ask(ctx)));
    assert_eq!(got, Some(false), "{tag}: Esc did not go back");
}

en_ru!(project_overwrite, project_overwrite_en, project_overwrite_ru);

/// Default window sizes of the main screen; the Properties panel is about 300 points wide.
const MAIN_SCREENS: [(f32, f32); 2] = [(1280.0, 720.0), (1600.0, 900.0)];
const PROPS_WIDTHS: [f32; 2] = [280.0, 300.0];

/// Summary rows of the Properties panel (model totals, stairs).
const SUMMARY_KEYS: [&str; 13] = [
    "type_dialog.warn.levels",
    "type_dialog.warn.walls",
    "type_dialog.warn.wall_area",
    "type_dialog.warn.wall_vol",
    "type_dialog.warn.doors_windows",
    "type_dialog.warn.slabs",
    "type_dialog.warn.slab_area",
    "type_dialog.warn.slab_vol",
    "type_dialog.warn.rooms",
    "type_dialog.warn.room_area",
    "type_dialog.warn.curtains",
    "type_dialog.warn.panel_area",
    "geom.max_riser",
];

fn props_panel(screen: (f32, f32), w: f32, add: impl FnMut(&mut egui::Ui)) -> Probe {
    let mut add = add;
    probe_on(egui::Id::new("ux64h.props"), screen, |ctx| {
        egui::SidePanel::right("ux64h.props").exact_width(w).show(ctx, |ui| add(ui));
    })
}

fn summary_keys(ru: bool) {
    for screen in MAIN_SCREENS {
        for w in PROPS_WIDTHS {
            let p = props_panel(screen, w, |ui| {
                crate::ui_kit::props(ui, "ux64h.tot", |ui| {
                    for k in SUMMARY_KEYS {
                        crate::ui_kit::kv_num(ui, &crate::tr_lang(ru, k, &[]), "1");
                    }
                });
            });
            let tag = format!("summary keys ru={ru} {screen:?} panel {w}");
            assert!(p.elided.is_empty(), "{tag}: elided {:?}", p.elided);
            for k in SUMMARY_KEYS {
                let t = crate::tr_lang(ru, k, &[]);
                assert!(p.text.iter().any(|s| s == &t), "{tag}: {t:?} not on one line");
            }
        }
    }
}

#[test]
fn summary_keys_stay_whole_en() {
    summary_keys(false);
}

#[test]
fn summary_keys_stay_whole_ru() {
    summary_keys(true);
}

/// Phase rows of the Properties grid with the standard phases; `cur` is the current one.
fn phase_combo(ru: bool, cur: i64) {
    let phases: Vec<ddc_model::Phase> = ddc_model::DEFAULT_PHASES
        .iter()
        .enumerate()
        .map(|(i, (k, n))| ddc_model::Phase {
            id: i as i64 + 1,
            name: (*n).into(),
            seq: i as i64,
            key: Some((*k).into()),
        })
        .collect();
    for screen in MAIN_SCREENS {
        for w in PROPS_WIDTHS {
            let (mut c, mut d) = (cur, None);
            let p = props_panel(screen, w, |ui| {
                crate::ui_kit::props(ui, "ux64h.ph", |ui| {
                    crate::phase_ui::phase_rows(ui, &phases, Some(cur), &mut c, &mut d, ru);
                });
            });
            let tag = format!("phase combo ru={ru} cur={cur} {screen:?} panel {w}");
            assert!(p.elided.is_empty(), "{tag}: elided {:?}", p.elided);
            let shown = crate::phase_ui::label(&phases[cur as usize - 1], ru);
            assert!(p.text.iter().any(|s| s == &shown), "{tag}: {shown:?} missing");
        }
    }
}

#[test]
fn phase_combo_shows_the_current_phase_whole_en() {
    phase_combo(false, 1);
    phase_combo(false, 2);
}

#[test]
fn phase_combo_shows_the_current_phase_whole_ru() {
    for cur in 1..=3 {
        phase_combo(true, cur);
    }
}

/// The select hint on the status bar: parts that do not fit are dropped whole, never a lone
/// ellipsis, and a hint that is cut keeps its tooltip (`paint` returns true).
fn status_hint(lang: Lang) {
    for screen in MAIN_SCREENS {
        let mut a = app_in(lang);
        let txt = a.sh.cat.t("hint.select.idle");
        assert_ne!(txt, "hint.select.idle", "hint text missing");
        let p = probe_on(egui::Id::new("ux64h.sb"), screen, |ctx| {
            a.hint = Some(("select".into(), "idle".into()));
            egui::TopBottomPanel::bottom("ux64h.sb")
                .exact_height(crate::shell::status_bar::BAR_H)
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| crate::shell::status_bar::show(&mut a, ui));
        });
        let tag = format!("status hint {lang:?} {screen:?}");
        assert!(!p.text.iter().any(|s| s.trim() == "\u{2026}"), "{tag}: lone ellipsis");
        for part in crate::shell::status_bar::parse_hint(&txt) {
            let s = match part {
                crate::shell::status_bar::Part::Text(t) => t,
                crate::shell::status_bar::Part::Key { desc, .. } => desc,
            };
            assert!(!p.elided.iter().any(|e| e == &s), "{tag}: part {s:?} elided");
        }
    }
}

#[test]
fn status_hint_has_no_lone_ellipsis_en() {
    status_hint(Lang::En);
}

#[test]
fn status_hint_has_no_lone_ellipsis_ru() {
    status_hint(Lang::Ru);
}

/// Drawing on the processor: the status chip shows its whole label at every window width,
/// and after the click it says the graphics card comes at the next start.
fn soft_chip(lang: Lang) {
    for screen in MAIN_SCREENS {
        let mut a = app_in(lang);
        a.view.gfx.set_adapter_for_test(ddc_render::AdapterFacts {
            class: ddc_render::perf::GpuClass::Cpu,
            ..Default::default()
        });
        let want = a.sh.cat.t("gfx.chip.soft");
        let tag = format!("software chip {lang:?} {screen:?}");
        let mut draw = |a: &mut crate::shell::EditorApp| {
            probe_on(egui::Id::new("perf1.sb"), screen, |ctx| {
                egui::TopBottomPanel::bottom("perf1.sb")
                    .exact_height(crate::shell::status_bar::BAR_H)
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| crate::shell::status_bar::show(a, ui));
            })
        };
        let p = draw(&mut a);
        assert!(p.text.iter().any(|s| s == &want), "{tag}: {:?}", p.text);
        assert!(!p.elided.iter().any(|e| e == &want), "{tag}: elided");
        // what the click sets (the click itself also clears the real start file)
        a.view.gfx.card_next = true;
        let next = a.sh.cat.t("gfx.chip.next");
        let p = draw(&mut a);
        assert!(p.text.iter().any(|s| s == &next), "{tag}: after the click {:?}", p.text);
    }
}

/// The start screen opens with the assistant: the "Describe the project" card shows its title,
/// explanation and button whole, inside the window, at every window width.
fn start_ai_card(lang: Lang) {
    for screen in MAIN_SCREENS {
        let mut a = app_in(lang);
        a.open_start();
        let p = probe_on(egui::Id::new("perf1.start"), screen, |ctx| a.ui(ctx, None));
        let tag = format!("start AI card {lang:?} {screen:?}");
        let sr = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(screen.0, screen.1));
        for key in ["start.ai.title", "start.ai.sub", "start.ai.go"] {
            let want = a.sh.cat.t(key);
            let span = p.spans.iter().find(|(s, _)| s == &want);
            let (_, r) = span.unwrap_or_else(|| panic!("{tag}: {key} not drawn: {:?}", p.text));
            assert!(sr.contains_rect(*r), "{tag}: {key} outside the window {r:?}");
            assert!(!p.elided.iter().any(|e| e == &want), "{tag}: {key} elided");
        }
    }
}

#[test]
fn start_ai_card_whole_en() {
    start_ai_card(Lang::En);
}

#[test]
fn start_ai_card_whole_ru() {
    start_ai_card(Lang::Ru);
}

#[test]
fn soft_chip_shows_whole_en() {
    soft_chip(Lang::En);
}

#[test]
fn soft_chip_shows_whole_ru() {
    soft_chip(Lang::Ru);
}

/// The ribbon of every tab, drawn headless at the window widths: every control stays inside the
/// window, no caption is elided, and from 1440 points up no command of the band is icon-only.
fn ribbon_tabs(lang: Lang) {
    use ddc_ui::icons::IconCache;
    use ddc_ui::theme::ribbon as k;
    use ddc_ui::widgets::ribbon::Ribbon;
    let reg = ddc_ui::Registry::builtin();
    let cat = Catalog::new(lang);
    let th = Theme::new(Mode::Dark, Density::Compact);
    // caret parts are named "<command> - <tip>"; a name that merely holds the tip word is not one
    let split_tip = format!(" - {}", cat.t("ribbon.split_tip"));
    let mut checked = 0;
    for tab in reg.tabs() {
        // 1024 and 1280: narrow windows with collapsed groups; the rest are the standard widths
        for w in [1024.0_f32, 1280.0, 1440.0, 1600.0, 1920.0] {
            let mut rb = Ribbon::default();
            rb.tab = tab.clone();
            let mut icons = IconCache::default();
            let id = egui::Id::new(("ux65h.ribbon", tab.as_str()));
            let p = probe_on(id, (w, 300.0), |ctx| {
                egui::TopBottomPanel::top("ux65h.ribbon")
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        rb.show(ui, &th, &cat, &reg, &mut icons, None);
                    });
            });
            let tag = format!("ribbon {tab} {lang:?} @{w}");
            assert!(!p.controls.is_empty(), "{tag}: nothing drawn");
            for c in &p.controls {
                assert!(
                    c.rect.left() >= -EPS && c.rect.right() <= w + EPS,
                    "{tag}: {:?} {:?} leaves the window",
                    c.label,
                    c.rect
                );
            }
            assert!(p.elided.is_empty(), "{tag}: elided {:?}", p.elided);
            if w < 1440.0 || !matches!(tab.as_str(), "arch" | "struct" | "systems") {
                continue;
            }
            let band_bottom = k::TAB_H + k::PAD_Y + k::BAND_H;
            // a button is named when a text is painted inside it, or inside the caret part right
            // under it (a large split button carries its label there)
            let painted = |r: egui::Rect| {
                p.spans
                    .iter()
                    .any(|(s, sr)| !s.trim().is_empty() && r.expand(EPS).contains_rect(*sr))
            };
            let named = |r: egui::Rect| {
                painted(r)
                    || p.controls.iter().any(|d| {
                        d.label.ends_with(&split_tip)
                            && (d.rect.left() - r.left()).abs() < EPS
                            && (d.rect.top() - r.bottom()).abs() < EPS
                            && painted(d.rect)
                    })
            };
            for c in &p.controls {
                let in_band = c.rect.top() >= k::TAB_H && c.rect.center().y < band_bottom;
                let caret = c.label.ends_with(&split_tip);
                if in_band && !caret && c.role == accesskit::Role::Button {
                    assert!(
                        c.rect.width() > 2.0 * k::SMALL_W || named(c.rect),
                        "{tag}: {:?} is icon-only ({:?})",
                        c.label,
                        c.rect
                    );
                    checked += 1;
                }
            }
        }
    }
    assert!(checked >= 20, "{lang:?}: only {checked} band buttons checked");
}

/// XLSX is the main workflow: the Data tab shows Sync with XLSX as a labelled button next to
/// export and import at the standard widths, and it has its own icon.
fn data_tab_sync_button(lang: Lang) {
    use ddc_ui::icons::IconCache;
    use ddc_ui::widgets::ribbon::{short_label, Ribbon};
    let reg = ddc_ui::Registry::builtin();
    let cat = Catalog::new(lang);
    let th = Theme::new(Mode::Dark, Density::Compact);
    let sync = reg.get("data.sync_xlsx").expect("sync");
    assert_eq!(sync.icon, "xlsx-sync");
    assert_ne!(sync.icon, reg.get("data.src_delta").expect("src_delta").icon);
    let label = short_label(&cat, sync);
    for w in [1280.0_f32, 1440.0, 1920.0] {
        let mut rb = Ribbon::default();
        rb.tab = "data".into();
        let mut icons = IconCache::default();
        let p = probe_on(egui::Id::new("r98a.ribbon"), (w, 300.0), |ctx| {
            egui::TopBottomPanel::top("r98a.ribbon")
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| {
                    rb.show(ui, &th, &cat, &reg, &mut icons, None);
                });
        });
        let tag = format!("data tab {lang:?} @{w}");
        let full = cat.t("cmd.data.sync_xlsx");
        let c = p
            .controls
            .iter()
            .find(|c| c.label.contains(&label) || c.label.contains(&full))
            .unwrap_or_else(|| panic!("{tag}: no Sync button in {:?}", p.controls));
        assert!(c.rect.width() > 40.0, "{tag}: icon-only {:?}", c.rect);
        assert!(p.elided.is_empty(), "{tag}: elided {:?}", p.elided);
    }
}

#[test]
fn data_tab_has_sync_with_xlsx_en() {
    data_tab_sync_button(Lang::En);
}

#[test]
fn data_tab_has_sync_with_xlsx_ru() {
    data_tab_sync_button(Lang::Ru);
}

#[test]
fn ribbon_tabs_en() {
    ribbon_tabs(Lang::En);
}

#[test]
fn ribbon_tabs_ru() {
    ribbon_tabs(Lang::Ru);
}

/// A collapsed group is one drop-down: clicked like a user does, it lists every command of the
/// group (variants included) with a real label, inside the window; Esc closes it.
fn ribbon_collapsed_menus(lang: Lang) {
    use ddc_ui::icons::IconCache;
    use ddc_ui::theme::ribbon as k;
    use ddc_ui::widgets::ribbon::{group_name, Ribbon};
    let reg = ddc_ui::Registry::builtin();
    let cat = Catalog::new(lang);
    let th = Theme::new(Mode::Dark, Density::Compact);
    let band_end = k::TAB_H + k::PAD_Y + k::BAND_H + k::TITLE_H;
    let key = |code: egui::Key| egui::Event::Key {
        key: code,
        physical_key: Some(code),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let mut clicked = 0;
    for tab in reg.tabs() {
        let cmds: Vec<&ddc_ui::Command> = reg
            .all()
            .iter()
            .filter(|c| c.tab.as_deref() == Some(tab.as_str()) && c.on("R"))
            .collect();
        let folded: std::collections::HashSet<&str> = cmds
            .iter()
            .flat_map(|c| c.variants.iter().map(String::as_str))
            .collect();
        for w in [1280.0_f32, 1440.0, 1920.0] {
            let screen = (w, 900.0);
            let mut rb = Ribbon::default();
            rb.tab = tab.clone();
            let mut icons = IconCache::default();
            let (ctx, mut dth) = driven_ctx();
            let id = egui::Id::new(("ux65h.collapsed", tab.as_str()));
            let mut frame = |ctx: &egui::Context| {
                egui::TopBottomPanel::top("ux65h.collapsed")
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        rb.show(ui, &th, &cat, &reg, &mut icons, None);
                    });
            };
            let mut p = pass_on(&ctx, &mut dth, id, screen, vec![], &mut frame);
            for _ in 0..3 {
                p = pass_on(&ctx, &mut dth, id, screen, vec![], &mut frame);
            }
            let mut groups: Vec<&str> = vec![];
            for c in cmds.iter().filter(|c| !folded.contains(c.id.as_str())) {
                let g = group_name(c);
                if !groups.contains(&g) {
                    groups.push(g);
                }
            }
            for g in groups {
                let members: Vec<&&ddc_ui::Command> = cmds
                    .iter()
                    .filter(|c| !folded.contains(c.id.as_str()) && group_name(c) == g)
                    .collect();
                let want: Vec<String> = members
                    .iter()
                    .flat_map(|c| std::iter::once(c.id.as_str()).chain(c.variants.iter().map(String::as_str)))
                    .filter_map(|id| reg.get(id))
                    .map(|c| cat.t(&c.label_key()))
                    .collect();
                let title = cat.t(&format!("rgroup.{g}"));
                // a collapsed group is one button named by the group, its commands drawn nowhere
                // else; a command named like its group (Source) is no head while the group is open
                let named = |c: &Control, l: &String| {
                    c.label == *l || c.label.starts_with(&format!("{l} (")) || c.label.starts_with(&format!("{l} - "))
                };
                let Some(head) = p.controls.iter().find(|c| {
                    let shown = p.controls.iter().any(|o| o.rect != c.rect && want.iter().any(|l| named(o, l)));
                    c.label == title && c.rect.top() >= k::TAB_H && want.len() >= 2 && !shown
                }) else {
                    continue;
                };
                let tag = format!("{lang:?} {tab} @{w}: group {title}");
                let at = head.rect.center();
                let down = egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                };
                let up = egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                };
                let _ = pass_on(&ctx, &mut dth, id, screen, vec![egui::Event::PointerMoved(at)], &mut frame);
                let _ = pass_on(&ctx, &mut dth, id, screen, vec![down], &mut frame);
                let _ = pass_on(&ctx, &mut dth, id, screen, vec![up], &mut frame);
                let open = pass_on(&ctx, &mut dth, id, screen, vec![], &mut frame);
                // the menu opens under the collapsed button, which spans the band without the
                // title strip: its rows start below the button, every ribbon control above it
                let below = head.rect.bottom();
                assert!(below <= band_end + EPS, "{tag}: group button {:?} below the band", head.rect);
                let rows: Vec<&Control> =
                    open.controls.iter().filter(|c| c.rect.top() >= below - EPS).collect();
                for l in &want {
                    assert!(
                        // the row name is the label, then " (keys)" or " - reason" as its suffix
                        rows.iter().any(|r| {
                            r.label == *l
                                || r.label.starts_with(&format!("{l} ("))
                                || r.label.starts_with(&format!("{l} - "))
                        }),
                        "{tag}: command {l:?} missing in the menu {:?}",
                        rows.iter().map(|r| &r.label).collect::<Vec<_>>()
                    );
                }
                for r in &rows {
                    assert!(!r.label.trim().is_empty(), "{tag}: unnamed menu row {:?}", r.rect);
                    assert!(!r.label.starts_with("cmd."), "{tag}: raw key {:?}", r.label);
                    assert!(
                        r.rect.left() >= -EPS
                            && r.rect.right() <= w + EPS
                            && r.rect.bottom() <= screen.1 + EPS,
                        "{tag}: {:?} {:?} leaves the window",
                        r.label,
                        r.rect
                    );
                }
                let _ = pass_on(&ctx, &mut dth, id, screen, vec![key(egui::Key::Escape)], &mut frame);
                let shut = pass_on(&ctx, &mut dth, id, screen, vec![], &mut frame);
                // the ribbon's own controls on the title strip were there before the menu opened
                let before = |c: &Control| {
                    p.controls.iter().any(|b| b.label == c.label && (b.rect.min - c.rect.min).length() <= EPS)
                };
                let left: Vec<_> = shut
                    .controls
                    .iter()
                    .filter(|c| c.rect.top() >= below - EPS && !before(c))
                    .map(|c| (&c.label, c.rect))
                    .collect();
                assert!(left.is_empty(), "{tag}: Esc left the menu open: head {:?}, {left:?}", head.rect);
                clicked += 1;
            }
        }
    }
    assert!(clicked >= 3, "{lang:?}: only {clicked} collapsed groups opened");
}

#[test]
fn ribbon_collapsed_menus_en() {
    ribbon_collapsed_menus(Lang::En);
}

#[test]
fn ribbon_collapsed_menus_ru() {
    ribbon_collapsed_menus(Lang::Ru);
}

use crate::ai::ui::tests_states::{layout_probe_shape, Shape};

/// The connected assistant panel in `shape`; nothing leaves the panel. Names of rows the design
/// elides with a tooltip (item titles) are allowed to be cut.
fn ai_shape(lang: Lang, shape: Shape) -> Probe {
    let p = layout_probe_shape(lang, true, shape);
    let bad: Vec<String> = violations(&p, false)
        .into_iter()
        .filter(|b| !b.ends_with("is elided"))
        .collect();
    let tag = format!("ai panel {lang:?} {:?} items={}", shape.size, shape.items);
    assert!(bad.is_empty(), "{tag}: {}", bad.join("; "));
    p
}

fn span_top(p: &Probe, text: &str) -> Option<f32> {
    p.spans.iter().filter(|(t, _)| t.contains(text)).map(|(_, r)| r.top()).reduce(f32::min)
}

/// Panel open and idle: the starter rows are quiet rows grouped right above the composer.
fn ai_open_prompts(lang: Lang) {
    let cat = Catalog::new(lang);
    for width in [280.0_f32, 360.0, 640.0] {
        let shape = Shape { items: 0, log: false, size: egui::vec2(width, 900.0) };
        let p = ai_shape(lang, shape);
        let input = p
            .controls
            .iter()
            .find(|c| is_text_field(c))
            .unwrap_or_else(|| panic!("{lang:?} {width}: no composer"));
        let title = span_top(&p, &cat.t("ai.state.idle")).expect("idle title");
        for key in ["ai.suggest.count", "ai.suggest.check"] {
            let label = cat.t(key);
            let row = p
                .controls
                .iter()
                .find(|c| c.label == label)
                .unwrap_or_else(|| panic!("{lang:?} {width}: no row {key}"));
            assert!(row.rect.top() > title, "{lang:?} {width}: row {key} above the title");
            assert!(
                row.rect.bottom() <= input.rect.top() + EPS,
                "{lang:?} {width}: row {key} overlaps the composer"
            );
            assert!(
                input.rect.top() - row.rect.bottom() < 160.0,
                "{lang:?} {width}: row {key} floats far above the composer"
            );
        }
    }
}

/// Narrow window: the shortest usable panel keeps the composer and every row inside.
fn ai_narrow(lang: Lang) {
    for items in [0usize, 3] {
        let shape = Shape { items, log: false, size: egui::vec2(280.0, 420.0) };
        let p = ai_shape(lang, shape);
        assert!(
            p.controls.iter().any(is_text_field),
            "{lang:?} items={items}: composer lost in a narrow window"
        );
    }
}

/// A long conversation scrolls inside the panel; the composer stays pinned and visible.
fn ai_long_conversation(lang: Lang) {
    let shape = Shape { items: 0, log: true, size: egui::vec2(360.0, 700.0) };
    let p = ai_shape(lang, shape);
    let win = p.inner.expect("window");
    let input = p
        .controls
        .iter()
        .find(|c| is_text_field(c))
        .expect("composer");
    assert!(input.rect.bottom() <= win.bottom() + EPS, "{lang:?}: composer pushed out");
}

/// The receipt of an apply that wrote a draft family: one "Make working" button, inside the panel.
fn ai_family_receipt(lang: Lang) {
    let cat = Catalog::new(lang);
    let p = crate::ai::ui::tests_states::layout_probe_draft_receipt(lang);
    assert_contract(&format!("ai family receipt {lang:?}"), &p, false);
    let label = cat.t("ai.receipt.promote");
    let found: Vec<_> = p.controls.iter().filter(|c| c.label == label).collect();
    assert_eq!(found.len(), 1, "{lang:?}: one Make working button");
    let win = p.inner.expect("window");
    assert!(win.contains_rect(found[0].rect.shrink(EPS)), "{lang:?}: button outside the panel");
}

#[test]
fn ai_family_receipt_en() {
    ai_family_receipt(Lang::En);
}

#[test]
fn ai_family_receipt_ru() {
    ai_family_receipt(Lang::Ru);
}

/// A draft of 200 changes: the counts line comes first, Apply stays inside the panel.
fn ai_plan_card(lang: Lang) {
    let cat = Catalog::new(lang);
    let shape = Shape { items: 200, log: false, size: egui::vec2(360.0, 900.0) };
    let p = ai_shape(lang, shape);
    let win = p.inner.expect("window");
    let counts = span_top(&p, &cat.t("ai.plan.title")).expect("counts line");
    let first = span_top(&p, "Change 0 ").expect("first item");
    assert!(counts < first, "{lang:?}: counts {counts} not above the first item {first}");
    let apply = cat.plural("ai.items.apply", 200);
    let btn = p
        .controls
        .iter()
        .find(|c| c.label == apply)
        .unwrap_or_else(|| panic!("{lang:?}: no Apply {apply:?}"));
    assert!(win.contains_rect(btn.rect.shrink(EPS)), "{lang:?}: Apply outside the panel");
}

/// 1600 px, assistant open: Properties folds, the canvas keeps 960 px; closing restores it.
fn ai_dock_fold(lang: Lang) {
    use crate::ai::ui::dock_fold::{canvas_width, sync, Fold, CANVAS_MIN};
    let mut prefs = app_in(lang).prefs.clone();
    prefs.browser_open = true;
    prefs.props_open = true;
    prefs.browser_width = crate::shell::dock_ui::BROWSER_W;
    prefs.props_width = crate::ui_kit::PROPS_W;
    prefs.ai_panel_w = 360.0;
    let mut fold = Fold::default();
    sync(&mut fold, &mut prefs, 1600.0, false);
    assert!(prefs.props_open, "{lang:?}: closed panel changed Properties");
    sync(&mut fold, &mut prefs, 1600.0, true);
    assert!(!prefs.props_open, "{lang:?}: Properties stayed beside the assistant");
    assert!(canvas_width(1600.0, &prefs, 360.0) >= CANVAS_MIN, "{lang:?}: canvas too narrow");
    sync(&mut fold, &mut prefs, 1600.0, false);
    assert!(prefs.props_open, "{lang:?}: Properties not restored on close");
}

en_ru!(ai_open_prompts, ai_open_prompts_en, ai_open_prompts_ru);
en_ru!(ai_narrow, ai_narrow_en, ai_narrow_ru);
en_ru!(ai_long_conversation, ai_long_conversation_en, ai_long_conversation_ru);
en_ru!(ai_plan_card, ai_plan_card_en, ai_plan_card_ru);
en_ru!(ai_dock_fold, ai_dock_fold_en, ai_dock_fold_ru);

// ---- project browser: one row system, quiet empty sections, pinned headers ----

use crate::shell::panels::browser::ROW_H as BROW_H;
use crate::shell::panels::browser_sec;

/// Rows sit edge to edge, as in the browser panel.
const BROW_FLUSH: f32 = 0.0;

/// Tall enough that every row of the "many items" cases is painted.
const BROWSER_SCREEN: (f32, f32) = (400.0, 3200.0);

fn browser_probe(app: &mut EditorApp, width: f32) -> Probe {
    probe_on(egui::Id::new("ddc-browser"), BROWSER_SCREEN, |ctx| app.browser_for_test(ctx, width))
}

fn rect_of(p: &Probe, text: &str) -> egui::Rect {
    // the last one painted: a pinned header is drawn over its scrolled-away twin
    match p.spans.iter().rev().find(|(s, _)| s == text) {
        Some((_, r)) => *r,
        None => panic!("text {text:?} was not drawn"),
    }
}

/// Rects of the painted texts that start with `prefix`, top to bottom.
fn rects_starting(p: &Probe, prefix: &str) -> Vec<egui::Rect> {
    let mut v: Vec<egui::Rect> =
        p.spans.iter().filter(|(s, _)| s.starts_with(prefix)).map(|(_, r)| *r).collect();
    v.sort_by(|a, b| a.top().total_cmp(&b.top()));
    v
}

fn browser_empty(lang: Lang, width: f32) {
    let ru = lang == Lang::Ru;
    let mut app = app_neutral(lang);
    let p = browser_probe(&mut app, width);
    let cat = &app.sh.cat;
    let tag = format!("browser empty {lang:?} {width}");
    let add = crate::tr_lang(ru, "shv.new_sheet", &[]);
    let add_level = format!("+ {}", cat.t("cmd.mgr.level"));
    let keys = ["browser.views", "browser.levels", "browser.sheets"];
    let heads = keys.map(|k| cat.t(k).to_uppercase());
    // no block for the empty section: only a header and one quiet add row
    let title = crate::tr_lang(ru, "empty.sheets.title", &[]);
    assert!(!p.text.iter().any(|t| *t == title), "{tag}: the empty block is back");
    let dy = rect_of(&p, &add).center().y - rect_of(&p, &heads[2]).center().y;
    assert!(dy > 0.0 && dy < BROW_H * 1.5, "{tag}: sheets section is {dy} high");
    // one label x for headers and rows of every section
    let plan = format!("{} \u{b7} Ground", cat.t("browser.plan"));
    let mut labels = vec![plan, "Ground".to_string(), cat.t("cmd.view.3d"), add, add_level];
    labels.extend(heads);
    let x0 = rect_of(&p, &labels[0]).left();
    for l in &labels {
        let x = rect_of(&p, l).left();
        assert!((x - x0).abs() < EPS, "{tag}: {l:?} starts at {x}, rows at {x0}");
    }
    assert!(p.spans.iter().all(|(_, r)| r.right() <= width + EPS), "{tag}: text leaves the panel");
}

fn browser_many(lang: Lang, width: f32) {
    let mut app = app_neutral(lang);
    let long = "with an unusually long descriptive name that cannot fit the panel";
    for i in 1..=30 {
        app.ed.model.add_level(&format!("Level {i:02} {long}"), f64::from(i) * 3.0).unwrap();
        let sheet = ddc_model::SheetDef::new(&format!("A-{i:03}"), &format!("Plan {long}"));
        app.ed.model.add_sheet(&sheet).unwrap();
    }
    let p = browser_probe(&mut app, width);
    let tag = format!("browser many {lang:?} {width}");
    let levels = rects_starting(&p, "Level ");
    let sheets = rects_starting(&p, "A-");
    assert!(levels.len() >= 20 && sheets.len() >= 20, "{tag}: rows missing");
    for rows in [&levels, &sheets] {
        for w in rows.windows(2) {
            let dy = w[1].center().y - w[0].center().y;
            assert!((dy - BROW_H).abs() < EPS, "{tag}: rows {dy} apart, not {BROW_H}");
        }
    }
    assert!((levels[0].left() - sheets[0].left()).abs() < EPS, "{tag}: label x differs");
    assert!(p.elided.iter().any(|e| e.starts_with("Level ")), "{tag}: long names are not cut");
    assert!(p.spans.iter().all(|(_, r)| r.right() <= width + EPS), "{tag}: text leaves the panel");
    // the counts sit on one right edge
    let (n_levels, n_sheets) = (rect_of(&p, "31"), rect_of(&p, "30"));
    assert!((n_levels.right() - n_sheets.right()).abs() < EPS, "{tag}: counts not aligned");
}

#[test]
fn browser_empty_project_en() {
    browser_empty(Lang::En, crate::shell::dock_ui::BROWSER_W);
    browser_empty(Lang::En, crate::shell::dock_ui::BROWSER_MIN_W);
}

#[test]
fn browser_empty_project_ru() {
    browser_empty(Lang::Ru, crate::shell::dock_ui::BROWSER_W);
    browser_empty(Lang::Ru, crate::shell::dock_ui::BROWSER_MIN_W);
}

#[test]
fn browser_many_long_names_en() {
    browser_many(Lang::En, crate::shell::dock_ui::BROWSER_W);
    browser_many(Lang::En, crate::shell::dock_ui::BROWSER_MIN_W);
}

#[test]
fn browser_many_long_names_ru() {
    browser_many(Lang::Ru, crate::shell::dock_ui::BROWSER_W);
    browser_many(Lang::Ru, crate::shell::dock_ui::BROWSER_MIN_W);
}

/// Content of a scrolled browser column: three sections of 30 rows.
fn sticky(offset: f32) -> (Probe, f32) {
    let top = std::cell::Cell::new(0.0_f32);
    let mut icons = ddc_ui::icons::IconCache::default();
    let p = probe_on(egui::Id::new("ux65d.sticky"), (300.0, 400.0), |ctx| {
        egui::SidePanel::left("ux65d.sticky").exact_width(240.0).show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .vertical_scroll_offset(offset)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = BROW_FLUSH;
                    top.set(ui.clip_rect().top());
                    let th = ddc_ui::theme::of(ui.ctx());
                    for (k, name) in ["Alpha", "Beta", "Gamma"].iter().enumerate() {
                        browser_sec::section(ui, &th, name, name, Some(30), None);
                        for i in 0..30 {
                            let label = format!("Item {k}-{i}");
                            let s = egui::Sense::click();
                            let icon = Some("level");
                            browser_sec::row(ui, &th, &mut icons, icon, &label, "", false, 0, s);
                        }
                    }
                    browser_sec::end(ui, &th);
                });
        });
    });
    (p, top.get())
}

#[test]
fn section_header_stays_at_the_top_after_scrolling() {
    let (p, top) = sticky(400.0);
    let r = rect_of(&p, "ALPHA");
    assert!(r.top() >= top - EPS && r.top() < top + BROW_H, "pinned header at {r:?}, top {top}");
    assert!(!p.spans.iter().any(|(s, r)| s == "BETA" && r.top() < top + BROW_H));
}

#[test]
fn next_header_pushes_the_pinned_one_out() {
    // at 750 the Alpha section has 2 pt left: its header is pushed above the top edge
    let (p, top) = sticky(750.0);
    let r = rect_of(&p, "ALPHA");
    assert!(r.top() < top && r.top() > top - BROW_H, "pushed header at {r:?}, top {top}");
}

/// What a pinned-header scene did: Add seen, row clicked, fold state of the first section.
struct PinRun {
    add: bool,
    row: Option<usize>,
    open: bool,
    top: f32,
    rows: Vec<(usize, egui::Rect)>,
}

/// Three sections of 30 rows with an Add button on each header, scrolled to 400. `aim` picks the
/// click point from the settled frame (screen top, painted texts, row rects); `None` clicks
/// nothing. `target` scrolls that row of the first section into view like a keyboard move.
fn pin_scene(
    name: &str,
    target: Option<usize>,
    aim: impl Fn(&Probe, f32, &[(usize, egui::Rect)]) -> Option<egui::Pos2>,
) -> PinRun {
    use std::cell::{Cell, RefCell};
    let (add, row) = (Cell::new(false), Cell::new(None));
    let (open, top) = (Cell::new(true), Cell::new(0.0_f32));
    let rows = RefCell::new(Vec::<(usize, egui::Rect)>::new());
    let first = Cell::new(true);
    let want = Cell::new(false);
    let mut icons = ddc_ui::icons::IconCache::default();
    let (ctx, mut th) = driven_ctx();
    let id = egui::Id::new("r97f.pin");
    let mut frame = |ctx: &egui::Context| {
        rows.borrow_mut().clear();
        egui::SidePanel::left("r97f.pin").exact_width(240.0).show(ctx, |ui| {
            let mut sa = egui::ScrollArea::vertical().auto_shrink([false, false]);
            if first.get() {
                sa = sa.vertical_scroll_offset(400.0);
            }
            sa.show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = BROW_FLUSH;
                top.set(ui.clip_rect().top());
                let th = ddc_ui::theme::of(ui.ctx());
                for (k, n) in [name, "Beta", "Gamma"].iter().enumerate() {
                    let s = browser_sec::section(ui, &th, n, n, Some(30), Some("Add"));
                    if k == 0 {
                        add.set(add.get() | s.add);
                        open.set(s.open);
                    }
                    for i in 0..30 {
                        let label = format!("Item {k}-{i}");
                        let sense = egui::Sense::click();
                        let r = browser_sec::row(
                            ui, &th, &mut icons, None, &label, "", false, 0, sense,
                        );
                        if k == 0 {
                            rows.borrow_mut().push((i, r.rect));
                            if r.clicked() {
                                row.set(Some(i));
                            }
                            if want.get() && Some(i) == target {
                                ui.scroll_to_rect(r.rect.expand2(egui::vec2(0.0, BROW_H)), None);
                            }
                        }
                    }
                }
                browser_sec::end(ui, &th);
            });
        });
    };
    let mut seen = Probe::default();
    for _ in 0..FRAMES {
        seen = pass_on(&ctx, &mut th, id, (300.0, 400.0), vec![], &mut frame);
    }
    first.set(false);
    if target.is_some() {
        want.set(true);
        // egui animates scroll_to_rect at 1/60 s a pass: run until the longest animation is over
        let anim = ctx.style().scroll_animation.duration.max;
        for _ in 0..FRAMES + (anim * 60.0).ceil() as usize {
            seen = pass_on(&ctx, &mut th, id, (300.0, 400.0), vec![], &mut frame);
        }
        want.set(false);
    }
    let snap = rows.borrow().clone();
    if let Some(at) = aim(&seen, top.get(), &snap) {
        let b = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut go = |ev| pass_on(&ctx, &mut th, id, (300.0, 400.0), ev, &mut frame);
        go(vec![egui::Event::PointerMoved(at)]);
        go(vec![b(true)]);
        go(vec![b(false)]);
        for _ in 0..FRAMES {
            go(vec![]);
        }
    }
    let rows = rows.borrow().clone();
    PinRun { add: add.get(), row: row.get(), open: open.get(), top: top.get(), rows }
}

/// Header, the row scrolled under it, the row below it and Add, in en and ru.
fn pinned_header_clicks(name: &str) {
    let mid = |t: f32| egui::pos2(60.0, t + BROW_H / 2.0);
    // the strip is the header: it folds the section and no row gets the click
    let r = pin_scene(name, None, |_, t, _| Some(mid(t)));
    assert_eq!(r.row, None, "{name}: row under the pinned header got the click");
    assert!(!r.open && !r.add, "{name}: header click did not fold the section");
    // the first row fully below the strip takes its own click
    let below = |t: f32, rows: &[(usize, egui::Rect)]| {
        rows.iter().find(|(_, rc)| rc.top() >= t + BROW_H - EPS).copied()
    };
    let r = pin_scene(name, None, |_, t, rows| below(t, rows).map(|(_, rc)| rc.center()));
    let idle = pin_scene(name, None, |_, _, _| None);
    let want = below(idle.top, &idle.rows).map(|(i, _)| i);
    assert!(want.is_some() && r.row == want, "{name}: row below the header: {:?}", r.row);
    assert!(r.open && !r.add, "{name}: row click touched the header");
    // Add on the pinned header adds and leaves the fold alone
    let r = pin_scene(name, None, |p, t, _| {
        let c = rect_of(p, "30");
        Some(egui::pos2(c.left() - ddc_ui::theme::space::XS - BROW_H / 2.0, t + BROW_H / 2.0))
    });
    assert!(r.add, "{name}: pinned Add did nothing");
    assert!(r.open && r.row.is_none(), "{name}: pinned Add folded or hit a row");
}

#[test]
fn pinned_header_clicks_en() {
    pinned_header_clicks("Levels");
}

#[test]
fn pinned_header_clicks_ru() {
    pinned_header_clicks("\u{423}\u{440}\u{43e}\u{432}\u{43d}\u{438}");
}

/// A keyboard move to a row above the view leaves it fully below the pinned header.
fn pinned_keyboard_room(name: &str) {
    let r = pin_scene(name, Some(5), |_, _, _| None);
    let (_, rc) = r.rows.iter().find(|(i, _)| *i == 5).unwrap();
    let top = r.top;
    assert!(rc.top() >= top + BROW_H - EPS, "{name}: row 5 at {rc:?} under header, top {top}");
}

#[test]
fn pinned_keyboard_room_en() {
    pinned_keyboard_room("Levels");
}

#[test]
fn pinned_keyboard_room_ru() {
    pinned_keyboard_room("\u{423}\u{440}\u{43e}\u{432}\u{43d}\u{438}");
}

#[test]
fn kit_rows_share_one_height_and_label_x() {
    let mut icons = ddc_ui::icons::IconCache::default();
    let mut rects = vec![];
    let p = probe_on(egui::Id::new("ux65d.rows"), (300.0, 400.0), |ctx| {
        rects.clear();
        egui::SidePanel::left("ux65d.rows").exact_width(240.0).show(ctx, |ui| {
            let th = ddc_ui::theme::of(ui.ctx());
            ui.spacing_mut().item_spacing.y = BROW_FLUSH;
            let s = egui::Sense::click();
            let rows = [
                (Some("level"), "With icon", "+3,000", false),
                (None, "Without icon", "+3,000", false),
                (Some("level"), "Selected", "", true),
            ];
            for (icon, label, right, on) in rows {
                let r = browser_sec::row(ui, &th, &mut icons, icon, label, right, on, 0, s);
                rects.push(r.rect);
            }
            assert!(!browser_sec::add_row(ui, &th, "+ Add", ""));
        });
    });
    let heights: Vec<f32> = rects[..3].iter().map(|r| r.height()).collect();
    assert!(heights.iter().all(|h| (h - BROW_H).abs() < EPS), "heights {heights:?}");
    let x = rect_of(&p, "With icon").left();
    for l in ["Without icon", "Selected", "+ Add"] {
        assert!((rect_of(&p, l).left() - x).abs() < EPS, "{l:?} is off the label x");
    }
}

type Seg = (egui::Pos2, egui::Pos2, egui::Color32);

fn shape_lines(s: &egui::Shape, out: &mut Vec<Seg>) {
    match s {
        egui::Shape::LineSegment { points, stroke } => {
            out.push((points[0], points[1], stroke.color));
        }
        egui::Shape::Vec(v) => v.iter().for_each(|x| shape_lines(x, out)),
        _ => {}
    }
}

/// The plan canvas for several frames on `screen`, in light or dark, with or without a model
/// and a selection: the HUD stays on the canvas, beside the scale bar, and the axes are short.
fn plan_canvas(ru: bool, mode: Mode, screen: (f32, f32), model: bool, select: bool) {
    use crate::plan_hud as hud;
    use crate::wall_tool::{Event, Mode as Tool};
    use ddc_geom::v2;
    crate::with_test_lang(ru, || {
        let (ctx, _) = driven_ctx();
        let mut th = Theme::new(mode, Density::Compact);
        let tok = crate::ui_kit::from_theme(&th);
        let mut ed = Editor::new_in_memory().unwrap();
        if model {
            ed.set_mode(Tool::Rect);
            for e in [
                Event::Move(v2(-2.0, -1.0)),
                Event::Press(v2(-2.0, -1.0)),
                Event::Move(v2(3.0, 2.0)),
                Event::Release(v2(3.0, 2.0)),
            ] {
                ed.handle(e);
            }
            ed.set_mode(Tool::Select);
            assert!(!ed.walls.is_empty());
            if select {
                ed.selection = vec![crate::editor::Sel::Wall(ed.walls[0].id)];
            }
        }
        let mut view = crate::plan_view::PlanView::default();
        view.center = v2(0.0, 0.0);
        let mut p = Probe::default();
        for _ in 0..FRAMES {
            let mut frame = |ctx: &egui::Context| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| view.show(ui, &mut ed, &tok));
            };
            let id = egui::Id::new("ux65i.plan");
            p = pass_on(&ctx, &mut th, id, screen, vec![], &mut frame);
        }
        let tag = format!("plan ru={ru} {mode:?} {screen:?} model={model} select={select}");
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(screen.0, screen.1));
        let name = crate::tr_lang(ru, "view_gfx.view.plan", &[("name", ed.level_name().as_str())]);
        let fit = crate::tr_lang(ru, "plan.hud.fit", &[]);
        let denom = ed.model.anno_scale(ddc_model::AnnoView::Plan(ed.level)).unwrap_or(100.0);
        let ratio = hud::view_scale_text(denom, view.scale);
        let units = crate::units::Units::Metric;
        let bar = crate::units::display_length(
            crate::plan_view::scale_bar_len(view.scale, units),
            units,
        );
        let span = |s: &str| p.spans.iter().find(|(t, _)| t == s).map(|(_, r)| *r);
        let (fit_r, ratio_r) = (span(&fit), span(&ratio));
        let (fit_r, ratio_r) = (fit_r.expect("fit chip"), ratio_r.expect("scale chip"));
        let bar_r = span(&bar).expect("scale bar label");
        for r in [fit_r, ratio_r, bar_r] {
            assert!(canvas.contains_rect(r), "{tag}: {r:?} leaves the canvas");
        }
        assert!(!fit_r.intersects(bar_r) && !ratio_r.intersects(bar_r), "{tag}: covers the bar");
        assert!((fit_r.center().y - ratio_r.center().y).abs() < 0.5, "{tag}: baseline");
        if let Some(n) = span(&name) {
            assert!(canvas.contains_rect(n) && !n.intersects(bar_r), "{tag}: view name {n:?}");
        } else {
            assert!(screen.0 < 1000.0, "{tag}: view name {name:?} missing on a wide canvas");
        }
        let ctl = p.controls.iter().find(|c| c.label == fit);
        let ctl = ctl.unwrap_or_else(|| panic!("{tag}: fit chip is not a control"));
        assert!(canvas.contains_rect(ctl.rect), "{tag}: fit control {:?}", ctl.rect);
        assert!(p.elided.is_empty(), "{tag}: elided {:?}", p.elided);
        let mut lines = vec![];
        p.shapes.iter().for_each(|s| shape_lines(s, &mut lines));
        for i in 0..2 {
            let col = hud::axis_on_paper(&tok, i);
            let segs: Vec<&Seg> = lines.iter().filter(|s| s.2 == col).collect();
            assert!(!segs.is_empty(), "{tag}: axis {i} not drawn");
            for (a, b, _) in segs {
                let len = (*b - *a).length();
                assert!(len <= hud::AXIS_ARM_PX + hud::AXIS_TAIL_PX + 0.5, "{tag}: axis {len}");
            }
        }
        // the Settings toggle: same canvas, axes now run across the sheet
        ed.axes_pref = true;
        for _ in 0..FRAMES {
            let mut frame = |ctx: &egui::Context| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| view.show(ui, &mut ed, &tok));
            };
            p = pass_on(&ctx, &mut th, egui::Id::new("ux65i.plan"), screen, vec![], &mut frame);
        }
        let mut lines = vec![];
        p.shapes.iter().for_each(|s| shape_lines(s, &mut lines));
        for i in 0..2 {
            let col = hud::axis_on_paper(&tok, i);
            let longest = lines
                .iter()
                .filter(|s| s.2 == col)
                .map(|(a, b, _)| (*b - *a).length())
                .fold(0.0_f32, f32::max);
            let side = if i == 0 { screen.0 } else { screen.1 };
            assert!(longest > side * 0.9, "{tag}: full axis {i} is {longest} of {side}");
        }
    });
}

fn plan_canvas_all(ru: bool) {
    for mode in [Mode::Light, Mode::Dark] {
        for screen in [(1280.0, 720.0), (480.0, 600.0)] {
            plan_canvas(ru, mode, screen, false, false);
            plan_canvas(ru, mode, screen, true, false);
            plan_canvas(ru, mode, screen, true, true);
        }
    }
}

#[test]
fn plan_canvas_overlays_en() {
    plan_canvas_all(false);
}

#[test]
fn plan_canvas_overlays_ru() {
    plan_canvas_all(true);
}

#[derive(Clone, Copy, Debug)]
enum SecCase {
    Section,
    SectionFar,
    Elevation,
    ElevationFar,
    Lost,
}

/// The section canvas for several frames: the shared HUD stays on the canvas beside the scale
/// bar, and an empty or lost view shows its designed card with a reachable action.
fn section_canvas(ru: bool, screen: (f32, f32), case: SecCase) {
    use crate::section_hud as sh;
    use crate::wall_tool::{Event, Mode as Tool};
    use ddc_geom::v2;
    crate::with_test_lang(ru, || {
        let (ctx, _) = driven_ctx();
        let mut th = Theme::new(Mode::Light, Density::Compact);
        let tok = crate::ui_kit::from_theme(&th);
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(Tool::Rect);
        for e in [
            Event::Move(v2(0.0, 0.0)),
            Event::Press(v2(0.0, 0.0)),
            Event::Move(v2(6.0, 4.0)),
            Event::Release(v2(6.0, 4.0)),
        ] {
            ed.handle(e);
        }
        ed.set_mode(Tool::Select);
        let (a, b, c) = match case {
            SecCase::Section => (v2(3.0, -1.0), v2(3.0, 5.0), v2(1.0, 2.0)),
            SecCase::Elevation => (v2(-1.0, -1.0), v2(7.0, -1.0), v2(3.0, 2.0)),
            _ => (v2(100.0, 100.0), v2(100.0, 105.0), v2(99.0, 102.0)),
        };
        let mut spec = crate::editor::sec::spec_from("Cut 1", a, b, Some(c)).unwrap();
        if matches!(case, SecCase::Elevation | SecCase::ElevationFar) {
            spec.kind = ddc_model::ViewKind::Elevation;
        }
        let id = if matches!(case, SecCase::Lost) { 9999 } else { ed.add_view(&spec).unwrap() };
        let want = sh::section_empty_kind(ed.draw_view(id).as_ref(), spec.kind);
        match case {
            SecCase::Section => assert_eq!(want, None),
            SecCase::SectionFar => assert_eq!(want, Some(sh::SectionEmpty::CutsNothing)),
            SecCase::ElevationFar => assert_eq!(want, Some(sh::SectionEmpty::ShowsNothing)),
            SecCase::Lost => assert_eq!(want, Some(sh::SectionEmpty::NotFound)),
            SecCase::Elevation => {}
        }
        let vis = crate::isolate::Vis::default();
        let mut view = crate::section_view::SectionView::default();
        let mut p = Probe::default();
        for _ in 0..FRAMES {
            let mut frame = |ctx: &egui::Context| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| view.show(ui, &mut ed, id, &tok, &vis, &mut None));
            };
            p = pass_on(&ctx, &mut th, egui::Id::new("ux67f.sv"), screen, vec![], &mut frame);
        }
        let tag = format!("section ru={ru} {screen:?} {case:?}");
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(screen.0, screen.1));
        let span = |s: &str| p.spans.iter().find(|(t, _)| t == s).map(|(_, r)| *r);
        let fit = crate::tr_lang(ru, "plan.hud.fit", &[]);
        let mut hud_boxes = vec![];
        if matches!(case, SecCase::Lost) {
            assert!(span(&fit).is_none(), "{tag}: a lost view has nothing to fit");
        } else {
            let scale = if view.scale > 0.0 { view.scale } else { 50.0 };
            let units = ed.tool.units;
            let bar = crate::units::display_length(
                crate::plan_view::scale_bar_len(scale, units),
                units,
            );
            let fit_r = span(&fit).unwrap_or_else(|| panic!("{tag}: fit chip"));
            let ratio_r = span(&crate::plan_hud::scale_text(scale)).expect("scale chip");
            let bar_r = span(&bar).expect("scale bar label");
            for r in [fit_r, ratio_r, bar_r] {
                assert!(canvas.contains_rect(r), "{tag}: {r:?} leaves the canvas");
            }
            assert!(!fit_r.intersects(bar_r) && !ratio_r.intersects(bar_r), "{tag}: covers bar");
            assert!((fit_r.center().y - ratio_r.center().y).abs() < 0.5, "{tag}: baseline");
            let name = crate::editor::sec::display_name(&spec);
            if let Some(n) = span(&name) {
                assert!(canvas.contains_rect(n) && !n.intersects(bar_r), "{tag}: name {n:?}");
            } else {
                assert!(screen.0 < 1000.0, "{tag}: view name {name:?} missing on a wide canvas");
            }
            let ctl = p.controls.iter().find(|c| c.label == fit);
            let ctl = ctl.unwrap_or_else(|| panic!("{tag}: fit chip is not a control"));
            assert!(canvas.contains_rect(ctl.rect), "{tag}: fit control {:?}", ctl.rect);
            hud_boxes = vec![fit_r, ratio_r, bar_r];
        }
        for k in sh::SectionEmpty::ALL {
            let title = crate::tr_lang(ru, &format!("empty.{}.title", k.what()), &[]);
            assert_eq!(span(&title).is_some(), want == Some(k), "{tag}: card {k:?}");
        }
        if let Some(k) = want {
            let title = crate::tr_lang(ru, &format!("empty.{}.title", k.what()), &[]);
            let action = crate::tr_lang(ru, &format!("empty.{}.action", k.what()), &[]);
            let t_r = span(&title).unwrap_or_else(|| panic!("{tag}: card title"));
            let btn = p.controls.iter().find(|c| c.label == action);
            let btn = btn.unwrap_or_else(|| panic!("{tag}: action {action:?} is not a control"));
            assert!(canvas.contains_rect(t_r), "{tag}: title {t_r:?}");
            assert!(canvas.contains_rect(btn.rect), "{tag}: action {:?}", btn.rect);
            for h in &hud_boxes {
                assert!(!h.intersects(btn.rect) && !h.intersects(t_r), "{tag}: card on HUD");
            }
        }
        assert!(p.elided.is_empty(), "{tag}: elided {:?}", p.elided);
    });
}

fn section_canvas_all(ru: bool) {
    let cases = [
        SecCase::Section,
        SecCase::SectionFar,
        SecCase::Elevation,
        SecCase::ElevationFar,
        SecCase::Lost,
    ];
    for screen in [(1280.0, 720.0), (1024.0, 700.0), (480.0, 600.0)] {
        for case in cases {
            section_canvas(ru, screen, case);
        }
    }
}

#[test]
fn section_canvas_hud_and_cards_en() {
    section_canvas_all(false);
}

#[test]
fn section_canvas_hud_and_cards_ru() {
    section_canvas_all(true);
}

/// Key-figure strip of the stair: every caption and value whole, inside the panel.
fn figure_strip(ru: bool) {
    let keys = ["stair.risers", "stair.riser", "stair.comfort"];
    for screen in MAIN_SCREENS {
        for w in PROPS_WIDTHS {
            let items: Vec<(String, String)> = keys
                .iter()
                .map(|k| (crate::tr_lang(ru, k, &[]), "12 345 mm".to_string()))
                .collect();
            let p = props_panel(screen, w, |ui| crate::ui_kit::figures(ui, &items));
            let tag = format!("figures ru={ru} {screen:?} panel {w}");
            assert!(p.elided.is_empty(), "{tag}: elided {:?}", p.elided);
            let left = screen.0 - w;
            for (k, _) in &items {
                let hit = p.spans.iter().find(|(t, _)| t == k);
                let Some((_, r)) = hit else { panic!("{tag}: {k:?} missing") };
                let inside = r.left() >= left - EPS && r.right() <= screen.0 + EPS;
                assert!(inside, "{tag}: {k:?} {r:?}");
            }
        }
    }
}

#[test]
fn figure_strip_keeps_captions_whole_en() {
    figure_strip(false);
}

#[test]
fn figure_strip_keeps_captions_whole_ru() {
    figure_strip(true);
}

/// Element header with a very long name: the id stays visible, nothing leaves the panel.
#[test]
fn element_header_cuts_a_long_name_and_keeps_the_id() {
    let long = "W".repeat(90);
    for screen in MAIN_SCREENS {
        for w in PROPS_WIDTHS {
            let p = props_panel(screen, w, |ui| {
                crate::ui_kit::element_header(ui, "stair", &long, &long, 7);
            });
            let tag = format!("header {screen:?} panel {w}");
            assert!(p.text.iter().any(|s| s == "#7"), "{tag}: id missing");
            let left = screen.0 - w;
            for (t, r) in &p.spans {
                assert!(r.right() <= screen.0 + EPS, "{tag}: {t:.12} {r:?} past the right edge");
                assert!(r.left() >= left - EPS, "{tag}: {t:.12} {r:?} past the left edge");
            }
        }
    }
}

/// Height of the title bar panel.
const TITLE_H: f32 = 40.0;
const TITLE_WIDTHS: [f32; 4] = [900.0, 1024.0, 1280.0, 1920.0];

struct TitleCase {
    app: EditorApp,
    dir: Option<std::path::PathBuf>,
    /// What the control shows: the file name, or the untitled name.
    name: String,
}

impl Drop for TitleCase {
    fn drop(&mut self) {
        if let Some(d) = &self.dir {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

/// An app in `lang` on `layout`; `long` opens a project saved under a 120 character name,
/// `dirty` adds an unsaved edit.
fn title_case(lang: Lang, layout: &str, small: bool, long: bool, dirty: bool) -> TitleCase {
    let mut app = app_neutral(lang);
    app.set_layout(layout);
    app.prefs.ribbon_small = small;
    let mut dir = None;
    let mut name = app.sh.cat.t("project.untitled");
    if long {
        // tests run in parallel and each holds a lock on its project: no two share a folder
        static CASES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tag = format!("{}-{n}-{lang:?}-{layout}-{small}-{dirty}", std::process::id());
        let d = std::env::temp_dir().join(format!("ddc-ux65g-{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        name = format!("{}.ddc", "N".repeat(116));
        app.ed.save_project_as(&d.join(&name)).expect("save the long-named project");
        dir = Some(d);
    }
    if dirty {
        let seg = (ddc_geom::v2(0.0, 0.0), ddc_geom::v2(6.0, 0.0), 0.0);
        app.ed.add_curtains(&[seg]);
        assert!(app.ed.is_dirty());
    }
    TitleCase { app, dir, name }
}

fn title_pass(c: &mut TitleCase, ctx: &egui::Context, w: f32, ev: Vec<egui::Event>) -> Probe {
    let mut th = Theme::new(Mode::Dark, Density::Compact);
    let app = &mut c.app;
    pass_on(ctx, &mut th, egui::Id::new("ux65g.title"), (w, 1080.0), ev, &mut |x| {
        app.ui(x, None)
    })
}

/// The last of a few settled frames at width `w`.
fn title_frame(c: &mut TitleCase, ctx: &egui::Context, w: f32) -> Probe {
    for _ in 0..FRAMES - 1 {
        title_pass(c, ctx, w, vec![]);
    }
    title_pass(c, ctx, w, vec![])
}

fn title_controls(p: &Probe) -> Vec<&Control> {
    p.controls.iter().filter(|c| c.rect.bottom() <= TITLE_H + EPS).collect()
}

fn file_control<'a>(p: &'a Probe, name: &str) -> Option<&'a Control> {
    title_controls(p).into_iter().find(|c| c.label == name)
}

fn alt_f() -> Vec<egui::Event> {
    let key = egui::Key::F;
    let ev = |pressed| egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::ALT,
    };
    vec![ev(true), ev(false)]
}

fn title_bar_contract(lang: Lang) {
    // the old outlined menu buttons, in both languages
    let gone = [
        "File",
        "Data",
        "\u{424}\u{430}\u{439}\u{43b}",
        "\u{414}\u{430}\u{43d}\u{43d}\u{44b}\u{435}",
    ];
    for layout in ["classic", "context"] {
        for small in [false, true] {
            for (long, dirty) in [(false, false), (false, true), (true, false), (true, true)] {
                let mut c = title_case(lang, layout, small, long, dirty);
                let ctx = font_ctx();
                ctx.enable_accesskit();
                for w in TITLE_WIDTHS {
                    let tag = format!("{lang:?} {layout} small={small} long={long} d={dirty} {w}");
                    let p = title_frame(&mut c, &ctx, w);
                    let bar = title_controls(&p);
                    let file = file_control(&p, &c.name)
                        .unwrap_or_else(|| panic!("{tag}: no File control in {bar:?}"));
                    assert_eq!(file.role, accesskit::Role::Button, "{tag}");
                    assert!(file.rect.height() <= TITLE_H, "{tag}: {:?}", file.rect);
                    for b in &bar {
                        assert!(!gone.contains(&b.label.as_str()), "{tag}: outlined {:?}", b.label);
                        assert!(b.rect.left() >= -EPS && b.rect.right() <= w + EPS, "{tag}: {b:?}");
                    }
                    for (i, a) in bar.iter().enumerate() {
                        for b in &bar[i + 1..] {
                            let hit = a.rect.shrink(1.0).intersects(b.rect.shrink(1.0));
                            assert!(!hit, "{tag}: {:?} covers {:?}", a.label, b.label);
                        }
                    }
                    // the name is cut with an ellipsis only when it is long; other title text
                    // is never cut
                    let cut: Vec<&str> = p
                        .spans
                        .iter()
                        .filter(|(t, r)| r.bottom() <= TITLE_H && p.elided.contains(t))
                        .map(|(t, _)| t.as_str())
                        .collect();
                    assert!(cut.iter().all(|t| *t == c.name), "{tag}: cut {cut:?}");
                    assert_eq!(cut.is_empty(), !long, "{tag}: cut {cut:?}");
                    // the layout switch is a quiet icon: it keeps its tooltip name as the label
                    let ws = c.app.workspace();
                    let name = c.app.layout_name(&ws);
                    let tip = c.app.sh.cat.tf("workspace.layout.tip", &[("name", &name)]);
                    assert!(bar.iter().any(|b| b.label == tip), "{tag}: no layout button");
                    assert!(
                        !p.spans.iter().any(|(t, r)| r.bottom() <= TITLE_H && *t == name),
                        "{tag}: the layout name is printed in the bar"
                    );
                    // the untitled name speaks the language of the catalog
                    assert_eq!(has_cyrillic(&file.label), lang == Lang::Ru && !long, "{tag}");
                }
            }
        }
    }
}

#[test]
fn title_bar_en() {
    title_bar_contract(Lang::En);
}

#[test]
fn title_bar_ru() {
    title_bar_contract(Lang::Ru);
}

/// The unsaved state is part of the one control: it is wider than the saved one.
fn title_unsaved(lang: Lang) {
    for long in [false, true] {
        let ctx = font_ctx();
        ctx.enable_accesskit();
        let mut width = [0.0_f32; 2];
        for (i, dirty) in [false, true].into_iter().enumerate() {
            let mut c = title_case(lang, "classic", false, long, dirty);
            let p = title_frame(&mut c, &ctx, 1920.0);
            width[i] = file_control(&p, &c.name).expect("file control").rect.width();
        }
        assert!(width[1] > width[0] + 1.0, "{lang:?} long={long}: {width:?}");
    }
}

#[test]
fn title_unsaved_state_en() {
    title_unsaved(Lang::En);
}

#[test]
fn title_unsaved_state_ru() {
    title_unsaved(Lang::Ru);
}

/// Alt+F opens the File menu of the name control; Alt+F again closes it.
fn title_alt_f(lang: Lang) {
    for long in [false, true] {
        let mut c = title_case(lang, "classic", false, long, false);
        let ctx = font_ctx();
        ctx.enable_accesskit();
        let new = c.app.sh.cat.t("cmd.file.new");
        let before = title_frame(&mut c, &ctx, 1280.0);
        assert!(!before.text.contains(&new), "{lang:?}: the menu is open before Alt+F");
        title_pass(&mut c, &ctx, 1280.0, alt_f());
        let open = title_frame(&mut c, &ctx, 1280.0);
        // the XLSX round trip, Sync included, is a labelled row of the open menu
        let xlsx = ["data.export_xlsx", "data.import_xlsx", "data.sync_xlsx"];
        for id in ["file.new", "file.open", "file.save", "file.save_as"].into_iter().chain(xlsx) {
            let label = c.app.sh.cat.t(&format!("cmd.{id}"));
            assert!(open.text.contains(&label), "{lang:?} long={long}: {label:?} missing");
        }
        // Save as / Export (.rvt, IFC, DXF, PDF) is a labelled submenu row of the open menu
        let export = c.app.sh.cat.t("fmenu.export");
        assert!(open.text.contains(&export), "{lang:?} long={long}: {export:?} missing");
        title_pass(&mut c, &ctx, 1280.0, alt_f());
        let shut = title_frame(&mut c, &ctx, 1280.0);
        assert!(!shut.text.contains(&new), "{lang:?}: Alt+F did not close the menu");
    }
}

#[test]
fn title_alt_f_opens_file_menu_en() {
    title_alt_f(Lang::En);
}

#[test]
fn title_alt_f_opens_file_menu_ru() {
    title_alt_f(Lang::Ru);
}

/// The Data items left the title bar: each one is a command with a handler, so the ribbon tab
/// and the palette still reach it.
#[test]
fn data_commands_stay_reachable() {
    let reg = ddc_ui::Registry::builtin();
    for id in ["data.export_xlsx", "data.import_xlsx", "data.sync_xlsx", "mgr.phases"] {
        assert!(reg.get(id).is_some(), "{id} is not a command");
    }
    let src = include_str!("shell/xlsx_win.rs");
    assert!(src.contains("register_command(\"data.sync_xlsx\""));
}

// ---------------------------------------------------------------- option bar

const OPT_BAR: &str = "ux65f.optbar";
const OPT_BAR_H: f32 = 40.0;
const OPT_WIDE: (f32, f32) = (1280.0, 720.0);
const OPT_NARROW: (f32, f32) = (480.0, 720.0);

fn draw_option_bar(ctx: &egui::Context, a: &mut EditorApp, fire: &mut Vec<String>) {
    let t = crate::shell::tokens(&a.sh.theme);
    egui::TopBottomPanel::top(OPT_BAR).exact_height(OPT_BAR_H).show(ctx, |ui| {
        ui.horizontal_centered(|ui| a.tool_bar(ui, &t, fire));
    });
}

/// Four walls of a room, all selected, Select mode.
pub(crate) fn select_room_walls(a: &mut EditorApp) {
    use crate::wall_tool::{Event, Mode};
    use ddc_geom::v2;
    a.ed.set_mode(Mode::Rect);
    a.ed.handle(Event::Move(v2(0.0, 0.0)));
    a.ed.handle(Event::Press(v2(0.0, 0.0)));
    a.ed.handle(Event::Move(v2(6.0, 4.0)));
    a.ed.handle(Event::Release(v2(6.0, 4.0)));
    a.ed.set_mode(Mode::Select);
    a.ed.selection = a.ed.walls.iter().map(|w| crate::editor::Sel::Wall(w.id)).collect();
}

fn wall_tool_on(a: &mut EditorApp) {
    a.ed.set_mode(crate::wall_tool::Mode::Polyline);
}

/// The option bar alone on `screen`; `setup` puts the editor in the state under test.
fn option_bar(lang: Lang, screen: (f32, f32), setup: fn(&mut EditorApp)) -> (Probe, EditorApp) {
    let mut a = app_in(lang);
    setup(&mut a);
    let mut fire = vec![];
    let p = probe_on(egui::Id::new(OPT_BAR), screen, |ctx| draw_option_bar(ctx, &mut a, &mut fire));
    (p, a)
}

fn named<'a>(p: &'a Probe, label: &str) -> Option<&'a Control> {
    p.controls.iter().find(|c| c.label == label)
}

fn has_text(p: &Probe, s: &str) -> bool {
    p.text.iter().any(|t| t == s)
}

/// One left click on the control with `label`, driven by hand; returns the commands it fired.
fn click_control(lang: Lang, setup: fn(&mut EditorApp), label: &str) -> (EditorApp, Vec<String>) {
    let mut a = app_in(lang);
    setup(&mut a);
    let mut fire = vec![];
    let (ctx, mut th) = driven_ctx();
    let id = egui::Id::new(OPT_BAR);
    let mut frame = |c: &egui::Context| draw_option_bar(c, &mut a, &mut fire);
    let mut seen = Probe::default();
    for _ in 0..FRAMES {
        seen = pass_on(&ctx, &mut th, id, OPT_WIDE, vec![], &mut frame);
    }
    let at = named(&seen, label).unwrap_or_else(|| panic!("no control {label:?}")).rect.center();
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    pass_on(&ctx, &mut th, id, OPT_WIDE, vec![egui::Event::PointerMoved(at)], &mut frame);
    pass_on(&ctx, &mut th, id, OPT_WIDE, vec![button(true)], &mut frame);
    pass_on(&ctx, &mut th, id, OPT_WIDE, vec![button(false)], &mut frame);
    (a, fire)
}

/// Select mode, nothing selected: level and a hint; no tool icons and no units toggle.
fn option_bar_idle(lang: Lang) {
    let ru = lang == Lang::Ru;
    let (p, a) = option_bar(lang, OPT_WIDE, |_| {});
    let hint = a.sh.cat.t("sel_chip.opt.sel_hint");
    assert_ne!(hint, "sel_chip.opt.sel_hint", "hint text missing");
    assert!(has_text(&p, &hint), "{lang:?}: hint missing in {:?}", p.text);
    assert!(has_text(&p, &crate::tr_lang(ru, "opt.level", &[])), "{lang:?}: no level caption");
    assert!(!has_text(&p, &a.sh.cat.t("sel_chip.opt.units")), "{lang:?}: units sit in Select mode");
    assert!(p.controls.len() <= 2, "{lang:?}: tool icons in Select mode {:?}", p.controls);
    assert!(p.elided.is_empty(), "{lang:?}: elided {:?}", p.elided);
}

/// Select mode with a selection: chip with count and clear, type for walls, isolate; no tool
/// name, no level, no units toggle. The clear button empties the selection.
fn option_bar_selected(lang: Lang) {
    let ru = lang == Lang::Ru;
    let (p, a) = option_bar(lang, OPT_WIDE, select_room_walls);
    let chip = format!("{}: 4", crate::tr_lang(ru, "type_dialog.selcat.wall", &[]));
    assert!(has_text(&p, &chip), "{lang:?}: chip {chip:?} missing in {:?}", p.text);
    let clear = a.sh.cat.t("props.multi.clear");
    assert!(named(&p, &clear).is_some(), "{lang:?}: no clear button in {:?}", p.controls);
    assert!(has_text(&p, &crate::tr_lang(ru, "opt.type", &[])), "{lang:?}: no type caption");
    assert!(!has_text(&p, &a.sh.cat.t("sel_chip.opt.sel_hint")), "{lang:?}: hint with selection");
    assert!(!has_text(&p, &a.sh.cat.t("sel_chip.opt.units")), "{lang:?}: units sit in Select mode");
    assert!(p.elided.is_empty(), "{lang:?}: elided {:?}", p.elided);
    let (a, _) = click_control(lang, select_room_walls, &clear);
    assert!(a.ed.selection.is_empty(), "{lang:?}: clear kept the selection");
}

/// Wall tool: quiet tool name, then its options with a labelled units control; the tool icons of
/// the ribbon are not repeated, a way back to Select stays.
fn option_bar_wall_tool(lang: Lang) {
    let ru = lang == Lang::Ru;
    let (p, a) = option_bar(lang, OPT_WIDE, wall_tool_on);
    let cmd = a.sh.reg.get("arch.wall").expect("wall command");
    let name = a.sh.cat.t(&cmd.label_key());
    let short = name.split(" (").next().unwrap_or(&name).to_string();
    assert!(has_text(&p, &short), "{lang:?}: tool name {short:?} missing in {:?}", p.text);
    assert!(has_text(&p, &crate::tr_lang(ru, "opt.level", &[])), "{lang:?}: no level caption");
    assert!(has_text(&p, &a.sh.cat.t("sel_chip.opt.units")), "{lang:?}: units control unlabelled");
    let fields = p.controls.iter().filter(|c| c.role == accesskit::Role::TextInput).count();
    assert!(fields >= 1, "{lang:?}: no wall option field in {:?}", p.controls);
    assert!(!has_text(&p, &a.sh.cat.t("sel_chip.opt.sel_hint")), "{lang:?}: select hint in a tool");
    assert!(p.elided.is_empty(), "{lang:?}: elided {:?}", p.elided);
    // the unit toggle still works from the tool bar
    let ft = crate::tr_lang(ru, "type_dialog.units.ftin", &[]);
    let (a, _) = click_control(lang, wall_tool_on, &ft);
    assert_eq!(a.ed.tool.units, crate::units::Units::Imperial, "{lang:?}: ft-in not applied");
}

/// A 480 point window: the bar keeps its height, the first controls stay on screen and
/// nothing is painted above or below the bar.
fn option_bar_narrow(lang: Lang) {
    let cases: [(&str, fn(&mut EditorApp)); 3] =
        [("idle", |_| {}), ("selected", select_room_walls), ("wall tool", wall_tool_on)];
    for (tag, setup) in cases {
        let (p, a) = option_bar(lang, OPT_NARROW, setup);
        let tag = format!("option bar narrow {lang:?} {tag}");
        for (t, r) in &p.spans {
            let inside = r.top() >= -EPS && r.bottom() <= OPT_BAR_H + EPS;
            assert!(inside, "{tag}: {t:.16} {r:?} leaves the bar height");
        }
        let first = p.controls.iter().map(|c| c.rect.left()).fold(f32::MAX, f32::min);
        assert!(first < OPT_NARROW.0, "{tag}: no control starts on screen");
        if tag.ends_with("selected") {
            let clear = a.sh.cat.t("props.multi.clear");
            let c = named(&p, &clear).unwrap_or_else(|| panic!("{tag}: no clear button"));
            assert!(c.rect.right() <= OPT_NARROW.0 + EPS, "{tag}: clear {:?}", c.rect);
        }
    }
}

en_ru!(option_bar_idle, option_bar_idle_en, option_bar_idle_ru);
en_ru!(option_bar_selected, option_bar_selected_en, option_bar_selected_ru);
en_ru!(option_bar_wall_tool, option_bar_wall_tool_en, option_bar_wall_tool_ru);
en_ru!(option_bar_narrow, option_bar_narrow_en, option_bar_narrow_ru);

/// Tab walks through the bar of a narrow window: the focused control is always inside the
/// bar's visible width, the last one included.
fn option_bar_tab_scrolls(lang: Lang) {
    let mut a = app_in(lang);
    wall_tool_on(&mut a);
    let mut fire = vec![];
    let (ctx, mut th) = driven_ctx();
    let id = egui::Id::new(OPT_BAR);
    let mut frame = |c: &egui::Context| draw_option_bar(c, &mut a, &mut fire);
    let mut seen = Probe::default();
    for _ in 0..FRAMES {
        seen = pass_on(&ctx, &mut th, id, OPT_NARROW, vec![], &mut frame);
    }
    let off = |c: &Control| c.rect.right() > OPT_NARROW.0 + EPS;
    assert!(seen.controls.iter().any(off), "{lang:?}: nothing overflows, test proves nothing");
    let tab = |pressed| egui::Event::Key {
        key: egui::Key::Tab,
        physical_key: Some(egui::Key::Tab),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let mut reached = vec![];
    for _ in 0..seen.controls.len() + 1 {
        pass_on(&ctx, &mut th, id, OPT_NARROW, vec![tab(true), tab(false)], &mut frame);
        for _ in 0..60 {
            seen = pass_on(&ctx, &mut th, id, OPT_NARROW, vec![], &mut frame);
        }
        let Some(f) = ctx.memory(|m| m.focused()) else { continue };
        let Some(c) = seen.controls.iter().find(|c| c.id == f.value()) else { continue };
        assert!(c.rect.left() >= -EPS, "{lang:?}: {:?} left of the bar {:?}", c.label, c.rect);
        assert!(
            c.rect.right() <= OPT_NARROW.0 + EPS,
            "{lang:?}: focused {:?} past the right edge {:?}",
            c.label,
            c.rect
        );
        reached.push(c.id);
    }
    assert!(reached.len() >= 2, "{lang:?}: Tab reached {} controls", reached.len());
}

en_ru!(option_bar_tab_scrolls, option_bar_tab_scrolls_en, option_bar_tab_scrolls_ru);

/// The tool icons live on the ribbon and the toolbar only; the option bar never repeats them,
/// and the units setting keeps its labelled page in Settings.
#[test]
fn option_bar_repeats_no_tools_and_units_stay_reachable() {
    let src = include_str!("shell/panels.rs");
    let start = src.find("fn tool_bar(").expect("tool_bar");
    let end = start + src[start..].find("fn props(").expect("props");
    let body = &src[start..end];
    for id in ["arch.wall", "arch.wall_room", "arch.curtain_wall", "arch.door", "arch.window"] {
        assert!(!body.contains(&format!("\"{id}\"")), "{id} repeated in the option bar");
    }
    // a narrow window reaches the cut-off controls through a visible bar, not only a wheel
    assert!(!body.contains("AlwaysHidden"), "the option bar hides its scroll bar");
    assert!(body.contains("!(select_on && active.is_none())"), "Select mode reads as a tool");
    assert!(include_str!("shell/settings_win.rs").contains("\"settings.page.units\""));
    let mut a = app_in(Lang::En);
    a.ed.set_units(crate::units::Units::Imperial);
    assert_eq!(a.ed.tool.units, crate::units::Units::Imperial);
}

// The XLSX states and the windows opened daily (progress, scope, overwrite, loss, cell notes,
// foreign book, sync offer, settings, selection filter, level, open card, save report, recovery).
// Each is drawn at 1280x720 and at the smallest window the app allows, in both languages: the
// window and its primary action lie inside the screen, no control is cut or unnamed, and no
// catalog key shows raw.

const QA_SCREENS: [(&str, (f32, f32)); 2] = [("1280", (1280.0, 720.0)), ("narrow", (800.0, 520.0))];
const QA_BOOK: &str = "C:/Projects/Very Long Customer Name/Building Complex/Structure/Revisions/\
                       2026/Tower_architecture_model_export_final_v12.xlsx";
const QA_LONG: &str = "Fire resistance rating of the assembly including all finishes and tail";

/// A catalog key shown as text: lower-case dotted words, no spaces, not a file name.
fn looks_like_key(s: &str) -> bool {
    let s = s.trim();
    let word = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.';
    let ext = ["xlsx", "xls", "ddc", "ifc", "rvt", "dwg", "dxf", "csv", "json"];
    s.contains('.')
        && !s.starts_with('.')
        && !s.ends_with('.')
        && s.chars().all(word)
        && s.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && !s.rsplit('.').next().is_some_and(|e| ext.contains(&e))
}

#[test]
fn qa_key_check_reads_keys_and_not_names() {
    for key in ["xlw.exporting", "settings.page.units", "save.report.title", "xlsx_scope.title"] {
        assert!(looks_like_key(key), "{key}");
    }
    for text in ["Walls", "book-2.xlsx", "3.5 m", "Tower.ddc", "C:/a/b.c", "Save as...", "a.b c"] {
        assert!(!looks_like_key(text), "{text}");
    }
}

/// What every probed window keeps; `primary` is its default action, `title` its title text.
fn assert_card(tag: &str, p: &Probe, lang: Lang, primary: Option<&str>, title: Option<&str>) {
    assert_contract(tag, p, false);
    assert_language(tag, p, lang);
    let (win, screen) = (p.inner.expect("window"), p.screen.expect("screen"));
    assert!(screen.expand(EPS).contains_rect(win), "{tag}: window {win:?} leaves {screen:?}");
    if let Some(label) = primary {
        let c = named(p, label)
            .unwrap_or_else(|| panic!("{tag}: no primary {label:?} in {:?}", p.controls));
        let r = c.rect;
        assert!(win.expand(EPS).contains_rect(r), "{tag}: {label:?} {r:?} not in {win:?}");
    }
    if let Some(t) = title {
        assert!(p.text.iter().any(|x| x.contains(t)), "{tag}: no title {t:?} in {:?}", p.text);
    }
    let unnamed: Vec<_> = p.controls.iter().filter(|c| c.label.trim().is_empty()).collect();
    assert!(unnamed.is_empty(), "{tag}: unnamed controls {unnamed:?}");
    let raw: Vec<&String> = p.text.iter().filter(|t| looks_like_key(t)).collect();
    assert!(raw.is_empty(), "{tag}: raw keys {raw:?}");
    let labels = p.controls.iter().map(|c| &c.label);
    let raw: Vec<&String> = labels.filter(|t| looks_like_key(t)).collect();
    assert!(raw.is_empty(), "{tag}: raw key labels {raw:?}");
}

fn dialog_id(key: &str) -> egui::Id {
    egui::Id::new(("ddc.dialog", key))
}

fn qa_xlsx_progress(lang: Lang) {
    use crate::shell::{xlsx_job::XlsxJob, xlsx_load::XlsxLoad, xlsx_win::layout_cards};
    let ru = lang == Lang::Ru;
    let t = |k: &'static str| crate::tr_str(ru, k);
    let path = std::path::Path::new(QA_BOOK);
    for (name, screen) in QA_SCREENS {
        let mut app = app_in(lang);
        app.xlsx_job = Some(XlsxJob::parked(path, 1));
        let p = probe_on(dialog_id("xlsx_exporting"), screen, |ctx| layout_cards(&mut app, ctx));
        let tag = format!("xlw.exporting {name} {lang:?}");
        assert_card(&tag, &p, lang, Some(t("xlw.cancel")), Some(t("xlw.exporting")));
        let mut app = app_in(lang);
        app.xlsx_load = Some(XlsxLoad::parked(path));
        let p = probe_on(dialog_id("xlsx_loading"), screen, |ctx| layout_cards(&mut app, ctx));
        let tag = format!("xlw.loading {name} {lang:?}");
        assert_card(&tag, &p, lang, Some(t("xlw.cancel")), Some(t("xlw.loading")));
        let mut app = app_in(lang);
        app.xlsx_offer = true;
        let p = probe_on(dialog_id("xlsx_sync_offer"), screen, |ctx| layout_cards(&mut app, ctx));
        let tag = format!("xlw.sync_none_title {name} {lang:?}");
        let (primary, title) = (t("xlw.offer_export"), t("xlw.sync_none_title"));
        assert_card(&tag, &p, lang, Some(primary), Some(title));
    }
}

fn qa_xlsx_scope(lang: Lang) {
    use crate::shell::{xlsx_scope::ScopeCard, xlsx_win::layout_cards};
    let ru = lang == Lang::Ru;
    let t = |k: &'static str| crate::tr_str(ru, k);
    let refused = Some(QA_LONG.repeat(2));
    for (name, screen) in QA_SCREENS {
        for (case, error) in [("plain", None), ("refused", refused.clone())] {
            let mut app = app_in(lang);
            app.xlsx_card = Some(ScopeCard { error, ..ScopeCard::default() });
            let p = probe_on(dialog_id("xlsx_scope"), screen, |ctx| layout_cards(&mut app, ctx));
            let tag = format!("xl16.title {case} {name} {lang:?}");
            assert_card(&tag, &p, lang, Some(t("xlsx_scope.export")), Some(t("xlsx_scope.title")));
            assert_eq!(checkboxes(&p), 5, "{tag}: one box per category");
        }
    }
}

fn qa_xlsx_overwrite(lang: Lang) {
    use crate::shell::{xlsx_scope::OverCard, xlsx_win::layout_cards};
    let ru = lang == Lang::Ru;
    let t = |k: &'static str| crate::tr_str(ru, k);
    let book = std::path::PathBuf::from(QA_BOOK);
    let name = std::path::PathBuf::from(QA_BOOK.replace(".xlsx", "-2.xlsx"));
    let file = name.file_name().unwrap().to_string_lossy().into_owned();
    for (name_s, screen) in QA_SCREENS {
        for (case, ours, pending, outside) in [("ours", true, 7, 12), ("foreign", false, 0, 0)] {
            let mut app = app_in(lang);
            app.xlsx_over = Some(OverCard {
                path: book.clone(),
                scope: None,
                pending,
                outside,
                ours,
                name: name.clone(),
            });
            let id = dialog_id("xlsx_overwrite");
            let p = probe_on(id, screen, |ctx| layout_cards(&mut app, ctx));
            let tag = format!("xl18.title {case} {name_s} {lang:?}");
            let primary = crate::tr_lang(ru, "xlsx_exists.new_name", &[("name", &file)]);
            assert_card(&tag, &p, lang, Some(&primary), Some(t("xlsx_exists.title")));
            let replace = t("xlsx_exists.replace");
            assert_eq!(named(&p, replace).is_some(), ours, "{tag}: Replace only for our book");
        }
    }
}

fn qa_xlsx_loss(lang: Lang) {
    use crate::shell::{xlsx_loss::LossCard, xlsx_win::layout_cards};
    let ru = lang == Lang::Ru;
    let t = |k: &'static str| crate::tr_str(ru, k);
    for (name, screen) in QA_SCREENS {
        for n in [2, 20] {
            let items = (0..n).map(|i| format!("Chart sheet {i}: {QA_LONG}")).collect();
            let mut app = app_in(lang);
            app.xlsx_loss = Some(LossCard {
                path: std::path::PathBuf::from(QA_BOOK),
                again: true,
                scope: None,
                items,
            });
            let p = probe_on(dialog_id("xlloss"), screen, |ctx| layout_cards(&mut app, ctx));
            let tag = format!("xlloss {n} items {name} {lang:?}");
            assert_card(&tag, &p, lang, Some(t("xlloss.keep")), Some(t("xlloss.title")));
            // dropping parts of the old book is the danger action, apart from the safe one
            let (keep, go) = (t("xlloss.keep"), t("xlloss.export_anyway"));
            let (keep, go) = (button(&p, &tag, keep).rect, button(&p, &tag, go).rect);
            assert!(go.max.x <= keep.min.x + EPS, "{tag}: Export anyway not left of Keep");
        }
    }
}

fn qa_xlsx_cell_notes(lang: Lang) {
    use crate::shell::xlsx_win::layout_cards;
    use ddc_io::xlsx::{ExportNote, ExportNoteKind};
    let ru = lang == Lang::Ru;
    let title = crate::tr_lang(ru, "xlcell.title", &[]);
    let close = crate::tr_lang(ru, "xlcell.close", &[]);
    for (name, screen) in QA_SCREENS {
        let mut app = app_in(lang);
        app.xlsx_cell_notes = (0..40u32)
            .map(|i| ExportNote {
                sheet: format!("{QA_LONG} {i}"),
                row: 2 + i,
                col: 9,
                id: Some(i64::from(i) + 90),
                kind: if i % 2 == 0 { ExportNoteKind::Clipped } else { ExportNoteKind::NonFinite },
            })
            .collect();
        let p = probe_on(dialog_id("xlsx_cell_notes"), screen, |ctx| layout_cards(&mut app, ctx));
        let tag = format!("xlcell.title {name} {lang:?}");
        assert_card(&tag, &p, lang, Some(&close), Some(&title));
        // a long sheet name is cut in its column, it does not widen the window
        let cut = p.elided.iter().any(|t| t.starts_with(&QA_LONG[..12]));
        assert!(cut, "{tag}: long sheet names not cut: {:?}", p.elided);
        let w = p.inner.expect("window").width();
        assert!(w <= screen.0 - 48.0 + EPS, "{tag}: window {w} wider than the screen allows");
    }
}

fn qa_xlsx_foreign(lang: Lang) {
    use crate::shell::xlsx_win::layout_frame;
    let ru = lang == Lang::Ru;
    let t = |k: &'static str| crate::tr_str(ru, k);
    for (name, screen) in QA_SCREENS {
        let mut app = app_in(lang);
        let pv = crate::shell::xlsx_project::refused(std::path::Path::new(QA_BOOK), false);
        app.xlsx = Some(pv);
        let id = egui::Id::new("xlsx_foreign_book");
        let p = probe_on(id, screen, |ctx| layout_frame(&mut app, ctx));
        let tag = format!("xs21.title {name} {lang:?}");
        assert_card(&tag, &p, lang, Some(t("xlsx_project.export_new")), Some(t("xlsx_project.title")));
        assert!(named(&p, t("xlsx_project.close")).is_some(), "{tag}: no way to close the card");
    }
}

fn qa_settings(lang: Lang) {
    use crate::shell::settings_win::{layout_frame, PAGE_COUNT};
    for (name, screen) in QA_SCREENS {
        let cases: Vec<(String, u8, &str)> = (0..PAGE_COUNT)
            .map(|i| (format!("page {i}"), i as u8, ""))
            .chain([("no results".to_string(), 0, "zzqxzzqx")])
            .collect();
        for (case, page, query) in cases {
            let mut app = app_neutral(lang);
            let close = app.sh.cat.t("settings.close");
            let p = probe_on(modal_id("settings"), screen, |ctx| {
                layout_frame(&mut app, ctx, page, query)
            });
            let tag = format!("settings {case} {name} {lang:?}");
            let title = app.sh.cat.t("settings.title");
            assert_card(&tag, &p, lang, Some(&close), Some(&title));
        }
    }
}

/// At 1024 px the AI page shows the connection state and two live buttons inside the
/// window, no "later release" text; "Connection settings..." runs the assistant's own settings.
fn qa_settings_ai(lang: Lang) {
    use crate::shell::settings_win::{layout_frame, AI_PAGE};
    let screen = (1024.0, 640.0);
    let mut app = app_neutral(lang);
    let t = |k: &str| app.sh.cat.t(k);
    let (connect, panel, none) =
        (t("settings.ai_connect_btn"), t("settings.ai_open_panel"), t("settings.ai_not_connected"));
    let id = modal_id("settings");
    let (ctx, mut th) = driven_ctx();
    let mut frame = |c: &egui::Context| layout_frame(&mut app, c, AI_PAGE, "");
    let mut p = Probe::default();
    for _ in 0..FRAMES {
        p = pass_on(&ctx, &mut th, id, screen, vec![], &mut frame);
    }
    let tag = format!("settings ai 1024 {lang:?}");
    let win = p.inner.unwrap_or_else(|| panic!("{tag}: window not drawn"));
    for label in [&connect, &panel] {
        let c = named(&p, label).unwrap_or_else(|| panic!("{tag}: no {label:?}"));
        assert!(win.contains_rect(c.rect.shrink(EPS)), "{tag}: {label:?} outside {win:?}");
    }
    drop(frame);
    // the data folder may hold a ready profile: then its name shows instead
    let want = app.chrome.settings.ai_name.clone().unwrap_or(none);
    assert!(has_text(&p, &want), "{tag}: no connection state {want:?} in {:?}", p.text);
    let mut frame = |c: &egui::Context| layout_frame(&mut app, c, AI_PAGE, "");
    for s in &p.text {
        let low = s.to_lowercase();
        assert!(!low.contains("later release") && !low.contains("soon"), "{tag}: {s}");
    }
    assert!(!p.elided.iter().any(|s| *s == connect || *s == panel), "{tag}: cut {:?}", p.elided);
    let at = named(&p, &connect).map(|c| c.rect.center()).unwrap_or_default();
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    pass_on(&ctx, &mut th, id, screen, vec![egui::Event::PointerMoved(at)], &mut frame);
    pass_on(&ctx, &mut th, id, screen, vec![button(true)], &mut frame);
    pass_on(&ctx, &mut th, id, screen, vec![button(false)], &mut frame);
    drop(frame);
    let ran = app.sh.palette.recent.iter().any(|r| r == "ai.settings");
    assert!(ran, "{tag}: the button did not open AI settings");
}

en_ru!(qa_settings_ai, qa_settings_ai_en, qa_settings_ai_ru);

/// logo1: the branding page with a logo loaded fits the window at every class size: zone,
/// Choose file and Remove inside, the size line and the hint drawn, nothing cut.
fn qa_settings_logo(lang: Lang) {
    use crate::shell::settings_win::{layout_frame, BRANDING_PAGE};
    for (name, screen) in QA_SCREENS {
        let mut app = app_neutral(lang);
        let px = (0..600 * 200).flat_map(|_| [20u8, 90, 160, 255]).collect();
        let png = ddc_io::logo::Logo::from_rgba(600, 200, px).unwrap().to_png();
        app.ed.model.set_project_logo(Some(&png)).unwrap();
        let t = |k: &str| app.sh.cat.t(k);
        let (choose, remove, zone) = (t("branding.choose"), t("branding.remove"), t("branding.zone"));
        let title = t("settings.title");
        let hint = t("branding.hint");
        let p = probe_on(modal_id("settings"), screen, |ctx| {
            layout_frame(&mut app, ctx, BRANDING_PAGE, "")
        });
        let tag = format!("settings logo {name} {lang:?}");
        assert_card(&tag, &p, lang, Some(&choose), Some(&title));
        let win = p.inner.expect("window");
        for label in [&remove, &zone] {
            let c = named(&p, label).unwrap_or_else(|| panic!("{tag}: no {label:?}"));
            assert!(
                win.expand(EPS).contains_rect(c.rect),
                "{tag}: {label:?} {:?} not in {win:?}",
                c.rect
            );
        }
        assert!(app.chrome.settings.logo.has_picture(), "{tag}: no preview");
        assert!(p.text.iter().any(|s| s.contains("600 \u{d7} 200")), "{tag}: no size line");
        assert!(p.text.iter().any(|s| *s == hint), "{tag}: hint missing");
        assert!(p.elided.iter().all(|e| *e != choose && *e != remove && *e != zone), "{tag}: cut");
    }
}

en_ru!(qa_settings_logo, qa_settings_logo_en, qa_settings_logo_ru);

/// A short page ends the window under its last row, no empty lower half; the search
/// text sits inside the field with room after the glyph, off the border.
fn settings_short_page_fits(lang: Lang) {
    use crate::shell::settings_win::{layout_frame, AI_PAGE};
    let screen = (1280.0, 800.0);
    let mut app = app_neutral(lang);
    let hint = app.sh.cat.t("settings.search");
    let p = probe_on(modal_id("settings"), screen, |ctx| {
        layout_frame(&mut app, ctx, AI_PAGE, "")
    });
    let tag = format!("settings short page {lang:?}");
    let win = p.inner.unwrap_or_else(|| panic!("{tag}: window not drawn"));
    let low = p
        .spans
        .iter()
        .map(|(_, r)| *r)
        .chain(p.controls.iter().map(|c| c.rect))
        .filter(|r| win.contains_rect(r.shrink(EPS)))
        .map(|r| r.bottom())
        .fold(win.top(), f32::max);
    let row = app.sh.theme.d().row;
    let empty = win.bottom() - low;
    assert!(empty <= 2.0 * row, "{tag}: {empty} px empty under the page");
    let text = p.spans.iter().find(|(s, _)| *s == hint).map(|(_, r)| *r);
    let text = text.unwrap_or_else(|| panic!("{tag}: no search hint in {:?}", p.text));
    let field = p.controls.iter().map(|c| c.rect).find(|r| r.contains(text.center()));
    let field = field.unwrap_or_else(|| panic!("{tag}: hint {text:?} in no field"));
    let pad = ddc_ui::theme::space::LG;
    assert!(text.left() >= field.left() + pad - EPS, "{tag}: hint on the glyph");
    assert!(field.contains_rect(text.shrink(EPS)), "{tag}: hint {text:?} out of {field:?}");
}

en_ru!(settings_short_page_fits, settings_short_page_fits_en, settings_short_page_fits_ru);

/// Four walls and a door selected: two categories in the filter.
fn qa_filter_app(lang: Lang) -> EditorApp {
    let mut a = app_neutral(lang);
    select_room_walls(&mut a);
    let wall = a.ed.walls[0].id;
    let door = ddc_model::OpeningSpec {
        along: 1.0,
        width: 0.9,
        sill: 0.0,
        height: 2.1,
        kind: "door".into(),
    };
    let id = a.ed.model.add_opening(wall, &door).unwrap();
    a.ed.reload().unwrap();
    let mut sel: Vec<crate::editor::Sel> =
        a.ed.walls.iter().map(|w| crate::editor::Sel::Wall(w.id)).collect();
    sel.push(crate::editor::Sel::Opening(id));
    a.ed.selection = sel;
    a.plan.b.labels = crate::modify_tools::Labels::from(&a.sh.cat);
    a
}

fn qa_selection_filter(lang: Lang) {
    let ru = lang == Lang::Ru;
    for (name, screen) in QA_SCREENS {
        let mut a = qa_filter_app(lang);
        let id = dialog_id("selection_filter");
        let p = probe_on(id, screen, |ctx| a.plan.b.layout_filter(ctx, &mut a.ed, ru));
        let tag = format!("selection_filter {name} {lang:?}");
        let (ok, title) = (a.plan.b.labels.t("flt.ok"), a.plan.b.labels.t("flt.title"));
        assert_card(&tag, &p, lang, Some(&ok), Some(&title));
    }
}

fn qa_level(lang: Lang) {
    let ru = lang == Lang::Ru;
    let long = "Ground floor of the main building with a very long descriptive name";
    for (name, screen) in QA_SCREENS {
        for case in ["new", "edit", "bad"] {
            let mut a = app_neutral(lang);
            a.ed.model.sql().execute("UPDATE level SET name = ?1", [long]).unwrap();
            let edit = if case == "new" { None } else { Some(a.ed.level) };
            a.start_level_edit(edit);
            if case == "bad" {
                a.level_edit.as_mut().unwrap().z = "not a number".into();
            }
            let p = probe_on(dialog_id("level"), screen, |ctx| a.layout_level(ctx, ru));
            let tag = format!("level {case} {name} {lang:?}");
            let key = if case == "new" { "dlg.create" } else { "dlg.save" };
            let primary = crate::tr_lang(ru, key, &[]);
            let title_key = if case == "new" { "dlg.level.new" } else { "dlg.level.title" };
            let title = crate::tr_lang(ru, title_key, &[]);
            assert_card(&tag, &p, lang, Some(&primary), Some(&title));
        }
    }
}

fn qa_open_card(lang: Lang) {
    use crate::open::{card, decide::{Ask, Door}};
    let cat = Catalog::new(lang);
    for (name, screen) in QA_SCREENS {
        for (case, kind, primary) in [
            ("model", Ask::ModelDrop, cat.t("open_file.card.new")),
            ("large", Ask::Large, cat.t("open_file.card.open_all")),
        ] {
            let mut c = card::Card {
                inputs: vec![],
                door: Door::Drop,
                kind,
                include_sub: false,
                has_folder: true,
                count: 31,
                bytes: 3_400_000_000,
                name: format!("{QA_LONG}.ifc"),
            };
            let p = probe_on(dialog_id("open.card"), screen, |ctx| {
                let _ = card::show(ctx, &mut c, &cat);
            });
            let tag = format!("open.card {case} {name} {lang:?}");
            assert_card(&tag, &p, lang, Some(&primary), None);
            assert!(named(&p, &cat.t("open_file.card.cancel")).is_some(), "{tag}: no Cancel");
        }
    }
}

fn qa_save_report(lang: Lang) {
    use crate::save::report::{NotWritten, SaveReport};
    for (name, screen) in QA_SCREENS {
        let mut a = app_neutral(lang);
        let not_written = (0..30i64)
            .map(|i| NotWritten {
                kind: "wall".into(),
                id: Some(i + 1),
                reason: if i % 3 == 0 {
                    ddc_io::ifc::patch::REASON_LINK.to_string()
                } else {
                    "save.reason.no_exporter".to_string()
                },
            })
            .collect();
        a.save.report = Some(SaveReport {
            target: QA_BOOK.replace(".xlsx", ".ifc"),
            backend: "ifc".into(),
            secs: 1.5,
            bytes: 123_456,
            elements: 4000,
            proxies: 3,
            by_kind: [("wall".to_string(), 120u64), ("slab".to_string(), 8)].into_iter().collect(),
            ids_kept: 100,
            ids_new: 20,
            ids_conflict: 2,
            not_written_total: 30,
            not_written,
            warnings: vec![QA_LONG.repeat(2)],
            target_changed: true,
            replaced_unknown: true,
            prev: Some(QA_BOOK.to_string()),
            ..SaveReport::default()
        });
        a.save.report_open = true;
        let id = dialog_id("save.report");
        let p = probe_on(id, screen, |ctx| crate::save::report::window(&mut a, ctx));
        let tag = format!("save.report {name} {lang:?}");
        let title = a.sh.cat.t("save.report.title");
        // a read-only report: its links are the controls, there is no primary action
        assert_card(&tag, &p, lang, None, Some(&title));
    }
}

fn qa_recover(lang: Lang) {
    use crate::shell::recover_win::{layout_recover, Recover};
    use web_time::{Duration, SystemTime, UNIX_EPOCH};
    for (name, screen) in QA_SCREENS {
        for n in [0usize, 3, 12] {
            let mut a = app_neutral(lang);
            let now = SystemTime::now();
            let ms = now.duration_since(UNIX_EPOCH).unwrap().as_millis();
            let snaps = (0..n)
                .map(|i| ddc_model::SnapInfo {
                    path: std::path::PathBuf::from(format!("snap_{i}.ddc")),
                    ms: ms - (i as u128 + 1) * 90_000,
                    bytes: 4_200_000 + i as u64 * 1000,
                    damage: (i == 1).then(|| "integrity check failed".to_string()),
                })
                .collect();
            a.recover = Some(Recover {
                project: Some(std::path::PathBuf::from(QA_BOOK.replace(".xlsx", ".ddc"))),
                file_time: Some(now - Duration::from_secs(3600)),
                snaps,
                ..Recover::default()
            });
            let p = probe_on(modal_id("recover"), screen, |ctx| layout_recover(&mut a, ctx));
            let tag = format!("recover {n} snapshots {name} {lang:?}");
            let (restore, title) = (a.sh.cat.t("recover.restore"), a.sh.cat.t("recover.title"));
            assert_card(&tag, &p, lang, Some(&restore), Some(&title));
        }
    }
}

en_ru!(qa_xlsx_progress, qa_xlsx_progress_en, qa_xlsx_progress_ru);
en_ru!(qa_xlsx_scope, qa_xlsx_scope_en, qa_xlsx_scope_ru);
en_ru!(qa_xlsx_overwrite, qa_xlsx_overwrite_en, qa_xlsx_overwrite_ru);
en_ru!(qa_xlsx_loss, qa_xlsx_loss_en, qa_xlsx_loss_ru);
en_ru!(qa_xlsx_cell_notes, qa_xlsx_cell_notes_en, qa_xlsx_cell_notes_ru);
en_ru!(qa_xlsx_foreign, qa_xlsx_foreign_en, qa_xlsx_foreign_ru);
en_ru!(qa_settings, qa_settings_en, qa_settings_ru);
en_ru!(qa_selection_filter, qa_selection_filter_en, qa_selection_filter_ru);
en_ru!(qa_level, qa_level_en, qa_level_ru);
en_ru!(qa_open_card, qa_open_card_en, qa_open_card_ru);
en_ru!(qa_save_report, qa_save_report_en, qa_save_report_ru);
en_ru!(qa_recover, qa_recover_en, qa_recover_ru);

/// Schedule import preview with every part shown: the note of a book saved again, counts, rows
/// without Id, a long list of changes and refusals. Speaks the language it is given.
fn schedule_import(lang: Lang) {
    use crate::schedule::xlsx_in::{CellChange, Plan, Refusal, Why};
    let ru = matches!(lang, Lang::Ru);
    crate::with_test_lang(ru, || {
        let mut pl = Plan::default();
        pl.sheet = "Walls".into();
        pl.errors = 2;
        pl.no_id_rows = (40..60).collect();
        pl.ignored_cols = vec!["Notes of the site team, second issue".into()];
        for i in 0..40u32 {
            pl.changes.push(CellChange {
                row: i + 2,
                col: 4,
                element: (100 + i).to_string(),
                column: "Unconnected height of the wall above its base level".into(),
                before: "3000".into(),
                after: "3500".into(),
            });
            pl.cells.push((i as usize, 0, "3500mm".into()));
        }
        pl.refusals.push(Refusal {
            row: 9,
            col: 5,
            header: "Mark".into(),
            text: "#REF!".into(),
            why: Why::FormulaError,
        });
        let mut v = crate::schedule::ui::View::default();
        v.import = Some(crate::schedule::ui::Pending {
            path: std::path::PathBuf::from("a rather long book name for the schedule import.xlsx"),
            sheet: ddc_io::xlsx::ReadSheet::default(),
            plan: pl,
            key: None,
            stamp: None,
            note: Some(crate::tr("sched_import.resaved", &[])),
        });
        let p = probe_id(egui::Id::new("sch3_import"), |ctx| v.import_frame(ctx));
        let tag = format!("schedule_import {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
        let apply = crate::tr("sched_import.apply", &[("n", "40")]);
        assert!(p.controls.iter().any(|c| c.label == apply), "{tag}: Apply names the count");
    });
}

#[test]
fn schedule_import_en() {
    schedule_import(Lang::En);
}

#[test]
fn schedule_import_ru() {
    schedule_import(Lang::Ru);
}

/// "Reading workbook..." with its Cancel while the worker reads.
fn schedule_reading(lang: Lang) {
    let ru = matches!(lang, Lang::Ru);
    crate::with_test_lang(ru, || {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut ed);
        let mut v = crate::schedule::ui::View::default();
        v.set_table(crate::schedule::build(&ed, crate::schedule::Cat::Walls), ed.tool.units);
        let path = std::env::temp_dir().join("ddc_layout_sch3_missing.xlsx");
        assert!(v.import_read(&ed, &path, ed.tool.units).is_none());
        let p = probe_id(egui::Id::new("sch3_reading"), |ctx| v.import_frame(ctx));
        let tag = format!("schedule_reading {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
        let cancel = crate::tr("sched_import.cancel", &[]);
        assert!(p.controls.iter().any(|c| c.label == cancel), "{tag}: Cancel is offered");
        v.cancel_read();
    });
}

#[test]
fn schedule_reading_en() {
    schedule_reading(Lang::En);
}

#[test]
fn schedule_reading_ru() {
    schedule_reading(Lang::Ru);
}

/// Windows drawn alone in both languages: the grid set, the preview of an attached file, the
/// link consent, the missing-workbook card in its cases and every step of the first-start wizard.
fn small_windows(lang: Lang) {
    let ru = lang == Lang::Ru;
    let mut probes: Vec<(String, Probe)> = vec![];
    crate::with_test_lang(ru, || {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::grid_set_ui::open();
        let p = probe("grid_set", |ctx| crate::grid_set_ui::window(ctx, &mut ed, ru));
        probes.push(("grid set".into(), p));
        probes.push(("attached file preview".into(), crate::ai::ui::attach_ui::layout_probe(lang)));
        for (found, left) in [(0, false), (1, false), (3, false), (1, true)] {
            let p = crate::shell::xlsx_relink::layout_probe(ru, found, left);
            probes.push((format!("workbook card found={found} left={left}"), p));
        }
        // the ERP link is an optional feature: its consent window exists only with it
        #[cfg(feature = "link")]
        {
            let mut app = app_neutral(lang);
            probes.push(("link consent".into(), crate::link::panel::consent_layout_probe(&mut app)));
        }
        let mut app = app_neutral(lang);
        app.open_onboarding(0);
        for step in 0..crate::shell::onboard::STEPS {
            app.chrome.onboard.step = step;
            let p = probe_on(modal_id("onboard"), (1280.0, 720.0), |ctx| app.onboarding_window(ctx));
            probes.push((format!("wizard step {step}"), p));
        }
    });
    for (what, p) in &probes {
        let tag = format!("{what} {lang:?}");
        assert!(p.inner.is_some(), "{tag}: the window was not drawn");
        assert_contract(&tag, p, false);
        assert_language(&tag, p, lang);
    }
}

#[test]
fn small_windows_en() {
    small_windows(Lang::En);
}

#[test]
fn small_windows_ru() {
    small_windows(Lang::Ru);
}

/// Multi-selection of two walls with a project parameter that differs: the block title, the
/// parameter row and `<varies>` show whole at the panel widths, the row stays editable.
fn multi_params(lang: Lang) {
    let ru = lang == Lang::Ru;
    let mut ed = Editor::new_in_memory().unwrap();
    crate::schedule::test_room(&mut ed);
    let spec = ddc_model::ParamSpec {
        name: "Mark".into(),
        ptype: ddc_model::ParamType::Text,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    };
    let pid = ed.model.add_param(&spec).unwrap();
    ed.reload().unwrap();
    let ids: Vec<i64> = ed.walls.iter().map(|w| w.id).take(2).collect();
    crate::schedule::extra::edit_param(&mut ed, pid, "wall", ids[0], "A-1");
    let sels: Vec<crate::editor::Sel> = ids.iter().map(|i| crate::editor::Sel::Wall(*i)).collect();
    let title = crate::tr_lang(ru, "sketch_edit.props_title", &[]);
    let varies = crate::tr_lang(ru, "type_dialog.panels.varies", &[]);
    for screen in MAIN_SCREENS {
        for w in PROPS_WIDTHS {
            let p = props_panel(screen, w, |ui| crate::params_multi::show(ui, &mut ed, ru, &sels));
            let tag = format!("multi params {lang:?} {screen:?} panel {w}");
            assert!(p.elided.is_empty(), "{tag}: elided {:?}", p.elided);
            // section headers are drawn in capitals (Role::Section): compare without case
            for t in [title.as_str(), "Mark", varies.as_str()] {
                let t = t.to_lowercase();
                let shown = p.text.iter().any(|s| s.to_lowercase().contains(&t));
                assert!(shown, "{tag}: {t:?} in {:?}", p.text);
            }
            let fields = p.controls.iter().filter(|c| c.role == accesskit::Role::TextInput);
            assert!(fields.count() >= 1, "{tag}: no value field in {:?}", p.controls);
        }
    }
    assert_eq!(ed.model.param_value(pid, "wall", ids[1]).unwrap(), None, "drawing writes nothing");
}

en_ru!(multi_params, multi_params_en, multi_params_ru);

/// Welcome card over an empty plan at 1024 and 1280 px: every text stays inside the card
/// (a long recent path is cut in the middle), the import formats stay inside their tile
/// and the two-letter import mnemonic shows as two keycaps.
fn welcome_card(lang: Lang) {
    let id = egui::Id::new("ddc.welcome");
    // paths as this system writes them: a backslash separates folders on one system only
    let sep = std::path::MAIN_SEPARATOR_STR;
    let long = [
        "C:", "Users", "someone", "AppData", "Local", "Temp", "projects", "a-very-long-folder-name",
        "another-long-folder", "deeper", "still-deeper", "Tower.ddc",
    ]
    .join(sep);
    for screen in [(1024.0, 768.0), (1280.0, 800.0)] {
        let mut app = app_in(lang);
        app.prefs.recent = vec![long.clone(), ["C:", "p", "Short.ifc"].join(sep)];
        let (ctx, mut th) = driven_ctx();
        let mut frame = |ctx: &egui::Context| {
            let area = ctx.screen_rect();
            app.welcome(ctx, area, &mut vec![]);
        };
        let mut p = Probe::default();
        for _ in 0..FRAMES {
            p = pass_on(&ctx, &mut th, id, screen, vec![], &mut frame);
        }
        let tag = format!("welcome {lang:?} {screen:?}");
        let card = egui::AreaState::load(&ctx, id).expect("welcome drawn").rect();
        // the frame keeps a 24 px margin; text may not reach into it
        let content = card.shrink(24.0 - EPS);
        let out: Vec<_> = p
            .spans
            .iter()
            .filter(|(_, r)| r.left() < content.left() || r.right() > content.right())
            .map(|(t, r)| format!("{t:?} {r:?}"))
            .collect();
        assert!(out.is_empty(), "{tag}: text leaves the card {content:?}: {out:?}");
        assert!(p.elided.is_empty(), "{tag}: elided {:?}", p.elided);
        assert!(p.text.iter().any(|t| t.contains("Tower.ddc")), "{tag}: recent name");
        assert!(p.text.iter().any(|t| t.contains('\u{2026}')), "{tag}: path not cut");
        assert!(p.text.iter().any(|t| t.contains(".xlsx")), "{tag}: .xlsx shown");
        assert!(!p.text.iter().any(|t| t == "IC"), "{tag}: mnemonic as one keycap");
    }
}

/// The welcome card is drawn on an empty plan, steps back while a menu is open (nothing of it
/// under the menu) and is gone for good after Esc, the menu or not.
fn welcome_card_esc_and_menu(lang: Lang) {
    let id = egui::Id::new("ddc.welcome");
    let lead = Catalog::new(lang).t("welcome.lead");
    let mut app = app_in(lang);
    let (ctx, mut th) = driven_ctx();
    let menu = egui::Id::new("s17.menu");
    let mut run = |app: &mut EditorApp, open: bool, events: Vec<egui::Event>| {
        let mut frame = |ctx: &egui::Context| {
            if open {
                egui::Popup::open_id(ctx, menu);
            } else {
                egui::Popup::close_id(ctx, menu);
            }
            let area = ctx.screen_rect();
            app.welcome(ctx, area, &mut vec![]);
        };
        pass_on(&ctx, &mut th, id, (1280.0, 800.0), events, &mut frame)
    };
    // a fresh context installs its fonts on the first pass and draws from the next
    let mut p = Probe::default();
    for _ in 0..FRAMES {
        p = run(&mut app, false, vec![]);
    }
    assert!(p.text.iter().any(|t| *t == lead), "{lang:?}: the card is drawn");
    let p = run(&mut app, true, vec![]);
    assert!(!p.text.iter().any(|t| *t == lead), "{lang:?}: card under the open menu");
    assert!(!app.welcome_closed, "{lang:?}: a menu only hides it for a while");
    let p = run(&mut app, false, vec![]);
    assert!(p.text.iter().any(|t| *t == lead), "{lang:?}: back after the menu");
    let esc = egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: Some(egui::Key::Escape),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let p = run(&mut app, false, vec![esc]);
    assert!(app.welcome_closed, "{lang:?}: Esc dismisses the card");
    assert!(!p.text.iter().any(|t| *t == lead), "{lang:?}: gone on the Esc frame");
    for open in [true, false] {
        let p = run(&mut app, open, vec![]);
        assert!(!p.text.iter().any(|t| *t == lead), "{lang:?}: stays gone, menu {open}");
    }
}

en_ru!(welcome_card_esc_and_menu, welcome_card_esc_and_menu_en, welcome_card_esc_and_menu_ru);

/// Audit 11: the empty-canvas card steps back while a drawing tool is on or a window covers
/// it (settings here), and comes back once both are gone; it is never closed for good.
fn welcome_card_waits_for_tools_and_windows(lang: Lang) {
    use crate::wall_tool::Mode;
    let id = egui::Id::new("ddc.welcome");
    let lead = Catalog::new(lang).t("welcome.lead");
    let mut app = app_in(lang);
    let (ctx, mut th) = driven_ctx();
    let mut run = |app: &mut EditorApp, settings: bool| {
        let mut frame = |ctx: &egui::Context| {
            let area = ctx.screen_rect();
            app.welcome(ctx, area, &mut vec![]);
            if settings {
                crate::shell::settings_win::layout_frame(app, ctx, 0, "");
            }
        };
        let mut p = Probe::default();
        for _ in 0..FRAMES {
            p = pass_on(&ctx, &mut th, id, (1280.0, 800.0), vec![], &mut frame);
        }
        p.text.iter().any(|t| *t == lead)
    };
    assert!(run(&mut app, false), "{lang:?}: drawn on an empty plan");
    app.ed.set_mode(Mode::Rect);
    assert!(!run(&mut app, false), "{lang:?}: shown with the room tool on");
    app.ed.set_mode(Mode::Polyline);
    assert!(run(&mut app, false), "{lang:?}: back on the default wall tool");
    assert!(!run(&mut app, true), "{lang:?}: shown under the settings window");
    assert!(run(&mut app, false), "{lang:?}: back after the window");
    assert!(!app.welcome_closed, "{lang:?}: only waits, never closed");
}

/// A press on the canvas beside the card dismisses it for good (the press still reaches the tool).
fn welcome_card_dismissed_by_canvas_click(lang: Lang) {
    let id = egui::Id::new("ddc.welcome");
    let mut app = app_in(lang);
    let (ctx, mut th) = driven_ctx();
    let mut run = |app: &mut EditorApp, events: Vec<egui::Event>| {
        let mut frame = |ctx: &egui::Context| {
            let area = ctx.screen_rect();
            app.welcome(ctx, area, &mut vec![]);
        };
        pass_on(&ctx, &mut th, id, (1280.0, 800.0), events, &mut frame)
    };
    for _ in 0..FRAMES {
        run(&mut app, vec![]);
    }
    assert!(!app.welcome_closed);
    let at = egui::pos2(60.0, 740.0);
    run(
        &mut app,
        vec![egui::Event::PointerMoved(at), egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    assert!(app.welcome_closed, "{lang:?}: a canvas press dismisses the card");
}

en_ru!(welcome_card_dismissed_by_canvas_click, welcome_card_click_en, welcome_card_click_ru);

/// Desktop screen sizes at which the dialog anatomy is checked.
const DESK_SCREENS: [(f32, f32); 3] = [(1280.0, 720.0), (1440.0, 900.0), (1920.0, 1080.0)];

/// Start screen: one vocabulary (New project / Open), the units control has its label, no
/// caption is cut.
fn start_screen_at_desktop_widths(lang: Lang) {
    for screen in DESK_SCREENS {
        let mut a = app_in(lang);
        a.open_start();
        let p = probe_on(egui::Id::new("sheet_tools.start"), screen, |ctx| a.ui(ctx, None));
        let tag = format!("start {lang:?} {screen:?}");
        let sr = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(screen.0, screen.1));
        for key in ["start.new", "start.open", "sheet_tools.start.units"] {
            let want = a.sh.cat.t(key);
            let span = p.spans.iter().find(|(s, _)| s == &want);
            let (_, r) = span.unwrap_or_else(|| panic!("{tag}: {key} not drawn: {:?}", p.text));
            assert!(sr.contains_rect(*r), "{tag}: {key} outside the window {r:?}");
        }
        assert!(p.elided.is_empty(), "{tag}: cut captions {:?}", p.elided);
    }
}

en_ru!(
    start_screen_at_desktop_widths,
    start_screen_at_desktop_widths_en,
    start_screen_at_desktop_widths_ru
);

/// Settings keep one size on every page (workspace class or 90 % of the screen); the title
/// sits left, the close cross right, inside the window.
fn settings_anatomy_at_desktop_widths(lang: Lang) {
    use crate::shell::settings_win::{layout_frame, PAGE_COUNT};
    for screen in DESK_SCREENS {
        let mut widths = vec![];
        for page in 0..PAGE_COUNT as u8 {
            let mut app = app_neutral(lang);
            let (close, title) = (app.sh.cat.t("settings.close"), app.sh.cat.t("settings.title"));
            let p = probe_on(modal_id("settings"), screen, |ctx| {
                layout_frame(&mut app, ctx, page, "")
            });
            let tag = format!("settings anatomy page {page} {screen:?} {lang:?}");
            // the anatomy; the wording of each page is held by `qa_settings`
            assert_contract(&tag, &p, false);
            let unnamed: Vec<_> = p.controls.iter().filter(|c| c.label.trim().is_empty()).collect();
            assert!(unnamed.is_empty(), "{tag}: unnamed controls {unnamed:?}");
            let win = p.inner.expect("window");
            let screen_r = p.screen.expect("screen");
            assert!(screen_r.expand(EPS).contains_rect(win), "{tag}: window leaves the screen");
            assert!(win.expand(EPS).contains_rect(named(&p, &close).expect("close").rect));
            let x = named(&p, &close).expect("close").rect;
            let t = p.spans.iter().find(|(s, _)| *s == title).expect("title span").1;
            assert!(t.center().x < win.center().x, "{tag}: title not on the left {t:?}");
            assert!(x.center().x > win.center().x, "{tag}: close not on the right {x:?}");
            assert!(p.elided.iter().all(|e| *e != title), "{tag}: title cut");
            widths.push(win.width());
        }
        let lo = widths.iter().copied().fold(f32::MAX, f32::min);
        let hi = widths.iter().copied().fold(0.0f32, f32::max);
        assert!(hi - lo <= EPS, "settings {screen:?} {lang:?}: width follows the page {widths:?}");
        let cap = 980.0f32.min(screen.0 * 0.9);
        assert!(hi <= cap + 32.0, "settings {screen:?} {lang:?}: {hi} wider than {cap}");
    }
}

en_ru!(
    settings_anatomy_at_desktop_widths,
    settings_anatomy_at_desktop_widths_en,
    settings_anatomy_at_desktop_widths_ru
);

/// Recovery at desktop widths: the primary Restore closes the footer on the right, the
/// destructive archive action stays on the left, secondary-shaped.
fn recover_anatomy_at_desktop_widths(lang: Lang) {
    use crate::shell::recover_win::{layout_recover, Recover};
    use web_time::{Duration, SystemTime, UNIX_EPOCH};
    for screen in DESK_SCREENS {
        let mut a = app_neutral(lang);
        let now = SystemTime::now();
        let ms = now.duration_since(UNIX_EPOCH).unwrap().as_millis();
        let snaps = (0..3u64)
            .map(|i| ddc_model::SnapInfo {
                path: std::path::PathBuf::from(format!("snap_{i}.ddc")),
                ms: ms - (i as u128 + 1) * 90_000,
                bytes: 4_200_000 + i * 1000,
                damage: None,
            })
            .collect();
        a.recover = Some(Recover {
            project: Some(std::path::PathBuf::from("C:\\p\\Tower.ddc")),
            file_time: Some(now - Duration::from_secs(3600)),
            snaps,
            ..Recover::default()
        });
        let p = probe_on(modal_id("recover"), screen, |ctx| layout_recover(&mut a, ctx));
        let tag = format!("recover anatomy {screen:?} {lang:?}");
        let (restore, title) = (a.sh.cat.t("recover.restore"), a.sh.cat.t("recover.title"));
        assert_card(&tag, &p, lang, Some(&restore), Some(&title));
        let win = p.inner.expect("window");
        let r = named(&p, &restore).expect("restore").rect;
        let foot: Vec<_> = p
            .controls
            .iter()
            .filter(|c| c.role == accesskit::Role::Button)
            .filter(|c| (c.rect.center().y - r.center().y).abs() < 4.0)
            .collect();
        let right = foot.iter().map(|c| c.rect.right()).fold(0.0f32, f32::max);
        assert!((r.right() - right).abs() <= EPS, "{tag}: primary not last on the right {foot:?}");
        let archive = a.sh.cat.t("recover.archive_move");
        let ar = named(&p, &archive).unwrap_or_else(|| panic!("{tag}: no {archive:?}")).rect;
        assert!(ar.right() < win.center().x, "{tag}: destructive action not on the left {ar:?}");
        assert!(ar.top() >= r.top() - EPS, "{tag}: destructive action above the footer");
    }
}

en_ru!(
    recover_anatomy_at_desktop_widths,
    recover_anatomy_at_desktop_widths_en,
    recover_anatomy_at_desktop_widths_ru
);

en_ru!(
    welcome_card_waits_for_tools_and_windows,
    welcome_card_waits_for_tools_and_windows_en,
    welcome_card_waits_for_tools_and_windows_ru
);

en_ru!(welcome_card, welcome_card_en, welcome_card_ru);

/// The assistant's view panel: legend rows with a long Russian empty value, the status lines
/// and Reset view, all inside the card; nothing drawn once the view is reset.
fn ai_view_panel(lang: Lang) {
    use crate::ai::sets::Key;
    use crate::ai::ui::found;
    use crate::ai::view_state;
    use crate::ai::view_tools::{self, ViewCmd};
    use std::collections::BTreeMap;
    use std::sync::Arc;
    let mut app = app_in(lang);
    view_state::forget();
    let mut g: BTreeMap<String, Vec<Key>> = BTreeMap::new();
    for id in 0..30i64 {
        let v = ["EI 30", "EI 60", "EI 90", ""][(id % 4) as usize];
        g.entry(v.to_string()).or_default().push(Key { cat: "window", id });
    }
    let keys = Arc::new(g["EI 30"].clone());
    view_state::apply(&mut app, &ViewCmd::Isolate { keys, hide: true });
    view_state::apply(&mut app, &ViewCmd::Color(Arc::new(view_tools::legend("fire_rating", g))));
    let th = Theme::new(Mode::Dark, Density::Compact);
    let area = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN.0, SCREEN.1));
    let id = egui::Id::new(found::VIEW_PANEL_ID);
    let p = probe_id(id, |ctx| found::view_panel(ctx, &th, &app.sh.cat, area));
    let tag = format!("ai view panel {lang:?}");
    assert_contract(&tag, &p, true);
    let rows = p.controls.iter().filter(|c| c.role == accesskit::Role::Button).count();
    assert!(rows >= 5, "{tag}: four legend rows and Reset view, {rows}");
    view_state::reset(&mut app);
    let p = probe_id(id, |ctx| found::view_panel(ctx, &th, &app.sh.cat, area));
    assert!(p.controls.is_empty(), "{tag}: the panel goes with the assistant's view");
}

en_ru!(ai_view_panel, ai_view_panel_en, ai_view_panel_ru);

/// In a Properties-wide panel the AI source badge and the bulk "Mark verified" read in
/// full, in the panel's language, and the button stays inside the panel.
fn value_src_props(lang: Lang) {
    let ru = lang == Lang::Ru;
    let (mut ed, w1, _) = crate::value_src_ui::test_editor();
    ed.selection = vec![crate::editor::Sel::Wall(w1)];
    let id = egui::Id::new("ag2.props");
    let (ctx, mut th) = driven_ctx();
    let mut frame = |c_: &egui::Context| {
        egui::SidePanel::right(id).exact_width(300.0).show(c_, |ui| {
            crate::value_src_ui::bulk_bar(ui, &mut ed, ru);
            crate::params_ui::props(ui, &mut ed, ru);
        });
    };
    let mut p = Probe::default();
    for _ in 0..FRAMES {
        p = pass_on(&ctx, &mut th, id, (1280.0, 800.0), vec![], &mut frame);
    }
    let tag = format!("value source props {lang:?}");
    let bulk = crate::tr_lang(ru, "value_src.bulk_verify", &[("n", "1")]);
    let badge = crate::tr_str(ru, "value_src.badge.ai");
    for s in [bulk.as_str(), badge] {
        assert!(has_text(&p, s), "{tag}: {s:?} not drawn in {:?}", p.text);
        assert!(!p.elided.iter().any(|e| e == s), "{tag}: {s:?} cut");
    }
    let b = p.controls.iter().find(|c| c.label == bulk).expect("the bulk button");
    let panel = ctx.screen_rect();
    assert!(b.rect.right() <= panel.right() + EPS, "{tag}: the button leaves the screen");
    assert!(b.rect.left() >= panel.right() - 300.0 - EPS, "{tag}: outside the panel");
    let words = p.controls.iter().any(|c| has_cyrillic(&c.label));
    assert_eq!(words, ru, "{tag}: the controls speak the panel's language");
}

en_ru!(value_src_props, value_src_props_en, value_src_props_ru);

/// Requirements offer cards and a report with its manual list at the panel's narrowest width:
/// nothing clipped, every control labelled, in the probe's language.
fn ai_requirements(lang: Lang) {
    let p = crate::ai::ui::requirements::tests::layout_probe(lang);
    let tag = format!("ai_requirements {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    let cat = Catalog::new(lang);
    for key in ["req.offer.save", "req.load", "req.template"] {
        let label = cat.t(key);
        assert_eq!(p.controls.iter().filter(|c| c.label == label).count(), 1, "{tag}: {key}");
    }
}

en_ru!(ai_requirements, ai_requirements_en, ai_requirements_ru);

/// Work plane window with a reference plane to pick: nothing clipped, labelled, one language.
fn workplane_win(lang: Lang) {
    let mut app = app_neutral(lang);
    app.ed.model.add_ref_plane("Axis K", (0.0, 0.0), (0.0, 5.0)).unwrap();
    app.ed.reload().unwrap();
    crate::edit3d::wp_win::open_for_layout(&mut app);
    let p = probe("workplane", |ctx| crate::edit3d::wp_win::frame(&mut app, ctx));
    let tag = format!("workplane {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
}

en_ru!(workplane_win, workplane_win_en, workplane_win_ru);

/// Classifications on a selected wall with a code already assigned.
fn classify_win(lang: Lang) {
    use crate::classify_ui as cls;
    let mut app = app_neutral(lang);
    select_room_walls(&mut app);
    app.ed.selection = vec![crate::editor::Sel::Wall(app.ed.walls[0].id)];
    let ts = cls::targets(&app.ed, cls::Scope::Categories);
    cls::assign(&mut app.ed, "Building", Some("21.10"), &ts[..1]).unwrap();
    cls::open_for_layout("Building", "21.20", cls::Scope::Categories);
    let p = probe("classify", |ctx| cls::frame(&mut app, ctx));
    cls::open_for_layout("", "", cls::Scope::Selection);
    let tag = format!("classify {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    let assign = Catalog::new(lang).t("cls.assign");
    assert!(p.controls.iter().any(|c| c.label == assign), "{tag}: no Assign button");
}

en_ru!(classify_win, classify_win_en, classify_win_ru);

/// Wall profile editor on a gable outline.
fn wall_profile_win(lang: Lang) {
    use crate::wall_profile_ui as wp;
    let mut app = app_neutral(lang);
    select_room_walls(&mut app);
    let id = app.ed.walls[0].id;
    let len = app.ed.model.wall_length(id).unwrap();
    let g = wp::preset(wp::Preset::Gable, len, 3.0, 1.0);
    wp::write(&mut app.ed, id, Some(g)).unwrap();
    wp::open_for_layout(&app.ed, id);
    let p = probe("wall_profile", |ctx| wp::frame(&mut app, ctx));
    let tag = format!("wall profile {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
}

en_ru!(wall_profile_win, wall_profile_win_en, wall_profile_win_ru);

/// Insert fitting on a duct, both the transition and the cap page.
fn mep_fitting_win(lang: Lang) {
    use crate::fitting_insert as fi;
    for kind in [fi::Kind::Transition, fi::Kind::Cap] {
        let mut app = app_neutral(lang);
        let r = fi::test_duct(&mut app.ed);
        fi::open_for_layout(&app.ed, r, kind);
        let p = probe("mep_fitting", |ctx| fi::frame(&mut app, ctx));
        let tag = format!("mep fitting {kind:?} {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
    }
}

en_ru!(mep_fitting_win, mep_fitting_win_en, mep_fitting_win_ru);

/// Explain element on a pinned wall with a profile.
fn explain_win(lang: Lang) {
    let mut app = app_neutral(lang);
    select_room_walls(&mut app);
    let id = app.ed.walls[0].id;
    app.ed.model.pin(&[("wall", id)]).unwrap();
    app.ed.reload().unwrap();
    app.ed.selection = vec![crate::editor::Sel::Wall(id)];
    crate::explain_ui::open_for_layout();
    let p = probe("explain", |ctx| crate::explain_ui::frame(&mut app, ctx));
    let tag = format!("explain {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
}

en_ru!(explain_win, explain_win_en, explain_win_ru);

/// Generation rules on a two-storey office: every tab on the three desktop screens, and the
/// note a model without a generated project gets.
fn gen_rules_win(lang: Lang) {
    let office = serde_json::json!({"typology": "office", "floors": 2});
    for screen in DESK_SCREENS {
        for tab in 0..4 {
            let mut app = app_neutral(lang);
            crate::gen_rules_ui::open_for_layout(Some(office.clone()), tab);
            let p = probe_on(dialog_id("gen_rules"), screen, |ctx| crate::gen_rules_ui::frame(&mut app, ctx));
            let tag = format!("gen rules tab {tab} {screen:?} {lang:?}");
            assert_contract(&tag, &p, false);
            assert_language(&tag, &p, lang);
        }
        let mut app = app_neutral(lang);
        crate::gen_rules_ui::open_for_layout(None, 0);
        let p = probe_on(dialog_id("gen_rules"), screen, |ctx| crate::gen_rules_ui::frame(&mut app, ctx));
        let tag = format!("gen rules empty {screen:?} {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
    }
}

en_ru!(gen_rules_win, gen_rules_win_en, gen_rules_win_ru);

/// Where is... with hits, and with nothing typed.
fn where_is_win(lang: Lang) {
    for q in ["schedule", ""] {
        let mut app = app_neutral(lang);
        crate::where_is::open_for_layout(q);
        let p = probe("where_is", |ctx| crate::where_is::frame(&mut app, ctx));
        let tag = format!("where is {q:?} {lang:?}");
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
    }
}

en_ru!(where_is_win, where_is_win_en, where_is_win_ru);

/// Drawing set dialog on a one-room model: options, the sheet list it will make, the footer;
/// nothing clipped, every control labelled, in the probe's language.
fn drawing_set(lang: Lang) {
    let mut app = app_neutral(lang);
    one_room(&mut app);
    crate::sheet_view::with(|s| s.dset = Some(crate::sheet_view::DsetDialog::default()));
    let p = probe("drawing_set", |ctx| crate::sheet_view::windows(ctx, &mut app));
    crate::sheet_view::with(|s| s.dset = None);
    let tag = format!("drawing_set {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    let cat = Catalog::new(lang);
    let apply = cat.t("dset.dlg.apply");
    assert_eq!(p.controls.iter().filter(|c| c.label == apply).count(), 1, "{tag}: apply");
    let first = format!("{}1", cat.t("dset.prefix"));
    assert!(p.text.iter().any(|t| t.starts_with(&first)), "{tag}: sheet list {:?}", p.text);
}

en_ru!(drawing_set, drawing_set_en, drawing_set_ru);

/// The delivery review on a one-room model: brief, drawing set, files, folder, one primary;
/// with a brief the primary hands it to the assistant. Nothing clipped, in the probe's language.
fn deliver_review(lang: Lang) {
    use crate::deliver::ui::{frame, with, Review};
    let ru = lang == Lang::Ru;
    let mut app = app_neutral(lang);
    one_room(&mut app);
    let cat = Catalog::new(lang);
    for brief in ["", "Office, 5 floors"] {
        with(|s| {
            let mut r = Review::new(&app, ru);
            r.brief = brief.to_string();
            s.review = Some(r);
            s.shown = true;
        });
        let p = probe("deliver", |ctx| frame(ctx, &mut app));
        with(|s| *s = Default::default());
        let tag = format!("deliver_review {lang:?} brief={}", !brief.is_empty());
        assert_contract(&tag, &p, false);
        assert_language(&tag, &p, lang);
        let main = cat.t(if brief.is_empty() { "deliver.go" } else { "deliver.draft" });
        assert_eq!(p.controls.iter().filter(|c| c.label == main).count(), 1, "{tag}: primary");
        for key in ["deliver.pdf", "deliver.ifc"] {
            let l = cat.t(key);
            assert!(p.controls.iter().any(|c| c.label == l), "{tag}: {key}");
        }
    }
}

en_ru!(deliver_review, deliver_review_en, deliver_review_ru);

/// The delivery result after a real run: a row per file with Open, Show in folder as the
/// primary; nothing clipped, in the probe's language.
fn deliver_result(lang: Lang) {
    use crate::deliver::ui::{frame, start, with, Review};
    let ru = lang == Lang::Ru;
    let mut app = app_neutral(lang);
    one_room(&mut app);
    let name = format!("ddc-deliver-probe-{lang:?}-{}", std::process::id());
    let dir = std::env::temp_dir().join(name);
    let mut r = Review::new(&app, ru);
    r.target = crate::deliver::Target {
        dir: dir.clone(),
        stem: "Probe".into(),
    };
    let mut run = start(&mut app, &r, ru).unwrap();
    let end = web_time::Instant::now() + std::time::Duration::from_secs(120);
    while !run.step(&app.ed) {
        assert!(web_time::Instant::now() < end, "the run did not end");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    with(|s| {
        s.run = Some(run);
        s.shown = true;
    });
    let p = probe("deliver", |ctx| frame(ctx, &mut app));
    with(|s| *s = Default::default());
    let _ = std::fs::remove_dir_all(&dir);
    let tag = format!("deliver_result {lang:?}");
    assert_contract(&tag, &p, false);
    assert_language(&tag, &p, lang);
    let cat = Catalog::new(lang);
    let open = cat.t("deliver.open");
    assert_eq!(p.controls.iter().filter(|c| c.label == open).count(), 2, "{tag}: Open per file");
    let show = cat.t("deliver.show_folder");
    assert_eq!(p.controls.iter().filter(|c| c.label == show).count(), 1, "{tag}: show");
    assert!(p.text.iter().any(|t| t == &cat.t("deliver.done.title")), "{tag}: {:?}", p.text);
}

en_ru!(deliver_result, deliver_result_en, deliver_result_ru);

/// A closed room of four walls on the current level, for the drawing set probes.
fn one_room(app: &mut EditorApp) {
    let lv = app.ed.level;
    let layer = ddc_model::LayerSpec {
        material: "brick".into(),
        thickness: 0.3,
    };
    let wt = app.ed.model.add_wall_type("Probe 300", &[layer]).unwrap();
    let pts = [(0.0, 0.0), (9.0, 0.0), (9.0, 6.0), (0.0, 6.0), (0.0, 0.0)];
    let specs: Vec<ddc_model::WallSpec> = pts
        .windows(2)
        .map(|w| ddc_model::WallSpec {
            a: w[0],
            b: w[1],
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .collect();
    app.ed.model.add_walls(&specs).unwrap();
}

/// AI tabs (chat, build, review) in setup, idle, draft, applied and checked states at
/// 1280, 1440 and 1920 points: the whole app frame keeps the contract.
fn ai_tabs(lang: Lang) {
    crate::ai::ui::tests_modes::contract(lang);
}

en_ru!(ai_tabs, ai_tabs_en, ai_tabs_ru);

/// Form rows of the whole app at 1280, 1440 and 1920 points: one wall selected (canvas bar,
/// option bar, Properties) and two walls; then the File menu. Every label and unit sits on its
/// field's centre line, no number breaks over lines, no line of text is cut by its clip, and
/// the menu rows share one height and width with their shortcuts clear of their labels.
fn app_form_rows(lang: Lang) {
    use crate::ai::ui::tests_modes::{Shell, SIZES};
    use crate::editor::Sel;
    let mut fails: Vec<String> = vec![];
    for size in SIZES {
        let mut sh = Shell::new(lang, size);
        // the app starts in Select; the room is typed with the wall tool
        sh.app.ed.set_mode(crate::wall_tool::Mode::Polyline);
        crate::schedule::test_room(&mut sh.app.ed);
        sh.app.ed.set_mode(crate::wall_tool::Mode::Select);
        let walls: Vec<i64> = sh.app.ed.walls.iter().map(|w| w.id).collect();
        assert!(walls.len() >= 2, "the room has walls");
        for n in [1, 2] {
            sh.app.ed.selection = walls[..n].iter().map(|i| Sel::Wall(*i)).collect();
            let p = sh.settle();
            let tag = format!("{lang:?} {}x{} walls={n}", size.0, size.1);
            if !p.controls.iter().any(|c| c.role == accesskit::Role::TextInput) {
                fails.push(format!("{tag}: no field on screen"));
            }
            let bad = row_faults(&p, true);
            if !bad.is_empty() {
                eprintln!("{}", dump(&tag, &p, &bad));
            }
            fails.extend(bad.into_iter().map(|b| format!("{tag}: {b}")));
        }
        sh.app.ed.selection.clear();
        sh.settle();
        sh.pass(alt_f());
        let p = sh.settle();
        let tag = format!("{lang:?} {}x{} file menu", size.0, size.1);
        fails.extend(menu_faults(&p, &sh.app.sh.cat, &sh.app.sh.theme).into_iter().map(|b| format!("{tag}: {b}")));
        fails.extend(row_faults(&p, true).into_iter().map(|b| format!("{tag}: {b}")));
    }
    assert!(fails.is_empty(), "{} faults:\n{}", fails.len(), fails.join("\n"));
}

/// Menu width range of the design rules (UX audit), in points. The row height follows the
/// density since design system v3 (`menu::row_h`: 28 comfortable, 24 compact and dense).
const MENU_WIDTH: (f32, f32) = (220.0, 320.0);

/// Faults of the open File menu: rows of different heights or widths, rows off the design
/// size, a cut label, a shortcut that touches its label or leaves its row.
fn menu_faults(p: &Probe, cat: &Catalog, th: &Theme) -> Vec<String> {
    let menu_row = ddc_ui::widgets::menu::row_h(th);
    let ids = ["file.new", "file.open", "file.save", "file.save_as", "data.export_xlsx"];
    let labels: Vec<String> = ids.iter().map(|id| cat.t(&format!("cmd.{id}"))).collect();
    let named: Vec<&Control> = p.controls.iter().filter(|c| labels.contains(&c.label)).collect();
    // the menu's rows stand in one column; a ribbon button of the same name stands elsewhere
    let col = |x: f32| named.iter().filter(|o| (o.rect.left() - x).abs() <= EPS).count();
    let Some(x) = named.iter().map(|c| c.rect.left()).max_by_key(|x| col(*x)) else {
        return vec![format!("menu rows 0 of {}", labels.len())];
    };
    let rows: Vec<&Control> =
        named.into_iter().filter(|c| (c.rect.left() - x).abs() <= EPS).collect();
    let mut bad = vec![];
    if rows.len() < labels.len() {
        return vec![format!("menu rows {} of {}", rows.len(), labels.len())];
    }
    let first = rows[0].rect;
    for c in &rows {
        let r = c.rect;
        if (r.height() - first.height()).abs() > EPS || (r.width() - first.width()).abs() > EPS {
            let (l, s, f) = (&c.label, r.size(), first.size());
            bad.push(format!("row {l:?} is {s:?}, the first is {f:?}"));
        }
        // the design rules: menu rows of the density's height, menus 220 to 320 wide
        let (lo, hi) = MENU_WIDTH;
        if r.height() < menu_row - EPS || r.width() < lo - EPS || r.width() > hi + EPS {
            let (l, s) = (&c.label, r.size());
            bad.push(format!("row {l:?} is {s:?}: not {menu_row} high, {lo}..{hi} wide"));
        }
        if p.elided.contains(&c.label) {
            bad.push(format!("row {:?} is elided", c.label));
        }
        let inside: Vec<&Line> = p.lines.iter().filter(|l| r.contains(l.rect.center())).collect();
        let Some(label) = inside.iter().find(|l| l.text == c.label) else {
            bad.push(format!("row {:?} paints no label", c.label));
            continue;
        };
        let gap = ddc_ui::theme::space::SM;
        // text of the window under the menu shares the place, not the menu's clip
        let keys = inside.iter().filter(|l| l.text != c.label && same_clip(l.clip, label.clip));
        for k in keys {
            if k.rect.left() < label.rect.right() + gap || k.rect.right() > r.right() + EPS {
                let (k, l) = (&k.text, &c.label);
                bad.push(format!("key {k:?} of {l:?} touches its label or leaves the row"));
            }
        }
    }
    bad
}

en_ru!(app_form_rows, app_form_rows_en, app_form_rows_ru);

/// Every surface of the app at 1440 points: each settings page, and each command of the
/// registry run on a room with one wall selected (dialogs, option bars, tool panels, menus).
/// In each frame every label, unit and control text reads on its control's line.
fn app_text_rows(lang: Lang) {
    use crate::ai::ui::tests_modes::Shell;
    use crate::editor::Sel;
    crate::shot_press::set_screenshot_mode(true);
    let size = (1440.0, 900.0);
    let fresh = || {
        let mut sh = Shell::new(lang, size);
        sh.app.ed.set_mode(crate::wall_tool::Mode::Polyline);
        crate::schedule::test_room(&mut sh.app.ed);
        sh.app.ed.set_mode(crate::wall_tool::Mode::Select);
        let w = sh.app.ed.walls[0].id;
        sh.app.ed.selection = vec![Sel::Wall(w)];
        sh
    };
    let mut fails: Vec<String> = vec![];
    let mut seen = 0usize;
    let mut told = std::collections::HashSet::new();
    let mut check = |tag: &str, p: &Probe| {
        seen += 1;
        let bad = row_faults(p, false);
        // one dump per distinct fault, so a failure shows the rects behind it
        let new: Vec<String> = bad.iter().filter(|b| told.insert((*b).clone())).cloned().collect();
        if !new.is_empty() {
            eprintln!("{}", dump(tag, p, &new));
        }
        fails.extend(bad.into_iter().map(|b| format!("{tag}: {b}")));
    };
    let mut sh = fresh();
    for page in 0..crate::shell::settings_win::PAGES.len() as u8 {
        sh.app.open_settings(page);
        let p = sh.settle();
        check(&format!("settings page {page}"), &p);
    }
    let ids: Vec<String> = sh.app.sh.reg.all().iter().map(|c| c.id.to_string()).collect();
    drop(sh);
    // these leave the app (browser, network); they draw nothing of their own
    let away = ["help.check_update", "help.reference"];
    // one app per batch of commands: Esc closes what a command opened before the next runs
    let esc = || {
        let ev = |pressed| egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        vec![ev(true), ev(false)]
    };
    let mut sh = fresh();
    for (n, id) in ids.iter().filter(|i| !away.contains(&i.as_str())).enumerate() {
        if n % 40 == 39 {
            sh = fresh();
        }
        if !sh.app.run(id) {
            continue;
        }
        let p = sh.settle();
        check(id, &p);
        for _ in 0..2 {
            sh.pass(esc());
        }
        sh.app.ed.set_mode(crate::wall_tool::Mode::Select);
    }
    assert!(seen > 100, "only {seen} surfaces drawn");
    fails.sort();
    fails.dedup();
    assert!(fails.is_empty(), "{lang:?}: {} faults:\n{}", fails.len(), fails.join("\n"));
}

en_ru!(app_text_rows, app_text_rows_en, app_text_rows_ru);

/// The row probe itself: a label 6 px above its field's centre line is a fault, a centred one
/// and a number on one line are not, a number broken over two lines is.
#[test]
fn row_faults_catch_a_label_off_its_field_line() {
    for (shift, wrap) in [(0.0, false), (6.0, true)] {
        let mut s = String::from("12");
        let p = probe("uipol1_row_self_test", |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let top = ui.max_rect().min + egui::vec2(8.0, 8.0);
                let th = ddc_ui::theme::of(ui.ctx());
                let font = th.font(ddc_ui::theme::Role::Body);
                let ink = th.c("text.primary");
                let at = top + egui::vec2(96.0, 0.0);
                let field = egui::Rect::from_min_size(at, egui::vec2(80.0, 24.0));
                // a field whose text sits on its middle, as the kit's fields draw it
                let edit = egui::TextEdit::singleline(&mut s)
                    .desired_width(80.0)
                    .margin(egui::Margin::ZERO)
                    .vertical_align(egui::Align::Center);
                ui.put(field, edit);
                let g = ui.painter().layout_no_wrap("Width".into(), font.clone(), ink);
                let y = field.center().y - g.size().y / 2.0 - shift;
                ui.painter().galley(egui::pos2(top.x, y), g, ink);
                let w = if wrap { 20.0 } else { f32::INFINITY };
                let n = ui.painter().layout("3600 mm".into(), font, ink, w);
                ui.painter().galley(top + egui::vec2(0.0, 48.0), n, ink);
            });
        });
        let faults = row_faults(&p, true);
        let off = faults.iter().any(|f| f.starts_with("label") && f.contains("off the line"));
        let broken = faults.iter().any(|f| f.contains("breaks over"));
        assert_eq!((off, broken), (shift > 0.0, wrap), "shift {shift}: {faults:?}");
    }
}

/// Text of the app under a dialog is not the text of the dialog's field it lies under: only
/// the same text painted on the dialog's own layer is measured, and then it is a fault.
#[test]
fn row_faults_skip_text_a_dialog_covers() {
    // (text on the dialog, alpha of the dialog's background): the text counts unless it lies
    // under the dialog's opaque fill; a see-through fill hides nothing
    for (on_dialog, alpha) in [(false, 255u8), (true, 255), (false, 0), (false, 160)] {
        let filled = alpha == 255;
        let mut s = String::new();
        let p = probe("uipol1_cover_self_test", |ctx| {
            let field = egui::Rect::from_min_size(egui::pos2(120.0, 120.0), egui::vec2(120.0, 24.0));
            let th = ddc_ui::theme::of(ctx);
            let (font, ink) = (th.font(ddc_ui::theme::Role::Body), th.c("text.primary"));
            // 3 px above the middle: off its line, still inside the field
            let at = |h: f32| egui::pos2(field.left() + 8.0, field.center().y - h / 2.0 - 3.0);
            egui::CentralPanel::default().show(ctx, |ui| {
                let g = ui.painter().layout_no_wrap("Below".into(), font.clone(), ink);
                if !on_dialog {
                    ui.painter().galley(at(g.size().y), g, ink);
                }
            });
            egui::Window::new("cover")
                .id(egui::Id::new(("ddc.dialog", "uipol1_cover_self_test")))
                .title_bar(false)
                .fixed_pos(egui::pos2(100.0, 100.0))
                // as kit::Dialog: a window fading in is not yet opaque on the probe's last pass
                .fade_in(false)
                .frame(match alpha {
                    0 => egui::Frame::NONE,
                    a => {
                        let f = ctx.style().visuals.window_fill;
                        let fill = egui::Color32::from_rgba_unmultiplied(f.r(), f.g(), f.b(), a);
                        egui::Frame::window(&ctx.style()).fill(fill)
                    }
                })
                .show(ctx, |ui| {
                    let edit = egui::TextEdit::singleline(&mut s).desired_width(field.width()).frame(filled);
                    ui.put(field, edit);
                    let g = ui.painter().layout_no_wrap("Below".into(), font.clone(), ink);
                    if on_dialog {
                        ui.painter().galley(at(g.size().y), g, ink);
                    }
                });
        });
        assert!(p.lines.iter().any(|l| l.text == "Below"), "the text was not painted");
        let off = row_faults(&p, true).iter().any(|f| f.contains("off the middle"));
        let want = on_dialog || !filled;
        assert_eq!(off, want, "on the dialog {on_dialog}, fill alpha {alpha}: {:?}", row_faults(&p, true));
    }
}

/// The loading window running, failed, done and cancelled, with a long file name and a long
/// reason: it fits both screens, every control is named, in the probe's language.
fn qa_load_window(lang: Lang) {
    use crate::shell::load_win::{self, End, LoadWin};
    let ru = lang == Lang::Ru;
    let t = |k: &'static str| crate::tr_str(ru, k);
    let view = crate::jobs::JobView {
        title: "job.title.import",
        stage: "job.stage.commit",
        stages: crate::bigfile::STAGES_IFC,
        fraction: 0.72,
        elapsed: 14.0,
        eta: Some(6.0),
        counts: (5_230, 12_480),
        cancellable: true,
        file: Some("Tower_A_structure_coordination_model_v12_final_revision.ifc".into()),
        size: Some(195_035_136),
        detail: "job.kind.curtain_wall".into(),
        ..Default::default()
    };
    let failed = End::Failed { stage: "job.stage.commit", message: QA_LONG.repeat(2) };
    let done = End::Done { added: Some(12_480), total: 12_480, warnings: 3, log: true };
    let ends = [
        ("running", None, "job.cancel"),
        ("failed", Some(failed), "load.close"),
        ("done", Some(done), "load.log"),
        ("cancelled", Some(End::Cancelled), "load.close"),
    ];
    for (name, screen) in QA_SCREENS {
        for (case, end, primary) in ends.iter().cloned() {
            let mut app = app_in(lang);
            app.chrome.load = LoadWin::parked(view.clone(), end);
            let id = dialog_id(load_win::KEY);
            let p = probe_on(id, screen, |ctx| load_win::draw(&mut app, ctx));
            let tag = format!("load {case} {name} {lang:?}");
            assert_card(&tag, &p, lang, Some(t(primary)), Some(t("job.title.import")));
        }
    }
}

en_ru!(qa_load_window, qa_load_window_en, qa_load_window_ru);

// ------------------------------------------------ option row over the plan (tools/ui.rs)

/// The row a placement tool draws over the plan, on a pane of `screen` points.
fn plan_tool_row(lang: Lang, cmd: &str, screen: (f32, f32)) -> (Probe, EditorApp) {
    let mut a = app_in(lang);
    assert!(a.run(cmd), "{cmd}");
    let t = crate::shell::tokens(&a.sh.theme);
    let p = probe_on(egui::Id::new("t.plan.toolrow"), screen, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.max_rect();
            let painter = ui.painter().clone();
            let to_s = |v: ddc_geom::V2| egui::pos2(v.x as f32, v.y as f32);
            crate::tools::paint(ui, &painter, rect, &mut a.ed, &t, &to_s);
        });
    });
    (p, a)
}

/// Controls of the row stay inside a narrow pane: they wrap to the next line instead of running
/// past the edge, and the row repeats neither the tool name nor the level of the option bar.
fn plan_tool_row_wraps(lang: Lang) {
    let ru = lang == Lang::Ru;
    let narrow = (560.0, 700.0);
    for cmd in ["mep.duct", "mep.pipe", "mep.terminal", "str.column", "str.slab"] {
        let (p, a) = plan_tool_row(lang, cmd, narrow);
        assert!(!p.controls.is_empty(), "{lang:?} {cmd}: the row drew no control");
        for c in &p.controls {
            assert!(
                c.rect.left() >= -EPS && c.rect.right() <= narrow.0 + EPS,
                "{lang:?} {cmd}: {:?} {:?} runs past the pane",
                c.label,
                c.rect
            );
        }
        assert!(!has_text(&p, &crate::tr_lang(ru, "opt.level", &[])), "{lang:?} {cmd}: level repeated");
        let name = a.sh.cat.t(&format!("tool.{}", a.ed.model_tool().unwrap().kind.key()));
        assert!(!has_text(&p, &name), "{lang:?} {cmd}: tool name {name:?} repeated");
    }
}

en_ru!(plan_tool_row_wraps, plan_tool_row_wraps_en, plan_tool_row_wraps_ru);

/// View panes at 1280, 1440 and 1920 points, every layout and one pane maximized: the panes
/// tile the view area, each header keeps its view list and its two buttons inside it, apart and
/// uncut, and every form row of the frame passes.
fn app_panes(lang: Lang) {
    use crate::ai::ui::tests_modes::{Shell, SIZES};
    use crate::view_panes::Layout;
    let mut fails: Vec<String> = vec![];
    for size in SIZES {
        let mut sh = Shell::new(lang, size);
        sh.app.ed.set_mode(crate::wall_tool::Mode::Polyline);
        crate::schedule::test_room(&mut sh.app.ed);
        sh.app.ed.set_mode(crate::wall_tool::Mode::Select);
        sh.app.ed.add_compass();
        assert!(sh.app.run("view.panes"));
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size.0, size.1));
        let cases = Layout::ALL.iter().map(|l| (*l, false)).chain([(Layout::Quad, true)]);
        for (l, max) in cases {
            sh.app.panes.set_layout(l);
            if max {
                sh.app.panes.active = 3;
                sh.app.panes.toggle_max();
            }
            let p = sh.settle();
            let tag = format!("{lang:?} {}x{} {l:?} max={max}", size.0, size.1);
            let drawn = sh.app.panes.drawn.clone();
            let want = if max { 1 } else { l.count() };
            if drawn.len() != want {
                fails.push(format!("{tag}: {} panes drawn of {want}", drawn.len()));
                continue;
            }
            for (k, (i, r, head)) in drawn.iter().enumerate() {
                if !window.contains_rect(*r) || r.width() < 120.0 || r.height() < 120.0 {
                    fails.push(format!("{tag}: pane {i} at {r:?}"));
                }
                for (_, q, _) in &drawn[k + 1..] {
                    if r.intersect(*q).area() > 0.0 {
                        fails.push(format!("{tag}: pane {i} overlaps {q:?}"));
                    }
                }
                let ins: Vec<&Control> = p
                    .controls
                    .iter()
                    .filter(|c| head.contains(c.rect.center()) && c.rect.width() < head.width())
                    .collect();
                if ins.len() < 3 {
                    fails.push(format!("{tag}: header {i} has {} controls", ins.len()));
                }
                for (m, c) in ins.iter().enumerate() {
                    if !head.expand(EPS).contains_rect(c.rect) {
                        fails.push(format!("{tag}: {:?} leaves header {i}", c.label));
                    }
                    if !c.label.is_empty() && p.elided.contains(&c.label) {
                        fails.push(format!("{tag}: {:?} is elided", c.label));
                    }
                    for o in &ins[m + 1..] {
                        if c.rect.shrink(EPS).intersects(o.rect.shrink(EPS)) {
                            fails.push(format!("{tag}: {:?} overlaps {:?}", c.label, o.label));
                        }
                    }
                }
            }
            let bad = row_faults(&p, false);
            if !bad.is_empty() {
                eprintln!("{}", dump(&tag, &p, &bad));
            }
            fails.extend(bad.into_iter().map(|b| format!("{tag}: {b}")));
            if max {
                sh.app.panes.toggle_max();
            }
        }
    }
    assert!(fails.is_empty(), "{} faults:\n{}", fails.len(), fails.join("\n"));
}

en_ru!(app_panes, app_panes_en, app_panes_ru);

/// Controls of the status bar, by the salt of their widget id.
const SB_SALTS: [&str; 12] = [
    "recent", "grid", "snap", "ortho", "level", "style", "perf", "pick", "warn", "msgs", "save",
    "more",
];
/// Controls present in every state of the bar.
const SB_ALWAYS: [&str; 8] = ["recent", "grid", "snap", "ortho", "style", "perf", "pick", "save"];
/// The controls that open a menu above the bar.
const SB_MENUS: [&str; 7] = ["more", "level", "style", "perf", "pick", "msgs", "save"];

fn sb_id(salt: &str) -> egui::Id {
    egui::Id::new(("ddc.sb", salt))
}

fn sb_rects(ctx: &egui::Context) -> Vec<(&'static str, egui::Rect)> {
    SB_SALTS
        .iter()
        .filter_map(|s| ctx.read_response(sb_id(s)).map(|r| (*s, r.rect)))
        .collect()
}

fn sb_bar(ctx: &egui::Context) -> egui::Rect {
    egui::containers::panel::PanelState::load(ctx, egui::Id::new("ddc.status"))
        .expect("status bar drawn")
        .rect
}

fn sb_click(at: egui::Pos2) -> [Vec<egui::Event>; 3] {
    let b = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    [vec![egui::Event::PointerMoved(at)], vec![b(true)], vec![b(false)]]
}

/// The status bar of the main window, every control clicked in turn: the bar keeps its height
/// and place, no control moves or leaves the bar, and a menu opens above the bar, inside the
/// window, clear of the bar; a second click closes it and nothing jumps.
fn status_bar_clicks(lang: Lang) {
    let h = crate::shell::status_bar::BAR_H;
    for screen in [(1280.0, 720.0), (1920.0, 1080.0), (900.0, 600.0), (480.0, 600.0), (320.0, 600.0)] {
        let mut app = app_in(lang);
        let (ctx, mut th) = driven_ctx();
        let win = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(screen.0, screen.1));
        let mut run = |app: &mut EditorApp, ev: Vec<egui::Event>| {
            pass_on(&ctx, &mut th, egui::Id::new("bt1.sb"), screen, ev, &mut |c| {
                app.ui(c, None)
            });
        };
        for _ in 0..FRAMES {
            run(&mut app, vec![]);
        }
        let bar = sb_bar(&ctx);
        let tag = format!("status bar {lang:?} {screen:?}");
        assert!((bar.height() - h).abs() <= EPS, "{tag}: height {} not {h}", bar.height());
        assert!((bar.bottom() - win.bottom()).abs() <= EPS, "{tag}: bar off the bottom {bar:?}");
        let rects = sb_rects(&ctx);
        // however narrow the window, every control stays reachable (labels fold to icons)
        // (a control that does not fit is a row of the "more" menu, checked below)
        let more = rects.iter().any(|x| x.0 == "more");
        if !more {
            for need in SB_ALWAYS {
                assert!(rects.iter().any(|x| x.0 == need), "{tag}: {need} missing in {rects:?}");
            }
        }
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert!(!a.1.intersects(b.1), "{tag}: {} overlaps {}", a.0, b.0);
            }
        }
        for (s, r) in &rects {
            assert!(bar.contains_rect(*r), "{tag}: {s} {r:?} leaves the bar {bar:?}");
            assert!(r.height() >= 20.0 - EPS, "{tag}: {s} target {r:?} too small");
        }
        for (salt, r) in rects.clone() {
            let rects = sb_rects(&ctx);
            let r = rects.iter().find(|x| x.0 == salt).map_or(r, |x| x.1);
            let menu = SB_MENUS.contains(&salt);
            let enabled = ctx.read_response(sb_id(salt)).is_some_and(|r| r.enabled());
            // the theme delays tooltips by half a second; frames advance by 1/60 s
            for _ in 0..45 {
                run(&mut app, vec![egui::Event::PointerMoved(r.center())]);
            }
            let tip = ctx.memory(|m| {
                m.layer_ids().any(|l| l.order == egui::Order::Tooltip)
            });
            assert!(tip, "{salt} {lang:?} {screen:?}: no tooltip on hover");
            run(&mut app, vec![egui::Event::PointerMoved(win.center_top())]);
            for ev in sb_click(r.center()) {
                run(&mut app, ev);
            }
            for _ in 0..2 {
                run(&mut app, vec![egui::Event::PointerMoved(r.center())]);
            }
            let tag = format!("{tag} {salt}");
            assert_eq!(sb_bar(&ctx), bar, "{tag}: the bar moved or changed height");
            let pid = sb_id(salt).with("popup");
            let open = egui::Popup::is_id_open(&ctx, pid);
            // what lies over the control when its click went nowhere
            let over: Vec<String> = ctx
                .memory(|m| m.layer_ids().collect::<Vec<_>>())
                .into_iter()
                .filter_map(|l| {
                    let a = egui::AreaState::load(&ctx, l.id)?;
                    a.rect().contains(r.center()).then(|| format!("{l:?} {:?}", a.rect()))
                })
                .collect();
            let at = ctx.layer_id_at(r.center());
            assert_eq!(open, menu && enabled, "{tag}: menu open state; top {at:?}, areas {over:?}");
            if open {
                let pr = egui::AreaState::load(&ctx, pid).expect("menu area").rect();
                assert!(win.contains_rect(pr), "{tag}: menu {pr:?} leaves the window");
                assert!(pr.bottom() <= bar.top() + EPS, "{tag}: menu {pr:?} over the bar");
            }
            // a chip that appears or goes (messages) reflows the right zone; the left may not move
            let same = sb_rects(&ctx).len() == rects.len();
            for (s, r0) in &rects {
                let now = ctx.read_response(sb_id(s)).map(|x| x.rect);
                if matches!(*s, "recent" | "grid" | "snap" | "ortho") || (open && same) {
                    assert_eq!(
                        now,
                        Some(*r0),
                        "{tag}: {s} jumped; before {rects:?}; after {:?}",
                        sb_rects(&ctx)
                    );
                }
            }
            if open {
                for ev in sb_click(r.center()) {
                    run(&mut app, ev);
                }
                run(&mut app, vec![]);
                assert!(!egui::Popup::is_id_open(&ctx, pid), "{tag}: second click keeps it");
                assert_eq!(sb_bar(&ctx), bar, "{tag}: the bar moved on close");
            }
        }
        if more {
            sb_overflow(&tag, &mut |ev| run(&mut app, ev), &ctx, bar, win);
        }
    }
}

/// Every control that left the bar is a row of the "more" menu: the row is inside the window,
/// clickable, and a click opens that control's own menu above the bar.
fn sb_overflow(
    tag: &str,
    run: &mut dyn FnMut(Vec<egui::Event>),
    ctx: &egui::Context,
    bar: egui::Rect,
    win: egui::Rect,
) {
    let on_bar: Vec<&str> = sb_rects(ctx).iter().map(|x| x.0).collect();
    let hidden: Vec<&str> = SB_SALTS.into_iter().filter(|s| !on_bar.contains(s)).collect();
    // the pill of a running task is not a control of the list; the rest must all be rows
    for need in SB_ALWAYS {
        assert!(on_bar.contains(&need) || hidden.contains(&need), "{tag}: {need} lost");
    }
    let more = ctx.read_response(sb_id("more")).expect("more button").rect;
    for salt in hidden.iter().copied().filter(|s| *s != "more") {
        let row_id = sb_id(salt).with("row");
        for ev in sb_click(more.center()) {
            run(ev);
        }
        run(vec![]);
        let Some(row) = ctx.read_response(row_id) else {
            // level, warnings and messages exist only in some states of the bar
            assert!(!SB_ALWAYS.contains(&salt), "{tag}: {salt} is not in the more menu");
            continue;
        };
        assert!(win.contains_rect(row.rect), "{tag}: row {salt} {:?} leaves the window", row.rect);
        assert!(row.rect.bottom() <= bar.top() + EPS, "{tag}: row {salt} over the bar");
        let enabled = row.enabled();
        for ev in sb_click(row.rect.center()) {
            run(ev);
        }
        for _ in 0..2 {
            run(vec![]);
        }
        let pid = sb_id(salt).with("popup");
        let opens = SB_MENUS.contains(&salt) && enabled;
        assert_eq!(egui::Popup::is_id_open(ctx, pid), opens, "{tag}: {salt} menu from the row");
        if opens {
            let area = egui::AreaState::load(ctx, pid).expect("menu area").rect();
            assert!(win.contains_rect(area), "{tag}: {salt} menu {area:?} leaves the window");
            assert!(area.bottom() <= bar.top() + EPS, "{tag}: {salt} menu over the bar");
        }
        run(vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        run(vec![]);
        assert!(!egui::Popup::is_id_open(ctx, pid), "{tag}: {salt} menu stays after Escape");
    }
}

en_ru!(status_bar_clicks, status_bar_clicks_en, status_bar_clicks_ru);
