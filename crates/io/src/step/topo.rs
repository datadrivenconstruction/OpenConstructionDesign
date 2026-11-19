//! Shells and faces to triangles. Each edge is sampled once and shared by both of its faces;
//! loops are mapped to surface parameters (periodic unwrapping, seams, poles), triangulated
//! with `triangulate_region` and, on curved surfaces, refined until chord and normal steps hold.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use ddc_geom::math::V2;
use ddc_geom::mesh_ops::triangulate_region;

use super::curve::{self, point, MAX_STEP};
use super::math::*;
use super::p21::{Store, P};
use super::surf::Surf;
use crate::job::Progress;

/// Refinement stops growing a face beyond this many triangles.
const MAX_TRI: usize = 200_000;

pub const SOLIDS: [&str; 4] = [
    "MANIFOLD_SOLID_BREP",
    "BREP_WITH_VOIDS",
    "FACETED_BREP",
    "SHELL_BASED_SURFACE_MODEL",
];

/// Triangles of one solid in file units; vertices shared between faces.
#[derive(Default, Debug)]
pub struct Mesh {
    pub pos: Vec<V>,
    pub tri: Vec<[u32; 3]>,
    weld: HashMap<[u64; 3], u32>,
    /// Face id and its triangle count.
    pub faces: Vec<(u64, usize)>,
}

impl Mesh {
    fn vid(&mut self, p: V) -> u32 {
        let p = p.map(|c| c + 0.0);
        let k = p.map(f64::to_bits);
        if let Some(&i) = self.weld.get(&k) {
            return i;
        }
        let i = self.pos.len() as u32;
        self.pos.push(p);
        self.weld.insert(k, i);
        i
    }
}

/// Shells of a solid or shell model with their orientation flip.
pub fn shells(st: &Store, id: u64) -> Vec<(u64, bool)> {
    let Some(kind) = st.kind(id) else { return vec![] };
    let a = st.rec(id, kind).unwrap_or(&[]);
    let one = || a.get(1).and_then(P::id).map(|s| (s, false));
    match kind {
        "MANIFOLD_SOLID_BREP" | "FACETED_BREP" => one().into_iter().collect(),
        "BREP_WITH_VOIDS" => {
            let mut v: Vec<(u64, bool)> = one().into_iter().collect();
            v.extend(a.get(2).map(P::ids).unwrap_or_default().into_iter().map(|s| (s, false)));
            v
        }
        "SHELL_BASED_SURFACE_MODEL" => a
            .get(1)
            .map(P::ids)
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s, false))
            .collect(),
        _ => vec![],
    }
}

/// Faces of a shell with their orientation flip.
pub fn faces(st: &Store, shell: u64, flip: bool, out: &mut Vec<(u64, bool)>, depth: u32) {
    let Some(kind) = st.kind(shell) else { return };
    let a = st.rec(shell, kind).unwrap_or(&[]);
    match kind {
        "CLOSED_SHELL" | "OPEN_SHELL" | "CONNECTED_FACE_SET" => {
            out.extend(a.get(1).map(P::ids).unwrap_or_default().into_iter().map(|f| (f, flip)))
        }
        "ORIENTED_CLOSED_SHELL" | "ORIENTED_OPEN_SHELL" if depth < 8 => {
            if let Some(e) = a.get(2).and_then(P::id) {
                let o = a.get(3).and_then(P::flag).unwrap_or(true);
                faces(st, e, flip ^ !o, out, depth + 1);
            }
        }
        _ => {}
    }
}

pub fn face_count(st: &Store, solid: u64) -> usize {
    let mut fs = vec![];
    for (s, f) in shells(st, solid) {
        faces(st, s, f, &mut fs, 0);
    }
    fs.len()
}

/// One bound of a face in 3D: points with the index of the edge (block) each one starts.
struct Lp {
    p: Vec<V>,
    tag: Vec<usize>,
    /// Edge id per block; 0 for polygon loops.
    blocks: Vec<u64>,
    outer: bool,
}

struct UvLoop {
    q: Vec<[f64; 2]>,
    p: Vec<V>,
    /// Periods the loop advances per turn, per parameter direction.
    wrap: [i64; 2],
    outer: bool,
}

type Ring = Vec<([f64; 2], V)>;

