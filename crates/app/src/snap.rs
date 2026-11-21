// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Cursor snaps: endpoints, midpoints, perpendicular, wall axis/face, 0/45/90° angles, grid;
//! datums (`datum_candidates`): intersection ✕, grid axis ◇, reference plane ┆.
//! Pure logic without UI: takes the cursor and context in meters, returns ordered candidates.

use ddc_geom::{v2, V2};

/// Temporary guide of a captured point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guide {
    X,
    Y,
    Extension,
    /// Crossing of two guides.
    Cross,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SnapKind {
    Endpoint,
    Midpoint,
    Perpendicular,
    WallAxis,
    WallFace,
    /// Angle from the last point, degrees (multiple of 45).
    Angle(i32),
    Grid,
    /// Intersection of datum lines (axes, planes, wall axes).
    Intersection,
    GridAxis,
    RefPlane,
    /// On the trace of the user's work plane.
    WorkPlane,
    /// Nearest point on an underlay line.
    Nearest,
    /// Alignment in X / in Y / in both with distant points (tracking).
    AlignX,
    AlignY,
    AlignXY,
    /// Centre of an arc or circle.
    Center,
    /// Exact curve: where a line from the last point touches it.
    Tangent,
    /// Exact curve: a third of its length from either end.
    Param,
    /// Point of a circle or arc at 0/90/180/270 degrees.
    Quadrant,
    /// On the line of a wall beyond its end (dashed guide).
    Extension,
    /// Rubber band parallel to a nearby wall.
    Parallel,
    /// Aligned with a captured point: the guide it rides on.
    Tracking(Guide),
    /// Step equal to the previous segment of the chain.
    EqualStep,
    Free,
}

impl SnapKind {
    pub fn rank(self) -> u8 {
        match self {
            Self::Endpoint | Self::Intersection | Self::Quadrant | Self::Tracking(Guide::Cross) => 0,
            Self::Midpoint | Self::Center | Self::Param => 1,
            Self::Perpendicular | Self::Tangent => 2,
            Self::WallAxis
            | Self::WallFace
            | Self::GridAxis
            | Self::RefPlane
            | Self::WorkPlane
            | Self::Nearest => 3,
            Self::AlignXY => 3,
            Self::Angle(_) | Self::AlignX | Self::AlignY | Self::Extension | Self::Parallel | Self::Tracking(_) | Self::EqualStep => 4,
            Self::Grid => 5,
            Self::Free => 6,
        }
    }

    /// Locale key `snap.<kind>`; angle and free point have no key.
    pub fn key(self) -> Option<&'static str> {
        Some(match self {
            Self::Endpoint => "snap.endpoint",
            Self::Midpoint => "snap.midpoint",
            Self::Perpendicular => "snap.perpendicular",
            Self::WallAxis => "snap.wall_axis",
            Self::WallFace => "snap.wall_face",
            Self::Grid => "snap.grid",
            Self::Intersection => "snap.intersection",
            Self::GridAxis => "snap.grid_axis",
            Self::RefPlane => "snap.ref_plane",
            Self::WorkPlane => "snap.work_plane",
            Self::Nearest => "snap.nearest",
            Self::Center => "snap.center",
            Self::Quadrant => "snap.quadrant",
            Self::Extension => "snap.extension",
            Self::Parallel => "snap.parallel",
            Self::Tracking(Guide::X) => "snap.tracking_x",
            Self::Tracking(Guide::Y) => "snap.tracking_y",
            Self::Tracking(Guide::Extension) => "snap.extension",
            Self::Tracking(Guide::Cross) => "snap.tracking_cross",
            Self::EqualStep => "snap.equal_step",
            // the 3D snap strings name the same curve snaps
            Self::Tangent => "e3d.snap.tangent",
            Self::Param => "e3d.snap.param",
            Self::AlignX => "snap.align_x",
            Self::AlignY => "snap.align_y",
            Self::AlignXY => "snap.align_xy",
            Self::Angle(_) | Self::Free => return None,
        })
    }

    pub fn label(self) -> String {
        match (self, self.key()) {
            (Self::Angle(a), _) => format!("{a}°"),
            (_, Some(k)) => crate::tr(k, &[]),
            _ => String::new(),
        }
    }

    /// Bit of this kind in a [`SnapProfile`] mask (all angles share one).
    pub const fn bit(self) -> u32 {
        1 << match self {
            Self::Endpoint => 0,
            Self::Midpoint => 1,
            Self::Perpendicular => 2,
            Self::WallAxis => 3,
            Self::WallFace => 4,
            Self::Angle(_) => 5,
            Self::Grid => 6,
            Self::Intersection => 7,
            Self::GridAxis => 8,
            Self::RefPlane => 9,
            Self::WorkPlane => 10,
            Self::Nearest => 11,
            Self::AlignX | Self::AlignY | Self::AlignXY => 12,
            Self::Center => 13,
            Self::Tangent => 14,
            Self::Param => 15,
            Self::Quadrant => 16,
            Self::Extension => 17,
            Self::Parallel => 18,
            Self::Tracking(_) => 19,
            Self::EqualStep => 20,
            Self::Free => 21,
        }
    }

    pub fn is_align(self) -> bool {
        matches!(self, Self::AlignX | Self::AlignY | Self::AlignXY)
    }
}

