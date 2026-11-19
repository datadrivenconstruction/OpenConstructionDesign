// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The 3D view's tools under the common scope: box select, measure, 3D creation,
//! work-plane picks, 3D mirror/array, 3D grip and gizmo drags. Bridge (until 86–89): the 3D
//! view keeps its tool and drag, computes the scope they mean each frame and hands it here
//! (`sync`); the session only holds that scope. An end from outside (the second Esc, another
//! session) is seen by the view at its next sync and drops its tool.
use crate::editor::Editor;
use crate::interact::{Cx, End, Hint, Input, Preview, Scope, Step, ToolId, ToolSession};
use crate::tools::Rec;
use ddc_model::refusal::{Refusal, RefusalCode};

pub struct View3dSession {
    scope: Scope,
}

impl ToolSession for View3dSession {
    fn on_input_ed(&mut self, i: &Input, ed: &mut Editor, _now_ms: u64) -> Step {
        // input from another view (the plan after a view switch): the plan groups as without
        // a session; a plan tool armed meanwhile takes over and the 3D tool ends
        let Some(e) = i.event() else {
            return Step::Continue;
        };
        if super::special::route_live(ed, e) || super::arch::route_live(ed, e) {
            return Step::Done;
        }
        super::select::route(ed, e);
        Step::Continue
    }
    fn id(&self) -> ToolId {
        match self.scope {
            Scope::BoxSelect => ToolId("e3d.box"),
            Scope::Measuring => ToolId("e3d.measure"),
            Scope::Pointing => ToolId("e3d.wp_pick"),
            Scope::GripDrag { .. } => ToolId("e3d.grip"),
            Scope::Moving { op } => ToolId(op.command()),
            s => s.tool().unwrap_or(ToolId("e3d")),
        }
    }
    fn step(&self) -> &'static str {
        match self.scope {
            Scope::Drafting { step, .. } => step,
            _ => "3d",
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
}

pub fn running(ed: &Editor) -> bool {
    ed.ix.get::<View3dSession>().is_some()
}

/// Hand the scope the 3D view's tool means now. `prev` is what the view handed last time;
/// returns false when the session it began was ended from outside (the view drops its tool).
pub fn sync(ed: &mut Editor, prev: Scope, want: Scope) -> bool {
    let ours = running(ed);
    if prev != Scope::Idle && prev == want && !ours {
        return false;
    }
    match (want, ours) {
        (Scope::Idle, true) => ed.ix.end(End::Done),
        (Scope::Idle, false) => {}
        (s, true) => {
            if let Some(v) = ed.ix.get_mut::<View3dSession>() {
                v.scope = s;
            }
            ed.ix.follow();
        }
        (s, false) => ed.ix.begin(Box::new(View3dSession { scope: s })),
    }
    true
}
