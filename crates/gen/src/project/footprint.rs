// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Footprints of any shape: a polygon with straight and arc edges, a named template, or a
//! plot outline set back inward; optional convex courtyards; per-floor set-back, offset,
//! rotation, twist and taper. Arcs become chords within the asked tolerance.

use super::limits::{field, ParamDef};
use super::offset;
use ddc_geom::arc::BulgeRing;
use ddc_geom::v2;
use serde_json::{Map, Value as J};

pub(crate) type P = (f64, f64);

/// A plan region: the outer ring counter-clockwise, holes clockwise.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub outer: Vec<P>,
    pub holes: Vec<Vec<P>>,
}

/// From storey `from` (0 = ground) up: the base plate set back, moved and turned.
#[derive(Clone, Debug, PartialEq)]
pub struct Level {
    pub from: u32,
    pub setback: f64,
    pub offset: P,
    /// Degrees counter-clockwise about the plate's pivot.
    pub rotation: f64,
}

/// The footprint of a plate building and how its floors vary.
#[derive(Clone, Debug, PartialEq)]
pub struct Plate {
    pub base: Region,
    pub levels: Vec<Level>,
    /// Degrees per storey, about the pivot.
    pub twist: f64,
    /// Set-back per storey, m; negative grows the floors upward.
    pub taper: f64,
    /// Turning point of every rotation: the centroid of the base plate.
    pub pivot: P,
    /// Degrees the footprint was turned; terrace steps face their side of the turned plate.
    pub rotation: f64,
    /// What the brief gave: polygon, plot or the template name.
    pub source: String,
}

/// How a pitched roof meets its edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoofForm {
    /// Every edge slopes.
    Hip,
    /// The long sides slope, the ends are vertical gables.
    Gable,
    /// One plane rising from the longest edge.
    Shed,
    /// A barrel vault: a profile (arc, segment or half ellipse) drawn along the long axis;
    /// where the rules refuse the vault the roof stays a gable.
    Barrel,
}

field!(
    /// Largest extent of a footprint, m.
    SPAN = "massing.span"
);
field!(TOLERANCE = "massing.arc_tolerance");
field!(PLOT_SETBACK = "site.plot_setback");
field!(ROTATION = "massing.rotation");
field!(ARM_ANGLE = "massing.arm_angle");
field!(SIDES = "massing.sides");
field!(TWIST = "massing.twist");
field!(TAPER = "massing.taper");
field!(LEVEL_SETBACK = "massing.level_setback");
field!(LEVEL_OFFSET = "massing.level_offset");
/// Vertices of a ring after the arcs are cut into chords.
pub const MAX_VERTICES: usize = 400;
pub const MAX_HOLES: usize = 8;
pub const MAX_LEVELS: usize = 16;

pub const SHAPES: [&str; 10] = [
    "triangle",
    "trapezoid",
    "circle",
    "ellipse",
    "crescent",
    "l",
    "u",
    "courtyard",
    "regular",
    "rectangle",
];

/// Keys of `footprint` the plate reads besides `width` and `depth`.
pub const KEYS: [&str; 13] = [
    "polygon",
    "holes",
    "shape",
    "plot",
    "plot_setback",
    "rotation",
    "tolerance",
    "angle",
    "sides",
    "top",
    "thickness",
    "arm",
    "wings",
];

/// Arms of a branched plan.
pub const MAX_WINGS: usize = 12;
field!(WING_WIDTH = "massing.wing_width");

/// Whether a footprint asks for the plate pipeline at all.
pub fn asks_plate(fp: &J) -> bool {
    ["polygon", "shape", "plot", "wings"].iter().any(|k| !fp[*k].is_null())
}

