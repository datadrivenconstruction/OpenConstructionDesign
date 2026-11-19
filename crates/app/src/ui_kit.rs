//! Thin UI layer: tokens of the light engineering theme and panel primitives.
//! Application code uses only this module, not egui styles directly.

use crate::expr_field::Scope;
use crate::units::{
    edit_length, parse_edit_length, parse_edit_length_lang, DecimalMark,
    InputError, Units,
};
use ddc_ui::widgets::field::NumberFormat;
use egui::{self, Color32, CornerRadius, RichText, Stroke};

pub mod autosave;
pub mod banner;
pub mod card;
pub mod dialog;
pub mod element;
pub mod size;
pub use autosave::{autosave_section, int_field, AutosaveOut};
pub use card::*;
pub use dialog::*;
pub use element::{element_header, figures, group_title};
pub use size::fmt_size;

#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub bg: Color32,
    pub panel: Color32,
    pub viewport: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub accent: Color32,
    pub border: Color32,
    pub radius: u8,
    pub spacing: f32,
    /// Plan drawing: paper, grid levels, cut lines, poche fill.
    pub paper: Color32,
    pub grid_minor: Color32,
    pub grid_major: Color32,
    pub cut: Color32,
    pub poche: Color32,
    /// Section/elevation drawing: projection and beyond lines, poche by material class
    /// (`MAT_CLASSES` order), line weights cut / projection / beyond / level.
    pub projected: Color32,
    pub beyond: Color32,
    pub mat: [Color32; 6],
    pub lw: [f32; 4],
}

/// Material classes of section poche, in `Tokens::mat` order.
pub const MAT_CLASSES: [&str; 6] = [
    "concrete",
    "masonry",
    "insulation",
    "glass",
    "wood",
    "other",
];

/// Canvas colours derived from the shared theme tokens.
pub fn from_theme(th: &ddc_ui::Theme) -> Tokens {
    Tokens {
        bg: th.c("bg.app"),
        panel: th.c("bg.panel"),
        viewport: th.c("bg.viewport"),
        text: th.c("text.primary"),
        text_dim: th.c("text.secondary"),
        accent: th.c("accent"),
        border: th.c("border.subtle"),
        radius: th
            .tokens
            .radius
            .get("md")
            .copied()
            .unwrap_or(ddc_ui::theme::radius::MD) as u8,
        spacing: th.tokens.space.unit,
        paper: th.c("canvas.paper"),
        grid_minor: th.c("canvas.grid.minor"),
        grid_major: th.c("canvas.grid.major"),
        cut: th.c("canvas.cut"),
        poche: th.c("canvas.poche"),
        projected: th.c("canvas.projected"),
        beyond: th.c("canvas.beyond"),
        mat: MAT_CLASSES.map(|m| th.c(&format!("canvas.mat.{m}"))),
        lw: ["cut", "projection", "beyond", "level"].map(|k| th.line_px(k)),
    }
}

/// Light tokens for code that runs before the shell theme exists.
pub fn light() -> Tokens {
    from_theme(&ddc_ui::Theme::new(
        ddc_ui::Mode::Light,
        ddc_ui::Density::Compact,
    ))
}

impl Tokens {
    /// Semantic palette of the same theme, for canvas colours outside this struct.
    pub fn sem(&self) -> &'static ddc_ui::theme::Sem {
        ddc_ui::theme::sem(self.paper.r() < 128)
    }
}

pub fn apply(ctx: &egui::Context, t: &Tokens) {
    let mut v = egui::Visuals::light();
    v.panel_fill = t.panel;
    v.window_fill = t.panel;
    v.extreme_bg_color = t.bg;
    v.override_text_color = Some(t.text);
    v.selection.bg_fill = t.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, t.accent);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, t.border);
    let r = CornerRadius::same(t.radius);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
    ] {
        w.corner_radius = r;
    }
    ctx.set_visuals(v);
    ctx.style_mut(|s| s.spacing.item_spacing = egui::vec2(t.spacing, t.spacing * 0.7));
}

pub fn top_bar(ctx: &egui::Context, t: &Tokens, add: impl FnOnce(&mut egui::Ui)) {
    egui::TopBottomPanel::top("ddc-top")
        .frame(egui::Frame::new().fill(t.panel).inner_margin(t.spacing))
        .show(ctx, |ui| ui.horizontal(add));
}

/// Properties: no narrower than `PROPS_MIN_W` (labels and numbers do not wrap); the user's width
/// is stored by the egui panel and in settings (`Prefs::props_width`).
pub const PROPS_MIN_W: f32 = 280.0;
pub const PROPS_W: f32 = 300.0;

pub fn side_panel(
    ctx: &egui::Context,
    t: &Tokens,
    width: f32,
    add: impl FnOnce(&mut egui::Ui),
) -> f32 {
    use crate::shell::fold::{dock_max, PANEL_MAX_W};
    let max = dock_max(PANEL_MAX_W, PROPS_MIN_W, ctx.screen_rect().width());
    egui::SidePanel::right("ddc-props")
        .resizable(true)
        .min_width(PROPS_MIN_W)
        .max_width(max)
        .default_width(width.clamp(PROPS_MIN_W, max))
        .frame(
            egui::Frame::new()
                .fill(t.panel)
                .inner_margin(egui::Margin::symmetric(12, 10)),
        )
        .show(ctx, add)
        .response
        .rect
        .width()
}

pub fn viewport_panel(
    ctx: &egui::Context,
    t: &Tokens,
    add: impl FnOnce(&mut egui::Ui),
) -> egui::Rect {
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(t.viewport))
        .show(ctx, add)
        .response
        .rect
}

/// Collapsible section header: chevron + small-caps caption; click toggles, state persists for
/// the session. Returns whether the section is open.
pub fn heading(ui: &mut egui::Ui, text: &str) -> bool {
    heading_keyed(ui, text, text, None)
}

/// Memory id of a section's open state: `heading` uses its text, the properties panel a key
/// that includes the element category.
pub fn section_id(state_key: &str) -> egui::Id {
    egui::Id::new(("ddc.section", state_key))
}

/// [`heading`] with its own state key and an optional row count shown after the caption.
pub fn heading_keyed(ui: &mut egui::Ui, state_key: &str, text: &str, count: Option<usize>) -> bool {
    let text = &match count {
        Some(n) => format!("{text}  {n}"),
        None => text.to_string(),
    };
    ui.add_space(ddc_ui::theme::space::SM);
    let id = section_id(state_key);
    let mut open = ui.data_mut(|d| *d.get_persisted_mut_or(id, true));
    let f = ddc_ui::typo::font(ddc_ui::theme::Role::Section);
    let (r, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), egui::Sense::click());
    let col = if resp.hovered() {
        ui.visuals().widgets.hovered.fg_stroke.color
    } else {
        weak(ui)
    };
    let t = ui
        .ctx()
        .animate_bool_with_time(id, open, ui.style().animation_time);
    let c = egui::pos2(r.left() + 5.0, r.center().y);
    // chevron rotates from ▸ (closed) to ▾ (open)
    let a = (1.0 - t) * -std::f32::consts::FRAC_PI_2;
    let rot = |x: f32, y: f32| c + egui::vec2(x * a.cos() - y * a.sin(), x * a.sin() + y * a.cos());
    ui.painter().add(egui::Shape::line(
        vec![rot(-3.5, -1.75), rot(0.0, 1.75), rot(3.5, -1.75)],
        Stroke::new(1.25, col),
    ));
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &text.to_uppercase(),
        0.0,
        egui::TextFormat {
            font_id: f,
            color: col,
            extra_letter_spacing: 0.4,
            ..Default::default()
        },
    );
    let g = ui.fonts(|fo| fo.layout_job(job));
    // the letters' middle on the row's middle, level with the chevron: centring the line box
    // of the small section font sets the caps 1.5 px off
    ui.painter().galley(
        egui::pos2(r.left() + 14.0, r.center().y - x_mid(&g)),
        g,
        col,
    );
    if resp.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_persisted(id, open));
    }
    ui.add_space(2.0);
    open
}

