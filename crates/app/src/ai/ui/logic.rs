// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Everything in the panel that can be decided without a frame: which state it is in, how an
//! error reads, how a plan is counted. The tests pin these without a window.

use crate::ai::plan::{ElemDiff, PlanItem, QtyDelta};
use crate::ai::validate::{IssueLevel, PlanIssue};
use crate::ai::AiError;
use ddc_ai::{Mode, TurnEnd};
use ddc_model::{ElKey, RowKind};
use ddc_ui::Catalog;
use std::collections::HashSet;

/// Every state the panel can be in, each with its own face (§11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelState {
    NoProfile,
    NeedConsent,
    LocalOnlyBlocked,
    Preparing,
    Idle,
    Streaming,
    Tool,
    PlanReady,
    Recomputing,
    Stale,
    Error,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StateIn {
    pub has_profile: bool,
    pub needs_consent: bool,
    pub local_blocked: bool,
    pub preparing: bool,
    pub busy: bool,
    pub tool_running: bool,
    pub has_steps: bool,
    pub can_apply: bool,
    pub stale: bool,
    pub blocked: bool,
    pub error: bool,
}

/// Order matters: a running turn wins over its previous plan, a stale plan over a ready one.
pub fn panel_state(s: StateIn) -> PanelState {
    if !s.has_profile {
        return PanelState::NoProfile;
    }
    if s.local_blocked {
        return PanelState::LocalOnlyBlocked;
    }
    if s.needs_consent {
        return PanelState::NeedConsent;
    }
    if s.preparing {
        return PanelState::Preparing;
    }
    if s.busy {
        return if s.tool_running {
            PanelState::Tool
        } else {
            PanelState::Streaming
        };
    }
    if s.error {
        return PanelState::Error;
    }
    if s.has_steps {
        if s.stale {
            return PanelState::Stale;
        }
        // a blocked plan is ready to read: the card names why Apply is off
        if s.can_apply || s.blocked {
            return PanelState::PlanReady;
        }
        return PanelState::Recomputing;
    }
    PanelState::Idle
}

pub const fn state_key(s: PanelState) -> &'static str {
    match s {
        PanelState::NoProfile => "ai.state.no_profile",
        PanelState::NeedConsent => "ai.state.need_consent",
        PanelState::LocalOnlyBlocked => "ai.state.local_only",
        PanelState::Preparing => "ai.state.preparing",
        PanelState::Idle => "ai.state.idle",
        PanelState::Streaming => "ai.state.streaming",
        PanelState::Tool => "ai.state.tool",
        PanelState::PlanReady => "ai.state.plan_ready",
        PanelState::Recomputing => "ai.state.recomputing",
        PanelState::Stale => "ai.state.stale",
        PanelState::Error => "ai.state.error",
    }
}

/// What a failure is about, for the title and the action next to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrKind {
    Auth,
    /// The key's own spending limit is used up.
    KeyLimit,
    /// The account has no money left.
    NoCredit,
    RateLimit,
    Context,
    NoTools,
    Blocked,
    Network,
    Server,
    Protocol,
    Stale,
    PlanBlocked,
    Busy,
    NeedsConfirm,
    Empty,
    Model,
    TurnLimit,
    Disk,
    /// Committed with its undo step; only the editor's reload failed.
    NotRefreshed,
    /// Agent items joined the card after the person looked.
    ReviewNeeded,
    /// Apply was taken back: it differed from the draft (the detail names how).
    Diverged,
    Internal,
}

/// Provider failures reach the UI as text: `AiError::Provider` carries the error's `Display`.
pub fn classify(e: &AiError) -> ErrKind {
    match e {
        AiError::EmptyPlan => ErrKind::Empty,
        AiError::Stale => ErrKind::Stale,
        AiError::Blocked => ErrKind::PlanBlocked,
        AiError::Busy => ErrKind::Busy,
        AiError::ReviewNeeded => ErrKind::ReviewNeeded,
        AiError::AppliedNotRefreshed(_) => ErrKind::NotRefreshed,
        AiError::NeedsConfirmation => ErrKind::NeedsConfirm,
        AiError::Diverged(_) => ErrKind::Diverged,
        AiError::Model(_) => ErrKind::Model,
        AiError::Internal(t) => {
            let t = t.to_lowercase();
            if t.contains("no space") || t.contains("disk") {
                ErrKind::Disk
            } else {
                ErrKind::Internal
            }
        }
        AiError::Provider(t) => {
            let p = |s: &str| t.starts_with(s);
            if p("authentication failed") {
                ErrKind::Auth
            } else if p("key spending limit reached") {
                ErrKind::KeyLimit
            } else if p("account balance used up") {
                ErrKind::NoCredit
            } else if p("rate limited") {
                ErrKind::RateLimit
            } else if p("request exceeds the model context") {
                ErrKind::Context
            } else if p("model does not support tool calling") {
                ErrKind::NoTools
            } else if p("request blocked") {
                ErrKind::Blocked
            } else if p("network error") {
                ErrKind::Network
            } else if p("http status") {
                ErrKind::Server
            } else if p("protocol error") {
                ErrKind::Protocol
            } else {
                ErrKind::Internal
            }
        }
    }
}

/// Longest provider text shown under the hint, in characters.
const DETAIL_MAX: usize = ddc_ai::limits::UI_DETAIL.us();

/// The failure's own text (HTTP status, server message, retry-after) as one masked line.
pub fn err_detail(e: &AiError) -> Option<String> {
    let raw = match e {
        AiError::Provider(t) | AiError::Model(t) | AiError::Internal(t) | AiError::Diverged(t) => t,
        _ => return None,
    };
    let one_line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let masked = mask_secrets(&one_line);
    if masked.is_empty() {
        return None;
    }
    if masked.chars().count() > DETAIL_MAX {
        let cut: String = masked.chars().take(DETAIL_MAX - 1).collect();
        return Some(format!("{cut}\u{2026}"));
    }
    Some(masked)
}

fn is_tok(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '~' | '+' | '/')
}

fn secret_like(w: &str) -> bool {
    let l = w.to_ascii_lowercase();
    const PREFIX: [&str; 8] = ["sk-", "pk-", "rk-", "ghp_", "gho_", "xox", "akia", "eyj"];
    PREFIX.iter().any(|p| l.starts_with(p))
        || (w.len() >= 32 && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
}

/// Replaces key-like words with `***`: `sk-...` prefixes, long opaque tokens, the word after
/// `Bearer`/`Basic` and the value after `key:`/`token=` style names.
pub fn mask_secrets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut hide_next = false;
    let mut rest = s;
    while !rest.is_empty() {
        let tok_len: usize = rest
            .chars()
            .take_while(|c| is_tok(*c))
            .map(char::len_utf8)
            .sum();
        if tok_len == 0 {
            let c = rest.chars().next().unwrap();
            out.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        }
        let (w, tail) = rest.split_at(tok_len);
        let lw = w.to_ascii_lowercase();
        if hide_next && (lw == "bearer" || lw == "basic") {
            // `Authorization: Bearer <token>`: the scheme stays, the token after it goes
            out.push_str(w);
        } else if hide_next || secret_like(w) {
            out.push_str("***");
            hide_next = false;
        } else {
            out.push_str(w);
            let sep: String = tail.chars().take_while(|c| !is_tok(*c)).collect();
            let named = ["key", "token", "secret", "password", "auth"]
                .iter()
                .any(|n| lw.contains(n));
            hide_next = lw == "bearer"
                || lw == "basic"
                || (named && sep.contains([':', '=']));
        }
        rest = tail;
    }
    out
}

