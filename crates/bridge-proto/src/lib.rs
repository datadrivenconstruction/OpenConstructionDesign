// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Bridge protocol v1 (docs/plugins/BRIDGE_PROTOCOL.md): frames a bridge process writes to stdout.
//! Neutral building data only — levels, types, walls, openings, slabs, rooms, meshes, properties.
//! Encoding: little-endian, `str` = u32 len + UTF-8, `vec` = u32 count + items, `opt` = u8 0/1 + item.
//! Fields are only ever appended within a major version; readers ignore trailing payload bytes.

use std::io::{self, Read, Write};

pub const PROTOCOL: u16 = 1;
/// Minor version the host understands, passed to the bridge as `--minor`. A bridge sends the
/// frames of a newer minor (curtain walls, slab slopes: 2; per-line mullions: 3; columns, beams
/// and MEP runs: 4) only when the host asked for it and falls back to meshes otherwise.
/// 5: the host reads the provenance frames (tags 24–27); a bridge may send them to any host.
/// 6: bodies measured as one extrusion (tag 28), meshes for older hosts.
pub const MINOR: u16 = 6;
pub const MAGIC: [u8; 4] = *b"DDCB";
/// Largest frame payload (tag + body) a reader accepts by default.
pub const MAX_FRAME: u32 = 16 << 20;
pub const MAX_STR: usize = 64 << 10;
/// Host → bridge: one line on stdin. Closing stdin means the same.
pub const CANCEL_LINE: &[u8] = b"cancel\n";

pub type Guid = String;
pub type P2 = (f64, f64);

#[derive(Clone, Debug, PartialEq)]
pub struct Hello {
    pub protocol: u16,
    pub bridge: String,
    pub version: String,
    /// Extensions without dot, lower case.
    pub formats: Vec<String>,
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    pub done: u64,
    /// 0 = unknown.
    pub total: u64,
    pub phase: String,
}

