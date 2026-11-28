// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Unified canvas language (plan and 3D, both themes): overlay palette from tokens and primitives
//! with a constant size in screen pixels: handles, snap glyphs, dimension labels and lines,
//! selection box, tool ghost, material hatches, axis marks, AI preview. Values and meaning:
//! docs/design/VISUAL_LANGUAGE.md. No per-frame allocations except the outlines that
//! `Painter` itself takes.

use std::sync::OnceLock;

use ddc_ui::theme::{Mode, Tokens as DesignTokens, W_SEMIBOLD};
use egui::{
    self, Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2,
};

// Sizes in logical pixels; independent of view scale (`tokens.json`, `viewport`).
pub const GRIP_PX: f32 = 8.0;
pub const GRIP_HOT_PX: f32 = 10.0;
/// Snap type caption appears after the cursor rests this long.
pub const SNAP_LABEL_DELAY_MS: u64 = 300;
/// Soft halo around a selected outline, on the 4 px grid.
pub const HALO_PX: f32 = 4.0;
pub const GRIP_STROKE: f32 = 1.5;
/// Handle hover and grab radius in plan: highlight and grab stay in agreement.
pub const GRIP_HIT_PX: f32 = 12.0;
/// Side of the grab square of a section-box handle in 3D.
pub const BOX_HANDLE_PX: f32 = 18.0;
pub const SNAP_PX: f32 = 12.0;
pub const SNAP_STROKE: f32 = 1.5;
pub const OUTLINE_PX: f32 = 2.0;
pub const THIN_PX: f32 = 1.0;
pub const GHOST_PX: f32 = 1.5;
pub const GHOST_DASH: (f32, f32) = (6.0, 4.0);
pub const CROSSING_DASH: (f32, f32) = (4.0, 3.0);
pub const TRACE_DASH: (f32, f32) = (2.0, 3.0);
pub const BUBBLE_R: f32 = 11.0;
pub const PLATE_PAD: Vec2 = Vec2::new(8.0, 4.0);
const PLATE_RADIUS: f32 = ddc_ui::theme::radius::SM;
/// Paper plate under rotated-free labels: near-opaque so the grid never shows through.
const PLATE_ALPHA: f32 = 0.92;
const TICK_PX: f32 = 4.0;
/// Limit of hatch lines and dots per outline.
const HATCH_MAX: u32 = 8000;
/// Limit of strokes and dots per plan frame.
pub const HATCH_FRAME_MAX: u32 = 20_000;
/// More walls in the frame than this: no hatching, otherwise the frame budget would cut it off at an arbitrary wall.
pub const HATCH_WALLS_MAX: usize = 2000;
/// An outline thinner than this on screen is not hatched: the lines would merge into a blob.
const HATCH_MIN_PX: f32 = 3.0;
/// Sparsest dot step, used when the outline does not fit the budget.
const DOTS_STEP_MAX: f32 = 96.0;

/// Kind of change in the AI preview (`docs/design/AI_ASSISTANT.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AiKind {
    Created,
    Changed,
    Deleted,
}

impl AiKind {
    /// Legend sign.
    pub const fn sign(self) -> &'static str {
        match self {
            AiKind::Created => "+",
            AiKind::Changed => "~",
            AiKind::Deleted => "−",
        }
    }
}

/// Canvas colors of one theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub selection: Color32,
    pub selection_fill: Color32,
    pub hover: Color32,
    pub preview: Color32,
    pub snap: Color32,
    pub error: Color32,
    pub warning: Color32,
    pub success: Color32,
    pub ai_created: Color32,
    pub ai_changed: Color32,
    pub ai_deleted: Color32,
    /// X, Y, Z axes: gizmo, origin, cube.
    pub axis: [Color32; 3],
    pub refplane: Color32,
    pub paper: Color32,
    /// Label chips and handle fill.
    pub plate: Color32,
    /// Canvas chip and label plates (`bg.canvas_chip`, already translucent).
    pub chip: Color32,
    /// Grip body (`bg.elevated`).
    pub elevated: Color32,
    pub plate_border: Color32,
    pub ink: Color32,
    pub ink_dim: Color32,
}

fn build(t: &DesignTokens, mode: Mode, dark: bool) -> Palette {
    let c = |name: &str| {
        t.color(mode, name)
            .unwrap_or_else(|_| panic!("canvas token {name} is missing"))
    };
    Palette {
        dark,
        selection: c("selection"),
        selection_fill: c("selection.fill"),
        hover: c("hover"),
        preview: c("preview"),
        snap: c("snap"),
        error: c("error"),
        warning: c("warning"),
        success: c("success"),
        ai_created: c(ddc_ui::diff_kind::DiffKind::Added.token()),
        ai_changed: c(ddc_ui::diff_kind::DiffKind::Changed.token()),
        ai_deleted: c(ddc_ui::diff_kind::DiffKind::Deleted.token()),
        axis: [c("axis.x"), c("axis.y"), c("axis.z")],
        refplane: c("canvas.ref"),
        paper: c("canvas.paper"),
        plate: c("bg.panel"),
        chip: c("bg.canvas_chip"),
        elevated: c("bg.elevated"),
        plate_border: c("border.subtle"),
        ink: c("text.primary"),
        ink_dim: c("text.secondary"),
    }
}

/// Theme palette; built once, afterwards a static read.
pub fn pal(dark: bool) -> &'static Palette {
    static PALS: OnceLock<[Palette; 2]> = OnceLock::new();
    let p = PALS.get_or_init(|| {
        let t = DesignTokens::embedded().expect("embedded tokens.json is validated by tests");
        [build(&t, Mode::Light, false), build(&t, Mode::Dark, true)]
    });
    &p[usize::from(dark)]
}

impl crate::ui_kit::Tokens {
    /// Overlay palette of the theme for these tokens.
    pub fn ov(&self) -> &'static Palette {
        pal(self.paper.r() < 128)
    }
}

impl Palette {
    /// Element line by state: selection outranks hover; both are `OUTLINE_PX` or thicker than the base line.
    pub fn state(&self, sel: bool, hot: bool, base: Stroke) -> Stroke {
        if sel {
            Stroke::new(base.width.max(OUTLINE_PX), self.selection)
        } else if hot {
            Stroke::new(base.width.max(OUTLINE_PX), self.hover)
        } else {
            base
        }
    }
    /// Fill over the element color: selection only; hover does not change the fill.
    /// Wide soft stroke drawn under a selected outline.
    pub fn halo(&self, base: Stroke) -> Stroke {
        Stroke::new(base.width.max(OUTLINE_PX) + 2.0 * HALO_PX, self.selection_fill)
    }

