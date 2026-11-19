// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Input with no tool session: selection. The select-mode hooks keep the order the
//! editor always asked them in: section-box cursor, text and solid drags started from a
//! selection, the press that grabs a grip (the drag itself is a `GripSession`), then the
//! selection click, box and keys of the wall tool's select mode, which takes every event.
//! `reset` is the forced end of the second Esc for tools that keep state in editor flags.
use crate::editor::Editor;
use crate::wall_tool::{Event, Mode};

/// One select-mode hook: `f` returns true when it took the event.
#[derive(Clone, Copy)]
pub struct Hook {
    pub id: &'static str,
    pub f: fn(&mut Editor, Event) -> bool,
}

fn grip(ed: &mut Editor, e: Event) -> bool {
    !ed.tool_armed() && super::grip::grab(ed, |ed| crate::grips::handle(ed, e))
}

fn view_grip(ed: &mut Editor, e: Event) -> bool {
    !ed.tool_armed() && !ed.opening_mode && super::grip::grab(ed, |ed| ed.handle_view_grips(e))
}

fn drag(ed: &mut Editor, e: Event) -> bool {
    !ed.tool_armed() && super::grip::grab(ed, |ed| ed.handle_drag(e))
}

/// Selection: click, box, Delete and the keys of the select mode (and the wall tool's own
/// fallback when a session hands an event it leaves). Last, takes every event.
fn select(ed: &mut Editor, e: Event) -> bool {
    ed.handle_wall_tool(e);
    true
}

/// The hooks in priority order.
pub const HOOKS: [Hook; 7] = [
    Hook { id: "clip", f: crate::clip_tools::handle },
    Hook { id: "annot_text", f: crate::annot_text::handle },
    Hook { id: "solid", f: crate::solid_tools::handle },
    Hook { id: "grip", f: grip },
    Hook { id: "view_grip", f: view_grip },
    Hook { id: "drag", f: drag },
    Hook { id: "select", f: select },
];

/// Hooks before the grip press: what came before every tool group in the old order.
pub const BEFORE_TOOLS: &str = "grip";

/// Run `e` through the hooks; returns the id of the one that took it.
pub fn route(ed: &mut Editor, e: Event) -> &'static str {
    for h in HOOKS.iter() {
        if (h.f)(ed, e) {
            return h.id;
        }
    }
    unreachable!("selection takes every event")
}

/// Run `e` through the hooks before the tool groups; true when one took it.
pub fn pre_tools(ed: &mut Editor, e: Event) -> bool {
    HOOKS.iter().take_while(|h| h.id != BEFORE_TOOLS).any(|h| (h.f)(ed, e))
}

/// Drop every tool state the editor flags keep: what the second Esc in a row guarantees.
/// `set_mode(Select)` covers the editor flags; the rest are modes it leaves alone.
pub fn reset(ed: &mut Editor) {
    crate::clip_tools::reset();
    crate::roof_edge_tools::reset();
    ed.sketch = None;
    ed.shape = None;
    ed.grip.active = None;
    ed.grip.typing = None;
    ed.set_mode(Mode::Select);
}