/// Multiply every length in later frames by `length_to_m` to get metres. Angles are radians.
#[derive(Clone, Debug, PartialEq)]
pub struct Units {
    pub length_to_m: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Level {
    pub guid: Guid,
    pub name: String,
    pub elevation: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub guid: Guid,
    pub name: String,
    pub a: P2,
    pub b: P2,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    pub guid: Guid,
    pub name: String,
    /// 0xRRGGBBAA.
    pub color: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    /// Material guid.
    pub material: Guid,
    pub thickness: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Real(f64),
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    /// Layers from the left face of the axis direction to the right.
    Wall {
        guid: Guid,
        name: String,
        layers: Vec<Layer>,
    },
    /// Layers top to bottom.
    Slab {
        guid: Guid,
        name: String,
        layers: Vec<Layer>,
    },
    /// Door/window/other placeable type; `category` is a neutral word ("door", "window", ...).
    Family {
        guid: Guid,
        category: String,
        name: String,
        params: Vec<(String, Value)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Curve {
    Line {
        a: P2,
        b: P2,
    },
    /// Circular arc through `a`, `mid`, `b`.
    Arc {
        a: P2,
        mid: P2,
        b: P2,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Top {
    Unconnected { height: f64 },
    Level { level: Guid, offset: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpeningKind {
    Door,
    Window,
    Void,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Element {
    Wall {
        guid: Guid,
        level: Guid,
        wall_type: Guid,
        axis: Curve,
        base_offset: f64,
        top: Top,
        /// Left offset of the first layer from the axis; None = centred.
        offset0: Option<f64>,
    },
    /// `host` must appear in an earlier Elements frame (or the same one, earlier).
    Opening {
        guid: Guid,
        host: Guid,
        kind: OpeningKind,
        family_type: Option<Guid>,
        /// Distance along the host axis from its start to the near jamb (arc length for arcs).
        along: f64,
        width: f64,
        height: f64,
        sill: f64,
        /// false: leaf opens to the left of the axis; true: right.
        swing_side: bool,
        /// false: hinges at the jamb nearer the axis start.
        swing_hinge: bool,
    },
    Slab {
        guid: Guid,
        level: Guid,
        slab_type: Guid,
        /// Top relative to the level.
        offset: f64,
        outer: Vec<P2>,
        holes: Vec<Vec<P2>>,
    },
    Room {
        guid: Guid,
        level: Guid,
        name: String,
        number: String,
        height: f64,
        boundary: Vec<P2>,
        point: Option<P2>,
    },
    /// Anything else: triangle mesh in world coordinates (lengths scaled by Units).
    Generic {
        guid: Guid,
        level: Option<Guid>,
        category: String,
        name: String,
        positions: Vec<[f64; 3]>,
        indices: Vec<u32>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Property {
    pub owner: Guid,
    pub key: String,
    pub value: Value,
    /// "" = unitless.
    pub unit: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Warning {
    pub code: String,
    pub message: String,
    pub element: Option<Guid>,
}

/// Top edge of a straight wall seen from its left side: (distance along the axis from its start,
/// height above the wall base) in order of distance; equal distances make a step. Beyond the
/// first and last point the edge stays level. A wall without one has a flat top.
#[derive(Clone, Debug, PartialEq)]
pub struct WallTop {
    pub wall: Guid,
    pub points: Vec<P2>,
}

/// Top plane of a slab: `offset` of the Slab element is the top at `point`; the top rises by
/// `gradient.0` per length along x and `gradient.1` along y. Thickness stays vertical.
#[derive(Clone, Debug, PartialEq)]
pub struct SlabSlope {
    pub slab: Guid,
    pub point: P2,
    pub gradient: P2,
}

/// Cross-section of a curtain mullion: width in the wall plane, depth along its normal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MullionSection {
    None,
    Rect { width: f64, depth: f64 },
    Circle { diameter: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKind {
    Glazed,
    Solid,
    Door,
    Window,
    Empty,
}

/// Panel of cell `i` (along the axis from its start) × `j` (from the base up).
#[derive(Clone, Debug, PartialEq)]
pub struct CurtainPanel {
    pub i: u32,
    pub j: u32,
    pub kind: PanelKind,
    pub name: String,
    pub family_type: Option<Guid>,
    pub swing_side: bool,
    pub swing_hinge: bool,
}

/// Curtain wall: grid lines `u` (interior, distance along the axis) and `v` (interior, height
/// above the base), mullions on every line (border ones lie inside the outline), one panel per
/// cell; cells without a listed panel are glazed.
#[derive(Clone, Debug, PartialEq)]
pub struct CurtainWall {
    pub guid: Guid,
    pub level: Guid,
    pub name: String,
    pub axis: Curve,
    pub base_offset: f64,
    pub height: f64,
    pub u: Vec<f64>,
    pub v: Vec<f64>,
    pub interior: MullionSection,
    pub border: MullionSection,
    pub panels: Vec<CurtainPanel>,
}

/// Curtain wall details beyond CurtainWall. `u` / `v`: grid lines whose mullion differs from
/// `interior` / `border` — (node, segment, section); node 0 is the start (base), node
/// `u.len() + 1` the end (top); segment -1 is the whole line, k ≥ 0 row k of a vertical line or
/// column k of a horizontal one. `top`: top edge (distance along the axis, height above the
/// base), ascending; empty for a straight top.
#[derive(Clone, Debug, PartialEq)]
pub struct CurtainLines {
    pub wall: Guid,
    pub u: Vec<(u32, i32, MullionSection)>,
    pub v: Vec<(u32, i32, MullionSection)>,
    pub top: Vec<P2>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberKind {
    Column,
    Beam,
}

/// Column or beam: straight centre line of the section `a` → `b` in world coordinates (a column
/// runs bottom → top). `rotation`: section width axis from world +x (columns) or from the
/// horizontal normal left of a → b (beams), radians.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub guid: Guid,
    pub level: Guid,
    pub kind: MemberKind,
    pub name: String,
    pub a: [f64; 3],
    pub b: [f64; 3],
    /// Rect: width across, depth along the second section axis.
    pub section: MullionSection,
    pub rotation: f64,
    pub structural: bool,
    /// Measured section outline (y, z) about the centre line in the section frame (after
    /// `rotation`); empty = `section`. Used when the body is no rectangle or circle.
    pub profile: Vec<P2>,
}

/// Ceiling: bottom `height` above the level, plan outline and holes, layers of `slab_type`.
#[derive(Clone, Debug, PartialEq)]
pub struct Ceiling {
    pub guid: Guid,
    pub level: Guid,
    pub slab_type: Guid,
    pub height: f64,
    pub outer: Vec<P2>,
    pub holes: Vec<Vec<P2>>,
}

/// Body that is one extrusion: `outline` in the plane of `x`, `y` about the world point `o`,
/// extruded along `x × y` over `depth`. `x` and `y` are orthonormal; lengths in stream units.
/// `material` is empty when the source names none.
#[derive(Clone, Debug, PartialEq)]
pub struct Prism {
    pub guid: Guid,
    pub level: Guid,
    pub name: String,
    pub category: String,
    pub material: String,
    pub o: [f64; 3],
    pub x: [f64; 3],
    pub y: [f64; 3],
    pub outline: Vec<P2>,
    pub depth: f64,
}

/// Straight MEP segment: `domain` "duct" | "pipe" | "conduit" | "cable_tray"; centre line in world
/// coordinates; Rect: width horizontal, depth vertical.
#[derive(Clone, Debug, PartialEq)]
pub struct MepRun {
    pub guid: Guid,
    pub level: Guid,
    pub domain: String,
    pub name: String,
    pub section: MullionSection,
    pub points: Vec<[f64; 3]>,
}

/// Parameter definition in the source document (tag 25): group, type or instance, shared flag.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ParamDef {
    /// Same key as in Properties (type parameters copied into an instance as `type.<key>`).
    pub key: String,
    pub group: String,
    pub type_param: bool,
    /// None: the reader cannot tell shared from project parameters.
    pub shared: Option<bool>,
    pub shared_guid: String,
    /// Data type as the source names it (length, area, text, yes/no, …).
    pub spec: String,
}

/// Element's id in the source's IFC exports (tag 26): exports keep it as GlobalId.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceId {
    pub guid: Guid,
    pub ifc_guid: String,
}

/// View of the source document (tag 27): names are kept; `kind` plan|ceiling|section|elevation|3d|sheet|schedule|other.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub guid: Guid,
    pub name: String,
    pub kind: String,
    pub level: Option<Guid>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    /// Total elements sent in Elements frames; the host checks it.
    pub elements: u64,
    pub warnings: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    Hello(Hello),
    Progress(Progress),
    Units(Units),
    Levels(Vec<Level>),
    Grids(Vec<Grid>),
    Materials(Vec<Material>),
    Types(Vec<Type>),
    Elements(Vec<Element>),
    Properties(Vec<Property>),
    Warnings(Vec<Warning>),
    End(Stats),
    /// Source document facts (format version, project name, …): any time before End. Tags 24–27
    /// carry provenance only; a bridge may always send them, older hosts skip unknown tags.
    DocInfo(Vec<(String, String)>),
    /// Before Properties: the host reads a unitless number of a length/area/volume parameter
    /// as source units and converts it (Units frame scale). Sent after, it only annotates.
    ParamDefs(Vec<ParamDef>),
    /// After the Elements frames holding these guids.
    SourceIds(Vec<SourceId>),
    /// After Levels.
    Views(Vec<View>),
    /// Before the Elements frames holding these walls (minor addition: older hosts skip it).
    WallTops(Vec<WallTop>),
    /// Minor 2, before the Elements frames holding these slabs.
    SlabSlopes(Vec<SlabSlope>),
    /// Minor 2, after Levels and Types.
    CurtainWalls(Vec<CurtainWall>),
    /// Minor 3, before the CurtainWalls frames holding these walls.
    CurtainLines(Vec<CurtainLines>),
    /// Minor 4, after Levels.
    Members(Vec<Member>),
    /// Minor 4, after Levels.
    MepRuns(Vec<MepRun>),
    /// Minor 4, after Types.
    Ceilings(Vec<Ceiling>),
    /// Minor 6, after Levels.
    Prisms(Vec<Prism>),
    /// Tag from a newer minor version: skipped by the host.
    Unknown(u8),
}

#[derive(Debug)]
pub enum ProtoError {
    Io(io::Error),
    BadMagic,
    Version(u16),
    Oversize(u32),
    Truncated,
    Malformed(String),
}

impl std::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o: {e}"),
            Self::BadMagic => write!(f, "not a bridge stream (bad magic)"),
            Self::Version(v) => write!(f, "unsupported protocol version {v}"),
            Self::Oversize(n) => write!(f, "frame of {n} bytes exceeds limit"),
            Self::Truncated => write!(f, "stream truncated"),
            Self::Malformed(m) => write!(f, "malformed frame: {m}"),
        }
    }
}

impl std::error::Error for ProtoError {}

fn bad<T>(m: impl Into<String>) -> Result<T, ProtoError> {
    Err(ProtoError::Malformed(m.into()))
}

// ---------- encoding ----------

#[derive(Default)]
struct Enc(Vec<u8>);

impl Enc {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn bool(&mut self, v: bool) {
        self.u8(v as u8);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn len(&mut self, n: usize) {
        self.u32(u32::try_from(n).unwrap_or(u32::MAX));
    }
    fn str(&mut self, s: &str) {
        self.len(s.len());
        self.0.extend_from_slice(s.as_bytes());
    }
    fn strs(&mut self, v: &[String]) {
        self.len(v.len());
        v.iter().for_each(|s| self.str(s));
    }
    fn p2(&mut self, p: P2) {
        self.f64(p.0);
        self.f64(p.1);
    }
    fn ring(&mut self, r: &[P2]) {
        self.len(r.len());
        r.iter().for_each(|p| self.p2(*p));
    }
    fn curve(&mut self, c: &Curve) {
        match c {
            Curve::Line { a, b } => {
                self.u8(0);
                self.p2(*a);
                self.p2(*b);
            }
            Curve::Arc { a, mid, b } => {
                self.u8(1);
                self.p2(*a);
                self.p2(*mid);
                self.p2(*b);
            }
        }
    }
    fn reals(&mut self, v: &[f64]) {
        self.len(v.len());
        v.iter().for_each(|x| self.f64(*x));
    }
    fn section(&mut self, m: &MullionSection) {
        match *m {
            MullionSection::None => self.u8(0),
            MullionSection::Rect { width, depth } => {
                self.u8(1);
                self.f64(width);
                self.f64(depth);
            }
            MullionSection::Circle { diameter } => {
                self.u8(2);
                self.f64(diameter);
            }
        }
    }
    fn opt_str(&mut self, s: &Option<String>) {
        match s {
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
            None => self.u8(0),
        }
    }
    fn value(&mut self, v: &Value) {
        match v {
            Value::Bool(b) => {
                self.u8(0);
                self.bool(*b);
            }
            Value::Int(i) => {
                self.u8(1);
                self.0.extend_from_slice(&i.to_le_bytes());
            }
            Value::Real(r) => {
                self.u8(2);
                self.f64(*r);
            }
            Value::Text(t) => {
                self.u8(3);
                self.str(t);
            }
        }
    }
    fn layers(&mut self, ls: &[Layer]) {
        self.len(ls.len());
        for l in ls {
            self.str(&l.material);
            self.f64(l.thickness);
        }
    }
}

/// Encodes one frame body (tag + payload), without the length prefix.
pub fn encode(f: &Frame) -> Vec<u8> {
    let mut e = Enc::default();
    match f {
        Frame::Hello(h) => {
            e.u8(1);
            e.u16(h.protocol);
            e.str(&h.bridge);
            e.str(&h.version);
            e.strs(&h.formats);
            e.strs(&h.capabilities);
        }
        Frame::Progress(p) => {
            e.u8(2);
            e.u64(p.done);
            e.u64(p.total);
            e.str(&p.phase);
        }
        Frame::Units(u) => {
            e.u8(3);
            e.f64(u.length_to_m);
        }
        Frame::Levels(v) => {
            e.u8(4);
            e.len(v.len());
            for l in v {
                e.str(&l.guid);
                e.str(&l.name);
                e.f64(l.elevation);
            }
        }
        Frame::Grids(v) => {
            e.u8(5);
            e.len(v.len());
            for g in v {
                e.str(&g.guid);
                e.str(&g.name);
                e.p2(g.a);
                e.p2(g.b);
            }
        }
        Frame::Materials(v) => {
            e.u8(6);
            e.len(v.len());
            for m in v {
                e.str(&m.guid);
                e.str(&m.name);
                e.u32(m.color);
            }
        }
        Frame::Types(v) => {
            e.u8(7);
            e.len(v.len());
            for t in v {
                match t {
                    Type::Wall { guid, name, layers } | Type::Slab { guid, name, layers } => {
                        e.u8(if matches!(t, Type::Wall { .. }) { 0 } else { 1 });
                        e.str(guid);
                        e.str(name);
                        e.layers(layers);
                    }
                    Type::Family {
                        guid,
                        category,
                        name,
                        params,
                    } => {
                        e.u8(2);
                        e.str(guid);
                        e.str(category);
                        e.str(name);
                        e.len(params.len());
                        for (k, v) in params {
                            e.str(k);
                            e.value(v);
                        }
                    }
                }
            }
        }
        Frame::Elements(v) => {
            e.u8(8);
            e.len(v.len());
            v.iter().for_each(|x| element(&mut e, x));
        }
        Frame::Properties(v) => {
            e.u8(9);
            e.len(v.len());
            for p in v {
                e.str(&p.owner);
                e.str(&p.key);
                e.value(&p.value);
                e.str(&p.unit);
            }
        }
        Frame::Warnings(v) => {
            e.u8(10);
            e.len(v.len());
            for w in v {
                e.str(&w.code);
                e.str(&w.message);
                e.opt_str(&w.element);
            }
        }
        Frame::End(s) => {
            e.u8(11);
            e.u64(s.elements);
            e.u64(s.warnings);
        }
        Frame::DocInfo(v) => {
            e.u8(24);
            e.len(v.len());
            for (k, x) in v {
                e.str(k);
                e.str(x);
            }
        }
        Frame::ParamDefs(v) => {
            e.u8(25);
            e.len(v.len());
            for d in v {
                e.str(&d.key);
                e.str(&d.group);
                e.bool(d.type_param);
                e.u8(match d.shared {
                    Some(false) => 0,
                    Some(true) => 1,
                    None => 2,
                });
                e.str(&d.shared_guid);
                e.str(&d.spec);
            }
        }
        Frame::SourceIds(v) => {
            e.u8(26);
            e.len(v.len());
            for i in v {
                e.str(&i.guid);
                e.str(&i.ifc_guid);
            }
        }
        Frame::Views(v) => {
            e.u8(27);
            e.len(v.len());
            for w in v {
                e.str(&w.guid);
                e.str(&w.name);
                e.str(&w.kind);
                e.opt_str(&w.level);
            }
        }
        Frame::WallTops(v) => {
            e.u8(12);
            e.len(v.len());
            for t in v {
                e.str(&t.wall);
                e.ring(&t.points);
            }
        }
        Frame::SlabSlopes(v) => {
            e.u8(13);
            e.len(v.len());
            for t in v {
                e.str(&t.slab);
                e.p2(t.point);
                e.p2(t.gradient);
            }
        }
        Frame::CurtainWalls(v) => {
            e.u8(14);
            e.len(v.len());
            for c in v {
                e.str(&c.guid);
                e.str(&c.level);
                e.str(&c.name);
                e.curve(&c.axis);
                e.f64(c.base_offset);
                e.f64(c.height);
                e.reals(&c.u);
                e.reals(&c.v);
                e.section(&c.interior);
                e.section(&c.border);
                e.len(c.panels.len());
                for p in &c.panels {
                    e.u32(p.i);
                    e.u32(p.j);
                    e.u8(p.kind as u8);
                    e.str(&p.name);
                    e.opt_str(&p.family_type);
                    e.bool(p.swing_side);
                    e.bool(p.swing_hinge);
                }
            }
        }
        Frame::CurtainLines(v) => {
            e.u8(15);
            e.len(v.len());
            for c in v {
                e.str(&c.wall);
                for lines in [&c.u, &c.v] {
                    e.len(lines.len());
                    for (k, seg, sec) in lines {
                        e.u32(*k);
                        e.u32(*seg as u32);
                        e.section(sec);
                    }
                }
                e.len(c.top.len());
                c.top.iter().for_each(|p| e.p2(*p));
            }
        }
        Frame::Members(v) => {
            e.u8(16);
            e.len(v.len());
            for m in v {
                e.str(&m.guid);
                e.str(&m.level);
                e.u8(m.kind as u8);
                e.str(&m.name);
                m.a.iter().chain(&m.b).for_each(|c| e.f64(*c));
                e.section(&m.section);
                e.f64(m.rotation);
                e.bool(m.structural);
                e.ring(&m.profile);
            }
        }
        Frame::Ceilings(v) => {
            e.u8(18);
            e.len(v.len());
            for c in v {
                e.str(&c.guid);
                e.str(&c.level);
                e.str(&c.slab_type);
                e.f64(c.height);
                e.ring(&c.outer);
                e.len(c.holes.len());
                c.holes.iter().for_each(|h| e.ring(h));
            }
        }
        Frame::Prisms(v) => {
            e.u8(28);
            e.len(v.len());
            for p in v {
                e.str(&p.guid);
                e.str(&p.level);
                e.str(&p.name);
                e.str(&p.category);
                e.str(&p.material);
                p.o.iter().chain(&p.x).chain(&p.y).for_each(|c| e.f64(*c));
                e.ring(&p.outline);
                e.f64(p.depth);
            }
        }
        Frame::MepRuns(v) => {
            e.u8(17);
            e.len(v.len());
            for r in v {
                e.str(&r.guid);
                e.str(&r.level);
                e.str(&r.domain);
                e.str(&r.name);
                e.section(&r.section);
                e.len(r.points.len());
                r.points.iter().flatten().for_each(|c| e.f64(*c));
            }
        }
        Frame::Unknown(t) => e.u8(*t),
    }
    e.0
}

fn element(e: &mut Enc, x: &Element) {
    match x {
        Element::Wall {
            guid,
            level,
            wall_type,
            axis,
            base_offset,
            top,
            offset0,
        } => {
            e.u8(0);
            e.str(guid);
            e.str(level);
            e.str(wall_type);
            match axis {
                Curve::Line { a, b } => {
                    e.u8(0);
                    e.p2(*a);
                    e.p2(*b);
                }
                Curve::Arc { a, mid, b } => {
                    e.u8(1);
                    e.p2(*a);
                    e.p2(*mid);
                    e.p2(*b);
                }
            }
            e.f64(*base_offset);
            match top {
                Top::Unconnected { height } => {
                    e.u8(0);
                    e.f64(*height);
                }
                Top::Level { level, offset } => {
                    e.u8(1);
                    e.str(level);
                    e.f64(*offset);
                }
            }
            match offset0 {
                Some(o) => {
                    e.u8(1);
                    e.f64(*o);
                }
                None => e.u8(0),
            }
        }
        Element::Opening {
            guid,
            host,
            kind,
            family_type,
            along,
            width,
            height,
            sill,
            swing_side,
            swing_hinge,
        } => {
            e.u8(1);
            e.str(guid);
            e.str(host);
            e.u8(*kind as u8);
            e.opt_str(family_type);
            e.f64(*along);
            e.f64(*width);
            e.f64(*height);
            e.f64(*sill);
            e.bool(*swing_side);
            e.bool(*swing_hinge);
        }
        Element::Slab {
            guid,
            level,
            slab_type,
            offset,
            outer,
            holes,
        } => {
            e.u8(2);
            e.str(guid);
            e.str(level);
            e.str(slab_type);
            e.f64(*offset);
            e.ring(outer);
            e.len(holes.len());
            holes.iter().for_each(|h| e.ring(h));
        }
        Element::Room {
            guid,
            level,
            name,
            number,
            height,
            boundary,
            point,
        } => {
            e.u8(3);
            e.str(guid);
            e.str(level);
            e.str(name);
            e.str(number);
            e.f64(*height);
            e.ring(boundary);
            match point {
                Some(p) => {
                    e.u8(1);
                    e.p2(*p);
                }
                None => e.u8(0),
            }
        }
        Element::Generic {
            guid,
            level,
            category,
            name,
            positions,
            indices,
        } => {
            e.u8(4);
            e.str(guid);
            e.opt_str(level);
            e.str(category);
            e.str(name);
            e.len(positions.len());
            for p in positions {
                p.iter().for_each(|c| e.f64(*c));
            }
            e.len(indices.len());
            indices.iter().for_each(|i| e.u32(*i));
        }
    }
}

// ---------- decoding ----------

struct Dec<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Dec<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtoError> {
        if self.b.len() - self.at < n {
            return bad("field runs past frame end");
        }
        let s = &self.b[self.at..self.at + n];
        self.at += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, ProtoError> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Result<bool, ProtoError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            v => bad(format!("bool {v}")),
        }
    }
    fn u16(&mut self) -> Result<u16, ProtoError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, ProtoError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, ProtoError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, ProtoError> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// Non-finite numbers are rejected at the wire level.
    fn f64(&mut self) -> Result<f64, ProtoError> {
        let v = f64::from_le_bytes(self.take(8)?.try_into().unwrap());
        if !v.is_finite() {
            return bad("non-finite number");
        }
        Ok(v)
    }
    /// Count of items each at least `min` bytes: bounded by what is left, so no huge allocations.
    fn count(&mut self, min: usize) -> Result<usize, ProtoError> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min) > self.b.len() - self.at {
            return bad(format!("count {n} exceeds frame"));
        }
        Ok(n)
    }
    fn str(&mut self) -> Result<String, ProtoError> {
        let n = self.u32()? as usize;
        if n > MAX_STR {
            return bad(format!("string of {n} bytes"));
        }
        let s = self.take(n)?;
        String::from_utf8(s.to_vec()).or_else(|_| bad("invalid UTF-8"))
    }
    fn strs(&mut self) -> Result<Vec<String>, ProtoError> {
        let n = self.count(4)?;
        (0..n).map(|_| self.str()).collect()
    }
    fn p2(&mut self) -> Result<P2, ProtoError> {
        Ok((self.f64()?, self.f64()?))
    }
    fn ring(&mut self) -> Result<Vec<P2>, ProtoError> {
        let n = self.count(16)?;
        (0..n).map(|_| self.p2()).collect()
    }
    fn curve(&mut self) -> Result<Curve, ProtoError> {
        Ok(match self.u8()? {
            0 => Curve::Line {
                a: self.p2()?,
                b: self.p2()?,
            },
            1 => Curve::Arc {
                a: self.p2()?,
                mid: self.p2()?,
                b: self.p2()?,
            },
            t => return bad(format!("curve tag {t}")),
        })
    }
    fn reals(&mut self) -> Result<Vec<f64>, ProtoError> {
        let n = self.count(8)?;
        (0..n).map(|_| self.f64()).collect()
    }
    fn section(&mut self) -> Result<MullionSection, ProtoError> {
        Ok(match self.u8()? {
            0 => MullionSection::None,
            1 => MullionSection::Rect {
                width: self.f64()?,
                depth: self.f64()?,
            },
            2 => MullionSection::Circle {
                diameter: self.f64()?,
            },
            t => return bad(format!("mullion tag {t}")),
        })
    }
    fn opt_str(&mut self) -> Result<Option<String>, ProtoError> {
        Ok(if self.bool()? {
            Some(self.str()?)
        } else {
            None
        })
    }
    fn value(&mut self) -> Result<Value, ProtoError> {
        Ok(match self.u8()? {
            0 => Value::Bool(self.bool()?),
            1 => Value::Int(self.i64()?),
            2 => Value::Real(self.f64()?),
            3 => Value::Text(self.str()?),
            t => return bad(format!("value tag {t}")),
        })
    }
    fn layers(&mut self) -> Result<Vec<Layer>, ProtoError> {
        let n = self.count(12)?;
        (0..n)
            .map(|_| {
                Ok(Layer {
                    material: self.str()?,
                    thickness: self.f64()?,
                })
            })
            .collect()
    }
}

/// Decodes one frame body (tag + payload).
pub fn decode(body: &[u8]) -> Result<Frame, ProtoError> {
    let mut d = Dec { b: body, at: 0 };
    let tag = d.u8()?;
    Ok(match tag {
        1 => Frame::Hello(Hello {
            protocol: d.u16()?,
            bridge: d.str()?,
            version: d.str()?,
            formats: d.strs()?,
            capabilities: d.strs()?,
        }),
        2 => Frame::Progress(Progress {
            done: d.u64()?,
            total: d.u64()?,
            phase: d.str()?,
        }),
        3 => Frame::Units(Units {
            length_to_m: d.f64()?,
        }),
        4 => {
            let n = d.count(16)?;
            Frame::Levels(
                (0..n)
                    .map(|_| {
                        Ok(Level {
                            guid: d.str()?,
                            name: d.str()?,
                            elevation: d.f64()?,
                        })
                    })
                    .collect::<Result<_, ProtoError>>()?,
            )
        }
        5 => {
            let n = d.count(36)?;
            Frame::Grids(
                (0..n)
                    .map(|_| {
                        Ok(Grid {
                            guid: d.str()?,
                            name: d.str()?,
                            a: d.p2()?,
                            b: d.p2()?,
                        })
                    })
                    .collect::<Result<_, ProtoError>>()?,
            )
        }
        6 => {
            let n = d.count(12)?;
            Frame::Materials(
                (0..n)
                    .map(|_| {
                        Ok(Material {
                            guid: d.str()?,
                            name: d.str()?,
                            color: d.u32()?,
                        })
                    })
                    .collect::<Result<_, ProtoError>>()?,
            )
        }
        7 => {
            let n = d.count(9)?;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(match d.u8()? {
                    0 => Type::Wall {
                        guid: d.str()?,
                        name: d.str()?,
                        layers: d.layers()?,
                    },
                    1 => Type::Slab {
                        guid: d.str()?,
                        name: d.str()?,
                        layers: d.layers()?,
                    },
                    2 => {
                        let (guid, category, name) = (d.str()?, d.str()?, d.str()?);
                        let k = d.count(6)?;
                        let params = (0..k)
                            .map(|_| Ok((d.str()?, d.value()?)))
                            .collect::<Result<_, ProtoError>>()?;
                        Type::Family {
                            guid,
                            category,
                            name,
                            params,
                        }
                    }
                    t => return bad(format!("type tag {t}")),
                });
            }
            Frame::Types(v)
        }
        8 => {
            let n = d.count(9)?;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(dec_element(&mut d)?);
            }
            Frame::Elements(v)
        }
        9 => {
            let n = d.count(14)?;
            Frame::Properties(
                (0..n)
                    .map(|_| {
                        Ok(Property {
                            owner: d.str()?,
                            key: d.str()?,
                            value: d.value()?,
                            unit: d.str()?,
                        })
                    })
                    .collect::<Result<_, ProtoError>>()?,
            )
        }
        10 => {
            let n = d.count(9)?;
            Frame::Warnings(
                (0..n)
                    .map(|_| {
                        Ok(Warning {
                            code: d.str()?,
                            message: d.str()?,
                            element: d.opt_str()?,
                        })
                    })
                    .collect::<Result<_, ProtoError>>()?,
            )
        }
        11 => Frame::End(Stats {
            elements: d.u64()?,
            warnings: d.u64()?,
        }),
        24 => {
            let n = d.count(8)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                out.push((d.str()?, d.str()?));
            }
            Frame::DocInfo(out)
        }
        25 => {
            let n = d.count(18)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                let (key, group, type_param) = (d.str()?, d.str()?, d.bool()?);
                let shared = match d.u8()? {
                    0 => Some(false),
                    1 => Some(true),
                    2 => None,
                    t => return bad(format!("shared flag {t}")),
                };
                out.push(ParamDef {
                    key,
                    group,
                    type_param,
                    shared,
                    shared_guid: d.str()?,
                    spec: d.str()?,
                });
            }
            Frame::ParamDefs(out)
        }
        26 => {
            let n = d.count(8)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                out.push(SourceId {
                    guid: d.str()?,
                    ifc_guid: d.str()?,
                });
            }
            Frame::SourceIds(out)
        }
        27 => {
            let n = d.count(13)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                out.push(View {
                    guid: d.str()?,
                    name: d.str()?,
                    kind: d.str()?,
                    level: d.opt_str()?,
                });
            }
            Frame::Views(out)
        }
        12 => {
            let n = d.count(8)?;
            Frame::WallTops(
                (0..n)
                    .map(|_| {
                        Ok(WallTop {
                            wall: d.str()?,
                            points: d.ring()?,
                        })
                    })
                    .collect::<Result<_, ProtoError>>()?,
            )
        }
        13 => {
            let n = d.count(36)?;
            Frame::SlabSlopes(
                (0..n)
                    .map(|_| {
                        Ok(SlabSlope {
                            slab: d.str()?,
                            point: d.p2()?,
                            gradient: d.p2()?,
                        })
                    })
                    .collect::<Result<_, ProtoError>>()?,
            )
        }
        14 => {
            let n = d.count(64)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                let (guid, level, name) = (d.str()?, d.str()?, d.str()?);
                let axis = d.curve()?;
                let (base_offset, height) = (d.f64()?, d.f64()?);
                let (u, v) = (d.reals()?, d.reals()?);
                let (interior, border) = (d.section()?, d.section()?);
                let np = d.count(16)?;
                let mut panels = Vec::with_capacity(np);
                for _ in 0..np {
                    let (i, j) = (d.u32()?, d.u32()?);
                    let kind = match d.u8()? {
                        0 => PanelKind::Glazed,
                        1 => PanelKind::Solid,
                        2 => PanelKind::Door,
                        3 => PanelKind::Window,
                        4 => PanelKind::Empty,
                        t => return bad(format!("panel kind {t}")),
                    };
                    panels.push(CurtainPanel {
                        i,
                        j,
                        kind,
                        name: d.str()?,
                        family_type: d.opt_str()?,
                        swing_side: d.bool()?,
                        swing_hinge: d.bool()?,
                    });
                }
                out.push(CurtainWall {
                    guid,
                    level,
                    name,
                    axis,
                    base_offset,
                    height,
                    u,
                    v,
                    interior,
                    border,
                    panels,
                });
            }
            Frame::CurtainWalls(out)
        }
        15 => {
            let n = d.count(8)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                let wall = d.str()?;
                let mut lines = [vec![], vec![]];
                for l in &mut lines {
                    let m = d.count(9)?;
                    for _ in 0..m {
                        l.push((d.u32()?, d.u32()? as i32, d.section()?));
                    }
                }
                let [u, v] = lines;
                let nt = d.count(16)?;
                let top = (0..nt).map(|_| d.p2()).collect::<Result<_, _>>()?;
                out.push(CurtainLines { wall, u, v, top });
            }
            Frame::CurtainLines(out)
        }
        16 => {
            let n = d.count(71)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                let (guid, level) = (d.str()?, d.str()?);
                let kind = match d.u8()? {
                    0 => MemberKind::Column,
                    1 => MemberKind::Beam,
                    t => return bad(format!("member kind {t}")),
                };
                let name = d.str()?;
                let mut c = [0.0; 6];
                for x in &mut c {
                    *x = d.f64()?;
                }
                out.push(Member {
                    guid,
                    level,
                    kind,
                    name,
                    a: [c[0], c[1], c[2]],
                    b: [c[3], c[4], c[5]],
                    section: d.section()?,
                    rotation: d.f64()?,
                    structural: d.bool()?,
                    profile: d.ring()?,
                });
            }
            Frame::Members(out)
        }
        18 => {
            let n = d.count(28)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                let (guid, level, slab_type, height) = (d.str()?, d.str()?, d.str()?, d.f64()?);
                let outer = d.ring()?;
                let k = d.count(4)?;
                let holes = (0..k).map(|_| d.ring()).collect::<Result<_, _>>()?;
                out.push(Ceiling {
                    guid,
                    level,
                    slab_type,
                    height,
                    outer,
                    holes,
                });
            }
            Frame::Ceilings(out)
        }
        28 => {
            let n = d.count(104)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                let (guid, level, name, category, material) =
                    (d.str()?, d.str()?, d.str()?, d.str()?, d.str()?);
                let o = [d.f64()?, d.f64()?, d.f64()?];
                let x = [d.f64()?, d.f64()?, d.f64()?];
                let y = [d.f64()?, d.f64()?, d.f64()?];
                let outline = d.ring()?;
                let depth = d.f64()?;
                out.push(Prism {
                    guid,
                    level,
                    name,
                    category,
                    material,
                    o,
                    x,
                    y,
                    outline,
                    depth,
                });
            }
            Frame::Prisms(out)
        }
        17 => {
            let n = d.count(21)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                let (guid, level, domain, name) = (d.str()?, d.str()?, d.str()?, d.str()?);
                let section = d.section()?;
                let np = d.count(24)?;
                let points = (0..np)
                    .map(|_| Ok([d.f64()?, d.f64()?, d.f64()?]))
                    .collect::<Result<_, ProtoError>>()?;
                out.push(MepRun {
                    guid,
                    level,
                    domain,
                    name,
                    section,
                    points,
                });
            }
            Frame::MepRuns(out)
        }
        0 => return bad("frame tag 0"),
        t => Frame::Unknown(t),
    })
}

