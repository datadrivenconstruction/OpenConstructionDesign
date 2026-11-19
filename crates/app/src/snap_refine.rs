// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Snap refinements while drawing and dragging:
//! the touch point of the rubber band beats a farther vertex, a dragged item never snaps to
//! itself, the capture radius is a user setting in screen pixels, inference hints are shown only
//! when the click really applies them, and a held modifier places exactly at the cursor.

use crate::grips::{self, Grip};
use crate::overlay::{self, Palette, SNAP_PX, SNAP_STROKE, THIN_PX};
use crate::snap::{self, Snap, SnapKind};
use crate::wall_tool::{Mode, WallDraft, WallTool};
use ddc_geom::{v2, V2};
use ddc_model::Model;
use egui::{FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};
use std::sync::atomic::{AtomicU32, Ordering};

/// Capture radius in screen pixels: default, and the range the setting accepts.
pub const SNAP_PX_DEFAULT: f32 = overlay::GRIP_HIT_PX;
pub const SNAP_PX_MIN: f32 = 4.0;
pub const SNAP_PX_MAX: f32 = 48.0;
/// Radius in metres when the view scale is unusable.
const FALLBACK_M: f64 = 0.15;
/// Parallel / perpendicular to the previous segment is inferred within this angle.
pub const INFER_TOL_DEG: f64 = 2.0;
/// "Really applied": the placed point satisfies the relation to this precision.
const EXACT: f64 = 1e-6;
/// Probe step used to find which parts of a dragged item move with its grip.
const PROBE_M: f64 = 0.1;

/// Bits of the chosen radius in px; 0 means "not set" (the default).
static SNAP_PX_BITS: AtomicU32 = AtomicU32::new(0);

pub fn clamp_px(px: f32) -> f32 {
    if px.is_finite() {
        px.clamp(SNAP_PX_MIN, SNAP_PX_MAX)
    } else {
        SNAP_PX_DEFAULT
    }
}

/// Applied from the user preferences (`Prefs::apply`).
pub fn set_snap_px(px: f32) {
    SNAP_PX_BITS.store(clamp_px(px).to_bits(), Ordering::Relaxed);
}

pub fn snap_px() -> f32 {
    match SNAP_PX_BITS.load(Ordering::Relaxed) {
        0 => SNAP_PX_DEFAULT,
        b => clamp_px(f32::from_bits(b)),
    }
}

/// Capture radius in model metres for `px` screen pixels at `px_per_m` view scale:
/// the same on screen at any zoom, display density or model unit.
pub fn radius_m(px: f32, px_per_m: f64) -> f64 {
    if px_per_m.is_finite() && px_per_m > 0.0 {
        f64::from(clamp_px(px)) / px_per_m
    } else {
        FALLBACK_M
    }
}

/// The no-snap modifier: only the free point is left.
pub fn gate(list: Vec<Snap>, cursor: V2, no_snap: bool) -> Vec<Snap> {
    if no_snap {
        vec![Snap {
            p: cursor,
            kind: SnapKind::Free,
        }]
    } else {
        list
    }
}

fn finite(p: V2) -> bool {
    p.x.is_finite() && p.y.is_finite()
}

/// Points where the segment `from -> cursor`, extended by `r` past the cursor, touches one of
/// `segs` within `r` of the cursor. The start point itself never counts, and a segment whose
/// own end is within `r` is left to the ordinary end snap (the user aims at that corner).
pub fn touch_points(segs: &[(V2, V2)], from: V2, cursor: V2, r: f64) -> Vec<Snap> {
    let v = cursor - from;
    let l = v.len();
    if !(l > 1e-9 && r > 0.0 && finite(from) && finite(cursor)) {
        return vec![];
    }
    let end = cursor + v * (r / l);
    let mut out = vec![];
    for &(a, b) in segs {
        let reach = a.x.min(b.x) - r <= cursor.x
            && cursor.x <= a.x.max(b.x) + r
            && a.y.min(b.y) - r <= cursor.y
            && cursor.y <= a.y.max(b.y) + r;
        if !reach || (a - cursor).len() <= r || (b - cursor).len() <= r {
            continue;
        }
        if let Some(x) = snap::seg_cross(from, end, a, b) {
            if finite(x) && (x - from).len() > 1e-9 && (x - cursor).len() <= r {
                out.push(Snap {
                    p: x,
                    kind: SnapKind::Intersection,
                });
            }
        }
    }
    out
}

