// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The plan card: counts by created, changed and deleted, per-item check marks, hover to highlight
//! on the plan and in 3D, click to zoom, and the one primary Apply.

use super::kit::{self, space, Button};
use super::logic::{self, Counts};
use super::review::{self, ReviewAct, ReviewCtx};
use super::{Action, Cx, Facts, PanelUi};
use crate::ai::plan::{ElemDiff, PlanView, QtyDelta};
use crate::ai::preview::{self, PreviewSet};
use crate::ai::validate::{IssueLevel, PlanIssue};
use crate::units::{fmt_area_in_lang, fmt_num_lang, fmt_volume_lang, precision, Units};
use ddc_model::{ElKey, RowKind};
use ddc_ui::i18n::Catalog;
use ddc_ui::theme::Role;
use ddc_ui::widgets;
use ddc_ui::Theme;
use egui::{
    self, Align2, RichText, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
};

const ITEM_ROWS_SHOWN: f32 = 8.0;
const QTY_LINES: usize = 3;
const VARIANT_NAMES: [&str; 6] = ["A", "B", "C", "D", "E", "F"];

pub(super) fn kind_color(ui: &Ui, k: RowKind) -> egui::Color32 {
    preview::diff_kind(k).color_for(ui.visuals().dark_mode)
}

fn kind_icon(ui: &mut Ui, k: RowKind) {
    let dk = preview::diff_kind(k);
    ddc_ui::diff_kind::badge(ui, dk, dk.color_for(ui.visuals().dark_mode));
}

/// `+12`: coloured like the overlay, absent at zero; hover shows those elements, click goes there.
pub(super) fn chip(ui: &mut Ui, th: &Theme, k: RowKind, n: usize, tip: &str) -> Option<egui::Response> {
    if n == 0 {
        return None;
    }
    let col = kind_color(ui, k);
    let text = crate::units::fmt_num(n as f64, 0);
    let bg = ui.painter().add(Shape::Noop);
    let inner = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        kind_icon(ui, k);
        ui.label(
            RichText::new(&text)
                .font(th.font(Role::Numeric))
                .color(col),
        );
    });
    // the pill is one control: hover, pressed and keyboard focus read on its whole area
    let pill = inner.response.rect.expand2(Vec2::new(space::XS, 2.0));
    let r = ui.interact(pill, ui.id().with(("ai.chip", tip)), Sense::click());
    let name = format!("{tip} {text}");
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
    let fill = if r.is_pointer_button_down_on() {
        Some(th.pressed_fill())
    } else if r.hovered() || r.has_focus() {
        Some(th.hover_fill())
    } else {
        None
    };
    if let Some(f) = fill {
        ui.painter().set(
            bg,
            egui::epaint::RectShape::filled(pill, th.radius("md"), f),
        );
    }
    if r.has_focus() {
        ui.painter().rect_stroke(
            pill,
            th.radius("md"),
            Stroke::new(1.5, th.c("focus.ring")),
            StrokeKind::Outside,
        );
    }
    Some(r.on_hover_text(tip))
}

/// Open/closed state of one item's element list; other items keep theirs.
fn flip_open(set: &mut std::collections::HashSet<u32>, id: u32) {
    if !set.remove(&id) {
        set.insert(id);
    }
}

/// `All 30 items` box and the `checked of total` count above the item rows.
fn all_row(ui: &mut Ui, cx: &Cx<'_>, plan: &PlanView, acts: &mut Vec<Action>) {
    if plan.items.len() < 2 {
        return;
    }
    let (th, cat) = (cx.th, cx.cat);
    let state = logic::all_state(&plan.items);
    let label = cat.plural("ai.items.all", plan.items.len() as u64);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        let mut on = state == logic::AllState::All;
        let mixed = state == logic::AllState::Mixed;
        let r = widgets::checkbox_mixed(ui, th, &mut on, mixed, &label)
            .on_hover_text(cat.t("ai.items.all.tip"));
        if r.changed() {
            acts.push(Action::SetChecked(logic::toggle_all(
                &plan.items,
                logic::all_click_target(state),
            )));
        }
        let (n, m) = review::accepted(&plan.items);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            kit::caption(
                ui,
                th,
                &cat.tf(
                    "ai.items.checked",
                    &[("n", &n.to_string()), ("m", &m.to_string())],
                ),
            );
        });
    });
}

/// Apply label by the checked count; bare `Apply` when nothing is checked.
fn apply_label(cat: &Catalog, checked: usize) -> String {
    if checked == 0 {
        cat.t("ai.items.apply_none")
    } else {
        cat.plural("ai.items.apply", checked as u64)
    }
}

/// Apply needs a checked item on top of everything the session already requires.
/// Apply of a draft (the plan or an agent proposal) is armed only while no reason holds it off.
pub(super) fn apply_armed(reason: Option<(&'static str, Tone)>) -> bool {
    reason.is_none()
}

/// A draft card is stale once the live model moved past the head its preview was drawn on (the
/// check the executor repeats on Apply), or once the session says so.
pub(super) fn draft_stale(cx: &Cx<'_>, base_head: u64, said: bool) -> bool {
    said || cx.ed.model.plan_stale(base_head)
}

fn keys_of_kind(diff: &[ElemDiff], k: RowKind) -> Vec<ElKey> {
    diff.iter()
        .filter(|e| e.kind == k)
        .map(|e| e.key.clone())
        .collect()
}

fn summary(
    ui: &mut Ui,
    cx: &Cx<'_>,
    plan: &PlanView,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let (c, ch, d) = crate::ai::plan::counts(&plan.diff);
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(cat.t("ai.plan.title"))
                .font(th.font(Role::Section))
                .color(th.c("text.secondary")),
        );
        for (k, n, tip) in [
            (RowKind::Created, c, "ai.plan.created"),
            (RowKind::Changed, ch, "ai.plan.changed"),
            (RowKind::Deleted, d, "ai.plan.deleted"),
        ] {
            if let Some(r) = chip(ui, th, k, n, &cat.t(tip)) {
                if r.hovered() {
                    *hot = Some(keys_of_kind(&plan.diff, k));
                }
                if r.clicked() {
                    acts.push(Action::Zoom(keys_of_kind(&plan.diff, k)));
                }
            }
        }
    });
    let units = cx.ed.tool.units;
    for q in logic::headline_qty(&plan.qty, QTY_LINES) {
        kit::caption(ui, th, &qty_line(cat, q, units));
    }
}

