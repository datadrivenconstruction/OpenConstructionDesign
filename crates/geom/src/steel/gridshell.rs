//! Surface-hosted node graphs: diagrids, gridshells and cable nets generated on any
//! [`HostSurface`]. Patterns: diagrid (two helical families and edge rings), triangular, quad
//! with alternating diagonal bracing, and plain quad (cable nets). Each node carries the surface
//! normal; the node frame is that normal and the bisector of two member families. A straight
//! member is rolled to the mean of its end normals (one web plane for both ends, no torsion);
//! a curved lath follows a C1 Hermite curve through its line of nodes. Includes the node clash
//! check and the data the form finder needs (edges, supports, lath bends).

use std::collections::HashMap;

use super::lattice::{Frame, MemRole};
use crate::formfind::Bend;
use crate::host::HostSurface;
use crate::math::{v3, V3};

/// Grid divisions above this per direction are refused (a node table stays editable).
pub const MAX_DIV: usize = 400;
/// Line id of members that do not continue through nodes (bracing).
pub const NO_LINE: u32 = u32::MAX;

/// What the grid is built as (members, nodes and IFC classes follow from it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridKind {
    /// Straight steel members between ball nodes.
    Diagrid,
    /// Continuous curved laths clamped at the crossings (bending-active).
    Gridshell,
    /// Straight cable segments between clamps.
    CableNet,
}

impl GridKind {
    pub const ALL: [GridKind; 3] = [GridKind::Diagrid, GridKind::Gridshell, GridKind::CableNet];
    pub fn key(self) -> &'static str {
        match self {
            GridKind::Diagrid => "diagrid",
            GridKind::Gridshell => "gridshell",
            GridKind::CableNet => "cable_net",
        }
    }
    pub fn parse(s: &str) -> Option<GridKind> {
        GridKind::ALL.into_iter().find(|k| k.key() == s.trim())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// Nodes on alternate lattice points, members along both lattice diagonals, edge members
    /// along the domain boundary.
    Diagrid,
    /// Quad lines plus one diagonal family.
    Triangular,
    /// Quad lines plus one diagonal per cell, alternating direction.
    QuadBraced,
    /// Quad lines only (cable nets, gridshells of two lath families).
    Quad,
}

impl Pattern {
    pub const ALL: [Pattern; 4] = [Pattern::Diagrid, Pattern::Triangular, Pattern::QuadBraced, Pattern::Quad];
    pub fn key(self) -> &'static str {
        match self {
            Pattern::Diagrid => "diagrid",
            Pattern::Triangular => "triangular",
            Pattern::QuadBraced => "quad_braced",
            Pattern::Quad => "quad",
        }
    }
    pub fn parse(s: &str) -> Option<Pattern> {
        Pattern::ALL.into_iter().find(|k| k.key() == s.trim())
    }
}

/// Which nodes are supported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Supports {
    /// The first row (v = v0): the springing line of a dome or vault.
    Base,
    /// Every node on the domain boundary.
    Edges,
    /// The four corners of an open grid.
    Corners,
}

