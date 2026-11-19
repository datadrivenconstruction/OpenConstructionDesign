//! The worker's side of an external agent: every call runs through the same handlers and the
//! same validation as the chat, and every write lands in the connection's own draft
//! (`agent_draft.rs`). Only the person's Apply takes a draft into the live model.

use super::*;
use ddc_ai::mcp::tools as mcp_tools;
use ddc_ai::registry::{self, Caller, Surface};

pub const ACTOR_PREFIX: &str = "mcp:";
/// Older projects stored "auto" here; it is read as a draft and never rewritten on open.
pub const POLICY_KEY: &str = "mcp.policy";
/// Said to the agent beside every `pending_user`: nothing reached the model yet.
pub const AWAITING_APPLY: &str = "draft created, awaiting user Apply";

const ITEM_WAITING: &str = "waiting";
const ITEM_APPLIED: &str = "applied";
const ITEM_REJECTED: &str = "rejected";
const ITEM_STALE: &str = "stale";

/// The fence stays the first field; `json!` would sort it away.
pub(super) fn reply(fields: &[(&str, J)]) -> String {
    let mut s = String::from(mcp_tools::FENCE);
    for (k, v) in fields {
        s.push(',');
        s.push_str(&J::String((*k).to_string()).to_string());
        s.push(':');
        s.push_str(&v.to_string());
    }
    s.push('}');
    s
}

/// A call that did not run: the status the agent knows, a message and the registry code.
fn not_run(status: &str, reason: &str, code: ErrorCode, message: &str) -> ToolOutcome {
    ToolOutcome::error(reply(&[
        ("status", json!(status)),
        ("reason", json!(reason)),
        ("error", json!(message)),
        ("code", json!(code.as_str())),
    ]))
}

/// The agent's own open group holds its other writes back until it is committed or aborted.
pub(super) fn busy_because(reason: &str) -> ToolOutcome {
    not_run(
        "busy",
        reason,
        ErrorCode::Busy,
        "a changeset is open: pass its handle, or commit or abort it first",
    )
}

impl Core {
    pub(super) fn on_mcp(&mut self, call: McpCall) {
        let out = if call.gate.start() {
            let actor = format!("{ACTOR_PREFIX}{}", fmt::text(&call.client));
            let out = self.agent_outcome(&call.name, &call.args, &actor, Caller::Agent);
            registry::ensure_coded_in(out, call.lang)
        } else {
            not_run(
                "refused",
                "gave_up_or_revoked",
                ErrorCode::NotAllowed,
                "the call was not run: the client stopped waiting or its access was revoked",
            )
        };
        let _ = call.reply.send(out);
        self.send(FromWorker::McpDone);
    }

    /// An agent's or the command line's call. Each caller works its own draft; no answer here
    /// ever means "written to the model". Every call passes the registry gate first.
    pub(crate) fn agent_outcome(
        &mut self,
        name: &str,
        raw: &J,
        actor: &str,
        caller: Caller,
    ) -> ToolOutcome {
        // an agent proposes into its own draft; only the person applies
        let ceiling = catalog::ceiling(
            catalog::Client::Mcp,
            Mode::Propose,
            catalog::TierPolicy::default(),
            self.target,
        );
        let agent_only = registry::find(name).is_some_and(|c| c.surface == Surface::AgentOnly);
        // the group commands take the handle as their own argument
        let (handle, args) = if agent_only {
            (None, raw.clone())
        } else {
            mcp_tools::split_handle(raw)
        };
        // agents may ask for images whatever the chat profile says
        let cmd = match registry::gate_as(name, &args, caller, ceiling, true) {
            Ok(c) => c,
            // a bad write inside a group takes the whole group back: group_call does that
            Err(e) if e.code == ErrorCode::InvalidArgs && handle.is_some() => {
                match registry::find(name) {
                    Some(c) => c,
                    None => return e.outcome(),
                }
            }
            Err(e) => return e.outcome(),
        };
        if agent_only {
            return match name {
                mcp_tools::CHANGESET_BEGIN => self.group_begin(actor, &args),
                mcp_tools::CHANGESET_COMMIT => self.group_commit(actor, &args),
                mcp_tools::CHANGESET_ABORT => self.group_abort(actor, &args),
                mcp_tools::DRAFT_STATUS => self.draft_status(actor),
                mcp_tools::DISCARD_DRAFT => self.discard_draft(actor, "agent"),
                mcp_tools::REQUEST_APPLY => self.request_apply(actor),
                mcp_tools::OPS_DRY_RUN => self.ops_dry_run(actor, &args),
                mcp_tools::OPS_PROPOSE => self.ops_propose(actor, &args),
                mcp_tools::PROJECT_INFO => self.project_info(),
                mcp_tools::EXPORT_IFC => self.export_ifc_tool(&args),
                mcp_tools::PLAN_STATUS => self.plan_status(actor),
                other => CmdError::from_text(&format!("unknown tool: {other}")).outcome(),
            };
        }
        let spec = cmd.spec;
        // a stale handle must never turn a call of a dead group into a plain write
        if let Some(h) = handle {
            return self.group_call(spec, &args, actor, &h);
        }
        if self.group_open(actor) {
            return busy_because("changeset_open");
        }
        if spec.name == "family_draft" {
            return self.family_draft_agent(&args, actor);
        }
        if matches!(spec.access, Access::Read) {
            return self.draft_read(spec.name, &args, actor);
        }
        self.draft_write(spec, &args, actor)
    }