/// Snap kinds a family offers and whether its hits may be pinned as constraints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapProfile {
    pub kinds: u32,
    pub pin: bool,
}

impl Default for SnapProfile {
    fn default() -> Self {
        SnapProfile::ALL
    }
}

impl SnapProfile {
    /// Every kind, pinning on: the default of a family.
    pub const ALL: SnapProfile = SnapProfile { kinds: u32::MAX, pin: true };
    /// Columns sit on axis crossings and centres: no wall faces, axes or perpendiculars.
    pub const COLUMN: SnapProfile = SnapProfile {
        kinds: SnapKind::Intersection.bit()
            | SnapKind::GridAxis.bit()
            | SnapKind::Center.bit()
            | SnapKind::Endpoint.bit()
            | SnapKind::Midpoint.bit()
            | SnapKind::Grid.bit()
            | SnapKind::Tracking(Guide::X).bit()
            | SnapKind::Free.bit(),
        pin: true,
    };

    pub fn has(self, k: SnapKind) -> bool {
        self.kinds & k.bit() != 0
    }

    /// Does the profile offer a 3D kind: shared kinds follow it, 3D-only kinds always stay.
    pub fn has3(self, k: crate::edit3d::snap::SnapKind) -> bool {
        k.plan_kind().is_none_or(|q| self.has(q))
    }

    /// The 3D mask: the user's toggles narrowed by this profile.
    pub fn mask3(self, user: crate::edit3d::snap::Mask) -> crate::edit3d::snap::Mask {
        use crate::edit3d::snap::{Mask, ALL};
        Mask(ALL.iter().filter(|k| user.has(**k) && self.has3(**k)).fold(0, |m, k| m | k.bit()))
    }

    /// Session profile: what the family offers, narrowed by the user's mode (a kind mask).
    pub fn within(self, user: u32) -> SnapProfile {
        SnapProfile { kinds: self.kinds & (user | SnapKind::Free.bit()), pin: self.pin }
    }

