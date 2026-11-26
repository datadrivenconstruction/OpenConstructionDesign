// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ops_dry_run` and `ops_propose`: an external agent's envelope runs on a throwaway copy of the
//! base snapshot, never on the live model or the chat's work copy. A proposal only waits on a
//! card; it reaches the model when the person presses Apply, whatever the MCP policy says.

use super::mcp_core::{busy_because, reply};
use super::*;
use ddc_ai::mcp::tools as mcp_tools;
use ddc_ops::{Actor, Envelope, WireError};

/// More than this is a script's job, not one card.
pub const MAX_OPS: usize = ddc_ai::limits::MCP_OPS.us();
const KEPT_PROPOSALS: usize = 64;

/// A problem the person or the agent can act on. `path` is a JSON pointer into the envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub path: String,
    pub code: String,
    pub detail: String,
}

impl Rejection {
    pub fn new(path: impl Into<String>, code: impl Into<String>, detail: impl Into<String>) -> Self {
        Rejection {
            path: path.into(),
            code: code.into(),
            detail: detail.into(),
        }
    }

    /// The executor's rejection on the wire: stable code, readable expected/got.
    pub fn from_ops(r: &ddc_ops::Rejection) -> Self {
        let mut detail = String::new();
        if let Some(e) = &r.expected {
            detail.push_str(&format!("expected {e}"));
        }
        if let Some(g) = &r.got {
            if !detail.is_empty() {
                detail.push_str(", ");
            }
            detail.push_str(&format!("got {g}"));
        }
        Rejection::new(r.path.clone(), r.code.key(), detail)
    }

    fn to_json(&self) -> J {
        json!({"path": self.path, "code": self.code, "detail": fmt::text(&self.detail)})
    }
}

/// What the executor reports for one envelope on a scratch model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DryRun {
    pub diff: J,
    pub rejections: Vec<Rejection>,
    pub findings: Vec<J>,
    pub preview: J,
}

/// The seam to the executor (`Model::dry_run_envelope`), joined once at start-up.
/// `scratch` is a private copy that is thrown away afterwards: writing to it is allowed.
pub trait EnvelopeRunner: Sync {
    fn dry_run(&self, scratch: &mut Model, env: &Envelope) -> Result<DryRun, String>;
}

struct NotJoined;

impl EnvelopeRunner for NotJoined {
    fn dry_run(&self, _: &mut Model, _: &Envelope) -> Result<DryRun, String> {
        Err("executor_not_joined".to_string())
    }
}

static RUNNER: OnceLock<&'static dyn EnvelopeRunner> = OnceLock::new();

/// False when a runner was joined already: the first one stays.
pub fn join_runner(r: &'static dyn EnvelopeRunner) -> bool {
    RUNNER.set(r).is_ok()
}

fn runner() -> &'static dyn EnvelopeRunner {
    RUNNER.get().copied().unwrap_or(&NotJoined)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProposalState {
    Waiting,
    /// Handed to the person's Apply; it becomes Approved only when the run succeeded.
    Applying,
    Approved,
    Rejected,
    /// The live model moved after the dry run: the card no longer shows what Apply would do.
    Stale,
}

