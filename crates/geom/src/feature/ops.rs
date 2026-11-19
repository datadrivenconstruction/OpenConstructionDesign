// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Modifier operations a `Modify` feature runs on its input body. The timeline knows them only
//! through [`FeatureOp`]: new kernel operations (edge blends, draft, ...) become features by
//! adding one implementation to [`standard_ops`], with no change to the timeline or storage.

use std::sync::OnceLock;

use crate::component::expr::{Dim, ANGLE, LENGTH, NUMBER};
use crate::csg::Body;
use crate::mass as gm;
use crate::math::{v3, Tol, V3};

pub(crate) const TOL: Tol = Tol {
    length: 1e-7,
    angle: 1e-9,
};

/// One numeric argument: name (storage key), dimension, default expression.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArgSpec {
    pub name: &'static str,
    pub dim: Dim,
    pub default: &'static str,
}

pub trait FeatureOp: Send + Sync {
    /// Stored kind name; never change it once released.
    fn kind(&self) -> &'static str;
    fn args(&self) -> &'static [ArgSpec];
    /// Allowed number of face references (min, max).
    fn faces(&self) -> (usize, usize);
    /// Takes a point list after its fixed arguments: stored as `p{i}.x/y/z` (lengths, no gaps),
    /// passed to `build` as three values per point.
    fn points(&self) -> bool {
        false
    }
    /// New faces get tags `ftag(op, k)`; `args` follow [`FeatureOp::args`] order.
    fn build(&self, b: &Body, faces: &[u32], args: &[f64], op: u32) -> Result<Body, String>;
    /// [`FeatureOp::build`] for an operation that makes several new faces: their local numbers
    /// come from `keys` (extended in place with new ones), so a rebuild with other argument
    /// values keeps every face's tag. Operations with one fixed new face need not override it.
    fn build_keyed(
        &self,
        b: &Body,
        faces: &[u32],
        args: &[f64],
        op: u32,
        keys: &mut gm::FaceKeys,
    ) -> Result<Body, String> {
        let _ = keys;
        self.build(b, faces, args, op)
    }
}

pub struct Registry {
    ops: Vec<Box<dyn FeatureOp>>,
}

impl Registry {
    pub fn empty() -> Self {
        Registry { ops: vec![] }
    }

    /// Adds an operation; one with the same kind is replaced.
    pub fn with(mut self, op: Box<dyn FeatureOp>) -> Self {
        self.ops.retain(|o| o.kind() != op.kind());
        self.ops.push(op);
        self
    }

    pub fn get(&self, kind: &str) -> Option<&dyn FeatureOp> {
        self.ops.iter().find(|o| o.kind() == kind).map(|b| b.as_ref())
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        self.ops.iter().map(|o| o.kind()).collect()
    }
}

/// Every built-in modifier.
pub fn standard_ops() -> Vec<Box<dyn FeatureOp>> {
    vec![
        Box::new(Chamfer),
        Box::new(Fillet),
        Box::new(Shell),
        Box::new(Offset),
        Box::new(PushPull),
        Box::new(PushRegion),
        Box::new(Blend),
        Box::new(Draft),
    ]
}

/// The shared registry of built-in modifiers.
pub fn standard() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(|| standard_ops().into_iter().fold(Registry::empty(), Registry::with))
}

fn err(e: gm::MassError) -> String {
    e.to_string()
}

struct Chamfer;
impl FeatureOp for Chamfer {
    fn kind(&self) -> &'static str {
        "chamfer"
    }
    fn args(&self) -> &'static [ArgSpec] {
        &[ArgSpec {
            name: "d",
            dim: LENGTH,
            default: "10mm",
        }]
    }
    fn faces(&self) -> (usize, usize) {
        (2, 2)
    }
    fn build(&self, b: &Body, f: &[u32], a: &[f64], op: u32) -> Result<Body, String> {
        let e = gm::edge_between(b, f[0], f[1], 1e-7).map_err(err)?;
        gm::chamfer(b, &e, a[0], op, TOL).map_err(err)
    }
}