pub struct Tess<'a> {
    st: &'a Store,
    tol: f64,
    ang: f64,
    edges: HashMap<u64, Rc<Vec<V>>>,
    /// Skipped faces by reason.
    pub skipped: BTreeMap<String, usize>,
    progress: Option<&'a Progress>,
    done: u64,
    pub cancelled: bool,
}

impl<'a> Tess<'a> {
    pub fn new(st: &'a Store, progress: Option<&'a Progress>) -> Self {
        Tess {
            st,
            tol: 0.0,
            ang: 1.0,
            edges: HashMap::new(),
            skipped: BTreeMap::new(),
            progress,
            done: 0,
            cancelled: false,
        }
    }

    /// `tol`: chord tolerance in file units; `ang`: radians per file angle unit.
    pub fn set_units(&mut self, tol: f64, ang: f64) {
        if tol != self.tol || ang != self.ang {
            self.edges.clear();
            self.tol = tol;
            self.ang = ang;
        }
    }

    pub fn solid(&mut self, id: u64) -> Mesh {
        let mut m = Mesh::default();
        let mut fs = vec![];
        for (s, f) in shells(self.st, id) {
            faces(self.st, s, f, &mut fs, 0);
        }
        for (f, flip) in fs {
            if self.cancelled {
                break;
            }
            if let Err(e) = self.face(f, flip, &mut m) {
                // "#12: bad radius" and "#13: bad radius" are one reason
                let reason = match e.split_once(": ") {
                    Some((head, tail)) if head.starts_with('#') => tail.to_string(),
                    _ => e,
                };
                *self.skipped.entry(reason).or_default() += 1;
            }
            self.done += 1;
            if let Some(p) = self.progress {
                if self.done % 16 == 0 {
                    p.set(self.done);
                    if p.is_cancelled() {
                        self.cancelled = true;
                    }
                }
            }
        }
        m
    }

    fn face(&mut self, id: u64, flip: bool, m: &mut Mesh) -> Result<(), String> {
        let st = self.st;
        let (mut id, mut flip) = (id, flip);
        for _ in 0..4 {
            let Some(a) = st.rec(id, "ORIENTED_FACE") else { break };
            if a.get(3).and_then(P::flag) == Some(false) {
                flip = !flip;
            }
            id = a
                .get(2)
                .and_then(P::id)
                .ok_or_else(|| format!("#{id}: oriented face without a face"))?;
        }
        let (bounds, geom, sense) =
            if let Some(a) = st.rec(id, "ADVANCED_FACE").or_else(|| st.rec(id, "FACE_SURFACE")) {
                (
                    a.get(1).map(P::ids).unwrap_or_default(),
                    a.get(2).and_then(P::id),
                    a.get(3).and_then(P::flag).unwrap_or(true),
                )
            } else if let Some(a) = st.rec(id, "FACE") {
                (a.get(1).map(P::ids).unwrap_or_default(), None, true)
            } else {
                return Err(format!("unsupported face {}", st.describe(id)));
            };
        let mut loops = vec![];
        for b in bounds {
            if let Some(l) = self.bound(b)? {
                loops.push(l);
            }
        }
        let surf = match geom {
            Some(g) => Surf::read(st, g, self.ang, self.tol)?,
            None => newell(&loops, self.tol)?,
        };
        let uv: Vec<UvLoop> = loops.iter().map(|l| to_uv(&surf, l)).collect();
        let (outer, holes) = domain(&surf, uv, sense, self.tol)?;
        self.mesh(&surf, outer, holes, flip ^ !sense, m, id)
    }