/// `Concrete +12.4 m³` (localized separators); totals read through their own names.
pub fn qty_line(cat: &Catalog, q: &QtyDelta, u: Units) -> String {
    let d = q.after - q.before;
    let sign = if d < 0.0 { "−" } else { "+" };
    let m = d.abs();
    let ru = cat.lang == ddc_ui::Lang::Ru;
    let vol = |m: f64| fmt_volume_lang(m, u, precision().1, ru);
    if let Some(name) = q.key.strip_prefix("volume:") {
        return format!("{name} {sign}{}", vol(m));
    }
    if let Some(name) = q.key.strip_prefix("area:") {
        return format!("{name} {sign}{}", fmt_area_in_lang(m, u, ru));
    }
    let value = if q.key.ends_with("_volume") {
        vol(m)
    } else if q.key.ends_with("_area") {
        fmt_area_in_lang(m, u, ru)
    } else {
        fmt_num_lang(m, 0, ru)
    };
    format!("{} {sign}{value}", cat.t(&format!("ai.qty.{}", q.key)))
}

fn variants(ui: &mut Ui, cx: &Cx<'_>, plan: &PlanView, acts: &mut Vec<Action>) {
    if plan.variants.len() < 2 {
        return;
    }
    let th = cx.th;
    let names: Vec<&str> = plan
        .variants
        .iter()
        .enumerate()
        .map(|(i, _)| VARIANT_NAMES.get(i).copied().unwrap_or("…"))
        .collect();
    let sel = plan
        .variants
        .iter()
        .position(|v| v.id == plan.variant)
        .unwrap_or(0);
    if let Some(i) = widgets::segmented(ui, th, &names, sel) {
        if i != sel {
            acts.push(Action::SetVariant(plan.variants[i].id));
        }
    }
    if let Some(v) = plan.variants.get(sel) {
        let ru = cx.cat.lang == ddc_ui::Lang::Ru;
        let line = v
            .metrics
            .iter()
            .map(|(k, x)| format!("{k}: {}", fmt_num_lang(*x, 1, ru)))
            .collect::<Vec<_>>()
            .join(" · ");
        kit::caption(ui, th, &line);
    }
}

fn issues(
    ui: &mut Ui,
    cx: &Cx<'_>,
    plan: &PlanView,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let all: Vec<PlanIssue> = plan
        .issues
        .iter()
        .chain(&plan.main_issues)
        .cloned()
        .collect();
    let sorted = logic::sorted_issues(&all);
    for issue in sorted.iter().copied().filter(|i| i.level == IssueLevel::Block) {
        issue_line(ui, cx, issue, acts, hot);
    }
    // warnings and footnotes fold behind one line; blockers above always stay in view
    let warns: Vec<&PlanIssue> = sorted
        .iter()
        .copied()
        .filter(|i| i.level == IssueLevel::Warn)
        .collect();
    let notes = warns.len() + usize::from(plan.flagged > 0) + usize::from(plan.truncated);
    let mut open = false;
    if notes > 0 {
        open = notes_header(ui, cx, notes);
    }
    if open {
        for issue in warns {
            issue_line(ui, cx, issue, acts, hot);
        }
        if plan.flagged > 0 {
            kit::warn_note(
                ui,
                th,
                &cat.tf("ai.plan.flagged", &[("n", &plan.flagged.to_string())]),
            );
        }
        if plan.truncated {
            kit::caption(ui, th, &cat.t("ai.plan.truncated"));
        }
    }
    if logic::allow_large_offer(&plan.issues, &plan.main_issues, plan.allow_large) {
        let mut on = plan.allow_large;
        if kit::checkbox(ui, th, &mut on, &cat.t("ai.plan.limit_allow")).changed() {
            acts.push(Action::AllowLarge(on));
        }
    }
}

fn issue_line(
    ui: &mut Ui,
    cx: &Cx<'_>,
    issue: &PlanIssue,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let col = match issue.level {
        IssueLevel::Block => th.c("error"),
        IssueLevel::Warn => th.c("warning"),
    };
    let sense = if issue.el.is_some() {
        Sense::click()
    } else {
        Sense::hover()
    };
    let r = ui.add(
        egui::Label::new(
            RichText::new(logic::issue_text(cat, issue))
                .font(th.font(Role::Label))
                .color(col),
        )
        .wrap()
        .sense(sense),
    );
    if let Some(el) = &issue.el {
        if r.hovered() {
            *hot = Some(vec![el.clone()]);
        }
        if r.clicked() {
            acts.push(Action::Zoom(vec![el.clone()]));
        }
    }
}

/// One line "Notes: n" with the fold toggle; true while the notes are open.
fn notes_header(ui: &mut Ui, cx: &Cx<'_>, n: usize) -> bool {
    let (th, cat) = (cx.th, cx.cat);
    let id = egui::Id::new("ddc.ai.plan.notes_open");
    let mut open: bool = ui.data(|d| d.get_temp(id)).unwrap_or(false);
    ui.horizontal(|ui| {
        let tip = cat.t(if open { "ai.plan.notes.hide" } else { "ai.plan.notes.show" });
        if widgets::shell_kit::disclosure(ui, th, open, &tip).clicked() {
            open = !open;
        }
        ui.label(
            RichText::new(cat.tf("ai.plan.notes", &[("n", &n.to_string())]))
                .font(th.font(Role::Label))
                .color(th.c("warning")),
        );
    });
    ui.data_mut(|d| d.insert_temp(id, open));
    open
}

