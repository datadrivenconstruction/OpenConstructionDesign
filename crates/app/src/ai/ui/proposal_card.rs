// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Cards of an agent's `ops_propose`: a draft preview read from the dry run (counts by kind, the
//! affected elements by name, their outline in the views) with Apply and Reject. Apply is the
//! only way such an envelope reaches the model (`UiSession::apply_proposal`).

use super::kit::{self, space, Button};
use super::plan_card::{
    apply_armed, chip, draft_stale, kind_color, reason_note, reject_confirm, reject_step, Ask, Tone,
};
use super::proposal_draft::{self, DraftRow};
use super::{Action, Cx, Facts};
use crate::ai::preview;
use crate::ai::session::{ProposalCard, ProposalState};
use ddc_model::{ElKey, RowKind};
use ddc_ui::theme::Role;
use egui::{self, Align2, RichText, Sense, Ui, Vec2};

const ROWS_SHOWN: f32 = 8.0;

/// One row per element: sign, human name; a live one lights on hover and is selected on click.
fn list(
    ui: &mut Ui,
    cx: &Cx<'_>,
    id: u32,
    rows: &[DraftRow],
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let h = th.d().row;
    let shown = (rows.len() as f32).min(ROWS_SHOWN) * h;
    egui::ScrollArea::vertical()
        .id_salt(("ddc.ai.proposal.rows", id))
        .max_height(shown)
        .auto_shrink([false, true])
        // drag-to-scroll takes the hover on press: the pressed row must keep its highlight
        .drag_to_scroll(false)
        .show_rows(ui, h, rows.len(), |ui, range| {
            for r in &rows[range] {
                let sense = if r.key.is_some() {
                    Sense::click()
                } else {
                    Sense::hover()
                };
                let (rect, resp) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), h), sense);
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let name = proposal_draft::human(cat, r);
                if let (true, Some(k)) = (resp.hovered(), &r.key) {
                    kit::row_highlight(ui, th, rect, false);
                    *hot = Some(vec![k.clone()]);
                }
                let p = ui.painter_at(rect);
                let font = th.font(Role::Label);
                let sign = p.text(
                    rect.left_center() + Vec2::new(space::XS, 0.0),
                    Align2::LEFT_CENTER,
                    preview::sign(r.kind),
                    font.clone(),
                    kind_color(ui, r.kind),
                );
                let ink = if r.key.is_some() {
                    "text.primary"
                } else {
                    "text.secondary"
                };
                p.text(
                    sign.right_center() + Vec2::new(space::XS, 0.0),
                    Align2::LEFT_CENTER,
                    &name,
                    font,
                    th.c(ink),
                );
                if let (true, Some(k)) = (resp.clicked(), &r.key) {
                    acts.push(Action::ShowDraft(vec![k.clone()]));
                }
                let tip = match (&r.key, r.kind) {
                    (Some(_), _) => format!("{name}\n{}", cat.t("ai.proposal.row.tip")),
                    (None, RowKind::Created) => {
                        format!("{name}\n{}", cat.t("ai.proposal.row.new_tip"))
                    }
                    (None, _) => name.clone(),
                };
                resp.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, r.key.is_some(), &name)
                });
                resp.on_hover_text(tip);
            }
        });
}