/// `[{from: [x, y], to: [x, y], width?}, ...]`: the arms of a branched plan, joined into one
/// outline and the courts they close.
fn wings(rd: &mut Rd<'_>, v: &J) -> Option<(BulgeRing, Vec<BulgeRing>)> {
    let Some(list) = v.as_array().filter(|a| !a.is_empty() && a.len() <= MAX_WINGS) else {
        rd.bad("footprint.wings", format!("must be a list of 1..{MAX_WINGS} {{from: [x, y], to: [x, y], width?}}"));
        return None;
    };
    let pt = |x: &J| {
        x.as_array()
            .filter(|a| a.len() == 2)
            .and_then(|a| Some((a[0].as_f64()?, a[1].as_f64()?)))
            .filter(|p| p.0.is_finite() && p.1.is_finite())
    };
    let n0 = rd.issues.len();
    let mut arms = vec![];
    for (i, w) in list.iter().enumerate() {
        let path = format!("footprint.wings[{i}]");
        match w.as_object() {
            Some(o) => keys(rd, &path, o, &["from", "to", "width"]),
            None => {
                rd.bad(&path, "must be {from: [x, y], to: [x, y], width?}");
                continue;
            }
        }
        let (Some(a), Some(b)) = (pt(&w["from"]), pt(&w["to"])) else {
            rd.bad(&path, "needs from and to, each [x, y] in metres");
            continue;
        };
        let width = WING_WIDTH.default.unwrap_or(WING_WIDTH.min);
        let width = rd.num(&format!("{path}.width"), &w["width"], &WING_WIDTH, width);
        arms.push(super::anygeo::shape::Arm { a, b, w: width });
    }
    if rd.issues.len() > n0 {
        return None;
    }
    match super::anygeo::shape::union_arms(&arms) {
        Ok((outer, holes)) => {
            let ring = |r: Vec<P>| BulgeRing::from_polygon(r.into_iter().map(|p| v2(p.0, p.1)).collect());
            Some((ring(outer), holes.into_iter().map(ring).collect()))
        }
        Err(e) => {
            rd.bad("footprint.wings", e);
            None
        }
    }
}

struct Rd<'a> {
    issues: &'a mut Vec<(String, String)>,
}

impl Rd<'_> {
    fn bad(&mut self, path: &str, msg: impl Into<String>) {
        self.issues.push((path.to_string(), msg.into()));
    }

    fn num(&mut self, path: &str, v: &J, p: &ParamDef, def: f64) -> f64 {
        match v {
            J::Null => def,
            _ => match v.as_f64() {
                Some(x) if p.contains(x) => x,
                _ => {
                    self.bad(path, format!("must be a number in {}", p.range()));
                    def
                }
            },
        }
    }

    /// `[[x, y], [x, y, bulge], ...]`: points and the bulge of the edge to the next point.
    fn ring(&mut self, path: &str, v: &J) -> Option<BulgeRing> {
        let Some(a) = v.as_array() else {
            self.bad(path, "must be a list of [x, y] or [x, y, bulge] in metres");
            return None;
        };
        if a.len() < 3 {
            self.bad(path, "needs at least 3 points");
            return None;
        }
        let (mut pts, mut bulges) = (vec![], vec![]);
        for (i, q) in a.iter().enumerate() {
            let xs: Option<Vec<f64>> = q
                .as_array()
                .filter(|x| x.len() == 2 || x.len() == 3)
                .and_then(|x| x.iter().map(J::as_f64).collect());
            match xs {
                Some(x) if x.iter().all(|c| c.is_finite() && c.abs() < 1e5) => {
                    let b = x.get(2).copied().unwrap_or(0.0);
                    if b.abs() > 1.0 + 1e-9 {
                        self.bad(&format!("{path}[{i}]"), "bulge must be in -1..1 (a half circle at most)");
                        return None;
                    }
                    pts.push(v2(x[0], x[1]));
                    bulges.push(b);
                }
                _ => {
                    self.bad(&format!("{path}[{i}]"), "must be [x, y] or [x, y, bulge], metres");
                    return None;
                }
            }
        }
        Some(BulgeRing { pts, bulges })
    }
}

