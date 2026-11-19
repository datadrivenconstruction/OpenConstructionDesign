// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The draft of one external agent connection. Its writes are plan items like the chat's, kept
//! apart from the chat's card: the worker swaps them onto the scratch copy for each call, then
//! puts the chat's plan back. Nothing here reaches the live model; only the person's Apply does,
//! on the UI thread, as one undo step (`UiSession::apply_agent_draft`).

use super::mcp_core::{busy_because, reply};
use super::*;
use crate::ai::plan::{ElemDiff, PlanItem, VariantInfo};
use ddc_ai::mcp::tools as mcp_tools;
use ddc_model::{ElKey, RowKind};

/// Ended drafts remembered per connection, for `draft_status`.
const ENDED_KEPT: usize = 64;

/// What the agent reads with every queued write.
pub fn queued_note(number: u32) -> String {
    format!("queued in draft #{number}, pending user Apply")
}

/// One operation of a draft as the person reads it: the item and the elements it touches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentOp {
    pub item: u32,
    pub title: String,
    pub checked: bool,
    pub conflict: Option<String>,
    pub created: usize,
    pub changed: usize,
    pub deleted: usize,
}

impl AgentOp {
    pub fn elements(&self) -> usize {
        self.created + self.changed + self.deleted
    }
}

/// Per-item counts from the card's diff; elements no item claims stay out of every row.
pub fn ops_of(items: &[PlanItem], diff: &[ElemDiff]) -> Vec<AgentOp> {
    items
        .iter()
        .map(|i| {
            let n = |k: RowKind| {
                diff.iter()
                    .filter(|d| d.item == Some(i.id) && d.kind == k)
                    .count()
            };
            AgentOp {
                item: i.id,
                title: fmt::visible(&i.title),
                checked: i.checked,
                conflict: i.conflict.clone(),
                created: n(RowKind::Created),
                changed: n(RowKind::Changed),
                deleted: n(RowKind::Deleted),
            }
        })
        .collect()
}

/// What the UI shows and applies for one connection.
#[derive(Clone)]
pub struct AgentDraftView {
    /// `mcp:<client>`: the audit actor and the key of the draft.
    pub actor: String,
    /// The connection's own name, cleaned for display.
    pub name: String,
    pub number: u32,
    /// Bumped by every new picture of this draft: Apply names the one the person saw.
    pub rev: u64,
    pub ops: Vec<AgentOp>,
    pub plan: Arc<PlanView>,
}

impl AgentDraftView {
    pub fn base_head(&self) -> u64 {
        self.plan.compiled.base_head
    }

    pub fn blocked(&self) -> bool {
        self.plan.blocked() || self.plan.main_gate_blocks()
    }

    /// The checks that keep Apply off, so the person and the agent can name them.
    pub fn blockers(&self) -> Vec<&crate::ai::validate::PlanIssue> {
        self.plan
            .issues
            .iter()
            .chain(self.plan.main_issues.iter())
            .filter(|i| i.level == crate::ai::validate::IssueLevel::Block)
            .collect()
    }

    pub fn checked(&self) -> usize {
        self.ops.iter().filter(|o| o.checked).count()
    }

    /// Something to write and nothing that forbids it; staleness is the UI's to judge.
    pub fn appliable(&self) -> bool {
        !self.blocked() && !self.plan.compiled.steps.is_empty()
    }

    /// Every element the draft touches, for the outline in plan and 3D.
    pub fn keys(&self) -> Vec<ElKey> {
        self.plan.diff.iter().map(|d| d.key.clone()).collect()
    }
}

/// An open `changeset_begin` group inside one draft.
pub(super) struct Group {
    id: String,
    first_item: u32,
    title: Option<String>,
    variants: (Vec<VariantInfo>, u32),
    /// Asked with `atomic: true`: one failed write takes the whole group out.
    atomic: bool,
}

#[derive(Default)]
pub(super) struct AgentDraft {
    number: u32,
    plan: PlanState,
    group: Option<Group>,
    view: Option<Arc<AgentDraftView>>,
}

