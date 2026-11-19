// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! MEP networks: cross-sections (circle, rectangle, flat oval), pipe catalogs
//! (DN/NPS, outer and inner diameter per material), system types with colors, polyline routing
//! with elbows (radius by size), node fittings (tee, cross, reducer,
//! cap), slope, exact quantities, solid sweeps, plan view (single- and double-line),
//! plane sections. Clashes with structure are in [`clash`].
//!
//! Section dimensions are the reference ones: inner for ducts (sheet area is derived from them), outer for pipes and
//! trunking. The duct wall goes outward, the pipe wall inward; insulation is always outside,
//! and the inner duct lining reduces the free cross-section.

pub mod clash;
pub mod duct_sizing;
pub mod fittings;
pub mod route3d;
#[cfg(test)]
mod tests;

use crate::math::{v2, v3, V2, V3};
use crate::mesh::Mesh;
use std::f64::consts::{PI, TAU};

/// Network kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Domain {
    Duct,
    Pipe,
    CableTray,
    Conduit,
}

impl Domain {
    pub const ALL: [Domain; 4] = [
        Domain::Duct,
        Domain::Pipe,
        Domain::CableTray,
        Domain::Conduit,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Domain::Duct => "duct",
            Domain::Pipe => "pipe",
            Domain::CableTray => "cable_tray",
            Domain::Conduit => "conduit",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.name() == s)
    }
}

/// System type: purpose, network kind, plan color (RGB).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SystemKind {
    SupplyAir,
    ReturnAir,
    ExhaustAir,
    ColdWater,
    HotWater,
    Sanitary,
    Heating,
    ChilledWater,
    Sprinkler,
    Power,
    MedicalGas,
}

impl SystemKind {
    pub const ALL: [SystemKind; 11] = [
        SystemKind::SupplyAir,
        SystemKind::ReturnAir,
        SystemKind::ExhaustAir,
        SystemKind::ColdWater,
        SystemKind::HotWater,
        SystemKind::Sanitary,
        SystemKind::Heating,
        SystemKind::ChilledWater,
        SystemKind::Sprinkler,
        SystemKind::Power,
        SystemKind::MedicalGas,
    ];
    pub fn name(self) -> &'static str {
        match self {
            SystemKind::SupplyAir => "supply_air",
            SystemKind::ReturnAir => "return_air",
            SystemKind::ExhaustAir => "exhaust_air",
            SystemKind::ColdWater => "cold_water",
            SystemKind::HotWater => "hot_water",
            SystemKind::Sanitary => "sanitary",
            SystemKind::Heating => "heating",
            SystemKind::ChilledWater => "chilled_water",
            SystemKind::Sprinkler => "sprinkler",
            SystemKind::Power => "power",
            SystemKind::MedicalGas => "medical_gas",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.name() == s)
    }
    pub fn color(self) -> [u8; 3] {
        match self {
            SystemKind::SupplyAir => [0, 112, 192],
            SystemKind::ReturnAir => [255, 0, 255],
            SystemKind::ExhaustAir => [255, 192, 0],
            SystemKind::ColdWater => [0, 176, 240],
            SystemKind::HotWater => [255, 0, 0],
            SystemKind::Sanitary => [128, 96, 0],
            SystemKind::Heating => [192, 0, 0],
            SystemKind::ChilledWater => [0, 96, 160],
            SystemKind::Sprinkler => [255, 64, 64],
            SystemKind::Power => [0, 160, 0],
            SystemKind::MedicalGas => [0, 176, 120],
        }
    }
    /// Allowed network kinds.
    pub fn domains(self) -> &'static [Domain] {
        match self {
            SystemKind::SupplyAir | SystemKind::ReturnAir | SystemKind::ExhaustAir => {
                &[Domain::Duct]
            }
            SystemKind::Power => &[Domain::CableTray, Domain::Conduit],
            _ => &[Domain::Pipe],
        }
    }
    /// IFC: PredefinedType of the system.
    pub fn ifc(self) -> &'static str {
        match self {
            SystemKind::SupplyAir => "VENTILATION",
            SystemKind::ReturnAir => "VENTILATION",
            SystemKind::ExhaustAir => "EXHAUST",
            SystemKind::ColdWater => "DOMESTICCOLDWATER",
            SystemKind::HotWater => "DOMESTICHOTWATER",
            SystemKind::Sanitary => "SEWAGE",
            SystemKind::Heating => "HEATING",
            SystemKind::ChilledWater => "CHILLEDWATER",
            SystemKind::Sprinkler => "FIREPROTECTION",
            SystemKind::Power => "ELECTRICAL",
            // IfcDistributionSystemEnum has no medical value; GAS is the nearest
            SystemKind::MedicalGas => "GAS",
        }
    }
}

