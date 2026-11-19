//! Wall editing: the joint type at each end (butt / mitre / square), joint suppression,
//! walls composite along their height, profiles along a wall (a projection adds a body, a groove cuts
//! from the layers), the vertical structure of a type (layer spans by height, assigning a layer to a span,
//! layer wrapping at openings and end faces), rectangular openings on a wall facade.
//! All quantities are exact: areas use the polygon area formula, volumes are area × length.

use crate::math::{v2, V2};
use crate::profile::signed_area;

/// Joint type of two walls at a corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinKind {
    /// The main wall runs to the far face of the second and is cut along that face; the second butts against it.
    Butt,
    /// Both walls are cut on the bisector (along the line through the intersections of corresponding faces).
    Miter,
    /// The main wall is extended with a perpendicular end face to fully overlap; the second butts against it.
    SquareOff,
}

impl JoinKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JoinKind::Butt => "butt",
            JoinKind::Miter => "miter",
            JoinKind::SquareOff => "square",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "butt" => JoinKind::Butt,
            "miter" => JoinKind::Miter,
            "square" => JoinKind::SquareOff,
            _ => return None,
        })
    }
    /// Next type in the cycle, for clicking the joint icon.
    pub fn cycle(self) -> Self {
        match self {
            JoinKind::Butt => JoinKind::Miter,
            JoinKind::Miter => JoinKind::SquareOff,
            JoinKind::SquareOff => JoinKind::Butt,
        }
    }
}

/// Wall in plan for joint editing: axis a→b, faces are `left`/`right` of the axis (left normal).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub a: V2,
    pub b: V2,
    pub left: f64,
    pub right: f64,
}

impl Band {
    pub fn centered(a: V2, b: V2, t: f64) -> Self {
        Band {
            a,
            b,
            left: t / 2.0,
            right: t / 2.0,
        }
    }
    fn dir(&self) -> V2 {
        let d = self.b - self.a;
        d * (1.0 / d.len())
    }
    fn normal(&self) -> V2 {
        let d = self.dir();
        v2(-d.y, d.x)
    }
    /// Face lines (point, direction): [left, right].
    fn faces(&self) -> [(V2, V2); 2] {
        let n = self.normal();
        [
            (self.a + n * self.left, self.dir()),
            (self.a - n * self.right, self.dir()),
        ]
    }
    /// Rectangular footprint without joints.
    pub fn plain(&self) -> Vec<V2> {
        let n = self.normal();
        vec![
            self.a - n * self.right,
            self.b - n * self.right,
            self.b + n * self.left,
            self.a + n * self.left,
        ]
    }
}

/// Cutting line (point, direction).
type Line = (V2, V2);

fn meet(p: V2, d: V2, q: Line) -> Option<V2> {
    let den = d.cross(q.1);
    if den.abs() < 1e-12 {
        return None;
    }
    Some(p + d * ((q.0 - p).cross(q.1) / den))
}

fn dist(l: Line, p: V2) -> f64 {
    let d = l.1 * (1.0 / l.1.len());
    (p - l.0).cross(d).abs()
}

/// Wall footprint with ends trimmed by lines `cut_a`, `cut_b` (None means the end face is perpendicular to the axis).
pub fn trimmed(w: &Band, cut_a: Option<Line>, cut_b: Option<Line>) -> Vec<V2> {
    let n = w.normal();
    let [fl, fr] = w.faces();
    let end = |p: V2, cut: Option<Line>| -> (V2, V2) {
        let c = cut.unwrap_or((p, n));
        (
            meet(fr.0, fr.1, c).unwrap_or(p - n * w.right),
            meet(fl.0, fl.1, c).unwrap_or(p + n * w.left),
        )
    };
    let (ar, al) = end(w.a, cut_a);
    let (br, bl) = end(w.b, cut_b);
    vec![ar, br, bl, al]
}

/// The wall end that enters the corner: `true` means end `b`.
fn corner_end(w: &Band, o: &Band) -> bool {
    let c = |p: V2| (p - o.a).len().min((p - o.b).len());
    c(w.b) <= c(w.a)
}

fn far_end(w: &Band, at_b: bool) -> V2 {
    if at_b {
        w.a
    } else {
        w.b
    }
}

/// Faces of wall `o`, ordered [near, far] relative to the body of wall `w`.
fn faces_from(o: &Band, w: &Band, w_at_b: bool) -> [Line; 2] {
    let p = far_end(w, w_at_b);
    let [l, r] = o.faces();
    if dist(l, p) <= dist(r, p) {
        [l, r]
    } else {
        [r, l]
    }
}