struct Fillet;
impl FeatureOp for Fillet {
    fn kind(&self) -> &'static str {
        "fillet"
    }
    fn args(&self) -> &'static [ArgSpec] {
        &[
            ArgSpec {
                name: "r",
                dim: LENGTH,
                default: "10mm",
            },
            ArgSpec {
                name: "seg",
                dim: NUMBER,
                default: "8",
            },
        ]
    }
    fn faces(&self) -> (usize, usize) {
        (2, 2)
    }
    fn build(&self, b: &Body, f: &[u32], a: &[f64], op: u32) -> Result<Body, String> {
        if !(a[1].is_finite() && a[1] >= 2.0 && a[1] <= gm::MAX_SEG as f64) {
            return Err(format!("fillet segments must be 2..{}", gm::MAX_SEG));
        }
        let e = gm::edge_between(b, f[0], f[1], 1e-7).map_err(err)?;
        gm::fillet(b, &e, a[0], a[1].round() as usize, op, TOL).map_err(err)
    }
}

struct Shell;
impl FeatureOp for Shell {
    fn kind(&self) -> &'static str {
        "shell"
    }
    fn args(&self) -> &'static [ArgSpec] {
        &[ArgSpec {
            name: "t",
            dim: LENGTH,
            default: "100mm",
        }]
    }
    fn faces(&self) -> (usize, usize) {
        (0, usize::MAX)
    }
    fn build(&self, b: &Body, f: &[u32], a: &[f64], op: u32) -> Result<Body, String> {
        self.build_keyed(b, f, a, op, &mut vec![])
    }
    fn build_keyed(&self, b: &Body, f: &[u32], a: &[f64], op: u32, k: &mut gm::FaceKeys) -> Result<Body, String> {
        gm::shell_keyed(b, a[0], f, op, k, TOL).map_err(err)
    }
}

struct Offset;
impl FeatureOp for Offset {
    fn kind(&self) -> &'static str {
        "offset"
    }
    fn args(&self) -> &'static [ArgSpec] {
        &[ArgSpec {
            name: "d",
            dim: LENGTH,
            default: "100mm",
        }]
    }
    fn faces(&self) -> (usize, usize) {
        (1, usize::MAX)
    }
    fn build(&self, b: &Body, f: &[u32], a: &[f64], op: u32) -> Result<Body, String> {
        self.build_keyed(b, f, a, op, &mut vec![])
    }
    fn build_keyed(&self, b: &Body, f: &[u32], a: &[f64], op: u32, k: &mut gm::FaceKeys) -> Result<Body, String> {
        gm::offset_faces_keyed(b, f, a[0], op, k, TOL).map_err(err)
    }
}

struct PushPull;
impl FeatureOp for PushPull {
    fn kind(&self) -> &'static str {
        "push_pull"
    }
    fn args(&self) -> &'static [ArgSpec] {
        &[ArgSpec {
            name: "d",
            dim: LENGTH,
            default: "100mm",
        }]
    }
    fn faces(&self) -> (usize, usize) {
        (1, 1)
    }
    fn build(&self, b: &Body, f: &[u32], a: &[f64], op: u32) -> Result<Body, String> {
        self.build_keyed(b, f, a, op, &mut vec![])
    }
    fn build_keyed(&self, b: &Body, f: &[u32], a: &[f64], op: u32, k: &mut gm::FaceKeys) -> Result<Body, String> {
        gm::push_pull_keyed(b, f[0], a[0], op, k, TOL).map_err(err)
    }
}

