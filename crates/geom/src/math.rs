// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Vectors and tolerances. The length unit is the metre.

use std::ops::{Add, Mul, Neg, Sub};

/// Body tolerances: explicit, no global eps (KERNEL.md §2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tol {
    pub length: f64,
    pub angle: f64,
}

impl Default for Tol {
    fn default() -> Self {
        Self {
            length: 1e-6,
            angle: 1e-9,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct V2 {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct V3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// Metres rounded to whole millimetres: shared coordinates compare exactly after it.
pub fn snap_mm(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Metres as whole millimetres: an exact integer key for lengths and coordinates.
pub fn to_mm(x: f64) -> i64 {
    (x * 1000.0).round() as i64
}

pub const fn v2(x: f64, y: f64) -> V2 {
    V2 { x, y }
}

pub const fn v3(x: f64, y: f64, z: f64) -> V3 {
    V3 { x, y, z }
}

impl V2 {
    pub fn cross(self, o: V2) -> f64 {
        self.x * o.y - self.y * o.x
    }
    pub fn len(self) -> f64 {
        self.x.hypot(self.y)
    }
    pub fn dot(self, o: V2) -> f64 {
        self.x * o.x + self.y * o.y
    }
}

impl V3 {
    pub fn dot(self, o: V3) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    pub fn cross(self, o: V3) -> V3 {
        v3(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    pub fn len(self) -> f64 {
        self.dot(self).sqrt()
    }
    pub fn normalized(self) -> V3 {
        self * (1.0 / self.len())
    }
}

macro_rules! ops {
    ($t:ident { $($f:ident),+ }) => {
        impl Add for $t { type Output = $t; fn add(self, o: $t) -> $t { $t { $($f: self.$f + o.$f),+ } } }
        impl Sub for $t { type Output = $t; fn sub(self, o: $t) -> $t { $t { $($f: self.$f - o.$f),+ } } }
        impl Mul<f64> for $t { type Output = $t; fn mul(self, k: f64) -> $t { $t { $($f: self.$f * k),+ } } }
        impl Neg for $t { type Output = $t; fn neg(self) -> $t { $t { $($f: -self.$f),+ } } }
    };
}
ops!(V2 { x, y });
ops!(V3 { x, y, z });
