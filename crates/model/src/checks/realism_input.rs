//! Fills the realism critic's `Input` from the model: solids (members, plates, bolts), rebar
//! sets, stairs and what carries them. A solid says what it is in its material text:
//! `S355;bent=cold`, `S355;ecc`, `S355;plate`, `S355;bolt=M20;hole=22` (mm). Tags after the
//! first `;` never change the material.

use ddc_geom::arc::BulgeRing;
use ddc_geom::gsolid::{Form, Frame, Path3, Section};
use ddc_geom::{v3, V2, V3};

use super::realism::{
    near_pairs, BendMethod, Bolt, Input, Member, Part, Plate, Rebar, Shape, Stair, AXIS_TOL,
};
use crate::{Error, Model};

/// Sag of sampled arcs, m: the axis stays within 0.1 mm of the true curve.
const SAG: f64 = 1e-4;
const MAX_ARC_PTS: usize = 2000;
/// A joint turning the axis by more than this is a sharp bend, rad.
const KINK: f64 = 0.5 * std::f64::consts::PI / 180.0;
/// An extrusion shorter than this many section depths is a block, not a member.
const MEMBER_ASPECT: f64 = 3.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tags {
    pub bend: Option<BendMethod>,
    pub ecc: bool,
    pub plate: bool,
    pub bolt: Option<Option<f64>>,
    pub hole: Option<f64>,
}

/// `(material, tags)` of a solid's material text; unknown tags are ignored.
pub fn tags_of(material: &str) -> (String, Tags) {
    let mut it = material.split(';');
    let base = it.next().unwrap_or_default().trim().to_string();
    let mut t = Tags::default();
    for raw in it {
        let (k, v) = match raw.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (raw.trim(), ""),
        };
        match k {
            "bent" => t.bend = BendMethod::parse(v),
            "ecc" => t.ecc = true,
            "plate" => t.plate = true,
            "bolt" => {
                let d = v.trim_start_matches(['M', 'm']).parse::<f64>().ok();
                t.bolt = Some(d.map(|d| d / 1000.0));
            }
            "hole" => t.hole = v.parse::<f64>().ok().map(|d| d / 1000.0),
            _ => {}
        }
    }
    (base, t)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionInfo {
    pub shape: Shape,
    pub depth: f64,
    pub min_side: f64,
    /// Mean wall thickness of a thin-walled section: twice the area over the perimeter.
    pub thin: f64,
    pub centroid: V2,
}

pub fn section_info(s: &Section) -> Option<SectionInfo> {
    let pts = s.outer.tessellate(1e-3);
    if pts.len() < 3 {
        return None;
    }
    let (mut lo, mut hi) = (V2 { x: f64::MAX, y: f64::MAX }, V2 { x: f64::MIN, y: f64::MIN });
    for p in &pts {
        lo = V2 { x: lo.x.min(p.x), y: lo.y.min(p.y) };
        hi = V2 { x: hi.x.max(p.x), y: hi.y.max(p.y) };
    }
    let (w, h) = (hi.x - lo.x, hi.y - lo.y);
    let (mut a2, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for i in 0..pts.len() {
        let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
        let c = p.x * q.y - q.x * p.y;
        a2 += c;
        cx += (p.x + q.x) * c;
        cy += (p.y + q.y) * c;
    }
    let centroid = if a2.abs() > 1e-18 {
        V2 { x: cx / (3.0 * a2), y: cy / (3.0 * a2) }
    } else {
        V2 { x: (lo.x + hi.x) / 2.0, y: (lo.y + hi.y) / 2.0 }
    };
    let holes: f64 = s.holes.iter().map(|r: &BulgeRing| r.area().abs()).sum();
    let area = s.outer.area().abs() - holes;
    let per = s.outer.perimeter() + s.holes.iter().map(|r| r.perimeter()).sum::<f64>();
    let thin = if per > 1e-12 { 2.0 * area / per } else { 0.0 };
    let min_side = w.min(h);
    let shape = if !s.holes.is_empty() {
        Shape::Hollow
    } else if min_side > 0.0 && thin < 0.35 * min_side {
        Shape::Open
    } else {
        Shape::Bar
    };
    Some(SectionInfo { shape, depth: w.max(h), min_side, thin, centroid })
}

fn world(f: &Frame, z0: f64, p: V3) -> [f64; 3] {
    let w = f.at(p);
    [w.x, w.y, w.z + z0]
}

/// Points of the arc of one path segment (plan arc, linear z), the chord ends included.
fn seg_points(p: V3, q: V3, b: f64) -> (Vec<V3>, Option<f64>) {
    if b.abs() < 1e-12 {
        return (vec![p, q], None);
    }
    let c = ((q.x - p.x).powi(2) + (q.y - p.y).powi(2)).sqrt();
    if c < 1e-12 {
        return (vec![p, q], None);
    }
    let r = c * (1.0 + b * b) / (4.0 * b.abs());
    let theta = 4.0 * b.atan();
    let step = 2.0 * (1.0 - (SAG / r).min(0.5)).acos();
    let n = ((theta.abs() / step).ceil() as usize).clamp(2, MAX_ARC_PTS);
    let phi = (q.y - p.y).atan2(q.x - p.x);
    let a0 = phi - theta / 2.0;
    // the centre lies left of the travel for a counter-clockwise arc, right for a clockwise one
    let side = if b > 0.0 { 1.0 } else { -1.0 };
    let centre = V2 {
        x: p.x + side * r * (-(a0.sin())),
        y: p.y + side * r * a0.cos(),
    };
    let start = (p.y - centre.y).atan2(p.x - centre.x);
    let pts = (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            let a = start + theta * t;
            v3(centre.x + r * a.cos(), centre.y + r * a.sin(), p.z + (q.z - p.z) * t)
        })
        .collect();
    (pts, Some(r))
}

