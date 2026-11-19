//! Recognition of an imported mesh as a native element ("convert to native").
//!
//! The body is searched for as a prism: the axis is the normal of a pair of opposing planar faces (or the vertical), all
//! other faces are parallel to the axis, and the end caps lie at the ends. The prism section (exact, mesh_ops)
//! gives the profile; from the axis direction and proportions the profile becomes a wall (axis = thickness,
//! holes and cutouts from below = openings), a slab (vertical axis, thin), a roof (sloped
//! plate), a column/beam (long axis, section = rectangle/circle/I-section/tube/outline),
//! or a generic extrusion. Confidence is the product of prism quality, volume agreement with the
//! native element, and plausibility of proportions; a category hint adds weight, while a foreign
//! category (doors, furniture, panels, equipment...) restricts the native kinds: furniture, doors,
//! panels and glazing never become a wall or column.

use std::f64::consts::PI;

use crate::math::{v2, v3, V2, V3};
use crate::member::Shape;
use crate::mesh::Mesh;
use crate::mesh_ops::{self, simplify, Plane, Region};
use crate::profile::signed_area;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Wall,
    Slab,
    Roof,
    Column,
    Beam,
    Extrusion,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Wall,
        Kind::Slab,
        Kind::Roof,
        Kind::Column,
        Kind::Beam,
        Kind::Extrusion,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Kind::Wall => "wall",
            Kind::Slab => "slab",
            Kind::Roof => "roof",
            Kind::Column => "column",
            Kind::Beam => "beam",
            Kind::Extrusion => "extrusion",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.key() == s)
    }
}

/// Categories that never become a wall or column.
const NOT_WALL_COLUMN: &[&str] = &[
    "furnish",
    "furniture",
    "casework",
    "cabinet",
    "door",
    "window",
    "panel",
    "glazing",
    "glass",
    "plate",
    "pipe",
    "duct",
    "\u{43c}\u{435}\u{431}\u{435}\u{43b}",
    "\u{448}\u{43a}\u{430}\u{444}",
    "\u{434}\u{432}\u{435}\u{440}",
    "\u{43e}\u{43a}\u{43d}",
    "\u{43f}\u{430}\u{43d}\u{435}\u{43b}",
    "\u{43e}\u{441}\u{442}\u{435}\u{43a}\u{43b}",
    "\u{441}\u{442}\u{435}\u{43a}\u{43b}",
    "\u{43f}\u{43b}\u{430}\u{441}\u{442}\u{438}\u{43d}",
    "\u{442}\u{440}\u{443}\u{431}",
    "\u{432}\u{43e}\u{437}\u{434}\u{443}\u{445}\u{43e}\u{432}\u{43e}\u{434}",
];

/// Other foreign categories: native kinds (except extrusion) are capped at [`FOREIGN_CAP`].
const FOREIGN: &[&str] = &[
    "curtain",
    "railing",
    "stair",
    "plumbing",
    "sanitary",
    "equipment",
    "fixture",
    "generic",
    "proxy",
    "cable",
    "flow",
    "terminal",
    "fitting",
    "\u{43d}\u{430}\u{432}\u{435}\u{441}\u{43d}",
    "\u{43e}\u{433}\u{440}\u{430}\u{436}\u{434}",
    "\u{43b}\u{435}\u{441}\u{442}\u{43d}",
    "\u{441}\u{430}\u{43d}\u{442}\u{435}\u{445}",
    "\u{43e}\u{431}\u{43e}\u{440}\u{443}\u{434}",
    "\u{43f}\u{440}\u{438}\u{431}\u{43e}\u{440}",
    "\u{43a}\u{430}\u{431}\u{435}\u{43b}",
];

/// Confidence ceiling for a native kind on a mesh of a foreign category.
pub const FOREIGN_CAP: f64 = 0.4;

/// What the category allows recognition to produce.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Gate {
    #[default]
    Open,
    /// Native kinds (except extrusion) are capped at [`FOREIGN_CAP`].
    Foreign,
    /// Like `Foreign`, and with no wall or column.
    NoWallColumn,
}

pub fn gate_from_category(cat: &str) -> Gate {
    let c = cat.to_lowercase();
    if NOT_WALL_COLUMN.iter().any(|w| c.contains(w)) {
        Gate::NoWallColumn
    } else if FOREIGN.iter().any(|w| c.contains(w)) {
        Gate::Foreign
    } else {
        Gate::Open
    }
}

/// Kind from an import category name (any language in the dictionary); None if it cannot be guessed.
pub fn hint_from_category(cat: &str) -> Option<Kind> {
    let c = cat.to_lowercase();
    let has = |ws: &[&str]| ws.iter().any(|w| c.contains(w));
    if gate_from_category(cat) != Gate::Open {
        None
    } else if has(&["wall", "\u{441}\u{442}\u{435}\u{43d}"]) {
        Some(Kind::Wall)
    } else if has(&["roof", "\u{43a}\u{440}\u{43e}\u{432}\u{43b}", "\u{43a}\u{440}\u{44b}\u{448}"]) {
        Some(Kind::Roof)
    } else if has(&["slab", "floor", "deck", "\u{43f}\u{435}\u{440}\u{435}\u{43a}\u{440}", "\u{43f}\u{43b}\u{438}\u{442}"]) {
        Some(Kind::Slab)
    } else if has(&["column", "pillar", "\u{43a}\u{43e}\u{43b}\u{43e}\u{43d}", "\u{441}\u{442}\u{43e}\u{439}\u{43a}"]) {
        Some(Kind::Column)
    } else if has(&[
        "beam",
        "girder",
        "joist",
        "rafter",
        "member",
        "\u{431}\u{430}\u{43b}\u{43a}",
        "\u{440}\u{438}\u{433}\u{435}\u{43b}",
        "\u{43f}\u{440}\u{43e}\u{433}\u{43e}\u{43d}",
    ]) {
        Some(Kind::Beam)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Face parallelism tolerance, degrees.
    pub angle_tol: f64,
    /// Weld and "on plane" tolerance, m.
    pub tol: f64,
    /// Minimum candidate confidence to be returned.
    pub min_confidence: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            angle_tol: 1.5,
            tol: 1e-6,
            min_confidence: 0.3,
        }
    }
}

