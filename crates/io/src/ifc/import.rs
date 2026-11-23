// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! IFC import (IFC2X3, IFC4, IFC4X1..IFC4X3, IFCZIP): storeys → levels, walls → wall types +
//! walls, openings, slabs, roofs, ceilings, columns/beams, curtain walls, rooms, grids; every
//! other product with geometry → an editable mesh. Element data → `prop`, GlobalId → `ext_ref`.
//! Imported objects become native (ADR 0008).

mod circuits;
mod commit;
mod fit;
mod gsolid;
pub use gsolid::PSolid;
mod host_voids;
mod matdef;
mod mep_links;
mod project;
mod props;
mod source;
mod stairs;
mod surf;
mod tess;
mod types;
pub use types::{type_drafts, TypeDraft};
pub(crate) use types::item_parts;
mod uparams;

pub(in crate::ifc) use props::{units as file_units, Units};

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::Path;

use ddc_model::Model;
use rusqlite::Connection;

use super::reader::{Args, Entity, StepError, StepFile, Value};

pub(in crate::ifc) use commit::commit_with_ctl;
pub use mep_links::{PEnd, PMepLink};
pub use source::IfcFile;
pub use stairs::{PRailing, PRamp, PStair};

#[derive(Debug)]
pub enum ImportError {
    Io(String),
    Step(StepError),
    Schema(String),
    Units(String),
    Model(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ImportError::Io(e) => write!(f, "IFC read: {e}"),
            ImportError::Step(e) => write!(f, "IFC {e}"),
            ImportError::Schema(s) => write!(f, "IFC schema not supported: {s}"),
            ImportError::Units(s) => write!(f, "IFC units: {s}"),
            ImportError::Model(s) => write!(f, "IFC import into model: {s}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<StepError> for ImportError {
    fn from(e: StepError) -> Self {
        ImportError::Step(e)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ImportReport {
    pub schema: String,
    /// Metres per file length unit.
    pub length_unit: f64,
    pub levels: usize,
    pub wall_types: usize,
    pub walls: usize,
    pub openings: usize,
    /// Openings cut into slabs and roofs (IfcRelVoidsElement on a slab or roof host).
    pub host_openings: usize,
    pub slabs: usize,
    pub curtain_walls: usize,
    pub rooms: usize,
    /// Room separators (virtual elements with an axis curve).
    pub separators: usize,
    /// IfcRelSpaceBoundary that resolved to an imported room and wall/separator.
    pub space_boundaries: usize,
    /// Grid axes (IfcGridAxis).
    pub grids: usize,
    /// Component instances (products with a `DDC_Component` property set).
    pub components: usize,
    /// Of those, instances of families made from foreign type objects (IfcTypeProduct maps).
    pub type_families: usize,
    /// Network runs (segments with a `DDC_MepRun` property set); fittings are derived.
    pub mep_runs: usize,
    /// IfcRelConnectsPorts between imported runs and components that were read.
    pub mep_links: usize,
    /// Generic meshes (products without a native equivalent, or whose native form failed).
    pub meshes: usize,
    pub columns: usize,
    pub beams: usize,
    pub roofs: usize,
    pub ceilings: usize,
    pub stairs: usize,
    pub ramps: usize,
    pub railings: usize,
    /// Isolated and strip footings.
    pub footings: usize,
    /// Property rows written.
    pub properties: usize,
    /// Re-import of the same project: linked rows changed / identical / not in the file.
    pub updated: usize,
    pub unchanged: usize,
    pub missing: usize,
    /// Re-import merge: fields kept because they were edited here, conflicts with the file,
    /// rows removed because the file no longer has them.
    pub kept_local: usize,
    pub conflicts: usize,
    pub removed: usize,
    /// Re-import: locked parameter values the file would change, kept as they are
    /// (table, id, parameter).
    pub locked: Vec<(String, i64, String)>,
    /// File-level problems that did not stop the import (dangling references, duplicate ids).
    pub warnings: Vec<String>,
    /// "TYPE: what" → count of geometry imported approximately.
    pub approximated: BTreeMap<String, usize>,
    /// "TYPE: why not native" → count of products imported as meshes instead.
    pub as_mesh: BTreeMap<String, usize>,
    /// "TYPE" or "TYPE: reason" → count. Nothing is dropped without an entry here.
    pub skipped: BTreeMap<String, usize>,
}

impl ImportReport {
    fn skip(&mut self, ty: &str, why: &str) {
        let k = if why.is_empty() {
            ty.to_string()
        } else {
            format!("{ty}: {why}")
        };
        *self.skipped.entry(k).or_default() += 1;
    }

    /// Per IFC class: (class, count, reason) of every product not imported as its native
    /// category, meshes and skipped alike, sorted by class.
    pub fn not_native(&self) -> Vec<(String, usize, String)> {
        let split = |k: &String, n: &usize, how: &str| {
            let (class, why) = k.split_once(": ").unwrap_or((k.as_str(), ""));
            let why = if why.is_empty() { how.to_string() } else { format!("{how}: {why}") };
            (class.to_string(), *n, why)
        };
        let mut v: Vec<_> = self
            .as_mesh
            .iter()
            .map(|(k, n)| split(k, n, "mesh"))
            .chain(self.skipped.iter().map(|(k, n)| split(k, n, "skipped")))
            .collect();
        v.sort();
        v
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PLevel {
    pub name: String,
    pub elevation: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PType {
    pub name: String,
    pub layers: Vec<(String, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PWall {
    pub level: usize,
    pub wall_type: usize,
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub base_offset: f64,
    pub height: f64,
    pub offset0: Option<f64>,
    /// tan(sweep/4) of an arc axis; 0 = straight.
    pub bulge: f64,
    /// Clipped top (u along the axis, h above the base); None = flat at `height`.
    pub top: Option<Vec<(f64, f64)>>,
}

/// Property value.
#[derive(Debug, Clone, PartialEq)]
pub enum PVal {
    Int(i64),
    Real(f64),
    Text(String),
    Bool(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PProp {
    pub key: String,
    pub value: PVal,
    pub unit: String,
}

/// Plan element `kind`[`index`] from the product with `guid` (empty: none), with its data.
#[derive(Debug, Clone, PartialEq)]
pub struct PElem {
    pub kind: &'static str,
    pub index: usize,
    pub guid: String,
    pub props: Vec<PProp>,
}

/// Generic mesh: world positions, triangle indices.
#[derive(Debug, Clone, PartialEq)]
pub struct PMesh {
    pub level: usize,
    pub category: String,
    pub name: String,
    pub positions: Vec<[f64; 3]>,
    pub indices: Vec<u32>,
}

/// Column (vertical or slanted) or beam from a straight extrusion; `a`→`b` world, section
/// rotation about the axis (radians).
#[derive(Debug, Clone, PartialEq)]
pub struct PMember {
    pub column: bool,
    pub level: usize,
    pub shape: ddc_model::MemberShape,
    pub type_name: String,
    pub material: String,
    pub a: [f64; 3],
    pub b: [f64; 3],
    pub rotation: f64,
    /// Exact model row from `DDC_Member` (level and type are filled at commit).
    pub exact: Option<MemberExact>,
}

/// Exact column (with its top level by name) or beam row.
#[derive(Debug, Clone, PartialEq)]
pub enum MemberExact {
    Column(ddc_model::ColumnSpec, Option<String>),
    Beam(ddc_model::BeamSpec),
}

/// Roof: planes at `offset` above the level along the eave line.
#[derive(Debug, Clone, PartialEq)]
pub struct PRoof {
    pub level: usize,
    pub roof_type: usize,
    pub offset: f64,
    pub overhang: f64,
    pub fascia: f64,
    pub kind: ddc_model::RoofKind,
}

/// Ceiling: bottom `height` above the level.
#[derive(Debug, Clone, PartialEq)]
pub struct PCeiling {
    pub level: usize,
    pub ceiling_type: usize,
    pub height: f64,
    pub outer: Vec<(f64, f64)>,
    pub holes: Vec<Vec<(f64, f64)>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct POpening {
    pub wall: usize,
    pub along: f64,
    pub width: f64,
    pub sill: f64,
    pub height: f64,
    pub kind: String,
    /// Door only: (opens to the right of the wall axis, hinge at the far end).
    pub swing: (bool, bool),
}

/// Room separator: segment on a level.
#[derive(Debug, Clone, PartialEq)]
pub struct PSep {
    pub level: usize,
    pub a: (f64, f64),
    pub b: (f64, f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PTarget {
    Wall(usize),
    Sep(usize),
}

/// Room boundary relation; boundaries are derived by the model, so this is checked, not stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PBoundary {
    pub room: usize,
    pub target: PTarget,
}

/// Slab: plan rings (outer CCW, holes CW), top relative to its level; type layers top first.
#[derive(Debug, Clone, PartialEq)]
pub struct PSlab {
    pub level: usize,
    pub slab_type: usize,
    pub offset: f64,
    pub outer: Vec<(f64, f64)>,
    pub holes: Vec<Vec<(f64, f64)>>,
    /// Sloped top: (point where the top is at `offset`, gradient).
    pub slope: Option<((f64, f64), (f64, f64))>,
}

/// Curtain wall: grid nodes from its members (or panel gaps), one panel per cell.
#[derive(Debug, Clone, PartialEq)]
pub struct PCurtain {
    pub level: usize,
    pub name: String,
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub bulge: f64,
    pub base_offset: f64,
    pub height: f64,
    /// Interior grid lines along the axis / above the base.
    pub u: Vec<f64>,
    pub v: Vec<f64>,
    pub interior: ddc_geom::curtain::Section,
    pub border: ddc_geom::curtain::Section,
    /// (i, j, kind, name, swing side, swing hinge); cells not listed are empty.
    pub panels: Vec<(usize, usize, &'static str, String, bool, bool)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PRoom {
    pub level: usize,
    pub name: String,
    pub number: String,
    pub point: (f64, f64),
    pub height: f64,
    /// Plan outline of the space body (outer ring); empty when unknown.
    pub ring: Vec<(f64, f64)>,
}

/// Isolated footing (`spec.level` is filled at commit).
#[derive(Debug, Clone, PartialEq)]
pub struct PFooting {
    pub level: usize,
    pub spec: ddc_model::FootingSpec,
}

/// Straight strip footing without a host wall (`spec.level` is filled at commit).
#[derive(Debug, Clone, PartialEq)]
pub struct PStrip {
    pub level: usize,
    pub spec: ddc_model::StripFootingSpec,
}

/// Model-independent result of reading a file; [`import_ifc`] commits it.
/// Grid axis: line or arc (bulge), level range by plan level index, bubbles bit 0 start / 1 end.
#[derive(Debug, Clone, PartialEq)]
pub struct PGrid {
    pub name: String,
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub bulge: f64,
    pub base: Option<usize>,
    pub top: Option<usize>,
    pub bubbles: i64,
    ord: usize,
}

/// Component instance from `DDC_Component`: exact definition, type and placement.
#[derive(Debug, Clone, PartialEq)]
pub struct PComp {
    pub def: String,
    pub type_name: String,
    pub type_vals: String,
    pub vals: String,
    pub level: usize,
    /// x, y, z, rotation in the host frame (level frame without a host), mirror.
    pub local: ([f64; 4], bool),
    /// Same relative to the storey — used when the host did not come along.
    pub world: ([f64; 4], bool),
    pub host: PCompHost,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PCompHost {
    None,
    Wall(usize),
    Slab(usize),
}

/// Colour (0xRRGGBB) and SI properties of a material named in the file; only set values.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PMaterial {
    pub name: String,
    pub color: Option<u32>,
    pub density: Option<f64>,
    pub conductivity: Option<f64>,
    pub specific_heat: Option<f64>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Plan {
    /// Materials with a colour or physical values; they become rows only when the name is new.
    pub materials: Vec<PMaterial>,
    pub levels: Vec<PLevel>,
    pub types: Vec<PType>,
    pub walls: Vec<PWall>,
    pub openings: Vec<POpening>,
    pub slab_types: Vec<PType>,
    pub slabs: Vec<PSlab>,
    pub rooms: Vec<PRoom>,
    pub seps: Vec<PSep>,
    pub boundaries: Vec<PBoundary>,
    pub curtains: Vec<PCurtain>,
    pub grids: Vec<PGrid>,
    pub comps: Vec<PComp>,
    pub meps: Vec<PMep>,
    pub mep_links: Vec<PMepLink>,
    pub circuits: Vec<PCircuit>,
    pub meshes: Vec<PMesh>,
    pub members: Vec<PMember>,
    pub roofs: Vec<PRoof>,
    /// Plan rings (counter-clockwise) cut into roofs as host openings: (roof index, ring).
    pub roof_holes: Vec<(usize, Vec<(f64, f64)>)>,
    pub ceilings: Vec<PCeiling>,
    pub stairs: Vec<PStair>,
    pub ramps: Vec<PRamp>,
    pub railings: Vec<PRailing>,
    pub footings: Vec<PFooting>,
    pub strip_footings: Vec<PStrip>,
    /// Exact generic solids of this exporter, with their IFC class.
    pub solids: Vec<gsolid::PSolid>,
    pub elems: Vec<PElem>,
    pub project_guid: String,
    pub site_guid: String,
    pub building_guid: String,
    pub project_props: Vec<PProp>,
    /// Name, number, address, stage read from IFCPROJECT / IFCSITE / IFCBUILDING.
    pub project_info: Option<ddc_model::ProjectInfo>,
    /// Coordinate system and base point of the file's map conversion, when it has one.
    pub site_crs: Option<ddc_model::SiteProps>,
    /// The file this plan was read from (path, content hash, stat): recorded as the source.
    pub file: Option<IfcFile>,
    pub report: ImportReport,
}

/// Electrical circuit from `DDC_Circuit`: `panel` and device components are indices in
/// `Plan::comps`; a device is (component, connector, explicit load).
#[derive(Debug, Clone, PartialEq)]
pub struct PCircuit {
    pub panel: usize,
    /// Way number, 0: the next free one.
    pub number: u32,
    pub name: String,
    pub kind: ddc_model::CircuitKind,
    /// Name of the MEP system, empty for none.
    pub system: String,
    pub voltage: f64,
    pub poles: u32,
    pub breaker: f64,
    pub cable: f64,
    pub max_drop: f64,
    pub cos_phi: f64,
    pub laying: String,
    pub ambient_c: f64,
    pub grouped: u32,
    pub conductor: ddc_model::Conductor,
    pub load_type: String,
    pub rcd: String,
    pub cable_type: String,
    pub devices: Vec<(usize, String, Option<f64>)>,
    /// Cable pieces from the IfcCableSegment grouped with it: axis points, z absolute.
    pub cables: Vec<Vec<(f64, f64, f64)>>,
}

/// Network run from `DDC_MepRun`: exact section, catalog, slope, points and caps.
#[derive(Debug, Clone, PartialEq)]
pub struct PMep {
    pub system: String,
    pub kind: ddc_model::SystemKind,
    pub level: usize,
    pub spec: ddc_model::RunSpec,
}

fn mep_run(kv: &HashMap<String, String>, storey: Option<usize>) -> Result<PMep, Why> {
    use ddc_model::{MepDomain, MepSection, MepShape, PipeMaterial, SystemKind};
    let level = storey.ok_or("not in a storey")?;
    let bad = "bad DDC_MepRun";
    let get = |k: &str| kv.get(k).map(String::as_str).ok_or("incomplete DDC_MepRun");
    let num = |k: &str| -> Result<f64, Why> {
        get(k)?
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or(bad)
    };
    let kind = SystemKind::parse(get("SystemKind")?).ok_or(bad)?;
    let domain = MepDomain::parse(get("Domain")?).ok_or(bad)?;
    let (w, h) = (num("Width")?, num("Height")?);
    let shape = match get("Shape")? {
        "round" => MepShape::Round { d: w },
        "rect" => MepShape::Rect { w, h },
        "oval" => MepShape::Oval { w, h },
        _ => return Err(bad),
    };
    let mut section = MepSection::new(domain, shape);
    section.wall = num("Wall")?;
    section.insulation = num("Insulation")?;
    section.lining = num("Lining")?;
    let opt = |k: &str| kv.get(k).map(String::as_str).filter(|v| !v.is_empty());
    let caps: Vec<&str> = get("Caps")?.split_whitespace().collect();
    let spec = ddc_model::RunSpec {
        system: 0,
        level: 0,
        section,
        material: opt("Material").and_then(PipeMaterial::parse),
        dn: match opt("DN") {
            Some(d) => Some(d.parse().map_err(|_| bad)?),
            None => None,
        },
        bend: match opt("Bend") {
            Some(b) => Some(b.parse().map_err(|_| bad)?),
            None => None,
        },
        slope: num("Slope")?,
        points: ddc_model::parse_points(get("Points")?).ok_or(bad)?,
        caps: (caps.first() == Some(&"1"), caps.get(1) == Some(&"1")),
    };
    Ok(PMep {
        system: get("System")?.to_string(),
        kind,
        level,
        spec,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ElevMode {
    Placement,
    PlacementWarn,
    Attr,
}

/// Which source gives storey elevations. Placement wins, except when all placements are flat
/// (equal z) while the Elevation attributes differ: then the attribute is the real height.
fn storey_elevation_mode(c: &[(u32, Option<f64>, Option<f64>)]) -> ElevMode {
    const TOL: f64 = 1e-3;
    let span = |v: Vec<f64>| {
        let lo = v.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        hi - lo
    };
    let zs: Vec<f64> = c.iter().filter_map(|x| x.1).collect();
    let es: Vec<f64> = c.iter().filter_map(|x| x.2).collect();
    let disagree = c
        .iter()
        .any(|x| matches!((x.1, x.2), (Some(p), Some(a)) if (p - a).abs() > TOL));
    if !disagree {
        return ElevMode::Placement;
    }
    if es.len() > 1 && span(es) > TOL && span(zs) <= TOL {
        ElevMode::Attr
    } else {
        ElevMode::PlacementWarn
    }
}

/// Lengths land on a 1 nm grid: decimal source values come back bit-exact.
fn snap(v: f64) -> f64 {
    let r = (v * 1e9).round() / 1e9;
    if r == 0.0 {
        0.0
    } else {
        r
    }
}

type V3 = [f64; 3];

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn mul(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: V3) -> Option<V3> {
    let l = dot(a, a).sqrt();
    (l > 1e-12 && l.is_finite()).then(|| mul(a, 1.0 / l))
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    o: V3,
    x: V3,
    y: V3,
    z: V3,
}

impl Frame {
    const ID: Frame = Frame {
        o: [0.0; 3],
        x: [1.0, 0.0, 0.0],
        y: [0.0, 1.0, 0.0],
        z: [0.0, 0.0, 1.0],
    };
    fn dir(&self, v: V3) -> V3 {
        add(add(mul(self.x, v[0]), mul(self.y, v[1])), mul(self.z, v[2]))
    }
    fn apply(&self, p: V3) -> V3 {
        add(self.o, self.dir(p))
    }
    fn then(&self, l: &Frame) -> Frame {
        Frame {
            o: self.apply(l.o),
            x: self.dir(l.x),
            y: self.dir(l.y),
            z: self.dir(l.z),
        }
    }
    fn from_axes(o: V3, z: Option<V3>, x: Option<V3>) -> Result<Frame, &'static str> {
        let z = z.unwrap_or([0.0, 0.0, 1.0]);
        let r = x.unwrap_or(if z[0].abs() > 0.9 {
            [0.0, 1.0, 0.0]
        } else {
            [1.0, 0.0, 0.0]
        });
        let x = norm(add(r, mul(z, -dot(r, z)))).ok_or("degenerate placement")?;
        Ok(Frame {
            o,
            x,
            y: cross(z, x),
            z,
        })
    }
}

type Why = &'static str;

struct Rd<'a> {
    f: &'a StepFile,
    scale: f64,
    /// Radians per file plane-angle unit.
    angle: f64,
    frames: HashMap<u32, Frame>,
    /// Mapped representation meshes by map id (instanced items reuse them).
    maps: HashMap<u32, tess::Tri>,
}

impl<'a> Rd<'a> {
    fn ent(&self, id: u32, ty: &[&str]) -> Result<&'a Entity, Why> {
        let t = self.f.type_of(id).ok_or("missing entity")?;
        if !ty.is_empty() && !ty.contains(&t) {
            return Err("unexpected entity type");
        }
        self.f.get(id).ok_or("missing entity")
    }
    /// Arguments of a leaf entity (see [`StepFile::leaf`]): not pinned for the plan's life.
    fn leaf(&self, id: u32, ty: &[&str]) -> Result<Args<'a>, Why> {
        let t = self.f.type_of(id).ok_or("missing entity")?;
        if !ty.is_empty() && !ty.contains(&t) {
            return Err("unexpected entity type");
        }
        self.f.leaf(id).ok_or("missing entity")
    }
    fn arg(e: &Entity, i: usize) -> &Value {
        Self::at(e.args(), i)
    }
    fn at(a: &[Value], i: usize) -> &Value {
        a.get(i).unwrap_or(&Value::Null)
    }
    fn reference(e: &Entity, i: usize) -> Result<u32, Why> {
        Self::arg(e, i).as_ref().ok_or("missing reference")
    }
    fn refs(e: &Entity, i: usize) -> Vec<u32> {
        Self::refs_at(e.args(), i)
    }
    fn refs_at(a: &[Value], i: usize) -> Vec<u32> {
        Self::at(a, i)
            .as_list()
            .map_or(vec![], |l| l.iter().filter_map(Value::as_ref).collect())
    }
    fn coords(&self, id: u32, ty: &str) -> Result<V3, Why> {
        let e = self.leaf(id, &[ty])?;
        let l = Self::at(&e, 0).as_list().ok_or("bad coordinates")?;
        if l.is_empty() || l.len() > 3 {
            return Err("bad coordinates");
        }
        let mut v = [0.0; 3];
        for (k, c) in l.iter().enumerate() {
            v[k] = c.as_f64().ok_or("bad coordinates")?;
        }
        Ok(v)
    }
    fn point(&self, id: u32) -> Result<V3, Why> {
        Ok(mul(self.coords(id, "IFCCARTESIANPOINT")?, self.scale))
    }
    fn direction(&self, v: &Value) -> Result<Option<V3>, Why> {
        match v {
            Value::Null => Ok(None),
            Value::Ref(id) => Ok(Some(
                norm(self.coords(*id, "IFCDIRECTION")?).ok_or("zero direction")?,
            )),
            _ => Err("bad direction"),
        }
    }
    fn axis_placement(&self, v: &Value) -> Result<Frame, Why> {
        let Some(id) = v.as_ref() else {
            return if *v == Value::Null {
                Ok(Frame::ID)
            } else {
                Err("bad placement")
            };
        };
        let e = self.ent(id, &["IFCAXIS2PLACEMENT3D", "IFCAXIS2PLACEMENT2D"])?;
        let o = self.point(Self::reference(e, 0)?)?;
        if self.f.type_of(id) == Some("IFCAXIS2PLACEMENT2D") {
            let x = self.direction(Self::arg(e, 1))?;
            return Frame::from_axes(o, None, x.map(|x| [x[0], x[1], 0.0]));
        }
        let z = self.direction(Self::arg(e, 1))?;
        let x = self.direction(Self::arg(e, 2))?;
        Frame::from_axes(o, z, x)
    }
    fn local_placement(&mut self, id: u32) -> Result<Frame, Why> {
        let mut chain = vec![];
        let mut cur = Some(id);
        let mut base = Frame::ID;
        while let Some(c) = cur {
            if let Some(f) = self.frames.get(&c) {
                base = *f;
                break;
            }
            if chain.len() > 64 || chain.contains(&c) {
                return Err("placement cycle");
            }
            let e = self.ent(c, &["IFCLOCALPLACEMENT"])?;
            chain.push(c);
            cur = Self::arg(e, 0).as_ref();
        }
        for c in chain.into_iter().rev() {
            let e = self.f.get(c).ok_or("missing entity")?;
            let l = self.axis_placement(Self::arg(e, 1))?;
            base = base.then(&l);
            self.frames.insert(c, base);
        }
        Ok(base)
    }
    /// World frame of a product (ObjectPlacement = argument 5).
    fn product_frame(&mut self, e: &Entity) -> Result<Frame, Why> {
        match Self::arg(e, 5) {
            Value::Null => Ok(Frame::ID),
            Value::Ref(p) => self.local_placement(*p),
            _ => Err("bad placement"),
        }
    }
    /// Items of the shape representation named `ident` (Representation = argument 6).
    fn rep_items(&self, e: &Entity, ident: &str) -> Result<Vec<u32>, Why> {
        let Some(pds) = Self::arg(e, 6).as_ref() else {
            return Ok(vec![]);
        };
        let pds = self.ent(pds, &["IFCPRODUCTDEFINITIONSHAPE"])?;
        for r in Self::refs(pds, 2) {
            let rep = self.ent(r, &[])?;
            if Self::arg(rep, 1).as_str() == Some(ident) {
                return Ok(Self::refs(rep, 3));
            }
        }
        Ok(vec![])
    }
    /// Axis curve as straight segments in the product frame (one per polyline span).
    fn axis(&self, items: &[u32]) -> Result<Vec<(V3, V3)>, Why> {
        let &[id] = items else {
            return Err(if items.is_empty() {
                "no axis"
            } else {
                "unsupported axis"
            });
        };
        let e = self.ent(id, &[])?;
        match self.f.type_of(id) {
            Some("IFCPOLYLINE") => {
                let pts = Self::refs(e, 0);
                if pts.len() < 2 {
                    return Err("unsupported axis");
                }
                let pts = pts
                    .into_iter()
                    .map(|p| self.point(p))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(pts.windows(2).map(|w| (w[0], w[1])).collect())
            }
            Some("IFCTRIMMEDCURVE") => {
                let line = self.ent(Self::reference(e, 0)?, &["IFCLINE"])?;
                let p = self.point(Self::reference(line, 0)?)?;
                let vec = self.ent(Self::reference(line, 1)?, &["IFCVECTOR"])?;
                let d = self.direction(Self::arg(vec, 0))?.ok_or("bad direction")?;
                let mag = Self::arg(vec, 1).as_f64().ok_or("bad direction")?;
                let trim = |i: usize| -> Result<V3, Why> {
                    let sel = Self::arg(e, i).as_list().ok_or("bad trim")?;
                    for s in sel {
                        if let Some(r) = s.as_ref() {
                            return self.point(r);
                        }
                    }
                    for s in sel {
                        if let Value::Typed(t, a) = s {
                            if t == "IFCPARAMETERVALUE" && a.len() == 1 {
                                let t = a[0].as_f64().ok_or("bad trim")?;
                                return Ok(add(p, mul(d, t * mag * self.scale)));
                            }
                        }
                    }
                    Err("bad trim")
                };
                let (a, b) = (trim(1)?, trim(2)?);
                // SenseAgreement only says how the basis curve is traversed; the trimmed
                // curve itself always runs from Trim1 to Trim2.
                Ok(vec![(a, b)])
            }
            _ => Err("unsupported axis"),
        }
    }
    /// Corner points of a solid item, in the product frame.
    fn solid_points(&self, id: u32, out: &mut Vec<V3>, depth: usize) -> Result<(), Why> {
        if depth > 32 {
            return Err("unsupported body");
        }
        let e = self.ent(id, &[])?;
        match self.f.type_of(id).unwrap_or("") {
            "IFCBOOLEANCLIPPINGRESULT" | "IFCBOOLEANRESULT" => {
                self.solid_points(Self::reference(e, 1)?, out, depth + 1)
            }
            "IFCEXTRUDEDAREASOLID" => {
                let prof = Self::reference(e, 0)?;
                let pe = self.ent(prof, &[])?;
                let mut pts2: Vec<V3> = vec![];
                match self.f.type_of(prof).unwrap_or("") {
                    "IFCRECTANGLEPROFILEDEF" => {
                        let pos = self.axis_placement(Self::arg(pe, 2))?;
                        let xd = Self::arg(pe, 3).as_f64().ok_or("bad profile")? * self.scale;
                        let yd = Self::arg(pe, 4).as_f64().ok_or("bad profile")? * self.scale;
                        for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                            pts2.push(pos.apply([sx * xd / 2.0, sy * yd / 2.0, 0.0]));
                        }
                    }
                    "IFCARBITRARYCLOSEDPROFILEDEF" | "IFCARBITRARYPROFILEDEFWITHVOIDS" => {
                        pts2.extend(self.curve_points(Self::reference(pe, 2)?)?);
                    }
                    _ => return Err("unsupported profile"),
                }
                let pos = self.axis_placement(Self::arg(e, 1))?;
                let d = self.direction(Self::arg(e, 2))?.ok_or("bad extrusion")?;
                let depth = Self::arg(e, 3).as_f64().ok_or("bad extrusion")? * self.scale;
                for p in pts2 {
                    out.push(pos.apply(p));
                    out.push(pos.apply(add(p, mul(d, depth))));
                }
                Ok(())
            }
            _ => Err("unsupported body"),
        }
    }
    /// Vertices of a closed profile curve: polyline points, or the point list of an indexed
    /// poly curve (arc mid points included; enough for extents).
    fn curve_points(&self, c: u32) -> Result<Vec<V3>, Why> {
        let e = self.ent(c, &[]).map_err(|_| "unsupported profile")?;
        match self.f.type_of(c) {
            Some("IFCPOLYLINE") => Self::refs(e, 0)
                .into_iter()
                .map(|p| self.point(p))
                .collect(),
            Some("IFCINDEXEDPOLYCURVE") => {
                let pl = self.ent(Self::reference(e, 0)?, &["IFCCARTESIANPOINTLIST2D"])?;
                let rows = Self::arg(pl, 0).as_list().ok_or("bad point list")?;
                rows.iter()
                    .map(|row| {
                        let v = row.as_list().ok_or("bad point list")?;
                        let x = v.first().and_then(|x| x.as_f64()).ok_or("bad point list")?;
                        let y = v.get(1).and_then(|x| x.as_f64()).ok_or("bad point list")?;
                        Ok([x * self.scale, y * self.scale, 0.0])
                    })
                    .collect()
            }
            _ => Err("unsupported profile"),
        }
    }
    /// Arc axis (IfcTrimmedCurve on an IfcCircle, Cartesian trims) as (start, end, bulge) in
    /// the product frame; `None` for any other axis curve.
    fn arc_axis(&self, items: &[u32]) -> Result<Option<(V3, V3, f64)>, Why> {
        let &[id] = items else { return Ok(None) };
        if self.f.type_of(id) != Some("IFCTRIMMEDCURVE") {
            return Ok(None);
        }
        let e = self.ent(id, &[])?;
        let basis = Self::reference(e, 0)?;
        if self.f.type_of(basis) != Some("IFCCIRCLE") {
            return Ok(None);
        }
        let ce = self.ent(basis, &[])?;
        let cf = self.axis_placement(Self::arg(ce, 0))?;
        let trim = |i: usize| -> Result<V3, Why> {
            let sel = Self::arg(e, i).as_list().ok_or("bad trim")?;
            sel.iter()
                .find_map(|s| s.as_ref())
                .map(|r| self.point(r))
                .unwrap_or(Err("unsupported trim"))
        };
        let (a, b) = (trim(1)?, trim(2)?);
        let ang = |p: V3| (p[1] - cf.o[1]).atan2(p[0] - cf.o[0]);
        let tau = 2.0 * std::f64::consts::PI;
        let ccw = (ang(b) - ang(a)).rem_euclid(tau);
        if !(ccw > 0.0 && ccw < tau) {
            return Err("degenerate arc axis");
        }
        let sense = Self::arg(e, 3).as_enum() == Some("T");
        // a circle placed upside down runs clockwise in plan
        let ccw_dir = sense == (cf.z[2] >= 0.0);
        let sweep = if ccw_dir { ccw } else { ccw - tau };
        Ok(Some((a, b, (sweep / 4.0).tan())))
    }
    /// Rings of a profile in its own 2D frame (closing point dropped).
    fn profile_rings(&self, prof: u32) -> Result<Vec<Vec<V3>>, Why> {
        let pe = self.ent(prof, &[])?;
        let poly = |c: u32| -> Result<Vec<V3>, Why> {
            let mut pts = self.curve_points(c)?;
            if pts.len() > 1 && pts.first() == pts.last() {
                pts.pop();
            }
            Ok(pts)
        };
        match self.f.type_of(prof).unwrap_or("") {
            "IFCRECTANGLEPROFILEDEF" => {
                let pos = self.axis_placement(Self::arg(pe, 2))?;
                let xd = Self::arg(pe, 3).as_f64().ok_or("bad profile")? * self.scale;
                let yd = Self::arg(pe, 4).as_f64().ok_or("bad profile")? * self.scale;
                Ok(vec![[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
                    .iter()
                    .map(|(sx, sy)| pos.apply([sx * xd / 2.0, sy * yd / 2.0, 0.0]))
                    .collect()])
            }
            "IFCARBITRARYCLOSEDPROFILEDEF" => Ok(vec![poly(Self::reference(pe, 2)?)?]),
            "IFCARBITRARYPROFILEDEFWITHVOIDS" => {
                let mut v = vec![poly(Self::reference(pe, 2)?)?];
                for h in Self::refs(pe, 3) {
                    v.push(poly(h)?);
                }
                Ok(v)
            }
            _ => {
                let area = self.profile_area(prof, 0)?;
                match area.as_slice() {
                    [p] => Ok(p.clone()),
                    _ => Err("unsupported profile"),
                }
            }
        }
    }
    /// Single vertical extrusion of the "Body": world plan rings, bottom z, top z.
    fn vertical_prism(&mut self, e: &Entity) -> Result<(Vec<Vec<(f64, f64)>>, f64, f64), Why> {
        match self.sloped_prism(e)? {
            (rings, z0, z1, None) => Ok((rings, z0, z1)),
            _ => Err("non-vertical extrusion"),
        }
    }
    /// Plan rings and z range of a body extruded straight up or down; a profile plane tilted
    /// from horizontal also gives (point, gradient) of the top — then the range is at the point.
    #[allow(clippy::type_complexity)]
    fn sloped_prism(
        &mut self,
        e: &Entity,
    ) -> Result<
        (
            Vec<Vec<(f64, f64)>>,
            f64,
            f64,
            Option<((f64, f64), (f64, f64))>,
        ),
        Why,
    > {
        let fr = self.product_frame(e)?;
        let items = self.body_items(e)?;
        let &[mut id] = items.as_slice() else {
            return Err(if items.is_empty() {
                "no body"
            } else {
                "unsupported body"
            });
        };
        for _ in 0..32 {
            if matches!(
                self.f.type_of(id),
                Some("IFCBOOLEANCLIPPINGRESULT" | "IFCBOOLEANRESULT")
            ) {
                id = Self::reference(self.ent(id, &[])?, 1)?;
            }
        }
        let e = self
            .ent(id, &["IFCEXTRUDEDAREASOLID"])
            .map_err(|_| "unsupported body")?;
        let pos = fr.then(&self.axis_placement(Self::arg(e, 1))?);
        let d = pos.dir(self.direction(Self::arg(e, 2))?.ok_or("bad extrusion")?);
        let depth = Self::arg(e, 3).as_f64().ok_or("bad extrusion")? * self.scale;
        if d[2].abs() < 1.0 - 1e-9 || pos.z[2].abs() < 0.1 {
            return Err("non-vertical extrusion");
        }
        let rings = self.profile_rings(Self::reference(e, 0)?)?;
        let z0 = pos.o[2];
        let z1 = z0 + d[2] * depth;
        let slope = (pos.z[2].abs() < 1.0 - 1e-12).then(|| {
            let n = pos.z;
            (
                (snap(pos.o[0]), snap(pos.o[1])),
                (-n[0] / n[2], -n[1] / n[2]),
            )
        });
        let plan = rings
            .iter()
            .map(|r| {
                r.iter()
                    .map(|p| {
                        let w = pos.apply(*p);
                        (snap(w[0]), snap(w[1]))
                    })
                    .collect()
            })
            .collect();
        Ok((plan, z0.min(z1), z0.max(z1), slope))
    }
    /// World points of the body representation (tessellated when not a simple solid).
    fn body_world(&mut self, e: &Entity) -> Result<Vec<V3>, Why> {
        let fr = self.product_frame(e)?;
        let items = self.body_items(e)?;
        let mut pts = vec![];
        let mut simple = true;
        for &it in &items {
            if self.solid_points(it, &mut pts, 0).is_err() {
                simple = false;
                break;
            }
        }
        if !simple {
            pts.clear();
            let mut approx = vec![];
            for &it in &items {
                if let Ok(m) = self.item_mesh(it, 0, &mut approx) {
                    pts.extend(m.p);
                }
            }
        }
        if pts.is_empty() {
            return Err("no body");
        }
        Ok(pts.into_iter().map(|p| fr.apply(p)).collect())
    }
    /// Items of the 3D body: "Body", else "Facetation", "Body-FallBack", any other 3D
    /// representation, else "Box".
    fn body_items(&self, e: &Entity) -> Result<Vec<u32>, Why> {
        let Some(pds) = Self::arg(e, 6).as_ref() else {
            return Ok(vec![]);
        };
        let pds = self.ent(pds, &["IFCPRODUCTDEFINITIONSHAPE"])?;
        type Rep<'b> = (Option<&'b str>, Option<&'b str>, Vec<u32>);
        let reps: Vec<Rep> = Self::refs(pds, 2)
            .into_iter()
            .filter_map(|r| self.f.get(r))
            .map(|rep| {
                (
                    Self::arg(rep, 1).as_str(),
                    Self::arg(rep, 2).as_str(),
                    Self::refs(rep, 3),
                )
            })
            .collect();
        for want in ["Body", "Facetation", "Body-FallBack"] {
            if let Some(r) = reps.iter().find(|r| r.0 == Some(want) && !r.2.is_empty()) {
                return Ok(r.2.clone());
            }
        }
        const NOT_BODY: [&str; 9] = [
            "Axis",
            "FootPrint",
            "Profile",
            "Box",
            "Annotation",
            "Reference",
            "Clearance",
            "Lighting",
            "CoG",
        ];
        let solid = |t: Option<&str>| {
            !matches!(
                t,
                Some(
                    "Curve2D"
                        | "Curve3D"
                        | "Curve"
                        | "Point"
                        | "PointCloud"
                        | "Annotation2D"
                        | "GeometricSet"
                        | "GeometricCurveSet"
                )
            )
        };
        let other = reps
            .iter()
            .find(|r| !r.0.is_some_and(|i| NOT_BODY.contains(&i)) && solid(r.1) && !r.2.is_empty());
        if let Some(r) = other {
            return Ok(r.2.clone());
        }
        Ok(reps
            .iter()
            .find(|r| r.0 == Some("Box"))
            .map(|r| r.2.clone())
            .unwrap_or_default())
    }
}

fn si_factor(e: &Entity) -> Result<f64, ImportError> {
    let bad = |m: &str| ImportError::Units(m.to_string());
    if Rd::arg(e, 3).as_enum() != Some("METRE") {
        return Err(bad("length unit is not based on metre"));
    }
    let p = Rd::arg(e, 2).as_enum();
    props::si_prefix(p).ok_or_else(|| bad(&format!("prefix {}", p.unwrap_or_default())))
}

/// Metres per unit of a length unit entity `u`.
fn unit_factor(f: &StepFile, u: u32) -> Result<f64, ImportError> {
    let bad = |m: &str| ImportError::Units(m.to_string());
    let ue = f.get(u).ok_or_else(|| bad("missing unit"))?;
    match f.type_of(u) {
        Some("IFCSIUNIT") => si_factor(ue),
        Some("IFCCONVERSIONBASEDUNIT") => {
            let m = Rd::arg(ue, 3)
                .as_ref()
                .and_then(|m| f.get(m))
                .ok_or_else(|| bad("conversion without factor"))?;
            let v = Rd::arg(m, 0)
                .as_f64()
                .ok_or_else(|| bad("conversion value"))?;
            let base = Rd::arg(m, 1)
                .as_ref()
                .filter(|b| f.type_of(*b) == Some("IFCSIUNIT"))
                .ok_or_else(|| bad("conversion base unit"))?;
            let k = v * si_factor(f.get(base).ok_or_else(|| bad("missing entity"))?)?;
            if !(k > 0.0 && k.is_finite()) {
                return Err(bad("conversion factor"));
            }
            Ok(k)
        }
        Some(t) => Err(bad(&format!("unsupported {t}"))),
        None => Err(bad("missing unit")),
    }
}

/// Geometry/relationship resources that are never placed products (skips typing them).
fn is_resource(t: &str) -> bool {
    t.starts_with("IFCREL")
        || matches!(
            t,
            "IFCCARTESIANPOINT"
                | "IFCDIRECTION"
                | "IFCPOLYLINE"
                | "IFCPOLYLOOP"
                | "IFCFACE"
                | "IFCFACEOUTERBOUND"
                | "IFCFACEBOUND"
                | "IFCCARTESIANPOINTLIST3D"
                | "IFCTRIANGULATEDFACESET"
                | "IFCPOLYGONALFACESET"
                | "IFCINDEXEDPOLYGONALFACE"
                | "IFCAXIS2PLACEMENT3D"
                | "IFCAXIS2PLACEMENT2D"
                | "IFCLOCALPLACEMENT"
                | "IFCSHAPEREPRESENTATION"
                | "IFCPRODUCTDEFINITIONSHAPE"
                | "IFCPROPERTYSINGLEVALUE"
                | "IFCPROPERTYSET"
        )
}

fn is_wall(t: &str) -> bool {
    matches!(
        t,
        "IFCWALL" | "IFCWALLSTANDARDCASE" | "IFCWALLELEMENTEDCASE"
    )
}
fn is_opening(t: &str) -> bool {
    matches!(t, "IFCOPENINGELEMENT" | "IFCOPENINGSTANDARDCASE")
}
fn fill_kind(t: &str) -> Option<&'static str> {
    match t {
        "IFCDOOR" | "IFCDOORSTANDARDCASE" => Some("door"),
        "IFCWINDOW" | "IFCWINDOWSTANDARDCASE" => Some("window"),
        _ => None,
    }
}

/// Products that never become meshes: voids, spaces, spatial structure, annotation, ports.
fn never_mesh(t: &str) -> bool {
    is_opening(t)
        || t.starts_with("IFCALIGNMENT")
        || t.starts_with("IFCSTRUCTURAL")
        || matches!(
            t,
            "IFCSPACE"
                | "IFCVIRTUALELEMENT"
                | "IFCGRID"
                | "IFCANNOTATION"
                | "IFCPROJECT"
                | "IFCBUILDING"
                | "IFCBUILDINGSTOREY"
                | "IFCDISTRIBUTIONPORT"
                | "IFCSPATIALZONE"
                | "IFCEXTERNALSPATIALELEMENT"
                | "IFCFACILITY"
                | "IFCFACILITYPART"
                | "IFCFACILITYPARTCOMMON"
                | "IFCBRIDGE"
                | "IFCBRIDGEPART"
                | "IFCROAD"
                | "IFCROADPART"
                | "IFCRAILWAY"
                | "IFCRAILWAYPART"
                | "IFCMARINEFACILITY"
                | "IFCMARINEPART"
        )
}

/// Classes with a native category in the model: as a mesh they are listed with a reason.
fn has_native(t: &str) -> bool {
    is_wall(t)
        || is_slab(t)
        || fill_kind(t).is_some()
        || member_kind(t).is_some()
        || matches!(
            t,
            "IFCROOF"
                | "IFCCOVERING"
                | "IFCCURTAINWALL"
                | "IFCSTAIR"
                | "IFCSTAIRFLIGHT"
                | "IFCRAMP"
                | "IFCRAMPFLIGHT"
                | "IFCRAILING"
                | "IFCFOOTING"
        )
}

/// Column (true) or beam-like member (false).
fn member_kind(t: &str) -> Option<bool> {
    match t {
        "IFCCOLUMN" | "IFCCOLUMNSTANDARDCASE" => Some(true),
        "IFCBEAM" | "IFCBEAMSTANDARDCASE" | "IFCMEMBER" | "IFCMEMBERSTANDARDCASE" => Some(false),
        _ => None,
    }
}

/// Neutral display category of an entity type.
pub fn category(t: &str) -> String {
    let b = t.strip_prefix("IFC").unwrap_or(t);
    let b = b
        .strip_suffix("STANDARDCASE")
        .or_else(|| b.strip_suffix("ELEMENTEDCASE"))
        .unwrap_or(b);
    const KNOWN: [(&str, &str); 42] = [
        ("WALL", "Wall"),
        ("SLAB", "Slab"),
        ("ROOF", "Roof"),
        ("COLUMN", "Column"),
        ("BEAM", "Beam"),
        ("MEMBER", "Member"),
        ("PLATE", "Plate"),
        ("DOOR", "Door"),
        ("WINDOW", "Window"),
        ("STAIR", "Stair"),
        ("STAIRFLIGHT", "Stair flight"),
        ("RAMP", "Ramp"),
        ("RAMPFLIGHT", "Ramp flight"),
        ("RAILING", "Railing"),
        ("COVERING", "Covering"),
        ("CURTAINWALL", "Curtain wall"),
        ("FURNISHINGELEMENT", "Furniture"),
        ("FURNITURE", "Furniture"),
        ("SYSTEMFURNITUREELEMENT", "Furniture"),
        ("BUILDINGELEMENTPROXY", "Generic"),
        ("FOOTING", "Footing"),
        ("PILE", "Pile"),
        ("SITE", "Site"),
        ("CHIMNEY", "Chimney"),
        ("SHADINGDEVICE", "Shading device"),
        ("GEOGRAPHICELEMENT", "Geographic"),
        ("FLOWSEGMENT", "MEP segment"),
        ("PIPESEGMENT", "Pipe"),
        ("DUCTSEGMENT", "Duct"),
        ("CABLECARRIERSEGMENT", "Cable tray"),
        ("CABLESEGMENT", "Cable"),
        ("PIPEFITTING", "Pipe fitting"),
        ("DUCTFITTING", "Duct fitting"),
        ("FLOWFITTING", "MEP fitting"),
        ("FLOWTERMINAL", "MEP terminal"),
        ("REINFORCINGBAR", "Rebar"),
        ("REINFORCINGMESH", "Rebar mesh"),
        ("TRANSPORTELEMENT", "Transport"),
        ("BUILDINGELEMENTPART", "Part"),
        ("ELEMENTASSEMBLY", "Assembly"),
        ("DISCRETEACCESSORY", "Accessory"),
        ("MECHANICALFASTENER", "Fastener"),
    ];
    if let Some((_, c)) = KNOWN.iter().find(|k| k.0 == b) {
        return c.to_string();
    }
    let mut s = b.to_ascii_lowercase();
    if let Some(c) = s.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    if s.is_empty() {
        "Generic".into()
    } else {
        s
    }
}

/// Reads a parsed file into a plan (no model access).
pub fn plan(f: &StepFile) -> Result<Plan, ImportError> {
    let schema = f.schema.first().cloned().unwrap_or_default();
    let up = schema.to_ascii_uppercase();
    if !(up.starts_with("IFC2X3") || up.starts_with("IFC4")) {
        return Err(ImportError::Schema(schema));
    }
    let units = props::units(f)?;
    let scale = units.length;
    let mut pl = Plan::default();
    pl.report.schema = schema;
    pl.report.length_unit = scale;
    pl.report.warnings.extend(f.warnings.iter().cloned());
    pl.materials = matdef::read(f);
    let mut rd = Rd {
        f,
        scale,
        angle: units.angle,
        frames: HashMap::new(),
        maps: HashMap::new(),
    };

    // relations
    let mut contained: HashMap<u32, u32> = HashMap::new();
    let mut voids: HashMap<u32, u32> = HashMap::new();
    let mut fills: HashMap<u32, u32> = HashMap::new();
    let mut filled_by: HashMap<u32, u32> = HashMap::new();
    let mut material: HashMap<u32, u32> = HashMap::new();
    let mut typed: HashMap<u32, u32> = HashMap::new();
    let mut aggregated: HashMap<u32, u32> = HashMap::new();
    let mut props: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut classif: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut bounds: Vec<(Option<u32>, Option<u32>)> = vec![];
    for (id, ty) in f.iter().filter(|x| x.1.starts_with("IFCREL")) {
        let Some(a) = f.leaf(id) else { continue };
        let e: &[Value] = &a;
        let (one, many) = (Rd::at(e, 5).as_ref(), Rd::refs_at(e, 4));
        match ty {
            "IFCRELCONTAINEDINSPATIALSTRUCTURE" => {
                if let Some(s) = one {
                    for x in many {
                        contained.insert(x, s);
                    }
                }
            }
            "IFCRELASSOCIATESMATERIAL" => {
                if let Some(m) = one {
                    for x in many {
                        material.insert(x, m);
                    }
                }
            }
            "IFCRELASSOCIATESCLASSIFICATION" => {
                if let Some(c) = one {
                    for x in many {
                        classif.entry(x).or_default().push(c);
                    }
                }
            }
            "IFCRELAGGREGATES" => {
                if let Some(s) = Rd::at(e, 4).as_ref() {
                    for x in Rd::refs_at(e, 5) {
                        aggregated.insert(x, s);
                    }
                }
            }
            "IFCRELDEFINESBYPROPERTIES" => {
                // IFC4 allows a set of property set definitions
                let qs = match one {
                    Some(q) => vec![q],
                    None => Rd::refs_at(e, 5),
                };
                for x in many {
                    props.entry(x).or_default().extend(qs.iter().copied());
                }
            }
            "IFCRELDEFINESBYTYPE" => {
                if let Some(t) = one {
                    for x in many {
                        typed.insert(x, t);
                    }
                }
            }
            "IFCRELSPACEBOUNDARY" => {
                bounds.push((Rd::at(e, 4).as_ref(), one));
            }
            "IFCRELVOIDSELEMENT" | "IFCRELFILLSELEMENT" => {
                if let (Some(a), Some(b)) = (Rd::at(e, 4).as_ref(), one) {
                    if ty == "IFCRELVOIDSELEMENT" {
                        voids.insert(b, a);
                    } else {
                        fills.insert(a, b);
                        filled_by.insert(b, a);
                    }
                }
            }
            _ => {}
        }
    }
    // round-trip elements keep their own path
    let ddc: HashSet<u32> = props
        .iter()
        .filter(|(_, qs)| {
            qs.iter().any(|q| {
                f.leaf(*q).is_some_and(|pe| {
                    matches!(
                        Rd::at(&pe, 2).as_str(),
                        Some("DDC_Component" | "DDC_MepRun" | "DDC_MepFitting" | "DDC_MepPort" | "DDC_Cable")
                    )
                })
            })
        })
        .map(|(id, _)| *id)
        .collect();
    let ctx = props::Ctx {
        f,
        u: &units,
        props: &props,
        typed: &typed,
        material: &material,
        classif: &classif,
    };
    let guid_of = |id: u32| -> String {
        f.get(id)
            .and_then(|e| Rd::arg(e, 0).as_str())
            .unwrap_or_default()
            .to_string()
    };
    let link = |pl: &mut Plan, kind: &'static str, index: usize, id: u32, guid: String| {
        let props = ctx.element(id);
        pl.elems.push(PElem {
            kind,
            index,
            guid,
            props,
        });
    };

    // storeys
    let mut storey_ix: HashMap<u32, usize> = HashMap::new();
    let mut cand: Vec<(u32, Option<f64>, Option<f64>)> = vec![];
    for (id, ty) in f.iter() {
        if ty == "IFCPROJECT" && pl.project_guid.is_empty() {
            pl.project_guid = guid_of(id);
        }
        if ty == "IFCSITE" && pl.site_guid.is_empty() {
            pl.site_guid = guid_of(id);
        }
        if ty == "IFCBUILDING" && pl.building_guid.is_empty() {
            pl.building_guid = guid_of(id);
        }
        if ty != "IFCBUILDINGSTOREY" {
            continue;
        }
        let Some(e) = f.get(id) else { continue };
        let placed = match Rd::arg(e, 5) {
            Value::Null => None,
            _ => rd.product_frame(e).ok().map(|fr| fr.o[2]),
        };
        let attr = Rd::arg(e, 9).as_f64().map(|v| v * scale);
        let ok = |v: Option<f64>| v.filter(|x| x.is_finite());
        cand.push((id, ok(placed), ok(attr)));
    }
    let elev_mode = storey_elevation_mode(&cand);
    if elev_mode == ElevMode::Attr {
        pl.report.warnings.push(
            "storeys: placements are flat, levels taken from the Elevation attribute".into(),
        );
    } else if elev_mode == ElevMode::PlacementWarn {
        pl.report
            .warnings
            .push("storeys: placement and Elevation disagree, placement used".into());
    }
    for (id, placed, attr) in cand {
        let Some(e) = f.get(id) else { continue };
        let ty = "IFCBUILDINGSTOREY";
        let elev = match elev_mode {
            ElevMode::Attr => attr.or(placed),
            _ => placed.or(attr),
        };
        let Some(elev) = elev.filter(|v| v.is_finite()) else {
            pl.report.skip(ty, "bad placement");
            continue;
        };
        let name = Rd::arg(e, 2)
            .as_str()
            .filter(|s| !s.is_empty())
            .map_or_else(|| format!("Level {}", pl.levels.len() + 1), str::to_string);
        storey_ix.insert(id, pl.levels.len());
        pl.elems.push(PElem {
            kind: "level",
            index: pl.levels.len(),
            guid: guid_of(id),
            props: vec![],
        });
        pl.levels.push(PLevel {
            name,
            elevation: snap(elev),
        });
    }
    pl.project_props = props::georef(f, &units);
    pl.project_info = project::read(f);
    pl.site_crs = props::site_crs(f, &units);
    // storey of a product: its container, else the container of the assembly it belongs to
    let storey_of = |id: u32| -> Option<usize> {
        let mut cur = id;
        for _ in 0..16 {
            if let Some(s) = contained.get(&cur).and_then(|s| storey_ix.get(s)) {
                return Some(*s);
            }
            if let Some(s) = storey_ix.get(&cur) {
                return Some(*s);
            }
            cur = *aggregated.get(&cur)?;
        }
        None
    };
    let mut done: HashSet<u32> = HashSet::new();
    let mut why_native: HashMap<u32, Why> = HashMap::new();

    // generic solids of this exporter, before any class-based path reads them natively
    for (id, ty) in f.iter() {
        if !props.contains_key(&id) || is_resource(ty) || ty == "IFCOPENINGELEMENT" {
            continue;
        }
        let Some(kv) = pset(f, &props, id, "DDC_GenericSolid") else { continue };
        let Some(e) = f.get(id) else { continue };
        let n = pl.solids.len();
        match gsolid::read(&mut pl, scale, ty, e, &kv, storey_of(id)) {
            Ok(()) => {
                link(&mut pl, "solid", n, id, guid_of(id));
                done.insert(id);
            }
            Err(why) => pl.report.warnings.push(format!("#{id}: {why}")),
        }
    }

    // grids
    for (id, ty) in f.iter() {
        if ty != "IFCGRID" {
            continue;
        }
        let Some(e) = f.get(id) else { continue };
        let n = pl.grids.len();
        match grid(&mut rd, &mut pl, &props, storey_of(id), id, e) {
            Ok(()) => {
                let g = guid_of(id);
                for k in n..pl.grids.len() {
                    let gk = if k == n {
                        g.clone()
                    } else {
                        format!("{g}#{}", k - n)
                    };
                    link(&mut pl, "grid", k, id, gk);
                }
            }
            Err(why) => pl.report.skip(ty, why),
        }
    }
    let grid_order: Vec<usize> = {
        let mut v: Vec<usize> = (0..pl.grids.len()).collect();
        v.sort_by_key(|&i| pl.grids[i].ord);
        v
    };
    let mut new_pos = vec![0; grid_order.len()];
    for (p, &i) in grid_order.iter().enumerate() {
        new_pos[i] = p;
    }
    pl.grids = grid_order.iter().map(|&i| pl.grids[i].clone()).collect();
    for el in pl.elems.iter_mut().filter(|x| x.kind == "grid") {
        el.index = new_pos[el.index];
    }

    // walls
    let mut type_ix: HashMap<(u32, bool), usize> = HashMap::new();
    let mut wall_ix: HashMap<u32, (Vec<usize>, Frame, f64)> = HashMap::new();
    for (id, ty) in f.iter() {
        if !is_wall(ty) {
            continue;
        }
        let Some(e) = f.get(id) else { continue };
        let st = contained
            .get(&id)
            .and_then(|s| storey_ix.get(s))
            .copied()
            .or_else(|| storey_of(id));
        match wall(&mut rd, &mut pl, &mut type_ix, st, &ctx, id, e) {
            Ok(v) => {
                let g = guid_of(id);
                for (k, &w) in v.0.iter().enumerate() {
                    let gk = if k == 0 {
                        g.clone()
                    } else {
                        format!("{g}#{k}")
                    };
                    link(&mut pl, "wall", w, id, gk);
                }
                wall_ix.insert(id, v);
                done.insert(id);
            }
            Err(why) => {
                why_native.insert(id, why);
            }
        }
    }

    // curtain walls with their aggregated parts
    let mut parts_of: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&child, &parent) in &aggregated {
        parts_of.entry(parent).or_default().push(child);
    }
    for (id, ty) in f.iter() {
        if ty != "IFCCURTAINWALL" {
            continue;
        }
        let Some(e) = f.get(id) else { continue };
        let mut parts = parts_of.get(&id).cloned().unwrap_or_default();
        parts.sort_unstable();
        let n = pl.curtains.len();
        match curtain(&mut rd, &mut pl, storey_of(id), e, &parts) {
            Ok(()) => {
                link(&mut pl, "curtain_wall", n, id, guid_of(id));
                done.insert(id);
                done.extend(parts);
            }
            Err(why) => {
                why_native.insert(id, why);
            }
        }
    }

    // openings; those voiding a slab or roof are cut once the hosts are read
    let mut slab_voids: Vec<(u32, u32)> = vec![];
    for (id, ty) in f.iter() {
        if !is_opening(ty) {
            continue;
        }
        let Some(e) = f.get(id) else { continue };
        let host = voids.get(&id).copied();
        let Some((wis, _, base_z)) = host.and_then(|h| wall_ix.get(&h)) else {
            let slab_or_roof =
                |h: &u32| f.type_of(*h).is_some_and(|t| is_slab(t) || t == "IFCROOF");
            if let Some(h) = host.filter(slab_or_roof) {
                slab_voids.push((id, h));
                continue;
            }
            let why = match host {
                Some(h) if f.type_of(h).is_some_and(is_wall) => "host wall imported as mesh",
                Some(_) => "host is not a wall (not cut)",
                None => "host wall not imported",
            };
            pl.report.skip(ty, why);
            continue;
        };
        let filler = fills.get(&id).copied();
        let kind = filler
            .and_then(|x| f.type_of(x))
            .and_then(fill_kind)
            .unwrap_or("void");
        let base_z = *base_z;
        // a split wall hosts the opening on the first segment that contains it
        // past every segment: the one it overlaps most (cut back later in `fit_openings`)
        let mut found = Err("outside host wall");
        let mut best = 0.0;
        for &wi in wis {
            match opening(&mut rd, &pl.walls[wi], base_z, e) {
                Ok(r) => {
                    let len = axis_len(&pl.walls[wi]);
                    if r.0 >= -1e-9 && r.0 + r.1 <= len + 1e-9 {
                        found = Ok((wi, r));
                        break;
                    }
                    let overlap = (r.0 + r.1).min(len) - r.0.max(0.0);
                    if overlap > best {
                        best = overlap;
                        found = Ok((wi, r));
                    }
                }
                Err(w) if found.is_err() => found = Err(w),
                Err(_) => {}
            }
        }
        match found {
            Ok((wi, (along, width, sill, height))) => {
                if let Some(x) = filler {
                    if kind != "void" {
                        done.insert(x);
                    }
                }
                let swing = match (kind, filler) {
                    ("door", Some(x)) => door_swing(&mut rd, &pl.walls[wi], x),
                    _ => (false, false),
                };
                let n = pl.openings.len();
                pl.openings.push(POpening {
                    wall: wi,
                    along,
                    width,
                    sill,
                    height,
                    kind: kind.into(),
                    swing,
                });
                link(&mut pl, "opening", n, id, guid_of(id));
                if let (Some(x), true) = (filler, kind != "void") {
                    let extra = ctx.element(x);
                    if let Some(el) = pl.elems.last_mut() {
                        el.props.retain(|p| !extra.iter().any(|q| q.key == p.key));
                        el.props.push(PProp {
                            key: "IFC.OpeningGlobalId".into(),
                            value: PVal::Text(guid_of(id)),
                            unit: String::new(),
                        });
                        el.props.extend(extra);
                    }
                    // the door/window keeps its own GlobalId, apart from the opening element's
                    pl.elems.push(PElem {
                        kind: "opening_fill",
                        index: n,
                        guid: guid_of(x),
                        props: vec![],
                    });
                }
            }
            Err(why) => pl.report.skip(ty, why),
        }
    }

    // slabs, roofs, ceilings, members, spaces, separators
    let mut slab_ix: HashMap<u32, usize> = HashMap::new();
    let mut slab_ent: HashMap<u32, usize> = HashMap::new();
    let mut roof_ent: HashMap<u32, usize> = HashMap::new();
    let mut room_ix: HashMap<u32, usize> = HashMap::new();
    let mut sep_ix: HashMap<u32, usize> = HashMap::new();
    let is_roof_part = |id: u32, e: &Entity| {
        Rd::arg(e, 8).as_enum() == Some("ROOF")
            || aggregated
                .get(&id)
                .is_some_and(|p| f.type_of(*p) == Some("IFCROOF"))
    };
    // landings and stringers of a stair (or of a ramp we wrote) are its parts, not slabs and beams
    let stair_part: HashSet<u32> = aggregated
        .iter()
        .filter(|(_, p)| match f.type_of(**p) {
            Some("IFCSTAIR") => true,
            Some("IFCRAMP") => {
                pset(f, &props, **p, "DDC_Ramp").is_some_and(|k| k.contains_key("Spec"))
            }
            _ => false,
        })
        .map(|(c, _)| *c)
        .collect();
    for (id, ty) in f.iter() {
        let interesting = is_slab(ty)
            || ty == "IFCROOF"
            || ty == "IFCCOVERING"
            || ty == "IFCSPACE"
            || ty == "IFCVIRTUALELEMENT"
            || ty == "IFCFOOTING"
            || member_kind(ty).is_some();
        if !interesting || done.contains(&id) || ddc.contains(&id) || stair_part.contains(&id) {
            continue;
        }
        let Some(e) = f.get(id) else { continue };
        let st = storey_of(id);
        if ty == "IFCROOF" {
            if let Some(kv) = pset(f, &props, id, "DDC_Roof") {
                let n = pl.roofs.len();
                match exact_roof(&rd, &mut pl, &mut slab_ix, st, &ctx, id, &kv) {
                    Ok(()) => {
                        link(&mut pl, "roof", n, id, guid_of(id));
                        roof_ent.insert(id, n);
                        done.insert(id);
                        continue;
                    }
                    Err(why) => pl.report.warnings.push(format!("#{id}: {why}")),
                }
            }
            let has_parts = parts_of.get(&id).is_some_and(|ps| {
                ps.iter()
                    .any(|p| f.get(*p).is_some_and(|x| Rd::arg(x, 6).as_ref().is_some()))
            });
            if has_parts {
                continue;
            }
            let n = pl.roofs.len();
            match roof_part(&mut rd, &mut pl, &mut slab_ix, st, &ctx, id, e) {
                Ok(()) => {
                    link(&mut pl, "roof", n, id, guid_of(id));
                    roof_ent.insert(id, n);
                    done.insert(id);
                }
                Err(why) => {
                    why_native.insert(id, why);
                }
            }
            continue;
        }
        if is_slab(ty) {
            if is_roof_part(id, e) {
                let n = pl.roofs.len();
                if roof_part(&mut rd, &mut pl, &mut slab_ix, st, &ctx, id, e).is_ok() {
                    link(&mut pl, "roof", n, id, guid_of(id));
                    roof_ent.insert(id, n);
                    done.insert(id);
                    continue;
                }
            }
            let n = pl.slabs.len();
            match slab(&mut rd, &mut pl, &mut slab_ix, st, &ctx, id, e) {
                Ok(()) => {
                    slab_ent.insert(id, n);
                    link(&mut pl, "slab", n, id, guid_of(id));
                    done.insert(id);
                }
                Err(why) => {
                    why_native.insert(id, why);
                }
            }
            continue;
        }
        if ty == "IFCFOOTING" {
            let (ni, ns) = (pl.footings.len(), pl.strip_footings.len());
            match footing(&mut rd, &mut pl, st, &ctx, id, e) {
                Ok(()) if pl.footings.len() > ni => {
                    link(&mut pl, "iso_footing", ni, id, guid_of(id));
                    done.insert(id);
                }
                Ok(()) if pl.strip_footings.len() > ns => {
                    link(&mut pl, "strip_footing", ns, id, guid_of(id));
                    done.insert(id);
                }
                Ok(()) => {}
                Err(why) => {
                    why_native.insert(id, why);
                }
            }
            continue;
        }
        if ty == "IFCCOVERING" {
            let ceil = Rd::arg(e, 8).as_enum() == Some("CEILING")
                || typed
                    .get(&id)
                    .and_then(|t| f.get(*t))
                    .is_some_and(|t| Rd::arg(t, 9).as_enum() == Some("CEILING"));
            if !ceil {
                why_native.insert(id, "covering is not a ceiling");
                continue;
            }
            let n = pl.ceilings.len();
            match ceiling(&mut rd, &mut pl, &mut slab_ix, st, &ctx, id, e) {
                Ok(()) => {
                    link(&mut pl, "ceiling", n, id, guid_of(id));
                    done.insert(id);
                }
                Err(why) => {
                    why_native.insert(id, why);
                }
            }
            continue;
        }
        if let Some(column) = member_kind(ty) {
            let n = pl.members.len();
            let exact = pset(f, &props, id, "DDC_Member").map(|kv| exact_member(&mut pl, st, &kv));
            if let Some(Err(why)) = exact {
                pl.report.warnings.push(format!("#{id}: {why}"));
            }
            let r = match exact {
                Some(Ok(())) => Ok(()),
                _ => member(&mut rd, &mut pl, st, column, &ctx, id, e),
            };
            match r {
                Ok(()) => {
                    let kind = if pl.members[n].column { "col" } else { "beam" };
                    link(&mut pl, kind, n, id, guid_of(id));
                    done.insert(id);
                }
                Err(why) => {
                    why_native.insert(id, why);
                }
            }
            continue;
        }
        let r = if ty == "IFCSPACE" {
            let h = props
                .get(&id)
                .and_then(|qs| qs.iter().find_map(|q| quantity(f, q, "Height", scale)));
            let n = pl.rooms.len();
            let r = space(&mut rd, &mut pl, st, h, e);
            if pl.rooms.len() > n {
                room_ix.insert(id, n);
                link(&mut pl, "room", n, id, guid_of(id));
            }
            r
        } else {
            let n = pl.seps.len();
            let r = separator(&mut rd, &mut pl, st, e);
            if pl.seps.len() > n {
                sep_ix.insert(id, n);
                link(&mut pl, "room_sep", n, id, guid_of(id));
            }
            r
        };
        if let Err(why) = r {
            pl.report.skip(ty, why);
        }
    }
    for (id, h) in slab_voids {
        let (Some(e), Some(ty)) = (f.get(id), f.type_of(id)) else { continue };
        let hosts: Vec<host_voids::Host> = std::iter::once(h)
            .chain(parts_of.get(&h).into_iter().flatten().copied())
            .filter_map(|x| {
                slab_ent
                    .get(&x)
                    .map(|k| host_voids::Host::Slab(*k))
                    .or_else(|| roof_ent.get(&x).map(|k| host_voids::Host::Roof(*k)))
            })
            .collect();
        if hosts.is_empty() {
            host_voids::not_cut(&mut pl, id, ty, "host slab or roof imported as mesh");
            continue;
        }
        host_voids::cut(&mut rd, &mut pl, id, ty, e, &hosts);
    }
    for (sp, el) in bounds {
        let (Some(sp), Some(el)) = (sp, el) else {
            pl.report
                .skip("IFCRELSPACEBOUNDARY", "missing space or element reference");
            continue;
        };
        let target = wall_ix
            .get(&el)
            .and_then(|w| w.0.first())
            .map(|&w| PTarget::Wall(w))
            .or_else(|| sep_ix.get(&el).map(|&x| PTarget::Sep(x)));
        match (room_ix.get(&sp), target) {
            (Some(&room), Some(target)) => pl.boundaries.push(PBoundary { room, target }),
            (None, _) => pl.report.skip("IFCRELSPACEBOUNDARY", "space not imported"),
            _ => pl
                .report
                .skip("IFCRELSPACEBOUNDARY", "element not imported"),
        }
    }

    // round-trip components and networks
    let mut hosts: HashMap<&str, PCompHost> = HashMap::new();
    for (e, (wis, _, _)) in &wall_ix {
        if let (Some(g), Some(&w)) = (f.get(*e).and_then(|x| Rd::arg(x, 0).as_str()), wis.first()) {
            hosts.insert(g, PCompHost::Wall(w));
        }
    }
    for (e, &k) in &slab_ent {
        if let Some(g) = f.get(*e).and_then(|x| Rd::arg(x, 0).as_str()) {
            hosts.insert(g, PCompHost::Slab(k));
        }
    }
    // port owners for the connections read after the loop
    let mut run_ent: HashMap<u32, usize> = HashMap::new();
    let mut comp_ent: HashMap<u32, usize> = HashMap::new();
    let mut fitting_ent: HashSet<u32> = HashSet::new();
    for (id, ty) in f.iter() {
        if is_resource(ty) || done.contains(&id) {
            continue;
        }
        if let Some(kv) = pset(f, &props, id, "DDC_MepRun") {
            done.insert(id);
            match mep_run(&kv, storey_of(id)) {
                Ok(m) => {
                    let n = pl.meps.len();
                    pl.meps.push(m);
                    run_ent.insert(id, n);
                    link(&mut pl, "mep_run", n, id, guid_of(id));
                }
                Err(why) => pl.report.skip(ty, why),
            }
            continue;
        }
        if pset(f, &props, id, "DDC_MepFitting").is_some() {
            done.insert(id);
            fitting_ent.insert(id);
            continue;
        }
        // a cable piece comes back with its circuit
        if pset(f, &props, id, "DDC_Cable").is_some() {
            done.insert(id);
            continue;
        }
        if let Some(kv) = pset(f, &props, id, "DDC_Component") {
            done.insert(id);
            match component(&kv, storey_of(id), &hosts) {
                Ok(c) => {
                    let n = pl.comps.len();
                    pl.comps.push(c);
                    comp_ent.insert(id, n);
                    link(&mut pl, "comp", n, id, guid_of(id));
                }
                Err(why) => pl.report.skip(ty, why),
            }
        }
    }

    stairs::read(
        &mut rd,
        &mut pl,
        &stairs::Cx {
            f,
            props: &props,
            parts_of: &parts_of,
            aggregated: &aggregated,
        },
        &storey_of,
        &guid_of,
        &link,
        &mut done,
        &mut why_native,
    );

    // every other product with geometry: a type's family instance, else a generic mesh
    let mut type_fams = types::Types::default();
    for (id, ty) in f.iter() {
        if is_resource(ty) || done.contains(&id) || never_mesh(ty) {
            continue;
        }
        // most of what is left is geometry and resources: look before pinning them decoded
        let rooted = f
            .leaf(id)
            .is_some_and(|a| a.len() >= 7 && Rd::at(&a, 0).as_str().is_some());
        if !rooted {
            continue;
        }
        let Some(e) = f.get(id) else { continue };
        let decomposed = parts_of.get(&id).is_some_and(|ps| {
            ps.iter()
                .any(|p| f.get(*p).is_some_and(|x| Rd::arg(x, 6).as_ref().is_some()))
        });
        if decomposed {
            // the parts carry the geometry; an own body would display twice
            if Rd::arg(e, 6).as_ref().is_some() {
                pl.report.skip(ty, "decomposed into parts");
            }
            continue;
        }
        let product = e.args().len() >= 7
            && Rd::arg(e, 0).as_str().is_some()
            && matches!(Rd::arg(e, 5), Value::Null | Value::Ref(_))
            && Rd::arg(e, 6)
                .as_ref()
                .is_some_and(|r| f.type_of(r) == Some("IFCPRODUCTDEFINITIONSHAPE"));
        if !product {
            // a site without terrain is pure spatial structure; with one it becomes a mesh
            if ty != "IFCSITE"
                && Rd::arg(e, 6).as_ref().is_none()
                && e.args().len() >= 7
                && Rd::arg(e, 0).as_str().is_some_and(|g| g.len() == 22)
                && Rd::arg(e, 5).as_ref().is_some_and(|p| {
                    f.type_of(p)
                        .is_some_and(|t| t.ends_with("PLACEMENT") && t != "IFCAXIS2PLACEMENT3D")
                })
            {
                let why = match (fill_kind(ty), filled_by.contains_key(&id)) {
                    (Some(_), true) => "opening not imported",
                    (Some(_), false) => "no opening",
                    _ => why_native.get(&id).copied().unwrap_or("no body"),
                };
                pl.report.skip(ty, why);
            }
            continue;
        }
        match type_fams.instance(&mut rd, e, ty, typed.get(&id).copied()) {
            Some(Ok(t)) => {
                let level = level_for(&mut pl, storey_of(id), t.at[2]);
                let z = t.at[2] - pl.levels[level].elevation;
                let frame = ([t.at[0], t.at[1], z, t.rotation], false);
                let n = pl.comps.len();
                pl.comps.push(PComp {
                    def: t.fam.def,
                    type_name: t.fam.type_name,
                    type_vals: t.fam.type_vals,
                    vals: String::new(),
                    level,
                    local: frame,
                    world: frame,
                    host: PCompHost::None,
                });
                pl.report.type_families += 1;
                link(&mut pl, "comp", n, id, guid_of(id));
                continue;
            }
            Some(Err(why)) => {
                *pl.report.as_mesh.entry(format!("{ty}: {why}")).or_default() += 1;
            }
            None => {}
        }
        match product_mesh(&mut rd, e) {
            Ok((m, approx)) => {
                for a in approx {
                    *pl.report
                        .approximated
                        .entry(format!("{ty}: {a}"))
                        .or_default() += 1;
                }
                let why = why_native.get(&id).copied().or_else(|| {
                    let host = filled_by.get(&id).and_then(|o| voids.get(o));
                    match (fill_kind(ty), host.and_then(|h| f.type_of(*h))) {
                        (Some(_), Some(h)) if is_slab(h) || h == "IFCROOF" => {
                            Some("fills a slab or roof opening; doors and windows are wall-hosted")
                        }
                        (Some(_), _) => Some("opening not imported"),
                        _ if stair_part.contains(&id) => Some("part of a stair kept as a mesh"),
                        _ if has_native(ty) => Some("not read natively"),
                        _ => None,
                    }
                });
                if let Some(w) = why {
                    *pl.report.as_mesh.entry(format!("{ty}: {w}")).or_default() += 1;
                }
                let name = [2usize, 4]
                    .iter()
                    .find_map(|&i| Rd::arg(e, i).as_str().filter(|s| !s.is_empty()))
                    .map(str::to_string)
                    .or_else(|| {
                        typed
                            .get(&id)
                            .and_then(|t| f.get(*t))
                            .and_then(|t| Rd::arg(t, 2).as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| category(ty));
                let zmin = m.bbox().map_or(0.0, |b| b.0[2]);
                let level = level_for(&mut pl, storey_of(id), zmin);
                let n = pl.meshes.len();
                let cat = pset(f, &props, id, "DDC_Mesh")
                    .and_then(|kv| kv.get("Category").cloned())
                    .unwrap_or_else(|| category(ty));
                pl.meshes.push(PMesh {
                    level,
                    category: cat,
                    name,
                    positions: m.p,
                    indices: m.t.into_iter().flatten().collect(),
                });
                link(&mut pl, "mesh", n, id, guid_of(id));
            }
            Err(why) => {
                let w = why_native.get(&id).copied();
                let why = match (fill_kind(ty), filled_by.contains_key(&id)) {
                    (Some(_), true) => "opening not imported",
                    _ => w.unwrap_or(why),
                };
                pl.report.skip(ty, why);
            }
        }
    }
    let known = mep_links::Known {
        runs: &run_ent,
        comps: &comp_ent,
        fittings: &fitting_ent,
    };
    pl.mep_links = mep_links::read(&mut rd, &props, &known, &pl.meps, &mut pl.report.warnings);
    pl.report.mep_links = pl.mep_links.len();
    pl.circuits = circuits::read(f, &props, &comp_ent, &mut pl.report.warnings);
    fit_openings(&mut pl);
    pl.report.levels = pl.levels.len();
    pl.report.wall_types = pl.types.len();
    pl.report.walls = pl.walls.len();
    pl.report.openings = pl.openings.iter().filter(|o| o.width > 0.0).count();
    pl.report.slabs = pl.slabs.len();
    pl.report.curtain_walls = pl.curtains.len();
    pl.report.rooms = pl.rooms.len();
    pl.report.separators = pl.seps.len();
    pl.report.space_boundaries = pl.boundaries.len();
    pl.report.grids = pl.grids.len();
    pl.report.components = pl.comps.len();
    pl.report.mep_runs = pl.meps.len();
    pl.report.meshes = pl.meshes.len();
    pl.report.columns = pl.members.iter().filter(|m| m.column).count();
    pl.report.beams = pl.members.len() - pl.report.columns;
    pl.report.roofs = pl.roofs.len();
    pl.report.ceilings = pl.ceilings.len();
    pl.report.stairs = pl.stairs.len();
    pl.report.ramps = pl.ramps.len();
    pl.report.railings = pl.railings.len();
    pl.report.footings = pl.footings.len() + pl.strip_footings.len();
    Ok(pl)
}

/// World mesh of a product's body representation.
fn product_mesh(rd: &mut Rd, e: &Entity) -> Result<(tess::Tri, Vec<Why>), Why> {
    let fr = rd.product_frame(e)?;
    let items = rd.body_items(e)?;
    if items.is_empty() {
        return Err("no body");
    }
    let mut approx = vec![];
    let mut m = tess::Tri::default();
    let mut last = "empty body";
    for it in items {
        match rd.item_mesh(it, 0, &mut approx) {
            Ok(x) => m.append(&x),
            Err(w) if tess::is_curve_only(w) => {}
            Err(w) => {
                approx.push(w);
                last = w;
            }
        }
    }
    if m.is_empty() {
        return Err(last);
    }
    m.transform(&fr);
    m.check()?;
    Ok((m, approx))
}

/// Name → value of the product's property set `name` (single values only).
fn pset(
    f: &StepFile,
    props: &HashMap<u32, Vec<u32>>,
    id: u32,
    name: &str,
) -> Option<HashMap<String, String>> {
    let mut out = None;
    for ps in props.get(&id).into_iter().flatten() {
        let Some(pe) = f.get(*ps) else { continue };
        if f.type_of(*ps) != Some("IFCPROPERTYSET") || Rd::arg(pe, 2).as_str() != Some(name) {
            continue;
        }
        let m = out.get_or_insert_with(HashMap::new);
        for p in Rd::refs(pe, 4) {
            if let Some(pv) = f.get(p) {
                if let (Some(n), Some(v)) = (Rd::arg(pv, 0).as_str(), Rd::arg(pv, 2).as_str()) {
                    m.insert(n.to_string(), v.to_string());
                }
            }
        }
    }
    out
}

fn component(
    kv: &HashMap<String, String>,
    storey: Option<usize>,
    hosts: &HashMap<&str, PCompHost>,
) -> Result<PComp, Why> {
    let level = storey.ok_or("not in a storey")?;
    let get = |k: &str| kv.get(k).cloned().ok_or("incomplete DDC_Component");
    let frame = |k: &str| -> Result<([f64; 4], bool), Why> {
        let v: Vec<f64> = get(k)?
            .split_whitespace()
            .map(|x| x.parse::<f64>().map_err(|_| "bad DDC_Component placement"))
            .collect::<Result<_, _>>()?;
        match v[..] {
            [x, y, z, r, m] if v.iter().all(|a| a.is_finite()) => Ok(([x, y, z, r], m != 0.0)),
            _ => Err("bad DDC_Component placement"),
        }
    };
    let host = match kv.get("Host").filter(|h| !h.is_empty()) {
        Some(h) => hosts.get(h.as_str()).copied().unwrap_or(PCompHost::None),
        None => PCompHost::None,
    };
    if host == PCompHost::None && kv.get("HostKind").is_some_and(|k| k == "wall") {
        return Err("host wall not imported");
    }
    Ok(PComp {
        def: get("Definition")?,
        type_name: get("Type")?,
        type_vals: get("TypeValues")?,
        vals: get("Values")?,
        level,
        local: frame("Local")?,
        world: frame("World")?,
        host,
    })
}

/// IfcGrid → axes. Exact bulge, bubbles, level range and order come from `DDC_Grid` when present;
/// otherwise an arc's bulge follows from its midpoint and the axis spans all levels.
fn grid(
    rd: &mut Rd,
    pl: &mut Plan,
    props: &HashMap<u32, Vec<u32>>,
    storey: Option<usize>,
    id: u32,
    e: &Entity,
) -> Result<(), Why> {
    let fr = rd.product_frame(e)?;
    let f = rd.f;
    let mut extra: HashMap<String, String> = HashMap::new();
    for ps in props.get(&id).into_iter().flatten() {
        let Some(pe) = f.get(*ps) else { continue };
        if f.type_of(*ps) != Some("IFCPROPERTYSET") || Rd::arg(pe, 2).as_str() != Some("DDC_Grid") {
            continue;
        }
        for p in Rd::refs(pe, 4) {
            if let Some(pv) = f.get(p) {
                if let (Some(n), Some(v)) = (Rd::arg(pv, 0).as_str(), Rd::arg(pv, 2).as_str()) {
                    extra.insert(n.to_string(), v.to_string());
                }
            }
        }
    }
    let lvl = |name: &str, pl: &Plan| pl.levels.iter().position(|l| l.name == name);
    let mut axes = Rd::refs(e, 7);
    axes.extend(Rd::refs(e, 8));
    axes.extend(Rd::refs(e, 9));
    for (k, ax) in axes.into_iter().enumerate() {
        let ae = rd.ent(ax, &["IFCGRIDAXIS"])?;
        let name = Rd::arg(ae, 0).as_str().unwrap_or_default().to_string();
        let curve = Rd::reference(ae, 1)?;
        let pts: Vec<V3> = rd
            .curve_points(curve)?
            .into_iter()
            .map(|p| fr.apply(p))
            .collect();
        if pts.len() < 2 {
            return Err("bad grid axis");
        }
        let (a, b) = (pts[0], pts[pts.len() - 1]);
        let mut g = PGrid {
            name: name.clone(),
            a: (a[0], a[1]),
            b: (b[0], b[1]),
            bulge: 0.0,
            base: storey,
            top: None,
            bubbles: 3,
            ord: pl.grids.len() + k,
        };
        if pts.len() == 3 {
            let (m, c) = (pts[1], ((b[0] - a[0]).hypot(b[1] - a[1])));
            // midpoint sagitta to the right of the chord a→b: bulge = 2·s / chord
            let s = ((m[0] - a[0]) * (b[1] - a[1]) - (m[1] - a[1]) * (b[0] - a[0])) / c;
            g.bulge = 2.0 * s / c;
        }
        if let Some(v) = extra.get(&name) {
            for kv in v.split(';') {
                let Some((key, val)) = kv.split_once('=') else {
                    continue;
                };
                match key {
                    "ord" => g.ord = val.parse().unwrap_or(g.ord),
                    "bubbles" => g.bubbles = val.parse().unwrap_or(3),
                    "base" => g.base = lvl(val, pl),
                    "top" => g.top = lvl(val, pl),
                    "bulge" => g.bulge = val.parse().unwrap_or(g.bulge),
                    _ => {}
                }
            }
        }
        pl.grids.push(g);
    }
    Ok(())
}

/// Layer set of a product (usage on the instance, or a set on the instance or its type):
/// (set id, usage sense negative, usage offset, layers in file order without zero layers).
fn layer_set(
    rd: &Rd,
    material: &HashMap<u32, u32>,
    typed: &HashMap<u32, u32>,
    id: u32,
) -> Result<LayerSet, Why> {
    let f = rd.f;
    let mat = material
        .get(&id)
        .or_else(|| typed.get(&id).and_then(|t| material.get(t)))
        .copied()
        .ok_or("no layer set")?;
    let me = f.get(mat).ok_or("missing entity")?;
    let (set, sense_neg0, off) = match f.type_of(mat) {
        Some("IFCMATERIALLAYERSETUSAGE") => {
            let set = Rd::reference(me, 0)?;
            let neg = Rd::arg(me, 2).as_enum() == Some("NEGATIVE");
            let off = Rd::arg(me, 3).as_f64().ok_or("bad layer usage")? * rd.scale;
            (set, neg, Some(off))
        }
        Some("IFCMATERIALLAYERSET") => (mat, false, None),
        _ => return Err("no layer set"),
    };
    let se = rd.ent(set, &["IFCMATERIALLAYERSET"])?;
    let mut layers = vec![];
    for l in Rd::refs(se, 0) {
        let le = rd.ent(l, &["IFCMATERIALLAYER"])?;
        let t = Rd::arg(le, 1).as_f64().ok_or("bad layer")? * rd.scale;
        let t = snap(t);
        if !t.is_finite() || t < 0.0 {
            return Err("bad layer");
        }
        if t == 0.0 {
            continue;
        }
        let name = Rd::arg(le, 0)
            .as_ref()
            .and_then(|m| f.get(m))
            .and_then(|m| Rd::arg(m, 0).as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Material");
        layers.push((name.to_string(), t));
    }
    if layers.is_empty() {
        return Err("zero thickness");
    }
    Ok((set, sense_neg0, off, layers))
}

type LayerSet = (u32, bool, Option<f64>, Vec<(String, f64)>);

/// Storey of a product: its spatial container, else the nearest storey at or below `z`,
/// else the first storey, else a new "Level 1".
fn level_for(pl: &mut Plan, storey: Option<usize>, z: f64) -> usize {
    if let Some(l) = storey {
        return l;
    }
    let best = pl
        .levels
        .iter()
        .enumerate()
        .filter(|(_, l)| l.elevation <= z + 1e-9)
        .max_by(|x, y| x.1.elevation.total_cmp(&y.1.elevation))
        .map(|(i, _)| i);
    match best.or(if pl.levels.is_empty() { None } else { Some(0) }) {
        Some(i) => i,
        None => {
            pl.levels.push(PLevel {
                name: "Level 1".into(),
                elevation: 0.0,
            });
            0
        }
    }
}

fn is_slab(t: &str) -> bool {
    matches!(
        t,
        "IFCSLAB" | "IFCSLABSTANDARDCASE" | "IFCSLABELEMENTEDCASE"
    )
}

/// Plan axis of a wall without an `Axis` representation: the centre line of a rectangular or
/// parallelogram extrusion footprint, in the product frame, with the wall thickness.
fn footprint_axis(rd: &mut Rd, e: &Entity) -> Result<(V3, V3, f64), Why> {
    let items = rd.body_items(e)?;
    let &[mut id] = items.as_slice() else {
        return Err(if items.is_empty() {
            "no axis, no body"
        } else {
            "no axis, unsupported body"
        });
    };
    for _ in 0..32 {
        if matches!(
            rd.f.type_of(id),
            Some("IFCBOOLEANCLIPPINGRESULT" | "IFCBOOLEANRESULT")
        ) {
            id = Rd::reference(rd.ent(id, &[])?, 1)?;
        }
    }
    let x = rd
        .ent(id, &["IFCEXTRUDEDAREASOLID"])
        .map_err(|_| "no axis, unsupported body")?;
    let pos = rd.axis_placement(Rd::arg(x, 1))?;
    let d = pos.dir(rd.direction(Rd::arg(x, 2))?.ok_or("bad extrusion")?);
    if d[2].abs() < 1.0 - 1e-9 || pos.z[2].abs() < 1.0 - 1e-9 {
        return Err("no axis, non-vertical body");
    }
    let area = rd.profile_area(Rd::reference(x, 0)?, 0)?;
    let [p] = area.as_slice() else {
        return Err("no axis, footprint not a wall");
    };
    let [r] = p.as_slice() else {
        return Err("no axis, footprint with voids");
    };
    // drop collinear vertices
    let mut r: Vec<V3> = r.clone();
    let mut k = 0;
    while r.len() > 3 && k < r.len() {
        let n = r.len();
        let (a, b, c) = (r[(k + n - 1) % n], r[k], r[(k + 1) % n]);
        let u = tess::sub(b, a);
        let v = tess::sub(c, b);
        if cross(u, v)[2].abs() <= 1e-9 * dot(u, u).sqrt().max(1e-9) * dot(v, v).sqrt().max(1e-9) {
            r.remove(k);
        } else {
            k += 1;
        }
    }
    if r.len() != 4 {
        return Err("no axis, footprint not a quadrilateral");
    }
    let edge = |i: usize| tess::sub(r[(i + 1) % 4], r[i % 4]);
    let l = |v: V3| dot(v, v).sqrt();
    let pair = if l(edge(0)) + l(edge(2)) >= l(edge(1)) + l(edge(3)) {
        0
    } else {
        1
    };
    let (e0, e2) = (edge(pair), edge(pair + 2));
    let (Some(u0), Some(u2)) = (norm(e0), norm(e2)) else {
        return Err("no axis, degenerate footprint");
    };
    if cross(u0, u2)[2].abs() > 1e-6 {
        return Err("no axis, footprint not a wall");
    }
    let mid = |i: usize| mul(add(r[i % 4], r[(i + 1) % 4]), 0.5);
    let (p0, p1) = (mid(pair + 3), mid(pair + 1));
    let thick = cross(u0, tess::sub(r[(pair + 2) % 4], r[pair]))[2].abs();
    if !(thick > 0.0 && thick.is_finite()) {
        return Err("no axis, zero thickness");
    }
    Ok((pos.apply(p0), pos.apply(p1), thick))
}

/// Half-space cuts of the body (DIFFERENCE chain of the single body item): (point, normal of
/// the removed side), product frame.
fn clip_planes(rd: &mut Rd, e: &Entity) -> Result<Vec<(V3, V3)>, Why> {
    let items = rd.body_items(e)?;
    let &[mut id] = items.as_slice() else {
        return Ok(vec![]);
    };
    let mut out = vec![];
    for _ in 0..32 {
        if !matches!(
            rd.f.type_of(id),
            Some("IFCBOOLEANCLIPPINGRESULT" | "IFCBOOLEANRESULT")
        ) {
            break;
        }
        let b = rd.ent(id, &[])?;
        if Rd::arg(b, 0).as_enum() == Some("DIFFERENCE") {
            if let Some((o, n, _)) = rd.half_space(Rd::reference(b, 2)?)? {
                out.push((o, n));
            }
        }
        id = Rd::reference(b, 1)?;
    }
    Ok(out)
}

/// Top of a straight wall a→b (bottom at `z0`, flat top at `height`) under clipping planes:
/// (u along the axis, h above the bottom); `None` when no plane lowers the top.
fn top_profile(
    a: (f64, f64),
    b: (f64, f64),
    z0: f64,
    height: f64,
    planes: &[(V3, V3)],
) -> Result<Option<Vec<(f64, f64)>>, Why> {
    let l = (b.0 - a.0).hypot(b.1 - a.1);
    let d = ((b.0 - a.0) / l, (b.1 - a.1) / l);
    let mut lines = vec![(height, 0.0)];
    for (o, n) in planes {
        if n[2] <= 1e-6 {
            if n[2] < -1e-6 {
                return Err("clipped from below");
            }
            continue; // vertical cuts trim the ends: joins do that natively
        }
        let c = o[2] - (n[0] * (a.0 - o[0]) + n[1] * (a.1 - o[1])) / n[2] - z0;
        let k = -(n[0] * d.0 + n[1] * d.1) / n[2];
        lines.push((c, k));
    }
    if lines.len() == 1 {
        return Ok(None);
    }
    let mut us = vec![0.0, l];
    for i in 0..lines.len() {
        for j in i + 1..lines.len() {
            let (ci, ki) = lines[i];
            let (cj, kj) = lines[j];
            if (ki - kj).abs() > 1e-12 {
                let u = (cj - ci) / (ki - kj);
                if u > 1e-9 && u < l - 1e-9 {
                    us.push(u);
                }
            }
        }
    }
    us.sort_by(f64::total_cmp);
    us.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
    let h = |u: f64| {
        lines
            .iter()
            .map(|(c, k)| c + k * u)
            .fold(f64::INFINITY, f64::min)
    };
    let prof: Vec<(f64, f64)> = us.iter().map(|&u| (snap(u), snap(h(u)))).collect();
    if prof.iter().any(|p| !(p.1 > 1e-6) || !p.1.is_finite()) {
        return Err("clipped to zero height");
    }
    if prof.iter().all(|p| (p.1 - height).abs() < 1e-9) {
        return Ok(None);
    }
    // drop interior points on a straight run
    let mut out: Vec<(f64, f64)> = vec![];
    for p in prof {
        if out.len() >= 2 {
            let (q, r) = (out[out.len() - 2], out[out.len() - 1]);
            let cr = (r.0 - q.0) * (p.1 - q.1) - (r.1 - q.1) * (p.0 - q.0);
            if cr.abs() < 1e-12 {
                out.pop();
            }
        }
        out.push(p);
    }
    Ok(Some(out))
}

/// Layer offset of body points from a plan segment (left of a→b positive; arcs: radial).
fn offsets(segs: &[((f64, f64), (f64, f64), f64)], pts: &[V3]) -> (f64, f64) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in pts {
        let mut best = (f64::INFINITY, 0.0);
        for &(a, b, bulge) in segs {
            let l = (b.0 - a.0).hypot(b.1 - a.1);
            if !(l > 0.0) {
                continue;
            }
            let (dx, dy) = ((b.0 - a.0) / l, (b.1 - a.1) / l);
            let (rx, ry) = (p[0] - a.0, p[1] - a.1);
            let (along, s) = if bulge == 0.0 {
                (rx * dx + ry * dy, -rx * dy + ry * dx)
            } else {
                let th = 4.0 * bulge.atan();
                let rs = l / (2.0 * (th / 2.0).sin());
                let hgt = rs * (th / 2.0).cos();
                let c = ((a.0 + b.0) / 2.0 - dy * hgt, (a.1 + b.1) / 2.0 + dx * hgt);
                let dist = (p[0] - c.0).hypot(p[1] - c.1);
                let s = if bulge > 0.0 {
                    rs.abs() - dist
                } else {
                    dist - rs.abs()
                };
                (l / 2.0, s)
            };
            let out = (along.clamp(0.0, l) - along).abs() + s.abs() * 1e-12;
            if out < best.0 {
                best = (out, s);
            }
        }
        lo = lo.min(best.1);
        hi = hi.max(best.1);
    }
    (lo, hi)
}

#[allow(clippy::too_many_arguments)]
fn wall(
    rd: &mut Rd,
    pl: &mut Plan,
    type_ix: &mut HashMap<(u32, bool), usize>,
    storey: Option<usize>,
    ctx: &props::Ctx,
    id: u32,
    e: &Entity,
) -> Result<(Vec<usize>, Frame, f64), Why> {
    let f = rd.f;
    let fr = rd.product_frame(e)?;
    if fr.z[2].abs() < 1.0 - 1e-9 {
        return Err("non-vertical placement");
    }
    // a mirrored product frame turns arcs the other way in plan
    let mirror = fr.z[2] < 0.0;
    let mut segs: Vec<((f64, f64), (f64, f64), f64)> = vec![];
    let mut push = |p0: V3, p1: V3, bulge: f64| {
        let (a, b) = (fr.apply(p0), fr.apply(p1));
        let (a, b) = ((snap(a[0]), snap(a[1])), (snap(b[0]), snap(b[1])));
        let len = (b.0 - a.0).hypot(b.1 - a.1);
        if len.is_finite() && len > 1e-6 {
            segs.push((a, b, if mirror { -bulge } else { bulge }));
        }
    };
    let items = rd.rep_items(e, "Axis")?;
    let mut thick_known = None;
    if items.is_empty() {
        let (p0, p1, t) = footprint_axis(rd, e)?;
        push(p0, p1, 0.0);
        thick_known = Some(snap(t));
    } else {
        match rd.arc_axis(&items)? {
            Some((p0, p1, b)) => push(p0, p1, b),
            None => match rd.axis(&items) {
                Ok(v) => {
                    for (p0, p1) in v {
                        push(p0, p1, 0.0);
                    }
                }
                Err("unsupported axis") if items.len() == 1 => {
                    for s in rd.segs(items[0], 0)? {
                        match s {
                            tess::Seg::Line(a, b) => push(a, b, 0.0),
                            tess::Seg::Arc { a, b, n, sweep, .. } => {
                                if n[2].abs() < 1.0 - 1e-9
                                    || sweep.abs() >= 2.0 * std::f64::consts::PI - 1e-9
                                {
                                    return Err("unsupported axis arc");
                                }
                                push(a, b, (sweep * n[2].signum() / 4.0).tan());
                            }
                        }
                    }
                }
                Err(w) => return Err(w),
            },
        }
    }
    if segs.is_empty() {
        return Err("zero length");
    }
    let pts = rd.body_world(e)?;
    let zmin = pts.iter().map(|p| p[2]).fold(f64::INFINITY, f64::min);
    let zmax = pts.iter().map(|p| p[2]).fold(f64::NEG_INFINITY, f64::max);
    let height = snap(zmax - zmin);
    if height.is_nan() || height <= 0.0 {
        return Err("zero height");
    }
    let level = level_for(pl, storey, zmin);
    let base_offset = snap(zmin - pl.levels[level].elevation);

    let (wt, offset0) = match layer_set(rd, ctx.material, ctx.typed, id) {
        Ok((set, sense_neg0, off, mut layers)) => {
            let se = f.get(set).ok_or("missing entity")?;
            if sense_neg0 {
                layers.reverse();
            }
            let total: f64 = layers.iter().map(|l| l.1).sum();
            // The layer offset runs along the frame's local Y; when that is the right-hand
            // side of the axis (reversed Z or reversed axis), the layer order mirrors.
            let (sa, sb) = (segs[0].0, segs[0].1);
            let left = [-(sb.1 - sa.1), sb.0 - sa.0];
            let flip = left[0] * fr.y[0] + left[1] * fr.y[1] < 0.0;
            let sense_neg = sense_neg0 ^ flip;
            if flip {
                layers.reverse();
            }
            let offset0 = off
                .map(|o| if sense_neg0 { o - total } else { o })
                .map(|lo| if flip { -(lo + total) } else { lo })
                .map(snap)
                .filter(|o| (o + total / 2.0).abs() > 1e-9);
            let key = (set, sense_neg);
            let wt = match type_ix.get(&key) {
                Some(t) => *t,
                None => {
                    let base = Rd::arg(se, 1)
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map_or_else(|| format!("Wall type #{set}"), str::to_string);
                    let name = if sense_neg {
                        format!("{base} (reversed)")
                    } else {
                        base
                    };
                    pl.types.push(PType { name, layers });
                    type_ix.insert(key, pl.types.len() - 1);
                    pl.types.len() - 1
                }
            };
            (wt, offset0)
        }
        Err(_) => {
            // single-layer type from the body thickness across the axis
            let (th, offset0) = match thick_known {
                Some(t) => (t, None),
                None => {
                    let (lo, hi) = offsets(&segs, &pts);
                    let th = snap(hi - lo);
                    let lo = snap(lo);
                    (th, Some(lo).filter(|o| (o + th / 2.0).abs() > 1e-9))
                }
            };
            if !(th > 0.0 && th.is_finite()) {
                return Err("zero thickness");
            }
            let mat = ctx
                .material_name(id)
                .unwrap_or_else(|| "Material".to_string());
            let name = format!("{mat} {}", snap(th * 1000.0));
            let layers = vec![(mat, th)];
            let wt = match pl
                .types
                .iter()
                .position(|t| t.name == name && t.layers == layers)
            {
                Some(t) => t,
                None => {
                    pl.types.push(PType { name, layers });
                    pl.types.len() - 1
                }
            };
            (wt, offset0)
        }
    };
    let planes: Vec<(V3, V3)> = clip_planes(rd, e)
        .unwrap_or_default()
        .into_iter()
        .map(|(o, n)| (fr.apply(o), fr.dir(n)))
        .collect();
    let mut ids = vec![];
    for (a, b, bulge) in segs {
        let (top, h) = if bulge == 0.0 && !planes.is_empty() {
            match top_profile(a, b, zmin, height, &planes) {
                Ok(Some(t)) => {
                    let h = t.iter().map(|p| p.1).fold(0.0, f64::max);
                    (Some(t), h)
                }
                Ok(None) => (None, height),
                Err(_) => (None, height),
            }
        } else {
            (None, height)
        };
        ids.push(pl.walls.len());
        pl.walls.push(PWall {
            level,
            wall_type: wt,
            a,
            b,
            base_offset,
            height: h,
            offset0,
            bulge,
            top,
        });
    }
    Ok((ids, fr, zmin))
}

/// Slab type of a product: its layer set (top first), else one layer of `thick`.
#[allow(clippy::too_many_arguments)]
fn slab_type_of(
    rd: &Rd,
    pl: &mut Plan,
    type_ix: &mut HashMap<u32, usize>,
    ctx: &props::Ctx,
    id: u32,
    thick: f64,
    what: &str,
) -> usize {
    let (set, layers) = match layer_set(rd, ctx.material, ctx.typed, id) {
        Ok((set, neg, _, mut layers)) => {
            // positive sense: the first layer is at the reference (bottom) side
            if !neg {
                layers.reverse();
            }
            (Some(set), layers)
        }
        Err(_) => (
            None,
            vec![(
                ctx.material_name(id).unwrap_or_else(|| "Material".into()),
                thick,
            )],
        ),
    };
    let key = set.unwrap_or(0);
    if let Some(t) = type_ix.get(&key).filter(|_| set.is_some()) {
        return *t;
    }
    let name = set
        .and_then(|s| rd.f.get(s))
        .and_then(|se| Rd::arg(se, 1).as_str())
        .filter(|s| !s.is_empty())
        .map_or_else(|| format!("{what} {thick}"), str::to_string);
    let t = match pl
        .slab_types
        .iter()
        .position(|t| t.layers == layers && t.name == name)
    {
        Some(t) => t,
        None => {
            pl.slab_types.push(PType { name, layers });
            pl.slab_types.len() - 1
        }
    };
    if set.is_some() {
        type_ix.insert(key, t);
    }
    t
}

/// Slab = vertical extrusion of a plan profile; layers top first.
#[allow(clippy::too_many_arguments)]
fn slab(
    rd: &mut Rd,
    pl: &mut Plan,
    type_ix: &mut HashMap<u32, usize>,
    storey: Option<usize>,
    ctx: &props::Ctx,
    id: u32,
    e: &Entity,
) -> Result<(), Why> {
    let (rings, zmin, zmax, slope) = match rd.sloped_prism(e) {
        Ok(v) => v,
        // a triangulated body that is a flat vertical prism is a slab all the same
        Err("unsupported body") => {
            let (m, _) = product_mesh(rd, e)?;
            let (rings, z0, z1) = fit::prism(&m)?;
            (rings, z0, z1, None)
        }
        Err(w) => return Err(w),
    };
    let thick = snap(zmax - zmin);
    if thick.is_nan() || thick <= 0.0 {
        return Err("zero thickness");
    }
    let prof = repaired_profile(pl, id, &rings)?;
    let back = |r: &Vec<ddc_geom::V2>| r.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>();
    let st = slab_type_of(rd, pl, type_ix, ctx, id, thick, "Slab");
    let level = level_for(pl, storey, zmax);
    pl.slabs.push(PSlab {
        level,
        slab_type: st,
        offset: snap(zmax - pl.levels[level].elevation),
        outer: back(&prof.outer),
        holes: prof.holes.iter().map(back).collect(),
        slope,
    });
    Ok(())
}

/// Footing from a body of concentric box or round steps; a STRIP_FOOTING box becomes a strip
/// footing along its long side. Top relative to the level.
fn footing(
    rd: &mut Rd,
    pl: &mut Plan,
    storey: Option<usize>,
    ctx: &props::Ctx,
    id: u32,
    e: &Entity,
) -> Result<(), Why> {
    use ddc_geom::footing::{Isolated, Step};
    let (m, _) = product_mesh(rd, e)?;
    let fit = fit::steps(&m)?;
    let material = ctx
        .material_name(id)
        .unwrap_or_else(|| "Material".to_string());
    let level = level_for(pl, storey, fit.top);
    let top_offset = snap(fit.top - pl.levels[level].elevation);
    if Rd::arg(e, 8).as_enum() == Some("STRIP_FOOTING") {
        let &[(w, l, h)] = fit.steps.as_slice() else {
            return Err("strip footing is not a single box");
        };
        if fit.round {
            return Err("strip footing is not a single box");
        }
        // the strip runs along the longer side
        let (len, width, a) = if w >= l {
            (w, l, fit.rotation)
        } else {
            (l, w, fit.rotation + std::f64::consts::FRAC_PI_2)
        };
        let d = (a.cos() * len / 2.0, a.sin() * len / 2.0);
        let c = fit.center;
        pl.strip_footings.push(PStrip {
            level,
            spec: ddc_model::StripFootingSpec {
                level: 0,
                wall: None,
                a: (snap(c.0 - d.0), snap(c.1 - d.1)),
                b: (snap(c.0 + d.0), snap(c.1 + d.1)),
                bulge: 0.0,
                width,
                depth: h,
                ecc: 0.0,
                top_offset,
                material,
            },
        });
        return Ok(());
    }
    let steps: Vec<Step> = fit.steps.iter().map(|&(w, l, h)| Step { w, l, h }).collect();
    Isolated {
        round: fit.round,
        steps: steps.clone(),
        center: ddc_geom::v2(fit.center.0, fit.center.1),
        rotation: fit.rotation,
        top: fit.top,
    }
    .check()
    .map_err(|_| "footing steps widen upwards")?;
    pl.footings.push(PFooting {
        level,
        spec: ddc_model::FootingSpec {
            level: 0,
            column: None,
            at: fit.center,
            rotation: fit.rotation,
            round: fit.round,
            steps,
            top_offset,
            material,
        },
    });
    Ok(())
}

/// Axis length of a planned wall (arc length for an arc).
fn axis_len(w: &PWall) -> f64 {
    if w.bulge == 0.0 {
        return (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1);
    }
    ddc_geom::arc::ArcAxis::new(
        ddc_geom::v2(w.a.0, w.a.1),
        ddc_geom::v2(w.b.0, w.b.1),
        w.bulge,
        ddc_geom::Tol::default(),
    )
    .map_or(0.0, |a| a.length())
}

/// An opening past its wall (the model rejects it) is cut back to the wall; with nothing left it
/// is skipped (width 0, not committed). Both are report warnings.
fn fit_openings(pl: &mut Plan) {
    let guid: HashMap<usize, &str> = pl
        .elems
        .iter()
        .filter(|e| e.kind == "opening")
        .map(|e| (e.index, e.guid.as_str()))
        .collect();
    let mut warnings = vec![];
    for (k, o) in pl.openings.iter_mut().enumerate() {
        let w = &pl.walls[o.wall];
        let len = axis_len(w);
        let top = if w.top.is_some() {
            f64::INFINITY
        } else {
            w.height
        };
        let eps = ddc_geom::math::Tol::default().length;
        let fits = o.along >= -eps
            && o.along + o.width <= len + eps
            && o.sill >= -eps
            && o.sill + o.height <= top + eps
            && o.width > 0.0
            && o.height > 0.0;
        if fits {
            continue;
        }
        let who = guid.get(&k).map_or(String::new(), |g| format!(" {g}"));
        // a hair inside the wall so along + width never rounds past its end
        let (u0, u1) = (o.along.max(0.0), (o.along + o.width).min(len - 1e-9));
        let (z0, z1) = (o.sill.max(0.0), (o.sill + o.height).min(top - 1e-9));
        if !(u1 - u0 > 0.01 && z1 - z0 > 0.01) {
            o.width = 0.0;
            warnings.push(format!("opening{who}: outside its wall, skipped"));
            continue;
        }
        (o.along, o.width, o.sill, o.height) = (u0, u1 - u0, z0, z1 - z0);
        warnings.push(format!("opening{who}: past its wall, cut back to the wall"));
    }
    pl.report.warnings.extend(warnings);
}

/// Plan profile, repaired where the outline allows: repeated and collinear points dropped, a
/// bad or outside hole dropped; a repair is a report warning.
fn repaired_profile(
    pl: &mut Plan,
    id: u32,
    rings: &[Vec<(f64, f64)>],
) -> Result<ddc_geom::Profile, Why> {
    use ddc_geom::ProfileError as E;
    let tol = ddc_geom::Tol::default();
    let to2 = |r: &Vec<(f64, f64)>| r.iter().map(|p| ddc_geom::v2(p.0, p.1)).collect::<Vec<_>>();
    let first = rings.first().ok_or("bad profile")?;
    if let Ok(p) = ddc_geom::Profile::new(to2(first), rings[1..].iter().map(to2).collect(), tol) {
        return Ok(p);
    }
    let clean = |r: &Vec<(f64, f64)>| {
        let mut v: Vec<ddc_geom::V2> = vec![];
        for p in to2(r) {
            if v.last().is_none_or(|q| (p - *q).len() > 1e-6) {
                v.push(p);
            }
        }
        while v.len() > 1 && (v[0] - v[v.len() - 1]).len() <= 1e-6 {
            v.pop();
        }
        // collinear and back-tracking vertices
        let mut k = 0;
        while v.len() > 3 && k < v.len() {
            let n = v.len();
            let (a, b, c) = (v[(k + n - 1) % n], v[k], v[(k + 1) % n]);
            let (d1, d2) = (b - a, c - b);
            if d1.cross(d2).abs() <= 1e-9 * d1.len().max(d2.len()) {
                v.remove(k);
                k = k.saturating_sub(1);
            } else {
                k += 1;
            }
        }
        v
    };
    let outer = clean(first);
    let mut holes: Vec<Vec<ddc_geom::V2>> = rings[1..]
        .iter()
        .map(clean)
        .filter(|h| h.len() >= 3)
        .collect();
    let mut dropped = rings.len() - 1 - holes.len();
    loop {
        let bad_hole = match ddc_geom::Profile::new(outer.clone(), holes.clone(), tol) {
            Ok(p) => {
                pl.report.warnings.push(format!(
                    "#{id}: outline repaired (points cleaned, {dropped} hole(s) dropped)"
                ));
                return Ok(p);
            }
            Err(E::HoleOutside { hole }) => hole,
            Err(E::HolesOverlap { b, .. }) => b,
            Err(
                E::BadCoordinate { ring, .. }
                | E::TooFewPoints { ring }
                | E::ZeroLengthEdge { ring, .. }
                | E::ZeroArea { ring }
                | E::SelfIntersection { ring, .. },
            ) if ring > 0 => ring - 1,
            Err(_) => return Err("bad profile"),
        };
        if bad_hole >= holes.len() {
            return Err("bad profile");
        }
        holes.remove(bad_hole);
        dropped += 1;
    }
}

/// Ceiling covering = vertical extrusion of a plan profile; bottom height above its level.
fn ceiling(
    rd: &mut Rd,
    pl: &mut Plan,
    type_ix: &mut HashMap<u32, usize>,
    storey: Option<usize>,
    ctx: &props::Ctx,
    id: u32,
    e: &Entity,
) -> Result<(), Why> {
    let (rings, zmin, zmax) = rd.vertical_prism(e)?;
    let thick = snap(zmax - zmin);
    if thick.is_nan() || thick <= 0.0 {
        return Err("zero thickness");
    }
    let prof = repaired_profile(pl, id, &rings)?;
    let back = |r: &Vec<ddc_geom::V2>| r.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>();
    let st = slab_type_of(rd, pl, type_ix, ctx, id, thick, "Ceiling");
    let level = level_for(pl, storey, zmin);
    pl.ceilings.push(PCeiling {
        level,
        ceiling_type: st,
        height: snap(zmin - pl.levels[level].elevation),
        outer: back(&prof.outer),
        holes: prof.holes.iter().map(back).collect(),
    });
    Ok(())
}

/// Planar sloped roof part: plan ring, top z at a point, the point, top gradient, thickness
/// normal to the slope.
type RoofPlane = (Vec<(f64, f64)>, f64, (f64, f64), (f64, f64), f64);

impl Rd<'_> {
    /// Body extruded along the normal of a tilted profile plane.
    fn tilted_prism(&mut self, e: &Entity) -> Result<RoofPlane, Why> {
        let fr = self.product_frame(e)?;
        let items = self.body_items(e)?;
        let &[mut id] = items.as_slice() else {
            return Err("unsupported body");
        };
        for _ in 0..32 {
            if matches!(
                self.f.type_of(id),
                Some("IFCBOOLEANCLIPPINGRESULT" | "IFCBOOLEANRESULT")
            ) {
                id = Self::reference(self.ent(id, &[])?, 1)?;
            }
        }
        let x = self
            .ent(id, &["IFCEXTRUDEDAREASOLID"])
            .map_err(|_| "unsupported body")?;
        let pos = fr.then(&self.axis_placement(Self::arg(x, 1))?);
        let d = pos.dir(self.direction(Self::arg(x, 2))?.ok_or("bad extrusion")?);
        let depth = Self::arg(x, 3).as_f64().ok_or("bad extrusion")? * self.scale;
        let c = cross(d, pos.z);
        if dot(c, c).sqrt() > 1e-6 {
            return Err("oblique extrusion");
        }
        let n = if pos.z[2] >= 0.0 {
            pos.z
        } else {
            mul(pos.z, -1.0)
        };
        if n[2] < 0.1 {
            return Err("steep roof part");
        }
        if n[2] > 1.0 - 1e-12 {
            return Err("flat roof part");
        }
        let rings = self.profile_rings(Self::reference(x, 0)?)?;
        let [r] = rings.as_slice() else {
            return Err("roof with holes");
        };
        let v = mul(d, depth);
        let lift = if dot(v, n) > 0.0 { v } else { [0.0; 3] };
        let face: Vec<V3> = r.iter().map(|p| add(pos.apply(*p), lift)).collect();
        let ring = face.iter().map(|p| (snap(p[0]), snap(p[1]))).collect();
        let p = (face[0][0], face[0][1]);
        Ok((
            ring,
            face[0][2],
            p,
            (-n[0] / n[2], -n[1] / n[2]),
            depth.abs(),
        ))
    }
}

/// Planar sloped roof part → footprint roof: the eave edge across the gradient slopes, the
/// other edges are gables; a flat or warped part is left to the slab/mesh path.
#[allow(clippy::too_many_arguments)]
fn roof_part(
    rd: &mut Rd,
    pl: &mut Plan,
    type_ix: &mut HashMap<u32, usize>,
    storey: Option<usize>,
    ctx: &props::Ctx,
    id: u32,
    e: &Entity,
) -> Result<(), Why> {
    let (mut ring, z_at, p0, g, thick) = match rd.sloped_prism(e) {
        Ok((rings, zmin, zmax, Some((p, g)))) => {
            if rings.len() != 1 {
                return Err("roof with holes");
            }
            let th = (zmax - zmin) / (1.0 + g.0 * g.0 + g.1 * g.1).sqrt();
            (rings[0].clone(), zmax, p, g, th)
        }
        Ok(_) => return Err("flat roof part"),
        Err("non-vertical extrusion") => rd.tilted_prism(e)?,
        Err(w) => return Err(w),
    };
    let a2: f64 = (0..ring.len())
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    if a2 < 0.0 {
        ring.reverse();
    }
    let gl = g.0.hypot(g.1);
    if !(gl > 1e-9 && gl.is_finite()) {
        return Err("flat roof part");
    }
    let n = ring.len();
    let eave = (0..n).find(|&i| {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let l = (b.0 - a.0).hypot(b.1 - a.1);
        l > 1e-9 && (-(b.1 - a.1) / l * g.0 + (b.0 - a.0) / l * g.1) / gl > 1.0 - 1e-7
    });
    let Some(i) = eave else {
        return Err("roof slope not across an edge");
    };
    let q = ring[i];
    let z_eave = z_at + g.0 * (q.0 - p0.0) + g.1 * (q.1 - p0.1);
    let thick = snap(thick);
    if thick.is_nan() || thick <= 0.0 {
        return Err("zero thickness");
    }
    let st = slab_type_of(rd, pl, type_ix, ctx, id, thick, "Roof");
    let level = level_for(pl, storey, z_eave);
    let mut slopes = vec![None; n];
    slopes[i] = Some(gl.atan().to_degrees());
    pl.roofs.push(PRoof {
        level,
        roof_type: st,
        offset: snap(z_eave - pl.levels[level].elevation),
        overhang: 0.0,
        fascia: 0.0,
        kind: ddc_model::RoofKind::Footprint {
            ring,
            slopes,
            mansard: None,
        },
    });
    Ok(())
}

/// Space-separated numbers, `-` = none.
fn ddc_nums(t: &str, bad: Why) -> Result<Vec<Option<f64>>, Why> {
    t.split_whitespace()
        .map(|x| match x {
            "-" => Ok(None),
            _ => x
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .map(Some)
                .ok_or(bad),
        })
        .collect()
}

/// Member row from `DDC_Member` (our own export): exact spec, type and top level.
fn exact_member(
    pl: &mut Plan,
    storey: Option<usize>,
    kv: &HashMap<String, String>,
) -> Result<(), Why> {
    use ddc_model::{BeamJoin, BeamSpec, ColumnSpec, Just, MemberShape};
    let bad = "bad DDC_Member";
    let get = |k: &str| kv.get(k).map(String::as_str).ok_or(bad);
    let shape = MemberShape::parse(get("Shape")?).ok_or(bad)?;
    let v = ddc_nums(get("Spec")?, bad)?;
    let req = |i: usize| v.get(i).copied().flatten().ok_or(bad);
    let exact = match get("Kind")? {
        "col" if v.len() == 8 => {
            let top_at = match (v[6], v[7]) {
                (Some(x), Some(y)) => Some((x, y)),
                _ => None,
            };
            let top = get("TopLevel")
                .ok()
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            MemberExact::Column(
                ColumnSpec {
                    level: 0,
                    top_level: None,
                    member_type: 0,
                    structural: req(5)? != 0.0,
                    at: (req(0)?, req(1)?),
                    base_offset: req(2)?,
                    top_offset: req(3)?,
                    rotation: req(4)?,
                    top_at,
                },
                top,
            )
        }
        "beam" if v.len() == 12 => MemberExact::Beam(BeamSpec {
            level: 0,
            member_type: 0,
            a: (req(0)?, req(1)?, req(2)?),
            b: (req(3)?, req(4)?, req(5)?),
            bulge: req(6)?,
            yjust: Just::from_i(req(7)? as i64),
            zjust: Just::from_i(req(8)? as i64),
            rotation: req(9)?,
            start_ext: req(10)?,
            end_ext: req(11)?,
            join_start: BeamJoin::parse(get("JoinStart")?),
            join_end: BeamJoin::parse(get("JoinEnd")?),
        }),
        _ => return Err(bad),
    };
    let level = level_for(pl, storey, 0.0);
    let e = pl.levels[level].elevation;
    let (column, a, b, rotation) = match &exact {
        MemberExact::Column(c, _) => {
            let t = c.top_at.unwrap_or(c.at);
            (
                true,
                [c.at.0, c.at.1, e + c.base_offset],
                [t.0, t.1, e + c.top_offset],
                c.rotation,
            )
        }
        MemberExact::Beam(s) => (
            false,
            [s.a.0, s.a.1, e + s.a.2],
            [s.b.0, s.b.1, e + s.b.2],
            s.rotation,
        ),
    };
    pl.members.push(PMember {
        column,
        level,
        shape,
        type_name: get("Type")?.to_string(),
        material: get("Material").unwrap_or("").to_string(),
        a,
        b,
        rotation,
        exact: Some(exact),
    });
    Ok(())
}

/// Roof row from `DDC_Roof` (our own export); the type comes from its material layer set.
#[allow(clippy::too_many_arguments)]
fn exact_roof(
    rd: &Rd,
    pl: &mut Plan,
    type_ix: &mut HashMap<u32, usize>,
    storey: Option<usize>,
    ctx: &props::Ctx,
    id: u32,
    kv: &HashMap<String, String>,
) -> Result<(), Why> {
    let bad = "bad DDC_Roof";
    let get = |k: &str| kv.get(k).map(String::as_str).ok_or(bad);
    let all = |t: &str| -> Result<Vec<f64>, Why> {
        ddc_nums(t, bad)?
            .into_iter()
            .map(|v| v.ok_or(bad))
            .collect()
    };
    let &[offset, overhang, fascia] = all(get("Spec")?)?.as_slice() else {
        return Err(bad);
    };
    let kind = match get("Kind")? {
        "footprint" => {
            let mut ring = vec![];
            let mut slopes = vec![];
            for v in get("Ring")?.split(';').filter(|t| !t.trim().is_empty()) {
                let &[Some(x), Some(y), sl] = ddc_nums(v, bad)?.as_slice() else {
                    return Err(bad);
                };
                ring.push((x, y));
                slopes.push(sl);
            }
            let mansard = match *ddc_nums(get("Mansard").unwrap_or(""), bad)?.as_slice() {
                [] => None,
                [Some(h), d] => Some((h, d)),
                _ => return Err(bad),
            };
            ddc_model::RoofKind::Footprint {
                ring,
                slopes,
                mansard,
            }
        }
        "extrusion" => {
            let &[ax, ay, bx, by, depth] = all(get("Axis")?)?.as_slice() else {
                return Err(bad);
            };
            let mut profile = vec![];
            for v in get("Profile")?.split(';').filter(|t| !t.trim().is_empty()) {
                let &[u, z] = all(v)?.as_slice() else {
                    return Err(bad);
                };
                profile.push((u, z));
            }
            ddc_model::RoofKind::Extrusion {
                a: (ax, ay),
                b: (bx, by),
                profile,
                depth,
            }
        }
        _ => return Err(bad),
    };
    let st = slab_type_of(rd, pl, type_ix, ctx, id, 0.2, "Roof");
    let level = level_for(pl, storey, 0.0);
    pl.roofs.push(PRoof {
        level,
        roof_type: st,
        offset,
        overhang,
        fascia,
        kind,
    });
    Ok(())
}

/// Column/beam/member with a single straight extrusion of a parametric or closed profile.
fn member(
    rd: &mut Rd,
    pl: &mut Plan,
    storey: Option<usize>,
    column: bool,
    ctx: &props::Ctx,
    id: u32,
    e: &Entity,
) -> Result<(), Why> {
    use ddc_model::MemberShape as Shape;
    let fr = rd.product_frame(e)?;
    let items = rd.body_items(e)?;
    let &[mut it] = items.as_slice() else {
        return Err(if items.is_empty() {
            "no body"
        } else {
            "unsupported body"
        });
    };
    let mut mfr = fr;
    if rd.f.type_of(it) == Some("IFCMAPPEDITEM") {
        let me = rd.ent(it, &[])?;
        let src = rd.ent(Rd::reference(me, 0)?, &["IFCREPRESENTATIONMAP"])?;
        let target = rd.operator(Rd::reference(me, 1)?)?;
        let rigid = [target.x, target.y, target.z]
            .iter()
            .all(|v| (dot(*v, *v) - 1.0).abs() < 1e-9);
        if !rigid {
            return Err("scaled mapped body");
        }
        let origin = rd.axis_placement(Rd::arg(src, 0))?;
        let rep = rd.ent(Rd::reference(src, 1)?, &[])?;
        let &[inner] = Rd::refs(rep, 3).as_slice() else {
            return Err("unsupported body");
        };
        it = inner;
        mfr = fr.then(&target).then(&origin);
    }
    let x = rd
        .ent(it, &["IFCEXTRUDEDAREASOLID"])
        .map_err(|_| "unsupported body")?;
    let pos = mfr.then(&rd.axis_placement(Rd::arg(x, 1))?);
    let dir = rd.direction(Rd::arg(x, 2))?.ok_or("bad extrusion")?;
    let depth = Rd::arg(x, 3).as_f64().ok_or("bad extrusion")? * rd.scale;
    let v = pos.dir(mul(dir, depth));
    let prof = Rd::reference(x, 0)?;
    let pe = rd.ent(prof, &[])?;
    let pty = rd.f.type_of(prof).unwrap_or("");
    let num = |i: usize| {
        Rd::arg(pe, i)
            .as_f64()
            .map(|v| v * rd.scale)
            .unwrap_or(f64::NAN)
    };
    // shape and its frame (origin at the shape centre)
    let parametric = match pty {
        "IFCRECTANGLEPROFILEDEF" => Some(Shape::Rect {
            b: num(3),
            h: num(4),
        }),
        "IFCCIRCLEPROFILEDEF" => Some(Shape::Circle { d: 2.0 * num(3) }),
        "IFCISHAPEPROFILEDEF" => Some(Shape::I {
            h: num(4),
            b: num(3),
            tw: num(5),
            tf: num(6),
        }),
        "IFCUSHAPEPROFILEDEF" => Some(Shape::Channel {
            h: num(3),
            b: num(4),
            tw: num(5),
            tf: num(6),
        }),
        "IFCLSHAPEPROFILEDEF" => {
            let d = num(3);
            let w = Rd::arg(pe, 4).as_f64().map_or(d, |v| v * rd.scale);
            Some(Shape::Angle {
                h: d,
                b: w,
                t: num(5),
            })
        }
        "IFCTSHAPEPROFILEDEF" => Some(Shape::Tee {
            h: num(3),
            b: num(4),
            tw: num(5),
            tf: num(6),
        }),
        "IFCCIRCLEHOLLOWPROFILEDEF" => Some(Shape::Pipe {
            d: 2.0 * num(3),
            t: num(4),
        }),
        "IFCRECTANGLEHOLLOWPROFILEDEF" => Some(Shape::Tube {
            h: num(4),
            b: num(3),
            t: num(5),
        }),
        _ => None,
    };
    let (shape, sfr) = match parametric {
        Some(s) => (s, pos.then(&rd.axis_placement(Rd::arg(pe, 2))?)),
        None => {
            let area = rd.profile_area(prof, 0)?;
            let [p] = area.as_slice() else {
                return Err("composite profile");
            };
            let [r] = p.as_slice() else {
                return Err("profile with voids");
            };
            let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
            for q in r {
                for k in 0..2 {
                    lo[k] = lo[k].min(q[k]);
                    hi[k] = hi[k].max(q[k]);
                }
            }
            let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, 0.0];
            let poly = r
                .iter()
                .map(|q| (snap(q[0] - c[0]), snap(q[1] - c[1])))
                .collect();
            let mut s = pos;
            s.o = pos.apply(c);
            (Shape::Poly(poly), s)
        }
    };
    let (Some(xw), Some(yw), Some(mut u)) = (norm(sfr.x), norm(sfr.y), norm(v)) else {
        return Err("degenerate member");
    };
    if dot(xw, u).abs() > 1e-6 || dot(yw, u).abs() > 1e-6 {
        return Err("oblique extrusion");
    }
    let mut a = sfr.o;
    let mut b = add(a, v);
    let vertical = u[2].abs() > 1.0 - 1e-9;
    let column = column || vertical;
    if column && u[2] < 0.0 {
        std::mem::swap(&mut a, &mut b);
        u = mul(u, -1.0);
    }
    if column && u[2] < 0.5 {
        return Err("column not upright");
    }
    // member section basis (as the model builds it): ey0 across, ez0 up
    let h = cross([0.0, 0.0, 1.0], u);
    let ey0 = norm(h).unwrap_or([1.0, 0.0, 0.0]);
    let ez0 = cross(u, ey0);
    let mut rot = dot(xw, ez0).atan2(dot(xw, ey0));
    let ez = add(mul(ez0, rot.cos()), mul(ey0, -rot.sin()));
    let mirrored = dot(ez, yw) < 0.0;
    let shape = match (mirrored, shape) {
        (false, s) => s,
        (true, Shape::Tee { h, b, tw, tf }) => {
            rot += std::f64::consts::PI;
            Shape::Tee { h, b, tw, tf }
        }
        (true, Shape::Angle { h, b, t }) => {
            let (y, z) = (b / 2.0, h / 2.0);
            Shape::Poly(vec![
                (-y, z),
                (-y + t, z),
                (-y + t, -z + t),
                (y, -z + t),
                (y, -z),
                (-y, -z),
            ])
        }
        (true, Shape::Poly(p)) => Shape::Poly(p.into_iter().rev().map(|q| (q.0, -q.1)).collect()),
        (true, s) => s,
    };
    shape.check().map_err(|_| "bad profile dimensions")?;
    if ![a, b].iter().flatten().all(|c| c.is_finite()) || !rot.is_finite() {
        return Err("bad geometry");
    }
    let snap3 = |p: V3| [snap(p[0]), snap(p[1]), snap(p[2])];
    let level = level_for(pl, storey, a[2].min(b[2]));
    let type_name = ctx
        .type_name(id)
        .or_else(|| {
            Rd::arg(pe, 1)
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| shape.text());
    pl.members.push(PMember {
        column,
        level,
        shape,
        type_name,
        material: ctx.material_name(id).unwrap_or_default(),
        a: snap3(a),
        b: snap3(b),
        rotation: snap(rot),
        exact: None,
    });
    Ok(())
}

/// Profile of a part's single extruded body: rectangle (x, y) or circle (diameter).
fn part_section(rd: &mut Rd, e: &Entity) -> Option<ddc_geom::curtain::Section> {
    use ddc_geom::curtain::Section;
    let items = rd.rep_items(e, "Body").ok()?;
    let sol = rd.ent(*items.first()?, &["IFCEXTRUDEDAREASOLID"]).ok()?;
    let prof = rd.f.get(Rd::reference(sol, 0).ok()?)?;
    let k = rd.scale;
    match rd.f.type_of(Rd::reference(sol, 0).ok()?)? {
        "IFCRECTANGLEPROFILEDEF" => Some(Section::Rect {
            w: snap(Rd::arg(prof, 3).as_f64()? * k),
            d: snap(Rd::arg(prof, 4).as_f64()? * k),
        }),
        "IFCCIRCLEPROFILEDEF" => Some(Section::Circle {
            d: snap(2.0 * Rd::arg(prof, 3).as_f64()? * k),
        }),
        _ => None,
    }
}

/// Clusters sorted values closer than 1 mm; returns the cluster means.
fn clusters(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(f64::total_cmp);
    let mut out: Vec<(f64, usize)> = vec![];
    for x in v {
        match out.last_mut() {
            Some((s, n)) if x - *s / *n as f64 <= 1e-3 => {
                *s += x;
                *n += 1;
            }
            _ => out.push((x, 1)),
        }
    }
    out.into_iter().map(|(s, n)| snap(s / n as f64)).collect()
}

/// Curtain wall: axis from its Axis representation; members give the grid lines (border ones
/// by their description or position) and sections; plates, doors and windows fill the cells.
fn curtain(
    rd: &mut Rd,
    pl: &mut Plan,
    storey: Option<usize>,
    e: &Entity,
    parts: &[u32],
) -> Result<(), Why> {
    use ddc_geom::curtain::{Axis, Section};
    let fr = rd.product_frame(e)?;
    if fr.z[2].abs() < 1.0 - 1e-9 {
        return Err("non-vertical placement");
    }
    let items = rd.rep_items(e, "Axis")?;
    let arc = rd.arc_axis(&items)?;
    let (p0, p1) = match arc {
        Some((p0, p1, _)) => (p0, p1),
        None => *rd.axis(&items)?.first().ok_or("no axis")?,
    };
    let bulge = arc.map_or(0.0, |(_, _, b)| if fr.z[2] < 0.0 { -b } else { b });
    let (wa, wb) = (fr.apply(p0), fr.apply(p1));
    let (a, b) = ((snap(wa[0]), snap(wa[1])), (snap(wb[0]), snap(wb[1])));
    let ax =
        Axis::new(ddc_geom::v2(a.0, a.1), ddc_geom::v2(b.0, b.1), bulge).map_err(|_| "bad axis")?;
    let len = ax.length();
    if parts.is_empty() {
        return Err("no parts");
    }
    // (kind, s0, s1, z0, z1, name, entity)
    let mut boxes = vec![];
    for &p in parts {
        let Some(ty) = rd.f.type_of(p) else { continue };
        let kind = match ty {
            "IFCPLATE" | "IFCPLATESTANDARDCASE" => "plate",
            "IFCMEMBER" | "IFCMEMBERSTANDARDCASE" => "member",
            "IFCDOOR" | "IFCDOORSTANDARDCASE" => "door",
            "IFCWINDOW" | "IFCWINDOWSTANDARDCASE" => "window",
            _ => return Err("unsupported part"),
        };
        let pe = rd.f.get(p).ok_or("missing entity")?;
        let pts = rd.body_world(pe)?;
        let (mut s0, mut s1, mut z0, mut z1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for q in &pts {
            let s = ax.param_of(ddc_geom::v2(q[0], q[1]));
            s0 = s0.min(s);
            s1 = s1.max(s);
            z0 = z0.min(q[2]);
            z1 = z1.max(q[2]);
        }
        boxes.push((kind, s0, s1, z0, z1, p));
    }
    let zb = boxes.iter().map(|b| b.3).fold(f64::MAX, f64::min);
    let zt = boxes.iter().map(|b| b.4).fold(f64::MIN, f64::max);
    let height = snap(zt - zb);
    if height.is_nan() || height <= 1e-6 {
        return Err("zero height");
    }
    let (mut interior, mut border) = (Section::None, Section::None);
    let (mut us, mut vs) = (vec![], vec![]);
    for &(kind, s0, s1, z0, z1, p) in &boxes {
        if kind != "member" {
            continue;
        }
        let pe = rd.f.get(p).ok_or("missing entity")?;
        let sec = part_section(rd, pe).ok_or("member profile")?;
        let w = sec.width();
        let vertical = z1 - z0 > s1 - s0;
        let (c, lo, hi) = if vertical {
            ((s0 + s1) / 2.0, 0.0, len)
        } else {
            ((z0 + z1) / 2.0 - zb, 0.0, height)
        };
        let desc = Rd::arg(pe, 3).as_str();
        let at_edge = (c - lo - w / 2.0).abs() < 1e-4 || (hi - c - w / 2.0).abs() < 1e-4;
        let is_border = desc.map_or(at_edge, |d| d == "border");
        if is_border {
            border = sec;
        } else {
            interior = sec;
            if vertical {
                us.push(c)
            } else {
                vs.push(c)
            }
        }
    }
    let (wi, wbd) = (interior.width(), border.width());
    let fills: Vec<_> = boxes.iter().filter(|b| b.0 != "member").collect();
    // no interior members: lines halfway between neighbouring panels
    let gaps = |lo: Vec<f64>, hi: Vec<f64>| -> Vec<f64> {
        let (lo, hi) = (clusters(lo), clusters(hi));
        let mut out = vec![];
        for &l in lo.iter().skip(1) {
            if let Some(h) = hi.iter().rev().find(|&&h| h <= l + 1e-6) {
                out.push((h + l) / 2.0);
            }
        }
        out
    };
    if us.is_empty() {
        us = gaps(
            fills.iter().map(|b| b.1).collect(),
            fills.iter().map(|b| b.2).collect(),
        );
    }
    if vs.is_empty() {
        vs = gaps(
            fills.iter().map(|b| b.3 - zb).collect(),
            fills.iter().map(|b| b.4 - zb).collect(),
        );
    }
    let u = clusters(us);
    let v = clusters(vs);
    let un = ddc_geom::curtain::explicit_lines(len, &u);
    let vn = ddc_geom::curtain::explicit_lines(height, &v);
    let cell = |nodes: &[f64], x: f64| {
        nodes
            .windows(2)
            .position(|w| x >= w[0] - 1e-9 && x <= w[1] + 1e-9)
    };
    let mut panels = vec![];
    for &&(kind, s0, s1, z0, z1, p) in &fills {
        let (Some(i), Some(j)) = (cell(&un, (s0 + s1) / 2.0), cell(&vn, (z0 + z1) / 2.0 - zb))
        else {
            return Err("panel outside the grid");
        };
        let pe = rd.f.get(p).ok_or("missing entity")?;
        let name = Rd::arg(pe, 2).as_str().unwrap_or("").to_string();
        let (k, side, hinge) = match kind {
            "door" | "window" => {
                let left = Rd::arg(pe, 11).as_enum() == Some("SINGLE_SWING_LEFT");
                let pf = rd.product_frame(pe)?;
                let mid = ax.point_at((s0 + s1) / 2.0);
                let t = ax.left_at(ax.param_of(mid));
                let side = pf.x[0] * t.y - pf.x[1] * t.x < 0.0;
                let hinge = if left { side } else { !side };
                (kind, side, kind == "door" && hinge)
            }
            _ => (
                if Rd::arg(pe, 3).as_str() == Some("solid") {
                    "solid"
                } else {
                    "glazed"
                },
                false,
                false,
            ),
        };
        panels.push((i, j, k, name, side, k == "door" && hinge));
    }
    panels.sort_by_key(|p| (p.0, p.1));
    if panels
        .windows(2)
        .any(|w| (w[0].0, w[0].1) == (w[1].0, w[1].1))
    {
        return Err("two panels in one cell");
    }
    let level = level_for(pl, storey, zb);
    let _ = (wi, wbd);
    pl.curtains.push(PCurtain {
        level,
        name: Rd::arg(e, 2).as_str().unwrap_or("Curtain wall").to_string(),
        a,
        b,
        bulge,
        base_offset: snap(zb - pl.levels[level].elevation),
        height,
        u,
        v,
        interior,
        border,
        panels,
    });
    Ok(())
}

/// Space: point = placement origin when it lies inside the body profile, else an interior
/// point of the profile; height from the body, else from the Height quantity.
fn space(
    rd: &mut Rd,
    pl: &mut Plan,
    storey: Option<usize>,
    qty_height: Option<f64>,
    e: &Entity,
) -> Result<(), Why> {
    let fr = rd.product_frame(e)?;
    let mut ring = vec![];
    // any other body: the room is its floor (lowest horizontal faces) and full height
    let body = match rd.vertical_prism(e) {
        Err("unsupported body" | "non-vertical extrusion") => {
            product_mesh(rd, e).and_then(|(m, _)| fit::floor(&m))
        }
        r => r,
    };
    let (point, height, z) = match body {
        Ok((rings, zmin, zmax)) => {
            ring = rings[0].clone();
            let rs: Vec<Vec<ddc_geom::V2>> = rings
                .iter()
                .map(|r| r.iter().map(|p| ddc_geom::v2(p.0, p.1)).collect())
                .collect();
            let o = ddc_geom::v2(snap(fr.o[0]), snap(fr.o[1]));
            let inside = ddc_geom::predicates::point_in_ring(o, &rs[0]) > 0
                && rs[1..]
                    .iter()
                    .all(|h| ddc_geom::predicates::point_in_ring(o, h) < 0);
            let p = if inside {
                o
            } else {
                let refs: Vec<&[ddc_geom::V2]> = rs.iter().map(|r| r.as_slice()).collect();
                ddc_geom::rooms::interior_point(&refs).ok_or("bad profile")?
            };
            ((p.x, p.y), snap(zmax - zmin), zmin)
        }
        Err("no body") => {
            let h = qty_height.ok_or("no body")?;
            ((snap(fr.o[0]), snap(fr.o[1])), snap(h), fr.o[2])
        }
        Err(w) => return Err(w),
    };
    if height.is_nan() || height <= 0.0 {
        return Err("zero height");
    }
    let level = level_for(pl, storey, z);
    let txt = |i: usize| Rd::arg(e, i).as_str().unwrap_or("").to_string();
    let (number, long) = (txt(2), txt(7));
    pl.rooms.push(PRoom {
        level,
        name: long,
        number,
        point,
        height,
        ring,
    });
    Ok(())
}

/// Door swing from OperationType and the door frame relative to its host wall axis.
/// Only single-swing doors carry a side; anything else is the default (left of axis, hinge at start).
fn door_swing(rd: &mut Rd, w: &PWall, door: u32) -> (bool, bool) {
    let Ok(e) = rd.ent(door, &["IFCDOOR", "IFCDOORSTANDARDCASE"]) else {
        return (false, false);
    };
    let left = match Rd::arg(e, 11).as_enum() {
        Some("SINGLE_SWING_LEFT") => true,
        Some("SINGLE_SWING_RIGHT") => false,
        _ => return (false, false),
    };
    let Ok(fr) = rd.product_frame(e) else {
        return (false, false);
    };
    let len = (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1);
    if len.is_nan() || len <= 0.0 {
        return (false, false);
    }
    let u = [(w.b.0 - w.a.0) / len, (w.b.1 - w.a.1) / len];
    let side = fr.y[0] * -u[1] + fr.y[1] * u[0] < 0.0;
    let sx = if left { -1.0 } else { 1.0 };
    let hinge = sx * (fr.x[0] * u[0] + fr.x[1] * u[1]) > 0.0;
    (side, hinge)
}

/// Separator: virtual element with a straight two-point axis curve.
fn separator(rd: &mut Rd, pl: &mut Plan, storey: Option<usize>, e: &Entity) -> Result<(), Why> {
    let fr = rd.product_frame(e)?;
    if fr.z[2].abs() < 1.0 - 1e-9 {
        return Err("non-vertical placement");
    }
    let items = rd.rep_items(e, "Axis")?;
    let segs = rd.axis(&items)?;
    let &[(p, q)] = segs.as_slice() else {
        return Err("unsupported axis");
    };
    let (p, q) = (fr.apply(p), fr.apply(q));
    let (a, b) = ((snap(p[0]), snap(p[1])), (snap(q[0]), snap(q[1])));
    if (a.0 - b.0).hypot(a.1 - b.1) <= 1e-9 {
        return Err("zero length");
    }
    let level = level_for(pl, storey, fr.o[2]);
    pl.seps.push(PSep { level, a, b });
    Ok(())
}

fn opening(rd: &mut Rd, w: &PWall, base_z: f64, e: &Entity) -> Result<(f64, f64, f64, f64), Why> {
    let pts = rd.body_world(e)?;
    if w.bulge != 0.0 {
        return arc_opening(w, base_z, &pts);
    }
    let len = (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1);
    let d = [(w.b.0 - w.a.0) / len, (w.b.1 - w.a.1) / len];
    let (mut lo, mut hi, mut zlo, mut zhi) = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    for p in &pts {
        let s = (p[0] - w.a.0) * d[0] + (p[1] - w.a.1) * d[1];
        lo = lo.min(s);
        hi = hi.max(s);
        zlo = zlo.min(p[2]);
        zhi = zhi.max(p[2]);
    }
    let along = snap(lo);
    let width = snap(hi - lo);
    let sill = snap(zlo - base_z);
    let height = snap(zhi - zlo);
    if !(width > 0.0 && height > 0.0) {
        return Err("empty body");
    }
    if hi < -1e-9 || lo > len + 1e-9 {
        return Err("outside host wall");
    }
    Ok((along, width, sill, height))
}

/// Opening box on an arc host: centred on the arc, width along the tangent at its centre.
fn arc_opening(w: &PWall, base_z: f64, pts: &[V3]) -> Result<(f64, f64, f64, f64), Why> {
    use ddc_geom::arc::ArcAxis;
    let ax = ArcAxis::new(
        ddc_geom::v2(w.a.0, w.a.1),
        ddc_geom::v2(w.b.0, w.b.1),
        w.bulge,
        ddc_geom::Tol::default(),
    )
    .map_err(|_| "degenerate arc host")?;
    if pts.is_empty() {
        return Err("empty body");
    }
    let n = pts.len() as f64;
    let m = ddc_geom::v2(
        pts.iter().map(|p| p[0]).sum::<f64>() / n,
        pts.iter().map(|p| p[1]).sum::<f64>() / n,
    );
    let sc = ax.param_of(m);
    let t = ax.tangent_at(sc);
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut zlo, mut zhi) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in pts {
        let s = (p[0] - m.x) * t.x + (p[1] - m.y) * t.y;
        lo = lo.min(s);
        hi = hi.max(s);
        zlo = zlo.min(p[2]);
        zhi = zhi.max(p[2]);
    }
    let width = snap(hi - lo);
    let along = snap(sc - width / 2.0);
    let (sill, height) = (snap(zlo - base_z), snap(zhi - zlo));
    if !(width > 0.0 && height > 0.0) {
        return Err("empty body");
    }
    if along < -1e-9 || along + width > ax.length() + 1e-9 {
        return Err("outside host wall");
    }
    Ok((along, width, sill, height))
}

/// Value of quantity `name` in an IfcElementQuantity, in file units.
/// Quantity value in project units: its own unit if given, else the project length unit (`scale`).
fn quantity(f: &StepFile, q: &u32, name: &str, scale: f64) -> Option<f64> {
    let e = f.get(*q)?;
    if f.type_of(*q) != Some("IFCELEMENTQUANTITY") {
        return None;
    }
    Rd::refs(e, 5).into_iter().find_map(|x| {
        let qe = f.get(x)?;
        if Rd::arg(qe, 0).as_str() != Some(name) {
            return None;
        }
        let v = Rd::arg(qe, 3).as_f64()?;
        let k = match Rd::arg(qe, 2).as_ref() {
            Some(u) => unit_factor(f, u).unwrap_or(scale),
            None => scale,
        };
        Some(v * k)
    })
}

fn unique(db: &Connection, table: &str, name: &str) -> Result<String, rusqlite::Error> {
    let taken = |n: &str| -> Result<bool, rusqlite::Error> {
        db.query_row(
            &format!("SELECT count(*) FROM {table} WHERE name=?1"),
            [n],
            |r| r.get::<_, i64>(0),
        )
        .map(|c| c > 0)
    };
    let mut k = 2;
    let mut n = name.to_string();
    while taken(&n)? {
        n = format!("{name} ({k})");
        k += 1;
    }
    Ok(n)
}

/// Imports IFC bytes into the model as one undoable operation.
pub fn import_ifc(model: &mut Model, bytes: &[u8]) -> Result<ImportReport, ImportError> {
    import_plan(
        model,
        plan_bytes(bytes)?,
        ddc_model::source::Policy::default(),
    )
}

/// Commits a plan as one undo step: a re-import of the same project merges with the last import
/// (local edits stay, see `source.rs`); `policy` decides about removed elements and the guard.
pub fn import_plan(
    model: &mut Model,
    pl: Plan,
    policy: ddc_model::source::Policy,
) -> Result<ImportReport, ImportError> {
    let sync = model
        .batch_walls_edit("wall_import_ifc", |db, snaps| {
            let sync = commit::commit_with(db, snaps, &pl, policy)?;
            apply_project_info(db, snaps, &pl)?;
            Ok(sync)
        })
        .map_err(|e| ImportError::Model(e.to_string()))?;
    let mut rep = pl.report;
    sync.fill(&mut rep);
    Ok(rep)
}

/// Fills the model's empty project fields from the file inside the import batch (one undo step
/// with the import). Non-empty local values stay; a fresh model takes the file's values.
pub(in crate::ifc) fn apply_project_info(
    db: &Connection,
    snaps: &mut ddc_model::Snapshots,
    pl: &Plan,
) -> Result<(), ddc_model::Error> {
    if let Some(found) = &pl.site_crs {
        ddc_model::fill_site_in_batch(db, snaps, found)?;
    }
    let Some(file) = &pl.project_info else {
        return Ok(());
    };
    let cur = ddc_model::read_project_info(db)?;
    ddc_model::write_project_info_in_batch(db, snaps, &project::merge(&cur, file))?;
    Ok(())
}

/// Plan of `.ifc` or IFCZIP bytes; damaged references become report warnings.
pub fn plan_bytes(bytes: &[u8]) -> Result<Plan, ImportError> {
    let unzipped;
    let b = if super::zip::is_zip(bytes) {
        unzipped = super::zip::ifc_member(bytes).map_err(ImportError::Io)?;
        &unzipped[..]
    } else {
        bytes
    };
    let f = StepFile::parse_tolerant(b)?;
    plan(&f)
}

pub fn import_ifc_file(model: &mut Model, path: &Path) -> Result<ImportReport, ImportError> {
    import_ifc_file_with(model, path, ddc_model::source::Policy::default())
}

/// [`import_ifc_file`] with a merge policy; the file's hash and stat become the source's.
pub fn import_ifc_file_with(
    model: &mut Model,
    path: &Path,
    policy: ddc_model::source::Policy,
) -> Result<ImportReport, ImportError> {
    let stat = ddc_model::source::file::stat_of(path).ok();
    let b = std::fs::read(path).map_err(|e| ImportError::Io(e.to_string()))?;
    let mut pl = plan_bytes(&b)?;
    pl.file = IfcFile::from_bytes(path, &b, stat);
    import_plan(model, pl, policy)
}

/// `.ifc` or `.ifczip` (first `.ifc` entry of the archive) into the model, one undo step.
/// The archive is detected by its signature, so a mislabelled extension still works.
pub fn import_ifc_path(model: &mut Model, path: &Path) -> Result<ImportReport, ImportError> {
    import_ifc_file(model, path)
}

#[cfg(test)]
use super::zip::ifczip_entry_ok;

#[cfg(test)]
mod ifczip_tests {
    #[test]
    fn macosx_metadata_entries_are_skipped() {
        use super::ifczip_entry_ok as ok;
        assert!(ok("model.IFC"));
        assert!(ok("dir/model.ifc"));
        assert!(!ok("__MACOSX/._model.ifc"));
        assert!(!ok("dir/._model.ifc"));
        assert!(!ok("x/__MACOSX/model.ifc"));
        assert!(!ok("model.ifcxml"));
    }
}

/// Registry entry: `.ifc` → the generic sink (one element per wall/opening with plan values).
/// Native import into a model is [`import_ifc_file`].
pub struct IfcImporter;

impl crate::Importer for IfcImporter {
    fn extensions(&self) -> &[&str] {
        &["ifc", "ifczip"]
    }
    fn import(&self, path: &Path, sink: &mut dyn crate::Sink) -> Result<(), String> {
        let b = std::fs::read(path).map_err(|e| e.to_string())?;
        let pl = plan_bytes(&b).map_err(|e| e.to_string())?;
        for w in &pl.walls {
            let th: f64 = pl.types[w.wall_type].layers.iter().map(|l| l.1).sum();
            sink.element(
                "wall",
                &[
                    ("ax", w.a.0),
                    ("ay", w.a.1),
                    ("bx", w.b.0),
                    ("by", w.b.1),
                    ("elevation", pl.levels[w.level].elevation + w.base_offset),
                    ("height", w.height),
                    ("thickness", th),
                ],
            );
        }
        for o in &pl.openings {
            sink.element(
                &o.kind,
                &[
                    ("wall", o.wall as f64),
                    ("along", o.along),
                    ("width", o.width),
                    ("sill", o.sill),
                    ("height", o.height),
                ],
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests_any;
#[cfg(test)]
mod tests_storey;

#[cfg(test)]
mod tests_children;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ifc::export_ifc;
    use ddc_model::{Boundary, LayerSpec, OpeningSpec, SepSpec, Swing, WallSpec};

    type Snapshot = (
        Vec<(String, f64)>,
        Vec<(String, Vec<(String, f64)>)>,
        Vec<(String, String, [f64; 6], Option<f64>)>,
        Vec<(f64, f64, f64, f64, String)>,
        Vec<(String, f64)>,
    );

    fn rows<T>(m: &Model, sql: &str, f: impl Fn(&rusqlite::Row) -> rusqlite::Result<T>) -> Vec<T> {
        let mut st = m.sql().prepare(sql).unwrap();
        let v = st.query_map([], f).unwrap().map(Result::unwrap).collect();
        v
    }

    fn snapshot(m: &Model) -> Snapshot {
        let levels = rows(m, "SELECT name, elevation FROM level ORDER BY id", |r| {
            Ok((r.get(0)?, r.get(1)?))
        });
        let tids: Vec<(i64, String)> = rows(m, "SELECT id, name FROM wall_type ORDER BY id", |r| {
            Ok((r.get(0)?, r.get(1)?))
        });
        let types = tids
            .into_iter()
            .map(|(id, n)| {
                let ls = rows(
                    m,
                    &format!(
                        "SELECT material, thickness FROM wall_type_layer WHERE wtype={id} ORDER BY pos"
                    ),
                    |r| Ok((r.get(0)?, r.get(1)?)),
                );
                (n, ls)
            })
            .collect();
        let walls = rows(
            m,
            "SELECT l.name, t.name, ax, ay, bx, by, base_offset, w.height, offset0 \
             FROM wall w JOIN level l ON l.id=w.level JOIN wall_type t ON t.id=w.wtype ORDER BY w.id",
            |r| {
                let mut f = [0.0; 6];
                for (k, s) in f.iter_mut().enumerate() {
                    *s = r.get(k + 2)?;
                }
                Ok((r.get(0)?, r.get(1)?, f, r.get(8)?))
            },
        );
        let ops = rows(
            m,
            "SELECT along, width, sill, height, kind FROM opening ORDER BY id",
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        );
        (levels, types, walls, ops, m.material_volumes().unwrap())
    }

    fn layer(material: &str, thickness: f64) -> LayerSpec {
        LayerSpec {
            material: material.into(),
            thickness,
        }
    }

    fn source_model() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let l1 = m.add_level("L1", 0.0).unwrap();
        let l2 = m.add_level(crate::ru_fixtures::ru("storey_2"), 3.2).unwrap();
        let ext = m
            .add_wall_type("ext", &[layer("brick", 0.2), layer(crate::ru_fixtures::ru("insulation_lc"), 0.1)])
            .unwrap();
        let int = m.add_wall_type("int", &[layer("gypsum", 0.125)]).unwrap();
        let wall = |m: &mut Model, a, b, level, base_offset, height, wall_type, offset0| {
            m.add_wall(&WallSpec {
                a,
                b,
                level,
                base_offset,
                height,
                wall_type,
                offset0,
            })
            .unwrap()
        };
        let pts = [(0., 0.), (6., 0.), (6., 4.), (0., 4.), (0., 0.)];
        let ids: Vec<i64> = (0..4)
            .map(|k| wall(&mut m, pts[k], pts[k + 1], l1, 0.0, 3.0, ext, None))
            .collect();
        wall(
            &mut m,
            (3.0, 0.15),
            (3.0, 3.85),
            l1,
            0.0,
            2.7,
            int,
            Some(0.0),
        );
        wall(&mut m, (0.0, 0.0), (5.0, 0.0), l2, 0.1, 2.9, ext, None);
        for (w, kind, along, width, sill, height) in [
            (0, "door", 1.5, 0.9, 0.0, 2.1),
            (1, "window", 1.0, 1.2, 0.9, 1.4),
            (2, "void", 2.0, 0.6, 0.3, 0.6),
        ] {
            let o = OpeningSpec {
                along,
                width,
                sill,
                height,
                kind: kind.into(),
            };
            m.add_opening(ids[w], &o).unwrap();
        }
        m
    }

    #[test]
    fn round_trip_is_exact() {
        let src = source_model();
        let text = export_ifc(&src).unwrap();
        let mut dst = Model::open(":memory:").unwrap();
        let rep = import_ifc(&mut dst, text.as_bytes()).unwrap();
        assert_eq!(
            (rep.levels, rep.wall_types, rep.walls, rep.openings),
            (2, 2, 6, 3)
        );
        assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
        assert_eq!(snapshot(&src), snapshot(&dst));
        // a second import of the same project matches rows by GlobalId: nothing changes
        let before = snapshot(&dst);
        assert!(dst.wall_warnings().unwrap().is_empty());
        let again = import_ifc(&mut dst, text.as_bytes()).unwrap();
        assert_eq!(again.updated, 0, "{again:?}");
        assert_eq!(snapshot(&dst), before);
        // an edited model with an overlapping corner exports (butt ends) and imports again
        let (a, b, level, wall_type): ((f64, f64), (f64, f64), i64, i64) = dst
            .sql()
            .query_row(
                "SELECT ax,ay,bx,by,level,wtype FROM wall ORDER BY id LIMIT 1",
                [],
                |r| {
                    Ok((
                        (r.get(0)?, r.get(1)?),
                        (r.get(2)?, r.get(3)?),
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        let spec = WallSpec {
            a,
            b,
            level,
            base_offset: 0.0,
            height: 3.0,
            wall_type,
            offset0: None,
        };
        dst.add_wall(&spec).unwrap();
        assert!(!dst.wall_warnings().unwrap().is_empty());
        let edited = export_ifc(&dst).unwrap();
        let mut back = Model::open(":memory:").unwrap();
        let rep = import_ifc(&mut back, edited.as_bytes()).unwrap();
        assert_eq!(rep.walls, 7, "{rep:?}");
        assert!(!back.wall_warnings().unwrap().is_empty());
        dst.undo().unwrap();
        assert_eq!(snapshot(&dst), before);
        // the import is one undo step
        let mut one = Model::open(":memory:").unwrap();
        import_ifc(&mut one, text.as_bytes()).unwrap();
        one.undo().unwrap();
        let empty = snapshot(&one);
        assert!(empty.0.is_empty() && empty.2.is_empty() && empty.4.is_empty());
    }

    fn grid_rows(
        m: &Model,
    ) -> Vec<(
        String,
        f64,
        f64,
        f64,
        f64,
        f64,
        Option<String>,
        Option<String>,
        i64,
    )> {
        rows(
            m,
            "SELECT g.name, g.ax, g.ay, g.bx, g.by, g.bulge, b.name, t.name, g.bubbles FROM grid g              LEFT JOIN level b ON b.id=g.base_level LEFT JOIN level t ON t.id=g.top_level ORDER BY g.id",
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
    }

    #[test]
    fn grid_5x4_round_trip_is_identical() {
        let mut src = Model::open(":memory:").unwrap();
        let l1 = src.add_level("L1", 0.0).unwrap();
        let l2 = src.add_level("L2", 3.3).unwrap();
        src.add_grid_row("1", (0.0, -1.5), (0.0, 13.5), (6.0, 0.0), 5)
            .unwrap();
        src.add_grid_row("A", (-1.5, 0.0), (25.5, 0.0), (0.0, 4.0), 4)
            .unwrap();
        // an axis with a level range, a single bubble, and an arc axis
        let mut g = ddc_model::GridSpec::line("E", (-1.5, 13.1), (25.5, 13.1));
        g.base_level = Some(l1);
        g.top_level = Some(l2);
        g.bubble_end = false;
        src.add_grid(&g).unwrap();
        src.add_grid(&ddc_model::GridSpec {
            bulge: 0.3,
            ..ddc_model::GridSpec::line("R1", (30.0, 0.0), (30.0, 12.0))
        })
        .unwrap();
        let text = export_ifc(&src).unwrap();
        assert_eq!(text.matches("IFCGRIDAXIS(").count(), 11);
        let mut dst = Model::open(":memory:").unwrap();
        let rep = import_ifc(&mut dst, text.as_bytes()).unwrap();
        assert_eq!(rep.grids, 11);
        assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
        assert_eq!(grid_rows(&src), grid_rows(&dst));
        assert_eq!(
            src.grid_intersections().unwrap(),
            dst.grid_intersections().unwrap()
        );
        assert_eq!(dst.grid_intersections().unwrap().len(), 20 + 5);
        dst.undo().unwrap();
        assert!(grid_rows(&dst).is_empty());
    }

    /// 6x4 walls 0.3 thick, a room inside, a 6x4 slab 0.2 with a 1x1 hole, an unenclosed room.
    pub(crate) fn slab_room_model() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m.add_wall_type("w", &[layer("brick", 0.3)]).unwrap();
        let p = [(0., 0.), (6., 0.), (6., 4.), (0., 4.)];
        let ws: Vec<WallSpec> = (0..4)
            .map(|i| WallSpec {
                a: p[i],
                b: p[(i + 1) % 4],
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .collect();
        m.add_walls(&ws).unwrap();
        let st = m
            .add_slab_type("floor", &[layer("screed", 0.05), layer("concrete", 0.15)])
            .unwrap();
        m.add_slab(&ddc_model::SlabSpec {
            level: lv,
            slab_type: st,
            offset: 0.0,
            outer: p.to_vec(),
            holes: vec![vec![(1., 1.), (2., 1.), (2., 2.), (1., 2.)]],
        })
        .unwrap();
        for (name, number, pt) in [("Office", "101", (3., 2.)), ("Yard", "X1", (10., 10.))] {
            m.add_room(&ddc_model::RoomSpec {
                level: lv,
                name: name.into(),
                number: number.into(),
                point: pt,
                height: 2.7,
            })
            .unwrap();
        }
        m
    }

    type Rooms = Vec<(String, String, f64, f64, f64)>;
    type Slabs = Vec<(f64, Vec<Vec<(f64, f64)>>, Vec<(String, f64, f64)>)>;

    fn rooms_slabs(m: &Model) -> (Rooms, Slabs, Vec<Option<(f64, f64)>>) {
        let rooms = rows(
            m,
            "SELECT name,number,px,py,height FROM room ORDER BY id",
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        );
        let ids: Vec<i64> = rows(m, "SELECT id FROM room ORDER BY id", |r| r.get(0));
        let q = ids
            .iter()
            .map(|&i| m.room_quantities(i).unwrap().map(|q| (q.area, q.perimeter)))
            .collect();
        let sids: Vec<(i64, f64)> = rows(m, "SELECT id, offset FROM slab ORDER BY id", |r| {
            Ok((r.get(0)?, r.get(1)?))
        });
        let slabs = sids
            .iter()
            .map(|&(i, off)| {
                let q = m.slab_quantities(i).unwrap();
                (
                    off,
                    m.slab_rings(i).unwrap(),
                    q.into_iter()
                        .map(|l| (l.material, l.thickness, l.volume))
                        .collect(),
                )
            })
            .collect();
        (rooms, slabs, q)
    }

    /// Eight doors on three walls (one reversed, one diagonal) covering every side/hinge pair.
    pub(crate) fn swing_model() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m.add_wall_type("w", &[layer("brick", 0.25)]).unwrap();
        let axes = [
            ((0., 0.), (8., 0.)),
            ((8., 5.), (0., 5.)),
            ((0., 8.), (6., 14.)),
        ];
        let ids: Vec<i64> = axes
            .iter()
            .map(|&(a, b)| {
                m.add_wall(&WallSpec {
                    a,
                    b,
                    level: lv,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: t,
                    offset0: None,
                })
                .unwrap()
            })
            .collect();
        for (w, along, side, hinge) in [
            (0, 1.0, false, false),
            (0, 3.0, false, true),
            (0, 5.0, true, false),
            (0, 6.5, true, true),
            (1, 1.0, false, true),
            (1, 3.0, true, false),
            (2, 1.0, true, true),
            (2, 3.0, false, false),
        ] {
            let o = OpeningSpec {
                along,
                width: 0.9,
                sill: 0.0,
                height: 2.1,
                kind: "door".into(),
            };
            m.add_opening_with(ids[w], &o, Swing { side, hinge })
                .unwrap();
        }
        m
    }

    fn swings(m: &Model) -> Vec<(f64, Swing)> {
        let ids: Vec<(i64, f64)> = rows(m, "SELECT id, along FROM opening ORDER BY id", |r| {
            Ok((r.get(0)?, r.get(1)?))
        });
        ids.into_iter()
            .map(|(i, a)| (a, m.opening_swing(i).unwrap()))
            .collect()
    }

    #[test]
    fn door_swing_round_trips_all_combinations() {
        let src = swing_model();
        let text = export_ifc(&src).unwrap();
        // LEFT when side == hinge: (f,f) (t,t) (t,t) (f,f); RIGHT for the other four
        assert_eq!(text.matches(".SINGLE_SWING_LEFT.").count(), 4);
        assert_eq!(text.matches(".SINGLE_SWING_RIGHT.").count(), 4);
        let mut dst = Model::open(":memory:").unwrap();
        let rep = import_ifc(&mut dst, text.as_bytes()).unwrap();
        assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
        assert_eq!(swings(&src), swings(&dst));
        assert_eq!(
            export_ifc(&dst)
                .unwrap()
                .matches(".SINGLE_SWING_LEFT.")
                .count(),
            4
        );
        dst.undo().unwrap();
        assert!(swings(&dst).is_empty());
    }

    /// 8x4 room (walls 0.2) cut by a separator in two rooms, plus an unenclosed room.
    pub(crate) fn separated_model() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m.add_wall_type("w", &[layer("brick", 0.2)]).unwrap();
        let p = [(0., 0.), (8., 0.), (8., 4.), (0., 4.)];
        let ws: Vec<WallSpec> = (0..4)
            .map(|i| WallSpec {
                a: p[i],
                b: p[(i + 1) % 4],
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .collect();
        m.add_walls(&ws).unwrap();
        m.add_room_sep(&SepSpec {
            level: lv,
            a: (4., 0.),
            b: (4., 4.),
        })
        .unwrap();
        for (name, number, pt) in [
            ("West", "1", (1., 2.)),
            ("East", "2", (6., 2.)),
            ("Yard", "3", (20., 20.)),
        ] {
            m.add_room(&ddc_model::RoomSpec {
                level: lv,
                name: name.into(),
                number: number.into(),
                point: pt,
                height: 2.7,
            })
            .unwrap();
        }
        m
    }

    #[test]
    fn dangling_space_boundary_and_tilted_separator_are_reported() {
        let text = export_ifc(&separated_model()).unwrap();
        let rel = text
            .lines()
            .find(|l| l.contains("=IFCRELSPACEBOUNDARY("))
            .unwrap();
        let args: Vec<&str> = rel.split(',').collect();
        for ix in [4usize, 5] {
            let mut a = args.clone();
            a[ix] = "$";
            let bad = text.replace(rel, &a.join(","));
            let pl = plan(&StepFile::parse(bad.as_bytes()).unwrap()).unwrap();
            assert_eq!(
                pl.report
                    .skipped
                    .get("IFCRELSPACEBOUNDARY: missing space or element reference"),
                Some(&1),
                "{ix} {:?}",
                pl.report.skipped
            );
            assert_eq!(pl.report.space_boundaries, 7);
        }
        // tilt the separator's placement axis
        let ve = text
            .lines()
            .find(|l| l.contains("=IFCVIRTUALELEMENT("))
            .unwrap();
        let place = ve.split(',').nth(5).unwrap().to_string();
        let lp = text
            .lines()
            .find(|l| l.starts_with(&format!("{}=IFCLOCALPLACEMENT(", place)))
            .unwrap();
        let ax = lp
            .trim_end_matches(");")
            .rsplit(',')
            .next()
            .unwrap()
            .to_string();
        let ap = text
            .lines()
            .find(|l| l.starts_with(&format!("{}=IFCAXIS2PLACEMENT3D(", ax)))
            .unwrap();
        let tilted = text.replace(
            ap,
            &format!(
                "{}\n#99999=IFCDIRECTION((0.,0.5,1.));",
                {
                    let mut parts: Vec<&str> = ap.trim_end_matches(");").split(',').collect();
                    parts[1] = "#99999";
                    format!("{});", parts.join(","))
                }
            ),
        );
        let pl = plan(&StepFile::parse(tilted.as_bytes()).unwrap()).unwrap();
        assert_eq!(pl.report.separators, 0, "{:?}", pl.report.skipped);
        assert_eq!(
            pl.report
                .skipped
                .get("IFCVIRTUALELEMENT: non-vertical placement"),
            Some(&1),
            "{:?}",
            pl.report.skipped
        );
    }

    #[test]
    fn space_boundary_and_separator_round_trip() {
        let src = separated_model();
        let text = export_ifc(&src).unwrap();
        assert_eq!(text.matches("=IFCRELSPACEBOUNDARY(").count(), 8);
        // the four walls are the outer shell: their boundaries are external
        assert_eq!(text.matches(",.PHYSICAL.,.EXTERNAL.)").count(), 6);
        assert_eq!(text.matches(",.VIRTUAL.,.INTERNAL.)").count(), 2);
        assert_eq!(text.matches("=IFCVIRTUALELEMENT(").count(), 1);
        assert_eq!(text, export_ifc(&src).unwrap());
        let f = StepFile::parse(text.as_bytes()).unwrap();
        let pl = plan(&f).unwrap();
        assert_eq!((pl.report.separators, pl.report.space_boundaries), (1, 8));
        assert!(pl.report.skipped.is_empty(), "{:?}", pl.report.skipped);
        let mut dst = Model::open(":memory:").unwrap();
        import_ifc(&mut dst, text.as_bytes()).unwrap();
        assert_eq!(dst.room_seps(1).unwrap(), src.room_seps(1).unwrap());
        for room in 1..=3 {
            assert_eq!(
                dst.room_boundaries(room).unwrap(),
                src.room_boundaries(room).unwrap()
            );
        }
        // every relation read from the file is a boundary of the imported model
        for b in &pl.boundaries {
            let want = match b.target {
                PTarget::Wall(w) => Boundary::Wall(w as i64 + 1),
                PTarget::Sep(x) => Boundary::Sep(x as i64 + 1),
            };
            assert!(dst
                .room_boundaries(b.room as i64 + 1)
                .unwrap()
                .contains(&want));
        }
        assert_eq!(
            dst.room_boundaries(1).unwrap(),
            vec![
                Boundary::Wall(1),
                Boundary::Wall(3),
                Boundary::Wall(4),
                Boundary::Sep(1)
            ]
        );
        assert!(dst.room_boundaries(3).unwrap().is_empty());
        let q = dst.room_quantities(1).unwrap().unwrap();
        assert!((q.area - 14.82).abs() < 1e-9);
        dst.undo().unwrap();
        assert!(dst.room_seps(1).unwrap().is_empty());
    }

    #[test]
    fn slabs_and_rooms_round_trip() {
        let src = slab_room_model();
        let text = export_ifc(&src).unwrap();
        for (ent, n) in [
            ("IFCSLAB", 1),
            ("IFCSPACE", 2),
            ("IFCARBITRARYPROFILEDEFWITHVOIDS", 1),
        ] {
            assert_eq!(text.matches(&format!("={ent}(")).count(), n, "{ent}");
        }
        assert!(text.contains("'Qto_SlabBaseQuantities'"));
        assert_eq!(text.matches("'Qto_SpaceBaseQuantities'").count(), 2);
        assert!(text.contains(".AXIS3.,.NEGATIVE."));
        let mut dst = Model::open(":memory:").unwrap();
        let rep = import_ifc(&mut dst, text.as_bytes()).unwrap();
        assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
        assert_eq!((rep.slabs, rep.rooms), (1, 2));
        let (a, b) = (rooms_slabs(&src), rooms_slabs(&dst));
        assert_eq!(a, b);
        let (_, slabs, q) = b;
        let vol: f64 = slabs[0].2.iter().map(|l| l.2).sum();
        assert!((vol - 23.0 * 0.2).abs() < 1e-9, "{vol}");
        assert_eq!(slabs[0].2[0].0, "screed");
        let (area, per) = q[0].unwrap();
        assert!((area - 21.09).abs() < 1e-9 && (per - 18.8).abs() < 1e-9);
        assert_eq!(q[1], None);
        assert_eq!(dst.check_integrity().unwrap(), vec![]);
    }

    #[test]
    fn room_empty_name_and_height_unit_round_trip() {
        let mut src = slab_room_model();
        let lv = rows(&src, "SELECT id FROM level", |r| r.get::<_, i64>(0))[0];
        src.add_room(&ddc_model::RoomSpec {
            level: lv,
            name: String::new(),
            number: "102".into(),
            point: (20., 20.),
            height: 2.7,
        })
        .unwrap();
        let text = export_ifc(&src).unwrap();
        let mut dst = Model::open(":memory:").unwrap();
        import_ifc(&mut dst, text.as_bytes()).unwrap();
        let names = |m: &Model| -> Vec<(String, String)> {
            rows(m, "SELECT name,number FROM room ORDER BY id", |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
        };
        assert_eq!(names(&src), names(&dst));
        // Unenclosed space has no body: height comes from its quantity, in the quantity's own unit.
        let k = text.rfind("ENDSEC;").unwrap();
        let mut t2 = format!(
            "{}#99999=IFCSIUNIT(*,.LENGTHUNIT.,.CENTI.,.METRE.);\n{}",
            &text[..k],
            &text[k..]
        );
        let mut out = String::new();
        let mut hit = 0;
        while let Some(i) = t2.find("IFCQUANTITYLENGTH('Height',$,#") {
            let j = i + "IFCQUANTITYLENGTH('Height',$,#".len();
            let k = t2[j..].find(',').unwrap() + j;
            out.push_str(&t2[..j]);
            out.push_str("99999");
            // value 2.7 m -> 270 cm
            let rest = &t2[k..];
            let e = rest.find(",$)").unwrap();
            out.push_str(",270.");
            t2 = rest[e..].to_string();
            hit += 1;
        }
        out.push_str(&t2);
        assert!(hit >= 1);
        let mut d2 = Model::open(":memory:").unwrap();
        import_ifc(&mut d2, out.as_bytes()).unwrap();
        let h: Vec<f64> = rows(&d2, "SELECT height FROM room ORDER BY id", |r| r.get(0));
        assert!(h.iter().all(|v| (v - 2.7).abs() < 1e-9), "{h:?}");
    }

    #[test]
    fn diagonal_wall_round_trip_within_nanometre() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L", 0.0).unwrap();
        let t = m.add_wall_type("t", &[layer("c", 0.3)]).unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (1.0, 2.0),
                b: (4.7, 5.3),
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        let o = OpeningSpec {
            along: 1.0,
            width: 1.0,
            sill: 0.5,
            height: 1.0,
            kind: "window".into(),
        };
        m.add_opening(w, &o).unwrap();
        let mut d = Model::open(":memory:").unwrap();
        import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        let (a, b) = (snapshot(&m), snapshot(&d));
        for (x, y) in a.2[0].2.iter().zip(b.2[0].2.iter()) {
            assert!((x - y).abs() <= 1e-9, "{x} vs {y}");
        }
        assert!((a.3[0].0 - b.3[0].0).abs() <= 1e-9);
        assert!((a.4[0].1 - b.4[0].1).abs() <= 1e-9);
    }

    /// Hand-written IFC2X3, millimetres, wall rotated 90°, door cut through local Y.
    pub(crate) const SAMPLE_2X3: &str = r"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');
FILE_NAME('sample.ifc','2020-01-01T00:00:00',(''),(''),'','','');
FILE_SCHEMA(('IFC2X3'));
ENDSEC;
DATA;
#1=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#2=IFCUNITASSIGNMENT((#1));
#3=IFCPROJECT('0000000000000000000001',$,'P',$,$,$,$,$,#2);
#10=IFCCARTESIANPOINT((0.,0.,0.));
#11=IFCAXIS2PLACEMENT3D(#10,$,$);
#12=IFCLOCALPLACEMENT($,#11);
#13=IFCCARTESIANPOINT((0.,0.,3000.));
#14=IFCAXIS2PLACEMENT3D(#13,$,$);
#15=IFCLOCALPLACEMENT(#12,#14);
#16=IFCBUILDINGSTOREY('0000000000000000000002',$,'\X2\042D044204300436\X0\ 2',$,$,#15,$,$,.ELEMENT.,3000.);
/* wall along +Y from (1000,2000), 4000 long */
#20=IFCCARTESIANPOINT((1000.,2000.,0.));
#21=IFCDIRECTION((0.,0.,1.));
#22=IFCDIRECTION((0.,1.,0.));
#23=IFCAXIS2PLACEMENT3D(#20,#21,#22);
#24=IFCLOCALPLACEMENT(#15,#23);
#30=IFCCARTESIANPOINT((0.,0.));
#31=IFCCARTESIANPOINT((4000.,0.));
#32=IFCPOLYLINE((#30,#31));
#33=IFCSHAPEREPRESENTATION($,'Axis','Curve2D',(#32));
#34=IFCCARTESIANPOINT((2000.,0.));
#35=IFCAXIS2PLACEMENT2D(#34,$);
#36=IFCRECTANGLEPROFILEDEF(.AREA.,$,#35,4000.,250.);
#37=IFCAXIS2PLACEMENT3D(#10,$,$);
#38=IFCEXTRUDEDAREASOLID(#36,#37,#21,2800.);
#39=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#38));
#40=IFCPRODUCTDEFINITIONSHAPE($,$,(#33,#39));
#41=IFCWALLSTANDARDCASE('0000000000000000000003',$,'\X2\042104420435043D0430\X0\',$,$,#24,#40,$);
#50=IFCMATERIAL('Brick');
#51=IFCMATERIAL('\X2\042304420435043F043B043804420435043B044C\X0\');
#52=IFCMATERIALLAYER(#50,200.,$);
#53=IFCMATERIALLAYER(#51,50.,$);
#54=IFCMATERIALLAYERSET((#52,#53),'Ext 250');
#55=IFCMATERIALLAYERSETUSAGE(#54,.AXIS2.,.POSITIVE.,-125.);
#56=IFCRELASSOCIATESMATERIAL('0000000000000000000004',$,$,$,(#41),#55);
#57=IFCRELCONTAINEDINSPATIALSTRUCTURE('0000000000000000000005',$,$,$,(#41,#70,#80),#16);
/* opening: profile in the wall's XZ plane, extruded through -Y */
#60=IFCCARTESIANPOINT((1000.,200.,0.));
#61=IFCAXIS2PLACEMENT3D(#60,$,$);
#62=IFCLOCALPLACEMENT(#24,#61);
#63=IFCCARTESIANPOINT((450.,1050.));
#64=IFCAXIS2PLACEMENT2D(#63,$);
#65=IFCRECTANGLEPROFILEDEF(.AREA.,$,#64,900.,2100.);
#66=IFCDIRECTION((0.,-1.,0.));
#67=IFCDIRECTION((1.,0.,0.));
#68=IFCAXIS2PLACEMENT3D(#10,#66,#67);
#69=IFCEXTRUDEDAREASOLID(#65,#68,#21,400.);
#71=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#69));
#72=IFCPRODUCTDEFINITIONSHAPE($,$,(#71));
#73=IFCOPENINGELEMENT('0000000000000000000006',$,'O',$,$,#62,#72,$);
#74=IFCRELVOIDSELEMENT('0000000000000000000007',$,$,$,#41,#73);
#70=IFCDOOR('0000000000000000000008',$,'D',$,$,#62,$,$,2100.,900.);
#75=IFCRELFILLSELEMENT('0000000000000000000009',$,$,$,#73,#70);
#80=IFCSLAB('0000000000000000000010',$,'S',$,$,#15,$,$,.FLOOR.);
#81=IFCWINDOW('0000000000000000000011',$,'W',$,$,#15,$,$,1000.,1000.);
ENDSEC;
END-ISO-10303-21;
";

    #[test]
    fn ifc2x3_millimetres_and_cyrillic() {
        let mut m = Model::open(":memory:").unwrap();
        let rep = import_ifc(&mut m, SAMPLE_2X3.as_bytes()).unwrap();
        assert_eq!(rep.schema, "IFC2X3");
        assert_eq!(rep.length_unit, 1e-3);
        assert_eq!((rep.walls, rep.openings), (1, 1));
        let skipped: Vec<_> = rep.skipped.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(
            skipped,
            [("IFCSLAB: no body", 1), ("IFCWINDOW: no opening", 1)]
        );
        let s = snapshot(&m);
        assert_eq!(s.0, [(crate::ru_fixtures::ru("storey_2").to_string(), 3.0)]);
        let layers = vec![
            ("Brick".to_string(), 0.2),
            (crate::ru_fixtures::ru("insulation_cap").to_string(), 0.05),
        ];
        assert_eq!(s.1, [("Ext 250".to_string(), layers)]);
        assert_eq!(s.2[0].2, [1.0, 2.0, 1.0, 6.0, 0.0, 2.8]);
        assert_eq!(s.2[0].3, None);
        assert_eq!(s.3, [(1.0, 0.9, 0.0, 2.1, "door".to_string())]);
        let vol: HashMap<String, f64> = s.4.into_iter().collect();
        assert!((vol["Brick"] - 1.862).abs() < 1e-9, "{vol:?}");
        assert!((vol[crate::ru_fixtures::ru("insulation_cap")] - 0.4655).abs() < 1e-9, "{vol:?}");
    }

    #[test]
    fn feet_and_negative_sense() {
        let src = SAMPLE_2X3
            .replace(
                "#1=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);",
                "#1=IFCCONVERSIONBASEDUNIT(#4,.LENGTHUNIT.,'FOOT',#5);\
                 #4=IFCDIMENSIONALEXPONENTS(1,0,0,0,0,0,0);\
                 #5=IFCMEASUREWITHUNIT(IFCLENGTHMEASURE(0.3048),#6);\
                 #6=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);",
            )
            .replace(".POSITIVE.,-125.", ".NEGATIVE.,125.");
        let p = plan(&StepFile::parse(src.as_bytes()).unwrap()).unwrap();
        assert_eq!(p.report.length_unit, 0.3048);
        assert_eq!(p.types[0].name, "Ext 250 (reversed)");
        assert_eq!(
            p.types[0].layers[0],
            (crate::ru_fixtures::ru("insulation_cap").into(), snap(50.0 * 0.3048))
        );
        assert_eq!(p.walls[0].a, (304.8, 609.6));
        assert_eq!(p.walls[0].offset0, None);
    }

    #[test]
    fn malformed_input_is_typed_error() {
        let mut m = Model::open(":memory:").unwrap();
        let half = &SAMPLE_2X3[..SAMPLE_2X3.len() / 2];
        for (bad, want) in [
            ("".to_string(), "Step"),
            ("not a step file".to_string(), "Step"),
            (half.to_string(), "Step"),
            (SAMPLE_2X3.replace("IFC2X3", "CONFIG_X"), "Schema"),
            (SAMPLE_2X3.replace(".MILLI.,.METRE.", "$,.FOOT."), "Units"),
        ] {
            let e = import_ifc(&mut m, bad.as_bytes()).unwrap_err();
            assert!(format!("{e:?}").starts_with(want), "{e:?}");
        }
        // a dangling reference is a warning; the damaged layer drops the layer set
        let src = SAMPLE_2X3.replace("(#52,#53)", "(#52,#999)");
        let p = plan_bytes(src.as_bytes()).unwrap();
        assert!(
            p.report.warnings.iter().any(|w| w.contains("#999")),
            "{:?}",
            p.report.warnings
        );
        assert_eq!(p.walls.len(), 1);
        // no axis: derived from the body footprint, the opening still lands in the wall
        let src = SAMPLE_2X3.replace("(#33,#39)", "(#39)");
        let p = plan(&StepFile::parse(src.as_bytes()).unwrap()).unwrap();
        assert_eq!(
            (p.walls.len(), p.openings.len()),
            (1, 1),
            "{:?}",
            p.report.skipped
        );
        assert_eq!((p.walls[0].a, p.walls[0].b), ((1.0, 2.0), (1.0, 6.0)));
        assert!(snapshot(&m).2.is_empty());
    }

    /// 300 mutated inputs on 4 workers. An input over its budget, or a run over the total
    /// budget, fails the test (input saved to the temp dir) instead of hanging the run.
    #[test]
    fn mutated_bytes_never_panic() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{mpsc, Arc};
        use web_time::{Duration, Instant};
        const PER_INPUT: Duration = Duration::from_secs(10);
        const TOTAL: Duration = Duration::from_secs(150);
        const WORKERS: usize = 4;
        let base = export_ifc(&source_model()).unwrap().into_bytes();
        let alphabet = b"#$*'(),.;=0123456789-E\\X";
        let mut seed = 0x2545F4914F6CDD1Du64;
        let inputs: Arc<Vec<Vec<u8>>> = Arc::new(
            (0..300)
                .map(|_| {
                    let mut b = base.clone();
                    for _ in 0..4 {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        let at = (seed as usize) % b.len();
                        b[at] = alphabet[(seed >> 40) as usize % alphabet.len()];
                    }
                    b
                })
                .collect(),
        );
        let save = |i: usize| {
            let p = std::env::temp_dir().join(format!("ddc-ifc-mutation-{i}.ifc"));
            std::fs::write(&p, &inputs[i]).ok();
            p.display().to_string()
        };
        let next = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel::<(usize, Result<Duration, String>)>();
        for _ in 0..WORKERS {
            let (inputs, next, tx) = (inputs.clone(), next.clone(), tx.clone());
            std::thread::spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= inputs.len() {
                    return;
                }
                let t0 = Instant::now();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut m = Model::open(":memory:").unwrap();
                    let _ = import_ifc(&mut m, &inputs[i]);
                }));
                let r = r.map(|()| t0.elapsed()).map_err(|e| {
                    e.downcast_ref::<String>()
                        .cloned()
                        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_default()
                });
                if tx.send((i, r)).is_err() {
                    return;
                }
            });
        }
        drop(tx);
        let t0 = Instant::now();
        let mut done = vec![false; inputs.len()];
        let mut slowest = (Duration::ZERO, 0usize);
        for _ in 0..inputs.len() {
            match rx.recv_timeout(PER_INPUT) {
                Ok((i, Ok(dt))) => {
                    assert!(dt <= PER_INPUT, "mutation {i} took {dt:?}; input: {}", save(i));
                    done[i] = true;
                    slowest = slowest.max((dt, i));
                }
                Ok((i, Err(msg))) => {
                    panic!("mutation {i} panicked: {msg}; input: {}", save(i))
                }
                Err(_) => {
                    let stuck: Vec<String> = (0..done.len())
                        .filter(|&i| !done[i] && i < next.load(Ordering::Relaxed))
                        .map(|i| format!("{i} ({})", save(i)))
                        .collect();
                    panic!("no import finished within {PER_INPUT:?}; still running: {stuck:?}");
                }
            }
        }
        let total = t0.elapsed();
        eprintln!("300 mutations in {total:?}; slowest {} took {:?}", slowest.1, slowest.0);
        assert!(total <= TOTAL, "300 mutations took {total:?}, budget {TOTAL:?}");
    }

    #[test]
    fn registry_finds_ifc_importer() {
        struct Count(usize);
        impl crate::Sink for Count {
            fn element(&mut self, _: &str, _: &[(&str, f64)]) -> u64 {
                self.0 += 1;
                self.0 as u64
            }
        }
        let dir = std::env::temp_dir().join(format!("ddc-ifc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.IFC");
        std::fs::write(&p, SAMPLE_2X3).unwrap();
        let mut r = crate::Registry::default();
        r.register(Box::new(IfcImporter));
        let mut c = Count(0);
        r.for_path(&p).unwrap().import(&p, &mut c).unwrap();
        assert_eq!(c.0, 2);
        let mut m = Model::open(":memory:").unwrap();
        assert_eq!(import_ifc_file(&mut m, &p).unwrap().walls, 1);
        std::fs::remove_file(&p).ok();
    }

    const TRIM: &str = "#90=IFCCARTESIANPOINT((0.,0.));#91=IFCDIRECTION((1.,0.));        #92=IFCVECTOR(#91,1.);#93=IFCLINE(#90,#92);        #94=IFCTRIMMEDCURVE(#93,(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(4000.)),.F.,.PARAMETER.);        #33=IFCSHAPEREPRESENTATION($,'Axis','Curve2D',(#94));";

    #[test]
    fn trimmed_curve_sense_false_keeps_trim_order() {
        let src = SAMPLE_2X3.replace(
            "#33=IFCSHAPEREPRESENTATION($,'Axis','Curve2D',(#32));",
            TRIM,
        );
        let p = plan(&StepFile::parse(src.as_bytes()).unwrap()).unwrap();
        assert_eq!(p.walls.len(), 1, "{:?}", p.report);
        // Trim1 (0) -> Trim2 (4000) in the rotated frame: (1,2) -> (1,6)
        assert_eq!((p.walls[0].a, p.walls[0].b), ((1.0, 2.0), (1.0, 6.0)));
        assert_eq!(p.walls[0].offset0, None);
        let off = src.replace(".POSITIVE.,-125.", ".POSITIVE.,0.");
        let p = plan(&StepFile::parse(off.as_bytes()).unwrap()).unwrap();
        assert_eq!(p.walls[0].offset0, Some(0.0));
        assert!(!p.types[0].name.contains("reversed"));
    }

    #[test]
    fn polyline_axis_splits_into_segment_walls() {
        let src = SAMPLE_2X3.replace(
            "#32=IFCPOLYLINE((#30,#31));",
            "#95=IFCCARTESIANPOINT((4000.,3000.));#32=IFCPOLYLINE((#30,#31,#95));",
        );
        let p = plan(&StepFile::parse(src.as_bytes()).unwrap()).unwrap();
        assert_eq!(p.walls.len(), 2, "{:?}", p.report);
        assert_eq!(p.types.len(), 1);
        // frame: x=(0,1), y=(-1,0): local (4,0)->(1,6), local (4,3)->(-2,6)
        assert_eq!((p.walls[0].a, p.walls[0].b), ((1.0, 2.0), (1.0, 6.0)));
        assert_eq!((p.walls[1].a, p.walls[1].b), ((1.0, 6.0), (-2.0, 6.0)));
        assert_eq!(p.walls[0].height, p.walls[1].height);
        assert_eq!(p.walls[0].wall_type, p.walls[1].wall_type);
    }

    #[test]
    fn reversed_z_mirrors_layer_side_and_tilt_is_rejected() {
        // Z down: local Y flips relative to the axis' left side, layers swap
        let down = SAMPLE_2X3.replace(
            "#21=IFCDIRECTION((0.,0.,1.));",
            "#21=IFCDIRECTION((0.,0.,-1.));",
        );
        // keep the extrusion/opening direction sane: only the wall placement uses #96
        let down = down
            .replace(
                "#23=IFCAXIS2PLACEMENT3D(#20,#21,#22);",
                "#23=IFCAXIS2PLACEMENT3D(#20,#96,#22);#96=IFCDIRECTION((0.,0.,-1.));",
            )
            .replace(
                "#21=IFCDIRECTION((0.,0.,-1.));",
                "#21=IFCDIRECTION((0.,0.,1.));",
            );
        let p = plan(&StepFile::parse(down.as_bytes()).unwrap()).unwrap();
        assert_eq!(p.walls.len(), 1, "{:?}", p.report);
        assert_eq!(p.types[0].layers[0].0, crate::ru_fixtures::ru("insulation_cap"));
        assert!(p.types[0].name.ends_with("(reversed)"));
        assert_eq!(p.walls[0].a, (1.0, 2.0));
        let tilt = SAMPLE_2X3.replace(
            "#23=IFCAXIS2PLACEMENT3D(#20,#21,#22);",
            "#23=IFCAXIS2PLACEMENT3D(#20,#97,#22);#97=IFCDIRECTION((0.,0.5,1.));",
        );
        let p = plan(&StepFile::parse(tilt.as_bytes()).unwrap()).unwrap();
        // not a native wall, but never dropped: kept as a mesh with the reason
        assert_eq!(p.walls.len(), 0);
        assert_eq!(p.meshes.len(), 1, "{:?}", p.report);
        assert_eq!(
            p.report
                .as_mesh
                .get("IFCWALLSTANDARDCASE: non-vertical placement"),
            Some(&1)
        );
    }

    /// Semicircle R=3 (bulge 1) with a door, closed by a chord wall, a room inside.
    fn arc_model() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m.add_wall_type("c", &[layer("concrete", 0.2)]).unwrap();
        let spec = |a, b| WallSpec {
            a,
            b,
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        let w = m.add_arc_wall(&spec((3., 0.), (-3., 0.)), 1.0).unwrap();
        m.add_wall(&spec((-3., 0.), (3., 0.))).unwrap();
        m.add_opening(
            w,
            &OpeningSpec {
                along: 4.0,
                width: 1.0,
                sill: 0.0,
                height: 2.0,
                kind: "door".into(),
            },
        )
        .unwrap();
        m.add_room(&ddc_model::RoomSpec {
            level: lv,
            name: "R".into(),
            number: "1".into(),
            point: (0.0, 1.0),
            height: 2.5,
        })
        .unwrap();
        m
    }

    #[test]
    fn arc_wall_round_trips() {
        let src = arc_model();
        let text = export_ifc(&src).unwrap();
        assert!(text.contains("IFCTRIMMEDCURVE(") && text.contains("IFCCIRCLE("));
        assert!(text.contains("IFCARCINDEX(("), "body with arc segments");
        let mut dst = Model::open(":memory:").unwrap();
        let rep = import_ifc(&mut dst, text.as_bytes()).unwrap();
        assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
        assert_eq!((rep.walls, rep.openings), (2, 1));
        let b: Vec<f64> = rows(&dst, "SELECT bulge FROM wall ORDER BY id", |r| r.get(0));
        assert!((b[0] - 1.0).abs() < 1e-9 && b[1] == 0.0, "{b:?}");
        let o: Vec<(f64, f64)> = rows(&dst, "SELECT along, width FROM opening", |r| {
            Ok((r.get(0)?, r.get(1)?))
        });
        assert!(
            (o[0].0 - 4.0).abs() < 1e-6 && (o[0].1 - 1.0).abs() < 1e-6,
            "{o:?}"
        );
        let (vs, vd) = (
            src.material_volumes().unwrap(),
            dst.material_volumes().unwrap(),
        );
        assert!((vs[0].1 - vd[0].1).abs() < 1e-6, "{vs:?} {vd:?}");
        let pi = std::f64::consts::PI;
        let single = 1.8 * pi - 0.4;
        assert!(vs[0].1 > single);
        assert!((dst.wall_length(1).unwrap() - 3.0 * pi).abs() < 1e-6);
    }

    #[test]
    fn curtain_wall_and_sloped_slab_round_trip() {
        use ddc_model::{
            CurtainSpec, GridLines, MullionSection, PanelKind, PanelSpec, SlabSlope, SlabSpec,
        };
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let sec = MullionSection::Rect { w: 0.05, d: 0.15 };
        let cw = m
            .add_curtain_wall(&CurtainSpec {
                level: lv,
                name: "Storefront".into(),
                a: (0.0, 0.0),
                b: (6.0, 0.0),
                bulge: 0.0,
                base_offset: 0.0,
                height: 3.0,
                u: GridLines::Spacing(1.5),
                v: GridLines::Spacing(1.0),
                interior: sec,
                border: sec,
                lines: Default::default(),
                top: vec![],
            })
            .unwrap();
        let door = PanelSpec {
            kind: PanelKind::Door,
            name: "Door".into(),
            swing_side: true,
            swing_hinge: false,
        };
        m.set_curtain_panel(cw, 1, 0, &door).unwrap();
        let st = m
            .add_slab_type(
                "floor",
                &[LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let sl = m
            .add_slab(&SlabSpec {
                level: lv,
                slab_type: st,
                offset: 3.0,
                outer: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
                holes: vec![],
            })
            .unwrap();
        m.set_slab_slope(
            sl,
            Some(SlabSlope::arrow(ddc_geom::v2(0., 0.), ddc_geom::v2(6., 0.), 0.12).unwrap()),
        )
        .unwrap();
        let ifc = export_ifc(&m).unwrap();
        assert_eq!(ifc.matches("=IFCPLATE(").count(), 11);
        assert_eq!(
            ifc.matches("=IFCMEMBER(").count(),
            m.curtain_layout(cw).unwrap().layout.bars.len()
        );
        assert_eq!(ifc.matches("=IFCDOOR(").count(), 1);
        let mut d = Model::open(":memory:").unwrap();
        let rep = import_ifc(&mut d, ifc.as_bytes()).unwrap();
        assert_eq!((rep.curtain_walls, rep.slabs), (1, 1), "{rep:?}");
        assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
        let id = d.curtain_walls().unwrap()[0];
        let (q0, q1) = (
            m.curtain_quantities(cw).unwrap(),
            d.curtain_quantities(id).unwrap(),
        );
        assert_eq!(
            (q1.cells, q1.panels, q1.glazed, q1.doors),
            (q0.cells, q0.panels, q0.glazed, q0.doors)
        );
        assert!(
            (q1.glass_area - q0.glass_area).abs() < 1e-6,
            "{} {}",
            q1.glass_area,
            q0.glass_area
        );
        assert!((q1.mullion_length - q0.mullion_length).abs() < 1e-6);
        let p = d.curtain_layout(id).unwrap();
        let dp = p.panels.iter().find(|x| x.kind == PanelKind::Door).unwrap();
        assert_eq!((dp.swing_side, dp.swing_hinge), (true, false));
        let s1 = d
            .sql()
            .query_row("SELECT id FROM slab", [], |r| r.get::<_, i64>(0))
            .unwrap();
        let pl = d.slab_slope(s1).unwrap().unwrap();
        assert!((pl.gx - 0.02).abs() < 1e-9 && pl.gy.abs() < 1e-9, "{pl:?}");
        let v = d.slab_mesh(s1).unwrap().volume();
        assert!((v - 4.8).abs() < 1e-6, "{v}");
        let zmax = d
            .slab_mesh(s1)
            .unwrap()
            .positions
            .iter()
            .map(|q| q.z)
            .fold(f64::MIN, f64::max);
        assert!((zmax - 3.12).abs() < 1e-6, "{zmax}");
    }
}
