// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Stable names for faces, edges and axes (`wall:55/face:left`, `slab:9/edge:3`,
//! `solid:4/face:2.5`, `wall:5/axis`, `grid:A`) and their resolution to geometry.
//!
//! A name depends only on the element it hangs on: wall faces on the wall axis and thickness,
//! slab edges on the outline order, solid faces on their face tags. Edits of other elements
//! never change what a name points to. A name that no longer resolves gives an `Unresolved`
//! with a stable `code()` and a readable message, never a panic.
//! The model side implements `PartSource`; `ShapeMap` is the in-memory one (tests, scratch).

use std::collections::BTreeMap;
use std::fmt;

use crate::geo::End;
use crate::refs::{Cat, Ref};

/// Bits for the face number inside one history operation of a solid face tag (the split
/// the geometry crate uses): `face:<op>.<local>` is tag `op << 12 | local`.
pub const FACE_LOCAL_BITS: u32 = 12;
const FACE_LOCAL_MAX: u32 = 1 << FACE_LOCAL_BITS;

/// A face by role, or a solid face by its tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FaceName {
    /// Wall side to the left of the axis direction a -> b.
    Left,
    Right,
    Top,
    Bottom,
    /// Wall end face at axis end a.
    Start,
    /// Wall end face at axis end b.
    End,
    /// Solid face tag.
    Tag(u32),
}

impl FaceName {
    pub fn tag(op: u32, local: u32) -> FaceName {
        FaceName::Tag((op << FACE_LOCAL_BITS) | (local & (FACE_LOCAL_MAX - 1)))
    }
}

/// One step of a part chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PartName {
    Face(FaceName),
    /// Outline edge `i` -> `i + 1` (0-based, wrapping), at the top of a slab.
    Edge(u32),
    Axis,
    /// End point of an axis, edge or line.
    End(End),
    /// Middle of an axis or line.
    Mid,
    /// Centre of the element; for lines and walls the middle of the axis.
    Center,
}

impl PartName {
    /// Parse one part token (`face:left`, `face:2.5`, `edge:3`, `axis`, `end:a`).
    pub fn parse(s: &str) -> Result<PartName, &'static str> {
        match s {
            "axis" => return Ok(PartName::Axis),
            "mid" => return Ok(PartName::Mid),
            "center" => return Ok(PartName::Center),
            _ => {}
        }
        let (kind, val) = s.split_once(':').ok_or("unknown part")?;
        match kind {
            "face" => Ok(PartName::Face(match val {
                "left" => FaceName::Left,
                "right" => FaceName::Right,
                "top" => FaceName::Top,
                "bottom" => FaceName::Bottom,
                "start" => FaceName::Start,
                "end" => FaceName::End,
                _ => {
                    let (op, local) = val.split_once('.').ok_or("unknown face name")?;
                    let op = digits(op).ok_or("bad face tag")?;
                    let local = digits(local).ok_or("bad face tag")?;
                    if op == 0 || op >= (1 << (32 - FACE_LOCAL_BITS)) || local >= FACE_LOCAL_MAX {
                        return Err("face tag out of range");
                    }
                    FaceName::tag(op, local)
                }
            })),
            "edge" => Ok(PartName::Edge(digits(val).ok_or("bad edge number")?)),
            "end" => match val {
                "a" => Ok(PartName::End(End::A)),
                "b" => Ok(PartName::End(End::B)),
                _ => Err("end must be a or b"),
            },
            _ => Err("unknown part"),
        }
    }
}

impl fmt::Display for PartName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PartName::Face(face) => match face {
                FaceName::Left => f.write_str("face:left"),
                FaceName::Right => f.write_str("face:right"),
                FaceName::Top => f.write_str("face:top"),
                FaceName::Bottom => f.write_str("face:bottom"),
                FaceName::Start => f.write_str("face:start"),
                FaceName::End => f.write_str("face:end"),
                FaceName::Tag(t) => {
                    write!(f, "face:{}.{}", t >> FACE_LOCAL_BITS, t & (FACE_LOCAL_MAX - 1))
                }
            },
            PartName::Edge(i) => write!(f, "edge:{i}"),
            PartName::Axis => f.write_str("axis"),
            PartName::End(End::A) => f.write_str("end:a"),
            PartName::End(End::B) => f.write_str("end:b"),
            PartName::Mid => f.write_str("mid"),
            PartName::Center => f.write_str("center"),
        }
    }
}

