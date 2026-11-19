// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Curtain wall: a grid along the axis (segment or arc) x height, flat panels between grid nodes,
//! mullions along grid lines. Quantities are exact: panel area = chord x cell height (between
//! mullion axes). Mullions: vertical ones (including edge ones) run solid over the full height; horizontal
//! ones (including edge ones) are segments between the faces of the verticals (butt joint). Lengths and
//! volumes do not double-count intersections; mullion bodies do not overlap.

use crate::arc::ArcAxis;
use crate::math::{v2, v3, Tol, V2, V3};
use crate::mesh::Mesh;

/// Mullion section: rectangle (`w` in the wall plane across the mullion, `d` along the normal) or circle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Section {
    None,
    Rect { w: f64, d: f64 },
    Circle { d: f64 },
}

/// Curtain wall axis in plan.
#[derive(Clone, Copy, Debug)]
pub struct Axis {
    pub a: V2,
    pub b: V2,
    pub arc: Option<ArcAxis>,
}

impl Axis {
    pub fn new(a: V2, b: V2, bulge: f64) -> Result<Self, String> {
        if (b - a).len() <= Tol::default().length {
            return Err("zero length axis".into());
        }
        let arc = if bulge == 0.0 {
            None
        } else {
            Some(ArcAxis::new(a, b, bulge, Tol::default()).map_err(|e| format!("{e:?}"))?)
        };
        Ok(Self { a, b, arc })
    }
    pub fn length(&self) -> f64 {
        self.arc.map_or((self.b - self.a).len(), |c| c.length())
    }
    pub fn point_at(&self, s: f64) -> V2 {
        match self.arc {
            Some(c) => c.point_at(s),
            None => self.a + (self.b - self.a) * (s / (self.b - self.a).len()),
        }
    }
    /// Distance along the axis to the projection of a point.
    pub fn param_of(&self, p: V2) -> f64 {
        match self.arc {
            Some(c) => c.param_of(p),
            None => {
                let d = self.b - self.a;
                (p - self.a).dot(d) / d.len()
            }
        }
    }
    /// Unit normal to the left of the axis at `s`.
    pub fn left_at(&self, s: f64) -> V2 {
        let t = match self.arc {
            Some(c) => c.tangent_at(s),
            None => {
                let d = self.b - self.a;
                d * (1.0 / d.len())
            }
        };
        v2(-t.y, t.x)
    }
}

/// Grid nodes from 0 to `len`: spacing from the start (the last cell is shorter), no nodes closer than 1 mm to the end.
pub fn spacing_lines(len: f64, step: f64) -> Vec<f64> {
    let mut v = vec![0.0];
    if step.is_finite() && step > 1e-3 {
        let mut k = 1.0;
        while k * step < len - 1e-3 {
            v.push(k * step);
            k += 1.0;
        }
    }
    v.push(len);
    v
}

/// Grid node tolerance: a node closer than 1 mm to an end or neighbor is dropped (as in `spacing_lines`).
pub const LINE_TOL: f64 = 1e-3;

/// Grid nodes from 0 to `len` from interior positions (sorted; values outside (0, len) and duplicates dropped).
pub fn explicit_lines(len: f64, inner: &[f64]) -> Vec<f64> {
    let mut v: Vec<f64> = inner
        .iter()
        .copied()
        .filter(|x| x.is_finite() && *x >= LINE_TOL && *x <= len - LINE_TOL)
        .collect();
    v.sort_by(f64::total_cmp);
    v.dedup_by(|a, b| (*a - *b).abs() < LINE_TOL);
    v.insert(0, 0.0);
    v.push(len);
    v
}

/// Cell `i` (along the axis) x `j` (along the height); corners: bottom-start, bottom-end, top-end, top-start.
/// `net` is the panel net of mullions: half the width of an interior one, the full width of an edge one
/// (an edge mullion lies inside the wall outline).
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub i: usize,
    pub j: usize,
    pub corners: [V3; 4],
    pub width: f64,
    pub height: f64,
    pub area: f64,
    pub net: [V3; 4],
    pub net_area: f64,
    /// Panel outline (x along the chord from `corners[0]`, height above the wall bottom) counter-clockwise;
    /// with a polyline top, the `net` rectangle clipped by the edge of the top edge mullion.
    pub outline: Vec<(f64, f64)>,
}