fn one(
    ui: &mut Ui,
    cx: &Cx<'_>,
    c: &ProposalCard,
    busy: bool,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let rows = proposal_draft::rows(&c.diff);
    let n = proposal_draft::counts(&rows);
    kit::card(ui, th, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(cat.t("ai.proposal.title"))
                    .font(th.font(Role::Label))
                    .color(th.c("text.secondary")),
            );
            for (k, num, tip) in [
                (RowKind::Created, n.created, "ai.plan.created"),
                (RowKind::Changed, n.changed, "ai.plan.changed"),
                (RowKind::Deleted, n.deleted, "ai.plan.deleted"),
            ] {
                if let Some(r) = chip(ui, th, k, num, &cat.t(tip)) {
                    let keys = proposal_draft::keys_of_kind(&rows, k);
                    if r.hovered() && !keys.is_empty() {
                        *hot = Some(keys.clone());
                    }
                    if r.clicked() && !keys.is_empty() {
                        acts.push(Action::ShowDraft(keys));
                    }
                }
            }
        });
        if !c.title.is_empty() {
            ui.label(
                RichText::new(&c.title)
                    .font(th.font(Role::BodyStrong))
                    .color(th.c("text.primary")),
            );
        }
        let ops = c.ops.to_string();
        kit::caption(ui, th, &cat.tf("ai.proposal.by", &[("actor", c.actor.as_str())]))
            .on_hover_text(cat.tf(
                "ai.proposal.meta",
                &[("actor", c.actor.as_str()), ("ops", ops.as_str())],
            ));
        if rows.is_empty() {
            kit::caption(ui, th, &cat.t("ai.proposal.empty"));
        } else {
            ui.add_space(space::XS);
            list(ui, cx, c.id, &rows, acts, hot);
        }
        ui.add_space(space::XS);
        let stale = draft_stale(cx, c.base_head, false);
        actions(ui, cx, c, rows.len(), busy, stale, acts);
    });
}

/// Why Apply is off, as a catalog key and a tone; None while it is armed. Stale wins, as on
/// the plan draft: Apply must never run what the card no longer shows.
fn apply_why(state: ProposalState, busy: bool, stale: bool) -> Option<(&'static str, Tone)> {
    if state == ProposalState::Applying {
        None
    } else if stale {
        Some(("ai.proposal.stale", Tone::Warn))
    } else if busy {
        Some(("ai.proposal.busy", Tone::Muted))
    } else {
        None
    }
}

/// An agent's proposal cannot be drafted again by a click, so any change asks before Reject;
/// a proposal that changes nothing has nothing to lose.
fn reject_needs_confirm(rows: usize) -> bool {
    rows > 0
}

/// Temp-data id of the one open Reject confirm; it holds the proposal id.
fn ask_id() -> egui::Id {
    egui::Id::new("ai.proposal.reject_ask")
}

/// Apply and Reject with the plan draft's labels, confirm, Esc/Enter and disabled reasons.
fn actions(
    ui: &mut Ui,
    cx: &Cx<'_>,
    c: &ProposalCard,
    rows: usize,
    busy: bool,
    stale: bool,
    acts: &mut Vec<Action>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let seen = ask_id().with(("seen", c.id));
    let mut asking = ui.ctx().data(|d| d.get_temp::<u32>(ask_id())) == Some(c.id);
    if asking {
        let (a, go) =
            reject_confirm(ui, th, cat, rows as u64, &cat.t("ai.proposal.reject.desc"), seen);
        asking = a;
        if go {
            acts.push(Action::RejectProposal(c.id));
        }
    } else {
        ui.ctx().data_mut(|d| d.insert_temp(seen, false));
        let applying = c.state == ProposalState::Applying;
        let why_key = apply_why(c.state, busy, stale);
        let reason = why_key.map(|(k, tone)| (cat.t(k), tone));
        ui.horizontal(|ui| {
            let label = cat.t(if applying {
                "ai.proposal.applying"
            } else {
                "ai.items.apply_none"
            });
            let why = reason
                .as_ref()
                .map(|(t, _)| t.clone())
                .unwrap_or_else(|| cat.t("ai.proposal.apply.desc"));
            if Button::primary(&label)
                .enabled(c.state == ProposalState::Waiting && apply_armed(why_key))
                .tooltip(&why, "")
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::ApplyProposal(c.id));
            }
            if Button::quiet(&cat.t("ai.plan.reject"))
                .enabled(!applying)
                .tooltip(&cat.t("ai.proposal.reject.desc"), "")
                .show(ui, th)
                .clicked()
            {
                let (a, go) = reject_step(false, reject_needs_confirm(rows), Ask::Reject);
                asking = a;
                if go {
                    acts.push(Action::RejectProposal(c.id));
                }
            }
        });
        if let Some((text, tone)) = &reason {
            reason_note(ui, th, text, *tone);
        }
    }
    ui.ctx().data_mut(|d| {
        if asking {
            d.insert_temp(ask_id(), c.id);
        } else if d.get_temp::<u32>(ask_id()) == Some(c.id) {
            d.remove::<u32>(ask_id());
        }
    });
}

