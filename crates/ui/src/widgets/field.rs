// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Text input with a muted unit suffix, numeric field with locale decimals, dropdown.
//! Enter, Tab and leaving the field commit an edit, an untouched field commits nothing; Esc
//! restores the text it had on focus.
use crate::theme::{control, space, stroke, ControlSize, Role};
use crate::{Lang, Theme};
use egui::{
    Align, Id, Key, Layout, Margin, Modifiers, Rect, Response, RichText, Sense, Shape, Stroke,
    StrokeKind, TextEdit, Ui, UiBuilder, Vec2,
};

/// What happened to a field this pass.
#[derive(Debug, Clone)]
pub struct FieldOutput {
    pub response: Response,
    /// Text changed by typing.
    pub changed: bool,
    /// Edit finished (Enter, Tab, focus left) and there is something to apply.
    pub committed: bool,
    /// Esc or unusable input: the value is back to what it was on focus.
    pub cancelled: bool,
    /// The field's whole slot (frame and the unit right of it), for alignment with
    /// neighbouring rows.
    pub rect: Rect,
    /// Stepper press this pass: +1 up, -1 down, 0 none (see [`TextField::stepper`]).
    pub step: i8,
}

/// Width of the up/down stepper inside the right edge of a numeric field.
pub const STEPPER_W: f32 = 16.0;

/// Stroke of the chevrons drawn by fields and lists (until the icon set supplies them).
pub const CHEVRON_STROKE: f32 = 1.5;

/// Paints the up/down stepper in `area` (inside the field frame, right end) and returns the
/// press: +1, -1 or 0. Each half is its own button, so screen readers can reach both. The area
/// is reserved always; the halves show and take presses only while `shown` (hover or focus), so
/// the text never moves when they appear.
fn stepper(ui: &mut Ui, th: &Theme, id: Id, area: Rect, enabled: bool, shown: bool) -> i8 {
    let s = th.sem();
    let mut out = 0;
    let half_h = (area.height() / 2.0).floor();
    for (up, sign) in [(true, 1_i8), (false, -1_i8)] {
        let top = if up { area.center().y - half_h } else { area.center().y };
        let hr = Rect::from_min_size(egui::pos2(area.left(), top), Vec2::new(area.width(), half_h));
        // mouse only: Tab goes field to field, the keyboard steps with Up/Down
        let sense = if enabled { Sense::CLICK } else { Sense::hover() };
        let r = ui.interact(hr, id.with(("step", up)), sense);
        let name = if up { "+" } else { "-" };
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, name));
        if !shown && !r.hovered() {
            continue;
        }
        let col = if enabled && r.hovered() {
            ui.painter().rect_filled(hr, th.radius("xs"), th.hover_fill());
            s.text_primary
        } else {
            s.text_secondary
        };
        let c = hr.center();
        let d = if up { -1.0 } else { 1.0 };
        ui.painter().add(Shape::line(
            vec![
                c + Vec2::new(-3.0, -d * 1.5),
                c + Vec2::new(0.0, d * 1.5),
                c + Vec2::new(3.0, -d * 1.5),
            ],
            Stroke::new(CHEVRON_STROKE, col),
        ));
        if r.clicked() {
            out = sign;
        }
    }
    out
}

/// Search look of a [`TextField`]: leading glyph, clear glyph while non-empty, Esc empties the
/// text and keeps focus.
#[derive(Clone, Copy)]
struct SearchLook<'a> {
    icon: &'a str,
    /// Glyph left edge and text left edge, measured from the field's left edge.
    glyph_x: f32,
    text_x: f32,
    clear_tip: &'a str,
}

/// Text on focus. `pass` tells a live edit from one left behind by a field that vanished.
#[derive(Clone, Default)]
struct FocusStart {
    text: String,
    pass: u64,
}

pub struct TextField<'a> {
    buf: &'a mut String,
    id: Option<Id>,
    id_salt: Option<Id>,
    hint: &'a str,
    unit: &'a str,
    width: Option<f32>,
    align: Align,
    error: bool,
    label_id: Option<Id>,
    label: &'a str,
    error_text: &'a str,
    search: Option<SearchLook<'a>>,
    stepper: bool,
}

/// Accessible name of an input: label, unit after a comma, invalid-state text after a dash
/// (this egui has no separate description slot).
pub(crate) fn field_name(label: &str, unit: &str, error: &str) -> String {
    let mut s = label.to_string();
    if !unit.is_empty() {
        if !s.is_empty() {
            s.push_str(", ");
        }
        s.push_str(unit);
    }
    if !error.is_empty() {
        if !s.is_empty() {
            s.push_str(" - ");
        }
        s.push_str(error);
    }
    s
}