/// Cross-section shape, m.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Round {
        d: f64,
    },
    Rect {
        w: f64,
        h: f64,
    },
    /// Flat oval: two semicircles of diameter min(w,h) joined by straight runs.
    Oval {
        w: f64,
        h: f64,
    },
}

impl Shape {
    pub fn area(self) -> f64 {
        match self {
            Shape::Round { d } => PI * d * d / 4.0,
            Shape::Rect { w, h } => w * h,
            Shape::Oval { w, h } => {
                let (a, b) = (w.max(h), w.min(h));
                (a - b) * b + PI * b * b / 4.0
            }
        }
    }
    pub fn perimeter(self) -> f64 {
        match self {
            Shape::Round { d } => PI * d,
            Shape::Rect { w, h } => 2.0 * (w + h),
            Shape::Oval { w, h } => {
                let (a, b) = (w.max(h), w.min(h));
                2.0 * (a - b) + PI * b
            }
        }
    }
    /// Width (along `side`) and height (along `up`).
    pub fn wh(self) -> (f64, f64) {
        match self {
            Shape::Round { d } => (d, d),
            Shape::Rect { w, h } | Shape::Oval { w, h } => (w, h),
        }
    }
    /// Section grown by `t` on each side (negative `t` shrinks it).
    pub fn grow(self, t: f64) -> Shape {
        match self {
            Shape::Round { d } => Shape::Round { d: d + 2.0 * t },
            Shape::Rect { w, h } => Shape::Rect {
                w: w + 2.0 * t,
                h: h + 2.0 * t,
            },
            Shape::Oval { w, h } => Shape::Oval {
                w: w + 2.0 * t,
                h: h + 2.0 * t,
            },
        }
    }
    pub fn valid(self) -> bool {
        let (w, h) = self.wh();
        w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0
    }
    /// Outline in (side, up) coordinates, counter-clockwise; circle and oval use `n` segments per revolution.
    pub fn outline(self, n: usize) -> Vec<V2> {
        match self {
            Shape::Rect { w, h } => vec![
                v2(-w / 2.0, -h / 2.0),
                v2(w / 2.0, -h / 2.0),
                v2(w / 2.0, h / 2.0),
                v2(-w / 2.0, h / 2.0),
            ],
            Shape::Round { d } => (0..n)
                .map(|k| {
                    let a = TAU * k as f64 / n as f64;
                    v2(d / 2.0 * a.cos(), d / 2.0 * a.sin())
                })
                .collect(),
            Shape::Oval { w, h } => {
                let r = w.min(h) / 2.0;
                let half = n / 2;
                let mut out = vec![];
                // semicircle centers lie on the long axis
                let (c1, c2, a0) = if w >= h {
                    (v2(w / 2.0 - r, 0.0), v2(-(w / 2.0 - r), 0.0), -PI / 2.0)
                } else {
                    (v2(0.0, h / 2.0 - r), v2(0.0, -(h / 2.0 - r)), 0.0)
                };
                for (c, a) in [(c1, a0), (c2, a0 + PI)] {
                    for k in 0..=half {
                        let t = a + PI * k as f64 / half as f64;
                        out.push(c + v2(r * t.cos(), r * t.sin()));
                    }
                }
                out
            }
        }
    }
    /// Outline with the same area as the exact section (for exact clash volumes).
    pub fn outline_equal_area(self, n: usize) -> Vec<V2> {
        let o = self.outline(n);
        let a = crate::poly::area(&o);
        let k = (self.area() / a).sqrt();
        o.into_iter().map(|p| p * k).collect()
    }
    /// Size in the bend plane: width if the bend normal is along `up`, otherwise height.
    pub fn in_plane(self, bend_normal_along_up: bool) -> f64 {
        let (w, h) = self.wh();
        if bend_normal_along_up {
            w
        } else {
            h
        }
    }
}

/// Run section: shape, wall, insulation, lining (m).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Section {
    pub domain: Domain,
    pub shape: Shape,
    pub wall: f64,
    pub insulation: f64,
    pub lining: f64,
}

