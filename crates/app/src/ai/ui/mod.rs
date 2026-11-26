// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The assistant's side panel. Frames draw from state and push `Action`s; the actions run after the
//! draw with the whole app in hand, so no widget ever holds the model across a change.

pub mod agent_draft_card;
pub mod attach_ui;
pub mod audit_ui;
pub mod budget;
pub mod chat;
pub mod point_marks;
pub mod checks;
pub mod compare;
pub mod compare_logic;
pub mod connect;
pub mod consent;
pub mod dock_fold;
pub mod ctx_chips;
pub mod demo;
pub mod depth_chip;
pub mod found;
pub mod keys;
pub mod kit;
pub mod logic;
pub mod md;
pub mod modes;
pub mod modes_view;
pub mod norms;
pub mod palette_rows;
pub mod plan_card;
pub mod plan_delta;
pub mod proposal_card;
pub mod proposal_draft;
pub mod receipt;
pub mod recall;
pub mod requirements;
pub mod retry;
pub mod review;
pub mod sent;
pub mod settings;
pub mod stream_status;
pub mod steps;
pub mod value_row;
#[cfg(test)]
pub(crate) mod tests_card_keys;
#[cfg(test)]
pub(crate) mod tests_states;
#[cfg(test)]
mod tests_draft_contract;
#[cfg(test)]
pub(crate) mod tests_modes;

use super::plan::PlanView;
use super::preview;
use super::session::{set_view_rect, with_session, UiEvent, ViewCtx};
use super::AiError;
use crate::editor::OpFn;
use crate::shell::EditorApp;
use connect::Gate;
use ddc_ai::{Mode, Profile, TurnEnd, UsageTotals};
use ddc_geom::v2;
use ddc_model::checks::RuleDef;
use ddc_model::{AuditEntry, ElKey, Model};
use ddc_render::Aabb;
use ddc_ui::i18n::Catalog;
use ddc_ui::notify::Level;
use ddc_ui::Theme;
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub(crate) const PANEL_W: (f32, f32) = (280.0, 640.0);

/// The dock frame: cards and rows keep a clear gutter from the window edge on both sides.
fn panel_frame(th: &Theme) -> egui::Frame {
    egui::Frame::new()
        .fill(th.c("bg.panel"))
        .inner_margin(egui::Margin::symmetric(kit::space::MD as i8, kit::space::SM as i8))
}

const WORKER_STOPPED: &str = "assistant worker stop";
/// Entries of the visible chat kept in memory and in the project file.
const LOG_KEPT: usize = ddc_model::CHAT_ROWS;
/// Notes that survive a reopen; a stored key outside this list is dropped on load.
const NOTE_KEYS: [&str; 8] = [
    "ai.state.cancelled",
    "ai.redraft.emptied",
    "ai.proposal.applied",
    "ai.proposal.applied_resync",
    "ai.proposal.stale",
    "ai.plan.applied",
    "ai.plan.rejected",
    "ai.proposal.rejected",
];

/// Text, note, receipt and tool-step entries as (kind, text) rows; other cards are not kept.
/// A long run of steps keeps at most `steps::SAVED_PER_RUN`, so tool calls never crowd out
/// the conversation in the capped history.
fn chat_rows(log: &[Entry]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < log.len() {
        let run: Vec<&steps::Step> = log[i..]
            .iter()
            .map_while(|e| match e {
                Entry::Tool(s) => Some(s),
                _ => None,
            })
            .collect();
        if !run.is_empty() {
            let kept = steps::saved(&run);
            out.extend(kept.into_iter().map(|k| ("tool".to_string(), run[k].to_row())));
            i += run.len();
            continue;
        }
        let row = match &log[i] {
            Entry::User(t) => Some(("user".to_string(), t.clone())),
            Entry::Assistant(t) => Some(("assistant".to_string(), t.clone())),
            Entry::Note(k) => Some(("note".to_string(), (*k).to_string())),
            Entry::Applied(r) => Some(("applied".to_string(), r.to_row())),
            Entry::Redraft(n) => Some(("redraft".to_string(), n.to_row())),
            _ => None,
        };
        out.extend(row);
        i += 1;
    }
    out
}

fn chat_entries(rows: Vec<(String, String)>) -> Vec<Entry> {
    rows.into_iter()
        .filter_map(|(kind, text)| match kind.as_str() {
            "user" => Some(Entry::User(text)),
            "assistant" => Some(Entry::Assistant(text)),
            "note" => NOTE_KEYS.iter().find(|k| **k == text).map(|k| Entry::Note(*k)),
            "applied" => receipt::Receipt::from_row(&text).map(Entry::Applied),
            "tool" => steps::Step::from_row(&text).map(Entry::Tool),
            "redraft" => plan_delta::Note::from_row(&text).map(Entry::Redraft),
            _ => None,
        })
        .collect()
}

