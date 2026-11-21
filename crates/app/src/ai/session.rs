// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One assistant session: `Core` on the worker thread owns the scratch `Editor`; `UiSession` on the
//! UI thread is the only place that ever changes the live model, and only through `exec::apply`.

use super::critic_hook;
use super::fmt::{self, Reply};
use super::plan::{
    self, CompileCx, CompiledPlan, FindingRef, PlanOp, PlanState, PlanView, Targets,
};
use super::materials;
use super::preview;
use super::rows;
use super::sets::{Key, Sets};
use super::tools_read;
use super::tools_write;
use super::validate::{self, Baseline, IssueLevel, PlanIssue};
use super::AiError;
use crate::ai::ui::sent::SentEntry;
use crate::editor::{Editor, Sel};
use crate::units::Units;
use ddc_ai::catalog::{self, Access};
use ddc_ai::mcp::{McpCall, McpPolicy};
use ddc_ai::registry::{CmdError, ErrorCode};
use ddc_ai::{
    CallCx, CancelToken, Conversation, ConvoEvent, ImageData, Limits, Mode, Profile, Provider,
    ToolHost, ToolOutcome, TurnEnd, UsageTotals,
};
use ddc_gen::Generator;
use ddc_model::checks::{CheckRule, Finding, RuleDef};
use ddc_model::{AuditEntry, Model, PlanCopies};
use ddc_render::Aabb;
use serde_json::{json, Value as J};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread;
use web_time::{Duration, Instant};

const DIFF_LIMIT: usize = 200_000;
const SYNC_GAP: Duration = Duration::from_millis(500);
const SYNC_RETRY: Duration = Duration::from_secs(5);
pub const CONFIRM_TTL: Duration = Duration::from_secs(600);
const TEXT_KEPT: usize = 2000;
const FINDINGS_KEPT: u32 = 5000;
/// How long closing a session may hold the UI for its worker; past it the worker is left to end alone.
const JOIN_WAIT: Duration = Duration::from_secs(2);

mod agent_draft;
mod lifecycle;
mod mcp_core;
mod mcp_info;
mod mcp_ops;
#[doc(hidden)]
pub mod pure_exec;
mod merge_card;
pub use lifecycle::{sweep_on_start, sweep_stale_snapshots, SNAP_STALE_AGE};
/// `Display` of a provider failure, with the wait the server asked for when it named one.
pub fn provider_text(e: &ddc_ai::ProviderError) -> String {
    match e {
        ddc_ai::ProviderError::RateLimited {
            retry_after_s: Some(n),
        } => format!("rate limited (retry after {n} s)"),
        other => other.to_string(),
    }
}

/// The journal text of a person's Apply that carries an agent's items: who proposed.
pub fn apply_note(proposers: &[String]) -> Option<String> {
    if proposers.is_empty() {
        None
    } else {
        Some(format!("proposer=agent:{}; approver=user", proposers.join(",")))
    }
}

pub use agent_draft::{ops_of, queued_note, AgentDraftView, AgentOp};
pub use mcp_core::{ACTOR_PREFIX, AWAITING_APPLY, POLICY_KEY};
pub use mcp_ops::{
    join_runner, DryRun, EnvelopeRunner, Proposal, ProposalCard, ProposalState, Proposals, Rejection,
};

#[derive(Clone, Debug, Default)]
pub struct ViewCtx {
    pub view: String,
    pub level: i64,
    pub units: Units,
    pub selection: Vec<Sel>,
    pub bounds: Option<Aabb>,
    /// Chips the person took out of the context: the note of the turn leaves them out.
    pub hide_view: bool,
    pub hide_level: bool,
}

/// `Done`: the project lives in memory, so the UI already made the copy with `snapshot_to`.
#[derive(Clone, Debug)]
pub enum SourceCopy {
    File(PathBuf),
    Done(PathBuf),
}

pub struct SessionCfg {
    pub provider: Arc<dyn Provider>,
    pub profile: Profile,
    pub limits: Limits,
    pub session: String,
    pub allow_snapshots: Arc<AtomicBool>,
    pub extra_generators: Vec<&'static dyn Generator>,
    pub extra_rules: Vec<&'static dyn CheckRule>,
    pub keep_text: bool,
    /// How long an agent's change may wait for Apply before it counts as rejected.
    pub confirm_ttl: Duration,
    /// `Draft` only for a session on an option's own copy; the live project is `Main`.
    pub target: catalog::Target,
    /// A live edit that conflicts with plan items starts one turn of the chat to draft them
    /// again (`replan::auto_text`); off where a script plays the provider.
    pub auto_replan: bool,
}

pub enum ToWorker {
    Turn {
        text: String,
        view: ViewCtx,
        mode: Mode,
    },
    SetChecked(Vec<(u32, bool)>),
    SetVariant(u32),
    AllowLarge(bool),
    /// Images the person attached for comparison (a picture, or the first page of a PDF).
    SetReference(Vec<ddc_ai::ImageData>),
    /// The `point_ctx` block of the next turn (from `point::message` without the text); `None` clears it.
    SetPoint(Option<String>),
    /// Attached files and the requirements the person confirmed; replaces the last ones.
    SetAttachments(Vec<Arc<super::attach::AttachData>>, Vec<ddc_io::attach::facts::Fact>),
    /// The person corrected one drafted value on the card; only the draft changes.
    SetValue {
        item: u32,
        field: super::value_edit::Field,
        value: super::value_edit::Value,
    },
    /// The person looked at these agent items; ones that joined since stay unreviewed.
    Reviewed(Vec<u32>),
    /// `head` was read before the copy started, so an edit during the copy still reads as stale.
    Sync {
        source: SourceCopy,
        head: u64,
        /// Live rows changed since the worker's last head (`Model::plan_changes`); `None`:
        /// unknown, the whole plan is compiled again by intent.
        changes: Option<ddc_model::Changes>,
    },
    /// `stamped`: the value sources the Apply recorded on live, (cells, source); the copy
    /// records the same, so it stays the exact base of the next draft.
    Applied {
        head: u64,
        op_seq: i64,
        stamped: Option<(Vec<(String, i64, String)>, ddc_model::ValueSrc)>,
    },
    View(ViewCtx),
    /// Every write of the call lands in the connection's own draft, never on the live model.
    Mcp {
        call: McpCall,
    },
    /// Access was revoked: open groups are dropped; the drafts stay for the person.
    McpCut,
    /// The person's Discard of a connection's draft.
    AgentDiscard(String),
    /// The model moved since the draft was drawn: compile it again on the fresh copy.
    AgentRecheck(String),
    /// Another draft went into the live model: this one is checked against the rows it
    /// changed (`changes` since this draft's base); only the items it touched conflict.
    AgentRebase {
        actor: String,
        changes: ddc_model::Changes,
    },
    /// The person's ticks on the draft card.
    AgentChecked {
        actor: String,
        list: Vec<(u32, bool)>,
    },
    /// The person's Apply of draft `number` went into the live model.
    AgentApplied {
        actor: String,
        number: u32,
    },
    /// A design option merged whole or by parts: it becomes the card, compiled against live.
    Merge(Box<super::opt_merge::MergeCard>),
    Reject,
    /// The person's Apply on an `ops_propose` card: the worker hands the envelope back.
    TakeProposal(u32),
    /// How the UI thread's run of a taken card ended.
    SettleProposal {
        id: u32,
        to: ProposalState,
    },
    RejectProposal(u32),
    End,
}

pub enum FromWorker {
    Text(String),
    Tool {
        name: String,
        ok: Option<bool>,
        err: Option<String>,
    },
    Plan(Arc<PlanView>),
    Usage(UsageTotals),
    Sent(String),
    /// One model step of the turn, for the step feed and the journal export.
    Step(ddc_ai::StepRecord),
    Audit(AuditEntry),
    Failed(AiError),
    Idle,
    /// The worker started a turn by itself (`replan::auto_text`): busy until its `Idle`.
    AutoTurn,
    TurnEnd(TurnEnd),
    Ready {
        base: PathBuf,
        work: PathBuf,
    },
    Preparing {
        done: u64,
        total: u64,
    },
    Synced {
        head: u64,
    },
    NeedSync,
    Snapshot {
        max_px: u32,
        reply: mpsc::Sender<Result<ImageData, String>>,
    },
    /// Every external agent's draft with something in it; `ack` counts the person's draft
    /// messages the worker has handled.
    AgentDrafts {
        list: Vec<Arc<AgentDraftView>>,
        ack: u64,
    },
    McpDone,
    /// An agent's `request_apply`: the card is put before the person; only they apply.
    AgentApplyAsked {
        actor: String,
    },
    RuleDraft {
        rule: RuleDef,
        text: String,
    },
    IssueDraft(Box<ddc_model::Topic>),
    ExportOffer {
        cat: String,
        filter: Option<String>,
        columns: Vec<String>,
        group: Option<String>,
    },
    /// A checked family definition for the family editor; the project is not touched.
    FamilyDraft {
        name: String,
        text: String,
        base_family: Option<String>,
        /// Rounds of the repair loop used and its estimated tokens.
        repair: (u32, u64),
        /// From an external agent: waits in the person's list, never opens by itself.
        queued: bool,
    },
    /// A schedule the assistant laid out: the card opens it as the view's own.
    ScheduleMade(Box<crate::ai::sched_tool::Made>),
    /// The open `ops_propose` cards, sent whenever one changes.
    Proposals(Vec<ProposalCard>),
    /// The envelope of a card the person applies; only the UI thread runs it on live.
    ProposalTaken {
        id: u32,
        envelope: Box<ddc_ops::Envelope>,
        base_head: u64,
    },
    ProposalRefused {
        id: u32,
        code: String,
    },
    /// The item could not take the corrected value; the draft kept the old one.
    ValueRefused {
        item: u32,
        why: String,
    },
    /// A view tool's change of the screen; the UI thread runs it, the model is not touched.
    View(super::view_tools::ViewCmd),
    /// A result that names elements: the chat shows it as a card with Show, Isolate, Select.
    Found(super::view_tools::Found),
    /// Information requirements the assistant read, or its report offered for saving.
    Req(ReqOffer),
}

/// What `load_requirements` and `requirements_report` hand the person; nothing is stored or
/// written to disk until they press the card's button.
#[derive(Clone, Debug)]
pub enum ReqOffer {
    Draft { rules: Vec<RuleDef>, source: String },
    Export,
}

/// What the panel reads each frame; the plan itself is `UiSession::plan`.
#[derive(Clone, Debug)]
pub enum UiEvent {
    Text(String),
    Tool {
        name: String,
        ok: Option<bool>,
        /// The tool's own failure text, when it failed.
        err: Option<String>,
        /// The refusal code of that failure (`refusal::AI_CODES`), untranslated.
        code: Option<&'static str>,
    },
    Usage(UsageTotals),
    Sent(SentEntry),
    /// One model step ended (role, model, tokens, cost, time, tools): the step feed.
    Step(ddc_ai::StepRecord),
    Failed(AiError),
    TurnDone(TurnEnd),
    /// The plan was recompiled after the person's own edits.
    Replanned,
    /// Agent changes nobody confirmed in time left the card.
    AgentExpired,
    /// An agent asked the person to review its draft; the panel opens on the card.
    AgentApplyAsked(String),
    /// A follow-up removed every item of the card on show.
    DraftEmptied,
    /// A corrected value did not compile with its item; the card keeps the old value.
    ValueRefused {
        item: u32,
        why: String,
    },
    Preparing {
        done: u64,
        total: u64,
    },
    RuleDraft {
        rule: RuleDef,
        text: String,
    },
    IssueDraft(Box<ddc_model::Topic>),
    ExportOffer {
        cat: String,
        filter: Option<String>,
        columns: Vec<String>,
        group: Option<String>,
    },
    ScheduleMade(Box<crate::ai::sched_tool::Made>),
    FamilyDraft {
        name: String,
        text: String,
        base_family: Option<String>,
        /// Rounds of the repair loop used and its estimated tokens.
        repair: (u32, u64),
        /// From an external agent: waits in the person's list, never opens by itself.
        queued: bool,
    },
    /// The person's Apply of an `ops_propose` card ended: the journal seq, or why not.
    /// `reload_error`: the step is written and undoable, only the view did not update yet.
    ProposalApplied {
        id: u32,
        result: Result<i64, AiError>,
        reload_error: Option<String>,
    },
    View(super::view_tools::ViewCmd),
    Found(super::view_tools::Found),
    Req(ReqOffer),
}

pub struct Done {
    pub reply: Reply,
    pub images: Vec<ImageData>,
}

