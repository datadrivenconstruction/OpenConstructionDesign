//! Member framing: trusses (Pratt/Howe/Warren web, parallel or pitched chords)
//! and beam-system layout within a boundary. A truss yields `member::Member` members with ends:
//! chords are mitered at nodes, web members end on the chord faces; nodes lie on axes (chord centroids).
//! The web is not trimmed against verticals: at a node the diagonal and vertical solids overlap slightly
//! (web volume is overstated by a fraction of a percent, a few percent for short panels).

use crate::math::{v2, v3, V2, V3};
use crate::member::{miter, Cap, Just, Member, Path, Shape};
use crate::poly;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Web {
    /// Diagonals descend toward midspan (\\//), verticals at all nodes.
    #[default]
    Pratt,
    /// Diagonals ascend toward midspan (//\\), verticals at all nodes.
    Howe,
    /// Zigzag diagonals, no intermediate verticals.
    Warren,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Chord {
    /// Parallel chords: height is `height` everywhere.
    #[default]
    Flat,
    /// Pitched top chord: `heel` at the supports, `height` at midspan.
    Pitched,
}

impl Web {
    pub fn text(self) -> &'static str {
        match self {
            Web::Pratt => "pratt",
            Web::Howe => "howe",
            Web::Warren => "warren",
        }
    }
    pub fn parse(s: &str) -> Web {
        match s {
            "howe" => Web::Howe,
            "warren" => Web::Warren,
            _ => Web::Pratt,
        }
    }
}

impl Chord {
    pub fn text(self) -> &'static str {
        match self {
            Chord::Flat => "flat",
            Chord::Pitched => "pitched",
        }
    }
    pub fn parse(s: &str) -> Chord {
        match s {
            "pitched" => Chord::Pitched,
            _ => Chord::Flat,
        }
    }
}

/// Role of a truss member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Top,
    Bottom,
    Vertical,
    Diagonal,
}

impl Role {
    pub fn text(self) -> &'static str {
        match self {
            Role::Top => "top",
            Role::Bottom => "bottom",
            Role::Vertical => "vertical",
            Role::Diagonal => "diagonal",
        }
    }
    pub fn web(self) -> bool {
        matches!(self, Role::Vertical | Role::Diagonal)
    }
}

/// Truss in a vertical plane above the bottom chord axis a→b.
#[derive(Clone, Debug, PartialEq)]
pub struct Truss {
    pub a: V3,
    pub b: V3,
    pub height: f64,
    pub heel: f64,
    pub panels: usize,
    pub web: Web,
    pub chord: Chord,
}

/// Node: (is top chord?, panel point index).
pub type Node = (bool, usize);

/// Member axis between nodes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bar {
    pub role: Role,
    pub from: Node,
    pub to: Node,
}

const EPS: f64 = 1e-9;

impl Truss {
    pub fn check(&self) -> Result<(), String> {
        let v = [
            self.a.x,
            self.a.y,
            self.a.z,
            self.b.x,
            self.b.y,
            self.b.z,
            self.height,
            self.heel,
        ];
        if !v.iter().all(|x| x.is_finite()) {
            return Err("truss: non-finite".into());
        }
        let d = self.b - self.a;
        if v2(d.x, d.y).len() < 1e-6 {
            return Err("truss: span must be non-zero in plan".into());
        }
        if d.z.abs() > 1e-6 {
            return Err("truss: bottom chord ends must be at one height".into());
        }
        if self.height <= 0.0 || self.heel < 0.0 || self.heel > self.height {
            return Err("truss: 0 <= heel <= height, height > 0".into());
        }
        if self.panels < 2 || self.panels > 200 {
            return Err("truss: 2..200 panels".into());
        }
        if self.chord == Chord::Pitched && self.panels % 2 != 0 {
            return Err("truss: pitched truss needs an even panel count".into());
        }
        Ok(())
    }

