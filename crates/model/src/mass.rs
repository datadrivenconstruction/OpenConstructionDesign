// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! In-place forms and solids: the construction history (`Form`) is stored as a JSON tree and evaluated
//! into `ddc_geom::csg::Body`. Voids (`void`) cut the elements they are joined to.
//! Geometry join `geom_join`: the winner cuts the loser (ordered by category priority,
//! changeable). Face-hosted elements (`mass_host`) are updated when the form is edited.
//! Face paint `mass_paint`, form floors `mass_qty` (derived).
//!
//! Face tags are `ftag(history operation, face)`: an element binding stores the tag and the kind
//! of the operation that created it, so a vanished face is never replaced by another one.

use ddc_geom::csg::{ftag, tag_op, Body, BoolOp, LOCAL_BITS};
use ddc_geom::poly;
use ddc_geom::{mass as gm, v2, v3, Tol, V2, V3};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::ops::Inv;
use crate::{CurtainSpec, Error, GridLines, LineSections, Model, MullionSection, RoofKind};
use crate::{RoofSpec, SlabSpec, WallSpec};

fn invalid<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

pub(crate) const TOL: Tol = Tol {
    length: 1e-7,
    angle: 1e-9,
};

/// Form construction history (coordinates are from the form level, meters).
#[derive(Clone, Debug, PartialEq)]
pub enum Form {
    Cuboid {
        lo: (f64, f64, f64),
        hi: (f64, f64, f64),
    },
    /// `ids` are stable contour edge numbers (empty = 0..n): an edge's side is face `2 + id`.
    Extrude {
        ring: Vec<(f64, f64)>,
        z0: f64,
        h: f64,
        ids: Vec<u32>,
    },
    /// Profile (r, z) revolved about the vertical through `axis`.
    Revolve {
        profile: Vec<(f64, f64)>,
        axis: (f64, f64),
        angle: f64,
        seg: usize,
        ids: Vec<u32>,
    },
    Sweep {
        profile: Vec<(f64, f64)>,
        path: Vec<(f64, f64, f64)>,
        ids: Vec<u32>,
    },
    /// `ids` are edge numbers per section, one shared number space (empty = sequential).
    Blend {
        sections: Vec<(Vec<(f64, f64)>, f64)>,
        ids: Vec<Vec<u32>>,
    },
    SweptBlend {
        p0: Vec<(f64, f64)>,
        p1: Vec<(f64, f64)>,
        path: Vec<(f64, f64, f64)>,
        ids: Vec<u32>,
    },
    Bool {
        op: BoolOp,
        a: Box<Form>,
        b: Box<Form>,
    },
    /// `keys` are the numbers of new side faces by neighbouring faces ([`gm::FaceKeys`]),
    /// recorded when the form is saved.
    PushPull {
        base: Box<Form>,
        face: u32,
        d: f64,
        keys: Vec<[u32; 4]>,
    },
    Chamfer {
        base: Box<Form>,
        faces: (u32, u32),
        d: f64,
    },
    Fillet {
        base: Box<Form>,
        faces: (u32, u32),
        r: f64,
        seg: usize,
    },
    Shell {
        base: Box<Form>,
        t: f64,
        open: Vec<u32>,
        keys: Vec<[u32; 4]>,
    },
    Offset {
        base: Box<Form>,
        faces: Vec<u32>,
        d: f64,
        keys: Vec<[u32; 4]>,
    },
    /// Translation of the whole form (face tags are preserved).
    Move { base: Box<Form>, d: (f64, f64, f64) },
    SplitFace {
        base: Box<Form>,
        face: u32,
        at: (f64, f64, f64),
        n: (f64, f64, f64),
    },
    /// Parametric feature timeline (`feature_tl`): takes one op per node id ever given, its
    /// faces are `ftag(op before it + node id, k)`.
    Timeline(Box<ddc_geom::feature::Timeline>),
    /// Push/pull of a loop drawn on a face (empty loop: the whole face) to a target that is
    /// re-evaluated with the history (a moved target face moves the push).
    PushTo {
        base: Box<Form>,
        face: u32,
        region: Vec<(f64, f64, f64)>,
        to: PushGoal,
        keys: Vec<[u32; 4]>,
    },
    /// Fillets (`round`) or chamfers of several edges (face pairs) in one step.
    Blends {
        base: Box<Form>,
        edges: Vec<(u32, u32)>,
        round: bool,
        size: f64,
        seg: usize,
    },
    /// Faces turned about the neutral plane (point `at`, normal `pull`) by `angle` radians.
    Draft {
        base: Box<Form>,
        faces: Vec<u32>,
        at: (f64, f64, f64),
        pull: (f64, f64, f64),
        angle: f64,
    },
}

/// Target of [`Form::PushTo`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PushGoal {
    Dist(f64),
    Face(u32),
    Point((f64, f64, f64)),
    Through,
}

impl PushGoal {
    pub(crate) fn geom(self) -> gm::PushTarget {
        match self {
            PushGoal::Dist(d) => gm::PushTarget::Dist(d),
            PushGoal::Face(t) => gm::PushTarget::Face(t),
            PushGoal::Point(p) => gm::PushTarget::Point(t3(p)),
            PushGoal::Through => gm::PushTarget::Through,
        }
    }

    fn to_json(self) -> Value {
        match self {
            PushGoal::Dist(d) => json!({ "d": d }),
            PushGoal::Face(t) => json!({ "face": t }),
            PushGoal::Point(p) => json!({ "pt": [p.0, p.1, p.2] }),
            PushGoal::Through => json!({ "through": true }),
        }
    }

    fn from_json(v: &Value) -> Option<PushGoal> {
        if let Some(d) = v.get("d").and_then(Value::as_f64) {
            return Some(PushGoal::Dist(d));
        }
        if let Some(t) = v.get("face").and_then(Value::as_u64) {
            return u32::try_from(t).ok().map(PushGoal::Face);
        }
        if let Some(a) = v.get("pt").and_then(Value::as_array) {
            let c: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
            return (c.len() == 3 && a.len() == 3).then(|| PushGoal::Point((c[0], c[1], c[2])));
        }
        (v.get("through").and_then(Value::as_bool) == Some(true)).then_some(PushGoal::Through)
    }
}

fn p2(v: &[(f64, f64)]) -> Vec<V2> {
    v.iter().map(|p| v2(p.0, p.1)).collect()
}
fn p3(v: &[(f64, f64, f64)]) -> Vec<V3> {
    v.iter().map(|p| v3(p.0, p.1, p.2)).collect()
}
fn t3(p: (f64, f64, f64)) -> V3 {
    v3(p.0, p.1, p.2)
}

fn take(next: &mut u32) -> u32 {
    let op = *next;
    *next += 1;
    op
}

fn ids_or(ids: &[u32], n: usize) -> Vec<u32> {
    if ids.is_empty() {
        (0..n as u32).collect()
    } else {
        ids.to_vec()
    }
}

/// Edge numbers of the blend sections (sequential by default, as in `gm::blend_ids`).
fn blend_ids_or(sections: &[(Vec<(f64, f64)>, f64)], ids: &[Vec<u32>]) -> Vec<Vec<u32>> {
    let mut off = 0u32;
    sections
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let n = s.0.len() as u32;
            let r = match ids.get(i).filter(|x| !x.is_empty()) {
                Some(x) => x.clone(),
                None => (off..off + n).collect(),
            };
            off += n;
            r
        })
        .collect()
}

/// Edge numbers of a new contour from the old one: an edge with the same endpoints keeps its number; with
/// the same edge count, a non-matching edge takes its position's number (vertex shift); others are new.
fn match_ids(ring: &[(f64, f64)], old: &[(f64, f64)], old_ids: &[u32], next: &mut u32) -> Vec<u32> {
    let same = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
    let (n, m) = (ring.len(), old.len());
    let mut out: Vec<Option<u32>> = (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            (0..m)
                .find(|&j| same(a, old[j]) && same(b, old[(j + 1) % m]))
                .and_then(|j| old_ids.get(j).copied())
        })
        .collect();
    if n == m {
        for i in 0..n {
            if out[i].is_none() {
                let id = old_ids.get(i).copied();
                if id.is_some() && !out.contains(&id) {
                    out[i] = id;
                }
            }
        }
    }
    out.into_iter()
        .map(|x| {
            x.unwrap_or_else(|| {
                *next += 1;
                *next - 1
            })
        })
        .collect()
}

/// Cache of solids by form history (JSON): editing one form does not recompute the others, and
/// previewing and displaying one form do not repeat the CSG.
static EVAL_CACHE: std::sync::Mutex<Vec<(String, Body)>> = std::sync::Mutex::new(Vec::new());
const EVAL_CACHE_CAP: usize = 64;

fn cache_get(key: &str) -> Option<Body> {
    let mut c = EVAL_CACHE.lock().ok()?;
    let i = c.iter().position(|e| e.0 == key)?;
    let e = c.remove(i);
    let b = e.1.clone();
    c.push(e);
    Some(b)
}

/// Frozen histories by the history they were frozen from (JSON), so a history frozen on a
/// worker is not frozen again when the model operation writes it.
static FROZEN_CACHE: std::sync::Mutex<Vec<(String, Form)>> = std::sync::Mutex::new(Vec::new());

fn frozen_get(key: &str) -> Option<Form> {
    let c = FROZEN_CACHE.lock().ok()?;
    c.iter().find(|e| e.0 == key).map(|e| e.1.clone())
}

fn frozen_put(key: String, f: &Form) {
    if let Ok(mut c) = FROZEN_CACHE.lock() {
        c.retain(|e| e.0 != key);
        if c.len() >= EVAL_CACHE_CAP {
            c.remove(0);
        }
        c.push((key, f.clone()));
    }
}

fn cache_put(key: String, b: &Body) {
    if let Ok(mut c) = EVAL_CACHE.lock() {
        c.retain(|e| e.0 != key);
        if c.len() >= EVAL_CACHE_CAP {
            c.remove(0);
        }
        c.push((key, b.clone()));
    }
}

impl Form {
    pub fn base(self) -> Box<Form> {
        Box::new(self)
    }

    /// The form's solid; an error if an operation found no face/edge, or the solid is empty, not closed or
    /// non-manifold. Face tags are `ftag(operation, face)`; operations are numbered by a
    /// bottom-up tree walk (numbers do not change when parameters are edited or steps are appended).
    /// The result is cached by history (a process-wide cache).
    pub fn eval(&self) -> Result<Body, Error> {
        let key = self.to_json().to_string();
        if let Some(b) = cache_get(&key) {
            return Ok(b);
        }
        let mut f = self.clone();
        let b = f.eval_checked()?;
        cache_put(key, &b);
        Ok(b)
    }

    /// History with the numbers of new faces recorded (this is how it is saved: an edit lower in the
    /// history does not renumber faces above it).
    pub fn frozen(&self) -> Result<Form, Error> {
        let key = self.to_json().to_string();
        if let Some(f) = frozen_get(&key) {
            // the solid of a cached history is still in the body cache (same capacity, used
            // together); after an eviction `eval` recomputes it
            return Ok(f);
        }
        let mut f = self.clone();
        let b = f.eval_checked()?;
        cache_put(f.to_json().to_string(), &b);
        frozen_put(key, &f);
        Ok(f)
    }

    /// What `add_mass` (`old` = `None`) and `update_mass` do to a new history before writing it:
    /// inherit the face numbers of `old`, then freeze. Pure, so it can run on a worker under a
    /// cancel token; the result warms the caches the model operation then reads.
    pub fn prepared(&self, old: Option<&Form>) -> Result<Form, Error> {
        let mut f = self.clone();
        if let Some(o) = old {
            f.inherit(o);
        }
        f.frozen()
    }

    fn eval_checked(&mut self) -> Result<Body, Error> {
        let mut next = 1;
        let b = self.eval_at(&mut next)?;
        // any step may have left a kernel loop early: the result is not cached or returned
        if ddc_geom::cancel::requested() {
            return Err(Error::Cancelled);
        }
        if b.is_empty() {
            return invalid("form is empty");
        }
        b.validate(TOL)
            .map_err(|e| Error::Invalid(format!("form body: {e}")))?;
        Ok(b)
    }