/// Plain ASCII digits only, so the printed form is the one that parses.
fn digits(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0'))
    {
        return None;
    }
    s.parse().ok()
}

/// Point in metres, project coordinates, z up.
pub type Pt = [f64; 3];

/// What a ref resolves to.
#[derive(Clone, Debug, PartialEq)]
pub enum PartGeo {
    Point(Pt),
    Polyline(Vec<Pt>),
    /// Planar face: outward unit normal; loops counter-clockwise seen from outside.
    Plane { origin: Pt, normal: Pt, loops: Vec<Vec<Pt>> },
}

/// Element geometry as the resolver needs it (the model lowers its rows to this).
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// Straight wall: axis a -> b at the base elevation (`a[2]`), centred thickness.
    Wall { a: Pt, b: Pt, thickness: f64, height: f64 },
    /// Plan outline (either orientation) with the top elevation and the thickness below it.
    Slab { outline: Vec<[f64; 2]>, top: f64, thickness: f64 },
    /// Grid, beam, brace, column axis, reference plane trace.
    Line { a: Pt, b: Pt },
    /// Closed solid as tagged planar polygons; one tag may have several polygons.
    Solid { faces: Vec<TagFace> },
    /// Exists but has nothing to resolve against (annotation, type row).
    Empty,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TagFace {
    pub tag: u32,
    pub pts: Vec<Pt>,
}

impl Shape {
    fn kind(&self) -> &'static str {
        match self {
            Shape::Wall { .. } => "wall",
            Shape::Slab { .. } => "slab",
            Shape::Line { .. } => "line",
            Shape::Solid { .. } => "solid",
            Shape::Empty => "element without geometry",
        }
    }
}

/// The model as seen by the resolver.
pub trait PartSource {
    /// `None` when the element does not exist (deleted or never created).
    fn shape(&self, cat: Cat, id: i64) -> Option<Shape>;
    /// Id of the element with this unique name in the category.
    fn id_by_name(&self, cat: Cat, name: &str) -> Option<i64>;
    /// Element created by an envelope slot, once that op has run.
    fn slot(&self, _slot: u32) -> Option<(Cat, i64)> {
        None
    }
}

/// Why a ref does not resolve. `r` is the label of the ref at the failing step.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum Unresolved {
    #[error("{r} no longer exists in the model")]
    Gone { r: String },
    #[error("{r}: nothing in the model has this name")]
    NoName { r: String },
    #[error("{r}: the op creating this slot has not run")]
    NoSlot { r: String },
    #[error("{r}: a selection set has no single geometry")]
    NotSingle { r: String },
    #[error("{r}: `{part}` is not a part name ({why})")]
    BadPart { r: String, part: String, why: &'static str },
    #[error("{r}: `{part}` does not apply to a {kind}")]
    NotOnKind { r: String, part: String, kind: &'static str },
    #[error("{r}: edge {index} does not exist, the outline has {count} edges")]
    NoSuchEdge { r: String, index: u32, count: usize },
    #[error("{r}: `{part}` is gone, an edit of this element removed it")]
    NoSuchFace { r: String, part: String },
    #[error("{r}: `{part}` matches several faces ({}); name one of them", .tags.join(", "))]
    Ambiguous { r: String, part: String, tags: Vec<String> },
    #[error("{r}: name a face, edge or axis of it")]
    NeedsPart { r: String },
    #[error("{r}: geometry is degenerate")]
    Degenerate { r: String },
    #[error("{r}: the element has no geometry")]
    NoGeometry { r: String },
}

impl Unresolved {
    /// Stable code for rejections.
    pub fn code(&self) -> &'static str {
        match self {
            Unresolved::Gone { .. } => "ref_gone",
            Unresolved::NoName { .. } => "ref_no_name",
            Unresolved::NoSlot { .. } => "ref_no_slot",
            Unresolved::NotSingle { .. } => "ref_not_single",
            Unresolved::BadPart { .. } => "ref_bad_part",
            Unresolved::NotOnKind { .. } => "ref_part_not_on_kind",
            Unresolved::NoSuchEdge { .. } => "ref_no_edge",
            Unresolved::NoSuchFace { .. } => "ref_no_face",
            Unresolved::Ambiguous { .. } => "ref_ambiguous",
            Unresolved::NeedsPart { .. } => "ref_needs_part",
            Unresolved::Degenerate { .. } => "ref_degenerate",
            Unresolved::NoGeometry { .. } => "ref_no_geometry",
        }
    }
}

