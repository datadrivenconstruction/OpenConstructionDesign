// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "External agent changes": one card per agent connection. Its writes wait here, apart from the
//! chat's card; Apply takes the ticked operations into the model as one undo step
//! (`UiSession::apply_agent_draft`), Discard drops them, and a stale draft asks for a re-check.

use super::kit::{self, space, Button};
use super::plan_card::{apply_armed, chip, draft_stale, reason_note, reject_confirm, reject_step};
use super::plan_card::{Ask, Tone};
use super::{Action, Cx, Facts};
use crate::ai::session::{AgentDraftView, AgentOp};
use ddc_model::{ElKey, RowKind};
use ddc_ui::theme::Role;
use egui::{self, RichText, Ui};

/// Why Apply is off, as a catalog key and a tone; None while it is armed. Stale wins: Apply
/// must never run a draft compiled against a model that has moved since.
pub(super) fn apply_why(
    d: &AgentDraftView,
    stale: bool,
    ready: bool,
) -> Option<(&'static str, Tone)> {
    if stale {
        Some(("agent_draft.card.stale", Tone::Warn))
    } else if d.blocked() {
        Some(("agent_draft.card.blocked", Tone::Warn))
    } else if d.checked() == 0 || !d.appliable() {
        Some(("agent_draft.card.none", Tone::Muted))
    } else if !ready {
        Some(("agent_draft.card.busy", Tone::Muted))
    } else {
        None
    }
}

/// Every element one operation touches, for the outline under the pointer.
fn op_keys(d: &AgentDraftView, item: u32) -> Vec<ElKey> {
    d.plan
        .diff
        .iter()
        .filter(|e| e.item == Some(item))
        .map(|e| e.key.clone())
        .collect()
}

fn ask_id() -> egui::Id {
    egui::Id::new("ag7.card.discard_ask")
}

fn op_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    d: &AgentDraftView,
    op: &AgentOp,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let n = op.elements().to_string();
    let r = ui.horizontal(|ui| {
        let mut on = op.checked;
        let label = if op.title.is_empty() {
            cat.tf("agent_draft.card.op", &[("n", op.item.to_string().as_str())])
        } else {
            op.title.clone()
        };
        if kit::checkbox(ui, th, &mut on, &label).changed() {
            acts.push(Action::SetAgentChecked {
                actor: d.actor.clone(),
                list: vec![(op.item, on)],
            });
        }
        kit::caption(ui, th, &cat.tf("agent_draft.card.elements", &[("n", n.as_str())]));
    });
    if r.response.hovered() {
        let keys = op_keys(d, op.item);
        if !keys.is_empty() {
            *hot = Some(keys);
        }
    }
    if let Some(c) = &op.conflict {
        let lang = ddc_ai::refusal::lang(ddc_ui::i18n::current().code());
        kit::warn_note(ui, th, &super::logic::conflict_text(c, lang));
    }
}