/// Reads the plate of a brief; `None` when the footprint is a plain bar or on a problem
/// (the problem is in `issues` with its path).
pub fn read(v: &J, floors: u32, issues: &mut Vec<(String, String)>) -> Option<Plate> {
    let fp = &v["footprint"];
    if !asks_plate(fp) {
        return None;
    }
    let mut rd = Rd { issues };
    let n0 = rd.issues.len();
    let tol = rd.num(TOLERANCE.path, &fp["tolerance"], &TOLERANCE, 0.05);
    let given = ["polygon", "shape", "plot", "wings"].iter().filter(|k| !fp[**k].is_null()).count();
    if given > 1 {
        rd.bad("footprint", "give one of polygon, shape, plot or wings");
        return None;
    }
    let (outer, mut holes, source) =if !fp["polygon"].is_null() {
        let r = rd.ring("footprint.polygon", &fp["polygon"])?;
        (r, vec![], "polygon".to_string())
    } else if !fp["wings"].is_null() {
        let (r, hs) = wings(&mut rd, &fp["wings"])?;
        (r, hs, "wings".to_string())
    } else if !fp["plot"].is_null() {
        let r = rd.ring("footprint.plot", &fp["plot"])?;
        (r, vec![], "plot".to_string())
    } else {
        let name = fp["shape"].as_str().unwrap_or("");
        let Some(t) = template(&mut rd, name, fp) else {
            if !SHAPES.contains(&name) {
                let all = SHAPES.join(", ");
                rd.bad("footprint.shape", format!("unknown '{name}', one of {all}"));
            }
            return None;
        };
        (t.0, t.1, name.to_string())
    };
    if let Some(hs) = fp["holes"].as_array() {
        if hs.len() > MAX_HOLES {
            rd.bad("footprint.holes", format!("at most {MAX_HOLES} courtyards"));
        }
        for (i, h) in hs.iter().enumerate().take(MAX_HOLES) {
            if let Some(r) = rd.ring(&format!("footprint.holes[{i}]"), h) {
                holes.push(r);
            }
        }
    } else if !fp["holes"].is_null() {
        rd.bad("footprint.holes", "must be a list of rings");
    }
    let rot = rd.num(ROTATION.path, &fp["rotation"], &ROTATION, 0.0);
    if rd.issues.len() > n0 {
        return None;
    }
    let mut region = match tessellate(&outer, &holes, tol) {
        Ok(r) => r,
        Err(e) => {
            rd.bad(if source == "plot" { "footprint.plot" } else { "footprint.polygon" }, e);
            return None;
        }
    };
    if source == "plot" {
        let s = rd.num(PLOT_SETBACK.path, &fp["plot_setback"], &PLOT_SETBACK, 0.0);
        if s > 0.0 {
            match largest(offset::inset(&region, s)) {
                Some(r) => region = r,
                None => {
                    rd.bad("footprint.plot_setback", format!("{s} m leaves nothing of the plot"));
                    return None;
                }
            }
        }
    } else if !fp["plot_setback"].is_null() {
        rd.bad("footprint.plot_setback", "only with footprint.plot");
    }
    let pivot = centroid(&region.outer);
    if rot != 0.0 {
        region = turn(&region, rot, pivot);
    }
    let (lo, hi) = bbox(&region.outer);
    let span = (hi.0 - lo.0).max(hi.1 - lo.1);
    if !SPAN.contains(span) {
        rd.bad("footprint", format!("the footprint spans {span:.1} m; it must span {} m", SPAN.range()));
    }
    let dv = &v["design"];
    let twist = rd.num(TWIST.path, &dv["twist"], &TWIST, 0.0);
    let taper = rd.num(TAPER.path, &dv["taper"], &TAPER, 0.0);
    let levels = read_levels(&mut rd, &dv["levels"], floors);
    if rd.issues.len() > n0 {
        return None;
    }
    Some(Plate {
        base: region,
        levels,
        twist,
        taper,
        pivot,
        rotation: rot,
        source,
    })
}