pub const fn err_key(k: ErrKind) -> &'static str {
    match k {
        ErrKind::Auth => "ai.err.auth",
        ErrKind::KeyLimit => "ai.err.key_limit",
        ErrKind::NoCredit => "ai.err.no_credit",
        ErrKind::RateLimit => "ai.err.rate_limit",
        ErrKind::Context => "ai.err.context",
        ErrKind::NoTools => "ai.err.no_tools",
        ErrKind::Blocked => "ai.err.blocked",
        ErrKind::Network => "ai.err.network",
        ErrKind::Server => "ai.err.server",
        ErrKind::Protocol => "ai.err.protocol",
        ErrKind::Stale => "ai.err.stale",
        ErrKind::PlanBlocked => "ai.err.plan_blocked",
        ErrKind::Busy => "ai.err.busy",
        ErrKind::NeedsConfirm => "ai.err.needs_confirm",
        ErrKind::Empty => "ai.err.empty",
        ErrKind::Model => "ai.err.model",
        ErrKind::TurnLimit => "ai.err.turn_limit",
        ErrKind::Disk => "ai.err.disk",
        ErrKind::NotRefreshed => "ai.err.not_refreshed",
        ErrKind::ReviewNeeded => "ai.err.review_needed",
        ErrKind::Diverged => "apply_verify.err.diverged",
        ErrKind::Internal => "ai.err.internal",
    }
}

/// The toast of a committed Apply: a warning when the view did not refresh (the change is in the
/// model), never a success. `error` is what the Apply left on the panel.
pub fn applied_toast(error: Option<&AiError>) -> (ddc_ui::notify::Level, &'static str) {
    use ddc_ui::notify::Level;
    match error {
        Some(AiError::AppliedNotRefreshed(_)) => (Level::Warning, "ai_toast.toast_not_refreshed"),
        _ => (Level::Success, "ai.plan.applied"),
    }
}

/// What a provider failure says beyond its title: the HTTP code, the server's own words and the
/// wait it asked for. Shown on an expandable line, never as the headline.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ErrDetail {
    pub status: Option<u16>,
    pub retry_after_s: Option<u32>,
    pub text: String,
}

impl ErrDetail {
    pub fn is_empty(&self) -> bool {
        self.status.is_none() && self.retry_after_s.is_none() && self.text.is_empty()
    }
}

pub fn error_detail(e: &AiError) -> ErrDetail {
    let mut d = ErrDetail::default();
    match e {
        AiError::AppliedNotRefreshed(t) | AiError::Diverged(t) => d.text = t.clone(),
        AiError::Provider(t) => {
            if let Some(rest) = t.strip_prefix("http status ") {
                let (code, body) = rest.split_once(':').unwrap_or((rest, ""));
                d.status = code.trim().parse().ok();
                d.text = body.trim().to_string();
            } else if let Some(rest) = t.strip_prefix("rate limited") {
                d.status = Some(429);
                d.retry_after_s = rest
                    .split("retry after ")
                    .nth(1)
                    .and_then(|r| r.split_whitespace().next())
                    .and_then(|n| n.parse().ok());
            } else if let Some((_, rest)) = t.split_once(": ") {
                d.text = rest.trim().to_string();
            }
        }
        _ => {}
    }
    let cut = d.text.char_indices().nth(400).map_or(d.text.len(), |(i, _)| i);
    d.text.truncate(cut);
    d
}

/// The fix of these errors is in the connection settings, not in another try.
pub const fn goes_to_settings(k: ErrKind) -> bool {
    matches!(k, ErrKind::Auth | ErrKind::NoTools | ErrKind::Blocked)
}

pub const fn can_retry(k: ErrKind) -> bool {
    matches!(
        k,
        ErrKind::RateLimit
            | ErrKind::Network
            | ErrKind::Server
            | ErrKind::Protocol
            | ErrKind::Model
            | ErrKind::Context
            | ErrKind::TurnLimit
    )
}

/// The detail line of a turn stopped by a limit; the names are `Limits`' own.
pub fn turn_limit_key(what: &str) -> &'static str {
    match what {
        "tokens" => "ai.err.limit_tokens",
        "cost" => "ai.err.limit_cost",
        "wall" => "ai.err.limit_time",
        "output" => "ai.err.limit_output",
        "filtered" => "ai.err.limit_filtered",
        "empty" => "ai.err.limit_empty",
        _ => "ai.err.limit_steps",
    }
}

/// The value of the limit that stopped a turn, as the message shows it.
pub fn turn_limit_value(what: &str, l: &ddc_ai::Limits, max_output: u32) -> String {
    match what {
        "tokens" => l.max_turn_tokens.to_string(),
        "wall" => l.max_wall_s.to_string(),
        "cost" => l.max_cost.map(|c| format!("{c:.2}")).unwrap_or_default(),
        "output" => max_output.to_string(),
        _ => l.max_steps.to_string(),
    }
}

/// `{0}`, `{1}`: the positional arguments of issue texts.
/// The card's self-check line: rounds the critic ran and the findings still open.
pub fn self_check_text(cat: &Catalog, sc: crate::ai::critic_hook::SelfCheck) -> String {
    let rounds = sc.rounds.to_string();
    if sc.left == 0 {
        cat.tf("ai.plan.self_check_clear", &[("rounds", &rounds)])
    } else {
        cat.tf("ai.plan.self_check", &[("rounds", &rounds), ("left", &sc.left.to_string())])
    }
}

pub fn fill(template: &str, args: &[String]) -> String {
    let mut out = template.to_string();
    for (i, a) in args.iter().enumerate() {
        out = out.replace(&format!("{{{i}}}"), a);
    }
    out
}

pub fn issue_text(cat: &Catalog, issue: &PlanIssue) -> String {
    fill(&cat.t(issue.text_key), &issue.args)
}

/// Blocking first: the reason Apply is off is the first thing read.
pub fn sorted_issues(issues: &[PlanIssue]) -> Vec<&PlanIssue> {
    let mut v: Vec<&PlanIssue> = issues.iter().collect();
    v.sort_by_key(|i| match i.level {
        IssueLevel::Block => 0,
        IssueLevel::Warn => 1,
    });
    v
}