    fn bound(&mut self, b: u64) -> Result<Option<Lp>, String> {
        let st = self.st;
        let (outer, a) = if let Some(a) = st.rec(b, "FACE_OUTER_BOUND") {
            (true, a)
        } else if let Some(a) = st.rec(b, "FACE_BOUND") {
            (false, a)
        } else {
            return Err(format!("unsupported face bound {}", st.describe(b)));
        };
        let lid = a
            .get(1)
            .and_then(P::id)
            .ok_or_else(|| format!("#{b}: face bound without a loop"))?;
        let ori = a.get(2).and_then(P::flag).unwrap_or(true);
        let mut lp = Lp {
            p: vec![],
            tag: vec![],
            blocks: vec![],
            outer,
        };
        if st.is(lid, "VERTEX_LOOP") {
            return Ok(None);
        }
        if let Some(la) = st.rec(lid, "POLY_LOOP") {
            let mut pts = la
                .get(1)
                .map(P::ids)
                .unwrap_or_default()
                .into_iter()
                .map(|i| point(st, i))
                .collect::<Result<Vec<V>, String>>()?;
            if !ori {
                pts.reverse();
            }
            lp.tag = vec![0; pts.len()];
            lp.blocks.push(0);
            lp.p = pts;
        } else if let Some(la) = st.rec(lid, "EDGE_LOOP") {
            let mut oes: Vec<(u64, bool)> = vec![];
            for oe in la.get(1).map(P::ids).unwrap_or_default() {
                match st.rec(oe, "ORIENTED_EDGE") {
                    Some(o) => oes.push((
                        o.get(3)
                            .and_then(P::id)
                            .ok_or_else(|| format!("#{oe}: oriented edge without an edge"))?,
                        o.get(4).and_then(P::flag).unwrap_or(true),
                    )),
                    None => oes.push((oe, true)),
                }
            }
            if !ori {
                oes.reverse();
                for x in &mut oes {
                    x.1 = !x.1;
                }
            }
            for (k, (e, fwd)) in oes.into_iter().enumerate() {
                let s = self.edge(e, 0)?;
                let n = s.len() - 1;
                if fwd {
                    lp.p.extend(s.iter().take(n));
                } else {
                    lp.p.extend(s.iter().rev().take(n));
                }
                lp.tag.resize(lp.p.len(), k);
                lp.blocks.push(e);
            }
        } else {
            return Err(format!("unsupported loop {}", st.describe(lid)));
        }
        let (mut p2, mut t2): (Vec<V>, Vec<usize>) = (vec![], vec![]);
        for (p, t) in lp.p.iter().zip(&lp.tag) {
            if p2.last() != Some(p) {
                p2.push(*p);
                t2.push(*t);
            }
        }
        while p2.len() > 1 && p2.first() == p2.last() {
            p2.pop();
            t2.pop();
        }
        if p2.len() < 3 {
            return Ok(None);
        }
        lp.p = p2;
        lp.tag = t2;
        Ok(Some(lp))
    }

    /// Samples of an edge from its start vertex to its end vertex.
    fn edge(&mut self, id: u64, depth: u32) -> Result<Rc<Vec<V>>, String> {
        if let Some(s) = self.edges.get(&id) {
            return Ok(s.clone());
        }
        let st = self.st;
        let vertex = |v: Option<&P>| -> Result<V, String> {
            let v = v
                .and_then(P::id)
                .ok_or_else(|| format!("#{id}: edge without a vertex"))?;
            let a = st
                .rec(v, "VERTEX_POINT")
                .ok_or_else(|| format!("#{v} is {}, not a VERTEX_POINT", st.describe(v)))?;
            point(st, a.get(1).and_then(P::id).unwrap_or(0))
        };
        let s = if let Some(a) = st.rec(id, "EDGE_CURVE") {
            let (va, vb) = (vertex(a.get(1))?, vertex(a.get(2))?);
            let c = a
                .get(3)
                .and_then(P::id)
                .ok_or_else(|| format!("#{id}: edge without a curve"))?;
            let c = curve::read(st, c, 0)?;
            curve::sample(&c, va, vb, a.get(4).and_then(P::flag).unwrap_or(true), self.tol)
        } else if let Some(a) = st.rec(id, "ORIENTED_EDGE") {
            let e = a.get(3).and_then(P::id).filter(|_| depth < 4);
            let e = e.ok_or_else(|| format!("#{id}: bad oriented edge"))?;
            let mut v = (*self.edge(e, depth + 1)?).clone();
            if a.get(4).and_then(P::flag) == Some(false) {
                v.reverse();
            }
            v
        } else if let Some(a) = st.rec(id, "EDGE") {
            vec![vertex(a.get(1))?, vertex(a.get(2))?]
        } else {
            return Err(format!("unsupported edge {}", st.describe(id)));
        };
        let s = Rc::new(s);
        self.edges.insert(id, s.clone());
        Ok(s)
    }

