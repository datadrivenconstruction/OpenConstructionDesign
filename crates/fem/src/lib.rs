//! Linear static 3D frame finite elements: pure, no dependency on the model, app or ui crates.
//!
//! Conventions (one item each):
//! - Units: any consistent set (N, m, Pa, kg/m3 and g in m/s2); results come in the same units.
//! - Global axes: right-handed X, Y, Z. Node DOFs: ux, uy, uz, rx, ry, rz (index 0..6).
//! - Rotations and moments are right-hand vectors (positive = counter-clockwise seen from +axis).
//! - Local x runs from the flexible start (node i + offset i) to the flexible end (j + offset j).
//! - Local z lies in the plane of x and the reference vector, on its side; local y = z x x.
//! - Reference vector: `Orient::Vector`, else global Z, or global X for vertical members.
//! - `Orient::Roll(a)` turns the default y, z about local x by `a` radians.
//! - Iy bends about local y (deflection along z, Avz shear); Iz about z (along y, Avy shear).
//! - Offsets are rigid links in global axes from the node to the flexible end.
//! - Releases: 6 local flags per end (ux..rz); a released DOF carries zero end force.
//! - Member loads act on the flexible part; `a`, `b` are distances from the flexible start.
//! - `b` is clamped to L (f64::INFINITY = to the end); global line loads are per unit length.
//! - End forces: local axes, at the flexible ends, forces the nodes exert on the member.
//! - Diagrams N, Vy, Vz, T, My, Mz: action of the far part on the cut face with normal +x.
//! - So N > 0 is tension, and a beam with local z up sagging under gravity has My < 0.
//! - Reactions: forces and moments the supports exert on the structure, global axes.
//! - Equilibrium: applied load + reactions about the global origin; `residual` should be ~0.

mod element;
/// Sparse symmetric solver (profile LDL^T, RCM order), shared with form finding in geometry.
pub mod skyline;
mod solve;
#[cfg(test)]
mod tests;

pub use solve::{solve, CaseResult, Diagram, EnvelopeResult, Equilibrium, Solution, Summary};

/// Isotropic material: Young's modulus, shear modulus, mass density.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    pub e: f64,
    pub g: f64,
    pub density: f64,
}

/// Cross-section constants; `avy`/`avz` are shear areas used only with `Options::shear`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Section {
    pub a: f64,
    pub iy: f64,
    pub iz: f64,
    pub j: f64,
    pub avy: Option<f64>,
    pub avz: Option<f64>,
}

impl Section {
    pub fn new(a: f64, iy: f64, iz: f64, j: f64) -> Self {
        Section { a, iy, iz, j, avy: None, avz: None }
    }
}

/// Member orientation about its own axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Orient {
    Auto,
    /// Reference vector (global) lying in the local x-z plane.
    Vector([f64; 3]),
    /// Roll angle in radians from the `Auto` axes.
    Roll(f64),
}

/// Frame member between nodes `i` and `j`.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub i: usize,
    pub j: usize,
    pub material: usize,
    pub section: usize,
    pub orient: Orient,
    /// `release[end][dof]`, end 0 = i, 1 = j, dof in local ux, uy, uz, rx, ry, rz.
    pub release: [[bool; 6]; 2],
    /// Rigid offsets (global) from node i and node j to the flexible ends.
    pub offset: [[f64; 3]; 2],
}

impl Member {
    pub fn new(i: usize, j: usize, material: usize, section: usize) -> Self {
        Member {
            i,
            j,
            material,
            section,
            orient: Orient::Auto,
            release: [[false; 6]; 2],
            offset: [[0.0; 3]; 2],
        }
    }

    /// Bending hinge (ry and rz released) at end 0 (i) or 1 (j).
    pub fn pin_end(mut self, end: usize) -> Self {
        self.release[end][4] = true;
        self.release[end][5] = true;
        self
    }
}