    pub fn state_fill(&self, sel: bool, base: Color32) -> Color32 {
        if sel {
            mix(base, self.selection, 0.4)
        } else {
            base
        }
    }
    /// Tool ghost line.
    pub fn ghost(&self) -> Stroke {
        Stroke::new(GHOST_PX, self.preview.gamma_multiply(0.9))
    }
    pub fn ghost_fill(&self) -> Color32 {
        self.preview.gamma_multiply(0.18)
    }
    pub fn error_fill(&self) -> Color32 {
        self.error.gamma_multiply(0.18)
    }
    /// Fill of the temporary-mode chip: paper-colored text stays readable in both themes (≥ 4.5).
    pub fn banner_fill(&self) -> Color32 {
        mix(self.snap, self.ink, 0.2)
    }
    pub fn ai(&self, k: AiKind) -> Color32 {
        match k {
            AiKind::Created => self.ai_created,
            AiKind::Changed => self.ai_changed,
            AiKind::Deleted => self.ai_deleted,
        }
    }
    /// Preview mesh color for `scene::tag_rgb` (sRGB).
    pub fn ai_rgb(&self, k: AiKind) -> [u8; 3] {
        rgb(self.ai(k))
    }
}

pub fn rgb(c: Color32) -> [u8; 3] {
    [c.r(), c.g(), c.b()]
}

/// Opaque blend: `k` = 0 gives `a`, 1 gives `b`.
pub fn mix(a: Color32, b: Color32, k: f32) -> Color32 {
    let m = |x: u8, y: u8| (f32::from(x) * (1.0 - k) + f32::from(y) * k).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

// ---- handles ----

/// Handle shape: square is a point; diamond is an edge and parameter; circle is move; ring is rotate;
/// triangle is height; plus is insert; arrows are movement along a direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grip {
    Square,
    Diamond,
    Circle,
    Ring,
    Triangle,
    Plus,
    Arrows(Vec2),
}

/// Handle in the selection color; when hovered it is filled and slightly larger. Size is independent of view scale.
pub fn grip(p: &Painter, at: Pos2, g: Grip, hot: bool, ov: &Palette) {
    grip_col(p, at, g, hot, ov.selection, ov);
}

pub fn grip_col(p: &Painter, at: Pos2, g: Grip, hot: bool, col: Color32, ov: &Palette) {
    let (size, fill, mark) = if hot {
        (GRIP_HOT_PX, col, ov.elevated)
    } else {
        (GRIP_PX, ov.elevated, col)
    };
    let h = size / 2.0;
    let st = Stroke::new(GRIP_STROKE, col);
    match g {
        Grip::Square => {
            let r = Rect::from_center_size(at, Vec2::splat(size));
            p.rect_filled(r, 1.0, fill);
            p.rect_stroke(r, 1.0, st, StrokeKind::Inside);
        }
        Grip::Diamond => {
            let k = h * 1.15;
            p.add(Shape::convex_polygon(
                vec![
                    at + Vec2::new(0.0, -k),
                    at + Vec2::new(k, 0.0),
                    at + Vec2::new(0.0, k),
                    at + Vec2::new(-k, 0.0),
                ],
                fill,
                st,
            ));
        }
        Grip::Circle => {
            p.circle(at, h, fill, st);
        }
        Grip::Ring => {
            p.circle(at, h, fill, st);
            p.circle_filled(at, 1.75, mark);
        }
        Grip::Triangle => {
            let k = h * 1.2;
            p.add(Shape::convex_polygon(
                vec![
                    at + Vec2::new(0.0, -k),
                    at + Vec2::new(k, k * 0.8),
                    at + Vec2::new(-k, k * 0.8),
                ],
                fill,
                st,
            ));
        }
        Grip::Plus => {
            p.circle(at, h, fill, st);
            let m = Stroke::new(GRIP_STROKE, mark);
            p.line_segment(
                [at - Vec2::new(h * 0.55, 0.0), at + Vec2::new(h * 0.55, 0.0)],
                m,
            );
            p.line_segment(
                [at - Vec2::new(0.0, h * 0.55), at + Vec2::new(0.0, h * 0.55)],
                m,
            );
        }
        Grip::Arrows(dir) => {
            let d = if dir.length_sq() > 1e-6 {
                dir.normalized()
            } else {
                Vec2::new(0.0, -1.0)
            };
            let n = Vec2::new(-d.y, d.x);
            let k = h * 1.4;
            for sign in [1.0f32, -1.0] {
                let tip = at + d * sign * (k + 3.0);
                let base = at + d * sign * 2.0;
                p.add(Shape::convex_polygon(
                    vec![tip, base + n * k * 0.7, base - n * k * 0.7],
                    fill,
                    st,
                ));
            }
        }
    }
}

// ---- snaps ----

/// Snap glyph: the shape is distinguishable without color (VISUAL_LANGUAGE, "Snaps").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapGlyph {
    /// Endpoint, vertex.
    Square,
    /// Midpoint.
    Triangle,
    /// Intersection.
    Cross,
    /// Perpendicular.
    Perp,
    /// On a wall axis or face, on a reference line.
    Diamond,
    /// Center, body face, corner.
    Circle,
    /// Nearest point on a line.
    Hourglass,
    /// Grid, work plane.
    Plus,
    /// Wall face: flat chip.
    Slab,
    /// Grid line: diamond with a dot.
    DiamondDot,
    /// Reference plane: dashed vertical.
    Dashes,
    /// Alignment with a distant point: two parallels.
    Track,
    /// Trace of the work plane: a slanted dashed stroke.
    Slant,
    /// Point of a circle at 0/90/180/270: circle with four ticks.
    Quadrant,
    /// Extension of a wall: a dotted line through the point.
    Guide,
    /// Parallel to a wall: two short slanted strokes.
    Parallel,
}