fn read_levels(rd: &mut Rd<'_>, v: &J, floors: u32) -> Vec<Level> {
    let Some(a) = v.as_array() else {
        if !v.is_null() {
            rd.bad("design.levels", "must be a list of {from, setback?, offset?, rotation?}");
        }
        return vec![];
    };
    if a.len() > MAX_LEVELS {
        rd.bad("design.levels", format!("at most {MAX_LEVELS} entries"));
    }
    let mut out: Vec<Level> = vec![];
    for (i, e) in a.iter().enumerate().take(MAX_LEVELS) {
        let p = format!("design.levels[{i}]");
        let Some(o) = e.as_object() else {
            rd.bad(&p, "must be {from, setback?, offset?, rotation?}");
            continue;
        };
        keys(rd, &p, o, &["from", "setback", "offset", "rotation"]);
        let from = match e["from"].as_u64() {
            Some(f) if f >= 1 && f < u64::from(floors) => f as u32,
            _ => {
                rd.bad(&format!("{p}.from"), format!("required, a storey in 1..{} (0 is the ground)", floors.max(2) - 1));
                continue;
            }
        };
        let setback = rd.num(&format!("{p}.setback"), &e["setback"], &LEVEL_SETBACK, 0.0);
        let offset = match &e["offset"] {
            J::Null => (0.0, 0.0),
            x => {
                let xy = x.as_array().filter(|a| a.len() == 2).and_then(|a| Some((a[0].as_f64()?, a[1].as_f64()?)));
                match xy {
                    Some((dx, dy)) if LEVEL_OFFSET.contains(dx) && LEVEL_OFFSET.contains(dy) => (dx, dy),
                    _ => {
                        rd.bad(&format!("{p}.offset"), format!("must be [dx, dy], each in {} m", LEVEL_OFFSET.range()));
                        (0.0, 0.0)
                    }
                }
            }
        };
        let rotation = rd.num(&format!("{p}.rotation"), &e["rotation"], &ROTATION, 0.0);
        if out.iter().any(|l| l.from == from) {
            rd.bad(&format!("{p}.from"), "two entries start on the same storey");
        }
        out.push(Level {
            from,
            setback,
            offset,
            rotation,
        });
    }
    out.sort_by_key(|l| l.from);
    out
}

fn keys(rd: &mut Rd<'_>, path: &str, o: &Map<String, J>, known: &[&str]) {
    for k in o.keys() {
        if !known.contains(&k.as_str()) {
            rd.bad(&format!("{path}.{k}"), format!("unknown field; known: {}", known.join(", ")));
        }
    }
}

