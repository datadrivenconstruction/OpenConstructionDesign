// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Design intent of a brief: massing, facade system, balconies, roof terrace, entrance and
//! site. Every field has a default; a brief without `design` builds the plain bar it always did.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Massing {
    Bar,
    /// The bar and one wing at its east end, joined by a corner hall.
    L,
    /// The bar and a wing at each end.
    U,
    /// Four bars round a courtyard.
    Courtyard,
    /// Lower floors widened into a podium, the tower set back above it.
    TowerPodium,
}

impl Massing {
    pub const NAMES: [&'static str; 5] = ["bar", "l", "u", "courtyard", "tower_podium"];
    pub const ALL: [Massing; 5] = [
        Massing::Bar,
        Massing::L,
        Massing::U,
        Massing::Courtyard,
        Massing::TowerPodium,
    ];

    pub fn name(self) -> &'static str {
        Self::NAMES[Self::ALL.iter().position(|m| *m == self).unwrap_or(0)]
    }

    /// Wings besides the main bar.
    pub fn wings(self) -> usize {
        match self {
            Massing::L => 1,
            Massing::U => 2,
            Massing::Courtyard => 3,
            Massing::Bar | Massing::TowerPodium => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FacadeSystem {
    /// Windows as the daylight rule placed them.
    Punched,
    /// A continuous band of glass across every room.
    Ribbon,
    /// Storey-high glazing with mullions.
    Curtain,
    /// Tall slot windows in a rhythm, cladding panels between them.
    Panels,
    /// Windows with vertical fins in a rhythm.
    Fins,
}

impl FacadeSystem {
    pub const NAMES: [&'static str; 5] = ["punched", "ribbon", "curtain", "panels", "fins"];
    pub const ALL: [FacadeSystem; 5] = [
        FacadeSystem::Punched,
        FacadeSystem::Ribbon,
        FacadeSystem::Curtain,
        FacadeSystem::Panels,
        FacadeSystem::Fins,
    ];

    pub fn name(self) -> &'static str {
        Self::NAMES[Self::ALL.iter().position(|f| *f == self).unwrap_or(0)]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Balconies {
    None,
    Balcony,
    /// A balcony between side screens, read as a recess.
    Loggia,
}

impl Balconies {
    pub const NAMES: [&'static str; 3] = ["none", "balcony", "loggia"];
    pub const ALL: [Balconies; 3] = [Balconies::None, Balconies::Balcony, Balconies::Loggia];
}

/// Structural system: the types walls, floors, roof and columns build with
/// (`data/structure.json`); the geometry is the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Structure {
    Concrete,
    /// Cross-laminated timber walls and floors, glulam columns, a concrete core spine.
    Timber,
    Masonry,
    /// Reinforced concrete columns on the grid, infill walls between them.
    Frame,
    /// Glulam columns on the grid, CLT floors, timber infill, a concrete core spine.
    TimberFrame,
}

impl Structure {
    pub const NAMES: [&'static str; 5] = ["concrete", "timber", "masonry", "frame", "timber_frame"];
    pub const ALL: [Structure; 5] =
        [Structure::Concrete, Structure::Timber, Structure::Masonry, Structure::Frame, Structure::TimberFrame];

    /// Columns on every axis crossing carry the floors; the walls only enclose.
    pub fn is_frame(self) -> bool {
        matches!(self, Structure::Frame | Structure::TimberFrame)
    }

    pub fn is_timber(self) -> bool {
        matches!(self, Structure::Timber | Structure::TimberFrame)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Design {
    pub massing: Massing,
    /// First storey (from 0) of the upper part when the floors step.
    pub upper_from: Option<u32>,
    /// Upper part narrower than the lower one on each side, m.
    pub setback: f64,
    /// Upper part wider than the lower one at each end, m.
    pub cantilever: f64,
    pub facade: FacadeSystem,
    /// Spacing of windows, fins or panels along the facade, m.
    pub rhythm: f64,
    pub balconies: Balconies,
    /// Planters and deck on the flat roof and on podium roofs.
    pub terrace: bool,
    pub canopy: bool,
    /// Width of the paved band round the building, m.
    pub paving: f64,
    pub trees: u32,
    /// The courtyard roofed over: a floor on the ground slab, a roof at the top.
    pub atrium: bool,
    /// How far the south and the north facade bow out at their middle, m; a bar only.
    pub curve: f64,
    pub curve_north: f64,
    /// A free-form skin and a double-curved roof round the plate, in patches that can be made.
    pub shell: bool,
    pub structure: Structure,
}

impl Default for Design {
    fn default() -> Self {
        Design {
            massing: Massing::Bar,
            upper_from: None,
            setback: 0.0,
            cantilever: 0.0,
            facade: FacadeSystem::Punched,
            rhythm: 3.0,
            balconies: Balconies::None,
            terrace: false,
            canopy: false,
            paving: 0.0,
            trees: 0,
            atrium: false,
            curve: 0.0,
            curve_north: 0.0,
            shell: false,
            structure: Structure::Concrete,
        }
    }
}

impl Design {
    /// Defaults once a brief names any design intent: an entrance canopy and a little site.
    pub fn asked() -> Self {
        Design {
            canopy: true,
            paving: 3.0,
            trees: 6,
            ..Design::default()
        }
    }

    /// The storey the upper part starts on, if the floors step at all.
    pub fn step_at(&self, floors: u32) -> Option<u32> {
        let stepped = self.massing == Massing::TowerPodium
            || self.setback > 0.0
            || self.cantilever > 0.0;
        if !stepped || floors < 2 {
            return None;
        }
        let def = if self.massing == Massing::TowerPodium {
            (floors / 4).clamp(1, 3)
        } else {
            floors / 2
        };
        Some(self.upper_from.unwrap_or(def).clamp(1, floors - 1))
    }
}
