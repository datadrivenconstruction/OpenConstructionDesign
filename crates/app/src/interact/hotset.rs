// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `HotSet` — which pick categories a ray or a click takes in a scope — and `SCOPE_TABLE`,
//! the one table both `HotSet` and `OverlaySet` are read from.
use super::overlay::OverlaySet;
use super::scope::{Scope, ScopeKind};
use crate::edit3d::pick::Cat;

/// Mask of `edit3d::pick::Cat`: one bit per category, one bit for all extension categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct HotSet(pub u32);

impl HotSet {
    pub const NONE: HotSet = HotSet(0);
    pub const ALL: HotSet = HotSet((1 << 16) - 1);
    /// Elements that host a placed one (doors, windows, openings, fixtures).
    pub const HOSTS: HotSet = HotSet(
        bit(Cat::Wall) | bit(Cat::Curtain) | bit(Cat::Slab) | bit(Cat::Roof) | bit(Cat::Ceiling),
    );
    /// Everything with a face to paint (rooms have none).
    pub const FACES: HotSet = HotSet(Self::ALL.0 & !bit(Cat::Room));

    pub fn has(self, c: Cat) -> bool {
        self.0 & bit(c) != 0
    }

    pub fn of(s: &Scope) -> HotSet {
        row(s).1
    }
}

/// Bit of a category; the exhaustive match keeps the mask in step with `Cat`.
pub const fn bit(c: Cat) -> u32 {
    1 << match c {
        Cat::Wall => 0,
        Cat::Opening => 1,
        Cat::Curtain => 2,
        Cat::Panel => 3,
        Cat::Slab => 4,
        Cat::Roof => 5,
        Cat::Ceiling => 6,
        Cat::Column => 7,
        Cat::Beam => 8,
        Cat::Stair => 9,
        Cat::Railing => 10,
        Cat::Comp => 11,
        Cat::Room => 12,
        Cat::Mep => 13,
        Cat::Mesh => 14,
        Cat::Ext(_) => 15,
    }
}

const O: OverlaySet = OverlaySet::NONE;
const fn ov(list: &[OverlaySet]) -> OverlaySet {
    let mut r = O;
    let mut i = 0;
    while i < list.len() {
        r = r.or(list[i]);
        i += 1;
    }
    r
}

use OverlaySet as V;

/// Row per `ScopeKind`, in `ScopeKind::ALL` order (checked at compile time).
/// What snapping leaves out in a scope: the bodies being moved must not snap to themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapSkip {
    None,
    /// The selection (a modify move, a gizmo drag).
    Selection,
    /// The one element under the grip or reshape.
    Target,
}

impl SnapSkip {
    pub fn of(s: &Scope) -> SnapSkip {
        row(s).3
    }
}

pub const SCOPE_TABLE: [(ScopeKind, HotSet, OverlaySet, SnapSkip); 10] = [
    (ScopeKind::Idle, HotSet::ALL, ov(&[V::GRIPS, V::SELECTION, V::HOVER]), SnapSkip::None),
    (ScopeKind::Drafting, HotSet::NONE, ov(&[V::PREVIEW, V::SNAP, V::TEMP_DIMS, V::HINT]), SnapSkip::None),
    (ScopeKind::Placing, HotSet::HOSTS, ov(&[V::PREVIEW, V::SNAP, V::HOVER, V::HINT]), SnapSkip::None),
    (ScopeKind::Moving, HotSet::NONE, ov(&[V::PREVIEW, V::SNAP, V::TEMP_DIMS, V::SELECTION, V::HINT]), SnapSkip::Selection),
    (ScopeKind::GripDrag, HotSet::NONE, ov(&[V::GRIPS, V::PREVIEW, V::SNAP, V::TEMP_DIMS, V::HINT]), SnapSkip::Target),
    (ScopeKind::Reshaping, HotSet::NONE, ov(&[V::GRIPS, V::PREVIEW, V::SNAP, V::HINT]), SnapSkip::Target),
    (ScopeKind::BoxSelect, HotSet::ALL, ov(&[V::BOX, V::SELECTION]), SnapSkip::None),
    (ScopeKind::Painting, HotSet::FACES, ov(&[V::HOVER, V::HINT]), SnapSkip::None),
    (ScopeKind::Measuring, HotSet::ALL, ov(&[V::SNAP, V::MEASURE, V::TEMP_DIMS, V::HINT]), SnapSkip::None),
    (ScopeKind::Pointing, HotSet::ALL, ov(&[V::HOVER, V::SNAP, V::HINT]), SnapSkip::None),
];

const _: () = {
    let mut i = 0;
    while i < SCOPE_TABLE.len() {
        assert!(SCOPE_TABLE[i].0 as usize == ScopeKind::ALL[i] as usize);
        assert!(SCOPE_TABLE[i].0 as usize == i);
        i += 1;
    }
};

pub(crate) fn row(s: &Scope) -> &'static (ScopeKind, HotSet, OverlaySet, SnapSkip) {
    &SCOPE_TABLE[s.kind() as usize]
}