/// Wall opening in wall coordinates: from the axis start, from the wall bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitOpening {
    pub along: f64,
    pub width: f64,
    pub sill: f64,
    pub height: f64,
}

/// Generic extrusion: a profile in the plane (origin, x, y) extruded along `dir` by `depth`.
/// Contours are polylines with bulge (tan(angle/4) of the arc to the next vertex, 0 for a segment).
#[derive(Clone, Debug, PartialEq)]
pub struct Extrusion {
    pub origin: V3,
    pub x: V3,
    pub y: V3,
    pub dir: V3,
    pub depth: f64,
    pub outer: Vec<(V2, f64)>,
    pub holes: Vec<Vec<(V2, f64)>>,
}

fn num(v: f64) -> String {
    if v.is_finite() {
        let s = format!("{v}");
        if s == "-0" {
            "0".into()
        } else {
            s
        }
    } else {
        "0".into()
    }
}

fn vec3(v: V3) -> String {
    format!("[{},{},{}]", num(v.x), num(v.y), num(v.z))
}

fn ring_json(r: &[(V2, f64)]) -> String {
    let pts: Vec<String> = r
        .iter()
        .map(|(p, b)| format!("[{},{},{}]", num(p.x), num(p.y), num(*b)))
        .collect();
    format!("[{}]", pts.join(","))
}

impl Extrusion {
    /// Work plane in the parametric-solid format: {"o","x","y","z"}; `o.z` is relative to `z_ref`.
    pub fn plane_json(&self, z_ref: f64) -> String {
        let o = self.origin - v3(0.0, 0.0, z_ref);
        format!(
            "{{\"o\":{},\"x\":{},\"y\":{},\"z\":{}}}",
            vec3(o),
            vec3(self.x),
            vec3(self.y),
            vec3(self.dir)
        )
    }

    /// Solid shape: {"kind":"extrusion","profile":{"outer":[[x,y,bulge]..],"holes":[..]},"start","end"}.
    pub fn def_json(&self) -> String {
        let holes: Vec<String> = self.holes.iter().map(|h| ring_json(h)).collect();
        format!(
            "{{\"kind\":\"extrusion\",\"profile\":{{\"outer\":{},\"holes\":[{}]}},\"start\":0,\"end\":{}}}",
            ring_json(&self.outer),
            holes.join(","),
            num(self.depth)
        )
    }

    /// {"plane": …, "def": …} in world elevations.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"plane\":{},\"def\":{}}}",
            self.plane_json(0.0),
            self.def_json()
        )
    }

    fn to_3d(&self, q: V2) -> V3 {
        self.origin + self.x * q.x + self.y * q.y
    }
}

/// Native element fitted to a mesh (world coordinates, m; angles in rad, slope in °).
#[derive(Clone, Debug, PartialEq)]
pub enum Fit {
    /// Axis at mid-thickness a→b, bottom at `z0`.
    Wall {
        a: V2,
        b: V2,
        z0: f64,
        height: f64,
        thickness: f64,
        openings: Vec<FitOpening>,
        /// Top polyline (u from `a`, h above the bottom; u ascending); None for a flat top.
        top: Option<Vec<(f64, f64)>>,
    },
    Slab {
        outer: Vec<V2>,
        holes: Vec<Vec<V2>>,
        top: f64,
        thickness: f64,
    },
    /// Sloped slab: top `top_z` at `p0`, top gradient (gx, gy); thickness measured vertically.
    SlopedSlab {
        outer: Vec<V2>,
        holes: Vec<Vec<V2>>,
        p0: V2,
        top_z: f64,
        gx: f64,
        gy: f64,
        thickness: f64,
    },
    /// Mono-pitch roof over the top outline: edge `eave` is the sloped eave, the others are gables;
    /// the top at the eave is `base_z`, thickness is measured along the normal.
    Roof {
        ring: Vec<V2>,
        eave: usize,
        slope_deg: f64,
        base_z: f64,
        thickness: f64,
    },
    Column {
        at: V2,
        z0: f64,
        z1: f64,
        shape: Shape,
        rotation: f64,
    },
    /// Axis through the center of the section's bounding box.
    Beam {
        a: V3,
        b: V3,
        shape: Shape,
        rotation: f64,
    },
    Extrusion(Extrusion),
}

impl Fit {
    pub fn kind(&self) -> Kind {
        match self {
            Fit::Wall { .. } => Kind::Wall,
            Fit::Slab { .. } | Fit::SlopedSlab { .. } => Kind::Slab,
            Fit::Roof { .. } => Kind::Roof,
            Fit::Column { .. } => Kind::Column,
            Fit::Beam { .. } => Kind::Beam,
            Fit::Extrusion(_) => Kind::Extrusion,
        }
    }

    /// Volume of the native element.
    pub fn volume(&self) -> f64 {
        match self {
            Fit::Wall {
                a,
                b,
                height,
                thickness,
                openings,
                top,
                ..
            } => {
                let holes: f64 = openings.iter().map(|o| o.width * o.height).sum();
                let face = match top {
                    Some(t) => under(t),
                    None => (*b - *a).len() * height,
                };
                (face - holes) * thickness
            }
            Fit::Slab {
                outer,
                holes,
                thickness,
                ..
            }
            | Fit::SlopedSlab {
                outer,
                holes,
                thickness,
                ..
            } => ring_area(outer, holes) * thickness,
            Fit::Roof {
                ring,
                slope_deg,
                thickness,
                ..
            } => signed_area(ring).abs() / slope_deg.to_radians().cos() * thickness,
            Fit::Column { z0, z1, shape, .. } => shape.area() * (z1 - z0),
            Fit::Beam { a, b, shape, .. } => shape.area() * (*b - *a).len(),
            Fit::Extrusion(e) => {
                let pts = |r: &[(V2, f64)]| r.iter().map(|q| q.0).collect::<Vec<_>>();
                let holes: Vec<Vec<V2>> = e.holes.iter().map(|h| pts(h)).collect();
                ring_area(&pts(&e.outer), &holes) * e.depth
            }
        }
    }