/// Mullion: vertical is line `line` of `u`, solid (`seg` = 0); horizontal is line
/// `line` of `v`, cell segment `seg`, trimmed by the faces of the verticals.
#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    pub a: V3,
    pub b: V3,
    pub border: bool,
    pub vertical: bool,
    /// Normal to the wall plane (horizontal) at the mullion.
    pub normal: V3,
    /// Edge mullion: unit vector into the wall (body shifted by half the width); interior mullion: 0.
    pub inward: V3,
    pub line: usize,
    pub seg: usize,
    /// Section of this mullion (by grid line).
    pub section: Section,
}

impl Bar {
    pub fn length(&self) -> f64 {
        (self.b - self.a).len()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub u: Vec<f64>,
    pub v: Vec<f64>,
    pub cells: Vec<Cell>,
    pub bars: Vec<Bar>,
}

impl Section {
    /// Width in the wall plane.
    pub fn width(&self) -> f64 {
        match *self {
            Section::None => 0.0,
            Section::Rect { w, .. } => w,
            Section::Circle { d } => d,
        }
    }
}

/// Sections per node: edge nodes use `border`, interior nodes use `interior`.
pub fn line_sections(n: usize, interior: Section, border: Section) -> Vec<Section> {
    (0..n)
        .map(|k| {
            if k == 0 || k + 1 == n {
                border
            } else {
                interior
            }
        })
        .collect()
}

/// Layout: `u` are nodes along the axis (0..length), `v` are nodes along the height (0..height), `z0` is the bottom.
pub fn layout(
    axis: &Axis,
    z0: f64,
    u: &[f64],
    v: &[f64],
    interior: Section,
    border: Section,
) -> Layout {
    let (su, sv) = (
        line_sections(u.len(), interior, border),
        line_sections(v.len(), interior, border),
    );
    layout_lines(axis, z0, u, v, &su, &sv)
}

/// Layout with a section per node (`su[k]` is line `u[k]`, `sv[k]` is `v[k]`): a cell
/// is inset from an interior line by half its width, from an edge line by the full width.
pub fn layout_lines(
    axis: &Axis,
    z0: f64,
    u: &[f64],
    v: &[f64],
    su: &[Section],
    sv: &[Section],
) -> Layout {
    let g = Grid {
        su: su.to_vec(),
        sv: sv.to_vec(),
        ..Default::default()
    };
    layout_grid(axis, z0, u, v, &g)
}

/// Grid sections: per node (`su`, `sv`), per-segment overrides `segs` as (vertical, line,
/// segment, section): a segment of a vertical line is row `j`, of a horizontal one column `i`.
/// `top` is the top polyline (s, height above the bottom) in ascending s; empty means a straight top.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Grid {
    pub su: Vec<Section>,
    pub sv: Vec<Section>,
    pub segs: Vec<(bool, usize, usize, Section)>,
    pub top: Vec<(f64, f64)>,
}

impl Grid {
    fn line(&self, vertical: bool, k: usize) -> Section {
        let v = if vertical { &self.su } else { &self.sv };
        v.get(k).copied().unwrap_or(Section::None)
    }
    pub fn sec(&self, vertical: bool, k: usize, seg: usize) -> Section {
        self.segs
            .iter()
            .rev()
            .find(|x| x.0 == vertical && x.1 == k && x.2 == seg)
            .map_or_else(|| self.line(vertical, k), |x| x.3)
    }
    fn split(&self, vertical: bool, k: usize) -> bool {
        self.segs.iter().any(|x| x.0 == vertical && x.1 == k)
    }
}