impl Section {
    pub fn new(domain: Domain, shape: Shape) -> Self {
        Section {
            domain,
            shape,
            wall: 0.0,
            insulation: 0.0,
            lining: 0.0,
        }
    }
    /// Outer outline of the wall.
    pub fn outer(&self) -> Shape {
        match self.domain {
            Domain::Duct => self.shape.grow(self.wall),
            _ => self.shape,
        }
    }
    /// Free cross-section (inside the wall and lining).
    pub fn inner(&self) -> Shape {
        match self.domain {
            Domain::Duct => self.shape.grow(-self.lining),
            _ => self.shape.grow(-self.wall - self.lining),
        }
    }
    /// Overall envelope including insulation, used for solids and clashes.
    pub fn envelope(&self) -> Shape {
        self.outer().grow(self.insulation)
    }
}

/// Pipe catalog row: DN, NPS (inches, if any), outer diameter and wall, m.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PipeSize {
    pub dn: u32,
    pub nps: Option<&'static str>,
    pub od: f64,
    pub wall: f64,
}

impl PipeSize {
    pub fn id(&self) -> f64 {
        self.od - 2.0 * self.wall
    }
}

/// Pipe material and its catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PipeMaterial {
    /// Steel, "schedule 40" wall thickness (inch-based outer diameters).
    Steel,
    /// Copper, metric series of outer diameters.
    Copper,
    /// PVC-U non-pressure drainage.
    Pvc,
    /// Polypropylene PN20.
    Pp,
}

const STEEL: &[(u32, &str, f64, f64)] = &[
    (15, "1/2", 21.3, 2.77),
    (20, "3/4", 26.7, 2.87),
    (25, "1", 33.4, 3.38),
    (32, "1-1/4", 42.2, 3.56),
    (40, "1-1/2", 48.3, 3.68),
    (50, "2", 60.3, 3.91),
    (65, "2-1/2", 73.0, 5.16),
    (80, "3", 88.9, 5.49),
    (100, "4", 114.3, 6.02),
    (125, "5", 141.3, 6.55),
    (150, "6", 168.3, 7.11),
    (200, "8", 219.1, 8.18),
    (250, "10", 273.0, 9.27),
    (300, "12", 323.8, 10.31),
];
const COPPER: &[(u32, f64, f64)] = &[
    (10, 12.0, 1.0),
    (15, 15.0, 1.0),
    (20, 22.0, 1.0),
    (25, 28.0, 1.0),
    (32, 35.0, 1.2),
    (40, 42.0, 1.2),
    (50, 54.0, 1.5),
    (65, 76.1, 2.0),
    (80, 88.9, 2.0),
    (100, 108.0, 2.5),
];
const PVC: &[(u32, f64, f64)] = &[
    (32, 32.0, 1.8),
    (40, 40.0, 1.8),
    (50, 50.0, 1.8),
    (75, 75.0, 1.9),
    (100, 110.0, 3.2),
    (125, 125.0, 3.2),
    (150, 160.0, 4.0),
    (200, 200.0, 4.9),
];
const PP: &[(u32, f64, f64)] = &[
    (15, 20.0, 3.4),
    (20, 25.0, 4.2),
    (25, 32.0, 5.4),
    (32, 40.0, 6.7),
    (40, 50.0, 8.3),
    (50, 63.0, 10.5),
    (65, 75.0, 12.5),
    (80, 90.0, 15.0),
    (100, 110.0, 18.3),
];

impl PipeMaterial {
    pub const ALL: [PipeMaterial; 4] = [
        PipeMaterial::Steel,
        PipeMaterial::Copper,
        PipeMaterial::Pvc,
        PipeMaterial::Pp,
    ];
    pub fn name(self) -> &'static str {
        match self {
            PipeMaterial::Steel => "steel",
            PipeMaterial::Copper => "copper",
            PipeMaterial::Pvc => "pvc",
            PipeMaterial::Pp => "pp",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.name() == s)
    }
    pub fn sizes(self) -> Vec<PipeSize> {
        let mm = 0.001;
        match self {
            PipeMaterial::Steel => STEEL
                .iter()
                .map(|&(dn, nps, od, w)| PipeSize {
                    dn,
                    nps: Some(nps),
                    od: od * mm,
                    wall: w * mm,
                })
                .collect(),
            o => {
                let t = match o {
                    PipeMaterial::Copper => COPPER,
                    PipeMaterial::Pvc => PVC,
                    _ => PP,
                };
                t.iter()
                    .map(|&(dn, od, w)| PipeSize {
                        dn,
                        nps: None,
                        od: od * mm,
                        wall: w * mm,
                    })
                    .collect()
            }
        }
    }
    pub fn by_dn(self, dn: u32) -> Option<PipeSize> {
        self.sizes().into_iter().find(|s| s.dn == dn)
    }
    pub fn by_nps(self, nps: &str) -> Option<PipeSize> {
        self.sizes().into_iter().find(|s| s.nps == Some(nps))
    }
    /// Centerline bend radius: steel uses the long radius 1.5·NPS (inches); the others use 1.5·D.
    pub fn bend_radius(self, s: &PipeSize) -> f64 {
        match (self, s.nps) {
            (PipeMaterial::Steel, Some(n)) => 1.5 * nps_inches(n) * 0.0254,
            _ => 1.5 * s.od,
        }
    }
}

