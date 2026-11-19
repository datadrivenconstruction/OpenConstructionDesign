//! Internal dimensions of a plan's implicit chains: the openings of walls that do not stand on
//! the building's extent (partitions, set-back walls) measured along the wall, and the clear
//! size of every room along two chords. They carry `chain >= INNER_CHAIN`: the sheet draws them
//! right at their points instead of on a tier outside the extent, and drops those whose text
//! cannot be placed clear of other text. A skew wall is measured along its lower face (openings
//! included); an arc wall by its arc length at the outer face and its radius.

use crate::views::{Side, ViewPrim, WallRow};
use crate::{Error, Model};

/// First `chain` value of an internal dimension.
pub const INNER_CHAIN: u8 = 16;
/// `chain` of a radius: the sheet writes an `R` before its value.
pub const RADIUS_CHAIN: u8 = INNER_CHAIN + 1;
/// Pieces shorter than this are not dimensioned, m.
const MIN_PIECE: f64 = 0.05;
/// Clear lengths of a room shorter than this get no dimension, m.
const MIN_CHORD: f64 = 0.5;
/// Where the chords cross a room, as a share of its extent from the low side: off the centre,
/// where the room's tag stands.
const CHORD_AT: f64 = 0.2;
/// Tolerance of a wall face lying on a side of the extent, m.
const EPS: f64 = 1e-3;

/// Opening chains of the straight, axis-aligned walls not on the extent: wall end, opening
/// edges, wall end along the lower face. `ops` are (wall index, along, width).
pub(crate) fn opening_chains(
    walls: &[WallRow],
    ops: &[(usize, f64, f64)],
    bb: [f64; 4],
) -> Vec<ViewPrim> {
    let mut out = vec![];
    for (i, w) in walls.iter().enumerate() {
        if w.arc.is_some() || !ops.iter().any(|o| o.0 == i) {
            continue;
        }
        let (dx, dy) = (w.b.0 - w.a.0, w.b.1 - w.a.1);
        let len = dx.hypot(dy);
        if len < 1e-9 {
            continue;
        }
        let d = (dx / len, dy / len);
        let horiz = d.1.abs() < 1e-9;
        if !horiz && d.0.abs() > 1e-9 {
            continue;
        }
        let n = (-d.1, d.0);
        let across = |f: f64| {
            let q = (w.a.0 + n.0 * f, w.a.1 + n.1 * f);
            if horiz {
                q.1
            } else {
                q.0
            }
        };
        let faces = [across(w.offset0), across(w.offset0 + w.total)];
        let sides = if horiz { [bb[1], bb[3]] } else { [bb[0], bb[2]] };
        if faces.iter().any(|f| sides.iter().any(|s| (f - s).abs() < EPS)) {
            continue; // the outer chain already measures it
        }
        let face = faces[0].min(faces[1]);
        let along = |t: f64| {
            let q = (w.a.0 + d.0 * t, w.a.1 + d.1 * t);
            if horiz {
                q.0
            } else {
                q.1
            }
        };
        let mut pts = vec![along(0.0), along(len)];
        for &(_, a, wd) in ops.iter().filter(|o| o.0 == i) {
            pts.push(along(a));
            pts.push(along(a + wd));
        }
        pts.sort_by(f64::total_cmp);
        pts.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        for k in 1..pts.len() {
            let v = pts[k] - pts[k - 1];
            if v < MIN_PIECE {
                continue;
            }
            let at = |t: f64| if horiz { (t, face) } else { (face, t) };
            out.push(ViewPrim::Dim {
                side: if horiz { Side::South } else { Side::West },
                chain: INNER_CHAIN,
                a: at(pts[k - 1]),
                b: at(pts[k]),
                value: v,
            });
        }
    }
    out
}

/// The side of the extent a dimension of direction-free `n` (unit) faces: its dominant axis.
fn side_of(n: (f64, f64)) -> Side {
    if n.1.abs() >= n.0.abs() {
        if n.1 < 0.0 { Side::South } else { Side::North }
    } else if n.0 < 0.0 {
        Side::West
    } else {
        Side::East
    }
}

