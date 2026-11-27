// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Help window (F1): topic search on the left, the topic on the right (what it is, steps, keys, tips);
//! topic commands are buttons that run the command; links to other topics.
use crate::ui_kit as kit;
use ddc_ui::help::{Block, Inline, Library, Topic, REQUIRED};
use ddc_ui::theme::{self, layout, space, Role};
use ddc_ui::widgets::table::{row_colors, RowState};
use ddc_ui::widgets::{self, Button, TextField};
use ddc_ui::{Catalog, Registry, Theme};
use egui::{self, Align, RichText, Sense, Vec2};
use crate::ui_kit::KitScroll;

/// Shortcut shown in the back button tooltip; the key itself is handled in `show`.
const BACK_KEYS: &str = "Alt+Left";
const SEARCH_ID: &str = "ddc.help.search";

#[derive(Default)]
pub struct Panel {
    pub open: bool,
    pub query: String,
    pub topic: Option<String>,
    /// Navigation history for "Back".
    pub back: Vec<String>,
    /// F1 was pressed: pick a topic by context this frame.
    pub want_context: bool,
    focus: bool,
}

/// An action from the help window, run by the host after the frame.
#[derive(Debug, PartialEq)]
pub enum Act {
    Run(String),
}

impl Panel {
    pub fn show_topic(&mut self, id: &str) {
        if self.topic.as_deref() != Some(id) {
            if let Some(cur) = self.topic.take() {
                self.back.push(cur);
            }
            self.topic = Some(id.to_string());
        }
        self.open = true;
    }
    pub fn focus_search(&mut self) {
        self.focus = true;
    }
    /// One step back in the history; false when there is nothing to go back to.
    pub fn go_back(&mut self) -> bool {
        match self.back.pop() {
            Some(prev) => {
                self.topic = Some(prev);
                true
            }
            None => false,
        }
    }
}

/// What a command chip shows: icon, label, shortcut keys, whether it can run and why not.
#[derive(Debug, Clone, PartialEq)]
pub struct ChipParts {
    pub icon: String,
    pub label: String,
    pub keys: String,
    pub enabled: bool,
    pub reason: String,
}

/// Chip of a command from a topic. An id the registry does not know gives a disabled chip with
/// the reason; a command without a drawn icon falls back to the help icon.
pub fn chip_parts(cat: &Catalog, reg: &Registry, id: &str) -> ChipParts {
    match reg.get(id) {
        None => ChipParts {
            icon: "warning".into(),
            label: id.into(),
            keys: String::new(),
            enabled: false,
            reason: cat.t("help_nav.help.unknown_cmd"),
        },
        Some(c) => {
            let (label, keys) = cmd_label(cat, reg, id);
            let icon = if ddc_ui::icons::source(&c.icon).is_some() {
                c.icon.clone()
            } else {
                "help".into()
            };
            ChipParts {
                icon,
                label,
                keys,
                enabled: true,
                reason: String::new(),
            }
        }
    }
}

/// Tooltip of the back button: title and shortcut in the kit's tooltip form.
pub fn back_tip(cat: &Catalog) -> String {
    format!("{}  ·  {BACK_KEYS}", cat.t("help_nav.help.back_tip"))
}

/// Hover text of a see-also link: the topic title, never its id.
pub fn see_hover(t: &Topic) -> &str {
    &t.title
}

/// Command chip: icon and label on a kit button, shortcut keycaps after it. Disabled chips
/// explain why in the tooltip. Returns true when the chip was clicked.
fn chip(ui: &mut egui::Ui, th: &Theme, p: &ChipParts) -> bool {
    let ctx = ui.ctx().clone();
    // the kit button shrinks to the rest of the row; its whole width makes the row wrap instead
    let whole = Button::secondary(&p.label).small().icon_width(ui, th);
    // the button sits in its own scope, which never wraps: the row breaks here when the chip
    // and its keycaps would run past the room, or the window grows to the unbroken row
    let keys_w = if p.enabled && !p.keys.is_empty() {
        ui.spacing().item_spacing.x + widgets::keycaps_width(ui.painter(), th, &p.keys)
    } else {
        0.0
    };
    let row_start = ui.cursor().min.x <= ui.max_rect().left() + 0.5;
    // a wrapping Ui reports the whole line as its available width; the rest is before the wrap
    if !row_start && ui.available_rect_before_wrap().width() < whole + keys_w {
        ui.end_row();
    }
    let resp = ui
        .add_enabled_ui(p.enabled, |ui| {
            widgets::with_icons(&ctx, |icons| {
                Button::secondary(&p.label)
                    .small()
                    .min_width(whole)
                    .show_icon(ui, th, icons, &p.icon)
            })
        })
        .inner;
    if p.enabled && !p.keys.is_empty() {
        ui.scope(|ui| widgets::keycap(ui, th, &p.keys));
    }
    if !p.enabled {
        widgets::rich_tooltip_reason(resp, th, &p.label, "", "", &p.reason);
        return false;
    }
    resp.clicked()
}