fn dec_element(d: &mut Dec) -> Result<Element, ProtoError> {
    Ok(match d.u8()? {
        0 => {
            let (guid, level, wall_type) = (d.str()?, d.str()?, d.str()?);
            let axis = match d.u8()? {
                0 => Curve::Line {
                    a: d.p2()?,
                    b: d.p2()?,
                },
                1 => Curve::Arc {
                    a: d.p2()?,
                    mid: d.p2()?,
                    b: d.p2()?,
                },
                t => return bad(format!("curve tag {t}")),
            };
            let base_offset = d.f64()?;
            let top = match d.u8()? {
                0 => Top::Unconnected { height: d.f64()? },
                1 => Top::Level {
                    level: d.str()?,
                    offset: d.f64()?,
                },
                t => return bad(format!("top tag {t}")),
            };
            let offset0 = if d.bool()? { Some(d.f64()?) } else { None };
            Element::Wall {
                guid,
                level,
                wall_type,
                axis,
                base_offset,
                top,
                offset0,
            }
        }
        1 => Element::Opening {
            guid: d.str()?,
            host: d.str()?,
            kind: match d.u8()? {
                0 => OpeningKind::Door,
                1 => OpeningKind::Window,
                2 => OpeningKind::Void,
                t => return bad(format!("opening kind {t}")),
            },
            family_type: d.opt_str()?,
            along: d.f64()?,
            width: d.f64()?,
            height: d.f64()?,
            sill: d.f64()?,
            swing_side: d.bool()?,
            swing_hinge: d.bool()?,
        },
        2 => {
            let (guid, level, slab_type, offset) = (d.str()?, d.str()?, d.str()?, d.f64()?);
            let outer = d.ring()?;
            let k = d.count(4)?;
            let holes = (0..k).map(|_| d.ring()).collect::<Result<_, _>>()?;
            Element::Slab {
                guid,
                level,
                slab_type,
                offset,
                outer,
                holes,
            }
        }
        3 => Element::Room {
            guid: d.str()?,
            level: d.str()?,
            name: d.str()?,
            number: d.str()?,
            height: d.f64()?,
            boundary: d.ring()?,
            point: if d.bool()? { Some(d.p2()?) } else { None },
        },
        4 => {
            let (guid, level, category, name) = (d.str()?, d.opt_str()?, d.str()?, d.str()?);
            let n = d.count(24)?;
            let positions = (0..n)
                .map(|_| Ok([d.f64()?, d.f64()?, d.f64()?]))
                .collect::<Result<_, ProtoError>>()?;
            let k = d.count(4)?;
            let indices = (0..k).map(|_| d.u32()).collect::<Result<_, _>>()?;
            Element::Generic {
                guid,
                level,
                category,
                name,
                positions,
                indices,
            }
        }
        t => return bad(format!("element tag {t}")),
    })
}

