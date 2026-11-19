//! Steel connections (g5): an own catalogue of bolts, welds, plates and spacing rules, and ten
//! parametric connections that turn joined members into plates, stiffeners, bolts and welds as
//! real solids. A connection is a function of its members and parameters: change a section and
//! building it again gives the adapted detail. Checks, schedules and the analysis hook read
//! the same output.

pub mod bar;
mod base;
pub mod catalog;
pub mod check;
pub mod detail;
pub mod forces;
pub mod gridshell;
mod gusset;
mod kit;
mod moment;
pub mod part;
pub mod schedule;
mod shear;
mod sphere;
pub mod hier;
pub mod hier_check;
pub mod hier_render;
pub mod lattice;
pub mod longspan;
pub mod outfit;
pub mod plate;
pub mod profile;
pub mod rivet;
pub mod rolled;
pub mod sections;
pub mod tower;
pub mod tower_size;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_fit;
#[cfg(test)]
mod tests_lattice;
#[cfg(test)]
mod tests_grid;
#[cfg(test)]
mod tests_hier;
#[cfg(test)]
mod tests_outfit;
#[cfg(test)]
mod tests_rolled;

use crate::math::V3;
use crate::member::Member;

pub use bar::{Bar, End};
pub use part::{BoltInfo, Part, PartQty, Role};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnError(pub String);

impl ConnError {
    pub fn new(m: impl Into<String>) -> Self {
        ConnError(m.into())
    }
}

impl std::fmt::Display for ConnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ConnError {
    fn from(s: String) -> Self {
        ConnError(s)
    }
}

/// The ten connection types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ConnKind {
    BasePlate,
    Pinned,
    RigidEndPlate,
    BeamSplice,
    ColumnSplice,
    TrussGusset,
    BracingGusset,
    PurlinCleat,
    StairStringer,
    SphereNode,
}

impl ConnKind {
    pub const ALL: [ConnKind; 10] = [
        ConnKind::BasePlate,
        ConnKind::Pinned,
        ConnKind::RigidEndPlate,
        ConnKind::BeamSplice,
        ConnKind::ColumnSplice,
        ConnKind::TrussGusset,
        ConnKind::BracingGusset,
        ConnKind::PurlinCleat,
        ConnKind::StairStringer,
        ConnKind::SphereNode,
    ];

    pub fn key(self) -> &'static str {
        match self {
            ConnKind::BasePlate => "base_plate",
            ConnKind::Pinned => "pinned",
            ConnKind::RigidEndPlate => "rigid_end_plate",
            ConnKind::BeamSplice => "beam_splice",
            ConnKind::ColumnSplice => "column_splice",
            ConnKind::TrussGusset => "truss_gusset",
            ConnKind::BracingGusset => "bracing_gusset",
            ConnKind::PurlinCleat => "purlin_cleat",
            ConnKind::StairStringer => "stair_stringer",
            ConnKind::SphereNode => "sphere_node",
        }
    }

    pub fn parse(s: &str) -> Option<ConnKind> {
        ConnKind::ALL.into_iter().find(|k| k.key() == s.trim())
    }

    /// Members the type takes: (least, most).
    pub fn arity(self) -> (usize, usize) {
        match self {
            ConnKind::BasePlate => (1, 1),
            ConnKind::Pinned
            | ConnKind::RigidEndPlate
            | ConnKind::BeamSplice
            | ConnKind::ColumnSplice
            | ConnKind::PurlinCleat
            | ConnKind::StairStringer => (2, 2),
            ConnKind::TrussGusset => (2, 8),
            ConnKind::BracingGusset => (2, 5),
            ConnKind::SphereNode => (2, 16),
        }
    }

    /// The order members are given in, for messages and the tool schema.
    pub fn members_hint(self) -> &'static str {
        match self {
            ConnKind::BasePlate => "column",
            ConnKind::Pinned | ConnKind::RigidEndPlate => "support (column or girder), beam",
            ConnKind::BeamSplice => "two collinear beams",
            ConnKind::ColumnSplice => "lower column, upper column",
            ConnKind::TrussGusset => "chord, then web members",
            ConnKind::BracingGusset => "column or beam (and the other one at a corner), braces",
            ConnKind::PurlinCleat => "rafter, purlin",
            ConnKind::StairStringer => "support beam, stringer",
            ConnKind::SphereNode => "members meeting at the ball",
        }
    }
}