/// Joint of two walls at a corner: footprints (main, second). `allowed = false` means no joint; end faces
/// are perpendicular to the axis at the axis endpoints.
pub fn corner(primary: &Band, secondary: &Band, kind: JoinKind, allowed: bool) -> [Vec<V2>; 2] {
    if !allowed {
        return [primary.plain(), secondary.plain()];
    }
    let pb = corner_end(primary, secondary);
    let sb = corner_end(secondary, primary);
    let put = |w: &Band, at_b: bool, cut: Line| {
        if at_b {
            trimmed(w, None, Some(cut))
        } else {
            trimmed(w, Some(cut), None)
        }
    };
    match kind {
        JoinKind::Miter => {
            // near face with near (inner corner), far with far (outer corner)
            let x = |f: Line, g: Line| meet(f.0, f.1, g);
            let p_near = faces_from(primary, secondary, sb);
            let s_near = faces_from(secondary, primary, pb);
            let inner = x(p_near[0], s_near[0]);
            let outer = x(p_near[1], s_near[1]);
            match (inner, outer) {
                (Some(i), Some(o)) => {
                    let cut = (i, o - i);
                    [put(primary, pb, cut), put(secondary, sb, cut)]
                }
                _ => [primary.plain(), secondary.plain()],
            }
        }
        JoinKind::Butt => {
            let s = faces_from(secondary, primary, pb);
            let p = faces_from(primary, secondary, sb);
            [put(primary, pb, s[1]), put(secondary, sb, p[0])]
        }
        JoinKind::SquareOff => {
            let s = faces_from(secondary, primary, pb);
            let p = faces_from(primary, secondary, sb);
            let butt = put(primary, pb, s[1]);
            // the main wall's end face is perpendicular to its axis through the farthest point of the butt cut
            let d = primary.dir();
            let origin = far_end(primary, pb);
            let sign = if pb { 1.0 } else { -1.0 };
            let reach = butt
                .iter()
                .map(|q| (*q - origin).dot(d) * sign)
                .fold(f64::MIN, f64::max);
            let tip = origin + d * (reach * sign);
            let n = primary.normal();
            [put(primary, pb, (tip, n)), put(secondary, sb, p[0])]
        }
    }
}

pub fn area(pts: &[V2]) -> f64 {
    signed_area(pts).abs()
}

/// Joint preferences at the wall ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EndPrefs {
    pub kind: JoinKind,
    pub allowed: bool,
}

impl Default for EndPrefs {
    fn default() -> Self {
        EndPrefs {
            kind: JoinKind::Butt,
            allowed: true,
        }
    }
}

/// Joint cleanup lines in plan by layer priority: a layer joins the other wall's layer of matching
/// priority; a weaker layer stops at the face of the more important one. Returns, for
/// each layer of the main wall, a "passes through the joint" flag (visible without a dividing line).
pub fn cleanup_runs(primary: &[u8], secondary: &[u8]) -> Vec<bool> {
    let best = secondary.iter().copied().min().unwrap_or(u8::MAX);
    primary.iter().map(|&p| p <= best).collect()
}

/// Cleanup by same-named layers (one function, one material): a layer passes through the joint
/// if the second wall has a layer of the same name and its priority is not higher (number not smaller);
/// without a match, compared against the most important layer of the second wall.
pub fn cleanup_runs_named(primary: &[(u8, &str)], secondary: &[(u8, &str)]) -> Vec<bool> {
    let best = secondary.iter().map(|l| l.0).min().unwrap_or(u8::MAX);
    primary
        .iter()
        .map(|&(p, name)| match secondary.iter().find(|l| l.1 == name) {
            Some(&(q, _)) => p <= q,
            None => p <= best,
        })
        .collect()
}

// ---------- walls composite along their height ----------

/// Part of a composite wall: type (thickness), height (None means variable, takes the remainder),
/// offset of the part's axis from the composite wall axis (left is +).
#[derive(Clone, Debug, PartialEq)]
pub struct StackPart {
    pub name: String,
    pub thickness: f64,
    pub height: Option<f64>,
    pub offset: f64,
}

/// Part level: bottom, top, offset, thickness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tier {
    pub z0: f64,
    pub z1: f64,
    pub offset: f64,
    pub thickness: f64,
}

/// Part heights from bottom to top. Exactly one part is variable; the remainder must be > 0.
pub fn stack_tiers(parts: &[StackPart], base: f64, total: f64) -> Result<Vec<Tier>, String> {
    let vars = parts.iter().filter(|p| p.height.is_none()).count();
    if parts.is_empty() || vars != 1 {
        return Err(format!("stack needs exactly one variable part, got {vars}"));
    }
    let fixed: f64 = parts.iter().filter_map(|p| p.height).sum();
    let rest = total - fixed;
    if rest <= 0.0 || parts.iter().any(|p| p.height.is_some_and(|h| h <= 0.0)) {
        return Err(format!("fixed parts {fixed} exceed wall height {total}"));
    }
    let mut z = base;
    Ok(parts
        .iter()
        .map(|p| {
            let h = p.height.unwrap_or(rest);
            let t = Tier {
                z0: z,
                z1: z + h,
                offset: p.offset,
                thickness: p.thickness,
            };
            z += h;
            t
        })
        .collect())
}

pub fn stack_volume(tiers: &[Tier], length: f64) -> f64 {
    tiers
        .iter()
        .map(|t| (t.z1 - t.z0) * t.thickness * length)
        .sum()
}

