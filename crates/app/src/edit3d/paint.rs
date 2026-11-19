// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Drawing over 3D: selection and hover outlines, grips, gizmo, snap icons,
//! work-plane grid, selection box (window is solid, crossing is dashed),
//! temporary dimensions and the value input field.

use ddc_render::glam::Vec3;
use egui::{Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};

use super::gizmo::{self, Handle};
use super::math::Cam;
use super::snap::SnapKind;
use crate::overlay::{self, Grip, Palette, SnapGlyph};

const MAX_EDGES: usize = 30_000;

pub fn seg(p: &Painter, cam: &Cam, a: Vec3, b: Vec3, s: Stroke) {
    if let (Some(a), Some(b)) = (cam.to_screen(a), cam.to_screen(b)) {
        p.line_segment([a, b], s);
    }
}

pub fn dashed(p: &Painter, cam: &Cam, a: Vec3, b: Vec3, s: Stroke) {
    if let (Some(a), Some(b)) = (cam.to_screen(a), cam.to_screen(b)) {
        p.extend(Shape::dashed_line(&[a, b], s, 6.0, 4.0));
    }
}

pub fn polyline(p: &Painter, cam: &Cam, pts: &[Vec3], s: Stroke) {
    for w in pts.windows(2) {
        seg(p, cam, w[0], w[1], s);
    }
}