fn counts_row(ui: &mut Ui, th: &Theme, c: Counts) {
    for (k, n) in [
        (RowKind::Deleted, c.deleted),
        (RowKind::Changed, c.changed),
        (RowKind::Created, c.created),
    ] {
        if n > 0 {
            let col = kind_color(ui, k);
            let text = crate::units::fmt_num(n as f64, 0);
            kind_icon(ui, k);
            ui.label(RichText::new(text).font(th.font(Role::Numeric)).color(col));
        }
    }
}

/// Plain painter text on one allocated rectangle: `show_rows` may pass thousands of these.
fn diff_rows(
    ui: &mut Ui,
    cx: &Cx<'_>,
    salt: u32,
    rows: &[&ElemDiff],
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let th = cx.th;
    let h = th.d().row;
    let shown = (rows.len() as f32).min(ITEM_ROWS_SHOWN) * h;
    egui::ScrollArea::vertical()
        .id_salt(("ddc.ai.rows", salt))
        .max_height(shown)
        .auto_shrink([false, true])
        .show_rows(ui, h, rows.len(), |ui, range| {
            for e in &rows[range] {
                let (rect, r) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                if r.hovered() {
                    kit::row_highlight(ui, th, rect, false);
                    *hot = Some(vec![e.key.clone()]);
                }
                let p = ui.painter_at(rect);
                let font = th.font(Role::Label);
                let at = rect.left_center() + Vec2::new(space::XS, 0.0);
                let sign = p.text(
                    at,
                    Align2::LEFT_CENTER,
                    preview::sign(e.kind),
                    font.clone(),
                    kind_color(ui, e.kind),
                );
                let label = p.text(
                    sign.right_center() + Vec2::new(space::XS, 0.0),
                    Align2::LEFT_CENTER,
                    &e.label,
                    font,
                    th.c("text.primary"),
                );
                let detail = logic::cell_summary(e);
                if !detail.is_empty() {
                    p.text(
                        label.right_center() + Vec2::new(space::SM, 0.0),
                        Align2::LEFT_CENTER,
                        &detail,
                        th.font(Role::Caption),
                        th.c("text.secondary"),
                    );
                }
                if r.clicked() {
                    acts.push(Action::Zoom(vec![e.key.clone()]));
                }
                if !detail.is_empty() {
                    r.on_hover_text(format!("{}\n{detail}", e.label));
                } else {
                    r.on_hover_text(&e.label);
                }
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn item_row(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    plan: &PlanView,
    index: usize,
    fresh: bool,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let it = &plan.items[index];
    let counts = logic::item_counts(&plan.diff, it.id);
    let bg = ui.painter().add(Shape::Noop);
    let at_cursor = st.cursor == Some(it.id);
    // an id per item keeps hover and open state on the same row when the list changes
    let row = ui.push_id(("ai.item", it.id), |ui| ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        let mut on = it.checked;
        // the title beside the box zooms, so the box is named for screen readers and tooltips
        let tip = cat.tf("ai.items.check.tip", &[("title", &it.title)]);
        let cb = kit::checkbox(ui, th, &mut on, "");
        cb.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, &it.title)
        });
        if cb.on_hover_text(tip).changed() {
            st.cursor = Some(it.id);
            acts.push(Action::SetChecked(logic::toggled(&plan.items, it.id)));
        }
        let open = st.expanded.contains(&it.id);
        if counts.total() > 0 {
            let tip = cat.t(if open {
                "ai.items.collapse"
            } else {
                "ai.items.expand"
            });
            if widgets::shell_kit::disclosure(ui, th, open, &tip).clicked() {
                flip_open(&mut st.expanded, it.id);
            }
        } else {
            ui.add_space(th.d().control_sm);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            counts_row(ui, th, counts);
            // back to left-to-right for what is left of the row: the title starts at its left edge
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let col = if it.checked {
                    th.c("text.primary")
                } else {
                    th.c("text.disabled")
                };
                if fresh {
                    super::plan_delta::row_mark(ui, cx);
                }
                if let Some(name) = logic::agent_of(&it.actor) {
                    kit::badge(ui, th, name, kit::Tone::Accent);
                }
                let title = ui.add(
                    egui::Label::new(
                        RichText::new(&it.title)
                            .font(th.font(Role::Label))
                            .color(col),
                    )
                    .truncate()
                    .sense(Sense::click()),
                );
                if title.clicked() {
                    st.cursor = Some(it.id);
                    acts.push(Action::Zoom(logic::keys_of(&plan.diff, it.id)));
                }
                title.on_hover_text(&it.title);
            });
        });
    })).inner;
    let rect = row.response.rect;
    let hover = ui.rect_contains_pointer(rect);
    if hover {
        *hot = Some(logic::keys_of(&plan.diff, it.id));
    }
    if hover || at_cursor {
        let fill = th.c("bg.hover");
        ui.painter().set(
            bg,
            egui::epaint::RectShape::filled(rect, th.radius("sm"), fill),
        );
    }
    if at_cursor {
        if st.cursor_moved {
            ui.scroll_to_rect(rect, None);
        }
        // the ring is bold while the review keys are live
        let width = if st.card_focus { 2.0 } else { 1.0 };
        ui.painter().rect_stroke(
            rect,
            th.radius("sm"),
            Stroke::new(width, th.c("accent")),
            StrokeKind::Inside,
        );
        if hot.is_none() {
            *hot = Some(logic::keys_of(&plan.diff, it.id));
        }
    }
    super::value_row::values(ui, st, cx, plan, it, acts);
    if let Some(c) = &it.conflict {
        let lang = ddc_ai::refusal::lang(ddc_ui::i18n::current().code());
        let c = logic::conflict_text(c, lang);
        kit::warn_note(ui, th, &cat.tf("ai.plan.conflict", &[("what", &c)]));
    }
    if st.expanded.contains(&it.id) {
        diff_rows(ui, cx, it.id, &logic::rows_of(&plan.diff, it.id), acts, hot);
    }
}