impl<'a> TextField<'a> {
    pub fn new(buf: &'a mut String) -> Self {
        Self {
            buf,
            id: None,
            id_salt: None,
            hint: "",
            unit: "",
            width: None,
            align: Align::Min,
            error: false,
            label_id: None,
            label: "",
            error_text: "",
            search: None,
            stepper: false,
        }
    }
    /// Up/down stepper inside the right edge of the field; `FieldOutput::step` reports a press.
    /// The field keeps its width, so stepped and plain fields of a column end at one x.
    pub fn stepper(mut self, on: bool) -> Self {
        self.stepper = on;
        self
    }
    /// Search field: `icon` glyph at `glyph_x` and the text at `text_x` (both from the field's
    /// left edge, so a list below can share its row slots), a clear glyph with tooltip
    /// `clear_tip` while the text is non-empty, and Esc empties the text without leaving the field.
    pub fn search(mut self, icon: &'a str, glyph_x: f32, text_x: f32, clear_tip: &'a str) -> Self {
        self.search = Some(SearchLook {
            icon,
            glyph_x,
            text_x,
            clear_tip,
        });
        self
    }
    /// The caller's label widget (its id and text) names the input for screen readers. The
    /// relation is set only when a widget with that id was drawn; the text names it always.
    pub fn labelled_by(mut self, label_id: Id, label: &'a str) -> Self {
        self.label_id = Some(label_id);
        self.label = label;
        self
    }
    /// Text read out while the input is invalid.
    pub fn error_text(mut self, text: &'a str) -> Self {
        self.error_text = text;
        self
    }
    /// Stable identity when the caller lays out several fields in a loop.
    pub fn id_salt(mut self, salt: impl std::hash::Hash) -> Self {
        self.id_salt = Some(Id::new(salt));
        self
    }
    /// Exact widget id.
    pub fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }
    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }
    /// Unit shown muted after the text (for example "mm").
    pub fn unit(mut self, unit: &'a str) -> Self {
        self.unit = unit;
        self
    }
    pub fn width(mut self, w: f32) -> Self {
        self.width = Some(w);
        self
    }
    /// Numbers are right-aligned.
    pub fn right_aligned(mut self) -> Self {
        self.align = Align::Max;
        self
    }
    pub fn error(mut self, error: bool) -> Self {
        self.error = error;
        self
    }

    pub fn show(self, ui: &mut Ui, th: &Theme) -> FieldOutput {
        let s = th.sem();
        let d = th.d();
        let font = th.font(Role::Body);
        let w = self
            .width
            .unwrap_or_else(|| ui.available_width())
            .max(control::FIELD_MIN_W);
        let (slot, frame) = ui.allocate_exact_size(Vec2::new(w, d.control), Sense::hover());
        let rect = slot;
        let id = self.id.unwrap_or_else(|| match self.id_salt {
            Some(salt) => ui.id().with(salt),
            None => frame.id.with("edit"),
        });
        let enabled = ui.is_enabled();
        let bg = ui.painter().add(Shape::Noop);
        let mut inner = rect.shrink2(Vec2::new(d.pad_x, 0.0));
        let step_area = self.stepper.then(|| {
            Rect::from_min_max(
                egui::pos2(rect.right() - space::XXS - STEPPER_W, rect.top() + space::XXS),
                egui::pos2(rect.right() - space::XXS, rect.bottom() - space::XXS),
            )
        });
        if let Some(a) = step_area {
            inner.max.x = inner.max.x.min(a.left() - space::XS);
        }
        // the unit sits inside the frame after the text, muted, gap XS: the frame is the whole
        // field, and the number's right edge is the same with or without a stepper
        let mut unit_paint = None;
        if !self.unit.is_empty() {
            let col = s.text_secondary;
            let g = ui.painter().layout_no_wrap(self.unit.to_owned(), font.clone(), col);
            let gw = g.size().x.min((inner.width() - control::FIELD_MIN_W / 2.0).max(0.0));
            let pos = egui::pos2(inner.right() - gw, rect.center().y - g.size().y / 2.0);
            inner.max.x -= gw + space::XS;
            unit_paint = Some((pos, g, col));
        }
        let hover_in = ui.rect_contains_pointer(rect);
        let show_clear = self.search.is_some()
            && !self.buf.is_empty()
            && enabled
            && (hover_in || ui.memory(|m| m.has_focus(id)));
        let clear_side = d.control_sm;
        let clear_rect = Rect::from_center_size(
            egui::pos2(rect.right() - space::XS - clear_side / 2.0, rect.center().y),
            Vec2::splat(clear_side),
        );
        let clear_w = if show_clear { clear_side + space::XS } else { 0.0 };
        let left = self
            .search
            .map_or(inner.left(), |k| rect.left() + k.text_x);
        let text_rect = Rect::from_min_max(
            egui::pos2(left, inner.top()),
            egui::pos2(
                (inner.right() - clear_w).max(left + 1.0),
                inner.bottom(),
            ),
        );
        if let Some(k) = self.search {
            let px = crate::theme::icon_size::SM;
            let ic = Rect::from_center_size(
                egui::pos2(rect.left() + k.glyph_x + px / 2.0, rect.center().y),
                Vec2::splat(px),
            );
            let col = s.text_secondary;
            let ctx = ui.ctx().clone();
            super::with_icons(&ctx, |icons| icons.paint(ui, k.icon, px as u32, ic, col));
        }
        if self.search.is_some() {
            // an empty focused search field has nothing for Esc to clear: the window may take it
            let key = Id::new("ddc.search.empty_focus");
            let mine = self.buf.is_empty() && ui.memory(|m| m.has_focus(id));
            ui.data_mut(|d| {
                if mine {
                    d.insert_temp(key, id);
                } else if d.get_temp::<Id>(key) == Some(id) {
                    d.remove::<Id>(key);
                }
            });
        }
        let mut esc_cleared = false;
        if self.search.is_some()
            && !self.buf.is_empty()
            && ui.memory(|m| m.has_focus(id))
            && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.buf.clear();
            esc_cleared = true;
        }
        // the padding belongs to the field: a click there focuses it without adding a Tab stop
        let pad_click = ui.interact(rect, id.with("pad"), Sense::CLICK);
        if let Some((pos, g, col)) = unit_paint {
            ui.painter().galley(pos, g, col);
        }
        // the edit spans the whole frame (leading glyph included) and keeps the text where it
        // is with margins, so the widget rect a reader or a test sees is the field, not its text
        let margin = Margin {
            left: (text_rect.left() - rect.left()).round().clamp(0.0, 127.0) as i8,
            right: (rect.right() - text_rect.right()).round().clamp(0.0, 127.0) as i8,
            top: 0,
            bottom: 0,
        };
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        // a disabled Ui fades its painter: dimming the colour too would dim twice
        let text_col = if enabled {
            s.text_primary
        } else {
            s.text_secondary
        };
        let mut resp = child.add(
            TextEdit::singleline(&mut *self.buf)
                .id(id)
                .frame(false)
                .margin(margin)
                .desired_width(rect.width())
                .font(font)
                .text_color(text_col)
                .horizontal_align(self.align)
                .vertical_align(Align::Center)
                .hint_text(RichText::new(self.hint).color(s.text_placeholder)),
        );
        if pad_click.clicked() {
            resp.request_focus();
        }
        // after the padding, which covers the whole frame: the stepper takes its own presses
        let step = step_area.map_or(0, |a| {
            stepper(ui, th, id, a, enabled, hover_in || resp.has_focus())
        });
        // a relation to an id no widget owns would point screen readers at nothing; a passive
        // caption that only paints gets its node here so the relation always resolves
        if let Some(lr) = self.label_id.and_then(|l| ui.ctx().read_response(l)) {
            if !lr.sense.is_focusable() {
                let text = self.label;
                lr.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
            }
            resp = resp.labelled_by(lr.id);
        }
        if !self.label.is_empty() || !self.error_text.is_empty() {
            let err = if self.error { self.error_text } else { "" };
            let name = field_name(self.label, self.unit, err);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, enabled, name.as_str()));
        }
        let origin = id.with("origin");
        let pass = ui.ctx().cumulative_pass_nr();
        let start = ui
            .data(|m| m.get_temp::<FocusStart>(origin))
            .filter(|o| o.pass + 1 >= pass);
        let mut changed = resp.changed() || esc_cleared;
        if esc_cleared {
            resp.mark_changed();
        }
        if show_clear {
            if let Some(k) = self.search {
                let cr = ui.interact(clear_rect, id.with("clear"), Sense::click());
                cr.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, k.clear_tip)
                });
                let col = if cr.hovered() {
                    s.text_primary
                } else {
                    s.text_secondary
                };
                let px = crate::theme::icon_size::SM;
                let ic = Rect::from_center_size(clear_rect.center(), Vec2::splat(px));
                let ctx = ui.ctx().clone();
                super::with_icons(&ctx, |icons| icons.paint(ui, "close", px as u32, ic, col));
                if cr.clicked() {
                    self.buf.clear();
                    resp.request_focus();
                    changed = true;
                    resp.mark_changed();
                }
                let (title, keys) = super::split_tip(k.clear_tip);
                super::rich_tooltip(cr, th, title, "", keys);
            }
        }
        let mut committed = false;
        let mut cancelled = false;
        if resp.has_focus() {
            // the field border marks focus (error border too): no global ring on top
            super::own_focus(ui.ctx(), resp.id);
            if self.search.is_some() && !self.buf.is_empty() {
                // egui drops focus on Esc before the field runs: keep it so Esc can clear the text
                let keep_esc = egui::EventFilter {
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    tab: false,
                    escape: true,
                };
                ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, keep_esc));
            }
            let text = start.map_or_else(|| self.buf.clone(), |o| o.text);
            ui.data_mut(|m| m.insert_temp(origin, FocusStart { text, pass }));
        } else {
            ui.data_mut(|m| m.remove_temp::<FocusStart>(origin));
            // egui reports lost_focus only if focus moved before this widget ran: a press on a
            // later field or Shift+Tab would go unseen, so leaving is read from the stored start
            if let Some(start) = start {
                let (esc, enter) =
                    ui.input(|i| (i.key_pressed(Key::Escape), i.key_pressed(Key::Enter)));
                if esc {
                    if *self.buf != start.text {
                        *self.buf = start.text;
                        changed = true;
                    }
                    cancelled = true;
                } else {
                    committed = enter || *self.buf != start.text;
                }
            }
        }
        // one frame of constant width in every state: focus recolours it and adds the ring
        // outside, so the text never moves
        let (line, line_w) = if self.error {
            (s.error, stroke::CONTROL)
        } else if resp.has_focus() {
            (s.focus, stroke::CONTROL)
        } else if (resp.hovered() || pad_click.hovered()) && enabled {
            (s.text_secondary, stroke::CONTROL)
        } else if enabled {
            (s.border_control, stroke::CONTROL)
        } else {
            (s.border_subtle, stroke::CONTROL)
        };
        let fill = if enabled { s.bg_input } else { s.bg_sunken };
        ui.painter().set(
            bg,
            egui::epaint::RectShape::new(
                rect,
                th.radius("sm"),
                fill,
                Stroke::new(line_w, line),
                StrokeKind::Inside,
            ),
        );
        if resp.has_focus() && super::keyboard_focus(ui.ctx()) {
            th.paint_focus_ring(ui.painter(), rect, th.radius("sm"));
        }
        FieldOutput {
            response: resp,
            changed,
            committed,
            cancelled,
            rect: slot,
            step,
        }
    }
}

