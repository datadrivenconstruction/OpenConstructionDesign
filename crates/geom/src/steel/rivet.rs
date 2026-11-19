//! Rivets and instanced fasteners. A riveted joint keeps only its layout parameters; the
//! individual rivets are compact instances (20 bytes) of a few prototype meshes per size and
//! level of detail, so a tower with millions of rivets stays in tens of megabytes.

use crate::math::{v3, V3};
use crate::mesh::Mesh;
use std::f64::consts::PI;
use std::sync::OnceLock;

const RIVETS: &str = include_str!("../../data/steel/rivets.csv");

#[derive(Clone, Debug, PartialEq)]
pub struct RivetSize {
    /// Shank, hole, head diameter and head height, m.
    pub d: f64,
    pub d0: f64,
    pub head_d: f64,
    pub head_k: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RivetRules {
    pub p_min: f64,
    pub e_min: f64,
    pub e2_min: f64,
    pub p_max_t: f64,
    pub p_max_abs: f64,
    pub stitch_t: f64,
    pub grip_max_d: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RivetCatalog {
    pub sizes: Vec<RivetSize>,
    pub rules: RivetRules,
}

pub fn parse(text: &str) -> Result<RivetCatalog, String> {
    let mut sizes = vec![];
    let mut kv = std::collections::HashMap::new();
    let mut in_rules = false;
    for (n, line) in text.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with("d,") {
            continue;
        }
        if l == "[rules]" {
            in_rules = true;
            continue;
        }
        let c: Vec<&str> = l.split(',').map(str::trim).collect();
        let num = |s: &str| s.parse::<f64>().map_err(|_| format!("rivets.csv:{}: bad number '{s}'", n + 1));
        if in_rules {
            if c.len() != 2 {
                return Err(format!("rivets.csv:{}: key,value expected", n + 1));
            }
            kv.insert(c[0].to_string(), num(c[1])?);
        } else {
            if c.len() != 4 {
                return Err(format!("rivets.csv:{}: 4 fields expected", n + 1));
            }
            let v: Vec<f64> = c.iter().map(|s| num(s)).collect::<Result<_, _>>()?;
            sizes.push(RivetSize { d: v[0] / 1e3, d0: v[1] / 1e3, head_d: v[2] / 1e3, head_k: v[3] / 1e3 });
        }
    }
    let g = |k: &str| kv.get(k).copied().ok_or(format!("rivets.csv: rule '{k}' missing"));
    let rules = RivetRules {
        p_min: g("p_min")?,
        e_min: g("e_min")?,
        e2_min: g("e2_min")?,
        p_max_t: g("p_max_t")?,
        p_max_abs: g("p_max_abs")? / 1e3,
        stitch_t: g("stitch_t")?,
        grip_max_d: g("grip_max_d")?,
    };
    Ok(RivetCatalog { sizes, rules })
}

pub fn catalog() -> &'static RivetCatalog {
    static C: OnceLock<RivetCatalog> = OnceLock::new();
    C.get_or_init(|| parse(RIVETS).expect("built-in rivet table"))
}

impl RivetCatalog {
    pub fn size(&self, d: f64) -> Option<(usize, &RivetSize)> {
        self.sizes.iter().enumerate().find(|(_, s)| (s.d - d).abs() < 1e-6)
    }
    /// Common sizing rule d = sqrt(50 t) - 2 (mm), rounded up to the series (t, d in m).
    pub fn for_ply(&self, t: f64) -> &RivetSize {
        let want = ((50.0 * t * 1e3).sqrt() - 2.0) / 1e3;
        self.sizes.iter().find(|s| s.d >= want - 1e-9).unwrap_or(self.sizes.last().unwrap())
    }
}

