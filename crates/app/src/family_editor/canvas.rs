//! Plan sketch: x/y reference planes (free ones in the accent color, under-constrained ones with a
//! warning), reference lines, dimensions and EQ, body outlines, the insertion point. A plane is
//! dragged with the mouse (a free one gets a new initial position, the sketch is re-solved);
//! the wheel zooms, the middle button pans, a double click fits.

use ddc_geom::component::ext::{AngleSrc, Cons};
use ddc_geom::component::{Axis, Family, Resolved};
use ddc_geom::sketch_solve::Verdict;
use ddc_ui::{Catalog, Theme};
use egui::{self, Align2, Color32, FontId, Pos2, Sense, Stroke};

use super::Session;
use ddc_geom::component::doc::mm;

#[derive(Clone, Debug, Default)]
pub struct View {
    /// Pixels per meter; 0 means fit on the next frame.
    pub scale: f32,
    pub center: [f64; 2],
    /// The plane being dragged: name, x axis, current position.
    pub drag: Option<(String, bool, f64)>,
}

struct Plane {
    name: String,
    x: bool,
    at: f64,
    free: bool,
}

fn planes(f: &Family, r: &Resolved) -> Vec<Plane> {
    let mut v = vec![];
    for p in &f.refs {
        if p.axis != Axis::Z {
            if let Some(at) = r.num(&p.name) {
                v.push(Plane {
                    name: p.name.clone(),
                    x: p.axis == Axis::X,
                    at,
                    free: false,
                });
            }
        }
    }
    for p in &f.ext.free_refs {
        if p.axis != Axis::Z {
            v.push(Plane {
                name: p.name.clone(),
                x: p.axis == Axis::X,
                at: r.num(&p.name).unwrap_or(p.guess),
                free: true,
            });
        }
    }
    v
}