/// The one arrow of a closed list: a thin chevron in secondary ink, as the type picker draws.
pub fn down_chevron(p: &egui::Painter, c: egui::Pos2, ink: egui::Color32) {
    p.add(Shape::line(
        vec![c + Vec2::new(-3.5, -1.5), c + Vec2::new(0.0, 2.0), c + Vec2::new(3.5, -1.5)],
        Stroke::new(CHEVRON_STROKE, ink),
    ));
}

/// Decimal and group separators of a locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumberFormat {
    pub decimal: char,
    pub group: Option<char>,
}

impl NumberFormat {
    /// «1 234,57» with a no-break space.
    pub const RU: Self = Self {
        decimal: ',',
        group: Some('\u{a0}'),
    };
    /// «1,234.57».
    pub const EN: Self = Self {
        decimal: '.',
        group: Some(','),
    };
    /// The locale's marks from `data/number_formats.json`; a locale missing there reads as `en`.
    pub fn for_lang(lang: Lang) -> Self {
        static TABLE: std::sync::OnceLock<std::collections::BTreeMap<String, NumberFormat>> =
            std::sync::OnceLock::new();
        let t = TABLE.get_or_init(|| {
            let v: serde_json::Value =
                serde_json::from_str(include_str!("../../data/number_formats.json")).expect("number_formats.json");
            let ch = |x: &serde_json::Value| x.as_str().and_then(|s| s.chars().next());
            v.as_object()
                .into_iter()
                .flatten()
                .filter_map(|(k, x)| Some((k.clone(), NumberFormat { decimal: ch(&x["decimal"])?, group: ch(&x["group"]) })))
                .collect()
        });
        t.get(lang.code()).copied().unwrap_or(Self::EN)
    }
}