/// A template outline with its bulges, and courtyards, from the width and depth.
fn template(rd: &mut Rd<'_>, name: &str, fp: &J) -> Option<(BulgeRing, Vec<BulgeRing>)> {
    let wdef = |n: &str| match n {
        "circle" | "regular" => 30.0,
        "crescent" => 60.0,
        "courtyard" => 48.0,
        _ => 40.0,
    };
    let w = rd.num("footprint.width", &fp["width"], &SPAN, wdef(name));
    let d = rd.num("footprint.depth", &fp["depth"], &SPAN, if name == "crescent" { 24.0 } else { w.min(30.0) });
    let poly = |p: Vec<P>| BulgeRing::from_polygon(p.into_iter().map(|q| v2(q.0, q.1)).collect());
    let thick_def: f64 = 14.0;
    let ring = match name {
        "rectangle" => poly(vec![(0.0, 0.0), (w, 0.0), (w, d), (0.0, d)]),
        "triangle" => poly(vec![(0.0, 0.0), (w, 0.0), (w / 2.0, d)]),
        "trapezoid" => {
            let top = rd.num("footprint.top", &fp["top"], &SPAN, (w / 2.0).max(6.0));
            if top >= w {
                rd.bad("footprint.top", "the top side must be shorter than the width");
                return None;
            }
            poly(vec![(0.0, 0.0), (w, 0.0), ((w + top) / 2.0, d), ((w - top) / 2.0, d)])
        }
        "circle" => BulgeRing {
            pts: vec![v2(0.0, w / 2.0), v2(w, w / 2.0)],
            bulges: vec![1.0, 1.0],
        },
        "ellipse" => {
            let n = 64;
            poly(
                (0..n)
                    .map(|i| {
                        let a = std::f64::consts::TAU * i as f64 / n as f64;
                        (w / 2.0 * (1.0 + a.cos()), d / 2.0 * (1.0 + a.sin()))
                    })
                    .collect(),
            )
        }
        "regular" => {
            let n = rd.num(SIDES.path, &fp["sides"], &SIDES, 6.0).round() as usize;
            poly(
                (0..n)
                    .map(|i| {
                        let a = std::f64::consts::TAU * i as f64 / n as f64 - std::f64::consts::FRAC_PI_2;
                        (w / 2.0 * (1.0 + a.cos()), w / 2.0 * (1.0 + a.sin()))
                    })
                    .collect(),
            )
        }
        "crescent" => {
            // tips on y = 0, the outer arc sags `depth`, the inner one `depth - thickness`
            let t = rd.num("footprint.thickness", &fp["thickness"], &SPAN, thick_def.min(d * 0.6));
            if t >= d {
                rd.bad("footprint.thickness", "must be less than the depth");
                return None;
            }
            BulgeRing {
                pts: vec![v2(0.0, 0.0), v2(w, 0.0)],
                bulges: vec![(2.0 * d / w).min(1.0), -(2.0 * (d - t) / w).min(1.0)],
            }
        }
        "l" | "u" | "courtyard" => {
            let t = rd.num("footprint.arm", &fp["arm"], &SPAN, thick_def.min(w / 3.0).min(d / 2.0));
            if 2.0 * t >= w.min(d) {
                rd.bad("footprint.arm", format!("an arm of {t} m leaves no inside in {w} x {d} m"));
                return None;
            }
            match name {
                "l" => {
                    let a = rd.num(ARM_ANGLE.path, &fp["angle"], &ARM_ANGLE, 90.0).to_radians();
                    let (u2, m2) = ((a.cos(), a.sin()), (a.sin(), -a.cos()));
                    let ci = (t * (1.0 + a.cos()) / a.sin(), t);
                    if ci.0 >= w - 1.0 {
                        rd.bad("footprint.angle", "the arms are too short for this angle");
                        return None;
                    }
                    poly(vec![
                        (0.0, 0.0),
                        (w, 0.0),
                        (w, t),
                        ci,
                        (u2.0 * d + m2.0 * t, u2.1 * d + m2.1 * t),
                        (u2.0 * d, u2.1 * d),
                    ])
                }
                "u" => poly(vec![
                    (0.0, 0.0),
                    (w, 0.0),
                    (w, d),
                    (w - t, d),
                    (w - t, t),
                    (t, t),
                    (t, d),
                    (0.0, d),
                ]),
                _ => {
                    let outer = poly(vec![(0.0, 0.0), (w, 0.0), (w, d), (0.0, d)]);
                    let hole = poly(vec![(t, t), (t, d - t), (w - t, d - t), (w - t, t)]);
                    return Some((outer, vec![hole]));
                }
            }
        }
        _ => return None,
    };
    Some((ring, vec![]))
}

/// Chords within `tol`, rings cleaned and oriented, holes convex and inside.
pub fn tessellate(outer: &BulgeRing, holes: &[BulgeRing], tol: f64) -> Result<Region, String> {
    let to = |r: &BulgeRing| -> Vec<P> { r.tessellate(tol).into_iter().map(|p| (p.x, p.y)).collect() };
    let o = clean(&to(outer), true).ok_or("the outline is degenerate: three points not on a line at least")?;
    simple(&o).map_err(|e| format!("the outline {e}"))?;
    if o.len() > MAX_VERTICES {
        return Err(format!("{} vertices after the arcs; at most {MAX_VERTICES}: a larger tolerance", o.len()));
    }
    let mut hs = vec![];
    for (i, h) in holes.iter().enumerate() {
        let h = clean(&to(h), false).ok_or(format!("courtyard {i} is degenerate"))?;
        simple(&h).map_err(|e| format!("courtyard {i} {e}"))?;
        if !convex(&h) {
            return Err(format!("courtyard {i} must be convex"));
        }
        if !h.iter().all(|p| crate::check::inside(&o, *p)) {
            return Err(format!("courtyard {i} is not inside the outline"));
        }
        hs.push(h);
    }
    for i in 0..hs.len() {
        for j in i + 1..hs.len() {
            if hs[i].iter().any(|p| crate::check::inside(&hs[j], *p)) || hs[j].iter().any(|p| crate::check::inside(&hs[i], *p)) {
                return Err(format!("courtyards {i} and {j} overlap"));
            }
        }
    }
    Ok(Region { outer: o, holes: hs })
}