    /// Plan outlines for preview.
    pub fn plan_outline(&self) -> Vec<Vec<V2>> {
        match self {
            Fit::Wall {
                a, b, thickness, ..
            } => {
                let d = *b - *a;
                let l = d.len().max(1e-12);
                let n = v2(-d.y, d.x) * (thickness / 2.0 / l);
                vec![vec![*a - n, *b - n, *b + n, *a + n]]
            }
            Fit::Slab { outer, holes, .. } | Fit::SlopedSlab { outer, holes, .. } => {
                std::iter::once(outer.clone())
                    .chain(holes.iter().cloned())
                    .collect()
            }
            Fit::Roof { ring, .. } => vec![ring.clone()],
            Fit::Column {
                at,
                shape,
                rotation,
                ..
            } => {
                let (c, s) = (rotation.cos(), rotation.sin());
                vec![shape
                    .rings()
                    .0
                    .iter()
                    .map(|p| *at + v2(p.x * c - p.y * s, p.x * s + p.y * c))
                    .collect()]
            }
            Fit::Beam { a, b, shape, .. } => {
                let (w, _) = shape.size();
                let d = v2(b.x - a.x, b.y - a.y);
                let l = d.len();
                if l < 1e-9 {
                    return vec![];
                }
                let n = v2(-d.y, d.x) * (w / 2.0 / l);
                let (pa, pb) = (v2(a.x, a.y), v2(b.x, b.y));
                vec![vec![pa - n, pb - n, pb + n, pa + n]]
            }
            Fit::Extrusion(e) => {
                let ring = |o: V3| -> Vec<V2> {
                    e.outer
                        .iter()
                        .map(|(q, _)| {
                            let p = e.to_3d(*q) + o;
                            v2(p.x, p.y)
                        })
                        .collect()
                };
                vec![ring(v3(0.0, 0.0, 0.0)), ring(e.dir * e.depth)]
            }
        }
    }
}

/// Area under the top polyline (u, h).
fn under(t: &[(f64, f64)]) -> f64 {
    t.windows(2)
        .map(|w| (w[1].0 - w[0].0) * (w[0].1 + w[1].1) / 2.0)
        .sum()
}

fn ring_area(outer: &[V2], holes: &[Vec<V2>]) -> f64 {
    signed_area(outer).abs() - holes.iter().map(|h| signed_area(h).abs()).sum::<f64>()
}

/// Conversion candidate. `notes` holds warning keys (`holes_ignored`, `open_mesh`, …).
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub fit: Fit,
    pub confidence: f64,
    pub mesh_volume: f64,
    pub fit_volume: f64,
    pub notes: Vec<&'static str>,
    /// The kind matches the mesh category: only such candidates are checked automatically.
    pub consistent: bool,
}

impl Candidate {
    pub fn kind(&self) -> Kind {
        self.fit.kind()
    }
}

/// Prism along an axis: end caps at t0/t1, section taken at the middle.
struct Prism {
    axis: V3,
    t0: f64,
    t1: f64,
    mid: Plane,
    region: Region,
    quality: f64,
    closed: bool,
}

impl Prism {
    fn depth(&self) -> f64 {
        self.t1 - self.t0
    }
}

struct Body {
    w: Mesh,
    normals: Vec<(V3, f64)>,
    area: f64,
    volume: f64,
    closed: bool,
}

fn body(m: &Mesh, tol: f64) -> Body {
    let w = mesh_ops::weld(m, tol);
    let normals: Vec<(V3, f64)> = w
        .triangles
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(|k| w.positions[k as usize]);
            let n = (b - a).cross(c - a);
            let l = n.len();
            if l > 0.0 {
                (n * (1.0 / l), l / 2.0)
            } else {
                (v3(0.0, 0.0, 0.0), 0.0)
            }
        })
        .collect();
    let st = mesh_ops::stats(&w, tol);
    Body {
        area: normals.iter().map(|n| n.1).sum(),
        volume: if st.oriented { st.volume.abs() } else { 0.0 },
        closed: st.oriented,
        normals,
        w,
    }
}

/// Planar face groups by normal: (unit normal, area), in descending area order.
fn clusters(b: &Body, cos_tol: f64) -> Vec<(V3, f64)> {
    let mut order: Vec<usize> = (0..b.normals.len()).collect();
    order.sort_by(|&i, &j| b.normals[j].1.total_cmp(&b.normals[i].1));
    let mut cl: Vec<(V3, f64)> = vec![];
    for i in order {
        let (n, a) = b.normals[i];
        if a <= 0.0 {
            continue;
        }
        if let Some(c) = cl.iter_mut().find(|c| c.0.dot(n) > cos_tol) {
            c.1 += a;
        } else if cl.len() < 256 {
            cl.push((n, a));
        }
    }
    cl.sort_by(|p, q| q.1.total_cmp(&p.1));
    cl
}

/// Axis sign: upward, otherwise toward +x, otherwise toward +y.
fn canonical(a: V3) -> V3 {
    let a = a.normalized();
    let flip = if a.z.abs() > 1e-9 {
        a.z < 0.0
    } else if a.x.abs() > 1e-9 {
        a.x < 0.0
    } else {
        a.y < 0.0
    };
    if flip {
        -a
    } else {
        a
    }
}