/// Formats with fixed decimals and locale separators; a value rounded to zero has no sign.
pub fn format_number(v: f64, decimals: usize, nf: NumberFormat) -> String {
    let body = format!("{:.*}", decimals, v.abs());
    let (int, frac) = body.split_once('.').unwrap_or((body.as_str(), ""));
    let mut out = String::new();
    if v < 0.0 && body.chars().any(|c| c.is_ascii_digit() && c != '0') {
        out.push('-');
    }
    let n = int.len();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            if let Some(g) = nf.group {
                out.push(g);
            }
        }
        out.push(c);
    }
    if !frac.is_empty() {
        out.push(nf.decimal);
        out.push_str(frac);
    }
    out
}

/// Decimal mark of a number format: a comma decimal reads the Russian way, else English.
pub fn decimal_mark(nf: NumberFormat) -> crate::numparse::DecimalMark {
    crate::numparse::DecimalMark::for_ru(nf.decimal == ',')
}

/// Parses user input by the shared locale rules of [`crate::numparse`]: groups of three only,
/// the minus may be typographic, an English «1,5» is refused rather than guessed.
pub fn parse_number(s: &str, nf: NumberFormat) -> Option<f64> {
    crate::numparse::parse(s, decimal_mark(nf)).ok()
}

/// A number being edited. `at_focus` is what the field showed untouched: text equal to it is no
/// edit, so a value with more precision than the field displays stays as stored.
#[derive(Clone, Default)]
struct NumberEdit {
    text: String,
    at_focus: String,
    pass: u64,
}

/// Numeric field: right-aligned, muted unit, locale decimals, Up/Down step (Shift ×10). While
/// focused it shows the number without group separators: editing «1,234» would read as a decimal.
/// The value changes only on an edit; Tab or a click through the field commits nothing.
pub struct NumberField<'a> {
    value: &'a mut f64,
    salt: Id,
    decimals: usize,
    step: f64,
    min: f64,
    max: f64,
    unit: &'a str,
    width: Option<f32>,
    format: NumberFormat,
    label_id: Option<Id>,
    label: &'a str,
    error_text: &'a str,
    stepper: bool,
}

impl<'a> NumberField<'a> {
    /// Up/down stepper inside the field: one `step` per press, Shift ×10, within the range.
    pub fn stepper(mut self, on: bool) -> Self {
        self.stepper = on;
        self
    }
    /// True while the field `salt` (as given to `new`, in the same `ui`) holds typed text that
    /// does not parse, so a caller can hold back an action that would use the old value.
    pub fn refused(ui: &Ui, salt: impl std::hash::Hash, format: NumberFormat) -> bool {
        let buf_id = ui.id().with(Id::new(salt)).with("buf");
        let pass = ui.ctx().cumulative_pass_nr();
        ui.data(|d| d.get_temp::<NumberEdit>(buf_id))
            .filter(|e| e.pass + 1 >= pass)
            .is_some_and(|e| e.text != e.at_focus && parse_number(&e.text, format).is_none())
    }
    /// `salt` names the edited value: include the element id, or an edit in progress lands on
    /// whatever the field shows next.
    pub fn new(salt: impl std::hash::Hash, value: &'a mut f64, format: NumberFormat) -> Self {
        Self {
            value,
            salt: Id::new(salt),
            decimals: 0,
            step: 1.0,
            min: f64::NEG_INFINITY,
            max: f64::INFINITY,
            unit: "",
            width: None,
            format,
            label_id: None,
            label: "",
            error_text: "",
            stepper: false,
        }
    }
    /// The caller's label widget (its id and text) names the input for screen readers. The
    /// relation is set only when a widget with that id was drawn; the text names it always.
    pub fn labelled_by(mut self, label_id: Id, label: &'a str) -> Self {
        self.label_id = Some(label_id);
        self.label = label;
        self
    }
    /// Text read out while the typed number does not parse.
    pub fn error_text(mut self, text: &'a str) -> Self {
        self.error_text = text;
        self
    }
    pub fn decimals(mut self, d: usize) -> Self {
        self.decimals = d;
        self
    }
    pub fn step(mut self, s: f64) -> Self {
        self.step = s;
        self
    }
    pub fn range(mut self, min: f64, max: f64) -> Self {
        self.min = min;
        self.max = max;
        self
    }
    pub fn unit(mut self, u: &'a str) -> Self {
        self.unit = u;
        self
    }
    pub fn width(mut self, w: f32) -> Self {
        self.width = Some(w);
        self
    }