#[derive(Default)]
pub(super) struct AgentDrafts {
    open: BTreeMap<String, AgentDraft>,
    ended: BTreeMap<String, (u32, &'static str)>,
    next_number: u32,
    next_group: u32,
    rev: u64,
    /// The person's draft messages handled so far: the UI's Apply waits until it catches up.
    pub(super) ack: u64,
}

impl AgentDrafts {
    fn end(&mut self, actor: &str, number: u32, how: &'static str) {
        if number > 0 {
            self.ended.insert(actor.to_string(), (number, how));
        }
        while self.ended.len() > ENDED_KEPT {
            self.ended.pop_first();
        }
    }
}

fn no_group() -> ToolOutcome {
    ToolOutcome::error(reply(&[
        ("status", json!("no_changeset")),
        ("error", json!("no open changeset with this handle: begin a new one")),
        ("code", json!(ErrorCode::NotFound.as_str())),
    ]))
}

fn op_json(o: &AgentOp) -> J {
    json!({
        "item": o.item,
        "title": fmt::text(&o.title),
        "checked": o.checked,
        "conflict": o.conflict.as_deref().map(fmt::text),
        "created": o.created,
        "changed": o.changed,
        "deleted": o.deleted,
    })
}

/// The explicit flag on every read over MCP: whether it saw this connection's draft on top.
fn mark_view(out: ToolOutcome, draft: Option<u32>) -> ToolOutcome {
    if out.is_error {
        return out;
    }
    let seen = draft.is_some();
    let flag = format!(",\"draft_view\":{seen},\"draft\":{}", json!(draft));
    let text = match out.text.strip_prefix(mcp_tools::FENCE) {
        Some(rest) => format!("{}{flag}{rest}", mcp_tools::FENCE),
        None => reply(&[
            ("draft_view", json!(seen)),
            ("draft", json!(draft)),
            ("text", J::String(out.text.clone())),
        ]),
    };
    ToolOutcome { text, ..out }
}

impl Core {
    pub fn group_open(&self, actor: &str) -> bool {
        self.drafts.open.get(actor).is_some_and(|d| d.group.is_some())
    }

    /// The pictures the UI holds; sent after every change of any draft.
    fn send_drafts(&self) {
        let list: Vec<Arc<AgentDraftView>> = self
            .drafts
            .open
            .values()
            .filter_map(|d| d.view.clone())
            .collect();
        self.send(FromWorker::AgentDrafts {
            list,
            ack: self.drafts.ack,
        });
    }

    pub(super) fn number_of(&self, actor: &str) -> Option<u32> {
        self.drafts
            .open
            .get(actor)
            .map(|d| d.number)
            .filter(|n| *n > 0)
    }

    /// Runs `f` with this connection's draft on the copy, then the chat's plan comes back.
    /// `picture`: the card is drawn again afterwards and the UI hears of it.
    fn on_draft<R>(
        &mut self,
        actor: &str,
        picture: bool,
        f: impl FnOnce(&mut Core) -> R,
    ) -> Result<R, AiError> {
        let chat = self.stash_chat();
        let mut d = self.drafts.open.remove(actor).unwrap_or_default();
        let mut plan = std::mem::take(&mut d.plan);
        // one numbering for every plan of the session: item 7 is one item, whoever made it
        plan.next_item = plan.next_item.max(self.plan.next_item);
        plan.next_slot = plan.next_slot.max(self.plan.next_slot);
        self.plan = plan;
        let ready = self
            .rollback()
            .and_then(|()| self.ensure_baseline())
            .and_then(|()| {
                if self.plan.items.is_empty() {
                    return Ok(());
                }
                match self.rebase.take() {
                    Some(ch) if !ch.all => {
                        let conn = self.ed.model.sql();
                        let hit = crate::ai::conflict::conflicts(&self.plan.items, &ch, &|t, id| {
                            crate::ai::conflict::row_gone(conn, t, id)
                        });
                        for (id, c) in hit {
                            self.plan.held.insert(id, c.text());
                        }
                        plan::replay_checked(
                            &mut self.plan,
                            &mut self.ed,
                            &self.sets,
                            &self.gens,
                            &self.rules,
                            self.live_head,
                        )
                    }
                    _ => plan::replay(
                        &mut self.plan,
                        &mut self.ed,
                        &self.sets,
                        &self.gens,
                        &self.rules,
                    ),
                }
                .map_err(AiError::from)
            });
        let out = ready.map(|()| f(self));
        if !self.plan.items.is_empty() && d.number == 0 {
            self.drafts.next_number += 1;
            d.number = self.drafts.next_number;
        }
        if picture && out.is_ok() {
            d.view = None;
            if !self.plan.items.is_empty() {
                match self.build_view(true) {
                    Ok(v) => {
                        self.drafts.rev += 1;
                        let name = actor.strip_prefix(ACTOR_PREFIX).unwrap_or(actor);
                        d.view = Some(Arc::new(AgentDraftView {
                            actor: actor.to_string(),
                            name: fmt::visible(name),
                            number: d.number,
                            rev: self.drafts.rev,
                            ops: ops_of(&v.items, &v.diff),
                            plan: Arc::new(v),
                        }));
                    }
                    Err(e) => self.send(FromWorker::Failed(e)),
                }
            }
        }
        let counters = (self.plan.next_item, self.plan.next_slot);
        d.plan = std::mem::take(&mut self.plan);
        (self.plan.next_item, self.plan.next_slot) = counters;
        if !d.plan.items.is_empty() || d.group.is_some() {
            self.drafts.open.insert(actor.to_string(), d);
        }
        self.quiet = false;
        self.restore_chat_plan(chat);
        if picture {
            self.send_drafts();
        }
        out
    }

