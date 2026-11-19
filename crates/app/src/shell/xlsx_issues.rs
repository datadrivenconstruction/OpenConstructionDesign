//! Issue cards of the XLSX preview: skipped rows, conflicts, missing and look-alike sheets and
//! a mass deletion, one compact card per kind. `issue_cards` is pure; `show` only paints and
//! reports a click. A click never changes the model: it picks a preview choice or opens the
//! workbook, and the explicit Apply button stays the only way to write.
use crate::tr_lang;
use ddc_io::xlsx::{Report, SheetIssue, SheetIssueKind};
use ddc_ui::theme::{space, stroke};
use ddc_ui::Theme;
use ddc_ui::widgets::{self, surface::Tone};
use egui::{self, RichText};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueKind {
    Skipped,
    Conflicts,
    MissingSheets,
    /// Sheets cleared or left with only their header: nothing read, their elements stay.
    EmptySheets,
    /// Sheets whose file part or formulas cannot be read: nothing read, their elements stay.
    UnreadableSheets,
    LookAlikes,
    MassDelete,
    /// The book has no whole baseline (`Report::no_base`).
    NoBaseline,
}

/// What the card offers. All actions only navigate or choose inside the preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueAction {
    None,
    /// Segmented [Keep model] [Take workbook] for every conflicting cell.
    PickConflicts,
    OpenWorkbook,
    /// Checkbox the Apply button waits for.
    ConfirmMass,
    /// Checkbox the Apply button waits for: a book without baseline overwrites model values.
    ConfirmNoBase,
}

/// A catalog key with its arguments; the language is picked when it is painted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Msg {
    pub key: &'static str,
    pub args: Vec<(&'static str, String)>,
}

impl Msg {
    fn new(key: &'static str, args: Vec<(&'static str, String)>) -> Self {
        Self { key, args }
    }
    pub fn text(&self, ru: bool) -> String {
        let a: Vec<(&str, &str)> = self.args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        tr_lang(ru, self.key, &a)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueCard {
    pub kind: IssueKind,
    pub count: usize,
    pub title: Msg,
    pub detail: Vec<Msg>,
    pub action: IssueAction,
}

impl IssueCard {
    pub fn tone(&self) -> Tone {
        if self.kind == IssueKind::MassDelete {
            Tone::Danger
        } else {
            Tone::Warning
        }
    }
}

/// Which part of the dialog a call paints: the cards under the chips, or the mass-delete
/// card under the delete toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Top,
    Mass,
    /// The no-baseline card, painted even while only errors are shown (it holds a confirmation).
    NoBase,
}

/// What a click on a card asks the window for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardClick {
    /// `true` - take the workbook value for every conflicting cell.
    PickAll(bool),
    OpenWorkbook,
}

fn count_arg(n: usize) -> Vec<(&'static str, String)> {
    vec![("n", n.to_string())]
}

fn quoted(names: impl Iterator<Item = String>) -> String {
    names.map(|f| format!("'{f}'")).collect::<Vec<_>>().join(", ")
}

/// The line naming one sheet that was not read, with its reason.
fn sheet_note(s: &SheetIssue) -> Msg {
    let sheet = ("sheet", s.sheet.clone());
    match s.kind {
        SheetIssueKind::Empty if s.kept > 0 => {
            Msg::new("xlsx_issues.empty_kept", vec![sheet, ("n", s.kept.to_string())])
        }
        SheetIssueKind::Empty => Msg::new("xlsx_issues.empty_none", vec![sheet]),
        SheetIssueKind::FormulasUnreadable => Msg::new("xlsx_issues.formulas", vec![sheet]),
        SheetIssueKind::Unreadable => Msg::new("xlsx_issues.unreadable", vec![sheet]),
        SheetIssueKind::Missing => Msg::new("xlw.sheet_missing", vec![sheet]),
    }
}