fn axes(b: &Body, cos_tol: f64) -> Vec<V3> {
    let cl = clusters(b, cos_tol);
    let mut pairs: Vec<(f64, V3)> = vec![];
    for (i, p) in cl.iter().enumerate() {
        for q in &cl[i + 1..] {
            if p.0.dot(q.0) < -cos_tol {
                pairs.push((p.1.min(q.1), (p.0 - q.0).normalized()));
            }
        }
    }
    pairs.sort_by(|p, q| q.0.total_cmp(&p.0));
    pairs.truncate(256);
    // rank is the lateral-surface share: profile ends with fillets are smaller than pairs of fillet faces
    let sin_tol = (1.0 - cos_tol * cos_tol).max(0.0).sqrt();
    let side = |a: V3| -> f64 {
        b.normals
            .iter()
            .filter(|(n, _)| {
                let c = n.dot(a).abs();
                c <= sin_tol || c >= cos_tol
            })
            .map(|x| x.1)
            .sum()
    };
    let mut ranked: Vec<(f64, V3)> = pairs.iter().map(|&(_, a)| (side(a), a)).collect();
    ranked.sort_by(|p, q| q.0.total_cmp(&p.0));
    let mut out = vec![v3(0.0, 0.0, 1.0)];
    for (_, a) in ranked {
        let a = canonical(a);
        if !out.iter().any(|o| o.dot(a).abs() > cos_tol) {
            out.push(a);
        }
        if out.len() > 8 {
            break;
        }
    }
    out
}

fn prism(b: &Body, axis: V3, o: &Options) -> Option<Prism> {
    let (cos_t, sin_t) = (
        o.angle_tol.to_radians().cos(),
        o.angle_tol.to_radians().sin(),
    );
    let ts: Vec<f64> = b.w.positions.iter().map(|p| p.dot(axis)).collect();
    let (t0, t1) = ts
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, c), &t| (a.min(t), c.max(t)));
    let depth = t1 - t0;
    if !(depth > o.tol * 10.0) || b.area <= 0.0 {
        return None;
    }
    let lt = (depth * 1e-3).max(o.tol * 10.0);
    let mut good = 0.0;
    for (t, &(n, a)) in b.w.triangles.iter().zip(&b.normals) {
        let c = n.dot(axis).abs();
        if c >= cos_t {
            let at = |e: f64| t.iter().all(|&k| (ts[k as usize] - e).abs() <= lt);
            if at(t0) || at(t1) {
                good += a;
            }
        } else if c <= sin_t {
            good += a;
        } else if c <= MITER_COS {
            // angled end cap (walls mitered at corners): counted at half weight
            good += a * 0.5;
        }
    }
    let side = good / b.area;
    if side < 0.9 {
        return None;
    }
    let tm = (t0 + t1) / 2.0;
    let mid = Plane { n: axis, d: tm };
    let sec = mesh_ops::section_welded(&b.w, &mid, o.tol);
    if sec.regions.len() != 1 || !sec.open.is_empty() {
        return None;
    }
    let region = sec.regions.into_iter().next()?;
    let am = region.area();
    if am <= o.tol * o.tol {
        return None;
    }
    // sections inside the body: a miter or bevel of the end cap shifts the outline no farther than the distance from
    // the middle (45°), i.e. the area changes by at most perimeter × that distance
    let per = perimeter(&region.outer) + region.holes.iter().map(|h| perimeter(h)).sum::<f64>();
    let mut cons = 1.0f64;
    for f in [0.2, 0.8] {
        let pl = Plane {
            n: axis,
            d: t0 + depth * f,
        };
        let a = mesh_ops::section_welded(&b.w, &pl, o.tol).area();
        let span = (f - 0.5f64).abs() * depth;
        let dev = (a - am).abs() / (per * span).max(am * 1e-3);
        cons = cons.min((1.0 - 0.05 * dev.min(1.0) - (dev - 1.0).max(0.0)).clamp(0.0, 1.0));
    }
    let pv = am * depth;
    let vol = if b.closed {
        pv.min(b.volume) / pv.max(b.volume)
    } else {
        0.85
    };
    Some(Prism {
        axis,
        t0,
        t1,
        mid,
        region,
        quality: side * side * vol * cons,
        closed: b.closed,
    })
}

/// |cos| between the end-cap normal and the axis below which the cap counts as a miter (~41°).
const MITER_COS: f64 = 0.75;

fn perimeter(r: &[V2]) -> f64 {
    (0..r.len())
        .map(|i| (r[(i + 1) % r.len()] - r[i]).len())
        .sum()
}