/// Chains of the skew walls (wall end, opening edges, wall end along the face looking down
/// and left) and, for arc walls, the arc length at the outer face and the radius. `ops` are
/// (wall index, along, width); openings on an arc wall are not split off.
pub(crate) fn oblique_chains(walls: &[WallRow], ops: &[(usize, f64, f64)]) -> Vec<ViewPrim> {
    let mut out = vec![];
    for (i, w) in walls.iter().enumerate() {
        let faces = [w.offset0, w.offset0 + w.total];
        if let Some(ax) = &w.arc {
            let len = ax.length();
            if len < MIN_PIECE || ax.radius < 1e-9 {
                continue;
            }
            let outer = if ax.radius_at(faces[0]) >= ax.radius_at(faces[1]) { faces[0] } else { faces[1] };
            let (p0, p1, pm) = (w.at(0.0, outer), w.at(len, outer), w.at(len / 2.0, outer));
            let r = ax.radius_at(outer);
            let dir = (pm.0 - ax.center.x, pm.1 - ax.center.y);
            let dl = dir.0.hypot(dir.1).max(1e-12);
            let n = (dir.0 / dl, dir.1 / dl);
            out.push(ViewPrim::Dim {
                side: side_of(n),
                chain: INNER_CHAIN,
                a: p0,
                b: p1,
                value: len / ax.radius * r,
            });
            out.push(ViewPrim::Dim {
                side: side_of((-n.1, n.0)),
                chain: RADIUS_CHAIN,
                a: (ax.center.x, ax.center.y),
                b: pm,
                value: r,
            });
            continue;
        }
        let (dx, dy) = (w.b.0 - w.a.0, w.b.1 - w.a.1);
        let len = dx.hypot(dy);
        if len < 1e-9 || dx.abs() < 1e-9 * len || dy.abs() < 1e-9 * len {
            continue; // axis-aligned: `opening_chains` and the overall chains
        }
        let d = (dx / len, dy / len);
        let left = (-d.1, d.0);
        // the face on the lower-left side of the wall
        let down = if left.0 + left.1 < 0.0 { left } else { (-left.0, -left.1) };
        let f = if (down == left) == (faces[0] > faces[1]) { faces[0] } else { faces[1] };
        let at = |t: f64| (w.a.0 + d.0 * t + left.0 * f, w.a.1 + d.1 * t + left.1 * f);
        let mut pts = vec![0.0, len];
        for &(_, a, wd) in ops.iter().filter(|o| o.0 == i) {
            pts.push(a);
            pts.push(a + wd);
        }
        pts.sort_by(f64::total_cmp);
        pts.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        for k in 1..pts.len() {
            if pts[k] - pts[k - 1] < MIN_PIECE {
                continue;
            }
            out.push(ViewPrim::Dim {
                side: side_of(down),
                chain: INNER_CHAIN,
                a: at(pts[k - 1]),
                b: at(pts[k]),
                value: pts[k] - pts[k - 1],
            });
        }
    }
    out
}

/// Clear segments of the rings (even-odd) along the line `y = c` (`horiz`) or `x = c`, sorted.
fn chords(rings: &[Vec<(f64, f64)>], horiz: bool, c: f64) -> Vec<(f64, f64)> {
    let mut xs = vec![];
    for r in rings {
        for i in 0..r.len() {
            let (p, q) = (r[i], r[(i + 1) % r.len()]);
            let (pc, qc, pt, qt) = if horiz { (p.1, q.1, p.0, q.0) } else { (p.0, q.0, p.1, q.1) };
            if (pc <= c) != (qc <= c) {
                xs.push(pt + (c - pc) / (qc - pc) * (qt - pt));
            }
        }
    }
    xs.sort_by(f64::total_cmp);
    xs.chunks_exact(2).map(|p| (p[0], p[1])).collect()
}

/// Clear dimensions of one room: its chords along both axes.
pub(crate) fn room_chords(rings: &[Vec<(f64, f64)>]) -> Vec<ViewPrim> {
    let Some(outer) = rings.first().filter(|r| r.len() >= 3) else {
        return vec![];
    };
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for p in outer {
        b = [b[0].min(p.0), b[1].min(p.1), b[2].max(p.0), b[3].max(p.1)];
    }
    let mut out = vec![];
    for horiz in [true, false] {
        let (lo, hi) = if horiz { (b[1], b[3]) } else { (b[0], b[2]) };
        let c = lo + CHORD_AT * (hi - lo);
        for (s, e) in chords(rings, horiz, c) {
            if e - s < MIN_CHORD {
                continue;
            }
            let at = |t: f64| if horiz { (t, c) } else { (c, t) };
            out.push(ViewPrim::Dim {
                side: if horiz { Side::South } else { Side::West },
                chain: INNER_CHAIN,
                a: at(s),
                b: at(e),
                value: e - s,
            });
        }
    }
    out
}

