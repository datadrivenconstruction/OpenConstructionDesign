// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Query window (`data.query`, QY): the filter language of the palette in a window of its own.
//! The text is read live: an error in plain words, or the count and a summary by category.
//! Without `->` the primary action selects the result (the level follows when all of it is on
//! another one); with `-> field=value` it changes the result in one operation (one undo step).
//! A valid query can be saved as a project filter (one undo step); the project filters are
//! listed below and a click selects what one finds.

use std::cell::RefCell;

use crate::editor::{Editor, Sel};
use crate::query::{self, Expr};
use crate::shell::EditorApp;
use crate::ui_kit::{
    cancel_button, dialog_footer, empty_note, error_note, labeled_row_id, primary_click,
    secondary_button, section_title, Dialog, DialogSize,
};
use crate::units::ui_decimal_mark;
use ddc_ui::i18n::Catalog;
use ddc_ui::theme::space;
use ddc_ui::widgets::TextField;

/// What the typed text does on this model.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Outcome {
    Empty,
    Error(String),
    /// Elements found; `edit`: the text assigns after `->` (to them, or to the selection).
    Found { sels: Vec<Sel>, summary: String, edit: bool },
}

#[derive(Default)]
struct QueryUi {
    open: bool,
    focus: bool,
    text: String,
    /// (text, model revision, ru) the outcome was computed for.
    stamp: Option<(String, u64, bool)>,
    outcome: Option<Outcome>,
    /// Result line of the last action.
    msg: Option<String>,
}

thread_local! {
    static ST: RefCell<QueryUi> = RefCell::new(QueryUi::default());
}

/// Reads `text` against the model in the given language.
pub(crate) fn outcome(ed: &Editor, text: &str, ru: bool) -> Outcome {
    if text.trim().is_empty() {
        return Outcome::Empty;
    }
    let q = match query::canon_query(text, ui_decimal_mark()).and_then(|t| query::parse(&t)) {
        Ok(q) => q,
        Err(e) => return Outcome::Error(e.render(ru)),
    };
    let edit = !q.set.is_empty();
    let sels = if edit && q.expr == Expr::And(vec![]) {
        ed.selection.clone()
    } else {
        query::eval(ed, &q)
    };
    let rows = query::rows(ed);
    let found: Vec<&query::Row> = rows.iter().filter(|r| sels.contains(&r.sel)).collect();
    Outcome::Found {
        summary: query::summary(&found, ru),
        sels,
        edit,
    }
}

/// Applies an edit text (one undo step); the changed elements become the selection.
pub(crate) fn apply(ed: &mut Editor, text: &str, ru: bool) -> String {
    let run = query::canon_query(text, ui_decimal_mark()).and_then(|t| query::run_text(ed, &t));
    match run {
        Ok((sels, n)) => {
            ed.selection = sels;
            crate::tr_lang(ru, "query.status.changed", &[("n", &n.to_string())])
        }
        Err(e) => crate::tr_lang(ru, "query.status.not_changed", &[("e", &e.render(ru))]),
    }
}

fn h_open(_: &mut EditorApp) {
    ST.with(|s| {
        let mut s = s.borrow_mut();
        s.open = true;
        s.focus = true;
        s.msg = None;
    });
}

/// Registration in the shell (`shell::REGISTER`).
pub fn register(app: &mut EditorApp) {
    app.register_command("data.query", h_open);
}

enum Act {
    Select(Vec<Sel>),
    Apply,
    Save,
    Saved(String),
}

