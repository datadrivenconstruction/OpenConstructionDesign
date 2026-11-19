// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Autosave settings: one section for the settings page and the autosave window.
//! Numbers are kit number fields with a muted unit; the host decides how a row is laid out.

use crate::prefs::{Prefs, REMIND_MIN};
use ddc_ui::theme::{control, space, Role};
use ddc_ui::widgets::{self, NumberField, NumberFormat};
use ddc_ui::{Catalog, Theme};

/// Label key and hint key of every row, in display order.
pub const ROWS: [[&str; 2]; 5] = [
    ["autosave.interval", "autosave.interval_desc"],
    ["autosave.edits", "autosave.edits_tip"],
    ["autosave.keep", "autosave.keep_desc"],
    ["autosave.folder", "autosave.folder_desc"],
    ["autosave.remind", "autosave.remind_desc"],
];

const NUM_W: f32 = 112.0;
const FOLDER_W: f32 = 220.0;

/// What the user did in this frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AutosaveOut {
    /// A preference changed and should be saved.
    pub changed: bool,
    /// The folder button was pressed.
    pub browse: bool,
    /// The folder text lost focus (apply it then, not per keystroke).
    pub left_folder: bool,
}

/// Lays out one row: `keys` = [label, hint], `add` fills the value cell.
pub type RowFn<'r> = &'r mut dyn FnMut(&mut egui::Ui, [&'static str; 2], &mut dyn FnMut(&mut egui::Ui));

/// A whole number in a kit field with a muted unit, named by its row `label`. True when the
/// value changed.
#[allow(clippy::too_many_arguments)]
pub fn int_field(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    salt: &str,
    label: &str,
    value: &mut f64,
    (lo, hi): (f64, f64),
    unit: &str,
) -> bool {
    let out = NumberField::new(salt, value, NumberFormat::for_lang(cat.lang))
        .labelled_by(egui::Id::new(salt).with("lbl"), label)
        .decimals(0)
        .step(1.0)
        .range(lo, hi)
        .unit(unit)
        .width(NUM_W)
        .show(ui, th);
    *value = value.round().clamp(lo, hi);
    out.changed || out.committed
}

/// `unit.min` and friends carry a leading space for suffixes; a separate unit cell does not.
fn bare(s: String) -> String {
    s.trim().to_string()
}

/// Plural word without the number (`#` is absent in the unit strings).
fn word(cat: &Catalog, key: &str, n: u64) -> String {
    bare(cat.plural(key, n))
}

#[allow(clippy::too_many_arguments)]
fn count<T: Copy + Into<f64>>(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    (salt, label): (&str, &str),
    v: &mut T,
    range: (f64, f64),
    unit: &str,
    back: impl Fn(f64) -> T,
) -> bool {
    let mut x: f64 = (*v).into();
    let changed = int_field(ui, th, cat, salt, label, &mut x, range, unit);
    if changed {
        *v = back(x);
    }
    changed
}

/// Whether the snapshot schedule from the autosave prefs is due; the hosts never read the
/// interval and edit keys themselves.
pub fn is_due(p: &Prefs, changes: u64, elapsed: std::time::Duration) -> bool {
    crate::shell::recover_win::due(changes, elapsed, p.autosave_min, p.autosave_edits)
}

/// Interval, edits, copies, folder and reminder. `on` greys everything but the reminder (a
/// reminder is for a project that has no file yet, so it does not depend on autosave).
pub fn autosave_section(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    p: &mut Prefs,
    on: bool,
    row: RowFn<'_>,
) -> AutosaveOut {
    let mut out = AutosaveOut::default();
    let min = bare(cat.t("unit.min"));
    // each field is named by its row label
    let names: Vec<String> = ROWS.iter().map(|r| cat.t(r[0])).collect();
    row(ui, ROWS[0], &mut |ui| {
        ui.add_enabled_ui(on, |ui| {
            let id = ("ddc.autosave.min", names[0].as_str());
            let v = &mut p.autosave_min;
            out.changed |= count(ui, th, cat, id, v, (1.0, 60.0), &min, |x| x as u32);
        });
    });
    let edits = word(cat, "autosave_status.unit.edits", u64::from(p.autosave_edits));
    row(ui, ROWS[1], &mut |ui| {
        ui.add_enabled_ui(on, |ui| {
            let id = ("ddc.autosave.edits", names[1].as_str());
            let v = &mut p.autosave_edits;
            out.changed |= count(ui, th, cat, id, v, (0.0, 10_000.0), &edits, |x| x as u32);
        });
    });
    let copies = word(cat, "autosave_status.unit.copies", p.autosave_keep as u64);
    row(ui, ROWS[2], &mut |ui| {
        ui.add_enabled_ui(on, |ui| {
            let mut k = p.autosave_keep as f64;
            let salt = "ddc.autosave.keep";
            if int_field(ui, th, cat, salt, &names[2], &mut k, (1.0, 50.0), &copies) {
                p.autosave_keep = k as usize;
                out.changed = true;
            }
        });
    });
    row(ui, ROWS[3], &mut |ui| {
        let hint = p.autosave_folder().map(|d| d.display().to_string()).unwrap_or_default();
        ui.add_enabled_ui(on, |ui| {
            // the button is capped at the room left, so the field leaves it its full width
            let browse = cat.t("autosave.browse");
            let text_w = ui
                .painter()
                .layout_no_wrap(browse.clone(), th.font(Role::Body), th.sem().text_primary)
                .size()
                .x;
            let btn_w = (text_w + 2.0 * space::MD).max(control::BUTTON_MIN_W);
            // a pixel of slack: the field's rounded width must not squeeze the button's text
            let room = ui.available_width() - btn_w - ui.spacing().item_spacing.x - 1.0;
            // «Browse…» always stands right of the path, in a host row of either direction
            let rtl = ui.layout().main_dir() == egui::Direction::RightToLeft;
            let button = |ui: &mut egui::Ui| {
                widgets::Button::secondary(&browse).min_width(btn_w).show(ui, th).clicked()
            };
            let pressed_first = rtl && button(ui);
            let edit = widgets::TextField::new(&mut p.autosave_dir)
                .id_salt("ddc.autosave.dir")
                .labelled_by(egui::Id::new("ddc.autosave.dir").with("lbl"), &names[3])
                .hint(&hint)
                .width(room.clamp(control::FIELD_MIN_W, FOLDER_W))
                .show(ui, th)
                .response;
            // named by its row: an empty folder with no hint would leave it nameless
            ui.ctx().accesskit_node_builder(edit.id, |n| n.set_label(names[3].as_str()));
            let pressed_last = !rtl && button(ui);
            out.browse |= pressed_first || pressed_last;
            out.changed |= edit.changed();
            out.left_folder |= edit.lost_focus();
        });
    });
    row(ui, ROWS[4], &mut |ui| {
        let mut remind = p.remind_min > 0;
        if widgets::toggle(ui, th, &mut remind, &cat.t("settings.remind_on")).changed() {
            p.remind_min = if remind { 30 } else { 0 };
            out.changed = true;
        }
        // the field exists only while on: a numeric field clamps, and off (0) is below the minimum
        if remind {
            let mut x = f64::from(p.remind_min);
            let range = (f64::from(REMIND_MIN), 240.0);
            if int_field(ui, th, cat, "ddc.autosave.remind", &names[4], &mut x, range, &min) {
                p.remind_min = x as u32;
                out.changed = true;
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::Lang;

    /// Runs the section headless with a plain grid host; returns the rows laid out and the output.
    fn run(lang: Lang, p: &mut Prefs, on: bool) -> (usize, AutosaveOut) {
        let th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let cat = Catalog::new(lang);
        let (mut rows, mut out) = (0, AutosaveOut::default());
        ddc_ui::theme::headless_ctx().run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut host = |ui: &mut egui::Ui, keys: [&'static str; 2], add: &mut dyn FnMut(&mut egui::Ui)| {
                    assert!(!cat.t(keys[0]).is_empty() && cat.t(keys[0]) != keys[0], "{lang:?} {}", keys[0]);
                    assert!(cat.t(keys[1]) != keys[1], "{lang:?} {}", keys[1]);
                    rows += 1;
                    ui.horizontal(|ui| add(ui));
                };
                out = autosave_section(ui, &th, &cat, p, on, &mut host);
            });
        });
        (rows, out)
    }

    #[test]
    fn renders_every_row_in_both_languages_without_touching_values() {
        for lang in [Lang::En, Lang::Ru] {
            for on in [true, false] {
                let mut p = Prefs::default();
                p.remind_min = 30;
                let before = (p.autosave_min, p.autosave_edits, p.autosave_keep, p.remind_min);
                let (rows, out) = run(lang, &mut p, on);
                assert_eq!(rows, ROWS.len());
                assert!(!out.changed && !out.browse, "rendering alone changes nothing");
                assert_eq!(before, (p.autosave_min, p.autosave_edits, p.autosave_keep, p.remind_min));
            }
        }
    }

    #[test]
    fn reminder_off_is_not_lifted_to_the_minimum() {
        let mut p = Prefs::default();
        p.remind_min = 0;
        run(Lang::En, &mut p, true);
        assert_eq!(p.remind_min, 0);
    }

    #[test]
    fn unit_words_follow_the_language() {
        let (en, ru) = (Catalog::new(Lang::En), Catalog::new(Lang::Ru));
        assert_eq!(word(&en, "autosave_status.unit.copies", 1), "copy");
        assert_eq!(word(&en, "autosave_status.unit.copies", 5), "copies");
        assert_eq!(word(&ru, "autosave_status.unit.copies", 2), "\u{43a}\u{43e}\u{43f}\u{438}\u{438}");
        assert_eq!(word(&ru, "autosave_status.unit.edits", 5), "\u{43f}\u{440}\u{430}\u{432}\u{43e}\u{43a}");
        assert_eq!(bare(en.t("unit.min")), "min");
    }

    #[test]
    fn both_hosts_use_the_section_with_the_same_prefs_keys() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/shell");
        for f in ["settings_win.rs", "recover_win.rs"] {
            let s = std::fs::read_to_string(dir.join(f)).unwrap();
            let body = s.split("mod tests {").next().unwrap_or("");
            assert!(body.contains("autosave_section("), "{f}");
            assert!(!body.contains("egui::DragValue"), "{f}");
            for key in ["p.autosave_min", "p.autosave_edits", "p.autosave_keep"] {
                assert!(!body.contains(key), "{f}: {key} is edited only by the shared section");
            }
        }
        let own = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui_kit/autosave.rs")).unwrap();
        let body = own.split("#[cfg(test)]").next().unwrap_or("");
        for key in ["autosave_min", "autosave_edits", "autosave_keep", "autosave_dir", "remind_min"] {
            assert!(body.contains(&format!("p.{key}")), "{key}");
        }
    }
}
