// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One snap service for draw tools, modify tools and grips: candidates come from the view level's
//! walls plus grids and reference planes, held in a bucket index cached by (level, revision).
//! Arc walls and the plan outlines of freeform patches are exact curves: their ends, middle,
//! thirds, centre, nearest point, and from the last point the perpendicular and tangent.

use crate::editor::Editor;
use crate::snap::{self, Snap, SnapCtx, SnapKind, SnapWall};
use crate::snap_core::{refs_for, SnapHit, SnapRef};
use ddc_geom::nurbs::Curve;
use ddc_geom::{v2, v3, V2};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Which guide kinds a tool profile asks for; other tools keep their candidate lists unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Guides {
    pub extension: bool,
    pub parallel: bool,
}

impl Guides {
    pub const NONE: Guides = Guides { extension: false, parallel: false };
    pub const ALL: Guides = Guides { extension: true, parallel: true };
}

const CELL: f64 = 4.0;
/// Segments spanning more cells than this live in a flat list instead of the buckets.
const MAX_CELLS: i64 = 256;

type Line = (V2, V2, SnapKind);

/// An exact plan curve (z = 0): an arc wall axis with its two faces, or a patch outline.
#[derive(Clone, Debug)]
pub struct PlanCurve {
    /// Wall id (left out for its own grip), None for other elements.
    pub wall: Option<i64>,
    pub curve: Curve,
    /// Kind of the nearest point on it.
    pub kind: SnapKind,
    pub center: Option<V2>,
    /// Exact arc of an arc wall: quadrants and tangents come from the circle, not the spline.
    pub arc: Option<ddc_geom::arc::ArcAxis>,
    /// Wall faces: nearest points on them.
    pub faces: Vec<Curve>,
    /// Plan box of all control points (the curves lie inside it).
    pub bb: [f64; 4],
}

impl PlanCurve {
    pub fn new(wall: Option<i64>, curve: Curve, kind: SnapKind, center: Option<V2>, faces: Vec<Curve>) -> Self {
        let mut bb = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for p in curve.ctrl().iter().chain(faces.iter().flat_map(|f| f.ctrl().iter())) {
            bb = [bb[0].min(p.x), bb[1].min(p.y), bb[2].max(p.x), bb[3].max(p.y)];
        }
        PlanCurve { wall, curve, kind, center, arc: None, faces, bb }
    }

    /// Exact axis of an arc wall and its faces at `half` on either side.
    pub fn arc_wall(id: i64, a: V2, b: V2, bulge: f64, half: f64) -> Option<Self> {
        let ax = ddc_geom::arc::ArcAxis::new(a, b, bulge, ddc_geom::Tol::default()).ok()?;
        let arc = |r: f64| {
            let c = v3(ax.center.x, ax.center.y, 0.0);
            // a clockwise arc runs with the angle in the mirrored frame
            let (y, s) = if ax.sweep >= 0.0 { (1.0, 1.0) } else { (-1.0, -1.0) };
            let a0 = s * ax.start;
            Curve::arc(c, v3(1.0, 0.0, 0.0), v3(0.0, y, 0.0), r, a0, a0 + ax.sweep.abs()).ok()
        };
        let axis = arc(ax.radius)?;
        let faces = [ax.radius + half, ax.radius - half]
            .into_iter()
            .filter(|r| half > 0.0 && *r > 1e-6)
            .filter_map(arc)
            .collect();
        let mut pc = PlanCurve::new(Some(id), axis, SnapKind::WallAxis, Some(ax.center), faces);
        pc.arc = Some(ax);
        Some(pc)
    }

    fn near(&self, p: V2, r: f64) -> bool {
        p.x >= self.bb[0] - r && p.y >= self.bb[1] - r && p.x <= self.bb[2] + r && p.y <= self.bb[3] + r
    }

