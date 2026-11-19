//! Context (options) bar, command line echo and status bar (SHELL_AND_COMMANDS §6.4–6.5).
use crate::{Catalog, Theme};
use egui::{RichText, Ui};

pub fn context_bar(ui: &mut Ui, th: &Theme, cat: &Catalog, active: Option<&str>) {
    ui.horizontal(|ui| match active {
        Some(id) => {
            let name = cat.t(&format!("cmd.{id}"));
            ui.label(RichText::new(&name).strong().color(th.c("accent")));
            ui.label(cat.tf("ctx.command", &[("name", &name)]));
        }
        None => {
            ui.label(RichText::new(cat.t("ctx.idle")).color(th.c("text.secondary")));
        }
    });
}

pub fn command_line(ui: &mut Ui, th: &Theme, cat: &Catalog, echo: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("›").monospace().color(th.c("accent")));
        if !echo.is_empty() {
            let t = cat.tf("cmdline.echo", &[("keys", echo)]);
            ui.label(RichText::new(t).monospace().color(th.c("text.secondary")));
        }
    });
}

/// Status bar height: one 28 px line.
pub const STATUS_H: f32 = 28.0;

pub struct Status<'a> {
    pub selected: u64,
    pub level: &'a str,
    pub issues: u64,
    pub regen_ms: u32,
    pub saved: bool,
}

pub fn status_bar(ui: &mut Ui, th: &Theme, cat: &Catalog, s: &Status) {
    let sec = th.c("text.secondary");
    ui.set_min_height(STATUS_H);
    ui.horizontal(|ui| {
        ui.set_min_height(STATUS_H);
        ui.label(cat.plural("status.sel", s.selected));
        ui.separator();
        ui.label(RichText::new(cat.tf("status.level", &[("name", s.level)])).color(sec));
        ui.separator();
        ui.label(RichText::new(cat.t("status.snaps")).color(sec));
        ui.separator();
        let ic = if s.issues == 0 {
            th.c("success")
        } else {
            th.c("warning")
        };
        ui.label(RichText::new(cat.plural("status.checks", s.issues)).color(ic));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (k, c) = if s.saved {
                ("status.saved", th.c("success"))
            } else {
                ("status.unsaved", th.c("warning"))
            };
            ui.label(RichText::new(cat.t(k)).color(c));
            ui.separator();
            let ms = s.regen_ms.to_string();
            ui.label(RichText::new(cat.tf("status.regen", &[("ms", &ms)])).color(sec));
        });
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn status_bar_is_one_28px_line_on_the_grid() {
        assert_eq!(super::STATUS_H, 28.0);
        assert_eq!(super::STATUS_H % 4.0, 0.0);
    }
}
