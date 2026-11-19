// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Apply gate of the workbook preview: why Apply is off (a locale key, shown as the button
//! tooltip and as one line under the footer) and the single tick that confirms deletions.
//! Only the window reads it; what an apply writes is decided by `xlsx_win::apply_begin`.
use super::xlsx_issues::{self, IssueAction};
use crate::xl_text::{self, Lang};
use ddc_io::xlsx::Report;
use egui::{self, RichText};
use std::collections::BTreeMap;

/// What the footer knows when it draws Apply.
#[derive(Clone, Copy, Debug, Default)]
pub struct GateIn {
    pub busy: bool,
    /// Blocking errors of the book.
    pub errors: usize,
    /// Rows matched by name still waiting for a decision.
    pub pending: usize,
    /// Elements the apply deletes.
    pub deleted: usize,
    /// The deletion is a mass one (its tick lives in the mass-deletion card).
    pub mass: bool,
    /// The one deletion tick (`XlsxPreview::confirm_mass`).
    pub ticked: bool,
    /// Elements that take a type or layer change of the book (`type_use::report_users`).
    pub type_users: usize,
    /// The type tick holds for this report and this count (`types_ok`).
    pub types_ticked: bool,
    /// A book without comparison data, not confirmed.
    pub no_base: bool,
    /// Rows created by the last apply still have no id in the book.
    pub no_ids: bool,
    /// Something to write: changes, accepted proposals or matches.
    pub work: bool,
}

/// Locale key of the reason Apply is off; None when it may run. Every disabled state has one.
pub fn blocked(g: &GateIn) -> Option<&'static str> {
    if g.busy {
        Some("xlw.export_running")
    } else if g.errors > 0 {
        Some("xlw.fix_the_errors_in_the_workbook_and")
    } else if g.pending > 0 {
        Some("xlsx_confirm.decide_first")
    } else if (g.mass || g.deleted > 0) && !g.ticked {
        Some(if g.mass { "xlw.confirm_the_mass_deletion_first" } else { "xlsx_delete.tick_delete_first" })
    } else if g.type_users > 0 && !g.types_ticked {
        Some("type_change.tick_types_first")
    } else if g.no_base {
        Some("xlsx_no_base.confirm_first")
    } else if g.no_ids {
        Some("xlw.close_the_workbook_and_export_it_a")
    } else if !g.work {
        Some("xlw.nothing_to_apply")
    } else {
        None
    }
}

/// The mass-deletion card (and its tick) is painted.
pub fn mass_card(r: &Report, delete_missing: bool) -> bool {
    delete_missing && !r.mass_deletes().is_empty()
}

/// The window paints its own tick: deletions without the mass card.
pub fn plain_tick(r: &Report, delete_missing: bool) -> bool {
    r.counts().deleted > 0 && !mass_card(r, delete_missing)
}

/// Deletion ticks on screen: the mass card's or the plain one, never both.
pub fn ticks(r: &Report, delete_missing: bool) -> usize {
    let card = xlsx_issues::issue_cards(r, delete_missing)
        .iter()
        .filter(|c| c.action == IssueAction::ConfirmMass)
        .count();
    card + plain_tick(r, delete_missing) as usize
}

/// English refusal of an Apply whose type change is not ticked (`xl.msg.xs67_types_first`).
pub const TYPES_UNCONFIRMED: &str = "confirm the type changes first";

/// The type tick holds for one report and one count: a new read, or a book that now reaches
/// another number of elements, asks again. Nothing to reach needs no tick.
pub fn types_ok(tick: Option<(u64, usize)>, report_gen: u64, users: usize) -> bool {
    users == 0 || tick == Some((report_gen, users))
}

/// The type tick with the total it applies to; `tick` keeps (report, count) while on.
pub fn types_tick(
    ui: &mut egui::Ui,
    ru: bool,
    (gen, users): (u64, usize),
    tick: &mut Option<(u64, usize)>,
) {
    let th = ddc_ui::theme::of(ui.ctx());
    let mut on = types_ok(*tick, gen, users);
    let t = crate::tr_lang(ru, "type_change.tick_types", &[("n", &users.to_string())]);
    ddc_ui::widgets::checkbox(ui, &th, &mut on, &t);
    *tick = on.then_some((gen, users));
}