/// Cards for a report, in a fixed order, none for an empty kind.
pub fn issue_cards(r: &Report, delete_missing: bool) -> Vec<IssueCard> {
    let mut v = Vec::new();
    if r.no_base {
        v.push(super::xlsx_no_base::issue_card(r));
    }
    let skipped = r.skipped();
    if skipped > 0 {
        v.push(IssueCard {
            kind: IssueKind::Skipped,
            count: skipped,
            title: Msg::new("xlsx_issues.skipped_title", count_arg(skipped)),
            detail: vec![Msg::new("xlw.skipped_note", count_arg(skipped))],
            action: IssueAction::None,
        });
    }
    let nc = r.conflicts.len();
    if nc > 0 {
        v.push(IssueCard {
            kind: IssueKind::Conflicts,
            count: nc,
            title: Msg::new("xlsx_issues.conflicts_title", count_arg(nc)),
            detail: vec![Msg::new("xlw.conflicts_note", count_arg(nc))],
            action: IssueAction::PickConflicts,
        });
    }
    let of_kind = |k: fn(SheetIssueKind) -> bool| {
        r.sheet_issues.iter().filter(move |s| k(s.kind))
    };
    let missing: Vec<_> = of_kind(|k| k == SheetIssueKind::Missing).collect();
    let nm = missing.len();
    if nm > 0 {
        let detail = if nm == 1 {
            Msg::new("xlw.sheet_missing", vec![("sheet", missing[0].sheet.clone())])
        } else {
            let sheets = quoted(missing.iter().map(|s| s.sheet.clone()));
            Msg::new("xlsx_issues.sheets_missing", vec![("sheets", sheets)])
        };
        v.push(IssueCard {
            kind: IssueKind::MissingSheets,
            count: nm,
            title: Msg::new("xlsx_issues.missing_title", count_arg(nm)),
            detail: vec![detail],
            action: IssueAction::OpenWorkbook,
        });
    }
    let empty: Vec<Msg> = of_kind(|k| k == SheetIssueKind::Empty).map(sheet_note).collect();
    if !empty.is_empty() {
        v.push(IssueCard {
            kind: IssueKind::EmptySheets,
            count: empty.len(),
            title: Msg::new("xlsx_issues.empty_title", count_arg(empty.len())),
            detail: empty,
            action: IssueAction::OpenWorkbook,
        });
    }
    let bad: Vec<Msg> = of_kind(|k| {
        matches!(k, SheetIssueKind::Unreadable | SheetIssueKind::FormulasUnreadable)
    })
    .map(sheet_note)
    .collect();
    if !bad.is_empty() {
        v.push(IssueCard {
            kind: IssueKind::UnreadableSheets,
            count: bad.len(),
            title: Msg::new("xlsx_issues.unreadable_title", count_arg(bad.len())),
            detail: bad,
            action: IssueAction::OpenWorkbook,
        });
    }
    let nl = r.look_alikes.len();
    if nl > 0 {
        let detail = r
            .look_alikes
            .iter()
            .map(|la| {
                let key = if la.found.len() > 1 { "xlw.sheet_look_alike_many" } else { "xlw.sheet_look_alike" };
                let found = quoted(la.found.iter().cloned());
                Msg::new(key, vec![("found", found), ("sheet", la.expected.clone())])
            })
            .collect();
        v.push(IssueCard {
            kind: IssueKind::LookAlikes,
            count: nl,
            title: Msg::new("xlsx_issues.look_alike_title", count_arg(nl)),
            detail,
            action: IssueAction::OpenWorkbook,
        });
    }
    let mass = if delete_missing { r.mass_deletes() } else { Vec::new() };
    if !mass.is_empty() {
        let list = mass
            .iter()
            .map(|d| format!("{} {}", d.sheet, d.deleted))
            .collect::<Vec<_>>()
            .join(", ");
        v.push(IssueCard {
            kind: IssueKind::MassDelete,
            count: mass.len(),
            title: Msg::new("xlsx_issues.mass_title", count_arg(mass.len())),
            detail: vec![
                Msg::new("xlw.mass_delete_warning", vec![]),
                Msg::new("xlw.mass_delete_confirm", vec![("list", list)]),
            ],
            action: IssueAction::ConfirmMass,
        });
    }
    v
}

