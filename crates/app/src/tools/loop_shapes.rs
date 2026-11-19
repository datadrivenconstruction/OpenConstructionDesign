//! Sketch shapes for loop tools (floor, slab, roof, ceiling, shaft): pure ring builders.
//! The ring then goes through the normal finish path (CCW, one undo step).

use super::{Active, Kind};
use ddc_geom::arc::CHORD_SAG;
use ddc_geom::{v2, V2};
use std::f64::consts::PI;

/// Smallest accepted side or radius, m.
pub const MIN_SIZE: f64 = 0.01;
const MIN_SEGMENTS: usize = 12;
const MAX_SEGMENTS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SketchShape {
    /// Point by point (the classic way).
    #[default]
    Points,
    Rect,
    Circle,
    Polygon,
}

impl SketchShape {
    pub const ALL: [SketchShape; 4] = [
        SketchShape::Points,
        SketchShape::Rect,
        SketchShape::Circle,
        SketchShape::Polygon,
    ];

    pub fn key(self) -> &'static str {
        match self {
            SketchShape::Points => "sketch.points",
            SketchShape::Rect => "sketch.rect",
            SketchShape::Circle => "sketch.circle",
            SketchShape::Polygon => "sketch.polygon",
        }
    }

    pub fn next(self) -> SketchShape {
        let i = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// Tools that take a sketch shape.
pub fn supports(k: Kind) -> bool {
    matches!(
        k,
        Kind::Floor | Kind::StructSlab | Kind::Roof | Kind::Ceiling | Kind::Shaft
    )
}

/// A shape (not point-by-point) is drawn now.
pub fn active(a: &Active) -> bool {
    supports(a.kind) && a.opts.shape != SketchShape::Points && !a.opts.walls
}

/// Switch the shape; the picking modes are left because a shape needs free clicks.
pub fn set_shape(a: &mut Active, s: SketchShape) {
    if a.opts.shape == s {
        return;
    }
    a.opts.shape = s;
    if s != SketchShape::Points {
        a.opts.pick = false;
        a.opts.walls = false;
    }
    a.pts.clear();
    a.slopes.clear();
}

/// Axis-aligned rectangle from two opposite corners, CCW.
pub fn rect_two_corners(c0: V2, c1: V2) -> Result<Vec<V2>, &'static str> {
    let d = c1 - c0;
    if d.x.abs() < MIN_SIZE || d.y.abs() < MIN_SIZE {
        return Err("tool.err.zero_size");
    }
    let mut r = vec![c0, v2(c1.x, c0.y), c1, v2(c0.x, c1.y)];
    super::ccw(&mut r);
    Ok(r)
}

/// Rectangle from a corner and typed width and height; their signs choose the quadrant.
pub fn rect_sized(c0: V2, w: f64, h: f64) -> Result<Vec<V2>, &'static str> {
    rect_two_corners(c0, v2(c0.x + w, c0.y + h))
}

/// Segment count for a circle of radius `r` so the chord sag stays within `CHORD_SAG`.
pub fn circle_segments(r: f64) -> usize {
    if r <= CHORD_SAG {
        return MIN_SEGMENTS;
    }
    let step = 2.0 * (1.0 - CHORD_SAG / r).acos();
    ((2.0 * PI / step).ceil() as usize).clamp(MIN_SEGMENTS, MAX_SEGMENTS)
}

/// Circle as a polygon, CCW; the first vertex lies towards `through`.
pub fn circle_ring(c: V2, through: V2) -> Result<Vec<V2>, &'static str> {
    let d = through - c;
    let r = d.len();
    if r < MIN_SIZE {
        return Err("tool.err.zero_size");
    }
    Ok(regular(c, r, d.y.atan2(d.x), circle_segments(r)))
}

/// Regular polygon with `n` sides. Inscribed: `through` is a vertex. Circumscribed: `through`
/// is the middle of a side (the distance is the apothem).
pub fn polygon_ring(c: V2, through: V2, n: usize, circum: bool) -> Result<Vec<V2>, &'static str> {
    let d = through - c;
    let len = d.len();
    if len < MIN_SIZE || n < 3 {
        return Err("tool.err.zero_size");
    }
    let ang = d.y.atan2(d.x);
    Ok(if circum {
        regular(c, len / (PI / n as f64).cos(), ang - PI / n as f64, n)
    } else {
        regular(c, len, ang, n)
    })
}

fn regular(c: V2, r: f64, a0: f64, n: usize) -> Vec<V2> {
    (0..n)
        .map(|i| {
            let t = a0 + 2.0 * PI * i as f64 / n as f64;
            v2(c.x + t.cos() * r, c.y + t.sin() * r)
        })
        .collect()
}

