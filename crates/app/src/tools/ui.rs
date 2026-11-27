// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Drawing of model tools in plan: element layer, preview (canvas-palette ghost,
//! red = invalid), temporary dimensions (click a value to type), 32 px option bar.

use super::layer::Cls;
use super::opt_field;
use super::{column, mep, roof, stair, Active, Kind, Shape};
use crate::editor::Editor;
use crate::overlay::{self, Palette, Plate};
use crate::ui_kit::Tokens;
use crate::units::format_length;
use ddc_geom::{v2, V2};
use ddc_model::PhaseShow;
use egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

/// Option bar height, px.
pub const BAR_H: f32 = 32.0;
/// The bar wraps its controls onto at most this many lines of `BAR_H`.
const MAX_LINES: f32 = 3.0;

struct Pal {
    accent: Color32,
    fill: Color32,
    bad: Color32,
    dim: Color32,
    faint: Color32,
    datum: Color32,
    refp: Color32,
    ov: &'static Palette,
}

fn pal(t: &Tokens) -> Pal {
    let ov = t.ov();
    Pal {
        accent: ov.preview,
        fill: ov.ghost_fill(),
        bad: ov.error,
        dim: ov.preview,
        faint: t.text_dim,
        datum: t.text_dim,
        refp: ov.refplane,
        ov,
    }
}

/// Plan rectangle: centre, half-sizes, rotation.
fn rect_pts(c: V2, hx: f64, hy: f64, rot: f64) -> Vec<V2> {
    let (s, co) = rot.sin_cos();
    [(-hx, -hy), (hx, -hy), (hx, hy), (-hx, hy)]
        .iter()
        .map(|&(x, y)| c + v2(x * co - y * s, x * s + y * co))
        .collect()
}

/// Band of width w along a→b.
fn band(a: V2, b: V2, w: f64) -> Vec<V2> {
    let d = b - a;
    let l = d.len().max(1e-9);
    let n = v2(-d.y / l, d.x / l) * (w / 2.0);
    vec![a + n, b + n, b - n, a - n]
}

pub(super) fn poly(p: &egui::Painter, pts: Vec<Pos2>, fill: Color32, st: Stroke) {
    if pts.len() >= 3 {
        p.add(egui::Shape::convex_polygon(pts.clone(), fill, Stroke::NONE));
    }
    p.add(egui::Shape::closed_line(pts, st));
}

/// Polygon without a convexity requirement: outline + translucent triangle fill.
pub(super) fn poly_any(p: &egui::Painter, pts: &[Pos2], fill: Color32, st: Stroke) {
    if pts.len() >= 3 {
        let mut m = egui::Mesh::default();
        let idx: Vec<[f64; 2]> = pts.iter().map(|q| [q.x as f64, q.y as f64]).collect();
        for q in pts {
            m.colored_vertex(*q, fill);
        }
        for tri in ear_cut(&idx) {
            m.add_triangle(tri[0], tri[1], tri[2]);
        }
        p.add(m);
    }
    p.add(egui::Shape::closed_line(pts.to_vec(), st));
}

/// Simple-polygon triangulation by ear clipping (any winding order).
fn ear_cut(pts: &[[f64; 2]]) -> Vec<[u32; 3]> {
    let n = pts.len();
    let area: f64 = (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a[0] * b[1] - a[1] * b[0]
        })
        .sum();
    let mut v: Vec<usize> = (0..n).collect();
    if area < 0.0 {
        v.reverse();
    }
    let cross = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    };
    let mut out = vec![];
    let mut guard = 0;
    while v.len() > 3 && guard < n * n {
        guard += 1;
        let k = v.len();
        let mut cut = false;
        for i in 0..k {
            let (ia, ib, ic) = (v[(i + k - 1) % k], v[i], v[(i + 1) % k]);
            let (a, b, c) = (pts[ia], pts[ib], pts[ic]);
            if cross(a, b, c) <= 0.0 {
                continue;
            }
            let inside = v.iter().any(|&j| {
                j != ia
                    && j != ib
                    && j != ic
                    && cross(a, b, pts[j]) > 0.0
                    && cross(b, c, pts[j]) > 0.0
                    && cross(c, a, pts[j]) > 0.0
            });
            if !inside {
                out.push([ia as u32, ib as u32, ic as u32]);
                v.remove(i);
                cut = true;
                break;
            }
        }
        if !cut {
            break;
        }
    }
    if v.len() == 3 {
        out.push([v[0] as u32, v[1] as u32, v[2] as u32]);
    }
    out
}

/// Edge count of a polyline of `n` points (fewer than two — none).
fn dash_segs(n: usize, closed: bool) -> usize {
    match n {
        0 | 1 => 0,
        _ if closed => n,
        _ => n - 1,
    }
}

pub(super) fn dashed(p: &egui::Painter, a: Pos2, b: Pos2, st: Stroke, dash: f32, gap: f32) {
    p.add(egui::Shape::dashed_line(&[a, b], st, dash, gap));
}

const BUBBLE_R: f32 = overlay::BUBBLE_R;

