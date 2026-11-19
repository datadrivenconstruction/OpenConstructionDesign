//! Rolled steel sections from dimension tables: IPE, HEA, HEB, HEM, UPN (EN 10365), UB, UC
//! (BS 4-1), the parallel-flange I-beams of GOST 57837 and W shapes (ASTM A6). The files in
//! `data/steel/` hold nominal dimensions only; every property is computed here from them,
//! root fillets included (provenance in `data/steel/README.md`).
//!
//! Section plane (y, z) as in `section_props`: y across the flanges, z along the depth.
//! `iy` = integral of z^2 dA (strong axis), `iz` = integral of y^2 dA, `ry`, `rz` the radii of
//! gyration. SI units: m, m2, m3, m4, m6, kg/m.
//!
//! Methods:
//! - Parallel-flange I: exact closed form, plates plus four quarter-circle spandrel fillets
//!   (`i_closed_form`). Channels: exact polygon integrals over the outline with the fillet and
//!   toe arcs sampled by `ARC_SEGS` chords (relative error below 1e-4).
//! - Wpl: first moments about the equal-area axis (bisection for the channel weak axis).
//! - It: I, thin plates plus the fillet junction term of El Darwish and Johnston (1965), within
//!   a few per cent of the exact value; channel, thin plates with net web height, fillets
//!   neglected (about 6 % low).
//! - Iw: I, Iz (h - tf)^2 / 4; channel, thin-walled closed form on the wall centre lines.
//!
//! Properties are computed once on first use and never stored: the tables stay plain
//! dimensions, and a formula fix updates every row without regenerating data.

use std::f64::consts::PI;
use std::sync::OnceLock;

use crate::math::{v2, V2};
use crate::member::Shape;

/// Steel density for the mass per metre, kg/m3.
pub const DENSITY: f64 = 7850.0;
/// Chords per fillet arc of the channel outline.
pub const ARC_SEGS: usize = 64;

const TABLES: [(&str, &str, FileUnit); 4] = [
    ("EN 10365", include_str!("../../data/steel/en_ipe_he_upn.csv"), FileUnit::Mm),
    ("BS 4-1", include_str!("../../data/steel/uk_ub_uc.csv"), FileUnit::Mm),
    ("GOST 57837", include_str!("../../data/steel/ru_gost57837.csv"), FileUnit::Mm),
    ("ASTM A6", include_str!("../../data/steel/us_w.csv"), FileUnit::Inch),
];

/// Length unit of a table file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileUnit {
    Mm,
    Inch,
}

