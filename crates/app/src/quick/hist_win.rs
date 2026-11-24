// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! History window: undo steps top to bottom, undone ones dimmed; a click jumps to that step.
use crate::shell::EditorApp;
use crate::ui_kit as kit;
use ddc_ui::theme::{space, Role};
use ddc_ui::widgets::{self, Button};

/// History rows: (depth after the step, label, whether undone). The first is the "Start" row (depth 0).
pub fn rows(app: &EditorApp) -> Vec<(usize, String, bool)> {
    let c = &app.sh.cat;
    let (done, redo) = app.ed.history();
    let name = |depth: usize, kind: &str| -> String {
        match app.quick.labels.get(&depth) {
            Some((k, l)) if k == kind => l.clone(),
            _ => c.t(kind),
        }
    };
    let mut v = vec![(0, c.t("quick.hist.start"), false)];
    for (i, k) in done.iter().enumerate() {
        v.push((i + 1, name(i, k), false));
    }
    let n = done.len();
    for (j, k) in redo.iter().enumerate() {
        v.push((n + j + 1, name(n + j, k), true));
    }
    v
}

/// Labels of steps that no longer exist: new steps (`fresh`) took the slots from depth
/// `undo_len - fresh`, everything undone after them was dropped; an empty history has no labels.
pub fn prune(
    labels: &mut std::collections::BTreeMap<usize, (String, String)>,
    undo_len: usize,
    redo_len: usize,
    fresh: u64,
) {
    if undo_len == 0 && redo_len == 0 {
        labels.clear();
        return;
    }
    if fresh > 0 {
        let from = undo_len.saturating_sub(fresh.min(undo_len as u64) as usize);
        labels.retain(|k, _| *k < from);
    }
}

/// AI batch behind each done step (index = depth - 1). Steps own journal operations from the
/// newest down, so a step is AI when one of its operations is a live AI batch.
pub fn ai_marks(app: &EditorApp) -> Vec<Option<ddc_model::AiBatch>> {
    let batches = app.ed.model.ai_batches().unwrap_or_default();
    if batches.is_empty() {
        return vec![None; app.ed.step_ops().len()];
    }
    step_seqs(app)
        .iter()
        .map(|seqs| batches.iter().find(|b| seqs.contains(&b.op_seq)).cloned())
        .collect()
}

/// Journal operations of each done step (index = depth - 1).
fn step_seqs(app: &EditorApp) -> Vec<Vec<i64>> {
    let ops = app.ed.step_ops();
    let total: usize = ops.iter().sum();
    let seqs: Vec<i64> = app
        .ed
        .model
        .journal(total)
        .unwrap_or_default()
        .iter()
        .map(|e| e.seq)
        .collect();
    let mut out = vec![vec![]; ops.len()];
    let mut at = 0;
    for (i, n) in ops.iter().enumerate().rev() {
        let from = at.min(seqs.len());
        let to = (at + n).min(seqs.len());
        at += n;
        out[i] = seqs[from..to].to_vec();
    }
    out
}

/// Tables a conflict may name, with the element word users know them by.
const KINDS: [(&str, &str); 22] = [
    ("wall", "history.kind.wall"),
    ("opening", "history.kind.opening"),
    ("curtain_wall", "history.kind.curtain_wall"),
    ("slab", "history.kind.slab"),
    ("room", "history.kind.room"),
    ("col", "history.kind.col"),
    ("beam", "history.kind.beam"),
    ("roof", "history.kind.roof"),
    ("ceiling", "history.kind.ceiling"),
    ("stair", "history.kind.stair"),
    ("railing", "history.kind.railing"),
    ("mep_run", "history.kind.mep_run"),
    ("comp", "history.kind.comp"),
    ("shaft", "history.kind.shaft"),
    ("ramp", "history.kind.ramp"),
    ("member", "history.kind.member"),
    ("level", "history.kind.level"),
    ("grid", "history.kind.grid"),
    ("mass", "history.kind.mass"),
    ("wall_type", "history.kind.wall_type"),
    ("material", "history.kind.material"),
    ("sheet", "history.kind.sheet"),
];

/// Element word for a model table; an unknown table reads as a plain "element".
pub fn kind_word(c: &ddc_ui::Catalog, table: &str) -> String {
    let key = KINDS
        .iter()
        .find(|k| k.0.eq_ignore_ascii_case(table))
        .map_or("history.kind.other", |k| k.1);
    c.t(key)
}

/// Why a revert conflicts, in the user's terms: the element (kind and name) and the later
/// history step that changed it. Never a journal number or a table name.
pub fn conflict_text(app: &EditorApp, with_op: i64, table: &str, row: Option<i64>) -> String {
    let c = &app.sh.cat;
    let kind = kind_word(c, table);
    let name = row.and_then(|r| app.ed.model.conflict_row_name(table, r));
    let what = match &name {
        Some(n) => c.tf("history.what.named", &[("kind", &kind), ("name", n)]),
        None => c.tf("history.what.kind", &[("kind", &kind)]),
    };
    let step = step_seqs(app)
        .iter()
        .position(|s| s.contains(&with_op))
        .and_then(|k| rows(app).get(k + 1).map(|r| r.1.clone()));
    match step {
        Some(s) => c.tf("history.conflict", &[("step", &s), ("what", &what)]),
        None => c.tf("history.conflict_any", &[("what", &what)]),
    }
}

/// Journal operations of the done AI steps not reverted yet.
pub fn live_seqs(marks: &[Option<ddc_model::AiBatch>]) -> Vec<i64> {
    marks
        .iter()
        .flatten()
        .filter(|b| !b.reverted)
        .map(|b| b.op_seq)
        .collect()
}

/// Label of the "revert all" button; `None` hides it (no live AI step).
pub fn revert_all_label(c: &ddc_ui::Catalog, live: &[i64]) -> Option<String> {
    (!live.is_empty())
        .then(|| c.tf("history.hist.revert_all_n", &[("n", &live.len().to_string())]))
}

/// The "revert all" button asks first; nothing changes until the answer.
pub fn ask_revert_all(app: &mut EditorApp, live: Vec<i64>) {
    app.quick.hist_ask = (!live.is_empty()).then_some(live);
}