impl Supports {
    pub const ALL: [Supports; 3] = [Supports::Base, Supports::Edges, Supports::Corners];
    pub fn key(self) -> &'static str {
        match self {
            Supports::Base => "base",
            Supports::Edges => "edges",
            Supports::Corners => "corners",
        }
    }
    pub fn parse(s: &str) -> Option<Supports> {
        Supports::ALL.into_iter().find(|k| k.key() == s.trim())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BarRole {
    Grid,
    Brace,
    Edge,
}

impl BarRole {
    pub fn key(self) -> &'static str {
        match self {
            BarRole::Grid => "grid",
            BarRole::Brace => "brace",
            BarRole::Edge => "edge",
        }
    }
    pub fn parse(s: &str) -> Option<BarRole> {
        [BarRole::Grid, BarRole::Brace, BarRole::Edge]
            .into_iter()
            .find(|k| k.key() == s.trim())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GNode {
    pub p: V3,
    /// Unit surface normal.
    pub n: V3,
    /// Host parameters the node was generated at.
    pub uv: (f64, f64),
    pub fixed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GBar {
    pub a: u32,
    pub b: u32,
    pub role: BarRole,
    /// Continuous line (lath, cable, helix) the member belongs to; [`NO_LINE`] for bracing.
    pub line: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Grid {
    pub nodes: Vec<GNode>,
    pub bars: Vec<GBar>,
    /// Faces of the net (node indices counter-clockwise in u, v), for normals after the net
    /// leaves its host.
    pub cells: Vec<Vec<u32>>,
}

/// Generates a grid of `nu` x `nv` lattice divisions over the host domain.
pub fn generate(
    host: &dyn HostSurface,
    pat: Pattern,
    nu: usize,
    nv: usize,
    sup: Supports,
) -> Result<Grid, String> {
    let closed = host.closed_u();
    if !(1..=MAX_DIV).contains(&nu) || !(1..=MAX_DIV).contains(&nv) {
        return Err(format!("grid divisions must be 1 to {MAX_DIV}, got {nu} x {nv}"));
    }
    if closed && nu < 3 {
        return Err("a closed grid needs at least 3 divisions around".into());
    }
    if pat == Pattern::Diagrid {
        if nu < 2 || nv < 2 {
            return Err("a diagrid needs at least 2 x 2 divisions".into());
        }
        if closed && nu % 2 == 1 {
            return Err(format!(
                "a closed diagrid needs an even number of divisions around, got {nu}"
            ));
        }
    }
    if closed && sup == Supports::Corners {
        return Err("corner supports need an open grid".into());
    }
    let ((u0, u1), (v0, v1)) = host.domain();
    let ni = if closed { nu } else { nu + 1 };
    let wrap = |i: usize| if closed { i % nu } else { i };
    let has = |i: usize, j: usize| pat != Pattern::Diagrid || (i + j) % 2 == 0;
    let mut g = Grid::default();
    let mut id = vec![None; ni * (nv + 1)];
    for j in 0..=nv {
        for i in 0..ni {
            if !has(i, j) {
                continue;
            }
            let u = u0 + (u1 - u0) * i as f64 / nu as f64;
            let v = v0 + (v1 - v0) * j as f64 / nv as f64;
            let n = host.normal(u, v);
            if !(n.len() > 0.5) {
                return Err(format!("the host has no normal at u = {u:.4}, v = {v:.4}"));
            }
            let on_side = !closed && (i == 0 || i == nu);
            let fixed = match sup {
                Supports::Base => j == 0,
                Supports::Edges => j == 0 || j == nv || on_side,
                Supports::Corners => on_side && (j == 0 || j == nv),
            };
            id[j * ni + i] = Some(g.nodes.len() as u32);
            g.nodes.push(GNode {
                p: host.point(u, v),
                n,
                uv: (u, v),
                fixed,
            });
        }
    }
    let at = |i: usize, j: usize| -> Option<u32> {
        if j > nv {
            return None;
        }
        let i = wrap(i);
        if i >= ni {
            return None;
        }
        id[j * ni + i]
    };
    let add = |g: &mut Grid, a: Option<u32>, b: Option<u32>, role: BarRole, line: u32| {
        if let (Some(a), Some(b)) = (a, b) {
            if a != b {
                g.bars.push(GBar { a, b, role, line });
            }
        }
    };
    // cells and bars run i -> i + 1 for i < nu (wrapping when closed)
    let cols = nu;
    let nl = |k: usize| k as u32;
    match pat {
        Pattern::Quad | Pattern::QuadBraced | Pattern::Triangular => {
            for j in 0..=nv {
                let role = if j == 0 || j == nv { BarRole::Edge } else { BarRole::Grid };
                for i in 0..cols {
                    add(&mut g, at(i, j), at(i + 1, j), role, nl(j));
                }
            }
            for i in 0..ni {
                let role = if !closed && (i == 0 || i == nu) {
                    BarRole::Edge
                } else {
                    BarRole::Grid
                };
                for j in 0..nv {
                    add(&mut g, at(i, j), at(i, j + 1), role, nl(nv + 1 + i));
                }
            }
            let base = nv + 1 + ni;
            for j in 0..nv {
                for i in 0..cols {
                    let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1));
                    let q: Vec<u32> = [a, b, c, d].into_iter().flatten().collect();
                    match pat {
                        Pattern::Quad => g.cells.push(q),
                        Pattern::Triangular => {
                            // diagonal lines i - j = const continue through nodes
                            let line = if closed {
                                (i + nu - j % nu) % nu
                            } else {
                                i + nv - j
                            };
                            add(&mut g, a, c, BarRole::Grid, nl(base + line));
                            g.cells.push(vec![q[0], q[1], q[2]]);
                            g.cells.push(vec![q[0], q[2], q[3]]);
                        }
                        _ => {
                            if (i + j) % 2 == 0 {
                                add(&mut g, a, c, BarRole::Brace, NO_LINE);
                                g.cells.push(vec![q[0], q[1], q[2]]);
                                g.cells.push(vec![q[0], q[2], q[3]]);
                            } else {
                                add(&mut g, b, d, BarRole::Brace, NO_LINE);
                                g.cells.push(vec![q[0], q[1], q[3]]);
                                g.cells.push(vec![q[1], q[2], q[3]]);
                            }
                        }
                    }
                }
            }
        }
        Pattern::Diagrid => {
            let fam = if closed { nu } else { nu + nv + 1 };
            for j in 0..nv {
                for i in 0..cols {
                    if !has(i, j) {
                        continue;
                    }
                    let la = if closed { (i + nu - j % nu) % nu } else { i + nv - j };
                    add(&mut g, at(i, j), at(i + 1, j + 1), BarRole::Grid, nl(la));
                }
                for i in 0..ni {
                    // family B rises toward lower i: (i, j) -> (i - 1, j + 1)
                    if !has(i, j) || (!closed && i == 0) {
                        continue;
                    }
                    let im = if i == 0 { nu - 1 } else { i - 1 };
                    let lb = if closed { (i + j) % nu } else { i + j };
                    add(&mut g, at(i, j), at(im, j + 1), BarRole::Grid, nl(fam + lb));
                }
            }
            let eb = 2 * fam;
            for (j, line) in [(0, eb), (nv, eb + 1)] {
                for i in 0..cols {
                    if has(i, j) && (closed || i + 2 <= nu) {
                        add(&mut g, at(i, j), at(i + 2, j), BarRole::Edge, nl(line));
                    }
                }
            }
            if !closed {
                for (i, line) in [(0, eb + 2), (nu, eb + 3)] {
                    for j in 0..nv.saturating_sub(1) {
                        if has(i, j) {
                            add(&mut g, at(i, j), at(i, j + 2), BarRole::Edge, nl(line));
                        }
                    }
                }
            }
            // diamonds around the empty lattice points, triangles at the boundary
            for b in 0..=nv {
                for a in 0..ni {
                    if has(a, b) {
                        continue;
                    }
                    let left = if a == 0 {
                        if closed {
                            at(nu - 1, b)
                        } else {
                            None
                        }
                    } else {
                        at(a - 1, b)
                    };
                    let down = if b == 0 { None } else { at(a, b - 1) };
                    let c: Vec<u32> = [down, at(a + 1, b), at(a, b + 1), left]
                        .into_iter()
                        .flatten()
                        .collect();
                    if c.len() >= 3 {
                        g.cells.push(c);
                    }
                }
            }
        }
    }
    Ok(g)
}

fn unit_or(d: V3, alt: V3) -> V3 {
    let l = d.len();
    if l > 1e-15 {
        d * (1.0 / l)
    } else {
        alt
    }
}

/// Signed angle from `a` to `b` about unit `t`.
fn signed(a: V3, b: V3, t: V3) -> f64 {
    a.cross(b).dot(t).atan2(a.dot(b))
}

/// Neighbours of a node along one line: (node, line) -> nodes.
pub type LineNbrs = HashMap<(u32, u32), Vec<u32>>;

impl Grid {
    pub fn edges(&self) -> Vec<(u32, u32)> {
        self.bars.iter().map(|b| (b.a, b.b)).collect()
    }

    pub fn fixed(&self) -> Vec<bool> {
        self.nodes.iter().map(|n| n.fixed).collect()
    }

    pub fn positions(&self) -> Vec<V3> {
        self.nodes.iter().map(|n| n.p).collect()
    }

    /// Bars at each node.
    pub fn adjacency(&self) -> Vec<Vec<u32>> {
        let mut adj = vec![vec![]; self.nodes.len()];
        for (k, b) in self.bars.iter().enumerate() {
            adj[b.a as usize].push(k as u32);
            adj[b.b as usize].push(k as u32);
        }
        adj
    }

    pub fn line_nbrs(&self) -> LineNbrs {
        let mut m: LineNbrs = HashMap::new();
        for b in &self.bars {
            if b.line == NO_LINE {
                continue;
            }
            m.entry((b.a, b.line)).or_default().push(b.b);
            m.entry((b.b, b.line)).or_default().push(b.a);
        }
        m
    }

    /// Node frame: x the acute bisector of the first two member lines through the node (line
    /// tangents in the tangent plane), z the normal.
    pub fn node_frame(&self, i: usize, adj: &[Vec<u32>]) -> [V3; 3] {
        let nd = &self.nodes[i];
        let z = nd.n;
        let flat = |d: V3| {
            let t = d - z * d.dot(z);
            (t.len() > 1e-12).then(|| t * (1.0 / t.len()))
        };
        // neighbours per line, in the order the lines first appear at the node
        let mut lines: Vec<(u32, Vec<V3>)> = vec![];
        for &k in &adj[i] {
            let b = &self.bars[k as usize];
            let o = if b.a as usize == i { b.b } else { b.a };
            let p = self.nodes[o as usize].p;
            match lines.iter_mut().find(|l| l.0 == b.line && b.line != NO_LINE) {
                Some(l) => l.1.push(p),
                None => lines.push((b.line, vec![p])),
            }
        }
        let tangent = |ps: &[V3]| match ps {
            [a, b, ..] => flat(*b - *a),
            [a] => flat(*a - nd.p),
            [] => None,
        };
        let ts: Vec<V3> = lines.iter().filter_map(|l| tangent(&l.1)).collect();
        let fallback = unit_or(
            v3(1.0, 0.0, 0.0) - z * z.x,
            unit_or(v3(0.0, 1.0, 0.0) - z * z.y, v3(1.0, 0.0, 0.0)),
        );
        let x = match ts.as_slice() {
            [] => fallback,
            [t] => *t,
            [t1, t2, ..] => {
                let t2 = if t1.dot(*t2) < 0.0 { -*t2 } else { *t2 };
                unit_or(*t1 + t2, *t1)
            }
        };
        [x, z.cross(x), z]
    }

    /// Web direction of a straight member: the bisector of its two end normals seen square to
    /// the axis, so both ends turn by the same angle the other way (no torsion along it).
    pub fn bar_up(&self, k: usize) -> V3 {
        let b = &self.bars[k];
        let (na, nb) = (self.nodes[b.a as usize], self.nodes[b.b as usize]);
        let e = unit_or(nb.p - na.p, v3(1.0, 0.0, 0.0));
        let sq = |n: V3| {
            let t = n - e * n.dot(e);
            (t.len() > 1e-12).then(|| t * (1.0 / t.len()))
        };
        let w = match (sq(na.n), sq(nb.n)) {
            (Some(a), Some(b)) => a + b,
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => v3(0.0, 0.0, 0.0),
        };
        if w.len() > 1e-9 {
            return w.normalized();
        }
        unit_or(
            v3(0.0, 0.0, 1.0) - e * e.z,
            unit_or(v3(1.0, 0.0, 0.0) - e * e.x, v3(0.0, 1.0, 0.0)),
        )
    }

    /// Largest angle between the member web and the normal at either end (both normals seen
    /// square to the axis): what the node detail has to take up.
    pub fn bar_twist(&self, k: usize) -> f64 {
        let b = &self.bars[k];
        let (na, nb) = (self.nodes[b.a as usize], self.nodes[b.b as usize]);
        let e = unit_or(nb.p - na.p, v3(1.0, 0.0, 0.0));
        let w = self.bar_up(k);
        [na.n, nb.n]
            .iter()
            .map(|n| {
                let t = *n - e * n.dot(e);
                if t.len() < 1e-12 {
                    0.0
                } else {
                    signed(w, t.normalized(), e).abs()
                }
            })
            .fold(0.0, f64::max)
    }

    /// Three interior points of the lath curve of member `k`: a cubic Hermite segment whose end
    /// tangents come from the neighbours on the same line (C1 along the lath).
    pub fn lath_via(&self, k: usize, nb: &LineNbrs) -> Vec<V3> {
        let b = &self.bars[k];
        let (pa, pb) = (self.nodes[b.a as usize].p, self.nodes[b.b as usize].p);
        let other = |at: u32, not: u32| -> Option<V3> {
            nb.get(&(at, b.line))?
                .iter()
                .find(|&&o| o != not)
                .map(|&o| self.nodes[o as usize].p)
        };
        let chord = pb - pa;
        let (ma, mb) = if b.line == NO_LINE {
            (chord, chord)
        } else {
            (
                other(b.a, b.b).map_or(chord, |pp| (pb - pp) * 0.5),
                other(b.b, b.a).map_or(chord, |pn| (pn - pa) * 0.5),
            )
        };
        [0.25, 0.5, 0.75]
            .iter()
            .map(|&t: &f64| {
                let (t2, t3) = (t * t, t * t * t);
                pa * (2.0 * t3 - 3.0 * t2 + 1.0)
                    + ma * (t3 - 2.0 * t2 + t)
                    + pb * (-2.0 * t3 + 3.0 * t2)
                    + mb * (t3 - t2)
            })
            .collect()
    }

    /// Bending elements of every continuous line (consecutive node triples).
    pub fn bends(&self, ei: f64) -> Vec<Bend> {
        let nb = self.line_nbrs();
        let mut out: Vec<Bend> = nb
            .iter()
            .filter(|(_, v)| v.len() == 2)
            .map(|(&(b, _), v)| Bend {
                a: v[0],
                b,
                c: v[1],
                ei,
            })
            .collect();
        out.sort_by_key(|x| (x.b, x.a, x.c));
        out
    }

    /// Normals from the faces of the net (area weighted), oriented like the current normals.
    pub fn normals_from_cells(&mut self) {
        let mut acc = vec![v3(0.0, 0.0, 0.0); self.nodes.len()];
        for c in &self.cells {
            let mut n = v3(0.0, 0.0, 0.0);
            for k in 0..c.len() {
                let (p, q) = (self.nodes[c[k] as usize].p, self.nodes[c[(k + 1) % c.len()] as usize].p);
                n = n + p.cross(q);
            }
            for &i in c {
                acc[i as usize] = acc[i as usize] + n;
            }
        }
        for (nd, a) in self.nodes.iter_mut().zip(acc) {
            if a.len() > 1e-15 {
                let a = a.normalized();
                nd.n = if a.dot(nd.n) < 0.0 { -a } else { a };
            }
        }
    }

    /// Node clashes for nodes of diameter `node_d` and member radii `bar_r` (per bar): nodes
    /// closer than a node plus `gap`, members shorter than that, and members whose sleeves
    /// overlap on the node (angular gap below asin(ri/R) + asin(rj/R) + gap/R).
    pub fn clashes(&self, node_d: f64, bar_r: &[f64], gap: f64) -> Vec<Clash> {
        let mut out = vec![];
        if !(node_d > 0.0) {
            return out;
        }
        let h = node_d + gap.max(0.0);
        let key = |p: V3| {
            (
                (p.x / h).floor() as i64,
                (p.y / h).floor() as i64,
                (p.z / h).floor() as i64,
            )
        };
        let mut cells: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            cells.entry(key(n.p)).or_default().push(i as u32);
        }
        for (i, n) in self.nodes.iter().enumerate() {
            let (x, y, z) = key(n.p);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let Some(v) = cells.get(&(x + dx, y + dy, z + dz)) else {
                            continue;
                        };
                        for &o in v {
                            if (o as usize) <= i {
                                continue;
                            }
                            let d = (self.nodes[o as usize].p - n.p).len();
                            if d < h {
                                out.push(Clash {
                                    node: i as u32,
                                    other: Some(o),
                                    text: format!(
                                        "nodes {i} and {o} are {:.0} mm apart, closer than a node ({:.0} mm)",
                                        d * 1e3,
                                        h * 1e3
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }
        for (k, b) in self.bars.iter().enumerate() {
            let l = (self.nodes[b.b as usize].p - self.nodes[b.a as usize].p).len();
            if l < h {
                out.push(Clash {
                    node: b.a,
                    other: Some(b.b),
                    text: format!("member {k} is {:.0} mm long, shorter than a node", l * 1e3),
                });
            }
        }
        let r = node_d / 2.0;
        let adj = self.adjacency();
        for (i, bars) in adj.iter().enumerate() {
            let p = self.nodes[i].p;
            let dirs: Vec<(u32, V3)> = bars
                .iter()
                .map(|&k| {
                    let b = &self.bars[k as usize];
                    let o = if b.a as usize == i { b.b } else { b.a };
                    (k, unit_or(self.nodes[o as usize].p - p, v3(1.0, 0.0, 0.0)))
                })
                .collect();
            for x in 0..dirs.len() {
                for y in x + 1..dirs.len() {
                    let (ka, da) = dirs[x];
                    let (kb, db) = dirs[y];
                    let th = da.cross(db).len().atan2(da.dot(db));
                    let half = |k: u32| (bar_r.get(k as usize).copied().unwrap_or(0.0) / r).min(1.0).asin();
                    let need = half(ka) + half(kb) + gap.max(0.0) / r;
                    if th + 1e-9 < need {
                        out.push(Clash {
                            node: i as u32,
                            other: None,
                            text: format!(
                                "members {ka} and {kb} meet at {:.1} deg at node {i}; the node needs {:.1} deg",
                                th.to_degrees(),
                                need.to_degrees()
                            ),
                        });
                    }
                }
            }
        }
        out
    }

    /// The grid as a lattice frame (grid members as diagonals, bracing as struts, edges as
    /// chords) for the lattice joints, checks and mass.
    pub fn to_lattice(&self, sec: &str, edge_sec: &str) -> Frame {
        let mut f = Frame::new();
        for b in &self.bars {
            let (na, nb) = (self.nodes[b.a as usize], self.nodes[b.b as usize]);
            let (role, s) = match b.role {
                BarRole::Grid => (MemRole::Diagonal, sec),
                BarRole::Brace => (MemRole::Strut, sec),
                BarRole::Edge => (MemRole::Chord, edge_sec),
            };
            f.add(na.p, nb.p, s, role, na.n);
        }
        f
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Clash {
    pub node: u32,
    pub other: Option<u32>,
    pub text: String,
}