    #[allow(clippy::too_many_arguments)]
    fn mesh(&self, s: &Surf, outer: Ring, holes: Vec<Ring>, flip: bool, m: &mut Mesh, face: u64) -> Result<(), String> {
        let tol = self.tol;
        let all: Vec<&Ring> = std::iter::once(&outer).chain(holes.iter()).collect();
        let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
        for r in &all {
            for (x, _) in r.iter() {
                for d in 0..2 {
                    lo[d] = lo[d].min(x[d]);
                    hi[d] = hi[d].max(x[d]);
                }
            }
        }
        let span = [hi[0] - lo[0], hi[1] - lo[1]];
        if !(span[0] > 0.0 && span[1] > 0.0 && span[0].is_finite() && span[1].is_finite()) {
            return Err("degenerate face".into());
        }
        let same = |a: [f64; 2], b: [f64; 2]| {
            (a[0] - b[0]).abs() <= 1e-12 * span[0] && (a[1] - b[1]).abs() <= 1e-12 * span[1]
        };
        let mut q: Vec<[f64; 2]> = vec![];
        let mut p: Vec<V> = vec![];
        let mut rings: Vec<Vec<usize>> = vec![];
        for (ri, r) in all.iter().enumerate() {
            let mut idx: Vec<usize> = vec![];
            for &(x, y) in r.iter() {
                if idx.last().is_some_and(|&l| same(q[l], x)) {
                    continue;
                }
                q.push(x);
                p.push(y);
                idx.push(q.len() - 1);
            }
            while idx.len() > 1 && same(q[idx[0]], q[idx[idx.len() - 1]]) {
                idx.pop();
            }
            if idx.len() < 3 {
                if ri == 0 {
                    return Err("degenerate face".into());
                }
                continue;
            }
            rings.push(idx);
        }
        let key = |a: usize, b: usize| if a < b { (a, b) } else { (b, a) };
        let mut bnd: HashSet<(usize, usize)> = HashSet::new();
        for r in &rings {
            for k in 0..r.len() {
                bnd.insert(key(r[k], r[(k + 1) % r.len()]));
            }
        }
        // points in a straight run are left out of the ear clipping and fanned back in after:
        // a zero-area ear in parameter space can be a large triangle in space
        let mut simp: Vec<Vec<usize>> = vec![];
        let mut inter: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
        for (ri, r) in rings.iter().enumerate() {
            let n = r.len();
            let col = |a: usize, b: usize, c: usize| collinear(q[a], q[b], q[c]);
            let Some(s0) = (0..n).find(|&i| !col(r[(i + n - 1) % n], r[i], r[(i + 1) % n])) else {
                if ri == 0 {
                    return Err("degenerate face".into());
                }
                continue;
            };
            let rr: Vec<usize> = (0..n).map(|k| r[(s0 + k) % n]).collect();
            let mut kept = vec![rr[0]];
            let mut pend = vec![];
            for k in 1..n {
                let (b, c) = (rr[k], rr[(k + 1) % n]);
                let a = kept[kept.len() - 1];
                if col(a, b, c) {
                    pend.push(b);
                } else {
                    if !pend.is_empty() {
                        inter.insert((a, b), std::mem::take(&mut pend));
                    }
                    kept.push(b);
                }
            }
            if !pend.is_empty() {
                inter.insert((kept[kept.len() - 1], rr[0]), pend);
            }
            if kept.len() < 3 {
                if ri == 0 {
                    return Err("degenerate face".into());
                }
                continue;
            }
            simp.push(kept);
        }
        let v2 = |i: usize| V2 {
            x: (q[i][0] - lo[0]) / span[0],
            y: (q[i][1] - lo[1]) / span[1],
        };
        let outer2: Vec<V2> = simp[0].iter().map(|&i| v2(i)).collect();
        let holes2: Vec<Vec<V2>> = simp[1..]
            .iter()
            .map(|r| r.iter().map(|&i| v2(i)).collect())
            .collect();
        let tris = triangulate_region(&outer2, &holes2).ok_or_else(|| "triangulation failed".to_string())?;
        let flat: Vec<usize> = simp.concat();
        let mut work: Vec<[usize; 3]> = tris.into_iter().map(|t| t.map(|k| flat[k])).collect();
        let mut tri: Vec<[usize; 3]> = Vec::with_capacity(work.len());
        while let Some(t) = work.pop() {
            let mut split = None;
            for k in 0..3 {
                let (a, b, c) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
                let ms = inter
                    .get(&(a, b))
                    .cloned()
                    .or_else(|| inter.get(&(b, a)).map(|v| v.iter().rev().copied().collect()));
                if let Some(ms) = ms {
                    split = Some((a, b, c, ms));
                    break;
                }
            }
            match split {
                None => tri.push(t),
                Some((a, b, c, ms)) => {
                    let mut chain = vec![a];
                    chain.extend(ms);
                    chain.push(b);
                    for w in chain.windows(2) {
                        work.push([w[0], w[1], c]);
                    }
                }
            }
        }
        if s.curved() {
            refine(s, &mut q, &mut p, &mut tri, &bnd, tol);
        }
        let mut n = 0;
        let mut map = vec![u32::MAX; q.len()];
        for t in tri {
            let t = if flip { [t[0], t[2], t[1]] } else { t };
            let g = t.map(|i| {
                if map[i] == u32::MAX {
                    map[i] = m.vid(p[i]);
                }
                map[i]
            });
            if g[0] != g[1] && g[1] != g[2] && g[0] != g[2] {
                m.tri.push(g);
                n += 1;
            }
        }
        m.faces.push((face, n));
        Ok(())
    }
}