/// Secondary text colour of the current style.
pub fn weak(ui: &egui::Ui) -> Color32 {
    ui.visuals().widgets.inactive.fg_stroke.color
}

/// Which item gap `no_gap` removes: between rows (`Rows`) or between cells of a row (`Cells`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gap {
    Rows,
    Cells,
}

fn zero_gap(sp: &mut egui::style::Spacing, gap: Gap) {
    let g = &mut sp.item_spacing;
    match gap {
        Gap::Rows => g.y = 0.0,
        Gap::Cells => g.x = 0.0,
    }
}

/// Removes one item gap of `ui` (and its children) for virtual lists and fixed cells that
/// place their own padding.
pub fn no_gap(ui: &mut egui::Ui, gap: Gap) {
    zero_gap(ui.spacing_mut(), gap);
}

/// Fixed column widths shared by every property grid of the panel, so units line up
/// across sections. The key column takes what the value and unit columns leave.
const VAL_W: f32 = ddc_ui::theme::layout::PROP_VALUE;
const UNIT_W: f32 = ddc_ui::theme::layout::PROP_UNIT;
const GAP: f32 = ddc_ui::theme::layout::PROP_GAP;

/// Height of a Properties row: the kit form row, so every cell of a row (label, field, unit)
/// shares one centre line.
pub fn row_h(ui: &egui::Ui) -> f32 {
    ddc_ui::widgets::form::row_h(&ddc_ui::theme::of(ui.ctx()))
}

fn key_w(ui: &egui::Ui) -> f32 {
    ui.data(|d| d.get_temp::<f32>(egui::Id::new("ddc.props.key_w")))
        .unwrap_or(128.0)
}

/// A grid cell exactly one row high: its content is centred on the row's centre line whatever
/// height the content asks for.
fn cell<R>(
    ui: &mut egui::Ui,
    w: f32,
    layout: egui::Layout,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let h = row_h(ui);
    ui.allocate_ui_with_layout(egui::vec2(w, h), layout, |ui| {
        ui.set_min_width(w);
        ui.set_max_width(w);
        ui.set_min_height(h);
        ui.set_max_height(h);
        add(ui)
    })
    .inner
}

/// Key column width for a grid that gets `avail` points: the value and unit columns keep
/// their fixed widths, the label takes the rest.
fn key_col_w(avail: f32) -> f32 {
    (avail - VAL_W - UNIT_W - 2.0 * GAP).clamp(
        ddc_ui::theme::layout::PROP_KEY_MIN,
        ddc_ui::theme::layout::PROP_KEY_MAX,
    )
}

/// Label on one line in the key column; text wider than `w` ends with an ellipsis
/// (`Galley::elided`).
fn key_galley(ui: &egui::Ui, k: &str, w: f32, c: Color32) -> std::sync::Arc<egui::Galley> {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut job = egui::text::LayoutJob::single_section(k.to_owned(), egui::TextFormat::simple(font, c));
    job.wrap = egui::text::TextWrapping::truncate_at_width(w);
    ui.fonts(|f| f.layout_job(job))
}

/// Single-line width of `k` in the key font; the widest key of the language decides whether
/// the shared key column has to elide.
#[cfg(test)]
fn key_natural_w(ui: &egui::Ui, k: &str) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    ui.fonts(|f| f.layout_no_wrap(k.to_owned(), font, Color32::PLACEHOLDER).size().x)
}

/// Test hook: (key text, rect, laid-out rows, elided) of every key cell drawn since the log
/// was cleared.
#[cfg(test)]
pub(crate) type KeyRow = (String, egui::Rect, usize, bool);

#[cfg(test)]
pub(crate) fn key_log_id() -> egui::Id {
    egui::Id::new("ddc.kv.keys")
}

/// Label cell: one line, never wraps; an elided label shows the full text as tooltip.
fn key(ui: &mut egui::Ui, k: &str) -> egui::Response {
    key_spill(ui, k, 0.0)
}

/// [`key`] whose text may run `spill` points into the value cell that its value leaves empty;
/// the cell keeps the shared column width.
fn key_spill(ui: &mut egui::Ui, k: &str, spill: f32) -> egui::Response {
    let c = weak(ui);
    let w = key_w(ui);
    cell(
        ui,
        w,
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            let g = key_galley(ui, k, w + spill.max(0.0), c);
            let elided = g.elided;
            #[cfg(test)]
            let rows = g.rows.len();
            let size = egui::vec2(g.size().x.min(w), g.size().y);
            let (mut r, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
            r.max.x = r.min.x + g.size().x;
            #[cfg(test)]
            ui.ctx().data_mut(|d| {
                d.get_temp_mut_or_default::<Vec<KeyRow>>(key_log_id()).push((k.to_owned(), r, rows, elided))
            });
            ui.painter().galley(r.min, g, c);
            if elided {
                resp.on_hover_text(k)
            } else {
                resp
            }
        },
    )
}

/// Top-left corner of the canvas option bar in `row` (0 = first slot, 1 = the row below it):
/// every canvas bar takes its row here so two bars never overlap.
pub fn canvas_bar_pos(rect: egui::Rect, row: u8) -> egui::Pos2 {
    use ddc_ui::theme::space;
    rect.left_top() + egui::vec2(space::MD, space::MD + f32::from(row) * space::XXXL)
}

pub fn dim(ui: &mut egui::Ui, t: &Tokens, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).color(t.text_dim));
}

/// Distance from a galley's top to the middle of its lower-case letters (baseline less 3/8 of
/// the ascent): where the eye reads the line, unlike the box egui centres.
fn x_mid(g: &egui::Galley) -> f32 {
    let Some(row) = g.rows.first() else { return g.size().y / 2.0 };
    match row.row.glyphs.iter().find(|x| !x.chr.is_whitespace()) {
        Some(gl) => row.pos.y + gl.pos.y - 0.375 * gl.font_ascent,
        None => row.pos.y + row.row.size.y / 2.0,
    }
}

/// A caption whose font differs from the row's controls (`line` is their text font), placed in
/// a centred row so both read on one line: boxes of a larger or bolder font centre lower.
pub fn caption_on_line(ui: &mut egui::Ui, text: egui::RichText, line: &egui::FontId) -> egui::Response {
    let plain = text.text().to_owned();
    let wrap = Some(egui::TextWrapMode::Extend);
    let g = egui::WidgetText::from(text).into_galley(ui, wrap, f32::INFINITY, egui::TextStyle::Body);
    let probe = ui.painter().layout_no_wrap("x".into(), line.clone(), egui::Color32::PLACEHOLDER);
    let dy = (x_mid(&probe) - probe.size().y / 2.0) - (x_mid(&g) - g.size().y / 2.0);
    let (rect, resp) = ui.allocate_exact_size(g.size(), egui::Sense::hover());
    let col = ui.visuals().text_color();
    ui.painter().galley(rect.min + egui::vec2(0.0, dy.round()), g, col);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &plain));
    resp
}

/// A person's own name (a filter, a view) drawn by `resp` in room it may not fit: an ellipsis
/// is allowed there because the tooltip `tip` carries it whole. Interface captions never take
/// this path; the layout contract reads the mark and still flags every other cut caption.
pub fn user_text_tip(
    resp: egui::Response,
    text: &str,
    tip: impl Into<egui::WidgetText>,
) -> egui::Response {
    mark_user_text(&resp.ctx, text);
    resp.on_hover_text(tip)
}

/// The mark alone, for a widget that already shows the whole text on hover when it cuts it.
pub fn mark_user_text(ctx: &egui::Context, text: &str) {
    #[cfg(test)]
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Vec<String>>(user_text_id()).push(text.to_owned())
    });
    #[cfg(not(test))]
    let _ = (ctx, text);
}

/// Where the user texts of the pass are kept for the layout contract.
#[cfg(test)]
pub(crate) fn user_text_id() -> egui::Id {
    egui::Id::new("ddc.kit.user_text")
}

/// The viewport of a kit scroll area this pass: the layout contract lets rows end on its edge
/// (and hide past it) only inside such a viewport, never inside any other nested clip.
pub fn mark_scroll_viewport(ctx: &egui::Context, viewport: egui::Rect) {
    #[cfg(test)]
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Vec<egui::Rect>>(scroll_viewport_id()).push(viewport)
    });
    #[cfg(not(test))]
    let _ = (ctx, viewport);
}