    /// Height of the top chord above the bottom chord at panel point k.
    pub fn rise(&self, k: usize) -> f64 {
        match self.chord {
            Chord::Flat => self.height,
            Chord::Pitched => {
                let s = 2.0 * k as f64 / self.panels as f64 - 1.0;
                self.heel + (self.height - self.heel) * (1.0 - s.abs())
            }
        }
    }

    pub fn node(&self, (top, k): Node) -> V3 {
        let p = self.a + (self.b - self.a) * (k as f64 / self.panels as f64);
        if top {
            p + v3(0.0, 0.0, self.rise(k))
        } else {
            p
        }
    }

    /// Axes of all members: chords by panel, then verticals, then diagonals.
    pub fn bars(&self) -> Vec<Bar> {
        let n = self.panels;
        let low = |k: usize| self.rise(k) < EPS;
        let mut out = vec![];
        for k in 0..n {
            out.push(Bar {
                role: Role::Bottom,
                from: (false, k),
                to: (false, k + 1),
            });
        }
        for k in 0..n {
            // pitched with zero heel: the end panels of the top chord start at the support node
            let from = if low(k) { (false, k) } else { (true, k) };
            let to = if low(k + 1) {
                (false, k + 1)
            } else {
                (true, k + 1)
            };
            out.push(Bar {
                role: Role::Top,
                from,
                to,
            });
        }
        for k in 0..=n {
            let end = k == 0 || k == n;
            if low(k) || (!end && self.web == Web::Warren) {
                continue;
            }
            out.push(Bar {
                role: Role::Vertical,
                from: (false, k),
                to: (true, k),
            });
        }
        for k in 0..n {
            if low(k) || low(k + 1) {
                continue;
            }
            let left = 2 * k < n;
            // true: from top node k to bottom node k+1
            let down = match self.web {
                Web::Pratt => left,
                Web::Howe => !left,
                Web::Warren => k % 2 == 1,
            };
            let (from, to) = if down {
                ((true, k), (false, k + 1))
            } else {
                ((false, k), (true, k + 1))
            };
            out.push(Bar {
                role: Role::Diagonal,
                from,
                to,
            });
        }
        out
    }

    /// Truss plane normal (to the left of a→b, horizontal).
    fn plane_normal(&self) -> V3 {
        let d = self.b - self.a;
        let h = v2(d.x, d.y);
        let h = h * (1.0 / h.len());
        v3(-h.y, h.x, 0.0)
    }

    /// Unit chord normal in the truss plane, pointing into the truss.
    fn chord_inward(&self, p: V3, q: V3, top: bool, np: V3) -> V3 {
        let u = (q - p).normalized();
        let n = np.cross(u).normalized();
        let up = if n.z >= 0.0 { n } else { -n };
        if top {
            -up
        } else {
            up
        }
    }

