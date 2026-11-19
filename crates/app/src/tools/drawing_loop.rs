//! Closed drawing loop under a point: fallback of the one-click floor, slab and ceiling pick
//! when no wall region exists. Loops are chains of underlay segments; the smallest closed
//! chain around the point is the outline, closed chains inside it are holes.

use crate::wall_pick::{chain, JOIN};
use ddc_geom::predicates::point_in_ring;
use ddc_geom::rooms::Region;
use ddc_geom::{v2, V2};
use std::collections::HashSet;

/// Why no outline was produced; each maps to a UI message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No drawing line near the point at all.
    NoContour,
    /// Lines around the point do not close into a loop.
    Open,
    /// The loop around the point crosses itself.
    SelfCrossing,
}

impl Refusal {
    pub fn key(self) -> &'static str {
        match self {
            Refusal::NoContour => "tool.err.no_region",
            Refusal::Open => "tool.err.drawing_open",
            Refusal::SelfCrossing => "tool.err.drawing_crossing",
        }
    }
}

const DIRS: [(f64, f64); 8] = [
    (1.0, 0.0),
    (-1.0, 0.0),
    (0.0, 1.0),
    (0.0, -1.0),
    (1.0, 1.0),
    (-1.0, 1.0),
    (1.0, -1.0),
    (-1.0, -1.0),
];

fn signed_area(r: &[V2]) -> f64 {
    0.5 * (0..r.len())
        .map(|i| r[i].cross(r[(i + 1) % r.len()]))
        .sum::<f64>()
}

fn ring_len(r: &[V2]) -> f64 {
    (0..r.len()).map(|i| (r[(i + 1) % r.len()] - r[i]).len()).sum()
}

fn key(a: V2, b: V2) -> (i64, i64) {
    let m = (a + b) * 0.5;
    ((m.x * 1000.0).round() as i64, (m.y * 1000.0).round() as i64)
}

/// Indices of segments hit by the ray from `p` along `d`, nearest first.
fn ray_hits(segs: &[(V2, V2)], p: V2, d: V2) -> Vec<usize> {
    let mut hits: Vec<(f64, usize)> = vec![];
    for (i, &(a, b)) in segs.iter().enumerate() {
        let e = b - a;
        let den = d.cross(e);
        if den.abs() < 1e-12 {
            continue;
        }
        let t = (a - p).cross(e) / den;
        let u = (a - p).cross(d) / den;
        if t > 0.0 && (0.0..=1.0).contains(&u) {
            hits.push((t, i));
        }
    }
    hits.sort_by(|x, y| x.0.total_cmp(&y.0));
    hits.into_iter().map(|h| h.1).collect()
}

fn proper_cross(a: V2, b: V2, c: V2, d: V2) -> bool {
    let s = |p: V2, q: V2, r: V2| (q - p).cross(r - p);
    let (d1, d2, d3, d4) = (s(a, b, c), s(a, b, d), s(c, d, a), s(c, d, b));
    d1 * d2 < 0.0 && d3 * d4 < 0.0
}

/// True when two non-neighbouring edges of the ring cross.
pub fn self_crossing(r: &[V2]) -> bool {
    let n = r.len();
    for i in 0..n {
        for j in i + 2..n {
            if i == 0 && j == n - 1 {
                continue;
            }
            if proper_cross(r[i], r[(i + 1) % n], r[j], r[(j + 1) % n]) {
                return true;
            }
        }
    }
    false
}

fn ring_of(ch: &[(V2, V2)]) -> Vec<V2> {
    ch.iter().map(|s| s.0).collect()
}

/// Ring without edges shorter than `MIN_LEN` and without straight or folded-back vertices:
/// wall and drawing loops meet at near-coincident points that no tool could draw.
fn tidy_ring(r: &mut Vec<V2>) {
    let min = crate::wall_tool::MIN_LEN;
    loop {
        let n = r.len();
        if n < 3 {
            return;
        }
        let short = (0..n).find(|&i| (r[(i + 1) % n] - r[i]).len() < min);
        let flat = || {
            (0..n).find(|&i| {
                let (a, b, c) = (r[(i + n - 1) % n], r[i], r[(i + 1) % n]);
                let (u, w) = (b - a, c - b);
                u.cross(w).abs() <= 1e-9 * u.len() * w.len()
            })
        };
        match short.map(|i| (i + 1) % n).or_else(flat) {
            Some(i) => {
                r.remove(i);
            }
            None => return,
        }
    }
}

/// The region a pick makes a slab or ceiling of, its rings tidied ([`tidy_ring`]); holes that
/// collapse go, an outline that collapses or comes to cross itself is refused.
pub fn tidy(mut r: Region) -> Result<Region, Refusal> {
    tidy_ring(&mut r.outer);
    if r.outer.len() < 3 || signed_area(&r.outer).abs() < 1e-9 {
        return Err(Refusal::NoContour);
    }
    if self_crossing(&r.outer) {
        return Err(Refusal::SelfCrossing);
    }
    for h in &mut r.holes {
        tidy_ring(h);
    }
    r.holes.retain(|h| h.len() >= 3 && !self_crossing(h));
    Ok(r)
}

