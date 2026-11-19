// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Edge fade for scroll areas: shown only on the side where content continues.

use crate::theme::space;
use crate::Theme;
use egui::{pos2, Color32, Mesh, Rect, Shape, Ui};

/// Height of the fade band.
pub const FADE_H: f32 = space::LG;

/// Which edges of a viewport hide content: (top, bottom). Nothing when it all fits.
pub fn overflow_edges(offset: f32, content: f32, view: f32) -> (bool, bool) {
    let max = content - view;
    if max <= 1.0 {
        return (false, false);
    }
    (offset > 1.0, offset < max - 1.0)
}

/// Gradient band from `solid` at the edge to transparent `h` further in.
fn band(rect: Rect, top: bool, solid: Color32) -> Shape {
    let (edge, inner) = if top {
        (rect.top(), rect.top() + FADE_H)
    } else {
        (rect.bottom(), rect.bottom() - FADE_H)
    };
    let clear = Color32::TRANSPARENT;
    let mut m = Mesh::default();
    m.colored_vertex(pos2(rect.left(), edge), solid);
    m.colored_vertex(pos2(rect.right(), edge), solid);
    m.colored_vertex(pos2(rect.right(), inner), clear);
    m.colored_vertex(pos2(rect.left(), inner), clear);
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    Shape::mesh(m)
}

/// Paints the fades of a finished scroll area over its viewport.
pub fn scroll_fade<R>(ui: &Ui, th: &Theme, out: &egui::scroll_area::ScrollAreaOutput<R>) {
    let view = out.inner_rect;
    let (top, bottom) =
        overflow_edges(out.state.offset.y, out.content_size.y, view.height());
    if view.height() < FADE_H * 2.0 {
        return;
    }
    let solid = th.sem().bg_panel;
    let p = ui.painter();
    if top {
        p.add(band(view, true, solid));
    }
    if bottom {
        p.add(band(view, false, solid));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_only_when_content_overflows() {
        assert_eq!(overflow_edges(0.0, 100.0, 100.0), (false, false));
        assert_eq!(overflow_edges(0.0, 100.5, 100.0), (false, false));
        assert_eq!(overflow_edges(0.0, 300.0, 100.0), (false, true));
        assert_eq!(overflow_edges(50.0, 300.0, 100.0), (true, true));
        assert_eq!(overflow_edges(200.0, 300.0, 100.0), (true, false));
    }

    #[test]
    fn band_is_solid_at_the_edge_and_clear_inside() {
        let r = Rect::from_min_max(pos2(0.0, 0.0), pos2(80.0, 100.0));
        let solid = Color32::from_rgb(10, 20, 30);
        let Shape::Mesh(m) = band(r, false, solid) else {
            panic!("mesh expected");
        };
        let edge: Vec<_> = m.vertices.iter().filter(|v| v.pos.y == 100.0).collect();
        assert_eq!(edge.len(), 2);
        assert!(edge.iter().all(|v| v.color == solid));
        assert!(m
            .vertices
            .iter()
            .filter(|v| v.pos.y < 100.0)
            .all(|v| v.color == Color32::TRANSPARENT));
    }
}
