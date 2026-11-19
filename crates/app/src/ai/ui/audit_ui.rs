//! The changes journal window: who changed what, at what price, and the way back. A row whose
//! step is still in the undo log selects and frames the elements it touched; Revert of an older
//! step asks first and names how many elements change; the model is only read until the yes.

use super::kit::{self, space, Button, Dialog, DialogSize};
use super::{logic, Action, Cx, PanelUi};
use crate::editor::Sel;
use crate::shell::EditorApp;
use ddc_model::{AuditEntry, Model};
use ddc_ui::i18n::Catalog;
use ddc_ui::notify::Level;
use ddc_ui::theme::Role;
use ddc_ui::widgets::field::TextField;
use egui::{self, RichText, Ui};

/// Journal rows as the window keeps them: (row number, Unix seconds, entry).
pub type Row = (i64, i64, AuditEntry);

/// Whose rows the window lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Src {
    #[default]
    All,
    /// Drafted by a model or applied by an agent.
    Ai,
    /// The person's own.
    Me,
}

impl Src {
    const ALL: [Src; 3] = [Src::All, Src::Ai, Src::Me];

    fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

/// A revert waiting for the person's yes: journal row and the question as shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Confirm {
    pub seq: i64,
    pub text: String,
}

/// The window's own state: filter, search and the open question.
#[derive(Default)]
pub struct AuditUi {
    pub src: Src,
    pub q: String,
    pub confirm: Option<Confirm>,
}

impl AuditUi {
    /// True once per yes, and only for the row the question named: Revert never runs unasked.
    pub fn take_confirmed(&mut self, seq: i64) -> bool {
        match &self.confirm {
            Some(c) if c.seq == seq => {
                self.confirm = None;
                true
            }
            _ => false,
        }
    }
}

/// The newest `consent::AUDIT_ROWS` rows, oldest first: the window lists the latest work, not
/// the first rows of a long journal.
pub fn load(m: &Model) -> Vec<Row> {
    let top: i64 = m
        .sql()
        .query_row("SELECT coalesce(max(seq),0) FROM ai_audit", [], |r| r.get(0))
        .unwrap_or(0);
    let rows = super::consent::AUDIT_ROWS;
    m.audit_list(top.saturating_sub(rows as i64), rows)
        .unwrap_or_default()
}

/// Made by a model: an agent's step, or a draft that names the model behind it.
pub fn is_ai(e: &AuditEntry) -> bool {
    matches!(logic::audit_source(&e.actor), logic::Source::Agent(_))
        || e.model.as_deref().is_some_and(|m| !m.trim().is_empty())
}

/// Indices of the changesets to list, newest first: of `src`, and with `q` (any case) in the
/// title, the agent's name or the model.
pub fn audit_filter(rows: &[Row], src: Src, q: &str) -> Vec<usize> {
    let q = q.trim().to_lowercase();
    (0..rows.len())
        .rev()
        .filter(|&i| {
            let e = &rows[i].2;
            if !logic::is_changeset(&e.event) {
                return false;
            }
            let side = match src {
                Src::All => true,
                Src::Ai => is_ai(e),
                Src::Me => !is_ai(e),
            };
            side && (q.is_empty()
                || [e.text.as_deref(), Some(e.actor.as_str()), e.model.as_deref()]
                    .into_iter()
                    .flatten()
                    .any(|s| s.to_lowercase().contains(&q)))
        })
        .collect()
}

/// What the entry did, in words: the changeset's title, else its step count, else its outcome.
pub fn summary(cat: &Catalog, e: &AuditEntry) -> String {
    let what = match (&e.text, e.plan.as_deref().and_then(logic::steps_of)) {
        (Some(t), _) if !t.is_empty() => t.clone(),
        (_, Some(n)) => cat.tf("ai.audit.steps", &[("n", &n.to_string())]),
        _ => cat.t(&format!("ai.audit.event.{}", e.event)),
    };
    if e.status == "ok" || e.status.is_empty() {
        what
    } else {
        format!(
            "{what}  \u{b7}  {}",
            cat.t(&format!("ai.audit.status.{}", e.status))
        )
    }
}