/// Whether the card offers the large-plan confirm: the size cap is the only thing in the way
/// (either gate), or the confirm is already on and the person may take it back.
pub fn allow_large_offer(
    issues: &[PlanIssue],
    main_issues: &[PlanIssue],
    allow_large: bool,
) -> bool {
    let all = || issues.iter().chain(main_issues);
    let size = |i: &PlanIssue| matches!(i.text_key, "ai.issue.too_large" | "ai.issue.delete_large");
    let block = |i: &&PlanIssue| i.level == IssueLevel::Block;
    allow_large || (all().filter(block).any(size) && all().filter(block).all(size))
}

/// Per kind, in the `+ ~ −` order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub created: usize,
    pub changed: usize,
    pub deleted: usize,
}

impl Counts {
    pub fn add(&mut self, k: RowKind) {
        match k {
            RowKind::Created => self.created += 1,
            RowKind::Changed => self.changed += 1,
            RowKind::Deleted => self.deleted += 1,
        }
    }

    pub const fn total(self) -> usize {
        self.created + self.changed + self.deleted
    }
}

/// Elements of the item, without the tables that are not elements (types, parameters).
pub fn item_counts(diff: &[ElemDiff], item: u32) -> Counts {
    let mut c = Counts::default();
    diff.iter()
        .filter(|e| e.item == Some(item))
        .for_each(|e| c.add(e.kind));
    c
}

pub fn rows_of(diff: &[ElemDiff], item: u32) -> Vec<&ElemDiff> {
    diff.iter().filter(|e| e.item == Some(item)).collect()
}

pub fn keys_of(diff: &[ElemDiff], item: u32) -> Vec<ElKey> {
    diff.iter()
        .filter(|e| e.item == Some(item))
        .map(|e| e.key.clone())
        .collect()
}

pub fn hot_set(keys: impl IntoIterator<Item = ElKey>) -> HashSet<ElKey> {
    keys.into_iter().collect()
}

/// `door:55 EI15 → EI30`: the first changed value is the row's detail.
pub fn cell_summary(e: &ElemDiff) -> String {
    let Some(c) = e.cells.first() else {
        return String::new();
    };
    let v = |x: &Option<String>| x.clone().unwrap_or_else(|| "—".to_string());
    format!("{}: {} → {}", c.col, v(&c.before), v(&c.after))
}

/// Which items are off and how the check marks would change when one is flipped: unchecking pulls
/// the items that depend on it, checking pulls what it depends on.
pub fn toggled(items: &[PlanItem], id: u32) -> Vec<(u32, bool)> {
    let Some(me) = items.iter().find(|i| i.id == id) else {
        return vec![];
    };
    let on = !me.checked;
    let mut out = vec![(id, on)];
    let mut seen: HashSet<u32> = HashSet::from([id]);
    let mut i = 0;
    while i < out.len() {
        let cur = out[i].0;
        for it in items {
            let linked = if on {
                items
                    .iter()
                    .find(|x| x.id == cur)
                    .is_some_and(|x| x.depends_on.contains(&it.id))
            } else {
                it.depends_on.contains(&cur)
            };
            if linked && seen.insert(it.id) {
                out.push((it.id, on));
            }
        }
        i += 1;
    }
    out
}

/// Material and total deltas worth a place in the summary line; the headline ones first.
pub fn headline_qty(qty: &[QtyDelta], max: usize) -> Vec<&QtyDelta> {
    let mut v: Vec<&QtyDelta> = qty
        .iter()
        .filter(|q| (q.after - q.before).abs() > 1e-9)
        .collect();
    v.sort_by(|a, b| {
        let rank = |q: &QtyDelta| u8::from(!q.key.starts_with("volume:"));
        rank(a).cmp(&rank(b)).then(
            (b.after - b.before)
                .abs()
                .total_cmp(&(a.after - a.before).abs()),
        )
    });
    v.truncate(max);
    v
}

/// Floor-area delta and, when a changed material has a price, the cost delta (volume delta x
/// price per m3). `None` cost: nothing priced moved.
pub fn area_cost_delta(
    qty: &[QtyDelta],
    prices: &std::collections::BTreeMap<String, f64>,
) -> (f64, Option<f64>) {
    let mut area = 0.0;
    let mut cost: Option<f64> = None;
    for q in qty {
        let d = q.after - q.before;
        if q.key == "slab_area" {
            area += d;
        } else if let Some(p) = q.key.strip_prefix("volume:").and_then(|m| prices.get(m)) {
            *cost.get_or_insert(0.0) += d * p;
        }
    }
    (area, cost)
}

/// One `Name: value` per line, the form the settings' header box uses. A line without a colon is
/// skipped: half a header must not reach the wire.
pub fn parse_headers(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect()
}

pub fn headers_text(h: &[(String, String)]) -> String {
    h.iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn tool_key(name: &str) -> String {
    format!("ai.tool.{name}")
}

/// Who made a journal entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Person,
    Agent(String),
}

pub fn audit_source(actor: &str) -> Source {
    match actor.strip_prefix(crate::ai::session::ACTOR_PREFIX) {
        Some(name) => Source::Agent(name.to_string()),
        None => Source::Person,
    }
}

/// The agent's name of an item's actor; the person's own items have none.
pub fn agent_of(actor: &str) -> Option<&str> {
    actor.strip_prefix(crate::ai::session::ACTOR_PREFIX)
}