    /// A catalog write of an agent: an item of its own draft, never a step on the live model.
    pub(super) fn draft_write(
        &mut self,
        spec: &catalog::ToolSpec,
        args: &J,
        actor: &str,
    ) -> ToolOutcome {
        let res = self.on_draft(actor, true, |c| {
            let before = c.plan.next_item;
            let cancel = c.stop.clone();
            let cx = CallCx {
                mode: Mode::Propose,
                cancel: &cancel,
                actor,
                lang: c.lang,
            };
            let out = c.call_tool(spec.name, args, &cx, true);
            let ids: Vec<u32> = c
                .plan
                .items
                .iter()
                .filter(|i| i.id > before)
                .map(|i| i.id)
                .collect();
            (out, ids)
        });
        match res {
            Err(e) => crate::ai::refusals::outcome(e),
            Ok((out, ids)) if out.is_error || ids.is_empty() => out,
            Ok((out, ids)) => self.queued(actor, &ids, out, None),
        }
    }

    fn queued(&self, actor: &str, ids: &[u32], out: ToolOutcome, receipt: Option<J>) -> ToolOutcome {
        let number = self.number_of(actor).unwrap_or(0);
        let ops = self.drafts.open.get(actor).map_or(0, |d| d.plan.items.len());
        let text = reply(&[
            ("status", json!("pending_user")),
            ("queued", json!(true)),
            ("draft", json!(number)),
            ("note", json!(queued_note(number))),
            ("items", json!(ids)),
            ("ops", json!(ops)),
            ("result", J::String(out.text.clone())),
            ("receipt", receipt.unwrap_or(J::Null)),
        ]);
        ToolOutcome { text, ..out }
    }

    /// Reads see the live model with this connection's draft on top, so steps can chain.
    pub(super) fn draft_read(&mut self, name: &str, args: &J, actor: &str) -> ToolOutcome {
        let draft = self.number_of(actor);
        let read = |c: &mut Core| {
            let cancel = c.stop.clone();
            let cx = CallCx {
                mode: Mode::ReadOnly,
                cancel: &cancel,
                actor,
                lang: c.lang,
            };
            c.call_tool(name, args, &cx, true)
        };
        // the chat's card is no agent's business: with one on the copy, the copy is swapped
        let out = if draft.is_some() || !self.plan.items.is_empty() {
            match self.on_draft(actor, false, read) {
                Ok(o) => o,
                Err(e) => crate::ai::refusals::outcome(e),
            }
        } else {
            read(self)
        };
        mark_view(out, draft)
    }

    pub(super) fn draft_status(&self, actor: &str) -> ToolOutcome {
        let d = self.drafts.open.get(actor);
        let view = d.and_then(|d| d.view.as_ref());
        let ops: Vec<J> = view.map_or(vec![], |v| v.ops.iter().map(op_json).collect());
        let stale = view.is_some_and(|v| v.base_head() < self.live_head);
        let last = self
            .drafts
            .ended
            .get(actor)
            .map_or(J::Null, |(n, how)| json!({"draft": n, "state": how}));
        let state = if ops.is_empty() { "none" } else { "pending_user" };
        ToolOutcome::ok(reply(&[
            ("draft", json!(self.number_of(actor))),
            ("state", json!(state)),
            ("ops", J::Array(ops)),
            ("stale", json!(stale)),
            (
                mcp_tools::HANDLE_ARG,
                json!(d.and_then(|d| d.group.as_ref()).map(|g| g.id.clone())),
            ),
            ("last", last),
            ("note", json!("only the person applies a draft")),
        ]))
    }