fn is_vertex(k: SnapKind) -> bool {
    matches!(k, SnapKind::Endpoint | SnapKind::Midpoint)
}

/// Merges touch points into a ranked list (free point last); the nearest touch point goes
/// first when everything ahead of it is a vertex farther from the cursor.
pub fn with_touch(mut list: Vec<Snap>, touches: Vec<Snap>, cursor: V2) -> Vec<Snap> {
    let d = |s: &Snap| (s.p - cursor).len();
    let Some(best) = touches.iter().copied().min_by(|a, b| d(a).total_cmp(&d(b))) else {
        return list;
    };
    snap::merge(&mut list, touches, cursor);
    if let Some(i) = list.iter().position(|s| (s.p - best.p).len() < 1e-9) {
        let db = d(&best);
        if i > 0 && list[..i].iter().all(|s| is_vertex(s.kind) && d(s) > db) {
            let t = list.remove(i);
            list.insert(0, t);
        }
    }
    list
}

fn deg_of(d: V2) -> i32 {
    (d.y.atan2(d.x).to_degrees().round() as i32).rem_euclid(360)
}

/// Directions parallel and perpendicular to the previous segment `prev`, from `from`, when the
/// cursor is within [`INFER_TOL_DEG`] and `r` of them.
pub fn inference_points(prev: (V2, V2), from: V2, cursor: V2, r: f64) -> Vec<Snap> {
    let d = prev.1 - prev.0;
    let (ld, v) = (d.len(), cursor - from);
    let lv = v.len();
    if !(ld > 1e-9 && lv > 1e-9 && finite(cursor)) {
        return vec![];
    }
    let u = d * (1.0 / ld);
    let tol = INFER_TOL_DEG.to_radians().sin();
    let mut out = vec![];
    for dir in [u, v2(-u.y, u.x)] {
        let along = v.dot(dir);
        let off = v.cross(dir).abs();
        if along.abs() > 1e-9 && off / lv <= tol && off <= r {
            out.push(Snap {
                p: from + dir * along,
                kind: SnapKind::Angle(deg_of(dir * along.signum())),
            });
        }
    }
    out
}

/// The draw tool's start point for the segment being drawn.
fn tool_from(t: &WallTool) -> Option<V2> {
    match t.kind() {
        Mode::Polyline if !t.arc && t.last.refine => t.last.pts.last().copied(),
        _ => None,
    }
}

fn tool_segs(t: &WallTool) -> Vec<(V2, V2)> {
    let mut out: Vec<(V2, V2)> = Vec::with_capacity(t.walls.len() * 3 + t.last.pts.len());
    for w in t.walls.iter().chain(&t.link_walls) {
        out.push((w.a, w.b));
        let len = (w.b - w.a).len();
        if w.half > 0.0 && len > 0.0 {
            let n = v2(-(w.b - w.a).y / len, (w.b - w.a).x / len) * w.half;
            out.push((w.a + n, w.b + n));
            out.push((w.a - n, w.b - n));
        }
    }
    out.extend(t.last.pts.windows(2).map(|s| (s[0], s[1])));
    out.extend(t.underlay_segs.iter().copied());
    out
}

/// Called at the end of the draw tool's candidate update.
pub fn refine_tool(t: &mut WallTool, cursor: V2) {
    if t.no_snap {
        t.snaps = gate(vec![], cursor, true);
        return;
    }
    let Some(from) = tool_from(t) else {
        return;
    };
    let touches = touch_points(&tool_segs(t), from, cursor, t.radius);
    let list = std::mem::take(&mut t.snaps);
    let mut list = with_touch(list, touches, cursor);
    if let [.., a, b] = t.last.pts.as_slice() {
        let inf = inference_points((*a, *b), from, cursor, t.radius);
        if !inf.is_empty() {
            snap::merge(&mut list, inf, cursor);
        }
    }
    t.snaps = list;
}