/// Names of the agents behind the items still waiting for the person's look.
pub fn proposer_line(items: &[PlanItem], ids: &[u32]) -> String {
    let mut names: Vec<&str> = vec![];
    for i in items.iter().filter(|i| ids.contains(&i.id)) {
        if let Some(n) = agent_of(&i.actor) {
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    names.join(", ")
}

/// Keys of a card act only while keyboard focus is inside it: `focused_in` is `Some(inside)` for
/// a focused widget and `None` when nothing has focus. A text field elsewhere keeps its keys.
pub fn key_scope(focused_in: Option<bool>) -> bool {
    focused_in == Some(true)
}

/// The Stop key caps: Esc where the card's keys reach, the global chord from anywhere.
pub const STOP_KEYS: &str = "Esc / Ctrl+Alt+.";

/// Where the keyboard is when the card's Ctrl+Enter and Esc arrive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyAt {
    /// Focus in the panel: `Some(true)`; in another widget: `Some(false)`; nothing: `None`.
    pub focus_in: Option<bool>,
    /// A text field has the keyboard.
    pub typing: bool,
    /// That field is the panel's composer, whose Ctrl+Enter has always applied.
    pub composer: bool,
    /// No dialog, popup or menu holds the keys.
    pub canvas_free: bool,
    /// A canvas tool, drag or selection takes Esc first.
    pub canvas_esc: bool,
}

/// Which card keys this frame may take: `(Ctrl+Enter applies, Esc stops)`. They reach the plan
/// and 3D view too, so looking at the preview does not lose them; a text field keeps them.
pub fn card_keys(at: KeyAt) -> (bool, bool) {
    if at.typing && !(at.composer && at.focus_in == Some(true)) {
        return (false, false);
    }
    if key_scope(at.focus_in) {
        return (true, true);
    }
    if !at.canvas_free {
        return (false, false);
    }
    (true, !at.canvas_esc)
}

/// Plain Enter applies the draft and Esc discards it only outside the panel on a bare canvas: no
/// text field, no dialog, and no tool, drag or selection that takes those keys itself.
pub fn draft_keys_on_canvas(at: KeyAt) -> bool {
    !at.typing && at.focus_in != Some(true) && at.canvas_free && !at.canvas_esc
}

/// The settings tab's choices as the panel's actions.
pub fn access_actions(renew: bool, policy: Option<ddc_ai::mcp::McpPolicy>) -> Vec<super::Action> {
    let mut v = vec![];
    if renew {
        v.push(super::Action::McpRevoke);
    }
    if let Some(p) = policy {
        v.push(super::Action::McpConfirm(p == ddc_ai::mcp::McpPolicy::Confirm));
    }
    v
}

/// Entries that are a changeset: an apply or a rejection. Chat turns and settings stay out.
pub fn is_changeset(event: &str) -> bool {
    matches!(event, "apply" | "reject")
}

/// `steps=3` as written by the apply record.
pub fn steps_of(plan: &str) -> Option<u32> {
    plan.strip_prefix("steps=")?.trim().parse().ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoState {
    /// Nothing was written: a rejection or a failed apply.
    NoChange,
    /// Already undone, or its undo step is gone.
    Gone,
    /// Newer changes sit on top: the undo log is a stack, so they go first.
    Covered,
    Ready,
}

/// Only the newest step of the undo log can be taken back without touching later work.
pub fn undo_state(op_seq: Option<i64>, live: bool, top: Option<i64>) -> UndoState {
    match (op_seq, live) {
        (None, _) => UndoState::NoChange,
        (Some(_), false) => UndoState::Gone,
        (Some(s), true) if top == Some(s) => UndoState::Ready,
        (Some(_), true) => UndoState::Covered,
    }
}

/// Calendar fields of a moment: year, month, day, hour, minute.
pub type Civil = (i64, i64, i64, i64, i64);

/// UTC calendar fields of Unix seconds (civil-from-days, proleptic Gregorian).
pub fn civil_utc(ts: i64) -> Civil {
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, secs / 3600, secs % 3600 / 60)
}

fn fmt_civil((y, m, d, h, mi): Civil) -> String {
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}")
}

/// `2026-11-05 14:03 UTC` from Unix seconds.
pub fn fmt_utc(ts: i64) -> String {
    format!("{} UTC", fmt_civil(civil_utc(ts)))
}

/// Local time of the machine, in the zone rules of that date (summer time included), as
/// `2026-11-05 17:03`. Where the system gives no zone the time stays UTC and says so.
pub fn fmt_local(ts: i64) -> String {
    match local_civil(ts) {
        Some(c) => fmt_civil(c),
        None => fmt_utc(ts),
    }
}

#[cfg(windows)]
fn local_civil(ts: i64) -> Option<Civil> {
    #[repr(C)]
    #[derive(Default)]
    struct SysTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        millis: u16,
    }
    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn FileTimeToSystemTime(file: *const FileTime, out: *mut SysTime) -> i32;
        fn SystemTimeToTzSpecificLocalTime(
            zone: *const core::ffi::c_void,
            utc: *const SysTime,
            local: *mut SysTime,
        ) -> i32;
    }
    // 100 ns ticks since 1601-01-01
    let ticks = u64::try_from(ts.checked_add(11_644_473_600)?)
        .ok()?
        .checked_mul(10_000_000)?;
    let ft = FileTime {
        low: ticks as u32,
        high: (ticks >> 32) as u32,
    };
    let (mut utc, mut local) = (SysTime::default(), SysTime::default());
    // SAFETY: both calls only read the first pointer and write the second, which are live,
    // correctly laid out local values; a null zone means the current one.
    let ok = unsafe {
        FileTimeToSystemTime(&ft, &mut utc) != 0
            && SystemTimeToTzSpecificLocalTime(core::ptr::null(), &utc, &mut local) != 0
    };
    ok.then(|| {
        (
            i64::from(local.year),
            i64::from(local.month),
            i64::from(local.day),
            i64::from(local.hour),
            i64::from(local.minute),
        )
    })
}

#[cfg(not(windows))]
fn local_civil(_ts: i64) -> Option<Civil> {
    None
}

/// Thousands as `1,2k`, in the person's decimal separator.
pub fn fmt_tokens(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let k = n as f64 / 1000.0;
    format!("{}k", crate::units::fmt_num(k, 1))
}

/// Dollars to the cent in the person's decimal separator; a price under half a cent reads `<$0.01`.
pub fn fmt_cost(usd: f64) -> String {
    if usd < 0.005 {
        return format!("<${}", crate::units::fmt_num(0.01, 2));
    }
    format!("${}", crate::units::fmt_num(usd, 2))
}

/// The signed delta of a card line: a real minus, one sign always.
pub fn signed(v: f64, decimals: usize) -> String {
    let s = crate::units::fmt_num(v.abs(), decimals);
    if v < 0.0 {
        format!("−{s}")
    } else {
        format!("+{s}")
    }
}

/// A short readable name for a new session or profile: never a timestamp of the wall clock.
pub fn profile_id(existing: &[String]) -> String {
    ddc_ai::profile::free_id(existing)
}

pub fn policy_on(v: Option<&str>, default: bool) -> bool {
    match v {
        Some("1" | "on" | "true") => true,
        Some("0" | "off" | "false") => false,
        _ => default,
    }
}

/// Policy key of "this project may send to this address".
pub fn consent_policy(host: &str) -> String {
    format!("egress.consent.{host}")
}

/// `!text` in the palette asks right away; a phrase is offered after the search rows.
pub fn palette_offer(query: &str, parses: bool) -> Option<(String, bool)> {
    let q = query.trim();
    if let Some(rest) = q.strip_prefix('!') {
        let rest = rest.trim();
        return (!rest.is_empty()).then(|| (rest.to_string(), true));
    }
    if q.is_empty() {
        return None;
    }
    (q.split_whitespace().count() >= 3 || !parses).then(|| (q.to_string(), false))
}

pub const fn mode_key(m: Mode) -> &'static str {
    match m {
        Mode::ReadOnly => "ai.mode.read_only",
        Mode::Propose => "ai.mode.propose",
    }
}

/// The limit named in the turn's end, when it ended on one.
pub fn turn_end_limit(e: &TurnEnd) -> Option<&'static str> {
    match e {
        TurnEnd::LimitHit(what) => Some(*what),
        _ => None,
    }
}

/// How a turn ended, in the journal's words (`ddc_ai::journal`); the same as the core writes.
pub fn journal_end(e: &TurnEnd) -> String {
    match e {
        TurnEnd::Answered => "answered".to_string(),
        TurnEnd::LimitHit(l) => format!("limit:{l}"),
        TurnEnd::Cancelled => "cancelled".to_string(),
    }
}