/// Resolve a ref to geometry. A bare line-like element (`grid:A`) is its axis.
pub fn resolve(src: &dyn PartSource, r: &Ref) -> Result<PartGeo, Unresolved> {
    let label = r.label();
    match r {
        Ref::Part { of, part } => {
            let name = PartName::parse(part).map_err(|why| Unresolved::BadPart {
                r: label.clone(),
                part: part.clone(),
                why,
            })?;
            if let Ref::Part { .. } = **of {
                // Only an end point can hang on a part (`slab:9/edge:3/end:a`).
                let inner = resolve(src, of)?;
                return match (name, inner) {
                    (PartName::End(e), PartGeo::Polyline(pts)) => Ok(PartGeo::Point(end_of(&pts, e))),
                    _ => Err(Unresolved::NotOnKind { r: label, part: part.clone(), kind: "part" }),
                };
            }
            let shape = root_shape(src, of)?;
            part_of(&shape, name, &label, part)
        }
        _ => match root_shape(src, r)? {
            Shape::Line { a, b } => line(a, b, &label),
            Shape::Empty => Err(Unresolved::NoGeometry { r: label }),
            _ => Err(Unresolved::NeedsPart { r: label }),
        },
    }
}

fn root_shape(src: &dyn PartSource, r: &Ref) -> Result<Shape, Unresolved> {
    let label = r.label();
    let (cat, id) = match r {
        Ref::Id { cat, id } => (*cat, *id),
        Ref::Name { cat, name } => {
            (*cat, src.id_by_name(*cat, name).ok_or(Unresolved::NoName { r: label.clone() })?)
        }
        Ref::New { slot } => src.slot(*slot).ok_or(Unresolved::NoSlot { r: label.clone() })?,
        Ref::Set { .. } => return Err(Unresolved::NotSingle { r: label }),
        Ref::Part { .. } => return Err(Unresolved::NeedsPart { r: label }),
    };
    src.shape(cat, id).ok_or(Unresolved::Gone { r: label })
}

fn end_of(pts: &[Pt], e: End) -> Pt {
    match e {
        End::A => pts[0],
        End::B => pts[pts.len() - 1],
    }
}

fn line(a: Pt, b: Pt, label: &str) -> Result<PartGeo, Unresolved> {
    if dist(a, b) < EPS {
        return Err(Unresolved::Degenerate { r: label.to_owned() });
    }
    Ok(PartGeo::Polyline(vec![a, b]))
}

