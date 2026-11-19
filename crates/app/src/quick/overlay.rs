//! Strips over the view: recent commands, first-run tips. Undo after a command is a regular
//! toast tied to its history entry (`quick::track_runs`).
use crate::shell::EditorApp;
use ddc_ui::frecency;

pub const BAR_MAX: usize = 6;

/// How long recent commands stay above the status bar after a command runs.
pub const RECENT_SECS: f64 = 4.0;

/// Recent commands strip: shown for a while after a command and while hovered, never resting
/// over the drawing.
#[derive(Clone, Debug, Default)]
pub struct Recent {
    seq: Option<u64>,
    at: Option<f64>,
    /// The status bar's recent button is hovered or focused this frame.
    pub hold: bool,
    /// Where the strip was drawn last: hovering it keeps it.
    rect: Option<egui::Rect>,
}

impl Recent {
    /// Usage number this frame; a new one (a command ran) starts the timer. The first one seen
    /// is the stored history, not a run.
    pub fn note(&mut self, seq: u64, now: f64) {
        if self.seq.is_some_and(|s| s != seq) {
            self.at = Some(now);
        }
        self.seq = Some(seq);
    }

    pub fn visible(&self, now: f64, hovered: bool) -> bool {
        hovered || self.hold || self.at.is_some_and(|t| now - t < RECENT_SECS)
    }

    /// Seconds until the timer hides the strip.
    pub fn left(&self, now: f64) -> Option<f64> {
        self.at
            .map(|t| RECENT_SECS - (now - t))
            .filter(|s| *s > 0.0)
    }
}

/// Recent commands right above the status bar: a click runs one, the one Enter repeats carries
/// its key. Hidden while a tool or a window owns the input.
pub fn recent_bar(
    app: &mut EditorApp,
    ctx: &egui::Context,
    area: egui::Rect,
    fire: &mut Vec<String>,
) {
    use ddc_ui::theme::{space, stroke, Role};
    use ddc_ui::widgets;
    let now = ctx.input(|i| i.time);
    let seq = frecency::with(|u| u.seq);
    let r = &mut app.sbar.recent;
    r.note(seq, now);
    let hold = std::mem::take(&mut r.hold);
    let hovered = r
        .rect
        .zip(ctx.pointer_hover_pos())
        .is_some_and(|(rect, p)| rect.expand(space::SM).contains(p));
    let wanted = (hold || r.visible(now, hovered)) && (hold || !app.quick.ux.bar_off);
    let ids: Vec<String> = frecency::with(|u| u.recent(BAR_MAX))
        .into_iter()
        .filter(|i| app.sh.reg.get(i).is_some())
        .collect();
    let show = wanted && super::idle(app) && !ids.is_empty();
    let fade = ctx.animate_bool_with_time(
        egui::Id::new("ddc.quick.bar.fade"),
        show,
        ddc_ui::theme::MOTION_FAST_S,
    );
    if let Some(s) = app.sbar.recent.left(now).filter(|_| show) {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(s));
    }
    if fade <= 0.0 || ids.is_empty() {
        app.sbar.recent.rect = None;
        return;
    }
    let repeat = frecency::with(|u| u.last_repeatable(&app.sh.reg).map(String::from));
    let enter = super::keys_text(app, "app.repeat");
    // (id, chip label, tooltip title, tooltip keys, repeated by Enter)
    let rows: Vec<(String, String, String, String, bool)> = ids
        .iter()
        .map(|id| {
            let name = super::label(app, id);
            let rep = repeat.as_deref() == Some(id.as_str());
            let (tip, keys) = if rep {
                let t = app.sh.cat.tf("quick.bar.repeat_tip", &[("cmd", &name)]);
                (t, enter.clone())
            } else {
                (name.clone(), super::keys_text(app, id))
            };
            (id.clone(), short(&name, 18), tip, keys, rep)
        })
        .collect();
    let th = &app.sh.theme;
    let head = app.sh.cat.t("quick.bar.recent");
    let out = egui::Area::new(egui::Id::new("ddc.quick.bar"))
        .order(egui::Order::Middle)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .fixed_pos(egui::pos2(
            area.left() + space::SM,
            area.bottom() - space::SM,
        ))
        .show(ctx, |ui| {
            ui.set_opacity(fade);
            egui::Frame::new()
                .fill(th.c("bg.elevated"))
                .stroke(egui::Stroke::new(stroke::HAIRLINE, th.c("border.subtle")))
                .corner_radius(th.radius("md"))
                .shadow(th.shadow(1))
                .inner_margin(egui::Margin::same(space::XS as i8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::XXS;
                        ui.add_space(space::XS);
                        ui.label(
                            egui::RichText::new(head)
                                .font(th.font(Role::Caption))
                                .color(th.c("text.secondary")),
                        );
                        ui.add_space(space::XS);
                        for (id, name, tip, keys, rep) in &rows {
                            let g = ui.painter().layout_no_wrap(
                                name.clone(),
                                th.font(Role::Body),
                                th.c("text.primary"),
                            );
                            let kw = if *rep {
                                space::XS + keycaps_w(ui, th, &enter)
                            } else {
                                0.0
                            };
                            let w = space::SM + g.size().x + kw + space::SM;
                            let (r, resp) = ui.allocate_exact_size(
                                egui::vec2(w, space::XL),
                                egui::Sense::click(),
                            );
                            let fg = widgets::paint_state(ui, th, r, &resp, false);
                            let fg = if *rep { th.c("text.primary") } else { fg };
                            let cy = r.center().y;
                            let gy = cy - g.size().y / 2.0;
                            ui.painter()
                                .galley(egui::pos2(r.left() + space::SM, gy), g, fg);
                            if *rep {
                                let right = egui::pos2(r.right() - space::SM, cy);
                                widgets::paint_keycaps(ui.painter(), th, right, &enter);
                            }
                            let resp = widgets::rich_tooltip(resp, th, tip, "", keys);
                            if resp.clicked() {
                                fire.push(id.clone());
                            }
                        }
                    });
                });
        });
    app.sbar.recent.rect = Some(out.response.rect);
}