/// Answer to the "revert all" question: `true` reverts the asked batches, `false` keeps them.
pub fn answer_revert_all(app: &mut EditorApp, revert_them: bool) {
    if let Some(seqs) = app.quick.hist_ask.take() {
        if revert_them {
            revert(app, seqs);
        }
    }
}

/// The "revert all" confirmation: the count, Keep as the default (Enter), Revert in red.
pub fn confirm_ui(app: &mut EditorApp, ctx: &egui::Context) {
    let Some(n) = app.quick.hist_ask.as_ref().map(|s| s.len()) else {
        return;
    };
    let c = &app.sh.cat;
    let title = c.t("history.ask.title");
    let text = c.plural("history.ask.text", n as u64);
    let (keep_txt, go_txt) = (c.t("history.ask.keep"), c.t("history.ask.revert"));
    let (mut open, mut keep, mut go) = (true, false, false);
    kit::Dialog::new("history.revert_all", title, kit::DialogSize::Confirm)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.add(egui::Label::new(text.as_str()).wrap());
            kit::dialog_footer(ui, |ui| {
                keep = kit::primary_action(ui, keep_txt.as_str(), None);
                go = kit::confirm_danger_button(ui, go_txt.as_str(), true).clicked();
            });
        });
    if go {
        answer_revert_all(app, true);
    } else if keep || !open {
        answer_revert_all(app, false);
    }
}

/// Reverts the AI batches as one undo step, or says why not; nothing changes on a refusal.
pub fn revert(app: &mut EditorApp, seqs: Vec<i64>) {
    use ddc_model::{Refusal, RevertError};
    let msg = match app.ed.model.ai_revert_check(&seqs) {
        Err(RevertError::Refused(r)) => {
            let c = &app.sh.cat;
            match r {
                Refusal::Empty => c.t("ai.revert.empty"),
                Refusal::NotAiBatch(_) => c.t("ai.revert.not_ai"),
                Refusal::AlreadyReverted(_) => c.t("ai.revert.already"),
                Refusal::Opaque { .. } => c.t("ai.revert.opaque"),
                Refusal::Conflict {
                    with_op,
                    table,
                    row,
                    ..
                } => conflict_text(app, with_op, &table, row),
            }
        }
        Err(RevertError::Db(e)) => app
            .sh
            .cat
            .tf("ai.revert.failed", &[("e", &e.to_string())]),
        Ok(()) => {
            let n = seqs.len();
            let f = crate::editor::OpFn(std::sync::Arc::new(move |m: &mut ddc_model::Model| {
                m.revert_ai(&seqs).map(|_| ()).map_err(|e| e.into_error())
            }));
            match app.ed.run_op(f) {
                Ok(()) => {
                    let t = app.sh.cat.tf("ai.revert.done", &[("n", &n.to_string())]);
                    app.sh.toasts.push(ddc_ui::notify::Level::Success, t.clone(), None);
                    t
                }
                Err(e) => app
                    .sh
                    .cat
                    .tf("ai.revert.failed", &[("e", &e.to_string())]),
            }
        }
    };
    app.ed.status = Some(msg);
}

/// How one history row reads; colours are theme token names, never literals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowStyle {
    /// Marked with the accent bar: the state the model is in now.
    pub current: bool,
    pub ink: &'static str,
    pub step_ink: &'static str,
}

/// Style of the row at `depth`: undone steps are disabled ink, a reverted AI step is muted.
pub fn row_style(depth: usize, cur: usize, undone: bool, reverted: bool) -> RowStyle {
    let current = depth == cur && !undone;
    let (ink, step_ink) = if undone {
        ("text.disabled", "text.disabled")
    } else if current {
        ("text.primary", "text.primary")
    } else if reverted {
        ("text.secondary", "text.secondary")
    } else {
        ("text.primary", "text.secondary")
    };
    RowStyle {
        current,
        ink,
        step_ink,
    }
}

/// Only the leading «Start» row exists: nothing was done yet.
pub fn is_empty(rows: &[(usize, String, bool)]) -> bool {
    rows.len() <= 1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKey {
    Prev,
    Next,
    First,
    Last,
}

/// Row reached from `from` by a navigation key; `last` is the deepest row.
pub fn nav_target(from: usize, last: usize, key: NavKey) -> usize {
    match key {
        NavKey::Prev => from.saturating_sub(1),
        NavKey::Next => (from + 1).min(last),
        NavKey::First => 0,
        NavKey::Last => last,
    }
}

/// Hover text of an AI-applied step.
pub fn ai_tip(c: &ddc_ui::Catalog, b: &ddc_model::AiBatch) -> String {
    let title = if b.title.is_empty() {
        c.t("ai.hist.untitled")
    } else {
        b.title.clone()
    };
    let mut t = c.tf(
        "ai.hist.tip",
        &[
            ("title", &title),
            ("actor", &b.actor),
            ("model", b.model.as_deref().unwrap_or("-")),
        ],
    );
    if let Some(w) = &b.why {
        t = format!("{t}\n{}", c.tf("ai.hist.why", &[("why", w)]));
    }
    t
}

struct Strs {
    hint: String,
    badge: String,
    revert_one: String,
    reverted: String,
    undone_tip: String,
    empty: String,
    empty_hint: String,
}

impl Strs {
    fn new(c: &ddc_ui::Catalog) -> Self {
        Self {
            hint: c.t("quick.hist.hint"),
            badge: c.t("ai.hist.badge"),
            revert_one: c.t("ai.hist.revert_one"),
            reverted: c.t("ai.hist.reverted"),
            undone_tip: c.t("history.hist.undone_tip"),
            empty: c.t("history.hist.empty"),
            empty_hint: c.t("history.hist.empty_hint"),
        }
    }
}

struct RowSpec<'a> {
    id: egui::Id,
    depth: usize,
    text: &'a str,
    style: RowStyle,
    ai: Option<&'a str>,
    ai_tip: Option<&'a str>,
    reverted_txt: Option<&'a str>,
    undone_tip: Option<&'a str>,
}