fn collinear(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    let u = [b[0] - a[0], b[1] - a[1]];
    let w = [c[0] - b[0], c[1] - b[1]];
    let (lu, lw) = (u[0].hypot(u[1]), w[0].hypot(w[1]));
    let cr = u[0] * w[1] - u[1] * w[0];
    lu > 0.0 && lw > 0.0 && cr.abs() <= 1e-9 * lu * lw && u[0] * w[0] + u[1] * w[1] > 0.0
}

fn mid2(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1])]
}

/// Splits interior edges whose midpoint leaves the chord by more than `tol` or whose end
/// normals turn by more than [`MAX_STEP`]; boundary edges stay as sampled.
fn refine(s: &Surf, q: &mut Vec<[f64; 2]>, p: &mut Vec<V>, tri: &mut Vec<[usize; 3]>, bnd: &HashSet<(usize, usize)>, tol: f64) {
    let key = |a: usize, b: usize| if a < b { (a, b) } else { (b, a) };
    let mut nrm: Vec<Option<V>> = q.iter().map(|&x| s.normal(x)).collect();
    let cos_step = MAX_STEP.cos();
    for _ in 0..12 {
        if tri.len() > MAX_TRI {
            break;
        }
        let mut test: HashMap<(usize, usize), bool> = HashMap::new();
        let mut mark: HashMap<(usize, usize), usize> = HashMap::new();
        for t in tri.iter() {
            for k in 0..3 {
                let e = key(t[k], t[(k + 1) % 3]);
                if bnd.contains(&e) {
                    continue;
                }
                let need = *test.entry(e).or_insert_with(|| {
                    let (a, b) = e;
                    let dev = dist(s.eval(mid2(q[a], q[b])), lerp(p[a], p[b], 0.5));
                    let turn = match (nrm[a], nrm[b]) {
                        (Some(x), Some(y)) => dot(x, y) < cos_step && dist(p[a], p[b]) > tol,
                        _ => false,
                    };
                    dev > tol || turn
                });
                if need {
                    mark.insert(e, usize::MAX);
                }
            }
            let c = [0, 1].map(|d| (q[t[0]][d] + q[t[1]][d] + q[t[2]][d]) / 3.0);
            let pc = [0, 1, 2].map(|d| (p[t[0]][d] + p[t[1]][d] + p[t[2]][d]) / 3.0);
            if dist(s.eval(c), pc) > tol {
                let longest = (0..3)
                    .map(|k| key(t[k], t[(k + 1) % 3]))
                    .filter(|e| !bnd.contains(e))
                    .max_by(|x, y| dist(p[x.0], p[x.1]).total_cmp(&dist(p[y.0], p[y.1])));
                if let Some(e) = longest {
                    mark.insert(e, usize::MAX);
                }
            }
        }
        if mark.is_empty() {
            break;
        }
        let mut keys: Vec<(usize, usize)> = mark.keys().copied().collect();
        keys.sort_unstable();
        for e in keys {
            let mq = mid2(q[e.0], q[e.1]);
            q.push(mq);
            p.push(s.eval(mq));
            nrm.push(s.normal(mq));
            mark.insert(e, q.len() - 1);
        }
        let mut next = Vec::with_capacity(tri.len() * 2);
        for t in tri.iter() {
            let ms: [Option<usize>; 3] = [0, 1, 2].map(|k| mark.get(&key(t[k], t[(k + 1) % 3])).copied());
            match ms {
                [None, None, None] => next.push(*t),
                [Some(a), Some(b), Some(c)] => {
                    next.extend([[t[0], a, c], [a, t[1], b], [c, b, t[2]], [a, b, c]]);
                }
                _ => {
                    let Some(k0) = (0..3).find(|&k| ms[k].is_some()) else { continue };
                    let mut poly = vec![ms[k0].unwrap_or(t[k0])];
                    for j in 1..=3 {
                        let k = (k0 + j) % 3;
                        poly.push(t[k]);
                        if j < 3 {
                            if let Some(mi) = ms[k] {
                                poly.push(mi);
                            }
                        }
                    }
                    for i in 1..poly.len() - 1 {
                        next.push([poly[0], poly[i], poly[i + 1]]);
                    }
                }
            }
        }
        *tri = next;
    }
}