impl FileUnit {
    /// Metres, converted the same way as the other section tables (bit-identical shapes).
    pub fn m(self, v: f64) -> f64 {
        match self {
            FileUnit::Mm => v / 1000.0,
            FileUnit::Inch => v * 0.0254,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RolledKind {
    /// Parallel flanges.
    I,
    /// Channel with inner flange slope; `tf` is measured at b/2 from the back of the web.
    Channel,
}

/// Nominal dimensions, metres. `r1` root radius, `r2` toe radius, `slope` inner flange slope.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RolledDims {
    pub kind: RolledKind,
    pub h: f64,
    pub b: f64,
    pub tw: f64,
    pub tf: f64,
    pub r1: f64,
    pub r2: f64,
    pub slope: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RolledProps {
    pub area: f64,
    pub iy: f64,
    pub iz: f64,
    pub wel_y: f64,
    pub wel_z: f64,
    pub wpl_y: f64,
    pub wpl_z: f64,
    pub ry: f64,
    pub rz: f64,
    /// Saint-Venant torsion constant.
    pub it: f64,
    /// Warping constant about the shear centre.
    pub iw: f64,
    /// Centroid offset from the back of the web along y (channels; 0 for I).
    pub ey: f64,
    pub mass: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RolledRow {
    pub name: String,
    /// Latin spelling of the designation, empty when the name already is one.
    pub alias: String,
    /// Standard the dimensions follow.
    pub standard: &'static str,
    pub dims: RolledDims,
    pub shape: Shape,
    pub props: RolledProps,
}

impl RolledDims {
    pub fn check(&self) -> Result<(), String> {
        let v = [self.h, self.b, self.tw, self.tf, self.r1, self.r2, self.slope];
        if v.iter().any(|x| !x.is_finite() || *x < 0.0) || self.h <= 0.0 || self.b <= 0.0 {
            return Err("dimensions must be finite and positive".into());
        }
        if self.tw <= 0.0 || self.tf <= 0.0 || 2.0 * self.tf >= self.h || self.tw >= self.b {
            return Err("web and flanges must fit inside the outline".into());
        }
        let outstand = match self.kind {
            RolledKind::I => (self.b - self.tw) / 2.0,
            RolledKind::Channel => self.b - self.tw,
        };
        if self.r1 + self.r2 >= outstand || 2.0 * (self.tf + self.r1) >= self.h {
            return Err("fillets larger than the flange outstand or the web".into());
        }
        Ok(())
    }

    /// Outline without fillets, as the member geometry uses it.
    pub fn shape(&self) -> Shape {
        let (h, b, tw, tf) = (self.h, self.b, self.tw, self.tf);
        match self.kind {
            RolledKind::I => Shape::I { h, b, tw, tf },
            RolledKind::Channel => Shape::Channel { h, b, tw, tf },
        }
    }
}

/// All properties of a rolled section.
pub fn props(d: &RolledDims) -> RolledProps {
    let mut p = match d.kind {
        RolledKind::I => {
            let c = i_closed_form(d.h, d.b, d.tw, d.tf, d.r1);
            RolledProps {
                area: c.area,
                iy: c.iy,
                iz: c.iz,
                wel_y: c.iy / (d.h / 2.0),
                wel_z: c.iz / (d.b / 2.0),
                wpl_y: c.wpl_y,
                wpl_z: c.wpl_z,
                it: i_torsion(d.h, d.b, d.tw, d.tf, d.r1),
                iw: c.iz * (d.h - d.tf).powi(2) / 4.0,
                ..Default::default()
            }
        }
        RolledKind::Channel => {
            let o = outline(d, ARC_SEGS);
            let g = poly_props(&o);
            let (bp, hp) = (d.b - d.tw / 2.0, d.h - d.tf);
            let iw = d.tf * bp.powi(3) * hp * hp / 12.0 * (3.0 * bp * d.tf + 2.0 * hp * d.tw)
                / (6.0 * bp * d.tf + hp * d.tw);
            RolledProps {
                area: g.area,
                iy: g.iy,
                iz: g.iz,
                wel_y: g.iy / g.cz,
                wel_z: g.iz / g.cy,
                wpl_y: plastic_about(&o, 1, Some(0.0)),
                wpl_z: plastic_modulus(&o, 0),
                it: (2.0 * d.b * d.tf.powi(3) + (d.h - 2.0 * d.tf) * d.tw.powi(3)) / 3.0,
                iw,
                ey: g.yc,
                ..Default::default()
            }
        }
    };
    p.ry = (p.iy / p.area).sqrt();
    p.rz = (p.iz / p.area).sqrt();
    p.mass = p.area * DENSITY;
    p
}

/// Area, second moments and plastic moduli of a parallel-flange I.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IClosed {
    pub area: f64,
    pub iy: f64,
    pub iz: f64,
    pub wpl_y: f64,
    pub wpl_z: f64,
}

/// Closed form for a doubly symmetric I with root radius `r`: two flanges, the web between
/// them, and four spandrels (square r x r minus a quarter disc). About its two legs a spandrel
/// has area (1 - pi/4) r^2, first moment (5/6 - pi/4) r^3, second moment (1 - 5 pi/16) r^4.
pub fn i_closed_form(h: f64, b: f64, tw: f64, tf: f64, r: f64) -> IClosed {
    let af = (1.0 - PI / 4.0) * r * r;
    let s = (5.0 / 6.0 - PI / 4.0) * r.powi(3);
    let i0 = (1.0 - 5.0 * PI / 16.0) * r.powi(4);
    let hw = h - 2.0 * tf;
    let (zc, yc) = (h / 2.0 - tf, tw / 2.0);
    let zf = (h - tf) / 2.0;
    IClosed {
        area: 2.0 * b * tf + hw * tw + 4.0 * af,
        iy: 2.0 * (b * tf.powi(3) / 12.0 + b * tf * zf * zf)
            + tw * hw.powi(3) / 12.0
            + 4.0 * (zc * zc * af - 2.0 * zc * s + i0),
        iz: 2.0 * tf * b.powi(3) / 12.0 + hw * tw.powi(3) / 12.0 + 4.0 * (yc * yc * af + 2.0 * yc * s + i0),
        wpl_y: 2.0 * (b * tf * zf + tw * hw * hw / 8.0 + 2.0 * (zc * af - s)),
        wpl_z: 2.0 * (tf * b * b / 4.0 + hw * tw * tw / 8.0 + 2.0 * (yc * af + s)),
    }
}

/// Torsion constant of a rolled I: flanges and web as thin plates, the junction term
/// 2 alpha D^4 for the fillets and the end correction -0.420 tf^4 (El Darwish, Johnston).
pub fn i_torsion(h: f64, b: f64, tw: f64, tf: f64, r: f64) -> f64 {
    let alpha = -0.042 + 0.2204 * tw / tf + 0.1355 * r / tf - 0.0865 * r * tw / (tf * tf)
        - 0.0725 * tw * tw / (tf * tf);
    let d = ((tf + r).powi(2) + tw * (r + tw / 4.0)) / (2.0 * r + tf);
    2.0 / 3.0 * b * tf.powi(3) + (h - 2.0 * tf) * tw.powi(3) / 3.0 + 2.0 * alpha * d.powi(4)
        - 0.420 * tf.powi(4)
}

/// Counter-clockwise outline with arcs of `segs` chords. I: centred on the origin; channel:
/// back of the web on y = 0, flanges towards +y, symmetric about z = 0.
pub fn outline(d: &RolledDims, segs: usize) -> Vec<V2> {
    let (h2, b, tw, r1, r2) = (d.h / 2.0, d.b, d.tw, d.r1, d.r2);
    let (pts, radii): (Vec<V2>, Vec<f64>) = match d.kind {
        RolledKind::I => {
            let (b2, w, t) = (b / 2.0, tw / 2.0, d.tf);
            (
                vec![
                    v2(-b2, -h2), v2(b2, -h2), v2(b2, -h2 + t), v2(w, -h2 + t),
                    v2(w, h2 - t), v2(b2, h2 - t), v2(b2, h2), v2(-b2, h2),
                    v2(-b2, h2 - t), v2(-w, h2 - t), v2(-w, -h2 + t), v2(-b2, -h2 + t),
                ],
                vec![0.0, 0.0, 0.0, r1, r1, 0.0, 0.0, 0.0, 0.0, r1, r1, 0.0],
            )
        }
        RolledKind::Channel => {
            let t = |y: f64| d.tf + d.slope * (b / 2.0 - y);
            (
                vec![
                    v2(0.0, -h2), v2(b, -h2), v2(b, -h2 + t(b)), v2(tw, -h2 + t(tw)),
                    v2(tw, h2 - t(tw)), v2(b, h2 - t(b)), v2(b, h2), v2(0.0, h2),
                ],
                vec![0.0, 0.0, r2, r1, r1, r2, 0.0, 0.0],
            )
        }
    };
    round_corners(&pts, &radii, segs)
}

/// Replaces each corner with radius > 0 by a tangent arc of `segs` chords.
fn round_corners(pts: &[V2], radii: &[f64], segs: usize) -> Vec<V2> {
    let n = pts.len();
    let unit = |a: V2, b: V2| {
        let (x, y) = (b.x - a.x, b.y - a.y);
        let l = x.hypot(y);
        v2(x / l, y / l)
    };
    let mut out = Vec::with_capacity(n * (segs + 1));
    for i in 0..n {
        let (p, r) = (pts[i], radii[i]);
        if r <= 0.0 {
            out.push(p);
            continue;
        }
        let u = unit(p, pts[(i + n - 1) % n]);
        let w = unit(p, pts[(i + 1) % n]);
        let th = (u.x * w.x + u.y * w.y).clamp(-1.0, 1.0).acos();
        let t = r / (th / 2.0).tan();
        let bis = unit(v2(0.0, 0.0), v2(u.x + w.x, u.y + w.y));
        let k = r / (th / 2.0).sin();
        let c = v2(p.x + bis.x * k, p.y + bis.y * k);
        let a1 = (p.y + u.y * t - c.y).atan2(p.x + u.x * t - c.x);
        let a2 = (p.y + w.y * t - c.y).atan2(p.x + w.x * t - c.x);
        let mut da = a2 - a1;
        while da > PI {
            da -= 2.0 * PI;
        }
        while da < -PI {
            da += 2.0 * PI;
        }
        for s in 0..=segs {
            let a = a1 + da * s as f64 / segs as f64;
            out.push(v2(c.x + r * a.cos(), c.y + r * a.sin()));
        }
    }
    out
}

/// Polygon integrals: area, centroid, centroidal second moments, extreme fibre distances.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PolyProps {
    pub area: f64,
    pub yc: f64,
    pub zc: f64,
    pub iy: f64,
    pub iz: f64,
    /// Largest distance from the centroid to the outline along y and along z.
    pub cy: f64,
    pub cz: f64,
}

/// Exact integrals of a simple counter-clockwise polygon (Green's theorem), x = y, y = z.
pub fn poly_props(p: &[V2]) -> PolyProps {
    let [a, sy, sz, iyy, izz] = moments(p);
    let (yc, zc) = (sy / a, sz / a);
    let (cy, cz) = p.iter().fold((0.0f64, 0.0f64), |(m, n), q| (m.max((q.x - yc).abs()), n.max((q.y - zc).abs())));
    PolyProps { area: a, yc, zc, iy: iyy - a * zc * zc, iz: izz - a * yc * yc, cy, cz }
}

/// [A, integral y, integral z, integral z^2, integral y^2] about the origin.
fn moments(p: &[V2]) -> [f64; 5] {
    let n = p.len();
    let mut m = [0.0; 5];
    for i in 0..n {
        let (a, b) = (p[i], p[(i + 1) % n]);
        let c = a.x * b.y - b.x * a.y;
        m[0] += c / 2.0;
        m[1] += (a.x + b.x) * c / 6.0;
        m[2] += (a.y + b.y) * c / 6.0;
        m[3] += (a.y * a.y + a.y * b.y + b.y * b.y) * c / 12.0;
        m[4] += (a.x * a.x + a.x * b.x + b.x * b.x) * c / 12.0;
    }
    m
}

/// Part of the polygon with coordinate `axis` (0 = y, 1 = z) above `c` (half-plane clip).
fn clip_above(p: &[V2], axis: usize, c: f64) -> Vec<V2> {
    let q = |v: V2| if axis == 0 { v.x } else { v.y };
    let n = p.len();
    let mut out = Vec::with_capacity(n + 4);
    for i in 0..n {
        let (a, b) = (p[i], p[(i + 1) % n]);
        let (qa, qb) = (q(a) - c, q(b) - c);
        if qa >= 0.0 {
            out.push(a);
        }
        if (qa >= 0.0) != (qb >= 0.0) {
            let t = qa / (qa - qb);
            out.push(v2(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y)));
        }
    }
    out
}

/// Plastic modulus of a simple polygon, the neutral line normal to `axis` (0 = y, 1 = z)
/// at the equal-area position: sum of the first moments of both halves about it.
pub fn plastic_modulus(p: &[V2], axis: usize) -> f64 {
    plastic_about(p, axis, None)
}

/// `plastic_modulus` with a known neutral line `pna` (bisection when None).
fn plastic_about(p: &[V2], axis: usize, pna: Option<f64>) -> f64 {
    let q = |v: &V2| if axis == 0 { v.x } else { v.y };
    let total = moments(p)[0];
    let first = |c: f64| {
        let m = moments(&clip_above(p, axis, c));
        (m[0], m[1 + axis] - c * m[0])
    };
    let c = pna.unwrap_or_else(|| {
        let (mut lo, mut hi) = p.iter().fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(q(v)), h.max(q(v))));
        for _ in 0..100 {
            let mid = 0.5 * (lo + hi);
            if first(mid).0 > total / 2.0 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    });
    let s_up = first(c).1;
    // the lower part's first moment about c is the whole one minus the upper (it is negative)
    let s_all = moments(p)[1 + axis] - c * total;
    2.0 * s_up - s_all
}