/// One line of the step feed: number, role, model, tokens in (cached) and out, cost, time, tools.
pub fn step_line(cat: &Catalog, s: &ddc_ai::StepRecord) -> String {
    let role = cat.t(match s.role {
        ddc_ai::StepRole::Planner => "ai.journal.planner",
        ddc_ai::StepRole::Executor => "ai.journal.executor",
    });
    let mut line = cat.tf(
        "ai.journal.step",
        &[
            ("n", &s.n.to_string()),
            ("role", &role),
            ("model", &s.model),
            ("in", &fmt_tokens(s.tokens_in)),
            ("cached", &fmt_tokens(s.tokens_cached)),
            ("out", &fmt_tokens(s.tokens_out)),
            ("s", &crate::units::fmt_num(s.ms as f64 / 1000.0, 1)),
        ],
    );
    if let Some(c) = s.cost {
        line.push_str(" · ");
        line.push_str(&fmt_cost(c));
    }
    if !s.tools.is_empty() {
        line.push_str(" · ");
        line.push_str(&s.tools.join(", "));
    }
    line
}

/// The feed's last line: the request's totals.
pub fn journal_total_line(cat: &Catalog, j: &ddc_ai::TaskJournal) -> String {
    let t = j.totals();
    let mut line = cat.tf(
        "ai.journal.total",
        &[
            ("steps", &t.steps.to_string()),
            ("calls", &t.tool_calls.to_string()),
            ("in", &fmt_tokens(t.tokens_in)),
            ("out", &fmt_tokens(t.tokens_out)),
            ("s", &crate::units::fmt_num(t.ms as f64 / 1000.0, 1)),
        ],
    );
    if let Some(c) = t.cost {
        line.push_str(" · ");
        line.push_str(&fmt_cost(c));
    }
    line
}

/// How many of the plan's items are checked: drives the "All N" tri-state box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllState {
    None,
    Mixed,
    All,
}

pub fn all_state(items: &[PlanItem]) -> AllState {
    let on = items.iter().filter(|i| i.checked).count();
    if on == 0 {
        AllState::None
    } else if on == items.len() {
        AllState::All
    } else {
        AllState::Mixed
    }
}

/// Every item with the wanted mark, for one `SetChecked`: all on or all off keeps the
/// dependencies consistent, so no item is left out.
pub fn toggle_all(items: &[PlanItem], on: bool) -> Vec<(u32, bool)> {
    items.iter().map(|i| (i.id, on)).collect()
}

/// What a click on the "All" box does: anything but fully checked turns everything on.
pub fn all_click_target(state: AllState) -> bool {
    state != AllState::All
}

