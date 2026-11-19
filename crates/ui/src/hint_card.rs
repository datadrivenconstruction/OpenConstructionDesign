// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Hint card: the keys of the current step and the values they use. The host builds
//! the rows from one table; this widget knows nothing about the editor. Colours come from the
//! egui style the tokens already set, so the card follows light/dark and density.
use egui::{Frame, Margin, RichText, Ui};

/// Widest the card may be, in points (a narrow phone-class window).
pub const MAX_W: f32 = 320.0;

/// One key line: the chord as the user reads it and what it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintRow {
    pub chord_label: String,
    pub text: String,
}

/// One current value: «Snap: end».
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueRow {
    pub label: String,
    pub value: String,
}

/// Draw the card; returns its rect. Empty input draws nothing.
pub fn draw(ui: &mut Ui, rows: &[HintRow], values: &[ValueRow], refusal: Option<&str>) -> Option<egui::Rect> {
    if rows.is_empty() && values.is_empty() && refusal.is_none() {
        return None;
    }
    let v = ui.visuals().clone();
    let fill = v.window_fill.gamma_multiply(0.94);
    let r = Frame::new()
        .fill(fill)
        .stroke(v.window_stroke)
        .corner_radius(6.0)
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_max_width(MAX_W - 16.0);
            if let Some(t) = refusal {
                ui.label(RichText::new(t).color(v.error_fg_color).strong());
            }
            for h in rows {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&h.chord_label).monospace().strong());
                    ui.label(&h.text);
                });
            }
            for x in values {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("{}:", x.label)).color(v.weak_text_color()));
                    ui.label(&x.value);
                });
            }
        })
        .response
        .rect;
    Some(r)
}

/// Text of the compact form (status line, tooltip): `Click — next point · Esc — cancel`.
pub fn line(rows: &[HintRow]) -> String {
    rows.iter().map(|r| format!("{} — {}", r.chord_label, r.text)).collect::<Vec<_>>().join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(c: &str, t: &str) -> HintRow {
        HintRow { chord_label: c.into(), text: t.into() }
    }

    /// The widest realistic card (long Russian labels) stays inside 320 points.
    #[test]
    fn layout_fits_320px() {
        use crate::test_fx::fx;
        let rows = vec![
            row(fx("hc_k1"), fx("hc_t1")),
            row(fx("hc_k2"), fx("hc_t2")),
            row("A", fx("hc_t3")),
            row("C", fx("hc_t4")),
            row("Esc", fx("hc_t5")),
        ];
        let vals = vec![
            ValueRow { label: fx("hc_l1").into(), value: fx("hc_v1").into() },
            ValueRow { label: fx("hc_l2").into(), value: fx("hc_v2").into() },
        ];
        let ctx = egui::Context::default();
        let mut got = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                got = draw(ui, &rows, &vals, Some(fx("hc_warn")));
            });
        });
        let r = got.expect("card drawn");
        assert!(r.width() <= MAX_W + 0.5, "card is {} wide", r.width());
        assert!(r.height() > 0.0);
        assert_eq!(draw_none(), None);
    }

    fn draw_none() -> Option<egui::Rect> {
        let ctx = egui::Context::default();
        let mut got = Some(egui::Rect::NOTHING);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| got = draw(ui, &[], &[], None));
        });
        got
    }

    #[test]
    fn line_joins_rows() {
        assert_eq!(line(&[row("Esc", "cancel"), row("A", "arc")]), "Esc — cancel · A — arc");
    }
}