    pub fn show(self, ui: &mut Ui, th: &Theme) -> FieldOutput {
        let Self {
            value,
            salt,
            decimals,
            step,
            min,
            max,
            unit,
            width,
            format,
            label_id,
            label,
            error_text,
            stepper,
        } = self;
        let plain_format = NumberFormat {
            group: None,
            ..format
        };
        let id = ui.id().with(salt);
        let buf_id = id.with("buf");
        let pass = ui.ctx().cumulative_pass_nr();
        let shown = format_number(*value, decimals, format);
        let plain = format_number(*value, decimals, plain_format);
        let untouched = || NumberEdit {
            text: plain.clone(),
            at_focus: plain.clone(),
            pass,
        };
        let focused = ui.memory(|m| m.has_focus(id));
        // read without focus too: a press on an earlier field moves focus before this one runs
        let pending = ui
            .data(|d| d.get_temp::<NumberEdit>(buf_id))
            .filter(|e| e.pass + 1 >= pass);
        let edit = match pending {
            Some(e) if e.text != e.at_focus => Some(e),
            Some(_) => Some(untouched()),
            None if focused => Some(untouched()),
            None => None,
        };
        let mut buf = edit
            .as_ref()
            .map_or_else(|| shown.clone(), |e| e.text.clone());
        let invalid = edit.is_some() && parse_number(&buf, format).is_none();
        let mut f = TextField::new(&mut buf)
            .id(id)
            .right_aligned()
            .stepper(stepper)
            .unit(unit)
            .error(invalid)
            .error_text(error_text);
        if let Some(l) = label_id {
            f = f.labelled_by(l, label);
        }
        if let Some(w) = width {
            f = f.width(w);
        }
        let mut out = f.show(ui, th);
        out.committed = false;
        let at_focus = edit.map(|e| e.at_focus);
        if out.response.has_focus() {
            let mut at_focus = match at_focus {
                Some(a) => a,
                None if buf == shown => {
                    buf = plain.clone();
                    plain
                }
                None => shown,
            };
            let (up, down, mult) = ui.input(|i| {
                (
                    i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::ArrowDown),
                    crate::numparse::scrub_factor(i.modifiers.shift, i.modifiers.alt),
                )
            });
            if up || down {
                let k = if up { 1.0 } else { -1.0 } * mult;
                let base = if buf == at_focus {
                    *value
                } else {
                    parse_number(&buf, format).unwrap_or(*value)
                };
                let next = (base + k * step).clamp(min, max);
                if next != *value {
                    *value = next;
                    out.committed = true;
                    out.changed = true;
                }
                buf = format_number(*value, decimals, plain_format);
                at_focus = buf.clone();
            }
            let edit = NumberEdit {
                text: buf,
                at_focus,
                pass,
            };
            ui.data_mut(|d| d.insert_temp(buf_id, edit));
        } else {
            ui.data_mut(|d| d.remove_temp::<NumberEdit>(buf_id));
            if let Some(at_focus) = at_focus {
                if !out.cancelled && buf != at_focus {
                    match parse_number(&buf, format) {
                        Some(v) => {
                            let v = v.clamp(min, max);
                            if v != *value {
                                *value = v;
                                out.committed = true;
                            }
                        }
                        None if ui.input(|i| i.key_pressed(Key::Enter)) => {
                            // stays open with the error: a dialog must not read this Enter as
                            // its primary action and run with the old value
                            ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
                            out.response.request_focus();
                            let edit = NumberEdit {
                                text: buf,
                                at_focus,
                                pass,
                            };
                            ui.data_mut(|d| d.insert_temp(buf_id, edit));
                        }
                        None => out.cancelled = true,
                    }
                }
            }
        }
        if out.step != 0 {
            let mult = ui.input(|i| crate::numparse::scrub_factor(i.modifiers.shift, i.modifiers.alt));
            let k = f64::from(out.step) * mult;
            let next = (*value + k * step).clamp(min, max);
            if next != *value {
                *value = next;
                out.committed = true;
                out.changed = true;
            }
            // the field shows the stepped value, not text typed before the press
            ui.data_mut(|d| d.remove_temp::<NumberEdit>(buf_id));
        }
        out
    }
}

/// Minimum widget height of `ui` equals the density control height (egui reads `interact_size.y`).
fn set_control_h(ui: &mut Ui, th: &Theme) {
    ui.spacing_mut().interact_size.y = th.control_h(ControlSize::Medium);
}

/// Popup rows of a dropdown, each `control_h` tall. Returns the clicked index.
#[cfg(test)]
fn dropdown_rows(ui: &mut Ui, th: &Theme, selected: usize, items: &[&str]) -> Option<usize> {
    dropdown_rows_marked(ui, th, selected, items, None)
}

/// Mark painter of a dropdown row: gets the row index and paints in front of the label.
pub type RowMark<'a> = &'a dyn Fn(&mut Ui, usize);