/// The review keys of `review::CHORDS`; which one acts is decided by the pure `review_key`.
/// Alt+Space would open the window's system menu, so Space is bare and needs card focus.
fn keyboard(ui: &Ui, st: &mut PanelUi, plan: &PlanView, can_apply: bool, acts: &mut Vec<Action>) {
    let ctx = ui.ctx();
    let typing = ctx.wants_keyboard_input();
    let rc = ReviewCtx {
        card_focus: st.card_focus,
        typing,
        widget_focus: ctx.memory(|m| m.focused().is_some()),
        can_apply,
        has_items: !plan.items.is_empty(),
    };
    for (key, mods) in review::CHORDS {
        let Some(act) = review::review_key(key, mods, &rc) else {
            continue;
        };
        // consume_key ignores extra Shift/Alt; Alt+A must stay a ribbon chord, not Accept
        if !ui.input(|i| i.modifiers.matches_exact(mods)) {
            continue;
        }
        if !ui.input_mut(|i| i.consume_key(mods, key)) {
            continue;
        }
        let o = review::outcome(act, &plan.items, &plan.diff, st.cursor);
        if o.cursor != st.cursor {
            st.cursor = o.cursor;
            st.cursor_moved = true;
        }
        if matches!(act, ReviewAct::Move(_)) && mods.alt {
            // walking the card takes the keyboard from the chat input
            st.card_focus = true;
            if typing {
                ctx.memory_mut(|m| {
                    if let Some(id) = m.focused() {
                        m.surrender_focus(id);
                    }
                });
            }
        }
        if !o.check.is_empty() {
            acts.push(Action::SetChecked(o.check));
        }
        if o.apply {
            acts.push(Action::Apply);
        }
        if let Some(id) = o.zoom {
            acts.push(Action::Zoom(logic::keys_of(&plan.diff, id)));
        }
        if o.leave {
            // back to the viewport; the draft and its check marks stay as they are
            st.card_focus = false;
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
    }
}

/// A checked item writes values: the card says the assistant filled them and what that is not.
fn fills_values(items: &[crate::ai::plan::PlanItem]) -> bool {
    use crate::ai::plan::PlanOp;
    items.iter().any(|i| i.checked && matches!(i.op, PlanOp::Assign { .. }))
}

pub fn card(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    plan: &PlanView,
    f: &Facts,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (th, cat) = (cx.th, cx.cat);
    // the review keys wait while Reject asks; Esc and Enter there only keep the draft
    let confirm_open = ui.ctx().data(|d| d.get_temp::<u64>(egui::Id::new("ai.plan.reject_ask")))
        == Some(f.plan_rev);
    let stale = draft_stale(cx, plan.compiled.base_head, f.stale);
    st.values.busy = f.busy;
    if !cx.palette_open && !confirm_open {
        // Ctrl+Enter follows the button: armed only while no reason holds Apply off
        let why = apply_why(f, plan, stale, st.values.choosing(f.busy));
        keyboard(ui, st, plan, apply_armed(why), acts);
    }
    let top = ui.cursor().min;
    let wide = ui.available_width();
    kit::card(ui, th, |ui| {
        ui.set_width(ui.available_width());
        summary(ui, cx, plan, acts, hot);
        if stale {
            kit::warn_note(ui, th, &cat.t("ai.plan.stale"));
        } else if let Some(d) = &f.rebased {
            // what the recompile changed, not only that it ran: the rows carry the mark
            let what = d.note().counts(cat);
            kit::caption(ui, th, &cat.tf("ai.plan.rebased", &[("what", &what)]))
                .on_hover_text(d.tip(cat));
        } else if st.recomputed {
            kit::caption(ui, th, &cat.t("ai.plan.recomputed"));
        }
        variants(ui, cx, plan, acts);
        if fills_values(&plan.items) {
            kit::warn_note(ui, th, &cat.t("value_src.ai_fill_note"));
        }
        issues(ui, cx, plan, acts, hot);
        ui.add_space(space::XS);
        if let Some(d) = &f.redraft {
            super::plan_delta::strip(ui, cx, d, acts);
        }
        review_bar(ui, cx, plan, acts);
        all_row(ui, cx, plan, acts);
        for i in 0..plan.items.len() {
            let id = plan.items[i].id;
            let fresh = f.redraft.iter().chain(&f.rebased).any(|d| d.marks(id));
            item_row(ui, st, cx, plan, i, fresh, acts, hot);
        }
        super::value_row::held_lost(ui, st, cx, plan);
        if plan.secondary > 0 {
            ui.add_space(space::XS);
            kit::caption(
                ui,
                th,
                &cat.tf("ai.plan.secondary", &[("n", &plan.secondary.to_string())]),
            );
            if st.show_all {
                let rest: Vec<&ElemDiff> = plan.diff.iter().filter(|e| e.item.is_none()).collect();
                diff_rows(ui, cx, u32::MAX, &rest, acts, hot);
            }
        }
        deliver_row(ui, st, cx, plan);
        ui.add_space(space::XS);
        kit::checkbox(ui, th, &mut st.preview_on, &cat.t("ai.plan.preview_on"));
    });
    st.cursor_moved = false;
    // a press in the card gives it the review keys, a press anywhere else gives them back
    let area = egui::Rect::from_min_max(top, egui::pos2(top.x + wide, ui.min_rect().max.y));
    if let Some(at) = ui.input(|i| i.pointer.any_pressed().then(|| i.pointer.interact_pos()).flatten()) {
        st.card_focus = area.contains(at);
    }
}

/// The files a delivery card writes after Apply: ticked as the assistant asked, the person's
/// to change. The folder is the one the run will use.
fn deliver_row(ui: &mut Ui, st: &mut PanelUi, cx: &Cx<'_>, plan: &PlanView) {
    if st.deliver_seen != plan.deliver {
        st.deliver_seen = plan.deliver;
        st.deliver = plan.deliver;
    }
    let Some(o) = st.deliver.as_mut() else {
        return;
    };
    let (th, cat) = (cx.th, cx.cat);
    ui.add_space(space::XS);
    kit::caption(ui, th, &cat.t("deliver.card.after"));
    let ifc = format!("{} {}", cat.t("deliver.ifc"), o.schema_name());
    for (on, label) in [
        (&mut o.pdf, cat.t("deliver.pdf")),
        (&mut o.ifc, ifc),
    ] {
        kit::checkbox(ui, th, on, &label);
    }
    let dir = crate::deliver::Target::default_for(cx.ed).dir.display().to_string();
    kit::caption(ui, th, &cat.tf("deliver.card.folder", &[("dir", &dir)]));
}

/// Agent items that joined after the person's last look: Apply waits for this confirmation.
fn review_bar(ui: &mut Ui, cx: &Cx<'_>, plan: &PlanView, acts: &mut Vec<Action>) {
    if plan.unreviewed.is_empty() {
        return;
    }
    let (th, cat) = (cx.th, cx.cat);
    let who = logic::proposer_line(&plan.items, &plan.unreviewed);
    kit::warn_note(
        ui,
        th,
        &cat.tf(
            "ai.plan.review_needed",
            &[("n", &plan.unreviewed.len().to_string()), ("who", &who)],
        ),
    );
    if Button::secondary(&cat.t("ai.plan.review_done"))
        .small()
        .show(ui, th)
        .clicked()
    {
        acts.push(Action::Review);
    }
}

/// Under the dialog, always in reach: Apply, Reject, and "show all" on the views.
pub fn actions(
    ui: &mut Ui,
    st: &PanelUi,
    cx: &Cx<'_>,
    plan: &PlanView,
    f: &Facts,
    acts: &mut Vec<Action>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let ask_id = egui::Id::new("ai.plan.reject_ask");
    let mut asking = ui.ctx().data(|d| d.get_temp::<u64>(ask_id)) == Some(f.plan_rev);
    // the `ai.reject` command asked for the same question the button asks
    asking |= take_ask(f.plan_rev);
    let stale = draft_stale(cx, plan.compiled.base_head, f.stale);
    let reason = apply_why(f, plan, stale, st.values.choosing(f.busy));
    let reason_text = reason.map(|(key, tone)| (cat.t(key), tone));
    if asking {
        let hint = cat.t("ai.plan.reject_ask.hint");
        let (a, go) =
            reject_confirm(ui, th, cat, plan.items.len() as u64, &hint, ask_id.with("seen"));
        asking = a;
        if go {
            acts.push(Action::Reject);
        }
    } else {
        ui.ctx().data_mut(|d| d.insert_temp(ask_id.with("seen"), false));
        // at the narrowest panel "show all" takes the next line instead of leaving the panel
        ui.horizontal_wrapped(|ui| {
            let why = reason_text
                .as_ref()
                .map(|(t, _)| t.clone())
                .unwrap_or_else(|| cat.t("cmd.ai.apply.desc"));
            let (n, m) = review::accepted(&plan.items);
            let (ns, ms) = (n.to_string(), m.to_string());
            let label = match &f.rebased {
                Some(_) if n > 0 => cat.t("ai.plan.apply_updated"),
                _ => apply_label(cat, n),
            };
            let why = format!(
                "{why}\n{}\n{}",
                cat.tf("ai.plan.will_apply", &[("n", &ns), ("m", &ms)]),
                cat.t("ai.plan.keys_anywhere")
            );
            if Button::primary(&label)
                .enabled(apply_armed(reason))
                .tooltip(&why, "Ctrl+Enter")
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::Apply);
            }
            if Button::quiet(&cat.t("ai.plan.reject"))
                .show(ui, th)
                .clicked()
            {
                let (a, go) = reject_step(false, reject_needs_confirm(plan), Ask::Reject);
                asking = a;
                if go {
                    acts.push(Action::Reject);
                }
            }
            show_all_button(ui, st, cx, acts);
            if Button::quiet(&cat.t("collab_diff.reveal.start")).show(ui, th).clicked() {
                crate::reveal::request(crate::reveal::from_elem_diffs(&plan.diff), true);
            }
        });
        if let Some((text, tone)) = &reason_text {
            reason_note(ui, th, text, *tone);
        }
    }
    ui.ctx().data_mut(|d| {
        if asking {
            d.insert_temp(ask_id, f.plan_rev);
        } else {
            d.remove::<u64>(ask_id);
        }
    });
    plan_card_footer(ui, cx, plan);
}