/// Selectable row of the topic list on the row tokens of the kit tables.
fn topic_row(ui: &mut egui::Ui, th: &Theme, title: &str, selected: bool) -> egui::Response {
    let size = Vec2::new(ui.available_width(), th.d().control);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let state = if selected {
        RowState::Selected
    } else {
        RowState::Normal
    };
    let (fill, text, _) = row_colors(th.sem(), state, resp.hovered());
    let enabled = ui.is_enabled();
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, enabled, selected, title)
    });
    ui.painter().rect_filled(rect, th.radius("sm"), fill);
    let inner = rect.shrink2(Vec2::new(space::SM, 0.0));
    let cut = widgets::text::paint_in(ui, inner, title, th.font(Role::Body), text, Align::Min);
    if cut {
        resp.on_hover_text(title)
    } else {
        resp
    }
}

/// A command label and its key (a chord or a two-letter one).
pub fn cmd_label(cat: &Catalog, reg: &Registry, id: &str) -> (String, String) {
    let keys = reg
        .get(id)
        .and_then(|c| c.shortcuts.first().cloned().or(c.mnemonic.clone()))
        .unwrap_or_default();
    (cat.t(&format!("cmd.{id}")), keys)
}

pub fn show(
    ctx: &egui::Context,
    p: &mut Panel,
    lib: &Library,
    cat: &Catalog,
    reg: &Registry,
    acts: &mut Vec<Act>,
) {
    if !p.open {
        return;
    }
    let mut open = true;
    let mut go: Option<String> = None;
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft)) {
        p.go_back();
    }
    kit::Dialog::new("help", cat.t("help.title"), kit::DialogSize::Wide)
        .open(&mut open)
        .show(ctx, |ui| {
            let th = theme::of(ui.ctx());
            // the window keeps to the screen whatever a topic holds: columns are sized from
            // this width and never from what the rows ask for
            let total = ui.available_width().min(ui.ctx().screen_rect().width() - 64.0);
            ui.set_max_width(total.max(layout::PROP_KEY_MAX));
            ui.horizontal_wrapped(|ui| {
                let can_back = !p.back.is_empty();
                if widgets::glyph_button(ui, "arrow-left", can_back, &back_tip(cat)).clicked() {
                    p.go_back();
                }
                let hint = cat.t("help.search");
                let name = cat.t("help_nav.help.search_label");
                let field_id = egui::Id::new(SEARCH_ID);
                TextField::new(&mut p.query)
                    .id(field_id)
                    .hint(&hint)
                    .width(layout::PROP_KEY_MAX)
                    .labelled_by(egui::Id::new("ddc.help.search.label"), &name)
                    .show(ui, &th);
                if std::mem::take(&mut p.focus) {
                    ui.memory_mut(|m| m.request_focus(field_id));
                }
                for id in [
                    "help.tutorial",
                    "help.sample",
                    "help.reference",
                    "help.whats_new",
                    "ui.onboarding",
                ] {
                    if chip(ui, &th, &chip_parts(cat, reg, id)) {
                        acts.push(Act::Run(id.into()));
                    }
                }
            });
            ui.separator();
            let h = ui.available_height();
            let list_w = layout::PROP_KEY_MAX + layout::PROP_KEY_MIN;
            // list, two gaps and the divider are fixed; the topic gets the rest
            let gaps = ui.spacing().item_spacing.x * 2.0 + 1.0;
            let body_w = (ui.available_width() - list_w - gaps).max(layout::PROP_KEY_MIN);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    // wide enough for the longest topic title in both languages
                    ui.set_width(list_w);
                    egui::ScrollArea::vertical()
                        .id_salt("ddc.help.list")
                        .max_height(h)
                        .auto_shrink([false, false])
                        .kit_show(ui, |ui| {
                            let found = lib.search(&p.query);
                            if found.is_empty() {
                                let msg = th.rich(Role::Body, &cat.t("help.empty"));
                                ui.label(msg.color(th.c("text.secondary")));
                            }
                            ui.spacing_mut().item_spacing.y = space::XXS;
                            for t in found {
                                let on = p.topic.as_deref() == Some(t.id.as_str());
                                if topic_row(ui, &th, &t.title, on).clicked() && !on {
                                    go = Some(t.id.clone());
                                }
                            }
                        });
                });
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_width(body_w);
                    egui::ScrollArea::vertical()
                        .id_salt("ddc.help.body")
                        .max_height(h)
                        .auto_shrink([false, false])
                        .kit_show(ui, |ui| {
                            let topic = p
                                .topic
                                .as_deref()
                                .and_then(|id| lib.get(id))
                                .or_else(|| lib.get("interface"))
                                .or(lib.topics.first());
                            if let Some(t) = topic {
                                if let Some(id) = body(ui, t, lib, cat, reg, acts) {
                                    go = Some(id);
                                }
                            }
                        });
                });
            });
        });
    if let Some(id) = go {
        p.show_topic(&id);
    }
    if !open {
        p.open = false;
    }
}

