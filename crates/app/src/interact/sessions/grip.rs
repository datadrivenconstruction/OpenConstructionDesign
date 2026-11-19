// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Grip drags as one session: an element grip (`grips`), a section/elevation mark
//! grip (`vdrag`) or an opening slid along its wall (`drag`) owns the input under
//! `Scope::GripDrag` from the press that grabs it to the release (or the typed value) that
//! ends it. The grip cache stays in the editor; the active grip lives in the editor flags the
//! handlers read (bridge), the session only routes input to the one that holds it.
use crate::editor::Editor;
use crate::interact::{Cx, End, Hint, Input, Preview, Scope, Step, ToolId, ToolSession};
use crate::tools::Rec;
use crate::wall_tool::Event;
use ddc_model::refusal::{Refusal, RefusalCode};

pub struct GripSession {
    scope: Scope,
    /// Last pointer plan point: Alt pressed or released re-snaps the drag there.
    last: Option<ddc_geom::V2>,
}

/// The grip drag the editor flags describe now, if any.
pub fn live(ed: &Editor) -> Option<Scope> {
    if ed.tool_armed() {
        return None;
    }
    match Scope::from_legacy(ed) {
        s @ Scope::GripDrag { .. } => Some(s),
        _ => None,
    }
}

/// The handler of the drag that runs: element grip, view mark grip, opening slide.
fn route(ed: &mut Editor, e: Event) -> bool {
    if ed.grip.active.is_some() || ed.grip.typing.is_some() {
        crate::grips::handle(ed, e)
    } else if ed.vdrag.is_some() {
        ed.handle_view_grips(e)
    } else if ed.drag.is_some() {
        ed.handle_drag(e)
    } else {
        false
    }
}

impl ToolSession for GripSession {
    fn on_input_ed(&mut self, i: &Input, ed: &mut Editor, _now_ms: u64) -> Step {
        if let Some(q) = i.point() {
            self.last = Some(q);
        }
        let e = match (*i, self.last) {
            // Alt toggles free placement: the drag re-snaps at once, not on the next move
            (Input::Mods(m), Some(q)) => {
                ed.tool.no_snap = m.alt;
                Event::Move(q)
            }
            _ => match i.event() {
                Some(e) => e,
                None => return Step::Continue,
            },
        };
        if !route(ed, e) {
            // what the drag leaves (a key it ignores) goes down the old chain as before
            super::select::route(ed, e);
        }
        match live(ed) {
            Some(s) => {
                self.scope = s;
                Step::Continue
            }
            None => Step::Done,
        }
    }
    fn id(&self) -> ToolId {
        ToolId("grip")
    }
    fn step(&self) -> &'static str {
        "drag"
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
}

/// Run `f` (a handler that may grab a grip); when it grabbed one the drag becomes the session.
/// Returns what `f` returned: the old chain's "took the event".
pub fn grab(ed: &mut Editor, f: impl FnOnce(&mut Editor) -> bool) -> bool {
    let took = f(ed);
    if took && ed.ix.session_id().is_none() {
        if let Some(scope) = live(ed) {
            ed.ix.begin(Box::new(GripSession { scope, last: None }));
        }
    }
    took
}

pub fn running(ed: &Editor) -> bool {
    ed.ix.get::<GripSession>().is_some()
}

/// Drop the drag the flags hold (the forced end of the second Esc), nothing else.
pub fn drop_flags(ed: &mut Editor) {
    ed.grip.active = None;
    ed.grip.typing = None;
    ed.vdrag = None;
    ed.drag = None;
}

pub fn stop(ed: &mut Editor) {
    if running(ed) {
        ed.ix.end(End::Cancel);
    }
}
