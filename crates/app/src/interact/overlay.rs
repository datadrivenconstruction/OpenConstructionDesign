// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `OverlaySet` — which overlays a view shows in a scope (row of `SCOPE_TABLE`).
use super::scope::Scope;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct OverlaySet(pub u16);

impl OverlaySet {
    pub const NONE: OverlaySet = OverlaySet(0);
    /// Grips of the selection.
    pub const GRIPS: OverlaySet = OverlaySet(1 << 0);
    /// Temporary dimensions.
    pub const TEMP_DIMS: OverlaySet = OverlaySet(1 << 1);
    /// Snap marker and tracking lines.
    pub const SNAP: OverlaySet = OverlaySet(1 << 2);
    /// Session preview.
    pub const PREVIEW: OverlaySet = OverlaySet(1 << 3);
    /// Selection halo.
    pub const SELECTION: OverlaySet = OverlaySet(1 << 4);
    /// Hover highlight of what a click would take.
    pub const HOVER: OverlaySet = OverlaySet(1 << 5);
    /// Rubber rectangle of a box selection.
    pub const BOX: OverlaySet = OverlaySet(1 << 6);
    /// Measurement read-out.
    pub const MEASURE: OverlaySet = OverlaySet(1 << 7);
    /// Step hint at the cursor.
    pub const HINT: OverlaySet = OverlaySet(1 << 8);

    /// Names in bit order, for the table snapshot.
    pub const NAMES: [&'static str; 9] =
        ["grips", "temp_dims", "snap", "preview", "selection", "hover", "box", "measure", "hint"];

    pub const fn or(self, o: OverlaySet) -> OverlaySet {
        OverlaySet(self.0 | o.0)
    }

    pub fn has(self, o: OverlaySet) -> bool {
        self.0 & o.0 == o.0
    }

    pub fn names(self) -> Vec<&'static str> {
        (0..Self::NAMES.len()).filter(|b| self.0 & (1 << b) != 0).map(|b| Self::NAMES[b]).collect()
    }

    pub fn of(s: &Scope) -> OverlaySet {
        super::hotset::row(s).2
    }
}