/// Parts of a dragged item that move with its grip (rest positions).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Moving {
    pub pts: Vec<V2>,
    pub segs: Vec<(V2, V2)>,
}

fn same_pt(a: V2, b: V2) -> bool {
    (a - b).len() <= EXACT
}

fn same_seg(s: (V2, V2), t: (V2, V2)) -> bool {
    (same_pt(s.0, t.0) && same_pt(s.1, t.1)) || (same_pt(s.0, t.1) && same_pt(s.1, t.0))
}

fn on_seg(p: V2, (a, b): (V2, V2)) -> bool {
    same_pt(snap::closest_on_segment(p, a, b).0, p)
}

impl Moving {
    /// `rest`: the item's outline as it is; `probes`: its outline with the grip moved a little
    /// (one entry per direction). The grip and every own vertex are excluded (snapping onto
    /// them only makes a degenerate item); an edge is excluded when it differs in any probe,
    /// together with its midpoint. Static own edges stay: other elements often share them.
    pub fn diff(rest: &[Vec<V2>], probes: &[Vec<Vec<V2>>], grip: V2) -> Self {
        let mut m = Moving {
            pts: vec![grip],
            segs: vec![],
        };
        m.pts.extend(rest.iter().flatten().copied());
        let probes: Vec<&Vec<Vec<V2>>> = probes.iter().filter(|p| !p.is_empty()).collect();
        if probes.is_empty() {
            return m;
        }
        let segs = |o: &[Vec<V2>]| -> Vec<(V2, V2)> {
            o.iter()
                .flat_map(|l| l.windows(2).map(|s| (s[0], s[1])))
                .collect()
        };
        let probe_segs: Vec<Vec<(V2, V2)>> =
            probes.iter().map(|p| segs(p.as_slice())).collect();
        for s in segs(rest) {
            if probe_segs.iter().any(|ps| !ps.iter().any(|&q| same_seg(s, q))) {
                m.segs.push(s);
                m.pts.push((s.0 + s.1) * 0.5);
            }
        }
        m
    }

    /// True when the candidate belongs to the moving parts.
    pub fn owns(&self, s: &Snap) -> bool {
        let at_pt = || self.pts.iter().any(|&q| same_pt(q, s.p));
        match s.kind {
            SnapKind::Free | SnapKind::Grid | SnapKind::Angle(_) => false,
            SnapKind::Endpoint
            | SnapKind::Midpoint
            | SnapKind::AlignX
            | SnapKind::AlignY
            | SnapKind::AlignXY => at_pt(),
            _ => at_pt() || self.segs.iter().any(|&g| on_seg(s.p, g)),
        }
    }
}

/// Grip drags: the item and whatever moves with it are never snap targets.
pub fn drop_moving(list: Vec<Snap>, m: &Moving) -> Vec<Snap> {
    list.into_iter().filter(|s| !m.owns(s)).collect()
}

/// Moving parts of the item under grip `g`, found by probing the grip in X and Y.
pub fn moving_parts(m: &Model, g: &Grip) -> Moving {
    let outline = |to: V2| {
        grips::compute(m, g, to)
            .map(|c| grips::outline(&c))
            .unwrap_or_default()
    };
    let rest = outline(g.pos);
    if rest.is_empty() {
        return Moving::diff(&[], &[], g.pos);
    }
    let probes = [
        outline(g.pos + v2(PROBE_M, 0.0)),
        outline(g.pos + v2(0.0, PROBE_M)),
    ];
    Moving::diff(&rest, &probes, g.pos)
}

/// Relations the click will keep, shown as glyphs next to the placed point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    Horizontal,
    Vertical,
    Perpendicular,
    Parallel,
    OnAxis,
}