fn near(x: f64, prev: f64, per: f64) -> f64 {
    x + per * ((prev - x) / per).round()
}

fn area(q: &[[f64; 2]]) -> f64 {
    let n = q.len();
    (0..n)
        .map(|i| {
            let (a, b) = (q[i], q[(i + 1) % n]);
            a[0] * b[1] - a[1] * b[0]
        })
        .sum::<f64>()
        * 0.5
}

fn newell(loops: &[Lp], tol: f64) -> Result<Surf, String> {
    let l = loops
        .iter()
        .find(|l| l.outer)
        .or(loops.first())
        .ok_or_else(|| "face without loops".to_string())?;
    let o = l.p[0];
    let n = l.p.len();
    let mut nz = [0.0; 3];
    for i in 0..n {
        nz = add(nz, cross(sub(l.p[i], o), sub(l.p[(i + 1) % n], o)));
    }
    let z = unit(nz).ok_or_else(|| "degenerate planar face".to_string())?;
    Ok(Surf::plane(Ax::new(o, Some(z), Some(sub(l.p[1], o))), tol))
}

/// Parameters of a loop: continuous across periodic seams, a seam edge's second traversal
/// moved one period away from its first, a pole point split into its two neighbours' params.
fn to_uv(s: &Surf, l: &Lp) -> UvLoop {
    let n = l.p.len();
    let mut q = Vec::with_capacity(n);
    let mut sg = Vec::with_capacity(n);
    let mut hint = None;
    for &p in &l.p {
        let (x, f) = s.project(p, hint);
        if !f[0] && !f[1] {
            hint = Some(x);
        }
        q.push(x);
        sg.push(f);
    }
    for d in 0..2 {
        let Some(per) = s.per[d] else { continue };
        let mut prev: Option<f64> = None;
        for i in 0..n {
            if sg[i][d] {
                continue;
            }
            if let Some(pv) = prev {
                q[i][d] = near(q[i][d], pv, per);
            }
            prev = Some(q[i][d]);
        }
    }
    let mut ranges: Vec<(usize, usize)> = vec![(n, 0); l.blocks.len()];
    for (i, &t) in l.tag.iter().enumerate() {
        let r = &mut ranges[t];
        r.0 = r.0.min(i);
        r.1 = r.1.max(i + 1);
    }
    let stats = |q: &[[f64; 2]], r: (usize, usize), d: usize| -> Option<(f64, f64)> {
        let v: Vec<f64> = (r.0..r.1).filter(|&i| !sg[i][d]).map(|i| q[i][d]).collect();
        if v.is_empty() {
            return None;
        }
        let (lo, hi) = v.iter().fold((f64::MAX, f64::MIN), |a, &x| (a.0.min(x), a.1.max(x)));
        Some((v.iter().sum::<f64>() / v.len() as f64, hi - lo))
    };
    let mut first: HashMap<u64, usize> = HashMap::new();
    for (bi, &e) in l.blocks.iter().enumerate() {
        if e == 0 || ranges[bi].0 >= ranges[bi].1 {
            continue;
        }
        let Some(&bj) = first.get(&e) else {
            first.insert(e, bi);
            continue;
        };
        let mut pick: Option<(usize, f64, f64)> = None;
        for d in 0..2 {
            let Some(per) = s.per[d] else { continue };
            let (Some((m1, x1)), Some((m2, _))) = (stats(&q, ranges[bj], d), stats(&q, ranges[bi], d)) else {
                continue;
            };
            if (m1 - m2).abs() < per / 4.0 && x1 < per / 8.0 && pick.map_or(true, |p| x1 / per < p.1) {
                pick = Some((d, x1 / per, m2));
            }
        }
        if let Some((d, _, m2)) = pick {
            let per = s.per[d].unwrap_or(0.0);
            let start = (0..n).find(|&i| !sg[i][d]).map_or(m2, |i| q[i][d]);
            let sh = if start < m2 - per / 4.0 { -per } else { per };
            for x in q.iter_mut().skip(ranges[bi].0) {
                x[d] += sh;
            }
        }
    }
    let mut oq = Vec::with_capacity(n + 4);
    let mut op = Vec::with_capacity(n + 4);
    for i in 0..n {
        if !sg[i][0] && !sg[i][1] {
            oq.push(q[i]);
            op.push(l.p[i]);
            continue;
        }
        let (mut a, mut b) = (q[i], q[i]);
        for d in 0..2 {
            if !sg[i][d] {
                continue;
            }
            if let Some(j) = (1..n).map(|k| (i + n - k) % n).find(|&j| !sg[j][d]) {
                a[d] = q[j][d];
            }
            if let Some(j) = (1..n).map(|k| (i + k) % n).find(|&j| !sg[j][d]) {
                b[d] = q[j][d];
            }
        }
        oq.push(a);
        op.push(l.p[i]);
        if b != a {
            oq.push(b);
            op.push(l.p[i]);
        }
    }
    let mut wrap = [0i64; 2];
    for d in 0..2 {
        if let (Some(per), Some(f), Some(e)) = (s.per[d], oq.first(), oq.last()) {
            wrap[d] = -((f[d] - e[d]) / per).round() as i64;
        }
    }
    UvLoop {
        q: oq,
        p: op,
        wrap,
        outer: l.outer,
    }
}