// ---------- stream ----------

/// Bridge side: writes the preamble once, then length-prefixed frames.
pub struct FrameWriter<W: Write> {
    w: W,
    pub max_frame: u32,
}

impl<W: Write> FrameWriter<W> {
    pub fn new(mut w: W) -> io::Result<Self> {
        w.write_all(&MAGIC)?;
        w.write_all(&PROTOCOL.to_le_bytes())?;
        Ok(Self {
            w,
            max_frame: MAX_FRAME,
        })
    }

    /// Refuses frames over `max_frame`: split Elements into smaller batches instead.
    pub fn write(&mut self, f: &Frame) -> io::Result<()> {
        let body = encode(f);
        if body.len() > self.max_frame as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("frame of {} bytes exceeds {}", body.len(), self.max_frame),
            ));
        }
        self.write_raw(&body)
    }

    /// Writes a pre-encoded body unchecked (tests of host limits use it).
    pub fn write_raw(&mut self, body: &[u8]) -> io::Result<()> {
        self.w.write_all(&(body.len() as u32).to_le_bytes())?;
        self.w.write_all(body)?;
        self.w.flush()
    }

    pub fn inner(&mut self) -> &mut W {
        &mut self.w
    }
}

/// Host side: checks the preamble, then yields frames; `Ok(None)` = clean end of stream.
pub struct FrameReader<R: Read> {
    r: R,
    pub max_frame: u32,
    started: bool,
}

