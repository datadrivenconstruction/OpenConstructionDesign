//! `ddc-ops`: the one serialisable edit language shared by UI tools, XLSX import, scripts,
//! the chat plan and MCP agents (docs/design/AI_NATIVE.md section 2).
//!
//! Wire types only. The model crate lowers them; nothing here touches the model or the UI.
//! Rules of the wire: `v` must equal `OP_VERSION` (older versions are upgraded first,
//! `upgrade.rs`), unknown fields are rejected, every
//! quantity names its unit, refs are stable (`cat:id`, envelope slots, names).

pub mod check;
pub mod geo;
pub mod op;
pub mod reason;
pub mod parts;
pub mod place;
pub mod refs;
pub mod schema;
pub mod sample;
pub mod units;
pub mod upgrade;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use check::Issue;
pub use reason::{Code, Rejection};
pub use geo::{
    BoolKind, ConvertTo, Curve3, CurveAlign, CurveFit, Div, End, Form, Frame, GeoOp, IfcClass,
    LatticeHost, LatticeKind, LatticePattern, LatticeSupports, MemberRole, Mullion, RoleTag, PanelDiv, PanelPattern, Path, Pattern, PatternOut, Place, PlateKind, Profile, Section, ShellKind,
    ShellLayer, ShellSide, Side, SnapKind, Spacing, SurfAlign, SurfaceBy, Tool, UvTrim,
    plate_kind,
};
pub use op::{
    Axis, FamilyForm, FamilyOp, FamilyParam, FamilyRule, GridOp, LevelOp, NewEl, Op, OptionOp,
    ParamKind, ParamOp, RefPlaneDef, Rule, RuleKind, TypeOp, Xform,
};
pub use parts::{
    resolve, FaceName, PartGeo, PartName, PartSource, Shape, ShapeMap, TagFace, Unresolved,
};
pub use place::{resolve_all, PlaceModel, Placed};
pub use refs::{Cat, Ref, RefParseError, Targets};
pub use units::{Angle, AngleUnit, Dim, Dir3, Field, Len, LenUnit, Unit, Value, P2, P3, V3};

/// Wire version this build writes. v2: type refs name their table (`wtype:`, `stype:`,
/// `mtype:`); v1 envelopes are read through `upgrade::v1_to_v2`.
pub const OP_VERSION: u32 = 2;

/// One envelope = one journal entry = one undo step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub v: u32,
    pub actor: Actor,
    pub title: String,
    pub why: Option<String>,
    pub groups: Vec<Group>,
}

/// Unit of partial merge: a person may take some groups; `depends_on` names earlier groups.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: u32,
    pub title: String,
    pub ops: Vec<Op>,
    #[serde(default)]
    pub depends_on: Vec<u32>,
    /// Optional rationale for this group; additive, absent on older envelopes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
}

/// Who produced the envelope (recorded per journal row).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Actor {
    User,
    // wire name: the format, `xlsx`
    Xlsx,
    Script { name: String },
    Ai { session: String, model: String },
    Agent { name: String },
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("ops json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("envelope has no version field `v`")]
    NoVersion,
    #[error("envelope version {got}, this build reads {want}")]
    Version { got: u64, want: u32 },
}

impl Envelope {
    pub fn new(actor: Actor, title: &str) -> Envelope {
        Envelope { v: OP_VERSION, actor, title: title.to_owned(), why: None, groups: Vec::new() }
    }

    /// Strict parse: version first (so a wrong version is reported as such, not as an
    /// unknown field), then the full shape with unknown fields rejected.
    pub fn from_json(s: &str) -> Result<Envelope, WireError> {
        #[derive(Deserialize)]
        struct Probe {
            v: Option<u64>,
        }
        let probe: Probe = serde_json::from_str(s)?;
        let got = probe.v.ok_or(WireError::NoVersion)?;
        if got == u64::from(OP_VERSION) {
            return Ok(serde_json::from_str(s)?);
        }
        Envelope::from_value(serde_json::from_str(s)?)
    }

    /// Same rules as `from_json`; an older version is upgraded to `OP_VERSION` first.
    pub fn from_value(mut v: serde_json::Value) -> Result<Envelope, WireError> {
        let got = v.get("v").ok_or(WireError::NoVersion)?;
        let got = got.as_u64().ok_or(WireError::NoVersion)?;
        if got != u64::from(OP_VERSION) && !upgrade::upgrade(&mut v, got) {
            return Err(WireError::Version { got, want: OP_VERSION });
        }
        Ok(serde_json::from_value(v)?)
    }

    pub fn to_json(&self) -> Result<String, WireError> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn to_json_pretty(&self) -> Result<String, WireError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn ops(&self) -> impl Iterator<Item = &Op> {
        self.groups.iter().flat_map(|g| g.ops.iter())
    }

    pub fn op_count(&self) -> usize {
        self.groups.iter().map(|g| g.ops.len()).sum()
    }
}