/// A whole topic; returns the topic whose link was clicked.
pub fn body(
    ui: &mut egui::Ui,
    t: &Topic,
    lib: &Library,
    cat: &Catalog,
    reg: &Registry,
    acts: &mut Vec<Act>,
) -> Option<String> {
    let mut go = None;
    let th = theme::of(ui.ctx());
    ui.heading(&t.title);
    ui.add_space(space::XS);
    for b in &t.blocks {
        match b {
            Block::Heading(h) => {
                ui.add_space(space::SM);
                let text = if REQUIRED.contains(&h.as_str()) {
                    cat.t(&format!("help.sec.{h}"))
                } else {
                    h.clone()
                };
                ui.label(
                    RichText::new(text)
                        .strong()
                        .size(ddc_ui::theme::type_size::TITLE),
                );
            }
            Block::Para(s) => {
                if let Some(id) = spans(ui, None, s, lib, cat, reg, acts) {
                    go = Some(id);
                }
            }
            Block::Step(n, s) => {
                if let Some(id) = spans(ui, Some(format!("{n}.")), s, lib, cat, reg, acts) {
                    go = Some(id);
                }
            }
            Block::Bullet(s) => {
                if let Some(id) = spans(ui, Some("•".into()), s, lib, cat, reg, acts) {
                    go = Some(id);
                }
            }
        }
    }
    if !t.commands.is_empty() {
        ui.add_space(space::MD);
        ui.label(
            RichText::new(cat.t("help.commands"))
                .strong()
                .size(ddc_ui::theme::type_size::TITLE),
        );
        ui.horizontal_wrapped(|ui| {
            for id in &t.commands {
                if chip(ui, &th, &chip_parts(cat, reg, id)) {
                    acts.push(Act::Run(id.clone()));
                }
            }
        });
    }
    let see: Vec<&Topic> = t.see.iter().filter_map(|id| lib.get(id)).collect();
    if !see.is_empty() {
        ui.add_space(space::MD);
        ui.label(
            RichText::new(cat.t("help.see"))
                .strong()
                .size(ddc_ui::theme::type_size::TITLE),
        );
        ui.horizontal_wrapped(|ui| {
            for s in see {
                let r = Button::quiet(&s.title).small().show(ui, &th);
                if r.on_hover_text(see_hover(s)).clicked() {
                    go = Some(s.id.clone());
                }
            }
        });
    }
    go
}