impl ProposalState {
    fn name(self) -> &'static str {
        match self {
            ProposalState::Waiting => "waiting",
            ProposalState::Applying => "applying",
            ProposalState::Approved => "applied",
            ProposalState::Rejected => "rejected",
            ProposalState::Stale => "stale",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Proposal {
    pub id: u32,
    pub actor: String,
    pub envelope: Envelope,
    pub dry: DryRun,
    pub state: ProposalState,
    /// Change head of the live model that the dry run's base copy was taken at.
    pub base_head: u64,
}

/// What the UI shows of a card; the envelope itself stays in the worker until Apply.
#[derive(Clone, Debug, PartialEq)]
pub struct ProposalCard {
    pub id: u32,
    pub actor: String,
    pub title: String,
    pub ops: usize,
    pub diff: J,
    pub state: ProposalState,
    /// Live change head of the dry run: the card greys Apply once `plan_stale(base_head)`.
    pub base_head: u64,
}

/// The cards of `ops_propose`. Only the UI's Apply and Reject change a state; no MCP tool can.
#[derive(Debug, Default)]
pub struct Proposals {
    next: u32,
    items: VecDeque<Proposal>,
}

impl Proposals {
    fn add(&mut self, actor: &str, envelope: Envelope, dry: DryRun, base_head: u64) -> u32 {
        self.next += 1;
        self.items.push_back(Proposal {
            id: self.next,
            actor: actor.to_string(),
            envelope,
            dry,
            state: ProposalState::Waiting,
            base_head,
        });
        while self.items.len() > KEPT_PROPOSALS {
            // a waiting card is never dropped silently: the oldest decided one goes first
            let decided = |p: &Proposal| {
                !matches!(p.state, ProposalState::Waiting | ProposalState::Applying)
            };
            match self.items.iter().position(decided) {
                Some(i) => {
                    self.items.remove(i);
                }
                None => break,
            }
        }
        self.next
    }

    /// Test seam: a waiting card for a dry run the test took itself.
    #[cfg(test)]
    pub(crate) fn add_for_test(
        &mut self,
        actor: &str,
        envelope: Envelope,
        dry: DryRun,
        base_head: u64,
    ) -> u32 {
        self.add(actor, envelope, dry, base_head)
    }

    pub fn waiting(&self) -> impl Iterator<Item = &Proposal> {
        self.items.iter().filter(|p| p.state == ProposalState::Waiting)
    }

    pub fn get(&self, id: u32) -> Option<&Proposal> {
        self.items.iter().find(|p| p.id == id)
    }

    /// The person's Apply takes a waiting card: it is `Applying` until [`Proposals::settle`].
    /// Returns the envelope and the live head its dry run was based on. The worker holds no
    /// live model, so the staleness check is the caller's (`approve`, or the UI thread).
    pub fn take(&mut self, id: u32) -> Result<(Envelope, u64), Rejection> {
        let Some(p) = self
            .items
            .iter_mut()
            .find(|p| p.id == id && p.state == ProposalState::Waiting)
        else {
            return Err(Rejection::new("", "not_waiting", "no waiting proposal with this id"));
        };
        p.state = ProposalState::Applying;
        Ok((p.envelope.clone(), p.base_head))
    }

    /// Called on the person's Apply only, with the live model. The envelope goes to the
    /// executor as one undo step. Refused when the live model changed since the dry run, so
    /// Apply never runs what the card did not show; the agent proposes again on a fresh base.
    /// On `Ok` the card is `Applying`: the caller settles it with the result of the run.
    pub fn approve(&mut self, id: u32, live: &Model) -> Result<Envelope, Rejection> {
        let (env, base_head) = self.take(id)?;
        if live.plan_stale(base_head) {
            self.settle(id, ProposalState::Stale);
            return Err(stale_rejection());
        }
        Ok(env)
    }

    /// The end of an Apply: `Approved` only after the run succeeded, `Waiting` again after a
    /// failed run (nothing was written), `Stale` when the live model moved. False if the card
    /// was not being applied.
    pub fn settle(&mut self, id: u32, to: ProposalState) -> bool {
        if to == ProposalState::Applying || to == ProposalState::Rejected {
            return false;
        }
        match self
            .items
            .iter_mut()
            .find(|p| p.id == id && p.state == ProposalState::Applying)
        {
            Some(p) => {
                p.state = to;
                true
            }
            None => false,
        }
    }

    pub fn reject(&mut self, id: u32) -> bool {
        match self
            .items
            .iter_mut()
            .find(|p| p.id == id && p.state == ProposalState::Waiting)
        {
            Some(p) => {
                p.state = ProposalState::Rejected;
                true
            }
            None => false,
        }
    }

    /// Cards still open for the person: waiting or being applied.
    pub fn cards(&self) -> Vec<ProposalCard> {
        self.items
            .iter()
            .filter(|p| matches!(p.state, ProposalState::Waiting | ProposalState::Applying))
            .map(|p| ProposalCard {
                id: p.id,
                actor: p.actor.clone(),
                title: p.envelope.title.clone(),
                ops: p.envelope.op_count(),
                diff: p.dry.diff.clone(),
                state: p.state,
                base_head: p.base_head,
            })
            .collect()
    }

    pub fn status_json(&self) -> J {
        J::Array(
            self.items
                .iter()
                .map(|p| json!({"proposal": p.id, "state": p.state.name()}))
                .collect(),
        )
    }
}

pub fn stale_rejection() -> Rejection {
    Rejection::new(
        "",
        "model_changed",
        "the model changed after this proposal was previewed; propose it again",
    )
}

fn rejected(list: &[Rejection]) -> ToolOutcome {
    let list: Vec<J> = list.iter().map(Rejection::to_json).collect();
    ToolOutcome::error(reply(&[
        ("status", json!("rejected")),
        ("rejections", J::Array(list)),
    ]))
}

fn wire_rejection(e: &WireError) -> Rejection {
    match e {
        WireError::NoVersion => Rejection::new("/v", "no_version", e.to_string()),
        WireError::Version { .. } => Rejection::new("/v", "version", e.to_string()),
        WireError::Json(j) => {
            let msg = j.to_string();
            let code = if msg.starts_with("unknown field") {
                "unknown_field"
            } else if msg.starts_with("unknown variant") {
                "unknown_variant"
            } else {
                "shape"
            };
            Rejection::new("", code, msg)
        }
    }
}

/// Strict parse and the structural checks; nothing is copied or opened before this passes.
/// The actor is the connected client, never what the envelope claims.
pub(super) fn parse(args: &J, actor: &str) -> Result<Envelope, Vec<Rejection>> {
    let raw = args.get(mcp_tools::ENVELOPE_ARG).cloned().unwrap_or(J::Null);
    let mut env = Envelope::from_value(raw).map_err(|e| vec![wire_rejection(&e)])?;
    if env.op_count() > MAX_OPS {
        return Err(vec![Rejection::new(
            "/groups",
            "too_many_ops",
            format!("{} ops, at most {MAX_OPS}", env.op_count()),
        )]);
    }
    let issues = env.check();
    if !issues.is_empty() {
        return Err(issues
            .into_iter()
            .map(|i| Rejection::new(i.path, i.code, ""))
            .collect());
    }
    env.actor = Actor::Agent {
        name: actor.to_string(),
    };
    Ok(env)
}

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

fn remove_scratch(p: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut s = p.as_os_str().to_owned();
        s.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(s));
    }
}

