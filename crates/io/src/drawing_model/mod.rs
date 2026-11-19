// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Drawing to model: an underlay (`Underlay`) from a DXF or a PDF page →
//! proposals for walls, openings and rooms (`propose*`) → accepted in one operation (`accept_proposals`).
//! Units are model metres; a proposal id is a hash of the rounded geometry, so it is stable between runs.

mod accept;
pub mod blob;
mod blocks;
mod extra;
pub mod fixture;
mod propose;
mod source;
mod textfmt;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_blocks;
#[cfg(any(test, feature = "dxf-write"))]
mod write;

pub use accept::{accept_proposals, AcceptOptions, AcceptReport};
pub use blocks::{BlockSrc, UBlocks, URef};
pub use extra::{
    glob, openings_from_blocks, openings_from_blocks_checked, propose_columns, propose_grids,
    rooms_from_outlines, BlockOpenings, BlockRule, BlockRules, ColumnCand, GridCand, RuleKind,
    UNREADABLE_WIDTH_CONFIDENCE,
};
pub use propose::{
    propose, propose_openings, propose_rooms, propose_walls, wall_types, WallProposal,
};
pub use source::{
    aci_rgb, guess_unit, Affine2, Pen, Pick, UArc, UBlockHint, UFill, UInsert, ULayer, ULine, UPoly,
    UText, Underlay,
};
pub use textfmt::TextFmt;
#[cfg(any(test, feature = "dxf-write"))]
pub use write::{append_underlay, to_dxf, Placement, WriteReport};

use std::collections::BTreeSet;

use ddc_geom::recognize::Options as WallOptions;

/// Model wall type: id, name, full thickness.
#[derive(Clone, Debug, PartialEq)]
pub struct WallTypeInfo {
    pub id: i64,
    pub name: String,
    pub thickness: f64,
}

#[derive(Clone, Debug)]
pub struct ProposeOptions {
    pub walls: WallOptions,
    /// Geometry layers skipped entirely: case-insensitive name substrings.
    pub ignore_geom_layers: Vec<String>,
    /// Text layers that do not take part in room labels.
    pub ignore_text_layers: Vec<String>,
    /// Skip dashed lines (axes, hidden outlines).
    pub skip_dashed: bool,
    /// Minimum number of parallel equally spaced lines to count them as hatching.
    pub hatch_min_lines: usize,
    pub hatch_max_step: f64,
    /// Closed shapes entirely within a square of this size, m, are furniture and symbols, not walls.
    pub symbol_max_size: f64,
    /// Allowed door width, m; outside the range confidence drops.
    pub door_width: (f64, f64),
    pub door_height: f64,
    pub window_sill: f64,
    pub window_height: f64,
    /// Thickness mismatch with the type above which the pair is considered ambiguous.
    pub type_tol: f64,
    /// Rules "block → opening/family".
    pub rules: BlockRules,
}

impl Default for ProposeOptions {
    fn default() -> Self {
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect();
        Self {
            walls: WallOptions {
                min_thickness: 0.08,
                tol: 1e-3,
                angle_tol: 1e-3,
                ..WallOptions::default()
            },
            ignore_geom_layers: words(&[
                "dim",
                "hatch",
                "hat",
                "anno",
                "text",
                "defpoints",
                "axis",
                "grid",
                "furn",
                "\u{440}\u{430}\u{437}\u{43c}\u{435}\u{440}",
                "\u{448}\u{442}\u{440}\u{438}\u{445}",
                "\u{442}\u{435}\u{43a}\u{441}\u{442}",
                "\u{430}\u{43d}\u{43d}\u{43e}\u{442}",
                "\u{43e}\u{441}\u{44c}",
                "\u{43e}\u{441}\u{438}",
                "\u{441}\u{435}\u{442}\u{43a}\u{430}",
                "\u{43c}\u{435}\u{431}\u{435}\u{43b}",
                "area",
                "room",
                "\u{43f}\u{43e}\u{43c}\u{435}\u{449}",
                "\u{43f}\u{43b}\u{43e}\u{449}\u{430}\u{434}",
                "column",
                "\u{43a}\u{43e}\u{43b}\u{43e}\u{43d}",
            ]),
            ignore_text_layers: words(&["dim", "defpoints", "\u{440}\u{430}\u{437}\u{43c}\u{435}\u{440}"]),
            skip_dashed: true,
            hatch_min_lines: 5,
            hatch_max_step: 0.5,
            symbol_max_size: 2.5,
            door_width: (0.6, 1.4),
            door_height: 2.1,
            window_sill: 0.9,
            window_height: 1.2,
            type_tol: 0.02,
            rules: BlockRules::default(),
        }
    }
}