/// The row's second line: `<summary> · <model> · 3,2k tokens · $0,04`; a part that is not
/// known (no model, no tokens, no price) is left out.
pub fn audit_line(cat: &Catalog, e: &AuditEntry) -> String {
    let mut parts = vec![summary(cat, e)];
    if let Some(m) = e.model.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        parts.push(m.to_string());
    }
    let tokens = e.tok_in.saturating_add(e.tok_out);
    if tokens > 0 {
        parts.push(cat.tf("ai.audit.tokens", &[("n", &logic::fmt_tokens(tokens))]));
    }
    if let Some(c) = e.cost.filter(|c| c.is_finite() && *c > 0.0) {
        parts.push(logic::fmt_cost(c));
    }
    parts.join("  \u{b7}  ")
}

/// The question before a revert: what is taken back and how many elements change.
pub fn revert_prompt(cat: &Catalog, e: &AuditEntry, n: usize) -> String {
    cat.tf(
        "ai.audit.revert.ask",
        &[
            ("what", &summary(cat, e)),
            ("change", &cat.plural("ai.audit.revert.elements", n as u64)),
        ],
    )
}

/// The entry of journal row `seq` and its undo step, while that step is still in the log.
fn live_entry(app: &EditorApp, seq: i64) -> Option<(AuditEntry, i64)> {
    let m = &app.ed.model;
    let (_, _, e) = m
        .audit_list(seq - 1, 1)
        .ok()?
        .into_iter()
        .find(|r| r.0 == seq)?;
    let op = e.op_seq?;
    m.audit_op_live(seq).unwrap_or(false).then_some((e, op))
}

/// Revert asked for row `seq`: the question when the step can go back alone, else a warning
/// toast with the reason (a newer change touched the same elements, ...) and no question.
pub(super) fn ask(app: &mut EditorApp, seq: i64) -> Option<Confirm> {
    let (e, op) = live_entry(app, seq)?;
    let top = app.ed.model.last_op_seq().ok().flatten();
    if logic::undo_state(Some(op), true, top) != logic::UndoState::Covered {
        return None;
    }
    let check = app
        .ed
        .model
        .revert_check(op)
        .unwrap_or(ddc_model::RevertCheck::Unsafe);
    if check != ddc_model::RevertCheck::Ready {
        let table = match &check {
            ddc_model::RevertCheck::Conflict(t) => t.as_str(),
            _ => "",
        };
        let text = app.sh.cat.tf(super::revert_why(&check), &[("table", table)]);
        app.sh.toasts.push(Level::Warning, text, None);
        return None;
    }
    let n = app.ed.model.op_element_ids(op).map_or(0, |v| v.len());
    Some(Confirm {
        seq,
        text: revert_prompt(&app.sh.cat, &e, n),
    })
}