// ---------- profiles along a wall ----------

/// Clipping a polygon by the half-plane `n·p ≥ c` (Sutherland–Hodgman).
fn clip(poly: &[V2], n: V2, c: f64) -> Vec<V2> {
    let mut out = Vec::new();
    let k = poly.len();
    for i in 0..k {
        let (p, q) = (poly[i], poly[(i + 1) % k]);
        let (dp, dq) = (n.dot(p) - c, n.dot(q) - c);
        if dp >= 0.0 {
            out.push(p);
        }
        if (dp >= 0.0) != (dq >= 0.0) {
            out.push(p + (q - p) * (dp / (dp - dq)));
        }
    }
    out
}

/// Area of the profile part in the depth strip `u0 ≤ u ≤ u1` (u is into the wall from the face).
pub fn band_area(profile: &[V2], u0: f64, u1: f64) -> f64 {
    let a = clip(profile, v2(1.0, 0.0), u0);
    let b = clip(&a, v2(-1.0, 0.0), -u1);
    if b.len() < 3 {
        0.0
    } else {
        area(&b)
    }
}

/// Profile along a wall: (u, z) with u > 0 into the wall (groove), u < 0 outward (projection).
#[derive(Clone, Debug, PartialEq)]
pub struct Sweep {
    pub profile: Vec<V2>,
    /// Span along the wall [s0, s1].
    pub s0: f64,
    pub s1: f64,
    /// Groove (cut) or projection (body).
    pub reveal: bool,
    /// Face: true means left (outer).
    pub left: bool,
}

/// Rectangular profile: depth × height, bottom at `z`.
pub fn rect_profile(depth: f64, height: f64, z: f64, reveal: bool) -> Vec<V2> {
    let (u0, u1) = if reveal { (0.0, depth) } else { (-depth, 0.0) };
    vec![v2(u0, z), v2(u1, z), v2(u1, z + height), v2(u0, z + height)]
}

/// Facade opening for profiles: [s0, s1] × [z0, z1].
pub type FacadeHole = (f64, f64, f64, f64);

/// Area of the polygon part in the height strip `z0 ≤ z ≤ z1`.
fn z_band(profile: &[V2], z0: f64, z1: f64) -> Vec<V2> {
    let a = clip(profile, v2(0.0, 1.0), z0);
    if a.len() < 3 {
        return vec![];
    }
    clip(&a, v2(0.0, -1.0), -z1)
}

/// Polygon centroid in u (for the Pappus correction on an arc).
pub fn centroid_u(pts: &[V2]) -> f64 {
    let (mut a, mut cx) = (0.0, 0.0);
    for i in 0..pts.len() {
        let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
        let c = p.cross(q);
        a += c;
        cx += (p.x + q.x) * c;
    }
    if a.abs() < 1e-18 {
        0.0
    } else {
        cx / (3.0 * a)
    }
}

/// Profile on a specific wall: the span is clipped to length [0, len], the section to the top `h`,
/// and openings crossing the profile in length and height are subtracted. `arc_k(u)` is the ratio of the path
/// length at depth u to the axis length (1 for a straight wall; on an arc R(u)/R, Pappus's theorem).
pub struct SweepOn<'a> {
    pub sweep: &'a Sweep,
    pub len: f64,
    pub h: f64,
    pub holes: &'a [FacadeHole],
}

impl SweepOn<'_> {
    pub fn run(&self) -> (f64, f64) {
        (self.sweep.s0.max(0.0), self.sweep.s1.min(self.len))
    }
    /// Profile section under the wall top.
    pub fn section(&self) -> Vec<V2> {
        z_band(&self.sweep.profile, f64::MIN / 4.0, self.h)
    }
    /// Integral of the area of section part `part` along the span, minus openings.
    fn integral(&self, part: &[V2], arc_k: &dyn Fn(f64) -> f64) -> f64 {
        if part.len() < 3 {
            return 0.0;
        }
        let (a, b) = self.run();
        let k = arc_k(centroid_u(part));
        let mut v = area(part) * (b - a).max(0.0);
        for &(s0, s1, z0, z1) in self.holes {
            let l = (s1.min(b) - s0.max(a)).max(0.0);
            if l > 0.0 {
                let cut = z_band(part, z0, z1);
                if cut.len() >= 3 {
                    v -= area(&cut) * l;
                }
            }
        }
        v.max(0.0) * k
    }
    /// Projection volume (the part of the profile outside the face).
    pub fn added_volume(&self, arc_k: &dyn Fn(f64) -> f64) -> f64 {
        if self.sweep.reveal {
            return 0.0;
        }
        let out = clip(&self.section(), v2(-1.0, 0.0), 0.0);
        self.integral(&out, arc_k)
    }
    /// Groove cuts per layer (thicknesses measured from this face inward).
    pub fn cut_per_layer(&self, layers_from_face: &[f64], arc_k: &dyn Fn(f64) -> f64) -> Vec<f64> {
        let sec = self.section();
        let mut u = 0.0;
        layers_from_face
            .iter()
            .map(|t| {
                let v = if self.sweep.reveal && sec.len() >= 3 {
                    let a = clip(&sec, v2(1.0, 0.0), u);
                    let b = if a.len() >= 3 {
                        clip(&a, v2(-1.0, 0.0), -(u + t))
                    } else {
                        vec![]
                    };
                    self.integral(&b, arc_k)
                } else {
                    0.0
                };
                u += t;
                v
            })
            .collect()
    }
}