fn dropdown_rows_marked(
    ui: &mut Ui,
    th: &Theme,
    selected: usize,
    items: &[&str],
    mark: Option<RowMark<'_>>,
) -> Option<usize> {
    set_control_h(ui, th);
    let mut picked = None;
    for (i, item) in items.iter().enumerate() {
        let clicked = match mark {
            Some(paint) => ui
                .horizontal(|ui| {
                    paint(ui, i);
                    ui.selectable_label(i == selected, *item).clicked()
                })
                .inner,
            None => ui.selectable_label(i == selected, *item).clicked(),
        };
        if clicked {
            picked = Some(i);
        }
    }
    picked
}

/// Dropdown over string items. Returns the response; `changed()` when the selection moved.
pub fn dropdown(
    ui: &mut Ui,
    th: &Theme,
    id_salt: impl std::hash::Hash,
    selected: &mut usize,
    items: &[&str],
    width: Option<f32>,
) -> Response {
    dropdown_h(ui, th, id_salt, selected, items, width, None)
}

/// As `dropdown`; `row_h` sets the closed control's height to the row it sits in (a tool
/// options bar), None uses the theme control height. Popup rows always use the theme height.
pub fn dropdown_h(
    ui: &mut Ui,
    th: &Theme,
    id_salt: impl std::hash::Hash,
    selected: &mut usize,
    items: &[&str],
    width: Option<f32>,
    row_h: Option<f32>,
) -> Response {
    dropdown_marked(ui, th, id_salt, selected, items, width, row_h, None)
}

/// As `dropdown_h`; `mark` paints a swatch in front of every popup row.
#[allow(clippy::too_many_arguments)]
pub fn dropdown_marked(
    ui: &mut Ui,
    th: &Theme,
    id_salt: impl std::hash::Hash,
    selected: &mut usize,
    items: &[&str],
    width: Option<f32>,
    row_h: Option<f32>,
    mark: Option<RowMark<'_>>,
) -> Response {
    dropdown_shown(ui, th, id_salt, selected, items, width, row_h, mark, None)
}

/// As `dropdown_marked`; `closed` is the shorter text the closed control shows for the
/// selected item, its tooltip stays the full item text.
#[allow(clippy::too_many_arguments)]
pub fn dropdown_shown(
    ui: &mut Ui,
    th: &Theme,
    id_salt: impl std::hash::Hash,
    selected: &mut usize,
    items: &[&str],
    width: Option<f32>,
    row_h: Option<f32>,
    mark: Option<RowMark<'_>>,
    closed: Option<&str>,
) -> Response {
    let w = width.unwrap_or(control::DROPDOWN_W);
    let full = items.get(*selected).copied().unwrap_or("");
    let cur = closed.unwrap_or(full);
    let mut picked = None;
    let inner = ui
        .scope(|ui| {
            match row_h {
                Some(h) => ui.spacing_mut().interact_size.y = h,
                None => set_control_h(ui, th),
            }
            // tight frame: a closed list in a narrow column keeps more room for its text
            ui.spacing_mut().button_padding.x = space::SM;
            ui.spacing_mut().icon_spacing = space::XS;
            // a truncated text is cut to the room left, then framed: keep the frame within `w`.
            // The frame already insets the text by the padding, so no padding is taken here.
            ui.set_max_width(ui.available_width().min(w).max(0.0));
            // chevron glyph 12 in a 16 slot (spec 2.8)
            ui.spacing_mut().icon_width = 16.0;
            ui.spacing_mut().combo_width = w;
            // egui grows a short combo frame at the bottom only: an even vertical padding keeps
            // the closed text on the control's middle line
            let font = egui::TextStyle::Button.resolve(ui.style());
            let inner = ui.fonts(|f| f.row_height(&font)).max(ui.spacing().icon_width);
            let pad_y = ((ui.spacing().interact_size.y - inner) / 2.0).max(0.0);
            ui.spacing_mut().button_padding.y = pad_y;
            let ink = th.sem().text_secondary;
            egui::ComboBox::from_id_salt(id_salt)
                .width(w)
                .icon(move |ui, r, _, _| down_chevron(ui.painter(), r.center(), ink))
                .truncate()
                .selected_text(cur)
                .show_ui(ui, |ui| {
                    // the popup has its own style, so the row height is set again here
                    picked = dropdown_rows_marked(ui, th, *selected, items, mark);
                })
        })
        .inner;
    let mut resp = inner.response;
    if let Some(i) = picked {
        if i != *selected {
            *selected = i;
            resp.mark_changed();
        }
    }
    let text_w = ui
        .painter()
        .layout_no_wrap(cur.to_owned(), th.font(Role::Body), th.sem().text_primary)
        .size()
        .x;
    // frame, arrow and gaps take about 40 points (measured on a 136 wide column): wider text
    // is elided, so show it on hover
    if cur != full || text_w + 2.0 * space::LG + space::SM > w {
        resp.on_hover_text(full)
    } else {
        resp
    }
}

/// Slider geometry (spec 2.9), points.
pub mod slider_geom {
    pub const TRACK_H: f32 = 4.0;
    pub const THUMB: f32 = 16.0;
    pub const THUMB_HOT: f32 = 18.0;
    /// Linked numeric field to the right of the track.
    pub const FIELD_W: f32 = 56.0;
}

/// New value after `key` on a slider: arrows move one `step`, PgUp/PgDn ten, Home/End jump to
/// the ends. `None` for any other key.
pub fn slider_key(key: Key, value: f64, step: f64, min: f64, max: f64) -> Option<f64> {
    let v = match key {
        Key::ArrowRight | Key::ArrowUp => value + step,
        Key::ArrowLeft | Key::ArrowDown => value - step,
        Key::PageUp => value + 10.0 * step,
        Key::PageDown => value - 10.0 * step,
        Key::Home => min,
        Key::End => max,
        _ => return None,
    };
    Some(v.clamp(min, max))
}