    /// Members with sections and ends: chords use `chord`, web members use `web`.
    pub fn members(&self, chord: &Shape, web: &Shape) -> Vec<(Role, Member)> {
        let bars = self.bars();
        let np = self.plane_normal();
        let hc = chord.size().1;
        // verticals: the frame of a vertical axis gives ey = +x, ez = +y; the rotation aligns ey with the normal
        let vrot = np.y.atan2(np.x);
        let chord_of = |role: Role| -> Vec<(V3, V3)> {
            bars.iter()
                .filter(|b| b.role == role)
                .map(|b| (self.node(b.from), self.node(b.to)))
                .collect()
        };
        let (tops, bots) = (chord_of(Role::Top), chord_of(Role::Bottom));
        let mk = |shape: &Shape, a: V3, b: V3, rot: f64, s: Option<Cap>, e: Option<Cap>| Member {
            shape: shape.clone(),
            path: Path::Line { a, b },
            rotation: rot,
            yjust: Just::Center,
            zjust: Just::Center,
            column: false,
            start: s,
            end: e,
        };
        let mut out = vec![];
        for (role, segs) in [(Role::Bottom, &bots), (Role::Top, &tops)] {
            let n = segs.len();
            for (i, &(p, q)) in segs.iter().enumerate() {
                let u = (q - p).normalized();
                let mut s = (i > 0).then(|| {
                    let (pp, _) = segs[i - 1];
                    let c = miter(p, (p - pp).normalized(), u);
                    Cap { p: c.p, n: -c.n }
                });
                let e = (i + 1 < n).then(|| {
                    let (_, qq) = segs[i + 1];
                    miter(q, u, (qq - q).normalized())
                });
                // top chord from the support node runs to the top face of the bottom chord
                if role == Role::Top && i == 0 && self.rise(0) < EPS {
                    let (b0, b1) = bots[0];
                    let inw = self.chord_inward(b0, b1, false, np);
                    s = Some(Cap {
                        p: p + inw * (hc / 2.0),
                        n: -inw,
                    });
                }
                let mut e = e;
                if role == Role::Top && i + 1 == n && self.rise(self.panels) < EPS {
                    let (b0, b1) = bots[bots.len() - 1];
                    let inw = self.chord_inward(b0, b1, false, np);
                    e = Some(Cap {
                        p: q + inw * (hc / 2.0),
                        n: -inw,
                    });
                }
                out.push((role, mk(chord, p, q, 0.0, s, e)));
            }
        }
        // chord face at the node: inward normal (averaged over adjoining panels)
        let face = |(top, k): Node| -> Cap {
            let segs = if top { &tops } else { &bots };
            let p = self.node((top, k));
            let mut acc = v3(0.0, 0.0, 0.0);
            for &(a, b) in segs.iter() {
                if (a - p).len() < EPS || (b - p).len() < EPS {
                    acc = acc + self.chord_inward(a, b, top, np);
                }
            }
            let n = if acc.len() < EPS {
                if top {
                    v3(0.0, 0.0, -1.0)
                } else {
                    v3(0.0, 0.0, 1.0)
                }
            } else {
                acc.normalized()
            };
            Cap {
                p: p + n * (hc / 2.0),
                n: -n,
            }
        };
        for b in bars.iter().filter(|b| b.role.web()) {
            let (p, q) = (self.node(b.from), self.node(b.to));
            let (p, q, f0, f1) = if b.role == Role::Vertical || p.z <= q.z {
                (p, q, b.from, b.to)
            } else {
                (q, p, b.to, b.from)
            };
            let rot = if b.role == Role::Vertical { vrot } else { 0.0 };
            out.push((b.role, mk(web, p, q, rot, Some(face(f0)), Some(face(f1)))));
        }
        out
    }
}

/// Beam-system layout rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Layout {
    /// Fixed spacing, the first beam is one spacing from the edge.
    #[default]
    FixedStart,
    /// Fixed spacing, layout symmetric about the midpoint.
    FixedCenter,
    /// Equal gaps no larger than the spacing.
    MaxEqual,
    /// A given number of beams with equal gaps (spacing is unused).
    FixedCount(usize),
}

impl Layout {
    pub fn text(self) -> &'static str {
        match self {
            Layout::FixedStart => "fixed_start",
            Layout::FixedCenter => "fixed_center",
            Layout::MaxEqual => "max_equal",
            Layout::FixedCount(_) => "fixed_count",
        }
    }
    /// Beam count of the `FixedCount` rule (0 otherwise), stored separately from the rule name.
    pub fn count(self) -> usize {
        match self {
            Layout::FixedCount(n) => n,
            _ => 0,
        }
    }
    pub fn parse(s: &str) -> Layout {
        Layout::parse_with(s, 0)
    }
    /// Rule by name and beam count (for `fixed_count`).
    pub fn parse_with(s: &str, count: usize) -> Layout {
        match s {
            "fixed_center" => Layout::FixedCenter,
            "max_equal" => Layout::MaxEqual,
            "fixed_count" => Layout::FixedCount(count),
            _ => Layout::FixedStart,
        }
    }
}

