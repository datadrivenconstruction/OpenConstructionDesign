// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Shell composition: title bar (A), ribbon (B), context bar (C), viewport (F),
//! command line (G), status bar; palette and toasts float. Routes keyboard to commands.
use crate::commands::{MnemonicEvent, MnemonicInput};
use crate::notify::{Level, Toasts};
use crate::widgets::{self, bars, palette::Palette, ribbon::Ribbon};
use crate::{icons::IconCache, Catalog, Lang, Registry, Theme};
use egui::{Event, Rect};

/// Screen rects of the shell regions from the last frame (logical points).
#[derive(Debug, Clone, Copy)]
pub struct ShellLayout {
    pub title: Rect,
    pub ribbon: Rect,
    pub context: Rect,
    pub cmdline: Rect,
    pub status: Rect,
    pub viewport: Rect,
}

impl Default for ShellLayout {
    fn default() -> Self {
        let r = Rect::NOTHING;
        Self {
            title: r,
            ribbon: r,
            context: r,
            cmdline: r,
            status: r,
            viewport: r,
        }
    }
}

pub struct Shell {
    pub theme: Theme,
    pub cat: Catalog,
    pub reg: Registry,
    pub icons: IconCache,
    pub palette: Palette,
    pub ribbon: Ribbon,
    pub toasts: Toasts,
    pub mnemonic: MnemonicInput,
    pub active: Option<String>,
    pub echo: String,
    pub selected: u64,
    pub issues: u64,
    pub saved: bool,
    /// Commands fired, in order (the host drains this).
    pub fired: Vec<String>,
}

/// Shortcuts that still reach the command registry while a text field has focus;
/// everything else (Ctrl+Z/C/V/A, word delete, arrows…) belongs to the field.
pub fn passes_while_typing(key: egui::Key, m: egui::Modifiers) -> bool {
    let fkey = key.name().starts_with('F') && key.name().len() > 1;
    fkey || (m.command && !m.alt && !m.shift && matches!(key, egui::Key::K | egui::Key::Enter))
}

impl Shell {
    pub fn new(theme: Theme, lang: Lang) -> Self {
        Self {
            theme,
            cat: Catalog::new(lang),
            reg: Registry::builtin(),
            icons: IconCache::default(),
            palette: Palette::default(),
            ribbon: Ribbon::default(),
            toasts: Toasts::default(),
            mnemonic: MnemonicInput::default(),
            active: None,
            echo: String::new(),
            selected: 0,
            issues: 0,
            saved: true,
            fired: Vec::new(),
        }
    }

    pub fn run(&mut self, id: &str) {
        let Some(cmd) = self.reg.get(id) else { return };
        if id == "app.palette" {
            self.palette.toggle();
            return;
        }
        self.palette.note_used(id);
        if cmd.tab.is_some() {
            self.active = Some(id.to_string());
        }
        self.fired.push(id.to_string());
        let name = self.cat.t(&cmd.label_key());
        let msg = self.cat.tf("toast.ran", &[("name", &name)]);
        self.toasts.push(Level::Info, msg, None);
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let typing = ctx.wants_keyboard_input();
        let now = ctx.input(|i| i.time);
        let events = ctx.input(|i| i.events.clone());
        for ev in events {
            let Event::Key {
                key,
                physical_key,
                pressed: true,
                repeat: false,
                modifiers,
            } = ev
            else {
                continue;
            };
            if key == egui::Key::Escape && !self.palette.open {
                self.active = None;
                self.mnemonic.reset();
                self.echo.clear();
                continue;
            }
            let has_mod = modifiers.command || modifiers.alt;
            let fkey = key.name().starts_with('F') && key.name().len() > 1;
            if has_mod || fkey || key == egui::Key::Delete {
                if typing && !passes_while_typing(key, modifiers) {
                    continue;
                }
                if let Some(id) = self.reg.by_shortcut(key, modifiers).map(|c| c.id.clone()) {
                    self.run(&id);
                }
                continue;
            }
            if typing || self.palette.open || modifiers.shift {
                continue;
            }
            match self
                .mnemonic
                .key(&self.reg, physical_key.unwrap_or(key), now)
            {
                Some(MnemonicEvent::Pending(s)) => self.echo = s,
                Some(MnemonicEvent::Fired(id)) => {
                    self.echo = self
                        .reg
                        .get(&id)
                        .and_then(|c| c.mnemonic.clone())
                        .unwrap_or_default();
                    self.run(&id);
                }
                Some(MnemonicEvent::NoMatch(m)) => self.echo = format!("{m} ?"),
                None => {}
            }
        }
    }