    /// Candidates of this curve within `r` of `cursor`.
    fn snaps(&self, cursor: V2, from: Option<V2>, r: f64, out: &mut Vec<Snap>) {
        let c3 = v3(cursor.x, cursor.y, 0.0);
        let mut push = |p: ddc_geom::V3, kind: SnapKind| {
            let p = v2(p.x, p.y);
            if (p - cursor).len() <= r {
                out.push(Snap { p, kind });
            }
        };
        let cv = &self.curve;
        let (t0, t1) = cv.domain();
        push(cv.point(t0), SnapKind::Endpoint);
        push(cv.point(t1), SnapKind::Endpoint);
        push(cv.point(cv.at_fraction(0.5)), SnapKind::Midpoint);
        for f in [1.0 / 3.0, 2.0 / 3.0] {
            push(cv.point(cv.at_fraction(f)), SnapKind::Param);
        }
        if let Some(c) = self.center {
            push(v3(c.x, c.y, 0.0), SnapKind::Center);
        }
        if let Some(ax) = &self.arc {
            for q in ddc_geom::arc::quadrants(ax) {
                push(v3(q.x, q.y, 0.0), SnapKind::Quadrant);
            }
        }
        push(cv.closest(c3).1, self.kind);
        for f in &self.faces {
            push(f.closest(c3).1, SnapKind::WallFace);
        }
        if let Some(o) = from {
            let o3 = v3(o.x, o.y, 0.0);
            for t in cv.perpendicular_feet(o3) {
                push(cv.point(t), SnapKind::Perpendicular);
            }
            match &self.arc {
                Some(ax) => {
                    let tol = ddc_geom::Tol::default();
                    for q in ddc_geom::arc::tangent_points(ax.center, ax.radius, o).into_iter().flatten() {
                        if ax.contains(q, tol).is_some() {
                            push(v3(q.x, q.y, 0.0), SnapKind::Tangent);
                        }
                    }
                }
                None => {
                    for t in cv.tangent_points(o3, 1e-9) {
                        push(cv.point(t), SnapKind::Tangent);
                    }
                }
            }
        }
    }
}

#[derive(Default)]
pub struct Index {
    key: Option<(i64, u64)>,
    walls: Vec<(i64, SnapWall)>,
    cells: HashMap<(i64, i64), Vec<u32>>,
    long: Vec<u32>,
    lines: Vec<Line>,
    curves: Vec<PlanCurve>,
}

fn cell(v: f64) -> i64 {
    (v / CELL).floor() as i64
}

impl Index {
    fn build(walls: Vec<(i64, SnapWall)>, lines: Vec<Line>, curves: Vec<PlanCurve>) -> Self {
        let mut ix = Index {
            key: None,
            walls,
            lines,
            curves,
            ..Default::default()
        };
        for (i, (_, w)) in ix.walls.iter().enumerate() {
            let (x0, x1) = (cell(w.a.x.min(w.b.x)), cell(w.a.x.max(w.b.x)));
            let (y0, y1) = (cell(w.a.y.min(w.b.y)), cell(w.a.y.max(w.b.y)));
            if (x1 - x0 + 1) * (y1 - y0 + 1) > MAX_CELLS {
                ix.long.push(i as u32);
                continue;
            }
            for x in x0..=x1 {
                for y in y0..=y1 {
                    ix.cells.entry((x, y)).or_default().push(i as u32);
                }
            }
        }
        ix
    }