/// How filter and set targets are frozen on the scratch copy: the project units filter
/// numbers are read in, and the session's frozen sets. `None`: the envelope runs as sent.
pub(super) type Freeze<'a> = Option<(Units, &'a Sets)>;

/// `base` is read only (a read-only backup connection); the scratch file lives in `dir` and is
/// gone when this returns, whatever the runner did to it. Returns the envelope that ran: with
/// `fz`, filter and set targets are frozen into refs on the copy (ai/freeze.rs), and that
/// frozen envelope is what a card keeps, so Apply touches exactly the ids the dry run showed.
pub(super) fn dry_run_on_copy(
    runner: &dyn EnvelopeRunner,
    base: &Path,
    dir: &Path,
    env: &Envelope,
    fz: Freeze<'_>,
) -> Result<(Envelope, DryRun), String> {
    let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
    let scratch = dir.join(format!("ops-dry-{}-{n}.db", std::process::id()));
    remove_scratch(&scratch);
    let out = (|| {
        Model::backup_file(base, &scratch, &mut |_: u64, _: u64| true).map_err(|e| e.to_string())?;
        let mut model = Model::open_copy(&scratch.to_string_lossy()).map_err(|e| e.to_string())?;
        let Some((units, sets)) = fz.filter(|_| crate::ai::freeze::needs_freeze(env)) else {
            return Ok((env.clone(), runner.dry_run(&mut model, env)?));
        };
        let mut ed = Editor::new_in_memory().map_err(|e| e.to_string())?;
        ed.replace_model(model).map_err(|e| e.to_string())?;
        ed.set_units(units);
        // row caches key on the connection address: a reused address must not see old rows
        ed.revision = u64::MAX / 2 + n;
        let frozen = match crate::ai::freeze::freeze(&ed, Some(sets), env) {
            Ok(f) => f,
            Err(r) => {
                let d = DryRun {
                    rejections: vec![Rejection::from_ops(&r)],
                    ..DryRun::default()
                };
                return Ok((env.clone(), d));
            }
        };
        let d = runner.dry_run(&mut ed.model, &frozen)?;
        Ok((frozen, d))
    })();
    remove_scratch(&scratch);
    out
}

fn dry_fields(env: &Envelope, d: &DryRun) -> Vec<(&'static str, J)> {
    vec![
        ("ops", json!(env.op_count())),
        ("diff", d.diff.clone()),
        ("findings", J::Array(d.findings.clone())),
        ("preview", d.preview.clone()),
    ]
}

