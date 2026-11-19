//! Live clearance dimensions of a selected plan element: distance from each side of its box to
//! the nearest wall face, plus a pure label placement cascade (rects in, positions out).

use ddc_geom::{v2, V2};
use crate::overlay::{self, Palette, Plate};
use egui::{Color32, Painter, Pos2, Rect, Vec2};

/// Gap kept between a label and its neighbours, px.
pub const GAP: f32 = 3.0;
/// Extra offset of the beside-the-line candidates, px.
pub const SIDE: f32 = 6.0;
/// Clearances are drawn for at most this many selected elements (cost and clutter).
pub const MAX_BODIES: usize = 8;

/// Edges of wall plan outlines (every layer ring, closed): the faces a clearance ends on.
pub fn faces<'a>(outlines: impl IntoIterator<Item = &'a Vec<Vec<V2>>>) -> Vec<(V2, V2)> {
    let mut out = vec![];
    for ring in outlines.into_iter().flatten() {
        let n = ring.len();
        if n < 2 {
            continue;
        }
        out.extend((0..n).map(|i| (ring[i], ring[(i + 1) % n])));
    }
    out
}

/// One clearance in world metres: `from` on the element box, `to` on the wall.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clear {
    pub from: V2,
    pub to: V2,
    pub dist: f64,
}

/// Ray `o + t*d` (unit `d`) against segment `a..b`; t > 0 or None.
fn ray_hit(o: V2, d: V2, a: V2, b: V2) -> Option<f64> {
    let e = b - a;
    let den = d.x * e.y - d.y * e.x;
    if den.abs() < 1e-12 {
        return None;
    }
    let w = a - o;
    let t = (w.x * e.y - w.y * e.x) / den;
    let u = (w.x * d.y - w.y * d.x) / den;
    (t > 1e-6 && (-1e-9..=1.0 + 1e-9).contains(&u)).then_some(t)
}

/// Nearest wall in each of +x, -x, +y, -y from the middle of the matching side of the box
/// `lo..hi`. Directions without a wall are skipped.
pub fn clearances(lo: V2, hi: V2, walls: &[(V2, V2)]) -> Vec<Clear> {
    let c = (lo + hi) * 0.5;
    let sides = [
        (v2(hi.x, c.y), v2(1.0, 0.0)),
        (v2(lo.x, c.y), v2(-1.0, 0.0)),
        (v2(c.x, hi.y), v2(0.0, 1.0)),
        (v2(c.x, lo.y), v2(0.0, -1.0)),
    ];
    let mut out = vec![];
    for (o, d) in sides {
        let best = walls
            .iter()
            .filter_map(|&(a, b)| ray_hit(o, d, a, b))
            .fold(f64::INFINITY, f64::min);
        if best.is_finite() {
            out.push(Clear {
                from: o,
                to: o + d * best,
                dist: best,
            });
        }
    }
    out
}

/// A dimension line in screen space with the size of its label.
#[derive(Clone, Copy, Debug)]
pub struct Req {
    pub a: Pos2,
    pub b: Pos2,
    pub size: Vec2,
}

fn fits(r: Rect, taken: &[Rect]) -> bool {
    !taken.iter().any(|t| t.expand(GAP * 0.5).intersects(r))
}

/// Label centre per request, in order. Candidates: line midpoint, past the far end, beside the
/// line (both sides). The first whose rect misses `fixed` and every label already placed wins;
/// a request with no free candidate gets `None` (the caller drops the whole clearance).
pub fn place(reqs: &[Req], fixed: &[Rect]) -> Vec<Option<Pos2>> {
    let mut taken: Vec<Rect> = fixed.to_vec();
    let mut out = Vec::with_capacity(reqs.len());
    for q in reqs {
        let v = q.b - q.a;
        let dir = if v.length() > 1e-3 { v.normalized() } else { Vec2::X };
        let n = Vec2::new(-dir.y, dir.x);
        let reach = (q.size.x * dir.x.abs() + q.size.y * dir.y.abs()) * 0.5 + GAP;
        let side = (q.size.x * n.x.abs() + q.size.y * n.y.abs()) * 0.5 + SIDE;
        let mid = q.a + v * 0.5;
        let cands = [
            mid,
            q.b + dir * reach,
            mid + n * side,
            mid - n * side,
        ];
        let pick = cands
            .into_iter()
            .find(|&c| fits(Rect::from_center_size(c, q.size), &taken));
        if let Some(c) = pick {
            taken.push(Rect::from_center_size(c, q.size));
        }
        out.push(pick);
    }
    out
}

