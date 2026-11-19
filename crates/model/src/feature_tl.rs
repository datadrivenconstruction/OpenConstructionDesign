//! Parametric feature timelines of masses (`Form::Timeline`, JSON `"k":"timeline"`): storage
//! codec, evaluation inside a form history (faces `ftag(op_base + node id, k)`, a process-wide
//! node cache so an edit rebuilds only what changed), model edits (one undo step each) and the
//! conversion of a plain form history into an editable timeline with literal parameters.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use ddc_geom::csg::{ftag, tag_op, Body, LOCAL_BITS};
use ddc_geom::feature::{
    self as ft, rebuild, standard, Cache, FeatureError, Kind, Node, NodeId, PlaneKind, Rebuild, Shape,
    Timeline,
};
use serde_json::Value;

use crate::mass::{Form, PushGoal, TOL};
use ddc_geom::mass as gm;

fn s(x: &str) -> String {
    x.to_string()
}
use crate::{Error, MassSpec, Model};

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Cache::new(512)))
}

fn fe(e: FeatureError) -> Error {
    Error::Invalid(format!("timeline: {e}"))
}

/// Builds with the shared cache.
pub fn build(t: &Timeline, op_base: u32) -> Result<Rebuild, Error> {
    let mut c = cache().lock().unwrap_or_else(|p| p.into_inner());
    rebuild(t, standard(), &mut c, op_base).map_err(fe)
}

/// Body of a timeline inside a form history; `next` is the history's op counter.
/// Face keys the build adds are written back, so a saved form names its faces the same way
/// on every later rebuild.
pub(crate) fn eval(t: &mut Timeline, next: &mut u32) -> Result<Body, Error> {
    let base = *next - 1;
    let r = build(t, base)?;
    t.adopt_keys(&r);
    *next += t.next_id - 1;
    if let Some(b) = r.body {
        return Ok(b);
    }
    Err({
        let why = r.first_error().map_or("nothing is built".to_string(), |(id, m)| {
            let name = t.node(id).map_or(String::new(), |n| n.name.clone());
            format!("{name}: {m}")
        });
        Error::Invalid(format!("timeline: {why}"))
    })
}

fn slot_kind(k: &Kind) -> &'static str {
    match k {
        Kind::Sketch { .. } => "sketch",
        Kind::Extrude { .. } => "extrude",
        Kind::Revolve { .. } => "revolve",
        Kind::Sweep { .. } => "sweep",
        Kind::Loft { .. } => "loft",
        Kind::Boolean { .. } => "bool",
        Kind::Move { .. } => "move",
        Kind::Modify { op, .. } => standard().get(op).map_or("feature", |o| o.kind()),
    }
}

/// Op kinds of the history slots a timeline takes (one per node id ever given).
pub(crate) fn walk(t: &Timeline, out: &mut Vec<&'static str>) {
    for id in 1..t.next_id {
        out.push(t.node(id).map_or("feature", |n| slot_kind(&n.kind)));
    }
}

pub(crate) fn to_json(t: &Timeline) -> Value {
    ft::json::to_json(t)
}

pub(crate) fn from_json(v: &Value) -> Result<Timeline, Error> {
    ft::json::from_json(v).map_err(Error::Invalid)
}

/// The timeline at the bottom of a chain of direct edits (push/pull, chamfer, ... on top).
fn find_tl(f: &mut Form) -> Option<&mut Timeline> {
    match f {
        Form::Timeline(t) => Some(t.as_mut()),
        Form::PushPull { base, .. }
        | Form::Chamfer { base, .. }
        | Form::Fillet { base, .. }
        | Form::Shell { base, .. }
        | Form::Offset { base, .. }
        | Form::Move { base, .. }
        | Form::SplitFace { base, .. }
        | Form::PushTo { base, .. }
        | Form::Blends { base, .. }
        | Form::Draft { base, .. } => find_tl(base),
        _ => None,
    }
}

fn len(v: f64) -> String {
    format!("{v}m")
}

/// How a converted primitive renumbers its local faces.
#[derive(Clone)]
enum Remap {
    Same,
    /// Stable edge ids: local `2 + ids[i]` becomes `2 + i`.
    Sides(Vec<u32>),
}

