// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Toast stack, bottom-right above the status bar; never takes focus.
use crate::notify::{Level, Toasts};
use crate::theme::{control, icon_size, layout, space, Role};
use crate::{Catalog, Theme};
use super::{Button, Variant};
use egui::{Align2, Rect, RichText, Sense, Vec2};

const CARD_W: f32 = layout::TOAST_W;
const ICON_W: f32 = icon_size::SM;
const CLOSE_W: f32 = control::HIT_MIN;

/// Icon of a level. The shapes differ (circle with i, check, triangle, cross), so the level
/// is readable without colour.
pub fn toast_icon(level: Level) -> &'static str {
    match level {
        Level::Info => "level-info",
        Level::Success => "level-success",
        Level::Warning => "warning",
        Level::Error => "level-error",
    }
}

/// Catalog key of the level's name (tooltip of the icon).
pub fn level_label_key(level: Level) -> &'static str {
    match level {
        Level::Info => "toast.level.info",
        Level::Success => "toast.level.success",
        Level::Warning => "toast.level.warning",
        Level::Error => "toast.level.error",
    }
}

/// Where toast `i`'s button `which` ("action" or "copy") was drawn last frame.
pub(crate) fn button_rect_id(i: usize, which: &str) -> egui::Id {
    egui::Id::new(("ddc.toast.button", i, which))
}

/// Stable id of toast `i`'s close target.
pub(crate) fn close_id(i: usize) -> egui::Id {
    egui::Id::new(("ddc.toast.close", i))
}

/// Card height used to reserve room above the stack (one text line plus an action row).
pub const CARD_H: f32 = 72.0;
const MORE_H: f32 = 24.0;
const ENTER_S: f32 = 0.2;
const SLIDE_PX: f32 = 8.0;

/// Horizontal shift of the stack at enter `progress` (0..=1): 8 px to the right, none at rest
/// and none at all with reduced motion.
pub fn slide_offset(progress: f32, reduced: bool) -> f32 {
    if reduced {
        0.0
    } else {
        SLIDE_PX * (1.0 - progress.clamp(0.0, 1.0))
    }
}

/// Height of the toast stack for `rows` visible cards, with the "+n more" line when
/// `more` is set. Zero without toasts. Other bottom-right windows stack above this.
pub fn stack_height(rows: usize, more: bool) -> f32 {
    if rows == 0 {
        return 0.0;
    }
    let lines = rows + usize::from(more);
    let gap = space::SM * (lines - 1) as f32;
    rows as f32 * CARD_H + if more { MORE_H } else { 0.0 } + gap
}

fn measured_id() -> egui::Id {
    egui::Id::new("ddc.toasts.measured_h")
}

/// Time of the last aging frame and the wait asked for after it.
fn clock_id() -> egui::Id {
    egui::Id::new("ddc.toasts.clock")
}

/// Repaint a little after the expiry so the frame finds the toast already run out.
const EXPIRY_SLACK: f64 = 0.02;
/// Longest step of an unplanned frame (one that came before the asked wait).
const MAX_STEP: f64 = 0.25;

/// Life to take off the toasts for `elapsed` seconds since the last frame, when that frame
/// asked to be woken after `asked` seconds: a frame later than that (a stall) counts only
/// `asked` plus a short step.
pub fn age_step(elapsed: f64, asked: f64) -> f64 {
    elapsed.clamp(0.0, asked.max(0.0) + MAX_STEP)
}

/// Stack height for stacking other bottom-right windows: the height measured last frame by
/// `show`, or the estimate before the first frame. Zero without toasts.
pub fn stack_height_in(ctx: &egui::Context, rows: usize, more: bool) -> f32 {
    if rows == 0 {
        return 0.0;
    }
    ctx.data(|d| d.get_temp::<f32>(measured_id()))
        .unwrap_or_else(|| stack_height(rows, more))
}

