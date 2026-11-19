// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Network run grips in 3D: axis vertices and moving the whole run. Dragging, snaps and
//! temporary dimensions are the shared 3D grips (`edit3d::grips`, the network provider
//! `edit3d::mep_grips`); here only the grip kind and the drag result for
//! `mep3d::apply_grip`.

use ddc_render::glam::Vec3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GripKind {
    /// Axis vertex (point number).
    Vertex(usize),
    /// Move of the whole element.
    Move,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grip {
    pub at: Vec3,
    pub kind: GripKind,
}

/// A finished drag (scene coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Done {
    pub kind: GripKind,
    pub from: Vec3,
    pub to: Vec3,
    pub copy: bool,
}