/// Elements the editor can select among `(table, id)` rows, once each, in order.
pub fn sels_of(ids: &[(String, i64)]) -> Vec<Sel> {
    let mut out: Vec<Sel> = vec![];
    for (t, id) in ids {
        if let Some(s) = crate::clip_tools::sel_of(t, *id) {
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

/// Selects and frames what the step of row `seq` touched and still exists; returns how many.
/// Reads only: the selection is the view's, not the model's.
pub(super) fn show(app: &mut EditorApp, seq: i64) -> usize {
    let Some((_, op)) = live_entry(app, seq) else {
        return 0;
    };
    let ids = app.ed.model.op_element_ids(op).unwrap_or_default();
    let db = app.ed.model.sql();
    let there: Vec<(String, i64)> = ids
        .into_iter()
        .filter(|(t, id)| {
            let tq = format!("\"{}\"", t.replace('"', "\"\""));
            db.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM main.{tq} WHERE rowid=?1)"),
                [*id],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false)
        })
        .collect();
    let sels = sels_of(&there);
    if sels.is_empty() {
        return 0;
    }
    let n = sels.len();
    app.ed.selection = sels;
    crate::open::summary::frame_selection(app);
    n
}

/// Changesets, newest first: who, when, what, at what price, and the way back. The undo log is
/// a stack, so the newest step is undone, an older one is reverted alone after a yes.
pub fn window(ctx: &egui::Context, st: &mut PanelUi, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    if !st.audit_open {
        return;
    }
    let (th, cat) = (cx.th, cx.cat);
    let title = cat.t("cmd.ai.audit");
    let top = cx.ed.model.last_op_seq().ok().flatten();
    let rows = &st.audit;
    let view = &mut st.audit_ui;
    Dialog::new("ai.audit", &title, DialogSize::Wide)
        .open(&mut st.audit_open)
        .show(ctx, |ui| {
            if let Some(c) = &view.confirm {
                confirm_card(ui, cx, c, acts);
                ui.add_space(space::SM);
            }
            ui.horizontal(|ui| {
                let items = [
                    cat.t("ai.audit.filter.all"),
                    cat.t("ai.audit.filter.ai"),
                    cat.t("ai.audit.filter.me"),
                ];
                let refs: Vec<&str> = items.iter().map(String::as_str).collect();
                if let Some(i) = ddc_ui::widgets::segmented(ui, th, &refs, view.src.index()) {
                    view.src = Src::ALL.get(i).copied().unwrap_or_default();
                }
                ui.add_space(space::SM);
                let (hint, tip) = (cat.t("ai.audit.search"), cat.t("ai.audit.search.clear"));
                let w = ui.available_width();
                TextField::new(&mut view.q)
                    .id_salt("ai.audit.search")
                    .labelled_by(egui::Id::new("ai.audit.search.lbl"), &hint)
                    .hint(&hint)
                    .width(w)
                    .search("find", SEARCH_GLYPH_X, SEARCH_TEXT_X, &tip)
                    .show(ui, th);
            });
            ui.add_space(space::XS);
            let shown = audit_filter(rows, view.src, &view.q);
            if shown.is_empty() {
                let any = rows.iter().any(|r| logic::is_changeset(&r.2.event));
                if any {
                    let all = cat.t("ai.audit.show_all");
                    if kit::empty_state(ui, th, &cat.t("ai.audit.no_match"), "", Some(&all)) {
                        view.src = Src::All;
                        view.q.clear();
                    }
                } else {
                    kit::empty_state(
                        ui,
                        th,
                        &cat.t("ai.audit.empty"),
                        &cat.t("ai.audit.empty.hint"),
                        None,
                    );
                }
                return;
            }
            let h = row_height(th);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show_rows(ui, h, shown.len(), |ui, range| {
                    for k in range {
                        let (seq, ts, e) = &rows[shown[k]];
                        row(ui, cx, (*seq, *ts, e), top, h, acts);
                        ui.separator();
                    }
                });
        });
}

fn row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    (seq, ts, e): (i64, i64, &AuditEntry),
    top: Option<i64>,
    h: f32,
    acts: &mut Vec<Action>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let live = e
        .op_seq
        .is_some_and(|_| cx.ed.model.audit_op_live(seq).unwrap_or(false));
    let state = logic::undo_state(e.op_seq, live, top);
    ui.horizontal(|ui| {
        ui.set_min_height(h);
        // The button is placed first from the right edge; the text column then gets only what
        // is left, so a long line is elided before it reaches the button.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let slot = match state {
                logic::UndoState::Ready => Some(("ai.audit.undo", "")),
                logic::UndoState::Covered => Some(("ai.audit.revert", "ai.audit.revert.tip")),
                logic::UndoState::Gone => Some(("ai.audit.undone", "ai.audit.undo.gone")),
                logic::UndoState::NoChange => None,
            };
            if let Some((key, why)) = slot {
                let tip = if why.is_empty() { String::new() } else { cat.t(why) };
                let ready = matches!(state, logic::UndoState::Ready | logic::UndoState::Covered);
                if Button::secondary(&cat.t(key))
                    .small()
                    .enabled(ready)
                    .tooltip(&tip, "")
                    .show(ui, th)
                    .clicked()
                {
                    acts.push(if state == logic::UndoState::Ready {
                        Action::UndoAudit(seq)
                    } else {
                        Action::AskRevert(seq)
                    });
                }
                ui.add_space(space::SM);
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let text = ui
                    .vertical(|ui| {
                        let who = match logic::audit_source(&e.actor) {
                            logic::Source::Person => cat.t("ai.audit.src.person"),
                            logic::Source::Agent(n) => {
                                cat.tf("ai.audit.src.agent", &[("name", n.as_str())])
                            }
                        };
                        kit::elided_label(
                            ui,
                            &format!("{who}  \u{b7}  {}", logic::fmt_local(ts)),
                            th.font(Role::BodyStrong),
                            th.c("text.primary"),
                        );
                        kit::elided_label(
                            ui,
                            &audit_line(cat, e),
                            th.font(Role::Label),
                            th.c("text.secondary"),
                        );
                    })
                    .response;
                if live {
                    let hit = ui
                        .interact(
                            text.rect,
                            egui::Id::new(("ddc.ai.audit.row", seq)),
                            egui::Sense::click(),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(cat.t("ai.audit.show.tip"));
                    if hit.clicked() {
                        acts.push(Action::ShowAudit(seq));
                    }
                }
            });
        });
    });
}