fn simple(r: &[P]) -> Result<(), &'static str> {
    let v: Vec<_> = r.iter().map(|p| v2(p.0, p.1)).collect();
    ddc_geom::poly::simple_ring(&v, 1e-6).map_err(|_| "crosses itself")
}

pub(crate) fn signed_area(r: &[P]) -> f64 {
    crate::check::ring_area(r)
}

/// Drops repeated points and points on a straight line; counter-clockwise when `ccw`.
pub fn clean(r: &[P], ccw: bool) -> Option<Vec<P>> {
    let mut v: Vec<P> = vec![];
    for &p in r {
        if v.last().is_none_or(|q: &P| (p.0 - q.0).hypot(p.1 - q.1) > 1e-4) {
            v.push(p);
        }
    }
    while v.len() > 1 && (v[0].0 - v[v.len() - 1].0).hypot(v[0].1 - v[v.len() - 1].1) <= 1e-4 {
        v.pop();
    }
    loop {
        let n = v.len();
        if n < 3 {
            return None;
        }
        let k = (0..n).find(|&i| {
            let (a, b, c) = (v[(i + n - 1) % n], v[i], v[(i + 1) % n]);
            let (e0, e1) = ((b.0 - a.0, b.1 - a.1), (c.0 - b.0, c.1 - b.1));
            let cr = e0.0 * e1.1 - e0.1 * e1.0;
            cr.abs() <= 1e-9 * e0.0.hypot(e0.1) * e1.0.hypot(e1.1)
        });
        match k {
            Some(i) => {
                v.remove(i);
            }
            None => break,
        }
    }
    if (signed_area(&v) > 0.0) != ccw {
        v.reverse();
    }
    (signed_area(&v).abs() > 1e-6).then_some(v)
}

pub fn convex(r: &[P]) -> bool {
    let n = r.len();
    let s = signed_area(r).signum();
    (0..n).all(|i| {
        let (a, b, c) = (r[i], r[(i + 1) % n], r[(i + 2) % n]);
        ((b.0 - a.0) * (c.1 - b.1) - (b.1 - a.1) * (c.0 - b.0)) * s >= -1e-9
    })
}

pub fn centroid(r: &[P]) -> P {
    let c = ddc_geom::poly::centroid(&r.iter().map(|p| v2(p.0, p.1)).collect::<Vec<_>>());
    (c.x, c.y)
}

pub fn bbox(r: &[P]) -> (P, P) {
    r.iter().fold(((f64::MAX, f64::MAX), (f64::MIN, f64::MIN)), |(lo, hi), p| {
        ((lo.0.min(p.0), lo.1.min(p.1)), (hi.0.max(p.0), hi.1.max(p.1)))
    })
}

pub fn rot(p: P, deg: f64, c: P) -> P {
    let (s, co) = deg.to_radians().sin_cos();
    let (x, y) = (p.0 - c.0, p.1 - c.1);
    (c.0 + x * co - y * s, c.1 + x * s + y * co)
}

pub fn turn(r: &Region, deg: f64, c: P) -> Region {
    Region {
        outer: r.outer.iter().map(|p| rot(*p, deg, c)).collect(),
        holes: r.holes.iter().map(|h| h.iter().map(|p| rot(*p, deg, c)).collect()).collect(),
    }
}