    /// The list without the kinds the profile leaves out; the free point always stays.
    pub fn gate(self, list: Vec<Snap>) -> Vec<Snap> {
        list.into_iter().filter(|s| s.kind == SnapKind::Free || self.has(s.kind)).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snap {
    pub p: V2,
    pub kind: SnapKind,
}

/// Wall for snapping: axis and half thickness.
#[derive(Clone, Copy, Debug)]
pub struct SnapWall {
    pub a: V2,
    pub b: V2,
    pub half: f64,
}

pub struct SnapCtx<'a> {
    pub walls: &'a [SnapWall],
    /// Points of the current draft (also endpoint/midpoint targets).
    pub draft: &'a [V2],
    /// Last point: angles and perpendicular are measured from it.
    pub from: Option<V2>,
    /// Grid step, m (0 — no grid).
    pub grid: f64,
    /// Capture radius, m.
    pub radius: f64,
}

/// Angle snap tolerance, degrees.
pub const ANGLE_TOL_DEG: f64 = 4.0;

pub fn dot(a: V2, b: V2) -> f64 {
    a.x * b.x + a.y * b.y
}

/// Nearest point on a segment and its parameter t∈[0,1].
pub fn closest_on_segment(p: V2, a: V2, b: V2) -> (V2, f64) {
    let d = b - a;
    let l2 = dot(d, d);
    if l2 == 0.0 {
        return (a, 0.0);
    }
    let t = (dot(p - a, d) / l2).clamp(0.0, 1.0);
    (a + d * t, t)
}

/// Candidates by priority, then by distance; the last one is always the free point.
pub fn candidates(ctx: &SnapCtx, cursor: V2) -> Vec<Snap> {
    let r = ctx.radius;
    let mut out: Vec<(Snap, f64)> = vec![];
    let mut angle = None;
    let mut push = |p: V2, kind: SnapKind| {
        let d = (p - cursor).len();
        if d <= r && p.x.is_finite() && p.y.is_finite() {
            out.push((Snap { p, kind }, d));
        }
    };
    let segs = ctx
        .walls
        .iter()
        .map(|w| (w.a, w.b, w.half))
        .chain(ctx.draft.windows(2).map(|s| (s[0], s[1], 0.0)));
    for (a, b, half) in segs.clone() {
        push(a, SnapKind::Endpoint);
        push(b, SnapKind::Endpoint);
        push((a + b) * 0.5, SnapKind::Midpoint);
        let len = (b - a).len();
        if len == 0.0 {
            continue;
        }
        let (c, t) = closest_on_segment(cursor, a, b);
        if t > 0.0 && t < 1.0 {
            push(c, SnapKind::WallAxis);
            if half > 0.0 {
                let n = v2(-(b - a).y / len, (b - a).x / len);
                for s in [1.0, -1.0] {
                    let (f, _) = closest_on_segment(cursor, a + n * (s * half), b + n * (s * half));
                    push(f, SnapKind::WallFace);
                }
            }
        }
        if let Some(f) = ctx.from {
            // foot of the perpendicular from the last point onto the wall line
            let d = b - a;
            let t = dot(f - a, d) / dot(d, d);
            if (0.0..=1.0).contains(&t) {
                let foot = a + d * t;
                if (foot - f).len() > 1e-9 {
                    push(foot, SnapKind::Perpendicular);
                }
            }
        }
    }
    for &p in ctx.draft {
        push(p, SnapKind::Endpoint);
    }
    if let Some(f) = ctx.from {
        let v = cursor - f;
        let l = v.len();
        if l > 1e-9 {
            let ang = v.y.atan2(v.x).to_degrees();
            let k = (ang / 45.0).round();
            if (ang - k * 45.0).abs() <= ANGLE_TOL_DEG {
                // exact unit steps: cos/sin of a right angle leave ~1e-16 off the axis, and a turn
                // back along that ray is then a sliver spike no exact predicate refuses
                const S: f64 = std::f64::consts::FRAC_1_SQRT_2;
                let dir = [
                    v2(1.0, 0.0),
                    v2(S, S),
                    v2(0.0, 1.0),
                    v2(-S, S),
                    v2(-1.0, 0.0),
                    v2(-S, -S),
                    v2(0.0, -1.0),
                    v2(S, -S),
                ][(k as i64).rem_euclid(8) as usize];
                let along = dot(v, dir);
                let mut p = f + dir * along;
                // length along the ray snaps to the grid step if there is a grid
                if ctx.grid > 0.0 {
                    let q = (along / ctx.grid).round() * ctx.grid;
                    if (q - along).abs() <= r {
                        p = f + dir * q;
                    }
                }
                let deg = ((k * 45.0) as i32).rem_euclid(360);
                angle = Some((
                    Snap {
                        p,
                        kind: SnapKind::Angle(deg),
                    },
                    (p - cursor).len(),
                ));
            }
        }
    }
    if ctx.grid > 0.0 {
        let g = ctx.grid;
        push(
            v2((cursor.x / g).round() * g, (cursor.y / g).round() * g),
            SnapKind::Grid,
        );
    }
    out.extend(angle);
    out.sort_by(|x, y| {
        x.0.kind
            .rank()
            .cmp(&y.0.kind.rank())
            .then(x.1.total_cmp(&y.1))
    });
    let mut res: Vec<Snap> = vec![];
    for (s, _) in out {
        if !res.iter().any(|q| (q.p - s.p).len() < 1e-9) {
            res.push(s);
        }
    }
    res.push(Snap {
        p: cursor,
        kind: SnapKind::Free,
    });
    res
}

/// How far beyond a wall end an extension guide reaches, m.
pub const EXT_REACH: f64 = 6.0;

/// Points on the lines of `segs` beyond their ends that lie within `r` of the cursor.
pub fn extension_candidates(segs: &[(V2, V2)], cursor: V2, r: f64) -> Vec<Snap> {
    let mut out = vec![];
    for &(a, b) in segs {
        let d = b - a;
        let len = d.len();
        if len < 1e-9 {
            continue;
        }
        let u = d * (1.0 / len);
        let w = cursor - a;
        let t = dot(w, u);
        let perp = (u.x * w.y - u.y * w.x).abs();
        let over = if t < 0.0 { -t } else { t - len };
        if perp <= r && over > 0.0 && over <= EXT_REACH {
            out.push(Snap { p: a + u * t, kind: SnapKind::Extension });
        }
    }
    out
}

/// Points on the ray from `from` parallel to each wall of `segs`, within `r` of the cursor.
/// A wall whose own line carries `from` gives nothing (that is an extension).
pub fn parallel_candidates(segs: &[(V2, V2)], from: V2, cursor: V2, r: f64) -> Vec<Snap> {
    let mut out = vec![];
    let v = cursor - from;
    for &(a, b) in segs {
        let d = b - a;
        let len = d.len();
        if len < 1e-9 || v.len() < 1e-9 {
            continue;
        }
        let u = d * (1.0 / len);
        let w = from - a;
        if (u.x * w.y - u.y * w.x).abs() < 1e-9 {
            continue;
        }
        let p = from + u * dot(v, u);
        if (p - cursor).len() <= r && (p - from).len() > r {
            out.push(Snap { p, kind: SnapKind::Parallel });
        }
    }
    out
}

/// Snaps to the X and Y guides of captured points and to the crossing of two of them.
/// A guide holds within `r` of its line; the crossing is the exact corner of the two lines.
pub fn tracking_candidates(acq: &[V2], cursor: V2, r: f64) -> Vec<Snap> {
    let mut out = vec![];
    let mut push = |p: V2, g: Guide| out.push(Snap { p, kind: SnapKind::Tracking(g) });
    for (i, a) in acq.iter().enumerate() {
        if (cursor.x - a.x).abs() <= r {
            push(v2(a.x, cursor.y), Guide::X);
        }
        if (cursor.y - a.y).abs() <= r {
            push(v2(cursor.x, a.y), Guide::Y);
        }
        for b in &acq[i + 1..] {
            for c in [v2(a.x, b.y), v2(b.x, a.y)] {
                if (c - cursor).len() <= r && c != *a && c != *b {
                    push(c, Guide::Cross);
                }
            }
        }
    }
    out
}

/// The step that repeats the last segment of `chain`, along the cursor direction from its end.
pub fn equal_step_candidate(chain: &[V2], cursor: V2, r: f64) -> Option<Snap> {
    let [.., a, b] = chain else { return None };
    let step = (*b - *a).len();
    let v = cursor - *b;
    if step < 1e-9 || v.len() < 1e-9 {
        return None;
    }
    let p = *b + v * (step / v.len());
    ((p - cursor).len() <= r).then_some(Snap { p, kind: SnapKind::EqualStep })
}

/// Intersection of lines a1→b1 and a2→b2 within both segments.
pub fn seg_cross(a1: V2, b1: V2, a2: V2, b2: V2) -> Option<V2> {
    let (d1, d2) = (b1 - a1, b2 - a2);
    let den = d1.x * d2.y - d1.y * d2.x;
    if den.abs() < 1e-12 {
        return None;
    }
    let w = a2 - a1;
    let t = (w.x * d2.y - w.y * d2.x) / den;
    let u = (w.x * d1.y - w.y * d1.x) / den;
    let ok = |v: f64| (-1e-9..=1.0 + 1e-9).contains(&v);
    (ok(t) && ok(u)).then(|| a1 + d1 * t)
}

/// Snaps to datum lines (`kind` — GridAxis/RefPlane/WallAxis): intersections and projections
/// within radius `r`; merged with [`candidates`] by rank.
pub fn datum_candidates(lines: &[(V2, V2, SnapKind)], cursor: V2, r: f64) -> Vec<Snap> {
    let mut out: Vec<(Snap, f64)> = vec![];
    for (i, &(a, b, k)) in lines.iter().enumerate() {
        for &(c, d, _) in &lines[i + 1..] {
            if let Some(x) = seg_cross(a, b, c, d) {
                out.push((
                    Snap {
                        p: x,
                        kind: SnapKind::Intersection,
                    },
                    (x - cursor).len(),
                ));
            }
        }
        if k != SnapKind::WallAxis {
            let (c, _) = closest_on_segment(cursor, a, b);
            out.push((Snap { p: c, kind: k }, (c - cursor).len()));
        }
    }
    out.retain(|s| s.1 <= r);
    out.sort_by(|x, y| {
        x.0.kind
            .rank()
            .cmp(&y.0.kind.rank())
            .then(x.1.total_cmp(&y.1))
    });
    out.into_iter().map(|s| s.0).collect()
}

/// Snaps to underlay lines: endpoints, midpoints, intersections of adjacent lines, nearest point.
pub fn underlay_candidates(segs: &[(V2, V2)], cursor: V2, r: f64) -> Vec<Snap> {
    let near: Vec<&(V2, V2)> = segs
        .iter()
        .filter(|(a, b)| {
            let (lo_x, hi_x) = (a.x.min(b.x) - r, a.x.max(b.x) + r);
            let (lo_y, hi_y) = (a.y.min(b.y) - r, a.y.max(b.y) + r);
            cursor.x >= lo_x && cursor.x <= hi_x && cursor.y >= lo_y && cursor.y <= hi_y
        })
        .take(400)
        .collect();
    let mut out = vec![];
    let mut push = |p: V2, kind: SnapKind| {
        if (p - cursor).len() <= r {
            out.push(Snap { p, kind });
        }
    };
    for (i, (a, b)) in near.iter().enumerate() {
        push(*a, SnapKind::Endpoint);
        push(*b, SnapKind::Endpoint);
        push((*a + *b) * 0.5, SnapKind::Midpoint);
        let d = *b - *a;
        let l2 = d.dot(d);
        if l2 > 1e-12 {
            let t = ((cursor - *a).dot(d) / l2).clamp(0.0, 1.0);
            push(*a + d * t, SnapKind::Nearest);
        }
        for (c, e) in near.iter().skip(i + 1) {
            if let Some(p) = seg_cross(*a, *b, *c, *e) {
                push(p, SnapKind::Intersection);
            }
        }
    }
    out
}

/// Merge datum snaps into the `candidates` list (rank, then distance; Free goes last). On the
/// same point a guide with a source (extension, tracking, step) names it before a bare alignment,
/// whatever was merged first.
pub fn merge(base: &mut Vec<Snap>, extra: Vec<Snap>, cursor: V2) {
    let free = base.pop();
    base.extend(extra);
    base.sort_by(|x, y| {
        x.kind
            .rank()
            .cmp(&y.kind.rank())
            .then((x.p - cursor).len().total_cmp(&(y.p - cursor).len()))
    });
    let mut res: Vec<Snap> = vec![];
    for s in base.drain(..) {
        match res.iter_mut().find(|q| (q.p - s.p).len() < 1e-9) {
            None => res.push(s),
            Some(q) if q.kind.is_align() && !s.kind.is_align() && q.kind.rank() == s.kind.rank() => {
                q.kind = s.kind;
            }
            Some(_) => {}
        }
    }
    *base = res;
    base.extend(free);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_gates_kinds_and_keeps_free() {
        let l = vec![
            Snap { p: v2(0.0, 0.0), kind: SnapKind::WallFace },
            Snap { p: v2(0.0, 0.0), kind: SnapKind::Intersection },
            Snap { p: v2(0.0, 0.0), kind: SnapKind::Free },
        ];
        let kinds: Vec<_> = SnapProfile::COLUMN.gate(l.clone()).iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [SnapKind::Intersection, SnapKind::Free]);
        assert_eq!(SnapProfile::ALL.gate(l.clone()).len(), 3);
        // the user's mode narrows the family, never widens it
        let p = SnapProfile::COLUMN.within(SnapKind::Intersection.bit() | SnapKind::WallFace.bit());
        assert!(p.has(SnapKind::Intersection) && !p.has(SnapKind::WallFace) && !p.has(SnapKind::Grid));
        assert!(p.has(SnapKind::Free));
    }