/// Polyline `top` offset down by `w` along the normal of each segment (joints are intersections of the
/// offset segments; for nearly parallel segments the node at s is kept).
pub fn offset_down(top: &[(f64, f64)], w: f64) -> Vec<(f64, f64)> {
    if top.len() < 2 || w == 0.0 {
        return top.to_vec();
    }
    // segment i: z = m·s + c − w·√(1+m²)
    let lines: Vec<(f64, f64)> = top
        .windows(2)
        .map(|p| {
            let ds = (p[1].0 - p[0].0).max(1e-12);
            let m = (p[1].1 - p[0].1) / ds;
            (m, p[0].1 - m * p[0].0 - w * (1.0 + m * m).sqrt())
        })
        .collect();
    let at = |l: (f64, f64), s: f64| l.0 * s + l.1;
    let mut out = vec![(top[0].0, at(lines[0], top[0].0))];
    for k in 1..lines.len() {
        let (a, b) = (lines[k - 1], lines[k]);
        let s = if (a.0 - b.0).abs() > 1e-9 {
            ((b.1 - a.1) / (a.0 - b.0)).clamp(top[k - 1].0, top[k + 1].0)
        } else {
            top[k].0
        };
        out.push((s, at(a, s).min(at(b, s))));
    }
    let l = top[top.len() - 1].0;
    out.push((l, at(lines[lines.len() - 1], l)));
    out
}

/// Points on (a, b) where `f(s) − z` changes sign, ascending.
fn crossings(f: &[(f64, f64)], z: f64, a: f64, b: f64) -> Vec<f64> {
    let mut x = vec![];
    for w in f.windows(2) {
        let ((s0, h0), (s1, h1)) = (w[0], w[1]);
        if (h0 - z) * (h1 - z) < 0.0 && s1 > s0 {
            let s = s0 + (z - h0) * (s1 - s0) / (h1 - h0);
            if s > a && s < b {
                x.push(s);
            }
        }
    }
    x
}

/// Intervals within (a, b) where `f(s) >= z`.
fn above(f: &[(f64, f64)], z: f64, a: f64, b: f64) -> Vec<(f64, f64)> {
    let mut pts = vec![a];
    pts.extend(crossings(f, z, a, b));
    pts.push(b);
    pts.windows(2)
        .filter(|w| w[1] - w[0] > 1e-9 && crate::top::height_at(f, (w[0] + w[1]) / 2.0) >= z)
        .map(|w| (w[0], w[1]))
        .collect()
}