/// Draws the clearances of the box `lo..hi`; a clearance whose label finds no free spot is
/// dropped together with its line.
#[allow(clippy::too_many_arguments)]
pub fn paint(
    p: &Painter,
    to_s: &dyn Fn(V2) -> Pos2,
    lo: V2,
    hi: V2,
    walls: &[(V2, V2)],
    units: crate::units::Units,
    col: Color32,
    ov: &Palette,
) {
    let cl = clearances(lo, hi, walls);
    let texts: Vec<String> = cl
        .iter()
        .map(|c| crate::units::display_length(c.dist, units))
        .collect();
    let reqs: Vec<Req> = cl
        .iter()
        .zip(&texts)
        .map(|(c, t)| Req {
            a: to_s(c.from),
            b: to_s(c.to),
            size: overlay::dim_label_size(p, t),
        })
        .collect();
    let (sa, sb) = (to_s(lo), to_s(hi));
    let elem = Rect::from_two_pos(sa, sb);
    let pos = place(&reqs, &[elem]);
    for ((q, at), t) in reqs.iter().zip(pos).zip(texts) {
        let Some(at) = at else { continue };
        overlay::dim_line(p, q.a, q.b, col);
        overlay::dim_label(p, at, t, Plate::Temp, ov);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn req(ax: f32, bx: f32) -> Req {
        Req {
            a: pos2(ax, 0.0),
            b: pos2(bx, 0.0),
            size: Vec2::new(40.0, 16.0),
        }
    }

    #[test]
    fn four_sides_hit_box_walls() {
        let w = [
            (v2(-5.0, -5.0), v2(-5.0, 5.0)),
            (v2(5.0, -5.0), v2(5.0, 5.0)),
            (v2(-5.0, 4.0), v2(5.0, 4.0)),
            (v2(-5.0, -3.0), v2(5.0, -3.0)),
        ];
        let c = clearances(v2(-1.0, -1.0), v2(1.0, 1.0), &w);
        let d: Vec<f64> = c.iter().map(|x| x.dist).collect();
        assert_eq!(d, vec![4.0, 4.0, 3.0, 2.0]);
    }

    #[test]
    fn nearest_wall_wins_and_missing_side_is_skipped() {
        let w = [
            (v2(3.0, -5.0), v2(3.0, 5.0)),
            (v2(6.0, -5.0), v2(6.0, 5.0)),
        ];
        let c = clearances(v2(-1.0, -1.0), v2(1.0, 1.0), &w);
        assert_eq!(c.len(), 1);
        assert!((c[0].dist - 2.0).abs() < 1e-9);
    }

    #[test]
    fn clearance_ends_on_the_wall_face_not_the_axis() {
        // a 0.2 m wall on the axis x = 3.1: its near face is x = 3.0
        let wall = vec![vec![v2(3.0, -5.0), v2(3.2, -5.0), v2(3.2, 5.0), v2(3.0, 5.0)]];
        let f = faces([&wall]);
        assert_eq!(f.len(), 4, "a closed ring: every edge");
        let c = clearances(v2(-1.0, -1.0), v2(1.0, 1.0), &f);
        assert_eq!(c.len(), 1);
        assert!((c[0].dist - 2.0).abs() < 1e-9, "to the face: {}", c[0].dist);
        assert!((c[0].to.x - 3.0).abs() < 1e-9);
    }

    #[test]
    fn midpoint_first_when_free() {
        let p = place(&[req(0.0, 100.0)], &[]);
        assert_eq!(p, vec![Some(pos2(50.0, 0.0))]);
    }

    #[test]
    fn overlap_with_element_moves_to_far_end() {
        let el = Rect::from_center_size(pos2(50.0, 0.0), Vec2::new(30.0, 30.0));
        let p = place(&[req(0.0, 100.0)], &[el])[0].unwrap();
        assert!(p.x > 100.0);
        assert!(!Rect::from_center_size(p, Vec2::new(40.0, 16.0)).intersects(el));
    }

    #[test]
    fn cascade_goes_beside_the_line_after_far_end() {
        let fixed = [
            Rect::from_center_size(pos2(50.0, 0.0), Vec2::new(30.0, 6.0)),
            Rect::from_center_size(pos2(125.0, 0.0), Vec2::new(60.0, 30.0)),
        ];
        let p = place(&[req(0.0, 100.0)], &fixed)[0].unwrap();
        assert!((p.x - 50.0).abs() < 1e-3 && p.y > 0.0);
    }

    #[test]
    fn placed_labels_block_each_other() {
        let p = place(&[req(0.0, 100.0), req(0.0, 100.0)], &[]);
        let (a, b) = (p[0].unwrap(), p[1].unwrap());
        let sz = Vec2::new(40.0, 16.0);
        assert!(!Rect::from_center_size(a, sz).intersects(Rect::from_center_size(b, sz)));
    }

    #[test]
    fn dropped_when_every_candidate_is_taken() {
        let big = Rect::from_center_size(pos2(50.0, 0.0), Vec2::new(600.0, 600.0));
        assert_eq!(place(&[req(0.0, 100.0)], &[big]), vec![None]);
    }

    #[test]
    fn drop_does_not_consume_space() {
        let big = Rect::from_center_size(pos2(50.0, 0.0), Vec2::new(600.0, 600.0));
        let p = place(&[req(0.0, 100.0), req(0.0, 100.0)], &[big]);
        assert_eq!(p, vec![None, None]);
    }
}
