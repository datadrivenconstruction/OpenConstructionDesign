//! Preview guard for a workbook without its baseline (`Report::no_base`): the hidden comparison
//! sheet was dropped by a tool that re-saved the book, damaged, or never written. Every differing
//! cell then replaces the model value, so the preview shows a warning card and Apply waits for an
//! explicit tick that holds for one report only: any re-read asks again.
use super::xlsx_issues::{IssueAction, IssueCard, IssueKind, Msg, Slot};
use super::xlsx_win::XlsxPreview;
use ddc_io::xlsx::Report;

/// English report text of the refusal to apply such a book before the tick.
pub(super) const UNCONFIRMED: &str = "confirm the workbook without comparison data first";

/// The tick of one report: kept with the `report_gen` it was given for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Confirm(Option<u64>);

impl Confirm {
    pub fn is_for(&self, gen: u64) -> bool {
        self.0 == Some(gen)
    }

    pub fn set(&mut self, gen: u64, on: bool) {
        self.0 = on.then_some(gen);
    }
}

/// The report writes cells that nothing checks against model edits made since the export.
pub fn needs_confirm(r: &Report) -> bool {
    r.no_base && (!r.changes.is_empty() || !r.props.is_empty())
}

pub(super) fn unconfirmed(pv: &XlsxPreview) -> bool {
    needs_confirm(&pv.report) && !pv.no_base_ok.is_for(pv.report_gen)
}

/// The warning card; it carries the confirmation only while there is something to write.
pub(super) fn issue_card(r: &Report) -> IssueCard {
    let n = r.changes.len() + r.props.len();
    let mut detail = vec![Msg { key: "xlsx_no_base.detail", args: vec![] }];
    let action = if needs_confirm(r) {
        detail.push(Msg { key: "xlsx_no_base.confirm", args: vec![("n", n.to_string())] });
        IssueAction::ConfirmNoBase
    } else {
        IssueAction::None
    };
    IssueCard { kind: IssueKind::NoBaseline, count: n, title: Msg { key: "xlsx_no_base.title", args: vec![] }, detail, action }
}

/// Paints the card with its tick (nothing for a book with a baseline); never applies.
pub(super) fn card(ui: &mut egui::Ui, pv: &mut XlsxPreview, ru: bool) {
    if !pv.report.no_base {
        return;
    }
    let th = ddc_ui::theme::of(ui.ctx());
    let cards = vec![issue_card(&pv.report)];
    let mut on = pv.no_base_ok.is_for(pv.report_gen);
    super::xlsx_issues::show(ui, &th, ru, &cards, Slot::NoBase, None, &mut on);
    pv.no_base_ok.set(pv.report_gen, on);
}

/// English report text of the refusal to delete rows of a book without a baseline.
pub(super) const NO_BASE_DELETE: &str = "a workbook without comparison data deletes nothing";

/// "Delete missing" as it really acts: never without a baseline, whatever the flag says
/// (set earlier for another book or kept across reads). Every UI site reads this, not the flag.
pub(super) fn effective_delete(pv: &XlsxPreview) -> bool {
    deletes(&pv.report, pv.delete_missing)
}

pub(super) fn deletes(r: &Report, delete_missing: bool) -> bool {
    delete_missing && !r.no_base
}

/// The report would delete rows although it has no baseline (the reader never builds one).
pub(super) fn deletes_without_base(r: &Report) -> bool {
    r.no_base && (r.counts().deleted > 0 || !r.deletions.is_empty())
}

/// True when the disabled "delete missing" toggle is shown: the book has no baseline and the
/// model has elements without a row.
pub(super) fn off_shown(r: &Report) -> bool {
    r.no_base && r.removed_count() > 0
}

/// Disabled, unticked "delete missing" checkbox with its reason. Takes nothing mutable: it
/// cannot arm.
pub(super) fn missing_off(ui: &mut egui::Ui, ru: bool) -> egui::Response {
    let th = ddc_ui::theme::of(ui.ctx());
    let label = crate::tr_str(ru, "xlsx_delete.delete_missing");
    let mut off = false;
    let r = ui
        .add_enabled_ui(false, |ui| ddc_ui::widgets::checkbox(ui, &th, &mut off, label))
        .inner;
    r.on_disabled_hover_text(crate::tr_str(ru, "xlsx_delete.no_base_reason"))
}