/// General layout: sections per line and segment, polyline top.
pub fn layout_grid(axis: &Axis, z0: f64, u: &[f64], v: &[f64], g: &Grid) -> Layout {
    let h = v.last().copied().unwrap_or(0.0);
    let sloped = g.top.len() >= 2 && g.top.iter().any(|t| (t.1 - h).abs() > 1e-6);
    let p = |s: f64, z: f64| {
        let q = axis.point_at(s);
        v3(q.x, q.y, z0 + z)
    };
    let n = |s: f64| {
        let l = axis.left_at(s);
        v3(l.x, l.y, 0.0)
    };
    let (nu, nv) = (u.len().saturating_sub(1), v.len().saturating_sub(1));
    let wtop = g.line(false, nv).width();
    // inner edge of the top edge mullion
    let inner = if sloped {
        offset_down(&g.top, wtop)
    } else {
        vec![]
    };
    let mut cells = vec![];
    for i in 0..nu {
        for j in 0..nv {
            let c = [
                p(u[i], v[j]),
                p(u[i + 1], v[j]),
                p(u[i + 1], v[j + 1]),
                p(u[i], v[j + 1]),
            ];
            let width = (c[1] - c[0]).len();
            let height = v[j + 1] - v[j];
            let near = |w: f64, edge: bool| if edge { w } else { w / 2.0 };
            let s0 = near(g.sec(true, i, j).width(), i == 0);
            let s1 = near(g.sec(true, i + 1, j).width(), i + 1 == nu);
            let h0 = near(g.sec(false, j, i).width(), j == 0);
            let h1 = near(g.sec(false, j + 1, i).width(), j + 1 == nv);
            let (nw, nh) = ((width - s0 - s1).max(0.0), (height - h0 - h1).max(0.0));
            let side = (c[1] - c[0]) * (1.0 / width.max(1e-12));
            let b0 = c[0] + side * s0 + v3(0.0, 0.0, h0);
            let net = [
                b0,
                b0 + side * nw,
                b0 + side * nw + v3(0.0, 0.0, nh),
                b0 + v3(0.0, 0.0, nh),
            ];
            let (xa, xb, za, zb) = (s0, s0 + nw, v[j] + h0, v[j] + h0 + nh);
            let mut outline = vec![(xa, za), (xb, za), (xb, zb), (xa, zb)];
            let mut net_area = nw * nh;
            if sloped && nw > 0.0 && nh > 0.0 {
                // x along the chord -> s along the axis
                let k = (u[i + 1] - u[i]) / width.max(1e-12);
                let fx: Vec<(f64, f64)> = inner.iter().map(|q| ((q.0 - u[i]) / k, q.1)).collect();
                let mut xs = vec![xa, xb];
                xs.extend(fx.iter().map(|q| q.0).filter(|x| *x > xa && *x < xb));
                xs.extend(crossings(&fx, zb, xa, xb));
                xs.extend(crossings(&fx, za, xa, xb));
                xs.sort_by(f64::total_cmp);
                xs.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
                outline = vec![(xa, za), (xb, za)];
                for &x in xs.iter().rev() {
                    outline.push((x, crate::top::height_at(&fx, x).clamp(za, zb)));
                }
                let mut a2 = 0.0;
                for k in 0..outline.len() {
                    let (p0, p1) = (outline[k], outline[(k + 1) % outline.len()]);
                    a2 += p0.0 * p1.1 - p1.0 * p0.1;
                }
                net_area = (a2 / 2.0).abs();
            }
            cells.push(Cell {
                i,
                j,
                corners: c,
                width,
                height,
                area: width * height,
                net,
                net_area,
                outline,
            });
        }
    }
    let top_at = |s: f64| {
        if sloped {
            crate::top::height_at(&g.top, s)
        } else {
            h
        }
    };
    // an edge mullion is shifted inward by half its width; verticals are solid, except
    // lines with differing segments (one row per segment)
    let mut bars = vec![];
    if nv > 0 {
        for (k, &s) in u.iter().enumerate() {
            let l = axis.left_at(s);
            let t = v3(l.y, -l.x, 0.0);
            let inward = if k == 0 {
                t
            } else if k == nu {
                -t
            } else {
                V3::default()
            };
            let ztop = top_at(s);
            let pieces: Vec<(f64, f64, usize, Section)> = if g.split(true, k) {
                (0..nv)
                    .filter(|&j| v[j] < ztop)
                    .map(|j| (v[j], v[j + 1].min(ztop), j, g.sec(true, k, j)))
                    .collect()
            } else {
                vec![(0.0, ztop, 0, g.line(true, k))]
            };
            for (za, zb, seg, section) in pieces {
                bars.push(Bar {
                    a: p(s, za),
                    b: p(s, zb),
                    border: k == 0 || k == nu,
                    vertical: true,
                    normal: n(s),
                    inward,
                    line: k,
                    seg,
                    section,
                });
            }
        }
    }
    for (k, &z) in v.iter().enumerate() {
        if sloped && k == nv {
            continue;
        }
        for i in 0..nu {
            let section = g.sec(false, k, i);
            // faces of the verticals at this height: rows on both sides of the line
            let face = |line: usize| {
                let r0 = g.sec(true, line, k.min(nv.saturating_sub(1))).width();
                let r1 = g.sec(true, line, k.saturating_sub(1)).width();
                r0.max(r1)
            };
            let (a, b) = (p(u[i], z), p(u[i + 1], z));
            let len = (b - a).len();
            let s0 = if i == 0 { face(i) } else { face(i) / 2.0 };
            let s1 = if i + 1 == nu {
                face(i + 1)
            } else {
                face(i + 1) / 2.0
            };
            if len - s0 - s1 <= 1e-9 {
                continue;
            }
            let d = (b - a) * (1.0 / len);
            let ks = (u[i + 1] - u[i]) / len;
            let spans: Vec<(f64, f64)> = if sloped && k > 0 {
                above(
                    &inner,
                    z + section.width() / 2.0,
                    u[i] + s0 * ks,
                    u[i + 1] - s1 * ks,
                )
                .into_iter()
                .map(|(sa, sb)| ((sa - u[i]) / ks, (sb - u[i]) / ks))
                .collect()
            } else {
                vec![(s0, len - s1)]
            };
            for (xa, xb) in spans {
                bars.push(Bar {
                    a: a + d * xa,
                    b: a + d * xb,
                    border: k == 0 || k == nv,
                    vertical: false,
                    normal: n((u[i] + u[i + 1]) / 2.0),
                    inward: if k == 0 {
                        v3(0.0, 0.0, 1.0)
                    } else if k == nv {
                        v3(0.0, 0.0, -1.0)
                    } else {
                        V3::default()
                    },
                    line: k,
                    seg: i,
                    section,
                });
            }
        }
    }
    if sloped && nv > 0 {
        // top edge mullion along the polyline: axis half a width below the outline,
        // pieces between the faces of the verticals
        let mid = offset_down(&g.top, wtop / 2.0);
        let section = g.line(false, nv);
        for i in 0..nu {
            let w0 = g.sec(true, i, nv - 1).width();
            let w1 = g.sec(true, i + 1, nv - 1).width();
            let sa = u[i] + if i == 0 { w0 } else { w0 / 2.0 };
            let sb = u[i + 1] - if i + 1 == nu { w1 } else { w1 / 2.0 };
            if sb - sa <= 1e-9 {
                continue;
            }
            let mut ss = vec![sa];
            ss.extend(
                mid.iter()
                    .map(|q| q.0)
                    .filter(|x| *x > sa + 1e-9 && *x < sb - 1e-9),
            );
            ss.push(sb);
            for w in ss.windows(2) {
                let za = crate::top::height_at(&mid, w[0]);
                let zb = crate::top::height_at(&mid, w[1]);
                bars.push(Bar {
                    a: p(w[0], za),
                    b: p(w[1], zb),
                    border: true,
                    vertical: false,
                    normal: n((w[0] + w[1]) / 2.0),
                    inward: V3::default(),
                    line: nv,
                    seg: i,
                    section,
                });
            }
        }
    }
    Layout {
        u: u.to_vec(),
        v: v.to_vec(),
        cells,
        bars,
    }
}

