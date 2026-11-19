//! Snap core without UI or session: hits that carry element references, one pixel radius,
//! hysteresis, the per-point override and the off switch (INTERACTIONS §1.2-1.3).
//! Input is plain data (cursor, scale, held modifiers), so any tool session can feed it.

use crate::snap::{Snap, SnapKind, SnapWall};
use ddc_geom::{v3, V2, V3};
use ddc_model::Target;

/// A held hit stays while the cursor is within this many radii of it.
pub const HOLD: f64 = 1.5;

/// Which feature of the target the hit sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feat {
    Axis,
    /// Face on the left (+1) or right (-1) of the axis direction.
    Side(i8),
    End(u8),
    Mid,
    /// Labelled face of a mass (shapes only).
    MassFace(u32),
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapRef {
    pub target: Target,
    pub feat: Feat,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapHit {
    pub kind: SnapKind,
    /// World point; in the plan z is the work-plane elevation.
    pub p: V3,
    pub refs: [Option<SnapRef>; 2],
}

impl SnapHit {
    pub fn bare(s: Snap) -> Self {
        SnapHit::at(s, 0.0)
    }

    /// A plan hit lifted to the work-plane elevation `z`, the same z as the free point.
    pub fn at(s: Snap, z: f64) -> Self {
        SnapHit { kind: s.kind, p: v3(s.p.x, s.p.y, z), refs: [None, None] }
    }

    /// Plan position.
    pub fn xy(&self) -> V2 {
        V2 { x: self.p.x, y: self.p.y }
    }
}

const EPS: f64 = 1e-6;

fn cross(a: V2, b: V2) -> f64 {
    a.x * b.y - a.y * b.x
}

/// Elements the hit lies on, up to two (a corner or a crossing names both walls).
pub fn refs_for(s: &Snap, walls: &[(i64, SnapWall)]) -> [Option<SnapRef>; 2] {
    let mut out: [Option<SnapRef>; 2] = [None, None];
    let mut put = |target: Target, feat: Feat| {
        if let Some(slot) = out.iter_mut().find(|x| x.is_none()) {
            *slot = Some(SnapRef { target, feat });
        }
    };
    for (id, w) in walls {
        let t = Target::Wall(*id);
        let d = w.b - w.a;
        let len = d.len();
        if len == 0.0 {
            continue;
        }
        let off = cross(d, s.p - w.a) / len; // signed distance to the axis line
        let along = d.dot(s.p - w.a) / len;
        let inside = along > -EPS && along < len + EPS;
        match s.kind {
            SnapKind::Endpoint => {
                if (s.p - w.a).len() < EPS {
                    put(t, Feat::End(0));
                } else if (s.p - w.b).len() < EPS {
                    put(t, Feat::End(1));
                }
            }
            SnapKind::Midpoint => {
                if (s.p - (w.a + w.b) * 0.5).len() < EPS {
                    put(t, Feat::Mid);
                }
            }
            SnapKind::WallAxis | SnapKind::Intersection if off.abs() < EPS && inside => put(t, Feat::Axis),
            SnapKind::Perpendicular if off.abs() < EPS && inside => put(t, Feat::Line),
            SnapKind::WallFace if inside && (off.abs() - w.half).abs() < EPS => {
                put(t, Feat::Side(if off > 0.0 { 1 } else { -1 }))
            }
            _ => {}
        }
    }
    out
}

/// The point at distance `d` from a captured point along the guide the hit rides on (X guide:
/// vertical, Y guide: horizontal), on the cursor's side of the captured point.
pub fn point_along_guide(acq: &[V2], hit: &Snap, cursor: V2, d: f64) -> Option<V2> {
    use crate::snap::Guide;
    let SnapKind::Tracking(g) = hit.kind else { return None };
    for a in acq {
        let (on, dir) = match g {
            Guide::X if (hit.p.x - a.x).abs() < 1e-9 => (true, V2 { x: 0.0, y: if cursor.y >= a.y { 1.0 } else { -1.0 } }),
            Guide::Y if (hit.p.y - a.y).abs() < 1e-9 => (true, V2 { x: if cursor.x >= a.x { 1.0 } else { -1.0 }, y: 0.0 }),
            _ => (false, *a),
        };
        if on {
            return Some(*a + dir * d);
        }
    }
    None
}

/// Pins one kind until the next click (Shift+M: midpoints only).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Override(pub SnapKind);

/// A point captured by resting the cursor on it: guides run from it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Acquired {
    pub p: V2,
    pub from: Option<SnapRef>,
    pub at_ms: u64,
}

