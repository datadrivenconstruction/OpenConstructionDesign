// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Whether the editor is busy with input: a tool (including structural (`model_tool`), terrain, roof edges,
//! bodies), a drag, number entry, sketch and mass modes.
//! Space/Enter quick commands fire only when it is not busy.
use super::Editor;

impl Editor {
    /// A drawing tool holds the plan input (a click places, it does not select). Every reader
    /// asks this, not the wall tool's mode: `WallSession` changes only this body.
    pub fn tool_armed(&self) -> bool {
        self.ix.draws()
    }

    /// The wall tool's drawing mode (`Select` when none is armed): the wall session's kind.
    /// Another drawing session draws a chain: `Polyline` (options and previews follow it).
    pub fn wall_mode(&self) -> crate::wall_tool::Mode {
        use crate::wall_tool::Mode;
        if let Some(m) = crate::interact::sessions::arch::wall_kind(self) {
            return m;
        }
        match self.tool.mode {
            Mode::Select if self.tool_armed() => Mode::Polyline,
            m => m,
        }
    }

    /// The draft the wall engine draws on (chain, typed input, rectangle corner): the drawing
    /// session's (`Interaction::draft`), empty when none draws.
    pub fn wall_draft(&self) -> &crate::wall_tool::WallDraft {
        self.ix.draft()
    }

    pub fn wall_draft_mut(&mut self) -> &mut crate::wall_tool::WallDraft {
        self.ix.draft_mut()
    }

    /// No chain, typed number or corner in the drawing session.
    pub fn wall_idle(&self) -> bool {
        self.tool.is_idle_in(self.ix.draft())
    }

    /// The point the wall engine places now (snap, ortho, angle lock of the draft).
    pub fn wall_snapped(&self) -> ddc_geom::V2 {
        self.tool.snapped_in(self.ix.draft())
    }

    /// Input is in progress inside the editor (Enter/Space belong to it, not to quick commands).
    pub fn is_busy(&self) -> bool {
        self.tool_armed()
            || self.opening_mode
            || self.section_tool().is_some()
            || self.model_tool().is_some()
            || self.grip.typing.is_some()
            || self.drag.is_some()
            || self.vdrag.is_some()
            || self.mass.is_some()
            || self.sketch.is_some()
            || self.shape.is_some()
            || self.topo.tool.is_some()
            || crate::roof_edge_tools::armed()
            || crate::solid_tools::busy()
            || crate::annot_text::busy()
            || self.ix.session_id().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wall_tool::Mode;

    #[test]
    fn esc_in_an_idle_tool_returns_to_select() {
        use crate::wall_tool::{Event, Key};
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Polyline);
        ed.handle(Event::Key(Key::Escape));
        assert_eq!(ed.wall_mode(), Mode::Select);
        // with a chain in progress the first Esc only finishes it
        ed.set_mode(Mode::Polyline);
        ed.handle(Event::Click(ddc_geom::v2(0.0, 0.0)));
        assert!(!ed.wall_idle());
        ed.handle(Event::Key(Key::Escape));
        assert_eq!(ed.wall_mode(), Mode::Polyline);
        ed.handle(Event::Key(Key::Escape));
        assert_eq!(ed.wall_mode(), Mode::Select);
    }

    #[test]
    fn opening_a_model_leaves_the_drawing_tool() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Polyline);
        ed.replace_model(ddc_model::Model::open(":memory:").unwrap())
            .unwrap();
        assert_eq!(ed.wall_mode(), Mode::Select);
        assert!(!ed.is_busy());
    }

    #[test]
    fn busy_in_tools_and_modes() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Select);
        assert!(!ed.is_busy());
        ed.set_mode(Mode::Polyline);
        assert!(ed.is_busy());
        ed.set_mode(Mode::Select);
        assert!(!ed.is_busy());
        ed.opening_mode = true;
        assert!(ed.is_busy());
        ed.opening_mode = false;
        ed.grip.typing = Some((0, "12".into()));
        assert!(ed.is_busy());
        ed.grip.typing = None;
        assert!(!ed.is_busy());
        ed.topo.tool = Some(crate::topo::Tool::Points);
        assert!(ed.is_busy());
        ed.topo.tool = None;
        assert!(!ed.is_busy());
    }
}