pub fn snap_glyph(p: &Painter, at: Pos2, g: SnapGlyph, ov: &Palette) {
    let s = Stroke::new(SNAP_STROKE, ov.snap);
    let r = SNAP_PX / 2.0;
    let v = |x: f32, y: f32| at + Vec2::new(x * r, y * r);
    match g {
        SnapGlyph::Square => {
            p.rect_stroke(
                Rect::from_center_size(at, Vec2::splat(SNAP_PX)),
                0.0,
                s,
                StrokeKind::Middle,
            );
        }
        SnapGlyph::Triangle => {
            p.add(Shape::closed_line(
                vec![v(0.0, -1.0), v(1.0, 0.8), v(-1.0, 0.8)],
                s,
            ));
        }
        SnapGlyph::Cross => {
            p.line_segment([v(-1.0, -1.0), v(1.0, 1.0)], s);
            p.line_segment([v(-1.0, 1.0), v(1.0, -1.0)], s);
        }
        SnapGlyph::Perp => {
            p.line_segment([v(-1.0, 0.85), v(1.0, 0.85)], s);
            p.line_segment([v(0.0, 0.85), v(0.0, -1.0)], s);
        }
        SnapGlyph::Diamond => {
            p.add(Shape::closed_line(
                vec![v(0.0, -1.0), v(1.0, 0.0), v(0.0, 1.0), v(-1.0, 0.0)],
                s,
            ));
        }
        SnapGlyph::Circle => {
            p.circle_stroke(at, r, s);
        }
        SnapGlyph::Quadrant => {
            p.circle_stroke(at, r * 0.6, s);
            for (a, b) in [((0.6, 0.0), (1.0, 0.0)), ((-0.6, 0.0), (-1.0, 0.0)), ((0.0, 0.6), (0.0, 1.0)), ((0.0, -0.6), (0.0, -1.0))] {
                p.line_segment([v(a.0, a.1), v(b.0, b.1)], s);
            }
        }
        SnapGlyph::Guide => {
            for x in [-1.0, -0.35, 0.35] {
                p.line_segment([v(x, 0.0), v(x + 0.3, 0.0)], s);
            }
            p.circle_filled(at, SNAP_STROKE, ov.snap);
        }
        SnapGlyph::Parallel => {
            p.line_segment([v(-1.0, 0.2), v(0.2, -1.0)], s);
            p.line_segment([v(-0.2, 1.0), v(1.0, -0.2)], s);
        }
        SnapGlyph::Hourglass => {
            p.add(Shape::closed_line(
                vec![v(-1.0, -1.0), v(1.0, -1.0), v(-1.0, 1.0), v(1.0, 1.0)],
                s,
            ));
        }
        SnapGlyph::Plus => {
            p.line_segment([v(-1.0, 0.0), v(1.0, 0.0)], s);
            p.line_segment([v(0.0, -1.0), v(0.0, 1.0)], s);
        }
        SnapGlyph::Slab => {
            p.rect_stroke(
                Rect::from_center_size(at, Vec2::new(SNAP_PX, SNAP_PX * 0.5)),
                0.0,
                s,
                StrokeKind::Middle,
            );
        }
        SnapGlyph::DiamondDot => {
            p.add(Shape::closed_line(
                vec![v(0.0, -1.0), v(1.0, 0.0), v(0.0, 1.0), v(-1.0, 0.0)],
                s,
            ));
            p.circle_filled(at, SNAP_STROKE, ov.snap);
        }
        SnapGlyph::Dashes => {
            p.line_segment([v(0.0, -1.0), v(0.0, -0.3)], s);
            p.line_segment([v(0.0, 0.3), v(0.0, 1.0)], s);
        }
        SnapGlyph::Track => {
            p.line_segment([v(-1.0, -0.45), v(1.0, -0.45)], s);
            p.line_segment([v(-1.0, 0.45), v(1.0, 0.45)], s);
        }
        SnapGlyph::Slant => {
            p.line_segment([v(-1.0, 1.0), v(-0.3, 0.3)], s);
            p.line_segment([v(0.3, -0.3), v(1.0, -1.0)], s);
        }
    }
}

/// Snap label offset by (+14, +14) px: primary text on a chip framed in the snap color
/// (the snap color as text color falls below 4.5 contrast in the light theme).
pub fn snap_label(p: &Painter, at: Pos2, text: String, ov: &Palette) -> Rect {
    if !cursor_rested(p.ctx(), "marker", at) {
        return Rect::from_min_size(at, Vec2::ZERO);
    }
    let size = snap_label_size(p, &text);
    let r = crate::cursor_labels::stack(at, &[size], p.clip_rect())[0];
    snap_label_at(p, r.min, text, ov)
}

/// Plate around text of size `text`: padding 4x8, never lower than a chip (24).
pub fn plate_size(text: Vec2) -> Vec2 {
    let s = text + PLATE_PAD * 2.0;
    Vec2::new(s.x, s.y.max(crate::draw_chip::CHIP_H))
}

fn text_origin(plate: Rect, text: Vec2) -> Pos2 {
    plate.min + Vec2::new(PLATE_PAD.x, (plate.height() - text.y) / 2.0)
}

/// One step of the rest timer: `(position, since)` and whether `pos` has been still for
/// `SNAP_LABEL_DELAY_MS`.
pub fn rest_step(prev: Option<(Pos2, f64)>, pos: Pos2, now: f64) -> ((Pos2, f64), bool) {
    let since = match prev {
        Some((p, t)) if p.distance(pos) < 1.0 => t,
        _ => now,
    };
    ((pos, since), now - since >= SNAP_LABEL_DELAY_MS as f64 / 1000.0)
}

/// True once the cursor has rested at `pos`; asks for a repaint when the delay ends.
pub fn cursor_rested(ctx: &egui::Context, salt: &str, pos: Pos2) -> bool {
    let id = egui::Id::new(("ddc.snap.rest", salt));
    let now = ctx.input(|i| i.time);
    let prev = ctx.data(|d| d.get_temp::<(Pos2, f64)>(id));
    let (state, ready) = rest_step(prev, pos, now);
    ctx.data_mut(|d| d.insert_temp(id, state));
    if !ready {
        ctx.request_repaint_after(std::time::Duration::from_millis(SNAP_LABEL_DELAY_MS));
    }
    ready
}

/// Plate size of a snap label.
pub fn snap_label_size(p: &Painter, text: &str) -> Vec2 {
    let g = p.layout_no_wrap(
        text.to_owned(),
        FontId::proportional(ddc_ui::theme::type_size::CAPTION),
        Color32::PLACEHOLDER,
    );
    plate_size(g.size())
}

/// Plate size of a dimension label.
pub fn dim_label_size(p: &Painter, text: &str) -> Vec2 {
    let g = p.layout_no_wrap(
        text.to_owned(),
        FontId::proportional(ddc_ui::theme::type_size::LABEL),
        Color32::PLACEHOLDER,
    );
    plate_size(g.size())
}

/// Snap label plate with its top-left corner at `left_top`.
pub fn snap_label_at(p: &Painter, left_top: Pos2, text: String, ov: &Palette) -> Rect {
    let g = p.layout_no_wrap(
        text,
        FontId::proportional(ddc_ui::theme::type_size::CAPTION),
        ov.ink,
    );
    let r = Rect::from_min_size(left_top, plate_size(g.size()));
    p.rect_filled(r, PLATE_RADIUS, ov.chip);
    p.rect_stroke(
        r,
        PLATE_RADIUS,
        Stroke::new(THIN_PX, ov.snap),
        StrokeKind::Inside,
    );
    p.galley(text_origin(r, g.size()), g, ov.ink);
    r
}

/// Temporary guides of captured points: dashed vertical and horizontal rays across the view.
pub fn guide_rays(p: &Painter, at: Pos2, ov: &Palette) {
    let r = p.clip_rect();
    snap_trace(p, Pos2::new(at.x, r.top()), Pos2::new(at.x, r.bottom()), ov);
    snap_trace(p, Pos2::new(r.left(), at.y), Pos2::new(r.right(), at.y), ov);
    snap_glyph(p, at, SnapGlyph::Plus, ov);
}

/// Snap tracking line: 2/3 dashes in the snap color @ 60 %.
pub fn snap_trace(p: &Painter, a: Pos2, b: Pos2, ov: &Palette) {
    let st = Stroke::new(THIN_PX, ov.snap.gamma_multiply(0.6));
    p.extend(Shape::dashed_line(&[a, b], st, TRACE_DASH.0, TRACE_DASH.1));
}