/// Minimum-area bounding box over edge directions: (angle, x0, x1, y0, y1) in the rotated axes.
fn obb(r: &[V2]) -> (f64, f64, f64, f64, f64) {
    let mut best = (0.0, 0.0, 0.0, 0.0, 0.0);
    let mut best_a = f64::MAX;
    let n = r.len();
    for i in 0..n {
        let d = r[(i + 1) % n] - r[i];
        if d.len() < 1e-12 {
            continue;
        }
        let th = d.y.atan2(d.x);
        let (c, s) = (th.cos(), th.sin());
        let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for p in r {
            let (x, y) = (p.x * c + p.y * s, -p.x * s + p.y * c);
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
        let a = (x1 - x0) * (y1 - y0);
        if a < best_a - 1e-12 {
            best_a = a;
            best = (th, x0, x1, y0, y1);
        }
    }
    best
}

fn rot(p: V2, th: f64) -> V2 {
    let (c, s) = (th.cos(), th.sin());
    v2(p.x * c - p.y * s, p.x * s + p.y * c)
}

/// Angle wrapped into (−lim, lim] with period 2·lim.
fn wrap(a: f64, lim: f64) -> f64 {
    let p = 2.0 * lim;
    let mut a = (a + lim).rem_euclid(p) - lim;
    if a <= -lim + 1e-12 {
        a += p;
    }
    a
}

/// A circle is recognized from an inscribed polygon of at least 16 vertices (an octagon is
/// a polygon, not a circle).
fn circle_fit(r: &[V2]) -> Option<(V2, f64)> {
    if r.len() < 16 {
        return None;
    }
    let c = r.iter().fold(v2(0.0, 0.0), |s, p| s + *p) * (1.0 / r.len() as f64);
    let rs: Vec<f64> = r.iter().map(|p| (*p - c).len()).collect();
    let mean = rs.iter().sum::<f64>() / rs.len() as f64;
    let dev = rs.iter().map(|x| (x - mean).abs()).fold(0.0, f64::max);
    (mean > 0.0 && dev / mean < 0.01).then_some((c, mean))
}

/// Member section in (ey, ez) axes: (shape, bounding-box center, rotation, quality).
fn fit_shape(reg: &Region, tol: f64) -> Option<(Shape, V2, f64, f64)> {
    let r = simplify(&reg.outer, tol, true);
    if r.len() < 3 {
        return None;
    }
    let area = signed_area(&r).abs();
    let (th, x0, x1, y0, y1) = obb(&r);
    let (bw, bh) = (x1 - x0, y1 - y0);
    let center = rot(v2((x0 + x1) / 2.0, (y0 + y1) / 2.0), th);
    let lt = (bw.max(bh) * 1e-3).max(tol * 10.0);
    match reg.holes.len() {
        0 => {}
        1 => {
            let h = simplify(&reg.holes[0], tol, true);
            let ha = signed_area(&h).abs();
            if let (Some((co, ro)), Some((ci, ri))) = (circle_fit(&r), circle_fit(&h)) {
                if (co - ci).len() < lt && ri < ro {
                    let s = Shape::Pipe {
                        d: 2.0 * ro,
                        t: ro - ri,
                    };
                    let q = (area - ha) / (PI * (ro * ro - ri * ri));
                    return Some((s, co, 0.0, q.min(1.0)));
                }
            }
            if r.len() == 4 && h.len() == 4 && (area / (bw * bh) - 1.0).abs() < 1e-3 {
                let tx = (bw * bh - ha) / (2.0 * (bw + bh));
                let s = rect_shape(bw, bh, th, |b, h| Shape::Tube { h, b, t: tx });
                let net = s.0.area();
                if net > 0.0 && ((area - ha) - net).abs() / net < 0.01 {
                    return Some((s.0, center, s.1, 1.0));
                }
            }
            return None;
        }
        _ => return None,
    }
    if r.len() == 4 && (area / (bw * bh) - 1.0).abs() < 1e-3 {
        let (s, rotation) = rect_shape(bw, bh, th, |b, h| Shape::Rect { b, h });
        return Some((s, center, rotation, 1.0));
    }
    if let Some((c, rad)) = circle_fit(&r) {
        let ratio = area / (PI * rad * rad);
        // a coarse polygon loses the circle's area: confidence drops faster
        let q = if ratio < 0.97 { ratio * ratio } else { ratio };
        return Some((Shape::Circle { d: 2.0 * rad }, c, 0.0, q.min(1.0)));
    }
    let loc: Vec<V2> = r.iter().map(|p| rot(*p, -th)).collect();
    // flanges along x (b = Δx) or along y
    for b_along_x in [true, false] {
        let pts: Vec<V2> = if b_along_x {
            loc.clone()
        } else {
            loc.iter().map(|p| v2(p.y, p.x)).collect()
        };
        if let Some((s, q)) = i_section(&pts, area, lt) {
            let rotation = if b_along_x { th } else { th + PI / 2.0 };
            return Some((s, center, wrap(rotation, PI / 2.0), q));
        }
    }
    // unrotated outline: Shape::Poly is centered on the axis-aligned bounding box, not the rotated one
    let (ax0, ax1, ay0, ay1) = bbox(&r);
    let center = v2((ax0 + ax1) / 2.0, (ay0 + ay1) / 2.0);
    let pts: Vec<(f64, f64)> = r
        .iter()
        .map(|p| {
            let q = *p - center;
            (q.x, q.y)
        })
        .collect();
    let s = Shape::Poly(pts);
    s.check().ok()?;
    Some((s, center, 0.0, 0.85))
}

/// Outline width on the horizontal line y: (sum of segments, segment count, midpoint of the first).
fn chord(r: &[V2], y: f64) -> (f64, usize, f64) {
    let n = r.len();
    let mut xs: Vec<f64> = (0..n)
        .filter_map(|i| {
            let (a, b) = (r[i], r[(i + 1) % n]);
            ((a.y > y) != (b.y > y)).then(|| a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y))
        })
        .collect();
    xs.sort_by(f64::total_cmp);
    let w = xs.chunks_exact(2).map(|c| c[1] - c[0]).sum();
    let mid = if xs.len() >= 2 {
        (xs[0] + xs[1]) / 2.0
    } else {
        0.0
    };
    (w, xs.len() / 2, mid)
}

/// I-section from section widths (flanges along x): a flange is a full-width band at the face, the web is
/// a constant width in the middle and centered. Fillets at the web root are tolerated: the outline
/// area is then larger than the I-section area (by up to 10 %), with quality 0.95.
fn i_section(loc: &[V2], area: f64, lt: f64) -> Option<(Shape, f64)> {
    let (x0, x1, y0, y1) = bbox(loc);
    let (b, h) = (x1 - x0, y1 - y0);
    if !(b > 0.0 && h > 0.0) || area <= 0.0 {
        return None;
    }
    let mids: Vec<(f64, usize, f64)> = (0..=10)
        .map(|k| chord(loc, y0 + h * (0.35 + 0.03 * k as f64)))
        .collect();
    let (tw, _, wc) = mids[5];
    let web = mids.iter().all(|c| c.1 == 1 && (c.0 - tw).abs() <= lt);
    if !web || tw >= 0.6 * b || (wc - (x0 + x1) / 2.0).abs() > lt {
        return None;
    }
    let full = |y: f64| {
        let c = chord(loc, y);
        c.1 == 1 && c.0 >= b - lt
    };
    // flange thickness is the edge of the full-width band (found by bisection)
    let band = |face: f64, dir: f64| -> f64 {
        let (mut lo, mut hi) = (h * 1e-6, h * 0.35);
        if !full(face + dir * lo) || full(face + dir * hi) {
            return 0.0;
        }
        for _ in 0..60 {
            let m = (lo + hi) / 2.0;
            if full(face + dir * m) {
                lo = m;
            } else {
                hi = m;
            }
        }
        (lo + hi) / 2.0
    };
    let (tf, tf2) = (band(y0, 1.0), band(y1, -1.0));
    if tf <= 0.0 || (tf - tf2).abs() > lt {
        return None;
    }
    let s = Shape::I { h, b, tw, tf };
    s.check().ok()?;
    let rel = (area - s.area()) / s.area();
    if rel.abs() < 0.01 {
        Some((s, 1.0))
    } else if (0.0..0.10).contains(&rel) {
        Some((s, 0.95))
    } else {
        None
    }
}