    /// Operation kind by number (to check that a face was created by the same operation).
    pub fn op_kinds(&self) -> Vec<&'static str> {
        let mut out = vec![];
        self.walk(&mut out);
        out
    }

    fn walk(&self, out: &mut Vec<&'static str>) {
        match self {
            Form::Bool { a, b, .. } => {
                a.walk(out);
                b.walk(out);
            }
            Form::PushPull { base, .. }
            | Form::Chamfer { base, .. }
            | Form::Fillet { base, .. }
            | Form::Shell { base, .. }
            | Form::Offset { base, .. }
            | Form::Move { base, .. }
            | Form::SplitFace { base, .. }
            | Form::PushTo { base, .. }
            | Form::Blends { base, .. }
            | Form::Draft { base, .. } => base.walk(out),
            Form::Timeline(t) => return crate::feature_tl::walk(t, out),
            _ => {}
        }
        out.push(self.kind());
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Form::Cuboid { .. } => "cuboid",
            Form::Extrude { .. } => "extrude",
            Form::Revolve { .. } => "revolve",
            Form::Sweep { .. } => "sweep",
            Form::Blend { .. } => "blend",
            Form::SweptBlend { .. } => "swept_blend",
            Form::Bool { .. } => "bool",
            Form::PushPull { .. } => "push_pull",
            Form::Chamfer { .. } => "chamfer",
            Form::Fillet { .. } => "fillet",
            Form::Shell { .. } => "shell",
            Form::Offset { .. } => "offset",
            Form::SplitFace { .. } => "split_face",
            Form::Move { .. } => "move",
            Form::PushTo { .. } => "push_to",
            Form::Blends { .. } => "blends",
            Form::Draft { .. } => "draft",
            Form::Timeline(_) => "timeline",
        }
    }

    /// Root operation number (= number of operations in the tree).
    pub fn root_op(&self) -> u32 {
        self.op_kinds().len() as u32
    }

    /// Edge and face numbers from the previous version of the same history: edges matching by endpoints
    /// (and, with the same edge count, shifted ones too) keep their number, new ones get a free one;
    /// keys of derived faces are carried over. A wall bound to a split face does not
    /// jump to the neighbouring face; it is left without a face.
    pub fn inherit(&mut self, old: &Form) {
        match (self, old) {
            (
                Form::Extrude { ring, ids, .. },
                Form::Extrude {
                    ring: o, ids: oi, ..
                },
            )
            | (
                Form::Revolve {
                    profile: ring, ids, ..
                },
                Form::Revolve {
                    profile: o,
                    ids: oi,
                    ..
                },
            )
            | (
                Form::Sweep {
                    profile: ring, ids, ..
                },
                Form::Sweep {
                    profile: o,
                    ids: oi,
                    ..
                },
            )
            | (Form::SweptBlend { p0: ring, ids, .. }, Form::SweptBlend { p0: o, ids: oi, .. }) => {
                if ids.is_empty() {
                    let oi = ids_or(oi, o.len());
                    let mut next = oi.iter().max().map_or(0, |m| m + 1);
                    *ids = match_ids(ring, o, &oi, &mut next);
                }
            }
            (
                Form::Blend { sections, ids },
                Form::Blend {
                    sections: os,
                    ids: oi,
                },
            ) => {
                if ids.is_empty() {
                    let oi = blend_ids_or(os, oi);
                    let mut next = oi.iter().flatten().max().map_or(0, |m| m + 1);
                    *ids = sections
                        .iter()
                        .enumerate()
                        .map(|(i, s)| match (os.get(i), oi.get(i)) {
                            (Some(o), Some(x)) => match_ids(&s.0, &o.0, x, &mut next),
                            _ => (0..s.0.len())
                                .map(|_| {
                                    next += 1;
                                    next - 1
                                })
                                .collect(),
                        })
                        .collect();
                }
            }
            (Form::Bool { a, b, .. }, Form::Bool { a: oa, b: ob, .. }) => {
                a.inherit(oa);
                b.inherit(ob);
            }
            (
                Form::PushPull { base, keys, .. },
                Form::PushPull {
                    base: ob, keys: ok, ..
                },
            )
            | (
                Form::PushTo { base, keys, .. },
                Form::PushTo {
                    base: ob, keys: ok, ..
                },
            )
            | (
                Form::Shell { base, keys, .. },
                Form::Shell {
                    base: ob, keys: ok, ..
                },
            )
            | (
                Form::Offset { base, keys, .. },
                Form::Offset {
                    base: ob, keys: ok, ..
                },
            ) => {
                if keys.is_empty() {
                    keys.clone_from(ok);
                }
                base.inherit(ob);
            }
            (Form::Chamfer { base, .. }, Form::Chamfer { base: ob, .. })
            | (Form::Fillet { base, .. }, Form::Fillet { base: ob, .. })
            | (Form::SplitFace { base, .. }, Form::SplitFace { base: ob, .. })
            | (Form::Blends { base, .. }, Form::Blends { base: ob, .. })
            | (Form::Draft { base, .. }, Form::Draft { base: ob, .. })
            | (Form::Move { base, .. }, Form::Move { base: ob, .. }) => base.inherit(ob),
            _ => {}
        }
    }

    fn eval_at(&mut self, next: &mut u32) -> Result<Body, Error> {
        let kind = self.kind();
        let ge = move |e: gm::MassError| Error::Invalid(format!("{kind}: {e}"));
        if ddc_geom::cancel::requested() {
            return Err(Error::Cancelled);
        }
        let prim = |b: Result<Body, gm::MassError>, next: &mut u32| -> Result<Body, Error> {
            // a stopped kernel loop returns a partial body: never let it out
            if ddc_geom::cancel::requested() {
                return Err(Error::Cancelled);
            }
            let op = *next;
            *next += 1;
            Ok(b.map_err(ge)?.tagged(op))
        };
        match self {
            Form::Cuboid { lo, hi } => {
                let (lo, hi) = (t3(*lo), t3(*hi));
                if !(hi.x > lo.x && hi.y > lo.y && hi.z > lo.z) {
                    return invalid("cuboid: lo < hi");
                }
                prim(Ok(gm::cuboid(lo, hi)), next)
            }
            Form::Extrude { ring, z0, h, ids } => {
                if ring.len() < 3 || !(h.abs() > TOL.length) || !h.is_finite() {
                    return invalid("extrude: ring/height");
                }
                prim(gm::extrude_ids(&p2(ring), ids, *z0, *h), next)
            }
            Form::Revolve {
                profile,
                axis,
                angle,
                seg,
                ids,
            } => {
                if profile.len() < 3 || profile.iter().any(|p| p.0 < 0.0) {
                    return invalid("revolve: profile must have r >= 0");
                }
                if !(angle.abs() > 1e-6 && angle.abs() <= std::f64::consts::TAU + 1e-12) {
                    return invalid("revolve: angle");
                }
                let b = gm::revolve_ids(&p2(profile), ids, *angle, *seg)
                    .map(|b| b.translated(v3(axis.0, axis.1, 0.0)));
                prim(b, next)
            }
            Form::Sweep { profile, path, ids } => {
                prim(gm::sweep_ids(&p2(profile), ids, &p3(path)), next)
            }
            Form::Blend { sections, ids } => {
                let s: Vec<(Vec<V2>, f64)> = sections.iter().map(|s| (p2(&s.0), s.1)).collect();
                prim(gm::blend_ids(&s, ids), next)
            }
            Form::SweptBlend { p0, p1, path, ids } => {
                prim(gm::swept_blend_ids(&p2(p0), &p2(p1), ids, &p3(path)), next)
            }
            Form::Bool { op, a, b } => {
                let a = a.eval_at(next)?;
                let b = b.eval_at(next)?;
                *next += 1;
                let (out, path, _) = ddc_geom::csg_check::routed_boolean(&a, &b, *op, TOL);
                if ddc_geom::cancel::requested() {
                    return Err(Error::Cancelled);
                }
                if path == ddc_geom::csg_check::BoolPath::Planar {
                    PLANAR_BOOLS.with(|c| c.set(c.get() + 1));
                }
                if out.is_empty() {
                    return invalid("bool: result is empty");
                }
                Ok(out)
            }
            Form::Move { base, d } => {
                let b = base.eval_at(next)?;
                *next += 1;
                if ![d.0, d.1, d.2].iter().all(|v| v.is_finite()) {
                    return invalid("move: offset");
                }
                Ok(b.translated(t3(*d)))
            }
            Form::PushPull {
                base,
                face,
                d,
                keys,
            } => {
                let b = base.eval_at(next)?;
                let op = take(next);
                gm::push_pull_keyed(&b, *face, *d, op, keys, TOL).map_err(ge)
            }
            Form::Shell {
                base,
                t,
                open,
                keys,
            } => {
                let b = base.eval_at(next)?;
                let op = take(next);
                gm::shell_keyed(&b, *t, open, op, keys, TOL).map_err(ge)
            }
            Form::Offset {
                base,
                faces,
                d,
                keys,
            } => {
                let b = base.eval_at(next)?;
                let op = take(next);
                gm::offset_faces_keyed(&b, faces, *d, op, keys, TOL).map_err(ge)
            }
            Form::Chamfer { base, faces, d } => {
                let b = base.eval_at(next)?;
                let op = take(next);
                gm::edge_between(&b, faces.0, faces.1, 1e-7)
                    .and_then(|e| gm::chamfer(&b, &e, *d, op, TOL))
                    .map_err(ge)
            }
            Form::Fillet {
                base,
                faces,
                r,
                seg,
            } => {
                let b = base.eval_at(next)?;
                let op = take(next);
                gm::edge_between(&b, faces.0, faces.1, 1e-7)
                    .and_then(|e| gm::fillet(&b, &e, *r, (*seg).max(2), op, TOL))
                    .map_err(ge)
            }
            Form::SplitFace { base, face, at, n } => {
                let b = base.eval_at(next)?;
                let op = take(next);
                gm::split_face(&b, *face, t3(*at), t3(*n), op, TOL).map_err(ge)
            }
            Form::PushTo {
                base,
                face,
                region,
                to,
                keys,
            } => {
                let b = base.eval_at(next)?;
                let op = take(next);
                let d = gm::push_distance(&b, *face, to.geom(), TOL).map_err(ge)?;
                if region.is_empty() {
                    gm::push_pull_keyed(&b, *face, d, op, keys, TOL).map_err(ge)
                } else {
                    gm::push_region(&b, *face, &p3(region), d, op, TOL).map_err(ge)
                }
            }
            Form::Blends {
                base,
                edges,
                round,
                size,
                seg,
            } => {
                let b = base.eval_at(next)?;
                let op = take(next);
                let kind = if *round {
                    gm::BlendKind::Fillet {
                        r: *size,
                        seg: (*seg).max(2),
                    }
                } else {
                    gm::BlendKind::Chamfer { d: *size }
                };
                gm::blend_edges(&b, edges, kind, op, TOL).map_err(ge)
            }
            Form::Draft {
                base,
                faces,
                at,
                pull,
                angle,
            } => {
                let b = base.eval_at(next)?;
                let op = take(next);
                gm::draft(&b, faces, t3(*at), t3(*pull), *angle, op, TOL).map_err(ge)
            }
            Form::Timeline(t) => crate::feature_tl::eval(t, next),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut v = self.json_core();
        // stable edge ids and derived face keys are written only when present
        if let Value::Object(o) = &mut v {
            match self {
                Form::Extrude { ids, .. }
                | Form::Revolve { ids, .. }
                | Form::Sweep { ids, .. }
                | Form::SweptBlend { ids, .. }
                    if !ids.is_empty() =>
                {
                    o.insert("ids".into(), json!(ids));
                }
                Form::Blend { ids, .. } if !ids.is_empty() => {
                    o.insert("ids".into(), json!(ids));
                }
                Form::PushPull { keys, .. }
                | Form::PushTo { keys, .. }
                | Form::Shell { keys, .. }
                | Form::Offset { keys, .. }
                    if !keys.is_empty() =>
                {
                    o.insert("keys".into(), json!(keys));
                }
                _ => {}
            }
        }
        v
    }

    fn json_core(&self) -> Value {
        let b = |f: &Form| f.to_json();
        match self {
            Form::Cuboid { lo, hi } => {
                json!({"k":"cuboid","lo":[lo.0,lo.1,lo.2],"hi":[hi.0,hi.1,hi.2]})
            }
            Form::Extrude { ring, z0, h, .. } => {
                json!({"k":"extrude","ring":ring,"z0":z0,"h":h})
            }
            Form::Revolve {
                profile,
                axis,
                angle,
                seg,
                ..
            } => json!({"k":"revolve","profile":profile,"axis":axis,"angle":angle,"seg":seg}),
            Form::Sweep { profile, path, .. } => {
                json!({"k":"sweep","profile":profile,"path":path})
            }
            Form::Blend { sections, .. } => json!({"k":"blend","sections":sections}),
            Form::SweptBlend { p0, p1, path, .. } => {
                json!({"k":"swept_blend","p0":p0,"p1":p1,"path":path})
            }
            Form::Bool { op, a, b: bb } => {
                let o = match op {
                    BoolOp::Union => "union",
                    BoolOp::Subtract => "subtract",
                    BoolOp::Intersect => "intersect",
                };
                json!({"k":"bool","op":o,"a":b(a),"b":b(bb)})
            }
            Form::PushPull { base, face, d, .. } => {
                json!({"k":"push_pull","base":b(base),"face":face,"d":d})
            }
            Form::Chamfer { base, faces, d } => {
                json!({"k":"chamfer","base":b(base),"faces":[faces.0,faces.1],"d":d})
            }
            Form::Fillet {
                base,
                faces,
                r,
                seg,
            } => {
                json!({"k":"fillet","base":b(base),"faces":[faces.0,faces.1],"r":r,"seg":seg})
            }
            Form::Shell { base, t, open, .. } => {
                json!({"k":"shell","base":b(base),"t":t,"open":open})
            }
            Form::Offset { base, faces, d, .. } => {
                json!({"k":"offset","base":b(base),"faces":faces,"d":d})
            }
            Form::Move { base, d } => json!({"k":"move","base":b(base),"d":[d.0,d.1,d.2]}),
            Form::SplitFace { base, face, at, n } => {
                json!({"k":"split_face","base":b(base),"face":face,"at":[at.0,at.1,at.2],"n":[n.0,n.1,n.2]})
            }
            Form::PushTo {
                base,
                face,
                region,
                to,
                ..
            } => {
                json!({"k":"push_to","base":b(base),"face":face,"region":region,"to":to.to_json()})
            }
            Form::Blends {
                base,
                edges,
                round,
                size,
                seg,
            } => {
                let e: Vec<[u32; 2]> = edges.iter().map(|x| [x.0, x.1]).collect();
                json!({"k":"blends","base":b(base),"edges":e,"round":round,"size":size,"seg":seg})
            }
            Form::Draft {
                base,
                faces,
                at,
                pull,
                angle,
            } => json!({"k":"draft","base":b(base),"faces":faces,
                "at":[at.0,at.1,at.2],"pull":[pull.0,pull.1,pull.2],"angle":angle}),
            Form::Timeline(t) => crate::feature_tl::to_json(t),
        }
    }

    pub fn from_json(v: &Value) -> Result<Form, Error> {
        let bad = || Error::Invalid(format!("form json: {v}"));
        let f = |k: &str| v.get(k).and_then(Value::as_f64).ok_or_else(bad);
        let u = |k: &str| v.get(k).and_then(Value::as_u64).ok_or_else(bad);
        let pt3 = |x: &Value| -> Option<(f64, f64, f64)> {
            let a = x.as_array()?;
            Some((
                a.first()?.as_f64()?,
                a.get(1)?.as_f64()?,
                a.get(2)?.as_f64()?,
            ))
        };
        let pt2 = |x: &Value| -> Option<(f64, f64)> {
            let a = x.as_array()?;
            Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
        };
        let l2 = |x: Option<&Value>| -> Result<Vec<(f64, f64)>, Error> {
            x.and_then(Value::as_array)
                .ok_or_else(bad)?
                .iter()
                .map(|p| pt2(p).ok_or_else(bad))
                .collect()
        };
        let l3 = |k: &str| -> Result<Vec<(f64, f64, f64)>, Error> {
            v.get(k)
                .and_then(Value::as_array)
                .ok_or_else(bad)?
                .iter()
                .map(|p| pt3(p).ok_or_else(bad))
                .collect()
        };
        let tags = |k: &str| -> Result<Vec<u32>, Error> {
            v.get(k)
                .and_then(Value::as_array)
                .ok_or_else(bad)?
                .iter()
                .map(|p| p.as_u64().map(|x| x as u32).ok_or_else(bad))
                .collect()
        };
        let ids = |x: Option<&Value>| -> Result<Vec<u32>, Error> {
            match x {
                None => Ok(vec![]),
                Some(a) => a
                    .as_array()
                    .ok_or_else(bad)?
                    .iter()
                    .map(|p| p.as_u64().map(|x| x as u32).ok_or_else(bad))
                    .collect(),
            }
        };
        let keys = || -> Result<Vec<[u32; 4]>, Error> {
            let Some(a) = v.get("keys") else {
                return Ok(vec![]);
            };
            a.as_array()
                .ok_or_else(bad)?
                .iter()
                .map(|k| {
                    let k = ids(Some(k))?;
                    <[u32; 4]>::try_from(k.as_slice()).map_err(|_| bad())
                })
                .collect()
        };
        let sub = |k: &str| -> Result<Box<Form>, Error> {
            Ok(Box::new(Form::from_json(v.get(k).ok_or_else(bad)?)?))
        };
        let k = v.get("k").and_then(Value::as_str).ok_or_else(bad)?;
        Ok(match k {
            "cuboid" => Form::Cuboid {
                lo: v.get("lo").and_then(pt3).ok_or_else(bad)?,
                hi: v.get("hi").and_then(pt3).ok_or_else(bad)?,
            },
            "extrude" => Form::Extrude {
                ring: l2(v.get("ring"))?,
                z0: f("z0")?,
                h: f("h")?,
                ids: ids(v.get("ids"))?,
            },
            "revolve" => Form::Revolve {
                profile: l2(v.get("profile"))?,
                axis: v.get("axis").and_then(pt2).ok_or_else(bad)?,
                angle: f("angle")?,
                seg: u("seg")? as usize,
                ids: ids(v.get("ids"))?,
            },
            "sweep" => Form::Sweep {
                profile: l2(v.get("profile"))?,
                path: l3("path")?,
                ids: ids(v.get("ids"))?,
            },
            "blend" => Form::Blend {
                sections: v
                    .get("sections")
                    .and_then(Value::as_array)
                    .ok_or_else(bad)?
                    .iter()
                    .map(|s| {
                        let a = s.as_array().ok_or_else(bad)?;
                        Ok((
                            l2(a.first())?,
                            a.get(1).and_then(Value::as_f64).ok_or_else(bad)?,
                        ))
                    })
                    .collect::<Result<_, Error>>()?,
                ids: match v.get("ids") {
                    None => vec![],
                    Some(a) => a
                        .as_array()
                        .ok_or_else(bad)?
                        .iter()
                        .map(|x| ids(Some(x)))
                        .collect::<Result<_, Error>>()?,
                },
            },
            "swept_blend" => Form::SweptBlend {
                p0: l2(v.get("p0"))?,
                p1: l2(v.get("p1"))?,
                path: l3("path")?,
                ids: ids(v.get("ids"))?,
            },
            "bool" => Form::Bool {
                op: match v.get("op").and_then(Value::as_str) {
                    Some("union") => BoolOp::Union,
                    Some("subtract") => BoolOp::Subtract,
                    Some("intersect") => BoolOp::Intersect,
                    _ => return Err(bad()),
                },
                a: sub("a")?,
                b: sub("b")?,
            },
            "push_pull" => Form::PushPull {
                base: sub("base")?,
                face: u("face")? as u32,
                d: f("d")?,
                keys: keys()?,
            },
            "chamfer" | "fillet" => {
                let t = tags("faces")?;
                if t.len() != 2 {
                    return Err(bad());
                }
                if k == "chamfer" {
                    Form::Chamfer {
                        base: sub("base")?,
                        faces: (t[0], t[1]),
                        d: f("d")?,
                    }
                } else {
                    Form::Fillet {
                        base: sub("base")?,
                        faces: (t[0], t[1]),
                        r: f("r")?,
                        seg: u("seg")? as usize,
                    }
                }
            }
            "shell" => Form::Shell {
                base: sub("base")?,
                t: f("t")?,
                open: tags("open")?,
                keys: keys()?,
            },
            "offset" => Form::Offset {
                base: sub("base")?,
                faces: tags("faces")?,
                d: f("d")?,
                keys: keys()?,
            },
            "move" => Form::Move {
                base: sub("base")?,
                d: v.get("d").and_then(pt3).ok_or_else(bad)?,
            },
            "split_face" => Form::SplitFace {
                base: sub("base")?,
                face: u("face")? as u32,
                at: v.get("at").and_then(pt3).ok_or_else(bad)?,
                n: v.get("n").and_then(pt3).ok_or_else(bad)?,
            },
            "push_to" => Form::PushTo {
                base: sub("base")?,
                face: u("face")? as u32,
                region: l3("region")?,
                to: v.get("to").and_then(PushGoal::from_json).ok_or_else(bad)?,
                keys: keys()?,
            },
            "blends" => Form::Blends {
                base: sub("base")?,
                edges: v
                    .get("edges")
                    .and_then(Value::as_array)
                    .ok_or_else(bad)?
                    .iter()
                    .map(|e| {
                        let p = ids(Some(e))?;
                        if p.len() == 2 {
                            Ok((p[0], p[1]))
                        } else {
                            Err(bad())
                        }
                    })
                    .collect::<Result<_, Error>>()?,
                round: v.get("round").and_then(Value::as_bool).ok_or_else(bad)?,
                size: f("size")?,
                seg: u("seg")? as usize,
            },
            "draft" => Form::Draft {
                base: sub("base")?,
                faces: tags("faces")?,
                at: v.get("at").and_then(pt3).ok_or_else(bad)?,
                pull: v.get("pull").and_then(pt3).ok_or_else(bad)?,
                angle: f("angle")?,
            },
            "timeline" => Form::Timeline(Box::new(crate::feature_tl::from_json(v)?)),
            _ => return Err(bad()),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MassSpec {
    pub level: i64,
    pub name: String,
    /// Void form: cuts the joined elements and is not displayed as solid itself.
    pub void: bool,
    /// In-place solid (part of the project, not a conceptual form).
    pub inplace: bool,
    pub form: Form,
}

/// Form face class for building elements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaceClass {
    Vertical,
    Up,
    Down,
    Sloped,
    /// Curved surface (revolve and blend facets): elements are not built on it.
    Curved,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MassFace {
    pub tag: u32,
    /// Normal of a flat face; for a curved one, the area-weighted mean.
    pub normal: (f64, f64, f64),
    pub area: f64,
    pub class: FaceClass,
    /// Face contours in world coordinates (outer ones counter-clockwise, holes clockwise).
    pub outline: Vec<Vec<(f64, f64, f64)>>,
    pub material: Option<String>,
}

/// Form schedule row: a form floor (level) with area and perimeter; the volume and surface
/// of the whole form are repeated in every row.
#[derive(Clone, Debug, PartialEq)]
pub struct MassRow {
    pub mass: i64,
    pub name: String,
    pub level: i64,
    pub level_name: String,
    pub area: f64,
    pub perimeter: f64,
    pub volume: f64,
    pub surface: f64,
}

/// Element binding to a form: a face (`face`, with the kind of the operation that created it) or a floor (`floor`).
#[derive(Clone, Debug, PartialEq)]
pub struct MassHost {
    pub kind: String,
    pub id: i64,
    pub mass: i64,
    pub face: u32,
    pub floor_level: Option<i64>,
    /// The face vanished when the form was edited: the element stays in place, the binding is not carried over.
    pub orphan: bool,
}

fn elev(db: &Connection, l: i64) -> Result<f64, Error> {
    db.query_row("SELECT elevation FROM level WHERE id=?1", [l], |r| r.get(0))
        .optional()?
        .map_or_else(|| invalid(format!("no level {l}")), Ok)
}

fn levels(db: &Connection) -> Result<Vec<(i64, f64)>, Error> {
    Ok(db
        .prepare("SELECT id,elevation FROM level ORDER BY elevation,id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?)
}

fn read(db: &Connection, id: i64) -> Result<MassSpec, Error> {
    let row = db
        .query_row(
            "SELECT level,name,void,inplace,form FROM mass WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((level, name, void, inplace, form)) = row else {
        return invalid(format!("no such mass {id}"));
    };
    Ok(MassSpec {
        level,
        name,
        void: void != 0,
        inplace: inplace != 0,
        form: Form::from_json(&serde_json::from_str(&form)?)?,
    })
}

thread_local! {
    /// Form booleans built by the planar BSP path on this thread (the mesh path is not counted).
    static PLANAR_BOOLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count of form booleans the planar BSP path built on this thread.
#[cfg(test)]
pub(crate) fn planar_bools() -> usize {
    PLANAR_BOOLS.with(|c| c.get())
}

/// Form solid in world coordinates.
fn world(db: &Connection, s: &MassSpec) -> Result<Body, Error> {
    Ok(s.form.eval()?.translated(v3(0., 0., elev(db, s.level)?)))
}

/// Form ready to write: edge and face numbers from the previous version, keys of new faces recorded;
/// the solid is validated.
fn prepare(db: &Connection, s: &MassSpec, old: Option<&Form>) -> Result<MassSpec, Error> {
    let mut s = s.clone();
    s.form = s.form.prepared(old)?;
    elev(db, s.level)?;
    Ok(s)
}

fn write(db: &Connection, s: &MassSpec, id: Option<i64>) -> Result<i64, Error> {
    db.execute(
        "INSERT OR REPLACE INTO mass(id,level,name,void,inplace,form) VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            id,
            s.level,
            s.name,
            s.void as i64,
            s.inplace as i64,
            s.form.to_json().to_string()
        ],
    )?;
    Ok(db.last_insert_rowid())
}

fn member_kind(kind: &str) -> &str {
    if kind == "col" {
        "column"
    } else {
        kind
    }
}

/// Part of an element solid: the layer position in the type (material and quantities follow it) and the solid in
/// world coordinates.
pub type ElementPart = (i64, Body);

/// Wall layer solids: the layer footprint after joints from bottom to top, top by profile, minus openings.
fn wall_parts(db: &Connection, id: i64) -> Result<Vec<ElementPart>, Error> {
    type Row = (i64, f64, f64, f64, f64, f64, f64, Option<String>);
    let (level, ax, ay, bx, by, h, bulge, top): Row = db
        .query_row(
            "SELECT level,ax,ay,bx,by,height,bulge,top_profile FROM wall WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()?
        .map_or_else(|| invalid(format!("no wall {id}")), Ok)?;
    let a = v2(ax, ay);
    let len = v2(bx - ax, by - ay).len();
    if len <= 0.0 || h <= 0.0 {
        return invalid(format!("wall {id}: degenerate"));
    }
    let dir = v2(bx - ax, by - ay) * (1.0 / len);
    let nrm = v2(-dir.y, dir.x);
    let lw = crate::walls::level_walls(db, level, elev(db, level)?)?;
    let top = top
        .as_deref()
        .and_then(crate::walls::parse_top_profile)
        .filter(|t| !t.is_empty());
    let arc = (bulge != 0.0)
        .then(|| ddc_geom::arc::ArcAxis::new(a, v2(bx, by), bulge, ddc_geom::Tol::default()).ok())
        .flatten();
    let hmax = top
        .as_ref()
        .map_or(h, |t| t.iter().map(|p| p.1).fold(h, f64::max));
    let ops: Vec<(f64, f64, f64, f64)> = db
        .prepare_cached("SELECT along,width,sill,height FROM opening WHERE wall=?1 ORDER BY id")?
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    // across the wall: wider than any layer footprint, but only this wall's layers are cut
    let w = len + 10.0;
    let at = |u: f64, s: f64, z: f64| {
        let p = a + dir * u + nrm * s;
        v3(p.x, p.y, z)
    };
    let mut out = vec![];
    for (_, k, rings, z0, _) in lw.layers.into_iter().filter(|l| l.0 == id) {
        let mut rings: Vec<Vec<V2>> = rings.into_iter().filter(|r| r.len() >= 3).collect();
        rings.sort_by(|p, q| poly::area(q).abs().total_cmp(&poly::area(p).abs()));
        let Some(first) = rings.first() else {
            continue;
        };
        let sign = poly::area(first) > 0.0;
        let (mut body, mut holes) = (Body::default(), Body::default());
        for r in &rings {
            let p = gm::extrude(r, z0, hmax);
            if (poly::area(r) > 0.0) == sign {
                body = body.union(&p, TOL);
            } else {
                holes = holes.union(&p, TOL);
            }
        }
        if !holes.is_empty() {
            body = body.subtract(&holes, TOL);
        }
        if let (Some(t), Some(ax)) = (&top, &arc) {
            body = arc_trim(&body, ax, t, z0, hmax, w);
        } else if let Some(t) = &top {
            // keep what is under the top polyline (heights over the wall base)
            let (u0, u1) = (-len - 10.0, 2.0 * len + 10.0);
            let mut ring = vec![at(u0, -w, z0 - 1.0), at(u1, -w, z0 - 1.0)];
            ring.push(at(u1, -w, z0 + t.last().map_or(h, |p| p.1)));
            ring.extend(t.iter().rev().map(|p| at(p.0, -w, z0 + p.1)));
            ring.push(at(u0, -w, z0 + t.first().map_or(h, |p| p.1)));
            ring.dedup_by(|p, q| (*p - *q).len() < 1e-9);
            let keep = gm::prism(&ring, v3(nrm.x, nrm.y, 0.0) * (2.0 * w));
            body = body.boolean(&keep, BoolOp::Intersect, TOL);
        } else if hmax > h + 1e-12 {
            body = body.boolean(
                &gm::cuboid(v3(-1e5, -1e5, z0 - 1.0), v3(1e5, 1e5, z0 + h)),
                BoolOp::Intersect,
                TOL,
            );
        }
        for &(along, width, sill, oh) in &ops {
            if width <= 0.0 || oh <= 0.0 {
                continue;
            }
            let r = [
                at(along, -w, 0.0),
                at(along + width, -w, 0.0),
                at(along + width, w, 0.0),
                at(along, w, 0.0),
            ];
            let r2: Vec<V2> = r.iter().map(|p| v2(p.x, p.y)).collect();
            body = body.subtract(&gm::extrude(&r2, z0 + sill, oh), TOL);
        }
        out.push((k, body));
    }
    if out.is_empty() {
        return invalid(format!("wall {id}: no footprint"));
    }
    Ok(out)
}

/// Element solids by layer: form, floor (layers along the slope, with holes), wall (layers
/// after joints, openings, top by profile), curtain wall, column and beam (after joints),
/// roof.
fn element_parts(db: &Connection, kind: &str, id: i64) -> Result<Vec<ElementPart>, Error> {
    let mesh = |m: ddc_geom::mesh::Mesh| vec![(0, Body::from_mesh(&m, 0))];
    Ok(match kind {
        "mass" => vec![(0, world(db, &read(db, id)?)?)],
        "slab" => crate::slabs::layer_meshes(db, id)?
            .into_iter()
            .map(|(k, m)| (k, Body::from_mesh(&m, 0)))
            .collect(),
        "wall" => wall_parts(db, id)?,
        "curtain_wall" => mesh(crate::curtain::mesh_db(db, id)?),
        "col" | "beam" => mesh(crate::structure::member_mesh_db(db, member_kind(kind), id)?),
        "roof" => mesh(crate::roofs::mesh_db(db, id)?),
        _ => return invalid(format!("join: unsupported kind {kind}")),
    })
}

/// All element parts as one solid (layers side by side; the face tag is the layer position).
fn concat(parts: Vec<ElementPart>, keep_tags: bool) -> Body {
    let mut polys = vec![];
    for (k, b) in parts {
        polys.extend(b.polys.into_iter().map(|mut p| {
            if !keep_tags {
                p.tag = k.max(0) as u32;
            }
            p
        }));
    }
    Body { polys }
}

fn element_body(db: &Connection, kind: &str, id: i64) -> Result<Body, Error> {
    Ok(concat(element_parts(db, kind, id)?, kind == "mass"))
}

/// Category priority in a join: the greater cuts the lesser.
pub fn join_priority(kind: &str) -> i32 {
    match kind {
        "col" => 50,
        "beam" => 40,
        "wall" => 30,
        "slab" => 20,
        "roof" => 15,
        "mass" => 10,
        _ => 0,
    }
}

fn is_void(db: &Connection, kind: &str, id: i64) -> Result<bool, Error> {
    if kind != "mass" {
        return Ok(false);
    }
    Ok(db
        .query_row("SELECT void FROM mass WHERE id=?1", [id], |r| {
            r.get::<_, i64>(0)
        })
        .optional()?
        .unwrap_or(0)
        != 0)
}

#[allow(clippy::type_complexity)]
fn join_rows(db: &Connection) -> Result<Vec<((String, i64), (String, i64))>, Error> {
    let mut st =
        db.prepare("SELECT winner_kind,winner,loser_kind,loser FROM geom_join ORDER BY rowid")?;
    let r = st.query_map([], |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?))))?;
    Ok(r.collect::<Result<_, _>>()?)
}

/// Who cuts the element: join winners and any voids joined to it.
fn cutters(db: &Connection, kind: &str, id: i64) -> Result<Vec<(String, i64)>, Error> {
    let mut out = vec![];
    for (w, l) in join_rows(db)? {
        if l.0 == kind && l.1 == id && !is_void(db, kind, id)? {
            out.push(w);
        } else if w.0 == kind && w.1 == id && is_void(db, &l.0, l.1)? {
            out.push(l);
        }
    }
    Ok(out)
}

/// Parts of all solids cutting the element.
fn cutter_parts(db: &Connection, kind: &str, id: i64) -> Result<Vec<Body>, Error> {
    let mut out = vec![];
    for (k, i) in cutters(db, kind, id)? {
        out.extend(element_parts(db, &k, i)?.into_iter().map(|p| p.1));
    }
    Ok(out)
}

/// Element layers after cuts: each layer minus all parts of the cutting solids.
fn joined_parts(db: &Connection, kind: &str, id: i64) -> Result<Vec<ElementPart>, Error> {
    let cut = cutter_parts(db, kind, id)?;
    let mut parts = element_parts(db, kind, id)?;
    for (_, b) in &mut parts {
        let fresh = b.max_tag() + 1;
        for c in &cut {
            if b.is_empty() {
                break;
            }
            *b = b.subtract(&c.clone().retagged(fresh), TOL);
        }
    }
    Ok(parts)
}

fn joined_db(db: &Connection, kind: &str, id: i64) -> Result<Body, Error> {
    Ok(concat(joined_parts(db, kind, id)?, kind == "mass"))
}

fn classify(n: V3) -> FaceClass {
    if n.z.abs() < 1e-6 {
        FaceClass::Vertical
    } else if n.z > 1.0 - 1e-6 {
        FaceClass::Up
    } else if n.z < -1.0 + 1e-6 {
        FaceClass::Down
    } else {
        FaceClass::Sloped
    }
}

fn faces_of(db: &Connection, id: i64, b: &Body) -> Result<Vec<MassFace>, Error> {
    let mut out = vec![];
    let mut st = db.prepare_cached("SELECT material FROM mass_paint WHERE mass=?1 AND face=?2")?;
    for tag in b.tags() {
        let plane = b.face_plane(tag, 1e-6);
        let n = match plane {
            Some(p) => p.n,
            None => b
                .polys
                .iter()
                .filter(|p| p.tag == tag)
                .fold(v3(0., 0., 0.), |s, p| s + p.plane.n * p.area())
                .normalized(),
        };
        let material = st.query_row(params![id, tag], |r| r.get(0)).optional()?;
        out.push(MassFace {
            tag,
            normal: (n.x, n.y, n.z),
            area: b.tag_area(tag),
            class: if plane.is_some() {
                classify(n)
            } else {
                FaceClass::Curved
            },
            outline: b
                .face_outline(tag, 1e-7)
                .into_iter()
                .map(|r| r.iter().map(|p| (p.x, p.y, p.z)).collect())
                .collect(),
            material,
        });
    }
    Ok(out)
}

/// Wall axis from a vertical face: the bottom edge (normal on the right along travel), base and height.
fn wall_line(f: &MassFace) -> Option<((f64, f64), (f64, f64), f64, f64)> {
    if f.class != FaceClass::Vertical {
        return None;
    }
    let pts: Vec<&(f64, f64, f64)> = f.outline.iter().flatten().collect();
    let z0 = pts.iter().map(|p| p.2).fold(f64::MAX, f64::min);
    let z1 = pts.iter().map(|p| p.2).fold(f64::MIN, f64::max);
    let dir = v2(-f.normal.1, f.normal.0);
    let s = |p: &&(f64, f64, f64)| v2(p.0, p.1).dot(dir);
    let a = pts.iter().min_by(|p, q| s(p).total_cmp(&s(q)))?;
    let b = pts.iter().max_by(|p, q| s(p).total_cmp(&s(q)))?;
    (z1 - z0 > 1e-6 && (s(&b) - s(&a)) > 1e-6).then_some(((a.0, a.1), (b.0, b.1), z0, z1 - z0))
}

type Ring = Vec<(f64, f64)>;
/// Plan island: an outer contour (counter-clockwise) and its holes (clockwise).
type Island = (Ring, Vec<Ring>);

/// Islands from contours: even nesting depth is an outer contour, odd is a hole of the
/// nearest enclosing one; by descending area. Two towers on one level are two islands.
fn islands(rings: Vec<Vec<V2>>) -> Vec<Island> {
    let mut rings: Vec<Vec<V2>> = rings
        .into_iter()
        .filter(|r| r.len() >= 3 && poly::area(r).abs() > 1e-9)
        .collect();
    rings.sort_by(|a, b| poly::area(b).abs().total_cmp(&poly::area(a).abs()));
    let n = rings.len();
    let probe = |r: &[V2]| (r[0] + r[1]) * 0.5;
    let within =
        |i: usize, j: usize| poly::contains(std::slice::from_ref(&rings[j]), probe(&rings[i]));
    let depth: Vec<usize> = (0..n)
        .map(|i| (0..i).filter(|&j| within(i, j)).count())
        .collect();
    let tup = |r: &[V2], ccw: bool| -> Ring {
        let mut r = r.to_vec();
        if (poly::area(&r) > 0.0) != ccw {
            r.reverse();
        }
        r.into_iter().map(|p| (p.x, p.y)).collect()
    };
    let mut out: Vec<(usize, Island)> = (0..n)
        .filter(|&i| depth[i] % 2 == 0)
        .map(|i| (i, (tup(&rings[i], true), vec![])))
        .collect();
    for i in (0..n).filter(|&i| depth[i] % 2 == 1) {
        let parent = (0..i)
            .rev()
            .find(|&j| depth[j] + 1 == depth[i] && within(i, j));
        if let Some(o) = parent.and_then(|j| out.iter_mut().find(|o| o.0 == j)) {
            o.1 .1.push(tup(&rings[i], false));
        }
    }
    out.into_iter().map(|o| o.1).collect()
}

/// Plan islands of a face.
fn plan_rings(f: &MassFace) -> Vec<Island> {
    islands(
        f.outline
            .iter()
            .map(|r| r.iter().map(|p| v2(p.0, p.1)).collect())
            .collect(),
    )
}

/// Number of separate parts of a flat face (outer contours in its plane).
fn face_parts(f: &MassFace) -> usize {
    let n = v3(f.normal.0, f.normal.1, f.normal.2);
    f.outline
        .iter()
        .filter(|r| {
            let r: Vec<V3> = r.iter().map(|p| v3(p.0, p.1, p.2)).collect();
            let mut s = v3(0., 0., 0.);
            for i in 0..r.len() {
                s = s + r[i].cross(r[(i + 1) % r.len()]);
            }
            s.dot(n) > 1e-9
        })
        .count()
}

fn ring_centroid(r: &[(f64, f64)]) -> V2 {
    poly::centroid(&r.iter().map(|p| v2(p.0, p.1)).collect::<Vec<V2>>())
}

/// The island nearest to the element's current contour (`None` if there are no islands or the nearest one
/// is already taken by another element of the same face: ambiguous).
fn pick_island(isl: &[Island], current: Option<V2>, taken: &[usize]) -> Option<usize> {
    let c = current.unwrap_or(v2(0., 0.));
    let i = (0..isl.len()).min_by(|&a, &b| {
        let da = (ring_centroid(&isl[a].0) - c).len();
        let db = (ring_centroid(&isl[b].0) - c).len();
        da.total_cmp(&db)
    })?;
    (!taken.contains(&i)).then_some(i)
}

/// Center of the element's outer contour in plan (floor, roof).
fn element_centroid(db: &Connection, kind: &str, id: i64) -> Result<Option<V2>, Error> {
    let sql = match kind {
        "slab" => "SELECT x,y FROM slab_vertex WHERE slab=?1 AND ring=0 ORDER BY idx",
        "roof" => "SELECT x,y FROM roof_vertex WHERE roof=?1 ORDER BY idx",
        _ => return Ok(None),
    };
    let r: Ring = db
        .prepare_cached(sql)?
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok((r.len() >= 3).then(|| ring_centroid(&r)))
}

/// Elevation of the element's own level (face offsets are measured from it).
fn host_elev(db: &Connection, kind: &str, id: i64) -> Result<f64, Error> {
    if !ELEMENT_TABLES.contains(&kind) {
        return invalid(format!("unknown element kind {kind}"));
    }
    let l: i64 = db.query_row(
        &format!("SELECT level FROM {kind} WHERE id=?1"),
        [id],
        |r| r.get(0),
    )?;
    elev(db, l)
}

/// Elevation of the face bottom.
fn face_zmin(f: &MassFace) -> f64 {
    f.outline
        .iter()
        .flatten()
        .map(|p| p.2)
        .fold(f64::MAX, f64::min)
}

/// Roof edge slopes for a face: a horizontal face gives every edge `flat_deg`; an inclined face gives
/// the face slope on the eave edges (at the lowest elevation; if the face bottom is a single vertex, on the
/// edges nearest to the bottom), the rest are gables.
fn roof_slopes(f: &MassFace, ring: &[(f64, f64)], flat_deg: f64) -> Vec<Option<f64>> {
    if f.class == FaceClass::Up {
        return vec![Some(flat_deg); ring.len()];
    }
    let n = f.normal;
    let deg = n.2.clamp(-1.0, 1.0).acos().to_degrees();
    let p0 = f.outline[0][0];
    let z = |p: (f64, f64)| p0.2 - (n.0 * (p.0 - p0.0) + n.1 * (p.1 - p0.1)) / n.2;
    let zmin = face_zmin(f);
    let k = ring.len();
    let edge_z = |i: usize| (z(ring[i]) + z(ring[(i + 1) % k])) * 0.5;
    let mut out: Vec<Option<f64>> = (0..k)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % k]);
            ((z(a) - zmin).abs() < 1e-6 && (z(b) - zmin).abs() < 1e-6).then_some(deg)
        })
        .collect();
    if out.iter().all(Option::is_none) && k > 0 {
        let low = (0..k).map(edge_z).fold(f64::MAX, f64::min);
        for (i, o) in out.iter_mut().enumerate() {
            if (edge_z(i) - low).abs() < 1e-6 {
                *o = Some(deg);
            }
        }
    }
    out
}