impl Model {
    /// Clear dimensions of the closed rooms of a level.
    pub(crate) fn inner_room_dims(&self, level: i64) -> Result<Vec<ViewPrim>, Error> {
        let ids: Vec<i64> = {
            let mut st = self
                .db
                .prepare("SELECT id FROM room WHERE level=?1 ORDER BY id")?;
            let rows = st.query_map([level], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut out = vec![];
        for id in ids {
            if let Some(q) = self.room_quantities(id)? {
                out.extend(room_chords(&q.rings));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(d: &[ViewPrim]) -> Vec<f64> {
        d.iter()
            .filter_map(|p| match p {
                ViewPrim::Dim { value, .. } => Some(*value),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn rectangle_gives_a_chord_each_way() {
        let r = vec![vec![(0.0, 0.0), (5.0, 0.0), (5.0, 3.0), (0.0, 3.0)]];
        assert_eq!(values(&room_chords(&r)), [5.0, 3.0]);
    }

    #[test]
    fn l_shape_follows_the_clear_part_and_a_column_splits_the_chord() {
        // L: 6 x 4 with the top right 3 x 2 cut away; the low chord crosses 6, the left one 4
        let l = vec![vec![
            (0.0, 0.0),
            (6.0, 0.0),
            (6.0, 2.0),
            (3.0, 2.0),
            (3.0, 4.0),
            (0.0, 4.0),
        ]];
        assert_eq!(values(&room_chords(&l)), [6.0, 4.0]);
        let h = vec![
            vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
            vec![(2.8, 0.5), (2.8, 3.5), (3.2, 3.5), (3.2, 0.5)],
        ];
        let n = room_chords(&h)
            .iter()
            .filter(|p| matches!(p, ViewPrim::Dim { side: Side::South, .. }))
            .count();
        assert_eq!(n, 2);
    }

    fn row(a: (f64, f64), b: (f64, f64), arc: Option<ddc_geom::arc::ArcAxis>) -> WallRow {
        WallRow {
            id: 1,
            a,
            b,
            base_offset: 0.0,
            height: 3.0,
            offset0: -0.1,
            total: 0.2,
            mats: vec![],
            thick: vec![0.2],
            arc,
            top: None,
            base: None,
        }
    }

    #[test]
    fn skew_wall_is_measured_along_its_face_with_the_opening() {
        let w = row((0.0, 0.0), (3.0, 4.0), None);
        let d = oblique_chains(&[w], &[(0, 1.0, 1.0)]);
        // 1 + 1 + 3 along the axis; the face is as long as the axis
        assert_eq!(values(&d).iter().map(|v| (v * 100.0).round()).collect::<Vec<_>>(), [100.0, 100.0, 300.0]);
        // the chain stands on the face on the lower-left side of the axis
        let ViewPrim::Dim { a, .. } = &d[0] else { panic!() };
        assert!(a.0 < 0.0 && a.1 > 0.0, "{a:?}");
        // a straight axis-aligned wall is left to the other chains
        assert!(oblique_chains(&[row((0.0, 0.0), (4.0, 0.0), None)], &[]).is_empty());
    }

    #[test]
    fn arc_wall_gives_arc_length_and_radius() {
        use ddc_geom::{arc::ArcAxis, v2, Tol};
        // quarter circle of radius 2 about the origin
        let ax = ArcAxis::new(v2(2.0, 0.0), v2(0.0, 2.0), (std::f64::consts::PI / 8.0).tan(), Tol::default()).unwrap();
        let d = oblique_chains(&[row((2.0, 0.0), (0.0, 2.0), Some(ax))], &[]);
        assert_eq!(d.len(), 2);
        let vals = values(&d);
        let outer = ax.radius_at(0.1_f64.max(-0.1)).max(ax.radius_at(-0.1));
        assert!((vals[1] - outer).abs() < 1e-9, "{vals:?}");
        assert!((vals[0] - outer * std::f64::consts::FRAC_PI_2).abs() < 1e-6, "{vals:?}");
        assert!(matches!(d[1], ViewPrim::Dim { chain: RADIUS_CHAIN, .. }));
    }
}
