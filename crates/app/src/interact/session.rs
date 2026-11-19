// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ToolSession`: a tool reads input and the editor, draws a preview and returns
//! a record to commit. It never writes the model: `Interaction` applies the record through
//! `Editor::apply_ext` (one undo step, one write path).
use super::hints::{Hint, HintValue};
use super::input::Input;
use super::preview::Preview;
use super::scope::{Scope, ToolId};
use crate::editor::Editor;
use crate::tools::Rec;
use ddc_model::refusal::Refusal;

/// Read-only window into the editor for a session.
pub struct Cx<'a> {
    pub ed: &'a Editor,
    pub now_ms: u64,
}

/// What the session wants after an input.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Keep collecting.
    Continue,
    /// Call `commit` and apply its record; the session stays (chains go on).
    Commit,
    /// Finished: end without a commit.
    Done,
    /// Cancelled: end without a commit.
    Cancel,
    /// Declined this input; the session stays and the refusal is shown.
    Refuse(Refusal),
}

/// Downcast of a running session (views read the modify tool's state to draw it).
pub trait AsAny: std::any::Any {
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    /// The session by value (a tool handed back to code that keeps it, as tests do).
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any>;
}

impl<T: std::any::Any> AsAny for T {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

pub trait ToolSession: AsAny {
    /// Bridge for tools that still commit through the editor themselves (`ModTool`, grips):
    /// they get the editor mutably. Pure sessions keep the default, `on_input` + `commit`.
    /// A session begun inside this one (`Interaction::begin_child`) ended; this one runs again.
    /// Bridges commit through the editor, so the child's record is already applied.
    fn on_child(&mut self, _why: super::End) {}

    fn on_input_ed(&mut self, i: &Input, ed: &mut Editor, now_ms: u64) -> Step {
        self.on_input(i, &Cx { ed: &*ed, now_ms })
    }
    fn id(&self) -> ToolId;
    /// Hint key of the current step (`hint.<tool>.<step>`).
    fn step(&self) -> &'static str;
    fn hints(&self) -> &[Hint];
    /// Current values the hint card shows next to the keys («Snap: end»).
    fn values(&self, _cx: &Cx) -> Vec<HintValue> {
        Vec::new()
    }
    fn on_input(&mut self, i: &Input, cx: &Cx) -> Step;
    fn preview(&self, cx: &Cx, out: &mut Preview);
    fn commit(&mut self, cx: &Cx) -> Result<Rec, Refusal>;
    /// Views the session works in; the parity gate replays each of them. All by default.
    fn views(&self) -> super::parity::ViewMask {
        super::parity::ViewMask::ALL
    }
    /// The session draws in the plan (a chain, a placement): `Editor::tool_armed` is this.
    /// Selection, grips and views do not.
    fn draws(&self) -> bool {
        false
    }
    /// The draft the wall engine draws on (chain, typed input, corner): the wall session's and
    /// a model tool's own; `Interaction::draft` reads it.
    fn draft(&self) -> Option<&crate::wall_tool::WallDraft> {
        None
    }
    fn draft_mut(&mut self) -> Option<&mut crate::wall_tool::WallDraft> {
        None
    }
    /// Scope while the session runs; a collecting tool by default.
    fn scope(&self) -> Scope {
        Scope::Drafting { tool: self.id(), step: self.step() }
    }
}