/// Current conflict choice: `Some(true)` all taken from the workbook, `Some(false)` none, `None` mixed.
pub fn conflict_choice(r: &Report) -> Option<bool> {
    if r.conflicts.is_empty() {
        return Some(false);
    }
    let taken = r.conflicts.iter().filter(|c| c.take_book).count();
    if taken == r.conflicts.len() {
        Some(true)
    } else if taken == 0 {
        Some(false)
    } else {
        None
    }
}

fn in_slot(c: &IssueCard, slot: Slot) -> bool {
    let own = match c.kind {
        IssueKind::MassDelete => Slot::Mass,
        IssueKind::NoBaseline => Slot::NoBase,
        _ => Slot::Top,
    };
    own == slot
}

/// Paints the cards of `slot`; returns the click of this frame, if any.
pub fn show(
    ui: &mut egui::Ui,
    th: &Theme,
    ru: bool,
    cards: &[IssueCard],
    slot: Slot,
    choice: Option<bool>,
    confirm_mass: &mut bool,
) -> Option<CardClick> {
    let mut click = None;
    let s = th.sem();
    for c in cards.iter().filter(|c| in_slot(c, slot)) {
        let tone = c.tone();
        let inner_w = ui.available_width() - 2.0 * (space::MD + stroke::HAIRLINE);
        egui::Frame::new()
            .fill(tone.background(s))
            .stroke(egui::Stroke::new(stroke::HAIRLINE, tone.foreground(s)))
            .corner_radius(th.radius("lg"))
            .inner_margin(egui::Margin::same(space::MD as i8))
            .show(ui, |ui| {
                ui.set_min_width(inner_w.max(0.0));
                ui.horizontal(|ui| {
                    // the baseline card states its count in the confirmation line, not as a bare number
                    if c.kind != IssueKind::NoBaseline {
                        widgets::surface::badge(ui, th, &c.count.to_string(), tone);
                    }
                    ui.label(RichText::new(c.title.text(ru)).strong().color(s.text_primary));
                });
                let confirm_at = if matches!(c.action, IssueAction::ConfirmMass | IssueAction::ConfirmNoBase) {
                    c.detail.len() - 1
                } else {
                    usize::MAX
                };
                for (i, d) in c.detail.iter().enumerate() {
                    if i != confirm_at {
                        ui.label(RichText::new(d.text(ru)).color(s.text_secondary));
                    }
                }
                ui.add_space(space::XS);
                match c.action {
                    IssueAction::None => {}
                    IssueAction::PickConflicts => {
                        let keep = tr_lang(ru, "xlsx_issues.keep_model", &[]);
                        let take = tr_lang(ru, "xlsx_issues.take_book", &[]);
                        let sel = match choice {
                            Some(true) => 1,
                            Some(false) => 0,
                            None => usize::MAX,
                        };
                        let r = ui.horizontal(|ui| widgets::segmented(ui, th, &[keep.as_str(), take.as_str()], sel)).inner;
                        if let Some(i) = r {
                            click = Some(CardClick::PickAll(i == 1));
                        }
                    }
                    IssueAction::OpenWorkbook => {
                        let t = tr_lang(ru, "xlw.open_workbook", &[]);
                        let tip = tr_lang(ru, "xlsx_issues.open_book_tip", &[]);
                        if widgets::Button::secondary(&t).small().show(ui, th).on_hover_text(tip).clicked() {
                            click = Some(CardClick::OpenWorkbook);
                        }
                    }
                    // the slot's own flag: the mass-delete or the no-baseline confirmation
                    IssueAction::ConfirmMass | IssueAction::ConfirmNoBase => {
                        let t = c.detail[confirm_at].text(ru);
                        widgets::checkbox(ui, th, confirm_mass, &t);
                    }
                }
            });
        ui.add_space(space::XS);
    }
    click
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_io::xlsx::{Conflict, LookAlike, Rejection, SheetDeletion};

    fn rej(row: u32) -> Rejection {
        Rejection { sheet: "Walls".into(), row, col: 3, column: "Height".into(), reason: "not a number".into() }
    }

    fn conflict(take: bool) -> Conflict {
        Conflict {
            sheet: "Walls".into(),
            row: 2,
            col: 3,
            column: "Height".into(),
            id: Some(1),
            base: "1".into(),
            book: "2".into(),
            model: "3".into(),
            take_book: take,
        }
    }

    fn full() -> Report {
        let mut r = Report::default();
        r.rejected.push(rej(5));
        r.rejected.push(rej(6));
        r.conflicts.push(conflict(false));
        r.sheet_issues.push(SheetIssue::missing("Doors"));
        r.look_alikes.push(LookAlike { expected: "Walls".into(), found: vec!["walls (2)".into()] });
        r.deletions.push(SheetDeletion { sheet: "Walls".into(), deleted: 9, total: 10 });
        r
    }

    #[test]
    fn empty_report_has_no_cards() {
        assert!(issue_cards(&Report::default(), true).is_empty());
    }

    #[test]
    fn cards_come_in_fixed_order_with_counts() {
        let c = issue_cards(&full(), true);
        let kinds: Vec<_> = c.iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            [
                IssueKind::Skipped,
                IssueKind::Conflicts,
                IssueKind::MissingSheets,
                IssueKind::LookAlikes,
                IssueKind::MassDelete
            ]
        );
        assert_eq!(c[0].count, 2);
        assert!(c.iter().skip(1).all(|c| c.count == 1));
    }

    #[test]
    fn no_card_for_a_kind_with_zero() {
        let mut r = Report::default();
        r.conflicts.push(conflict(false));
        let c = issue_cards(&r, false);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].kind, IssueKind::Conflicts);
    }

    #[test]
    fn mass_card_only_while_delete_is_on() {
        let r = full();
        assert!(issue_cards(&r, false).iter().all(|c| c.kind != IssueKind::MassDelete));
        assert!(issue_cards(&r, true).iter().any(|c| c.kind == IssueKind::MassDelete));
        let mut small = Report::default();
        small.deletions.push(SheetDeletion { sheet: "Walls".into(), deleted: 1, total: 100 });
        assert!(issue_cards(&small, true).is_empty());
    }

    #[test]
    fn actions_never_apply() {
        for c in issue_cards(&full(), true) {
            assert!(matches!(
                c.action,
                IssueAction::None | IssueAction::PickConflicts | IssueAction::OpenWorkbook | IssueAction::ConfirmMass
            ));
        }
        let c = issue_cards(&full(), true);
        assert_eq!(c[1].action, IssueAction::PickConflicts);
        assert_eq!(c[2].action, IssueAction::OpenWorkbook);
        assert_eq!(c[3].action, IssueAction::OpenWorkbook);
        assert_eq!(c[4].tone(), Tone::Danger);
        assert_eq!(c[0].tone(), Tone::Warning);
    }

    #[test]
    fn many_missing_sheets_name_all() {
        let mut r = Report::default();
        r.sheet_issues.push(SheetIssue::missing("Doors"));
        r.sheet_issues.push(SheetIssue::missing("Rooms"));
        let c = issue_cards(&r, false);
        assert_eq!(c[0].count, 2);
        let t = c[0].detail[0].text(false);
        assert!(t.contains("'Doors'") && t.contains("'Rooms'"), "{t}");
    }

    fn unread_sheets() -> Report {
        let mut r = Report::default();
        r.sheet_issues.push(SheetIssue::missing("Doors"));
        r.sheet_issues.push(SheetIssue::new("Walls", SheetIssueKind::Empty, 12));
        r.sheet_issues.push(SheetIssue::new("Rooms", SheetIssueKind::Empty, 0));
        r.sheet_issues.push(SheetIssue::new("Slabs", SheetIssueKind::Unreadable, 3));
        r.sheet_issues.push(SheetIssue::new("Levels", SheetIssueKind::FormulasUnreadable, 2));
        r
    }

    #[test]
    fn every_unread_sheet_is_named_with_its_reason() {
        let c = issue_cards(&unread_sheets(), false);
        let kinds: Vec<_> = c.iter().map(|c| (c.kind, c.count)).collect();
        assert_eq!(
            kinds,
            [
                (IssueKind::MissingSheets, 1),
                (IssueKind::EmptySheets, 2),
                (IssueKind::UnreadableSheets, 2)
            ]
        );
        let keys: Vec<&str> = c[1].detail.iter().chain(&c[2].detail).map(|m| m.key).collect();
        let want = ["xlsx_issues.empty_kept", "xlsx_issues.empty_none", "xlsx_issues.unreadable", "xlsx_issues.formulas"];
        assert_eq!(keys, want);
        for ru in [false, true] {
            assert!(c[1].detail[0].text(ru).contains("Walls"));
            assert!(c[1].detail[0].text(ru).contains("12"));
            assert!(c[1].detail[1].text(ru).contains("Rooms"));
            assert!(c[2].detail[0].text(ru).contains("Slabs"));
            assert!(c[2].detail[1].text(ru).contains("Levels"));
        }
        assert!(c.iter().all(|c| c.action == IssueAction::OpenWorkbook));
        assert!(c.iter().all(|c| c.tone() == Tone::Warning));
    }

    #[test]
    fn unread_sheet_cards_read_in_both_languages() {
        let cards = issue_cards(&unread_sheets(), false);
        for c in &cards {
            for m in std::iter::once(&c.title).chain(c.detail.iter()) {
                let (en, ru) = (m.text(false), m.text(true));
                for t in [&en, &ru] {
                    assert!(!t.is_empty() && !t.contains('{') && t != m.key, "{}: {t}", m.key);
                }
                assert_ne!(en, ru, "{}", m.key);
                assert!(ru.chars().any(|ch| ('\u{400}'..='\u{4ff}').contains(&ch)), "{ru}");
                assert!(en.chars().all(|ch| !('\u{400}'..='\u{4ff}').contains(&ch)), "{en}");
            }
        }
    }

    #[test]
    fn conflict_choice_follows_the_cells() {
        let mut r = Report::default();
        assert_eq!(conflict_choice(&r), Some(false));
        r.conflicts.push(conflict(false));
        assert_eq!(conflict_choice(&r), Some(false));
        r.conflicts.push(conflict(true));
        assert_eq!(conflict_choice(&r), None);
        r.conflicts[0].take_book = true;
        assert_eq!(conflict_choice(&r), Some(true));
    }

    #[test]
    fn every_text_resolves_in_both_languages() {
        for ru in [false, true] {
            for c in issue_cards(&full(), true) {
                for m in std::iter::once(&c.title).chain(c.detail.iter()) {
                    let t = m.text(ru);
                    assert!(!t.is_empty() && !t.contains('{') && t != m.key, "{} ru={ru}: {t}", m.key);
                }
            }
            for k in ["xlsx_issues.keep_model", "xlsx_issues.take_book", "xlsx_issues.open_book_tip", "xlw.open_workbook"] {
                let t = tr_lang(ru, k, &[]);
                assert!(t != k && !t.is_empty(), "{k} ru={ru}");
            }
        }
        assert_ne!(tr_lang(true, "xlsx_issues.keep_model", &[]), tr_lang(false, "xlsx_issues.keep_model", &[]));
    }

    #[test]
    fn cards_paint_headless_and_report_no_click() {
        let mut th = Theme::new(ddc_ui::theme::Mode::Light, ddc_ui::theme::Density::Comfortable);
        let cx = egui::Context::default();
        let _ = cx.run(Default::default(), |c| {
            th.apply(c);
        });
        let cards = issue_cards(&full(), true);
        let mut confirm = false;
        let mut got = Vec::new();
        let _ = cx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                got.push(show(ui, &th, false, &cards, Slot::Top, None, &mut confirm));
                got.push(show(ui, &th, true, &cards, Slot::Mass, Some(true), &mut confirm));
            });
        });
        assert_eq!(got, [None, None]);
        assert!(!confirm);
    }
}