    #[test]
    fn tracking_cross_is_exact() {
        let acq = [v2(6.0, 0.0), v2(0.0, 5.0)];
        let c = tracking_candidates(&acq, v2(6.03, 5.04), 0.12);
        let x = c.iter().find(|s| s.kind == SnapKind::Tracking(Guide::Cross)).expect("cross");
        assert_eq!(x.p, v2(6.0, 5.0));
        assert_eq!(SnapKind::Tracking(Guide::Cross).rank(), 0);
        // one guide: X of the first point, y stays the cursor's
        let c = tracking_candidates(&acq[..1], v2(6.05, 3.0), 0.12);
        assert_eq!((c[0].kind, c[0].p), (SnapKind::Tracking(Guide::X), v2(6.0, 3.0)));
        assert!(tracking_candidates(&acq[..1], v2(6.5, 3.0), 0.12).is_empty());
    }

    #[test]
    fn equal_step_repeats_the_last_segment() {
        let chain = [v2(0.0, 0.0), v2(6.0, 0.0)];
        // the step runs toward the cursor: a cursor 5 mm off the line lands within 0.01 of (12, 0)
        let s = equal_step_candidate(&chain, v2(11.9, 0.005), 0.2).expect("step");
        assert_eq!(s.kind, SnapKind::EqualStep);
        assert!((s.p - v2(12.0, 0.0)).len() < 0.01 && ((s.p - chain[1]).len() - 6.0).abs() < 1e-9);
        // a74859c60 docstring: "along the cursor direction from its end", so 0.1 m off stays on the cursor ray
        let (c, v) = (v2(11.9, 0.1), v2(11.9, 0.1) - chain[1]);
        let d = equal_step_candidate(&chain, c, 0.2).expect("step at 0.1 m off").p - chain[1];
        assert!((d.len() - 6.0).abs() < 1e-9);
        assert!((d.x * v.y - d.y * v.x).abs() < 1e-9 && d.x * v.x > 0.0);
        assert!(equal_step_candidate(&chain, v2(10.0, 0.0), 0.2).is_none());
        assert!(equal_step_candidate(&chain[..1], v2(1.0, 0.0), 0.2).is_none());
    }

