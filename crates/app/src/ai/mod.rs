// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The AI assistant inside the editor: a worker thread works on its own copy of the model and
//! never touches the live `Editor`; only Apply, on the UI thread, changes the person's model.

pub mod card;
pub mod agent_view;
pub mod apply_verify;
pub mod attach;
pub mod deliver_tool;
pub mod dset_tool;
pub mod eval;
pub mod exec;
pub mod fam_tools;
pub mod fix;
pub mod fmt;
pub mod freeze;
pub mod gen_arcs;
pub mod gen_map;
pub mod gen_rules;
pub mod gen_tools;
pub mod conflict;
pub mod replan;
pub mod create_more;
pub mod station_tool;
pub mod critic_hook;
mod geo_args;
pub mod geo_shape;
pub mod geo_tools;
pub mod measure_tool;
pub mod reference;
pub mod checkpoints;
pub mod receipt;
pub mod refusals;
pub mod grid_tool;
pub mod ghost;
pub mod lower;
pub mod materials;
pub mod modify_cmds;
pub mod modify_plan;
pub mod form_cmds;
pub mod mcp_bridge;
pub mod mep_gate;
pub mod mep_plan;
pub mod mep_tools;
pub mod opt_merge;
pub mod option_exec;
pub mod option_freeze;
pub mod options_gen;
pub mod options_keep;
pub mod plan;
pub mod plan_read;
pub mod point;
pub mod pose;
pub mod snapshot;
pub mod preview;
pub mod project_plan;
pub mod shell_plan;
pub mod project_tools;
pub mod pin_toast;
pub mod plant_plan;
pub mod plant_tools;
pub mod norm_tools;
pub mod qrel;
pub mod qspatial;
pub mod render_off;
pub mod render_req;
pub mod rel_value;
pub mod sched_tool;
pub mod script_tools;
pub mod readset;
pub mod reqbook;
pub mod rows;
pub mod session;
pub mod sets;
pub mod tools_read;
pub mod tools_write;
pub mod type_lib_plan;
pub mod type_lib_tools;
pub mod ui;
pub mod validate;
pub mod verify_view;
pub mod value_edit;
pub mod view_state;
pub mod view_tools;

pub use exec::{apply, ModelStep, NewResolved};
pub use plan::{
    CompiledPlan, ElRef, ElemDiff, FindingRef, IdOrNew, NewElem, NewType, PlanError, PlanItem,
    PlanOp, PlanView, QtyDelta, Targets, VariantInfo,
};
pub use preview::PreviewSet;
pub use session::{
    pump, set_view_rect, with_session, FromWorker, ReqOffer, SessionCfg, SourceCopy, ToWorker,
    UiEvent, UiSession, ViewCtx,
};
pub use ui::register;
pub use validate::{Baseline, IssueLevel, PlanIssue};

#[derive(Clone, Debug, PartialEq)]
pub enum AiError {
    /// Nothing to apply: an undo step without an operation would undo someone else's edit.
    EmptyPlan,
    /// The live model changed after the plan was compiled.
    Stale,
    /// The plan has a blocking issue: the card names it.
    Blocked,
    Busy,
    /// Agent items joined the card after the person looked: they confirm the review first.
    ReviewNeeded,
    /// The plan is committed with its undo step; only the editor's reload failed.
    AppliedNotRefreshed(String),
    /// An agent's write met the person's "require confirmation": it goes on the card instead.
    NeedsConfirmation,
    /// Apply did not write what the draft showed (or could not be checked): taken back, the
    /// text in the UI language names the first difference.
    Diverged(String),
    Model(String),
    Provider(String),
    Internal(String),
}

impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AiError::EmptyPlan => f.write_str("empty plan"),
            AiError::Stale => f.write_str("the model changed: the plan is being recalculated"),
            AiError::Blocked => f.write_str("the plan has a blocking issue"),
            AiError::Busy => f.write_str("busy"),
            AiError::ReviewNeeded => f.write_str("the card changed: review it first"),
            AiError::AppliedNotRefreshed(e) => write!(f, "applied, view not refreshed: {e}"),
            AiError::NeedsConfirmation => f.write_str("the person requires confirmation"),
            AiError::Diverged(e) => write!(f, "apply differed from the draft: {e}"),
            AiError::Model(e) | AiError::Provider(e) | AiError::Internal(e) => f.write_str(e),
        }
    }
}

impl From<ddc_model::Error> for AiError {
    fn from(e: ddc_model::Error) -> Self {
        AiError::Model(e.to_string())
    }
}

impl From<PlanError> for AiError {
    fn from(e: PlanError) -> Self {
        AiError::Model(e.0)
    }
}
