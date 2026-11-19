//! Pure, seed-deterministic generators and family templates. No model access.
//! Results are schematic starting points: valid geometry, not a checked design.

use serde_json::Value as J;

mod check;
#[doc(hidden)]
pub use check::miter_clear;
mod columns;
mod family;
mod floor;
pub mod hangar;
mod lang;
mod layout;
mod params;
pub mod plant;
mod program;
pub mod project;
pub mod sample;
mod stair;
pub mod station;
pub mod steel;
pub mod steel_bridge;
mod steel_free;

#[cfg(test)]
mod tests;

pub use check::validate_layout;
pub use family::family_metrics;

/// Said with every result, so the assistant cannot present a generated layout as compliant.
pub const SCHEMATIC_NOTE: &str = "Schematic starting point with valid geometry. It is not a checked design and says nothing about code compliance.";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamKind {
    Length,
    Area,
    Count,
    /// A share or a dimensionless factor.
    Ratio,
    /// A JSON list of `{name, area, window?}`.
    RoomList,
    /// A JSON list of `[x, y]` number pairs, `min..=max` of them.
    Points,
    /// A JSON object of rule id -> value (within the rule's range) overriding built-in rules.
    Rules,
    /// `default` is the index of the default choice.
    Choice(&'static [&'static str]),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamDef {
    pub name: &'static str,
    pub kind: ParamKind,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub help: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WallRole {
    Exterior,
    Party,
    Partition,
    Corridor,
    /// Outside wall of a storey below ground: it holds back the soil.
    Retaining,
}

impl WallRole {
    /// The thickness of the build-up the rules in force give this role
    /// ([`project::assembly`]); outside a build, under the built-in rules.
    pub fn thickness(self) -> f64 {
        project::assembly::role_thickness(self)
    }
}

/// What a fire-rated wall separates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FireSep {
    /// A boundary between fire compartments of one storey.
    Compartment,
    /// The enclosure of an escape stair.
    Stair,
}

/// A wall the fire rules rate: what it separates and its fire resistance in minutes. Doors in
/// it take a rating from it (the openings read this).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WallFire {
    pub sep: FireSep,
    pub minutes: u32,
}

#[derive(Clone, PartialEq)]
pub struct GWall {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub role: WallRole,
    /// Arc axis a -> b: bulge = tan(sweep/4), > 0 counter-clockwise; 0 is a straight wall.
    pub bulge: f64,
    /// Fire separation the wall forms; `None` for an unrated wall.
    pub fire: Option<WallFire>,
}

/// Straight walls print as before arcs existed, so pinned project hashes stay put.
impl std::fmt::Debug for GWall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("GWall");
        d.field("a", &self.a).field("b", &self.b).field("role", &self.role);
        if self.bulge != 0.0 {
            d.field("bulge", &self.bulge);
        }
        if let Some(x) = &self.fire {
            d.field("fire", x);
        }
        d.finish()
    }
}

impl GWall {
    pub fn line(a: (f64, f64), b: (f64, f64), role: WallRole) -> GWall {
        GWall { a, b, role, bulge: 0.0, fire: None }
    }

    fn arc(&self) -> Option<ddc_geom::arc::ArcAxis> {
        let (a, b) = (ddc_geom::v2(self.a.0, self.a.1), ddc_geom::v2(self.b.0, self.b.1));
        ddc_geom::arc::ArcAxis::new(a, b, self.bulge, ddc_geom::Tol::default()).ok()
    }

    /// Centre and radius of an arc wall.
    pub fn circle(&self) -> Option<((f64, f64), f64)> {
        self.arc().map(|ax| ((ax.center.x, ax.center.y), ax.radius))
    }

    /// Axis length (along the arc for an arc wall).
    pub fn length(&self) -> f64 {
        match self.arc() {
            Some(ax) => ax.length(),
            None => (self.b.0 - self.a.0).hypot(self.b.1 - self.a.1),
        }
    }

    /// The axis point `t` along the wall from `a`.
    pub fn at(&self, t: f64) -> (f64, f64) {
        match self.arc() {
            Some(ax) => {
                let p = ax.point_at(t);
                (p.x, p.y)
            }
            None => {
                let (d, l) = ((self.b.0 - self.a.0, self.b.1 - self.a.1), self.length().max(1e-12));
                (self.a.0 + d.0 * t / l, self.a.1 + d.1 * t / l)
            }
        }
    }