/// Search glyph and text positions inside the field (the kit's search look).
const SEARCH_GLYPH_X: f32 = space::SM;
const SEARCH_TEXT_X: f32 = space::SM + ddc_ui::theme::icon_size::SM + space::XS;

/// Two text lines (title, detail) on the density's own steps.
fn row_height(th: &ddc_ui::Theme) -> f32 {
    th.d().row + th.d().control_sm
}

/// Inline question: Revert is the one primary action, Cancel stays quiet; neither is a key
/// default, so a stray Enter in the search box never reverts.
fn confirm_card(ui: &mut Ui, cx: &Cx<'_>, c: &Confirm, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    kit::card(ui, th, |ui| {
        ui.set_width(ui.available_width());
        ui.add(
            egui::Label::new(
                RichText::new(&c.text)
                    .font(th.font(Role::BodyStrong))
                    .color(th.c("text.primary")),
            )
            .wrap(),
        );
        kit::caption(ui, th, &cat.t("ai.audit.revert.keeps"));
        ui.add_space(space::SM);
        ui.horizontal(|ui| {
            if Button::primary(&cat.t("ai.audit.revert"))
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::RevertAudit(c.seq));
            }
            if Button::quiet(&cat.t("ai.audit.cancel")).show(ui, th).clicked() {
                acts.push(Action::CancelRevert);
            }
        });
    });
}

/// What the layout contract draws: the empty journal, rows with long lines, a filter that hides
/// every row.
#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum LayoutCase {
    Empty,
    Rows,
    NoMatch,
}