/// «1-1/4» → 1.25.
pub fn nps_inches(s: &str) -> f64 {
    let frac = |f: &str| -> f64 {
        match f.split_once('/') {
            Some((a, b)) => a.parse::<f64>().unwrap_or(0.0) / b.parse::<f64>().unwrap_or(1.0),
            None => f.parse().unwrap_or(0.0),
        }
    };
    match s.split_once('-') {
        Some((w, f)) => frac(w) + frac(f),
        None => frac(s),
    }
}

/// Default centerline bend radius: duct is 1.0 × the size in the bend plane
/// (round: 1.5·D); pipe is 1.5·D; cable tray is 1.0 × width; flex/cable conduit is 6·D.
pub fn default_bend_radius(sec: &Section, normal_along_up: bool) -> f64 {
    let s = sec.outer();
    match (sec.domain, s) {
        (Domain::Duct, Shape::Round { d }) => 1.5 * d,
        (Domain::Duct, _) => s.in_plane(normal_along_up),
        (Domain::Pipe, _) => 1.5 * s.wh().0,
        (Domain::CableTray, _) => s.in_plane(normal_along_up),
        (Domain::Conduit, _) => 6.0 * s.wh().0,
    }
}

/// Section frame: `side` (width) and `up` (height), perpendicular to the direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub side: V3,
    pub up: V3,
}

/// Initial frame: for a horizontal run `up` = z; for a vertical run `side` = x.
pub fn start_frame(t: V3) -> Frame {
    if !(t.len() > 1e-12) {
        // zero or non-finite direction: return a finite frame instead of NaN
        return Frame {
            side: v3(1.0, 0.0, 0.0),
            up: v3(0.0, 1.0, 0.0),
        };
    }
    let z = v3(0.0, 0.0, 1.0);
    if t.cross(z).len() < 1e-6 {
        let side = v3(1.0, 0.0, 0.0);
        Frame {
            side,
            up: t.cross(side).normalized(),
        }
    } else {
        let side = z.cross(t).normalized();
        Frame {
            side,
            up: t.cross(side).normalized(),
        }
    }
}

fn rotate(v: V3, axis: V3, a: f64) -> V3 {
    let (s, c) = a.sin_cos();
    v * c + axis.cross(v) * s + axis * (axis.dot(v) * (1.0 - c))
}

/// Route piece: a straight run or an elbow (arc of radius `radius` about `axis`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Piece {
    Straight {
        a: V3,
        b: V3,
        frame: Frame,
    },
    Elbow {
        center: V3,
        start: V3,
        end: V3,
        /// Polyline vertex.
        corner: V3,
        axis: V3,
        radius: f64,
        angle: f64,
        frame: Frame,
    },
}