/// A rectangular rivet pattern on a plate: `lines` across the force, `per_line` along it.
#[derive(Clone, Debug, PartialEq)]
pub struct RivetGroup {
    pub d: f64,
    pub lines: usize,
    pub per_line: usize,
    /// Pitch along the force, gauge between lines, end and edge distance, m.
    pub pitch: f64,
    pub gauge: f64,
    pub end: f64,
    pub edge: f64,
    /// Thinnest outer ply and total grip, m.
    pub t_min: f64,
    pub grip: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RivetRule {
    Series,
    PitchMin,
    GaugeMin,
    PitchMax,
    EndMin,
    EdgeMin,
    Grip,
}

impl RivetRule {
    pub fn key(self) -> &'static str {
        match self {
            RivetRule::Series => "rivet_series",
            RivetRule::PitchMin => "pitch_min",
            RivetRule::GaugeMin => "gauge_min",
            RivetRule::PitchMax => "pitch_max",
            RivetRule::EndMin => "end_min",
            RivetRule::EdgeMin => "edge_min",
            RivetRule::Grip => "grip",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RivetFinding {
    pub rule: RivetRule,
    pub value: f64,
    pub limit: f64,
}

impl RivetGroup {
    /// Preferred layout for `n` rivets in `lines` lines, sized from the plies.
    pub fn layout(n: usize, lines: usize, t_min: f64, grip: f64) -> RivetGroup {
        let c = catalog();
        let s = c.for_ply(t_min);
        let lines = lines.max(1);
        let per_line = n.div_ceil(lines).max(1);
        let r = |x: f64| (x * 200.0).ceil() / 200.0; // 5 mm
        RivetGroup {
            d: s.d,
            lines,
            per_line,
            pitch: r(c.rules.p_min * s.d0 + 0.005),
            gauge: r(c.rules.p_min * s.d0 + 0.005),
            end: r(c.rules.e_min * s.d0 + 0.005),
            edge: r(c.rules.e2_min * s.d0 + 0.010),
            t_min,
            grip,
        }
    }

    pub fn count(&self) -> usize {
        self.lines * self.per_line
    }

    /// Plate patch the group needs: (along, across), m.
    pub fn footprint(&self) -> (f64, f64) {
        (
            2.0 * self.end + self.pitch * (self.per_line.saturating_sub(1)) as f64,
            2.0 * self.edge + self.gauge * (self.lines.saturating_sub(1)) as f64,
        )
    }

    /// Rule check; empty when the group is valid.
    pub fn check(&self) -> Vec<RivetFinding> {
        let c = catalog();
        let mut out = vec![];
        let Some((_, s)) = c.size(self.d) else {
            out.push(RivetFinding { rule: RivetRule::Series, value: self.d, limit: 0.0 });
            return out;
        };
        let ru = &c.rules;
        let mut need = |rule, ok: bool, value, limit| {
            if !ok {
                out.push(RivetFinding { rule, value, limit });
            }
        };
        let eps = 1e-9;
        if self.per_line > 1 {
            need(RivetRule::PitchMin, self.pitch + eps >= ru.p_min * s.d0, self.pitch, ru.p_min * s.d0);
            let pmax = (ru.p_max_t * self.t_min).min(ru.p_max_abs);
            need(RivetRule::PitchMax, self.pitch <= pmax + eps, self.pitch, pmax);
        }
        if self.lines > 1 {
            need(RivetRule::GaugeMin, self.gauge + eps >= ru.p_min * s.d0, self.gauge, ru.p_min * s.d0);
        }
        need(RivetRule::EndMin, self.end + eps >= ru.e_min * s.d0, self.end, ru.e_min * s.d0);
        need(RivetRule::EdgeMin, self.edge + eps >= ru.e2_min * s.d0, self.edge, ru.e2_min * s.d0);
        need(RivetRule::Grip, self.grip <= ru.grip_max_d * self.d + eps, self.grip, ru.grip_max_d * self.d);
        out
    }

    /// Rivet centres in the plate frame (along, across), from the patch corner.
    pub fn local_points(&self) -> Vec<(f64, f64)> {
        let mut v = Vec::with_capacity(self.count());
        for j in 0..self.lines {
            for i in 0..self.per_line {
                v.push((self.end + i as f64 * self.pitch, self.edge + j as f64 * self.gauge));
            }
        }
        v
    }
}

/// One fastener instance: position relative to the set origin, axis (unit, i16-quantised),
/// grip in 0.1 mm. 20 bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct FastInst {
    pub p: [f32; 3],
    pub axis: [i16; 3],
    pub grip: u16,
}

impl FastInst {
    pub fn new(p: V3, origin: V3, axis: V3, grip: f64) -> FastInst {
        let a = axis.normalized();
        let q = |x: f64| (x.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        let d = p - origin;
        FastInst {
            p: [d.x as f32, d.y as f32, d.z as f32],
            axis: [q(a.x), q(a.y), q(a.z)],
            grip: (grip * 1e4).round().clamp(0.0, u16::MAX as f64) as u16,
        }
    }
    pub fn pos(&self, origin: V3) -> V3 {
        origin + v3(self.p[0] as f64, self.p[1] as f64, self.p[2] as f64)
    }
    pub fn axis(&self) -> V3 {
        v3(self.axis[0] as f64, self.axis[1] as f64, self.axis[2] as f64).normalized()
    }
}

/// All rivets of one size in a model: one prototype, many instances.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FastenerSet {
    pub size: usize,
    pub origin: V3,
    pub items: Vec<FastInst>,
}

impl FastenerSet {
    pub fn bytes(&self) -> usize {
        self.items.capacity() * std::mem::size_of::<FastInst>()
    }

