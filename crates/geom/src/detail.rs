// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Detailed parametric geometry of building elements (LOD 350–400): doors, windows, railings,
//! stair nosings, member sections, roof edges. Each builder takes a type's parameters and the
//! rule values ([`rules::DetailRules`]) and returns closed shells grouped by material, at three
//! levels of detail; identical inputs share one built prototype ([`cache`]).

pub mod cache;
pub mod door;
pub mod opening;
pub mod plan;
pub mod prim;
pub mod rail;
pub mod roof;
pub mod rules;
pub mod section;
pub mod stair;
pub mod window;

pub use crate::component::Lod;
use crate::math::V3;
use crate::mesh::Mesh;

/// Material slot of a detail part; the viewer picks its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mat {
    /// Door lining and casings (light), window frame and sashes (dark).
    Frame,
    Leaf,
    Glass,
    /// Hardware, balusters, handrails, gutters.
    Metal,
    /// Thresholds, sills, copings.
    Stone,
    /// Inside window boards, stair treads.
    Board,
}

/// Closed shells by material, in the element's local frame.
#[derive(Clone, Debug, Default)]
pub struct Detail {
    pub parts: Vec<(Mat, Mesh)>,
}

impl Detail {
    /// The mesh of material `m`, created on first use.
    pub fn part(&mut self, m: Mat) -> &mut Mesh {
        let i = match self.parts.iter().position(|p| p.0 == m) {
            Some(i) => i,
            None => {
                self.parts.push((m, Mesh::default()));
                self.parts.len() - 1
            }
        };
        &mut self.parts[i].1
    }

    pub fn triangles(&self) -> usize {
        self.parts.iter().map(|p| p.1.triangles.len()).sum()
    }

    pub fn vertices(&self) -> usize {
        self.parts.iter().map(|p| p.1.positions.len()).sum()
    }

    /// Number of closed shells (separate pieces).
    pub fn shells(&self) -> usize {
        self.parts
            .iter()
            .map(|p| p.1.face_of.last().map_or(0, |f| *f as usize + 1))
            .sum()
    }

    /// The whole element is one plain 8-vertex block: a placeholder, not detail.
    pub fn is_plain_box(&self) -> bool {
        self.vertices() <= 8 && self.triangles() <= 12
    }

    /// Every part closed and turned outward.
    pub fn is_closed(&self) -> bool {
        self.parts
            .iter()
            .all(|p| p.1.triangles.is_empty() || (p.1.is_watertight() && p.1.volume() > 0.0))
    }

    pub fn bounds(&self) -> Option<(V3, V3)> {
        let mut it = self.parts.iter().flat_map(|p| p.1.positions.iter());
        let f = *it.next()?;
        Some(it.fold((f, f), |(lo, hi), p| {
            (
                crate::math::v3(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)),
                crate::math::v3(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)),
            )
        }))
    }

    /// Mirror across the plane `axis = at` (0 = x, 1 = y); windings follow, shells stay outward.
    pub fn mirror(&mut self, axis: usize, at: f64) {
        for (_, m) in &mut self.parts {
            for p in &mut m.positions {
                match axis {
                    0 => p.x = 2.0 * at - p.x,
                    _ => p.y = 2.0 * at - p.y,
                }
            }
            for t in &mut m.triangles {
                t.swap(1, 2);
            }
        }
    }
}

/// Triangle budget of one element at each level of detail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    pub coarse: usize,
    pub medium: usize,
    pub fine: usize,
}

impl Budget {
    pub fn at(&self, lod: Lod) -> usize {
        match lod {
            Lod::Coarse => self.coarse,
            Lod::Medium => self.medium,
            Lod::Fine => self.fine,
        }
    }
}

/// Per door leaf (a double door has two).
pub const DOOR_BUDGET: Budget = Budget {
    coarse: 120,
    medium: 900,
    fine: 2600,
};
/// Per glazing field of a window.
pub const WINDOW_BUDGET: Budget = Budget {
    coarse: 80,
    medium: 420,
    fine: 1100,
};
/// Per metre of railing.
pub const RAIL_BUDGET: Budget = Budget {
    coarse: 60,
    medium: 600,
    fine: 1500,
};

/// Sides of a round part at a level of detail.
pub fn round_sides(lod: Lod, small: bool) -> usize {
    match (lod, small) {
        (Lod::Coarse, _) => 4,
        (Lod::Medium, true) => 6,
        (Lod::Medium, false) => 10,
        (Lod::Fine, true) => 10,
        (Lod::Fine, false) => 16,
    }
}