impl Piece {
    /// Length along the axis.
    pub fn length(&self) -> f64 {
        match *self {
            Piece::Straight { a, b, .. } => (b - a).len(),
            Piece::Elbow { radius, angle, .. } => radius * angle,
        }
    }
    pub fn ends(&self) -> (V3, V3) {
        match *self {
            Piece::Straight { a, b, .. } => (a, b),
            Piece::Elbow { start, end, .. } => (start, end),
        }
    }
    /// Axis points (an arc uses `n` segments per 90°) and the frames at them.
    pub fn samples(&self, n: usize) -> Vec<(V3, Frame)> {
        match *self {
            Piece::Straight { a, b, frame } => vec![(a, frame), (b, frame)],
            Piece::Elbow {
                center,
                start,
                axis,
                angle,
                frame,
                ..
            } => {
                let k = ((angle / (PI / 2.0)) * n as f64).ceil().max(1.0) as usize;
                (0..=k)
                    .map(|i| {
                        let a = angle * i as f64 / k as f64;
                        let p = center + rotate(start - center, axis, a);
                        let f = Frame {
                            side: rotate(frame.side, axis, a),
                            up: rotate(frame.up, axis, a),
                        };
                        (p, f)
                    })
                    .collect()
            }
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum RouteError {
    TooFewPoints,
    NotFinite,
    /// The elbow does not fit: there is not enough length between vertex `at` and the next one.
    TooShort {
        at: usize,
    },
    /// 180° reversal at a vertex.
    Reversal {
        at: usize,
    },
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            RouteError::TooFewPoints => write!(f, "route needs at least 2 points"),
            RouteError::NotFinite => write!(f, "route points must be finite"),
            RouteError::TooShort { at } => {
                write!(f, "segment after vertex {at} too short for bends")
            }
            RouteError::Reversal { at } => write!(f, "reversal at vertex {at}"),
        }
    }
}

/// Polyline route: elbows at every turning vertex, the frame is parallel-transported.
/// `radius(normal_along_up)` gives the elbow radius; `trim` is the setback from the ends (for node fittings).
pub fn route(
    pts: &[V3],
    radius: &dyn Fn(bool) -> f64,
    trim: (f64, f64),
) -> Result<Vec<Piece>, RouteError> {
    let mut p: Vec<V3> = vec![];
    for &q in pts {
        if ![q.x, q.y, q.z].iter().all(|v| v.is_finite()) {
            return Err(RouteError::NotFinite);
        }
        if p.last().is_none_or(|l: &V3| (q - *l).len() > 1e-9) {
            p.push(q);
        }
    }
    // collinear intermediate vertices produce no elbows
    let mut k = 1;
    while k + 1 < p.len() {
        let (a, b) = (
            (p[k] - p[k - 1]).normalized(),
            (p[k + 1] - p[k]).normalized(),
        );
        if a.cross(b).len() < 1e-9 && a.dot(b) > 0.0 {
            p.remove(k);
        } else {
            k += 1;
        }
    }
    if p.len() < 2 {
        return Err(RouteError::TooFewPoints);
    }
    let n = p.len();
    let dir = |i: usize| (p[i + 1] - p[i]).normalized();
    // setbacks at both ends of every segment
    let mut cut = vec![(0.0, 0.0); n - 1];
    cut[0].0 = trim.0;
    cut[n - 2].1 = trim.1;
    let mut frame = start_frame(dir(0));
    let mut bends = vec![];
    for i in 1..n - 1 {
        let (a, b) = (dir(i - 1), dir(i));
        let c = a.dot(b).clamp(-1.0, 1.0);
        if c < -1.0 + 1e-9 {
            return Err(RouteError::Reversal { at: i });
        }
        let ang = c.acos();
        let axis = a.cross(b).normalized();
        bends.push((i, ang, axis));
    }
    // the radius comes from the incoming segment's frame
    let mut elbows = vec![];
    let mut frames = vec![frame];
    for &(i, ang, axis) in &bends {
        let r = radius(axis.cross(frame.up).len() < 1e-6);
        let t = r * (ang / 2.0).tan();
        cut[i - 1].1 += t;
        cut[i].0 += t;
        elbows.push((i, ang, axis, r, t, frame));
        frame = Frame {
            side: rotate(frame.side, axis, ang),
            up: rotate(frame.up, axis, ang),
        };
        frames.push(frame);
    }
    let mut out = vec![];
    for s in 0..n - 1 {
        let l = (p[s + 1] - p[s]).len();
        if cut[s].0 + cut[s].1 > l + 1e-9 {
            return Err(RouteError::TooShort { at: s });
        }
        let d = dir(s);
        if s > 0 {
            let (i, ang, axis, r, t, f) = elbows[s - 1];
            let a = dir(i - 1);
            let start = p[i] - a * t;
            let center = start + axis.cross(a) * r;
            out.push(Piece::Elbow {
                center,
                start,
                end: p[i] + d * t,
                corner: p[i],
                axis,
                radius: r,
                angle: ang,
                frame: f,
            });
        }
        let (a, b) = (p[s] + d * cut[s].0, p[s + 1] - d * cut[s].1);
        if (b - a).len() > 1e-9 {
            out.push(Piece::Straight {
                a,
                b,
                frame: frames[s],
            });
        }
    }
    Ok(out)
}

/// Points sloped by `pct` (%) downward along the route from the first point: each link drops by
/// pct/100 × its plan length; a link steeper than 45° (a joint with a different elevation) keeps its own drop.
pub fn apply_slope(pts: &[V3], pct: f64) -> Vec<V3> {
    let mut out: Vec<V3> = Vec::with_capacity(pts.len());
    for (k, &p) in pts.iter().enumerate() {
        let z = if k == 0 {
            p.z
        } else {
            let q = pts[k - 1];
            let run = v2(p.x - q.x, p.y - q.y).len();
            if run < 1e-9 || (p.z - q.z).abs() > run {
                out[k - 1].z + (p.z - q.z)
            } else {
                out[k - 1].z - pct / 100.0 * run
            }
        };
        out.push(v3(p.x, p.y, z));
    }
    out
}

/// Piece quantities: axis length, surface area (on the reference outline), free-section
/// volume, wall volume and insulation volume. Elbows use Pappus's theorem (exact).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Qty {
    pub length: f64,
    pub surface: f64,
    pub volume: f64,
    pub material: f64,
    pub insulation: f64,
    /// Outer insulation area.
    pub insulation_surface: f64,
}