/// One history row: fixed numeric step column, optional badges, elided label. Returns the
/// click response and whether the label was cut.
fn paint_row(ui: &mut egui::Ui, th: &ddc_ui::Theme, s: &RowSpec) -> (egui::Response, bool) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), th.d().row),
        egui::Sense::hover(),
    );
    let resp = ui
        .interact(rect, s.id, egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, s.text));
    let sem = th.sem();
    let radius = th.radius("sm");
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if s.style.current {
            p.rect_filled(rect, radius, sem.accent_subtle);
            let bar = egui::Rect::from_min_size(rect.min, egui::vec2(space::XXS, rect.height()));
            p.rect_filled(bar, 0.0, th.c("accent"));
        } else if resp.hovered() {
            p.rect_filled(rect, radius, th.hover_fill());
        }
        if resp.has_focus() {
            p.rect_stroke(
                rect,
                radius,
                egui::Stroke::new(1.0, th.c("accent")),
                egui::StrokeKind::Inside,
            );
        }
    }
    let num = th.font(Role::Numeric);
    let step_w = ui
        .painter()
        .layout_no_wrap("0000".to_owned(), num.clone(), egui::Color32::PLACEHOLDER)
        .size()
        .x;
    let x0 = rect.left() + space::SM;
    let step_rect = egui::Rect::from_min_max(
        egui::pos2(x0, rect.top()),
        egui::pos2(x0 + step_w, rect.bottom()),
    );
    widgets::text::paint_in(
        ui,
        step_rect,
        &s.depth.to_string(),
        num,
        th.c(s.style.step_ink),
        egui::Align::Max,
    );
    let body = egui::Rect::from_min_max(
        egui::pos2(step_rect.right() + space::SM, rect.top()),
        egui::pos2((rect.right() - space::SM).max(step_rect.right() + space::SM), rect.bottom()),
    );
    let mut cut = false;
    let mut badge_hot = false;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(body)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            if let Some(b) = s.ai {
                let r = widgets::badge(ui, th, b, widgets::Tone::Accent);
                badge_hot |= r.hovered();
                if let Some(t) = s.ai_tip {
                    r.on_hover_text(t);
                }
            }
            if let Some(b) = s.reverted_txt {
                let r = widgets::badge(ui, th, b, widgets::Tone::Neutral);
                badge_hot |= r.hovered();
            }
            let free = ui.available_rect_before_wrap();
            cut = widgets::text::paint_in(
                ui,
                free,
                s.text,
                th.font(Role::Body),
                th.c(s.style.ink),
                egui::Align::Min,
            );
        },
    );
    let mut tip = vec![];
    if cut {
        tip.push(s.text);
    }
    if let Some(u) = s.undone_tip {
        tip.push(u);
    }
    let resp = if !tip.is_empty() && !badge_hot {
        resp.on_hover_text(tip.join("\n"))
    } else {
        resp
    };
    (resp, cut)
}

