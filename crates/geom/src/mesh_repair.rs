//! Isotropic remesh and repair for imported triangle meshes.
//!
//! `remesh` brings edge lengths close to a target (split, collapse, flip, tangential smoothing;
//! boundary and sharp-feature vertices stay in place). `repair` welds, fixes orientation, fills
//! small holes and returns a readable report. Everything is deterministic (ordered maps only),
//! capped by iteration and triangle limits, and returns errors instead of panicking.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use crate::math::V3;
use crate::mesh::Mesh;
use crate::mesh_ops::clean;

type Edge = (u32, u32);
type EdgeMap = BTreeMap<Edge, Vec<usize>>;

/// Hard upper bound for remesh iterations regardless of the options.
pub const MAX_ITERATIONS: usize = 50;

#[derive(Clone, Debug, PartialEq)]
pub enum RepairError {
    /// Target edge length is not a positive finite number.
    BadTarget,
    /// A vertex coordinate is NaN or infinite.
    NonFinite,
    /// A triangle references a missing vertex.
    BadIndex { triangle: usize },
}

impl fmt::Display for RepairError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RepairError::BadTarget => write!(f, "target edge length must be positive and finite"),
            RepairError::NonFinite => write!(f, "mesh has non-finite coordinates"),
            RepairError::BadIndex { triangle } => {
                write!(f, "triangle {triangle} references a missing vertex")
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct RemeshOpts {
    pub target_edge: f64,
    pub iterations: usize,
    pub max_triangles: usize,
    /// Edges whose neighbouring faces differ by more than this angle are kept as they are.
    pub feature_angle_deg: f64,
    /// Weld tolerance applied before remeshing.
    pub tol: f64,
}

impl RemeshOpts {
    pub fn new(target_edge: f64) -> Self {
        Self {
            target_edge,
            iterations: 8,
            max_triangles: 200_000,
            feature_angle_deg: 30.0,
            tol: 1e-9,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemeshReport {
    pub iterations: usize,
    pub splits: usize,
    pub collapses: usize,
    pub flips: usize,
    /// The triangle cap stopped a refinement step.
    pub capped: bool,
    pub vertices: usize,
    pub triangles: usize,
    pub mean_edge: f64,
    pub min_edge: f64,
    pub max_edge: f64,
}

impl fmt::Display for RemeshReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "Remesh: {} iterations, {} splits, {} collapses, {} flips{}",
            self.iterations,
            self.splits,
            self.collapses,
            self.flips,
            if self.capped { " (triangle cap reached)" } else { "" }
        )?;
        writeln!(f, "Result: {} vertices, {} triangles", self.vertices, self.triangles)?;
        write!(
            f,
            "Edge length: mean {:.4}, min {:.4}, max {:.4}",
            self.mean_edge, self.min_edge, self.max_edge
        )
    }
}

#[derive(Clone, Debug)]
pub struct RepairOpts {
    pub tol: f64,
    pub fix_orientation: bool,
    pub fill_holes: bool,
    /// Holes with more boundary edges than this are left open.
    pub max_hole_edges: usize,
    pub max_triangles: usize,
}

impl Default for RepairOpts {
    fn default() -> Self {
        Self {
            tol: 1e-9,
            fix_orientation: true,
            fill_holes: true,
            max_hole_edges: 64,
            max_triangles: 500_000,
        }
    }
}

impl RepairOpts {
    /// Diagnose only: no orientation change, no hole filling.
    pub fn report_only() -> Self {
        Self {
            fix_orientation: false,
            fill_holes: false,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepairReport {
    pub vertices_in: usize,
    pub triangles_in: usize,
    pub merged_vertices: usize,
    pub degenerate_removed: usize,
    pub duplicate_removed: usize,
    /// Edges used by exactly one triangle (before filling).
    pub open_edges: usize,
    /// Edges used by more than two triangles.
    pub nonmanifold_edges: usize,
    /// Triangles whose winding disagreed with their neighbours.
    pub flipped_triangles: usize,
    /// Closed shells that pointed inwards and were inverted.
    pub inverted_components: usize,
    pub holes_found: usize,
    pub holes_filled: usize,
    pub holes_skipped: usize,
    pub triangles_added: usize,
    pub open_edges_after: usize,
    pub closed_after: bool,
    pub volume_after: f64,
    pub triangles_out: usize,
    /// Input exceeded the triangle cap and was returned unchanged.
    pub capped: bool,
}

impl fmt::Display for RepairReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Input: {} vertices, {} triangles", self.vertices_in, self.triangles_in)?;
        if self.capped {
            return write!(f, "Too many triangles: mesh left unchanged");
        }
        writeln!(
            f,
            "Cleanup: {} vertices merged, {} degenerate and {} duplicate triangles removed",
            self.merged_vertices, self.degenerate_removed, self.duplicate_removed
        )?;
        writeln!(
            f,
            "Problems: {} open edges, {} non-manifold edges, {} flipped triangles, {} inverted shells",
            self.open_edges, self.nonmanifold_edges, self.flipped_triangles, self.inverted_components
        )?;
        writeln!(
            f,
            "Holes: {} found, {} filled, {} skipped ({} triangles added)",
            self.holes_found, self.holes_filled, self.holes_skipped, self.triangles_added
        )?;
        write!(
            f,
            "Result: {} triangles, {} open edges left, {}, volume {:.6}",
            self.triangles_out,
            self.open_edges_after,
            if self.closed_after { "closed" } else { "not closed" },
            self.volume_after
        )
    }
}

impl RepairReport {
    pub fn to_text(&self) -> String {
        self.to_string()
    }
}

fn validate(m: &Mesh) -> Result<(), RepairError> {
    if m
        .positions
        .iter()
        .any(|p| !(p.x.is_finite() && p.y.is_finite() && p.z.is_finite()))
    {
        return Err(RepairError::NonFinite);
    }
    let n = m.positions.len() as u32;
    if let Some(i) = m.triangles.iter().position(|t| t.iter().any(|&v| v >= n)) {
        return Err(RepairError::BadIndex { triangle: i });
    }
    Ok(())
}

fn ek(a: u32, b: u32) -> Edge {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

fn build_edges(tris: &[[u32; 3]]) -> EdgeMap {
    let mut e = EdgeMap::new();
    for (i, t) in tris.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if a != b {
                e.entry(ek(a, b)).or_default().push(i);
            }
        }
    }
    e
}

fn nrm(pos: &[V3], t: &[u32; 3]) -> V3 {
    let [a, b, c] = t.map(|i| pos[i as usize]);
    (b - a).cross(c - a)
}

fn fo_of(m: &Mesh) -> Vec<u32> {
    if m.face_of.len() == m.triangles.len() {
        m.face_of.clone()
    } else {
        (0..m.triangles.len() as u32).collect()
    }
}

/// Feature edges (boundary, non-manifold, sharp) and the vertices that must not move.
fn features(
    pos: &[V3],
    tris: &[[u32; 3]],
    edges: &EdgeMap,
    cos_tol: f64,
) -> (BTreeSet<Edge>, Vec<bool>) {
    let mut feat = BTreeSet::new();
    let mut locked = vec![false; pos.len()];
    for (&(a, b), ts) in edges {
        let f = if ts.len() != 2 {
            true
        } else {
            let n1 = nrm(pos, &tris[ts[0]]);
            let n2 = nrm(pos, &tris[ts[1]]);
            let (l1, l2) = (n1.len(), n2.len());
            l1 < 1e-18 || l2 < 1e-18 || n1.dot(n2) < cos_tol * l1 * l2
        };
        if f {
            feat.insert((a, b));
            locked[a as usize] = true;
            locked[b as usize] = true;
        }
    }
    (feat, locked)
}

fn neighbours(edges: &EdgeMap, n: usize) -> Vec<Vec<u32>> {
    let mut nb = vec![Vec::new(); n];
    for &(a, b) in edges.keys() {
        nb[a as usize].push(b);
        nb[b as usize].push(a);
    }
    for l in &mut nb {
        l.sort_unstable();
        l.dedup();
    }
    nb
}

fn vert_tris(tris: &[[u32; 3]], n: usize) -> Vec<Vec<usize>> {
    let mut vt = vec![Vec::new(); n];
    for (i, t) in tris.iter().enumerate() {
        for &v in t {
            vt[v as usize].push(i);
        }
    }
    vt
}

fn compact(pos: &mut Vec<V3>, tris: &mut [[u32; 3]]) {
    let mut used = vec![false; pos.len()];
    for t in tris.iter() {
        for &v in t {
            used[v as usize] = true;
        }
    }
    let mut map = vec![u32::MAX; pos.len()];
    let mut out = Vec::with_capacity(pos.len());
    for (i, p) in pos.iter().enumerate() {
        if used[i] {
            map[i] = out.len() as u32;
            out.push(*p);
        }
    }
    for t in tris.iter_mut() {
        for v in t.iter_mut() {
            *v = map[*v as usize];
        }
    }
    *pos = out;
}

/// Splits every edge longer than `hi` at its midpoint. Returns (splits, capped).
fn split_pass(
    pos: &mut Vec<V3>,
    tris: &mut Vec<[u32; 3]>,
    fo: &mut Vec<u32>,
    hi: f64,
    max_tris: usize,
) -> (usize, bool) {
    let edges = build_edges(tris);
    let mut mids: BTreeMap<Edge, u32> = BTreeMap::new();
    let mut new_pts = Vec::new();
    for &(a, b) in edges.keys() {
        let (pa, pb) = (pos[a as usize], pos[b as usize]);
        if (pb - pa).len() > hi {
            mids.insert((a, b), (pos.len() + new_pts.len()) as u32);
            new_pts.push((pa + pb) * 0.5);
        }
    }
    if mids.is_empty() {
        return (0, false);
    }
    let mut added = 0usize;
    for t in tris.iter() {
        for k in 0..3 {
            if mids.contains_key(&ek(t[k], t[(k + 1) % 3])) {
                added += 1;
            }
        }
    }
    if tris.len() + added > max_tris {
        return (0, true);
    }
    pos.extend(new_pts);
    let mut nt = Vec::with_capacity(tris.len() + added);
    let mut nf = Vec::with_capacity(tris.len() + added);
    for (i, t) in tris.iter().enumerate() {
        let m: [Option<u32>; 3] =
            [0usize, 1, 2].map(|k| mids.get(&ek(t[k], t[(k + 1) % 3])).copied());
        let g = |k: usize| m[k].unwrap_or(0);
        let cnt = m.iter().filter(|x| x.is_some()).count();
        let mut out: Vec<[u32; 3]> = Vec::with_capacity(4);
        match cnt {
            0 => out.push(*t),
            1 => {
                let k = m.iter().position(|x| x.is_some()).unwrap_or(0);
                let (p0, p1, p2) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
                let mid = g(k);
                out.push([p0, mid, p2]);
                out.push([mid, p1, p2]);
            }
            2 => {
                let u = m.iter().position(|x| x.is_none()).unwrap_or(0);
                let (p0, p1, p2) = (t[u], t[(u + 1) % 3], t[(u + 2) % 3]);
                let (m1, m2) = (g((u + 1) % 3), g((u + 2) % 3));
                out.push([p0, p1, m1]);
                out.push([p0, m1, m2]);
                out.push([m1, p2, m2]);
            }
            _ => {
                let (a, b, c) = (t[0], t[1], t[2]);
                let (m0, m1, m2) = (g(0), g(1), g(2));
                out.push([a, m0, m2]);
                out.push([m0, b, m1]);
                out.push([m2, m1, c]);
                out.push([m0, m1, m2]);
            }
        }
        for o in out {
            nt.push(o);
            nf.push(fo[i]);
        }
    }
    *tris = nt;
    *fo = nf;
    (mids.len(), false)
}

/// Collapses interior edges shorter than `lo`; the removed vertex moves onto the kept one.
fn collapse_pass(
    pos: &[V3],
    tris: &mut Vec<[u32; 3]>,
    fo: &mut Vec<u32>,
    lo: f64,
    hi: f64,
    cos_tol: f64,
) -> usize {
    let edges = build_edges(tris);
    let (_, locked) = features(pos, tris, &edges, cos_tol);
    let nb = neighbours(&edges, pos.len());
    let vt = vert_tris(tris, pos.len());
    let mut alive = vec![true; tris.len()];
    let mut touched = vec![false; pos.len()];
    let mut done = 0usize;
    for (&(u, v), ts) in &edges {
        if ts.len() != 2 || (pos[v as usize] - pos[u as usize]).len() >= lo {
            continue;
        }
        let (a, b) = match (locked[u as usize], locked[v as usize]) {
            (true, true) => continue,
            (true, false) => (v, u),
            _ => (u, v),
        };
        let (au, bu) = (a as usize, b as usize);
        let free = |x: usize| !touched[x] && nb[x].iter().all(|&y| !touched[y as usize]);
        if !free(au) || !free(bu) {
            continue;
        }
        let opp = |t: usize| -> Option<u32> { tris[t].iter().copied().find(|&x| x != a && x != b) };
        let (c1, c2) = match (opp(ts[0]), opp(ts[1])) {
            (Some(x), Some(y)) if x != y => (x, y),
            _ => continue,
        };
        let common: Vec<u32> = nb[au]
            .iter()
            .copied()
            .filter(|x| nb[bu].binary_search(x).is_ok())
            .collect();
        if common.len() != 2 || !common.contains(&c1) || !common.contains(&c2) {
            continue;
        }
        let mut repl: Vec<(usize, [u32; 3])> = Vec::new();
        let mut ok = true;
        for &t in &vt[au] {
            if t == ts[0] || t == ts[1] {
                continue;
            }
            let old = tris[t];
            let new = old.map(|x| if x == a { b } else { x });
            let (n0, n1) = (nrm(pos, &old), nrm(pos, &new));
            if n1.len() < 1e-18 || n1.dot(n0) < 0.3 * n0.len() * n1.len() {
                ok = false;
                break;
            }
            if new
                .iter()
                .any(|&x| x != b && (pos[x as usize] - pos[bu]).len() > hi)
            {
                ok = false;
                break;
            }
            repl.push((t, new));
        }
        if !ok {
            continue;
        }
        alive[ts[0]] = false;
        alive[ts[1]] = false;
        for (t, new) in repl {
            tris[t] = new;
        }
        for x in [au, bu] {
            touched[x] = true;
            for &y in &nb[x] {
                touched[y as usize] = true;
            }
        }
        done += 1;
    }
    if done > 0 {
        let mut nt = Vec::with_capacity(tris.len());
        let mut nf = Vec::with_capacity(tris.len());
        for (i, t) in tris.iter().enumerate() {
            if alive[i] {
                nt.push(*t);
                nf.push(fo[i]);
            }
        }
        *tris = nt;
        *fo = nf;
    }
    done
}

/// Flips interior non-feature edges when it brings vertex valences closer to the ideal.
fn flip_pass(pos: &[V3], tris: &mut [[u32; 3]], cos_tol: f64) -> usize {
    let edges = build_edges(tris);
    let (feat, _) = features(pos, tris, &edges, cos_tol);
    let nb = neighbours(&edges, pos.len());
    let mut boundary = vec![false; pos.len()];
    for (&(a, b), ts) in &edges {
        if ts.len() == 1 {
            boundary[a as usize] = true;
            boundary[b as usize] = true;
        }
    }
    let ideal = |x: usize| -> i64 {
        if boundary[x] {
            4
        } else {
            6
        }
    };
    let mut touched = vec![false; pos.len()];
    let mut done = 0usize;
    for (&(u, v), ts) in &edges {
        if ts.len() != 2 || feat.contains(&(u, v)) {
            continue;
        }
        let (t1, t2) = (ts[0], ts[1]);
        let tr1 = tris[t1];
        let Some(k) = (0..3).find(|&k| ek(tr1[k], tr1[(k + 1) % 3]) == (u, v)) else {
            continue;
        };
        let (p, q, c1) = (tr1[k], tr1[(k + 1) % 3], tr1[(k + 2) % 3]);
        let tr2 = tris[t2];
        let has_qp = (0..3).any(|j| tr2[j] == q && tr2[(j + 1) % 3] == p);
        let Some(c2) = tr2.iter().copied().find(|&x| x != p && x != q) else {
            continue;
        };
        if !has_qp || c1 == c2 {
            continue;
        }
        let quad = [p as usize, q as usize, c1 as usize, c2 as usize];
        if quad.iter().any(|&x| touched[x]) {
            continue;
        }
        if nb[c1 as usize].binary_search(&c2).is_ok() {
            continue;
        }
        let val = |x: usize| nb[x].len() as i64;
        let before: i64 = quad.iter().map(|&x| (val(x) - ideal(x)).abs()).sum();
        let after = (val(quad[0]) - 1 - ideal(quad[0])).abs()
            + (val(quad[1]) - 1 - ideal(quad[1])).abs()
            + (val(quad[2]) + 1 - ideal(quad[2])).abs()
            + (val(quad[3]) + 1 - ideal(quad[3])).abs();
        if after >= before {
            continue;
        }
        let (n1, n2) = ([p, c2, c1], [q, c1, c2]);
        let r = nrm(pos, &tr1);
        let (a1, a2) = (nrm(pos, &n1), nrm(pos, &n2));
        if a1.len() < 1e-18
            || a2.len() < 1e-18
            || a1.dot(r) < 0.3 * a1.len() * r.len()
            || a2.dot(r) < 0.3 * a2.len() * r.len()
        {
            continue;
        }
        tris[t1] = n1;
        tris[t2] = n2;
        for x in quad {
            touched[x] = true;
        }
        done += 1;
    }
    done
}

/// One Jacobi step of tangential Laplacian smoothing on free vertices.
fn smooth_pass(pos: &mut [V3], tris: &[[u32; 3]], cos_tol: f64) {
    let edges = build_edges(tris);
    let (_, locked) = features(pos, tris, &edges, cos_tol);
    let nb = neighbours(&edges, pos.len());
    let mut vn = vec![V3::default(); pos.len()];
    for t in tris {
        let n = nrm(pos, t);
        for &v in t {
            vn[v as usize] = vn[v as usize] + n;
        }
    }
    let mut out = pos.to_vec();
    for i in 0..pos.len() {
        if locked[i] || nb[i].is_empty() {
            continue;
        }
        let mut c = V3::default();
        for &j in &nb[i] {
            c = c + pos[j as usize];
        }
        c = c * (1.0 / nb[i].len() as f64);
        let mut d = c - pos[i];
        let l = vn[i].len();
        if l > 1e-18 {
            let n = vn[i] * (1.0 / l);
            d = d - n * d.dot(n);
        }
        out[i] = pos[i] + d * 0.5;
    }
    pos.copy_from_slice(&out);
}

fn edge_stats(pos: &[V3], tris: &[[u32; 3]]) -> (f64, f64, f64) {
    let edges = build_edges(tris);
    if edges.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let (mut sum, mut mn, mut mx) = (0.0, f64::MAX, 0.0f64);
    for &(a, b) in edges.keys() {
        let l = (pos[b as usize] - pos[a as usize]).len();
        sum += l;
        mn = mn.min(l);
        mx = mx.max(l);
    }
    (sum / edges.len() as f64, mn, mx)
}

/// Isotropic remesh to `opts.target_edge`. Boundary and sharp-feature vertices never move.
pub fn remesh(m: &Mesh, opts: &RemeshOpts) -> Result<(Mesh, RemeshReport), RepairError> {
    if !(opts.target_edge.is_finite() && opts.target_edge > 0.0) {
        return Err(RepairError::BadTarget);
    }
    validate(m)?;
    let src = clean(m, opts.tol).0;
    let mut pos = src.positions.clone();
    let mut tris = src.triangles.clone();
    let mut fo = fo_of(&src);
    let (hi, lo) = (opts.target_edge * 4.0 / 3.0, opts.target_edge * 0.8);
    let cos_tol = opts.feature_angle_deg.to_radians().cos();
    let mut rep = RemeshReport::default();
    for it in 0..opts.iterations.min(MAX_ITERATIONS) {
        let (s, capped) = split_pass(&mut pos, &mut tris, &mut fo, hi, opts.max_triangles);
        rep.capped |= capped;
        let c = collapse_pass(&pos, &mut tris, &mut fo, lo, hi, cos_tol);
        let f = flip_pass(&pos, &mut tris, cos_tol);
        smooth_pass(&mut pos, &tris, cos_tol);
        compact(&mut pos, &mut tris);
        rep.iterations = it + 1;
        rep.splits += s;
        rep.collapses += c;
        rep.flips += f;
        if s == 0 && c == 0 && f == 0 {
            break;
        }
    }
    let (mean, mn, mx) = edge_stats(&pos, &tris);
    rep.vertices = pos.len();
    rep.triangles = tris.len();
    rep.mean_edge = mean;
    rep.min_edge = mn;
    rep.max_edge = mx;
    Ok((
        Mesh {
            positions: pos,
            triangles: tris,
            face_of: fo,
        },
        rep,
    ))
}

fn has_dir(t: &[u32; 3], a: u32, b: u32) -> bool {
    (0..3).any(|k| t[k] == a && t[(k + 1) % 3] == b)
}

/// Makes the winding consistent per connected shell; returns (flipped triangles, inverted shells).
fn orient(tris: &mut [[u32; 3]], pos: &[V3], apply: bool) -> (usize, usize) {
    let edges = build_edges(tris);
    let n = tris.len();
    let mut flip = vec![false; n];
    let mut seen = vec![false; n];
    let (mut flipped, mut inverted) = (0usize, 0usize);
    for seed in 0..n {
        if seen[seed] {
            continue;
        }
        seen[seed] = true;
        let mut comp = vec![seed];
        let mut q = VecDeque::new();
        q.push_back(seed);
        while let Some(t) = q.pop_front() {
            for k in 0..3 {
                let (a, b) = (tris[t][k], tris[t][(k + 1) % 3]);
                if a == b {
                    continue;
                }
                let key = ek(a, b);
                let Some(ts) = edges.get(&key) else { continue };
                if ts.len() != 2 {
                    continue;
                }
                let s = if ts[0] == t { ts[1] } else { ts[0] };
                if s == t || seen[s] {
                    continue;
                }
                let eff_t = (a < b) ^ flip[t];
                let dir_s = has_dir(&tris[s], key.0, key.1);
                seen[s] = true;
                flip[s] = dir_s == eff_t;
                comp.push(s);
                q.push_back(s);
            }
        }
        let mut f = comp.iter().filter(|&&t| flip[t]).count();
        if 2 * f > comp.len() {
            for &t in &comp {
                flip[t] = !flip[t];
            }
            f = comp.len() - f;
        }
        flipped += f;
        let closed = comp.iter().all(|&t| {
            (0..3).all(|k| {
                edges
                    .get(&ek(tris[t][k], tris[t][(k + 1) % 3]))
                    .is_some_and(|v| v.len() == 2)
            })
        });
        if closed {
            let vol: f64 = comp
                .iter()
                .map(|&t| {
                    let [a, b, c] = tris[t];
                    let (b, c) = if flip[t] { (c, b) } else { (b, c) };
                    pos[a as usize].dot(pos[b as usize].cross(pos[c as usize]))
                })
                .sum();
            if vol < 0.0 {
                for &t in &comp {
                    flip[t] = !flip[t];
                }
                inverted += 1;
            }
        }
    }
    if apply {
        for (t, tri) in tris.iter_mut().enumerate() {
            if flip[t] {
                tri.swap(1, 2);
            }
        }
    }
    (flipped, inverted)
}

/// Boundary loops as vertex rings in boundary-edge direction; `None` marks a loop that could
/// not be traced (branching or pinched boundary).
fn boundary_loops(edges: &EdgeMap, tris: &[[u32; 3]]) -> Vec<Option<Vec<u32>>> {
    let mut half = BTreeSet::new();
    let mut out: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (key, ts) in edges {
        if ts.len() != 1 {
            continue;
        }
        let t = &tris[ts[0]];
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if ek(a, b) == *key {
                half.insert((a, b));
                out.entry(a).or_default().push(b);
            }
        }
    }
    let mut used: BTreeSet<Edge> = BTreeSet::new();
    let mut loops = Vec::new();
    for &(a, b) in &half {
        if !used.insert((a, b)) {
            continue;
        }
        let mut path = vec![a];
        let mut cur = b;
        let mut ok = true;
        while cur != a {
            if path.len() > 1_000_000 {
                ok = false;
                break;
            }
            path.push(cur);
            match out.get(&cur) {
                Some(v) if v.len() == 1 => {
                    let nx = v[0];
                    if !used.insert((cur, nx)) {
                        ok = false;
                        break;
                    }
                    cur = nx;
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        loops.push(if ok { Some(path) } else { None });
    }
    loops
}

/// Weld, fix orientation, fill small holes. With `RepairOpts::report_only()` the mesh is only
/// cleaned and the report describes what a full repair would handle.
pub fn repair(m: &Mesh, opts: &RepairOpts) -> Result<(Mesh, RepairReport), RepairError> {
    validate(m)?;
    let mut rep = RepairReport {
        vertices_in: m.positions.len(),
        triangles_in: m.triangles.len(),
        ..Default::default()
    };
    if m.triangles.len() > opts.max_triangles {
        rep.capped = true;
        rep.triangles_out = m.triangles.len();
        return Ok((m.clone(), rep));
    }
    let (cm, cr) = clean(m, opts.tol);
    rep.merged_vertices = cr.merged_vertices;
    rep.degenerate_removed = cr.degenerate;
    rep.duplicate_removed = cr.duplicate;
    let mut pos = cm.positions.clone();
    let mut tris = cm.triangles.clone();
    let mut fo = fo_of(&cm);

    let edges = build_edges(&tris);
    rep.open_edges = edges.values().filter(|v| v.len() == 1).count();
    rep.nonmanifold_edges = edges.values().filter(|v| v.len() > 2).count();
    let (fl, inv) = orient(&mut tris, &pos, opts.fix_orientation);
    rep.flipped_triangles = fl;
    rep.inverted_components = inv;

    let edges = build_edges(&tris);
    let loops = boundary_loops(&edges, &tris);
    rep.holes_found = loops.len();
    let mut next_face = fo.iter().copied().max().map_or(0, |x| x.saturating_add(1));
    for l in &loops {
        let ring = match l {
            Some(r) if opts.fill_holes && r.len() >= 3 && r.len() <= opts.max_hole_edges => r,
            _ => {
                rep.holes_skipped += 1;
                continue;
            }
        };
        let add = if ring.len() == 3 { 1 } else { ring.len() };
        if tris.len() + add > opts.max_triangles {
            rep.holes_skipped += 1;
            continue;
        }
        if ring.len() == 3 {
            tris.push([ring[2], ring[1], ring[0]]);
            fo.push(next_face);
        } else {
            let mut c = V3::default();
            for &v in ring {
                c = c + pos[v as usize];
            }
            c = c * (1.0 / ring.len() as f64);
            let ci = pos.len() as u32;
            pos.push(c);
            for i in 0..ring.len() {
                let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
                tris.push([q, p, ci]);
                fo.push(next_face);
            }
        }
        next_face = next_face.saturating_add(1);
        rep.holes_filled += 1;
        rep.triangles_added += add;
    }

    let edges = build_edges(&tris);
    rep.open_edges_after = edges.values().filter(|v| v.len() == 1).count();
    let nm_after = edges.values().filter(|v| v.len() > 2).count();
    rep.closed_after = rep.open_edges_after == 0 && nm_after == 0 && !tris.is_empty();
    rep.triangles_out = tris.len();
    let out = Mesh {
        positions: pos,
        triangles: tris,
        face_of: fo,
    };
    rep.volume_after = out.volume();
    Ok((out, rep))
}

/// Report without changing the mesh.
pub fn analyze(m: &Mesh, tol: f64) -> Result<RepairReport, RepairError> {
    let opts = RepairOpts {
        tol,
        ..RepairOpts::report_only()
    };
    repair(m, &opts).map(|(_, r)| r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::v3;

    fn cube() -> Mesh {
        let p = [
            (0., 0., 0.),
            (1., 0., 0.),
            (1., 1., 0.),
            (0., 1., 0.),
            (0., 0., 1.),
            (1., 0., 1.),
            (1., 1., 1.),
            (0., 1., 1.),
        ];
        Mesh {
            positions: p.iter().map(|&(x, y, z)| v3(x, y, z)).collect(),
            triangles: vec![
                [0, 2, 1],
                [0, 3, 2],
                [4, 5, 6],
                [4, 6, 7],
                [0, 1, 5],
                [0, 5, 4],
                [3, 7, 6],
                [3, 6, 2],
                [0, 4, 7],
                [0, 7, 3],
                [1, 2, 6],
                [1, 6, 5],
            ],
            face_of: vec![],
        }
    }

    fn plate(side: f64) -> Mesh {
        Mesh {
            positions: vec![
                v3(0., 0., 0.),
                v3(side, 0., 0.),
                v3(side, side, 0.),
                v3(0., side, 0.),
            ],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
            face_of: vec![],
        }
    }

    #[test]
    fn remesh_edge_length_within_20pct() {
        let (m, rep) = remesh(&plate(4.0), &RemeshOpts::new(1.0)).unwrap();
        assert!(rep.mean_edge > 0.8 && rep.mean_edge < 1.2, "{rep}");
        assert!((m.area() - 16.0).abs() < 1e-6);
    }

    #[test]
    fn hole_fill_makes_closed() {
        let mut c = cube();
        c.triangles.remove(11);
        let (out, rep) = repair(&c, &RepairOpts::default()).unwrap();
        assert_eq!(rep.holes_found, 1);
        assert_eq!(rep.holes_filled, 1);
        assert!(rep.closed_after && out.is_watertight());
        assert!((out.volume() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn repair_report_counts() {
        let mut c = cube();
        c.triangles.remove(11);
        c.triangles[3].swap(1, 2);
        let r = analyze(&c, 1e-9).unwrap();
        assert_eq!(r.open_edges, 3);
        assert_eq!(r.nonmanifold_edges, 0);
        assert_eq!(r.flipped_triangles, 1);
        assert_eq!(r.holes_found, 1);
        assert!(!r.closed_after);
        assert!(r.to_text().contains("3 open edges"));
        let (_, fixed) = repair(&c, &RepairOpts::default()).unwrap();
        assert!(fixed.closed_after);
        assert!((fixed.volume_after - 1.0).abs() < 1e-6);
    }

    #[test]
    fn nonmanifold_input_reported() {
        let m = Mesh {
            positions: vec![
                v3(0., 0., 0.),
                v3(1., 0., 0.),
                v3(0., 1., 0.),
                v3(0., 0., 1.),
                v3(0., -1., 0.),
            ],
            triangles: vec![[0, 1, 2], [1, 0, 3], [0, 1, 4]],
            face_of: vec![],
        };
        let r = analyze(&m, 1e-9).unwrap();
        assert_eq!(r.nonmanifold_edges, 1);
        assert!(repair(&m, &RepairOpts::default()).is_ok());
        assert!(remesh(&m, &RemeshOpts::new(0.3)).is_ok());
    }

    #[test]
    fn bad_input_is_an_error() {
        let mut m = cube();
        m.triangles.push([0, 1, 99]);
        assert_eq!(
            repair(&m, &RepairOpts::default()).unwrap_err(),
            RepairError::BadIndex { triangle: 12 }
        );
        assert_eq!(
            remesh(&cube(), &RemeshOpts::new(0.0)).unwrap_err(),
            RepairError::BadTarget
        );
    }

    #[test]
    fn deterministic() {
        let a = remesh(&cube(), &RemeshOpts::new(0.3)).unwrap().0;
        let b = remesh(&cube(), &RemeshOpts::new(0.3)).unwrap().0;
        assert_eq!(a.triangles, b.triangles);
        assert_eq!(a.positions, b.positions);
    }

    #[test]
    fn closed_cube_volume_preserved() {
        let (m, rep) = remesh(&cube(), &RemeshOpts::new(0.3)).unwrap();
        assert!(rep.triangles > 12);
        assert!((m.volume() - 1.0).abs() < 1e-6);
        assert!(m.is_watertight());
        let (r, _) = repair(&m, &RepairOpts::default()).unwrap();
        assert!((r.volume() - 1.0).abs() < 1e-6);
    }
}