/// Unit tangents at the start and end of a path segment.
fn seg_tangents(p: V3, q: V3, b: f64) -> (V3, V3) {
    let c = ((q.x - p.x).powi(2) + (q.y - p.y).powi(2)).sqrt();
    let dz = q.z - p.z;
    if b.abs() < 1e-12 || c < 1e-12 {
        let d = q - p;
        let u = d * (1.0 / d.len().max(1e-300));
        return (u, u);
    }
    let theta = 4.0 * b.atan();
    let r = c * (1.0 + b * b) / (4.0 * b.abs());
    let phi = (q.y - p.y).atan2(q.x - p.x);
    let slope = dz / (r * theta.abs());
    let at = |a: f64| {
        let u = v3(a.cos(), a.sin(), slope);
        u * (1.0 / u.len())
    };
    (at(phi - theta / 2.0), at(phi + theta / 2.0))
}

/// Sampled axis of a path and its smallest radius of curvature (0 at a sharp joint).
fn path_axis(path: &Path3) -> (Vec<V3>, Option<f64>) {
    let mut axis: Vec<V3> = vec![];
    let mut radius: Option<f64> = None;
    let mut prev_end: Option<V3> = None;
    let mut take = |r: f64| radius = Some(radius.map_or(r, |x: f64| x.min(r)));
    for (i, w) in path.pts.windows(2).enumerate() {
        let b = path.bulges.get(i).copied().unwrap_or(0.0);
        let (pts, r) = seg_points(w[0], w[1], b);
        if let Some(r) = r {
            take(r);
        }
        let (t0, t1) = seg_tangents(w[0], w[1], b);
        if let Some(e) = prev_end {
            if e.dot(t0).clamp(-1.0, 1.0).acos() > KINK {
                take(0.0);
            }
        }
        prev_end = Some(t1);
        if axis.is_empty() {
            axis.extend(pts);
        } else {
            axis.extend(pts.into_iter().skip(1));
        }
    }
    (axis, radius)
}

fn rodrigues(p: V3, o: V3, k: V3, a: f64) -> V3 {
    let v = p - o;
    o + v * a.cos() + k.cross(v) * a.sin() + k * (k.dot(v) * (1.0 - a.cos()))
}

/// Axis of a revolved section: the centroid turned about the revolve axis.
fn revolve_axis(si: &SectionInfo, o: V2, d: V2, a0: f64, a1: f64) -> Option<(Vec<V3>, f64, bool)> {
    let l = d.len();
    if l < 1e-12 {
        return None;
    }
    let k = v3(d.x / l, d.y / l, 0.0);
    let o3 = v3(o.x, o.y, 0.0);
    let c = v3(si.centroid.x, si.centroid.y, 0.0);
    let r = (c - o3).cross(k).len();
    if r <= 0.5 * si.depth {
        return None;
    }
    let sweep = a1 - a0;
    let closed = sweep >= 2.0 * std::f64::consts::PI - 1e-6;
    let step = 2.0 * (1.0 - (SAG / r).min(0.5)).acos();
    let n = ((sweep.abs() / step).ceil() as usize).clamp(8, MAX_ARC_PTS);
    let pts = (0..=n).map(|i| rodrigues(c, o3, k, a0 + sweep * i as f64 / n as f64)).collect();
    Some((pts, r, closed))
}

struct Built {
    part: Part,
    member: Option<Member>,
    info: Option<SectionInfo>,
    /// Axis start and direction of a straight extrusion, for bolts.
    ext: Option<([f64; 3], [f64; 3])>,
}