impl Sweep {
    pub fn length(&self) -> f64 {
        (self.s1 - self.s0).max(0.0)
    }
    /// Projection volume (the part of the profile outside the face).
    pub fn added_volume(&self) -> f64 {
        if self.reveal {
            0.0
        } else {
            band_area(&self.profile, f64::MIN / 4.0, 0.0) * self.length()
        }
    }
    /// Volumes cut by the groove from each layer (layers from this face inward).
    pub fn cut_per_layer(&self, layers_from_face: &[f64]) -> Vec<f64> {
        let mut u = 0.0;
        layers_from_face
            .iter()
            .map(|t| {
                let v = if self.reveal {
                    band_area(&self.profile, u, u + t) * self.length()
                } else {
                    0.0
                };
                u += t;
                v
            })
            .collect()
    }
}

// ---------- vertical structure ----------

/// Layer span by height: layer `layer` (column), [z0, z1] from the wall bottom, material is
/// the source layer index (after "assign layer" a span takes the material of another layer).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub layer: usize,
    pub z0: f64,
    pub z1: f64,
    pub material: usize,
}

/// Layer wrapping at openings and end faces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Wrap {
    #[default]
    None,
    Exterior,
    Interior,
    Both,
}

impl Wrap {
    pub fn as_str(self) -> &'static str {
        match self {
            Wrap::None => "none",
            Wrap::Exterior => "ext",
            Wrap::Interior => "int",
            Wrap::Both => "both",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "ext" => Wrap::Exterior,
            "int" => Wrap::Interior,
            "both" => Wrap::Both,
            _ => Wrap::None,
        }
    }
}

/// Vertical structure of a wall type. Layers run from outside (0) inward; `core` is the range
/// of load-bearing layers [c0, c1); `wraps[i]` means layer i wraps.
#[derive(Clone, Debug, PartialEq)]
pub struct Structure {
    pub thickness: Vec<f64>,
    pub core: (usize, usize),
    pub height: f64,
    pub regions: Vec<Region>,
    pub wraps: Vec<bool>,
    pub at_inserts: Wrap,
    pub at_ends: Wrap,
}

impl Structure {
    pub fn new(thickness: Vec<f64>, core: (usize, usize), height: f64) -> Self {
        let n = thickness.len();
        Structure {
            regions: (0..n)
                .map(|i| Region {
                    layer: i,
                    z0: 0.0,
                    z1: height,
                    material: i,
                })
                .collect(),
            wraps: vec![false; n],
            thickness,
            core,
            height,
            at_inserts: Wrap::None,
            at_ends: Wrap::None,
        }
    }

    fn find(&self, layer: usize, z: f64) -> Option<usize> {
        self.regions
            .iter()
            .position(|r| r.layer == layer && r.z0 < z && z < r.z1)
    }

    /// Split a layer span with a horizontal line at height z. Returns the index of the upper one.
    pub fn split(&mut self, layer: usize, z: f64) -> Result<usize, String> {
        let k = self
            .find(layer, z)
            .ok_or_else(|| format!("no region of layer {layer} strictly contains z={z}"))?;
        let mut up = self.regions[k];
        self.regions[k].z1 = z;
        up.z0 = z;
        self.regions.push(up);
        Ok(self.regions.len() - 1)
    }

    /// Assign the material of layer `material` to a span.
    pub fn assign(&mut self, region: usize, material: usize) -> Result<(), String> {
        if material >= self.thickness.len() {
            return Err(format!("no layer {material}"));
        }
        let r = self
            .regions
            .get_mut(region)
            .ok_or_else(|| format!("no region {region}"))?;
        r.material = material;
        Ok(())
    }

    /// Remove a split: the span merges with its lower neighbour of the same layer and material.
    pub fn merge_down(&mut self, region: usize) -> Result<(), String> {
        let r = *self.regions.get(region).ok_or("no region")?;
        let k = self
            .regions
            .iter()
            .position(|o| {
                o.layer == r.layer && (o.z1 - r.z0).abs() < 1e-9 && o.material == r.material
            })
            .ok_or("no matching region below")?;
        self.regions[k].z1 = r.z1;
        self.regions.remove(region);
        Ok(())
    }

    /// Volume per material over wall length `length`, without openings.
    pub fn volumes(&self, length: f64) -> Vec<f64> {
        let mut v = vec![0.0; self.thickness.len()];
        for r in &self.regions {
            if r.layer < v.len() && r.material < v.len() {
                v[r.material] += self.thickness[r.layer] * (r.z1 - r.z0).max(0.0) * length;
            }
        }
        v
    }