/// Offsets of beam lines across the direction within [s0, s1].
pub fn offsets(s0: f64, s1: f64, spacing: f64, layout: Layout) -> Vec<f64> {
    let w = s1 - s0;
    if w.is_nan() || w <= 0.0 {
        return vec![];
    }
    if let Layout::FixedCount(n) = layout {
        if n == 0 || n > 100_000 {
            return vec![];
        }
        let g = w / (n + 1) as f64;
        return (1..=n).map(|j| s0 + g * j as f64).collect();
    }
    if spacing.is_nan() || spacing <= 0.0 || w / spacing > 1e5 {
        return vec![];
    }
    // relative tolerance: a beam closer than 1e-6·spacing to the edge would lie on the boundary beam
    let eps = spacing * 1e-6;
    match layout {
        Layout::FixedStart | Layout::FixedCount(_) => {
            let k = ((w - eps) / spacing).floor().max(0.0) as usize;
            (1..=k).map(|j| s0 + spacing * j as f64).collect()
        }
        Layout::FixedCenter => {
            let k = ((w - eps) / spacing).floor().max(0.0) as usize;
            let mid = (s0 + s1) / 2.0;
            (0..k)
                .map(|j| mid + (j as f64 - (k as f64 - 1.0) / 2.0) * spacing)
                .collect()
        }
        Layout::MaxEqual => {
            let k = ((w / spacing) - 1e-6).ceil().max(1.0) as usize - 1;
            let g = w / (k + 1) as f64;
            (1..=k).map(|j| s0 + g * j as f64).collect()
        }
    }
}

/// Beam-system members: segments inside the boundary, parallel to direction `angle` (rad).
/// Each segment is a piece of a line inside the boundary (a concave boundary yields several pieces).
pub fn beam_lines(ring: &[V2], angle: f64, spacing: f64, layout: Layout) -> Vec<(V2, V2)> {
    beam_lines_in(ring, &[], angle, spacing, layout, None)
}