    /// Unit tangent of the axis `t` along the wall, in the direction a -> b.
    pub fn tangent(&self, t: f64) -> (f64, f64) {
        match self.arc() {
            Some(ax) => {
                let u = ax.tangent_at(t);
                (u.x, u.y)
            }
            None => {
                let (d, l) = ((self.b.0 - self.a.0, self.b.1 - self.a.1), self.length().max(1e-12));
                (d.0 / l, d.1 / l)
            }
        }
    }

    /// Distance along the axis of the foot of `p` (unclamped).
    pub fn along(&self, p: (f64, f64)) -> f64 {
        match self.arc() {
            Some(ax) => ax.param_of(ddc_geom::v2(p.0, p.1)),
            None => {
                let u = self.tangent(0.0);
                (p.0 - self.a.0) * u.0 + (p.1 - self.a.1) * u.1
            }
        }
    }

    /// Distance from `p` to the axis (its ends included).
    pub fn dist(&self, p: (f64, f64)) -> f64 {
        let Some(ax) = self.arc() else {
            return check::dist_to_seg(p, self.a, self.b);
        };
        let t = self.along(p);
        let to = |q: (f64, f64)| (p.0 - q.0).hypot(p.1 - q.1);
        if (0.0..=ax.length()).contains(&t) {
            let c = ax.center;
            ((p.0 - c.x).hypot(p.1 - c.y) - ax.radius).abs()
        } else {
            to(self.a).min(to(self.b))
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct GOpening {
    pub wall: usize,
    pub along: f64,
    pub door: bool,
    pub width: f64,
    pub height: f64,
    pub sill: f64,
    /// Type, mark, leaves, swing and ratings; filled by the project builder.
    pub ty: Option<GOpeningType>,
}

/// Prints the geometry only, so project hashes pinned before openings had types stay put.
impl std::fmt::Debug for GOpening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GOpening")
            .field("wall", &self.wall)
            .field("along", &self.along)
            .field("door", &self.door)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("sill", &self.sill)
            .finish()
    }
}

/// Side and hinge of a leaf, as the model's `Swing`: `side` false opens to the left of the
/// wall axis a -> b, true to the right; `hinge` false hangs at the start of the opening.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GSwing {
    pub side: bool,
    pub hinge: bool,
}