    /// Structure on a wall of height `h`: spans are clipped at the top, empty ones removed, and the top
    /// span of each layer is extended to `h` (wall taller than the type). Volumes are never negative.
    pub fn fit(&self, h: f64) -> Structure {
        let n = self.thickness.len();
        let mut out = self.clone();
        out.height = h;
        out.regions = self
            .regions
            .iter()
            .filter(|r| r.layer < n && r.material < n)
            .map(|r| Region {
                z0: r.z0.clamp(0.0, h),
                z1: r.z1.clamp(0.0, h),
                ..*r
            })
            .filter(|r| r.z1 - r.z0 > 1e-9)
            .collect();
        for l in 0..n {
            let top = out
                .regions
                .iter()
                .enumerate()
                .filter(|(_, r)| r.layer == l)
                .max_by(|a, b| a.1.z1.total_cmp(&b.1.z1))
                .map(|(k, _)| k);
            match top {
                Some(k) => out.regions[k].z1 = h,
                None => out.regions.push(Region {
                    layer: l,
                    z0: 0.0,
                    z1: h,
                    material: l,
                }),
            }
        }
        out
    }

    /// Height shares per material for each layer: `[layer] -> [(material, share)]`.
    pub fn shares(&self) -> Vec<Vec<(usize, f64)>> {
        let n = self.thickness.len();
        let mut out = vec![vec![]; n];
        if self.height <= 0.0 {
            return out;
        }
        for r in &self.regions {
            if r.layer < n && r.material < n {
                out[r.layer].push((r.material, (r.z1 - r.z0).max(0.0) / self.height));
            }
        }
        out
    }

    /// Layer volume change from wrapping at an opening with reveal perimeter `perimeter`
    /// (the layer wraps to the core face, displacing the layers between it and the core).
    pub fn wrap_delta(&self, wrap: Wrap, perimeter: f64) -> Vec<f64> {
        let n = self.thickness.len();
        let mut d = vec![0.0; n];
        let mut side = |range: Vec<usize>| {
            for (k, &i) in range.iter().enumerate() {
                if !self.wraps[i] {
                    continue;
                }
                // layer i runs along the reveal through all layers between it and the core
                for &j in &range[k + 1..] {
                    let dv = self.thickness[i] * self.thickness[j] * perimeter;
                    d[i] += dv;
                    d[j] -= dv;
                }
            }
        };
        if matches!(wrap, Wrap::Exterior | Wrap::Both) {
            side((0..self.core.0).collect());
        }
        if matches!(wrap, Wrap::Interior | Wrap::Both) {
            side((self.core.1..n).rev().collect());
        }
        d
    }
}

// ---------- facade openings ----------

/// Rectangular opening defined on a wall facade by two corners (s along, z height).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WallOpening {
    pub s0: f64,
    pub s1: f64,
    pub z0: f64,
    pub z1: f64,
}

impl WallOpening {
    /// From two arbitrary corners, clipped to the wall outline [0, len] × [0, h].
    pub fn from_corners(p: V2, q: V2, len: f64, h: f64) -> Option<Self> {
        let o = WallOpening {
            s0: p.x.min(q.x).max(0.0),
            s1: p.x.max(q.x).min(len),
            z0: p.y.min(q.y).max(0.0),
            z1: p.y.max(q.y).min(h),
        };
        (o.s1 - o.s0 > 1e-9 && o.z1 - o.z0 > 1e-9).then_some(o)
    }
    pub fn area(&self) -> f64 {
        (self.s1 - self.s0) * (self.z1 - self.z0)
    }
    /// Reveal perimeter (without the bottom if the opening reaches the floor).
    pub fn jamb_perimeter(&self) -> f64 {
        let w = self.s1 - self.s0;
        let h = self.z1 - self.z0;
        2.0 * h + w + if self.z0 > 1e-9 { w } else { 0.0 }
    }
}

/// Opening "by face": the rectangle of a face of another element (for example a slab or beam)
/// crossing the wall is projected onto the wall facade along its axis a→b.
/// `band` is the wall faces along the normal (min, max) from the axis, `h` is the wall height: a face that does not
/// cross the wall body yields no opening; the opening is clipped at the top.
pub fn opening_by_face(
    a: V2,
    b: V2,
    band: (f64, f64),
    h: f64,
    face: &[(V2, f64)],
) -> Option<WallOpening> {
    let d = b - a;
    let len = d.len();
    if len <= 1e-12 || face.is_empty() {
        return None;
    }
    let t = d * (1.0 / len);
    let n = v2(-t.y, t.x);
    let (mut s0, mut s1, mut z0, mut z1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    let (mut o0, mut o1) = (f64::MAX, f64::MIN);
    for (p, z) in face {
        let s = (*p - a).dot(t);
        let o = (*p - a).dot(n);
        s0 = s0.min(s);
        s1 = s1.max(s);
        z0 = z0.min(*z);
        z1 = z1.max(*z);
        o0 = o0.min(o);
        o1 = o1.max(o);
    }
    // the face must lie within the wall thickness or cross it along the normal
    if o1 < band.0 - 1e-9 || o0 > band.1 + 1e-9 || s1 < 0.0 || s0 > len || z0 >= h {
        return None;
    }
    WallOpening::from_corners(v2(s0, z0), v2(s1, z1), len, h)
}

/// Intersection of opening rectangles on the facade (a window above a door is not an intersection).
pub fn overlaps_2d(a: FacadeHole, b: FacadeHole) -> bool {
    let e = 1e-9;
    a.0 < b.1 - e && b.0 < a.1 - e && a.2 < b.3 - e && b.2 < a.3 - e
}

/// Temporary dimensions of a door/window (`along` is the opening start from point a) on the facade: to the wall ends, bottom (sill), top (lintel).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InsertDims {
    pub to_start: f64,
    pub to_end: f64,
    pub sill: f64,
    pub head: f64,
}