fn build(key: (String, i64), form: &Form, frame: &Frame, z0: f64, t: &Tags, part: Part) -> Built {
    let mut member = None;
    let mut info = None;
    let mut ext = None;
    let mk = |si: &SectionInfo, axis: Vec<[f64; 3]>, closed: bool, radius: Option<f64>| Member {
        key: key.clone(),
        shape: si.shape,
        depth: si.depth,
        axis,
        closed,
        radius,
        bend: t.bend,
        ecc: t.ecc,
    };
    match form {
        Form::Extrusion { profile, start, end } => {
            info = section_info(profile);
            if let Some(si) = info {
                let c = si.centroid;
                let (a, b) = (v3(c.x, c.y, *start), v3(c.x, c.y, *end));
                let (wa, wb) = (world(frame, z0, a), world(frame, z0, b));
                ext = Some((wa, [wb[0] - wa[0], wb[1] - wa[1], wb[2] - wa[2]]));
                if (end - start).abs() >= MEMBER_ASPECT * si.depth {
                    member = Some(mk(&si, vec![wa, wb], false, None));
                }
            }
        }
        Form::Sweep { profile, path } => {
            info = section_info(profile);
            if let Some(si) = info {
                let (axis, radius) = path_axis(path);
                let w = axis.iter().map(|p| world(frame, z0, *p)).collect();
                member = Some(mk(&si, w, false, radius));
            }
        }
        Form::Revolve { profile, axis_o, axis_d, start, end } => {
            info = section_info(profile);
            if let Some(si) = info {
                if let Some((axis, r, closed)) = revolve_axis(&si, *axis_o, *axis_d, *start, *end) {
                    let w = axis.iter().map(|p| world(frame, z0, *p)).collect();
                    member = Some(mk(&si, w, closed, Some(r)));
                }
            }
        }
        _ => {}
    }
    Built { part, member, info, ext }
}

fn length_of(form: &Form) -> f64 {
    match form {
        Form::Extrusion { start, end, .. } => (end - start).abs(),
        Form::Sweep { path, .. } => path.pts.windows(2).map(|w| (w[1] - w[0]).len()).sum(),
        _ => f64::MAX,
    }
}

fn level_z(m: &Model, level: i64) -> f64 {
    m.db.query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| r.get(0)).unwrap_or(0.0)
}

fn bbox_dims(b: &[f64; 6]) -> [f64; 3] {
    [b[3] - b[0], b[4] - b[1], b[5] - b[2]]
}

/// The model as the critic reads it.
pub(super) fn collect(m: &Model) -> Result<Input, Error> {
    let mut inp = Input::default();
    inp.ground_z = m
        .db
        .query_row("SELECT MIN(elevation) FROM level", [], |r| r.get::<_, Option<f64>>(0))
        .ok()
        .flatten();
    solids(m, &mut inp)?;
    rebar(m, &mut inp);
    stairs(m, &mut inp);
    if let Ok((ids, bodies)) = m.structure_bodies() {
        for (_, b) in ids.iter().zip(&bodies) {
            let (lo, hi) = (b.aabb.min, b.aabb.max);
            inp.supports.push([lo.x, lo.y, lo.z, hi.x, hi.y, hi.z]);
        }
    }
    Ok(inp)
}

fn solids(m: &Model, inp: &mut Input) -> Result<(), Error> {
    let mut built: Vec<(Built, Tags)> = vec![];
    for (id, s) in m.solids(None)? {
        if s.void {
            continue;
        }
        let Some(q) = m.solid_quantities(id)? else { continue };
        let (_, tags) = tags_of(&s.material);
        let key = ("solid".to_string(), id);
        let mesh = crate::solids::mesh_of(&m.db, &s).ok();
        let z0 = level_z(m, s.level);
        let dims = bbox_dims(&q.bbox);
        let min_ext = dims.iter().copied().fold(f64::MAX, f64::min);
        let part = Part { key: key.clone(), bbox: q.bbox, min_dim: min_ext, mesh };
        let mut b = build(key, &s.form, &s.frame, z0, &tags, part);
        if let Some(si) = b.info {
            let own = if si.shape == Shape::Bar { si.min_side } else { si.thin };
            b.part.min_dim = own.min(length_of(&s.form)).max(0.0);
        }
        built.push((b, tags));
    }
    joints(&built, inp);
    for (b, _) in built {
        inp.parts.push(b.part);
        inp.members.extend(b.member);
    }
    Ok(())
}