#[cfg(test)]
pub(super) fn dry_run_reply(
    runner: &dyn EnvelopeRunner,
    base: &Path,
    dir: &Path,
    args: &J,
    actor: &str,
) -> ToolOutcome {
    dry_run_reply_in(runner, base, dir, args, actor, None)
}

pub(super) fn dry_run_reply_in(
    runner: &dyn EnvelopeRunner,
    base: &Path,
    dir: &Path,
    args: &J,
    actor: &str,
    fz: Freeze<'_>,
) -> ToolOutcome {
    let env = match parse(args, actor) {
        Ok(e) => e,
        Err(r) => return rejected(&r),
    };
    match dry_run_on_copy(runner, base, dir, &env, fz) {
        Err(e) => ToolOutcome::error(reply(&[
            ("status", json!("failed")),
            ("reason", json!(fmt::text(&e))),
        ])),
        Ok((_, d)) if !d.rejections.is_empty() => rejected(&d.rejections),
        Ok((env, d)) => {
            let mut f = vec![("status", json!("ok"))];
            f.extend(dry_fields(&env, &d));
            ToolOutcome::ok(reply(&f))
        }
    }
}

/// Same dry run, then a card for the person. Never a write, under any policy.
/// `base_head` is the live change head `base` was copied at; Apply checks it.
#[cfg(test)]
pub(super) fn propose_reply(
    proposals: &mut Proposals,
    runner: &dyn EnvelopeRunner,
    at: (&Path, u64),
    dir: &Path,
    args: &J,
    actor: &str,
) -> ToolOutcome {
    propose_reply_in(proposals, runner, at, dir, args, actor, None)
}

pub(super) fn propose_reply_in(
    proposals: &mut Proposals,
    runner: &dyn EnvelopeRunner,
    (base, base_head): (&Path, u64),
    dir: &Path,
    args: &J,
    actor: &str,
    fz: Freeze<'_>,
) -> ToolOutcome {
    let env = match parse(args, actor) {
        Ok(e) => e,
        Err(r) => return rejected(&r),
    };
    let (env, d) = match dry_run_on_copy(runner, base, dir, &env, fz) {
        Ok(d) => d,
        Err(e) => {
            return ToolOutcome::error(reply(&[
                ("status", json!("failed")),
                ("reason", json!(fmt::text(&e))),
            ]))
        }
    };
    if !d.rejections.is_empty() {
        return rejected(&d.rejections);
    }
    let mut f = vec![
        ("status", json!("pending_user")),
        ("note", json!(super::mcp_core::AWAITING_APPLY)),
    ];
    f.extend(dry_fields(&env, &d));
    let id = proposals.add(actor, env, d, base_head);
    f.push(("proposal", json!(id)));
    f.push(("next", json!(mcp_tools::PLAN_STATUS)));
    ToolOutcome::ok(reply(&f))
}

impl Core {
    /// An envelope is one undo step of its own: it does not mix with an open changeset.
    pub(super) fn ops_dry_run(&mut self, actor: &str, args: &J) -> ToolOutcome {
        if self.group_open(actor) {
            return busy_because("changeset_open");
        }
        let dir = match self.scratch_dir() {
            Ok(d) => d,
            Err(e) => return ToolOutcome::error(e),
        };
        let fz = Some((self.ed.tool.units, &self.sets));
        dry_run_reply_in(runner(), self.copies.base(), &dir, args, actor, fz)
    }

    pub(super) fn ops_propose(&mut self, actor: &str, args: &J) -> ToolOutcome {
        if self.group_open(actor) {
            return busy_because("changeset_open");
        }
        let dir = match self.scratch_dir() {
            Ok(d) => d,
            Err(e) => return ToolOutcome::error(e),
        };
        let base = self.copies.base().to_path_buf();
        let at = (base.as_path(), self.live_head);
        let fz = Some((self.ed.tool.units, &self.sets));
        let out = propose_reply_in(&mut self.proposals, runner(), at, &dir, args, actor, fz);
        self.send_cards();
        out
    }

    pub(super) fn send_cards(&self) {
        self.send(FromWorker::Proposals(self.proposals.cards()));
    }

