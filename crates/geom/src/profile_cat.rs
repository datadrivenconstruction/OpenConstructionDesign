//! Parametric profile catalogue: a profile is requested by kind name plus named parameters,
//! each with an explicit unit. Every request is validated (a refusal names the field) and
//! lowered to a deterministic [`Section`]: outer ring counter-clockwise, holes clockwise,
//! lengths in metres. Circles are exact arcs, so areas are analytic.

use crate::arc::BulgeRing;
use crate::gsolid::Section;
use crate::math::{v2, Tol, V2};
use crate::profile::{Profile, ProfileError};
use std::f64::consts::PI;

/// Largest accepted dimension, metres.
const MAX_LEN: f64 = 1.0e4;

const UNIT_HELP: &str = "unit is required (m, cm, mm, ft, in)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    M,
    Cm,
    Mm,
    Ft,
    In,
}

impl Unit {
    pub fn parse(s: &str) -> Option<Unit> {
        match s.trim() {
            "m" => Some(Unit::M),
            "cm" => Some(Unit::Cm),
            "mm" => Some(Unit::Mm),
            "ft" => Some(Unit::Ft),
            "in" => Some(Unit::In),
            _ => None,
        }
    }

    pub fn to_metres(self, v: f64) -> f64 {
        v * match self {
            Unit::M => 1.0,
            Unit::Cm => 0.01,
            Unit::Mm => 0.001,
            Unit::Ft => 0.3048,
            Unit::In => 0.0254,
        }
    }
}

/// A number with its unit as written by the caller; an empty unit is refused.
#[derive(Clone, Debug, PartialEq)]
pub struct Quantity {
    pub value: f64,
    pub unit: String,
}

impl Quantity {
    pub fn new(value: f64, unit: &str) -> Self {
        Quantity {
            value,
            unit: unit.to_string(),
        }
    }
}

/// Refusal: the offending field and a human-readable reason.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamError {
    pub field: String,
    pub reason: String,
}

impl std::fmt::Display for ParamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.reason)
    }
}

fn err<T>(field: &str, reason: impl Into<String>) -> Result<T, ParamError> {
    Err(ParamError {
        field: field.to_string(),
        reason: reason.into(),
    })
}

fn unit_of(field: &str, text: &str) -> Result<Unit, ParamError> {
    match Unit::parse(text) {
        Some(u) => Ok(u),
        None if text.trim().is_empty() => err(field, UNIT_HELP),
        None => err(field, format!("unknown unit '{text}' (m, cm, mm, ft, in)")),
    }
}

/// Catalogue entry for listing to a caller.
pub struct KindInfo {
    pub name: &'static str,
    pub params: &'static [&'static str],
    pub doc: &'static str,
}

const FLANGED: &[&str] = &["height", "width", "web", "flange"];

pub const KINDS: &[KindInfo] = &[
    KindInfo { name: "rect", params: &["width", "height"], doc: "Rectangle centred on the origin" },
    KindInfo { name: "circle", params: &["diameter"], doc: "Solid circle" },
    KindInfo { name: "ring", params: &["outer_diameter", "inner_diameter"], doc: "Annulus" },
    KindInfo { name: "tube", params: &["outer_diameter", "wall"], doc: "Round tube" },
    KindInfo { name: "rect_tube", params: &["width", "height", "wall"], doc: "Rectangular hollow section" },
    KindInfo { name: "i", params: FLANGED, doc: "Symmetric I section, web vertical" },
    KindInfo { name: "h", params: FLANGED, doc: "Symmetric H section, web horizontal" },
    KindInfo { name: "c", params: FLANGED, doc: "Channel open towards +x" },
    KindInfo { name: "t", params: FLANGED, doc: "T section, flange on top" },
    KindInfo { name: "l", params: &["height", "width", "thickness"], doc: "Angle, legs along the -x and -y box edges" },
    KindInfo { name: "z", params: FLANGED, doc: "Z section; width is the flange length from the web face" },
];

/// Point list for free-form kinds: one unit for all coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct PointList {
    pub unit: String,
    pub pts: Vec<[f64; 2]>,
    /// Arc bulge per edge (edge i runs pts[i] to pts[i+1]); `None` means straight edges.
    pub bulges: Option<Vec<f64>>,
}

