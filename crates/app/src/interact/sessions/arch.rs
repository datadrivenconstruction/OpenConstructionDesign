// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The architecture group as one session: door/window/opening placing (`opening_mode`) and wall / curtain wall
//! drawing own the input under `Scope::Drafting`/`Placing` from the arm to the commit or Esc
//! that leaves the tool. The wall session (`Arch::Wall`) carries its drawing kind; the editor
//! reads it through `Editor::wall_mode`, and owns the wall draft (`Editor::wall_draft`). Bridge: the tools keep their state in
//! the editor flags and commit through the editor; the session routes input to the one armed.
use crate::editor::Editor;
use crate::interact::{Cx, End, Hint, Input, Preview, Scope, Step, ToolId, ToolSession};
use crate::tools::Rec;
use crate::wall_tool::{Event, Mode, WallDraft};
use ddc_model::refusal::{Refusal, RefusalCode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    Opening,
    /// Wall session: wall, wall room, curtain wall, wall on a picked line (the kind).
    Wall(Mode),
}

pub struct ArchSession {
    k: Arch,
    scope: Scope,
    /// What the wall engine draws now (chain, typed input, corner): it ends with the session.
    draft: WallDraft,
}

/// The architecture tool the editor flags arm now, with its scope; `None` when another group
/// (sketch, topo, mass…) holds the input or nothing is armed.
pub fn live(ed: &Editor) -> Option<(Arch, Scope)> {
    let k = if ed.model_tool().is_some() || ed.mass.is_some() {
        // a `tools::Kind` tool is its own session (`model_tool`)
        return None;
    } else if ed.opening_mode {
        Arch::Opening
    } else if ed.tool.armed() {
        // the wall tool's own flag is the arm request; the session carries the kind from here
        Arch::Wall(ed.tool.mode)
    } else {
        return None;
    };
    // a group earlier in the chain (clip, sketch, shape, roof edge, text, topo, solid) owns it
    let s = Scope::from_legacy(ed);
    (s.tool() == Some(ToolId(command(ed, k)))).then_some((k, s))
}

/// The command `from_legacy` names for `k` when no earlier group is armed.
fn command(ed: &Editor, k: Arch) -> &'static str {
    match k {
        Arch::Opening => match ed.otool.ty.kind {
            crate::opening_tool::Kind::Door => "arch.door",
            crate::opening_tool::Kind::Window => "arch.window",
            crate::opening_tool::Kind::Void => "arch.opening",
        },
        Arch::Wall(m) => match m {
            Mode::PickLine => crate::line_pick::command(&ed.tool),
            Mode::Polyline if ed.tool.curtain => "arch.curtain_wall",
            Mode::Polyline => "arch.wall",
            Mode::Rect => "arch.wall_room",
            Mode::Select => "",
        },
    }
}

fn route(ed: &mut Editor, k: Arch, e: Event) -> bool {
    match k {
        Arch::Opening => {
            ed.handle_opening(e);
            true
        }
        Arch::Wall(_) => {
            ed.handle_wall_tool(e);
            true
        }
    }
}

impl ToolSession for ArchSession {
    fn on_input_ed(&mut self, i: &Input, ed: &mut Editor, _now_ms: u64) -> Step {
        let Some(e) = i.event() else {
            return Step::Continue;
        };
        // a ribbon command may have armed another tool of the group since the last input
        match live(ed) {
            Some((k, _)) => self.k = k,
            None => {
                super::select::route(ed, e);
                return Step::Done;
            }
        }
        let took = super::select::pre_tools(ed, e);
        if !took && !route(ed, self.k, e) {
            super::select::route(ed, e);
        }
        match live(ed) {
            Some((k, s)) => {
                self.k = k;
                self.scope = s;
                Step::Continue
            }
            None => Step::Done,
        }
    }
    fn id(&self) -> ToolId {
        self.scope.tool().unwrap_or(ToolId("arch"))
    }
    fn step(&self) -> &'static str {
        match self.scope {
            Scope::Drafting { step, .. } => step,
            _ => "place",
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
        matches!(self.k, Arch::Wall(m) if m != Mode::Select)
    }
    fn draft(&self) -> Option<&WallDraft> {
        Some(&self.draft)
    }
    fn draft_mut(&mut self) -> Option<&mut WallDraft> {
        Some(&mut self.draft)
    }
}

/// Route `e` to the architecture tool armed now, if any (input under another view's session).
pub fn route_live(ed: &mut Editor, e: Event) -> bool {
    live(ed).is_some_and(|(k, _)| {
        let took = super::select::pre_tools(ed, e);
        if !took && !route(ed, k, e) {
            super::select::route(ed, e);
        }
        true
    })
}

/// Begin the session when an architecture tool is armed and no session runs.
pub fn adopt(ed: &mut Editor) {
    if ed.ix.session_id().is_none() {
        if let Some((k, scope)) = live(ed) {
            ed.ix.begin(Box::new(ArchSession { k, scope, draft: WallDraft::default() }));
        }
    }
}

/// Follow a command that re-armed the flags outside input (ribbon): the session takes the new
/// tool of the group, or ends when the group holds nothing. The scope it returns is the new one.
pub fn refresh(ed: &mut Editor) -> Option<Scope> {
    running(ed).then_some(())?;
    match live(ed) {
        Some((k, scope)) => {
            let s = ed.ix.get_mut::<ArchSession>()?;
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
    ed.ix.get::<ArchSession>().is_some()
}

/// The drawing kind of the running wall session.
pub fn wall_kind(ed: &Editor) -> Option<Mode> {
    match kind(ed)? {
        Arch::Wall(m) => Some(m),
        Arch::Opening => None,
    }
}

pub fn kind(ed: &Editor) -> Option<Arch> {
    ed.ix.get::<ArchSession>().map(|s| s.k)
}

pub fn stop(ed: &mut Editor) {
    if running(ed) {
        ed.ix.end(End::Cancel);
    }
}