impl<R: Read> FrameReader<R> {
    pub fn new(r: R) -> Self {
        Self {
            r,
            max_frame: MAX_FRAME,
            started: false,
        }
    }

    /// Fills `buf`; Ok(false) if the stream ended before the first byte.
    fn fill(&mut self, buf: &mut [u8]) -> Result<bool, ProtoError> {
        let mut got = 0;
        while got < buf.len() {
            match self.r.read(&mut buf[got..]) {
                Ok(0) if got == 0 => return Ok(false),
                Ok(0) => return Err(ProtoError::Truncated),
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ProtoError::Io(e)),
            }
        }
        Ok(true)
    }

    pub fn next_frame(&mut self) -> Result<Option<Frame>, ProtoError> {
        if !self.started {
            let mut pre = [0u8; 6];
            if !self.fill(&mut pre)? {
                return Err(ProtoError::Truncated);
            }
            if pre[..4] != MAGIC {
                return Err(ProtoError::BadMagic);
            }
            let v = u16::from_le_bytes([pre[4], pre[5]]);
            if v != PROTOCOL {
                return Err(ProtoError::Version(v));
            }
            self.started = true;
        }
        let mut len = [0u8; 4];
        if !self.fill(&mut len)? {
            return Ok(None);
        }
        let n = u32::from_le_bytes(len);
        if n > self.max_frame {
            return Err(ProtoError::Oversize(n));
        }
        if n == 0 {
            return bad("empty frame");
        }
        let mut body = vec![0u8; n as usize];
        if !self.fill(&mut body)? {
            return Err(ProtoError::Truncated);
        }
        decode(&body).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<Frame> {
        vec![
            Frame::Hello(Hello {
                protocol: 1,
                bridge: "t".into(),
                version: "1.0".into(),
                formats: vec!["abc".into()],
                capabilities: vec!["views".into()],
            }),
            Frame::Progress(Progress {
                done: 1,
                total: 9,
                phase: "walls".into(),
            }),
            Frame::Units(Units { length_to_m: 0.001 }),
            Frame::Levels(vec![Level {
                guid: "L1".into(),
                name: "\u{042d}\u{0442} 1".into(),
                elevation: 0.0,
            }]),
            Frame::Grids(vec![Grid {
                guid: "G".into(),
                name: "A".into(),
                a: (0.0, 0.0),
                b: (1.0, 0.0),
            }]),
            Frame::Materials(vec![Material {
                guid: "M".into(),
                name: "m".into(),
                color: 0xff00ffff,
            }]),
            Frame::Types(vec![
                Type::Wall {
                    guid: "W".into(),
                    name: "w".into(),
                    layers: vec![Layer {
                        material: "M".into(),
                        thickness: 0.2,
                    }],
                },
                Type::Family {
                    guid: "D".into(),
                    category: "door".into(),
                    name: "d".into(),
                    params: vec![("fire".into(), Value::Text("EI30".into()))],
                },
            ]),
            Frame::Elements(vec![
                Element::Wall {
                    guid: "w1".into(),
                    level: "L1".into(),
                    wall_type: "W".into(),
                    axis: Curve::Arc {
                        a: (0.0, 0.0),
                        mid: (1.0, 1.0),
                        b: (2.0, 0.0),
                    },
                    base_offset: 0.0,
                    top: Top::Level {
                        level: "L2".into(),
                        offset: -0.1,
                    },
                    offset0: Some(0.05),
                },
                Element::Generic {
                    guid: "g".into(),
                    level: None,
                    category: "furniture".into(),
                    name: "box".into(),
                    positions: vec![[0.0, 1.0, 2.0]; 3],
                    indices: vec![0, 1, 2],
                },
            ]),
            Frame::WallTops(vec![WallTop {
                wall: "w1".into(),
                points: vec![(0.0, 3.0), (2.0, 4.5), (2.0, 4.0)],
            }]),
            Frame::SlabSlopes(vec![SlabSlope {
                slab: "s1".into(),
                point: (1.0, 2.0),
                gradient: (0.02, -0.01),
            }]),
            Frame::CurtainWalls(vec![CurtainWall {
                guid: "c1".into(),
                level: "L".into(),
                name: "Storefront".into(),
                axis: Curve::Arc {
                    a: (0.0, 0.0),
                    mid: (1.0, 1.0),
                    b: (2.0, 0.0),
                },
                base_offset: 0.1,
                height: 3.0,
                u: vec![1.5, 3.0],
                v: vec![1.0],
                interior: MullionSection::Rect {
                    width: 0.05,
                    depth: 0.15,
                },
                border: MullionSection::Circle { diameter: 0.06 },
                panels: vec![
                    CurtainPanel {
                        i: 1,
                        j: 0,
                        kind: PanelKind::Door,
                        name: "door".into(),
                        family_type: Some("F".into()),
                        swing_side: true,
                        swing_hinge: false,
                    },
                    CurtainPanel {
                        i: 2,
                        j: 1,
                        kind: PanelKind::Empty,
                        name: String::new(),
                        family_type: None,
                        swing_side: false,
                        swing_hinge: false,
                    },
                ],
            }]),
            Frame::CurtainLines(vec![CurtainLines {
                wall: "c1".into(),
                u: vec![
                    (0, -1, MullionSection::None),
                    (2, 1, MullionSection::Circle { diameter: 0.1 }),
                ],
                v: vec![(
                    1,
                    -1,
                    MullionSection::Rect {
                        width: 0.2,
                        depth: 0.1,
                    },
                )],
                top: vec![(0.0, 2.0), (1.0, 3.0)],
            }]),
            Frame::Members(vec![Member {
                guid: "c9".into(),
                level: "L1".into(),
                kind: MemberKind::Beam,
                name: "B 300x600".into(),
                a: [0.0, 1.0, 3.0],
                b: [5.0, 1.0, 3.0],
                section: MullionSection::Rect {
                    width: 0.3,
                    depth: 0.6,
                },
                rotation: 0.0,
                structural: true,
                profile: vec![(-0.1, -0.2), (0.1, -0.2), (0.0, 0.3)],
            }]),
            Frame::Prisms(vec![Prism {
                guid: "pr1".into(),
                level: "L1".into(),
                name: "Panel".into(),
                category: "curtain panels".into(),
                material: "Glass".into(),
                o: [1.0, 2.0, 3.0],
                x: [1.0, 0.0, 0.0],
                y: [0.0, 0.0, 1.0],
                outline: vec![(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)],
                depth: 0.02,
            }]),
            Frame::Ceilings(vec![Ceiling {
                guid: "ce1".into(),
                level: "L1".into(),
                slab_type: "st".into(),
                height: 2.7,
                outer: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 3.0)],
                holes: vec![vec![(1.0, 0.5), (2.0, 0.5), (1.5, 1.0)]],
            }]),
            Frame::MepRuns(vec![MepRun {
                guid: "p1".into(),
                level: "L1".into(),
                domain: "pipe".into(),
                name: "Pipe".into(),
                section: MullionSection::Circle { diameter: 0.05 },
                points: vec![[0.0, 0.0, 2.5], [3.0, 0.0, 2.5]],
            }]),
            Frame::Properties(vec![Property {
                owner: "w1".into(),
                key: "k".into(),
                value: Value::Real(1.5),
                unit: "m".into(),
            }]),
            Frame::Warnings(vec![Warning {
                code: "x".into(),
                message: "y".into(),
                element: Some("w1".into()),
            }]),
            Frame::DocInfo(vec![("file_version".into(), "2024".into())]),
            Frame::ParamDefs(vec![
                ParamDef {
                    key: "Fire rating".into(),
                    group: "Identity".into(),
                    type_param: true,
                    shared: Some(true),
                    shared_guid: "a-b".into(),
                    spec: "text".into(),
                },
                ParamDef {
                    key: "Mark".into(),
                    ..Default::default()
                },
            ]),
            Frame::SourceIds(vec![SourceId {
                guid: "w1".into(),
                ifc_guid: "0123456789ABCDEFGHIJKL".into(),
            }]),
            Frame::Views(vec![
                View {
                    guid: "v1".into(),
                    name: "Level 1".into(),
                    kind: "plan".into(),
                    level: Some("L1".into()),
                },
                View {
                    guid: "v2".into(),
                    name: "{3D}".into(),
                    kind: "3d".into(),
                    level: None,
                },
            ]),
            Frame::End(Stats {
                elements: 2,
                warnings: 1,
            }),
        ]
    }

    #[test]
    fn round_trip_stream() {
        let mut w = FrameWriter::new(Vec::new()).unwrap();
        for f in all() {
            w.write(&f).unwrap();
        }
        let buf = w.inner().clone();
        let mut r = FrameReader::new(&buf[..]);
        let mut got = vec![];
        while let Some(f) = r.next_frame().unwrap() {
            got.push(f);
        }
        assert_eq!(got, all());
    }

    #[test]
    fn every_truncation_is_an_error_not_a_panic() {
        let mut w = FrameWriter::new(Vec::new()).unwrap();
        for f in all() {
            w.write(&f).unwrap();
        }
        let buf = w.inner().clone();
        let mut ends = vec![6];
        for f in all() {
            ends.push(ends.last().unwrap() + 4 + encode(&f).len());
        }
        for cut in 0..buf.len() {
            let mut r = FrameReader::new(&buf[..cut]);
            let res = (|| loop {
                if r.next_frame()?.is_none() {
                    return Ok::<_, ProtoError>(());
                }
            })();
            assert_eq!(res.is_ok(), ends.contains(&cut), "cut {cut}");
        }
    }

    #[test]
    fn limits_and_garbage() {
        let mut w = FrameWriter::new(Vec::new()).unwrap();
        w.max_frame = 8;
        assert!(w.write(&all()[0]).is_err());
        let mut b = MAGIC.to_vec();
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&(MAX_FRAME + 1).to_le_bytes());
        assert!(matches!(
            FrameReader::new(&b[..]).next_frame(),
            Err(ProtoError::Oversize(_))
        ));
        assert!(matches!(
            FrameReader::new(&b"XXXX\x01\x00"[..]).next_frame(),
            Err(ProtoError::BadMagic)
        ));
        // count claims 2^32-1 items in a 5-byte body
        assert!(decode(&[8, 0xff, 0xff, 0xff, 0xff]).is_err());
        assert!(decode(&[3, 0, 0, 0, 0, 0, 0, 0xf8, 0x7f]).is_err()); // NaN
        assert_eq!(decode(&[200, 1, 2]).unwrap(), Frame::Unknown(200));
        // deterministic pseudo-random garbage never panics
        let mut x = 0x9e3779b97f4a7c15u64;
        for _ in 0..20000 {
            let n = (x % 64) as usize;
            let body: Vec<u8> = (0..n)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    (x % 13) as u8
                })
                .collect();
            let _ = decode(&body);
        }
    }
}