pub fn shift(r: &Region, o: P) -> Region {
    let m = |p: &P| (p.0 + o.0, p.1 + o.1);
    Region {
        outer: r.outer.iter().map(m).collect(),
        holes: r.holes.iter().map(|h| h.iter().map(m).collect()).collect(),
    }
}

pub fn region_area(r: &Region) -> f64 {
    signed_area(&r.outer).abs() - r.holes.iter().map(|h| signed_area(h).abs()).sum::<f64>()
}

/// The piece with the largest area.
pub fn largest(v: Vec<Region>) -> Option<Region> {
    v.into_iter().max_by(|a, b| region_area(a).total_cmp(&region_area(b)))
}

impl Plate {
    /// The plate of storey `k` (0 = ground).
    pub fn at(&self, k: usize) -> Result<Region, String> {
        let lv = self.levels.iter().rev().find(|l| l.from as usize <= k);
        let mut r = self.base.clone();
        // terrace steps by rule: every `massing.step_every` storeys the plate gives up
        // `massing.step_setback`, all round or from the side `massing.step_dir` faces
        let every = super::rules::rule("massing.step_every").round() as usize;
        let steps = if every > 0 { k / every } else { 0 };
        let step = steps as f64 * super::rules::rule("massing.step_setback");
        let terrace = super::rules::rule("massing.step_mode").round() as usize == 1;
        if steps > 0 && terrace {
            let dir = rot((1.0, 0.0), super::rules::rule("massing.step_dir") + self.rotation, (0.0, 0.0));
            let far = r.outer.iter().map(|p| p.0 * dir.0 + p.1 * dir.1).fold(f64::MIN, f64::max);
            r = super::anygeo::shape::clip_half(&r, dir, far - step)
                .ok_or_else(|| format!("floor {}: terrace steps of {step:.2} m leave nothing", k + 1))?;
        }
        let setback = lv.map_or(0.0, |l| l.setback) + self.taper * k as f64 + if terrace { 0.0 } else { step };
        if setback > 1e-9 {
            r = largest(offset::inset(&r, setback))
                .ok_or_else(|| format!("floor {}: a set-back of {setback:.2} m leaves nothing", k + 1))?;
        } else if setback < -1e-9 {
            r = offset::outset(&r, -setback);
        }
        let deg = lv.map_or(0.0, |l| l.rotation) + self.twist * k as f64;
        if deg.abs() > 1e-12 {
            r = turn(&r, deg, self.pivot);
        }
        if let Some(l) = lv.filter(|l| l.offset != (0.0, 0.0)) {
            r = shift(&r, l.offset);
        }
        let outer = clean(&r.outer, true).ok_or_else(|| format!("floor {}: the plate degenerates", k + 1))?;
        let holes = r.holes.iter().filter_map(|h| clean(h, false)).collect();
        Ok(Region { outer, holes })
    }

    /// Whether every floor is the base plate.
    pub fn uniform(&self) -> bool {
        self.twist == 0.0
            && self.taper == 0.0
            && super::rules::rule("massing.step_every") == 0.0
            && self.levels.iter().all(|l| l.setback == 0.0 && l.offset == (0.0, 0.0) && l.rotation == 0.0)
    }
}

