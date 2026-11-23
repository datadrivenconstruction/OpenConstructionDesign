// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 3D creation on the level plane by the plan tools: the 3D view is only a way to
//! point. Its pointer (`View3dAdapter`) and keys go to `Editor::input`, the same route as the
//! plan, so a wall, floor, column or beam drawn in 3D is the record drawn in plan. Raised and
//! vertical work planes (offset elements, inclined beams) stay with `create`.
//! Any plan tool armed while the 3D view is idle (ribbon, hotkey, script: doors, rooms, stairs,
//! railings, annotations, modify sessions…) is `live` and takes the same route on the level plane.
use super::create::Kind;
use super::plane::WorkPlane;
use super::Key3;
use crate::editor::Editor;
use crate::interact::{Btn, Input, Interaction, Key, Mods, Pointer, Scope, ScopeKind};

/// Plan tool of a 3D creation kind (hotkey of the ribbon).
pub fn hotkey(k: Kind) -> &'static str {
    match k {
        Kind::Wall => "WA",
        Kind::Floor => "SB",
        Kind::Column => "CL",
        Kind::Beam => "BM",
    }
}

/// The work plane is the active level itself: the plan tools apply as they are.
pub fn on_level(wp: &WorkPlane) -> bool {
    wp.is_horizontal() && wp.z().abs() < 1e-6
}

/// A plan tool owns the editor input (drafting, placing, a modify session) and it is not the
/// 3D view's own bridge session: its pointer and keys come from the 3D view.
pub fn live(ed: &Editor) -> bool {
    !crate::interact::sessions::view3d::running(ed)
        && matches!(ed.ix.scope().kind(), ScopeKind::Drafting | ScopeKind::Placing | ScopeKind::Moving)
}

/// `live` and clicks are points of the tool; a modify session still collecting its selection
/// takes the 3D pick instead.
pub fn takes_pointer(ed: &Editor) -> bool {
    live(ed) && !crate::interact::sessions::modify::tool(ed).is_some_and(|t| t.selecting)
}

/// Work plane a live plan tool points on: the active level, whatever plane 3D tools use.
pub fn plane_for(ed: &Editor, own: WorkPlane) -> WorkPlane {
    if live(ed) {
        WorkPlane::level(ed.level, 0.0)
    } else {
        own
    }
}

/// Pointer of this frame: a move when it changed, then the click.
pub fn pointer(ed: &mut Editor, p: Pointer, last: &mut Option<[f32; 2]>, click: bool) {
    if *last != Some(p.screen) {
        ed.input(Input::Move(p));
        *last = Some(p.screen);
    }
    if click {
        ed.input(Input::Click(p, Btn::Left));
    }
}

/// A key of the 3D view for the plan tool; `true` when the tool is left (scope back to Idle).
pub fn key(ed: &mut Editor, k: Key3) -> bool {
    let i = match k {
        Key3::Esc => Input::Key(Key::Escape, Mods::NONE),
        Key3::Tab => Input::Key(Key::Tab, Mods::NONE),
        Key3::Enter => Input::Key(Key::Enter, Mods::NONE),
        Key3::Back => Input::Key(Key::Backspace, Mods::NONE),
        Key3::Char(c) => Input::Char(c),
    };
    ed.input(i);
    *ed.ix.scope() == Scope::Idle
}

/// Leave the plan tool armed for 3D (another 3D tool, a raised plane).
pub fn disarm(ed: &mut Editor) {
    crate::interact::sessions::select::reset(ed);
    Interaction::refresh(ed);
}

#[cfg(test)]
#[path = "plan_route_tests.rs"]
mod tests;