/// One message or card in the dialog.
pub enum Entry {
    User(String),
    Assistant(String),
    /// One tool call: state, duration and, when it failed, the reason.
    Tool(steps::Step),
    /// An i18n key.
    Note(&'static str),
    /// What an Apply changed, with Show and Undo.
    Applied(receipt::Receipt),
    /// The person acknowledged a revised draft: its counts.
    Redraft(plan_delta::Note),
    Export {
        cat: String,
        filter: Option<String>,
        columns: Vec<String>,
        group: Option<String>,
        saved: bool,
    },
    Rule {
        def: RuleDef,
        text: String,
        saved: bool,
    },
    /// Elements a read found or a view tool changed: "N of M" with Show, Isolate and Select.
    Found(crate::ai::view_tools::Found),
    /// A schedule the assistant made: opens as a live, editable schedule view.
    Schedule {
        made: crate::ai::sched_tool::Made,
        saved: bool,
    },
    /// A drafted issue waiting for the person; `saved` holds the new issue id.
    Issue {
        topic: Box<ddc_model::Topic>,
        saved: Option<i64>,
    },
    /// Requirements to save, or a report to export; `done` once the person did.
    Req {
        offer: super::session::ReqOffer,
        done: bool,
    },
}

/// What the frame decided; run after the draw.
pub enum Action {
    Send(String, Mode),
    Retry,
    /// Skip the automatic wait and resend now.
    RetryNow,
    /// Stop the automatic resends of this prompt.
    RetryCancel,
    /// Context error: take the prompt back into the composer, without the selection.
    Shorten,
    Apply,
    /// The person looked at the agent's items on the card.
    Review,
    Reject,
    Cancel,
    SetChecked(Vec<(u32, bool)>),
    SetVariant(u32),
    AllowLarge(bool),
    /// Pick a picture or a PDF to compare the draft with.
    AttachReference,
    /// Pick files to attach to the chat.
    AttachFiles,
    /// Files dropped on the chat or paths pasted into the composer.
    AttachPaths(Vec<std::path::PathBuf>),
    /// An image pasted into the composer.
    AttachClipboardImage,
    AttachRemove(u32),
    /// Share a file with the model, or keep it on this machine.
    AttachShare(u32),
    AttachPreview(Option<u32>),
    /// Place an attached drawing as an underlay on the current level.
    AttachUnderlay(u32),
    /// The person confirmed the project requirements card.
    ReqConfirm,
    /// A drafted value corrected on the card: the draft changes, the model does not.
    SetValue {
        item: u32,
        field: crate::ai::value_edit::Field,
        value: crate::ai::value_edit::Value,
    },
    Zoom(Vec<ElKey>),
    /// The selection chip was clicked: bring the selected elements into view.
    FrameSelection,
    ZoomAll,
    SelectEl(ElKey),
    /// The receipt's Show: select and frame what the apply touched.
    ShowApplied(Vec<ElKey>),
    /// The receipt's Undo (or the toast's) for the apply that wrote undo-log row `op_seq`.
    UndoApplied(i64),
    /// "Roll back to here": the model as it was before the apply with undo-log row `op_seq`.
    RollbackApplied(i64),
    /// "Make working": the draft families the apply with undo-log row `op_seq` wrote.
    PromoteFamilies(i64),
    ConsentProject {
        snapshots: bool,
    },
    ConsentOnce {
        snapshots: bool,
    },
    ConsentNo,
    SetProfile(usize),
    OpenSettings,
    /// Not connected: answer an example by hand with this app command (schedule, checks…).
    Offline(&'static str),
    OpenSent,
    /// Show or hide the step feed of the last request.
    ToggleJournal,
    /// Save the last request's journal (steps, tokens, cost, time) as JSON.
    ExportJournal,
    Export(usize),
    OpenSchedule(usize),
    ExportSchedule(usize),
    SaveRule(usize),
    SaveIssue(usize),
    NewIssue(Box<crate::issues_ui::Draft>),
    /// The guard is fixed when a provider is built: policy changes need a new session.
    RestartSession,
    ProfilesSaved(Vec<Profile>, usize),
    ClosePanel,
    OpenPanel,
    AskFix(String),
    McpConfirm(bool),
    McpRevoke,
    UndoAudit(i64),
    /// Takes back an older journal entry alone, if no newer change touched its rows. Runs only
    /// as the yes to the question `AskRevert` put up for the same row.
    RevertAudit(i64),
    /// Puts up the revert question (or a warning when the step cannot go back alone).
    AskRevert(i64),
    CancelRevert,
    /// Selects and frames the elements a journal row's step touched.
    ShowAudit(i64),
    McpServer,
    /// The depth chip changed the project setting: one undo step.
    SetDepth(ddc_model::DepthSettings),
    /// The depth heat map on or off.
    DepthHeat(bool),
    SetPropose(bool),
    /// Centre the plan on a finding's point.
    Center([f64; 3]),
    /// An option from the comparison view: runs through the Apply path, never a direct write.
    ApplyOption(i64),
    /// Rejects a draft option; its row stays in the table.
    DiscardOption(i64),
    /// An agent's `ops_propose` card: Apply runs it as one undo step, Reject drops it.
    ApplyProposal(u32),
    RejectProposal(u32),
    /// A proposal row or chip: select and frame those live elements; nothing is written.
    ShowDraft(Vec<ElKey>),
    /// A result card's button: the view changes, the model does not; Reset view takes it back.
    FoundView(crate::ai::view_tools::ViewCmd),
    /// An external agent's draft, revision `rev` as the person saw it: one undo step.
    ApplyAgentDraft {
        actor: String,
        rev: u64,
    },
    DiscardAgentDraft(String),
    /// The model moved since the draft was compiled: compile it again before Apply.
    RecheckAgentDraft(String),
    /// Ticks on the draft card; Apply takes only the ticked operations.
    SetAgentChecked {
        actor: String,
        list: Vec<(u32, bool)>,
    },
    /// Information requirements: load a book, save the template, export, save an offer.
    Req(requirements::Do),
    /// Norm checks: export the findings on show.
    Norms(norms::Do),
    /// An app command from an AI tab (the build tab's drawings and files).
    Command(&'static str),
    /// The AI tab's preview pane shows plan, 3D or the first sheet (its id).
    Preview(modes::Preview, Option<i64>),
    /// A finding explained in an answers-only turn; nothing is drafted.
    AskExplain(String),
    /// The last request back in the composer to change and send again; the draft stays.
    EditRequest,
    /// The review tab with the model checks and the norm checks run on the live model.
    CheckInReview,
}

/// What the frame needs of the session, read once per frame.
#[derive(Clone, Default)]
pub struct Facts {
    pub plan: Option<Arc<PlanView>>,
    pub has_session: bool,
    pub busy: bool,
    pub stale: bool,
    pub can_apply: bool,
    /// What a follow-up changed on the card since the last look; None when nothing did.
    pub redraft: Option<plan_delta::PlanDelta>,
    /// Rows the recompile against the changed live model touched; None when it touched none.
    pub rebased: Option<plan_delta::PlanDelta>,
    /// Revision of the card drawn this frame: Apply and "Got it" name it.
    pub plan_rev: u64,
    pub proposals: Vec<crate::ai::session::ProposalCard>,
    pub proposal_applying: Option<u32>,
    /// External agents' drafts, one per connection; only the person's Apply takes one in.
    pub agents: Vec<Arc<crate::ai::session::AgentDraftView>>,
    /// Nothing in flight that a draft's Apply would wait for.
    pub agent_ready: bool,
}

impl Facts {
    fn read() -> Facts {
        // a session the agent bridge made without a model is not a chat
        let inert = super::mcp_bridge::session_is_inert();
        with_session(|s| {
            let mut f = match s.as_ref().filter(|_| !inert) {
                Some(u) => Facts {
                    plan: u.plan().cloned(),
                    has_session: true,
                    busy: u.is_busy(),
                    stale: u.is_stale(),
                    can_apply: u.can_apply(),
                    redraft: u.redraft().cloned(),
                    rebased: u.rebased().cloned(),
                    plan_rev: u.plan_rev(),
                    ..Facts::default()
                },
                None => Facts::default(),
            };
            // an agent's cards belong to the person even when no chat runs
            if let Some(u) = s.as_ref() {
                f.proposals = u.proposals().to_vec();
                f.proposal_applying = u.proposal_applying();
                f.agents = u.agent_drafts().to_vec();
                f.agent_ready = u.agent_ready();
            }
            f
        })
    }
}

/// The shared, read-only part of the frame.
pub struct Cx<'a> {
    pub th: &'a Theme,
    pub cat: &'a Catalog,
    pub ed: &'a crate::editor::Editor,
    pub view3d: bool,
    pub palette_open: bool,
}

#[derive(Default)]
pub struct PanelUi {
    init: bool,
    /// Point-and-ask mode: a pick in a view fills its context, the next question carries it.
    pub point: crate::ai::point::PointMode,
    mark_msg: u64,
    mark_open: Option<u64>,
    /// The selection last looked at, so only a new pick fills the context.
    point_seen: Vec<crate::editor::Sel>,
    pub fold: dock_fold::Fold,
    pub open: bool,
    pub tab: usize,
    pub propose: bool,
    pub profiles: Vec<Profile>,
    pub profile: usize,
    pub log: Vec<Entry>,
    pub input: String,
    pub recall: recall::Recall,
    pub focus_input: bool,
    pub composer_focused: bool,
    pub use_selection: bool,
    /// View and level chips taken out of the context.
    pub ctx_off: ctx_chips::Off,
    /// The project depth setting at an `Editor::cache_key`, for the depth chip.
    pub depth: Option<((u64, u64), ddc_model::DepthSettings)>,
    pub error: Option<AiError>,
    /// Proposal outcomes waiting for a toast; `drain` has no app access.
    toasts: Vec<Result<i64, AiError>>,
    /// The connections file could not be read at start: said once as a toast.
    unreadable: Option<connect::Unreadable>,
    pub limit: Option<&'static str>,
    pub usage: UsageTotals,
    pub tool_running: bool,
    /// Timestamps of the running turn; `stream_status` makes the status row of them.
    pub stream: Vec<stream_status::Ev>,
    /// The session's copy is being made; turns wait for it.
    pub starting: bool,
    pub prepared: Option<(u64, u64)>,
    pub recomputed: bool,
    pub preview_on: bool,
    pub show_all: bool,
    pub expanded: HashSet<u32>,
    pub cursor: Option<u32>,
    /// The review keys act: the person is working in the plan card, not in the viewport.
    pub card_focus: bool,
    /// The cursor moved this frame: its row scrolls into view.
    pub cursor_moved: bool,
    pub pinned: Vec<ElKey>,
    pending: Option<(String, Mode)>,
    last: Option<(String, Mode)>,
    again: bool,
    pub consent: Option<String>,
    pub consent_snapshots: bool,
    pub local_wait: bool,
    once_host: Option<String>,
    session_host: Option<String>,
    session_profile: String,
    project: Option<String>,
    chat_loaded: bool,
    chat_dirty: bool,
    /// The last chat save failed; its note is already in the log.
    chat_failed: bool,
    pub snapshots: Arc<AtomicBool>,
    /// The card revision last drawn; an Apply or "Got it" acts on that one only.
    shown_rev: u64,
    pub sent: sent::SentLog,
    pub sent_view: sent::SentView,
    pub sent_open: bool,
    /// The steps of the last request (`ddc_ai::journal`) and whether its feed is open.
    pub journal: ddc_ai::TaskJournal,
    pub journal_open: bool,
    pub audit_open: bool,
    pub audit: Vec<(i64, i64, AuditEntry)>,
    pub audit_ui: audit_ui::AuditUi,
    pub settings_open: bool,
    pub settings: settings::Settings,
    pub checks: checks::Checks,
    pub cmp: compare::Compare,
    pub retry: retry::RetryState,
    /// Values corrected on the card: what each showed before the first edit, and a refusal.
    pub values: value_row::Edited,
    /// A canvas tool is active this frame (`EditorApp::active_command`): Esc is its first.
    pub canvas_tool: bool,
    /// View tool results waiting for the app: `drain` has none, `show` runs them.
    view_q: Vec<crate::ai::view_tools::ViewCmd>,
    /// Family drafts the assistant offered, (name, text, base family): the family editor opens
    /// them as documents; the project is changed only by its load button.
    pub family_drafts: Vec<(String, String, Option<String>, (u32, u64))>,
    /// Drafts of external agents waiting for the person to open them in the family editor.
    pub family_offers: Vec<(String, String, Option<String>, (u32, u64))>,
    /// The AI tabs of the view bar: they draw this same conversation full size.
    pub modes: modes::Modes,
    /// The card's files as the person ticked them, and the card's own they started from.
    pub deliver: Option<crate::deliver::Outputs>,
    pub deliver_seen: Option<crate::deliver::Outputs>,
    /// Files attached to the chat and the requirements card.
    pub attach: attach_ui::AttachState,
}

thread_local! {
    static PANEL: RefCell<PanelUi> = RefCell::new(PanelUi::default());
}

/// Files dropped on the window: over the open chat they attach (true), elsewhere they open.
pub fn take_chat_drop(ctx: &egui::Context, files: &[std::path::PathBuf]) -> bool {
    let at = ctx.input(|i| i.pointer.latest_pos());
    with(|p| attach_ui::take_drop(p, at, files))
}

/// The pointer is over the open chat while files hover: the hint says they will attach.
pub fn chat_drop_hint(ctx: &egui::Context) -> bool {
    let at = ctx.input(|i| i.pointer.latest_pos());
    with(|p| p.open && at.zip(p.attach.rect).is_some_and(|(a, r)| r.contains(a)))
}

fn with<R>(f: impl FnOnce(&mut PanelUi) -> R) -> R {
    PANEL.with(|p| f(&mut p.borrow_mut()))
}

/// Shows tab `i` of the panel on this thread (0 dialog, 1 checks, 2 options).
pub fn show_tab(i: usize) {
    with(|p| p.tab = i);
}

/// The plan card holds the bare review keys (A/R/J/K/Space): the shell's key sequences skip them.
pub fn review_keys_live() -> bool {
    with(|s| s.card_focus)
}

/// What the journal keeps of a service address: its origin, masked.
fn journal_endpoint(s: &str) -> String {
    ddc_ai::secret::scrub_known(&ddc_ai::profile::url_origin(s))
}

fn audit(app: &EditorApp, event: &str, status: &str) {
    let e = AuditEntry {
        actor: "person".to_string(),
        event: event.to_string(),
        status: status.to_string(),
        ..AuditEntry::default()
    };
    crate::ai::session::audit_write(&app.ed.model, &e);
}

impl PanelUi {
    fn init(&mut self, app: &EditorApp) {
        if self.init {
            return;
        }
        self.init = true;
        (self.profiles, self.unreadable) = connect::load_profiles();
        self.profile = self
            .profiles
            .iter()
            .position(|p| p.id == app.prefs.ai_profile)
            .unwrap_or(0);
        self.open = app.prefs.ai_panel_open;
        self.propose = true;
        self.preview_on = true;
        self.use_selection = true;
        self.consent_snapshots = connect::snapshots_card_default(&app.ed.model);
        self.snapshots.store(
            connect::snapshots_for(&app.ed.model, self.active()),
            Ordering::Relaxed,
        );
    }

    fn active(&self) -> Option<&Profile> {
        self.profiles.get(self.profile)
    }

    /// A profile counts once it names an address and a model.
    pub fn active_ready(&self) -> bool {
        self.active()
            .is_some_and(|p| !p.base_url.trim().is_empty() && !p.model.trim().is_empty())
            || connect::default_profile().is_some()
    }

    fn end_session(&mut self) {
        with_session(|s| {
            if let Some(mut u) = s.take() {
                u.end();
            }
        });
        self.starting = false;
        self.prepared = None;
        self.tool_running = false;
        self.stream.clear();
        self.pinned.clear();
        self.cursor = None;
        preview::set_hot(HashSet::new());
    }

    /// Consents, the conversation and the plan all belong to the project.
    fn watch_project(&mut self, app: &EditorApp) {
        let now = connect::project_of(&app.ed.model);
        if now == self.project && self.chat_loaded {
            return;
        }
        self.project = now;
        self.end_session();
        self.view_q.clear();
        crate::ai::view_state::forget();
        point_marks::with(|k| *k = Default::default());
        self.mark_open = None;
        self.point.esc();
        // a log written by an older version may hold a service address with its key
        if let Err(e) = app.ed.model.audit_rewrite(journal_endpoint, ddc_ai::secret::scrub_known) {
            eprintln!("assistant audit: {}", ddc_ai::secret::scrub_known(&e.to_string()));
        }
        self.log = chat_entries(app.ed.model.chat_load().unwrap_or_default());
        self.chat_loaded = true;
        self.chat_dirty = false;
        self.chat_failed = false;
        self.sent.clear();
        self.error = None;
        self.limit = None;
        self.pending = None;
        self.last = None;
        self.retry.reset();
        self.consent = None;
        self.local_wait = false;
        self.once_host = None;
        self.usage = UsageTotals::default();
        self.snapshots.store(
            connect::snapshots_for(&app.ed.model, self.active()),
            Ordering::Relaxed,
        );
        self.consent_snapshots = connect::snapshots_card_default(&app.ed.model);
        self.checks = checks::Checks::default();
    }