// ---- labels and dimensions ----

/// Dimension label state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plate {
    /// Value: `border.subtle` frame.
    Normal,
    /// Temporary dimension at the tool: frame in the preview color.
    Temp,
    /// Being edited: accent frame.
    Editing,
    Error,
}

/// Dimension label centered at `center`: numeric 12, bg.canvas_chip chip, radius 4, padding 4×8.
pub fn dim_label(p: &Painter, center: Pos2, text: String, kind: Plate, ov: &Palette) -> Rect {
    let (ink, border) = match kind {
        Plate::Normal => (ov.ink, ov.plate_border),
        Plate::Temp => (ov.ink, ov.preview),
        Plate::Editing => (ov.ink, ov.selection),
        Plate::Error => (ov.error, ov.error),
    };
    let g = p.layout_no_wrap(
        text,
        FontId::proportional(ddc_ui::theme::type_size::LABEL),
        ink,
    );
    let r = Rect::from_center_size(center, plate_size(g.size()));
    p.rect_filled(r, PLATE_RADIUS, ov.chip);
    p.rect_stroke(
        r,
        PLATE_RADIUS,
        Stroke::new(THIN_PX, border),
        StrokeKind::Inside,
    );
    p.galley(text_origin(r, g.size()), g, ink);
    r
}

/// The same label anchored at the top-left corner (input line and label at the cursor).
pub fn label_at(p: &Painter, left_top: Pos2, text: String, kind: Plate, ov: &Palette) -> Rect {
    let probe = p.layout_no_wrap(
        text.clone(),
        FontId::proportional(ddc_ui::theme::type_size::LABEL),
        ov.ink,
    );
    let size = plate_size(probe.size());
    dim_label(p, left_top + size / 2.0, text, kind, ov)
}

/// Dimension line with 45° ticks (tick length is constant in pixels).
pub fn dim_line(p: &Painter, a: Pos2, b: Pos2, col: Color32) {
    let st = Stroke::new(THIN_PX, col);
    p.line_segment([a, b], st);
    let d = b - a;
    if d.length_sq() < 1e-6 {
        return;
    }
    let d = d.normalized();
    let tick = (d + Vec2::new(-d.y, d.x)) * (TICK_PX * std::f32::consts::FRAC_1_SQRT_2);
    let heavy = Stroke::new(GHOST_PX, col);
    p.line_segment([a - tick, a + tick], heavy);
    p.line_segment([b - tick, b + tick], heavy);
}

/// Text on a paper-colored chip so it reads over the grid and hatching (rotated text has no chip).
pub fn halo_text(
    p: &Painter,
    pos: Pos2,
    g: std::sync::Arc<egui::Galley>,
    ink: Color32,
    paper: Color32,
    angle: f32,
) {
    if angle.abs() < 1e-3 {
        let r = Rect::from_min_size(pos, g.size()).expand2(Vec2::new(3.0, 1.0));
        p.rect_filled(r, PLATE_RADIUS, paper.gamma_multiply(PLATE_ALPHA));
    }
    p.add(egui::epaint::TextShape::new(pos, g, ink).with_angle(angle));
}

/// Grid or level mark: paper-colored circle, thin outline, name in `ink`.
pub fn bubble(p: &Painter, c: Pos2, name: &str, st: Stroke, ink: Color32, ov: &Palette) {
    p.circle(c, BUBBLE_R, ov.paper, st);
    p.text(
        c,
        Align2::CENTER_CENTER,
        name,
        FontId::new(
            ddc_ui::theme::type_size::CAPTION,
            ddc_ui::theme::family(W_SEMIBOLD),
        ),
        ink,
    );
}

// ---- selection box and ghost ----

/// Selection box: "window" is solid in the selection color, "crossing" is 4/3 dashed in the preview color.
pub fn rubber_band(p: &Painter, r: Rect, crossing: bool, ov: &Palette) {
    let col = if crossing { ov.preview } else { ov.selection };
    p.rect_filled(r, 0.0, col.gamma_multiply(0.1));
    let st = Stroke::new(THIN_PX, col);
    if crossing {
        let c = [
            r.left_top(),
            r.right_top(),
            r.right_bottom(),
            r.left_bottom(),
            r.left_top(),
        ];
        p.extend(Shape::dashed_line(&c, st, CROSSING_DASH.0, CROSSING_DASH.1));
    } else {
        p.rect_stroke(r, 0.0, st, StrokeKind::Inside);
    }
}

/// Tool ghost segment: 6/4 dashes.
pub fn ghost_line(p: &Painter, a: Pos2, b: Pos2, st: Stroke) {
    p.extend(Shape::dashed_line(&[a, b], st, GHOST_DASH.0, GHOST_DASH.1));
}

/// Ghost outline: fill (convex outline) and dashes.
pub fn ghost_poly(p: &Painter, pts: Vec<Pos2>, ov: &Palette) {
    let st = ov.ghost();
    let mut ring = pts.clone();
    if let Some(&f) = pts.first() {
        ring.push(f);
    }
    p.add(Shape::convex_polygon(pts, ov.ghost_fill(), Stroke::NONE));
    p.extend(Shape::dashed_line(&ring, st, GHOST_DASH.0, GHOST_DASH.1));
}

/// Session preview of any view: the view's adapter made `draws` from the primitives;
/// here they become shapes. `len` formats a temporary dimension's value.
pub fn paint_preview(
    p: &Painter,
    draws: &[crate::interact::Draw],
    ov: &Palette,
    len: &dyn Fn(f64) -> String,
) {
    use crate::interact::{Draw, Glyph, Style};
    let pos = |s: [f32; 2]| Pos2::new(s[0], s[1]);
    let col = |st: Style| if st == Style::Invalid { ov.error } else { ov.preview };
    for d in draws {
        match d {
            Draw::Path { pts, closed, fill, style } => {
                let pts: Vec<Pos2> = pts.iter().map(|&s| pos(s)).collect();
                let st = Stroke::new(GHOST_PX, col(*style).gamma_multiply(0.9));
                if *fill && pts.len() >= 3 {
                    let f = if *style == Style::Invalid { ov.error_fill() } else { ov.ghost_fill() };
                    // a band or a cut may be concave: fill by triangulation
                    let mut m = egui::Mesh::default();
                    for &q in &pts {
                        m.colored_vertex(q, f);
                    }
                    for t in ddc_geom::poly::triangulate(
                        &pts.iter().map(|q| ddc_geom::v2(q.x as f64, q.y as f64)).collect::<Vec<_>>(),
                    ) {
                        m.add_triangle(t[0] as u32, t[1] as u32, t[2] as u32);
                    }
                    p.add(Shape::mesh(m));
                }
                let mut ring = pts.clone();
                if *closed {
                    if let Some(&f) = pts.first() {
                        ring.push(f);
                    }
                }
                p.extend(Shape::dashed_line(&ring, st, GHOST_DASH.0, GHOST_DASH.1));
            }
            Draw::Tris { pts, tris, style } => {
                let f = if *style == Style::Invalid { ov.error_fill() } else { ov.ghost_fill() };
                let mut m = egui::Mesh::default();
                for &q in pts {
                    m.colored_vertex(pos(q), f);
                }
                for t in tris {
                    m.add_triangle(t[0], t[1], t[2]);
                }
                p.add(Shape::mesh(m));
            }
            Draw::Glyph { at, glyph, style } => {
                let g = match glyph {
                    Glyph::Point => SnapGlyph::Square,
                    Glyph::Snap => SnapGlyph::Circle,
                    Glyph::Arrow => SnapGlyph::Triangle,
                };
                if *style == Style::Invalid {
                    p.circle_stroke(pos(*at), SNAP_PX * 0.5, Stroke::new(SNAP_STROKE, ov.error));
                } else {
                    snap_glyph(p, pos(*at), g, ov);
                }
            }
            Draw::Dim { a, b, value, editable, style } => {
                let (pa, pb) = (pos(*a), pos(*b));
                dim_line(p, pa, pb, col(*style));
                let kind = match (*style, *editable) {
                    (Style::Invalid, _) => Plate::Error,
                    (_, true) => Plate::Temp,
                    _ => Plate::Normal,
                };
                dim_label(p, pa + (pb - pa) * 0.5, len(*value), kind, ov);
            }
            Draw::Label { at, key, style } => {
                let kind = if *style == Style::Invalid { Plate::Error } else { Plate::Temp };
                label_at(p, pos(*at) + Vec2::new(8.0, 8.0), crate::tr(key, &[]), kind, ov);
            }
        }
    }
}