/// "Show all" on the views, next to Apply and Reject.
fn show_all_button(ui: &mut Ui, st: &PanelUi, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    {
        let all = cat.t(if st.show_all {
            "ai.plan.show_less"
        } else {
            "ai.plan.show_all"
        });
        if Button::quiet(&all).show(ui, th).clicked() {
            acts.push(Action::ZoomAll);
        }
    }
}

fn plan_card_footer(ui: &mut Ui, cx: &Cx<'_>, plan: &PlanView) {
    let (th, cat) = (cx.th, cx.cat);
    let (n, m) = review::accepted(&plan.items);
    kit::caption(
        ui,
        th,
        &cat.tf(
            "ai.plan.will_apply",
            &[("n", &n.to_string()), ("m", &m.to_string())],
        ),
    );
    kit::caption(ui, th, &cat.t("ai.plan.keys_hint"));
}

/// Drafts of this many items or more ask before Reject.
const REJECT_CONFIRM_ITEMS: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Warn,
    Muted,
}

/// Why Apply is off, under the buttons: a warning, or a quiet caption.
pub(super) fn reason_note(ui: &mut Ui, th: &Theme, text: &str, tone: Tone) {
    match tone {
        Tone::Warn => kit::warn_note(ui, th, text),
        Tone::Muted => {
            kit::caption(ui, th, text);
        }
    }
}