/// Parses one table; `k` converts the file unit to metres.
pub fn parse(text: &str, standard: &'static str, unit: FileUnit) -> Result<Vec<RolledRow>, String> {
    let k = |v: f64| unit.m(v);
    let mut out = vec![];
    for (n, line) in text.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with("name,") {
            continue;
        }
        let at = |e: String| format!("{standard}:{}: {e}", n + 1);
        let c: Vec<&str> = l.split(',').map(str::trim).collect();
        if c.len() != 10 {
            return Err(at("10 fields expected".into()));
        }
        let mut f = [0.0; 7];
        for (i, v) in c[3..].iter().enumerate() {
            f[i] = v.parse().map_err(|_| at(format!("bad number '{v}'")))?;
        }
        let kind = match c[2] {
            "I" => RolledKind::I,
            "U" => RolledKind::Channel,
            o => return Err(at(format!("unknown kind '{o}'"))),
        };
        let dims = RolledDims {
            kind,
            h: k(f[0]),
            b: k(f[1]),
            tw: k(f[2]),
            tf: k(f[3]),
            r1: k(f[4]),
            r2: k(f[5]),
            slope: f[6],
        };
        dims.check().map_err(|e| at(format!("{}: {e}", c[0])))?;
        out.push(RolledRow {
            name: c[0].into(),
            alias: c[1].into(),
            standard,
            shape: dims.shape(),
            props: props(&dims),
            dims,
        });
    }
    Ok(out)
}

/// Every built-in rolled section, in table order.
pub fn table() -> &'static [RolledRow] {
    static T: OnceLock<Vec<RolledRow>> = OnceLock::new();
    T.get_or_init(|| {
        TABLES
            .iter()
            .flat_map(|(s, text, u)| parse(text, *s, *u).expect("built-in rolled section table"))
            .collect()
    })
}

/// A rolled section by designation or alias, ignoring case and spaces ("HEM 300",
/// "ub 305x165x40", "20B1", "W12X26").
pub fn find(name: &str) -> Option<&'static RolledRow> {
    let key = norm(name);
    if key.is_empty() {
        return None;
    }
    table().iter().find(|r| norm(&r.name) == key || (!r.alias.is_empty() && norm(&r.alias) == key))
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}