    /// One frame. `viewport` paints the document area.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        viewport: impl FnOnce(&mut egui::Ui),
    ) -> ShellLayout {
        if !self.theme.apply(ctx) {
            return ShellLayout::default();
        }
        self.handle_keys(ctx);
        let th = &self.theme;
        let d = th.d();
        let mut lay = ShellLayout::default();
        let bar = |fill| {
            egui::Frame::new()
                .fill(fill)
                .inner_margin(egui::Margin::symmetric(d.pad_x as i8, 0))
        };
        let mut clicked: Option<String> = None;

        lay.title = egui::TopBottomPanel::top("ddc.title")
            .frame(bar(th.c("bg.app")))
            .exact_height(d.toolbar)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    for id in ["edit.undo", "edit.redo"] {
                        let Some(c) = self.reg.get(id) else { continue };
                        let tip = format!(
                            "{}  ·  {}",
                            self.cat.t(&c.label_key()),
                            c.shortcuts.join(" / ")
                        );
                        if widgets::icon_button(ui, &mut self.icons, th, &c.icon, false, &tip)
                            .clicked()
                        {
                            clicked = Some(id.into());
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let t = format!("{}  Ctrl+K", self.cat.t("cmd.app.palette"));
                        if ui.button(t).clicked() {
                            clicked = Some("app.palette".into());
                        }
                    });
                });
            })
            .response
            .rect;

        lay.ribbon = egui::TopBottomPanel::top("ddc.ribbon")
            .frame(bar(th.c("bg.panel")))
            .show(ctx, |ui| {
                let act = self.active.as_deref();
                if let Some(id) =
                    self.ribbon
                        .show(ui, th, &self.cat, &self.reg, &mut self.icons, act)
                {
                    clicked = Some(id);
                }
            })
            .response
            .rect;

        lay.context = egui::TopBottomPanel::top("ddc.context")
            .frame(bar(th.c("bg.app")))
            .exact_height(d.row)
            .show(ctx, |ui| {
                bars::context_bar(ui, th, &self.cat, self.active.as_deref())
            })
            .response
            .rect;

        lay.status = egui::TopBottomPanel::bottom("ddc.status")
            .frame(bar(th.c("bg.app")))
            .exact_height(d.row)
            .show(ctx, |ui| {
                let s = bars::Status {
                    selected: self.selected,
                    level: "L1",
                    issues: self.issues,
                    regen_ms: 16,
                    saved: self.saved,
                };
                bars::status_bar(ui, th, &self.cat, &s)
            })
            .response
            .rect;

        lay.cmdline = egui::TopBottomPanel::bottom("ddc.cmdline")
            .frame(bar(th.c("bg.panel")))
            .exact_height(d.row)
            .show(ctx, |ui| bars::command_line(ui, th, &self.cat, &self.echo))
            .response
            .rect;

        lay.viewport = egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(th.c("bg.viewport")))
            .show(ctx, viewport)
            .response
            .rect;

        let bottom = lay.status.height() + lay.cmdline.height();
        if let Some(a) = widgets::toasts::show(ctx, th, &self.cat, &mut self.toasts, bottom) {
            self.fired.push(format!("toast:{a}"));
        }
        if let Some(id) = self
            .palette
            .show(ctx, th, &self.cat, &self.reg, &mut self.icons)
        {
            clicked = Some(id);
        }
        if let Some(id) = clicked {
            self.run(&id);
        }
        lay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Density, Mode};

    fn frame(
        shell: &mut Shell,
        ctx: &egui::Context,
        size: egui::Vec2,
        events: Vec<Event>,
    ) -> ShellLayout {
        let mut lay = ShellLayout::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            lay = shell.show(ctx, |ui| {
                ui.label("viewport");
            })
        });
        lay
    }

    fn key(k: egui::Key, modifiers: egui::Modifiers) -> Event {
        Event::Key {
            key: k,
            physical_key: Some(k),
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// Headless layout snapshot: regions stack without overlap and the viewport keeps most of
    /// the window at every UI scale, density, theme and language.
    #[test]
    fn shell_layout_headless() {
        for (mode, dens, lang) in [
            (Mode::Light, Density::Compact, Lang::En),
            (Mode::Dark, Density::Comfortable, Lang::Ru),
        ] {
            for scale in crate::theme::SCALE_STEPS {
                let mut th = Theme::new(mode, dens);
                th.set_scale(scale);
                let mut sh = Shell::new(th, lang);
                let ctx = egui::Context::default();
                // Logical window = 1366×768 physical at this scale.
                let size = egui::vec2(1366.0, 768.0);
                frame(&mut sh, &ctx, size, vec![]);
                let l = frame(&mut sh, &ctx, size, vec![]);
                let tag = format!("{mode:?} {dens:?} {lang:?} {scale}");
                assert!(l.title.bottom() <= l.ribbon.top() + 0.5, "{tag} {l:?}");
                assert!(l.ribbon.bottom() <= l.context.top() + 0.5, "{tag}");
                assert!(l.context.bottom() <= l.viewport.top() + 0.5, "{tag}");
                assert!(l.viewport.bottom() <= l.cmdline.top() + 0.5, "{tag}");
                assert!(
                    l.cmdline.bottom() <= l.status.top() + 0.5,
                    "{tag} {:?} {:?}",
                    l.cmdline,
                    l.status
                );
                let screen = ctx.screen_rect();
                let frac = l.viewport.area() / screen.area();
                let min = if scale >= 1.75 { 0.25 } else { 0.5 };
                assert!(frac > min, "{tag}: viewport {frac:.2}");
                assert!(sh.icons.len() > 0, "{tag}: ribbon icons rasterized");
            }
        }
    }

    #[test]
    fn shell_keyboard_routes() {
        let mut sh = Shell::new(Theme::new(Mode::Light, Density::Compact), Lang::Ru);
        let ctx = egui::Context::default();
        let s = egui::vec2(1280.0, 800.0);
        frame(&mut sh, &ctx, s, vec![]);
        frame(
            &mut sh,
            &ctx,
            s,
            vec![key(egui::Key::W, Default::default())],
        );
        assert_eq!(sh.echo, "W");
        frame(
            &mut sh,
            &ctx,
            s,
            vec![key(egui::Key::A, Default::default())],
        );
        assert_eq!(sh.active.as_deref(), Some("arch.wall"));
        assert_eq!(sh.toasts.live.len(), 1);
        frame(
            &mut sh,
            &ctx,
            s,
            vec![key(egui::Key::Escape, Default::default())],
        );
        assert_eq!(sh.active, None);
        frame(
            &mut sh,
            &ctx,
            s,
            vec![key(egui::Key::K, egui::Modifiers::COMMAND)],
        );
        assert!(sh.palette.open);
        // Palette: type, Enter picks the top match.
        frame(&mut sh, &ctx, s, vec![Event::Text(crate::test_fx::fx("sh_typed").into())]);
        assert_eq!(sh.palette.query, crate::test_fx::fx("sh_typed"));
        frame(
            &mut sh,
            &ctx,
            s,
            vec![key(egui::Key::Enter, Default::default())],
        );
        assert!(!sh.palette.open);
        assert_eq!(sh.fired.last().map(String::as_str), Some("mod.move"));
    }

    #[test]
    fn typing_keeps_text_editing_combos() {
        use egui::{Key, Modifiers as M};
        for k in [
            Key::Z,
            Key::Y,
            Key::X,
            Key::C,
            Key::V,
            Key::A,
            Key::E,
            Key::Backspace,
            Key::Delete,
            Key::ArrowLeft,
            Key::Home,
            Key::End,
        ] {
            assert!(!passes_while_typing(k, M::COMMAND), "{k:?}");
            assert!(!passes_while_typing(k, M::ALT), "{k:?}");
        }
        assert!(passes_while_typing(Key::K, M::COMMAND));
        assert!(passes_while_typing(Key::Enter, M::COMMAND));
        assert!(passes_while_typing(Key::F2, M::NONE));
    }
}
