//! The panel's call shapes over the shared widgets (`ddc_ui::widgets`) and the dialog scaffold
//! (`ui_kit`). Nothing is drawn here except what the shared kit has no widget for: a caption,
//! the hover row and the second action of an inline error.

use ddc_ui::theme::Role;
use ddc_ui::widgets;
use ddc_ui::Theme;
use egui::{self, Response, RichText, Stroke, StrokeKind, Ui};

pub use ddc_ui::theme::space;
pub use ddc_ui::widgets::surface::spinner;
pub use ddc_ui::widgets::{badge, checkbox, elided_label, loading_state, tab_bar, Tone};

pub use crate::ui_kit::{dialog_footer, DialogSize};

/// The shared button with an `enabled` switch.
pub struct Button<'a> {
    inner: widgets::Button<'a>,
    enabled: bool,
    primary: bool,
    label: &'a str,
}

/// Buttons a headless frame drew: the state tests count primaries and measure widths.
#[cfg(test)]
pub mod probe {
    use std::cell::RefCell;

    #[derive(Clone, Debug)]
    pub struct Seen {
        pub label: String,
        pub primary: bool,
        pub enabled: bool,
        pub right: f32,
        pub top: f32,
        pub bottom: f32,
    }

    thread_local! {
        static LOG: RefCell<Vec<Seen>> = const { RefCell::new(Vec::new()) };
        static FIELD: RefCell<Option<egui::Rect>> = const { RefCell::new(None) };
    }

    /// The composer's visible input rectangle of the last frame.
    pub fn set_field(r: egui::Rect) {
        FIELD.with(|f| *f.borrow_mut() = Some(r));
    }

    pub fn take_field() -> Option<egui::Rect> {
        FIELD.with(|f| f.borrow_mut().take())
    }

    pub(super) fn push(s: Seen) {
        LOG.with(|l| l.borrow_mut().push(s));
    }

    /// Everything drawn since the last call.
    pub fn take() -> Vec<Seen> {
        LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }
}

impl<'a> Button<'a> {
    fn of(inner: widgets::Button<'a>, label: &'a str, primary: bool) -> Self {
        Self {
            inner,
            enabled: true,
            primary,
            label,
        }
    }

    pub fn primary(label: &'a str) -> Self {
        Self::of(widgets::Button::primary(label), label, true)
    }

    pub fn secondary(label: &'a str) -> Self {
        Self::of(widgets::Button::secondary(label), label, false)
    }

    pub fn quiet(label: &'a str) -> Self {
        Self::of(widgets::Button::quiet(label), label, false)
    }

    pub fn danger(label: &'a str) -> Self {
        Self::of(widgets::Button::danger(label), label, false)
    }

    pub fn small(mut self) -> Self {
        self.inner = self.inner.small();
        self
    }

    pub fn min_width(mut self, w: f32) -> Self {
        self.inner = self.inner.min_width(w);
        self
    }

    pub fn full_width(mut self) -> Self {
        self.inner = self.inner.full_width();
        self
    }

    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }

    pub fn tooltip(mut self, desc: &'a str, keys: &'a str) -> Self {
        self.inner = self.inner.tooltip(desc, keys);
        self
    }

    /// Width with the whole label on one line.
    pub fn width(&self, ui: &Ui, th: &Theme) -> f32 {
        self.inner.width(ui, th)
    }

    pub fn show(self, ui: &mut Ui, th: &Theme) -> Response {
        let Self {
            inner,
            enabled,
            primary,
            label,
        } = self;
        // the enabled scope is a child Ui, so the wrapping row never sees the whole width:
        // start the next line here when the label would not fit the rest of this one
        if ui.layout().main_wrap
            && ui.cursor().left() > ui.max_rect().left() + 0.5
            && inner.width(ui, th)
                > ui.max_rect().right().min(ui.clip_rect().right()) - ui.cursor().left()
        {
            ui.end_row();
        }
        let r = ui.add_enabled_ui(enabled, |ui| inner.show(ui, th)).inner;
        #[cfg(test)]
        probe::push(probe::Seen {
            label: label.to_string(),
            primary,
            enabled,
            right: r.rect.right(),
            top: r.rect.top(),
            bottom: r.rect.bottom(),
        });
        #[cfg(not(test))]
        let _ = (primary, label);
        r
    }
}