fn at(d: usize, xd: f64, xe: f64) -> [f64; 2] {
    let mut q = [0.0; 2];
    q[d] = xd;
    q[1 - d] = xe;
    q
}

fn increasing(l: UvLoop, d: usize) -> (Vec<[f64; 2]>, Vec<V>) {
    let (mut q, mut p) = (l.q, l.p);
    if l.wrap[d] < 0 {
        q.reverse();
        p.reverse();
    }
    (q, p)
}

/// Outer ring and holes in parameter space.
fn domain(s: &Surf, mut ls: Vec<UvLoop>, sense: bool, tol: f64) -> Result<(Ring, Vec<Ring>), String> {
    let layout = || "unsupported loop layout on a closed surface".to_string();
    if ls.is_empty() {
        let r = s.full().ok_or_else(|| "face without edge loops".to_string())?;
        return Ok((rect(s, r, tol), vec![]));
    }
    let wr: Vec<usize> = (0..ls.len()).filter(|&i| ls[i].wrap != [0, 0]).collect();
    let outer: Ring = match wr.len() {
        0 => {
            let k = ls.iter().position(|l| l.outer).unwrap_or_else(|| {
                (0..ls.len())
                    .max_by(|&a, &b| area(&ls[a].q).abs().total_cmp(&area(&ls[b].q).abs()))
                    .unwrap_or(0)
            });
            let l = ls.remove(k);
            l.q.into_iter().zip(l.p).collect()
        }
        1 | 2 => {
            let d = if ls[wr[0]].wrap[0] != 0 { 0 } else { 1 };
            if wr.iter().any(|&i| ls[i].wrap[1 - d] != 0 || ls[i].wrap[d].abs() != 1) {
                return Err(layout());
            }
            if wr.len() == 2 {
                let b = ls.remove(wr[1]);
                let a = ls.remove(wr[0]);
                band(s, a, b, d, tol)
            } else {
                let a = ls.remove(wr[0]);
                cap(s, a, d, sense, tol).ok_or_else(layout)?
            }
        }
        _ => return Err(layout()),
    };
    let mut holes = vec![];
    for l in ls {
        let mut q = l.q;
        for d in 0..2 {
            let Some(per) = s.per[d] else { continue };
            let omin = outer.iter().map(|x| x.0[d]).fold(f64::MAX, f64::min);
            let m = q.iter().map(|x| x[d]).sum::<f64>() / q.len() as f64;
            let k = ((omin - m) / per).ceil();
            if k != 0.0 {
                for x in &mut q {
                    x[d] += k * per;
                }
            }
        }
        holes.push(q.into_iter().zip(l.p).collect());
    }
    Ok((outer, holes))
}