pub fn ui(app: &mut EditorApp, ctx: &egui::Context) {
    let rows = rows(app);
    let marks = ai_marks(app);
    let cur = app.ed.undo_len();
    let c = &app.sh.cat;
    let mut open = app.quick.hist_open;
    let mut jump: Option<usize> = None;
    let mut undo_ai: Option<Vec<i64>> = None;
    let mut ask_all = false;
    let mut play = false;
    let th = ddc_ui::theme::of(ctx);
    let th = &*th;
    let strs = Strs::new(c);
    let live = live_seqs(&marks);
    let tips: Vec<Option<String>> = marks
        .iter()
        .map(|m| m.as_ref().map(|b| ai_tip(c, b)))
        .collect();
    let all_txt = revert_all_label(c, &live);
    kit::Dialog::new("history", c.t("quick.hist.title"), kit::DialogSize::Tool)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label(th.rich(Role::Caption, &strs.hint).color(th.c("text.secondary")));
            ui.add_space(space::XS);
            if let Some(t) = &all_txt {
                if Button::secondary(t).small().show(ui, th).clicked() {
                    ask_all = true;
                }
                ui.add_space(space::XS);
            }
            if Button::secondary(&c.t("collab_diff.reveal.journal_play")).small().show(ui, th).clicked() {
                play = true;
            }
            ui.add_space(space::XS);
            if is_empty(&rows) {
                widgets::icon_empty_state(
                    ui,
                    "history",
                    &strs.empty,
                    &strs.empty_hint,
                    None,
                );
                return;
            }
            let last = rows.len() - 1;
            let base = ui.id();
            let row_id = move |d: usize| base.with(("hist_row", d));
            let focused = (0..=last).find(|d| ui.memory(|m| m.has_focus(row_id(*d))));
            let mut want_focus: Option<usize> = None;
            if focused.is_some() {
                let keys = [
                    (egui::Key::ArrowUp, NavKey::Prev),
                    (egui::Key::ArrowDown, NavKey::Next),
                    (egui::Key::Home, NavKey::First),
                    (egui::Key::End, NavKey::Last),
                ];
                for (key, nk) in keys {
                    if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key)) {
                        let from = focused.unwrap_or(cur);
                        let to = nav_target(from, last, nk);
                        want_focus = Some(to);
                        if to != cur {
                            jump = Some(to);
                        }
                        break;
                    }
                }
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for (depth, text, undone) in &rows {
                        let k = depth.checked_sub(1).filter(|_| !*undone);
                        let mark = k.and_then(|k| marks.get(k)).and_then(|m| m.as_ref());
                        let reverted = mark.is_some_and(|b| b.reverted);
                        let style = row_style(*depth, cur, *undone, reverted);
                        let tip = k.and_then(|k| tips.get(k)).cloned().flatten();
                        let spec = RowSpec {
                            id: row_id(*depth),
                            depth: *depth,
                            text,
                            style,
                            ai: mark.map(|_| strs.badge.as_str()),
                            ai_tip: tip.as_deref(),
                            reverted_txt: reverted.then_some(strs.reverted.as_str()),
                            undone_tip: (*undone).then_some(strs.undone_tip.as_str()),
                        };
                        let (r, _) = paint_row(ui, th, &spec);
                        if want_focus == Some(*depth) {
                            r.request_focus();
                            r.scroll_to_me(None);
                        }
                        if r.clicked() && *depth != cur {
                            jump = Some(*depth);
                        }
                        if let Some(b) = mark.filter(|b| !b.reverted) {
                            r.context_menu(|ui| {
                                if widgets::menu_item(
                                    ui,
                                    th,
                                    &strs.revert_one,
                                    "",
                                    widgets::MenuTone::Normal,
                                    true,
                                )
                                .clicked()
                                {
                                    undo_ai = Some(vec![b.op_seq]);
                                    ui.close();
                                }
                            });
                        }
                    }
                });
        });
    app.quick.hist_open = open;
    if play {
        crate::reveal::start_journal(app);
    }
    if ask_all {
        ask_revert_all(app, live);
    }
    if !open {
        app.quick.hist_ask = None;
    }
    confirm_ui(app, ctx);
    if let Some(seqs) = undo_ai {
        revert(app, seqs);
    }
    if let Some(d) = jump {
        app.ed.jump_history(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wall_tool::{Event, Mode};
    use ddc_geom::v2;

    fn level_op(name: &'static str, z: f64) -> crate::editor::OpFn {
        crate::editor::OpFn(std::sync::Arc::new(move |m: &mut ddc_model::Model| {
            m.add_level(name, z).map(|_| ())
        }))
    }

    fn mark_ai(a: &EditorApp, text: &str) {
        let seq = a.ed.model.op_top().unwrap();
        a.ed.model
            .audit_append(&ddc_model::AuditEntry {
                session: "s".into(),
                actor: "user".into(),
                event: "apply".into(),
                model: Some("m".into()),
                text: Some(text.into()),
                op_seq: Some(seq),
                status: "ok".into(),
                ..Default::default()
            })
            .unwrap();
    }

    fn app() -> EditorApp {
        EditorApp::new(
            crate::editor::Editor::new_in_memory().unwrap(),
            None,
            "test".into(),
        )
    }

    fn level_names(a: &EditorApp) -> Vec<String> {
        a.ed.model
            .level_list()
            .unwrap()
            .into_iter()
            .map(|l| l.1)
            .collect()
    }

    #[test]
    fn history_shows_ai_badge() {
        let mut a = app();
        a.ed.run_op(level_op("U1", 0.0)).unwrap();
        a.ed.run_op(level_op("AI1", 3.0)).unwrap();
        mark_ai(&a, "Add level\nbecause the brief says so");
        a.ed.run_op(level_op("U2", 6.0)).unwrap();
        let m = ai_marks(&a);
        assert_eq!(m.len(), 3);
        assert!(m[0].is_none() && m[2].is_none());
        let b = m[1].as_ref().expect("AI badge on the second step");
        assert_eq!(b.title, "Add level");
        assert_eq!(b.why.as_deref(), Some("because the brief says so"));
        assert!(!b.reverted);
        // an undone AI step loses the badge
        a.ed.jump_history(1);
        assert!(ai_marks(&a).iter().all(|x| x.is_none()));
    }

    #[test]
    fn why_goes_from_plan_item_to_history_row() {
        let mut a = app();
        let item = crate::ai::plan::PlanItem {
            id: 1,
            title: "Add level".into(),
            why: "the brief asks for it".into(),
            op: crate::ai::plan::PlanOp::Delete {
                target: crate::ai::plan::Targets::Selection,
            },
            checked: true,
            depends_on: vec![],
            actor: "chat".into(),
            conflict: None,
            sets: Default::default(),
        };
        a.ed.run_op(level_op("AI1", 3.0)).unwrap();
        let text = crate::ai::plan::audit_text(&item.title, &item.why).unwrap();
        mark_ai(&a, &text);
        let b = ai_marks(&a)[0].clone().expect("badge");
        assert_eq!(b.title, "Add level");
        assert_eq!(b.why.as_deref(), Some("the brief asks for it"));
    }

    #[test]
    fn overlong_why_is_capped_in_the_row() {
        let mut a = app();
        a.ed.run_op(level_op("AI1", 3.0)).unwrap();
        let long = "w".repeat(5000);
        let text = crate::ai::plan::audit_text("T", &long).unwrap();
        mark_ai(&a, &text);
        let b = ai_marks(&a)[0].clone().unwrap();
        // the cut is marked, so the row keeps the first MAX_WHY_CHARS and says the rest is gone
        let why = b.why.unwrap();
        assert_eq!(why.chars().filter(|c| *c == 'w').count(), ddc_ai::catalog::MAX_WHY_CHARS, "{why}");
        assert!(why.ends_with("chars cut]"), "{why}");
    }

    #[test]
    fn an_old_audit_row_without_why_still_shows() {
        let mut a = app();
        a.ed.run_op(level_op("AI1", 3.0)).unwrap();
        mark_ai(&a, "Add level");
        let b = ai_marks(&a)[0].clone().unwrap();
        assert_eq!(b.title, "Add level");
        assert!(b.why.is_none());
    }

    #[test]
    fn revert_from_history_keeps_a_later_edit_and_undoes_in_one_step() {
        let mut a = app();
        // the seeded default level stays put; only the edits after it are compared
        let base = level_names(&a);
        let with = |extra: &[&str]| -> Vec<String> {
            base.iter().cloned().chain(extra.iter().map(|s| s.to_string())).collect()
        };
        a.ed.run_op(level_op("AI1", 3.0)).unwrap();
        mark_ai(&a, "Add level");
        a.ed.run_op(level_op("U1", 6.0)).unwrap();
        let seq = ai_marks(&a)[0].as_ref().unwrap().op_seq;
        revert(&mut a, vec![seq]);
        assert_eq!(level_names(&a), with(&["U1"]));
        assert_eq!(a.ed.undo_len(), 3);
        assert!(ai_marks(&a)[0].as_ref().unwrap().reverted);
        a.ed.undo();
        assert_eq!(level_names(&a), with(&["AI1", "U1"]));
    }

    #[test]
    fn revert_from_history_refuses_a_dependent_batch_and_changes_nothing() {
        let mut a = app();
        let base = level_names(&a);
        a.ed.run_op(level_op("AI1", 3.0)).unwrap();
        mark_ai(&a, "Add level");
        let id = a.ed.model.level_list().unwrap().into_iter().find(|l| l.1 == "AI1").unwrap().0;
        a.ed.run_op(crate::editor::OpFn(std::sync::Arc::new(
            move |m: &mut ddc_model::Model| m.update_level(id, "Mine", 3.0),
        )))
        .unwrap();
        let seq = ai_marks(&a)[0].as_ref().unwrap().op_seq;
        let depth = a.ed.undo_len();
        revert(&mut a, vec![seq]);
        assert_eq!(level_names(&a), [&base[..], &["Mine".to_string()][..]].concat());
        assert_eq!(a.ed.undo_len(), depth);
        let msg = a.ed.status.clone().unwrap_or_default();
        assert!(!msg.is_empty() && msg != "ai.revert.done", "{msg}");
    }

    #[test]
    fn rows_follow_undo_and_command_labels() {
        let mut a = EditorApp::new(
            crate::editor::Editor::new_in_memory().unwrap(),
            None,
            "test".into(),
        );
        for x in [0.0, 5.0] {
            a.ed.set_mode(Mode::Rect);
            a.ed.handle(Event::Press(v2(x, 0.0)));
            a.ed.handle(Event::Release(v2(x + 2.0, 2.0)));
        }
        a.ed.set_mode(Mode::Select);
        let r = rows(&a);
        assert_eq!(r.len(), 3);
        assert_eq!(r[1].1, a.sh.cat.t("hist.walls_add"));
        a.quick
            .labels
            .insert(1, ("hist.walls_add".into(), "Room".into()));
        a.ed.jump_history(1);
        let r = rows(&a);
        assert_eq!(r.len(), 3);
        assert_eq!((r[2].1.as_str(), r[2].2), ("Room", true));
        // a label of a different step kind is not substituted
        a.quick.labels.insert(0, ("hist.import".into(), "X".into()));
        assert_ne!(rows(&a)[1].1, "X");
        // a new step of the same kind in place of an undone one: the old label goes away
        a.quick.labels.remove(&0);
        let p0 = a.ed.pushes;
        a.ed.set_mode(Mode::Rect);
        a.ed.handle(Event::Press(v2(10.0, 0.0)));
        a.ed.handle(Event::Release(v2(12.0, 2.0)));
        a.ed.set_mode(Mode::Select);
        let fresh = a.ed.pushes - p0;
        assert!(fresh >= 1);
        prune(&mut a.quick.labels, a.ed.undo_len(), a.ed.redo_len(), fresh);
        assert!(a.quick.labels.is_empty(), "{:?}", a.quick.labels);
        assert!(rows(&a).iter().all(|r| r.1 != "Room"));
    }

    fn three_steps() -> EditorApp {
        let mut a = app();
        for (n, z) in [("A", 0.0), ("B", 3.0), ("C", 6.0)] {
            a.ed.run_op(level_op(n, z)).unwrap();
        }
        a
    }

    #[test]
    fn row_style_covers_normal_current_undone_reverted() {
        let n = row_style(1, 3, false, false);
        assert_eq!((n.current, n.ink, n.step_ink), (false, "text.primary", "text.secondary"));
        let c = row_style(3, 3, false, false);
        assert_eq!((c.current, c.ink), (true, "text.primary"));
        let u = row_style(4, 3, true, false);
        assert_eq!((u.current, u.ink, u.step_ink), (false, "text.disabled", "text.disabled"));
        let r = row_style(2, 3, false, true);
        assert_eq!((r.current, r.ink), (false, "text.secondary"));
    }

    #[test]
    fn empty_history_is_the_empty_state_and_a_step_ends_it() {
        let mut a = app();
        assert!(is_empty(&rows(&a)));
        a.ed.run_op(level_op("A", 0.0)).unwrap();
        assert!(!is_empty(&rows(&a)));
        a.ed.undo();
        assert!(!is_empty(&rows(&a)), "undone steps stay listed");
    }

    #[test]
    fn current_marker_follows_undo_and_redo() {
        let mut a = three_steps();
        let current = |a: &EditorApp| -> Vec<usize> {
            let cur = a.ed.undo_len();
            rows(a)
                .iter()
                .filter(|r| row_style(r.0, cur, r.2, false).current)
                .map(|r| r.0)
                .collect()
        };
        assert_eq!(current(&a), vec![3]);
        a.ed.undo();
        assert_eq!(current(&a), vec![2]);
        a.ed.undo();
        a.ed.undo();
        assert_eq!(current(&a), vec![0]);
        a.ed.redo();
        assert_eq!(current(&a), vec![1]);
    }

    #[test]
    fn jump_back_and_forward_lands_on_the_exact_step() {
        let mut a = three_steps();
        // the seeded default level is not a history step: it stays at every position
        a.ed.jump_history(0);
        let base = level_names(&a);
        a.ed.jump_history(1);
        assert_eq!(a.ed.undo_len(), 1);
        assert_eq!(level_names(&a), [&base[..], &["A".to_string()][..]].concat());
        a.ed.jump_history(3);
        assert_eq!(a.ed.undo_len(), 3);
        assert_eq!(level_names(&a).len(), base.len() + 3);
        a.ed.jump_history(0);
        assert_eq!(a.ed.undo_len(), 0);
        assert_eq!(level_names(&a), base);
        a.ed.jump_history(2);
        assert_eq!((a.ed.undo_len(), a.ed.redo_len()), (2, 1));
    }

    #[test]
    fn keyboard_navigation_clamps_at_both_ends() {
        assert_eq!(nav_target(2, 5, NavKey::Prev), 1);
        assert_eq!(nav_target(0, 5, NavKey::Prev), 0);
        assert_eq!(nav_target(4, 5, NavKey::Next), 5);
        assert_eq!(nav_target(5, 5, NavKey::Next), 5);
        assert_eq!(nav_target(3, 5, NavKey::First), 0);
        assert_eq!(nav_target(3, 5, NavKey::Last), 5);
    }

    #[test]
    fn labels_exist_in_english_and_russian() {
        use ddc_ui::{Catalog, Lang};
        let en = Catalog::new(Lang::En);
        let ru = Catalog::new(Lang::Ru);
        for k in [
            "history.hist.undone_tip",
            "history.hist.empty",
            "history.hist.empty_hint",
            "history.hist.revert_all_n",
            "quick.hist.start",
            "ai.hist.badge",
        ] {
            let (e, r) = (en.t_in(Lang::En, k), ru.t_in(Lang::Ru, k));
            assert_ne!(e, k, "{k}");
            assert_ne!(r, k, "{k}");
            assert_ne!(e, r, "{k} is not translated");
        }
        assert!(ru.tf("history.hist.revert_all_n", &[("n", "7")]).contains('7'));
        assert!(!en.tf("history.hist.revert_all_n", &[("n", "7")]).contains("{n}"));
    }

    #[test]
    fn ai_applied_step_is_labelled_in_both_languages() {
        use ddc_ui::{Catalog, Lang};
        let mut a = app();
        a.ed.run_op(level_op("AI1", 3.0)).unwrap();
        mark_ai(&a, "Add level\nbecause");
        let b = ai_marks(&a)[0].clone().expect("AI mark");
        let en = ai_tip(&Catalog::new(Lang::En), &b);
        let ru = ai_tip(&Catalog::new(Lang::Ru), &b);
        assert!(en.contains("Add level") && en.contains("because"));
        assert!(ru.contains("Add level") && ru.contains("because"));
        assert_ne!(en, ru);
    }

    #[test]
    fn long_labels_are_elided_and_short_ones_are_not() {
        let ctx = egui::Context::default();
        ctx.set_fonts(ddc_ui::theme::font_definitions());
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        let long = "very long step label ".repeat(20);
        let mut out = vec![];
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let th = ddc_ui::theme::of(ctx);
            egui::CentralPanel::default().show(ctx, |ui| {
                // the panel itself spans the screen, so the width limit is a child ui
                ui.allocate_ui(egui::vec2(220.0, 200.0), |ui| {
                for (d, text) in [(1usize, long.as_str()), (2, "Short")] {
                    let spec = RowSpec {
                        id: egui::Id::new(("t", d)),
                        depth: d,
                        text,
                        style: row_style(d, 9, false, false),
                        ai: Some("AI"),
                        ai_tip: None,
                        reverted_txt: None,
                        undone_tip: None,
                    };
                    let (r, cut) = paint_row(ui, &th, &spec);
                    out.push((cut, r.rect.height(), r.rect.width()));
                }
                });
            });
        });
        assert!(out[0].0 && !out[1].0, "{out:?}");
        assert_eq!(out[0].1, out[1].1, "rows keep one height");
        assert!(out[0].2 <= 221.0, "no overflow: {out:?}");
    }

    #[test]
    fn hovering_a_row_shows_the_pointing_hand() {
        let ctx = egui::Context::default();
        ctx.set_fonts(ddc_ui::theme::font_definitions());
        let rect = std::cell::Cell::new(egui::Rect::NOTHING);
        let frame = |ev: Vec<egui::Event>| {
            let raw = egui::RawInput {
                events: ev,
                ..Default::default()
            };
            let out = ctx.run(raw, |ctx| {
                let th = ddc_ui::theme::of(ctx);
                egui::CentralPanel::default().show(ctx, |ui| {
                    let spec = RowSpec {
                        id: egui::Id::new("hand"),
                        depth: 1,
                        text: "Step",
                        style: row_style(1, 3, false, false),
                        ai: None,
                        ai_tip: None,
                        reverted_txt: None,
                        undone_tip: None,
                    };
                    rect.set(paint_row(ui, &th, &spec).0.rect);
                });
            });
            out.platform_output.cursor_icon
        };
        frame(vec![]);
        assert_ne!(frame(vec![]), egui::CursorIcon::PointingHand);
        let at = rect.get().center();
        let mut last = egui::CursorIcon::Default;
        for _ in 0..3 {
            last = frame(vec![egui::Event::PointerMoved(at)]);
        }
        assert_eq!(last, egui::CursorIcon::PointingHand);
    }

    #[test]
    fn source_has_no_raw_link_colour_selectable_rows_or_space_padding() {
        let src = include_str!("hist_win.rs");
        let (a, b, c) = (
            ["hyper", "link_color"].concat(),
            ["selectable", "_label"].concat(),
            ["{depth", ":>3}"].concat(),
        );
        for needle in [a, b, c] {
            assert!(!src.contains(&needle), "{needle}");
        }
    }

    #[test]
    fn prune_keeps_valid_labels() {
        let mut l = std::collections::BTreeMap::new();
        for i in 0..4 {
            l.insert(i, ("k".to_string(), format!("{i}")));
        }
        prune(&mut l, 4, 0, 0);
        assert_eq!(l.len(), 4);
        // two steps undone, one new: slots 2 and 3 are new/dropped
        prune(&mut l, 3, 0, 1);
        assert_eq!(l.keys().copied().collect::<Vec<_>>(), vec![0, 1]);
        prune(&mut l, 0, 0, 0);
        assert!(l.is_empty());
    }

    /// U1, AI1, U2, AI2: two live AI steps between the person's own edits.
    fn two_ai_steps() -> EditorApp {
        let mut a = app();
        a.ed.run_op(level_op("U1", 0.0)).unwrap();
        a.ed.run_op(level_op("AI1", 3.0)).unwrap();
        mark_ai(&a, "Add level");
        a.ed.run_op(level_op("U2", 6.0)).unwrap();
        a.ed.run_op(level_op("AI2", 9.0)).unwrap();
        mark_ai(&a, "Add another level");
        a
    }

    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    /// One frame of the history window; returns every button (label, rect) it drew.
    fn hist_frame(
        ctx: &egui::Context,
        a: &mut EditorApp,
        events: Vec<egui::Event>,
    ) -> Vec<(String, egui::Rect)> {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 1000.0),
            )),
            events,
            ..Default::default()
        };
        let out = ctx.run(raw, |ctx| ui(a, ctx));
        let mut v = vec![];
        if let Some(update) = out.platform_output.accesskit_update {
            for (_, node) in update.nodes {
                if node.role() != egui::accesskit::Role::Button {
                    continue;
                }
                let Some(b) = node.bounds() else { continue };
                let label = node.label().unwrap_or("").to_string();
                let r = egui::Rect::from_min_max(
                    egui::pos2(b.x0 as f32, b.y0 as f32),
                    egui::pos2(b.x1 as f32, b.y1 as f32),
                );
                v.push((label, r));
            }
        }
        v
    }

    fn click(ctx: &egui::Context, a: &mut EditorApp, at: egui::Pos2) {
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        hist_frame(ctx, a, vec![egui::Event::PointerMoved(at)]);
        hist_frame(ctx, a, vec![press(true)]);
        hist_frame(ctx, a, vec![press(false)]);
    }

    fn find(btns: &[(String, egui::Rect)], label: &str) -> Option<egui::Pos2> {
        btns.iter().find(|b| b.0 == label).map(|b| b.1.center())
    }

    #[test]
    fn revert_all_is_offered_only_with_live_ai_steps() {
        use ddc_ui::{Catalog, Lang};
        let c = Catalog::new(Lang::En);
        let mut a = three_steps();
        assert!(live_seqs(&ai_marks(&a)).is_empty());
        assert_eq!(revert_all_label(&c, &[]), None);
        ask_revert_all(&mut a, vec![]);
        assert!(a.quick.hist_ask.is_none(), "nothing to ask about");
        let a = two_ai_steps();
        let live = live_seqs(&ai_marks(&a));
        assert_eq!(live.len(), 2);
        let t = revert_all_label(&c, &live).expect("button shown");
        assert!(t.contains('2') && !t.contains("{n}"), "{t}");
    }

    #[test]
    fn revert_all_asks_first_and_keep_changes_nothing() {
        let mut a = two_ai_steps();
        let (names, depth) = (level_names(&a), a.ed.undo_len());
        let toasts = a.sh.toasts.history.len();
        let live = live_seqs(&ai_marks(&a));
        ask_revert_all(&mut a, live.clone());
        assert_eq!(a.quick.hist_ask.as_deref(), Some(live.as_slice()));
        assert_eq!((level_names(&a), a.ed.undo_len()), (names.clone(), depth));
        answer_revert_all(&mut a, false);
        assert!(a.quick.hist_ask.is_none());
        assert_eq!((level_names(&a), a.ed.undo_len()), (names, depth));
        assert_eq!(a.sh.toasts.history.len(), toasts);
        assert!(ai_marks(&a).iter().flatten().all(|b| !b.reverted));
    }

    #[test]
    fn confirmed_revert_all_undoes_exactly_the_ai_steps_in_one_undo_step() {
        let mut a = two_ai_steps();
        let (names, depth) = (level_names(&a), a.ed.undo_len());
        let seqs = live_seqs(&ai_marks(&a));
        ask_revert_all(&mut a, seqs);
        answer_revert_all(&mut a, true);
        let mut left = level_names(&a);
        left.sort();
        let mut want: Vec<String> =
            names.iter().filter(|n| !n.starts_with("AI")).cloned().collect();
        want.sort();
        assert_eq!(left, want, "only the AI levels are gone; the seeded one stays");
        assert!(left.iter().any(|n| n == "U1") && left.iter().any(|n| n == "U2"));
        assert_eq!(a.ed.undo_len(), depth + 1, "one undo step");
        assert!(ai_marks(&a).iter().flatten().all(|b| b.reverted));
        let done = a.sh.cat.tf("ai.revert.done", &[("n", "2")]);
        assert!(
            a.sh.toasts.history.iter().any(|(l, t)| {
                matches!(l, ddc_ui::notify::Level::Success) && *t == done
            }),
            "success toast"
        );
        a.ed.undo();
        assert_eq!(level_names(&a), names, "one undo brings both back");
    }

    #[test]
    fn revert_all_takes_two_clicks_in_the_window() {
        let ctx = font_ctx();
        ctx.enable_accesskit();
        let mut a = two_ai_steps();
        a.quick.hist_open = true;
        let (names, depth) = (level_names(&a), a.ed.undo_len());
        let all = revert_all_label(&a.sh.cat, &live_seqs(&ai_marks(&a))).unwrap();
        let mut btns = vec![];
        for _ in 0..4 {
            btns = hist_frame(&ctx, &mut a, vec![]);
        }
        let at = find(&btns, &all).unwrap_or_else(|| panic!("no {all:?} in {btns:?}"));
        click(&ctx, &mut a, at);
        assert!(a.quick.hist_ask.is_some(), "first click asks");
        assert_eq!((level_names(&a), a.ed.undo_len()), (names.clone(), depth));
        let go = a.sh.cat.t("history.ask.revert");
        for _ in 0..3 {
            btns = hist_frame(&ctx, &mut a, vec![]);
        }
        let at = find(&btns, &go).unwrap_or_else(|| panic!("no {go:?} in {btns:?}"));
        click(&ctx, &mut a, at);
        assert!(a.quick.hist_ask.is_none());
        assert_eq!(a.ed.undo_len(), depth + 1);
        assert!(ai_marks(&a).iter().flatten().all(|b| b.reverted));
    }

    #[test]
    fn keep_in_the_window_leaves_the_model_as_it_was() {
        let ctx = font_ctx();
        ctx.enable_accesskit();
        let mut a = two_ai_steps();
        a.quick.hist_open = true;
        let (names, depth) = (level_names(&a), a.ed.undo_len());
        let seqs = live_seqs(&ai_marks(&a));
        ask_revert_all(&mut a, seqs);
        let keep = a.sh.cat.t("history.ask.keep");
        let mut btns = vec![];
        for _ in 0..4 {
            btns = hist_frame(&ctx, &mut a, vec![]);
        }
        assert!(a.quick.hist_ask.is_some(), "drawing alone never answers");
        let at = find(&btns, &keep).unwrap_or_else(|| panic!("no {keep:?} in {btns:?}"));
        click(&ctx, &mut a, at);
        assert!(a.quick.hist_ask.is_none());
        assert_eq!((level_names(&a), a.ed.undo_len()), (names, depth));
    }

    /// AI adds a level, the person renames it later: reverting the AI step conflicts.
    fn conflicting() -> (EditorApp, i64) {
        let mut a = app();
        a.ed.run_op(level_op("AI1", 3.0)).unwrap();
        mark_ai(&a, "Add level");
        let id = a.ed.model.level_list().unwrap().into_iter().find(|l| l.1 == "AI1").unwrap().0;
        a.ed.run_op(crate::editor::OpFn(std::sync::Arc::new(
            move |m: &mut ddc_model::Model| m.update_level(id, "Mine", 3.0),
        )))
        .unwrap();
        let seq = ai_marks(&a)[0].as_ref().unwrap().op_seq;
        (a, seq)
    }

    fn has_cyrillic(s: &str) -> bool {
        s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c))
    }

    #[test]
    fn conflict_names_the_element_and_the_step_never_ids_or_tables() {
        use ddc_ui::{Catalog, Lang};
        for lang in [Lang::En, Lang::Ru] {
            let (mut a, seq) = conflicting();
            a.sh.cat = Catalog::new(lang);
            let depth = a.ed.undo_len();
            revert(&mut a, vec![seq]);
            assert_eq!(a.ed.undo_len(), depth, "{lang:?}: nothing changed");
            let msg = a.ed.status.clone().unwrap_or_default();
            let step = rows(&a).last().unwrap().1.clone();
            let kind = kind_word(&a.sh.cat, "level");
            assert!(msg.contains("Mine"), "{lang:?}: element name: {msg}");
            assert!(msg.contains(&kind), "{lang:?}: element kind: {msg}");
            assert!(msg.contains(&step), "{lang:?}: later step: {msg}");
            let rest = msg.replace(&step, "");
            assert!(!rest.chars().any(|c| c.is_ascii_digit()), "{lang:?}: no op number: {msg}");
            assert!(!msg.contains('{') && !msg.contains("update_level"), "{lang:?}: {msg}");
            if lang == Lang::Ru {
                assert!(has_cyrillic(&msg) && !msg.contains("level"), "ru: {msg}");
            }
        }
    }

    #[test]
    fn conflict_on_an_unknown_table_reads_as_an_element() {
        use ddc_ui::{Catalog, Lang};
        for lang in [Lang::En, Lang::Ru] {
            let mut a = app();
            a.sh.cat = Catalog::new(lang);
            let t = conflict_text(&a, 987_654, "zz_internal_tbl", Some(42));
            assert!(t.contains(&a.sh.cat.t("history.kind.other")), "{lang:?}: {t}");
            assert!(!t.contains("zz_internal") && !t.chars().any(|c| c.is_ascii_digit()), "{t}");
            assert_eq!(kind_word(&a.sh.cat, "mep_run"), a.sh.cat.t("history.kind.mep_run"));
            assert!(!kind_word(&a.sh.cat, "curtain_wall").contains('_'));
        }
    }

    #[test]
    fn source_never_formats_journal_ids_or_tables_into_text() {
        let src = include_str!("hist_win.rs");
        let prod = src.split(["#[cfg(test)]", "\nmod tests"].concat().as_str()).next().unwrap();
        for needle in [
            ["(\"op\"", ","].concat(),
            ["(\"table\"", ","].concat(),
            ["ai.revert", ".conflict"].concat(),
            ["with_op", ".to_string()"].concat(),
            [".min_", "size(["].concat(),
        ] {
            assert!(!prod.contains(&needle), "{needle}");
        }
        for json in [
            include_str!("../../../ui/data/i18n/en/history.json"),
            include_str!("../../../ui/data/i18n/ru/history.json"),
        ] {
            for p in ["{op}", "{table}", "{seq}", "{batch}"] {
                assert!(!json.contains(p), "{p}");
            }
        }
    }

    #[test]
    fn confirm_texts_exist_in_both_languages_with_the_count() {
        use ddc_ui::{Catalog, Lang};
        let en = Catalog::new(Lang::En);
        let ru = Catalog::new(Lang::Ru);
        for k in [
            "history.ask.title",
            "history.ask.keep",
            "history.ask.revert",
            "history.conflict",
            "history.conflict_any",
            "history.what.named",
            "history.what.kind",
            "history.kind.level",
            "history.kind.other",
        ] {
            let (e, r) = (en.t_in(Lang::En, k), ru.t_in(Lang::Ru, k));
            assert_ne!(e, k, "{k}");
            assert_ne!(r, k, "{k}");
            assert_ne!(e, r, "{k} is not translated");
            assert!(!has_cyrillic(&e) && has_cyrillic(&r), "{k}");
        }
        for n in [1u64, 2, 3, 5, 11, 21] {
            let (e, r) = (en.plural("history.ask.text", n), ru.plural("history.ask.text", n));
            assert!(e.contains(&n.to_string()) && !e.contains('#'), "en {n}: {e}");
            assert!(r.contains(&n.to_string()) && has_cyrillic(&r), "ru {n}: {r}");
        }
        assert_ne!(en.plural("history.ask.text", 1), en.plural("history.ask.text", 2).replace('2', "1"));
        assert_ne!(ru.plural("history.ask.text", 2), ru.plural("history.ask.text", 5).replace('5', "2"));
    }

    #[test]
    fn confirm_draws_headless_in_both_languages_and_writes_nothing() {
        use ddc_ui::{Catalog, Lang};
        for lang in [Lang::En, Lang::Ru] {
            let ctx = font_ctx();
            let mut a = two_ai_steps();
            a.sh.cat = Catalog::new(lang);
            let depth = a.ed.undo_len();
            let seqs = live_seqs(&ai_marks(&a));
            ask_revert_all(&mut a, seqs);
            for _ in 0..3 {
                let _ = ctx.run(egui::RawInput::default(), |ctx| confirm_ui(&mut a, ctx));
            }
            assert_eq!(a.quick.hist_ask.as_ref().map(Vec::len), Some(2), "{lang:?}");
            assert_eq!(a.ed.undo_len(), depth, "{lang:?}");
        }
    }
}