fn part_of(shape: &Shape, name: PartName, label: &str, part: &str) -> Result<PartGeo, Unresolved> {
    let not_on = || Unresolved::NotOnKind { r: label.to_owned(), part: part.to_owned(), kind: shape.kind() };
    let degenerate = || Unresolved::Degenerate { r: label.to_owned() };
    match shape {
        Shape::Empty => Err(Unresolved::NoGeometry { r: label.to_owned() }),
        Shape::Line { a, b } => match name {
            PartName::Axis => line(*a, *b, label),
            PartName::End(e) => {
                line(*a, *b, label)?;
                Ok(PartGeo::Point(if e == End::A { *a } else { *b }))
            }
            PartName::Mid | PartName::Center => {
                line(*a, *b, label)?;
                Ok(PartGeo::Point(scale(add(*a, *b), 0.5)))
            }
            _ => Err(not_on()),
        },
        Shape::Wall { a, b, thickness, height } => {
            let len = dist([a[0], a[1], 0.0], [b[0], b[1], 0.0]);
            if len < EPS || *thickness <= 0.0 || *height <= 0.0 {
                return Err(degenerate());
            }
            let base = a[2];
            let a = [a[0], a[1], base];
            let b = [b[0], b[1], base];
            let u = scale(sub(b, a), 1.0 / len);
            let nl = [-u[1], u[0], 0.0];
            let h = thickness / 2.0;
            let up = [0.0, 0.0, *height];
            let (al, bl) = (add(a, scale(nl, h)), add(b, scale(nl, h)));
            let (ar, br) = (add(a, scale(nl, -h)), add(b, scale(nl, -h)));
            let face = |origin: Pt, normal: Pt, pts: Vec<Pt>| PartGeo::Plane { origin, normal, loops: vec![pts] };
            match name {
                PartName::Axis => Ok(PartGeo::Polyline(vec![a, b])),
                PartName::Mid | PartName::Center => Ok(PartGeo::Point(scale(add(a, b), 0.5))),
                PartName::End(End::A) => Ok(PartGeo::Point(a)),
                PartName::End(End::B) => Ok(PartGeo::Point(b)),
                PartName::Face(FaceName::Left) => {
                    Ok(face(al, nl, vec![al, add(al, up), add(bl, up), bl]))
                }
                PartName::Face(FaceName::Right) => {
                    Ok(face(ar, scale(nl, -1.0), vec![ar, br, add(br, up), add(ar, up)]))
                }
                PartName::Face(FaceName::Top) => {
                    let t = |p: Pt| add(p, up);
                    Ok(face(t(a), [0.0, 0.0, 1.0], vec![t(ar), t(br), t(bl), t(al)]))
                }
                PartName::Face(FaceName::Bottom) => Ok(face(a, [0.0, 0.0, -1.0], vec![al, bl, br, ar])),
                PartName::Face(FaceName::Start) => {
                    Ok(face(a, scale(u, -1.0), vec![al, ar, add(ar, up), add(al, up)]))
                }
                PartName::Face(FaceName::End) => Ok(face(b, u, vec![br, bl, add(bl, up), add(br, up)])),
                PartName::Face(FaceName::Tag(_)) | PartName::Edge(_) => Err(not_on()),
            }
        }
        Shape::Slab { outline, top, thickness } => {
            let n = outline.len();
            if n < 3 || *thickness <= 0.0 || signed_area(outline).abs() < EPS * EPS {
                return Err(degenerate());
            }
            let at = |p: [f64; 2], z: f64| [p[0], p[1], z];
            match name {
                PartName::Edge(i) => {
                    let i = i as usize;
                    if i >= n {
                        return Err(Unresolved::NoSuchEdge { r: label.to_owned(), index: i as u32, count: n });
                    }
                    Ok(PartGeo::Polyline(vec![at(outline[i], *top), at(outline[(i + 1) % n], *top)]))
                }
                PartName::Face(FaceName::Top) | PartName::Face(FaceName::Bottom) => {
                    let is_top = name == PartName::Face(FaceName::Top);
                    let z = if is_top { *top } else { top - thickness };
                    let mut pts: Vec<Pt> = outline.iter().map(|p| at(*p, z)).collect();
                    // Counter-clockwise from above for the top, from below for the bottom.
                    if (signed_area(outline) > 0.0) != is_top {
                        pts.reverse();
                    }
                    let normal = [0.0, 0.0, if is_top { 1.0 } else { -1.0 }];
                    Ok(PartGeo::Plane { origin: pts[0], normal, loops: vec![pts] })
                }
                _ => Err(not_on()),
            }
        }
        Shape::Solid { faces } => {
            let mut by_tag: BTreeMap<u32, Vec<&TagFace>> = BTreeMap::new();
            for f in faces {
                by_tag.entry(f.tag).or_default().push(f);
            }
            let tag = match name {
                PartName::Face(FaceName::Tag(t)) => {
                    if !by_tag.contains_key(&t) {
                        return Err(Unresolved::NoSuchFace { r: label.to_owned(), part: part.to_owned() });
                    }
                    t
                }
                PartName::Face(FaceName::Top) | PartName::Face(FaceName::Bottom) => {
                    let sign = if name == PartName::Face(FaceName::Top) { 1.0 } else { -1.0 };
                    flat_face(&by_tag, sign, label, part)?
                }
                _ => return Err(not_on()),
            };
            let polys = &by_tag[&tag];
            let normal = newell(&polys[0].pts).ok_or_else(degenerate)?;
            let loops: Vec<Vec<Pt>> = polys.iter().map(|f| f.pts.clone()).collect();
            Ok(PartGeo::Plane { origin: loops[0][0], normal, loops })
        }
    }
}