/// Centres of grid bubbles for line ends `(at, out)` (`out` — one radius outward along the line),
/// in drawing order. Dense axes (6/6a, D/D') and grids of neighbouring buildings end side by
/// side: a bubble that would touch one already placed moves outward along its own line, one
/// radius per step, until it is clear (a leader joins it to its line).
pub(crate) fn place_bubbles(ends: &[(Pos2, egui::Vec2)]) -> Vec<Pos2> {
    let clear = 2.0 * BUBBLE_R + 1.0;
    let mut placed: Vec<Pos2> = Vec::with_capacity(ends.len());
    for &(at, out) in ends {
        let mut c = at;
        for _ in 0..24 {
            if placed.iter().all(|q| q.distance(c) >= clear) {
                break;
            }
            c += out;
        }
        placed.push(c);
    }
    placed
}

/// Model-tool element layer: below room labels, handles and the draft.
pub fn paint_layer(p: &egui::Painter, ed: &mut Editor, t: &Tokens, to_s: &dyn Fn(V2) -> Pos2) {
    ed.tool.layer.sync(&ed.model, ed.level, ed.revision);
    draw_layer(p, ed, t, &pal(t), to_s);
    crate::struct_props::paint_sel(p, ed, t.ov().selection, to_s);
}

/// Active tool preview, temporary dimensions and option bar.
pub fn paint(
    ui: &mut egui::Ui,
    p: &egui::Painter,
    rect: Rect,
    ed: &mut Editor,
    t: &Tokens,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    let c = pal(t);
    crate::interact::sessions::model_tool::with(ed, |ed, a| {
        if a.seen || !a.pts.is_empty() {
            preview(p, ed, a, &c, to_s);
            temp_dims(ui, p, ed, a, &c, to_s);
        }
        options_bar(ui, rect, ed, a, t);
    });
}