pub fn insert_dims(along: f64, width: f64, sill: f64, height: f64, wall_len: f64) -> InsertDims {
    InsertDims {
        to_start: along,
        to_end: wall_len - along - width,
        sill,
        head: sill + height,
    }
}

/// Editing a temporary dimension: returns (along, sill, height) after the value is entered.
pub fn apply_dim(
    which: &str,
    value: f64,
    along: f64,
    width: f64,
    sill: f64,
    height: f64,
    wall_len: f64,
) -> Result<(f64, f64, f64), String> {
    let r = match which {
        "start" => (value, sill, height),
        "end" => (wall_len - value - width, sill, height),
        "sill" => (along, value, height),
        // the top moves, the bottom stays
        "head" => (along, sill, value - sill),
        _ => return Err(format!("unknown dim {which}")),
    };
    if r.0 < -1e-9 || r.0 + width > wall_len + 1e-9 || r.1 < 0.0 || r.2 <= 0.0 {
        return Err("insert leaves the wall".into());
    }
    Ok(r)
}

/// `apply_dim` with a check of the opening top against the wall height `wall_h`.
#[allow(clippy::too_many_arguments)]
pub fn apply_dim_h(
    which: &str,
    value: f64,
    along: f64,
    width: f64,
    sill: f64,
    height: f64,
    wall_len: f64,
    wall_h: f64,
) -> Result<(f64, f64, f64), String> {
    let r = apply_dim(which, value, along, width, sill, height, wall_len)?;
    if r.1 + r.2 > wall_h + 1e-9 {
        return Err("insert head above the wall top".into());
    }
    Ok(r)
}

/// Door/window flip: swing side (outward/inward) and hinges (left/right).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Flip {
    pub facing: bool,
    pub hand: bool,
}