/// Closed prism along `a→b` over the section outline (counter-clockwise in the `side`, `up` axes).
fn prism(m: &mut Mesh, a: V3, b: V3, side: V3, up: V3, sec: &[(f64, f64)], face: u32) {
    let base = m.positions.len() as u32;
    let k = sec.len() as u32;
    for &o in &[a, b] {
        for &(x, y) in sec {
            m.positions.push(o + side * x + up * y);
        }
    }
    let mut tri = |t: [u32; 3]| {
        m.triangles.push(t);
        m.face_of.push(face);
    };
    for i in 0..k {
        let j = (i + 1) % k;
        tri([base + i, base + j, base + k + j]);
        tri([base + i, base + k + j, base + k + i]);
    }
    for i in 1..k - 1 {
        tri([base, base + i + 1, base + i]);
        tri([base + k, base + k + i, base + k + i + 1]);
    }
}

fn section_pts(s: Section) -> Option<Vec<(f64, f64)>> {
    match s {
        Section::None => None,
        Section::Rect { w, d } => Some(vec![
            (-w / 2.0, -d / 2.0),
            (w / 2.0, -d / 2.0),
            (w / 2.0, d / 2.0),
            (-w / 2.0, d / 2.0),
        ]),
        Section::Circle { d } => Some(
            (0..16)
                .map(|i| {
                    let t = i as f64 * std::f64::consts::TAU / 16.0;
                    (d / 2.0 * t.cos(), d / 2.0 * t.sin())
                })
                .collect(),
        ),
    }
}