struct Conv {
    t: Timeline,
    next: u32,
    ops: HashMap<u32, (NodeId, Remap)>,
}

impl Conv {
    fn add(&mut self, kind: Kind) -> NodeId {
        let id = self.t.next_id;
        self.t.next_id += 1;
        self.t.nodes.push(Node {
            id,
            name: format!("{}{id}", kind.name()),
            suppressed: false,
            kind,
            keys: vec![],
        });
        id
    }

    fn take(&mut self, node: NodeId, rm: Remap) {
        self.ops.insert(self.next, (node, rm));
        self.next += 1;
    }

    fn face(&self, tag: u32) -> Result<u32, Error> {
        let (node, rm) = self
            .ops
            .get(&tag_op(tag))
            .ok_or_else(|| Error::Invalid(format!("timeline: no face {tag}")))?;
        let local = tag & ((1 << LOCAL_BITS) - 1);
        let local = match rm {
            Remap::Same => local,
            Remap::Sides(ids) => match ids.iter().position(|i| local >= 2 && *i == local - 2) {
                Some(i) => 2 + i as u32,
                None if local < 2 => local,
                None => return Err(Error::Invalid(format!("timeline: no face {tag}"))),
            },
        };
        Ok(ftag(*node, local))
    }

    fn sketch(&mut self, plane: PlaneKind, offset: f64, pts: &[(f64, f64)]) -> NodeId {
        self.add(Kind::Sketch {
            plane,
            offset: len(offset),
            shape: Shape::Poly(pts.iter().map(|p| (len(p.0), len(p.1))).collect()),
        })
    }

    fn sides(ids: &[u32]) -> Remap {
        if ids.iter().enumerate().all(|(i, v)| *v == i as u32) {
            Remap::Same
        } else {
            Remap::Sides(ids.to_vec())
        }
    }

    fn modify(
        &mut self,
        base: &Form,
        op: &str,
        faces: &[u32],
        args: &[(&str, String)],
        keys: &[[u32; 4]],
    ) -> Result<NodeId, Error> {
        let input = self.form(base)?;
        let faces = faces.iter().map(|f| self.face(*f)).collect::<Result<Vec<_>, _>>()?;
        // stored face keys name source and neighbour faces by tag: renumber those, keep roles
        let tag = |t: u32| match self.ops.contains_key(&tag_op(t)) {
            true => self.face(t),
            false => Ok(t),
        };
        let keys = keys
            .iter()
            .map(|k| Ok([tag(k[0])?, tag(k[1])?, k[2], k[3]]))
            .collect::<Result<Vec<_>, Error>>()?;
        let n = self.add(Kind::Modify {
            op: op.into(),
            input,
            faces,
            args: args.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        });
        if let Some(node) = self.t.nodes.last_mut() {
            node.keys = keys;
        }
        self.take(n, Remap::Same);
        Ok(n)
    }

