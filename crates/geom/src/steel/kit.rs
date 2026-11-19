//! Builder state shared by the ten connections: parts, bolts, notes and the catalogue choices
//! every detail makes the same way (bolt size, plate thickness under the ratio rule, fillet
//! legs, welds of a member section onto a plate).

use super::bar::{across, hit_plane, Bar, End};
use super::catalog::{catalog, fmt_mm, BoltSize, Catalog};
use super::part::{self, frame_from, BoltInfo, Part, Role};
use super::{Conn, ConnError, ConnKind, Params, Trim};
use crate::gsolid::Frame;
use crate::math::{v2, v3, V2, V3};

pub(super) struct Kit {
    pub kind: ConnKind,
    pub p: Params,
    pub cat: &'static Catalog,
    pub class: String,
    pub node: V3,
    pub parts: Vec<Part>,
    pub bolts: Vec<(usize, BoltInfo)>,
    pub notes: Vec<String>,
    pub trims: Vec<Trim>,
}

impl Kit {
    pub fn new(kind: ConnKind, p: &Params) -> Result<Kit, ConnError> {
        let cat = catalog();
        let class = p
            .class
            .clone()
            .unwrap_or_else(|| cat.conn.bolt_class.clone());
        cat.class(&class)
            .ok_or_else(|| ConnError(format!("unknown bolt class {class}")))?;
        Ok(Kit {
            kind,
            p: p.clone(),
            cat,
            class,
            node: V3::default(),
            parts: vec![],
            bolts: vec![],
            notes: vec![],
            trims: vec![],
        })
    }

    pub fn finish(self) -> Conn {
        Conn {
            kind: self.kind,
            node: self.node,
            parts: self.parts,
            bolts: self.bolts,
            notes: self.notes,
            trims: self.trims,
        }
    }

    pub fn note(&mut self, s: String) {
        if !self.notes.contains(&s) {
            self.notes.push(s);
        }
    }

    /// The bolt asked for, or `default` (a diameter of the series).
    pub fn bolt_size(&self, default: f64) -> Result<BoltSize, ConnError> {
        let d = self.p.bolt.unwrap_or(default);
        self.cat
            .bolt(d)
            .cloned()
            .ok_or_else(|| ConnError(format!("M{} is not in the bolt series", fmt_mm(d))))
    }

    /// Plate thickness: the asked one, else `want` from the series, never more than the ratio
    /// rule allows against the thinnest joined element `thin`.
    pub fn plate_t(&mut self, want: f64, thin: f64) -> f64 {
        if let Some(t) = self.p.plate {
            return t;
        }
        let t = self.cat.plate_t(want);
        let cap = self.cat.plate.ratio_max * thin;
        if t <= cap + 1e-9 {
            return t;
        }
        let fit = self
            .cat
            .plate
            .thickness
            .iter()
            .rev()
            .copied()
            .find(|x| *x <= cap + 1e-9 && *x >= self.cat.plate.min_t - 1e-9);
        fit.unwrap_or(t)
    }

    /// Fillet leg joining parts `a` and `b` thick.
    pub fn leg(&self, a: f64, b: f64) -> f64 {
        self.p.leg.unwrap_or_else(|| self.cat.weld_leg_fit(a, b))
    }

    pub fn add(&mut self, p: Part) -> usize {
        self.parts.push(p);
        self.parts.len() - 1
    }

    /// A plate part: outline and holes in the frame xy, body from `z0` to `z0 + t`.
    pub fn plate(
        &mut self,
        role: Role,
        f: Frame,
        outline: Vec<V2>,
        holes: Vec<(V2, f64)>,
        t: f64,
        z0: f64,
    ) -> usize {
        self.add(part::plate(role, f, outline, holes, t, z0))
    }

    /// A bolt set with the head bearing on `o`, through `grip` along `dir`.
    pub fn bolt(
        &mut self,
        size: &BoltSize,
        o: V3,
        dir: V3,
        grip: f64,
        plies: Vec<usize>,
        xhint: V3,
    ) -> Result<(), ConnError> {
        let (parts, mut info) = part::bolt_set(size, &self.class, o, dir, grip, size.hole, xhint)?;
        info.plies = plies;
        let base = self.parts.len();
        let shank = parts.iter().position(|p| p.role == Role::Bolt).unwrap_or(0) + base;
        self.parts.extend(parts);
        self.bolts.push((shank, info));
        Ok(())
    }

    /// Anchor rod at `top` (plate top), `grip` the plate, `below` grout plus embedment.
    pub fn anchor(
        &mut self,
        size: &BoltSize,
        top: V3,
        grip: f64,
        below: f64,
        ply: usize,
        xhint: V3,
    ) {
        let class = self.cat.conn.anchor_class.clone();
        let (parts, mut info) = part::anchor_set(size, &class, top, grip, below, xhint);
        info.plies = vec![ply];
        let base = self.parts.len();
        self.parts.extend(parts);
        self.bolts.push((base, info));
    }

    /// Fillet weld; a run shorter than the catalogue minimum is noted, not dropped.
    pub fn weld(&mut self, c: V3, dir: V3, len: f64, a: V3, b: V3, leg: f64) {
        if len < leg {
            return;
        }
        let min = self.cat.weld_len_min(leg);
        if len < min - 1e-9 {
            self.note(format!(
                "weld run {} mm is shorter than the minimum {} mm for leg {} mm",
                fmt_mm(len),
                fmt_mm(min),
                fmt_mm(leg)
            ));
        }
        self.add(part::fillet(c, dir, len, a, b, leg));
    }