    fn drain(&mut self) {
        let events = with_session(|s| s.as_mut().map_or_else(Vec::new, |u| u.drain_events()));
        let mut stopped = false;
        let now = web_time::Instant::now();
        for e in events {
            use stream_status::{push, Ev};
            match &e {
                UiEvent::Text(t) if t.is_empty() => push(&mut self.stream, Ev::Heartbeat(now)),
                UiEvent::Text(_) => push(&mut self.stream, Ev::Text(now)),
                UiEvent::Tool { name, ok: None, .. } => {
                    push(&mut self.stream, Ev::ToolStart(now, name.clone()))
                }
                UiEvent::Tool { .. } => push(&mut self.stream, Ev::ToolEnd(now)),
                UiEvent::Failed(_) => push(&mut self.stream, Ev::Done(now)),
                UiEvent::TurnDone(TurnEnd::Cancelled) => {
                    if !matches!(self.stream.last(), Some(Ev::Cancel(_))) {
                        push(&mut self.stream, Ev::Cancel(now));
                    }
                }
                UiEvent::TurnDone(_) => push(&mut self.stream, Ev::Done(now)),
                _ => {}
            }
            match e {
                UiEvent::Text(t) => {
                    if t.is_empty() {
                        continue;
                    }
                    self.tool_running = false;
                    match self.log.last_mut() {
                        Some(Entry::Assistant(a)) => a.push_str(&t),
                        _ => self.log.push(Entry::Assistant(t)),
                    }
                }
                UiEvent::Tool { name, ok: None, .. } => {
                    self.tool_running = true;
                    self.log
                        .push(Entry::Tool(steps::Step::start(&name, steps::now_ms())));
                }
                UiEvent::Tool { name, ok, err, .. } => {
                    self.tool_running = false;
                    let at = steps::now_ms();
                    let open = self.log.iter_mut().rev().find_map(|e| match e {
                        Entry::Tool(s) if s.state() == steps::State::Running && s.name == name => {
                            Some(s)
                        }
                        _ => None,
                    });
                    match open {
                        Some(s) => s.finish(ok == Some(true), err.as_deref(), at),
                        None => {
                            let mut s = steps::Step::start(&name, at);
                            s.finish(ok == Some(true), err.as_deref(), at);
                            self.log.push(Entry::Tool(s));
                        }
                    }
                    self.chat_dirty = true;
                }
                UiEvent::Usage(u) => self.usage = u,
                UiEvent::Step(r) => self.journal.steps.push(r),
                UiEvent::Sent(e) => self.sent.push(e),
                UiEvent::Failed(e) => {
                    if let Some(m) = self.mark_open.take() {
                        let (code, text) = (point_marks::code_of(&e), e.to_string());
                        point_marks::with(|k| k.failed(m, code, &text));
                    }
                    stopped |= matches!(&e, AiError::Internal(t) if t.starts_with(WORKER_STOPPED));
                    self.tool_running = false;
                    self.again = false;
                    self.pending = None;
                    self.close_steps(Some(logic::err_detail(&e).unwrap_or_default().as_str()));
                    self.journal.end = Some("failed".to_string());
                    self.error = Some(e);
                    self.chat_dirty = true;
                }
                UiEvent::TurnDone(end) => {
                    self.chat_dirty = true;
                    if let Some(m) = self.mark_open.take() {
                        if matches!(end, TurnEnd::Cancelled) {
                            point_marks::with(|k| k.failed(m, ddc_model::RefusalCode::Cancelled, ""));
                        } else {
                            point_marks::with(|k| k.answered(m));
                        }
                    }
                    self.tool_running = false;
                    self.close_steps(None);
                    self.journal.end = Some(logic::journal_end(&end));
                    self.limit = logic::turn_end_limit(&end);
                    if matches!(end, TurnEnd::Cancelled) {
                        self.log.push(Entry::Note("ai.state.cancelled"));
                    }
                }
                UiEvent::Replanned => self.recomputed = true,
                UiEvent::AgentExpired => self.log.push(Entry::Note("ai.plan.agent_expired")),
                UiEvent::AgentApplyAsked(_) => {
                    // the request only shows the card; Apply stays the person's own click
                    self.open = true;
                    self.log.push(Entry::Note("agent_draft.card.asked"));
                }
                UiEvent::DraftEmptied => self.log.push(Entry::Note("ai.redraft.emptied")),
                UiEvent::ValueRefused { item, why } => self.values.refused(item, why),
                UiEvent::Preparing { done, total } => {
                    self.prepared = (done < total).then_some((done, total))
                }
                UiEvent::RuleDraft { rule, text } => self.log.push(Entry::Rule {
                    def: rule,
                    text,
                    saved: false,
                }),
                UiEvent::IssueDraft(topic) => {
                    self.log.push(Entry::Issue { topic, saved: None })
                }
                UiEvent::Req(offer) => self.log.push(Entry::Req { offer, done: false }),
                UiEvent::ProposalApplied {
                    result,
                    reload_error,
                    ..
                } => {
                    self.chat_dirty = true;
                    // a written step whose view reload failed toasts as the plan draft's does
                    self.toasts.push(match (&result, &reload_error) {
                        (Ok(_), Some(e)) => Err(AiError::AppliedNotRefreshed(e.clone())),
                        _ => result.clone(),
                    });
                    match result {
                        Ok(_) => {
                            self.error = None;
                            self.log.push(Entry::Note(match reload_error {
                                Some(_) => "ai.proposal.applied_resync",
                                None => "ai.proposal.applied",
                            }));
                        }
                        Err(AiError::Stale) => self.log.push(Entry::Note("ai.proposal.stale")),
                        // the refusal is shown by the toast alone
                        Err(_) => {}
                    }
                }
                UiEvent::ExportOffer {
                    cat,
                    filter,
                    columns,
                    group,
                } => self.log.push(Entry::Export {
                    cat,
                    filter,
                    columns,
                    group,
                    saved: false,
                }),
                UiEvent::View(cmd) => self.view_q.push(cmd),
                UiEvent::FamilyDraft {
                    name,
                    text,
                    base_family,
                    repair,
                    queued,
                } => {
                    self.chat_dirty = true;
                    let list = if queued {
                        &mut self.family_offers
                    } else {
                        &mut self.family_drafts
                    };
                    list.retain(|d| d.0 != name);
                    list.push((name, text, base_family, repair));
                }
                UiEvent::Found(f) => {
                    self.chat_dirty = true;
                    self.log.push(Entry::Found(f));
                }
                UiEvent::ScheduleMade(m) => self.log.push(Entry::Schedule {
                    made: *m,
                    saved: false,
                }),
            }
        }
        if stopped {
            self.end_session();
        }
        self.trim_log();
    }

    /// A turn that ended under a running step: failed with `reason`, or stopped.
    fn close_steps(&mut self, reason: Option<&str>) {
        let at = steps::now_ms();
        for e in &mut self.log {
            if let Entry::Tool(s) = e {
                s.stop(reason, at);
            }
        }
    }

    fn trim_log(&mut self) {
        if self.log.len() > LOG_KEPT {
            self.log.drain(..self.log.len() - LOG_KEPT);
        }
    }

    /// Writes the visible chat into the project file when it changed. Not a model edit:
    /// no journal record, no undo step.
    fn save_chat(&mut self, app: &EditorApp) {
        if !self.chat_dirty {
            return;
        }
        match app.ed.model.chat_save(&chat_rows(&self.log)) {
            Ok(()) => {
                self.chat_dirty = false;
                self.chat_failed = false;
            }
            // stays dirty, so the next frame tries again; the note is shown once per failure run
            Err(err) => {
                eprintln!("assistant chat: {}", ddc_ai::secret::scrub_known(&err.to_string()));
                if !self.chat_failed {
                    self.chat_failed = true;
                    self.log.push(Entry::Note("ai_safety.chat_not_saved"));
                }
            }
        }
    }

    /// Shows the queued proposal outcomes as toasts.
    fn flush_toasts(&mut self, app: &mut EditorApp) {
        for r in std::mem::take(&mut self.toasts) {
            let (level, text) = proposal_toast(&app.sh.cat, &r, app.ed.undo_len());
            app.sh.toasts.push(level, text, None);
        }
        if let Some(u) = self.unreadable.take() {
            let text = connect::unreadable_note(&app.sh.cat, &u);
            app.sh.toasts.push(Level::Warning, text, None);
        }
        let (applied, other) = crate::ai::session::take_audit_lost();
        if applied {
            let text = app.sh.cat.t("ai_safety.apply_unlogged");
            app.sh.toasts.push(Level::Warning, text, None);
        } else if other {
            let text = app.sh.cat.t("ai_safety.audit_unlogged");
            app.sh.toasts.push(Level::Warning, text, None);
        }
    }

    /// Sending starts with the gate: nothing leaves before it says so, and the text waits.
    fn dispatch(&mut self, app: &mut EditorApp) {
        let Some((text, mode)) = self.pending.clone() else {
            return;
        };
        self.again = false;
        let profile = self.active().cloned().or_else(connect::default_profile);
        let host = match connect::gate(&app.ed.model, profile.as_ref(), self.once_host.as_deref()) {
            Gate::NoProfile => {
                self.settings_open = true;
                return;
            }
            Gate::LocalBlocked => {
                self.local_wait = true;
                return;
            }
            Gate::NeedConsent { host } => {
                self.consent = Some(host);
                return;
            }
            Gate::BadAddress(e) => {
                self.pending = None;
                self.error = Some(AiError::Provider(format!("request blocked: {e}")));
                return;
            }
            Gate::Ready { host } => host,
        };
        let Some(p) = profile else {
            return;
        };
        self.consent = None;
        self.local_wait = false;
        let alive = !super::mcp_bridge::session_is_inert() && with_session(|s| s.is_some());
        if !alive || self.session_host != host || self.session_profile != p.id {
            self.end_session();
            let view = connect::view_ctx(app, false);
            match connect::open_session(app, &p, host.clone(), self.snapshots.clone(), view) {
                Ok(u) => {
                    with_session(|s| *s = Some(u));
                    self.session_host = host;
                    self.session_profile = p.id.clone();
                    self.starting = true;
                }
                Err(e) => {
                    self.pending = None;
                    self.error = Some(e);
                    return;
                }
            }
        }
        let mut view: ViewCtx = connect::view_ctx(app, self.use_selection);
        ctx_chips::hide(&mut view, self.ctx_off);
        // the files go first on the same channel; the log keeps the person's words only
        let files = self.attach.files.clone();
        let facts = self.attach.req.confirmed_facts();
        if let crate::ai::point::PointMode::Pointing { ctx: Some(c), .. } = &self.point {
            if let Some(t) = c.targets.first() {
                self.mark_msg += 1;
                point_marks::with(|m| {
                    let id = m.add(t.hit, c.view.clone(), self.mark_msg);
                    m.set_pose(id, c.pose);
                });
                self.mark_open = Some(self.mark_msg);
            }
        }
        let point = self.point.take_block();
        let text = text + &crate::ai::attach::context_block(&files, &self.attach.req);
        let r = with_session(|s| {
            s.as_mut().map(|u| {
                u.set_attachments(files, facts);
                u.set_point(point);
                u.send_turn(&app.ed, text, view, mode)
            })
        });
        match r {
            Some(Err(AiError::Busy)) => self.again = true,
            Some(Err(e)) => {
                self.pending = None;
                self.error = Some(e);
            }
            Some(Ok(())) => {
                self.pending = None;
                self.starting = false;
                stream_status::push(
                    &mut self.stream,
                    stream_status::Ev::Start(web_time::Instant::now()),
                );
                // "Once" covers the turn just sent
                self.once_host = None;
            }
            None => {}
        }
    }

    fn submit(&mut self, app: &mut EditorApp, text: String, mode: Mode) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.error = None;
        self.limit = None;
        self.recomputed = false;
        self.values.next_request();
        self.log.push(Entry::User(text.clone()));
        self.trim_log();
        self.chat_dirty = true;
        self.journal = ddc_ai::TaskJournal::new(&text);
        self.last = Some((text.clone(), mode));
        self.pending = Some((text, mode));
        self.retry.reset();
        self.dispatch(app);
    }

    /// Sends the last prompt again in its own mode.
    fn resend(&mut self, app: &mut EditorApp) {
        if let Some((text, mode)) = self.last.clone() {
            self.error = None;
            self.limit = None;
            self.pending = Some((text, mode));
            self.dispatch(app);
        }
    }

