// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Quantities on the wire. Every length, angle and point names its unit; nothing is implied
//! by project display settings.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

/// Physical dimension of a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dim {
    Length,
    Area,
    Volume,
    Angle,
    Mass,
    Density,
    Force,
    Pressure,
    Time,
    Ratio,
}

/// Any unit a `Value` may carry. Wire names are stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum Unit {
    #[serde(rename = "m")]
    M,
    #[serde(rename = "cm")]
    Cm,
    #[serde(rename = "mm")]
    Mm,
    #[serde(rename = "ft")]
    Ft,
    #[serde(rename = "in")]
    In,
    #[serde(rename = "m2")]
    M2,
    #[serde(rename = "mm2")]
    Mm2,
    #[serde(rename = "m3")]
    M3,
    #[serde(rename = "deg")]
    Deg,
    #[serde(rename = "rad")]
    Rad,
    #[serde(rename = "kg")]
    Kg,
    #[serde(rename = "kg/m3")]
    KgPerM3,
    #[serde(rename = "kN")]
    KN,
    #[serde(rename = "kPa")]
    KPa,
    #[serde(rename = "s")]
    S,
    #[serde(rename = "pct")]
    Pct,
}

impl Unit {
    pub const ALL: [Unit; 16] = [
        Unit::M,
        Unit::Cm,
        Unit::Mm,
        Unit::Ft,
        Unit::In,
        Unit::M2,
        Unit::Mm2,
        Unit::M3,
        Unit::Deg,
        Unit::Rad,
        Unit::Kg,
        Unit::KgPerM3,
        Unit::KN,
        Unit::KPa,
        Unit::S,
        Unit::Pct,
    ];

    pub fn dim(self) -> Dim {
        match self {
            Unit::M | Unit::Cm | Unit::Mm | Unit::Ft | Unit::In => Dim::Length,
            Unit::M2 | Unit::Mm2 => Dim::Area,
            Unit::M3 => Dim::Volume,
            Unit::Deg | Unit::Rad => Dim::Angle,
            Unit::Kg => Dim::Mass,
            Unit::KgPerM3 => Dim::Density,
            Unit::KN => Dim::Force,
            Unit::KPa => Dim::Pressure,
            Unit::S => Dim::Time,
            Unit::Pct => Dim::Ratio,
        }
    }

    /// Factor to the SI base of the dimension (m, m2, m3, rad, kg, kg/m3, N, Pa, s, ratio).
    pub fn si_factor(self) -> f64 {
        match self {
            Unit::M | Unit::M2 | Unit::M3 | Unit::Rad | Unit::Kg | Unit::KgPerM3 | Unit::S => 1.0,
            Unit::Cm => 0.01,
            Unit::Mm => 0.001,
            Unit::Ft => 0.3048,
            Unit::In => 0.0254,
            Unit::Mm2 => 1e-6,
            Unit::Deg => std::f64::consts::PI / 180.0,
            Unit::KN | Unit::KPa => 1000.0,
            Unit::Pct => 0.01,
        }
    }

    pub fn to_si(self, v: f64) -> f64 {
        v * self.si_factor()
    }
}

/// Units allowed where the schema expects a length.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum LenUnit {
    #[serde(rename = "m")]
    M,
    #[serde(rename = "cm")]
    Cm,
    #[serde(rename = "mm")]
    Mm,
    #[serde(rename = "ft")]
    Ft,
    #[serde(rename = "in")]
    In,
}

impl From<LenUnit> for Unit {
    fn from(u: LenUnit) -> Unit {
        match u {
            LenUnit::M => Unit::M,
            LenUnit::Cm => Unit::Cm,
            LenUnit::Mm => Unit::Mm,
            LenUnit::Ft => Unit::Ft,
            LenUnit::In => Unit::In,
        }
    }
}

/// Units allowed where the schema expects an angle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum AngleUnit {
    #[serde(rename = "deg")]
    Deg,
    #[serde(rename = "rad")]
    Rad,
}

impl From<AngleUnit> for Unit {
    fn from(u: AngleUnit) -> Unit {
        match u {
            AngleUnit::Deg => Unit::Deg,
            AngleUnit::Rad => Unit::Rad,
        }
    }
}

/// A length with its unit, e.g. `{"v": 2100.0, "unit": "mm"}`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Len {
    /// Magnitude, expressed in `unit`.
    pub v: f64,
    /// Length unit of `v`: m, cm, mm, ft or in. Required.
    pub unit: LenUnit,
}