/// Width `paint_keycaps` takes for `keys`.
fn keycaps_w(ui: &egui::Ui, th: &ddc_ui::Theme, keys: &str) -> f32 {
    use ddc_ui::theme::{control, space, Role};
    let caps: Vec<f32> = keys
        .split(['+', ' '])
        .filter(|k| !k.is_empty())
        .map(|k| {
            let g = ui.painter().layout_no_wrap(
                k.to_string(),
                th.font(Role::Caption),
                th.c("text.secondary"),
            );
            (g.size().x + space::MD).max(control::KEYCAP_H)
        })
        .collect();
    caps.iter().sum::<f32>() + space::XS * caps.len().saturating_sub(1) as f32
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n - 1).collect();
        format!("{t}…")
    }
}

/// Tips deck: shown after the first-run welcome, until the "Don't show again" option is chosen.
pub fn tips(app: &mut EditorApp, ctx: &egui::Context, area: egui::Rect) {
    // not on top of a tool, the palette, or recovery/warning windows
    if !app.quick.tips.open || !app.prefs.first_run_done || !super::idle(app) {
        return;
    }
    let c = &app.sh.cat;
    let k = app.quick.tips.key();
    let n = ddc_ui::tips::TIPS.len();
    let title = c.t(&format!("tip.{k}.title"));
    let text = c.t(&format!("tip.{k}.text"));
    let count = c.tf(
        "quick.tips.count",
        &[
            ("i", &(app.quick.tips.idx + 1).to_string()),
            ("n", &n.to_string()),
        ],
    );
    let (prev, next, done, head) = (
        c.t("quick.tips.prev"),
        if app.quick.tips.idx + 1 >= n {
            c.t("quick.tips.finish")
        } else {
            c.t("quick.tips.next")
        },
        c.t("quick.tips.never"),
        c.t("quick.tips.head"),
    );
    let mut act = 0;
    egui::Area::new(egui::Id::new("ddc.quick.tips"))
        .order(ddc_ui::z::Z::Canvas.order())
        .fixed_pos(egui::pos2(area.right() - 340.0, area.top() + 12.0).max(area.left_top()))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_max_width(310.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(head).weak().small());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new(count).weak().small());
                        });
                    });
                    ui.label(egui::RichText::new(title).strong());
                    ui.label(text);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.small_button(done).clicked() {
                            act = 3;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button(next).clicked() {
                                act = 2;
                            }
                            if app.quick.tips.idx > 0 && ui.button(prev).clicked() {
                                act = 1;
                            }
                        });
                    });
                });
        });
    let t = &mut app.quick.tips;
    match act {
        1 => t.prev(),
        2 => t.next(),
        3 => t.finish(),
        _ => return,
    }
    if t.done {
        app.quick.ux.tips_done = true;
        app.quick.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn short_labels() {
        assert_eq!(super::short("Wall", 18), "Wall");
        assert_eq!(
            super::short("A very long command name", 10),
            "A very lo\u{2026}"
        );
    }
}