impl std::ops::AddAssign for Qty {
    fn add_assign(&mut self, o: Qty) {
        self.length += o.length;
        self.surface += o.surface;
        self.volume += o.volume;
        self.material += o.material;
        self.insulation += o.insulation;
        self.insulation_surface += o.insulation_surface;
    }
}

/// Quantities for axis length `l` at a constant section.
pub fn qty_along(sec: &Section, l: f64) -> Qty {
    let (o, i) = (sec.outer(), sec.inner());
    let wall_a = match sec.domain {
        Domain::Duct => o.area() - sec.shape.area(),
        _ => o.area() - sec.shape.grow(-sec.wall).area(),
    };
    let env = sec.envelope();
    Qty {
        length: l,
        surface: sec.shape.perimeter() * l,
        volume: i.area() * l,
        material: wall_a * l,
        insulation: (env.area() - o.area()) * l,
        insulation_surface: if sec.insulation > 0.0 {
            env.perimeter() * l
        } else {
            0.0
        },
    }
}

/// Network node fitting.
#[derive(Clone, Debug, PartialEq)]
pub enum FittingKind {
    Elbow,
    Tee,
    Cross,
    /// Transition between sections (change of size or shape).
    Transition,
    Cap,
}

impl FittingKind {
    pub fn name(&self) -> &'static str {
        match self {
            FittingKind::Elbow => "elbow",
            FittingKind::Tee => "tee",
            FittingKind::Cross => "cross",
            FittingKind::Transition => "transition",
            FittingKind::Cap => "cap",
        }
    }
    /// IFC PredefinedType of the fitting.
    pub fn ifc(&self) -> &'static str {
        match self {
            FittingKind::Elbow => "BEND",
            FittingKind::Tee | FittingKind::Cross => "JUNCTION",
            FittingKind::Transition => "TRANSITION",
            FittingKind::Cap => "OBSTRUCTION",
        }
    }
}

/// Tee/cross branch length from the node center: the largest dimension of the main section.
pub fn junction_leg(main: &Section) -> f64 {
    let (w, h) = main.envelope().wh();
    w.max(h)
}

/// Reducer length: 30° taper (15° per side), at least 50 mm.
pub fn transition_length(a: &Section, b: &Section) -> f64 {
    let (aw, ah) = a.outer().wh();
    let (bw, bh) = b.outer().wh();
    let d = (aw - bw).abs().max((ah - bh).abs());
    (d / 2.0 / (15f64).to_radians().tan()).max(0.05)
}

/// Cap thickness equals the wall, at least 2 mm.
pub fn cap_length(s: &Section) -> f64 {
    s.wall.max(0.002)
}