    /// Converts in the order a form history numbers its ops; returns the result's node.
    fn form(&mut self, f: &Form) -> Result<NodeId, Error> {
        let cant = |k: &str| Err(Error::Invalid(format!("timeline: {k} cannot be converted")));
        Ok(match f {
            Form::Cuboid { lo, hi } => {
                let ring = [(lo.0, lo.1), (hi.0, lo.1), (hi.0, hi.1), (lo.0, hi.1)];
                let s = self.sketch(PlaneKind::Xy, lo.2, &ring);
                let e = self.add(Kind::Extrude {
                    sketch: s,
                    dist: len(hi.2 - lo.2),
                    start: len(0.0),
                });
                self.take(e, Remap::Same);
                e
            }
            Form::Extrude { ring, z0, h, ids } => {
                let s = self.sketch(PlaneKind::Xy, *z0, ring);
                let e = self.add(Kind::Extrude {
                    sketch: s,
                    dist: len(*h),
                    start: len(0.0),
                });
                self.take(e, Conv::sides(ids));
                e
            }
            Form::Revolve {
                profile,
                axis,
                angle,
                seg,
                ids,
            } => {
                let s = self.sketch(PlaneKind::Xz, 0.0, profile);
                let r = self.add(Kind::Revolve {
                    sketch: s,
                    angle: format!("{angle}rad"),
                    seg: *seg,
                });
                self.take(r, Conv::sides(ids));
                if axis.0 == 0.0 && axis.1 == 0.0 {
                    r
                } else {
                    self.add(Kind::Move {
                        input: r,
                        d: [len(axis.0), len(axis.1), len(0.0)],
                        turn: "0".into(),
                    })
                }
            }
            Form::Sweep { profile, path, ids } => {
                if !matches!(Conv::sides(ids), Remap::Same) {
                    return cant("sweep with renumbered edges");
                }
                let s = self.sketch(PlaneKind::Xy, 0.0, profile);
                let w = self.add(Kind::Sweep {
                    sketch: s,
                    path: path.iter().map(|p| [len(p.0), len(p.1), len(p.2)]).collect(),
                });
                self.take(w, Remap::Same);
                w
            }
            Form::Blend { sections, ids } => {
                if !ids.is_empty() {
                    return cant("blend with renumbered edges");
                }
                let sk = sections
                    .iter()
                    .map(|(ring, z)| self.sketch(PlaneKind::Xy, *z, ring))
                    .collect();
                let l = self.add(Kind::Loft { sketches: sk });
                self.take(l, Remap::Same);
                l
            }
            Form::Bool { op, a, b } => {
                let a = self.form(a)?;
                let b = self.form(b)?;
                self.next += 1;
                self.add(Kind::Boolean { op: *op, a, b })
            }
            Form::Move { base, d } => {
                let input = self.form(base)?;
                self.next += 1;
                self.add(Kind::Move {
                    input,
                    d: [len(d.0), len(d.1), len(d.2)],
                    turn: "0".into(),
                })
            }
            Form::PushPull { base, face, d, keys } => {
                self.modify(base, "push_pull", &[*face], &[("d", len(*d))], keys)?
            }
            Form::Chamfer { base, faces, d } => {
                self.modify(base, "chamfer", &[faces.0, faces.1], &[("d", len(*d))], &[])?
            }
            Form::Fillet { base, faces, r, seg } => self.modify(
                base,
                "fillet",
                &[faces.0, faces.1],
                &[("r", len(*r)), ("seg", seg.to_string())],
                &[],
            )?,
            Form::Shell { base, t, open, keys } => {
                self.modify(base, "shell", open, &[("t", len(*t))], keys)?
            }
            Form::Offset { base, faces, d, keys } => {
                self.modify(base, "offset", faces, &[("d", len(*d))], keys)?
            }
            Form::PushTo {
                base,
                face,
                region,
                to,
                keys,
            } => {
                let mut faces = vec![*face];
                let mut a = vec![(s("d"), len(0.0)), (s("through"), s("0"))];
                match to {
                    PushGoal::Dist(d) => a[0].1 = len(*d),
                    PushGoal::Face(t) => faces.push(*t),
                    PushGoal::Through => a[1].1 = s("1"),
                    // a point goal is the distance it gives on the converted body
                    PushGoal::Point(_) => {
                        let b = base.eval()?;
                        let d = gm::push_distance(&b, *face, to.geom(), TOL)
                            .map_err(|e| Error::Invalid(format!("push to: {e}")))?;
                        a[0].1 = len(d);
                    }
                }
                for (i, p) in region.iter().enumerate() {
                    for (c, v) in ["x", "y", "z"].iter().zip([p.0, p.1, p.2]) {
                        a.push((format!("p{i}.{c}"), len(v)));
                    }
                }
                let a: Vec<(&str, String)> = a.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                self.modify(base, "push_region", &faces, &a, if region.is_empty() { keys.as_slice() } else { &[] })?
            }
            Form::Blends {
                base,
                edges,
                round,
                size,
                seg,
            } => {
                let faces: Vec<u32> = edges.iter().flat_map(|e| [e.0, e.1]).collect();
                let a = [
                    ("r", len(*size)),
                    ("round", s(if *round { "1" } else { "0" })),
                    ("seg", seg.to_string()),
                ];
                self.modify(base, "blend", &faces, &a, &[])?
            }
            Form::Draft {
                base,
                faces,
                at,
                pull,
                angle,
            } => {
                let a = [
                    ("angle", format!("{angle}rad")),
                    ("at.x", len(at.0)),
                    ("at.y", len(at.1)),
                    ("at.z", len(at.2)),
                    ("pull.x", pull.0.to_string()),
                    ("pull.y", pull.1.to_string()),
                    ("pull.z", pull.2.to_string()),
                ];
                self.modify(base, "draft", faces, &a, &[])?
            }
            Form::SweptBlend { .. } => return cant("swept blend"),
            Form::SplitFace { .. } => return cant("split face"),
            Form::Timeline(_) => return cant("a timeline inside a history"),
        })
    }
}