/// Kind of the operation that created the face (`None` if the face is not in the history).
fn face_sig(form: &Form, tag: u32) -> Option<&'static str> {
    let op = tag_op(tag) as usize;
    form.op_kinds().get(op.checked_sub(1)?).copied()
}

/// A face by its history tag for a measurement anchor: found only while the operation that made
/// it still has the recorded kind (`op`), so a renumbered neighbour never stands in for it.
/// `None`: the mass or the face is gone.
pub(crate) fn anchor_face(
    db: &Connection,
    mass: i64,
    tag: u32,
    op: &str,
) -> Result<Option<MassFace>, Error> {
    if !crate::measure::row_exists(db, "mass", mass)? {
        return Ok(None);
    }
    let s = read(db, mass)?;
    if face_sig(&s.form, tag) != Some(op) {
        return Ok(None);
    }
    let b = world(db, &s)?;
    Ok(faces_of(db, mass, &b)?.into_iter().find(|f| f.tag == tag))
}

/// The kind of the operation that made a face now (what a new anchor records).
pub(crate) fn anchor_face_op(db: &Connection, mass: i64, tag: u32) -> Result<Option<String>, Error> {
    if !crate::measure::row_exists(db, "mass", mass)? {
        return Ok(None);
    }
    Ok(face_sig(&read(db, mass)?.form, tag).map(str::to_string))
}