pub type Handler = fn(&mut Core, &J, &CallCx<'_>) -> Result<Done, String>;

/// Skipped elements named in a no-effect error, at most this many.
const NO_EFFECT_SHOWN: usize = 10;

/// The coded error of an item that would write nothing, with why each element was left out.
fn no_effect_text(skips: &[super::rel_value::Skip]) -> String {
    let none = ErrorCode::NoEffect;
    if skips.is_empty() {
        return CmdError::wire(none, "the call is valid but changes nothing: no element matched");
    }
    let shown: Vec<String> = skips
        .iter()
        .take(NO_EFFECT_SHOWN)
        .map(|s| format!("{}:{} {}", s.cat, s.id, fmt::text(&s.why)))
        .collect();
    let why = format!(
        "every matched element was left out ({}): {}",
        skips.len(),
        shown.join("; ")
    );
    CmdError::wire(none, why)
}

fn handler(name: &str) -> Option<Handler> {
    tools_read::HANDLERS
        .iter()
        .chain(tools_write::HANDLERS.iter())
        .chain(super::view_tools::HANDLERS.iter())
        .chain(super::mep_tools::HANDLERS.iter())
        .chain(super::project_tools::HANDLERS.iter())
        .chain(super::gen_tools::HANDLERS.iter())
        .chain(super::norm_tools::HANDLERS.iter())
        .chain(super::fam_tools::HANDLERS.iter())
        .chain(super::plant_tools::HANDLERS.iter())
        .chain(super::station_tool::HANDLERS.iter())
        .find(|h| h.0 == name)
        .map(|h| h.1)
}

pub(crate) fn open_editor(path: &Path, units: Units) -> Result<Editor, AiError> {
    let mut ed = Editor::new_in_memory()?;
    ed.replace_model(Model::open_copy(&path.to_string_lossy())?)?;
    ed.set_units(units);
    Ok(ed)
}

impl Core {
    /// The address of the model in use, as the audit names it.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

/// The heart of the worker. The field order matters: connections close before `copies` removes
/// their files.
pub struct Core {
    pub ed: Editor,
    /// Session language of refusal texts the chat model reads (`refusal::LANGS`, else English).
    pub(crate) lang: &'static str,
    before: Option<Editor>,
    copies: PlanCopies,
    pub sets: Sets,
    pub plan: PlanState,
    pub view: ViewCtx,
    pub findings: HashMap<String, FindingRef>,
    next_finding: u32,
    baseline: Option<Baseline>,
    live_head: u64,
    copy_head: u64,
    base_seq: i64,
    rx: mpsc::Receiver<ToWorker>,
    deferred: VecDeque<ToWorker>,
    tx: mpsc::Sender<FromWorker>,
    pub gens: Vec<&'static dyn Generator>,
    pub rules: Vec<&'static dyn CheckRule>,
    pub max_chars: usize,
    pub allow_snapshots: Arc<AtomicBool>,
    images: bool,
    session: String,
    endpoint: String,
    model_name: String,
    keep_text: bool,
    /// Parts of a long `run_script` sent with `more`; empty at every turn start.
    pub script_parts: String,
    flagged: u32,
    ack: u64,
    /// Agent items the person has not looked at since they joined the card.
    unreviewed: BTreeSet<u32>,
    stop: CancelToken,
    /// An idle UI paints no frames: the worker wakes it after every message.
    wake: Arc<OnceLock<egui::Context>>,
    /// The card shows a plan: emptying it has to reach the UI too.
    published: bool,
    /// The last recompile failed, so `plan.steps` do not describe the scratch copy.
    broken: bool,
    /// An agent's plan is on the copy: a sync must not show it as the chat's card.
    quiet: bool,
    mcp_items: BTreeMap<u32, &'static str>,
    confirm_ttl: Duration,
    /// Since when the oldest agent change waits on the card, and whose it is.
    waiting: Option<(Instant, String)>,
    /// One draft per external agent connection, apart from the chat's card.
    drafts: agent_draft::AgentDrafts,
    /// Envelopes from `ops_propose`, waiting for the person's Apply.
    pub proposals: mcp_ops::Proposals,
    pub critic_cycles: u8,
    /// The write, check, fix loop of family drafts; fresh at every request.
    pub fam_repair: ddc_ai::family_repair::RepairBook,
    /// Set while an external agent's `family_draft` runs: the draft is queued for the person.
    pub fam_from_agent: bool,
    /// Values the `measure` tool returned this turn, each with its source.
    pub measured: Vec<(f64, &'static str)>,
    /// What read tools saw since the last item: it joins the next item of this turn.
    pub reads: super::readset::ReadSet,
    /// The mode of the person's last turn; an automatic turn needs a dialog to follow, and
    /// runs in `replan::AUTO_MODE` whatever this was.
    pub last_mode: Option<Mode>,
    pub auto_replan: bool,
    /// Held items an automatic turn was already started for.
    pub prompted: HashSet<u32>,
    /// Set for one `AgentRebase`: the draft replays by its conflicts, not whole.
    rebase: Option<ddc_model::Changes>,
    /// Images the person attached for comparison; `get_reference` hands them to the model.
    pub reference: Vec<ddc_ai::ImageData>,
    /// The pointed-at context that goes before the text of the next turn only.
    point: Option<String>,
    /// Files attached to the chat; tools read only the shared ones.
    pub attachments: Vec<Arc<super::attach::AttachData>>,
    /// Requirements the person confirmed on the card.
    pub requirements: Vec<ddc_io::attach::facts::Fact>,
    /// Marks on the change journal and the findings that stood at each.
    pub checkpoints: super::checkpoints::Checkpoints,
    target: catalog::Target,
}

impl Core {
    /// An idle UI paints no frames, and an agent's call must not wait for the next mouse move.
    pub fn send(&self, m: FromWorker) {
        // an ended session lands nothing: its UI may already belong to another project
        if self.stop.is_cancelled() {
            return;
        }
        let _ = self.tx.send(m);
        if let Some(c) = self.wake.get() {
            c.request_repaint();
        }
    }

    fn entry(
        &self,
        actor: &str,
        event: &str,
        plan: Option<String>,
        text: Option<String>,
        status: &str,
    ) -> AuditEntry {
        AuditEntry {
            session: self.session.clone(),
            actor: actor.to_string(),
            event: event.to_string(),
            endpoint: Some(self.endpoint.clone()),
            model: Some(self.model_name.clone()),
            text,
            plan,
            status: status.to_string(),
            ..AuditEntry::default()
        }
    }

    /// The worker only reports: the audit is written by the UI thread into the live file.
    pub fn audit(&self, event: &str, plan: Option<String>, text: Option<String>, status: &str) {
        self.audit_as("assistant", event, plan, text, status);
    }

    pub fn audit_as(
        &self,
        actor: &str,
        event: &str,
        plan: Option<String>,
        text: Option<String>,
        status: &str,
    ) {
        self.send(FromWorker::Audit(
            self.entry(actor, event, plan, text, status),
        ));
    }

    /// Marks for a new frozen set. With a plan on the copy a row shows the plan's own edits, not
    /// what the live model holds, so such a set is checked by category only.
    pub fn marks_of(&self, keys: &[Key]) -> Vec<u32> {
        if !self.plan.items.is_empty() {
            return vec![];
        }
        let now = rows::marks(&self.ed);
        keys.iter()
            .map(|k| now.get(&(k.table(), k.id)).map_or(0, |x| x.1))
            .collect()
    }

    pub fn selection_keys(&self) -> Vec<Key> {
        let door = |id: i64| match self.ed.model.opening_kind(id).as_deref() {
            Ok("window") => "window",
            _ => "door",
        };
        self.view
            .selection
            .iter()
            .filter_map(|s| rows::key_of_sel(*s, &door))
            .collect()
    }

    pub fn remember_finding(&mut self, f: &Finding) -> String {
        self.next_finding += 1;
        let id = format!("f{}", self.next_finding);
        self.findings.insert(
            id.clone(),
            FindingRef {
                id: id.clone(),
                rule: f.rule.to_string(),
                key: f.key(),
                elements: f.elements.clone(),
                fix: f.fix.clone(),
            },
        );
        if let Some(old) = self.next_finding.checked_sub(FINDINGS_KEPT) {
            self.findings.remove(&format!("f{old}"));
        }
        id
    }

    pub fn resolve_target(&mut self, t: &Targets) -> Result<Vec<Key>, plan::PlanError> {
        let cx = CompileCx {
            ed: &mut self.ed,
            slots: &mut self.plan.slots,
            sets: &self.sets,
            gens: &self.gens,
            rules: &self.rules,
            declared: HashSet::new(),
            skips: vec![],
        };
        let (keys, reads) = super::readset::record(|| plan::resolve(&cx, t));
        self.reads.merge(reads);
        keys
    }

    /// Base snapshot and the folder for design option copies of this session.
    pub fn option_paths(&self) -> (PathBuf, PathBuf) {
        (self.copies.base.clone(), self.copies.dir.join("options"))
    }

    pub fn next_item_id(&self) -> u32 {
        self.plan.next_item + 1
    }

    fn ensure_baseline(&mut self) -> Result<(), AiError> {
        if self.baseline.is_none() {
            self.baseline = Some(Baseline::capture(&self.ed.model)?);
        }
        Ok(())
    }

    /// Runs the operation on the scratch copy at once; a failure leaves neither item nor change.
    pub fn add_item(
        &mut self,
        title: String,
        why: String,
        op: PlanOp,
        actor: &str,
    ) -> Result<(u32, (usize, usize, usize)), String> {
        self.ensure_baseline().map_err(|e| e.to_string())?;
        let id = self.next_item_id();
        let mut depends_on = self.plan.depends_of(&op);
        depends_on.extend(self.plan.depends_on_created(&op, &self.sets));
        depends_on.sort_unstable();
        depends_on.dedup();
        let head = self.ed.model.change_head();
        let top = self.ed.model.op_top().map_err(|e| e.to_string())?;
        let slots = self.plan.slots.clone();
        let mut cx = CompileCx {
            ed: &mut self.ed,
            slots: &mut self.plan.slots,
            sets: &self.sets,
            gens: &self.gens,
            rules: &self.rules,
            declared: HashSet::new(),
            skips: vec![],
        };
        let span = ddc_model::perf::span("draft.compile");
        let (compiled, compile_reads) = super::readset::record(|| plan::compile_one(&mut cx, &op));
        drop(span);
        let declared = std::mem::take(&mut cx.declared);
        let skips = std::mem::take(&mut cx.skips);
        let steps = match compiled {
            Ok(s) => s,
            Err(e) => {
                self.plan.slot_owner.retain(|_, owner| *owner != id);
                self.audit_as(
                    actor,
                    "tool_write",
                    None,
                    plan::audit_text(&title, &why),
                    "failed",
                );
                // every element was left out: name each one and why
                let none = CmdError::from_text(&e.0).code == ErrorCode::NoEffect;
                return Err(if none && !skips.is_empty() { no_effect_text(&skips) } else { e.0 });
            }
        };
        let touched = self.ed.model.changes_since(head);
        // a valid call that writes nothing is reported, never kept as an empty item
        if touched.is_empty() {
            self.plan.slots = slots;
            self.plan.slot_owner.retain(|_, owner| *owner != id);
            self.audit_as(
                actor,
                "tool_write",
                None,
                plan::audit_text(&title, &why),
                "failed",
            );
            return Err(no_effect_text(&skips));
        }
        let diff = match self
            .ed
            .model
            .diff_from(&self.copies.base, &touched, DIFF_LIMIT)
        {
            Ok(d) => d,
            Err(e) => {
                // an item the model hears failed must not stay on the copy
                let undone = self
                    .ed
                    .model
                    .rollback_to(top)
                    .map_err(AiError::from)
                    .and_then(|_| self.ed.reload().map_err(AiError::from));
                if let Err(u) = undone {
                    self.broken = true;
                    self.send(FromWorker::Failed(u));
                    self.send(FromWorker::NeedSync);
                }
                self.plan.slots = slots;
                self.plan.slot_owner.retain(|_, owner| *owner != id);
                self.audit_as(
                    actor,
                    "tool_write",
                    None,
                    plan::audit_text(&title, &why),
                    "failed",
                );
                return Err(e.to_string());
            }
        };
        let counts = plan::item_counts(&self.plan, &steps, &diff, &touched);
        let made: HashSet<ddc_model::ElKey> =
            steps.iter().flat_map(super::exec::created_keys).collect();
        self.plan.next_item = id;
        self.plan.units.insert(id, self.ed.tool.units);
        self.plan.steps.push((id, steps));
        self.plan.declared.insert(id, declared);
        if !skips.is_empty() {
            self.plan.skips.insert(id, skips);
        }
        let mut reads = std::mem::take(&mut self.reads);
        reads.merge(compile_reads);
        reads.merge(plan::named_reads(&op, &self.sets));
        let writes = super::readset::ReadSet::from_changes_except(&touched, &made);
        let sets = super::readset::ItemSets {
            fp: super::conflict::written_fp(self.ed.model.sql(), &writes),
            reads,
            writes,
            base: self.live_head,
        };
        let item = plan::PlanItem {
            id,
            title: title.clone(),
            why: why.clone(),
            op,
            checked: true,
            depends_on,
            actor: actor.to_string(),
            conflict: None,
            sets: Arc::new(sets),
        };
        super::replan::place(&mut self.plan, item);
        let entry = self.entry(
            actor,
            "tool_write",
            Some(format!("{id}: +{} ~{} -{}", counts.0, counts.1, counts.2)),
            plan::audit_text(&title, &why),
            "ok",
        );
        self.send(FromWorker::Audit(entry));
        Ok((id, counts))
    }

    /// After this the scratch copy equals the base, by proof rather than by trust in the journal.
    pub fn rollback(&mut self) -> Result<(), AiError> {
        self.ed.model.rollback_to(self.base_seq)?;
        self.ed.reload()?;
        let touched = self.ed.model.changes_since(self.copy_head);
        if !self
            .ed
            .model
            .diff_from(&self.copies.base, &touched, 1)?
            .rows
            .is_empty()
        {
            self.close_scratch()?;
            self.copies.reset_work()?;
            self.open_scratch()?;
        }
        Ok(())
    }

    /// A failure leaves the old steps behind: `broken` keeps them from being applied until a new
    /// copy of the live model brings the scratch back to a known state.
    pub fn replan(&mut self) -> Result<(), AiError> {
        let res = self.rollback().and_then(|()| {
            plan::replay(
                &mut self.plan,
                &mut self.ed,
                &self.sets,
                &self.gens,
                &self.rules,
            )
            .map_err(AiError::from)
        });
        self.broken = res.is_err();
        if self.broken {
            self.send(FromWorker::NeedSync);
        }
        res
    }

    fn close_scratch(&mut self) -> Result<(), AiError> {
        self.ed.replace_model(Model::open(":memory:")?)?;
        Ok(())
    }

    fn open_scratch(&mut self) -> Result<(), AiError> {
        let work = Model::open_copy(&self.copies.work.to_string_lossy())?;
        // a throwaway copy, made again from the live model after a crash: its commits (one
        // per drafted operation) need not wait for the disk
        work.sql()
            .execute_batch("PRAGMA synchronous=OFF")
            .map_err(ddc_model::Error::from)?;
        self.ed.replace_model(work)?;
        self.ed.set_units(self.view.units);
        self.ed.set_level(self.view.level);
        self.copy_head = self.ed.model.change_head();
        self.base_seq = self.ed.model.op_top()?;
        Ok(())
    }

    fn set_view(&mut self, view: ViewCtx) {
        if view.units != self.view.units {
            self.ed.set_units(view.units);
        }
        if view.level != self.view.level {
            self.ed.set_level(view.level);
        }
        self.view = view;
    }

    /// The full picture of the plan on the scratch copy.
    pub fn build_view(&mut self, with_preview: bool) -> Result<PlanView, AiError> {
        let _s = ddc_model::perf::span("draft.view");
        if self.broken && !self.plan.items.is_empty() {
            let mut v = self.empty_view();
            v.items = self.plan.items.clone();
            v.issues = vec![validate::replan_issue()];
            v.variants = self.plan.variants.clone();
            v.variant = self.plan.variant;
            return Ok(v);
        }
        let span = ddc_model::perf::span("draft.view.diff");
        let touched = self.ed.model.changes_since(self.copy_head);
        let raw = self
            .ed
            .model
            .diff_from(&self.copies.base, &touched, DIFF_LIMIT)?;
        drop(span);
        let span = ddc_model::perf::span("draft.view.group");
        let mut diff = plan::split_reused(plan::group_diff(&raw), &plan::reused_ids(&self.plan));
        let secondary = plan::attribute(&mut diff, &self.plan, &self.ed);
        drop(span);
        let mut issues: Vec<PlanIssue> = vec![];
        let mut qty = vec![];
        if let Some(b) = &self.baseline {
            let span = ddc_model::perf::span("draft.view.validate");
            issues = validate::validate(&self.ed, b, &diff);
            drop(span);
            let span = ddc_model::perf::span("draft.view.rules");
            issues.extend(validate::rule_issues(
                &self.ed.model,
                &plan::all_rules(&self.rules),
                &diff,
            ));
            drop(span);
            issues.extend(materials::issues(&self.plan.items, &b.material_names));
            let _q = ddc_model::perf::span("draft.view.qty");
            qty = validate::qty_deltas(&self.ed.model, b)?;
        }
        let caps = validate::PlanCaps::read(&self.ed.model);
        issues.extend(gate_issues(&caps, self.target, &self.plan, &diff));
        let mass = validate::mass_issues(&self.ed.model, &diff, self.plan.allow_large);
        if self.target == catalog::Target::Main {
            issues.extend(mass.iter().cloned());
        }
        issues.extend(super::rel_value::skip_issues(&self.plan));
        issues.extend(validate::truncation_issues(raw.truncated, raw.rows.len()));
        let span = ddc_model::perf::span("draft.view.preview");
        let shown = if with_preview && !diff.is_empty() {
            // the base is read only for what the plan changes or deletes: a plan that only
            // creates does not open it
            let old = diff.iter().any(|e| {
                e.kind != ddc_model::RowKind::Created
                    && super::rows::ELEMENT_TABLES.contains(&e.key.0.as_str())
            });
            if old && self.before.is_none() {
                let _b = ddc_model::perf::span("draft.view.before");
                self.before = Some(open_editor(&self.copies.base, self.view.units)?);
            }
            let before = if old { self.before.as_mut() } else { None };
            preview::build(&mut self.ed, before, &diff)
        } else {
            preview::PreviewSet::default()
        };
        drop(span);
        let _t = ddc_model::perf::span("draft.view.tail");
        // model text reaches the card here: nothing in it may hide or reorder what the person reads
        for c in diff.iter_mut().flat_map(|e| e.cells.iter_mut()) {
            c.col = fmt::visible(&c.col);
            c.before = c.before.as_deref().map(fmt::visible);
            c.after = c.after.as_deref().map(fmt::visible);
        }
        for a in issues.iter_mut().flat_map(|i| i.args.iter_mut()) {
            *a = fmt::visible(a);
        }
        for q in &mut qty {
            q.key = fmt::visible(&q.key);
        }
        // no visible change means no undo step: the run_op would have nothing to record
        let compiled = if diff.is_empty() {
            CompiledPlan {
                base_head: self.live_head,
                steps: Arc::new(vec![]),
            }
        } else {
            self.plan.compiled(self.live_head)
        };
        let mut main_issues = main_issues_of(&caps, self.target, &self.plan, &diff);
        if self.target != catalog::Target::Main {
            main_issues.extend(mass);
        }
        Ok(PlanView {
            items: self.plan.items.clone(),
            diff,
            secondary,
            qty,
            issues,
            main_issues,
            preview: Arc::new(shown),
            compiled,
            stale: false,
            variants: self.plan.variants.clone(),
            variant: self.plan.variant,
            truncated: raw.truncated,
            flagged: self.flagged,
            ack: self.ack,
            gated: self.target,
            allow_large: self.plan.allow_large,
            unreviewed: self
                .unreviewed
                .iter()
                .copied()
                .filter(|id| self.plan.items.iter().any(|i| i.id == *id))
                .collect(),
            expires_in: self.confirm_time_left(),
            // dropping every item drops the files with them
            deliver: self.plan.deliver.filter(|_| !self.plan.items.is_empty()),
        })
    }

    fn empty_view(&self) -> PlanView {
        PlanView {
            items: vec![],
            diff: vec![],
            secondary: 0,
            qty: vec![],
            issues: vec![],
            main_issues: vec![],
            preview: Arc::new(preview::PreviewSet::default()),
            compiled: CompiledPlan {
                base_head: self.live_head,
                steps: Arc::new(vec![]),
            },
            stale: false,
            variants: vec![],
            variant: 0,
            truncated: false,
            flagged: self.flagged,
            ack: self.ack,
            gated: self.target,
            allow_large: self.plan.allow_large,
            unreviewed: vec![],
            expires_in: None,
            deliver: None,
        }
    }

    fn publish(&mut self) {
        match self.build_view(true) {
            Ok(v) => {
                self.published = !v.items.is_empty();
                self.send(FromWorker::Plan(Arc::new(v)));
            }
            Err(e) => {
                self.send(FromWorker::Failed(e));
                // the old card must not stay appliable once its plan cannot be described
                let v = self.empty_view();
                self.send(FromWorker::Plan(Arc::new(v)));
            }
        }
    }

    fn publish_shown(&mut self) {
        if !self.quiet && (!self.plan.items.is_empty() || self.published) {
            self.publish();
        }
    }

    fn refresh(&mut self) {
        if let Err(e) = self.replan() {
            self.send(FromWorker::Failed(e));
        }
        self.publish();
    }

    /// Item and slot numbers keep counting: the conversation may still mention the old ones.
    fn clear_plan(&mut self) {
        self.plan = PlanState {
            next_item: self.plan.next_item,
            next_slot: self.plan.next_slot,
            ..PlanState::default()
        };
    }

    fn take_copy(&mut self, source: SourceCopy) -> Result<(), AiError> {
        let tx = self.tx.clone();
        let stop = self.stop.clone();
        match source {
            SourceCopy::File(p) => {
                self.copies.prepare(&p, &mut |done, total| {
                    let _ = tx.send(FromWorker::Preparing { done, total });
                    !stop.is_cancelled()
                })?;
            }
            SourceCopy::Done(p) => {
                self.copies.adopt(&p)?;
            }
        }
        Ok(())
    }

    /// A new copy of the live model, then the plan again by intent: filters are recalculated and
    /// references to what is gone become conflicts of their items.
    fn sync(
        &mut self,
        source: SourceCopy,
        head: u64,
        changes: Option<ddc_model::Changes>,
    ) -> Result<(), AiError> {
        self.broken = true;
        self.before = None;
        self.close_scratch()?;
        let copied = self.take_copy(source);
        self.open_scratch()?;
        copied?;
        self.live_head = head;
        self.baseline = None;
        let now = rows::marks(&self.ed);
        self.sets.revalidate(&now);
        if !self.plan.items.is_empty() {
            self.ensure_baseline()?;
            match changes {
                // only the items the change touched conflict; the rest replay as they were
                Some(ch) if !ch.all => {
                    let conn = self.ed.model.sql();
                    let hit = super::conflict::conflicts(&self.plan.items, &ch, &|t, id| {
                        super::conflict::row_gone(conn, t, id)
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
                        head,
                    )?;
                }
                // project settings, a lost window or unknown: the whole plan again by intent
                _ => plan::replay(
                    &mut self.plan,
                    &mut self.ed,
                    &self.sets,
                    &self.gens,
                    &self.rules,
                )?,
            }
        }
        self.broken = false;
        Ok(())
    }

    /// `between`: a turn is running, so no automatic turn starts from here.
    fn on_sync(
        &mut self,
        source: SourceCopy,
        head: u64,
        changes: Option<ddc_model::Changes>,
        between: bool,
    ) {
        match self.sync(source, head, changes) {
            Ok(()) => {
                // the UI hears of the turn before the copy, so it never reads as idle between
                if let Some(text) = super::replan::auto_text(self).filter(|_| !between) {
                    if self.last_mode.is_some() {
                        self.send(FromWorker::AutoTurn);
                        let view = self.view.clone();
                        let mode = super::replan::AUTO_MODE;
                        self.deferred.push_front(ToWorker::Turn { text, view, mode });
                    }
                }
                self.send(FromWorker::Synced { head });
                self.publish_shown();
            }
            Err(e) => {
                self.send(FromWorker::Failed(e));
                self.send(FromWorker::NeedSync);
                self.publish_shown();
            }
        }
    }

    /// The live model took the plan, so the scratch copy is the new base.
    fn on_applied(
        &mut self,
        head: u64,
        stamped: Option<(Vec<(String, i64, String)>, ddc_model::ValueSrc)>,
    ) {
        let _s = ddc_model::perf::span("worker.applied");
        self.before = None;
        let mut res = Ok(());
        if let Some((cells, src)) = &stamped {
            let refs: Vec<(&str, i64, &str)> =
                cells.iter().map(|(t, id, n)| (t.as_str(), *id, n.as_str())).collect();
            res = self.ed.model.record_value_src(&refs, src).map(|_| ());
        }
        // a copy without the Apply's records is not the live model: never promote it
        if let Err(e) = res.and_then(|()| self.copies.promote_work(&self.ed.model)) {
            self.send(FromWorker::Failed(e.into()));
            self.send(FromWorker::NeedSync);
        }
        self.live_head = head;
        self.copy_head = self.ed.model.change_head();
        self.base_seq = self.ed.model.op_top().unwrap_or(self.base_seq);
        self.baseline = None;
        self.clear_plan();
        self.published = false;
        let now = rows::marks(&self.ed);
        self.sets.remark(&now);
    }

    fn next_msg(&mut self) -> Option<ToWorker> {
        loop {
            if let Some(m) = self.deferred.pop_front() {
                return Some(m);
            }
            let Some(left) = self.confirm_time_left() else {
                return self.rx.recv().ok();
            };
            match self.rx.recv_timeout(left) {
                Ok(m) => return Some(m),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if self.confirm_time_left() == Some(Duration::ZERO) {
                        self.expire_waiting();
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Between tool calls only a new copy is taken at once; everything else waits for the turn.
    fn drain_inbox(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                ToWorker::Sync {
                    source,
                    head,
                    changes,
                } => self.on_sync(source, head, changes, true),
                other => self.deferred.push_back(other),
            }
        }
    }

    /// Shared by the chat and the MCP bridge: same catalog, same access check, same handlers.
    pub fn note_measured(&mut self, value: f64, source: &'static str) {
        self.measured.push((value, source));
    }

    pub fn call_tool(
        &mut self,
        name: &str,
        args: &J,
        cx: &CallCx<'_>,
        images: bool,
    ) -> ToolOutcome {
        if cx.cancel.is_cancelled() {
            return CmdError::new(ErrorCode::Cancelled, "cancelled").outcome();
        }
        // the host checks again what the registry gate checked: a caller may skip the gate
        let Some(spec) = catalog::find(name) else {
            return CmdError::from_text("unknown tool").outcome();
        };
        if !catalog::allowed(spec, cx.mode, images) {
            return CmdError::new(ErrorCode::NotAllowed, "not_allowed").outcome();
        }
        if let Err(e) = catalog::validate(spec, args) {
            return CmdError::from_text(&e).outcome();
        }
        let Some(run) = handler(name) else {
            return CmdError::from_text("unknown tool").outcome();
        };
        let can_see = images;
        match run(self, args, cx) {
            Ok(Done { reply, mut images }) => {
                self.flagged += reply.flagged;
                let mut text = reply.text;
                if critic_hook::is_draft_tool(name) {
                    if let Some(note) = critic_hook::critic_note(self) {
                        text = format!("{text}\n{note}");
                    }
                    if can_see {
                        images.extend(critic_hook::snapshots(self));
                    }
                }
                ToolOutcome {
                    text,
                    images,
                    is_error: false,
                    flagged: reply.flagged,
                }
            }
            Err(e) => {
                let r = fmt::error(&e);
                self.flagged += r.flagged;
                ToolOutcome {
                    text: r.text,
                    images: vec![],
                    is_error: true,
                    flagged: r.flagged,
                }
            }
        }
    }

    /// The project depth setting and the depth the request names ("D4", or a Russian "up to working drawings"), for
    /// the planner; the request wins for this turn, the setting is not changed by it.
    fn depth_note(&self, text: &str) -> String {
        use ddc_model::Discipline;
        let s = self.ed.model.depth_settings().unwrap_or_default();
        let mut parts = vec![format!("depth=D{}", s.project)];
        for d in Discipline::ALL {
            parts.push(match s.of(d) {
                Some(x) => format!("{}=D{x}", d.as_str()),
                None => format!("{}=off", d.as_str()),
            });
        }
        if let Some(d) = ddc_model::depth::parse_words(text) {
            parts.push(format!("asked_depth=D{d}"));
        }
        parts.join(" ")
    }

    fn context_note(&self) -> String {
        let mut parts = Vec::new();
        if !self.view.hide_view {
            parts.push(format!("view={}", fmt::text(&self.view.view)));
        }
        if !self.view.hide_level {
            parts.push(format!("level_id={}", self.view.level));
        }
        parts.push(format!("selected={}", self.view.selection.len()));
        parts.push(format!("plan_items={}", self.plan.items.len()));
        parts.join(" ")
    }

    fn turn(&mut self, text: String, view: ViewCtx, mode: Mode, conv: &mut Conversation) {
        self.set_view(view);
        self.last_mode = Some(mode);
        let note = format!("{} {}", self.context_note(), self.depth_note(&text));
        let tx = self.tx.clone();
        let stop = self.stop.clone();
        let mut last = None;
        let mut on_event = |e: ConvoEvent| {
            if stop.is_cancelled() {
                return;
            }
            let m = match e {
                ConvoEvent::Text(t) => FromWorker::Text(t),
                ConvoEvent::ToolStart { name } => FromWorker::Tool {
                    name,
                    ok: None,
                    err: None,
                },
                ConvoEvent::ToolEnd {
                    name,
                    is_error,
                    detail,
                } => FromWorker::Tool {
                    name,
                    ok: Some(!is_error),
                    err: detail,
                },
                ConvoEvent::Usage(u) => {
                    last = Some(u);
                    FromWorker::Usage(u)
                }
                ConvoEvent::Sent(s) => FromWorker::Sent(s),
                ConvoEvent::Step(r) => FromWorker::Step(r),
            };
            let _ = tx.send(m);
        };
        self.critic_cycles = 0;
        self.fam_repair.reset_all();
        self.measured.clear();
        self.reads = Default::default();
        let kept = self.keep_text.then(|| fmt::clip(&text, TEXT_KEPT));
        let text = match self.point.take() {
            Some(b) => format!("{b}{text}"),
            None => text,
        };
        let res = conv.run_turn(text, note, mode, &mut *self, &mut on_event);
        let status = match &res {
            Ok(TurnEnd::Answered) => "ok",
            Ok(TurnEnd::LimitHit(_)) => "limit",
            Ok(TurnEnd::Cancelled) => "cancelled",
            Err(_) => "failed",
        };
        let mut entry = self.entry("user", "turn", None, kept, status);
        if let Some(u) = last {
            entry.tok_in = u.turn.input_tokens;
            entry.tok_out = u.turn.output_tokens;
            entry.cost = u.cost;
        }
        self.send(FromWorker::Audit(entry));
        if matches!(res, Ok(TurnEnd::Cancelled)) {
            self.audit_as("user", "cancel", None, None, "ok");
        }
        self.publish_shown();
        if !self.plan.items.is_empty() {
            let summary = format!(
                "items={} compiled={} flagged={}",
                self.plan.items.len(),
                self.plan.steps.len(),
                self.flagged
            );
            self.audit("plan", Some(summary), None, "ok");
        }
        let go_on = super::replan::after_turn(&res);
        match res {
            Ok(end) => self.send(FromWorker::TurnEnd(end)),
            Err(e) => self.send(FromWorker::Failed(AiError::Provider(provider_text(&e)))),
        }
        // places a replan freed and this turn did not fill: a later item goes last, as new
        self.plan.refill.clear();
        // a live edit came in while the turn ran (`drain_inbox`): its replan turn starts now,
        // and the UI stays busy across both
        if let Some(text) = go_on.then(|| super::replan::auto_text(self)).flatten() {
            self.send(FromWorker::AutoTurn);
            let view = self.view.clone();
            let mode = super::replan::AUTO_MODE;
            self.deferred.push_front(ToWorker::Turn { text, view, mode });
            return;
        }
        self.send(FromWorker::Idle);
    }

    /// A new variant is another layout under the same slots: `wall:n2` would silently become some
    /// other wall, so what was built on the old one waits for the person's decision.
    fn set_variant(&mut self, v: u32) {
        if v == self.plan.variant {
            return;
        }
        let Some(info) = self.plan.variants.iter().find(|x| x.id == v).cloned() else {
            return;
        };
        self.plan.variant = v;
        let last = self
            .plan
            .items
            .iter_mut()
            .rev()
            .find_map(|i| match &mut i.op {
                PlanOp::Generate { seed, .. } => Some((i.id, seed)),
                _ => None,
            });
        let Some((root, seed)) = last else {
            return;
        };
        *seed = info.seed;
        let mut gone: HashSet<u32> = HashSet::new();
        gone.insert(root);
        for it in &mut self.plan.items {
            if let Some(d) = it.depends_on.iter().copied().find(|d| gone.contains(d)) {
                it.checked = false;
                it.conflict = Some(format!("variant changed: depended on item {d}"));
                gone.insert(it.id);
            }
        }
    }

    /// The person's correction of one drafted value: the plan is compiled again on the copy with
    /// it. A value the item cannot take is refused and the draft keeps the old one.
    fn set_value(
        &mut self,
        item: u32,
        field: &super::value_edit::Field,
        value: &super::value_edit::Value,
    ) {
        let Some(at) = self.plan.items.iter().position(|i| i.id == item) else {
            self.publish();
            return;
        };
        let old = self.plan.items[at].op.clone();
        let had = Some(&self.plan.items[at])
            .filter(|i| i.checked)
            .and_then(|i| i.conflict.clone());
        let mut op = old.clone();
        if !super::value_edit::write(&mut op, field, value) {
            // the person typed it: a value the draft cannot take is said, never dropped
            let why = super::value_edit::refusal(field, value, self.view.units, crate::ui_is_ru());
            self.send(FromWorker::ValueRefused { item, why });
            self.publish();
            return;
        }
        self.plan.items[at].op = op;
        let res = self.replan().map_err(|e| e.to_string());
        // an unticked item is not compiled: it is checked when ticked again
        let now = self
            .plan
            .items
            .iter()
            .find(|i| i.id == item && i.checked)
            .and_then(|i| i.conflict.clone());
        let why = value_refusal(res, had.as_deref(), now.as_deref());
        match why {
            Some(why) => {
                if let Some(i) = self.plan.items.iter_mut().find(|i| i.id == item) {
                    i.op = old;
                }
                self.send(FromWorker::ValueRefused { item, why });
                self.refresh();
            }
            None => self.publish(),
        }
    }

    fn handle(&mut self, m: ToWorker, conv: &mut Conversation) -> bool {
        match m {
            ToWorker::Turn { text, view, mode } => {
                self.script_parts.clear();
                self.turn(text, view, mode, conv)
            }
            ToWorker::SetChecked(list) => {
                self.ack += 1;
                for (id, on) in list {
                    if let Some(i) = self.plan.items.iter_mut().find(|i| i.id == id) {
                        i.checked = on;
                    }
                }
                self.refresh();
            }
            ToWorker::SetVariant(v) => {
                self.ack += 1;
                self.set_variant(v);
                self.refresh();
            }
            ToWorker::Reviewed(seen) => {
                self.ack += 1;
                for id in &seen {
                    self.unreviewed.remove(id);
                }
                self.publish();
            }
            ToWorker::SetReference(images) => {
                self.reference = images;
            }
            ToWorker::SetPoint(block) => {
                self.point = block;
            }
            ToWorker::SetAttachments(files, facts) => {
                self.attachments = files;
                self.requirements = facts;
            }
            ToWorker::AllowLarge(on) => {
                self.ack += 1;
                self.plan.allow_large = on;
                self.publish();
            }
            ToWorker::SetValue { item, field, value } => {
                self.ack += 1;
                self.set_value(item, &field, &value);
            }
            ToWorker::Sync {
                    source,
                    head,
                    changes,
                } => self.on_sync(source, head, changes, false),
            ToWorker::Applied { head, stamped, .. } => {
                self.unreviewed.clear();
                self.settle_items(true);
                self.on_applied(head, stamped);
            }
            ToWorker::View(view) => self.set_view(view),
            ToWorker::Mcp { call } => self.on_mcp(call),
            ToWorker::McpCut => self.drop_groups("revoked"),
            ToWorker::AgentDiscard(actor) => {
                self.drafts.ack += 1;
                let _ = self.discard_draft(&actor, "user");
            }
            ToWorker::AgentRecheck(actor) => self.draft_recheck(&actor),
            ToWorker::AgentRebase { actor, changes } => {
                self.rebase = Some(changes);
                self.draft_recheck(&actor);
                self.rebase = None;
            }
            ToWorker::AgentChecked { actor, list } => self.draft_checked(&actor, &list),
            ToWorker::AgentApplied { actor, number } => {
                self.drafts.ack += 1;
                self.draft_applied(&actor, number);
            }
            ToWorker::Merge(card) => self.open_merge(*card),
            ToWorker::Reject => {
                self.ack += 1;
                self.unreviewed.clear();
                self.settle_items(false);
                self.clear_plan();
                if let Err(e) = self.rollback() {
                    self.send(FromWorker::Failed(e));
                }
                self.audit("reject", None, None, "ok");
                self.publish();
            }
            ToWorker::TakeProposal(id) => self.take_proposal(id),
            ToWorker::SettleProposal { id, to } => self.settle_proposal(id, to),
            ToWorker::RejectProposal(id) => self.reject_proposal(id),
            ToWorker::End => return false,
        }
        true
    }

    fn restore_chat_plan(&mut self, chat: PlanState) {
        let (next_item, next_slot) = (self.plan.next_item, self.plan.next_slot);
        self.plan = chat;
        self.plan.next_item = next_item;
        self.plan.next_slot = next_slot;
        let had_plan = !self.plan.items.is_empty();
        let res = self.rollback().and_then(|()| {
            if had_plan {
                self.ensure_baseline()?;
                plan::replay(
                    &mut self.plan,
                    &mut self.ed,
                    &self.sets,
                    &self.gens,
                    &self.rules,
                )?;
            }
            Ok(())
        });
        self.broken = res.is_err();
        if let Err(e) = res {
            self.send(FromWorker::Failed(e));
            self.send(FromWorker::NeedSync);
        }
        self.publish();
    }
}

impl ToolHost for Core {
    /// Chat tools can only add plan items on the scratch copy: the sole way from here to the live
    /// model is the person's Apply, whatever produced the call, a native call or a text block.
    fn call(&mut self, name: &str, args: &J, cx: &CallCx<'_>) -> ToolOutcome {
        if self.stop.is_cancelled() {
            self.script_parts.clear();
            return CmdError::new(ErrorCode::Cancelled, "cancelled").outcome();
        }
        self.drain_inbox();
        let images = self.images;
        self.call_tool(name, args, cx, images)
    }
}

struct WorkerInit {
    cfg: SessionCfg,
    source: SourceCopy,
    head: u64,
    view: ViewCtx,
    rx: mpsc::Receiver<ToWorker>,
    tx: mpsc::Sender<FromWorker>,
    cancel: CancelToken,
    stop: CancelToken,
    wake: Arc<OnceLock<egui::Context>>,
}

fn open_core(init: WorkerInit) -> Result<(Core, Conversation), AiError> {
    let WorkerInit {
        cfg,
        source,
        head,
        view,
        rx,
        tx,
        cancel,
        stop,
        wake,
    } = init;
    let copies = PlanCopies::new(&cfg.session)?;
    let mut gens: Vec<&'static dyn Generator> = ddc_gen::registry().to_vec();
    gens.extend(cfg.extra_generators.iter().copied());
    let mut conv = Conversation::new(
        cfg.provider,
        cfg.profile.clone(),
        cfg.limits.clone(),
        cancel,
    );
    let lang = if crate::ui_lang_now() { "ru" } else { ddc_ai::refusal::lang(ddc_ui::i18n::current().code()) };
    conv.set_lang(lang);
    let mut core = Core {
        ed: Editor::new_in_memory()?,
        lang,
        before: None,
        copies,
        sets: Sets::default(),
        plan: PlanState::default(),
        view,
        findings: HashMap::new(),
        next_finding: 0,
        baseline: None,
        live_head: head,
        copy_head: 0,
        base_seq: 0,
        rx,
        deferred: VecDeque::new(),
        tx,
        gens,
        rules: cfg.extra_rules,
        max_chars: cfg.limits.max_result_chars,
        allow_snapshots: cfg.allow_snapshots,
        images: cfg.profile.images,
        session: cfg.session,
        // the journal travels with the project: the host only, never a path or query key
        endpoint: ddc_ai::profile::url_origin(&cfg.profile.base_url),
        model_name: cfg.profile.model,
        keep_text: cfg.keep_text,
        script_parts: String::new(),
        target: cfg.target,
        flagged: 0,
        ack: 0,
        unreviewed: BTreeSet::new(),
        stop,
        wake,
        published: false,
        broken: false,
        quiet: false,
        mcp_items: BTreeMap::new(),
        confirm_ttl: cfg.confirm_ttl,
        waiting: None,
        drafts: agent_draft::AgentDrafts::default(),
        proposals: mcp_ops::Proposals::default(),
        critic_cycles: 0,
        fam_repair: Default::default(),
        fam_from_agent: false,
        measured: Vec::new(),
        reads: Default::default(),
        last_mode: None,
        auto_replan: cfg.auto_replan,
        prompted: HashSet::new(),
        rebase: None,
        reference: Vec::new(),
        point: None,
        attachments: Vec::new(),
        requirements: Vec::new(),
        checkpoints: Default::default(),
    };
    core.take_copy(source)?;
    core.open_scratch()?;
    Ok((core, conv))
}

/// Why a corrected value is refused: it does not compile, or it brings a conflict the item did
/// not have (a new one or another one). A conflict the item already had and keeps unchanged is
/// not the value's doing, so the value is taken and the card goes on showing that conflict.
fn value_refusal(
    compiled: Result<(), String>,
    had: Option<&str>,
    now: Option<&str>,
) -> Option<String> {
    match compiled {
        Err(e) => Some(e),
        Ok(()) => now.filter(|c| had != Some(*c)).map(str::to_owned),
    }
}

fn worker(init: WorkerInit) {
    let tx = init.tx.clone();
    let head = init.head;
    let orphan = match &init.source {
        SourceCopy::Done(p) => Some(p.clone()),
        SourceCopy::File(_) => None,
    };
    let (mut core, mut conv) = match open_core(init) {
        Ok(x) => x,
        Err(e) => {
            if let Some(p) = orphan {
                lifecycle::remove_snap(&p);
            }
            let _ = tx.send(FromWorker::Failed(e));
            let _ = tx.send(FromWorker::Idle);
            return;
        }
    };
    if core.stop.is_cancelled() {
        core.discard_pending();
        return;
    }
    let _ = tx.send(FromWorker::Ready {
        base: core.copies.base.clone(),
        work: core.copies.work.clone(),
    });
    let _ = tx.send(FromWorker::Synced { head });
    let _ = tx.send(FromWorker::Idle);
    while let Some(m) = core.next_msg() {
        // checked before every message: a late answer must not land in an ended session
        if core.stop.is_cancelled() {
            lifecycle::discard_msg(m);
            break;
        }
        if !core.handle(m, &mut conv) {
            break;
        }
    }
    core.discard_pending();
}

static SNAP_SEQ: AtomicU64 = AtomicU64::new(0);

fn source_of(ed: &Editor) -> Result<SourceCopy, AiError> {
    let file: String = ed
        .model
        .sql()
        .query_row("PRAGMA database_list", [], |r| r.get(2))
        .map_err(|e| AiError::Internal(e.to_string()))?;
    if !file.is_empty() {
        return Ok(SourceCopy::File(PathBuf::from(file)));
    }
    let n = SNAP_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("ddc-ai-snap-{}-{n}.db", std::process::id()));
    ed.model.snapshot_to(&path)?;
    Ok(SourceCopy::Done(path))
}

pub type SnapshotRequest = (u32, mpsc::Sender<Result<ImageData, String>>);

/// Read fresh at every use. A file that cannot answer asks for the person, not for less care.
pub fn live_policy(ed: &Editor) -> McpPolicy {
    match ed.model.ai_policy(POLICY_KEY) {
        Ok(v) => crate::ai::mcp_bridge::session_policy(McpPolicy::from_setting(v.as_deref())),
        Err(_) => McpPolicy::Confirm,
    }
}

/// The UI-side handle. `pump_core` must run once a frame on the thread that owns the live `Editor`.
pub struct UiSession {
    tx: mpsc::Sender<ToWorker>,
    rx: mpsc::Receiver<FromWorker>,
    cancel: CancelToken,
    stop: CancelToken,
    session: String,
    endpoint: String,
    model_name: String,
    usage: Option<UsageTotals>,
    wake: Arc<OnceLock<egui::Context>>,
    head: u64,
    plan: Option<Arc<PlanView>>,
    /// When the card last arrived: the agent changes' countdown runs from there.
    plan_at: Instant,
    busy: bool,
    stale: bool,
    sync_out: bool,
    next_sync: Option<Instant>,
    force_sync: bool,
    sent: u64,
    acked: u64,
    alive: bool,
    events: VecDeque<UiEvent>,
    snapshots: VecDeque<SnapshotRequest>,
    paths: Option<(PathBuf, PathBuf)>,
    join: Option<thread::JoinHandle<Option<()>>>,
    /// Disconnects when the worker thread returns, panics included.
    done: Option<mpsc::Receiver<()>>,
    gesture: bool,
    agent_out: usize,
    /// The external agents' drafts as the worker last drew them.
    agent_drafts: Vec<Arc<AgentDraftView>>,
    /// Draft messages sent to the worker and the count it has handled.
    agent_sent: u64,
    agent_acked: u64,
    model_epoch: u64,
    agent_batches: BTreeMap<String, u64>,
    /// `(option, note)` of the merge card on show: its Apply marks the option merged.
    merge_of: Option<(i64, Option<String>)>,
    /// Open `ops_propose` cards as the worker last reported them.
    proposals: Vec<ProposalCard>,
    /// A card handed to Apply whose envelope has not come back yet.
    proposal_out: Option<u32>,
    /// An Apply was written but the view reload failed: retried each pump, Apply waits for it.
    resync_due: bool,
    /// The card as of the person's last look; Apply waits until a revision is acknowledged.
    redraft: crate::ai::ui::plan_delta::Redraft,
    /// The person's own value edits in flight and the rows a recompile on resync changed.
    track: crate::ai::ui::plan_delta::CardTrack,
    /// Requests sent in this session and the last one's text: the source of applied values.
    asks: u32,
    last_ask: String,
}

/// Who an Apply of `items` says set the values: a generator, a check fix, or the assistant.
pub(crate) fn src_kind(items: &[plan::PlanItem]) -> ddc_model::SrcKind {
    use ddc_model::SrcKind;
    let on: Vec<&plan::PlanItem> = items.iter().filter(|i| i.checked).collect();
    let built = |op: &plan::PlanOp| {
        matches!(op, plan::PlanOp::Generate { .. } | plan::PlanOp::Project(_) | plan::PlanOp::Plant(_))
    };
    if on.iter().any(|i| built(&i.op)) {
        SrcKind::Generator
    } else if !on.is_empty() && on.iter().all(|i| matches!(i.op, plan::PlanOp::Fix(_))) {
        SrcKind::RuleFix
    } else {
        SrcKind::Ai
    }
}

impl UiSession {
    pub fn start(ed: &Editor, cfg: SessionCfg, view: ViewCtx) -> Result<UiSession, AiError> {
        ddc_model::set_audit_scrubber(ddc_ai::secret::scrub_known);
        let head = ed.model.change_head();
        let source = source_of(ed)?;
        mcp_info::set_project(ed.project.path());
        let (to_tx, to_rx) = mpsc::channel();
        let (from_tx, from_rx) = mpsc::channel();
        let cancel = CancelToken::new();
        let stop = CancelToken::new();
        let session = cfg.session.clone();
        let endpoint = ddc_ai::profile::url_origin(&cfg.profile.base_url);
        let model_name = cfg.profile.model.clone();
        let wake = Arc::new(OnceLock::new());
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let init = WorkerInit {
            cfg,
            source,
            head,
            view,
            rx: to_rx,
            tx: from_tx,
            cancel: cancel.clone(),
            stop: stop.clone(),
            wake: wake.clone(),
        };
        // the worker never writes the live model: proposals come back and run on the main thread;
        // a crash drops the channel and pump_core reports the internal error
        let join = crate::jobs::spawn_guarded(
            "ai-worker",
            move || {
                let _done = done_tx;
                worker(init)
            },
            |_| {},
        )
        .map_err(|e| AiError::Internal(e.to_string()))?;
        Ok(UiSession {
            tx: to_tx,
            rx: from_rx,
            cancel,
            stop,
            session,
            endpoint,
            model_name,
            usage: None,
            wake,
            head,
            plan: None,
            plan_at: Instant::now(),
            busy: true,
            stale: false,
            sync_out: false,
            next_sync: None,
            force_sync: false,
            sent: 0,
            acked: 0,
            alive: true,
            events: VecDeque::new(),
            snapshots: VecDeque::new(),
            paths: None,
            join: Some(join),
            done: Some(done_rx),
            gesture: false,
            agent_out: 0,
            agent_drafts: Vec::new(),
            agent_sent: 0,
            agent_acked: 0,
            model_epoch: crate::ai::mcp_bridge::model_epoch(),
            agent_batches: BTreeMap::new(),
            merge_of: None,
            proposals: Vec::new(),
            proposal_out: None,
            resync_due: false,
            redraft: Default::default(),
            track: Default::default(),
            asks: 0,
            last_ask: String::new(),
        })
    }

    pub fn plan(&self) -> Option<&Arc<PlanView>> {
        self.plan.as_ref()
    }

    /// What a follow-up changed since the person last looked; None when nothing did.
    pub fn redraft(&self) -> Option<&crate::ai::ui::plan_delta::PlanDelta> {
        Some(self.redraft.delta()).filter(|d| !d.is_empty())
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    pub fn is_stale(&self) -> bool {
        self.stale
    }

    pub fn head(&self) -> u64 {
        self.head
    }

    pub fn base_path(&self) -> Option<&Path> {
        self.paths.as_ref().map(|p| p.0.as_path())
    }

    pub fn work_path(&self) -> Option<&Path> {
        self.paths.as_ref().map(|p| p.1.as_path())
    }

    pub fn drain_events(&mut self) -> Vec<UiEvent> {
        self.events.drain(..).collect()
    }

    pub fn take_snapshot_requests(&mut self) -> Vec<SnapshotRequest> {
        self.snapshots.drain(..).collect()
    }

    fn send(&self, m: ToWorker) {
        if let Err(mpsc::SendError(m)) = self.tx.send(m) {
            lifecycle::discard_msg(m);
        }
    }

    /// Without a plan the copy is refreshed only here, not on every edit: a full copy of a big
    /// model per keystroke would be absurd.
    pub fn send_turn(
        &mut self,
        ed: &Editor,
        text: String,
        view: ViewCtx,
        mode: Mode,
    ) -> Result<(), AiError> {
        if self.busy {
            return Err(AiError::Busy);
        }
        if ed.model.plan_stale(self.head) && !self.sync_out {
            self.send_sync(ed);
        }
        self.cancel.reset();
        self.busy = true;
        self.redraft.on_send();
        self.track.clear();
        self.asks += 1;
        self.last_ask = ddc_model::value_src::prompt_summary(&text);
        self.send(ToWorker::Turn { text, view, mode });
        Ok(())
    }

    fn acked_send(&mut self, m: ToWorker) {
        self.sent += 1;
        self.send(m);
    }

    /// Apply stays off until the worker confirms it recompiled with these ticks.
    pub fn set_checked(&mut self, list: Vec<(u32, bool)>) {
        self.acked_send(ToWorker::SetChecked(list));
    }

    pub fn set_variant(&mut self, id: u32) {
        self.acked_send(ToWorker::SetVariant(id));
    }

    /// One drafted value corrected on the card. Only the draft changes; Apply stays off until
    /// the worker has compiled the item with it, and a value it cannot take is refused.
    pub fn set_value(
        &mut self,
        item: u32,
        field: crate::ai::value_edit::Field,
        value: crate::ai::value_edit::Value,
    ) {
        let own = crate::ai::ui::plan_delta::OwnEdit {
            item,
            field: field.clone(),
            value: value.clone(),
        };
        self.acked_send(ToWorker::SetValue { item, field, value });
        self.track.own_edit(own, self.sent);
    }

    /// Rows a recompile against the changed live model touched; None when it touched none.
    pub fn rebased(&self) -> Option<&crate::ai::ui::plan_delta::PlanDelta> {
        self.track.rebased()
    }

    /// The person confirms they looked at the card with the agent's items on it.
    /// Also acknowledges a revised draft; its counts come back for the chat history.
    /// `rev`: the card revision the person saw (`plan_rev` when it was drawn).
    pub fn review(&mut self, rev: Option<u64>) -> Option<crate::ai::ui::plan_delta::Note> {
        // only what the card showed: items an agent adds before the worker reads this stay locked
        let seen = self.plan.as_ref().map(|p| p.unreviewed.clone()).unwrap_or_default();
        let note = self.redraft.acknowledge(rev);
        if note.is_none() || !seen.is_empty() {
            self.acked_send(ToWorker::Reviewed(seen));
        }
        note
    }

    pub fn set_reference(&mut self, images: Vec<ddc_ai::ImageData>) {
        self.send(ToWorker::SetReference(images));
    }

    /// Goes before the turn on the same channel; the block is used by that turn only.
    pub fn set_point(&mut self, block: Option<String>) {
        self.send(ToWorker::SetPoint(block));
    }

    /// Goes before the turn on the same channel, so the turn's tools see these files.
    pub fn set_attachments(
        &mut self,
        files: Vec<Arc<super::attach::AttachData>>,
        facts: Vec<ddc_io::attach::facts::Fact>,
    ) {
        self.send(ToWorker::SetAttachments(files, facts));
    }

    pub fn allow_large(&mut self, on: bool) {
        self.acked_send(ToWorker::AllowLarge(on));
    }

    pub fn reject(&mut self) {
        self.plan = None;
        self.redraft.clear();
        self.track.clear();
        self.merge_of = None;
        self.acked_send(ToWorker::Reject);
    }

    /// Whatever the call does, its writes wait in the connection's draft for the person.
    pub fn mcp(&mut self, _ed: &Editor, call: McpCall) {
        self.agent_out += 1;
        self.send(ToWorker::Mcp { call });
    }

    /// What the agent reads must be the current model and what the person sees now.
    pub fn mcp_call(&mut self, ed: &Editor, call: McpCall, view: Option<ViewCtx>) {
        if ed.model.plan_stale(self.head) && !self.sync_out {
            self.send_sync(ed);
        }
        if let Some(v) = view {
            self.send(ToWorker::View(v));
        }
        self.mcp(ed, call);
    }

    pub fn mcp_cut(&self) {
        self.send(ToWorker::McpCut);
    }

    /// Open `ops_propose` cards, newest last.
    pub fn proposals(&self) -> &[ProposalCard] {
        &self.proposals
    }

    pub fn proposal_applying(&self) -> Option<u32> {
        self.proposal_out
    }

    /// The person's Apply on an `ops_propose` card. The worker hands the envelope back and
    /// `pump_core` runs it on live through `Editor::run_ops`: one journal entry, one undo
    /// step, all or nothing. The result arrives as `UiEvent::ProposalApplied`.
    pub fn apply_proposal(&mut self, id: u32) -> Result<(), AiError> {
        if !self.alive {
            return Err(AiError::Internal(crate::jobs::internal_error()));
        }
        if self.proposal_out.is_some() || self.resync_due || !self.agent_free() {
            return Err(AiError::Busy);
        }
        let waiting = self
            .proposals
            .iter()
            .any(|c| c.id == id && c.state == ProposalState::Waiting);
        if !waiting {
            return Err(AiError::EmptyPlan);
        }
        self.proposal_out = Some(id);
        self.send(ToWorker::TakeProposal(id));
        Ok(())
    }

    pub fn reject_proposal(&mut self, id: u32) {
        self.proposals.retain(|c| c.id != id);
        self.send(ToWorker::RejectProposal(id));
    }

    /// The last gate of a card's Apply, on the UI thread with the live model. `Err`: nothing
    /// was written. `Ok((seq, Some(e)))`: the entry and its undo step are recorded and only the
    /// view reload failed; the card must still settle Approved, or a second Apply journals twice.
    fn run_proposal(
        &mut self,
        ed: &mut Editor,
        env: ddc_ops::Envelope,
        base_head: u64,
    ) -> Result<(i64, Option<String>), AiError> {
        if self.model_epoch != crate::ai::mcp_bridge::model_epoch() {
            return Err(AiError::Internal("another project was opened".to_string()));
        }
        if self.gesture {
            return Err(AiError::Busy);
        }
        if ed.model.plan_stale(base_head) {
            return Err(AiError::Stale);
        }
        let top = ed.model.op_top()?;
        let pushes = ed.pushes;
        let run = ed.run_ops(env);
        if ed.pushes == pushes {
            // no undo step: the executor wrote nothing or rolled its write back
            run?;
            return Err(AiError::EmptyPlan);
        }
        let mut reload_error = run.err().map(|e| e.to_string());
        let seq = match ed.model.op_top() {
            Ok(seq) => seq,
            Err(e) => {
                if reload_error.is_none() {
                    reload_error = Some(e.to_string());
                }
                top + 1
            }
        };
        Ok((seq, reload_error))
    }

    /// After an Apply whose reload failed: the view and the worker copy catch up once the
    /// model reads again.
    fn resync(&mut self, ed: &mut Editor) {
        if self.resync_due && ed.reload().is_ok() {
            self.resync_due = false;
            self.force_sync = true;
        }
    }

    /// An applied proposal waits for the view to reload; no Apply runs until it has.
    pub fn resync_due(&self) -> bool {
        self.resync_due
    }

    fn on_proposal_taken(
        &mut self,
        ed: &mut Editor,
        id: u32,
        env: ddc_ops::Envelope,
        base_head: u64,
    ) {
        if self.proposal_out == Some(id) {
            self.proposal_out = None;
        }
        // the person applies; the proposing agent is kept beside it
        let by = match &env.actor {
            ddc_ops::Actor::Agent { name } => Some(name.clone()),
            _ => None,
        };
        let run = self.run_proposal(ed, env, base_head);
        let reload_error = run.as_ref().ok().and_then(|(_, e)| e.clone());
        let result = run.map(|(seq, _)| seq);
        if let Some(e) = &reload_error {
            eprintln!("assistant proposal {id}: applied, view reload failed: {e}");
            self.resync_due = true;
        }
        let to = match &result {
            Ok(_) => ProposalState::Approved,
            Err(AiError::Stale) => ProposalState::Stale,
            Err(_) => ProposalState::Waiting,
        };
        let (seq, status) = match &result {
            Ok(seq) if reload_error.is_some() => (Some(*seq), "ok_reload_failed"),
            Ok(seq) => (Some(*seq), "ok"),
            Err(AiError::Stale) => (None, "stale"),
            Err(AiError::EmptyPlan) => (None, "empty"),
            Err(_) => (None, "failed"),
        };
        let e = AuditEntry {
            session: self.session.clone(),
            actor: "user".to_string(),
            event: "apply".to_string(),
            plan: Some(format!("proposal={id}")),
            text: by,
            op_seq: seq,
            status: status.to_string(),
            ..AuditEntry::default()
        };
        audit_write(&ed.model, &e);
        self.send(ToWorker::SettleProposal { id, to });
        self.events.push_back(UiEvent::ProposalApplied {
            id,
            result,
            reload_error,
        });
    }

    /// The person is dragging or in the middle of a tool: an agent's write must not land under it.
    pub fn set_gesture(&mut self, on: bool) {
        self.gesture = on;
    }

    pub fn session_id(&self) -> &str {
        &self.session
    }

    pub fn model_epoch(&self) -> u64 {
        self.model_epoch
    }

    /// Batches of this agent applied in this session, for the indicator.
    pub fn agent_batches(&self, client: &str) -> u64 {
        self.agent_batches
            .get(&format!("{ACTOR_PREFIX}{client}"))
            .copied()
            .unwrap_or(0)
    }

    /// Calls still in the worker.
    pub fn agent_active(&self) -> bool {
        self.agent_out > 0
    }

    /// Operations waiting in the external agents' drafts, all connections together.
    pub fn agent_writes_waiting(&self) -> usize {
        self.agent_drafts.iter().map(|d| d.ops.len()).sum()
    }

    /// The external agents' drafts on show, one per connection.
    pub fn agent_drafts(&self) -> &[Arc<AgentDraftView>] {
        &self.agent_drafts
    }

    pub fn agent_draft(&self, actor: &str) -> Option<&Arc<AgentDraftView>> {
        self.agent_drafts.iter().find(|d| d.actor == actor)
    }

    /// The live model moved since this draft was compiled: Apply waits for a re-check.
    pub fn agent_draft_stale(&self, ed: &Editor, actor: &str) -> bool {
        self.agent_draft(actor)
            .is_none_or(|d| ed.model.plan_stale(d.base_head()))
    }

    /// The worker's copy belongs to an agent until this is true: the card's steps would not match.
    fn agent_free(&self) -> bool {
        self.agent_out == 0 && self.agent_acked == self.agent_sent
    }

    fn agent_send(&mut self, m: ToWorker) {
        self.agent_sent += 1;
        self.send(m);
    }

    /// Nothing in flight that a draft's Apply would have to wait for; staleness is apart.
    pub fn agent_ready(&self) -> bool {
        self.agent_free() && !self.busy && !self.sync_out && self.alive
    }

    /// The person's Apply of the draft revision `rev` they saw: one undo step on the live model,
    /// checked against the diff on show like the chat's card.
    pub fn apply_agent_draft(
        &mut self,
        ed: &mut Editor,
        actor: &str,
        rev: u64,
    ) -> Result<i64, AiError> {
        if !self.alive {
            return Err(AiError::Internal(crate::jobs::internal_error()));
        }
        let Some(d) = self.agent_draft(actor).cloned() else {
            return Err(AiError::EmptyPlan);
        };
        if d.rev != rev {
            return Err(AiError::ReviewNeeded);
        }
        if self.busy || self.sync_out || !self.agent_free() {
            return Err(AiError::Busy);
        }
        let by = vec![d.name.clone()];
        let p = &d.plan;
        if ed.model.plan_stale(d.base_head()) {
            self.audit_apply(ed, "user", None, &p.compiled, "stale", None, &by);
            return Err(AiError::Stale);
        }
        if d.blocked() {
            self.audit_apply(ed, "user", None, &p.compiled, "blocked", None, &by);
            return Err(AiError::Blocked);
        }
        if p.compiled.steps.is_empty() {
            return Err(AiError::EmptyPlan);
        }
        if self.model_epoch != crate::ai::mcp_bridge::model_epoch() {
            return Err(AiError::Internal("another project was opened".to_string()));
        }
        let top = ed.model.op_top()?;
        let base = self.paths.as_ref().map(|x| x.0.clone()).unwrap_or_default();
        let shown = super::apply_verify::Shown {
            base: &base,
            diff: &p.diff,
            truncated: p.truncated,
            limit: DIFF_LIMIT,
        };
        let audit = AuditEntry {
            session: self.session.clone(),
            actor: "user".to_string(),
            endpoint: Some(self.endpoint.clone()),
            model: Some(self.model_name.clone()),
            ..AuditEntry::default()
        };
        let ru = crate::ui_is_ru();
        match super::apply_verify::apply_verified(ed, &p.compiled, None, &shown, &audit, ru) {
            Ok(None) => {}
            Ok(Some(e)) => self.events.push_back(UiEvent::Failed(e)),
            Err(e) => {
                let status = if matches!(e, AiError::Stale | AiError::Diverged(_)) {
                    "stale"
                } else {
                    "failed"
                };
                self.audit_apply(ed, "user", None, &p.compiled, status, None, &by);
                if status == "stale" {
                    self.send_sync(ed);
                }
                return Err(e);
            }
        }
        let op_seq = applied_seq(ed.model.op_top(), top, &mut self.events);
        let title = p
            .items
            .iter()
            .filter(|i| i.checked)
            .map(|i| i.title.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        let text = plan::audit_text(&title, "");
        let text = Some(format!(
            "{}draft=#{}",
            text.map(|t| format!("{t}\n")).unwrap_or_default(),
            d.number
        ));
        self.audit_apply(ed, "user", Some(op_seq), &p.compiled, "ok", text, &by);
        *self.agent_batches.entry(actor.to_string()).or_default() += 1;
        self.agent_drafts.retain(|x| x.actor != actor);
        self.agent_send(ToWorker::AgentApplied {
            actor: actor.to_string(),
            number: d.number,
        });
        // a fresh copy of the live model: the chat's card and the other drafts compare to it
        self.send_sync(ed);
        // each other draft: only its items whose sets this Apply touched conflict
        let others: Vec<(String, u64)> = self
            .agent_drafts
            .iter()
            .map(|o| (o.actor.clone(), o.base_head()))
            .collect();
        for (actor, base) in others {
            let changes = ed.model.plan_changes(base);
            self.agent_send(ToWorker::AgentRebase { actor, changes });
        }
        Ok(op_seq)
    }

    /// The person's Discard: the draft goes, the live model never saw it.
    pub fn discard_agent_draft(&mut self, actor: &str) {
        self.agent_drafts.retain(|x| x.actor != actor);
        self.agent_send(ToWorker::AgentDiscard(actor.to_string()));
    }

    /// Compiles the draft again against the live model as it is now.
    pub fn recheck_agent_draft(&mut self, ed: &Editor, actor: &str) {
        if ed.model.plan_stale(self.head) || self.agent_draft_stale(ed, actor) {
            self.send_sync(ed);
        }
        self.agent_send(ToWorker::AgentRecheck(actor.to_string()));
    }

    /// Ticks on the draft card: "Apply selected" takes only the ticked operations.
    pub fn set_agent_checked(&mut self, actor: &str, list: Vec<(u32, bool)>) {
        self.agent_send(ToWorker::AgentChecked {
            actor: actor.to_string(),
            list,
        });
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn end(&mut self) {
        self.cancel.cancel();
        self.stop.cancel();
        self.send(ToWorker::End);
        self.alive = false;
    }

    /// Agent changes on the card, with the time left before they count as rejected.
    pub fn agent_waiting(&self) -> Option<super::agent_view::Waiting> {
        let p = self.plan.as_ref()?;
        super::agent_view::waiting_of(&p.items, p.expires_in, self.plan_at.elapsed())
    }

    pub fn can_apply(&self) -> bool {
        let Some(p) = &self.plan else {
            return false;
        };
        !p.blocked()
            && !p.main_gate_blocks()
            && p.ack == self.sent
            && !self.stale
            && !self.sync_out
            && !self.busy
            && !p.compiled.steps.is_empty()
            && p.unreviewed.is_empty()
            && self.redraft.apply_armed()
            && self.agent_free()
    }

    fn audit_apply(
        &self,
        ed: &Editor,
        actor: &str,
        op_seq: Option<i64>,
        plan: &CompiledPlan,
        status: &str,
        text: Option<String>,
        proposers: &[String],
    ) {
        let note = apply_note(proposers);
        let mut e = AuditEntry {
            text: match (text, note) {
                (Some(a), Some(b)) => Some(format!("{a}\n{b}")),
                (a, b) => a.or(b),
            },
            session: self.session.clone(),
            actor: actor.to_string(),
            event: "apply".to_string(),
            endpoint: Some(self.endpoint.clone()),
            model: Some(self.model_name.clone()),
            plan: Some(format!("steps={}", plan.steps.len())),
            op_seq,
            status: status.to_string(),
            ..AuditEntry::default()
        };
        if let Some(u) = self.usage {
            e.tok_in = u.turn.input_tokens;
            e.tok_out = u.turn.output_tokens;
            e.cost = u.cost;
        }
        audit_write(&ed.model, &e);
    }

    /// Revision of the card on show; bumped by every card the worker sends.
    pub fn plan_rev(&self) -> u64 {
        self.redraft.rev()
    }

    /// Apply of the card revision `rev` the person saw: a newer card arrived meanwhile is refused.
    pub fn apply_rev(&mut self, ed: &mut Editor, rev: u64) -> Result<i64, AiError> {
        if rev != self.redraft.rev() {
            return Err(AiError::ReviewNeeded);
        }
        self.apply(ed)
    }

    /// The person's Apply: one `run_op`, one undo step, nothing else.
    pub fn apply(&mut self, ed: &mut Editor) -> Result<i64, AiError> {
        let _s = ddc_model::perf::span("apply.total");
        let Some(p) = self.plan.clone() else {
            return Err(AiError::EmptyPlan);
        };
        if self.busy || p.ack != self.sent || self.sync_out || !self.agent_free() {
            return Err(AiError::Busy);
        }
        // a revision the person has not seen is not the one they meant to apply
        if !p.unreviewed.is_empty() || !self.redraft.apply_armed() {
            return Err(AiError::ReviewNeeded);
        }
        let by = plan::proposers(&p.items);
        if p.blocked() {
            self.audit_apply(ed, "user", None, &p.compiled, "blocked", None, &by);
            return Err(AiError::Blocked);
        }
        if p.main_gate_blocks() {
            self.audit_apply(ed, "user", None, &p.compiled, "draft_gate", None, &by);
            return Err(AiError::Blocked);
        }
        if self.model_epoch != crate::ai::mcp_bridge::model_epoch() {
            return Err(AiError::Internal("another project was opened".to_string()));
        }
        // read before the write: a failed read after the commit must not undo the bookkeeping
        let top = ed.model.op_top()?;
        let mut stamp: Option<(super::exec::CellValues, ddc_model::ValueSrc)> = None;
        let res = if self.stale {
            Err(AiError::Stale)
        } else {
            // checked against the base copy the card was diffed with; no copy: refused
            let base = self.paths.as_ref().map(|x| x.0.clone()).unwrap_or_default();
            let shown = super::apply_verify::Shown {
                base: &base,
                diff: &p.diff,
                truncated: p.truncated,
                limit: DIFF_LIMIT,
            };
            let audit = AuditEntry {
                session: self.session.clone(),
                actor: "user".to_string(),
                endpoint: Some(self.endpoint.clone()),
                model: Some(self.model_name.clone()),
                ..AuditEntry::default()
            };
            let ru = crate::ui_is_ru();
            // AI values start unverified; one clock read, so a redo records the same time
            let src = ddc_model::ValueSrc::ai(
                src_kind(&p.items),
                ddc_model::value_src::now_s(),
                &self.session,
                &format!("{}#{}", self.session, self.asks),
                &self.last_ask,
            );
            let before = super::exec::CellValues::read(&ed.model, &p.compiled.steps)?;
            stamp = Some((before, src.clone()));
            let merge = self.merge_of.clone();
            let (c, s) = (&p.compiled, Some(src));
            super::apply_verify::apply_verified_src(ed, c, merge, s, &shown, &audit, ru)
        };
        match res {
            Ok(None) => {}
            Ok(Some(e)) => self.events.push_back(UiEvent::Failed(e)),
            Err(AiError::Diverged(t)) => {
                // the draft stays; a new copy re-previews it against the model as it is now
                self.stale = true;
                self.send_sync(ed);
                return Err(AiError::Diverged(t));
            }
            Err(AiError::Stale) => {
                self.audit_apply(ed, "user", None, &p.compiled, "stale", None, &by);
                self.stale = true;
                self.send_sync(ed);
                return Err(AiError::Stale);
            }
            Err(e) => {
                self.audit_apply(ed, "user", None, &p.compiled, "failed", None, &by);
                return Err(e);
            }
        }
        let op_seq = applied_seq(ed.model.op_top(), top, &mut self.events);
        let title = p
            .items
            .iter()
            .filter(|i| i.checked)
            .map(|i| i.title.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        let why = p
            .items
            .iter()
            .filter(|i| i.checked && !i.why.is_empty())
            .map(|i| i.why.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        let text = plan::audit_text(&title, &why);
        self.audit_apply(ed, "user", Some(op_seq), &p.compiled, "ok", text, &by);
        self.head = ed.model.change_head();
        // a failed read leaves the copy short of the records: a fresh copy replaces it
        let stamped = stamp.and_then(|(before, src)| match before.changed(&ed.model) {
            Ok(cells) => (!cells.is_empty()).then_some((cells, src)),
            Err(_) => {
                self.force_sync = true;
                None
            }
        });
        self.send(ToWorker::Applied {
            head: self.head,
            op_seq,
            stamped,
        });
        self.plan = None;
        // the next draft is a first draft, not a revision of the applied one
        self.redraft.clear();
        self.track.clear();
        self.stale = false;
        // the merge mark went in with the Apply's journal entry
        self.merge_of = None;
        Ok(op_seq)
    }

    fn send_sync(&mut self, ed: &Editor) {
        match source_of(ed) {
            Ok(source) => {
                self.sync_out = true;
                self.force_sync = false;
                self.next_sync = Some(Instant::now() + SYNC_GAP);
                self.send(ToWorker::Sync {
                    source,
                    head: ed.model.change_head(),
                    changes: Some(ed.model.plan_changes(self.head)),
                });
            }
            Err(e) => {
                self.next_sync = Some(Instant::now() + SYNC_RETRY);
                self.events.push_back(UiEvent::Failed(e));
            }
        }
    }

    pub fn pump_core(&mut self, ed: &mut Editor) {
        // Save As moves the project: agents must see the new file at once
        mcp_info::set_project(ed.project.path());
        loop {
            match self.rx.try_recv() {
                Ok(m) => self.on_message(m, ed),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if self.alive {
                        self.alive = false;
                        self.busy = false;
                        self.sync_out = false;
                        self.agent_out = 0;
                        self.agent_acked = self.agent_sent;
                        self.events.push_back(UiEvent::Failed(AiError::Internal(
                            crate::jobs::internal_error(),
                        )));
                    }
                    break;
                }
            }
        }
        self.resync(ed);
        if !self.alive {
            return;
        }
        let moved = ed.model.plan_stale(self.head);
        self.stale = moved
            || self
                .plan
                .as_ref()
                .is_some_and(|p| ed.model.plan_stale(p.compiled.base_head));
        let due = self.next_sync.is_none_or(|t| Instant::now() >= t);
        let wanted =
            self.force_sync || (moved
                && (self.plan.is_some() || self.busy || !self.agent_drafts.is_empty()));
        if wanted && !self.sync_out && due {
            self.send_sync(ed);
        }
    }

    fn on_message(&mut self, m: FromWorker, ed: &mut Editor) {
        match m {
            FromWorker::Text(t) => self.events.push_back(UiEvent::Text(t)),
            FromWorker::Tool { name, ok, err } => {
                // the model got the refusal as JSON; the person reads it in the UI language
                let code = err.as_deref().and_then(super::refusals::code_of);
                let err = err.map(|e| super::refusals::human(&e));
                self.events.push_back(UiEvent::Tool { name, ok, err, code })
            }
            FromWorker::Plan(v) => {
                self.acked = v.ack;
                self.plan_at = Instant::now();
                if v.items.is_empty() && self.plan.is_some() && self.redraft.showing() {
                    self.events.push_back(UiEvent::DraftEmptied);
                }
                let keyed = crate::ai::ui::plan_delta::keyed(&v.items, &v.diff);
                let mine = self.track.take_own(v.ack);
                self.redraft.adopt(&keyed, &mine);
                self.track.on_plan(&keyed);
                let fixes = self.redraft.on_plan(&keyed);
                self.plan = if v.items.is_empty() { None } else { Some(v) };
                if !fixes.is_empty() {
                    self.set_checked(fixes);
                }
            }
            FromWorker::Usage(u) => {
                self.usage = Some(u);
                self.events.push_back(UiEvent::Usage(u));
            }
            FromWorker::Step(r) => self.events.push_back(UiEvent::Step(r)),
            FromWorker::Sent(s) => self.events.push_back(UiEvent::Sent(SentEntry::now(
                &self.endpoint,
                &self.model_name,
                &s,
            ))),
            FromWorker::Audit(e) => {
                if e.event == "reject" && e.status == "timeout" && e.actor.starts_with(ACTOR_PREFIX) {
                    self.events.push_back(UiEvent::AgentExpired);
                }
                audit_write(&ed.model, &e);
            }
            FromWorker::Failed(e) => self.events.push_back(UiEvent::Failed(e)),
            FromWorker::Idle => self.busy = false,
            FromWorker::AutoTurn => self.busy = true,
            FromWorker::TurnEnd(end) => self.events.push_back(UiEvent::TurnDone(end)),
            FromWorker::Ready { base, work } => self.paths = Some((base, work)),
            FromWorker::Preparing { done, total } => {
                self.events.push_back(UiEvent::Preparing { done, total })
            }
            FromWorker::Synced { head } => {
                self.head = head;
                if let Some(p) = self.plan.as_ref().filter(|_| self.sync_out) {
                    // the recompiled card follows this message: its rows are compared with these
                    self.track.on_synced(crate::ai::ui::plan_delta::keyed(&p.items, &p.diff));
                    self.events.push_back(UiEvent::Replanned);
                }
                self.sync_out = false;
            }
            FromWorker::NeedSync => {
                self.sync_out = false;
                self.next_sync = Some(Instant::now() + SYNC_RETRY);
                self.force_sync = true;
            }
            FromWorker::Snapshot { max_px, reply } => self.snapshots.push_back((max_px, reply)),
            FromWorker::AgentDrafts { list, ack } => {
                self.agent_acked = ack;
                // a picture older than the person's last Apply or Discard would bring it back
                if ack == self.agent_sent {
                    self.agent_drafts = list;
                }
            }
            FromWorker::AgentApplyAsked { actor } => {
                self.events.push_back(UiEvent::AgentApplyAsked(actor))
            }
            FromWorker::Proposals(list) => self.proposals = list,
            FromWorker::ProposalTaken {
                id,
                envelope,
                base_head,
            } => self.on_proposal_taken(ed, id, *envelope, base_head),
            FromWorker::ProposalRefused { id, code } => {
                if self.proposal_out == Some(id) {
                    self.proposal_out = None;
                }
                let result = Err(AiError::Model(code));
                self.events.push_back(UiEvent::ProposalApplied {
                    id,
                    result,
                    reload_error: None,
                });
            }
            FromWorker::ValueRefused { item, why } => {
                self.events.push_back(UiEvent::ValueRefused { item, why })
            }
            FromWorker::McpDone => self.agent_out = self.agent_out.saturating_sub(1),
            FromWorker::RuleDraft { rule, text } => {
                self.events.push_back(UiEvent::RuleDraft { rule, text })
            }
            FromWorker::IssueDraft(t) => self.events.push_back(UiEvent::IssueDraft(t)),
            FromWorker::ExportOffer {
                cat,
                filter,
                columns,
                group,
            } => self.events.push_back(UiEvent::ExportOffer {
                cat,
                filter,
                columns,
                group,
            }),
            FromWorker::View(cmd) => self.events.push_back(UiEvent::View(cmd)),
            FromWorker::Found(f) => self.events.push_back(UiEvent::Found(f)),
            FromWorker::ScheduleMade(m) => self.events.push_back(UiEvent::ScheduleMade(m)),
            FromWorker::FamilyDraft {
                name,
                text,
                base_family,
                repair,
                queued,
            } => self.events.push_back(UiEvent::FamilyDraft {
                name,
                text,
                base_family,
                repair,
                queued,
            }),
            FromWorker::Req(offer) => self.events.push_back(UiEvent::Req(offer)),
        }
    }

    /// For tests and for shutting down: pumps until the worker has answered everything sent.
    pub fn wait_idle(&mut self, ed: &mut Editor, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        loop {
            self.pump_core(ed);
            let settled = !self.busy
                && !self.sync_out
                && self.acked == self.sent
                && !self.agent_active()
                && self.agent_acked == self.agent_sent
                && self.proposal_out.is_none();
            if settled && !(self.plan.is_some() && self.stale) {
                return true;
            }
            if !self.alive || Instant::now() > until {
                return false;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for UiSession {
    fn drop(&mut self) {
        // past the wait the worker is left alone: it lands nothing and removes its files itself
        self.finish(JOIN_WAIT);
    }
}

struct ShotTag;

struct PendingShot {
    max_px: u32,
    reply: mpsc::Sender<Result<ImageData, String>>,
}

thread_local! {
    static SESSION: RefCell<Option<UiSession>> = const { RefCell::new(None) };
    static VIEW_RECT: Cell<Option<egui::Rect>> = const { Cell::new(None) };
    static SHOTS: RefCell<Vec<PendingShot>> = const { RefCell::new(Vec::new()) };
}

/// The session of this UI thread; the panel starts it, reads it and ends it through here.
pub fn with_session<R>(f: impl FnOnce(&mut Option<UiSession>) -> R) -> R {
    SESSION.with(|s| f(&mut s.borrow_mut()))
}

/// The panel reports where the plan or 3D view is drawn: the snapshot shows that rectangle.
pub fn set_view_rect(r: Option<egui::Rect>) {
    VIEW_RECT.with(|v| v.set(r));
}

/// The canvas rectangle of the last painted frame on this thread.
pub fn view_rect() -> Option<egui::Rect> {
    VIEW_RECT.with(Cell::get)
}

/// Once a frame on the UI thread.
pub fn pump(app: &mut crate::shell::EditorApp, ctx: &egui::Context) {
    let requests = SESSION.with(|s| {
        let mut s = s.borrow_mut();
        let Some(u) = s.as_mut() else {
            return vec![];
        };
        let _ = u.wake.get_or_init(|| ctx.clone());
        u.pump_core(&mut app.ed);
        if u.is_busy() || u.agent_active() {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
        u.take_snapshot_requests()
    });
    if !requests.is_empty() {
        SHOTS.with(|q| {
            q.borrow_mut().extend(
                requests
                    .into_iter()
                    .map(|(max_px, reply)| PendingShot { max_px, reply }),
            )
        });
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
            ShotTag,
        )));
        ctx.request_repaint();
    }
    let image = ctx.input(|i| {
        i.raw.events.iter().find_map(|e| match e {
            egui::Event::Screenshot {
                user_data, image, ..
            } if user_data
                .data
                .as_ref()
                .is_some_and(|d| d.downcast_ref::<ShotTag>().is_some()) =>
            {
                Some(image.clone())
            }
            _ => None,
        })
    });
    let Some(image) = image else {
        return;
    };
    let ppp = ctx.pixels_per_point();
    let rect = VIEW_RECT.with(Cell::get).map(|r| {
        [
            (r.min.x * ppp) as usize,
            (r.min.y * ppp) as usize,
            (r.width() * ppp) as usize,
            (r.height() * ppp) as usize,
        ]
    });
    for shot in SHOTS.with(|q| std::mem::take(&mut *q.borrow_mut())) {
        let _ = shot.reply.send(shot_data(&image, rect, shot.max_px));
    }
}

fn shot_data(
    image: &egui::ColorImage,
    rect: Option<[usize; 4]>,
    max_px: u32,
) -> Result<ImageData, String> {
    let (rgba, width, height) = crop_scale(image, rect, max_px);
    if width == 0 || height == 0 {
        return Err("empty view".to_string());
    }
    let bytes = encode_png(&rgba, width, height)?;
    Ok(ImageData {
        mime: "image/png",
        bytes: Arc::new(bytes),
        width,
        height,
    })
}

/// Crops to `[x, y, w, h]` in pixels and shrinks so the longer side fits `max_px`.
fn crop_scale(
    image: &egui::ColorImage,
    rect: Option<[usize; 4]>,
    max_px: u32,
) -> (Vec<u8>, u32, u32) {
    let [full_w, full_h] = image.size;
    let [x0, y0, w, h] = rect.unwrap_or([0, 0, full_w, full_h]);
    let x0 = x0.min(full_w);
    let y0 = y0.min(full_h);
    let w = w.min(full_w - x0);
    let h = h.min(full_h - y0);
    if w == 0 || h == 0 {
        return (vec![], 0, 0);
    }
    let longest = w.max(h);
    let limit = (max_px.max(1) as usize).min(longest);
    let out_w = (w * limit / longest).max(1);
    let out_h = (h * limit / longest).max(1);
    let mut rgba = Vec::with_capacity(out_w * out_h * 4);
    for oy in 0..out_h {
        let sy = y0 + oy * h / out_h;
        for ox in 0..out_w {
            let sx = x0 + ox * w / out_w;
            rgba.extend_from_slice(&image.pixels[sy * full_w + sx].to_array());
        }
    }
    (rgba, out_w as u32, out_h as u32)
}

pub(crate) fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Caps and confirms follow the session's own target: only a draft skips the main model's caps.
fn gate_issues(
    caps: &validate::PlanCaps,
    target: catalog::Target,
    plan: &PlanState,
    diff: &[plan::ElemDiff],
) -> Vec<PlanIssue> {
    validate::plan_issues_with(
        caps,
        &plan.items,
        validate::PlanSize::of(diff),
        target,
        plan.allow_large,
    )
}

/// A draft plan is gated as a draft, but Apply lands in the main model: its caps are shown too.
fn main_issues_of(
    caps: &validate::PlanCaps,
    target: catalog::Target,
    plan: &PlanState,
    diff: &[plan::ElemDiff],
) -> Vec<PlanIssue> {
    if target == catalog::Target::Main {
        return vec![];
    }
    gate_issues(caps, catalog::Target::Main, plan, diff)
}

/// The journal seq of a committed Apply. A failed read after the commit settles it as applied
/// with the view not refreshed (the step is `top + 1`), never as a failed Apply.
thread_local! {
    /// Log writes that failed since the panel last looked: (an applied change, anything else).
    static AUDIT_LOST: Cell<(bool, bool)> = const { Cell::new((false, false)) };
}

/// Writes one entry of the assistant log. A failure is kept for the panel's warning, so an
/// Apply never lands silently without its entry; the change itself stays.
pub fn audit_write(m: &Model, e: &AuditEntry) -> bool {
    match m.audit_append(e) {
        Ok(_) => true,
        Err(err) => {
            eprintln!("assistant audit: {}", ddc_ai::secret::scrub_known(&err.to_string()));
            let applied = e.event == "apply" && e.status.starts_with("ok");
            AUDIT_LOST.with(|c| {
                let (a, o) = c.get();
                c.set((a || applied, o || !applied));
            });
            false
        }
    }
}

/// What failed to reach the log since the last call, and forgets it.
pub fn take_audit_lost() -> (bool, bool) {
    AUDIT_LOST.with(|c| c.replace((false, false)))
}

fn applied_seq(
    read: Result<i64, ddc_model::Error>,
    top: i64,
    events: &mut VecDeque<UiEvent>,
) -> i64 {
    match read {
        Ok(seq) => seq,
        Err(e) => {
            events.push_back(UiEvent::Failed(AiError::AppliedNotRefreshed(e.to_string())));
            top + 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_top_error_after_commit_settles_as_applied() {
        let mut ev = VecDeque::new();
        assert_eq!(applied_seq(Ok(9), 4, &mut ev), 9);
        assert!(ev.is_empty(), "a good read says nothing");
        let read = Err(ddc_model::Error::Invalid("disk read".into()));
        assert_eq!(applied_seq(read, 4, &mut ev), 5, "the committed step, not a failure");
        assert_eq!(ev.len(), 1, "said once");
        match ev.pop_front() {
            Some(UiEvent::Failed(AiError::AppliedNotRefreshed(t))) => {
                assert!(t.contains("disk read"), "{t}")
            }
            other => panic!("expected applied, not refreshed: {other:?}"),
        }
    }

    #[test]
    fn every_catalog_tool_has_one_handler_and_back() {
        for s in catalog::catalog() {
            assert!(handler(s.name).is_some(), "no handler for {}", s.name);
        }
        for (name, _) in tools_read::HANDLERS
            .iter()
            .chain(tools_write::HANDLERS.iter())
            .chain(super::super::norm_tools::HANDLERS.iter())
            .chain(super::super::plant_tools::HANDLERS.iter())
        {
            assert!(
                catalog::find(name).is_some(),
                "{name} is not in the catalog"
            );
        }
    }

    #[test]
    fn handlers_match_the_declared_access() {
        for (name, _) in tools_read::HANDLERS
            .iter()
            .chain(super::super::norm_tools::HANDLERS.iter())
        {
            assert!(
                matches!(catalog::find(name).unwrap().access, Access::Read),
                "{name}"
            );
        }
        for (name, _) in tools_write::HANDLERS.iter().chain(super::super::plant_tools::HANDLERS.iter()) {
            assert!(
                !matches!(catalog::find(name).unwrap().access, Access::Read),
                "{name}"
            );
        }
    }

    fn image(w: usize, h: usize) -> egui::ColorImage {
        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                pixels.push(egui::Color32::from_rgb(x as u8, y as u8, 0));
            }
        }
        egui::ColorImage {
            size: [w, h],
            source_size: egui::vec2(w as f32, h as f32),
            pixels,
        }
    }

    #[test]
    fn crop_takes_the_view_rectangle() {
        let (rgba, w, h) = crop_scale(&image(10, 10), Some([2, 3, 4, 5]), 1280);
        assert_eq!((w, h), (4, 5));
        assert_eq!(&rgba[..4], &[2, 3, 0, 255]);
    }

    #[test]
    fn scale_fits_the_longer_side_and_keeps_shape() {
        let (rgba, w, h) = crop_scale(&image(200, 100), None, 50);
        assert_eq!((w, h), (50, 25));
        assert_eq!(rgba.len(), 50 * 25 * 4);
    }

    #[test]
    fn a_rectangle_outside_the_image_is_empty() {
        assert_eq!(crop_scale(&image(4, 4), Some([9, 9, 3, 3]), 100).1, 0);
    }

    #[test]
    fn session_passes_core_target() {
        let n = validate::LARGE_PLAN + 1;
        let diff: Vec<plan::ElemDiff> = (0..n)
            .map(|i| plan::ElemDiff {
                key: ("walls".to_string(), i as i64),
                kind: ddc_model::RowKind::Changed,
                item: Some(1),
                level: None,
                label: String::new(),
                cells: vec![],
            })
            .collect();
        let mut st = PlanState::default();
        assert!(gate_issues(&validate::PlanCaps::default(), catalog::Target::Draft, &st, &diff).is_empty());
        assert!(!gate_issues(&validate::PlanCaps::default(), catalog::Target::Main, &st, &diff).is_empty());
        st.allow_large = true;
        assert!(gate_issues(&validate::PlanCaps::default(), catalog::Target::Main, &st, &diff).is_empty());
    }

    fn gated_view(gated: catalog::Target, n: usize, compiled: CompiledPlan) -> PlanView {
        let diff: Vec<plan::ElemDiff> = (0..n)
            .map(|i| plan::ElemDiff {
                key: ("walls".to_string(), i as i64),
                kind: ddc_model::RowKind::Changed,
                item: Some(1),
                level: None,
                label: String::new(),
                cells: vec![],
            })
            .collect();
        let st = PlanState::default();
        PlanView {
            items: vec![],
            issues: gate_issues(&validate::PlanCaps::default(), gated, &st, &diff),
            main_issues: main_issues_of(&validate::PlanCaps::default(), gated, &st, &diff),
            diff,
            secondary: 0,
            qty: vec![],
            preview: Arc::new(preview::PreviewSet::default()),
            compiled,
            stale: false,
            variants: vec![],
            variant: 0,
            truncated: false,
            flagged: 0,
            ack: 0,
            gated,
            allow_large: st.allow_large,
            unreviewed: vec![],
            expires_in: None,
            deliver: None,
        }
    }

    fn no_steps(ed: &Editor) -> CompiledPlan {
        CompiledPlan {
            base_head: ed.model.change_head(),
            steps: Arc::new(vec![]),
        }
    }

    #[test]
    fn draft_plan_over_cap_refused_on_main_apply() {
        let ed = Editor::new_in_memory().unwrap();
        let mut v = gated_view(
            catalog::Target::Draft,
            validate::LARGE_PLAN + 1,
            no_steps(&ed),
        );
        assert!(!v.blocked(), "the draft gate let it through");
        assert!(v.main_gate_blocks(), "the main model's caps refuse it");
        let mut st = PlanState::default();
        st.allow_large = true;
        v.main_issues = main_issues_of(&validate::PlanCaps::default(), catalog::Target::Draft, &st, &v.diff);
        assert!(!v.main_gate_blocks(), "the person's confirm still counts");
    }

    #[test]
    fn draft_plan_over_cap_shows_allow_large() {
        let ed = Editor::new_in_memory().unwrap();
        let v = gated_view(
            catalog::Target::Draft,
            validate::LARGE_PLAN + 1,
            no_steps(&ed),
        );
        assert!(v.issues.is_empty(), "the draft gate itself is silent");
        assert!(v
            .main_issues
            .iter()
            .any(|i| i.text_key == "ai.issue.too_large"));
        assert!(super::super::ui::logic::allow_large_offer(
            &v.issues,
            &v.main_issues,
            v.allow_large
        ));
    }

    #[test]
    fn can_apply_false_for_draft_plan_over_cap() {
        let ed = Editor::new_in_memory().unwrap();
        let mut v = gated_view(
            catalog::Target::Draft,
            validate::LARGE_PLAN + 1,
            no_steps(&ed),
        );
        assert!(v.main_gate_blocks(), "over the cap: Apply must be off");
        // the toggle republishes a view gated with the confirm: nothing stale to invalidate
        let mut st = PlanState::default();
        st.allow_large = true;
        v.allow_large = true;
        v.main_issues = main_issues_of(&validate::PlanCaps::default(), catalog::Target::Draft, &st, &v.diff);
        assert!(!v.main_gate_blocks());
        let small = gated_view(catalog::Target::Draft, 1, no_steps(&ed));
        assert!(!small.main_gate_blocks() && small.main_issues.is_empty());
    }

    #[test]
    fn draft_plan_within_cap_applies_one_undo() {
        let mut ed = Editor::new_in_memory().unwrap();
        let top = ed.model.op_top().unwrap();
        let next: i64 = ed
            .model
            .sql()
            .query_row("SELECT coalesce(max(id),0)+1 FROM wall", [], |r| r.get(0))
            .unwrap();
        let spec = ddc_model::WallSpec {
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            level: ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ed.types[0].id,
            offset0: None,
        };
        let compiled = CompiledPlan {
            base_head: ed.model.change_head(),
            steps: Arc::new(vec![super::super::ModelStep::Walls {
                specs: vec![spec],
                expect: vec![next],
            }]),
        };
        let v = gated_view(catalog::Target::Draft, 1, compiled);
        assert!(!v.blocked() && !v.main_gate_blocks());
        super::super::exec::apply_checked(&mut ed, &v.compiled).unwrap();
        assert_eq!(ed.model.op_top().unwrap(), top + 1, "one undo step");
        ed.undo();
        assert_eq!(ed.model.op_top().unwrap(), top);
    }

    #[test]
    fn main_plan_gates_unchanged() {
        let ed = Editor::new_in_memory().unwrap();
        let v = gated_view(
            catalog::Target::Main,
            validate::LARGE_PLAN + 1,
            no_steps(&ed),
        );
        assert!(v.blocked(), "main caps block it as before");
        assert!(!v.main_gate_blocks(), "no second gate for a main plan");
        let small = gated_view(catalog::Target::Main, 1, no_steps(&ed));
        assert!(!small.blocked() && !small.main_gate_blocks());
    }

    #[test]
    fn value_edit_on_conflicted_item_keeps_old_conflict_only() {
        let old = "variant changed: depended on item 3";
        // the item's own conflict, unchanged by the value: the value is taken
        assert_eq!(value_refusal(Ok(()), Some(old), Some(old)), None);
        // the value cleared it: taken
        assert_eq!(value_refusal(Ok(()), Some(old), None), None);
        // a new conflict, or another one than before: refused with that reason
        let new = "the wall overlaps item 2";
        assert_eq!(value_refusal(Ok(()), None, Some(new)), Some(new.to_string()));
        assert_eq!(value_refusal(Ok(()), Some(old), Some(new)), Some(new.to_string()));
        // a value that does not compile is refused whatever the item had
        let err = Err("zero length".to_string());
        assert_eq!(value_refusal(err, Some(old), Some(old)), Some("zero length".to_string()));
        // the refusal says what to do next, in both languages
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let line = cat.tf("ai.value.refused", &[("why", new)]);
            assert!(line.contains(new), "{lang:?}: {line}");
            let next = cat.t("ai.value.refused_next");
            assert!(!next.is_empty() && next != "ai.value.refused_next", "{lang:?}");
            let ru = next.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c));
            assert_eq!(ru, lang == ddc_ui::Lang::Ru, "{lang:?}: {next}");
        }
    }

    #[test]
    fn png_has_the_signature() {
        let bytes = encode_png(&[0, 0, 0, 255], 1, 1).unwrap();
        assert_eq!(&bytes[..4], &[0x89, b'P', b'N', b'G']);
    }
}