/// A request for one profile.
#[derive(Clone, Debug, PartialEq)]
pub enum ProfileSpec {
    Named {
        kind: String,
        params: Vec<(String, Quantity)>,
    },
    Polygon(PointList),
    /// Outer ring and holes; any of them may carry bulges.
    Rings {
        outer: PointList,
        holes: Vec<PointList>,
    },
}

struct Reader<'a> {
    params: &'a [(String, Quantity)],
}

impl Reader<'_> {
    /// Value in metres, strictly positive and finite.
    fn len(&self, name: &str) -> Result<f64, ParamError> {
        let mut it = self.params.iter().filter(|(n, _)| n == name);
        let Some((_, q)) = it.next() else {
            return err(name, "missing parameter");
        };
        if it.next().is_some() {
            return err(name, "given more than once");
        }
        let u = unit_of(name, &q.unit)?;
        if !q.value.is_finite() {
            return err(name, "value is not a finite number");
        }
        let m = u.to_metres(q.value);
        if m <= 0.0 {
            return err(name, "must be greater than zero");
        }
        if m > MAX_LEN {
            return err(name, "too large (limit 10000 m)");
        }
        Ok(m)
    }
}

fn rect_pts(w: f64, h: f64) -> Vec<V2> {
    let (a, b) = (w / 2.0, h / 2.0);
    vec![v2(-a, -b), v2(a, -b), v2(a, b), v2(-a, b)]
}

fn circle_ring(r: f64, ccw: bool) -> BulgeRing {
    let b = if ccw { 1.0 } else { -1.0 };
    BulgeRing {
        pts: vec![v2(r, 0.0), v2(-r, 0.0)],
        bulges: vec![b, b],
    }
}

/// Reverses a ring, keeping each arc on the same geometric curve.
fn reversed(r: &BulgeRing) -> BulgeRing {
    let n = r.pts.len();
    BulgeRing {
        pts: r.pts.iter().rev().copied().collect(),
        bulges: (0..n).map(|k| -r.bulges[(2 * n - 2 - k) % n]).collect(),
    }
}

fn rotate90(pts: Vec<V2>) -> Vec<V2> {
    pts.into_iter().map(|p| v2(-p.y, p.x)).collect()
}