/// Plates with what they join, bolts with edge distance and pitch.
fn joints(built: &[(Built, Tags)], inp: &mut Input) {
    let boxes: Vec<[f64; 6]> = built.iter().map(|b| b.0.part.bbox).collect();
    let thick = |b: &Built| b.info.map_or(b.part.min_dim, |i| i.thin.max(1e-6));
    let plate_ix: Vec<usize> = (0..built.len()).filter(|&i| built[i].1.plate).collect();
    let bolt_ix: Vec<usize> = (0..built.len()).filter(|&i| built[i].1.bolt.is_some()).collect();
    if plate_ix.is_empty() && bolt_ix.is_empty() {
        return;
    }
    let pairs = near_pairs(&boxes, AXIS_TOL);
    for &p in &plate_ix {
        let t = built[p].0.part.min_dim;
        let joined: Vec<_> = pairs
            .iter()
            .filter_map(|&(a, b)| match (a == p, b == p) {
                (true, _) => Some(b),
                (_, true) => Some(a),
                _ => None,
            })
            .filter(|&o| !built[o].1.plate && built[o].1.bolt.is_none())
            .map(|o| (built[o].0.part.key.clone(), thick(&built[o].0)))
            .collect();
        inp.plates.push(Plate { key: built[p].0.part.key.clone(), thick: t, joined });
    }
    let centre = |i: usize| {
        let b = &boxes[i];
        [(b[0] + b[3]) / 2.0, (b[1] + b[4]) / 2.0, (b[2] + b[5]) / 2.0]
    };
    for &i in &bolt_ix {
        let (b, t) = (&built[i].0, &built[i].1);
        let d = t.bolt.flatten().or(b.info.map(|s| s.depth)).unwrap_or(0.0);
        let c = centre(i);
        let host = plate_ix.iter().copied().find(|&p| {
            (0..3).all(|k| c[k] >= boxes[p][k] - 1e-9 && c[k] <= boxes[p][k + 3] + 1e-9)
        });
        let (mut edge, mut pitch) = (None, None);
        if let Some(p) = host {
            let dims = bbox_dims(&boxes[p]);
            let thin_k = (0..3).min_by(|&a, &b| dims[a].total_cmp(&dims[b])).unwrap_or(2);
            edge = (0..3)
                .filter(|&k| k != thin_k)
                .map(|k| (c[k] - boxes[p][k]).min(boxes[p][k + 3] - c[k]))
                .reduce(f64::min);
            pitch = bolt_ix
                .iter()
                .filter(|&&o| o != i && boxes_touch(&boxes[p], &centre(o)))
                .map(|&o| {
                    let q = centre(o);
                    ((c[0] - q[0]).powi(2) + (c[1] - q[1]).powi(2) + (c[2] - q[2]).powi(2)).sqrt()
                })
                .reduce(f64::min);
        }
        inp.bolts.push(Bolt { key: b.part.key.clone(), d, hole: t.hole, edge, pitch });
    }
}

fn boxes_touch(b: &[f64; 6], p: &[f64; 3]) -> bool {
    (0..3).all(|k| p[k] >= b[k] - 1e-9 && p[k] <= b[k + 3] + 1e-9)
}

fn rebar(m: &Model, inp: &mut Input) {
    use ddc_geom::rebar::PathLayout;
    let types: std::collections::HashMap<i64, f64> = match m.rebar_types() {
        Ok(v) => v.into_iter().map(|t| (t.id, t.d)).collect(),
        Err(_) => return,
    };
    for kind in ["column", "beam", "wall", "slab", "footing", "raft"] {
        let Ok(sets) = m.rebar_sets_of(kind, None) else { continue };
        for (id, s) in sets {
            let cover = m
                .rebar_covers(kind, s.host)
                .ok()
                .and_then(|c| c.into_iter().map(|x| x.1).reduce(f64::min));
            let spacing = match &s.layout {
                crate::RebarLayout::Stirrup { rule, .. } => match *rule {
                    PathLayout::MaxSpacing(x) | PathLayout::Spacing(x) => Some((x, x)),
                    PathLayout::Zones { end_s, mid_s, .. } => {
                        Some((end_s.min(mid_s), end_s.max(mid_s)))
                    }
                    PathLayout::Fixed(_) => None,
                },
                _ => None,
            };
            let d = types.get(&s.rtype).copied().unwrap_or(0.0);
            inp.rebar.push(Rebar { key: ("rebar_set".to_string(), id), d, cover, spacing });
        }
    }
}

fn stairs(m: &Model, inp: &mut Input) {
    let Ok(mut st) = m.db.prepare("SELECT id FROM stair ORDER BY id") else { return };
    let ids: Vec<i64> = st
        .query_map([], |r| r.get(0))
        .map(|r| r.flatten().collect())
        .unwrap_or_default();
    for id in ids {
        if let Ok(s) = m.stair_layout(id) {
            let key = ("stair".to_string(), id);
            inp.stairs.push(Stair { key, riser: s.riser, tread: s.tread });
        }
    }
}