/// Rectangular b×h section from angle th: rotation in (−π/4, π/4]; a 90° rotation swaps b↔h.
fn rect_shape(bw: f64, bh: f64, th: f64, mk: impl Fn(f64, f64) -> Shape) -> (Shape, f64) {
    let r = wrap(th, PI / 2.0);
    if r.abs() <= PI / 4.0 + 1e-12 {
        (mk(bw, bh), r)
    } else {
        (mk(bh, bw), wrap(r - PI / 2.0, PI / 2.0))
    }
}

/// Outline through a plane: from the section basis into (ey, ez) axes with origin `o`.
fn remap(r: &Region, pl: &Plane, o: V3, ey: V3, ez: V3) -> Region {
    let f = |q: &V2| {
        let p = pl.to_3d(*q) - o;
        v2(p.dot(ey), p.dot(ez))
    };
    Region {
        outer: r.outer.iter().map(f).collect(),
        holes: r.holes.iter().map(|h| h.iter().map(f).collect()).collect(),
    }
}

/// Member section basis for axis `u` (as in ddc_geom::member): ey points left, ez = u × ey.
fn member_frame(u: V3) -> (V3, V3) {
    let z = v3(0.0, 0.0, 1.0);
    let h = z.cross(u);
    let ey = if h.len() < 1e-9 {
        v3(1.0, 0.0, 0.0)
    } else {
        h.normalized()
    };
    (ey, u.cross(ey))
}

fn bbox(r: &[V2]) -> (f64, f64, f64, f64) {
    r.iter().fold(
        (f64::MAX, f64::MIN, f64::MAX, f64::MIN),
        |(a, b, c, d), p| (a.min(p.x), b.max(p.x), c.min(p.y), d.max(p.y)),
    )
}

fn wall(p: &Prism, o: &Options) -> Option<(Fit, f64, Vec<&'static str>)> {
    let t = p.depth();
    let (x0, x1, z0, z1) = bbox(&p.region.outer);
    let (len, h) = (x1 - x0, z1 - z0);
    // shorter than three thicknesses: a pier, not a wall
    if !(0.02..=1.2).contains(&t) || len < (3.0 * t).max(0.2) || h < (1.5 * t).max(0.2) {
        return None;
    }
    let lt = (h.max(len) * 1e-4).max(o.tol * 10.0);
    let ring = &p.region.outer;
    let n = ring.len();
    // bottom: outline segments at z0; gaps between them are cutouts from below (doors)
    let mut bottom: Vec<(f64, f64)> = (0..n)
        .filter_map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            ((a.y - z0).abs() <= lt && (b.y - z0).abs() <= lt).then(|| (a.x.min(b.x), a.x.max(b.x)))
        })
        .collect();
    bottom.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut gaps = vec![];
    let mut end = x0;
    for (a, b) in bottom {
        if a - end > lt {
            gaps.push((end, a));
        }
        end = end.max(b);
    }
    if x1 - end > lt {
        gaps.push((end, x1));
    }
    let mut openings = vec![];
    let mut notch = 0.0;
    let mut notes = vec![];
    for (g0, g1) in gaps {
        let top = ring
            .iter()
            .filter(|q| q.x >= g0 - lt && q.x <= g1 + lt && q.y > z0 + lt)
            .map(|q| q.y)
            .fold(f64::MAX, f64::min);
        if top >= z1 - lt || top == f64::MAX {
            // a full-height cutout means this is more than one wall
            return None;
        }
        notch += (g1 - g0) * (top - z0);
        openings.push(FitOpening {
            along: g0 - x0,
            width: g1 - g0,
            sill: 0.0,
            height: top - z0,
        });
    }
    let mut holes_q = 1.0f64;
    for hole in &p.region.holes {
        let (hx0, hx1, hy0, hy1) = bbox(hole);
        let ba = (hx1 - hx0) * (hy1 - hy0);
        if ba > 0.0 {
            holes_q = holes_q.min(signed_area(hole).abs() / ba);
        }
        openings.push(FitOpening {
            along: hx0 - x0,
            width: hx1 - hx0,
            sill: hy0 - z0,
            height: hy1 - hy0,
        });
    }
    if holes_q < 0.98 {
        notes.push("openings_approx");
    }
    openings.sort_by(|a, b| a.along.total_cmp(&b.along));
    // gable, slope, stepped top becomes the native wall's top profile
    let top: Option<Vec<(f64, f64)>> = top_chain(ring, x0, x1, lt)
        .map(|c| c.iter().map(|q| (q.x - x0, q.y - z0)).collect::<Vec<_>>())
        .filter(|t: &Vec<(f64, f64)>| {
            t.iter().all(|p| p.1 > lt) && t.iter().any(|p| (p.1 - h).abs() > lt)
        });
    let face = top.as_deref().map_or(len * h, under);
    let rect_q = ((signed_area(ring).abs() + notch) / face).min(1.0);
    if rect_q < 0.98 {
        notes.push(if top.is_some() {
            "outline_approx"
        } else {
            "top_not_flat"
        });
    }
    let to_plan = |x: f64| {
        let q = p.mid.to_3d(v2(x, 0.0));
        v2(q.x, q.y)
    };
    let fit = Fit::Wall {
        a: to_plan(x0),
        b: to_plan(x1),
        z0,
        height: h,
        thickness: t,
        openings,
        top,
    };
    let mut prior = if h >= 1.0 {
        1.0
    } else {
        0.6 + 0.4 * (h - 0.2) / 0.8
    };
    // a thin short plate is more likely a door leaf or a panel
    if t < 0.05 && len < 1.2 {
        prior *= 0.5;
    }
    Some((fit, rect_q * rect_q * holes_q * prior, notes))
}