/// Where the scroll viewports of the pass are kept for the layout contract.
#[cfg(test)]
pub(crate) fn scroll_viewport_id() -> egui::Id {
    egui::Id::new("ddc.kit.scroll_viewport")
}

/// Empty panel state from `empty.<what>.{title,hint,action}` via the shared widget; true when
/// the action button was clicked.
pub fn empty_state(ui: &mut egui::Ui, what: &str) -> bool {
    empty_state_action(ui, what, &format!("empty.{what}.action"))
}

/// Same as `empty_state` with the action label taken from `action_key` (state-dependent action).
pub fn empty_state_action(ui: &mut egui::Ui, what: &str, action_key: &str) -> bool {
    let th = ddc_ui::theme::of(ui.ctx());
    let action = crate::tr(action_key, &[]);
    ddc_ui::widgets::empty_state(
        ui,
        &th,
        None,
        &crate::tr(&format!("empty.{what}.title"), &[]),
        &crate::tr(&format!("empty.{what}.hint"), &[]),
        Some(&action),
    )
}

/// Designed empty state of `empty_kind::Kind` in an explicit language; `with_action` adds the
/// one-click way out `empty.<what>.action`. True when that action was clicked.
pub fn empty_state_lang(ui: &mut egui::Ui, ru: bool, what: &str, with_action: bool) -> bool {
    let th = ddc_ui::theme::of(ui.ctx());
    let action = with_action.then(|| crate::tr_lang(ru, &format!("empty.{what}.action"), &[]));
    ddc_ui::widgets::empty_state(
        ui,
        &th,
        None,
        &crate::tr_lang(ru, &format!("empty.{what}.title"), &[]),
        &crate::tr_lang(ru, &format!("empty.{what}.hint"), &[]),
        action.as_deref(),
    )
}

/// Compact empty state (no action) for a panel with its form directly below.
pub fn empty_state_compact_lang(ui: &mut egui::Ui, ru: bool, what: &str) {
    let th = ddc_ui::theme::of(ui.ctx());
    ddc_ui::widgets::empty_state_compact(
        ui,
        &th,
        &crate::tr_lang(ru, &format!("empty.{what}.title"), &[]),
        &crate::tr_lang(ru, &format!("empty.{what}.hint"), &[]),
    );
}

const HINT_ID: &str = "ddc.props.hint";

/// Keyboard hint of the current selection: shown once as the panel footer, not body text.
pub fn hint(ui: &mut egui::Ui, text: impl Into<String>) {
    let s: String = text.into();
    ui.data_mut(|d| d.insert_temp(egui::Id::new(HINT_ID), s));
}

/// Below this panel height the keyboard hint footer is left out: rows need the room.
pub const HINT_MIN_PANEL_H: f32 = 480.0;

fn hint_pinned_id() -> egui::Id {
    egui::Id::new(HINT_ID).with("pinned")
}

fn hint_last_id() -> egui::Id {
    egui::Id::new(HINT_ID).with("last")
}

/// Height of the pinned footer strip: hairline, gap and one caption line.
pub fn hint_strip_h() -> f32 {
    let cap = ddc_ui::typo::font(ddc_ui::theme::Role::Caption).size;
    (cap * 1.3).ceil() + 2.0 * ddc_ui::theme::space::XS + 1.0
}

/// A panel whose body scrolls above a pinned keyboard-hint strip: the scroll area ends above
/// the strip, so the hint never lies over a row. The strip is one elided line, left out when
/// the panel is lower than `HINT_MIN_PANEL_H`. `body` calls `hint` as before.
pub fn with_hint_strip(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui)) {
    let full = ui.available_rect_before_wrap();
    let room = full.height() >= HINT_MIN_PANEL_H;
    let had = ui.data(|d| d.get_temp::<bool>(hint_last_id())).unwrap_or(false);
    let strip = if room && had { hint_strip_h() } else { 0.0 };
    let foot = egui::pos2(full.right(), full.bottom() - strip);
    let body_rect = egui::Rect::from_min_max(full.min, foot);
    ui.data_mut(|d| {
        d.insert_temp(hint_pinned_id(), true);
        d.remove_temp::<String>(egui::Id::new(HINT_ID));
    });
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body_rect));
    child.set_clip_rect(body_rect.intersect(ui.clip_rect()));
    body(&mut child);
    let text = ui.data_mut(|d| {
        d.remove_temp::<bool>(hint_pinned_id());
        d.remove_temp::<String>(egui::Id::new(HINT_ID))
    });
    let has = room && text.is_some();
    if has != had {
        ui.data_mut(|d| d.insert_temp(hint_last_id(), has));
        ui.ctx().request_repaint();
    }
    if let (Some(s), true) = (text, strip > 0.0) {
        let r = egui::Rect::from_min_max(egui::pos2(full.left(), full.bottom() - strip), full.max);
        let px = 1.0 / ui.pixels_per_point();
        let line = ui.visuals().widgets.noninteractive.bg_stroke.color;
        ui.painter().hline(r.x_range(), r.top() + px / 2.0, Stroke::new(px, line));
        let inner = r.shrink2(egui::vec2(0.0, ddc_ui::theme::space::XS));
        let mut s_ui = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        let th = ddc_ui::theme::of(ui.ctx());
        ddc_ui::widgets::elided_label(
            &mut s_ui,
            &s,
            ddc_ui::typo::font(ddc_ui::theme::Role::Caption),
            th.sem().text_secondary,
        );
    }
    ui.advance_cursor_after_rect(full);
}

/// Panel footer: hairline and one caption line pinned to the bottom (elided if long). Inside
/// `with_hint_strip` the strip draws it instead.
pub fn hint_footer(ui: &mut egui::Ui) {
    if ui.data(|d| d.get_temp::<bool>(hint_pinned_id())).unwrap_or(false) {
        return;
    }
    let Some(s) = ui.data_mut(|d| d.remove_temp::<String>(egui::Id::new(HINT_ID))) else {
        return;
    };
    let c = weak(ui);
    // inside a scroll area there is no bottom to anchor to: the hint just follows the rows
    if !ui.max_rect().bottom().is_finite() || ui.max_rect().bottom() > 1e6 {
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(s)
                    .color(c)
                    .font(ddc_ui::typo::font(ddc_ui::theme::Role::Caption)),
            )
            .wrap(),
        );
        return;
    }
    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
        ui.add(
            egui::Label::new(
                RichText::new(s)
                    .color(c)
                    .font(ddc_ui::typo::font(ddc_ui::theme::Role::Caption)),
            )
            .wrap(),
        );
        ui.add_space(4.0);
        let r = ui.available_rect_before_wrap();
        let y = (r.bottom() * ui.pixels_per_point()).round() / ui.pixels_per_point();
        ui.painter().hline(
            r.x_range(),
            y - 0.5 / ui.pixels_per_point(),
            Stroke::new(
                1.0 / ui.pixels_per_point(),
                ui.visuals().widgets.noninteractive.bg_stroke.color,
            ),
        );
    });
}

/// "Key — value — unit" table; rows are added via `kv`/`kv_num`/`kv_edit`.
pub fn props(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    props_at(ui, id, shared_key_w(ui), add);
}

/// Key column width of the panel at the current width; read once per panel and handed to
/// every group, so all value columns start at the same x.
pub fn shared_key_w(ui: &egui::Ui) -> f32 {
    key_col_w(ui.available_width())
}

/// [`props`] with the key column width given by the caller.
pub fn props_at(ui: &mut egui::Ui, id: &str, kw: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.data_mut(|d| d.insert_temp(egui::Id::new("ddc.props.key_w"), kw));
    egui::Grid::new(id)
        .num_columns(3)
        .striped(false)
        .min_col_width(0.0)
        .min_row_height(row_h(ui))
        // rows carry their own air (`form::row_h`): fields of adjacent rows keep 4 points apart
        .spacing([GAP, 0.0])
        .show(ui, add);
}

