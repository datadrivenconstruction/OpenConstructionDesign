// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Alignment tracking: the cursor lines up with a remote point (wall end, midpoint, face corner)
//! it never touched. Points are kept sorted by X and by Y, so a query is a binary search plus
//! a short scan, independent of the wall count.

use crate::snap::{Snap, SnapKind, SnapWall};
use ddc_geom::{v2, V2};

/// Entries examined per axis inside the tolerance window.
const SCAN: usize = 48;

#[derive(Default)]
pub struct AlignIndex {
    key: u64,
    xs: Vec<(f64, V2)>,
    ys: Vec<(f64, V2)>,
}

impl AlignIndex {
    /// Hash of the wall set: the index is rebuilt only when it changes.
    pub fn fingerprint(walls: &[SnapWall]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ walls.len() as u64;
        for w in walls {
            for v in [w.a.x, w.a.y, w.b.x, w.b.y, w.half] {
                h = (h ^ v.to_bits()).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        h
    }

    /// Ends, midpoints and face corners.
    pub fn points(walls: &[SnapWall]) -> Vec<V2> {
        let mut out = Vec::with_capacity(walls.len() * 4);
        for w in walls {
            out.push(w.a);
            out.push(w.b);
            out.push((w.a + w.b) * 0.5);
            let len = (w.b - w.a).len();
            if w.half > 0.0 && len > 0.0 {
                let n = v2(-(w.b - w.a).y / len, (w.b - w.a).x / len) * w.half;
                for s in [1.0, -1.0] {
                    out.push(w.a + n * s);
                    out.push(w.b + n * s);
                }
            }
        }
        out
    }

    pub fn build(walls: &[SnapWall]) -> Self {
        let pts: Vec<V2> = Self::points(walls)
            .into_iter()
            .filter(|p| p.x.is_finite() && p.y.is_finite())
            .collect();
        let mut xs: Vec<(f64, V2)> = pts.iter().map(|&p| (p.x, p)).collect();
        let mut ys: Vec<(f64, V2)> = pts.iter().map(|&p| (p.y, p)).collect();
        xs.sort_by(|a, b| a.0.total_cmp(&b.0));
        ys.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self {
            key: Self::fingerprint(walls),
            xs,
            ys,
        }
    }

    pub fn refresh(&mut self, walls: &[SnapWall]) {
        if self.key != Self::fingerprint(walls) {
            *self = Self::build(walls);
        }
    }

    /// Nearest point to the cursor whose coordinate matches within `r` and that lies farther
    /// than `2r` (closer points are handled by the ordinary end and midpoint snaps).
    fn near(list: &[(f64, V2)], v: f64, cursor: V2, r: f64) -> Option<V2> {
        let lo = list.partition_point(|e| e.0 < v - r);
        let mut best: Option<(f64, V2)> = None;
        for e in list[lo..].iter().take(SCAN).take_while(|e| e.0 <= v + r) {
            let d = (e.1 - cursor).len();
            if d > 2.0 * r && best.map_or(true, |b| d < b.0) {
                best = Some((d, e.1));
            }
        }
        best.map(|b| b.1)
    }

    fn pick(&self, extra: &[V2], cursor: V2, r: f64) -> (Option<V2>, Option<V2>) {
        let mut sx = Self::near(&self.xs, cursor.x, cursor, r);
        let mut sy = Self::near(&self.ys, cursor.y, cursor, r);
        for &p in extra {
            let d = (p - cursor).len();
            if d <= 2.0 * r {
                continue;
            }
            if (p.x - cursor.x).abs() <= r && sx.map_or(true, |q| d < (q - cursor).len()) {
                sx = Some(p);
            }
            if (p.y - cursor.y).abs() <= r && sy.map_or(true, |q| d < (q - cursor).len()) {
                sy = Some(p);
            }
        }
        (sx, sy)
    }

    /// Candidates: dual (both lines), then X, then Y. `extra` are draft points.
    pub fn candidates(&self, extra: &[V2], cursor: V2, r: f64) -> Vec<Snap> {
        let (sx, sy) = self.pick(extra, cursor, r);
        let mut out = vec![];
        if let (Some(x), Some(y)) = (sx, sy) {
            if (x - y).len() > 1e-9 {
                out.push(Snap {
                    p: v2(x.x, y.y),
                    kind: SnapKind::AlignXY,
                });
            }
        }
        if let Some(x) = sx {
            out.push(Snap {
                p: v2(x.x, cursor.y),
                kind: SnapKind::AlignX,
            });
        }
        if let Some(y) = sy {
            out.push(Snap {
                p: v2(cursor.x, y.y),
                kind: SnapKind::AlignY,
            });
        }
        out
    }

    /// Source points of the guide lines of a chosen alignment snap, for drawing.
    pub fn sources(&self, extra: &[V2], s: &Snap, r: f64) -> Vec<V2> {
        let (sx, sy) = self.pick(extra, s.p, r);
        let mut out = vec![];
        if matches!(s.kind, SnapKind::AlignX | SnapKind::AlignXY) {
            out.extend(sx);
        }
        if matches!(s.kind, SnapKind::AlignY | SnapKind::AlignXY) {
            out.extend(sy);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(ax: f64, ay: f64, bx: f64, by: f64) -> SnapWall {
        SnapWall {
            a: v2(ax, ay),
            b: v2(bx, by),
            half: 0.0,
        }
    }

    #[test]
    fn align_x_from_remote_endpoint() {
        let ix = AlignIndex::build(&[wall(5.0, 0.0, 5.0, 8.0)]);
        let c = ix.candidates(&[], v2(5.05, 3.0), 0.15);
        assert!(c
            .iter()
            .any(|s| s.kind == SnapKind::AlignX && (s.p.x - 5.0).abs() < 1e-9));
        assert!(c.iter().all(|s| s.kind != SnapKind::AlignXY));
    }

    #[test]
    fn align_dual_gives_intersection() {
        let ix = AlignIndex::build(&[wall(0.0, 10.0, 0.0, 20.0), wall(10.0, 0.0, 20.0, 0.0)]);
        let c = ix.candidates(&[], v2(0.05, 0.05), 0.15);
        let xy = c.iter().find(|s| s.kind == SnapKind::AlignXY).expect("dual");
        assert!((xy.p - v2(0.0, 0.0)).len() < 1e-9);
        assert_eq!(c[0].kind, SnapKind::AlignXY, "dual ranks first");
    }

    #[test]
    fn align_none_when_far_or_too_close() {
        let ix = AlignIndex::build(&[wall(5.0, 0.0, 5.0, 8.0)]);
        assert!(ix.candidates(&[], v2(7.0, 3.0), 0.15).is_empty());
        // every point within 2r: the ordinary end snap owns it (the long wall's far end still aligns)
        let short = AlignIndex::build(&[wall(5.0, 0.0, 5.0, 0.2)]);
        assert!(short.candidates(&[], v2(5.05, 0.1), 0.15).is_empty());
    }

    #[test]
    fn align_picks_nearest_source_and_reports_it() {
        let ix = AlignIndex::build(&[wall(5.0, 0.0, 5.0, 0.5), wall(5.0, 9.0, 5.0, 9.5)]);
        let c = ix.candidates(&[], v2(5.02, 2.0), 0.15);
        let s = c.iter().find(|s| s.kind == SnapKind::AlignX).unwrap();
        let src = ix.sources(&[], s, 0.15);
        assert_eq!(src.len(), 1);
        assert!((src[0].x - 5.0).abs() < 1e-9 && src[0].y < 1.0);
    }

    #[test]
    fn align_uses_midpoints_faces_and_draft_points() {
        let thick = SnapWall {
            half: 0.1,
            ..wall(0.0, 0.0, 10.0, 0.0)
        };
        let ix = AlignIndex::build(&[thick]);
        assert!(!ix.candidates(&[], v2(5.0, 2.0), 0.15).is_empty(), "midpoint");
        let face = ix.candidates(&[], v2(10.0, 3.0), 0.15);
        assert!(face.iter().any(|s| s.kind == SnapKind::AlignX));
        let none = AlignIndex::default();
        let c = none.candidates(&[v2(3.0, 9.0)], v2(3.02, 1.0), 0.15);
        assert_eq!(c[0].kind, SnapKind::AlignX);
    }

    #[test]
    fn align_index_refreshes_on_change_and_stays_fast() {
        let mut ws: Vec<SnapWall> = (0..20000)
            .map(|i| wall(i as f64 * 0.37, 0.0, i as f64 * 0.37, 1.0))
            .collect();
        let mut ix = AlignIndex::default();
        ix.refresh(&ws);
        let k = ix.key;
        ix.refresh(&ws);
        assert_eq!(ix.key, k);
        ws[3].b.y = 2.0;
        ix.refresh(&ws);
        assert_ne!(ix.key, k);
        let t = web_time::Instant::now();
        for i in 0..1000 {
            let _ = ix.candidates(&[], v2(i as f64 * 0.37 + 0.02, 40.0), 0.15);
        }
        assert!(t.elapsed().as_millis() < 1000);
    }

    #[test]
    fn non_finite_points_are_ignored() {
        let ix = AlignIndex::build(&[wall(f64::NAN, 0.0, 1.0, 1.0)]);
        let _ = ix.candidates(&[], v2(1.0, 5.0), 0.15);
    }

    #[test]
    fn snap_keys_exist_in_both_locales() {
        let kinds = [
            SnapKind::Endpoint,
            SnapKind::Midpoint,
            SnapKind::Perpendicular,
            SnapKind::WallAxis,
            SnapKind::WallFace,
            SnapKind::Grid,
            SnapKind::Intersection,
            SnapKind::GridAxis,
            SnapKind::RefPlane,
            SnapKind::WorkPlane,
            SnapKind::Nearest,
            SnapKind::AlignX,
            SnapKind::AlignY,
            SnapKind::AlignXY,
        ];
        for k in kinds {
            let key = k.key().expect("key");
            for lang in ["en", "ru"] {
                let dir = format!("{}/../ui/data/i18n/{lang}", env!("CARGO_MANIFEST_DIR"));
                let needle = format!("\"{key}\"");
                let found = std::fs::read_dir(&dir).unwrap().flatten().any(|f| {
                    std::fs::read_to_string(f.path()).is_ok_and(|t| t.contains(&needle))
                });
                assert!(found, "{key} missing in {lang}");
            }
        }
    }
}