/// Drawing; `Some((plane, position))` means the drag is finished.
pub fn show(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    s: &mut Session,
) -> Option<(String, f64)> {
    let size = egui::vec2(
        ui.available_width(),
        ui.available_height().clamp(260.0, 640.0),
    );
    let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
    let rect = resp.rect;
    painter.rect_filled(rect, ddc_ui::theme::radius::MD, th.c("canvas.paper"));
    let (Some(f), Some(r0)) = (s.eval.family.as_ref(), s.eval.resolved.as_ref()) else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            cat.t("fe.canvas.none"),
            FontId::proportional(ddc_ui::theme::type_size::BODY),
            th.c("text.secondary"),
        );
        return None;
    };
    // the check-mode probe is shown instead of the type
    let flex = s
        .flex
        .last
        .as_ref()
        .filter(|_| s.flex.show)
        .and_then(|o| o.resolved.as_ref().map(|r| (r, o.built.as_ref())));
    let (r, built) = flex.unwrap_or((r0, s.eval.built.as_ref()));
    let pl = planes(f, r);
    let origin = f.ext.origin_at(r).unwrap_or_default();
    let plan: Vec<[f64; 4]> = built
        .map(|b| {
            b.plan
                .iter()
                .map(|(a, c)| {
                    [
                        a.x + origin.x,
                        a.y + origin.y,
                        c.x + origin.x,
                        c.y + origin.y,
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    let v = &mut s.view;
    // fit
    if v.scale <= 0.0 || resp.double_clicked() {
        let (mut lo, mut hi) = ([-0.5f64, -0.5], [0.5f64, 0.5]);
        let mut grow = |x: f64, y: f64| {
            lo = [lo[0].min(x), lo[1].min(y)];
            hi = [hi[0].max(x), hi[1].max(y)];
        };
        for l in &plan {
            grow(l[0], l[1]);
            grow(l[2], l[3]);
        }
        for p in &pl {
            if p.x {
                grow(p.at, 0.0);
            } else {
                grow(0.0, p.at);
            }
        }
        let ext = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(0.1);
        v.scale = (rect.width().min(rect.height()) * 0.75) / ext as f32;
        v.center = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
    }
    if resp.hovered() {
        let dy = ui.input(|i| i.smooth_scroll_delta.y);
        if dy != 0.0 {
            v.scale = (v.scale * (1.0 + dy * 0.002)).clamp(1.0, 1e5);
        }
    }
    if resp.dragged_by(egui::PointerButton::Middle) {
        let d = resp.drag_delta();
        v.center[0] -= (d.x / v.scale) as f64;
        v.center[1] += (d.y / v.scale) as f64;
    }
    let (sc, cx, cy, mid) = (v.scale as f64, v.center[0], v.center[1], rect.center());
    let to = |x: f64, y: f64| {
        Pos2::new(
            mid.x + ((x - cx) * sc) as f32,
            mid.y - ((y - cy) * sc) as f32,
        )
    };
    let from = |p: Pos2| {
        (
            cx + ((p.x - mid.x) as f64) / sc,
            cy - ((p.y - mid.y) as f64) / sc,
        )
    };
    let painter = painter.with_clip_rect(rect);
    // body outlines
    let line = Stroke::new(1.4, th.c("text.primary"));
    for l in &plan {
        painter.line_segment([to(l[0], l[1]), to(l[2], l[3])], line);
    }
    // planes
    let rig = s.eval.rig.as_ref();
    // a plane in an inconsistent set: by constraint indices from the solver
    let bad = |n: &str| {
        rig.is_some_and(|g| {
            g.conflicting_ids
                .iter()
                .any(|(k, _)| f.ext.cons.get(*k).is_some_and(|c| c.names().contains(&n)))
        })
    };
    let under = |n: &str| rig.is_some_and(|g| g.free.iter().any(|x| x == n));
    let font = FontId::proportional(ddc_ui::theme::type_size::CAPTION);
    let mut drag_end = None;
    let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
    // hit-testing uses the press point: by the drag start the pointer has already moved
    let press = ui.input(|i| i.pointer.press_origin()).or(pointer);
    if resp.drag_started_by(egui::PointerButton::Primary) {
        if let Some(p) = press {
            let near = pl
                .iter()
                .map(|q| {
                    let s = to(if q.x { q.at } else { 0.0 }, if q.x { 0.0 } else { q.at });
                    ((if q.x { s.x - p.x } else { s.y - p.y }).abs(), q)
                })
                .filter(|(d, _)| *d < 6.0)
                .min_by(|a, b| a.0.total_cmp(&b.0));
            v.drag = near.map(|(_, q)| (q.name.clone(), q.x, q.at));
        }
    }
    if let (Some((_, x, at)), Some(p)) = (v.drag.as_mut(), pointer) {
        if resp.dragged_by(egui::PointerButton::Primary) {
            let w = from(p);
            *at = if *x { w.0 } else { w.1 };
        }
    }
    if resp.drag_stopped() {
        if let Some((n, _, at)) = v.drag.take() {
            drag_end = Some((n, (at * 1e4).round() / 1e4));
        }
    }
    let under_ptr = match (&v.drag, resp.hover_pos()) {
        (Some((_, x, _)), _) => Some(*x),
        (None, Some(p)) => pl
            .iter()
            .find(|q| {
                let s = to(if q.x { q.at } else { 0.0 }, if q.x { 0.0 } else { q.at });
                (if q.x { s.x - p.x } else { s.y - p.y }).abs() < 6.0
            })
            .map(|q| q.x),
        _ => None,
    };
    let panning = resp.dragged_by(egui::PointerButton::Middle);
    if let Some(i) = crate::nav_cursor::canvas_cursor(panning, under_ptr) {
        ui.ctx().set_cursor_icon(i);
    }
    let dragging = v.drag.clone();
    for q in &pl {
        let at = match &dragging {
            Some((n, _, a)) if *n == q.name => *a,
            _ => q.at,
        };
        let col = if bad(&q.name) {
            th.c("error")
        } else if under(&q.name) {
            th.c("warning")
        } else if q.free {
            th.c("accent")
        } else {
            th.c("text.secondary")
        };
        let (a, b) = if q.x {
            (
                Pos2::new(to(at, 0.0).x, rect.top()),
                Pos2::new(to(at, 0.0).x, rect.bottom()),
            )
        } else {
            (
                Pos2::new(rect.left(), to(0.0, at).y),
                Pos2::new(rect.right(), to(0.0, at).y),
            )
        };
        painter.extend(egui::Shape::dashed_line(
            &[a, b],
            Stroke::new(1.0, col),
            6.0,
            4.0,
        ));
        let lp = if q.x {
            a + egui::vec2(3.0, 2.0)
        } else {
            a + egui::vec2(3.0, -14.0)
        };
        painter.text(lp, Align2::LEFT_TOP, &q.name, font.clone(), col);
    }
    // reference lines
    for l in &f.ext.lines {
        let env = |n: &str| r.nums.get(n).copied();
        let (Ok(x), Ok(y)) = (l.at[0].eval(&env), l.at[1].eval(&env)) else {
            continue;
        };
        let ang = match &l.angle {
            AngleSrc::Expr(e) => e.eval(&env).map(|q| q.v).ok(),
            AngleSrc::Free(g) => Some(r.num(&l.name).unwrap_or(*g)),
        };
        let Some(ang) = ang else { continue };
        let k = (rect.width().max(rect.height()) as f64) / sc;
        let (dx, dy) = (ang.cos() * k, ang.sin() * k);
        let col = if bad(&l.name) {
            th.c("error")
        } else {
            th.c("accent")
        };
        let (p0, p1) = (to(x.v - dx, y.v - dy), to(x.v + dx, y.v + dy));
        painter.extend(egui::Shape::dashed_line(
            &[p0, p1],
            Stroke::new(1.0, col),
            6.0,
            4.0,
        ));
        painter.text(
            to(x.v, y.v),
            Align2::LEFT_BOTTOM,
            &l.name,
            font.clone(),
            col,
        );
    }
    // dimensions and equalities
    let at_of = |n: &str| pl.iter().find(|q| q.name == n);
    let dim_col = th.c("accent");
    let mut row = 0f32;
    for c in &f.ext.cons {
        match c {
            Cons::Dim { a, b, .. } => {
                let (Some(qa), Some(qb)) = (at_of(a), at_of(b)) else {
                    continue;
                };
                row += 1.0;
                let label = format!("{} = {}", value_label(c), mm(qb.at - qa.at));
                if qa.x {
                    let y = rect.top() + 8.0 + 16.0 * row;
                    let (p0, p1) = (
                        Pos2::new(to(qa.at, 0.0).x, y),
                        Pos2::new(to(qb.at, 0.0).x, y),
                    );
                    arrow(&painter, p0, p1, dim_col);
                    painter.text(
                        Pos2::new((p0.x + p1.x) / 2.0, y - 2.0),
                        Align2::CENTER_BOTTOM,
                        label,
                        font.clone(),
                        dim_col,
                    );
                } else {
                    let x = rect.right() - 8.0 - 16.0 * row;
                    let (p0, p1) = (
                        Pos2::new(x, to(0.0, qa.at).y),
                        Pos2::new(x, to(0.0, qb.at).y),
                    );
                    arrow(&painter, p0, p1, dim_col);
                    painter.text(
                        Pos2::new(x - 3.0, (p0.y + p1.y) / 2.0),
                        Align2::RIGHT_CENTER,
                        label,
                        font.clone(),
                        dim_col,
                    );
                }
            }
            Cons::Eq(names) => {
                row += 1.0;
                let mut qs: Vec<&Plane> = names.iter().filter_map(|n| at_of(n)).collect();
                qs.sort_by(|a, b| a.at.total_cmp(&b.at));
                for w in qs.windows(2) {
                    let (pa, pb) = if w[0].x {
                        let y = rect.top() + 8.0 + 16.0 * row;
                        (
                            Pos2::new(to(w[0].at, 0.0).x, y),
                            Pos2::new(to(w[1].at, 0.0).x, y),
                        )
                    } else {
                        let x = rect.right() - 8.0 - 16.0 * row;
                        (
                            Pos2::new(x, to(0.0, w[0].at).y),
                            Pos2::new(x, to(0.0, w[1].at).y),
                        )
                    };
                    arrow(&painter, pa, pb, dim_col);
                    painter.text(
                        Pos2::new((pa.x + pb.x) / 2.0, (pa.y + pb.y) / 2.0 - 2.0),
                        Align2::CENTER_BOTTOM,
                        cat.t("fe.canvas.eq"),
                        font.clone(),
                        dim_col,
                    );
                }
            }
            Cons::Align(a, b) => {
                if let (Some(qa), Some(qb)) = (at_of(a), at_of(b)) {
                    let p = to(
                        if qa.x { qa.at } else { 0.0 },
                        if qa.x { 0.0 } else { qa.at },
                    );
                    let p = if qa.x {
                        Pos2::new(p.x, rect.bottom() - 14.0)
                    } else {
                        Pos2::new(rect.left() + 14.0, p.y)
                    };
                    painter.rect_stroke(
                        egui::Rect::from_center_size(p, egui::vec2(8.0, 8.0)),
                        1.0,
                        Stroke::new(1.2, dim_col),
                        egui::StrokeKind::Middle,
                    );
                    let _ = qb;
                }
            }
            Cons::Angle { a, b, .. } => {
                if let Some(l) = f.ext.lines.iter().find(|l| l.name == *b || l.name == *a) {
                    let env = |n: &str| r.nums.get(n).copied();
                    if let (Ok(x), Ok(y)) = (l.at[0].eval(&env), l.at[1].eval(&env)) {
                        painter.text(
                            to(x.v, y.v) + egui::vec2(10.0, 10.0),
                            Align2::LEFT_TOP,
                            value_label(c),
                            font.clone(),
                            dim_col,
                        );
                    }
                }
            }
        }
    }
    // insertion point
    let o = to(origin.x, origin.y);
    let oc = th.c("canvas.cut");
    painter.circle_stroke(o, 5.0, Stroke::new(1.5, oc));
    painter.line_segment(
        [o - egui::vec2(8.0, 0.0), o + egui::vec2(8.0, 0.0)],
        Stroke::new(1.0, oc),
    );
    painter.line_segment(
        [o - egui::vec2(0.0, 8.0), o + egui::vec2(0.0, 8.0)],
        Stroke::new(1.0, oc),
    );
    // z planes are shown as a label
    let z: Vec<String> = f
        .refs
        .iter()
        .filter(|p| p.axis == Axis::Z)
        .filter_map(|p| r.num(&p.name).map(|v| format!("{} {}", p.name, mm(v))))
        .chain(
            f.ext
                .free_refs
                .iter()
                .filter(|p| p.axis == Axis::Z)
                .map(|p| format!("{} {}", p.name, mm(r.num(&p.name).unwrap_or(p.guess)))),
        )
        .collect();
    if !z.is_empty() {
        painter.text(
            rect.left_bottom() + egui::vec2(6.0, -6.0),
            Align2::LEFT_BOTTOM,
            cat.tf("fe.canvas.z", &[("list", &z.join(", "))]),
            font.clone(),
            th.c("text.secondary"),
        );
    }
    if let Some(g) = rig.filter(|g| g.verdict == Verdict::Conflict) {
        let l: Vec<&str> = g.conflicting.iter().map(|c| c.0.as_str()).collect();
        painter.text(
            rect.left_top() + egui::vec2(6.0, 6.0),
            Align2::LEFT_TOP,
            l.join("; "),
            font,
            th.c("error"),
        );
    }
    if let Some((n, _, at)) = &dragging {
        resp.on_hover_text(format!("{n}: {}", mm(*at)));
    }
    drag_end
}

fn value_label(c: &Cons) -> String {
    let l = c.label();
    l.split_whitespace().nth(1).unwrap_or("").to_string()
}

fn arrow(p: &egui::Painter, a: Pos2, b: Pos2, col: Color32) {
    let st = Stroke::new(1.0, col);
    p.line_segment([a, b], st);
    let d = (b - a).normalized() * 5.0;
    let n = egui::vec2(-d.y, d.x) * 0.6;
    for (tip, dir) in [(a, d), (b, -d)] {
        p.line_segment([tip, tip + dir + n], st);
        p.line_segment([tip, tip + dir - n], st);
    }
}
