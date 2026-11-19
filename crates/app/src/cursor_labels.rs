// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Labels that follow the cursor on a canvas (typed input, reading chip, tool label, snap name,
//! hover tip). One pure layout fn places them as a stack beside the cursor, flipped inward near
//! the canvas edges, so they never overlap each other and are never clipped.

use crate::overlay::{self, Palette, Plate};
use ddc_ui::theme::space;
use egui::{Painter, Pos2, Rect, Vec2};

/// Distance from the cursor to the nearest corner of the stack (clears the cursor glyph).
pub const CURSOR_OFFSET: f32 = space::MD + space::XXS;
/// Free margin kept between a label and the canvas edge.
pub const EDGE_MARGIN: f32 = space::XS;
/// Vertical gap between stacked labels.
pub const ROW_GAP: f32 = space::XXS;

/// Stack order, highest first: the most important label sits nearest to the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Prio {
    Input,
    Chip,
    Tool,
    Snap,
    Tip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Dim(Plate),
    Snap,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub prio: Prio,
    pub style: Style,
    pub text: String,
}

/// Per-frame collector of cursor labels.
#[derive(Default)]
pub struct CursorLabels {
    items: Vec<Item>,
}

impl CursorLabels {
    pub fn push(&mut self, prio: Prio, style: Style, text: String) {
        self.items.push(Item { prio, style, text });
    }

    /// Drops hover tips: while a tool is active they only crowd the tool's own labels.
    pub fn drop_tips(&mut self) {
        self.items.retain(|i| i.prio != Prio::Tip);
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Items by priority; equal priorities keep the order they were pushed in.
    pub fn ordered(mut self) -> Vec<Item> {
        self.items.sort_by_key(|i| i.prio);
        self.items
    }

    /// Lays the labels out around `cursor` inside `canvas` and paints them.
    pub fn paint(self, p: &Painter, cursor: Pos2, canvas: Rect, ov: &Palette) {
        let items = self.ordered();
        if items.is_empty() {
            return;
        }
        let sizes: Vec<Vec2> = items
            .iter()
            .map(|i| match i.style {
                Style::Dim(_) => overlay::dim_label_size(p, &i.text),
                Style::Snap => overlay::snap_label_size(p, &i.text),
            })
            .collect();
        let rects = stack(cursor, &sizes, canvas);
        let rested = overlay::cursor_rested(p.ctx(), "cursor", cursor);
        for (item, r) in items.into_iter().zip(rects) {
            if item.style == Style::Snap && !rested {
                continue;
            }
            match item.style {
                Style::Dim(kind) => {
                    overlay::label_at(p, r.min, item.text, kind, ov);
                }
                Style::Snap => {
                    overlay::snap_label_at(p, r.min, item.text, ov);
                }
            }
        }
    }
}

/// Rectangles for labels of `sizes` (in priority order, first = nearest to the cursor), as a
/// column to the lower right of `cursor`. The column flips left and/or up where the side it
/// starts on has less room than it needs, then slides inside `canvas` (minus `EDGE_MARGIN`).
/// A label or column larger than the canvas is pinned to the top-left corner. Result order
/// matches `sizes`.
pub fn stack(cursor: Pos2, sizes: &[Vec2], canvas: Rect) -> Vec<Rect> {
    if sizes.is_empty() {
        return Vec::new();
    }
    let cursor = if cursor.x.is_finite() && cursor.y.is_finite() {
        cursor
    } else {
        canvas.center()
    };
    let (cw, ch) = (canvas.width().max(0.0), canvas.height().max(0.0));
    let m = EDGE_MARGIN.min(cw / 2.0).min(ch / 2.0);
    let lo = canvas.min + Vec2::splat(m);
    let hi = Pos2::new(lo.x + (cw - 2.0 * m), lo.y + (ch - 2.0 * m));

    let total_w = sizes.iter().map(|s| s.x).fold(0.0, f32::max);
    let total_h = sizes.iter().map(|s| s.y).sum::<f32>() + ROW_GAP * (sizes.len() - 1) as f32;

    let room_r = hi.x - (cursor.x + CURSOR_OFFSET);
    let room_l = (cursor.x - CURSOR_OFFSET) - lo.x;
    let flip_x = room_r < total_w && room_l > room_r;
    let room_d = hi.y - (cursor.y + CURSOR_OFFSET);
    let room_u = (cursor.y - CURSOR_OFFSET) - lo.y;
    let flip_y = room_d < total_h && room_u > room_d;

    let top = if flip_y {
        cursor.y - CURSOR_OFFSET - total_h
    } else {
        cursor.y + CURSOR_OFFSET
    };
    let top = slide(top, total_h, lo.y, hi.y);

    // Row offsets from the top of the column, in slot order.
    let mut row_y = Vec::with_capacity(sizes.len());
    let mut y = 0.0;
    for s in sizes {
        row_y.push(y);
        y += s.y + ROW_GAP;
    }
    sizes
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let off = if flip_y {
                // Flipped up: the first label takes the bottom row, nearest the cursor.
                let below: f32 = sizes[..i].iter().map(|b| b.y + ROW_GAP).sum();
                total_h - below - s.y
            } else {
                row_y[i]
            };
            let x = if flip_x {
                cursor.x - CURSOR_OFFSET - s.x
            } else {
                cursor.x + CURSOR_OFFSET
            };
            let x = slide(x, s.x, lo.x, hi.x);
            Rect::from_min_size(Pos2::new(x, top + off), *s)
        })
        .collect()
}