/// Value under pointer `x` on a track spanning `left..right`, snapped to `step`.
pub fn slider_value_at(x: f32, left: f32, right: f32, min: f64, max: f64, step: f64) -> f64 {
    let t = if right > left {
        f64::from(((x - left) / (right - left)).clamp(0.0, 1.0))
    } else {
        0.0
    };
    let raw = min + t * (max - min);
    if step > 0.0 {
        (min + ((raw - min) / step).round() * step).clamp(min, max)
    } else {
        raw
    }
}

/// Track and thumb only; the caller sizes it. Returns true when the value moved.
pub fn slider_track(
    ui: &mut Ui,
    th: &Theme,
    id: Id,
    width: f32,
    value: &mut f64,
    range: (f64, f64),
    step: f64,
) -> (Response, bool) {
    let s = th.sem();
    let (min, max) = range;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, th.d().control), Sense::hover());
    let enabled = ui.is_enabled();
    let sense = if enabled { Sense::click_and_drag() } else { Sense::hover() };
    let resp = ui.interact(rect, id, sense);
    let half = slider_geom::THUMB / 2.0;
    let (left, right) = (rect.left() + half, rect.right() - half);
    let mut moved = false;
    if enabled {
        if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
            let v = slider_value_at(p.x, left, right, min, max, step);
            if v != *value {
                *value = v;
                moved = true;
            }
            resp.request_focus();
        }
        if resp.has_focus() {
            let key = ui.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key { key, pressed: true, .. } => Some(*key),
                    _ => None,
                })
            });
            if let Some(v) = key.and_then(|k| slider_key(k, *value, step, min, max)) {
                if v != *value {
                    *value = v;
                    moved = true;
                }
            }
        }
    }
    let t = if max > min { ((*value - min) / (max - min)).clamp(0.0, 1.0) as f32 } else { 0.0 };
    let cx = left + t * (right - left);
    let cy = rect.center().y;
    let track = Rect::from_min_max(
        egui::pos2(left, cy - slider_geom::TRACK_H / 2.0),
        egui::pos2(right, cy + slider_geom::TRACK_H / 2.0),
    );
    let p = ui.painter();
    let (fill, thumb_line) = if enabled {
        (s.accent, s.border_control)
    } else {
        (s.border_subtle, s.border_subtle)
    };
    p.rect_filled(track, th.radius("pill"), s.bg_sunken);
    p.rect_filled(
        Rect::from_min_max(track.min, egui::pos2(cx, track.max.y)),
        th.radius("pill"),
        fill,
    );
    // the thumb grows by animation, not by jump; reduced motion is instant
    let hot = enabled && (resp.hovered() || resp.dragged());
    let grow = if th.reduced_motion {
        f32::from(u8::from(hot))
    } else {
        ui.ctx().animate_bool_with_time(id.with("hot"), hot, crate::theme::MOTION_FAST_S)
    };
    let d = slider_geom::THUMB + (slider_geom::THUMB_HOT - slider_geom::THUMB) * grow;
    let thumb = Rect::from_center_size(egui::pos2(cx, cy), Vec2::splat(d));
    p.circle_filled(thumb.center(), d / 2.0, s.bg_elevated);
    p.circle_stroke(thumb.center(), d / 2.0, Stroke::new(stroke::CONTROL, thumb_line));
    if resp.has_focus() && super::keyboard_focus(ui.ctx()) {
        super::own_focus(ui.ctx(), id);
        th.paint_focus_ring(p, Rect::from_center_size(thumb.center(), Vec2::splat(slider_geom::THUMB)), 8.0);
    }
    let name = format!("{value}");
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, enabled, name.as_str()));
    (resp, moved)
}

/// Slider with its linked numeric field (56 wide) to the right; both edit the same value.
#[allow(clippy::too_many_arguments)]
pub fn slider(
    ui: &mut Ui,
    th: &Theme,
    salt: impl std::hash::Hash + Copy,
    value: &mut f64,
    range: (f64, f64),
    step: f64,
    decimals: usize,
    format: NumberFormat,
) -> bool {
    let id = ui.id().with(salt);
    let gap = space::SM;
    let track_w = (ui.available_width() - slider_geom::FIELD_W - gap).max(slider_geom::THUMB * 2.0);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        let (_, moved) = slider_track(ui, th, id.with("track"), track_w, value, range, step);
        changed |= moved;
        let out = NumberField::new((salt, "num"), value, format)
            .decimals(decimals)
            .step(step)
            .range(range.0, range.1)
            .width(slider_geom::FIELD_W)
            .show(ui, th);
        changed |= out.changed || out.committed;
    });
    changed
}

#[cfg(test)]
mod dropdown_tests {
    use super::*;
    use crate::{Density, Mode};

    const DENSITIES: [Density; 2] = [Density::Compact, Density::Comfortable];

    fn run_field(
        th: &Theme,
        ctx: &egui::Context,
        input: egui::RawInput,
        search: bool,
        buf: &mut String,
    ) -> (Rect, Rect) {
        let mut out = (Rect::NOTHING, Rect::NOTHING);
        let _ = ctx.run(input, |c| {
            egui::CentralPanel::default().show(c, |ui| {
                let mut f = TextField::new(buf).id(Id::new("f")).width(200.0);
                if search {
                    f = f.search("find", space::XS, space::XS + 16.0 + space::SM, "Clear");
                }
                let o = f.show(ui, th);
                out = (o.rect, o.response.rect);
            });
        });
        out
    }