/// The Reject confirm of the plan draft and of agent proposals: the question for `n` changes,
/// `hint`, Keep (focused on first show, `seen` remembers it) and the danger Reject. Esc and
/// Enter keep. Returns (confirm still open, drop now).
pub(super) fn reject_confirm(
    ui: &mut Ui,
    th: &Theme,
    cat: &Catalog,
    n: u64,
    hint: &str,
    seen: egui::Id,
) -> (bool, bool) {
    let mut asking = true;
    let esc = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let enter = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    for ev in [(esc, Ask::Esc), (enter, Ask::Enter)] {
        if ev.0 {
            asking = reject_step(asking, true, ev.1).0;
        }
    }
    if !asking {
        return (false, false);
    }
    let mut go = false;
    kit::warn_note(ui, th, &cat.plural("ai.plan.reject_ask", n));
    kit::caption(ui, th, hint);
    ui.horizontal(|ui| {
        let keep = Button::secondary(&cat.t("ai.plan.reject_keep")).show(ui, th);
        if !ui.ctx().data(|d| d.get_temp::<bool>(seen).unwrap_or(false)) {
            keep.request_focus();
            ui.ctx().data_mut(|d| d.insert_temp(seen, true));
        }
        if keep.clicked() {
            asking = reject_step(asking, true, Ask::Keep).0;
        }
        if Button::danger(&cat.t("ai.plan.reject_confirm"))
            .show(ui, th)
            .clicked()
        {
            (asking, go) = reject_step(asking, true, Ask::Confirm);
        }
    });
    (asking, go)
}

/// Everything `apply_why` reads, so the rule is testable without a session.
struct WhyIn {
    can_apply: bool,
    stale: bool,
    blocked: bool,
    unreviewed: bool,
    redraft: bool,
    checked: usize,
    /// A value the person corrected while the agent changed it too waits for their pick.
    choosing: bool,
}

/// Why Apply is off, as a catalog key and a tone; None while it is armed.
fn apply_why_of(w: &WhyIn) -> Option<(&'static str, Tone)> {
    if w.stale {
        Some(("ai.plan.stale", Tone::Warn))
    } else if w.blocked {
        Some(("ai.err.plan_blocked", Tone::Warn))
    } else if w.unreviewed {
        Some(("ai.plan.review_first", Tone::Warn))
    } else if w.redraft {
        Some(("ai.redraft.review_first", Tone::Warn))
    } else if w.choosing {
        Some(("ai.value.choose_first", Tone::Warn))
    } else if w.checked == 0 {
        Some(("ai.plan.nothing_checked", Tone::Muted))
    } else if !w.can_apply {
        Some(("ai.state.recomputing", Tone::Muted))
    } else {
        None
    }
}

fn apply_why(
    f: &Facts,
    plan: &PlanView,
    stale: bool,
    choosing: bool,
) -> Option<(&'static str, Tone)> {
    apply_why_of(&WhyIn {
        can_apply: f.can_apply,
        stale,
        blocked: plan.blocked(),
        unreviewed: !plan.unreviewed.is_empty(),
        redraft: f.redraft.is_some(),
        checked: review::accepted(&plan.items).0,
        choosing,
    })
}

/// A big draft, or one the person has worked on (unchecked an item, picked a variant, allowed
/// a large change), is not dropped on one click.
fn reject_needs_confirm_of(items: usize, unchecked: usize, variant: u32, allow_large: bool) -> bool {
    items >= REJECT_CONFIRM_ITEMS || unchecked > 0 || variant != 0 || allow_large
}

/// What the person did at the Reject control.
#[derive(Clone, Copy, Debug)]
pub(super) enum Ask {
    Reject,
    Confirm,
    Keep,
    Esc,
    Enter,
}

/// (confirm still open, drop the draft now). Only Reject on a small draft and Confirm drop it;
/// Esc and Enter never do.
pub(super) fn reject_step(asking: bool, needs_confirm: bool, ev: Ask) -> (bool, bool) {
    match ev {
        Ask::Reject if !needs_confirm => (false, true),
        Ask::Reject => (true, false),
        Ask::Confirm if asking => (false, true),
        Ask::Confirm => (false, false),
        Ask::Keep | Ask::Esc | Ask::Enter => (false, false),
    }
}