pub fn edges(p: &Painter, cam: &Cam, es: &[[Vec3; 2]], s: Stroke, f: impl Fn(Vec3) -> Vec3) {
    for e in es.iter().take(MAX_EDGES) {
        seg(p, cam, f(e[0]), f(e[1]), s);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// Point grip (end, vertex, move).
    Square,
    /// Line grip (height, sill, thickness).
    Arrow,
    /// Rotation grip.
    Circle,
}

pub fn grip(p: &Painter, at: Pos2, g: Glyph, dir: Vec2, hot: bool, col: Color32, ov: &Palette) {
    let shape = match g {
        Glyph::Square => Grip::Square,
        Glyph::Arrow => Grip::Arrows(dir),
        Glyph::Circle => Grip::Ring,
    };
    overlay::grip_col(p, at, shape, hot, col, ov);
}

/// Snap icon and kind label: the same visual language as in the plan.
pub fn snap(p: &Painter, at: Pos2, k: SnapKind, label: &str, ov: &Palette) {
    let glyph = match k {
        SnapKind::Vertex => SnapGlyph::Square,
        SnapKind::Mid | SnapKind::Param => SnapGlyph::Triangle,
        SnapKind::Intersection | SnapKind::GridX => SnapGlyph::Cross,
        SnapKind::Tangent => SnapGlyph::Circle,
        SnapKind::Perp => SnapGlyph::Perp,
        SnapKind::Edge | SnapKind::Curve => SnapGlyph::Hourglass,
        SnapKind::Iso => SnapGlyph::Diamond,
        SnapKind::Surface => SnapGlyph::DiamondDot,
        SnapKind::Face => SnapGlyph::Circle,
        SnapKind::GridLine | SnapKind::Plane => SnapGlyph::Plus,
    };
    overlay::snap_glyph(p, at, glyph, ov);
    if k != SnapKind::Plane {
        overlay::snap_label(p, at, label.to_string(), ov);
    }
}

/// Label on a plate; returns its rectangle (for clicks).
pub fn label(
    p: &Painter,
    at: Pos2,
    text: &str,
    fg: Color32,
    bg: Color32,
    frame: Option<Color32>,
) -> Rect {
    let g = p.layout_no_wrap(
        text.to_string(),
        FontId::proportional(ddc_ui::theme::type_size::LABEL),
        fg,
    );
    let r = Rect::from_center_size(at, g.size() + Vec2::new(8.0, 4.0));
    p.rect_filled(r, ddc_ui::theme::radius::SM, bg);
    if let Some(c) = frame {
        p.rect_stroke(r, 3.0, Stroke::new(1.0, c), StrokeKind::Inside);
    }
    p.galley(r.min + Vec2::new(4.0, 2.0), g, fg);
    r
}

/// Label from the top-left corner (tool hint).
pub fn note(p: &Painter, left_top: Pos2, text: &str, fg: Color32, bg: Color32) -> Rect {
    let g = p.layout_no_wrap(
        text.to_string(),
        FontId::proportional(ddc_ui::theme::type_size::LABEL),
        fg,
    );
    let r = Rect::from_min_size(left_top, g.size() + Vec2::new(10.0, 6.0));
    p.rect_filled(r, ddc_ui::theme::radius::SM, bg);
    p.galley(r.min + Vec2::new(5.0, 3.0), g, fg);
    r
}

/// Temporary dimension: extension ticks at the ends and the value in the middle.
pub fn dim(
    p: &Painter,
    cam: &Cam,
    a: Vec3,
    b: Vec3,
    text: &str,
    c: Color32,
    bg: Color32,
) -> Option<Rect> {
    let (sa, sb) = (cam.to_screen(a)?, cam.to_screen(b)?);
    let s = Stroke::new(1.0, c);
    p.line_segment([sa, sb], s);
    let d = sb - sa;
    let n = if d.length_sq() > 1e-6 {
        Vec2::new(-d.y, d.x).normalized() * 4.0
    } else {
        Vec2::ZERO
    };
    p.line_segment([sa - n, sa + n], s);
    p.line_segment([sb - n, sb + n], s);
    Some(label(p, sa + d * 0.5, text, c, bg, Some(c)))
}

pub fn gizmo(
    p: &Painter,
    cam: &Cam,
    g: &gizmo::Shape,
    hot: Option<Handle>,
    rings: u8,
    ov: &Palette,
) {
    let w = |h: Handle| if hot == Some(h) { 3.5 } else { 2.0 };
    for i in (0..3u8).filter(|i| rings & (1 << i) != 0) {
        let col = ov.axis[i as usize].gamma_multiply(0.85);
        polyline(p, cam, &g.ring(i), Stroke::new(w(Handle::Ring(i)), col));
    }
    for i in 0..3u8 {
        let q = g.plane_quad(i);
        let s: Vec<Pos2> = q.iter().filter_map(|v| cam.to_screen(*v)).collect();
        if s.len() == 4 {
            let col = ov.axis[i as usize];
            let a = if hot == Some(Handle::Plane(i)) {
                0.55
            } else {
                0.25
            };
            p.add(Shape::convex_polygon(
                s,
                col.gamma_multiply(a),
                Stroke::new(1.0, col),
            ));
        }
    }
    for i in 0..3u8 {
        let col = ov.axis[i as usize];
        let (Some(o), Some(e)) = (cam.to_screen(g.origin), cam.to_screen(g.axis_end(i))) else {
            continue;
        };
        p.line_segment([o, e], Stroke::new(w(Handle::Axis(i)), col));
        let d = e - o;
        if d.length_sq() > 1.0 {
            let d = d.normalized();
            let n = Vec2::new(-d.y, d.x) * 5.0;
            p.add(Shape::convex_polygon(
                vec![e + d * 10.0, e + n, e - n],
                col,
                Stroke::NONE,
            ));
        }
    }
    if let Some(o) = cam.to_screen(g.origin) {
        p.circle_filled(o, 4.0, ov.plate);
        p.circle_stroke(o, 4.0, Stroke::new(overlay::THIN_PX, ov.ink_dim));
    }
}

/// Selection box: left to right is a window (solid), right to left is a crossing (dashed).
pub fn rubber(p: &Painter, from: Pos2, to: Pos2, ov: &Palette) {
    overlay::rubber_band(p, Rect::from_two_pos(from, to), to.x < from.x, ov);
}

/// Work-plane grid around a point and the plane frame.
pub fn plane_grid(
    p: &Painter,
    cam: &Cam,
    wp: &super::plane::WorkPlane,
    c: Vec3,
    step: f32,
    col: Color32,
) {
    let s = Stroke::new(0.6, col.gamma_multiply(0.35));
    for [a, b] in wp.grid_lines(c, step, 10) {
        seg(p, cam, a, b, s);
    }
}

/// Value input field at the cursor.
pub fn input_box(p: &Painter, at: Pos2, text: &str, ov: &Palette) -> Rect {
    let t = format!("{text}▏");
    label(
        p,
        at + Vec2::new(18.0, -18.0),
        &t,
        ov.ink,
        ov.plate,
        Some(ov.selection),
    )
}

/// Body outline with a cross at the cursor for an invalid state.
pub fn bad(p: &Painter, at: Pos2, ov: &Palette) {
    let s = Stroke::new(2.0, ov.error);
    p.line_segment([at + Vec2::new(8.0, -14.0), at + Vec2::new(16.0, -6.0)], s);
    p.line_segment([at + Vec2::new(16.0, -14.0), at + Vec2::new(8.0, -6.0)], s);
}