/// Builds a section from a named kind. Unknown kinds and parameters are refused.
pub fn build_named(kind: &str, params: &[(String, Quantity)]) -> Result<Section, ParamError> {
    let key = kind.trim().to_ascii_lowercase();
    let Some(info) = KINDS.iter().find(|k| k.name == key) else {
        return err("kind", format!("unknown profile kind '{kind}'"));
    };
    for (n, _) in params {
        if !info.params.contains(&n.as_str()) {
            return err(
                n,
                format!("not a parameter of '{}' (expected {})", info.name, info.params.join(", ")),
            );
        }
    }
    let rd = Reader { params };
    let g = |n: &str| rd.len(n);
    match info.name {
        "rect" => Ok(Section::polygon(rect_pts(g("width")?, g("height")?))),
        "circle" => Ok(Section {
            outer: circle_ring(g("diameter")? / 2.0, true),
            holes: vec![],
        }),
        "ring" => {
            let (o, i) = (g("outer_diameter")?, g("inner_diameter")?);
            if i >= o {
                return err("inner_diameter", "must be smaller than outer_diameter");
            }
            Ok(Section {
                outer: circle_ring(o / 2.0, true),
                holes: vec![circle_ring(i / 2.0, false)],
            })
        }
        "tube" => {
            let (o, t) = (g("outer_diameter")?, g("wall")?);
            if 2.0 * t >= o {
                return err("wall", "must be smaller than half of outer_diameter");
            }
            Ok(Section {
                outer: circle_ring(o / 2.0, true),
                holes: vec![circle_ring(o / 2.0 - t, false)],
            })
        }
        "rect_tube" => {
            let (w, h, t) = (g("width")?, g("height")?, g("wall")?);
            if 2.0 * t >= w.min(h) {
                return err("wall", "must be smaller than half of the smaller side");
            }
            let mut hole = rect_pts(w - 2.0 * t, h - 2.0 * t);
            hole.reverse();
            Ok(Section {
                outer: BulgeRing::from_polygon(rect_pts(w, h)),
                holes: vec![BulgeRing::from_polygon(hole)],
            })
        }
        "l" => {
            let (h, b, t) = (g("height")?, g("width")?, g("thickness")?);
            if t >= b {
                return err("thickness", "must be smaller than width");
            }
            if t >= h {
                return err("thickness", "must be smaller than height");
            }
            let (x, y) = (b / 2.0, h / 2.0);
            Ok(Section::polygon(vec![
                v2(-x, -y),
                v2(x, -y),
                v2(x, -y + t),
                v2(-x + t, -y + t),
                v2(-x + t, y),
                v2(-x, y),
            ]))
        }
        name => {
            let (h, b, tw, tf) = (g("height")?, g("width")?, g("web")?, g("flange")?);
            if tw >= b {
                return err("web", "must be smaller than width");
            }
            if name != "t" && 2.0 * tf >= h {
                return err("flange", "two flanges must be thinner than height");
            }
            if tf >= h {
                return err("flange", "must be smaller than height");
            }
            let (x, y, w2) = (b / 2.0, h / 2.0, tw / 2.0);
            let pts = match name {
                "i" | "h" => {
                    let p = vec![
                        v2(-x, -y), v2(x, -y), v2(x, -y + tf), v2(w2, -y + tf),
                        v2(w2, y - tf), v2(x, y - tf), v2(x, y), v2(-x, y),
                        v2(-x, y - tf), v2(-w2, y - tf), v2(-w2, -y + tf), v2(-x, -y + tf),
                    ];
                    if name == "h" { rotate90(p) } else { p }
                }
                "c" => vec![
                    v2(-x, -y), v2(x, -y), v2(x, -y + tf), v2(-x + tw, -y + tf),
                    v2(-x + tw, y - tf), v2(x, y - tf), v2(x, y), v2(-x, y),
                ],
                "t" => vec![
                    v2(-w2, -y), v2(w2, -y), v2(w2, y - tf), v2(x, y - tf),
                    v2(x, y), v2(-x, y), v2(-x, y - tf), v2(-w2, y - tf),
                ],
                // z: `b` is the flange length measured from the web face.
                _ => vec![
                    v2(w2 - b, -y), v2(w2, -y), v2(w2, y - tf), v2(b - w2, y - tf),
                    v2(b - w2, y), v2(-w2, y), v2(-w2, -y + tf), v2(w2 - b, -y + tf),
                ],
            };
            Ok(Section::polygon(pts))
        }
    }
}

fn to_ring(field: &str, pl: &PointList) -> Result<BulgeRing, ParamError> {
    let u = unit_of(field, &pl.unit)?;
    if pl.pts.len() < 3 {
        return err(field, "needs at least 3 points");
    }
    let mut pts = Vec::with_capacity(pl.pts.len());
    for (i, p) in pl.pts.iter().enumerate() {
        if !p[0].is_finite() || !p[1].is_finite() {
            return err(field, format!("point {i} is not finite"));
        }
        let (x, y) = (u.to_metres(p[0]), u.to_metres(p[1]));
        if x.abs() > MAX_LEN || y.abs() > MAX_LEN {
            return err(field, format!("point {i} is too far from the origin"));
        }
        pts.push(v2(x, y));
    }
    let bulges = match &pl.bulges {
        None => vec![0.0; pts.len()],
        Some(b) => {
            if b.len() != pts.len() {
                return err(field, "bulges must have one value per point");
            }
            if let Some(i) = b.iter().position(|v| !v.is_finite()) {
                return err(field, format!("bulge {i} is not finite"));
            }
            b.clone()
        }
    };
    Ok(BulgeRing { pts, bulges })
}

/// Builds a section from any spec; outer ring counter-clockwise, holes clockwise.
pub fn build(spec: &ProfileSpec) -> Result<Section, ParamError> {
    match spec {
        ProfileSpec::Named { kind, params } => build_named(kind, params),
        ProfileSpec::Polygon(pl) => build_rings("points", pl, &[]),
        ProfileSpec::Rings { outer, holes } => build_rings("outer", outer, holes),
    }
}

