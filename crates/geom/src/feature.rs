//! Parametric feature timeline: named parameters with expressions between them, an ordered list
//! of features (sketch, extrude, revolve, sweep, loft, boolean, move, modifiers from a registry),
//! a roll-back marker and suppression. Every number is an expression text (`component::expr`,
//! units and dimensions); a parameter cycle is refused and leaves the timeline unchanged.
//! A node id is stable and is also the face-tag operation of its faces (`ftag(id, local)`), so
//! face references survive reordering and parameter edits. Rebuild with a key cache: see `build`.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::component::expr::{self, Dim, Q, ANGLE, LENGTH, NUMBER};
use crate::csg::BoolOp;
use crate::sketch_dims::ProfileSketch;

mod build;
pub mod codec;
pub mod json;
mod ops;
#[cfg(test)]
mod tests;

pub use build::{rebuild, Cache, Frame3, NodeReport, Out, Profile, Rebuild, Status};
pub use ops::{standard, standard_ops, ArgSpec, FeatureOp, Registry};

pub type NodeId = u32;

/// A named value; `expr` may use other parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub expr: String,
}

/// Sketch plane parallel to a coordinate plane, shifted by `offset` (normal `n = u × v`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaneKind {
    /// u = x, v = y, n = z; offset is the elevation.
    Xy,
    /// u = x, v = z, n = -y; offset is y.
    Xz,
    /// u = y, v = z, n = x; offset is x.
    Yz,
}

impl PlaneKind {
    pub fn name(self) -> &'static str {
        match self {
            PlaneKind::Xy => "xy",
            PlaneKind::Xz => "xz",
            PlaneKind::Yz => "yz",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [PlaneKind::Xy, PlaneKind::Xz, PlaneKind::Yz]
            .into_iter()
            .find(|k| k.name() == s)
    }
}

/// Closed profile of a sketch, in plane coordinates (u, v).
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Rect {
        cx: String,
        cy: String,
        w: String,
        h: String,
    },
    Circle {
        cx: String,
        cy: String,
        r: String,
        seg: usize,
    },
    Poly(Vec<(String, String)>),
    /// Constrained sketch; driving dimension texts may use the timeline parameters.
    Solved(ProfileSketch),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind0 {
    Sketch,
    Extrude,
    Revolve,
    Sweep,
    Loft,
    Boolean,
    Move,
    Modify,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Sketch {
        plane: PlaneKind,
        offset: String,
        shape: Shape,
    },
    /// Profile moved `start` along the normal, then extruded by `dist` (sign = side).
    Extrude {
        sketch: NodeId,
        dist: String,
        start: String,
    },
    /// Profile (u >= 0) turned about the sketch's v axis through the plane origin.
    Revolve {
        sketch: NodeId,
        angle: String,
        seg: usize,
    },
    /// Profile (u along the path normal, v along the binormal) along a 3D polyline.
    Sweep {
        sketch: NodeId,
        path: Vec<[String; 3]>,
    },
    /// Plan sketches at their own elevations, bottom to top.
    Loft { sketches: Vec<NodeId> },
    Boolean { op: BoolOp, a: NodeId, b: NodeId },
    /// Turn about the vertical through the origin, then shift.
    Move {
        input: NodeId,
        d: [String; 3],
        turn: String,
    },
    /// A registry operation (chamfer, fillet, shell, ...) on the input body.
    Modify {
        op: String,
        input: NodeId,
        faces: Vec<u32>,
        args: Vec<(String, String)>,
    },
}

impl Kind {
    pub fn class(&self) -> Kind0 {
        match self {
            Kind::Sketch { .. } => Kind0::Sketch,
            Kind::Extrude { .. } => Kind0::Extrude,
            Kind::Revolve { .. } => Kind0::Revolve,
            Kind::Sweep { .. } => Kind0::Sweep,
            Kind::Loft { .. } => Kind0::Loft,
            Kind::Boolean { .. } => Kind0::Boolean,
            Kind::Move { .. } => Kind0::Move,
            Kind::Modify { .. } => Kind0::Modify,
        }
    }