/// Edge slopes of a roof ring by its form; `None` edges are vertical gables.
pub fn edge_slopes(ring: &[P], form: RoofForm, slope: f64) -> Vec<Option<f64>> {
    let n = ring.len();
    let edge = |i: usize| {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        (b.0 - a.0, b.1 - a.1)
    };
    let longest = (0..n).max_by(|&i, &j| {
        let (a, b) = (edge(i), edge(j));
        a.0.hypot(a.1).total_cmp(&b.0.hypot(b.1))
    });
    let Some(k) = longest else { return vec![] };
    match form {
        RoofForm::Hip => vec![Some(slope); n],
        RoofForm::Shed => (0..n).map(|i| (i == k).then_some(slope)).collect(),
        RoofForm::Gable | RoofForm::Barrel => {
            // edges within 30 degrees of the longest one's direction slope, the rest are gables
            let d = edge(k);
            let l = d.0.hypot(d.1);
            (0..n)
                .map(|i| {
                    let e = edge(i);
                    let c = (e.0 * d.0 + e.1 * d.1).abs() / (e.0.hypot(e.1) * l);
                    (c > 30f64.to_radians().cos()).then_some(slope)
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plate(fp: J) -> Plate {
        let mut is = vec![];
        let p = read(&json!({ "footprint": fp }), 4, &mut is);
        assert!(is.is_empty(), "{is:?}");
        p.unwrap()
    }

    #[test]
    fn templates_compile_to_clean_rings() {
        for s in SHAPES {
            let p = plate(json!({ "shape": s }));
            assert!(p.base.outer.len() >= 3, "{s}");
            assert!(signed_area(&p.base.outer) > 50.0, "{s}");
            assert!(simple(&p.base.outer).is_ok(), "{s}");
            for h in &p.base.holes {
                assert!(signed_area(h) < 0.0 && convex(h), "{s}");
            }
        }
    }

    #[test]
    fn arcs_keep_the_tolerance_and_the_area() {
        let p = plate(json!({ "shape": "circle", "width": 40.0, "tolerance": 0.05 }));
        let r = 20.0;
        let c = centroid(&p.base.outer);
        for w in p.base.outer.windows(2) {
            let m = ((w[0].0 + w[1].0) / 2.0, (w[0].1 + w[1].1) / 2.0);
            let sag = r - (m.0 - c.0).hypot(m.1 - c.1);
            assert!(sag <= 0.05 + 1e-9 && sag > 0.0, "sag {sag}");
        }
        let a = signed_area(&p.base.outer);
        assert!((a - std::f64::consts::PI * r * r).abs() / a < 0.005, "area {a}");
    }

    #[test]
    fn plot_setback_and_floor_variation() {
        let mut is = vec![];
        let v = json!({
            "footprint": { "plot": [[0, 0], [50, 0], [40, 30], [0, 30]], "plot_setback": 3 },
            "design": { "twist": 2, "levels": [{ "from": 2, "setback": 2 }] }
        });
        let p = read(&v, 4, &mut is).unwrap();
        assert!(is.is_empty(), "{is:?}");
        let (lo, _) = bbox(&p.base.outer);
        assert!((lo.0 - 3.0).abs() < 1e-6 && (lo.1 - 3.0).abs() < 1e-6, "{lo:?}");
        let a0 = region_area(&p.at(0).unwrap());
        let a1 = region_area(&p.at(1).unwrap());
        let a2 = region_area(&p.at(2).unwrap());
        assert!((a0 - a1).abs() < 1e-6, "a twist keeps the area");
        assert!(a2 < a1 - 50.0, "the set-back floor is smaller");
        assert!(!p.uniform());
    }

    #[test]
    fn bad_input_names_the_path() {
        let mut is = vec![];
        let v = json!({ "footprint": { "polygon": [[0, 0], [10, 10], [10, 0], [0, 10]] } });
        assert!(read(&v, 2, &mut is).is_none());
        assert_eq!(is[0].0, "footprint.polygon", "{is:?}");
        is.clear();
        let v = json!({ "footprint": { "shape": "blob" } });
        assert!(read(&v, 2, &mut is).is_none());
        assert!(is[0].1.contains("triangle"), "{is:?}");
    }

    #[test]
    fn roof_forms_pick_edges() {
        let r = vec![(0.0, 0.0), (30.0, 0.0), (30.0, 10.0), (0.0, 10.0)];
        assert_eq!(edge_slopes(&r, RoofForm::Hip, 30.0), vec![Some(30.0); 4]);
        assert_eq!(edge_slopes(&r, RoofForm::Gable, 30.0), vec![Some(30.0), None, Some(30.0), None]);
        assert_eq!(edge_slopes(&r, RoofForm::Shed, 30.0).iter().filter(|s| s.is_some()).count(), 1);
    }
}