fn draw_layer(p: &egui::Painter, ed: &Editor, t: &Tokens, c: &Pal, to_s: &dyn Fn(V2) -> Pos2) {
    let l = &ed.tool.layer;
    let st = Stroke::new(1.0, c.datum);
    // visible grid line ends (bubbles hidden in the view take no space in the layout)
    let mut ends = Vec::with_capacity(l.grids.len() * 2);
    let mut names: Vec<&str> = Vec::with_capacity(l.grids.len() * 2);
    let edge = Stroke::new(1.0, t.border);
    for (k, (name, a, b)) in l.grids.iter().enumerate() {
        let bub = l.grid_bubbles.get(k).copied().unwrap_or((true, true));
        let (sa, sb) = (to_s(*a), to_s(*b));
        p.add(egui::Shape::dashed_line_with_offset(
            &[sa, sb],
            st,
            &[14.0, 3.0],
            &[3.0, 3.0],
            0.0,
        ));
        if bub.1 {
            ends.push(super::grid_bubble::End { a: sa, b: sb });
            names.push(name);
        }
        if bub.0 {
            ends.push(super::grid_bubble::End { a: sb, b: sa });
            names.push(name);
        }
    }
    let spots = super::grid_bubble::layout(&ends, p.clip_rect());
    for (spot, name) in spots.iter().zip(&names) {
        let Some(s) = spot else { continue };
        if !s.pinned && s.centre != s.at {
            p.line_segment([s.at - s.out, s.centre - s.out], st);
        }
        let stroke = if s.pinned { edge } else { st };
        super::grid_bubble::paint(p, s.centre, name, stroke, t.text, c.ov);
    }
    for (a, b) in &l.refs {
        dashed(p, to_s(*a), to_s(*b), Stroke::new(1.0, c.refp), 5.0, 4.0);
    }
    for (cls, pts, closed, sh) in &l.polys {
        let s: Vec<Pos2> = pts.iter().map(|q| to_s(*q)).collect();
        if s.len() < 2 {
            continue;
        }
        // view phase: demolished — dashed outline without fill, existing — half tone
        if *sh == PhaseShow::Dashed {
            let mut ring = s.clone();
            if *closed {
                ring.push(s[0]);
            }
            p.extend(egui::Shape::dashed_line(
                &ring,
                Stroke::new(1.2, t.text_dim),
                6.0,
                4.0,
            ));
            continue;
        }
        let half = *sh == PhaseShow::Halftone;
        let tone = |c: Color32| if half { c.gamma_multiply(0.35) } else { c };
        let (fill, st) = match cls {
            Cls::Column if half => (Color32::TRANSPARENT, Stroke::new(t.lw[0], t.cut)),
            Cls::MeshFill => {
                let mut mesh = egui::Mesh::default();
                for (i, q) in s.iter().enumerate().take(s.len() / 3 * 3) {
                    mesh.colored_vertex(*q, t.poche);
                    mesh.indices.push(i as u32);
                }
                p.add(egui::Shape::mesh(mesh));
                continue;
            }
            Cls::MeshCut | Cls::MeshHole => (Color32::TRANSPARENT, Stroke::new(t.lw[0], t.cut)),
            Cls::MeshEdge => (Color32::TRANSPARENT, Stroke::new(t.lw[2], t.projected)),
            Cls::Column => (t.poche, Stroke::new(t.lw[0], t.cut)),
            Cls::Beam => (Color32::TRANSPARENT, Stroke::new(t.lw[1], t.projected)),
            Cls::Roof => (Color32::TRANSPARENT, Stroke::new(t.lw[2], t.beyond)),
            Cls::Stair => (Color32::TRANSPARENT, Stroke::new(t.lw[1], t.projected)),
            Cls::Mep(rgb) => (
                Color32::TRANSPARENT,
                Stroke::new(t.lw[1], super::mep_color::plan(*rgb, t)),
            ),
            // thin line and dashes carry the secondary role: a faded colour would drop under 3:1
            Cls::MepIns(rgb) => (
                Color32::TRANSPARENT,
                Stroke::new(0.7, super::mep_color::plan(*rgb, t)),
            ),
            Cls::MepAxis(rgb) => {
                let col = tone(super::mep_color::plan(*rgb, t));
                for w in s.windows(2) {
                    dashed(p, w[0], w[1], Stroke::new(t.lw[2], col), 8.0, 4.0);
                }
                continue;
            }
            Cls::Shaft => {
                if s.len() >= 3 {
                    p.line_segment([s[0], s[2]], Stroke::new(t.lw[1], t.cut));
                    if s.len() >= 4 {
                        p.line_segment([s[1], s[3]], Stroke::new(t.lw[1], t.cut));
                    }
                }
                (Color32::TRANSPARENT, Stroke::new(t.lw[0], t.cut))
            }
            Cls::Ceiling => {
                for i in 0..s.len() {
                    dashed(
                        p,
                        s[i],
                        s[(i + 1) % s.len()],
                        Stroke::new(t.lw[2], tone(c.faint)),
                        2.0,
                        3.0,
                    );
                }
                continue;
            }
            Cls::Rail | Cls::Comp => (Color32::TRANSPARENT, Stroke::new(t.lw[1], t.projected)),
            Cls::Mass => (t.poche.gamma_multiply(0.45), Stroke::new(t.lw[1], t.cut)),
            Cls::Footing => {
                let n = s.len();
                for i in 0..dash_segs(n, *closed) {
                    let st = Stroke::new(t.lw[1], t.projected);
                    dashed(p, s[i], s[(i + 1) % n], st, 6.0, 4.0);
                }
                continue;
            }
            Cls::MeshOverhead => {
                let n = s.len();
                for i in 0..dash_segs(n, *closed) {
                    let st = Stroke::new(t.lw[2], tone(t.beyond));
                    dashed(p, s[i], s[(i + 1) % n], st, 6.0, 4.0);
                }
                continue;
            }
            Cls::RoomSep => {
                let n = s.len();
                for i in 0..dash_segs(n, *closed) {
                    let st = Stroke::new(1.0, tone(t.projected));
                    dashed(p, s[i], s[(i + 1) % n], st, 7.0, 3.0);
                }
                continue;
            }
            Cls::Zone => (
                c.accent.gamma_multiply(0.1),
                Stroke::new(1.0, c.accent.gamma_multiply(ddc_ui::sem::fade::ZONE_EDGE)),
            ),
        };
        let st = Stroke::new(st.width, tone(st.color));
        if *closed {
            poly_any(p, &s, fill, st);
        } else {
            p.add(egui::Shape::line(s, st));
        }
    }
    for (q, txt) in &l.labels {
        p.text(
            to_s(*q),
            egui::Align2::CENTER_CENTER,
            txt,
            ddc_ui::typo::font(ddc_ui::theme::Role::CanvasLabel),
            t.text_dim,
        );
    }
    for (q, sh) in &l.comps {
        let r = Rect::from_center_size(to_s(*q), Vec2::splat(8.0));
        match sh {
            PhaseShow::Dashed => {
                let c4 = [
                    r.left_top(),
                    r.right_top(),
                    r.right_bottom(),
                    r.left_bottom(),
                    r.left_top(),
                ];
                p.extend(egui::Shape::dashed_line(
                    &c4,
                    Stroke::new(1.2, t.text_dim),
                    3.0,
                    2.0,
                ));
            }
            _ => {
                let col = if *sh == PhaseShow::Halftone {
                    t.projected.gamma_multiply(0.35)
                } else {
                    t.projected
                };
                p.rect_stroke(r, 1.0, Stroke::new(1.2, col), egui::StrokeKind::Middle);
            }
        }
    }
}