fn buttons(
    ui: &mut Ui,
    cx: &Cx<'_>,
    d: &AgentDraftView,
    ready: bool,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let stale = draft_stale(cx, d.base_head(), false);
    let seen = ask_id().with(("seen", d.number));
    let mut asking = ui.ctx().data(|m| m.get_temp::<u32>(ask_id())) == Some(d.number);
    if asking {
        let n = d.ops.len() as u64;
        let (a, go) = reject_confirm(ui, th, cat, n, &cat.t("agent_draft.card.discard.desc"), seen);
        asking = a;
        if go {
            acts.push(Action::DiscardAgentDraft(d.actor.clone()));
        }
    } else {
        ui.ctx().data_mut(|m| m.insert_temp(seen, false));
        let why_key = apply_why(d, stale, ready);
        let reason = why_key.map(|(k, tone)| (cat.t(k), tone));
        ui.horizontal_wrapped(|ui| {
            let some = d.checked() < d.ops.len();
            let label = if some {
                let n = d.checked().to_string();
                cat.tf("agent_draft.card.apply_some", &[("n", n.as_str())])
            } else {
                cat.t("agent_draft.card.apply")
            };
            let why = reason
                .as_ref()
                .map(|(t, _)| t.clone())
                .unwrap_or_else(|| cat.t("agent_draft.card.apply.desc"));
            if Button::primary(&label)
                .enabled(apply_armed(why_key))
                .tooltip(&why, "")
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::ApplyAgentDraft {
                    actor: d.actor.clone(),
                    rev: d.rev,
                });
            }
            // the outline of the draft is on the canvas while the pointer is on the button
            let look = Button::secondary(&cat.t("agent_draft.ag.preview"))
                .tooltip(&cat.t("agent_draft.ag.preview.desc"), "")
                .show(ui, th);
            if look.hovered() || look.has_focus() {
                *hot = Some(d.keys());
            }
            if stale
                && Button::secondary(&cat.t("agent_draft.card.recheck"))
                    .tooltip(&cat.t("agent_draft.card.recheck.desc"), "")
                    .show(ui, th)
                    .clicked()
            {
                acts.push(Action::RecheckAgentDraft(d.actor.clone()));
            }
            if Button::quiet(&cat.t("agent_draft.card.discard"))
                .tooltip(&cat.t("agent_draft.card.discard.desc"), "")
                .show(ui, th)
                .clicked()
            {
                let (a, go) = reject_step(false, !d.ops.is_empty(), Ask::Reject);
                asking = a;
                if go {
                    acts.push(Action::DiscardAgentDraft(d.actor.clone()));
                }
            }
        });
        if let Some((text, tone)) = &reason {
            reason_note(ui, th, text, *tone);
        }
        if !stale {
            // name each blocking check: "a check blocks" alone leaves the person guessing
            for issue in d.blockers() {
                kit::warn_note(ui, th, &super::logic::issue_text(cat, issue));
            }
        }
    }
    ui.ctx().data_mut(|m| {
        if asking {
            m.insert_temp(ask_id(), d.number);
        } else if m.get_temp::<u32>(ask_id()) == Some(d.number) {
            m.remove::<u32>(ask_id());
        }
    });
}

fn one(
    ui: &mut Ui,
    cx: &Cx<'_>,
    d: &AgentDraftView,
    ready: bool,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (th, cat) = (cx.th, cx.cat);
    kit::card(ui, th, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(cat.t("agent_draft.card.title"))
                    .font(th.font(Role::Label))
                    .color(th.c("text.secondary")),
            );
            let sum = |k: RowKind| {
                d.ops
                    .iter()
                    .map(|o| match k {
                        RowKind::Created => o.created,
                        RowKind::Deleted => o.deleted,
                        _ => o.changed,
                    })
                    .sum::<usize>()
            };
            for (k, tip) in [
                (RowKind::Created, "ai.plan.created"),
                (RowKind::Changed, "ai.plan.changed"),
                (RowKind::Deleted, "ai.plan.deleted"),
            ] {
                if let Some(r) = chip(ui, th, k, sum(k), &cat.t(tip)) {
                    if r.hovered() {
                        *hot = Some(d.keys());
                    }
                }
            }
        });
        let number = d.number.to_string();
        ui.label(
            RichText::new(cat.tf(
                "agent_draft.card.by",
                &[("name", d.name.as_str()), ("n", number.as_str())],
            ))
            .font(th.font(Role::BodyStrong))
            .color(th.c("text.primary")),
        );
        let ops = d.ops.len().to_string();
        kit::caption(ui, th, &cat.tf("agent_draft.card.ops", &[("n", ops.as_str())]));
        ui.add_space(space::XS);
        for op in &d.ops {
            ui.push_id(("ag7.op", op.item), |ui| op_row(ui, cx, d, op, acts, hot));
        }
        ui.add_space(space::XS);
        buttons(ui, cx, d, ready, acts, hot);
    });
}

/// Every pending external draft, oldest connection first.
pub fn cards(
    ui: &mut Ui,
    cx: &Cx<'_>,
    f: &Facts,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    for d in &f.agents {
        ui.add_space(space::SM);
        ui.push_id(("ag7.draft", d.actor.as_str()), |ui| {
            one(ui, cx, d, f.agent_ready, acts, hot)
        });
    }
}
