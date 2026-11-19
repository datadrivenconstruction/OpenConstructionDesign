//! The one executor adapter over `Model::apply_envelope` / `Model::dry_run_envelope`.
//!
//! Every envelope path goes through here: the editor (`Editor::run_ops`, previews, redo), the
//! MCP dry run and propose cards, and the Apply of a design option. `link` is called once at
//! start-up, so no path is left on the refusing default (`Unlinked`, `NotJoined`).

use std::sync::{Arc, Mutex};

use ddc_model::{Model, OptionApplyError, Receipt};
use ddc_ops::{Envelope, Ref};
use serde_json::{json, Value as J};

use crate::ai::session::{
    join_runner, DryRun, EnvelopeRunner, ProposalState, Proposals, Rejection as McpRejection,
};
use crate::editor::ops_run::{link_exec, EnvelopeExec};
use crate::editor::{Editor, OpFn};

/// Model lowering as the editor's executor and the MCP runner.
pub struct ModelExec;

static MODEL_EXEC: ModelExec = ModelExec;

/// Joins the adapter to every seam. Safe to call more than once: the first link stays.
pub fn link() {
    link_exec(Arc::new(ModelExec));
    let _ = join_runner(&MODEL_EXEC);
}

fn rejected(r: ddc_ops::Rejection) -> ddc_model::Error {
    ddc_model::Error::Invalid(r.to_string())
}

impl EnvelopeExec for ModelExec {
    fn apply(&self, m: &mut Model, env: &Envelope) -> Result<Receipt, ddc_model::Error> {
        m.apply_envelope(env).map_err(rejected)
    }
}

fn labels(v: &[Ref]) -> J {
    J::Array(v.iter().map(|r| json!(r.label())).collect())
}

/// Receipt as the `diff` of a dry run reply.
pub fn receipt_json(r: &Receipt) -> J {
    let created: Vec<J> = r
        .created
        .iter()
        .map(|(slot, x)| json!({"slot": slot, "ref": x.label()}))
        .collect();
    json!({
        "created": created,
        "copied": labels(&r.copied),
        "changed": labels(&r.changed),
        "deleted": labels(&r.deleted),
        "loss": r.loss,
    })
}

impl EnvelopeRunner for ModelExec {
    /// `scratch` is already a throwaway copy: the envelope is applied to it directly.
    fn dry_run(&self, scratch: &mut Model, env: &Envelope) -> Result<DryRun, String> {
        match scratch.apply_envelope(env) {
            Ok(r) => Ok(DryRun {
                diff: receipt_json(&r),
                ..DryRun::default()
            }),
            Err(r) => Ok(DryRun {
                rejections: vec![McpRejection::from_ops(&r)],
                ..DryRun::default()
            }),
        }
    }
}

/// The person's Apply on an MCP proposal card held on this thread: one undo step through
/// `Editor::run_ops`, all or nothing. The card is Approved once its step is recorded, even when
/// only the reload after it failed: a card back on Waiting would journal twice on a second Apply.
/// A run that writes nothing is [`NOTHING_TO_APPLY`] and the card waits again.
/// A session's cards live in its worker; `UiSession::apply_proposal` is that path.
pub fn apply_proposal(ed: &mut Editor, cards: &mut Proposals, id: u32) -> Result<Receipt, String> {
    let env = cards.approve(id, &ed.model).map_err(|r| format!("{}: {}", r.code, r.detail))?;
    let pushes = ed.pushes;
    match ed.run_ops(env) {
        Ok(_) if ed.pushes == pushes => {
            cards.settle(id, ProposalState::Waiting);
            Err(NOTHING_TO_APPLY.to_string())
        }
        Ok(r) => {
            cards.settle(id, ProposalState::Approved);
            Ok(r)
        }
        Err(e) if ed.pushes > pushes => {
            cards.settle(id, ProposalState::Approved);
            Err(format!("{APPLIED_RELOAD_FAILED}: {e}"))
        }
        Err(e) => {
            cards.settle(id, ProposalState::Waiting);
            Err(e.to_string())
        }
    }
}

/// `apply_proposal` outcome: the envelope changed nothing, no step and no journal entry.
pub const NOTHING_TO_APPLY: &str = "nothing to apply";
/// `apply_proposal` outcome: the step is recorded, only the view reload failed.
pub const APPLIED_RELOAD_FAILED: &str = "applied, view reload failed";

/// Apply of a design option whose recipe is an ops envelope: one undo step, the option is
/// accepted with the same journal entry (`Model::ai_option_apply_ops`). The only option Apply.
/// The recipe runs as stored: its targets were frozen when it was made (`ai::option_freeze`).
pub fn apply_option(ed: &mut Editor, id: i64) -> Result<(), OptionApplyError> {
    crate::ai::option_freeze::check_frozen(&ed.model, id)?;
    run_option(ed, move |m| m.ai_option_apply_ops(id).map(|_| ()))
}

/// [`apply_option`] of a session store's recipe: its model row is written by the Apply step
/// itself, so a refused Apply leaves no row. A redo finds the row the first run wrote.
pub fn apply_option_rec(
    ed: &mut Editor,
    rec: ddc_model::OptionRec,
) -> Result<(), OptionApplyError> {
    crate::ai::option_freeze::check_frozen_rec(&rec)?;
    run_option(ed, move |m| match crate::ai::options_keep::model_twin(m, &rec) {
        Some(t) => m.ai_option_apply_ops(t.id).map(|_| ()),
        None => m.ai_option_apply_new_ops(&rec).map(|_| ()),
    })
}

fn run_option(
    ed: &mut Editor,
    f: impl Fn(&mut Model) -> Result<(), OptionApplyError> + Send + Sync + 'static,
) -> Result<(), OptionApplyError> {
    let why: Arc<Mutex<Option<OptionApplyError>>> = Arc::default();
    let cell = why.clone();
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        f(m).map_err(|e| {
            let text = format!("{e:?}");
            if let Ok(mut g) = cell.lock() {
                *g = Some(e);
            }
            ddc_model::Error::Invalid(text)
        })
    })))
    .map_err(|e| {
        let got = why.lock().ok().and_then(|mut g| g.take());
        got.unwrap_or(OptionApplyError::Model(e.to_string()))
    })
}
