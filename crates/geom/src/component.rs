// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Parametric components ("families") v0: named parameters (type/instance, formulas
//! with units, dependency ordering, cycles are errors), reference planes, bodies built from primitives
//! (extrude, revolve, sweep along a path, loft, cut), visibility by detail level and view,
//! a 2D plan symbol. The definition is text (one instruction per line, `#` starts a comment):
//!
//! ```text
//! family Table
//! category furniture            # furniture generic casework plumbing lighting equipment air_terminal electrical
//! host none                     # none wall floor ceiling face
//! ifc IfcFurniture TABLE        # optional: class and PredefinedType
//! param Width length type 1600mm
//! param LegH length type = Height - Top      # formula
//! param Material material type "Oak"
//! ref Left x = -Width / 2       # reference plane x = …; coordinates that reference it are "locked"
//! type "1600x800" Width=1600mm, Depth=800mm
//! extrude Top rect(Left, Front, Right, Back) z(Height - Top, Height) mat(Material) show(medium, fine, 3d)
//! revolve Bowl poly(0,0, 250mm,0, 250mm,180mm) axis(0, 0) angle(360deg)
//! sweep Rail circle(0, 0, 15mm) path(0,0,0, 1m,0,0, 1m,1m,0)
//! blend Leg rect(-30mm,-30mm,30mm,30mm) rect(-20mm,-20mm,20mm,20mm) z(0, LegH)
//! symbol rect(Left, Front, Right, Back) show(coarse, medium, fine, plan)
//! connector Waste pipe sanitary at(0, 100mm, 0) dir(0, 0, -1) size(50mm)   # network connection
//! mesh Body data(eJx...) scale(Width / 900mm, 1, 1) mat("Oak") show(medium, fine, 3d)  # mesh
//! feature Body data(eJx...)     # feature timeline; its parameters follow the family's
//! plant Crown part(foliage) form(Form) height(H) crown(W) trunk(D) clear(C) leaf(1) seed(Seed) mat(Leaves)
//!                               # procedural tree: part(wood | foliage), form round oval conical columnar
//!                               # weeping palm vase spreading (or its index), see `plant`
//! ```
//!
//! Coordinates are relative to the insertion point: x right, y into the depth, z up. `void` on a body subtracts its volume.

mod build;
pub mod catalog;
pub mod ddcf;
#[cfg(test)]
mod ddcf_tests;
pub mod define;
#[cfg(test)]
mod define_tests;
pub mod doc;
pub mod expr;
pub mod ext;
#[cfg(test)]
mod ext_tests;
pub mod flex;
pub mod from_feature;
pub mod from_mesh;
pub mod gen;
#[cfg(test)]
mod gen_tests;
pub mod kernel;
pub mod meshbody;
pub mod plant;
pub mod rig;
#[cfg(test)]
mod tests;
pub mod thumb;
pub mod validate;
#[cfg(test)]
mod doc_diff_tests;
#[cfg(test)]
mod validate_tests;

use std::collections::{BTreeMap, HashMap};
use std::fmt;

pub use build::{plant_of, Built, Lod, Part};
use expr::{Dim, Expr, ANGLE, AREA, LENGTH, NUMBER, Q, VOLUME};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Furniture,
    Generic,
    Casework,
    Plumbing,
    Lighting,
    Equipment,
    AirTerminal,
    Electrical,
    Door,
    Window,
    Stairs,
    Structural,
    PipeFitting,
    DuctFitting,
    Planting,
    Entourage,
    Vehicle,
}

impl Category {
    pub const ALL: [Category; 17] = [
        Category::Furniture,
        Category::Generic,
        Category::Casework,
        Category::Plumbing,
        Category::Lighting,
        Category::Equipment,
        Category::AirTerminal,
        Category::Electrical,
        Category::Door,
        Category::Window,
        Category::Stairs,
        Category::Structural,
        Category::PipeFitting,
        Category::DuctFitting,
        Category::Planting,
        Category::Entourage,
        Category::Vehicle,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Category::Furniture => "furniture",
            Category::Generic => "generic",
            Category::Casework => "casework",
            Category::Plumbing => "plumbing",
            Category::Lighting => "lighting",
            Category::Equipment => "equipment",
            Category::AirTerminal => "air_terminal",
            Category::Electrical => "electrical",
            Category::Door => "door",
            Category::Window => "window",
            Category::Stairs => "stairs",
            Category::Structural => "structural",
            Category::PipeFitting => "pipe_fitting",
            Category::DuctFitting => "duct_fitting",
            Category::Planting => "planting",
            Category::Entourage => "entourage",
            Category::Vehicle => "vehicle",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == s)
    }
    /// Default IFC class and PredefinedType.
    pub fn ifc(self) -> (&'static str, &'static str) {
        match self {
            Category::Furniture | Category::Casework => ("IfcFurniture", "NOTDEFINED"),
            Category::Generic => ("IfcBuildingElementProxy", "ELEMENT"),
            Category::Plumbing => ("IfcSanitaryTerminal", "NOTDEFINED"),
            Category::Lighting => ("IfcLightFixture", "NOTDEFINED"),
            Category::Equipment => ("IfcBuildingElementProxy", "NOTDEFINED"),
            Category::AirTerminal => ("IfcAirTerminal", "NOTDEFINED"),
            Category::Electrical => ("IfcOutlet", "NOTDEFINED"),
            Category::Door => ("IfcDoor", "DOOR"),
            Category::Window => ("IfcWindow", "WINDOW"),
            Category::Stairs => ("IfcStair", "NOTDEFINED"),
            Category::Structural => ("IfcMember", "NOTDEFINED"),
            Category::PipeFitting => ("IfcPipeFitting", "NOTDEFINED"),
            Category::DuctFitting => ("IfcDuctFitting", "NOTDEFINED"),
            Category::Planting | Category::Entourage | Category::Vehicle => {
                ("IfcBuildingElementProxy", "NOTDEFINED")
            }
        }
    }
}

/// Instance host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hosting {
    None,
    /// Along the wall axis; z is measured from the wall bottom.
    Wall,
    /// On the top of a slab.
    Floor,
    /// Under the ceiling soffit; the body extends down from the insertion point.
    Ceiling,
    /// On a face: wall, slab or ceiling.
    Face,
}