    #[test]
    fn kinds_table_extension_parallel() {
        let wall = [(v2(0.0, 0.0), v2(4.0, 0.0))];
        // beyond either end on the wall line: exact projection onto the line
        let e = extension_candidates(&wall, v2(5.5, 0.05), 0.15);
        assert_eq!((e[0].kind, e[0].p), (SnapKind::Extension, v2(5.5, 0.0)));
        let e = extension_candidates(&wall, v2(-2.0, -0.1), 0.15);
        assert_eq!(e[0].p, v2(-2.0, 0.0));
        // over the wall itself, or too far off the line: nothing
        assert!(extension_candidates(&wall, v2(2.0, 0.0), 0.15).is_empty());
        assert!(extension_candidates(&wall, v2(5.5, 0.4), 0.15).is_empty());
        // parallel: from (1,2) the ray along the wall passes the cursor at y = 2
        let p = parallel_candidates(&wall, v2(1.0, 2.0), v2(3.0, 2.08), 0.15);
        assert_eq!((p[0].kind, p[0].p), (SnapKind::Parallel, v2(3.0, 2.0)));
        // a start on the wall line is not parallel; a skewed band is not either
        assert!(parallel_candidates(&wall, v2(1.0, 0.0), v2(3.0, 0.05), 0.15).is_empty());
        assert!(parallel_candidates(&wall, v2(1.0, 2.0), v2(3.0, 2.6), 0.15).is_empty());
    }

