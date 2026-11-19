// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Joints between rigid parts of an assembly: rigid, revolute, slider, cylindrical, ball, with
//! limits on the rotation and the slide. A joint ties frame `fa` on part `a` to frame `fb` on
//! part `b` (or on the ground, then `fb` is in world coordinates); frames are in part
//! coordinates: origin, axis `z`, reference `x` (for angles and the twist lock).
//! Equations: point coincidence (3), axis parallel (2), point on the axis line (2), twist lock
//! (1). The Jacobian is analytic over 6 variables per moving part (shift, small turn
//! `R ← exp(ω)·R`). Degrees of freedom = 6·moving − rank (`ddc_solver::RowSpace`), so closed
//! loops give their true mobility and dependent equations are reported as redundant.
//! Drive: damped Gauss-Newton with a minimum-norm step (the rest of the assembly moves as
//! little as possible), the driven coordinate clamped to its limits.

use ddc_solver::{gauss, RowSpace};

pub type Vec3 = [f64; 3];
/// Rows.
pub type Mat3 = [[f64; 3]; 3];

pub fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn scale(a: Vec3, k: f64) -> Vec3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
pub fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: Vec3) -> f64 {
    dot(a, a).sqrt()
}
fn unit(a: Vec3) -> Option<Vec3> {
    let n = norm(a);
    (n > 1e-12 && n.is_finite()).then(|| scale(a, 1.0 / n))
}
fn mv(r: &Mat3, v: Vec3) -> Vec3 {
    [dot(r[0], v), dot(r[1], v), dot(r[2], v)]
}
fn mm(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut o = [[0.0; 3]; 3];
    for (i, row) in o.iter_mut().enumerate() {
        for (j, c) in row.iter_mut().enumerate() {
            *c = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    o
}
fn tr(a: &Mat3) -> Mat3 {
    let mut o = [[0.0; 3]; 3];
    for (i, row) in a.iter().enumerate() {
        for (j, v) in row.iter().enumerate() {
            o[j][i] = *v;
        }
    }
    o
}
const I3: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Rotation by the vector `w` (axis × angle, Rodrigues).
pub fn exp(w: Vec3) -> Mat3 {
    let th = norm(w);
    if th < 1e-15 {
        return I3;
    }
    let k = scale(w, 1.0 / th);
    let (s, c) = th.sin_cos();
    let mut r = [[0.0; 3]; 3];
    for (i, row) in r.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            let kx = [[0.0, -k[2], k[1]], [k[2], 0.0, -k[0]], [-k[1], k[0], 0.0]][i][j];
            *v = I3[i][j] * c + s * kx + (1.0 - c) * k[i] * k[j];
        }
    }
    r
}

/// Rigid placement of a part: world = r · local + t.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub t: Vec3,
    pub r: Mat3,
}

impl Pose {
    pub const IDENTITY: Pose = Pose { t: [0.0; 3], r: I3 };