/// Optional parameters; None takes the catalogue default for the members.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params {
    /// Bolt diameter, m.
    pub bolt: Option<f64>,
    pub class: Option<String>,
    pub rows: Option<usize>,
    pub cols: Option<usize>,
    /// Main plate thickness, m.
    pub plate: Option<f64>,
    /// `fin` | `end_plate` (pinned), `cover` | `end_plate` (splices).
    pub variant: Option<String>,
    pub stiffeners: Option<bool>,
    pub anchors: Option<usize>,
    /// Fillet leg, m.
    pub leg: Option<f64>,
    /// Fin plate side: +1 along the beam's section y, -1 against it.
    pub side: Option<i8>,
    /// Ball diameter, m.
    pub ball: Option<f64>,
    /// Gap between member end and support, m.
    pub gap: Option<f64>,
    /// Anchor embedment below the grout, m.
    pub embed: Option<f64>,
}

const PARAM_KEYS: [&str; 13] = [
    "bolt",
    "class",
    "rows",
    "cols",
    "plate",
    "variant",
    "stiffeners",
    "anchors",
    "leg",
    "side",
    "ball",
    "gap",
    "embed",
];

impl Params {
    /// Names a parameter may have (lengths in mm).
    pub fn keys() -> &'static [&'static str] {
        &PARAM_KEYS
    }

    /// `bolt=20;class=8.8;rows=3` (lengths in mm); empty when all are defaults.
    pub fn text(&self) -> String {
        let mm = |x: f64| catalog::fmt_mm(x);
        let mut v: Vec<String> = vec![];
        let mut put = |k: &str, s: Option<String>| {
            if let Some(s) = s {
                v.push(format!("{k}={s}"));
            }
        };
        put("bolt", self.bolt.map(mm));
        put("class", self.class.clone());
        put("rows", self.rows.map(|x| x.to_string()));
        put("cols", self.cols.map(|x| x.to_string()));
        put("plate", self.plate.map(mm));
        put("variant", self.variant.clone());
        put("stiffeners", self.stiffeners.map(|x| x.to_string()));
        put("anchors", self.anchors.map(|x| x.to_string()));
        put("leg", self.leg.map(mm));
        put("side", self.side.map(|x| x.to_string()));
        put("ball", self.ball.map(mm));
        put("gap", self.gap.map(mm));
        put("embed", self.embed.map(mm));
        v.join(";")
    }

    /// Reads [`Self::text`]; also takes `M20` for a bolt. Unknown keys and bad values are refused.
    pub fn parse(s: &str) -> Result<Params, String> {
        let mut p = Params::default();
        for kv in s.split(';').map(str::trim).filter(|x| !x.is_empty()) {
            let (k, v) = kv
                .split_once('=')
                .ok_or_else(|| format!("parameter {kv:?} needs key=value"))?;
            p.set(k.trim(), v.trim())?;
        }
        Ok(p)
    }

    /// Sets one parameter from text (lengths in mm).
    pub fn set(&mut self, k: &str, v: &str) -> Result<(), String> {
        let bad = || format!("bad value for {k}: {v:?}");
        let mm = |v: &str| -> Result<f64, String> {
            let x: f64 = v
                .trim()
                .trim_end_matches("mm")
                .trim()
                .parse()
                .map_err(|_| bad())?;
            if x.is_finite() && x > 0.0 && x < 10_000.0 {
                Ok(x / 1000.0)
            } else {
                Err(bad())
            }
        };
        let count = |v: &str| -> Result<usize, String> {
            v.parse::<usize>()
                .ok()
                .filter(|n| (1..=64).contains(n))
                .ok_or_else(bad)
        };
        match k {
            "bolt" => self.bolt = Some(mm(v.trim_start_matches(['M', 'm']))?),
            "class" => {
                catalog::catalog().class(v).ok_or_else(bad)?;
                self.class = Some(v.to_string());
            }
            "rows" => self.rows = Some(count(v)?),
            "cols" => self.cols = Some(count(v)?),
            "plate" => self.plate = Some(mm(v)?),
            "variant" => {
                if !["fin", "end_plate", "cover"].contains(&v) {
                    return Err(bad());
                }
                self.variant = Some(v.to_string());
            }
            "stiffeners" => self.stiffeners = Some(v.parse().map_err(|_| bad())?),
            "anchors" => self.anchors = Some(count(v)?),
            "leg" => self.leg = Some(mm(v)?),
            "side" => {
                self.side = Some(match v {
                    "1" | "+1" => 1,
                    "-1" => -1,
                    _ => return Err(bad()),
                })
            }
            "ball" => self.ball = Some(mm(v)?),
            "gap" => self.gap = Some(mm(v)?),
            "embed" => self.embed = Some(mm(v)?),
            other => {
                return Err(format!(
                    "unknown parameter {other:?}; known: {}",
                    PARAM_KEYS.join(", ")
                ))
            }
        }
        Ok(())
    }
}

