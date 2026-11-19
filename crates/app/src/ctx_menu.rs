// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rows of the project browser context menus: kit `menu_item` with the key hint taken from the
//! command registry, destructive rows in danger ink after a divider.
use ddc_ui::widgets::{menu_item, menu_item_icon, MenuTone};
use ddc_ui::{Registry, Theme};
use std::sync::OnceLock;

/// Registry ids behind the common row commands.
pub const OPEN: &str = "browser.row_open";
pub const RENAME: &str = "browser.row_rename";
pub const DELETE: &str = "browser.row_delete";

fn reg() -> &'static Registry {
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(Registry::builtin)
}

/// Key hint of a row command; "" for an empty id or a command without keys.
pub fn keys(cmd: &str) -> String {
    reg().menu_keys(cmd)
}

/// Tone of a row: only the delete command is destructive.
pub fn tone(cmd: &str) -> MenuTone {
    if cmd == DELETE {
        MenuTone::Danger
    } else {
        MenuTone::Normal
    }
}

/// One menu row for `cmd` (hint and tone from the registry data).
pub fn item(ui: &mut egui::Ui, th: &Theme, label: &str, cmd: &str) -> egui::Response {
    menu_item(ui, th, label, &keys(cmd), tone(cmd), true)
}

/// Like `item`, with an icon slot: `icon` from the icon set, `None` keeps the label aligned.
pub fn item_icon(
    ui: &mut egui::Ui,
    th: &Theme,
    icon: Option<&str>,
    label: &str,
    cmd: &str,
) -> egui::Response {
    menu_item_icon(ui, th, icon, label, &keys(cmd), tone(cmd), true)
}

/// A destructive row without a key hint (the action is not bound to a key).
pub fn danger_row(ui: &mut egui::Ui, th: &Theme, label: &str) -> egui::Response {
    menu_item(ui, th, label, "", MenuTone::Danger, true)
}

/// Every verb the context menus use; each has one icon slot content, see `verb_icon`.
pub const VERBS: [&str; 9] =
    ["open", "rename", "duplicate", "delete", "print", "edit", "move_up", "move_down", "close"];

/// Icon of a menu verb, the same in every menu. `None` (rename) keeps the slot empty so the
/// label still aligns with the rows that have an icon.
pub fn verb_icon(verb: &str) -> Option<&'static str> {
    match verb {
        "open" => Some("open"),
        "duplicate" => Some("copy"),
        "delete" => Some("delete"),
        "print" => Some("print"),
        "edit" => Some("properties"),
        "move_up" => Some("move-up"),
        "move_down" => Some("move-down"),
        "close" => Some("close"),
        _ => None,
    }
}

/// Row for a verb: icon from `verb_icon`, hint and tone from the registry data of `cmd`.
pub fn verb_row(
    ui: &mut egui::Ui,
    th: &Theme,
    verb: &str,
    label: &str,
    cmd: &str,
) -> egui::Response {
    item_icon(ui, th, verb_icon(verb), label, cmd)
}