    /// Short kind name (i18n key suffix, journal op kind). `Modify` gives its operation.
    pub fn name(&self) -> &str {
        match self {
            Kind::Sketch { .. } => "sketch",
            Kind::Extrude { .. } => "extrude",
            Kind::Revolve { .. } => "revolve",
            Kind::Sweep { .. } => "sweep",
            Kind::Loft { .. } => "loft",
            Kind::Boolean { .. } => "boolean",
            Kind::Move { .. } => "move",
            Kind::Modify { op, .. } => op,
        }
    }

    pub fn is_sketch(&self) -> bool {
        matches!(self, Kind::Sketch { .. })
    }

    /// Inputs: sketches for profile features, bodies otherwise. The first is the one a
    /// suppressed pass-through feature forwards.
    pub fn inputs(&self) -> Vec<NodeId> {
        match self {
            Kind::Sketch { .. } => vec![],
            Kind::Extrude { sketch, .. } | Kind::Revolve { sketch, .. } | Kind::Sweep { sketch, .. } => {
                vec![*sketch]
            }
            Kind::Loft { sketches } => sketches.clone(),
            Kind::Boolean { a, b, .. } => vec![*a, *b],
            Kind::Move { input, .. } | Kind::Modify { input, .. } => vec![*input],
        }
    }

    /// Whether the inputs are sketches (profiles) rather than bodies.
    pub fn takes_sketches(&self) -> bool {
        matches!(
            self,
            Kind::Extrude { .. } | Kind::Revolve { .. } | Kind::Sweep { .. } | Kind::Loft { .. }
        )
    }

    /// Suppressed, it forwards its first input (instead of dropping out).
    pub fn passes_through(&self) -> bool {
        matches!(self, Kind::Boolean { .. } | Kind::Move { .. } | Kind::Modify { .. })
    }

    /// Every numeric argument: key, expression text, expected dimension. `reg` gives the
    /// dimensions of registry operations (unknown operation: number).
    pub fn args(&self, reg: &Registry) -> Vec<(String, String, Dim)> {
        let mut out = vec![];
        let mut put = |k: String, v: &String, d: Dim| out.push((k, v.clone(), d));
        match self {
            Kind::Sketch {
                offset, shape, ..
            } => {
                put("offset".into(), offset, LENGTH);
                match shape {
                    Shape::Rect { cx, cy, w, h } => {
                        put("cx".into(), cx, LENGTH);
                        put("cy".into(), cy, LENGTH);
                        put("w".into(), w, LENGTH);
                        put("h".into(), h, LENGTH);
                    }
                    Shape::Circle { cx, cy, r, .. } => {
                        put("cx".into(), cx, LENGTH);
                        put("cy".into(), cy, LENGTH);
                        put("r".into(), r, LENGTH);
                    }
                    Shape::Poly(p) => {
                        for (i, (u, v)) in p.iter().enumerate() {
                            put(format!("p{i}.u"), u, LENGTH);
                            put(format!("p{i}.v"), v, LENGTH);
                        }
                    }
                    Shape::Solved(s) => {
                        for d in s.dims.iter().filter(|d| !d.driven) {
                            put(format!("dim{}", d.tag), &d.text, LENGTH);
                        }
                    }
                }
            }
            Kind::Extrude { dist, start, .. } => {
                put("dist".into(), dist, LENGTH);
                put("start".into(), start, LENGTH);
            }
            Kind::Revolve { angle, .. } => put("angle".into(), angle, ANGLE),
            Kind::Sweep { path, .. } => {
                for (i, p) in path.iter().enumerate() {
                    for (c, v) in ["x", "y", "z"].iter().zip(p) {
                        put(format!("path{i}.{c}"), v, LENGTH);
                    }
                }
            }
            Kind::Loft { .. } | Kind::Boolean { .. } => {}
            Kind::Move { d, turn, .. } => {
                for (c, v) in ["dx", "dy", "dz"].iter().zip(d) {
                    put(c.to_string(), v, LENGTH);
                }
                put("turn".into(), turn, ANGLE);
            }
            Kind::Modify { op, args, .. } => {
                let o = reg.get(op);
                let spec = o.map(|o| o.args()).unwrap_or(&[]);
                for s in spec {
                    let v = args
                        .iter()
                        .find(|a| a.0 == s.name)
                        .map_or_else(|| s.default.to_string(), |a| a.1.clone());
                    put(s.name.to_string(), &v, s.dim);
                }
                if o.is_some_and(|o| o.points()) {
                    let zero = String::from("0");
                    for i in 0.. {
                        if !args.iter().any(|a| a.0 == format!("p{i}.x")) {
                            break;
                        }
                        for c in ["x", "y", "z"] {
                            let k = format!("p{i}.{c}");
                            let v = args.iter().find(|a| a.0 == k).map_or(&zero, |a| &a.1);
                            put(k, v, LENGTH);
                        }
                    }
                }
            }
        }
        out
    }