/// A markup line: text wraps, commands are buttons, topics are links.
fn spans(
    ui: &mut egui::Ui,
    lead: Option<String>,
    s: &[Inline],
    lib: &Library,
    cat: &Catalog,
    reg: &Registry,
    acts: &mut Vec<Act>,
) -> Option<String> {
    let mut go = None;
    let th = theme::of(ui.ctx());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if let Some(l) = lead {
            ui.label(RichText::new(format!("{l} ")).strong());
        }
        for x in s {
            match x {
                Inline::Text(t) => {
                    ui.label(t.as_str());
                }
                Inline::Bold(t) => {
                    ui.label(RichText::new(t.as_str()).strong());
                }
                Inline::Key(k) => {
                    ui.scope(|ui| widgets::keycap(ui, &th, k));
                }
                Inline::Cmd(id) => {
                    ui.add_space(space::XS);
                    if chip(ui, &th, &chip_parts(cat, reg, id)) {
                        acts.push(Act::Run(id.clone()));
                    }
                    ui.add_space(space::XS);
                }
                Inline::Topic(id) => {
                    let title = lib.get(id).map_or(id.as_str(), |t| t.title.as_str());
                    if ui.link(title).clicked() {
                        go = Some(id.clone());
                    }
                }
            }
        }
    });
    go
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::widgets::split_tip;
    use ddc_ui::Lang;

    fn langs() -> [Catalog; 2] {
        [Catalog::new(Lang::En), Catalog::new(Lang::Ru)]
    }

    #[test]
    fn unknown_command_gives_a_disabled_chip_with_a_reason() {
        let reg = Registry::builtin();
        for cat in langs() {
            let p = chip_parts(&cat, &reg, "no.such.command");
            assert!(!p.enabled);
            assert_eq!(p.label, "no.such.command");
            assert!(p.keys.is_empty());
            assert_ne!(p.reason, "help_nav.help.unknown_cmd", "string is translated");
            assert!(!p.reason.is_empty());
            assert!(ddc_ui::icons::source(&p.icon).is_some());
        }
    }

    #[test]
    fn known_command_chip_takes_keys_from_the_registry() {
        let reg = Registry::builtin();
        let cat = Catalog::new(Lang::En);
        let c = reg
            .all()
            .iter()
            .find(|c| !c.shortcuts.is_empty())
            .expect("a command with a shortcut");
        let p = chip_parts(&cat, &reg, &c.id);
        assert!(p.enabled && p.reason.is_empty());
        assert_eq!(p.keys, c.shortcuts[0]);
        assert_eq!(p.label, cat.t(&format!("cmd.{}", c.id)));
    }

    #[test]
    fn every_chip_icon_is_drawn() {
        let reg = Registry::builtin();
        let cat = Catalog::new(Lang::En);
        for c in reg.all() {
            let p = chip_parts(&cat, &reg, &c.id);
            assert!(ddc_ui::icons::source(&p.icon).is_some(), "{}", c.id);
        }
    }

    #[test]
    fn every_command_of_a_topic_makes_an_enabled_chip() {
        let reg = Registry::builtin();
        for cat in langs() {
            let lib = Library::builtin(cat.lang);
            for t in &lib.topics {
                for id in &t.commands {
                    assert!(chip_parts(&cat, &reg, id).enabled, "{}: {id}", t.id);
                }
            }
        }
    }

    #[test]
    fn back_walks_the_history_and_stops_at_its_start() {
        let mut p = Panel::default();
        assert!(!p.go_back());
        p.show_topic("a");
        p.show_topic("b");
        p.show_topic("b");
        assert_eq!(p.back, vec!["a".to_string()]);
        assert!(p.go_back());
        assert_eq!(p.topic.as_deref(), Some("a"));
        assert!(!p.go_back());
        assert_eq!(p.topic.as_deref(), Some("a"));
    }

    #[test]
    fn back_tip_names_the_shortcut() {
        for cat in langs() {
            let tip = back_tip(&cat);
            let (title, keys) = split_tip(&tip);
            assert_eq!(title, cat.t("help_nav.help.back_tip"));
            assert_ne!(title, "help_nav.help.back_tip", "string is translated");
            assert_eq!(keys, "Alt+Left");
        }
    }

    #[test]
    fn see_also_hover_is_the_title_not_the_id() {
        let lib = Library::builtin(Lang::En);
        let mut n = 0;
        for t in &lib.topics {
            for s in t.see.iter().filter_map(|id| lib.get(id)) {
                assert_eq!(see_hover(s), s.title);
                assert_ne!(see_hover(s), s.id);
                n += 1;
            }
        }
        assert!(n > 0, "the library has see-also links");
    }

    #[test]
    fn panel_has_no_raw_egui_controls() {
        let src = include_str!("panel.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap();
        for raw in [
            "small_button(",
            "selectable_label(",
            "egui::Button::new(",
            "ui.button(",
            "TextEdit::singleline(",
            "desired_width(",
            "add_space(4.0",
            "add_space(8.0",
            "add_space(10.0",
        ] {
            assert!(!prod.contains(raw), "raw control left: {raw}");
        }
    }
}