/// Smallest closed loop of `segs` around `p` with the closed loops inside it as holes.
pub fn region(segs: &[(V2, V2)], p: V2) -> Result<Region, Refusal> {
    let mut seen: HashSet<(i64, i64)> = HashSet::new();
    let mut best: Option<Vec<V2>> = None;
    let mut saw_open = false;
    for (dx, dy) in DIRS {
        let d = v2(dx, dy) * (1.0 / (dx * dx + dy * dy).sqrt());
        for i in ray_hits(segs, p, d) {
            if seen.contains(&key(segs[i].0, segs[i].1)) {
                continue;
            }
            let (ch, closed, _) = chain(segs, i);
            seen.extend(ch.iter().map(|s| key(s.0, s.1)));
            if !closed || ch.len() < 3 {
                saw_open = true;
                continue;
            }
            let ring = ring_of(&ch);
            if point_in_ring(p, &ring) > 0
                && best
                    .as_ref()
                    .map_or(true, |b| signed_area(&ring).abs() < signed_area(b).abs())
            {
                best = Some(ring);
            }
        }
    }
    let Some(mut outer) = best else {
        return Err(if saw_open {
            Refusal::Open
        } else {
            Refusal::NoContour
        });
    };
    if self_crossing(&outer) {
        return Err(Refusal::SelfCrossing);
    }
    if signed_area(&outer) < 0.0 {
        outer.reverse();
    }
    let holes = holes_in(segs, &outer, p);
    let area = signed_area(&outer) + holes.iter().map(|h| signed_area(h)).sum::<f64>();
    let perimeter = ring_len(&outer) + holes.iter().map(|h| ring_len(h)).sum::<f64>();
    Ok(Region {
        outer,
        holes,
        area,
        perimeter,
    })
}

/// Outermost simple closed loops strictly inside `outer`, clockwise.
fn holes_in(segs: &[(V2, V2)], outer: &[V2], p: V2) -> Vec<Vec<V2>> {
    let inside: Vec<(V2, V2)> = segs
        .iter()
        .copied()
        .filter(|&(a, b)| (b - a).len() > JOIN && point_in_ring((a + b) * 0.5, outer) > 0)
        .collect();
    let mut seen: HashSet<(i64, i64)> = HashSet::new();
    let mut loops: Vec<Vec<V2>> = vec![];
    for i in 0..inside.len() {
        if seen.contains(&key(inside[i].0, inside[i].1)) {
            continue;
        }
        let (ch, closed, _) = chain(&inside, i);
        seen.extend(ch.iter().map(|s| key(s.0, s.1)));
        if !closed || ch.len() < 3 {
            continue;
        }
        let ring = ring_of(&ch);
        if point_in_ring(p, &ring) != -1
            || self_crossing(&ring)
            || ring.iter().any(|v| point_in_ring(*v, outer) <= 0)
        {
            continue;
        }
        loops.push(ring);
    }
    let nested = |a: &Vec<V2>| {
        loops
            .iter()
            .any(|b| !std::ptr::eq(a, b) && a.iter().all(|v| point_in_ring(*v, b) >= 0))
    };
    let mut out: Vec<Vec<V2>> = loops.iter().filter(|a| !nested(a)).cloned().collect();
    for h in &mut out {
        if signed_area(h) > 0.0 {
            h.reverse();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(V2, V2)> {
        let c = [v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)];
        (0..4).map(|i| (c[i], c[(i + 1) % 4])).collect()
    }

    #[test]
    fn smallest_loop_wins_and_inner_loop_is_a_hole() {
        let mut s = rect(0.0, 0.0, 10.0, 10.0);
        s.extend(rect(-5.0, -5.0, 20.0, 20.0));
        s.extend(rect(2.0, 2.0, 4.0, 4.0));
        let r = region(&s, v2(7.0, 7.0)).unwrap();
        assert!((signed_area(&r.outer) - 100.0).abs() < 1e-9);
        assert_eq!(r.holes.len(), 1);
        assert!(signed_area(&r.holes[0]) < 0.0, "holes run clockwise");
        assert!((r.area - 96.0).abs() < 1e-9);
        assert!(region(&s, v2(3.0, 3.0)).is_ok(), "inside the hole is its own loop");
    }

    #[test]
    fn gap_is_open_and_nothing_is_no_contour() {
        let mut s = rect(0.0, 0.0, 10.0, 10.0);
        s.pop();
        assert_eq!(region(&s, v2(5.0, 5.0)), Err(Refusal::Open));
        assert_eq!(region(&[], v2(5.0, 5.0)), Err(Refusal::NoContour));
        assert_eq!(
            region(&rect(0.0, 0.0, 10.0, 10.0), v2(50.0, 50.0)),
            Err(Refusal::NoContour)
        );
    }

    #[test]
    fn bowtie_is_self_crossing() {
        let s = vec![
            (v2(0.0, 0.0), v2(4.0, 4.0)),
            (v2(4.0, 4.0), v2(4.0, 0.0)),
            (v2(4.0, 0.0), v2(0.0, 4.0)),
            (v2(0.0, 4.0), v2(0.0, 0.0)),
        ];
        assert_eq!(region(&s, v2(3.0, 2.0)), Err(Refusal::SelfCrossing));
    }
}