/// Ring of the active shape from the first point and the cursor.
pub fn ring(a: &Active, cur: V2) -> Result<Vec<V2>, &'static str> {
    let Some(&p0) = a.pts.first() else {
        return Err("tool.err.short");
    };
    match a.opts.shape {
        SketchShape::Points => Err("tool.err.short"),
        SketchShape::Rect => rect_two_corners(p0, cur),
        SketchShape::Circle => circle_ring(p0, cur),
        SketchShape::Polygon => polygon_ring(p0, cur, a.opts.sides, a.opts.circum),
    }
}

/// Live dimension label for the preview.
pub fn dim_label(a: &Active, cur: V2) -> Option<String> {
    let p0 = *a.pts.first()?;
    let d = cur - p0;
    Some(match a.opts.shape {
        SketchShape::Points => return None,
        SketchShape::Rect => format!("{:.2} x {:.2}", d.x.abs(), d.y.abs()),
        _ => format!("{} {:.2}", a.t("sketch.radius"), d.len()),
    })
}

/// A click while a shape is active: the first sets the corner or centre, the second builds the
/// ring and hands it to the normal finish path.
pub(super) fn click(ed: &mut crate::editor::Editor, a: &mut Active, q: V2) {
    if a.pts.is_empty() {
        a.pts.push(q);
        return;
    }
    match ring(a, q) {
        Ok(r) => {
            a.slopes = vec![true; r.len()];
            a.pts = r;
            super::finish(ed, a);
            // a refused commit must not leave the ring behind as clicked points
            a.pts.clear();
            a.slopes.clear();
        }
        Err(k) => ed.status = Some(a.t(k)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(r: &[V2]) -> f64 {
        (0..r.len())
            .map(|i| r[i].cross(r[(i + 1) % r.len()]))
            .sum::<f64>()
            / 2.0
    }

    fn perim(r: &[V2]) -> f64 {
        (0..r.len())
            .map(|i| (r[(i + 1) % r.len()] - r[i]).len())
            .sum()
    }

    #[test]
    fn rect_two_corners_ccw_ring() {
        for (a, b) in [(v2(0.0, 0.0), v2(6.0, 4.0)), (v2(6.0, 4.0), v2(0.0, 0.0))] {
            let r = rect_two_corners(a, b).unwrap();
            assert_eq!(r.len(), 4);
            assert!((area(&r) - 24.0).abs() < 1e-9, "CCW, area 24");
            assert!((perim(&r) - 20.0).abs() < 1e-9);
        }
    }

    #[test]
    fn rect_typed_size() {
        let r = rect_sized(v2(1.0, 1.0), 3.0, -2.0).unwrap();
        assert!((area(&r) - 6.0).abs() < 1e-9);
        assert!(r.contains(&v2(4.0, -1.0)));
    }

    #[test]
    fn circle_segments_follow_sag() {
        assert_eq!(circle_segments(0.01), MIN_SEGMENTS);
        assert!(circle_segments(5.0) > circle_segments(1.0));
        let r = 3.0;
        let n = circle_segments(r);
        let sag = r * (1.0 - (PI / n as f64).cos());
        assert!(sag <= CHORD_SAG * 1.0001, "sag {sag}");
        let ring = circle_ring(v2(0.0, 0.0), v2(r, 0.0)).unwrap();
        assert_eq!(ring.len(), n);
        assert!((area(&ring) - PI * r * r).abs() / (PI * r * r) < 1e-3);
    }

    #[test]
    fn polygon_inscribed_vs_circumscribed() {
        let c = v2(0.0, 0.0);
        let ins = polygon_ring(c, v2(2.0, 0.0), 6, false).unwrap();
        let cir = polygon_ring(c, v2(2.0, 0.0), 6, true).unwrap();
        assert!((ins[0] - v2(2.0, 0.0)).len() < 1e-9, "vertex at the click");
        assert!((area(&ins) - 1.5 * 3f64.sqrt() * 4.0).abs() < 1e-9);
        let mid = v2((cir[0].x + cir[1].x) / 2.0, (cir[0].y + cir[1].y) / 2.0);
        assert!((mid - v2(2.0, 0.0)).len() < 1e-9, "apothem is 2");
        assert!(area(&cir) > area(&ins));
    }

    #[test]
    fn zero_size_refused() {
        let p = v2(1.0, 1.0);
        assert!(rect_two_corners(p, p).is_err());
        assert!(rect_two_corners(p, v2(5.0, 1.0)).is_err(), "no height");
        assert!(circle_ring(p, p).is_err());
        assert!(polygon_ring(p, p, 5, false).is_err());
        assert!(polygon_ring(p, v2(2.0, 1.0), 2, false).is_err());
    }
}