fn build_rings(name: &str, outer: &PointList, holes: &[PointList]) -> Result<Section, ParamError> {
    let mut o = to_ring(name, outer)?;
    if o.area() < 0.0 {
        o = reversed(&o);
    }
    let mut hs = Vec::with_capacity(holes.len());
    for (i, h) in holes.iter().enumerate() {
        let mut r = to_ring(&format!("holes[{i}]"), h)?;
        if r.area() > 0.0 {
            r = reversed(&r);
        }
        hs.push(r);
    }
    // Validate on a fine tessellation: self-crossings, holes outside, overlapping holes.
    let flat = |r: &BulgeRing| r.tessellate(1.0e-4);
    if let Err(e) = Profile::new(flat(&o), hs.iter().map(flat).collect(), Tol::default()) {
        let ring_field = |ring: usize| {
            if ring == 0 {
                name.to_string()
            } else {
                format!("holes[{}]", ring - 1)
            }
        };
        let field = match &e {
            ProfileError::HoleOutside { hole } => format!("holes[{hole}]"),
            ProfileError::HolesOverlap { b, .. } => format!("holes[{b}]"),
            ProfileError::BadCoordinate { ring, .. }
            | ProfileError::TooFewPoints { ring }
            | ProfileError::ZeroLengthEdge { ring, .. }
            | ProfileError::ZeroArea { ring }
            | ProfileError::SelfIntersection { ring, .. } => ring_field(*ring),
        };
        return err(&field, format!("invalid outline ({e:?})"));
    }
    Ok(Section { outer: o, holes: hs })
}

/// Net area of a section: outer minus holes (hole rings are clockwise, so they subtract).
pub fn section_area(s: &Section) -> f64 {
    s.outer.area() + s.holes.iter().map(|h| h.area()).sum::<f64>()
}