impl Len {
    pub fn m(v: f64) -> Len {
        Len { v, unit: LenUnit::M }
    }
    pub fn mm(v: f64) -> Len {
        Len { v, unit: LenUnit::Mm }
    }
    pub fn to_m(self) -> f64 {
        Unit::from(self.unit).to_si(self.v)
    }
}

/// An angle with its unit.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Angle {
    /// Magnitude, expressed in `unit`.
    pub v: f64,
    /// Angle unit of `v`: deg or rad. Required.
    pub unit: AngleUnit,
}

impl Angle {
    pub fn deg(v: f64) -> Angle {
        Angle { v, unit: AngleUnit::Deg }
    }
    pub fn rad(v: f64) -> Angle {
        Angle { v, unit: AngleUnit::Rad }
    }
    pub fn to_rad(self) -> f64 {
        Unit::from(self.unit).to_si(self.v)
    }
}

/// Plan point (project coordinates).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct P2 {
    /// X coordinate, expressed in `unit`.
    pub x: f64,
    /// Y coordinate, expressed in `unit`.
    pub y: f64,
    /// Length unit of x and y: m, cm, mm, ft or in. Required.
    pub unit: LenUnit,
}

impl P2 {
    pub fn m(x: f64, y: f64) -> P2 {
        P2 { x, y, unit: LenUnit::M }
    }
    pub fn to_m(self) -> [f64; 2] {
        let k = Unit::from(self.unit).si_factor();
        [self.x * k, self.y * k]
    }
}

/// Model point (project coordinates, z up).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct P3 {
    /// X component, expressed in `unit`.
    pub x: f64,
    /// Y component, expressed in `unit`.
    pub y: f64,
    /// Z component (up), expressed in `unit`.
    pub z: f64,
    /// Length unit of x, y and z: m, cm, mm, ft or in. Required.
    pub unit: LenUnit,
}

impl P3 {
    pub fn m(x: f64, y: f64, z: f64) -> P3 {
        P3 { x, y, z, unit: LenUnit::M }
    }
    pub fn to_m(self) -> [f64; 3] {
        let k = Unit::from(self.unit).si_factor();
        [self.x * k, self.y * k, self.z * k]
    }
}

/// Displacement (has a length unit, unlike `Dir3`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct V3 {
    /// X component, expressed in `unit`.
    pub x: f64,
    /// Y component, expressed in `unit`.
    pub y: f64,
    /// Z component (up), expressed in `unit`.
    pub z: f64,
    /// Length unit of x, y and z: m, cm, mm, ft or in. Required.
    pub unit: LenUnit,
}

impl V3 {
    pub fn m(x: f64, y: f64, z: f64) -> V3 {
        V3 { x, y, z, unit: LenUnit::M }
    }
    pub fn to_m(self) -> [f64; 3] {
        let k = Unit::from(self.unit).si_factor();
        [self.x * k, self.y * k, self.z * k]
    }
}

/// Unitless direction; the executor normalises it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dir3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Dir3 {
    pub const X: Dir3 = Dir3 { x: 1.0, y: 0.0, z: 0.0 };
    pub const Y: Dir3 = Dir3 { x: 0.0, y: 1.0, z: 0.0 };
    pub const Z: Dir3 = Dir3 { x: 0.0, y: 0.0, z: 1.0 };
}

/// A parameter value. `unit` must be present on the wire; `null` means dimensionless
/// (text, count, bool, ratio given as a plain number).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Value {
    /// The value, expressed in `unit` when a unit is given.
    pub v: serde_json::Value,
    /// Unit of `v` (m, mm, ft, m2, m3, deg, kg, kN, kPa, s, pct, ...) or null for a plain number, text or bool. The key must be present.
    #[serde(deserialize_with = "explicit")]
    #[schemars(required)]
    pub unit: Option<Unit>,
}

impl Value {
    pub fn num(v: f64, unit: Unit) -> Value {
        Value { v: serde_json::json!(v), unit: Some(unit) }
    }
    pub fn plain(v: serde_json::Value) -> Value {
        Value { v, unit: None }
    }
    pub fn text(s: &str) -> Value {
        Value::plain(serde_json::Value::String(s.to_owned()))
    }
    /// Numeric value in SI, or the plain number when dimensionless.
    pub fn si(&self) -> Option<f64> {
        let x = self.v.as_f64()?;
        Some(match self.unit {
            Some(u) => u.to_si(x),
            None => x,
        })
    }
}

/// One named field of a `Set` or of a new element's extra rows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    pub value: Value,
}

impl Field {
    pub fn new(name: &str, value: Value) -> Field {
        Field { name: name.to_owned(), value }
    }
}

/// `Option` field whose key must be present (a missing key is an error, `null` is `None`).
pub(crate) fn explicit<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}