fn body(
    ui: &mut egui::Ui,
    c: &Catalog,
    ed: &Editor,
    st: &mut QueryUi,
    acts: &mut Vec<Act>,
    close: &mut bool,
) {
    let th = ddc_ui::theme::of(ui.ctx());
    let ru = c.lang == ddc_ui::Lang::Ru;
    let (label, hint) = (c.t("view_cmds.query.label"), c.t("view_cmds.query.hint"));
    let enter = labeled_row_id(ui, &label, |ui, label_id| {
        let out = TextField::new(&mut st.text)
            .id_salt("cmd1.query.text")
            .hint(&hint)
            .labelled_by(label_id, &label)
            .width(ui.available_width())
            .show(ui, &th);
        if std::mem::take(&mut st.focus) {
            out.response.request_focus();
        }
        if out.changed {
            st.msg = None;
        }
        out.response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
    });
    let stamp = (st.text.clone(), ed.revision, ru);
    if st.stamp.as_ref() != Some(&stamp) {
        st.outcome = Some(outcome(ed, &st.text, ru));
        st.stamp = Some(stamp);
    }
    ui.add_space(space::XS);
    let s = ddc_ui::theme::sem_of(ui.ctx());
    match st.outcome.as_ref().unwrap_or(&Outcome::Empty) {
        Outcome::Empty => {
            empty_note(ui, c.t("view_cmds.query.empty"), Some(c.t("view_cmds.query.empty_hint").as_str()))
        }
        Outcome::Error(e) => error_note(ui, e),
        Outcome::Found { sels, .. } if sels.is_empty() => {
            empty_note(ui, c.t("view_cmds.query.none"), Some(c.t("view_cmds.query.none_hint").as_str()))
        }
        Outcome::Found { sels, summary, .. } => {
            let n = sels.len().to_string();
            ui.label(
                egui::RichText::new(c.tf("view_cmds.query.found", &[("n", &n), ("what", summary)]))
                    .color(s.text_primary),
            );
        }
    }
    if let Some(m) = &st.msg {
        ui.label(egui::RichText::new(m).color(s.text_secondary));
    }
    let saved = query::saved(ed);
    section_title(ui, c.t("view_cmds.query.saved"));
    if saved.is_empty() {
        ui.label(egui::RichText::new(c.t("view_cmds.query.saved_none")).color(s.text_secondary));
    } else {
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 2.0 * space::XS;
        let height = (ui.available_height() - space::XXXL - space::LG).max(row_h * 2.0);
        egui::ScrollArea::vertical()
            .id_salt("view_cmds.query.saved")
            .auto_shrink([false, true])
            .max_height(height)
            .show(ui, |ui| {
                for q in &saved {
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), row_h),
                        egui::Sense::click(),
                    );
                    let fg = ddc_ui::widgets::paint_state(ui, &th, rect, &resp, false);
                    let inner = rect.shrink2(egui::vec2(space::SM, 0.0));
                    let font = th.font(ddc_ui::theme::Role::Body);
                    let g = ddc_ui::widgets::elide(ui, q, font, fg, inner.width());
                    let y = inner.center().y - g.size().y / 2.0;
                    ui.painter().galley(egui::pos2(inner.left(), y), g, fg);
                    if resp.on_hover_text(c.t("view_cmds.query.saved_tip")).clicked() {
                        acts.push(Act::Saved(q.clone()));
                    }
                }
            });
    }
    let found = match st.outcome.as_ref() {
        Some(Outcome::Found { sels, edit, .. }) => Some((sels.clone(), *edit)),
        _ => None,
    };
    dialog_footer(ui, |ui| {
        let n = found.as_ref().map_or(0, |f| f.0.len()).to_string();
        let edit = found.as_ref().is_some_and(|f| f.1);
        let why = match &found {
            None => Some(c.t("view_cmds.query.blocked")),
            Some((s, _)) if s.is_empty() => Some(c.t("view_cmds.query.none")),
            _ => None,
        };
        let label = if edit {
            c.tf("view_cmds.query.apply", &[("n", &n)])
        } else {
            c.tf("view_cmds.query.select", &[("n", &n)])
        };
        let go = primary_click(ui, label, why.as_deref()) || (enter && why.is_none());
        if go {
            match &found {
                Some((_, true)) => acts.push(Act::Apply),
                Some((s, false)) => acts.push(Act::Select(s.clone())),
                None => {}
            }
        }
        let can_save = found.is_some();
        let r = ui.add_enabled_ui(can_save, |ui| secondary_button(ui, c.t("view_cmds.query.save")));
        if r.inner.clicked() {
            acts.push(Act::Save);
        }
        if cancel_button(ui, c.t("find_replace.close")).clicked() {
            *close = true;
        }
    });
}