impl Hint {
    pub fn key(self) -> &'static str {
        match self {
            Self::Horizontal => "snap_refine.hint.horizontal",
            Self::Vertical => "snap_refine.hint.vertical",
            Self::Perpendicular => "snap_refine.hint.perpendicular",
            Self::Parallel => "snap_refine.hint.parallel",
            Self::OnAxis => "snap_refine.hint.on_axis",
        }
    }
}

/// Hints for the point `placed` that the click will create: computed from the placed point
/// itself, so a relation shows only when it holds exactly, never because the cursor is near it.
pub fn hints(
    from: Option<V2>,
    prev: Option<(V2, V2)>,
    placed: V2,
    axes: &[(V2, V2)],
) -> Vec<Hint> {
    let mut out = vec![];
    if !finite(placed) {
        return out;
    }
    if let Some(f) = from {
        let v = placed - f;
        let l = v.len();
        if l > EXACT {
            if v.y.abs() / l <= EXACT {
                out.push(Hint::Horizontal);
            } else if v.x.abs() / l <= EXACT {
                out.push(Hint::Vertical);
            }
            if let Some((a, b)) = prev {
                let d = b - a;
                let ld = d.len();
                if ld > EXACT {
                    if (v.cross(d) / (l * ld)).abs() <= EXACT {
                        out.push(Hint::Parallel);
                    } else if (v.dot(d) / (l * ld)).abs() <= EXACT {
                        out.push(Hint::Perpendicular);
                    }
                }
            }
        }
    }
    let on_axis = axes.iter().any(|&(a, b)| {
        let (c, _) = snap::closest_on_segment(placed, a, b);
        same_pt(c, placed) && !same_pt(placed, a) && !same_pt(placed, b)
    });
    if on_axis {
        out.push(Hint::OnAxis);
    }
    out
}

/// Hints for the draw tool's current placed point (`dr`: the drawing session's draft).
pub fn tool_hints(t: &WallTool, dr: &WallDraft) -> Vec<Hint> {
    if t.kind() == Mode::Select || !t.last.refine {
        return vec![];
    }
    let from = tool_from(t);
    let prev = match (from, &t.last.pts[..]) {
        (Some(_), [.., a, b]) => Some((*a, *b)),
        _ => None,
    };
    let axes: Vec<(V2, V2)> = t
        .walls
        .iter()
        .chain(&t.link_walls)
        .map(|w| (w.a, w.b))
        .collect();
    hints(from, prev, t.snapped_in(dr), &axes)
}

fn hint_glyph(p: &Painter, c: Pos2, h: Hint, ov: &Palette) {
    let s = Stroke::new(SNAP_STROKE, ov.snap);
    let r = SNAP_PX * 0.4;
    let v = |x: f32, y: f32| c + Vec2::new(x * r, y * r);
    match h {
        Hint::Horizontal => {
            p.line_segment([v(-1.0, 0.0), v(1.0, 0.0)], s);
            p.line_segment([v(-1.0, -0.5), v(-1.0, 0.5)], s);
            p.line_segment([v(1.0, -0.5), v(1.0, 0.5)], s);
        }
        Hint::Vertical => {
            p.line_segment([v(0.0, -1.0), v(0.0, 1.0)], s);
            p.line_segment([v(-0.5, -1.0), v(0.5, -1.0)], s);
            p.line_segment([v(-0.5, 1.0), v(0.5, 1.0)], s);
        }
        Hint::Perpendicular => {
            p.line_segment([v(-1.0, 1.0), v(1.0, 1.0)], s);
            p.line_segment([v(0.0, 1.0), v(0.0, -1.0)], s);
        }
        Hint::Parallel => {
            p.line_segment([v(-0.9, 1.0), v(-0.1, -1.0)], s);
            p.line_segment([v(0.1, 1.0), v(0.9, -1.0)], s);
        }
        Hint::OnAxis => {
            p.line_segment([v(-1.0, 0.0), v(-0.4, 0.0)], s);
            p.circle_filled(v(0.0, 0.0), SNAP_STROKE, ov.snap);
            p.line_segment([v(0.4, 0.0), v(1.0, 0.0)], s);
        }
    }
}

