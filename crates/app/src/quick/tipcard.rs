//! Tips at the right moment: a quiet card in the lower right corner after the first wall, the
//! first selection and the first save. Dismissable, shown once, switched off for good by a button
//! or the command «Tips at the right moment». Logic and texts: `ddc_ui::ctx_tips`.
use crate::shell::EditorApp;
use ddc_ui::ctx_tips::{Probe, Watch};
use ddc_ui::notify::Level;
use std::cell::RefCell;
use std::time::Duration;

thread_local! {
    static WATCH: RefCell<Watch> = RefCell::new(Watch::default());
}

pub fn register(app: &mut EditorApp) {
    app.register_command("app.ctx_tips", toggle);
}

fn toggle(app: &mut EditorApp) {
    let off = !app.quick.ux.ctx_tips.off;
    app.quick.ux.ctx_tips.set_off(off);
    app.quick.dirty = true;
    let key = if off {
        "tip.ctx.state_off"
    } else {
        "tip.ctx.state_on"
    };
    super::toast(app, Level::Info, key, &[]);
}

fn probe(app: &EditorApp) -> Probe {
    let ed = &app.ed;
    Probe {
        project: ed.project_id(),
        level: ed.level,
        walls: ed.walls.len(),
        undo: ed.undo_len(),
        selected: ed.selection.len(),
        saved: ed.project.path().is_some() || app.save.saved_mark.is_some(),
    }
}

/// Once per frame from `quick::frame`: watch for the moments, show the card.
pub fn frame(app: &mut EditorApp, ctx: &egui::Context, area: egui::Rect) {
    let now = ctx.input(|i| i.time);
    let p = probe(app);
    // the intro deck, palette, recovery windows and toasts (same corner) go first
    let free = app.prefs.first_run_done
        && !app.quick.ux.ctx_tips.off
        && !app.quick.tips.open
        && super::idle(app)
        && app.sh.toasts.live.is_empty();
    let next = WATCH.with(|w| {
        let mut w = w.borrow_mut();
        w.step(p);
        if free && app.quick.ux.ctx_tips.current().is_none() {
            w.next(&app.quick.ux.ctx_tips)
        } else {
            None
        }
    });
    if let Some(m) = next {
        if app.quick.ux.ctx_tips.offer(m, now) {
            // stored at once: a tip shown is a tip done, even if the app closes meanwhile
            app.quick.dirty = true;
        }
    }
    app.quick.ux.ctx_tips.expire(now);
    if !free {
        return;
    }
    let Some(m) = app.quick.ux.ctx_tips.current() else {
        return;
    };
    let c = &app.sh.cat;
    let k = m.key();
    let title = c.t(&format!("tip.ctx.{k}.title"));
    let text = c.t(&format!("tip.ctx.{k}.text"));
    let (head, got, off) = (c.t("tip.ctx.head"), c.t("tip.ctx.got_it"), c.t("tip.ctx.off"));
    let mut act = 0;
    let out = egui::Area::new(egui::Id::new("ddc.quick.ctx_tip"))
        .order(ddc_ui::z::Z::Canvas.order())
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .fixed_pos(egui::pos2(area.right() - 12.0, area.bottom() - 12.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_max_width(300.0);
                    ui.label(egui::RichText::new(head).weak().small());
                    ui.label(egui::RichText::new(title).strong());
                    ui.label(text);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button(got).clicked() {
                            act = 1;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(off).clicked() {
                                act = 2;
                            }
                        });
                    });
                });
        });
    let over = ctx
        .pointer_hover_pos()
        .is_some_and(|p| out.response.rect.contains(p));
    let t = &mut app.quick.ux.ctx_tips;
    match act {
        1 => t.dismiss(),
        2 => {
            t.set_off(true);
            app.quick.dirty = true;
            super::toast(app, Level::Info, "tip.ctx.state_off", &[]);
        }
        _ => {
            if over {
                t.touch(now);
            }
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::ctx_tips::Moment;

    fn app() -> EditorApp {
        let ed = crate::editor::Editor::new_in_memory().unwrap();
        EditorApp::new(ed, None, "test".into())
    }

    #[test]
    fn command_switches_the_tips_and_keeps_the_mark() {
        let mut a = app();
        assert!(a.handlers.contains_key("app.ctx_tips"));
        assert!(a.sh.reg.get("app.ctx_tips").is_some());
        a.quick.ux.ctx_tips.offer(Moment::FirstWall, 0.0);
        super::toggle(&mut a);
        assert!(a.quick.ux.ctx_tips.off && a.quick.ux.ctx_tips.current().is_none());
        assert!(a.quick.dirty);
        super::toggle(&mut a);
        assert!(!a.quick.ux.ctx_tips.off);
        assert!(a.quick.ux.ctx_tips.seen.contains("first_wall"));
    }

    #[test]
    fn probe_follows_the_editor() {
        let mut a = app();
        let p0 = probe(&a);
        assert_eq!((p0.selected, p0.walls), (0, 0));
        assert!(!p0.saved);
        a.ed.selection.push(crate::editor::Sel::Wall(1));
        assert_eq!(probe(&a).selected, 1);
    }
}