/// A loop that goes once around a closed direction and ends at a pole (sphere cap, cone).
fn cap(s: &Surf, l: UvLoop, d: usize, sense: bool, tol: f64) -> Option<Ring> {
    let e = 1 - d;
    let per = s.per[d]?;
    let inc = l.wrap[d] > 0;
    // the face lies left of its bound when the surface normal is the face normal
    let up = if d == 0 { inc == sense } else { inc != sense };
    let from = l.q.iter().map(|x| x[e]).sum::<f64>() / l.q.len() as f64;
    let pole = s.pole(e, if up { 1 } else { -1 }, from)?;
    let (q, p) = increasing(l, d);
    let (a0, p0) = (q[0], p[0]);
    let mut r: Ring = q.into_iter().zip(p).collect();
    let end = at(d, a0[d] + per, a0[e]);
    let (pe, ps) = (at(d, a0[d] + per, pole), at(d, a0[d], pole));
    let pp = s.eval(ps);
    r.push((end, p0));
    r.extend(cut(s, end, pe, tol));
    r.push((pe, pp));
    r.push((ps, pp));
    r.extend(cut(s, ps, a0, tol));
    Some(r)
}

/// Two loops that each go once around a closed direction (a cylinder between two circles).
fn band(s: &Surf, a: UvLoop, b: UvLoop, d: usize, tol: f64) -> Ring {
    let per = s.per[d].unwrap_or(std::f64::consts::TAU);
    let (qa, pa) = increasing(a, d);
    let (qb, pb) = increasing(b, d);
    let a0 = qa[0];
    let off = |k: usize| (near(qb[k][d], a0[d], per) - a0[d]).abs();
    let j = (0..qb.len()).min_by(|&x, &y| off(x).total_cmp(&off(y))).unwrap_or(0);
    let sh = near(qb[j][d], a0[d], per) - qb[j][d];
    let n = qb.len();
    let bq: Vec<[f64; 2]> = (0..n)
        .map(|k| {
            let i = (j + k) % n;
            let mut x = qb[i];
            x[d] += sh + if i < j { per } else { 0.0 };
            x
        })
        .collect();
    let bp: Vec<V> = (0..n).map(|k| pb[(j + k) % n]).collect();
    let b0 = bq[0];
    let mut a_end = a0;
    a_end[d] += per;
    let mut b_end = b0;
    b_end[d] += per;
    let mut r: Ring = qa.iter().copied().zip(pa.iter().copied()).collect();
    r.push((a_end, pa[0]));
    r.extend(cut(s, a_end, b_end, tol));
    r.push((b_end, bp[0]));
    for k in (1..n).rev() {
        r.push((bq[k], bp[k]));
    }
    r.push((b0, bp[0]));
    r.extend(cut(s, b0, a0, tol));
    r
}

/// The whole parameter rectangle (a sphere or torus face without edges).
fn rect(s: &Surf, r: [[f64; 2]; 2], tol: f64) -> Ring {
    let c = [
        [r[0][0], r[1][0]],
        [r[0][1], r[1][0]],
        [r[0][1], r[1][1]],
        [r[0][0], r[1][1]],
    ];
    let mut out = vec![];
    for k in 0..4 {
        out.push((c[k], s.eval(c[k])));
        out.extend(cut(s, c[k], c[(k + 1) % 4], tol));
    }
    out
}

/// Interior points of a straight parameter segment that keep its image within `tol`.
fn cut(s: &Surf, a: [f64; 2], b: [f64; 2], tol: f64) -> Vec<([f64; 2], V)> {
    let mut out = vec![];
    cut_rec(s, a, s.eval(a), b, s.eval(b), tol, 0, &mut out);
    out
}

#[allow(clippy::too_many_arguments)]
fn cut_rec(s: &Surf, a: [f64; 2], pa: V, b: [f64; 2], pb: V, tol: f64, depth: u32, out: &mut Ring) {
    if depth >= 12 {
        return;
    }
    let lq = |t: f64| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
    let dev = [0.25, 0.5, 0.75]
        .iter()
        .map(|&t| dist(s.eval(lq(t)), lerp(pa, pb, t)))
        .fold(0.0, f64::max);
    let turn = match (s.normal(a), s.normal(b)) {
        (Some(x), Some(y)) => dot(x, y) < MAX_STEP.cos() && dist(pa, pb) > tol,
        _ => false,
    };
    if dev > tol || turn {
        let m = lq(0.5);
        let pm = s.eval(m);
        cut_rec(s, a, pa, m, pm, tol, depth + 1, out);
        out.push((m, pm));
        cut_rec(s, m, pm, b, pb, tol, depth + 1, out);
    }
}