/// What an opening is: a type of the catalogue at its size, its mark, and the decisions the
/// rules made for it (`rules` names the rule behind each).
#[derive(Clone, Debug, PartialEq)]
pub struct GOpeningType {
    /// Catalogue id, e.g. `door.rated.1`.
    pub catalogue: String,
    /// Type name for schedules.
    pub name: String,
    /// `D01`, `W03`: one per distinct type, the same on every opening of it.
    pub mark: String,
    /// `room`, `wet`, `unit`, `stair`, `shaft`, `compartment`, `exit`, `gate`, `window`.
    pub function: &'static str,
    pub leaves: u8,
    /// `swing`, `sliding`, `casement`, `fixed`.
    pub operation: &'static str,
    pub swing: GSwing,
    /// Fire resistance of the leaf in minutes (EI); 0 is none.
    pub fire: u32,
    /// Fire resistance the host wall asks for, minutes.
    pub wall_fire: u32,
    /// Sound reduction index of the leaf or glazing, dB; 0 is not rated.
    pub rw: u32,
    /// Thermal transmittance, W/m2K; 0 for an interior opening.
    pub u: f64,
    /// Clear passage width, m.
    pub clear: f64,
    /// Height of the threshold above the floor, m.
    pub threshold: f64,
    /// On an escape route: the swing follows the escape direction.
    pub exit: bool,
    /// On the accessible route: clear width held to `opening.clear_access`.
    pub accessible: bool,
    /// The opening was widened about its centre to reach a required clear width.
    pub widened: bool,
    /// Rule ids that decided it.
    pub rules: Vec<&'static str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GRoom {
    pub name: String,
    pub apartment: Option<u32>,
    pub point: (f64, f64),
    pub polygon: Vec<(f64, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GColumn {
    pub at: (f64, f64),
    pub rotation: f64,
    /// Width and depth of a rectangular section.
    pub section: (f64, f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct GGrid {
    pub name: String,
    pub a: (f64, f64),
    pub b: (f64, f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct GStair {
    pub origin: (f64, f64),
    pub angle: f64,
    pub shape: ddc_geom::stair::Shape,
    pub width: f64,
    pub max_riser: f64,
    pub landing: f64,
    pub height: f64,
}

impl GStair {
    pub fn def(&self) -> ddc_geom::stair::StairDef {
        ddc_geom::stair::StairDef {
            origin: ddc_geom::v2(self.origin.0, self.origin.1),
            angle: self.angle,
            base_z: 0.0,
            height: self.height,
            width: self.width,
            max_riser: self.max_riser,
            tread: None,
            landing: self.landing,
            thickness: 0.15,
            stringer: None,
            shape: self.shape,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GShaft {
    pub ring: Vec<(f64, f64)>,
    pub kind: ShaftKind,
}

/// What a vertical shaft carries; lift logic reads only `Lift`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaftKind {
    Lift,
    Duct,
    Riser,
    Electrical,
}

impl GShaft {
    pub fn new(ring: Vec<(f64, f64)>, kind: ShaftKind) -> Self {
        Self { ring, kind }
    }
}

/// A built-in library component: furniture and fixtures of a room.
#[derive(Clone, Debug, PartialEq)]
pub struct GFixture {
    pub family: &'static str,
    pub ty: &'static str,
    /// Plan point of the family origin; for a wall-hosted family, on the wall face.
    pub at: (f64, f64),
    /// Radians counter-clockwise from plan +x to where the family's local +x points.
    pub rotation: f64,
    /// The hosting wall of a wall-hosted family.
    pub wall: Option<usize>,
    /// Height of the family origin above its level: ceiling terminals hang at the ceiling.
    pub z: f64,
    /// Size of the instance relative to its type (trees: seeded height jitter); 1 keeps the type.
    pub scale: f64,
    /// Least clear trunk under the crown of the instance, m (trees; 0 keeps the type's).
    pub min_clear: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GLayout {
    pub walls: Vec<GWall>,
    pub openings: Vec<GOpening>,
    pub rooms: Vec<GRoom>,
    pub slab: Vec<(f64, f64)>,
    pub slab_holes: Vec<Vec<(f64, f64)>>,
    pub columns: Vec<GColumn>,
    pub grids: Vec<GGrid>,
    pub stairs: Vec<GStair>,
    pub shafts: Vec<GShaft>,
    pub fixtures: Vec<GFixture>,
    /// Storey height; 0 means the default storey.
    pub height: f64,
    pub metrics: Vec<(&'static str, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GenOut {
    Layout(GLayout),
    Family { def: String },
    /// A steel frame graph; joints and fasteners are derived from it on demand.
    Steel {
        frame: Box<ddc_geom::steel::lattice::Frame>,
        metrics: Vec<(&'static str, f64)>,
        /// Every decision of a detailed (latticed) tower with its rule.
        why: Vec<project::why::Why>,
        /// What a truss hall built as a hangar adds to its frame.
        hall: Option<Box<hangar::HallOutfit>>,
        /// Join every free node of the frame (plates and bolts) when it lands in the model.
        joints: bool,
    },
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum GenError {
    #[error("invalid parameter {name}: {why}")]
    Param { name: String, why: String },
    #[error("no layout satisfies the parameters: {0}")]
    Infeasible(String),
}

pub trait Generator: Send + Sync {
    fn id(&self) -> &'static str;
    fn params(&self) -> &'static [ParamDef];
    /// Seed 1, 2, 3 give the variants named by `variants`, in order; a larger seed wraps around.
    fn build(&self, params: &J, seed: u64) -> Result<GenOut, GenError>;
    /// The stated criterion of each variant.
    fn variants(&self) -> &'static [&'static str] {
        &[]
    }
    fn summary(&self) -> &'static str {
        ""
    }
}

/// Index of the variant a seed selects.
pub(crate) fn variant_of(seed: u64) -> usize {
    (seed.saturating_sub(1) % 3) as usize
}

static REGISTRY: [&dyn Generator; 6] = [
    &floor::TypicalFloor,
    &program::AreaProgram,
    &columns::ColumnGrid,
    &stair::StairFlight,
    &family::FamilyTemplate,
    &steel::SteelFrameGen,
];

pub fn registry() -> &'static [&'static dyn Generator] {
    &REGISTRY
}