    /// The search field's widget rect is the whole frame, glyph included, not the text part.
    #[test]
    fn search_field_rect_includes_the_icon() {
        let th = Theme::new(Mode::Light, Density::Compact);
        let ctx = egui::Context::default();
        let mut buf = String::new();
        let _ = run_field(&th, &ctx, Default::default(), true, &mut buf);
        let (frame, widget) = run_field(&th, &ctx, Default::default(), true, &mut buf);
        assert!((widget.left() - frame.left()).abs() < 0.5, "{frame:?} {widget:?}");
        assert!((widget.right() - frame.right()).abs() < 0.5, "{frame:?} {widget:?}");
    }

    /// Fields sit on the 4 grid and keep their size when focused.
    #[test]
    fn field_height_on_grid_and_stable_in_focus() {
        let th = Theme::new(Mode::Light, Density::Compact);
        let ctx = egui::Context::default();
        let mut buf = String::from("x");
        let (a, _) = run_field(&th, &ctx, Default::default(), false, &mut buf);
        assert_eq!(a.height() % 4.0, 0.0, "{a:?}");
        let id = Id::new("f");
        ctx.memory_mut(|m| m.request_focus(id));
        let _ = run_field(&th, &ctx, Default::default(), false, &mut buf);
        let (b, w) = run_field(&th, &ctx, Default::default(), false, &mut buf);
        assert_eq!(a, b);
        assert!((w.height() - a.height()).abs() < 0.5);
    }

    #[test]
    fn stepper_width_is_on_grid() {
        assert_eq!(STEPPER_W % 4.0, 0.0);
        assert_eq!(slider_geom::FIELD_W % 4.0, 0.0);
    }

    #[test]
    fn slider_keys_step_page_and_ends() {
        let f = |k| slider_key(k, 50.0, 2.0, 0.0, 100.0);
        assert_eq!(f(Key::ArrowRight), Some(52.0));
        assert_eq!(f(Key::ArrowLeft), Some(48.0));
        assert_eq!(f(Key::PageUp), Some(70.0));
        assert_eq!(f(Key::PageDown), Some(30.0));
        assert_eq!(f(Key::Home), Some(0.0));
        assert_eq!(f(Key::End), Some(100.0));
        assert_eq!(f(Key::A), None);
        assert_eq!(slider_key(Key::PageUp, 95.0, 2.0, 0.0, 100.0), Some(100.0));
    }

    #[test]
    fn slider_pointer_snaps_to_step() {
        assert_eq!(slider_value_at(0.0, 10.0, 110.0, 0.0, 10.0, 1.0), 0.0);
        assert_eq!(slider_value_at(60.0, 10.0, 110.0, 0.0, 10.0, 1.0), 5.0);
        assert_eq!(slider_value_at(500.0, 10.0, 110.0, 0.0, 10.0, 1.0), 10.0);
        assert_eq!(slider_value_at(63.0, 10.0, 110.0, 0.0, 10.0, 1.0), 5.0);
    }

    /// Numbers end at one x with and without the stepper, and a shown unit stays inside the frame.
    #[test]
    fn number_unit_stays_inside_the_frame() {
        let th = Theme::new(Mode::Light, Density::Compact);
        let ctx = egui::Context::default();
        let mut v = 1.0;
        let mut r = Rect::NOTHING;
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    let o = NumberField::new("n", &mut v, NumberFormat::EN)
                        .unit("m2")
                        .width(120.0)
                        .show(ui, &th);
                    r = o.rect;
                });
            });
        }
        assert!((r.width() - 120.0).abs() < 0.5, "{r:?}");
    }

    #[test]
    fn dropdown_height_equals_control_h_each_density() {
        for dens in DENSITIES {
            let th = Theme::new(Mode::Light, dens);
            let ctx = egui::Context::default();
            let mut h = 0.0;
            let _ = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    let mut sel = 0;
                    h = dropdown(ui, &th, "dd", &mut sel, &["a", "b"], None).rect.height();
                });
            });
            assert!((h - th.control_h(ControlSize::Medium)).abs() < 0.5, "{dens:?}: {h}");
        }
    }

    #[test]
    fn dropdown_h_takes_the_row_height() {
        let th = Theme::new(Mode::Light, Density::Comfortable);
        let ctx = egui::Context::default();
        let mut h = 0.0;
        let _ = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                let mut sel = 0;
                h = dropdown_h(ui, &th, "dd", &mut sel, &["a", "b"], None, Some(22.0))
                    .rect
                    .height();
            });
        });
        assert!((h - 22.0).abs() < 0.5, "{h}");
    }

    #[test]
    fn dropdown_rows_height_control_h() {
        for dens in DENSITIES {
            let th = Theme::new(Mode::Light, dens);
            let ctx = egui::Context::default();
            let mut hs = Vec::new();
            let _ = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    hs.clear();
                    // measure the rows alone: a central panel's own rect is the whole screen
                    let h = ui
                        .vertical(|ui| {
                            dropdown_rows(ui, &th, 0, &["a", "b", "c"]);
                        })
                        .response
                        .rect
                        .height();
                    hs.push(h / 3.0);
                });
            });
            let ch = th.control_h(ControlSize::Medium);
            let sp = ctx.style().spacing.item_spacing.y;
            // three rows plus two gaps fill the used height
            assert!((hs[0] * 3.0 - (3.0 * ch + 2.0 * sp)).abs() < 1.0, "{dens:?}: {hs:?}");
        }
    }
}