/// Top of the wall outline (counter-clockwise winding): from the top-right vertex to the top-left,
/// with x ascending; None if the top is not monotonic in x.
fn top_chain(ring: &[V2], x0: f64, x1: f64, lt: f64) -> Option<Vec<V2>> {
    let n = ring.len();
    let pick = |x: f64| {
        (0..n)
            .filter(|&i| (ring[i].x - x).abs() <= lt)
            .max_by(|&i, &j| ring[i].y.total_cmp(&ring[j].y))
    };
    let (s, e) = (pick(x1)?, pick(x0)?);
    let mut out = vec![ring[s]];
    let mut i = s;
    while i != e {
        i = (i + 1) % n;
        let p = ring[i];
        if p.x > out[out.len() - 1].x + lt || out.len() > n {
            return None;
        }
        out.push(p);
    }
    out.reverse();
    Some(out)
}

fn slab(p: &Prism) -> Option<(Fit, f64, Vec<&'static str>)> {
    let t = p.depth();
    let (_, x0, x1, y0, y1) = obb(&p.region.outer);
    let w = (x1 - x0).min(y1 - y0);
    if t > 1.2 || w < (3.0 * t).max(0.3) {
        return None;
    }
    let fit = Fit::Slab {
        outer: p.region.outer.clone(),
        holes: p.region.holes.clone(),
        top: p.t1,
        thickness: t,
    };
    Some((fit, if t <= 0.6 { 1.0 } else { 0.7 }, vec![]))
}

fn column(p: &Prism, o: &Options) -> Option<(Fit, f64, Vec<&'static str>)> {
    let h = p.depth();
    let (sh, c, rotation, q) = fit_shape(&p.region, o.tol)?;
    let (bw, bh) = sh.size();
    let s = bw.max(bh);
    if s > 2.0 || h < (2.0 * s).max(0.5) {
        return None;
    }
    let fit = Fit::Column {
        at: c,
        z0: p.t0,
        z1: p.t1,
        shape: sh,
        rotation,
    };
    let slender = if h >= 4.0 * s { 1.0 } else { 0.8 };
    // below human height: more likely a cabinet or wardrobe
    let tall = if h >= 1.8 { 1.0 } else { 0.7 };
    Some((fit, q * slender * tall, vec![]))
}

fn beam(p: &Prism, o: &Options) -> Option<(Fit, f64, Vec<&'static str>)> {
    let u = p.axis;
    if u.z.abs() > 0.9 {
        return None;
    }
    let (ey, ez) = member_frame(u);
    let origin = u * p.mid.d;
    let reg = remap(&p.region, &p.mid, origin, ey, ez);
    let (sh, c, rotation, q) = fit_shape(&reg, o.tol)?;
    let (bw, bh) = sh.size();
    let s = bw.max(bh);
    let l = p.depth();
    if s > 1.5 || l < 3.0 * s {
        return None;
    }
    let mid = origin + ey * c.x + ez * c.y;
    let fit = Fit::Beam {
        a: mid + u * (p.t0 - p.mid.d),
        b: mid + u * (p.t1 - p.mid.d),
        shape: sh,
        rotation,
    };
    Some((fit, q * if l >= 6.0 * s { 1.0 } else { 0.8 }, vec![]))
}

/// Sloped plate: a roof (eave ⟂ slope) or a sloped slab.
fn plate(p: &Prism, o: &Options) -> Vec<(Fit, f64, Vec<&'static str>)> {
    let a = p.axis;
    let t = p.depth();
    let (_, x0, x1, y0, y1) = obb(&p.region.outer);
    let tilt = a.z.clamp(-1.0, 1.0).acos();
    if t > 0.6
        || (x1 - x0).min(y1 - y0) < 3.0 * t
        || !(3f64.to_radians()..=75f64.to_radians()).contains(&tilt)
    {
        return vec![];
    }
    let top = |q: &V2| {
        let p3 = p.mid.to_3d(*q) + a * (p.t1 - p.mid.d);
        (v2(p3.x, p3.y), p3.z)
    };
    let mut ring: Vec<(V2, f64)> = p.region.outer.iter().map(top).collect();
    if signed_area(&ring.iter().map(|q| q.0).collect::<Vec<_>>()) < 0.0 {
        ring.reverse();
    }
    let holes: Vec<Vec<V2>> = p
        .region
        .holes
        .iter()
        .map(|h| h.iter().map(|q| top(q).0).collect())
        .collect();
    let plan: Vec<V2> = ring.iter().map(|q| q.0).collect();
    let (gx, gy) = (-a.x / a.z, -a.y / a.z);
    let mut out = vec![];
    let sloped = Fit::SlopedSlab {
        outer: plan.clone(),
        holes: holes.clone(),
        p0: ring[0].0,
        top_z: ring[0].1,
        gx,
        gy,
        thickness: t / a.z,
    };
    out.push((sloped, 0.9, vec![]));
    let g = v2(a.x, a.y);
    let gl = g.len();
    if gl < 1e-9 {
        return out;
    }
    let g = g * (1.0 / gl);
    let n = plan.len();
    let sin_t = o.angle_tol.to_radians().sin();
    let far = plan.iter().map(|q| q.dot(g)).fold(f64::MIN, f64::max);
    let lt = ((x1 - x0).max(y1 - y0) * 1e-4).max(o.tol * 10.0);
    let eave = (0..n).find(|&i| {
        let (pa, pb) = (plan[i], plan[(i + 1) % n]);
        let d = pb - pa;
        let l = d.len();
        l > lt && (d.dot(g) / l).abs() <= sin_t && (pa.dot(g) - far).abs() <= lt
    });
    if let Some(e) = eave {
        let mut notes = vec![];
        if !holes.is_empty() {
            notes.push("holes_ignored");
        }
        let roof = Fit::Roof {
            base_z: (ring[e].1 + ring[(e + 1) % n].1) / 2.0,
            ring: plan,
            eave: e,
            slope_deg: tilt.to_degrees(),
            thickness: t,
        };
        out.push((roof, if holes.is_empty() { 1.0 } else { 0.8 }, notes));
    }
    out
}