/// A member end the connection asks to stop short of the node (ball nodes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trim {
    pub member: usize,
    pub end: End,
    /// Distance from the node to where the member should end.
    pub setback: f64,
}

/// One built connection.
#[derive(Clone, Debug, PartialEq)]
pub struct Conn {
    pub kind: ConnKind,
    pub node: V3,
    pub parts: Vec<Part>,
    /// Bolt sets and anchor rods; `shank` indices point into `parts`.
    pub bolts: Vec<(usize, BoltInfo)>,
    /// What the detail had to give up (English, for the agent and the issue list).
    pub notes: Vec<String>,
    pub trims: Vec<Trim>,
}

/// Builds a connection of `kind` on `members` (in [`ConnKind::members_hint`] order).
pub fn build(kind: ConnKind, members: &[Member], p: &Params) -> Result<Conn, ConnError> {
    let (lo, hi) = kind.arity();
    if members.len() < lo || members.len() > hi {
        return Err(ConnError(format!(
            "{} joins {}{} members ({}), got {}",
            kind.key(),
            lo,
            if hi > lo {
                format!(" to {hi}")
            } else {
                String::new()
            },
            kind.members_hint(),
            members.len()
        )));
    }
    let bars = members
        .iter()
        .map(Bar::new)
        .collect::<Result<Vec<_>, _>>()?;
    let mut k = kit::Kit::new(kind, p)?;
    match kind {
        ConnKind::BasePlate => base::base_plate(&mut k, &bars[0])?,
        ConnKind::Pinned => shear::pinned(&mut k, &bars[0], &bars[1])?,
        ConnKind::StairStringer => shear::stringer(&mut k, &bars[0], &bars[1])?,
        ConnKind::PurlinCleat => shear::purlin_cleat(&mut k, &bars[0], &bars[1])?,
        ConnKind::RigidEndPlate => moment::rigid(&mut k, &bars[0], &bars[1])?,
        ConnKind::BeamSplice | ConnKind::ColumnSplice => {
            moment::splice(&mut k, &bars[0], &bars[1])?
        }
        ConnKind::TrussGusset => gusset::truss(&mut k, &bars)?,
        ConnKind::BracingGusset => gusset::bracing(&mut k, &bars)?,
        ConnKind::SphereNode => sphere::ball(&mut k, &bars)?,
    }
    Ok(k.finish())
}