thread_local! {
    /// Draft revision whose Reject confirm the `ai.reject` command wants open on the next card.
    static ASK_REQUEST: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

fn take_ask(rev: u64) -> bool {
    ASK_REQUEST.with(|c| {
        if c.get() == Some(rev) {
            c.set(None);
            true
        } else {
            false
        }
    })
}

/// True when the command may send `Action::Reject` now; false when the card asks first.
fn route_reject(needs_confirm: bool, rev: u64) -> bool {
    let (ask, go) = reject_step(false, needs_confirm, Ask::Reject);
    if ask {
        ASK_REQUEST.with(|c| c.set(Some(rev)));
    }
    go
}

/// The `ai.reject` command: the same confirm rule as the Reject button. No draft: sends at once.
pub fn reject_command(plan: Option<&PlanView>, rev: u64) -> bool {
    match plan {
        Some(p) => route_reject(reject_needs_confirm(p), rev),
        None => true,
    }
}

fn reject_needs_confirm(plan: &PlanView) -> bool {
    let unchecked = plan.items.iter().filter(|i| !i.checked).count();
    reject_needs_confirm_of(plan.items.len(), unchecked, plan.variant, plan.allow_large)
}

fn legend_item(ui: &mut Ui, th: &Theme, k: RowKind, n: usize, label: &str) {
    if n == 0 {
        return;
    }
    let col = kind_color(ui, k);
    kind_icon(ui, k);
    ui.label(
        RichText::new(format!("{label} {}", crate::units::fmt_num(n as f64, 0)))
            .font(th.font(Role::Caption))
            .color(col),
    );
}

fn omitted_note(ui: &mut Ui, th: &Theme, cat: &Catalog, set: &PreviewSet) {
    if set.omitted > 0 {
        kit::caption(
            ui,
            th,
            &cat.tf("ai.plan.omitted", &[("n", &set.omitted.to_string())]),
        );
    }
}

/// The form of a mark also says its kind, so colour alone does not carry the meaning.
pub fn legend(ui: &mut Ui, th: &Theme, cat: &Catalog, plan: &PlanView) {
    let (c, ch, d) = crate::ai::plan::counts(&plan.diff);
    kit::card(ui, th, |ui| {
        ui.horizontal(|ui| {
            legend_item(ui, th, RowKind::Created, c, &cat.t("ai.plan.created"));
            legend_item(ui, th, RowKind::Changed, ch, &cat.t("ai.plan.changed"));
            legend_item(ui, th, RowKind::Deleted, d, &cat.t("ai.plan.deleted"));
        });
        omitted_note(ui, th, cat, &plan.preview);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat() -> Catalog {
        Catalog::new(ddc_ui::Lang::En)
    }

    fn q(key: &str, before: f64, after: f64) -> QtyDelta {
        QtyDelta {
            key: key.to_string(),
            level: None,
            before,
            after,
        }
    }

    #[test]
    fn material_lines_use_the_name_a_sign_and_a_unit() {
        let line = qty_line(&cat(), &q("volume:Concrete", 1.0, 13.4), Units::Metric);
        assert!(line.starts_with("Concrete +"), "{line}");
        let line = qty_line(&cat(), &q("area:Glass", 5.0, 2.0), Units::Metric);
        assert!(line.starts_with("Glass −"), "{line}");
    }

    fn en() -> Catalog {
        Catalog::new(ddc_ui::Lang::En)
    }

    fn ru() -> Catalog {
        Catalog::new(ddc_ui::Lang::Ru)
    }

    /// Values the assistant fills carry the note, in both languages; other edits do not.
    #[test]
    fn a_fill_card_says_who_filled_the_values() {
        use crate::ai::plan::{PlanItem, PlanOp, Targets};
        let it = |op: PlanOp, checked: bool| PlanItem {
            id: 1,
            title: String::new(),
            why: String::new(),
            op,
            checked,
            depends_on: vec![],
            actor: String::new(),
            conflict: None,
            sets: Default::default(),
        };
        let fill = || PlanOp::Assign {
            target: Targets::Selection,
            set: vec![],
        };
        assert!(fills_values(&[it(fill(), true)]));
        assert!(!fills_values(&[it(fill(), false)]), "an unchecked fill is not applied");
        let del = PlanOp::Delete {
            target: Targets::Selection,
        };
        assert!(!fills_values(&[it(del, true)]));
        let (e, r) = (en().t("value_src.ai_fill_note"), ru().t("value_src.ai_fill_note"));
        assert!(e.starts_with("Filled by AI.") && e.contains("verify"), "{e}");
        assert!(r != e && r != "value_src.ai_fill_note", "{r}");
    }

    #[test]
    fn apply_label_follows_the_checked_count() {
        assert_eq!(apply_label(&en(), 0), "Apply");
        assert_eq!(apply_label(&en(), 1), "Apply 1 item");
        assert_eq!(apply_label(&en(), 5), "Apply 5 items");
        assert_eq!(apply_label(&ru(), 1), crate::app2_fx::fx("plan_apply_1"));
        assert_eq!(apply_label(&ru(), 3), crate::app2_fx::fx("plan_apply_3"));
        assert_eq!(apply_label(&ru(), 11), crate::app2_fx::fx("plan_apply_11"));
        assert_eq!(apply_label(&ru(), 21), crate::app2_fx::fx("plan_apply_21"));
    }

    #[test]
    fn apply_is_disabled_at_zero_checked() {
        let on = |w: WhyIn| apply_armed(apply_why_of(&w));
        assert!(!on(WhyIn { checked: 0, ..armed() }));
        assert!(on(WhyIn { checked: 2, ..armed() }));
        assert!(!on(WhyIn { can_apply: false, ..armed() }));
    }

    #[test]
    fn a_stale_draft_is_never_armed_even_if_the_session_says_it_may_apply() {
        // can_apply and checked alone used to arm Apply; stale now holds it off with its reason
        let w = WhyIn { stale: true, ..armed() };
        assert_eq!(apply_why_of(&w), Some(("ai.plan.stale", Tone::Warn)));
        assert!(!apply_armed(apply_why_of(&w)));
        assert!(apply_armed(apply_why_of(&armed())));
    }

    #[test]
    fn all_label_and_count_read_in_both_languages() {
        assert_eq!(en().plural("ai.items.all", 1), "All 1 item");
        assert_eq!(en().plural("ai.items.all", 30), "All 30 items");
        assert_eq!(ru().plural("ai.items.all", 2), crate::app2_fx::fx("plan_all_2"));
        assert_eq!(ru().plural("ai.items.all", 5), crate::app2_fx::fx("plan_all_5"));
        assert_eq!(
            en().tf("ai.items.checked", &[("n", "3"), ("m", "30")]),
            "3 of 30 checked"
        );
        assert_ne!(ru().t("ai.items.expand"), en().t("ai.items.expand"));
    }

    #[test]
    fn each_item_keeps_its_own_open_state() {
        let mut open = std::collections::HashSet::new();
        flip_open(&mut open, 2);
        flip_open(&mut open, 7);
        assert!(open.contains(&2) && open.contains(&7));
        // the same set read on the next frame, then item 2 closed: item 7 stays open
        flip_open(&mut open, 2);
        assert!(!open.contains(&2) && open.contains(&7));
        flip_open(&mut open, 2);
        assert!(open.contains(&2) && open.contains(&7));
    }

    #[test]
    fn rows_use_the_kit_not_raw_checkboxes_or_glyph_toggles() {
        let src = include_str!("plan_card.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        let (down, right) = (char::from_u32(0x25BE).unwrap(), char::from_u32(0x25B8).unwrap());
        assert!(!code.contains(down) && !code.contains(right));
        assert!(!code.contains("ui.checkbox("));
        // the chevron is the shared kit one, not a local copy
        assert!(code.contains("shell_kit::disclosure("));
        assert!(!code.contains("fn disclosure(") && !code.contains("fn chevron("));
    }

    fn armed() -> WhyIn {
        WhyIn {
            can_apply: true,
            stale: false,
            blocked: false,
            unreviewed: false,
            redraft: false,
            checked: 3,
            choosing: false,
        }
    }

    #[test]
    fn apply_why_names_each_disabled_reason() {
        assert_eq!(apply_why_of(&armed()), None);
        let cases = [
            (WhyIn { stale: true, can_apply: false, ..armed() }, "ai.plan.stale", Tone::Warn),
            (WhyIn { blocked: true, can_apply: false, ..armed() }, "ai.err.plan_blocked", Tone::Warn),
            (WhyIn { unreviewed: true, can_apply: false, ..armed() }, "ai.plan.review_first", Tone::Warn),
            (WhyIn { redraft: true, can_apply: false, ..armed() }, "ai.redraft.review_first", Tone::Warn),
            (WhyIn { choosing: true, ..armed() }, "ai.value.choose_first", Tone::Warn),
            (WhyIn { checked: 0, ..armed() }, "ai.plan.nothing_checked", Tone::Muted),
            (WhyIn { can_apply: false, ..armed() }, "ai.state.recomputing", Tone::Muted),
        ];
        for (w, key, tone) in cases {
            assert_eq!(apply_why_of(&w), Some((key, tone)), "{key}");
            let (e, r) = (en().t(key), ru().t(key));
            assert!(!e.is_empty() && e != key, "{key} en");
            assert!(!r.is_empty() && r != key && r != e, "{key} ru");
        }
    }

    #[test]
    fn stale_wins_over_the_other_reasons() {
        let w = WhyIn { stale: true, blocked: true, checked: 0, can_apply: false, ..armed() };
        assert_eq!(apply_why_of(&w), Some(("ai.plan.stale", Tone::Warn)));
    }

    #[test]
    fn reject_confirm_thresholds() {
        assert!(!reject_needs_confirm_of(1, 0, 0, false));
        assert!(!reject_needs_confirm_of(REJECT_CONFIRM_ITEMS - 1, 0, 0, false));
        assert!(reject_needs_confirm_of(REJECT_CONFIRM_ITEMS, 0, 0, false));
        // a small draft the person worked on still asks
        assert!(reject_needs_confirm_of(2, 1, 0, false));
        assert!(reject_needs_confirm_of(2, 0, 1, false));
        assert!(reject_needs_confirm_of(2, 0, 0, true));
    }

    #[test]
    fn small_reject_is_immediate_and_large_asks() {
        assert_eq!(reject_step(false, false, Ask::Reject), (false, true));
        assert_eq!(reject_step(false, true, Ask::Reject), (true, false));
    }

    #[test]
    fn confirm_discards_and_cancel_keeps() {
        assert_eq!(reject_step(true, true, Ask::Confirm), (false, true));
        assert_eq!(reject_step(true, true, Ask::Keep), (false, false));
    }

    #[test]
    fn reject_command_asks_on_a_large_draft() {
        assert!(!route_reject(true, 7), "large draft: not rejected at once");
        assert!(!take_ask(8), "another revision does not open the question");
        assert!(take_ask(7), "the card opens the question");
        assert!(!take_ask(7), "asked once");
    }

    #[test]
    fn reject_command_drops_a_small_draft_at_once() {
        assert!(route_reject(false, 3));
        assert!(!take_ask(3), "no question for a small draft");
        assert!(reject_command(None, 1), "no draft keeps the old behaviour");
    }

    #[test]
    fn esc_and_enter_never_reject() {
        assert_eq!(reject_step(true, true, Ask::Esc), (false, false));
        assert_eq!(reject_step(true, true, Ask::Enter), (false, false));
        assert_eq!(reject_step(false, true, Ask::Enter), (false, false));
    }

    #[test]
    fn reject_never_touches_the_model() {
        let src = include_str!("mod.rs");
        let arm = src
            .split("Action::Reject => {")
            .nth(1)
            .and_then(|s| s.split("Action::Cancel =>").next())
            .unwrap();
        assert!(!arm.contains("apply(") && !arm.contains("run_op"));
        // the card only emits Action::Reject; Apply is a different arm
        let code = include_str!("plan_card.rs").split("#[cfg(test)]").next().unwrap();
        assert!(code.contains("reject_step("));
    }

    #[test]
    fn confirm_texts_read_in_both_languages() {
        assert_eq!(en().plural("ai.plan.reject_ask", 1), "Reject 1 change?");
        assert_eq!(en().plural("ai.plan.reject_ask", 40), "Reject 40 changes?");
        // ru "few" (2) and "many" (40) forms differ; texts stay in the locale files
        let (few, many) = (ru().plural("ai.plan.reject_ask", 2), ru().plural("ai.plan.reject_ask", 40));
        assert!(few.contains('2') && many.contains("40"), "{few} / {many}");
        assert_ne!(few.replace('2', "#"), many.replace("40", "#"));
        assert_ne!(few, en().plural("ai.plan.reject_ask", 2));
        assert_ne!(ru().t("ai.plan.reject_keep"), en().t("ai.plan.reject_keep"));
    }

    #[test]
    fn qty_lines_follow_the_card_language() {
        let m = Units::Metric;
        // the thread speaks the other language: the catalog decides
        for (c, thread_ru, n, minus) in [
            (en(), true, "+1,234", "Glass \u{2212}1,000"),
            (ru(), false, "+1\u{a0}234", "Glass \u{2212}1\u{a0}000"),
        ] {
            crate::with_test_lang(thread_ru, || {
                let line = qty_line(&c, &q("doors", 0.0, 1234.0), m);
                assert!(line.ends_with(n), "{line}");
                let line = qty_line(&c, &q("area:Glass", 1000.25, 0.0), m);
                assert!(line.starts_with(minus), "{line}");
                let line = qty_line(&c, &q("doors", 7.0, 7.0), m);
                assert!(line.ends_with("+0"), "{line}");
            });
        }
    }

    #[test]
    fn counts_are_whole_numbers() {
        let line = qty_line(&cat(), &q("doors", 340.0, 343.0), Units::Metric);
        assert!(line.ends_with("+3"), "{line}");
    }
}
