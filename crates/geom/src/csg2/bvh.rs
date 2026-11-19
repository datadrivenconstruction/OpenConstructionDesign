// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Bounding volume hierarchy over triangles: candidate pairs by closed box overlap (exact,
//! boxes are min/max of input coordinates) and segment queries for ray parity.

use crate::math::{v3, V3};

const LEAF: usize = 4;

#[derive(Clone, Copy)]
struct Node {
    lo: V3,
    hi: V3,
    /// Leaf: range in `idx`; inner: children `a`, `b`.
    a: u32,
    b: u32,
    leaf: bool,
}

pub(super) struct Bvh {
    nodes: Vec<Node>,
    idx: Vec<u32>,
    pub boxes: Vec<(V3, V3)>,
}

fn vmin(a: V3, b: V3) -> V3 {
    v3(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z))
}
fn vmax(a: V3, b: V3) -> V3 {
    v3(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z))
}
fn comp(p: V3, k: usize) -> f64 {
    [p.x, p.y, p.z][k]
}

pub(super) fn overlap(a: &(V3, V3), b: &(V3, V3)) -> bool {
    a.0.x <= b.1.x
        && b.0.x <= a.1.x
        && a.0.y <= b.1.y
        && b.0.y <= a.1.y
        && a.0.z <= b.1.z
        && b.0.z <= a.1.z
}

impl Bvh {
    pub fn new(boxes: Vec<(V3, V3)>) -> Bvh {
        let mut b = Bvh {
            nodes: Vec::with_capacity(2 * boxes.len() / LEAF + 2),
            idx: (0..boxes.len() as u32).collect(),
            boxes,
        };
        if !b.boxes.is_empty() {
            b.build(0, b.idx.len());
        }
        b
    }

    fn build(&mut self, s: usize, e: usize) -> u32 {
        let me = self.nodes.len() as u32;
        let first = self.boxes[self.idx[s] as usize];
        let (mut lo, mut hi) = first;
        let (mut clo, mut chi) = (first.0 + first.1, first.0 + first.1);
        for &i in &self.idx[s..e] {
            let bx = self.boxes[i as usize];
            lo = vmin(lo, bx.0);
            hi = vmax(hi, bx.1);
            let c = bx.0 + bx.1;
            clo = vmin(clo, c);
            chi = vmax(chi, c);
        }
        self.nodes.push(Node {
            lo,
            hi,
            a: s as u32,
            b: e as u32,
            leaf: true,
        });
        if e - s <= LEAF {
            return me;
        }
        let d = chi - clo;
        let k = if d.x >= d.y && d.x >= d.z {
            0
        } else if d.y >= d.z {
            1
        } else {
            2
        };
        let m = (s + e) / 2;
        let boxes = &self.boxes;
        self.idx[s..e].select_nth_unstable_by(m - s, |p, q| {
            let (bp, bq) = (boxes[*p as usize], boxes[*q as usize]);
            comp(bp.0 + bp.1, k).total_cmp(&comp(bq.0 + bq.1, k))
        });
        let a = self.build(s, m);
        let b = self.build(m, e);
        self.nodes[me as usize] = Node {
            lo,
            hi,
            a,
            b,
            leaf: false,
        };
        me
    }

    /// Box of all items.
    pub fn bounds(&self) -> Option<(V3, V3)> {
        self.nodes.first().map(|n| (n.lo, n.hi))
    }

    /// Every item whose box meets the closed box `q`.
    pub fn query(&self, q: &(V3, V3), out: &mut Vec<u32>) {
        out.clear();
        if self.nodes.is_empty() {
            return;
        }
        let mut st = vec![0u32];
        while let Some(n) = st.pop() {
            let nd = self.nodes[n as usize];
            if !overlap(&(nd.lo, nd.hi), q) {
                continue;
            }
            if nd.leaf {
                for &i in &self.idx[nd.a as usize..nd.b as usize] {
                    if overlap(&self.boxes[i as usize], q) {
                        out.push(i);
                    }
                }
            } else {
                st.push(nd.a);
                st.push(nd.b);
            }
        }
    }

    /// Items whose (slightly inflated) box the segment o-e may meet.
    pub fn query_seg(&self, o: V3, e: V3, out: &mut Vec<u32>) {
        out.clear();
        if self.nodes.is_empty() {
            return;
        }
        let d = e - o;
        let hit = |lo: V3, hi: V3| {
            let pad = 1e-9 * (1.0 + (hi - lo).len());
            let (mut t0, mut t1) = (0.0f64, 1.0f64);
            for k in 0..3 {
                let (oo, dd) = (comp(o, k), comp(d, k));
                let (l, h) = (comp(lo, k) - pad, comp(hi, k) + pad);
                if dd.abs() < 1e-300 {
                    if oo < l || oo > h {
                        return false;
                    }
                    continue;
                }
                let (mut a, mut b) = ((l - oo) / dd, (h - oo) / dd);
                if a > b {
                    std::mem::swap(&mut a, &mut b);
                }
                t0 = t0.max(a);
                t1 = t1.min(b);
                if t0 > t1 * (1.0 + 1e-12) + 1e-12 {
                    return false;
                }
            }
            true
        };
        let mut st = vec![0u32];
        while let Some(n) = st.pop() {
            let nd = self.nodes[n as usize];
            if !hit(nd.lo, nd.hi) {
                continue;
            }
            if nd.leaf {
                for &i in &self.idx[nd.a as usize..nd.b as usize] {
                    let bx = self.boxes[i as usize];
                    if hit(bx.0, bx.1) {
                        out.push(i);
                    }
                }
            } else {
                st.push(nd.a);
                st.push(nd.b);
            }
        }
    }
}