/// Per frame from the shell.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut st = ST.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if !st.open {
        ST.with(|s| *s.borrow_mut() = st);
        return;
    }
    let (mut open, mut close, mut acts) = (true, false, vec![]);
    {
        let c = &a.sh.cat;
        let ed = &a.ed;
        Dialog::new("view_cmds.query", c.t("cmd.data.query"), DialogSize::Tool)
            .open(&mut open)
            .show(ctx, |ui| body(ui, c, ed, &mut st, &mut acts, &mut close));
    }
    let ru = a.sh.cat.lang == ddc_ui::Lang::Ru;
    for act in acts {
        match act {
            Act::Select(s) => {
                query::select(a, s);
                st.msg = a.ed.status.clone();
            }
            Act::Apply => {
                let m = apply(&mut a.ed, &st.text, ru);
                a.ed.status = Some(m.clone());
                st.msg = Some(m);
                st.stamp = None;
            }
            Act::Save => {
                st.msg = Some(match query::save(&mut a.ed, &st.text) {
                    Ok(name) => crate::tr_lang(ru, "query.status.filter_saved", &[("name", &name)]),
                    Err(e) => crate::tr_lang(ru, "query.status.query_err", &[("e", &e.render(ru))]),
                });
            }
            Act::Saved(q) => match query::eval_saved(&a.ed, &q) {
                Ok(s) => {
                    query::select(a, s);
                    st.msg = a.ed.status.clone();
                }
                Err(e) => {
                    st.msg =
                        Some(crate::tr_lang(ru, "query.status.query_err", &[("e", &e.render(ru))]))
                }
            },
        }
    }
    st.open = open && !close;
    ST.with(|s| *s.borrow_mut() = st);
}

/// Fresh window state for the layout probes.
#[cfg(test)]
pub(crate) fn open_for_layout(text: &str) {
    ST.with(|s| {
        *s.borrow_mut() = QueryUi {
            open: true,
            text: text.into(),
            ..QueryUi::default()
        };
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> EditorApp {
        crate::shot_press::set_screenshot_mode(true);
        let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
        a.prefs.path = None;
        for (x0, x1) in [(0.0, 4.0), (0.0, 9.0)] {
            let spec = ddc_model::WallSpec {
                a: (x0, 0.0),
                b: (x1, 0.0),
                level: a.ed.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: a.ed.types[0].id,
                offset0: None,
            };
            a.ed.model.add_wall(&spec).unwrap();
        }
        a.ed.reload().unwrap();
        a
    }

    #[test]
    fn command_opens_the_window() {
        let mut a = app();
        assert!(a.run("data.query"));
        assert!(ST.with(|s| s.borrow().open));
        open_for_layout("");
    }

    #[test]
    fn outcome_reads_empty_errors_and_results_in_both_languages() {
        let a = app();
        for ru in [false, true] {
            assert_eq!(outcome(&a.ed, "  ", ru), Outcome::Empty);
            match outcome(&a.ed, "(walls", ru) {
                Outcome::Error(e) => assert!(!e.is_empty() && !e.contains("query."), "{e}"),
                o => panic!("{o:?}"),
            }
            match outcome(&a.ed, "walls", ru) {
                Outcome::Found { sels, summary, edit } => {
                    assert_eq!(sels.len(), 2);
                    assert!(!edit);
                    assert!(summary.contains('2'), "{summary}");
                }
                o => panic!("{o:?}"),
            }
        }
        // lengths without a unit are in project units (mm)
        match outcome(&a.ed, "walls length>5000", false) {
            Outcome::Found { sels, .. } => assert_eq!(sels.len(), 1),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn apply_is_one_undo_step_and_selects_the_changed() {
        let mut a = app();
        let h0: Vec<f64> = a.ed.walls.iter().map(|w| w.height).collect();
        let m = apply(&mut a.ed, "walls -> height=4000", false);
        assert!(m.contains('2'), "{m}");
        assert_eq!(a.ed.selection.len(), 2);
        assert!(a.ed.walls.iter().all(|w| (w.height - 4.0).abs() < 1e-6));
        a.run("edit.undo");
        let h1: Vec<f64> = a.ed.walls.iter().map(|w| w.height).collect();
        assert_eq!(h0, h1);
        // a broken edit writes nothing
        let rev = a.ed.revision;
        let m = apply(&mut a.ed, "walls -> nosuchfield=1", true);
        assert!(!m.is_empty());
        assert_eq!(a.ed.revision, rev);
    }
}
