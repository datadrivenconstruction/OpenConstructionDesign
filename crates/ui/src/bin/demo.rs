// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ddc-ui-demo`: the shell around a dummy viewport, with theme/density/scale/language switches.
use ddc_ui::notify::Level;
use ddc_ui::{Density, Lang, Mode, Shell, Theme};

struct Demo {
    shell: Shell,
}

impl eframe::App for Demo {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut toggle: Option<u8> = None;
        let cat = &self.shell.cat;
        let labels = [
            cat.t("demo.light"),
            cat.t("demo.dark"),
            cat.t("demo.compact"),
            cat.t("demo.comfortable"),
            cat.t("demo.toast"),
            cat.t("demo.viewport"),
            cat.t("demo.scale"),
        ];
        let (mode, dens, lang) = (self.shell.theme.mode, self.shell.theme.density, cat.lang);
        let mut scale = self.shell.theme.scale;
        let accent = self.shell.theme.c("border.subtle");
        self.shell.show(ctx, |ui| {
            let r = ui.max_rect();
            let p = ui.painter();
            let step = 48.0;
            let mut x = r.left();
            while x < r.right() {
                p.line_segment(
                    [egui::pos2(x, r.top()), egui::pos2(x, r.bottom())],
                    (1.0, accent),
                );
                x += step;
            }
            let mut y = r.top();
            while y < r.bottom() {
                p.line_segment(
                    [egui::pos2(r.left(), y), egui::pos2(r.right(), y)],
                    (1.0, accent),
                );
                y += step;
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(&labels[5]);
                ui.separator();
                if ui
                    .selectable_label(mode == Mode::Light, &labels[0])
                    .clicked()
                {
                    toggle = Some(0);
                }
                if ui
                    .selectable_label(mode == Mode::Dark, &labels[1])
                    .clicked()
                {
                    toggle = Some(1);
                }
                ui.separator();
                if ui
                    .selectable_label(dens == Density::Compact, &labels[2])
                    .clicked()
                {
                    toggle = Some(2);
                }
                if ui
                    .selectable_label(dens == Density::Comfortable, &labels[3])
                    .clicked()
                {
                    toggle = Some(3);
                }
                ui.separator();
                if ui.selectable_label(lang == Lang::En, "EN").clicked() {
                    toggle = Some(5);
                }
                if ui.selectable_label(lang == Lang::Ru, "RU").clicked() {
                    toggle = Some(6);
                }
                ui.separator();
                ui.label(&labels[6]);
                egui::ComboBox::from_id_salt("scale")
                    .selected_text(format!("{:.0} %", scale * 100.0))
                    .show_ui(ui, |ui| {
                        for s in ddc_ui::theme::SCALE_STEPS {
                            ui.selectable_value(&mut scale, s, format!("{:.0} %", s * 100.0));
                        }
                    });
                ui.separator();
                if ui.button(&labels[4]).clicked() {
                    toggle = Some(4);
                }
            });
        });
        let sh = &mut self.shell;
        sh.theme.set_scale(scale);
        match toggle {
            Some(0) => sh.theme.mode = Mode::Light,
            Some(1) => sh.theme.mode = Mode::Dark,
            Some(2) => sh.theme.density = Density::Compact,
            Some(3) => sh.theme.density = Density::Comfortable,
            Some(4) => {
                let (t, a) = (sh.cat.t("toast.export_done"), sh.cat.t("toast.open_folder"));
                sh.toasts.push(Level::Success, t, Some(a));
            }
            Some(5) => sh.cat.lang = Lang::En,
            Some(6) => sh.cat.lang = Lang::Ru,
            _ => {}
        }
        sh.fired.clear();
    }
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1366.0, 768.0])
            .with_min_inner_size([1024.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DDC UI demo",
        opts,
        Box::new(|_cc| {
            let mut shell = Shell::new(Theme::new(Mode::Light, Density::Compact), Lang::Ru);
            shell.selected = 3;
            shell.saved = false;
            Ok(Box::new(Demo { shell }))
        }),
    )
}