    #[test]
    fn datum_intersection_beats_axis() {
        let l = [
            (v2(0.0, -5.0), v2(0.0, 5.0), SnapKind::GridAxis),
            (v2(-5.0, 2.0), v2(5.0, 2.0), SnapKind::RefPlane),
        ];
        let c = datum_candidates(&l, v2(0.05, 1.9), 0.3);
        assert_eq!(c[0].kind, SnapKind::Intersection);
        assert!((c[0].p - v2(0.0, 2.0)).len() < 1e-9);
        assert!(c.iter().any(|s| s.kind == SnapKind::GridAxis));
        let far = datum_candidates(&l, v2(3.0, -3.0), 0.3);
        assert!(far.is_empty());
    }

    fn ctx<'a>(walls: &'a [SnapWall], from: Option<V2>) -> SnapCtx<'a> {
        SnapCtx {
            walls,
            draft: &[],
            from,
            grid: 1.0,
            radius: 0.15,
        }
    }

    fn wall() -> [SnapWall; 1] {
        [SnapWall {
            a: v2(0.0, 0.0),
            b: v2(6.0, 0.0),
            half: 0.1,
        }]
    }

    #[test]
    fn endpoint_beats_grid_and_axis() {
        let w = wall();
        let c = candidates(&ctx(&w, None), v2(5.95, 0.03));
        assert_eq!(c[0].kind, SnapKind::Endpoint);
        assert_eq!(c[0].p, v2(6.0, 0.0));
        assert_eq!(c.last().unwrap().kind, SnapKind::Free);
    }

    #[test]
    fn midpoint_and_faces() {
        let w = wall();
        let c = candidates(&ctx(&w, None), v2(3.02, 0.01));
        assert_eq!(
            c[0],
            Snap {
                p: v2(3.0, 0.0),
                kind: SnapKind::Midpoint
            }
        );
        let c = candidates(&ctx(&w, None), v2(2.5, 0.09));
        let face = c.iter().find(|s| s.kind == SnapKind::WallFace).unwrap();
        assert!((face.p.y - 0.1).abs() < 1e-12 && (face.p.x - 2.5).abs() < 1e-12);
        assert!(c.iter().any(|s| s.kind == SnapKind::WallAxis));
    }

    #[test]
    fn perpendicular_from_last_point() {
        let w = wall();
        let c = candidates(&ctx(&w, Some(v2(2.3, 3.0))), v2(2.35, 0.05));
        assert_eq!(c[0].kind, SnapKind::Perpendicular);
        assert_eq!(c[0].p, v2(2.3, 0.0));
    }

    #[test]
    fn angles_0_45_90() {
        let none: [SnapWall; 0] = [];
        let mut cx = ctx(&none, Some(v2(0.0, 0.0)));
        cx.grid = 0.0;
        cx.radius = 1.0;
        let c = candidates(&cx, v2(3.0, 0.1));
        assert_eq!(c[0].kind, SnapKind::Angle(0));
        assert!(c[0].p.y.abs() < 1e-12);
        let c = candidates(&cx, v2(2.0, 2.1));
        assert_eq!(c[0].kind, SnapKind::Angle(45));
        let c = candidates(&cx, v2(-0.05, -2.0));
        assert_eq!(c[0].kind, SnapKind::Angle(270));
        let c = candidates(&cx, v2(3.0, 1.0)); // 18° — no angle snap
        assert_eq!(c[0].kind, SnapKind::Free);
    }

    #[test]
    fn grid_snap_and_out_of_radius() {
        let none: [SnapWall; 0] = [];
        let c = candidates(&ctx(&none, None), v2(2.04, 2.97));
        assert_eq!(
            c[0],
            Snap {
                p: v2(2.0, 3.0),
                kind: SnapKind::Grid
            }
        );
        let c = candidates(&ctx(&none, None), v2(2.5, 2.5));
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].kind, SnapKind::Free);
    }

    #[test]
    fn degenerate_input_no_panic() {
        let w = [SnapWall {
            a: v2(1.0, 1.0),
            b: v2(1.0, 1.0),
            half: 0.1,
        }];
        let c = candidates(&ctx(&w, Some(v2(1.0, 1.0))), v2(f64::NAN, 1.0));
        assert_eq!(c.last().unwrap().kind, SnapKind::Free);
    }

    /// An alignment merged first does not hide the extension or tracking guide on its point.
    #[test]
    fn a_guide_beats_a_bare_alignment_on_the_same_point() {
        let cursor = v2(8.0, 0.05);
        let free = Snap { p: cursor, kind: SnapKind::Free };
        for guide in [SnapKind::Extension, SnapKind::Tracking(Guide::X)] {
            let mut list = vec![free];
            // the alignment a hair closer: two projections of one point differ in the last bits
            merge(&mut list, vec![Snap { p: v2(8.0, 1e-12), kind: SnapKind::AlignY }], cursor);
            merge(&mut list, vec![Snap { p: v2(8.0, 0.0), kind: guide }], cursor);
            assert_eq!(list[0].kind, guide, "{list:?}");
            assert_eq!(list.len(), 2, "{list:?}");
            assert_eq!(list.last().unwrap().kind, SnapKind::Free);
        }
    }
}