/// Destructive row with the delete icon and no key hint.
pub fn danger_icon_row(ui: &mut egui::Ui, th: &Theme, label: &str) -> egui::Response {
    menu_item_icon(ui, th, verb_icon("delete"), label, "", MenuTone::Danger, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::widgets::menu::ink_token;
    use ddc_ui::widgets::menu_divider;
    use ddc_ui::{Density, Mode};

    /// Bodies of every `context_menu(` closure in `src`.
    fn menus(src: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut at = 0;
        while let Some(p) = src[at..].find("context_menu(") {
            let start = at + p;
            let Some(open) = src[start..].find('{') else { break };
            let mut depth = 0usize;
            let mut end = start + open;
            for (i, ch) in src[start + open..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = start + open + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            out.push(&src[start..end]);
            at = end.max(start + 1);
        }
        out
    }

    #[test]
    fn row_keys_come_from_the_registry() {
        assert_eq!(keys(OPEN), "Enter");
        assert_eq!(keys(RENAME), "F2");
        assert_eq!(keys(DELETE), "Del");
        assert_eq!(keys(""), "");
        assert_eq!(keys(OPEN), reg().menu_keys(OPEN));
    }

    #[test]
    fn row_commands_are_not_global_shortcuts() {
        for id in [OPEN, RENAME, DELETE] {
            let c = reg().get(id).expect(id);
            assert!(c.shortcuts.is_empty(), "{id}");
        }
        assert_eq!(reg().conflicts(), vec![]);
    }

    #[test]
    fn row_labels_exist_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            for id in [OPEN, RENAME, DELETE] {
                let key = reg().get(id).unwrap().label_key();
                assert_ne!(cat.t(&key), key, "{id} {lang:?}");
            }
        }
    }

    #[test]
    fn delete_is_danger_the_rest_normal() {
        assert_eq!(tone(DELETE), MenuTone::Danger);
        assert_eq!(ink_token(tone(DELETE), true), "error");
        assert_eq!(tone(OPEN), MenuTone::Normal);
        assert_eq!(tone(RENAME), MenuTone::Normal);
        assert_eq!(tone(""), MenuTone::Normal);
    }

    #[test]
    fn rows_render_headless_with_the_same_click_semantics() {
        let th = Theme::new(Mode::Light, Density::Comfortable);
        let ctx = ddc_ui::theme::headless_ctx();
        let mut sizes = Vec::new();
        let _ = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                for (label, cmd) in [("Open", OPEN), ("Rename", RENAME), ("Delete", DELETE)] {
                    let r = item(ui, &th, label, cmd);
                    assert!(!r.clicked());
                    sizes.push(r.rect.height());
                }
                menu_divider(ui, &th);
                let r = danger_row(ui, &th, "Remove");
                sizes.push(r.rect.height());
            });
        });
        assert_eq!(sizes.len(), 4);
        assert!(sizes.iter().all(|h| *h > 0.0));
    }

    /// Menus keep the kit rows: a plain egui button or separator in them is a regression.
    #[test]
    fn browser_context_menus_use_kit_rows() {
        let files = [
            ("panels", include_str!("shell/panels.rs")),
            ("browser", include_str!("shell/browser.rs")),
            ("drafting_view", include_str!("drafting_view.rs")),
            ("imported_props", include_str!("imported_props.rs")),
        ];
        let mut count = 0;
        for (name, src) in files {
            for m in menus(src) {
                count += 1;
                assert!(!m.contains("ui.button("), "{name}: raw button in a context menu");
                assert!(!m.contains("ui.separator("), "{name}: raw separator in a context menu");
                assert!(!m.contains("menu_button("), "{name}: egui submenu in a context menu");
            }
        }
        assert!(count >= 6, "menus found: {count}");
    }

    #[test]
    fn verb_icons_cover_every_verb_and_exist() {
        for v in VERBS {
            match verb_icon(v) {
                Some(n) => assert!(ddc_ui::icons::source(n).is_some(), "{v}: icon {n}"),
                None => assert_eq!(v, "rename", "{v} has no icon"),
            }
        }
        assert_eq!(verb_icon("delete"), Some("delete"));
        assert_eq!(verb_icon("unknown"), None);
        // the browser menus use the same glyphs for the same verbs
        let panels = include_str!("shell/panels.rs");
        assert!(panels.contains("Some(\"open\"), &s.cat.t(\"browser.open\")"));
        assert!(panels.contains("Some(\"delete\"), &s.cat.t(\"browser.delete\")"));
    }

    #[test]
    fn remaining_menus_use_icon_rows() {
        // `#[cfg(test)] mod x;` lines sit among the code: the scan ends at the test module
        let strip = |s: &str| {
            s[..s.find("#[cfg(test)]\nmod tests {").unwrap_or(s.len())].to_string()
        };
        let dv = strip(include_str!("drafting_view.rs"));
        let ip = strip(include_str!("imported_props.rs"));
        let sv = strip(include_str!("sheet_view.rs"));
        let pn = strip(include_str!("shell/panels.rs"));
        assert!(dv.contains("ctx_menu::verb_row("));
        assert!(!dv.contains("ctx_menu::item("));
        assert!(ip.contains("ctx_menu::danger_icon_row("));
        assert!(!ip.contains("ctx_menu::danger_row("));
        assert!(sv.contains("menu_item_icon(") && !sv.contains("menu_item("));
        assert!(pn.contains("menu_item_icon(") && !pn.contains(" menu_item("));
        for (n, s) in [("dv", &dv), ("ip", &ip), ("sv", &sv), ("pn", &pn)] {
            for m in menus(s) {
                assert!(!m.contains("menu_button("), "{n}");
                assert!(!m.contains(".button("), "{n}");
            }
        }
        // same commands, same order as before the icons
        let open = dv.find("\"open\", \"drafting_view.open\"").unwrap();
        let ren = dv.find("\"rename\", \"drafting_view.rename\"").unwrap();
        let dup = dv.find("\"duplicate\", \"drafting_view.duplicate\"").unwrap();
        // the first delete row belongs to the toolbar menu above the list menu
        let del = dup + dv[dup..].find("verb_row(ui, th, \"delete\"").unwrap();
        assert!(open < ren && ren < dup && dup < del);
        assert!(sv.contains("Print::new(st.active)") && sv.contains("fn sheet_menu_verb"));
    }

    /// Text x of every galley with this exact text in one frame.
    fn label_xs(ctx: &egui::Context, out: &egui::FullOutput, label: &str) -> Vec<f32> {
        let _ = ctx;
        out.shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::Shape::Text(t) if t.galley.text() == label => Some(t.pos.x),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn verb_rows_render_aligned_in_both_languages() {
        let th = Theme::new(Mode::Light, Density::Comfortable);
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let rows = [
                ("open", "browser.open", OPEN),
                ("rename", "browser.rename", RENAME),
                ("duplicate", "browser.duplicate", ""),
                ("delete", "browser.delete", DELETE),
            ];
            let ctx = egui::Context::default();
            ddc_ui::theme::install_fonts(&ctx);
            let mut labels = Vec::new();
            let mut last = None;
            for _ in 0..2 {
                labels.clear();
                last = Some(ctx.run(Default::default(), |c| {
                    egui::CentralPanel::default().show(c, |ui| {
                        for (verb, key, cmd) in rows {
                            let mut label = cat.t(key);
                            if label == key {
                                label = verb.to_string();
                            }
                            let r = verb_row(ui, &th, verb, &label, cmd);
                            assert!(!r.clicked());
                            assert!(r.rect.height() > 0.0);
                            labels.push(label);
                        }
                        let r = danger_icon_row(ui, &th, "Remove");
                        assert!(!r.clicked());
                        labels.push("Remove".to_string());
                    });
                }));
            }
            let out = last.unwrap();
            let mut xs = Vec::new();
            for l in &labels {
                let x = label_xs(&ctx, &out, l);
                assert_eq!(x.len(), 1, "{lang:?} {l}");
                xs.push(x[0]);
            }
            let first = xs[0];
            assert!(xs.iter().all(|x| (x - first).abs() < 0.5), "{lang:?} {xs:?}");
        }
    }

    /// Every menu row maps to the command it ran before the move to the kit.
    #[test]
    fn browser_menus_keep_their_actions() {
        let panels = include_str!("shell/panels.rs");
        for needle in [
            "&s.cat.t(\"browser.open\"), ctx_menu::OPEN)",
            "&s.cat.t(\"browser.rename\"), ctx_menu::RENAME)",
            "&s.cat.t(\"browser.delete\"), ctx_menu::DELETE)",
        ] {
            assert!(panels.contains(needle), "{needle}");
        }
        let dv = include_str!("drafting_view.rs");
        for needle in ["Row::Open(d.id)", "Row::Rename(d.id)", "Row::Duplicate(d.id)", "Row::Delete(d.id)"] {
            assert!(dv.contains(needle), "{needle}");
        }
    }
}