/// An item's conflict as the person reads it: a refusal off the wire becomes its message in
/// `lang` and its detail; any other text stays as it is.
pub fn conflict_text(raw: &str, lang: &str) -> String {
    let Some(r) = ddc_ai::refusal::from_wire(raw, &|_| None) else {
        return raw.to_string();
    };
    let what = ddc_ai::refusal::message(&r, lang);
    match r.detail.as_deref().filter(|d| !d.is_empty()) {
        Some(d) => format!("{what} {d}"),
        None => what,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::plan::PlanOp;

    fn dq(key: &str, before: f64, after: f64) -> QtyDelta {
        QtyDelta { key: key.into(), level: None, before, after }
    }

    #[test]
    fn cost_none_without_price() {
        let q = [dq("volume:Concrete", 0.0, 5.0)];
        assert_eq!(area_cost_delta(&q, &Default::default()).1, None);
    }

    #[test]
    fn cost_is_volume_delta_times_price() {
        let q = [dq("volume:Concrete", 1.0, 3.0)];
        let p = [("Concrete".to_string(), 100.0)].into();
        assert_eq!(area_cost_delta(&q, &p).1, Some(200.0));
    }

    #[test]
    fn cost_negative_delta() {
        let q = [dq("volume:Concrete", 3.0, 1.0)];
        let p = [("Concrete".to_string(), 100.0)].into();
        assert_eq!(area_cost_delta(&q, &p).1, Some(-200.0));
    }

    #[test]
    fn cost_mixed_keys_skip_unpriced_and_sum_area() {
        let q = [
            dq("volume:Concrete", 0.0, 1.0),
            dq("volume:Glass", 0.0, 9.0),
            dq("area:Concrete", 0.0, 7.0),
            dq("slab_area", 10.0, 14.0),
            dq("walls", 1.0, 2.0),
        ];
        let p = [("Concrete".to_string(), 50.0)].into();
        assert_eq!(area_cost_delta(&q, &p), (4.0, Some(50.0)));
    }

    fn item(id: u32, on: bool, deps: &[u32]) -> PlanItem {
        PlanItem {
            id,
            title: String::new(),
            why: String::new(),
            op: PlanOp::Delete {
                target: crate::ai::plan::Targets::Selection,
            },
            checked: on,
            depends_on: deps.to_vec(),
            actor: String::new(),
            conflict: None,
            sets: Default::default(),
        }
    }

    fn diff(item: u32, kind: RowKind) -> ElemDiff {
        ElemDiff {
            key: ("wall".to_string(), 1),
            kind,
            item: Some(item),
            level: None,
            label: String::new(),
            cells: vec![],
        }
    }

    fn state() -> StateIn {
        StateIn {
            has_profile: true,
            ..StateIn::default()
        }
    }

    #[test]
    fn no_profile_wins_over_everything() {
        let s = StateIn {
            busy: true,
            error: true,
            ..StateIn::default()
        };
        assert_eq!(panel_state(s), PanelState::NoProfile);
    }

    #[test]
    fn local_only_comes_before_consent() {
        let s = StateIn {
            local_blocked: true,
            needs_consent: true,
            ..state()
        };
        assert_eq!(panel_state(s), PanelState::LocalOnlyBlocked);
    }

    #[test]
    fn a_running_turn_hides_the_previous_plan() {
        let s = StateIn {
            busy: true,
            has_steps: true,
            can_apply: true,
            ..state()
        };
        assert_eq!(panel_state(s), PanelState::Streaming);
        assert_eq!(
            panel_state(StateIn {
                tool_running: true,
                ..s
            }),
            PanelState::Tool
        );
    }

    #[test]
    fn plan_states() {
        let p = StateIn {
            has_steps: true,
            ..state()
        };
        assert_eq!(panel_state(p), PanelState::Recomputing);
        assert_eq!(
            panel_state(StateIn {
                can_apply: true,
                ..p
            }),
            PanelState::PlanReady
        );
        assert_eq!(
            panel_state(StateIn { blocked: true, ..p }),
            PanelState::PlanReady
        );
        assert_eq!(
            panel_state(StateIn {
                stale: true,
                can_apply: true,
                ..p
            }),
            PanelState::Stale
        );
    }

    #[test]
    fn empty_session_is_idle() {
        assert_eq!(panel_state(state()), PanelState::Idle);
        assert_eq!(
            panel_state(StateIn {
                error: true,
                ..state()
            }),
            PanelState::Error
        );
    }

    #[test]
    fn provider_errors_are_classified_from_their_display() {
        use ddc_ai::ProviderError as P;
        let of = |e: P| classify(&AiError::Provider(e.to_string()));
        assert_eq!(
            of(P::RateLimited {
                retry_after_s: None
            }),
            ErrKind::RateLimit
        );
        assert_eq!(
            classify(&AiError::Provider("authentication failed".into())),
            ErrKind::Auth
        );
        assert_eq!(of(P::KeyLimit("Key limit exceeded".into())), ErrKind::KeyLimit);
        assert_eq!(of(P::NoCredit("Insufficient credits".into())), ErrKind::NoCredit);
        for k in [ErrKind::KeyLimit, ErrKind::NoCredit] {
            assert!(!can_retry(k) && !goes_to_settings(k), "{k:?}: more tries cannot pay");
            let cat = ddc_ui::Catalog::new(ddc_ui::Lang::En);
            for key in [err_key(k).to_string(), format!("{}.hint", err_key(k))] {
                assert_ne!(cat.t(&key), key, "{key}");
            }
        }
        assert_eq!(
            classify(&AiError::Provider("network error: reset".into())),
            ErrKind::Network
        );
        assert_eq!(
            classify(&AiError::Provider("http status 500: x".into())),
            ErrKind::Server
        );
        assert_eq!(
            classify(&AiError::Provider(
                "model does not support tool calling".into()
            )),
            ErrKind::NoTools
        );
        assert_eq!(classify(&AiError::Stale), ErrKind::Stale);
    }

    #[test]
    fn provider_http_detail_shown() {
        let e = AiError::Provider("http status 502: upstream\n  down".into());
        assert_eq!(
            err_detail(&e).as_deref(),
            Some("http status 502: upstream down")
        );
        assert!(err_detail(&AiError::Model("bad json".into())).is_some());
        assert!(err_detail(&AiError::Internal("oops".into())).is_some());
        for none in [
            AiError::EmptyPlan,
            AiError::Stale,
            AiError::Busy,
            AiError::Blocked,
            AiError::NeedsConfirmation,
        ] {
            assert_eq!(err_detail(&none), None);
        }
        assert_eq!(err_detail(&AiError::Provider("  ".into())), None);
        let long = AiError::Provider("a b ".repeat(200));
        let d = err_detail(&long).unwrap();
        assert_eq!(d.chars().count(), DETAIL_MAX);
        assert!(d.ends_with('\u{2026}'));
    }

    #[test]
    fn rate_limit_detail_keeps_retry_after() {
        use ddc_ai::ProviderError as P;
        let e = AiError::Provider(
            P::RateLimited {
                retry_after_s: Some(7),
            }
            .to_string(),
        );
        assert_eq!(classify(&e), ErrKind::RateLimit);
        let d = err_detail(&e).unwrap();
        assert!(d.contains("retry after 7 s"), "{d}");
        let none = AiError::Provider(
            P::RateLimited {
                retry_after_s: None,
            }
            .to_string(),
        );
        assert_eq!(classify(&none), ErrKind::RateLimit);
        assert_eq!(err_detail(&none).as_deref(), Some("rate limited"));
    }

    #[test]
    fn secret_like_words_masked() {
        let key = "sk-abcDEF1234567890";
        let long = "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7";
        for text in [
            format!("http status 401: bad key {key} given"),
            format!("http status 400: Authorization: Bearer {long}"),
            format!("http status 400: {{\"api_key\": \"{long}\"}}"),
            format!("network error: GET /v1?token={long}"),
            format!("http status 403: Bearer {key}"),
        ] {
            let d = err_detail(&AiError::Provider(text)).unwrap();
            assert!(!d.contains(key) && !d.contains("sk-abc"), "{d}");
            assert!(!d.contains(long) && !d.contains("A1b2C3"), "{d}");
            assert!(d.contains("***"), "{d}");
        }
        let short = mask_secrets("Authorization: Bearer abc123 and x-api-key: k9");
        assert_eq!(short, "Authorization: Bearer *** and x-api-key: ***");
        let plain = "http status 500: internal server error";
        assert_eq!(mask_secrets(plain), plain);
        assert_eq!(mask_secrets("sk-"), "***");
        let cut = AiError::Internal(format!("{}{key}", "x ".repeat(148)));
        assert!(!err_detail(&cut).unwrap().contains("sk-"));
    }

    #[test]
    fn key_errors_send_to_settings_network_errors_retry() {
        assert!(goes_to_settings(ErrKind::Auth));
        assert!(!goes_to_settings(ErrKind::Network));
        assert!(can_retry(ErrKind::Network));
        assert!(!can_retry(ErrKind::Auth));
    }

    #[test]
    fn fill_replaces_positional_arguments() {
        assert_eq!(
            fill("{0} \u{438} {1}, again {0}", &["a".into(), "b".into()]),
            "a \u{438} b, again a"
        );
        assert_eq!(fill("\u{431}\u{435}\u{437}", &[]), "\u{431}\u{435}\u{437}");
    }

    #[test]
    fn counts_go_by_item() {
        let d = [
            diff(1, RowKind::Created),
            diff(1, RowKind::Changed),
            diff(2, RowKind::Deleted),
            diff(1, RowKind::Changed),
        ];
        assert_eq!(
            item_counts(&d, 1),
            Counts {
                created: 1,
                changed: 2,
                deleted: 0
            }
        );
        assert_eq!(item_counts(&d, 2).total(), 1);
    }

    #[test]
    fn unchecking_takes_the_dependants_along() {
        let items = [
            item(1, true, &[]),
            item(2, true, &[1]),
            item(3, true, &[2]),
            item(4, true, &[]),
        ];
        assert_eq!(toggled(&items, 1), vec![(1, false), (2, false), (3, false)]);
    }

    #[test]
    fn checking_brings_back_what_it_needs() {
        let items = [
            item(1, false, &[]),
            item(2, false, &[1]),
            item(3, false, &[2]),
            item(4, false, &[]),
        ];
        assert_eq!(toggled(&items, 3), vec![(3, true), (2, true), (1, true)]);
    }

    #[test]
    fn headers_parse_and_round_trip() {
        let h = parse_headers("X-One: 1\nbroken\n : empty\nX-Two:2:3\n");
        assert_eq!(
            h,
            vec![
                ("X-One".to_string(), "1".to_string()),
                ("X-Two".to_string(), "2:3".to_string())
            ]
        );
        assert_eq!(parse_headers(&headers_text(&h)), h);
    }

    #[test]
    fn policy_reads_with_a_default() {
        assert!(policy_on(None, true));
        assert!(!policy_on(None, false));
        assert!(!policy_on(Some("0"), true));
        assert!(policy_on(Some("1"), false));
    }

    #[test]
    fn palette_offers_phrases_and_bang() {
        assert_eq!(palette_offer("  ", true), None);
        assert_eq!(palette_offer("door", true), None);
        assert_eq!(
            palette_offer("door =", false),
            Some(("door =".to_string(), false))
        );
        assert_eq!(
            palette_offer(&crate::app2_fx::fx("palette_replace_doors"), true),
            Some((crate::app2_fx::fx("palette_replace_doors"), false))
        );
        assert_eq!(
            palette_offer(&format!("! {}", crate::app2_fx::fx("palette_count_walls")), true),
            Some((crate::app2_fx::fx("palette_count_walls"), true))
        );
        assert_eq!(palette_offer("!", true), None);
    }

    #[test]
    fn profile_ids_do_not_collide() {
        assert_eq!(profile_id(&[]), "p1");
        assert_eq!(profile_id(&["p1".into(), "p2".into()]), "p3");
    }

    #[test]
    fn signed_uses_a_real_minus() {
        assert!(signed(-1.5, 1).starts_with('−'));
        assert!(signed(1.5, 1).starts_with('+'));
    }

    #[test]
    fn issues_put_the_blocking_first() {
        let i = |level, key| PlanIssue {
            level,
            item: None,
            el: None,
            text_key: key,
            args: vec![],
        };
        let list = [i(IssueLevel::Warn, "w"), i(IssueLevel::Block, "b")];
        assert_eq!(sorted_issues(&list)[0].text_key, "b");
    }

    #[test]
    fn the_journal_names_the_source_and_the_step_count() {
        assert_eq!(audit_source("user"), Source::Person);
        assert_eq!(audit_source("mcp:tool-x"), Source::Agent("tool-x".into()));
        assert!(is_changeset("apply") && is_changeset("reject"));
        assert!(!is_changeset("turn"));
        assert_eq!(steps_of("steps=3"), Some(3));
        assert_eq!(steps_of("x"), None);
    }

    #[test]
    fn only_the_newest_live_step_can_be_undone_from_the_journal() {
        assert_eq!(undo_state(None, false, Some(5)), UndoState::NoChange);
        assert_eq!(undo_state(Some(5), false, Some(6)), UndoState::Gone);
        assert_eq!(undo_state(Some(5), true, Some(6)), UndoState::Covered);
        assert_eq!(undo_state(Some(6), true, Some(6)), UndoState::Ready);
        assert_eq!(undo_state(Some(6), true, None), UndoState::Covered);
    }

    #[test]
    fn unix_seconds_read_as_a_utc_date() {
        assert_eq!(fmt_utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(fmt_utc(1_709_164_800 + 3_600 * 14 + 180), "2024-02-29 14:03 UTC");
    }

    #[test]
    fn local_time_is_the_utc_time_shifted_by_the_zone_or_labelled_utc() {
        let ts = 1_709_164_800 + 3_600 * 14 + 180;
        let local = fmt_local(ts);
        if local.ends_with("UTC") {
            assert_eq!(local, fmt_utc(ts), "no zone from the system");
        } else {
            // shape only: the zone of the test machine is not known
            assert_eq!(local.len(), "2024-02-29 14:03".len());
        }
        assert_eq!(civil_utc(ts + 3 * 3_600), (2024, 2, 29, 17, 3));
        assert_eq!(civil_utc(ts + 10 * 3_600), (2024, 3, 1, 0, 3), "day rolls over");
    }

    fn iss(level: IssueLevel, key: &'static str) -> PlanIssue {
        PlanIssue {
            level,
            item: None,
            el: None,
            text_key: key,
            args: vec![],
        }
    }

    #[test]
    fn allow_large_offered_when_size_is_the_only_blocker() {
        let big = [iss(IssueLevel::Block, "ai.issue.too_large")];
        assert!(allow_large_offer(&[], &big, false), "draft plan: reason is in main_issues");
        assert!(allow_large_offer(&big, &[], false));
    }

    #[test]
    fn allow_large_not_offered_with_another_blocker_or_none() {
        let big = [iss(IssueLevel::Block, "ai.issue.too_large")];
        let other = [iss(IssueLevel::Block, "ai.issue.conflict")];
        assert!(!allow_large_offer(&other, &big, false));
        let refused = [iss(IssueLevel::Block, "ai.issue.refused_large")];
        assert!(!allow_large_offer(&[], &refused, false));
        assert!(!allow_large_offer(&[], &[], false));
        let warn = [iss(IssueLevel::Warn, "ai.issue.too_large")];
        assert!(!allow_large_offer(&warn, &[], false));
    }

    #[test]
    fn allow_large_stays_offered_once_on() {
        assert!(allow_large_offer(&[], &[], true), "the person can take it back");
    }

    #[test]
    fn provider_error_shows_status_and_retry_after() {
        let http = error_detail(&AiError::Provider("http status 503: overloaded".into()));
        assert_eq!(http.status, Some(503));
        assert_eq!(http.text, "overloaded");
        let rate = error_detail(&AiError::Provider("rate limited (retry after 7 s)".into()));
        assert_eq!((rate.status, rate.retry_after_s), (Some(429), Some(7)));
        let bare = error_detail(&AiError::Provider("rate limited".into()));
        assert_eq!((bare.status, bare.retry_after_s), (Some(429), None));
        assert!(error_detail(&AiError::Busy).is_empty());
        let long = error_detail(&AiError::Provider(format!(
            "http status 500: {}",
            "x".repeat(900)
        )));
        assert_eq!(long.text.chars().count(), 400, "a server page does not flood the card");
    }

    #[test]
    fn not_refreshed_toast_is_warning_plain_is_success() {
        use ddc_ui::notify::Level;
        let nr = AiError::AppliedNotRefreshed("reload".into());
        let (lvl, key) = applied_toast(Some(&nr));
        assert!(matches!(lvl, Level::Warning));
        assert_eq!(key, "ai_toast.toast_not_refreshed");
        let (lvl, key) = applied_toast(None);
        assert!(matches!(lvl, Level::Success));
        assert_eq!(key, "ai.plan.applied");
        // a refused apply error is not a committed one
        let (lvl, _) = applied_toast(Some(&AiError::Stale));
        assert!(matches!(lvl, Level::Success));
    }

    #[test]
    fn not_refreshed_toast_wording_names_undo_in_both_languages() {
        for lang in ["en", "ru"] {
            let p = format!("{}/../ui/data/i18n/{lang}/ai_logic.json", env!("CARGO_MANIFEST_DIR"));
            let t = std::fs::read_to_string(p).unwrap();
            assert!(t.contains("ai_toast.toast_not_refreshed") && t.contains("{n}") && t.contains("Ctrl+Z"), "{lang}");
        }
    }

    #[test]
    fn reload_failure_after_commit_not_reported_as_unchanged() {
        let e = AiError::AppliedNotRefreshed("reload".into());
        let k = classify(&e);
        assert_eq!(k, ErrKind::NotRefreshed);
        assert_ne!(k, ErrKind::Model, "not the 'model did not change' text");
        assert!(!can_retry(k), "a retry would apply twice");
        assert!(!goes_to_settings(k));
        assert_eq!(err_key(k), "ai.err.not_refreshed");
        assert_eq!(classify(&AiError::ReviewNeeded), ErrKind::ReviewNeeded);
        assert!(!can_retry(ErrKind::ReviewNeeded));
    }

    #[test]
    fn mcp_tab_offers_enable_and_token() {
        use ddc_ai::mcp::McpPolicy;
        assert!(access_actions(false, None).is_empty());
        let both = access_actions(true, Some(McpPolicy::AutoApply));
        assert!(matches!(both[0], super::super::Action::McpRevoke));
        assert!(matches!(both[1], super::super::Action::McpConfirm(false)));
        let ask = access_actions(false, Some(McpPolicy::Confirm));
        assert!(matches!(ask[0], super::super::Action::McpConfirm(true)));
    }

    #[test]
    fn enter_outside_card_grants_nothing() {
        assert!(!key_scope(None), "nothing focused");
        assert!(!key_scope(Some(false)), "focus in another text field");
        assert!(key_scope(Some(true)));
    }

    #[test]
    fn ctrl_enter_in_annotation_editor_commits_text_not_plan() {
        // the annotation editor holds focus outside the panel: the panel takes no key
        assert!(!key_scope(Some(false)));
        let away = KeyAt {
            focus_in: Some(false),
            typing: true,
            canvas_free: true,
            ..KeyAt::default()
        };
        assert_eq!(card_keys(away), (false, false));
    }

    #[test]
    fn the_card_keys_follow_the_person_into_the_view() {
        let canvas = KeyAt { canvas_free: true, ..KeyAt::default() };
        // a click into the plan or 3D view focuses nothing or a canvas widget
        assert_eq!(card_keys(canvas), (true, true));
        assert_eq!(card_keys(KeyAt { focus_in: Some(false), ..canvas }), (true, true));
        // a drawing tool or a selection keeps its own Esc; Ctrl+Enter still applies
        assert_eq!(card_keys(KeyAt { canvas_esc: true, ..canvas }), (true, false));
        // a dialog or a menu owns the keys
        assert_eq!(card_keys(KeyAt::default()), (false, false));
        // in the panel: the composer keeps Ctrl+Enter, any other field keeps both keys
        let panel = KeyAt { focus_in: Some(true), ..KeyAt::default() };
        assert_eq!(card_keys(panel), (true, true));
        assert_eq!(card_keys(KeyAt { typing: true, composer: true, ..panel }), (true, true));
        assert_eq!(card_keys(KeyAt { typing: true, ..panel }), (false, false));
        assert_eq!(card_keys(KeyAt { canvas_esc: true, ..panel }), (true, true));
    }

    #[test]
    fn plain_enter_and_esc_take_the_draft_only_on_a_bare_canvas() {
        let canvas = KeyAt { canvas_free: true, ..KeyAt::default() };
        assert!(draft_keys_on_canvas(canvas));
        assert!(draft_keys_on_canvas(KeyAt { focus_in: Some(false), ..canvas }));
        // a wall tool finishes on Enter, a selection clears on Esc: theirs
        assert!(!draft_keys_on_canvas(KeyAt { canvas_esc: true, ..canvas }));
        // the composer sends on Enter; any field types it; a dialog owns the keys
        assert!(!draft_keys_on_canvas(KeyAt { focus_in: Some(true), ..canvas }));
        assert!(!draft_keys_on_canvas(KeyAt { typing: true, ..canvas }));
        assert!(!draft_keys_on_canvas(KeyAt::default()));
    }

    #[test]
    fn each_agent_item_shows_its_actor() {
        let mut a = item(1, true, &[]);
        a.actor = "mcp:scan".into();
        let mut b = item(2, true, &[]);
        b.actor = "assistant".into();
        assert_eq!(agent_of(&a.actor), Some("scan"));
        assert_eq!(agent_of(&b.actor), None);
        assert_eq!(proposer_line(&[a, b], &[1, 2]), "scan");
    }

    #[test]
    fn all_toggle_is_a_tri_state() {
        let none = [item(1, false, &[]), item(2, false, &[])];
        let some = [item(1, true, &[]), item(2, false, &[])];
        let all = [item(1, true, &[]), item(2, true, &[])];
        assert_eq!(all_state(&none), AllState::None);
        assert_eq!(all_state(&some), AllState::Mixed);
        assert_eq!(all_state(&all), AllState::All);
        assert!(all_click_target(AllState::None));
        assert!(all_click_target(AllState::Mixed));
        assert!(!all_click_target(AllState::All));
    }

    #[test]
    fn toggle_all_lists_every_id_and_leaves_the_draft_alone() {
        let items = [item(3, true, &[]), item(5, false, &[3]), item(9, false, &[])];
        let before: Vec<(u32, bool)> = items.iter().map(|i| (i.id, i.checked)).collect();
        assert_eq!(toggle_all(&items, true), vec![(3, true), (5, true), (9, true)]);
        assert_eq!(toggle_all(&items, false), vec![(3, false), (5, false), (9, false)]);
        let after: Vec<(u32, bool)> = items.iter().map(|i| (i.id, i.checked)).collect();
        assert_eq!(before, after);
        assert!(toggle_all(&[], true).is_empty());
    }

    #[test]
    fn toggling_one_item_moves_the_header_state() {
        let mut items = vec![item(1, true, &[]), item(2, true, &[])];
        assert_eq!(all_state(&items), AllState::All);
        for (id, on) in toggled(&items, 2) {
            items.iter_mut().find(|i| i.id == id).unwrap().checked = on;
        }
        assert_eq!(all_state(&items), AllState::Mixed);
        for (id, on) in toggled(&items, 1) {
            items.iter_mut().find(|i| i.id == id).unwrap().checked = on;
        }
        assert_eq!(all_state(&items), AllState::None);
    }

    #[test]
    fn the_step_feed_names_role_tokens_cost_time_and_tools_in_both_languages() {
        let mut j = ddc_ai::TaskJournal::new("t");
        j.steps.push(ddc_ai::StepRecord {
            n: 2,
            role: ddc_ai::StepRole::Executor,
            model: "small".into(),
            tokens_in: 12_400,
            tokens_cached: 9_000,
            tokens_out: 310,
            cost: Some(0.02),
            ms: 1500,
            tools: vec!["create_geometry".into(), "create_elements".into()],
        });
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let line = step_line(&cat, &j.steps[0]);
            for part in ["2", "small", "12", "310", "create_geometry, create_elements"] {
                assert!(line.contains(part), "{part}: {line}");
            }
            assert!(line.contains(&cat.t("ai.journal.executor")), "{line}");
            assert!(!line.contains('{'), "every placeholder filled: {line}");
            let total = journal_total_line(&cat, &j);
            assert!(total.contains('$') && !total.contains('{'), "{total}");
        }
        assert_eq!(journal_end(&TurnEnd::LimitHit("cost")), "limit:cost");
        assert_eq!(journal_end(&TurnEnd::Answered), "answered");
    }
}