/// Values use the same font as labels (its digits have equal width — verified by a test).
fn value_text(v: impl Into<String>) -> RichText {
    RichText::new(v.into()).text_style(egui::TextStyle::Body)
}

/// The one value/unit splitter ("12.5 m" into number and unit). Only the last space may split,
/// only before a known unit, and only after a plain number: "3 000 mm" keeps its digit groups,
/// "12 ft 6 in" and "19'-8 1/4\"" stay whole.
pub fn split_unit(s: &str) -> (&str, &str) {
    const GAP: [char; 3] = [' ', '\u{a0}', '\u{202f}'];
    let t = s.trim_end_matches(GAP);
    let Some((n, u)) = t.rsplit_once(GAP) else {
        return (s, "");
    };
    let n = n.trim_end_matches(GAP);
    let plain = n.chars().any(|c| c.is_ascii_digit())
        && n.chars().all(|c| c.is_ascii_digit() || GAP.contains(&c) || ".,+-\u{2212}".contains(c));
    if plain && is_unit_token(u) {
        (n, u)
    } else {
        (s, "")
    }
}

/// Unit words drawn apart from the number (en and ru, with powers).
pub(crate) fn is_unit_token(u: &str) -> bool {
    const UNITS: [&str; 33] = [
        "mm", "cm", "dm", "m", "km", "ft", "in", "yd", "ft-in", "%", "\u{b0}", "kg", "t",
        "\u{43c}\u{43c}", "\u{441}\u{43c}", "\u{434}\u{43c}", "\u{43c}", "\u{43a}\u{43c}",
        "\u{43a}\u{433}", "\u{442}", "kN", "kW", "W", "Pa", "kPa", "MPa", "L", "h", "pcs",
        "\u{b0}C", "\u{b0}F", "\u{43b}", "\u{448}\u{442}",
    ];
    let base = u.trim_end_matches(['\u{b2}', '\u{b3}', '2', '3']);
    UNITS.contains(&u) || UNITS.contains(&base)
}

fn unit_cell(ui: &mut egui::Ui, unit: &str) {
    let c = weak(ui);
    cell(
        ui,
        UNIT_W,
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(RichText::new(unit).color(c));
        },
    );
}

/// Text value: right-aligned across the value and unit columns (long names do not widen
/// the grid).
pub fn kv(ui: &mut egui::Ui, k: &str, v: impl Into<String>) {
    key(ui, k);
    let h = row_h(ui);
    let (r, _) = ui.allocate_exact_size(egui::vec2(VAL_W, h), egui::Sense::hover());
    let font = egui::TextStyle::Body.resolve(ui.style());
    let col = ui.visuals().text_color();
    let max = VAL_W + GAP + UNIT_W + key_w(ui) * 0.5;
    let mut job =
        egui::text::LayoutJob::single_section(v.into(), egui::TextFormat::simple(font, col));
    job.wrap = egui::text::TextWrapping::truncate_at_width(max);
    let g = ui.fonts(|f| f.layout_job(job));
    let right = r.right() + GAP + UNIT_W;
    ui.painter().galley(
        egui::pos2(right - g.size().x, r.center().y - g.size().y / 2.0),
        g,
        col,
    );
    ui.label("");
    ui.end_row();
}

/// Value widget (combo box) spanning the value and unit columns; laid out in a detached child
/// so the grid keeps its shared column widths.
pub fn kv_widget<R>(ui: &mut egui::Ui, k: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    kv_widget_w(ui, k, WIDGET_W, |ui, _| add(ui))
}

/// As `kv_widget` for a value that wants `want` points: a wider one grows left into the free
/// part of its label cell (never over the label), so a long name is not cut; `add` gets the
/// width it may use.
pub fn kv_widget_w<R>(
    ui: &mut egui::Ui,
    k: &str,
    want: f32,
    add: impl FnOnce(&mut egui::Ui, f32) -> R,
) -> R {
    let label = key(ui, k).rect;
    let h = row_h(ui);
    let (r, _) = ui.allocate_exact_size(egui::vec2(VAL_W, h), egui::Sense::hover());
    let room = (r.min.x - label.right() - GAP).max(0.0);
    let w = want.clamp(WIDGET_W, WIDGET_W + room);
    let span = egui::Rect::from_min_size(
        egui::pos2(r.min.x + WIDGET_W - w, r.min.y),
        egui::vec2(w, h),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(span)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    // a bare combo box fills the column too, like the kit dropdowns given WIDGET_W
    child.spacing_mut().combo_width = w;
    let out = add(&mut child, w);
    #[cfg(test)]
    probe(ui, k, child.min_rect());
    ui.label("");
    ui.end_row();
    out
}

/// Test hook: where the value widget of row `k` ended up.
#[cfg(test)]
fn probe(ui: &egui::Ui, k: &str, r: egui::Rect) {
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("ddc.kv.probe", k.to_string())), r));
}

#[cfg(test)]
pub(crate) fn probed(ctx: &egui::Context, k: &str) -> Option<egui::Rect> {
    ctx.data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("ddc.kv.probe", k.to_string()))))
}

/// Value column width for widgets placed with `kv_widget`.
pub const WIDGET_W: f32 = VAL_W + GAP + UNIT_W;

/// Numeric row: value right-aligned, unit in a separate column.
pub fn kv_num(ui: &mut egui::Ui, k: &str, v: impl Into<String>) {
    let v = v.into();
    let (n, u) = split_unit(&v);
    // a short number leaves most of its right-aligned cell empty: a long label may use it
    let font = egui::TextStyle::Body.resolve(ui.style());
    let col = ui.visuals().text_color();
    let num_w = ui.fonts(|f| f.layout_no_wrap(n.to_owned(), font, col).size().x);
    key_spill(ui, k, VAL_W - num_w - GAP);
    cell(
        ui,
        VAL_W,
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            ui.label(value_text(n));
        },
    );
    unit_cell(ui, u);
    ui.end_row();
}

/// What a text field does when it loses focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldEnd {
    Keep,
    Apply,
    Revert,
}

/// Focus-loss rule of property fields: Esc cancels, Enter always applies, Tab or a click
/// elsewhere applies only a changed text. Nothing happens while the field keeps focus.
pub fn field_end(lost_focus: bool, enter: bool, esc: bool, changed: bool) -> FieldEnd {
    if !lost_focus {
        FieldEnd::Keep
    } else if esc {
        FieldEnd::Revert
    } else if enter || changed {
        FieldEnd::Apply
    } else {
        FieldEnd::Keep
    }
}

/// True for non-empty text that is not a length (the field shows the reason).
pub fn length_text_invalid(s: &str, u: Units) -> bool {
    !s.trim().is_empty() && parse_edit_length(s, u).is_none()
}

/// Remembers the text at focus gain, so Esc can restore it and blur can tell a change.
pub fn track_field(ui: &egui::Ui, r: &egui::Response, buf: &mut String) -> FieldEnd {
    let id = r.id.with("field_orig");
    if r.gained_focus() {
        ui.data_mut(|d| d.insert_temp(id, buf.clone()));
    }
    if !r.lost_focus() {
        return FieldEnd::Keep;
    }
    let orig: Option<String> = ui.data(|d| d.get_temp(id));
    let (enter, esc) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Escape),
        )
    });
    let changed = orig.as_ref().is_some_and(|o| o != buf);
    let end = field_end(true, enter, esc, changed);
    if end == FieldEnd::Revert {
        if let Some(o) = orig {
            *buf = o;
        }
    }
    end
}

/// Editable field: Enter, Tab and a click elsewhere apply (returns the text), Esc reverts.
pub fn kv_edit(
    ui: &mut egui::Ui,
    k: &str,
    buf: &mut String,
    unit: &str,
    tip: &str,
) -> Option<String> {
    let kr = key(ui, k);
    let th = ddc_ui::theme::of(ui.ctx());
    let r = cell(
        ui,
        VAL_W,
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            let r = ddc_ui::widgets::TextField::new(buf)
                .labelled_by(kr.id, k)
                .right_aligned()
                .width(VAL_W)
                .show(ui, &th)
                .response
                .on_hover_text(tip);
            // the field frame, not the inset text node, is the row's value edge
            #[cfg(test)]
            probe(ui, k, ui.min_rect());
            r
        },
    );
    unit_cell(ui, unit);
    ui.end_row();
    (track_field(ui, &r, buf) == FieldEnd::Apply).then(|| buf.clone())
}