// ---- material hatches ----

/// Material fill pattern; the step is constant in pixels so it reads at any scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hatch {
    None,
    /// Masonry: 45° diagonal.
    Diagonal,
    /// Wood: reverse diagonal, sparser.
    BackDiagonal,
    /// Insulation: diagonal crosshatch.
    Cross,
    /// Concrete: dots in a staggered pattern.
    Dots,
    /// Metal: dense 45 degree lines.
    Steel,
    /// Soil: diagonal lines with dots between.
    Earth,
    /// Water: horizontal lines.
    Water,
    /// Stone and tile: a square grid.
    Stone,
    /// Render and plaster: sparse stipple.
    Stipple,
    /// Glass: widely spaced thin diagonals.
    Glass,
}

/// Hatch an outline (convex or concave: intersections taken in pairs) within `clip`.
/// `budget` is the remaining strokes and dots for the frame.
pub fn hatch(p: &Painter, poly: &[Pos2], kind: Hatch, st: Stroke, clip: Rect, budget: &mut u32) {
    if poly.len() < 3 || kind == Hatch::None || *budget == 0 {
        return;
    }
    if contour_width(poly) < HATCH_MIN_PX {
        return;
    }
    let (mut lo, mut hi) = (poly[0], poly[0]);
    for q in poly {
        lo = Pos2::new(lo.x.min(q.x), lo.y.min(q.y));
        hi = Pos2::new(hi.x.max(q.x), hi.y.max(q.y));
    }
    let bb = Rect::from_min_max(lo, hi).intersect(clip);
    if !bb.is_positive() {
        return;
    }
    match kind {
        Hatch::None => {}
        Hatch::Diagonal => family(p, poly, 5.0, -1.0, st, bb, budget),
        Hatch::BackDiagonal => family(p, poly, 7.0, 1.0, st, bb, budget),
        Hatch::Cross => {
            family(p, poly, 6.0, -1.0, st, bb, budget);
            family(p, poly, 6.0, 1.0, st, bb, budget);
        }
        Hatch::Steel => family(p, poly, 2.6, -1.0, st, bb, budget),
        Hatch::Glass => family(p, poly, 11.0, -1.0, st, bb, budget),
        Hatch::Water => family_ab(p, poly, 5.0, 0.0, 1.0, st, bb, budget),
        Hatch::Stone => {
            family_ab(p, poly, 7.0, 0.0, 1.0, st, bb, budget);
            family_ab(p, poly, 10.0, 1.0, 0.0, st, bb, budget);
        }
        Hatch::Earth => {
            family(p, poly, 6.0, -1.0, st, bb, budget);
            stipple(p, poly, 9.0, st, bb, budget);
        }
        Hatch::Stipple => stipple(p, poly, 8.0, st, bb, budget),
        Hatch::Dots => {
            let limit = (*budget).min(HATCH_MAX);
            let drawn = for_each_dot(poly, 6.0, bb, limit, |q| {
                p.circle_filled(q, 0.7, st.color);
            });
            *budget -= drawn;
        }
    }
}

/// Nominal outline thickness, 2·area / perimeter: for a long wall this is its thickness.
/// The shortest edge is unsuitable: the cut at a joint is short even for a thick wall.
fn contour_width(poly: &[Pos2]) -> f32 {
    let (mut twice_area, mut perimeter) = (0.0f32, 0.0f32);
    for i in 0..poly.len() {
        let (a, b) = (poly[i] - poly[0], poly[(i + 1) % poly.len()] - poly[0]);
        twice_area += a.x * b.y - b.x * a.y;
        perimeter += (b - a).length();
    }
    if perimeter > 0.0 {
        twice_area.abs() / perimeter
    } else {
        0.0
    }
}

fn stipple(p: &Painter, poly: &[Pos2], step: f32, st: Stroke, bb: Rect, budget: &mut u32) {
    let limit = (*budget).min(HATCH_MAX);
    let drawn = for_each_dot(poly, step, bb, limit, |q| {
        p.circle_filled(q, 0.6, st.color);
    });
    *budget -= drawn;
}

/// Family of lines x + sign·y = k with spacing `step`.
fn family(
    p: &Painter,
    poly: &[Pos2],
    step: f32,
    sign: f32,
    st: Stroke,
    bb: Rect,
    budget: &mut u32,
) {
    family_ab(p, poly, step, 1.0, sign, st, bb, budget);
}

/// Family of lines a·x + b·y = k with spacing `step` in k (horizontal: a = 0, b = 1).
#[allow(clippy::too_many_arguments)]
fn family_ab(
    p: &Painter,
    poly: &[Pos2],
    step: f32,
    a: f32,
    b: f32,
    st: Stroke,
    bb: Rect,
    budget: &mut u32,
) {
    let f = |q: Pos2| a * q.x + b * q.y;
    let (mut k0, mut k1) = (f32::MAX, f32::MIN);
    for q in poly {
        k0 = k0.min(f(*q));
        k1 = k1.max(f(*q));
    }
    // only lines that pass through the visible part of the outline
    let corners = [
        f(bb.left_top()),
        f(bb.right_top()),
        f(bb.right_bottom()),
        f(bb.left_bottom()),
    ];
    let (c0, c1) = corners
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), &k| (a.min(k), b.max(k)));
    let (k0, k1) = (k0.max(c0), k1.min(c1));
    let mut k = (k0 / step).ceil() * step;
    let mut n = 0;
    while k < k1 && n < HATCH_MAX && *budget > 0 {
        let mut hits = [Pos2::ZERO; 8];
        let mut c = 0usize;
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (fa, fb) = (f(a) - k, f(b) - k);
            if (fa < 0.0) != (fb < 0.0) && c < hits.len() {
                let q = a + (b - a) * (fa / (fa - fb));
                // insertion by x: pairs run along the line
                let mut j = c;
                while j > 0 && (hits[j - 1].x > q.x || (hits[j - 1].x == q.x && hits[j - 1].y > q.y)) {
                    hits[j] = hits[j - 1];
                    j -= 1;
                }
                hits[j] = q;
                c += 1;
            }
        }
        let mut j = 0;
        while j + 1 < c && *budget > 0 {
            p.line_segment([hits[j], hits[j + 1]], st);
            *budget -= 1;
            j += 2;
        }
        k += step;
        n += 1;
    }
}