    /// Shift and turn about the vertical (a plan placement).
    pub fn plan(t: Vec3, yaw: f64) -> Pose {
        Pose {
            t,
            r: exp([0.0, 0.0, yaw]),
        }
    }
    pub fn apply(&self, p: Vec3) -> Vec3 {
        add(mv(&self.r, p), self.t)
    }
    pub fn dir(&self, d: Vec3) -> Vec3 {
        mv(&self.r, d)
    }
    /// World point to part coordinates.
    pub fn local(&self, p: Vec3) -> Vec3 {
        mv(&tr(&self.r), sub(p, self.t))
    }
    pub fn local_dir(&self, d: Vec3) -> Vec3 {
        mv(&tr(&self.r), d)
    }
    /// The turn about the vertical, if the pose has no tilt.
    pub fn yaw(&self, tol: f64) -> Option<f64> {
        (self.r[2][2] > 1.0 - tol).then(|| self.r[1][0].atan2(self.r[0][0]))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JointKind {
    Rigid,
    Revolute,
    Slider,
    Cylindrical,
    Ball,
}

impl JointKind {
    pub const ALL: [JointKind; 5] = [
        JointKind::Rigid,
        JointKind::Revolute,
        JointKind::Slider,
        JointKind::Cylindrical,
        JointKind::Ball,
    ];
    pub fn name(self) -> &'static str {
        match self {
            JointKind::Rigid => "rigid",
            JointKind::Revolute => "revolute",
            JointKind::Slider => "slider",
            JointKind::Cylindrical => "cylindrical",
            JointKind::Ball => "ball",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        JointKind::ALL.into_iter().find(|k| k.name() == s)
    }
    /// Has a rotation coordinate about the axis.
    pub fn turns(self) -> bool {
        matches!(self, JointKind::Revolute | JointKind::Cylindrical)
    }
    /// Has a slide coordinate along the axis.
    pub fn slides(self) -> bool {
        matches!(self, JointKind::Slider | JointKind::Cylindrical)
    }
    /// Degrees of freedom the joint leaves between two free parts.
    pub fn freedom(self) -> usize {
        match self {
            JointKind::Rigid => 0,
            JointKind::Revolute | JointKind::Slider => 1,
            JointKind::Cylindrical => 2,
            JointKind::Ball => 3,
        }
    }
}

/// Joint frame in part coordinates; `x` is kept perpendicular to `z`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JFrame {
    pub o: Vec3,
    pub z: Vec3,
    pub x: Vec3,
}

impl JFrame {
    /// Normalized frame; `x` falls back to any perpendicular when it is parallel to `z`.
    pub fn new(o: Vec3, z: Vec3, x: Vec3) -> Option<JFrame> {
        let z = unit(z)?;
        let x = unit(sub(x, scale(z, dot(x, z)))).or_else(|| {
            let a = if z[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
            unit(sub(a, scale(z, dot(a, z))))
        })?;
        o.iter().all(|v| v.is_finite()).then_some(JFrame { o, z, x })
    }

    /// The same frame seen from another pose: world frame of `from`, local to `to`.
    pub fn moved(&self, from: &Pose, to: &Pose) -> JFrame {
        JFrame {
            o: to.local(from.apply(self.o)),
            z: to.local_dir(from.dir(self.z)),
            x: to.local_dir(from.dir(self.x)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Joint {
    pub kind: JointKind,
    pub a: usize,
    /// `None` = ground (then `fb` is in world coordinates).
    pub b: Option<usize>,
    pub fa: JFrame,
    pub fb: JFrame,
    /// Rotation limits (rad), for kinds that turn.
    pub rot: Option<(f64, f64)>,
    /// Slide limits (m), for kinds that slide.
    pub slide: Option<(f64, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Assembly {
    pub poses: Vec<Pose>,
    /// Fixed parts (do not move when solving).
    pub grounded: Vec<bool>,
    pub joints: Vec<Joint>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JointError {
    NoPart(usize),
    SamePart(usize),
    /// The joint has no such coordinate (e.g. a slide on a revolute joint).
    NoCoordinate,
    NotConverged { residual: f64 },
    /// Another joint ends outside its limits.
    Limit { joint: usize, value: f64 },
}

impl std::fmt::Display for JointError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            JointError::NoPart(p) => write!(f, "no part {p}"),
            JointError::SamePart(p) => write!(f, "joint ties part {p} to itself"),
            JointError::NoCoordinate => f.write_str("the joint has no such coordinate"),
            JointError::NotConverged { residual } => {
                write!(f, "joints cannot be satisfied (residual {residual:.2e})")
            }
            JointError::Limit { joint, value } => {
                write!(f, "joint {joint} would leave its limits ({value:.4})")
            }
        }
    }
}

impl std::error::Error for JointError {}

/// One equation: residual and gradients over (shift A, turn A, shift B, turn B).
struct Row {
    r: f64,
    g: [Vec3; 4],
    joint: usize,
}

/// World quantities of one side of a joint.
struct Side {
    p: Vec3,
    lever: Vec3,
    z: Vec3,
    x: Vec3,
}

fn side(pose: &Pose, f: &JFrame) -> Side {
    let lever = pose.dir(f.o);
    Side {
        p: add(lever, pose.t),
        lever,
        z: pose.dir(f.z),
        x: pose.dir(f.x),
    }
}

const Z0: Vec3 = [0.0; 3];

/// Which coordinate of a joint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coord {
    Angle,
    Slide,
}

impl Assembly {
    fn pose(&self, p: Option<usize>) -> Pose {
        p.map_or(Pose::IDENTITY, |i| self.poses[i])
    }

    fn sides(&self, j: &Joint) -> (Side, Side) {
        (side(&self.pose(Some(j.a)), &j.fa), side(&self.pose(j.b), &j.fb))
    }

    pub fn check(&self) -> Result<(), JointError> {
        if self.grounded.len() != self.poses.len() {
            return Err(JointError::NoPart(self.grounded.len()));
        }
        for j in &self.joints {
            for p in std::iter::once(j.a).chain(j.b) {
                if p >= self.poses.len() {
                    return Err(JointError::NoPart(p));
                }
            }
            if j.b == Some(j.a) {
                return Err(JointError::SamePart(j.a));
            }
        }
        Ok(())
    }

    fn rows(&self, out: &mut Vec<Row>) {
        for (ji, j) in self.joints.iter().enumerate() {
            let (a, b) = self.sides(j);
            let ya = cross(a.z, a.x);
            let coincident = |out: &mut Vec<Row>| {
                for e in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
                    out.push(Row {
                        r: dot(sub(b.p, a.p), e),
                        g: [scale(e, -1.0), scale(cross(a.lever, e), -1.0), e, cross(b.lever, e)],
                        joint: ji,
                    });
                }
            };
            let parallel = |out: &mut Vec<Row>| {
                for u in [a.x, ya] {
                    out.push(Row {
                        r: dot(b.z, u),
                        g: [Z0, cross(u, b.z), Z0, cross(b.z, u)],
                        joint: ji,
                    });
                }
            };
            let on_line = |out: &mut Vec<Row>| {
                let d = sub(b.p, a.p);
                for u in [a.x, ya] {
                    out.push(Row {
                        r: dot(d, u),
                        g: [
                            scale(u, -1.0),
                            add(scale(cross(a.lever, u), -1.0), cross(u, d)),
                            u,
                            cross(b.lever, u),
                        ],
                        joint: ji,
                    });
                }
            };
            let twist = |out: &mut Vec<Row>| {
                out.push(Row {
                    r: dot(b.x, ya),
                    g: [Z0, cross(ya, b.x), Z0, cross(b.x, ya)],
                    joint: ji,
                });
            };
            match j.kind {
                JointKind::Rigid => {
                    coincident(out);
                    parallel(out);
                    twist(out);
                }
                JointKind::Revolute => {
                    coincident(out);
                    parallel(out);
                }
                JointKind::Slider => {
                    parallel(out);
                    on_line(out);
                    twist(out);
                }
                JointKind::Cylindrical => {
                    parallel(out);
                    on_line(out);
                }
                JointKind::Ball => coincident(out),
            }
        }
    }

    /// Current value of a joint coordinate (part `a` relative to `b`): angle of A's reference
    /// about B's axis (counter-clockwise looking against it), or the slide of A's origin along it.
    pub fn coordinate(&self, ji: usize, c: Coord) -> Option<f64> {
        let j = self.joints.get(ji)?;
        let (a, b) = self.sides(j);
        match c {
            Coord::Angle if j.kind.turns() => {
                Some(dot(cross(b.x, a.x), b.z).atan2(dot(b.x, a.x)))
            }
            Coord::Slide if j.kind.slides() => Some(dot(sub(a.p, b.p), b.z)),
            _ => None,
        }
    }

    fn coord_row(&self, ji: usize, c: Coord, target: f64) -> Option<Row> {
        let j = &self.joints[ji];
        let (a, b) = self.sides(j);
        let v = self.coordinate(ji, c)?;
        Some(match c {
            Coord::Angle => {
                let pi = std::f64::consts::PI;
                Row {
                    r: (v - target + pi).rem_euclid(std::f64::consts::TAU) - pi,
                    g: [Z0, b.z, Z0, scale(b.z, -1.0)],
                    joint: ji,
                }
            }
            Coord::Slide => {
                let d = sub(a.p, b.p);
                Row {
                    r: v - target,
                    g: [
                        b.z,
                        cross(a.lever, b.z),
                        scale(b.z, -1.0),
                        add(scale(cross(b.lever, b.z), -1.0), cross(b.z, d)),
                    ],
                    joint: ji,
                }
            }
        })
    }

    /// Variable columns of each part (None = fixed): 6 per moving part.
    fn columns(&self, fixed: &[usize]) -> (Vec<Option<usize>>, usize) {
        let mut n = 0;
        let cols = (0..self.poses.len())
            .map(|i| {
                if self.grounded[i] || fixed.contains(&i) {
                    None
                } else {
                    n += 6;
                    Some(n - 6)
                }
            })
            .collect();
        (cols, n)
    }

    fn dense(&self, rows: &[Row], cols: &[Option<usize>], n: usize) -> Vec<Vec<f64>> {
        rows.iter()
            .map(|row| {
                let j = &self.joints[row.joint];
                let mut v = vec![0.0; n];
                for (part, g) in [(Some(j.a), &row.g[..2]), (j.b, &row.g[2..])] {
                    if let Some(c) = part.and_then(|p| cols[p]) {
                        for k in 0..3 {
                            v[c + k] += g[0][k];
                            v[c + 3 + k] += g[1][k];
                        }
                    }
                }
                v
            })
            .collect()
    }

    /// Degrees of freedom at the current poses.
    pub fn dof(&self) -> Result<Dof, JointError> {
        self.check()?;
        let mut rows = vec![];
        self.rows(&mut rows);
        let (cols, n) = self.columns(&[]);
        let jac = self.dense(&rows, &cols, n);
        let mut space = RowSpace::default();
        let mut redundant: Vec<usize> = vec![];
        for (row, v) in rows.iter().zip(&jac) {
            if !space.add(v) && !redundant.contains(&row.joint) {
                redundant.push(row.joint);
            }
        }
        let moving = (0..self.poses.len())
            .filter(|&i| {
                cols[i].is_some_and(|c| (c..c + 6).any(|k| space.coverage(k) < 1.0 - 1e-9))
            })
            .collect();
        Ok(Dof {
            dof: n - space.rank(),
            equations: rows.len(),
            rank: space.rank(),
            redundant,
            moving,
        })
    }

    /// Largest residual of the joint equations.
    pub fn residual(&self) -> f64 {
        let mut rows = vec![];
        self.rows(&mut rows);
        rows.iter().map(|r| r.r.abs()).fold(0.0, f64::max)
    }

    /// Moves joint `ji`'s coordinate to `value` (clamped to its limits); `fixed` parts do not
    /// move. Returns the new poses and the value applied; the assembly itself is unchanged.
    pub fn drive(&self, ji: usize, c: Coord, value: f64, fixed: &[usize]) -> Result<Driven, JointError> {
        self.check()?;
        let j = self.joints.get(ji).ok_or(JointError::NoPart(ji))?;
        self.coordinate(ji, c).ok_or(JointError::NoCoordinate)?;
        let lim = match c {
            Coord::Angle => j.rot,
            Coord::Slide => j.slide,
        };
        let value = lim.map_or(value, |(lo, hi)| value.clamp(lo.min(hi), hi.max(lo)));
        let mut asm = self.clone();
        let (cols, n) = asm.columns(fixed);
        let mut lambda = 1e-9;
        let total = |a: &Assembly| -> Option<(Vec<Row>, f64)> {
            let mut rows = vec![];
            a.rows(&mut rows);
            rows.push(a.coord_row(ji, c, value)?);
            let e = rows.iter().map(|r| r.r * r.r).sum::<f64>();
            Some((rows, e))
        };
        let (mut rows, mut err) = total(&asm).ok_or(JointError::NoCoordinate)?;
        let mut it = 0;
        while it < 200 && rows.iter().any(|r| r.r.abs() > 1e-11) {
            it += 1;
            let jac = asm.dense(&rows, &cols, n);
            let m = rows.len();
            // minimum-norm step: dx = J^T (J J^T + lambda I)^-1 (-r)
            let mut a = vec![0.0; m * m];
            for i in 0..m {
                for k in 0..m {
                    a[i * m + k] = (0..n).map(|q| jac[i][q] * jac[k][q]).sum::<f64>();
                }
                a[i * m + i] += lambda;
            }
            let y = gauss(a, rows.iter().map(|r| -r.r).collect(), m);
            let dx: Vec<f64> = (0..n).map(|q| (0..m).map(|i| jac[i][q] * y[i]).sum()).collect();
            let mut next = asm.clone();
            for (p, c0) in cols.iter().enumerate() {
                if let Some(c0) = *c0 {
                    let pose = &mut next.poses[p];
                    pose.t = add(pose.t, [dx[c0], dx[c0 + 1], dx[c0 + 2]]);
                    pose.r = mm(&exp([dx[c0 + 3], dx[c0 + 4], dx[c0 + 5]]), &pose.r);
                }
            }
            match total(&next) {
                Some((r2, e2)) if e2 < err => {
                    asm = next;
                    rows = r2;
                    err = e2;
                    lambda = (lambda * 0.1).max(1e-15);
                }
                _ => {
                    lambda *= 10.0;
                    if lambda > 1e6 {
                        break;
                    }
                }
            }
        }
        let residual = rows.iter().map(|r| r.r.abs()).fold(0.0, f64::max);
        if residual > 1e-8 {
            return Err(JointError::NotConverged { residual });
        }
        for (k, jk) in asm.joints.iter().enumerate() {
            for (cc, lim) in [(Coord::Angle, jk.rot), (Coord::Slide, jk.slide)] {
                if let (Some((lo, hi)), Some(v)) = (lim, asm.coordinate(k, cc)) {
                    if v < lo.min(hi) - 1e-9 || v > hi.max(lo) + 1e-9 {
                        return Err(JointError::Limit { joint: k, value: v });
                    }
                }
            }
        }
        Ok(Driven {
            poses: asm.poses,
            value,
            iterations: it,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dof {
    pub dof: usize,
    pub equations: usize,
    pub rank: usize,
    /// Joints with at least one equation that depends on earlier ones.
    pub redundant: Vec<usize>,
    /// Parts that can still move.
    pub moving: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Driven {
    pub poses: Vec<Pose>,
    /// The coordinate value applied (after the limits).
    pub value: f64,
    pub iterations: usize,
}

#[cfg(test)]
mod tests;