/// Push/pull of the whole face `f[0]` or of a loop drawn on it (points `p{i}`), by `d`, up to
/// the parallel face `f[1]`, or through the body (`through` = 1). New faces are named by role
/// and loop edge: region cap `ftag(op, 1)`, side of loop edge i `ftag(op, 2 + i)`; a whole-face
/// push names its sides by the stored keys. A changed distance keeps every name.
struct PushRegion;
impl FeatureOp for PushRegion {
    fn kind(&self) -> &'static str {
        "push_region"
    }
    fn args(&self) -> &'static [ArgSpec] {
        &[
            ArgSpec {
                name: "d",
                dim: LENGTH,
                default: "100mm",
            },
            ArgSpec {
                name: "through",
                dim: NUMBER,
                default: "0",
            },
        ]
    }
    fn faces(&self) -> (usize, usize) {
        (1, 2)
    }
    fn points(&self) -> bool {
        true
    }
    fn build(&self, b: &Body, f: &[u32], a: &[f64], op: u32) -> Result<Body, String> {
        self.build_keyed(b, f, a, op, &mut vec![])
    }
    fn build_keyed(&self, b: &Body, f: &[u32], a: &[f64], op: u32, k: &mut gm::FaceKeys) -> Result<Body, String> {
        let to = match (f.get(1), a[1] != 0.0) {
            (Some(t), _) => gm::PushTarget::Face(*t),
            (None, true) => gm::PushTarget::Through,
            (None, false) => gm::PushTarget::Dist(a[0]),
        };
        let d = gm::push_distance(b, f[0], to, TOL).map_err(err)?;
        let pts: Vec<V3> = a[2..].chunks(3).map(|c| v3(c[0], c[1], c[2])).collect();
        match pts.len() {
            0 => gm::push_pull_keyed(b, f[0], d, op, k, TOL).map_err(err),
            1 | 2 => Err("a region needs at least 3 points".into()),
            _ => gm::push_region(b, f[0], &pts, d, op, TOL).map_err(err),
        }
    }
}

/// Fillet (`round` = 1, `seg` steps) or chamfer of the edges between face pairs
/// (f[0], f[1]), (f[2], f[3]), ... in one step. The blend face of edge i is `ftag(op, 2 + 3i)`
/// and vertex blends follow, so names depend on the edge list, not on the size.
struct Blend;
impl FeatureOp for Blend {
    fn kind(&self) -> &'static str {
        "blend"
    }
    fn args(&self) -> &'static [ArgSpec] {
        &[
            ArgSpec {
                name: "r",
                dim: LENGTH,
                default: "10mm",
            },
            ArgSpec {
                name: "round",
                dim: NUMBER,
                default: "1",
            },
            ArgSpec {
                name: "seg",
                dim: NUMBER,
                default: "8",
            },
        ]
    }
    fn faces(&self) -> (usize, usize) {
        (2, 512)
    }
    fn build(&self, b: &Body, f: &[u32], a: &[f64], op: u32) -> Result<Body, String> {
        if f.len() % 2 != 0 {
            return Err("blend takes faces in pairs, one pair per edge".into());
        }
        let edges: Vec<(u32, u32)> = f.chunks(2).map(|p| (p[0], p[1])).collect();
        let kind = if a[1] != 0.0 {
            gm::BlendKind::Fillet {
                r: a[0],
                seg: a[2].round().max(2.0) as usize,
            }
        } else {
            gm::BlendKind::Chamfer { d: a[0] }
        };
        gm::blend_edges(b, &edges, kind, op, TOL).map_err(err)
    }
}

/// Draft of faces about the neutral plane through `at` with normal `pull` by `angle`; each
/// face keeps its name.
struct Draft;
impl FeatureOp for Draft {
    fn kind(&self) -> &'static str {
        "draft"
    }
    fn args(&self) -> &'static [ArgSpec] {
        &[
            ArgSpec {
                name: "angle",
                dim: ANGLE,
                default: "3deg",
            },
            ArgSpec {
                name: "at.x",
                dim: LENGTH,
                default: "0",
            },
            ArgSpec {
                name: "at.y",
                dim: LENGTH,
                default: "0",
            },
            ArgSpec {
                name: "at.z",
                dim: LENGTH,
                default: "0",
            },
            ArgSpec {
                name: "pull.x",
                dim: NUMBER,
                default: "0",
            },
            ArgSpec {
                name: "pull.y",
                dim: NUMBER,
                default: "0",
            },
            ArgSpec {
                name: "pull.z",
                dim: NUMBER,
                default: "1",
            },
        ]
    }
    fn faces(&self) -> (usize, usize) {
        (1, 512)
    }
    fn build(&self, b: &Body, f: &[u32], a: &[f64], op: u32) -> Result<Body, String> {
        let pull = v3(a[4], a[5], a[6]);
        if pull.len() < 1e-12 {
            return Err("draft pull direction is zero".into());
        }
        gm::draft(b, f, v3(a[1], a[2], a[3]), pull, a[0], op, TOL).map_err(err)
    }
}