/// The journal window headless at its class size, for `layout_contract`.
#[cfg(test)]
pub(crate) fn layout_probe(lang: ddc_ui::Lang, case: LayoutCase) -> crate::layout_contract::Probe {
    let app = EditorApp::new(crate::editor::Editor::new_in_memory().unwrap(), None, "test".into());
    let cat = Catalog::new(lang);
    let mut st = PanelUi::default();
    st.audit_open = true;
    if !matches!(case, LayoutCase::Empty) {
        let long = "Raise every wall of the level by two hundred millimetres ".repeat(4);
        for i in 0..3 {
            let e = AuditEntry {
                actor: "user".into(),
                event: "apply".into(),
                text: Some(format!("{i} {long}")),
                status: "ok".into(),
                ..AuditEntry::default()
            };
            app.ed.model.audit_append(&e).unwrap();
        }
        st.audit = load(&app.ed.model);
    }
    if matches!(case, LayoutCase::NoMatch) {
        st.audit_ui.q = "no such change".into();
    }
    let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    crate::layout_contract::probe("ai.audit", |ctx| {
        let _ = th.apply(ctx);
        let cx = Cx {
            th: &th,
            cat: &cat,
            ed: &app.ed,
            view3d: false,
            palette_open: false,
        };
        window(ctx, &mut st, &cx, &mut Vec::new());
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use ddc_ui::Lang;
    use egui::accesskit;

    /// One headless window with an explicit catalog; the global language is never read.
    struct Rig {
        ctx: egui::Context,
        th: ddc_ui::Theme,
        cat: Catalog,
    }

    struct Drawn {
        texts: Vec<(String, egui::Rect)>,
        controls: Vec<(accesskit::Role, String, egui::Rect)>,
        boxes: Vec<egui::Rect>,
        acts: Vec<Action>,
    }

    fn shape_texts(s: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
        match s {
            egui::Shape::Text(t) => out.push((
                t.galley.text().to_string(),
                t.galley.rect.translate(t.pos.to_vec2()),
            )),
            egui::Shape::Vec(v) => v.iter().for_each(|x| shape_texts(x, out)),
            _ => {}
        }
    }

    impl Rig {
        fn new(lang: Lang) -> Self {
            let ctx = egui::Context::default();
            // named font weights bind from the pass after install: a bare first frame panics
            ddc_ui::theme::install_fonts(&ctx);
            let _ = ctx.run(Default::default(), |_| {});
            ctx.enable_accesskit();
            Rig {
                ctx,
                th: ddc_ui::Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact),
                cat: Catalog::new(lang),
            }
        }

        fn frame(&mut self, st: &mut PanelUi, ed: &Editor, events: Vec<egui::Event>) -> Drawn {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 1000.0),
                )),
                events,
                ..egui::RawInput::default()
            };
            let mut acts = vec![];
            let out = self.ctx.run(raw, |ctx| {
                let _ = self.th.apply(ctx);
                let cx = Cx {
                    th: &self.th,
                    cat: &self.cat,
                    ed,
                    view3d: false,
                    palette_open: false,
                };
                window(ctx, st, &cx, &mut acts);
            });
            let mut texts = vec![];
            for c in &out.shapes {
                shape_texts(&c.shape, &mut texts);
            }
            let mut controls = vec![];
            if let Some(u) = out.platform_output.accesskit_update {
                for (_, n) in u.nodes {
                    let Some(b) = n.bounds() else { continue };
                    let r = egui::Rect::from_min_max(
                        egui::pos2(b.x0 as f32, b.y0 as f32),
                        egui::pos2(b.x1 as f32, b.y1 as f32),
                    );
                    let label = n.label().or_else(|| n.value()).unwrap_or("").to_string();
                    controls.push((n.role(), label, r));
                }
            }
            let boxes = out
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Rect(r) => Some(r.rect),
                    _ => None,
                })
                .collect();
            Drawn { texts, controls, boxes, acts }
        }

        fn settle(&mut self, st: &mut PanelUi, ed: &Editor) -> Drawn {
            let mut d = self.frame(st, ed, vec![]);
            for _ in 0..3 {
                d = self.frame(st, ed, vec![]);
            }
            d
        }

        fn click(&mut self, st: &mut PanelUi, ed: &Editor, at: egui::Pos2) {
            let btn = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            let _ = self.frame(st, ed, vec![egui::Event::PointerMoved(at)]);
            let _ = self.frame(st, ed, vec![btn(true)]);
            let _ = self.frame(st, ed, vec![btn(false)]);
        }
    }

    fn button<'a>(d: &'a Drawn, label: &str) -> Option<&'a egui::Rect> {
        d.controls
            .iter()
            .find(|(r, l, _)| *r == accesskit::Role::Button && l == label)
            .map(|c| &c.2)
    }

    /// Rows in the four undo states, each with a 200-character line.
    fn long_rows(a: &mut EditorApp) -> Vec<String> {
        let long = "wall ".repeat(40);
        let mut marks = vec![];
        let mut add = |a: &mut EditorApp, tag: &str, with_op: bool| {
            let op = if with_op {
                a.ed.model.add_level(tag, 3.0).unwrap();
                a.ed.model.last_op_seq().unwrap()
            } else {
                None
            };
            let text = format!("{tag}{long}");
            a.ed.model
                .audit_append(&AuditEntry {
                    op_seq: op,
                    ..entry("user", "apply", &text)
                })
                .unwrap();
            marks.push(tag.to_string());
        };
        add(a, "Covered", true);
        add(a, "Gone", true);
        a.ed.model
            .sql()
            .execute("UPDATE ai_audit SET op_undone=1 WHERE text LIKE 'Gone%'", [])
            .unwrap();
        add(a, "Ready", true);
        add(a, "Plain", false);
        marks
    }

    #[test]
    fn a_long_line_ends_before_the_button_of_its_row_in_every_state_and_language() {
        for lang in [Lang::En, Lang::Ru] {
            let mut a = app();
            let marks = long_rows(&mut a);
            let mut st = PanelUi::default();
            st.audit_open = true;
            st.audit = load(&a.ed.model);
            let mut rig = Rig::new(lang);
            let d = rig.settle(&mut st, &a.ed);
            let cat = Catalog::new(lang);
            let mut buttons = 0;
            for m in &marks {
                let (_, line) = d
                    .texts
                    .iter()
                    .find(|(t, _)| t.starts_with(m.as_str()))
                    .unwrap_or_else(|| panic!("{lang:?}: row {m} not drawn"));
                let key = match m.as_str() {
                    "Covered" => Some("ai.audit.revert"),
                    "Gone" => Some("ai.audit.undone"),
                    "Ready" => Some("ai.audit.undo"),
                    _ => None,
                };
                let Some(key) = key else {
                    continue;
                };
                let label = cat.t(key);
                let b = d
                    .controls
                    .iter()
                    .filter(|(r, l, c)| {
                        *r == accesskit::Role::Button
                            && *l == label
                            && (c.center().y - line.center().y).abs() < rig.th.d().row * 2.0
                    })
                    .map(|c| c.2)
                    .next()
                    .unwrap_or_else(|| panic!("{lang:?}: no {label} button for {m}"));
                buttons += 1;
                assert!(
                    line.right() < b.left(),
                    "{lang:?} {m}: text right {} reaches button left {}",
                    line.right(),
                    b.left()
                );
            }
            assert_eq!(buttons, 3, "{lang:?}");
        }
    }

    #[test]
    fn the_search_is_the_kit_field_with_the_control_height() {
        let a = app();
        let mut st = PanelUi::default();
        st.audit_open = true;
        let mut rig = Rig::new(Lang::En);
        let d = rig.settle(&mut st, &a.ed);
        let f = d
            .controls
            .iter()
            .find(|(r, _, _)| *r == accesskit::Role::TextInput)
            .expect("a search field");
        // the kit paints its frame at control height; the a11y node is the text row inside it
        let control = rig.th.d().control;
        let framed = d
            .boxes
            .iter()
            .any(|b| b.contains_rect(f.2) && (b.height() - control).abs() < 0.5);
        assert!(framed, "{:?} in {:?}", f.2, d.boxes);
        assert!(f.2.width() > 100.0, "the field takes the free width: {:?}", f.2);
    }

    #[test]
    fn an_empty_journal_says_what_appears_here_and_offers_no_show_all() {
        for lang in [Lang::En, Lang::Ru] {
            let a = app();
            let mut st = PanelUi::default();
            st.audit_open = true;
            let mut rig = Rig::new(lang);
            let d = rig.settle(&mut st, &a.ed);
            let cat = Catalog::new(lang);
            let has = |s: String| d.texts.iter().any(|(t, _)| *t == s);
            assert!(has(cat.t("ai.audit.empty")), "{lang:?}: title");
            assert!(has(cat.t("ai.audit.empty.hint")), "{lang:?}: hint");
            assert!(button(&d, &cat.t("ai.audit.show_all")).is_none(), "{lang:?}");
            assert!(!has(cat.t("ai.audit.no_match")), "{lang:?}");
        }
    }

    #[test]
    fn a_filter_that_hides_everything_offers_show_all_and_it_clears_both_filters() {
        for lang in [Lang::En, Lang::Ru] {
            let mut a = app();
            let _ = long_rows(&mut a);
            let mut st = PanelUi::default();
            st.audit_open = true;
            st.audit = load(&a.ed.model);
            st.audit_ui.src = Src::Ai;
            st.audit_ui.q = "no such change".into();
            let mut rig = Rig::new(lang);
            let cat = Catalog::new(lang);
            let d = rig.settle(&mut st, &a.ed);
            assert!(d.texts.iter().any(|(t, _)| *t == cat.t("ai.audit.no_match")), "{lang:?}");
            assert!(
                !d.texts.iter().any(|(t, _)| *t == cat.t("ai.audit.empty")),
                "{lang:?}: rows exist, the journal is not empty"
            );
            let at = button(&d, &cat.t("ai.audit.show_all"))
                .unwrap_or_else(|| panic!("{lang:?}: no Show all"))
                .center();
            rig.click(&mut st, &a.ed, at);
            assert_eq!(st.audit_ui.src, Src::All, "{lang:?}");
            assert!(st.audit_ui.q.is_empty(), "{lang:?}");
            let d = rig.settle(&mut st, &a.ed);
            assert!(d.texts.iter().any(|(t, _)| t.starts_with("Ready")), "{lang:?}: rows are back");
            assert!(d.acts.is_empty(), "{lang:?}: no action without a click on a row");
        }
    }

    #[test]
    fn a_frame_without_input_writes_nothing() {
        let mut a = app();
        let _ = long_rows(&mut a);
        let mut st = PanelUi::default();
        st.audit_open = true;
        st.audit = load(&a.ed.model);
        let head = a.ed.model.change_head();
        let mut rig = Rig::new(Lang::En);
        let d = rig.settle(&mut st, &a.ed);
        assert!(d.acts.is_empty());
        assert_eq!(a.ed.model.change_head(), head);
    }

    fn entry(actor: &str, event: &str, text: &str) -> AuditEntry {
        AuditEntry {
            actor: actor.into(),
            event: event.into(),
            text: Some(text.into()),
            status: "ok".into(),
            ..AuditEntry::default()
        }
    }

    #[test]
    fn audit_line_shows_tokens_and_cost_only_when_known() {
        let cat = Catalog::new(Lang::En);
        let bare = entry("user", "apply", "Raise walls");
        assert_eq!(audit_line(&cat, &bare), "Raise walls");
        let paid = AuditEntry {
            model: Some("m-large".into()),
            tok_in: 3000,
            tok_out: 200,
            cost: Some(0.04),
            ..bare.clone()
        };
        let line = audit_line(&cat, &paid);
        assert!(line.starts_with("Raise walls  \u{b7}  m-large  \u{b7}  "), "{line}");
        assert!(line.contains("tokens") && line.contains('$') && line.contains("04"), "{line}");
        let free = AuditEntry {
            cost: Some(0.0),
            ..paid
        };
        assert!(!audit_line(&cat, &free).contains('$'));
        assert_eq!(logic::fmt_tokens(0), "0");
        assert!(logic::fmt_cost(0.001).starts_with("<$"));
    }

    #[test]
    fn audit_line_and_prompt_read_in_both_languages() {
        let (en, ru) = (Catalog::new(Lang::En), Catalog::new(Lang::Ru));
        let e = AuditEntry {
            tok_in: 10,
            ..entry("user", "apply", "Raise walls")
        };
        let (le, lr) = (audit_line(&en, &e), audit_line(&ru, &e));
        assert!(le.contains("10 tokens"), "{le}");
        assert!(lr.contains("10") && lr != le, "{lr}");
        for cat in [&en, &ru] {
            let p = revert_prompt(cat, &e, 3);
            assert!(p.contains("Raise walls") && p.contains('3'), "{p}");
            assert!(!p.contains('{') && !p.contains("ai.audit"), "{p}");
            for k in [
                "ai.audit.revert.keeps",
                "ai.audit.cancel",
                "ai.audit.search",
                "ai.audit.no_match",
                "ai.audit.filter.all",
                "ai.audit.filter.ai",
                "ai.audit.filter.me",
                "ai.audit.show.tip",
                "ai.audit.toast.reverted",
                "ai.audit.toast.undone",
                "ai.audit.redo",
            ] {
                assert_ne!(cat.t(k), k, "{k}");
            }
        }
        assert_ne!(en.t("ai.audit.revert.ask"), ru.t("ai.audit.revert.ask"));
    }

    #[test]
    fn audit_filter_by_source_and_text_newest_first() {
        let agent = format!("{}bot", crate::ai::session::ACTOR_PREFIX);
        let drafted = AuditEntry {
            model: Some("m".into()),
            ..entry("user", "apply", "Doors to 900")
        };
        let rows: Vec<Row> = vec![
            (1, 0, entry("user", "apply", "Move grid")),
            (2, 0, entry(&agent, "apply", "Add slab")),
            (3, 0, entry("user", "chat", "hello")),
            (4, 0, drafted),
            (5, 0, entry("user", "reject", "Wall heights")),
        ];
        assert_eq!(audit_filter(&rows, Src::All, ""), vec![4, 3, 1, 0], "no chat turns");
        assert_eq!(audit_filter(&rows, Src::Ai, ""), vec![3, 1]);
        assert_eq!(audit_filter(&rows, Src::Me, ""), vec![4, 0]);
        assert_eq!(audit_filter(&rows, Src::All, "  SLAB "), vec![1]);
        assert_eq!(audit_filter(&rows, Src::All, "bot"), vec![1], "agent name");
        assert!(audit_filter(&rows, Src::Me, "slab").is_empty());
    }

    #[test]
    fn confirm_is_taken_once_and_only_for_its_row() {
        let mut v = AuditUi {
            confirm: Some(Confirm {
                seq: 7,
                text: "q".into(),
            }),
            ..AuditUi::default()
        };
        assert!(!v.take_confirmed(8), "another row");
        assert!(v.take_confirmed(7));
        assert!(!v.take_confirmed(7), "one yes, one revert");
    }

    fn app() -> EditorApp {
        EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
    }

    /// Two levels, each its own step; the first is journaled as an applied draft.
    fn two_steps(a: &mut EditorApp) -> (i64, i64, i64) {
        let m = &mut a.ed.model;
        let l1 = m.add_level("A", 0.0).unwrap();
        let op = m.last_op_seq().unwrap().unwrap();
        let row = m
            .audit_append(&AuditEntry {
                op_seq: Some(op),
                model: Some("m".into()),
                ..entry("user", "apply", "Level A")
            })
            .unwrap();
        let l2 = m.add_level("B", 3.0).unwrap();
        (row, l1, l2)
    }

    fn levels(a: &EditorApp) -> i64 {
        a.ed.model
            .sql()
            .query_row("SELECT count(*) FROM level", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn revert_runs_only_after_the_yes_and_is_one_undo_step() {
        let mut a = app();
        let mut st = PanelUi::default();
        let (row, _, _) = two_steps(&mut a);
        let n0 = levels(&a);
        st.run(&mut a, Action::RevertAudit(row));
        assert_eq!(levels(&a), n0, "no question asked: nothing reverted");
        st.run(&mut a, Action::AskRevert(row));
        let c = st.audit_ui.confirm.clone().expect("a question");
        assert!(c.text.contains("Level A"), "{}", c.text);
        assert_eq!(levels(&a), n0, "asking changes nothing");
        st.run(&mut a, Action::CancelRevert);
        assert!(st.audit_ui.confirm.is_none());
        st.run(&mut a, Action::RevertAudit(row));
        assert_eq!(levels(&a), n0, "cancelled: still nothing");
        st.run(&mut a, Action::AskRevert(row));
        st.run(&mut a, Action::RevertAudit(row));
        assert_eq!(levels(&a), n0 - 1, "reverted after the yes");
        let (lv, text) = a.sh.toasts.history.last().cloned().unwrap();
        assert_eq!(lv, Level::Success);
        assert!(text.contains("Level A"), "{text}");
        a.ed.undo();
        assert_eq!(levels(&a), n0, "one undo step brings it back");
    }

    #[test]
    fn revert_after_a_later_edit_of_the_same_element_is_refused_with_a_warning() {
        let mut a = app();
        let mut st = PanelUi::default();
        let (row, l1, _) = two_steps(&mut a);
        a.ed.model.update_level(l1, "A2", 1.0).unwrap();
        st.run(&mut a, Action::AskRevert(row));
        assert!(st.audit_ui.confirm.is_none(), "no question for a refused revert");
        let (lv, _) = a.sh.toasts.history.last().cloned().expect("a reason");
        assert_eq!(lv, Level::Warning);
        let name: String = a
            .ed
            .model
            .sql()
            .query_row("SELECT name FROM level WHERE id=?1", [l1], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "A2", "the later edit stays");
    }

    #[test]
    fn show_selects_what_the_step_touched_and_writes_nothing() {
        let mut a = app();
        let spec = ddc_model::WallSpec {
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            level: a.ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: a.ed.types[0].id,
            offset0: None,
        };
        let w = a.ed.model.add_wall(&spec).unwrap();
        let op = a.ed.model.last_op_seq().unwrap().unwrap();
        let row = a
            .ed
            .model
            .audit_append(&AuditEntry {
                op_seq: Some(op),
                ..entry("user", "apply", "Wall")
            })
            .unwrap();
        let before = a.ed.model.last_op_seq().unwrap();
        assert!(show(&mut a, row) >= 1);
        assert!(a.ed.selection.contains(&Sel::Wall(w)), "{:?}", a.ed.selection);
        assert_eq!(a.ed.model.last_op_seq().unwrap(), before, "no undo step");
    }

    #[test]
    fn the_window_loads_the_newest_rows() {
        let a = app();
        let m = &a.ed.model;
        let n = super::super::consent::AUDIT_ROWS + 5;
        for i in 0..n {
            m.audit_append(&entry("user", "apply", &format!("r{i}"))).unwrap();
        }
        let rows = load(m);
        assert_eq!(rows.len(), super::super::consent::AUDIT_ROWS, "bounded");
        let last = rows.last().unwrap().2.text.clone().unwrap();
        assert_eq!(last, format!("r{}", n - 1), "the newest is in");
    }
}