    /// Per frame: counts down a retryable failure and resends when the time is up.
    /// A turn that left a draft or an agent card is never resent.
    fn retry_step(&mut self, app: &mut EditorApp, ctx: &egui::Context, has_draft: bool) {
        let kind = self.error.as_ref().map(retry::auto_kind);
        let after = self.error.as_ref().and_then(|e| logic::error_detail(e).retry_after_s);
        let now = ctx.input(|i| i.time);
        if self.retry.step(kind, after, has_draft, now) == retry::Step::Fire {
            self.resend(app);
        }
        if self.retry.remaining(now).is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn apply(&mut self, app: &mut EditorApp) {
        let plan = with_session(|s| s.as_ref().and_then(|u| u.plan().cloned()));
        let rev = self.shown_rev;
        let before_seq = app.ed.model.op_top().unwrap_or(0);
        let r = with_session(|s| s.as_mut().map(|u| u.apply_rev(&mut app.ed, rev)));
        match r {
            Some(Ok(op_seq)) => {
                point_marks::with(|k| {
                    if let Some(m) = k.latest_open() {
                        let row = app.ed.model.ai_option_head().unwrap_or((op_seq, 0));
                        k.applied(&app.ed, m, before_seq, row);
                    }
                });
                // the files of a delivery card follow the Apply, as the person ticked them
                let files = plan.as_ref().and_then(|p| p.deliver).and(self.deliver.take());
                self.deliver_seen = None;
                if let Some(o) = files.filter(crate::deliver::Outputs::any) {
                    crate::deliver::ui::after_apply(app, o);
                }
                self.error = None;
                self.pinned.clear();
                self.cursor = None;
                self.expanded.clear();
                self.values.clear();
                let head = app.ed.model.ai_option_head().unwrap_or((0, 0));
                let receipt = plan.and_then(|p| {
                    let checked: Vec<u32> =
                        p.items.iter().filter(|i| i.checked).map(|i| i.id).collect();
                    let drafts: Vec<String> = p
                        .items
                        .iter()
                        .filter(|i| i.checked)
                        .filter_map(|i| match &i.op {
                            crate::ai::plan::PlanOp::DefineFamily { def } => {
                                def["name"].as_str().map(str::to_string)
                            }
                            _ => None,
                        })
                        .collect();
                    receipt::Receipt::summarize(op_seq, &p.diff, &checked).map(|mut r| {
                        r.drafts = drafts;
                        r.of_head(head)
                    })
                });
                let has_receipt = receipt.is_some();
                self.log.push(match receipt {
                    Some(r) => Entry::Applied(r),
                    None => Entry::Note("ai.plan.applied"),
                });
                self.chat_dirty = true;
                // the not-refreshed failure is queued by the apply: fold it in before the toast
                self.drain();
                let (level, key) = logic::applied_toast(self.error.as_ref());
                let n = app.ed.undo_len().to_string();
                let text = crate::tr(key, &[("n", &n)]);
                if has_receipt {
                    let action = crate::tr("ai.receipt.undo", &[]);
                    app.sh.toasts.push_cmd(level, text, action, "ai.receipt_undo");
                } else {
                    app.sh.toasts.push(level, text, None);
                }
            }
            Some(Err(e)) => {
                self.error = Some(e);
                // Ctrl+Alt+Enter works with the panel closed: the refusal must still be seen
                self.open = true;
            }
            None => {}
        }
    }

    /// Files are read here, on this machine; nothing leaves before a turn and a tool ask for it.
    fn attach_paths(&mut self, app: &mut EditorApp, paths: Vec<std::path::PathBuf>) {
        for p in paths {
            let name = p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            let r = self.attach.add_path(&p);
            self.attach_result(app, &name, r);
        }
        self.focus_input = true;
    }

    fn attach_result(&mut self, app: &mut EditorApp, name: &str, r: Result<(), String>) {
        match r {
            Ok(()) => {
                let text = app.sh.cat.tf("ai.att.added", &[("name", name)]);
                app.sh.toasts.push(Level::Info, text, None);
            }
            Err(e) => {
                let text = app.sh.cat.tf("ai.att.failed", &[("name", name), ("why", e.as_str())]);
                app.sh.toasts.push(Level::Warning, text, None);
            }
        }
    }

    /// The picked file is read here, on the UI thread, and only the small image goes on.
    fn attach_reference(&mut self, app: &mut EditorApp) {
        let Some(path) = crate::file_pick::Dlg::new()
            .set_title(app.sh.cat.t("ai.ref.attach"))
            .add_filter("Image / PDF", &["png", "jpg", "jpeg", "pdf"])
            .pick_file()
        else {
            return;
        };
        match crate::ai::reference::load(&path) {
            Ok(img) => {
                with_session(|s| {
                    if let Some(u) = s.as_mut() {
                        u.set_reference(vec![img]);
                    }
                });
                let text = app.sh.cat.t("ai.ref.attached");
                app.sh.toasts.push(Level::Info, text, None);
            }
            Err(e) => {
                let text = app.sh.cat.tf("ai.ref.failed", &[("why", e.as_str())]);
                app.sh.toasts.push(Level::Warning, text, None);
            }
        }
    }

    fn run(&mut self, app: &mut EditorApp, a: Action) {
        match a {
            Action::Send(text, mode) => self.submit(app, text, mode),
            Action::Retry => {
                self.retry.reset();
                self.resend(app);
            }
            Action::RetryNow => {
                self.retry.retry_now();
                self.resend(app);
            }
            Action::RetryCancel => self.retry.cancel(),
            Action::Shorten => {
                if let Some((text, _)) = self.last.take() {
                    self.input = text;
                    self.focus_input = true;
                }
                self.use_selection = false;
                self.error = None;
                self.limit = None;
                self.retry.reset();
            }
            Action::Apply => self.apply(app),
            Action::AttachReference => self.attach_reference(app),
            Action::AttachFiles => {
                let picked = crate::file_pick::Dlg::new()
                    .set_title(app.sh.cat.t("ai.att.attach"))
                    .add_filter("PDF, DWG, DXF, IFC, XLSX, CSV, DOCX, TXT, PNG, JPEG", ddc_io::attach::EXTENSIONS)
                    .pick_files();
                if let Some(paths) = picked {
                    self.attach_paths(app, paths);
                }
            }
            Action::AttachPaths(paths) => self.attach_paths(app, paths),
            Action::AttachClipboardImage => {
                if let Some((w, h, rgba)) = attach_ui::clipboard_image() {
                    let id = self.attach.next_id();
                    let name = format!("image-{id}.png");
                    let r = crate::ai::attach::from_rgba(&name, w, h, rgba, id).and_then(|d| self.attach.add(d));
                    self.attach_result(app, &name, r);
                }
            }
            Action::AttachRemove(id) => self.attach.remove(id),
            Action::AttachShare(id) => self.attach.toggle_share(id),
            Action::AttachPreview(id) => self.attach.preview = id,
            Action::ReqConfirm => self.attach.req.confirmed = true,
            Action::AttachUnderlay(id) => {
                if let Some(path) = self.attach.get(id).and_then(|a| a.path.clone()) {
                    let level = crate::shell::drawing_win::LevelPick::Existing(app.ed.level);
                    let ratio = self.attach.get(id).and_then(|a| a.ex.scale.as_ref()).map_or(100.0, |s| s.ratio);
                    crate::open::build::start_underlay(app, vec![(path, level)], None, ratio);
                }
            }
            Action::Review => {
                let rev = Some(self.shown_rev);
                if let Some(n) = with_session(|s| s.as_mut().and_then(|u| u.review(rev))) {
                    self.log.push(Entry::Redraft(n));
                }
            }
            Action::Reject => {
                with_session(|s| {
                    if let Some(u) = s.as_mut() {
                        u.reject();
                    }
                });
                self.pinned.clear();
                self.cursor = None;
                self.expanded.clear();
                self.values.clear();
                self.error = None;
                self.log.push(Entry::Note("ai.plan.rejected"));
                self.chat_dirty = true;
            }
            Action::Cancel => {
                with_session(|s| {
                    if let Some(u) = s.as_ref() {
                        u.cancel();
                    }
                });
                self.pending = None;
                // the row ends now; the worker's own end arrives later and changes nothing
                if stream_status::stream_status(&self.stream, web_time::Instant::now()).live() {
                    stream_status::push(
                        &mut self.stream,
                        stream_status::Ev::Cancel(web_time::Instant::now()),
                    );
                }
            }
            Action::SetChecked(list) => with_session(|s| {
                if let Some(u) = s.as_mut() {
                    u.set_checked(list);
                }
            }),
            Action::SetVariant(id) => with_session(|s| {
                if let Some(u) = s.as_mut() {
                    u.set_variant(id);
                }
            }),
            Action::AllowLarge(on) => with_session(|s| {
                if let Some(u) = s.as_mut() {
                    u.allow_large(on);
                }
            }),
            Action::SetValue { item, field, value } => with_session(|s| {
                if let Some(u) = s.as_mut() {
                    u.set_value(item, field, value);
                }
            }),
            Action::Zoom(keys) => {
                let plan = with_session(|s| s.as_ref().and_then(|u| u.plan().cloned()));
                if let Some((b, level)) = plan.and_then(|p| preview::focus_of(&p.preview, &keys)) {
                    zoom(app, &b, level);
                }
                self.pinned = keys;
            }
            Action::FrameSelection => crate::open::summary::frame_selection(app),
            Action::ZoomAll => {
                let plan = with_session(|s| s.as_ref().and_then(|u| u.plan().cloned()));
                if let Some(b) = plan.and_then(|p| p.preview.bounds) {
                    zoom(app, &b, None);
                }
                self.show_all = !self.show_all;
            }
            Action::SelectEl(key) => {
                if let (Some(sel), _) = preview::hide_of(&key) {
                    app.ed.selection = vec![sel];
                }
            }
            Action::ShowApplied(ids) => {
                if receipt::show(app, &ids) {
                    self.pinned = ids;
                }
            }
            Action::UndoApplied(seq) => {
                if receipt::undo(app, &mut self.log, seq) {
                    audit(app, "undo", "receipt");
                    self.chat_dirty = true;
                    let text = crate::tr("ai.receipt.toast_undone", &[]);
                    app.sh.toasts.push(Level::Info, text, None);
                }
            }
            Action::RollbackApplied(seq) => {
                let (level, text) = match receipt::rollback(app, &mut self.log, seq) {
                    Some(n) => {
                        audit(app, "rollback", "receipt");
                        self.chat_dirty = true;
                        (Level::Info, crate::tr("ai.receipt.toast_rolled", &[("n", &n.to_string())]))
                    }
                    None => (Level::Warning, crate::tr("ai.receipt.rollback_failed", &[])),
                };
                app.sh.toasts.push(level, text, None);
            }
            Action::PromoteFamilies(seq) => {
                let names = self.log.iter().rev().find_map(|e| match e {
                    Entry::Applied(r) if r.op_seq == seq && !r.undone => Some(r.drafts.clone()),
                    _ => None,
                });
                let mut left = vec![];
                for name in names.unwrap_or_default() {
                    if crate::ai::geo_tools::promote_family(&mut app.ed, &name).is_err() {
                        left.push(name);
                    }
                }
                let ok = left.is_empty();
                if let Some(r) = self.log.iter_mut().rev().find_map(|e| match e {
                    Entry::Applied(r) if r.op_seq == seq => Some(r),
                    _ => None,
                }) {
                    r.drafts = left;
                }
                self.chat_dirty = true;
                let (level, key) = if ok {
                    (Level::Info, "ai.receipt.promoted")
                } else {
                    (Level::Warning, "ai.receipt.promote_failed")
                };
                app.sh.toasts.push(level, crate::tr(key, &[]), None);
            }
            Action::ConsentProject { snapshots } => {
                if let Some(host) = self.consent.take() {
                    let m = &app.ed.model;
                    let saved = m
                        .set_ai_policy(&logic::consent_policy(&host), "1")
                        .and_then(|()| {
                            m.set_ai_policy("egress.snapshots", if snapshots { "1" } else { "0" })
                        });
                    match saved {
                        Ok(()) => {
                            audit(app, "consent", &host);
                            self.snapshots.store(snapshots, Ordering::Relaxed);
                            self.dispatch(app);
                        }
                        Err(e) => self.error = Some(AiError::Internal(e.to_string())),
                    }
                }
            }
            Action::ConsentOnce { snapshots } => {
                if let Some(host) = self.consent.take() {
                    audit(app, "consent_once", &host);
                    self.once_host = Some(host);
                    self.snapshots.store(snapshots, Ordering::Relaxed);
                    self.dispatch(app);
                }
            }
            Action::ConsentNo => {
                self.consent = None;
                self.local_wait = false;
                if let Some((text, _)) = self.pending.take() {
                    if matches!(self.log.last(), Some(Entry::User(t)) if *t == text) {
                        self.log.pop();
                    }
                    self.input = text;
                    self.focus_input = true;
                }
            }
            Action::SetProfile(i) => {
                if i != self.profile && i < self.profiles.len() {
                    self.profile = i;
                    self.end_session();
                    self.consent = None;
                    self.local_wait = false;
                    self.once_host = None;
                    app.prefs.ai_profile = self.profiles[i].id.clone();
                    app.prefs.save();
                }
            }
            Action::OpenSettings => self.settings_open = true,
            // handled by `act` before the panel state is borrowed
            Action::Offline(_) => {}
            Action::OpenSent => self.sent_open = true,
            Action::ToggleJournal => self.journal_open = !self.journal_open,
            Action::ExportJournal => {
                if let Err(e) = consent::export_journal(app, &self.journal) {
                    self.error = Some(AiError::Internal(e));
                }
            }
            Action::Export(i) => {
                if let Some(Entry::Export {
                    cat,
                    filter,
                    columns,
                    group,
                    saved,
                }) = self.log.get_mut(i)
                {
                    match consent::export_offer(
                        app,
                        cat,
                        filter.as_deref(),
                        columns,
                        group.as_deref(),
                    ) {
                        Ok(true) => *saved = true,
                        Ok(false) => {}
                        Err(e) => self.error = Some(AiError::Internal(e)),
                    }
                }
            }
            Action::OpenSchedule(i) => {
                if let Some(Entry::Schedule { made, .. }) = self.log.get(i) {
                    app.table.apply_preset(&app.ed, &made.preset);
                    app.set_layout("data");
                }
            }
            Action::ExportSchedule(i) => {
                if let Some(Entry::Schedule { made, saved }) = self.log.get_mut(i) {
                    match consent::export_preset(app, &made.preset, &made.title) {
                        Ok(true) => *saved = true,
                        Ok(false) => {}
                        Err(e) => self.error = Some(AiError::Internal(e)),
                    }
                }
            }
            Action::SaveRule(i) => {
                if let Some(Entry::Rule { def, saved, .. }) = self.log.get_mut(i) {
                    match app.ed.model.save_rule(def) {
                        Ok(()) => *saved = true,
                        Err(e) => self.error = Some(AiError::Internal(e.to_string())),
                    }
                }
            }
            Action::NewIssue(d) => crate::issues_ui::start_draft(app, *d),
            Action::SaveIssue(i) => {
                if let Some(Entry::Issue { topic, saved }) = self.log.get_mut(i) {
                    match crate::issues_ui::save_draft(app, topic) {
                        Ok(id) => *saved = Some(id),
                        Err(e) => self.error = Some(AiError::Internal(e)),
                    }
                }
            }
            Action::RestartSession => {
                self.end_session();
                self.snapshots.store(
                    connect::snapshots_for(&app.ed.model, self.active()),
                    Ordering::Relaxed,
                );
            }
            Action::ProfilesSaved(list, sel) => {
                self.profiles = list;
                self.profile = sel.min(self.profiles.len().saturating_sub(1));
                self.end_session();
                self.consent = None;
                self.local_wait = false;
                self.once_host = None;
                app.prefs.ai_profile = self.active().map(|p| p.id.clone()).unwrap_or_default();
                app.prefs.save();
                if self.pending.is_some() {
                    self.dispatch(app);
                }
            }
            Action::ClosePanel => self.set_open(app, false),
            Action::OpenPanel => {
                self.tab = 0;
                self.set_open(app, true);
            }
            Action::AskFix(text) => {
                self.tab = 0;
                self.submit(app, text, Mode::Propose);
            }
            Action::AskExplain(text) => {
                self.tab = 0;
                self.submit(app, text, Mode::ReadOnly);
            }
            Action::Command(id) => {
                app.run(id);
            }
            Action::Preview(p, sheet) => {
                self.modes.to_preview(p, sheet);
                self.modes.rebase(modes::base_of(app));
            }
            Action::EditRequest => {
                if let Some((text, _)) = self.last.clone() {
                    self.input = text;
                    self.focus_input = true;
                }
            }
            Action::CheckInReview => {
                self.modes.enter(modes::AiMode::Review, modes::base_of(app));
                self.checks.run(&app.ed.model);
                self.checks.norms.run(&app.ed.model);
            }
            Action::McpConfirm(on) => set_mcp_policy(app, on),
            Action::McpRevoke => h_revoke(app),
            Action::UndoAudit(seq) => undo_audit(app, &mut self.audit, seq),
            Action::RevertAudit(seq) => {
                if self.audit_ui.take_confirmed(seq) {
                    revert_audit(app, &mut self.audit, seq);
                }
            }
            Action::AskRevert(seq) => self.audit_ui.confirm = audit_ui::ask(app, seq),
            Action::CancelRevert => self.audit_ui.confirm = None,
            Action::ShowAudit(seq) => {
                audit_ui::show(app, seq);
            }
            Action::McpServer => crate::ai::mcp_bridge::open_settings(),
            Action::SetDepth(s) => {
                if let Err(e) = crate::depth_ui::apply(&mut app.ed, s) {
                    app.ed.status = Some(e.to_string());
                }
                self.depth = None;
            }
            Action::DepthHeat(on) => crate::depth_ui::set_heat(app, on),
            Action::SetPropose(on) => self.propose = on,
            Action::Center(at) => app.plan.center = v2(at[0], at[1]),
            Action::ApplyOption(id) => match crate::ai::option_exec::apply(app, id) {
                Ok(a) => {
                    let level = if a.warn { Level::Warning } else { Level::Success };
                    app.sh.toasts.push(level, a.text, None);
                }
                Err(text) => app.sh.toasts.push(Level::Warning, text, None),
            },
            Action::ApplyProposal(id) => {
                let r = with_session(|s| s.as_mut().map(|u| u.apply_proposal(id)));
                if let Some(Err(e)) = r {
                    self.error = Some(e);
                }
            }
            Action::RejectProposal(id) => {
                with_session(|s| {
                    if let Some(u) = s.as_mut() {
                        u.reject_proposal(id);
                    }
                });
                self.log.push(Entry::Note("ai.proposal.rejected"));
                self.chat_dirty = true;
            }
            Action::ShowDraft(keys) => {
                receipt::show(app, &keys);
            }
            Action::FoundView(cmd) => self.view(app, &cmd),
            Action::ApplyAgentDraft { actor, rev } => {
                let ed = &mut app.ed;
                let r = with_session(|s| s.as_mut().map(|u| u.apply_agent_draft(ed, &actor, rev)));
                match r {
                    Some(Ok(_)) => {
                        let text = app.sh.cat.t("agent_draft.applied");
                        app.sh.toasts.push(Level::Success, text, None);
                    }
                    Some(Err(e)) => self.error = Some(e),
                    None => {}
                }
            }
            Action::DiscardAgentDraft(actor) => {
                with_session(|s| {
                    if let Some(u) = s.as_mut() {
                        u.discard_agent_draft(&actor);
                    }
                });
                let text = app.sh.cat.t("agent_draft.discarded");
                app.sh.toasts.push(Level::Info, text, None);
            }
            Action::RecheckAgentDraft(actor) => {
                let ed = &app.ed;
                with_session(|s| {
                    if let Some(u) = s.as_mut() {
                        u.recheck_agent_draft(ed, &actor);
                    }
                });
            }
            Action::SetAgentChecked { actor, list } => {
                with_session(|s| {
                    if let Some(u) = s.as_mut() {
                        u.set_agent_checked(&actor, list);
                    }
                });
            }
            Action::Req(d) => requirements::act(self, app, d),
            Action::Norms(d) => norms::act(self, app, d),
            Action::DiscardOption(id) => match self.cmp.cache.discard(&app.ed.model, id, option_dir(app).as_deref()) {
                Ok(()) => {
                    let text = app.sh.cat.t("ai.cmp.discarded");
                    app.sh.toasts.push(Level::Info, text, None);
                }
                Err(e) => app.sh.toasts.push(Level::Warning, e, None),
            },
        }
    }

    /// A view command from a tool or a result card; shown elements stay lit until the next.
    fn view(&mut self, app: &mut EditorApp, cmd: &crate::ai::view_tools::ViewCmd) {
        let lit = crate::ai::view_state::apply(app, cmd);
        let reset = matches!(cmd, crate::ai::view_tools::ViewCmd::Reset);
        if reset || !lit.is_empty() {
            self.pinned = lit;
        }
    }

    /// The quick panel is on screen: open, and no AI tab draws the same conversation.
    pub fn docked(&self) -> bool {
        self.open && self.modes.cur.is_none()
    }

    /// The conversation is on screen, in the quick panel or an AI tab.
    pub fn visible(&self) -> bool {
        self.open || self.modes.cur.is_some()
    }

    fn set_open(&mut self, app: &mut EditorApp, on: bool) {
        self.open = on;
        if app.prefs.ai_panel_open != on {
            app.prefs.ai_panel_open = on;
            app.prefs.save();
        }
    }
}

/// Folder of the branch copies a Discard archives from: the session store's folder for an
/// unsaved project, `<project>.options` for a saved one.
fn option_dir(app: &EditorApp) -> Option<std::path::PathBuf> {
    let session = crate::ai::options_keep::live_session_dir();
    match crate::ai::options_keep::ui_store(&app.ed.model, session.as_deref()) {
        Some((dir, _)) => Some(dir),
        None => app
            .ed
            .model
            .db_path()
            .map(|p| crate::ai::options_keep::options_dir(std::path::Path::new(&p))),
    }
}

fn set_mcp_policy(app: &mut EditorApp, confirm: bool) {
    let policy = if confirm {
        ddc_ai::mcp::McpPolicy::Confirm
    } else {
        ddc_ai::mcp::McpPolicy::AutoApply
    };
    super::mcp_bridge::set_policy_now(app, policy);
}

/// Plan and 3D both go to the element; the person's selection is left alone.
fn zoom(app: &mut EditorApp, b: &Aabb, level: Option<i64>) {
    if let Some(l) = level {
        if l != app.ed.level {
            app.ed.set_level(l);
        }
    }
    let (w, h) = (f64::from(b.max.x - b.min.x), f64::from(b.max.y - b.min.y));
    let span = (w.max(h) * 1.2).max(1.0);
    app.plan.center = v2(f64::from(b.min.x) + w / 2.0, f64::from(b.min.y) + h / 2.0);
    app.plan.scale = (320.0 / span).clamp(20.0, 2000.0);
    let dz = app.ed.elevation as f32;
    let mut rel = *b;
    rel.min.z -= dz;
    rel.max.z -= dz;
    app.view.camera.fit(&rel, 1.6);
}

/// Toast for the end of an agent proposal's Apply: success, stale hint, or the reason. No journal
/// seq: the person sees words, not an internal row number. `undo_n` only for the not-refreshed case.
fn proposal_toast(
    cat: &ddc_ui::i18n::Catalog,
    r: &Result<i64, AiError>,
    undo_n: usize,
) -> (Level, String) {
    match r {
        Ok(_) => (Level::Success, cat.t("ai.proposal.toast.applied")),
        Err(e @ AiError::AppliedNotRefreshed(_)) => {
            let (level, key) = logic::applied_toast(Some(e));
            (level, cat.tf(key, &[("n", &undo_n.to_string())]))
        }
        Err(AiError::Stale) => (Level::Warning, cat.t("ai.proposal.toast.stale")),
        Err(e) => {
            let reason = logic::err_detail(e).unwrap_or_else(|| e.to_string());
            (
                Level::Warning,
                cat.tf("ai.proposal.toast.refused", &[("reason", &reason)]),
            )
        }
    }
}

thread_local! {
    static SHOWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The assistant panel was on screen in the last drawn frame (canvas cards step aside for it).
pub fn panel_shown() -> bool {
    SHOWN.with(|v| v.get())
}

/// Before the central area: the panel takes its width from it.
pub fn show(app: &mut EditorApp, ctx: &egui::Context) {
    let mut st = PANEL.with(|p| std::mem::take(&mut *p.borrow_mut()));
    st.init(app);
    let docked = st.docked();
    dock_fold::sync(&mut st.fold, &mut app.prefs, ctx.screen_rect().width(), docked);
    st.watch_project(app);
    st.drain();
    point_marks::with(|k| k.sync(&app.ed));
    if point_marks::take_focus() {
        st.open = true;
        st.chat_dirty = true;
    }
    if st.point.active() {
        if app.ed.selection != st.point_seen {
            st.point_seen = app.ed.selection.clone();
            if !st.point_seen.is_empty() {
                let c = crate::ai::point::collect(&app.ed, &connect::point_input(app));
                st.point.point(c);
            }
        }
    } else {
        st.point_seen = app.ed.selection.clone();
    }
    st.flush_toasts(app);
    for cmd in std::mem::take(&mut st.view_q) {
        st.view(app, &cmd);
    }
    if let Some(lit) = crate::ai::view_state::tick(app) {
        st.pinned = lit;
    }
    keys::clip_tick(ctx);
    if st.again && !with_session(|s| s.as_ref().is_some_and(|u| u.is_busy())) {
        st.dispatch(app);
    }
    let facts = Facts::read();
    st.shown_rev = facts.plan_rev;
    st.retry_step(app, ctx, facts.plan.is_some() || !facts.proposals.is_empty());
    if !facts.busy {
        st.starting = false;
    }
    let dark = ctx.style().visuals.dark_mode;
    // with no chat card, the oldest external agent's draft is what plan and 3D preview
    let shown = facts
        .plan
        .as_ref()
        .or_else(|| facts.agents.first().map(|a| &a.plan))
        .filter(|_| st.visible() && st.preview_on)
        .map(|p| p.preview.clone());
    preview::publish(shown, dark);
    preview::set_before(crate::ai::ghost::read_before(ctx, preview::rev() != 0));

    let mut acts: Vec<Action> = Vec::new();
    let mut hot: Option<Vec<ElKey>> = None;
    let width = app.prefs.ai_panel_w.clamp(PANEL_W.0, PANEL_W.1);
    let palette_open = app.sh.palette.open;
    let view3d = app.view3d;
    // any canvas tool (plan builders, dimensions, sections, 3D edits) keeps its own Esc
    st.canvas_tool = app.active_command().is_some();
    let clean = app.chrome_clean();
    {
        let sh = &mut app.sh;
        let (th, cat, icons) = (&sh.theme, &sh.cat, &mut sh.icons);
        let cx = Cx {
            th,
            cat,
            ed: &app.ed,
            view3d,
            palette_open,
        };
        if st.docked() && !clean {
            let frame = panel_frame(th);
            let r = egui::SidePanel::right("ddc.panel.ai")
                .default_width(width)
                .width_range(PANEL_W.0..=PANEL_W.1)
                .resizable(true)
                .frame(frame)
                .show(ctx, |ui| {
                    crate::shell::pane_focus::anchor(ui, crate::shell::pane_focus::Pane::Chat);
                    chat::panel(ui, &mut st, &cx, icons, &facts, &mut acts, &mut hot)
                });
            let w = r.response.rect.width();
            if !ctx.input(|i| i.pointer.any_down()) && (w - width).abs() > 0.5 {
                app.prefs.ai_panel_w = w;
                app.prefs.save();
            }
        }
        // no card on screen: the review keys go back to the viewport and its key sequences
        if !(st.visible() && facts.plan.as_ref().is_some_and(|p| !p.items.is_empty())) {
            st.card_focus = false;
        }
        settings::window(ctx, &mut st, &cx, &mut acts);
        sent::window(ctx, &mut st, &cx);
        audit_ui::window(ctx, &mut st, &cx, &mut acts);
    }
    // the row under the pointer wins, then the last zoomed one, then the keyboard cursor
    // hot outlines draw without a plan too: nothing stays lit while the panel is closed
    // with neither, an open agent proposal keeps its affected elements lit until Apply or Reject
    let keys = hot.unwrap_or_else(|| {
        if st.pinned.is_empty() {
            let mut keys = proposal_draft::open_keys(&facts.proposals);
            keys.extend(facts.agents.iter().flat_map(|a| a.keys()));
            keys
        } else {
            st.pinned.clone()
        }
    });
    // the view panel's legend row lights its elements with the chat closed too
    let legend_hot = crate::ai::view_state::hover_keys().unwrap_or_default();
    let lit = keys.into_iter().filter(|_| st.visible()).chain(legend_hot);
    preview::set_hot(logic::hot_set(lit));
    let mut offline = vec![];
    for a in acts {
        match a {
            Action::Offline(id) => offline.push(id),
            a => st.run(app, a),
        }
    }
    st.save_chat(app);
    SHOWN.with(|v| v.set(st.visible()));
    PANEL.with(|p| *p.borrow_mut() = st);
    // after the panel state is back: the command may open views that read it
    for id in offline {
        app.run(id);
    }
}

/// After the views: the rectangle for snapshots, the 3D highlight and the legend.
pub fn overlay(app: &EditorApp, ctx: &egui::Context, area: egui::Rect) {
    set_view_rect(Some(area));
    preview::paint_3d(ctx, &app.view, &app.ed);
    found::view_panel(ctx, &app.sh.theme, &app.sh.cat, area);
    if preview::rev() == 0 {
        return;
    }
    let facts = Facts::read();
    let Some(plan) = facts.plan else {
        return;
    };
    let sh = &app.sh;
    let pad = kit::space::SM;
    egui::Area::new(egui::Id::new("ddc.ai.legend"))
        .order(egui::Order::Middle)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .fixed_pos(area.left_bottom() + egui::vec2(pad, -pad))
        .interactable(false)
        .show(ctx, |ui| plan_card::legend(ui, &sh.theme, &sh.cat, &plan));
    let before = preview::before_mode();
    egui::Area::new(egui::Id::new("ddc.ai.ghostchip"))
        .order(egui::Order::Middle)
        .pivot(egui::Align2::LEFT_TOP)
        .fixed_pos(area.left_top() + egui::vec2(pad, pad))
        .interactable(false)
        .show(ctx, |ui| {
            crate::ai::ghost::chip(ui, &sh.theme, &sh.cat, before);
        });
}

pub use palette_rows::palette_ai;

/// Opens the panel and starts the turn at once; the mode follows the selection like the
/// starter prompts (`palette_rows::ask_mode`).
pub fn ask(app: &mut EditorApp, text: &str) {
    let sel = !app.ed.selection.is_empty();
    with(|st| {
        st.init(app);
        st.watch_project(app);
        st.set_open(app, true);
        st.tab = 0;
        st.use_selection |= sel;
        let mode = palette_rows::ask_mode(sel, st.propose);
        st.submit(app, text.to_string(), mode);
    });
}

fn h_ask(a: &mut EditorApp) {
    with(|st| {
        st.init(a);
        // an AI tab already shows the conversation: the key goes to its composer
        if let Some(m) = st.modes.cur {
            if m == modes::AiMode::Build {
                st.modes.focus_brief = true;
            } else {
                st.focus_input = true;
            }
            st.tab = 0;
        } else if st.open && st.composer_focused {
            st.set_open(a, false);
        } else {
            st.set_open(a, true);
            st.focus_input = true;
            st.tab = 0;
        }
    });
}

fn h_panel(a: &mut EditorApp) {
    with(|st| {
        st.init(a);
        // from an AI tab: back to the model, the same conversation in the quick panel
        if st.modes.cur.is_some() {
            st.modes.leave();
            st.set_open(a, true);
            st.focus_input = true;
            return;
        }
        let on = !st.open;
        st.set_open(a, on);
        st.focus_input = on;
    });
}

fn h_explain(a: &mut EditorApp) {
    if a.ed.selection.is_empty() {
        let t = a.sh.cat.t("ai.explain.no_selection");
        a.sh.toasts.push(Level::Info, t, None);
        return;
    }
    let text = a.sh.cat.t("ai.prompt.explain");
    with(|st| {
        st.init(a);
        st.watch_project(a);
        st.set_open(a, true);
        st.tab = 0;
        st.use_selection = true;
        st.submit(a, text, Mode::ReadOnly);
    });
}

fn h_fix(a: &mut EditorApp) {
    let text = a.sh.cat.t("ai.prompt.fix");
    with(|st| {
        st.init(a);
        st.watch_project(a);
        st.set_open(a, true);
        st.tab = 0;
        st.submit(a, text, Mode::Propose);
    });
}

fn h_check(a: &mut EditorApp) {
    with(|st| {
        st.init(a);
        st.watch_project(a);
        st.set_open(a, true);
        st.tab = 1;
        st.checks.run(&a.ed.model);
    });
}

/// The bar's "Review": opens the panel on the dialog, where the waiting card is.
pub fn open_panel(a: &mut EditorApp) {
    act(a, Action::OpenPanel);
}

fn act(a: &mut EditorApp, action: Action) {
    // an app command runs outside the panel state: it may open panels that read that state
    if let Action::Offline(id) = action {
        a.run(id);
        return;
    }
    with(|st| {
        st.init(a);
        st.run(a, action);
    });
}

/// Takes back the changeset of journal row `seq` if it is still the newest undo step.
fn undo_audit(app: &mut EditorApp, shown: &mut Vec<(i64, i64, AuditEntry)>, seq: i64) {
    let m = &app.ed.model;
    let rows = m.audit_list(seq - 1, 1).unwrap_or_default();
    let Some((_, _, e)) = rows.into_iter().find(|r| r.0 == seq) else {
        return;
    };
    let live = e.op_seq.is_some() && m.audit_op_live(seq).unwrap_or(false);
    if logic::undo_state(e.op_seq, live, m.last_op_seq().ok().flatten()) != logic::UndoState::Ready
    {
        return;
    }
    app.ed.undo();
    audit(app, "undo", &seq.to_string());
    *shown = audit_ui::load(&app.ed.model);
    let what = audit_ui::summary(&app.sh.cat, &e);
    let text = app.sh.cat.tf("ai.audit.toast.undone", &[("what", &what)]);
    let redo = app.sh.cat.t("ai.audit.redo");
    app.sh.toasts.push_cmd(Level::Info, text, redo, "edit.redo");
}

/// Reason keys of a revert that cannot happen, in the catalog.
fn revert_why(c: &ddc_model::RevertCheck) -> &'static str {
    use ddc_model::RevertCheck as R;
    match c {
        R::Ready => "ai.audit.revert.done",
        R::Gone | R::Empty => "ai.audit.undo.gone",
        R::Conflict(_) => "ai.audit.revert.conflict",
        R::TooMany => "ai.audit.revert.many",
        R::Unsafe => "ai.audit.revert.unsafe",
    }
}

/// Takes back journal row `seq` alone while newer changes stay, as one undo step. Refused with a
/// reason when a newer change touched any row of it.
fn revert_audit(app: &mut EditorApp, shown: &mut Vec<(i64, i64, AuditEntry)>, seq: i64) {
    let rows = app.ed.model.audit_list(seq - 1, 1).unwrap_or_default();
    let Some((_, _, e)) = rows.into_iter().find(|r| r.0 == seq) else {
        return;
    };
    let live = e.op_seq.is_some() && app.ed.model.audit_op_live(seq).unwrap_or(false);
    let top = app.ed.model.last_op_seq().ok().flatten();
    let (Some(op), logic::UndoState::Covered) = (e.op_seq, logic::undo_state(e.op_seq, live, top)) else {
        return;
    };
    let check = match app.ed.model.revert_check(op) {
        Ok(c) => c,
        Err(_) => ddc_model::RevertCheck::Unsafe,
    };
    let key = revert_why(&check);
    if check == ddc_model::RevertCheck::Ready {
        let pushes = app.ed.pushes;
        let done = app.ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.revert_op(op))));
        if done.is_ok() {
            audit(app, "revert", &seq.to_string());
            *shown = audit_ui::load(&app.ed.model);
            app.ed.status = Some(app.sh.cat.t(key));
            let what = audit_ui::summary(&app.sh.cat, &e);
            let text = app.sh.cat.tf("ai.audit.toast.reverted", &[("what", &what)]);
            let undo = app.sh.cat.t("ai.audit.undo");
            let entry = app.ed.entry_since(pushes);
            app.sh.toasts.push_step(Level::Success, text, undo, "edit.undo", entry);
            return;
        }
        let text = app.sh.cat.t("ai.audit.revert.unsafe");
        app.sh.toasts.push(Level::Warning, text, None);
        return;
    }
    let table = match &check {
        ddc_model::RevertCheck::Conflict(t) => t.as_str(),
        _ => "",
    };
    let text = app.sh.cat.tf(key, &[("table", table)]);
    app.sh.toasts.push(Level::Warning, text, None);
}