/// Reducer quantities via the prismatoid formula (exact for circle→circle, a truncated cone).
pub fn transition_qty(a: &Section, b: &Section, l: f64) -> Qty {
    let mid = |f: &dyn Fn(&Section) -> Shape, g: fn(Shape) -> f64| {
        let (x, y) = (f(a), f(b));
        let m = match (x, y) {
            (Shape::Round { d: d1 }, Shape::Round { d: d2 }) => Shape::Round { d: (d1 + d2) / 2.0 },
            _ => {
                let ((w1, h1), (w2, h2)) = (x.wh(), y.wh());
                Shape::Rect {
                    w: (w1 + w2) / 2.0,
                    h: (h1 + h2) / 2.0,
                }
            }
        };
        l / 6.0 * (g(x) + 4.0 * g(m) + g(y))
    };
    let qa = |s: &Section| s.shape;
    let inner = |s: &Section| s.inner();
    let outer = |s: &Section| s.outer();
    let env = |s: &Section| s.envelope();
    let vol_ref = mid(&qa, Shape::area);
    let vol_o = mid(&outer, Shape::area);
    Qty {
        length: l,
        surface: mid(&qa, Shape::perimeter),
        volume: mid(&inner, Shape::area),
        material: match a.domain {
            Domain::Duct => vol_o - vol_ref,
            _ => vol_o - mid(&inner, Shape::area),
        },
        insulation: mid(&env, Shape::area) - vol_o,
        insulation_surface: if a.insulation > 0.0 {
            mid(&env, Shape::perimeter)
        } else {
            0.0
        },
    }
}

/// Closed sweep mesh of section `s` along the axis points with frames (ends capped).
pub fn sweep(s: Shape, path: &[(V3, Frame)], n: usize) -> Mesh {
    let ring = s.outline(n);
    let m = ring.len() as u32;
    let mut mesh = Mesh::default();
    if path.is_empty() {
        return mesh;
    }
    for (p, f) in path {
        for q in &ring {
            mesh.positions.push(*p + f.side * q.x + f.up * q.y);
        }
    }
    let rows = path.len() as u32;
    // the outline is counter-clockwise in (side, up) and side × up = −t: outward faces are (i, i+1) to the next ring
    for r in 0..rows - 1 {
        for i in 0..m {
            let (a, b) = (r * m + i, r * m + (i + 1) % m);
            let (c, d) = (a + m, b + m);
            mesh.triangles.push([a, c, b]);
            mesh.triangles.push([b, c, d]);
        }
    }
    for (row, flip) in [(0, false), (rows - 1, true)] {
        let c = mesh.positions.len() as u32;
        mesh.positions.push(path[row as usize].0);
        for i in 0..m {
            let (a, b) = (row * m + i, row * m + (i + 1) % m);
            mesh.triangles
                .push(if flip { [c, b, a] } else { [c, a, b] });
        }
    }
    // the sweep direction may yield an inside-out mesh, so orient it by the sign of the volume
    if mesh.volume() < 0.0 {
        for t in &mut mesh.triangles {
            t.swap(1, 2);
        }
    }
    mesh.face_of = vec![0; mesh.triangles.len()];
    mesh
}

/// Plane `n·x = c`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    pub n: V3,
    pub c: f64,
}

/// Section of a straight run by a plane: intersection points of the generators (outline of `n` segments),
/// if the plane crosses all generators; otherwise None.
pub fn section_cut(s: Shape, piece: &Piece, plane: Plane, n: usize) -> Option<Vec<V3>> {
    let Piece::Straight { a, b, frame } = *piece else {
        return None;
    };
    let mut out = vec![];
    for q in s.outline(n) {
        let off = frame.side * q.x + frame.up * q.y;
        let (pa, pb) = (a + off, b + off);
        let (da, db) = (plane.n.dot(pa) - plane.c, plane.n.dot(pb) - plane.c);
        if (da > 0.0) == (db > 0.0) || (da - db).abs() < 1e-15 {
            return None;
        }
        out.push(pa + (pb - pa) * (da / (da - db)));
    }
    Some(out)
}

/// Plan level of detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detail {
    /// A single line along the axis.
    Single,
    /// Two lines along the overall width.
    Double,
}

/// Network plan primitive.
#[derive(Clone, Debug, PartialEq)]
pub enum PlanPrim {
    /// Axis (a single line); a dashed centerline in the double-line view.
    Centerline(Vec<V2>),
    /// Outline (overall width) line.
    Edge(Vec<V2>),
    /// Riser: the run goes up at point `at` (section outline and a diagonal).
    Rise { at: V2, outline: Vec<V2> },
    /// Drop: the run goes down (section outline and a cross).
    Drop { at: V2, outline: Vec<V2> },
}