/// Vertices with bulge: runs of ≥ 3 equal edges with equal turning become arcs.
pub fn with_arcs(ring: &[V2], tol: f64) -> Vec<(V2, f64)> {
    let n = ring.len();
    let plain = || ring.iter().map(|p| (*p, 0.0)).collect::<Vec<_>>();
    if n < 6 {
        return plain();
    }
    let len: Vec<f64> = (0..n)
        .map(|i| (ring[(i + 1) % n] - ring[i]).len())
        .collect();
    // turn at vertex i+1 between edges i and i+1
    let turn: Vec<f64> = (0..n)
        .map(|i| {
            let (a, b) = (
                ring[(i + 1) % n] - ring[i],
                ring[(i + 2) % n] - ring[(i + 1) % n],
            );
            a.cross(b).atan2(a.dot(b))
        })
        .collect();
    // at most 22.5° per edge (16 or more sides): an octagon stays a polygon
    let arcish = |t: f64| t.abs() > 0.5f64.to_radians() && t.abs() <= 23f64.to_radians();
    // edge j continues the run started by edge e: same length and same turn before it
    let cont = |e: usize, j: usize| {
        (len[j] - len[e]).abs() <= len[e] * 0.01 + tol
            && (turn[(j + n - 1) % n] - turn[e]).abs() <= 0.5f64.to_radians()
            && arcish(turn[e])
    };
    let Some(s0) = (0..n).find(|&i| !cont((i + n - 1) % n, i)) else {
        // full circle: two semicircles
        let h = n / 2;
        let b = (turn[0] * h as f64 / 4.0).tan();
        let b2 = (turn[0] * (n - h) as f64 / 4.0).tan();
        return vec![(ring[0], b), (ring[h], b2)];
    };
    let mut out = vec![];
    let mut i = 0;
    while i < n {
        let e = (s0 + i) % n;
        let mut k = 1;
        while i + k < n && cont(e, (e + k) % n) {
            k += 1;
        }
        if k >= 3 {
            // the central angle of an inscribed-polygon edge equals the turn
            out.push((ring[e], (turn[e] * k as f64 / 4.0).tan()));
        } else {
            for j in 0..k {
                out.push((ring[(e + j) % n], 0.0));
            }
        }
        i += k;
    }
    out
}

fn extrusion(p: &Prism, o: &Options) -> Fit {
    let (orig, x, y) = p.mid.frame();
    let shift = p.axis * (p.t0 - p.mid.d);
    Fit::Extrusion(Extrusion {
        origin: orig + shift,
        x,
        y,
        dir: p.axis,
        depth: p.depth(),
        outer: with_arcs(&p.region.outer, o.tol),
        holes: p.region.holes.iter().map(|h| with_arcs(h, o.tol)).collect(),
    })
}

/// Candidates in descending confidence, best one per kind; `hint` is the kind from the import category.
pub fn recognize(m: &Mesh, hint: Option<Kind>, o: &Options) -> Vec<Candidate> {
    recognize_gated(m, hint, Gate::Open, o)
}

/// Recognition aware of the import category: kind hint and restrictions for foreign categories;
/// `hint` (Some) overrides the category hint.
pub fn recognize_category(
    m: &Mesh,
    category: &str,
    hint: Option<Kind>,
    o: &Options,
) -> Vec<Candidate> {
    let hint = hint.or_else(|| hint_from_category(category));
    recognize_gated(m, hint, gate_from_category(category), o)
}

pub fn recognize_gated(m: &Mesh, hint: Option<Kind>, gate: Gate, o: &Options) -> Vec<Candidate> {
    let b = body(m, o.tol);
    if b.w.triangles.len() < 4 {
        return vec![];
    }
    let cos_t = o.angle_tol.to_radians().cos();
    let mut raw: Vec<(Fit, f64, Vec<&'static str>)> = vec![];
    for axis in axes(&b, cos_t) {
        let Some(p) = prism(&b, axis, o) else {
            continue;
        };
        let q = p.quality;
        let mut push = |r: Option<(Fit, f64, Vec<&'static str>)>| {
            if let Some((f, s, mut n)) = r {
                if !p.closed {
                    n.push("open_mesh");
                }
                raw.push((f, q * s, n));
            }
        };
        let vertical = axis.z.abs() >= cos_t;
        let horizontal = axis.z.abs() <= o.angle_tol.to_radians().sin();
        if vertical {
            push(slab(&p));
            push(column(&p, o));
        } else {
            if horizontal {
                push(wall(&p, o));
            } else {
                for r in plate(&p, o) {
                    push(Some(r));
                }
            }
            push(beam(&p, o));
        }
        push(Some((extrusion(&p, o), 0.75, vec![])));
    }
    // sort key without clamping at 1, so the hint can distinguish ties
    let mut best: Vec<(f64, Candidate)> = vec![];
    for (fit, s, notes) in raw {
        let kind = fit.kind();
        if gate == Gate::NoWallColumn && matches!(kind, Kind::Wall | Kind::Column) {
            continue;
        }
        let fv = fit.volume();
        let mv = if b.closed { b.volume } else { fv };
        let vq = if mv > 0.0 && fv > 0.0 {
            mv.min(fv) / mv.max(fv)
        } else {
            0.0
        };
        let consistent = hint == Some(kind);
        let bonus = if consistent { 0.15 } else { 0.0 };
        let mut key = s * vq + bonus;
        if gate != Gate::Open && kind != Kind::Extrusion {
            key = key.min(FOREIGN_CAP);
        }
        let confidence = key.clamp(0.0, 1.0);
        if confidence < o.min_confidence {
            continue;
        }
        let c = Candidate {
            fit,
            confidence,
            mesh_volume: mv,
            fit_volume: fv,
            notes,
            consistent,
        };
        match best.iter().position(|x| x.1.kind() == c.kind()) {
            Some(i) if best[i].0 >= key => {}
            Some(i) => best[i] = (key, c),
            None => best.push((key, c)),
        }
    }
    best.sort_by(|p, q| q.0.total_cmp(&p.0));
    best.into_iter().map(|x| x.1).collect()
}

#[cfg(test)]
#[path = "mesh_fit_tests.rs"]
mod tests;