    /// The expression text behind `key`; a registry argument not yet stored is added.
    fn arg_mut(&mut self, key: &str, reg: &Registry) -> Option<&mut String> {
        let pair = |k: &str| -> Option<(usize, usize)> {
            let (a, b) = k.split_once('.')?;
            let i = a.trim_start_matches(|c: char| c.is_ascii_alphabetic()).parse().ok()?;
            let j = match b {
                "u" | "x" => 0,
                "v" | "y" => 1,
                "z" => 2,
                _ => return None,
            };
            Some((i, j))
        };
        match self {
            Kind::Sketch { offset, shape, .. } => match (key, shape) {
                ("offset", _) => Some(offset),
                ("cx", Shape::Rect { cx, .. } | Shape::Circle { cx, .. }) => Some(cx),
                ("cy", Shape::Rect { cy, .. } | Shape::Circle { cy, .. }) => Some(cy),
                ("w", Shape::Rect { w, .. }) => Some(w),
                ("h", Shape::Rect { h, .. }) => Some(h),
                ("r", Shape::Circle { r, .. }) => Some(r),
                (k, Shape::Poly(p)) if k.starts_with('p') => {
                    let (i, j) = pair(k)?;
                    let q = p.get_mut(i)?;
                    match j {
                        0 => Some(&mut q.0),
                        1 => Some(&mut q.1),
                        _ => None,
                    }
                }
                (k, Shape::Solved(s)) if k.starts_with("dim") => {
                    let tag: i64 = k[3..].parse().ok()?;
                    s.dims
                        .iter_mut()
                        .find(|d| d.tag == tag && !d.driven)
                        .map(|d| &mut d.text)
                }
                _ => None,
            },
            Kind::Extrude { dist, start, .. } => match key {
                "dist" => Some(dist),
                "start" => Some(start),
                _ => None,
            },
            Kind::Revolve { angle, .. } => (key == "angle").then_some(angle),
            Kind::Sweep { path, .. } => {
                let (i, j) = pair(key).filter(|_| key.starts_with("path"))?;
                path.get_mut(i).map(|p| &mut p[j])
            }
            Kind::Loft { .. } | Kind::Boolean { .. } => None,
            Kind::Move { d, turn, .. } => match key {
                "dx" => Some(&mut d[0]),
                "dy" => Some(&mut d[1]),
                "dz" => Some(&mut d[2]),
                "turn" => Some(turn),
                _ => None,
            },
            Kind::Modify { op, args, .. } => {
                let o = reg.get(op)?;
                let default = match o.args().iter().find(|s| s.name == key) {
                    Some(s) => s.default,
                    // a coordinate of a stored contour point
                    None if o.points() => {
                        let (p, c) = key.split_once('.')?;
                        let x = format!("{p}.x");
                        (["x", "y", "z"].contains(&c) && args.iter().any(|a| a.0 == x)).then_some("0")?
                    }
                    None => return None,
                };
                let i = match args.iter().position(|a| a.0 == key) {
                    Some(i) => i,
                    None => {
                        args.push((key.to_string(), default.to_string()));
                        args.len() - 1
                    }
                };
                Some(&mut args[i].1)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub name: String,
    pub suppressed: bool,
    pub kind: Kind,
    /// Stable names of the faces this feature creates: `[source face, neighbour face or role,
    /// occurrence, local number]` (kernel [`crate::mass::FaceKeys`]). A side of a push/pull is
    /// keyed by the pushed face and the face across the edge, a shell's inner face by its outer
    /// face; a rebuild gives the same key the same local number, so face references survive
    /// parameter changes. Filled by [`Timeline::adopt_keys`] after a build.
    pub keys: crate::mass::FaceKeys,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FeatureError {
    BadName(String),
    Parse { what: String, msg: String },
    Eval { what: String, msg: String },
    /// Parameter names along the cycle, first repeated at the end.
    Cycle(Vec<String>),
    NoNode(NodeId),
    NoArg(String),
    /// `node` would come before (or is) its input `input`, or the input has the wrong class.
    Order { node: NodeId, input: NodeId },
    /// Still used by this node (or parameter).
    InUse(String),
    Duplicate(String),
}

impl fmt::Display for FeatureError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FeatureError::BadName(n) => write!(f, "bad parameter name '{n}'"),
            FeatureError::Parse { what, msg } => write!(f, "{what}: {msg}"),
            FeatureError::Eval { what, msg } => write!(f, "{what}: {msg}"),
            FeatureError::Cycle(c) => write!(f, "parameter cycle: {}", c.join(" -> ")),
            FeatureError::NoNode(n) => write!(f, "no feature #{n}"),
            FeatureError::NoArg(k) => write!(f, "no argument '{k}'"),
            FeatureError::Order { node, input } => {
                write!(f, "feature #{node} cannot use #{input} here")
            }
            FeatureError::InUse(by) => write!(f, "still used by {by}"),
            FeatureError::Duplicate(n) => write!(f, "'{n}' already exists"),
        }
    }
}

impl std::error::Error for FeatureError {}

/// Parameters, features in order, roll-back marker (the last feature still built; `None` =
/// all) and the next free node id (ids are never reused).
#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    pub params: Vec<Param>,
    pub nodes: Vec<Node>,
    pub marker: Option<NodeId>,
    pub next_id: NodeId,
}