    /// Two fillets along a plate edge standing on a face: root from `c` along `dir` for `len`;
    /// `n` is the plate normal, `away` points from the face along the plate, `t` the plate
    /// thickness (the plate occupies `[-t/2, t/2]` along `n` about `c`).
    pub fn weld_both_sides(&mut self, c: V3, dir: V3, len: f64, n: V3, away: V3, t: f64, leg: f64) {
        let n = n.normalized();
        for s in [1.0, -1.0] {
            let root = c + n * (s * t / 2.0);
            self.weld(root, dir, len, n * s, away, leg);
        }
    }

    /// Welds all round a bar's section where it meets the plane (`o`, `n`); `n` points from the
    /// plane into the bar side. Round sections square to the plane get one ring.
    pub fn weld_section(&mut self, bar: &Bar, o: V3, n: V3, leg: f64) {
        let n = n.normalized();
        let into = if bar.u.dot(n) >= 0.0 { bar.u } else { -bar.u };
        if bar.dims.round() && into.dot(n) > 1.0 - 1e-6 {
            let Some(s) = hit_plane(bar.c0, bar.u, o, n) else {
                return;
            };
            let c = bar.c0 + bar.u * s;
            let r = bar.dims.h / 2.0;
            let f = frame_from(c, n, bar.ey);
            let tri = vec![v2(r, 0.0), v2(r + leg, 0.0), v2(r, leg)];
            let len = 2.0 * std::f64::consts::PI * (r + leg / 3.0);
            let mut w = part::revolve(
                Role::Weld,
                f,
                tri,
                format!("weld;leg={}", fmt_mm(leg)),
                format!("a{} L={}", fmt_mm(leg), fmt_mm(len)),
            );
            w.qty = super::PartQty::Weld { leg, length: len };
            self.add(w);
            return;
        }
        let ring = bar.outer.clone();
        let m = ring.len();
        for i in 0..m {
            let (p, q) = (ring[i], ring[(i + 1) % m]);
            let e = q - p;
            let l = e.len();
            if l < 1e-9 {
                continue;
            }
            let hit = |s: V2| {
                let w = bar.at(0.0, s.x, s.y);
                hit_plane(w, bar.u, o, n).map(|k| w + bar.u * k)
            };
            let (Some(a), Some(b)) = (hit(p), hit(q)) else {
                continue;
            };
            let run = b - a;
            let len = run.len();
            if len < leg {
                continue;
            }
            let dir = run * (1.0 / len);
            let out = bar.sdir(v2(e.y / l, -e.x / l));
            let along_plate = across(out, n);
            let along_bar = across(into, dir);
            self.weld(a, dir, len, along_plate, along_bar, leg);
        }
    }

    /// Records a member end that has to stop short of the node.
    pub fn trim(&mut self, member: usize, end: End, setback: f64) {
        self.trims.push(super::Trim {
            member,
            end,
            setback,
        });
    }
}

/// Rectangle outline centred on (cx, cy).
pub(super) fn rect(cx: f64, cy: f64, w: f64, h: f64) -> Vec<V2> {
    let (a, b) = (w / 2.0, h / 2.0);
    vec![
        v2(cx - a, cy - b),
        v2(cx + a, cy - b),
        v2(cx + a, cy + b),
        v2(cx - a, cy + b),
    ]
}

/// Rectangle outline from corner (x0, y0) to (x1, y1).
pub(super) fn rect2(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<V2> {
    vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)]
}

/// Up in the world.
pub(super) const UP: V3 = v3(0.0, 0.0, 1.0);

/// Bounds (x0, x1, y0, y1) in the frame `f` of a bar's section carried along its axis onto
/// the plane z = `z` of `f`.
pub(super) fn footprint(bar: &Bar, f: &Frame, z: f64) -> (f64, f64, f64, f64) {
    let (o, n) = (f.at(v3(0.0, 0.0, z)), f.z);
    let mut b = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for p in &bar.outer {
        let w = bar.at(0.0, p.x, p.y);
        let s = hit_plane(w, bar.u, o, n).unwrap_or(0.0);
        let q = f.local(w + bar.u * s);
        b = (b.0.min(q.x), b.1.max(q.x), b.2.min(q.y), b.3.max(q.y));
    }
    b
}

/// Holes of diameter `d` at world points, in the frame `f`.
pub(super) fn holes_at(f: &Frame, pts: &[V3], d: f64) -> Vec<(V2, f64)> {
    pts.iter()
        .map(|p| {
            let q = f.local(*p);
            (v2(q.x, q.y), d)
        })
        .collect()
}

/// Convex hull, counter-clockwise, without collinear points.
pub(super) fn hull(mut pts: Vec<V2>) -> Vec<V2> {
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup_by(|a, b| (*a - *b).len() < 1e-9);
    if pts.len() < 3 {
        return pts;
    }
    let turn = |o: V2, a: V2, b: V2| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
    let mut h: Vec<V2> = vec![];
    for pass in 0..2 {
        let start = h.len();
        let it: Box<dyn Iterator<Item = &V2>> = if pass == 0 {
            Box::new(pts.iter())
        } else {
            Box::new(pts.iter().rev())
        };
        for &p in it {
            while h.len() >= start + 2 && turn(h[h.len() - 2], h[h.len() - 1], p) <= 1e-12 {
                h.pop();
            }
            h.push(p);
        }
        h.pop();
    }
    h
}

/// `n` positions at `pitch` centred on 0.
pub(super) fn centred(n: usize, pitch: f64) -> Vec<f64> {
    (0..n)
        .map(|i| (i as f64 - (n as f64 - 1.0) / 2.0) * pitch)
        .collect()
}
