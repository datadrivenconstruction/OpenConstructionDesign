// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The special plan modes as one session: section box, boundary sketch, slab shape
//! edit, roof edges, text and annotations, topography, solids and masses own the input from the
//! arm to the finish or Esc that leaves them. Bridge: each mode keeps its
//! state where it was (editor fields, module state) and commits through the editor; the session
//! routes input to the one armed. Clip, text and solid stay in the old chain as select-mode
//! hooks only (cursor tracking, text and solid drags started from a selection).
use crate::editor::Editor;
use crate::interact::{Cx, End, Hint, Input, Preview, Scope, Step, ToolId, ToolSession};
use crate::tools::Rec;
use crate::wall_tool::Event;
use ddc_model::refusal::{Refusal, RefusalCode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Special {
    Clip,
    Sketch,
    Shape,
    RoofEdge,
    Text,
    Topo,
    Solid,
    Mass,
}

/// The old chain order: an earlier mode keeps its priority, a hook still sees the event first.
const ORDER: [(Special, fn(&mut Editor, Event) -> bool, bool); 8] = [
    (Special::Clip, crate::clip_tools::handle, true),
    (Special::Sketch, crate::sketch_mode::handle, false),
    (Special::Shape, crate::shape_edit::handle, false),
    (Special::RoofEdge, crate::roof_edge_tools::handle, false),
    (Special::Text, crate::annot_text::handle, true),
    (Special::Topo, crate::topo::handle, false),
    (Special::Solid, crate::solid_tools::handle, true),
    (Special::Mass, crate::mass_tools::handle, false),
];

pub struct SpecialSession {
    k: Special,
    scope: Scope,
    /// The mode draws in the plan now (`draws_now`).
    draws: bool,
}

/// Whether the mode `k` draws now: text, solid and plan form placement, topo contours (they
/// held the wall's drawing flag earlier) and the boundary sketch (it draws its chain).
/// Clip, shapes and roof edges place or pick, they do not draw.
fn draws_now(ed: &Editor, k: Special) -> bool {
    match k {
        Special::Text => crate::annot_text::armed_command().is_some(),
        Special::Solid => crate::solid_tools::armed_command(ed).is_some(),
        Special::Mass => ed.mass.as_ref().is_some_and(|m| m.cmd.in_plan()),
        Special::Topo => ed.topo.tool.is_some_and(|t| t != crate::topo::Tool::Edit),
        Special::Sketch => true,
        Special::Clip | Special::Shape | Special::RoofEdge => false,
    }
}

/// The special mode armed now, in the old chain's order, with its scope.
pub fn live(ed: &Editor) -> Option<(Special, Scope)> {
    let k = if crate::clip_tools::armed_command(ed).is_some() {
        Special::Clip
    } else if ed.sketch.is_some() {
        Special::Sketch
    } else if ed.shape.is_some() {
        Special::Shape
    } else if crate::roof_edge_tools::armed_command(ed).is_some() {
        Special::RoofEdge
    } else if crate::annot_text::dragging() || crate::annot_text::armed_command().is_some() || crate::annot_text::busy() {
        Special::Text
    } else if ed.topo.tool.is_some() {
        Special::Topo
    } else if crate::solid_tools::dragging() || crate::solid_tools::armed_command(ed).is_some() {
        Special::Solid
    } else if ed.mass.is_some() && ed.model_tool().is_none() {
        // (a `tools::Kind` tool came before masses in the chain)
        Special::Mass
    } else {
        return None;
    };
    Some((k, Scope::from_legacy(ed)))
}

fn route(ed: &mut Editor, k: Special, e: Event) -> bool {
    for (kk, f, hook) in ORDER {
        if kk == k {
            return f(ed, e);
        }
        if hook && f(ed, e) {
            return true;
        }
    }
    false
}

impl ToolSession for SpecialSession {
    fn on_input_ed(&mut self, i: &Input, ed: &mut Editor, _now_ms: u64) -> Step {
        let Some(e) = i.event() else {
            return Step::Continue;
        };
        match live(ed) {
            Some((k, _)) => self.k = k,
            None => {
                super::select::route(ed, e);
                return Step::Done;
            }
        }
        if !route(ed, self.k, e) {
            super::select::route(ed, e);
        }
        match live(ed) {
            Some((k, s)) => {
                self.k = k;
                self.scope = s;
                self.draws = draws_now(ed, k);
                Step::Continue
            }
            None => Step::Done,
        }
    }
    fn id(&self) -> ToolId {
        match self.scope {
            Scope::Reshaping { .. } => ToolId("shape"),
            Scope::Moving { .. } => ToolId("special.drag"),
            s => s.tool().unwrap_or(ToolId("special")),
        }
    }
    fn step(&self) -> &'static str {
        match self.scope {
            Scope::Drafting { step, .. } => step,
            Scope::Placing { .. } => "place",
            _ => "edit",
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
        Err(Refusal::new(RefusalCode::Blocked, "ix.refuse.bridge"))
    }
    fn scope(&self) -> Scope {
        self.scope
    }
    fn draws(&self) -> bool {
        self.draws
    }
}

/// Route `e` to the special mode armed now, if any (input under another view's session).
pub fn route_live(ed: &mut Editor, e: Event) -> bool {
    live(ed).is_some_and(|(k, _)| {
        if !route(ed, k, e) {
            super::select::route(ed, e);
        }
        true
    })
}

/// Begin the session when a special mode is armed and no session runs.
pub fn adopt(ed: &mut Editor) {
    if ed.ix.session_id().is_none() {
        if let Some((k, scope)) = live(ed) {
            let draws = draws_now(ed, k);
            ed.ix.begin(Box::new(SpecialSession { k, scope, draws }));
        }
    }
}

/// Follow a command that re-armed a mode outside input; `None` when no such session runs or it ended.
pub fn refresh(ed: &mut Editor) -> Option<Scope> {
    running(ed).then_some(())?;
    match live(ed) {
        Some((k, scope)) => {
            let draws = draws_now(ed, k);
            let s = ed.ix.get_mut::<SpecialSession>()?;
            s.draws = draws;
            s.k = k;
            s.scope = scope;
            Some(scope)
        }
        None => {
            ed.ix.end(End::Cancel);
            None
        }
    }
}

pub fn running(ed: &Editor) -> bool {
    ed.ix.get::<SpecialSession>().is_some()
}

pub fn kind(ed: &Editor) -> Option<Special> {
    ed.ix.get::<SpecialSession>().map(|s| s.k)
}

pub fn stop(ed: &mut Editor) {
    if running(ed) {
        ed.ix.end(End::Cancel);
    }
}