/// User decisions: these ids are no longer proposed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    pub rejected: BTreeSet<u64>,
    pub accepted: BTreeSet<u64>,
}

impl Memory {
    pub fn reject(&mut self, ids: &[u64]) {
        self.rejected.extend(ids);
    }
    pub fn forget(&mut self, ids: &[u64]) {
        for i in ids {
            self.rejected.remove(i);
            self.accepted.remove(i);
        }
    }
    pub fn contains(&self, id: u64) -> bool {
        self.rejected.contains(&id) || self.accepted.contains(&id)
    }
    /// Lines `r <hex>` / `a <hex>` for storing next to the project.
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        for i in &self.rejected {
            s.push_str(&format!("r {i:016x}\n"));
        }
        for i in &self.accepted {
            s.push_str(&format!("a {i:016x}\n"));
        }
        s
    }
    /// Unreadable lines are skipped.
    pub fn from_text(t: &str) -> Self {
        let mut m = Memory::default();
        for l in t.lines() {
            if let Some((k, v)) = l.split_once(' ') {
                if let Ok(id) = u64::from_str_radix(v.trim(), 16) {
                    match k {
                        "r" => m.rejected.insert(id),
                        "a" => m.accepted.insert(id),
                        _ => false,
                    };
                }
            }
        }
        m
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WallCand {
    pub id: u64,
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub thickness: f64,
    pub height: f64,
    pub confidence: f64,
    /// The model type nearest in thickness.
    pub wall_type: Option<i64>,
    /// |thickness − type thickness|.
    pub type_delta: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpeningCand {
    pub id: u64,
    /// Wall proposal id.
    pub wall: u64,
    /// "door" or "window".
    pub kind: String,
    /// From the start of wall axis `a`.
    pub along: f64,
    pub width: f64,
    pub sill: f64,
    pub height: f64,
    pub swing_side: bool,
    pub swing_hinge: bool,
    pub confidence: f64,
    /// Family from the block rule (empty — default).
    pub family: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoomCand {
    pub id: u64,
    pub name: String,
    pub number: String,
    pub point: (f64, f64),
    pub area: f64,
    pub confidence: f64,
    /// Outline from the drawing (empty — from walls).
    pub boundary: Vec<(f64, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ambiguity {
    pub id: u64,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub walls: usize,
    pub doors: usize,
    pub windows: usize,
    pub rooms: usize,
    /// Lines that joined no wall and are not explained by an opening (long ones).
    pub unmatched: Vec<((f64, f64), (f64, f64))>,
    /// Shorter than `min_length`: ticks, reveals, noise.
    pub short_lines: usize,
    /// Lines of small closed shapes (furniture, columns) that were skipped.
    pub symbol_lines: usize,
    /// Line pairs discarded as dimension chains.
    pub dimension_pairs: usize,
    pub ambiguous: Vec<Ambiguity>,
    pub ignored_layer_lines: usize,
    pub dashed_lines: usize,
    pub hatch_lines: usize,
    pub door_leaf_lines: usize,
    /// Regions between walls without a label.
    pub unlabeled_regions: usize,
    /// Proposals dropped because of `Memory`.
    pub remembered_skipped: usize,
    /// Area labels inside rooms whose number reads two ways or not at all (`1,250 m2` with no
    /// decimal mark elsewhere in the drawing); never used as a value, their rooms are flagged.
    /// Also counts such block width attributes; those openings are flagged for review.
    pub unreadable_numbers: usize,
    pub grids: usize,
    pub columns: usize,
    /// Openings added from block inserts.
    pub block_openings: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Proposals {
    pub walls: Vec<WallCand>,
    pub openings: Vec<OpeningCand>,
    pub rooms: Vec<RoomCand>,
    pub grids: Vec<GridCand>,
    pub columns: Vec<ColumnCand>,
    pub report: Report,
}

impl Proposals {
    pub fn all_ids(&self) -> Vec<u64> {
        let w = self.walls.iter().map(|x| x.id);
        let o = self.openings.iter().map(|x| x.id);
        let r = self.rooms.iter().map(|x| x.id);
        let g = self.grids.iter().map(|x| x.id);
        let c = self.columns.iter().map(|x| x.id);
        w.chain(o).chain(r).chain(g).chain(c).collect()
    }
}

/// FNV-1a over integer values: a stable proposal id.
pub(crate) fn hash_key(kind: u8, vals: &[i64], s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut put = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    };
    put(kind);
    for v in vals {
        v.to_le_bytes().iter().for_each(|&b| put(b));
    }
    s.bytes().for_each(&mut put);
    h
}

/// Centimetres.
pub(crate) fn q(x: f64) -> i64 {
    (x * 100.0).round() as i64
}
