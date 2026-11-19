// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The section mark tool as a session that owns its points: two clicks
//! for the line, the third for the depth side; Esc steps back, then ends the tool.
use crate::editor::sec::SectionTool;
use crate::editor::Editor;
use crate::interact::{Cx, End, Hint, Input, Preview, Scope, Step, ToolId, ToolSession};
use crate::tools::Rec;
use ddc_model::refusal::{Refusal, RefusalCode};

pub const ID: &str = "arch.section";

#[derive(Default)]
pub struct SectionSession {
    pub t: SectionTool,
}

impl ToolSession for SectionSession {
    fn on_input_ed(&mut self, i: &Input, ed: &mut Editor, _now_ms: u64) -> Step {
        let Some(e) = i.event() else {
            return Step::Continue;
        };
        // the select-mode hooks (section box, text, solids) came before the tool in the old order
        if super::select::pre_tools(ed, e) {
            return Step::Continue;
        }
        match ed.handle_section(&mut self.t, e) {
            true => Step::Continue,
            false => Step::Done,
        }
    }
    fn id(&self) -> ToolId {
        ToolId(ID)
    }
    fn step(&self) -> &'static str {
        match (self.t.a, self.t.b) {
            (None, _) => "first",
            (Some(_), None) => "next",
            (Some(_), Some(_)) => "depth",
        }
    }
    fn hints(&self) -> &[Hint] {
        &[]
    }
    fn on_input(&mut self, _i: &Input, _cx: &Cx) -> Step {
        // never called: `on_input_ed` takes every input
        Step::Continue
    }
    fn preview(&self, _cx: &Cx, _out: &mut Preview) {}
    fn commit(&mut self, _cx: &Cx) -> Result<Rec, Refusal> {
        // the third click adds the view itself (one journal entry)
        Err(Refusal::new(RefusalCode::Blocked, "ix.refuse.bridge"))
    }
    fn scope(&self) -> Scope {
        // the two steps the flag scope named; the depth step stays "next"
        let step = if self.t.a.is_some() { "next" } else { "first" };
        Scope::Drafting { tool: ToolId(ID), step }
    }
}

pub fn tool(ed: &Editor) -> Option<&SectionTool> {
    ed.ix.get::<SectionSession>().map(|s| &s.t)
}

pub fn running(ed: &Editor) -> bool {
    ed.ix.get::<SectionSession>().is_some()
}

pub fn stop(ed: &mut Editor) {
    if running(ed) {
        ed.ix.end(End::Cancel);
    }
}