fn preview(p: &egui::Painter, ed: &Editor, a: &Active, c: &Pal, to_s: &dyn Fn(V2) -> Pos2) {
    let col = if a.invalid.is_some() { c.bad } else { c.accent };
    let st = Stroke::new(1.5, col);
    let fill = if a.invalid.is_some() {
        c.bad.gamma_multiply(ddc_ui::sem::fade::INVALID)
    } else {
        c.fill
    };
    let font = egui::FontId::proportional(ddc_ui::theme::type_size::BODY);
    let s = |v: &[V2]| v.iter().map(|q| to_s(*q)).collect::<Vec<Pos2>>();
    let cur = a.cur;
    if let Some(h) = &a.mep.hit {
        let (x, y, _) = h.at();
        p.circle_stroke(to_s(v2(x, y)), 9.0, Stroke::new(2.0, col));
    }
    match a.kind {
        Kind::Terminal | Kind::Fixture | Kind::Light | Kind::Device | Kind::Equipment => {
            p.circle(to_s(cur), 6.0, fill, st);
        }
        Kind::Connect | Kind::Disconnect => {}
        Kind::Route => {
            if let Some(&a0) = a.pts.first() {
                let corner = v2(cur.x, a0.y);
                p.add(egui::Shape::line(s(&[a0, corner, cur]), st));
            }
        }
        Kind::Column | Kind::StructColumn => {
            let sec = column::section(a);
            let (b, h) = column::extent(&sec);
            if matches!(sec, ddc_model::MemberShape::Circle { .. }) {
                let r = (to_s(cur + v2(b / 2.0, 0.0)) - to_s(cur)).length();
                p.circle(to_s(cur), r, fill, st);
            } else {
                poly(
                    p,
                    s(&rect_pts(cur, b / 2.0, h / 2.0, a.rotation())),
                    fill,
                    st,
                );
            }
        }
        Kind::Component => {
            let r = rect_pts(cur, 0.4, 0.3, a.rotation());
            // the family's own footprint comes as the session's ghost (`component::ghost`)
            if super::component::ghost(ed, a).is_none() {
                poly(p, s(&r), fill, st);
            }
            let (sn, co) = a.rotation().sin_cos();
            p.arrow(to_s(cur), to_s(cur + v2(-sn, co) * 0.5) - to_s(cur), st);
            let name = a.type_name().split(" : ").last().unwrap_or("").to_string();
            p.text(
                to_s(r[2]) + Vec2::new(6.0, 0.0),
                egui::Align2::LEFT_BOTTOM,
                name,
                font,
                col,
            );
        }
        Kind::Ramp => super::ramp::preview(p, ed, a, c.bad, fill, st, &font, to_s),
        Kind::Zone => super::zone::preview(p, ed, a, fill, st, to_s),
        Kind::Room => super::room::preview(p, a, fill, st, to_s),
        Kind::Beam | Kind::Grid | Kind::RefPlane | Kind::Stair | Kind::RoomSep => {
            let Some(&p0) = a.pts.first() else { return };
            match a.kind {
                Kind::Beam => {
                    let (b, _) = column::extent(&column::section(a));
                    let (sa, sb) = if a.flip { (cur, p0) } else { (p0, cur) };
                    poly(p, s(&band(sa, sb, b)), fill, st);
                    dashed(p, to_s(sa), to_s(sb), Stroke::new(1.0, col), 10.0, 4.0);
                    p.arrow(
                        to_s((sa + sb) * 0.5),
                        (to_s(sb) - to_s(sa)).normalized() * 14.0,
                        st,
                    );
                }
                Kind::Grid => {
                    dashed(p, to_s(p0), to_s(cur), st, 14.0, 4.0);
                    let have: Vec<String> =
                        ed.tool.layer.grids.iter().map(|g| g.0.clone()).collect();
                    let name = super::grid::next_name(
                        &have,
                        super::grid::is_horizontal(super::tup(p0), super::tup(cur)),
                        a.cat.lang == ddc_ui::i18n::Lang::Ru,
                    );
                    let d = (to_s(cur) - to_s(p0)).normalized() * BUBBLE_R;
                    super::grid_bubble::paint(p, to_s(cur) + d, &name, st, st.color, c.ov);
                }
                Kind::RefPlane => {
                    dashed(p, to_s(p0), to_s(cur), Stroke::new(1.5, c.refp), 5.0, 4.0)
                }
                Kind::RoomSep => dashed(p, to_s(p0), to_s(cur), st, 7.0, 3.0),
                Kind::Stair if stair::is_multi(a) && a.pts.len() >= 2 => {
                    stair_multi_preview(p, a, cur, c.bad, fill, st, &font, to_s)
                }
                _ => stair_preview(p, a, p0, cur, fill, st, &font, to_s),
            }
        }
        Kind::Railing | Kind::Duct | Kind::Pipe | Kind::Tray | Kind::Conduit => {
            let mut v = a.pts.clone();
            if v.is_empty() {
                return;
            }
            v.push(cur);
            if a.kind == Kind::Railing {
                p.add(egui::Shape::line(s(&v), Stroke::new(2.0, col)));
                return;
            }
            let w = mep::plan_width(a);
            for seg in v.windows(2) {
                poly(p, s(&band(seg[0], seg[1], w)), fill, st);
                dashed(
                    p,
                    to_s(seg[0]),
                    to_s(seg[1]),
                    Stroke::new(0.8, col),
                    8.0,
                    4.0,
                );
            }
            // elbows at bends (placed by the model on write)
            let elbows = v.len().saturating_sub(2);
            for q in &v[1..v.len() - 1] {
                p.circle(to_s(*q), 7.0, c.fill, Stroke::new(1.5, col));
            }
            if elbows > 0 {
                let lab = a.cat.tf("tool.mep.elbows", &[("n", &elbows.to_string())]);
                p.text(
                    to_s(cur) + Vec2::new(12.0, 12.0),
                    egui::Align2::LEFT_TOP,
                    lab,
                    font,
                    col,
                );
            }
        }
        Kind::Footing
        | Kind::StripFooting
        | Kind::Raft
        | Kind::Brace
        | Kind::Truss
        | Kind::BeamSystem
        | Kind::Gridshell => super::structx::preview(p, a, fill, st, to_s),
        Kind::Roof | Kind::Ceiling | Kind::Shaft | Kind::Floor | Kind::StructSlab => {
            if a.pts.is_empty() {
                super::floor::highlight_walls(p, ed, a, st, to_s);
                if !a.region.is_empty() {
                    let mut sv = s(&a.region);
                    poly_any(p, &sv, fill, st);
                    if a.kind == Kind::Roof {
                        sv.push(sv[0]);
                        roof_marks(p, a, &sv, col, &font);
                    }
                }
                return;
            }
            let mut v = a.pts.clone();
            v.push(cur);
            if super::loop_shapes::active(a) {
                // shape preview: closed ring, so the edge count matches the roof slope flags
                v = super::loop_shapes::ring(a, cur).unwrap_or_else(|_| vec![a.pts[0], cur]);
                if v.len() > 2 {
                    v.push(v[0]);
                }
                if let Some(lab) = super::loop_shapes::dim_label(a, cur) {
                    p.text(
                        to_s(cur) + Vec2::new(12.0, 12.0),
                        egui::Align2::LEFT_TOP,
                        lab,
                        font.clone(),
                        col,
                    );
                }
            }
            let sv = s(&v);
            if v.len() >= 3 {
                poly_any(p, &sv, fill, Stroke::NONE);
            }
            p.add(egui::Shape::line(sv.clone(), st));
            dashed(p, sv[sv.len() - 1], sv[0], Stroke::new(1.0, col), 4.0, 4.0);
            if a.kind == Kind::Roof {
                roof_marks(p, a, &sv, col, &font);
            }
        }
    }
}