/// Text field row with a placeholder (`<varies>` of a multi-selection); the caller reads Enter
/// and focus from the response, so an unfocused field can show the stored value again.
pub fn kv_text(
    ui: &mut egui::Ui,
    k: &str,
    buf: &mut String,
    unit: &str,
    tip: &str,
    hint: &str,
    align: egui::Align,
) -> egui::Response {
    let kr = key(ui, k);
    let th = ddc_ui::theme::of(ui.ctx());
    let r = cell(
        ui,
        VAL_W,
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            let mut f = ddc_ui::widgets::TextField::new(buf)
                .labelled_by(kr.id, k)
                .hint(hint)
                .width(VAL_W);
            if align == egui::Align::Max {
                f = f.right_aligned();
            }
            let r = f.show(ui, &th).response.on_hover_text(tip);
            #[cfg(test)]
            probe(ui, k, ui.min_rect());
            r
        },
    );
    unit_cell(ui, unit);
    ui.end_row();
    r
}

/// Length field: type + Enter, ▴/▾ steppers (10 mm, Shift ×10) or drag the label to scrub
/// (1 mm per point). Returns the new value in metres when a change is committed.
pub fn kv_length(ui: &mut egui::Ui, k: &str, buf: &mut String, u: Units, tip: &str) -> Option<f64> {
    kv_length_hint(ui, k, buf, u, tip, "")
}

/// `kv_length` with a hint in the empty field (e.g. "<various>" for multi-selection).
pub fn kv_length_hint(
    ui: &mut egui::Ui,
    k: &str,
    buf: &mut String,
    u: Units,
    tip: &str,
    hint: &str,
) -> Option<f64> {
    let label = key(ui, k).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    let mut out = None;
    let cur = parse_edit_length(buf, u);
    let fast = ui.input(|i| i.modifiers.shift);
    let step = if fast { 0.1 } else { 0.01 };
    let tip = if length_text_invalid(buf, u) {
        crate::tr("units.err.parse", &[])
    } else {
        tip.to_string()
    };
    let th = ddc_ui::theme::of(ui.ctx());
    let label_id = label.id;
    let bad = length_text_invalid(buf, u);
    let r = cell(
        ui,
        VAL_W,
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            // the stepper lives inside the field, so stepped and plain fields end at one x
            let f = ddc_ui::widgets::TextField::new(buf)
                .labelled_by(label_id, k)
                .right_aligned()
                .stepper(true)
                .error(bad)
                .hint(hint)
                .width(VAL_W)
                .show(ui, &th);
            if f.step != 0 {
                if let Some(v) = cur {
                    let nv = (v + f64::from(f.step) * step).max(0.0);
                    *buf = edit_length(nv, u);
                    out = Some(nv);
                }
            }
            #[cfg(test)]
            probe(ui, k, ui.min_rect());
            f.response.on_hover_text(tip)
        },
    );
    unit_cell(ui, crate::units::unit_label(u));
    ui.end_row();
    if label.dragged() {
        if let Some(v) = cur {
            let dx = label.drag_delta().x as f64;
            *buf = edit_length((v + dx * step * 0.1).max(0.0), u);
        }
    }
    if label.drag_stopped() {
        out = parse_edit_length(buf, u);
    }
    if track_field(ui, &r, buf) == FieldEnd::Apply {
        out = parse_edit_length(buf, u);
    }
    out
}

/// Length field in current units: same format and parsing as in properties and status.
pub fn length_drag(v: &mut f64, u: Units, max: f64) -> egui::DragValue<'_> {
    egui::DragValue::new(v)
        .speed(0.01)
        .range(0.0..=max)
        .custom_formatter(move |x, _| edit_length(x, u))
        .custom_parser(move |s| parse_edit_length(s, u))
}

/// What a Properties number row holds: a length in project units (metres in the model) or a
/// plain number (degrees, percent, counts, site coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NumKind {
    Len(Units),
    Plain { decimals: usize, step: f64 },
}

impl NumKind {
    pub const DEG: Self = Self::Plain { decimals: 2, step: 1.0 };
    pub const PCT: Self = Self::Plain { decimals: 2, step: 0.1 };
    pub const COUNT: Self = Self::Plain { decimals: 0, step: 1.0 };
}

/// Field text of a length in the locale of `mark`, at the project display precision
/// (whole mm by default, the fraction denominator in imperial). Display only: see `len_step`.
pub fn len_text(m: f64, u: Units, mark: DecimalMark) -> String {
    len_text_prec(m, u, mark, crate::units::length_decimals(), crate::units::frac_denominator())
}

/// [`len_text`] at an explicit precision (`dec` mm decimals, 1/`den` inch).
pub fn len_text_prec(m: f64, u: Units, mark: DecimalMark, dec: usize, den: usize) -> String {
    crate::units::shown_length(m, u, mark, dec, den)
}

/// What a length field does with its text on commit.
#[derive(Clone, Debug, PartialEq)]
pub enum LenStep {
    /// Text equals what the field showed: nothing is written (no silent rounding).
    Keep,
    /// A typed value that differs from the stored one.
    Set(f64),
    Refuse(InputError),
}

/// Decides a commit from the typed `text`, the `shown` text of the stored value `v`.
pub fn len_step(text: &str, shown: &str, v: f64, u: Units, lo: f64, hi: f64, mark: DecimalMark) -> LenStep {
    if text.trim() == shown {
        return LenStep::Keep;
    }
    match len_commit(text, u, lo, hi, mark) {
        Ok(n) if n == v => LenStep::Keep,
        Ok(n) => LenStep::Set(n),
        Err(e) => LenStep::Refuse(e),
    }
}

/// Typed length to metres, clamped to `lo..=hi`; refused text gives the reason and no value.
/// A leading minus is read only where the range allows negatives.
pub fn len_commit(text: &str, u: Units, lo: f64, hi: f64, mark: DecimalMark) -> Result<f64, InputError> {
    let sc = Scope::none();
    let v = match parse_edit_length_lang(text, u, &sc, mark) {
        Ok(v) => v,
        // a range that includes zero and below: the positive-only parser refused a zero or a
        // negative value, which are valid here
        Err(InputError::NonPositive) if lo < 0.0 => {
            crate::units::parse_typed_signed(text, u, mark)?
        }
        Err(e) if lo < 0.0 => match text.trim().strip_prefix(['-', '\u{2212}']) {
            Some(rest) => parse_edit_length_lang(rest.trim(), u, &sc, mark)
                .map(|v| -v)
                .map_err(|_| e)?,
            None => return Err(e),
        },
        Err(e) => return Err(e),
    };
    Ok(v.clamp(lo, hi))
}