/// Mesh: panels (`panel[k]` is the panel thickness of cell `k`, `None` is an empty cell) and mullions.
/// Faces: cell k -> face k, mullion -> cells.len() + mullion index (section is `Bar::section`).
pub fn mesh(l: &Layout, panel: &[Option<f64>]) -> Mesh {
    let mut m = Mesh::default();
    for (k, c) in l.cells.iter().enumerate() {
        let Some(t) = panel.get(k).copied().flatten() else {
            continue;
        };
        let along = c.corners[1] - c.corners[0];
        let side = along * (1.0 / along.len());
        let nrm = v3(-side.y, side.x, 0.0);
        let (a, b) = (c.net[0], c.net[3]);
        let w = (c.net[1] - c.net[0]).len();
        if (c.net_area - w * (b - a).len()).abs() > 1e-12 {
            // clipped panel: outline in the wall plane, extruded along the normal
            if c.net_area <= 1e-9 {
                continue;
            }
            let o = c.corners[0] - v3(0.0, 0.0, c.corners[0].z);
            let base = v3(o.x, o.y, c.corners[0].z - (l.v[c.j]));
            let up = v3(0.0, 0.0, 1.0);
            // side x up = -nrm: extrusion from +nrm·t/2 to -nrm·t/2
            prism(
                &mut m,
                base + nrm * (t / 2.0),
                base - nrm * (t / 2.0),
                side,
                up,
                &c.outline,
                k as u32,
            );
            continue;
        }
        if w <= 1e-9 || (b - a).len() <= 1e-9 {
            continue;
        }
        let sec = [(0.0, -t / 2.0), (w, -t / 2.0), (w, t / 2.0), (0.0, t / 2.0)];
        prism(&mut m, a, b, side, nrm, &sec, k as u32);
    }
    for (k, bar) in l.bars.iter().enumerate() {
        let s = bar.section;
        let Some(sec) = section_pts(s) else { continue };
        let dir = bar.b - bar.a;
        if dir.len() <= 1e-9 {
            continue;
        }
        let d = dir * (1.0 / dir.len());
        let shift = bar.inward * (s.width() / 2.0);
        // side: in the wall plane across the bar; up: wall normal (side × up = bar direction)
        let side = bar.normal.cross(d).normalized();
        prism(
            &mut m,
            bar.a + shift,
            bar.b + shift,
            side,
            bar.normal,
            &sec,
            (l.cells.len() + k) as u32,
        );
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_6x3_by_1_5x1_gives_12_panels_18m2_39m_bars() {
        let ax = Axis::new(v2(0.0, 0.0), v2(6.0, 0.0), 0.0).unwrap();
        let u = spacing_lines(6.0, 1.5);
        let v = spacing_lines(3.0, 1.0);
        assert_eq!(u, vec![0.0, 1.5, 3.0, 4.5, 6.0]);
        let rect = Section::Rect { w: 0.05, d: 0.15 };
        let l = layout(&ax, 0.0, &u, &v, rect, rect);
        assert_eq!(l.cells.len(), 12);
        let area: f64 = l.cells.iter().map(|c| c.area).sum();
        assert!((area - 18.0).abs() < 1e-12);
        // (6 − 2·0.05 − 3·0.05) × (3 − 2·0.05 − 2·0.05)
        let net: f64 = l.cells.iter().map(|c| c.net_area).sum();
        assert!((net - 16.1).abs() < 1e-12, "{net}");
        // verticals continuous: 3·3 + 2·3; transoms between vertical faces: 2·5.75 each kind
        assert_eq!(l.bars.len(), 5 + 4 * 4);
        let inner: f64 = l.bars.iter().filter(|b| !b.border).map(Bar::length).sum();
        let outer: f64 = l.bars.iter().filter(|b| b.border).map(Bar::length).sum();
        assert!((inner - 20.5).abs() < 1e-12, "{inner}");
        assert!((outer - 17.5).abs() < 1e-12, "{outer}");
        let m = mesh(&l, &vec![Some(0.025); 12]);
        assert!(m.is_watertight());
        // panels + bars as disjoint prisms: volume is the plain sum
        let want = 16.1 * 0.025 + 38.0 * 0.05 * 0.15;
        assert!((m.volume() - want).abs() < 1e-9, "{}", m.volume());
    }

    #[test]
    fn explicit_and_arc_grid() {
        assert_eq!(
            explicit_lines(5.0, &[4.0, 1.0, 1.0, 9.0]),
            vec![0.0, 1.0, 4.0, 5.0]
        );
        assert_eq!(spacing_lines(3.0, 1.5), vec![0.0, 1.5, 3.0]);
        // 1 mm: near the start, a duplicate and near the end are dropped
        assert_eq!(
            explicit_lines(5.0, &[0.0000011, 3.0, 3.0000001, 4.9995]),
            vec![0.0, 3.0, 5.0]
        );
        // half circle r=2: chords of 4 equal panels
        let ax = Axis::new(v2(-2.0, 0.0), v2(2.0, 0.0), -1.0).unwrap();
        let len = ax.length();
        assert!((len - std::f64::consts::PI * 2.0).abs() < 1e-9);
        let u = spacing_lines(len, len / 4.0 + 1e-12);
        let l = layout(&ax, 1.0, &u, &[0.0, 3.0], Section::None, Section::None);
        assert_eq!(l.cells.len(), 4);
        let chord = 2.0 * 2.0 * (std::f64::consts::PI / 8.0).sin();
        for c in &l.cells {
            assert!((c.width - chord).abs() < 1e-9 && (c.area - 3.0 * chord).abs() < 1e-9);
            assert!((c.corners[0].z - 1.0).abs() < 1e-12);
        }
        let l = layout(
            &ax,
            1.0,
            &u,
            &[0.0, 3.0],
            Section::Circle { d: 0.06 },
            Section::None,
        );
        let m = mesh(&l, &[Some(0.02); 4]);
        assert!(m.is_watertight());
    }

    /// No two mullion bodies overlap: interiors of their bounding boxes in wall coordinates.
    #[test]
    fn mullions_do_not_overlap() {
        let ax = Axis::new(v2(0.0, 0.0), v2(6.0, 0.0), 0.0).unwrap();
        let (u, v) = (spacing_lines(6.0, 1.5), spacing_lines(3.0, 1.0));
        let (wi, wb) = (0.05, 0.08);
        let l = layout(
            &ax,
            0.0,
            &u,
            &v,
            Section::Rect { w: wi, d: 0.1 },
            Section::Rect { w: wb, d: 0.1 },
        );
        let boxes: Vec<[f64; 4]> = l
            .bars
            .iter()
            .map(|b| {
                let w = if b.border { wb } else { wi };
                let c = (b.a + b.b) * 0.5 + b.inward * (w / 2.0);
                let (hx, hz) = if b.vertical {
                    (w / 2.0, b.length() / 2.0)
                } else {
                    (b.length() / 2.0, w / 2.0)
                };
                [c.x - hx, c.z - hz, c.x + hx, c.z + hz]
            })
            .collect();
        for i in 0..boxes.len() {
            for j in i + 1..boxes.len() {
                let (a, b) = (boxes[i], boxes[j]);
                let ov = (a[2].min(b[2]) - a[0].max(b[0])).min(a[3].min(b[3]) - a[1].max(b[1]));
                assert!(ov <= 1e-12, "bars {i} and {j} overlap by {ov}");
            }
        }
        let len: f64 = l.bars.iter().map(Bar::length).sum();
        // 5 verticals × 3 + 4 rows × (6 − 2·0.08 − 3·0.05)
        assert!((len - (15.0 + 4.0 * 5.69)).abs() < 1e-12, "{len}");
    }
    /// Gable top: panels are clipped under the inner edge of the sloped top mullion, the net
    /// area adds up to the area under the edge; the top mullion follows the slope.
    #[test]
    fn sloped_top_clips_panels_and_follows_the_edge() {
        let ax = Axis::new(v2(0.0, 0.0), v2(4.0, 0.0), 0.0).unwrap();
        let (u, v) = (vec![0.0, 2.0, 4.0], vec![0.0, 1.0, 3.0]);
        let mut g = Grid {
            su: line_sections(3, Section::None, Section::None),
            sv: line_sections(3, Section::None, Section::None),
            top: vec![(0.0, 2.0), (2.0, 3.0), (4.0, 2.0)],
            ..Default::default()
        };
        let l = layout_grid(&ax, 0.0, &u, &v, &g);
        let areas: Vec<f64> = l.cells.iter().map(|c| c.net_area).collect();
        for (a, w) in areas.iter().zip([2.0, 3.0, 2.0, 3.0]) {
            assert!((a - w).abs() < 1e-12, "{areas:?}");
        }
        let rect = Section::Rect { w: 0.1, d: 0.1 };
        g.su = line_sections(3, rect, rect);
        g.sv = line_sections(3, rect, rect);
        let l = layout_grid(&ax, 0.0, &u, &v, &g);
        // the top border pieces run along the slope (rise 1 over 2)
        let tops: Vec<&Bar> = l
            .bars
            .iter()
            .filter(|b| b.border && !b.vertical && b.line == 2)
            .collect();
        assert_eq!(tops.len(), 2);
        for b in &tops {
            let d = b.b - b.a;
            assert!((d.z.abs() / d.x.abs() - 0.5).abs() < 1e-9, "{d:?}");
        }
        // verticals reach the edge: the middle one to 3, the borders to 2
        let vh: Vec<f64> = l
            .bars
            .iter()
            .filter(|b| b.vertical)
            .map(|b| b.b.z)
            .collect();
        assert_eq!(vh, vec![2.0, 3.0, 2.0]);
        // net area: under the inner edge minus the other mullions
        let k = (1.0f64 + 0.25).sqrt() * 0.1;
        let edge = |x: f64| 2.0 + x / 2.0 - k;
        let (a, b) = (0.1, 1.95);
        let low = (b - a) * (1.0 - 0.1 - 0.05);
        let high = ((edge(a) + edge(b)) / 2.0 - 1.05) * (b - a);
        let want = 2.0 * (low + high);
        let got: f64 = l.cells.iter().map(|c| c.net_area).sum();
        assert!((got - want).abs() < 1e-9, "{got} {want}");
        let m = mesh(&l, &vec![Some(0.02); 4]);
        assert!(m.is_watertight());
        let panels = got * 0.02;
        assert!(m.volume() > panels, "{}", m.volume());
    }

    /// A grid line can differ per line and per segment: no mullion on one segment leaves the
    /// two cells against each other, a wider line insets its cells by half its width.
    #[test]
    fn line_and_segment_sections() {
        let ax = Axis::new(v2(0.0, 0.0), v2(4.0, 0.0), 0.0).unwrap();
        let (u, v) = (vec![0.0, 2.0, 4.0], vec![0.0, 1.0, 3.0]);
        let (a, b) = (
            Section::Rect { w: 0.1, d: 0.1 },
            Section::Rect { w: 0.3, d: 0.1 },
        );
        let mut su = line_sections(3, a, a);
        su[1] = b;
        let g = Grid {
            su,
            sv: line_sections(3, a, a),
            segs: vec![(false, 1, 0, Section::None)],
            top: vec![],
        };
        let l = layout_grid(&ax, 0.0, &u, &v, &g);
        let c00 = &l.cells[0];
        assert!((c00.net[3].z - 1.0).abs() < 1e-12 && (c00.net[1].x - 1.85).abs() < 1e-12);
        let c01 = &l.cells[1];
        assert!((c01.net[0].z - 1.0).abs() < 1e-12);
        let c11 = &l.cells[3];
        assert!((c11.net[0].z - 1.05).abs() < 1e-12 && (c11.net[0].x - 2.15).abs() < 1e-12);
        assert!(!l
            .bars
            .iter()
            .any(|x| !x.vertical && x.line == 1 && x.seg == 0 && x.section != Section::None));
        let m = mesh(&l, &vec![Some(0.02); 4]);
        assert!(m.is_watertight());
    }
}
