// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Read 'X' as Walls": the card of the XLSX preview for a main sheet the user renamed. The
//! workbook is never written; a click only sets this import's sheet alias and re-reads.
use crate::tr_lang;
use ddc_io::xlsx::Report;
use ddc_ui::theme::{space, stroke};
use ddc_ui::widgets::{self, surface::Tone};
use ddc_ui::Theme;
use egui::{self, RichText};
use std::collections::HashMap;

/// What a click on the card asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadAsClick {
    /// Read the sheet `found` as the known sheet `expected` in this import.
    Read { expected: String, found: String },
    /// Drop the alias of `expected`.
    Undo { expected: String },
}

/// Number of sheets with a proposal or an active alias: the "notes" of the preview.
pub fn count(r: &Report, alias: &HashMap<String, String>) -> usize {
    r.candidates.len() + alias.len()
}

fn quoted(names: impl Iterator<Item = String>) -> String {
    names.map(|f| format!("'{f}'")).collect::<Vec<_>>().join(", ")
}

/// Text of the explanation for one proposal: key and arguments.
pub fn detail(expected: &str, found: &[(String, u32)]) -> (&'static str, Vec<(&'static str, String)>) {
    let sheet = expected.to_string();
    match found {
        [(f, score)] => (
            "xlsx_read_as.detail",
            vec![("found", format!("'{f}'")), ("sheet", sheet), ("score", score.to_string())],
        ),
        _ => (
            "xlsx_read_as.detail_many",
            vec![("found", quoted(found.iter().map(|f| f.0.clone()))), ("sheet", sheet)],
        ),
    }
}

fn say(ru: bool, key: &str, args: &[(&'static str, String)]) -> String {
    let a: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
    tr_lang(ru, key, &a)
}

/// Paints the proposals of `r` and the active aliases; returns the click of this frame.
pub fn show(
    ui: &mut egui::Ui,
    th: &Theme,
    ru: bool,
    r: &Report,
    alias: &HashMap<String, String>,
) -> Option<ReadAsClick> {
    if count(r, alias) == 0 {
        return None;
    }
    let mut click = None;
    let s = th.sem();
    let tone = Tone::Warning;
    let inner_w = ui.available_width() - 2.0 * (space::MD + stroke::HAIRLINE);
    egui::Frame::new()
        .fill(tone.background(s))
        .stroke(egui::Stroke::new(stroke::HAIRLINE, tone.foreground(s)))
        .corner_radius(th.radius("lg"))
        .inner_margin(egui::Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.set_min_width(inner_w.max(0.0));
            ui.horizontal(|ui| {
                widgets::surface::badge(ui, th, &count(r, alias).to_string(), tone);
                ui.label(RichText::new(tr_lang(ru, "xlsx_read_as.title", &[])).strong().color(s.text_primary));
            });
            let mut active: Vec<(&String, &String)> = alias.iter().collect();
            active.sort();
            for (expected, found) in active {
                let args = [("found", found.clone()), ("sheet", expected.clone())];
                ui.label(RichText::new(say(ru, "xlsx_read_as.active", &args)).color(s.text_secondary));
                let t = say(ru, "xlsx_read_as.undo", &[("sheet", expected.clone())]);
                let tip = tr_lang(ru, "xlsx_read_as.undo_tip", &[]);
                if widgets::Button::secondary(&t).small().show(ui, th).on_hover_text(tip).clicked() {
                    click = Some(ReadAsClick::Undo { expected: expected.clone() });
                }
            }
            for c in &r.candidates {
                let found: Vec<(String, u32)> = c.found.iter().map(|f| (f.sheet.clone(), f.score)).collect();
                let (key, args) = detail(&c.expected, &found);
                ui.label(RichText::new(say(ru, key, &args)).color(s.text_secondary));
                ui.add_space(space::XS);
                ui.horizontal_wrapped(|ui| {
                    for (name, _) in &found {
                        let args = [("found", name.clone()), ("sheet", c.expected.clone())];
                        let t = say(ru, "xlsx_read_as.read_as", &args);
                        let tip = tr_lang(ru, "xlsx_read_as.read_as_tip", &[]);
                        if widgets::Button::secondary(&t).small().show(ui, th).on_hover_text(tip).clicked() {
                            click = Some(ReadAsClick::Read { expected: c.expected.clone(), found: name.clone() });
                        }
                    }
                });
            }
        });
    ui.add_space(space::XS);
    click
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_io::xlsx::{Renamed, SheetCandidate};

    fn report(found: &[(&str, u32)]) -> Report {
        let mut r = Report::default();
        r.candidates.push(SheetCandidate {
            expected: "Walls".into(),
            found: found.iter().map(|(s, p)| Renamed { sheet: (*s).into(), score: *p }).collect(),
        });
        r
    }

    #[test]
    fn nothing_to_show_without_candidates_or_aliases() {
        assert_eq!(count(&Report::default(), &HashMap::new()), 0);
    }

    #[test]
    fn one_candidate_and_two_candidates_use_different_texts() {
        let one = detail("Walls", &[("A".into(), 92)]);
        let two = detail("Walls", &[("A".into(), 92), ("B".into(), 90)]);
        assert_eq!(one.0, "xlsx_read_as.detail");
        assert_eq!(two.0, "xlsx_read_as.detail_many");
        assert!(two.1.iter().any(|(k, v)| *k == "found" && v == "'A', 'B'"));
    }

    #[test]
    fn every_text_resolves_in_both_languages() {
        for ru in [false, true] {
            for (k, args) in [
                ("xlsx_read_as.title", vec![]),
                ("xlsx_read_as.detail", vec![("found", "'A'".to_string()), ("sheet", "Walls".into()), ("score", "92".into())]),
                ("xlsx_read_as.detail_many", vec![("found", "'A', 'B'".to_string()), ("sheet", "Walls".into())]),
                ("xlsx_read_as.read_as", vec![("found", "A".to_string()), ("sheet", "Walls".into())]),
                ("xlsx_read_as.read_as_tip", vec![]),
                ("xlsx_read_as.active", vec![("found", "A".to_string()), ("sheet", "Walls".into())]),
                ("xlsx_read_as.undo", vec![("sheet", "Walls".to_string())]),
                ("xlsx_read_as.undo_tip", vec![]),
                ("xlsx_read_as.not_rewritten", vec![]),
            ] {
                let t = say(ru, k, &args);
                assert!(t != k && !t.is_empty() && !t.contains('{'), "{k} ru={ru}: {t}");
            }
        }
        assert_ne!(tr_lang(true, "xlsx_read_as.title", &[]), tr_lang(false, "xlsx_read_as.title", &[]));
    }

    #[test]
    fn card_paints_headless_and_reports_no_click() {
        let mut th = Theme::new(ddc_ui::theme::Mode::Light, ddc_ui::theme::Density::Comfortable);
        let cx = egui::Context::default();
        let _ = cx.run(Default::default(), |c| {
            th.apply(c);
        });
        let r = report(&[("A", 92), ("B", 90)]);
        let mut alias = HashMap::new();
        alias.insert("Rooms".to_string(), "C".to_string());
        let mut got = Vec::new();
        let _ = cx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                got.push(show(ui, &th, false, &r, &alias));
                got.push(show(ui, &th, true, &r, &alias));
                got.push(show(ui, &th, true, &Report::default(), &HashMap::new()));
            });
        });
        assert_eq!(got, [None, None, None]);
    }
}