/// The highest (`sign` 1) or lowest (`sign` -1) tag whose polygons all face straight up or down.
fn flat_face(
    by_tag: &BTreeMap<u32, Vec<&TagFace>>,
    sign: f64,
    label: &str,
    part: &str,
) -> Result<u32, Unresolved> {
    let mut best: Vec<(u32, f64)> = Vec::new();
    for (tag, polys) in by_tag {
        let flat = polys.iter().all(|f| newell(&f.pts).is_some_and(|n| n[2] * sign > 1.0 - 1e-6));
        if !flat {
            continue;
        }
        let z = polys[0].pts.iter().map(|p| p[2]).sum::<f64>() / polys[0].pts.len() as f64;
        match best.first() {
            Some(&(_, bz)) if (z - bz) * sign < -EPS => {}
            Some(&(_, bz)) if (z - bz).abs() <= EPS => best.push((*tag, z)),
            _ => best = vec![(*tag, z)],
        }
    }
    match best.as_slice() {
        [] => Err(Unresolved::NoSuchFace { r: label.to_owned(), part: part.to_owned() }),
        [(t, _)] => Ok(*t),
        many => Err(Unresolved::Ambiguous {
            r: label.to_owned(),
            part: part.to_owned(),
            tags: many.iter().map(|(t, _)| PartName::Face(FaceName::Tag(*t)).to_string()).collect(),
        }),
    }
}

const EPS: f64 = 1e-9;

fn sub(a: Pt, b: Pt) -> Pt {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: Pt, b: Pt) -> Pt {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: Pt, k: f64) -> Pt {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dist(a: Pt, b: Pt) -> f64 {
    let d = sub(a, b);
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

fn signed_area(pts: &[[f64; 2]]) -> f64 {
    let n = pts.len();
    (0..n).map(|i| {
        let (p, q) = (pts[i], pts[(i + 1) % n]);
        p[0] * q[1] - q[0] * p[1]
    })
    .sum::<f64>()
        / 2.0
}

/// Unit normal of a planar polygon (counter-clockwise gives the right-hand normal).
fn newell(pts: &[Pt]) -> Option<Pt> {
    if pts.len() < 3 {
        return None;
    }
    let mut n = [0.0; 3];
    for i in 0..pts.len() {
        let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
        n[0] += (p[1] - q[1]) * (p[2] + q[2]);
        n[1] += (p[2] - q[2]) * (p[0] + q[0]);
        n[2] += (p[0] - q[0]) * (p[1] + q[1]);
    }
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len < EPS {
        return None;
    }
    Some(scale(n, 1.0 / len))
}

/// In-memory `PartSource`: tests, and a scratch copy that lowers its rows once.
#[derive(Clone, Debug, Default)]
pub struct ShapeMap {
    pub shapes: BTreeMap<(Cat, i64), Shape>,
    pub names: BTreeMap<(Cat, String), i64>,
    pub slots: BTreeMap<u32, (Cat, i64)>,
}

impl ShapeMap {
    pub fn put(&mut self, cat: Cat, id: i64, shape: Shape) -> &mut ShapeMap {
        self.shapes.insert((cat, id), shape);
        self
    }
    pub fn name(&mut self, cat: Cat, name: &str, id: i64) -> &mut ShapeMap {
        self.names.insert((cat, name.to_owned()), id);
        self
    }
    /// Removes the element; its name is left dangling on purpose, as a stale index would.
    pub fn delete(&mut self, cat: Cat, id: i64) -> Option<Shape> {
        self.shapes.remove(&(cat, id))
    }
}

impl PartSource for ShapeMap {
    fn shape(&self, cat: Cat, id: i64) -> Option<Shape> {
        self.shapes.get(&(cat, id)).cloned()
    }
    fn id_by_name(&self, cat: Cat, name: &str) -> Option<i64> {
        self.names.get(&(cat, name.to_owned())).copied()
    }
    fn slot(&self, slot: u32) -> Option<(Cat, i64)> {
        self.slots.get(&slot).copied()
    }
}
