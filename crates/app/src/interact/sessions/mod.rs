//! Sessions of tool groups moved off the views: modify —
//! `ModTool` under `Scope::Moving`; grip — a grip, view-mark or opening drag under `Scope::GripDrag`;
//! arch — wall, curtain wall, door/window/opening and the `tools::Kind` tools; section — the
//! section mark tool, which owns its points; model_tool — a `tools::Kind` tool and its `Active`;
//! special — sketch, shape, clip, roof edges, text, topo, solids, masses; view3d — the 3D tools.
pub mod arch;
pub mod grip;
pub mod model_tool;
pub mod modify;
pub mod section;
pub mod select;
pub mod special;
pub mod view3d;

use crate::editor::Editor;

/// End the bridge sessions whose state the editor flags keep, and the section tool: a mode
/// change drops them.
pub fn stop_bridges(ed: &mut Editor) {
    modify::stop(ed);
    grip::stop(ed);
    arch::stop(ed);
    section::stop(ed);
    model_tool::stop(ed);
    special::stop(ed);
}

/// A session that acts on the selection runs (Delete, nudges and the Tab pick wait for it).
pub fn owns_selection(ed: &Editor) -> bool {
    modify::running(ed) || grip::running(ed)
}