/// Captures kept at most; the oldest goes first.
pub const MAX_ACQUIRED: usize = 7;
/// Rest time that captures (or releases) a point, ms.
pub const DWELL_MS: u64 = 300;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnapState {
    pub held: Option<SnapHit>,
    pub over: Option<Override>,
    /// F3: snapping is off, every answer is the free point.
    pub off: bool,
    /// Work-plane elevation the plan answers are lifted to.
    pub z: f64,
    pub acquired: Vec<Acquired>,
    /// Where the cursor rests, since when, and whether that rest already toggled.
    rest: Option<(V2, u64, bool)>,
}

/// What the user holds while moving.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hold {
    /// Alt: free point for as long as it is held.
    pub free: bool,
}

impl SnapState {
    /// Answer for `cursor`: the first usable candidate (best first, free point last), with
    /// the held hit kept until the cursor leaves `HOLD * r` or a better-ranked hit appears.
    pub fn resolve(&mut self, cursor: V2, r: f64, cands: &[SnapHit], hold: Hold) -> SnapHit {
        let free = SnapHit { kind: SnapKind::Free, p: v3(cursor.x, cursor.y, self.z), refs: [None, None] };
        if self.off || hold.free {
            self.held = None;
            return free;
        }
        let over = self.over;
        let best = cands
            .iter()
            .find(|c| match over {
                Some(Override(k)) => c.kind == k,
                None => c.kind != SnapKind::Free,
            })
            .copied();
        let keep = self.held.filter(|h| {
            (h.xy() - cursor).len() <= HOLD * r
                && over.is_none_or(|Override(k)| h.kind == k)
                && best.is_none_or(|b| b.kind.rank() >= h.kind.rank())
        });
        let hit = keep.or(best).unwrap_or(free);
        self.held = (hit.kind != SnapKind::Free).then_some(hit);
        hit
    }

    /// Clock tick with the current point hit (`None` over empty space). A rest of `DWELL_MS` on
    /// the same point captures it, or releases it when it is captured already; returns true then.
    pub fn dwell(&mut self, hit: Option<&SnapHit>, now_ms: u64) -> bool {
        let Some(h) = hit.filter(|h| h.kind != SnapKind::Free) else {
            self.rest = None;
            return false;
        };
        let (p, since, done) = match self.rest {
            Some((p, t, d)) if (p - h.xy()).len() < 1e-9 => (p, t, d),
            _ => (h.xy(), now_ms, false),
        };
        let fire = !done && now_ms.saturating_sub(since) >= DWELL_MS;
        self.rest = Some((p, since, done || fire));
        if fire {
            match self.acquired.iter().position(|a| (a.p - p).len() < 1e-9) {
                Some(i) => {
                    self.acquired.remove(i);
                }
                None => {
                    if self.acquired.len() == MAX_ACQUIRED {
                        self.acquired.remove(0);
                    }
                    self.acquired.push(Acquired { p, from: h.refs[0], at_ms: now_ms });
                }
            }
        }
        fire
    }

    /// Captured points, for the guides.
    pub fn acquired_points(&self) -> Vec<V2> {
        self.acquired.iter().map(|a| a.p).collect()
    }