    /// The agent's `request_apply`: the card goes before the person, who alone applies it.
    /// The answer names every check that keeps Apply off, so the agent can fix its writes.
    pub(super) fn request_apply(&mut self, actor: &str) -> ToolOutcome {
        if self.group_open(actor) {
            return busy_because("changeset_open");
        }
        let Some(v) = self.drafts.open.get(actor).and_then(|d| d.view.clone()) else {
            return ToolOutcome::error(reply(&[
                ("status", json!("none")),
                ("error", json!("your draft is empty: nothing to apply")),
                ("code", json!(ErrorCode::NotFound.as_str())),
            ]));
        };
        let blocked: Vec<J> = v
            .blockers()
            .iter()
            .map(|i| json!({"check": i.text_key, "item": i.item, "args": i.args}))
            .collect();
        let stale = v.base_head() < self.live_head;
        let entry = self.entry(actor, "plan", Some(format!("draft={}", v.number)), None, "apply_asked");
        self.send(FromWorker::Audit(entry));
        self.send(FromWorker::AgentApplyAsked {
            actor: actor.to_string(),
        });
        let note = if !blocked.is_empty() {
            "a check blocks Apply: fix or drop the named operations, then ask again"
        } else if stale {
            "the model changed since: the person re-checks the draft before Apply"
        } else {
            "the card is before the person; only their Apply writes the model"
        };
        ToolOutcome::ok(reply(&[
            ("status", json!("awaiting_user")),
            ("draft", json!(v.number)),
            ("ops", json!(v.ops.len())),
            ("blocked", J::Array(blocked)),
            ("stale", json!(stale)),
            ("note", json!(note)),
        ]))
    }

    /// The agent's own `discard_draft` or the person's Discard: the copy never held it.
    pub(super) fn discard_draft(&mut self, actor: &str, by: &str) -> ToolOutcome {
        let Some(d) = self.drafts.open.remove(actor) else {
            // the person's Discard waits for this ack, even when the agent was faster
            self.send_drafts();
            return ToolOutcome::ok(reply(&[("status", json!("none"))]));
        };
        self.drafts.end(actor, d.number, "discarded");
        let plan = Some(format!("draft={}", d.number));
        let entry = self.entry(actor, "reject", plan, None, by);
        self.send(FromWorker::Audit(entry));
        self.send_drafts();
        ToolOutcome::ok(reply(&[
            ("status", json!("discarded")),
            ("draft", json!(d.number)),
        ]))
    }

    /// The person's Apply of draft `number` went in: the live model holds it now.
    pub(super) fn draft_applied(&mut self, actor: &str, number: u32) {
        if self.drafts.open.get(actor).is_some_and(|d| d.number == number) {
            self.drafts.open.remove(actor);
            self.drafts.end(actor, number, "applied");
        }
        self.send_drafts();
    }

    /// Compiled again on the copy as the live model is now; conflicts land on their items.
    pub(super) fn draft_recheck(&mut self, actor: &str) {
        self.drafts.ack += 1;
        if !self.drafts.open.contains_key(actor) {
            self.send_drafts();
            return;
        }
        if let Err(e) = self.on_draft(actor, true, |_| ()) {
            self.send(FromWorker::Failed(e));
            self.send_drafts();
        }
    }

    /// The person's ticks: Apply takes only the ticked operations.
    pub(super) fn draft_checked(&mut self, actor: &str, list: &[(u32, bool)]) {
        self.drafts.ack += 1;
        if !self.drafts.open.contains_key(actor) {
            self.send_drafts();
            return;
        }
        let res = self.on_draft(actor, true, |c| {
            for (id, on) in list {
                if let Some(i) = c.plan.items.iter_mut().find(|i| i.id == *id) {
                    i.checked = *on;
                }
            }
            c.replan()
        });
        if let Err(e) = res.and_then(|r| r) {
            self.send(FromWorker::Failed(e));
            self.send_drafts();
        }
    }