/// Moves a span of `len` starting at `at` into `[lo, hi]`; a span that does not fit sticks to `lo`.
fn slide(at: f32, len: f32, lo: f32, hi: f32) -> f32 {
    if len >= hi - lo {
        lo
    } else {
        at.clamp(lo, hi - len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    fn canvas() -> Rect {
        Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 600.0))
    }

    fn inside(r: Rect, c: Rect) -> bool {
        r.min.x >= c.min.x + EDGE_MARGIN - 1e-3
            && r.min.y >= c.min.y + EDGE_MARGIN - 1e-3
            && r.max.x <= c.max.x - EDGE_MARGIN + 1e-3
            && r.max.y <= c.max.y - EDGE_MARGIN + 1e-3
    }

    fn disjoint(rs: &[Rect]) -> bool {
        for i in 0..rs.len() {
            for j in i + 1..rs.len() {
                if rs[i].intersects(rs[j]) {
                    return false;
                }
            }
        }
        true
    }

    #[test]
    fn single_label_inside_sits_lower_right() {
        let c = pos2(400.0, 300.0);
        let r = stack(c, &[vec2(80.0, 20.0)], canvas())[0];
        assert_eq!(r.min, pos2(c.x + CURSOR_OFFSET, c.y + CURSOR_OFFSET));
        assert_eq!(r.size(), vec2(80.0, 20.0));
    }

    #[test]
    fn right_edge_flips_left() {
        let cv = canvas();
        let c = pos2(cv.max.x - 10.0, 300.0);
        let r = stack(c, &[vec2(80.0, 20.0)], cv)[0];
        assert!(r.max.x <= c.x - CURSOR_OFFSET + 1e-3, "{r:?}");
        assert!(inside(r, cv));
    }

    #[test]
    fn bottom_edge_flips_up() {
        let cv = canvas();
        let c = pos2(400.0, cv.max.y - 6.0);
        let r = stack(c, &[vec2(80.0, 20.0)], cv)[0];
        assert!(r.max.y <= c.y - CURSOR_OFFSET + 1e-3, "{r:?}");
        assert!(inside(r, cv));
    }

    #[test]
    fn left_and_top_edges_stay_inside() {
        let cv = canvas();
        for c in [pos2(cv.min.x, 300.0), pos2(400.0, cv.min.y), cv.min] {
            let r = stack(c, &[vec2(80.0, 20.0)], cv)[0];
            assert!(inside(r, cv), "{c:?} -> {r:?}");
        }
    }

    #[test]
    fn corners_flip_inward() {
        let cv = canvas();
        let corners = [
            cv.min,
            pos2(cv.max.x, cv.min.y),
            pos2(cv.min.x, cv.max.y),
            cv.max,
        ];
        for c in corners {
            let rs = stack(c, &[vec2(120.0, 20.0), vec2(90.0, 20.0)], cv);
            assert!(rs.iter().all(|r| inside(*r, cv)), "{c:?} -> {rs:?}");
            assert!(disjoint(&rs));
        }
        let br = stack(cv.max, &[vec2(120.0, 20.0)], cv)[0];
        assert!(br.max.x < cv.max.x && br.max.y < cv.max.y);
        assert!(br.max.x <= cv.max.x - CURSOR_OFFSET + 1e-3);
    }

    #[test]
    fn two_and_three_labels_do_not_overlap() {
        let cv = canvas();
        let sizes = [vec2(100.0, 20.0), vec2(60.0, 18.0), vec2(140.0, 22.0)];
        let cursors = [
            pos2(400.0, 300.0),
            pos2(cv.max.x - 3.0, 300.0),
            pos2(400.0, cv.max.y - 3.0),
            cv.max,
            cv.min,
            pos2(cv.max.x, cv.min.y),
        ];
        for c in cursors {
            for n in 2..=3 {
                let rs = stack(c, &sizes[..n], cv);
                assert_eq!(rs.len(), n);
                assert!(disjoint(&rs), "{c:?} n={n}: {rs:?}");
                assert!(rs.iter().all(|r| inside(*r, cv)), "{c:?} n={n}: {rs:?}");
            }
        }
    }

    #[test]
    fn first_label_is_nearest_the_cursor() {
        let cv = canvas();
        let sizes = [vec2(100.0, 20.0), vec2(60.0, 18.0)];
        let down = stack(pos2(400.0, 300.0), &sizes, cv);
        assert!(down[0].min.y < down[1].min.y);
        let up = stack(pos2(400.0, cv.max.y - 3.0), &sizes, cv);
        assert!(up[0].min.y > up[1].min.y);
    }

    #[test]
    fn oversized_label_pins_to_canvas_corner() {
        let cv = Rect::from_min_size(pos2(10.0, 10.0), vec2(50.0, 40.0));
        let rs = stack(pos2(30.0, 30.0), &[vec2(200.0, 100.0)], cv);
        assert_eq!(rs[0].min, pos2(10.0 + EDGE_MARGIN, 10.0 + EDGE_MARGIN));
        assert_eq!(rs[0].size(), vec2(200.0, 100.0));
    }

    #[test]
    fn zero_size_canvas_does_not_panic() {
        let cv = Rect::from_min_size(pos2(5.0, 7.0), Vec2::ZERO);
        let rs = stack(pos2(5.0, 7.0), &[vec2(30.0, 10.0), vec2(20.0, 10.0)], cv);
        assert_eq!(rs.len(), 2);
        assert!(rs.iter().all(|r| r.min.x.is_finite() && r.min.y.is_finite()));
        assert_eq!(rs[0].min, cv.min);
        assert!(stack(pos2(0.0, 0.0), &[], cv).is_empty());
    }

    #[test]
    fn non_finite_cursor_falls_back_to_center() {
        let cv = canvas();
        let rs = stack(pos2(f32::NAN, 10.0), &[vec2(50.0, 20.0)], cv);
        assert!(inside(rs[0], cv));
    }

    #[test]
    fn layout_is_deterministic() {
        let cv = canvas();
        let sizes = [vec2(100.0, 20.0), vec2(60.0, 18.0), vec2(30.0, 18.0)];
        let c = pos2(cv.max.x - 20.0, cv.max.y - 20.0);
        assert_eq!(stack(c, &sizes, cv), stack(c, &sizes, cv));
    }

    #[test]
    fn collector_orders_by_priority_then_push_order() {
        let mut cl = CursorLabels::default();
        cl.push(Prio::Tip, Style::Dim(Plate::Normal), "tip".into());
        cl.push(Prio::Snap, Style::Snap, "snap".into());
        cl.push(Prio::Input, Style::Dim(Plate::Editing), "in".into());
        cl.push(Prio::Snap, Style::Snap, "snap2".into());
        cl.push(Prio::Chip, Style::Dim(Plate::Temp), "chip".into());
        let order: Vec<String> = cl.ordered().into_iter().map(|i| i.text).collect();
        assert_eq!(order, ["in", "chip", "snap", "snap2", "tip"]);
    }

    #[test]
    fn drop_tips_keeps_tool_labels() {
        let mut cl = CursorLabels::default();
        cl.push(Prio::Tip, Style::Dim(Plate::Normal), "tip".into());
        cl.push(Prio::Snap, Style::Snap, "snap".into());
        cl.drop_tips();
        let order: Vec<String> = cl.ordered().into_iter().map(|i| i.text).collect();
        assert_eq!(order, ["snap"]);
    }
}