/// Intersections of the outline with the horizontal `y`, ascending in x; pairs are the spans inside the outline.
fn row_crossings(poly: &[Pos2], y: f32, xs: &mut [f32; 16]) -> usize {
    let mut c = 0;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        if (a.y > y) != (b.y > y) && c < xs.len() {
            let x = a.x + (b.x - a.x) * (y - a.y) / (b.y - a.y);
            let mut j = c;
            while j > 0 && xs[j - 1] > x {
                xs[j] = xs[j - 1];
                j -= 1;
            }
            xs[j] = x;
            c += 1;
        }
    }
    c
}

/// Staggered dots only on spans inside the outline. An outline over `limit` gets a
/// sparser grid rather than a cutoff: the pattern covers the whole visible part. Returns the dot count.
fn for_each_dot(poly: &[Pos2], step: f32, bb: Rect, limit: u32, mut put: impl FnMut(Pos2)) -> u32 {
    let mut xs = [0.0f32; 16];
    let mut inner = 0.0f32;
    let mut y = (bb.top() / step).ceil() * step;
    while y < bb.bottom() {
        let c = row_crossings(poly, y, &mut xs);
        for pair in xs[..c].chunks_exact(2) {
            inner += (pair[1].min(bb.right()) - pair[0].max(bb.left())).max(0.0);
        }
        y += step;
    }
    let mut step = step;
    let mut estimate = inner / step;
    while estimate > limit as f32 && step < DOTS_STEP_MAX {
        step *= 2.0;
        estimate /= 4.0;
    }
    let mut n = 0;
    let mut y = (bb.top() / step).ceil() * step;
    while y < bb.bottom() && n < limit {
        let odd = (y / step).round() as i64 & 1 == 1;
        let shift = if odd { step / 2.0 } else { 0.0 };
        let c = row_crossings(poly, y, &mut xs);
        for pair in xs[..c].chunks_exact(2) {
            let (from, to) = (pair[0].max(bb.left()), pair[1].min(bb.right()));
            let mut x = ((from - shift) / step).ceil() * step + shift;
            while x < to && n < limit {
                put(Pos2::new(x, y));
                n += 1;
                x += step;
            }
        }
        y += step;
    }
    n
}

// ---- AI preview on the plan ----

/// Element outline in the AI preview: created is solid with fill, modified is dashed,
/// deleted is hatched (meaning is not conveyed by color alone).
pub fn ai_mark(p: &Painter, ring: &[Pos2], k: AiKind, clip: Rect, ov: &Palette) {
    if ring.len() < 2 {
        return;
    }
    let col = ov.ai(k);
    let st = Stroke::new(OUTLINE_PX, col);
    let mut line = Vec::with_capacity(ring.len() + 1);
    line.extend_from_slice(ring);
    if ring.len() > 2 {
        line.push(ring[0]);
    }
    match k {
        AiKind::Created => {
            p.add(Shape::line(line, st));
        }
        AiKind::Changed => {
            p.extend(Shape::dashed_line(&line, st, GHOST_DASH.0, GHOST_DASH.1));
        }
        AiKind::Deleted => {
            let mut budget = HATCH_MAX;
            let st = Stroke::new(THIN_PX, col.gamma_multiply(0.8));
            hatch(p, ring, Hatch::Diagonal, st, clip, &mut budget);
            p.add(Shape::line(line, Stroke::new(THIN_PX, col)));
        }
    }
}

// ---- 3D background ----