/// Length field of a Properties row. Enter, Tab or a click away commits what parses; text that
/// does not parse keeps the old value and shows the field error (Enter keeps the field open).
/// Esc restores. An untouched field commits nothing, so a sub-mm value is not rounded.
#[allow(clippy::too_many_arguments)]
pub fn len_field(ui: &mut egui::Ui, th: &ddc_ui::Theme, id: egui::Id, v: &mut f64, u: Units, lo: f64, hi: f64, ru: bool, label_id: egui::Id, k: &str) -> bool {
    let mark = DecimalMark::for_ru(ru);
    let buf_id = id.with("buf");
    let stored = ui.data(|d| d.get_temp::<String>(buf_id));
    let shown = len_text(*v, u, mark);
    let mut text = stored.clone().unwrap_or_else(|| shown.clone());
    let bad = stored.as_deref().and_then(|s| len_commit(s, u, lo, hi, mark).err());
    let reason = bad.as_ref().map(|e| e.text(ru)).unwrap_or_default();
    let out = ddc_ui::widgets::field::TextField::new(&mut text)
        .id_salt(id)
        .labelled_by(label_id, k)
        .error(bad.is_some())
        .error_text(&reason)
        .right_aligned()
        .stepper(true)
        .width(VAL_W)
        .show(ui, th);
    let mut done = false;
    if out.step != 0 {
        // 10 mm a press, Shift ×10, as the Up/Down keys of the other number fields
        let fast = ui.input(|i| i.modifiers.shift);
        let k = f64::from(out.step) * if fast { 0.1 } else { 0.01 };
        let n = (*v + k).clamp(lo, hi);
        if n != *v {
            *v = n;
            done = true;
        }
        ui.data_mut(|d| d.remove_temp::<String>(buf_id));
        return done;
    }
    let mut keep = out.response.has_focus();
    if out.committed && !out.cancelled {
        match len_step(&text, &shown, *v, u, lo, hi, mark) {
            LenStep::Set(n) => {
                *v = n;
                done = true;
            }
            LenStep::Keep => {}
            LenStep::Refuse(_) if ui.input(|i| i.key_pressed(egui::Key::Enter)) => {
                // stays open with the error: a dialog must not read this Enter as its action
                ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
                out.response.request_focus();
                keep = true;
            }
            LenStep::Refuse(_) => {}
        }
    }
    if keep {
        ui.data_mut(|d| d.insert_temp(buf_id, text));
    } else {
        ui.data_mut(|d| d.remove_temp::<String>(buf_id));
    }
    done
}

/// Number in the Properties grid, in project units and the UI locale, unit in its own column.
/// `salt` names the edited value (include the element id). true: the value was changed by a
/// commit (one per Enter, Tab or blur; Up/Down step); never while typing, never on bad text.
#[allow(clippy::too_many_arguments)]
pub fn kv_field(ui: &mut egui::Ui, salt: egui::Id, k: &str, v: &mut f64, kind: NumKind, lo: f64, hi: f64, unit: &str) -> bool {
    kv_field_lang(ui, salt, k, v, kind, lo, hi, unit, crate::ui_is_ru())
}

/// [`kv_field`] in an explicit language (no global read).
#[allow(clippy::too_many_arguments)]
pub fn kv_field_lang(ui: &mut egui::Ui, salt: egui::Id, k: &str, v: &mut f64, kind: NumKind, lo: f64, hi: f64, unit: &str, ru: bool) -> bool {
    let kr = key(ui, k);
    let th = ddc_ui::theme::of(ui.ctx());
    let done = cell(
        ui,
        VAL_W,
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            let d = kv_field_body(ui, &th, salt, v, kind, lo, hi, ru, &kr, k);
            #[cfg(test)]
            probe(ui, k, ui.min_rect());
            d
        },
    );
    unit_cell(ui, unit);
    ui.end_row();
    done
}

