//! Pointer icons of the navigation drags: 3D orbit and pan, view cube faces and the family
//! canvas (reference planes, pan). Pure tables; callers set the icon only when one is returned.

use egui::CursorIcon;

/// What the primary or middle button is doing in a model viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drag {
    None,
    Orbit,
    Pan,
    /// A tool owns the drag and sets its own icon.
    Tool,
}

/// Which drag a pointer press starts in the 3D view: middle button or shift+primary pans,
/// primary orbits, unless a tool captured the pointer.
pub fn drag_of(
    dragged_primary: bool,
    dragged_middle: bool,
    shift: bool,
    capture: bool,
) -> Drag {
    if capture && !shift && dragged_primary {
        Drag::Tool
    } else if dragged_middle || (shift && dragged_primary && !capture) {
        Drag::Pan
    } else if dragged_primary && !capture {
        Drag::Orbit
    } else {
        Drag::None
    }
}

/// Icon of the 3D view: Grabbing while panning, Move while orbiting, Grab when shift readies a
/// pan, PointingHand over a view cube face. `None` keeps whatever the tool or panel set.
pub fn cursor_for(drag: Drag, shift: bool, capture: bool, over_cube: bool) -> Option<CursorIcon> {
    match drag {
        Drag::Pan => Some(CursorIcon::Grabbing),
        Drag::Orbit => Some(CursorIcon::Move),
        Drag::Tool => None,
        Drag::None if over_cube => Some(CursorIcon::PointingHand),
        Drag::None if shift && !capture => Some(CursorIcon::Grab),
        Drag::None => None,
    }
}

/// Family canvas reference plane: a vertical line (`x` plane) slides sideways, a horizontal
/// one up and down.
pub fn plane_cursor(x_plane: bool) -> CursorIcon {
    if x_plane {
        CursorIcon::ResizeHorizontal
    } else {
        CursorIcon::ResizeVertical
    }
}

/// Family canvas: panning with the middle button wins, then a plane under or in the drag.
pub fn canvas_cursor(panning: bool, plane: Option<bool>) -> Option<CursorIcon> {
    if panning {
        Some(CursorIcon::Grabbing)
    } else {
        plane.map(plane_cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CursorIcon as C;

    #[test]
    fn drag_kinds_follow_buttons_and_capture() {
        assert_eq!(drag_of(true, false, false, false), Drag::Orbit);
        assert_eq!(drag_of(true, false, true, false), Drag::Pan);
        assert_eq!(drag_of(false, true, false, false), Drag::Pan);
        assert_eq!(drag_of(true, false, false, true), Drag::Tool);
        assert_eq!(drag_of(true, false, true, true), Drag::None);
        assert_eq!(drag_of(false, false, false, false), Drag::None);
    }

    #[test]
    fn view_cursor_table() {
        assert_eq!(cursor_for(Drag::Orbit, false, false, false), Some(C::Move));
        assert_eq!(cursor_for(Drag::Pan, false, false, false), Some(C::Grabbing));
        assert_eq!(cursor_for(Drag::Tool, false, true, false), None);
        assert_eq!(cursor_for(Drag::None, false, false, true), Some(C::PointingHand));
        assert_eq!(cursor_for(Drag::None, true, false, false), Some(C::Grab));
        // a tool owning the pointer keeps its own icon; shift does not promise a pan there
        assert_eq!(cursor_for(Drag::None, true, true, false), None);
        assert_eq!(cursor_for(Drag::None, false, false, false), None);
        // the cube stays a button while a drag runs elsewhere
        assert_eq!(cursor_for(Drag::Orbit, false, false, true), Some(C::Move));
    }

    #[test]
    fn family_canvas_table() {
        assert_eq!(canvas_cursor(false, Some(true)), Some(C::ResizeHorizontal));
        assert_eq!(canvas_cursor(false, Some(false)), Some(C::ResizeVertical));
        assert_eq!(canvas_cursor(true, Some(true)), Some(C::Grabbing));
        assert_eq!(canvas_cursor(false, None), None);
    }
}