/// The user's tick, for tests of other features that read books without a baseline.
#[cfg(test)]
pub(super) fn confirm(pv: &mut XlsxPreview) {
    pv.no_base_ok.set(pv.report_gen, true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use crate::shell::xlsx_issues::issue_cards;
    use crate::shell::xlsx_win::{apply_preview, Applied};
    use crate::xl_text::Lang;
    use ddc_io::xlsx;
    use std::path::{Path, PathBuf};

    fn editor() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.level;
        let t = ed
            .model
            .add_wall_type("ext", &[ddc_model::LayerSpec { material: "brick".into(), thickness: 0.25 }])
            .unwrap();
        let p = [(0., 0.), (6., 0.), (6., 4.), (0., 4.)];
        for k in 0..4 {
            ed.model
                .add_wall(&ddc_model::WallSpec {
                    a: p[k],
                    b: p[(k + 1) % 4],
                    level: lv,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: t,
                    offset0: None,
                })
                .unwrap();
        }
        ed.reload().unwrap();
        ed
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc_app_xs10_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn walls(ed: &Editor) -> i64 {
        ed.model.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap()
    }

    fn heights(ed: &Editor) -> Vec<f64> {
        let mut st = ed.model.sql().prepare("SELECT height FROM wall ORDER BY id").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    }

    /// Values-only copy of `src` as another program saves it: no baseline sheet, the first
    /// wall's height set to `h`, the last `cut` wall rows gone.
    fn resaved(src: &Path, dst: &Path, h: f64, cut: usize) {
        use calamine::{open_workbook, Data, Reader, Xlsx};
        let mut wb: Xlsx<_> = open_workbook(src).unwrap();
        let mut out = rust_xlsxwriter::Workbook::new();
        for name in wb.sheet_names() {
            if name == "Baseline" {
                continue;
            }
            let mut g: Vec<Vec<Data>> = wb.worksheet_range(&name).unwrap().rows().map(|r| r.to_vec()).collect();
            if name == "Walls" {
                let c = g[0].iter().position(|d| d.to_string() == "height [m]").unwrap();
                g[1][c] = Data::Float(h);
                let keep = g.len() - cut;
                g.truncate(keep);
            }
            let ws = out.add_worksheet();
            ws.set_name(&name).unwrap();
            for (i, row) in g.iter().enumerate() {
                for (j, d) in row.iter().enumerate() {
                    let (i, j) = (i as u32, j as u16);
                    match d {
                        Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                        Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                        Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                        _ => Ok(()),
                    }
                    .unwrap();
                }
            }
        }
        out.save(dst).unwrap();
    }

    /// The editor and a preview of its export re-saved without baseline (`cut` rows removed).
    fn no_base_preview(tag: &str, delete_missing: bool, cut: usize) -> (Editor, XlsxPreview) {
        let mut ed = editor();
        let d = dir(tag);
        let src = d.join("src.xlsx");
        xlsx::export_xlsx(&ed.model, &src).unwrap();
        let p = d.join("book.xlsx");
        resaved(&src, &p, 3.5, cut);
        let mut pv = XlsxPreview::load(&mut ed.model, &p, delete_missing).unwrap();
        pv.rev = ed.content_snap();
        (ed, pv)
    }

    fn hash(ed: &Editor) -> u64 {
        ed.model.content_hash().unwrap()
    }

    #[test]
    fn book_with_baseline_has_no_card_and_applies_without_tick() {
        let mut ed = editor();
        let d = dir("with");
        let p = d.join("book.xlsx");
        xlsx::export_xlsx(&ed.model, &p).unwrap();
        let pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        assert!(!pv.report.no_base);
        assert!(issue_cards(&pv.report, false).iter().all(|c| c.kind != IssueKind::NoBaseline));
        assert!(!unconfirmed(&pv));
        // the control: the same export re-saved without its baseline sheet is flagged
        let (_, nb) = no_base_preview("with_nb", false, 0);
        assert!(nb.report.no_base);
    }

    #[test]
    fn book_without_baseline_warns_and_blocks_apply_until_ticked() {
        let (mut ed, mut pv) = no_base_preview("block", false, 0);
        assert!(pv.report.no_base);
        assert_eq!(pv.report.counts().changed, 1, "{:?}", pv.report.changes);
        let cards = issue_cards(&pv.report, pv.delete_missing);
        assert_eq!(cards[0].kind, IssueKind::NoBaseline, "the warning comes first");
        assert_eq!(cards[0].action, IssueAction::ConfirmNoBase);
        assert!(unconfirmed(&pv));
        let (h0, ops) = (hash(&ed), ed.model.op_top().ok());
        let r = apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En);
        assert_eq!(r, Err(UNCONFIRMED.to_string()));
        assert_eq!(hash(&ed), h0, "the model is untouched until the tick");
        assert_eq!(ed.model.op_top().ok(), ops);
        assert_eq!(pv.report.counts().changed, 1, "the preview stays whole");
        confirm(&mut pv);
        assert!(!unconfirmed(&pv));
        assert!(matches!(apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En), Ok(Applied::Done { .. })));
        assert_ne!(hash(&ed), h0);
        assert!((heights(&ed)[0] - 3.5).abs() < 1e-9);
        // one undo step takes it back
        ed.undo();
        assert!((heights(&ed)[0] - 3.0).abs() < 1e-9);
    }

    #[test]
    fn tick_holds_for_one_report_only() {
        let (mut ed, mut pv) = no_base_preview("once", false, 0);
        confirm(&mut pv);
        assert!(!unconfirmed(&pv));
        // a re-read (book saved again, a decision, a stale model) builds a new report: ask again
        pv.reload(&mut ed.model);
        assert!(pv.report.no_base && unconfirmed(&pv));
        // a new preview of the same book starts unticked
        let fresh = XlsxPreview::load(&mut ed.model, &pv.path, false).unwrap();
        assert!(unconfirmed(&fresh));
        let mut c = Confirm::default();
        assert!(!c.is_for(0));
        c.set(3, true);
        assert!(c.is_for(3) && !c.is_for(4));
        c.set(3, false);
        assert!(!c.is_for(3));
    }

    #[test]
    fn missing_rows_are_never_deleted_without_baseline() {
        let (mut ed, mut pv) = no_base_preview("del", true, 2);
        assert!(pv.report.no_base);
        assert_eq!(pv.report.counts().deleted, 0, "{:?}", pv.report.changes);
        assert!(pv.report.deletions.is_empty());
        let n = walls(&ed);
        confirm(&mut pv);
        assert!(matches!(apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En), Ok(Applied::Done { .. })));
        assert_eq!(walls(&ed), n, "rows missing from the book stay in the model");
    }

    #[test]
    fn unchanged_book_without_baseline_warns_but_asks_nothing() {
        let mut r = Report { no_base: true, ..Default::default() };
        assert!(!needs_confirm(&r));
        let c = issue_card(&r);
        assert_eq!((c.action, c.detail.len()), (IssueAction::None, 1));
        r.no_base = false;
        assert!(issue_cards(&r, true).is_empty());
    }

    #[test]
    fn texts_resolve_in_both_languages() {
        let (_, pv) = no_base_preview("txt", false, 0);
        let c = issue_card(&pv.report);
        for ru in [false, true] {
            for m in std::iter::once(&c.title).chain(c.detail.iter()) {
                let t = m.text(ru);
                assert!(!t.is_empty() && !t.contains('{') && t != m.key, "{} ru={ru}: {t}", m.key);
            }
            let t = crate::tr_lang(ru, "xlsx_no_base.confirm_first", &[]);
            assert!(!t.is_empty() && t != "xlsx_no_base.confirm_first", "ru={ru}");
        }
        assert_ne!(c.title.text(true), c.title.text(false));
    }

    #[test]
    fn card_paints_headless_and_the_tick_binds_to_the_report() {
        let (_, mut pv) = no_base_preview("paint", false, 0);
        let mut th = ddc_ui::Theme::new(ddc_ui::theme::Mode::Light, ddc_ui::theme::Density::Comfortable);
        let cx = egui::Context::default();
        let _ = cx.run(Default::default(), |c| {
            th.apply(c);
        });
        let _ = cx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| card(ui, &mut pv, false));
        });
        assert!(unconfirmed(&pv), "painting never ticks");
        pv.no_base_ok.set(pv.report_gen, true);
        let _ = cx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| card(ui, &mut pv, true));
        });
        assert!(!unconfirmed(&pv), "a painted tick stays for the same report");
    }

    #[test]
    fn delete_flag_set_earlier_is_inert_without_baseline() {
        // read without the flag, then the flag comes back on (kept from another book) and re-reads
        let (mut ed, mut pv) = no_base_preview("xs44_flag", false, 2);
        pv.delete_missing = true;
        pv.reload(&mut ed.model);
        pv.rev = ed.content_snap();
        assert!(pv.report.no_base && pv.delete_missing);
        assert!(pv.report.removed_count() > 0, "the model has elements without a row");
        assert!(!effective_delete(&pv), "no baseline: deletion is never in effect");
        assert_eq!(pv.report.counts().deleted, 0);
        assert!(pv.report.deletions.is_empty() && !deletes_without_base(&pv.report));
        let dm = effective_delete(&pv);
        assert!(issue_cards(&pv.report, dm).iter().all(|c| c.action != IssueAction::ConfirmMass));
        assert!(!crate::shell::xlsx_gate::plain_tick(&pv.report, dm));
        assert_eq!(crate::shell::xlsx_gate::ticks(&pv.report, dm), 0);
        let n = walls(&ed);
        confirm(&mut pv);
        assert!(matches!(apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En), Ok(Applied::Done { .. })));
        assert_eq!(walls(&ed), n, "nothing is deleted");
        assert!((heights(&ed)[0] - 3.5).abs() < 1e-9, "the other changes still apply");
    }

    /// Context with the named font weights bound: a bare one panics on kit text.
    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    fn texts(s: &egui::Shape, out: &mut Vec<String>) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|x| texts(x, out)),
            egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
            _ => {}
        }
    }

    /// One frame of `missing_off`; the response's (enabled, clicked, rect) and painted texts.
    fn off_frame(ctx: &egui::Context, r: &Report, ru: bool, events: Vec<egui::Event>) -> (Option<(bool, bool, egui::Rect)>, Vec<String>) {
        let mut got = None;
        let raw = egui::RawInput { events, ..Default::default() };
        let out = ctx.run(raw, |cx| {
            egui::CentralPanel::default().show(cx, |ui| {
                got = off_shown(r)
                    .then(|| missing_off(ui, ru))
                    .map(|x| (x.enabled(), x.clicked(), x.rect));
            });
        });
        let mut t = vec![];
        out.shapes.iter().for_each(|cs| texts(&cs.shape, &mut t));
        (got, t)
    }

    #[test]
    fn delete_missing_is_disabled_with_a_reason_headless_en_ru() {
        let gone = vec![xlsx::Removed { sheet: "Walls".into(), id: 3 }];
        let nb = Report { no_base: true, removed: gone.clone(), ..Default::default() };
        let base = Report { removed: gone, ..Default::default() };
        for ru in [false, true] {
            let ctx = font_ctx();
            let (got, t) = off_frame(&ctx, &nb, ru, vec![]);
            let (on, _, rect) = got.expect("shown while rows are missing");
            assert!(!on, "ru={ru}: disabled");
            let label = crate::tr_str(ru, "xlsx_delete.delete_missing");
            assert!(t.iter().any(|s| s == label), "ru={ru}: {t:?}");
            // a click on it arms nothing
            let at = rect.center();
            let press = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            let (got, _) = off_frame(&ctx, &nb, ru, vec![egui::Event::PointerMoved(at), press(true)]);
            assert_eq!(got.map(|g| g.1), Some(false));
            let (got, _) = off_frame(&ctx, &nb, ru, vec![press(false)]);
            assert_eq!(got.map(|g| (g.0, g.1)), Some((false, false)), "ru={ru}");
            // a book with a baseline keeps its own toggle; an unchanged one shows nothing
            assert!(off_frame(&ctx, &base, ru, vec![]).0.is_none());
            let empty = Report { no_base: true, ..Default::default() };
            assert!(off_frame(&ctx, &empty, ru, vec![]).0.is_none());
        }
        for key in ["xlsx_delete.delete_missing", "xlsx_delete.not_removed", "xlsx_delete.no_base_reason"] {
            let (en, ru) = (crate::tr_str(false, key), crate::tr_str(true, key));
            assert!(en != key && ru != key && en != ru, "{key}");
        }
    }

    type Grid = Vec<Vec<calamine::Data>>;

    /// Values-only copy of `src`: `f` edits each sheet and drops the ones it returns false for.
    fn rewritten(src: &Path, dst: &Path, f: impl Fn(&str, &mut Grid) -> bool) {
        use calamine::{open_workbook, Data, Reader, Xlsx};
        let mut wb: Xlsx<_> = open_workbook(src).unwrap();
        let mut out = rust_xlsxwriter::Workbook::new();
        for name in wb.sheet_names() {
            let range = wb.worksheet_range(&name).unwrap();
            let mut g: Grid = range.rows().map(|r| r.to_vec()).collect();
            if !f(&name, &mut g) {
                continue;
            }
            let ws = out.add_worksheet();
            ws.set_name(&name).unwrap();
            for (i, row) in g.iter().enumerate() {
                for (j, d) in row.iter().enumerate() {
                    let (i, j) = (i as u32, j as u16);
                    match d {
                        Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                        Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                        Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                        _ => Ok(()),
                    }
                    .unwrap();
                }
            }
        }
        out.save(dst).unwrap();
    }

    #[test]
    fn an_emptied_sheet_keeps_only_exported_walls_still_in_the_model() {
        let mut ed = editor();
        let d = dir("kept97");
        let src = d.join("src.xlsx");
        xlsx::export_xlsx(&ed.model, &src).unwrap();
        // since the export one wall went and one came: 4 exported, 3 of them are left
        let q = |s: &str| -> i64 { ed.model.sql().query_row(s, [], |r| r.get(0)).unwrap() };
        let (first, t) = (q("SELECT MIN(id) FROM wall"), q("SELECT MIN(wtype) FROM wall"));
        ed.model.delete_wall(first).unwrap();
        let spec = ddc_model::WallSpec {
            a: (0., 9.),
            b: (6., 9.),
            level: ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        ed.model.add_wall(&spec).unwrap();
        ed.reload().unwrap();
        let p = d.join("book.xlsx");
        rewritten(&src, &p, |name, g| {
            if name == "Walls" {
                g.truncate(1);
            }
            true
        });
        let pv = XlsxPreview::load(&mut ed.model, &p, true).unwrap();
        assert!(!pv.report.no_base, "{:?}", pv.report);
        let want = [xlsx::SheetIssue::new("Walls", xlsx::SheetIssueKind::Empty, 3)];
        assert_eq!(pv.report.sheet_issues, want);
        assert!(pv.report.removed.is_empty() && pv.report.counts().deleted == 0);
        let cards = issue_cards(&pv.report, true);
        let c = cards.iter().find(|c| c.kind == IssueKind::EmptySheets).expect("an empty sheet");
        for ru in [false, true] {
            let t = c.detail[0].text(ru);
            assert!(t.contains("Walls") && t.contains('3') && !t.contains('4'), "ru={ru}: {t}");
        }
        assert_eq!(walls(&ed), 4);
    }

    fn with_curtain() -> Editor {
        let mut ed = editor();
        let sec = ddc_model::MullionSection::Rect { w: 0.05, d: 0.1 };
        ed.model
            .add_curtain_wall(&ddc_model::CurtainSpec {
                level: ed.level,
                name: "CW".into(),
                a: (0.0, 6.0),
                b: (4.0, 6.0),
                bulge: 0.0,
                base_offset: 0.0,
                height: 3.0,
                u: ddc_model::GridLines::Spacing(1.0),
                v: ddc_model::GridLines::Spacing(1.5),
                interior: sec,
                border: sec,
                lines: Default::default(),
                top: vec![],
            })
            .unwrap();
        ed.reload().unwrap();
        ed
    }

    /// Issue cards of the export re-saved without baseline and curtain sheet, `cut` Info rows gone.
    fn without_curtains(tag: &str, cut: &[&str]) -> Vec<IssueCard> {
        let mut ed = with_curtain();
        let d = dir(tag);
        let src = d.join("src.xlsx");
        xlsx::export_xlsx(&ed.model, &src).unwrap();
        let p = d.join("book.xlsx");
        rewritten(&src, &p, |name, g| {
            if name == "Info" {
                g.retain(|r| !r.first().is_some_and(|k| cut.contains(&k.to_string().as_str())));
            }
            name != "Baseline" && name != "Curtain walls"
        });
        let pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        assert!(pv.report.no_base, "{:?}", pv.report);
        issue_cards(&pv.report, false)
    }

    #[test]
    fn a_book_older_than_the_curtain_sheet_does_not_miss_it() {
        let cards = without_curtains("old97", &["sheets", "revision"]);
        assert!(cards.iter().all(|c| c.kind != IssueKind::MissingSheets), "{cards:?}");
        for ru in [false, true] {
            for m in cards.iter().flat_map(|c| std::iter::once(&c.title).chain(&c.detail)) {
                assert!(!m.text(ru).contains("Curtain"), "ru={ru}: {}", m.text(ru));
            }
        }
    }

    #[test]
    fn a_book_that_had_the_curtain_sheet_names_it_missing() {
        // a current book lists its sheets; an older one with a revision row came after curtains
        for (tag, cut) in [("cur97", &[][..]), ("rev97", &["sheets"][..])] {
            let cards = without_curtains(tag, cut);
            let c = cards.iter().find(|c| c.kind == IssueKind::MissingSheets).expect(tag);
            assert_eq!(c.count, 1, "{tag}");
            for ru in [false, true] {
                let t = c.detail[0].text(ru);
                assert!(t.contains("Curtain walls"), "{tag} ru={ru}: {t}");
            }
        }
    }
}
