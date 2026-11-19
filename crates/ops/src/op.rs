// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `Op` v1 and its sub-op families (AI_NATIVE.md section 2).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geo::{Form, GeoOp, Pattern, PatternOut, Place};
use crate::refs::{Cat, Ref, Targets};
use crate::units::{explicit, Angle, Dir3, Field, Len, Unit, Value, P2, P3, V3};

/// One edit. Wire form is internally tagged: `{"op": "set", ...}`; sub-op families add
/// `"action"`: `{"op": "type", "action": "duplicate", ...}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    Create { slot: u32, el: NewEl },
    Set { target: Targets, fields: Vec<Field> },
    SetType { target: Targets, to: Ref },
    Delete { target: Targets },
    Transform {
        target: Targets,
        xf: Xform,
        #[serde(default)]
        copy: bool,
    },
    Pattern {
        source: Targets,
        pattern: Pattern,
        #[serde(default)]
        out: PatternOut,
    },
    Geometry(GeoOp),
    Constrain { rule: Rule },
    Type(TypeOp),
    Param(ParamOp),
    Family(FamilyOp),
    Level(LevelOp),
    Grid(GridOp),
    Option(OptionOp),
}

/// New element, one variant per family row of the element contract. `fields` carries the
/// family's remaining rows by name; `Generic` covers families without a typed variant yet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NewEl {
    Wall {
        level: Ref,
        a: Place,
        b: Place,
        height: Len,
        top: Option<Ref>,
        #[serde(rename = "type")]
        ty: Option<Ref>,
        fields: Vec<Field>,
    },
    Door {
        host: Ref,
        at: Place,
        #[serde(rename = "type")]
        ty: Option<Ref>,
        fields: Vec<Field>,
    },
    Window {
        host: Ref,
        at: Place,
        sill: Len,
        #[serde(rename = "type")]
        ty: Option<Ref>,
        fields: Vec<Field>,
    },
    Opening { host: Ref, at: Place, w: Len, h: Len, sill: Len },
    Slab {
        level: Ref,
        boundary: Vec<Place>,
        offset: Len,
        #[serde(rename = "type")]
        ty: Option<Ref>,
        fields: Vec<Field>,
    },
    Roof {
        level: Ref,
        boundary: Vec<Place>,
        slope: Angle,
        #[serde(rename = "type")]
        ty: Option<Ref>,
        fields: Vec<Field>,
    },
    Ceiling {
        level: Ref,
        boundary: Vec<Place>,
        height: Len,
        #[serde(rename = "type")]
        ty: Option<Ref>,
        fields: Vec<Field>,
    },
    Column {
        level: Ref,
        at: Place,
        top: Option<Ref>,
        height: Len,
        rotation: Angle,
        #[serde(rename = "type")]
        ty: Option<Ref>,
        fields: Vec<Field>,
    },
    Beam {
        level: Ref,
        a: Place,
        b: Place,
        #[serde(rename = "type")]
        ty: Option<Ref>,
        fields: Vec<Field>,
    },
    Stair {
        level: Ref,
        a: Place,
        b: Place,
        top: Option<Ref>,
        width: Len,
        #[serde(rename = "type")]
        ty: Option<Ref>,
        fields: Vec<Field>,
    },
    Railing {
        level: Ref,
        path: Vec<Place>,
        host: Option<Ref>,
        height: Len,
        #[serde(rename = "type")]
        ty: Option<Ref>,
        fields: Vec<Field>,
    },
    Room { level: Ref, at: Place, name: String, number: Option<String> },
    Component {
        level: Ref,
        at: Place,
        family: Ref,
        host: Option<Ref>,
        rotation: Angle,
        #[serde(rename = "type")]
        ty: Option<Ref>,
        fields: Vec<Field>,
    },
    MepRun {
        level: Ref,
        system: String,
        path: Vec<Place>,
        size: Len,
        #[serde(rename = "type")]
        ty: Option<Ref>,
        fields: Vec<Field>,
    },
    Generic {
        cat: Cat,
        level: Option<Ref>,
        at: Vec<Place>,
        #[serde(rename = "type")]
        ty: Option<Ref>,
        fields: Vec<Field>,
    },
}

