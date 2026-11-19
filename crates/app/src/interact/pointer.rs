//! `Pointer`: a cursor in model coordinates — the ray under it and the
//! work plane it lands on. A view only supplies the projection (`adapter`); sessions see model
//! points. Everything is f64 so plan, 3D and section agree to 1e-9 m.
use ddc_geom::{v2, v3, V2, V3};

/// Plan zoom assumed for a point that came without a view (old `Event`, scripts).
pub const DEFAULT_PX_PER_M: f64 = 100.0;
/// Plan points are canonical to 1 nm: the same model point reached through any view is the
/// same number (the model itself works in mm), so plan and 3D give the same journal.
pub const RESOLUTION: f64 = 1e-9;
/// Steps of `RESOLUTION` per metre (exact in f64: division by it rounds correctly).
const PER_M: f64 = 1e9;
/// Height above the plane from which a plan ray starts.
const PLAN_EYE: f64 = 1.0;
const PARALLEL: f64 = 1e-12;

/// The view that produced the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewKind {
    /// Plan of a level (id).
    Plan { level: i64 },
    View3d,
    /// Section or elevation view (id).
    Section { view: i64 },
}

/// Where a work plane comes from (the scene plane's `edit3d::plane::Source`, plus views).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Level(i64),
    /// Face of an element under the cursor.
    Face,
    /// Reference plane of the model (id).
    Ref(i64),
    /// Named work plane of the project.
    Named(i64),
    /// Vertical plane through a straight grid axis.
    Grid(i64),
    /// Cut plane of a section or elevation view (id).
    View(i64),
}

/// Ray in model coordinates; `d` is a unit vector.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray3 {
    pub o: V3,
    pub d: V3,
}

impl Ray3 {
    pub fn at(&self, t: f64) -> V3 {
        self.o + self.d * t
    }
}

/// Work plane in model coordinates (f64): `u`, `v` span it, `n` is its unit normal.
/// `edit3d::plane::WorkPlane` is the f32 scene copy used for drawing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorkPlane {
    pub origin: V3,
    pub u: V3,
    pub v: V3,
    pub n: V3,
    pub source: Source,
}

impl WorkPlane {
    /// Horizontal plane of a level at model elevation `z`.
    pub fn level(id: i64, z: f64) -> WorkPlane {
        WorkPlane {
            origin: v3(0.0, 0.0, z),
            u: v3(1.0, 0.0, 0.0),
            v: v3(0.0, 1.0, 0.0),
            n: v3(0.0, 0.0, 1.0),
            source: Source::Level(id),
        }
    }

    /// Vertical plane through the plan line a→b: `u` along it, `v` up, `n` its left normal.
    pub fn vertical(source: Source, a: V2, b: V2) -> Option<WorkPlane> {
        let d = b - a;
        let l = d.len();
        if l.is_nan() || l <= PARALLEL {
            return None;
        }
        let u = v3(d.x / l, d.y / l, 0.0);
        Some(WorkPlane { origin: v3(a.x, a.y, 0.0), u, v: v3(0.0, 0.0, 1.0), n: v3(-u.y, u.x, 0.0), source })
    }

    pub fn is_horizontal(&self) -> bool {
        self.n.z.abs() > 0.999
    }

    /// Where the ray meets the plane (either side of the origin); `None` when parallel.
    pub fn hit(&self, r: &Ray3) -> Option<V3> {
        let dn = r.d.dot(self.n);
        if dn.abs() < PARALLEL {
            return None;
        }
        let t = (self.origin - r.o).dot(self.n) / dn;
        t.is_finite().then(|| r.at(t))
    }

    pub fn to_uv(&self, p: V3) -> V2 {
        let d = p - self.origin;
        v2(d.dot(self.u), d.dot(self.v))
    }

    pub fn from_uv(&self, q: V2) -> V3 {
        self.origin + self.u * q.x + self.v * q.y
    }
}

/// Which feature of the host the ray met.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feat {
    /// A long side of a wall: +1 the side of its left normal, -1 the other.
    Side(i8),
    /// Any other face (end, top, a slab face).
    Face,
}

/// Element under the pointer (filled by the 3D and section routes, by the wall axis in plan).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostHit {
    pub target: super::scope::Target,
    pub feat: Feat,
    /// Model point on the host.
    pub p: V3,
    /// Unit normal of the face met.
    pub n: V3,
    /// The same place on the host's reference line (wall axis, same along and height): what
    /// a plan click on the host gives.
    pub axis: V3,
}

/// A cursor position as every tool session sees it, whatever view it came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pointer {
    pub ray: Ray3,
    pub plane: WorkPlane,
    /// Screen pixels per model metre at the point (snap radii, glyph sizes).
    pub px_per_m: f64,
    pub view: ViewKind,
    /// Screen position the pointer came from, px.
    pub screen: [f32; 2],
    /// Element the ray meets first, when the route looked for one.
    pub host: Option<HostHit>,
}

impl Pointer {
    /// A plan point on the plane of `level` at elevation `z`: a vertical ray down onto it.
    pub fn at_plan(p: V2, level: i64, z: f64, px_per_m: f64) -> Pointer {
        Pointer {
            ray: Ray3 { o: v3(p.x, p.y, z + PLAN_EYE), d: v3(0.0, 0.0, -1.0) },
            plane: WorkPlane::level(level, z),
            px_per_m,
            view: ViewKind::Plan { level },
            screen: [0.0, 0.0],
            host: None,
        }
    }

    /// The model point under the cursor: on the host when there is one, else on the work
    /// plane; `None` when the ray runs along the plane.
    pub fn point(&self) -> Option<V3> {
        match self.host {
            Some(h) => Some(h.p),
            None => self.plane.hit(&self.ray),
        }
    }

    /// The plan point (x, y) for sessions that work in plan, to `RESOLUTION` (on a host: its
    /// reference line); `None` when the view gives no point.
    pub fn plan(&self) -> Option<V2> {
        let p = match self.host {
            Some(h) => h.axis,
            None => self.plane.hit(&self.ray)?,
        };
        Some(v2(canon(p.x), canon(p.y)))
    }
}

/// Old plan-only callers (`wall_tool::Event`, scripts): level 0 at elevation 0.
impl From<V2> for Pointer {
    fn from(p: V2) -> Pointer {
        Pointer::at_plan(p, 0, 0.0, DEFAULT_PX_PER_M)
    }
}

/// `x` on the 1 nm grid; -0 becomes 0 so equal points print equal.
fn canon(x: f64) -> f64 {
    if !x.is_finite() || x.abs() >= 1e6 {
        return x;
    }
    (x * PER_M).round() / PER_M + 0.0
}