    pub(super) fn group_begin(&mut self, actor: &str, args: &J) -> ToolOutcome {
        if self.group_open(actor) {
            return busy_because("changeset_open");
        }
        self.drafts.next_group += 1;
        let id = format!("cs{}", self.drafts.next_group);
        let title = args["title"].as_str().map(fmt::text);
        let chat_next = self.plan.next_item;
        let d = self.drafts.open.entry(actor.to_string()).or_default();
        d.group = Some(Group {
            id: id.clone(),
            first_item: d.plan.next_item.max(chat_next),
            title: title.clone(),
            variants: (d.plan.variants.clone(), d.plan.variant),
            atomic: args["atomic"].as_bool().unwrap_or(false),
        });
        let entry = self.entry(actor, "plan", Some(id.clone()), title, "open");
        self.send(FromWorker::Audit(entry));
        let atomic = args["atomic"].as_bool().unwrap_or(false);
        ToolOutcome::ok(reply(&[
            ("status", json!("open")),
            (mcp_tools::HANDLE_ARG, json!(id)),
            ("atomic", json!(atomic)),
        ]))
    }

    fn own_group(&self, actor: &str, handle: &str) -> bool {
        self.drafts
            .open
            .get(actor)
            .and_then(|d| d.group.as_ref())
            .is_some_and(|g| g.id == handle)
    }

    /// Items a failed call may have left behind (ids above `before`) leave the draft.
    fn drop_items_after(&mut self, actor: &str, before: u32) {
        let left = self
            .drafts
            .open
            .get(actor)
            .is_some_and(|d| d.plan.items.iter().any(|i| i.id > before));
        if !left {
            return;
        }
        let res = self.on_draft(actor, true, move |c| {
            c.plan.items.retain(|i| i.id <= before);
            c.plan.slot_owner.retain(|_, owner| *owner <= before);
            if c.plan.items.is_empty() {
                return Ok(());
            }
            c.replan()
        });
        if let Err(e) = res.and_then(|r| r) {
            self.send(FromWorker::Failed(e));
        }
    }

    /// The group's items leave the draft again; the rest of the draft stays.
    fn cut_group(&mut self, actor: &str) -> Option<Group> {
        let g = self.drafts.open.get_mut(actor)?.group.take()?;
        let first = g.first_item;
        let variants = g.variants.clone();
        let res = self.on_draft(actor, true, move |c| {
            c.plan.items.retain(|i| i.id <= first);
            c.plan.slot_owner.retain(|_, owner| *owner <= first);
            (c.plan.variants, c.plan.variant) = variants;
            if c.plan.items.is_empty() {
                return Ok(());
            }
            c.replan()
        });
        if let Err(e) = res.and_then(|r| r) {
            self.send(FromWorker::Failed(e));
        }
        Some(g)
    }

    fn group_rolled_back(&mut self, actor: &str, e: CmdError) -> ToolOutcome {
        if let Some(g) = self.cut_group(actor) {
            let entry = self.entry(actor, "reject", Some(g.id), g.title, "rolled_back");
            self.send(FromWorker::Audit(entry));
        }
        // the group's own failure keeps its code: the agent learns why, not only that
        ToolOutcome::error(reply(&[
            ("status", json!("rolled_back")),
            ("error", json!(fmt::clip(&e.message, 1000))),
            ("code", json!(e.code.as_str())),
        ]))
    }

    fn group_atomic(&self, actor: &str) -> bool {
        self.drafts
            .open
            .get(actor)
            .and_then(|d| d.group.as_ref())
            .is_some_and(|g| g.atomic)
    }

    /// A failed write in a group: an atomic group goes back whole; otherwise only this call
    /// failed, it left nothing, and the earlier writes and the open group stay.
    fn group_op_failed(&mut self, actor: &str, tool: &str, e: CmdError) -> ToolOutcome {
        if self.group_atomic(actor) {
            return self.group_rolled_back(actor, e);
        }
        let ops = self.drafts.open.get(actor).map_or(0, |d| d.plan.items.len());
        ToolOutcome::error(reply(&[
            ("status", json!("op_failed")),
            ("tool", json!(tool)),
            ("error", json!(fmt::clip(&e.message, 1000))),
            ("code", json!(e.code.as_str())),
            ("kept_ops", json!(ops)),
            ("note", json!("this call wrote nothing; earlier writes stay and the changeset stays open")),
        ]))
    }