    /// Axes of walls (except `skip`) and grid lines within `reach` of `p`: the references for
    /// temporary dimensions. Reads only the buckets around `p`, never every wall.
    pub fn axes_near(&self, p: V2, reach: f64, skip: Option<i64>) -> Vec<(V2, V2)> {
        let mut ids: Vec<u32> = self.long.clone();
        for x in cell(p.x - reach)..=cell(p.x + reach) {
            for y in cell(p.y - reach)..=cell(p.y + reach) {
                if let Some(v) = self.cells.get(&(x, y)) {
                    ids.extend(v);
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        let near = |a: V2, b: V2| (snap::closest_on_segment(p, a, b).0 - p).len() <= reach;
        let mut out: Vec<(V2, V2)> = ids
            .iter()
            .map(|&i| &self.walls[i as usize])
            .filter(|(id, w)| Some(*id) != skip && near(w.a, w.b))
            .map(|(_, w)| (w.a, w.b))
            .collect();
        out.extend(
            self.lines
                .iter()
                .filter(|l| l.2 == SnapKind::GridAxis)
                .map(|l| (l.0, l.1)),
        );
        out
    }

    /// Candidates around `cursor`, best first, the free point last. `plane` is the work plane
    /// trace near the cursor: a snap line of its own that crosses the axes too.
    pub fn candidates(
        &self,
        cursor: V2,
        from: Option<V2>,
        grid: f64,
        radius: f64,
        skip: Option<i64>,
        plane: Option<(V2, V2)>,
    ) -> Vec<Snap> {
        self.candidates_with(cursor, from, grid, radius, skip, plane, Guides::NONE)
    }

    /// [`Self::candidates`] plus the guide kinds the tool's profile asks for.
    #[allow(clippy::too_many_arguments)]
    pub fn candidates_with(
        &self,
        cursor: V2,
        from: Option<V2>,
        grid: f64,
        radius: f64,
        skip: Option<i64>,
        plane: Option<(V2, V2)>,
        guides: Guides,
    ) -> Vec<Snap> {
        let mut ids: Vec<u32> = self.long.clone();
        for x in cell(cursor.x - radius)..=cell(cursor.x + radius) {
            for y in cell(cursor.y - radius)..=cell(cursor.y + radius) {
                if let Some(v) = self.cells.get(&(x, y)) {
                    ids.extend(v);
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        let near: Vec<&(i64, SnapWall)> = ids
            .iter()
            .map(|&i| &self.walls[i as usize])
            .filter(|(id, w)| {
                let d = (snap::closest_on_segment(cursor, w.a, w.b).0 - cursor).len();
                Some(*id) != skip && d <= radius + w.half
            })
            .collect();
        let sw: Vec<SnapWall> = near.iter().map(|(_, w)| *w).collect();
        let ctx = SnapCtx {
            walls: &sw,
            draft: &[],
            from,
            grid,
            radius,
        };
        let mut out = snap::candidates(&ctx, cursor);
        let lines: Vec<Line> = self
            .lines
            .iter()
            .copied()
            .chain(sw.iter().map(|w| (w.a, w.b, SnapKind::WallAxis)))
            .chain(plane.map(|(a, b)| (a, b, SnapKind::WorkPlane)))
            .filter(|l| (snap::closest_on_segment(cursor, l.0, l.1).0 - cursor).len() <= radius)
            .collect();
        let mut extra = snap::datum_candidates(&lines, cursor, radius);
        for c in self.curves.iter().filter(|c| c.wall.is_none_or(|w| Some(w) != skip)) {
            if c.near(cursor, radius) {
                c.snaps(cursor, from, radius, &mut extra);
            }
        }
        if guides.extension || guides.parallel {
            let reach: Vec<(V2, V2)> = self
                .walls_around(cursor, snap::EXT_REACH)
                .into_iter()
                .filter(|(id, w)| Some(*id) != skip && (snap::closest_on_segment(cursor, w.a, w.b).0 - cursor).len() <= snap::EXT_REACH)
                .map(|(_, w)| (w.a, w.b))
                .collect();
            if guides.extension {
                extra.extend(snap::extension_candidates(&reach, cursor, radius));
            }
            if let (Some(f), true) = (from, guides.parallel) {
                extra.extend(snap::parallel_candidates(&reach, f, cursor, radius));
            }
        }
        snap::merge(&mut out, extra, cursor);
        out
    }

    /// Like [`Self::candidates_with`], each hit naming the walls it lies on.
    #[allow(clippy::too_many_arguments)]
    pub fn hits(
        &self,
        cursor: V2,
        from: Option<V2>,
        grid: f64,
        radius: f64,
        skip: Option<i64>,
        plane: Option<(V2, V2)>,
        guides: Guides,
    ) -> Vec<SnapHit> {
        self.candidates_with(cursor, from, grid, radius, skip, plane, guides)
            .into_iter()
            .map(|s| SnapHit { refs: self.refs_at(&s, skip), ..SnapHit::bare(s) })
            .collect()
    }

    /// Walls (except `skip`) a candidate lies on.
    pub fn refs_at(&self, s: &Snap, skip: Option<i64>) -> [Option<SnapRef>; 2] {
        if !matches!(
            s.kind,
            SnapKind::Endpoint | SnapKind::Midpoint | SnapKind::WallAxis | SnapKind::WallFace | SnapKind::Perpendicular | SnapKind::Intersection
        ) {
            return [None, None];
        }
        let near: Vec<(i64, SnapWall)> = self.walls_around(s.p, 1e-6).into_iter().filter(|(id, _)| Some(*id) != skip).collect();
        refs_for(s, &near)
    }

    /// Walls whose buckets touch the box of `p` grown by `reach` (a superset of the walls within it).
    fn walls_around(&self, p: V2, reach: f64) -> Vec<(i64, SnapWall)> {
        let mut ids: Vec<u32> = self.long.clone();
        for x in cell(p.x - reach)..=cell(p.x + reach) {
            for y in cell(p.y - reach)..=cell(p.y + reach) {
                if let Some(v) = self.cells.get(&(x, y)) {
                    ids.extend(v);
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids.iter().map(|&i| self.walls[i as usize]).collect()
    }
}

/// Editor-side holder: interior mutability so hover paths keep `&Editor`.
#[derive(Default)]
pub struct Service {
    ix: Mutex<Index>,
    /// How many times the index was rebuilt.
    pub builds: AtomicUsize,
}

impl Service {
    fn with<R>(
        &self,
        key: (i64, u64),
        load: impl FnOnce() -> Index,
        f: impl FnOnce(&Index) -> R,
    ) -> R {
        let mut g = self.ix.lock().unwrap_or_else(|e| e.into_inner());
        if g.key != Some(key) {
            *g = load();
            g.key = Some(key);
            self.builds.fetch_add(1, Ordering::Relaxed);
        }
        f(&g)
    }
}

impl Editor {
    fn snap_index(&self) -> Index {
        let half = |w: &crate::editor::Wall| self.type_by_id(w.wall_type).map_or(0.0, |t| t.thickness() / 2.0);
        // an arc wall's chord is not on the wall: arcs snap as exact curves
        let mut curves: Vec<PlanCurve> = self
            .walls
            .iter()
            .filter(|w| w.bulge != 0.0)
            .filter_map(|w| PlanCurve::arc_wall(w.id, w.a, w.b, w.bulge, half(w)))
            .collect();
        let arcs: Vec<i64> = curves.iter().filter_map(|c| c.wall).collect();
        let walls = self
            .walls
            .iter()
            .filter(|w| !arcs.contains(&w.id))
            .map(|w| {
                let sw = SnapWall {
                    a: w.a,
                    b: w.b,
                    half: half(w),
                };
                (w.id, sw)
            })
            .collect();
        // plan outlines of the level's freeform patches (the boundary projected to z = 0)
        for (_, s) in self.model.freeforms(Some(self.level)).unwrap_or_default() {
            for b in s.surface.boundary().map(|b| b.to_vec()).unwrap_or_default() {
                let flat: Vec<ddc_geom::V3> = b.ctrl().iter().map(|p| v3(p.x, p.y, 0.0)).collect();
                let Ok(c) = Curve::new(b.degree(), b.knots().to_vec(), flat, b.weights().to_vec()) else {
                    continue;
                };
                // an edge that projects to a point gives nothing to snap to
                if c.length() > 1e-6 {
                    curves.push(PlanCurve::new(None, c, SnapKind::Nearest, None, vec![]));
                }
            }
        }
        let mut lines: Vec<Line> = self
            .model
            .plan_grid_axes(self.level)
            .unwrap_or_default()
            .into_iter()
            .map(|(g, _)| (v2(g.a.0, g.a.1), v2(g.b.0, g.b.1), SnapKind::GridAxis))
            .collect();
        lines.extend(
            self.model
                .ref_planes()
                .unwrap_or_default()
                .into_iter()
                .map(|r| (v2(r.1 .0, r.1 .1), v2(r.2 .0, r.2 .1), SnapKind::RefPlane)),
        );
        Index::build(walls, lines, curves)
    }

    /// Walls a snap candidate lies on, from the cached index of the view level.
    pub fn snap_refs(&self, s: &Snap, skip: Option<i64>) -> [Option<SnapRef>; 2] {
        self.snap_svc.with((self.level, self.revision), || self.snap_index(), |ix| ix.refs_at(s, skip))
    }

    /// Reference axes near `p` on the view level, from the cached index.
    pub fn ref_axes_near(&self, p: V2, reach: f64, skip: Option<i64>) -> Vec<(V2, V2)> {
        self.snap_svc.with(
            (self.level, self.revision),
            || self.snap_index(),
            |ix| ix.axes_near(p, reach, skip),
        )
    }

    /// Snap candidates for a plan point on the view level; `skip` leaves one wall out (its own grip).
    pub fn snap_list(
        &self,
        cursor: V2,
        from: Option<V2>,
        radius: f64,
        skip: Option<i64>,
    ) -> Vec<Snap> {
        // the plane is view state, not in the cached index: its trace is cut near the cursor
        let plane = crate::wp_views::snap_line(self, cursor, radius * 4.0);
        self.snap_svc.with(
            (self.level, self.revision),
            || self.snap_index(),
            |ix| ix.candidates(cursor, from, self.tool.grid, radius, skip, plane),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modify_tools::tests::walls;

    fn first(l: &[Snap]) -> Snap {
        l[0]
    }

    #[test]
    fn offers_wall_end_and_midpoint() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (5.0, 0.0))]);
        let s = first(&ed.snap_list(v2(5.004, 0.003), None, 0.15, None));
        assert_eq!((s.kind, s.p), (SnapKind::Endpoint, v2(5.0, 0.0)));
        let s = first(&ed.snap_list(v2(2.52, 0.01), None, 0.15, None));
        assert_eq!((s.kind, s.p), (SnapKind::Midpoint, v2(2.5, 0.0)));
    }

    fn grid_index(n: i64) -> Index {
        let walls = (0..n)
            .map(|i| {
                let (x, y) = ((i % 100) as f64 * 5.0, (i / 100) as f64 * 5.0);
                (i, SnapWall { a: v2(x, y), b: v2(x + 4.0, y), half: 0.1 })
            })
            .collect();
        Index::build(walls, vec![], vec![])
    }

    #[test]
    fn hits_carry_wall_refs() {
        use crate::snap_core::{Feat, SnapRef};
        use ddc_model::Target;
        let ix = grid_index(4);
        let h = ix.hits(v2(4.01, 0.0), None, 0.0, 0.15, None, None, Guides::NONE);
        assert_eq!(h[0].kind, SnapKind::Endpoint);
        assert_eq!(h[0].refs[0], Some(SnapRef { target: Target::Wall(0), feat: Feat::End(1) }));
        assert_eq!(h.last().unwrap().refs, [None, None]);
        let skipped = ix.hits(v2(4.01, 0.0), None, 0.0, 0.15, Some(0), None, Guides::NONE);
        assert!(skipped.iter().all(|s| s.refs[0].is_none()));
    }

    /// The same cursor on screen (same pixels from a wall end) snaps at every zoom.
    #[test]
    fn scale_invariant_px_radius() {
        use crate::snap_refine::{radius_m, SNAP_PX_DEFAULT};
        // one wall: at 10 px/m the radius is 1.2 m and would reach the next wall of a grid
        let ix = grid_index(1);
        for ppm in [10.0, 50.0, 400.0] {
            let r = radius_m(SNAP_PX_DEFAULT, ppm);
            let inside = ix.hits(v2(4.0 + 0.9 * r, 0.0), None, 0.0, r, None, None, Guides::NONE);
            assert_eq!(inside[0].kind, SnapKind::Endpoint, "ppm {ppm}");
            let outside = ix.hits(v2(4.0 + 1.3 * r, 0.3 * r), None, 0.0, r, None, None, Guides::NONE);
            assert_ne!(outside[0].kind, SnapKind::Endpoint, "ppm {ppm}");
        }
    }

    /// 10 000 walls, 5 m apart: a query reads the buckets around the cursor, guides reach 6 m.
    #[test]
    fn query_10k_walls() {
        let ix = grid_index(10_000);
        let t = std::time::Instant::now();
        for i in 0..200 {
            let p = v2(100.0 + (i % 50) as f64 * 3.1, 80.0 + (i % 7) as f64);
            ix.hits(p, Some(v2(90.0, 85.0)), 0.0, 0.12, None, None, Guides::ALL);
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / 200.0;
        let budget = if cfg!(debug_assertions) { 20.0 } else { 2.0 };
        assert!(ms <= budget, "{ms} ms per query, budget {budget}");
    }

    /// Every kind of the plan table: the cursor near the exact point answers with that kind at that
    /// point, to 1e-9 (the radius is the only tolerance).
    /// Seven captured points and 10 000 walls: the guides cost one pass over the seven.
    #[test]
    fn query_10k_walls_with_7_acquired() {
        let ix = grid_index(10_000);
        let acq: Vec<V2> = (0..7).map(|i| v2(100.0 + i as f64 * 7.0, 90.0)).collect();
        let t = std::time::Instant::now();
        for i in 0..200 {
            let p = v2(100.0 + (i % 50) as f64 * 3.1, 80.0 + (i % 7) as f64);
            let mut l = ix.hits(p, Some(v2(90.0, 85.0)), 0.0, 0.12, None, None, Guides::ALL);
            let extra = snap::tracking_candidates(&acq, p, 0.12);
            let mut bare: Vec<Snap> = l.iter().map(|h| Snap { p: h.xy(), kind: h.kind }).collect();
            snap::merge(&mut bare, extra, p);
            l.truncate(0);
            assert_eq!(bare.last().map(|s| s.kind), Some(SnapKind::Free));
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / 200.0;
        let budget = if cfg!(debug_assertions) { 20.0 } else { 2.0 };
        assert!(ms <= budget, "{ms} ms per query, budget {budget}");
    }

    #[test]
    fn kinds_table() {
        use std::f64::consts::PI;
        let walls = vec![
            (1, SnapWall { a: v2(0.0, 0.0), b: v2(4.0, 0.0), half: 0.1 }),
            (2, SnapWall { a: v2(0.0, 10.0), b: v2(4.0, 13.0), half: 0.1 }),
        ];
        let lines = vec![
            // the axis reaches the reference plane at y = 8: datum crossings are within both segments
            (v2(10.0, -5.0), v2(10.0, 10.0), SnapKind::GridAxis),
            (v2(-5.0, 8.0), v2(20.0, 8.0), SnapKind::RefPlane),
        ];
        // half circle round (22, 0), radius 2, below the axis: ends at angles pi and 2 pi
        let arc = PlanCurve::arc_wall(3, v2(20.0, 0.0), v2(24.0, 0.0), 1.0, 0.1).expect("arc");
        let ix = Index::build(walls, lines, vec![arc]);
        let on_arc = |a: f64| v2(22.0 + 2.0 * a.cos(), 2.0 * a.sin());
        let plane = Some((v2(-5.0, -3.0), v2(20.0, -3.0)));
        let (c, a) = (v2(22.0, 0.0), 4.2);
        let rows: Vec<(&str, SnapKind, V2, Option<V2>, f64, Option<(V2, V2)>, V2)> = vec![
            ("endpoint e", SnapKind::Endpoint, v2(4.05, 0.03), None, 0.0, None, v2(4.0, 0.0)),
            ("endpoint s", SnapKind::Endpoint, v2(-0.04, 0.0), None, 0.0, None, v2(0.0, 0.0)),
            ("midpoint", SnapKind::Midpoint, v2(2.03, 0.02), None, 0.0, None, v2(2.0, 0.0)),
            ("axis", SnapKind::WallAxis, v2(1.0, 0.04), None, 0.0, None, v2(1.0, 0.0)),
            ("face left", SnapKind::WallFace, v2(1.0, 0.12), None, 0.0, None, v2(1.0, 0.1)),
            ("face right", SnapKind::WallFace, v2(1.0, -0.12), None, 0.0, None, v2(1.0, -0.1)),
            ("perpendicular", SnapKind::Perpendicular, v2(1.52, 0.03), Some(v2(1.5, 2.0)), 0.0, None, v2(1.5, 0.0)),
            ("grid", SnapKind::Grid, v2(7.04, 3.03), None, 1.0, None, v2(7.0, 3.0)),
            ("angle 45", SnapKind::Angle(45), v2(3.03, -1.98), Some(v2(0.0, -5.0)), 0.0, None, v2(3.025, -1.975)),
            ("grid axis", SnapKind::GridAxis, v2(10.05, 3.0), None, 0.0, None, v2(10.0, 3.0)),
            ("ref plane", SnapKind::RefPlane, v2(6.0, 8.05), None, 0.0, None, v2(6.0, 8.0)),
            ("intersection", SnapKind::Intersection, v2(10.04, 7.95), None, 0.0, None, v2(10.0, 8.0)),
            ("work plane", SnapKind::WorkPlane, v2(6.0, -3.04), None, 0.0, plane, v2(6.0, -3.0)),
            ("arc axis", SnapKind::WallAxis, c + (on_arc(a) - c) * 1.015, None, 0.0, None, on_arc(a)),
            ("arc centre", SnapKind::Center, v2(22.03, 0.02), None, 0.0, None, c),
            ("arc third", SnapKind::Param, on_arc(PI + PI / 3.0) + v2(0.02, -0.01), None, 0.0, None, on_arc(PI + PI / 3.0)),
            ("arc quadrant", SnapKind::Quadrant, v2(22.02, -2.03), None, 0.0, None, v2(22.0, -2.0)),
            ("arc tangent", SnapKind::Tangent, v2(23.02, -1.75), Some(v2(26.0, 0.0)), 0.0, None, v2(23.0, -(3f64.sqrt()))),
            ("extension", SnapKind::Extension, v2(6.5, 0.05), None, 0.0, None, v2(6.5, 0.0)),
            ("extension start", SnapKind::Extension, v2(-3.0, -0.05), None, 0.0, None, v2(-3.0, 0.0)),
            ("parallel", SnapKind::Parallel, v2(8.382, 13.824), Some(v2(6.0, 12.0)), 0.0, None, v2(6.0 + 3.0 * 0.8, 12.0 + 3.0 * 0.6)),
        ];
        for (name, kind, cur, from, grid, plane, want) in rows {
            let l = ix.candidates_with(cur, from, grid, 0.15, None, plane, Guides::ALL);
            let hit = l.iter().find(|s| s.kind == kind && (s.p - want).len() < 1e-9);
            assert!(hit.is_some(), "{name}: want {kind:?} at {want:?}, got {l:?}");
        }
    }

    #[test]
    fn half_circle_wall_offers_one_quadrant() {
        let c = PlanCurve::arc_wall(1, v2(0.0, 0.0), v2(4.0, 0.0), 1.0, 0.1).expect("arc");
        let ix = Index::build(vec![], vec![], vec![c]);
        let q = |p: V2| ix.candidates(p, None, 0.0, 0.15, None, None).into_iter().any(|s| s.kind == SnapKind::Quadrant && (s.p - v2(2.0, p.y.signum() * 2.0)).len() < 1e-9);
        // exactly one of the two apexes (2, +-2) lies on the half circle
        assert_ne!(q(v2(2.0, 2.01)), q(v2(2.0, -2.01)));
    }

    #[test]
    fn extension_of_a_wall_beyond_its_end() {
        let ix = grid_index(1);
        let none = ix.candidates(v2(6.0, 0.05), None, 0.0, 0.15, None, None);
        assert!(none.iter().all(|s| s.kind != SnapKind::Extension), "off by default");
        let s = ix.candidates_with(v2(6.0, 0.05), None, 0.0, 0.15, None, None, Guides::ALL);
        let e = s.iter().find(|s| s.kind == SnapKind::Extension).expect("extension");
        assert_eq!(e.p, v2(6.0, 0.0));
    }

    fn near(a: V2, b: V2) -> bool {
        (a - b).len() < 1e-9
    }

    #[test]
    fn arc_wall_snaps_exactly_on_the_arc_not_the_chord() {
        let mut ed = Editor::new_in_memory().unwrap();
        let ty = ed.current_type().expect("type").id;
        let spec = ddc_model::WallSpec {
            a: (5.0, 0.0),
            b: (-5.0, 0.0),
            level: ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ty,
            offset0: None,
        };
        // a half circle of radius 5 round the origin through (0, 5)
        let id = ed.model.add_arc_wall(&spec, 1.0).unwrap();
        ed.reload().unwrap();
        let half = ed.type_by_id(ty).unwrap().thickness() / 2.0;
        // the middle of a half circle is its apex: the quadrant ranks with ends and wins the point
        let s = first(&ed.snap_list(v2(0.01, 5.02), None, 0.15, None));
        assert!(s.kind == SnapKind::Quadrant && near(s.p, v2(0.0, 5.0)), "{s:?}");
        let third = v2(2.5, 2.5 * 3f64.sqrt());
        let s = first(&ed.snap_list(third + v2(0.02, -0.01), None, 0.15, None));
        assert!(s.kind == SnapKind::Param && near(s.p, third), "{s:?}");
        let s = first(&ed.snap_list(v2(0.02, 0.01), None, 0.15, None));
        assert!(s.kind == SnapKind::Center && near(s.p, v2(0.0, 0.0)), "{s:?}");
        // tangent from (13, 0): the touching point at (25/13, 60/13)
        let t = v2(25.0 / 13.0, 60.0 / 13.0);
        let s = first(&ed.snap_list(t + v2(0.03, 0.02), Some(v2(13.0, 0.0)), 0.15, None));
        assert!(s.kind == SnapKind::Tangent && near(s.p, t), "{s:?}");
        // nearest on the axis and on a face lie on their exact circles
        let dir = v2(0.6, 0.8);
        let l = ed.snap_list(dir * 5.03, None, 0.15, None);
        let ax = l.iter().find(|s| s.kind == SnapKind::WallAxis).expect("axis");
        assert!((ax.p.len() - 5.0).abs() < 1e-9, "{ax:?}");
        let l = ed.snap_list(dir * (5.0 + half + 0.01), None, 0.15, None);
        let f = l.iter().find(|s| s.kind == SnapKind::WallFace).expect("face");
        assert!((f.p.len() - 5.0 - half).abs() < 1e-9, "{f:?}");
        // the chord is not the wall
        let l = ed.snap_list(v2(3.0, 0.02), None, 0.15, None);
        assert!(l.iter().all(|s| s.kind == SnapKind::Free || s.kind == SnapKind::Grid), "{l:?}");
        // its own grip leaves it out
        let l = ed.snap_list(v2(0.01, 5.02), None, 0.15, Some(id));
        assert!(l.iter().all(|s| s.kind == SnapKind::Free || s.kind == SnapKind::Grid), "{l:?}");
    }

    #[test]
    fn freeform_outline_snaps_in_plan() {
        use ddc_geom::nurbs;
        let mut ed = Editor::new_in_memory().unwrap();
        let p = |x: f64, y: f64| v3(x, y, 1.0 + 0.1 * x);
        let k = vec![0.0, 0.0, 1.0, 1.0];
        let ctrl = vec![p(20.0, 20.0), p(20.0, 23.0), p(24.0, 20.0), p(24.0, 23.0)];
        let surface = nurbs::Surface::new(1, 1, k.clone(), k, 2, 2, ctrl, vec![1.0; 4]).unwrap();
        ed.model
            .add_freeform(&ddc_model::FreeformSpec {
                level: ed.level,
                name: "Canopy".into(),
                material: "Glulam".into(),
                surface,
                thickness: 0.0,
                side: ddc_model::ShellSide::Normal,
                trim: nurbs::Trim::default(),
                origin: "points".into(),
            })
            .unwrap();
        ed.reload().unwrap();
        let s = first(&ed.snap_list(v2(22.03, 20.02), None, 0.15, None));
        assert!(s.kind == SnapKind::Midpoint && near(s.p, v2(22.0, 20.0)), "{s:?}");
        let s = first(&ed.snap_list(v2(20.02, 23.04), None, 0.15, None));
        assert!(s.kind == SnapKind::Endpoint && near(s.p, v2(20.0, 23.0)), "{s:?}");
        let l = ed.snap_list(v2(24.05, 21.1), None, 0.15, None);
        let n = l.iter().find(|s| s.kind == SnapKind::Nearest).expect("nearest");
        assert!(near(n.p, v2(24.0, 21.1)), "{n:?}");
        // perpendicular from a point left of the patch onto its far edge (off the edge's middle,
        // which outranks it on the same point)
        let l = ed.snap_list(v2(24.04, 22.0), Some(v2(10.0, 22.0)), 0.15, None);
        assert!(l.iter().any(|s| s.kind == SnapKind::Perpendicular && near(s.p, v2(24.0, 22.0))), "{l:?}");
    }

    #[test]
    fn axes_near_reads_buckets_not_all_walls() {
        let mut ed = Editor::new_in_memory().unwrap();
        let segs: Vec<_> = (0..400)
            .map(|i| ((i as f64 * 10.0, 0.0), (i as f64 * 10.0 + 5.0, 0.0)))
            .collect();
        walls(&mut ed, &segs);
        let b = ed.snap_svc.builds.load(Ordering::Relaxed);
        for _ in 0..50 {
            let r = ed.ref_axes_near(v2(2.0, 3.0), 12.0, None);
            assert!(!r.is_empty() && r.len() <= 3, "{}", r.len());
        }
        assert!(ed.snap_svc.builds.load(Ordering::Relaxed) <= b + 1);
    }

    #[test]
    fn other_level_is_never_offered_and_cache_follows_key() {
        let mut ed = Editor::new_in_memory().unwrap();
        let l0 = ed.level;
        walls(&mut ed, &[((0.0, 0.0), (5.0, 0.0))]);
        let l1 = ed.add_level("L2", 3.0).unwrap();
        walls(&mut ed, &[((20.0, 0.0), (25.0, 0.0))]);
        let near_l0 = v2(5.0, 0.0);
        let l = ed.snap_list(near_l0, None, 0.15, None);
        assert!(l.iter().all(|s| s.kind == SnapKind::Free || s.kind == SnapKind::Grid));
        ed.set_level(l0);
        let b = ed.snap_svc.builds.load(Ordering::Relaxed);
        assert_eq!(first(&ed.snap_list(near_l0, None, 0.15, None)).kind, SnapKind::Endpoint);
        let l = ed.snap_list(v2(20.0, 0.0), None, 0.15, None);
        assert!(l.iter().all(|s| s.kind == SnapKind::Free || s.kind == SnapKind::Grid));
        ed.snap_list(near_l0, None, 0.15, None);
        // the same level and revision: no rebuild between queries
        assert_eq!(ed.snap_svc.builds.load(Ordering::Relaxed), b + 1);
        let _ = l1;
        ed.set_level(l1);
        ed.snap_list(near_l0, None, 0.15, None);
        assert_eq!(ed.snap_svc.builds.load(Ordering::Relaxed), b + 2);
        ed.reload().unwrap();
        ed.snap_list(near_l0, None, 0.15, None);
        assert_eq!(ed.snap_svc.builds.load(Ordering::Relaxed), b + 3);
    }

    fn cost_of_idle_frames(n: usize) -> (usize, usize, usize) {
        use crate::grips::{self, Kind};
        let mut ed = Editor::new_in_memory().unwrap();
        let segs: Vec<_> = (0..n)
            .map(|i| ((i as f64 * 10.0, 0.0), (i as f64 * 10.0 + 5.0, 0.0)))
            .collect();
        walls(&mut ed, &segs);
        let id = ed.walls[0].id;
        let g = grips::grips_in(&ed.model, Kind::Wall, id, &grips::view_key(&ed))
            .unwrap()
            .remove(0);
        ed.grip.last = Some((Kind::Wall, id, g.role.clone()));
        let (b0, d0, p0) = (
            ed.snap_svc.builds.load(Ordering::Relaxed),
            ed.grip.cache.dim_builds.load(Ordering::Relaxed),
            ed.big.plan_builds.load(Ordering::Relaxed),
        );
        for _ in 0..100 {
            grips::last_dims(&ed);
            ed.big.plan_visible(&ed, v2(1.0, 0.0), v2(1.0, 0.0));
        }
        (
            ed.snap_svc.builds.load(Ordering::Relaxed) - b0,
            ed.grip.cache.dim_builds.load(Ordering::Relaxed) - d0,
            ed.big.plan_builds.load(Ordering::Relaxed) - p0,
        )
    }

    #[test]
    fn idle_frames_rebuild_nothing_and_cost_does_not_grow_with_the_model() {
        let small = cost_of_idle_frames(20);
        let large = cost_of_idle_frames(2100);
        // 100 frames: one dimension build, at most one snap build, at most one plan build
        assert!(small.0 <= 1 && small.1 == 1, "{small:?}");
        assert!(large.0 <= 1 && large.1 == 1 && large.2 <= 1, "{large:?}");
    }
}