/// Every open card, oldest first. A row or chip under the pointer sets `hot`; with none, the
/// views outline every open card's elements (`proposal_draft::open_keys`).
pub fn cards(
    ui: &mut Ui,
    cx: &Cx<'_>,
    f: &Facts,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let busy = f.proposal_applying.is_some();
    for c in &f.proposals {
        ui.add_space(space::SM);
        ui.push_id(("ddc.ai.proposal", c.id), |ui| one(ui, cx, c, busy, acts, hot));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use serde_json::json;
    use std::collections::HashSet;

    // named font weights bind from the pass after install: a bare first frame would panic
    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    /// One waiting card whose dry run was taken at `ed`'s current head.
    fn facts(ed: &Editor, diff: serde_json::Value) -> Facts {
        Facts {
            proposals: vec![ProposalCard {
                id: 7,
                actor: "mcp:agent".into(),
                title: "Tidy walls".into(),
                ops: 3,
                diff,
                state: ProposalState::Waiting,
                base_head: ed.model.change_head(),
            }],
            ..Facts::default()
        }
    }

    fn collect(s: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
        match s {
            egui::Shape::Text(t) => {
                let text = t.galley.text().to_string();
                if !text.trim().is_empty() {
                    out.push((text, t.galley.rect.translate(t.pos.to_vec2())));
                }
            }
            egui::Shape::Vec(v) => v.iter().for_each(|x| collect(x, out)),
            _ => {}
        }
    }

    struct Out {
        texts: Vec<(String, egui::Rect)>,
        acts: Vec<Action>,
        hot: Option<Vec<ElKey>>,
    }

    fn frame(
        ctx: &egui::Context,
        ed: &Editor,
        f: &Facts,
        lang: ddc_ui::Lang,
        events: Vec<egui::Event>,
    ) -> Out {
        let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let cat = ddc_ui::Catalog::new(lang);
        let mut acts = Vec::new();
        let mut hot = None;
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(420.0, 640.0),
            )),
            events,
            ..egui::RawInput::default()
        };
        let out = ctx.run(raw, |ctx| {
            let _ = th.apply(ctx);
            let cx = Cx {
                th: &th,
                cat: &cat,
                ed,
                view3d: false,
                palette_open: false,
            };
            egui::CentralPanel::default().show(ctx, |ui| cards(ui, &cx, f, &mut acts, &mut hot));
        });
        let mut texts = Vec::new();
        out.shapes.iter().for_each(|c| collect(&c.shape, &mut texts));
        Out { texts, acts, hot }
    }

    fn at(texts: &[(String, egui::Rect)], s: &str) -> egui::Pos2 {
        texts
            .iter()
            .find(|(t, _)| t == s)
            .unwrap_or_else(|| panic!("{s} not drawn: {texts:?}"))
            .1
            .center()
    }

    fn walls(ids: &[i64]) -> Vec<ElKey> {
        ids.iter().map(|i| ("wall".to_string(), *i)).collect()
    }

    #[test]
    fn three_changed_walls_read_as_one_chip_and_three_named_rows() {
        let ed = Editor::new_in_memory().unwrap();
        let head = ed.model.change_head();
        let f = facts(&ed, json!({"created": [], "copied": [], "deleted": [],
                             "changed": ["wall:1", "wall:2", "wall:3"]}));
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let ctx = font_ctx();
            let o = frame(&ctx, &ed, &f, lang, vec![]);
            let t: Vec<&str> = o.texts.iter().map(|(s, _)| s.as_str()).collect();
            assert!(t.contains(&"3"), "one chip with the true count: {t:?}");
            assert!(!t.contains(&"0"), "zero kinds are hidden: {t:?}");
            let word = cat.t("ai.proposal.el.wall");
            for i in 1..=3 {
                let name = format!("{word} {i}");
                assert!(t.contains(&name.as_str()), "{name} listed: {t:?}");
            }
            assert!(t.iter().all(|s| !s.contains("wall:")), "no raw refs: {t:?}");
            assert!(t.iter().all(|s| !s.contains("ai.proposal")), "no missing keys: {t:?}");
            assert!(o.acts.is_empty(), "drawing pushes nothing");
            assert!(o.hot.is_none(), "no pointer, no hover");
            assert_eq!(ed.model.change_head(), head, "the card never writes");
        }
    }

    #[test]
    fn hovering_the_chip_lights_its_three_walls_and_a_row_click_selects_one() {
        let ed = Editor::new_in_memory().unwrap();
        let head = ed.model.change_head();
        let f = facts(&ed, json!({"changed": ["wall:1", "wall:2", "wall:3"]}));
        let lang = ddc_ui::Lang::En;
        let cat = ddc_ui::Catalog::new(lang);
        let ctx = font_ctx();
        let first = frame(&ctx, &ed, &f, lang, vec![]);
        let chip_at = at(&first.texts, "3");
        let _ = frame(&ctx, &ed, &f, lang, vec![egui::Event::PointerMoved(chip_at)]);
        let o = frame(&ctx, &ed, &f, lang, vec![]);
        let lit: HashSet<ElKey> = o.hot.expect("the chip is hovered").into_iter().collect();
        assert_eq!(lit, walls(&[1, 2, 3]).into_iter().collect());

        let row_at = at(&o.texts, &format!("{} 2", cat.t("ai.proposal.el.wall")));
        let btn = |pressed| egui::Event::PointerButton {
            pos: row_at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = frame(&ctx, &ed, &f, lang, vec![egui::Event::PointerMoved(row_at)]);
        let o = frame(&ctx, &ed, &f, lang, vec![btn(true)]);
        assert_eq!(o.hot, Some(walls(&[2])), "the row lights its own element");
        let o = frame(&ctx, &ed, &f, lang, vec![btn(false)]);
        assert!(
            o.acts.iter().any(|a| matches!(a, Action::ShowDraft(k) if *k == walls(&[2]))),
            "a click selects and frames that element"
        );
        assert!(
            !o.acts
                .iter()
                .any(|a| matches!(a, Action::ApplyProposal(_) | Action::RejectProposal(_))),
            "selecting is not deciding"
        );
        assert_eq!(ed.model.change_head(), head, "hover and click never write");
    }

    #[test]
    fn an_empty_dry_run_says_so_and_still_offers_reject() {
        let ed = Editor::new_in_memory().unwrap();
        let f = facts(&ed, json!({"created": [], "changed": [], "deleted": []}));
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let ctx = font_ctx();
            let o = frame(&ctx, &ed, &f, lang, vec![]);
            let t: Vec<&str> = o.texts.iter().map(|(s, _)| s.as_str()).collect();
            let empty = cat.t("ai.proposal.empty");
            assert!(t.contains(&empty.as_str()), "{t:?}");
            let reject = cat.t("ai.plan.reject");
            assert!(t.contains(&reject.as_str()), "{t:?}");
            assert!(!t.contains(&"0"), "{t:?}");
            // nothing to lose: one click drops it, no confirm
            let o = click(&ctx, &ed, &f, lang, at(&o.texts, &reject));
            assert!(rejected(&o.acts), "{lang:?}");
        }
    }

    /// Hover, press, release at `pos`; the release frame's output.
    fn click(
        ctx: &egui::Context,
        ed: &Editor,
        f: &Facts,
        lang: ddc_ui::Lang,
        pos: egui::Pos2,
    ) -> Out {
        let btn = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = frame(ctx, ed, f, lang, vec![egui::Event::PointerMoved(pos)]);
        let _ = frame(ctx, ed, f, lang, vec![btn(true)]);
        frame(ctx, ed, f, lang, vec![btn(false)])
    }

    fn key(k: egui::Key) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: Some(k),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn rejected(acts: &[Action]) -> bool {
        acts.iter().any(|a| matches!(a, Action::RejectProposal(7)))
    }

    fn applied(acts: &[Action]) -> bool {
        acts.iter().any(|a| matches!(a, Action::ApplyProposal(7)))
    }

    fn drawn(o: &Out, s: &str) -> bool {
        o.texts.iter().any(|(t, _)| t == s)
    }

    fn three_walls(ed: &Editor) -> Facts {
        facts(ed, json!({"changed": ["wall:1", "wall:2", "wall:3"]}))
    }

    /// Clicks Reject once; returns the next frame, which shows the confirm.
    fn open_confirm(ctx: &egui::Context, ed: &Editor, f: &Facts, lang: ddc_ui::Lang) -> Out {
        let cat = ddc_ui::Catalog::new(lang);
        let o = frame(ctx, ed, f, lang, vec![]);
        let o = click(ctx, ed, f, lang, at(&o.texts, &cat.t("ai.plan.reject")));
        assert!(!rejected(&o.acts), "the first click only asks");
        frame(ctx, ed, f, lang, vec![])
    }

    #[test]
    fn reject_needs_two_clicks_and_leaves_the_model_untouched() {
        let ed = Editor::new_in_memory().unwrap();
        let head = ed.model.change_head();
        let f = three_walls(&ed);
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let ctx = font_ctx();
            let o = open_confirm(&ctx, &ed, &f, lang);
            let ask = cat.plural("ai.plan.reject_ask", 3);
            assert!(drawn(&o, &ask), "{ask} asked: {:?}", o.texts);
            assert!(drawn(&o, &cat.t("ai.plan.reject_keep")));
            assert!(!drawn(&o, &cat.t("ai.items.apply_none")), "Apply waits under the confirm");
            let o = click(&ctx, &ed, &f, lang, at(&o.texts, &cat.t("ai.plan.reject_confirm")));
            assert!(rejected(&o.acts), "the second click drops it ({lang:?})");
            assert!(!applied(&o.acts));
            assert_eq!(ed.model.change_head(), head, "Reject never writes");
        }
    }

    #[test]
    fn keep_esc_and_enter_close_the_confirm_and_keep_the_proposal() {
        let ed = Editor::new_in_memory().unwrap();
        let head = ed.model.change_head();
        let f = three_walls(&ed);
        let lang = ddc_ui::Lang::En;
        let cat = ddc_ui::Catalog::new(lang);
        let ask = cat.plural("ai.plan.reject_ask", 3);
        for k in [egui::Key::Escape, egui::Key::Enter] {
            let ctx = font_ctx();
            let _ = open_confirm(&ctx, &ed, &f, lang);
            let o = frame(&ctx, &ed, &f, lang, vec![key(k)]);
            assert!(o.acts.is_empty(), "{k:?} decides nothing: {:?}", o.acts.len());
            let o = frame(&ctx, &ed, &f, lang, vec![]);
            assert!(!drawn(&o, &ask), "{k:?} closes the confirm");
            assert!(drawn(&o, &cat.t("ai.items.apply_none")), "the card is back");
        }
        let ctx = font_ctx();
        let o = open_confirm(&ctx, &ed, &f, lang);
        let o = click(&ctx, &ed, &f, lang, at(&o.texts, &cat.t("ai.plan.reject_keep")));
        assert!(!rejected(&o.acts), "Keep keeps");
        let o = frame(&ctx, &ed, &f, lang, vec![]);
        assert!(!drawn(&o, &ask));
        assert_eq!(ed.model.change_head(), head);
    }

    #[test]
    fn labels_are_the_plan_drafts_in_both_languages() {
        let ed = Editor::new_in_memory().unwrap();
        let f = three_walls(&ed);
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let ctx = font_ctx();
            let o = frame(&ctx, &ed, &f, lang, vec![]);
            for k in ["ai.items.apply_none", "ai.plan.reject"] {
                let s = cat.t(k);
                assert_ne!(s, k, "{k} has a {lang:?} string");
                assert!(drawn(&o, &s), "{s} drawn: {:?}", o.texts);
            }
            let o = open_confirm(&ctx, &ed, &f, lang);
            for k in ["ai.plan.reject_keep", "ai.plan.reject_confirm"] {
                assert!(drawn(&o, &cat.t(k)), "{k} in the confirm ({lang:?})");
            }
            assert!(drawn(&o, &cat.t("ai.proposal.reject.desc")), "the hint says the model stays");
        }
        let code = include_str!("proposal_card.rs");
        let (body, _) = code.split_at(code.find("#[cfg(test)]").unwrap());
        for gone in ["\"ai.proposal.apply\"", "\"ai.proposal.reject\""] {
            assert!(!body.contains(gone), "{gone}: the card uses the plan draft's keys");
        }
    }

    #[test]
    fn apply_click_asks_for_the_one_step_run_and_writes_nothing_itself() {
        let ed = Editor::new_in_memory().unwrap();
        let head = ed.model.change_head();
        let f = three_walls(&ed);
        let lang = ddc_ui::Lang::En;
        let cat = ddc_ui::Catalog::new(lang);
        let ctx = font_ctx();
        let o = frame(&ctx, &ed, &f, lang, vec![]);
        let o = click(&ctx, &ed, &f, lang, at(&o.texts, &cat.t("ai.items.apply_none")));
        assert!(applied(&o.acts), "one click: Apply is the primary action");
        assert!(!rejected(&o.acts));
        // the run itself (one journal entry, one undo step) is mcp_ops_draft_tests' apply test
        assert_eq!(ed.model.change_head(), head, "the card never writes");
    }

    #[test]
    fn a_stale_proposal_greys_apply_with_the_reason_and_still_rejects() {
        let mut ed = Editor::new_in_memory().unwrap();
        let f = three_walls(&ed);
        ed.model.add_level("moved", 3.0).unwrap();
        let head = ed.model.change_head();
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let ctx = font_ctx();
            let o = frame(&ctx, &ed, &f, lang, vec![]);
            let why = cat.t("ai.proposal.stale");
            assert!(drawn(&o, &why), "the reason is shown: {:?}", o.texts);
            let o = click(&ctx, &ed, &f, lang, at(&o.texts, &cat.t("ai.items.apply_none")));
            assert!(!applied(&o.acts), "Apply is off while stale ({lang:?})");
            let o = open_confirm(&ctx, &ed, &f, lang);
            let o = click(&ctx, &ed, &f, lang, at(&o.texts, &cat.t("ai.plan.reject_confirm")));
            assert!(rejected(&o.acts), "a stale proposal can still be dropped");
            assert_eq!(ed.model.change_head(), head);
        }
    }

    #[test]
    fn apply_waits_while_another_change_is_applied_and_says_why() {
        let ed = Editor::new_in_memory().unwrap();
        let mut f = three_walls(&ed);
        f.proposal_applying = Some(3);
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let ctx = font_ctx();
            let o = frame(&ctx, &ed, &f, lang, vec![]);
            let why = cat.t("ai.proposal.busy");
            assert_ne!(why, "ai.proposal.busy");
            assert!(drawn(&o, &why), "{:?}", o.texts);
            let o = click(&ctx, &ed, &f, lang, at(&o.texts, &cat.t("ai.items.apply_none")));
            assert!(!applied(&o.acts), "no second apply while one runs");
        }
    }

    #[test]
    fn apply_reasons_follow_the_plan_drafts_order() {
        use ProposalState::{Applying, Waiting};
        assert_eq!(apply_why(Waiting, false, false), None);
        assert_eq!(apply_why(Waiting, true, true), Some(("ai.proposal.stale", Tone::Warn)));
        assert_eq!(apply_why(Waiting, true, false), Some(("ai.proposal.busy", Tone::Muted)));
        assert_eq!(apply_why(Applying, true, true), None, "its own run shows Applying");
        assert!(reject_needs_confirm(1) && !reject_needs_confirm(0));
    }
}
