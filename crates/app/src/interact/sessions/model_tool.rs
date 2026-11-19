// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A `tools::Kind` tool (column … zone) as a session that owns its `Active` state:
//! armed by `tools::arm`, it ends when its step leaves the tool or the mode changes.
use crate::editor::Editor;
use crate::interact::{Cx, End, Hint, Input, Interaction, Preview, Scope, Step, ToolId, ToolSession};
use crate::tools::{Active, Rec};
use crate::wall_tool::Mode;
use ddc_geom::V2;
use ddc_model::refusal::{Refusal, RefusalCode};

pub struct ModelToolSession {
    pub a: Box<Active>,
    /// Typed input of the tool (its points are `a.pts`).
    draft: crate::wall_tool::WallDraft,
}

/// The scope the tool's step means: a placing tool before its first point, else drafting.
pub fn scope_of(a: &Active) -> Scope {
    let tool = ToolId(a.kind.commands().first().copied().unwrap_or("tool"));
    match a.step() {
        "place" => Scope::Placing { tool },
        step => Scope::Drafting { tool, step },
    }
}

impl ToolSession for ModelToolSession {
    fn on_input_ed(&mut self, i: &Input, ed: &mut Editor, _now_ms: u64) -> Step {
        let Some(e) = i.event() else {
            return Step::Continue;
        };
        // the select-mode hooks (section box, text, solids) came before the tools in the old order
        if super::select::pre_tools(ed, e) {
            return Step::Continue;
        }
        let keep = crate::tools::step_active(ed, &mut self.a, e);
        if keep && ed.tool_armed() {
            Step::Continue
        } else {
            if !keep {
                ed.set_mode(Mode::Select);
            }
            Step::Done
        }
    }
    fn id(&self) -> ToolId {
        self.scope().tool().unwrap_or(ToolId("tool"))
    }
    fn step(&self) -> &'static str {
        self.a.step()
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
        // the tool commits through the editor at its own step
        Err(Refusal::new(RefusalCode::Blocked, "ix.refuse.bridge"))
    }
    fn scope(&self) -> Scope {
        scope_of(&self.a)
    }
    fn draws(&self) -> bool {
        true
    }
    fn draft(&self) -> Option<&crate::wall_tool::WallDraft> {
        Some(&self.draft)
    }
    fn draft_mut(&mut self) -> Option<&mut crate::wall_tool::WallDraft> {
        Some(&mut self.draft)
    }
}

/// Start the tool `a` (the mode is already set by `tools::arm`).
pub fn begin(ed: &mut Editor, a: Active) {
    ed.ix.begin(Box::new(ModelToolSession { a: Box::new(a), draft: Default::default() }));
}

pub fn get(ed: &Editor) -> Option<&Active> {
    ed.ix.get::<ModelToolSession>().map(|s| &*s.a)
}

pub fn get_mut(ed: &mut Editor) -> Option<&mut Active> {
    ed.ix.get_mut::<ModelToolSession>().map(|s| &mut *s.a)
}

/// Run `f` on the tool with the editor free to borrow (option bar, commands on the armed
/// tool); a tool that left the drawing mode meanwhile ends.
pub fn with<R>(ed: &mut Editor, f: impl FnOnce(&mut Editor, &mut Active) -> R) -> Option<R> {
    let r = Interaction::with::<ModelToolSession, R>(ed, |s, ed| f(ed, &mut s.a))?;
    if !ed.tool_armed() {
        stop(ed);
    }
    Some(r)
}

/// End the tool and hand its state over (a command or test that keeps the armed tool).
pub fn take(ed: &mut Editor) -> Option<Box<Active>> {
    if !running(ed) {
        return None;
    }
    let s = ed.ix.session.take()?;
    ed.ix.end(End::Cancel);
    s.into_any().downcast::<ModelToolSession>().ok().map(|m| m.a)
}

pub fn running(ed: &Editor) -> bool {
    ed.ix.get::<ModelToolSession>().is_some()
}

pub fn stop(ed: &mut Editor) {
    if running(ed) {
        ed.ix.end(End::Cancel);
    }
}

impl Editor {
    /// The armed `tools::Kind` tool, if its session runs.
    pub fn model_tool(&self) -> Option<&Active> {
        get(self)
    }

    pub fn model_tool_mut(&mut self) -> Option<&mut Active> {
        get_mut(self)
    }

    /// The chain of the session drawing now: a model tool's, a form's, a solid's,
    /// else the wall tool's draft.
    pub fn chain(&self) -> Vec<V2> {
        if let Some(a) = self.model_tool() {
            return a.pts.clone();
        }
        if let Some(t) = &self.mass {
            return t.pts.clone();
        }
        crate::solid_tools::chain().unwrap_or_else(|| self.wall_draft().points.clone())
    }

    /// Straight draft: a model tool draws its own; forms and solids show their chain.
    pub fn draft_preview(&self) -> Vec<(V2, V2)> {
        if self.model_tool().is_some() {
            return vec![];
        }
        if let Some(t) = &self.mass {
            return self.tool.chain_preview(&t.pts);
        }
        match crate::solid_tools::chain() {
            Some(pts) => self.tool.chain_preview(&pts),
            None => self.tool.preview_in(self.ix.draft()),
        }
    }

    /// Arc or spline draft of the chain drawing now; a model tool draws its own.
    pub fn draft_arc(&self) -> Vec<V2> {
        match self.model_tool() {
            Some(_) => vec![],
            None => self.tool.arc_preview_of_in(self.ix.draft(), &self.chain()),
        }
    }

    /// Points are being placed (a model tool's chain or the wall tool's).
    pub fn drawing(&self) -> bool {
        match self.model_tool() {
            Some(a) => !a.pts.is_empty(),
            None => self.tool.is_drawing_in(self.ix.draft()),
        }
    }

    /// Take back the draft: Ctrl+Z in a model tool's chain takes back its last point.
    pub fn reset_draft(&mut self) {
        if let Some(a) = self.model_tool_mut() {
            a.pts.pop();
            self.wall_draft_mut().input.clear();
            return;
        }
        self.tool.reset_in(self.ix.draft_mut());
    }
}