/// Piece plan: horizontal pieces give an axis or two lines; vertical pieces give a riser/drop symbol
/// (`up` is the direction along the route).
pub fn plan(s: Shape, piece: &Piece, detail: Detail, n: usize) -> Vec<PlanPrim> {
    let flat = |p: V3| v2(p.x, p.y);
    if let Piece::Straight { a, b, frame } = *piece {
        let t = (b - a).normalized();
        if (b - a).len() > 1e-12 && t.z.abs() > 0.999 {
            let out: Vec<V2> = s
                .outline(n)
                .iter()
                .map(|q| flat(a + frame.side * q.x + frame.up * q.y))
                .collect();
            let at = flat(a);
            return vec![if t.z > 0.0 {
                PlanPrim::Rise { at, outline: out }
            } else {
                PlanPrim::Drop { at, outline: out }
            }];
        }
    }
    let path = piece.samples(8);
    let center: Vec<V2> = path.iter().map(|(p, _)| flat(*p)).collect();
    match detail {
        Detail::Single => vec![PlanPrim::Centerline(center)],
        Detail::Double => {
            let mut out = vec![PlanPrim::Centerline(center)];
            // half of the overall width in plan, along the horizontal normal to the axis
            for sign in [-1.0, 1.0] {
                let e: Vec<V2> = path
                    .iter()
                    .map(|(p, f)| {
                        let (w, h) = s.wh();
                        // project the frame onto the plan: take the axis closer to horizontal
                        let side = if f.side.z.abs() <= f.up.z.abs() {
                            f.side * (w / 2.0)
                        } else {
                            f.up * (h / 2.0)
                        };
                        flat(*p + side * sign)
                    })
                    .collect();
                out.push(PlanPrim::Edge(e));
            }
            out
        }
    }
}

/// Outline point on a ray from the center at angle `a` (for transitions between shapes).
pub fn radial(s: Shape, a: f64) -> V2 {
    let u = v2(a.cos(), a.sin());
    let t = match s {
        Shape::Round { d } => d / 2.0,
        Shape::Rect { w, h } => {
            let tx = if u.x.abs() > 1e-15 {
                w / 2.0 / u.x.abs()
            } else {
                f64::INFINITY
            };
            let ty = if u.y.abs() > 1e-15 {
                h / 2.0 / u.y.abs()
            } else {
                f64::INFINITY
            };
            tx.min(ty)
        }
        Shape::Oval { w, h } => {
            // the long axis is x; when h > w, rotate the ray
            let (long, short, u) = if w >= h {
                (w, h, u)
            } else {
                (h, w, v2(u.y, u.x))
            };
            let (r, c) = (short / 2.0, long / 2.0 - short / 2.0);
            let flat = if u.y.abs() > 1e-15 {
                r / u.y.abs()
            } else {
                f64::INFINITY
            };
            if (flat * u.x).abs() <= c {
                flat
            } else {
                let cx = c * u.x.signum();
                let b = u.x * cx;
                b + (b * b - cx * cx + r * r).sqrt()
            }
        }
    };
    u * t
}

/// Transition: closed mesh between sections `a` (at `p0`) and `b` (at `p1`), with `n` points along the rays.
pub fn loft(a: Shape, b: Shape, p0: V3, p1: V3, f: Frame, n: usize) -> Mesh {
    let mut m = Mesh::default();
    let n32 = n as u32;
    for (s, p) in [(a, p0), (b, p1)] {
        for k in 0..n {
            let q = radial(s, TAU * k as f64 / n as f64);
            m.positions.push(p + f.side * q.x + f.up * q.y);
        }
    }
    for i in 0..n32 {
        let (x, y) = (i, (i + 1) % n32);
        m.triangles.push([x, x + n32, y]);
        m.triangles.push([y, x + n32, y + n32]);
    }
    for (row, flip, p) in [(0, false, p0), (1, true, p1)] {
        let c = m.positions.len() as u32;
        m.positions.push(p);
        for i in 0..n32 {
            let (x, y) = (row * n32 + i, row * n32 + (i + 1) % n32);
            m.triangles.push(if flip { [c, y, x] } else { [c, x, y] });
        }
    }
    if m.volume() < 0.0 {
        for t in &mut m.triangles {
            t.swap(1, 2);
        }
    }
    m.face_of = vec![0; m.triangles.len()];
    m
}

/// Merges meshes (no boolean operations); `face_of` is the index of the source mesh.
pub fn merge(ms: &[Mesh]) -> Mesh {
    let mut out = Mesh::default();
    for (k, m) in ms.iter().enumerate() {
        let o = out.positions.len() as u32;
        out.positions.extend_from_slice(&m.positions);
        out.triangles
            .extend(m.triangles.iter().map(|t| [t[0] + o, t[1] + o, t[2] + o]));
        out.face_of
            .extend(std::iter::repeat_n(k as u32, m.triangles.len()));
    }
    out
}