    /// Instances within `radius` of `eye` (the near-camera rivet pass).
    pub fn near(&self, eye: V3, radius: f64) -> impl Iterator<Item = &FastInst> + '_ {
        let e = eye - self.origin;
        let (ex, ey, ez, r2) = (e.x as f32, e.y as f32, e.z as f32, (radius * radius) as f32);
        self.items.iter().filter(move |f| {
            let (dx, dy, dz) = (f.p[0] - ex, f.p[1] - ey, f.p[2] - ez);
            dx * dx + dy * dy + dz * dz <= r2
        })
    }
}

/// Level of detail by camera distance: 0 full, 1 coarse, None culled.
pub fn lod(dist: f64, d: f64) -> Option<u8> {
    let k = dist / d;
    if k < 600.0 {
        Some(0)
    } else if k < 2500.0 {
        Some(1)
    } else {
        None
    }
}

/// Prototype mesh of a driven rivet along +z, grip from z=0 to z=1 (scaled by the instance
/// grip in z): shank plus two snap heads. `lod` 0: 12 sides, domed; 1: 6 sides, flat heads.
pub fn proto_mesh(s: &RivetSize, lod: u8) -> Mesh {
    let n = if lod == 0 { 12 } else { 6 };
    let rings: &[(f64, f64)] = if lod == 0 {
        &[(1.0, 0.0), (0.87, 0.5), (0.5, 0.87), (0.0, 1.0)]
    } else {
        &[(1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
    };
    let (rh, k, rs) = (s.head_d / 2.0, s.head_k, s.d / 2.0);
    let mut pos: Vec<V3> = vec![];
    let mut tri: Vec<[u32; 3]> = vec![];
    // Profile rings from bottom head apex to top head apex: (radius, z).
    let mut prof: Vec<(f64, f64)> = vec![];
    for &(cr, sz) in rings.iter().rev() {
        prof.push((rh * cr, -k * sz));
    }
    prof.push((rs, 0.0));
    prof.push((rs, 1.0));
    for &(cr, sz) in rings.iter() {
        prof.push((rh * cr, 1.0 + k * sz));
    }
    for &(r, z) in &prof {
        for i in 0..n {
            let a = 2.0 * PI * i as f64 / n as f64;
            pos.push(v3(r * a.cos(), r * a.sin(), z));
        }
    }
    for j in 0..prof.len() - 1 {
        for i in 0..n {
            let (a, b) = ((j * n + i) as u32, (j * n + (i + 1) % n) as u32);
            let (c, d) = (a + n as u32, b + n as u32);
            tri.push([a, b, d]);
            tri.push([a, d, c]);
        }
    }
    let face_of = vec![0; tri.len()];
    Mesh { positions: pos, triangles: tri, face_of }
}