    /// A catalog call that names a group: it must be this agent's open one.
    pub(super) fn group_call(
        &mut self,
        spec: &catalog::ToolSpec,
        args: &J,
        actor: &str,
        handle: &str,
    ) -> ToolOutcome {
        if !self.own_group(actor, handle) {
            return no_group();
        }
        let is_read = matches!(spec.access, Access::Read);
        if let Err(e) = catalog::validate(spec, args) {
            return if is_read {
                ToolOutcome::error(e)
            } else {
                self.group_op_failed(actor, spec.name, CmdError::from_text(&e))
            };
        }
        if is_read {
            return self.draft_read(spec.name, args, actor);
        }
        let before = self
            .drafts
            .open
            .get(actor)
            .and_then(|d| d.plan.items.iter().map(|i| i.id).max())
            .unwrap_or(0);
        let out = self.draft_write(spec, args, actor);
        if out.is_error {
            let e = ddc_ai::registry::error_of(&out)
                .unwrap_or_else(|| CmdError::from_text(&out.text));
            self.drop_items_after(actor, before);
            return self.group_op_failed(actor, spec.name, e);
        }
        out
    }

    pub(super) fn group_commit(&mut self, actor: &str, args: &J) -> ToolOutcome {
        let handle = args[mcp_tools::HANDLE_ARG].as_str().unwrap_or_default();
        if !self.own_group(actor, handle) {
            return no_group();
        }
        if let Some(v) = args["variant"].as_u64() {
            let v = u32::try_from(v).unwrap_or(u32::MAX);
            let res = self.on_draft(actor, true, |c| {
                if !c.plan.variants.iter().any(|x| x.id == v) {
                    return Err(AiError::Internal("unknown variant".to_string()));
                }
                c.set_variant(v);
                c.replan()
            });
            if let Err(e) = res.and_then(|r| r) {
                return self.group_rolled_back(actor, CmdError::from_text(&e.to_string()));
            }
        }
        let Some(g) = self
            .drafts
            .open
            .get_mut(actor)
            .and_then(|d| d.group.take())
        else {
            return no_group();
        };
        let entry = self.entry(actor, "plan", Some(g.id.clone()), g.title, "commit");
        self.send(FromWorker::Audit(entry));
        let receipt = self.group_receipt(actor, g.first_item);
        let ids: Vec<u32> = self.drafts.open.get(actor).map_or(vec![], |d| {
            d.plan
                .items
                .iter()
                .filter(|i| i.id > g.first_item)
                .map(|i| i.id)
                .collect()
        });
        if self.drafts.open.get(actor).is_some_and(|d| d.plan.items.is_empty()) {
            self.drafts.open.remove(actor);
        }
        if ids.is_empty() {
            return ToolOutcome::ok(reply(&[("status", json!("no_change"))]));
        }
        self.queued(actor, &ids, ToolOutcome::ok(String::new()), receipt)
    }

    /// What the group will do once the person applies it, from the card's own diff.
    fn group_receipt(&self, actor: &str, first_item: u32) -> Option<J> {
        let v = self.drafts.open.get(actor)?.view.clone()?;
        let diff: Vec<ElemDiff> = v
            .plan
            .diff
            .iter()
            .filter(|e| e.item.is_some_and(|i| i > first_item))
            .cloned()
            .collect();
        let issues: Vec<_> = v.plan.issues.iter().chain(&v.plan.main_issues).cloned().collect();
        let label = format!("AI draft #{}", v.number);
        let r = crate::ai::receipt::WriteReceipt::build(&diff, &v.plan.qty, &issues, v.base_head(), false, &label);
        Some(r.to_json())
    }

    pub(super) fn group_abort(&mut self, actor: &str, args: &J) -> ToolOutcome {
        let handle = args[mcp_tools::HANDLE_ARG].as_str().unwrap_or_default();
        if !self.own_group(actor, handle) {
            return no_group();
        }
        if let Some(g) = self.cut_group(actor) {
            let entry = self.entry(actor, "reject", Some(g.id), g.title, "aborted");
            self.send(FromWorker::Audit(entry));
        }
        ToolOutcome::ok(reply(&[("status", json!("aborted"))]))
    }

    /// Access was revoked: open groups go, the drafts stay for the person to decide.
    pub(super) fn drop_groups(&mut self, reason: &str) {
        let open: Vec<String> = self
            .drafts
            .open
            .iter()
            .filter(|(_, d)| d.group.is_some())
            .map(|(a, _)| a.clone())
            .collect();
        for actor in open {
            if let Some(g) = self.cut_group(&actor) {
                let entry = self.entry(&actor, "reject", Some(g.id), g.title, reason);
                self.send(FromWorker::Audit(entry));
            }
        }
    }
}

#[cfg(test)]
#[path = "agent_draft_tests.rs"]
mod tests;