/// World body of a mass for measurements; `None` when the mass is gone.
pub(crate) fn anchor_body(db: &Connection, mass: i64) -> Result<Option<Body>, Error> {
    if !crate::measure::row_exists(db, "mass", mass)? {
        return Ok(None);
    }
    Ok(Some(world(db, &read(db, mass)?)?))
}

fn face(db: &Connection, mass: i64, tag: u32) -> Result<(MassSpec, MassFace), Error> {
    let s = read(db, mass)?;
    let b = world(db, &s)?;
    let f = faces_of(db, mass, &b)?
        .into_iter()
        .find(|f| f.tag == tag)
        .map_or_else(|| invalid(format!("mass {mass}: no face {tag}")), Ok)?;
    Ok((s, f))
}

fn set_slab(
    db: &Connection,
    inv: Inv,
    id: i64,
    offset: Option<f64>,
    outer: &[(f64, f64)],
    holes: &[Ring],
) -> Result<Inv, Error> {
    let inv = inv
        .delete("slab_vertex", "slab", id)
        .restore(db, "slab", "id", id)?
        .restore(db, "slab_vertex", "slab", id)?;
    if let Some(o) = offset {
        db.execute("UPDATE slab SET offset=?1 WHERE id=?2", params![o, id])?;
    }
    db.execute("DELETE FROM slab_vertex WHERE slab=?1", [id])?;
    for (k, r) in std::iter::once(outer)
        .chain(holes.iter().map(|h| h.as_slice()))
        .enumerate()
    {
        for (i, p) in r.iter().enumerate() {
            db.execute(
                "INSERT INTO slab_vertex VALUES (?1,?2,?3,?4,?5)",
                params![id, k as i64, i as i64, p.0, p.1],
            )?;
        }
    }
    Ok(inv)
}