fn h_revoke(a: &mut EditorApp) {
    super::mcp_bridge::revoke_now(a);
}

pub fn register(a: &mut EditorApp) {
    crate::ai::session::sweep_on_start();
    a.register_command("ai.ask", h_ask);
    a.register_command("ai.panel", h_panel);
    a.register_command("ai.explain", h_explain);
    a.register_command("ai.fix", h_fix);
    a.register_command("data.check", h_check);
    a.register_command("ai.settings", |a| act(a, Action::OpenSettings));
    a.register_command("ai.what_sent", |a| act(a, Action::OpenSent));
    a.register_command("ai.apply", |a| act(a, Action::Apply));
    a.register_command("ai.reject", |a| {
        let f = Facts::read();
        if plan_card::reject_command(f.plan.as_deref(), f.plan_rev) {
            act(a, Action::Reject);
        } else {
            // the card shows the question; it must be on screen
            with(|st| st.open = true);
        }
    });
    a.register_command("ai.cancel", |a| act(a, Action::Cancel));
    a.register_command("ai.next_change", |a| {
        palette_rows::run_draft(a, palette_rows::Draft::Next)
    });
    a.register_command("ai.show_draft", |a| {
        palette_rows::run_draft(a, palette_rows::Draft::Show)
    });
    a.register_command("ai.audit", |a| {
        with(|st| {
            st.init(a);
            st.audit = audit_ui::load(&a.ed.model);
            st.audit_ui.confirm = None;
            st.audit_open = true;
        });
    });
    a.register_command("ai.receipt_undo", |a| {
        let latest = with(|st| {
            st.init(a);
            receipt::latest(&st.log)
        });
        if let Some(seq) = latest {
            act(a, Action::UndoApplied(seq));
        }
    });
    a.register_command("ai.mcp_confirm", |a| act(a, Action::McpConfirm(true)));
    a.register_command("ai.mcp_revoke", h_revoke);
    a.register_command("ai.mode_chat", |a| modes::enter(a, modes::AiMode::Chat));
    a.register_command("ai.mode_build", |a| modes::enter(a, modes::AiMode::Build));
    a.register_command("ai.mode_review", |a| modes::enter(a, modes::AiMode::Review));
    modes::note_keys(&a.sh.reg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use ddc_model::{OptionRec, OptionStatus};

    const ENV: &str = r#"{"v":1,"actor":{"kind":"user"},"title":"opt","why":null,
        "groups":[{"id":1,"title":"g","depends_on":[],"ops":[{"op":"level","action":"create",
        "slot":1,"name":"tab-L9","elevation":{"v":9.0,"unit":"m"}}]}]}"#;

    #[test]
    fn the_dock_keeps_a_side_gutter_wider_than_its_top() {
        for mode in [ddc_ui::Mode::Light, ddc_ui::Mode::Dark] {
            let th = Theme::new(mode, ddc_ui::Density::Compact);
            let m = panel_frame(&th).inner_margin;
            assert_eq!(m.left, m.right, "{mode:?}: the gutter is even");
            assert!(f32::from(m.right) >= kit::space::MD, "{mode:?}: cards touch the edge");
            assert!(m.left > m.top, "{mode:?}: the sides breathe more than the rows");
        }
    }

    #[test]
    fn proposal_ok_toasts_success() {
        let mut a = app();
        let mut st = PanelUi::default();
        st.toasts.push(Ok(48213));
        st.flush_toasts(&mut a);
        let want = a.sh.cat.t("ai.proposal.toast.applied");
        assert_ne!(want, "ai.proposal.toast.applied");
        assert_eq!(a.sh.toasts.history, vec![(Level::Success, want.clone())]);
        assert!(!want.chars().any(|c| c.is_ascii_digit()), "no journal seq: {want}");
        assert!(want.contains("Ctrl+Z"), "names the undo: {want}");
    }

    /// Inserts into `table` fail from now on, as in a file that cannot be written.
    fn refuse(a: &EditorApp, table: &str) {
        let sql = format!(
            "CREATE TRIGGER no_{table} BEFORE INSERT ON {table} \
             BEGIN SELECT RAISE(ABORT, 'read-only'); END;"
        );
        a.ed.model.sql().execute_batch(&sql).unwrap();
    }

    #[test]
    fn an_apply_whose_log_entry_fails_warns_and_stays() {
        let mut a = app();
        let mut st = PanelUi::default();
        st.watch_project(&a);
        let _ = crate::ai::session::take_audit_lost();
        refuse(&a, "ai_audit");
        let undo = a.ed.undo_len();
        let e = AuditEntry {
            event: "apply".into(),
            status: "ok".into(),
            ..AuditEntry::default()
        };
        assert!(!crate::ai::session::audit_write(&a.ed.model, &e));
        st.flush_toasts(&mut a);
        let want = a.sh.cat.t("ai_safety.apply_unlogged");
        assert_ne!(want, "ai_safety.apply_unlogged");
        assert_eq!(a.sh.toasts.history, vec![(Level::Warning, want)]);
        assert_eq!(a.ed.undo_len(), undo, "the Apply is not taken back");
        st.flush_toasts(&mut a);
        assert_eq!(a.sh.toasts.history.len(), 1, "one warning per failure");
        audit(&a, "panel", "ok");
        st.flush_toasts(&mut a);
        let other = a.sh.cat.t("ai_safety.audit_unlogged");
        assert_eq!(a.sh.toasts.history.last(), Some(&(Level::Warning, other)));
    }

    #[test]
    fn a_failed_chat_save_is_said_once_and_retried() {
        let a = app();
        let mut st = PanelUi::default();
        st.watch_project(&a);
        refuse(&a, "ai_chat");
        st.log.push(Entry::User("q".into()));
        st.chat_dirty = true;
        st.save_chat(&a);
        assert!(st.chat_dirty, "still to be saved");
        st.save_chat(&a);
        let notes = st
            .log
            .iter()
            .filter(|e| matches!(e, Entry::Note("ai_safety.chat_not_saved")))
            .count();
        assert_eq!(notes, 1);
        a.ed.model.sql().execute_batch("DROP TRIGGER no_ai_chat;").unwrap();
        st.save_chat(&a);
        assert!(!st.chat_dirty);
        assert!(!a.ed.model.chat_load().unwrap().is_empty());
    }

    #[test]
    fn an_old_journal_loses_address_keys_on_open() {
        let a = app();
        let fake = "fake-SECRETSECRET-ai15j";
        let e = AuditEntry {
            event: "apply".into(),
            status: "ok".into(),
            endpoint: Some(format!("https://h.test/v1?key={fake}")),
            text: Some(format!("to https://u:{fake}@h.test/v1")),
            ..AuditEntry::default()
        };
        a.ed.model.audit_append(&e).unwrap();
        let mut st = PanelUi::default();
        st.watch_project(&a);
        let rows = a.ed.model.audit_list(0, 10).unwrap();
        assert_eq!(rows[0].2.endpoint.as_deref(), Some("https://h.test"));
        assert!(!format!("{rows:?}").contains(fake));
        assert!(!format!("{rows:?}").contains("?key"));
    }

    #[test]
    fn proposal_toast_has_no_digits_in_either_language() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let (lv, text) = proposal_toast(&cat, &Ok(48213), 3);
            assert_eq!(lv, Level::Success);
            assert!(!text.chars().any(|c| c.is_ascii_digit()), "{text}");
        }
    }

    #[test]
    fn proposal_not_refreshed_toasts_like_the_plan_draft() {
        let cat = ddc_ui::Catalog::new(ddc_ui::Lang::En);
        let e = AiError::AppliedNotRefreshed("gpu lost".into());
        let (lv, text) = proposal_toast(&cat, &Err(e.clone()), 4);
        let (plan_lv, key) = logic::applied_toast(Some(&e));
        assert_eq!((lv, text), (plan_lv, cat.tf(key, &[("n", "4")])));
        assert_eq!(lv, Level::Warning);
    }

    #[test]
    fn proposal_stale_toasts_hint() {
        let mut a = app();
        let mut st = PanelUi::default();
        st.toasts.push(Err(AiError::Stale));
        st.flush_toasts(&mut a);
        let want = a.sh.cat.t("ai.proposal.toast.stale");
        assert_ne!(want, "ai.proposal.toast.stale");
        assert_eq!(a.sh.toasts.history, vec![(Level::Warning, want)]);
    }

    #[test]
    fn proposal_refusal_toasts_reason() {
        let mut a = app();
        let mut st = PanelUi::default();
        st.toasts.push(Err(AiError::Model("wall 5 is locked".into())));
        st.flush_toasts(&mut a);
        let (lv, text) = a.sh.toasts.history[0].clone();
        assert_eq!(lv, Level::Warning);
        assert!(text.contains("wall 5 is locked"));
    }

    fn app() -> EditorApp {
        EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
    }

    /// The global Apply key with the panel closed: the refusal opens the panel, nothing moves.
    #[test]
    fn a_refused_apply_opens_the_panel_to_say_why() {
        let mut a = app();
        let cfg = crate::ai::SessionCfg {
            provider: ddc_ai::fake::ScriptedProvider::new(vec![]),
            profile: ddc_ai::fake::profile(),
            limits: ddc_ai::Limits::default(),
            session: "refused-apply".to_string(),
            allow_snapshots: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            extra_generators: vec![],
            extra_rules: vec![],
            keep_text: false,
            confirm_ttl: crate::ai::session::CONFIRM_TTL,
            target: ddc_ai::catalog::Target::Main,
            auto_replan: false,
        };
        let view = crate::ai::ViewCtx {
            view: "plan".to_string(),
            level: a.ed.level,
            units: a.ed.tool.units,
            ..Default::default()
        };
        let mut s = crate::ai::UiSession::start(&a.ed, cfg, view).unwrap();
        assert!(s.wait_idle(&mut a.ed, std::time::Duration::from_secs(60)));
        with_session(|x| *x = Some(s));
        let mut st = PanelUi::default();
        st.open = false;
        let head = (a.ed.model.change_head(), a.ed.model.op_top().unwrap());
        st.run(&mut a, Action::Apply);
        let ended = with_session(|x| x.take());
        drop(ended);
        assert_eq!(st.error, Some(AiError::EmptyPlan));
        assert!(st.open, "a refusal behind a closed panel is never seen");
        assert_eq!((a.ed.model.change_head(), a.ed.model.op_top().unwrap()), head);
        assert!(!a.ed.can_undo());
    }

    fn talk(st: &mut PanelUi, a: &EditorApp, n: usize) {
        for i in 0..n {
            st.log.push(Entry::User(format!("q{i}")));
            st.log.push(Entry::Assistant(format!("a{i}")));
        }
        st.log.push(Entry::Note("ai.plan.applied"));
        let mut step = steps::Step::start("t", 10);
        step.finish(true, None, 20);
        st.log.push(Entry::Tool(step));
        st.chat_dirty = true;
        st.trim_log();
        st.save_chat(a);
    }

    #[test]
    fn chat_log_survives_project_reopen() {
        let a = app();
        let mut st = PanelUi::default();
        st.watch_project(&a);
        talk(&mut st, &a, 2);
        let mut again = PanelUi::default();
        again.watch_project(&a);
        let rows = chat_rows(&again.log);
        assert_eq!(rows, chat_rows(&st.log));
        assert_eq!(rows.len(), 6, "the tool step is kept with the chat");
        assert!(matches!(again.log.last(), Some(Entry::Tool(s)) if s.ms == Some(10)));
    }

    #[test]
    fn a_failed_step_keeps_its_reason_through_chat_save_and_load() {
        let a = app();
        let mut st = PanelUi::default();
        st.watch_project(&a);
        let mut s = steps::Step::start("query_elements", 100);
        s.finish(false, Some("element 7 not found"), 350);
        st.log.push(Entry::User("find it".into()));
        st.log.push(Entry::Tool(s.clone()));
        st.chat_dirty = true;
        st.save_chat(&a);
        let mut again = PanelUi::default();
        again.watch_project(&a);
        let Some(Entry::Tool(back)) = again.log.last() else {
            panic!("the step is gone");
        };
        assert_eq!(back.state(), steps::State::Failed);
        assert_eq!(back.err.as_deref(), Some("element 7 not found"));
        assert_eq!(back.ms, Some(250));
    }

    #[test]
    fn history_without_step_rows_loads() {
        let a = app();
        a.ed.model
            .chat_save(&[
                ("user".to_string(), "hi".to_string()),
                ("assistant".to_string(), "hello".to_string()),
                ("tool".to_string(), r#"{"n":"get_context","ok":true}"#.to_string()),
                ("tool".to_string(), "garbage".to_string()),
            ])
            .unwrap();
        let mut st = PanelUi::default();
        st.watch_project(&a);
        assert_eq!(st.log.len(), 3, "an unreadable step row is dropped");
        assert!(matches!(&st.log[2], Entry::Tool(s) if s.ms.is_none() && s.ok == Some(true)));
    }

    #[test]
    fn a_failed_or_stopped_turn_closes_the_running_step() {
        let mut st = PanelUi::default();
        st.log.push(Entry::Tool(steps::Step::start("clash", 0)));
        st.close_steps(Some("HTTP status 503"));
        assert!(matches!(&st.log[0], Entry::Tool(s)
            if s.state() == steps::State::Failed && s.err.as_deref() == Some("HTTP status 503")));
        st.log.push(Entry::Tool(steps::Step::start("page", 0)));
        st.close_steps(None);
        assert!(matches!(&st.log[1], Entry::Tool(s) if s.state() == steps::State::Cancelled));
        assert!(matches!(&st.log[0], Entry::Tool(s) if s.state() == steps::State::Failed));
    }

    #[test]
    fn a_long_run_saves_few_steps_and_keeps_the_failure() {
        let mut log = vec![Entry::User("go".into())];
        for i in 0..64u64 {
            let mut s = steps::Step::start("get_element", i);
            s.finish(i != 3, Some(&"x".repeat(5_000)), i + 1);
            log.push(Entry::Tool(s));
        }
        log.push(Entry::Assistant("done".into()));
        let rows = chat_rows(&log);
        let tools: Vec<&(String, String)> = rows.iter().filter(|r| r.0 == "tool").collect();
        assert_eq!(tools.len(), steps::SAVED_PER_RUN);
        assert_eq!(rows.len(), steps::SAVED_PER_RUN + 2, "text rows stay");
        assert!(tools.iter().any(|r| r.1.contains("\"ok\":false")), "failure kept");
        assert!(tools.iter().all(|r| r.1.len() < 1_000), "row size is bounded");
    }

    fn sample_receipt() -> receipt::Receipt {
        receipt::Receipt {
            op_seq: 12,
            stamp: 77,
            n: 5,
            cats: vec![("wall".to_string(), 4), ("opening".to_string(), 1)],
            ids: vec![("wall".to_string(), 1), ("opening".to_string(), 2)],
            undone: false,
            drafts: vec![],
        }
    }

    #[test]
    fn redraft_note_survives_project_reopen() {
        let a = app();
        let mut st = PanelUi::default();
        st.watch_project(&a);
        let n = plan_delta::Note {
            added: 2,
            removed: 1,
            changed: 3,
        };
        st.log.push(Entry::User("also add a roof".into()));
        st.log.push(Entry::Redraft(n));
        st.chat_dirty = true;
        st.save_chat(&a);
        let mut again = PanelUi::default();
        again.watch_project(&a);
        assert_eq!(chat_rows(&again.log), chat_rows(&st.log));
        assert!(matches!(again.log.last(), Some(Entry::Redraft(b)) if *b == n));
    }

    #[test]
    fn receipt_entry_survives_project_reopen() {
        let a = app();
        let mut st = PanelUi::default();
        st.watch_project(&a);
        st.log.push(Entry::User("make walls".into()));
        st.log.push(Entry::Applied(sample_receipt()));
        st.chat_dirty = true;
        st.save_chat(&a);
        let mut again = PanelUi::default();
        again.watch_project(&a);
        assert_eq!(chat_rows(&again.log), chat_rows(&st.log));
        assert!(matches!(again.log.last(), Some(Entry::Applied(r)) if *r == sample_receipt()));
    }

    #[test]
    fn old_history_without_receipts_still_loads() {
        let a = app();
        a.ed.model
            .chat_save(&[
                ("user".to_string(), "hi".to_string()),
                ("assistant".to_string(), "hello".to_string()),
                ("note".to_string(), "ai.plan.applied".to_string()),
                ("applied".to_string(), "garbage".to_string()),
            ])
            .unwrap();
        let mut st = PanelUi::default();
        st.watch_project(&a);
        assert_eq!(st.log.len(), 3, "a broken receipt row is dropped, the rest loads");
        assert!(matches!(st.log.last(), Some(Entry::Note("ai.plan.applied"))));
    }

    #[test]
    fn receipt_undone_flag_is_stored() {
        let a = app();
        let mut st = PanelUi::default();
        st.watch_project(&a);
        let mut r = sample_receipt();
        r.undone = true;
        st.log.push(Entry::Applied(r));
        st.chat_dirty = true;
        st.save_chat(&a);
        let mut again = PanelUi::default();
        again.watch_project(&a);
        assert!(matches!(again.log.last(), Some(Entry::Applied(r)) if r.undone));
    }

    #[test]
    fn chat_log_not_leaked_to_other_project() {
        let a = app();
        let mut st = PanelUi::default();
        st.watch_project(&a);
        talk(&mut st, &a, 1);
        let b = app();
        let mut other = PanelUi::default();
        other.watch_project(&b);
        assert!(other.log.is_empty());
        // the same panel pointed at another project drops the old log
        st.project = Some("elsewhere".into());
        st.watch_project(&b);
        assert!(st.log.is_empty());
    }

    #[test]
    fn chat_log_capped_and_has_no_secret() {
        let a = app();
        ddc_model::set_audit_scrubber(ddc_ai::secret::scrub_known);
        let key = "sk-test-chat-secret-0123456789";
        let _s = ddc_ai::secret::Secret::new(key);
        let mut st = PanelUi::default();
        st.watch_project(&a);
        talk(&mut st, &a, LOG_KEPT);
        assert_eq!(st.log.len(), LOG_KEPT, "memory log is capped");
        assert_eq!(a.ed.model.chat_load().unwrap().len(), LOG_KEPT);
        st.log.push(Entry::User(format!("my key is {key}")));
        st.chat_dirty = true;
        st.save_chat(&a);
        let stored = a.ed.model.chat_load().unwrap();
        assert!(stored.len() <= ddc_model::CHAT_ROWS);
        let last = &stored.last().unwrap().1;
        assert!(last.contains("***") && !last.contains(key));
    }

    #[test]
    fn chat_save_is_not_an_undo_step() {
        let a = app();
        let (head, seq) = (a.ed.model.change_head(), a.ed.model.last_op_seq().unwrap());
        let mut st = PanelUi::default();
        st.watch_project(&a);
        talk(&mut st, &a, 3);
        assert_eq!(a.ed.model.change_head(), head);
        assert_eq!(a.ed.model.last_op_seq().unwrap(), seq);
        assert!(a.ed.model.changes_since(head).is_empty());
        assert!(!a.ed.model.chat_load().unwrap().is_empty());
    }

    fn option(app: &EditorApp) -> i64 {
        let (base_head, base_stamp) = app.ed.model.ai_option_head().unwrap();
        let rec = OptionRec {
            id: 0,
            set: "s".into(),
            name: "A".into(),
            base_head,
            base_stamp,
            envelope: ENV.into(),
            seed: 0,
            params: "{}".into(),
            generator: Some("test".into()),
            metrics: None,
            note: None,
            why: None,
            status: OptionStatus::Draft,
        };
        app.ed.model.ai_option_add(&rec).unwrap()
    }

    fn status(app: &EditorApp, id: i64) -> OptionStatus {
        app.ed.model.ai_option_get(id).unwrap().unwrap().status
    }

    #[test]
    fn options_tab_lists_drafts() {
        let a = app();
        let (x, y) = (option(&a), option(&a));
        let mut c = compare::OptCache::default();
        assert!(c.sync(&a.ed.model));
        let ids: Vec<i64> = c.rows.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![x, y]);
        assert!(c.rejected.is_empty());
    }

    #[test]
    fn rows_not_reloaded_without_head_change() {
        let a = app();
        option(&a);
        let mut c = compare::OptCache::default();
        assert!(c.sync(&a.ed.model));
        assert!(!c.sync(&a.ed.model));
        assert_eq!(c.loads, 1);
        option(&a);
        assert!(c.sync(&a.ed.model), "a new option reloads");
        assert_eq!((c.loads, c.rows.len()), (2, 2));
    }

    #[test]
    fn options_sync_on_the_editor_key_sees_new_rows() {
        let a = app();
        option(&a);
        let mut c = compare::OptCache::default();
        assert!(c.sync_ed(&a.ed));
        assert!(!c.sync_ed(&a.ed));
        option(&a);
        assert!(c.sync_ed(&a.ed), "a new option moves the editor key");
        assert_eq!((c.loads, c.rows.len()), (2, 2));
    }

    #[test]
    fn discard_keeps_row_rejected() {
        let mut a = app();
        let id = option(&a);
        let mut st = PanelUi::default();
        st.cmp.cache.sync(&a.ed.model);
        st.run(&mut a, Action::DiscardOption(id));
        assert_eq!(status(&a, id), OptionStatus::Rejected);
        assert_eq!(st.cmp.cache.rows.len(), 1, "row kept");
        assert!(st.cmp.cache.rejected.contains(&id));
        assert!(!st.cmp.cache.sync(&a.ed.model), "no reload needed");
    }

    #[test]
    fn apply_action_routes_to_option_exec() {
        let mut a = app();
        let id = option(&a);
        let mut st = PanelUi::default();
        st.run(&mut a, Action::ApplyOption(id));
        assert_eq!(status(&a, id), OptionStatus::Accepted);
        let n: i64 = a
            .ed
            .model
            .sql()
            .query_row("SELECT count(*) FROM level WHERE name='tab-L9'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        st.run(&mut a, Action::DiscardOption(id));
        assert_eq!(status(&a, id), OptionStatus::Accepted, "an accepted option is not rejected");
    }
}

/// Family drafts the assistant offered since the last call; the family editor opens them.
/// The person's answer to an agent's family offer (open it, or dismiss it).
pub fn answer_family_offer(ctx: &egui::Context) {
    let id = egui::Id::new("ddc.fam_offer");
    let Some((name, open)) = ctx.data_mut(|d| d.remove_temp::<(String, bool)>(id)) else {
        return;
    };
    with(|p| {
        if let Some(i) = p.family_offers.iter().position(|d| d.0 == name) {
            let d = p.family_offers.remove(i);
            if open {
                p.family_drafts.push(d);
            }
        }
    });
}

pub fn take_family_drafts() -> Vec<(String, String, Option<String>, (u32, u64))> {
    with(|p| std::mem::take(&mut p.family_drafts))
}