/// Draw and age toasts. Returns what a clicked action runs: the toast's command when it has
/// one, otherwise its label.
pub fn show(
    ctx: &egui::Context,
    th: &Theme,
    cat: &Catalog,
    q: &mut Toasts,
    bottom: f32,
) -> Option<String> {
    if q.live.is_empty() {
        ctx.data_mut(|d| {
            d.remove::<f32>(measured_id());
            d.remove::<(f64, f64)>(clock_id());
        });
        return None;
    }
    let before = q.live.len();
    let mut hovered = None;
    let mut action = None;
    let mut fired: Option<Option<u64>> = None;
    let mut close = None;
    let mut open_history = false;
    let shown = q.visible_idx();
    // a new card slides in 8 px from the right over 200 ms; reduced motion shows it at once
    let enter_id = egui::Id::new(("ddc.toast.enter", q.live.len()));
    // egui jumps to the target on an id's first use: seed it with 0
    if !ctx.data(|d| d.get_temp::<bool>(enter_id).unwrap_or(false)) {
        ctx.data_mut(|d| d.insert_temp(enter_id, true));
        ctx.animate_value_with_time(enter_id, 0.0, 0.0);
    }
    let enter = ctx.animate_value_with_time(
        enter_id,
        1.0,
        if th.reduced_motion { 0.0 } else { ENTER_S },
    );
    let slide = slide_offset(enter, th.reduced_motion);
    let area = egui::Area::new(egui::Id::new("ddc.toasts"))
        .anchor(
            Align2::RIGHT_BOTTOM,
            Vec2::new(-space::MD + slide, -bottom - space::SM),
        )
        .order(crate::z::Z::Toast.order())
        .interactable(true)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = space::SM;
            let more = q.overflow();
            if more > 0 {
                let n = more.to_string();
                let label = cat.tf("toast.more", &[("n", &n)]);
                let b = Button::new(&label, Variant::Quiet).small().show(ui, th);
                if b.on_hover_text(cat.t("toast.more_tip")).clicked() {
                    open_history = true;
                }
            }
            for (i, &li) in shown.iter().enumerate() {
                let t = &q.live[li];
                let col = super::surface::Tone::of_level(t.level).foreground(th.sem());
                let r = egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::symmetric(space::MD as i8, space::SM as i8))
                    .corner_radius(th.radius("lg"))
                    .shadow(th.shadow(2))
                    .show(ui, |ui| {
                        ui.set_width(CARD_W);
                        ui.spacing_mut().item_spacing = Vec2::new(space::SM, space::XS);
                        ui.horizontal_top(|ui| {
                            // the level is told by the icon shape, the colour only backs it up
                            let (mark, mark_resp) = ui
                                .allocate_exact_size(Vec2::new(ICON_W, control::BADGE_H), Sense::hover());
                            let ic = Rect::from_center_size(mark.center(), Vec2::splat(icon_size::SM));
                            let ctx = ui.ctx().clone();
                            super::with_icons(&ctx, |icons| {
                                icons.paint(ui, toast_icon(t.level), icon_size::SM as u32, ic, col)
                            });
                            mark_resp.on_hover_text(cat.t(level_label_key(t.level)));
                            let mut text = t.text.clone();
                            if t.count > 1 {
                                text += &format!("  ×{}", t.count);
                            }
                            ui.scope(|ui| {
                                ui.set_width(CARD_W - ICON_W - CLOSE_W - space::LG);
                                ui.vertical(|ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(text)
                                                .font(th.font(Role::Body))
                                                .color(th.c("text.primary")),
                                        )
                                        .wrap(),
                                    );
                                    if let Some(d) = t.details.as_deref().filter(|d| !d.is_empty()) {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(d)
                                                    .font(th.font(Role::Caption))
                                                    .color(th.c("text.secondary")),
                                            )
                                            .wrap(),
                                        );
                                    }
                                });
                            });
                            let (cr, _) =
                                ui.allocate_exact_size(Vec2::splat(CLOSE_W), Sense::hover());
                            let resp =
                                ui.interact(cr, close_id(i), Sense::click());
                            let fg = super::paint_state(ui, th, cr, &resp, false);
                            let ic = Rect::from_center_size(cr.center(), Vec2::splat(icon_size::SM));
                            let ctx = ui.ctx().clone();
                            super::with_icons(&ctx, |icons| {
                                icons.paint(ui, "close", icon_size::SM as u32, ic, fg)
                            });
                            if resp.on_hover_text(cat.t("toast.dismiss")).clicked() {
                                close = Some(li);
                            }
                        });
                        let copyable = t.level == Level::Error;
                        if t.action.is_some() || copyable {
                            ui.horizontal(|ui| {
                                ui.add_space(ICON_W + space::SM);
                                if let Some(a) = &t.action {
                                    let b = Button::new(a, Variant::Quiet).show(ui, th);
                                    ui.ctx().data_mut(|d| {
                                        d.insert_temp(button_rect_id(i, "action"), b.rect)
                                    });
                                    if b.clicked() {
                                        action =
                                            Some(t.command.clone().unwrap_or_else(|| a.clone()));
                                        fired = Some(t.entry);
                                    }
                                }
                                if copyable {
                                    let label = cat.t("toast.copy");
                                    let b = Button::new(&label, Variant::Quiet).show(ui, th);
                                    ui.ctx()
                                        .data_mut(|d| d.insert_temp(button_rect_id(i, "copy"), b.rect));
                                    if b.clicked() {
                                        ui.ctx().copy_text(t.copy_text());
                                    }
                                }
                            });
                        }
                    });
                if r.response.contains_pointer() {
                    hovered = Some(i);
                }
            }
        });
    let h = area.response.rect.height();
    ctx.data_mut(|d| d.insert_temp(measured_id(), h));
    // Esc over a toast closes only that toast: consumed so a tool or dialog drawn later keeps
    // its state (handlers that ran earlier this frame already saw the key)
    if let Some(k) = hovered {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            close = shown.get(k).copied();
        }
    }
    if let Some(li) = close {
        q.dismiss(li);
        hovered = None;
    }
    // frames come only on input or at the next expiry, so age by the real time since the last
    // frame, capped by what was asked for (a stalled frame does not eat a toast's life)
    let now = ctx.input(|i| i.time);
    let clock = clock_id();
    let (last, asked) = ctx
        .data(|d| d.get_temp::<(f64, f64)>(clock))
        .unwrap_or((now, 0.0));
    q.tick(age_step(now - last, asked), hovered);
    if q.live.len() != before {
        // the expired card was drawn this frame: one more frame takes it off the screen
        ctx.request_repaint();
    }
    let next = q.next_expiry(hovered);
    ctx.data_mut(|d| d.insert_temp(clock, (now, next.unwrap_or(0.0))));
    if let Some(s) = next {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(s + EXPIRY_SLACK));
    }
    if open_history {
        q.release_hidden_errors();
        return Some(crate::notify::HISTORY_CMD.to_string());
    }
    if let Some(f) = fired {
        q.fired_entry = f;
        // a step toast is spent once clicked: a second click must not step another entry
        if let Some(e) = f {
            q.live.retain(|t| t.entry != Some(e));
        }
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::Level;
    use egui::{Event, Key, RawInput};

    #[test]
    fn toast_slides_8px_and_not_with_reduced_motion() {
        assert_eq!(slide_offset(0.0, false), 8.0);
        assert_eq!(slide_offset(1.0, false), 0.0);
        assert_eq!(slide_offset(0.0, true), 0.0);
        assert_eq!(MORE_H, 24.0);
    }

    fn tab() -> RawInput {
        RawInput {
            events: vec![Event::Key {
                key: Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn close_target_is_24_and_both_controls_take_keyboard_focus() {
        let ctx = crate::theme::headless_ctx();
        let mut th = Theme::new(crate::theme::Mode::Light, crate::theme::Density::Compact);
        let cat = Catalog::new(crate::Lang::En);
        let mut q = Toasts::default();
        q.push_step(Level::Info, "done", "Undo", "edit.undo", Some(1));
        let mut seen = Vec::new();
        for i in 0..4 {
            let input = if i == 0 { RawInput::default() } else { tab() };
            let _ = ctx.run(input, |c| {
                th.apply(c);
                show(c, &th, &cat, &mut q, 0.0);
            });
            seen.push(ctx.memory(|m| m.focused()));
        }
        let r = ctx.read_response(close_id(0)).expect("close target drawn");
        assert!(r.rect.width() >= control::HIT_MIN && r.rect.height() >= control::HIT_MIN);
        assert!(r.sense.is_focusable());
        let ids: std::collections::HashSet<_> = seen.iter().flatten().collect();
        assert!(ids.len() >= 2, "tab reaches the close target and the action: {seen:?}");
    }

    #[test]
    fn stack_grows_with_rows() {
        assert_eq!(stack_height(0, true), 0.0);
        assert!(stack_height(3, false) > stack_height(1, false));
        assert!(stack_height(3, true) > stack_height(3, false));
    }

    fn frames(ctx: &egui::Context, q: &mut Toasts) {
        let (th, cat) = (
            Theme::new(crate::Mode::Light, crate::Density::Compact),
            Catalog::new(crate::Lang::En),
        );
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1280.0, 720.0),
            )),
            ..Default::default()
        };
        for _ in 0..3 {
            let _ = ctx.run(input.clone(), |c| {
                show(c, &th, &cat, q, 24.0);
            });
        }
    }

    #[test]
    fn measured_stack_height_two_line_toast() {
        let ctx = crate::theme::headless_ctx();
        let mut q = Toasts::default();
        q.push(crate::notify::Level::Info, "Saved", None);
        frames(&ctx, &mut q);
        let one = stack_height_in(&ctx, 1, false);
        let mut q2 = Toasts::default();
        let long = "A long message that needs several lines to fit inside one toast card ".repeat(3);
        q2.push(crate::notify::Level::Info, long, Some("Undo".into()));
        let ctx2 = crate::theme::headless_ctx();
        frames(&ctx2, &mut q2);
        let two = stack_height_in(&ctx2, 1, false);
        assert!(one > 0.0 && two > one);
        assert_ne!(two, stack_height(1, false));
    }

    #[test]
    fn stack_height_zero_without_toasts() {
        let ctx = crate::theme::headless_ctx();
        let mut q = Toasts::default();
        q.push(crate::notify::Level::Info, "Saved", None);
        frames(&ctx, &mut q);
        q.live.clear();
        frames(&ctx, &mut q);
        assert_eq!(stack_height_in(&ctx, 0, false), 0.0);
        assert_eq!(stack_height_in(&ctx, 1, false), stack_height(1, false));
    }

    const LEVELS: [Level; 4] = [Level::Info, Level::Success, Level::Warning, Level::Error];

    #[test]
    fn every_level_has_a_distinct_existing_icon_and_name() {
        let icons: std::collections::HashSet<_> = LEVELS.iter().map(|l| toast_icon(*l)).collect();
        assert_eq!(icons.len(), LEVELS.len());
        for l in LEVELS {
            let n = toast_icon(l);
            assert!(crate::icons::source(n).is_some(), "{n}");
            assert!(crate::icons::source(&format!("{n}.fill")).is_some(), "{n}.fill");
            assert!(crate::icons::rasterize(n, icon_size::SM as u32).is_ok(), "{n}");
        }
        let cat = Catalog::new(crate::Lang::En);
        let ru = Catalog::new(crate::Lang::Ru);
        for l in LEVELS {
            let k = level_label_key(l);
            assert!(cat.table(crate::Lang::En).contains_key(k), "{k}");
            assert!(ru.table(crate::Lang::Ru).contains_key(k), "{k}");
        }
        assert!(cat.table(crate::Lang::En).contains_key("toast.copy"));
        assert!(ru.table(crate::Lang::Ru).contains_key("toast.copy"));
    }

    #[test]
    fn level_tones_are_distinct_and_readable_in_both_themes() {
        use super::super::surface::Tone;
        for mode in [crate::Mode::Light, crate::Mode::Dark] {
            let th = Theme::new(mode, crate::Density::Compact);
            let bg = th.c("bg.panel");
            let cols: Vec<_> = LEVELS
                .iter()
                .map(|l| Tone::of_level(*l).foreground(th.sem()))
                .collect();
            for (l, c) in LEVELS.iter().zip(&cols) {
                let r = crate::theme::contrast(*c, bg);
                assert!(r >= 3.0, "{mode:?} {l:?}: {r:.2}");
            }
            let uniq: std::collections::HashSet<_> = cols.iter().collect();
            assert_eq!(uniq.len(), LEVELS.len(), "{mode:?}");
        }
    }

    /// Runs frames, then a click at `p`; returns (command of a clicked action, copied text).
    fn click(ctx: &egui::Context, q: &mut Toasts, p: egui::Pos2) -> (Option<String>, Option<String>) {
        let (th, cat) = (
            Theme::new(crate::Mode::Light, crate::Density::Compact),
            Catalog::new(crate::Lang::En),
        );
        let screen = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1280.0, 720.0)));
        let btn = |pressed| Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        let mut got = (None, None);
        for ev in [Event::PointerMoved(p), btn(true), btn(false)] {
            let input = RawInput {
                screen_rect: screen,
                events: vec![ev],
                ..Default::default()
            };
            let out = ctx.run(input, |c| {
                if let Some(a) = show(c, &th, &cat, q, 24.0) {
                    got.0 = Some(a);
                }
            });
            for c in out.platform_output.commands {
                if let egui::OutputCommand::CopyText(s) = c {
                    got.1 = Some(s);
                }
            }
        }
        got
    }

    fn rect_of(ctx: &egui::Context, i: usize, which: &str) -> Option<egui::Rect> {
        ctx.data(|d| d.get_temp::<egui::Rect>(button_rect_id(i, which)))
    }

    #[test]
    fn error_toast_copies_exact_text_with_details() {
        let ctx = crate::theme::headless_ctx();
        let mut q = Toasts::default();
        q.push_details(Level::Error, "Import failed", "line 1\nline 2");
        frames(&ctx, &mut q);
        let r = rect_of(&ctx, 0, "copy").expect("error toast has Copy");
        let (act, copied) = click(&ctx, &mut q, r.center());
        assert_eq!(copied.as_deref(), Some("Import failed\nline 1\nline 2"));
        assert_eq!(act, None);
        assert_eq!(q.live.len(), 1, "copying does not dismiss");
    }

    #[test]
    fn only_errors_get_copy_and_undo_action_still_runs() {
        let ctx = crate::theme::headless_ctx();
        let mut q = Toasts::default();
        q.push_step(Level::Success, "Deleted", "Undo", "edit.undo", Some(7));
        frames(&ctx, &mut q);
        assert!(rect_of(&ctx, 0, "copy").is_none());
        let r = rect_of(&ctx, 0, "action").expect("action drawn");
        let (act, copied) = click(&ctx, &mut q, r.center());
        assert_eq!(act.as_deref(), Some("edit.undo"));
        assert_eq!(q.fired_entry, Some(7), "the host learns which entry to step");
        assert!(q.live.is_empty(), "a clicked step toast is spent");
        assert_eq!(copied, None);

        let ctx = crate::theme::headless_ctx();
        let mut q = Toasts::default();
        q.push_cmd(Level::Error, "Save failed", "Retry", "file.save");
        frames(&ctx, &mut q);
        let a = rect_of(&ctx, 0, "action").unwrap();
        let c = rect_of(&ctx, 0, "copy").unwrap();
        assert!(a.max.x <= c.min.x, "action first, Copy after it");
        let (act, copied) = click(&ctx, &mut q, a.center());
        assert_eq!(act.as_deref(), Some("file.save"));
        assert_eq!(copied, None);
    }

    #[test]
    fn escape_dismisses_the_hovered_toast() {
        let ctx = crate::theme::headless_ctx();
        let mut q = Toasts::default();
        q.push(Level::Error, "bad", None);
        frames(&ctx, &mut q);
        let r = ctx.read_response(close_id(0)).expect("drawn").rect;
        let (th, cat) = (
            Theme::new(crate::Mode::Light, crate::Density::Compact),
            Catalog::new(crate::Lang::En),
        );
        let screen = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1280.0, 720.0)));
        let esc = Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        for events in [vec![Event::PointerMoved(r.center())], vec![esc]] {
            let input = RawInput {
                screen_rect: screen,
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |c| {
                show(c, &th, &cat, &mut q, 24.0);
            });
        }
        assert!(q.live.is_empty());
    }

    /// One idle frame at `time`; returns the repaint the frame asked for.
    fn idle_at(ctx: &egui::Context, q: &mut Toasts, time: f64) -> std::time::Duration {
        let (th, cat) = (
            Theme::new(crate::Mode::Light, crate::Density::Compact),
            Catalog::new(crate::Lang::En),
        );
        let input = RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1280.0, 720.0))),
            time: Some(time),
            ..Default::default()
        };
        let out = ctx.run(input, |c| {
            show(c, &th, &cat, q, 24.0);
        });
        out.viewport_output[&egui::ViewportId::ROOT].repaint_delay
    }

    #[test]
    fn an_error_toast_alone_asks_for_no_repaint() {
        let ctx = crate::theme::headless_ctx();
        let mut q = Toasts::default();
        q.push(Level::Error, "bad", None);
        for i in 0..4 {
            idle_at(&ctx, &mut q, i as f64 * 0.1);
        }
        assert_eq!(idle_at(&ctx, &mut q, 60.0), std::time::Duration::MAX);
        assert_eq!(q.live.len(), 1, "the error stays until dismissed");
    }

    #[test]
    fn a_timed_toast_wakes_once_at_its_expiry_then_stops() {
        let ctx = crate::theme::headless_ctx();
        let mut q = Toasts::default();
        q.push(Level::Info, "Saved", None);
        for i in 0..4 {
            idle_at(&ctx, &mut q, i as f64 * 0.01);
        }
        // the card slides in over ENTER_S and egui keeps an immediate repaint for one frame
        // after the last animated one, so the first idle frame comes one frame after the slide
        let end = f64::from(ENTER_S);
        idle_at(&ctx, &mut q, end);
        let idle = end + 0.05;
        let d = idle_at(&ctx, &mut q, idle).as_secs_f64();
        assert!(d > 3.0 && d <= crate::notify::LIFE + EXPIRY_SLACK, "wake at expiry: {d}");
        // the asked wake comes: the toast is gone and nothing more is asked
        let d = idle_at(&ctx, &mut q, idle + d);
        assert!(q.live.is_empty(), "expired on the planned frame");
        idle_at(&ctx, &mut q, 4.5);
        assert_eq!(idle_at(&ctx, &mut q, 4.6), std::time::Duration::MAX, "after {d:?}");
    }

    #[test]
    fn a_late_frame_ages_by_the_asked_wait_and_a_stall_is_capped() {
        assert_eq!(age_step(0.1, 4.0), 0.1);
        assert_eq!(age_step(4.0, 4.0), 4.0);
        assert_eq!(age_step(30.0, 4.0), 4.0 + MAX_STEP);
        assert_eq!(age_step(30.0, 0.0), MAX_STEP);
        assert_eq!(age_step(-1.0, 1.0), 0.0);
        let mut q = Toasts::default();
        q.push(Level::Info, "a", None);
        q.push(Level::Error, "b", None);
        let left = q.next_expiry(None).unwrap();
        assert!((left - crate::notify::LIFE).abs() < 1e-9);
        // the hovered card is paused; the error never expires
        assert_eq!(q.next_expiry(Some(0)), None);
    }
}