    /// The person pressed Apply on a card: the envelope goes to the UI thread, which alone
    /// writes the live model (`UiSession::apply_proposal`). The card waits in `Applying`.
    pub(super) fn take_proposal(&mut self, id: u32) {
        let m = match self.proposals.take(id) {
            Ok((envelope, base_head)) => FromWorker::ProposalTaken {
                id,
                envelope: Box::new(envelope),
                base_head,
            },
            Err(r) => FromWorker::ProposalRefused { id, code: r.code },
        };
        self.send(m);
        self.send_cards();
    }

    pub(super) fn settle_proposal(&mut self, id: u32, to: ProposalState) {
        self.proposals.settle(id, to);
        self.send_cards();
    }

    pub(super) fn reject_proposal(&mut self, id: u32) {
        if self.proposals.reject(id) {
            self.audit("proposal", Some(format!("proposal={id}")), None, "rejected");
        }
        self.send_cards();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Writes to whatever model it gets, as a buggy executor would.
    struct Writer {
        calls: AtomicUsize,
    }

    impl EnvelopeRunner for Writer {
        fn dry_run(&self, scratch: &mut Model, env: &Envelope) -> Result<DryRun, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            for _ in 0..env.op_count() {
                scratch.create("wall").map_err(|e| e.to_string())?;
            }
            let n = scratch.count().map_err(|e| e.to_string())?;
            Ok(DryRun {
                diff: json!({"created": env.op_count(), "count": n}),
                ..DryRun::default()
            })
        }
    }

    fn writer() -> Writer {
        Writer {
            calls: AtomicUsize::new(0),
        }
    }