fn plate(p: &Painter, r: Rect, ov: &Palette) {
    p.rect_filled(r, ddc_ui::theme::radius::SM, ov.plate.gamma_multiply(0.92));
    p.rect_stroke(
        r,
        ddc_ui::theme::radius::SM,
        Stroke::new(THIN_PX, ov.snap),
        StrokeKind::Inside,
    );
}

fn caption(p: &Painter, at: Pos2, text: String, ov: &Palette) {
    let g = p.layout_no_wrap(
        text,
        FontId::proportional(ddc_ui::theme::type_size::CAPTION),
        ov.ink,
    );
    let pad = Vec2::new(4.0, 2.0);
    let r = Rect::from_min_size(at, g.size()).expand2(pad);
    plate(p, r, ov);
    p.galley(at, g, ov.ink);
}

/// Draws the hint glyphs above-right of the placed point and the no-snap tag at the cursor.
pub fn paint_tool(p: &Painter, to_s: &dyn Fn(V2) -> Pos2, t: &WallTool, dr: &WallDraft, ov: &Palette) {
    if t.kind() == Mode::Select {
        return;
    }
    let at = to_s(t.snapped_in(dr));
    let hs = tool_hints(t, dr);
    let cell = SNAP_PX + 6.0;
    let mut x = at.x + 14.0;
    let y = at.y - 14.0 - cell;
    for &h in &hs {
        let r = Rect::from_min_size(Pos2::new(x, y), Vec2::splat(cell));
        plate(p, r, ov);
        hint_glyph(p, r.center(), h, ov);
        x += cell + 2.0;
    }
    if !hs.is_empty() {
        let names: Vec<String> = hs.iter().map(|h| crate::tr(h.key(), &[])).collect();
        caption(p, Pos2::new(x + 4.0, y + 3.0), names.join(" \u{b7} "), ov);
    }
    if t.no_snap {
        caption(p, at + Vec2::new(14.0, -14.0 - 2.0 * cell), crate::tr("snap_refine.no_snap", &[]), ov);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snap::{SnapCtx, SnapWall};
    use crate::wall_tool::Event;

    fn ctx<'a>(walls: &'a [SnapWall], from: Option<V2>, r: f64) -> SnapCtx<'a> {
        SnapCtx {
            walls,
            draft: &[],
            from,
            grid: 0.0,
            radius: r,
        }
    }

    fn sn(p: V2, kind: SnapKind) -> Snap {
        Snap { p, kind }
    }

    fn segs(w: &[SnapWall]) -> Vec<(V2, V2)> {
        w.iter().map(|w| (w.a, w.b)).collect()
    }

    fn walls() -> [SnapWall; 2] {
        [
            SnapWall {
                a: v2(0.0, 0.0),
                b: v2(10.0, 0.0),
                half: 0.0,
            },
            SnapWall {
                a: v2(2.12, -3.0),
                b: v2(2.12, 0.0),
                half: 0.0,
            },
        ]
    }

    fn snapped(w: &[SnapWall], from: V2, cursor: V2) -> Snap {
        let r = 0.15;
        let base = snap::candidates(&ctx(w, Some(from), r), cursor);
        let t = touch_points(&segs(w), from, cursor, r);
        with_touch(base, t, cursor)[0]
    }

    #[test]
    fn touch_beats_farther_vertex() {
        let w = walls();
        // coming straight down onto the wall: the touch point is 5 cm away, the vertex 13 cm
        let base = snap::candidates(&ctx(&w, Some(v2(2.0, 5.0)), 0.15), v2(2.0, 0.05));
        assert_eq!(base[0].kind, SnapKind::Endpoint, "without the refinement the vertex wins");
        let s = snapped(&w, v2(2.0, 5.0), v2(2.0, 0.05));
        assert_eq!(s.kind, SnapKind::Intersection);
        assert!((s.p - v2(2.0, 0.0)).len() < 1e-9, "{:?}", s.p);
        // the touch point is ahead of the cursor too (segment not yet reaching the wall)
        let s = snapped(&w, v2(1.0, 5.0), v2(1.0, 0.1));
        assert_eq!((s.kind, s.p), (SnapKind::Intersection, v2(1.0, 0.0)));
        // a closer vertex still wins over a farther touch point
        let s = snapped(&w, v2(0.0, 0.5), v2(2.12, 0.02));
        assert_eq!((s.kind, s.p), (SnapKind::Endpoint, v2(2.12, 0.0)));
        // the start point of the segment is never a touch point
        assert!(touch_points(&segs(&w), v2(3.0, 0.0), v2(3.0, 0.1), 0.15).is_empty());
        // near the touched segment's own end the end wins: closing a loop on its start
        let loop_segs = [(v2(0.0, 0.0), v2(2.0, 0.0)), (v2(2.0, 0.0), v2(2.0, 2.0))];
        assert!(touch_points(&loop_segs, v2(2.0, 2.0), v2(0.03, 0.02), 0.15).is_empty());
        // degenerate input gives nothing and no panic
        assert!(touch_points(&segs(&w), v2(1.0, 1.0), v2(1.0, 1.0), 0.15).is_empty());
        assert!(touch_points(&segs(&w), v2(1.0, 1.0), v2(f64::NAN, 1.0), 0.15).is_empty());
    }

    #[test]
    fn touch_in_the_draw_tool() {
        let mut t = crate::wall_tool::Rig::new();
        t.mode = Mode::Polyline;
        t.grid = 0.0;
        t.radius = 0.15;
        t.walls = walls().to_vec();
        t.draft.points = vec![v2(2.0, 5.0)];
        t.handle(Event::Move(v2(2.0, 0.05)));
        assert!((t.snapped() - v2(2.0, 0.0)).len() < 1e-9, "{:?}", t.snapped());
        assert_eq!(t.snaps.last().map(|s| s.kind), Some(SnapKind::Free));
    }

    #[test]
    fn drag_ignores_own_points() {
        // wall end grip at (5, 0): its axis and the far end are the item; (5, 0) moves
        let rest = vec![vec![v2(0.0, 0.0), v2(5.0, 0.0)]];
        let probes = vec![
            vec![vec![v2(0.0, 0.0), v2(5.1, 0.0)]],
            vec![vec![v2(0.0, 0.0), v2(5.0, 0.1)]],
        ];
        let m = Moving::diff(&rest, &probes, v2(5.0, 0.0));
        let list = vec![
            sn(v2(5.0, 0.0), SnapKind::Endpoint),
            sn(v2(2.5, 0.0), SnapKind::Midpoint),
            sn(v2(3.0, 0.0), SnapKind::Perpendicular),
            sn(v2(1.0, 0.0), SnapKind::Intersection),
            sn(v2(0.0, 0.0), SnapKind::Endpoint),
            sn(v2(2.0, 0.0), SnapKind::Endpoint),
            sn(v2(5.0, 0.0), SnapKind::Grid),
            sn(v2(5.0, 0.01), SnapKind::Free),
        ];
        let kept = drop_moving(list, &m);
        assert_eq!(
            kept,
            vec![
                sn(v2(2.0, 0.0), SnapKind::Endpoint),
                sn(v2(5.0, 0.0), SnapKind::Grid),
                sn(v2(5.0, 0.01), SnapKind::Free),
            ],
            "another element's end on the axis and the grid stay; own ends, axis, midpoint go"
        );
        // slab vertex 0 drag: the far edge 1-2 stays static and keeps its snaps
        let ring = vec![v2(0.0, 0.0), v2(4.0, 0.0), v2(4.0, 3.0), v2(0.0, 0.0)];
        let moved = |d: V2| vec![vec![d, v2(4.0, 0.0), v2(4.0, 3.0), d]];
        let m = Moving::diff(
            &[ring],
            &[moved(v2(0.1, 0.0)), moved(v2(0.0, 0.1))],
            v2(0.0, 0.0),
        );
        let on_static = sn(v2(4.0, 1.5), SnapKind::WallAxis);
        let on_moving = sn(v2(2.0, 0.0), SnapKind::WallAxis);
        assert!(!m.owns(&on_static) && m.owns(&on_moving));
        // probes failed: own points are still dropped, no edge is guessed as moving
        let wall = [vec![v2(0.0, 0.0), v2(5.0, 0.0)]];
        let m = Moving::diff(&wall, &[vec![], vec![]], v2(5.0, 0.0));
        assert_eq!((m.pts.len(), m.segs.len()), (3, 0));
    }

    #[test]
    fn drag_ignores_joined_end_in_the_editor() {
        use crate::editor::Editor;
        use crate::grips::{Kind, Role};
        let mut ed = Editor::new_in_memory().unwrap();
        let segs = [((0.0, 0.0), (5.0, 0.0)), ((5.0, 0.0), (5.0, 4.0))];
        crate::modify_tools::tests::walls(&mut ed, &segs);
        let id = ed.walls.iter().find(|w| w.a == v2(0.0, 0.0)).unwrap().id;
        let g = grips::grips_in(&ed.model, Kind::Wall, id, &grips::view_key(&ed))
            .unwrap()
            .into_iter()
            .find(|g| g.role == Role::End(1))
            .expect("end grip");
        let m = moving_parts(&ed.model, &g);
        let raw = ed.snap_list(v2(5.01, 0.01), None, 0.15, Some(id));
        let joined = |l: &[Snap]| {
            l.iter()
                .any(|s| s.kind == SnapKind::Endpoint && (s.p - v2(5.0, 0.0)).len() < 1e-9)
        };
        assert!(joined(&raw), "the joined wall's end is offered before the filter");
        assert!(!joined(&drop_moving(raw, &m)), "it moves with the grip");
        let far = drop_moving(ed.snap_list(v2(5.01, 3.99), None, 0.15, Some(id)), &m);
        assert_eq!((far[0].kind, far[0].p), (SnapKind::Endpoint, v2(5.0, 4.0)));
    }

    #[test]
    fn snap_radius_is_pixels_not_mm() {
        // the same 12 px is 12 cm at 100 px/m and 1.2 cm at 1000 px/m
        assert!((radius_m(12.0, 100.0) - 0.12).abs() < 1e-12);
        assert!((radius_m(12.0, 1000.0) - 0.012).abs() < 1e-12);
        assert!((radius_m(24.0, 100.0) - 0.24).abs() < 1e-12, "the user setting scales it");
        assert_eq!(radius_m(1000.0, 100.0), f64::from(SNAP_PX_MAX) / 100.0);
        assert_eq!(radius_m(0.0, 100.0), f64::from(SNAP_PX_MIN) / 100.0);
        assert_eq!(radius_m(f32::NAN, 100.0), f64::from(SNAP_PX_DEFAULT) / 100.0);
        assert_eq!(radius_m(12.0, 0.0), FALLBACK_M);
        assert_eq!(radius_m(12.0, f64::NAN), FALLBACK_M);
        assert_eq!(SNAP_PX_DEFAULT, overlay::GRIP_HIT_PX, "the default keeps today's radius");
        let p = crate::prefs::Prefs::parse("snap.px=30\n");
        assert_eq!(p.snap_px, 30.0);
        assert_eq!(crate::prefs::Prefs::parse(&p.text()).snap_px, 30.0);
        assert_eq!(crate::prefs::Prefs::parse("snap.px=900\n").snap_px, SNAP_PX_MAX);
        assert_eq!(crate::prefs::Prefs::parse("snap.px=NaN\n").snap_px, SNAP_PX_DEFAULT);
        assert_eq!(crate::prefs::Prefs::parse("").snap_px, SNAP_PX_DEFAULT);
    }

    #[test]
    fn glyph_shown_only_when_applied() {
        let o = v2(0.0, 0.0);
        assert_eq!(hints(Some(o), None, v2(5.0, 0.0), &[]), vec![Hint::Horizontal]);
        assert_eq!(hints(Some(o), None, v2(0.0, -3.0), &[]), vec![Hint::Vertical]);
        // 2 degrees off and not snapped: nothing is claimed
        assert!(hints(Some(o), None, v2(5.0, 0.17), &[]).is_empty());
        let prev = Some((v2(0.0, -4.0), o));
        assert_eq!(
            hints(Some(o), prev, v2(3.0, 0.0), &[]),
            vec![Hint::Horizontal, Hint::Perpendicular]
        );
        let prev = Some((o, v2(3.0, 4.0)));
        assert_eq!(hints(Some(v2(3.0, 4.0)), prev, v2(6.0, 8.0), &[]), vec![Hint::Parallel]);
        assert!(hints(Some(v2(3.0, 4.0)), prev, v2(6.0, 8.05), &[]).is_empty());
        let axis = [(v2(0.0, 2.0), v2(10.0, 2.0))];
        assert_eq!(hints(None, None, v2(4.0, 2.0), &axis), vec![Hint::OnAxis]);
        assert!(hints(None, None, v2(4.0, 2.001), &axis).is_empty());
        assert!(hints(None, None, v2(10.0, 2.0), &axis).is_empty(), "an end is not 'on axis'");

        // the draw tool: hints follow the point the click will place
        let mut t = crate::wall_tool::Rig::new();
        t.mode = Mode::Polyline;
        t.grid = 0.0;
        t.radius = 0.15;
        t.draft.points = vec![v2(0.0, -4.0), o];
        let near = v2(5.0, 0.12); // ~1.4 degrees off horizontal
        t.handle(Event::Move(near));
        assert_eq!(t.snapped().y, 0.0, "the angle snap applies");
        assert_eq!(tool_hints(&t, &t.draft), vec![Hint::Horizontal, Hint::Perpendicular]);
        t.no_snap = true;
        t.handle(Event::Move(near));
        assert_eq!(t.snapped(), near, "no snap: exactly at the cursor");
        assert!(tool_hints(&t, &t.draft).is_empty(), "nothing is applied, nothing is shown");
        // the user picks the free point with Tab: hints go away too
        t.no_snap = false;
        t.handle(Event::Move(near));
        t.snap_idx = t.snaps.len() - 1;
        assert!(tool_hints(&t, &t.draft).is_empty());
    }

    #[test]
    fn inference_parallel_and_perpendicular_to_previous() {
        let prev = (v2(0.0, 0.0), v2(3.0, 4.0)); // 53.13 degrees
        let from = prev.1;
        let c = inference_points(prev, from, from + v2(3.0, 4.05), 0.15);
        assert_eq!(c.len(), 1);
        assert!(((c[0].p - from).cross(v2(3.0, 4.0))).abs() < 1e-9, "exactly parallel");
        assert_eq!(c[0].kind, SnapKind::Angle(53));
        let c = inference_points(prev, from, from + v2(-4.0, 3.05), 0.15);
        assert_eq!(c.len(), 1);
        assert!((c[0].p - from).dot(v2(3.0, 4.0)).abs() < 1e-9, "exactly perpendicular");
        assert!(inference_points(prev, from, from + v2(3.0, 5.0), 0.15).is_empty());
        let placed = c[0].p;
        assert_eq!(hints(Some(from), Some(prev), placed, &[]), vec![Hint::Perpendicular]);
    }

    #[test]
    fn no_snap_modifier_places_at_cursor() {
        let mut t = crate::wall_tool::Rig::new();
        t.mode = Mode::Polyline;
        t.grid = 1.0;
        t.radius = 0.15;
        t.walls = walls().to_vec();
        let c = v2(10.03, 0.02);
        t.handle(Event::Move(c));
        assert_eq!(t.snapped(), v2(10.0, 0.0));
        t.no_snap = true;
        t.handle(Event::Click(c));
        assert_eq!(t.draft.points.last().copied(), Some(c), "placed exactly at the cursor");
        assert_eq!(t.snaps, vec![sn(c, SnapKind::Free)]);
        let l = gate(vec![sn(v2(1.0, 1.0), SnapKind::Endpoint)], c, true);
        assert_eq!(l, vec![sn(c, SnapKind::Free)]);
        let l = gate(vec![sn(v2(1.0, 1.0), SnapKind::Endpoint)], c, false);
        assert_eq!(l[0].kind, SnapKind::Endpoint);
    }
}