#[allow(clippy::too_many_arguments)]
fn kv_field_body(ui: &mut egui::Ui, th: &ddc_ui::Theme, salt: egui::Id, v: &mut f64, kind: NumKind, lo: f64, hi: f64, ru: bool, kr: &egui::Response, k: &str) -> bool {
    match kind {
            NumKind::Len(u) => len_field(ui, th, salt, v, u, lo, hi, ru, kr.id, k),
            NumKind::Plain { decimals, step } => {
                let nf = if ru { NumberFormat::RU } else { NumberFormat::EN };
                let err = crate::tr_lang(ru, "number_input.bad_number", &[]);
                ddc_ui::widgets::NumberField::new(salt, v, nf)
                    .labelled_by(kr.id, k)
                    .error_text(&err)
                    .decimals(decimals)
                    .step(step)
                    .range(lo, hi)
                    .stepper(true)
                    .width(VAL_W)
                    .show(ui, th)
                    .committed
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audit 6: in a tall panel the hint gets its own strip under the scrolling body, so it
    /// never lies over a row; a low panel drops it and gives the rows all the room.
    #[test]
    fn hint_strip_sits_below_the_body_and_only_in_tall_panels() {
        for (h, want_strip) in [(640.0, true), (420.0, false)] {
            let ctx = egui::Context::default();
            let input = || egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(320.0, h),
                )),
                ..Default::default()
            };
            let mut clip = egui::Rect::NOTHING;
            let mut full = egui::Rect::NOTHING;
            for _ in 0..3 {
                let _ = ctx.run(input(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        full = ui.available_rect_before_wrap();
                        with_hint_strip(ui, |ui| {
                            clip = ui.clip_rect();
                            hint(ui, "Tab next field");
                            hint_footer(ui);
                        });
                    });
                });
            }
            let gap = full.bottom() - clip.bottom();
            if want_strip {
                assert!((gap - hint_strip_h()).abs() < 0.5, "{h}: body ends {gap} above");
            } else {
                assert!(gap.abs() < 0.5, "{h}: low panel keeps a strip of {gap}");
            }
        }
    }

    #[test]
    fn no_gap_clears_only_the_asked_axis() {
        let mut sp = egui::style::Spacing::default();
        sp.item_spacing = egui::vec2(7.0, 5.0);
        zero_gap(&mut sp, Gap::Rows);
        assert_eq!((sp.item_spacing.x, sp.item_spacing.y), (7.0, 0.0));
        sp.item_spacing = egui::vec2(7.0, 5.0);
        zero_gap(&mut sp, Gap::Cells);
        assert_eq!((sp.item_spacing.x, sp.item_spacing.y), (0.0, 5.0));
    }

    #[test]
    fn no_gap_applies_to_the_ui_style() {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let before = ui.spacing().item_spacing.x;
                no_gap(ui, Gap::Rows);
                assert_eq!(ui.spacing().item_spacing.y, 0.0);
                assert_eq!(ui.spacing().item_spacing.x, before);
            });
        });
    }

    #[test]
    fn units_split_into_own_column() {
        assert_eq!(
            split_unit("1\u{a0}234,57\u{a0}\u{43c}\u{b2}"),
            ("1\u{a0}234,57", "\u{43c}\u{b2}")
        );
        assert_eq!(split_unit("4"), ("4", ""));
        assert_eq!(split_unit("#3"), ("#3", ""));
        assert_eq!(split_unit("19'-8 1/4\""), ("19'-8 1/4\"", ""));
    }

    #[test]
    fn split_unit_rules_hold_for_the_kit_callers() {
        assert_eq!(split_unit("3 000 mm"), ("3 000", "mm"));
        assert_eq!(split_unit("3\u{a0}000\u{a0}mm"), ("3\u{a0}000", "mm"));
        assert_eq!(split_unit("3\u{202f}000\u{202f}mm"), ("3\u{202f}000", "mm"));
        assert_eq!(split_unit("1 250,5 m\u{b2}"), ("1 250,5", "m\u{b2}"));
        assert_eq!(split_unit("12 ft 6 in"), ("12 ft 6 in", ""));
        assert_eq!(split_unit("3000"), ("3000", ""));
        assert_eq!(split_unit("3000 mm"), ("3000", "mm"));
        assert_eq!(split_unit("0.5 kN"), ("0.5", "kN"));
        assert_eq!(split_unit("21 \u{b0}C"), ("21", "\u{b0}C"));
        assert_eq!(split_unit("Level 1 m"), ("Level 1 m", ""));
    }

    #[test]
    fn xlsx_head_shares_the_kit_splitter() {
        for s in ["3 000 mm", "3\u{a0}000\u{a0}mm", "1 250,5 m\u{b2}", "12 ft 6 in", "3000", "A 3 m"] {
            assert_eq!(crate::shell::xlsx_head::split_unit(s), split_unit(s));
        }
    }

    #[test]
    fn body_font_digits_are_tabular() {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        ctx.set_style(ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact).style());
        let _ = ctx.run(Default::default(), |ctx| {
            let f = egui::TextStyle::Body.resolve(&ctx.style());
            let w: Vec<f32> = ('0'..='9')
                .map(|c| ctx.fonts(|fo| fo.glyph_width(&f, c)))
                .collect();
            assert!(w.iter().all(|x| (x - w[0]).abs() < 0.01), "{w:?}");
        });
    }

    fn with_ui(f: impl FnOnce(&mut egui::Ui)) {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        ctx.set_style(ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact).style());
        let mut f = Some(f);
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(f) = f.take() {
                    f(ui);
                }
            });
        });
    }

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

    /// Russian summary and stair labels at 300 px: one line each; elided exactly when the
    /// single-line width exceeds the column.
    #[test]
    fn ru_summary_labels_stay_on_one_line_at_300px() {
        let w = key_col_w(300.0 - 24.0);
        with_ui(|ui| {
            let c = weak(ui);
            for k in SUMMARY_KEYS {
                let t = crate::tr_lang(true, k, &[]);
                let g = key_galley(ui, &t, w, c);
                assert_eq!(g.rows.len(), 1, "{k}: {t}");
                assert_eq!(g.elided, key_natural_w(ui, &t) > w + 0.5, "{k}: {t}");
                assert!(g.size().x <= w + 0.5, "{k}");
            }
        });
    }

    /// A label wider than the column is elided on one line; `key` then offers the full text.
    #[test]
    fn narrow_key_column_elides_and_a_long_label_keeps_a_tooltip() {
        with_ui(|ui| {
            let c = weak(ui);
            let long = crate::tr_lang(true, "type_dialog.warn.slab_area", &[]).repeat(3);
            let g = key_galley(ui, &long, key_col_w(0.0), c);
            assert!(g.elided);
            assert_eq!(g.rows.len(), 1);
            ui.data_mut(|d| d.insert_temp(egui::Id::new("ddc.props.key_w"), key_col_w(0.0)));
            let r = key(ui, &long);
            assert!(r.rect.width() <= key_col_w(0.0) + 0.5);
        });
    }

    /// Every empty-state place has a title, hint and action in both languages.
    #[test]
    fn every_empty_place_has_an_action() {
        for what in ["props", "totals", "sheets", "schedule", "schedule_filtered", "schedule_fields", "library"] {
            for ru in [false, true] {
                for part in ["title", "hint", "action"] {
                    let k = format!("empty.{what}.{part}");
                    let v = crate::tr_lang(ru, &k, &[]);
                    assert!(!v.is_empty() && v != k, "{k} ru={ru}");
                }
            }
        }
    }

    /// Tab focuses the action, Enter clicks it: `empty_state` reports the click once.
    #[test]
    fn empty_state_returns_true_when_action_is_clicked() {
        use egui::{Event, Key, RawInput};
        let ctx = egui::Context::default();
        ctx.set_fonts(ddc_ui::theme::font_definitions());
        let key = |pressed| RawInput {
            events: vec![Event::Key {
                key: if pressed { Key::Tab } else { Key::Enter },
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            ..Default::default()
        };
        for what in ["props", "sheets", "schedule_filtered"] {
            let mut hits = 0;
            for input in [RawInput::default(), key(true), RawInput::default(), key(false), RawInput::default()] {
                let _ = ctx.run(input, |c| {
                    egui::CentralPanel::default().show(c, |ui| {
                        if empty_state(ui, what) {
                            hits += 1;
                        }
                    });
                });
            }
            assert_eq!(hits, 1, "{what}");
        }
    }

    #[test]
    fn commit_on_tab_and_click_away() {
        // changed text applies on blur without Enter; unchanged text does nothing
        assert_eq!(field_end(true, false, false, true), FieldEnd::Apply);
        assert_eq!(field_end(true, false, false, false), FieldEnd::Keep);
        assert_eq!(field_end(true, true, false, false), FieldEnd::Apply);
        assert_eq!(field_end(false, true, false, true), FieldEnd::Keep);
    }

    #[test]
    fn esc_reverts() {
        assert_eq!(field_end(true, false, true, true), FieldEnd::Revert);
        assert_eq!(field_end(true, true, true, true), FieldEnd::Revert);
    }

    #[test]
    fn invalid_marks_error() {
        assert!(length_text_invalid("abc", Units::Metric));
        assert!(!length_text_invalid("", Units::Metric));
        assert!(!length_text_invalid("1500", Units::Metric));
    }

    use crate::units::{FOOT, INCH};
    use ddc_ui::widgets::field::{format_number, parse_number};
    const PT: DecimalMark = DecimalMark::Point;
    const CM: DecimalMark = DecimalMark::Comma;

    fn fine(m: f64, u: Units, mark: DecimalMark) -> String {
        len_text_prec(m, u, mark, 3, 16)
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn mm_display_and_edit_round_trip() {
        let m = Units::Metric;
        assert_eq!(fine(0.2405, m, PT), "240.5");
        assert_eq!(fine(0.2405, m, CM), "240,5");
        close(len_commit("240.5", m, 0.0, 100.0, PT).unwrap(), 0.2405);
        close(len_commit("240,5", m, 0.0, 100.0, CM).unwrap(), 0.2405);
        for mark in [PT, CM] {
            for v in [0.0125, 0.2, 2.4005, 3.0] {
                let t = fine(v, m, mark);
                close(len_commit(&t, m, 0.0, 100.0, mark).unwrap(), v);
            }
        }
    }

    #[test]
    fn metres_round_trip_in_project_units() {
        // a metre-sized value is shown in mm with groups and read back from the same text
        assert_eq!(fine(3.0, Units::Metric, PT), "3,000");
        assert_eq!(fine(3.0, Units::Metric, CM), "3\u{a0}000");
        close(len_commit("3 000", Units::Metric, 0.0, 100.0, CM).unwrap(), 3.0);
    }

    #[test]
    fn feet_inches_round_trip() {
        let u = Units::Imperial;
        for v in [5.0 * FOOT + 6.5 * INCH, 10.0 * FOOT, 0.25 * INCH, 19.0 * FOOT + 8.25 * INCH] {
            for mark in [PT, CM] {
                let t = len_text(v, u, mark);
                let back = len_commit(&t, u, 0.0, 100.0, mark).unwrap();
                assert!((back - v).abs() < INCH / 8.0, "{t}: {back} vs {v}");
            }
        }
    }

    #[test]
    fn english_thousands_group_and_comma_decimal_is_refused() {
        let m = Units::Metric;
        close(len_commit("1,000", m, 0.0, 100.0, PT).unwrap(), 1.0);
        close(len_commit("1,000.5", m, 0.0, 100.0, PT).unwrap(), 1.0005);
        assert_eq!(len_commit("1,5", m, 0.0, 100.0, PT), Err(InputError::CommaDecimal));
        assert_eq!(parse_number("1,000", NumberFormat::EN), Some(1000.0));
        assert_eq!(parse_number("1,5", NumberFormat::EN), None);
    }

    #[test]
    fn russian_groups_and_decimal_comma() {
        close(len_commit("1 000,5", Units::Metric, 0.0, 100.0, CM).unwrap(), 1.0005);
        close(len_commit("1\u{a0}000,5", Units::Metric, 0.0, 100.0, CM).unwrap(), 1.0005);
        assert_eq!(parse_number("1 000,5", NumberFormat::RU), Some(1000.5));
        assert_eq!(format_number(2400.5, 1, NumberFormat::RU), "2\u{a0}400,5");
        assert_eq!(format_number(2400.5, 1, NumberFormat::EN), "2,400.5");
    }

    #[test]
    fn refused_text_gives_no_value() {
        let m = Units::Metric;
        assert!(len_commit("abc", m, 0.0, 100.0, PT).is_err());
        assert!(len_commit("", m, 0.0, 100.0, PT).is_err());
        assert!(len_commit("12x", m, 0.0, 100.0, CM).is_err());
        // a minus is read only where the range allows negatives
        assert!(len_commit("-1200", m, 0.0, 100.0, PT).is_err());
        close(len_commit("-1200", m, -10.0, 10.0, PT).unwrap(), -1.2);
        // out of range clamps
        close(len_commit("999999", m, 0.0, 5.0, PT).unwrap(), 5.0);
    }

    #[test]
    fn deg_pct_count_use_the_number_field_rules() {
        assert_eq!(parse_number("12.25", NumberFormat::EN), Some(12.25));
        assert_eq!(parse_number("12,25", NumberFormat::RU), Some(12.25));
        assert_eq!(parse_number("12,25", NumberFormat::EN), None);
        assert_eq!(format_number(12.25, 2, NumberFormat::RU), "12,25");
        assert_eq!(format_number(7.0, 0, NumberFormat::EN), "7");
    }

    #[test]
    fn stair_tread_uses_the_ui_decimal_mark() {
        // 0.277059 m at 3 decimals: a point in English, a comma in Russian
        assert_eq!(fine(0.277059, Units::Metric, PT), "277.059");
        assert_eq!(fine(0.277059, Units::Metric, CM), "277,059");
        assert_eq!(fine(0.28, Units::Metric, PT), "280");
    }

    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    fn frames(ctx: &egui::Context, mut add: impl FnMut(&mut egui::Ui)) {
        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| add(ui));
            });
        }
    }

    #[test]
    fn value_widgets_of_every_kind_share_one_column() {
        use ddc_model::Phase;
        let ph = |id: i64, n: &str| Phase { id, name: n.into(), seq: id, key: None };
        let phases = vec![ph(1, "Existing"), ph(2, "New construction")];
        for ru in [false, true] {
            let ctx = font_ctx();
            let th = ddc_ui::theme::of(&ctx);
            let (mut c, mut d) = (1i64, None);
            let (mut len, mut ang, mut pct, mut cnt) = (0.277059, 0.5, 2.0, 3.0);
            let mut ix = 0usize;
            let items = ["Rect", "Round"];
            frames(&ctx, |ui| {
                props(ui, "col", |ui| {
                    crate::phase_ui::phase_rows(ui, &phases, Some(1), &mut c, &mut d, ru);
                    kv_widget(ui, "dd", |ui| {
                        ddc_ui::widgets::field::dropdown(ui, &th, "dd", &mut ix, &items, Some(WIDGET_W));
                    });
                    kv_widget(ui, "bare", |ui| {
                        egui::ComboBox::from_id_salt("bare").selected_text("x").show_ui(ui, |_| {});
                    });
                    let m = Units::Metric;
                    let id = |s: &str| egui::Id::new(("col", s));
                    kv_field_lang(ui, id("len"), "len", &mut len, NumKind::Len(m), 0.0, 9.0, "mm", ru);
                    kv_field_lang(ui, id("ang"), "ang", &mut ang, NumKind::DEG, 0.0, 9.0, "d", ru);
                    kv_field_lang(ui, id("pct"), "pct", &mut pct, NumKind::PCT, 0.0, 9.0, "%", ru);
                    kv_field_lang(ui, id("cnt"), "cnt", &mut cnt, NumKind::COUNT, 0.0, 9.0, "", ru);
                });
            });
            let keys = [
                crate::tr_str(ru, "phui.created"),
                crate::tr_str(ru, "phui.demolished"),
                "dd",
                "bare",
            ];
            let left = probed(&ctx, "len").expect("len probed").min.x;
            for k in keys {
                let r = probed(&ctx, k).unwrap_or_else(|| panic!("{k} probed"));
                assert!((r.min.x - left).abs() < 0.5, "{k}: {} vs {left} (ru {ru})", r.min.x);
                assert!((r.width() - WIDGET_W).abs() < 0.5, "{k}: width {} (ru {ru})", r.width());
            }
            for k in ["ang", "pct", "cnt"] {
                let r = probed(&ctx, k).unwrap();
                assert!((r.min.x - left).abs() < 0.5, "{k}: {} vs {left}", r.min.x);
            }
        }
    }

    #[test]
    fn lengths_show_at_the_project_precision() {
        let m = Units::Metric;
        // default: whole millimetres, the raw 277.059 is not shown
        assert_eq!(crate::units::LENGTH_DECIMALS, 0);
        assert_eq!(len_text_prec(0.277059, m, PT, 0, 16), "277");
        assert_eq!(len_text_prec(0.277059, m, CM, 0, 16), "277");
        assert_eq!(len_text_prec(3.0, m, PT, 0, 16), "3,000");
        assert_eq!(len_text_prec(3.0, m, CM, 0, 16), "3\u{a0}000");
        // a project that asks for more gets more, trailing zeros dropped
        assert_eq!(len_text_prec(0.277059, m, PT, 1, 16), "277.1");
        assert_eq!(len_text_prec(0.2775, m, CM, 1, 16), "277,5");
        assert_eq!(len_text_prec(0.28, m, CM, 2, 16), "280");
        // imperial follows the fraction denominator
        let u = Units::Imperial;
        let v = 3.0 * FOOT + 4.0 * INCH + 0.3 * INCH;
        assert_eq!(len_text_prec(v, u, PT, 0, 16), "3'-4 5/16\"");
        assert_eq!(len_text_prec(v, u, CM, 0, 4), "3'-4 1/4\"");
        assert_eq!(len_text_prec(v, u, PT, 0, 2), "3'-4 1/2\"");
    }

    #[test]
    fn untouched_length_commits_nothing_and_a_real_edit_commits_once() {
        let cases = [
            (Units::Metric, PT, 0.277059),
            (Units::Metric, CM, 0.277059),
            (Units::Metric, CM, 3.0),
            (Units::Imperial, PT, 3.0 * FOOT + 4.3 * INCH),
            (Units::Imperial, CM, 10.0 * FOOT),
        ];
        for (u, mark, v) in cases {
            let shown = len_text_prec(v, u, mark, 0, 16);
            assert_eq!(len_step(&shown, &shown, v, u, 0.0, 100.0, mark), LenStep::Keep);
            // surrounding blanks are not an edit either
            let padded = format!(" {shown} ");
            assert_eq!(len_step(&padded, &shown, v, u, 0.0, 100.0, mark), LenStep::Keep);
        }
        let shown = len_text_prec(0.277059, Units::Metric, PT, 0, 16);
        match len_step("300", &shown, 0.277059, Units::Metric, 0.0, 100.0, PT) {
            LenStep::Set(n) => close(n, 0.3),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn refused_length_text_is_not_a_commit() {
        let shown = len_text_prec(0.277059, Units::Metric, CM, 0, 16);
        assert!(matches!(
            len_step("abc", &shown, 0.277059, Units::Metric, 0.0, 100.0, CM),
            LenStep::Refuse(_)
        ));
        assert_eq!(
            len_step("1,5", &len_text_prec(1.0, Units::Metric, PT, 0, 16), 1.0, Units::Metric, 0.0, 100.0, PT),
            LenStep::Refuse(InputError::CommaDecimal)
        );
    }

    #[test]
    fn length_field_renders_untouched_without_writing_in_both_languages() {
        for ru in [false, true] {
            for u in [Units::Metric, Units::Imperial] {
                let ctx = font_ctx();
                let mut v = 0.277059;
                let mut any = false;
                frames(&ctx, |ui| {
                    props(ui, "one", |ui| {
                        any |= kv_field_lang(ui, egui::Id::new("one"), "len", &mut v, NumKind::Len(u), 0.0, 9.0, "mm", ru);
                    });
                });
                assert!(!any, "idle frames write nothing (ru {ru}, {u:?})");
                assert_eq!(v.to_bits(), 0.277059f64.to_bits());
            }
        }
    }

    #[test]
    fn properties_callers_have_no_raw_drag_value() {
        let files = [
            ("geom_props", include_str!("geom_props.rs")),
            ("struct_props", include_str!("struct_props.rs")),
            ("elem/rebar", include_str!("elem/rebar.rs")),
            ("elem/roof", include_str!("elem/roof.rs")),
            ("elem/topo_fam", include_str!("elem/topo_fam.rs")),
            ("data_ui", include_str!("data_ui.rs")),
            ("shell/panels", include_str!("shell/panels.rs")),
            ("cw_props", include_str!("cw_props.rs")),
        ];
        for (name, src) in files {
            assert!(!src.contains("kv_drag"), "{name}: kv_drag");
            assert!(!src.contains("len_drag"), "{name}: len_drag");
            assert!(!src.contains("ComboBox"), "{name}: ComboBox");
        }
        for (name, src) in &files[..7] {
            assert!(!src.contains("DragValue"), "{name}: DragValue");
        }
        // split needle: this line itself must not match
        let literal = format!("[VAL_W, {}]", "22.0");
        assert!(!include_str!("ui_kit.rs").contains(&literal));
    }
}