    struct Fixture {
        dir: PathBuf,
        live: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn fixture(tag: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("ddc-ops-mcp-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let live = dir.join("live.db");
        let mut m = Model::open(&live.to_string_lossy()).unwrap();
        m.create("wall").unwrap();
        drop(m);
        Fixture { dir, live }
    }

    fn live_count(f: &Fixture) -> i64 {
        Model::open(&f.live.to_string_lossy()).unwrap().count().unwrap()
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// Every op kind; it passes `check()`.
    fn good() -> J {
        json!({"envelope": serde_json::to_value(ddc_ops::sample::envelope()).unwrap()})
    }

    fn body(out: &ToolOutcome) -> J {
        serde_json::from_str(&out.text).unwrap()
    }

    #[test]
    fn mcp_dry_run_never_touches_live() {
        let f = fixture("dry");
        let before = std::fs::read(&f.live).unwrap();
        let r = writer();
        let out = dry_run_reply(&r, &f.live, &f.dir, &good(), "mcp:agent");
        assert!(!out.is_error, "{}", out.text);
        let v = body(&out);
        assert_eq!(v["untrusted"], "data");
        assert_eq!(v["status"], "ok");
        assert!(v["diff"]["count"].as_i64().unwrap() > 1, "the runner wrote to its copy");
        assert_eq!(r.calls.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(&f.live).unwrap(), before, "not one byte of the live file");
        assert_eq!(live_count(&f), 1, "the live model has only its own wall");
        assert!(
            files_in(&f.dir).iter().all(|n| !n.starts_with("ops-dry-")),
            "the scratch copy is gone: {:?}",
            files_in(&f.dir)
        );
    }

    #[test]
    fn mcp_propose_needs_apply() {
        let f = fixture("propose");
        let r = writer();
        let mut cards = Proposals::default();
        let live = Model::open(":memory:").unwrap();
        let at = (f.live.as_path(), live.change_head());
        let out = propose_reply(&mut cards, &r, at, &f.dir, &good(), "mcp:agent");
        assert!(!out.is_error, "{}", out.text);
        let v = body(&out);
        assert_eq!(v["status"], "pending_user");
        let id = v["proposal"].as_u64().unwrap() as u32;
        assert_eq!(live_count(&f), 1, "a proposal writes nothing");
        assert_eq!(cards.waiting().count(), 1);
        let p = cards.get(id).unwrap();
        assert_eq!(
            p.envelope.actor,
            Actor::Agent { name: "mcp:agent".into() },
            "the card names the client, not what the envelope claimed"
        );
        assert_eq!(cards.status_json()[0]["state"], "waiting");
        assert!(
            mcp_tools::only_here()
                .iter()
                .chain(ddc_ai::catalog::catalog())
                .all(|s| !s.name.contains("approve") && !s.name.contains("proposal")),
            "no agent tool reaches the person's Apply"
        );
        assert!(cards.approve(id, &live).is_ok(), "the person's Apply takes the envelope");
        assert_eq!(cards.approve(id, &live).unwrap_err().code, "not_waiting", "and only once");
        assert!(!cards.reject(id));
        assert_eq!(cards.status_json()[0]["state"], "applying", "not applied before the run");
        assert!(cards.settle(id, ProposalState::Approved));
        assert_eq!(cards.status_json()[0]["state"], "applied");
        assert!(!cards.settle(id, ProposalState::Waiting), "a settled card stays settled");
    }

    #[test]
    fn proposal_is_refused_after_the_live_model_changed() {
        let f = fixture("stale");
        let r = writer();
        let mut cards = Proposals::default();
        let mut live = Model::open(":memory:").unwrap();
        let at = (f.live.as_path(), live.change_head());
        let v = body(&propose_reply(&mut cards, &r, at, &f.dir, &good(), "a"));
        let id = v["proposal"].as_u64().unwrap() as u32;
        live.add_level("Moved", 3.0).unwrap();
        let e = cards.approve(id, &live).unwrap_err();
        assert_eq!(e.code, "model_changed");
        assert_eq!(cards.get(id).unwrap().state, ProposalState::Stale);
        assert_eq!(cards.status_json()[0]["state"], "stale");
        assert_eq!(cards.waiting().count(), 0, "a stale card cannot be applied later");
        assert_eq!(cards.approve(id, &live).unwrap_err().code, "not_waiting");
        assert_eq!(live_count(&f), 1, "nothing reached the model file");
    }

    #[test]
    fn a_failed_apply_leaves_the_card_waiting_not_approved() {
        let f = fixture("failed");
        let r = writer();
        let mut cards = Proposals::default();
        let live = Model::open(":memory:").unwrap();
        let at = (f.live.as_path(), live.change_head());
        let v = body(&propose_reply(&mut cards, &r, at, &f.dir, &good(), "a"));
        let id = v["proposal"].as_u64().unwrap() as u32;
        assert!(cards.approve(id, &live).is_ok());
        assert!(!cards.settle(id, ProposalState::Rejected), "only a run result settles");
        assert!(cards.settle(id, ProposalState::Waiting), "the run failed");
        assert_eq!(cards.get(id).unwrap().state, ProposalState::Waiting);
        assert_eq!(cards.status_json()[0]["state"], "waiting");
        assert_eq!(cards.waiting().count(), 1, "the person can try again or reject");
        assert!(cards.approve(id, &live).is_ok());
        assert!(cards.settle(id, ProposalState::Approved));
        assert_eq!(cards.status_json()[0]["state"], "applied");
    }

    mod exec_apply {
        use super::*;
        use crate::editor::ops_run::{set_thread_exec, EnvelopeExec, OpsReceipt};
        use crate::editor::Editor;
        use crate::ops_exec::{apply_proposal, APPLIED_RELOAD_FAILED, NOTHING_TO_APPLY};

        /// Runs and writes nothing.
        struct Nothing;

        impl EnvelopeExec for Nothing {
            fn apply(&self, _: &mut Model, _: &Envelope) -> Result<OpsReceipt, ddc_model::Error> {
                Ok(OpsReceipt::default())
            }
        }

        /// Writes, then fails: the executor seam rolls the write back.
        struct FailAfterWrite;

        impl EnvelopeExec for FailAfterWrite {
            fn apply(&self, m: &mut Model, _: &Envelope) -> Result<OpsReceipt, ddc_model::Error> {
                m.add_level("Leak", 7.0)?;
                Err(ddc_model::Error::Invalid("test: failed after a write".into()))
            }
        }

        /// Writes for real, then breaks what the view reload reads.
        struct BreakReload;

        impl EnvelopeExec for BreakReload {
            fn apply(&self, m: &mut Model, env: &Envelope) -> Result<OpsReceipt, ddc_model::Error> {
                let r = m
                    .apply_envelope(env)
                    .map_err(|e| ddc_model::Error::Invalid(e.to_string()))?;
                m.sql()
                    .execute_batch("ALTER TABLE wall_type_layer RENAME TO zz_layer")?;
                Ok(r)
            }
        }

        fn setup(x: std::sync::Arc<dyn EnvelopeExec>) -> (Editor, Proposals, u32) {
            set_thread_exec(Some(x));
            let ed = Editor::new_in_memory().unwrap();
            let env = ed
                .wall_envelope(&[(ddc_geom::v2(0.0, 9.0), ddc_geom::v2(5.0, 9.0))])
                .expect("a wall envelope");
            let mut cards = Proposals::default();
            let id = cards.add("a", env, DryRun::default(), ed.model.change_head());
            (ed, cards, id)
        }

        fn entries_since(ed: &Editor, top: i64) -> i64 {
            ed.model
                .sql()
                .query_row("SELECT count(*) FROM op WHERE seq>?1", [top], |r| r.get(0))
                .unwrap()
        }

        #[test]
        fn empty_proposal_not_approved() {
            let (mut ed, mut cards, id) = setup(std::sync::Arc::new(Nothing));
            let top = ed.model.op_top().unwrap();
            let r = apply_proposal(&mut ed, &mut cards, id);
            set_thread_exec(None);
            assert_eq!(r.unwrap_err(), NOTHING_TO_APPLY);
            assert_eq!(
                cards.get(id).unwrap().state,
                ProposalState::Waiting,
                "a run that wrote nothing is not Approved"
            );
            assert_eq!(entries_since(&ed, top), 0);
            assert!(!ed.can_undo());
        }

        #[test]
        fn err_before_write_leaves_card_waiting_and_model_unchanged() {
            let (mut ed, mut cards, id) = setup(std::sync::Arc::new(FailAfterWrite));
            let top = ed.model.op_top().unwrap();
            let levels = ed.model.level_list().unwrap().len();
            let r = apply_proposal(&mut ed, &mut cards, id);
            set_thread_exec(None);
            assert!(r.is_err());
            assert_eq!(cards.get(id).unwrap().state, ProposalState::Waiting);
            assert_eq!(entries_since(&ed, top), 0, "no journal entry");
            assert_eq!(ed.model.level_list().unwrap().len(), levels, "the write was rolled back");
            assert!(!ed.can_undo(), "no undo step");
        }

        #[test]
        fn reload_fail_after_write_settles_approved_once() {
            let (mut ed, mut cards, id) = setup(std::sync::Arc::new(BreakReload));
            let top = ed.model.op_top().unwrap();
            let e = apply_proposal(&mut ed, &mut cards, id).unwrap_err();
            assert!(e.starts_with(APPLIED_RELOAD_FAILED), "{e}");
            assert_eq!(
                cards.get(id).unwrap().state,
                ProposalState::Approved,
                "the step is recorded: the card is never handed back"
            );
            assert_eq!(entries_since(&ed, top), 1, "one journal entry");
            assert!(ed.can_undo(), "one undo step");
            assert!(apply_proposal(&mut ed, &mut cards, id).is_err());
            set_thread_exec(None);
            assert_eq!(entries_since(&ed, top), 1, "a second Apply never re-journals");
        }
    }

    #[test]
    fn mcp_bad_envelope_returns_rejection_path() {
        let f = fixture("bad");
        let r = writer();
        let mut v = good();
        // group 3 uses a slot of group 2 without naming the dependency
        v["envelope"]["groups"][2]["depends_on"] = json!([]);
        let out = dry_run_reply(&r, &f.live, &f.dir, &v, "mcp:agent");
        assert!(out.is_error);
        let b = body(&out);
        assert_eq!(b["status"], "rejected");
        assert!(
            b["rejections"].as_array().unwrap().iter().any(|x| x["code"]
                == "undeclared_dependency"
                && x["path"].as_str().unwrap().starts_with("/groups/2/ops/")),
            "{b}"
        );
        // a version from the future (v2 is current since 6cc36bd6)
        let future = json!({"envelope": {"v": ddc_ops::OP_VERSION + 1}});
        let out = dry_run_reply(&r, &f.live, &f.dir, &future, "a");
        assert_eq!(body(&out)["rejections"][0]["path"], "/v");
        assert_eq!(r.calls.load(Ordering::SeqCst), 0, "a rejected envelope never runs");
    }

    #[test]
    fn envelope_with_unknown_fields_is_rejected_before_any_work() {
        let f = fixture("unknown");
        let r = writer();
        let mut cards = Proposals::default();
        let mut v = good();
        v["envelope"]["apply_now"] = json!(true);
        for out in [
            dry_run_reply(&r, &f.live, &f.dir, &v, "a"),
            propose_reply(&mut cards, &r, (&f.live, 0), &f.dir, &v, "a"),
        ] {
            let b = body(&out);
            assert_eq!(b["status"], "rejected");
            assert_eq!(b["rejections"][0]["code"], "unknown_field", "{b}");
        }
        assert_eq!(r.calls.load(Ordering::SeqCst), 0);
        assert_eq!(cards.waiting().count(), 0);
        assert!(
            files_in(&f.dir).iter().all(|n| n.starts_with("live.db")),
            "no copy was made: {:?}",
            files_in(&f.dir)
        );
    }

    #[test]
    fn a_runner_rejection_leaves_no_card() {
        struct No;
        impl EnvelopeRunner for No {
            fn dry_run(&self, _: &mut Model, _: &Envelope) -> Result<DryRun, String> {
                Ok(DryRun {
                    rejections: vec![Rejection::new("/groups/0/ops/0", "no_host", "")],
                    ..DryRun::default()
                })
            }
        }
        let f = fixture("norun");
        let mut cards = Proposals::default();
        let out = propose_reply(&mut cards, &No, (&f.live, 0), &f.dir, &good(), "a");
        assert_eq!(body(&out)["rejections"][0]["path"], "/groups/0/ops/0");
        assert_eq!(cards.waiting().count(), 0);
    }

    /// Base file with two walls, written from an editor so the rows carry real walls.
    fn walls_base(tag: &str) -> (Fixture, Vec<i64>) {
        let dir = std::env::temp_dir().join(format!("ddc-ops-mcp-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let live = dir.join("live.db");
        let mut ed = Editor::new_in_memory().unwrap();
        let spec = |x: f64| ddc_model::WallSpec {
            a: (x, 0.0),
            b: (x + 4.0, 0.0),
            level: ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ed.types[0].id,
            offset0: None,
        };
        let specs = [spec(0.0), spec(5.0)];
        let ids = ed.model.add_walls(&specs).unwrap();
        ed.model.snapshot_to(&live).unwrap();
        (Fixture { dir, live }, ids)
    }

    fn filter_env(query: &str) -> J {
        let mut env = Envelope::new(Actor::User, "f");
        env.groups.push(ddc_ops::Group {
            id: 1,
            title: "f".into(),
            ops: vec![ddc_ops::Op::Delete {
                target: ddc_ops::Targets::Filter { cat: None, query: query.into() },
            }],
            depends_on: vec![],
            why: String::new(),
        });
        json!({"envelope": serde_json::to_value(env).unwrap()})
    }

    #[test]
    fn proposal_card_keeps_the_frozen_filter_refs() {
        let (f, ids) = walls_base("freeze");
        let r = writer();
        let sets = Sets::default();
        let fz = Some((Units::Metric, &sets));
        let mut cards = Proposals::default();
        let args = filter_env("walls");
        let out = propose_reply_in(&mut cards, &r, (&f.live, 0), &f.dir, &args, "a", fz);
        assert!(!out.is_error, "{}", out.text);
        let id = body(&out)["proposal"].as_u64().unwrap() as u32;
        let want: Vec<ddc_ops::Ref> =
            ids.iter().map(|&id| ddc_ops::Ref::Id { cat: ddc_ops::Cat::Wall, id }).collect();
        assert_eq!(
            cards.get(id).unwrap().envelope.groups[0].ops,
            vec![ddc_ops::Op::Delete { target: ddc_ops::Targets::Refs { refs: want } }],
            "Apply runs the ids the dry run showed"
        );
        let args = filter_env("doors");
        let out = propose_reply_in(&mut cards, &r, (&f.live, 0), &f.dir, &args, "a", fz);
        let b = body(&out);
        assert_eq!(b["status"], "rejected");
        assert_eq!(b["rejections"][0]["code"], "not_found", "{b}");
        assert_eq!(cards.waiting().count(), 1, "an empty filter makes no card");
        assert_eq!(r.calls.load(Ordering::SeqCst), 1, "nothing ran for the empty filter");
    }

    #[test]
    fn without_a_joined_executor_nothing_runs() {
        let f = fixture("unjoined");
        let out = dry_run_reply(&NotJoined, &f.live, &f.dir, &good(), "a");
        assert_eq!(body(&out)["reason"], "executor_not_joined");
    }
}

#[cfg(test)]
#[path = "mcp_ops_draft_tests.rs"]
mod draft_tests;