/// Rigid transform (no scale).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Xform {
    Move { by: V3 },
    Rotate { center: P3, axis: Dir3, angle: Angle },
    /// Mirror about the vertical plane through `a`-`b`.
    Mirror { a: P3, b: P3 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    Align,
    Distance,
    Equal,
    Parallel,
    Perpendicular,
    Coincident,
    Distribute,
    Lock,
}

/// Persisted constraint (`constraint_rule`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub kind: RuleKind,
    pub refs: Vec<Ref>,
    pub value: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TypeOp {
    Duplicate { slot: u32, from: Ref, name: String, fields: Vec<Field> },
    Rename { target: Ref, name: String },
    Set { target: Ref, fields: Vec<Field> },
    /// Instances move to `replace_with` first; without it, a type in use is refused.
    Delete { target: Ref, replace_with: Option<Ref> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    Text,
    Integer,
    Number,
    Length,
    Area,
    Volume,
    Angle,
    Bool,
    Ref,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ParamOp {
    Define {
        name: String,
        kind: ParamKind,
        /// Unit of values of this parameter, or null when dimensionless. The key must be present.
        #[serde(deserialize_with = "explicit")]
        #[schemars(required)]
        unit: Option<Unit>,
        cats: Vec<Cat>,
        instance: bool,
        group: Option<String>,
    },
    Rename { from: String, to: String },
    Delete { name: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    X,
    Y,
    Z,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FamilyParam {
    pub name: String,
    pub kind: ParamKind,
    /// Unit of the value, or null when dimensionless. The key must be present.
    #[serde(deserialize_with = "explicit")]
    #[schemars(required)]
    pub unit: Option<Unit>,
    pub default: Value,
    pub formula: Option<String>,
    pub instance: bool,
}

/// Reference plane of a family rig, normal to `axis`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RefPlaneDef {
    pub name: String,
    pub axis: Axis,
    pub offset: Len,
    pub formula: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FamilyForm {
    pub name: String,
    pub form: Form,
    pub visible_if: Option<String>,
    pub material: Option<String>,
}

/// Constraint inside a family: planes by name, optional driving parameter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FamilyRule {
    pub kind: RuleKind,
    pub planes: Vec<String>,
    pub param: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum FamilyOp {
    /// Lands as a draft family; `tests` are the flex-check parameter sets.
    Define {
        slot: u32,
        name: String,
        cat: Cat,
        params: Vec<FamilyParam>,
        planes: Vec<RefPlaneDef>,
        forms: Vec<FamilyForm>,
        constraints: Vec<FamilyRule>,
        tests: Vec<Vec<Field>>,
    },
    SetFormula { target: Ref, param: String, formula: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum LevelOp {
    Create { slot: u32, name: String, elevation: Len },
    Rename { target: Ref, name: String },
    SetElevation { target: Ref, elevation: Len },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum GridOp {
    Create { slot: u32, name: String, a: P2, b: P2, bulge: f64 },
    Rename { target: Ref, name: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum OptionOp {
    CreateSet { slot: u32, name: String },
    Add { slot: u32, set: Ref, name: String },
    Assign { target: Targets, option: Ref },
    Accept { option: Ref },
}

impl Op {
    /// Every `kind()` value; a test checks the samples cover all of them.
    pub const KINDS: [&'static str; 41] = [
        "create",
        "set",
        "set_type",
        "delete",
        "transform",
        "pattern",
        "geometry.solid",
        "geometry.mass",
        "geometry.boolean",
        "geometry.void",
        "geometry.reshape",
        "geometry.convert",
        "geometry.freeform",
        "geometry.edit_point",
        "geometry.thicken",
        "geometry.trim",
        "geometry.rebuild",
        "geometry.panelize",
        "geometry.surface_shell",
        "geometry.gridshell",
        "geometry.member",
        "geometry.plate",
        "constrain",
        "type.duplicate",
        "type.rename",
        "type.set",
        "type.delete",
        "param.define",
        "param.rename",
        "param.delete",
        "family.define",
        "family.set_formula",
        "level.create",
        "level.rename",
        "level.set_elevation",
        "grid.create",
        "grid.rename",
        "option.create_set",
        "option.add",
        "option.assign",
        "option.accept",
    ];

    /// Stable kind name, e.g. `"type.duplicate"`. Exhaustive so new variants must be named.
    pub fn kind(&self) -> &'static str {
        match self {
            Op::Create { .. } => "create",
            Op::Set { .. } => "set",
            Op::SetType { .. } => "set_type",
            Op::Delete { .. } => "delete",
            Op::Transform { .. } => "transform",
            Op::Pattern { .. } => "pattern",
            Op::Geometry(g) => match g {
                GeoOp::Solid { .. } => "geometry.solid",
                GeoOp::Mass { .. } => "geometry.mass",
                GeoOp::Boolean { .. } => "geometry.boolean",
                GeoOp::Void { .. } => "geometry.void",
                GeoOp::Reshape { .. } => "geometry.reshape",
                GeoOp::Convert { .. } => "geometry.convert",
                GeoOp::Freeform { .. } => "geometry.freeform",
                GeoOp::EditPoint { .. } => "geometry.edit_point",
                GeoOp::Thicken { .. } => "geometry.thicken",
                GeoOp::Trim { .. } => "geometry.trim",
                GeoOp::Rebuild { .. } => "geometry.rebuild",
                GeoOp::Panelize { .. } => "geometry.panelize",
                GeoOp::SurfaceShell { .. } => "geometry.surface_shell",
                GeoOp::Gridshell { .. } => "geometry.gridshell",
                GeoOp::Member { .. } => "geometry.member",
                GeoOp::Plate { .. } => "geometry.plate",
            },
            Op::Constrain { .. } => "constrain",
            Op::Type(t) => match t {
                TypeOp::Duplicate { .. } => "type.duplicate",
                TypeOp::Rename { .. } => "type.rename",
                TypeOp::Set { .. } => "type.set",
                TypeOp::Delete { .. } => "type.delete",
            },
            Op::Param(p) => match p {
                ParamOp::Define { .. } => "param.define",
                ParamOp::Rename { .. } => "param.rename",
                ParamOp::Delete { .. } => "param.delete",
            },
            Op::Family(f) => match f {
                FamilyOp::Define { .. } => "family.define",
                FamilyOp::SetFormula { .. } => "family.set_formula",
            },
            Op::Level(l) => match l {
                LevelOp::Create { .. } => "level.create",
                LevelOp::Rename { .. } => "level.rename",
                LevelOp::SetElevation { .. } => "level.set_elevation",
            },
            Op::Grid(g) => match g {
                GridOp::Create { .. } => "grid.create",
                GridOp::Rename { .. } => "grid.rename",
            },
            Op::Option(o) => match o {
                OptionOp::CreateSet { .. } => "option.create_set",
                OptionOp::Add { .. } => "option.add",
                OptionOp::Assign { .. } => "option.assign",
                OptionOp::Accept { .. } => "option.accept",
            },
        }
    }

    /// Slot this op creates, if any.
    pub fn slot(&self) -> Option<u32> {
        match self {
            Op::Create { slot, .. }
            | Op::Geometry(GeoOp::Solid { slot, .. })
            | Op::Geometry(GeoOp::Mass { slot, .. })
            | Op::Geometry(GeoOp::Freeform { slot, .. })
            | Op::Geometry(GeoOp::Panelize { slot, .. })
            | Op::Geometry(GeoOp::SurfaceShell { slot, .. })
            | Op::Geometry(GeoOp::Gridshell { slot, .. })
            | Op::Geometry(GeoOp::Member { slot, .. })
            | Op::Geometry(GeoOp::Plate { slot, .. })
            | Op::Type(TypeOp::Duplicate { slot, .. })
            | Op::Family(FamilyOp::Define { slot, .. })
            | Op::Level(LevelOp::Create { slot, .. })
            | Op::Grid(GridOp::Create { slot, .. })
            | Op::Option(OptionOp::CreateSet { slot, .. })
            | Op::Option(OptionOp::Add { slot, .. }) => Some(*slot),
            _ => None,
        }
    }
}