/// Support condition of one DOF; springs are in force/length or moment/radian.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Restraint {
    Free,
    Fixed,
    Spring(f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Support {
    pub node: usize,
    pub dof: [Restraint; 6],
}

impl Support {
    pub fn new(node: usize, dof: [Restraint; 6]) -> Self {
        Support { node, dof }
    }
    pub fn fixed(node: usize) -> Self {
        Support { node, dof: [Restraint::Fixed; 6] }
    }
    /// All translations fixed, rotations free.
    pub fn pinned(node: usize) -> Self {
        let mut dof = [Restraint::Free; 6];
        dof[..3].fill(Restraint::Fixed);
        Support { node, dof }
    }
    /// Only the translation along global `axis` (0 = X, 1 = Y, 2 = Z) is fixed.
    pub fn roller(node: usize, axis: usize) -> Self {
        let mut dof = [Restraint::Free; 6];
        dof[axis] = Restraint::Fixed;
        Support { node, dof }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Axes {
    Local,
    Global,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Load {
    /// Nodal force and moment, global axes: [Fx, Fy, Fz, Mx, My, Mz].
    Node { node: usize, f: [f64; 6] },
    /// Concentrated force `f` and moment `m` at distance `a` along the member.
    Point { member: usize, a: f64, f: [f64; 3], m: [f64; 3], axes: Axes },
    /// Linearly varying line load, `w1` at `a` to `w2` at `b` (force per length).
    Dist { member: usize, a: f64, b: f64, w1: [f64; 3], w2: [f64; 3], axes: Axes },
    /// Weight density * A * g on every member, along the global direction `dir`.
    SelfWeight { dir: [f64; 3], g: f64 },
}

impl Load {
    /// Uniform line load over the whole flexible length.
    pub fn udl(member: usize, w: [f64; 3], axes: Axes) -> Load {
        Load::Dist { member, a: 0.0, b: f64::INFINITY, w1: w, w2: w, axes }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct LoadCase {
    pub name: String,
    pub loads: Vec<Load>,
}

/// Linear combination: sum of factor * case result.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Combo {
    pub name: String,
    pub factors: Vec<(usize, f64)>,
}

/// Max/min of every result value over the listed cases and combinations.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Envelope {
    pub name: String,
    pub cases: Vec<usize>,
    pub combos: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Frame {
    pub nodes: Vec<[f64; 3]>,
    pub materials: Vec<Material>,
    pub sections: Vec<Section>,
    pub members: Vec<Member>,
    pub supports: Vec<Support>,
    pub cases: Vec<LoadCase>,
    pub combos: Vec<Combo>,
    pub envelopes: Vec<Envelope>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// Timoshenko shear deformation for members whose section has shear areas.
    pub shear: bool,
    /// Uniform diagram stations per member (>= 2); point-load positions are added.
    pub stations: usize,
    /// Pivot below `pivot_tol` * original diagonal means a mechanism.
    pub pivot_tol: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options { shear: false, stations: 11, pivot_tol: 1e-10 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum FemError {
    Input(String),
    /// Stiffness is singular at this node DOF (0..6 = ux..rz).
    Mechanism { node: usize, dof: usize },
    /// A released local DOF (0..12, end i then end j) carries load but has no stiffness.
    Release { member: usize, dof: usize },
}

const AXIS: [&str; 3] = ["X", "Y", "Z"];
const DOF: [&str; 6] = ["ux", "uy", "uz", "rx", "ry", "rz"];

impl std::fmt::Display for FemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FemError::Input(s) => write!(f, "{s}"),
            FemError::Mechanism { node, dof } if *dof < 3 => {
                write!(f, "node {node} is unrestrained in translation along {}", AXIS[*dof])
            }
            FemError::Mechanism { node, dof } => {
                write!(f, "node {node} is unrestrained in rotation about {}", AXIS[dof % 3])
            }
            FemError::Release { member, dof } => {
                let end = if *dof < 6 { "start" } else { "end" };
                let d = DOF[dof % 6];
                let what = "is loaded but has no stiffness";
                write!(f, "member {member}: released {d} at the {end} {what}")
            }
        }
    }
}

impl std::error::Error for FemError {}