    /// An agent's family text is checked and queued for the person; it adds no plan item, so the
    /// agent's draft is not rolled back or replayed for it, and nothing opens by itself.
    fn family_draft_agent(&mut self, args: &J, actor: &str) -> ToolOutcome {
        let cancel = self.stop.clone();
        let cx = ddc_ai::CallCx {
            mode: ddc_ai::Mode::Propose,
            cancel: &cancel,
            actor,
            lang: self.lang,
        };
        self.fam_from_agent = true;
        let res = crate::ai::fam_tools::family_draft(self, args, &cx);
        self.fam_from_agent = false;
        match res {
            Ok(done) => ToolOutcome::ok(done.reply.text),
            Err(e) => ddc_ai::registry::CmdError::from_text(&e).outcome(),
        }
    }

    /// The chat's plan leaves the copy; its numbering stays, so ids never repeat.
    pub(super) fn stash_chat(&mut self) -> PlanState {
        let chat = std::mem::take(&mut self.plan);
        self.plan.next_item = chat.next_item;
        self.plan.next_slot = chat.next_slot;
        self.quiet = true;
        chat
    }

    /// The person's Apply or Reject decides what became of the agent's waiting items.
    pub(super) fn settle_items(&mut self, applied: bool) {
        for (id, state) in self.mcp_items.iter_mut().filter(|e| *e.1 == ITEM_WAITING) {
            let taken = applied && self.plan.items.iter().any(|i| i.id == *id && i.checked);
            *state = if taken { ITEM_APPLIED } else { ITEM_REJECTED };
        }
        self.waiting = None;
    }

    /// Time until the oldest waiting agent change on the chat card counts as rejected.
    pub(super) fn confirm_time_left(&self) -> Option<Duration> {
        let (since, _) = self.waiting.as_ref()?;
        Some(self.confirm_ttl.saturating_sub(since.elapsed()))
    }

    /// Nobody pressed Apply in time: the agent's waiting items leave the card, as on Reject.
    pub(super) fn expire_waiting(&mut self) {
        let Some((_, actor)) = self.waiting.take() else {
            return;
        };
        let gone: Vec<u32> = self
            .mcp_items
            .iter()
            .filter(|e| *e.1 == ITEM_WAITING)
            .map(|e| *e.0)
            .collect();
        for id in &gone {
            self.mcp_items.insert(*id, ITEM_REJECTED);
        }
        self.plan.items.retain(|i| !gone.contains(&i.id));
        self.plan.slot_owner.retain(|_, owner| !gone.contains(owner));
        let entry = self.entry(&actor, "reject", None, None, "timeout");
        self.send(FromWorker::Audit(entry));
        self.refresh();
    }

    fn plan_status(&self, actor: &str) -> ToolOutcome {
        let find = |id: u32| self.plan.items.iter().find(|i| i.id == id);
        let items: Vec<J> = self
            .mcp_items
            .iter()
            .map(|(id, state)| {
                let state = match find(*id) {
                    _ if *state != ITEM_WAITING => *state,
                    None => ITEM_REJECTED,
                    Some(i) if i.conflict.is_some() => ITEM_STALE,
                    Some(_) => ITEM_WAITING,
                };
                json!({"item": id, "state": state})
            })
            .collect();
        ToolOutcome::ok(reply(&[
            ("items", J::Array(items)),
            ("proposals", self.proposals.status_json()),
            ("draft", json!(self.number_of(actor))),
            ("note", json!("draft_status tells what waits in your draft")),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_starts_with_the_fence_whatever_the_fields_hold() {
        let text = reply(&[("status", json!("x")), ("result", json!("\"}"))]);
        assert!(text.starts_with(mcp_tools::FENCE));
        let v: J = serde_json::from_str(&text).unwrap();
        assert_eq!(v["result"], "\"}");
    }
}
