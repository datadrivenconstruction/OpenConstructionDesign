//! Rebuild: nodes in timeline order up to the roll-back marker. A node's key hashes its op
//! number, its definition, its resolved argument values and its inputs' keys; the [`Cache`]
//! keeps outputs by key, so after an edit only nodes whose key changed are built again (the
//! edited node and what depends on it). Suppressed modifiers forward their first input;
//! a failing modifier forwards its input and reports why.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};

use super::ops::TOL;
use super::{fit, FeatureError, Kind, Node, NodeId, PlaneKind, Registry, Shape, Timeline};
use crate::component::expr::{self, Q};
use crate::csg::{ftag, tag_op, Body, BoolOp, Poly, LOCAL_BITS};
use crate::mass as gm;
use crate::math::{v2, v3, V2, V3};
use crate::sketch_dims::ProfileSketch;
use crate::sketch_solve::Verdict;

/// Sketch plane frame: origin and in-plane axes (normal `u × v`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame3 {
    pub o: V3,
    pub u: V3,
    pub v: V3,
}

impl Frame3 {
    pub fn of(plane: PlaneKind, off: f64) -> Frame3 {
        let (x, y, z) = (v3(1., 0., 0.), v3(0., 1., 0.), v3(0., 0., 1.));
        match plane {
            PlaneKind::Xy => Frame3 {
                o: z * off,
                u: x,
                v: y,
            },
            PlaneKind::Xz => Frame3 {
                o: y * off,
                u: x,
                v: z,
            },
            PlaneKind::Yz => Frame3 {
                o: x * off,
                u: y,
                v: z,
            },
        }
    }
    pub fn n(&self) -> V3 {
        self.u.cross(self.v)
    }
    pub fn at(&self, p: V2) -> V3 {
        self.o + self.u * p.x + self.v * p.y
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub plane: PlaneKind,
    pub frame: Frame3,
    pub ring: Vec<V2>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    Profile(Profile),
    Solid(Body),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Built,
    /// Same key as before: the cached output was used.
    Reused,
    Suppressed,
    /// An input has no output (suppressed, skipped or failed).
    Skipped(NodeId),
    Failed(String),
    /// After the roll-back marker.
    Rolled,
}

impl Status {
    pub fn ok(&self) -> bool {
        matches!(self, Status::Built | Status::Reused)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodeReport {
    pub id: NodeId,
    pub status: Status,
    /// 0 when nothing was evaluated.
    pub key: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Rebuild {
    pub nodes: Vec<NodeReport>,
    pub outs: HashMap<NodeId, Out>,
    /// Solid outputs no other built feature consumed, in timeline order.
    pub leaves: Vec<NodeId>,
    /// Union of the leaves.
    pub body: Option<Body>,
    pub built: usize,
    pub reused: usize,
    /// Face keys of features whose keys the build extended (see [`Timeline::adopt_keys`]).
    pub keys: Vec<(NodeId, gm::FaceKeys)>,
}

impl Rebuild {
    pub fn status(&self, id: NodeId) -> Option<&Status> {
        self.nodes.iter().find(|n| n.id == id).map(|n| &n.status)
    }
    pub fn solid(&self, id: NodeId) -> Option<&Body> {
        match self.outs.get(&id) {
            Some(Out::Solid(b)) => Some(b),
            _ => None,
        }
    }
    /// First failure, for a one-line message.
    pub fn first_error(&self) -> Option<(NodeId, &str)> {
        self.nodes.iter().find_map(|n| match &n.status {
            Status::Failed(m) => Some((n.id, m.as_str())),
            _ => None,
        })
    }
}

/// Outputs by node key, oldest dropped first.
#[derive(Debug)]
pub struct Cache {
    map: HashMap<u64, (Out, gm::FaceKeys)>,
    order: VecDeque<u64>,
    cap: usize,
}

impl Default for Cache {
    fn default() -> Self {
        Cache::new(256)
    }
}

impl Cache {
    pub fn new(cap: usize) -> Self {
        Cache {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    fn get(&mut self, k: u64) -> Option<(Out, gm::FaceKeys)> {
        let o = self.map.get(&k)?.clone();
        self.order.retain(|x| *x != k);
        self.order.push_back(k);
        Some(o)
    }
    fn put(&mut self, k: u64, o: (Out, gm::FaceKeys)) {
        if self.map.insert(k, o).is_none() {
            self.order.push_back(k);
        }
        while self.map.len() > self.cap {
            match self.order.pop_front() {
                Some(old) => {
                    self.map.remove(&old);
                }
                None => break,
            }
        }
    }
}

fn resolve(n: &Node, env: &HashMap<String, Q>, reg: &Registry) -> Result<Vec<f64>, String> {
    n.kind
        .args(reg)
        .iter()
        .map(|(k, text, d)| {
            let e = expr::parse(text.trim()).map_err(|e| format!("{k}: {e}"))?;
            let q = e.eval(&|x| env.get(x).copied()).map_err(|m| format!("{k}: {m}"))?;
            let v = fit(q, *d).map_err(|m| format!("{k}: {m}"))?;
            if v.is_finite() {
                Ok(v)
            } else {
                Err(format!("{k}: not a finite value"))
            }
        })
        .collect()
}

fn key(op: u32, n: &Node, vals: &[f64], ins: &[u64]) -> u64 {
    let mut h = DefaultHasher::new();
    op.hash(&mut h);
    format!("{:?}", n.kind).hash(&mut h);
    n.keys.hash(&mut h);
    vals.iter().for_each(|v| v.to_bits().hash(&mut h));
    ins.hash(&mut h);
    h.finish()
}

fn map_body(b: &Body, f: impl Fn(V3) -> V3) -> Body {
    Body {
        polys: b
            .polys
            .iter()
            .filter_map(|p| Poly::new(p.pts.iter().map(|q| f(*q)).collect(), p.tag))
            .collect(),
    }
}

fn solved_ring(ps: &ProfileSketch, env: &HashMap<String, Q>) -> Result<Vec<V2>, String> {
    let mut s = ps.clone();
    let names = |n: &str| env.get(n).copied();
    let driving: Vec<(i64, String)> = s
        .dims
        .iter()
        .filter(|d| !d.driven)
        .map(|d| (d.tag, d.text.clone()))
        .collect();
    for (tag, text) in driving {
        s.set_value(tag, &text, &names)
            .map_err(|e| format!("dim{tag}: {e:?}"))?;
    }
    if s.analyze().verdict == Verdict::Conflict {
        return Err("sketch constraints conflict".into());
    }
    let lp = s.profile().map_err(|e| format!("sketch outline: {}", e.key()))?;
    Ok(lp.iter().map(|&p| v2(s.pts[p][0], s.pts[p][1])).collect())
}

fn ring_area(r: &[V2]) -> f64 {
    (0..r.len())
        .map(|i| r[i].cross(r[(i + 1) % r.len()]))
        .sum::<f64>()
        / 2.0
}

fn profile(o: &Out) -> Result<&Profile, String> {
    match o {
        Out::Profile(p) => Ok(p),
        Out::Solid(_) => Err("input is not a sketch".into()),
    }
}

fn solid(o: &Out) -> Result<&Body, String> {
    match o {
        Out::Solid(b) => Ok(b),
        Out::Profile(_) => Err("input is not a body".into()),
    }
}

/// Face references and keys are stored with node ids as ops; a build at `base` moves the
/// timeline's own tags by `base` on the way in and back on the way out.
#[derive(Clone, Copy)]
struct Shift {
    base: u32,
    next: u32,
}

impl Shift {
    fn local(t: u32) -> u32 {
        t & ((1 << LOCAL_BITS) - 1)
    }
    fn to_built(self, t: u32) -> u32 {
        let o = tag_op(t);
        if self.base > 0 && o >= 1 && o < self.next {
            ftag(o + self.base, Self::local(t))
        } else {
            t
        }
    }
    fn to_stored(self, t: u32) -> u32 {
        let o = tag_op(t);
        if self.base > 0 && o > self.base && o < self.base + self.next {
            ftag(o - self.base, Self::local(t))
        } else {
            t
        }
    }
    fn keys(self, k: &gm::FaceKeys, f: impl Fn(Self, u32) -> u32) -> gm::FaceKeys {
        k.iter().map(|k| [f(self, k[0]), f(self, k[1]), k[2], k[3]]).collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn eval(
    n: &Node,
    v: &[f64],
    ins: &[&Out],
    env: &HashMap<String, Q>,
    reg: &Registry,
    op: u32,
    keys: &mut gm::FaceKeys,
    sh: Shift,
) -> Result<Out, String> {
    let body = match &n.kind {
        Kind::Sketch { plane, shape, .. } => {
            let frame = Frame3::of(*plane, v[0]);
            let ring: Vec<V2> = match shape {
                Shape::Rect { .. } => {
                    let (cx, cy, w, h) = (v[1], v[2], v[3], v[4]);
                    if !(w > 0.0 && h > 0.0) {
                        return Err("rectangle sides must be positive".into());
                    }
                    let (a, b) = (w / 2.0, h / 2.0);
                    vec![
                        v2(cx - a, cy - b),
                        v2(cx + a, cy - b),
                        v2(cx + a, cy + b),
                        v2(cx - a, cy + b),
                    ]
                }
                Shape::Circle { seg, .. } => {
                    let (cx, cy, r) = (v[1], v[2], v[3]);
                    if r <= 0.0 {
                        return Err("radius must be positive".into());
                    }
                    let k = (*seg).clamp(3, gm::MAX_SEG);
                    (0..k)
                        .map(|i| {
                            let a = std::f64::consts::TAU * i as f64 / k as f64;
                            v2(cx + r * a.cos(), cy + r * a.sin())
                        })
                        .collect()
                }
                Shape::Poly(_) => v[1..].chunks(2).map(|c| v2(c[0], c[1])).collect(),
                Shape::Solved(ps) => solved_ring(ps, env)?,
            };
            if ring.len() < 3 || ring_area(&ring).abs() < 1e-12 {
                return Err("profile has no area".into());
            }
            return Ok(Out::Profile(Profile {
                plane: *plane,
                frame,
                ring,
            }));
        }
        Kind::Extrude { .. } => {
            let p = profile(ins[0])?;
            let (d, s) = (v[0], v[1]);
            if d.abs() < 1e-9 {
                return Err("extrusion distance is zero".into());
            }
            let nn = p.frame.n();
            let ring: Vec<V3> = p.ring.iter().map(|q| p.frame.at(*q) + nn * s).collect();
            let sides: Vec<u32> = (0..ring.len() as u32).map(|i| 2 + i).collect();
            gm::prism_tagged(&ring, nn * d, &sides, 0, 1)
        }
        Kind::Revolve { seg, .. } => {
            let p = profile(ins[0])?;
            let a = v[0];
            if !(a.abs() > 1e-6 && a.abs() <= std::f64::consts::TAU + 1e-12) {
                return Err("revolve angle must be within (0, 360] degrees".into());
            }
            if p.ring.iter().any(|q| q.x < -1e-12) {
                return Err("revolve profile must stay on one side of the axis (u >= 0)".into());
            }
            let f = p.frame;
            let w = f.v.cross(f.u);
            let b = gm::revolve(&p.ring, a, *seg);
            map_body(&b, |q| f.o + f.u * q.x + w * q.y + f.v * q.z)
        }
        Kind::Sweep { .. } => {
            let p = profile(ins[0])?;
            let path: Vec<V3> = v.chunks(3).map(|c| v3(c[0], c[1], c[2])).collect();
            gm::sweep(&p.ring, &path).map_err(|e| e.to_string())?
        }
        Kind::Loft { .. } => {
            let mut sec = vec![];
            for o in ins {
                let p = profile(o)?;
                if p.plane != PlaneKind::Xy {
                    return Err("loft needs plan (xy) sketches".into());
                }
                sec.push((p.ring.clone(), p.frame.o.z));
            }
            if sec.len() < 2 {
                return Err("loft needs two sketches or more".into());
            }
            gm::blend(&sec).map_err(|e| e.to_string())?
        }
        Kind::Boolean { op: bop, .. } => {
            let out = solid(ins[0])?.boolean(solid(ins[1])?, *bop, TOL);
            if out.is_empty() {
                return Err("boolean result is empty".into());
            }
            return Ok(Out::Solid(out));
        }
        Kind::Move { .. } => {
            let (d, t) = (v3(v[0], v[1], v[2]), v[3]);
            let (s, c) = t.sin_cos();
            let b = map_body(solid(ins[0])?, |q| v3(q.x * c - q.y * s, q.x * s + q.y * c, q.z) + d);
            return Ok(Out::Solid(b));
        }
        Kind::Modify { op: kind, faces, .. } => {
            let o = reg.get(kind).ok_or_else(|| format!("unknown operation '{kind}'"))?;
            let (lo, hi) = o.faces();
            if faces.len() < lo || faces.len() > hi {
                return Err(format!("{kind} needs {lo}..{hi} faces"));
            }
            let faces: Vec<u32> = faces.iter().map(|t| sh.to_built(*t)).collect();
            let mut built = sh.keys(keys, Shift::to_built);
            let b = o.build_keyed(solid(ins[0])?, &faces, v, op, &mut built)?;
            *keys = sh.keys(&built, Shift::to_stored);
            if b.is_empty() {
                return Err(format!("{kind}: empty body"));
            }
            return Ok(Out::Solid(b));
        }
    };
    if body.is_empty() {
        return Err("empty body".into());
    }
    Ok(Out::Solid(body.tagged(op)))
}

/// Builds the timeline. `op_base` shifts every face-tag op (a timeline inside a larger history).
/// Errors only for parameters (parse, unknown name, cycle); features report per node.
pub fn rebuild(
    tl: &Timeline,
    reg: &Registry,
    cache: &mut Cache,
    op_base: u32,
) -> Result<Rebuild, FeatureError> {
    let env = tl.values()?;
    let stop = tl.marker_index();
    let mut outs: HashMap<NodeId, (u64, Out)> = HashMap::new();
    let mut used: HashSet<NodeId> = HashSet::new();
    let mut r = Rebuild::default();
    for (i, n) in tl.nodes.iter().enumerate() {
        let inputs = n.kind.inputs();
        let forward = |outs: &mut HashMap<NodeId, (u64, Out)>, used: &mut HashSet<NodeId>| {
            if let Some(f) = inputs.first().and_then(|a| outs.get(a).cloned()) {
                used.insert(inputs[0]);
                outs.insert(n.id, f);
            }
        };
        let status = if i >= stop {
            Status::Rolled
        } else if n.suppressed {
            if n.kind.passes_through() {
                forward(&mut outs, &mut used);
            }
            Status::Suppressed
        } else if let Some(miss) = inputs.iter().find(|a| !outs.contains_key(*a)) {
            if matches!(n.kind, Kind::Boolean { .. }) {
                forward(&mut outs, &mut used);
            }
            Status::Skipped(*miss)
        } else {
            let ins: Vec<&(u64, Out)> = inputs.iter().filter_map(|a| outs.get(a)).collect();
            let op = op_base + n.id;
            let got = resolve(n, &env, reg).and_then(|vals| {
                let k = key(op, n, &vals, &ins.iter().map(|x| x.0).collect::<Vec<_>>());
                match cache.get(k) {
                    Some((o, keys)) => Ok((k, o, keys, true)),
                    None => {
                        let mut keys = n.keys.clone();
                        let ins: Vec<&Out> = ins.iter().map(|x| &x.1).collect();
                        let sh = Shift {
                            base: op_base,
                            next: tl.next_id,
                        };
                        let o = eval(n, &vals, &ins, &env, reg, op, &mut keys, sh)?;
                        cache.put(k, (o.clone(), keys.clone()));
                        Ok((k, o, keys, false))
                    }
                }
            });
            match got {
                Ok((k, o, keys, hit)) => {
                    if keys != n.keys {
                        r.keys.push((n.id, keys));
                    }
                    used.extend(inputs.iter().copied());
                    outs.insert(n.id, (k, o));
                    if hit {
                        r.reused += 1;
                        Status::Reused
                    } else {
                        r.built += 1;
                        Status::Built
                    }
                }
                Err(m) => {
                    if n.kind.passes_through() {
                        forward(&mut outs, &mut used);
                    }
                    Status::Failed(m)
                }
            }
        };
        r.nodes.push(NodeReport {
            id: n.id,
            status,
            key: outs.get(&n.id).map_or(0, |o| o.0),
        });
    }
    for n in &tl.nodes {
        if let Some((_, Out::Solid(_))) = outs.get(&n.id) {
            if !used.contains(&n.id) {
                r.leaves.push(n.id);
            }
        }
    }
    for id in &r.leaves {
        if let Some((_, Out::Solid(b))) = outs.get(id) {
            r.body = Some(match r.body.take() {
                None => b.clone(),
                Some(acc) => acc.boolean(b, BoolOp::Union, TOL),
            });
        }
    }
    r.outs = outs.into_iter().map(|(k, v)| (k, v.1)).collect();
    Ok(r)
}