/// One pointer click applies once the deletion is ticked; no second confirmation.
pub fn go(ticked: bool, deleted: usize, clicked: bool) -> bool {
    clicked && (deleted == 0 || ticked)
}

/// "Walls 2, Doors 1": deleted rows per sheet, in the UI language.
pub fn deleted_list(r: &Report, lang: Lang) -> String {
    let mut per: BTreeMap<&str, usize> = BTreeMap::new();
    for c in r.changes.iter().filter(|c| c.kind() == "deleted") {
        *per.entry(c.sheet.as_str()).or_default() += 1;
    }
    per.iter()
        .map(|(s, n)| format!("{} {n}", xl_text::sheet_name(s, lang)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The plain deletion tick with the elements it removes (up to five, then "and N more").
pub fn tick(ui: &mut egui::Ui, r: &Report, lang: Lang, lines: &(Vec<String>, usize), confirm: &mut bool) {
    let ru = lang == Lang::Ru;
    let th = ddc_ui::theme::of(ui.ctx());
    let t = crate::tr_lang(ru, "xlw.mass_delete_confirm", &[("list", &deleted_list(r, lang))]);
    ddc_ui::widgets::checkbox(ui, &th, confirm, &t);
    let sec = th.c("text.secondary");
    for line in &lines.0 {
        ui.label(RichText::new(line).color(sec));
    }
    if lines.1 > 0 {
        let m = lines.1.to_string();
        ui.label(RichText::new(crate::tr_lang(ru, "xlw.delete_more", &[("m", &m)])).color(sec));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_io::xlsx::{Change, SheetDeletion};

    fn deleted(sheet: &str, id: i64) -> Change {
        Change {
            sheet: sheet.into(),
            row: 1 + id as u32,
            col: 0,
            column: "id".into(),
            id: Some(id),
            old: String::new(),
            new: "deleted".into(),
        }
    }

    fn report(n: usize, total: usize) -> Report {
        let mut r = Report::default();
        for i in 0..n {
            r.changes.push(deleted("Walls", i as i64));
        }
        if n > 0 {
            r.deletions.push(SheetDeletion { sheet: "Walls".into(), deleted: n, total });
        }
        r
    }

    fn ready() -> GateIn {
        GateIn { work: true, ..Default::default() }
    }

    #[test]
    fn blocked_apply_has_reason() {
        let cases: [(GateIn, &str); 8] = [
            (GateIn { busy: true, ..ready() }, "xlw.export_running"),
            (GateIn { errors: 2, ..ready() }, "xlw.fix_the_errors_in_the_workbook_and"),
            (GateIn { pending: 1, ..ready() }, "xlsx_confirm.decide_first"),
            (GateIn { deleted: 3, mass: true, ..ready() }, "xlw.confirm_the_mass_deletion_first"),
            (GateIn { deleted: 1, ..ready() }, "xlsx_delete.tick_delete_first"),
            (GateIn { no_base: true, ..ready() }, "xlsx_no_base.confirm_first"),
            (GateIn { no_ids: true, ..ready() }, "xlw.close_the_workbook_and_export_it_a"),
            (GateIn::default(), "xlw.nothing_to_apply"),
        ];
        for (g, key) in cases {
            assert_eq!(blocked(&g), Some(key), "{g:?}");
            // the tooltip and the line under the footer read in both languages, explicitly
            let (en, ru) = (crate::tr_str(false, key), crate::tr_str(true, key));
            assert!(!en.is_empty() && en != key, "en {key}");
            assert!(!ru.is_empty() && ru != key, "ru {key}");
            assert_ne!(en, ru, "{key} is translated");
        }
        assert_eq!(blocked(&ready()), None);
        assert_eq!(blocked(&GateIn { deleted: 4, mass: true, ticked: true, ..ready() }), None);
        assert_eq!(blocked(&GateIn { deleted: 1, ticked: true, ..ready() }), None);
    }

    #[test]
    fn single_delete_confirmation() {
        // small, mass, and "delete" column rows read without the delete-missing mode
        for (n, total, dm) in [(1usize, 100usize, true), (5, 10, true), (10, 10, true), (2, 10, false)] {
            let r = report(n, total);
            assert_eq!(ticks(&r, dm), 1, "n {n} of {total}, delete missing {dm}");
            assert_eq!(mass_card(&r, dm), dm && n * 5 > total, "n {n} of {total}");
        }
        assert_eq!(ticks(&Report::default(), true), 0, "nothing deleted: no tick");
        // the tick is the only confirmation: one click applies, unticked never
        assert!(!go(false, 2, true));
        assert!(go(true, 2, true));
        assert!(!go(true, 2, false));
        assert!(go(false, 0, true), "no deletions: no tick needed");
    }

    #[test]
    fn a_type_change_with_users_needs_its_own_tick() {
        let g = GateIn { type_users: 124, ..ready() };
        assert_eq!(blocked(&g), Some("type_change.tick_types_first"));
        assert_eq!(blocked(&GateIn { types_ticked: true, ..g }), None);
        assert_eq!(blocked(&GateIn { type_users: 0, ..ready() }), None, "no users, no tick");
        // a deletion tick never stands in for the type tick
        let both = GateIn { deleted: 1, ticked: true, ..g };
        assert_eq!(blocked(&both), Some("type_change.tick_types_first"));
        for key in ["type_change.tick_types_first", "type_change.tick_types"] {
            let (en, ru) = (crate::tr_str(false, key), crate::tr_str(true, key));
            assert!(en != key && ru != key && en != ru, "{key}");
        }
        let en = crate::tr_lang(false, "type_change.tick_types", &[("n", "124")]);
        let ru = crate::tr_lang(true, "type_change.tick_types", &[("n", "124")]);
        assert!(en.contains("124") && ru.contains("124"), "{en} / {ru}");
        assert!(ru.chars().any(|c| ('\u{400}'..='\u{4FF}').contains(&c)), "{ru}");
    }

    #[test]
    fn the_type_tick_holds_for_one_report_and_one_count() {
        assert!(types_ok(None, 3, 0), "nothing to reach");
        assert!(!types_ok(None, 3, 5));
        assert!(types_ok(Some((3, 5)), 3, 5));
        assert!(!types_ok(Some((3, 5)), 4, 5), "a new read asks again");
        assert!(!types_ok(Some((3, 5)), 3, 6), "another count asks again");
        let ru = crate::xl_text::tr(TYPES_UNCONFIRMED, Lang::Ru);
        assert!(ru != TYPES_UNCONFIRMED && !ru.is_empty(), "{ru}");
    }

    #[test]
    fn type_tick_paints_headless_and_keeps_the_count() {
        let cx = egui::Context::default();
        let mut tick = Some((7u64, 3usize));
        let _ = cx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                types_tick(ui, false, (7, 3), &mut tick);
                types_tick(ui, true, (7, 3), &mut tick);
            });
        });
        assert_eq!(tick, Some((7, 3)), "painting never changes the tick");
        let _ = cx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| types_tick(ui, false, (8, 3), &mut tick));
        });
        assert_eq!(tick, None, "a tick of the old report is off");
    }

    #[test]
    fn deleted_list_counts_per_sheet() {
        let mut r = report(2, 10);
        r.changes.push(deleted("Openings", 7));
        let en = deleted_list(&r, Lang::En);
        assert!(en.contains(" 2") && en.contains(" 1") && en.contains(", "), "{en}");
        assert!(!deleted_list(&r, Lang::Ru).is_empty());
    }

    #[test]
    fn tick_paints_headless() {
        let r = report(2, 100);
        let mut th = ddc_ui::Theme::new(ddc_ui::theme::Mode::Light, ddc_ui::theme::Density::Comfortable);
        let cx = egui::Context::default();
        let _ = cx.run(Default::default(), |c| {
            th.apply(c);
        });
        let mut confirm = false;
        let lines = (vec!["Walls #0".to_string()], 3);
        let _ = cx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                tick(ui, &r, Lang::En, &lines, &mut confirm);
                tick(ui, &r, Lang::Ru, &lines, &mut confirm);
            });
        });
        assert!(!confirm, "painting never ticks");
    }
}