/// Element on face `f`; `false` if the face no longer suits this element or is
/// ambiguous (split into parts that cannot be matched unambiguously). `taken` is the
/// set of face islands already occupied by other elements.
fn sync_face(
    db: &Connection,
    inv: Inv,
    kind: &str,
    id: i64,
    f: &MassFace,
    taken: &mut Vec<usize>,
) -> Result<(Inv, bool), Error> {
    let e = host_elev(db, kind, id)?;
    match kind {
        "wall" | "curtain_wall" => {
            let line = wall_line(f).filter(|_| face_parts(f) == 1);
            let Some((a, b, z0, h)) = line else {
                return Ok((inv, false));
            };
            let inv = inv.restore(db, kind, "id", id)?;
            db.execute(
                &format!("UPDATE {kind} SET ax=?1,ay=?2,bx=?3,by=?4,base_offset=?5,height=?6 WHERE id=?7"),
                params![a.0, a.1, b.0, b.1, z0 - e, h, id],
            )?;
            Ok((inv, true))
        }
        "slab" => {
            if !matches!(f.class, FaceClass::Up | FaceClass::Down) {
                return Ok((inv, false));
            }
            let isl = plan_rings(f);
            let Some(k) = pick_island(&isl, element_centroid(db, kind, id)?, taken) else {
                return Ok((inv, false));
            };
            taken.push(k);
            let (outer, holes) = &isl[k];
            let inv = set_slab(db, inv, id, Some(face_zmin(f) - e), outer, holes)?;
            Ok((inv, true))
        }
        "roof" => {
            if !(matches!(f.class, FaceClass::Up | FaceClass::Sloped) && f.normal.2 > 0.0) {
                return Ok((inv, false));
            }
            let isl = plan_rings(f);
            let Some(k) = pick_island(&isl, element_centroid(db, kind, id)?, taken) else {
                return Ok((inv, false));
            };
            taken.push(k);
            let ring = &isl[k].0;
            let flat: Option<f64> = db
                .query_row(
                    "SELECT slope FROM roof_vertex WHERE roof=?1 ORDER BY idx",
                    [id],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let slopes = roof_slopes(f, ring, flat.unwrap_or(0.0));
            let inv = inv
                .delete("roof_vertex", "roof", id)
                .restore(db, "roof", "id", id)?
                .restore(db, "roof_vertex", "roof", id)?;
            db.execute(
                "UPDATE roof SET offset=?1 WHERE id=?2",
                params![face_zmin(f) - e, id],
            )?;
            db.execute("DELETE FROM roof_vertex WHERE roof=?1", [id])?;
            for (i, (p, s)) in ring.iter().zip(&slopes).enumerate() {
                db.execute(
                    "INSERT INTO roof_vertex VALUES (?1,?2,?3,?4,?5)",
                    params![id, i as i64, p.0, p.1, s],
                )?;
            }
            Ok((inv, true))
        }
        _ => Ok((inv, false)),
    }
}

/// Islands of a form floor at elevation z.
fn floor_rings(b: &Body, z: f64) -> Vec<Island> {
    islands(gm::section_rings(b, z + 1e-9))
}

fn hosts_of(db: &Connection, mass: i64) -> Result<Vec<MassHost>, Error> {
    let mut st = db.prepare_cached(
        "SELECT kind,ref,mass,face,level,orphan FROM mass_host WHERE mass=?1 ORDER BY kind,ref",
    )?;
    let r = st.query_map([mass], |r| {
        Ok(MassHost {
            kind: r.get(0)?,
            id: r.get(1)?,
            mass: r.get(2)?,
            face: r.get(3)?,
            floor_level: r.get(4)?,
            orphan: r.get::<_, i64>(5)? != 0,
        })
    })?;
    Ok(r.collect::<Result<_, _>>()?)
}

/// Resync of form elements (one solid evaluation): a face is found by tag and by the kind of the
/// operation that created it, a part of a face (floor) by the nearest contour; a missing or
/// ambiguous face leaves the binding orphaned and the element untouched.
fn sync_hosts(db: &Connection, mass: i64, mut inv: Inv) -> Result<Inv, Error> {
    let hs = hosts_of(db, mass)?;
    if hs.is_empty() {
        return Ok(inv);
    }
    let s = read(db, mass)?;
    let body = world(db, &s).ok();
    let faces = match &body {
        Some(b) => faces_of(db, mass, b)?,
        None => vec![],
    };
    let sigs: Vec<(String, i64, Option<String>)> = db
        .prepare_cached("SELECT kind,ref,sig FROM mass_host WHERE mass=?1")?
        .query_map([mass], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    // occupied islands per (kind, face, floor level): a slab and a roof share a top face
    let mut taken: Vec<((String, u32, Option<i64>), Vec<usize>)> = vec![];
    for h in hs {
        // a pinned element stays in place and does not fail the form or level edit
        if crate::groups::is_pinned_in(db, &h.kind, h.id)? {
            continue;
        }
        let slot = (h.kind.clone(), h.face, h.floor_level);
        let i = match taken.iter().position(|t| t.0 == slot) {
            Some(i) => i,
            None => {
                taken.push((slot, vec![]));
                taken.len() - 1
            }
        };
        let used = &mut taken[i].1;
        let ok;
        (inv, ok) = match (h.floor_level, &body) {
            (Some(l), Some(b)) if h.kind == "slab" => {
                let isl = floor_rings(b, elev(db, l)?);
                match pick_island(&isl, element_centroid(db, "slab", h.id)?, used) {
                    Some(k) => {
                        used.push(k);
                        (set_slab(db, inv, h.id, None, &isl[k].0, &isl[k].1)?, true)
                    }
                    None => (inv, false),
                }
            }
            (Some(_), _) => (inv, false),
            (None, _) => {
                let sig = sigs
                    .iter()
                    .find(|x| x.0 == h.kind && x.1 == h.id)
                    .and_then(|x| x.2.clone());
                let same = face_sig(&s.form, h.face).map(str::to_string) == sig || sig.is_none();
                match faces.iter().find(|f| f.tag == h.face).filter(|_| same) {
                    Some(f) => sync_face(db, inv, &h.kind, h.id, f, used)?,
                    None => (inv, false),
                }
            }
        };
        if ok == h.orphan {
            inv = inv.sql(
                "UPDATE mass_host SET orphan=?1 WHERE kind=?2 AND ref=?3",
                vec![(h.orphan as i64).into(), h.kind.clone().into(), h.id.into()],
            );
            db.execute(
                "UPDATE mass_host SET orphan=?1 WHERE kind=?2 AND ref=?3",
                params![(!ok) as i64, h.kind, h.id],
            )?;
        }
    }
    Ok(inv)
}

/// All forms with bound elements (after level elevations change).
fn sync_all(db: &Connection, mut inv: Inv) -> Result<Inv, Error> {
    let ids: Vec<i64> = db
        .prepare("SELECT DISTINCT mass FROM mass_host ORDER BY mass")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for id in ids {
        inv = sync_hosts(db, id, inv)?;
    }
    Ok(inv)
}

/// Element tables referenced by bindings and joins.
const ELEMENT_TABLES: [&str; 7] = [
    "wall",
    "slab",
    "roof",
    "curtain_wall",
    "col",
    "beam",
    "mass",
];

fn drop_rows(db: &Connection, mut inv: Inv, table: &str, sql: &str, k: &str) -> Result<Inv, Error> {
    let rows: Vec<i64> = db
        .prepare(sql)?
        .query_map([k], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for r in rows {
        inv = inv.restore(db, table, "rowid", r)?;
        db.execute(&format!("DELETE FROM {table} WHERE rowid=?1"), [r])?;
    }
    Ok(inv)
}

/// After every operation: bindings and joins of deleted elements are removed (undo restores
/// them) so a new element with the same id does not inherit them; a change of level elevations
/// rebuilds the elements from their forms.
pub(crate) fn after_op(db: &Connection, kind: &str, mut inv: Inv) -> Result<Inv, Error> {
    let any: i64 = db.query_row(
        "SELECT (SELECT count(*) FROM mass_host)+(SELECT count(*) FROM geom_join)\
         +(SELECT count(*) FROM mass_paint)",
        [],
        |r| r.get(0),
    )?;
    if any == 0 {
        return Ok(inv);
    }
    let moved = levels_changed(db)?;
    if moved || kind.starts_with("level") {
        inv = sync_all(db, inv)?;
    }
    for t in ELEMENT_TABLES {
        let q = format!(
            "SELECT rowid FROM mass_host WHERE kind=?1 AND ref NOT IN (SELECT id FROM {t})"
        );
        inv = drop_rows(db, inv, "mass_host", &q, t)?;
        for c in ["winner", "loser"] {
            let q = format!(
                "SELECT rowid FROM geom_join WHERE {c}_kind=?1 AND {c} NOT IN (SELECT id FROM {t})"
            );
            inv = drop_rows(db, inv, "geom_join", &q, t)?;
        }
    }
    for t in ["mass_host", "mass_paint"] {
        let q = format!("SELECT rowid FROM {t} WHERE mass NOT IN (SELECT id FROM mass) AND ?1=?1");
        inv = drop_rows(db, inv, t, &q, "")?;
    }
    Ok(inv)
}

fn cache_key(form: &str, level: f64, zs: &[f64]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    form.hash(&mut h);
    level.to_bits().hash(&mut h);
    zs.iter().for_each(|z| z.to_bits().hash(&mut h));
    format!("{:016x}", h.finish())
}

/// Form floors (only changed forms, by history and elevation key) and the volumes of elements
/// cut by joins.
pub(crate) fn regenerate(db: &Connection) -> Result<(), Error> {
    db.execute(
        "DELETE FROM mass_qty WHERE mass NOT IN (SELECT id FROM mass)",
        [],
    )?;
    db.execute(
        "DELETE FROM mass_cache WHERE mass NOT IN (SELECT id FROM mass)",
        [],
    )?;
    let lv = levels(db)?;
    let zs: Vec<f64> = lv.iter().map(|l| l.1).collect();
    let rows: Vec<(i64, i64, String, i64)> = db
        .prepare("SELECT id,level,form,void FROM mass ORDER BY id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    for (id, level, form, void) in rows {
        let le = lv.iter().find(|l| l.0 == level).map_or(f64::NAN, |l| l.1);
        let key = cache_key(&format!("{form}{void}"), le, &zs);
        let old: Option<String> = db
            .query_row("SELECT key FROM mass_cache WHERE mass=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        if old.as_deref() == Some(key.as_str()) {
            continue;
        }
        db.execute("DELETE FROM mass_qty WHERE mass=?1", [id])?;
        if void == 0 {
            if let Ok(b) = read(db, id).and_then(|s| world(db, &s)) {
                for (z, a, p) in gm::floors(&b, &zs) {
                    if let Some(l) = lv.iter().find(|l| l.1 == z) {
                        db.execute(
                            "INSERT OR REPLACE INTO mass_qty VALUES (?1,?2,?3,?4)",
                            params![id, l.0, a, p],
                        )?;
                    }
                }
            }
        }
        db.execute(
            "INSERT OR REPLACE INTO mass_cache VALUES (?1,?2)",
            params![id, key],
        )?;
    }
    join_quantities(db)
}

/// Query rows as text (for the cache key); parameter `?1` is the id, if the query has one.
fn dump(db: &Connection, sql: &str, id: i64, out: &mut String) -> Result<(), Error> {
    let mut st = db.prepare_cached(sql)?;
    let n = st.column_count();
    let mut rows = if st.parameter_count() > 0 {
        st.query([id])?
    } else {
        st.query([])?
    };
    while let Some(r) = rows.next()? {
        for i in 0..n {
            let v: rusqlite::types::Value = r.get(i)?;
            out.push_str(&format!("{v:?}|"));
        }
        out.push('\n');
    }
    Ok(())
}

/// Source data of an element's solid (everything it depends on), for the cut cache key.
fn element_sig(db: &Connection, kind: &str, id: i64) -> Result<String, Error> {
    let mut s = format!("{kind}:{id}\n");
    dump(db, "SELECT id,elevation FROM level ORDER BY id", id, &mut s)?;
    let qs: &[&str] = match kind {
        "mass" => &["SELECT form,void FROM mass WHERE id=?1"],
        "wall" => &[
            "SELECT * FROM wall WHERE level=(SELECT level FROM wall WHERE id=?1) ORDER BY id",
            "SELECT * FROM opening WHERE wall IN (SELECT id FROM wall WHERE level=\
             (SELECT level FROM wall WHERE id=?1)) ORDER BY id",
            "SELECT * FROM wall_type_layer ORDER BY wtype,pos",
        ],
        "slab" => &[
            "SELECT * FROM slab WHERE id=?1",
            "SELECT * FROM slab_vertex WHERE slab=?1 ORDER BY ring,idx",
            "SELECT * FROM slab_type_layer ORDER BY stype,pos",
        ],
        "roof" => &[
            "SELECT * FROM roof WHERE id=?1",
            "SELECT * FROM roof_vertex WHERE roof=?1 ORDER BY idx",
            "SELECT * FROM slab_type_layer ORDER BY stype,pos",
        ],
        "col" | "beam" => &[
            "SELECT * FROM col ORDER BY id",
            "SELECT * FROM beam ORDER BY id",
            "SELECT * FROM member_type ORDER BY id",
        ],
        "curtain_wall" => &[
            "SELECT * FROM curtain_wall WHERE id=?1",
            "SELECT * FROM curtain_panel WHERE cw=?1 ORDER BY i,j",
        ],
        _ => &[],
    };
    for q in qs {
        dump(db, q, id, &mut s)?;
    }
    Ok(s)
}

/// Level elevations changed since the last check (including by a batch edit).
fn levels_changed(db: &Connection) -> Result<bool, Error> {
    let mut s = String::new();
    dump(db, "SELECT id,elevation FROM level ORDER BY id", 0, &mut s)?;
    let key = cache_key(&s, 0.0, &[]);
    let old: Option<String> = db
        .query_row(
            "SELECT key FROM join_cache WHERE kind='levels' AND ref=0",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if old.as_deref() == Some(key.as_str()) {
        return Ok(false);
    }
    db.execute(
        "INSERT OR REPLACE INTO join_cache(kind,ref,key,ratios) VALUES ('levels',0,?1,'')",
        [&key],
    )?;
    Ok(old.is_some())
}

/// Volumes of join losers are reduced by the cut part, per layer (quantities of walls,
/// floors, roofs and members have just been fully recomputed). CSG runs only for
/// changed pairs: layer fractions are kept in `join_cache`, keyed by the element's source data and
/// that of its cutting solids.
fn join_quantities(db: &Connection) -> Result<(), Error> {
    let mut targets: Vec<(String, i64)> = vec![];
    for (w, l) in join_rows(db)? {
        let t = if is_void(db, &l.0, l.1)? { w } else { l };
        if !targets.contains(&t) {
            targets.push(t);
        }
    }
    let cached: Vec<(String, i64)> = db
        .prepare("SELECT kind,ref FROM join_cache WHERE kind<>'levels'")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for c in cached.iter().filter(|c| !targets.contains(c)) {
        db.execute(
            "DELETE FROM join_cache WHERE kind=?1 AND ref=?2",
            params![c.0, c.1],
        )?;
    }
    for (kind, id) in targets {
        let sql = match kind.as_str() {
            "wall" => "UPDATE wall_qty SET volume=volume*?1 WHERE wall=?2 AND pos=?3",
            "slab" => "UPDATE slab_qty SET volume=volume*?1 WHERE slab=?2 AND pos=?3",
            "roof" => "UPDATE roof_qty SET volume=volume*?1 WHERE roof=?2 AND ?3=?3",
            "col" | "beam" => "UPDATE member_qty SET volume=volume*?1 WHERE ref=?2 AND kind=?3",
            _ => continue,
        };
        let mut src = element_sig(db, &kind, id)?;
        for (k, i) in cutters(db, &kind, id)? {
            src.push_str(&element_sig(db, &k, i)?);
        }
        let key = cache_key(&src, 0.0, &[]);
        let hit: Option<(String, String)> = db
            .query_row(
                "SELECT key,ratios FROM join_cache WHERE kind=?1 AND ref=?2",
                params![kind, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let ratios: Vec<(i64, f64)> = match hit {
            Some((k, r)) if k == key => serde_json::from_str(&r).unwrap_or_default(),
            _ => {
                let (Ok(plain), Ok(cut)) =
                    (element_parts(db, &kind, id), joined_parts(db, &kind, id))
                else {
                    continue;
                };
                let r: Vec<(i64, f64)> = plain
                    .iter()
                    .zip(&cut)
                    .filter_map(|((k, p), (_, c))| {
                        let (p, c) = (p.volume(), c.volume());
                        (p > 1e-12 && (p - c).abs() > 1e-12).then(|| (*k, (c / p).clamp(0.0, 1.0)))
                    })
                    .collect();
                db.execute(
                    "INSERT OR REPLACE INTO join_cache(kind,ref,key,ratios) VALUES (?1,?2,?3,?4)",
                    params![kind, id, key, serde_json::to_string(&r)?],
                )?;
                r
            }
        };
        for (pos, r) in ratios {
            match kind.as_str() {
                "col" | "beam" => db.execute(sql, params![r, id, member_kind(&kind)])?,
                _ => db.execute(sql, params![r, id, pos])?,
            };
        }
    }
    Ok(())
}

/// Form warnings: orphaned element-to-face bindings.
pub(crate) fn warnings(db: &Connection) -> Result<Vec<crate::walls::ElementWarning>, Error> {
    let rows: Vec<(String, i64, i64, u32)> = db
        .prepare("SELECT kind,ref,mass,face FROM mass_host WHERE orphan=1 ORDER BY kind,ref")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    let mut out: Vec<crate::walls::ElementWarning> = rows
        .into_iter()
        .filter_map(|(k, id, mass, face)| {
            let table = ELEMENT_TABLES.into_iter().find(|t| *t == k)?;
            Some(crate::walls::ElementWarning {
                table,
                id,
                message: format!(
                    "mass {mass}: hosting face {face} no longer exists or is ambiguous, element left in place"
                ),
            })
        })
        .collect();
    let ids: Vec<i64> = db
        .prepare("SELECT id FROM mass ORDER BY id")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for id in ids {
        if let Err(e) = read(db, id).and_then(|s| world(db, &s)) {
            out.push(crate::walls::ElementWarning {
                table: "mass",
                id,
                message: format!("mass {id}: history does not build: {e}"),
            });
        }
    }
    Ok(out)
}

/// Shifts a form inside a batch of the modify module: the caller has snapshotted the mass rows;
/// the returned inverse restores what the hosted elements changed.
pub(crate) fn shift_in(db: &Connection, id: i64, d: (f64, f64, f64)) -> Result<Inv, Error> {
    if ![d.0, d.1, d.2].iter().all(|v| v.is_finite()) {
        return invalid("move mass: offset");
    }
    // a linked result is driven by its operands; moving it directly would be undone silently
    bool_link::guard_edit(db, id)?;
    let mut s = read(db, id)?;
    let old = s.form.clone();
    s.form = match s.form {
        Form::Move { base, d: d0 } => Form::Move {
            base,
            d: (d0.0 + d.0, d0.1 + d.1, d0.2 + d.2),
        },
        f => Form::Move {
            base: Box::new(f),
            d,
        },
    };
    let s = prepare(db, &s, Some(&old))?;
    write(db, &s, Some(id))?;
    sync_hosts(db, id, Inv::default())
}

/// After a batch moved the forms `ids` with `shift_in`: recompute the boolean results that use
/// them once, so a batch moving both operands never sees a half-moved pair.
pub(crate) fn follow_shifted(db: &Connection, ids: &[i64]) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    for &id in ids {
        inv = bool_link::follow(db, id, inv)?;
    }
    Ok(inv)
}

impl Model {
    pub fn add_mass(&mut self, s: &MassSpec) -> Result<i64, Error> {
        let s = &prepare(&self.db, s, None)?;
        self.op("mass_add", |db| {
            let id = write(db, s, None)?;
            let inv = Inv::default()
                .delete("mass_paint", "mass", id)
                .delete("mass", "id", id);
            Ok((id, id, inv))
        })
    }

    pub fn mass(&self, id: i64) -> Result<MassSpec, Error> {
        read(&self.db, id)
    }

    pub fn masses(&self) -> Result<Vec<i64>, Error> {
        let mut st = self.db.prepare("SELECT id FROM mass ORDER BY id")?;
        let r = st.query_map([], |r| r.get(0))?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    /// New form history; face- and floor-hosted elements are recomputed in the same operation.
    pub fn update_mass(&mut self, id: i64, s: &MassSpec) -> Result<(), Error> {
        bool_link::guard_edit(&self.db, id)?;
        let old = read(&self.db, id)?;
        let s = &prepare(&self.db, s, Some(&old.form))?;
        self.op("mass_edit", |db| {
            let inv = Inv::default().restore(db, "mass", "id", id)?;
            read(db, id)?;
            write(db, s, Some(id))?;
            let inv = sync_hosts(db, id, inv)?;
            let inv = bool_link::follow(db, id, inv)?;
            Ok(((), id, inv))
        })
    }

    /// Append a history step on top of the current form (push-pull, fillet, ...).
    pub fn edit_mass_form(
        &mut self,
        id: i64,
        f: impl FnOnce(Box<Form>) -> Form,
    ) -> Result<(), Error> {
        let mut s = self.mass(id)?;
        s.form = f(Box::new(s.form));
        self.update_mass(id, &s)
    }

    /// Number of the operation the next history step will get (for new face tags).
    pub fn next_mass_op(&self, id: i64) -> Result<u32, Error> {
        Ok(self.mass(id)?.form.root_op() + 1)
    }

    /// Push-pull preview without writing to the model (live display while dragging).
    pub fn preview_push_pull(&self, id: i64, face: u32, d: f64) -> Result<Body, Error> {
        let s = self.mass(id)?;
        let f = Form::PushPull {
            base: Box::new(s.form),
            face,
            d,
            keys: vec![],
        };
        Ok(f.eval()?.translated(v3(0., 0., elev(&self.db, s.level)?)))
    }

    /// Translates a form (one operation; face- and floor-hosted elements move with it).
    pub fn move_mass(&mut self, id: i64, d: (f64, f64, f64)) -> Result<(), Error> {
        if ![d.0, d.1, d.2].iter().all(|v| v.is_finite()) {
            return invalid("move mass: offset");
        }
        let mut s = self.mass(id)?;
        s.form = match s.form {
            Form::Move { base, d: d0 } => Form::Move {
                base,
                d: (d0.0 + d.0, d0.1 + d.1, d0.2 + d.2),
            },
            f => Form::Move {
                base: Box::new(f),
                d,
            },
        };
        self.update_mass(id, &s)
    }

    pub fn delete_mass(&mut self, id: i64) -> Result<(), Error> {
        self.op("mass_delete", |db| {
            let mut inv = Inv::default();
            for t in ["mass", "mass_paint", "mass_host"] {
                let col = if t == "mass" { "id" } else { "mass" };
                inv = inv.restore(db, t, col, id)?;
                db.execute(&format!("DELETE FROM {t} WHERE {col}=?1"), [id])?;
            }
            inv = bool_link::unlink(db, id, inv)?;
            inv = crate::recipe::unlink(db, "mass", id, inv)?;
            // derived rows: `regenerate` recomputes them on undo; restoring by rowid
            // would overwrite rows of other forms that the recompute renumbered
            db.execute("DELETE FROM mass_qty WHERE mass=?1", [id])?;
            Ok(((), id, inv))
        })
    }

    /// Boolean operation on two forms of one level: `a` gets the history `a op b`, `b` is deleted;
    /// one undo step.
    pub fn combine_masses(&mut self, a: i64, b: i64, op: BoolOp) -> Result<(), Error> {
        let (sa, sb) = (self.mass(a)?, self.mass(b)?);
        if a == b || sa.level != sb.level {
            return invalid("forms must differ and share a level");
        }
        // b's operations are numbered after a's inside the boolean
        let shift = sa.form.root_op();
        self.compound("mass_combine", |m| {
            m.op("mass_rehost", |db| {
                let inv = Inv::default()
                    .restore(db, "mass_host", "mass", b)?
                    .restore(db, "mass_paint", "mass", b)?;
                for t in ["mass_host", "mass_paint"] {
                    let rows: Vec<(i64, u32)> = db
                        .prepare(&format!("SELECT rowid,face FROM {t} WHERE mass=?1"))?
                        .query_map([b], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<Result<_, _>>()?;
                    for (row, face) in rows {
                        let f = if face == 0 {
                            0
                        } else {
                            ftag(tag_op(face) + shift, face & ((1 << LOCAL_BITS) - 1))
                        };
                        db.execute(
                            &format!("UPDATE {t} SET mass=?1,face=?2 WHERE rowid=?3"),
                            params![a, f, row],
                        )?;
                    }
                }
                Ok(((), a, inv))
            })?;
            let mut s = sa;
            s.form = Form::Bool {
                op,
                a: Box::new(s.form),
                b: Box::new(sb.form),
            };
            m.update_mass(a, &s)?;
            m.delete_mass(b)
        })
    }

    /// Form solid in world coordinates.
    pub fn mass_body(&self, id: i64) -> Result<Body, Error> {
        world(&self.db, &self.mass(id)?)
    }

    pub fn mass_faces(&self, id: i64) -> Result<Vec<MassFace>, Error> {
        let b = self.mass_body(id)?;
        faces_of(&self.db, id, &b)
    }

    /// Paints a face with a material (None clears it).
    pub fn paint_face(
        &mut self,
        mass: i64,
        face: u32,
        material: Option<&str>,
    ) -> Result<(), Error> {
        if !self.mass_body(mass)?.polys.iter().any(|p| p.tag == face) {
            return invalid(format!("mass {mass}: no face {face}"));
        }
        self.op("mass_paint", |db| {
            let inv = Inv::default()
                .sql(
                    "DELETE FROM mass_paint WHERE mass=?1 AND face=?2",
                    vec![mass.into(), (face as i64).into()],
                )
                .restore(db, "mass_paint", "mass", mass)?;
            db.execute(
                "DELETE FROM mass_paint WHERE mass=?1 AND face=?2",
                params![mass, face],
            )?;
            if let Some(m) = material {
                db.execute(
                    "INSERT INTO mass_paint VALUES (?1,?2,?3)",
                    params![mass, face, m],
                )?;
            }
            Ok(((), mass, inv))
        })
    }

    fn link_host(
        &mut self,
        kind: &str,
        id: i64,
        mass: i64,
        face: u32,
        floor: Option<i64>,
    ) -> Result<(), Error> {
        let s = self.mass(mass)?;
        let sig = floor.is_none().then(|| face_sig(&s.form, face)).flatten();
        let role = if floor.is_some() { "floor" } else { "face" };
        self.op("mass_host", |db| {
            db.execute(
                "INSERT OR REPLACE INTO mass_host(kind,ref,mass,face,role,level,sig,orphan) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,0)",
                params![kind, id, mass, face, role, floor, sig],
            )?;
            let inv = Inv::default().sql(
                "DELETE FROM mass_host WHERE kind=?1 AND ref=?2",
                vec![kind.to_string().into(), id.into()],
            );
            Ok(((), id, inv))
        })
    }

    /// Element binding to a form face: (form, face).
    pub fn host_face(&self, kind: &str, id: i64) -> Result<Option<(i64, u32)>, Error> {
        Ok(self
            .db
            .query_row(
                "SELECT mass,face FROM mass_host WHERE kind=?1 AND ref=?2",
                params![kind, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// All element bindings to a form.
    pub fn mass_hosts(&self, mass: i64) -> Result<Vec<MassHost>, Error> {
        hosts_of(&self.db, mass)
    }

    /// Several model operations as one (one undo step); on error everything is rolled back.
    pub(crate) fn compound<T>(
        &mut self,
        kind: &str,
        f: impl FnOnce(&mut Model) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let top = |m: &Model| -> Result<i64, Error> {
            Ok(m.db.query_row("SELECT coalesce(max(seq),0) FROM op", [], |r| r.get(0))?)
        };
        let before = top(self)?;
        match f(self) {
            Ok(v) => {
                crate::ops::merge_ops(&mut self.db, kind, before)?;
                Ok(v)
            }
            Err(e) => {
                while top(self)? > before {
                    self.undo()?;
                }
                Err(e)
            }
        }
    }

    fn planar_face(&self, mass: i64, tag: u32) -> Result<(MassSpec, MassFace, f64), Error> {
        let (s, f) = face(&self.db, mass, tag)?;
        if f.class == FaceClass::Curved {
            return invalid(format!("mass {mass}: face {tag} is curved"));
        }
        let e = elev(&self.db, s.level)?;
        Ok((s, f, e))
    }

    /// Wall on a vertical form face (axis, base and height all follow the face).
    pub fn wall_by_face(&mut self, mass: i64, tag: u32, wall_type: i64) -> Result<i64, Error> {
        let (s, f, e) = self.planar_face(mass, tag)?;
        if face_parts(&f) > 1 {
            return invalid("wall by face: face is split into several parts");
        }
        let (a, b, z0, h) =
            wall_line(&f).map_or_else(|| invalid("wall by face: face is not vertical"), Ok)?;
        self.compound("mass_wall_by_face", |m| {
            let id = m.add_wall(&WallSpec {
                a,
                b,
                level: s.level,
                base_offset: z0 - e,
                height: h,
                wall_type,
                offset0: None,
            })?;
            m.link_host("wall", id, mass, tag, None)?;
            Ok(id)
        })
    }

    /// Curtain wall on a vertical form face (grid with step `spacing`).
    pub fn curtain_by_face(&mut self, mass: i64, tag: u32, spacing: f64) -> Result<i64, Error> {
        if !(spacing.is_finite() && spacing > 0.0) {
            return invalid("curtain by face: spacing must be > 0");
        }
        let (s, f, e) = self.planar_face(mass, tag)?;
        if face_parts(&f) > 1 {
            return invalid("curtain by face: face is split into several parts");
        }
        let (a, b, z0, h) =
            wall_line(&f).map_or_else(|| invalid("curtain by face: face is not vertical"), Ok)?;
        let sec = MullionSection::Rect { w: 0.05, d: 0.15 };
        self.compound("mass_curtain_by_face", |m| {
            let id = m.add_curtain_wall(&CurtainSpec {
                level: s.level,
                name: format!("{} · \u{433}\u{440}\u{430}\u{43d}\u{44c} {tag}", s.name),
                a,
                b,
                bulge: 0.0,
                base_offset: z0 - e,
                height: h,
                u: GridLines::Spacing(spacing),
                v: GridLines::Spacing(spacing),
                interior: sec.clone(),
                border: sec,
                lines: LineSections::default(),
                top: vec![],
            })?;
            m.link_host("curtain_wall", id, mass, tag, None)?;
            Ok(id)
        })
    }

    /// Floors on a horizontal form face (top at the face, face holes
    /// are kept): one per each separate part of the face.
    pub fn slab_by_face(&mut self, mass: i64, tag: u32, slab_type: i64) -> Result<Vec<i64>, Error> {
        let (s, f, e) = self.planar_face(mass, tag)?;
        if !matches!(f.class, FaceClass::Up | FaceClass::Down) {
            return invalid("slab by face: face is not horizontal");
        }
        let isl = plan_rings(&f);
        if isl.is_empty() {
            return invalid("slab by face: no outline");
        }
        self.compound("mass_slab_by_face", |m| {
            let mut out = vec![];
            for (outer, holes) in isl {
                let id = m.add_slab(&SlabSpec {
                    level: s.level,
                    slab_type,
                    offset: face_zmin(&f) - e,
                    outer,
                    holes,
                })?;
                m.link_host("slab", id, mass, tag, None)?;
                out.push(id);
            }
            Ok(out)
        })
    }

    /// Roof on a form face: a horizontal face gives all slopes the angle `slope_deg` (a small one
    /// means "flat"); an inclined face uses its own slope from the eave, the other edges are gables.
    pub fn roof_by_face(
        &mut self,
        mass: i64,
        tag: u32,
        slab_type: i64,
        slope_deg: f64,
    ) -> Result<Vec<i64>, Error> {
        let (s, f, e) = self.planar_face(mass, tag)?;
        if !(matches!(f.class, FaceClass::Up | FaceClass::Sloped) && f.normal.2 > 0.0) {
            return invalid("roof by face: face does not look up");
        }
        let isl = plan_rings(&f);
        if isl.is_empty() {
            return invalid("roof by face: no outline");
        }
        self.compound("mass_roof_by_face", |m| {
            let mut out = vec![];
            for (ring, _) in isl {
                let slopes = roof_slopes(&f, &ring, slope_deg);
                let id = m.add_roof(&RoofSpec {
                    level: s.level,
                    slab_type,
                    offset: face_zmin(&f) - e,
                    overhang: 0.0,
                    fascia: 0.0,
                    kind: RoofKind::Footprint {
                        ring,
                        slopes,
                        mansard: None,
                    },
                })?;
                m.link_host("roof", id, mass, tag, None)?;
                out.push(id);
            }
            Ok(out)
        })
    }

    /// Floors by form floors: on each level inside the form, following the section contour (with
    /// holes); floors are bound to the form and rebuilt when it is edited.
    pub fn floor_slabs(&mut self, mass: i64, slab_type: i64) -> Result<Vec<i64>, Error> {
        let b = self.mass_body(mass)?;
        let lv = levels(&self.db)?;
        let zs: Vec<f64> = lv.iter().map(|l| l.1).collect();
        let mut plan = vec![];
        for (z, _, _) in gm::floors(&b, &zs) {
            let Some(l) = lv.iter().find(|l| l.1 == z) else {
                continue;
            };
            for r in floor_rings(&b, z) {
                plan.push((l.0, r));
            }
        }
        if plan.is_empty() {
            return invalid("floor slabs: no level crosses the mass");
        }
        self.compound("mass_floors", |m| {
            let mut out = vec![];
            for (level, (outer, holes)) in plan {
                let id = m.add_slab(&SlabSpec {
                    level,
                    slab_type,
                    offset: 0.0,
                    outer,
                    holes,
                })?;
                m.link_host("slab", id, mass, 0, Some(level))?;
                out.push(id);
            }
            Ok(out)
        })
    }

    /// Join geometry: the winner is chosen by category priority (the first on ties);
    /// a void form always cuts.
    pub fn join_geometry(&mut self, a: (&str, i64), b: (&str, i64)) -> Result<(), Error> {
        let (w, l) = if join_priority(b.0) > join_priority(a.0) {
            (b, a)
        } else {
            (a, b)
        };
        self.set_join(w, l)
    }

    /// Cut: `cutter` (usually a void form) cuts `target`.
    pub fn cut_geometry(&mut self, cutter: (&str, i64), target: (&str, i64)) -> Result<(), Error> {
        self.set_join(cutter, target)
    }

    fn set_join(&mut self, w: (&str, i64), l: (&str, i64)) -> Result<(), Error> {
        element_body(&self.db, w.0, w.1)?;
        element_body(&self.db, l.0, l.1)?;
        if w == l {
            return invalid("join: same element");
        }
        let (w, l) = if is_void(&self.db, l.0, l.1)? && !is_void(&self.db, w.0, w.1)? {
            (l, w)
        } else {
            (w, l)
        };
        self.op("mass_join", |db| {
            let mut inv = Inv::default();
            for (x, y) in [(w, l), (l, w)] {
                let rows: Vec<i64> = db
                    .prepare("SELECT rowid FROM geom_join WHERE winner_kind=?1 AND winner=?2 AND loser_kind=?3 AND loser=?4")?
                    .query_map(params![x.0, x.1, y.0, y.1], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                for r in rows {
                    inv = inv.restore(db, "geom_join", "rowid", r)?;
                    db.execute("DELETE FROM geom_join WHERE rowid=?1", [r])?;
                }
            }
            db.execute(
                "INSERT INTO geom_join VALUES (?1,?2,?3,?4)",
                params![w.0, w.1, l.0, l.1],
            )?;
            let id = db.last_insert_rowid();
            let inv = Inv::default()
                .sql("DELETE FROM geom_join WHERE rowid=?1", vec![id.into()])
                .then(inv);
            Ok(((), w.1, inv))
        })
    }

    /// Swap the join order (not allowed with a void form: it always cuts).
    pub fn switch_join(&mut self, a: (&str, i64), b: (&str, i64)) -> Result<(), Error> {
        if is_void(&self.db, a.0, a.1)? || is_void(&self.db, b.0, b.1)? {
            return invalid("switch join: a void always cuts");
        }
        let joins = self.joins()?;
        let is = |x: &(String, i64), y: (&str, i64)| x.0 == y.0 && x.1 == y.1;
        match joins
            .iter()
            .find(|j| (is(&j.0, a) && is(&j.1, b)) || (is(&j.0, b) && is(&j.1, a)))
        {
            Some((w, l)) => {
                let (w, l) = ((w.0.clone(), w.1), (l.0.clone(), l.1));
                self.set_join((&l.0, l.1), (&w.0, w.1))
            }
            None => invalid("switch join: elements are not joined"),
        }
    }

    pub fn unjoin_geometry(&mut self, a: (&str, i64), b: (&str, i64)) -> Result<(), Error> {
        self.op("mass_unjoin", |db| {
            let mut inv = Inv::default();
            let mut n = 0;
            for (x, y) in [(a, b), (b, a)] {
                let rows: Vec<i64> = db
                    .prepare("SELECT rowid FROM geom_join WHERE winner_kind=?1 AND winner=?2 AND loser_kind=?3 AND loser=?4")?
                    .query_map(params![x.0, x.1, y.0, y.1], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                for r in rows {
                    n += 1;
                    inv = inv.restore(db, "geom_join", "rowid", r)?;
                    db.execute("DELETE FROM geom_join WHERE rowid=?1", [r])?;
                }
            }
            if n == 0 {
                return invalid("unjoin: elements are not joined");
            }
            Ok(((), a.1, inv))
        })
    }

    /// Joins: (winner, loser).
    #[allow(clippy::type_complexity)]
    pub fn joins(&self) -> Result<Vec<((String, i64), (String, i64))>, Error> {
        join_rows(&self.db)
    }

    /// Element solid after joins: minus all winners (a void form always cuts).
    pub fn joined_body(&self, kind: &str, id: i64) -> Result<Body, Error> {
        joined_db(&self.db, kind, id)
    }

    /// Elements that something cuts (for rendering, plans and sections), without duplicates.
    pub fn cut_elements(&self) -> Result<Vec<(String, i64)>, Error> {
        let mut out: Vec<(String, i64)> = vec![];
        for (w, l) in join_rows(&self.db)? {
            let t = if is_void(&self.db, &l.0, l.1)? { w } else { l };
            if !out.contains(&t) {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// Element section after cuts by horizontal plane z (None if the element is not cut).
    pub fn cut_section(&self, kind: &str, id: i64, z: f64) -> Result<Option<Vec<Vec<V2>>>, Error> {
        if cutters(&self.db, kind, id)?.is_empty() {
            return Ok(None);
        }
        Ok(Some(gm::section_rings(&joined_db(&self.db, kind, id)?, z)))
    }

    /// Element mesh after cuts (None if the element is not cut).
    pub fn cut_mesh(&self, kind: &str, id: i64) -> Result<Option<ddc_geom::mesh::Mesh>, Error> {
        if cutters(&self.db, kind, id)?.is_empty() {
            return Ok(None);
        }
        Ok(Some(joined_db(&self.db, kind, id)?.to_mesh()))
    }

    /// Solids cutting the element (world coordinates; by layer parts).
    pub fn cutter_bodies(&self, kind: &str, id: i64) -> Result<Vec<Body>, Error> {
        cutter_parts(&self.db, kind, id)
    }

    /// Element layers after cuts: (layer position in the type, solid; world coordinates).
    pub fn joined_parts(&self, kind: &str, id: i64) -> Result<Vec<ElementPart>, Error> {
        joined_parts(&self.db, kind, id)
    }

    /// Section of element layers after cuts by plane z: (layer position, contours); `None` if
    /// the element is not cut.
    #[allow(clippy::type_complexity)]
    pub fn cut_section_layers(
        &self,
        kind: &str,
        id: i64,
        z: f64,
    ) -> Result<Option<Vec<(i64, Vec<Vec<V2>>)>>, Error> {
        if cutters(&self.db, kind, id)?.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            joined_parts(&self.db, kind, id)?
                .into_iter()
                .map(|(k, b)| (k, gm::section_rings(&b, z)))
                .collect(),
        ))
    }

    /// Form schedule by floors (levels inside the form).
    pub fn mass_schedule(&self) -> Result<Vec<MassRow>, Error> {
        let mut st = self.db.prepare(
            "SELECT q.mass,m.name,q.level,l.name,q.area,q.perimeter FROM mass_qty q \
             JOIN mass m ON m.id=q.mass JOIN level l ON l.id=q.level ORDER BY q.mass,l.elevation",
        )?;
        let rows: Vec<(i64, String, i64, String, f64, f64)> = st
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        let mut out = vec![];
        let mut cache: Option<(i64, f64, f64)> = None;
        for (mass, name, level, level_name, area, perimeter) in rows {
            if cache.is_none_or(|c| c.0 != mass) {
                // a broken history (see warnings) must not hide the other forms
                let Ok(b) = self.mass_body(mass) else {
                    continue;
                };
                cache = Some((mass, b.volume(), b.area()));
            }
            let c = cache.unwrap_or_default();
            out.push(MassRow {
                mass,
                name,
                level,
                level_name,
                area,
                perimeter,
                volume: c.1,
                surface: c.2,
            });
        }
        Ok(out)
    }
}

#[path = "mass_bool.rs"]
pub(crate) mod bool_link;
pub use bool_link::{BoolLink, BoolRequest, VoidRequest};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LayerSpec;
    use ddc_geom::csg::ftag;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
        vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
    }

    fn setup() -> (Model, Vec<i64>, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv: Vec<i64> = (0..4)
            .map(|k| m.add_level(&format!("L{k}"), 3.0 * k as f64).unwrap())
            .collect();
        let st = m
            .add_slab_type(
                "floor",
                &[LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let wt = m
            .add_wall_type(
                "wall",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.3,
                }],
            )
            .unwrap();
        (m, lv, st, wt)
    }

    fn tower(level: i64) -> MassSpec {
        MassSpec {
            level,
            name: "Tower".into(),
            void: false,
            inplace: false,
            form: Form::Extrude {
                ring: rect(0., 0., 10., 6.),
                z0: 0.0,
                h: 9.0,
                ids: vec![],
            },
        }
    }

    #[test]
    fn form_json_roundtrips_and_evaluates() {
        let f = Form::Fillet {
            base: Form::Bool {
                op: BoolOp::Subtract,
                a: Form::Cuboid {
                    lo: (0., 0., 0.),
                    hi: (2., 2., 2.),
                }
                .base(),
                b: Form::Revolve {
                    profile: vec![(0., -1.), (0.5, -1.), (0.5, 3.), (0., 3.)],
                    axis: (1., 1.),
                    angle: std::f64::consts::TAU,
                    seg: 24,
                    ids: vec![],
                }
                .base(),
            }
            .base(),
            faces: (ftag(1, 1), ftag(1, 2)),
            r: 0.2,
            seg: 8,
        };
        let back =
            Form::from_json(&serde_json::from_str(&f.to_json().to_string()).unwrap()).unwrap();
        assert_eq!(back, f);
        let b = f.eval().unwrap();
        b.validate(TOL).unwrap();
        assert!(b.volume() < 8.0 - 1.4);
    }

    #[test]
    fn mass_floors_schedule_and_undo() {
        let (mut m, lv, _, _) = setup();
        let id = m.add_mass(&tower(lv[0])).unwrap();
        let rows = m.mass_schedule().unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows
            .iter()
            .all(|r| (r.area - 60.0).abs() < 1e-9 && (r.volume - 540.0).abs() < 1e-9));
        m.edit_mass_form(id, |b| Form::PushPull {
            base: b,
            face: ftag(1, 1),
            d: 3.0,
            keys: vec![],
        })
        .unwrap();
        assert_eq!(m.mass_schedule().unwrap().len(), 4);
        m.undo().unwrap();
        assert_eq!(m.mass_schedule().unwrap().len(), 3);
        m.undo().unwrap();
        assert!(m.masses().unwrap().is_empty());
    }

    #[test]
    fn face_hosts_follow_the_mass() {
        let (mut m, lv, st, wt) = setup();
        let id = m.add_mass(&tower(lv[0])).unwrap();
        let faces = m.mass_faces(id).unwrap();
        let south = faces
            .iter()
            .find(|f| f.class == FaceClass::Vertical && f.normal.1 < -0.5)
            .unwrap()
            .tag;
        let top = faces.iter().find(|f| f.class == FaceClass::Up).unwrap().tag;
        let w = m.wall_by_face(id, south, wt).unwrap();
        let s = m.slab_by_face(id, top, st).unwrap()[0];
        let r = m.roof_by_face(id, top, st, 2.0).unwrap()[0];
        let cw = m
            .curtain_by_face(
                id,
                faces.iter().find(|f| f.normal.0 > 0.5).unwrap().tag,
                1.5,
            )
            .unwrap();
        assert_eq!(m.host_face("wall", w).unwrap(), Some((id, south)));
        let h = |m: &Model, t: &str, id: i64| -> f64 {
            m.sql()
                .query_row(&format!("SELECT height FROM {t} WHERE id=?1"), [id], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        assert!((h(&m, "wall", w) - 9.0).abs() < 1e-9);
        m.edit_mass_form(id, |b| Form::PushPull {
            base: b,
            face: ftag(1, 1),
            d: 3.0,
            keys: vec![],
        })
        .unwrap();
        assert!((h(&m, "wall", w) - 12.0).abs() < 1e-9);
        assert!((h(&m, "curtain_wall", cw) - 12.0).abs() < 1e-9);
        let off: f64 = m
            .sql()
            .query_row("SELECT offset FROM slab WHERE id=?1", [s], |r| r.get(0))
            .unwrap();
        assert!((off - 12.0).abs() < 1e-9, "slab offset {off}");
        let roff: f64 = m
            .sql()
            .query_row("SELECT offset FROM roof WHERE id=?1", [r], |r| r.get(0))
            .unwrap();
        assert!((roff - 12.0).abs() < 1e-9);
        m.undo().unwrap();
        assert!((h(&m, "wall", w) - 9.0).abs() < 1e-9);
        // the south wall axis keeps the exterior on the face normal side
        let (ax, bx): (f64, f64) = m
            .sql()
            .query_row("SELECT ax,bx FROM wall WHERE id=?1", [w], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(ax < bx);
        let fl = m.floor_slabs(id, st).unwrap();
        assert_eq!(fl.len(), 3);
    }

    #[test]
    fn void_cuts_slab_join_priority_and_unjoin() {
        let (mut m, lv, st, wt) = setup();
        let slab = m
            .add_slab(&SlabSpec {
                level: lv[1],
                slab_type: st,
                offset: 0.0,
                outer: rect(0., 0., 10., 10.),
                holes: vec![],
            })
            .unwrap();
        let void = m
            .add_mass(&MassSpec {
                level: lv[0],
                name: "Atrium".into(),
                void: true,
                inplace: true,
                form: Form::Cuboid {
                    lo: (2., 2., 0.),
                    hi: (5., 6., 9.),
                },
            })
            .unwrap();
        m.cut_geometry(("mass", void), ("slab", slab)).unwrap();
        let v = m.joined_body("slab", slab).unwrap().volume();
        assert!((v - (100.0 - 12.0) * 0.2).abs() < 1e-9, "{v}");
        // wall vs slab: the wall wins by category priority
        let w = m
            .add_wall(&WallSpec {
                a: (0., 5.),
                b: (10., 5.),
                level: lv[0],
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        m.join_geometry(("slab", slab), ("wall", w)).unwrap();
        let j = m.joins().unwrap();
        assert!(j.contains(&(("wall".into(), w), ("slab".into(), slab))));
        let v2 = m.joined_body("slab", slab).unwrap().volume();
        // wall 0.3 thick crosses the slab band 2.8..3.0 outside the void (void spans y 2..6)
        assert!((v2 - (v - 0.3 * 0.2 * 7.0)).abs() < 1e-9, "{v2}");
        m.switch_join(("wall", w), ("slab", slab)).unwrap();
        assert!((m.joined_body("slab", slab).unwrap().volume() - v).abs() < 1e-9);
        m.unjoin_geometry(("wall", w), ("slab", slab)).unwrap();
        assert_eq!(m.joins().unwrap().len(), 1);
        m.undo().unwrap();
        assert_eq!(m.joins().unwrap().len(), 2);
    }

    #[test]
    fn paint_split_faces_and_old_files_without_tables() {
        let (mut m, lv, _, _) = setup();
        assert!(m.mass_schedule().unwrap().is_empty());
        assert!(m.masses().unwrap().is_empty());
        let id = m.add_mass(&tower(lv[0])).unwrap();
        m.edit_mass_form(id, |b| Form::SplitFace {
            base: b,
            face: ftag(1, 2),
            at: (4., 0., 0.),
            n: (1., 0., 0.),
        })
        .unwrap();
        let faces = m.mass_faces(id).unwrap();
        let new = faces.iter().map(|f| f.tag).max().unwrap();
        m.paint_face(id, new, Some("glass")).unwrap();
        let f = m
            .mass_faces(id)
            .unwrap()
            .into_iter()
            .find(|f| f.tag == new)
            .unwrap();
        assert_eq!(f.material.as_deref(), Some("glass"));
        assert!((f.area - 6.0 * 9.0).abs() < 1e-9);
        m.undo().unwrap();
        assert!(m
            .mass_faces(id)
            .unwrap()
            .iter()
            .all(|f| f.material.is_none()));
        let p = m.preview_push_pull(id, ftag(1, 1), 2.0).unwrap();
        assert!((p.volume() - 660.0).abs() < 1e-9);
    }

    fn ops(m: &Model) -> i64 {
        m.sql()
            .query_row("SELECT count(*) FROM op", [], |r| r.get(0))
            .unwrap()
    }

    fn south(m: &Model, id: i64) -> u32 {
        m.mass_faces(id)
            .unwrap()
            .iter()
            .find(|f| f.class == FaceClass::Vertical && f.normal.1 < -0.5)
            .unwrap()
            .tag
    }

    #[test]
    fn element_by_face_is_one_undo_step_and_face_ids_are_stable() {
        let (mut m, lv, st, wt) = setup();
        let id = m.add_mass(&tower(lv[0])).unwrap();
        let s = south(&m, id);
        assert_eq!(s, ftag(1, 2));
        let n = ops(&m);
        let w = m.wall_by_face(id, s, wt).unwrap();
        assert_eq!(ops(&m), n + 1, "wall + link = one op");
        m.undo().unwrap();
        assert!(m.host_face("wall", w).unwrap().is_none());
        assert_eq!(
            m.sql()
                .query_row("SELECT count(*) FROM wall", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        let w = m.wall_by_face(id, s, wt).unwrap();
        let fl = m.floor_slabs(id, st).unwrap();
        assert_eq!(ops(&m), n + 2, "all floor slabs = one op");
        assert_eq!(fl.len(), 3);
        // a push/pull of the east face keeps the south face id and its wall
        let east = m
            .mass_faces(id)
            .unwrap()
            .iter()
            .find(|f| f.normal.0 > 0.5)
            .unwrap()
            .tag;
        m.edit_mass_form(id, |b| Form::PushPull {
            base: b,
            face: east,
            d: 2.0,
            keys: vec![],
        })
        .unwrap();
        assert_eq!(south(&m, id), s);
        let len: f64 = m
            .sql()
            .query_row("SELECT bx-ax FROM wall WHERE id=?1", [w], |r| r.get(0))
            .unwrap();
        assert!((len - 12.0).abs() < 1e-9, "{len}");
        // floor slabs follow the mass
        let area: f64 = m
            .slab_quantities(fl[1])
            .unwrap()
            .iter()
            .map(|q| q.area)
            .sum();
        assert!((area - 72.0).abs() < 1e-9, "{area}");
        assert!(m.wall_warnings().unwrap().is_empty());
    }

    #[test]
    fn vanished_face_orphans_the_host_with_a_warning() {
        let (mut m, lv, _, wt) = setup();
        let id = m.add_mass(&tower(lv[0])).unwrap();
        let s = south(&m, id);
        let w = m.wall_by_face(id, s, wt).unwrap();
        let before: (f64, f64) = m
            .sql()
            .query_row("SELECT ax,height FROM wall WHERE id=?1", [w], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        // replace the extrusion by a revolve: op 1 now has a different kind, tag 1:2 is curved
        let mut sp = m.mass(id).unwrap();
        sp.form = Form::Revolve {
            profile: vec![(0., 0.), (3., 0.), (3., 9.), (0., 9.)],
            axis: (5., 3.),
            angle: std::f64::consts::TAU,
            seg: 24,
            ids: vec![],
        };
        m.update_mass(id, &sp).unwrap();
        let after: (f64, f64) = m
            .sql()
            .query_row("SELECT ax,height FROM wall WHERE id=?1", [w], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(before, after);
        let h = m.mass_hosts(id).unwrap();
        assert!(h[0].orphan);
        let warn = m.wall_warnings().unwrap();
        assert!(
            warn.iter().any(|x| x.table == "wall" && x.id == w),
            "{warn:?}"
        );
        m.undo().unwrap();
        assert!(!m.mass_hosts(id).unwrap()[0].orphan);
        assert!(m.wall_warnings().unwrap().is_empty());
    }

    #[test]
    fn deleted_elements_drop_links_and_ids_are_not_inherited() {
        let (mut m, lv, _, wt) = setup();
        let id = m.add_mass(&tower(lv[0])).unwrap();
        let s = south(&m, id);
        let w = m.wall_by_face(id, s, wt).unwrap();
        let void = m
            .add_mass(&MassSpec {
                level: lv[0],
                name: "v".into(),
                void: true,
                inplace: true,
                form: Form::Cuboid {
                    lo: (1., -1., 1.),
                    hi: (2., 1., 2.),
                },
            })
            .unwrap();
        m.cut_geometry(("mass", void), ("wall", w)).unwrap();
        m.delete_wall(w).unwrap();
        assert!(m.host_face("wall", w).unwrap().is_none());
        assert!(m.joins().unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.host_face("wall", w).unwrap(), Some((id, s)));
        assert_eq!(m.joins().unwrap().len(), 1);
        m.delete_wall(w).unwrap();
        let w2 = m
            .add_wall(&WallSpec {
                a: (20., 0.),
                b: (25., 0.),
                level: lv[0],
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        assert_eq!(w2, w, "rowid reused");
        assert!(m.host_face("wall", w2).unwrap().is_none());
        assert!(m.joins().unwrap().is_empty());
        // a level used by a mass cannot be deleted
        let l = m.add_level("L9", 30.0).unwrap();
        let mut sp = m.mass(id).unwrap();
        sp.level = l;
        m.update_mass(id, &sp).unwrap();
        assert!(matches!(m.delete_level(l), Err(Error::Referenced { .. })));
        assert!(m.check_integrity().unwrap().is_empty());
    }

    #[test]
    fn void_always_cuts_and_reduces_quantities() {
        let (mut m, lv, st, _) = setup();
        let slab = m
            .add_slab(&SlabSpec {
                level: lv[1],
                slab_type: st,
                offset: 0.0,
                outer: rect(0., 0., 10., 10.),
                holes: vec![],
            })
            .unwrap();
        let vol = |m: &Model| -> f64 {
            m.slab_quantities(slab)
                .unwrap()
                .iter()
                .map(|q| q.volume)
                .sum()
        };
        assert!((vol(&m) - 20.0).abs() < 1e-9);
        let void = m
            .add_mass(&MassSpec {
                level: lv[0],
                name: "v".into(),
                void: true,
                inplace: true,
                form: Form::Cuboid {
                    lo: (2., 2., 0.),
                    hi: (5., 6., 9.),
                },
            })
            .unwrap();
        // by category priority the slab would win; the void cuts anyway
        m.join_geometry(("slab", slab), ("mass", void)).unwrap();
        let v = m.joined_body("slab", slab).unwrap().volume();
        assert!((v - 88.0 * 0.2).abs() < 1e-9, "{v}");
        assert!((vol(&m) - 88.0 * 0.2).abs() < 1e-9, "{}", vol(&m));
        assert!(m.switch_join(("slab", slab), ("mass", void)).is_err());
        assert_eq!(m.cut_elements().unwrap(), vec![("slab".to_string(), slab)]);
        m.undo().unwrap();
        assert!((vol(&m) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn level_elevation_change_resyncs_floor_slabs() {
        let (mut m, lv, st, _) = setup();
        // a pyramid-like blend: sections shrink upwards
        let id = m
            .add_mass(&MassSpec {
                level: lv[0],
                name: "b".into(),
                void: false,
                inplace: false,
                form: Form::Blend {
                    sections: vec![(rect(0., 0., 10., 10.), 0.0), (rect(4., 4., 6., 6.), 10.0)],
                    ids: vec![],
                },
            })
            .unwrap();
        let fl = m.floor_slabs(id, st).unwrap();
        let area = |m: &Model, s: i64| -> f64 {
            m.slab_quantities(s).unwrap().iter().map(|q| q.area).sum()
        };
        let a1 = area(&m, fl[1]);
        m.update_level(lv[1], "L1", 5.0).unwrap();
        let a2 = area(&m, fl[1]);
        assert!(a2 < a1 - 1.0, "{a1} -> {a2}");
        let w = 10.0 - 8.0 * 0.5;
        assert!((a2 - w * w).abs() < 1e-6, "{a2}");
        m.undo().unwrap();
        assert!((area(&m, fl[1]) - a1).abs() < 1e-9);
    }

    #[test]
    fn slab_by_face_keeps_holes_and_roof_by_sloped_face_slopes_from_eave() {
        let (mut m, lv, st, _) = setup();
        let id = m
            .add_mass(&MassSpec {
                level: lv[0],
                name: "court".into(),
                void: false,
                inplace: false,
                form: Form::Bool {
                    op: BoolOp::Subtract,
                    a: Form::Extrude {
                        ring: rect(0., 0., 10., 10.),
                        z0: 0.0,
                        h: 3.0,
                        ids: vec![],
                    }
                    .base(),
                    b: Form::Extrude {
                        ring: rect(4., 4., 6., 6.),
                        z0: -1.0,
                        h: 5.0,
                        ids: vec![],
                    }
                    .base(),
                },
            })
            .unwrap();
        let top = m
            .mass_faces(id)
            .unwrap()
            .iter()
            .find(|f| f.class == FaceClass::Up)
            .unwrap()
            .tag;
        let s = m.slab_by_face(id, top, st).unwrap();
        assert_eq!(s.len(), 1);
        let s = s[0];
        let a: f64 = m.slab_quantities(s).unwrap().iter().map(|q| q.area).sum();
        assert!((a - 96.0).abs() < 1e-9, "{a}");
        // shed: a cuboid with a chamfered top edge gives one sloped face
        let shed = m
            .add_mass(&MassSpec {
                level: lv[0],
                name: "shed".into(),
                void: false,
                inplace: false,
                form: Form::Chamfer {
                    base: Form::Cuboid {
                        lo: (20., 0., 0.),
                        hi: (24., 4., 4.),
                    }
                    .base(),
                    faces: (ftag(1, 1), ftag(1, 2)),
                    d: 2.0,
                },
            })
            .unwrap();
        let sl = m
            .mass_faces(shed)
            .unwrap()
            .into_iter()
            .find(|f| f.class == FaceClass::Sloped)
            .unwrap();
        let r = m.roof_by_face(shed, sl.tag, st, 30.0).unwrap()[0];
        let slopes: Vec<Option<f64>> = m
            .sql()
            .prepare("SELECT slope FROM roof_vertex WHERE roof=?1 ORDER BY idx")
            .unwrap()
            .query_map([r], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            slopes.iter().filter(|s| s.is_some()).count(),
            1,
            "{slopes:?}"
        );
        assert!(slopes.iter().flatten().all(|s| (s - 45.0).abs() < 1e-6));
        // curved and missing faces are rejected with an error
        assert!(m.roof_by_face(shed, 999_999, st, 30.0).is_err());
    }

    fn host_orphan(m: &Model, mass: i64, kind: &str, id: i64) -> bool {
        m.mass_hosts(mass)
            .unwrap()
            .iter()
            .find(|h| h.kind == kind && h.id == id)
            .unwrap()
            .orphan
    }

    #[test]
    fn inserting_a_vertex_keeps_other_faces_and_orphans_the_split_one() {
        let (mut m, lv, _, wt) = setup();
        let id = m.add_mass(&tower(lv[0])).unwrap();
        let (south, east, north) = (ftag(1, 2), ftag(1, 3), ftag(1, 4));
        let ws = m.wall_by_face(id, south, wt).unwrap();
        let we = m.wall_by_face(id, east, wt).unwrap();
        let wn = m.wall_by_face(id, north, wt).unwrap();
        let ax = |m: &Model| -> f64 {
            m.sql()
                .query_row("SELECT ax FROM wall WHERE id=?1", [we], |r| r.get(0))
                .unwrap()
        };
        let ax0 = ax(&m);
        let mut sp = m.mass(id).unwrap();
        sp.form = Form::Extrude {
            ring: vec![(0., 0.), (5., 0.), (10., 0.), (10., 6.), (0., 6.)],
            z0: 0.0,
            h: 9.0,
            ids: vec![],
        };
        m.update_mass(id, &sp).unwrap();
        assert!(!host_orphan(&m, id, "wall", we));
        assert!(!host_orphan(&m, id, "wall", wn));
        // the split south face is not silently rebound to one of its halves
        assert!(host_orphan(&m, id, "wall", ws));
        let f = m.mass_faces(id).unwrap();
        assert!(f.iter().any(|f| f.tag == east && f.normal.0 > 0.5));
        assert!(!f.iter().any(|f| f.tag == south));
        // moving vertices keeps the ids (same edge count): the east wall follows
        let mut sp = m.mass(id).unwrap();
        if let Form::Extrude { ring, .. } = &mut sp.form {
            ring[2] = (12., 0.);
            ring[3] = (12., 6.);
        }
        m.update_mass(id, &sp).unwrap();
        assert!((ax(&m) - ax0 - 2.0).abs() < 1e-9, "{ax0} -> {}", ax(&m));
    }

    fn twins(h: f64) -> Form {
        Form::Bool {
            op: BoolOp::Subtract,
            a: Form::Extrude {
                ring: rect(0., 0., 10., 4.),
                z0: 0.0,
                h,
                ids: vec![],
            }
            .base(),
            b: Form::Cuboid {
                lo: (4., -1., -1.),
                hi: (6., 5., 20.),
            }
            .base(),
        }
    }

    #[test]
    fn separate_parts_of_a_face_give_separate_slabs() {
        let (mut m, lv, st, _) = setup();
        // a block cut through by a slot: its top face has two islands
        let id = m
            .add_mass(&MassSpec {
                level: lv[0],
                name: "twins".into(),
                void: false,
                inplace: false,
                form: twins(9.0),
            })
            .unwrap();
        let top = m
            .mass_faces(id)
            .unwrap()
            .iter()
            .find(|f| f.class == FaceClass::Up)
            .unwrap()
            .tag;
        let s = m.slab_by_face(id, top, st).unwrap();
        assert_eq!(s.len(), 2);
        for &x in &s {
            let a: f64 = m.slab_quantities(x).unwrap().iter().map(|q| q.area).sum();
            assert!((a - 16.0).abs() < 1e-9, "{a}");
        }
        let fl = m.floor_slabs(id, st).unwrap();
        assert_eq!(fl.len(), 6);
        // both slabs follow the face after an edit, each to its own island
        let mut sp = m.mass(id).unwrap();
        sp.form = twins(10.0);
        m.update_mass(id, &sp).unwrap();
        let mut xs: Vec<(f64, f64)> = s
            .iter()
            .map(|&x| {
                assert!(!host_orphan(&m, id, "slab", x));
                m.sql()
                    .query_row(
                        "SELECT min(x),max(x) FROM slab_vertex WHERE slab=?1 AND ring=0",
                        [x],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap()
            })
            .collect();
        xs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let near =
            |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
        assert!(near(xs[0], (0., 4.)) && near(xs[1], (6., 10.)), "{xs:?}");
        for &x in &s {
            let off: f64 = m
                .sql()
                .query_row("SELECT offset FROM slab WHERE id=?1", [x], |r| r.get(0))
                .unwrap();
            assert!((off - 10.0).abs() < 1e-9, "{off}");
        }
    }

    #[test]
    fn move_mass_is_one_step_and_moves_hosts() {
        let (mut m, lv, _, wt) = setup();
        let id = m.add_mass(&tower(lv[0])).unwrap();
        let w = m.wall_by_face(id, ftag(1, 2), wt).unwrap();
        let at = |m: &Model| -> (f64, f64) {
            m.sql()
                .query_row("SELECT ax,ay FROM wall WHERE id=?1", [w], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap()
        };
        let p0 = at(&m);
        let n = ops(&m);
        m.move_mass(id, (1.0, 2.0, 0.0)).unwrap();
        m.move_mass(id, (1.0, 0.0, 0.0)).unwrap();
        assert_eq!(ops(&m), n + 2);
        let Form::Move { d, .. } = m.mass(id).unwrap().form else {
            panic!("not a move");
        };
        assert_eq!(d, (2.0, 2.0, 0.0));
        let p1 = at(&m);
        assert!(
            (p1.0 - p0.0 - 2.0).abs() < 1e-9 && (p1.1 - p0.1 - 2.0).abs() < 1e-9,
            "{p0:?} {p1:?}"
        );
        m.undo().unwrap();
        m.undo().unwrap();
        assert!(matches!(m.mass(id).unwrap().form, Form::Extrude { .. }));
        assert_eq!(at(&m), p0);
    }

    #[test]
    fn mass_target_moves_with_hosts_copies_and_refuses_turns() {
        use crate::Target;
        let (mut m, lv, _, wt) = setup();
        let id = m.add_mass(&tower(lv[0])).unwrap();
        let w = m.wall_by_face(id, ftag(1, 2), wt).unwrap();
        let at = |m: &Model| -> (f64, f64) {
            m.sql()
                .query_row("SELECT ax,ay FROM wall WHERE id=?1", [w], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap()
        };
        let p0 = at(&m);
        m.move_elements(&[Target::Mass(id)], (3.0, 0.0)).unwrap();
        assert!((at(&m).0 - p0.0 - 3.0).abs() < 1e-9);
        m.undo().unwrap();
        assert_eq!(at(&m), p0);

        let head = m.change_head();
        assert!(m
            .rotate_elements(&[Target::Mass(id)], (0.0, 0.0), 0.5, false)
            .is_err());
        assert!(m
            .mirror_elements(&[Target::Mass(id)], (0.0, 0.0), (0.0, 1.0), false)
            .is_err());
        assert_eq!(m.change_head(), head);

        let out = m
            .copy_elements(&[Target::Mass(id)], &[(10.0, 0.0)])
            .unwrap();
        let Target::Mass(c) = out[0] else { panic!() };
        assert_ne!(c, id);
        assert!(m.mass(c).is_ok());
        m.undo().unwrap();
        assert!(m.mass(c).is_err());
    }

    #[test]
    fn mass_copy_with_its_wall_keeps_the_host_link() {
        use crate::Target;
        let (mut m, lv, _, wt) = setup();
        let id = m.add_mass(&tower(lv[0])).unwrap();
        let w = m.wall_by_face(id, ftag(1, 2), wt).unwrap();
        let out = m
            .copy_elements(&[Target::Mass(id), Target::Wall(w)], &[(10.0, 0.0)])
            .unwrap();
        let c = out
            .iter()
            .find_map(|t| if let Target::Mass(c) = t { Some(*c) } else { None })
            .unwrap();
        let nw = out
            .iter()
            .find_map(|t| if let Target::Wall(c) = t { Some(*c) } else { None })
            .unwrap();
        let hosts = m.mass_hosts(c).unwrap();
        assert!(hosts.iter().any(|h| h.kind == "wall" && h.id == nw));
        assert!(m.mass_hosts(id).unwrap().iter().any(|h| h.id == w));
        m.undo().unwrap();
        assert!(m.mass(c).is_err());
    }

    #[test]
    fn mass_lifts_in_one_undo_step() {
        use crate::Target;
        let (mut m, lv, _, _) = setup();
        let id = m.add_mass(&tower(lv[0])).unwrap();
        let top = |m: &Model| m.mass_body(id).unwrap().bbox().unwrap().1.z;
        let z0 = top(&m);
        m.lift_elements(&[Target::Mass(id)], 2.5).unwrap();
        assert!((top(&m) - z0 - 2.5).abs() < 1e-9);
        m.undo().unwrap();
        assert!((top(&m) - z0).abs() < 1e-9);
    }

    #[test]
    fn combined_mass_keeps_hosts_of_both_operands() {
        let (mut m, lv, _, wt) = setup();
        let a = m.add_mass(&tower(lv[0])).unwrap();
        let mut sb = tower(lv[0]);
        sb.form = Form::Cuboid {
            lo: (20., 0., 0.),
            hi: (24., 4., 6.),
        };
        let b = m.add_mass(&sb).unwrap();
        let south_b = m
            .mass_faces(b)
            .unwrap()
            .into_iter()
            .find(|f| f.normal.1 < -0.5)
            .unwrap()
            .tag;
        let w = m.wall_by_face(b, south_b, wt).unwrap();
        m.combine_masses(a, b, BoolOp::Union).unwrap();
        let h = m.mass_hosts(a).unwrap();
        let hw = h.iter().find(|h| h.kind == "wall" && h.id == w).unwrap();
        assert!(!hw.orphan);
        assert_eq!(tag_op(hw.face), tag_op(south_b) + 1);
        let f = m.mass_faces(a).unwrap();
        let g = f.iter().find(|f| f.tag == hw.face).unwrap();
        assert!(g.normal.1 < -0.5 && g.outline[0].iter().all(|p| p.0 >= 20.0 - 1e-9));
    }
}

/// Top of an arc wall by profile (u is the length along the axis): the solid per sector up to 15 deg, in each
/// sector the plan wedge is intersected with a flat "elevator" along the chord; the extreme wedges extend
/// a meter past the ends so as not to trim the skewed joint ends.
fn arc_trim(body: &Body, ax: &ddc_geom::arc::ArcAxis, t: &[(f64, f64)], z0: f64, hmax: f64, w: f64) -> Body {
    let len = ax.length();
    let mut us = vec![0.0, len];
    us.extend(t.iter().map(|p| p.0).filter(|u| *u > 1e-9 && *u < len - 1e-9));
    us.sort_by(f64::total_cmp);
    us.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let step = (15f64.to_radians() * ax.radius).max(1e-3);
    let mut cuts = vec![];
    for pair in us.windows(2) {
        let n = ((pair[1] - pair[0]) / step).ceil().max(1.0) as usize;
        for k in 0..n {
            cuts.push((
                pair[0] + (pair[1] - pair[0]) * k as f64 / n as f64,
                pair[0] + (pair[1] - pair[0]) * (k + 1) as f64 / n as f64,
            ));
        }
    }
    let last = cuts.len().saturating_sub(1);
    let big = ax.radius + w;
    let mut out = Body::default();
    for (k, &(s0, s1)) in cuts.iter().enumerate() {
        let (p0, p1) = (ax.point_at(s0), ax.point_at(s1));
        let c = (p1 - p0).len();
        if c < 1e-9 {
            continue;
        }
        let e = (p1 - p0) * (1.0 / c);
        let ne = v2(-e.y, e.x);
        let (h0, h1) = (
            ddc_geom::top::height_at(t, s0 + 1e-7),
            ddc_geom::top::height_at(t, s1 - 1e-7),
        );
        let at = |x: f64, z: f64| {
            let p = p0 + e * x - ne * w;
            v3(p.x, p.y, z)
        };
        let ring = vec![
            at(-w, z0 - 1.0),
            at(c + w, z0 - 1.0),
            at(c + w, z0 + h1),
            at(c, z0 + h1),
            at(0.0, z0 + h0),
            at(-w, z0 + h0),
        ];
        let keep = gm::prism(&ring, v3(ne.x, ne.y, 0.0) * (2.0 * w));
        let (sa, sb) = (if k == 0 { s0 - 1.0 } else { s0 }, if k == last { s1 + 1.0 } else { s1 });
        let d = |s: f64| {
            let q = ax.angle_at(s);
            v2(q.cos(), q.sin())
        };
        let mut tri = vec![ax.center, ax.center + d(sa) * big, ax.center + d(sb) * big];
        if poly::area(&tri) < 0.0 {
            tri.reverse();
        }
        let wedge = gm::extrude(&tri, z0 - 1.0, hmax + 2.0);
        let slab = wedge.boolean(&keep, BoolOp::Intersect, TOL);
        out = out.union(&body.boolean(&slab, BoolOp::Intersect, TOL), TOL);
    }
    out
}