impl Hosting {
    pub fn name(self) -> &'static str {
        match self {
            Hosting::None => "none",
            Hosting::Wall => "wall",
            Hosting::Floor => "floor",
            Hosting::Ceiling => "ceiling",
            Hosting::Face => "face",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [
            Hosting::None,
            Hosting::Wall,
            Hosting::Floor,
            Hosting::Ceiling,
            Hosting::Face,
        ]
        .into_iter()
        .find(|h| h.name() == s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Length,
    Angle,
    Area,
    Volume,
    Number,
    Int,
    Bool,
    Material,
    Text,
    /// Type of the nested family: the value "Family:Type".
    FamilyType,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "length" => Kind::Length,
            "angle" => Kind::Angle,
            "area" => Kind::Area,
            "volume" => Kind::Volume,
            "number" => Kind::Number,
            "int" | "integer" => Kind::Int,
            "bool" | "yesno" => Kind::Bool,
            "material" => Kind::Material,
            "text" => Kind::Text,
            "familytype" | "family_type" => Kind::FamilyType,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Kind::Length => "length",
            Kind::Angle => "angle",
            Kind::Area => "area",
            Kind::Volume => "volume",
            Kind::Number => "number",
            Kind::Int => "int",
            Kind::Bool => "bool",
            Kind::Material => "material",
            Kind::Text => "text",
            Kind::FamilyType => "familytype",
        }
    }
    pub fn is_text(self) -> bool {
        matches!(self, Kind::Material | Kind::Text | Kind::FamilyType)
    }
    pub fn dim(self) -> Dim {
        match self {
            Kind::Length => LENGTH,
            Kind::Angle => ANGLE,
            Kind::Area => AREA,
            Kind::Volume => VOLUME,
            _ => NUMBER,
        }
    }
}

/// Parameter value: a number in SI or text.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Num(f64),
    Text(String),
}

