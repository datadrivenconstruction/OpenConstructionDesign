// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Formula cell (TOOLS_UI §4): `=`-prefixed text field, red frame and message on error,
//! value preview otherwise. The caller validates (the expression engine lives in ddc-geom).
use crate::sem::space;
use crate::theme::Role;
use crate::Theme;
use egui::{Align2, Color32, Sense, Stroke, StrokeKind, Ui, Vec2};

/// Result of validating the text in a formula cell.
#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    /// No formula (the value is typed directly).
    Empty,
    /// Valid: preview of the evaluated value.
    Ok(String),
    /// Invalid: byte position of the problem (if known) and the message.
    Err { pos: Option<usize>, msg: String },
}

impl Check {
    pub fn is_err(&self) -> bool {
        matches!(self, Check::Err { .. })
    }
}

/// Message line for an error: `msg` plus the offending fragment (`… «^b» …`).
pub fn error_line(src: &str, pos: Option<usize>, msg: &str) -> String {
    let Some(p) = pos.filter(|&p| p < src.len() && src.is_char_boundary(p)) else {
        return msg.to_string();
    };
    let frag: String = src[p..].chars().take(12).collect();
    format!("{msg} — «{}»", frag.trim_end())
}

/// Formula field of `width` points; returns the text edit response. The frame is red and the
/// error text sits under the field when `check` is an error; a valid formula shows `= value`.
pub fn formula(
    ui: &mut Ui,
    th: &Theme,
    id_salt: impl std::hash::Hash,
    buf: &mut String,
    width: f32,
    check: &Check,
) -> egui::Response {
    let err = th.c("error");
    let resp = ui
        .vertical(|ui| {
            // half the smallest step: the check line sits right under the field
            ui.spacing_mut().item_spacing.y = space::XXS * 0.5;
            let h = th.d().control;
            let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
            let p = ui.painter();
            let bad = check.is_err();
            p.rect_filled(rect, th.radius("sm"), th.c("bg.sunken"));
            let stroke = if bad {
                Stroke::new(1.5, err)
            } else {
                Stroke::new(1.0, th.c("border.subtle"))
            };
            p.rect_stroke(rect, th.radius("sm"), stroke, StrokeKind::Inside);
            p.text(
                rect.left_center() + Vec2::new(7.0, 0.0),
                Align2::LEFT_CENTER,
                "=",
                th.font(Role::Numeric),
                th.c("text.secondary"),
            );
            let inner = egui::Rect::from_min_max(
                rect.min + Vec2::new(18.0, 0.0),
                rect.max - Vec2::new(4.0, 0.0),
            );
            let r = ui.put(
                inner,
                egui::TextEdit::singleline(buf)
                    .id_salt(id_salt)
                    .frame(false)
                    // the edit fills the frame's height: its text sits on the frame's middle
                    .margin(egui::Margin::symmetric(space::XS as i8, 0))
                    .vertical_align(egui::Align::Center)
                    .min_size(Vec2::new(0.0, inner.height()))
                    .font(th.font(Role::Numeric))
                    .text_color(if bad { err } else { th.c("text.primary") })
                    .desired_width(inner.width()),
            );
            let (text, col): (String, Color32) = match check {
                Check::Empty => (String::new(), th.c("text.secondary")),
                Check::Ok(v) => (format!("= {v}"), th.c("text.secondary")),
                Check::Err { pos, msg } => (error_line(buf, *pos, msg), err),
            };
            if !text.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(text)
                            .font(th.font(Role::Caption))
                            .color(col),
                    )
                    .truncate(),
                );
            }
            r
        })
        .inner;
    if let Check::Err { msg, .. } = check {
        resp.on_hover_text(msg.clone())
    } else {
        resp
    }
}

/// Muted caption under a field bound to a formula: `name = formula = value`.
pub fn bound_caption(ui: &mut Ui, th: &Theme, name: &str, formula: &str, value: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(format!("{name} = {formula} = {value}"))
                .font(th.font(Role::Caption))
                .color(th.c("text.secondary")),
        )
        .truncate(),
    );
}

/// Name suggestions under a field. Up/Down move `sel`; returns the index chosen with Enter.
/// The caller applies it to the text and keeps Enter from also committing the field.
pub fn suggestions(ui: &mut Ui, th: &Theme, items: &[String], sel: &mut usize) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    *sel = (*sel).min(items.len() - 1);
    ui.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
            *sel = (*sel + 1) % items.len();
        }
        if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
            *sel = (*sel + items.len() - 1) % items.len();
        }
    });
    for (k, it) in items.iter().enumerate() {
        let col = if k == *sel { th.c("text.primary") } else { th.c("text.secondary") };
        ui.add(
            egui::Label::new(egui::RichText::new(it).font(th.font(Role::Numeric)).color(col))
                .truncate(),
        );
    }
    ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
        .then_some(*sel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_line_points_at_fragment() {
        assert_eq!(
            error_line("Width + 2", Some(8), "length + number"),
            "length + number — «2»"
        );
        assert_eq!(error_line("a", Some(9), "x"), "x");
        assert_eq!(error_line("a", None, "x"), "x");
        // multi-byte names: position inside a char is ignored, not a panic
        assert_eq!(error_line(crate::test_fx::fx("width"), Some(1), "x"), "x");
    }

    #[test]
    fn check_is_err() {
        assert!(Check::Err {
            pos: None,
            msg: "x".into()
        }
        .is_err());
        assert!(!Check::Ok("1".into()).is_err() && !Check::Empty.is_err());
    }
}