    /// A click was placed: the override ends, the held hit is dropped.
    pub fn clicked(&mut self) {
        self.over = None;
        self.held = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::v2;

    fn w(a: V2, b: V2) -> (i64, SnapWall) {
        (7, SnapWall { a, b, half: 0.1 })
    }

    fn hit(kind: SnapKind, p: V2) -> SnapHit {
        SnapHit { kind, p: v3(p.x, p.y, 0.0), refs: [None, None] }
    }

    #[test]
    fn z_is_the_same_for_hits_and_free_point() {
        let mut s = SnapState { z: 3.0, ..SnapState::default() };
        let e = Snap { p: v2(1.0, 0.0), kind: SnapKind::Endpoint };
        let cands = [SnapHit::at(e, s.z)];
        assert_eq!(s.resolve(v2(1.01, 0.0), 0.1, &cands, Hold::default()).p.z, 3.0);
        assert_eq!(s.resolve(v2(5.0, 0.0), 0.1, &[], Hold::default()).p, v3(5.0, 0.0, 3.0), "free point");
    }

    #[test]
    fn acquire_fifo_7_and_toggle() {
        let mut s = SnapState::default();
        let at = |i: usize| hit(SnapKind::Endpoint, v2(i as f64, 0.0));
        // a rest shorter than 300 ms captures nothing, a move restarts the clock
        assert!(!s.dwell(Some(&at(1)), 0));
        assert!(!s.dwell(Some(&at(1)), 299));
        assert!(s.acquired.is_empty());
        assert!(s.dwell(Some(&at(1)), 300));
        assert!(!s.dwell(Some(&at(1)), 900), "one toggle per rest");
        assert_eq!(s.acquired.len(), 1);
        // resting on it again releases it
        s.dwell(Some(&at(2)), 1000);
        s.dwell(Some(&at(1)), 1100);
        assert!(s.dwell(Some(&at(1)), 1400));
        assert!(s.acquired.is_empty());
        // eight captures keep the last seven
        let mut t = 2000;
        for i in 0..8 {
            s.dwell(Some(&at(i)), t);
            s.dwell(Some(&at(i)), t + 300);
            t += 1000;
        }
        let xs: Vec<f64> = s.acquired.iter().map(|a| a.p.x).collect();
        assert_eq!(xs, (1..8).map(f64::from).collect::<Vec<_>>());
        // leaving every point clears the rest
        assert!(!s.dwell(None, t));
    }

    #[test]
    fn typed_along_guide() {
        let acq = [v2(6.0, 0.0)];
        let x = Snap { p: v2(6.0, 3.0), kind: SnapKind::Tracking(crate::snap::Guide::X) };
        // 1200 mm up the X guide of the captured end, on the cursor's side
        assert_eq!(point_along_guide(&acq, &x, v2(6.04, 3.0), 1.2), Some(v2(6.0, 1.2)));
        assert_eq!(point_along_guide(&acq, &x, v2(6.04, -3.0), 1.2), Some(v2(6.0, -1.2)));
        // the Y guide: right of the captured point, so the distance runs right
        let y = Snap { p: v2(8.0, 0.0), kind: SnapKind::Tracking(crate::snap::Guide::Y) };
        assert_eq!(point_along_guide(&acq, &y, v2(8.0, 0.03), 1.2), Some(v2(7.2, 0.0)));
        // original point: 270372ebd doc "on the cursor's side of the captured point", so left
        assert_eq!(point_along_guide(&acq, &y, v2(2.0, 0.03), 1.2), Some(v2(4.8, 0.0)));
        // not on a guide of any captured point, or not a guide at all
        let off = Snap { p: v2(9.0, 3.0), kind: SnapKind::Tracking(crate::snap::Guide::X) };
        assert_eq!(point_along_guide(&acq, &off, v2(9.0, 3.0), 1.2), None);
        let e = Snap { p: v2(6.0, 0.0), kind: SnapKind::Endpoint };
        assert_eq!(point_along_guide(&acq, &e, v2(6.0, 0.0), 1.2), None);
    }

    #[test]
    fn refs_name_the_wall_and_feature() {
        let ws = [w(v2(0.0, 0.0), v2(6.0, 0.0))];
        let r = |k, p| refs_for(&Snap { p, kind: k }, &ws);
        assert_eq!(r(SnapKind::Endpoint, v2(6.0, 0.0))[0], Some(SnapRef { target: Target::Wall(7), feat: Feat::End(1) }));
        assert_eq!(r(SnapKind::Midpoint, v2(3.0, 0.0))[0].unwrap().feat, Feat::Mid);
        assert_eq!(r(SnapKind::WallFace, v2(2.0, 0.1))[0].unwrap().feat, Feat::Side(1));
        assert_eq!(r(SnapKind::WallFace, v2(2.0, -0.1))[0].unwrap().feat, Feat::Side(-1));
        assert_eq!(r(SnapKind::WallAxis, v2(2.0, 0.0))[0].unwrap().feat, Feat::Axis);
        assert_eq!(r(SnapKind::Grid, v2(2.0, 0.0)), [None, None]);
    }

    #[test]
    fn corner_names_both_walls() {
        let ws = [w(v2(0.0, 0.0), v2(4.0, 0.0)), (8, SnapWall { a: v2(4.0, 0.0), b: v2(4.0, 3.0), half: 0.1 })];
        let r = refs_for(&Snap { p: v2(4.0, 0.0), kind: SnapKind::Endpoint }, &ws);
        assert_eq!(r[0].unwrap().feat, Feat::End(1));
        assert_eq!(r[1].unwrap(), SnapRef { target: Target::Wall(8), feat: Feat::End(0) });
    }

    #[test]
    fn off_and_alt_give_the_free_point() {
        let c = [hit(SnapKind::Endpoint, v2(0.0, 0.0)), hit(SnapKind::Free, v2(0.01, 0.0))];
        let mut s = SnapState::default();
        assert_eq!(s.resolve(v2(0.01, 0.0), 0.1, &c, Hold { free: true }).kind, SnapKind::Free);
        s.off = true;
        assert_eq!(s.resolve(v2(0.01, 0.0), 0.1, &c, Hold::default()).kind, SnapKind::Free);
        s.off = false;
        assert_eq!(s.resolve(v2(0.01, 0.0), 0.1, &c, Hold::default()).kind, SnapKind::Endpoint);
    }

    #[test]
    fn override_picks_its_kind_until_click() {
        let c = [
            hit(SnapKind::Endpoint, v2(0.0, 0.0)),
            hit(SnapKind::Midpoint, v2(0.05, 0.0)),
            hit(SnapKind::Free, v2(0.02, 0.0)),
        ];
        let mut s = SnapState { over: Some(Override(SnapKind::Midpoint)), ..Default::default() };
        assert_eq!(s.resolve(v2(0.02, 0.0), 0.1, &c, Hold::default()).kind, SnapKind::Midpoint);
        // away from that midpoint (past the hold), at an end with no midpoint near
        let only_end = [hit(SnapKind::Endpoint, v2(1.0, 0.0)), hit(SnapKind::Free, v2(1.0, 0.0))];
        assert_eq!(s.resolve(v2(1.0, 0.0), 0.1, &only_end, Hold::default()).kind, SnapKind::Free);
        s.clicked();
        assert_eq!(s.resolve(v2(1.0, 0.0), 0.1, &only_end, Hold::default()).kind, SnapKind::Endpoint);
        // 4d167a804 resolve doc: "held hit kept until the cursor leaves HOLD * r", so at (0, 0)
        // the override midpoint 0.05 away still holds; after the click the better-ranked end wins
        let mut s = SnapState { over: Some(Override(SnapKind::Midpoint)), ..Default::default() };
        assert_eq!(s.resolve(v2(0.02, 0.0), 0.1, &c, Hold::default()).kind, SnapKind::Midpoint);
        let end0 = [hit(SnapKind::Endpoint, v2(0.0, 0.0)), hit(SnapKind::Free, v2(0.0, 0.0))];
        let held = s.resolve(v2(0.0, 0.0), 0.1, &end0, Hold::default());
        assert_eq!((held.kind, held.xy()), (SnapKind::Midpoint, v2(0.05, 0.0)));
        s.clicked();
        assert_eq!(s.resolve(v2(0.0, 0.0), 0.1, &end0, Hold::default()).kind, SnapKind::Endpoint);
    }

    #[test]
    fn no_jitter_on_hysteresis_edge() {
        let r = 0.1;
        let a = hit(SnapKind::Endpoint, v2(0.0, 0.0));
        let g = hit(SnapKind::Grid, v2(0.2, 0.0));
        let mut s = SnapState::default();
        assert_eq!(s.resolve(v2(0.05, 0.0), r, &[a], Hold::default()).p, a.p);
        // out of the radius but within 1.5 r: still held
        assert_eq!(s.resolve(v2(0.14, 0.0), r, &[g], Hold::default()).p, a.p);
        // wobble across the 1.5 r edge: the answer flips once, never back
        let mut flips = 0;
        let mut last = a.p;
        for x in [0.149, 0.151, 0.149, 0.151, 0.16, 0.149, 0.151] {
            let now = s.resolve(v2(x, 0.0), r, &[g], Hold::default()).p;
            flips += (now != last) as u32;
            last = now;
        }
        assert_eq!(flips, 1);
        assert_eq!(last, g.p);
    }

    #[test]
    fn better_rank_overrides_the_held_hit() {
        let mut s = SnapState::default();
        let g = hit(SnapKind::Grid, v2(0.0, 0.0));
        let e = hit(SnapKind::Endpoint, v2(0.05, 0.0));
        s.resolve(v2(0.0, 0.0), 0.1, &[g], Hold::default());
        assert_eq!(s.resolve(v2(0.03, 0.0), 0.1, &[e, g], Hold::default()).kind, SnapKind::Endpoint);
    }
}