impl Default for Timeline {
    fn default() -> Self {
        Timeline {
            params: vec![],
            nodes: vec![],
            marker: None,
            next_id: 1,
        }
    }
}

fn is_ident(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && c.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn vars_of(text: &str) -> Result<Vec<String>, String> {
    let e = expr::parse(text.trim()).map_err(|e| e.to_string())?;
    let mut v = vec![];
    e.vars(&mut v);
    Ok(v)
}

/// A value for an argument of dimension `want`. A bare number is metres for a length and
/// degrees for an angle (what a person types into a CAD field).
pub fn fit(q: Q, want: Dim) -> Result<f64, String> {
    if q.d == want {
        return Ok(q.v);
    }
    if q.d == NUMBER {
        return Ok(if want == ANGLE { q.v.to_radians() } else { q.v });
    }
    Err(format!("expected {want}, got {}", q.d))
}

impl Timeline {
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn index_of(&self, id: NodeId) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }

    /// Index after which nodes are rolled back (not built).
    pub fn marker_index(&self) -> usize {
        self.marker
            .and_then(|m| self.index_of(m))
            .map_or(self.nodes.len(), |i| i + 1)
    }

    /// Parameter evaluation order (dependencies first); a cycle is an error naming it.
    pub fn param_order(&self) -> Result<Vec<usize>, FeatureError> {
        let idx: HashMap<&str, usize> = self
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name.as_str(), i))
            .collect();
        let deps: Vec<Vec<usize>> = self
            .params
            .iter()
            .map(|p| {
                let v = vars_of(&p.expr).map_err(|msg| FeatureError::Parse {
                    what: p.name.clone(),
                    msg,
                })?;
                Ok(v.iter().filter_map(|n| idx.get(n.as_str()).copied()).collect())
            })
            .collect::<Result<_, FeatureError>>()?;
        // 0 new, 1 on the stack, 2 done
        let mut state = vec![0u8; self.params.len()];
        let mut order = vec![];
        let mut stack: Vec<usize> = vec![];
        fn visit(
            i: usize,
            deps: &[Vec<usize>],
            state: &mut [u8],
            stack: &mut Vec<usize>,
            order: &mut Vec<usize>,
        ) -> Result<(), Vec<usize>> {
            match state[i] {
                2 => return Ok(()),
                1 => {
                    let at = stack.iter().position(|&s| s == i).unwrap_or(0);
                    let mut c = stack[at..].to_vec();
                    c.push(i);
                    return Err(c);
                }
                _ => {}
            }
            state[i] = 1;
            stack.push(i);
            for &d in &deps[i] {
                visit(d, deps, state, stack, order)?;
            }
            stack.pop();
            state[i] = 2;
            order.push(i);
            Ok(())
        }
        for i in 0..self.params.len() {
            visit(i, &deps, &mut state, &mut stack, &mut order).map_err(|c| {
                FeatureError::Cycle(c.iter().map(|&k| self.params[k].name.clone()).collect())
            })?;
        }
        Ok(order)
    }

    /// Every parameter's value.
    pub fn values(&self) -> Result<HashMap<String, Q>, FeatureError> {
        let mut env: HashMap<String, Q> = HashMap::new();
        for i in self.param_order()? {
            let p = &self.params[i];
            let e = expr::parse(p.expr.trim()).map_err(|e| FeatureError::Parse {
                what: p.name.clone(),
                msg: e.to_string(),
            })?;
            let v = e
                .eval(&|n| env.get(n).copied())
                .map_err(|msg| FeatureError::Eval {
                    what: p.name.clone(),
                    msg,
                })?;
            env.insert(p.name.clone(), v);
        }
        Ok(env)
    }

    /// Structural check: unique ids, every input exists earlier and has the right class.
    pub fn check(&self) -> Result<(), FeatureError> {
        let mut seen: HashMap<NodeId, bool> = HashMap::new();
        for n in &self.nodes {
            if seen.contains_key(&n.id) || n.id == 0 || n.id >= self.next_id {
                return Err(FeatureError::Duplicate(format!("#{}", n.id)));
            }
            for i in n.kind.inputs() {
                match seen.get(&i) {
                    Some(&sk) if sk == n.kind.takes_sketches() => {}
                    _ => return Err(FeatureError::Order { node: n.id, input: i }),
                }
            }
            if let Kind::Boolean { a, b, .. } = &n.kind {
                if a == b {
                    return Err(FeatureError::Order { node: n.id, input: *b });
                }
            }
            seen.insert(n.id, n.kind.is_sketch());
        }
        if let Some(m) = self.marker {
            self.index_of(m).ok_or(FeatureError::NoNode(m))?;
        }
        Ok(())
    }

    /// Applies `f` to a copy; the copy replaces the timeline only if it passes `check` and the
    /// parameters still evaluate.
    fn edit<T>(
        &mut self,
        f: impl FnOnce(&mut Timeline) -> Result<T, FeatureError>,
    ) -> Result<T, FeatureError> {
        let mut t = self.clone();
        let out = f(&mut t)?;
        t.check()?;
        t.values()?;
        *self = t;
        Ok(out)
    }

    /// Adds a feature right after the roll-back marker (the marker moves onto it) or at the end.
    pub fn add(&mut self, name: &str, kind: Kind) -> Result<NodeId, FeatureError> {
        self.edit(|t| {
            let id = t.next_id;
            t.next_id += 1;
            let name = if name.trim().is_empty() {
                format!("{}{id}", kind.name())
            } else {
                name.trim().to_string()
            };
            let at = t.marker_index();
            let rolled = t.marker.is_some();
            t.nodes.insert(at, Node {
                id,
                name,
                suppressed: false,
                kind,
                keys: vec![],
            });
            if rolled {
                t.marker = Some(id);
            }
            Ok(id)
        })
    }

    /// Removes an unused feature.
    pub fn remove(&mut self, id: NodeId) -> Result<Node, FeatureError> {
        if let Some(u) = self.nodes.iter().find(|n| n.kind.inputs().contains(&id)) {
            return Err(FeatureError::InUse(u.name.clone()));
        }
        self.edit(|t| {
            let i = t.index_of(id).ok_or(FeatureError::NoNode(id))?;
            if t.marker == Some(id) {
                t.marker = (i > 0).then(|| t.nodes[i - 1].id);
            }
            Ok(t.nodes.remove(i))
        })
    }

    /// Moves a feature to position `to` (clamped); refused if an input would follow its user.
    pub fn move_to(&mut self, id: NodeId, to: usize) -> Result<(), FeatureError> {
        self.edit(|t| {
            let i = t.index_of(id).ok_or(FeatureError::NoNode(id))?;
            let n = t.nodes.remove(i);
            let to = to.min(t.nodes.len());
            t.nodes.insert(to, n);
            Ok(())
        })
    }

    pub fn suppress(&mut self, id: NodeId, on: bool) -> Result<(), FeatureError> {
        self.edit(|t| {
            let i = t.index_of(id).ok_or(FeatureError::NoNode(id))?;
            t.nodes[i].suppressed = on;
            Ok(())
        })
    }

    /// Roll back: features after `at` are kept but not built. `None` builds everything.
    pub fn set_marker(&mut self, at: Option<NodeId>) -> Result<(), FeatureError> {
        self.edit(|t| {
            t.marker = at;
            Ok(())
        })
    }

    pub fn rename(&mut self, id: NodeId, name: &str) -> Result<(), FeatureError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(FeatureError::BadName(name.into()));
        }
        self.edit(|t| {
            let i = t.index_of(id).ok_or(FeatureError::NoNode(id))?;
            t.nodes[i].name = name.to_string();
            Ok(())
        })
    }

    /// Adds or changes a parameter. Refused (nothing changes) for a bad name, a parse error,
    /// an unknown name, or a dependency cycle.
    pub fn set_param(&mut self, name: &str, text: &str) -> Result<(), FeatureError> {
        let name = name.trim();
        if !is_ident(name) {
            return Err(FeatureError::BadName(name.into()));
        }
        self.edit(|t| {
            match t.params.iter_mut().find(|p| p.name == name) {
                Some(p) => p.expr = text.trim().to_string(),
                None => t.params.push(Param {
                    name: name.to_string(),
                    expr: text.trim().to_string(),
                }),
            }
            Ok(())
        })
    }

    /// Names (parameters and features) whose expressions use parameter `name`.
    pub fn users_of(&self, name: &str, reg: &Registry) -> Vec<String> {
        let uses = |t: &str| vars_of(t).is_ok_and(|v| v.iter().any(|n| n == name));
        let mut out: Vec<String> = self
            .params
            .iter()
            .filter(|p| p.name != name && uses(&p.expr))
            .map(|p| p.name.clone())
            .collect();
        for n in &self.nodes {
            if n.kind.args(reg).iter().any(|a| uses(&a.1)) {
                out.push(n.name.clone());
            }
        }
        out
    }

    pub fn remove_param(&mut self, name: &str, reg: &Registry) -> Result<(), FeatureError> {
        if let Some(u) = self.users_of(name, reg).into_iter().next() {
            return Err(FeatureError::InUse(u));
        }
        self.edit(|t| {
            let i = t
                .params
                .iter()
                .position(|p| p.name == name)
                .ok_or_else(|| FeatureError::BadName(name.into()))?;
            t.params.remove(i);
            Ok(())
        })
    }

    /// Sets one argument expression of a feature. Refused for a parse error or a name that is
    /// not a parameter; a value of the wrong dimension is reported by the rebuild.
    pub fn set_arg(
        &mut self,
        id: NodeId,
        key: &str,
        text: &str,
        reg: &Registry,
    ) -> Result<(), FeatureError> {
        let vars = vars_of(text).map_err(|msg| FeatureError::Parse {
            what: key.into(),
            msg,
        })?;
        if let Some(v) = vars.iter().find(|v| !self.params.iter().any(|p| &p.name == *v)) {
            return Err(FeatureError::Eval {
                what: key.into(),
                msg: format!("unknown name '{v}'"),
            });
        }
        self.edit(|t| {
            let i = t.index_of(id).ok_or(FeatureError::NoNode(id))?;
            let slot = t.nodes[i]
                .kind
                .arg_mut(key, reg)
                .ok_or_else(|| FeatureError::NoArg(key.into()))?;
            *slot = text.trim().to_string();
            Ok(())
        })
    }

    /// Replaces a feature's kind (same inputs class rules apply), e.g. a new face selection.
    pub fn set_kind(&mut self, id: NodeId, kind: Kind) -> Result<(), FeatureError> {
        self.edit(|t| {
            let i = t.index_of(id).ok_or(FeatureError::NoNode(id))?;
            // another operation names its faces differently: its keys start over
            if std::mem::discriminant(&t.nodes[i].kind) != std::mem::discriminant(&kind)
                || matches!((&t.nodes[i].kind, &kind), (Kind::Modify { op: a, .. }, Kind::Modify { op: b, .. }) if a != b)
            {
                t.nodes[i].keys.clear();
            }
            t.nodes[i].kind = kind;
            Ok(())
        })
    }

    /// Takes the face keys a build added, so the next build names the faces the same way.
    /// Returns whether anything changed.
    pub fn adopt_keys(&mut self, r: &Rebuild) -> bool {
        let mut changed = false;
        for (id, k) in &r.keys {
            if let Some(n) = self.nodes.iter_mut().find(|n| n.id == *id) {
                if n.keys != *k {
                    n.keys.clone_from(k);
                    changed = true;
                }
            }
        }
        changed
    }

    /// Dirty set for changed parameters: features whose arguments use them (directly or through
    /// other parameters) and everything downstream of those, in timeline order.
    pub fn affected(&self, changed: &[&str], reg: &Registry) -> Vec<NodeId> {
        let mut names: HashSet<String> = changed.iter().map(|s| s.to_string()).collect();
        loop {
            let more: Vec<String> = self
                .params
                .iter()
                .filter(|p| !names.contains(&p.name))
                .filter(|p| vars_of(&p.expr).is_ok_and(|v| v.iter().any(|n| names.contains(n))))
                .map(|p| p.name.clone())
                .collect();
            if more.is_empty() {
                break;
            }
            names.extend(more);
        }
        let mut dirty: HashSet<NodeId> = HashSet::new();
        let mut out = vec![];
        for n in &self.nodes {
            let uses = n
                .kind
                .args(reg)
                .iter()
                .any(|a| vars_of(&a.1).is_ok_and(|v| v.iter().any(|x| names.contains(x))));
            if uses || n.kind.inputs().iter().any(|i| dirty.contains(i)) {
                dirty.insert(n.id);
                out.push(n.id);
            }
        }
        out
    }
}