/// Closed-form area of a named kind from the same parameters; cross-check for `build_named`.
pub fn named_area(kind: &str, p: &[(String, Quantity)]) -> Result<f64, ParamError> {
    let rd = Reader { params: p };
    let g = |n: &str| rd.len(n);
    let key = kind.trim().to_ascii_lowercase();
    Ok(match key.as_str() {
        "rect" => g("width")? * g("height")?,
        "circle" => PI * g("diameter")?.powi(2) / 4.0,
        "ring" => PI * (g("outer_diameter")?.powi(2) - g("inner_diameter")?.powi(2)) / 4.0,
        "tube" => {
            let (o, t) = (g("outer_diameter")?, g("wall")?);
            PI * (o * o - (o - 2.0 * t).powi(2)) / 4.0
        }
        "rect_tube" => {
            let (w, h, t) = (g("width")?, g("height")?, g("wall")?);
            w * h - (w - 2.0 * t) * (h - 2.0 * t)
        }
        "i" | "h" => {
            let (h, b, tw, tf) = (g("height")?, g("width")?, g("web")?, g("flange")?);
            2.0 * b * tf + (h - 2.0 * tf) * tw
        }
        "c" | "z" => {
            let (h, b, tw, tf) = (g("height")?, g("width")?, g("web")?, g("flange")?);
            tw * h + 2.0 * (b - tw) * tf
        }
        "t" => {
            let (h, b, tw, tf) = (g("height")?, g("width")?, g("web")?, g("flange")?);
            b * tf + tw * (h - tf)
        }
        "l" => {
            let (h, b, t) = (g("height")?, g("width")?, g("thickness")?);
            t * (b + h - t)
        }
        _ => return err("kind", "unknown profile kind"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type Params = Vec<(String, Quantity)>;

    fn q(v: &[(&str, f64, &str)]) -> Params {
        v.iter()
            .map(|(n, x, u)| (n.to_string(), Quantity::new(*x, u)))
            .collect()
    }

    fn rel(a: f64, b: f64) -> f64 {
        (a - b).abs() / b.abs()
    }

    fn i_params() -> Params {
        q(&[("height", 300.0, "mm"), ("width", 150.0, "mm"), ("web", 7.1, "mm"), ("flange", 10.7, "mm")])
    }

    fn all_cases() -> Vec<(&'static str, Params)> {
        let hw = [("height", 0.3, "m"), ("width", 0.15, "m"), ("web", 8.0, "mm"), ("flange", 12.0, "mm")];
        vec![
            ("rect", q(&[("width", 200.0, "mm"), ("height", 0.5, "m")])),
            ("circle", q(&[("diameter", 12.0, "in")])),
            ("ring", q(&[("outer_diameter", 300.0, "mm"), ("inner_diameter", 100.0, "mm")])),
            ("tube", q(&[("outer_diameter", 114.3, "mm"), ("wall", 4.0, "mm")])),
            ("rect_tube", q(&[("width", 100.0, "mm"), ("height", 60.0, "mm"), ("wall", 5.0, "mm")])),
            ("i", i_params()),
            ("h", q(&hw)),
            ("c", q(&hw)),
            ("t", q(&hw)),
            ("z", q(&hw)),
            ("l", q(&[("height", 100.0, "mm"), ("width", 75.0, "mm"), ("thickness", 8.0, "mm")])),
        ]
    }

    #[test]
    fn i_profile_area_matches_formula() {
        let s = build_named("I", &i_params()).unwrap();
        let (h, b, tw, tf) = (0.3, 0.15, 0.0071, 0.0107);
        let want = 2.0 * b * tf + (h - 2.0 * tf) * tw;
        assert!(rel(section_area(&s), want) < 1e-9);
        assert_eq!(s.outer.pts.len(), 12);
    }

    #[test]
    fn every_kind_area_matches_analytic_value() {
        for (k, p) in all_cases() {
            let s = build_named(k, &p).unwrap();
            let want = named_area(k, &p).unwrap();
            assert!(rel(section_area(&s), want) < 1e-9, "{k}");
        }
        let s = build_named("circle", &q(&[("diameter", 1.0, "m")])).unwrap();
        assert!(rel(section_area(&s), PI / 4.0) < 1e-9);
    }

    #[test]
    fn outlines_closed_and_counter_clockwise() {
        for (k, p) in all_cases() {
            let s = build_named(k, &p).unwrap();
            assert!(s.outer.area() > 0.0, "{k} outer must be counter-clockwise");
            assert_eq!(s.outer.pts.len(), s.outer.bulges.len(), "{k}");
            // closed by wrap-around: the first point is not repeated at the end
            assert_ne!(s.outer.pts.first(), s.outer.pts.last(), "{k}");
            assert_eq!(s, build_named(k, &p).unwrap(), "{k} must be deterministic");
        }
    }

    #[test]
    fn holes_clockwise() {
        for (k, p) in all_cases() {
            let s = build_named(k, &p).unwrap();
            for h in &s.holes {
                assert!(h.area() < 0.0, "{k} hole must be clockwise");
            }
        }
        // a user hole given counter-clockwise is flipped
        let sq = |a: f64| vec![[-a, -a], [a, -a], [a, a], [-a, a]];
        let pl = |pts| PointList { unit: "mm".into(), pts, bulges: None };
        let s = build(&ProfileSpec::Rings { outer: pl(sq(100.0)), holes: vec![pl(sq(20.0))] }).unwrap();
        assert!(s.outer.area() > 0.0 && s.holes[0].area() < 0.0);
        assert!(rel(section_area(&s), 0.2 * 0.2 - 0.04 * 0.04) < 1e-9);
    }

    #[test]
    fn profile_params_validated_with_units() {
        let e = build_named("rect", &q(&[("width", 200.0, ""), ("height", 1.0, "m")])).unwrap_err();
        assert_eq!(e.field, "width");
        assert!(e.reason.contains("unit"));
        let e = build_named("rect", &q(&[("width", 200.0, "yd"), ("height", 1.0, "m")])).unwrap_err();
        assert!(e.field == "width" && e.reason.contains("yd"));
        let e = build_named("rect", &q(&[("width", -1.0, "m"), ("height", 1.0, "m")])).unwrap_err();
        assert_eq!(e.field, "width");
        let e = build_named("rect", &q(&[("width", f64::NAN, "m"), ("height", 1.0, "m")])).unwrap_err();
        assert_eq!(e.field, "width");
        let e = build_named("rect", &q(&[("width", 1.0, "m")])).unwrap_err();
        assert_eq!(e.field, "height");
        let e = build_named("rect", &q(&[("width", 1.0, "m"), ("height", 1.0, "m"), ("depth", 1.0, "m")])).unwrap_err();
        assert_eq!(e.field, "depth");
        let e = build_named("rect", &q(&[("width", 1.0, "m"), ("width", 2.0, "m"), ("height", 1.0, "m")])).unwrap_err();
        assert!(e.field == "width" && e.reason.contains("more than once"));
        assert_eq!(build_named("blob", &[]).unwrap_err().field, "kind");
        // geometric consistency
        let e = build_named("i", &q(&[("height", 20.0, "mm"), ("width", 50.0, "mm"), ("web", 5.0, "mm"), ("flange", 10.0, "mm")])).unwrap_err();
        assert_eq!(e.field, "flange");
        let e = build_named("ring", &q(&[("outer_diameter", 10.0, "mm"), ("inner_diameter", 10.0, "mm")])).unwrap_err();
        assert_eq!(e.field, "inner_diameter");
        let e = build_named("l", &q(&[("height", 50.0, "mm"), ("width", 50.0, "mm"), ("thickness", 50.0, "mm")])).unwrap_err();
        assert_eq!(e.field, "thickness");
        // units are converted
        let a = build_named("rect", &q(&[("width", 1000.0, "mm"), ("height", 1.0, "m")])).unwrap();
        let b = build_named("rect", &q(&[("width", 100.0, "cm"), ("height", 39.37007874015748, "in")])).unwrap();
        assert!(rel(section_area(&a), 1.0) < 1e-12 && rel(section_area(&b), 1.0) < 1e-9);
        let ft = build_named("rect", &q(&[("width", 1.0, "ft"), ("height", 1.0, "ft")])).unwrap();
        assert!(rel(section_area(&ft), 0.3048 * 0.3048) < 1e-12);
    }

    #[test]
    fn polygon_refusals_name_the_field() {
        let pl = |pts: Vec<[f64; 2]>, u: &str| PointList { unit: u.into(), pts, bulges: None };
        let e = build(&ProfileSpec::Polygon(pl(vec![[0.0, 0.0], [1.0, 0.0]], "m"))).unwrap_err();
        assert_eq!(e.field, "points");
        let bow = vec![[0.0, 0.0], [1.0, 1.0], [1.0, 0.0], [0.0, 1.0]];
        assert_eq!(build(&ProfileSpec::Polygon(pl(bow, "m"))).unwrap_err().field, "points");
        let tri = vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let e = build(&ProfileSpec::Polygon(pl(tri.clone(), ""))).unwrap_err();
        assert!(e.reason.contains("unit"));
        let sq = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let far = vec![[5.0, 5.0], [6.0, 5.0], [6.0, 6.0], [5.0, 6.0]];
        let e = build(&ProfileSpec::Rings { outer: pl(sq, "m"), holes: vec![pl(far, "m")] }).unwrap_err();
        assert_eq!(e.field, "holes[0]");
        let bad = PointList { unit: "m".into(), pts: tri, bulges: Some(vec![0.0]) };
        assert_eq!(build(&ProfileSpec::Polygon(bad)).unwrap_err().field, "points");
    }

    #[test]
    fn bulge_polygon_area_and_orientation() {
        // semicircle on the bottom edge of a 2x2 square: area 4 + pi/2, either input orientation
        let ccw = PointList {
            unit: "m".into(),
            pts: vec![[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]],
            bulges: Some(vec![1.0, 0.0, 0.0, 0.0]),
        };
        let n = 4;
        let b = ccw.bulges.clone().unwrap();
        let mut cw = ccw.clone();
        cw.pts.reverse();
        cw.bulges = Some((0..n).map(|k| -b[(2 * n - 2 - k) % n]).collect());
        let want = 4.0 + PI / 2.0;
        let s1 = build(&ProfileSpec::Polygon(ccw)).unwrap();
        let s2 = build(&ProfileSpec::Polygon(cw)).unwrap();
        assert!(s1.outer.area() > 0.0 && s2.outer.area() > 0.0);
        assert!(rel(section_area(&s1), want) < 1e-9);
        assert!(rel(section_area(&s2), want) < 1e-9);
    }
}