/// 3D background: sky and ground. `horizon` is the on-screen horizon line; it is lighter than `top`, above it
/// the sky goes to the zenith, below it the ground goes to `bottom`. Without a horizon (ortho, top view) it is a vertical gradient;
/// a horizon above the top frame edge gives the same gradient, so orbiting to the top view has no jump.
pub fn sky_ground(rect: Rect, horizon: Option<f32>, top: Color32, bottom: Color32) -> egui::Mesh {
    let mut m = egui::Mesh::default();
    let zenith = mix(top, bottom, 0.35);
    let reach = rect.height().max(1.0);
    let horizon = horizon.map(|h| h.max(rect.top()).min(rect.bottom()));
    let at = |y: f32| match horizon {
        None => {
            let k = ((y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
            mix(top, bottom, k)
        }
        Some(h) if y >= h => mix(top, bottom, ((y - h) / reach).clamp(0.0, 1.0)),
        Some(h) => mix(top, zenith, ((h - y) / reach).clamp(0.0, 1.0)),
    };
    let row = |m: &mut egui::Mesh, y: f32| {
        let c = at(y);
        m.colored_vertex(Pos2::new(rect.left(), y), c);
        m.colored_vertex(Pos2::new(rect.right(), y), c);
    };
    row(&mut m, rect.top());
    if let Some(h) = horizon.filter(|h| *h > rect.top() && *h < rect.bottom()) {
        row(&mut m, h);
        m.add_triangle(0, 1, 3);
        m.add_triangle(0, 3, 2);
        row(&mut m, rect.bottom());
        m.add_triangle(2, 3, 5);
        m.add_triangle(2, 5, 4);
    } else {
        row(&mut m, rect.bottom());
        m.add_triangle(0, 1, 3);
        m.add_triangle(0, 3, 2);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::theme::contrast;

    #[test]
    fn every_hatch_pattern_marks_a_square_and_none_marks_nothing() {
        let ctx = egui::Context::default();
        let p = ctx.layer_painter(egui::LayerId::background());
        let sq = [Pos2::new(0.0, 0.0), Pos2::new(120.0, 0.0), Pos2::new(120.0, 120.0), Pos2::new(0.0, 120.0)];
        let clip = Rect::from_min_max(Pos2::new(-10.0, -10.0), Pos2::new(130.0, 130.0));
        let st = Stroke::new(1.0, Color32::BLACK);
        let used = |k: Hatch| {
            let mut budget = 100_000u32;
            hatch(&p, &sq, k, st, clip, &mut budget);
            100_000 - budget
        };
        assert_eq!(used(Hatch::None), 0);
        for k in [
            Hatch::Diagonal, Hatch::BackDiagonal, Hatch::Cross, Hatch::Dots, Hatch::Steel,
            Hatch::Earth, Hatch::Water, Hatch::Stone, Hatch::Stipple, Hatch::Glass,
        ] {
            assert!(used(k) > 0, "{k:?} draws nothing");
        }
        // dense metal lines out-number masonry lines; a grid is both families
        assert!(used(Hatch::Steel) > used(Hatch::Diagonal));
        assert!(used(Hatch::Stone) > used(Hatch::Water));
        assert!(used(Hatch::Glass) < used(Hatch::Diagonal));
    }

    #[test]
    fn palette_has_every_token_in_both_themes() {
        let magenta = Color32::from_rgb(255, 0, 255);
        for dark in [false, true] {
            let o = pal(dark);
            assert_eq!(o.dark, dark);
            for c in [
                o.selection,
                o.hover,
                o.preview,
                o.snap,
                o.error,
                o.warning,
                o.success,
                o.ai_created,
                o.ai_changed,
                o.ai_deleted,
                o.axis[0],
                o.axis[1],
                o.axis[2],
                o.refplane,
                o.paper,
                o.plate,
                o.plate_border,
                o.ink,
                o.ink_dim,
            ] {
                assert_ne!(c, magenta);
            }
        }
    }

    /// Text on the chip and on the temporary-mode fill: text threshold 4.5 (VISUAL_LANGUAGE).
    #[test]
    fn text_colors_reach_text_contrast() {
        for dark in [false, true] {
            let o = pal(dark);
            for c in [o.ink, o.ink_dim, o.error] {
                assert!(contrast(c, o.plate) >= 4.5, "dark={dark} {c:?}");
            }
            assert!(contrast(o.paper, o.banner_fill()) >= 4.5, "dark={dark}");
        }
    }

    /// Meaning reads on the paper of both themes (graphics threshold, VISUAL_LANGUAGE).
    #[test]
    fn overlay_colors_contrast_with_paper() {
        for dark in [false, true] {
            let o = pal(dark);
            for c in [
                o.selection,
                o.hover,
                o.preview,
                o.snap,
                o.error,
                o.ai_created,
                o.ai_changed,
                o.ai_deleted,
                o.axis[0],
                o.axis[1],
                o.axis[2],
                o.refplane,
            ] {
                assert!(contrast(c, o.paper) >= 3.0, "dark={dark} {c:?}");
            }
        }
    }

    #[test]
    fn one_meaning_one_color() {
        for dark in [false, true] {
            let o = pal(dark);
            assert_ne!(o.selection, o.hover);
            assert_ne!(o.selection, o.preview);
            assert_ne!(o.hover, o.snap);
            assert_ne!(o.ai_created, o.ai_changed);
            assert_ne!(o.ai_changed, o.ai_deleted);
        }
    }

    /// Canvas constants match `tokens.json` (single source).
    #[test]
    fn sizes_match_tokens() {
        let t = DesignTokens::embedded().unwrap();
        let v = &t.viewport;
        assert_eq!(v["snap_glyph_px"].as_f64(), Some(f64::from(SNAP_PX)));
        assert_eq!(
            v["snap_glyph_stroke"].as_f64(),
            Some(f64::from(SNAP_STROKE))
        );
        assert_eq!(
            v["preview"]["dash"][0].as_f64(),
            Some(f64::from(GHOST_DASH.0))
        );
        assert_eq!(
            v["preview"]["dash"][1].as_f64(),
            Some(f64::from(GHOST_DASH.1))
        );
        let cross = &v["rubber_band"]["crossing"]["stroke"];
        assert_eq!(cross[0].as_f64(), Some(f64::from(CROSSING_DASH.0)));
        assert_eq!(cross[1].as_f64(), Some(f64::from(CROSSING_DASH.1)));
    }

    fn flat(c: Color32, under: Color32) -> Color32 {
        let a = f32::from(c.a()) / 255.0;
        let m = |x: u8, y: u8| (f32::from(x) * a + f32::from(y) * (1.0 - a)).round() as u8;
        Color32::from_rgb(m(c.r(), under.r()), m(c.g(), under.g()), m(c.b(), under.b()))
    }

    #[test]
    fn canvas_overlay_contrast_both_themes() {
        use ddc_ui::theme::contrast;
        for dark in [false, true] {
            let o = pal(dark);
            let chip = flat(o.chip, o.paper);
            assert!(contrast(o.ink, chip) >= 4.5, "label text dark={dark}");
            assert!(contrast(o.ink_dim, chip) >= 4.5, "dim text dark={dark}");
            for (n, c) in [("selection", o.selection), ("snap", o.snap), ("preview", o.preview)] {
                assert!(contrast(c, chip) >= 3.0, "{n} vs chip dark={dark}");
            }
            assert!(contrast(o.selection, o.elevated) >= 3.0, "grip outline dark={dark}");
            assert!(contrast(o.snap, o.paper) >= 3.0, "snap glyph dark={dark}");
            assert!(contrast(o.selection, o.paper) >= 3.0, "selection dark={dark}");
        }
    }

    /// Paints a square grip at 1x and 2x and reads the produced shapes back.
    #[test]
    fn grip_probe_size_and_outline_at_1x_and_2x() {
        for ppp in [1.0_f32, 2.0] {
            for dark in [false, true] {
                let o = pal(dark);
                let ctx = egui::Context::default();
                ctx.set_pixels_per_point(ppp);
                let out = ctx.run(egui::RawInput::default(), |ctx| {
                    let p = ctx.layer_painter(egui::LayerId::background());
                    grip(&p, Pos2::new(40.0, 40.0), Grip::Square, false, o);
                });
                let rects: Vec<_> = out
                    .shapes
                    .iter()
                    .filter_map(|c| match &c.shape {
                        Shape::Rect(r) => Some(r.clone()),
                        _ => None,
                    })
                    .collect();
                let body = rects.iter().find(|r| r.fill != Color32::TRANSPARENT).unwrap();
                assert_eq!(body.fill, o.elevated);
                assert_eq!(body.rect.width() * ppp, GRIP_PX * ppp);
                assert_eq!(body.rect.height() * ppp, GRIP_PX * ppp);
                let ring = rects.iter().find(|r| r.stroke.width > 0.0).unwrap();
                assert_eq!(ring.stroke.color, o.selection);
                assert_eq!(ring.stroke.width, GRIP_STROKE);
            }
        }
    }

    #[test]
    fn plates_are_chip_height_and_rest_timer_waits() {
        let s = plate_size(Vec2::new(30.0, 12.0));
        assert_eq!(s, Vec2::new(46.0, crate::draw_chip::CHIP_H));
        let a = Pos2::new(5.0, 5.0);
        let (st, ready) = rest_step(None, a, 10.0);
        assert!(!ready);
        let (st, ready) = rest_step(Some(st), a, 10.2);
        assert!(!ready);
        let (st, ready) = rest_step(Some(st), a, 10.31);
        assert!(ready);
        let (_, ready) = rest_step(Some(st), Pos2::new(50.0, 5.0), 10.4);
        assert!(!ready, "moving restarts the timer");
    }

    #[test]
    fn grip_halo_and_plate_on_grid() {
        assert_eq!((GRIP_PX, GRIP_HOT_PX), (8.0, 10.0));
        assert_eq!(PLATE_PAD, Vec2::new(8.0, 4.0));
        assert_eq!(HALO_PX % 4.0, 0.0);
        assert_eq!(SNAP_LABEL_DELAY_MS, 300);
        let o = pal(false);
        let h = o.halo(Stroke::new(1.0, o.ink));
        assert_eq!(h.width, OUTLINE_PX + 2.0 * HALO_PX);
        assert_eq!(h.color, o.selection_fill);
        assert_eq!(o.elevated, pal(false).elevated);
    }

    #[test]
    fn state_prefers_selection_over_hover() {
        let o = pal(false);
        let base = Stroke::new(1.0, o.ink);
        assert_eq!(o.state(true, true, base).color, o.selection);
        assert_eq!(o.state(false, true, base).color, o.hover);
        assert_eq!(o.state(false, false, base), base);
        // hover does not thin a thick section line
        assert_eq!(o.state(false, true, Stroke::new(2.25, o.ink)).width, 2.25);
        assert_eq!(o.state_fill(false, o.paper), o.paper);
        assert_ne!(o.state_fill(true, o.paper), o.paper);
    }

    #[test]
    fn row_crossings_pair_up_on_concave_ring() {
        let l = [
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(10.0, 4.0),
            Pos2::new(4.0, 4.0),
            Pos2::new(4.0, 10.0),
            Pos2::new(0.0, 10.0),
        ];
        let mut xs = [0.0; 16];
        assert_eq!(row_crossings(&l, 8.0, &mut xs), 2);
        assert_eq!(&xs[..2], &[0.0, 4.0]);
        assert_eq!(row_crossings(&l, 2.0, &mut xs), 2);
        assert_eq!(&xs[..2], &[0.0, 10.0]);
        assert_eq!(row_crossings(&l, 11.0, &mut xs), 0);
    }

    fn dots_of(poly: &[Pos2], bb: Rect, limit: u32) -> Vec<Pos2> {
        let mut out = vec![];
        let n = for_each_dot(poly, 6.0, bb, limit, |q| out.push(q));
        assert_eq!(n as usize, out.len());
        out
    }

    fn diagonal_wall(length: f32, thickness: f32) -> [Pos2; 4] {
        let k = std::f32::consts::FRAC_1_SQRT_2;
        let (l, w) = (length * k, thickness * k);
        [
            Pos2::new(0.0, 0.0),
            Pos2::new(l, l),
            Pos2::new(l - w, l + w),
            Pos2::new(-w, w),
        ]
    }

    /// A 6 m wall at 45° and 100 px/m: dots reach the bottom edge of the frame.
    #[test]
    fn dots_cover_a_diagonal_wall() {
        let wall = diagonal_wall(600.0, 20.0);
        let bb = Rect::from_min_max(Pos2::new(wall[3].x, 0.0), Pos2::new(wall[1].x, wall[2].y));
        let dots = dots_of(&wall, bb, HATCH_MAX);
        assert!((250..420).contains(&dots.len()), "{}", dots.len());
        assert!(dots.iter().any(|q| q.y > wall[1].y - 12.0));
    }

    /// A straight wall across the whole frame: the pattern reaches the far end.
    #[test]
    fn dots_reach_the_far_end_of_a_long_wall() {
        let wall = [
            Pos2::new(0.0, 0.0),
            Pos2::new(1800.0, 0.0),
            Pos2::new(1800.0, 60.0),
            Pos2::new(0.0, 60.0),
        ];
        let bb = Rect::from_min_max(Pos2::ZERO, Pos2::new(1800.0, 60.0));
        let dots = dots_of(&wall, bb, HATCH_MAX);
        assert!(dots.len() > 2500, "{}", dots.len());
        assert!(dots.iter().any(|q| q.x > 1790.0 && q.y > 50.0));
    }

    /// When the limit is insufficient the grid gets sparser but covers the whole area, not just the top rows.
    #[test]
    fn dots_thin_out_instead_of_cutting_off() {
        let (a, b) = (Pos2::ZERO, Pos2::new(1920.0, 1080.0));
        let wall = [a, Pos2::new(b.x, a.y), b, Pos2::new(a.x, b.y)];
        let dots = dots_of(&wall, Rect::from_min_max(a, b), 2000);
        assert!(dots.len() <= 2000);
        assert!(dots.iter().any(|q| q.y > 1000.0 && q.x > 1800.0));
    }

    #[test]
    fn hatch_gate_measures_thickness_not_bbox() {
        // thin diagonal wall: frame 424×424, thickness 1 px
        assert!(contour_width(&diagonal_wall(600.0, 1.0)) < HATCH_MIN_PX);
        // straight wall with a short cut at the joint: thickness 20 px
        let wall = [
            Pos2::new(0.0, 0.0),
            Pos2::new(300.0, 0.0),
            Pos2::new(300.0, 19.0),
            Pos2::new(299.0, 20.0),
            Pos2::new(0.0, 20.0),
        ];
        assert!((contour_width(&wall) - 20.0).abs() < 2.0);
    }

    #[test]
    fn sky_ground_splits_at_horizon() {
        let r = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(100.0, 100.0));
        let (top, bottom) = (Color32::from_gray(240), Color32::from_gray(200));
        let m = sky_ground(r, Some(30.0), top, bottom);
        assert_eq!(m.vertices.len(), 6);
        assert_eq!(m.indices.len(), 12);
        assert_eq!(m.vertices[2].color, top, "the horizon is the lightest tone");
        // no horizon: vertical gradient
        let flat = sky_ground(r, None, top, bottom);
        assert_eq!(flat.vertices[0].color, top);
        assert_eq!(flat.vertices[2].color, bottom);
        // horizon above the frame (or right at its edge): the same gradient, so the top view has no jump
        for h in [-100.0, 0.0] {
            let m = sky_ground(r, Some(h), top, bottom);
            assert_eq!(m.vertices.len(), 4);
            assert_eq!(m.vertices[0].color, flat.vertices[0].color);
            assert_eq!(m.vertices[2].color, flat.vertices[2].color);
        }
        // horizon below the frame: sky only, lighter toward the horizon
        let m = sky_ground(r, Some(200.0), top, bottom);
        assert_eq!(m.vertices.len(), 4);
        assert_eq!(m.vertices[2].color, top);
        assert!(m.vertices[0].color.r() < top.r());
    }

    #[test]
    fn ai_signs_and_rgb() {
        let o = pal(true);
        assert_eq!(AiKind::Deleted.sign(), "−");
        let [r, g, b] = o.ai_rgb(AiKind::Created);
        assert_eq!(Color32::from_rgb(r, g, b), o.ai_created);
    }
}