pub fn card<R>(ui: &mut Ui, th: &Theme, add: impl FnOnce(&mut Ui) -> R) -> R {
    widgets::card(ui, th, add).inner
}

pub fn caption(ui: &mut Ui, th: &Theme, text: &str) -> Response {
    ui.label(
        RichText::new(text)
            .font(th.font(Role::Caption))
            .color(th.c("text.secondary")),
    )
}

/// Determinate bar in the theme's accent.
pub fn progress(ui: &mut Ui, th: &Theme, fraction: f32, label: &str) {
    widgets::progress(ui, th, Some(fraction), label);
}

/// Title, hint and at most one action; true when the action was clicked.
pub fn empty_state(ui: &mut Ui, th: &Theme, title: &str, hint: &str, action: Option<&str>) -> bool {
    widgets::empty_state(ui, th, None, title, hint, action)
}

/// Inline, not modal: the first action is the shared one, the rest are quiet buttons under it.
pub fn error_state(
    ui: &mut Ui,
    th: &Theme,
    title: &str,
    detail: &str,
    actions: &[&str],
) -> Option<usize> {
    let mut hit =
        widgets::error_state(ui, th, title, detail, actions.first().copied()).then_some(0);
    if actions.len() > 1 {
        ui.horizontal(|ui| {
            for (i, a) in actions.iter().enumerate().skip(1) {
                if Button::quiet(a).small().show(ui, th).clicked() {
                    hit = Some(i);
                }
            }
        });
    }
    hit
}

/// Combo of plain strings under a salt; true when the index changed.
pub fn dropdown(
    ui: &mut Ui,
    th: &Theme,
    salt: &str,
    sel: &mut usize,
    items: &[&str],
    width: f32,
) -> bool {
    // the combo elides against the room left in the row, not its width: hold it to `width`
    ui.scope(|ui| {
        ui.set_max_width(width);
        widgets::dropdown(ui, th, salt, sel, items, Some(width)).changed()
    })
    .inner
}

/// Rows the keyboard cursor or the pointer is on.
pub fn row_highlight(ui: &Ui, th: &Theme, rect: egui::Rect, cursor: bool) {
    let radius = th.radius("sm");
    ui.painter().rect_filled(rect, radius, th.c("bg.hover"));
    if cursor {
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, th.c("accent")),
            StrokeKind::Inside,
        );
    }
}

/// Window of the shared scaffold; `open` closes on the title's cross and on Esc.
pub struct Dialog<'a>(crate::ui_kit::Dialog<'a>);

impl<'a> Dialog<'a> {
    pub fn new(key: &str, title: &str, size: DialogSize) -> Self {
        Self(crate::ui_kit::Dialog::new(key, title, size))
    }

    pub fn open(mut self, open: &'a mut bool) -> Self {
        self.0 = self.0.open(open);
        self
    }

    pub fn scroll_body(mut self) -> Self {
        self.0 = self.0.scroll_body();
        self
    }

    pub fn show(self, ctx: &egui::Context, add: impl FnOnce(&mut Ui)) {
        let _ = self.0.show(ctx, add);
    }
}

pub fn section_title(ui: &mut Ui, _th: &Theme, text: &str) {
    crate::ui_kit::section_title(ui, text);
}

pub fn error_note(ui: &mut Ui, _th: &Theme, text: &str) {
    crate::ui_kit::error_note(ui, text);
}

pub fn warn_note(ui: &mut Ui, _th: &Theme, text: &str) {
    crate::ui_kit::warn_note(ui, text);
}

/// `Some(true)` when the focused widget sits inside `rect`, `Some(false)` when it is elsewhere,
/// `None` when nothing has focus.
pub fn focus_inside(ui: &Ui, rect: egui::Rect) -> Option<bool> {
    let id = ui.ctx().memory(|m| m.focused())?;
    let hit = ui.ctx().read_response(id)?;
    Some(rect.contains(hit.rect.center()))
}