/// Value source: an expression of constants, or a string.
#[derive(Clone, Debug, PartialEq)]
pub enum Src {
    Expr(Expr),
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub kind: Kind,
    /// true for an instance parameter, false for a type parameter.
    pub instance: bool,
    pub formula: Option<Expr>,
    pub default: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RefPlane {
    pub name: String,
    pub axis: Axis,
    pub at: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FamilyType {
    pub name: String,
    pub values: Vec<(String, Value)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Profile {
    /// x0, y0, x1, y1.
    Rect([Expr; 4]),
    /// cx, cy, r.
    Circle([Expr; 3]),
    Poly(Vec<[Expr; 2]>),
}

/// Solid statements; `Family::parse` routes exactly these to `solid`.
pub(crate) const SOLID_HEADS: &[&str] =
    &["extrude", "revolve", "sweep", "blend", "mesh", "feature", "plant"];

/// Core statement heads `Family::parse` accepts (besides `ext::STMTS`); any other head is
/// rejected before the match, so this list and the parser cannot diverge.
pub(crate) const HEADS: &[&str] = &[
    "family", "category", "host", "ifc", "param", "ref", "type", "symbol", "limit", "ddcf",
    "version", "uid", "status", "connector", "extrude", "revolve", "sweep", "blend", "mesh",
    "feature", "plant",
];

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// Profile in the plane perpendicular to `dir` (z: (x, y); x: (y, z); y: (x, z)), from a to b.
    Extrude {
        profile: Profile,
        dir: Axis,
        range: [Expr; 2],
    },
    /// Profile (r, z) revolved about a vertical axis at (x, y) through an angle.
    Revolve {
        profile: Profile,
        axis: [Expr; 2],
        angle: Expr,
    },
    /// Profile (u, v) in the plane normal to the path; u is left, v is up; joints are mitred on the bisector.
    Sweep {
        profile: Profile,
        path: Vec<[Expr; 3]>,
    },
    /// Linear loft between two profiles with the same vertex count; axes are as in `Extrude`.
    Blend {
        bottom: Profile,
        top: Profile,
        range: [Expr; 2],
        dir: Axis,
    },
    /// Free-form mesh from the insertion point; `scale(sx, sy, sz)` (numbers, usually
    /// `Width / 900mm`) stretches it about the insertion point.
    Mesh {
        body: std::sync::Arc<meshbody::MeshBody>,
        scale: Option<[Expr; 3]>,
    },
    /// Parametric feature timeline (`feature <name> data(...)`), coordinates from the insertion
    /// point. A timeline parameter named like a family parameter takes that parameter's value,
    /// so types and instances with other values build other bodies.
    Feature {
        tl: std::sync::Arc<crate::feature::Timeline>,
    },
    /// Procedural plant organ (`plant <name> part(wood|foliage) form(..) height(..) crown(..)
    /// trunk(..) clear(..) leaf(..) seed(..)`): see `plant`. `form` is a name or a number
    /// (index in `plant::Form::NAMES`), so a parameter can choose the form.
    Plant {
        organ: plant::Organ,
        form: Expr,
        /// Height, crown width, trunk diameter, clear height.
        dims: [Expr; 4],
        leaf: Expr,
        seed: Expr,
    },
}

/// Visibility: detail-level bits (1 coarse, 2 medium, 4 fine) and view bits (8 plan, 16 3d, 32 elev).
pub type Vis = u8;
pub const COARSE: Vis = 1;
pub const MEDIUM: Vis = 2;
pub const FINE: Vis = 4;
pub const PLAN: Vis = 8;
pub const MODEL: Vis = 16;
pub const ELEV: Vis = 32;
const ALL_LOD: Vis = COARSE | MEDIUM | FINE;

#[derive(Clone, Debug, PartialEq)]
pub enum MatRef {
    Param(String),
    Literal(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Solid {
    pub name: String,
    pub shape: Shape,
    pub void: bool,
    /// `alt`: another level-of-detail picture of other solids; drawn, not counted in volume.
    pub alt: bool,
    pub material: Option<MatRef>,
    pub vis: Vis,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SymShape {
    Line([Expr; 4]),
    /// cx, cy, r, a0, a1 (counter-clockwise).
    Arc([Expr; 5]),
    Circle([Expr; 3]),
    Rect([Expr; 4]),
    /// Polyline; `true` means closed.
    Poly(Vec<[Expr; 2]>, bool),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Symbol {
    pub shape: SymShape,
    pub vis: Vis,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Family {
    pub name: String,
    pub category: Category,
    pub hosting: Hosting,
    /// IFC class and PredefinedType (by default derived from the category).
    pub ifc: (String, String),
    pub params: Vec<Param>,
    pub refs: Vec<RefPlane>,
    pub types: Vec<FamilyType>,
    pub solids: Vec<Solid>,
    pub symbols: Vec<Symbol>,
    pub connectors: Vec<Connector>,
    /// Family editor: plane sketch, conditions, visibility, nesting (schema 29).
    pub ext: ext::Ext,
    /// Allowed ranges of numeric parameters (`limit <param> <min|_> <max|_>`).
    pub limits: Vec<Limit>,
    /// `status draft`: defined by a program, not yet promoted by a person.
    pub draft: bool,
    /// Format header: `ddcf`, `version`, `uid` (see `ddcf`).
    pub meta: ddcf::Meta,
    /// Evaluation order: indices into `params` (k) and `refs` (params.len() + k).
    order: Vec<usize>,
}

/// Allowed range of a numeric parameter, SI values; `None` is open.
#[derive(Clone, Debug, PartialEq)]
pub struct Limit {
    pub param: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// Start of the `FamilyError::Param` message for a value outside its `limit`.
pub const OUT_OF_RANGE: &str = "out of range";

#[derive(Clone, Debug, PartialEq)]
pub enum FamilyError {
    Parse { line: usize, msg: String },
    Cycle(Vec<String>),
    Param { name: String, msg: String },
    Geometry { solid: String, msg: String },
}

impl fmt::Display for FamilyError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FamilyError::Parse { line, msg } => write!(f, "line {line}: {msg}"),
            FamilyError::Cycle(p) => write!(f, "formula cycle: {}", p.join(" -> ")),
            FamilyError::Param { name, msg } => write!(f, "parameter {name}: {msg}"),
            FamilyError::Geometry { solid, msg } => write!(f, "{solid}: {msg}"),
        }
    }
}

impl std::error::Error for FamilyError {}

/// Evaluated values: numbers (with dimensions) and texts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resolved {
    pub nums: HashMap<String, Q>,
    pub texts: HashMap<String, String>,
    /// Bodies and nested items hidden by visibility parameters.
    pub hidden: Vec<String>,
}

impl Resolved {
    pub fn num(&self, n: &str) -> Option<f64> {
        self.nums.get(n).map(|q| q.v)
    }

    /// The material name a solid's reference stands for here.
    pub fn material_name(&self, m: &MatRef) -> String {
        match m {
            MatRef::Param(p) => self.texts.get(p).cloned().unwrap_or_default(),
            MatRef::Literal(t) => t.clone(),
        }
    }
}

/// Network connection: kind (pipe, duct, cable_tray, conduit), system (optional), point,
/// outward direction, nominal size (DN as a length, or the duct width).
#[derive(Clone, Debug, PartialEq)]
pub struct Connector {
    pub name: String,
    pub domain: crate::mep::Domain,
    pub system: Option<crate::mep::SystemKind>,
    pub at: [Expr; 3],
    pub dir: [Expr; 3],
    pub size: Expr,
}

/// Evaluated connection in component coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Port {
    pub name: String,
    pub domain: crate::mep::Domain,
    pub system: Option<crate::mep::SystemKind>,
    pub at: crate::math::V3,
    /// Unit outward vector.
    pub dir: crate::math::V3,
    pub size: f64,
}

fn connector(rest: &str) -> Result<Connector, String> {
    let it = items(rest)?;
    let usage =
        "connector <name> <pipe|duct|cable_tray|conduit> [system] at(x,y,z) dir(dx,dy,dz) size(d)";
    let Some((name, None)) = it.first().cloned() else {
        return Err(usage.into());
    };
    let (mut domain, mut system, mut at, mut dir, mut size) = (None, None, None, None, None);
    for (w, a) in &it[1..] {
        match (w.as_str(), a) {
            (w, None) => {
                if let Some(d) = crate::mep::Domain::parse(w) {
                    domain = Some(d);
                } else if let Some(s) = crate::mep::SystemKind::parse(w) {
                    system = Some(s);
                } else {
                    return Err(format!("unexpected '{w}'"));
                }
            }
            ("at", Some(a)) => at = Some(exprs::<3>("at", a)?),
            ("dir", Some(a)) => dir = Some(exprs::<3>("dir", a)?),
            ("size", Some(a)) => size = Some(ex(a)?),
            (o, Some(_)) => return Err(format!("unknown clause {o}()")),
        }
    }
    Ok(Connector {
        name,
        domain: domain.ok_or(usage)?,
        system,
        at: at.ok_or(usage)?,
        dir: dir.ok_or(usage)?,
        size: size.ok_or(usage)?,
    })
}

impl Family {
    /// Connections for the evaluated values.
    pub fn ports(&self, r: &Resolved) -> Result<Vec<Port>, FamilyError> {
        let env = |s: &str| r.nums.get(s).copied();
        self.connectors
            .iter()
            .map(|c| {
                let e = |x: &Expr| {
                    x.eval(&env).map(|q| q.v).map_err(|msg| FamilyError::Param {
                        name: c.name.clone(),
                        msg,
                    })
                };
                let o = self.ext.origin_at(r)?;
                let [ax, ay, az] = &c.at;
                let [dx, dy, dz] = &c.dir;
                let d = crate::math::v3(e(dx)?, e(dy)?, e(dz)?);
                if d.len().is_nan() || d.len() <= 0.0 {
                    return Err(FamilyError::Param {
                        name: c.name.clone(),
                        msg: "zero direction".into(),
                    });
                }
                Ok(Port {
                    name: c.name.clone(),
                    domain: c.domain,
                    system: c.system,
                    at: crate::math::v3(e(ax)? - o.x, e(ay)? - o.y, e(az)? - o.z),
                    dir: d.normalized(),
                    size: e(&c.size)?,
                })
            })
            .collect()
    }
}

/// Splits `a, b(c, d), "e,f"` on top-level commas.
fn split_top(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth, mut quote) = (vec![], String::new(), 0i32, false);
    let mut prev = ',';
    for c in s.chars() {
        // a quote opens a string only at the start of a value: 6" means inches
        let opens = matches!(prev, ',' | '=' | '(');
        if !c.is_whitespace() {
            prev = c;
        }
        match c {
            '"' if quote || opens => quote = !quote,
            '(' if !quote => depth += 1,
            ')' if !quote => depth -= 1,
            ',' if !quote && depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() || !out.is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// Words and `name(...)` calls of a line.
fn items(s: &str) -> Result<Vec<(String, Option<String>)>, String> {
    let c: Vec<char> = s.chars().collect();
    let (mut i, mut out) = (0, vec![]);
    while i < c.len() {
        if c[i].is_whitespace() {
            i += 1;
            continue;
        }
        if c[i] == '"' {
            let st = i;
            i += 1;
            while i < c.len() && c[i] != '"' {
                i += 1;
            }
            if i >= c.len() {
                return Err("unterminated string".into());
            }
            i += 1;
            out.push((c[st..i].iter().collect(), None));
            continue;
        }
        let st = i;
        while i < c.len() && !c[i].is_whitespace() && c[i] != '(' {
            i += 1;
        }
        let word: String = c[st..i].iter().collect();
        if i < c.len() && c[i] == '(' {
            let (a0, mut depth, mut quote, mut prev) = (i + 1, 0, false, '(');
            while i < c.len() {
                let opens = matches!(prev, ',' | '=' | '(');
                if !c[i].is_whitespace() {
                    prev = c[i];
                }
                match c[i] {
                    '"' if quote || opens => quote = !quote,
                    '(' if !quote => depth += 1,
                    ')' if !quote => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            if i >= c.len() {
                return Err(format!("unbalanced '(' after {word}"));
            }
            out.push((word, Some(c[a0..i].iter().collect())));
            i += 1;
        } else {
            out.push((word, None));
        }
    }
    Ok(out)
}

fn unquote(s: &str) -> Option<String> {
    let s = s.trim();
    (s.len() >= 2 && s.starts_with('"') && s.ends_with('"')).then(|| s[1..s.len() - 1].to_string())
}

fn ex(s: &str) -> Result<Expr, String> {
    expr::parse(s).map_err(|e| format!("'{s}' {e}"))
}

fn exprs<const N: usize>(what: &str, a: &str) -> Result<[Expr; N], String> {
    let v = split_top(a);
    if v.len() != N {
        return Err(format!("{what}() takes {N} values, got {}", v.len()));
    }
    let v: Vec<Expr> = v.iter().map(|s| ex(s)).collect::<Result<_, _>>()?;
    v.try_into().map_err(|_| "arity".to_string())
}

fn tuples<const N: usize>(what: &str, a: &str) -> Result<Vec<[Expr; N]>, String> {
    let v = split_top(a);
    if v.is_empty() || v.len() % N != 0 {
        return Err(format!("{what}() needs groups of {N} values"));
    }
    let e: Vec<Expr> = v.iter().map(|s| ex(s)).collect::<Result<_, _>>()?;
    Ok(e.chunks(N)
        .map(|c| c.to_vec().try_into().expect("chunk"))
        .collect())
}

fn profile(w: &str, a: &str) -> Result<Option<Profile>, String> {
    Ok(Some(match w {
        "rect" => Profile::Rect(exprs("rect", a)?),
        "circle" => Profile::Circle(exprs("circle", a)?),
        "poly" => {
            let p = tuples::<2>("poly", a)?;
            if p.len() < 3 {
                return Err("poly() needs at least 3 points".into());
            }
            Profile::Poly(p)
        }
        _ => return Ok(None),
    }))
}

fn vis(a: &str) -> Result<Vis, String> {
    let mut v = 0;
    for w in split_top(a) {
        v |= match w.as_str() {
            "coarse" => COARSE,
            "medium" => MEDIUM,
            "fine" => FINE,
            "plan" => PLAN,
            "3d" => MODEL,
            "elev" | "section" => ELEV,
            "all" => ALL_LOD | PLAN | MODEL | ELEV,
            _ => return Err(format!("unknown visibility '{w}'")),
        };
    }
    if v & ALL_LOD == 0 {
        v |= ALL_LOD;
    }
    if v & (PLAN | MODEL | ELEV) == 0 {
        v |= PLAN | MODEL | ELEV;
    }
    Ok(v)
}

/// First field and the rest: runs of blanks between fields are one separator.
fn split1(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.split_once(char::is_whitespace) {
        Some((a, b)) => (a, b.trim_start()),
        None => (s, ""),
    }
}

fn axis(s: &str) -> Result<Axis, String> {
    match s.trim() {
        "x" => Ok(Axis::X),
        "y" => Ok(Axis::Y),
        "z" => Ok(Axis::Z),
        o => Err(format!("axis must be x, y or z, got '{o}'")),
    }
}

/// Constant value (no parameter references), used in types and defaults.
fn constant(kind: Kind, src: &str) -> Result<Value, String> {
    if kind.is_text() {
        return unquote(src)
            .map(Value::Text)
            .ok_or_else(|| format!("expected a quoted string, got {src}"));
    }
    let e = ex(src)?;
    let v = e.eval(&|_| None)?;
    check_kind(kind, v).map(Value::Num)
}

fn check_kind(kind: Kind, v: Q) -> Result<f64, String> {
    if !expr::zero_fits(v, kind.dim()) {
        return Err(format!("expected {}, got {}", kind.name(), v.d));
    }
    Ok(match kind {
        Kind::Int => v.v.round(),
        Kind::Bool => (v.v != 0.0) as i32 as f64,
        _ => v.v,
    })
}

impl Family {
    pub fn parse(text: &str) -> Result<Family, FamilyError> {
        let (body, nested) = ext::split_nested(text)?;
        let text = body.as_str();
        let mut f = Family {
            name: String::new(),
            category: Category::Generic,
            hosting: Hosting::None,
            ifc: (String::new(), String::new()),
            params: vec![],
            refs: vec![],
            types: vec![],
            solids: vec![],
            symbols: vec![],
            connectors: vec![],
            ext: ext::Ext::default(),
            limits: vec![],
            draft: false,
            meta: ddcf::Meta::default(),
            order: vec![],
        };
        for (ln, src) in nested {
            let c = Family::parse(&src).map_err(|e| FamilyError::Parse {
                line: ln,
                msg: format!("nested family: {e}"),
            })?;
            if f.ext.child(&c.name).is_some() {
                return Err(FamilyError::Parse {
                    line: ln,
                    msg: format!("duplicate nested family {}", c.name),
                });
            }
            f.ext.children.push(std::sync::Arc::new(c));
        }
        let mut raw_types: Vec<(usize, String, String)> = vec![];
        let mut raw_limits: Vec<(usize, String, String, String)> = vec![];
        let mut ifc = None;
        // lines with an unclosed parenthesis continue onto the following ones
        let mut stmts: Vec<(usize, String)> = vec![];
        let mut open = 0i32;
        for (n, line) in text.lines().enumerate() {
            let line = match line.find('#') {
                Some(k) if !line[..k].contains('"') || line[..k].matches('"').count() % 2 == 0 => {
                    &line[..k]
                }
                _ => line,
            };
            if open > 0 {
                let last = stmts.last_mut().expect("open statement");
                last.1.push(' ');
                last.1.push_str(line.trim());
            } else if !line.trim().is_empty() {
                stmts.push((n + 1, line.trim().to_string()));
            }
            open += line.matches('(').count() as i32 - line.matches(')').count() as i32;
        }
        for (si, (ln, st)) in stmts.into_iter().enumerate() {
            let pe = |msg: String| FamilyError::Parse { line: ln, msg };
            let (head, rest) = st.split_once(char::is_whitespace).unwrap_or((&st, ""));
            let rest = rest.trim();
            if !HEADS.contains(&head) && !ext::STMTS.contains(&head) {
                return Err(pe(format!("unknown statement '{head}'")));
            }
            match head {
                "family" => f.name = unquote(rest).unwrap_or_else(|| rest.to_string()),
                "category" => {
                    f.category = Category::parse(rest)
                        .ok_or_else(|| pe(format!("unknown category '{rest}'")))?
                }
                "host" => {
                    f.hosting =
                        Hosting::parse(rest).ok_or_else(|| pe(format!("unknown host '{rest}'")))?
                }
                "ifc" => {
                    let w: Vec<&str> = rest.split_whitespace().collect();
                    if w.is_empty() || w.len() > 2 {
                        return Err(pe("ifc <Class> [PREDEFINED]".into()));
                    }
                    ifc = Some((
                        w[0].to_string(),
                        w.get(1).unwrap_or(&"NOTDEFINED").to_string(),
                    ));
                }
                "param" => {
                    let (name, r) = rest.split_once(char::is_whitespace).ok_or_else(|| {
                        pe("param <name> <kind> <type|instance> [value | = formula]".into())
                    })?;
                    let (kind, r2) = split1(r);
                    let kind =
                        Kind::parse(kind).ok_or_else(|| pe(format!("unknown kind '{kind}'")))?;
                    let (scope, r3) = split1(r2);
                    let instance = match scope {
                        "type" => false,
                        "instance" => true,
                        o => return Err(pe(format!("scope must be type or instance, got '{o}'"))),
                    };
                    let tail = r3.trim();
                    let (formula, default) = if let Some(fx) = tail.strip_prefix('=') {
                        if kind.is_text() {
                            let t = ext::TextF::parse(fx.trim()).map_err(pe)?;
                            f.ext.text_formulas.push((name.to_string(), t));
                            (None, Value::Text(String::new()))
                        } else {
                            (Some(ex(fx.trim()).map_err(pe)?), Value::Num(0.0))
                        }
                    } else if tail.is_empty() {
                        let d = if kind.is_text() {
                            Value::Text(String::new())
                        } else {
                            Value::Num(0.0)
                        };
                        (None, d)
                    } else {
                        (None, constant(kind, tail).map_err(pe)?)
                    };
                    if f.params.iter().any(|p| p.name == name) {
                        return Err(pe(format!("duplicate parameter {name}")));
                    }
                    f.params.push(Param {
                        name: name.to_string(),
                        kind,
                        instance,
                        formula,
                        default,
                    });
                }
                "ref" => {
                    let (n0, r1) = split1(rest);
                    let (n1, r2) = split1(r1);
                    let w = [n0, n1, r2];
                    if w.iter().any(|s| s.is_empty()) {
                        return Err(pe("ref <name> x|y|z [=] <expr>".into()));
                    }
                    let at = w[2].trim();
                    if let Some(g) = at.strip_prefix('~') {
                        f.ext
                            .free_ref(w[0], axis(w[1]).map_err(pe)?, g)
                            .map_err(pe)?;
                        continue;
                    }
                    let at = at.strip_prefix('=').unwrap_or(at);
                    f.refs.push(RefPlane {
                        name: w[0].to_string(),
                        axis: axis(w[1]).map_err(pe)?,
                        at: ex(at.trim()).map_err(pe)?,
                    });
                }
                "type" => {
                    let (name, body) = if let Some(r) = rest.strip_prefix('"') {
                        r.split_once('"')
                            .ok_or_else(|| pe("unterminated type name".into()))?
                    } else {
                        rest.split_once(char::is_whitespace).unwrap_or((rest, ""))
                    };
                    if name.is_empty() {
                        return Err(pe("type \"<name>\" Key=value, ...".into()));
                    }
                    raw_types.push((ln, name.to_string(), body.trim().to_string()));
                }
                h if SOLID_HEADS.contains(&h) => {
                    f.solids.push(solid(head, rest).map_err(pe)?);
                }
                "symbol" => f.symbols.push(symbol(rest).map_err(pe)?),
                "limit" => {
                    let w: Vec<&str> = rest.split_whitespace().collect();
                    if w.len() != 3 {
                        return Err(pe("limit <param> <min | _> <max | _>".into()));
                    }
                    raw_limits.push((ln, w[0].into(), w[1].into(), w[2].into()));
                }
                "ddcf" | "version" | "uid" => {
                    ddcf::header(&mut f.meta, si, head, rest).map_err(pe)?
                }
                "status" => {
                    f.draft = match rest {
                        "draft" => true,
                        "released" => false,
                        o => return Err(pe(format!("status must be draft or released, not '{o}'"))),
                    }
                }
                "connector" => {
                    let c = connector(rest).map_err(pe)?;
                    if f.connectors.iter().any(|x| x.name == c.name) {
                        return Err(pe(format!("duplicate connector {}", c.name)));
                    }
                    f.connectors.push(c);
                }
                h if ext::STMTS.contains(&h) => f.ext.stmt(h, rest).map_err(pe)?,
                o => return Err(pe(format!("unknown statement '{o}'"))),
            }
        }
        if f.name.is_empty() {
            return Err(FamilyError::Parse {
                line: 1,
                msg: "missing 'family <name>'".into(),
            });
        }
        let (c, p) = f.category.ifc();
        f.ifc = ifc.unwrap_or((c.into(), p.into()));
        for (ln, name, body) in raw_types {
            let values = f
                .assignments(&body)
                .map_err(|msg| FamilyError::Parse { line: ln, msg })?;
            if f.types.iter().any(|t| t.name == name) {
                return Err(FamilyError::Parse {
                    line: ln,
                    msg: format!("duplicate type {name}"),
                });
            }
            f.types.push(FamilyType { name, values });
        }
        for (ln, name, lo, hi) in raw_limits {
            let l = f
                .limit(&name, &lo, &hi)
                .map_err(|msg| FamilyError::Parse { line: ln, msg })?;
            f.limits.push(l);
        }
        f.order = f.dependency_order()?;
        f.check_names()?;
        f.ext.validate(&f)?;
        Ok(f)
    }

    /// `Key=value, Key=value` → values by parameter kind; formula parameters cannot be set.
    pub fn assignments(&self, body: &str) -> Result<Vec<(String, Value)>, String> {
        let mut out = vec![];
        for a in split_top(body) {
            if a.is_empty() {
                continue;
            }
            let (k, v) = a
                .split_once('=')
                .ok_or_else(|| format!("expected Key=value, got '{a}'"))?;
            let k = k.trim();
            let p = self
                .param(k)
                .ok_or_else(|| format!("unknown parameter {k}"))?;
            if p.formula.is_some() || self.ext.has_text_formula(k) {
                return Err(format!("{k} is a formula and cannot be set"));
            }
            out.push((
                k.to_string(),
                constant(p.kind, v.trim()).map_err(|e| format!("{k}: {e}"))?,
            ));
        }
        Ok(out)
    }

    /// Value text that `assignments` reads back losslessly.
    pub fn assignments_text(&self, vals: &[(String, Value)]) -> String {
        vals.iter()
            .map(|(k, v)| {
                let s = match v {
                    Value::Text(t) => format!("\"{}\"", t.replace('"', "'")),
                    Value::Num(x) => {
                        let d = self.param(k).map_or(NUMBER, |p| p.kind.dim());
                        expr::literal(expr::q(*x, d))
                    }
                };
                format!("{k}={s}")
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn limit(&self, name: &str, lo: &str, hi: &str) -> Result<Limit, String> {
        let p = self
            .param(name)
            .ok_or_else(|| format!("limit: unknown parameter {name}"))?;
        if p.kind.is_text() {
            return Err(format!("limit: {name} is not numeric"));
        }
        if self.limits.iter().any(|l| l.param == name) {
            return Err(format!("duplicate limit of {name}"));
        }
        let bound = |s: &str| -> Result<Option<f64>, String> {
            if s == "_" {
                return Ok(None);
            }
            match constant(p.kind, s)? {
                Value::Num(v) => Ok(Some(v)),
                Value::Text(_) => Err(format!("limit: {name} bound must be a number")),
            }
        };
        let (min, max) = (bound(lo)?, bound(hi)?);
        if let (Some(a), Some(b)) = (min, max) {
            if a > b {
                return Err(format!("limit: {name} min is greater than max"));
            }
        }
        Ok(Limit {
            param: name.into(),
            min,
            max,
        })
    }

    /// Value outside the parameter's `limit`: the message names the range.
    pub fn check_limit(&self, name: &str, v: f64) -> Result<(), String> {
        let Some(l) = self.limits.iter().find(|l| l.param == name) else {
            return Ok(());
        };
        if l.min.is_some_and(|a| v < a) || l.max.is_some_and(|b| v > b) {
            let b = |x: Option<f64>| x.map_or("_".to_string(), |x| format!("{x}"));
            return Err(format!(
                "{OUT_OF_RANGE}: {v} not in [{}, {}]",
                b(l.min),
                b(l.max)
            ));
        }
        Ok(())
    }

    pub fn param(&self, name: &str) -> Option<&Param> {
        self.params.iter().find(|p| p.name == name)
    }

    pub fn family_type(&self, name: &str) -> Option<&FamilyType> {
        self.types.iter().find(|t| t.name == name)
    }

    fn node_name(&self, k: usize) -> &str {
        if k < self.params.len() {
            &self.params[k].name
        } else {
            &self.refs[k - self.params.len()].name
        }
    }

    fn node_expr(&self, k: usize) -> Option<&Expr> {
        if k < self.params.len() {
            self.params[k].formula.as_ref()
        } else {
            Some(&self.refs[k - self.params.len()].at)
        }
    }

    /// Topological order of parameters and planes; a cycle is an error with its path.
    fn dependency_order(&self) -> Result<Vec<usize>, FamilyError> {
        let n = self.params.len() + self.refs.len();
        let mut ix: HashMap<&str, usize> = HashMap::new();
        for k in 0..n {
            if ix.insert(self.node_name(k), k).is_some() {
                return Err(FamilyError::Param {
                    name: self.node_name(k).into(),
                    msg: "name used twice".into(),
                });
            }
        }
        let deps: Vec<Vec<usize>> = (0..n)
            .map(|k| {
                let mut v = vec![];
                if let Some(e) = self.node_expr(k) {
                    e.vars(&mut v);
                }
                v.iter()
                    .filter_map(|s| ix.get(s.as_str()).copied())
                    .collect()
            })
            .collect();
        let (mut state, mut order, mut stack) = (vec![0u8; n], vec![], vec![]);
        fn visit(
            k: usize,
            deps: &[Vec<usize>],
            state: &mut [u8],
            order: &mut Vec<usize>,
            stack: &mut Vec<usize>,
        ) -> Result<(), Vec<usize>> {
            match state[k] {
                2 => return Ok(()),
                1 => {
                    let st = stack.iter().position(|&s| s == k).unwrap_or(0);
                    let mut p = stack[st..].to_vec();
                    p.push(k);
                    return Err(p);
                }
                _ => {}
            }
            state[k] = 1;
            stack.push(k);
            for &d in &deps[k] {
                visit(d, deps, state, order, stack)?;
            }
            stack.pop();
            state[k] = 2;
            order.push(k);
            Ok(())
        }
        for k in 0..n {
            visit(k, &deps, &mut state, &mut order, &mut stack).map_err(|p| {
                FamilyError::Cycle(p.iter().map(|&i| self.node_name(i).to_string()).collect())
            })?;
        }
        Ok(order)
    }

    /// Every name in formulas and geometry is a parameter or a plane.
    fn check_names(&self) -> Result<(), FamilyError> {
        let known = |s: &str| {
            self.param(s).is_some() || self.refs.iter().any(|r| r.name == s) || self.ext.defines(s)
        };
        let probe = |owner: &str, es: Vec<&Expr>| -> Result<(), FamilyError> {
            let mut v = vec![];
            es.iter().for_each(|e| e.vars(&mut v));
            if let Some(bad) = v.iter().find(|s| !known(s)) {
                return Err(FamilyError::Param {
                    name: owner.into(),
                    msg: format!("unknown name '{bad}'"),
                });
            }
            Ok(())
        };
        for k in 0..self.params.len() + self.refs.len() {
            if let Some(e) = self.node_expr(k) {
                probe(self.node_name(k), vec![e])?;
            }
        }
        for s in &self.solids {
            probe(&s.name, s.exprs())?;
            if let Some(MatRef::Param(m)) = &s.material {
                if !self.param(m).is_some_and(|p| p.kind.is_text()) {
                    return Err(FamilyError::Param {
                        name: s.name.clone(),
                        msg: format!("material '{m}' is not a material parameter"),
                    });
                }
            }
        }
        for s in &self.symbols {
            probe("symbol", s.exprs())?;
        }
        for c in &self.connectors {
            probe(
                &c.name,
                c.at.iter().chain(&c.dir).chain([&c.size]).collect(),
            )?;
        }
        Ok(())
    }

    /// (body, reference plane) pairs: body coordinates given by a plane reference are locked.
    pub fn locks(&self) -> Vec<(String, String)> {
        let mut out = vec![];
        for s in &self.solids {
            let mut v = vec![];
            for e in s.exprs() {
                if let Expr::Var(n) = e {
                    v.push(n.clone());
                }
            }
            let names = self.refs.iter().map(|r| &r.name);
            for r in names.chain(self.ext.free_refs.iter().map(|r| &r.name)) {
                if v.contains(r) && !out.contains(&(s.name.clone(), r.clone())) {
                    out.push((s.name.clone(), r.clone()));
                }
            }
        }
        out
    }

    /// Values: formula > instance (instance parameters only) > type > default.
    pub fn resolve(
        &self,
        type_values: &[(String, Value)],
        instance: &[(String, Value)],
    ) -> Result<Resolved, FamilyError> {
        let perr = |name: &str, msg: String| FamilyError::Param {
            name: name.into(),
            msg,
        };
        for (k, _) in instance {
            let p = self
                .param(k)
                .ok_or_else(|| perr(k, "unknown parameter".into()))?;
            if !p.instance {
                return Err(perr(k, "is a type parameter; set it on the type".into()));
            }
        }
        if let Some((k, _)) = instance
            .iter()
            .chain(type_values)
            .find(|(k, _)| self.ext.has_text_formula(k))
        {
            return Err(perr(k, "is a formula and cannot be set".into()));
        }
        let mut r = Resolved::default();
        for &k in &self.order {
            let name = self.node_name(k).to_string();
            let env = |s: &str| r.nums.get(s).copied();
            if k >= self.params.len() {
                let v = self.refs[k - self.params.len()]
                    .at
                    .eval(&env)
                    .map_err(|m| perr(&name, m))?;
                check_kind(Kind::Length, v).map_err(|m| perr(&name, m))?;
                r.nums.insert(name, expr::q(v.v, LENGTH));
                continue;
            }
            let p = &self.params[k];
            let given = instance
                .iter()
                .chain(type_values.iter())
                .find(|(n, _)| *n == p.name)
                .map(|x| x.1.clone());
            let val = match (&p.formula, given) {
                (Some(_), Some(_)) => {
                    return Err(perr(&name, "is a formula and cannot be set".into()))
                }
                (Some(e), None) => {
                    let v = e.eval(&env).map_err(|m| perr(&name, m))?;
                    Value::Num(check_kind(p.kind, v).map_err(|m| perr(&name, m))?)
                }
                (None, Some(v)) => v,
                (None, None) => p.default.clone(),
            };
            match (val, p.kind.is_text()) {
                (Value::Text(t), true) => {
                    r.texts.insert(name, t);
                }
                (Value::Num(x), false) => {
                    if !x.is_finite() {
                        return Err(perr(&name, "not finite".into()));
                    }
                    self.check_limit(&name, x).map_err(|m| perr(&name, m))?;
                    r.nums.insert(name, expr::q(x, p.kind.dim()));
                }
                _ => {
                    return Err(perr(
                        &name,
                        format!("value does not match kind {}", p.kind.name()),
                    ))
                }
            }
        }
        self.ext.finish(self, &mut r)?;
        Ok(r)
    }

    /// Type values by name, plus a type check: the type exists in the definition.
    pub fn type_values(&self, ty: &str) -> Option<&[(String, Value)]> {
        self.family_type(ty).map(|t| t.values.as_slice())
    }
}

impl Profile {
    fn exprs(&self) -> Vec<&Expr> {
        match self {
            Profile::Rect(a) => a.iter().collect(),
            Profile::Circle(a) => a.iter().collect(),
            Profile::Poly(p) => p.iter().flatten().collect(),
        }
    }
}

impl Solid {
    fn exprs(&self) -> Vec<&Expr> {
        match &self.shape {
            Shape::Extrude { profile, range, .. } => {
                let mut v = profile.exprs();
                v.extend(range.iter());
                v
            }
            Shape::Revolve {
                profile,
                axis,
                angle,
            } => {
                let mut v = profile.exprs();
                v.extend(axis.iter());
                v.push(angle);
                v
            }
            Shape::Sweep { profile, path } => {
                let mut v = profile.exprs();
                v.extend(path.iter().flatten());
                v
            }
            Shape::Blend {
                bottom, top, range, ..
            } => {
                let mut v = bottom.exprs();
                v.extend(top.exprs());
                v.extend(range.iter());
                v
            }
            Shape::Mesh { scale, .. } => scale.iter().flatten().collect(),
            Shape::Feature { .. } => vec![],
            Shape::Plant { form, dims, leaf, seed, .. } => {
                let mut v: Vec<&Expr> = dims.iter().collect();
                v.extend([form, leaf, seed]);
                v
            }
        }
    }
}

impl Symbol {
    fn exprs(&self) -> Vec<&Expr> {
        match &self.shape {
            SymShape::Line(a) | SymShape::Rect(a) => a.iter().collect(),
            SymShape::Arc(a) => a.iter().collect(),
            SymShape::Circle(a) => a.iter().collect(),
            SymShape::Poly(p, _) => p.iter().flatten().collect(),
        }
    }
}

fn solid(head: &str, rest: &str) -> Result<Solid, String> {
    let it = items(rest)?;
    let Some((name, None)) = it.first().cloned() else {
        return Err(format!("{head} <name> ..."));
    };
    let (mut profs, mut range, mut dir, mut ax, mut angle, mut path) =
        (vec![], None, Axis::Z, None, None, None);
    let (mut data, mut scale, mut tl) = (None, None, None);
    let mut pl = PlantClauses::default();
    let (mut void, mut material, mut v) = (false, None, ALL_LOD | PLAN | MODEL | ELEV);
    let mut alt = false;
    for (w, a) in &it[1..] {
        match (w.as_str(), a) {
            ("void", None) => void = true,
            ("alt", None) => alt = true,
            (w, Some(a)) => {
                if let Some(p) = profile(w, a)? {
                    profs.push(p);
                    continue;
                }
                match w {
                    "z" | "range" => range = Some(exprs::<2>(w, a)?),
                    "dir" => dir = axis(a)?,
                    "axis" => ax = Some(exprs::<2>(w, a)?),
                    "angle" => angle = Some(ex(a)?),
                    "path" => {
                        let p = tuples::<3>("path", a)?;
                        if p.len() < 2 {
                            return Err("path() needs at least 2 points".into());
                        }
                        path = Some(p);
                    }
                    "mat" => {
                        material = Some(match unquote(a) {
                            Some(t) => MatRef::Literal(t),
                            None => MatRef::Param(a.trim().to_string()),
                        })
                    }
                    "show" => v = vis(a)?,
                    "data" if head == "mesh" => {
                        data = Some(std::sync::Arc::new(meshbody::MeshBody::decode(a)?))
                    }
                    "data" if head == "feature" => {
                        tl = Some(std::sync::Arc::new(crate::feature::json::from_data(a)?))
                    }
                    "scale" if head == "mesh" => scale = Some(exprs::<3>(w, a)?),
                    "part" | "form" | "height" | "crown" | "trunk" | "clear" | "leaf" | "seed"
                        if head == "plant" =>
                    {
                        pl.set(w, a)?
                    }
                    o => return Err(format!("unknown clause {o}()")),
                }
            }
            (o, None) => return Err(format!("unexpected '{o}'")),
        }
    }
    let need = match head {
        "blend" => 2,
        "mesh" | "feature" | "plant" => 0,
        _ => 1,
    };
    if profs.len() != need {
        return Err(format!(
            "{head} needs {need} profile(s), got {}",
            profs.len()
        ));
    }
    let mut pr = profs.into_iter();
    let mut p = || pr.next().expect("profile");
    let shape = match head {
        "extrude" => Shape::Extrude {
            profile: p(),
            dir,
            range: range.ok_or("extrude needs z(a, b)")?,
        },
        "revolve" => Shape::Revolve {
            profile: p(),
            axis: ax.unwrap_or_else(|| {
                [
                    Expr::Num(expr::q(0.0, LENGTH)),
                    Expr::Num(expr::q(0.0, LENGTH)),
                ]
            }),
            angle: angle.unwrap_or(Expr::Num(expr::q(2.0 * std::f64::consts::PI, ANGLE))),
        },
        "mesh" => Shape::Mesh {
            body: data.ok_or("mesh needs data(...)")?,
            scale,
        },
        "feature" => Shape::Feature {
            tl: tl.ok_or("feature needs data(...)")?,
        },
        "plant" => pl.finish()?,
        "sweep" => Shape::Sweep {
            profile: p(),
            path: path.ok_or("sweep needs path(...)")?,
        },
        _ => Shape::Blend {
            bottom: p(),
            top: p(),
            range: range.ok_or("blend needs z(a, b)")?,
            dir,
        },
    };
    if alt && void {
        return Err("a solid is either void or alt".into());
    }
    Ok(Solid {
        name,
        shape,
        void,
        alt,
        material,
        vis: v,
    })
}

/// Clauses of a `plant` solid.
#[derive(Default)]
struct PlantClauses {
    organ: Option<plant::Organ>,
    form: Option<Expr>,
    dims: [Option<Expr>; 4],
    leaf: Option<Expr>,
    seed: Option<Expr>,
}

impl PlantClauses {
    fn set(&mut self, w: &str, a: &str) -> Result<(), String> {
        match w {
            "part" => {
                self.organ = Some(plant::Organ::from_name(a).ok_or("part() is wood, foliage, shade, light or streak")?)
            }
            "form" => {
                self.form = Some(match plant::Form::from_name(a) {
                    Some(f) => Expr::Num(expr::q(f.code(), NUMBER)),
                    None => ex(a)?,
                })
            }
            "height" => self.dims[0] = Some(ex(a)?),
            "crown" => self.dims[1] = Some(ex(a)?),
            "trunk" => self.dims[2] = Some(ex(a)?),
            "clear" => self.dims[3] = Some(ex(a)?),
            "leaf" => self.leaf = Some(ex(a)?),
            _ => self.seed = Some(ex(a)?),
        }
        Ok(())
    }

    fn finish(self) -> Result<Shape, String> {
        let [h, c, t, k] = self.dims;
        Ok(Shape::Plant {
            organ: self.organ.ok_or("plant needs part(wood | foliage)")?,
            form: self.form.ok_or("plant needs form(...)")?,
            dims: [
                h.ok_or("plant needs height(...)")?,
                c.ok_or("plant needs crown(...)")?,
                t.ok_or("plant needs trunk(...)")?,
                k.unwrap_or(Expr::Num(expr::q(0.0, LENGTH))),
            ],
            leaf: self.leaf.unwrap_or(Expr::Num(expr::q(1.0, NUMBER))),
            seed: self.seed.unwrap_or(Expr::Num(expr::q(0.0, NUMBER))),
        })
    }
}

fn symbol(rest: &str) -> Result<Symbol, String> {
    let it = items(rest)?;
    let (mut shape, mut v) = (None, ALL_LOD | PLAN);
    for (w, a) in it {
        let Some(a) = a else {
            return Err(format!("unexpected '{w}'"));
        };
        let s = match w.as_str() {
            "line" => SymShape::Line(exprs("line", &a)?),
            "arc" => SymShape::Arc(exprs("arc", &a)?),
            "circle" => SymShape::Circle(exprs("circle", &a)?),
            "rect" => SymShape::Rect(exprs("rect", &a)?),
            "poly" | "loop" => SymShape::Poly(tuples::<2>(&w, &a)?, w == "loop"),
            "show" => {
                v = vis(&a)?;
                continue;
            }
            o => return Err(format!("unknown symbol {o}()")),
        };
        shape = Some(s);
    }
    Ok(Symbol {
        shape: shape.ok_or("symbol needs line/arc/circle/rect/poly/loop")?,
        vis: v,
    })
}

/// Mesh cache key: identical values share one mesh.
pub fn cache_key(r: &Resolved) -> String {
    let mut m: BTreeMap<&str, String> = BTreeMap::new();
    for (k, v) in &r.nums {
        m.insert(k, format!("{:?}", v.v));
    }
    for (k, v) in &r.texts {
        m.insert(k, v.clone());
    }
    m.into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";")
}