impl Flip {
    pub fn flip_facing(self) -> Self {
        Flip {
            facing: !self.facing,
            ..self
        }
    }
    pub fn flip_hand(self) -> Self {
        Flip {
            hand: !self.hand,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const E: f64 = 1e-9;

    fn l_walls(t: f64) -> (Band, Band) {
        // corner at (0,0): wall 1 along X (from 5 to 0), wall 2 along Y (from 0 to 4)
        (
            Band::centered(v2(5.0, 0.0), v2(0.0, 0.0), t),
            Band::centered(v2(0.0, 0.0), v2(0.0, 4.0), t),
        )
    }

    #[test]
    fn miter_join_volumes_exact() {
        let (t, h) = (0.3, 3.0);
        let (p, s) = l_walls(t);
        let [a, b] = corner(&p, &s, JoinKind::Miter, true);
        assert!((area(&a) * h - 5.0 * t * h).abs() < E, "{}", area(&a));
        assert!((area(&b) * h - 4.0 * t * h).abs() < E);
        // no gap and no overlap: the sum equals the L-shape area
        let l_shape = (5.0 + t / 2.0) * t + (4.0 - t / 2.0) * t;
        assert!((area(&a) + area(&b) - l_shape).abs() < E);
    }

    #[test]
    fn butt_and_square_and_disallow() {
        let (t, h) = (0.3, 3.0);
        let (p, s) = l_walls(t);
        let [a, b] = corner(&p, &s, JoinKind::Butt, true);
        assert!((area(&a) - (5.0 + t / 2.0) * t).abs() < E);
        assert!((area(&b) - (4.0 - t / 2.0) * t).abs() < E);
        let [a2, b2] = corner(&p, &s, JoinKind::SquareOff, true);
        assert!((area(&a2) - area(&a)).abs() < E && (area(&b2) - area(&b)).abs() < E);
        let [a3, b3] = corner(&p, &s, JoinKind::Butt, false);
        assert!((area(&a3) * h - 5.0 * t * h).abs() < E);
        assert!((area(&b3) - 4.0 * t).abs() < E);
    }

    #[test]
    fn oblique_miter_and_square_off() {
        // walls at 60°: with a mitre both areas = axis length × thickness (the cut is symmetric)
        let t = 0.2;
        let p = Band::centered(v2(4.0, 0.0), v2(0.0, 0.0), t);
        let c = 60f64.to_radians();
        let s = Band::centered(v2(0.0, 0.0), v2(3.0 * c.cos(), 3.0 * c.sin()), t);
        let [a, b] = corner(&p, &s, JoinKind::Miter, true);
        assert!((area(&a) - 4.0 * t).abs() < E, "{}", area(&a));
        assert!((area(&b) - 3.0 * t).abs() < E);
        // with a square end face the main wall is longer than with a butt joint
        let [sq, _] = corner(&p, &s, JoinKind::SquareOff, true);
        let [bt, _] = corner(&p, &s, JoinKind::Butt, true);
        assert!(area(&sq) > area(&bt));
    }

    #[test]
    fn cleanup_by_priority() {
        assert_eq!(cleanup_runs(&[3, 1, 5], &[1, 2]), vec![false, true, false]);
    }

    #[test]
    fn stacked_wall_heights_exact() {
        let parts = vec![
            StackPart {
                name: "plinth".into(),
                thickness: 0.4,
                height: Some(0.9),
                offset: 0.05,
            },
            StackPart {
                name: "main".into(),
                thickness: 0.3,
                height: None,
                offset: 0.0,
            },
            StackPart {
                name: "parapet".into(),
                thickness: 0.2,
                height: Some(0.6),
                offset: -0.05,
            },
        ];
        let t = stack_tiers(&parts, 1.0, 6.0).unwrap();
        assert_eq!(
            t.iter().map(|x| (x.z0, x.z1)).collect::<Vec<_>>(),
            vec![(1.0, 1.9), (1.9, 6.4), (6.4, 7.0)]
        );
        let v = stack_volume(&t, 10.0);
        assert!((v - (0.9 * 0.4 + 4.5 * 0.3 + 0.6 * 0.2) * 10.0).abs() < E);
        assert!(stack_tiers(&parts, 0.0, 1.4).is_err());
        assert!(stack_tiers(&parts[..1], 0.0, 3.0).is_err());
    }

    #[test]
    fn sweep_and_reveal_volumes_exact() {
        let cornice = Sweep {
            profile: vec![v2(0.0, 2.8), v2(-0.15, 2.8), v2(-0.15, 3.0), v2(0.0, 3.0)],
            s0: 0.0,
            s1: 8.0,
            reveal: false,
            left: true,
        };
        assert!((cornice.added_volume() - 0.15 * 0.2 * 8.0).abs() < E);
        // triangular projection: area ½·0.1·0.2
        let tri = Sweep {
            profile: vec![v2(0.0, 1.0), v2(-0.1, 1.0), v2(0.0, 1.2)],
            ..cornice.clone()
        };
        assert!((tri.added_volume() - 0.5 * 0.1 * 0.2 * 8.0).abs() < E);
        // groove 0.05 deep through a 0.02 finish into the core
        let r = Sweep {
            profile: rect_profile(0.05, 0.03, 1.5, true),
            s0: 1.0,
            s1: 5.0,
            reveal: true,
            left: true,
        };
        let cut = r.cut_per_layer(&[0.02, 0.2, 0.01]);
        assert!((cut[0] - 0.02 * 0.03 * 4.0).abs() < E);
        assert!((cut[1] - 0.03 * 0.03 * 4.0).abs() < E);
        assert_eq!(cut[2], 0.0);
        assert_eq!(r.added_volume(), 0.0);
    }

    #[test]
    fn vertical_structure_split_assign_merge() {
        let mut s = Structure::new(vec![0.02, 0.2, 0.01], (1, 2), 3.0);
        let up = s.split(0, 1.0).unwrap();
        s.assign(up, 2).unwrap();
        let v = s.volumes(10.0);
        assert!((v[0] - 0.02 * 1.0 * 10.0).abs() < E);
        assert!((v[2] - (0.02 * 2.0 + 0.01 * 3.0) * 10.0).abs() < E);
        assert!(s.split(0, 3.0).is_err());
        s.assign(up, 0).unwrap();
        s.merge_down(up).unwrap();
        assert_eq!(s.regions.len(), 3);
        assert!((s.volumes(1.0)[0] - 0.06).abs() < E);
    }

    #[test]
    fn wrap_at_inserts_conserves_volume() {
        let mut s = Structure::new(vec![0.02, 0.05, 0.2, 0.01], (2, 3), 3.0);
        s.wraps[0] = true;
        let o = WallOpening::from_corners(v2(3.0, 2.0), v2(1.0, 0.9), 6.0, 3.0).unwrap();
        assert!((o.jamb_perimeter() - (2.0 * 1.1 + 2.0 * 2.0)).abs() < E);
        let d = s.wrap_delta(Wrap::Exterior, o.jamb_perimeter());
        assert!((d[0] - 0.02 * 0.05 * 6.2).abs() < E);
        assert!((d.iter().sum::<f64>()).abs() < E);
        assert_eq!(s.wrap_delta(Wrap::Interior, 6.2), vec![0.0; 4]);
    }

    #[test]
    fn openings_and_insert_dims() {
        let o = WallOpening::from_corners(v2(-1.0, 4.0), v2(2.0, 1.0), 5.0, 3.0).unwrap();
        assert_eq!((o.s0, o.s1, o.z0, o.z1), (0.0, 2.0, 1.0, 3.0));
        let face = [(v2(2.0, -1.0), 2.7), (v2(4.0, 1.0), 3.2)];
        let f = opening_by_face(v2(0.0, 0.0), v2(10.0, 0.0), (-0.1, 0.1), 3.0, &face).unwrap();
        assert!((f.area() - 2.0 * 0.3).abs() < E, "clipped by the wall top");
        let away = [(v2(2.0, 0.5), 1.0), (v2(4.0, 0.9), 2.0)];
        assert!(opening_by_face(v2(0.0, 0.0), v2(10.0, 0.0), (-0.1, 0.1), 3.0, &away).is_none());
        assert!(overlaps_2d((0.0, 1.0, 0.0, 2.1), (0.5, 1.5, 1.0, 2.0)));
        assert!(
            !overlaps_2d((0.0, 1.0, 0.0, 2.1), (0.0, 1.0, 2.2, 2.8)),
            "window above door"
        );
        assert!(apply_dim_h("head", 3.2, 2.0, 0.9, 0.0, 2.1, 6.0, 3.0).is_err());
        let d = insert_dims(2.0, 0.9, 0.0, 2.1, 6.0);
        assert!((d.to_start - 2.0).abs() < E && (d.to_end - 3.1).abs() < E && d.head == 2.1);
        let (al, si, he) = apply_dim("head", 2.4, 2.0, 0.9, 0.0, 2.1, 6.0).unwrap();
        assert_eq!((al, si), (2.0, 0.0));
        assert!((he - 2.4).abs() < E);
        assert!(apply_dim("start", 5.5, 2.0, 0.9, 0.0, 2.1, 6.0).is_err());
        let fl = Flip::default().flip_hand().flip_facing().flip_hand();
        assert_eq!(
            fl,
            Flip {
                facing: true,
                hand: false
            }
        );
        assert_eq!(JoinKind::Butt.cycle().cycle().cycle(), JoinKind::Butt);
    }

    #[test]
    fn sweep_on_wall_clips_and_subtracts() {
        let sw = Sweep {
            profile: rect_profile(0.1, 0.2, 2.9, false),
            s0: -1.0,
            s1: 7.0,
            reveal: false,
            left: true,
        };
        let one = |_: f64| 1.0;
        let on = SweepOn {
            sweep: &sw,
            len: 6.0,
            h: 3.0,
            holes: &[],
        };
        assert!(
            (on.added_volume(&one) - 0.1 * 0.1 * 6.0).abs() < E,
            "length and top clip"
        );
        let holes = [(1.0, 2.0, 0.0, 2.95)];
        let on = SweepOn {
            sweep: &sw,
            len: 6.0,
            h: 3.0,
            holes: &holes,
        };
        let v = 0.1 * 0.1 * 6.0 - 0.1 * 0.05 * 1.0;
        assert!((on.added_volume(&one) - v).abs() < E, "opening subtracted");
        let arc = |u: f64| (5.0 - u) / 5.0;
        assert!(
            (on.added_volume(&arc) - v * (5.0 + 0.05) / 5.0).abs() < E,
            "Pappus"
        );
        let rv = Sweep {
            profile: rect_profile(0.03, 0.02, 1.0, true),
            reveal: true,
            ..sw
        };
        let on = SweepOn {
            sweep: &rv,
            len: 6.0,
            h: 3.0,
            holes: &[],
        };
        let c = on.cut_per_layer(&[0.02, 0.2], &one);
        assert!((c[0] - 0.02 * 0.02 * 6.0).abs() < E && (c[1] - 0.01 * 0.02 * 6.0).abs() < E);
    }

    #[test]
    fn structure_fits_wall_height() {
        let mut s = Structure::new(vec![0.02, 0.2], (1, 2), 3.0);
        let up = s.split(0, 2.0).unwrap();
        s.assign(up, 1).unwrap();
        let low = s.fit(1.5);
        assert!(low.regions.iter().all(|r| r.z1 <= 1.5 && r.z1 > r.z0));
        let v = low.volumes(1.0);
        assert!((v[0] - 0.02 * 1.5).abs() < E && (v[1] - 0.2 * 1.5).abs() < E);
        let high = s.fit(4.0);
        let v = high.volumes(1.0);
        assert!((v[0] - 0.02 * 2.0).abs() < E && (v[1] - (0.2 * 4.0 + 0.02 * 2.0)).abs() < E);
        assert!(v.iter().all(|x| *x >= 0.0));
    }

    #[test]
    fn cleanup_by_same_function() {
        let p = [(5u8, "plaster"), (1, "brick")];
        let q = [(1u8, "brick"), (3, "plaster")];
        assert_eq!(cleanup_runs_named(&p, &q), vec![false, true]);
        assert_eq!(cleanup_runs_named(&[(2, "x")], &q), vec![false]);
    }
}