impl Form {
    /// The same body as an editable timeline (literal parameters). Face references of the
    /// direct edits are renumbered to the timeline's features.
    pub fn to_timeline(&self) -> Result<Timeline, Error> {
        if let Form::Timeline(t) = self {
            return Ok((**t).clone());
        }
        let mut c = Conv {
            t: Timeline::default(),
            next: 1,
            ops: HashMap::new(),
        };
        c.form(self)?;
        c.t.check().map_err(fe)?;
        Ok(c.t)
    }
}

impl Model {
    /// New mass driven by a timeline.
    pub fn add_mass_timeline(&mut self, level: i64, name: &str, t: Timeline) -> Result<i64, Error> {
        self.add_mass(&MassSpec {
            level,
            name: name.into(),
            void: false,
            inplace: true,
            form: Form::Timeline(Box::new(t)),
        })
    }

    /// The mass's timeline (also under direct edits made on top of it).
    pub fn mass_timeline(&self, id: i64) -> Result<Option<Timeline>, Error> {
        let mut s = self.mass(id)?;
        Ok(find_tl(&mut s.form).map(|t| t.clone()))
    }

    /// Per-feature status of the mass's timeline (cached nodes are not built again).
    pub fn mass_timeline_build(&self, id: i64) -> Result<Option<Rebuild>, Error> {
        match self.mass_timeline(id)? {
            Some(t) => build(&t, 0).map(Some),
            None => Ok(None),
        }
    }

    /// One timeline edit as one undo step; a refused edit changes nothing.
    pub fn edit_mass_timeline(
        &mut self,
        id: i64,
        f: impl FnOnce(&mut Timeline) -> Result<(), FeatureError>,
    ) -> Result<(), Error> {
        let mut s = self.mass(id)?;
        let t = find_tl(&mut s.form)
            .ok_or_else(|| Error::Invalid(format!("mass {id} has no timeline")))?;
        f(t).map_err(fe)?;
        self.update_mass(id, &s)
    }

    /// A component family whose body is the timeline: its parameters become the family's
    /// (plain values per instance, formulas kept), so each instance builds its own body.
    pub fn load_timeline_family(
        &mut self,
        name: &str,
        category: ddc_geom::component::Category,
        t: &Timeline,
    ) -> Result<i64, Error> {
        let text = ddc_geom::component::from_feature::family_text(name, category, t).map_err(Error::Invalid)?;
        self.load_family(&text)
    }

    /// The timeline of a family made by `load_timeline_family` (its first timeline body).
    pub fn family_timeline(&self, family: i64) -> Result<Option<Timeline>, Error> {
        Ok(self.family(family)?.solids.iter().find_map(|s| match &s.shape {
            ddc_geom::component::Shape::Feature { tl } => Some(tl.as_ref().clone()),
            _ => None,
        }))
    }

    /// Replaces the mass's plain history with an equivalent timeline (one undo step).
    pub fn mass_to_timeline(&mut self, id: i64) -> Result<(), Error> {
        let mut s = self.mass(id)?;
        if matches!(s.form, Form::Timeline(_)) {
            return Ok(());
        }
        s.form = Form::Timeline(Box::new(s.form.to_timeline()?));
        self.update_mass(id, &s)
    }
}

/// Registry kinds a `Modify` feature can use (for menus).
pub fn modifier_kinds() -> Vec<&'static str> {
    standard().kinds()
}

/// Argument list of a feature for editing: key, expression, dimension name.
pub fn feature_args(k: &Kind) -> Vec<(String, String, String)> {
    k.args(standard())
        .into_iter()
        .map(|(key, e, d)| (key, e, d.to_string()))
        .collect()
}

pub use ft::Status as FeatureStatus;

#[cfg(test)]
#[path = "feature_tl_tests.rs"]
mod tests;