/// Slope arrows and labels on the edges of a closed roof outline (`sv` is the ring, closed).
fn roof_marks(p: &egui::Painter, a: &Active, sv: &[Pos2], col: Color32, font: &egui::FontId) {
    let fl = roof::flags(a, sv.len().saturating_sub(1));
    let c = sv.iter().fold(Vec2::ZERO, |acc, q| acc + q.to_vec2()) / sv.len() as f32;
    for (i, f) in fl.iter().enumerate() {
        let (e0, e1) = (sv[i], sv[i + 1]);
        let m = e0 + (e1 - e0) * 0.5;
        let d = (e1 - e0).normalized();
        let mut n = Vec2::new(d.y, -d.x);
        // arrow and label point inside the outline; the label is shifted along the edge so it
        // stays off the edge dimension at its middle and inside the view
        if n.dot(c - m.to_vec2()) < 0.0 {
            n = -n;
        }
        if *f {
            let tri = vec![m + n * 2.0 - d * 6.0, m + n * 2.0 + d * 6.0, m + n * 11.0];
            p.add(egui::Shape::convex_polygon(tri, col, Stroke::NONE));
            let lab = format!("{:.0}°", a.opts.slope_deg);
            let along = ((e1 - e0).length() * 0.25).min(48.0);
            let at = m + n * 16.0 + d * along;
            let align = if n.x.abs() > n.y.abs() {
                if n.x > 0.0 {
                    egui::Align2::LEFT_CENTER
                } else {
                    egui::Align2::RIGHT_CENTER
                }
            } else if n.y > 0.0 {
                egui::Align2::CENTER_TOP
            } else {
                egui::Align2::CENTER_BOTTOM
            };
            p.text(at, align, lab, font.clone(), col);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn stair_preview(
    p: &egui::Painter,
    a: &Active,
    p0: V2,
    cur: V2,
    fill: Color32,
    st: Stroke,
    font: &egui::FontId,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    let d = cur - p0;
    let len = d.len();
    if len < 1e-6 {
        return;
    }
    let u = d * (1.0 / len);
    let o = stair::origin(a, p0, cur);
    let n = v2(-u.y, u.x) * (a.opts.width / 2.0);
    let (placed, total) = stair::count(a, len);
    let run = (placed.saturating_sub(1)) as f64 * stair::TREAD;
    let full = (total.saturating_sub(1)) as f64 * stair::TREAD;
    let ghost = [o + n, o + n + u * full, o - n + u * full, o - n];
    p.add(egui::Shape::closed_line(
        ghost.iter().map(|q| to_s(*q)).collect(),
        Stroke::new(1.0, st.color.gamma_multiply(0.5)),
    ));
    let body = [o + n, o + n + u * run, o - n + u * run, o - n];
    poly(p, body.iter().map(|q| to_s(*q)).collect(), fill, st);
    for k in 1..placed {
        let x = o + u * (k as f64 * stair::TREAD);
        p.line_segment([to_s(x + n), to_s(x - n)], Stroke::new(1.0, st.color));
    }
    p.arrow(
        to_s(o),
        to_s(o + u * run.max(0.3)) - to_s(o),
        Stroke::new(1.2, st.color),
    );
    let lab = a.cat.tf(
        "tool.stair.count",
        &[("n", &placed.to_string()), ("m", &total.to_string())],
    );
    let at = to_s(o + u * run - n) + Vec2::new(10.0, 10.0);
    let r = p.text(at, egui::Align2::LEFT_TOP, lab, font.clone(), st.color);
    p.rect_stroke(
        r.expand(3.0),
        2.0,
        Stroke::new(1.0, st.color),
        egui::StrokeKind::Outside,
    );
}

#[allow(clippy::too_many_arguments)]
fn stair_multi_preview(
    p: &egui::Painter,
    a: &Active,
    cur: V2,
    bad: Color32,
    fill: Color32,
    st: Stroke,
    font: &egui::FontId,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    let (p0, c) = (a.pts[0], a.pts[1]);
    match stair::multi_layout(a, p0, c, cur) {
        Ok(m) => {
            for r in &m.rings {
                poly(p, r.iter().map(|q| to_s(*q)).collect(), fill, st);
            }
            let each: Vec<String> = m.risers.iter().map(|n| n.to_string()).collect();
            let lab = a.cat.tf(
                "tool.stair.flights",
                &[("n", &each.join(" + ")), ("m", &m.total.to_string())],
            );
            p.text(
                to_s(cur) + Vec2::new(10.0, 10.0),
                egui::Align2::LEFT_TOP,
                lab,
                font.clone(),
                st.color,
            );
        }
        Err(msg) => {
            p.text(
                to_s(cur) + Vec2::new(10.0, 10.0),
                egui::Align2::LEFT_TOP,
                msg,
                font.clone(),
                bad,
            );
        }
    }
}

fn temp_dims(
    ui: &mut egui::Ui,
    p: &egui::Painter,
    ed: &mut Editor,
    a: &mut Active,
    c: &Pal,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    for (i, d) in a.dims.clone().iter().enumerate() {
        let (sa, sb) = (to_s(d.a), to_s(d.b));
        if (sb - sa).length() < 4.0 {
            continue;
        }
        let dir = (sb - sa).normalized();
        let n = Vec2::new(-dir.y, dir.x);
        // segment length above it, distance to the reference along the normal
        let off = if i == 0 && a.kind.shape() != Shape::Point && !a.pts.is_empty() {
            n * 18.0
        } else {
            Vec2::ZERO
        };
        let (la, lb) = (sa + off, sb + off);
        overlay::dim_line(p, la, lb, c.dim);
        let editing =
            a.edit == Some(i) || (a.edit.is_none() && i == 0 && !ed.wall_draft().input.is_empty());
        let txt = if editing && !ed.wall_draft().input.is_empty() {
            ed.wall_draft().input.clone()
        } else {
            format_length(d.value, ed.tool.units)
        };
        let mid = la + (lb - la) * 0.5;
        let kind = if editing { Plate::Editing } else { Plate::Temp };
        let r = overlay::dim_label(p, mid, txt, kind, c.ov);
        let id = ui.id().with(("tool_dim", i));
        if ui.interact(r, id, egui::Sense::click()).clicked() {
            a.edit = Some(i);
            ed.wall_draft_mut().input.clear();
        }
    }
    if let Some(r) = &a.invalid {
        let at = to_s(a.cur) + Vec2::new(14.0, 14.0);
        overlay::label_at(p, at, crate::refusals::RefusalText::text(r, &a.cat), Plate::Error, c.ov);
    }
}

/// Entry of the MEP bar, which still passes a unit suffix; it adopts `opt_field` itself later.
pub(super) fn num(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f64,
    suffix: &str,
    range: (f64, f64),
    u: crate::units::Units,
) {
    opt_field::legacy_num(ui, label, v, suffix, range, u);
}

/// Sketch shape switch (key A cycles it) with the polygon fields.
fn shape_options(ui: &mut egui::Ui, a: &mut Active) {
    use super::loop_shapes::SketchShape;
    let cur = a.opts.shape;
    let items: Vec<String> = SketchShape::ALL.iter().map(|s| a.cat.t(s.key())).collect();
    let mut sel = SketchShape::ALL.iter().position(|s| *s == cur).unwrap_or(0);
    let r = opt_field::pick(ui, "sketch_shape", &mut sel, &items, true)
        .on_hover_text(a.cat.t("sketch.shape.tip"));
    if r.changed() {
        super::loop_shapes::set_shape(a, SketchShape::ALL[sel]);
    }
    if a.opts.shape == SketchShape::Polygon {
        opt_field::count(ui, &a.cat.t("sketch.sides"), &mut a.opts.sides, (3, 64));
        opt_field::toggle(ui, &a.cat.t("sketch.circum"), &mut a.opts.circum);
    }
}

/// The option bar of a plan tool routed from the 3D view (`edit3d::plan_route`): build mode,
/// slope, overhang and type stay at hand while the outline is drawn in 3D.
pub fn options_3d(ui: &mut egui::Ui, rect: Rect, ed: &mut Editor, dark: bool) {
    static TOKENS: std::sync::OnceLock<[Tokens; 2]> = std::sync::OnceLock::new();
    let tok = TOKENS.get_or_init(|| {
        let of = |m| crate::ui_kit::from_theme(&ddc_ui::Theme::new(m, ddc_ui::Density::Compact));
        [of(ddc_ui::Mode::Light), of(ddc_ui::Mode::Dark)]
    });
    let t = &tok[dark as usize];
    crate::interact::sessions::model_tool::with(ed, |ed, a| options_bar(ui, rect, ed, a, t));
}

fn options_bar(ui: &mut egui::Ui, rect: Rect, ed: &mut Editor, a: &mut Active, t: &Tokens) {
    // Controls that do not fit the width go to the next line and the bar grows with them: a
    // field is never cut at the pane edge. The background is painted after the height is known.
    let top = Rect::from_min_size(rect.min, Vec2::new(rect.width(), BAR_H * MAX_LINES));
    let p = ui.painter_at(top);
    let back = p.add(egui::Shape::Noop);
    let mut cu = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(top.shrink2(Vec2::new(10.0, 4.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let ui = &mut cu;
    ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
    ui.spacing_mut().interact_size.y = 22.0;
    ui.horizontal_wrapped(|ui| options_row(ui, ed, a, t.paper));
    let h = (ui.min_rect().height() + 8.0).clamp(BAR_H, top.height());
    let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), h));
    p.set(
        back,
        egui::Shape::Vec(vec![
            egui::Shape::rect_filled(bar, 0.0, t.panel),
            egui::Shape::line_segment(
                [bar.left_bottom(), bar.right_bottom()],
                Stroke::new(1.0, t.border),
            ),
        ]),
    );
}

fn options_row(ui: &mut egui::Ui, ed: &mut Editor, a: &mut Active, paper: egui::Color32) {
    let m = " ".to_string() + &a.t("unit.m");
    let u = ed.tool.units;
    // the tool name, level and units sit in the option bar under the ribbon; this row carries
    // only what that bar does not have
    if !a.types.is_empty() {
        ui.label(a.t("opt.type"));
        // built-in «Family : Type» names read in the interface language; the ids stay English
        let lang = a.cat.lang;
        let names: Vec<String> = a.types.iter().map(|n| crate::fam_label::label(lang, n)).collect();
        opt_field::pick(ui, "tool_type", &mut a.opts.ty, &names, false);
    }
    let mut grids = false;
    let mut sketch = false;
    let o = &mut a.opts;
    match a.kind {
        Kind::Floor | Kind::StructSlab => {
            let before = (o.pick, o.walls);
            sketch = super::floor::options(ui, &a.cat, o, u);
            if before != (o.pick, o.walls) {
                super::floor::mode_changed(&mut a.picked, &mut a.pts);
            }
        }
        Kind::Ramp => super::ramp::options(ui, &a.cat, o, a.above.is_some(), u),
        Kind::RoomSep => {
            opt_field::toggle(ui, &a.cat.t("opt.chain"), &mut o.chain);
        }
        Kind::Zone => super::zone::options(ui, &a.cat, o),
        Kind::Room => super::room::options(ui, &a.cat, o),
        Kind::Column | Kind::StructColumn => {
            opt_field::len(ui, &a.cat.t("opt.base_off"), &mut o.base_off, (-10.0, 10.0), u);
            if a.above.is_none() {
                opt_field::len(ui, &a.cat.t("opt.height"), &mut o.top_off, (0.1, 50.0), u);
            } else {
                ui.label(a.cat.t("opt.top_above"));
            }
            opt_field::angle(ui, &a.cat.t("opt.rotation"), &mut o.rot_deg, opt_field::FULL_TURN);
            grids = opt_field::button(
                ui,
                &a.cat.t("opt.at_grids"),
                &a.cat.t("number_input.at_grids_tip"),
            );
        }
        Kind::Beam => {
            opt_field::len(ui, &a.cat.t("opt.z"), &mut o.z, (-10.0, 50.0), u);
            opt_field::toggle(ui, &a.cat.t("opt.chain"), &mut o.chain);
        }
        Kind::Roof => {
            let before = (o.pick, o.walls);
            super::roof::mode_combo(ui, &a.cat, o);
            if before != (o.pick, o.walls) {
                super::roof::mode_switched(o);
                super::floor::mode_changed(&mut a.picked, &mut a.pts);
                a.slopes.clear();
            }
            opt_field::len(ui, &a.cat.t("opt.base_off"), &mut o.base_off, (-10.0, 50.0), u);
            opt_field::len(ui, &a.cat.t("opt.overhang"), &mut o.overhang, (0.0, 3.0), u);
            opt_field::angle(ui, &a.cat.t("opt.slope"), &mut o.slope_deg, (0.0, 75.0));
        }
        Kind::Ceiling => {
            opt_field::len(ui, &a.cat.t("opt.height"), &mut o.height, (0.5, 20.0), u);
            opt_field::toggle(ui, &a.cat.t("opt.auto"), &mut o.pick);
        }
        Kind::Stair => {
            let before = o.stair_shape;
            for (i, k) in ["opt.stair.straight", "opt.stair.l", "opt.stair.u"]
                .iter()
                .enumerate()
            {
                ui.selectable_value(&mut o.stair_shape, i as u8, a.cat.t(k));
            }
            if before != o.stair_shape {
                a.pts.clear();
            }
            if o.stair_shape == 2 {
                opt_field::len(ui, &a.cat.t("opt.stair.gap"), &mut o.gap, (0.0, 2.0), u);
            }
            opt_field::len(ui, &a.cat.t("opt.width"), &mut o.width, (0.6, 4.0), u);
            let js: Vec<String> = ["opt.just.center", "opt.just.left", "opt.just.right"]
                .iter()
                .map(|k| a.cat.t(k))
                .collect();
            opt_field::pick_u8(ui, "tool_just", &mut o.just, &js, true);
            opt_field::toggle(ui, &a.cat.t("opt.auto_rail"), &mut o.auto_rail);
        }
        Kind::Railing => {
            opt_field::len(ui, &a.cat.t("opt.height"), &mut o.height, (0.3, 3.0), u);
            opt_field::toggle(ui, &a.cat.t("opt.pick_host"), &mut o.pick);
        }
        Kind::Shaft => {
            opt_field::len(ui, &a.cat.t("opt.base_off"), &mut o.base_off, (-10.0, 10.0), u);
            if a.above.is_none() {
                opt_field::len(ui, &a.cat.t("opt.height"), &mut o.top_off, (0.1, 100.0), u);
            }
        }
        Kind::Grid | Kind::RefPlane => {
            opt_field::toggle(ui, &a.cat.t("opt.chain"), &mut o.chain);
        }
        Kind::Component => {
            opt_field::angle(ui, &a.cat.t("opt.rotation"), &mut o.rot_deg, opt_field::FULL_TURN);
            if crate::ui_kit::secondary_button(ui, a.cat.t("complib.browse")).clicked() {
                crate::shell::panels::browser::open_library_window();
            }
        }
        Kind::Duct | Kind::Tray => {
            mep::run_system(ui, ed, a.kind, &a.cat, &mut a.mep, paper);
            if a.kind == Kind::Duct {
                ui.checkbox(&mut o.duct_round, a.cat.t("mep.round"));
            }
            if a.kind == Kind::Duct && o.duct_round {
                num(ui, &a.cat.t("mep3d.diameter"), &mut o.width, &m, (0.05, 3.0), u);
            } else {
                num(ui, &a.cat.t("opt.width"), &mut o.width, &m, (0.05, 3.0), u);
                num(ui, &a.cat.t("opt.depth"), &mut o.depth, &m, (0.03, 2.0), u);
            }
            num(ui, &a.cat.t("opt.z"), &mut o.z, &m, (-5.0, 30.0), u);
            if a.kind == Kind::Duct {
                num(
                    ui,
                    &a.cat.t("mep.insulation"),
                    &mut o.insulation,
                    &m,
                    (0.0, 0.2),
                    u,
                );
            }
        }
        Kind::Conduit
        | Kind::Terminal
        | Kind::Fixture
        | Kind::Light
        | Kind::Device
        | Kind::Equipment
        | Kind::Connect
        | Kind::Disconnect
        | Kind::Route => mep::options(ui, ed, a.kind, &a.cat, o, &mut a.mep, &m, paper),
        Kind::Footing
        | Kind::StripFooting
        | Kind::Raft
        | Kind::Brace
        | Kind::Truss
        | Kind::BeamSystem
        | Kind::Gridshell => super::structx::options(ui, a.kind, &a.cat, o, u),
        Kind::Pipe => {
            num(
                ui,
                &a.cat.t("mep.insulation"),
                &mut o.insulation,
                &m,
                (0.0, 0.2),
                u,
            );
            mep::run_system(ui, ed, a.kind, &a.cat, &mut a.mep, paper);
            mep::pipe_options(ui, &a.cat, o, ed.tool.units);
            num(ui, &a.cat.t("opt.z"), &mut o.z, &m, (-5.0, 30.0), u);
            num(
                ui,
                &a.cat.t("opt.slope_pct"),
                &mut o.slope,
                " %",
                (0.0, 10.0),
                u,
            );
        }
    }
    let roof_picks = a.kind == Kind::Roof && a.opts.pick;
    if super::loop_shapes::supports(a.kind) && !a.opts.walls && !roof_picks {
        shape_options(ui, a);
    }
    if grids {
        column::at_grids(ed, a);
    }
    if sketch {
        super::floor::open_sketch(ed, a);
    }
    ui.add_enabled(false, egui::Button::new(a.cat.t("opt.type_props")));
}

#[cfg(test)]
mod tests {
    use super::dash_segs;

    #[test]
    fn dash_segs_guard_short_prims() {
        assert_eq!(dash_segs(0, false), 0);
        assert_eq!(dash_segs(1, false), 0);
        assert_eq!(dash_segs(0, true), 0);
        assert_eq!(dash_segs(2, false), 1);
        assert_eq!(dash_segs(4, true), 4);
    }
}

#[cfg(test)]
mod bubble_tests {
    use super::*;

    /// Six parallel axes 6 px apart end on one line: no two bubbles touch.
    #[test]
    fn dense_grid_bubbles_do_not_touch() {
        let out = egui::vec2(0.0, -BUBBLE_R);
        let ends: Vec<(Pos2, egui::Vec2)> = (0..6)
            .map(|i| (egui::pos2(100.0 + 6.0 * i as f32, 50.0), out))
            .collect();
        let c = place_bubbles(&ends);
        for i in 0..c.len() {
            for j in 0..i {
                assert!(c[i].distance(c[j]) >= 2.0 * BUBBLE_R, "{i} {j}: {c:?}");
            }
            // moved only along its own line
            assert!((c[i].x - ends[i].0.x).abs() < 1e-3);
        }
        assert_eq!(c[0], ends[0].0, "a free bubble stays at its line end");
    }
}