/// Like [`beam_lines`], but with openings `holes` (beams are interrupted) and a reference edge:
/// `start` is the boundary edge `ring[i] → ring[i+1]` from whose side the layout is measured
/// (None means the side to the right of the beam direction).
pub fn beam_lines_in(
    ring: &[V2],
    holes: &[Vec<V2>],
    angle: f64,
    spacing: f64,
    layout: Layout,
    start: Option<usize>,
) -> Vec<(V2, V2)> {
    if ring.len() < 3 || !angle.is_finite() || poly::area(ring).abs() < 1e-12 {
        return vec![];
    }
    let d = v2(angle.cos(), angle.sin());
    let n = v2(-d.y, d.x);
    let (mut s0, mut s1, mut t0, mut t1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for p in ring {
        let (s, t) = (p.dot(n), p.dot(d));
        (s0, s1, t0, t1) = (s0.min(s), s1.max(s), t0.min(t), t1.max(t));
    }
    // reference edge on the s1 side: the layout is mirrored from s1
    let flip = start.filter(|&i| i < ring.len()).is_some_and(|i| {
        let m = (ring[i] + ring[(i + 1) % ring.len()]) * 0.5;
        let sm = m.dot(n);
        s1 - sm < sm - s0
    });
    let ss: Vec<f64> = if flip {
        offsets(-s1, -s0, spacing, layout)
            .into_iter()
            .map(|x| -x)
            .collect()
    } else {
        offsets(s0, s1, spacing, layout)
    };
    let mut rings = vec![ring.to_vec()];
    rings.extend(holes.iter().filter(|h| h.len() >= 3).cloned());
    let mut out = vec![];
    for s in ss {
        let a = n * s + d * (t0 - 1.0);
        let b = n * s + d * (t1 + 1.0);
        for (u, v) in poly::inside_intervals(a, b, &rings) {
            let (p, q) = (a + (b - a) * u, a + (b - a) * v);
            if (q - p).len() > 1e-6 {
                out.push((p, q));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn truss(web: Web, chord: Chord, heel: f64, n: usize) -> Truss {
        Truss {
            a: v3(0.0, 0.0, 3.0),
            b: v3(12.0, 0.0, 3.0),
            height: 2.0,
            heel,
            panels: n,
            web,
            chord,
        }
    }

    fn count(t: &Truss, r: Role) -> usize {
        t.bars().iter().filter(|b| b.role == r).count()
    }

    #[test]
    fn flat_pratt_topology() {
        let t = truss(Web::Pratt, Chord::Flat, 0.0, 6);
        t.check().unwrap();
        assert_eq!(count(&t, Role::Bottom), 6);
        assert_eq!(count(&t, Role::Top), 6);
        assert_eq!(count(&t, Role::Vertical), 7);
        assert_eq!(count(&t, Role::Diagonal), 6);
        // left half: diagonal from the top node to the next bottom node
        let d0 = t
            .bars()
            .into_iter()
            .find(|b| b.role == Role::Diagonal)
            .unwrap();
        assert_eq!((d0.from, d0.to), ((true, 0), (false, 1)));
        let w = truss(Web::Warren, Chord::Flat, 0.0, 6);
        assert_eq!(count(&w, Role::Vertical), 2);
        let h = truss(Web::Howe, Chord::Flat, 0.0, 6);
        let d0 = h
            .bars()
            .into_iter()
            .find(|b| b.role == Role::Diagonal)
            .unwrap();
        assert_eq!((d0.from, d0.to), ((false, 0), (true, 1)));
    }

    #[test]
    fn pitched_truss_rise_and_degenerate_ends() {
        let t = truss(Web::Howe, Chord::Pitched, 0.0, 8);
        t.check().unwrap();
        assert!((t.rise(4) - 2.0).abs() < 1e-12);
        assert!((t.rise(2) - 1.0).abs() < 1e-12);
        // no verticals at the supports, no diagonals in the end panels
        assert_eq!(count(&t, Role::Vertical), 7);
        assert_eq!(count(&t, Role::Diagonal), 6);
        let top: Vec<Bar> = t
            .bars()
            .into_iter()
            .filter(|b| b.role == Role::Top)
            .collect();
        assert_eq!(top[0].from, (false, 0));
        assert!(truss(Web::Pratt, Chord::Pitched, 0.0, 5).check().is_err());
        assert!(truss(Web::Pratt, Chord::Flat, 3.0, 4).check().is_err());
    }

    #[test]
    fn members_lengths_and_web_cutback() {
        let t = truss(Web::Pratt, Chord::Flat, 0.0, 4);
        let ch = Shape::Rect { b: 0.1, h: 0.2 };
        let wb = Shape::Rect { b: 0.08, h: 0.08 };
        let ms = t.members(&ch, &wb);
        let len = |r: Role| -> f64 {
            ms.iter()
                .filter(|m| m.0 == r)
                .map(|m| m.1.quantities().length)
                .sum()
        };
        assert!((len(Role::Bottom) - 12.0).abs() < 1e-9);
        assert!((len(Role::Top) - 12.0).abs() < 1e-9);
        // 5 verticals of 2.0, shortened by half a chord height at each end
        assert!(
            (len(Role::Vertical) - 5.0 * (2.0 - 0.2)).abs() < 1e-9,
            "{}",
            len(Role::Vertical)
        );
        let v: f64 = ms.iter().map(|m| m.1.quantities().volume).sum();
        assert!(v > 0.0);
        for (_, m) in &ms {
            let mv = m.mesh().volume();
            assert!((mv - m.quantities().volume).abs() < 1e-9, "{mv}");
        }
    }

    #[test]
    fn layouts_and_lines() {
        assert_eq!(
            offsets(0.0, 6.0, 1.5, Layout::FixedStart),
            vec![1.5, 3.0, 4.5]
        );
        let c = offsets(0.0, 7.0, 1.5, Layout::FixedCenter);
        assert_eq!(c.len(), 4);
        assert!((c[0] - 1.25).abs() < 1e-12 && (c[3] - 5.75).abs() < 1e-12);
        let e = offsets(0.0, 7.0, 1.5, Layout::MaxEqual);
        assert_eq!(e.len(), 4);
        assert!((e[0] - 1.4).abs() < 1e-12);
        let ring = vec![v2(0.0, 0.0), v2(6.0, 0.0), v2(6.0, 4.0), v2(0.0, 4.0)];
        let l = beam_lines(&ring, std::f64::consts::FRAC_PI_2, 1.5, Layout::FixedStart);
        assert_eq!(l.len(), 3);
        for (p, q) in &l {
            assert!(((*q - *p).len() - 4.0).abs() < 1e-9);
        }
        // concave boundary: a line through the notch gives two pieces
        let u = vec![
            v2(0.0, 0.0),
            v2(6.0, 0.0),
            v2(6.0, 4.0),
            v2(4.0, 4.0),
            v2(4.0, 2.0),
            v2(2.0, 2.0),
            v2(2.0, 4.0),
            v2(0.0, 4.0),
        ];
        let l = beam_lines(&u, 0.0, 3.0, Layout::FixedStart);
        assert_eq!(l.len(), 2);
    }

    #[test]
    fn layout_tolerance_count_holes_and_start_edge() {
        // width slightly over 4 spacings: a beam does not lie on the edge
        let o = offsets(0.0, 6.000_000_1, 1.5, Layout::FixedStart);
        assert_eq!(o.len(), 3, "{o:?}");
        assert_eq!(offsets(0.0, 6.000_000_1, 1.5, Layout::MaxEqual).len(), 3);
        let c = offsets(0.0, 6.0, 99.0, Layout::FixedCount(5));
        assert_eq!(c.len(), 5);
        assert!((c[0] - 1.0).abs() < 1e-12 && (c[4] - 5.0).abs() < 1e-12);
        assert_eq!(Layout::parse_with("fixed_count", 5), Layout::FixedCount(5));
        assert_eq!(Layout::FixedCount(5).text(), "fixed_count");
        let ring = vec![v2(0.0, 0.0), v2(7.0, 0.0), v2(7.0, 4.0), v2(0.0, 4.0)];
        let up = std::f64::consts::FRAC_PI_2;
        // by default from the right side (x = 7): 5.5, 4, 2.5, 1
        let xs = |l: &[(V2, V2)]| {
            let mut v: Vec<f64> = l.iter().map(|(p, _)| p.x).collect();
            v.sort_by(f64::total_cmp);
            v
        };
        let def = beam_lines_in(&ring, &[], up, 1.5, Layout::FixedStart, None);
        assert_eq!(def.len(), 4);
        assert!((xs(&def)[0] - 1.0).abs() < 1e-9);
        // edge x = 0 (edge 3: (0,4)→(0,0)): measured from it: 1.5, 3, 4.5, 6
        let left = beam_lines_in(&ring, &[], up, 1.5, Layout::FixedStart, Some(3));
        assert!((xs(&left)[0] - 1.5).abs() < 1e-9, "{:?}", xs(&left));
        // a 2×1 opening in the middle interrupts the beams that pass through it
        let hole = vec![v2(2.0, 1.5), v2(5.0, 1.5), v2(5.0, 2.5), v2(2.0, 2.5)];
        let h = beam_lines_in(&ring, &[hole], up, 1.5, Layout::FixedStart, Some(3));
        assert_eq!(h.len(), 4 + 2, "x = 3 and 4.5 are cut");
        let total: f64 = h.iter().map(|(p, q)| (*q - *p).len()).sum();
        assert!((total - (4.0 * 4.0 - 2.0)).abs() < 1e-9, "{total}");
    }

    #[test]
    fn sloped_truss_rejected() {
        let mut t = truss(Web::Pratt, Chord::Flat, 0.0, 4);
        t.b.z += 0.5;
        assert!(t.check().unwrap_err().contains("one height"));
    }
}
