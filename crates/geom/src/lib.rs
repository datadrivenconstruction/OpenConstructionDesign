// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! DDC geometry kernel, stage K0 (docs/specs/KERNEL.md).

pub mod analytical;
pub mod axes;
pub mod arc;
pub mod brep;
pub mod component;
pub mod csg;
pub mod csg_check;
pub mod csg2;
pub mod csv;
pub mod curtain;
pub mod curtain_edit;
pub mod cut;
pub mod datum;
/// Detailed parametric geometry of doors, windows, railings, stairs, members, roof edges.
pub mod detail;
pub mod dim;
pub mod footing;
pub mod formfind;
pub mod frame3;
pub mod framing;
mod grid;
pub mod gsolid;
pub mod host;
mod gsolid_vol;
pub mod instance;
/// Parametric feature timeline: parameters, features, minimal rebuild (param1).
pub mod feature;
/// Assembly joints between rigid parts: equations, DOF by Jacobian rank, drive (param1).
pub mod joint;
pub mod label;
pub mod layered;
pub mod mass;
pub mod math;
pub mod member;
pub mod mep;
pub mod mesh;
pub mod mesh_dist;
pub mod mesh_fit;
pub mod mesh_ops;
pub mod face_fill;
pub mod nurbs;
pub mod mesh_repair;
pub mod panelize;
pub mod poly;
pub mod predicates;
pub mod profile;
pub mod profile_cat;
pub mod ramp;
pub mod rebar;
pub mod recognize;
pub mod roof;
pub mod roof_shape;
pub mod rooms;
pub mod section;
pub mod section_props;
pub mod shape_edit;
pub mod simplify;
pub mod sketch;
/// Profile sketch with typed, auto-driven dimensions and construction lines (ri7).
pub mod sketch_dims;
/// Family sketch constraint solver (g11).
pub mod sketch_solve;
pub mod slope;
pub mod stair;
pub mod steel;
pub mod sweep;
pub mod taper;
pub mod top;
pub mod topo;
pub mod wall_edit;
pub mod wall_lean;
pub mod wall_spline;

pub use brep::{Origin, Solid, ValidateError};
pub use math::{snap_mm, to_mm, v2, v3, Tol, V2, V3};
pub use profile::{Profile, ProfileError};
